//! Each tenant in its own database (docs/en/design/multi-tenancy.md): a tenant is a
//! complete AgentIM instance — its own `Hub`, database, writer, caches,
//! keys and settings — opened in this process on first use. The
//! deployment's hub keeps only the tenants table and the deployment's own
//! administration; no tenant data.

use super::*;
use crate::config::{DatabaseConfig, StorageConfig, TenancyConfig, TenantCluster};
use crate::storage::Dialect;
use serde_json::Value;

impl Hub {
    /// Whether tenants live in their own databases (`[[tenancy.clusters]]`).
    pub fn siloed(&self) -> bool {
        self.inner.cfg.tenancy.enabled && !self.inner.cfg.tenancy.clusters.is_empty()
    }

    fn cluster(&self, name: &str) -> ApiResult<&TenantCluster> {
        self.inner
            .cfg
            .tenancy
            .clusters
            .iter()
            .find(|c| c.name == name)
            .ok_or_else(|| {
                rule!(
                    "tenant.cluster",
                    "bad_request",
                    format!("no database cluster named {name:?} in [[tenancy.clusters]]")
                )
            })
    }

    /// The database cluster a new tenant goes to when none is named.
    pub fn default_cluster(&self) -> Option<&str> {
        self.inner
            .cfg
            .tenancy
            .clusters
            .first()
            .map(|c| c.name.as_str())
    }

    /// Checks a cluster name for a new tenant (none: the first one).
    pub(crate) fn pick_cluster(&self, name: Option<&str>) -> ApiResult<Option<String>> {
        if !self.siloed() {
            return Ok(None);
        }
        let name = name.or(self.default_cluster()).unwrap_or_default();
        Ok(Some(self.cluster(name)?.name.clone()))
    }

    /// The configuration of a tenant's own instance: the deployment's, with
    /// one database (`url` of its cluster with `{tenant}` filled in), no
    /// tenancy, no cluster, no deployment webhooks, its own file folder or
    /// prefix.
    fn silo_config(&self, tenant: &str, cluster: &str) -> ApiResult<Config> {
        let c = self.cluster(cluster)?;
        let mut cfg = self.inner.cfg.clone();
        let base = self.inner.cfg.storage.databases[0].clone();
        cfg.storage = StorageConfig {
            control: "main".into(),
            databases: vec![DatabaseConfig {
                name: "main".into(),
                url: silo_url(&c.url, tenant),
                read_pool: Some(c.read_pool.max(1)),
                ..base
            }],
        };
        // The deployment's webhook policy holds inside (SSRF, limits).
        let t = &self.inner.cfg.tenancy;
        cfg.tenancy = TenancyConfig {
            webhook_private_addresses: t.webhook_private_addresses,
            webhook_disable_after_ms: t.webhook_disable_after_ms,
            webhooks_per_tenant: t.webhooks_per_tenant,
            ..TenancyConfig::default()
        };
        cfg.webhooks = vec![];
        cfg.before_send = None;
        cfg.cluster = Default::default();
        cfg.units = vec![];
        cfg.files.dir = format!("{}/tenants/{tenant}", cfg.files.dir.trim_end_matches('/'));
        cfg.files.s3.prefix = format!("{}tenants/{tenant}/", cfg.files.s3.prefix);
        Ok(cfg)
    }

    /// Creates a tenant's database on its cluster (nothing to do when it
    /// exists; SQLite creates its file on opening).
    pub(crate) async fn create_silo_database(&self, tenant: &str, cluster: &str) -> ApiResult<()> {
        let url = silo_url(&self.cluster(cluster)?.url, tenant);
        let d = Dialect::from_url(&url).map_err(|e| err("config", e))?;
        match d {
            Dialect::Sqlite => {
                let path = url
                    .trim_start_matches("sqlite://")
                    .trim_start_matches("sqlite:")
                    .split('?')
                    .next()
                    .unwrap_or_default()
                    .to_string();
                if let Some(dir) = std::path::Path::new(&path).parent()
                    && !dir.as_os_str().is_empty()
                {
                    std::fs::create_dir_all(dir)
                        .map_err(|e| err("storage_unavailable", e.to_string()))?;
                }
                Ok(())
            }
            Dialect::Postgres | Dialect::Mysql => {
                let (server, name) = split_database(&url)?;
                let admin = if d == Dialect::Postgres {
                    format!("{server}/postgres")
                } else {
                    server.to_string()
                };
                let mut c = <sqlx::AnyConnection as sqlx::Connection>::connect(
                    &crate::storage::driver_url(&admin),
                )
                .await
                .map_err(|e| err("storage_unavailable", e.to_string()))?;
                let exists = if d == Dialect::Postgres {
                    sqlx::query("SELECT 1 FROM pg_database WHERE datname = $1")
                        .bind(name.clone())
                        .fetch_optional(&mut c)
                        .await?
                        .is_some()
                } else {
                    false
                };
                if !exists {
                    let sql = if d == Dialect::Postgres {
                        format!("CREATE DATABASE \"{name}\"")
                    } else {
                        format!("CREATE DATABASE IF NOT EXISTS `{name}`")
                    };
                    if let Err(e) = sqlx::query(sqlx::AssertSqlSafe(sql)).execute(&mut c).await
                        && !e.to_string().contains("already exists")
                    {
                        return Err(err("storage_unavailable", e.to_string()));
                    }
                }
                Ok(())
            }
        }
    }

