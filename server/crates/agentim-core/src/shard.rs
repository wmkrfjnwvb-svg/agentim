//! One writer per database: group commit (docs/02-architecture.md §4).
//!
//! The writer takes the first queued command plus whatever is already queued
//! (up to `max_batch`), runs them in one transaction with one savepoint each,
//! and commits once — one fsync for the whole batch. Replies (ACKs) and
//! fan-out happen only after the commit returns. A failed commit is fatal for
//! this database: no retry (an fsync error may have dropped dirty pages), every
//! command in the batch gets `storage_unavailable`, and the database stops
//! accepting writes until the process restarts and recovers from its log.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use sqlx::{AnyConnection, AnyPool, Connection, Row};
use tokio::sync::{mpsc, oneshot};

use crate::config::{DatabaseConfig, WebhookConfig, WriterConfig};
use crate::db::Db;
use crate::logic::{Ctx, Op, ShardState, Shared};
use crate::metrics as m;
use crate::model::*;
use crate::registry::Out;
use crate::storage::{self, Dialect};
use crate::webhook::Wake;

#[derive(Default)]
pub struct Health {
    failed: AtomicBool,
    reason: Mutex<Option<String>>,
    depth: AtomicUsize,
    /// Fault injection for tests: the next N commits fail.
    pub fail_next_commits: AtomicU32,
    /// Fault injection for tests: the next N commands fail inside the
    /// transaction (their savepoint is rolled back, the batch commits).
    pub fail_next_jobs: AtomicU32,
    pub jobs: AtomicU64,
    pub batches: AtomicU64,
    pub max_batch: AtomicU64,
}

impl Health {
    pub fn failed(&self) -> Option<String> {
        if self.failed.load(Ordering::SeqCst) {
            Some(
                self.reason
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone()
                    .unwrap_or_default(),
            )
        } else {
            None
        }
    }

    fn fail(&self, db: &str, reason: String) {
        tracing::error!(database = db, %reason, "storage failure: database stops accepting writes");
        *self.reason.lock().unwrap_or_else(|e| e.into_inner()) = Some(reason);
        self.failed.store(true, Ordering::SeqCst);
        metrics::gauge!(m::STORAGE_HEALTHY, "database" => db.to_string()).set(0.0);
    }

    pub fn depth(&self) -> usize {
        self.depth.load(Ordering::Relaxed)
    }
}

/// Decrements a fault-injection counter if it is above zero.
fn take_one(n: &AtomicU32) -> bool {
    let mut cur = n.load(Ordering::SeqCst);
    while cur > 0 {
        match n.compare_exchange(cur, cur - 1, Ordering::SeqCst, Ordering::SeqCst) {
            Ok(_) => return true,
            Err(now) => cur = now,
        }
    }
    false
}

struct Job {
    op: Op,
    reply: oneshot::Sender<ApiResult<Value>>,
}

#[derive(Clone)]
pub struct Shard {
    pub idx: usize,
    pub name: String,
    pub dialect: Dialect,
    pub read: AnyPool,
    pub health: Arc<Health>,
    pub queue_capacity: usize,
    tx: mpsc::Sender<Job>,
    /// Encryption at rest of this database.
    pub crypto: Arc<crate::crypto::Crypto>,
}

impl Shard {
    pub async fn open(
        idx: usize,
        cfg: &DatabaseConfig,
        wcfg: &WriterConfig,
        shared: Arc<Shared>,
        webhooks: &[WebhookConfig],
        master_key: Option<&str>,
    ) -> ApiResult<(Shard, Wake)> {
        let dialect = Dialect::from_url(&cfg.url).map_err(|e| ApiError::new("config", e))?;
        let mut conn = storage::open_writer(&cfg.url, dialect).await?;
        storage::ensure_schema(&mut conn, dialect).await?;
        if let Err(e) = storage::check_durability(&mut conn, dialect).await {
            if cfg.unsafe_durability {
                tracing::warn!(
                    database = cfg.name,
                    reason = e,
                    "unsafe durability explicitly allowed"
                );
            } else {
                return Err(ApiError::new(
                    "unsafe_durability",
                    format!("database {}: {e}", cfg.name),
                ));
            }
        }
        let crypto = Arc::new(open_crypto(&mut conn, dialect, &cfg.name, master_key).await?);
        let read = storage::open_pool(&cfg.url, dialect, cfg.read_pool.unwrap_or(8)).await?;
        // Webhook outbox: next id, and cursors for webhooks new to this
        // database start at the end (no replay of history).
        let max: Option<i64> = sqlx::query("SELECT MAX(id) FROM webhook_outbox")
            .fetch_one(&mut conn)
            .await?
            .try_get(0)?;
        let max = max.unwrap_or(0);
        for w in webhooks {
            let key = crate::webhook::cursor_key(&w.name);
            let have = sqlx::query(dialect.q("SELECT v FROM meta WHERE k = ?"))
                .bind(key.clone())
                .fetch_optional(&mut conn)
                .await?;
            if have.is_none() {
                sqlx::query(dialect.q("INSERT INTO meta (k, v) VALUES (?, ?)"))
                    .bind(key)
                    .bind(max.to_string())
                    .execute(&mut conn)
                    .await?;
            }
        }
        let wake: Wake = Arc::new(tokio::sync::watch::channel(max).0);
        let (tx, rx) = mpsc::channel(wcfg.queue_capacity);
        let health = Arc::new(Health::default());
        metrics::gauge!(m::STORAGE_HEALTHY, "database" => cfg.name.clone()).set(1.0);
        let w = Writer {
            conn,
            dialect,
            idx,
            name: cfg.name.clone(),
            shared,
            health: health.clone(),
            cfg: wcfg.clone(),
            outbox_next: max + 1,
            outbox_patterns: webhooks.iter().flat_map(|w| w.events.clone()).collect(),
            wake: wake.clone(),
            crypto: crypto.clone(),
        };
        tokio::spawn(w.run(rx));
        tracing::info!(
            database = cfg.name,
            dialect = dialect.as_str(),
            "database writer started"
        );
        Ok((
            Shard {
                idx,
                name: cfg.name.clone(),
                dialect,
                read,
                health,
                queue_capacity: wcfg.queue_capacity,
                tx,
                crypto,
            },
            wake,
        ))
    }

