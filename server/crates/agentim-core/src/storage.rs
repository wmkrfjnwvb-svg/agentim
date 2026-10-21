//! Database plumbing shared by every backend: dialect, connections, schema,
//! durability checks. Business SQL lives in `db.rs` and is written once; the
//! dialect layer only rewrites placeholders and maps column types.

use std::time::Duration;

use sqlx::any::AnyPoolOptions;
use sqlx::{AnyConnection, AnyPool};
use sqlx::{AssertSqlSafe, Connection, Row};

use crate::model::{ApiError, ApiResult};

/// Version of the schema in `TABLES`. Bump it together with a new entry in
/// `MIGRATIONS` (docs/02-architecture.md §5.3).
pub const SCHEMA_VERSION: i64 = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    Sqlite,
    Postgres,
    Mysql,
}

impl Dialect {
    pub fn from_url(url: &str) -> Result<Dialect, String> {
        let scheme = url.split(':').next().unwrap_or("");
        match scheme {
            "sqlite" => Ok(Dialect::Sqlite),
            "postgres" | "postgresql" => Ok(Dialect::Postgres),
            "mysql" | "mariadb" => Ok(Dialect::Mysql),
            _ => Err(format!("unsupported database url scheme {scheme:?}")),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Dialect::Sqlite => "sqlite",
            Dialect::Postgres => "postgres",
            Dialect::Mysql => "mysql",
        }
    }

    /// Statements are written with `?` placeholders; PostgreSQL wants `$n`.
    /// No statement contains a literal `?`.
    pub fn q(self, sql: &str) -> AssertSqlSafe<String> {
        if self != Dialect::Postgres {
            return AssertSqlSafe(sql.to_string());
        }
        let mut out = String::with_capacity(sql.len() + 8);
        let mut n = 0;
        for ch in sql.chars() {
            if ch == '?' {
                n += 1;
                out.push('$');
                out.push_str(&n.to_string());
            } else {
                out.push(ch);
            }
        }
        AssertSqlSafe(out)
    }
}

/// Rewrites a configured URL into the form the sqlx `Any` driver accepts.
pub(crate) fn driver_url(url: &str) -> String {
    // sqlx's MySQL driver only knows the mysql:// scheme.
    if let Some(r) = url.strip_prefix("mariadb://") {
        return format!("mysql://{r}");
    }
    // `sqlite://C:/x.db` is parsed as a URL with host "c", which loses the
    // drive letter; `sqlite:C:/x.db` keeps the whole path.
    if let Some(r) = url.strip_prefix("sqlite://") {
        let b = r.as_bytes();
        if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
            return format!("sqlite:{}", r.replace('\\', "/"));
        }
    }
    url.to_string()
}

/// The single connection a database's writer owns.
pub async fn open_writer(url: &str, d: Dialect) -> ApiResult<AnyConnection> {
    let mut c = tokio::time::timeout(CONNECT_TIMEOUT, AnyConnection::connect(&driver_url(url)))
        .await
        .map_err(|_| {
            ApiError::new(
                "storage_unavailable",
                "connecting to the database timed out",
            )
        })?
        .map_err(conn_err)?;
    if d == Dialect::Sqlite {
        // secure_delete: deleted content (e.g. a conversation moved to another
        // database) is overwritten instead of lingering in free pages.
        for p in [
            "PRAGMA busy_timeout = 5000",
            "PRAGMA journal_mode = WAL",
            "PRAGMA synchronous = FULL",
            "PRAGMA foreign_keys = ON",
            "PRAGMA secure_delete = ON",
        ] {
            sqlx::query(p).execute(&mut c).await?;
        }
    }
    Ok(c)
}

/// Most time to open a database connection.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Bounds on how long the writer waits for a lock or holds a transaction
/// open (docs/11 §4.4, E1): a lock held by another process (a migration,
/// another cluster node) fails the batch instead of stalling every write.
/// `stop_ms` (cluster): the session ends when idle that long, so a writer
/// cut off from the database cannot keep lease rows locked (docs/11 §4.6).
pub async fn writer_timeouts(
    c: &mut AnyConnection,
    d: Dialect,
    stop_ms: Option<u64>,
) -> ApiResult<()> {
    let stmts: Vec<String> = match d {
        Dialect::Postgres => vec![
            "SET lock_timeout = '2s'".into(),
            format!(
                "SET idle_in_transaction_session_timeout = '{}ms'",
                stop_ms.unwrap_or(30_000)
            ),
        ],
        Dialect::Mysql => {
            let mut v = vec!["SET SESSION innodb_lock_wait_timeout = 2".to_string()];
            if let Some(ms) = stop_ms {
                v.push(format!(
                    "SET SESSION wait_timeout = {}",
                    ms.div_ceil(1000).max(1)
                ));
            }
            v
        }
        Dialect::Sqlite => vec![],
    };
    for s in stmts {
        sqlx::query(AssertSqlSafe(s)).execute(&mut *c).await?;
    }
    Ok(())
}

/// A database error that leaves nothing behind and may succeed if tried
/// again: a deadlock, a lock wait that timed out, a serialization failure
/// (E2). The batch is rolled back and answered as retryable.
pub fn is_transient(message: &str) -> bool {
    let m = message.to_ascii_lowercase();
    [
        "deadlock",
        "lock wait timeout",
        "lock timeout",
        "could not serialize",
        "serialization failure",
        "try restarting transaction",
        "database is locked",
    ]
    .iter()
    .any(|p| m.contains(p))
}

