//! Task channels, claims and the floor (docs/03-agents.md §8): a task moves through
//! A2A-aligned states and stops waking agents when it ends; a request is
//! taken by exactly one claimer; a coordinated agent speaks while it holds
//! the floor.

use serde_json::{Value, json};

use super::{Ctx, err, is_manager};
use crate::model::*;

pub const TASK_STATES: &[&str] = &[
    "submitted",
    "working",
    "input_required",
    "completed",
    "failed",
    "canceled",
];
const FLOOR_TTL_DEFAULT: i64 = 300;
const FLOOR_TTL_MIN: i64 = 10;
const FLOOR_TTL_MAX: i64 = 3600;

pub fn terminal(state: &str) -> bool {
    matches!(state, "completed" | "failed" | "canceled")
}

fn bad(msg: impl Into<String>) -> ApiError {
    rule!("task.input", "bad_request", msg)
}

impl Ctx<'_, '_> {
    /// A new task channel starts `submitted`; its assignee is woken.
    pub(super) async fn task_created(
        &mut self,
        conv_id: &str,
        creator: &User,
        assignee: Option<&User>,
    ) -> ApiResult<()> {
        self.db
            .create_task(conv_id, &creator.id, assignee.map(|a| a.id.as_str()))
            .await?;
        let wake: Vec<&str> = assignee
            .filter(|a| a.kind == UserKind::Agent && a.id != creator.id)
            .map(|a| vec![a.id.as_str()])
            .unwrap_or_default();
        let ev = self
            .db
            .append_event(
                conv_id,
                "task.updated",
                Some(&creator.id),
                None,
                json!({"state": "submitted", "previous": null, "assignee": assignee.map(|a| &a.id), "by": creator.id, "wake": wake}),
            )
            .await?;
        self.emit(ev).await
    }

    /// The state of a task channel; None for other conversations.
    pub(super) async fn task_state(&mut self, conv_id: &str) -> ApiResult<Option<String>> {
        Ok(self.db.task(conv_id).await?.map(|t| t.0))
    }

    pub(super) async fn update_task(
        &mut self,
        me: &User,
        conv_id: &str,
        state: Option<String>,
        assignee: Option<String>,
        note: Option<String>,
    ) -> ApiResult<Value> {
        self.own_conv(conv_id)?;
        let mine = self.require_member(conv_id, &me.id).await?;
        let (current, current_assignee, creator) = self
            .db
            .task(conv_id)
            .await?
            .ok_or_else(|| bad("not a task channel"))?;
        if state.is_none() && assignee.is_none() {
            return Err(bad("give a state or an assignee"));
        }
        if let Some(s) = &state
            && !TASK_STATES.contains(&s.as_str())
        {
            return Err(bad(format!("state is one of {}", TASK_STATES.join(", "))));
        }
        if note
            .as_ref()
            .is_some_and(|n| n.is_empty() || n.len() > 4096 || n.contains('\0'))
        {
            return Err(bad("a note must be 1..4KB"));
        }
        let new_assignee = match &assignee {
            None => current_assignee.clone(),
            Some(k) => {
                let members = self.members(conv_id).await?;
                Some(
                    members
                        .iter()
                        .find(|m| m.user_id == *k || m.name == *k)
                        .map(|m| m.user_id.clone())
                        .ok_or_else(|| bad("the assignee must be a member of the task"))?,
                )
            }
        };
        let next = state.clone().unwrap_or(current.clone());
        let in_charge = me.id == creator || is_manager(&mine.role);
        if assignee.is_some() && !in_charge {
            return Err(rule!(
                "task.reassign_not_allowed",
                "forbidden",
                "only the creator or a manager reassigns a task",
            ));
        }
        if terminal(&current) && next != current {
            // Reopening is a person's decision.
            if me.kind != UserKind::Human {
                return Err(rule!(
                    "task.state",
                    "conflict",
                    format!("the task is {current}")
                ));
            }
            if !in_charge {
                return Err(rule!(
                    "task.reopen_not_allowed",
                    "forbidden",
                    "only the creator or a manager reopens a task",
                ));
            }
        }
        if terminal(&next)
            && next != current
            && !(in_charge || current_assignee.as_deref() == Some(me.id.as_str()))
        {
            return Err(rule!(
                "task.end_not_allowed",
                "forbidden",
                "only the creator, a manager or the assignee ends a task",
            ));
        }
        if next == current && new_assignee == current_assignee {
            return Err(rule!(
                "task.state",
                "conflict",
                format!("the task is already {current}")
            ));
        }
        self.db
            .set_task(conv_id, &next, new_assignee.as_deref())
            .await?;
        // The assignee is woken to work on it, unless it made the change.
        let mut wake: Vec<String> = vec![];
        if let Some(a) = &new_assignee
            && *a != me.id
            && !terminal(&next)
            && self
                .members(conv_id)
                .await?
                .iter()
                .any(|m| m.user_id == *a && m.kind == UserKind::Agent)
        {
            wake.push(a.clone());
        }
        let ev = self
            .db
            .append_event(
                conv_id,
                "task.updated",
                Some(&me.id),
                None,
                json!({"state": next, "previous": current, "assignee": new_assignee, "note": note, "by": me.id, "wake": wake}),
            )
            .await?;
        let seq = ev.seq;
        self.emit(ev).await?;
        if terminal(&next) {
            self.db.clear_floor(conv_id).await?;
        }
        Ok(json!({"seq": seq, "state": next, "assignee": new_assignee}))
    }

