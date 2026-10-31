//! Business logic executed by a database's writer, inside the batch
//! transaction (one savepoint per command). Everything a command writes
//! commits together with everything else in its batch, after which its output
//! is delivered. A command that fails is rolled back alone, unless it set
//! `commit_on_error` (a rejection that must still be recorded, e.g. a breaker
//! trip or a policy block).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::db::{Cell, Committed, Db, DirEntry, NewMsg};
use crate::guard::{GuardConfig, Guards, Verdict};
use crate::metrics as m;
use crate::model::*;
use crate::policy::PolicyChain;
use crate::registry::{Out, Recip, Registry};

mod ai;
mod cards;
mod compliance;
pub mod files;
mod history;
mod lifecycle;
mod requests;
mod routing;
mod state;
mod tasks;
pub use requests::MAX_PENDING as MAX_PENDING_REQUESTS;
pub use routing::{MAX_AGENTS_FOR_ALL, MAX_WAKE};

pub const MAX_MESSAGE_BYTES: usize = 64 * 1024;
/// Serialized payload of a custom message kind.
pub const MAX_PAYLOAD_BYTES: usize = 32 * 1024;
/// Serialized custom fields of a user or conversation.
pub const MAX_FIELDS_BYTES: usize = 4 * 1024;

pub fn check_title(t: &str) -> ApiResult<()> {
    if t.is_empty() || t.len() > 255 || t.contains('\0') {
        return Err(rule!(
            "conv.title",
            "bad_request",
            "title must be 1..255 bytes"
        ));
    }
    Ok(())
}

/// Serialized data of a signal.
pub const MAX_SIGNAL_BYTES: usize = 4 * 1024;
const MODERATED_PLACEHOLDER: &str = "[该内容已被策略移除 / removed by policy]";

#[derive(Debug, Clone)]
pub struct HubConfig {
    pub stream_idle: Duration,
    pub stream_max: Duration,
    pub stream_max_bytes: usize,
    pub checkpoint_every: Duration,
    pub coalesce: Duration,
    pub guards: GuardConfig,
    pub human_queue: usize,
    pub agent_queue: usize,
    /// How long retries are deduplicated (`client_msg_id`), and how often
    /// expired records are removed.
    pub dedup_ttl: Duration,
    pub gc_every: Duration,
    /// How often requests past their time are expired.
    pub expire_every: Duration,
    /// Takeovers silent this long are released (docs/03-agents.md §4.1);
    /// zero = never.
    pub takeover_idle: Duration,
    /// Sync-only clients and retention (docs/08-sync-and-client.md).
    pub sync: crate::config::SyncConfig,
}

impl Default for HubConfig {
    fn default() -> Self {
        HubConfig {
            stream_idle: Duration::from_secs(30),
            stream_max: Duration::from_secs(600),
            stream_max_bytes: 256 * 1024,
            checkpoint_every: Duration::from_secs(1),
            coalesce: Duration::from_millis(60),
            guards: GuardConfig::default(),
            human_queue: 1024,
            agent_queue: 4096,
            dedup_ttl: Duration::from_secs(7 * 24 * 3600),
            gc_every: Duration::from_secs(600),
            expire_every: Duration::from_secs(10),
            takeover_idle: Duration::from_secs(30 * 60),
            sync: Default::default(),
        }
    }
}

// ------------------------------------------------------------ shared state

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirInfo {
    pub db: usize,
    pub unit: String,
    pub migrating: bool,
}

/// State shared by the hub and every writer. All of it is either derived
/// from the databases (and rebuilt at startup) or soft state.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub enum CredChange {
    SetPassword(String),
    Failed { max: i64, lock_ms: i64 },
    Succeeded { totp_step: Option<i64> },
    TotpPending(String),
    TotpEnable { step: i64 },
    TotpDisable,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenInfo {
    pub user_id: String,
    pub device_id: String,
    pub expires_at: Option<i64>,
}

impl TokenInfo {
    pub fn of(t: &crate::db::TokenRow) -> TokenInfo {
        TokenInfo {
            user_id: t.user_id.clone(),
            device_id: t.device_id.clone(),
            expires_at: t.expires_at,
        }
    }
}

/// Writes of the console's accounts and settings (docs/12 §4).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AdminOp {
    Create(crate::db::admin::AdminRow),
    Update {
        id: String,
        role: Option<String>,
        disabled: Option<bool>,
    },
    SessionPut(crate::db::admin::AdminSessionRow),
    SessionTouch {
        hash: String,
    },
    SessionsDelete {
        admin_id: Option<String>,
        hash: Option<String>,
    },
    SessionsExpire {
        idle_before: i64,
        max_before: i64,
    },
    KeyPut(crate::db::admin::AdminKeyRow),
    KeyRevoke {
        key_id: String,
    },
    /// An app setting; `clients.allow` also changes every node's defaults.
    Setting {
        tenant: String,
        key: String,
        value: Value,
    },
    /// Adds a dye rule (docs/14 §4.2): in the writer, so two admins adding
    /// at once cannot lose one, and the limit holds.
    DyeAdd(crate::trace::DyeRule),
    /// Removes a dye rule.
    DyeRemove {
        id: String,
    },
}

pub struct Shared {
    pub reg: Registry,
    pub users: std::sync::RwLock<HashMap<String, User>>,
    /// SHA-256 of each valid client token → its device.
    pub tokens: std::sync::RwLock<HashMap<String, TokenInfo>>,
    /// Signing keys in force: kid → (secret, created_at) (docs/13 §4.2).
    pub signing: std::sync::RwLock<HashMap<String, (String, i64)>>,
    /// (user, device or "") → client tokens issued before these seconds
    /// are refused.
    pub cutoffs: std::sync::RwLock<HashMap<(String, String), i64>>,
    /// What people may do in their client: the app's defaults and each
    /// person's overrides (docs/13 §4.1).
    pub allow: std::sync::RwLock<crate::config::AllowConfig>,
    /// Dye rules in force (docs/14 §4.2), the same on every node.
    pub dyes: std::sync::RwLock<Vec<crate::trace::DyeRule>>,
    pub perms: std::sync::RwLock<HashMap<String, Overrides>>,
    /// Users that may not log in or act (open API deactivation).
    pub deactivated: std::sync::RwLock<HashSet<String>>,
    /// Agents their owners stopped (docs/03-agents.md §4.3–§4.4).
    pub stopped: std::sync::RwLock<HashSet<String>>,
    /// Agents their owners destroyed (tombstones).
    pub destroyed: std::sync::RwLock<HashSet<String>>,
    /// Progress reports allowed per agent per conversation per second.
    pub progress_per_second: f64,
    /// Progress reports per (agent, conversation): tokens and when.
    pub progress_buckets: std::sync::Mutex<HashMap<(String, String), (f64, std::time::Instant)>>,
    pub dir: std::sync::RwLock<HashMap<String, DirInfo>>,
    /// Active stream (or streamed draft) id → database index.
    /// Open streams: message id -> (database, conversation). Every node
    /// learns them (`Out::Stream`), so stream requests reach the writer
    /// holding the stream wherever the agent is connected.
    pub stream_index: std::sync::Mutex<HashMap<String, (usize, String)>>,
    /// The cluster this node belongs to; unset on a single node.
    pub cluster: std::sync::OnceLock<Arc<dyn crate::cluster::Cluster>>,
    /// Fault points for tests (docs/11 §12.1).
    pub failpoints: crate::failpoint::Failpoints,
    pub policy: PolicyChain,
    pub cfg: HubConfig,
    pub budgets: crate::guard::Budgets,
    pub control: usize,
    pub db_names: Vec<String>,
    /// (user, blocked user) pairs, loaded from the control database.
    pub blocks: std::sync::RwLock<std::collections::HashSet<(String, String)>>,
    /// Where files are stored (docs/02-architecture.md §2.7).
    pub files: Arc<crate::filestore::Store>,
    pub files_cfg: crate::config::FilesConfig,
}

impl Shared {
    /// Token cache and stream index changes of committed writes, in write
    /// order (also those other nodes committed).
    pub fn apply_tokens(&self, outs: &[Out]) {
        for o in outs {
            match o {
                Out::Tokens { add, remove, .. } => {
                    let mut t = self.tokens.write().unwrap();
                    for h in remove {
                        t.remove(h);
                    }
                    for (h, info) in add {
                        t.insert(h.clone(), info.clone());
                    }
                }
                Out::Dir { conv_id, info } => self.dir_set_local(conv_id, info.clone()),
                Out::Cutoff {
                    user_id,
                    device_id,
                    at,
                } => {
                    self.cutoffs
                        .write()
                        .unwrap_or_else(|e| e.into_inner())
                        .insert((user_id.clone(), device_id.clone()), *at);
                }
                Out::AllowDefaults { allow } => {
                    *self.allow.write().unwrap_or_else(|e| e.into_inner()) = allow.clone();
                }
                Out::DyeRules { rules } => {
                    *self.dyes.write().unwrap_or_else(|e| e.into_inner()) = rules.clone();
                }
                Out::Perms { user_id, overrides } => {
                    let mut p = self.perms.write().unwrap_or_else(|e| e.into_inner());
                    if overrides.is_empty() {
                        p.remove(user_id);
                    } else {
                        p.insert(user_id.clone(), overrides.clone());
                    }
                }
                Out::KeyRevoked { kid } => {
                    self.signing
                        .write()
                        .unwrap_or_else(|e| e.into_inner())
                        .remove(kid);
                }
                Out::Block {
                    user_id,
                    target,
                    on,
                } => {
                    let mut b = self.blocks.write().unwrap_or_else(|e| e.into_inner());
                    if *on {
                        b.insert((user_id.clone(), target.clone()));
                    } else {
                        b.remove(&(user_id.clone(), target.clone()));
                    }
                }
                Out::Stream { msg_id, db, conv } => {
                    let mut s = self.stream_index.lock().unwrap_or_else(|e| e.into_inner());
                    match conv {
                        Some(c) => s.insert(msg_id.clone(), (*db, c.clone())),
                        None => s.remove(msg_id),
                    };
                }
                _ => {}
            }
        }
    }
}

impl Shared {
    /// What a person may do in their client (an agent: what its owner may).
    pub fn allowed(&self, u: &User) -> crate::config::AllowConfig {
        let id = match u.kind {
            UserKind::Agent => u.owner_id.as_deref().unwrap_or(&u.id),
            _ => &u.id,
        };
        let d = self.allow.read().unwrap_or_else(|e| e.into_inner()).clone();
        self.perms
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(id)
            .map(|o| o.over(&d))
            .unwrap_or(d)
    }

    /// Whether `a` blocked `b`.
    pub fn blocked(&self, a: &str, b: &str) -> bool {
        self.blocks
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .contains(&(a.to_string(), b.to_string()))
    }

    /// Whether either of the two blocked the other.
    pub fn blocked_either(&self, a: &str, b: &str) -> bool {
        self.blocked(a, b) || self.blocked(b, a)
    }

    pub fn user(&self, id: &str) -> Option<User> {
        self.users
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(id)
            .cloned()
    }

    pub fn user_by_name(&self, name: &str) -> Option<User> {
        self.users
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .find(|u| u.name == name)
            .cloned()
    }

    pub fn agents_of(&self, owner: &str) -> Vec<User> {
        let mut v: Vec<User> = self
            .users
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .filter(|u| u.owner_id.as_deref() == Some(owner))
            .cloned()
            .collect();
        v.sort_by(|a, b| a.id.cmp(&b.id));
        v
    }

    pub fn dir_get(&self, conv: &str) -> Option<DirInfo> {
        self.dir
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(conv)
            .cloned()
    }

    /// Sets a directory entry, and in a cluster on every node.
    pub fn dir_set(&self, conv: &str, info: Option<DirInfo>) {
        if let Some(c) = self.cluster.get() {
            c.publish(
                self.control,
                &[Out::Dir {
                    conv_id: conv.to_string(),
                    info: info.clone(),
                }],
            );
        }
        self.dir_set_local(conv, info);
    }

    pub fn dir_set_local(&self, conv: &str, info: Option<DirInfo>) {
        let mut d = self.dir.write().unwrap_or_else(|e| e.into_inner());
        match info {
            Some(i) => {
                d.insert(conv.to_string(), i);
            }
            None => {
                d.remove(conv);
            }
        }
    }
}

#[derive(Clone)]
enum Target {
    Main,
    Draft { audience: Vec<String> },
}

struct Stream {
    msg_id: String,
    conv_id: String,
    sender: User,
    target: Target,
    buf: String,
    pending: String,
    next_idx: u64,
    started: Instant,
    last_append: Instant,
    checkpointed: usize,
    hop: i64,
    causation_id: Option<String>,
    reply_to: Option<String>,
}

/// A conversation's member list and the fan-out recipients built from it.
type RecipCache = (Arc<Vec<Member>>, Arc<Vec<Recip>>);

/// Soft state owned by one writer.
pub struct ShardState {
    streams: HashMap<String, Stream>,
    guards: Guards,
    members: HashMap<String, Arc<Vec<Member>>>,
    /// Fan-out recipients derived from `members` (same Arc), so a large
    /// group's list is built once rather than once per event.
    recips: HashMap<String, RecipCache>,
    last_flush: Instant,
    last_checkpoint: Instant,
    last_gc: Instant,
    gc_every: Duration,
    last_expiry: Instant,
    expire_every: Duration,
}

impl ShardState {
    pub fn new(sh: &Shared) -> ShardState {
        let now = Instant::now();
        ShardState {
            streams: HashMap::new(),
            guards: Guards::with_budgets(sh.cfg.guards.clone(), sh.budgets.clone()),
            members: HashMap::new(),
            recips: HashMap::new(),
            last_flush: now,
            last_checkpoint: now,
            last_gc: now,
            gc_every: sh.cfg.gc_every,
            last_expiry: now,
            expire_every: sh.cfg.expire_every,
        }
    }

    /// Whether a tick has database work (otherwise the writer skips the
    /// transaction entirely).
    pub fn tick_needs_db(&self) -> bool {
        !self.streams.is_empty()
            || self.last_gc.elapsed() > self.gc_every
            || self.last_expiry.elapsed() > self.expire_every
    }

    pub fn active_streams(&self) -> Vec<Value> {
        self.streams
            .values()
            .map(|s| json!({"msg_id": s.msg_id, "conv_id": s.conv_id, "agent": s.sender.name, "bytes": s.buf.len(), "age_ms": s.started.elapsed().as_millis() as u64, "draft": matches!(s.target, Target::Draft{..})}))
            .collect()
    }
}

