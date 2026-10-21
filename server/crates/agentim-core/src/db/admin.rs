//! Console accounts, sessions, admin keys and app settings (docs/12 §4).

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
}
