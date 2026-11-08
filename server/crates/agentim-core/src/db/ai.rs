//! The app's own model services (docs/en/design/assistants.md): the key is sealed with the
//! master key and only opened to call the provider.

use sqlx::Row;

use super::Db;
use crate::model::*;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ServiceRow {
    pub name: String,
    pub provider: String,
    pub base_url: String,
    /// Empty in lists; opened by `model_service`. Never returned by an API.
    pub api_key: String,
    pub key_hint: String,
    pub daily_tokens: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

fn aad(name: &str) -> String {
    format!("model_services.api_key:{name}")
}

impl Db<'_> {
    pub async fn model_services(&mut self) -> ApiResult<Vec<ServiceRow>> {
        let rows = sqlx::query(self.d.q(
            "SELECT name, provider, base_url, key_hint, daily_tokens, created_at, updated_at FROM model_services ORDER BY name",
        ))
        .fetch_all(&mut *self.c)
        .await?;
        rows.iter()
            .map(|r| {
                Ok(ServiceRow {
                    name: r.try_get(0)?,
                    provider: r.try_get(1)?,
                    base_url: r.try_get(2)?,
                    api_key: String::new(),
                    key_hint: r.try_get(3)?,
                    daily_tokens: r.try_get(4)?,
                    created_at: r.try_get(5)?,
                    updated_at: r.try_get(6)?,
                })
            })
            .collect()
    }

    /// One service with its key opened.
    pub async fn model_service(&mut self, name: &str) -> ApiResult<Option<ServiceRow>> {
        let rows = sqlx::query(self.d.q(
            "SELECT name, provider, base_url, api_key, key_hint, daily_tokens, created_at, updated_at FROM model_services WHERE name = ?",
        ))
        .bind(name)
        .fetch_all(&mut *self.c)
        .await?;
        self.preload(&rows, &["api_key"]).await?;
        let Some(r) = rows.first() else {
            return Ok(None);
        };
        let sealed: String = r.try_get(3)?;
        let api_key = self
            .cx
            .open(&aad(name), &sealed)
            .map_err(|e| ApiError::new("storage", e))?;
        Ok(Some(ServiceRow {
            name: r.try_get(0)?,
            provider: r.try_get(1)?,
            base_url: r.try_get(2)?,
            api_key,
            key_hint: r.try_get(4)?,
            daily_tokens: r.try_get(5)?,
            created_at: r.try_get(6)?,
            updated_at: r.try_get(7)?,
        }))
    }

    /// Creates or replaces a service; `true` when it is new. `max`: how
    /// many an app may have (a new one past it is refused with `false`
    /// and nothing written).
    pub async fn model_service_put(
        &mut self,
        s: &ServiceRow,
        max: usize,
    ) -> ApiResult<Option<bool>> {
        let have: i64 = sqlx::query(self.d.q("SELECT COUNT(*) FROM model_services"))
            .fetch_one(&mut *self.c)
            .await?
            .try_get(0)?;
        let exists = sqlx::query(self.d.q("SELECT 1 FROM model_services WHERE name = ?"))
            .bind(&s.name)
            .fetch_optional(&mut *self.c)
            .await?
            .is_some();
        if !exists && have as usize >= max {
            return Ok(None);
        }
        let key = self.seal("", &aad(&s.name), &s.api_key).await?;
        if exists {
            sqlx::query(self.d.q(
                "UPDATE model_services SET provider = ?, base_url = ?, api_key = ?, key_hint = ?, daily_tokens = ?, updated_at = ? WHERE name = ?",
            ))
            .bind(&s.provider)
            .bind(&s.base_url)
            .bind(key)
            .bind(&s.key_hint)
            .bind(s.daily_tokens)
            .bind(s.updated_at)
            .bind(&s.name)
            .execute(&mut *self.c)
            .await?;
        } else {
            sqlx::query(self.d.q(
                "INSERT INTO model_services (name, provider, base_url, api_key, key_hint, daily_tokens, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            ))
            .bind(&s.name)
            .bind(&s.provider)
            .bind(&s.base_url)
            .bind(key)
            .bind(&s.key_hint)
            .bind(s.daily_tokens)
            .bind(s.created_at)
            .bind(s.updated_at)
            .execute(&mut *self.c)
            .await?;
        }
        Ok(Some(!exists))
    }

    pub async fn model_service_delete(&mut self, name: &str) -> ApiResult<bool> {
        let r = sqlx::query(self.d.q("DELETE FROM model_services WHERE name = ?"))
            .bind(name)
            .execute(&mut *self.c)
            .await?;
        Ok(r.rows_affected() > 0)
    }
}

/// An agent the server answers for (docs/en/design/assistants.md).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AssistantRow {
    pub agent_id: String,
    pub service: String,
    pub model: String,
    /// JSON object (`hub::assistants::Settings`).
    pub settings: String,
    pub created_at: i64,
    pub updated_at: i64,
}

/// One model call, as recorded.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct UsageRow {
    pub id: String,
    pub at: i64,
    pub day: String,
    pub agent_id: String,
    pub service: String,
    pub model: String,
    pub builtin: bool,
    pub input: i64,
    pub output: i64,
    pub cache_read: i64,
    pub cache_write: i64,
    pub cost_nanos: i64,
    pub conv_id: String,
    pub msg_id: String,
}

