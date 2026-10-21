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
            return Err(err(
                "bad_request",
                format!("seconds is 1..{MAX_SILENCE_S}"),
            ));
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