    /// Queues a command and waits for its result (ACK after commit).
    pub async fn call(&self, op: Op) -> ApiResult<Value> {
        if let Some(r) = self.health.failed() {
            metrics::counter!(m::WRITE_REJECTED, "reason" => "storage_failed").increment(1);
            return Err(ApiError::new("storage_unavailable", r));
        }
        let (reply, rx) = oneshot::channel();
        match self.tx.try_send(Job { op, reply }) {
            Ok(()) => {
                self.health.depth.fetch_add(1, Ordering::Relaxed);
            }
            Err(mpsc::error::TrySendError::Full(_)) => {
                metrics::counter!(m::WRITE_REJECTED, "reason" => "overloaded").increment(1);
                return Err(ApiError::new(
                    "overloaded",
                    "server is busy, retry with the same client_msg_id",
                ));
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                return Err(ApiError::new("storage_unavailable", "writer stopped"));
            }
        }
        rx.await
            .unwrap_or_else(|_| Err(ApiError::new("storage_unavailable", "writer stopped")))
    }

    /// Fire-and-forget (ticks, disconnect notices); dropped when the queue is full.
    pub fn post(&self, op: Op) {
        let (reply, _rx) = oneshot::channel();
        if self.tx.try_send(Job { op, reply }).is_ok() {
            self.health.depth.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn writer_stats(&self) -> Value {
        json!({
            "database": self.name,
            "dialect": self.dialect.as_str(),
            "jobs": self.health.jobs.load(Ordering::Relaxed),
            "batches": self.health.batches.load(Ordering::Relaxed),
            "max_batch": self.health.max_batch.load(Ordering::Relaxed),
            "queue_depth": self.health.depth(),
            "failed": self.health.failed(),
        })
    }
}

struct Writer {
    conn: AnyConnection,
    dialect: Dialect,
    idx: usize,
    name: String,
    shared: Arc<Shared>,
    health: Arc<Health>,
    cfg: WriterConfig,
    /// Next webhook outbox id (only this writer inserts).
    outbox_next: i64,
    /// Union of all webhooks' event patterns; empty = no outbox rows.
    outbox_patterns: Vec<String>,
    wake: Wake,
    crypto: Arc<crate::crypto::Crypto>,
}

impl Writer {
    async fn run(mut self, mut rx: mpsc::Receiver<Job>) {
        let mut st = ShardState::new(&self.shared);
        while let Some(first) = rx.recv().await {
            if self.cfg.commit_delay_us > 0 && !rx.is_empty() {
                tokio::time::sleep(Duration::from_micros(self.cfg.commit_delay_us)).await;
            }
            let mut batch = vec![first];
            while batch.len() < self.cfg.max_batch {
                match rx.try_recv() {
                    Ok(j) => batch.push(j),
                    Err(_) => break,
                }
            }
            self.health.depth.fetch_sub(batch.len(), Ordering::Relaxed);
            metrics::gauge!(m::WRITE_QUEUE_DEPTH, "database" => self.name.clone())
                .set(self.health.depth() as f64);

            if let Some(r) = self.health.failed() {
                for j in batch {
                    let _ = j
                        .reply
                        .send(Err(ApiError::new("storage_unavailable", r.clone())));
                }
                continue;
            }
            // Idle ticks need no transaction.
            if batch.iter().all(|j| j.op.is_tick()) && !st.tick_needs_db() {
                for j in batch {
                    let _ = j.reply.send(Ok(json!({})));
                }
                continue;
            }
            self.run_batch(&mut st, batch).await;
        }
    }

    async fn run_batch(&mut self, st: &mut ShardState, batch: Vec<Job>) {
        let n = batch.len();
        let started = Instant::now();
        let mut replies: Vec<(oneshot::Sender<ApiResult<Value>>, ApiResult<Value>)> =
            Vec::with_capacity(n);
        let mut outs: Vec<Out> = Vec::new();
        let result: Result<Option<i64>, String> = async {
            // SQLite: take the write lock when the batch starts. A deferred
            // transaction that has read and then must write fails at once
            // (SQLITE_BUSY) if another connection committed in between;
            // IMMEDIATE waits for the busy timeout instead.
            let mut tx = if self.dialect == Dialect::Sqlite {
                Connection::begin_with(&mut self.conn, "BEGIN IMMEDIATE").await
            } else {
                Connection::begin(&mut self.conn).await
            }
            .map_err(|e| e.to_string())?;
            for job in batch {
                let mut sp = Connection::begin(&mut *tx)
                    .await
                    .map_err(|e| e.to_string())?;
                let mut out = Vec::new();
                let mark = self.crypto.mark();
                let (res, keep) = {
                    let mut ctx = Ctx {
                        db: Db {
                            c: &mut sp,
                            d: self.dialect,
                            cx: &self.crypto,
                        },
                        st,
                        sh: &self.shared,
                        out: &mut out,
                        idx: self.idx,
                        commit_on_error: false,
                    };
                    let injected = !job.op.is_tick() && take_one(&self.health.fail_next_jobs);
                    let res = if injected {
                        Err(ApiError::new("storage", "injected statement failure"))
                    } else {
                        ctx.run(job.op).await
                    };
                    let keep = ctx.commit_on_error && !injected;
                    (res, keep)
                };
                if res.is_ok() || keep {
                    sp.commit().await.map_err(|e| e.to_string())?;
                    outs.extend(out);
                } else {
                    sp.rollback().await.map_err(|e| e.to_string())?;
                    // Data keys created by the rolled-back job do not exist.
                    self.crypto.rollback_to(mark);
                }
                if let Err(e) = &res
                    && e.code == "storage"
                {
                    metrics::counter!(m::STORAGE_ERRORS, "kind" => "statement").increment(1);
                }
                replies.push((job.reply, res));
            }
            let queued = queue_outbox(
                &mut self.outbox_next,
                &self.outbox_patterns,
                &self.name,
                self.dialect,
                &self.crypto,
                &mut tx,
                &outs,
            )
            .await
            .map_err(|e| e.message)?;
            if self.health.fail_next_commits.load(Ordering::SeqCst) > 0 {
                self.health.fail_next_commits.fetch_sub(1, Ordering::SeqCst);
                let _ = tx.rollback().await;
                return Err("injected commit failure".to_string());
            }
            let t = Instant::now();
            tx.commit().await.map_err(|e| e.to_string())?;
            metrics::histogram!(m::COMMIT_DURATION, "database" => self.name.clone())
                .record(t.elapsed().as_secs_f64());
            Ok(queued)
        }
        .await;

        match result {
            Ok(queued) => {
                self.crypto.commit();
                self.health.jobs.fetch_add(n as u64, Ordering::Relaxed);
                self.health.batches.fetch_add(1, Ordering::Relaxed);
                self.health.max_batch.fetch_max(n as u64, Ordering::Relaxed);
                metrics::histogram!(m::COMMIT_BATCH_SIZE).record(n as f64);
                // Durable now: deliver, then acknowledge.
                if let Some(last) = queued {
                    self.wake.send_replace(last);
                }
                self.shared.apply_tokens(&outs);
                self.shared.reg.deliver(outs);
                for (r, res) in replies {
                    let _ = r.send(res);
                }
                tracing::trace!(
                    database = self.name,
                    batch = n,
                    elapsed_us = started.elapsed().as_micros() as u64,
                    "batch committed"
                );
            }
            Err(e) => {
                self.crypto.abort();
                metrics::counter!(m::STORAGE_ERRORS, "kind" => "commit").increment(1);
                self.health.fail(&self.name, e.clone());
                for (r, _) in replies {
                    let _ = r.send(Err(ApiError::new("storage_unavailable", e.clone())));
                }
            }
        }
    }
}

/// Writes the batch's subscribed events to the webhook outbox, inside
/// the batch transaction. Returns the last id written.
async fn queue_outbox(
    next: &mut i64,
    patterns: &[String],
    db: &str,
    dialect: Dialect,
    crypto: &crate::crypto::Crypto,
    tx: &mut AnyConnection,
    outs: &[Out],
) -> ApiResult<Option<i64>> {
    if patterns.is_empty() {
        return Ok(None);
    }
    // Recalls and dissolutions in this batch also cover its earlier events,
    // which the commands' own cleanup could not see yet (D91).
    let mut recalled = std::collections::HashSet::new();
    let mut dissolved = std::collections::HashSet::new();
    for o in outs {
        if let Out::Event { ev, .. } = o {
            match ev.kind.as_str() {
                "msg.recalled" => {
                    if let Some(m) = &ev.msg {
                        recalled.insert(m.msg_id.as_str());
                    }
                }
                "conv.dissolved" => {
                    dissolved.insert(ev.conv_id.as_str());
                }
                _ => {}
            }
        }
    }
    let rows: Vec<std::borrow::Cow<crate::model::Event>> = outs
        .iter()
        .filter_map(|o| match o {
            Out::Event { ev, .. } if crate::webhook::matches(patterns, &ev.kind) => Some(ev),
            _ => None,
        })
        .filter(|ev| ev.kind == "conv.dissolved" || !dissolved.contains(ev.conv_id.as_str()))
        .map(|ev| match &ev.msg {
            Some(m) if ev.kind != "msg.recalled" && recalled.contains(m.msg_id.as_str()) => {
                let mut ev = ev.clone();
                if let Some(m) = ev.msg.as_mut() {
                    m.content.clear();
                    m.payload = None;
                    m.mentions.clear();
                }
                std::borrow::Cow::Owned(ev)
            }
            _ => std::borrow::Cow::Borrowed(ev),
        })
        .collect();
    if rows.is_empty() {
        return Ok(None);
    }
    let first = *next;
    let now = now_ms();
    // Bodies hold message content: encrypted with the database's data key.
    let mut bodies = Vec::with_capacity(rows.len());
    {
        let mut d = Db {
            c: &mut *tx,
            d: dialect,
            cx: crypto,
        };
        for (i, ev) in rows.iter().enumerate() {
            let id = *next + i as i64;
            let body = crate::webhook::event_body(db, ev).to_string();
            bodies.push(
                d.seal("", &format!("webhook_outbox.body:{id}"), &body)
                    .await?,
            );
        }
    }
    let mut bodies = bodies.into_iter();
    for chunk in rows.chunks(200) {
        let marks = vec!["(?, ?, ?, ?, ?)"; chunk.len()].join(", ");
        let sql = format!(
            "INSERT INTO webhook_outbox (id, conv_id, type, body, created_at) VALUES {marks}"
        );
        let mut q = sqlx::query(dialect.q(&sql));
        for ev in chunk {
            q = q
                .bind(*next)
                .bind(ev.conv_id.clone())
                .bind(ev.kind.clone())
                .bind(bodies.next().unwrap())
                .bind(now);
            *next += 1;
        }
        if let Err(e) = q.execute(&mut *tx).await {
            // Ids are only reused if nothing was committed with them.
            *next = first;
            return Err(e.into());
        }
    }
    Ok(Some(*next - 1))
}

/// Opens the database's encryption: refuses a database whose content is
/// encrypted with another master key, or encrypted while encryption is off.
async fn open_crypto(
    conn: &mut AnyConnection,
    d: Dialect,
    name: &str,
    master_key: Option<&str>,
) -> ApiResult<crate::crypto::Crypto> {
    let keys: i64 = sqlx::query("SELECT COUNT(*) FROM data_keys")
        .fetch_one(&mut *conn)
        .await?
        .try_get(0)?;
    let fp: Option<String> = sqlx::query(d.q("SELECT v FROM meta WHERE k = ?"))
        .bind("kek_fingerprint")
        .fetch_optional(&mut *conn)
        .await?
        .map(|r| r.try_get(0))
        .transpose()?;
    let Some(mk) = master_key else {
        if keys > 0 {
            return Err(ApiError::new(
                "config",
                format!("database {name} holds encrypted content; configure its master key"),
            ));
        }
        return Ok(crate::crypto::Crypto::new(None));
    };
    let kek = crate::crypto::Kek::from_base64(mk).map_err(|e| ApiError::new("config", e))?;
    match fp {
        Some(f) if f != kek.fingerprint => {
            return Err(ApiError::new(
                "config",
                format!("database {name} was encrypted with a different master key"),
            ));
        }
        Some(_) => {}
        None => {
            sqlx::query(d.q("INSERT INTO meta (k, v) VALUES (?, ?)"))
                .bind("kek_fingerprint")
                .bind(kek.fingerprint.clone())
                .execute(&mut *conn)
                .await?;
        }
    }
    Ok(crate::crypto::Crypto::new(Some(kek)))
}