// ------------------------------------------------------------------- ops

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SessCtx {
    pub sid: u64,
    pub user: User,
    /// The request's trace (docs/14 §4.1), so the writer, another node and
    /// the message row see the same one. Absent from older nodes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace: Option<crate::trace::Trace>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Op {
    Client {
        sess: SessCtx,
        /// The conversation the request is about (its shard decides where
        /// it runs in a cluster).
        conv: String,
        msg: ClientMsg,
    },
    Tick,
    /// Cluster: this node lost these shards; their in-memory state (open
    /// streams, member caches) goes.
    Forget {
        shards: Vec<u32>,
    },
    /// Member caches go (memberships may have changed elsewhere).
    ClearCaches,
    /// Ends streams a crash left open: all of them (single node, at start),
    /// or those of shards just taken over (cluster).
    Recover {
        shards: Option<Vec<u32>>,
    },
    /// A webhook handled the outbox up to `upto`; `dropped` gave up.
    WebhookAck {
        name: String,
        upto: i64,
        /// Events given up on: (webhook id, conversation, seq).
        dropped: Vec<(String, String, i64)>,
        /// Every configured webhook, to clean up rows all of them handled.
        all: Vec<String>,
    },
    AgentGone(String),
    Barrier,
    // control database
    /// `owner_cap`: an agent is refused when its owner already has this
    /// many (checked here, so concurrent creations cannot pass it).
    CreateUser {
        user: User,
        owner_cap: Option<usize>,
        /// The owner creates it in their client: refused unless the owner
        /// may create agents (checked here, so a revocation committed first
        /// always wins, docs/13 §4.1).
        #[serde(default)]
        owner_must_be_allowed: bool,
    },
    /// Replaces a person's permission overrides (control database).
    SetPermissions {
        user_id: String,
        overrides: Overrides,
    },
    /// Console accounts, sessions, keys and app settings (control database).
    Admin(AdminOp),
    /// A new device token; `revoke_others` removes every other token of the
    /// user in the same write (agent key rotation). Returns the revoked
    /// device ids.
    IssueToken {
        row: crate::db::TokenRow,
        revoke_others: bool,
        /// Refused (`conflict`) when the user already has this many tokens.
        max_devices: Option<usize>,
    },
    /// Looks up a token unknown to memory (issued by another process) and
    /// adds it; returns it with its user.
    LoadToken {
        hash: String,
    },
    /// All tokens of a user, or of one device. Returns the removed hashes.
    /// `cutoff` (seconds): client tokens of the user (or that device)
    /// issued before it are refused from then on.
    RevokeTokens {
        user_id: String,
        device_id: Option<String>,
        #[serde(default)]
        cutoff: Option<i64>,
    },
    /// A person's device seen for the first time with a signed client
    /// token, or again with another signing key (`issued_by`). Returns
    /// whether it is new.
    RegisterDevice {
        row: crate::db::TokenRow,
    },
    CreateSigningKey(crate::db::SigningKeyRow),
    /// Returns whether a key was revoked; devices that last connected
    /// with it are disconnected.
    RevokeSigningKey {
        kid: String,
    },
    /// Control database: forgets people's devices unused since
    /// `idle_before` (D94).
    ExpireTokens {
        idle_before: i64,
    },
    TouchToken {
        hash: String,
    },
    UpdateUser {
        user_id: String,
        display_name: Option<String>,
        avatar: Option<String>,
        fields: Option<Value>,
    },
    SetDeactivated {
        user_id: String,
        on: bool,
    },
    /// An agent's checkpoint write, compared against `version` (docs/03-agents.md
    /// §7.2).
    StatePut {
        agent_id: String,
        key: String,
        value: String,
        version: i64,
        max_keys: i64,
    },
    StateDelete {
        agent_id: String,
        key: String,
        version: i64,
    },
    /// Files (docs/02 §2.7): an upload checked and recorded in its
    /// conversation's database; `upload_id` is the S3 multipart upload the
    /// hub started for it.
    FileStart {
        conv_id: String,
        user: User,
        file_id: String,
        name: String,
        size: u64,
        mime: Option<String>,
        thumb_size: Option<u64>,
        upload_id: Option<String>,
    },
    /// The upload as recorded, for the hub to check the stored objects.
    FileCheck {
        conv_id: String,
        user: User,
        file_id: String,
    },
    /// The hub checked the objects: the file is usable.
    FileReady {
        conv_id: String,
        user: User,
        file_id: String,
        mime: String,
        kind: String,
        size: Option<(i64, i64)>,
        thumb: Option<(i64, String)>,
    },
    /// The objects failed the checks: the upload is thrown away.
    FileDiscard {
        conv_id: String,
        file_id: String,
    },
    /// Throws away expired uploads and files (maintenance).
    FileSweep {
        now: i64,
    },
    /// Objects of a thrown away file were deleted (or not yet).
    TrashDone {
        file_id: String,
        /// None: done; Some: try again at this time.
        retry_at: Option<i64>,
    },
    /// The key that signs the server's own file addresses, made once.
    FilesKey {
        candidate: String,
    },
    /// The owner stops or resumes an agent (control database), then each
    /// of its conversations records it (docs/03-agents.md §4.3–§4.4).
    SetStopped {
        agent_id: String,
        on: bool,
        by: String,
    },
    AgentStopped {
        conv_id: String,
        agent: User,
        on: bool,
        by: String,
        reason: Option<String>,
    },
    /// Moderation silences a member of a conversation until `until` (ms),
    /// or lifts it (None): they cannot send, edit or stream there.
    Silence {
        conv_id: String,
        user_id: String,
        until: Option<i64>,
        by: String,
        reason: Option<String>,
    },
    /// The owner destroys an agent (control database), then it is taken
    /// out of each conversation.
    Destroy {
        agent_id: String,
        by: String,
    },
    RemoveDestroyed {
        conv_id: String,
        agent: User,
        by: String,
    },
    /// Read-modify-write of a user's credentials in the writer, so
    /// concurrent logins count failures and TOTP steps exactly.
    Credentials {
        user_id: String,
        change: CredChange,
    },
    EncryptExisting {
        table: String,
        limit: i64,
        /// The other way round: encrypted values back to plaintext (B4).
        #[serde(default)]
        decrypt: bool,
    },
    RewrapKeys {
        new_key: String,
    },
    CreateApiKey(crate::db::ApiKeyRow),
    RevokeApiKey {
        key_id: String,
    },
    IdemPut {
        key_id: String,
        idem_key: String,
        request_hash: String,
        status: i64,
        response: String,
    },
    UpsertUnit {
        name: String,
        database: String,
    },
    RegisterConv {
        entry: DirEntry,
        member_ids: Vec<String>,
    },
    MemberIndexAdd {
        conv_id: String,
        user_id: String,
    },
    DirPut(DirEntry),
    DirDelete(String),
    // conversation databases
    CreateConv {
        id: String,
        kind: String,
        title: String,
        unit: String,
        creator: User,
        members: Vec<User>,
        /// The person who owns a conversation an agent starts.
        manager: Option<User>,
        /// Task channels: the member responsible for the task.
        assignee: Option<User>,
    },
    AddMember {
        conv_id: String,
        actor: User,
        user: User,
        history: Option<i64>,
        /// Most members a group may have (`clients.max_group_members`).
        cap: usize,
    },
    /// Removal by someone else, or leaving when `user` is the actor.
    RemoveMember {
        conv_id: String,
        actor: User,
        user: User,
    },
    Dissolve {
        conv_id: String,
        actor: User,
    },
    // control database, membership lifecycle
    /// Sync positions a device stored (docs/08 §2.1): (conv, seq, side seq).
    AckCursors {
        device_id: String,
        user_id: String,
        acks: Vec<(String, i64, i64)>,
        /// Read/mute state per conversation the acknowledged responses
        /// carried (M8).
        told: Vec<(String, String)>,
    },
    /// A device was told a conversation was dissolved (control database):
    /// it is not told again.
    DissolvedSeen {
        device_id: String,
        user_id: String,
        conv_id: String,
    },
    /// A device logged out (or every device of a user): their positions in
    /// this database go.
    DropDevice {
        user_id: String,
        device_id: Option<String>,
    },
    /// Control database: a user left (or lost) a conversation at `seq`.
    Departure {
        user_id: String,
        conv_id: String,
        seq: i64,
        dissolved: bool,
    },
    /// Control database: the user is a member again.
    Rejoined {
        user_id: String,
        conv_id: String,
    },
    SetPrefs {
        user_id: String,
        prefs: Value,
    },
    MemberIndexRemove {
        conv_id: String,
        user_id: String,
    },
    /// Control database: a team agent gets a new owner (checked here, so
    /// concurrent transfers cannot pass `agents.max_per_owner`).
    TransferAgent {
        agent_id: String,
        owner_id: String,
        owner_cap: usize,
    },
    /// Every database: members rows carry their agent's owner.
    SetMemberOwner {
        agent_id: String,
        owner_id: String,
    },
    /// The control database's view of a conversation in another database,
    /// in one transaction: `joined` are members (indexed, no departure),
    /// `left` are not (departure up to seq first, then off the index).
    IndexSync {
        conv_id: String,
        joined: Vec<String>,
        left: Vec<(String, i64)>,
    },
    ForgetConv(String),
    SetBlock {
        user_id: String,
        target: String,
        on: bool,
    },
    DmPut {
        pair_key: String,
        conv_id: String,
    },
    /// One direct conversation per pair, also across cluster nodes: claims
    /// the pair for a new conversation or returns the existing one.
    DmClaim {
        pair_key: String,
        conv_id: String,
        /// Gives the claim back (the conversation was not created).
        release: bool,
    },
    StopConvStreams(String),
    ImportConv {
        conv_id: String,
        rows: Vec<(String, Vec<Vec<Cell>>)>,
    },
    DeleteConv(String),
    Audit {
        actor: String,
        action: String,
        conv_id: String,
        target: Option<String>,
        detail: Value,
    },
    Stats,
    // account deletion and moderation (docs/04 §4.2)
    EraseStart {
        user_id: String,
        by: Option<String>,
    },
    EraseInConv {
        conv_id: String,
        user: User,
    },
    EraseProgress {
        user_id: String,
    },
    EraseFinish {
        user_id: String,
        name: String,
    },
    ReportPut {
        reporter: String,
        kind: String,
        target: String,
        conv_id: Option<String>,
        category: String,
        severity: i64,
        body: Value,
        due_at: i64,
        per_hour: u32,
    },
    ReportResolve {
        report_id: String,
        decision: Value,
        by: String,
        suspend_until: Option<i64>,
        ban: bool,
    },
    ModerationUpkeep {
        keep_before: i64,
    },
}

impl Op {
    /// Where the command must run in a cluster (docs/11 §5).
    pub fn route(&self) -> crate::cluster::Route {
        use crate::cluster::Route;
        match self {
            Op::Client { conv, .. } => Route::Conv(conv.clone()),
            Op::Recover { shards: Some(s) } => Route::Shards(s.clone()),
            Op::Recover { shards: None } => Route::Local,
            Op::Tick
            | Op::Forget { .. }
            | Op::ClearCaches
            | Op::Barrier
            | Op::AgentGone(_)
            | Op::Stats => Route::Local,
            Op::WebhookAck { .. } | Op::EncryptExisting { .. } | Op::RewrapKeys { .. } => {
                Route::Maintenance
            }
            Op::CreateConv { id, .. } => Route::Conv(id.clone()),
            Op::AddMember { conv_id, .. }
            | Op::RemoveMember { conv_id, .. }
            | Op::Dissolve { conv_id, .. }
            | Op::AgentStopped { conv_id, .. }
            | Op::Silence { conv_id, .. }
            | Op::RemoveDestroyed { conv_id, .. }
            | Op::ImportConv { conv_id, .. }
            | Op::EraseInConv { conv_id, .. }
            | Op::FileStart { conv_id, .. }
            | Op::FileCheck { conv_id, .. }
            | Op::FileReady { conv_id, .. }
            | Op::FileDiscard { conv_id, .. } => Route::Conv(conv_id.clone()),
            Op::FileSweep { .. } | Op::TrashDone { .. } => Route::Maintenance,
            Op::StopConvStreams(c) | Op::DeleteConv(c) => Route::Conv(c.clone()),
            Op::TouchToken { .. }
            | Op::AckCursors { .. }
            | Op::DropDevice { .. }
            | Op::SetMemberOwner { .. }
            | Op::Audit { .. } => Route::Anywhere,
            Op::CreateUser { .. }
            | Op::SetPermissions { .. }
            | Op::Admin(_)
            | Op::IssueToken { .. }
            | Op::LoadToken { .. }
            | Op::RevokeTokens { .. }
            | Op::RegisterDevice { .. }
            | Op::CreateSigningKey(_)
            | Op::RevokeSigningKey { .. }
            | Op::ExpireTokens { .. }
            | Op::UpdateUser { .. }
            | Op::SetDeactivated { .. }
            | Op::StatePut { .. }
            | Op::StateDelete { .. }
            | Op::SetStopped { .. }
            | Op::Destroy { .. }
            | Op::Credentials { .. }
            | Op::CreateApiKey(_)
            | Op::RevokeApiKey { .. }
            | Op::IdemPut { .. }
            | Op::UpsertUnit { .. }
            | Op::RegisterConv { .. }
            | Op::MemberIndexAdd { .. }
            | Op::DirPut(_)
            | Op::DirDelete(_)
            | Op::Departure { .. }
            | Op::Rejoined { .. }
            | Op::DissolvedSeen { .. }
            | Op::SetPrefs { .. }
            | Op::MemberIndexRemove { .. }
            | Op::IndexSync { .. }
            | Op::TransferAgent { .. }
            | Op::ForgetConv(_)
            | Op::SetBlock { .. }
            | Op::DmPut { .. }
            | Op::DmClaim { .. }
            | Op::EraseStart { .. }
            | Op::EraseProgress { .. }
            | Op::EraseFinish { .. }
            | Op::ReportPut { .. }
            | Op::ReportResolve { .. }
            | Op::ModerationUpkeep { .. }
            | Op::FilesKey { .. } => Route::Control,
        }
    }
}

impl Op {
    pub fn is_tick(&self) -> bool {
        matches!(self, Op::Tick)
    }

    pub fn name(&self) -> &'static str {
        match self {
            Op::Client { msg, .. } => msg.name(),
            Op::Tick => "tick",
            Op::Forget { .. } => "forget",
            Op::ClearCaches => "clear_caches",
            Op::Recover { .. } => "recover",
            Op::WebhookAck { .. } => "webhook_ack",
            Op::AgentGone(_) => "agent_gone",
            Op::Barrier => "barrier",
            Op::CreateUser { .. } => "create_user",
            Op::SetPermissions { .. } => "set_permissions",
            Op::Admin(_) => "admin",
            Op::IssueToken { .. } => "issue_token",
            Op::LoadToken { .. } => "load_token",
            Op::RevokeTokens { .. } => "revoke_tokens",
            Op::RegisterDevice { .. } => "register_device",
            Op::CreateSigningKey(_) => "create_signing_key",
            Op::RevokeSigningKey { .. } => "revoke_signing_key",
            Op::TouchToken { .. } => "touch_token",
            Op::ExpireTokens { .. } => "expire_tokens",
            Op::UpdateUser { .. } => "update_user",
            Op::SetDeactivated { .. } => "set_deactivated",
            Op::StatePut { .. } => "state_put",
            Op::StateDelete { .. } => "state_delete",
            Op::FileStart { .. } => "file_start",
            Op::FileCheck { .. } => "file_check",
            Op::FileReady { .. } => "file_ready",
            Op::FileDiscard { .. } => "file_discard",
            Op::FileSweep { .. } => "file_sweep",
            Op::TrashDone { .. } => "trash_done",
            Op::FilesKey { .. } => "files_key",
            Op::SetStopped { .. } => "set_stopped",
            Op::AgentStopped { .. } => "agent_stopped",
            Op::Silence { .. } => "silence",
            Op::Destroy { .. } => "destroy",
            Op::RemoveDestroyed { .. } => "remove_destroyed",
            Op::Credentials { .. } => "credentials",
            Op::EncryptExisting { .. } => "encrypt_existing",
            Op::RewrapKeys { .. } => "rewrap_keys",
            Op::CreateApiKey(_) => "create_api_key",
            Op::RevokeApiKey { .. } => "revoke_api_key",
            Op::IdemPut { .. } => "idem_put",
            Op::UpsertUnit { .. } => "upsert_unit",
            Op::RegisterConv { .. } => "register_conv",
            Op::MemberIndexAdd { .. } => "member_index_add",
            Op::DirPut(_) => "dir_put",
            Op::DirDelete(_) => "dir_delete",
            Op::CreateConv { .. } => "create_conv",
            Op::AddMember { .. } => "add_member",
            Op::RemoveMember { .. } => "remove_member",
            Op::Dissolve { .. } => "dissolve",
            Op::MemberIndexRemove { .. } => "member_index_remove",
            Op::IndexSync { .. } => "index_sync",
            Op::TransferAgent { .. } => "transfer_agent",
            Op::SetMemberOwner { .. } => "set_member_owner",
            Op::AckCursors { .. } => "ack_cursors",
            Op::DissolvedSeen { .. } => "dissolved_seen",
            Op::DropDevice { .. } => "drop_device",
            Op::Departure { .. } => "departure",
            Op::Rejoined { .. } => "rejoined",
            Op::SetPrefs { .. } => "set_prefs",
            Op::ForgetConv(_) => "forget_conv",
            Op::SetBlock { .. } => "set_block",
            Op::DmPut { .. } => "dm_put",
            Op::DmClaim { .. } => "dm_claim",
            Op::StopConvStreams(_) => "stop_conv_streams",
            Op::ImportConv { .. } => "import_conv",
            Op::DeleteConv(_) => "delete_conv",
            Op::Audit { .. } => "audit",
            Op::Stats => "stats",
            Op::EraseStart { .. } => "erase_start",
            Op::EraseInConv { .. } => "erase_in_conv",
            Op::EraseProgress { .. } => "erase_progress",
            Op::EraseFinish { .. } => "erase_finish",
            Op::ReportPut { .. } => "report_put",
            Op::ReportResolve { .. } => "report_resolve",
            Op::ModerationUpkeep { .. } => "moderation_upkeep",
        }
    }
}

fn err(code: &'static str, msg: impl Into<String>) -> ApiError {
    ApiError::new(code, msg)
}

/// Owners and admins manage members and moderate messages.
fn is_manager(role: &str) -> bool {
    role == "owner" || role == "admin"
}

/// The sender's id of a message (retries reuse it): 1..128 bytes, as on
/// the open API.
/// Events retention deletes per clean-up round, at most.
const PURGE_BATCH: i64 = 20_000;

pub fn check_client_msg_id(id: &str) -> ApiResult<()> {
    if id.is_empty() || id.len() > 128 || id.contains('\0') {
        return Err(ApiError::new(
            "bad_request",
            "client_msg_id must be 1..128 bytes",
        ));
    }
    Ok(())
}

/// Message text: 1..64KB, and no NUL (PostgreSQL cannot store it).
pub fn check_text(text: &str) -> ApiResult<()> {
    if text.is_empty() || text.len() > MAX_MESSAGE_BYTES {
        return Err(rule!(
            "msg.text_size",
            "bad_request",
            "text must be 1..64KB"
        ));
    }
    if text.contains('\0') {
        return Err(rule!(
            "msg.text_nul",
            "bad_request",
            "text must not contain NUL"
        ));
    }
    Ok(())
}

/// Namespaced kind of a custom message or signal: at least two dot-separated
/// segments of `a-z0-9_-` starting with a letter, at most 64 bytes, and not
/// under the reserved `agentim.` namespace.
pub fn check_custom_kind(kind: &str) -> ApiResult<()> {
    let seg_ok = |s: &str| {
        s.starts_with(|c: char| c.is_ascii_lowercase())
            && s.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
    };
    let ok = kind.len() <= 64 && kind.split('.').count() >= 2 && kind.split('.').all(seg_ok);
    if !ok {
        return Err(rule!(
            "msg.kind_namespace",
            "bad_request",
            "custom kinds are namespaced like com.example.order (a-z, 0-9, _ and -, at most 64 bytes)",
        ));
    }
    if kind.starts_with("agentim.") {
        return Err(rule!(
            "msg.kind_reserved",
            "bad_request",
            "the agentim. namespace is reserved"
        ));
    }
    Ok(())
}

/// A JSON object of at most `max` serialized bytes.
pub fn check_json_object(v: &Value, what: &str, max: usize) -> ApiResult<()> {
    if !v.is_object() {
        return Err(rule!(
            "msg.json_object",
            "bad_request",
            format!("{what} must be a JSON object")
        ));
    }
    if v.to_string().len() > max {
        return Err(rule!(
            "msg.json_size",
            "bad_request",
            format!("{what} is larger than {max} bytes"),
        ));
    }
    Ok(())
}

/// Kind and payload of a new message: `text` takes no payload; a custom kind
/// may carry one, and its `text` is the fallback older clients show.
pub fn check_kind(kind: Option<String>, payload: &Option<Value>) -> ApiResult<String> {
    match kind.as_deref() {
        None | Some("text") => {
            if payload.is_some() {
                return Err(rule!(
                    "msg.payload_needs_kind",
                    "bad_request",
                    "payload needs a custom kind"
                ));
            }
            Ok("text".into())
        }
        Some(k @ ("image" | "file")) => {
            files::check_payload(k, payload)?;
            Ok(k.to_string())
        }
        Some(cards::KIND) => {
            cards::check_card(payload.as_ref().ok_or_else(|| {
                rule!(
                    "msg.card_needs_payload",
                    "bad_request",
                    "a card needs a payload"
                )
            })?)?;
            Ok(cards::KIND.to_string())
        }
        Some(k) => {
            check_custom_kind(k)?;
            if let Some(p) = payload {
                check_json_object(p, "payload", MAX_PAYLOAD_BYTES)?;
            }
            Ok(k.to_string())
        }
    }
}

/// What content policies look at: the text and, for custom kinds, the payload.
pub fn scan_text<'t>(text: &'t str, payload: Option<&Value>) -> std::borrow::Cow<'t, str> {
    match payload {
        None => text.into(),
        Some(p) => format!("{text}\n{p}").into(),
    }
}

pub struct Ctx<'a, 'c> {
    pub db: Db<'c>,
    pub st: &'a mut ShardState,
    pub sh: &'a Shared,
    pub out: &'a mut Vec<Out>,
    pub idx: usize,
    /// Keep this command's writes even though it returns an error.
    pub commit_on_error: bool,
    /// Cluster: shards this batch wrote, whose leases it checks before
    /// committing (docs/11 §4.3).
    pub touched: &'a mut std::collections::BTreeSet<u32>,
}

impl Ctx<'_, '_> {
    /// Whether this node may write the conversation (always on a single
    /// node; in a cluster while it holds the conversation's shard).
    pub fn owns(&self, conv_id: &str) -> bool {
        match self.sh.cluster.get() {
            None => true,
            Some(c) => c
                .held(self.idx, crate::cluster::shard_of(conv_id, c.shards()))
                .is_some(),
        }
    }

    /// Records that this batch writes a conversation's shard.
    pub fn touch(&mut self, conv_id: &str) {
        if let Some(c) = self.sh.cluster.get() {
            self.touched
                .insert(crate::cluster::shard_of(conv_id, c.shards()));
        }
    }

    /// Whether this node does the database-wide upkeep; it is then part of
    /// the batch's check.
    fn upkeep(&mut self) -> bool {
        match self.sh.cluster.get() {
            None => true,
            Some(c) => {
                let m = crate::cluster::maintenance_shard(c.shards());
                let held = c.held(self.idx, m).is_some();
                if held {
                    self.touched.insert(m);
                }
                held
            }
        }
    }

