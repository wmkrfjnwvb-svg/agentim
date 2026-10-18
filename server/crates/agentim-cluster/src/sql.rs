//! The cluster's tables and statements (docs/11 §4). Times are milliseconds
//! since 1970 taken from the database clock inside each statement, never
//! from this process.

use agentim_core::storage::Dialect;
use sqlx::{AnyPool, Row};

/// The database's current time in milliseconds.
pub fn now(d: Dialect) -> &'static str {
    match d {
        // clock_timestamp(): the time now, not the transaction's start.
        Dialect::Postgres => "CAST(FLOOR(EXTRACT(EPOCH FROM clock_timestamp()) * 1000) AS BIGINT)",
        // Independent of the session's time zone.
        _ => {
            "CAST(TIMESTAMPDIFF(MICROSECOND, '1970-01-01 00:00:00', UTC_TIMESTAMP(6)) DIV 1000 AS SIGNED)"
        }
    }
}

const SCHEMA: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS cluster_meta (k VARCHAR(64) NOT NULL PRIMARY KEY, v TEXT NOT NULL)",
    "CREATE TABLE IF NOT EXISTS cluster_nodes (node_id VARCHAR(64) NOT NULL PRIMARY KEY, boot_id VARCHAR(64) NOT NULL, addr VARCHAR(255) NOT NULL, slot BIGINT NOT NULL, version VARCHAR(32) NOT NULL, state VARCHAR(16) NOT NULL, heartbeat_at BIGINT NOT NULL)",
    "CREATE UNIQUE INDEX cluster_nodes_slot ON cluster_nodes (slot)",
    "CREATE TABLE IF NOT EXISTS shard_lease (db VARCHAR(64) NOT NULL, shard_id BIGINT NOT NULL, owner VARCHAR(160), epoch BIGINT NOT NULL, expires_at BIGINT NOT NULL, fence_count BIGINT NOT NULL, PRIMARY KEY (db, shard_id))",
];

/// Creates the tables (idempotent; concurrent starts are fine).
pub async fn ensure_schema(pool: &AnyPool) -> Result<(), String> {
    for s in SCHEMA {
        if let Err(e) = sqlx::raw_sql(sqlx::AssertSqlSafe(*s)).execute(pool).await {
            let m = e.to_string();
            // CREATE INDEX has no IF NOT EXISTS on MySQL.
            if !(m.contains("already exists") || m.contains("Duplicate key name")) {
                return Err(m);
            }
        }
    }
    Ok(())
}

pub async fn meta_get(pool: &AnyPool, d: Dialect, k: &str) -> Result<Option<String>, String> {
    let r = sqlx::query(d.q("SELECT v FROM cluster_meta WHERE k = ?"))
        .bind(k)
        .fetch_optional(pool)
        .await
        .map_err(|e| e.to_string())?;
    r.map(|r| r.try_get::<String, _>(0).map_err(|e| e.to_string()))
        .transpose()
}

/// Sets up a database for the cluster once: its cluster id, shard count
/// and every lease row (rows are never inserted later, so taking a lease
/// is always an UPDATE). Returns the stored (cluster id, shards).
pub async fn init_db(
    pool: &AnyPool,
    d: Dialect,
    db: &str,
    cluster_id: &str,
    shards: u32,
    extra: &[(&str, String)],
) -> Result<(String, u32), String> {
    let stored = || async {
        let id = meta_get(pool, d, "cluster_id").await?;
        let n = meta_get(pool, d, "shards").await?;
        Ok::<_, String>(match (id, n) {
            (Some(id), Some(n)) => Some((id, n.parse::<u32>().map_err(|e| e.to_string())?)),
            _ => None,
        })
    };
    if let Some(s) = stored().await? {
        return Ok(s);
    }
    let attempt = async {
        let mut tx = pool.begin().await.map_err(|e| e.to_string())?;
        sqlx::query(d.q("INSERT INTO cluster_meta (k, v) VALUES ('cluster_id', ?)"))
            .bind(cluster_id)
            .execute(&mut *tx)
            .await
            .map_err(|e| e.to_string())?;
        sqlx::query(d.q("INSERT INTO cluster_meta (k, v) VALUES ('shards', ?)"))
            .bind(shards.to_string())
            .execute(&mut *tx)
            .await
            .map_err(|e| e.to_string())?;
        for (k, v) in extra {
            sqlx::query(d.q("INSERT INTO cluster_meta (k, v) VALUES (?, ?)"))
                .bind(*k)
                .bind(v)
                .execute(&mut *tx)
                .await
                .map_err(|e| e.to_string())?;
        }
        // Conversation shards, then the control and upkeep shards.
        let all: Vec<u32> = (0..shards + 2).collect();
        for chunk in all.chunks(200) {
            let mut sql =
                "INSERT INTO shard_lease (db, shard_id, owner, epoch, expires_at, fence_count) VALUES "
                    .to_string();
            sql.push_str(&vec!["(?, ?, NULL, 0, 0, 0)"; chunk.len()].join(", "));
            let mut q = sqlx::query(d.q(&sql));
            for s in chunk {
                q = q.bind(db).bind(i64::from(*s));
            }
            q.execute(&mut *tx).await.map_err(|e| e.to_string())?;
        }
        tx.commit().await.map_err(|e| e.to_string())
    }
    .await;
    match (attempt, stored().await?) {
        (_, Some(s)) => Ok(s),
        (Err(e), None) => Err(e),
        (Ok(()), None) => Err("cluster set-up did not persist".into()),
    }
}

