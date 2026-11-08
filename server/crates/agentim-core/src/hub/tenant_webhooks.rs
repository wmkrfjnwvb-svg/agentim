//! Webhooks of each tenant (docs/en/design/multi-tenancy.md): stored in the control database,
//! managed through the deployment's admin API, delivered by one task per
//! webhook and database, each with its own cursor, so one tenant's failing
//! receiver slows only itself.

use serde_json::Value;

use super::*;
use crate::logic::AdminOp;
use crate::webhook::TenantHook;

/// A tenant's webhook as the admin API sends it.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TenantHookReq {
    pub url: String,
    pub secret: String,
    #[serde(default)]
    pub events: Option<Vec<String>>,
    #[serde(default)]
    pub max_attempts: Option<u32>,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    #[serde(default)]
    pub backoff_ms: Option<u64>,
    #[serde(default)]
    pub enabled: Option<bool>,
}

impl Hub {
    /// Reads every tenant's webhooks into the writers' view and starts,
    /// restarts or stops their deliverers to match.
    pub(crate) async fn load_tenant_hooks(&self) -> ApiResult<()> {
        // Also on a tenant's own instance (its webhooks are `default`'s) and
        // a self-hosted server (none stored).
        let hooks = self
            .read(self.control(), |db| {
                Box::pin(async move { db.tenant_webhooks_all().await })
            })
            .await?;
        *self.inner.sh.tenant_hooks.write().unwrap() = hooks.clone();
        self.reconcile_tenant_deliverers(hooks);
        Ok(())
    }

    fn reconcile_tenant_deliverers(&self, hooks: Vec<TenantHook>) {
        // Only enabled hooks of tenants that are not suspended deliver.
        let want: HashMap<String, TenantHook> = hooks
            .into_iter()
            .filter(|h| h.enabled && self.tenant_active(&h.tenant))
            .map(|h| (h.cursor_name(), h))
            .collect();
        let mut running = self.inner.tenant_deliverers.lock().unwrap();
        running.retain(|k, (h, tasks)| {
            let keep = want.get(k) == Some(h);
            if !keep {
                for t in tasks.iter() {
                    t.abort();
                }
            }
            keep
        });
        let deploy: Vec<String> = self
            .inner
            .cfg
            .webhooks
            .iter()
            .map(|w| w.name.clone())
            .collect();
        let t = &self.inner.cfg.tenancy;
        for (k, h) in want {
            if running.contains_key(&k) {
                continue;
            }
            let scope = crate::webhook::TenantScope {
                tenant: h.tenant.clone(),
                name: h.name.clone(),
                private_ok: t.webhook_private_addresses,
                disable_after_ms: t.webhook_disable_after_ms,
                control: self.control().clone(),
            };
            let tasks = self
                .inner
                .shards
                .iter()
                .zip(&self.inner.wakes)
                .map(|(s, w)| {
                    tokio::spawn(crate::webhook::deliver_loop(
                        s.clone(),
                        h.config(),
                        deploy.clone(),
                        w.subscribe(),
                        self.inner.http.clone(),
                        Some(scope.clone()),
                    ))
                })
                .collect();
            running.insert(k, (h, tasks));
        }
    }

    /// A tenant's webhooks (never their secrets); in its own instance when
    /// it has one.
    pub async fn tenant_webhooks_of(&self, tenant: &str) -> ApiResult<Vec<Value>> {
        if self.siloed() && tenant != crate::tenant::DEFAULT {
            return Ok(self
                .tenant_hub(tenant)
                .await?
                .tenant_webhooks(crate::tenant::DEFAULT));
        }
        Ok(self.tenant_webhooks(tenant))
    }

    /// A tenant's webhooks (never their secrets).
    pub fn tenant_webhooks(&self, tenant: &str) -> Vec<Value> {
        self.inner
            .sh
            .tenant_hooks
            .read()
            .unwrap()
            .iter()
            .filter(|h| h.tenant == tenant)
            .map(TenantHook::view)
            .collect()
    }

