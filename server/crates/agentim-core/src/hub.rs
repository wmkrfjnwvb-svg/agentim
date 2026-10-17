//! The hub: opens the databases, routes every request to the writer (or read
//! pool) of the database that owns its conversation, merges cross-database
//! reads, and moves conversations between databases (docs/02-architecture.md
//! §4–§5.4).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde_json::{Value, json};

use crate::config::Config;
use crate::db::{Db, DirEntry, fingerprint, new_id};
use crate::logic::{DirInfo, HubConfig, Op, SessCtx, Shared, TokenInfo};
use crate::metrics as m;
use crate::model::*;
use crate::policy::PolicyChain;
use crate::protocol;
use crate::registry::{Out, Pushes, Registry};
use crate::shard::Shard;

/// A token bucket: tokens left and when they were counted.
type Bucket = (f64, Instant);
/// (conversation, seq, side seq) positions to acknowledge.
type Acks = Vec<(String, i64, i64)>;

mod agents;
mod export;
mod history;
mod lifecycle;
mod requests;
mod state;
pub use agents::MAX_AGENT_KEYS;
pub use requests::MAX_WAIT_SECS;
mod sync;

fn err(code: &'static str, msg: impl Into<String>) -> ApiError {
    ApiError::new(code, msg)
}

struct Inner {
    sh: Arc<Shared>,
    shards: Vec<Shard>,
    cfg: Config,
    tickers: Mutex<Vec<tokio::task::JoinHandle<()>>>,
    /// Serialises direct-conversation creation so a pair gets only one.
    dm_lock: tokio::sync::Mutex<()>,
    /// Webhook delivery tasks; they hold only their database, not the hub.
    deliverers: Vec<tokio::task::JoinHandle<()>>,
    http: reqwest::Client,
    /// Open API keys by hash.
    api_keys: std::sync::RwLock<HashMap<String, crate::db::ApiKeyRow>>,
    /// Unknown keys reload from the database at most once a second.
    keys_reloaded: Mutex<Option<Instant>>,
    /// Last lookup of a token unknown to memory (at most one per second).
    tokens_looked_up: Mutex<Option<Instant>>,
    /// MCP tool calls per agent key: (tokens, last refill).
    mcp_buckets: Mutex<HashMap<String, (f64, Instant)>>,
    /// One stateless sync at a time per device (MCP `get_updates`).
    device_locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    /// Conversations agents started: (tokens, last refill) per agent.
    conv_buckets: Mutex<HashMap<String, (f64, Instant)>>,
    /// History and search reads per agent (docs/09 §11.1).
    read_buckets: Mutex<HashMap<(String, &'static str), Bucket>>,
    /// Password hashing slots, and how many callers hold or wait for one.
    hash_slots: tokio::sync::Semaphore,
    hash_callers: std::sync::atomic::AtomicUsize,
    /// MCP `get_updates`: the batch each device was given last and not yet
    /// acknowledged (id, positions); the next call acknowledges it (D96).
    mcp_pending: Mutex<HashMap<String, (String, Acks)>>,
    /// The control database's token/deactivation counter last loaded.
    auth_epoch: std::sync::atomic::AtomicI64,
    /// Agent transfers run one at a time (owner row and member rows agree).
    transfer_lock: tokio::sync::Mutex<()>,
    /// Member changes of one conversation run one at a time, index included.
    member_locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    /// Conversations whose control index a failed write left behind, with
    /// the departure seq of members known to have left.
    index_dirty: Mutex<HashMap<String, HashMap<String, i64>>>,
}

fn hash_slot_count(cfg: &Config) -> usize {
    match cfg.auth.max_concurrent_hashes {
        0 => std::thread::available_parallelism().map_or(4, |n| n.get()),
        n => n,
    }
}

/// Held while a member change of one conversation runs; the lock entry is
/// dropped when nobody else waits for it.
struct MemberGuard {
    guard: Option<tokio::sync::OwnedMutexGuard<()>>,
    hub: Hub,
    conv: String,
}

impl Drop for MemberGuard {
    fn drop(&mut self) {
        self.guard.take();
        let mut locks = self.hub.inner.member_locks.lock().unwrap();
        if locks
            .get(&self.conv)
            .is_some_and(|m| Arc::strong_count(m) == 1)
        {
            locks.remove(&self.conv);
        }
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        for d in &self.deliverers {
            d.abort();
        }
    }
}

#[derive(Clone)]
pub struct Hub {
    inner: Arc<Inner>,
}

pub struct Session {
    hub: Hub,
    pub sid: u64,
    pub user: User,
    pub device_id: Option<String>,
    kicked: Arc<tokio::sync::Notify>,
    /// Negotiated protocol and what the client said about itself in `hello`.
    client: std::sync::Mutex<ClientState>,
    /// When this session's device was registered (cached).
    device_created: std::sync::Mutex<Option<i64>>,
}

/// Signals one user may send per second.
pub const SIGNALS_PER_SECOND: u32 = 20;

#[derive(Debug, Clone)]
pub struct ClientState {
    pub protocol: u32,
    pub info: Option<protocol::ClientInfo>,
    pub features: Vec<String>,
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Some((user_id, kind, still_online)) = self.hub.inner.sh.reg.remove(self.sid)
            && kind == UserKind::Agent
            && !still_online
        {
            for s in &self.hub.inner.shards {
                s.post(Op::AgentGone(user_id.clone()));
            }
        }
    }
}

impl Hub {
    pub async fn open(cfg: Config, hub_cfg: HubConfig) -> ApiResult<Hub> {
        let hub = Self::open_deferred(cfg, hub_cfg).await?;
        hub.finish_open().await?;
        Ok(hub)
    }

    /// The databases and writers, without the start-up reads and repairs:
    /// a cluster attaches itself (`attach_cluster`) before `finish_open`,
    /// so those writes are routed to the nodes holding their shards.
    pub async fn open_deferred(cfg: Config, hub_cfg: HubConfig) -> ApiResult<Hub> {
        cfg.validate().map_err(|e| err("config", e))?;
        hub_cfg.edition.check(&cfg).map_err(|e| err("config", e))?;
        m::init();
        sqlx::any::install_default_drivers();
        let db_names: Vec<String> = cfg
            .storage
            .databases
            .iter()
            .map(|d| d.name.clone())
            .collect();
        let control = db_names
            .iter()
            .position(|n| *n == cfg.storage.control)
            .unwrap();
        let sh = Arc::new(Shared {
            reg: Registry::new(hub_cfg.human_queue, hub_cfg.agent_queue),
            users: Default::default(),
            tokens: Default::default(),
            deactivated: Default::default(),
            stopped: Default::default(),
            destroyed: Default::default(),
            progress_buckets: Default::default(),
            progress_per_second: cfg.agents.progress_per_second as f64,
            dir: Default::default(),
            stream_index: Default::default(),
            cluster: Default::default(),
            failpoints: Default::default(),
            policy: PolicyChain::default_chain(),
            budgets: Default::default(),
            control,
            db_names: db_names.clone(),
            cfg: hub_cfg,
            blocks: Default::default(),
        });
        let mut shards = Vec::new();
        let master = cfg.master_key().map_err(|e| err("config", e))?;
        let mut wakes = Vec::new();
        let mut wcfg = cfg.writer.clone();
        if cfg.cluster.enabled {
            wcfg.cluster_stop_ms = Some(cfg.cluster.stop_ms);
        }
        for (i, d) in cfg.storage.databases.iter().enumerate() {
            let (s, w) =
                Shard::open(i, d, &wcfg, sh.clone(), &cfg.webhooks, master.as_deref()).await?;
            shards.push(s);
            wakes.push(w);
        }
        let http = crate::webhook::client();
        // One delivery task per webhook and database.
        let names: Vec<String> = cfg.webhooks.iter().map(|w| w.name.clone()).collect();
        let mut deliverers = Vec::new();
        for (s, w) in shards.iter().zip(&wakes) {
            for wh in &cfg.webhooks {
                deliverers.push(tokio::spawn(crate::webhook::deliver_loop(
                    s.clone(),
                    wh.clone(),
                    names.clone(),
                    w.subscribe(),
                    http.clone(),
                )));
            }
        }
        let hash_slots = hash_slot_count(&cfg);
        let hub = Hub {
            inner: Arc::new(Inner {
                sh,
                shards,
                cfg,
                tickers: Mutex::new(vec![]),
                deliverers,
                http,
                api_keys: Default::default(),
                keys_reloaded: Mutex::new(None),
                tokens_looked_up: Mutex::new(None),
                mcp_buckets: Mutex::default(),
                device_locks: Mutex::default(),
                conv_buckets: Mutex::default(),
                read_buckets: Mutex::default(),
                dm_lock: tokio::sync::Mutex::new(()),
                hash_slots: tokio::sync::Semaphore::new(hash_slots),
                hash_callers: Default::default(),
                member_locks: Mutex::default(),
                transfer_lock: tokio::sync::Mutex::new(()),
                auth_epoch: Default::default(),
                mcp_pending: Mutex::default(),
                index_dirty: Mutex::default(),
            }),
        };
        Ok(hub)
    }

    /// The start-up reads (users, tokens, directory) and, on a single node,
    /// the repairs after a crash. In a cluster the repairs run where the
    /// shards are taken over.
    pub async fn finish_open(&self) -> ApiResult<()> {
        self.load().await?;
        self.load_agent_flags().await?;
        Ok(())
    }

    /// Joins a cluster (before `finish_open`).
    pub fn attach_cluster(&self, c: Arc<dyn crate::cluster::Cluster>) {
        let _ = self.inner.sh.cluster.set(c);
    }

    pub fn clustered(&self) -> bool {
        self.inner.sh.cluster.get().is_some()
    }

    /// The writer of database `idx`, for a cluster's forwarded commands.
    pub fn shard(&self, idx: usize) -> Option<&Shard> {
        self.inner.shards.get(idx)
    }

    /// What another node's writer committed on database `db`: tokens and
    /// streams it changed, pushes for this node's connections, and cache
    /// invalidations (docs/11 §6.4, §6.5).
    pub fn deliver_published(&self, outs: Vec<crate::registry::Out>) {
        let sh = &self.inner.sh;
        sh.apply_tokens(&outs);
        if outs
            .iter()
            .any(|o| matches!(o, crate::registry::Out::Invalidate))
        {
            let hub = self.clone();
            tokio::spawn(async move {
                if let Err(e) = hub.refresh_auth().await {
                    tracing::warn!(error = e.message, "cache refresh failed");
                }
            });
        }
        sh.reg.deliver(outs);
    }

    /// In a cluster, a conversation another node created may not have
    /// reached this node's directory yet: it is read from the database.
    /// A lookup that fails answers `unavailable` (retryable), never
    /// `not_found`.
    pub async fn dir_ensure(&self, conv: &str) -> ApiResult<()> {
        let sh = &self.inner.sh;
        if !self.clustered() || sh.dir_get(conv).is_some() {
            return Ok(());
        }
        let c = conv.to_string();
        let found = self
            .read(self.control(), move |db| {
                Box::pin(async move { db.dir_one(&c).await })
            })
            .await
            .map_err(|e| {
                err(
                    "unavailable",
                    format!("conversation directory: {}; retry", e.message),
                )
            })?;
        let Some(e) = found else {
            return Ok(());
        };
        if let Some(i) = sh.db_names.iter().position(|n| *n == e.database) {
            sh.dir_set_local(
                conv,
                Some(DirInfo {
                    db: i,
                    unit: e.unit_id,
                    migrating: e.migrating,
                }),
            );
        }
        Ok(())
    }

    /// Session ids of this node start at `slot << 32` (unique in a cluster).
    pub fn set_session_slot(&self, slot: u32) {
        self.inner.sh.reg.set_slot(slot);
    }

    /// Every connection of this node is told to sync (after pushes from
    /// another node may have been lost).
    pub fn resync_all(&self) {
        self.inner.sh.reg.resync_all();
    }

    pub fn database_names(&self) -> &[String] {
        &self.inner.sh.db_names
    }

    /// Repairs the directory and indexes after a crash (the control shard's
    /// holder runs it in a cluster).
    pub async fn repair(&self) -> ApiResult<()> {
        if self.clustered() {
            // Other nodes changed the directory meanwhile: start from what
            // is stored.
            let sh = &self.inner.sh;
            let entries = self
                .read(self.control(), |db| {
                    Box::pin(async move { db.dir_all().await })
                })
                .await?;
            let mut fresh = HashMap::new();
            for e in entries {
                if let Some(db) = sh.db_names.iter().position(|n| *n == e.database) {
                    fresh.insert(
                        e.conv_id,
                        DirInfo {
                            db,
                            unit: e.unit_id,
                            migrating: e.migrating,
                        },
                    );
                }
            }
            *sh.dir.write().unwrap() = fresh;
        }
        self.reconcile().await
    }