    pub(super) async fn claim(&mut self, me: &User, msg_id: &str) -> ApiResult<Value> {
        let msg = self
            .db
            .message(msg_id)
            .await?
            .ok_or_else(|| err("conv_moved", "message not in this database"))?;
        self.own_conv(&msg.conv_id)?;
        self.require_member(&msg.conv_id, &me.id).await?;
        let is_request = msg
            .envelope
            .as_ref()
            .is_some_and(|e| e["intent"] == "request");
        if !is_request || msg.state == "recalled" {
            return Err(bad("only a request (intent: request) can be claimed"));
        }
        if msg.sender_id == me.id {
            return Err(bad("one does not claim one's own request"));
        }
        if let Some(s) = self.task_state(&msg.conv_id).await?
            && terminal(&s)
        {
            return Err(rule!("task.state", "conflict", format!("the task is {s}")));
        }
        if let Some(by) = self.db.claim_of(msg_id).await? {
            return Err(rule!(
                "task.claimed_already",
                "conflict",
                if by == me.id {
                    "you already claimed it".to_string()
                } else {
                    "already claimed".to_string()
                },
            ));
        }
        self.db.put_claim(msg_id, &msg.conv_id, &me.id).await?;
        let ev = self
            .db
            .append_event(
                &msg.conv_id,
                "msg.claimed",
                Some(&me.id),
                None,
                json!({"msg_id": msg_id, "by": me.id}),
            )
            .await?;
        let seq = ev.seq;
        self.emit(ev).await?;
        Ok(json!({"seq": seq, "msg_id": msg_id}))
    }

    pub(super) async fn release_claim(&mut self, me: &User, msg_id: &str) -> ApiResult<Value> {
        let msg = self
            .db
            .message(msg_id)
            .await?
            .ok_or_else(|| err("conv_moved", "message not in this database"))?;
        self.own_conv(&msg.conv_id)?;
        self.require_member(&msg.conv_id, &me.id).await?;
        match self.db.claim_of(msg_id).await? {
            None => return Err(rule!("task.not_claimed", "not_found", "nobody claimed it")),
            Some(by) if by != me.id => {
                return Err(rule!(
                    "task.claimed_by_other",
                    "forbidden",
                    "claimed by someone else"
                ));
            }
            Some(_) => {}
        }
        self.db.drop_claim(msg_id).await?;
        let ev = self
            .db
            .append_event(
                &msg.conv_id,
                "msg.released",
                Some(&me.id),
                None,
                json!({"msg_id": msg_id, "by": me.id}),
            )
            .await?;
        let seq = ev.seq;
        self.emit(ev).await?;
        Ok(json!({"seq": seq}))
    }

