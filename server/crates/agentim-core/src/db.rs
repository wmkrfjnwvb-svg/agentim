//! All business SQL, written once for SQLite, PostgreSQL and MySQL.
//!
//! Functions never open or commit transactions: writes run inside the
//! writer's batch transaction (one savepoint per command), reads run on a
//! pooled connection or inside that same transaction.

use serde_json::{Value, json};
use sqlx::AnyConnection;
use sqlx::Row;
use sqlx::any::AnyRow;

use crate::model::*;
use crate::storage::{Dialect, TABLES, Ty};

pub mod admin;
mod compliance;
mod files;
pub use compliance::ERASE_BATCH;
pub use files::{FileRow, TrashRow};

/// Escapes a user's text for `LIKE … ESCAPE '!'`.
fn like_escape(s: &str) -> String {
    s.replace('!', "!!").replace('%', "!%").replace('_', "!_")
}

pub fn new_id() -> String {
    uuid::Uuid::now_v7().simple().to_string()
}

pub struct Db<'a> {
    pub c: &'a mut AnyConnection,
    pub d: Dialect,
    /// Encryption at rest of this database (docs/04 §2.6).
    pub cx: &'a crate::crypto::Crypto,
    /// Reads stop at what the replicas have (application wait, docs/11
    /// §8.3). The writer reads everything.
    pub cap: bool,
    /// The writer: each new event counts as replicated at once (every mode
    /// but the application wait, which advances it after the replicas).
    pub confirm_now: bool,
}

/// Decrypts a column of a fetched row (keys must be preloaded).
fn dec(cx: &crate::crypto::Crypto, r: &AnyRow, col: &str, aad: &str) -> sqlx::Result<String> {
    let v: String = r.try_get(col)?;
    cx.open(aad, &v).map_err(|e| sqlx::Error::Decode(e.into()))
}

fn dec_opt(
    cx: &crate::crypto::Crypto,
    r: &AnyRow,
    col: &str,
    aad: &str,
) -> sqlx::Result<Option<String>> {
    match r.try_get::<Option<String>, _>(col)? {
        None => Ok(None),
        Some(v) => cx
            .open(aad, &v)
            .map(Some)
            .map_err(|e| sqlx::Error::Decode(e.into())),
    }
}

fn dec_json(
    cx: &crate::crypto::Crypto,
    r: &AnyRow,
    col: &str,
    aad: &str,
) -> sqlx::Result<Option<Value>> {
    Ok(dec_opt(cx, r, col, aad)?.and_then(|s| serde_json::from_str(&s).ok()))
}

#[derive(Debug, Clone)]
pub struct ConvRow {
    pub id: String,
    pub kind: String,
    pub title: String,
    pub unit_id: String,
    pub last_seq: i64,
    pub fields: Option<Value>,
}

#[derive(Debug, Clone)]
pub struct Draft {
    pub draft_id: String,
    pub conv_id: String,
    pub agent_id: String,
    pub content: String,
    pub hop: i64,
    pub causation_id: Option<String>,
    pub reply_to: Option<String>,
    pub state: String,
    pub created_at: i64,
    pub kind: String,
    pub payload: Option<Value>,
    pub envelope: Option<Value>,
}

/// A request for a human decision (docs/03-agents.md §4.2).
#[derive(Debug, Clone)]
pub struct Request {
    pub request_id: String,
    pub conv_id: String,
    pub agent_id: String,
    pub client_req_id: String,
    pub kind: String,
    pub title: String,
    pub options: Option<Value>,
    pub context: Option<Value>,
    pub state: String,
    pub created_at: i64,
    pub expires_at: i64,
    pub decided_by: Option<String>,
    pub decided_at: Option<i64>,
    pub response: Option<Value>,
}

impl Request {
    pub fn view(&self) -> Value {
        json!({
            "request_id": self.request_id,
            "conv_id": self.conv_id,
            "agent_id": self.agent_id,
            "client_req_id": self.client_req_id,
            "kind": self.kind,
            "title": self.title,
            "options": self.options,
            "context": self.context,
            "state": self.state,
            "created_at": self.created_at,
            "expires_at": self.expires_at,
            "decided_by": self.decided_by,
            "decided_at": self.decided_at,
            "response": self.response,
        })
    }
}

const REQUEST_COLS: &str = "request_id, conv_id, agent_id, client_req_id, kind, title, options, context, state, created_at, expires_at, decided_by, decided_at, response";

fn row_request(r: &AnyRow, cx: &crate::crypto::Crypto) -> sqlx::Result<Request> {
    let id: String = r.try_get("request_id")?;
    Ok(Request {
        title: dec(cx, r, "title", &format!("requests.title:{id}"))?,
        context: dec_json(cx, r, "context", &format!("requests.context:{id}"))?,
        response: dec_json(cx, r, "response", &format!("requests.response:{id}"))?,
        options: r
            .try_get::<Option<String>, _>("options")?
            .and_then(|o| serde_json::from_str(&o).ok()),
        request_id: id,
        conv_id: r.try_get("conv_id")?,
        agent_id: r.try_get("agent_id")?,
        client_req_id: r.try_get("client_req_id")?,
        kind: r.try_get("kind")?,
        state: r.try_get("state")?,
        created_at: r.try_get("created_at")?,
        expires_at: r.try_get("expires_at")?,
        decided_by: r.try_get("decided_by")?,
        decided_at: r.try_get("decided_at")?,
    })
}

/// A nullable JSON column.
fn json_col(r: &AnyRow, col: &str) -> sqlx::Result<Option<Value>> {
    Ok(r.try_get::<Option<String>, _>(col)?
        .and_then(|s| serde_json::from_str(&s).ok()))
}

/// Built-in login secrets of a user.
#[derive(Debug, Clone, Default)]
pub struct Credentials {
    pub password_hash: Option<String>,
    pub totp_secret: Option<String>,
    /// Set up but not yet confirmed with a code.
    pub totp_pending: Option<String>,
    /// Last accepted TOTP step: a code is never accepted twice.
    pub totp_last_step: Option<i64>,
    pub failures: i64,
    pub locked_until: Option<i64>,
}

/// An open API key (only its hash is stored).
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct ApiKeyRow {
    pub key_id: String,
    pub app_id: String,
    pub hash: String,
    pub name: String,
    pub scopes: Vec<String>,
    pub ip_allow: Option<Vec<String>>,
    pub created_at: i64,
    pub expires_at: Option<i64>,
    pub revoked_at: Option<i64>,
}

impl ApiKeyRow {
    pub fn view(&self) -> Value {
        json!({"key_id": self.key_id, "name": self.name, "scopes": self.scopes, "ip_allow": self.ip_allow, "created_at": self.created_at, "expires_at": self.expires_at, "revoked": self.revoked_at.is_some()})
    }
}

fn row_api_key(r: &AnyRow) -> sqlx::Result<ApiKeyRow> {
    Ok(ApiKeyRow {
        key_id: r.try_get("key_id")?,
        app_id: r.try_get("app_id")?,
        hash: r.try_get("hash")?,
        name: r.try_get("name")?,
        scopes: serde_json::from_str(&r.try_get::<String, _>("scopes")?).unwrap_or_default(),
        ip_allow: r
            .try_get::<Option<String>, _>("ip_allow")?
            .and_then(|s| serde_json::from_str(&s).ok()),
        created_at: r.try_get("created_at")?,
        expires_at: r.try_get("expires_at")?,
        revoked_at: r.try_get("revoked_at")?,
    })
}

/// A key developers sign client tokens with.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct SigningKeyRow {
    pub kid: String,
    pub name: String,
    /// `k:<wrapped>` (encrypted with the master key) or `p:<secret>`.
    pub secret: String,
    pub created_at: i64,
    pub revoked_at: Option<i64>,
}

/// One device: an agent key, or a person's device registered by the
/// first connection with a signed client token.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct TokenRow {
    pub hash: String,
    pub user_id: String,
    pub device_id: String,
    pub device_name: Option<String>,
    pub platform: Option<String>,
    pub issued_by: Option<String>,
    pub created_at: i64,
    pub expires_at: Option<i64>,
    pub last_seen: Option<i64>,
}

impl TokenRow {
    /// What a user or the API may see (never the hash).
    pub fn view(&self) -> Value {
        json!({"device_id": self.device_id, "name": self.device_name, "platform": self.platform, "created_at": self.created_at, "expires_at": self.expires_at, "last_seen": self.last_seen})
    }
}

fn row_token(r: &AnyRow) -> sqlx::Result<TokenRow> {
    Ok(TokenRow {
        hash: r.try_get("hash")?,
        user_id: r.try_get("user_id")?,
        device_id: r.try_get("device_id")?,
        device_name: r.try_get("device_name")?,
        platform: r.try_get("platform")?,
        issued_by: r.try_get("issued_by")?,
        created_at: r.try_get("created_at")?,
        expires_at: r.try_get("expires_at")?,
        last_seen: r.try_get("last_seen")?,
    })
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct DirEntry {
    pub conv_id: String,
    pub database: String,
    pub unit_id: String,
    pub migrating: bool,
}

pub struct NewMsg<'a> {
    pub conv_id: &'a str,
    pub sender: &'a User,
    pub client_msg_id: Option<&'a str>,
    pub on_behalf_of: Option<String>,
    pub kind: &'a str,
    pub content: String,
    pub payload: Option<Value>,
    pub state: &'a str,
    pub hop: i64,
    pub causation_id: Option<String>,
    pub reply_to: Option<String>,
    pub mentions: Vec<String>,
    /// Who approved a draft and whether they edited it (D36): the message
    /// stays the agent's, the event records the approval.
    pub approval: Option<Value>,
    /// Routing envelope (docs/03-agents.md §6).
    pub envelope: Option<Value>,
    /// Source of the chain; None: this message starts one.
    pub root_id: Option<String>,
    /// Agents this message wakes, and how many more it would have woken
    /// past the fan-out cap; recorded in the event.
    pub wake: Option<(Vec<String>, usize)>,
}

#[allow(clippy::large_enum_variant)]
pub enum Committed {
    New(Event),
    /// Retry of an already committed client_msg_id: the original result.
    Duplicate {
        seq: i64,
        msg_id: String,
    },
}

/// One row of a per-conversation table, for migration between databases.
#[derive(Debug, Clone, PartialEq, serde::Deserialize, serde::Serialize)]
pub enum Cell {
    S(Option<String>),
    I(Option<i64>),
}

/// Meta key counting token deletions and deactivations.
pub const AUTH_EPOCH: &str = "auth_epoch";

const MSG_COLS: &str = "msg_id, conv_id, created_seq, sender_id, sender_name, sender_type, on_behalf_of, kind, content,
    version, state, state_reason, hop, causation_id, reply_to, mentions, created_at, updated_at, payload, envelope, root_id";
const MEMBER_COLS: &str = "user_id, name, kind, owner_id, role, trigger_mode, approval, join_seq, visible_from, mute_mode, mute_until, topics, silenced_until";
const DRAFT_COLS: &str = "draft_id, conv_id, agent_id, content, hop, causation_id, reply_to, state, created_at, kind, payload, envelope";

fn row_user(r: &AnyRow) -> sqlx::Result<User> {
    Ok(User {
        id: r.try_get("id")?,
        name: r.try_get("name")?,
        kind: UserKind::parse(&r.try_get::<String, _>("kind")?),
        owner_id: r.try_get("owner_id")?,
        unit_id: r.try_get("unit_id")?,
        team: r.try_get::<Option<String>, _>("agent_scope")?.as_deref() == Some("team"),
    })
}

fn row_msg(r: &AnyRow, cx: &crate::crypto::Crypto) -> sqlx::Result<MsgView> {
    let id: String = r.try_get("msg_id")?;
    Ok(MsgView {
        content: dec(cx, r, "content", &format!("messages.content:{id}"))?,
        payload: dec_json(cx, r, "payload", &format!("messages.payload:{id}"))?,
        envelope: dec_json(cx, r, "envelope", &format!("messages.envelope:{id}"))?,
        root_id: r.try_get("root_id")?,
        client_msg_id: None,
        msg_id: id,
        conv_id: r.try_get("conv_id")?,
        created_seq: r.try_get("created_seq")?,
        sender_id: r.try_get("sender_id")?,
        sender_name: r.try_get("sender_name")?,
        sender_type: UserKind::parse(&r.try_get::<String, _>("sender_type")?),
        on_behalf_of: r.try_get("on_behalf_of")?,
        kind: r.try_get("kind")?,
        version: r.try_get("version")?,
        state: r.try_get("state")?,
        state_reason: r.try_get("state_reason")?,
        hop: r.try_get("hop")?,
        causation_id: r.try_get("causation_id")?,
        reply_to: r.try_get("reply_to")?,
        mentions: serde_json::from_str(&r.try_get::<String, _>("mentions")?).unwrap_or_default(),
        created_at: r.try_get("created_at")?,
        updated_at: r.try_get("updated_at")?,
    })
}

fn row_member(r: &AnyRow) -> sqlx::Result<Member> {
    Ok(Member {
        user_id: r.try_get("user_id")?,
        name: r.try_get("name")?,
        kind: UserKind::parse(&r.try_get::<String, _>("kind")?),
        owner_id: r.try_get("owner_id")?,
        role: r.try_get("role")?,
        trigger: Trigger::parse(&r.try_get::<String, _>("trigger_mode")?),
        approval: r.try_get::<i64, _>("approval")? != 0,
        join_seq: r.try_get("join_seq")?,
        visible_from: match r.try_get::<Option<i64>, _>("visible_from")? {
            Some(v) => v,
            // Before v3 people saw the whole history, agents from joining.
            None if UserKind::parse(&r.try_get::<String, _>("kind")?) == UserKind::Agent => {
                r.try_get("join_seq")?
            }
            None => 1,
        },
        mute_mode: r
            .try_get::<Option<String>, _>("mute_mode")?
            .unwrap_or_else(|| "none".into()),
        mute_until: r.try_get("mute_until")?,
        topics: r
            .try_get::<Option<String>, _>("topics")?
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default(),
        silenced_until: r.try_get("silenced_until")?,
    })
}

fn row_event(r: &AnyRow, cx: &crate::crypto::Crypto) -> sqlx::Result<Event> {
    let conv: String = r.try_get("conv_id")?;
    let seq: i64 = r.try_get("seq")?;
    let payload: Value =
        dec_json(cx, r, "payload", &format!("events.payload:{conv}:{seq}"))?.unwrap_or(Value::Null);
    Ok(Event {
        conv_id: conv,
        seq,
        kind: r.try_get("type")?,
        actor_id: r.try_get("actor_id")?,
        msg: serde_json::from_value(payload["msg"].clone()).ok(),
        data: payload["data"].clone(),
        created_at: r.try_get("created_at")?,
    })
}

fn row_side(r: &AnyRow, cx: &crate::crypto::Crypto) -> sqlx::Result<SideEvent> {
    let conv: String = r.try_get("conv_id")?;
    let side_seq: i64 = r.try_get("side_seq")?;
    Ok(SideEvent {
        data: dec_json(
            cx,
            r,
            "payload",
            &format!("side_events.payload:{conv}:{side_seq}"),
        )?
        .unwrap_or(Value::Null),
        conv_id: conv,
        side_seq,
        anchor_seq: r.try_get("anchor_seq")?,
        audience: serde_json::from_str(&r.try_get::<String, _>("audience")?).unwrap_or_default(),
        kind: r.try_get("type")?,
        actor_id: r.try_get("actor_id")?,
        created_at: r.try_get("created_at")?,
    })
}

fn row_draft(r: &AnyRow, cx: &crate::crypto::Crypto) -> sqlx::Result<Draft> {
    let id: String = r.try_get("draft_id")?;
    Ok(Draft {
        content: dec(cx, r, "content", &format!("drafts.content:{id}"))?,
        payload: dec_json(cx, r, "payload", &format!("drafts.payload:{id}"))?,
        envelope: dec_json(cx, r, "envelope", &format!("drafts.envelope:{id}"))?,
        draft_id: id,
        conv_id: r.try_get("conv_id")?,
        agent_id: r.try_get("agent_id")?,
        hop: r.try_get("hop")?,
        causation_id: r.try_get("causation_id")?,
        reply_to: r.try_get("reply_to")?,
        state: r.try_get("state")?,
        created_at: r.try_get("created_at")?,
        kind: r
            .try_get::<Option<String>, _>("kind")?
            .unwrap_or_else(|| "text".into()),
    })
}