    /// Deletes a tenant in its own database: its connections end, its
    /// database is dropped, then its row (the database first, so a failure
    /// leaves the tenant to delete again).
    pub async fn delete_tenant(&self, tenant: &str) -> ApiResult<()> {
        if !self.inner.tenants.read().unwrap().contains_key(tenant) {
            return Err(rule!("tenant.unknown", "not_found", "no such tenant"));
        }
        if let Some(h) = self.inner.silos.lock().await.remove(tenant) {
            h.close_all_sessions();
            h.close().await;
        }
        self.drop_tenant_database(tenant).await?;
        // Its files (local storage; an S3 prefix is removed by a bucket
        // lifecycle rule on `tenants/<id>/`).
        let dir = format!(
            "{}/tenants/{tenant}",
            self.inner.cfg.files.dir.trim_end_matches('/')
        );
        let _ = std::fs::remove_dir_all(dir);
        self.admin_op(crate::logic::AdminOp::TenantDelete {
            id: tenant.to_string(),
        })
        .await?;
        self.load_tenants().await
    }

    /// Removes a tenant's own database (its instance closed first); the
    /// tenant row stays.
    pub async fn drop_tenant_database(&self, tenant: &str) -> ApiResult<()> {
        let cluster = self
            .inner
            .tenants
            .read()
            .unwrap()
            .get(tenant)
            .and_then(|t| t.cluster.clone());
        match cluster {
            Some(c) => self.drop_silo_database(tenant, &c).await,
            None => Err(rule!("tenant.unknown", "not_found", "no such tenant")),
        }
    }

    /// Removes a tenant's database (its instance closed first).
    async fn drop_silo_database(&self, tenant: &str, cluster: &str) -> ApiResult<()> {
        self.inner.silos.lock().await.remove(tenant);
        let url = silo_url(&self.cluster(cluster)?.url, tenant);
        let d = Dialect::from_url(&url).map_err(|e| err("config", e))?;
        match d {
            Dialect::Sqlite => {
                let path = url
                    .trim_start_matches("sqlite://")
                    .trim_start_matches("sqlite:")
                    .split('?')
                    .next()
                    .unwrap_or_default()
                    .to_string();
                for p in [format!("{path}-wal"), format!("{path}-shm")] {
                    let _ = std::fs::remove_file(p);
                }
                // A closing handle can take a moment to let go (Windows).
                let mut tries = 0;
                loop {
                    match std::fs::remove_file(&path) {
                        Ok(()) => return Ok(()),
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
                        Err(_) if tries < 40 => {
                            tries += 1;
                            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                        }
                        Err(e) => {
                            return Err(err(
                                "storage_unavailable",
                                format!("removing the tenant's database {path}: {e}"),
                            ));
                        }
                    }
                }
            }
            Dialect::Postgres | Dialect::Mysql => {
                let (server, name) = split_database(&url)?;
                let admin = if d == Dialect::Postgres {
                    format!("{server}/postgres")
                } else {
                    server.to_string()
                };
                let mut c = <sqlx::AnyConnection as sqlx::Connection>::connect(
                    &crate::storage::driver_url(&admin),
                )
                .await
                .map_err(|e| err("storage_unavailable", e.to_string()))?;
                let sql = if d == Dialect::Postgres {
                    format!("DROP DATABASE IF EXISTS \"{name}\" WITH (FORCE)")
                } else {
                    format!("DROP DATABASE IF EXISTS `{name}`")
                };
                sqlx::query(sqlx::AssertSqlSafe(sql))
                    .execute(&mut c)
                    .await?;
                Ok(())
            }
        }
    }

