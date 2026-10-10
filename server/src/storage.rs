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
pub const SCHEMA_VERSION: i64 = 2;

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
    let mut c = AnyConnection::connect(&driver_url(url))
        .await
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
pub const MIGRATIONS: &[Migration] = &[Migration {
    version: 2,
    compat: 1,
    about: "custom message kinds, webhook outbox, hashed tokens and devices, open API keys, profiles and custom fields",
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
}];

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