/// The connection to the database broke (the server went away): the
/// outcome of what was in flight is unknown.
pub fn is_connection_lost(message: &str) -> bool {
    let m = message.to_ascii_lowercase();
    [
        "error communicating with database",
        "connection reset",
        "broken pipe",
        "unexpected end of file",
        "connection refused",
        "connection closed",
        "server has gone away",
        "lost connection",
    ]
    .iter()
    .any(|p| m.contains(p))
}

/// Read pool. Reads never go through the writer; they see committed data only.
pub async fn open_pool(url: &str, d: Dialect, size: u32) -> ApiResult<AnyPool> {
    let mut opts = AnyPoolOptions::new()
        .max_connections(size)
        .acquire_timeout(Duration::from_secs(10));
    if d == Dialect::Sqlite {
        opts = opts.after_connect(|c, _| {
            Box::pin(async move {
                sqlx::query("PRAGMA busy_timeout = 5000")
                    .execute(&mut *c)
                    .await?;
                Ok(())
            })
        });
    }
    opts.connect(&driver_url(url)).await.map_err(conn_err)
}

fn conn_err(e: sqlx::Error) -> ApiError {
    ApiError::new("storage_unavailable", e.to_string())
}

// ------------------------------------------------------------------- schema

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Ty {
    /// Identifiers, VARCHAR(64).
    Id,
    /// VARCHAR(128), e.g. client_msg_id.
    Key,
    /// Short human text, VARCHAR(255).
    Name,
    /// Unbounded text (JSON, message content).
    Text,
    Big,
}

pub struct Col {
    pub name: &'static str,
    pub ty: Ty,
    pub null: bool,
}

const fn c(name: &'static str, ty: Ty) -> Col {
    Col {
        name,
        ty,
        null: false,
    }
}
const fn n(name: &'static str, ty: Ty) -> Col {
    Col {
        name,
        ty,
        null: true,
    }
}

pub struct Table {
    pub name: &'static str,
    pub cols: &'static [Col],
    pub pk: &'static [&'static str],
    pub indexes: &'static [(&'static str, &'static [&'static str], bool)],
    /// Column holding the conversation id for per-conversation tables.
    pub conv_key: Option<&'static str>,
}

use Ty::*;