    pub async fn run(&mut self, op: Op) -> ApiResult<Value> {
        // Changes other processes keep in memory are counted, so they
        // reload (CLI, other cluster nodes; docs/11 §6.5).
        let invalidates = matches!(
            op,
            Op::SetStopped { .. }
                | Op::Destroy { .. }
                | Op::SetBlock { .. }
                | Op::TransferAgent { .. }
                | Op::SetMemberOwner { .. }
                | Op::UpdateUser { .. }
        );
        // The request's trace is current while the writer runs it, here or
        // on another node (docs/14 §4.1).
        let trace = match &op {
            Op::Client { sess, .. } => sess.trace.clone(),
            _ => None,
        };
        let r = match trace {
            Some(t) => {
                let span = tracing::info_span!("op", trace_id = %t.trace_id, span_id = %t.span_id);
                crate::trace::scope(
                    t,
                    tracing::Instrument::instrument(Box::pin(self.run_op(op)), span),
                )
                .await
            }
            None => self.run_op(op).await,
        };
        if r.is_ok() && invalidates {
            self.db.bump_meta(crate::db::AUTH_EPOCH).await?;
            self.out.push(Out::Invalidate);
        }
        r
    }

    async fn run_op(&mut self, op: Op) -> ApiResult<Value> {
        match op {
            Op::Client { sess, msg, .. } => self.client(&sess, msg).await,
            Op::Tick => {
                self.tick().await?;
                Ok(json!({}))
            }
            Op::WebhookAck {
                name,
                upto,
                dropped,
                all,
            } => {
                for (id, conv, seq) in &dropped {
                    self.db
                        .audit(
                            None,
                            "webhook.dropped",
                            Some(conv).filter(|c| !c.is_empty()).map(String::as_str),
                            Some(id),
                            json!({"webhook": name, "seq": seq}),
                        )
                        .await?;
                }
                self.db.webhook_ack(&name, upto, &all).await?;
                Ok(json!({}))
            }
            Op::Recover { shards } => {
                for msg in self.db.streaming_messages().await? {
                    if let (Some(s), Some(c)) = (&shards, self.sh.cluster.get())
                        && !s.contains(&crate::cluster::shard_of(&msg.conv_id, c.shards()))
                    {
                        continue;
                    }
                    let ev = self
                        .db
                        .update_message(
                            &msg.msg_id,
                            "msg.updated",
                            &msg.sender_id,
                            None,
                            Some("failed"),
                            Some("server_restart"),
                        )
                        .await?;
                    metrics::counter!(m::STREAMS_FINISHED, "state" => "failed", "reason" => "server_restart").increment(1);
                    self.emit(ev).await?;
                }
                Ok(json!({}))
            }
            Op::ClearCaches => {
                self.st.members.clear();
                self.st.recips.clear();
                Ok(json!({}))
            }
            Op::Forget { shards } => {
                if let Some(c) = self.sh.cluster.get() {
                    let n = c.shards();
                    let of = |conv: &str| shards.contains(&crate::cluster::shard_of(conv, n));
                    let gone: Vec<String> = self
                        .st
                        .streams
                        .values()
                        .filter(|s| of(&s.conv_id))
                        .map(|s| s.msg_id.clone())
                        .collect();
                    for id in gone {
                        self.st.streams.remove(&id);
                        self.stream_removed(&id);
                    }
                    self.st.members.retain(|conv, _| !of(conv));
                    self.st.recips.retain(|conv, _| !of(conv));
                }
                Ok(json!({}))
            }
            Op::AgentGone(agent_id) => {
                let ids: Vec<String> = self
                    .st
                    .streams
                    .values()
                    .filter(|s| s.sender.id == agent_id)
                    .map(|s| s.msg_id.clone())
                    .collect();
                for id in ids {
                    self.finish_stream(&id, "failed", Some("disconnected"), &agent_id)
                        .await?;
                }
                Ok(json!({}))
            }
            Op::Barrier => Ok(json!({})),
            Op::CreateUser {
                user,
                owner_cap,
                owner_must_be_allowed,
            } => {
                if owner_must_be_allowed
                    && let Some(owner) = &user.owner_id
                    && !self
                        .db
                        .permissions(owner)
                        .await?
                        .over(&self.sh.allow.read().unwrap_or_else(|e| e.into_inner()))
                        .create_agents
                {
                    return Err(ApiError::new(
                        "forbidden",
                        "the app does not allow this: creating agents",
                    ));
                }
                if self.db.name_taken(&user.name).await? {
                    return Err(ApiError::new("conflict", "name taken"));
                }
                if let (Some(cap), Some(owner)) = (owner_cap, &user.owner_id)
                    && user.kind == UserKind::Agent
                    && self.db.count_agents_of(owner).await? >= cap as i64
                {
                    return Err(ApiError::new(
                        "conflict",
                        format!("the owner already has {cap} agents (agents.max_per_owner)"),
                    ));
                }
                self.db.insert_user(&user).await?;
                Ok(json!({}))
            }
            Op::IssueToken {
                row,
                revoke_others,
                max_devices,
            } => {
                if let Some(max) = max_devices
                    && !revoke_others
                    && self.db.devices(&row.user_id).await?.len() >= max
                {
                    return Err(ApiError::new(
                        "conflict",
                        format!("at most {max} keys; revoke one first"),
                    ));
                }
                self.db.insert_token(&row).await?;
                let mut remove = vec![];
                let mut kick = vec![];
                if revoke_others {
                    for d in self.db.devices(&row.user_id).await? {
                        if d.device_id != row.device_id {
                            remove.extend(
                                self.db
                                    .delete_tokens(&row.user_id, Some(&d.device_id))
                                    .await?,
                            );
                            kick.push((row.user_id.clone(), Some(d.device_id)));
                        }
                    }
                }
                let revoked: Vec<String> = kick.iter().filter_map(|k| k.1.clone()).collect();
                self.out.push(Out::Tokens {
                    add: vec![(row.hash.clone(), TokenInfo::of(&row))],
                    remove,
                    kick,
                });
                Ok(json!({"revoked": revoked}))
            }
            Op::LoadToken { hash } => {
                let Some(t) = self.db.token_by_hash(&hash).await? else {
                    return Err(ApiError::new("not_found", "token"));
                };
                let Some((user, off)) = self.db.user_by_id(&t.user_id).await? else {
                    return Err(ApiError::new("not_found", "user"));
                };
                self.out.push(Out::Tokens {
                    add: vec![(t.hash.clone(), TokenInfo::of(&t))],
                    remove: vec![],
                    kick: vec![],
                });
                Ok(json!({"user": user, "deactivated": off}))
            }
            Op::RevokeTokens {
                user_id,
                device_id,
                cutoff,
            } => {
                let gone = self
                    .db
                    .delete_tokens(&user_id, device_id.as_deref())
                    .await?;
                if let Some(at) = cutoff {
                    let dev = device_id.clone().unwrap_or_default();
                    self.db.set_cutoff(&user_id, &dev, at).await?;
                    self.out.push(Out::Cutoff {
                        user_id: user_id.clone(),
                        device_id: dev,
                        at,
                    });
                }
                self.out.push(Out::Tokens {
                    add: vec![],
                    remove: gone.clone(),
                    kick: vec![(user_id, device_id)],
                });
                Ok(json!({"hashes": gone}))
            }
            Op::RegisterDevice { row } => {
                let new = match self.db.token_by_hash(&row.hash).await? {
                    Some(t) => {
                        if t.issued_by != row.issued_by
                            && let Some(by) = &row.issued_by
                        {
                            self.db.set_token_issuer(&row.hash, by).await?;
                        }
                        false
                    }
                    None => {
                        self.db.insert_token(&row).await?;
                        true
                    }
                };
                self.out.push(Out::Tokens {
                    add: vec![(row.hash.clone(), TokenInfo::of(&row))],
                    remove: vec![],
                    kick: vec![],
                });
                Ok(json!({"new": new}))
            }
            Op::Admin(a) => self.admin_op(a).await,
            Op::SetPermissions { user_id, overrides } => {
                self.db.set_permissions(&user_id, &overrides).await?;
                self.out.push(Out::Perms {
                    user_id,
                    overrides: overrides.clone(),
                });
                Ok(json!({}))
            }
            Op::CreateSigningKey(k) => {
                self.db.insert_signing_key(&k).await?;
                self.out.push(Out::Invalidate);
                Ok(json!({}))
            }
            Op::RevokeSigningKey { kid } => {
                if !self.db.revoke_signing_key(&kid).await? {
                    return Ok(json!({"revoked": false}));
                }
                let by = format!("jwt:{kid}");
                let kick: Vec<(String, Option<String>)> = self
                    .db
                    .tokens_all()
                    .await?
                    .into_iter()
                    .filter(|t| t.issued_by.as_deref() == Some(by.as_str()))
                    .map(|t| (t.user_id, Some(t.device_id)))
                    .collect();
                self.out.push(Out::KeyRevoked { kid });
                self.out.push(Out::Tokens {
                    add: vec![],
                    remove: vec![],
                    kick,
                });
                Ok(json!({"revoked": true}))
            }
            Op::ExpireTokens { idle_before } => {
                let gone = self.db.expire_tokens(idle_before).await?;
                let n = gone.len();
                if n > 0 {
                    self.out.push(Out::Tokens {
                        add: vec![],
                        remove: gone.iter().map(|(h, _, _)| h.clone()).collect(),
                        kick: gone.into_iter().map(|(_, u, d)| (u, Some(d))).collect(),
                    });
                }
                Ok(json!({"expired": n}))
            }
            Op::TouchToken { hash } => {
                self.db.touch_token(&hash).await?;
                Ok(json!({}))
            }
            Op::UpdateUser {
                user_id,
                display_name,
                avatar,
                fields,
            } => {
                self.db
                    .update_user(
                        &user_id,
                        display_name.as_deref(),
                        avatar.as_deref(),
                        fields.as_ref(),
                    )
                    .await?;
                Ok(json!({}))
            }
            Op::StatePut {
                agent_id,
                key,
                value,
                version,
                max_keys,
            } => {
                self.state_put(&agent_id, &key, &value, version, max_keys)
                    .await
            }
            Op::SetStopped { agent_id, on, by } => self.set_stopped(&agent_id, on, &by).await,
            Op::AgentStopped {
                conv_id,
                agent,
                on,
                by,
                reason,
            } => {
                self.agent_stopped(&conv_id, &agent, on, &by, reason.as_deref())
                    .await
            }
            Op::Silence {
                conv_id,
                user_id,
                until,
                by,
                reason,
            } => {
                self.silence_member(&conv_id, &user_id, until, &by, reason.as_deref())
                    .await
            }
            Op::Destroy { agent_id, by } => self.destroy(&agent_id, &by).await,
            Op::EraseStart { user_id, by } => self.erase_start(&user_id, by.as_deref()).await,
            Op::EraseInConv { conv_id, user } => self.erase_in_conv(&conv_id, &user).await,
            Op::EraseProgress { user_id } => {
                self.db.erasure_progress(&user_id).await?;
                Ok(json!({}))
            }
            Op::EraseFinish { user_id, name } => self.erase_finish(&user_id, &name).await,
            Op::ReportPut {
                reporter,
                kind,
                target,
                conv_id,
                category,
                severity,
                body,
                due_at,
                per_hour,
            } => {
                self.report_put(
                    &reporter,
                    &kind,
                    &target,
                    conv_id.as_deref(),
                    &category,
                    severity,
                    &body,
                    due_at,
                    per_hour,
                )
                .await
            }
            Op::ReportResolve {
                report_id,
                decision,
                by,
                suspend_until,
                ban,
            } => {
                self.report_resolve(&report_id, &decision, &by, suspend_until, ban)
                    .await
            }
            Op::ModerationUpkeep { keep_before } => self.moderation_upkeep(keep_before).await,
            Op::RemoveDestroyed { conv_id, agent, by } => {
                self.agent_gone(&conv_id, &agent, &by).await
            }
            Op::StateDelete {
                agent_id,
                key,
                version,
            } => self.state_delete(&agent_id, &key, version).await,
            Op::FileStart {
                conv_id,
                user,
                file_id,
                name,
                size,
                mime,
                thumb_size,
                upload_id,
            } => {
                self.file_start(
                    &user,
                    &conv_id,
                    &file_id,
                    &name,
                    size,
                    mime.as_deref(),
                    thumb_size,
                    upload_id,
                )
                .await
            }
            Op::FileCheck {
                conv_id,
                user,
                file_id,
            } => self.upload_check(&user, &conv_id, &file_id).await,
            Op::FileReady {
                conv_id,
                user,
                file_id,
                mime,
                kind,
                size,
                thumb,
            } => {
                self.file_ready(&user, &conv_id, &file_id, &mime, &kind, size, thumb)
                    .await
            }
            Op::FileDiscard { conv_id, file_id } => self.file_discard(&conv_id, &file_id).await,
            Op::FileSweep { now } => self.file_sweep(now).await,
            Op::TrashDone { file_id, retry_at } => {
                match retry_at {
                    None => self.db.trash_done(&file_id).await?,
                    Some(t) => self.db.trash_retry(&file_id, t).await?,
                }
                Ok(json!({}))
            }
            Op::FilesKey { candidate } => {
                Ok(json!({"key": self.db.meta_once("files_url_key", &candidate).await?}))
            }
            Op::SetDeactivated { user_id, on } => {
                self.db.set_deactivated(&user_id, on).await?;
                self.out.push(Out::Invalidate);
                Ok(json!({}))
            }
            Op::Credentials { user_id, change } => {
                let mut c = self.db.credentials(&user_id).await?.unwrap_or_default();
                let now = now_ms();
                let v = match change {
                    CredChange::SetPassword(h) => {
                        c.password_hash = Some(h);
                        c.failures = 0;
                        c.locked_until = None;
                        json!({})
                    }
                    CredChange::Failed { max, lock_ms } => {
                        c.failures += 1;
                        if c.failures >= max {
                            c.locked_until = Some(now + lock_ms);
                            c.failures = 0;
                        }
                        json!({"locked_until": c.locked_until})
                    }
                    CredChange::Succeeded { totp_step } => {
                        if let Some(s) = totp_step {
                            // A code is single-use, also between racing logins.
                            if c.totp_last_step.is_some_and(|l| s <= l) {
                                return Err(rule!(
                                    "admin.totp_code_replayed",
                                    "conflict",
                                    "this code was already used"
                                ));
                            }
                            c.totp_last_step = Some(s);
                        }
                        c.failures = 0;
                        c.locked_until = None;
                        json!({})
                    }
                    CredChange::TotpPending(secret) => {
                        c.totp_pending = Some(secret);
                        json!({})
                    }
                    CredChange::TotpEnable { step } => {
                        let Some(p) = c.totp_pending.take() else {
                            return Err(rule!(
                                "admin.totp_setup_first",
                                "conflict",
                                "run totp_setup first"
                            ));
                        };
                        c.totp_secret = Some(p);
                        c.totp_last_step = Some(step);
                        json!({})
                    }
                    CredChange::TotpDisable => {
                        c.totp_secret = None;
                        c.totp_pending = None;
                        json!({})
                    }
                };
                self.db.put_credentials(&user_id, &c).await?;
                Ok(v)
            }
            Op::EncryptExisting {
                table,
                limit,
                decrypt,
            } => {
                let n = if decrypt {
                    self.db.decrypt_existing(&table, limit).await?
                } else {
                    self.db.encrypt_existing(&table, limit).await?
                };
                Ok(json!({"changed": n}))
            }
            Op::RewrapKeys { new_key } => {
                let k = crate::crypto::Kek::from_base64(&new_key)
                    .map_err(|e| rule!("admin.master_key_invalid", "bad_request", e))?;
                let n = self.db.rewrap_keys(&k).await?;
                Ok(json!({"rewrapped": n}))
            }
            Op::CreateApiKey(k) => {
                self.db.insert_api_key(&k).await?;
                self.out.push(Out::ApiKeys);
                Ok(json!({}))
            }
            Op::RevokeApiKey { key_id } => {
                let n = self.db.revoke_api_key(&key_id).await?;
                if n > 0 {
                    self.out.push(Out::ApiKeys);
                }
                Ok(json!({"revoked": n}))
            }
            Op::IdemPut {
                key_id,
                idem_key,
                request_hash,
                status,
                response,
            } => {
                self.db
                    .idem_put(&key_id, &idem_key, &request_hash, status, &response)
                    .await?;
                Ok(json!({}))
            }
            Op::UpsertUnit { name, database } => {
                self.db.upsert_unit(&name, &database).await?;
                Ok(json!({}))
            }
            Op::RegisterConv { entry, member_ids } => {
                self.db.dir_put(&entry).await?;
                for u in member_ids {
                    self.db.member_index_add(&entry.conv_id, &u).await?;
                }
                Ok(json!({}))
            }
            Op::MemberIndexAdd { conv_id, user_id } => {
                self.db.member_index_add(&conv_id, &user_id).await?;
                Ok(json!({}))
            }
            Op::DirPut(e) => {
                self.db.dir_put(&e).await?;
                Ok(json!({}))
            }
            Op::DirDelete(c) => {
                self.db.dir_delete(&c).await?;
                Ok(json!({}))
            }
            Op::CreateConv {
                id,
                kind,
                title,
                unit,
                creator,
                members,
                manager,
                assignee,
            } => {
                let events = self
                    .db
                    .create_conv(
                        &id,
                        &kind,
                        &title,
                        &unit,
                        &creator,
                        &members,
                        manager.as_ref(),
                    )
                    .await?;
                if self.idx == self.sh.control {
                    // Same database as the directory: register atomically.
                    self.db
                        .dir_put(&DirEntry {
                            conv_id: id.clone(),
                            database: self.sh.db_names[self.idx].clone(),
                            unit_id: unit,
                            migrating: false,
                        })
                        .await?;
                    self.db.member_index_add(&id, &creator.id).await?;
                    for u in &members {
                        self.db.member_index_add(&id, &u.id).await?;
                    }
                }
                for ev in events {
                    self.emit(ev).await?;
                }
                if kind == "task" {
                    self.task_created(&id, &creator, assignee.as_ref()).await?;
                }
                Ok(json!({"conv_id": id}))
            }
            Op::AddMember {
                conv_id,
                actor,
                user,
                history,
                cap,
            } => self.add_member(&conv_id, &actor, &user, history, cap).await,
            Op::RemoveMember {
                conv_id,
                actor,
                user,
            } => self.remove_member(&conv_id, &actor, &user).await,
            Op::Dissolve { conv_id, actor } => self.dissolve(&conv_id, &actor).await,
            Op::DissolvedSeen {
                device_id,
                user_id,
                conv_id,
            } => {
                self.db
                    .ack_cursor(&device_id, &conv_id, &user_id, i64::MAX, 0)
                    .await?;
                Ok(json!({}))
            }
            Op::AckCursors {
                device_id,
                user_id,
                acks,
                told,
            } => {
                let mut n = 0;
                for (conv, seq, side) in acks {
                    // A device keeps acknowledging a conversation it lost
                    // until it has the removal (its position exists); one
                    // that never had it gets nothing new.
                    if self.db.member(&conv, &user_id).await?.is_none()
                        && self.db.cursor(&device_id, &conv).await?.is_none()
                    {
                        continue;
                    }
                    self.db
                        .ack_cursor(&device_id, &conv, &user_id, seq, side)
                        .await?;
                    n += 1;
                }
                for (conv, state) in told {
                    self.db.set_cursor_told(&device_id, &conv, &state).await?;
                }
                Ok(json!({"stored": n}))
            }
            Op::DropDevice { user_id, device_id } => {
                let n = match device_id {
                    Some(d) => self.db.drop_device_cursors(&d).await?,
                    None => self.db.drop_user_cursors(&user_id).await?,
                };
                Ok(json!({"dropped": n}))
            }
            Op::Departure {
                user_id,
                conv_id,
                seq,
                dissolved,
            } => {
                self.db
                    .departure_put(&user_id, &conv_id, seq, dissolved)
                    .await?;
                Ok(json!({}))
            }
            Op::Rejoined { user_id, conv_id } => {
                self.db.departure_clear(&user_id, &conv_id).await?;
                Ok(json!({}))
            }
            Op::SetPrefs { user_id, prefs } => {
                self.db.set_prefs(&user_id, &prefs).await?;
                Ok(json!({}))
            }
            Op::MemberIndexRemove { conv_id, user_id } => {
                self.db.member_index_remove(&conv_id, &user_id).await?;
                Ok(json!({}))
            }
            Op::TransferAgent {
                agent_id,
                owner_id,
                owner_cap,
            } => {
                if self.db.count_agents_of(&owner_id).await? >= owner_cap as i64 {
                    return Err(ApiError::new(
                        "conflict",
                        format!(
                            "the new owner already has {owner_cap} agents (agents.max_per_owner)"
                        ),
                    ));
                }
                let prev = self.db.set_agent_owner(&agent_id, &owner_id).await?;
                self.db
                    .audit(
                        Some(SYSTEM_ID),
                        "agent.transferred",
                        None,
                        Some(&agent_id),
                        json!({"from": prev, "to": owner_id}),
                    )
                    .await?;
                Ok(json!({"from": prev}))
            }
            Op::SetMemberOwner { agent_id, owner_id } => {
                self.db.set_member_owner(&agent_id, &owner_id).await?;
                self.st.members.clear();
                self.st.recips.clear();
                Ok(json!({}))
            }
            Op::IndexSync {
                conv_id,
                joined,
                left,
            } => {
                for u in &joined {
                    self.db.member_index_add(&conv_id, u).await?;
                    self.db.departure_clear(u, &conv_id).await?;
                }
                for (u, seq) in &left {
                    self.db.departure_put(u, &conv_id, *seq, false).await?;
                    self.db.member_index_remove(&conv_id, u).await?;
                }
                Ok(json!({}))
            }
            Op::ForgetConv(conv) => {
                self.db.forget_conv(&conv).await?;
                Ok(json!({}))
            }
            Op::SetBlock {
                user_id,
                target,
                on,
            } => {
                self.db.set_block(&user_id, &target, on).await?;
                // The caches change when the write is durable, on every node.
                self.out.push(Out::Block {
                    user_id,
                    target,
                    on,
                });
                Ok(json!({}))
            }
            Op::DmPut { pair_key, conv_id } => {
                self.db.dm_put(&pair_key, &conv_id).await?;
                Ok(json!({}))
            }
            Op::DmClaim {
                pair_key,
                conv_id,
                release: false,
            } => Ok(json!({
                "existing": self.db.dm_claim(&pair_key, &conv_id).await?
            })),
            Op::DmClaim {
                pair_key,
                conv_id,
                release: true,
            } => {
                self.db.dm_release(&pair_key, &conv_id).await?;
                Ok(json!({}))
            }
            Op::StopConvStreams(conv) => {
                let ids: Vec<String> = self
                    .st
                    .streams
                    .values()
                    .filter(|s| s.conv_id == conv)
                    .map(|s| s.msg_id.clone())
                    .collect();
                for id in ids {
                    let sender = self.st.streams[&id].sender.id.clone();
                    self.finish_stream(&id, "cancelled", Some("migrated"), &sender)
                        .await?;
                }
                Ok(json!({}))
            }
            Op::ImportConv { conv_id, rows } => {
                self.db.import_conv(&conv_id, &rows).await?;
                self.st.members.remove(&conv_id);
                self.st.recips.remove(&conv_id);
                Ok(json!({}))
            }
            Op::DeleteConv(conv) => {
                self.db.delete_conv(&conv).await?;
                self.st.members.remove(&conv);
                self.st.recips.remove(&conv);
                Ok(json!({}))
            }
            Op::Audit {
                actor,
                action,
                conv_id,
                target,
                detail,
            } => {
                self.db
                    .audit(
                        Some(&actor),
                        &action,
                        Some(conv_id.as_str()).filter(|c| !c.is_empty()),
                        target.as_deref(),
                        detail,
                    )
                    .await?;
                Ok(json!({}))
            }
            Op::Stats => Ok(
                json!({"counts": self.db.counts().await?, "active_streams": self.st.active_streams(), "audit": self.db.audit_log(50).await?}),
            ),
        }
    }

