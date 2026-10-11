//! Agent registration and keys (docs/10-agent-ecosystem.md §5.1): a person
//! creates agents they own, gets a key and ready-to-paste connection
//! settings, rotates keys (moving an agent in seconds) and revokes them.

use serde_json::{Value, json};

use super::{Hub, err};
use crate::logic::Op;
use crate::model::*;

/// Keys one agent may hold at a time.
pub const MAX_AGENT_KEYS: usize = 20;
const KEY_NAME: &str = "agent key";

impl Hub {
    pub async fn create_agent(&self, me: &User, name: &str) -> ApiResult<Value> {
        if me.kind != UserKind::Human {
            return Err(err("forbidden", "only people own agents"));
        }
        if !crate::open_api::valid_user_id(name) {
            return Err(err(
                "bad_request",
                "agent names are 1..64 of A-Z a-z 0-9 _ - . @ : and do not start with _",
            ));
        }
        let (agent, key) = self
            .create_user(name, UserKind::Agent, Some(&me.id), Some(&me.unit_id))
            .await?;
        let device_id = self
            .devices(&agent.id)
            .await?
            .first()
            .map(|d| d.device_id.clone());
        self.audit_user(me, "agent.create", &agent.id, json!({"name": name}))
            .await;
        Ok(json!({
            "agent": agent,
            "key": key,
            "device_id": device_id,
            "connect": self.connect_settings(&key, None),
        }))
    }

    pub async fn list_agents(&self, me: &User) -> ApiResult<Value> {
        let mut agents: Vec<User> = self
            .inner
            .sh
            .users
            .read()
            .unwrap()
            .values()
            .filter(|u| u.kind == UserKind::Agent && u.owner_id.as_deref() == Some(&me.id))
            .cloned()
            .collect();
        agents.sort_by(|a, b| a.name.cmp(&b.name));
        let mut out = vec![];
        for a in agents {
            let keys: Vec<Value> = self
                .devices(&a.id)
                .await?
                .iter()
                .map(|d| d.view())
                .collect();
            out.push(json!({
                "agent": a,
                "deactivated": self.is_deactivated(&a.id),
                "keys": keys,
            }));
        }
        Ok(json!({"agents": out}))
    }

    pub async fn agent_key(
        &self,
        me: &User,
        agent_id: &str,
        revoke_others: bool,
    ) -> ApiResult<Value> {
        let agent = self.owned_agent(me, agent_id)?;
        if self.is_deactivated(&agent.id) {
            return Err(err("conflict", "the agent is deactivated"));
        }
        let (key, row, revoked) = self
            .issue_token_with(
                &agent.id,
                Some(KEY_NAME),
                None,
                None,
                &me.id,
                revoke_others,
                Some(MAX_AGENT_KEYS),
            )
            .await?;
        self.audit_user(
            me,
            "agent.key",
            &agent.id,
            json!({"device_id": row.device_id, "revoked": revoked}),
        )
        .await;
        Ok(json!({
            "key": key,
            "device_id": row.device_id,
            "revoked": revoked,
            "connect": self.connect_settings(&key, None),
        }))
    }

    pub async fn revoke_agent_key(
        &self,
        me: &User,
        agent_id: &str,
        device_id: &str,
    ) -> ApiResult<Value> {
        let agent = self.owned_agent(me, agent_id)?;
        if !self
            .devices(&agent.id)
            .await?
            .iter()
            .any(|d| d.device_id == device_id)
        {
            return Err(err("not_found", "no such key"));
        }
        let n = self.revoke_tokens(&agent.id, Some(device_id)).await?;
        self.audit_user(
            me,
            "agent.revoke_key",
            &agent.id,
            json!({"device_id": device_id}),
        )
        .await;
        Ok(json!({"revoked": n}))
    }

    /// Whom an agent may bring into a conversation: its person, that
    /// person's agents, and whoever already shares a conversation with it.
    pub(super) async fn agent_may_contact(
        &self,
        agent: &User,
        owner: &User,
        u: &User,
    ) -> ApiResult<()> {
        if u.id == owner.id
            || (u.kind == UserKind::Agent && u.owner_id.as_deref() == Some(owner.id.as_str()))
        {
            return Ok(());
        }
        let ids = vec![agent.id.clone(), u.id.clone()];
        let rows = self
            .read(self.control(), move |db| {
                Box::pin(async move { db.member_index_for(&ids).await })
            })
            .await?;
        let mine: std::collections::HashSet<&str> = rows
            .iter()
            .filter(|(_, who)| *who == agent.id)
            .map(|(c, _)| c.as_str())
            .collect();
        if rows
            .iter()
            .any(|(c, who)| *who == u.id && mine.contains(c.as_str()))
        {
            return Ok(());
        }
        Err(err(
            "forbidden",
            format!(
                "an agent reaches only its owner, the owner's agents and those it already shares a conversation with; ask your owner to introduce {}",
                u.name
            ),
        ))
    }

    /// The per-agent budget of new conversations; `take` spends one.
    pub(super) fn agent_conversation_allowed(&self, agent_id: &str, take: bool) -> bool {
        let n = self.inner.cfg.agents.conversations_per_hour as f64;
        let now = std::time::Instant::now();
        let mut b = self.inner.conv_buckets.lock().unwrap();
        let (tokens, at) = b.entry(agent_id.to_string()).or_insert((n, now));
        *tokens = (*tokens + now.duration_since(*at).as_secs_f64() * n / 3600.0).min(n);
        *at = now;
        if *tokens < 1.0 {
            return false;
        }
        if take {
            *tokens -= 1.0;
        }
        true
    }

    /// An agent the caller owns: unknown ids and people are `not_found`,
    /// somebody else's agent is `forbidden`.
    fn owned_agent(&self, me: &User, agent_id: &str) -> ApiResult<User> {
        let a = self
            .inner
            .sh
            .user(agent_id)
            .filter(|u| u.kind == UserKind::Agent)
            .ok_or_else(|| err("not_found", "no such agent"))?;
        if a.owner_id.as_deref() != Some(me.id.as_str()) {
            return Err(err("forbidden", "not your agent"));
        }
        Ok(a)
    }

    /// Ready-to-paste settings for a key: the MCP endpoint (with a config
    /// block MCP clients accept) and the WebSocket address. `base` overrides
    /// `server.public_url`; without either the addresses are relative.
    pub fn connect_settings(&self, key: &str, base: Option<&str>) -> Value {
        let base = base
            .map(String::from)
            .or_else(|| self.inner.cfg.server.public_url.clone())
            .unwrap_or_default();
        let base = base.trim_end_matches('/');
        let ws = if let Some(rest) = base.strip_prefix("https://") {
            format!("wss://{rest}")
        } else if let Some(rest) = base.strip_prefix("http://") {
            format!("ws://{rest}")
        } else {
            base.to_string()
        };
        let mcp = format!("{base}/mcp");
        let auth = format!("Bearer {key}");
        json!({
            "mcp_url": mcp,
            "mcp_config": {"mcpServers": {"agentim": {
                "type": "http",
                "url": mcp,
                "headers": {"Authorization": auth},
            }}},
            "websocket_url": format!("{ws}/ws?token={key}"),
        })
    }

    /// One audit record for an account action by a person.
    async fn audit_user(&self, me: &User, action: &str, target: &str, detail: Value) {
        let _ = self
            .control()
            .call(Op::Audit {
                actor: me.id.clone(),
                action: action.into(),
                conv_id: String::new(),
                target: Some(target.into()),
                detail,
            })
            .await;
    }
}