pub const TABLES: &[Table] = &[
    Table {
        name: "meta",
        cols: &[c("k", Id), c("v", Text)],
        pk: &["k"],
        indexes: &[],
        conv_key: None,
    },
    // ---- control database
    Table {
        name: "units",
        cols: &[
            c("id", Id),
            c("name", Name),
            c("database_name", Name),
            c("created_at", Big),
        ],
        pk: &["id"],
        indexes: &[("units_name", &["name"], true)],
        conv_key: None,
    },
    Table {
        name: "users",
        cols: &[
            c("id", Id),
            c("tenant_id", Id),
            c("unit_id", Id),
            c("name", Name),
            c("kind", Id),
            n("owner_id", Id),
            // Legacy plaintext token; since v2 a placeholder (see `tokens`).
            c("token", Id),
            c("created_at", Big),
            // v2: profile, custom fields, deactivation.
            n("display_name", Name),
            n("avatar", Text),
            n("fields", Text),
            n("deactivated_at", Big),
            // v3: per-user settings (JSON), e.g. read on one device silences the others.
            n("prefs", Text),
            // v3: an agent stopped or destroyed by its owner (docs/03-agents.md §4.3–§4.4).
            n("stopped_at", Big),
            n("destroyed_at", Big),
            // v4: `team` for team agents; NULL is a person's assistant.
            n("agent_scope", Id),
            // v7: account deleted (docs/04 §4.2); suspended by moderation until.
            n("erased_at", Big),
            n("suspended_until", Big),
        ],
        pk: &["id"],
        indexes: &[
            ("users_name", &["name"], true),
            ("users_token", &["token"], true),
            ("users_owner", &["owner_id"], false),
        ],
        conv_key: None,
    },
    // v2: client tokens (one per device), only the SHA-256 is stored.
    Table {
        name: "tokens",
        cols: &[
            c("hash", Key),
            c("user_id", Id),
            c("device_id", Id),
            n("device_name", Name),
            n("platform", Id),
            n("issued_by", Id),
            c("created_at", Big),
            n("expires_at", Big),
            n("last_seen", Big),
        ],
        pk: &["hash"],
        indexes: &[("tokens_user", &["user_id"], false)],
        conv_key: None,
    },
    // v2: built-in passwords and TOTP (docs/04-security-compliance.md §1).
    Table {
        name: "credentials",
        cols: &[
            c("user_id", Id),
            n("password_hash", Text),
            n("totp_secret", Id),
            n("totp_pending", Id),
            n("totp_last_step", Big),
            c("failures", Big),
            n("locked_until", Big),
            c("updated_at", Big),
        ],
        pk: &["user_id"],
        indexes: &[],
        conv_key: None,
    },
    // v2: data keys for encryption at rest, wrapped by the master key.
    // conv_id is the scope: a conversation, or "" for database-wide content.
    Table {
        name: "data_keys",
        cols: &[
            c("key_id", Id),
            c("conv_id", Id),
            c("period", Big),
            c("wrapped", Text),
            c("created_at", Big),
        ],
        pk: &["key_id"],
        indexes: &[("data_keys_scope", &["conv_id", "period"], false)],
        conv_key: Some("conv_id"),
    },
    // v2: open API keys.
    Table {
        name: "api_keys",
        cols: &[
            c("key_id", Id),
            c("app_id", Id),
            c("hash", Key),
            c("name", Name),
            c("scopes", Text),
            n("ip_allow", Text),
            c("created_at", Big),
            n("expires_at", Big),
            n("revoked_at", Big),
        ],
        pk: &["key_id"],
        indexes: &[],
        conv_key: None,
    },
    // v8: keys developers sign client tokens (JWT) with (docs/13 §4.2).
    // `secret` is wrapped by the master key when encryption is on.
    Table {
        name: "signing_keys",
        cols: &[
            c("kid", Id),
            c("name", Name),
            c("secret", Text),
            c("created_at", Big),
            n("revoked_at", Big),
        ],
        pk: &["kid"],
        indexes: &[],
        conv_key: None,
    },
    // v8: client tokens issued before `at` (seconds) are refused, for one
    // device or, with device_id "", all of a user's (docs/13 §4.2).
    Table {
        name: "token_cutoffs",
        cols: &[c("user_id", Id), c("device_id", Id), c("at", Big)],
        pk: &["user_id", "device_id"],
        indexes: &[],
        conv_key: None,
    },
    // v2: open API Idempotency-Key results (24 hours).
    Table {
        name: "idempotency",
        cols: &[
            c("key_id", Id),
            c("idem_key", Name),
            c("request_hash", Key),
            c("status", Big),
            c("response", Text),
            c("created_at", Big),
        ],
        pk: &["key_id", "idem_key"],
        indexes: &[("idempotency_created", &["created_at"], false)],
        conv_key: None,
    },
    Table {
        name: "conv_dir",
        cols: &[
            c("conv_id", Id),
            c("database_name", Name),
            c("unit_id", Id),
            c("migrating", Big),
            c("created_at", Big),
        ],
        pk: &["conv_id"],
        indexes: &[("conv_dir_unit", &["unit_id"], false)],
        conv_key: None,
    },
    Table {
        name: "member_index",
        cols: &[c("conv_id", Id), c("user_id", Id)],
        pk: &["conv_id", "user_id"],
        indexes: &[("member_index_user", &["user_id"], false)],
        conv_key: None,
    },
    Table {
        name: "blocks",
        cols: &[c("user_id", Id), c("blocked_id", Id), c("created_at", Big)],
        pk: &["user_id", "blocked_id"],
        indexes: &[("blocks_blocked", &["blocked_id"], false)],
        conv_key: None,
    },
    // One direct conversation per pair of users: "<smaller id>|<larger id>".
    Table {
        name: "dm_index",
        cols: &[c("pair_key", Name), c("conv_id", Id)],
        pk: &["pair_key"],
        indexes: &[("dm_index_conv", &["conv_id"], false)],
        conv_key: None,
    },
    // ---- conversation databases
    Table {
        name: "conversations",
        cols: &[
            c("id", Id),
            c("tenant_id", Id),
            c("unit_id", Id),
            c("kind", Id),
            c("title", Name),
            c("last_seq", Big),
            c("side_seq", Big),
            c("created_at", Big),
            // v2: custom fields.
            n("fields", Text),
            // v6: the last seq the replicas have (docs/11 §8.3).
            n("confirmed_seq", Big),
        ],
        pk: &["id"],
        indexes: &[],
        conv_key: Some("id"),
    },
    Table {
        name: "members",
        cols: &[
            c("conv_id", Id),
            c("user_id", Id),
            c("name", Name),
            c("kind", Id),
            n("owner_id", Id),
            c("role", Id),
            c("trigger_mode", Id),
            c("approval", Big),
            c("join_seq", Big),
            // v3: first event this member may see (NULL: before v3 — people see
            // everything, agents from join_seq); mute setting.
            n("visible_from", Big),
            n("mute_mode", Id),
            n("mute_until", Big),
            // v3: subscribed topics of an agent in `topics` mode (JSON list).
            n("topics", Text),
        ],
        pk: &["conv_id", "user_id"],
        indexes: &[],
        conv_key: Some("conv_id"),
    },
    Table {
        name: "events",
        cols: &[
            c("conv_id", Id),
            c("seq", Big),
            c("type", Id),
            n("actor_id", Id),
            n("msg_id", Id),
            c("payload", Text),
            c("created_at", Big),
        ],
        pk: &["conv_id", "seq"],
        indexes: &[],
        conv_key: Some("conv_id"),
    },
    Table {
        name: "messages",
        cols: &[
            c("msg_id", Id),
            c("conv_id", Id),
            c("created_seq", Big),
            c("sender_id", Id),
            c("sender_name", Name),
            c("sender_type", Id),
            n("on_behalf_of", Id),
            c("kind", Id),
            c("content", Text),
            c("version", Big),
            c("state", Id),
            n("state_reason", Name),
            c("hop", Big),
            n("causation_id", Id),
            n("reply_to", Id),
            c("mentions", Text),
            c("created_at", Big),
            c("updated_at", Big),
            // v2: structured content of custom message kinds (JSON).
            n("payload", Text),
            // v3: routing envelope (to, topics, intent, refs; encrypted JSON)
            // and the source of the chain it belongs to (docs/03-agents.md §6).
            n("envelope", Text),
            n("root_id", Id),
        ],
        pk: &["msg_id"],
        indexes: &[
            ("messages_conv", &["conv_id", "created_seq"], false),
            (
                "messages_sender",
                &["conv_id", "sender_id", "created_seq"],
                false,
            ),
            ("messages_state", &["state"], false),
        ],
        conv_key: Some("conv_id"),
    },
    Table {
        name: "side_events",
        cols: &[
            c("conv_id", Id),
            c("side_seq", Big),
            c("anchor_seq", Big),
            c("audience", Text),
            c("type", Id),
            n("actor_id", Id),
            c("payload", Text),
            c("created_at", Big),
        ],
        pk: &["conv_id", "side_seq"],
        indexes: &[],
        conv_key: Some("conv_id"),
    },
    Table {
        name: "dedup",
        cols: &[
            c("conv_id", Id),
            c("sender_id", Id),
            c("client_msg_id", Key),
            c("result", Text),
            c("created_at", Big),
        ],
        pk: &["conv_id", "sender_id", "client_msg_id"],
        indexes: &[("dedup_created", &["created_at"], false)],
        conv_key: Some("conv_id"),
    },
    Table {
        name: "read_cursors",
        cols: &[c("conv_id", Id), c("user_id", Id), c("read_seq", Big)],
        pk: &["conv_id", "user_id"],
        indexes: &[],
        conv_key: Some("conv_id"),
    },
    // v3: conversations a user left or was removed from (or that were
    // dissolved), so their devices sync up to that point (control database).
    Table {
        name: "departures",
        cols: &[
            c("user_id", Id),
            c("conv_id", Id),
            c("seq", Big),
            c("dissolved", Big),
            c("at", Big),
        ],
        pk: &["user_id", "conv_id"],
        indexes: &[],
        conv_key: None,
    },
    // v4: a removed member's last stretch of the log (from its first visible
    // seq to its removal), so devices that were offline get it after the
    // member is added back (D92).
    Table {
        name: "past_members",
        cols: &[
            c("conv_id", Id),
            c("user_id", Id),
            c("visible_from", Big),
            c("until_seq", Big),
        ],
        pk: &["conv_id", "user_id"],
        indexes: &[],
        conv_key: Some("conv_id"),
    },
    // v3: how far each device has received and acknowledged a conversation
    // (docs/08-sync-and-client.md §2.1). Kept by the server; clients only ack.
    Table {
        name: "device_cursors",
        cols: &[
            c("device_id", Id),
            c("conv_id", Id),
            c("user_id", Id),
            c("acked_seq", Big),
            c("acked_side_seq", Big),
            c("updated_at", Big),
            // v5: the read/mute state the device confirmed it received (M8).
            n("told_state", Text),
        ],
        pk: &["device_id", "conv_id"],
        indexes: &[("device_cursors_conv_user", &["conv_id", "user_id"], false)],
        conv_key: Some("conv_id"),
    },
    // v3: task channels, claims and the floor (docs/03-agents.md §8).
    Table {
        name: "tasks",
        cols: &[
            c("conv_id", Id),
            c("state", Id),
            n("assignee_id", Id),
            c("created_by", Id),
            c("updated_at", Big),
        ],
        pk: &["conv_id"],
        indexes: &[],
        conv_key: Some("conv_id"),
    },
    Table {
        name: "claims",
        cols: &[
            c("msg_id", Id),
            c("conv_id", Id),
            c("user_id", Id),
            c("created_at", Big),
        ],
        pk: &["msg_id"],
        indexes: &[("claims_conv", &["conv_id"], false)],
        conv_key: Some("conv_id"),
    },
    Table {
        name: "floors",
        cols: &[
            c("conv_id", Id),
            c("agent_id", Id),
            c("granted_by", Id),
            c("expires_at", Big),
        ],
        pk: &["conv_id"],
        indexes: &[],
        conv_key: Some("conv_id"),
    },
    // v3: agent checkpoints, one row per version (docs/03-agents.md §7.2).
    Table {
        name: "agent_state",
        cols: &[
            c("agent_id", Id),
            c("state_key", Name),
            c("version", Big),
            c("value", Text),
            c("updated_at", Big),
        ],
        pk: &["agent_id", "state_key", "version"],
        indexes: &[],
        conv_key: None,
    },
    // v3: who responded to which card (docs/03-agents.md §3).
    Table {
        name: "card_responses",
        cols: &[
            c("msg_id", Id),
            c("user_id", Id),
            c("conv_id", Id),
            c("created_at", Big),
        ],
        pk: &["msg_id", "user_id"],
        indexes: &[("card_responses_conv", &["conv_id"], false)],
        conv_key: Some("conv_id"),
    },
    // v3: people's feedback on agent messages (docs/03-agents.md §3).
    Table {
        name: "feedback",
        cols: &[
            c("msg_id", Id),
            c("user_id", Id),
            c("conv_id", Id),
            c("agent_id", Id),
            c("rating", Id),
            n("reason", Text),
            c("created_at", Big),
        ],
        pk: &["msg_id", "user_id"],
        indexes: &[
            ("feedback_conv", &["conv_id"], false),
            ("feedback_agent", &["agent_id", "created_at"], false),
        ],
        conv_key: Some("conv_id"),
    },
    // v3: summaries agents write of conversations (docs/03-agents.md §7.1).
    Table {
        name: "summaries",
        cols: &[
            c("summary_id", Id),
            c("conv_id", Id),
            c("agent_id", Id),
            c("from_seq", Big),
            c("to_seq", Big),
            c("text", Text),
            c("created_at", Big),
        ],
        pk: &["summary_id"],
        indexes: &[("summaries_agent", &["conv_id", "agent_id", "to_seq"], false)],
        conv_key: Some("conv_id"),
    },
    // v3: agent messages per source of a chain (root budget, docs/03-agents.md §6).
    Table {
        name: "root_budgets",
        cols: &[c("conv_id", Id), c("root_id", Id), c("messages", Big)],
        pk: &["conv_id", "root_id"],
        indexes: &[],
        conv_key: Some("conv_id"),
    },
    // v3: requests for a human decision (docs/03-agents.md §4.2).
    Table {
        name: "requests",
        cols: &[
            c("request_id", Id),
            c("conv_id", Id),
            c("agent_id", Id),
            c("client_req_id", Name),
            c("kind", Id),
            c("title", Text),
            n("options", Text),
            n("context", Text),
            c("state", Id),
            c("created_at", Big),
            c("expires_at", Big),
            n("decided_by", Id),
            n("decided_at", Big),
            n("response", Text),
        ],
        pk: &["request_id"],
        indexes: &[
            ("requests_client", &["agent_id", "client_req_id"], true),
            ("requests_expiry", &["state", "expires_at"], false),
            ("requests_conv", &["conv_id"], false),
        ],
        conv_key: Some("conv_id"),
    },
    Table {
        name: "request_assignees",
        cols: &[c("request_id", Id), c("user_id", Id), c("conv_id", Id)],
        pk: &["request_id", "user_id"],
        indexes: &[("request_assignees_user", &["user_id"], false)],
        conv_key: Some("conv_id"),
    },
    Table {
        name: "drafts",
        cols: &[
            c("draft_id", Id),
            c("conv_id", Id),
            c("agent_id", Id),
            c("content", Text),
            c("hop", Big),
            n("causation_id", Id),
            n("reply_to", Id),
            c("state", Id),
            c("created_at", Big),
            n("decided_by", Id),
            n("decided_at", Big),
            // v2: kind and payload of custom message kinds.
            n("kind", Id),
            n("payload", Text),
            // v3: routing envelope (docs/03-agents.md §6).
            n("envelope", Text),
        ],
        pk: &["draft_id"],
        indexes: &[
            ("drafts_agent", &["agent_id", "state"], false),
            ("drafts_conv", &["conv_id"], false),
        ],
        conv_key: Some("conv_id"),
    },
    Table {
        name: "takeovers",
        cols: &[
            c("conv_id", Id),
            c("agent_id", Id),
            c("by_user", Id),
            c("since", Big),
            // v3: the agent's handover note when it handed off (docs/03-agents.md §3).
            n("note", Text),
        ],
        pk: &["conv_id", "agent_id"],
        indexes: &[],
        conv_key: Some("conv_id"),
    },
    // v2: events waiting for webhook delivery, in commit order.
    Table {
        name: "webhook_outbox",
        cols: &[
            c("id", Big),
            c("conv_id", Id),
            c("type", Id),
            c("body", Text),
            c("created_at", Big),
        ],
        pk: &["id"],
        indexes: &[],
        conv_key: None,
    },
    // v7: account deletions in progress or done (docs/04 §4.2).
    Table {
        name: "erasures",
        cols: &[
            c("user_id", Id),
            n("by_id", Id),
            c("state", Id),
            c("requested_at", Big),
            n("finished_at", Big),
            c("convs_done", Big),
        ],
        pk: &["user_id"],
        indexes: &[("erasures_state", &["state"], false)],
        conv_key: None,
    },
    // v7: reports and their decisions (docs/04 §4.2).
    Table {
        name: "reports",
        cols: &[
            c("id", Id),
            c("reporter_id", Id),
            c("target_kind", Id),
            c("target_id", Id),
            n("conv_id", Id),
            c("category", Id),
            c("severity", Big),
            n("body", Text),
            c("state", Id),
            c("created_at", Big),
            c("due_at", Big),
            n("resolved_at", Big),
            n("decision", Text),
        ],
        pk: &["id"],
        indexes: &[
            ("reports_queue", &["state", "severity", "due_at"], false),
            (
                "reports_target",
                &["target_kind", "target_id", "state"],
                false,
            ),
            ("reports_reporter", &["reporter_id", "created_at"], false),
        ],
        conv_key: None,
    },
    Table {
        name: "audit",
        cols: &[
            c("id", Id),
            c("ts", Big),
            n("actor_id", Id),
            c("action", Id),
            n("conv_id", Id),
            n("target", Id),
            c("detail", Text),
        ],
        pk: &["id"],
        indexes: &[
            ("audit_ts", &["ts"], false),
            ("audit_conv", &["conv_id"], false),
        ],
        conv_key: Some("conv_id"),
    },
];

