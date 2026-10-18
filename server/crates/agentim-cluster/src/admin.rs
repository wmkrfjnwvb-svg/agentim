//! `agentim cluster …` (docs/11 §7): reads and writes the cluster tables
//! directly, so it works while the nodes run and needs no node to answer.

use std::collections::BTreeMap;

use agentim_core::cli::ClusterCmd;
use agentim_core::cluster::BoxFuture;
use agentim_core::config::Config;
use agentim_core::storage::{self, Dialect};

use crate::{offline_ms, sql};

pub fn run(cfg: Config, cmd: ClusterCmd) -> BoxFuture<Result<(), String>> {
    Box::pin(async move {
        // Runs before (instead of) opening the hub, which installs them.
        sqlx::any::install_default_drivers();
        let out = match cmd {
            ClusterCmd::Status => status(&cfg).await?,
            ClusterCmd::Drain { node } => drain(&cfg, &node).await?,
        };
        println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
        Ok(())
    })
}

async fn pool(url: &str) -> Result<(Dialect, sqlx::AnyPool), String> {
    let d = Dialect::from_url(url)?;
    if d == Dialect::Sqlite {
        return Err("a cluster needs PostgreSQL or MySQL".into());
    }
    let p = storage::open_pool(url, d, 1).await.map_err(|e| e.message)?;
    Ok((d, p))
}

fn control(cfg: &Config) -> Result<&agentim_core::config::DatabaseConfig, String> {
    cfg.storage
        .databases
        .iter()
        .find(|d| d.name == cfg.storage.control)
        .ok_or_else(|| "no control database".to_string())
}

/// Nodes and, per database, how many shards each node holds.
pub async fn status(cfg: &Config) -> Result<serde_json::Value, String> {
    let c = control(cfg)?;
    let (d, p) = pool(&c.url).await?;
    let (nodes, now) = sql::nodes(&p, d).await?;
    let offline = offline_ms(&cfg.cluster);
    let nodes: Vec<_> = nodes
        .iter()
        .map(|n| {
            let age = now - n.heartbeat_at;
            serde_json::json!({
                "node_id": n.node_id,
                "addr": n.addr,
                "state": if n.state != "left" && age > offline { "offline" } else { n.state.as_str() },
                "heartbeat_ms_ago": age,
            })
        })
        .collect();
    let mut dbs = vec![];
    for db in &cfg.storage.databases {
        let (d, p) = pool(&db.url).await?;
        let (rows, now) = sql::leases(&p, d, &db.name).await?;
        let mut held: BTreeMap<String, u32> = BTreeMap::new();
        let mut free = 0;
        for r in &rows {
            match &r.owner {
                Some(o) if r.expires_at > now => {
                    // Owners are `node_id:boot_id`.
                    let node = o.rsplit_once(':').map(|(n, _)| n).unwrap_or(o).to_string();
                    *held.entry(node).or_default() += 1;
                }
                _ => free += 1,
            }
        }
        dbs.push(serde_json::json!({
            "database": db.name,
            "shards": rows.len(),
            "held": held,
            "unheld": free,
        }));
    }
    Ok(serde_json::json!({"nodes": nodes, "databases": dbs}))
}

/// Takes a node out of service (it notices within a heartbeat).
pub async fn drain(cfg: &Config, node: &str) -> Result<serde_json::Value, String> {
    let c = control(cfg)?;
    let (d, p) = pool(&c.url).await?;
    if !sql::drain(&p, d, node).await? {
        return Err(format!("no serving node {node:?}"));
    }
    Ok(serde_json::json!({
        "node_id": node,
        "state": sql::DRAINING,
        "next": "the node hands its shards over within a few seconds and reports not ready; then stop it",
    }))
}
