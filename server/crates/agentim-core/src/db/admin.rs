//! Console accounts, sessions, admin keys and app settings (docs/en/design/developer-experience.md).

use serde_json::{Value, json};
use sqlx::Row;

use super::Db;
use crate::model::*;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct AdminRow {
    pub id: String,
    pub name: String,
    /// owner, developer, operator or moderator.
    pub role: String,
    pub created_at: i64,
    pub disabled_at: Option<i64>,
}

impl AdminRow {
    pub fn view(&self) -> Value {
        json!({"id": self.id, "name": self.name, "role": self.role, "created_at": self.created_at, "disabled": self.disabled_at.is_some()})
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AdminSessionRow {
    pub hash: String,
    pub admin_id: String,
    pub csrf: String,
    pub created_at: i64,
    pub last_seen: i64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AdminKeyRow {
    pub key_id: String,
    pub hash: String,
    pub name: String,
    pub role: String,
    pub created_at: i64,
    pub revoked_at: Option<i64>,
}

/// A tenant of a multi-tenant server (docs/en/design/multi-tenancy.md).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TenantRow {
    pub id: String,
    pub name: String,
    /// Plan limits as JSON (docs/en/design/multi-tenancy.md).
    pub limits: Value,
    pub suspended_at: Option<i64>,
    pub created_at: i64,
    /// The database cluster of its own database; none: it shares
    /// the deployment's.
    #[serde(default)]
    pub cluster: Option<String>,
}

impl TenantRow {
    pub fn view(&self) -> Value {
        json!({"id": self.id, "name": self.name, "limits": self.limits, "suspended": self.suspended_at.is_some(), "suspended_at": self.suspended_at, "created_at": self.created_at, "cluster": self.cluster})
    }
}

impl AdminKeyRow {
    pub fn view(&self) -> Value {
        json!({"key_id": self.key_id, "name": self.name, "role": self.role, "created_at": self.created_at, "revoked": self.revoked_at.is_some()})
    }
}

fn admin_row(r: &sqlx::any::AnyRow) -> sqlx::Result<AdminRow> {
    Ok(AdminRow {
        id: r.try_get("id")?,
        name: r.try_get("name")?,
        role: r.try_get("role")?,
        created_at: r.try_get("created_at")?,
        disabled_at: r.try_get("disabled_at")?,
    })
}

impl Db<'_> {
    pub async fn admins(&mut self) -> ApiResult<Vec<AdminRow>> {
        let rows = sqlx::query(
            "SELECT id, name, role, created_at, disabled_at FROM admins ORDER BY created_at, id",
        )
        .fetch_all(&mut *self.c)
        .await?;
        rows.iter()
            .map(admin_row)
            .collect::<sqlx::Result<_>>()
            .map_err(Into::into)
    }

    pub async fn admin_by(&mut self, col: &str, v: &str) -> ApiResult<Option<AdminRow>> {
        let col = if col == "name" { "name" } else { "id" };
        let r = sqlx::query(self.d.q(&format!(
            "SELECT id, name, role, created_at, disabled_at FROM admins WHERE {col} = ?"
        )))
        .bind(v)
        .fetch_optional(&mut *self.c)
        .await?;
        Ok(r.as_ref().map(admin_row).transpose()?)
    }

    pub async fn insert_admin(&mut self, a: &AdminRow) -> ApiResult<()> {
        sqlx::query(self.d.q(
            "INSERT INTO admins (id, name, role, created_at, disabled_at) VALUES (?, ?, ?, ?, ?)",
        ))
        .bind(&a.id)
        .bind(&a.name)
        .bind(&a.role)
        .bind(a.created_at)
        .bind(a.disabled_at)
        .execute(&mut *self.c)
        .await
        .map_err(|e| ApiError::new("conflict", format!("admin name taken ({e})")))?;
        Ok(())
    }

    pub async fn update_admin(
        &mut self,
        id: &str,
        role: Option<&str>,
        disabled: Option<bool>,
    ) -> ApiResult<()> {
        if let Some(r) = role {
            sqlx::query(self.d.q("UPDATE admins SET role = ? WHERE id = ?"))
                .bind(r)
                .bind(id)
                .execute(&mut *self.c)
                .await?;
        }
        if let Some(d) = disabled {
            sqlx::query(self.d.q("UPDATE admins SET disabled_at = ? WHERE id = ?"))
                .bind(d.then(now_ms))
                .bind(id)
                .execute(&mut *self.c)
                .await?;
        }
        Ok(())
    }

    // ---- sessions

    pub async fn insert_admin_session(&mut self, s: &AdminSessionRow) -> ApiResult<()> {
        sqlx::query(self.d.q(
            "INSERT INTO admin_sessions (hash, admin_id, csrf, created_at, last_seen) VALUES (?, ?, ?, ?, ?)",
        ))
        .bind(&s.hash)
        .bind(&s.admin_id)
        .bind(&s.csrf)
        .bind(s.created_at)
        .bind(s.last_seen)
        .execute(&mut *self.c)
        .await?;
        Ok(())
    }

    pub async fn admin_session(&mut self, hash: &str) -> ApiResult<Option<AdminSessionRow>> {
        let r = sqlx::query(self.d.q(
            "SELECT hash, admin_id, csrf, created_at, last_seen FROM admin_sessions WHERE hash = ?",
        ))
        .bind(hash)
        .fetch_optional(&mut *self.c)
        .await?;
        Ok(match r {
            Some(r) => Some(AdminSessionRow {
                hash: r.try_get(0)?,
                admin_id: r.try_get(1)?,
                csrf: r.try_get(2)?,
                created_at: r.try_get(3)?,
                last_seen: r.try_get(4)?,
            }),
            None => None,
        })
    }

    pub async fn touch_admin_session(&mut self, hash: &str) -> ApiResult<()> {
        sqlx::query(
            self.d
                .q("UPDATE admin_sessions SET last_seen = ? WHERE hash = ?"),
        )
        .bind(now_ms())
        .bind(hash)
        .execute(&mut *self.c)
        .await?;
        Ok(())
    }

    /// One session (`hash`), or every session of an admin.
    pub async fn delete_admin_sessions(
        &mut self,
        admin_id: Option<&str>,
        hash: Option<&str>,
    ) -> ApiResult<u64> {
        let r = match (admin_id, hash) {
            (_, Some(h)) => {
                sqlx::query(self.d.q("DELETE FROM admin_sessions WHERE hash = ?"))
                    .bind(h)
                    .execute(&mut *self.c)
                    .await?
            }
            (Some(a), None) => {
                sqlx::query(self.d.q("DELETE FROM admin_sessions WHERE admin_id = ?"))
                    .bind(a)
                    .execute(&mut *self.c)
                    .await?
            }
            (None, None) => return Ok(0),
        };
        Ok(r.rows_affected())
    }

    /// Sessions unused since `idle_before` or created before `max_before`.
    pub async fn expire_admin_sessions(
        &mut self,
        idle_before: i64,
        max_before: i64,
    ) -> ApiResult<u64> {
        Ok(sqlx::query(
            self.d
                .q("DELETE FROM admin_sessions WHERE last_seen < ? OR created_at < ?"),
        )
        .bind(idle_before)
        .bind(max_before)
        .execute(&mut *self.c)
        .await?
        .rows_affected())
    }

    // ---- admin keys

    pub async fn insert_admin_key(&mut self, k: &AdminKeyRow) -> ApiResult<()> {
        sqlx::query(self.d.q(
            "INSERT INTO admin_keys (key_id, hash, name, role, created_at, revoked_at) VALUES (?, ?, ?, ?, ?, ?)",
        ))
        .bind(&k.key_id)
        .bind(&k.hash)
        .bind(&k.name)
        .bind(&k.role)
        .bind(k.created_at)
        .bind(k.revoked_at)
        .execute(&mut *self.c)
        .await?;
        Ok(())
    }

    pub async fn admin_keys(&mut self) -> ApiResult<Vec<AdminKeyRow>> {
        let rows = sqlx::query(
            "SELECT key_id, hash, name, role, created_at, revoked_at FROM admin_keys ORDER BY created_at, key_id",
        )
        .fetch_all(&mut *self.c)
        .await?;
        rows.iter()
            .map(|r| {
                Ok(AdminKeyRow {
                    key_id: r.try_get(0)?,
                    hash: r.try_get(1)?,
                    name: r.try_get(2)?,
                    role: r.try_get(3)?,
                    created_at: r.try_get(4)?,
                    revoked_at: r.try_get(5)?,
                })
            })
            .collect::<sqlx::Result<_>>()
            .map_err(Into::into)
    }

    pub async fn revoke_admin_key(&mut self, key_id: &str) -> ApiResult<bool> {
        Ok(sqlx::query(
            self.d
                .q("UPDATE admin_keys SET revoked_at = ? WHERE key_id = ? AND revoked_at IS NULL"),
        )
        .bind(now_ms())
        .bind(key_id)
        .execute(&mut *self.c)
        .await?
        .rows_affected()
            > 0)
    }

    // ---- app settings

    pub async fn app_setting(&mut self, tenant: &str, k: &str) -> ApiResult<Option<Value>> {
        let r = sqlx::query(
            self.d
                .q("SELECT v FROM app_settings WHERE tenant_id = ? AND k = ?"),
        )
        .bind(tenant)
        .bind(k)
        .fetch_optional(&mut *self.c)
        .await?;
        Ok(r.and_then(|r| r.try_get::<String, _>(0).ok())
            .and_then(|v| serde_json::from_str(&v).ok()))
    }

    pub async fn set_app_setting(&mut self, tenant: &str, k: &str, v: &Value) -> ApiResult<()> {
        sqlx::query(
            self.d
                .q("DELETE FROM app_settings WHERE tenant_id = ? AND k = ?"),
        )
        .bind(tenant)
        .bind(k)
        .execute(&mut *self.c)
        .await?;
        sqlx::query(
            self.d
                .q("INSERT INTO app_settings (tenant_id, k, v, updated_at) VALUES (?, ?, ?, ?)"),
        )
        .bind(tenant)
        .bind(k)
        .bind(v.to_string())
        .bind(now_ms())
        .execute(&mut *self.c)
        .await?;
        Ok(())
    }

    // ---- tenants (docs/en/design/multi-tenancy.md)

    pub async fn tenants_all(&mut self) -> ApiResult<Vec<TenantRow>> {
        let rows = sqlx::query(self.d.q(
            "SELECT id, name, limits, suspended_at, created_at, cluster FROM tenants ORDER BY id",
        ))
        .fetch_all(&mut *self.c)
        .await?;
        let mut out = vec![];
        for r in rows {
            out.push(TenantRow {
                id: r.try_get(0)?,
                name: r.try_get(1)?,
                limits: serde_json::from_str(&r.try_get::<String, _>(2)?).unwrap_or(Value::Null),
                suspended_at: r.try_get(3)?,
                created_at: r.try_get(4)?,
                cluster: r.try_get(5)?,
            });
        }
        Ok(out)
    }

    /// Creates a tenant; false when the id is taken.
    pub async fn create_tenant(&mut self, t: &TenantRow) -> ApiResult<bool> {
        let taken = sqlx::query(self.d.q("SELECT 1 FROM tenants WHERE id = ?"))
            .bind(&t.id)
            .fetch_optional(&mut *self.c)
            .await?
            .is_some();
        if taken {
            return Ok(false);
        }
        sqlx::query(self.d.q(
            "INSERT INTO tenants (id, name, limits, suspended_at, created_at, cluster) VALUES (?, ?, ?, ?, ?, ?)",
        ))
        .bind(&t.id)
        .bind(&t.name)
        .bind(t.limits.to_string())
        .bind(t.suspended_at)
        .bind(t.created_at)
        .bind(t.cluster.clone())
        .execute(&mut *self.c)
        .await?;
        Ok(true)
    }

    /// Changes a tenant's name, limits or suspension; false when unknown.
    pub async fn update_tenant(
        &mut self,
        id: &str,
        name: Option<&str>,
        limits: Option<&Value>,
        suspended_at: Option<Option<i64>>,
    ) -> ApiResult<bool> {
        let known = sqlx::query(self.d.q("SELECT 1 FROM tenants WHERE id = ?"))
            .bind(id)
            .fetch_optional(&mut *self.c)
            .await?
            .is_some();
        if !known {
            return Ok(false);
        }
        if let Some(n) = name {
            sqlx::query(self.d.q("UPDATE tenants SET name = ? WHERE id = ?"))
                .bind(n)
                .bind(id)
                .execute(&mut *self.c)
                .await?;
        }
        if let Some(l) = limits {
            sqlx::query(self.d.q("UPDATE tenants SET limits = ? WHERE id = ?"))
                .bind(l.to_string())
                .bind(id)
                .execute(&mut *self.c)
                .await?;
        }
        if let Some(at) = suspended_at {
            sqlx::query(self.d.q("UPDATE tenants SET suspended_at = ? WHERE id = ?"))
                .bind(at)
                .bind(id)
                .execute(&mut *self.c)
                .await?;
        }
        Ok(true)
    }

    // ---- tenant usage (docs/en/design/multi-tenancy.md; control database)

    /// People of a tenant with a device seen since `since_ms` (the month's
    /// active users so far; agents do not count).
    pub async fn tenant_mau(&mut self, tenant: &str, since_ms: i64) -> ApiResult<i64> {
        let r = sqlx::query(self.d.q(
            "SELECT COUNT(DISTINCT t.user_id) FROM tokens t JOIN users u ON u.id = t.user_id WHERE u.tenant_id = ? AND u.kind = 'human' AND t.last_seen >= ?",
        ))
        .bind(tenant)
        .bind(since_ms)
        .fetch_one(&mut *self.c)
        .await?;
        Ok(r.try_get::<i64, _>(0)?)
    }

    // ---- tenant usage (any database holding conversations)

    /// Messages of a tenant's conversations in this database created in
    /// `[from, to)`: all of them and those agents sent.
    pub async fn tenant_messages(
        &mut self,
        tenant: &str,
        from: i64,
        to: i64,
    ) -> ApiResult<(i64, i64)> {
        let mut out = [0i64; 2];
        for (i, extra) in ["", " AND m.sender_type = 'agent'"].iter().enumerate() {
            let q = format!(
                "SELECT COUNT(*) FROM messages m JOIN conversations c ON c.id = m.conv_id WHERE c.tenant_id = ? AND m.created_at >= ? AND m.created_at < ?{extra}"
            );
            let r = sqlx::query(self.d.q(&q))
                .bind(tenant)
                .bind(from)
                .bind(to)
                .fetch_one(&mut *self.c)
                .await?;
            out[i] = r.try_get::<i64, _>(0)?;
        }
        Ok((out[0], out[1]))
    }

    /// Removes a tenant's row and its webhooks kept here; false when unknown.
    pub async fn delete_tenant(&mut self, id: &str) -> ApiResult<bool> {
        sqlx::query(self.d.q("DELETE FROM tenant_webhooks WHERE tenant_id = ?"))
            .bind(id)
            .execute(&mut *self.c)
            .await?;
        let r = sqlx::query(self.d.q("DELETE FROM tenants WHERE id = ?"))
            .bind(id)
            .execute(&mut *self.c)
            .await?;
        Ok(r.rows_affected() > 0)
    }

    /// Every tenant's webhooks, secrets opened.
    pub async fn tenant_webhooks_all(&mut self) -> ApiResult<Vec<crate::webhook::TenantHook>> {
        let rows = sqlx::query(self.d.q(
            "SELECT tenant_id, name, url, secret, events, max_attempts, timeout_ms, backoff_ms, enabled, created_at, updated_at FROM tenant_webhooks ORDER BY tenant_id, name",
        ))
        .fetch_all(&mut *self.c)
        .await?;
        self.preload(&rows, &["secret"]).await?;
        let mut out = vec![];
        for r in rows {
            let tenant: String = r.try_get(0)?;
            let name: String = r.try_get(1)?;
            let secret: String = r.try_get(3)?;
            let secret = self
                .cx
                .open(&format!("tenant_webhooks.secret:{tenant}:{name}"), &secret)
                .map_err(|e| ApiError::new("storage", e))?;
            out.push(crate::webhook::TenantHook {
                tenant,
                name,
                url: r.try_get(2)?,
                secret,
                events: serde_json::from_str(&r.try_get::<String, _>(4)?).unwrap_or_default(),
                max_attempts: r.try_get::<i64, _>(5)?.max(1) as u32,
                timeout_ms: r.try_get::<i64, _>(6)?.max(1) as u64,
                backoff_ms: r.try_get::<i64, _>(7)?.max(0) as u64,
                enabled: r.try_get::<i64, _>(8)? != 0,
                created_at: r.try_get(9)?,
                updated_at: r.try_get(10)?,
            });
        }
        Ok(out)
    }

    /// Creates or replaces a tenant's webhook; false when the tenant
    /// already has `max` others.
    pub async fn tenant_webhook_put(
        &mut self,
        h: &crate::webhook::TenantHook,
        max: usize,
    ) -> ApiResult<bool> {
        let rows = sqlx::query(
            self.d
                .q("SELECT name FROM tenant_webhooks WHERE tenant_id = ?"),
        )
        .bind(&h.tenant)
        .fetch_all(&mut *self.c)
        .await?;
        let names: Vec<String> = rows
            .iter()
            .map(|r| r.try_get::<String, _>(0))
            .collect::<sqlx::Result<_>>()?;
        let exists = names.contains(&h.name);
        if !exists && names.len() >= max {
            return Ok(false);
        }
        let secret = self
            .seal(
                "",
                &format!("tenant_webhooks.secret:{}:{}", h.tenant, h.name),
                &h.secret,
            )
            .await?;
        let events = serde_json::to_string(&h.events).unwrap_or_else(|_| "[]".into());
        if exists {
            sqlx::query(self.d.q(
                "UPDATE tenant_webhooks SET url = ?, secret = ?, events = ?, max_attempts = ?, timeout_ms = ?, backoff_ms = ?, enabled = ?, updated_at = ? WHERE tenant_id = ? AND name = ?",
            ))
            .bind(&h.url)
            .bind(secret)
            .bind(events)
            .bind(h.max_attempts as i64)
            .bind(h.timeout_ms as i64)
            .bind(h.backoff_ms as i64)
            .bind(h.enabled as i64)
            .bind(h.updated_at)
            .bind(&h.tenant)
            .bind(&h.name)
            .execute(&mut *self.c)
            .await?;
        } else {
            sqlx::query(self.d.q(
                "INSERT INTO tenant_webhooks (tenant_id, name, url, secret, events, max_attempts, timeout_ms, backoff_ms, enabled, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            ))
            .bind(&h.tenant)
            .bind(&h.name)
            .bind(&h.url)
            .bind(secret)
            .bind(events)
            .bind(h.max_attempts as i64)
            .bind(h.timeout_ms as i64)
            .bind(h.backoff_ms as i64)
            .bind(h.enabled as i64)
            .bind(h.created_at)
            .bind(h.updated_at)
            .execute(&mut *self.c)
            .await?;
        }
        Ok(true)
    }

    /// Removes a tenant's webhook; false when there is none.
    pub async fn tenant_webhook_delete(&mut self, tenant: &str, name: &str) -> ApiResult<bool> {
        let r = sqlx::query(
            self.d
                .q("DELETE FROM tenant_webhooks WHERE tenant_id = ? AND name = ?"),
        )
        .bind(tenant)
        .bind(name)
        .execute(&mut *self.c)
        .await?;
        Ok(r.rows_affected() > 0)
    }

    /// Turns a tenant's webhook off (it kept failing, or its receiver asked).
    pub async fn tenant_webhook_disable(
        &mut self,
        tenant: &str,
        name: &str,
        at: i64,
    ) -> ApiResult<bool> {
        let r = sqlx::query(self.d.q(
            "UPDATE tenant_webhooks SET enabled = 0, updated_at = ? WHERE tenant_id = ? AND name = ? AND enabled <> 0",
        ))
        .bind(at)
        .bind(tenant)
        .bind(name)
        .execute(&mut *self.c)
        .await?;
        Ok(r.rows_affected() > 0)
    }

    pub async fn tenant_storage(&mut self, tenant: &str) -> ApiResult<i64> {
        let rows = sqlx::query(self.d.q(
            "SELECT f.size, f.thumb_size FROM files f JOIN conversations c ON c.id = f.conv_id WHERE c.tenant_id = ? AND f.state = 'ready'",
        ))
        .bind(tenant)
        .fetch_all(&mut *self.c)
        .await?;
        let mut n = 0i64;
        for r in rows {
            n += r.try_get::<i64, _>(0)? + r.try_get::<i64, _>(1)?;
        }
        Ok(n)
    }

    /// Whether a person had a device seen since `since_ms` (already one of
    /// this month's active people).
    pub async fn user_seen_since(&mut self, user_id: &str, since_ms: i64) -> ApiResult<bool> {
        Ok(sqlx::query(
            self.d
                .q("SELECT 1 FROM tokens WHERE user_id = ? AND last_seen >= ?"),
        )
        .bind(user_id)
        .bind(since_ms)
        .fetch_optional(&mut *self.c)
        .await?
        .is_some())
    }
}
