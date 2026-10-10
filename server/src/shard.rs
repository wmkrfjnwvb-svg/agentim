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
use sqlx::{AnyConnection, AnyPool, Connection};
use tokio::sync::{mpsc, oneshot};

use crate::config::{DatabaseConfig, WriterConfig};
use crate::db::Db;
use crate::logic::{Ctx, Op, ShardState, Shared};
use crate::metrics as m;
use crate::model::*;
use crate::registry::Out;
use crate::storage::{self, Dialect};

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

pub struct Shard {
    pub idx: usize,
    pub name: String,
    pub dialect: Dialect,
    pub read: AnyPool,
    pub health: Arc<Health>,
    pub queue_capacity: usize,
    tx: mpsc::Sender<Job>,
}

impl Shard {
    pub async fn open(
        idx: usize,
        cfg: &DatabaseConfig,
        wcfg: &WriterConfig,
        shared: Arc<Shared>,
    ) -> ApiResult<Shard> {
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
        let read = storage::open_pool(&cfg.url, dialect, cfg.read_pool.unwrap_or(8)).await?;
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
        };
        tokio::spawn(w.run(rx));
        tracing::info!(
            database = cfg.name,
            dialect = dialect.as_str(),
            "database writer started"
        );
        Ok(Shard {
            idx,
            name: cfg.name.clone(),
            dialect,
            read,
            health,
            queue_capacity: wcfg.queue_capacity,
            tx,
        })
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
        let result: Result<(), String> = async {
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
                let (res, keep) = {
                    let mut ctx = Ctx {
                        db: Db {
                            c: &mut sp,
                            d: self.dialect,
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
                }
                if let Err(e) = &res
                    && e.code == "storage"
                {
                    metrics::counter!(m::STORAGE_ERRORS, "kind" => "statement").increment(1);
                }
                replies.push((job.reply, res));
            }
            if self.health.fail_next_commits.load(Ordering::SeqCst) > 0 {
                self.health.fail_next_commits.fetch_sub(1, Ordering::SeqCst);
                let _ = tx.rollback().await;
                return Err("injected commit failure".to_string());
            }
            let t = Instant::now();
            tx.commit().await.map_err(|e| e.to_string())?;
            metrics::histogram!(m::COMMIT_DURATION, "database" => self.name.clone())
                .record(t.elapsed().as_secs_f64());
            Ok(())
        }
        .await;

        match result {
            Ok(()) => {
                self.health.jobs.fetch_add(n as u64, Ordering::Relaxed);
                self.health.batches.fetch_add(1, Ordering::Relaxed);
                self.health.max_batch.fetch_max(n as u64, Ordering::Relaxed);
                metrics::histogram!(m::COMMIT_BATCH_SIZE).record(n as f64);
                // Durable now: deliver, then acknowledge.
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
                metrics::counter!(m::STORAGE_ERRORS, "kind" => "commit").increment(1);
                self.health.fail(&self.name, e.clone());
                for (r, _) in replies {
                    let _ = r.send(Err(ApiError::new("storage_unavailable", e.clone())));
                }
            }
        }
    }
}
