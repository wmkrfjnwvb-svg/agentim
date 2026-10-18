//! Database replicas and "an acknowledged message is never lost"
//! (docs/11 §8). A write is acknowledged only once it is safe from a
//! primary failure:
//!
//! - `None`: no replica in sight; the commit is all there is.
//! - `DatabaseSync`: the database waits for a replica itself (PostgreSQL
//!   synchronous standbys; MySQL semi-sync, checked after each batch since
//!   it can fall back to asynchronous on its own).
//! - `AppWait`: asynchronous replicas (the default setup); the writer waits
//!   until every replicating replica has the commit, and never silently
//!   gives up: no replica in sight is "not confirmed", not "nothing to
//!   wait for".
//! - `ManagedSync`: declared by the operator for managed services that
//!   replicate synchronously out of sight (RDS Multi-AZ, Cloud SQL HA).

use std::time::{Duration, Instant};

use sqlx::{AnyConnection, AnyPool, Row};

use crate::config::{ReplicationConfig, ReplicationMode};
use crate::storage::Dialect;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    None,
    DatabaseSync,
    AppWait,
    ManagedSync,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::None => "none",
            Mode::DatabaseSync => "database_sync",
            Mode::AppWait => "app_wait",
            Mode::ManagedSync => "managed_sync",
        }
    }
}

/// Where a commit is in the primary's log (PostgreSQL LSN, MySQL GTID set,
/// MariaDB GTID position).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Position(pub String);

fn is_mariadb(version: &str) -> bool {
    version.to_ascii_lowercase().contains("mariadb")
}

async fn mysql_version(c: &mut AnyConnection) -> Result<String, String> {
    sqlx::query("SELECT VERSION()")
        .fetch_one(&mut *c)
        .await
        .and_then(|r| r.try_get::<String, _>(0))
        .map_err(|e| e.to_string())
}

/// Replicas the primary sees streaming to it.
pub async fn replicas_seen(c: &mut AnyConnection, d: Dialect) -> Result<i64, String> {
    match d {
        Dialect::Postgres => {
            sqlx::query("SELECT COUNT(*) FROM pg_stat_replication WHERE state = 'streaming'")
                .fetch_one(&mut *c)
                .await
                .and_then(|r| r.try_get::<i64, _>(0))
                .map_err(|e| e.to_string())
        }
        Dialect::Mysql => {
            let v = mysql_version(c).await?;
            let q = if is_mariadb(&v) {
                "SHOW SLAVE HOSTS"
            } else {
                "SHOW REPLICAS"
            };
            sqlx::query(sqlx::AssertSqlSafe(q))
                .fetch_all(&mut *c)
                .await
                .map(|r| r.len() as i64)
                .map_err(|e| e.to_string())
        }
        Dialect::Sqlite => Ok(0),
    }
}

async fn mysql_status(c: &mut AnyConnection, names: &[&str]) -> Result<Option<String>, String> {
    for n in names {
        let r = sqlx::query(sqlx::AssertSqlSafe(format!("SHOW STATUS LIKE '{n}'")))
            .fetch_optional(&mut *c)
            .await
            .map_err(|e| e.to_string())?;
        if let Some(r) = r {
            return Ok(Some(r.try_get::<String, _>(1).map_err(|e| e.to_string())?));
        }
    }
    Ok(None)
}

/// MySQL semi-sync is on and has not fallen back to asynchronous.
pub async fn mysql_semi_sync_on(c: &mut AnyConnection) -> Result<bool, String> {
    Ok(mysql_status(
        c,
        &["Rpl_semi_sync_source_status", "Rpl_semi_sync_master_status"],
    )
    .await?
    .is_some_and(|v| v.eq_ignore_ascii_case("ON")))
}