    async fn client(&mut self, sess: &SessCtx, msg: ClientMsg) -> ApiResult<Value> {
        let me = &sess.user;
        self.agent_running(me)?;
        if let Some(conv) = msg.conv_id() {
            self.own_conv(conv)?;
        }
        match msg {
            ClientMsg::Send {
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
            } => {
                check_client_msg_id(&client_msg_id)?;
                self.send(
                    me,
                    &conv_id,
                    &client_msg_id,
                    &text,
                    (kind, payload),
                    reply_to,
                    causation_id,
                    on_behalf_of,
                    to_user,
                    routing::EnvelopeIn {
                        to,
                        topics,
                        intent,
                        refs,
                        sources,
                    },
                )
                .await
            }
            ClientMsg::Edit {
                msg_id,
                text,
                payload,
                base_version,
            } => self.edit(me, &msg_id, &text, payload, base_version).await,
            ClientMsg::Recall { msg_id } => self.recall(me, &msg_id).await,
            ClientMsg::UploadParts {
                conv_id,
                file_id,
                from,
            } => self.upload_parts(me, &conv_id, &file_id, from).await,
            ClientMsg::FileUrl {
                conv_id,
                file_id,
                variant,
            } => {
                self.file_url(me, &conv_id, &file_id, variant.as_deref())
                    .await
            }
            // The hub checks the store first and comes back with FileStart
            // and FileReady.
            ClientMsg::UploadStart { .. } | ClientMsg::UploadComplete { .. } => {
                Err(err("internal", "uploads go through the hub"))
            }
            ClientMsg::SetMute {
                conv_id,
                mode,
                until,
            } => {
                self.require_member(&conv_id, &me.id).await?;
                if !["none", "all", "mentions"].contains(&mode.as_str()) {
                    return Err(rule!(
                        "member.mute_mode",
                        "bad_request",
                        "mode must be none, all or mentions"
                    ));
                }
                let until = if mode == "none" { None } else { until };
                if until.is_some_and(|u| u <= now_ms()) {
                    return Err(rule!(
                        "member.mute_until",
                        "bad_request",
                        "until must be in the future"
                    ));
                }
                self.db.set_mute(&conv_id, &me.id, &mode, until).await?;
                self.st.members.remove(&conv_id);
                self.st.recips.remove(&conv_id);
                // The user's other devices show the same setting.
                let frame =
                    json!({"type": "mute", "conv_id": conv_id, "mode": mode, "until": until});
                for sid in self.sh.reg.others(&me.id, sess.sid) {
                    self.out.push(Out::ToSession {
                        sid,
                        frame: frame.clone(),
                    });
                }
                Ok(json!({"mode": mode, "until": until}))
            }
            ClientMsg::Read { conv_id, seq } => {
                self.require_member(&conv_id, &me.id).await?;
                // A cursor past the head would hide messages that arrive later.
                let head = self
                    .db
                    .conv(&conv_id)
                    .await?
                    .map(|c| c.last_seq)
                    .unwrap_or(0);
                let seq = seq.min(head);
                self.db.set_read(&conv_id, &me.id, seq).await?;
                // The user's other devices update their unread counts.
                let frame = json!({"type": "read", "conv_id": conv_id, "seq": seq});
                for sid in self.sh.reg.others(&me.id, sess.sid) {
                    self.out.push(Out::ToSession {
                        sid,
                        frame: frame.clone(),
                    });
                }
                Ok(json!({}))
            }
            ClientMsg::Focus {
                conv_id: Some(c), ..
            } => {
                // Late joiners get the accumulated content of in-flight streams.
                // Pending text is flushed first, so the next delta continues
                // after this snapshot instead of repeating part of it.
                let ids: Vec<String> = self
                    .st
                    .streams
                    .values()
                    .filter(|s| s.conv_id == c && matches!(s.target, Target::Main))
                    .map(|s| s.msg_id.clone())
                    .collect();
                for id in &ids {
                    self.flush_one(id);
                }
                for st in ids.iter().filter_map(|id| self.st.streams.get(id)) {
                    self.out.push(Out::ToSession {
                        sid: sess.sid,
                        frame: json!({"type": "delta", "conv_id": st.conv_id, "msg_id": st.msg_id, "reset": true, "text": st.buf}),
                    });
                }
                Ok(json!({}))
            }
            ClientMsg::StreamStart {
                conv_id,
                client_msg_id,
                reply_to,
                causation_id,
            } => {
                check_client_msg_id(&client_msg_id)?;
                self.stream_start(me, &conv_id, &client_msg_id, reply_to, causation_id)
                    .await
            }
            ClientMsg::StreamAppend { msg_id, idx, text } => {
                self.stream_append(me, &msg_id, idx, &text).await
            }
            ClientMsg::StreamEnd { msg_id } => {
                let st = self
                    .st
                    .streams
                    .get(&msg_id)
                    .ok_or_else(|| rule!("stream.unknown", "not_found", "no active stream"))?;
                if st.sender.id != me.id {
                    return Err(rule!("stream.not_yours", "forbidden", "not your stream"));
                }
                self.finish_stream(&msg_id, "complete", None, &me.id)
                    .await?;
                Ok(json!({"msg_id": msg_id}))
            }
            ClientMsg::StreamCancel { msg_id } => self.stream_cancel(me, &msg_id).await,
            ClientMsg::Whisper {
                conv_id,
                agent_id,
                text,
            } => self.whisper(me, &conv_id, &agent_id, &text).await,
            ClientMsg::Approve { draft_id, text } => self.approve(me, &draft_id, text).await,
            ClientMsg::RequestHuman {
                conv_id,
                kind,
                title,
                options,
                context,
                assignees,
                ttl_secs,
                client_req_id,
            } => {
                self.request_human(
                    me,
                    requests::Ask {
                        conv_id,
                        kind,
                        title,
                        options,
                        context,
                        assignees,
                        ttl_secs,
                        client_req_id,
                    },
                )
                .await
            }
            ClientMsg::Decide {
                request_id,
                decision,
                text,
                choice,
            } => self.decide(me, &request_id, &decision, text, choice).await,
            ClientMsg::CancelRequest { request_id } => self.cancel_request(me, &request_id).await,
            ClientMsg::UpdateTask {
                conv_id,
                state,
                assignee,
                note,
            } => self.update_task(me, &conv_id, state, assignee, note).await,
            ClientMsg::Claim { msg_id } => self.claim(me, &msg_id).await,
            ClientMsg::Progress {
                conv_id,
                msg_id,
                kind,
                text,
                data,
            } => {
                self.progress(me, &conv_id, msg_id, &kind, &text, data)
                    .await
            }
            ClientMsg::Feedback {
                msg_id,
                rating,
                reason,
            } => self.feedback(me, &msg_id, rating, reason).await,
            ClientMsg::Respond {
                msg_id,
                block_id,
                action,
                values,
            } => self.respond(me, &msg_id, &block_id, action, values).await,
            ClientMsg::Handoff { conv_id, to, note } => self.handoff(me, &conv_id, to, &note).await,
            ClientMsg::Handback {
                conv_id,
                agent_id,
                note,
            } => self.handback(me, &conv_id, &agent_id, note).await,
            ClientMsg::PutSummary {
                conv_id,
                from_seq,
                to_seq,
                text,
            } => {
                self.put_summary(me, &conv_id, from_seq, to_seq, &text)
                    .await
            }
            ClientMsg::ReleaseClaim { msg_id } => self.release_claim(me, &msg_id).await,
            ClientMsg::FloorGrant {
                conv_id,
                agent_id,
                ttl_secs,
            } => self.floor_grant(me, &conv_id, &agent_id, ttl_secs).await,
            ClientMsg::FloorRelease { conv_id } => self.floor_release(me, &conv_id).await,
            ClientMsg::Reject { draft_id } => self.reject(me, &draft_id).await,
            ClientMsg::Takeover { conv_id, agent_id } => {
                self.takeover(me, &conv_id, &agent_id, true).await
            }
            ClientMsg::Release { conv_id, agent_id } => {
                self.takeover(me, &conv_id, &agent_id, false).await
            }
            ClientMsg::SetAgentPolicy {
                conv_id,
                agent_id,
                approval,
                trigger,
                topics,
            } => {
                let agent = self.policy_agent(me, &agent_id, &conv_id).await?;
                self.require_member(&conv_id, &agent.id).await?;
                self.check_mode(&conv_id, &agent.id, trigger, topics.as_deref())
                    .await?;
                let ev = self
                    .db
                    .set_agent_policy(
                        &conv_id,
                        &agent.id,
                        approval,
                        trigger,
                        topics.as_deref(),
                        &me.id,
                    )
                    .await?;
                self.st.members.remove(&conv_id);
                self.st.recips.remove(&conv_id);
                self.db
                    .audit(
                        Some(&me.id),
                        "agent.policy",
                        Some(&conv_id),
                        Some(&agent.id),
                        json!({"approval": approval, "trigger": trigger, "topics": topics}),
                    )
                    .await?;
                let seq = ev.seq;
                self.emit(ev).await?;
                Ok(json!({"seq": seq}))
            }
            ClientMsg::SetRole {
                conv_id,
                user_id,
                role,
            } => self.set_role(me, &conv_id, &user_id, &role).await,
            ClientMsg::UpdateConv {
                conv_id,
                title,
                fields,
            } => {
                let mine = self.require_member(&conv_id, &me.id).await?;
                if !is_manager(&mine.role) {
                    return Err(rule!(
                        "conv.update_not_admin",
                        "forbidden",
                        "only the owner or an admin changes the conversation",
                    ));
                }
                if title.is_none() && fields.is_none() {
                    return Err(rule!(
                        "conv.update_nothing",
                        "bad_request",
                        "nothing to change"
                    ));
                }
                if let Some(t) = &title {
                    check_title(t)?;
                }
                if let Some(f) = &fields {
                    check_json_object(f, "fields", MAX_FIELDS_BYTES)?;
                }
                if self.is_dm(&conv_id).await? && title.is_some() {
                    return Err(rule!(
                        "conv.dm_no_title",
                        "bad_request",
                        "a direct conversation has no title"
                    ));
                }
                self.db
                    .update_conv(&conv_id, title.as_deref(), fields.as_ref())
                    .await?;
                let ev = self
                    .db
                    .append_event(
                        &conv_id,
                        "conv.updated",
                        Some(&me.id),
                        None,
                        json!({"title": title, "fields": fields}),
                    )
                    .await?;
                let seq = ev.seq;
                self.emit(ev).await?;
                Ok(json!({"seq": seq}))
            }
            ClientMsg::TransferOwner { conv_id, user_id } => {
                self.transfer_owner(me, &conv_id, &user_id).await
            }
            ClientMsg::ResetGuard { conv_id, agent_id } => {
                let agent = self.policy_agent(me, &agent_id, &conv_id).await?;
                self.require_member(&conv_id, &agent.id).await?;
                if self.st.guards.is_tripped(&agent.id, &conv_id).is_none() {
                    return Err(rule!(
                        "agent.guard_not_tripped",
                        "conflict",
                        "the agent's guard is not tripped"
                    ));
                }
                let ev = self
                    .db
                    .append_event(
                        &conv_id,
                        "agent.state",
                        Some(&me.id),
                        None,
                        json!({"agent_id": agent.id, "state": "active", "reason": "guard_reset"}),
                    )
                    .await?;
                self.db
                    .audit(
                        Some(&me.id),
                        "guard.reset",
                        Some(&conv_id),
                        Some(&agent.id),
                        json!({}),
                    )
                    .await?;
                self.st.guards.reset(&agent.id, &conv_id);
                self.emit(ev).await?;
                Ok(json!({}))
            }
            other => Err(rule!(
                "session.wrong_writer",
                "bad_request",
                format!("{} is not handled by a conversation writer", other.name()),
            )),
        }
    }

    // ------------------------------------------------------------- helpers

    /// A command may only touch a conversation this database currently owns.
    /// A command routed before a migration switched the directory must not
    /// write into the old copy (it is about to be deleted): it is refused and
    /// the hub re-routes it.
    fn own_conv(&self, conv: &str) -> ApiResult<()> {
        match self.sh.dir_get(conv) {
            Some(d) if d.migrating => Err(err(
                "conv_migrating",
                "conversation is being moved; retry shortly",
            )),
            Some(d) if d.db == self.idx => Ok(()),
            Some(_) => Err(err("conv_moved", "conversation moved to another database")),
            None => Err(rule!("conv.unknown", "not_found", "conversation")),
        }
    }

    async fn members(&mut self, conv_id: &str) -> ApiResult<Arc<Vec<Member>>> {
        if let Some(m) = self.st.members.get(conv_id) {
            return Ok(m.clone());
        }
        let m = Arc::new(self.db.members(conv_id).await?);
        self.st.members.insert(conv_id.to_string(), m.clone());
        Ok(m)
    }

    async fn require_member(&mut self, conv_id: &str, user_id: &str) -> ApiResult<Member> {
        if user_id == SYSTEM_ID {
            return match self.db.conv(conv_id).await? {
                Some(_) => Ok(system_member()),
                None => Err(rule!("conv.unknown", "not_found", "conversation")),
            };
        }
        self.members(conv_id)
            .await?
            .iter()
            .find(|m| m.user_id == user_id)
            .cloned()
            .ok_or_else(|| {
                rule!(
                    "member.required",
                    "forbidden",
                    "not a member of this conversation"
                )
            })
    }

    fn owned_agent(&self, me: &User, agent_id: &str) -> ApiResult<User> {
        let a = self
            .sh
            .user(agent_id)
            .ok_or_else(|| rule!("agent.unknown", "not_found", "agent"))?;
        if a.kind != UserKind::Agent || a.owner_id.as_deref() != Some(me.id.as_str()) {
            return Err(rule!(
                "agent.not_owner",
                "forbidden",
                "you do not own this agent"
            ));
        }
        Ok(a)
    }

    fn resolve_user(&self, key: &str) -> ApiResult<User> {
        self.sh
            .user(key)
            .or_else(|| self.sh.user_by_name(key))
            .ok_or_else(|| rule!("account.unknown_user", "not_found", format!("user {key}")))
    }

    /// The caller's agent, to act on it in a conversation: the owner has
    /// these powers only where they are a member too (docs/03-agents.md §1.2).
    async fn owned_agent_in(
        &mut self,
        me: &User,
        agent_id: &str,
        conv_id: &str,
    ) -> ApiResult<User> {
        let a = self.owned_agent(me, agent_id)?;
        self.require_member(conv_id, &me.id).await?;
        Ok(a)
    }

