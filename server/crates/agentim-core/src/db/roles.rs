//! Agent roles (docs/en/design/agents.md): stored as written; each change bumps the
//! version. An agent names its role in `users.role`.

use serde_json::{Value, json};
use sqlx::Row;

use super::Db;
use crate::model::*;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RoleRow {
    pub name: String,
    pub description: String,
    pub markdown: String,
    pub version: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

impl RoleRow {
    /// The list view (no file).
    pub fn summary(&self) -> Value {
        json!({"name": self.name, "description": self.description, "version": self.version,
            "created_at": self.created_at, "updated_at": self.updated_at})
    }
}

impl Db<'_> {
    pub async fn role_get(&mut self, name: &str) -> ApiResult<Option<RoleRow>> {
        let r = sqlx::query(self.d.q(
            "SELECT name, description, markdown, version, created_at, updated_at FROM agent_roles WHERE name = ?",
        ))
        .bind(name)
        .fetch_optional(&mut *self.c)
        .await?;
        r.map(|r| row(&r)).transpose()
    }

    pub async fn roles(&mut self) -> ApiResult<Vec<RoleRow>> {
        let rows = sqlx::query(self.d.q(
            "SELECT name, description, markdown, version, created_at, updated_at FROM agent_roles ORDER BY name",
        ))
        .fetch_all(&mut *self.c)
        .await?;
        rows.iter().map(row).collect()
    }

    /// Creates or replaces a role; its new version.
    pub async fn role_put(
        &mut self,
        name: &str,
        description: &str,
        markdown: &str,
        now: i64,
    ) -> ApiResult<i64> {
        match self.role_get(name).await? {
            Some(old) => {
                sqlx::query(self.d.q(
                    "UPDATE agent_roles SET description = ?, markdown = ?, version = ?, updated_at = ? WHERE name = ?",
                ))
                .bind(description)
                .bind(markdown)
                .bind(old.version + 1)
                .bind(now)
                .bind(name)
                .execute(&mut *self.c)
                .await?;
                Ok(old.version + 1)
            }
            None => {
                sqlx::query(self.d.q(
                    "INSERT INTO agent_roles (name, description, markdown, version, created_at, updated_at) VALUES (?, ?, ?, 1, ?, ?)",
                ))
                .bind(name)
                .bind(description)
                .bind(markdown)
                .bind(now)
                .bind(now)
                .execute(&mut *self.c)
                .await?;
                Ok(1)
            }
        }
    }

    pub async fn role_delete(&mut self, name: &str) -> ApiResult<bool> {
        let n = sqlx::query(self.d.q("DELETE FROM agent_roles WHERE name = ?"))
            .bind(name)
            .execute(&mut *self.c)
            .await?
            .rows_affected();
        Ok(n > 0)
    }

    /// Agents (not destroyed) with this role.
    pub async fn agents_with_role(&mut self, name: &str) -> ApiResult<Vec<String>> {
        let rows = sqlx::query(
            self.d
                .q("SELECT id FROM users WHERE role = ? AND destroyed_at IS NULL ORDER BY id"),
        )
        .bind(name)
        .fetch_all(&mut *self.c)
        .await?;
        rows.iter()
            .map(|r| r.try_get::<String, _>(0))
            .collect::<sqlx::Result<_>>()
            .map_err(Into::into)
    }

    /// Every agent's role (agents without one are left out).
    pub async fn agent_roles(&mut self) -> ApiResult<Vec<(String, String)>> {
        let rows = sqlx::query(
            self.d
                .q("SELECT id, role FROM users WHERE role IS NOT NULL AND destroyed_at IS NULL"),
        )
        .fetch_all(&mut *self.c)
        .await?;
        rows.iter()
            .map(|r| Ok((r.try_get::<String, _>(0)?, r.try_get::<String, _>(1)?)))
            .collect::<sqlx::Result<_>>()
            .map_err(Into::into)
    }

    pub async fn user_role(&mut self, id: &str) -> ApiResult<Option<String>> {
        let r = sqlx::query(self.d.q("SELECT role FROM users WHERE id = ?"))
            .bind(id)
            .fetch_optional(&mut *self.c)
            .await?;
        Ok(match r {
            Some(r) => r.try_get::<Option<String>, _>(0)?,
            None => None,
        })
    }

    pub async fn set_user_role(&mut self, id: &str, role: Option<&str>) -> ApiResult<()> {
        sqlx::query(self.d.q("UPDATE users SET role = ? WHERE id = ?"))
            .bind(role.map(str::to_string))
            .bind(id)
            .execute(&mut *self.c)
            .await?;
        Ok(())
    }
}

fn row(r: &sqlx::any::AnyRow) -> ApiResult<RoleRow> {
    Ok(RoleRow {
        name: r.try_get(0)?,
        description: r.try_get(1)?,
        markdown: r.try_get(2)?,
        version: r.try_get(3)?,
        created_at: r.try_get(4)?,
        updated_at: r.try_get(5)?,
    })
}