fn sql_type(d: Dialect, t: Ty) -> &'static str {
    match (d, t) {
        (_, Id) => "VARCHAR(64)",
        (_, Key) => "VARCHAR(128)",
        (_, Name) => "VARCHAR(255)",
        (Dialect::Mysql, Text) => "LONGTEXT",
        (_, Text) => "TEXT",
        (_, Big) => "BIGINT",
    }
}

/// DDL statements for one table in the given dialect.
pub fn table_ddl(d: Dialect, t: &Table) -> Vec<String> {
    let mut cols: Vec<String> = t
        .cols
        .iter()
        .map(|c| {
            format!(
                "{} {}{}",
                c.name,
                sql_type(d, c.ty),
                if c.null { "" } else { " NOT NULL" }
            )
        })
        .collect();
    cols.push(format!("PRIMARY KEY ({})", t.pk.join(", ")));
    let mut out = vec![];
    if d == Dialect::Mysql {
        // MySQL has no CREATE INDEX IF NOT EXISTS; declare indexes inline.
        for (name, ic, unique) in t.indexes {
            cols.push(format!(
                "{} {} ({})",
                if *unique { "UNIQUE KEY" } else { "INDEX" },
                name,
                ic.join(", ")
            ));
        }
        out.push(format!(
            "CREATE TABLE IF NOT EXISTS {} ({}) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin",
            t.name,
            cols.join(", ")
        ));
    } else {
        out.push(format!(
            "CREATE TABLE IF NOT EXISTS {} ({})",
            t.name,
            cols.join(", ")
        ));
        for (name, ic, unique) in t.indexes {
            out.push(format!(
                "CREATE {}INDEX IF NOT EXISTS {} ON {} ({})",
                if *unique { "UNIQUE " } else { "" },
                name,
                t.name,
                ic.join(", ")
            ));
        }
    }
    out
}