    /// Who may set an agent's policy or reset its guard in a conversation:
    /// its owner there, and for a team agent the conversation's managers.
    async fn policy_agent(&mut self, me: &User, agent_id: &str, conv_id: &str) -> ApiResult<User> {
        match self.owned_agent_in(me, agent_id, conv_id).await {
            Err(e) if e.code == "forbidden" => {
                let manages = self
                    .members(conv_id)
                    .await?
                    .iter()
                    .any(|m| m.user_id == me.id && is_manager(&m.role));
                match self.sh.user(agent_id) {
                    Some(a) if a.team && manages => Ok(a),
                    _ => Err(e),
                }
            }
            r => r,
        }
    }

    /// Who gets an agent's side-channel entries in a conversation: the
    /// agent, and its owner while they are a member.
    async fn audience(&mut self, conv_id: &str, agent: &User) -> ApiResult<Vec<String>> {
        let mut a = vec![agent.id.clone()];
        if let Some(o) = &agent.owner_id
            && self.members(conv_id).await?.iter().any(|m| m.user_id == *o)
        {
            a.push(o.clone());
        }
        Ok(a)
    }

    /// Who sees an agent's drafts in a conversation; somebody must be able
    /// to approve them, so its owner has to be a member there.
    async fn approvers(&mut self, conv_id: &str, agent: &User) -> ApiResult<Vec<String>> {
        let a = self.audience(conv_id, agent).await?;
        if a.len() < 2 {
            return Err(rule!(
                "draft.no_approver",
                "conflict",
                "nobody here can approve this agent's messages: its owner is not a member",
            ));
        }
        Ok(a)
    }

    /// What an agent member may read (bounded by its trigger mode).
    pub async fn agent_can_see(db: &mut Db<'_>, m: &Member, ev: &Event) -> ApiResult<bool> {
        if !m.sees(ev) {
            return Ok(false);
        }
        let Some(msg) = &ev.msg else { return Ok(true) };
        if matches!(
            m.trigger,
            Trigger::All | Trigger::Observe | Trigger::Coordinated
        ) || msg.sender_id == m.user_id
        {
            return Ok(true);
        }
        let to = routing::to_of(msg.envelope.as_ref());
        if routing::wakes(m, &msg.mentions, msg.envelope.as_ref(), None, &to) {
            return Ok(true);
        }
        if let Some(r) = &msg.reply_to
            && let Some(parent) = db.message(r).await?
        {
            return Ok(parent.sender_id == m.user_id);
        }
        Ok(false)
    }

    /// Queues a committed event for fan-out with per-member visibility.
    async fn emit(&mut self, ev: Event) -> ApiResult<()> {
        self.touch(&ev.conv_id);
        let members = self.members(&ev.conv_id).await?;
        let base = match self.st.recips.get(&ev.conv_id) {
            Some((m, r)) if Arc::ptr_eq(m, &members) => r.clone(),
            _ => {
                let r: Arc<Vec<Recip>> = Arc::new(
                    members
                        .iter()
                        .map(|mb| Recip {
                            user_id: mb.user_id.clone(),
                            kind: mb.kind,
                            owner_id: mb.owner_id.clone(),
                            visible: true,
                            trust: None,
                            notify: false,
                            wake: false,
                        })
                        .collect(),
                );
                self.st
                    .recips
                    .insert(ev.conv_id.clone(), (members.clone(), r.clone()));
                r
            }
        };
        // Only agent members differ per event (trigger mode, trust labels).
        let recips = if members.iter().any(|mb| mb.kind == UserKind::Agent) {
            let mut recips = (*base).clone();
            for (mb, r) in members.iter().zip(recips.iter_mut()) {
                if mb.kind != UserKind::Agent {
                    continue;
                }
                r.visible = Self::agent_can_see(&mut self.db, mb, &ev).await?;
                if r.visible
                    && ev.kind == "msg.created"
                    && let Some(msg) = &ev.msg
                    && msg.sender_id != mb.user_id
                {
                    let (t, flags) = self.sh.policy.inbound(
                        &scan_text(&msg.content, msg.payload.as_ref()),
                        msg.sender_type,
                    );
                    for f in &flags {
                        metrics::counter!(m::INBOUND_FLAGS, "flag" => f.clone()).increment(1);
                    }
                    r.trust = Some((t.as_str().to_string(), flags));
                }
            }
            if ev.data.get("wake").is_some() {
                Self::push_wake(&ev, &mut recips);
            }
            Arc::new(recips)
        } else {
            base
        };
        // Events about a message from before a member could see the log
        // (edits, recalls) stay hidden from them.
        let recips = match &ev.msg {
            Some(msg) if members.iter().any(|mb| msg.created_seq < mb.visible_from) => {
                let mut r = (*recips).clone();
                for (mb, x) in members.iter().zip(r.iter_mut()) {
                    if !mb.sees(&ev) {
                        x.visible = false;
                        x.wake = false;
                    }
                }
                Arc::new(r)
            }
            _ => recips,
        };
        // New messages notify members who did not send them and have not
        // muted the conversation (docs/08 §2.6).
        let recips = match (&ev.msg, ev.kind.as_str()) {
            (Some(msg), "msg.created") => {
                let now = now_ms();
                let mut r = (*recips).clone();
                for (mb, x) in members.iter().zip(r.iter_mut()) {
                    x.notify = mb.notified_by(msg, now);
                }
                Arc::new(r)
            }
            // An edit notifies whom it newly mentions (D87).
            (Some(msg), "msg.updated") if ev.data.get("mentions_added").is_some() => {
                let now = now_ms();
                let added = &ev.data["mentions_added"];
                let mut r = (*recips).clone();
                for (mb, x) in members.iter().zip(r.iter_mut()) {
                    let named = added
                        .as_array()
                        .is_some_and(|a| a.iter().any(|u| u == &mb.user_id));
                    x.notify = named && x.visible && mb.notified_by(msg, now);
                }
                Arc::new(r)
            }
            _ => recips,
        };
        metrics::counter!(m::EVENTS_COMMITTED, "type" => ev.kind.clone()).increment(1);
        if ev.kind == "msg.created"
            && let Some(msg) = &ev.msg
        {
            metrics::counter!(m::MESSAGES_COMMITTED, "sender_type" => msg.sender_type.as_str())
                .increment(1);
        }
        let trace = crate::trace::current().map(|t| t.child());
        self.out.push(Out::Event { ev, recips, trace });
        Ok(())
    }

    /// The result of an earlier write with this `client_msg_id`, marked as
    /// a duplicate.
    async fn duplicate(
        &mut self,
        conv_id: &str,
        sender: &str,
        cid: &str,
    ) -> ApiResult<Option<Value>> {
        let Some(mut v) = self.db.dedup_get(conv_id, sender, cid).await? else {
            return Ok(None);
        };
        metrics::counter!(m::DEDUP_HITS).increment(1);
        v["duplicate"] = json!(true);
        Ok(Some(v))
    }

    async fn notify_owner(
        &mut self,
        conv_id: &str,
        agent: &User,
        kind: &str,
        data: Value,
    ) -> ApiResult<()> {
        let audience = self.audience(conv_id, agent).await?;
        let side = self
            .db
            .append_side(conv_id, &audience, kind, Some(&agent.id), data)
            .await?;
        self.out.push(Out::Side(side));
        Ok(())
    }

    /// Members named after an `@`. At each `@` the longest matching name
    /// wins, so `@bobby` mentions bobby and not bob. As in Mattermost, an
    /// `@` counts only at the start or after a character that is not part
    /// of a word (`x@bob.io` is an address), and the name must end there
    /// (`@bobcat` is not bob).
    async fn mentions(&mut self, conv_id: &str, text: &str) -> ApiResult<Vec<String>> {
        let members = self.members(conv_id).await?;
        Ok(mentions_in(
            text,
            members
                .iter()
                .map(|m| (m.user_id.as_str(), m.name.as_str())),
        ))
    }

    /// Causation and hop are computed by the server; agent-supplied values
    /// only pick which message in this conversation is being answered.
    async fn causation(
        &mut self,
        conv_id: &str,
        sender: &User,
        causation_id: Option<String>,
        reply_to: &Option<String>,
    ) -> ApiResult<(Option<String>, i64)> {
        let explicit = causation_id.or_else(|| reply_to.clone());
        let cause = match explicit {
            Some(id) => {
                let msg = self.db.message(&id).await?.ok_or_else(|| {
                    rule!(
                        "msg.causation_unknown",
                        "bad_request",
                        "unknown causation message"
                    )
                })?;
                if msg.conv_id != conv_id {
                    return Err(rule!(
                        "msg.causation_other_conv",
                        "bad_request",
                        "causation message is in another conversation",
                    ));
                }
                Some(msg)
            }
            None => self.db.latest_other_message(conv_id, &sender.id).await?,
        };
        if sender.kind != UserKind::Agent {
            return Ok((cause.map(|m| m.msg_id), 0));
        }
        let hop = cause.as_ref().map(|m| m.hop + 1).unwrap_or(1);
        Ok((cause.map(|m| m.msg_id), hop))
    }

    /// Takeover, breaker, rate, budget and loop checks for an agent message.
    async fn agent_gate(
        &mut self,
        agent: &User,
        conv_id: &str,
        hop: i64,
        text: &str,
    ) -> ApiResult<()> {
        if let Some(by) = self.db.takeover(conv_id, &agent.id).await? {
            return Err(err(
                "taken_over",
                format!("conversation is taken over by {by}"),
            ));
        }
        let recent = self
            .db
            .recent_contents(conv_id, &agent.id, self.st.guards.cfg.dup_window)
            .await?;
        match self.st.guards.check(&agent.id, conv_id, hop, text, &recent) {
            Verdict::Allow => Ok(()),
            Verdict::Reject(code, reason) => {
                metrics::counter!(m::GUARD_REJECTS, "code" => code).increment(1);
                self.db
                    .audit(
                        Some(&agent.id),
                        "guard.reject",
                        Some(conv_id),
                        None,
                        json!({"code": code, "reason": reason}),
                    )
                    .await?;
                self.commit_on_error = true;
                Err(err(code, reason))
            }
            Verdict::Trip(code, reason) => {
                metrics::counter!(m::GUARD_TRIPS, "reason" => code).increment(1);
                let ev = self
                    .db
                    .append_event(conv_id, "agent.state", Some(&agent.id), None, json!({"agent_id": agent.id, "state": "tripped", "reason": code, "detail": reason}))
                    .await?;
                self.db
                    .audit(
                        Some(&agent.id),
                        "guard.trip",
                        Some(conv_id),
                        None,
                        json!({"code": code, "reason": reason, "hop": hop}),
                    )
                    .await?;
                self.emit(ev).await?;
                self.notify_owner(
                    conv_id,
                    agent,
                    "guard.tripped",
                    json!({"agent_id": agent.id, "code": code, "reason": reason}),
                )
                .await?;
                self.commit_on_error = true;
                Err(err(code, reason))
            }
        }
    }

    // ---------------------------------------------------------- membership

    async fn add_member(
        &mut self,
        conv_id: &str,
        me: &User,
        u: &User,
        history: Option<i64>,
        cap: usize,
    ) -> ApiResult<Value> {
        self.own_conv(conv_id)?;
        let cfg = &self.sh.cfg.sync;
        if history.is_some_and(|n| !(0..=cfg.share_max).contains(&n)) {
            return Err(rule!(
                "member.add_history",
                "bad_request",
                format!("history must be 0..{}", cfg.share_max),
            ));
        }
        let mine = self.require_member(conv_id, &me.id).await?;
        let own_agent = u.owner_id.as_deref() == Some(me.id.as_str());
        if !is_manager(&mine.role) && !own_agent {
            return Err(rule!(
                "member.add_not_admin",
                "forbidden",
                "only the owner or an admin can add members",
            ));
        }
        if self.sh.deactivated.read().unwrap().contains(&u.id) {
            return Err(rule!("account.unknown_user", "not_found", "no such user"));
        }
        if self.blocked_now(&u.id, &me.id).await? {
            return Err(rule!(
                "block.refuses_invitations",
                "blocked",
                "this user does not accept invitations from you",
            ));
        }
        let conv = self
            .db
            .conv(conv_id)
            .await?
            .ok_or_else(|| rule!("conv.unknown", "not_found", "conversation"))?;
        if conv.kind == "dm" {
            return Err(rule!(
                "conv.dm_fixed_members",
                "bad_request",
                "a direct conversation has fixed members",
            ));
        }
        let members = self.members(conv_id).await?;
        if members.len() >= cap && !members.iter().any(|m| m.user_id == u.id) {
            return Err(rule!(
                "member.group_full",
                "conflict",
                format!("the group is full ({cap} members)"),
            ));
        }
        // How much of the past the new member sees (docs/08 §2.4): people get
        // the configured default, agents nothing, unless the adder says so.
        let cfg = &self.sh.cfg.sync;
        let n = history.unwrap_or(if u.kind == UserKind::Agent {
            0
        } else {
            cfg.member_messages
        });
        let since = now_ms() - cfg.member_window.as_millis() as i64;
        let from = self
            .db
            .window_start(conv_id, conv.last_seq, n, since)
            .await?;
        // A plain member adding their own agent cannot give it more of the
        // past than they can see themselves (it would let them read it
        // through the agent).
        let from = if is_manager(&mine.role) {
            from
        } else {
            from.max(mine.visible_from)
        };
        let history_from = (from <= conv.last_seq).then_some(from);
        let ev = self
            .db
            .add_member(conv_id, &conv.kind, u, "member", &me.id, history_from)
            .await?;
        self.st.members.remove(conv_id);
        self.st.recips.remove(conv_id);
        if u.kind == UserKind::Agent {
            self.db
                .audit(
                    Some(&me.id),
                    "agent.added",
                    Some(conv_id),
                    Some(&u.id),
                    json!({}),
                )
                .await?;
        }
        if self.idx == self.sh.control {
            self.db.member_index_add(conv_id, &u.id).await?;
            self.db.departure_clear(&u.id, conv_id).await?;
        }
        let seq = ev.seq;
        self.emit(ev).await?;
        Ok(json!({"seq": seq}))
    }

    async fn is_dm(&mut self, conv_id: &str) -> ApiResult<bool> {
        Ok(self.db.conv(conv_id).await?.is_some_and(|c| c.kind == "dm"))
    }

    /// Whether `a` blocked `b`. In a cluster another node may have just
    /// written the block: when this writer's database holds the blocks
    /// (a single database), it reads them in its own transaction.
    async fn blocked_now(&mut self, a: &str, b: &str) -> ApiResult<bool> {
        if self.sh.cluster.get().is_some() && self.idx == self.sh.control {
            return self.db.is_blocked(a, b).await;
        }
        Ok(self.sh.blocked(a, b))
    }

    /// In a direct conversation, a block in either direction stops messages.
    async fn check_dm_block(&mut self, conv_id: &str, me: &str) -> ApiResult<()> {
        let members = self.members(conv_id).await?;
        if members.len() != 2 {
            return Ok(());
        }
        let Some(other) = members.iter().find(|m| m.user_id != me) else {
            return Ok(());
        };
        let other = other.user_id.clone();
        if !(self.blocked_now(&other, me).await? || self.blocked_now(me, &other).await?) {
            return Ok(());
        }
        let dm = self.db.conv(conv_id).await?.is_some_and(|c| c.kind == "dm");
        if dm {
            return Err(rule!(
                "block.blocked",
                "blocked",
                "you cannot message this user"
            ));
        }
        Ok(())
    }

    async fn remove_member(&mut self, conv_id: &str, me: &User, u: &User) -> ApiResult<Value> {
        self.own_conv(conv_id)?;
        let mine = self.require_member(conv_id, &me.id).await?;
        let target = self
            .members(conv_id)
            .await?
            .iter()
            .find(|m| m.user_id == u.id)
            .cloned()
            .ok_or_else(|| {
                rule!(
                    "member.unknown",
                    "not_found",
                    "not a member of this conversation"
                )
            })?;
        if self.is_dm(conv_id).await? {
            return Err(rule!(
                "conv.dm_fixed_members",
                "bad_request",
                "a direct conversation has fixed members; dissolve it instead",
            ));
        }
        let leaving = me.id == u.id;
        if me.kind == UserKind::System && target.role == "owner" {
            return Err(rule!(
                "member.remove_owner",
                "conflict",
                "transfer ownership before removing the owner",
            ));
        }
        if leaving {
            if target.role == "owner" {
                return Err(rule!(
                    "member.owner_leaves",
                    "conflict",
                    "the owner must transfer ownership or dissolve the conversation",
                ));
            }
        } else {
            let own_agent = u.owner_id.as_deref() == Some(me.id.as_str());
            let allowed = match mine.role.as_str() {
                "owner" => true,
                "admin" => target.role == "member",
                _ => false,
            } || (own_agent && target.role != "owner");
            if !allowed {
                return Err(rule!(
                    "member.remove_not_allowed",
                    "forbidden",
                    "you cannot remove this member"
                ));
            }
        }
        // The person holds their agents' keys: their assistants leave with
        // them, or they could go on reading and speaking through them. Team
        // agents stay (docs/03-agents.md §1.2); a takeover by the person ends first,
        // since they can no longer speak here.
        let theirs: Vec<User> = if u.kind == UserKind::Human {
            self.members(conv_id)
                .await?
                .iter()
                .filter_map(|m| self.sh.user(&m.user_id))
                .filter(|a| {
                    a.kind == UserKind::Agent && a.owner_id.as_deref() == Some(u.id.as_str())
                })
                .collect()
        } else {
            vec![]
        };
        for a in theirs.iter().filter(|a| a.team) {
            if self.db.takeover(conv_id, &a.id).await?.is_some() {
                self.takeover(u, conv_id, &a.id, false).await?;
            }
        }
        let seq = self.detach(conv_id, me, u, leaving, None).await?;
        let mut agents = vec![];
        {
            for agent in theirs.into_iter().filter(|a| !a.team) {
                let s = self
                    .detach(conv_id, me, &agent, false, Some("owner_left"))
                    .await?;
                agents.push(json!({"user_id": agent.id, "seq": s}));
            }
        }
        Ok(json!({"seq": seq, "agents": agents}))
    }

