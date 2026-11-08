//! Agent roles and each agent's configuration (docs/en/design/agents.md): roles are
//! Markdown files in the app's library; an agent gets its configuration at
//! login (`welcome.config`), can fetch it (`get_config`, MCP, HTTP), and is
//! told `config.updated` when it changes.

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::Hub;
use crate::logic::{AdminOp, Op};
use crate::model::*;

fn role_error(rule: &str, m: String) -> ApiError {
    match rule {
        "role.name" => rule!("role.name", "bad_request", m),
        "role.size" => rule!("role.size", "bad_request", m),
        "role.name_mismatch" => rule!("role.name_mismatch", "bad_request", m),
        "role.description" => rule!("role.description", "bad_request", m),
        _ => rule!("role.header", "bad_request", m),
    }
}

fn check_name(name: &str) -> ApiResult<()> {
    if crate::role::valid_name(name) {
        Ok(())
    } else {
        Err(role_error(
            "role.name",
            "role names are 1..64 of a-z 0-9 - (not starting or ending with -)".into(),
        ))
    }
}

/// A role as an agent and the APIs see it.
fn role_view(row: &crate::db::roles::RoleRow) -> Value {
    match crate::role::parse(&row.name, &row.markdown) {
        Ok(r) => {
            let mut v = json!(r);
            v["version"] = json!(row.version);
            v["updated_at"] = json!(row.updated_at);
            v
        }
        // Stored files were checked when written; a rule change since
        // leaves the instructions as they were.
        Err(_) => json!({"name": row.name, "description": row.description, "version": row.version,
            "instructions": row.markdown, "updated_at": row.updated_at}),
    }
}

impl Hub {
    /// Creates or replaces a role from its Markdown file.
    pub async fn put_role(&self, name: &str, markdown: &str) -> ApiResult<Value> {
        let role = crate::role::parse(name, markdown).map_err(|(r, m)| role_error(r, m))?;
        self.control()
            .call(Op::Admin(AdminOp::RolePut {
                name: name.to_string(),
                description: role.description,
                markdown: markdown.to_string(),
                now: now_ms(),
            }))
            .await
    }

    /// The library: name, description, version.
    pub async fn roles(&self) -> ApiResult<Vec<Value>> {
        let rows = self
            .read(self.control(), |db| {
                Box::pin(async move { db.roles().await })
            })
            .await?;
        Ok(rows.iter().map(|r| r.summary()).collect())
    }

    /// One role: parsed, with its file.
    pub async fn role(&self, name: &str) -> ApiResult<Value> {
        check_name(name)?;
        let n = name.to_string();
        let row = self
            .read(self.control(), move |db| {
                Box::pin(async move { db.role_get(&n).await })
            })
            .await?
            .ok_or_else(|| rule!("role.unknown", "not_found", "no such role"))?;
        let mut v = role_view(&row);
        v["markdown"] = json!(row.markdown);
        Ok(v)
    }

    pub async fn delete_role(&self, name: &str) -> ApiResult<Value> {
        check_name(name)?;
        self.control()
            .call(Op::Admin(AdminOp::RoleDelete {
                name: name.to_string(),
            }))
            .await
    }

    /// Gives an agent a role (or none, `None`); for the app's developers
    /// and console.
    pub async fn set_agent_role(&self, agent_id: &str, role: Option<&str>) -> ApiResult<Value> {
        let a = self
            .inner
            .sh
            .user(agent_id)
            .filter(|u| u.kind == UserKind::Agent && !self.is_destroyed(&u.id))
            .ok_or_else(|| rule!("agent.unknown", "not_found", "no such agent"))?;
        if let Some(r) = role {
            check_name(r)?;
        }
        self.control()
            .call(Op::Admin(AdminOp::AgentRole {
                agent_id: a.id,
                role: role.map(str::to_string),
            }))
            .await
    }

    /// `set_agent_role` by the agent's owner (from their client).
    pub(crate) async fn set_agent_role_as_owner(
        &self,
        me: &User,
        agent_id: &str,
        role: Option<&str>,
    ) -> ApiResult<Value> {
        if me.kind != UserKind::Human {
            return Err(rule!(
                "role.people_assign",
                "forbidden",
                "only an agent's owner gives it a role"
            ));
        }
        let a = self.owned_agent(me, agent_id)?;
        if self.is_destroyed(&a.id) {
            return Err(rule!("agent.unknown", "not_found", "no such agent"));
        }
        let v = self.set_agent_role(&a.id, role).await?;
        if v["changed"] == true {
            self.audit_user(me, "agent.role", &a.id, json!({"role": role}))
                .await;
        }
        Ok(v)
    }

    /// Agent id → role, for the console's list.
    pub async fn agent_roles(&self) -> ApiResult<std::collections::HashMap<String, String>> {
        let v = self
            .read(self.control(), |db| {
                Box::pin(async move { db.agent_roles().await })
            })
            .await?;
        Ok(v.into_iter().collect())
    }

    /// An agent's configuration (docs/en/design/agents.md): its role, and (later) its
    /// skills, MCP servers and APIs; `version` changes with the content.
    pub async fn agent_config(&self, agent: &User) -> ApiResult<Value> {
        if agent.kind != UserKind::Agent {
            return Err(rule!(
                "config.agents_only",
                "forbidden",
                "only agents have a configuration"
            ));
        }
        let id = agent.id.clone();
        let row = self
            .read(self.control(), move |db| {
                Box::pin(async move {
                    match db.user_role(&id).await? {
                        Some(r) => db.role_get(&r).await,
                        None => Ok(None),
                    }
                })
            })
            .await?;
        let parsed = row
            .as_ref()
            .and_then(|r| crate::role::parse(&r.name, &r.markdown).ok());
        let (skill_names, mcp_names, api_names) = parsed
            .map(|p| (p.skills, p.mcp, p.apis))
            .unwrap_or_default();
        let skills = self.skills_named(&skill_names).await?;
        let (mcp, apis) = self.connections_named(&mcp_names, &api_names).await?;
        let mut v = json!({
            "role": row.as_ref().map(role_view),
            "skills": skills,
            "mcp": mcp,
            "apis": apis,
        });
        let hash = Sha256::digest(v.to_string().as_bytes());
        v["version"] = json!(hex::encode(&hash[..8]));
        Ok(v)
    }
}
