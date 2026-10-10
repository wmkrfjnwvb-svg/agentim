//! Business logic executed by a database's writer, inside the batch
//! transaction (one savepoint per command). Everything a command writes
//! commits together with everything else in its batch, after which its output
//! is delivered. A command that fails is rolled back alone, unless it set
//! `commit_on_error` (a rejection that must still be recorded, e.g. a breaker
//! trip or a policy block).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::db::{Cell, Committed, Db, DirEntry, NewMsg};
use crate::guard::{GuardConfig, Guards, Verdict};
use crate::metrics as m;
use crate::model::*;
use crate::policy::PolicyChain;
use crate::registry::{Out, Recip, Registry};

pub const MAX_MESSAGE_BYTES: usize = 64 * 1024;
/// Serialized payload of a custom message kind.
pub const MAX_PAYLOAD_BYTES: usize = 32 * 1024;
/// Serialized custom fields of a user or conversation.
pub const MAX_FIELDS_BYTES: usize = 4 * 1024;

pub fn check_title(t: &str) -> ApiResult<()> {
    if t.is_empty() || t.len() > 255 || t.contains('\0') {
        return Err(err("bad_request", "title must be 1..255 bytes"));
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
    /// `stats` (global counts, online users, audit) for every user. Only
    /// for the demo; operators get it from the admin API.
    pub open_stats: bool,
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
            open_stats: false,
        }
    }
}

// ------------------------------------------------------------ shared state

#[derive(Debug, Clone)]
pub struct DirInfo {
    pub db: usize,
    pub unit: String,
    pub migrating: bool,
}

/// State shared by the hub and every writer. All of it is either derived
/// from the databases (and rebuilt at startup) or soft state.
#[derive(Debug, Clone)]
pub struct TokenInfo {
    pub user_id: String,
    pub device_id: String,
    pub expires_at: Option<i64>,
}

pub struct Shared {
    pub reg: Registry,
    pub users: std::sync::RwLock<HashMap<String, User>>,
    /// SHA-256 of each valid client token → its device.
    pub tokens: std::sync::RwLock<HashMap<String, TokenInfo>>,
    /// Users that may not log in or act (open API deactivation).
    pub deactivated: std::sync::RwLock<HashSet<String>>,
    pub dir: std::sync::RwLock<HashMap<String, DirInfo>>,
    /// Active stream (or streamed draft) id → database index.
    pub stream_index: std::sync::Mutex<HashMap<String, usize>>,
    pub policy: PolicyChain,
    pub cfg: HubConfig,
    pub budgets: crate::guard::Budgets,
    pub control: usize,
    pub db_names: Vec<String>,
    /// (user, blocked user) pairs, loaded from the control database.
    pub blocks: std::sync::RwLock<std::collections::HashSet<(String, String)>>,
}

impl Shared {
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

    pub fn dir_set(&self, conv: &str, info: Option<DirInfo>) {
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
        }
    }

    /// Whether a tick has database work (otherwise the writer skips the
    /// transaction entirely).
    pub fn tick_needs_db(&self) -> bool {
        !self.streams.is_empty() || self.last_gc.elapsed() > self.gc_every
    }

    pub fn active_streams(&self) -> Vec<Value> {
        self.streams
            .values()
            .map(|s| json!({"msg_id": s.msg_id, "conv_id": s.conv_id, "agent": s.sender.name, "bytes": s.buf.len(), "age_ms": s.started.elapsed().as_millis() as u64, "draft": matches!(s.target, Target::Draft{..})}))
            .collect()
    }
}

// ------------------------------------------------------------------- ops

#[derive(Debug, Clone)]
pub struct SessCtx {
    pub sid: u64,
    pub user: User,
}