    /// Creates or replaces a tenant's webhook (the deployment's admin API).
    pub async fn put_tenant_webhook(
        &self,
        tenant: &str,
        name: &str,
        r: TenantHookReq,
    ) -> ApiResult<Value> {
        if self.siloed() && tenant != crate::tenant::DEFAULT {
            let h = self.tenant_hub(tenant).await?;
            return Box::pin(h.put_tenant_webhook(crate::tenant::DEFAULT, name, r)).await;
        }
        let ok_name = !name.is_empty()
            && name.len() <= 64
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "_-".contains(c));
        if !ok_name {
            return Err(rule!(
                "webhook.name",
                "bad_request",
                "name: 1..64 of A-Z a-z 0-9 _ -"
            ));
        }
        let t = &self.inner.cfg.tenancy;
        if let Err(why) = crate::webhook::check_tenant_url(&r.url, t.webhook_private_addresses) {
            return Err(rule!("webhook.url", "bad_request", format!("url: {why}")));
        }
        match crate::webhook::key_of(&r.secret) {
            Some(k) if k.len() >= 16 => {}
            _ => {
                return Err(rule!(
                    "webhook.secret",
                    "bad_request",
                    "secret: at least 16 bytes (whsec_ + base64, or text)"
                ));
            }
        }
        let events = r.events.unwrap_or_else(|| vec!["*".into()]);
        if events.is_empty()
            || events.len() > 32
            || events.iter().any(|e| !crate::webhook::valid_pattern(e))
        {
            return Err(rule!(
                "webhook.events",
                "bad_request",
                "events: like msg.created, msg.* or * (1..32 of them)"
            ));
        }
        let max_attempts = r.max_attempts.unwrap_or(8);
        let timeout_ms = r.timeout_ms.unwrap_or(5_000);
        let backoff_ms = r.backoff_ms.unwrap_or(1_000);
        if !(1..=20).contains(&max_attempts)
            || !(100..=30_000).contains(&timeout_ms)
            || backoff_ms > 300_000
        {
            return Err(rule!(
                "webhook.retries",
                "bad_request",
                "max_attempts 1..20, timeout_ms 100..30000, backoff_ms at most 300000"
            ));
        }
        let now = now_ms();
        let existing = self
            .inner
            .sh
            .tenant_hooks
            .read()
            .unwrap()
            .iter()
            .find(|h| h.tenant == tenant && h.name == name)
            .map(|h| h.created_at);
        let created_at = existing.unwrap_or(now);
        let hook = TenantHook {
            tenant: tenant.to_string(),
            name: name.to_string(),
            url: r.url,
            secret: r.secret,
            events,
            max_attempts,
            timeout_ms,
            backoff_ms,
            enabled: r.enabled.unwrap_or(true),
            created_at,
            updated_at: now,
        };
        let cursor = hook.cursor_name();
        let v = self
            .control()
            .call(Op::Admin(AdminOp::TenantWebhookPut {
                hook,
                max: t.webhooks_per_tenant,
            }))
            .await?;
        // A new webhook starts where the outbox is now, on every database
        // (a changed one keeps its place and its backlog).
        let deploy: Vec<String> = self
            .inner
            .cfg
            .webhooks
            .iter()
            .map(|w| w.name.clone())
            .collect();
        for s in self.inner.shards.iter().filter(|_| existing.is_none()) {
            let end = crate::webhook::outbox_end(s).await;
            s.call(Op::WebhookAck {
                name: cursor.clone(),
                upto: end,
                dropped: vec![],
                all: deploy.clone(),
            })
            .await?;
        }
        self.load_tenant_hooks().await?;
        Ok(v)
    }

    /// Removes a tenant's webhook (its undelivered events are no longer
    /// sent; the outbox clean-up or retention removes them).
    pub async fn delete_tenant_webhook(&self, tenant: &str, name: &str) -> ApiResult<Value> {
        if self.siloed() && tenant != crate::tenant::DEFAULT {
            let h = self.tenant_hub(tenant).await?;
            return Box::pin(h.delete_tenant_webhook(crate::tenant::DEFAULT, name)).await;
        }
        let v = self
            .control()
            .call(Op::Admin(AdminOp::TenantWebhookDelete {
                tenant: tenant.to_string(),
                name: name.to_string(),
            }))
            .await?;
        self.load_tenant_hooks().await?;
        Ok(v)
    }
}