/// The mode of a database at start-up (docs/11 §8.1).
pub async fn detect(
    c: &mut AnyConnection,
    d: Dialect,
    cfg: &ReplicationConfig,
) -> Result<Mode, String> {
    match cfg.mode {
        ReplicationMode::ManagedSync => return Ok(Mode::ManagedSync),
        ReplicationMode::None => return Ok(Mode::None),
        ReplicationMode::Auto => {}
    }
    if d == Dialect::Sqlite {
        return Ok(Mode::None);
    }
    let seen = replicas_seen(c, d).await?;
    let sync = match d {
        Dialect::Postgres => {
            let names: String = sqlx::query("SELECT current_setting('synchronous_standby_names')")
                .fetch_one(&mut *c)
                .await
                .and_then(|r| r.try_get(0))
                .map_err(|e| e.to_string())?;
            let level: String = sqlx::query("SELECT current_setting('synchronous_commit')")
                .fetch_one(&mut *c)
                .await
                .and_then(|r| r.try_get(0))
                .map_err(|e| e.to_string())?;
            !names.trim().is_empty() && matches!(level.as_str(), "on" | "remote_apply")
        }
        _ => mysql_semi_sync_on(c).await?,
    };
    if sync {
        return Ok(Mode::DatabaseSync);
    }
    if seen == 0 {
        return Ok(Mode::None);
    }
    // The application waits: check it can.
    match d {
        Dialect::Postgres => {
            let hidden: i64 = sqlx::query(
                "SELECT COUNT(*) FROM pg_stat_replication WHERE state = 'streaming' AND flush_lsn IS NULL",
            )
            .fetch_one(&mut *c)
            .await
            .and_then(|r| r.try_get(0))
            .map_err(|e| e.to_string())?;
            if hidden > 0 {
                return Err(
                    "replicas are visible but not their positions: GRANT pg_monitor TO <the AgentIM user>, \
                     or declare replication.mode = \"managed_sync\" / \"none\""
                        .into(),
                );
            }
        }
        _ => {
            if cfg.replica_urls.is_empty() {
                return Err(format!(
                    "{seen} asynchronous replica(s) and no replication.replica_urls: AgentIM waits for each \
                     replica and needs to reach them (or turn on semi-sync, or declare \
                     replication.mode = \"managed_sync\" / \"none\")"
                ));
            }
            let v = mysql_version(c).await?;
            if !is_mariadb(&v) {
                let gtid: String = sqlx::query("SELECT CAST(@@GLOBAL.gtid_mode AS CHAR)")
                    .fetch_one(&mut *c)
                    .await
                    .and_then(|r| r.try_get(0))
                    .map_err(|e| e.to_string())?;
                if !gtid.eq_ignore_ascii_case("ON") {
                    return Err("waiting for replicas needs gtid_mode = ON".into());
                }
            }
        }
    }
    Ok(Mode::AppWait)
}

/// The primary's position after a commit, read on the connection that
/// committed (so it covers that commit).
pub async fn position(c: &mut AnyConnection, d: Dialect) -> Result<Position, String> {
    let q = match d {
        Dialect::Postgres => "SELECT CAST(pg_current_wal_lsn() AS TEXT)".to_string(),
        _ => {
            let v = mysql_version(c).await?;
            if is_mariadb(&v) {
                "SELECT CAST(@@GLOBAL.gtid_binlog_pos AS CHAR)".to_string()
            } else {
                "SELECT CAST(@@GLOBAL.gtid_executed AS CHAR)".to_string()
            }
        }
    };
    sqlx::query(sqlx::AssertSqlSafe(q))
        .fetch_one(&mut *c)
        .await
        .and_then(|r| r.try_get::<String, _>(0))
        .map(Position)
        .map_err(|e| e.to_string())
}

/// Replicas that have `p` (flushed) and replicas replicating.
async fn pg_progress(c: &mut AnyConnection, p: &Position) -> Result<(i64, i64), String> {
    let r = sqlx::query(
        "SELECT COUNT(*) FILTER (WHERE flush_lsn >= CAST($1 AS pg_lsn)), COUNT(*) \
         FROM pg_stat_replication WHERE state = 'streaming'",
    )
    .bind(&p.0)
    .fetch_one(&mut *c)
    .await
    .map_err(|e| e.to_string())?;
    Ok((
        r.try_get(0).map_err(|e| e.to_string())?,
        r.try_get(1).map_err(|e| e.to_string())?,
    ))
}

/// Whether a listed replica has `p`; None when it is the primary itself
/// (after a failover) or does not answer.
async fn mysql_has(replica: &AnyPool, primary: &str, p: &Position) -> Option<bool> {
    let mut c = replica.acquire().await.ok()?;
    let (id, _) = identity(&mut c, Dialect::Mysql).await.ok()?;
    if id == primary {
        return None;
    }
    mysql_reached(&mut c, p).await.ok()
}

async fn mysql_reached(c: &mut AnyConnection, p: &Position) -> Result<bool, String> {
    let v = mysql_version(c).await?;
    let ok: i64 = if is_mariadb(&v) {
        // 0 = reached, -1 = not within the (zero) timeout.
        sqlx::query("SELECT MASTER_GTID_WAIT(?, 0)")
            .bind(&p.0)
            .fetch_one(&mut *c)
            .await
            .and_then(|r| r.try_get::<i64, _>(0))
            .map(|r| (r == 0) as i64)
            .map_err(|e| e.to_string())?
    } else {
        sqlx::query("SELECT GTID_SUBSET(?, @@GLOBAL.gtid_executed)")
            .bind(&p.0)
            .fetch_one(&mut *c)
            .await
            .and_then(|r| r.try_get::<i64, _>(0))
            .map_err(|e| e.to_string())?
    };
    Ok(ok == 1)
}