impl Db<'_> {
    pub async fn assistants(&mut self) -> ApiResult<Vec<AssistantRow>> {
        let rows = sqlx::query(self.d.q(
            "SELECT agent_id, service, model, settings, created_at, updated_at FROM assistants ORDER BY agent_id",
        ))
        .fetch_all(&mut *self.c)
        .await?;
        rows.iter().map(assistant_row).collect()
    }

    pub async fn assistant(&mut self, agent_id: &str) -> ApiResult<Option<AssistantRow>> {
        let r = sqlx::query(self.d.q(
            "SELECT agent_id, service, model, settings, created_at, updated_at FROM assistants WHERE agent_id = ?",
        ))
        .bind(agent_id)
        .fetch_optional(&mut *self.c)
        .await?;
        r.as_ref().map(assistant_row).transpose()
    }

    /// Creates or replaces; `true` when new.
    pub async fn assistant_put(&mut self, a: &AssistantRow) -> ApiResult<bool> {
        let old = self.assistant(&a.agent_id).await?;
        if old.is_some() {
            sqlx::query(self.d.q(
                "UPDATE assistants SET service = ?, model = ?, settings = ?, updated_at = ? WHERE agent_id = ?",
            ))
            .bind(&a.service)
            .bind(&a.model)
            .bind(&a.settings)
            .bind(a.updated_at)
            .bind(&a.agent_id)
            .execute(&mut *self.c)
            .await?;
        } else {
            sqlx::query(self.d.q(
                "INSERT INTO assistants (agent_id, service, model, settings, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?)",
            ))
            .bind(&a.agent_id)
            .bind(&a.service)
            .bind(&a.model)
            .bind(&a.settings)
            .bind(a.created_at)
            .bind(a.updated_at)
            .execute(&mut *self.c)
            .await?;
        }
        Ok(old.is_none())
    }

    pub async fn assistant_delete(&mut self, agent_id: &str) -> ApiResult<bool> {
        let r = sqlx::query(self.d.q("DELETE FROM assistants WHERE agent_id = ?"))
            .bind(agent_id)
            .execute(&mut *self.c)
            .await?;
        Ok(r.rows_affected() > 0)
    }

    /// How many services use this one (assistants naming it).
    pub async fn assistants_on(&mut self, service: &str) -> ApiResult<Vec<String>> {
        let rows = sqlx::query(
            self.d
                .q("SELECT agent_id FROM assistants WHERE service = ? ORDER BY agent_id"),
        )
        .bind(service)
        .fetch_all(&mut *self.c)
        .await?;
        rows.iter().map(|r| Ok(r.try_get(0)?)).collect()
    }

    pub async fn usage_put(&mut self, u: &UsageRow) -> ApiResult<()> {
        sqlx::query(self.d.q(
            "INSERT INTO ai_usage (id, at, day, agent_id, service, model, builtin, input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, cost_nanos, conv_id, msg_id) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        ))
        .bind(&u.id)
        .bind(u.at)
        .bind(&u.day)
        .bind(&u.agent_id)
        .bind(&u.service)
        .bind(&u.model)
        .bind(u.builtin as i64)
        .bind(u.input)
        .bind(u.output)
        .bind(u.cache_read)
        .bind(u.cache_write)
        .bind(u.cost_nanos)
        .bind(&u.conv_id)
        .bind(&u.msg_id)
        .execute(&mut *self.c)
        .await?;
        Ok(())
    }

    /// Tokens a service used on a day.
    pub async fn service_tokens(&mut self, day: &str, service: &str) -> ApiResult<i64> {
        let sql = format!(
            "SELECT {} FROM ai_usage WHERE day = ? AND service = ?",
            sum(
                self.d,
                "input_tokens + output_tokens + cache_read_tokens + cache_write_tokens"
            )
        );
        let r = sqlx::query(self.d.q(&sql))
            .bind(day)
            .bind(service)
            .fetch_one(&mut *self.c)
            .await?;
        int(&r, 0)
    }

    /// Replies an agent made on a day (calls that answered a message).
    pub async fn agent_replies(&mut self, day: &str, agent_id: &str) -> ApiResult<i64> {
        let r = sqlx::query(
            self.d
                .q("SELECT COUNT(DISTINCT msg_id) FROM ai_usage WHERE day = ? AND agent_id = ?"),
        )
        .bind(day)
        .bind(agent_id)
        .fetch_one(&mut *self.c)
        .await?;
        int(&r, 0)
    }

    /// Calls from `since` on, oldest first, at most `limit`; for usage
    /// reports and billing.
    pub async fn usage_since(&mut self, since: i64, limit: i64) -> ApiResult<Vec<UsageRow>> {
        let rows = sqlx::query(self.d.q(
            "SELECT id, at, day, agent_id, service, model, builtin, input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, cost_nanos, conv_id, msg_id FROM ai_usage WHERE at >= ? ORDER BY at, id LIMIT ?",
        ))
        .bind(since)
        .bind(limit)
        .fetch_all(&mut *self.c)
        .await?;
        rows.iter()
            .map(|r| {
                Ok(UsageRow {
                    id: r.try_get(0)?,
                    at: int(r, 1)?,
                    day: r.try_get(2)?,
                    agent_id: r.try_get(3)?,
                    service: r.try_get(4)?,
                    model: r.try_get(5)?,
                    builtin: int(r, 6)? != 0,
                    input: int(r, 7)?,
                    output: int(r, 8)?,
                    cache_read: int(r, 9)?,
                    cache_write: int(r, 10)?,
                    cost_nanos: int(r, 11)?,
                    conv_id: r.try_get(12)?,
                    msg_id: r.try_get(13)?,
                })
            })
            .collect()
    }

    /// Cost of built-in calls from `since` on (nano-dollars).
    pub async fn builtin_cost_since(&mut self, since: i64) -> ApiResult<i64> {
        let sql = format!(
            "SELECT {} FROM ai_usage WHERE builtin = 1 AND at >= ?",
            sum(self.d, "cost_nanos")
        );
        let r = sqlx::query(self.d.q(&sql))
            .bind(since)
            .fetch_one(&mut *self.c)
            .await?;
        int(&r, 0)
    }
}

/// `SUM(expr)` as a 64-bit integer on every database (MySQL's SUM is a
/// decimal otherwise).
fn sum(d: crate::storage::Dialect, expr: &str) -> String {
    let ty = if d == crate::storage::Dialect::Mysql {
        "SIGNED"
    } else {
        "BIGINT"
    };
    format!("CAST(COALESCE(SUM({expr}), 0) AS {ty})")
}

/// An integer column: counts come back as different types per database.
fn int(r: &sqlx::any::AnyRow, i: usize) -> ApiResult<i64> {
    if let Ok(v) = r.try_get::<i64, _>(i) {
        return Ok(v);
    }
    if let Ok(v) = r.try_get::<f64, _>(i) {
        return Ok(v as i64);
    }
    let s: String = r.try_get(i)?;
    s.split('.')
        .next()
        .unwrap_or("0")
        .parse()
        .map_err(|_| ApiError::new("storage", "not a number"))
}

fn assistant_row(r: &sqlx::any::AnyRow) -> ApiResult<AssistantRow> {
    Ok(AssistantRow {
        agent_id: r.try_get(0)?,
        service: r.try_get(1)?,
        model: r.try_get(2)?,
        settings: r.try_get(3)?,
        created_at: r.try_get(4)?,
        updated_at: r.try_get(5)?,
    })
}

/// One knowledge file of an assistant: its text (opened), not the file.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DocRow {
    pub agent_id: String,
    pub name: String,
    pub bytes: i64,
    pub chars: i64,
    /// Empty in lists.
    pub body: String,
    pub created_at: i64,
}

