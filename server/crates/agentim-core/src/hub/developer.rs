//! What the developer's backend does through the open API that a person
//! does in their client (docs/13 §4.1, problems 3, 4, 8): agents it hands
//! out and stops, and silencing members of a conversation. Same business
//! rules and codes as the client paths; `by` names the API key.

use serde_json::{Value, json};

use super::{Hub, err, now_ms};
use crate::logic::Op;
use crate::model::*;

/// Longest a member may be silenced at once: a year.
pub const MAX_SILENCE_S: i64 = 365 * 86400;

impl Hub {
    /// A live agent (not destroyed): unknown ids and people are not_found.
    pub fn live_agent_any(&self, id: &str) -> ApiResult<User> {
        self.user_by_name(id)
            .or_else(|| self.inner.sh.user(id))
            .filter(|u| u.kind == UserKind::Agent && !self.is_destroyed(&u.id))
            .ok_or_else(|| err("not_found", "no such agent"))
    }

    pub async fn stop_agent_as(
        &self,
        agent: &User,
        by: &str,
        note: Option<String>,
    ) -> ApiResult<Value> {
        if note
            .as_ref()
            .is_some_and(|n| n.is_empty() || n.len() > 500 || n.contains('\0'))
        {
            return Err(err("bad_request", "reason must be 1..500 bytes"));
        }
        if self.is_stopped(&agent.id) {
            return Err(err("conflict", "the agent is already stopped"));
        }
        self.stop_agent_by(agent.clone(), by, note).await
    }

    pub async fn resume_agent_as(&self, agent: &User, by: &str) -> ApiResult<Value> {
        if !self.is_stopped(&agent.id) {
            return Err(err("conflict", "the agent is not stopped"));
        }
        self.resume_agent_by(agent.clone(), by).await
    }

    pub async fn destroy_agent_as(&self, agent: &User, by: &str) -> ApiResult<Value> {
        self.destroy_agent_now(agent, by, false).await
    }

    /// Silences `user` in a conversation for `seconds` (None lifts it).
    pub async fn silence(
        &self,
        conv_id: &str,
        user: &User,
        seconds: Option<i64>,
        by: &str,
        reason: Option<String>,
    ) -> ApiResult<Value> {
        if seconds.is_some_and(|s| !(1..=MAX_SILENCE_S).contains(&s)) {
            return Err(err("bad_request", format!("seconds is 1..{MAX_SILENCE_S}")));
        }
        if reason
            .as_ref()
            .is_some_and(|r| r.is_empty() || r.len() > 500 || r.contains('\0'))
        {
            return Err(err("bad_request", "reason must be 1..500 bytes"));
        }
        if user.kind == UserKind::System {
            return Err(err("bad_request", "the system user is not silenced"));
        }
        let until = seconds.map(|s| now_ms() + s * 1000);
        let (c, u, b) = (conv_id.to_string(), user.id.clone(), by.to_string());
        self.on_conv_op(conv_id, || Op::Silence {
            conv_id: c.clone(),
            user_id: u.clone(),
            until,
            by: b.clone(),
            reason: reason.clone(),
        })
        .await
        .map(|mut v| {
            v["user_id"] = json!(user.name);
            v
        })
    }
}

impl Hub {
    // ------------------------------------------------- client permissions

    /// Reads every person's permission overrides into memory.
    pub(crate) async fn load_permissions(&self) -> ApiResult<()> {
        let all = self
            .read(self.control(), |db| {
                Box::pin(async move { db.permissions_all().await })
            })
            .await?;
        *self.inner.sh.perms.write().unwrap() = all.into_iter().collect();
        Ok(())
    }

    /// What a person may do in their client (docs/13 §4.1).
    pub fn permissions(&self, u: &User) -> crate::config::AllowConfig {
        self.inner.sh.allowed(u)
    }

    pub fn permission_overrides(&self, user_id: &str) -> Overrides {
        self.inner
            .sh
            .perms
            .read()
            .unwrap()
            .get(user_id)
            .cloned()
            .unwrap_or_default()
    }

    /// Changes a person's overrides (`change` as in `Overrides::apply`);
    /// their connected devices are told.
    pub async fn set_permissions(&self, user: &User, change: &Value) -> ApiResult<Value> {
        if user.kind != UserKind::Human {
            return Err(err("bad_request", "permissions are a person's"));
        }
        let o = self
            .permission_overrides(&user.id)
            .apply(change)
            .map_err(|m| err("bad_request", m))?;
        self.control()
            .call(Op::SetPermissions {
                user_id: user.id.clone(),
                overrides: o.clone(),
            })
            .await?;
        let now = self.permissions(user);
        self.inner
            .sh
            .reg
            .deliver(vec![crate::registry::Out::ToUser {
                user_id: user.id.clone(),
                frame: json!({"type": "user.updated", "user_id": user.id, "permissions": now}),
            }]);
        Ok(json!({"permissions": now, "overrides": o}))
    }

    /// Refuses what the app does not let this person (or this agent's
    /// owner) do from a client.
    pub(crate) fn allowed_in_client(&self, me: &User, msg: &ClientMsg) -> ApiResult<()> {
        let a = self.permissions(me);
        let (ok, what) = match msg {
            ClientMsg::CreateConv { kind, .. } if kind == "dm" => {
                (a.create_direct, "starting direct conversations")
            }
            ClientMsg::CreateConv { .. } => (a.create_groups, "creating groups"),
            ClientMsg::AddMember { .. } => (a.add_members, "adding members"),
            ClientMsg::FindUser { .. } => (a.find_users, "looking people up"),
            // Interventions (stop, resume, approve...) stay with the owner
            // whatever this says; listing their agents too.
            ClientMsg::CreateAgent { .. }
            | ClientMsg::AgentKey { .. }
            | ClientMsg::RevokeAgentKey { .. }
            | ClientMsg::DestroyAgent { .. } => (a.create_agents, "managing your own agents"),
            _ => (true, ""),
        };
        if ok {
            Ok(())
        } else {
            Err(err(
                "forbidden",
                format!("the app does not allow this: {what}"),
            ))
        }
    }
}
