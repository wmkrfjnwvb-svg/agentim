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

pub fn new_id() -> String {
    uuid::Uuid::now_v7().simple().to_string()
}

pub struct Db<'a> {
    pub c: &'a mut AnyConnection,
    pub d: Dialect,
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
}

/// A nullable JSON column.
fn json_col(r: &AnyRow, col: &str) -> sqlx::Result<Option<Value>> {
    Ok(r.try_get::<Option<String>, _>(col)?
        .and_then(|s| serde_json::from_str(&s).ok()))
}

/// An open API key (only its hash is stored).
#[derive(Debug, Clone)]
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

/// One client token = one device.
#[derive(Debug, Clone)]
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

#[derive(Debug, Clone)]
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
#[derive(Debug, Clone, PartialEq)]
pub enum Cell {
    S(Option<String>),
    I(Option<i64>),
}

const MSG_COLS: &str = "msg_id, conv_id, created_seq, sender_id, sender_name, sender_type, on_behalf_of, kind, content,
    version, state, state_reason, hop, causation_id, reply_to, mentions, created_at, updated_at, payload";
const MEMBER_COLS: &str = "user_id, name, kind, owner_id, role, trigger_mode, approval, join_seq";
const DRAFT_COLS: &str = "draft_id, conv_id, agent_id, content, hop, causation_id, reply_to, state, created_at, kind, payload";

fn row_user(r: &AnyRow) -> sqlx::Result<User> {
    Ok(User {
        id: r.try_get("id")?,
        name: r.try_get("name")?,
        kind: UserKind::parse(&r.try_get::<String, _>("kind")?),
        owner_id: r.try_get("owner_id")?,
        unit_id: r.try_get("unit_id")?,
    })
}

fn row_msg(r: &AnyRow) -> sqlx::Result<MsgView> {
    Ok(MsgView {
        msg_id: r.try_get("msg_id")?,
        conv_id: r.try_get("conv_id")?,
        created_seq: r.try_get("created_seq")?,
        sender_id: r.try_get("sender_id")?,
        sender_name: r.try_get("sender_name")?,
        sender_type: UserKind::parse(&r.try_get::<String, _>("sender_type")?),
        on_behalf_of: r.try_get("on_behalf_of")?,
        kind: r.try_get("kind")?,
        content: r.try_get("content")?,
        payload: json_col(r, "payload")?,
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
    })
}

fn row_event(r: &AnyRow) -> sqlx::Result<Event> {
    let payload: Value =
        serde_json::from_str(&r.try_get::<String, _>("payload")?).unwrap_or(Value::Null);
    Ok(Event {
        conv_id: r.try_get("conv_id")?,
        seq: r.try_get("seq")?,
        kind: r.try_get("type")?,
        actor_id: r.try_get("actor_id")?,
        msg: serde_json::from_value(payload["msg"].clone()).ok(),
        data: payload["data"].clone(),
        created_at: r.try_get("created_at")?,
    })
}

fn row_side(r: &AnyRow) -> sqlx::Result<SideEvent> {
    Ok(SideEvent {
        conv_id: r.try_get("conv_id")?,
        side_seq: r.try_get("side_seq")?,
        anchor_seq: r.try_get("anchor_seq")?,
        audience: serde_json::from_str(&r.try_get::<String, _>("audience")?).unwrap_or_default(),
        kind: r.try_get("type")?,
        actor_id: r.try_get("actor_id")?,
        data: serde_json::from_str(&r.try_get::<String, _>("payload")?).unwrap_or(Value::Null),
        created_at: r.try_get("created_at")?,
    })
}

fn row_draft(r: &AnyRow) -> sqlx::Result<Draft> {
    Ok(Draft {
        draft_id: r.try_get("draft_id")?,
        conv_id: r.try_get("conv_id")?,
        agent_id: r.try_get("agent_id")?,
        content: r.try_get("content")?,
        hop: r.try_get("hop")?,
        causation_id: r.try_get("causation_id")?,
        reply_to: r.try_get("reply_to")?,
        state: r.try_get("state")?,
        created_at: r.try_get("created_at")?,
        kind: r
            .try_get::<Option<String>, _>("kind")?
            .unwrap_or_else(|| "text".into()),
        payload: json_col(r, "payload")?,
    })
}