    pub(super) async fn floor_grant(
        &mut self,
        me: &User,
        conv_id: &str,
        agent_id: &str,
        ttl_secs: Option<i64>,
    ) -> ApiResult<Value> {
        self.own_conv(conv_id)?;
        let mine = self.require_member(conv_id, &me.id).await?;
        if !is_manager(&mine.role) {
            return Err(rule!(
                "task.floor_not_manager",
                "forbidden",
                "only a manager gives the floor"
            ));
        }
        let ttl = ttl_secs.unwrap_or(FLOOR_TTL_DEFAULT);
        if !(FLOOR_TTL_MIN..=FLOOR_TTL_MAX).contains(&ttl) {
            return Err(bad("ttl_secs must be 10..3600"));
        }
        let members = self.members(conv_id).await?;
        let target = members
            .iter()
            .find(|m| m.user_id == agent_id || m.name == agent_id)
            .ok_or_else(|| rule!("member.unknown", "not_found", "no such member"))?;
        if target.kind != UserKind::Agent {
            return Err(bad("the floor is for agents"));
        }
        let target = target.user_id.clone();
        if let Some(s) = self.task_state(conv_id).await?
            && terminal(&s)
        {
            return Err(rule!("task.state", "conflict", format!("the task is {s}")));
        }
        let previous = self.db.floor(conv_id).await?.map(|f| f.0);
        let expires_at = now_ms() + ttl * 1000;
        self.db
            .set_floor(conv_id, &target, &me.id, expires_at)
            .await?;
        let ev = self
            .db
            .append_event(
                conv_id,
                "floor.granted",
                Some(&me.id),
                None,
                json!({"agent_id": target, "by": me.id, "expires_at": expires_at, "previous": previous, "wake": [target]}),
            )
            .await?;
        let seq = ev.seq;
        self.emit(ev).await?;
        Ok(json!({"seq": seq, "agent_id": target, "expires_at": expires_at}))
    }

    pub(super) async fn floor_release(&mut self, me: &User, conv_id: &str) -> ApiResult<Value> {
        self.own_conv(conv_id)?;
        let mine = self.require_member(conv_id, &me.id).await?;
        let Some((holder, _)) = self.db.floor(conv_id).await? else {
            return Err(rule!(
                "task.floor_free",
                "conflict",
                "nobody holds the floor"
            ));
        };
        if holder != me.id && !is_manager(&mine.role) {
            return Err(rule!(
                "task.floor_release_not_allowed",
                "forbidden",
                "only the holder or a manager releases the floor",
            ));
        }
        self.db.clear_floor(conv_id).await?;
        let ev = self
            .db
            .append_event(
                conv_id,
                "floor.released",
                Some(&me.id),
                None,
                json!({"agent_id": holder, "by": me.id}),
            )
            .await?;
        let seq = ev.seq;
        self.emit(ev).await?;
        Ok(json!({"seq": seq}))
    }

    /// May this agent speak now? Not in an ended task; a coordinated agent
    /// only with the floor, or to answer a message addressed to it.
    pub(super) async fn agent_may_speak(
        &mut self,
        agent: &User,
        conv_id: &str,
        member: &Member,
        reply_to: Option<&str>,
    ) -> ApiResult<()> {
        if let Some(s) = self.task_state(conv_id).await?
            && terminal(&s)
        {
            return Err(rule!("task.state", "conflict", format!("the task is {s}")));
        }
        if member.trigger != Trigger::Coordinated {
            return Ok(());
        }
        if self
            .db
            .floor(conv_id)
            .await?
            .is_some_and(|(holder, _)| holder == agent.id)
        {
            return Ok(());
        }
        if let Some(r) = reply_to
            && let Some(q) = self.db.message(r).await?
            && (q.mentions.contains(&agent.id)
                || super::routing::to_of(q.envelope.as_ref()).contains(&agent.id))
        {
            return Ok(());
        }
        Err(rule!(
            "task.floor_wait",
            "conflict",
            "waiting for the floor (coordinated); answer only what is addressed to you",
        ))
    }
}