#[derive(Debug, Clone)]
pub struct NodeRow {
    pub node_id: String,
    pub boot_id: String,
    pub addr: String,
    pub slot: u32,
    pub state: String,
    pub heartbeat_at: i64,
}

/// All nodes, and the database time.
pub async fn nodes(pool: &AnyPool, d: Dialect) -> Result<(Vec<NodeRow>, i64), String> {
    let sql = format!(
        "SELECT node_id, boot_id, addr, slot, state, heartbeat_at, {} FROM cluster_nodes",
        now(d)
    );
    let rows = sqlx::query(d.q(&sql))
        .fetch_all(pool)
        .await
        .map_err(|e| e.to_string())?;
    let mut now_ms = None;
    let mut out = vec![];
    for r in rows {
        now_ms = Some(r.try_get::<i64, _>(6).map_err(|e| e.to_string())?);
        out.push(NodeRow {
            node_id: r.try_get(0).map_err(|e| e.to_string())?,
            boot_id: r.try_get(1).map_err(|e| e.to_string())?,
            addr: r.try_get(2).map_err(|e| e.to_string())?,
            slot: r.try_get::<i64, _>(3).map_err(|e| e.to_string())? as u32,
            state: r.try_get(4).map_err(|e| e.to_string())?,
            heartbeat_at: r.try_get(5).map_err(|e| e.to_string())?,
        });
    }
    let now_ms = match now_ms {
        Some(n) => n,
        None => db_now(pool, d).await?,
    };
    Ok((out, now_ms))
}

pub async fn db_now(pool: &AnyPool, d: Dialect) -> Result<i64, String> {
    sqlx::query(d.q(&format!("SELECT {}", now(d))))
        .fetch_one(pool)
        .await
        .and_then(|r| r.try_get::<i64, _>(0))
        .map_err(|e| e.to_string())
}

/// Registers this process as `node_id`: a new row (with the next free
/// slot) or, for a known node, a takeover from its previous boot.
/// `prev_boot` is the boot this process saw in the row (None: no row).
pub async fn register(
    pool: &AnyPool,
    d: Dialect,
    node_id: &str,
    prev_boot: Option<&str>,
    boot_id: &str,
    addr: &str,
    version: &str,
) -> Result<bool, String> {
    let n = now(d);
    match prev_boot {
        Some(prev) => {
            let sql = format!(
                "UPDATE cluster_nodes SET boot_id = ?, addr = ?, version = ?, state = 'joining', heartbeat_at = {n} WHERE node_id = ? AND boot_id = ?"
            );
            Ok(sqlx::query(d.q(&sql))
                .bind(boot_id)
                .bind(addr)
                .bind(version)
                .bind(node_id)
                .bind(prev)
                .execute(pool)
                .await
                .map_err(|e| e.to_string())?
                .rows_affected()
                == 1)
        }
        None => {
            let max: Option<i64> = sqlx::query("SELECT MAX(slot) FROM cluster_nodes")
                .fetch_one(pool)
                .await
                .and_then(|r| r.try_get(0))
                .map_err(|e| e.to_string())?;
            let sql = format!(
                "INSERT INTO cluster_nodes (node_id, boot_id, addr, slot, version, state, heartbeat_at) VALUES (?, ?, ?, ?, ?, 'joining', {n})"
            );
            match sqlx::query(d.q(&sql))
                .bind(node_id)
                .bind(boot_id)
                .bind(addr)
                .bind(max.unwrap_or(0) + 1)
                .bind(version)
                .execute(pool)
                .await
            {
                Ok(_) => Ok(true),
                // Another node took the slot or the id at the same time.
                Err(e) if is_duplicate(&e.to_string()) => Ok(false),
                Err(e) => Err(e.to_string()),
            }
        }
    }
}

pub fn is_duplicate(m: &str) -> bool {
    m.contains("duplicate key") || m.contains("Duplicate entry") || m.contains("UNIQUE")
}

/// Set by `agentim cluster drain`; the node's heartbeat keeps it until
/// the node leaves.
pub const DRAINING: &str = "draining";