impl Db<'_> {
    // ===================================================== control database

    pub async fn insert_user(&mut self, u: &User) -> ApiResult<()> {
        let token = crate::auth::legacy_placeholder(&u.id);
        sqlx::query(self.d.q(
            "INSERT INTO users (id, tenant_id, unit_id, name, kind, owner_id, token, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        ))
        .bind(&u.id)
        .bind(TENANT_DEFAULT)
        .bind(&u.unit_id)
        .bind(&u.name)
        .bind(u.kind.as_str())
        .bind(&u.owner_id)
        .bind(&token)
        .bind(now_ms())
        .execute(&mut *self.c)
        .await
        .map_err(|e| ApiError::new("conflict", e.to_string()))?;
        Ok(())
    }

    /// All users and whether each is deactivated (the in-memory directory).
    pub async fn load_users(&mut self) -> ApiResult<Vec<(User, bool)>> {
        let rows = sqlx::query(
            "SELECT id, name, kind, owner_id, unit_id, deactivated_at FROM users ORDER BY created_at, id",
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
        for h in &hashes {
            sqlx::query(self.d.q("DELETE FROM tokens WHERE hash = ?"))
                .bind(h)
                .execute(&mut *self.c)
                .await?;
        }
        Ok(hashes)
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

    pub async fn touch_token(&mut self, hash: &str) -> ApiResult<()> {
        sqlx::query(self.d.q("UPDATE tokens SET last_seen = ? WHERE hash = ?"))
            .bind(now_ms())
            .bind(hash)
            .execute(&mut *self.c)
            .await?;
        Ok(())
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
            "SELECT id, kind, title, unit_id, last_seq, fields FROM conversations WHERE id = ?",
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
                last_seq: r.try_get("last_seq")?,
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

    async fn next_seq(&mut self, conv_id: &str) -> ApiResult<i64> {
        let n = sqlx::query(
            self.d
                .q("UPDATE conversations SET last_seq = last_seq + 1 WHERE id = ?"),
        )
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
        let payload = json!({"msg": msg, "data": data});
        sqlx::query(self.d.q(
            "INSERT INTO events (conv_id, seq, type, actor_id, msg_id, payload, created_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
        ))
        .bind(conv_id)
        .bind(seq)
        .bind(kind)
        .bind(actor)
        .bind(msg.as_ref().map(|m| m.msg_id.clone()))
        .bind(payload.to_string())
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

    pub async fn create_conv(
        &mut self,
        id: &str,
        kind: &str,
        title: &str,
        unit_id: &str,
        creator: &User,
        members: &[User],
    ) -> ApiResult<Vec<Event>> {
        sqlx::query(self.d.q(
            "INSERT INTO conversations (id, tenant_id, unit_id, kind, title, last_seq, side_seq, created_at) VALUES (?, ?, ?, ?, ?, 0, 0, ?)",
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
        for u in all {
            let role = if u.id == creator.id {
                "owner"
            } else {
                "member"
            };
            events.push(self.add_member(id, kind, u, role, &creator.id).await?);
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
    ) -> ApiResult<Event> {
        if self.member(conv_id, &u.id).await?.is_some() {
            return Err(ApiError::new("conflict", "already a member"));
        }
        // Agents in groups only hear what is addressed to them by default.
        let trigger = if u.kind == UserKind::Agent && conv_kind == "group" {
            Trigger::Mention
        } else {
            Trigger::All
        };
        let ev = self
            .append_event(
                conv_id,
                "member.joined",
                Some(actor),
                None,
                json!({"user_id": u.id, "name": u.name, "kind": u.kind, "owner_id": u.owner_id, "role": role, "trigger": trigger}),
            )
            .await?;
        sqlx::query(self.d.q(
            "INSERT INTO members (conv_id, user_id, name, kind, owner_id, role, trigger_mode, approval, join_seq) VALUES (?, ?, ?, ?, ?, ?, ?, 0, ?)",
        ))
        .bind(conv_id)
        .bind(&u.id)
        .bind(&u.name)
        .bind(u.kind.as_str())
        .bind(&u.owner_id)
        .bind(role)
        .bind(trigger.as_str())
        .bind(ev.seq)
        .execute(&mut *self.c)
        .await?;
        Ok(ev)
    }

    pub async fn members(&mut self, conv_id: &str) -> ApiResult<Vec<Member>> {
        let rows = sqlx::query(self.d.q(&format!(
            "SELECT {MEMBER_COLS} FROM members WHERE conv_id = ? ORDER BY join_seq"
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
        Ok(())
    }

    /// `{id, name, kind, owner_id, display_name, avatar, fields, deactivated_at, created_at}`.
    pub async fn user_profile(&mut self, id: &str) -> ApiResult<Option<Value>> {
        let r = sqlx::query(self.d.q(
            "SELECT id, name, kind, owner_id, unit_id, display_name, avatar, fields, deactivated_at, created_at FROM users WHERE id = ?",
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
        actor: &str,
    ) -> ApiResult<Event> {
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
            json!({"user_id": agent_id, "approval": m.approval, "trigger": m.trigger}),
        )
        .await
    }

    // ------------------------------------------------------------ messages

    /// Dedup + seq assignment + persistence; the caller's transaction makes
    /// them atomic.
    pub async fn commit_message(&mut self, m: NewMsg<'_>) -> ApiResult<Committed> {
        if let Some(cid) = m.client_msg_id {
            let prev = sqlx::query(self.d.q("SELECT result FROM dedup WHERE conv_id = ? AND sender_id = ? AND client_msg_id = ?"))
                .bind(m.conv_id)
                .bind(&m.sender.id)
                .bind(cid)
                .fetch_optional(&mut *self.c)
                .await?;
            if let Some(prev) = prev {
                let v: Value =
                    serde_json::from_str(&prev.try_get::<String, _>(0)?).unwrap_or(Value::Null);
                return Ok(Committed::Duplicate {
                    seq: v["seq"].as_i64().unwrap_or(0),
                    msg_id: v["msg_id"].as_str().unwrap_or("").to_string(),
                });
            }
        }
        let seq = self.next_seq(m.conv_id).await?;
        let ts = now_ms();
        let view = MsgView {
            msg_id: format!("m_{}", new_id()),
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
        };
        sqlx::query(self.d.q(&format!(
            "INSERT INTO messages ({MSG_COLS}) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
        )))
        .bind(&view.msg_id)
        .bind(&view.conv_id)
        .bind(seq)
        .bind(&view.sender_id)
        .bind(&view.sender_name)
        .bind(view.sender_type.as_str())
        .bind(&view.on_behalf_of)
        .bind(&view.kind)
        .bind(&view.content)
        .bind(view.version)
        .bind(&view.state)
        .bind(&view.state_reason)
        .bind(view.hop)
        .bind(&view.causation_id)
        .bind(&view.reply_to)
        .bind(serde_json::to_string(&view.mentions).unwrap())
        .bind(ts)
        .bind(ts)
        .bind(view.payload.as_ref().map(|p| p.to_string()))
        .execute(&mut *self.c)
        .await?;
        if let Some(cid) = m.client_msg_id {
            sqlx::query(self.d.q("INSERT INTO dedup (conv_id, sender_id, client_msg_id, result, created_at) VALUES (?, ?, ?, ?, ?)"))
                .bind(m.conv_id)
                .bind(&m.sender.id)
                .bind(cid)
                .bind(json!({"seq": seq, "msg_id": view.msg_id}).to_string())
                .bind(ts)
                .execute(&mut *self.c)
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
                json!({}),
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
        r.as_ref().map(row_msg).transpose().map_err(Into::into)
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
        self.append_event(&conv_id, kind, Some(actor), Some(view), json!({}))
            .await
    }

    /// Replaces a custom kind's payload (before `update_message` announces it).
    pub async fn set_payload(&mut self, msg_id: &str, payload: Option<&Value>) -> ApiResult<()> {
        sqlx::query(self.d.q("UPDATE messages SET payload = ? WHERE msg_id = ?"))
            .bind(payload.map(|p| p.to_string()))
            .bind(msg_id)
            .execute(&mut *self.c)
            .await?;
        Ok(())
    }

    /// Streaming checkpoint: overwrites partial content without an event.
    pub async fn checkpoint(&mut self, msg_id: &str, content: &str) -> ApiResult<()> {
        sqlx::query(
            self.d
                .q("UPDATE messages SET content = ?, updated_at = ? WHERE msg_id = ?"),
        )
        .bind(content)
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
        rows.iter()
            .map(row_msg)
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
        r.as_ref().map(row_msg).transpose().map_err(Into::into)
    }

    pub async fn recent_contents(
        &mut self,
        conv_id: &str,
        sender_id: &str,
        n: i64,
    ) -> ApiResult<Vec<String>> {
        let rows = sqlx::query(self.d.q(
            "SELECT content FROM messages WHERE conv_id = ? AND sender_id = ? ORDER BY created_seq DESC LIMIT ?",
        ))
        .bind(conv_id)
        .bind(sender_id)
        .bind(n)
        .fetch_all(&mut *self.c)
        .await?;
        rows.iter()
            .map(|r| r.try_get(0))
            .collect::<sqlx::Result<_>>()
            .map_err(Into::into)
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
        // The LIKE only narrows the scan; the JSON check below decides.
        let pattern = format!("%\"msg_id\":\"{msg_id}\"%");
        let rows = sqlx::query(self.d.q(
            "SELECT seq, payload FROM events WHERE conv_id = ? AND seq >= ? AND payload LIKE ?",
        ))
        .bind(conv_id)
        .bind(from_seq)
        .bind(pattern)
        .fetch_all(&mut *self.c)
        .await?;
        for r in rows {
            let seq: i64 = r.try_get(0)?;
            let mut p: Value =
                serde_json::from_str(&r.try_get::<String, _>(1)?).unwrap_or(Value::Null);
            if p["msg"]["msg_id"] != msg_id {
                continue;
            }
            p["msg"]["content"] = json!("");
            p["msg"]["payload"] = Value::Null;
            p["msg"]["mentions"] = json!([]);
            sqlx::query(
                self.d
                    .q("UPDATE events SET payload = ? WHERE conv_id = ? AND seq = ?"),
            )
            .bind(p.to_string())
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
        let rows = sqlx::query(self.d.q(
            "SELECT conv_id, seq, type, actor_id, payload, created_at FROM events WHERE conv_id = ? AND seq > ? ORDER BY seq LIMIT ?",
        ))
        .bind(conv_id)
        .bind(after_seq)
        .bind(limit)
        .fetch_all(&mut *self.c)
        .await?;
        rows.iter()
            .map(row_event)
            .collect::<sqlx::Result<_>>()
            .map_err(Into::into)
    }

    pub async fn pull_side(&mut self, conv_id: &str, after: i64) -> ApiResult<Vec<SideEvent>> {
        let rows = sqlx::query(self.d.q(
            "SELECT conv_id, side_seq, anchor_seq, audience, type, actor_id, payload, created_at FROM side_events
             WHERE conv_id = ? AND side_seq > ? ORDER BY side_seq",
        ))
        .bind(conv_id)
        .bind(after)
        .fetch_all(&mut *self.c)
        .await?;
        rows.iter()
            .map(row_side)
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
        let read = self.read_seq(conv_id, user_id).await?;
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
        sqlx::query(self.d.q(
            "INSERT INTO side_events (conv_id, side_seq, anchor_seq, audience, type, actor_id, payload, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        ))
        .bind(conv_id)
        .bind(side_seq)
        .bind(anchor)
        .bind(serde_json::to_string(audience).unwrap())
        .bind(kind)
        .bind(actor)
        .bind(data.to_string())
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
        };
        sqlx::query(self.d.q(&format!(
            "INSERT INTO drafts ({DRAFT_COLS}) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
        )))
        .bind(&d.draft_id)
        .bind(&d.conv_id)
        .bind(&d.agent_id)
        .bind(&d.content)
        .bind(d.hop)
        .bind(&d.causation_id)
        .bind(&d.reply_to)
        .bind(&d.state)
        .bind(d.created_at)
        .bind(&d.kind)
        .bind(d.payload.as_ref().map(|p| p.to_string()))
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
        r.as_ref().map(row_draft).transpose().map_err(Into::into)
    }

    pub async fn pending_drafts_for(&mut self, agent_ids: &[String]) -> ApiResult<Vec<Draft>> {
        let mut out = vec![];
        for a in agent_ids {
            let rows = sqlx::query(self.d.q(&format!("SELECT {DRAFT_COLS} FROM drafts WHERE agent_id = ? AND state = ? ORDER BY created_at")))
                .bind(a)
                .bind("pending")
                .fetch_all(&mut *self.c)
                .await?;
            for r in rows {
                out.push(row_draft(&r)?);
            }
        }
        out.sort_by_key(|d| d.created_at);
        Ok(out)
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
        sqlx::query(
            self.d
                .q("DELETE FROM takeovers WHERE conv_id = ? AND agent_id = ?"),
        )
        .bind(conv_id)
        .bind(agent_id)
        .execute(&mut *self.c)
        .await?;
        if let Some(u) = by {
            sqlx::query(self.d.q(
                "INSERT INTO takeovers (conv_id, agent_id, by_user, since) VALUES (?, ?, ?, ?)",
            ))
            .bind(conv_id)
            .bind(agent_id)
            .bind(u)
            .bind(now_ms())
            .execute(&mut *self.c)
            .await?;
        }
        let state = if by.is_some() { "taken_over" } else { "active" };
        let actor = by.unwrap_or(agent_id);
        self.append_event(
            conv_id,
            "agent.state",
            Some(actor),
            None,
            json!({"agent_id": agent_id, "state": state, "by": by}),
        )
        .await
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
        sqlx::query(self.d.q("INSERT INTO audit (id, ts, actor_id, action, conv_id, target, detail) VALUES (?, ?, ?, ?, ?, ?, ?)"))
            .bind(new_id())
            .bind(now_ms())
            .bind(actor)
            .bind(action)
            .bind(conv_id)
            .bind(target)
            .bind(detail.to_string())
            .execute(&mut *self.c)
            .await?;
        Ok(())
    }

    pub async fn audit_log(&mut self, limit: i64) -> ApiResult<Vec<Value>> {
        let rows = sqlx::query(self.d.q("SELECT ts, actor_id, action, conv_id, target, detail FROM audit ORDER BY id DESC LIMIT ?"))
            .bind(limit)
            .fetch_all(&mut *self.c)
            .await?;
        rows.iter()
            .map(|r| {
                Ok(json!({
                    "ts": r.try_get::<i64, _>("ts")?,
                    "actor_id": r.try_get::<Option<String>, _>("actor_id")?,
                    "action": r.try_get::<String, _>("action")?,
                    "conv_id": r.try_get::<Option<String>, _>("conv_id")?,
                    "target": r.try_get::<Option<String>, _>("target")?,
                    "detail": serde_json::from_str::<Value>(&r.try_get::<String, _>("detail")?).unwrap_or(Value::Null),
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

    // ----------------------------------------------- migration between DBs

    /// Every row of every per-conversation table for one conversation.
    pub async fn export_conv(
        &mut self,
        conv_id: &str,
    ) -> ApiResult<Vec<(&'static str, Vec<Vec<Cell>>)>> {
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
            out.push((t.name, data));
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
        rows: &[(&'static str, Vec<Vec<Cell>>)],
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
pub fn fingerprint(rows: &[(&'static str, Vec<Vec<Cell>>)]) -> String {
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