    /// Takes a member out of a conversation (permissions already checked).
    async fn detach(
        &mut self,
        conv_id: &str,
        me: &User,
        u: &User,
        leaving: bool,
        reason: Option<&str>,
    ) -> ApiResult<i64> {
        if u.kind == UserKind::Agent {
            // Its live streams end visibly before it goes; drafts are dropped.
            let ids: Vec<String> = self
                .st
                .streams
                .values()
                .filter(|s| s.sender.id == u.id && s.conv_id == conv_id)
                .map(|s| s.msg_id.clone())
                .collect();
            for id in ids {
                self.finish_stream(&id, "cancelled", Some("removed"), &me.id)
                    .await?;
            }
            self.db.discard_drafts(conv_id, &u.id).await?;
            self.cancel_requests_of(conv_id, u).await?;
        }
        let kind = if leaving {
            "member.left"
        } else {
            "member.removed"
        };
        let ev = self
            .db
            .append_event(
                conv_id,
                kind,
                Some(&me.id),
                None,
                json!({"user_id": u.id, "name": u.name, "kind": u.kind, "by": me.id, "reason": reason}),
            )
            .await?;
        let seq = ev.seq;
        // The member still receives this event; nothing after it.
        self.emit(ev).await?;
        if let Some(m) = self.db.member(conv_id, &u.id).await? {
            self.db
                .remember_past(conv_id, &u.id, m.visible_from, seq)
                .await?;
        }
        self.db.remove_member(conv_id, &u.id).await?;
        self.st.members.remove(conv_id);
        self.st.recips.remove(conv_id);
        self.out.push(Out::Unfocus {
            conv_id: conv_id.to_string(),
            user_id: Some(u.id.clone()),
        });
        if self.idx == self.sh.control {
            self.db.member_index_remove(conv_id, &u.id).await?;
            // Their devices still sync up to this event (docs/08 §2.3).
            self.db.departure_put(&u.id, conv_id, seq, false).await?;
        }
        self.db
            .audit(Some(&me.id), kind, Some(conv_id), Some(&u.id), json!({}))
            .await?;
        Ok(seq)
    }

    async fn set_role(
        &mut self,
        me: &User,
        conv_id: &str,
        user_id: &str,
        role: &str,
    ) -> ApiResult<Value> {
        if role != "admin" && role != "member" {
            return Err(rule!(
                "member.role_value",
                "bad_request",
                "role must be admin or member"
            ));
        }
        if self.is_dm(conv_id).await? {
            return Err(rule!(
                "conv.dm_no_roles",
                "bad_request",
                "a direct conversation has no roles"
            ));
        }
        let mine = self.require_member(conv_id, &me.id).await?;
        if mine.role != "owner" {
            return Err(rule!(
                "member.role_not_owner",
                "forbidden",
                "only the owner changes roles"
            ));
        }
        let u = self.resolve_user(user_id)?;
        let target = self.require_member(conv_id, &u.id).await.map_err(|_| {
            rule!(
                "member.unknown",
                "not_found",
                "not a member of this conversation"
            )
        })?;
        if target.role == "owner" {
            return Err(rule!(
                "member.role_use_transfer",
                "conflict",
                "use transfer_owner to change the owner"
            ));
        }
        if target.kind != UserKind::Human {
            return Err(rule!(
                "member.admin_is_person",
                "bad_request",
                "only people can be admins"
            ));
        }
        self.db.set_role(conv_id, &u.id, role).await?;
        let ev = self
            .db
            .append_event(
                conv_id,
                "member.role",
                Some(&me.id),
                None,
                json!({"user_id": u.id, "role": role}),
            )
            .await?;
        self.st.members.remove(conv_id);
        self.st.recips.remove(conv_id);
        let seq = ev.seq;
        self.emit(ev).await?;
        Ok(json!({"seq": seq}))
    }

    /// Silences a member (`until` in ms) or lifts it (None). Repeating the
    /// same state writes nothing.
    async fn silence_member(
        &mut self,
        conv_id: &str,
        user_id: &str,
        until: Option<i64>,
        by: &str,
        reason: Option<&str>,
    ) -> ApiResult<Value> {
        self.own_conv(conv_id)?;
        if self.db.conv(conv_id).await?.is_none() {
            return Err(rule!("conv.unknown", "not_found", "conversation"));
        }
        let target = self.require_member(conv_id, user_id).await.map_err(|_| {
            rule!(
                "member.unknown",
                "not_found",
                "not a member of this conversation"
            )
        })?;
        let now = now_ms();
        let was = target.silenced_until.filter(|u| *u > now);
        if was.is_none() && until.is_none() {
            return Ok(json!({"silenced_until": null, "changed": false}));
        }
        if was.is_some() && was == until {
            return Ok(json!({"silenced_until": until, "changed": false}));
        }
        self.db.set_silenced(conv_id, user_id, until).await?;
        let (kind, data) = match until {
            Some(u) => (
                "member.silenced",
                json!({"user_id": user_id, "until": u, "reason": reason}),
            ),
            None => ("member.unsilenced", json!({"user_id": user_id})),
        };
        let ev = self
            .db
            .append_event(conv_id, kind, Some(by), None, data)
            .await?;
        self.db
            .audit(
                Some(by),
                kind,
                Some(conv_id),
                Some(user_id),
                json!({"until": until, "reason": reason}),
            )
            .await?;
        self.st.members.remove(conv_id);
        self.st.recips.remove(conv_id);
        let seq = ev.seq;
        self.emit(ev).await?;
        Ok(json!({"silenced_until": until, "changed": true, "seq": seq}))
    }

    /// A person's own message is edited or recalled only within the time
    /// the app allows (docs/13 §4.1); moderators and owners acting on
    /// others' messages are not limited.
    fn within_window(
        &self,
        me: &User,
        msg: &MsgView,
        minutes: Option<i64>,
        what: &str,
    ) -> ApiResult<()> {
        if me.kind != UserKind::Human || msg.sender_id != me.id {
            return Ok(());
        }
        match minutes {
            Some(m) if now_ms() - msg.created_at > m * 60_000 => Err(rule!(
                "msg.app_window",
                "forbidden",
                format!("the app does not allow this: messages are {what} within {m} minutes"),
            )),
            _ => Ok(()),
        }
    }

    async fn transfer_owner(
        &mut self,
        me: &User,
        conv_id: &str,
        user_id: &str,
    ) -> ApiResult<Value> {
        let mine = self.require_member(conv_id, &me.id).await?;
        if mine.role != "owner" {
            return Err(rule!(
                "conv.transfer_not_owner",
                "forbidden",
                "only the owner transfers ownership"
            ));
        }
        if self.is_dm(conv_id).await? {
            return Err(rule!(
                "conv.dm_no_owner",
                "bad_request",
                "a direct conversation has no owner to transfer",
            ));
        }
        let u = self.resolve_user(user_id)?;
        let target = self.require_member(conv_id, &u.id).await.map_err(|_| {
            rule!(
                "member.unknown",
                "not_found",
                "not a member of this conversation"
            )
        })?;
        if target.user_id == me.id {
            return Err(rule!(
                "conv.transfer_self",
                "conflict",
                "you are already the owner"
            ));
        }
        if target.kind != UserKind::Human {
            return Err(rule!(
                "conv.owner_is_person",
                "bad_request",
                "only a person can own a conversation"
            ));
        }
        self.db.set_role(conv_id, &u.id, "owner").await?;
        // The system is not a member: the previous owner becomes an admin.
        let previous = if me.kind == UserKind::System {
            self.members(conv_id)
                .await?
                .iter()
                .find(|m| m.role == "owner")
                .map(|m| m.user_id.clone())
        } else {
            Some(me.id.clone())
        };
        if let Some(p) = previous {
            self.db.set_role(conv_id, &p, "admin").await?;
        }
        let ev = self
            .db
            .append_event(
                conv_id,
                "owner.transferred",
                Some(&me.id),
                None,
                json!({"from": me.id, "to": u.id}),
            )
            .await?;
        self.db
            .audit(
                Some(&me.id),
                "owner.transferred",
                Some(conv_id),
                Some(&u.id),
                json!({}),
            )
            .await?;
        self.st.members.remove(conv_id);
        self.st.recips.remove(conv_id);
        let seq = ev.seq;
        self.emit(ev).await?;
        Ok(json!({"seq": seq}))
    }

    /// Every member loses access and the content is deleted; only the audit
    /// record that the conversation existed remains.
    async fn dissolve(&mut self, conv_id: &str, me: &User) -> ApiResult<Value> {
        self.own_conv(conv_id)?;
        let mine = self.require_member(conv_id, &me.id).await?;
        // A direct conversation can be deleted by either person in it.
        if mine.role != "owner" && !(self.is_dm(conv_id).await? && me.kind == UserKind::Human) {
            return Err(rule!(
                "conv.dissolve_not_owner",
                "forbidden",
                "only the owner dissolves a conversation"
            ));
        }
        let members: Vec<String> = self
            .members(conv_id)
            .await?
            .iter()
            .map(|m| m.user_id.clone())
            .collect();
        let streams: Vec<(String, String)> = self
            .st
            .streams
            .values()
            .filter(|s| s.conv_id == conv_id)
            .map(|s| (s.msg_id.clone(), s.sender.id.clone()))
            .collect();
        for (id, sender) in streams {
            self.st.streams.remove(&id);
            self.stream_removed(&id);
            self.out.push(Out::ToUser {
                user_id: sender,
                frame: json!({"type": "stream_cancelled", "msg_id": id, "reason": "dissolved"}),
            });
        }
        let ev = self
            .db
            .append_event(
                conv_id,
                "conv.dissolved",
                Some(&me.id),
                None,
                json!({"by": me.id}),
            )
            .await?;
        self.emit(ev).await?;
        // The conversation's data keys go too (crypto-shredding), and its
        // files (deleted from the store in the background).
        self.trash_conv(conv_id).await?;
        self.db.shred_scope(conv_id).await?;
        self.db.delete_conv_content(conv_id).await?;
        self.db.forget_outbox(conv_id, None).await?;
        if self.idx == self.sh.control {
            self.db.forget_conv(conv_id).await?;
            // Members' devices learn it is gone at their next sync.
            for u in &members {
                self.db.departure_put(u, conv_id, 0, true).await?;
            }
        }
        self.db
            .audit(
                Some(&me.id),
                "conv.dissolved",
                Some(conv_id),
                None,
                json!({"members": members.len()}),
            )
            .await?;
        self.st.members.remove(conv_id);
        self.st.recips.remove(conv_id);
        self.out.push(Out::Unfocus {
            conv_id: conv_id.to_string(),
            user_id: None,
        });
        // Later commands for it are refused here; the hub drops it everywhere.
        self.sh.dir_set(conv_id, None);
        Ok(json!({"members": members}))
    }

    // ---------------------------------------------------------------- send

    #[allow(clippy::too_many_arguments)]
    async fn send(
        &mut self,
        me: &User,
        conv_id: &str,
        client_msg_id: &str,
        text: &str,
        (kind, payload): (Option<String>, Option<Value>),
        reply_to: Option<String>,
        causation_id: Option<String>,
        on_behalf_of: Option<String>,
        to_user: Option<String>,
        envelope: routing::EnvelopeIn,
    ) -> ApiResult<Value> {
        // A retry of a committed write gets its result back before any other
        // check: membership, guards or budgets may have changed since (M1).
        if let Some(prev) = self.duplicate(conv_id, &me.id, client_msg_id).await? {
            return Ok(prev);
        }
        // Deactivated or deleted: checked here too, in the writer, so a send
        // that passed the session's check cannot commit after an erasure.
        if self.sh.deactivated.read().unwrap().contains(&me.id)
            && self
                .sh
                .failpoints
                .hit(crate::hub::DELETED_CAN_SEND)
                .await
                .is_ok()
        {
            return Err(rule!(
                "account.deactivated",
                "forbidden",
                "the account is deactivated"
            ));
        }
        let member = self.require_member(conv_id, &me.id).await?;
        not_silenced(&member)?;
        check_text(text)?;
        let kind = check_kind(kind, &payload)?;
        let envelope = self.envelope(conv_id, envelope).await?;
        if envelope.is_some() && to_user.is_some() {
            return Err(rule!(
                "msg.envelope_not_directed",
                "bad_request",
                "an envelope is for messages in the conversation, not to_user",
            ));
        }
        if let Some(o) = &on_behalf_of {
            // Community rule: an agent may only speak on behalf of its owner.
            if me.kind != UserKind::Agent || me.owner_id.as_deref() != Some(o.as_str()) {
                return Err(rule!(
                    "msg.on_behalf_not_owner",
                    "forbidden",
                    "on_behalf_of must be the agent's owner"
                ));
            }
        }
        let (causation_id, hop) = self.causation(conv_id, me, causation_id, &reply_to).await?;
        let root = self.root_of(me, causation_id.as_deref()).await?;
        if me.kind == UserKind::Agent {
            self.agent_may_speak(me, conv_id, &member, reply_to.as_deref())
                .await?;
            self.root_gate(me, conv_id, root.as_deref()).await?;
            self.agent_gate(me, conv_id, hop, text).await?;
            if let Some((hook, reason)) =
                self.sh.policy.outbound(&scan_text(text, payload.as_ref()))
            {
                metrics::counter!(m::POLICY_BLOCKS, "hook" => hook.clone()).increment(1);
                self.db
                    .audit(
                        Some(&me.id),
                        "policy.block",
                        Some(conv_id),
                        None,
                        json!({"hook": hook, "reason": reason}),
                    )
                    .await?;
                self.notify_owner(
                    conv_id,
                    me,
                    "policy.blocked",
                    json!({"agent_id": me.id, "hook": hook, "reason": reason}),
                )
                .await?;
                self.commit_on_error = true;
                return Err(rule!("msg.policy", "policy_blocked", reason));
            }
        }

        self.check_dm_block(conv_id, &me.id).await?;
        let has_file = files::KINDS.contains(&kind.as_str());
        if has_file && to_user.is_some() {
            return Err(rule!(
                "msg.files_not_directed",
                "bad_request",
                "files go to the conversation, not to_user",
            ));
        }
        // Targeted visibility goes to the side channel and takes no conv_seq.
        if let Some(target) = to_user {
            let t = self.resolve_user(&target)?;
            self.require_member(conv_id, &t.id).await?;
            if self.blocked_now(&t.id, &me.id).await? || self.blocked_now(&me.id, &t.id).await? {
                return Err(rule!(
                    "block.blocked",
                    "blocked",
                    "you cannot message this user"
                ));
            }
            let mut audience = self.audience(conv_id, me).await?;
            if !audience.contains(&t.id) {
                audience.push(t.id.clone());
            }
            let side = self
                .db
                .append_side(
                    conv_id,
                    &audience,
                    "ephemeral",
                    Some(&me.id),
                    json!({"text": text, "kind": kind, "payload": payload, "from": me.id, "from_name": me.name, "from_type": me.kind, "to": t.id, "hop": hop}),
                )
                .await?;
            let side_seq = side.side_seq;
            self.out.push(Out::Side(side));
            let res = json!({"side_seq": side_seq});
            self.db
                .dedup_put(conv_id, &me.id, client_msg_id, &res)
                .await?;
            return Ok(res);
        }

        if me.kind == UserKind::Agent && member.approval {
            let audience = self.approvers(conv_id, me).await?;
            let draft_id = format!("d_{}", crate::db::new_id());
            let payload = match &payload {
                Some(p) if has_file => Some(self.attach(conv_id, me, &kind, p, &draft_id).await?),
                _ => payload,
            };
            let (d, side) = self
                .db
                .create_draft(
                    &draft_id,
                    conv_id,
                    me,
                    text,
                    hop,
                    causation_id,
                    reply_to,
                    &audience,
                    &kind,
                    payload,
                    envelope,
                )
                .await?;
            self.db
                .audit(
                    Some(&me.id),
                    "draft.created",
                    Some(conv_id),
                    Some(&d.draft_id),
                    json!({}),
                )
                .await?;
            metrics::counter!(m::DRAFTS, "state" => "pending").increment(1);
            self.out.push(Out::Side(side));
            let res = json!({"draft_id": d.draft_id, "state": "pending_approval"});
            self.db
                .dedup_put(conv_id, &me.id, client_msg_id, &res)
                .await?;
            return Ok(res);
        }

        let mentions = self.mentions(conv_id, text).await?;
        let wake = self
            .wake_set(
                conv_id,
                &me.id,
                reply_to.as_deref(),
                &mentions,
                envelope.as_ref(),
            )
            .await?;
        // The file is taken under a placeholder, then by the message's id.
        let holding = format!("p_{}", crate::db::new_id());
        let file_id = payload
            .as_ref()
            .filter(|_| has_file)
            .and_then(|p| p["file_id"].as_str())
            .map(String::from);
        let payload = match &payload {
            Some(p) if has_file => Some(self.attach(conv_id, me, &kind, p, &holding).await?),
            _ => payload,
        };
        let res = self
            .db
            .commit_message(NewMsg {
                conv_id,
                sender: me,
                client_msg_id: Some(client_msg_id),
                on_behalf_of,
                kind: &kind,
                content: text.to_string(),
                payload,
                state: "committed",
                hop,
                causation_id,
                reply_to,
                mentions,
                approval: None,
                envelope,
                root_id: root,
                wake: Some(wake),
            })
            .await?;
        if let Some(fid) = &file_id {
            match &res {
                Committed::New(ev) => {
                    let mid = &ev.msg.as_ref().unwrap().msg_id;
                    self.db.file_take(fid, mid, Some(&holding)).await?;
                }
                Committed::Duplicate { .. } => self.db.file_release(fid, &holding).await?,
            }
        }
        match res {
            Committed::Duplicate { seq, msg_id } => {
                metrics::counter!(m::DEDUP_HITS).increment(1);
                Ok(json!({"seq": seq, "msg_id": msg_id, "duplicate": true}))
            }
            Committed::New(ev) => {
                let msg = ev.msg.as_ref().unwrap();
                self.charge(msg).await?;
                let v = json!({"seq": ev.seq, "msg_id": msg.msg_id, "hop": msg.hop, "root_id": msg.root_id, "wake": ev.data["wake"]});
                let seq = ev.seq;
                self.emit(ev).await?;
                // Writing means having read up to here (D93).
                if me.kind == UserKind::Human {
                    self.db.set_read(conv_id, &me.id, seq).await?;
                    self.out.push(Out::ToUser {
                        user_id: me.id.clone(),
                        frame: json!({"type": "read", "conv_id": conv_id, "seq": seq}),
                    });
                }
                Ok(v)
            }
        }
    }

    /// May `me` modify this message? The sender, the sending agent's owner,
    /// or (recall only) the conversation owner.
    /// May `me` modify this message? The sender while still a member, the
    /// owner of the sending agent while the agent is still a member, or
    /// (recall only) an owner or admin of the conversation.
    async fn can_modify(&mut self, me: &User, msg: &MsgView, recall: bool) -> ApiResult<bool> {
        if me.kind == UserKind::System {
            // Moderation: the system recalls anything, and edits its own.
            return Ok(recall || msg.sender_id == me.id);
        }
        let members = self.members(&msg.conv_id).await?;
        let member = |id: &str| members.iter().find(|m| m.user_id == id);
        let sender_present = member(&msg.sender_id).is_some();
        if msg.sender_id == me.id {
            return Ok(sender_present);
        }
        let owns_sender = self
            .sh
            .user(&msg.sender_id)
            .and_then(|s| s.owner_id)
            .as_deref()
            == Some(me.id.as_str());
        if owns_sender && sender_present {
            return Ok(true);
        }
        Ok(recall && member(&me.id).is_some_and(|m| is_manager(&m.role)))
    }