#[derive(Debug)]
pub enum Op {
    Client {
        sess: SessCtx,
        msg: ClientMsg,
    },
    Tick,
    Recover,
    /// A webhook handled the outbox up to `upto`; `dropped` gave up.
    WebhookAck {
        name: String,
        upto: i64,
        dropped: Vec<String>,
        /// Every configured webhook, to clean up rows all of them handled.
        all: Vec<String>,
    },
    AgentGone(String),
    Barrier,
    // control database
    CreateUser {
        user: User,
    },
    IssueToken(crate::db::TokenRow),
    /// All tokens of a user, or of one device. Returns the removed hashes.
    RevokeTokens {
        user_id: String,
        device_id: Option<String>,
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
    },
    AddMember {
        conv_id: String,
        actor: User,
        user: User,
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
    MemberIndexRemove {
        conv_id: String,
        user_id: String,
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
    StopConvStreams(String),
    ImportConv {
        conv_id: String,
        rows: Vec<(&'static str, Vec<Vec<Cell>>)>,
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
}

impl Op {
    pub fn is_tick(&self) -> bool {
        matches!(self, Op::Tick)
    }

    pub fn name(&self) -> &'static str {
        match self {
            Op::Client { msg, .. } => msg.name(),
            Op::Tick => "tick",
            Op::Recover => "recover",
            Op::WebhookAck { .. } => "webhook_ack",
            Op::AgentGone(_) => "agent_gone",
            Op::Barrier => "barrier",
            Op::CreateUser { .. } => "create_user",
            Op::IssueToken(_) => "issue_token",
            Op::RevokeTokens { .. } => "revoke_tokens",
            Op::TouchToken { .. } => "touch_token",
            Op::UpdateUser { .. } => "update_user",
            Op::SetDeactivated { .. } => "set_deactivated",
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
            Op::ForgetConv(_) => "forget_conv",
            Op::SetBlock { .. } => "set_block",
            Op::DmPut { .. } => "dm_put",
            Op::StopConvStreams(_) => "stop_conv_streams",
            Op::ImportConv { .. } => "import_conv",
            Op::DeleteConv(_) => "delete_conv",
            Op::Audit { .. } => "audit",
            Op::Stats => "stats",
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

/// Message text: 1..64KB, and no NUL (PostgreSQL cannot store it).
pub fn check_text(text: &str) -> ApiResult<()> {
    if text.is_empty() || text.len() > MAX_MESSAGE_BYTES {
        return Err(err("bad_request", "text must be 1..64KB"));
    }
    if text.contains('\0') {
        return Err(err("bad_request", "text must not contain NUL"));
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
        return Err(err(
            "bad_request",
            "custom kinds are namespaced like com.example.order (a-z, 0-9, _ and -, at most 64 bytes)",
        ));
    }
    if kind.starts_with("agentim.") {
        return Err(err("bad_request", "the agentim. namespace is reserved"));
    }
    Ok(())
}

/// A JSON object of at most `max` serialized bytes.
pub fn check_json_object(v: &Value, what: &str, max: usize) -> ApiResult<()> {
    if !v.is_object() {
        return Err(err("bad_request", format!("{what} must be a JSON object")));
    }
    if v.to_string().len() > max {
        return Err(err(
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
                return Err(err("bad_request", "payload needs a custom kind"));
            }
            Ok("text".into())
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
}

impl Ctx<'_, '_> {
    pub async fn run(&mut self, op: Op) -> ApiResult<Value> {
        match op {
            Op::Client { sess, msg } => self.client(&sess, msg).await,
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
                for id in &dropped {
                    self.db
                        .audit(
                            None,
                            "webhook.dropped",
                            None,
                            Some(id),
                            json!({"webhook": name}),
                        )
                        .await?;
                }
                self.db.webhook_ack(&name, upto, &all).await?;
                Ok(json!({}))
            }
            Op::Recover => {
                for msg in self.db.streaming_messages().await? {
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
            Op::CreateUser { user } => {
                self.db.insert_user(&user).await?;
                Ok(json!({}))
            }
            Op::IssueToken(t) => {
                self.db.insert_token(&t).await?;
                Ok(json!({}))
            }
            Op::RevokeTokens { user_id, device_id } => {
                let gone = self
                    .db
                    .delete_tokens(&user_id, device_id.as_deref())
                    .await?;
                Ok(json!({"hashes": gone}))
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
            Op::SetDeactivated { user_id, on } => {
                self.db.set_deactivated(&user_id, on).await?;
                Ok(json!({}))
            }
            Op::CreateApiKey(k) => {
                self.db.insert_api_key(&k).await?;
                Ok(json!({}))
            }
            Op::RevokeApiKey { key_id } => {
                let n = self.db.revoke_api_key(&key_id).await?;
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
            } => {
                let events = self
                    .db
                    .create_conv(&id, &kind, &title, &unit, &creator, &members)
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
                Ok(json!({"conv_id": id}))
            }
            Op::AddMember {
                conv_id,
                actor,
                user,
            } => self.add_member(&conv_id, &actor, &user).await,
            Op::RemoveMember {
                conv_id,
                actor,
                user,
            } => self.remove_member(&conv_id, &actor, &user).await,
            Op::Dissolve { conv_id, actor } => self.dissolve(&conv_id, &actor).await,
            Op::MemberIndexRemove { conv_id, user_id } => {
                self.db.member_index_remove(&conv_id, &user_id).await?;
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
                let mut b = self.sh.blocks.write().unwrap_or_else(|e| e.into_inner());
                if on {
                    b.insert((user_id, target));
                } else {
                    b.remove(&(user_id, target));
                }
                Ok(json!({}))
            }
            Op::DmPut { pair_key, conv_id } => {
                self.db.dm_put(&pair_key, &conv_id).await?;
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
            } => {
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
                )
                .await
            }
            ClientMsg::Edit {
                msg_id,
                text,
                payload,
            } => self.edit(me, &msg_id, &text, payload).await,
            ClientMsg::Recall { msg_id } => self.recall(me, &msg_id).await,
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
                    .ok_or_else(|| err("not_found", "no active stream"))?;
                if st.sender.id != me.id {
                    return Err(err("forbidden", "not your stream"));
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
            } => {
                let agent = self.owned_agent(me, &agent_id)?;
                self.require_member(&conv_id, &agent.id).await?;
                let ev = self
                    .db
                    .set_agent_policy(&conv_id, &agent.id, approval, trigger, &me.id)
                    .await?;
                self.st.members.remove(&conv_id);
                self.st.recips.remove(&conv_id);
                self.db
                    .audit(
                        Some(&me.id),
                        "agent.policy",
                        Some(&conv_id),
                        Some(&agent.id),
                        json!({"approval": approval, "trigger": trigger}),
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
                    return Err(err(
                        "forbidden",
                        "only the owner or an admin changes the conversation",
                    ));
                }
                if title.is_none() && fields.is_none() {
                    return Err(err("bad_request", "nothing to change"));
                }
                if let Some(t) = &title {
                    check_title(t)?;
                }
                if let Some(f) = &fields {
                    check_json_object(f, "fields", MAX_FIELDS_BYTES)?;
                }
                if self.is_dm(&conv_id).await? && title.is_some() {
                    return Err(err("bad_request", "a direct conversation has no title"));
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
                let agent = self.owned_agent(me, &agent_id)?;
                self.require_member(&conv_id, &agent.id).await?;
                if self.st.guards.is_tripped(&agent.id, &conv_id).is_none() {
                    return Err(err("conflict", "the agent's guard is not tripped"));
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
            other => Err(err(
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
            None => Err(err("not_found", "conversation")),
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
                None => Err(err("not_found", "conversation")),
            };
        }
        self.members(conv_id)
            .await?
            .iter()
            .find(|m| m.user_id == user_id)
            .cloned()
            .ok_or_else(|| err("forbidden", "not a member of this conversation"))
    }

    fn owned_agent(&self, me: &User, agent_id: &str) -> ApiResult<User> {
        let a = self
            .sh
            .user(agent_id)
            .ok_or_else(|| err("not_found", "agent"))?;
        if a.kind != UserKind::Agent || a.owner_id.as_deref() != Some(me.id.as_str()) {
            return Err(err("forbidden", "you do not own this agent"));
        }
        Ok(a)
    }

    fn resolve_user(&self, key: &str) -> ApiResult<User> {
        self.sh
            .user(key)
            .or_else(|| self.sh.user_by_name(key))
            .ok_or_else(|| err("not_found", format!("user {key}")))
    }

    fn agent_audience(agent: &User) -> Vec<String> {
        let mut a = vec![agent.id.clone()];
        if let Some(o) = &agent.owner_id {
            a.push(o.clone());
        }
        a
    }

    /// What an agent member may read (bounded by its trigger mode).
    pub async fn agent_can_see(db: &mut Db<'_>, m: &Member, ev: &Event) -> ApiResult<bool> {
        if ev.seq < m.join_seq {
            return Ok(false);
        }
        let Some(msg) = &ev.msg else { return Ok(true) };
        if m.trigger == Trigger::All
            || msg.sender_id == m.user_id
            || msg.mentions.contains(&m.user_id)
        {
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
            Arc::new(recips)
        } else {
            base
        };
        metrics::counter!(m::EVENTS_COMMITTED, "type" => ev.kind.clone()).increment(1);
        if ev.kind == "msg.created"
            && let Some(msg) = &ev.msg
        {
            metrics::counter!(m::MESSAGES_COMMITTED, "sender_type" => msg.sender_type.as_str())
                .increment(1);
        }
        self.out.push(Out::Event { ev, recips });
        Ok(())
    }

    async fn notify_owner(
        &mut self,
        conv_id: &str,
        agent: &User,
        kind: &str,
        data: Value,
    ) -> ApiResult<()> {
        let audience = Self::agent_audience(agent);
        let side = self
            .db
            .append_side(conv_id, &audience, kind, Some(&agent.id), data)
            .await?;
        self.out.push(Out::Side(side));
        Ok(())
    }

    /// Members named after an `@`. At each `@` the longest matching name
    /// wins, so `@bobby` mentions bobby and not bob.
    async fn mentions(&mut self, conv_id: &str, text: &str) -> ApiResult<Vec<String>> {
        let members = self.members(conv_id).await?;
        let mut out: Vec<String> = vec![];
        for (i, _) in text.match_indices('@') {
            let rest = &text[i + 1..];
            if let Some(m) = members
                .iter()
                .filter(|m| !m.name.is_empty() && rest.starts_with(m.name.as_str()))
                .max_by_key(|m| m.name.len())
                && !out.contains(&m.user_id)
            {
                out.push(m.user_id.clone());
            }
        }
        Ok(out)
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
                let msg = self
                    .db
                    .message(&id)
                    .await?
                    .ok_or_else(|| err("bad_request", "unknown causation message"))?;
                if msg.conv_id != conv_id {
                    return Err(err(
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

    async fn add_member(&mut self, conv_id: &str, me: &User, u: &User) -> ApiResult<Value> {
        self.own_conv(conv_id)?;
        let mine = self.require_member(conv_id, &me.id).await?;
        let own_agent = u.owner_id.as_deref() == Some(me.id.as_str());
        if !is_manager(&mine.role) && !own_agent {
            return Err(err(
                "forbidden",
                "only the owner or an admin can add members",
            ));
        }
        if self.sh.blocked(&u.id, &me.id) {
            return Err(err(
                "blocked",
                "this user does not accept invitations from you",
            ));
        }
        let conv = self
            .db
            .conv(conv_id)
            .await?
            .ok_or_else(|| err("not_found", "conversation"))?;
        if conv.kind == "dm" {
            return Err(err(
                "bad_request",
                "a direct conversation has fixed members",
            ));
        }
        let ev = self
            .db
            .add_member(conv_id, &conv.kind, u, "member", &me.id)
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
        }
        let seq = ev.seq;
        self.emit(ev).await?;
        Ok(json!({"seq": seq}))
    }

    async fn is_dm(&mut self, conv_id: &str) -> ApiResult<bool> {
        Ok(self.db.conv(conv_id).await?.is_some_and(|c| c.kind == "dm"))
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
        if !self.sh.blocked_either(&other.user_id, me) {
            return Ok(());
        }
        let dm = self.db.conv(conv_id).await?.is_some_and(|c| c.kind == "dm");
        if dm {
            return Err(err("blocked", "you cannot message this user"));
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
            .ok_or_else(|| err("not_found", "not a member of this conversation"))?;
        if self.is_dm(conv_id).await? {
            return Err(err(
                "bad_request",
                "a direct conversation has fixed members; dissolve it instead",
            ));
        }
        let leaving = me.id == u.id;
        if me.kind == UserKind::System && target.role == "owner" {
            return Err(err(
                "conflict",
                "transfer ownership before removing the owner",
            ));
        }
        if leaving {
            if target.role == "owner" {
                return Err(err(
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
                return Err(err("forbidden", "you cannot remove this member"));
            }
        }
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
                json!({"user_id": u.id, "name": u.name, "kind": u.kind, "by": me.id}),
            )
            .await?;
        let seq = ev.seq;
        // The member still receives this event; nothing after it.
        self.emit(ev).await?;
        self.db.remove_member(conv_id, &u.id).await?;
        self.st.members.remove(conv_id);
        self.st.recips.remove(conv_id);
        self.out.push(Out::Unfocus {
            conv_id: conv_id.to_string(),
            user_id: Some(u.id.clone()),
        });
        if self.idx == self.sh.control {
            self.db.member_index_remove(conv_id, &u.id).await?;
        }
        self.db
            .audit(Some(&me.id), kind, Some(conv_id), Some(&u.id), json!({}))
            .await?;
        Ok(json!({"seq": seq}))
    }

    async fn set_role(
        &mut self,
        me: &User,
        conv_id: &str,
        user_id: &str,
        role: &str,
    ) -> ApiResult<Value> {
        if role != "admin" && role != "member" {
            return Err(err("bad_request", "role must be admin or member"));
        }
        if self.is_dm(conv_id).await? {
            return Err(err("bad_request", "a direct conversation has no roles"));
        }
        let mine = self.require_member(conv_id, &me.id).await?;
        if mine.role != "owner" {
            return Err(err("forbidden", "only the owner changes roles"));
        }
        let u = self.resolve_user(user_id)?;
        let target = self
            .require_member(conv_id, &u.id)
            .await
            .map_err(|_| err("not_found", "not a member of this conversation"))?;
        if target.role == "owner" {
            return Err(err("conflict", "use transfer_owner to change the owner"));
        }
        if target.kind != UserKind::Human {
            return Err(err("bad_request", "only people can be admins"));
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

    async fn transfer_owner(
        &mut self,
        me: &User,
        conv_id: &str,
        user_id: &str,
    ) -> ApiResult<Value> {
        let mine = self.require_member(conv_id, &me.id).await?;
        if mine.role != "owner" {
            return Err(err("forbidden", "only the owner transfers ownership"));
        }
        if self.is_dm(conv_id).await? {
            return Err(err(
                "bad_request",
                "a direct conversation has no owner to transfer",
            ));
        }
        let u = self.resolve_user(user_id)?;
        let target = self
            .require_member(conv_id, &u.id)
            .await
            .map_err(|_| err("not_found", "not a member of this conversation"))?;
        if target.user_id == me.id {
            return Err(err("conflict", "you are already the owner"));
        }
        if target.kind != UserKind::Human {
            return Err(err("bad_request", "only a person can own a conversation"));
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
            return Err(err("forbidden", "only the owner dissolves a conversation"));
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
        self.db.delete_conv_content(conv_id).await?;
        if self.idx == self.sh.control {
            self.db.forget_conv(conv_id).await?;
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
    ) -> ApiResult<Value> {
        let member = self.require_member(conv_id, &me.id).await?;
        check_text(text)?;
        let kind = check_kind(kind, &payload)?;
        if let Some(o) = &on_behalf_of {
            // Community rule: an agent may only speak on behalf of its owner.
            if me.kind != UserKind::Agent || me.owner_id.as_deref() != Some(o.as_str()) {
                return Err(err("forbidden", "on_behalf_of must be the agent's owner"));
            }
        }
        let (causation_id, hop) = self.causation(conv_id, me, causation_id, &reply_to).await?;
        if me.kind == UserKind::Agent {
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
                return Err(err("policy_blocked", reason));
            }
        }

        self.check_dm_block(conv_id, &me.id).await?;
        // Targeted visibility goes to the side channel and takes no conv_seq.
        if let Some(target) = to_user {
            let t = self.resolve_user(&target)?;
            self.require_member(conv_id, &t.id).await?;
            if self.sh.blocked_either(&t.id, &me.id) {
                return Err(err("blocked", "you cannot message this user"));
            }
            let mut audience = Self::agent_audience(me);
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
            return Ok(json!({"side_seq": side_seq}));
        }

        if me.kind == UserKind::Agent && member.approval {
            let audience = Self::agent_audience(me);
            let draft_id = format!("d_{}", crate::db::new_id());
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
            return Ok(json!({"draft_id": d.draft_id, "state": "pending_approval"}));
        }

        let mentions = self.mentions(conv_id, text).await?;
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
            })
            .await?;
        match res {
            Committed::Duplicate { seq, msg_id } => {
                metrics::counter!(m::DEDUP_HITS).increment(1);
                Ok(json!({"seq": seq, "msg_id": msg_id, "duplicate": true}))
            }
            Committed::New(ev) => {
                let msg = ev.msg.as_ref().unwrap();
                let v = json!({"seq": ev.seq, "msg_id": msg.msg_id, "hop": msg.hop});
                self.emit(ev).await?;
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
    ) -> ApiResult<Value> {
        let msg = self
            .db
            .message(msg_id)
            .await?
            .ok_or_else(|| err("conv_moved", "message not in this database"))?;
        self.own_conv(&msg.conv_id)?;
        if !self.can_modify(me, &msg, false).await? {
            return Err(err("forbidden", "cannot edit this message"));
        }
        if msg.state == "streaming" || msg.state == "recalled" {
            return Err(err("conflict", format!("message is {}", msg.state)));
        }
        check_text(text)?;
        if let Some(p) = &payload {
            if msg.kind == "text" {
                return Err(err("bad_request", "text messages have no payload"));
            }
            check_json_object(p, "payload", MAX_PAYLOAD_BYTES)?;
        }
        let scanned = payload.as_ref().or(msg.payload.as_ref());
        if me.kind == UserKind::Agent
            && let Some((_, reason)) = self.sh.policy.outbound(&scan_text(text, scanned))
        {
            return Err(err("policy_blocked", reason));
        }
        if payload.is_some() {
            self.db.set_payload(msg_id, payload.as_ref()).await?;
        }
        let ev = self
            .db
            .update_message(msg_id, "msg.updated", &me.id, Some(text), None, None)
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
            return Err(err("forbidden", "cannot recall this message"));
        }
        if msg.state == "recalled" {
            return Err(err("conflict", "message is already recalled"));
        }
        if msg.state == "streaming" {
            self.finish_stream(msg_id, "cancelled", Some("recalled"), &me.id)
                .await?;
        }
        self.db.set_payload(msg_id, None).await?;
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
            return Err(err("forbidden", "only agents stream"));
        }
        let member = self.require_member(conv_id, &me.id).await?;
        let active = self
            .st
            .streams
            .values()
            .filter(|s| s.sender.id == me.id && s.conv_id == conv_id)
            .count();
        if active >= 2 {
            return Err(err(
                "rate_limited",
                "too many concurrent streams in this conversation",
            ));
        }
        let (causation_id, hop) = self.causation(conv_id, me, causation_id, &reply_to).await?;
        self.agent_gate(me, conv_id, hop, "").await?;
        let now = Instant::now();
        let (msg_id, target, seq) = if member.approval {
            (
                format!("d_{}", crate::db::new_id()),
                Target::Draft {
                    audience: Self::agent_audience(me),
                },
                None,
            )
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
                })
                .await?;
            match res {
                Committed::Duplicate { seq, msg_id } => {
                    return Ok(json!({"msg_id": msg_id, "seq": seq, "duplicate": true}));
                }
                Committed::New(ev) => {
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
            .insert(msg_id.clone(), self.idx);
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
            .ok_or_else(|| err("not_found", "no active stream"))?;
        if st.sender.id != me.id {
            return Err(err("forbidden", "not your stream"));
        }
        if idx < st.next_idx {
            return Ok(json!({"duplicate": true}));
        }
        if text.contains('\0') {
            return Err(err("bad_request", "text must not contain NUL"));
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
            return Err(err("budget_exceeded", "daily agent budget exhausted"));
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
            return Err(err("policy_blocked", reason));
        }
        Ok(json!({}))
    }

    async fn stream_cancel(&mut self, me: &User, msg_id: &str) -> ApiResult<Value> {
        let st = self
            .st
            .streams
            .get(msg_id)
            .ok_or_else(|| err("not_found", "no active stream"))?;
        let sender = st.sender.clone();
        let conv_id = st.conv_id.clone();
        let is_owner = sender.owner_id.as_deref() == Some(me.id.as_str());
        let human_member =
            me.kind == UserKind::Human && self.require_member(&conv_id, &me.id).await.is_ok();
        if sender.id != me.id && !is_owner && !human_member {
            return Err(err("forbidden", "cannot interrupt this stream"));
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
                let ev = self
                    .db
                    .update_message(
                        msg_id,
                        "msg.updated",
                        actor,
                        Some(&buf),
                        Some(state),
                        reason,
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
            let dirty: Vec<(String, String)> = self
                .st
                .streams
                .values_mut()
                .filter(|s| matches!(s.target, Target::Main) && s.buf.len() != s.checkpointed)
                .map(|s| {
                    s.checkpointed = s.buf.len();
                    (s.msg_id.clone(), s.buf.clone())
                })
                .collect();
            for (id, content) in dirty {
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
        if self.st.last_gc.elapsed() > self.sh.cfg.gc_every {
            self.st.last_gc = now;
            self.db
                .dedup_gc(now_ms() - self.sh.cfg.dedup_ttl.as_millis() as i64)
                .await?;
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
        let agent = self.owned_agent(me, agent_id)?;
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
        let agent = self.owned_agent(me, &d.agent_id)?;
        if d.state != "pending" {
            return Err(err("conflict", "draft already decided"));
        }
        if self.db.takeover(&d.conv_id, &agent.id).await?.is_some() {
            return Err(err(
                "conflict",
                "the agent is taken over; release it before publishing its drafts",
            ));
        }
        let audience = Self::agent_audience(&agent);
        let side = self
            .db
            .decide_draft(draft_id, "approved", &me.id, &audience, &d.conv_id)
            .await?;
        if let Some(t) = &text {
            check_text(t)?;
        }
        let content = text.unwrap_or(d.content.clone());
        let mentions = self.mentions(&d.conv_id, &content).await?;
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
                mentions,
            })
            .await?;
        self.db
            .audit(
                Some(&me.id),
                "draft.approved",
                Some(&d.conv_id),
                Some(draft_id),
                json!({"edited": d.content}),
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
        let agent = self.owned_agent(me, &d.agent_id)?;
        let audience = Self::agent_audience(&agent);
        let side = self
            .db
            .decide_draft(draft_id, "rejected", &me.id, &audience, &d.conv_id)
            .await?;
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

    async fn takeover(
        &mut self,
        me: &User,
        conv_id: &str,
        agent_id: &str,
        on: bool,
    ) -> ApiResult<Value> {
        let agent = self.owned_agent(me, agent_id)?;
        self.require_member(conv_id, &agent.id).await?;
        let current = self.db.takeover(conv_id, &agent.id).await?;
        if on == current.is_some() {
            return Err(err(
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
