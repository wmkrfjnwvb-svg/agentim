//! Several AgentIM nodes serving the same databases (docs/11-cluster.md).
//!
//! Each database's conversations are split into virtual shards; a node
//! writes a shard only while it holds the shard's lease in that database,
//! and every batch re-checks the leases it relies on in its own
//! transaction (the core's fence). Requests for a shard held elsewhere are
//! forwarded to its holder; what a holder commits is published to every
//! other node for the connections there.
//!
//! Start-up: `Hub::open_deferred` → [`join`] → [`Node::start`] →
//! `Hub::finish_open` (its writes go to the shards' holders).

pub mod assign;
pub mod ca;
pub mod codec;
pub mod peer;
pub mod sql;

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant};

use agentim_core::cluster::{Cluster, Lease, control_shard};
use agentim_core::config::ClusterConfig;
use agentim_core::hub::Hub;
use agentim_core::logic::Op;
use agentim_core::model::{ApiError, ApiResult};
use agentim_core::registry::Out;
use agentim_core::storage::{self, Dialect};
use serde_json::Value;
use sqlx::AnyPool;
use tokio::net::TcpListener;
use tokio::sync::Notify;

pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

/// Membership timing, from the lease time (docs/11 §7): heartbeat every
/// 2 s (at most the renewal interval), offline after two thirds of a lease
/// (10 s at the default 15 s), and shards are given to a node only after it
/// has been seen for two renewals.
fn offline_ms(c: &ClusterConfig) -> i64 {
    (c.lease_ms * 2 / 3) as i64
}

fn heartbeat_every(c: &ClusterConfig) -> Duration {
    Duration::from_millis(c.renew_ms.min(2_000))
}

fn settle(c: &ClusterConfig) -> Duration {
    Duration::from_millis(c.renew_ms * 2)
}

/// Shards handed to their preferred nodes per database per round.
const HAND_PER_ROUND: usize = 4;
/// Shards taken per database per round.
const TAKE_PER_ROUND: usize = 10;
/// Waiting for a forwarded command's answer; past it the result is
/// unknown and the caller retries with the same idempotency key.
pub const FORWARD_WAIT: Duration = Duration::from_secs(5);
/// How long a request follows a shard that is moving before it answers
/// `unavailable` (docs/11 §5).
pub const FORWARD_FOLLOW: Duration = Duration::from_secs(4);
/// Lease rounds failing in a row before the node stops taking clients.
pub const FAILED_ROUNDS: u32 = 5;

fn unavailable(why: &str) -> ApiError {
    ApiError::new(
        "unavailable",
        format!("{why}; retry with the same client_msg_id"),
    )
}

struct Held {
    epoch: i64,
    /// Local deadline: past it this node stops writing the shard, measured
    /// from before the renewal was sent (docs/11 §4.4).
    until: Instant,
    /// Being handed over: no new commands, but batches already admitted
    /// still commit under this lease.
    draining: bool,
}

struct DbLeases {
    idx: usize,
    name: String,
    dialect: Dialect,
    pool: AnyPool,
    held: RwLock<HashMap<u32, Held>>,
    /// Who holds each shard, as last read (for forwarding).
    owners: RwLock<HashMap<u32, Option<String>>>,
    /// Shards lost recently: not taken back for one lease period.
    cooldown: Mutex<HashMap<u32, Instant>>,
    /// Shards taken from a node whose lease lapsed: not handed back to it
    /// for a while (it may still be stuck; no ping-pong).
    rescued: Mutex<HashMap<u32, Instant>>,
}

#[derive(Clone)]
struct LiveNode {
    node_id: String,
    boot_id: String,
    addr: String,
    since: Instant,
    /// Handing its shards over before it stops: still reachable, but no
    /// longer anybody's choice for a shard.
    leaving: bool,
}