    async fn edit(
        &mut self,
        me: &User,
        msg_id: &str,
        text: &str,
        payload: Option<Value>,
        base_version: Option<i64>,
    ) -> ApiResult<Value> {
        let msg = self
            .db
            .message(msg_id)
            .await?
            .ok_or_else(|| err("conv_moved", "message not in this database"))?;
        self.own_conv(&msg.conv_id)?;
        if !self.can_modify(me, &msg, false).await? {
            return Err(rule!(
                "msg.edit_not_allowed",
                "forbidden",
                "cannot edit this message"
            ));
        }
        if let Ok(m) = self.require_member(&msg.conv_id, &me.id).await {
            not_silenced(&m)?;
        }
        self.within_window(me, &msg, self.sh.allowed(me).edit_minutes, "edited")?;
        if msg.state == "streaming" || msg.state == "recalled" {
            return Err(rule!(
                "msg.edit_state",
                "conflict",
                format!("message is {}", msg.state)
            ));
        }
        if base_version.is_some_and(|v| v != msg.version) {
            return Err(rule!(
                "msg.edit_version",
                "conflict",
                format!("the message changed since (now version {})", msg.version),
            ));
        }
        check_text(text)?;
        // A file stays: only its caption changes.
        let payload = match payload {
            Some(p) if files::KINDS.contains(&msg.kind.as_str()) => {
                files::check_payload(&msg.kind, &Some(p.clone()))?;
                let old = msg.payload.clone().unwrap_or_default();
                if p["file_id"] != old["file_id"] {
                    return Err(rule!(
                        "msg.edit_file",
                        "bad_request",
                        "the file of a message cannot be changed; send a new one",
                    ));
                }
                let mut new = old;
                match p.get("caption") {
                    Some(c) => new["caption"] = c.clone(),
                    None => {
                        if let Some(o) = new.as_object_mut() {
                            o.remove("caption");
                        }
                    }
                }
                Some(new)
            }
            other => other,
        };
        if let Some(p) = &payload {
            if msg.kind == "text" {
                return Err(rule!(
                    "msg.edit_text_payload",
                    "bad_request",
                    "text messages have no payload"
                ));
            }
            check_json_object(p, "payload", MAX_PAYLOAD_BYTES)?;
            if msg.kind == cards::KIND {
                cards::check_card(p)?;
            }
        }
        let scanned = payload.as_ref().or(msg.payload.as_ref());
        if me.kind == UserKind::Agent
            && let Some((_, reason)) = self.sh.policy.outbound(&scan_text(text, scanned))
        {
            return Err(rule!("msg.policy", "policy_blocked", reason));
        }
        if payload.is_some() {
            self.db.set_payload(msg_id, payload.as_ref()).await?;
        }
        // Mentions follow the new text (D87): new ones notify and wake,
        // removed ones stop counting.
        let mentions = self.mentions(&msg.conv_id, text).await?;
        let added: Vec<String> = mentions
            .iter()
            .filter(|m| !msg.mentions.contains(m) && **m != msg.sender_id)
            .cloned()
            .collect();
        self.db.set_mentions(msg_id, &mentions).await?;
        let wake: Vec<String> = {
            let members = self.members(&msg.conv_id).await?;
            added
                .iter()
                .filter(|u| {
                    members
                        .iter()
                        .any(|m| m.user_id == **u && m.kind == UserKind::Agent)
                })
                .cloned()
                .collect()
        };
        let mut data = json!({});
        if !added.is_empty() {
            data["mentions_added"] = json!(added);
        }
        if !wake.is_empty() {
            data["wake"] = json!(wake);
        }
        let ev = self
            .db
            .update_message_with(msg_id, "msg.updated", &me.id, Some(text), None, None, data)
            .await?;
        if msg.sender_id != me.id {
            self.db
                .audit(
                    Some(&me.id),
                    "msg.revised",
                    Some(&msg.conv_id),
                    Some(msg_id),
                    json!({}),
                )
                .await?;
        }
        let seq = ev.seq;
        self.emit(ev).await?;
        Ok(json!({"seq": seq}))
    }

    async fn recall(&mut self, me: &User, msg_id: &str) -> ApiResult<Value> {
        let msg = self
            .db
            .message(msg_id)
            .await?
            .ok_or_else(|| err("conv_moved", "message not in this database"))?;
        self.own_conv(&msg.conv_id)?;
        if !self.can_modify(me, &msg, true).await? {
            return Err(rule!(
                "msg.recall_not_allowed",
                "forbidden",
                "cannot recall this message"
            ));
        }
        self.within_window(me, &msg, self.sh.allowed(me).recall_minutes, "recalled")?;
        if msg.state == "recalled" {
            return Err(rule!(
                "msg.recall_done",
                "conflict",
                "message is already recalled"
            ));
        }
        if msg.state == "streaming" {
            self.finish_stream(msg_id, "cancelled", Some("recalled"), &me.id)
                .await?;
        }
        self.db.set_payload(msg_id, None).await?;
        // Its file goes too: no address works from now on.
        self.trash_ref(msg_id).await?;
        let ev = self
            .db
            .update_message(
                msg_id,
                "msg.recalled",
                &me.id,
                Some(""),
                Some("recalled"),
                None,
            )
            .await?;
        // Earlier events embed the content; recalled means gone from history.
        self.db
            .scrub_message(&msg.conv_id, msg_id, msg.created_seq)
            .await?;
        // So are the copies: the approved draft and webhook bodies not yet
        // delivered (D91).
        let created = self
            .db
            .pull(&msg.conv_id, msg.created_seq - 1, 1)
            .await?
            .into_iter()
            .next();
        if let Some(d) = created
            .as_ref()
            .and_then(|e| e.data["approval"]["draft_id"].as_str())
        {
            self.db.scrub_draft(&msg.conv_id, d).await?;
        }
        self.db.forget_outbox(&msg.conv_id, Some(msg_id)).await?;
        if msg.sender_id != me.id {
            self.db
                .audit(
                    Some(&me.id),
                    "msg.recalled",
                    Some(&msg.conv_id),
                    Some(msg_id),
                    json!({}),
                )
                .await?;
        }
        let seq = ev.seq;
        self.emit(ev).await?;
        Ok(json!({"seq": seq}))
    }

    // ----------------------------------------------------------- streaming

    async fn stream_start(
        &mut self,
        me: &User,
        conv_id: &str,
        client_msg_id: &str,
        reply_to: Option<String>,
        causation_id: Option<String>,
    ) -> ApiResult<Value> {
        if me.kind != UserKind::Agent {
            return Err(rule!(
                "stream.agents_only",
                "forbidden",
                "only agents stream"
            ));
        }
        if let Some(prev) = self.duplicate(conv_id, &me.id, client_msg_id).await? {
            return Ok(prev);
        }
        let member = self.require_member(conv_id, &me.id).await?;
        not_silenced(&member)?;
        let active = self
            .st
            .streams
            .values()
            .filter(|s| s.sender.id == me.id && s.conv_id == conv_id)
            .count();
        if active >= 2 {
            return Err(rule!(
                "stream.concurrent",
                "rate_limited",
                "too many concurrent streams in this conversation",
            ));
        }
        let (causation_id, hop) = self.causation(conv_id, me, causation_id, &reply_to).await?;
        let root = self.root_of(me, causation_id.as_deref()).await?;
        self.agent_may_speak(me, conv_id, &member, reply_to.as_deref())
            .await?;
        self.root_gate(me, conv_id, root.as_deref()).await?;
        self.agent_gate(me, conv_id, hop, "").await?;
        let now = Instant::now();
        let (msg_id, target, seq) = if member.approval {
            let id = format!("d_{}", crate::db::new_id());
            let audience = self.approvers(conv_id, me).await?;
            self.db
                .dedup_put(
                    conv_id,
                    &me.id,
                    client_msg_id,
                    &json!({"msg_id": id, "seq": null, "pending_approval": true, "hop": hop}),
                )
                .await?;
            (id, Target::Draft { audience }, None)
        } else {
            let res = self
                .db
                .commit_message(NewMsg {
                    conv_id,
                    sender: me,
                    client_msg_id: Some(client_msg_id),
                    on_behalf_of: None,
                    kind: "text",
                    content: String::new(),
                    payload: None,
                    state: "streaming",
                    hop,
                    causation_id: causation_id.clone(),
                    reply_to: reply_to.clone(),
                    mentions: vec![],
                    approval: None,
                    envelope: None,
                    root_id: root,
                    // Agents act on a stream when it completes (D43).
                    wake: Some((vec![], 0)),
                })
                .await?;
            match res {
                Committed::Duplicate { seq, msg_id } => {
                    return Ok(json!({"msg_id": msg_id, "seq": seq, "duplicate": true}));
                }
                Committed::New(ev) => {
                    self.charge(ev.msg.as_ref().unwrap()).await?;
                    let id = ev.msg.as_ref().unwrap().msg_id.clone();
                    let seq = ev.seq;
                    self.emit(ev).await?;
                    (id, Target::Main, Some(seq))
                }
            }
        };
        let pending_approval = matches!(target, Target::Draft { .. });
        self.st.streams.insert(
            msg_id.clone(),
            Stream {
                msg_id: msg_id.clone(),
                conv_id: conv_id.to_string(),
                sender: me.clone(),
                target,
                buf: String::new(),
                pending: String::new(),
                next_idx: 0,
                started: now,
                last_append: now,
                checkpointed: 0,
                hop,
                causation_id,
                reply_to,
            },
        );
        self.sh
            .stream_index
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(msg_id.clone(), (self.idx, conv_id.to_string()));
        self.out.push(Out::Stream {
            msg_id: msg_id.clone(),
            db: self.idx,
            conv: Some(conv_id.to_string()),
        });
        metrics::gauge!(m::STREAMS_ACTIVE).increment(1.0);
        Ok(json!({"msg_id": msg_id, "seq": seq, "pending_approval": pending_approval, "hop": hop}))
    }

    async fn stream_append(
        &mut self,
        me: &User,
        msg_id: &str,
        idx: u64,
        text: &str,
    ) -> ApiResult<Value> {
        let max_bytes = self.sh.cfg.stream_max_bytes;
        let st = self
            .st
            .streams
            .get_mut(msg_id)
            .ok_or_else(|| rule!("stream.unknown", "not_found", "no active stream"))?;
        if st.sender.id != me.id {
            return Err(rule!("stream.not_yours", "forbidden", "not your stream"));
        }
        if idx < st.next_idx {
            return Ok(json!({"duplicate": true}));
        }
        if text.contains('\0') {
            return Err(rule!(
                "msg.text_nul",
                "bad_request",
                "text must not contain NUL"
            ));
        }
        if idx > st.next_idx {
            return Err(err(
                "out_of_order",
                format!("expected chunk {}", st.next_idx),
            ));
        }
        if st.buf.len() + text.len() > max_bytes {
            // The chunk that would pass the limit is not kept: a stored
            // message never exceeds it.
            self.finish_stream(msg_id, "complete", Some("size_limit"), &me.id)
                .await?;
            self.commit_on_error = true;
            return Err(err(
                "size_limit",
                "stream reached the maximum size and was closed",
            ));
        }
        st.next_idx += 1;
        st.last_append = Instant::now();
        st.buf.push_str(text);
        st.pending.push_str(text);
        let buf = st.buf.clone();
        let conv_id = st.conv_id.clone();
        if !self
            .st
            .guards
            .charge_chars(&me.id, text.chars().count() as u64)
        {
            self.finish_stream(msg_id, "cancelled", Some("budget_exceeded"), &me.id)
                .await?;
            self.commit_on_error = true;
            return Err(rule!(
                "agent.daily_budget",
                "budget_exceeded",
                "daily agent budget exhausted"
            ));
        }
        // Streaming filter: check the accumulated text chunk by chunk.
        if let Some((hook, reason)) = self.sh.policy.outbound(&buf) {
            metrics::counter!(m::POLICY_BLOCKS, "hook" => hook.clone()).increment(1);
            self.db
                .audit(
                    Some(&me.id),
                    "policy.block",
                    Some(&conv_id),
                    Some(msg_id),
                    json!({"hook": hook, "reason": reason, "streaming": true}),
                )
                .await?;
            self.moderate_stream(msg_id, &reason).await?;
            self.commit_on_error = true;
            return Err(rule!("msg.policy", "policy_blocked", reason));
        }
        Ok(json!({}))
    }

    async fn stream_cancel(&mut self, me: &User, msg_id: &str) -> ApiResult<Value> {
        let st = self
            .st
            .streams
            .get(msg_id)
            .ok_or_else(|| rule!("stream.unknown", "not_found", "no active stream"))?;
        let sender = st.sender.clone();
        let conv_id = st.conv_id.clone();
        let is_owner = sender.owner_id.as_deref() == Some(me.id.as_str());
        let human_member =
            me.kind == UserKind::Human && self.require_member(&conv_id, &me.id).await.is_ok();
        if sender.id != me.id && !is_owner && !human_member {
            return Err(rule!(
                "stream.cancel_not_allowed",
                "forbidden",
                "cannot interrupt this stream"
            ));
        }
        let reason = format!("cancelled_by:{}", me.name);
        self.finish_stream(msg_id, "cancelled", Some(&reason), &me.id)
            .await?;
        if sender.id != me.id {
            self.db
                .audit(
                    Some(&me.id),
                    "stream.interrupt",
                    Some(&conv_id),
                    Some(msg_id),
                    json!({"agent_id": sender.id}),
                )
                .await?;
        }
        Ok(json!({"msg_id": msg_id}))
    }

    fn stream_removed(&mut self, msg_id: &str) {
        self.sh
            .stream_index
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(msg_id);
        self.out.push(Out::Stream {
            msg_id: msg_id.to_string(),
            db: self.idx,
            conv: None,
        });
        metrics::gauge!(m::STREAMS_ACTIVE).decrement(1.0);
    }

    /// Policy hit mid-stream: stop the stream and replace what was already
    /// pushed with a placeholder (a new version of the same message).
    async fn moderate_stream(&mut self, msg_id: &str, reason: &str) -> ApiResult<()> {
        let (conv_id, sender, target) = match self.st.streams.get(msg_id) {
            Some(s) => (s.conv_id.clone(), s.sender.clone(), s.target.clone()),
            None => return Ok(()),
        };
        let r = format!("policy: {reason}");
        match &target {
            Target::Main => {
                let ev = self
                    .db
                    .update_message(
                        msg_id,
                        "msg.updated",
                        &sender.id,
                        Some(MODERATED_PLACEHOLDER),
                        Some("cancelled"),
                        Some(&r),
                    )
                    .await?;
                self.emit(ev).await?;
            }
            Target::Draft { audience } => {
                let side = self
                    .db
                    .append_side(
                        &conv_id,
                        audience,
                        "draft.discarded",
                        Some(&sender.id),
                        json!({"draft_id": msg_id, "reason": r}),
                    )
                    .await?;
                self.out.push(Out::Side(side));
            }
        }
        self.notify_owner(
            &conv_id,
            &sender,
            "policy.blocked",
            json!({"agent_id": sender.id, "msg_id": msg_id, "reason": reason}),
        )
        .await?;
        self.out.push(Out::ToUser {
            user_id: sender.id.clone(),
            frame: json!({"type": "stream_cancelled", "msg_id": msg_id, "reason": r}),
        });
        self.st.streams.remove(msg_id);
        self.stream_removed(msg_id);
        metrics::counter!(m::STREAMS_FINISHED, "state" => "cancelled", "reason" => "policy")
            .increment(1);
        Ok(())
    }

    /// Terminal transition of a stream: complete | cancelled | failed.
    async fn finish_stream(
        &mut self,
        msg_id: &str,
        state: &str,
        reason: Option<&str>,
        actor: &str,
    ) -> ApiResult<()> {
        self.flush_one(msg_id);
        let (conv_id, sender, target, buf, hop, causation_id, reply_to) =
            match self.st.streams.get(msg_id) {
                Some(s) => (
                    s.conv_id.clone(),
                    s.sender.clone(),
                    s.target.clone(),
                    s.buf.clone(),
                    s.hop,
                    s.causation_id.clone(),
                    s.reply_to.clone(),
                ),
                None => return Ok(()),
            };
        match &target {
            Target::Main => {
                // A completed stream wakes the agents it is for (D43), and
                // whom it mentions, who are told as for an edit (D87).
                let data = if state == "complete" {
                    let mentions = self.mentions(&conv_id, &buf).await?;
                    self.db.set_mentions(msg_id, &mentions).await?;
                    let (w, capped) = self
                        .wake_set(&conv_id, &sender.id, reply_to.as_deref(), &mentions, None)
                        .await?;
                    let mut d = json!({"wake": w});
                    if capped > 0 {
                        d["wake_capped"] = json!(capped);
                    }
                    let named: Vec<&String> =
                        mentions.iter().filter(|m| **m != sender.id).collect();
                    if !named.is_empty() {
                        d["mentions_added"] = json!(named);
                    }
                    d
                } else {
                    json!({})
                };
                let ev = self
                    .db
                    .update_message_with(
                        msg_id,
                        "msg.updated",
                        actor,
                        Some(&buf),
                        Some(state),
                        reason,
                        data,
                    )
                    .await?;
                self.emit(ev).await?;
            }
            Target::Draft { audience } => {
                if state == "complete" && !buf.is_empty() {
                    let (d, side) = self
                        .db
                        .create_draft(
                            msg_id,
                            &conv_id,
                            &sender,
                            &buf,
                            hop,
                            causation_id,
                            reply_to,
                            audience,
                            "text",
                            None,
                            None,
                        )
                        .await?;
                    self.db
                        .audit(
                            Some(&sender.id),
                            "draft.created",
                            Some(&conv_id),
                            Some(&d.draft_id),
                            json!({"streamed": true}),
                        )
                        .await?;
                    metrics::counter!(m::DRAFTS, "state" => "pending").increment(1);
                    self.out.push(Out::Side(side));
                } else {
                    let side = self
                        .db
                        .append_side(
                            &conv_id,
                            audience,
                            "draft.discarded",
                            Some(actor),
                            json!({"draft_id": msg_id, "reason": reason.unwrap_or(state)}),
                        )
                        .await?;
                    self.out.push(Out::Side(side));
                }
            }
        }
        if state != "complete" && actor != sender.id {
            self.out.push(Out::ToUser { user_id: sender.id.clone(), frame: json!({"type": "stream_cancelled", "msg_id": msg_id, "reason": reason.unwrap_or(state)}) });
        }
        self.st.streams.remove(msg_id);
        self.stream_removed(msg_id);
        let reason_label = reason
            .map(|r| r.split(':').next().unwrap_or(r).to_string())
            .unwrap_or_else(|| "end".into());
        metrics::counter!(m::STREAMS_FINISHED, "state" => state.to_string(), "reason" => reason_label).increment(1);
        Ok(())
    }

    /// Queues a stream's pending delta for the sessions looking at it.
    fn flush_one(&mut self, msg_id: &str) {
        let Some(st) = self.st.streams.get_mut(msg_id) else {
            return;
        };
        if st.pending.is_empty() {
            return;
        }
        let text = std::mem::take(&mut st.pending);
        let audience = match &st.target {
            Target::Main => None,
            Target::Draft { audience } => Some(audience.clone()),
        };
        self.out.push(Out::Delta {
            conv_id: st.conv_id.clone(),
            msg_id: st.msg_id.clone(),
            text,
            reset: false,
            sender_id: st.sender.id.clone(),
            audience,
        });
    }