    /// Rebuilds in-memory directories from the databases, repairs the
    /// conversation directory after a crash, and fails interrupted streams.
    async fn load(&self) -> ApiResult<()> {
        let sh = &self.inner.sh;
        let control = self.control();
        for (u, off) in self
            .read(control, |db| Box::pin(async move { db.load_users().await }))
            .await?
        {
            if off {
                sh.deactivated.write().unwrap().insert(u.id.clone());
            }
            sh.users.write().unwrap().insert(u.id.clone(), u);
        }
        let epoch = self
            .read(control, |db| Box::pin(async move { db.auth_epoch().await }))
            .await?;
        self.inner
            .auth_epoch
            .store(epoch, std::sync::atomic::Ordering::SeqCst);
        for t in self
            .read(control, |db| Box::pin(async move { db.tokens_all().await }))
            .await?
        {
            sh.tokens.write().unwrap().insert(
                t.hash,
                TokenInfo {
                    user_id: t.user_id,
                    device_id: t.device_id,
                    expires_at: t.expires_at,
                },
            );
        }
        for k in self
            .read(control, |db| Box::pin(async move { db.api_keys().await }))
            .await?
        {
            self.inner
                .api_keys
                .write()
                .unwrap()
                .insert(k.hash.clone(), k);
        }
        if sh.user(SYSTEM_ID).is_none() {
            let system = User {
                id: SYSTEM_ID.into(),
                name: SYSTEM_NAME.into(),
                kind: UserKind::System,
                owner_id: None,
                unit_id: UNIT_DEFAULT.into(),
                team: false,
            };
            control
                .call(Op::CreateUser {
                    user: system.clone(),
                    owner_cap: None,
                })
                .await?;
            sh.users.write().unwrap().insert(system.id.clone(), system);
        }
        let blocks = self
            .read(control, |db| Box::pin(async move { db.blocks_all().await }))
            .await?;
        sh.blocks.write().unwrap().extend(blocks);
        for u in &self.inner.cfg.units {
            control
                .call(Op::UpsertUnit {
                    name: u.name.clone(),
                    database: u.database.clone(),
                })
                .await?;
        }
        let entries = self
            .read(control, |db| Box::pin(async move { db.dir_all().await }))
            .await?;
        for e in entries {
            let Some(idx) = sh.db_names.iter().position(|n| *n == e.database) else {
                return Err(err(
                    "config",
                    format!(
                        "conversation {} lives in database {} which is not configured",
                        e.conv_id, e.database
                    ),
                ));
            };
            // A migration interrupted before the switch: the source is intact.
            // (In a cluster one may be running on another node; the control
            // shard's holder repairs.)
            if e.migrating && !self.clustered() {
                control
                    .call(Op::DirPut(DirEntry {
                        migrating: false,
                        ..e.clone()
                    }))
                    .await?;
            }
            // Local only: every node reads the same table; an entry already
            // here came from a newer change.
            sh.dir.write().unwrap().entry(e.conv_id).or_insert(DirInfo {
                db: idx,
                unit: e.unit_id,
                migrating: e.migrating && self.clustered(),
            });
        }
        if !self.clustered() {
            self.reconcile().await?;
            for s in &self.inner.shards {
                s.call(Op::Recover { shards: None }).await?;
            }
        }
        Ok(())
    }

    /// The directory (control database) and the conversation databases are
    /// written separately when they differ; repair what a crash in between
    /// may have left behind.
    async fn reconcile(&self) -> ApiResult<()> {
        let sh = &self.inner.sh;
        let mut present: HashMap<String, Vec<(usize, String)>> = HashMap::new();
        for s in &self.inner.shards {
            for (id, unit) in self
                .read(s, |db| Box::pin(async move { db.conv_ids().await }))
                .await?
            {
                present.entry(id).or_default().push((s.idx, unit));
            }
        }
        let control = self.control();
        for (conv, copies) in &present {
            let dir = sh.dir_get(conv);
            let owner = match &dir {
                Some(d) if copies.iter().any(|(i, _)| *i == d.db) => d.db,
                _ => {
                    // Missing or dangling entry: point it at an existing copy.
                    let (i, unit) = copies[0].clone();
                    let members = self.member_ids(i, conv).await?;
                    tracing::warn!(
                        conv,
                        database = sh.db_names[i],
                        "repairing conversation directory entry"
                    );
                    control
                        .call(Op::RegisterConv {
                            entry: DirEntry {
                                conv_id: conv.clone(),
                                database: sh.db_names[i].clone(),
                                unit_id: unit.clone(),
                                migrating: false,
                            },
                            member_ids: members,
                        })
                        .await?;
                    sh.dir_set(
                        conv,
                        Some(DirInfo {
                            db: i,
                            unit,
                            migrating: false,
                        }),
                    );
                    i
                }
            };
            // In a cluster a migration may be running on another node.
            let moving = self.clustered() && dir.as_ref().is_some_and(|d| d.migrating);
            for (i, _) in copies.iter().filter(|(i, _)| *i != owner && !moving) {
                tracing::warn!(
                    conv,
                    database = sh.db_names[*i],
                    "deleting stale copy left by an interrupted migration"
                );
                self.inner.shards[*i]
                    .call(Op::DeleteConv(conv.clone()))
                    .await?;
            }
            if owner != sh.control {
                self.repair_index_at(owner, conv).await?;
            }
        }
        let dangling: Vec<String> = sh
            .dir
            .read()
            .unwrap()
            .keys()
            .filter(|c| !present.contains_key(*c))
            .cloned()
            .collect();
        for c in dangling {
            tracing::warn!(
                conv = c,
                "removing directory entry for a conversation that no database holds"
            );
            control.call(Op::DirDelete(c.clone())).await?;
            sh.dir_set(&c, None);
        }
        Ok(())
    }

    /// The positions of a device's last MCP batch, taken for acknowledging:
    /// with `ack`, only if it names that batch (a stale retry acknowledges
    /// nothing, so the batch comes again).
    pub(crate) fn take_mcp_pending(
        &self,
        device: &str,
        ack: Option<&str>,
    ) -> Vec<(String, i64, i64)> {
        let mut p = self.inner.mcp_pending.lock().unwrap();
        match (p.get(device), ack) {
            (Some((id, _)), Some(a)) if id != a => vec![],
            (Some(_), _) => p.remove(device).map(|(_, v)| v).unwrap_or_default(),
            (None, _) => vec![],
        }
    }

    /// Records a device's new MCP batch; returns its id.
    pub(crate) fn put_mcp_pending(
        &self,
        device: &str,
        acks: Vec<(String, i64, i64)>,
    ) -> Option<String> {
        if acks.is_empty() {
            return None;
        }
        let id = format!("b_{}", new_id());
        let mut p = self.inner.mcp_pending.lock().unwrap();
        if p.len() > 100_000 {
            p.clear();
        }
        p.insert(device.to_string(), (id.clone(), acks));
        Some(id)
    }

    /// Ends people's device tokens past their idle or absolute lifetime
    /// (D94). Returns how many.
    pub async fn expire_tokens(&self) -> ApiResult<u64> {
        let a = &self.inner.cfg.auth;
        let now = now_ms();
        let day = 86_400_000i64;
        let before = |days: i64| if days > 0 { now - days * day } else { 0 };
        let v = self
            .control()
            .call(Op::ExpireTokens {
                idle_before: before(a.token_idle_days),
                issued_before: before(a.token_max_days),
            })
            .await?;
        Ok(v["expired"].as_u64().unwrap_or(0))
    }

    /// A token bucket per user and kind of request: `rate` a second, at
    /// most `burst` at once.
    pub(crate) fn user_bucket(
        &self,
        user: &str,
        kind: &'static str,
        rate: f64,
        burst: f64,
    ) -> ApiResult<()> {
        let now = Instant::now();
        let mut b = self.inner.read_buckets.lock().unwrap();
        let (tokens, at) = b.entry((user.to_string(), kind)).or_insert((burst, now));
        *tokens = (*tokens + now.duration_since(*at).as_secs_f64() * rate).min(burst);
        *at = now;
        if *tokens < 1.0 {
            let wait = ((1.0 - *tokens) / rate * 1000.0).ceil() as i64;
            return Err(err(
                "rate_limited",
                format!("too many {kind} requests; retry in {wait} ms"),
            ));
        }
        *tokens -= 1.0;
        Ok(())
    }

    /// A WebSocket session for an authenticated token, and why it must be
    /// closed right away, if so. Checked once registered: a logout from
    /// then on reaches the session, one before shows here (M13); too many
    /// connections of one user are refused the same way (D95).
    pub fn open_device_session(
        &self,
        user: User,
        device: String,
        token: &str,
    ) -> (Session, Pushes, Option<&'static str>) {
        let user_id = user.id.clone();
        let (s, p) = self.connect_device(user, Some(device));
        let refused = if self.authenticate_device(token).is_none() {
            Some("logged_out")
        } else if self.connections_of(&user_id) > self.inner.cfg.clients.max_connections {
            Some("too_many_connections")
        } else {
            None
        };
        (s, p, refused)
    }

    /// Open connections of a user.
    pub fn connections_of(&self, user_id: &str) -> usize {
        self.inner.sh.reg.sessions_of_user(user_id)
    }

    pub fn clients_config(&self) -> &crate::config::ClientsConfig {
        &self.inner.cfg.clients
    }

    /// Picks up token revocations and deactivations written by another
    /// process (the CLI while the server runs, M10): tokens no longer in
    /// the database stop working and their connections close.
    pub async fn refresh_auth(&self) -> ApiResult<bool> {
        use std::sync::atomic::Ordering::SeqCst;
        let control = self.control();
        let epoch = self
            .read(control, |db| Box::pin(async move { db.auth_epoch().await }))
            .await?;
        if epoch == self.inner.auth_epoch.load(SeqCst) {
            return Ok(false);
        }
        let (tokens, users, blocks, flags) = self
            .read(control, |db| {
                Box::pin(async move {
                    Ok((
                        db.tokens_all().await?,
                        db.load_users().await?,
                        db.blocks_all().await?,
                        db.agent_flags().await?,
                    ))
                })
            })
            .await?;
        let sh = &self.inner.sh;
        let live: std::collections::HashSet<String> =
            tokens.iter().map(|t| t.hash.clone()).collect();
        let mut kick = vec![];
        {
            let mut t = sh.tokens.write().unwrap();
            t.retain(|h, info| {
                let keep = live.contains(h);
                if !keep {
                    kick.push((info.user_id.clone(), Some(info.device_id.clone())));
                }
                keep
            });
        }
        {
            let mut d = sh.deactivated.write().unwrap();
            let mut all = sh.users.write().unwrap();
            for (u, off) in users {
                if off && d.insert(u.id.clone()) {
                    kick.push((u.id.clone(), None));
                } else if !off {
                    d.remove(&u.id);
                }
                all.insert(u.id.clone(), u);
            }
        }
        *sh.blocks.write().unwrap() = blocks.into_iter().collect();
        {
            let mut stopped = sh.stopped.write().unwrap();
            let mut destroyed = sh.destroyed.write().unwrap();
            stopped.clear();
            destroyed.clear();
            for (id, s, d) in flags {
                if s {
                    stopped.insert(id.clone());
                }
                if d {
                    destroyed.insert(id);
                }
            }
        }
        for s in &self.inner.shards {
            s.post(Op::ClearCaches);
        }
        sh.reg.deliver(vec![Out::Tokens {
            add: vec![],
            remove: vec![],
            kick,
        }]);
        self.inner.auth_epoch.store(epoch, SeqCst);
        Ok(true)
    }

    /// Serialises member changes of one conversation (the database write
    /// and the control index after it).
    async fn member_lock(&self, conv: &str) -> MemberGuard {
        let m = self
            .inner
            .member_locks
            .lock()
            .unwrap()
            .entry(conv.to_string())
            .or_default()
            .clone();
        MemberGuard {
            guard: Some(m.clone().lock_owned().await),
            hub: self.clone(),
            conv: conv.to_string(),
        }
    }

    /// Brings the control index of a conversation in another database in
    /// line with what that database committed. A failure does not undo the
    /// committed change: the conversation is queued and repaired from its
    /// database later (background, next start or next member change).
    async fn index_sync(&self, conv: &str, joined: Vec<String>, left: Vec<(String, i64)>) {
        let r = self
            .control()
            .call(Op::IndexSync {
                conv_id: conv.to_string(),
                joined,
                left: left.clone(),
            })
            .await;
        if let Err(e) = r {
            tracing::warn!(
                conv,
                error = e.message,
                "control index not updated; queued for repair"
            );
            self.inner
                .index_dirty
                .lock()
                .unwrap()
                .entry(conv.to_string())
                .or_default()
                .extend(left);
        }
    }

    /// Repairs the conversations a failed control write left behind.
    pub async fn repair_indexes(&self) {
        let convs: Vec<String> = self
            .inner
            .index_dirty
            .lock()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        for c in convs {
            let _g = self.member_lock(&c).await;
            let at = self.inner.sh.dir_get(&c).map(|d| d.db);
            let r = match at {
                Some(i) if i != self.inner.sh.control => self.repair_index_at(i, &c).await,
                _ => Ok(()),
            };
            if let Err(e) = r {
                tracing::warn!(conv = c, error = e.message, "control index repair failed");
            }
        }
    }

    /// Sets the control index of `conv` (held in database `idx`) to its
    /// members there; whoever else the index lists has left.
    async fn repair_index_at(&self, idx: usize, conv: &str) -> ApiResult<()> {
        let members = self.member_ids(idx, conv).await?;
        let c = conv.to_string();
        let indexed = self
            .read(self.control(), move |db| {
                Box::pin(async move { db.member_index_of(&c).await })
            })
            .await?;
        let seqs = self
            .inner
            .index_dirty
            .lock()
            .unwrap()
            .get(conv)
            .cloned()
            .unwrap_or_default();
        let left: Vec<(String, i64)> = indexed
            .into_iter()
            .filter(|u| !members.contains(u))
            .map(|u| {
                let seq = seqs.get(&u).copied().unwrap_or(0);
                (u, seq)
            })
            .collect();
        let known: Vec<String> = seqs.keys().cloned().collect();
        let mut left = left;
        // Departures queued for people the index no longer lists.
        for u in known {
            if !members.contains(&u) && !left.iter().any(|(x, _)| *x == u) {
                left.push((u.clone(), seqs[&u]));
            }
        }
        self.control()
            .call(Op::IndexSync {
                conv_id: conv.to_string(),
                joined: members,
                left,
            })
            .await?;
        self.inner.index_dirty.lock().unwrap().remove(conv);
        Ok(())
    }