struct Inner {
    cfg: ClusterConfig,
    node_id: String,
    boot_id: String,
    owner: String,
    cluster_id: String,
    shards: u32,
    control: usize,
    hello: codec::Hello,
    tls: ca::Tls,
    hub: OnceLock<Hub>,
    dbs: Vec<DbLeases>,
    live: RwLock<Vec<LiveNode>>,
    peers: Mutex<HashMap<String, Arc<peer::Peer>>>,
    /// Nodes that connected to this one before (a reconnect may have lost
    /// pushes).
    seen: Mutex<HashSet<String>>,
    nudge: Notify,
    started: Instant,
    stopped: AtomicBool,
    /// Leaving the cluster (planned): not ready, shards being handed over.
    leaving: AtomicBool,
    /// Lease rounds in a row that could not renew, and whether the current
    /// round failed.
    fail_streak: AtomicU32,
    round_failed: AtomicBool,
    tasks: Mutex<Vec<tokio::task::JoinHandle<()>>>,
    /// The listener for other nodes; it outlives the other tasks when
    /// leaving.
    serving: Mutex<Option<tokio::task::JoinHandle<()>>>,
    listener: Mutex<Option<TcpListener>>,
    pub_addr: String,
    slot: AtomicU32,
    /// Test faults: network cuts and a local clock offset (docs/11 §12.1).
    faults: Arc<peer::Faults>,
    skew_ms: std::sync::atomic::AtomicU64,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// A joined node. Cloning shares it.
#[derive(Clone)]
pub struct Node {
    inner: Arc<Inner>,
}

struct Handle(Arc<Inner>);

/// Joins the cluster: start-up checks, set-up of the cluster tables,
/// registration of this node, certificates, and the listener. The hub
/// must come from `Hub::open_deferred`; it is attached here.
pub async fn join(hub: &Hub) -> Result<Node, String> {
    let cfg = hub.config().cluster.clone();
    if !cfg.enabled {
        return Err("cluster.enabled is false".into());
    }
    cfg.check()?;
    let secret = match &cfg.secret {
        Some(s) if !s.is_empty() => s.clone(),
        _ => hub
            .config()
            .master_key()?
            .ok_or("a cluster needs cluster.secret or an encryption master key")?,
    };
    let names = hub.database_names().to_vec();
    let control = names
        .iter()
        .position(|n| *n == hub.config().storage.control)
        .ok_or("no control database")?;
    let mut pools = vec![];
    for d in &hub.config().storage.databases {
        let dialect = Dialect::from_url(&d.url)?;
        if dialect == Dialect::Sqlite {
            return Err(format!(
                "database {}: a cluster needs PostgreSQL or MySQL",
                d.name
            ));
        }
        let pool = storage::open_pool(&d.url, dialect, 2)
            .await
            .map_err(|e| e.message)?;
        sql::check_server(&pool, dialect)
            .await
            .map_err(|e| format!("database {}: {e}", d.name))?;
        sql::ensure_schema(&pool).await?;
        pools.push((d.name.clone(), dialect, pool));
    }
    // The control database decides the cluster's id, shard count and CA.
    let (cname, cd, cpool) = &pools[control];
    let candidate = uuid::Uuid::new_v4().simple().to_string();
    let (ca_cert, ca_key) = ca::new_ca(&candidate)?;
    let extra = [
        ("ca_cert", ca_cert),
        ("ca_key", ca::seal(&secret, &candidate, &ca_key)?),
    ];
    let (cluster_id, shards) =
        sql::init_db(cpool, *cd, cname, &candidate, cfg.shards, &extra).await?;
    if shards != cfg.shards {
        return Err(format!(
            "cluster.shards is {} but the cluster was set up with {shards}; it cannot change",
            cfg.shards
        ));
    }
    for (i, (name, d, pool)) in pools.iter().enumerate() {
        if i == control {
            continue;
        }
        let (id, n) = sql::init_db(pool, *d, name, &cluster_id, shards, &[]).await?;
        if id != cluster_id || n != shards {
            return Err(format!(
                "database {name} belongs to another cluster ({id}, {n} shards)"
            ));
        }
    }
    let ca_cert = sql::meta_get(cpool, *cd, "ca_cert")
        .await?
        .ok_or("no cluster CA")?;
    let ca_key = ca::open(
        &secret,
        &cluster_id,
        &sql::meta_get(cpool, *cd, "ca_key")
            .await?
            .ok_or("no cluster CA key")?,
    )?;
    let tls = ca::Tls::new(&cluster_id, &ca_cert, &ca_key, &cfg.node_id)?;
    let listener = TcpListener::bind(&cfg.listen)
        .await
        .map_err(|e| format!("cluster.listen {}: {e}", cfg.listen))?;
    let pub_addr = match &cfg.advertise {
        Some(a) => a.clone(),
        None => {
            let a = listener.local_addr().map_err(|e| e.to_string())?;
            if a.ip().is_unspecified() {
                return Err(
                    "cluster.advertise is needed when cluster.listen is 0.0.0.0 or [::]".into(),
                );
            }
            a.to_string()
        }
    };
    let boot_id = uuid::Uuid::new_v4().simple().to_string();
    let slot = register(
        cpool,
        *cd,
        &cfg.node_id,
        &boot_id,
        &pub_addr,
        offline_ms(&cfg),
    )
    .await?;
    hub.set_session_slot(slot);
    let owner = format!("{}:{boot_id}", cfg.node_id);
    let hello = codec::Hello {
        cluster_id: cluster_id.clone(),
        node_id: cfg.node_id.clone(),
        boot_id: boot_id.clone(),
        proto_min: codec::PROTO_MIN,
        proto_max: codec::PROTO_MAX,
        version: env!("CARGO_PKG_VERSION").into(),
        capabilities: codec::CAPABILITIES.iter().map(|c| c.to_string()).collect(),
    };
    let dbs = pools
        .into_iter()
        .enumerate()
        .map(|(idx, (name, dialect, pool))| DbLeases {
            idx,
            name,
            dialect,
            pool,
            held: Default::default(),
            owners: Default::default(),
            cooldown: Default::default(),
            rescued: Default::default(),
        })
        .collect();
    let inner = Arc::new(Inner {
        node_id: cfg.node_id.clone(),
        cfg,
        boot_id,
        owner,
        cluster_id,
        shards,
        control,
        hello,
        tls,
        hub: OnceLock::new(),
        dbs,
        live: Default::default(),
        peers: Default::default(),
        seen: Default::default(),
        nudge: Notify::new(),
        started: Instant::now(),
        stopped: AtomicBool::new(false),
        leaving: AtomicBool::new(false),
        fail_streak: AtomicU32::new(0),
        round_failed: AtomicBool::new(false),
        tasks: Default::default(),
        serving: Default::default(),
        listener: Mutex::new(Some(listener)),
        pub_addr,
        slot: AtomicU32::new(slot),
        faults: Default::default(),
        skew_ms: Default::default(),
    });
    let _ = inner.hub.set(hub.clone());
    hub.attach_cluster(Arc::new(Handle(inner.clone())));
    tracing::info!(
        node = inner.node_id,
        cluster = inner.cluster_id,
        addr = inner.pub_addr,
        "joined the cluster"
    );
    Ok(Node { inner })
}

/// For the server programs (`cli::run_with`): joins and starts; the node
/// runs until the process ends.
pub fn join_and_start(hub: Hub) -> agentim_core::cluster::BoxFuture<Result<(), String>> {
    Box::pin(async move {
        let node = join(&hub).await?;
        node.start().await?;
        // Leaving on shutdown hands the shards over at once.
        let n = node.clone();
        tokio::spawn(async move {
            shutdown_signal().await;
            tracing::info!("leaving the cluster");
            n.leave().await;
            std::process::exit(0);
        });
        Ok(())
    })
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut term = signal(SignalKind::terminate()).expect("signal handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

/// Registers this process under its node id; a previous process with the
/// same id must have stopped (its heartbeat goes stale) first.
async fn register(
    pool: &AnyPool,
    d: Dialect,
    node_id: &str,
    boot_id: &str,
    addr: &str,
    offline: i64,
) -> Result<u32, String> {
    let version = env!("CARGO_PKG_VERSION");
    let deadline = Instant::now() + Duration::from_millis(offline as u64 + 2_000);
    loop {
        let (rows, now) = sql::nodes(pool, d).await?;
        let row = rows.into_iter().find(|r| r.node_id == node_id);
        let ok = match &row {
            None => sql::register(pool, d, node_id, None, boot_id, addr, version).await?,
            Some(r) if r.state == "left" || now - r.heartbeat_at > offline => {
                sql::register(pool, d, node_id, Some(&r.boot_id), boot_id, addr, version).await?
            }
            Some(r) => {
                if Instant::now() > deadline {
                    return Err(format!(
                        "cluster.node_id {node_id} is in use by a running process at {}",
                        r.addr
                    ));
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
                continue;
            }
        };
        if ok {
            let (rows, _) = sql::nodes(pool, d).await?;
            return rows
                .into_iter()
                .find(|r| r.node_id == node_id && r.boot_id == boot_id)
                .map(|r| r.slot)
                .ok_or_else(|| "registration lost".to_string());
        }
        if Instant::now() > deadline {
            return Err(format!("could not register cluster.node_id {node_id}"));
        }
    }
}

impl Node {
    /// Starts the heartbeat, the leases and the connections, and returns
    /// after the first round (the preferred shards are taken).
    pub async fn start(&self) -> Result<(), String> {
        let i = self.inner.clone();
        let listener = lock(&i.listener).take().ok_or("already started")?;
        let serve: Arc<dyn peer::Serve> = Arc::new(Handle(i.clone()));
        *lock(&i.serving) = Some(tokio::spawn(peer::serve(
            listener,
            i.hello.clone(),
            i.tls.server.clone(),
            serve,
        )));
        let mut tasks = vec![];
        i.membership_round().await;
        // Others see this node before it takes anything.
        for db in &i.dbs {
            i.lease_round(db).await;
        }
        tasks.push(tokio::spawn(i.clone().membership_loop()));
        tasks.push(tokio::spawn(i.clone().lease_loop()));
        tasks.push(tokio::spawn(i.clone().refresh_loop()));
        lock(&i.tasks).extend(tasks);
        let _ = sql::heartbeat(
            &i.dbs[i.control].pool,
            i.dbs[i.control].dialect,
            &i.node_id,
            &i.boot_id,
            "active",
        )
        .await;
        Ok(())
    }

    pub fn node_id(&self) -> &str {
        &self.inner.node_id
    }

    pub fn cluster_id(&self) -> &str {
        &self.inner.cluster_id
    }

    pub fn slot(&self) -> u32 {
        self.inner.slot.load(Ordering::Relaxed)
    }

    /// Shards this node holds in database `db` (for tests and status).
    pub fn held(&self, db: usize) -> Vec<u32> {
        let now = self.inner.now();
        let mut v: Vec<u32> = self.inner.dbs[db]
            .held
            .read()
            .unwrap()
            .iter()
            .filter(|(_, h)| h.until > now && !h.draining)
            .map(|(s, _)| *s)
            .collect();
        v.sort();
        v
    }

    /// Runs a lease round now.
    pub fn nudge(&self) {
        self.inner.nudge.notify_one();
    }

    /// Leaves: stops writing, gives every lease back so others take over
    /// at once, and stops the heartbeat.
    pub async fn leave(&self) {
        let i = &self.inner;
        if i.leaving.swap(true, Ordering::SeqCst) {
            return;
        }
        // Not ready any more: load balancers stop sending new clients.
        for t in lock(&i.tasks).drain(..) {
            t.abort();
        }
        i.membership_round().await;
        let c = &i.dbs[i.control];
        let _ = sql::heartbeat(&c.pool, c.dialect, &i.node_id, &i.boot_id, "leaving").await;
        // Every shard goes straight to its next preferred node, so nothing
        // waits for a lease to run out (docs/11 §7).
        let others: Vec<String> = i
            .live_ids(false)
            .into_iter()
            .filter(|n| *n != i.node_id)
            .collect();
        for db in &i.dbs {
            let held: Vec<(u32, i64)> = db
                .held
                .read()
                .unwrap()
                .iter()
                .map(|(s, h)| (*s, h.epoch))
                .collect();
            for (s, e) in held {
                let next = assign::preferred(&others, &db.name, s).map(str::to_string);
                if !i.hand_over(db, s, e, next.as_deref()).await && next.is_some() {
                    // Not reachable: release it for anyone to take.
                    let _ = sql::hand_over(&db.pool, db.dialect, &db.name, s, &i.owner, e, None, 0)
                        .await;
                }
            }
            db.held.write().unwrap().clear();
        }
        i.stopped.store(true, Ordering::SeqCst);
        let _ = sql::heartbeat(&c.pool, c.dialect, &i.node_id, &i.boot_id, "left").await;
        // Clients reconnect to another node; requests still on their way
        // here are answered (`not_owner`) for a moment, then the listener
        // closes.
        i.hub().go_away();
        let peers: Vec<Arc<peer::Peer>> = lock(&i.peers).drain().map(|(_, p)| p).collect();
        if let Ok(f) = codec::encode(codec::Kind::Check, b"") {
            for p in &peers {
                p.send(f.clone());
            }
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
        for p in peers {
            p.stop();
        }
        if let Some(t) = lock(&i.serving).take() {
            t.abort();
        }
    }

    /// Test fault: no traffic between this node and `other` either way
    /// (call on both nodes for a full partition).
    pub fn cut(&self, other: &str) {
        lock(&self.inner.faults.cut_to).insert(other.into());
        lock(&self.inner.faults.cut_from).insert(other.into());
        if let Some(p) = lock(&self.inner.peers).get(other) {
            p.stop_session();
        }
    }

    /// Test fault: frames from this node to `other` are lost (one way).
    pub fn cut_to(&self, other: &str) {
        lock(&self.inner.faults.cut_to).insert(other.into());
    }

    /// Test fault: frames to `other` wait `by` each (in order).
    pub fn delay_to(&self, other: &str, by: Duration) {
        lock(&self.inner.faults.delay_to).insert(other.into(), by);
    }

    /// Test fault: frames to `other` are kept back until `release_to`.
    pub fn hold_to(&self, other: &str) {
        if let Some(p) = lock(&self.inner.peers).get(other) {
            p.hold();
        }
    }

    pub fn release_to(&self, other: &str) {
        if let Some(p) = lock(&self.inner.peers).get(other) {
            p.release();
        }
    }

    /// For tests: what a peer would publish, as received from the wire.
    #[doc(hidden)]
    pub fn receive_publish(&self, body: &[u8]) {
        peer::Serve::publish(&Handle(self.inner.clone()), body);
    }

    /// For tests: a forwarded command as received from the wire (an older
    /// or newer node may send one this version does not know).
    #[doc(hidden)]
    pub async fn receive_forward(&self, db: usize, shard: u32, op: Value) -> codec::ForwardResp {
        peer::Serve::forward(
            &Handle(self.inner.clone()),
            codec::ForwardReq {
                id: 1,
                db,
                shard,
                op,
            },
        )
        .await
    }

    /// Whether this node takes clients (readiness), and why not.
    pub fn ready(&self) -> Result<(), String> {
        Cluster::ready(&Handle(self.inner.clone()))
    }

    pub fn heal(&self) {
        lock(&self.inner.faults.cut_to).clear();
        lock(&self.inner.faults.cut_from).clear();
        lock(&self.inner.faults.delay_to).clear();
        for p in lock(&self.inner.peers).values() {
            p.release();
        }
    }

    /// Test fault: this node's clock jumps forward.
    pub fn jump_clock(&self, by: Duration) {
        self.inner
            .skew_ms
            .fetch_add(by.as_millis() as u64, Ordering::SeqCst);
    }

    /// Stops like a crash: tasks and connections end, nothing is released
    /// (others take over when the leases expire). For tests.
    pub fn crash(&self) {
        let i = &self.inner;
        i.stopped.store(true, Ordering::SeqCst);
        if let Some(t) = lock(&i.serving).take() {
            t.abort();
        }
        for t in lock(&i.tasks).drain(..) {
            t.abort();
        }
        for db in &i.dbs {
            db.held.write().unwrap().clear();
        }
        for (_, p) in lock(&i.peers).drain() {
            p.stop();
        }
    }
}

impl Inner {
    /// This node's monotonic clock (tests can move it forward).
    fn now(&self) -> Instant {
        Instant::now() + Duration::from_millis(self.skew_ms.load(Ordering::Relaxed))
    }

    /// How long the lease loop waits for its own writer: never longer,
    /// so a stuck writer cannot hold up renewals (the gate decides those).
    fn writer_budget(&self) -> Duration {
        Duration::from_millis(
            self.cfg
                .stop_ms
                .saturating_sub(self.cfg.renew_ms + self.cfg.op_timeout_ms)
                .max(100),
        )
    }

    /// A writer command with a bounded wait; a command not answered in time
    /// stays queued and runs before anything queued after it.
    async fn bounded(
        &self,
        f: impl std::future::Future<Output = ApiResult<Value>>,
    ) -> ApiResult<Value> {
        tokio::time::timeout(self.writer_budget(), f)
            .await
            .unwrap_or_else(|_| Err(unavailable("the writer is busy")))
    }

    fn hub(&self) -> &Hub {
        self.hub.get().expect("attached")
    }

    fn op_timeout(&self) -> Duration {
        Duration::from_millis(self.cfg.op_timeout_ms)
    }

    async fn membership_loop(self: Arc<Self>) {
        let mut t = tokio::time::interval(heartbeat_every(&self.cfg));
        t.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            t.tick().await;
            self.membership_round().await;
        }
    }

    async fn membership_round(&self) {
        let c = &self.dbs[self.control];
        let state = if self.started.elapsed() < settle(&self.cfg) {
            "joining"
        } else {
            "active"
        };
        match tokio::time::timeout(
            self.op_timeout(),
            sql::heartbeat(&c.pool, c.dialect, &self.node_id, &self.boot_id, state),
        )
        .await
        {
            Ok(Ok(true)) => {}
            Ok(Ok(false)) => {
                // Another process registered under this node id: this one
                // must not write anything any more.
                tracing::error!(
                    node = self.node_id,
                    "another process took over this node id; stopping"
                );
                self.stopped.store(true, Ordering::SeqCst);
                for db in &self.dbs {
                    db.held.write().unwrap().clear();
                }
                return;
            }
            _ => return,
        }
        let Ok(Ok((rows, now))) =
            tokio::time::timeout(self.op_timeout(), sql::nodes(&c.pool, c.dialect)).await
        else {
            return;
        };
        let old: HashMap<(String, String), Instant> = self
            .live
            .read()
            .unwrap()
            .iter()
            .map(|n| ((n.node_id.clone(), n.boot_id.clone()), n.since))
            .collect();
        let live: Vec<LiveNode> = rows
            .into_iter()
            .filter(|r| r.state != "left" && now - r.heartbeat_at <= offline_ms(&self.cfg))
            .map(|r| LiveNode {
                since: old
                    .get(&(r.node_id.clone(), r.boot_id.clone()))
                    .copied()
                    .unwrap_or_else(Instant::now),
                leaving: r.state == "leaving",
                node_id: r.node_id,
                boot_id: r.boot_id,
                addr: r.addr,
            })
            .collect();
        // One connection per live peer boot.
        {
            let mut peers = lock(&self.peers);
            peers.retain(|id, p| {
                let keep = live
                    .iter()
                    .any(|n| n.node_id == *id && n.boot_id == p.boot_id && n.addr == p.addr);
                if !keep {
                    p.stop();
                }
                keep
            });
            for n in &live {
                if n.node_id != self.node_id && !peers.contains_key(&n.node_id) {
                    peers.insert(
                        n.node_id.clone(),
                        peer::Peer::start(
                            n.node_id.clone(),
                            n.boot_id.clone(),
                            n.addr.clone(),
                            self.hello.clone(),
                            self.tls.client.clone(),
                            self.faults.clone(),
                        ),
                    );
                }
            }
        }
        *self.live.write().unwrap() = live;
    }

    async fn lease_loop(self: Arc<Self>) {
        let every = Duration::from_millis(self.cfg.renew_ms);
        loop {
            let nudged = tokio::time::timeout(every, self.nudge.notified())
                .await
                .is_ok();
            if self.stopped.load(Ordering::SeqCst) {
                return;
            }
            if nudged {
                // Something changed (a node left, a shard was handed over):
                // see the current members before deciding.
                self.membership_round().await;
            }
            self.round_failed.store(false, Ordering::SeqCst);
            for db in &self.dbs {
                self.lease_round(db).await;
            }
            self.count_round();
        }
    }

    /// After `FAILED_ROUNDS` lease rounds in a row that could not renew, the
    /// node reports not ready and sends its clients to other nodes rather
    /// than only logging (docs/11 §4.6); it takes clients again once
    /// renewals succeed.
    fn count_round(&self) {
        if self.round_failed.load(Ordering::SeqCst) {
            let n = self.fail_streak.fetch_add(1, Ordering::SeqCst) + 1;
            if n == FAILED_ROUNDS {
                tracing::error!(
                    node = self.node_id,
                    "lease renewals keep failing; sending clients to other nodes"
                );
                self.hub().go_away();
            }
        } else if self.fail_streak.swap(0, Ordering::SeqCst) >= FAILED_ROUNDS {
            tracing::info!(node = self.node_id, "lease renewals work again");
            self.hub().come_back();
        }
    }

    /// Fallback for lost invalidations (docs/11 §6.5): the cache epoch is
    /// read every 2 seconds.
    async fn refresh_loop(self: Arc<Self>) {
        let mut t = tokio::time::interval(Duration::from_secs(2));
        loop {
            t.tick().await;
            // Test-only mutation: without the poll, a lost invalidation is
            // never made up for (the cut-off revocation test fails).
            if self
                .hub()
                .shared()
                .failpoints
                .hit("mutation.no_cache_poll")
                .await
                .is_err()
            {
                continue;
            }
            if let Err(e) = self.hub().refresh_auth().await {
                tracing::debug!(error = e.message, "cache refresh failed");
            }
        }
    }

    fn live_ids(&self, settled: bool) -> Vec<String> {
        let now = Instant::now();
        let settle = settle(&self.cfg);
        self.live
            .read()
            .unwrap()
            .iter()
            .filter(|n| !n.leaving)
            .filter(|n| {
                !settled || n.node_id == self.node_id || now.duration_since(n.since) >= settle
            })
            .map(|n| n.node_id.clone())
            .collect()
    }

    fn owner_of(&self, node_id: &str) -> Option<String> {
        self.live
            .read()
            .unwrap()
            .iter()
            .find(|n| n.node_id == node_id)
            .map(|n| format!("{}:{}", n.node_id, n.boot_id))
    }

    /// One round on one database (docs/11 §4.2): renew, compare with the
    /// table, take free preferred shards, hand one shard to its preferred
    /// node.
    async fn lease_round(&self, db: &DbLeases) {
        if self.stopped.load(Ordering::SeqCst) {
            return;
        }
        let t = self.op_timeout();
        let lease_ms = self.cfg.lease_ms;
        let t0 = self.now();
        // Renewal follows the writer's progress: a writer that is stuck or
        // cut off from the database lets its leases lapse (docs/11 §4.6).
        let budget = Duration::from_millis(
            self.cfg
                .stop_ms
                .saturating_sub(self.cfg.renew_ms + self.cfg.op_timeout_ms)
                .max(100),
        );
        let gate_off = self
            .hub()
            .shared()
            .failpoints
            .hit("mutation.no_progress_gate")
            .await
            .is_err();
        let writing = gate_off
            || match self.hub().shard(db.idx) {
                Some(w) => matches!(
                    tokio::time::timeout(budget, w.call_local(Op::Barrier)).await,
                    Ok(Ok(_))
                ),
                None => false,
            };
        let renewed = writing
            && matches!(
                tokio::time::timeout(
                    t,
                    sql::renew(&db.pool, db.dialect, &db.name, &self.owner, lease_ms)
                )
                .await,
                Ok(Ok(_))
            );
        let Ok(Ok((rows, now))) =
            tokio::time::timeout(t, sql::leases(&db.pool, db.dialect, &db.name)).await
        else {
            metrics::counter!("agentim_cluster_renew_failures_total").increment(1);
            self.round_failed.store(true, Ordering::SeqCst);
            return;
        };
        if !renewed {
            metrics::counter!("agentim_cluster_renew_failures_total").increment(1);
            self.round_failed.store(true, Ordering::SeqCst);
        }
        let until = t0 + Duration::from_millis(self.cfg.stop_ms);
        let mut lost = vec![];
        let mut handed = vec![];
        {
            let mut held = db.held.write().unwrap();
            let mut owners = db.owners.write().unwrap();
            for r in &rows {
                let valid = r.expires_at > now;
                owners.insert(r.shard, r.owner.clone().filter(|_| valid));
                let mine = valid && r.owner.as_deref() == Some(self.owner.as_str());
                match held.get_mut(&r.shard) {
                    Some(h) if mine && h.epoch == r.epoch => {
                        if renewed {
                            h.until = until;
                        }
                    }
                    Some(_) => {
                        held.remove(&r.shard);
                        lost.push(r.shard);
                    }
                    None if mine && renewed => handed.push((r.shard, r.epoch)),
                    None => {}
                }
            }
        }
        if !lost.is_empty() {
            self.forget(db, &lost).await;
        }
        if !renewed || self.stopped.load(Ordering::SeqCst) {
            return;
        }
        // Free shards this node should take: its preferred ones at once,
        // others' a renewal later (their preferred node had its chance).
        let live = self.live_ids(false);
        let now_i = Instant::now();
        lock(&db.cooldown).retain(|_, until| *until > now_i);
        let cooling: HashSet<u32> = lock(&db.cooldown).keys().copied().collect();
        let held_now: HashSet<u32> = db.held.read().unwrap().keys().copied().collect();
        let offset = (uuid::Uuid::new_v4().as_u128() % rows.len().max(1) as u128) as usize;
        // Shards this node owns in the table but does not use (handed over
        // to it, or given up after a writer failure) are taken again with a
        // new epoch: that waits for any batch still holding the lease row,
        // so the state reloaded afterwards is final (docs/11 §4.5).
        let mut taken = vec![];
        for (shard, epoch) in handed {
            let t1 = self.now();
            if let Ok(Ok(Some(e))) = tokio::time::timeout(
                t,
                sql::retake(
                    &db.pool,
                    db.dialect,
                    &db.name,
                    shard,
                    &self.owner,
                    epoch,
                    lease_ms,
                ),
            )
            .await
            {
                taken.push((shard, e, t1 + Duration::from_millis(self.cfg.stop_ms)));
            }
        }
        let mut tries = 0;
        for k in 0..rows.len() {
            let r = &rows[(offset + k) % rows.len()];
            if tries >= TAKE_PER_ROUND {
                break;
            }
            if held_now.contains(&r.shard)
                || cooling.contains(&r.shard)
                || taken.iter().any(|x| x.0 == r.shard)
            {
                continue;
            }
            let pref = assign::preferred(&live, &db.name, r.shard) == Some(self.node_id.as_str());
            let grace = if pref { 0 } else { self.cfg.renew_ms };
            if r.expires_at >= now - grace as i64 {
                continue;
            }
            tries += 1;
            let t1 = self.now();
            if let Ok(Ok(Some(epoch))) = tokio::time::timeout(
                t,
                sql::acquire(
                    &db.pool,
                    db.dialect,
                    &db.name,
                    r.shard,
                    &self.owner,
                    lease_ms,
                    grace,
                ),
            )
            .await
            {
                taken.push((r.shard, epoch, t1 + Duration::from_millis(self.cfg.stop_ms)));
                if r.owner.as_deref().is_some_and(|o| o != self.owner) {
                    lock(&db.rescued).insert(
                        r.shard,
                        Instant::now() + Duration::from_millis(self.cfg.lease_ms * 3),
                    );
                }
            }
        }
        if tries >= TAKE_PER_ROUND {
            // More may be free: another round at once.
            self.nudge.notify_one();
        }
        if !taken.is_empty() {
            self.take(db, taken).await;
        }
        self.rebalance(db).await;
    }

    /// Starts writing shards just taken: their old in-memory state goes,
    /// interrupted streams are ended, and the control shard's holder runs
    /// the repairs.
    async fn take(&self, db: &DbLeases, taken: Vec<(u32, i64, Instant)>) {
        let shards: Vec<u32> = taken.iter().map(|t| t.0).collect();
        let Some(shard) = self.hub().shard(db.idx) else {
            return;
        };
        let _ = self
            .bounded(shard.call_local(Op::Forget {
                shards: shards.clone(),
            }))
            .await;
        {
            let mut held = db.held.write().unwrap();
            let mut owners = db.owners.write().unwrap();
            for (s, epoch, until) in &taken {
                held.insert(
                    *s,
                    Held {
                        epoch: *epoch,
                        until: *until,
                        draining: false,
                    },
                );
                owners.insert(*s, Some(self.owner.clone()));
            }
        }
        tracing::info!(node = self.node_id, database = db.name, shards = ?shards, "took shards");
        metrics::counter!("agentim_cluster_takeovers_total").increment(shards.len() as u64);
        let convs: Vec<u32> = shards
            .iter()
            .copied()
            .filter(|s| *s < self.shards)
            .collect();
        if !convs.is_empty()
            && let Err(e) = self
                .bounded(shard.call_local(Op::Recover {
                    shards: Some(convs),
                }))
                .await
        {
            tracing::warn!(
                database = db.name,
                error = e.message,
                "recovery after takeover failed"
            );
        }
        if db.idx == self.control && shards.contains(&control_shard(self.shards)) {
            let hub = self.hub().clone();
            tokio::spawn(async move {
                if let Err(e) = hub.repair().await {
                    tracing::warn!(error = e.message, "repair after takeover failed");
                }
            });
        }
    }

    async fn forget(&self, db: &DbLeases, lost: &[u32]) {
        let until = Instant::now() + Duration::from_millis(self.cfg.lease_ms);
        {
            let mut c = lock(&db.cooldown);
            for s in lost {
                c.insert(*s, until);
            }
        }
        tracing::warn!(node = self.node_id, database = db.name, shards = ?lost, "lost shards");
        if let Some(shard) = self.hub().shard(db.idx) {
            let _ = self
                .bounded(shard.call_local(Op::Forget {
                    shards: lost.to_vec(),
                }))
                .await;
        }
    }

    /// Gives one shard to its preferred node when that node has been up
    /// for a while (a node that joins gets its share).
    async fn rebalance(&self, db: &DbLeases) {
        let live = self.live_ids(true);
        if live.len() < 2 {
            return;
        }
        for _ in 0..HAND_PER_ROUND {
            if !self.hand_one(db, &live).await {
                return;
            }
        }
    }

    async fn hand_one(&self, db: &DbLeases, live: &[String]) -> bool {
        let pick = {
            let held = db.held.read().unwrap();
            let mut v: Vec<(u32, i64)> = held.iter().map(|(s, h)| (*s, h.epoch)).collect();
            v.sort();
            let now = Instant::now();
            let mut rescued = lock(&db.rescued);
            rescued.retain(|_, until| *until > now);
            v.retain(|(s, _)| !rescued.contains_key(s));
            drop(rescued);
            v.into_iter().find_map(|(s, e)| {
                let p = assign::preferred(live, &db.name, s)?;
                (p != self.node_id).then(|| (s, e, p.to_string()))
            })
        };
        let Some((s, epoch, node)) = pick else {
            return false;
        };
        self.hand_over(db, s, epoch, Some(&node)).await
    }

    /// A planned hand-over (docs/11 §4.2): this node stops taking commands
    /// for the shard, lets its writer finish what is queued (commands for
    /// the shard come back `not_owner` and follow it), then gives the lease
    /// to `to` in one step (or releases it) and tells that node at once.
    async fn hand_over(&self, db: &DbLeases, s: u32, epoch: i64, to: Option<&str>) -> bool {
        let target = match to {
            Some(node) => {
                let Some(owner) = self.owner_of(node) else {
                    return false;
                };
                let Some(peer) = lock(&self.peers).get(node).cloned() else {
                    return false;
                };
                if !peer.connected() {
                    return false;
                }
                Some((owner, peer))
            }
            None => None,
        };
        // No new commands; what the writer already admitted commits under
        // the lease (the check still accepts it), then the lease moves.
        if let Some(h) = db.held.write().unwrap().get_mut(&s) {
            h.draining = true;
        }
        if let Some(w) = self.hub().shard(db.idx) {
            let _ = self.bounded(w.call_local(Op::Barrier)).await;
        }
        db.held.write().unwrap().remove(&s);
        let ok = matches!(
            tokio::time::timeout(
                self.op_timeout(),
                sql::hand_over(
                    &db.pool,
                    db.dialect,
                    &db.name,
                    s,
                    &self.owner,
                    epoch,
                    target.as_ref().map(|(o, _)| o.as_str()),
                    self.cfg.lease_ms
                ),
            )
            .await,
            Ok(Ok(true))
        );
        db.owners
            .write()
            .unwrap()
            .insert(s, target.as_ref().filter(|_| ok).map(|(o, _)| o.clone()));
        if let Some(w) = self.hub().shard(db.idx) {
            let _ = self
                .bounded(w.call_local(Op::Forget { shards: vec![s] }))
                .await;
        }
        if ok {
            metrics::counter!("agentim_cluster_handovers_total").increment(1);
            tracing::info!(database = db.name, shard = s, to = ?to, "handed a shard over");
            if let (Some((_, peer)), Ok(f)) = (&target, codec::encode(codec::Kind::Check, b"")) {
                peer.send(f);
            }
        }
        ok
    }

    async fn forward(self: Arc<Self>, db: usize, shard: u32, op: Op) -> ApiResult<Value> {
        let Some(d) = self.dbs.get(db) else {
            return Err(ApiError::new("bad_request", "unknown database"));
        };
        let body =
            serde_json::to_value(&op).map_err(|e| ApiError::new("internal", e.to_string()))?;
        // A shard moving between nodes (a hand-over, a node leaving or a
        // takeover) is followed for a while, backing off; a request whose
        // outcome is unknown is never sent twice (docs/11 §5).
        let deadline = Instant::now() + FORWARD_FOLLOW;
        let mut pause = Duration::from_millis(25);
        let mut refresh = false;
        loop {
            if refresh {
                if Instant::now() >= deadline {
                    break;
                }
                tokio::time::sleep(pause).await;
                pause = (pause * 2).min(Duration::from_millis(400));
            }
            let mut owner = d.owners.read().unwrap().get(&shard).cloned().flatten();
            if (refresh || owner.is_none())
                && let Ok(Ok(Some((r, now)))) = tokio::time::timeout(
                    self.op_timeout(),
                    sql::lease(&d.pool, d.dialect, &d.name, shard),
                )
                .await
            {
                owner = r.owner.filter(|_| r.expires_at > now);
                d.owners.write().unwrap().insert(shard, owner.clone());
            }
            refresh = true;
            // Nobody holds it: a takeover is under way.
            let Some(owner) = owner else { continue };
            if owner == self.owner {
                // Handed to this node and not yet in use.
                self.nudge.notify_one();
                if d.held
                    .read()
                    .unwrap()
                    .get(&shard)
                    .is_some_and(|h| h.until > self.now())
                {
                    let local = self
                        .hub()
                        .shard(db)
                        .ok_or_else(|| unavailable("no writer"))?;
                    match local.call_local(op.clone()).await {
                        Err(e) if e.code == "not_owner" => continue,
                        r => return r,
                    }
                }
                continue;
            }
            let node = owner.split(':').next().unwrap_or_default();
            let peer = lock(&self.peers).get(node).cloned();
            let Some(peer) = peer.filter(|p| owner.ends_with(&format!(":{}", p.boot_id))) else {
                continue;
            };
            match peer.request(db, shard, body.clone(), FORWARD_WAIT).await {
                Err(peer::Unsent::NotSent) => continue,
                Err(peer::Unsent::Unknown) => {
                    return Err(unavailable(
                        "the node holding the conversation did not answer",
                    ));
                }
                Ok(r) => match r.err {
                    Some(e) if e.code == "not_owner" => continue,
                    Some(e) => return Err(ApiError::from_wire(&e.code, e.message)),
                    None => return Ok(r.ok.unwrap_or(Value::Null)),
                },
            }
        }
        metrics::counter!("agentim_cluster_forward_exhausted_total").increment(1);
        Err(unavailable("the conversation is moving between nodes"))
    }
}

impl Cluster for Handle {
    fn shards(&self) -> u32 {
        self.0.shards
    }

    fn held(&self, db: usize, shard: u32) -> Option<Lease> {
        if self.0.stopped.load(Ordering::Relaxed) {
            return None;
        }
        let d = self.0.dbs.get(db)?;
        let g = d.held.read().unwrap();
        let h = g.get(&shard).filter(|h| !h.draining)?;
        (self.0.now() < h.until).then(|| Lease {
            owner: self.0.owner.clone(),
            epoch: h.epoch,
        })
    }

    fn writing(&self, db: usize, shard: u32) -> Option<Lease> {
        if self.0.stopped.load(Ordering::Relaxed) {
            return None;
        }
        let d = self.0.dbs.get(db)?;
        let g = d.held.read().unwrap();
        let h = g.get(&shard)?;
        (self.0.now() < h.until).then(|| Lease {
            owner: self.0.owner.clone(),
            epoch: h.epoch,
        })
    }

    fn forward(
        &self,
        db: usize,
        shard: u32,
        op: Op,
    ) -> agentim_core::cluster::BoxFuture<ApiResult<Value>> {
        Box::pin(self.0.clone().forward(db, shard, op))
    }

    fn publish(&self, db: usize, outs: &[Out]) {
        if outs.is_empty() {
            return;
        }
        let peers: Vec<Arc<peer::Peer>> = lock(&self.0.peers).values().cloned().collect();
        if peers.is_empty() {
            return;
        }
        let body = match serde_json::to_vec(&serde_json::json!({"db": db, "outs": outs})) {
            Ok(b) => b,
            Err(e) => {
                tracing::error!(error = %e, "could not encode a publish");
                return;
            }
        };
        let Ok(frame) = codec::encode(codec::Kind::Publish, &body) else {
            tracing::warn!(
                bytes = body.len(),
                "publish over the frame limit; peers resync"
            );
            for p in &peers {
                p.stop_session();
            }
            return;
        };
        for p in peers {
            p.send(frame.clone());
        }
    }

    fn ready(&self) -> Result<(), String> {
        let i = &self.0;
        if i.leaving.load(Ordering::SeqCst) {
            return Err("leaving the cluster".into());
        }
        if i.stopped.load(Ordering::SeqCst) {
            return Err("stopped".into());
        }
        let n = i.fail_streak.load(Ordering::SeqCst);
        if n >= FAILED_ROUNDS {
            return Err(format!("{n} lease rounds in a row could not renew"));
        }
        Ok(())
    }

    fn reset(&self, db: usize) {
        let i = self.0.clone();
        let shards: Vec<u32> = match i.dbs.get(db) {
            Some(d) => d.held.write().unwrap().drain().map(|(s, _)| s).collect(),
            None => return,
        };
        tokio::spawn(async move {
            if let (Some(d), false) = (i.dbs.get(db), shards.is_empty()) {
                tracing::warn!(node = i.node_id, database = d.name, shards = ?shards, "gave up shards after a writer failure");
                if let Some(w) = i.hub().shard(db) {
                    let _ = i.bounded(w.call_local(Op::Forget { shards })).await;
                }
            }
            i.nudge.notify_one();
        });
    }

    fn lost(&self, db: usize, shard: u32) {
        let i = self.0.clone();
        if let Some(d) = i.dbs.get(db) {
            d.held.write().unwrap().remove(&shard);
        }
        tokio::spawn(async move {
            if let Some(d) = i.dbs.get(db) {
                i.forget(d, &[shard]).await;
            }
            i.nudge.notify_one();
        });
    }
}

impl peer::Serve for Handle {
    fn connected(&self, hello: &codec::Hello) {
        let again = !lock(&self.0.seen).insert(hello.node_id.clone());
        if again && let Some(h) = self.0.hub.get() {
            // Pushes from that node may have been lost while apart.
            h.resync_all();
        }
    }

    fn forward(&self, req: codec::ForwardReq) -> BoxFuture<codec::ForwardResp> {
        let i = self.0.clone();
        Box::pin(async move {
            let id = req.id;
            let err = |code: &str, message: String| codec::ForwardResp {
                id,
                ok: None,
                err: Some(codec::WireError {
                    code: code.into(),
                    message,
                }),
            };
            let op: Op = match serde_json::from_value(req.op) {
                Ok(op) => op,
                Err(e) => return err("unsupported_op", e.to_string()),
            };
            let Some(hub) = i.hub.get() else {
                return err("unavailable", "starting".into());
            };
            let Some(shard) = hub.shard(req.db) else {
                return err("bad_request", "unknown database".into());
            };
            if let agentim_core::cluster::Route::Conv(c) = op.route()
                && let Err(e) = hub.dir_ensure(&c).await
            {
                return err(e.code, e.message);
            }
            // Only commands this node may run for the shard named.
            match tokio::time::timeout(FORWARD_WAIT, shard.call_local(op)).await {
                Ok(Ok(v)) => codec::ForwardResp {
                    id,
                    ok: Some(v),
                    err: None,
                },
                Ok(Err(e)) => err(e.code, e.message),
                Err(_) => err("unavailable", "the writer did not answer in time".into()),
            }
        })
    }

    fn publish(&self, body: &[u8]) {
        // Items this version does not know (a newer node during a rolling
        // upgrade) are skipped one by one; the rest still apply.
        #[derive(serde::Deserialize)]
        struct P {
            outs: Vec<serde_json::Value>,
        }
        let Some(h) = self.0.hub.get() else { return };
        match serde_json::from_slice::<P>(body) {
            Ok(p) => {
                let mut outs = Vec::with_capacity(p.outs.len());
                let mut unknown = 0u64;
                for o in p.outs {
                    match serde_json::from_value::<Out>(o) {
                        Ok(o) => outs.push(o),
                        Err(_) => unknown += 1,
                    }
                }
                if unknown > 0 {
                    metrics::counter!("agentim_cluster_unknown_outputs_total").increment(unknown);
                    // What they would have pushed is fetched by syncing.
                    h.resync_all();
                }
                h.deliver_published(outs);
            }
            Err(e) => {
                tracing::warn!(error = %e, "undecodable publish; resyncing");
                h.resync_all();
            }
        }
    }

    fn check(&self) {
        self.0.nudge.notify_one();
    }

    fn cut_from(&self, node: &str) -> bool {
        self.0.faults.is_cut_from(node)
    }
}