/// The heartbeat; false when another process took over this node id.
pub async fn heartbeat(
    pool: &AnyPool,
    d: Dialect,
    node_id: &str,
    boot_id: &str,
    state: &str,
) -> Result<bool, String> {
    let sql = format!(
        "UPDATE cluster_nodes SET heartbeat_at = {}, state = CASE WHEN state = 'draining' AND ? IN ('joining', 'active') THEN state ELSE ? END WHERE node_id = ? AND boot_id = ?",
        now(d)
    );
    Ok(sqlx::query(d.q(&sql))
        .bind(state)
        .bind(state)
        .bind(node_id)
        .bind(boot_id)
        .execute(pool)
        .await
        .map_err(|e| e.to_string())?
        .rows_affected()
        == 1)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaseRow {
    pub shard: u32,
    pub owner: Option<String>,
    pub epoch: i64,
    pub expires_at: i64,
}

/// Every lease row of a database, and the database time.
pub async fn leases(pool: &AnyPool, d: Dialect, db: &str) -> Result<(Vec<LeaseRow>, i64), String> {
    let sql = format!(
        "SELECT shard_id, owner, epoch, expires_at, {} FROM shard_lease WHERE db = ? ORDER BY shard_id",
        now(d)
    );
    let rows = sqlx::query(d.q(&sql))
        .bind(db)
        .fetch_all(pool)
        .await
        .map_err(|e| e.to_string())?;
    let mut now_ms = 0;
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        now_ms = r.try_get::<i64, _>(4).map_err(|e| e.to_string())?;
        out.push(LeaseRow {
            shard: r.try_get::<i64, _>(0).map_err(|e| e.to_string())? as u32,
            owner: r.try_get(1).map_err(|e| e.to_string())?,
            epoch: r.try_get(2).map_err(|e| e.to_string())?,
            expires_at: r.try_get(3).map_err(|e| e.to_string())?,
        });
    }
    Ok((out, now_ms))
}

pub async fn lease(
    pool: &AnyPool,
    d: Dialect,
    db: &str,
    shard: u32,
) -> Result<Option<(LeaseRow, i64)>, String> {
    let sql = format!(
        "SELECT shard_id, owner, epoch, expires_at, {} FROM shard_lease WHERE db = ? AND shard_id = ?",
        now(d)
    );
    let r = sqlx::query(d.q(&sql))
        .bind(db)
        .bind(i64::from(shard))
        .fetch_optional(pool)
        .await
        .map_err(|e| e.to_string())?;
    let Some(r) = r else { return Ok(None) };
    Ok(Some((
        LeaseRow {
            shard,
            owner: r.try_get(1).map_err(|e| e.to_string())?,
            epoch: r.try_get(2).map_err(|e| e.to_string())?,
            expires_at: r.try_get(3).map_err(|e| e.to_string())?,
        },
        r.try_get(4).map_err(|e| e.to_string())?,
    )))
}

/// Extends every lease this process holds that has not expired. One
/// statement; the caller compares with what it thinks it holds.
pub async fn renew(
    pool: &AnyPool,
    d: Dialect,
    db: &str,
    owner: &str,
    lease_ms: u64,
) -> Result<u64, String> {
    let n = now(d);
    let sql = format!(
        "UPDATE shard_lease SET expires_at = {n} + ? WHERE db = ? AND owner = ? AND expires_at > {n}"
    );
    Ok(sqlx::query(d.q(&sql))
        .bind(lease_ms as i64)
        .bind(db)
        .bind(owner)
        .execute(pool)
        .await
        .map_err(|e| e.to_string())?
        .rows_affected())
}

/// Takes a shard nobody holds, or whose lease expired more than `grace_ms`
/// ago. The new epoch on success.
pub async fn acquire(
    pool: &AnyPool,
    d: Dialect,
    db: &str,
    shard: u32,
    owner: &str,
    lease_ms: u64,
    grace_ms: u64,
) -> Result<Option<i64>, String> {
    let n = now(d);
    let mut tx = pool.begin().await.map_err(|e| e.to_string())?;
    let sql = format!(
        "UPDATE shard_lease SET owner = ?, epoch = epoch + 1, expires_at = {n} + ? WHERE db = ? AND shard_id = ? AND (owner IS NULL OR expires_at < {n} - ?)"
    );
    let hit = sqlx::query(d.q(&sql))
        .bind(owner)
        .bind(lease_ms as i64)
        .bind(db)
        .bind(i64::from(shard))
        .bind(grace_ms as i64)
        .execute(&mut *tx)
        .await
        .map_err(|e| e.to_string())?
        .rows_affected();
    if hit != 1 {
        let _ = tx.rollback().await;
        return Ok(None);
    }
    let epoch: i64 =
        sqlx::query(d.q("SELECT epoch FROM shard_lease WHERE db = ? AND shard_id = ?"))
            .bind(db)
            .bind(i64::from(shard))
            .fetch_one(&mut *tx)
            .await
            .and_then(|r| r.try_get(0))
            .map_err(|e| e.to_string())?;
    tx.commit().await.map_err(|e| e.to_string())?;
    Ok(Some(epoch))
}