    /// A tenant's own instance, opened on first use (unknown or suspended
    /// tenants: `not_found`). Concurrent first uses open it once.
    pub async fn tenant_hub(&self, tenant: &str) -> ApiResult<Hub> {
        let (cluster, limits) = {
            let t = self.inner.tenants.read().unwrap();
            match t.get(tenant) {
                Some(row) if row.suspended_at.is_none() => {
                    (row.cluster.clone(), row.limits.clone())
                }
                _ => (None, Value::Null),
            }
        };
        let Some(cluster) = cluster else {
            return Err(rule!("tenant.unknown", "not_found", "no such tenant"));
        };
        let mut open = self.inner.silos.lock().await;
        if let Some(h) = open.get(tenant) {
            return Ok(h.clone());
        }
        let cfg = self.silo_config(tenant, &cluster)?;
        let hub = Hub::open(cfg, self.inner.sh.cfg.clone()).await?;
        hub.set_own_limits(limits);
        if self
            .inner
            .silo_background
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            hub.spawn_background();
        }
        open.insert(tenant.to_string(), hub.clone());
        Ok(hub)
    }

    /// The plan limits of this tenant's own instance.
    pub fn set_own_limits(&self, limits: Value) {
        *self.inner.own_limits.write().unwrap() = if limits.is_object() {
            limits
        } else {
            serde_json::json!({})
        };
    }

    /// Opens every active tenant's instance (the server at start: their
    /// webhooks deliver and their upkeep runs without waiting for a
    /// request). A tenant that fails to open is logged and tried again on
    /// its next request.
    pub async fn open_all_tenants(&self) {
        let ids: Vec<String> = self
            .inner
            .tenants
            .read()
            .unwrap()
            .values()
            .filter(|t| t.suspended_at.is_none() && t.cluster.is_some())
            .map(|t| t.id.clone())
            .collect();
        for id in ids {
            if let Err(e) = self.tenant_hub(&id).await {
                tracing::warn!(tenant = id, error = e.message, "opening a tenant");
            }
        }
    }

    /// Tenants' instances run their own background work (the server sets
    /// this; tests drive it themselves).
    pub fn run_tenant_background(&self) {
        self.inner
            .silo_background
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

/// A cluster's database address for one tenant: `{tenant}` with `-`
/// written `_` (valid in database and file names).
pub fn silo_url(template: &str, tenant: &str) -> String {
    let url = template.replace("{tenant}", &tenant.replace('-', "_"));
    // SQLite creates the tenant's file on first use.
    if url.starts_with("sqlite:") && !url.contains('?') {
        format!("{url}?mode=rwc")
    } else {
        url
    }
}

/// `scheme://user:pass@host:port/name?x` → (`scheme://user:pass@host:port`, `name`).
fn split_database(url: &str) -> ApiResult<(String, String)> {
    let no_query = url.split('?').next().unwrap_or(url);
    let (scheme, rest) = no_query
        .split_once("://")
        .ok_or_else(|| err("config", "a database URL needs scheme://"))?;
    let (server, name) = rest
        .rsplit_once('/')
        .filter(|(_, n)| !n.is_empty())
        .ok_or_else(|| {
            err(
                "config",
                "the cluster URL needs a database name with {tenant}",
            )
        })?;
    if !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
        return Err(err(
            "config",
            format!("database name {name:?}: letters, digits, _"),
        ));
    }
    Ok((format!("{scheme}://{server}"), name.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls() {
        assert_eq!(
            silo_url("postgres://u:p@h:5432/agentim_{tenant}", "p-1a"),
            "postgres://u:p@h:5432/agentim_p_1a"
        );
        assert_eq!(
            split_database("postgres://u:p@h:5432/agentim_p1?sslmode=require").unwrap(),
            ("postgres://u:p@h:5432".into(), "agentim_p1".into())
        );
        assert!(split_database("postgres://h/").is_err());
        assert!(split_database("postgres://h/x;drop").is_err());
        assert_eq!(
            silo_url("sqlite://data/tenants/{tenant}.db", "acme"),
            "sqlite://data/tenants/acme.db?mode=rwc"
        );
    }
}