fn doc_aad(agent_id: &str, name: &str) -> String {
    format!("knowledge_docs.body:{agent_id}:{name}")
}

impl Db<'_> {
    /// An assistant's files; with `bodies`, their text opened.
    pub async fn knowledge(&mut self, agent_id: &str, bodies: bool) -> ApiResult<Vec<DocRow>> {
        let rows = sqlx::query(self.d.q(
            "SELECT agent_id, name, bytes, chars, body, created_at FROM knowledge_docs WHERE agent_id = ? ORDER BY name",
        ))
        .bind(agent_id)
        .fetch_all(&mut *self.c)
        .await?;
        if bodies {
            self.preload(&rows, &["body"]).await?;
        }
        let mut out = vec![];
        for r in &rows {
            let name: String = r.try_get(1)?;
            let body = if bodies {
                let sealed: String = r.try_get(4)?;
                self.cx
                    .open(&doc_aad(agent_id, &name), &sealed)
                    .map_err(|e| ApiError::new("storage", e))?
            } else {
                String::new()
            };
            out.push(DocRow {
                agent_id: r.try_get(0)?,
                name,
                bytes: r.try_get(2)?,
                chars: r.try_get(3)?,
                body,
                created_at: r.try_get(5)?,
            });
        }
        Ok(out)
    }

    /// Adds or replaces a file; `Err("files")` (nothing written) when a
    /// new one would pass `max_files`, `Err("chars")` when the text would
    /// pass `max_chars` in all.
    pub async fn knowledge_put(
        &mut self,
        d: &DocRow,
        max_files: usize,
        max_chars: i64,
    ) -> ApiResult<Result<bool, &'static str>> {
        let have = self.knowledge(&d.agent_id, false).await?;
        let old = have.iter().find(|x| x.name == d.name);
        if old.is_none() && have.len() >= max_files {
            return Ok(Err("files"));
        }
        let total: i64 = have
            .iter()
            .filter(|x| x.name != d.name)
            .map(|x| x.chars)
            .sum::<i64>()
            + d.chars;
        if total > max_chars {
            return Ok(Err("chars"));
        }
        let body = self
            .seal("", &doc_aad(&d.agent_id, &d.name), &d.body)
            .await?;
        if old.is_some() {
            sqlx::query(self.d.q(
                "UPDATE knowledge_docs SET bytes = ?, chars = ?, body = ?, created_at = ? WHERE agent_id = ? AND name = ?",
            ))
            .bind(d.bytes)
            .bind(d.chars)
            .bind(body)
            .bind(d.created_at)
            .bind(&d.agent_id)
            .bind(&d.name)
            .execute(&mut *self.c)
            .await?;
        } else {
            sqlx::query(self.d.q(
                "INSERT INTO knowledge_docs (agent_id, name, bytes, chars, body, created_at) VALUES (?, ?, ?, ?, ?, ?)",
            ))
            .bind(&d.agent_id)
            .bind(&d.name)
            .bind(d.bytes)
            .bind(d.chars)
            .bind(body)
            .bind(d.created_at)
            .execute(&mut *self.c)
            .await?;
        }
        Ok(Ok(old.is_none()))
    }

    pub async fn knowledge_delete(&mut self, agent_id: &str, name: &str) -> ApiResult<bool> {
        let r = sqlx::query(
            self.d
                .q("DELETE FROM knowledge_docs WHERE agent_id = ? AND name = ?"),
        )
        .bind(agent_id)
        .bind(name)
        .execute(&mut *self.c)
        .await?;
        Ok(r.rows_affected() > 0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SkillRow {
    pub name: String,
    pub description: String,
    pub markdown: String,
    pub version: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

impl Db<'_> {
    pub async fn skills(&mut self) -> ApiResult<Vec<SkillRow>> {
        let rows = sqlx::query(self.d.q(
            "SELECT name, description, markdown, version, created_at, updated_at FROM skills ORDER BY name",
        ))
        .fetch_all(&mut *self.c)
        .await?;
        rows.iter().map(skill_row).collect()
    }

    pub async fn skill(&mut self, name: &str) -> ApiResult<Option<SkillRow>> {
        let r = sqlx::query(self.d.q(
            "SELECT name, description, markdown, version, created_at, updated_at FROM skills WHERE name = ?",
        ))
        .bind(name)
        .fetch_optional(&mut *self.c)
        .await?;
        r.as_ref().map(skill_row).transpose()
    }

    /// Creates or replaces; its new version.
    pub async fn skill_put(
        &mut self,
        name: &str,
        description: &str,
        markdown: &str,
        now: i64,
    ) -> ApiResult<i64> {
        match self.skill(name).await? {
            Some(old) => {
                sqlx::query(self.d.q(
                    "UPDATE skills SET description = ?, markdown = ?, version = ?, updated_at = ? WHERE name = ?",
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
                    "INSERT INTO skills (name, description, markdown, version, created_at, updated_at) VALUES (?, ?, ?, 1, ?, ?)",
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

    /// Removes a skill and its files.
    pub async fn skill_delete(&mut self, name: &str) -> ApiResult<bool> {
        sqlx::query(self.d.q("DELETE FROM skill_files WHERE skill = ?"))
            .bind(name)
            .execute(&mut *self.c)
            .await?;
        let r = sqlx::query(self.d.q("DELETE FROM skills WHERE name = ?"))
            .bind(name)
            .execute(&mut *self.c)
            .await?;
        Ok(r.rows_affected() > 0)
    }

    /// A skill's files: (path, body) with bodies, or (path, "").
    pub async fn skill_files(
        &mut self,
        skill: &str,
        bodies: bool,
    ) -> ApiResult<Vec<(String, String)>> {
        let rows = sqlx::query(
            self.d
                .q("SELECT path, body FROM skill_files WHERE skill = ? ORDER BY path"),
        )
        .bind(skill)
        .fetch_all(&mut *self.c)
        .await?;
        rows.iter()
            .map(|r| {
                let body: String = if bodies { r.try_get(1)? } else { String::new() };
                Ok((r.try_get(0)?, body))
            })
            .collect()
    }

    /// Adds or replaces a file; `None` when a new one would pass `max`.
    pub async fn skill_file_put(
        &mut self,
        skill: &str,
        path: &str,
        body: &str,
        now: i64,
        max: usize,
    ) -> ApiResult<Option<bool>> {
        let have = self.skill_files(skill, false).await?;
        let exists = have.iter().any(|(p, _)| p == path);
        if !exists && have.len() >= max {
            return Ok(None);
        }
        if exists {
            sqlx::query(
                self.d.q(
                    "UPDATE skill_files SET body = ?, updated_at = ? WHERE skill = ? AND path = ?",
                ),
            )
            .bind(body)
            .bind(now)
            .bind(skill)
            .bind(path)
            .execute(&mut *self.c)
            .await?;
        } else {
            sqlx::query(
                self.d.q(
                    "INSERT INTO skill_files (skill, path, body, updated_at) VALUES (?, ?, ?, ?)",
                ),
            )
            .bind(skill)
            .bind(path)
            .bind(body)
            .bind(now)
            .execute(&mut *self.c)
            .await?;
        }
        // The skill's version changes with its files.
        sqlx::query(
            self.d
                .q("UPDATE skills SET version = version + 1, updated_at = ? WHERE name = ?"),
        )
        .bind(now)
        .bind(skill)
        .execute(&mut *self.c)
        .await?;
        Ok(Some(!exists))
    }

    pub async fn skill_file_delete(
        &mut self,
        skill: &str,
        path: &str,
        now: i64,
    ) -> ApiResult<bool> {
        let r = sqlx::query(
            self.d
                .q("DELETE FROM skill_files WHERE skill = ? AND path = ?"),
        )
        .bind(skill)
        .bind(path)
        .execute(&mut *self.c)
        .await?;
        if r.rows_affected() > 0 {
            sqlx::query(
                self.d
                    .q("UPDATE skills SET version = version + 1, updated_at = ? WHERE name = ?"),
            )
            .bind(now)
            .bind(skill)
            .execute(&mut *self.c)
            .await?;
        }
        Ok(r.rows_affected() > 0)
    }
}

fn skill_row(r: &sqlx::any::AnyRow) -> ApiResult<SkillRow> {
    Ok(SkillRow {
        name: r.try_get(0)?,
        description: r.try_get(1)?,
        markdown: r.try_get(2)?,
        version: r.try_get(3)?,
        created_at: r.try_get(4)?,
        updated_at: r.try_get(5)?,
    })
}

/// A connection (docs/en/design/assistants.md): the secret opened only by `connection`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ConnRow {
    pub name: String,
    /// `mcp` or `api`.
    pub kind: String,
    pub url: String,
    pub auth_header: String,
    pub secret: String,
    /// The OpenAPI document (JSON) of an API.
    pub spec: String,
    /// `read_only`, `confirm` or `allow`.
    pub writes: String,
    pub created_at: i64,
    pub updated_at: i64,
    /// Approval rules by operation and amount (JSON list, docs/en/design/assistants.md).
    pub approvals: String,
}

fn conn_aad(name: &str) -> String {
    format!("connections.secret:{name}")
}

impl Db<'_> {
    /// Every connection; with `secrets`, their secrets opened.
    pub async fn connections(&mut self, secrets: bool) -> ApiResult<Vec<ConnRow>> {
        let rows = sqlx::query(self.d.q(
            "SELECT name, kind, url, auth_header, secret, spec, writes, created_at, updated_at, approvals FROM connections ORDER BY name",
        ))
        .fetch_all(&mut *self.c)
        .await?;
        if secrets {
            self.preload(&rows, &["secret"]).await?;
        }
        let mut out = vec![];
        for r in &rows {
            let name: String = r.try_get(0)?;
            let secret = if secrets {
                let sealed: String = r.try_get(4)?;
                self.cx
                    .open(&conn_aad(&name), &sealed)
                    .map_err(|e| ApiError::new("storage", e))?
            } else {
                String::new()
            };
            out.push(ConnRow {
                name,
                kind: r.try_get(1)?,
                url: r.try_get(2)?,
                auth_header: r.try_get(3)?,
                secret,
                spec: r.try_get(5)?,
                writes: r.try_get(6)?,
                created_at: r.try_get(7)?,
                updated_at: r.try_get(8)?,
                approvals: r
                    .try_get::<Option<String>, _>(9)?
                    .unwrap_or_else(|| "[]".into()),
            });
        }
        Ok(out)
    }

    pub async fn connection(&mut self, name: &str) -> ApiResult<Option<ConnRow>> {
        Ok(self
            .connections(true)
            .await?
            .into_iter()
            .find(|c| c.name == name))
    }

    /// Creates or replaces; `None` when a new one would pass `max`.
    pub async fn connection_put(&mut self, c: &ConnRow, max: usize) -> ApiResult<Option<bool>> {
        let have = self.connections(false).await?;
        let exists = have.iter().any(|x| x.name == c.name);
        if !exists && have.len() >= max {
            return Ok(None);
        }
        let secret = self.seal("", &conn_aad(&c.name), &c.secret).await?;
        if exists {
            sqlx::query(self.d.q(
                "UPDATE connections SET kind = ?, url = ?, auth_header = ?, secret = ?, spec = ?, writes = ?, updated_at = ?, approvals = ? WHERE name = ?",
            ))
            .bind(&c.kind)
            .bind(&c.url)
            .bind(&c.auth_header)
            .bind(secret)
            .bind(&c.spec)
            .bind(&c.writes)
            .bind(c.updated_at)
            .bind(&c.approvals)
            .bind(&c.name)
            .execute(&mut *self.c)
            .await?;
        } else {
            sqlx::query(self.d.q(
                "INSERT INTO connections (name, kind, url, auth_header, secret, spec, writes, created_at, updated_at, approvals) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            ))
            .bind(&c.name)
            .bind(&c.kind)
            .bind(&c.url)
            .bind(&c.auth_header)
            .bind(secret)
            .bind(&c.spec)
            .bind(&c.writes)
            .bind(c.created_at)
            .bind(c.updated_at)
            .bind(&c.approvals)
            .execute(&mut *self.c)
            .await?;
        }
        Ok(Some(!exists))
    }

    pub async fn connection_delete(&mut self, name: &str) -> ApiResult<bool> {
        let r = sqlx::query(self.d.q("DELETE FROM connections WHERE name = ?"))
            .bind(name)
            .execute(&mut *self.c)
            .await?;
        Ok(r.rows_affected() > 0)
    }
}