/// Takes again a shard this process owns in the table but stopped using:
/// the epoch moves on (after any batch still holding the row). The new
/// epoch on success.
pub async fn retake(
    pool: &AnyPool,
    d: Dialect,
    db: &str,
    shard: u32,
    owner: &str,
    epoch: i64,
    lease_ms: u64,
) -> Result<Option<i64>, String> {
    let n = now(d);
    let sql = format!(
        "UPDATE shard_lease SET epoch = epoch + 1, expires_at = {n} + ? WHERE db = ? AND shard_id = ? AND owner = ? AND epoch = ?"
    );
    let hit = sqlx::query(d.q(&sql))
        .bind(lease_ms as i64)
        .bind(db)
        .bind(i64::from(shard))
        .bind(owner)
        .bind(epoch)
        .execute(pool)
        .await
        .map_err(|e| e.to_string())?
        .rows_affected();
    Ok((hit == 1).then_some(epoch + 1))
}

/// Hands a held shard to another owner (or to nobody), in one step.
#[allow(clippy::too_many_arguments)]
pub async fn hand_over(
    pool: &AnyPool,
    d: Dialect,
    db: &str,
    shard: u32,
    owner: &str,
    epoch: i64,
    to: Option<&str>,
    lease_ms: u64,
) -> Result<bool, String> {
    let n = now(d);
    let sql = match to {
        Some(_) => format!(
            "UPDATE shard_lease SET owner = ?, epoch = epoch + 1, expires_at = {n} + ? WHERE db = ? AND shard_id = ? AND owner = ? AND epoch = ?"
        ),
        None => format!(
            "UPDATE shard_lease SET owner = NULL, expires_at = {n} WHERE db = ? AND shard_id = ? AND owner = ? AND epoch = ?"
        ),
    };
    let q = sqlx::query(d.q(&sql));
    let q = match to {
        Some(t) => q.bind(t).bind(lease_ms as i64),
        None => q,
    };
    Ok(q.bind(db)
        .bind(i64::from(shard))
        .bind(owner)
        .bind(epoch)
        .execute(pool)
        .await
        .map_err(|e| e.to_string())?
        .rows_affected()
        == 1)
}

/// Start-up checks of docs/11 §9 that the core does not already make.
pub async fn check_server(pool: &AnyPool, d: Dialect) -> Result<(), String> {
    match d {
        Dialect::Postgres => {
            let v: String = sqlx::query("SELECT current_setting('server_version_num')")
                .fetch_one(pool)
                .await
                .and_then(|r| r.try_get(0))
                .map_err(|e| e.to_string())?;
            if v.parse::<u32>().unwrap_or(0) < 140_000 {
                return Err(format!("PostgreSQL 14 or later is required (found {v})"));
            }
            let rec: bool = sqlx::query("SELECT pg_is_in_recovery()")
                .fetch_one(pool)
                .await
                .and_then(|r| r.try_get(0))
                .map_err(|e| e.to_string())?;
            if rec {
                return Err("the database is a replica; point the cluster at the primary".into());
            }
        }
        _ => {
            let r = sqlx::query("SELECT VERSION(), CAST(@@global.read_only AS SIGNED)")
                .fetch_one(pool)
                .await
                .map_err(|e| e.to_string())?;
            let v: String = r.try_get(0).map_err(|e| e.to_string())?;
            let ro: i64 = r.try_get(1).map_err(|e| e.to_string())?;
            let major: u32 = v
                .split('.')
                .next()
                .and_then(|x| x.parse().ok())
                .unwrap_or(0);
            let ok = if v.contains("MariaDB") {
                major >= 10
            } else {
                major >= 8
            };
            if !ok {
                return Err(format!(
                    "MySQL 8.0 or MariaDB 10.5 or later is required (found {v})"
                ));
            }
            if ro != 0 {
                return Err(
                    "the database is read-only (a replica?); point the cluster at the primary"
                        .into(),
                );
            }
        }
    }
    Ok(())
}

/// Marks a serving node as draining; false when no such node serves.
pub async fn drain(pool: &AnyPool, d: Dialect, node_id: &str) -> Result<bool, String> {
    Ok(sqlx::query(d.q(
        "UPDATE cluster_nodes SET state = 'draining' WHERE node_id = ? AND state IN ('joining', 'active')",
    ))
    .bind(node_id)
    .execute(pool)
    .await
    .map_err(|e| e.to_string())?
    .rows_affected()
        == 1)
}