/// One additive change. Steps are idempotent: a MySQL migration cannot be
/// rolled back (DDL commits implicitly), so a restart must finish it.
pub enum Step {
    AddColumn {
        table: &'static str,
        column: &'static str,
    },
    /// Moves plaintext `users.token` values into `tokens`, hashed.
    HashLegacyTokens,
}

pub struct Migration {
    /// Schema version after this migration.
    pub version: i64,
    /// Oldest schema version a binary may have and still use the database
    /// after this migration. Additive migrations keep the previous value.
    pub compat: i64,
    pub about: &'static str,
    pub steps: &'static [Step],
}

/// Every migration after version 1, in order.
pub const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 2,
        compat: 1,
        about: "custom message kinds, webhook outbox, hashed tokens and devices, open API keys, profiles and custom fields, passwords, encryption keys",
        steps: &[
            Step::AddColumn {
                table: "messages",
                column: "payload",
            },
            Step::AddColumn {
                table: "drafts",
                column: "kind",
            },
            Step::AddColumn {
                table: "drafts",
                column: "payload",
            },
            Step::AddColumn {
                table: "users",
                column: "display_name",
            },
            Step::AddColumn {
                table: "users",
                column: "avatar",
            },
            Step::AddColumn {
                table: "users",
                column: "fields",
            },
            Step::AddColumn {
                table: "users",
                column: "deactivated_at",
            },
            Step::AddColumn {
                table: "conversations",
                column: "fields",
            },
            Step::HashLegacyTokens,
        ],
    },
    Migration {
        version: 3,
        compat: 1,
        about: "sync-only clients: device cursors, member history start and mute, user settings; requests for a human decision; message envelopes, root budgets and topic subscriptions; task channels, claims and the floor",
        steps: &[
            Step::AddColumn {
                table: "users",
                column: "prefs",
            },
            Step::AddColumn {
                table: "members",
                column: "visible_from",
            },
            Step::AddColumn {
                table: "members",
                column: "mute_mode",
            },
            Step::AddColumn {
                table: "members",
                column: "mute_until",
            },
            Step::AddColumn {
                table: "members",
                column: "topics",
            },
            Step::AddColumn {
                table: "messages",
                column: "envelope",
            },
            Step::AddColumn {
                table: "messages",
                column: "root_id",
            },
            Step::AddColumn {
                table: "drafts",
                column: "envelope",
            },
        ],
    },
    Migration {
        version: 4,
        compat: 1,
        about: "team agents",
        steps: &[Step::AddColumn {
            table: "users",
            column: "agent_scope",
        }],
    },
    Migration {
        version: 5,
        compat: 1,
        about: "read and mute state each device received",
        steps: &[Step::AddColumn {
            table: "device_cursors",
            column: "told_state",
        }],
    },
    Migration {
        version: 6,
        compat: 1,
        about: "replicated position of each conversation",
        steps: &[Step::AddColumn {
            table: "conversations",
            column: "confirmed_seq",
        }],
    },
    Migration {
        version: 7,
        compat: 1,
        about: "account deletion, suspensions, reports",
        steps: &[
            Step::AddColumn {
                table: "users",
                column: "erased_at",
            },
            Step::AddColumn {
                table: "users",
                column: "suspended_until",
            },
        ],
    },
    Migration {
        version: 8,
        compat: 1,
        about: "client tokens signed by the developer: signing keys and token cutoffs",
        steps: &[],
    },
];