    async fn member_ids(&self, idx: usize, conv: &str) -> ApiResult<Vec<String>> {
        let conv = conv.to_string();
        let members = self
            .read(&self.inner.shards[idx], move |db| {
                Box::pin(async move { db.members(&conv).await })
            })
            .await?;
        Ok(members.into_iter().map(|m| m.user_id).collect())
    }

    /// Runs read-only queries on a pooled connection of one database.
    async fn read<T, F>(&self, shard: &Shard, f: F) -> ApiResult<T>
    where
        F: for<'a> FnOnce(
            &'a mut Db<'_>,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = ApiResult<T>> + Send + 'a>,
        >,
    {
        let mut c = shard
            .read
            .acquire()
            .await
            .map_err(|e| err("storage_unavailable", e.to_string()))?;
        let mut db = Db {
            c: &mut c,
            d: shard.dialect,
            cx: &shard.crypto,
        };
        f(&mut db).await
    }

    fn control(&self) -> &Shard {
        &self.inner.shards[self.inner.sh.control]
    }

    pub fn config(&self) -> &Config {
        &self.inner.cfg
    }

    pub fn shared(&self) -> &Shared {
        &self.inner.sh
    }

    /// Periodic work per database: coalesced deltas, checkpoints, watchdog.
    pub fn spawn_background(&self) {
        let hub = self.clone();
        self.inner
            .tickers
            .lock()
            .unwrap()
            .push(tokio::spawn(async move {
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(10)).await;
                    let _ = hub.reload_api_keys().await;
                    hub.repair_indexes().await;
                    let _ = hub.expire_tokens().await;
                }
            }));
        let hub = self.clone();
        self.inner
            .tickers
            .lock()
            .unwrap()
            .push(tokio::spawn(async move {
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    let _ = hub.refresh_auth().await;
                }
            }));
        let every = (self.inner.sh.cfg.coalesce / 2).max(std::time::Duration::from_millis(10));
        let mut t = self.inner.tickers.lock().unwrap();
        for i in 0..self.inner.shards.len() {
            let hub = self.clone();
            t.push(tokio::spawn(async move {
                let mut iv = tokio::time::interval(every);
                loop {
                    iv.tick().await;
                    hub.inner.shards[i].post(Op::Tick);
                }
            }));
        }
    }

    /// Runs one round of periodic work now (tests).
    pub async fn tick(&self) {
        for s in &self.inner.shards {
            let _ = s.call(Op::Tick).await;
        }
    }

    // ------------------------------------------------- encryption at rest

    /// Encrypts content written before encryption was on, in batches, in
    /// every database. Safe to run again. Returns rows changed per database.
    pub async fn encrypt_existing(&self) -> ApiResult<Value> {
        let mut out = json!({});
        for s in &self.inner.shards {
            let mut total = 0;
            for t in [
                "messages",
                "events",
                "side_events",
                "drafts",
                "requests",
                "summaries",
                "takeovers",
                "feedback",
                "agent_state",
                "audit",
                "webhook_outbox",
            ] {
                loop {
                    let v = s
                        .call(Op::EncryptExisting {
                            table: t.into(),
                            limit: 500,
                        })
                        .await?;
                    let n = v["changed"].as_u64().unwrap_or(0);
                    total += n;
                    if n == 0 {
                        break;
                    }
                }
            }
            out[&s.name] = json!(total);
        }
        Ok(out)
    }

    /// Re-wraps all data keys with a new master key (then restart with it).
    pub async fn rotate_master_key(&self, new_key: &str) -> ApiResult<Value> {
        crate::crypto::Kek::from_base64(new_key).map_err(|e| err("bad_request", e))?;
        let mut out = json!({});
        for s in &self.inner.shards {
            let v = s
                .call(Op::RewrapKeys {
                    new_key: new_key.to_string(),
                })
                .await?;
            out[&s.name] = v["rewrapped"].clone();
        }
        Ok(out)
    }

    // ------------------------------------------------------- built-in login

    async fn credentials(&self, user_id: &str) -> ApiResult<crate::db::Credentials> {
        let u = user_id.to_string();
        Ok(self
            .read(self.control(), move |db| {
                Box::pin(async move { db.credentials(&u).await })
            })
            .await?
            .unwrap_or_default())
    }

    async fn cred(&self, user_id: &str, change: crate::logic::CredChange) -> ApiResult<Value> {
        self.control()
            .call(Op::Credentials {
                user_id: user_id.into(),
                change,
            })
            .await
    }

    /// Operator reset (the `user set-password` command): no current password
    /// needed; every device is logged out.
    pub async fn reset_password(&self, user: &User, new: &str) -> ApiResult<()> {
        self.store_password(user, new, None).await
    }

    async fn store_password(
        &self,
        user: &User,
        new: &str,
        keep_device: Option<&str>,
    ) -> ApiResult<()> {
        if user.kind != UserKind::Human {
            return Err(err("bad_request", "only people have passwords"));
        }
        crate::auth::check_password_policy(&user.name, new).map_err(|m| err("bad_request", m))?;
        let n = new.to_string();
        let hash = self
            .hash_job(move || crate::auth::hash_password(&n))
            .await?;
        self.cred(&user.id, crate::logic::CredChange::SetPassword(hash))
            .await?;
        for d in self.devices(&user.id).await? {
            if keep_device != Some(d.device_id.as_str()) {
                self.revoke_tokens(&user.id, Some(&d.device_id)).await?;
            }
        }
        Ok(())
    }

    /// Sets a password; `current` must match an existing one. Other devices
    /// are logged out (all of them when `keep_device` is None).
    pub async fn set_password(
        &self,
        user: &User,
        current: Option<&str>,
        new: &str,
        keep_device: Option<&str>,
    ) -> ApiResult<()> {
        if user.kind != UserKind::Human {
            return Err(err("bad_request", "only people have passwords"));
        }
        crate::auth::check_password_policy(&user.name, new).map_err(|m| err("bad_request", m))?;
        let c = self.credentials(&user.id).await?;
        if let Some(h) = c.password_hash {
            let cur = current.unwrap_or("").to_string();
            let ok = self
                .hash_job(move || crate::auth::verify_password(&cur, &h))
                .await?;
            if !ok {
                return Err(err("forbidden", "the current password is wrong"));
            }
        }
        self.store_password(user, new, keep_device).await
    }

    /// Password (and TOTP) login; a new device token on success. Every
    /// failure looks the same to the caller except a missing second factor.
    pub async fn login(
        &self,
        name: &str,
        password: &str,
        totp: Option<&str>,
        device_name: Option<&str>,
        platform: Option<&str>,
    ) -> ApiResult<(String, crate::db::TokenRow, User)> {
        let invalid = || err("invalid_credentials", "wrong name, password or code");
        let user = self
            .inner
            .sh
            .user_by_name(name)
            .filter(|u| u.kind == UserKind::Human);
        let c = match &user {
            Some(u) => self.credentials(&u.id).await?,
            None => Default::default(),
        };
        let now = now_ms();
        if let Some(until) = c.locked_until
            && until > now
        {
            return Err(err(
                "rate_limited",
                format!(
                    "too many failed logins; retry in {}s",
                    (until - now) / 1000 + 1
                ),
            ));
        }
        // Unknown users and users without a password take as long as a check.
        let hash = c
            .password_hash
            .clone()
            .unwrap_or_else(|| crate::auth::dummy_hash().to_string());
        let pw = password.to_string();
        let ok = self
            .hash_job(move || crate::auth::verify_password(&pw, &hash))
            .await?;
        let (Some(user), true, true) = (user, ok, c.password_hash.is_some()) else {
            if let Some(u) = self
                .inner
                .sh
                .user_by_name(name)
                .filter(|u| u.kind == UserKind::Human)
            {
                self.login_failed(&u.id).await?;
            }
            return Err(invalid());
        };
        let mut step = None;
        if let Some(secret) = &c.totp_secret {
            let Some(code) = totp else {
                return Err(err(
                    "totp_required",
                    "enter the code from your authenticator app",
                ));
            };
            match crate::auth::totp_check(secret, code, now / 1000) {
                Some(s) => step = Some(s),
                None => {
                    self.login_failed(&user.id).await?;
                    return Err(invalid());
                }
            }
        }
        if self.is_deactivated(&user.id) {
            return Err(err("forbidden", "the account is deactivated"));
        }
        self.cred(
            &user.id,
            crate::logic::CredChange::Succeeded { totp_step: step },
        )
        .await
        .map_err(|e| if e.code == "conflict" { invalid() } else { e })?;
        let (token, row) = self
            .issue_token(&user.id, device_name, platform, None, "password")
            .await?;
        Ok((token, row, user))
    }

    /// Runs a password hash or check in a bounded number of slots, so
    /// unauthenticated logins cannot take all memory and threads.
    async fn hash_job<T: Send + 'static>(
        &self,
        f: impl FnOnce() -> T + Send + 'static,
    ) -> ApiResult<T> {
        use std::sync::atomic::Ordering::SeqCst;
        struct Caller<'a>(&'a std::sync::atomic::AtomicUsize);
        impl Drop for Caller<'_> {
            fn drop(&mut self) {
                self.0.fetch_sub(1, SeqCst);
            }
        }
        let i = &self.inner;
        let cap = 4 * self.hash_slot_count();
        if i.hash_callers.fetch_add(1, SeqCst) >= cap {
            i.hash_callers.fetch_sub(1, SeqCst);
            return Err(err(
                "rate_limited",
                "too many logins at once; retry shortly",
            ));
        }
        let _caller = Caller(&i.hash_callers);
        let _slot = i
            .hash_slots
            .acquire()
            .await
            .map_err(|e| err("internal", e.to_string()))?;
        tokio::task::spawn_blocking(f)
            .await
            .map_err(|e| err("internal", e.to_string()))
    }

    fn hash_slot_count(&self) -> usize {
        hash_slot_count(&self.inner.cfg)
    }

    async fn login_failed(&self, user_id: &str) -> ApiResult<()> {
        let a = &self.inner.cfg.auth;
        self.cred(
            user_id,
            crate::logic::CredChange::Failed {
                max: a.max_failures,
                lock_ms: a.lock_minutes * 60_000,
            },
        )
        .await?;
        metrics::counter!(m::LOGIN_FAILURES).increment(1);
        Ok(())
    }

    /// Self-registration (only with `auth.open_registration`).
    pub async fn register(&self, name: &str, password: &str) -> ApiResult<User> {
        if !self.inner.cfg.auth.open_registration {
            return Err(err("forbidden", "registration is closed"));
        }
        if !crate::open_api::valid_user_id(name) {
            return Err(err(
                "bad_request",
                "names are 1..64 of A-Z a-z 0-9 _ - . @ : and do not start with _",
            ));
        }
        crate::auth::check_password_policy(name, password).map_err(|m| err("bad_request", m))?;
        let (user, _) = self.create_user(name, UserKind::Human, None, None).await?;
        // The first device of create_user is not handed out.
        self.revoke_tokens(&user.id, None).await?;
        self.reset_password(&user, password).await?;
        Ok(user)
    }

    pub async fn totp_setup(&self, user: &User) -> ApiResult<Value> {
        if self.credentials(&user.id).await?.totp_secret.is_some() {
            return Err(err(
                "conflict",
                "two-factor login is already on; turn it off first",
            ));
        }
        let secret = crate::auth::new_totp_secret();
        self.cred(
            &user.id,
            crate::logic::CredChange::TotpPending(secret.clone()),
        )
        .await?;
        Ok(
            json!({"secret": secret, "uri": crate::auth::otpauth_uri("AgentIM", &user.name, &secret)}),
        )
    }

    pub async fn totp_enable(&self, user: &User, code: &str) -> ApiResult<()> {
        let c = self.credentials(&user.id).await?;
        let Some(p) = c.totp_pending else {
            return Err(err("conflict", "run totp_setup first"));
        };
        let step = crate::auth::totp_check(&p, code, now_ms() / 1000)
            .ok_or_else(|| err("bad_request", "wrong code"))?;
        self.cred(&user.id, crate::logic::CredChange::TotpEnable { step })
            .await?;
        Ok(())
    }

    pub async fn totp_disable(&self, user: &User, code: &str) -> ApiResult<()> {
        let c = self.credentials(&user.id).await?;
        let Some(s) = c.totp_secret else {
            return Err(err("conflict", "two-factor login is off"));
        };
        let step = crate::auth::totp_check(&s, code, now_ms() / 1000)
            .ok_or_else(|| err("bad_request", "wrong code"))?;
        self.cred(
            &user.id,
            crate::logic::CredChange::Succeeded {
                totp_step: Some(step),
            },
        )
        .await
        .map_err(|_| err("bad_request", "this code was already used"))?;
        self.cred(&user.id, crate::logic::CredChange::TotpDisable)
            .await?;
        Ok(())
    }

    // ------------------------------------------------------------ open API

    /// Creates an API key; the secret is returned once (docs/07 §5).
    pub async fn create_api_key(
        &self,
        name: &str,
        scopes: &[String],
        ttl_ms: Option<i64>,
        ip_allow: Option<Vec<String>>,
    ) -> ApiResult<(String, crate::db::ApiKeyRow)> {
        if name.is_empty() || name.len() > 64 {
            return Err(err("bad_request", "name must be 1..64 bytes"));
        }
        if scopes.is_empty() {
            return Err(err("bad_request", "at least one scope"));
        }
        if let Some(s) = scopes
            .iter()
            .find(|s| !crate::open_api::SCOPES.contains(&s.as_str()))
        {
            return Err(err("bad_request", format!("unknown scope {s}")));
        }
        if let Some(ips) = &ip_allow
            && let Some(bad) = ips.iter().find(|i| crate::open_api::parse_net(i).is_none())
        {
            return Err(err("bad_request", format!("bad address {bad}")));
        }
        if ttl_ms.is_some_and(|t| t <= 0) {
            return Err(err("bad_request", "ttl must be positive"));
        }
        // The whole id: its first half is a timestamp and repeats within a millisecond.
        let key_id = format!("k{}", new_id());
        let secret = format!("ak_{key_id}_{}", &crate::auth::new_secret("x")[2..]);
        let now = now_ms();
        let mut scopes = scopes.to_vec();
        scopes.sort();
        scopes.dedup();
        let row = crate::db::ApiKeyRow {
            key_id,
            app_id: TENANT_DEFAULT.into(),
            hash: crate::auth::hash(&secret),
            name: name.into(),
            scopes,
            ip_allow,
            created_at: now,
            expires_at: ttl_ms.map(|t| now.saturating_add(t)),
            revoked_at: None,
        };
        self.control().call(Op::CreateApiKey(row.clone())).await?;
        self.inner
            .api_keys
            .write()
            .unwrap()
            .insert(row.hash.clone(), row.clone());
        Ok((secret, row))
    }

    pub async fn revoke_api_key(&self, key_id: &str) -> ApiResult<bool> {
        let v = self
            .control()
            .call(Op::RevokeApiKey {
                key_id: key_id.into(),
            })
            .await?;
        let mut keys = self.inner.api_keys.write().unwrap();
        for k in keys.values_mut() {
            if k.key_id == key_id && k.revoked_at.is_none() {
                k.revoked_at = Some(now_ms());
            }
        }
        Ok(v["revoked"].as_u64() == Some(1))
    }

    pub fn api_keys(&self) -> Vec<crate::db::ApiKeyRow> {
        let mut v: Vec<_> = self
            .inner
            .api_keys
            .read()
            .unwrap()
            .values()
            .cloned()
            .collect();
        v.sort_by(|a, b| (a.created_at, &a.key_id).cmp(&(b.created_at, &b.key_id)));
        v
    }

    /// A valid (not revoked, not expired) key. Keys created by another
    /// process (the `api-key` command) are found in the database.
    pub async fn api_key(&self, secret: &str) -> Option<crate::db::ApiKeyRow> {
        let hash = crate::auth::hash(secret);
        let cached = self.inner.api_keys.read().unwrap().get(&hash).cloned();
        let k = match cached {
            Some(k) => k,
            None => {
                {
                    let mut at = self.inner.keys_reloaded.lock().unwrap();
                    if at.is_some_and(|t| t.elapsed() < std::time::Duration::from_secs(1)) {
                        return None;
                    }
                    *at = Some(Instant::now());
                }
                self.reload_api_keys().await.ok()?;
                self.inner.api_keys.read().unwrap().get(&hash).cloned()?
            }
        };
        (k.revoked_at.is_none() && !k.expires_at.is_some_and(|e| e <= now_ms())).then_some(k)
    }

    /// Re-reads the keys (new keys and revocations from other processes).
    pub async fn reload_api_keys(&self) -> ApiResult<()> {
        let keys = self
            .read(self.control(), |db| {
                Box::pin(async move { db.api_keys().await })
            })
            .await?;
        let mut m = self.inner.api_keys.write().unwrap();
        m.clear();
        for k in keys {
            m.insert(k.hash.clone(), k);
        }
        Ok(())
    }

    pub fn open_api_config(&self) -> &crate::config::OpenApiConfig {
        &self.inner.cfg.open_api
    }

    /// Runs a client request as `user` without a connection: the same rules,
    /// deduplication, pushes and audit as a client (docs/07 §3.2).
    pub async fn act_as(&self, user: &User, msg: ClientMsg) -> ApiResult<Value> {
        if self.inner.sh.deactivated.read().unwrap().contains(&user.id) {
            return Err(err("forbidden", "the user is deactivated"));
        }
        let sess = Session {
            hub: self.clone(),
            sid: 0,
            user: user.clone(),
            device_id: None,
            kicked: Arc::new(tokio::sync::Notify::new()),
            client: std::sync::Mutex::new(ClientState {
                protocol: protocol::PROTOCOL_VERSIONS[0],
                info: None,
                features: Vec::new(),
            }),
            device_created: std::sync::Mutex::new(None),
        };
        self.dispatch(&sess, msg).await
    }

    pub fn system_user(&self) -> User {
        self.inner.sh.user(SYSTEM_ID).expect("system user")
    }

    pub fn user_by_name(&self, name: &str) -> Option<User> {
        self.inner.sh.user_by_name(name)
    }

    pub fn user(&self, id: &str) -> Option<User> {
        self.inner.sh.user(id)
    }

    pub fn is_deactivated(&self, user_id: &str) -> bool {
        self.inner.sh.deactivated.read().unwrap().contains(user_id)
    }

    pub async fn user_profile(&self, user_id: &str) -> ApiResult<Option<Value>> {
        let u = user_id.to_string();
        self.read(self.control(), move |db| {
            Box::pin(async move { db.user_profile(&u).await })
        })
        .await
    }

    pub async fn update_user(
        &self,
        user_id: &str,
        display_name: Option<String>,
        avatar: Option<String>,
        fields: Option<Value>,
    ) -> ApiResult<()> {
        self.control()
            .call(Op::UpdateUser {
                user_id: user_id.into(),
                display_name,
                avatar,
                fields,
            })
            .await
            .map(|_| ())
    }

    /// Deactivating also revokes every token, disconnects the user and
    /// stops their agents.
    pub async fn set_deactivated(&self, user_id: &str, on: bool) -> ApiResult<()> {
        self.control()
            .call(Op::SetDeactivated {
                user_id: user_id.into(),
                on,
            })
            .await?;
        if on {
            self.inner
                .sh
                .deactivated
                .write()
                .unwrap()
                .insert(user_id.into());
            self.revoke_tokens(user_id, None).await?;
            self.stop_agents_of(user_id).await?;
        } else {
            self.inner.sh.deactivated.write().unwrap().remove(user_id);
        }
        Ok(())
    }

    /// Conversation row and members, for the open API.
    pub async fn conv_detail(&self, conv_id: &str) -> ApiResult<Value> {
        let shard = self.conv_shard(conv_id)?;
        let c = conv_id.to_string();
        let (row, members) = self
            .read(shard, move |db| {
                Box::pin(async move { Ok((db.conv(&c).await?, db.members(&c).await?)) })
            })
            .await?;
        let row = row.ok_or_else(|| err("not_found", "conversation"))?;
        let sh = &self.inner.sh;
        let members: Vec<Value> = members
            .iter()
            .map(|m| {
                let name = sh.user(&m.user_id).map(|u| u.name).unwrap_or_default();
                json!({"user": name, "kind": m.kind, "role": m.role})
            })
            .collect();
        Ok(
            json!({"id": row.id, "kind": row.kind, "title": row.title, "fields": row.fields, "last_seq": row.last_seq, "members": members}),
        )
    }

    pub async fn idem_get(
        &self,
        key_id: &str,
        idem: &str,
    ) -> ApiResult<Option<(String, i64, String)>> {
        let (k, i) = (key_id.to_string(), idem.to_string());
        self.read(self.control(), move |db| {
            Box::pin(async move { db.idem_get(&k, &i).await })
        })
        .await
    }

    pub async fn idem_put(
        &self,
        key_id: &str,
        idem: &str,
        hash: &str,
        status: i64,
        response: String,
    ) -> ApiResult<()> {
        self.control()
            .call(Op::IdemPut {
                key_id: key_id.into(),
                idem_key: idem.into(),
                request_hash: hash.into(),
                status,
                response,
            })
            .await
            .map(|_| ())
    }

    /// One audit record per open API call that changes or reads content.
    pub async fn audit_api(
        &self,
        key_id: &str,
        actor: &str,
        action: &str,
        target: Option<&str>,
        detail: Value,
    ) {
        let mut d = detail;
        d["key_id"] = json!(key_id);
        let _ = self
            .control()
            .call(Op::Audit {
                actor: actor.into(),
                action: format!("api.{action}"),
                conv_id: String::new(),
                target: target.map(String::from),
                detail: d,
            })
            .await;
    }

    /// Latest audit records of the control database, newest first.
    pub async fn audit_log(&self, limit: i64) -> ApiResult<Vec<Value>> {
        self.read(self.control(), move |db| {
            Box::pin(async move { db.audit_log(limit).await })
        })
        .await
    }

    pub fn shutdown(&self) {
        for t in self.inner.tickers.lock().unwrap().drain(..) {
            t.abort();
        }
    }

    // ------------------------------------------------------------ sessions

    pub fn authenticate(&self, token: &str) -> Option<User> {
        self.authenticate_device(token).map(|(u, _)| u)
    }

    /// The user and device of a valid, unexpired client token.
    pub fn authenticate_device(&self, token: &str) -> Option<(User, String)> {
        let hash = crate::auth::hash(token);
        let t = self.inner.sh.tokens.read().unwrap().get(&hash).cloned()?;
        if t.expires_at.is_some_and(|e| e <= now_ms()) {
            return None;
        }
        if self
            .inner
            .sh
            .deactivated
            .read()
            .unwrap()
            .contains(&t.user_id)
        {
            return None;
        }
        let u = self.inner.sh.user(&t.user_id)?;
        self.control().post(Op::TouchToken { hash });
        Some((u, t.device_id))
    }

    /// Serializes stateless syncs of one device: two concurrent calls would
    /// both start at the stored position and return the same events.
    pub async fn device_lock(&self, device_id: &str) -> tokio::sync::OwnedMutexGuard<()> {
        let m = {
            let mut locks = self.inner.device_locks.lock().unwrap();
            if locks.len() > 10_000 {
                locks.retain(|_, m| Arc::strong_count(m) > 1);
            }
            locks.entry(device_id.to_string()).or_default().clone()
        };
        m.lock_owned().await
    }

    /// Takes one MCP tool call from a key's token bucket (`[mcp]`).
    pub fn mcp_allowed(&self, device_id: &str) -> bool {
        let cfg = &self.inner.cfg.mcp;
        let now = Instant::now();
        let mut b = self.inner.mcp_buckets.lock().unwrap();
        if b.len() > 100_000 {
            // Forget keys whose bucket is full again.
            b.retain(|_, (t, at)| {
                *t + at.elapsed().as_secs_f64() * cfg.rate_per_second < cfg.burst
            });
        }
        let (tokens, at) = b.entry(device_id.to_string()).or_insert((cfg.burst, now));
        *tokens =
            (*tokens + now.duration_since(*at).as_secs_f64() * cfg.rate_per_second).min(cfg.burst);
        *at = now;
        if *tokens >= 1.0 {
            *tokens -= 1.0;
            true
        } else {
            false
        }
    }

    /// A user by id or name; one unknown to memory (created by another
    /// process, e.g. `agentim agent create`) is read from the database.
    pub(crate) async fn user_or_load(&self, key: &str) -> ApiResult<Option<User>> {
        let sh = &self.inner.sh;
        if let Some(u) = sh.user(key).or_else(|| sh.user_by_name(key)) {
            return Ok(Some(u));
        }
        let k = key.to_string();
        let found = self
            .read(self.control(), move |db| {
                Box::pin(async move {
                    match db.user_by_id(&k).await? {
                        Some(u) => Ok(Some(u)),
                        None => db.user_by_name(&k).await,
                    }
                })
            })
            .await?;
        let Some((u, off)) = found else {
            return Ok(None);
        };
        if off {
            sh.deactivated.write().unwrap().insert(u.id.clone());
        }
        sh.users
            .write()
            .unwrap()
            .entry(u.id.clone())
            .or_insert_with(|| u.clone());
        Ok(Some(u))
    }

    /// Like `authenticate_device`, and a token unknown to memory is looked
    /// up in the database: issued by another process (the `agent` command)
    /// or another node. At most one lookup per second, so made-up tokens
    /// cannot load the database.
    pub async fn authenticate_or_load(&self, token: &str) -> Option<(User, String)> {
        if let Some(found) = self.authenticate_device(token) {
            return Some(found);
        }
        {
            let mut at = self.inner.tokens_looked_up.lock().unwrap();
            if at.is_some_and(|t| t.elapsed() < std::time::Duration::from_secs(1)) {
                return None;
            }
            *at = Some(Instant::now());
        }
        let v = self
            .control()
            .call(Op::LoadToken {
                hash: crate::auth::hash(token),
            })
            .await
            .ok()?;
        let user: User = serde_json::from_value(v["user"].clone()).ok()?;
        let sh = &self.inner.sh;
        if v["deactivated"] == true {
            sh.deactivated.write().unwrap().insert(user.id.clone());
        }
        sh.users
            .write()
            .unwrap()
            .entry(user.id.clone())
            .or_insert(user);
        self.authenticate_device(token)
    }

    /// Issues a client token for a new device. `ttl_ms` None = no expiry.
    pub async fn issue_token(
        &self,
        user_id: &str,
        device_name: Option<&str>,
        platform: Option<&str>,
        ttl_ms: Option<i64>,
        issued_by: &str,
    ) -> ApiResult<(String, crate::db::TokenRow)> {
        self.issue_token_with(
            user_id,
            device_name,
            platform,
            ttl_ms,
            issued_by,
            false,
            None,
        )
        .await
        .map(|(t, row, _)| (t, row))
    }

    /// Like `issue_token`; `revoke_others` also revokes every other token of
    /// the user in the same write and returns their device ids.
    #[allow(clippy::too_many_arguments)]
    pub async fn issue_token_with(
        &self,
        user_id: &str,
        device_name: Option<&str>,
        platform: Option<&str>,
        ttl_ms: Option<i64>,
        issued_by: &str,
        revoke_others: bool,
        max_devices: Option<usize>,
    ) -> ApiResult<(String, crate::db::TokenRow, Vec<String>)> {
        let user = self
            .inner
            .sh
            .user(user_id)
            .ok_or_else(|| err("not_found", "user"))?;
        if user.kind == UserKind::System {
            return Err(err("bad_request", "the system user does not log in"));
        }
        for (what, v) in [("device name", device_name), ("platform", platform)] {
            if v.is_some_and(|v| v.is_empty() || v.len() > 64 || v.contains('\0')) {
                return Err(err("bad_request", format!("{what} must be 1..64 bytes")));
            }
        }
        if ttl_ms.is_some_and(|t| t <= 0) {
            return Err(err("bad_request", "ttl must be positive"));
        }
        let token = crate::auth::new_secret("atk");
        let now = now_ms();
        let row = crate::db::TokenRow {
            hash: crate::auth::hash(&token),
            user_id: user.id.clone(),
            device_id: format!("d_{}", new_id()),
            device_name: device_name.map(String::from),
            platform: platform.map(String::from),
            issued_by: Some(issued_by.to_string()),
            created_at: now,
            expires_at: ttl_ms.map(|t| now.saturating_add(t)),
            last_seen: None,
        };
        let v = self
            .control()
            .call(Op::IssueToken {
                row: row.clone(),
                revoke_others,
                max_devices,
            })
            .await?;
        let revoked: Vec<String> = serde_json::from_value(v["revoked"].clone()).unwrap_or_default();
        for d in &revoked {
            self.drop_device(&user.id, Some(d)).await?;
        }
        // The user's other devices are told (security notice).
        self.inner.sh.reg.deliver(vec![Out::ToUser {
            user_id: user.id.clone(),
            frame: json!({"type": "device.added", "device": row.view()}),
        }]);
        Ok((token, row, revoked))
    }

    /// A revoked device forgets its positions (a new login starts like a
    /// new device).
    async fn drop_device(&self, user_id: &str, device_id: Option<&str>) -> ApiResult<()> {
        for s in &self.inner.shards {
            s.call(Op::DropDevice {
                user_id: user_id.to_string(),
                device_id: device_id.map(String::from),
            })
            .await?;
        }
        Ok(())
    }

    /// Revokes a user's tokens (all, or one device) and disconnects them.
    /// Returns how many tokens were removed.
    pub async fn revoke_tokens(&self, user_id: &str, device_id: Option<&str>) -> ApiResult<usize> {
        let v = self
            .control()
            .call(Op::RevokeTokens {
                user_id: user_id.to_string(),
                device_id: device_id.map(String::from),
            })
            .await?;
        let hashes: Vec<String> = serde_json::from_value(v["hashes"].clone()).unwrap_or_default();
        self.drop_device(user_id, device_id).await?;
        Ok(hashes.len())
    }

    pub async fn devices(&self, user_id: &str) -> ApiResult<Vec<crate::db::TokenRow>> {
        let u = user_id.to_string();
        self.read(self.control(), move |db| {
            Box::pin(async move { db.devices(&u).await })
        })
        .await
    }

    /// A session for a device (WebSocket); `kicked` fires on logout.
    pub fn connect_device(&self, user: User, device_id: Option<String>) -> (Session, Pushes) {
        let (sid, rx, kick) = self.inner.sh.reg.add_device(&user, device_id.clone());
        tracing::debug!(
            conn_id = sid,
            user_id = user.id,
            kind = user.kind.as_str(),
            "connected"
        );
        (
            Session {
                hub: self.clone(),
                sid,
                user,
                device_id,
                kicked: kick,
                client: std::sync::Mutex::new(ClientState {
                    protocol: protocol::PROTOCOL_VERSIONS[0],
                    info: None,
                    features: Vec::new(),
                }),
                device_created: std::sync::Mutex::new(None),
            },
            rx,
        )
    }

    pub fn connect(&self, user: User) -> (Session, Pushes) {
        self.connect_device(user, None)
    }

    pub async fn create_user(
        &self,
        name: &str,
        kind: UserKind,
        owner: Option<&str>,
        unit: Option<&str>,
    ) -> ApiResult<(User, String)> {
        self.create_user_with(name, kind, owner, unit, false).await
    }

    /// `create_user`, for a team agent when `team` (docs/03 §9.3.1).
    pub async fn create_user_with(
        &self,
        name: &str,
        kind: UserKind,
        owner: Option<&str>,
        unit: Option<&str>,
        team: bool,
    ) -> ApiResult<(User, String)> {
        if team && (kind != UserKind::Agent || owner.is_none()) {
            return Err(err(
                "bad_request",
                "only agents with an owner are team agents",
            ));
        }
        let unit = unit.unwrap_or(UNIT_DEFAULT).to_string();
        if unit != UNIT_DEFAULT && !self.inner.cfg.units.iter().any(|u| u.name == unit) {
            return Err(err("bad_request", format!("unknown unit {unit}")));
        }
        if name.starts_with('_') {
            return Err(err("bad_request", "names starting with _ are reserved"));
        }
        if self.inner.sh.user_by_name(name).is_some() {
            return Err(err("conflict", "name taken"));
        }
        let user = User {
            id: format!("u_{}", new_id()),
            name: name.to_string(),
            kind,
            owner_id: owner.map(String::from),
            unit_id: unit,
            team,
        };
        self.control()
            .call(Op::CreateUser {
                user: user.clone(),
                owner_cap: Some(self.inner.cfg.agents.max_per_owner),
            })
            .await?;
        self.inner
            .sh
            .users
            .write()
            .unwrap()
            .insert(user.id.clone(), user.clone());
        let (token, _) = self
            .issue_token(
                &user.id,
                Some(if kind == UserKind::Agent {
                    "agent key"
                } else {
                    "first device"
                }),
                None,
                None,
                "create",
            )
            .await?;
        Ok((user, token))
    }

    /// Demo login: a new device token for an existing human user.
    pub async fn token_of(&self, name: &str) -> Option<(User, String)> {
        let u = self.inner.sh.user_by_name(name)?;
        let (t, _) = self
            .issue_token(&u.id, Some("web"), Some("web"), None, "demo")
            .await
            .ok()?;
        Some((u, t))
    }

    pub fn users(&self) -> Vec<User> {
        let mut v: Vec<User> = self
            .inner
            .sh
            .users
            .read()
            .unwrap()
            .values()
            .filter(|u| u.kind != UserKind::System)
            .cloned()
            .collect();
        v.sort_by(|a, b| a.id.cmp(&b.id));
        v
    }

    // ------------------------------------------------------------ requests

    pub async fn handle_text(&self, sess: &Session, text: &str) -> Value {
        match serde_json::from_str::<Envelope>(text) {
            Ok(env) => self.handle(sess, env).await,
            Err(e) => {
                let raw = serde_json::from_str::<Value>(text).ok();
                let rid = raw.as_ref().and_then(|v| v["rid"].as_u64());
                // An op this server does not know is told apart from a known
                // op with bad fields, so clients can detect an old server.
                let unknown = raw
                    .as_ref()
                    .and_then(|v| v["op"].as_str())
                    .is_some_and(|op| !protocol::OPS.contains(&op));
                let (code, message) = if unknown {
                    (
                        "unknown_op",
                        "this server does not support this operation".to_string(),
                    )
                } else {
                    ("bad_request", e.to_string())
                };
                metrics::counter!(m::REQUESTS, "op" => "invalid", "result" => code).increment(1);
                json!({"type": "res", "rid": rid, "ok": false, "error": {"code": code, "message": message}})
            }
        }
    }

    /// One client request with metrics and logging (WebSocket, MCP).
    pub async fn request(
        &self,
        sess: &Session,
        rid: Option<u64>,
        msg: ClientMsg,
    ) -> ApiResult<Value> {
        let op = msg.name();
        let t = Instant::now();
        let res = match msg.conv_id() {
            Some(c) => match self.dir_ensure(c).await {
                Ok(()) => self.dispatch(sess, msg).await,
                Err(e) => Err(e),
            },
            None => self.dispatch(sess, msg).await,
        };
        let secs = t.elapsed().as_secs_f64();
        let code = match &res {
            Ok(_) => "ok",
            Err(e) => e.code,
        };
        metrics::counter!(m::REQUESTS, "op" => op, "result" => code).increment(1);
        metrics::histogram!(m::REQUEST_DURATION, "op" => op).record(secs);
        if res.is_ok() && matches!(op, "send" | "stream_start" | "approve") {
            metrics::histogram!(m::ACK_LATENCY).record(secs);
        }
        match &res {
            Err(e) if matches!(e.code, "storage" | "storage_unavailable" | "internal") => {
                tracing::warn!(
                    conn_id = sess.sid,
                    user_id = sess.user.id,
                    op,
                    rid = rid,
                    error.code = e.code,
                    error = e.message,
                    "request failed"
                )
            }
            _ => tracing::debug!(
                conn_id = sess.sid,
                user_id = sess.user.id,
                op,
                rid = rid,
                result = code,
                latency_ms = secs * 1000.0,
                "request"
            ),
        }
        res
    }

    pub async fn handle(&self, sess: &Session, env: Envelope) -> Value {
        let res = self.request(sess, env.rid, env.msg).await;
        match res {
            Ok(mut v) => {
                if !v.is_object() {
                    v = json!({ "value": v });
                }
                v["type"] = json!("res");
                v["rid"] = json!(env.rid);
                v["ok"] = json!(true);
                v
            }
            Err(e) => {
                json!({"type": "res", "rid": env.rid, "ok": false, "error": {"code": e.code, "message": e.message}})
            }
        }
    }

    fn conv_shard(&self, conv: &str) -> ApiResult<&Shard> {
        let d = self
            .inner
            .sh
            .dir_get(conv)
            .ok_or_else(|| err("not_found", "conversation"))?;
        Ok(&self.inner.shards[d.db])
    }

    async fn on_conv(&self, sess: &Session, conv: &str, msg: ClientMsg) -> ApiResult<Value> {
        // A conversation can move between routing and execution; the writer
        // refuses it (`conv_moved`) and we route again with the new directory.
        for _ in 0..3 {
            let res = self
                .conv_shard(conv)?
                .call(Op::Client {
                    sess: SessCtx {
                        sid: sess.sid,
                        user: sess.user.clone(),
                    },
                    conv: conv.to_string(),
                    msg: msg.clone(),
                })
                .await;
            match res {
                Err(e) if e.code == "conv_moved" => continue,
                other => return other,
            }
        }
        Err(err(
            "conv_migrating",
            "conversation is being moved; retry shortly",
        ))
    }

    async fn find_conv_of<F>(&self, f: F) -> ApiResult<String>
    where
        F: for<'a> Fn(
                &'a mut Db<'_>,
            ) -> std::pin::Pin<
                Box<dyn std::future::Future<Output = ApiResult<Option<String>>> + Send + 'a>,
            > + Copy,
    {
        for s in &self.inner.shards {
            if let Some(c) = self.read(s, f).await? {
                return Ok(c);
            }
        }
        Err(err("not_found", "not found"))
    }

    async fn dispatch(&self, sess: &Session, msg: ClientMsg) -> ApiResult<Value> {
        let me = &sess.user;
        // A stopped agent may still say hello and learn why nothing works.
        if !matches!(msg, ClientMsg::Hello { .. }) && self.is_stopped(&me.id) {
            return Err(err(
                "conflict",
                "your owner stopped you; wait until you are resumed",
            ));
        }
        // People's sends are limited per person, over all connections (D95).
        if me.kind == UserKind::Human && matches!(msg, ClientMsg::Send { .. }) {
            let c = &self.inner.cfg.clients;
            self.user_bucket(&me.id, "send", c.send_rate, c.send_burst)?;
        }
        match msg {
            ClientMsg::Hello {
                protocol: requested,
                client,
                features,
            } => {
                let version = protocol::negotiate(requested.as_deref())?;
                let client = client.unwrap_or_default();
                let features = features.unwrap_or_default();
                protocol::check_client(&client, &features, &self.inner.cfg.clients.min_versions)?;
                let users = self.visible_users(me).await?;
                metrics::counter!(
                    m::CLIENTS,
                    "protocol" => version.to_string(),
                    "client" => protocol::metric_client_name(&client.name)
                )
                .increment(1);
                *sess.client.lock().unwrap() = ClientState {
                    protocol: version,
                    info: Some(client),
                    features,
                };
                let mut res = protocol::server_info(self.inner.sh.cfg.edition);
                res["protocol"] = json!(version);
                res["me"] = json!(me);
                res["users"] = json!(users);
                res["my_agents"] = json!(self.inner.sh.agents_of(&me.id));
                Ok(res)
            }
            ClientMsg::FindUser { name } => {
                let found = self.inner.sh.user_by_name(&name).filter(|u| {
                    u.unit_id == me.unit_id
                        || self.inner.sh.agents_of(&me.id).iter().any(|a| a.id == u.id)
                });
                let found = match found {
                    Some(u) => Some(u),
                    None => {
                        // Someone you already share a conversation with.
                        let visible = self.visible_users(me).await?;
                        visible.into_iter().find(|u| u.name == name)
                    }
                };
                found
                    .map(|u| json!({"user": {"id": u.id, "name": u.name, "kind": u.kind, "owner_id": u.owner_id}}))
                    .ok_or_else(|| err("not_found", "user"))
            }
            ClientMsg::Block { user_id } => self.set_block(me, &user_id, true).await,
            ClientMsg::Unblock { user_id } => self.set_block(me, &user_id, false).await,
            ClientMsg::ListBlocks => {
                let mut ids: Vec<String> = self
                    .inner
                    .sh
                    .blocks
                    .read()
                    .unwrap()
                    .iter()
                    .filter(|(u, _)| *u == me.id)
                    .map(|(_, b)| b.clone())
                    .collect();
                ids.sort();
                Ok(json!({"blocked": ids}))
            }
            ClientMsg::RemoveMember { conv_id, user_id } => {
                let user = self
                    .user_or_load(&user_id)
                    .await?
                    .ok_or_else(|| err("not_found", "user"))?;
                self.remove_member(me, &conv_id, user).await
            }
            ClientMsg::Leave { conv_id } => self.remove_member(me, &conv_id, me.clone()).await,
            ClientMsg::Dissolve { conv_id } => {
                let shard = self.conv_shard(&conv_id)?;
                let v = shard
                    .call(Op::Dissolve {
                        conv_id: conv_id.clone(),
                        actor: me.clone(),
                    })
                    .await?;
                if shard.idx != self.inner.sh.control {
                    self.control().call(Op::ForgetConv(conv_id.clone())).await?;
                    for u in v["members"].as_array().into_iter().flatten() {
                        if let Some(u) = u.as_str() {
                            self.control()
                                .call(Op::Departure {
                                    user_id: u.to_string(),
                                    conv_id: conv_id.clone(),
                                    seq: 0,
                                    dissolved: true,
                                })
                                .await?;
                        }
                    }
                }
                self.inner.sh.dir_set(&conv_id, None);
                Ok(json!({"members": v["members"]}))
            }
            ClientMsg::ListConvs => self.list_convs(me).await,
            ClientMsg::CreateConv {
                kind,
                title,
                members,
                assignee,
            } => {
                self.create_conv(me, &kind, &title, &members, assignee.as_deref())
                    .await
            }
            ClientMsg::AddMember {
                conv_id,
                user_id,
                history,
            } => {
                let user = self
                    .user_or_load(&user_id)
                    .await?
                    .ok_or_else(|| err("not_found", "user"))?;
                if me.kind == UserKind::Agent {
                    let owner = me
                        .owner_id
                        .as_deref()
                        .and_then(|o| self.inner.sh.user(o))
                        .ok_or_else(|| err("forbidden", "an agent without an owner adds nobody"))?;
                    self.agent_may_contact(me, &owner, &user).await?;
                }
                let _members = self.member_lock(&conv_id).await;
                let mut attempt = 0;
                let (v, shard) = loop {
                    let shard = self.conv_shard(&conv_id)?;
                    match shard
                        .call(Op::AddMember {
                            conv_id: conv_id.clone(),
                            actor: me.clone(),
                            user: user.clone(),
                            history,
                            cap: self.inner.cfg.clients.max_group_members,
                        })
                        .await
                    {
                        Err(e) if e.code == "conv_moved" && attempt < 3 => attempt += 1,
                        res => break (res?, shard),
                    }
                };
                if shard.idx != self.inner.sh.control {
                    self.index_sync(&conv_id, vec![user.id], vec![]).await;
                }
                Ok(v)
            }
            // Clients only sync (docs/08); reading by seq stays for the
            // open API (the system identity) and owners observing an agent.
            ClientMsg::Pull {
                conv_id,
                after_seq,
                limit,
                as_agent,
            } if me.id == SYSTEM_ID || as_agent.is_some() => {
                self.pull_as(me, &conv_id, after_seq, limit.unwrap_or(200), as_agent)
                    .await
            }
            ClientMsg::Pull { .. } | ClientMsg::PullSide { .. } => Err(err(
                "bad_request",
                "reading history by seq was replaced by sync and ack",
            )),
            ClientMsg::Observe {
                conv_id,
                agent_id,
                after_seq,
                limit,
            } => {
                self.pull_as(
                    me,
                    &conv_id,
                    after_seq,
                    limit.unwrap_or(200),
                    Some(agent_id),
                )
                .await
            }
            ClientMsg::Sync { limit } => self.sync(sess, limit).await,
            ClientMsg::Ack { acks } => self.ack(sess, acks).await,
            ClientMsg::Unread => self.unread_summary(me).await,
            ClientMsg::SetPrefs {
                read_silences_others,
            } => self.set_prefs(me, read_silences_others).await,
            ClientMsg::Focus { conv_id, as_agent } => match &conv_id {
                Some(c) => {
                    self.reader_member(me, c, as_agent.as_deref()).await?;
                    self.inner
                        .sh
                        .reg
                        .set_focus(sess.sid, Some((c.clone(), as_agent.clone())));
                    let c = c.clone();
                    self.on_conv(
                        sess,
                        &c,
                        ClientMsg::Focus {
                            conv_id: Some(c.clone()),
                            as_agent,
                        },
                    )
                    .await
                }
                None => {
                    self.inner.sh.reg.set_focus(sess.sid, None);
                    Ok(json!({}))
                }
            },
            ClientMsg::Send { .. } if self.inner.cfg.before_send.is_some() => {
                let msg = self.before_send(me, msg).await?;
                let ClientMsg::Send { ref conv_id, .. } = msg else {
                    unreachable!()
                };
                let c = conv_id.clone();
                self.on_conv(sess, &c, msg).await
            }
            ClientMsg::Send { ref conv_id, .. }
            | ClientMsg::Read { ref conv_id, .. }
            | ClientMsg::SetMute { ref conv_id, .. }
            | ClientMsg::StreamStart { ref conv_id, .. }
            | ClientMsg::Whisper { ref conv_id, .. }
            | ClientMsg::Takeover { ref conv_id, .. }
            | ClientMsg::Release { ref conv_id, .. }
            | ClientMsg::SetAgentPolicy { ref conv_id, .. }
            | ClientMsg::SetRole { ref conv_id, .. }
            | ClientMsg::TransferOwner { ref conv_id, .. }
            | ClientMsg::ResetGuard { ref conv_id, .. }
            | ClientMsg::UpdateConv { ref conv_id, .. }
            | ClientMsg::RequestHuman { ref conv_id, .. }
            | ClientMsg::UpdateTask { ref conv_id, .. }
            | ClientMsg::FloorGrant { ref conv_id, .. }
            | ClientMsg::FloorRelease { ref conv_id }
            | ClientMsg::PutSummary { ref conv_id, .. }
            | ClientMsg::Handoff { ref conv_id, .. }
            | ClientMsg::Handback { ref conv_id, .. }
            | ClientMsg::Progress { ref conv_id, .. } => {
                let c = conv_id.clone();
                self.on_conv(sess, &c, msg).await
            }
            ClientMsg::Decide { ref request_id, .. }
            | ClientMsg::CancelRequest { ref request_id } => {
                let id = request_id.clone();
                let conv = self
                    .find_conv_of(|db| {
                        Box::pin({
                            let id = id.clone();
                            async move { Ok(db.request(&id).await?.map(|q| q.conv_id)) }
                        })
                    })
                    .await
                    .map_err(|_| err("not_found", "request"))?;
                self.on_conv(sess, &conv, msg).await
            }
            ClientMsg::GetRequest {
                request_id,
                wait_secs,
            } => self.get_request(me, &request_id, wait_secs).await,
            ClientMsg::Inbox => self.inbox(me).await,
            ClientMsg::History {
                conv_id,
                after_seq,
                before_seq,
                limit,
            } => {
                self.history(me, &conv_id, after_seq, before_seq, limit)
                    .await
            }
            ClientMsg::Context {
                conv_id,
                max_tokens,
            } => self.context(me, &conv_id, max_tokens).await,
            ClientMsg::Search {
                query,
                conv_id,
                limit,
            } => self.search(me, &query, conv_id.as_deref(), limit).await,
            ClientMsg::StateGet { key, version } => self.state_get(me, &key, version).await,
            ClientMsg::StatePut {
                key,
                value,
                version,
            } => self.state_put(me, &key, value, version).await,
            ClientMsg::StateList {
                prefix,
                after,
                limit,
            } => self.state_list(me, prefix, after, limit).await,
            ClientMsg::StateDelete { key, version } => self.state_delete(me, &key, version).await,
            ClientMsg::StopAgent { agent_id, reason } => {
                self.stop_agent(me, &agent_id, reason).await
            }
            ClientMsg::ResumeAgent { agent_id } => self.resume_agent(me, &agent_id).await,
            ClientMsg::ListFeedback { agent_id, limit } => {
                self.list_feedback(me, &agent_id, limit).await
            }
            ClientMsg::DestroyAgent { agent_id, confirm } => {
                self.destroy_agent(me, &agent_id, &confirm).await
            }
            ClientMsg::Edit { ref msg_id, .. }
            | ClientMsg::Recall { ref msg_id, .. }
            | ClientMsg::Claim { ref msg_id }
            | ClientMsg::Respond { ref msg_id, .. }
            | ClientMsg::Feedback { ref msg_id, .. }
            | ClientMsg::ReleaseClaim { ref msg_id } => {
                let id = msg_id.clone();
                let conv = self
                    .find_conv_of(|db| {
                        Box::pin({
                            let id = id.clone();
                            async move { db.msg_conv(&id).await }
                        })
                    })
                    .await
                    .map_err(|_| err("not_found", "message"))?;
                self.on_conv(sess, &conv, msg).await
            }
            ClientMsg::StreamAppend { ref msg_id, .. }
            | ClientMsg::StreamEnd { ref msg_id }
            | ClientMsg::StreamCancel { ref msg_id } => {
                let (idx, conv) = self
                    .inner
                    .sh
                    .stream_index
                    .lock()
                    .unwrap()
                    .get(msg_id)
                    .cloned()
                    .ok_or_else(|| err("not_found", "no active stream"))?;
                self.inner.shards[idx]
                    .call(Op::Client {
                        sess: SessCtx {
                            sid: sess.sid,
                            user: me.clone(),
                        },
                        conv,
                        msg,
                    })
                    .await
            }
            ClientMsg::Approve { ref draft_id, .. } | ClientMsg::Reject { ref draft_id } => {
                let id = draft_id.clone();
                let conv = self
                    .find_conv_of(|db| {
                        Box::pin({
                            let id = id.clone();
                            async move { Ok(db.draft(&id).await?.map(|d| d.conv_id)) }
                        })
                    })
                    .await
                    .map_err(|_| err("not_found", "draft"))?;
                self.on_conv(sess, &conv, msg).await
            }
            ClientMsg::ListDrafts => {
                let agents: Vec<String> = self
                    .inner
                    .sh
                    .agents_of(&me.id)
                    .into_iter()
                    .map(|a| a.id)
                    .collect();
                let mut list = vec![];
                for s in &self.inner.shards {
                    let a = agents.clone();
                    for d in self
                        .read(s, move |db| {
                            Box::pin(async move { db.pending_drafts_for(&a).await })
                        })
                        .await?
                    {
                        list.push((d.created_at, json!({"draft_id": d.draft_id, "conv_id": d.conv_id, "agent_id": d.agent_id, "content": d.content, "created_at": d.created_at})));
                    }
                }
                list.sort_by_key(|(t, _)| *t);
                Ok(json!({"drafts": list.into_iter().map(|(_, v)| v).collect::<Vec<_>>()}))
            }
            ClientMsg::Stats if !self.inner.sh.cfg.open_stats => Err(err(
                "forbidden",
                "stats are available to operators through the admin API",
            )),
            ClientMsg::Stats => self.stats().await,
            ClientMsg::Signal {
                conv_id,
                kind,
                data,
            } => self.signal(sess, &conv_id, &kind, data).await,
            ClientMsg::SetPassword { current, new } => {
                self.set_password(me, current.as_deref(), &new, sess.device_id.as_deref())
                    .await?;
                Ok(json!({}))
            }
            ClientMsg::TotpSetup => self.totp_setup(me).await,
            ClientMsg::TotpEnable { code } => self.totp_enable(me, &code).await.map(|_| json!({})),
            ClientMsg::TotpDisable { code } => {
                self.totp_disable(me, &code).await.map(|_| json!({}))
            }
            ClientMsg::ListDevices => {
                let list: Vec<Value> = self
                    .devices(&me.id)
                    .await?
                    .iter()
                    .map(|d| {
                        let mut v = d.view();
                        v["current"] =
                            json!(sess.device_id.as_deref() == Some(d.device_id.as_str()));
                        v
                    })
                    .collect();
                Ok(json!({"devices": list}))
            }
            ClientMsg::LogoutDevice { device_id } => {
                if !self
                    .devices(&me.id)
                    .await?
                    .iter()
                    .any(|d| d.device_id == device_id)
                {
                    return Err(err("not_found", "device"));
                }
                let n = self.revoke_tokens(&me.id, Some(&device_id)).await?;
                Ok(json!({"revoked": n}))
            }
            ClientMsg::CreateAgent { name } => self.create_agent(me, &name).await,
            ClientMsg::ListAgents => self.list_agents(me).await,
            ClientMsg::AgentKey {
                agent_id,
                revoke_others,
            } => self.agent_key(me, &agent_id, revoke_others).await,
            ClientMsg::RevokeAgentKey {
                agent_id,
                device_id,
            } => self.revoke_agent_key(me, &agent_id, &device_id).await,
        }
    }

    /// Asks the configured before-send hook; it may allow, reject or replace
    /// the content. Invalid requests are refused first and never reach it.
    async fn before_send(&self, me: &User, msg: ClientMsg) -> ApiResult<ClientMsg> {
        let Some(cfg) = &self.inner.cfg.before_send else {
            return Ok(msg);
        };
        // A retry of a committed message skips the hook (it already saw it),
        // and nobody's content goes out for a conversation they are not in (M1).
        if let ClientMsg::Send {
            conv_id,
            client_msg_id,
            ..
        } = &msg
        {
            let shard = self.conv_shard(conv_id)?;
            let (c, u, cid) = (conv_id.clone(), me.id.clone(), client_msg_id.clone());
            let (member, dup) = self
                .read(shard, move |db| {
                    Box::pin(async move {
                        Ok((
                            db.member(&c, &u).await?.is_some(),
                            db.dedup_get(&c, &u, &cid).await?.is_some(),
                        ))
                    })
                })
                .await?;
            if dup {
                return Ok(msg);
            }
            if !member {
                return Err(err("forbidden", "not a member of this conversation"));
            }
        }
        let ClientMsg::Send {
            conv_id,
            client_msg_id,
            mut text,
            kind,
            mut payload,
            reply_to,
            causation_id,
            on_behalf_of,
            to_user,
            to,
            topics,
            intent,
            refs,
            sources,
        } = msg
        else {
            return Ok(msg);
        };
        crate::logic::check_text(&text)?;
        crate::logic::check_kind(kind.clone(), &payload)?;
        let req = json!({"conv_id": conv_id, "client_msg_id": client_msg_id, "kind": kind.as_deref().unwrap_or("text"), "text": text, "payload": payload, "reply_to": reply_to, "to_user": to_user, "to": to, "topics": topics, "intent": intent, "refs": refs});
        if let crate::webhook::Verdict::Replace {
            text: t,
            payload: p,
        } = crate::webhook::before_send(&self.inner.http, cfg, me, req).await?
        {
            text = t;
            payload = p;
        }
        Ok(ClientMsg::Send {
            conv_id,
            client_msg_id,
            text,
            kind,
            payload,
            reply_to,
            causation_id,
            on_behalf_of,
            to_user,
            to,
            topics,
            intent,
            refs,
            sources,
        })
    }

    /// Ephemeral event to the members online now (docs/02-architecture.md
    /// §2.6): nothing is stored and no seq is taken, so a refused or dropped
    /// signal leaves no trace.
    async fn signal(
        &self,
        sess: &Session,
        conv_id: &str,
        kind: &str,
        data: Option<Value>,
    ) -> ApiResult<Value> {
        let me = &sess.user;
        if kind != "typing" {
            crate::logic::check_custom_kind(kind)?;
        }
        if let Some(d) = &data {
            crate::logic::check_json_object(d, "data", crate::logic::MAX_SIGNAL_BYTES)?;
        }
        // Per user, over all connections (M12).
        self.user_bucket(
            &me.id,
            "signal",
            SIGNALS_PER_SECOND as f64,
            SIGNALS_PER_SECOND as f64,
        )?;
        let shard = self.conv_shard(conv_id)?;
        let (conv, members) = {
            let c = conv_id.to_string();
            self.read(shard, move |db| {
                Box::pin(async move { Ok((db.conv(&c).await?, db.members(&c).await?)) })
            })
            .await?
        };
        let Some(conv) = conv else {
            return Err(err("not_found", "conversation"));
        };
        if !members.iter().any(|m| m.user_id == me.id) {
            return Err(err("forbidden", "not a member"));
        }
        if conv.kind == "dm"
            && members
                .iter()
                .any(|m| m.user_id != me.id && self.inner.sh.blocked_either(&m.user_id, &me.id))
        {
            return Err(err("blocked", "you cannot message this user"));
        }
        let frame = json!({"type": "signal", "conv_id": conv_id, "kind": kind, "from": me.id, "data": data});
        let outs: Vec<Out> = members
            .iter()
            .filter(|m| m.user_id != me.id)
            .map(|m| Out::ToUser {
                user_id: m.user_id.clone(),
                frame: frame.clone(),
            })
            .collect();
        self.inner.sh.reg.deliver(outs);
        Ok(json!({}))
    }

    /// The member whose read scope applies: the user, or (for an owner
    /// observing) the user's agent.
    async fn reader_member(
        &self,
        me: &User,
        conv: &str,
        as_agent: Option<&str>,
    ) -> ApiResult<Member> {
        let shard = self.conv_shard(conv)?;
        let who = match as_agent {
            Some(a) => {
                let agent = self
                    .inner
                    .sh
                    .user(a)
                    .ok_or_else(|| err("not_found", "agent"))?;
                if agent.kind != UserKind::Agent
                    || agent.owner_id.as_deref() != Some(me.id.as_str())
                {
                    return Err(err("forbidden", "you do not own this agent"));
                }
                agent.id
            }
            None => me.id.clone(),
        };
        let conv = conv.to_string();
        if who == SYSTEM_ID {
            return Ok(system_member());
        }
        // Observing through an agent needs the owner to be a member too
        // (docs/03 §9.3.1).
        let owner = (who != me.id).then(|| me.id.clone());
        let (m, owner_in) = self
            .read(shard, move |db| {
                Box::pin(async move {
                    let owner_in = match &owner {
                        Some(o) => db.member(&conv, o).await?.is_some(),
                        None => true,
                    };
                    Ok((db.member(&conv, &who).await?, owner_in))
                })
            })
            .await?;
        if !owner_in {
            return Err(err("forbidden", "not a member of this conversation"));
        }
        m.ok_or_else(|| err("forbidden", "not a member of this conversation"))
    }

    /// Server-side reads by seq for the open API, tests and owners
    /// observing an agent; people only see from where their membership
    /// allows (docs/08 §2.4). Clients sync instead.
    pub async fn read_log(
        &self,
        me: &User,
        conv: &str,
        after: i64,
        limit: i64,
    ) -> ApiResult<Value> {
        self.dir_ensure(conv).await?;
        self.pull_as(me, conv, after, limit, None).await
    }

    /// Side events of a conversation addressed to `me` (server-side read).
    pub async fn read_side(&self, me: &User, conv: &str, after: i64) -> ApiResult<Value> {
        self.dir_ensure(conv).await?;
        self.pull_side(me, conv, after).await
    }

    async fn pull_as(
        &self,
        me: &User,
        conv: &str,
        after: i64,
        limit: i64,
        as_agent: Option<String>,
    ) -> ApiResult<Value> {
        let reader = self.reader_member(me, conv, as_agent.as_deref()).await?;
        let after = after.max(reader.visible_from - 1);
        let shard = self.conv_shard(conv)?;
        if let Some(a) = &as_agent {
            shard
                .call(Op::Audit {
                    actor: me.id.clone(),
                    action: "observe".into(),
                    conv_id: conv.to_string(),
                    target: Some(a.clone()),
                    detail: json!({"after_seq": after}),
                })
                .await?;
        }
        let limit = limit.clamp(1, 1000);
        let conv_s = conv.to_string();
        let sh = self.inner.sh.clone();
        let (events, upto, has_more) = self
            .read(shard, move |db| {
                Box::pin(async move {
                    let events = db.pull(&conv_s, after, limit).await?;
                    let upto = events.last().map(|e| e.seq).unwrap_or(after);
                    let has_more = events.len() as i64 == limit;
                    let mut out = vec![];
                    for e in events {
                        if let Some(v) = history::agent_view(db, &sh, &reader, e).await? {
                            out.push(v);
                        }
                    }
                    Ok((out, upto, has_more))
                })
            })
            .await?;
        let filtered = as_agent.is_some() || me.kind == UserKind::Agent;
        // `upto_seq` lets filtered readers (agents) advance without gap checks.
        Ok(
            json!({"conv_id": conv, "events": events, "upto_seq": upto, "has_more": has_more, "filtered": filtered}),
        )
    }

    async fn pull_side(&self, me: &User, conv: &str, after: i64) -> ApiResult<Value> {
        let shard = self.conv_shard(conv)?;
        let c = conv.to_string();
        let members = self
            .read(shard, move |db| {
                Box::pin(async move { db.members(&c).await })
            })
            .await?;
        if !members.iter().any(|m| m.user_id == me.id) {
            return Err(err("forbidden", "no access"));
        }
        let c = conv.to_string();
        let me_id = me.id.clone();
        const PAGE: i64 = 1000;
        let page = self
            .read(shard, move |db| {
                Box::pin(async move { db.pull_side(&c, after, PAGE).await })
            })
            .await?;
        let has_more = page.len() as i64 == PAGE;
        let upto = page.last().map_or(after, |s| s.side_seq);
        let list: Vec<SideEvent> = page
            .into_iter()
            .filter(|s| s.audience.contains(&me_id))
            .collect();
        Ok(json!({"conv_id": conv, "side_events": list, "side_upto": upto, "has_more": has_more}))
    }

    async fn list_convs(&self, me: &User) -> ApiResult<Value> {
        let sh = &self.inner.sh;
        // Only conversations the person is in: their agents' other
        // conversations are not theirs to see (docs/03 §9.3.1).
        let ids = vec![me.id.clone()];
        let pairs = self
            .read(self.control(), move |db| {
                Box::pin(async move { db.member_index_for(&ids).await })
            })
            .await?;
        let convs: std::collections::BTreeSet<String> = pairs.into_iter().map(|(c, _)| c).collect();
        let mut out = vec![];
        for conv in convs {
            let Some(d) = sh.dir_get(&conv) else { continue };
            let shard = &self.inner.shards[d.db];
            let (c, me_id) = (conv.clone(), me.id.clone());
            let row = self
                .read(shard, move |db| {
                    Box::pin(async move {
                        let Some(row) = db.conv(&c).await? else {
                            return Ok(None);
                        };
                        let members = db.members(&c).await?;
                        let unread = db.unread(&c, &me_id).await?;
                        Ok(Some((row, members, unread)))
                    })
                })
                .await?;
            let Some((row, members, unread)) = row else {
                continue;
            };
            // The index can lag its database (repaired later): trust only
            // the conversation's own member list.
            if !members.iter().any(|m| m.user_id == me.id) {
                continue;
            }
            let members: Vec<Value> = members
                .iter()
                .map(|m| json!({"user_id": m.user_id, "name": m.name, "kind": m.kind, "owner_id": m.owner_id, "role": m.role, "trigger": m.trigger, "approval": m.approval}))
                .collect();
            out.push(json!({"conv_id": row.id, "kind": row.kind, "title": row.title, "unit": row.unit_id, "database": sh.db_names[d.db], "last_seq": row.last_seq, "unread": unread, "members": members}));
        }
        Ok(json!({"convs": out}))
    }

    /// Who a user may see in the directory: themselves, their agents and
    /// everyone they share a conversation with (docs/02-architecture.md §2.4).
    async fn visible_users(&self, me: &User) -> ApiResult<Vec<User>> {
        let id = me.id.clone();
        let mut ids: std::collections::BTreeSet<String> = self
            .read(self.control(), move |db| {
                Box::pin(async move { db.contacts(&id).await })
            })
            .await?
            .into_iter()
            .collect();
        ids.insert(me.id.clone());
        let sh = &self.inner.sh;
        for a in sh.agents_of(&me.id) {
            ids.insert(a.id);
        }
        if let Some(o) = &me.owner_id {
            ids.insert(o.clone());
        }
        Ok(ids.iter().filter_map(|i| sh.user(i)).collect())
    }

    async fn set_block(&self, me: &User, target: &str, on: bool) -> ApiResult<Value> {
        let sh = &self.inner.sh;
        let t = sh
            .user(target)
            .or_else(|| sh.user_by_name(target))
            .ok_or_else(|| err("not_found", "user"))?;
        if t.id == me.id {
            return Err(err("bad_request", "you cannot block yourself"));
        }
        self.control()
            .call(Op::SetBlock {
                user_id: me.id.clone(),
                target: t.id.clone(),
                on,
            })
            .await?;
        Ok(json!({"user_id": t.id, "blocked": on}))
    }

    async fn remove_member(&self, me: &User, conv_id: &str, user: User) -> ApiResult<Value> {
        let _members = self.member_lock(conv_id).await;
        let mut attempt = 0;
        let (v, shard) = loop {
            let shard = self.conv_shard(conv_id)?;
            match shard
                .call(Op::RemoveMember {
                    conv_id: conv_id.to_string(),
                    actor: me.clone(),
                    user: user.clone(),
                })
                .await
            {
                Err(e) if e.code == "conv_moved" && attempt < 3 => attempt += 1,
                res => break (res?, shard),
            }
        };
        if shard.idx != self.inner.sh.control {
            // The person, and their agents that left with them.
            let mut left = vec![(user.id, v["seq"].as_i64().unwrap_or(0))];
            for a in v["agents"].as_array().into_iter().flatten() {
                left.push((
                    a["user_id"].as_str().unwrap_or_default().to_string(),
                    a["seq"].as_i64().unwrap_or(0),
                ));
            }
            self.index_sync(conv_id, vec![], left).await;
        }
        Ok(v)
    }

    async fn create_conv(
        &self,
        me: &User,
        kind: &str,
        title: &str,
        members: &[String],
        assignee: Option<&str>,
    ) -> ApiResult<Value> {
        // An agent's conversations belong to its person (docs/10 §5.1).
        let agent_owner = match me.kind {
            UserKind::Human => None,
            UserKind::Agent => Some(
                me.owner_id
                    .as_deref()
                    .and_then(|o| self.inner.sh.user(o))
                    .ok_or_else(|| err("forbidden", "an agent without an owner starts nothing"))?,
            ),
            UserKind::System => {
                return Err(err(
                    "forbidden",
                    "the system creates conversations through the open API",
                ));
            }
        };
        if !matches!(kind, "dm" | "group" | "task") {
            return Err(err("bad_request", "kind must be dm, group or task"));
        }
        if assignee.is_some() && kind != "task" {
            return Err(err("bad_request", "only a task has an assignee"));
        }
        let cap = self.inner.cfg.clients.max_group_members;
        if members.len() + 1 > cap {
            return Err(err("bad_request", format!("at most {cap} members")));
        }
        if title.is_empty() || title.len() > 255 || title.contains('\0') {
            return Err(err("bad_request", "title must be 1..255 bytes"));
        }
        let sh = &self.inner.sh;
        let mut users: Vec<User> = vec![];
        for k in members {
            let u = self
                .user_or_load(k)
                .await?
                .ok_or_else(|| err("not_found", format!("user {k}")))?;
            // The creator and repeated entries are not added twice.
            if u.id != me.id && !users.iter().any(|x| x.id == u.id) {
                users.push(u);
            }
        }
        if kind == "dm" && users.len() != 1 {
            return Err(err(
                "bad_request",
                "a direct conversation has exactly one other member",
            ));
        }
        if let Some(owner) = &agent_owner {
            for u in &users {
                self.agent_may_contact(me, owner, u).await?;
            }
            // In a group the person comes along as its owner.
            if kind != "dm" && !users.iter().any(|u| u.id == owner.id) {
                users.push(owner.clone());
            }
        }
        if let Some(u) = users.iter().find(|u| sh.blocked(&u.id, &me.id)) {
            return Err(err(
                "blocked",
                format!("{} does not accept conversations from you", u.name),
            ));
        }
        if kind == "dm" && sh.blocked(&me.id, &users[0].id) {
            return Err(err("blocked", "unblock this user first"));
        }
        let id = format!("c_{}", new_id());
        // One direct conversation per pair: creating it again returns it.
        let _dm_guard = if kind == "dm" {
            let guard = self.inner.dm_lock.lock().await;
            let mut pair = [me.id.clone(), users[0].id.clone()];
            pair.sort();
            let key = pair.join("|");
            let k = key.clone();
            let existing = self
                .read(self.control(), move |db| {
                    Box::pin(async move { db.dm_get(&k).await })
                })
                .await?;
            if let Some(c) = existing {
                return Ok(json!({"conv_id": c, "existing": true}));
            }
            Some((guard, key))
        } else {
            None
        };
        // The assignee of a task is one of its members.
        let assignee_user = match assignee {
            None => None,
            Some(k) => Some(
                std::iter::once(me)
                    .chain(users.iter())
                    .find(|u| u.id == k || u.name == k)
                    .cloned()
                    .ok_or_else(|| {
                        err("bad_request", "the assignee must be a member of the task")
                    })?,
            ),
        };
        // Opening an existing direct conversation (above) costs nothing.
        if agent_owner.is_some() && !self.agent_conversation_allowed(&me.id, false) {
            return Err(err(
                "rate_limited",
                "too many new conversations (agents.conversations_per_hour)",
            ));
        }
        // New conversations live in the database of the creator's unit.
        let db_name = self.inner.cfg.unit_database(&me.unit_id).to_string();
        let idx = sh.db_names.iter().position(|n| *n == db_name).unwrap();
        // The claim runs on the control shard's writer, so two nodes
        // creating the same pair at once get one conversation.
        if let Some((_, key)) = &_dm_guard {
            let v = self
                .control()
                .call(Op::DmClaim {
                    pair_key: key.clone(),
                    conv_id: id.clone(),
                    release: false,
                })
                .await?;
            if let Some(c) = v["existing"].as_str() {
                return Ok(json!({"conv_id": c, "existing": true}));
            }
        }
        // Routable before its first events are pushed.
        sh.dir_set(
            &id,
            Some(DirInfo {
                db: idx,
                unit: me.unit_id.clone(),
                migrating: false,
            }),
        );
        let res = self.inner.shards[idx]
            .call(Op::CreateConv {
                id: id.clone(),
                kind: kind.into(),
                title: title.into(),
                unit: me.unit_id.clone(),
                creator: me.clone(),
                members: users.clone(),
                manager: agent_owner.clone().filter(|_| kind != "dm"),
                assignee: assignee_user.clone(),
            })
            .await;
        if let Err(e) = res {
            sh.dir_set(&id, None);
            if let Some((_, key)) = &_dm_guard {
                let _ = self
                    .control()
                    .call(Op::DmClaim {
                        pair_key: key.clone(),
                        conv_id: id.clone(),
                        release: true,
                    })
                    .await;
            }
            return Err(e);
        }
        if idx != sh.control {
            let mut ids: Vec<String> = vec![me.id.clone()];
            ids.extend(users.iter().map(|u| u.id.clone()));
            ids.dedup();
            self.control()
                .call(Op::RegisterConv {
                    entry: DirEntry {
                        conv_id: id.clone(),
                        database: db_name,
                        unit_id: me.unit_id.clone(),
                        migrating: false,
                    },
                    member_ids: ids,
                })
                .await?;
        }

        if agent_owner.is_some() {
            self.agent_conversation_allowed(&me.id, true);
        }
        Ok(json!({"conv_id": id}))
    }

    // ------------------------------------------------------------- operate

    /// Readiness: every database accepts writes and no queue is near full.
    pub fn health(&self) -> Value {
        let mut ready = true;
        let dbs: Vec<Value> = self
            .inner
            .shards
            .iter()
            .map(|s| {
                let failed = s.health.failed();
                let saturated = s.health.depth() * 10 >= s.queue_capacity * 9;
                if failed.is_some() || saturated {
                    ready = false;
                }
                json!({"database": s.name, "failed": failed, "queue_depth": s.health.depth(), "saturated": saturated})
            })
            .collect();
        json!({"ready": ready, "databases": dbs})
    }

    pub fn storage_failed(&self) -> Option<String> {
        self.inner
            .shards
            .iter()
            .find_map(|s| s.health.failed().map(|r| format!("{}: {r}", s.name)))
    }

    pub async fn stats(&self) -> ApiResult<Value> {
        let mut counts: BTreeMap<String, i64> = BTreeMap::new();
        let mut streams = vec![];
        let mut audit = vec![];
        let mut writers = vec![];
        for s in &self.inner.shards {
            let v = s.call(Op::Stats).await?;
            if let Some(o) = v["counts"].as_object() {
                for (k, n) in o {
                    *counts.entry(k.clone()).or_default() += n.as_i64().unwrap_or(0);
                }
            }
            streams.extend(v["active_streams"].as_array().cloned().unwrap_or_default());
            audit.extend(v["audit"].as_array().cloned().unwrap_or_default());
            writers.push(s.writer_stats());
        }
        let users = self.users();
        counts.insert(
            "users".into(),
            users.iter().filter(|u| u.kind == UserKind::Human).count() as i64,
        );
        counts.insert(
            "agents".into(),
            users.iter().filter(|u| u.kind == UserKind::Agent).count() as i64,
        );
        audit.sort_by_key(|a| std::cmp::Reverse(a["ts"].as_i64().unwrap_or(0)));
        audit.truncate(50);
        let names: HashMap<String, String> = users
            .iter()
            .map(|u| (u.id.clone(), u.name.clone()))
            .collect();
        for a in audit.iter_mut() {
            let n = a["actor_id"].as_str().and_then(|id| names.get(id)).cloned();
            a["actor"] = json!(n);
        }
        let online: Vec<String> = self
            .inner
            .sh
            .reg
            .online_users()
            .iter()
            .filter_map(|id| names.get(id).cloned())
            .collect();
        Ok(
            json!({"counts": counts, "active_streams": streams, "online": online, "sessions": self.inner.sh.reg.session_count(), "audit": audit, "writers": writers, "health": self.health()}),
        )
    }

    pub fn writer_stats(&self) -> Vec<Value> {
        self.inner.shards.iter().map(|s| s.writer_stats()).collect()
    }

    /// Fault injection for tests: the next `n` commits of `database` fail.
    pub fn inject_job_failures(&self, database: &str, n: u32) {
        if let Some(s) = self.inner.shards.iter().find(|s| s.name == database) {
            s.health.fail_next_jobs.store(n, Ordering::SeqCst);
        }
    }

    pub fn edition(&self) -> crate::edition::Edition {
        self.inner.sh.cfg.edition
    }

    pub fn stats_open(&self) -> bool {
        self.inner.sh.cfg.open_stats
    }

    pub fn inject_commit_failures(&self, database: &str, n: u32) {
        if let Some(s) = self.inner.shards.iter().find(|s| s.name == database) {
            s.health.fail_next_commits.store(n, Ordering::SeqCst);
        }
    }

    // ----------------------------------------------------------- migration

    /// Moves one conversation to another database (docs/02-architecture.md
    /// §5.4): read-only → copy → verify → switch directory → delete source.
    /// Writes to the conversation are refused with `conv_migrating` meanwhile.
    pub async fn migrate_conv(&self, conv: &str, target: &str) -> ApiResult<Value> {
        let sh = &self.inner.sh;
        let info = sh
            .dir_get(conv)
            .ok_or_else(|| err("not_found", "conversation"))?;
        let dst = sh
            .db_names
            .iter()
            .position(|n| n == target)
            .ok_or_else(|| err("bad_request", format!("unknown database {target}")))?;
        if info.db == dst {
            return Ok(json!({"conv_id": conv, "moved": false}));
        }
        let (src_s, dst_s) = (&self.inner.shards[info.db], &self.inner.shards[dst]);
        let entry = |db: usize, migrating: bool| DirEntry {
            conv_id: conv.to_string(),
            database: sh.db_names[db].clone(),
            unit_id: info.unit.clone(),
            migrating,
        };
        sh.dir_set(
            conv,
            Some(DirInfo {
                migrating: true,
                ..info.clone()
            }),
        );
        let result: ApiResult<String> = async {
            self.control()
                .call(Op::DirPut(entry(info.db, true)))
                .await?;
            src_s.call(Op::StopConvStreams(conv.to_string())).await?;
            src_s.call(Op::Barrier).await?;
            let c = conv.to_string();
            let rows = self
                .read(src_s, move |db| {
                    Box::pin(async move { db.export_conv(&c).await })
                })
                .await?;
            let want = fingerprint(&rows);
            dst_s
                .call(Op::ImportConv {
                    conv_id: conv.to_string(),
                    rows,
                })
                .await?;
            let c = conv.to_string();
            let got = fingerprint(
                &self
                    .read(dst_s, move |db| {
                        Box::pin(async move { db.export_conv(&c).await })
                    })
                    .await?,
            );
            if got != want {
                return Err(err("migration_failed", "copy does not match the source"));
            }
            self.control().call(Op::DirPut(entry(dst, false))).await?;
            Ok(want)
        }
        .await;
        match result {
            Ok(fp) => {
                sh.dir_set(
                    conv,
                    Some(DirInfo {
                        db: dst,
                        unit: info.unit.clone(),
                        migrating: false,
                    }),
                );
                if let Err(e) = src_s.call(Op::DeleteConv(conv.to_string())).await {
                    // Reconcile removes the stale copy at the next start.
                    tracing::warn!(conv, error = e.message, "source copy not deleted");
                }
                if dst != sh.control {
                    for u in self.member_ids(dst, conv).await? {
                        self.control()
                            .call(Op::MemberIndexAdd {
                                conv_id: conv.to_string(),
                                user_id: u,
                            })
                            .await?;
                    }
                }
                metrics::counter!(m::MIGRATIONS, "result" => "ok").increment(1);
                tracing::info!(
                    conv,
                    from = sh.db_names[info.db],
                    to = target,
                    "conversation migrated"
                );
                Ok(
                    json!({"conv_id": conv, "moved": true, "from": sh.db_names[info.db], "to": target, "fingerprint": fp}),
                )
            }
            Err(e) => {
                metrics::counter!(m::MIGRATIONS, "result" => "failed").increment(1);
                let _ = dst_s.call(Op::DeleteConv(conv.to_string())).await;
                let _ = self.control().call(Op::DirPut(entry(info.db, false))).await;
                sh.dir_set(
                    conv,
                    Some(DirInfo {
                        migrating: false,
                        ..info
                    }),
                );
                Err(e)
            }
        }
    }

    /// Moves every conversation of `unit` into the database the
    /// configuration now assigns to it.
    pub async fn migrate_unit(&self, unit: &str) -> ApiResult<Value> {
        let target = self.inner.cfg.unit_database(unit).to_string();
        let sh = &self.inner.sh;
        let tidx = sh.db_names.iter().position(|n| *n == target).unwrap();
        let convs: Vec<String> = sh
            .dir
            .read()
            .unwrap()
            .iter()
            .filter(|(_, d)| d.unit == unit && d.db != tidx)
            .map(|(c, _)| c.clone())
            .collect();
        let mut moved = vec![];
        for c in &convs {
            self.migrate_conv(c, &target).await?;
            moved.push(c.clone());
        }
        let _: HashSet<&String> = moved.iter().collect();
        Ok(json!({"unit": unit, "database": target, "moved": moved.len()}))
    }
}

impl Session {
    pub async fn handle_text(&self, text: &str) -> Value {
        self.hub.handle_text(self, text).await
    }

    /// Completes when this session's device is logged out.
    pub async fn kicked(&self) {
        self.kicked.notified().await
    }

    pub fn client(&self) -> ClientState {
        self.client.lock().unwrap().clone()
    }

    /// Protocol chosen when connecting (`/ws?v=`), before `hello`.
    pub fn set_protocol(&self, v: u32) {
        self.client.lock().unwrap().protocol = v;
    }
}
