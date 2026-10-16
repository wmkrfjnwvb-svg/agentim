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
/// Answers of `not_owner` followed before giving up (docs/11 §5).
const FORWARD_RETRIES: usize = 2;

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
}

#[derive(Clone)]
struct LiveNode {
    node_id: String,
    boot_id: String,
    addr: String,
    since: Instant,
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
    tasks: Mutex<Vec<tokio::task::JoinHandle<()>>>,
    listener: Mutex<Option<TcpListener>>,
    pub_addr: String,
    slot: AtomicU32,
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
        let pool = storage::open_pool(&d.url, dialect, 4)
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
        tasks: Default::default(),
        listener: Mutex::new(Some(listener)),
        pub_addr,
        slot: AtomicU32::new(slot),
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
        let mut tasks = vec![tokio::spawn(peer::serve(
            listener,
            i.hello.clone(),
            i.tls.server.clone(),
            serve,
        ))];
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
        let now = Instant::now();
        let mut v: Vec<u32> = self.inner.dbs[db]
            .held
            .read()
            .unwrap()
            .iter()
            .filter(|(_, h)| h.until > now)
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
        if i.stopped.swap(true, Ordering::SeqCst) {
            return;
        }
        for t in lock(&i.tasks).drain(..) {
            t.abort();
        }
        for db in &i.dbs {
            let held: Vec<(u32, i64)> = db
                .held
                .write()
                .unwrap()
                .drain()
                .map(|(s, h)| (s, h.epoch))
                .collect();
            for (s, e) in held {
                let _ =
                    sql::hand_over(&db.pool, db.dialect, &db.name, s, &i.owner, e, None, 0).await;
            }
        }
        let c = &i.dbs[i.control];
        let _ = sql::heartbeat(&c.pool, c.dialect, &i.node_id, &i.boot_id, "left").await;
        // The others take the shards now rather than at their next round.
        let peers: Vec<Arc<peer::Peer>> = lock(&i.peers).drain().map(|(_, p)| p).collect();
        if let Ok(f) = codec::encode(codec::Kind::Check, b"") {
            for p in &peers {
                p.send(f.clone());
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        for p in peers {
            p.stop();
        }
    }

    /// Stops like a crash: tasks and connections end, nothing is released
    /// (others take over when the leases expire). For tests.
    pub fn crash(&self) {
        let i = &self.inner;
        i.stopped.store(true, Ordering::SeqCst);
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
            for db in &self.dbs {
                self.lease_round(db).await;
            }
        }
    }

    /// Fallback for lost invalidations (docs/11 §6.5): the cache epoch is
    /// read every 2 seconds.
    async fn refresh_loop(self: Arc<Self>) {
        let mut t = tokio::time::interval(Duration::from_secs(2));
        loop {
            t.tick().await;
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
        let t0 = Instant::now();
        let renewed = matches!(
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
            return;
        };
        if !renewed {
            metrics::counter!("agentim_cluster_renew_failures_total").increment(1);
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
                    None if mine && renewed => handed.push((r.shard, r.epoch, until)),
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
        let mut taken = handed;
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
            let t1 = Instant::now();
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
        let _ = shard
            .call_local(Op::Forget {
                shards: shards.clone(),
            })
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
            && let Err(e) = shard
                .call_local(Op::Recover {
                    shards: Some(convs),
                })
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
            let _ = shard
                .call_local(Op::Forget {
                    shards: lost.to_vec(),
                })
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
            v.into_iter().find_map(|(s, e)| {
                let p = assign::preferred(live, &db.name, s)?;
                (p != self.node_id).then(|| (s, e, p.to_string()))
            })
        };
        let Some((s, epoch, node)) = pick else {
            return false;
        };
        let Some(to) = self.owner_of(&node) else {
            return false;
        };
        let Some(peer) = lock(&self.peers).get(&node).cloned() else {
            return false;
        };
        if !peer.connected() {
            return false;
        }
        // Stop writing first; a batch already checked commits before the
        // hand-over (it holds the lease row).
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
                    Some(&to),
                    self.cfg.lease_ms
                ),
            )
            .await,
            Ok(Ok(true))
        );
        db.owners.write().unwrap().insert(s, ok.then(|| to.clone()));
        if let Some(shard) = self.hub().shard(db.idx) {
            let _ = shard.call_local(Op::Forget { shards: vec![s] }).await;
        }
        if ok {
            tracing::info!(
                database = db.name,
                shard = s,
                to = node,
                "handed a shard over"
            );
            if let Ok(f) = codec::encode(codec::Kind::Check, b"") {
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
        let mut refresh = false;
        for attempt in 0..=FORWARD_RETRIES {
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
            let Some(owner) = owner else {
                // Nobody holds it (a takeover is under way).
                tokio::time::sleep(Duration::from_millis(300 * (attempt as u64 + 1))).await;
                continue;
            };
            if owner == self.owner {
                // Taken by this node and not yet in use.
                self.nudge.notify_one();
                tokio::time::sleep(Duration::from_millis(100)).await;
                if d.held
                    .read()
                    .unwrap()
                    .get(&shard)
                    .is_some_and(|h| h.until > Instant::now())
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
                tokio::time::sleep(Duration::from_millis(200)).await;
                continue;
            };
            let Some(r) = peer.request(db, shard, body.clone(), FORWARD_WAIT).await else {
                return Err(unavailable(
                    "the node holding the conversation did not answer",
                ));
            };
            match r.err {
                Some(e) if e.code == "not_owner" => continue,
                Some(e) => return Err(ApiError::from_wire(&e.code, e.message)),
                None => return Ok(r.ok.unwrap_or(Value::Null)),
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
        let h = g.get(&shard)?;
        (Instant::now() < h.until).then(|| Lease {
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
            if let agentim_core::cluster::Route::Conv(c) = op.route() {
                hub.dir_ensure(&c).await;
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
        #[derive(serde::Deserialize)]
        struct P {
            outs: Vec<Out>,
        }
        match serde_json::from_slice::<P>(body) {
            Ok(p) => {
                if let Some(h) = self.0.hub.get() {
                    h.deliver_published(p.outs);
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "undecodable publish; resyncing");
                if let Some(h) = self.0.hub.get() {
                    h.resync_all();
                }
            }
        }
    }

    fn check(&self) {
        self.0.nudge.notify_one();
    }
}