fn table(name: &str) -> &'static Table {
    TABLES
        .iter()
        .find(|t| t.name == name)
        .unwrap_or_else(|| panic!("unknown table {name}"))
}

fn col(t: &'static Table, name: &str) -> &'static Col {
    t.cols
        .iter()
        .find(|c| c.name == name)
        .unwrap_or_else(|| panic!("unknown column {}.{name}", t.name))
}

/// Column names of a table as the database reports them (lower case).
pub async fn columns(c: &mut AnyConnection, d: Dialect, table: &str) -> ApiResult<Vec<String>> {
    let sql = match d {
        Dialect::Sqlite => "SELECT name FROM pragma_table_info(?) ORDER BY name",
        Dialect::Postgres => {
            "SELECT CAST(column_name AS TEXT) FROM information_schema.columns \
             WHERE table_schema = current_schema() AND table_name = ? ORDER BY 1"
        }
        Dialect::Mysql => {
            "SELECT CAST(column_name AS CHAR) FROM information_schema.columns \
             WHERE table_schema = DATABASE() AND table_name = ? ORDER BY 1"
        }
    };
    let rows = sqlx::query(d.q(sql)).bind(table).fetch_all(&mut *c).await?;
    rows.iter()
        .map(|r| Ok(r.try_get::<String, _>(0)?.to_lowercase()))
        .collect()
}

/// `name TYPE [NOT NULL]` of a declared column.
pub fn column_ddl(d: Dialect, tn: &str, column: &str) -> String {
    let cl = col(table(tn), column);
    format!(
        "{column} {}{}",
        sql_type(d, cl.ty),
        if cl.null { "" } else { " NOT NULL" }
    )
}

async fn apply_step(c: &mut AnyConnection, d: Dialect, step: &Step) -> ApiResult<()> {
    match step {
        Step::AddColumn { table: tn, column } => {
            if columns(c, d, tn).await?.iter().any(|n| n == column) {
                return Ok(());
            }
            let sql = format!("ALTER TABLE {tn} ADD COLUMN {}", column_ddl(d, tn, column));
            sqlx::query(AssertSqlSafe(sql)).execute(&mut *c).await?;
        }
        Step::HashLegacyTokens => {
            let rows = sqlx::query("SELECT id, token, created_at FROM users")
                .fetch_all(&mut *c)
                .await?;
            for r in rows {
                let (id, token, at): (String, String, i64) =
                    (r.try_get(0)?, r.try_get(1)?, r.try_get(2)?);
                if token == crate::auth::legacy_placeholder(&id) {
                    continue;
                }
                let h = crate::auth::hash(&token);
                let have = sqlx::query(d.q("SELECT hash FROM tokens WHERE hash = ?"))
                    .bind(h.clone())
                    .fetch_optional(&mut *c)
                    .await?;
                if have.is_none() {
                    sqlx::query(d.q("INSERT INTO tokens (hash, user_id, device_id, device_name, issued_by, created_at) VALUES (?, ?, ?, ?, ?, ?)"))
                        .bind(h)
                        .bind(id.clone())
                        .bind(format!("d_legacy_{id}"))
                        .bind("legacy")
                        .bind("migration")
                        .bind(at)
                        .execute(&mut *c)
                        .await?;
                }
                sqlx::query(d.q("UPDATE users SET token = ? WHERE id = ?"))
                    .bind(crate::auth::legacy_placeholder(&id))
                    .bind(id)
                    .execute(&mut *c)
                    .await?;
            }
        }
    }
    Ok(())
}

/// Creates the schema on an empty database, migrates an older one, and
/// refuses one this binary cannot use (docs/02-architecture.md §5.3).
pub async fn ensure_schema(c: &mut AnyConnection, d: Dialect) -> ApiResult<()> {
    // Only one process migrates at a time. SQLite: the write lock, taken up
    // front (see the writer in shard.rs). PostgreSQL: a transaction-scoped
    // advisory lock. MySQL: a session lock around the transaction, because
    // its DDL commits implicitly.
    if d == Dialect::Mysql {
        let got: Option<i64> = sqlx::query("SELECT GET_LOCK('agentim_schema', 60)")
            .fetch_one(&mut *c)
            .await?
            .try_get(0)?;
        if got != Some(1) {
            return Err(ApiError::new(
                "storage_unavailable",
                "timed out waiting for another node to finish the schema setup",
            ));
        }
    }
    let res = async {
        let mut tx = if d == Dialect::Sqlite {
            Connection::begin_with(&mut *c, "BEGIN IMMEDIATE").await?
        } else {
            Connection::begin(&mut *c).await?
        };
        if d == Dialect::Postgres {
            sqlx::query("SELECT pg_advisory_xact_lock(7262011)")
                .execute(&mut *tx)
                .await?;
        }
        apply_schema(&mut tx, d).await?;
        tx.commit().await?;
        Ok(())
    }
    .await;
    if d == Dialect::Mysql {
        let _ = sqlx::query("SELECT RELEASE_LOCK('agentim_schema')")
            .execute(&mut *c)
            .await;
    }
    res
}

async fn meta_get(c: &mut AnyConnection, d: Dialect, k: &str) -> ApiResult<Option<String>> {
    Ok(sqlx::query(d.q("SELECT v FROM meta WHERE k = ?"))
        .bind(k.to_string())
        .fetch_optional(&mut *c)
        .await?
        .map(|r| r.try_get(0))
        .transpose()?)
}

async fn meta_set(c: &mut AnyConnection, d: Dialect, k: &str, v: i64) -> ApiResult<()> {
    sqlx::query(d.q("DELETE FROM meta WHERE k = ?"))
        .bind(k.to_string())
        .execute(&mut *c)
        .await?;
    sqlx::query(d.q("INSERT INTO meta (k, v) VALUES (?, ?)"))
        .bind(k.to_string())
        .bind(v.to_string())
        .execute(&mut *c)
        .await?;
    Ok(())
}

fn mismatch(msg: String) -> ApiError {
    ApiError::new("schema_mismatch", msg)
}

async fn apply_schema(c: &mut AnyConnection, d: Dialect) -> ApiResult<()> {
    for t in TABLES {
        for stmt in table_ddl(d, t) {
            sqlx::query(AssertSqlSafe(stmt)).execute(&mut *c).await?;
        }
    }
    let parse = |k: &str, v: String| {
        v.parse::<i64>()
            .ok()
            .filter(|n| *n >= 1)
            .ok_or_else(|| mismatch(format!("meta.{k} = {v:?} is not a schema version")))
    };
    let version = match meta_get(c, d, "schema_version").await? {
        None => {
            // A new database: the tables above are already the latest schema.
            meta_set(c, d, "schema_version", SCHEMA_VERSION).await?;
            meta_set(c, d, "schema_compat", current_compat()).await?;
            return Ok(());
        }
        Some(v) => parse("schema_version", v)?,
    };
    // Databases from before compat tracking require their own version.
    let compat = match meta_get(c, d, "schema_compat").await? {
        None => version,
        Some(v) => parse("schema_compat", v)?,
    };
    if version > SCHEMA_VERSION {
        if compat > SCHEMA_VERSION {
            return Err(mismatch(format!(
                "database schema version {version} needs a binary of schema version {compat} or newer; this binary has {SCHEMA_VERSION}"
            )));
        }
        tracing::warn!(
            database_version = version,
            binary_version = SCHEMA_VERSION,
            "database schema is newer than this binary but compatible; upgrade this node"
        );
        return Ok(());
    }
    let mut compat = compat;
    for m in MIGRATIONS.iter().filter(|m| m.version > version) {
        for step in m.steps {
            apply_step(c, d, step).await?;
        }
        compat = compat.max(m.compat);
        meta_set(c, d, "schema_version", m.version).await?;
        meta_set(c, d, "schema_compat", compat).await?;
        tracing::info!(version = m.version, about = m.about, "schema migrated");
    }
    Ok(())
}

/// `schema_compat` of a database created at the latest version.
pub fn current_compat() -> i64 {
    MIGRATIONS.iter().map(|m| m.compat).max().unwrap_or(1)
}

/// Refuses databases that could lose acknowledged writes (G2) unless
/// explicitly overridden.
pub async fn check_durability(c: &mut AnyConnection, d: Dialect) -> Result<(), String> {
    let get = |r: sqlx::any::AnyRow, i: usize| -> String {
        r.try_get::<String, _>(i).unwrap_or_default()
    };
    match d {
        Dialect::Sqlite => {
            let r = sqlx::query("PRAGMA synchronous")
                .fetch_one(&mut *c)
                .await
                .map_err(|e| e.to_string())?;
            let v: i64 = r.try_get(0).map_err(|e| e.to_string())?;
            if v < 2 {
                return Err(format!("sqlite synchronous={v}, need FULL (2)"));
            }
        }
        Dialect::Postgres => {
            let fsync = get(
                sqlx::query("SHOW fsync")
                    .fetch_one(&mut *c)
                    .await
                    .map_err(|e| e.to_string())?,
                0,
            );
            let sc = get(
                sqlx::query("SHOW synchronous_commit")
                    .fetch_one(&mut *c)
                    .await
                    .map_err(|e| e.to_string())?,
                0,
            );
            if fsync != "on" || sc == "off" {
                return Err(format!(
                    "postgres fsync={fsync} synchronous_commit={sc}; need fsync=on and synchronous_commit!=off"
                ));
            }
        }
        Dialect::Mysql => {
            let r = sqlx::query(
                "SELECT CAST(@@innodb_flush_log_at_trx_commit AS SIGNED), CAST(@@sync_binlog AS SIGNED), CAST(@@log_bin AS SIGNED)",
            )
            .fetch_one(&mut *c)
            .await
            .map_err(|e| e.to_string())?;
            let (flush, sync, log_bin): (i64, i64, i64) = (
                r.try_get(0).map_err(|e| e.to_string())?,
                r.try_get(1).map_err(|e| e.to_string())?,
                r.try_get(2).map_err(|e| e.to_string())?,
            );
            if flush != 1 || (log_bin == 1 && sync != 1) {
                return Err(format!(
                    "mysql innodb_flush_log_at_trx_commit={flush} sync_binlog={sync} log_bin={log_bin}; need 1 (and sync_binlog=1 when binlog is on)"
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholders_are_rewritten_for_postgres_only() {
        assert_eq!(
            Dialect::Postgres.q("a = ? AND b = ?").0,
            "a = $1 AND b = $2"
        );
        assert_eq!(Dialect::Mysql.q("a = ?").0, "a = ?");
        assert_eq!(Dialect::from_url("mariadb://x").unwrap(), Dialect::Mysql);
    }

    #[test]
    fn windows_sqlite_paths_keep_the_drive_letter() {
        assert_eq!(
            driver_url("sqlite://C:/data/a.db?mode=rwc"),
            "sqlite:C:/data/a.db?mode=rwc"
        );
        assert_eq!(driver_url("sqlite://a.db"), "sqlite://a.db");
        assert_eq!(driver_url("mariadb://h/db"), "mysql://h/db");
        let url = crate::config::sqlite_url(r"C:\Users\me\a b.db");
        assert_eq!(url, "sqlite:C:/Users/me/a%20b.db?mode=rwc");
        let opts: sqlx::any::AnyConnectOptions = driver_url(&url).parse().unwrap();
        let back = opts.database_url.as_str().trim_start_matches("sqlite:");
        assert!(back.starts_with("C:/Users/me/"), "{back}");
    }

    #[test]
    fn mysql_ddl_inlines_indexes() {
        let t = TABLES.iter().find(|t| t.name == "users").unwrap();
        let m = table_ddl(Dialect::Mysql, t);
        assert_eq!(m.len(), 1);
        assert!(m[0].contains("UNIQUE KEY users_name (name)"));
        let s = table_ddl(Dialect::Sqlite, t);
        assert_eq!(s.len(), 4);
    }
}