/// Waits until the replicas have `p`: all that replicate (or the number
/// configured), at least one. Err when they do not within `wait`.
/// PostgreSQL asks the primary on the writer's connection; MySQL asks each
/// listed replica that answers and is not the primary (`primary_id`).
pub async fn wait(
    d: Dialect,
    primary: &mut AnyConnection,
    primary_id: &str,
    replicas: &[AnyPool],
    required: Option<u32>,
    p: &Position,
    wait: Duration,
) -> Result<(), String> {
    let deadline = Instant::now() + wait;
    let primary_id = primary_id.to_string();
    let primary_id = primary_id.as_str();
    let mut pause = Duration::from_millis(2);
    loop {
        let (have, total) = match d {
            Dialect::Postgres => pg_progress(primary, p).await?,
            _ => {
                let (mut have, mut total) = (0, 0);
                // A replica that hangs counts as replicating but behind.
                let left = deadline
                    .saturating_duration_since(Instant::now())
                    .max(Duration::from_millis(50));
                for r in replicas {
                    match tokio::time::timeout(left, mysql_has(r, primary_id, p)).await {
                        Ok(Some(true)) => {
                            have += 1;
                            total += 1;
                        }
                        Ok(Some(false)) | Err(_) => total += 1,
                        Ok(None) => {}
                    }
                }
                (have, total)
            }
        };
        let need = required.map(i64::from).unwrap_or(total).max(1);
        if have >= need {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "{have} of {need} replica(s) confirmed the write within {} ms",
                wait.as_millis()
            ));
        }
        tokio::time::sleep(pause).await;
        pause = (pause * 2).min(Duration::from_millis(50));
    }
}

/// Which primary this is (docs/11 §8.4): PostgreSQL system identifier and
/// timeline, MySQL server UUID (MariaDB server id and GTID domain); and
/// whether it accepts writes.
pub async fn identity(c: &mut AnyConnection, d: Dialect) -> Result<(String, bool), String> {
    match d {
        Dialect::Postgres => {
            let r = sqlx::query(
                "SELECT pg_is_in_recovery() OR current_setting('transaction_read_only') = 'on', \
                 CASE WHEN pg_is_in_recovery() THEN '' ELSE substr(pg_walfile_name(pg_current_wal_lsn()), 1, 8) END",
            )
            .fetch_one(&mut *c)
            .await
            .map_err(|e| e.to_string())?;
            let replica: bool = r.try_get(0).map_err(|e| e.to_string())?;
            let timeline: String = r.try_get(1).map_err(|e| e.to_string())?;
            let system =
                sqlx::query("SELECT CAST(system_identifier AS TEXT) FROM pg_control_system()")
                    .fetch_one(&mut *c)
                    .await
                    .and_then(|r| r.try_get::<String, _>(0))
                    .unwrap_or_default();
            Ok((format!("pg:{system}:{timeline}"), !replica))
        }
        Dialect::Mysql => {
            let v = mysql_version(c).await?;
            let (q, ro) = if is_mariadb(&v) {
                (
                    "SELECT CONCAT(@@server_id, ':', @@gtid_domain_id)",
                    "SELECT CAST(@@read_only AS SIGNED)",
                )
            } else {
                (
                    "SELECT @@server_uuid",
                    "SELECT CAST(@@read_only + @@super_read_only AS SIGNED)",
                )
            };
            let id: String = sqlx::query(sqlx::AssertSqlSafe(q))
                .fetch_one(&mut *c)
                .await
                .and_then(|r| r.try_get(0))
                .map_err(|e| e.to_string())?;
            let ro: i64 = sqlx::query(sqlx::AssertSqlSafe(ro))
                .fetch_one(&mut *c)
                .await
                .and_then(|r| r.try_get(0))
                .map_err(|e| e.to_string())?;
            Ok((format!("mysql:{id}"), ro == 0))
        }
        Dialect::Sqlite => Ok(("sqlite".into(), true)),
    }
}

/// An error that says this server stopped being the primary (it is a
/// replica now, read-only, or shutting down).
pub fn is_not_primary(message: &str) -> bool {
    let m = message.to_ascii_lowercase();
    [
        "read-only transaction",
        "read only transaction",
        "recovery is in progress",
        "terminating connection due to administrator command",
        "--read-only",
        "super-read-only",
        "read_only",
        "1290",
        "1836",
        "1792",
        "25006",
        "57p01",
    ]
    .iter()
    .any(|p| m.contains(p))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_that_say_the_primary_moved() {
        for m in [
            "error returned from database: cannot execute UPDATE in a read-only transaction",
            "error returned from database: 1290 (HY000): The MySQL server is running with the --read-only option",
            "terminating connection due to administrator command",
            "recovery is in progress",
            "1836 (HY000): Running in read-only mode",
        ] {
            assert!(is_not_primary(m), "{m}");
        }
        for m in [
            "deadlock detected",
            "duplicate key value",
            "connection refused",
        ] {
            assert!(!is_not_primary(m), "{m}");
        }
        assert_eq!(Mode::AppWait.as_str(), "app_wait");
    }
}
