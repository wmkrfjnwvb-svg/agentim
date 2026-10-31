//! Stopping and destroying agents (docs/03-agents.md §4.3–§4.4). The flag is written in
//! the control database first and checked in every writer before an agent
//! acts, so nothing of a stopped agent commits after its `agent.state`
//! event; each conversation then records the change.

use serde_json::{Value, json};

use super::Ctx;
use crate::model::*;

impl Ctx<'_, '_> {
    /// A stopped agent does nothing until it is resumed.
    pub(super) fn agent_running(&self, me: &User) -> ApiResult<()> {
        if me.kind == UserKind::Agent && self.sh.stopped.read().unwrap().contains(&me.id) {
            return Err(rule!(
                "agent.stopped",
                "conflict",
                "your owner stopped you; wait until you are resumed",
            ));
        }
        Ok(())
    }

    pub(super) async fn set_stopped(
        &mut self,
        agent_id: &str,
        on: bool,
        by: &str,
    ) -> ApiResult<Value> {
        if !self.db.set_stopped(agent_id, on).await? {
            return Err(rule!(
                "agent.already_stopped",
                "conflict",
                if on {
                    "the agent is already stopped"
                } else {
                    "the agent is not stopped"
                },
            ));
        }
        self.db
            .audit(
                Some(by),
                if on { "agent.stopped" } else { "agent.resumed" },
                None,
                Some(agent_id),
                json!({}),
            )
            .await?;
        Ok(json!({}))
    }

    /// Ends the agent's live streams in a conversation and frees the floor
    /// it holds there.
    async fn silence(&mut self, conv_id: &str, agent: &User, why: &str, by: &str) -> ApiResult<()> {
        let ids: Vec<String> = self
            .st
            .streams
            .values()
            .filter(|s| s.sender.id == agent.id && s.conv_id == conv_id)
            .map(|s| s.msg_id.clone())
            .collect();
        for id in ids {
            self.finish_stream(&id, "cancelled", Some(why), by).await?;
        }
        if self
            .db
            .floor(conv_id)
            .await?
            .is_some_and(|(holder, _)| holder == agent.id)
        {
            self.db.clear_floor(conv_id).await?;
            let ev = self
                .db
                .append_event(
                    conv_id,
                    "floor.released",
                    Some(by),
                    None,
                    json!({"agent_id": agent.id, "by": by}),
                )
                .await?;
            self.emit(ev).await?;
        }
        Ok(())
    }

    /// Records a stop or resume in one conversation the agent is in.
    pub(super) async fn agent_stopped(
        &mut self,
        conv_id: &str,
        agent: &User,
        on: bool,
        by: &str,
        reason: Option<&str>,
    ) -> ApiResult<Value> {
        self.own_conv(conv_id)?;
        if !self
            .members(conv_id)
            .await?
            .iter()
            .any(|m| m.user_id == agent.id)
        {
            return Ok(json!({}));
        }
        if on {
            self.silence(conv_id, agent, "stopped", by).await?;
        }
        let (state, detail) = if on {
            ("stopped", "stopped by its owner")
        } else {
            ("running", "resumed by its owner")
        };
        let ev = self
            .db
            .append_event(
                conv_id,
                "agent.state",
                Some(by),
                None,
                json!({"agent_id": agent.id, "state": state, "reason": "owner", "detail": detail, "note": reason, "by": by}),
            )
            .await?;
        let seq = ev.seq;
        self.emit(ev).await?;
        Ok(json!({"seq": seq}))
    }

    pub(super) async fn destroy(&mut self, agent_id: &str, by: &str) -> ApiResult<Value> {
        if !self.db.destroy_agent(agent_id).await? {
            return Err(rule!("agent.unknown", "not_found", "the agent is gone"));
        }
        self.db
            .audit(Some(by), "agent.destroyed", None, Some(agent_id), json!({}))
            .await?;
        Ok(json!({}))
    }

    /// Takes a destroyed agent out of one conversation: its streams end,
    /// its drafts, pending requests, claims, summaries and floor go, and
    /// it leaves. Its messages stay: they are the conversation's record.
    /// Repeatable (a restart finishes what a crash interrupted).
    pub(super) async fn agent_gone(
        &mut self,
        conv_id: &str,
        agent: &User,
        by: &str,
    ) -> ApiResult<Value> {
        self.own_conv(conv_id)?;
        self.silence(conv_id, agent, "destroyed", by).await?;
        self.cancel_requests_of(conv_id, agent).await?;
        self.db.drop_agent_in_conv(conv_id, &agent.id).await?;
        if !self
            .members(conv_id)
            .await?
            .iter()
            .any(|m| m.user_id == agent.id)
        {
            return Ok(json!({}));
        }
        let ev = self
            .db
            .append_event(
                conv_id,
                "member.removed",
                Some(by),
                None,
                json!({"user_id": agent.id, "name": agent.name, "kind": agent.kind, "by": by, "reason": "destroyed"}),
            )
            .await?;
        let seq = ev.seq;
        self.emit(ev).await?;
        self.db.remove_member(conv_id, &agent.id).await?;
        self.st.members.remove(conv_id);
        self.st.recips.remove(conv_id);
        self.out.push(crate::registry::Out::Unfocus {
            conv_id: conv_id.to_string(),
            user_id: Some(agent.id.clone()),
        });
        if self.idx == self.sh.control {
            self.db.member_index_remove(conv_id, &agent.id).await?;
        }
        Ok(json!({"seq": seq}))
    }
}
