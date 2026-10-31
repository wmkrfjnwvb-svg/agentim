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
use crate::replication::{self, Mode};
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
    /// Committed batches the replicas have not confirmed yet.
    pub unconfirmed: AtomicU64,
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
    /// For the cluster (routing to the node holding a shard).
    shared: Arc<Shared>,
    /// How writes are kept safe from a primary failure (docs/11 §8.1).
    pub mode: Mode,
}

impl Shard {
    pub async fn open(
        idx: usize,
        cfg: &DatabaseConfig,
        wcfg: &WriterConfig,
        shared: Arc<Shared>,
        webhooks: &[WebhookConfig],
        master_key: Option<&str>,
        encrypt: bool,
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
        let mode = replication::detect(&mut conn, dialect, &cfg.replication)
            .await
            .map_err(|e| ApiError::new("replication", format!("database {}: {e}", cfg.name)))?;
        // Conversations from before schema v6 count as replicated.
        Db {
            c: &mut conn,
            d: dialect,
            cx: &crate::crypto::Crypto::default(),
            cap: false,
            confirm_now: false,
        }
        .confirm_old()
        .await?;
        let (identity, writable) = replication::identity(&mut conn, dialect)
            .await
            .map_err(|e| ApiError::new("replication", format!("database {}: {e}", cfg.name)))?;
        if !writable {
            return Err(ApiError::new(
                "replication",
                format!(
                    "database {}: connected to a replica or a read-only server, not the primary",
                    cfg.name
                ),
            ));
        }
        let mut crypto = open_crypto(&mut conn, dialect, &cfg.name, master_key).await?;
        if !encrypt {
            crypto = crypto.read_only();
        }
        let crypto = Arc::new(crypto);
        storage::writer_timeouts(&mut conn, dialect, wcfg.cluster_stop_ms).await?;
        let read = storage::open_pool(&cfg.url, dialect, cfg.read_pool.unwrap_or(8)).await?;
        let mut replicas = vec![];
        if mode == Mode::AppWait && dialect == Dialect::Mysql {
            for u in &cfg.replication.replica_urls {
                replicas.push(storage::open_pool(u, dialect, 2).await?);
            }
        }
        tracing::info!(
            database = cfg.name,
            mode = mode.as_str(),
            "replication mode"
        );
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
                meta_insert_once(&mut conn, dialect, &key, &max.to_string()).await?;
            }
        }
        let wake: Wake = Arc::new(tokio::sync::watch::channel(max).0);
        let (tx, rx) = mpsc::channel(wcfg.queue_capacity);
        let health = Arc::new(Health::default());
        metrics::gauge!(m::STORAGE_HEALTHY, "database" => cfg.name.clone()).set(1.0);
        let w = Writer {
            conn,
            url: cfg.url.clone(),
            broken: false,
            dialect,
            idx,
            name: cfg.name.clone(),
            shared: shared.clone(),
            health: health.clone(),
            cfg: wcfg.clone(),
            outbox_next: max + 1,
            outbox_patterns: webhooks.iter().flat_map(|w| w.events.clone()).collect(),
            wake: wake.clone(),
            crypto: crypto.clone(),
            mode,
            identity,
            reset_state: false,
            replicas,
            rcfg: cfg.replication.clone(),
            held: Held::default(),
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
                shared,
                mode,
            },
            wake,
        ))
    }

    /// Runs a command where it belongs: on this node's writer, or in a
    /// cluster on the node holding the command's shard (docs/11 §5).
    pub async fn call(&self, op: Op) -> ApiResult<Value> {
        let Some(c) = self.shared.cluster.get() else {
            return self.call_local(op).await;
        };
        let Some(shard) = crate::cluster::shard_for(&op.route(), c.shards()) else {
            return self.call_local(op).await;
        };
        if c.held(self.idx, shard).is_none() {
            return c.forward(self.idx, shard, op).await;
        }
        // The lease can go between the check and the batch: the writer
        // hands the command back untouched, and it goes to the new holder.
        match self.call_local(op.clone()).await {
            Err(e) if e.code == "not_owner" => c.forward(self.idx, shard, op).await,
            r => r,
        }
    }

    /// Whether this node does the database's upkeep (always on a single
    /// node).
    pub fn holds_upkeep(&self) -> bool {
        self.shared.cluster.get().is_none_or(|c| {
            c.held(self.idx, crate::cluster::maintenance_shard(c.shards()))
                .is_some()
        })
    }

    /// Queues a command on this node's writer and waits for its result
    /// (ACK after commit).
    pub async fn call_local(&self, op: Op) -> ApiResult<Value> {
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
                // Spread the retries of everyone refused at once.
                let wait = 500 + (uuid::Uuid::new_v4().as_u128() % 1500) as u64;
                return Err(ApiError::new(
                    "overloaded",
                    "server is busy, retry with the same client_msg_id",
                )
                .retry_after(wait));
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

    /// Reads stop at what the replicas have (docs/11 §8.3).
    pub fn app_wait(&self) -> bool {
        self.mode == Mode::AppWait
    }

    pub fn writer_stats(&self) -> Value {
        json!({
            "database": self.name,
            "dialect": self.dialect.as_str(),
            "replication": self.mode.as_str(),
            "unconfirmed": self.health.unconfirmed.load(Ordering::Relaxed),
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
    url: String,
    /// Cluster: the connection failed; reconnect before the next batch.
    broken: bool,
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
    mode: Mode,
    /// Which primary the writer last wrote to (docs/11 §8.4).
    identity: String,
    /// Cached conversation state is dropped before the next batch (the
    /// primary changed under it).
    reset_state: bool,
    /// MySQL replicas checked directly.
    replicas: Vec<AnyPool>,
    rcfg: crate::config::ReplicationConfig,
    held: Held,
}

/// Committed but not yet confirmed by the replicas (docs/11 §8.2): not
/// delivered, not readable; a later confirmation covers them too.
#[derive(Default)]
struct Held {
    batches: u64,
    outs: Vec<Out>,
    upto: std::collections::BTreeMap<String, i64>,
    wake: Option<i64>,
}

impl Writer {
    /// Whether this batch (and everything held before it) is safe from a
    /// primary failure; advances the read watermark when it is.
    async fn confirm(&mut self) -> Result<(), String> {
        match self.mode {
            Mode::AppWait => {
                // Test-only mutation (docs/11 §12.1): without the wait the
                // replica test must fail.
                let skip = self
                    .shared
                    .failpoints
                    .hit("mutation.no_replica_wait")
                    .await
                    .is_err();
                if !skip {
                    let pos = replication::position(&mut self.conn, self.dialect).await?;
                    if self
                        .shared
                        .failpoints
                        .hit(crate::failpoint::REPLICA_WAIT)
                        .await
                        .is_err()
                    {
                        return Err("injected: the replicas did not confirm".into());
                    }
                    replication::wait(
                        self.dialect,
                        &mut self.conn,
                        &self.identity,
                        &self.replicas,
                        self.rcfg.replicas_required,
                        &pos,
                        Duration::from_millis(self.rcfg.wait_ms),
                    )
                    .await?;
                }
                let upto = std::mem::take(&mut self.held.upto);
                let r = Db {
                    c: &mut self.conn,
                    d: self.dialect,
                    cx: &self.crypto,
                    cap: false,
                    confirm_now: false,
                }
                .set_confirmed(&upto)
                .await;
                if let Err(e) = r {
                    self.held.upto = upto;
                    return Err(e.message);
                }
                Ok(())
            }
            Mode::DatabaseSync if self.dialect == Dialect::Mysql => {
                // Semi-sync falls back to asynchronous on its own.
                if replication::mysql_semi_sync_on(&mut self.conn).await? {
                    Ok(())
                } else {
                    Err("MySQL semi-sync fell back to asynchronous".into())
                }
            }
            _ => Ok(()),
        }
    }

    /// Delivers what was held once a confirmation covers it.
    fn release_held(&mut self) {
        let h = std::mem::take(&mut self.held);
        if h.batches > 0 {
            self.health.unconfirmed.store(0, Ordering::Relaxed);
            metrics::gauge!(m::DB_UNCONFIRMED, "database" => self.name.clone()).set(0.0);
        }
        if let Some(last) = h.wake {
            self.wake.send_replace(last);
        }
        self.shared.apply_tokens(&h.outs);
        if let Some(c) = self.shared.cluster.get() {
            c.publish(self.idx, &h.outs);
        }
        self.shared.reg.deliver(h.outs);
    }

    /// An idle writer with held batches keeps asking the replicas.
    async fn retry_held(&mut self) {
        if self.held.batches == 0 || self.broken {
            return;
        }
        match self.confirm().await {
            Ok(()) => {
                tracing::info!(
                    database = self.name,
                    "held writes confirmed by the replicas"
                );
                self.release_held();
            }
            Err(e) => tracing::debug!(database = self.name, reason = e, "replicas still behind"),
        }
    }

    async fn run(mut self, mut rx: mpsc::Receiver<Job>) {
        let mut st = ShardState::new(&self.shared);
        // Cluster: an idle connection is checked a few times per stop time,
        // so the database's idle limit never ends it and a dead one is
        // noticed (docs/11 §4.6).
        let keepalive = self
            .cfg
            .cluster_stop_ms
            .map(|ms| Duration::from_millis((ms / 4).max(50)));
        loop {
            let wait = if self.held.batches > 0 {
                Some(Duration::from_millis(200))
            } else {
                keepalive
            };
            let first = match wait {
                None => rx.recv().await,
                Some(k) => match tokio::time::timeout(k, rx.recv()).await {
                    Ok(j) => j,
                    Err(_) => {
                        if self.held.batches > 0 {
                            self.retry_held().await;
                        } else {
                            self.ping().await;
                        }
                        continue;
                    }
                },
            };
            let Some(first) = first else { break };
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
            if self.reset_state {
                st = ShardState::new(&self.shared);
                self.reset_state = false;
            }
            self.run_batch(&mut st, batch).await;
        }
    }

    /// Cluster: a new writer connection after a failure.
    async fn reconnect(&mut self) -> bool {
        let r = async {
            let mut c = storage::open_writer(&self.url, self.dialect).await?;
            storage::writer_timeouts(&mut c, self.dialect, self.cfg.cluster_stop_ms).await?;
            // Only the primary takes writes (docs/11 §8.4).
            let (id, writable) = replication::identity(&mut c, self.dialect)
                .await
                .map_err(|e| ApiError::new("storage", e))?;
            if !writable {
                return Err(ApiError::new("storage", "not the primary (read-only)"));
            }
            Ok::<_, ApiError>((c, id))
        };
        match tokio::time::timeout(Duration::from_secs(5), r).await {
            Ok(Ok((c, id))) => {
                if id != self.identity {
                    tracing::warn!(
                        database = self.name,
                        before = self.identity,
                        now = id,
                        "the database primary changed"
                    );
                    metrics::counter!(m::DB_FAILOVERS, "database" => self.name.clone())
                        .increment(1);
                    self.identity = id;
                }
                self.conn = c;
                self.broken = false;
                tracing::info!(database = self.name, "writer reconnected");
                true
            }
            Ok(Err(e)) => {
                tracing::warn!(
                    database = self.name,
                    error = e.message,
                    "writer reconnect failed"
                );
                false
            }
            Err(_) => false,
        }
    }

    /// Cluster: the writer's connection failed or a batch got stuck.
    fn connection_lost(&mut self, why: &str) {
        tracing::warn!(
            database = self.name,
            reason = why,
            "writer connection lost; giving up this database's shards"
        );
        metrics::counter!(m::STORAGE_ERRORS, "kind" => "connection").increment(1);
        self.broken = true;
        if let Some(c) = self.shared.cluster.get() {
            c.reset(self.idx);
        }
    }

    /// The server stopped being the primary, or ended the connection: the
    /// batch's outcome is unknown, cached state and held writes are
    /// dropped, and the next batch first finds the primary (docs/11 §8.4).
    fn primary_lost(&mut self, why: &str) {
        if self.shared.cluster.get().is_some() {
            self.connection_lost(why);
        } else {
            tracing::warn!(
                database = self.name,
                reason = why,
                "the database is not the primary any more"
            );
            metrics::counter!(m::STORAGE_ERRORS, "kind" => "not_primary").increment(1);
            self.broken = true;
        }
        self.reset_state = true;
        self.held = Held::default();
        self.health.unconfirmed.store(0, Ordering::Relaxed);
        metrics::gauge!(m::DB_UNCONFIRMED, "database" => self.name.clone()).set(0.0);
    }

    /// Cluster keepalive on an idle writer.
    async fn ping(&mut self) {
        if self.broken {
            self.reconnect().await;
            return;
        }
        let ok = matches!(
            tokio::time::timeout(
                Duration::from_secs(5),
                sqlx::query("SELECT 1").execute(&mut self.conn)
            )
            .await,
            Ok(Ok(_))
        );
        if !ok {
            self.connection_lost("keepalive failed");
        }
    }

    async fn run_batch(&mut self, st: &mut ShardState, batch: Vec<Job>) {
        // Cluster: commands for shards this node does not hold go back to
        // be routed again (they may have just moved).
        let cluster = self.shared.cluster.get().cloned();
        if self.broken && !self.reconnect().await {
            for j in batch {
                let _ = j.reply.send(Err(ApiError::new(
                    "unavailable",
                    "the database cannot be reached; retry with the same client_msg_id",
                )));
            }
            return;
        }
        let mut batch = batch;
        let mut routed = std::collections::BTreeSet::new();
        if let Some(c) = &cluster {
            let mut keep = Vec::with_capacity(batch.len());
            for j in batch {
                let shards: Vec<u32> = match j.op.route() {
                    crate::cluster::Route::Shards(s) => s,
                    r => crate::cluster::shard_for(&r, c.shards())
                        .into_iter()
                        .collect(),
                };
                if shards.iter().any(|s| c.held(self.idx, *s).is_none()) {
                    let _ = j.reply.send(Err(ApiError::new(
                        "not_owner",
                        "this node does not hold the shard",
                    )));
                    continue;
                }
                routed.extend(shards);
                keep.push(j);
            }
            batch = keep;
            if batch.is_empty() {
                return;
            }
        }
        let n = batch.len();
        // Barriers measure the writer's progress (lease renewal) and wrote
        // nothing: answered at commit, not after the replicas.
        let marks: Vec<bool> = batch.iter().map(|j| matches!(j.op, Op::Barrier)).collect();
        let started = Instant::now();
        let fp = batch
            .iter()
            .any(|j| matches!(j.op, Op::Client { .. }))
            .then(|| self.shared.clone());
        // The answers stay outside the transaction's future, so a batch that
        // fails half-way (or is cut off as stuck) still answers every job.
        let (ops, senders): (Vec<Op>, Vec<oneshot::Sender<ApiResult<Value>>>) =
            batch.into_iter().map(|j| (j.op, j.reply)).unzip();
        let mut results: Vec<ApiResult<Value>> = Vec::with_capacity(n);
        let mut outs: Vec<Out> = Vec::new();
        let mut touched = routed;
        let mut lost: Vec<u32> = vec![];
        let stuck_after = self
            .cfg
            .cluster_stop_ms
            .filter(|_| cluster.is_some())
            .map(|ms| Duration::from_millis(ms * 3 / 4));
        let work = async {
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
            for op in ops {
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
                            cap: false,
                            confirm_now: self.mode != Mode::AppWait,
                        },
                        st,
                        sh: &self.shared,
                        out: &mut out,
                        idx: self.idx,
                        commit_on_error: false,
                        touched: &mut touched,
                    };
                    let injected = !op.is_tick() && take_one(&self.health.fail_next_jobs);
                    let res = if injected {
                        Err(ApiError::new("storage", "injected statement failure"))
                    } else {
                        ctx.run(op).await
                    };
                    let keep = ctx.commit_on_error && !injected;
                    (res, keep)
                };
                if res.is_ok() || keep {
                    sp.commit().await.map_err(|e| e.to_string())?;
                    outs.extend(out);
                } else if let Err(rb) = sp.rollback().await {
                    // MySQL ended the whole transaction on a deadlock and
                    // the savepoint went with it (1305): retryable, on a
                    // new connection (this one's transaction state is
                    // unknown to sqlx now).
                    if res
                        .as_ref()
                        .is_err_and(|e| e.code == "storage" && storage::is_transient(&e.message))
                    {
                        return Err(TRANSIENT_RECONNECT.to_string());
                    }
                    return Err(rb.to_string());
                } else {
                    // Data keys created by the rolled-back job do not exist.
                    self.crypto.rollback_to(mark);
                }
                if let Err(e) = &res
                    && e.code == "storage"
                {
                    metrics::counter!(m::STORAGE_ERRORS, "kind" => "statement").increment(1);
                    // A server that is no longer the primary fails every
                    // statement: the whole batch goes (docs/11 §8.4).
                    if replication::is_not_primary(&e.message) {
                        let _ = tx.rollback().await;
                        return Err(e.message.clone());
                    }
                    // MySQL rolls back the whole transaction on a deadlock:
                    // nothing of the batch can be kept (E2).
                    if storage::is_transient(&e.message) {
                        let _ = tx.rollback().await;
                        return Err(TRANSIENT.to_string());
                    }
                }
                results.push(res);
            }
            let queued = queue_outbox(
                &mut self.outbox_next,
                &self.outbox_patterns,
                &self.name,
                self.dialect,
                &self.crypto,
                &mut tx,
                &outs,
                cluster.is_some(),
            )
            .await
            .map_err(|e| e.message)?;
            if let Some(sh) = &fp {
                sh.failpoints.hit(crate::failpoint::BEFORE_FENCE).await?;
            }
            // Cluster: every shard this batch wrote must still be held by
            // this node, checked under the lease rows' locks until commit
            // (docs/11 §4.3); otherwise nothing of the batch is kept.
            // Test-only mutation (docs/11 §12.1): without the check the
            // stale-holder test must fail.
            let no_fence = self
                .shared
                .failpoints
                .hit("mutation.no_fence")
                .await
                .is_err();
            if let Some(c) = cluster.as_ref().filter(|_| !no_fence) {
                for s in outs.iter().filter_map(|o| match o {
                    Out::Event { ev, .. } => Some(ev.conv_id.as_str()),
                    Out::Side(s) => Some(s.conv_id.as_str()),
                    _ => None,
                }) {
                    touched.insert(crate::cluster::shard_of(s, c.shards()));
                }
                for s in &touched {
                    let ok = match c.writing(self.idx, *s) {
                        None => false,
                        Some(l) => {
                            sqlx::query(self.dialect.q(
                                "UPDATE shard_lease SET fence_count = fence_count + 1 WHERE db = ? AND shard_id = ? AND owner = ? AND epoch = ?",
                            ))
                            .bind(&self.name)
                            .bind(i64::from(*s))
                            .bind(&l.owner)
                            .bind(l.epoch)
                            .execute(&mut *tx)
                            .await
                            .map_err(|e| e.to_string())?
                            .rows_affected()
                                == 1
                        }
                    };
                    if !ok {
                        lost.push(*s);
                    }
                }
                if !lost.is_empty() {
                    let _ = tx.rollback().await;
                    return Err(FENCED.to_string());
                }
            }
            if let Some(sh) = &fp {
                sh.failpoints.hit(crate::failpoint::BEFORE_COMMIT).await?;
            }
            if self.health.fail_next_commits.load(Ordering::SeqCst) > 0 {
                self.health.fail_next_commits.fetch_sub(1, Ordering::SeqCst);
                let _ = tx.rollback().await;
                return Err("injected commit failure".to_string());
            }
            let t = Instant::now();
            tx.commit().await.map_err(|e| {
                let e = e.to_string();
                if storage::is_transient(&e) {
                    TRANSIENT.to_string()
                } else {
                    e
                }
            })?;
            metrics::histogram!(m::COMMIT_DURATION, "database" => self.name.clone())
                .record(t.elapsed().as_secs_f64());
            Ok(queued)
        };
        // A batch that takes longer than the stop time is stuck (docs/11
        // §4.6): its outcome is unknown and the connection is replaced.
        let result: Result<Option<i64>, String> = match stuck_after {
            None => work.await,
            Some(t) => tokio::time::timeout(t, work)
                .await
                .unwrap_or_else(|_| Err(STUCK.to_string())),
        };
        let mut results = results.into_iter();
        let replies: Vec<(oneshot::Sender<ApiResult<Value>>, ApiResult<Value>)> = senders
            .into_iter()
            .map(|r| {
                let res = results
                    .next()
                    .unwrap_or_else(|| Err(ApiError::new("unavailable", "not run")));
                (r, res)
            })
            .collect();

        match result {
            Ok(queued) => {
                self.crypto.commit();
                self.health.jobs.fetch_add(n as u64, Ordering::Relaxed);
                self.health.batches.fetch_add(1, Ordering::Relaxed);
                self.health.max_batch.fetch_max(n as u64, Ordering::Relaxed);
                metrics::histogram!(m::COMMIT_BATCH_SIZE).record(n as f64);
                if let Some(sh) = &fp {
                    let _ = sh.failpoints.hit(crate::failpoint::AFTER_COMMIT).await;
                }
                // Safe from a primary failure only once the replicas have
                // it (docs/11 §8.2): until then nothing is delivered, read
                // or acknowledged, and the next confirmation covers it.
                for o in &outs {
                    if let Out::Event { ev, .. } = o {
                        let e = self.held.upto.entry(ev.conv_id.clone()).or_default();
                        *e = (*e).max(ev.seq);
                    }
                }
                self.held.batches += 1;
                self.held.outs.extend(outs);
                let replies: Vec<_> = replies
                    .into_iter()
                    .zip(marks)
                    .filter_map(|((r, res), mark)| {
                        if mark {
                            let _ = r.send(res);
                            None
                        } else {
                            Some((r, res))
                        }
                    })
                    .collect();
                if queued.is_some() {
                    self.held.wake = queued;
                }
                if let Err(e) = self.confirm().await {
                    metrics::counter!(m::STORAGE_ERRORS, "kind" => "replica_wait").increment(1);
                    self.health
                        .unconfirmed
                        .store(self.held.batches, Ordering::Relaxed);
                    metrics::gauge!(m::DB_UNCONFIRMED, "database" => self.name.clone())
                        .set(self.held.batches as f64);
                    tracing::warn!(
                        database = self.name,
                        reason = e,
                        "write committed but not confirmed by the replicas"
                    );
                    for (r, res) in replies {
                        // Refused commands wrote nothing: their answer stands.
                        let _ = r.send(match res {
                            Ok(_) => Err(ApiError::new(
                                "unavailable",
                                "the database replicas have not confirmed the write yet; retry with the same client_msg_id",
                            )),
                            Err(e) => Err(e),
                        });
                    }
                    return;
                }
                // Durable now: deliver, then acknowledge.
                self.release_held();
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
            Err(e) if e == TRANSIENT || e == TRANSIENT_RECONNECT => {
                if e == TRANSIENT_RECONNECT {
                    self.broken = true;
                }
                // Rolled back whole; nothing is wrong with the database.
                self.crypto.abort();
                metrics::counter!(m::STORAGE_ERRORS, "kind" => "transient").increment(1);
                for (r, _) in replies {
                    let _ = r.send(Err(ApiError::new(
                        "unavailable",
                        "the database was busy; retry with the same client_msg_id",
                    )));
                }
            }
            Err(e) if e == FENCED => {
                // Not a storage failure: another node holds a shard now.
                self.crypto.abort();
                metrics::counter!(m::FENCE_FAILURES).increment(1);
                if let Some(c) = &cluster {
                    for s in &lost {
                        c.lost(self.idx, *s);
                    }
                }
                for (r, _) in replies {
                    let _ = r.send(Err(ApiError::new(
                        "unavailable",
                        "the conversation moved to another node; retry",
                    )));
                }
            }
            Err(e)
                if replication::is_not_primary(&e)
                    || (self.dialect != Dialect::Sqlite && storage::is_connection_lost(&e)) =>
            {
                self.crypto.abort();
                self.primary_lost(&e);
                for (r, _) in replies {
                    let _ = r.send(Err(ApiError::new(
                        "unavailable",
                        "the database primary changed; retry with the same client_msg_id",
                    )));
                }
            }
            Err(e) if cluster.is_some() => {
                // The outcome is unknown: give the shards up and let them be
                // taken again with a new epoch; clients retry with the same
                // client_msg_id (docs/11 §4.5).
                self.crypto.abort();
                self.connection_lost(&e);
                for (r, _) in replies {
                    let _ = r.send(Err(ApiError::new(
                        "unavailable",
                        "the result is unknown; retry with the same client_msg_id",
                    )));
                }
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

/// A batch whose lease check failed (not a storage failure).
const FENCED: &str = "\u{0}fenced";
/// A batch that ran past the stop time (cluster).
const STUCK: &str = "batch stuck past the stop time";
/// A batch rolled back by a deadlock or lock timeout (retryable).
const TRANSIENT: &str = "\u{0}transient";
/// The same, and the connection is replaced before the next batch.
const TRANSIENT_RECONNECT: &str = "\u{0}transient-reconnect";

/// Writes the batch's subscribed events to the webhook outbox, inside
/// the batch transaction. Returns the last id written.
#[allow(clippy::too_many_arguments)]
async fn queue_outbox(
    next: &mut i64,
    patterns: &[String],
    db: &str,
    dialect: Dialect,
    crypto: &crate::crypto::Crypto,
    tx: &mut AnyConnection,
    outs: &[Out],
    cluster: bool,
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
    // Each with the trace of the write (docs/14 §4.1).
    let mut traces = vec![];
    let rows: Vec<std::borrow::Cow<crate::model::Event>> = outs
        .iter()
        .filter_map(|o| match o {
            Out::Event { ev, trace, .. } if crate::webhook::matches(patterns, &ev.kind) => {
                Some((ev, trace))
            }
            _ => None,
        })
        .filter(|(ev, _)| ev.kind == "conv.dissolved" || !dissolved.contains(ev.conv_id.as_str()))
        .map(|(ev, trace)| {
            traces.push(trace.as_ref().and_then(|t| serde_json::to_string(t).ok()));
            ev
        })
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
    if cluster {
        // Several nodes write this outbox: ids come from a counter row held
        // locked until commit, so they also commit in id order (the
        // deliverer never passes an id committed later).
        *next = outbox_ids(dialect, tx, rows.len() as i64).await?;
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
            cap: false,
            confirm_now: false,
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
    let mut traces = traces.into_iter();
    for chunk in rows.chunks(200) {
        let marks = vec!["(?, ?, ?, ?, ?, ?)"; chunk.len()].join(", ");
        let sql = format!(
            "INSERT INTO webhook_outbox (id, conv_id, type, body, created_at, trace) VALUES {marks}"
        );
        let mut q = sqlx::query(dialect.q(&sql));
        for ev in chunk {
            q = q
                .bind(*next)
                .bind(ev.conv_id.clone())
                .bind(ev.kind.clone())
                .bind(bodies.next().unwrap())
                .bind(now)
                .bind(traces.next().flatten());
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

/// Takes `n` outbox ids from the shared counter; returns the first.
async fn outbox_ids(dialect: Dialect, tx: &mut AnyConnection, n: i64) -> ApiResult<i64> {
    let cur = sqlx::query(dialect.q("SELECT v FROM meta WHERE k = 'outbox_next' FOR UPDATE"))
        .fetch_optional(&mut *tx)
        .await?;
    let first = match cur {
        Some(r) => r.try_get::<String, _>(0)?.parse::<i64>().unwrap_or(1),
        None => {
            let max: Option<i64> = sqlx::query("SELECT MAX(id) FROM webhook_outbox")
                .fetch_one(&mut *tx)
                .await?
                .try_get(0)?;
            sqlx::query(dialect.q("INSERT INTO meta (k, v) VALUES ('outbox_next', ?)"))
                .bind((max.unwrap_or(0) + 1).to_string())
                .execute(&mut *tx)
                .await?;
            max.unwrap_or(0) + 1
        }
    };
    sqlx::query(dialect.q("UPDATE meta SET v = ? WHERE k = 'outbox_next'"))
        .bind((first + n).to_string())
        .execute(&mut *tx)
        .await?;
    Ok(first)
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
                format!(
                    "database {name} holds encrypted content: configure its master key (encryption.key_file or AGENTIM_MASTER_KEY), also with encryption.mode = \"off\", to read it; `agentim encryption decrypt-existing` turns it back into plaintext"
                ),
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
            // Another node starting at the same time may record it first.
            let f = meta_insert_once(conn, d, "kek_fingerprint", &kek.fingerprint).await?;
            if f != kek.fingerprint {
                return Err(ApiError::new(
                    "config",
                    format!("database {name} was encrypted with a different master key"),
                ));
            }
        }
    }
    Ok(crate::crypto::Crypto::new(Some(kek)))
}

/// Records a meta value that is set once (at start-up); when another
/// process starting at the same moment wrote it first, its value is
/// returned instead.
async fn meta_insert_once(
    conn: &mut AnyConnection,
    d: Dialect,
    k: &str,
    v: &str,
) -> ApiResult<String> {
    let r = sqlx::query(d.q("INSERT INTO meta (k, v) VALUES (?, ?)"))
        .bind(k.to_string())
        .bind(v.to_string())
        .execute(&mut *conn)
        .await;
    match r {
        Ok(_) => Ok(v.to_string()),
        Err(e) => {
            let have = sqlx::query(d.q("SELECT v FROM meta WHERE k = ?"))
                .bind(k.to_string())
                .fetch_optional(&mut *conn)
                .await?;
            match have {
                Some(row) => Ok(row.try_get(0)?),
                None => Err(e.into()),
            }
        }
    }
}