    async fn tick(&mut self) -> ApiResult<()> {
        let now = Instant::now();
        if now.duration_since(self.st.last_flush) >= self.sh.cfg.coalesce {
            self.st.last_flush = now;
            let ids: Vec<String> = self.st.streams.keys().cloned().collect();
            for id in ids {
                self.flush_one(&id);
            }
        }
        if now.duration_since(self.st.last_checkpoint) >= self.sh.cfg.checkpoint_every {
            self.st.last_checkpoint = now;
            let dirty: Vec<(String, String, String)> = self
                .st
                .streams
                .values_mut()
                .filter(|s| matches!(s.target, Target::Main) && s.buf.len() != s.checkpointed)
                .map(|s| {
                    s.checkpointed = s.buf.len();
                    (s.msg_id.clone(), s.conv_id.clone(), s.buf.clone())
                })
                .collect();
            for (id, conv, content) in dirty {
                self.touch(&conv);
                self.db.checkpoint(&id, &content).await?;
            }
        }
        let expired: Vec<(String, &'static str, &'static str, String)> = self
            .st
            .streams
            .values()
            .filter_map(|s| {
                if now.duration_since(s.last_append) > self.sh.cfg.stream_idle {
                    Some((s.msg_id.clone(), "failed", "timeout", s.sender.id.clone()))
                } else if now.duration_since(s.started) > self.sh.cfg.stream_max {
                    Some((
                        s.msg_id.clone(),
                        "complete",
                        "duration_limit",
                        s.sender.id.clone(),
                    ))
                } else {
                    None
                }
            })
            .collect();
        for (id, state, reason, sender) in expired {
            self.finish_stream(&id, state, Some(reason), &sender)
                .await?;
        }
        if self.st.last_expiry.elapsed() > self.sh.cfg.expire_every {
            self.st.last_expiry = now;
            self.expire_requests().await?;
            self.expire_takeovers().await?;
        }
        if self.st.last_gc.elapsed() > self.sh.cfg.gc_every && self.upkeep() {
            self.st.last_gc = now;
            self.db
                .dedup_gc(now_ms() - self.sh.cfg.dedup_ttl.as_millis() as i64)
                .await?;
            let sync = &self.sh.cfg.sync;
            let now = now_ms();
            let stale = now - sync.device_ttl.as_millis() as i64;
            self.db.drop_stale_cursors(stale).await?;
            self.db.drop_old_departures(stale).await?;
            if let Some(keep) = sync.retention {
                let owned: Option<std::collections::HashSet<u32>> =
                    self.sh.cluster.get().map(|c| {
                        (0..c.shards())
                            .filter(|s| c.held(self.idx, *s).is_some())
                            .collect()
                    });
                let shards = self.sh.cluster.get().map_or(1, |c| c.shards());
                let (n, purged) = self
                    .db
                    .purge_before(now - keep.as_millis() as i64, PURGE_BATCH, |c| {
                        owned
                            .as_ref()
                            .is_none_or(|o| o.contains(&crate::cluster::shard_of(c, shards)))
                    })
                    .await?;
                for c in purged {
                    self.touch(&c);
                }
                if n > 0 {
                    tracing::info!(rows = n, "retention: deleted old conversation content");
                }
            }
        }
        Ok(())
    }

    // -------------------------------------------------------- intervention

    async fn whisper(
        &mut self,
        me: &User,
        conv_id: &str,
        agent_id: &str,
        text: &str,
    ) -> ApiResult<Value> {
        let agent = self.owned_agent_in(me, agent_id, conv_id).await?;
        self.require_member(conv_id, &agent.id).await?;
        check_text(text)?;
        let audience = vec![agent.id.clone(), me.id.clone()];
        let side = self
            .db
            .append_side(
                conv_id,
                &audience,
                "whisper",
                Some(&me.id),
                json!({"text": text, "from": me.id, "from_name": me.name, "to": agent.id}),
            )
            .await?;
        self.db
            .audit(
                Some(&me.id),
                "whisper",
                Some(conv_id),
                Some(&agent.id),
                json!({}),
            )
            .await?;
        let v = json!({"side_seq": side.side_seq, "anchor_seq": side.anchor_seq});
        self.out.push(Out::Side(side));
        Ok(v)
    }

    async fn approve(
        &mut self,
        me: &User,
        draft_id: &str,
        text: Option<String>,
    ) -> ApiResult<Value> {
        let d = self
            .db
            .draft(draft_id)
            .await?
            .ok_or_else(|| err("conv_moved", "draft not in this database"))?;
        self.own_conv(&d.conv_id)?;
        let agent = self.owned_agent_in(me, &d.agent_id, &d.conv_id).await?;
        if d.state != "pending" {
            return Err(rule!("draft.decided", "conflict", "draft already decided"));
        }
        if self.db.takeover(&d.conv_id, &agent.id).await?.is_some() {
            return Err(rule!(
                "draft.agent_taken_over",
                "conflict",
                "the agent is taken over; release it before publishing its drafts",
            ));
        }
        if self.sh.stopped.read().unwrap().contains(&agent.id) {
            return Err(rule!(
                "draft.agent_stopped",
                "conflict",
                "the agent is stopped; resume it before publishing its drafts",
            ));
        }
        // Silenced by moderation: its drafts wait too.
        if let Ok(m) = self.require_member(&d.conv_id, &agent.id).await
            && m.silenced_until.is_some_and(|u| u > now_ms())
        {
            return Err(rule!(
                "draft.agent_silenced",
                "conflict",
                "the agent is silenced in this conversation; its drafts wait",
            ));
        }
        // An ended task stays silent for agents, drafts included: reopen it
        // first.
        if let Some(st) = self.task_state(&d.conv_id).await?
            && tasks::terminal(&st)
        {
            return Err(rule!("task.state", "conflict", format!("the task is {st}")));
        }
        if let Some(t) = &text {
            check_text(t)?;
        }
        let audience = self.audience(&d.conv_id, &agent).await?;
        let side = self
            .db
            .decide_draft(draft_id, "approved", &me.id, &audience, &d.conv_id)
            .await?;
        let edited = text.as_ref().is_some_and(|t| *t != d.content);
        let content = text.unwrap_or(d.content.clone());
        let mentions = self.mentions(&d.conv_id, &content).await?;
        let root = self.root_of(&agent, d.causation_id.as_deref()).await?;
        let wake = self
            .wake_set(
                &d.conv_id,
                &agent.id,
                d.reply_to.as_deref(),
                &mentions,
                d.envelope.as_ref(),
            )
            .await?;
        let res = self
            .db
            .commit_message(NewMsg {
                conv_id: &d.conv_id,
                sender: &agent,
                client_msg_id: Some(draft_id),
                on_behalf_of: None,
                kind: &d.kind,
                content,
                payload: d.payload.clone(),
                state: "committed",
                hop: d.hop,
                causation_id: d.causation_id.clone(),
                reply_to: d.reply_to.clone(),
                mentions: mentions.clone(),
                approval: Some(json!({"by": me.id, "edited": edited, "draft_id": draft_id})),
                envelope: d.envelope.clone(),
                root_id: root,
                wake: Some(wake),
            })
            .await?;
        if let Committed::New(ev) = &res {
            self.charge(ev.msg.as_ref().unwrap()).await?;
            let mid = ev.msg.as_ref().unwrap().msg_id.clone();
            self.reattach(&d.payload, draft_id, &mid).await?;
        }
        self.db
            .audit(
                Some(&me.id),
                "draft.approved",
                Some(&d.conv_id),
                Some(draft_id),
                // D97: whether a person changed it and a hash of the draft
                // (oversight evidence), never the draft's text: it would
                // outlive a recall or deletion of the message.
                json!({
                    "edited": edited,
                    "draft_sha256": hex::encode(<sha2::Sha256 as sha2::Digest>::digest(d.content.as_bytes())),
                }),
            )
            .await?;
        metrics::counter!(m::DRAFTS, "state" => "approved").increment(1);
        metrics::histogram!(m::APPROVAL_LATENCY)
            .record((now_ms() - d.created_at).max(0) as f64 / 1000.0);
        self.out.push(Out::Side(side));
        if let Committed::New(ev) = res {
            let seq = ev.seq;
            self.emit(ev).await?;
            return Ok(json!({"seq": seq}));
        }
        Ok(json!({}))
    }

    async fn reject(&mut self, me: &User, draft_id: &str) -> ApiResult<Value> {
        let d = self
            .db
            .draft(draft_id)
            .await?
            .ok_or_else(|| err("conv_moved", "draft not in this database"))?;
        self.own_conv(&d.conv_id)?;
        let agent = self.owned_agent_in(me, &d.agent_id, &d.conv_id).await?;
        let audience = self.audience(&d.conv_id, &agent).await?;
        let side = self
            .db
            .decide_draft(draft_id, "rejected", &me.id, &audience, &d.conv_id)
            .await?;
        self.trash_ref(draft_id).await?;
        self.db
            .audit(
                Some(&me.id),
                "draft.rejected",
                Some(&d.conv_id),
                Some(draft_id),
                json!({}),
            )
            .await?;
        metrics::counter!(m::DRAFTS, "state" => "rejected").increment(1);
        metrics::histogram!(m::APPROVAL_LATENCY)
            .record((now_ms() - d.created_at).max(0) as f64 / 1000.0);
        self.out.push(Out::Side(side));
        Ok(json!({}))
    }

    /// Takeovers whose person has been silent too long go back to the agent.
    async fn expire_takeovers(&mut self) -> ApiResult<()> {
        let idle = self.sh.cfg.takeover_idle.as_millis() as i64;
        if idle == 0 {
            return Ok(());
        }
        let now = now_ms();
        for (conv_id, agent_id, by, since) in self.db.takeovers().await? {
            if self.sh.dir_get(&conv_id).is_none_or(|d| d.db != self.idx) || !self.owns(&conv_id) {
                continue;
            }
            let spoke = self.db.last_said(&conv_id, &by).await?.unwrap_or(0);
            if now - spoke.max(since) < idle {
                continue;
            }
            let ev = self
                .db
                .set_takeover_with(
                    &conv_id,
                    &agent_id,
                    None,
                    None,
                    json!({"reason": "idle_timeout"}),
                )
                .await?;
            self.db
                .audit(
                    None,
                    "release",
                    Some(&conv_id),
                    Some(&agent_id),
                    json!({"reason": "idle_timeout", "by": by}),
                )
                .await?;
            metrics::counter!(m::TAKEOVERS, "action" => "timeout").increment(1);
            self.emit(ev).await?;
        }
        Ok(())
    }

    async fn takeover(
        &mut self,
        me: &User,
        conv_id: &str,
        agent_id: &str,
        on: bool,
    ) -> ApiResult<Value> {
        let agent = self.owned_agent_in(me, agent_id, conv_id).await?;
        self.require_member(conv_id, &agent.id).await?;
        let current = self.db.takeover(conv_id, &agent.id).await?;
        if on == current.is_some() {
            return Err(rule!(
                "agent.taken_over",
                "conflict",
                if on {
                    "already taken over"
                } else {
                    "not taken over"
                },
            ));
        }
        if on {
            let ids: Vec<String> = self
                .st
                .streams
                .values()
                .filter(|s| s.sender.id == agent.id && s.conv_id == conv_id)
                .map(|s| s.msg_id.clone())
                .collect();
            for id in ids {
                self.finish_stream(&id, "cancelled", Some("taken_over"), &me.id)
                    .await?;
            }
        }
        let ev = self
            .db
            .set_takeover(conv_id, &agent.id, if on { Some(&me.id) } else { None })
            .await?;
        self.db
            .audit(
                Some(&me.id),
                if on { "takeover" } else { "release" },
                Some(conv_id),
                Some(&agent.id),
                json!({}),
            )
            .await?;
        metrics::counter!(m::TAKEOVERS, "action" => if on { "takeover" } else { "release" })
            .increment(1);
        let seq = ev.seq;
        self.emit(ev).await?;
        Ok(json!({"seq": seq}))
    }
}

impl Ctx<'_, '_> {
    async fn admin_op(&mut self, op: AdminOp) -> ApiResult<Value> {
        match op {
            AdminOp::Create(row) => {
                if self.db.admin_by("name", &row.name).await?.is_some() {
                    return Err(rule!(
                        "admin.name_taken",
                        "conflict",
                        "an admin with this name exists"
                    ));
                }
                self.db.insert_admin(&row).await?;
                Ok(json!({}))
            }
            AdminOp::Update { id, role, disabled } => {
                let Some(cur) = self.db.admin_by("id", &id).await? else {
                    return Err(rule!("admin.unknown", "not_found", "no such admin"));
                };
                // The last active owner stays an active owner.
                let demoted =
                    role.as_deref().is_some_and(|r| r != "owner") || disabled == Some(true);
                if cur.role == "owner" && cur.disabled_at.is_none() && demoted {
                    let owners = self
                        .db
                        .admins()
                        .await?
                        .iter()
                        .filter(|a| a.role == "owner" && a.disabled_at.is_none())
                        .count();
                    if owners <= 1 {
                        return Err(rule!(
                            "admin.last_owner",
                            "conflict",
                            "the last owner cannot be demoted or disabled",
                        ));
                    }
                }
                self.db.update_admin(&id, role.as_deref(), disabled).await?;
                if disabled == Some(true) {
                    self.db.delete_admin_sessions(Some(&id), None).await?;
                }
                Ok(json!({}))
            }
            AdminOp::SessionPut(s) => {
                self.db.insert_admin_session(&s).await?;
                Ok(json!({}))
            }
            AdminOp::SessionTouch { hash } => {
                self.db.touch_admin_session(&hash).await?;
                Ok(json!({}))
            }
            AdminOp::SessionsDelete { admin_id, hash } => {
                let n = self
                    .db
                    .delete_admin_sessions(admin_id.as_deref(), hash.as_deref())
                    .await?;
                Ok(json!({"deleted": n}))
            }
            AdminOp::SessionsExpire {
                idle_before,
                max_before,
            } => {
                let n = self
                    .db
                    .expire_admin_sessions(idle_before, max_before)
                    .await?;
                Ok(json!({"deleted": n}))
            }
            AdminOp::KeyPut(k) => {
                self.db.insert_admin_key(&k).await?;
                Ok(json!({}))
            }
            AdminOp::KeyRevoke { key_id } => {
                Ok(json!({"revoked": self.db.revoke_admin_key(&key_id).await?}))
            }
            AdminOp::DyeAdd(rule) => {
                let mut rules = self.dye_rules().await?;
                if rules.len() >= crate::trace::DYE_MAX_RULES {
                    return Err(rule!(
                        "dye.limit",
                        "rate_limited",
                        format!(
                            "at most {} dye rules at once; remove one first",
                            crate::trace::DYE_MAX_RULES
                        ),
                    ));
                }
                rules.push(rule.clone());
                self.save_dye_rules(rules).await?;
                Ok(json!(rule))
            }
            AdminOp::DyeRemove { id } => {
                let mut rules = self.dye_rules().await?;
                let before = rules.len();
                rules.retain(|r| r.id != id);
                if rules.len() == before {
                    return Err(rule!(
                        "dye.unknown",
                        "not_found",
                        "no such dye rule (it may have expired)"
                    ));
                }
                self.save_dye_rules(rules).await?;
                Ok(json!({"removed": id}))
            }
            AdminOp::Setting { tenant, key, value } => {
                self.db.set_app_setting(&tenant, &key, &value).await?;
                // Other processes on the same databases reload it.
                self.db.bump_meta(crate::db::AUTH_EPOCH).await?;
                if key == "clients.allow"
                    && let Ok(allow) = serde_json::from_value(value)
                {
                    self.out.push(Out::AllowDefaults { allow });
                }
                Ok(json!({}))
            }
        }
    }
}

impl Ctx<'_, '_> {
    /// The dye rules still in force, as stored (expired ones dropped).
    async fn dye_rules(&mut self) -> ApiResult<Vec<crate::trace::DyeRule>> {
        let now = now_ms();
        Ok(self
            .db
            .app_setting("default", "dye.rules")
            .await?
            .and_then(|v| serde_json::from_value::<Vec<crate::trace::DyeRule>>(v).ok())
            .unwrap_or_default()
            .into_iter()
            .filter(|r| r.active(now))
            .collect())
    }

    async fn save_dye_rules(&mut self, rules: Vec<crate::trace::DyeRule>) -> ApiResult<()> {
        self.db
            .set_app_setting("default", "dye.rules", &json!(rules))
            .await?;
        // Every node applies it after commit; others reload on the epoch.
        self.db.bump_meta(crate::db::AUTH_EPOCH).await?;
        self.out.push(Out::DyeRules { rules });
        Ok(())
    }
}

/// Refuses a member silenced by moderation in the conversation.
fn not_silenced(m: &Member) -> ApiResult<()> {
    match m.silenced_until {
        Some(u) if u > now_ms() => Err(rule!(
            "member.silenced",
            "forbidden",
            format!("silenced in this conversation until {u} (ms since 1970)"),
        )),
        _ => Ok(()),
    }
}

/// `@name` mentions in `text` among `(user_id, name)` members: see
/// `Logic::mentions`.
pub(crate) fn mentions_in<'a>(
    text: &str,
    members: impl Iterator<Item = (&'a str, &'a str)> + Clone,
) -> Vec<String> {
    let word = |c: char| c.is_alphanumeric() || c == '_';
    let mut out: Vec<String> = vec![];
    for (i, _) in text.match_indices('@') {
        if text[..i].chars().next_back().is_some_and(word) {
            continue;
        }
        let rest = &text[i + 1..];
        if let Some((id, _)) = members
            .clone()
            .filter(|(_, name)| {
                !name.is_empty()
                    && rest.starts_with(*name)
                    && !rest[name.len()..].chars().next().is_some_and(word)
            })
            .max_by_key(|(_, name)| name.len())
            && !out.iter().any(|o| o == id)
        {
            out.push(id.to_string());
        }
    }
    out
}

#[cfg(test)]
mod mention_tests {
    use super::mentions_in;

    fn m(text: &str) -> Vec<String> {
        let members = [
            ("u_bob", "bob"),
            ("u_bobby", "bobby"),
            ("u_sb", "Support Bot"),
            ("u_li", "李雷"),
        ];
        mentions_in(text, members.iter().copied())
    }

    #[test]
    fn a_mention_is_a_whole_name_after_a_non_word_character() {
        assert_eq!(m("@bob hi"), ["u_bob"]);
        assert_eq!(m("hi @bob"), ["u_bob"]);
        assert_eq!(m("(@bob)"), ["u_bob"]);
        assert_eq!(m("@bob, @bob!"), ["u_bob"], "once each");
        assert_eq!(m("@bob."), ["u_bob"]);
        assert_eq!(m("@bobby"), ["u_bobby"], "the longest name wins");
        assert_eq!(
            m("@bobcat"),
            Vec::<String>::new(),
            "not a member's whole name"
        );
        assert_eq!(m("@bob_x"), Vec::<String>::new());
        assert_eq!(m("mail x@bob.io"), Vec::<String>::new(), "an address");
        assert_eq!(m("@Support Bot help"), ["u_sb"], "names with spaces");
        assert_eq!(
            m("你好@李雷"),
            Vec::<String>::new(),
            "after a letter, also in Chinese"
        );
        assert_eq!(m("你好 @李雷"), ["u_li"]);
        assert_eq!(m("@李雷，你好"), ["u_li"]);
        assert_eq!(m("@ bob"), Vec::<String>::new());
        assert_eq!(m("@"), Vec::<String>::new());
        assert_eq!(m(""), Vec::<String>::new());
    }
}