impl Db<'_> {
    // ========================================================= encryption

    /// The data key for new content of a scope (a conversation, or "" for
    /// the database), creating it in this transaction when needed.
    async fn key_for(&mut self, scope: &str) -> ApiResult<String> {
        let period = crate::crypto::period(now_ms());
        if let Some(k) = self.cx.current(scope, period) {
            return Ok(k);
        }
        let r = sqlx::query(self.d.q("SELECT key_id, wrapped FROM data_keys WHERE conv_id = ? AND period = ? ORDER BY created_at LIMIT 1"))
            .bind(scope)
            .bind(period)
            .fetch_optional(&mut *self.c)
            .await?;
        if let Some(r) = r {
            let (id, w): (String, String) = (r.try_get(0)?, r.try_get(1)?);
            self.cx.load(&id, &w)?;
            self.cx.set_current(scope, period, &id);
            return Ok(id);
        }
        let (id, wrapped) = self.cx.create(scope, period)?;
        sqlx::query(self.d.q("INSERT INTO data_keys (key_id, conv_id, period, wrapped, created_at) VALUES (?, ?, ?, ?, ?)"))
            .bind(&id)
            .bind(scope)
            .bind(period)
            .bind(wrapped)
            .bind(now_ms())
            .execute(&mut *self.c)
            .await?;
        Ok(id)
    }

    /// Deletes a scope's data keys, also from the cache: what they sealed
    /// can no longer be read (crypto-shredding).
    pub async fn shred_scope(&mut self, scope: &str) -> ApiResult<()> {
        let ids: Vec<String> =
            sqlx::query(self.d.q("SELECT key_id FROM data_keys WHERE conv_id = ?"))
                .bind(scope)
                .fetch_all(&mut *self.c)
                .await?
                .iter()
                .map(|r| r.try_get(0))
                .collect::<sqlx::Result<_>>()?;
        sqlx::query(self.d.q("DELETE FROM data_keys WHERE conv_id = ?"))
            .bind(scope)
            .execute(&mut *self.c)
            .await?;
        self.cx.drop_scope(scope);
        self.cx.evict(&ids);
        Ok(())
    }

    /// Encrypts content (empty strings stay empty; off = plaintext).
    pub async fn seal(&mut self, scope: &str, aad: &str, plain: &str) -> ApiResult<String> {
        if plain.is_empty() || !self.cx.enabled() {
            return Ok(crate::crypto::store_plain(plain).into_owned());
        }
        let k = self.key_for(scope).await?;
        self.cx.seal(&k, aad, plain)
    }

    async fn seal_json(
        &mut self,
        scope: &str,
        aad: &str,
        v: Option<&Value>,
    ) -> ApiResult<Option<String>> {
        match v {
            None => Ok(None),
            Some(v) => Ok(Some(self.seal(scope, aad, &v.to_string()).await?)),
        }
    }

    /// Loads the data keys the given rows' columns are encrypted with.
    pub async fn preload(&mut self, rows: &[AnyRow], cols: &[&str]) -> ApiResult<()> {
        let mut need = std::collections::BTreeSet::new();
        for r in rows {
            for c in cols {
                if let Ok(Some(v)) = r.try_get::<Option<String>, _>(*c)
                    && let Some((k, _)) = crate::crypto::parse(&v)
                    && !self.cx.has_key(k)
                {
                    need.insert(k.to_string());
                }
            }
        }
        for k in need {
            let r = sqlx::query(self.d.q("SELECT wrapped FROM data_keys WHERE key_id = ?"))
                .bind(&k)
                .fetch_optional(&mut *self.c)
                .await?;
            // A value naming a key this database never had is plaintext.
            if let Some(r) = r {
                self.cx.load(&k, &r.try_get::<String, _>(0)?)?;
            }
        }
        Ok(())
    }

    /// Encrypts up to `limit` plaintext values of one table (written before
    /// encryption was on). Returns how many rows changed.
    pub async fn encrypt_existing(&mut self, table: &str, limit: i64) -> ApiResult<u64> {
        if !self.cx.enabled() {
            return Err(ApiError::new("conflict", "encryption is off"));
        }
        self.recode(table, limit, false).await
    }

    /// Turns up to `limit` encrypted values of one table back into
    /// plaintext (encryption turned off, B4). Needs the master key.
    pub async fn decrypt_existing(&mut self, table: &str, limit: i64) -> ApiResult<u64> {
        if self.cx.kek.is_none() {
            return Err(ApiError::new("conflict", "no master key to decrypt with"));
        }
        self.recode(table, limit, true).await
    }

    async fn recode(&mut self, table: &str, limit: i64, decrypt: bool) -> ApiResult<u64> {
        // (key columns, scope column or "", encrypted columns)
        let (keys, scope, cols): (&[&str], &str, &[&str]) = match table {
            "messages" => (&["msg_id"], "conv_id", &["content", "payload", "envelope"]),
            "events" => (&["conv_id", "seq"], "conv_id", &["payload"]),
            "side_events" => (&["conv_id", "side_seq"], "conv_id", &["payload"]),
            "drafts" => (
                &["draft_id"],
                "conv_id",
                &["content", "payload", "envelope"],
            ),
            "requests" => (
                &["request_id"],
                "conv_id",
                &["title", "context", "response"],
            ),
            "summaries" => (&["summary_id"], "conv_id", &["text"]),
            "takeovers" => (&["conv_id", "agent_id"], "conv_id", &["note"]),
            "feedback" => (&["msg_id", "user_id"], "conv_id", &["reason"]),
            "agent_state" => (
                &["agent_id", "state_key", "version"],
                "agent_id",
                &["value"],
            ),
            "audit" => (&["id"], "", &["detail"]),
            "webhook_outbox" => (&["id"], "", &["body"]),
            "reports" => (&["id"], "", &["body"]),
            _ => {
                return Err(ApiError::new(
                    "bad_request",
                    format!("unknown table {table}"),
                ));
            }
        };
        let todo = cols
            .iter()
            .map(|c| {
                if decrypt {
                    format!("({c} LIKE 'enc:v1:%')")
                } else {
                    format!("({c} IS NOT NULL AND {c} <> '' AND {c} NOT LIKE 'enc:v1:%')")
                }
            })
            .collect::<Vec<_>>()
            .join(" OR ");
        let mut select: Vec<&str> = keys.to_vec();
        if !scope.is_empty() && !keys.contains(&scope) {
            select.push(scope);
        }
        select.extend_from_slice(cols);
        let rows = sqlx::query(self.d.q(&format!(
            "SELECT {} FROM {table} WHERE {todo} LIMIT ?",
            select.join(", ")
        )))
        .bind(limit)
        .fetch_all(&mut *self.c)
        .await?;
        if decrypt {
            self.preload(&rows, cols).await?;
        }
        let mut n = 0;
        for r in rows {
            let key_vals: Vec<String> = keys
                .iter()
                .map(|k| {
                    r.try_get::<String, _>(*k)
                        .or_else(|_| r.try_get::<i64, _>(*k).map(|v| v.to_string()))
                })
                .collect::<sqlx::Result<_>>()?;
            let sc: String = if scope.is_empty() {
                String::new()
            } else {
                r.try_get(scope)?
            };
            let mut sets = vec![];
            let mut vals = vec![];
            for c in cols {
                let Some(v) = r.try_get::<Option<String>, _>(*c)? else {
                    continue;
                };
                let sealed = v.starts_with(crate::crypto::PREFIX);
                if v.is_empty() || sealed != decrypt {
                    continue;
                }
                let aad = format!("{table}.{c}:{}", key_vals.join(":"));
                if decrypt {
                    let plain = self
                        .cx
                        .open(&aad, &v)
                        .map_err(|e| ApiError::new("storage", e))?;
                    vals.push(crate::crypto::store_plain(&plain).into_owned());
                } else {
                    let plain = v.strip_prefix(crate::crypto::RAW).unwrap_or(&v).to_string();
                    vals.push(self.seal(&sc, &aad, &plain).await?);
                }
                sets.push(format!("{c} = ?"));
            }
            let wh = keys
                .iter()
                .map(|k| format!("{k} = ?"))
                .collect::<Vec<_>>()
                .join(" AND ");
            let sql = format!("UPDATE {table} SET {} WHERE {wh}", sets.join(", "));
            let mut q = sqlx::query(self.d.q(&sql));
            for v in vals {
                q = q.bind(v);
            }
            for (k, v) in keys.iter().zip(&key_vals) {
                q = if *k == "seq"
                    || *k == "side_seq"
                    || *k == "version"
                    || (table == "webhook_outbox" && *k == "id")
                {
                    q.bind(v.parse::<i64>().unwrap_or(0))
                } else {
                    q.bind(v.clone())
                };
            }
            q.execute(&mut *self.c).await?;
            n += 1;
        }
        Ok(n)
    }

    /// Re-wraps every data key with a new master key; content is untouched.
    pub async fn rewrap_keys(&mut self, new: &crate::crypto::Kek) -> ApiResult<u64> {
        let old = self
            .cx
            .kek
            .as_ref()
            .ok_or_else(|| ApiError::new("conflict", "encryption is off"))?;
        let rows = sqlx::query("SELECT key_id, wrapped FROM data_keys")
            .fetch_all(&mut *self.c)
            .await?;
        let mut n = 0;
        for r in rows {
            let (id, w): (String, String) = (r.try_get(0)?, r.try_get(1)?);
            let dek = old
                .unwrap(&id, &w)
                .ok_or_else(|| ApiError::new("storage", format!("data key {id} does not open")))?;
            sqlx::query(
                self.d
                    .q("UPDATE data_keys SET wrapped = ? WHERE key_id = ?"),
            )
            .bind(new.wrap(&id, &dek))
            .bind(&id)
            .execute(&mut *self.c)
            .await?;
            n += 1;
        }
        // Signing secrets of client tokens are wrapped by the master key
        // too (docs/13 §4.2).
        for k in self.signing_keys().await? {
            let w = crate::hub::login::rewrap_secret(self.cx, new, &k.kid, &k.secret).ok_or_else(
                || ApiError::new("storage", format!("signing key {} does not open", k.kid)),
            )?;
            self.set_signing_secret(&k.kid, &w).await?;
            n += 1;
        }
        sqlx::query(self.d.q("UPDATE meta SET v = ? WHERE k = ?"))
            .bind(new.fingerprint.clone())
            .bind("kek_fingerprint")
            .execute(&mut *self.c)
            .await?;
        Ok(n)
    }

    // ===================================================== control database

    pub async fn insert_user(&mut self, u: &User) -> ApiResult<()> {
        let token = crate::auth::legacy_placeholder(&u.id);
        sqlx::query(self.d.q(
            "INSERT INTO users (id, tenant_id, unit_id, name, kind, owner_id, token, created_at, agent_scope) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        ))
        .bind(&u.id)
        .bind(TENANT_DEFAULT)
        .bind(&u.unit_id)
        .bind(&u.name)
        .bind(u.kind.as_str())
        .bind(&u.owner_id)
        .bind(&token)
        .bind(now_ms())
        .bind(u.team.then_some("team"))
        .execute(&mut *self.c)
        .await
        .map_err(|e| ApiError::new("conflict", e.to_string()))?;
        Ok(())
    }

    /// Every person's permission overrides (docs/13 §4.1).
    pub async fn permissions_all(&mut self) -> ApiResult<Vec<(String, Overrides)>> {
        let rows = sqlx::query("SELECT id, permissions FROM users WHERE permissions IS NOT NULL")
            .fetch_all(&mut *self.c)
            .await?;
        let mut out = vec![];
        for r in rows {
            let id: String = r.try_get(0)?;
            let p: String = r.try_get(1)?;
            if let Ok(o) = serde_json::from_str(&p) {
                out.push((id, o));
            }
        }
        Ok(out)
    }

    pub async fn permissions(&mut self, id: &str) -> ApiResult<Overrides> {
        let r = sqlx::query(self.d.q("SELECT permissions FROM users WHERE id = ?"))
            .bind(id)
            .fetch_optional(&mut *self.c)
            .await?;
        Ok(
            r.and_then(|r| r.try_get::<Option<String>, _>(0).ok().flatten())
                .and_then(|p| serde_json::from_str(&p).ok())
                .unwrap_or_default(),
        )
    }

    pub async fn set_permissions(&mut self, id: &str, o: &Overrides) -> ApiResult<()> {
        let v = (!o.is_empty()).then(|| json!(o).to_string());
        sqlx::query(self.d.q("UPDATE users SET permissions = ? WHERE id = ?"))
            .bind(v)
            .bind(id)
            .execute(&mut *self.c)
            .await?;
        Ok(())
    }

    /// All users and whether each is deactivated (the in-memory directory).
    pub async fn load_users(&mut self) -> ApiResult<Vec<(User, bool)>> {
        let rows = sqlx::query(
            "SELECT id, name, kind, owner_id, unit_id, agent_scope, deactivated_at FROM users ORDER BY created_at, id",
        )
        .fetch_all(&mut *self.c)
        .await?;
        rows.iter()
            .map(|r| {
                Ok((
                    row_user(r)?,
                    r.try_get::<Option<i64>, _>("deactivated_at")?.is_some(),
                ))
            })
            .collect::<sqlx::Result<_>>()
            .map_err(Into::into)
    }

    pub async fn insert_token(&mut self, t: &TokenRow) -> ApiResult<()> {
        sqlx::query(self.d.q("INSERT INTO tokens (hash, user_id, device_id, device_name, platform, issued_by, created_at, expires_at, last_seen) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)"))
            .bind(&t.hash)
            .bind(&t.user_id)
            .bind(&t.device_id)
            .bind(&t.device_name)
            .bind(&t.platform)
            .bind(&t.issued_by)
            .bind(t.created_at)
            .bind(t.expires_at)
            .bind(t.last_seen)
            .execute(&mut *self.c)
            .await?;
        Ok(())
    }

    pub async fn delete_tokens(
        &mut self,
        user_id: &str,
        device: Option<&str>,
    ) -> ApiResult<Vec<String>> {
        let rows = match device {
            Some(d) => {
                sqlx::query(
                    self.d
                        .q("SELECT hash FROM tokens WHERE user_id = ? AND device_id = ?"),
                )
                .bind(user_id)
                .bind(d)
                .fetch_all(&mut *self.c)
                .await?
            }
            None => {
                sqlx::query(self.d.q("SELECT hash FROM tokens WHERE user_id = ?"))
                    .bind(user_id)
                    .fetch_all(&mut *self.c)
                    .await?
            }
        };
        let hashes: Vec<String> = rows
            .iter()
            .map(|r| r.try_get(0))
            .collect::<sqlx::Result<_>>()?;
        if !hashes.is_empty() {
            self.bump_meta(AUTH_EPOCH).await?;
        }
        for h in &hashes {
            sqlx::query(self.d.q("DELETE FROM tokens WHERE hash = ?"))
                .bind(h)
                .execute(&mut *self.c)
                .await?;
        }
        Ok(hashes)
    }

    pub async fn credentials(&mut self, user_id: &str) -> ApiResult<Option<Credentials>> {
        let r = sqlx::query(self.d.q("SELECT password_hash, totp_secret, totp_pending, totp_last_step, failures, locked_until FROM credentials WHERE user_id = ?"))
            .bind(user_id)
            .fetch_optional(&mut *self.c)
            .await?;
        Ok(match r {
            Some(r) => Some(Credentials {
                password_hash: r.try_get(0)?,
                totp_secret: r.try_get(1)?,
                totp_pending: r.try_get(2)?,
                totp_last_step: r.try_get(3)?,
                failures: r.try_get(4)?,
                locked_until: r.try_get(5)?,
            }),
            None => None,
        })
    }

    pub async fn put_credentials(&mut self, user_id: &str, c: &Credentials) -> ApiResult<()> {
        sqlx::query(self.d.q("DELETE FROM credentials WHERE user_id = ?"))
            .bind(user_id)
            .execute(&mut *self.c)
            .await?;
        sqlx::query(self.d.q("INSERT INTO credentials (user_id, password_hash, totp_secret, totp_pending, totp_last_step, failures, locked_until, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)"))
            .bind(user_id)
            .bind(&c.password_hash)
            .bind(&c.totp_secret)
            .bind(&c.totp_pending)
            .bind(c.totp_last_step)
            .bind(c.failures)
            .bind(c.locked_until)
            .bind(now_ms())
            .execute(&mut *self.c)
            .await?;
        Ok(())
    }

    pub async fn insert_api_key(&mut self, k: &ApiKeyRow) -> ApiResult<()> {
        sqlx::query(self.d.q("INSERT INTO api_keys (key_id, app_id, hash, name, scopes, ip_allow, created_at, expires_at, revoked_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)"))
            .bind(&k.key_id)
            .bind(&k.app_id)
            .bind(&k.hash)
            .bind(&k.name)
            .bind(json!(k.scopes).to_string())
            .bind(k.ip_allow.as_ref().map(|v| json!(v).to_string()))
            .bind(k.created_at)
            .bind(k.expires_at)
            .bind(k.revoked_at)
            .execute(&mut *self.c)
            .await?;
        Ok(())
    }

    pub async fn revoke_api_key(&mut self, key_id: &str) -> ApiResult<u64> {
        let r = sqlx::query(
            self.d
                .q("UPDATE api_keys SET revoked_at = ? WHERE key_id = ? AND revoked_at IS NULL"),
        )
        .bind(now_ms())
        .bind(key_id)
        .execute(&mut *self.c)
        .await?;
        Ok(r.rows_affected())
    }

    pub async fn api_keys(&mut self) -> ApiResult<Vec<ApiKeyRow>> {
        let rows = sqlx::query("SELECT key_id, app_id, hash, name, scopes, ip_allow, created_at, expires_at, revoked_at FROM api_keys ORDER BY created_at, key_id")
            .fetch_all(&mut *self.c)
            .await?;
        rows.iter()
            .map(row_api_key)
            .collect::<sqlx::Result<_>>()
            .map_err(Into::into)
    }

    /// Stores an Idempotency-Key result; drops results older than a day.
    pub async fn idem_put(
        &mut self,
        key_id: &str,
        idem_key: &str,
        request_hash: &str,
        status: i64,
        response: &str,
    ) -> ApiResult<()> {
        let now = now_ms();
        sqlx::query(self.d.q("DELETE FROM idempotency WHERE created_at < ?"))
            .bind(now - 24 * 3600 * 1000)
            .execute(&mut *self.c)
            .await?;
        sqlx::query(
            self.d
                .q("DELETE FROM idempotency WHERE key_id = ? AND idem_key = ?"),
        )
        .bind(key_id)
        .bind(idem_key)
        .execute(&mut *self.c)
        .await?;
        sqlx::query(self.d.q("INSERT INTO idempotency (key_id, idem_key, request_hash, status, response, created_at) VALUES (?, ?, ?, ?, ?, ?)"))
            .bind(key_id)
            .bind(idem_key)
            .bind(request_hash)
            .bind(status)
            .bind(response)
            .bind(now)
            .execute(&mut *self.c)
            .await?;
        Ok(())
    }

    /// `(request_hash, status, response)` of a stored result younger than a day.
    pub async fn idem_get(
        &mut self,
        key_id: &str,
        idem_key: &str,
    ) -> ApiResult<Option<(String, i64, String)>> {
        let r = sqlx::query(self.d.q("SELECT request_hash, status, response FROM idempotency WHERE key_id = ? AND idem_key = ? AND created_at >= ?"))
            .bind(key_id)
            .bind(idem_key)
            .bind(now_ms() - 24 * 3600 * 1000)
            .fetch_optional(&mut *self.c)
            .await?;
        Ok(match r {
            Some(r) => Some((r.try_get(0)?, r.try_get(1)?, r.try_get(2)?)),
            None => None,
        })
    }

    /// People's devices unused since `idle_before`: deleted, returned as
    /// (hash, user, device).
    pub async fn expire_tokens(
        &mut self,
        idle_before: i64,
    ) -> ApiResult<Vec<(String, String, String)>> {
        let rows = sqlx::query(self.d.q(
            "SELECT t.hash, t.user_id, t.device_id FROM tokens t JOIN users u ON u.id = t.user_id
             WHERE u.kind = 'human' AND COALESCE(t.last_seen, t.created_at) < ?",
        ))
        .bind(idle_before)
        .fetch_all(&mut *self.c)
        .await?;
        let gone: Vec<(String, String, String)> = rows
            .iter()
            .map(|r| Ok((r.try_get(0)?, r.try_get(1)?, r.try_get(2)?)))
            .collect::<sqlx::Result<_>>()?;
        for (h, _, _) in &gone {
            sqlx::query(self.d.q("DELETE FROM tokens WHERE hash = ?"))
                .bind(h)
                .execute(&mut *self.c)
                .await?;
        }
        if !gone.is_empty() {
            self.bump_meta(AUTH_EPOCH).await?;
        }
        Ok(gone)
    }

    pub async fn touch_token(&mut self, hash: &str) -> ApiResult<()> {
        sqlx::query(self.d.q("UPDATE tokens SET last_seen = ? WHERE hash = ?"))
            .bind(now_ms())
            .bind(hash)
            .execute(&mut *self.c)
            .await?;
        Ok(())
    }

    /// One token by hash (tokens issued by another process or node).
    pub async fn token_by_hash(&mut self, hash: &str) -> ApiResult<Option<TokenRow>> {
        let r = sqlx::query(self.d.q("SELECT hash, user_id, device_id, device_name, platform, issued_by, created_at, expires_at, last_seen FROM tokens WHERE hash = ?"))
            .bind(hash)
            .fetch_optional(&mut *self.c)
            .await?;
        Ok(r.as_ref().map(row_token).transpose()?)
    }

    /// One user and whether it is deactivated.
    pub async fn user_by_id(&mut self, id: &str) -> ApiResult<Option<(User, bool)>> {
        let r =
            sqlx::query(self.d.q(
                "SELECT id, name, kind, owner_id, unit_id, agent_scope, deactivated_at FROM users WHERE id = ?",
            ))
            .bind(id)
            .fetch_optional(&mut *self.c)
            .await?;
        let Some(r) = r else { return Ok(None) };
        Ok(Some((
            row_user(&r)?,
            r.try_get::<Option<i64>, _>("deactivated_at")?.is_some(),
        )))
    }

    /// One user by name and whether it is deactivated.
    pub async fn user_by_name(&mut self, name: &str) -> ApiResult<Option<(User, bool)>> {
        let r = sqlx::query(self.d.q(
            "SELECT id, name, kind, owner_id, unit_id, agent_scope, deactivated_at FROM users WHERE name = ?",
        ))
        .bind(name)
        .fetch_optional(&mut *self.c)
        .await?;
        let Some(r) = r else { return Ok(None) };
        Ok(Some((
            row_user(&r)?,
            r.try_get::<Option<i64>, _>("deactivated_at")?.is_some(),
        )))
    }

    /// Whether a user name is taken.
    pub async fn name_taken(&mut self, name: &str) -> ApiResult<bool> {
        Ok(sqlx::query(self.d.q("SELECT id FROM users WHERE name = ?"))
            .bind(name)
            .fetch_optional(&mut *self.c)
            .await?
            .is_some())
    }

    /// Agents a person owns.
    /// Moves an agent to another owner; returns the previous one.
    pub async fn set_agent_owner(
        &mut self,
        agent_id: &str,
        owner_id: &str,
    ) -> ApiResult<Option<String>> {
        let prev: Option<String> = sqlx::query(self.d.q("SELECT owner_id FROM users WHERE id = ?"))
            .bind(agent_id)
            .fetch_optional(&mut *self.c)
            .await?
            .map(|r| r.try_get(0))
            .transpose()?
            .flatten();
        sqlx::query(
            self.d
                .q("UPDATE users SET owner_id = ? WHERE id = ? AND kind = 'agent'"),
        )
        .bind(owner_id)
        .bind(agent_id)
        .execute(&mut *self.c)
        .await?;
        Ok(prev)
    }

    pub async fn set_member_owner(&mut self, agent_id: &str, owner_id: &str) -> ApiResult<()> {
        sqlx::query(
            self.d
                .q("UPDATE members SET owner_id = ? WHERE user_id = ?"),
        )
        .bind(owner_id)
        .bind(agent_id)
        .execute(&mut *self.c)
        .await?;
        Ok(())
    }

    pub async fn count_agents_of(&mut self, owner_id: &str) -> ApiResult<i64> {
        let r = sqlx::query(
            self.d
                .q("SELECT COUNT(*) FROM users WHERE owner_id = ? AND kind = 'agent' AND destroyed_at IS NULL"),
        )
        .bind(owner_id)
        .fetch_one(&mut *self.c)
        .await?;
        Ok(r.try_get::<i64, _>(0)?)
    }

    /// Ids of the agents a person owns (not destroyed).
    pub async fn agent_ids_of(&mut self, owner_id: &str) -> ApiResult<Vec<String>> {
        let rows = sqlx::query(self.d.q(
            "SELECT id FROM users WHERE owner_id = ? AND kind = 'agent' AND destroyed_at IS NULL",
        ))
        .bind(owner_id)
        .fetch_all(&mut *self.c)
        .await?;
        rows.iter()
            .map(|r| r.try_get::<String, _>(0).map_err(Into::into))
            .collect()
    }

    pub async fn tokens_all(&mut self) -> ApiResult<Vec<TokenRow>> {
        let rows = sqlx::query("SELECT hash, user_id, device_id, device_name, platform, issued_by, created_at, expires_at, last_seen FROM tokens")
            .fetch_all(&mut *self.c)
            .await?;
        rows.iter()
            .map(row_token)
            .collect::<sqlx::Result<_>>()
            .map_err(Into::into)
    }

    pub async fn devices(&mut self, user_id: &str) -> ApiResult<Vec<TokenRow>> {
        let rows = sqlx::query(self.d.q("SELECT hash, user_id, device_id, device_name, platform, issued_by, created_at, expires_at, last_seen FROM tokens WHERE user_id = ? ORDER BY created_at, device_id"))
            .bind(user_id)
            .fetch_all(&mut *self.c)
            .await?;
        rows.iter()
            .map(row_token)
            .collect::<sqlx::Result<_>>()
            .map_err(Into::into)
    }

    // ---- client token signing keys and cutoffs (docs/13 §4.2)

    pub async fn insert_signing_key(&mut self, k: &SigningKeyRow) -> ApiResult<()> {
        sqlx::query(self.d.q("INSERT INTO signing_keys (kid, name, secret, created_at, revoked_at) VALUES (?, ?, ?, ?, ?)"))
            .bind(&k.kid)
            .bind(&k.name)
            .bind(&k.secret)
            .bind(k.created_at)
            .bind(k.revoked_at)
            .execute(&mut *self.c)
            .await?;
        self.bump_meta(AUTH_EPOCH).await?;
        Ok(())
    }

    /// Revokes a key; false when unknown or already revoked.
    pub async fn revoke_signing_key(&mut self, kid: &str) -> ApiResult<bool> {
        let r = sqlx::query(
            self.d
                .q("UPDATE signing_keys SET revoked_at = ? WHERE kid = ? AND revoked_at IS NULL"),
        )
        .bind(now_ms())
        .bind(kid)
        .execute(&mut *self.c)
        .await?;
        if r.rows_affected() > 0 {
            self.bump_meta(AUTH_EPOCH).await?;
        }
        Ok(r.rows_affected() > 0)
    }

    pub async fn signing_keys(&mut self) -> ApiResult<Vec<SigningKeyRow>> {
        let rows = sqlx::query(
            "SELECT kid, name, secret, created_at, revoked_at FROM signing_keys ORDER BY created_at, kid",
        )
        .fetch_all(&mut *self.c)
        .await?;
        rows.iter()
            .map(|r| {
                Ok(SigningKeyRow {
                    kid: r.try_get(0)?,
                    name: r.try_get(1)?,
                    secret: r.try_get(2)?,
                    created_at: r.try_get(3)?,
                    revoked_at: r.try_get(4)?,
                })
            })
            .collect::<sqlx::Result<_>>()
            .map_err(Into::into)
    }

    /// Replaces the secrets of all keys (master key rotation).
    pub async fn set_signing_secret(&mut self, kid: &str, secret: &str) -> ApiResult<()> {
        sqlx::query(self.d.q("UPDATE signing_keys SET secret = ? WHERE kid = ?"))
            .bind(secret)
            .bind(kid)
            .execute(&mut *self.c)
            .await?;
        Ok(())
    }

    /// Tokens of the user (device "" = every device) issued before `at`
    /// seconds are refused from now on.
    pub async fn set_cutoff(&mut self, user_id: &str, device_id: &str, at: i64) -> ApiResult<()> {
        sqlx::query(
            self.d
                .q("DELETE FROM token_cutoffs WHERE user_id = ? AND device_id = ?"),
        )
        .bind(user_id)
        .bind(device_id)
        .execute(&mut *self.c)
        .await?;
        sqlx::query(
            self.d
                .q("INSERT INTO token_cutoffs (user_id, device_id, at) VALUES (?, ?, ?)"),
        )
        .bind(user_id)
        .bind(device_id)
        .bind(at)
        .execute(&mut *self.c)
        .await?;
        self.bump_meta(AUTH_EPOCH).await?;
        Ok(())
    }

    pub async fn cutoffs(&mut self) -> ApiResult<Vec<(String, String, i64)>> {
        let rows = sqlx::query("SELECT user_id, device_id, at FROM token_cutoffs")
            .fetch_all(&mut *self.c)
            .await?;
        rows.iter()
            .map(|r| Ok((r.try_get(0)?, r.try_get(1)?, r.try_get(2)?)))
            .collect::<sqlx::Result<_>>()
            .map_err(Into::into)
    }

    /// Records the key a device last connected with (`issued_by`).
    pub async fn set_token_issuer(&mut self, hash: &str, issued_by: &str) -> ApiResult<()> {
        sqlx::query(self.d.q("UPDATE tokens SET issued_by = ? WHERE hash = ?"))
            .bind(issued_by)
            .bind(hash)
            .execute(&mut *self.c)
            .await?;
        Ok(())
    }

    pub async fn upsert_unit(&mut self, name: &str, database: &str) -> ApiResult<()> {
        let exists = sqlx::query(self.d.q("SELECT id FROM units WHERE name = ?"))
            .bind(name)
            .fetch_optional(&mut *self.c)
            .await?;
        if exists.is_some() {
            sqlx::query(
                self.d
                    .q("UPDATE units SET database_name = ? WHERE name = ?"),
            )
            .bind(database)
            .bind(name)
            .execute(&mut *self.c)
            .await?;
        } else {
            sqlx::query(
                self.d.q(
                    "INSERT INTO units (id, name, database_name, created_at) VALUES (?, ?, ?, ?)",
                ),
            )
            .bind(name)
            .bind(name)
            .bind(database)
            .bind(now_ms())
            .execute(&mut *self.c)
            .await?;
        }
        Ok(())
    }

    pub async fn dir_all(&mut self) -> ApiResult<Vec<DirEntry>> {
        let rows = sqlx::query("SELECT conv_id, database_name, unit_id, migrating FROM conv_dir")
            .fetch_all(&mut *self.c)
            .await?;
        rows.iter()
            .map(|r| {
                Ok(DirEntry {
                    conv_id: r.try_get("conv_id")?,
                    database: r.try_get("database_name")?,
                    unit_id: r.try_get("unit_id")?,
                    migrating: r.try_get::<i64, _>("migrating")? != 0,
                })
            })
            .collect::<sqlx::Result<_>>()
            .map_err(Into::into)
    }

    pub async fn dir_one(&mut self, conv_id: &str) -> ApiResult<Option<DirEntry>> {
        let r = sqlx::query(self.d.q(
            "SELECT conv_id, database_name, unit_id, migrating FROM conv_dir WHERE conv_id = ?",
        ))
        .bind(conv_id)
        .fetch_optional(&mut *self.c)
        .await?;
        r.map(|r| {
            Ok(DirEntry {
                conv_id: r.try_get("conv_id")?,
                database: r.try_get("database_name")?,
                unit_id: r.try_get("unit_id")?,
                migrating: r.try_get::<i64, _>("migrating")? != 0,
            })
        })
        .transpose()
        .map_err(|e: sqlx::Error| e.into())
    }

    pub async fn dir_put(&mut self, e: &DirEntry) -> ApiResult<()> {
        let n = sqlx::query(self.d.q(
            "UPDATE conv_dir SET database_name = ?, unit_id = ?, migrating = ? WHERE conv_id = ?",
        ))
        .bind(&e.database)
        .bind(&e.unit_id)
        .bind(e.migrating as i64)
        .bind(&e.conv_id)
        .execute(&mut *self.c)
        .await?
        .rows_affected();
        if n == 0 {
            sqlx::query(self.d.q("INSERT INTO conv_dir (conv_id, database_name, unit_id, migrating, created_at) VALUES (?, ?, ?, ?, ?)"))
                .bind(&e.conv_id)
                .bind(&e.database)
                .bind(&e.unit_id)
                .bind(e.migrating as i64)
                .bind(now_ms())
                .execute(&mut *self.c)
                .await?;
        }
        Ok(())
    }

    pub async fn dir_delete(&mut self, conv_id: &str) -> ApiResult<()> {
        sqlx::query(self.d.q("DELETE FROM conv_dir WHERE conv_id = ?"))
            .bind(conv_id)
            .execute(&mut *self.c)
            .await?;
        Ok(())
    }

    pub async fn member_index_add(&mut self, conv_id: &str, user_id: &str) -> ApiResult<()> {
        let exists = sqlx::query(
            self.d
                .q("SELECT 1 FROM member_index WHERE conv_id = ? AND user_id = ?"),
        )
        .bind(conv_id)
        .bind(user_id)
        .fetch_optional(&mut *self.c)
        .await?;
        if exists.is_none() {
            sqlx::query(
                self.d
                    .q("INSERT INTO member_index (conv_id, user_id) VALUES (?, ?)"),
            )
            .bind(conv_id)
            .bind(user_id)
            .execute(&mut *self.c)
            .await?;
        }
        Ok(())
    }

    /// (conv_id, user_id) pairs for any of the given users.
    pub async fn member_index_for(
        &mut self,
        user_ids: &[String],
    ) -> ApiResult<Vec<(String, String)>> {
        let mut out = vec![];
        for u in user_ids {
            let rows = sqlx::query(
                self.d
                    .q("SELECT conv_id, user_id FROM member_index WHERE user_id = ?"),
            )
            .bind(u)
            .fetch_all(&mut *self.c)
            .await?;
            for r in rows {
                out.push((r.try_get("conv_id")?, r.try_get("user_id")?));
            }
        }
        Ok(out)
    }

    /// Who the index says is in a conversation.
    pub async fn member_index_of(&mut self, conv_id: &str) -> ApiResult<Vec<String>> {
        let rows = sqlx::query(
            self.d
                .q("SELECT user_id FROM member_index WHERE conv_id = ?"),
        )
        .bind(conv_id)
        .fetch_all(&mut *self.c)
        .await?;
        rows.iter().map(|r| Ok(r.try_get(0)?)).collect()
    }

    pub async fn member_index_remove(&mut self, conv_id: &str, user_id: &str) -> ApiResult<()> {
        sqlx::query(
            self.d
                .q("DELETE FROM member_index WHERE conv_id = ? AND user_id = ?"),
        )
        .bind(conv_id)
        .bind(user_id)
        .execute(&mut *self.c)
        .await?;
        Ok(())
    }

    /// Removes everything the control database knows about a conversation.
    pub async fn forget_conv(&mut self, conv_id: &str) -> ApiResult<()> {
        for q in [
            "DELETE FROM member_index WHERE conv_id = ?",
            "DELETE FROM dm_index WHERE conv_id = ?",
            "DELETE FROM conv_dir WHERE conv_id = ?",
        ] {
            sqlx::query(self.d.q(q))
                .bind(conv_id)
                .execute(&mut *self.c)
                .await?;
        }
        Ok(())
    }

    /// Everyone who shares at least one conversation with the user.
    pub async fn contacts(&mut self, user_id: &str) -> ApiResult<Vec<String>> {
        let rows = sqlx::query(self.d.q(
            "SELECT DISTINCT b.user_id FROM member_index a JOIN member_index b ON a.conv_id = b.conv_id WHERE a.user_id = ?",
        ))
        .bind(user_id)
        .fetch_all(&mut *self.c)
        .await?;
        rows.iter()
            .map(|r| r.try_get::<String, _>(0))
            .collect::<sqlx::Result<_>>()
            .map_err(Into::into)
    }

    pub async fn set_block(&mut self, user_id: &str, blocked_id: &str, on: bool) -> ApiResult<()> {
        sqlx::query(
            self.d
                .q("DELETE FROM blocks WHERE user_id = ? AND blocked_id = ?"),
        )
        .bind(user_id)
        .bind(blocked_id)
        .execute(&mut *self.c)
        .await?;
        if on {
            sqlx::query(
                self.d
                    .q("INSERT INTO blocks (user_id, blocked_id, created_at) VALUES (?, ?, ?)"),
            )
            .bind(user_id)
            .bind(blocked_id)
            .bind(now_ms())
            .execute(&mut *self.c)
            .await?;
        }
        Ok(())
    }

    /// Whether `a` blocked `b`, as stored now.
    pub async fn is_blocked(&mut self, a: &str, b: &str) -> ApiResult<bool> {
        Ok(sqlx::query(
            self.d
                .q("SELECT 1 FROM blocks WHERE user_id = ? AND blocked_id = ?"),
        )
        .bind(a)
        .bind(b)
        .fetch_optional(&mut *self.c)
        .await?
        .is_some())
    }

    pub async fn blocks_all(&mut self) -> ApiResult<Vec<(String, String)>> {
        let rows = sqlx::query("SELECT user_id, blocked_id FROM blocks")
            .fetch_all(&mut *self.c)
            .await?;
        rows.iter()
            .map(|r| Ok((r.try_get(0)?, r.try_get(1)?)))
            .collect::<sqlx::Result<_>>()
            .map_err(Into::into)
    }

    pub async fn dm_get(&mut self, pair_key: &str) -> ApiResult<Option<String>> {
        let r = sqlx::query(self.d.q("SELECT conv_id FROM dm_index WHERE pair_key = ?"))
            .bind(pair_key)
            .fetch_optional(&mut *self.c)
            .await?;
        Ok(r.map(|r| r.try_get(0)).transpose()?)
    }

    /// Claims a pair's direct conversation for `conv_id`, unless another
    /// claimed it (being created, or existing; a dissolved one's claim is
    /// gone with it, a failed creation releases its claim): returns that
    /// one then.
    pub async fn dm_claim(&mut self, pair_key: &str, conv_id: &str) -> ApiResult<Option<String>> {
        if let Some(c) = self.dm_get(pair_key).await?
            && c != conv_id
        {
            return Ok(Some(c));
        }
        self.dm_put(pair_key, conv_id).await?;
        Ok(None)
    }

    /// Releases a claim made for a conversation that was not created.
    pub async fn dm_release(&mut self, pair_key: &str, conv_id: &str) -> ApiResult<()> {
        sqlx::query(
            self.d
                .q("DELETE FROM dm_index WHERE pair_key = ? AND conv_id = ?"),
        )
        .bind(pair_key)
        .bind(conv_id)
        .execute(&mut *self.c)
        .await?;
        Ok(())
    }

    pub async fn dm_put(&mut self, pair_key: &str, conv_id: &str) -> ApiResult<()> {
        sqlx::query(self.d.q("DELETE FROM dm_index WHERE pair_key = ?"))
            .bind(pair_key)
            .execute(&mut *self.c)
            .await?;
        sqlx::query(
            self.d
                .q("INSERT INTO dm_index (pair_key, conv_id) VALUES (?, ?)"),
        )
        .bind(pair_key)
        .bind(conv_id)
        .execute(&mut *self.c)
        .await?;
        Ok(())
    }

    // ============================================== conversation databases

    pub async fn conv(&mut self, id: &str) -> ApiResult<Option<ConvRow>> {
        let r = sqlx::query(self.d.q(
            "SELECT id, kind, title, unit_id, last_seq, confirmed_seq, fields FROM conversations WHERE id = ?",
        ))
        .bind(id)
        .fetch_optional(&mut *self.c)
        .await?;
        Ok(match r {
            Some(r) => Some(ConvRow {
                id: r.try_get("id")?,
                kind: r.try_get("kind")?,
                title: r.try_get("title")?,
                unit_id: r.try_get("unit_id")?,
                last_seq: {
                    let last: i64 = r.try_get("last_seq")?;
                    if self.cap {
                        let confirmed: Option<i64> = r.try_get("confirmed_seq")?;
                        last.min(confirmed.unwrap_or(0))
                    } else {
                        last
                    }
                },
                fields: json_col(&r, "fields")?,
            }),
            None => None,
        })
    }

    pub async fn conv_ids(&mut self) -> ApiResult<Vec<(String, String)>> {
        let rows = sqlx::query("SELECT id, unit_id FROM conversations")
            .fetch_all(&mut *self.c)
            .await?;
        rows.iter()
            .map(|r| Ok((r.try_get("id")?, r.try_get("unit_id")?)))
            .collect::<sqlx::Result<_>>()
            .map_err(Into::into)
    }

    /// The read cap (docs/11 §8.3); binds the conversation id once more.
    fn cap_sql(&self) -> &'static str {
        if self.cap {
            " AND seq <= (SELECT COALESCE(confirmed_seq, 0) FROM conversations WHERE id = ?)"
        } else {
            ""
        }
    }

    /// The replicas have these conversations up to these seqs (application
    /// wait, docs/11 §8.3). Never moves back.
    pub async fn set_confirmed(
        &mut self,
        upto: &std::collections::BTreeMap<String, i64>,
    ) -> ApiResult<()> {
        for (id, seq) in upto {
            sqlx::query(self.d.q(
                "UPDATE conversations SET confirmed_seq = ? WHERE id = ? AND (confirmed_seq IS NULL OR confirmed_seq < ?)",
            ))
            .bind(*seq)
            .bind(id)
            .bind(*seq)
            .execute(&mut *self.c)
            .await?;
        }
        Ok(())
    }

    /// Conversations from before schema v6 start replicated (start-up).
    pub async fn confirm_old(&mut self) -> ApiResult<u64> {
        Ok(sqlx::query(
            "UPDATE conversations SET confirmed_seq = last_seq WHERE confirmed_seq IS NULL",
        )
        .execute(&mut *self.c)
        .await?
        .rows_affected())
    }

    async fn next_seq(&mut self, conv_id: &str) -> ApiResult<i64> {
        // The new value goes first: MySQL assigns left to right.
        let n = sqlx::query(self.d.q(if self.confirm_now {
            "UPDATE conversations SET confirmed_seq = last_seq + 1, last_seq = last_seq + 1 WHERE id = ?"
        } else {
            "UPDATE conversations SET last_seq = last_seq + 1 WHERE id = ?"
        }))
        .bind(conv_id)
        .execute(&mut *self.c)
        .await?
        .rows_affected();
        if n == 0 {
            return Err(ApiError::new("not_found", "conversation"));
        }
        let r = sqlx::query(self.d.q("SELECT last_seq FROM conversations WHERE id = ?"))
            .bind(conv_id)
            .fetch_one(&mut *self.c)
            .await?;
        Ok(r.try_get(0)?)
    }

    #[allow(clippy::too_many_arguments)]
    async fn write_event(
        &mut self,
        conv_id: &str,
        seq: i64,
        kind: &str,
        actor: Option<&str>,
        msg: Option<MsgView>,
        data: Value,
        ts: i64,
    ) -> ApiResult<Event> {
        let payload = json!({"msg": msg, "data": data}).to_string();
        let payload = self
            .seal(
                conv_id,
                &format!("events.payload:{conv_id}:{seq}"),
                &payload,
            )
            .await?;
        sqlx::query(self.d.q(
            "INSERT INTO events (conv_id, seq, type, actor_id, msg_id, payload, created_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
        ))
        .bind(conv_id)
        .bind(seq)
        .bind(kind)
        .bind(actor)
        .bind(msg.as_ref().map(|m| m.msg_id.clone()))
        .bind(payload)
        .bind(ts)
        .execute(&mut *self.c)
        .await?;
        Ok(Event {
            conv_id: conv_id.to_string(),
            seq,
            kind: kind.to_string(),
            actor_id: actor.map(String::from),
            msg,
            data,
            created_at: ts,
        })
    }

    pub async fn append_event(
        &mut self,
        conv_id: &str,
        kind: &str,
        actor: Option<&str>,
        msg: Option<MsgView>,
        data: Value,
    ) -> ApiResult<Event> {
        let seq = self.next_seq(conv_id).await?;
        self.write_event(conv_id, seq, kind, actor, msg, data, now_ms())
            .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn create_conv(
        &mut self,
        id: &str,
        kind: &str,
        title: &str,
        unit_id: &str,
        creator: &User,
        members: &[User],
        manager: Option<&User>,
    ) -> ApiResult<Vec<Event>> {
        sqlx::query(self.d.q(
            "INSERT INTO conversations (id, tenant_id, unit_id, kind, title, last_seq, side_seq, created_at, confirmed_seq) VALUES (?, ?, ?, ?, ?, 0, 0, ?, 0)",
        ))
        .bind(id)
        .bind(TENANT_DEFAULT)
        .bind(unit_id)
        .bind(kind)
        .bind(title)
        .bind(now_ms())
        .execute(&mut *self.c)
        .await?;
        let mut events = vec![
            self.append_event(
                id,
                "conv.created",
                Some(&creator.id),
                None,
                json!({"kind": kind, "title": title}),
            )
            .await?,
        ];
        let mut all: Vec<&User> = vec![creator];
        all.extend(members.iter().filter(|m| m.id != creator.id));
        // A conversation an agent starts is owned by its person; the agent
        // manages it as an admin.
        let owner_id = manager.map(|m| m.id.as_str()).unwrap_or(&creator.id);
        let role = |u: &User| {
            if u.id == owner_id {
                "owner"
            } else if u.id == creator.id {
                "admin"
            } else {
                "member"
            }
        };
        // Members from the start see the whole conversation.
        if let [u] = all.as_slice() {
            events.push(
                self.add_member(id, kind, u, role(u), &creator.id, Some(1))
                    .await?,
            );
            return Ok(events);
        }
        // Several at once: one `members.joined` per JOIN_BATCH (D128), so a
        // large group is a few events rather than one per member. The order
        // is the member list's (owner, admins, members, then by id).
        all.sort_by_key(|u| (role_rank(role(u)), u.id.clone()));
        for chunk in all.chunks(JOIN_BATCH) {
            let entries: Vec<Value> = chunk
                .iter()
                .map(|u| member_entry(u, role(u), trigger_for(u, kind), Some(1)))
                .collect();
            let ev = self
                .append_event(
                    id,
                    "members.joined",
                    Some(&creator.id),
                    None,
                    json!({"members": entries}),
                )
                .await?;
            for u in chunk {
                self.insert_member(id, u, role(u), trigger_for(u, kind), ev.seq, 1)
                    .await?;
            }
            events.push(ev);
        }
        Ok(events)
    }

    pub async fn add_member(
        &mut self,
        conv_id: &str,
        conv_kind: &str,
        u: &User,
        role: &str,
        actor: &str,
        // Earlier events the member may see, from this seq (None: from joining).
        history_from: Option<i64>,
    ) -> ApiResult<Event> {
        if self.member(conv_id, &u.id).await?.is_some() {
            return Err(ApiError::new("conflict", "already a member"));
        }
        let trigger = trigger_for(u, conv_kind);
        let ev = self
            .append_event(
                conv_id,
                "member.joined",
                Some(actor),
                None,
                member_entry(u, role, trigger, history_from),
            )
            .await?;
        let visible_from = history_from.unwrap_or(ev.seq).min(ev.seq).max(1);
        self.insert_member(conv_id, u, role, trigger, ev.seq, visible_from)
            .await?;
        Ok(ev)
    }

    async fn insert_member(
        &mut self,
        conv_id: &str,
        u: &User,
        role: &str,
        trigger: Trigger,
        join_seq: i64,
        visible_from: i64,
    ) -> ApiResult<()> {
        sqlx::query(self.d.q(
            "INSERT INTO members (conv_id, user_id, name, kind, owner_id, role, trigger_mode, approval, join_seq, visible_from, mute_mode) VALUES (?, ?, ?, ?, ?, ?, ?, 0, ?, ?, 'none')",
        ))
        .bind(conv_id)
        .bind(&u.id)
        .bind(&u.name)
        .bind(u.kind.as_str())
        .bind(&u.owner_id)
        .bind(role)
        .bind(trigger.as_str())
        .bind(join_seq)
        .bind(visible_from)
        .execute(&mut *self.c)
        .await?;
        Ok(())
    }

    /// First seq of a conversation at or after a time (None: nothing that new).
    pub async fn first_seq_since(&mut self, conv_id: &str, ts: i64) -> ApiResult<Option<i64>> {
        let r = sqlx::query(
            self.d
                .q("SELECT MIN(seq) FROM events WHERE conv_id = ? AND created_at >= ?"),
        )
        .bind(conv_id)
        .bind(ts)
        .fetch_one(&mut *self.c)
        .await?;
        Ok(r.try_get::<Option<i64>, _>(0)?)
    }

    /// Oldest seq still stored (retention deletes older ones).
    pub async fn first_seq(&mut self, conv_id: &str) -> ApiResult<Option<i64>> {
        let r = sqlx::query(self.d.q("SELECT MIN(seq) FROM events WHERE conv_id = ?"))
            .bind(conv_id)
            .fetch_one(&mut *self.c)
            .await?;
        Ok(r.try_get::<Option<i64>, _>(0)?)
    }

    /// Where a window of history starts: the last `n` events within `since`,
    /// never before what is still stored. `last_seq + 1` when there is none.
    pub async fn window_start(
        &mut self,
        conv_id: &str,
        last_seq: i64,
        n: i64,
        since: i64,
    ) -> ApiResult<i64> {
        if n <= 0 {
            return Ok(last_seq + 1);
        }
        let by_time = self
            .first_seq_since(conv_id, since)
            .await?
            .unwrap_or(last_seq + 1);
        let floor = self.first_seq(conv_id).await?.unwrap_or(1);
        Ok((last_seq - n + 1).max(by_time).max(floor).max(1))
    }

    pub async fn members(&mut self, conv_id: &str) -> ApiResult<Vec<Member>> {
        let rows = sqlx::query(self.d.q(&format!(
            "SELECT {MEMBER_COLS} FROM members WHERE conv_id = ? ORDER BY join_seq, CASE role WHEN 'owner' THEN 0 WHEN 'admin' THEN 1 ELSE 2 END, {}",
            self.bytewise("user_id")
        )))
        .bind(conv_id)
        .fetch_all(&mut *self.c)
        .await?;
        rows.iter()
            .map(row_member)
            .collect::<sqlx::Result<_>>()
            .map_err(Into::into)
    }

    pub async fn member(&mut self, conv_id: &str, user_id: &str) -> ApiResult<Option<Member>> {
        let r = sqlx::query(self.d.q(&format!(
            "SELECT {MEMBER_COLS} FROM members WHERE conv_id = ? AND user_id = ?"
        )))
        .bind(conv_id)
        .bind(user_id)
        .fetch_optional(&mut *self.c)
        .await?;
        r.as_ref().map(row_member).transpose().map_err(Into::into)
    }

    /// Remembers a removed member's last visible stretch (D92).
    pub async fn remember_past(
        &mut self,
        conv_id: &str,
        user_id: &str,
        visible_from: i64,
        until_seq: i64,
    ) -> ApiResult<()> {
        sqlx::query(
            self.d
                .q("DELETE FROM past_members WHERE conv_id = ? AND user_id = ?"),
        )
        .bind(conv_id)
        .bind(user_id)
        .execute(&mut *self.c)
        .await?;
        sqlx::query(self.d.q(
            "INSERT INTO past_members (conv_id, user_id, visible_from, until_seq) VALUES (?, ?, ?, ?)",
        ))
        .bind(conv_id)
        .bind(user_id)
        .bind(visible_from)
        .bind(until_seq)
        .execute(&mut *self.c)
        .await?;
        Ok(())
    }

    /// (visible_from, until_seq) of a member's previous membership.
    /// Messages of a conversation in their current state, for people paging
    /// back (docs/08 §2.8): created at or below `pivot` (newest first) or
    /// above it (oldest first), within what the reader may see: from
    /// `visible_from`, or inside an earlier membership `past`.
    pub async fn messages_page(
        &mut self,
        conv_id: &str,
        visible_from: i64,
        past: Option<(i64, i64)>,
        pivot: i64,
        down: bool,
        limit: i64,
    ) -> ApiResult<Vec<MsgView>> {
        let (pf, pu) = past.unwrap_or((1, 0));
        let (cmp, order) = if down { ("<=", "DESC") } else { (">", "ASC") };
        let rows = sqlx::query(self.d.q(&format!(
            "SELECT {MSG_COLS} FROM messages WHERE conv_id = ? AND created_seq {cmp} ?
             AND (created_seq >= ? OR (created_seq >= ? AND created_seq <= ?))
             ORDER BY created_seq {order} LIMIT ?"
        )))
        .bind(conv_id)
        .bind(pivot)
        .bind(visible_from)
        .bind(pf)
        .bind(pu)
        .bind(limit)
        .fetch_all(&mut *self.c)
        .await?;
        self.preload(&rows, &["content", "payload", "envelope"])
            .await?;
        rows.iter()
            .map(|r| row_msg(r, self.cx))
            .collect::<sqlx::Result<_>>()
            .map_err(Into::into)
    }

    pub async fn past_member(
        &mut self,
        conv_id: &str,
        user_id: &str,
    ) -> ApiResult<Option<(i64, i64)>> {
        let r = sqlx::query(self.d.q(
            "SELECT visible_from, until_seq FROM past_members WHERE conv_id = ? AND user_id = ?",
        ))
        .bind(conv_id)
        .bind(user_id)
        .fetch_optional(&mut *self.c)
        .await?;
        Ok(match r {
            Some(r) => Some((r.try_get(0)?, r.try_get(1)?)),
            None => None,
        })
    }

    pub async fn remove_member(&mut self, conv_id: &str, user_id: &str) -> ApiResult<()> {
        for q in [
            "DELETE FROM members WHERE conv_id = ? AND user_id = ?",
            "DELETE FROM read_cursors WHERE conv_id = ? AND user_id = ?",
            "DELETE FROM takeovers WHERE conv_id = ? AND agent_id = ?",
        ] {
            sqlx::query(self.d.q(q))
                .bind(conv_id)
                .bind(user_id)
                .execute(&mut *self.c)
                .await?;
        }
        Ok(())
    }

    pub async fn update_conv(
        &mut self,
        id: &str,
        title: Option<&str>,
        fields: Option<&Value>,
    ) -> ApiResult<()> {
        sqlx::query(self.d.q(
            "UPDATE conversations SET title = COALESCE(?, title), fields = COALESCE(?, fields) WHERE id = ?",
        ))
        .bind(title)
        .bind(fields.map(|f| f.to_string()))
        .bind(id)
        .execute(&mut *self.c)
        .await?;
        Ok(())
    }

    /// Profile and custom fields of a user (control database).
    pub async fn update_user(
        &mut self,
        id: &str,
        display_name: Option<&str>,
        avatar: Option<&str>,
        fields: Option<&Value>,
    ) -> ApiResult<()> {
        sqlx::query(self.d.q(
            "UPDATE users SET display_name = COALESCE(?, display_name), avatar = COALESCE(?, avatar), fields = COALESCE(?, fields) WHERE id = ?",
        ))
        .bind(display_name)
        .bind(avatar)
        .bind(fields.map(|f| f.to_string()))
        .bind(id)
        .execute(&mut *self.c)
        .await?;
        Ok(())
    }

    pub async fn set_deactivated(&mut self, id: &str, on: bool) -> ApiResult<()> {
        sqlx::query(self.d.q("UPDATE users SET deactivated_at = ? WHERE id = ?"))
            .bind(on.then(now_ms))
            .bind(id)
            .execute(&mut *self.c)
            .await?;
        self.bump_meta(AUTH_EPOCH).await?;
        Ok(())
    }

    /// `{id, name, kind, owner_id, display_name, avatar, fields, deactivated_at, created_at}`.
    pub async fn user_profile(&mut self, id: &str) -> ApiResult<Option<Value>> {
        let r = sqlx::query(self.d.q(
            "SELECT id, name, kind, owner_id, unit_id, agent_scope, display_name, avatar, fields, deactivated_at, created_at FROM users WHERE id = ?",
        ))
        .bind(id)
        .fetch_optional(&mut *self.c)
        .await?;
        let Some(r) = r else { return Ok(None) };
        Ok(Some(json!({
            "id": r.try_get::<String, _>("name")?,
            "kind": r.try_get::<String, _>("kind")?,
            "owner": r.try_get::<Option<String>, _>("owner_id")?,
            "unit": r.try_get::<String, _>("unit_id")?,
            "team": r.try_get::<Option<String>, _>("agent_scope")?.as_deref() == Some("team"),
            "display_name": r.try_get::<Option<String>, _>("display_name")?,
            "avatar": r.try_get::<Option<String>, _>("avatar")?,
            "fields": json_col(&r, "fields")?,
            "deactivated": r.try_get::<Option<i64>, _>("deactivated_at")?.is_some(),
            "created_at": r.try_get::<i64, _>("created_at")?,
        })))
    }

    pub async fn set_role(&mut self, conv_id: &str, user_id: &str, role: &str) -> ApiResult<()> {
        sqlx::query(
            self.d
                .q("UPDATE members SET role = ? WHERE conv_id = ? AND user_id = ?"),
        )
        .bind(role)
        .bind(conv_id)
        .bind(user_id)
        .execute(&mut *self.c)
        .await?;
        Ok(())
    }

    /// Pending drafts of an agent in a conversation become `discarded`.
    pub async fn discard_drafts(&mut self, conv_id: &str, agent_id: &str) -> ApiResult<u64> {
        Ok(sqlx::query(
            self.d
                .q("UPDATE drafts SET state = ? WHERE conv_id = ? AND agent_id = ? AND state = ?"),
        )
        .bind("discarded")
        .bind(conv_id)
        .bind(agent_id)
        .bind("pending")
        .execute(&mut *self.c)
        .await?
        .rows_affected())
    }

    pub async fn set_agent_policy(
        &mut self,
        conv_id: &str,
        agent_id: &str,
        approval: Option<bool>,
        trigger: Option<Trigger>,
        topics: Option<&[String]>,
        actor: &str,
    ) -> ApiResult<Event> {
        if let Some(t) = topics {
            sqlx::query(
                self.d
                    .q("UPDATE members SET topics = ? WHERE conv_id = ? AND user_id = ?"),
            )
            .bind(serde_json::to_string(t).unwrap())
            .bind(conv_id)
            .bind(agent_id)
            .execute(&mut *self.c)
            .await?;
        }
        if let Some(a) = approval {
            sqlx::query(
                self.d
                    .q("UPDATE members SET approval = ? WHERE conv_id = ? AND user_id = ?"),
            )
            .bind(a as i64)
            .bind(conv_id)
            .bind(agent_id)
            .execute(&mut *self.c)
            .await?;
        }
        if let Some(t) = trigger {
            sqlx::query(
                self.d
                    .q("UPDATE members SET trigger_mode = ? WHERE conv_id = ? AND user_id = ?"),
            )
            .bind(t.as_str())
            .bind(conv_id)
            .bind(agent_id)
            .execute(&mut *self.c)
            .await?;
        }
        let m = self
            .member(conv_id, agent_id)
            .await?
            .ok_or_else(|| ApiError::new("not_found", "member"))?;
        self.append_event(
            conv_id,
            "member.policy",
            Some(actor),
            None,
            json!({"user_id": agent_id, "approval": m.approval, "trigger": m.trigger, "topics": m.topics}),
        )
        .await
    }

    // ------------------------------------------------------------ messages

    /// Dedup + seq assignment + persistence; the caller's transaction makes
    /// them atomic.
    /// The recorded result of an earlier write with this `client_msg_id`.
    pub async fn dedup_get(
        &mut self,
        conv_id: &str,
        sender_id: &str,
        cid: &str,
    ) -> ApiResult<Option<Value>> {
        let prev = sqlx::query(self.d.q(
            "SELECT result FROM dedup WHERE conv_id = ? AND sender_id = ? AND client_msg_id = ?",
        ))
        .bind(conv_id)
        .bind(sender_id)
        .bind(cid)
        .fetch_optional(&mut *self.c)
        .await?;
        Ok(match prev {
            Some(r) => {
                Some(serde_json::from_str(&r.try_get::<String, _>(0)?).unwrap_or(Value::Null))
            }
            None => None,
        })
    }

    pub async fn dedup_put(
        &mut self,
        conv_id: &str,
        sender_id: &str,
        cid: &str,
        result: &Value,
    ) -> ApiResult<()> {
        sqlx::query(self.d.q("INSERT INTO dedup (conv_id, sender_id, client_msg_id, result, created_at) VALUES (?, ?, ?, ?, ?)"))
            .bind(conv_id)
            .bind(sender_id)
            .bind(cid)
            .bind(result.to_string())
            .bind(now_ms())
            .execute(&mut *self.c)
            .await?;
        Ok(())
    }

    pub async fn commit_message(&mut self, m: NewMsg<'_>) -> ApiResult<Committed> {
        if let Some(cid) = m.client_msg_id
            && let Some(v) = self.dedup_get(m.conv_id, &m.sender.id, cid).await?
        {
            return Ok(Committed::Duplicate {
                seq: v["seq"].as_i64().unwrap_or(0),
                msg_id: v["msg_id"].as_str().unwrap_or("").to_string(),
            });
        }
        let seq = self.next_seq(m.conv_id).await?;
        let ts = now_ms();
        let view = MsgView {
            msg_id: format!("m_{}", new_id()),
            client_msg_id: m.client_msg_id.map(String::from),
            conv_id: m.conv_id.to_string(),
            created_seq: seq,
            sender_id: m.sender.id.clone(),
            sender_name: m.sender.name.clone(),
            sender_type: m.sender.kind,
            on_behalf_of: m.on_behalf_of,
            kind: m.kind.to_string(),
            content: m.content,
            payload: m.payload,
            version: 1,
            state: m.state.to_string(),
            state_reason: None,
            hop: m.hop,
            causation_id: m.causation_id,
            reply_to: m.reply_to,
            mentions: m.mentions,
            created_at: ts,
            updated_at: ts,
            envelope: m.envelope,
            root_id: None,
        };
        let mut view = view;
        view.root_id = Some(m.root_id.unwrap_or_else(|| view.msg_id.clone()));
        let sealed_envelope = self
            .seal_json(
                m.conv_id,
                &format!("messages.envelope:{}", view.msg_id),
                view.envelope.as_ref(),
            )
            .await?;
        let sealed_content = self
            .seal(
                m.conv_id,
                &format!("messages.content:{}", view.msg_id),
                &view.content,
            )
            .await?;
        let sealed_payload = self
            .seal_json(
                m.conv_id,
                &format!("messages.payload:{}", view.msg_id),
                view.payload.as_ref(),
            )
            .await?;
        sqlx::query(self.d.q(&format!(
            "INSERT INTO messages ({MSG_COLS}, trace_id) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
        )))
        .bind(&view.msg_id)
        .bind(&view.conv_id)
        .bind(seq)
        .bind(&view.sender_id)
        .bind(&view.sender_name)
        .bind(view.sender_type.as_str())
        .bind(&view.on_behalf_of)
        .bind(&view.kind)
        .bind(&sealed_content)
        .bind(view.version)
        .bind(&view.state)
        .bind(&view.state_reason)
        .bind(view.hop)
        .bind(&view.causation_id)
        .bind(&view.reply_to)
        .bind(serde_json::to_string(&view.mentions).unwrap())
        .bind(ts)
        .bind(ts)
        .bind(sealed_payload)
        .bind(sealed_envelope)
        .bind(&view.root_id)
        // The request's trace, for the message trace (docs/14 §4.3).
        .bind(crate::trace::current_id())
        .execute(&mut *self.c)
        .await?;
        if let Some(cid) = m.client_msg_id {
            self.dedup_put(
                m.conv_id,
                &m.sender.id,
                cid,
                &json!({"seq": seq, "msg_id": view.msg_id}),
            )
            .await?;
        }
        let sender_id = view.sender_id.clone();
        let ev = self
            .write_event(
                m.conv_id,
                seq,
                "msg.created",
                Some(&sender_id),
                Some(view),
                {
                    let mut data = json!({});
                    if let Some(a) = m.approval {
                        data["approval"] = a;
                    }
                    if let Some((w, capped)) = m.wake {
                        data["wake"] = json!(w);
                        if capped > 0 {
                            data["wake_capped"] = json!(capped);
                        }
                    }
                    data
                },
                ts,
            )
            .await?;
        Ok(Committed::New(ev))
    }

    pub async fn message(&mut self, msg_id: &str) -> ApiResult<Option<MsgView>> {
        let r = sqlx::query(
            self.d
                .q(&format!("SELECT {MSG_COLS} FROM messages WHERE msg_id = ?")),
        )
        .bind(msg_id)
        .fetch_optional(&mut *self.c)
        .await?;
        self.preload(r.as_slice(), &["content", "payload"]).await?;
        r.as_ref()
            .map(|x| row_msg(x, self.cx))
            .transpose()
            .map_err(Into::into)
    }

    /// Applies a change to a message and announces it with a new event
    /// (`msg.updated` or `msg.recalled`).
    pub async fn update_message(
        &mut self,
        msg_id: &str,
        kind: &str,
        actor: &str,
        content: Option<&str>,
        state: Option<&str>,
        reason: Option<&str>,
    ) -> ApiResult<Event> {
        self.update_message_with(msg_id, kind, actor, content, state, reason, json!({}))
            .await
    }

    /// `update_message` with event data (e.g. the agents a completed stream
    /// wakes).
    #[allow(clippy::too_many_arguments)]
    pub async fn update_message_with(
        &mut self,
        msg_id: &str,
        kind: &str,
        actor: &str,
        content: Option<&str>,
        state: Option<&str>,
        reason: Option<&str>,
        data: Value,
    ) -> ApiResult<Event> {
        let content = match content {
            Some(c) => {
                let conv = self.msg_conv(msg_id).await?.unwrap_or_default();
                Some(
                    self.seal(&conv, &format!("messages.content:{msg_id}"), c)
                        .await?,
                )
            }
            None => None,
        };
        sqlx::query(self.d.q(
            "UPDATE messages SET content = COALESCE(?, content), state = COALESCE(?, state),
             state_reason = COALESCE(?, state_reason), version = version + 1, updated_at = ? WHERE msg_id = ?",
        ))
        .bind(content)
        .bind(state)
        .bind(reason)
        .bind(now_ms())
        .bind(msg_id)
        .execute(&mut *self.c)
        .await?;
        let view = self
            .message(msg_id)
            .await?
            .ok_or_else(|| ApiError::new("not_found", "message"))?;
        let conv_id = view.conv_id.clone();
        self.append_event(&conv_id, kind, Some(actor), Some(view), data)
            .await
    }

    /// Replaces a custom kind's payload (before `update_message` announces it).
    pub async fn set_mentions(&mut self, msg_id: &str, mentions: &[String]) -> ApiResult<()> {
        sqlx::query(
            self.d
                .q("UPDATE messages SET mentions = ? WHERE msg_id = ?"),
        )
        .bind(serde_json::to_string(mentions).unwrap())
        .bind(msg_id)
        .execute(&mut *self.c)
        .await?;
        Ok(())
    }

    pub async fn set_payload(&mut self, msg_id: &str, payload: Option<&Value>) -> ApiResult<()> {
        let conv = self.msg_conv(msg_id).await?.unwrap_or_default();
        let sealed = self
            .seal_json(&conv, &format!("messages.payload:{msg_id}"), payload)
            .await?;
        sqlx::query(self.d.q("UPDATE messages SET payload = ? WHERE msg_id = ?"))
            .bind(sealed)
            .bind(msg_id)
            .execute(&mut *self.c)
            .await?;
        Ok(())
    }

    /// Streaming checkpoint: overwrites partial content without an event.
    pub async fn checkpoint(&mut self, msg_id: &str, content: &str) -> ApiResult<()> {
        let conv = self.msg_conv(msg_id).await?.unwrap_or_default();
        let content = self
            .seal(&conv, &format!("messages.content:{msg_id}"), content)
            .await?;
        sqlx::query(
            self.d
                .q("UPDATE messages SET content = ?, updated_at = ? WHERE msg_id = ?"),
        )
        .bind(&content)
        .bind(now_ms())
        .bind(msg_id)
        .execute(&mut *self.c)
        .await?;
        Ok(())
    }

    pub async fn streaming_messages(&mut self) -> ApiResult<Vec<MsgView>> {
        let rows = sqlx::query(
            self.d
                .q(&format!("SELECT {MSG_COLS} FROM messages WHERE state = ?")),
        )
        .bind("streaming")
        .fetch_all(&mut *self.c)
        .await?;
        self.preload(&rows, &["content", "payload"]).await?;
        rows.iter()
            .map(|x| row_msg(x, self.cx))
            .collect::<sqlx::Result<_>>()
            .map_err(Into::into)
    }

    pub async fn latest_other_message(
        &mut self,
        conv_id: &str,
        user_id: &str,
    ) -> ApiResult<Option<MsgView>> {
        let r = sqlx::query(self.d.q(&format!(
            "SELECT {MSG_COLS} FROM messages WHERE conv_id = ? AND sender_id <> ? ORDER BY created_seq DESC LIMIT 1"
        )))
        .bind(conv_id)
        .bind(user_id)
        .fetch_optional(&mut *self.c)
        .await?;
        self.preload(r.as_slice(), &["content", "payload"]).await?;
        r.as_ref()
            .map(|x| row_msg(x, self.cx))
            .transpose()
            .map_err(Into::into)
    }

    pub async fn recent_contents(
        &mut self,
        conv_id: &str,
        sender_id: &str,
        n: i64,
    ) -> ApiResult<Vec<String>> {
        let rows = sqlx::query(self.d.q(
            "SELECT msg_id, content FROM messages WHERE conv_id = ? AND sender_id = ? ORDER BY created_seq DESC LIMIT ?",
        ))
        .bind(conv_id)
        .bind(sender_id)
        .bind(n)
        .fetch_all(&mut *self.c)
        .await?;
        self.preload(&rows, &["content"]).await?;
        rows.iter()
            .map(|r| {
                let id: String = r.try_get("msg_id")?;
                dec(self.cx, r, "content", &format!("messages.content:{id}"))
            })
            .collect::<sqlx::Result<_>>()
            .map_err(Into::into)
    }

    /// The trace a message was created in (docs/14 §4.3), and its
    /// conversation; None for an unknown message.
    pub async fn msg_trace(&mut self, msg_id: &str) -> ApiResult<Option<(String, Option<String>)>> {
        let r = sqlx::query(
            self.d
                .q("SELECT conv_id, trace_id FROM messages WHERE msg_id = ?"),
        )
        .bind(msg_id)
        .fetch_optional(&mut *self.c)
        .await?;
        r.map(|r| Ok((r.try_get(0)?, r.try_get(1)?)))
            .transpose()
            .map_err(|e: sqlx::Error| e.into())
    }

    pub async fn msg_conv(&mut self, msg_id: &str) -> ApiResult<Option<String>> {
        let r = sqlx::query(self.d.q("SELECT conv_id FROM messages WHERE msg_id = ?"))
            .bind(msg_id)
            .fetch_optional(&mut *self.c)
            .await?;
        r.map(|r| r.try_get(0)).transpose().map_err(Into::into)
    }

    // ------------------------------------------------------------- reading

    /// Blanks a message's content in every event that embeds a snapshot of
    /// it (used by recall: the content must not stay readable in the history).
    pub async fn scrub_message(
        &mut self,
        conv_id: &str,
        msg_id: &str,
        from_seq: i64,
    ) -> ApiResult<()> {
        let rows = sqlx::query(
            self.d
                .q("SELECT seq, payload FROM events WHERE conv_id = ? AND seq >= ? AND msg_id = ?"),
        )
        .bind(conv_id)
        .bind(from_seq)
        .bind(msg_id)
        .fetch_all(&mut *self.c)
        .await?;
        self.preload(&rows, &["payload"]).await?;
        for r in rows {
            let seq: i64 = r.try_get(0)?;
            let aad = format!("events.payload:{conv_id}:{seq}");
            let mut p: Value =
                serde_json::from_str(&dec(self.cx, &r, "payload", &aad)?).unwrap_or(Value::Null);
            if p["msg"]["msg_id"] != msg_id {
                continue;
            }
            p["msg"]["content"] = json!("");
            p["msg"]["payload"] = Value::Null;
            p["msg"]["mentions"] = json!([]);
            let sealed = self.seal(conv_id, &aad, &p.to_string()).await?;
            sqlx::query(
                self.d
                    .q("UPDATE events SET payload = ? WHERE conv_id = ? AND seq = ?"),
            )
            .bind(sealed)
            .bind(conv_id)
            .bind(seq)
            .execute(&mut *self.c)
            .await?;
        }
        Ok(())
    }

    /// Removes undelivered webhook bodies of a conversation: all of them
    /// (dissolved), or those carrying one message (recalled). The
    /// `msg.recalled` / `conv.dissolved` event that follows is the tombstone.
    pub async fn forget_outbox(&mut self, conv_id: &str, msg_id: Option<&str>) -> ApiResult<()> {
        let Some(msg_id) = msg_id else {
            sqlx::query(self.d.q("DELETE FROM webhook_outbox WHERE conv_id = ?"))
                .bind(conv_id)
                .execute(&mut *self.c)
                .await?;
            return Ok(());
        };
        let rows = sqlx::query(
            self.d
                .q("SELECT id, body FROM webhook_outbox WHERE conv_id = ?"),
        )
        .bind(conv_id)
        .fetch_all(&mut *self.c)
        .await?;
        self.preload(&rows, &["body"]).await?;
        for r in rows {
            let id: i64 = r.try_get(0)?;
            let body: Value = serde_json::from_str(&dec(
                self.cx,
                &r,
                "body",
                &format!("webhook_outbox.body:{id}"),
            )?)
            .unwrap_or(Value::Null);
            if body["data"]["event"]["msg"]["msg_id"] == msg_id {
                sqlx::query(self.d.q("DELETE FROM webhook_outbox WHERE id = ?"))
                    .bind(id)
                    .execute(&mut *self.c)
                    .await?;
            }
        }
        Ok(())
    }

    /// Empties a draft whose published message was recalled, and the copy
    /// of its text in the side channel.
    pub async fn scrub_draft(&mut self, conv_id: &str, draft_id: &str) -> ApiResult<()> {
        let empty = self
            .seal(conv_id, &format!("drafts.content:{draft_id}"), "")
            .await?;
        sqlx::query(
            self.d.q(
                "UPDATE drafts SET content = ?, payload = NULL WHERE draft_id = ? AND conv_id = ?",
            ),
        )
        .bind(empty)
        .bind(draft_id)
        .bind(conv_id)
        .execute(&mut *self.c)
        .await?;
        let rows = sqlx::query(self.d.q(
            "SELECT side_seq, payload FROM side_events WHERE conv_id = ? AND type = 'draft.created'",
        ))
        .bind(conv_id)
        .fetch_all(&mut *self.c)
        .await?;
        self.preload(&rows, &["payload"]).await?;
        for r in rows {
            let seq: i64 = r.try_get(0)?;
            let aad = format!("side_events.payload:{conv_id}:{seq}");
            let mut p: Value =
                serde_json::from_str(&dec(self.cx, &r, "payload", &aad)?).unwrap_or(Value::Null);
            if p["draft_id"] != draft_id {
                continue;
            }
            p["content"] = json!("");
            p["payload"] = Value::Null;
            let sealed = self.seal(conv_id, &aad, &p.to_string()).await?;
            sqlx::query(
                self.d
                    .q("UPDATE side_events SET payload = ? WHERE conv_id = ? AND side_seq = ?"),
            )
            .bind(sealed)
            .bind(conv_id)
            .bind(seq)
            .execute(&mut *self.c)
            .await?;
        }
        Ok(())
    }

    pub async fn pull(
        &mut self,
        conv_id: &str,
        after_seq: i64,
        limit: i64,
    ) -> ApiResult<Vec<Event>> {
        let mut q = sqlx::query(self.d.q(&format!(
            "SELECT conv_id, seq, type, actor_id, payload, created_at FROM events WHERE conv_id = ? AND seq > ?{} ORDER BY seq LIMIT ?",
            self.cap_sql()
        )))
        .bind(conv_id)
        .bind(after_seq);
        if self.cap {
            q = q.bind(conv_id);
        }
        let rows = q.bind(limit).fetch_all(&mut *self.c).await?;
        self.preload(&rows, &["payload"]).await?;
        rows.iter()
            .map(|x| row_event(x, self.cx))
            .collect::<sqlx::Result<_>>()
            .map_err(Into::into)
    }

    /// Side events after `after`, at most `limit` (M5: never unbounded).
    pub async fn pull_side(
        &mut self,
        conv_id: &str,
        after: i64,
        limit: i64,
    ) -> ApiResult<Vec<SideEvent>> {
        let rows = sqlx::query(self.d.q(
            "SELECT conv_id, side_seq, anchor_seq, audience, type, actor_id, payload, created_at FROM side_events
             WHERE conv_id = ? AND side_seq > ? ORDER BY side_seq LIMIT ?",
        ))
        .bind(conv_id)
        .bind(after)
        .bind(limit)
        .fetch_all(&mut *self.c)
        .await?;
        self.preload(&rows, &["payload"]).await?;
        rows.iter()
            .map(|x| row_side(x, self.cx))
            .collect::<sqlx::Result<_>>()
            .map_err(Into::into)
    }

    pub async fn set_read(&mut self, conv_id: &str, user_id: &str, seq: i64) -> ApiResult<()> {
        let cur = self.read_seq(conv_id, user_id).await?;
        if seq <= cur {
            return Ok(());
        }
        let n = sqlx::query(
            self.d
                .q("UPDATE read_cursors SET read_seq = ? WHERE conv_id = ? AND user_id = ?"),
        )
        .bind(seq)
        .bind(conv_id)
        .bind(user_id)
        .execute(&mut *self.c)
        .await?
        .rows_affected();
        if n == 0 {
            sqlx::query(
                self.d
                    .q("INSERT INTO read_cursors (conv_id, user_id, read_seq) VALUES (?, ?, ?)"),
            )
            .bind(conv_id)
            .bind(user_id)
            .bind(seq)
            .execute(&mut *self.c)
            .await?;
        }
        Ok(())
    }

    // ------------------------------------------------- sync (docs/08)

    /// A device's acknowledged (seq, side seq) for a conversation.
    /// The read/mute state a device confirmed it received for a conversation.
    pub async fn cursor_told(
        &mut self,
        device_id: &str,
        conv_id: &str,
    ) -> ApiResult<Option<String>> {
        let r = sqlx::query(
            self.d
                .q("SELECT told_state FROM device_cursors WHERE device_id = ? AND conv_id = ?"),
        )
        .bind(device_id)
        .bind(conv_id)
        .fetch_optional(&mut *self.c)
        .await?;
        Ok(r.map(|r| r.try_get::<Option<String>, _>(0))
            .transpose()?
            .flatten())
    }

    pub async fn set_cursor_told(
        &mut self,
        device_id: &str,
        conv_id: &str,
        state: &str,
    ) -> ApiResult<()> {
        sqlx::query(
            self.d
                .q("UPDATE device_cursors SET told_state = ? WHERE device_id = ? AND conv_id = ?"),
        )
        .bind(state)
        .bind(device_id)
        .bind(conv_id)
        .execute(&mut *self.c)
        .await?;
        Ok(())
    }

    pub async fn cursor(
        &mut self,
        device_id: &str,
        conv_id: &str,
    ) -> ApiResult<Option<(i64, i64)>> {
        let r = sqlx::query(self.d.q(
            "SELECT acked_seq, acked_side_seq FROM device_cursors WHERE device_id = ? AND conv_id = ?",
        ))
        .bind(device_id)
        .bind(conv_id)
        .fetch_optional(&mut *self.c)
        .await?;
        Ok(
            r.map(|r| Ok::<_, sqlx::Error>((r.try_get(0)?, r.try_get(1)?)))
                .transpose()?,
        )
    }

    /// Moves a device's position forward (never back).
    pub async fn ack_cursor(
        &mut self,
        device_id: &str,
        conv_id: &str,
        user_id: &str,
        seq: i64,
        side_seq: i64,
    ) -> ApiResult<()> {
        let now = now_ms();
        match self.cursor(device_id, conv_id).await? {
            Some((a, s)) => {
                sqlx::query(self.d.q(
                    "UPDATE device_cursors SET acked_seq = ?, acked_side_seq = ?, updated_at = ? WHERE device_id = ? AND conv_id = ?",
                ))
                .bind(a.max(seq))
                .bind(s.max(side_seq))
                .bind(now)
                .bind(device_id)
                .bind(conv_id)
                .execute(&mut *self.c)
                .await?;
            }
            None => {
                sqlx::query(self.d.q(
                    "INSERT INTO device_cursors (device_id, conv_id, user_id, acked_seq, acked_side_seq, updated_at) VALUES (?, ?, ?, ?, ?, ?)",
                ))
                .bind(device_id)
                .bind(conv_id)
                .bind(user_id)
                .bind(seq)
                .bind(side_seq)
                .bind(now)
                .execute(&mut *self.c)
                .await?;
            }
        }
        Ok(())
    }

    /// A member lost access: their devices forget the conversation.
    pub async fn drop_cursors_of(&mut self, conv_id: &str, user_id: &str) -> ApiResult<u64> {
        Ok(sqlx::query(
            self.d
                .q("DELETE FROM device_cursors WHERE conv_id = ? AND user_id = ?"),
        )
        .bind(conv_id)
        .bind(user_id)
        .execute(&mut *self.c)
        .await?
        .rows_affected())
    }

    pub async fn departure_put(
        &mut self,
        user_id: &str,
        conv_id: &str,
        seq: i64,
        dissolved: bool,
    ) -> ApiResult<()> {
        self.departure_clear(user_id, conv_id).await?;
        sqlx::query(self.d.q(
            "INSERT INTO departures (user_id, conv_id, seq, dissolved, at) VALUES (?, ?, ?, ?, ?)",
        ))
        .bind(user_id)
        .bind(conv_id)
        .bind(seq)
        .bind(dissolved as i64)
        .bind(now_ms())
        .execute(&mut *self.c)
        .await?;
        Ok(())
    }

    pub async fn departure_clear(&mut self, user_id: &str, conv_id: &str) -> ApiResult<()> {
        sqlx::query(
            self.d
                .q("DELETE FROM departures WHERE user_id = ? AND conv_id = ?"),
        )
        .bind(user_id)
        .bind(conv_id)
        .execute(&mut *self.c)
        .await?;
        Ok(())
    }

    /// (conv_id, seq, dissolved) a user left, newest first.
    pub async fn departures(&mut self, user_id: &str) -> ApiResult<Vec<(String, i64, bool)>> {
        let rows = sqlx::query(self.d.q(
            "SELECT conv_id, seq, dissolved FROM departures WHERE user_id = ? ORDER BY at DESC",
        ))
        .bind(user_id)
        .fetch_all(&mut *self.c)
        .await?;
        rows.iter()
            .map(|r| Ok((r.try_get(0)?, r.try_get(1)?, r.try_get::<i64, _>(2)? != 0)))
            .collect::<sqlx::Result<_>>()
            .map_err(Into::into)
    }

    pub async fn drop_old_departures(&mut self, before: i64) -> ApiResult<u64> {
        Ok(sqlx::query(self.d.q("DELETE FROM departures WHERE at < ?"))
            .bind(before)
            .execute(&mut *self.c)
            .await?
            .rows_affected())
    }

    /// A device logged out: it forgets every conversation in this database.
    pub async fn drop_device_cursors(&mut self, device_id: &str) -> ApiResult<u64> {
        Ok(
            sqlx::query(self.d.q("DELETE FROM device_cursors WHERE device_id = ?"))
                .bind(device_id)
                .execute(&mut *self.c)
                .await?
                .rows_affected(),
        )
    }

    pub async fn drop_user_cursors(&mut self, user_id: &str) -> ApiResult<u64> {
        Ok(
            sqlx::query(self.d.q("DELETE FROM device_cursors WHERE user_id = ?"))
                .bind(user_id)
                .execute(&mut *self.c)
                .await?
                .rows_affected(),
        )
    }

    /// Devices silent since `before` lose their positions (docs/08 §2.5):
    /// all of a device's positions go together, so a quiet conversation of
    /// an active device keeps its position (M6).
    pub async fn drop_stale_cursors(&mut self, before: i64) -> ApiResult<u64> {
        Ok(
            sqlx::query(self.d.q("DELETE FROM device_cursors WHERE device_id IN (SELECT device_id FROM (SELECT device_id FROM device_cursors GROUP BY device_id HAVING MAX(updated_at) < ?) silent)"))
                .bind(before)
                .execute(&mut *self.c)
                .await?
                .rows_affected(),
        )
    }

    /// Deletes conversation content older than `before` (retention). The
    /// conversation keeps its `last_seq`, so new events continue the log.
    /// Returns the rows deleted and the conversations they were in; only
    /// conversations `mine` accepts are cleaned (a cluster node cleans the
    /// ones it holds).
    pub async fn purge_before(
        &mut self,
        before: i64,
        max_rows: i64,
        mine: impl Fn(&str) -> bool,
    ) -> ApiResult<(u64, Vec<String>)> {
        // M15: only a prefix of each log goes (a clock set back cannot cut a
        // hole in the middle), in bounded steps (the writer is not held up),
        // with what is derived from it.
        let convs: Vec<String> = sqlx::query(self.d.q("SELECT id FROM conversations"))
            .fetch_all(&mut *self.c)
            .await?
            .iter()
            .map(|r| r.try_get(0))
            .collect::<sqlx::Result<_>>()?;
        let mut n = 0u64;
        let mut purged = vec![];
        for conv in convs.into_iter().filter(|c| mine(c)) {
            if n as i64 >= max_rows {
                break;
            }
            let Some(first) = self.first_seq(&conv).await? else {
                continue;
            };
            let keep_from = match sqlx::query(self.d.q(
                "SELECT seq FROM events WHERE conv_id = ? AND created_at >= ? ORDER BY seq LIMIT 1",
            ))
            .bind(&conv)
            .bind(before)
            .fetch_optional(&mut *self.c)
            .await?
            {
                Some(r) => r.try_get::<i64, _>(0)?,
                None => i64::MAX,
            };
            let upto = keep_from.min(first + (max_rows - n as i64));
            if upto <= first {
                continue;
            }
            purged.push(conv.clone());
            // Files of the messages that go, go too.
            for f in self.files_of_messages_before(&conv, upto).await? {
                self.file_trash(&f).await?;
            }
            for (q, by_seq) in [
                ("DELETE FROM events WHERE conv_id = ? AND seq < ?", true),
                (
                    "DELETE FROM messages WHERE conv_id = ? AND created_seq < ?",
                    true,
                ),
                (
                    "DELETE FROM side_events WHERE conv_id = ? AND anchor_seq < ?",
                    true,
                ),
                (
                    "DELETE FROM summaries WHERE conv_id = ? AND to_seq < ?",
                    true,
                ),
                (
                    "DELETE FROM feedback WHERE conv_id = ? AND created_at < ?",
                    false,
                ),
                (
                    "DELETE FROM card_responses WHERE conv_id = ? AND created_at < ?",
                    false,
                ),
                (
                    "DELETE FROM claims WHERE conv_id = ? AND created_at < ?",
                    false,
                ),
                (
                    "DELETE FROM drafts WHERE conv_id = ? AND state <> 'pending' AND created_at < ?",
                    false,
                ),
                (
                    "DELETE FROM request_assignees WHERE conv_id = ? AND request_id IN (SELECT request_id FROM requests WHERE state <> 'pending' AND created_at < ?)",
                    false,
                ),
                (
                    "DELETE FROM requests WHERE conv_id = ? AND state <> 'pending' AND created_at < ?",
                    false,
                ),
            ] {
                let r = sqlx::query(self.d.q(q))
                    .bind(&conv)
                    .bind(if by_seq { upto } else { before })
                    .execute(&mut *self.c)
                    .await?
                    .rows_affected();
                if q.starts_with("DELETE FROM events") {
                    n += r;
                }
            }
        }
        // Undelivered webhook bodies older than the retention go too.
        sqlx::query(self.d.q("DELETE FROM webhook_outbox WHERE created_at < ?"))
            .bind(before)
            .execute(&mut *self.c)
            .await?;
        Ok((n, purged))
    }

    /// When an event was written (None: no longer stored).
    pub async fn event_time(&mut self, conv_id: &str, seq: i64) -> ApiResult<Option<i64>> {
        let r = sqlx::query(
            self.d
                .q("SELECT created_at FROM events WHERE conv_id = ? AND seq = ?"),
        )
        .bind(conv_id)
        .bind(seq)
        .fetch_optional(&mut *self.c)
        .await?;
        Ok(r.map(|r| r.try_get(0)).transpose()?)
    }

    pub async fn set_silenced(
        &mut self,
        conv_id: &str,
        user_id: &str,
        until: Option<i64>,
    ) -> ApiResult<()> {
        sqlx::query(
            self.d
                .q("UPDATE members SET silenced_until = ? WHERE conv_id = ? AND user_id = ?"),
        )
        .bind(until)
        .bind(conv_id)
        .bind(user_id)
        .execute(&mut *self.c)
        .await?;
        Ok(())
    }

    pub async fn set_mute(
        &mut self,
        conv_id: &str,
        user_id: &str,
        mode: &str,
        until: Option<i64>,
    ) -> ApiResult<()> {
        sqlx::query(self.d.q(
            "UPDATE members SET mute_mode = ?, mute_until = ? WHERE conv_id = ? AND user_id = ?",
        ))
        .bind(mode)
        .bind(until)
        .bind(conv_id)
        .bind(user_id)
        .execute(&mut *self.c)
        .await?;
        Ok(())
    }

    /// Messages at or below this seq do not count as unread: what the user
    /// read, and everything before they could see the conversation.
    async fn unread_floor(&mut self, conv_id: &str, user_id: &str) -> ApiResult<i64> {
        let read = self.read_seq(conv_id, user_id).await?;
        let r = sqlx::query(
            self.d
                .q("SELECT visible_from FROM members WHERE conv_id = ? AND user_id = ?"),
        )
        .bind(conv_id)
        .bind(user_id)
        .fetch_optional(&mut *self.c)
        .await?;
        let visible = match r {
            Some(r) => r.try_get::<Option<i64>, _>(0)?.unwrap_or(1),
            None => 1,
        };
        Ok(read.max(visible - 1))
    }

    /// Unread messages that mention the user.
    pub async fn unread_mentions(&mut self, conv_id: &str, user_id: &str) -> ApiResult<i64> {
        let read = self.unread_floor(conv_id, user_id).await?;
        let r = sqlx::query(self.d.q(
            "SELECT COUNT(*) FROM messages WHERE conv_id = ? AND created_seq > ? AND sender_id <> ? AND state <> ? AND mentions LIKE ?",
        ))
        .bind(conv_id)
        .bind(read)
        .bind(user_id)
        .bind("recalled")
        .bind(format!("%\"{user_id}\"%"))
        .fetch_one(&mut *self.c)
        .await?;
        Ok(r.try_get(0)?)
    }

    /// When a device was registered (its first token).
    pub async fn device_created(&mut self, device_id: &str) -> ApiResult<Option<i64>> {
        let r = sqlx::query(
            self.d
                .q("SELECT MIN(created_at) FROM tokens WHERE device_id = ?"),
        )
        .bind(device_id)
        .fetch_one(&mut *self.c)
        .await?;
        Ok(r.try_get::<Option<i64>, _>(0)?)
    }

    pub async fn prefs(&mut self, user_id: &str) -> ApiResult<Value> {
        let r = sqlx::query(self.d.q("SELECT prefs FROM users WHERE id = ?"))
            .bind(user_id)
            .fetch_optional(&mut *self.c)
            .await?;
        Ok(
            r.and_then(|r| r.try_get::<Option<String>, _>(0).ok().flatten())
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_else(|| json!({})),
        )
    }

    pub async fn set_prefs(&mut self, user_id: &str, prefs: &Value) -> ApiResult<()> {
        sqlx::query(self.d.q("UPDATE users SET prefs = ? WHERE id = ?"))
            .bind(prefs.to_string())
            .bind(user_id)
            .execute(&mut *self.c)
            .await?;
        Ok(())
    }

    pub async fn read_seq(&mut self, conv_id: &str, user_id: &str) -> ApiResult<i64> {
        let r = sqlx::query(
            self.d
                .q("SELECT read_seq FROM read_cursors WHERE conv_id = ? AND user_id = ?"),
        )
        .bind(conv_id)
        .bind(user_id)
        .fetch_optional(&mut *self.c)
        .await?;
        Ok(r.map(|r| r.try_get(0)).transpose()?.unwrap_or(0))
    }

    /// Unread counts only new messages from others (not edits, recalls or
    /// system events).
    pub async fn unread(&mut self, conv_id: &str, user_id: &str) -> ApiResult<i64> {
        let read = self.unread_floor(conv_id, user_id).await?;
        let r = sqlx::query(self.d.q(
            "SELECT COUNT(*) FROM messages WHERE conv_id = ? AND created_seq > ? AND sender_id <> ? AND state <> ?",
        ))
        .bind(conv_id)
        .bind(read)
        .bind(user_id)
        .bind("recalled")
        .fetch_one(&mut *self.c)
        .await?;
        Ok(r.try_get(0)?)
    }

    // -------------------------------------------------------- side channel

    pub async fn append_side(
        &mut self,
        conv_id: &str,
        audience: &[String],
        kind: &str,
        actor: Option<&str>,
        data: Value,
    ) -> ApiResult<SideEvent> {
        sqlx::query(
            self.d
                .q("UPDATE conversations SET side_seq = side_seq + 1 WHERE id = ?"),
        )
        .bind(conv_id)
        .execute(&mut *self.c)
        .await?;
        let r = sqlx::query(
            self.d
                .q("SELECT side_seq, last_seq FROM conversations WHERE id = ?"),
        )
        .bind(conv_id)
        .fetch_one(&mut *self.c)
        .await?;
        let (side_seq, anchor): (i64, i64) = (r.try_get(0)?, r.try_get(1)?);
        let ts = now_ms();
        let sealed = self
            .seal(
                conv_id,
                &format!("side_events.payload:{conv_id}:{side_seq}"),
                &data.to_string(),
            )
            .await?;
        sqlx::query(self.d.q(
            "INSERT INTO side_events (conv_id, side_seq, anchor_seq, audience, type, actor_id, payload, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        ))
        .bind(conv_id)
        .bind(side_seq)
        .bind(anchor)
        .bind(serde_json::to_string(audience).unwrap())
        .bind(kind)
        .bind(actor)
        .bind(sealed)
        .bind(ts)
        .execute(&mut *self.c)
        .await?;
        Ok(SideEvent {
            conv_id: conv_id.to_string(),
            side_seq,
            anchor_seq: anchor,
            audience: audience.to_vec(),
            kind: kind.to_string(),
            actor_id: actor.map(String::from),
            data,
            created_at: ts,
        })
    }

    // -------------------------------------------------------------- drafts

    #[allow(clippy::too_many_arguments)]
    pub async fn create_draft(
        &mut self,
        draft_id: &str,
        conv_id: &str,
        agent: &User,
        content: &str,
        hop: i64,
        causation_id: Option<String>,
        reply_to: Option<String>,
        audience: &[String],
        kind: &str,
        payload: Option<Value>,
        envelope: Option<Value>,
    ) -> ApiResult<(Draft, SideEvent)> {
        let d = Draft {
            draft_id: draft_id.to_string(),
            conv_id: conv_id.to_string(),
            agent_id: agent.id.clone(),
            content: content.to_string(),
            hop,
            causation_id,
            reply_to,
            state: "pending".into(),
            created_at: now_ms(),
            kind: kind.to_string(),
            payload,
            envelope,
        };
        let sealed_envelope = self
            .seal_json(
                conv_id,
                &format!("drafts.envelope:{}", d.draft_id),
                d.envelope.as_ref(),
            )
            .await?;
        let sealed_content = self
            .seal(
                conv_id,
                &format!("drafts.content:{}", d.draft_id),
                &d.content,
            )
            .await?;
        let sealed_payload = self
            .seal_json(
                conv_id,
                &format!("drafts.payload:{}", d.draft_id),
                d.payload.as_ref(),
            )
            .await?;
        sqlx::query(self.d.q(&format!(
            "INSERT INTO drafts ({DRAFT_COLS}) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
        )))
        .bind(&d.draft_id)
        .bind(&d.conv_id)
        .bind(&d.agent_id)
        .bind(&sealed_content)
        .bind(d.hop)
        .bind(&d.causation_id)
        .bind(&d.reply_to)
        .bind(&d.state)
        .bind(d.created_at)
        .bind(&d.kind)
        .bind(sealed_payload)
        .bind(sealed_envelope)
        .execute(&mut *self.c)
        .await?;
        let side = self
            .append_side(
                conv_id,
                audience,
                "draft.created",
                Some(&agent.id),
                json!({"draft_id": d.draft_id, "agent_id": agent.id, "agent_name": agent.name, "content": content, "kind": d.kind, "payload": d.payload}),
            )
            .await?;
        Ok((d, side))
    }

    pub async fn draft(&mut self, id: &str) -> ApiResult<Option<Draft>> {
        let r = sqlx::query(self.d.q(&format!(
            "SELECT {DRAFT_COLS} FROM drafts WHERE draft_id = ?"
        )))
        .bind(id)
        .fetch_optional(&mut *self.c)
        .await?;
        self.preload(r.as_slice(), &["content", "payload"]).await?;
        r.as_ref()
            .map(|x| row_draft(x, self.cx))
            .transpose()
            .map_err(Into::into)
    }

    pub async fn pending_drafts_for(&mut self, agent_ids: &[String]) -> ApiResult<Vec<Draft>> {
        let mut out = vec![];
        for a in agent_ids {
            let rows = sqlx::query(self.d.q(&format!("SELECT {DRAFT_COLS} FROM drafts WHERE agent_id = ? AND state = ? ORDER BY created_at")))
                .bind(a)
                .bind("pending")
                .fetch_all(&mut *self.c)
                .await?;
            self.preload(&rows, &["content", "payload", "envelope"])
                .await?;
            for r in rows {
                out.push(row_draft(&r, self.cx)?);
            }
        }
        out.sort_by_key(|d| d.created_at);
        Ok(out)
    }

    // ------------------------------------------- tasks, claims and floor

    pub async fn create_task(
        &mut self,
        conv_id: &str,
        created_by: &str,
        assignee: Option<&str>,
    ) -> ApiResult<()> {
        sqlx::query(self.d.q(
            "INSERT INTO tasks (conv_id, state, assignee_id, created_by, updated_at) VALUES (?, ?, ?, ?, ?)",
        ))
        .bind(conv_id)
        .bind("submitted")
        .bind(assignee.map(String::from))
        .bind(created_by)
        .bind(now_ms())
        .execute(&mut *self.c)
        .await?;
        Ok(())
    }

    /// (state, assignee, created_by) of a task channel.
    pub async fn task(
        &mut self,
        conv_id: &str,
    ) -> ApiResult<Option<(String, Option<String>, String)>> {
        let r = sqlx::query(
            self.d
                .q("SELECT state, assignee_id, created_by FROM tasks WHERE conv_id = ?"),
        )
        .bind(conv_id)
        .fetch_optional(&mut *self.c)
        .await?;
        Ok(match r {
            Some(r) => Some((r.try_get(0)?, r.try_get(1)?, r.try_get(2)?)),
            None => None,
        })
    }

    pub async fn set_task(
        &mut self,
        conv_id: &str,
        state: &str,
        assignee: Option<&str>,
    ) -> ApiResult<()> {
        sqlx::query(
            self.d
                .q("UPDATE tasks SET state = ?, assignee_id = ?, updated_at = ? WHERE conv_id = ?"),
        )
        .bind(state)
        .bind(assignee.map(String::from))
        .bind(now_ms())
        .bind(conv_id)
        .execute(&mut *self.c)
        .await?;
        Ok(())
    }

    pub async fn claim_of(&mut self, msg_id: &str) -> ApiResult<Option<String>> {
        let r = sqlx::query(self.d.q("SELECT user_id FROM claims WHERE msg_id = ?"))
            .bind(msg_id)
            .fetch_optional(&mut *self.c)
            .await?;
        Ok(match r {
            Some(r) => Some(r.try_get(0)?),
            None => None,
        })
    }

    pub async fn put_claim(&mut self, msg_id: &str, conv_id: &str, user_id: &str) -> ApiResult<()> {
        sqlx::query(
            self.d
                .q("INSERT INTO claims (msg_id, conv_id, user_id, created_at) VALUES (?, ?, ?, ?)"),
        )
        .bind(msg_id)
        .bind(conv_id)
        .bind(user_id)
        .bind(now_ms())
        .execute(&mut *self.c)
        .await?;
        Ok(())
    }

    pub async fn drop_claim(&mut self, msg_id: &str) -> ApiResult<()> {
        sqlx::query(self.d.q("DELETE FROM claims WHERE msg_id = ?"))
            .bind(msg_id)
            .execute(&mut *self.c)
            .await?;
        Ok(())
    }

    /// The agent holding the floor of a conversation, while unexpired.
    pub async fn floor(&mut self, conv_id: &str) -> ApiResult<Option<(String, i64)>> {
        let r = sqlx::query(
            self.d
                .q("SELECT agent_id, expires_at FROM floors WHERE conv_id = ?"),
        )
        .bind(conv_id)
        .fetch_optional(&mut *self.c)
        .await?;
        Ok(match r {
            Some(r) => {
                let exp: i64 = r.try_get(1)?;
                (exp > now_ms())
                    .then(|| r.try_get(0))
                    .transpose()?
                    .map(|a| (a, exp))
            }
            None => None,
        })
    }

    pub async fn set_floor(
        &mut self,
        conv_id: &str,
        agent_id: &str,
        by: &str,
        expires_at: i64,
    ) -> ApiResult<()> {
        sqlx::query(self.d.q("DELETE FROM floors WHERE conv_id = ?"))
            .bind(conv_id)
            .execute(&mut *self.c)
            .await?;
        sqlx::query(self.d.q(
            "INSERT INTO floors (conv_id, agent_id, granted_by, expires_at) VALUES (?, ?, ?, ?)",
        ))
        .bind(conv_id)
        .bind(agent_id)
        .bind(by)
        .bind(expires_at)
        .execute(&mut *self.c)
        .await?;
        Ok(())
    }

    pub async fn clear_floor(&mut self, conv_id: &str) -> ApiResult<()> {
        sqlx::query(self.d.q("DELETE FROM floors WHERE conv_id = ?"))
            .bind(conv_id)
            .execute(&mut *self.c)
            .await?;
        Ok(())
    }

    // -------------------------------------------------------- root budget

    /// Agent messages counted against a chain's source.
    pub async fn root_messages(&mut self, conv_id: &str, root_id: &str) -> ApiResult<i64> {
        let r = sqlx::query(
            self.d
                .q("SELECT messages FROM root_budgets WHERE conv_id = ? AND root_id = ?"),
        )
        .bind(conv_id)
        .bind(root_id)
        .fetch_optional(&mut *self.c)
        .await?;
        Ok(match r {
            Some(r) => r.try_get::<i64, _>(0)?,
            None => 0,
        })
    }

    pub async fn charge_root(&mut self, conv_id: &str, root_id: &str) -> ApiResult<()> {
        let n = sqlx::query(self.d.q(
            "UPDATE root_budgets SET messages = messages + 1 WHERE conv_id = ? AND root_id = ?",
        ))
        .bind(conv_id)
        .bind(root_id)
        .execute(&mut *self.c)
        .await?
        .rows_affected();
        if n == 0 {
            sqlx::query(
                self.d
                    .q("INSERT INTO root_budgets (conv_id, root_id, messages) VALUES (?, ?, 1)"),
            )
            .bind(conv_id)
            .bind(root_id)
            .execute(&mut *self.c)
            .await?;
        }
        Ok(())
    }

    // ------------------------------------------------------------ requests

    pub async fn create_request(&mut self, q: &Request, assignees: &[String]) -> ApiResult<()> {
        let id = &q.request_id;
        let title = self
            .seal(&q.conv_id, &format!("requests.title:{id}"), &q.title)
            .await?;
        let context = self
            .seal_json(
                &q.conv_id,
                &format!("requests.context:{id}"),
                q.context.as_ref(),
            )
            .await?;
        sqlx::query(self.d.q(&format!(
            "INSERT INTO requests ({REQUEST_COLS}) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
        )))
        .bind(id)
        .bind(&q.conv_id)
        .bind(&q.agent_id)
        .bind(&q.client_req_id)
        .bind(&q.kind)
        .bind(title)
        .bind(q.options.as_ref().map(|o| o.to_string()))
        .bind(context)
        .bind(&q.state)
        .bind(q.created_at)
        .bind(q.expires_at)
        .bind(None::<String>)
        .bind(None::<i64>)
        .bind(None::<String>)
        .execute(&mut *self.c)
        .await?;
        for u in assignees {
            sqlx::query(self.d.q(
                "INSERT INTO request_assignees (request_id, user_id, conv_id) VALUES (?, ?, ?)",
            ))
            .bind(id)
            .bind(u)
            .bind(&q.conv_id)
            .execute(&mut *self.c)
            .await?;
        }
        Ok(())
    }

    async fn requests_where(&mut self, cond: &str, binds: &[&str]) -> ApiResult<Vec<Request>> {
        let sql = format!(
            "SELECT {REQUEST_COLS} FROM requests WHERE {cond} ORDER BY created_at, request_id"
        );
        let mut q = sqlx::query(self.d.q(&sql));
        for b in binds {
            q = q.bind(b.to_string());
        }
        let rows = q.fetch_all(&mut *self.c).await?;
        self.preload(&rows, &["title", "context", "response"])
            .await?;
        rows.iter()
            .map(|r| row_request(r, self.cx))
            .collect::<sqlx::Result<_>>()
            .map_err(Into::into)
    }

    pub async fn request(&mut self, id: &str) -> ApiResult<Option<Request>> {
        Ok(self.requests_where("request_id = ?", &[id]).await?.pop())
    }

    pub async fn request_by_client(
        &mut self,
        agent_id: &str,
        client_req_id: &str,
    ) -> ApiResult<Option<Request>> {
        Ok(self
            .requests_where(
                "agent_id = ? AND client_req_id = ?",
                &[agent_id, client_req_id],
            )
            .await?
            .pop())
    }

    pub async fn request_assignees(&mut self, id: &str) -> ApiResult<Vec<String>> {
        let rows = sqlx::query(
            self.d
                .q("SELECT user_id FROM request_assignees WHERE request_id = ? ORDER BY user_id"),
        )
        .bind(id)
        .fetch_all(&mut *self.c)
        .await?;
        rows.iter()
            .map(|r| r.try_get(0))
            .collect::<sqlx::Result<_>>()
            .map_err(Into::into)
    }

    /// Pending requests assigned to a person, or made by the given agents.
    pub async fn pending_requests_for(
        &mut self,
        user_id: &str,
        agent_ids: &[String],
    ) -> ApiResult<Vec<Request>> {
        let mut out = self
            .requests_where(
                "state = 'pending' AND request_id IN (SELECT request_id FROM request_assignees WHERE user_id = ?)",
                &[user_id],
            )
            .await?;
        for a in agent_ids {
            for q in self
                .requests_where("state = 'pending' AND agent_id = ?", &[a])
                .await?
            {
                if !out.iter().any(|x| x.request_id == q.request_id) {
                    out.push(q);
                }
            }
        }
        out.sort_by(|a, b| (a.created_at, &a.request_id).cmp(&(b.created_at, &b.request_id)));
        Ok(out)
    }

    pub async fn pending_request_count(&mut self, agent_id: &str) -> ApiResult<i64> {
        let r = sqlx::query(
            self.d
                .q("SELECT COUNT(*) FROM requests WHERE agent_id = ? AND state = 'pending'"),
        )
        .bind(agent_id)
        .fetch_one(&mut *self.c)
        .await?;
        Ok(r.try_get::<i64, _>(0)?)
    }

    /// Moves a pending request to its final state; `false` when it was
    /// already decided (exactly one decision wins).
    pub async fn finish_request(
        &mut self,
        q: &Request,
        state: &str,
        by: Option<&str>,
        response: Option<&Value>,
    ) -> ApiResult<bool> {
        let id = &q.request_id;
        let sealed = self
            .seal_json(&q.conv_id, &format!("requests.response:{id}"), response)
            .await?;
        let n = sqlx::query(self.d.q("UPDATE requests SET state = ?, decided_by = ?, decided_at = ?, response = ? WHERE request_id = ? AND state = 'pending'"))
            .bind(state)
            .bind(by.map(String::from))
            .bind(now_ms())
            .bind(sealed)
            .bind(id)
            .execute(&mut *self.c)
            .await?
            .rows_affected();
        Ok(n == 1)
    }

    /// Pending requests past their expiry.
    pub async fn expired_requests(&mut self, now: i64) -> ApiResult<Vec<Request>> {
        let sql = format!(
            "SELECT {REQUEST_COLS} FROM requests WHERE state = 'pending' AND expires_at <= ? ORDER BY created_at, request_id"
        );
        let rows = sqlx::query(self.d.q(&sql))
            .bind(now)
            .fetch_all(&mut *self.c)
            .await?;
        self.preload(&rows, &["title", "context", "response"])
            .await?;
        rows.iter()
            .map(|r| row_request(r, self.cx))
            .collect::<sqlx::Result<_>>()
            .map_err(Into::into)
    }

    pub async fn decide_draft(
        &mut self,
        id: &str,
        state: &str,
        by: &str,
        audience: &[String],
        conv_id: &str,
    ) -> ApiResult<SideEvent> {
        let n = sqlx::query(self.d.q("UPDATE drafts SET state = ?, decided_by = ?, decided_at = ? WHERE draft_id = ? AND state = ?"))
            .bind(state)
            .bind(by)
            .bind(now_ms())
            .bind(id)
            .bind("pending")
            .execute(&mut *self.c)
            .await?
            .rows_affected();
        if n == 0 {
            return Err(ApiError::new("conflict", "draft already decided"));
        }
        self.append_side(
            conv_id,
            audience,
            "draft.decided",
            Some(by),
            json!({"draft_id": id, "state": state}),
        )
        .await
    }

    // ----------------------------------------------------------- takeovers

    pub async fn takeover(&mut self, conv_id: &str, agent_id: &str) -> ApiResult<Option<String>> {
        let r = sqlx::query(
            self.d
                .q("SELECT by_user FROM takeovers WHERE conv_id = ? AND agent_id = ?"),
        )
        .bind(conv_id)
        .bind(agent_id)
        .fetch_optional(&mut *self.c)
        .await?;
        r.map(|r| r.try_get(0)).transpose().map_err(Into::into)
    }

    pub async fn set_takeover(
        &mut self,
        conv_id: &str,
        agent_id: &str,
        by: Option<&str>,
    ) -> ApiResult<Event> {
        self.set_takeover_with(conv_id, agent_id, by, None, json!({}))
            .await
    }

    /// Takes over (`by` Some) or releases an agent in a conversation;
    /// `note` marks an agent's own handoff. `extra` joins the event data.
    pub async fn set_takeover_with(
        &mut self,
        conv_id: &str,
        agent_id: &str,
        by: Option<&str>,
        note: Option<&str>,
        extra: Value,
    ) -> ApiResult<Event> {
        sqlx::query(
            self.d
                .q("DELETE FROM takeovers WHERE conv_id = ? AND agent_id = ?"),
        )
        .bind(conv_id)
        .bind(agent_id)
        .execute(&mut *self.c)
        .await?;
        if let Some(u) = by {
            let note = match note {
                Some(n) => Some(
                    self.seal(conv_id, &format!("takeovers.note:{conv_id}:{agent_id}"), n)
                        .await?,
                ),
                None => None,
            };
            sqlx::query(self.d.q(
                "INSERT INTO takeovers (conv_id, agent_id, by_user, since, note) VALUES (?, ?, ?, ?, ?)",
            ))
            .bind(conv_id)
            .bind(agent_id)
            .bind(u)
            .bind(now_ms())
            .bind(note)
            .execute(&mut *self.c)
            .await?;
        }
        let state = if by.is_some() { "taken_over" } else { "active" };
        let actor = by.unwrap_or(agent_id);
        let mut data = json!({"agent_id": agent_id, "state": state, "by": by});
        if let (Some(d), Some(x)) = (data.as_object_mut(), extra.as_object()) {
            for (k, v) in x {
                d.insert(k.clone(), v.clone());
            }
        }
        self.append_event(conv_id, "agent.state", Some(actor), None, data)
            .await
    }

    /// Every takeover: (conv, agent, by, since).
    pub async fn takeovers(&mut self) -> ApiResult<Vec<(String, String, String, i64)>> {
        let rows = sqlx::query("SELECT conv_id, agent_id, by_user, since FROM takeovers")
            .fetch_all(&mut *self.c)
            .await?;
        rows.iter()
            .map(|r| Ok((r.try_get(0)?, r.try_get(1)?, r.try_get(2)?, r.try_get(3)?)))
            .collect::<sqlx::Result<_>>()
            .map_err(Into::into)
    }

    /// When a user last wrote an event in a conversation.
    pub async fn last_said(&mut self, conv_id: &str, user_id: &str) -> ApiResult<Option<i64>> {
        let r = sqlx::query(
            self.d
                .q("SELECT MAX(created_at) FROM events WHERE conv_id = ? AND actor_id = ?"),
        )
        .bind(conv_id)
        .bind(user_id)
        .fetch_one(&mut *self.c)
        .await?;
        Ok(r.try_get::<Option<i64>, _>(0)?)
    }

    /// Handoffs waiting for a person: (conv, agent, since, note).
    pub async fn handoffs_to(
        &mut self,
        user_id: &str,
    ) -> ApiResult<Vec<(String, String, i64, String)>> {
        let rows = sqlx::query(self.d.q(
            "SELECT conv_id, agent_id, since, note FROM takeovers WHERE by_user = ? AND note IS NOT NULL ORDER BY since",
        ))
        .bind(user_id)
        .fetch_all(&mut *self.c)
        .await?;
        self.preload(&rows, &["note"]).await?;
        let mut out = vec![];
        for r in &rows {
            let (c, a): (String, String) = (r.try_get(0)?, r.try_get(1)?);
            let note = dec(self.cx, r, "note", &format!("takeovers.note:{c}:{a}"))?;
            out.push((c, a, r.try_get(2)?, note));
        }
        Ok(out)
    }

    // --------------------------------------------------------------- audit

    /// Moves a webhook cursor forward and deletes outbox rows every
    /// configured webhook has handled.
    pub async fn webhook_ack(&mut self, name: &str, upto: i64, all: &[String]) -> ApiResult<()> {
        let key = crate::webhook::cursor_key(name);
        let cur = self.meta_i64(&key).await?.unwrap_or(0);
        if upto > cur {
            sqlx::query(self.d.q("UPDATE meta SET v = ? WHERE k = ?"))
                .bind(upto.to_string())
                .bind(key)
                .execute(&mut *self.c)
                .await?;
        }
        let mut min = i64::MAX;
        for w in all {
            min = min.min(
                self.meta_i64(&crate::webhook::cursor_key(w))
                    .await?
                    .unwrap_or(0),
            );
        }
        if min != i64::MAX && min > 0 {
            sqlx::query(self.d.q("DELETE FROM webhook_outbox WHERE id <= ?"))
                .bind(min)
                .execute(&mut *self.c)
                .await?;
        }
        Ok(())
    }

    /// Counts a change other processes must notice (another server on the
    /// same database, or the CLI writing while the server runs).
    pub async fn bump_meta(&mut self, k: &str) -> ApiResult<i64> {
        let n = self.meta_i64(k).await?;
        let next = n.unwrap_or(0) + 1;
        let sql = if n.is_some() {
            "UPDATE meta SET v = ? WHERE k = ?"
        } else {
            "INSERT INTO meta (v, k) VALUES (?, ?)"
        };
        sqlx::query(self.d.q(sql))
            .bind(next.to_string())
            .bind(k)
            .execute(&mut *self.c)
            .await?;
        Ok(next)
    }

    /// Changes to tokens and deactivations so far (M10).
    pub async fn auth_epoch(&mut self) -> ApiResult<i64> {
        Ok(self.meta_i64(AUTH_EPOCH).await?.unwrap_or(0))
    }

    async fn meta_i64(&mut self, k: &str) -> ApiResult<Option<i64>> {
        let r = sqlx::query(self.d.q("SELECT v FROM meta WHERE k = ?"))
            .bind(k.to_string())
            .fetch_optional(&mut *self.c)
            .await?;
        Ok(r.and_then(|r| r.try_get::<String, _>(0).ok())
            .and_then(|v| v.parse().ok()))
    }

    pub async fn audit(
        &mut self,
        actor: Option<&str>,
        action: &str,
        conv_id: Option<&str>,
        target: Option<&str>,
        detail: Value,
    ) -> ApiResult<()> {
        let id = new_id();
        let detail = self
            .seal("", &format!("audit.detail:{id}"), &detail.to_string())
            .await?;
        sqlx::query(self.d.q("INSERT INTO audit (id, ts, actor_id, action, conv_id, target, detail) VALUES (?, ?, ?, ?, ?, ?, ?)"))
            .bind(&id)
            .bind(now_ms())
            .bind(actor)
            .bind(action)
            .bind(conv_id)
            .bind(target)
            .bind(&detail)
            .execute(&mut *self.c)
            .await?;
        Ok(())
    }

    pub async fn audit_log(&mut self, limit: i64) -> ApiResult<Vec<Value>> {
        let rows = sqlx::query(self.d.q("SELECT id, ts, actor_id, action, conv_id, target, detail FROM audit ORDER BY id DESC LIMIT ?"))
            .bind(limit)
            .fetch_all(&mut *self.c)
            .await?;
        self.preload(&rows, &["detail"]).await?;
        rows.iter()
            .map(|r| {
                Ok(json!({
                    "ts": r.try_get::<i64, _>("ts")?,
                    "actor_id": r.try_get::<Option<String>, _>("actor_id")?,
                    "action": r.try_get::<String, _>("action")?,
                    "conv_id": r.try_get::<Option<String>, _>("conv_id")?,
                    "target": r.try_get::<Option<String>, _>("target")?,
                    "detail": serde_json::from_str::<Value>(&dec(self.cx, r, "detail", &format!("audit.detail:{}", r.try_get::<String, _>("id")?))?).unwrap_or(Value::Null),
                }))
            })
            .collect::<sqlx::Result<_>>()
            .map_err(Into::into)
    }

    /// Audit records newest first: `action` starting with `prefix`, older
    /// than the record `before` (an id), at most `limit`.
    pub async fn audit_page(
        &mut self,
        prefix: Option<&str>,
        before: Option<&str>,
        limit: i64,
    ) -> ApiResult<Vec<Value>> {
        let id = self.bytewise("id");
        let rows = sqlx::query(self.d.q(&format!(
            "SELECT id, ts, actor_id, action, conv_id, target, detail FROM audit
             WHERE action LIKE ? ESCAPE '!' AND {id} < ? ORDER BY {id} DESC LIMIT ?"
        )))
        .bind(format!("{}%", like_escape(prefix.unwrap_or(""))))
        .bind(before.unwrap_or("~"))
        .bind(limit)
        .fetch_all(&mut *self.c)
        .await?;
        self.preload(&rows, &["detail"]).await?;
        rows.iter()
            .map(|r| {
                let id: String = r.try_get("id")?;
                Ok(json!({
                    "id": id,
                    "ts": r.try_get::<i64, _>("ts")?,
                    "actor_id": r.try_get::<Option<String>, _>("actor_id")?,
                    "action": r.try_get::<String, _>("action")?,
                    "conv_id": r.try_get::<Option<String>, _>("conv_id")?,
                    "target": r.try_get::<Option<String>, _>("target")?,
                    "detail": serde_json::from_str::<Value>(&dec(self.cx, r, "detail", &format!("audit.detail:{id}"))?).unwrap_or(Value::Null),
                }))
            })
            .collect::<sqlx::Result<_>>()
            .map_err(Into::into)
    }

    /// Conversations of this database by id after `after`, titles
    /// containing `q` (case-sensitive), with their member counts.
    pub async fn conversations_page(
        &mut self,
        q: Option<&str>,
        after: Option<&str>,
        limit: i64,
    ) -> ApiResult<Vec<Value>> {
        let id = self.bytewise("c.id");
        let rows = sqlx::query(self.d.q(&format!(
            "SELECT c.id, c.kind, c.title, c.last_seq, c.created_at,
                    (SELECT COUNT(*) FROM members m WHERE m.conv_id = c.id) AS n
             FROM conversations c WHERE {id} > ? AND (c.title LIKE ? ESCAPE '!' OR c.id = ?)
             ORDER BY {id} LIMIT ?"
        )))
        .bind(after.unwrap_or(""))
        .bind(format!("%{}%", like_escape(q.unwrap_or(""))))
        .bind(q.unwrap_or(""))
        .bind(limit)
        .fetch_all(&mut *self.c)
        .await?;
        rows.iter()
            .map(|r| {
                Ok(json!({
                    "id": r.try_get::<String, _>("id")?,
                    "kind": r.try_get::<String, _>("kind")?,
                    "title": r.try_get::<String, _>("title")?,
                    "last_seq": r.try_get::<i64, _>("last_seq")?,
                    "created_at": r.try_get::<i64, _>("created_at")?,
                    "members": r.try_get::<i64, _>("n")?,
                }))
            })
            .collect::<sqlx::Result<_>>()
            .map_err(Into::into)
    }

    pub async fn counts(&mut self) -> ApiResult<Value> {
        let mut out = serde_json::Map::new();
        for (k, sql) in [
            ("conversations", "SELECT COUNT(*) FROM conversations"),
            ("events", "SELECT COUNT(*) FROM events"),
            ("messages", "SELECT COUNT(*) FROM messages"),
            (
                "agent_messages",
                "SELECT COUNT(*) FROM messages WHERE sender_type = 'agent'",
            ),
            ("side_events", "SELECT COUNT(*) FROM side_events"),
            (
                "pending_drafts",
                "SELECT COUNT(*) FROM drafts WHERE state = 'pending'",
            ),
            ("webhook_outbox", "SELECT COUNT(*) FROM webhook_outbox"),
        ] {
            let v: i64 = sqlx::query(sql).fetch_one(&mut *self.c).await?.try_get(0)?;
            out.insert(k.into(), json!(v));
        }
        Ok(Value::Object(out))
    }

    /// Dedup records older than `before` (ms) are dropped; a retry after the
    /// TTL is treated as a new message.
    pub async fn dedup_gc(&mut self, before: i64) -> ApiResult<u64> {
        Ok(
            sqlx::query(self.d.q("DELETE FROM dedup WHERE created_at < ?"))
                .bind(before)
                .execute(&mut *self.c)
                .await?
                .rows_affected(),
        )
    }

    // ------------------------------- agent checkpoints (docs/03-agents.md §7.2)
    //
    // One row per version of a key; the current value is the highest
    // version. Values are encrypted with the agent's own data key (scope =
    // the agent id), destroyed with the agent.

    /// A text column compared and ordered byte-wise on every database
    /// (PostgreSQL's linguistic collations do not; MySQL tables are
    /// `utf8mb4_bin`, SQLite compares bytes).
    fn bytewise(&self, col: &str) -> String {
        if self.d == Dialect::Postgres {
            format!("{col} COLLATE \"C\"")
        } else {
            col.to_string()
        }
    }

    /// Keys compare byte-wise on every database (PostgreSQL's default
    /// collation does not).
    fn key_col(&self) -> &'static str {
        if self.d == Dialect::Postgres {
            "state_key COLLATE \"C\""
        } else {
            "state_key"
        }
    }

    /// A version of a key (the current one when `version` is None):
    /// (version, value, updated_at, current version).
    pub async fn state(
        &mut self,
        agent_id: &str,
        key: &str,
        version: Option<i64>,
    ) -> ApiResult<Option<(i64, Value, i64, i64)>> {
        let rows = sqlx::query(self.d.q(
            "SELECT version, value, updated_at FROM agent_state WHERE agent_id = ? AND state_key = ? ORDER BY version DESC",
        ))
        .bind(agent_id)
        .bind(key)
        .fetch_all(&mut *self.c)
        .await?;
        let Some(cur) = rows.first().map(|r| r.try_get::<i64, _>(0)).transpose()? else {
            return Ok(None);
        };
        let want = version.unwrap_or(cur);
        let Some(r) = rows
            .iter()
            .find(|r| r.try_get::<i64, _>(0).ok() == Some(want))
        else {
            return Ok(None);
        };
        self.preload(std::slice::from_ref(r), &["value"]).await?;
        let text = dec(
            self.cx,
            r,
            "value",
            &format!("agent_state.value:{agent_id}:{key}:{want}"),
        )?;
        Ok(Some((
            want,
            serde_json::from_str(&text).unwrap_or(Value::Null),
            r.try_get(2)?,
            cur,
        )))
    }

    /// The current version of a key, 0 when there is none.
    pub async fn state_head(&mut self, agent_id: &str, key: &str) -> ApiResult<i64> {
        let r = sqlx::query(
            self.d
                .q("SELECT MAX(version) FROM agent_state WHERE agent_id = ? AND state_key = ?"),
        )
        .bind(agent_id)
        .bind(key)
        .fetch_one(&mut *self.c)
        .await?;
        Ok(r.try_get::<Option<i64>, _>(0)?.unwrap_or(0))
    }

    pub async fn state_keys(&mut self, agent_id: &str) -> ApiResult<i64> {
        Ok(sqlx::query(
            self.d
                .q("SELECT COUNT(DISTINCT state_key) FROM agent_state WHERE agent_id = ?"),
        )
        .bind(agent_id)
        .fetch_one(&mut *self.c)
        .await?
        .try_get(0)?)
    }

    /// Writes `version` of a key and keeps the latest `keep` older ones.
    pub async fn put_state(
        &mut self,
        agent_id: &str,
        key: &str,
        version: i64,
        value: &str,
        keep: i64,
    ) -> ApiResult<()> {
        let sealed = self
            .seal(
                agent_id,
                &format!("agent_state.value:{agent_id}:{key}:{version}"),
                value,
            )
            .await?;
        sqlx::query(self.d.q(
            "INSERT INTO agent_state (agent_id, state_key, version, value, updated_at) VALUES (?, ?, ?, ?, ?)",
        ))
        .bind(agent_id)
        .bind(key)
        .bind(version)
        .bind(sealed)
        .bind(now_ms())
        .execute(&mut *self.c)
        .await?;
        sqlx::query(
            self.d
                .q("DELETE FROM agent_state WHERE agent_id = ? AND state_key = ? AND version < ?"),
        )
        .bind(agent_id)
        .bind(key)
        .bind(version - keep)
        .execute(&mut *self.c)
        .await?;
        Ok(())
    }

    /// Removes a key with all its versions.
    pub async fn delete_state(&mut self, agent_id: &str, key: &str) -> ApiResult<()> {
        sqlx::query(
            self.d
                .q("DELETE FROM agent_state WHERE agent_id = ? AND state_key = ?"),
        )
        .bind(agent_id)
        .bind(key)
        .execute(&mut *self.c)
        .await?;
        Ok(())
    }

    /// Keys after `after` starting with `prefix`, in byte order:
    /// (key, current version, updated_at), and whether more follow.
    pub async fn list_state(
        &mut self,
        agent_id: &str,
        prefix: &str,
        after: &str,
        limit: i64,
    ) -> ApiResult<(Vec<(String, i64, i64)>, bool)> {
        let k = self.key_col();
        let (from, op) = if after.as_bytes() >= prefix.as_bytes() {
            (after, ">")
        } else {
            (prefix, ">=")
        };
        let rows = sqlx::query(self.d.q(&format!(
            "SELECT state_key, MAX(version), MAX(updated_at) FROM agent_state WHERE agent_id = ? AND {k} {op} ? GROUP BY state_key ORDER BY {k} LIMIT ?"
        )))
        .bind(agent_id)
        .bind(from)
        .bind(limit + 1)
        .fetch_all(&mut *self.c)
        .await?;
        let mut out = vec![];
        let mut more = false;
        for r in rows {
            let key: String = r.try_get(0)?;
            if !key.starts_with(prefix) {
                break;
            }
            if out.len() as i64 == limit {
                more = true;
                break;
            }
            out.push((key, r.try_get(1)?, r.try_get(2)?));
        }
        Ok((out, more))
    }

    // ------------------------------------- cards (docs/03-agents.md §3)

    pub async fn card_responses(&mut self, msg_id: &str) -> ApiResult<i64> {
        Ok(sqlx::query(
            self.d
                .q("SELECT COUNT(*) FROM card_responses WHERE msg_id = ?"),
        )
        .bind(msg_id)
        .fetch_one(&mut *self.c)
        .await?
        .try_get(0)?)
    }

    /// Records a member's response; false when they already responded.
    pub async fn put_card_response(
        &mut self,
        msg_id: &str,
        conv_id: &str,
        user_id: &str,
    ) -> ApiResult<bool> {
        let seen = sqlx::query(
            self.d
                .q("SELECT 1 FROM card_responses WHERE msg_id = ? AND user_id = ?"),
        )
        .bind(msg_id)
        .bind(user_id)
        .fetch_optional(&mut *self.c)
        .await?;
        if seen.is_some() {
            return Ok(false);
        }
        sqlx::query(self.d.q(
            "INSERT INTO card_responses (msg_id, user_id, conv_id, created_at) VALUES (?, ?, ?, ?)",
        ))
        .bind(msg_id)
        .bind(user_id)
        .bind(conv_id)
        .bind(now_ms())
        .execute(&mut *self.c)
        .await?;
        Ok(true)
    }

    // ----------------------------------- feedback (docs/03-agents.md §3)

    /// A person's current feedback on a message: (rating, reason).
    pub async fn feedback(
        &mut self,
        msg_id: &str,
        user_id: &str,
    ) -> ApiResult<Option<(String, Option<String>)>> {
        let rows = sqlx::query(
            self.d
                .q("SELECT rating, reason FROM feedback WHERE msg_id = ? AND user_id = ?"),
        )
        .bind(msg_id)
        .bind(user_id)
        .fetch_all(&mut *self.c)
        .await?;
        self.preload(&rows, &["reason"]).await?;
        let Some(r) = rows.first() else {
            return Ok(None);
        };
        Ok(Some((
            r.try_get(0)?,
            dec_opt(
                self.cx,
                r,
                "reason",
                &format!("feedback.reason:{msg_id}:{user_id}"),
            )?,
        )))
    }

    /// Sets (Some) or clears (None) a person's feedback on a message.
    #[allow(clippy::too_many_arguments)]
    pub async fn set_feedback(
        &mut self,
        msg_id: &str,
        user_id: &str,
        conv_id: &str,
        agent_id: &str,
        rating: Option<&str>,
        reason: Option<&str>,
    ) -> ApiResult<()> {
        sqlx::query(
            self.d
                .q("DELETE FROM feedback WHERE msg_id = ? AND user_id = ?"),
        )
        .bind(msg_id)
        .bind(user_id)
        .execute(&mut *self.c)
        .await?;
        if let Some(r) = rating {
            let reason = match reason {
                Some(x) => Some(
                    self.seal(conv_id, &format!("feedback.reason:{msg_id}:{user_id}"), x)
                        .await?,
                ),
                None => None,
            };
            sqlx::query(self.d.q(
                "INSERT INTO feedback (msg_id, user_id, conv_id, agent_id, rating, reason, created_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
            ))
            .bind(msg_id)
            .bind(user_id)
            .bind(conv_id)
            .bind(agent_id)
            .bind(r)
            .bind(reason)
            .bind(now_ms())
            .execute(&mut *self.c)
            .await?;
        }
        Ok(())
    }

    /// An agent's feedback in this database: counts (up, down) and the
    /// latest `limit` entries.
    pub async fn feedback_of(
        &mut self,
        agent_id: &str,
        limit: i64,
    ) -> ApiResult<(i64, i64, Vec<Value>)> {
        let counts = sqlx::query(
            self.d
                .q("SELECT rating, COUNT(*) FROM feedback WHERE agent_id = ? GROUP BY rating"),
        )
        .bind(agent_id)
        .fetch_all(&mut *self.c)
        .await?;
        let (mut up, mut down) = (0, 0);
        for r in counts {
            let k: String = r.try_get(0)?;
            let n: i64 = r.try_get(1)?;
            if k == "up" {
                up = n;
            } else {
                down = n;
            }
        }
        let rows = sqlx::query(self.d.q(
            "SELECT msg_id, user_id, conv_id, rating, reason, created_at FROM feedback WHERE agent_id = ? ORDER BY created_at DESC, msg_id DESC LIMIT ?",
        ))
        .bind(agent_id)
        .bind(limit)
        .fetch_all(&mut *self.c)
        .await?;
        self.preload(&rows, &["reason"]).await?;
        let mut out = vec![];
        for r in &rows {
            let (m, u): (String, String) = (r.try_get(0)?, r.try_get(1)?);
            out.push(json!({
                "msg_id": m,
                "by": u,
                "conv_id": r.try_get::<String, _>(2)?,
                "rating": r.try_get::<String, _>(3)?,
                "reason": dec_opt(self.cx, r, "reason", &format!("feedback.reason:{m}:{u}"))?,
                "created_at": r.try_get::<i64, _>(5)?,
            }));
        }
        Ok((up, down, out))
    }

    // --------------------- stopping and destroying agents (docs/03-agents.md §4.3–§4.4)

    /// Stops or resumes an agent; false when it already was so.
    pub async fn set_stopped(&mut self, agent_id: &str, on: bool) -> ApiResult<bool> {
        let sql = if on {
            "UPDATE users SET stopped_at = ? WHERE id = ? AND kind = 'agent' AND stopped_at IS NULL AND destroyed_at IS NULL"
        } else {
            "UPDATE users SET stopped_at = ? WHERE id = ? AND kind = 'agent' AND stopped_at IS NOT NULL AND destroyed_at IS NULL"
        };
        Ok(sqlx::query(self.d.q(sql))
            .bind(on.then(now_ms))
            .bind(agent_id)
            .execute(&mut *self.c)
            .await?
            .rows_affected()
            == 1)
    }

    /// Agents that are stopped or destroyed: (id, stopped, destroyed).
    pub async fn agent_flags(&mut self) -> ApiResult<Vec<(String, bool, bool)>> {
        let rows = sqlx::query(
            "SELECT id, stopped_at, destroyed_at FROM users WHERE kind = 'agent' AND (stopped_at IS NOT NULL OR destroyed_at IS NOT NULL)",
        )
        .fetch_all(&mut *self.c)
        .await?;
        rows.iter()
            .map(|r| {
                Ok((
                    r.try_get(0)?,
                    r.try_get::<Option<i64>, _>(1)?.is_some(),
                    r.try_get::<Option<i64>, _>(2)?.is_some(),
                ))
            })
            .collect::<sqlx::Result<_>>()
            .map_err(Into::into)
    }

    /// Marks an agent destroyed (and deactivated); false when it already
    /// was. Its checkpoints and their data keys are deleted.
    pub async fn destroy_agent(&mut self, agent_id: &str) -> ApiResult<bool> {
        let now = now_ms();
        let n = sqlx::query(self.d.q(
            "UPDATE users SET destroyed_at = ?, deactivated_at = ?, stopped_at = NULL WHERE id = ? AND kind = 'agent' AND destroyed_at IS NULL",
        ))
        .bind(now)
        .bind(now)
        .bind(agent_id)
        .execute(&mut *self.c)
        .await?
        .rows_affected();
        if n != 1 {
            return Ok(false);
        }
        sqlx::query(self.d.q("DELETE FROM agent_state WHERE agent_id = ?"))
            .bind(agent_id)
            .execute(&mut *self.c)
            .await?;
        self.shred_scope(agent_id).await?;
        Ok(true)
    }

    /// What a destroyed agent leaves in a conversation besides its
    /// messages: drafts, claims, summaries and the floor.
    pub async fn drop_agent_in_conv(&mut self, conv_id: &str, agent_id: &str) -> ApiResult<()> {
        for (t, col) in [
            ("drafts", "agent_id"),
            ("claims", "user_id"),
            ("summaries", "agent_id"),
            ("floors", "agent_id"),
            ("takeovers", "agent_id"),
        ] {
            sqlx::query(
                self.d
                    .q(&format!("DELETE FROM {t} WHERE conv_id = ? AND {col} = ?")),
            )
            .bind(conv_id)
            .bind(agent_id)
            .execute(&mut *self.c)
            .await?;
        }
        Ok(())
    }

    // ------------------------------------- agent history (docs/03-agents.md §7.1)

    /// Events below `before` and above `floor`, newest first.
    pub async fn pull_before(
        &mut self,
        conv_id: &str,
        before: i64,
        floor: i64,
        limit: i64,
    ) -> ApiResult<Vec<Event>> {
        let mut q = sqlx::query(self.d.q(&format!(
            "SELECT conv_id, seq, type, actor_id, payload, created_at FROM events WHERE conv_id = ? AND seq < ? AND seq > ?{} ORDER BY seq DESC LIMIT ?",
            self.cap_sql()
        )))
        .bind(conv_id)
        .bind(before)
        .bind(floor);
        if self.cap {
            q = q.bind(conv_id);
        }
        let rows = q.bind(limit).fetch_all(&mut *self.c).await?;
        self.preload(&rows, &["payload"]).await?;
        rows.iter()
            .map(|x| row_event(x, self.cx))
            .collect::<sqlx::Result<_>>()
            .map_err(Into::into)
    }

    /// When the last event of a conversation happened (0 when none).
    pub async fn last_event_at(&mut self, conv_id: &str) -> ApiResult<i64> {
        let r = sqlx::query(
            self.d
                .q("SELECT created_at FROM events WHERE conv_id = ? ORDER BY seq DESC LIMIT 1"),
        )
        .bind(conv_id)
        .fetch_optional(&mut *self.c)
        .await?;
        Ok(match r {
            Some(r) => r.try_get(0)?,
            None => 0,
        })
    }

    /// Stores an agent's summary of a conversation, keeping its latest
    /// `keep` there.
    pub async fn put_summary(
        &mut self,
        conv_id: &str,
        agent_id: &str,
        from_seq: i64,
        to_seq: i64,
        text: &str,
        keep: usize,
    ) -> ApiResult<String> {
        let id = format!("s_{}", new_id());
        let sealed = self
            .seal(conv_id, &format!("summaries.text:{id}"), text)
            .await?;
        sqlx::query(self.d.q(
            "INSERT INTO summaries (summary_id, conv_id, agent_id, from_seq, to_seq, text, created_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
        ))
        .bind(&id)
        .bind(conv_id)
        .bind(agent_id)
        .bind(from_seq)
        .bind(to_seq)
        .bind(sealed)
        .bind(now_ms())
        .execute(&mut *self.c)
        .await?;
        let rows = sqlx::query(self.d.q(
            "SELECT summary_id FROM summaries WHERE conv_id = ? AND agent_id = ? ORDER BY created_at DESC, summary_id DESC",
        ))
        .bind(conv_id)
        .bind(agent_id)
        .fetch_all(&mut *self.c)
        .await?;
        for r in rows.iter().skip(keep) {
            let old: String = r.try_get(0)?;
            sqlx::query(self.d.q("DELETE FROM summaries WHERE summary_id = ?"))
                .bind(old)
                .execute(&mut *self.c)
                .await?;
        }
        Ok(id)
    }

    /// All of an agent's summaries of a conversation, oldest first.
    pub async fn summaries_of(&mut self, conv_id: &str, agent_id: &str) -> ApiResult<Vec<Value>> {
        let rows = sqlx::query(self.d.q(
            "SELECT summary_id, from_seq, to_seq, text, created_at FROM summaries WHERE conv_id = ? AND agent_id = ? ORDER BY created_at, summary_id",
        ))
        .bind(conv_id)
        .bind(agent_id)
        .fetch_all(&mut *self.c)
        .await?;
        self.preload(&rows, &["text"]).await?;
        let mut out = vec![];
        for r in &rows {
            let id: String = r.try_get(0)?;
            out.push(json!({
                "from_seq": r.try_get::<i64, _>(1)?,
                "to_seq": r.try_get::<i64, _>(2)?,
                "text": dec(self.cx, r, "text", &format!("summaries.text:{id}"))?,
                "created_at": r.try_get::<i64, _>(4)?,
            }));
        }
        Ok(out)
    }

    /// The agent's summary of a conversation that reaches furthest.
    pub async fn latest_summary(
        &mut self,
        conv_id: &str,
        agent_id: &str,
    ) -> ApiResult<Option<Value>> {
        let rows = sqlx::query(self.d.q(
            "SELECT summary_id, from_seq, to_seq, text, created_at FROM summaries WHERE conv_id = ? AND agent_id = ? ORDER BY to_seq DESC, created_at DESC LIMIT 1",
        ))
        .bind(conv_id)
        .bind(agent_id)
        .fetch_all(&mut *self.c)
        .await?;
        self.preload(&rows, &["text"]).await?;
        let Some(r) = rows.first() else {
            return Ok(None);
        };
        let id: String = r.try_get(0)?;
        let text = dec(self.cx, r, "text", &format!("summaries.text:{id}"))?;
        Ok(Some(json!({
            "summary_id": id,
            "from_seq": r.try_get::<i64, _>(1)?,
            "to_seq": r.try_get::<i64, _>(2)?,
            "text": text,
            "created_at": r.try_get::<i64, _>(4)?,
            "derived": true,
        })))
    }

    // ----------------------------------------------- migration between DBs

    /// Every row of every per-conversation table for one conversation.
    pub async fn export_conv(&mut self, conv_id: &str) -> ApiResult<Vec<(String, Vec<Vec<Cell>>)>> {
        let mut out = vec![];
        for t in TABLES.iter().filter(|t| t.conv_key.is_some()) {
            let cols: Vec<&str> = t.cols.iter().map(|c| c.name).collect();
            let sql = format!(
                "SELECT {} FROM {} WHERE {} = ? ORDER BY {}",
                cols.join(", "),
                t.name,
                t.conv_key.unwrap(),
                t.pk.join(", ")
            );
            let rows = sqlx::query(self.d.q(&sql))
                .bind(conv_id)
                .fetch_all(&mut *self.c)
                .await?;
            let mut data = vec![];
            for r in rows {
                let mut cells = vec![];
                for (i, c) in t.cols.iter().enumerate() {
                    cells.push(match c.ty {
                        Ty::Big => Cell::I(r.try_get::<Option<i64>, _>(i)?),
                        _ => Cell::S(r.try_get::<Option<String>, _>(i)?),
                    });
                }
                data.push(cells);
            }
            out.push((t.name.to_string(), data));
        }
        Ok(out)
    }

    /// Deletes a conversation's rows (after it moved to another database).
    pub async fn delete_conv(&mut self, conv_id: &str) -> ApiResult<()> {
        self.delete_conv_rows(conv_id, true).await
    }

    /// Deletes a dissolved conversation's content; its audit trail stays.
    pub async fn delete_conv_content(&mut self, conv_id: &str) -> ApiResult<()> {
        self.delete_conv_rows(conv_id, false).await
    }

    async fn delete_conv_rows(&mut self, conv_id: &str, with_audit: bool) -> ApiResult<()> {
        for t in TABLES
            .iter()
            .filter(|t| t.conv_key.is_some() && (with_audit || t.name != "audit"))
        {
            sqlx::query(self.d.q(&format!(
                "DELETE FROM {} WHERE {} = ?",
                t.name,
                t.conv_key.unwrap()
            )))
            .bind(conv_id)
            .execute(&mut *self.c)
            .await?;
        }
        Ok(())
    }

    /// Replaces whatever copy of the conversation exists here with `rows`.
    pub async fn import_conv(
        &mut self,
        conv_id: &str,
        rows: &[(String, Vec<Vec<Cell>>)],
    ) -> ApiResult<()> {
        self.delete_conv(conv_id).await?;
        for (name, data) in rows {
            let t = TABLES
                .iter()
                .find(|t| t.name == *name)
                .ok_or_else(|| ApiError::new("internal", "unknown table"))?;
            let cols: Vec<&str> = t.cols.iter().map(|c| c.name).collect();
            let sql = format!(
                "INSERT INTO {} ({}) VALUES ({})",
                t.name,
                cols.join(", "),
                vec!["?"; cols.len()].join(", ")
            );
            for row in data {
                let mut q = sqlx::query(self.d.q(&sql));
                for cell in row {
                    q = match cell {
                        Cell::S(v) => q.bind(v.clone()),
                        Cell::I(v) => q.bind(*v),
                    };
                }
                q.execute(&mut *self.c).await?;
            }
        }
        Ok(())
    }
}

/// Content fingerprint of an exported conversation, used to verify a
/// migration before switching the directory.
pub fn fingerprint(rows: &[(String, Vec<Vec<Cell>>)]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    for (name, data) in rows {
        h.update(name.as_bytes());
        for row in data {
            for cell in row {
                match cell {
                    Cell::S(Some(s)) => {
                        h.update(b"s");
                        h.update(s.as_bytes());
                    }
                    Cell::I(Some(i)) => {
                        h.update(b"i");
                        h.update(i.to_le_bytes());
                    }
                    _ => h.update(b"n"),
                }
                h.update([0]);
            }
        }
    }
    hex::encode(h.finalize())
}

/// Most members one `members.joined` event lists (D128).
pub const JOIN_BATCH: usize = 200;

/// Members who joined together are listed owner, admins, members (D128).
fn role_rank(role: &str) -> u8 {
    match role {
        "owner" => 0,
        "admin" => 1,
        _ => 2,
    }
}

/// Agents in groups only hear what is addressed to them by default.
fn trigger_for(u: &User, conv_kind: &str) -> Trigger {
    if u.kind == UserKind::Agent && conv_kind != "dm" {
        Trigger::Addressed
    } else {
        Trigger::All
    }
}

/// A joining member as `member.joined` carries it (and each entry of
/// `members.joined`).
fn member_entry(u: &User, role: &str, trigger: Trigger, history_from: Option<i64>) -> Value {
    json!({"user_id": u.id, "name": u.name, "kind": u.kind, "owner_id": u.owner_id, "role": role, "trigger": trigger, "history_from": history_from})
}
