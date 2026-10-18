//! The AI message experience (docs/03-agents.md §3): an agent
//! hands a conversation to a person and gets it back.

use serde_json::{Value, json};

use super::{Ctx, err};
use crate::model::*;

const NOTE_MAX: usize = 4096;

fn check_note(note: &str) -> ApiResult<()> {
    if note.is_empty() || note.len() > NOTE_MAX || note.contains('\0') {
        return Err(err("bad_request", "a note must be 1 byte..4KB"));
    }
    Ok(())
}

impl Ctx<'_, '_> {
    /// The agent hands the conversation to a person: it is taken over
    /// (silent, not woken) until that person or its owner hands it back.
    pub(super) async fn handoff(
        &mut self,
        me: &User,
        conv_id: &str,
        to: Option<String>,
        note: &str,
    ) -> ApiResult<Value> {
        if me.kind != UserKind::Agent {
            return Err(err("bad_request", "only agents hand off"));
        }
        self.require_member(conv_id, &me.id).await?;
        check_note(note)?;
        let members = self.members(conv_id).await?;
        let target = match &to {
            Some(t) => members
                .iter()
                .find(|m| m.user_id == *t || m.name == *t)
                .ok_or_else(|| err("bad_request", "hand off to a person in this conversation"))?,
            None => {
                let owner = me.owner_id.clone().unwrap_or_default();
                members.iter().find(|m| m.user_id == owner).ok_or_else(|| {
                    err(
                        "bad_request",
                        "your owner is not in this conversation; name a person (to)",
                    )
                })?
            }
        };
        if target.kind != UserKind::Human {
            return Err(err("bad_request", "hand off to a person, not an agent"));
        }
        let target = target.user_id.clone();
        if let Some(by) = self.db.takeover(conv_id, &me.id).await? {
            return Err(err("conflict", format!("already taken over by {by}")));
        }
        let ids: Vec<String> = self
            .st
            .streams
            .values()
            .filter(|s| s.sender.id == me.id && s.conv_id == conv_id)
            .map(|s| s.msg_id.clone())
            .collect();
        for id in ids {
            self.finish_stream(&id, "cancelled", Some("handed_off"), &me.id)
                .await?;
        }
        let ev = self
            .db
            .set_takeover_with(
                conv_id,
                &me.id,
                Some(&target),
                Some(note),
                json!({"handoff": true, "note": note, "from": me.id}),
            )
            .await?;
        let seq = ev.seq;
        self.emit(ev).await?;
        // The person is told on their own channel too (and in their inbox).
        let side = self
            .db
            .append_side(
                conv_id,
                std::slice::from_ref(&target),
                "handoff",
                Some(&me.id),
                json!({"agent_id": me.id, "note": note, "seq": seq}),
            )
            .await?;
        self.out.push(crate::registry::Out::Side(side));
        self.db
            .audit(
                Some(&me.id),
                "agent.handoff",
                Some(conv_id),
                Some(&target),
                json!({}),
            )
            .await?;
        Ok(json!({"seq": seq, "to": target}))
    }

    /// The person it was handed to, or the agent's owner, hands it back;
    /// the agent is woken with the note.
    pub(super) async fn handback(
        &mut self,
        me: &User,
        conv_id: &str,
        agent_id: &str,
        note: Option<String>,
    ) -> ApiResult<Value> {
        self.require_member(conv_id, &me.id).await?;
        if let Some(n) = &note {
            check_note(n)?;
        }
        let agent = self
            .sh
            .user(agent_id)
            .filter(|u| u.kind == UserKind::Agent)
            .ok_or_else(|| err("not_found", "no such agent"))?;
        let Some(by) = self.db.takeover(conv_id, &agent.id).await? else {
            return Err(err("conflict", "the agent is not handed off"));
        };
        if me.id != by && agent.owner_id.as_deref() != Some(me.id.as_str()) {
            return Err(err(
                "forbidden",
                "only the person it was handed to or its owner hands it back",
            ));
        }
        let ev = self
            .db
            .set_takeover_with(
                conv_id,
                &agent.id,
                None,
                None,
                json!({"handback": true, "by": me.id, "note": note, "wake": [agent.id]}),
            )
            .await?;
        let seq = ev.seq;
        self.emit(ev).await?;
        self.db
            .audit(
                Some(&me.id),
                "agent.handback",
                Some(conv_id),
                Some(&agent.id),
                json!({}),
            )
            .await?;
        Ok(json!({"seq": seq}))
    }
}

const PROGRESS_KINDS: &[&str] = &[
    "thinking",
    "tool_call",
    "tool_result",
    "plan",
    "step",
    "error",
];
const DATA_MAX: usize = 8 * 1024;
const REASON_MAX: usize = 500;

impl Ctx<'_, '_> {
    /// An agent's progress: a side event every member receives, that wakes
    /// nobody and is not in the main log.
    pub(super) async fn progress(
        &mut self,
        me: &User,
        conv_id: &str,
        msg_id: Option<String>,
        kind: &str,
        text: &str,
        data: Option<Value>,
    ) -> ApiResult<Value> {
        if me.kind != UserKind::Agent {
            return Err(err("bad_request", "progress is for agents"));
        }
        self.require_member(conv_id, &me.id).await?;
        if !PROGRESS_KINDS.contains(&kind) {
            return Err(err(
                "bad_request",
                format!("kind is one of {}", PROGRESS_KINDS.join(", ")),
            ));
        }
        if text.is_empty() || text.len() > NOTE_MAX || text.contains('\0') {
            return Err(err("bad_request", "text must be 1 byte..4KB"));
        }
        if data
            .as_ref()
            .is_some_and(|d| d.to_string().len() > DATA_MAX)
        {
            return Err(err("bad_request", "data is at most 8KB"));
        }
        if let Some(m) = &msg_id {
            match self.db.message(m).await? {
                Some(x) if x.conv_id == conv_id && x.sender_id == me.id => {}
                _ => {
                    return Err(err(
                        "bad_request",
                        "msg_id must be your own message in this conversation",
                    ));
                }
            }
        }
        {
            let now = std::time::Instant::now();
            let mut b = self.sh.progress_buckets.lock().unwrap();
            let rate = self.sh.progress_per_second;
            let (tokens, at) = b
                .entry((me.id.clone(), conv_id.to_string()))
                .or_insert((rate, now));
            *tokens = (*tokens + now.duration_since(*at).as_secs_f64() * rate).min(rate);
            *at = now;
            if *tokens < 1.0 {
                return Err(err(
                    "rate_limited",
                    format!(
                        "at most {rate} progress reports a second (agents.progress_per_second)"
                    ),
                ));
            }
            *tokens -= 1.0;
        }
        let audience: Vec<String> = self
            .members(conv_id)
            .await?
            .iter()
            .map(|m| m.user_id.clone())
            .collect();
        let side = self
            .db
            .append_side(
                conv_id,
                &audience,
                "progress",
                Some(&me.id),
                json!({"agent_id": me.id, "msg_id": msg_id, "kind": kind, "text": text, "data": data}),
            )
            .await?;
        let side_seq = side.side_seq;
        self.out.push(crate::registry::Out::Side(side));
        Ok(json!({"side_seq": side_seq}))
    }

    /// A person's thumbs up or down (or none) on an agent's message; only
    /// the agent and its owner hear of it.
    pub(super) async fn feedback(
        &mut self,
        me: &User,
        msg_id: &str,
        rating: Option<String>,
        reason: Option<String>,
    ) -> ApiResult<Value> {
        let msg = self
            .db
            .message(msg_id)
            .await?
            .ok_or_else(|| err("conv_moved", "message not in this database"))?;
        self.own_conv(&msg.conv_id)?;
        self.require_member(&msg.conv_id, &me.id).await?;
        if msg.sender_type != UserKind::Agent || msg.state == "recalled" {
            return Err(err("bad_request", "feedback is for agents' messages"));
        }
        if msg.sender_id == me.id {
            return Err(err("bad_request", "one does not rate one's own message"));
        }
        if let Some(r) = &rating
            && !matches!(r.as_str(), "up" | "down")
        {
            return Err(err("bad_request", "rating is up, down or null"));
        }
        if let Some(x) = &reason
            && (rating.is_none() || x.is_empty() || x.len() > REASON_MAX || x.contains('\0'))
        {
            return Err(err(
                "bad_request",
                "a reason (1..500 bytes) goes with a rating",
            ));
        }
        let current = self.db.feedback(msg_id, &me.id).await?;
        let wanted = rating.clone().map(|r| (r, reason.clone()));
        if current == wanted {
            return Ok(json!({"changed": false}));
        }
        self.db
            .set_feedback(
                msg_id,
                &me.id,
                &msg.conv_id,
                &msg.sender_id,
                rating.as_deref(),
                reason.as_deref(),
            )
            .await?;
        let agent = self
            .sh
            .user(&msg.sender_id)
            .ok_or_else(|| err("not_found", "the agent is gone"))?;
        let audience = self.audience(&msg.conv_id, &agent).await?;
        let side = self
            .db
            .append_side(
                &msg.conv_id,
                &audience,
                "feedback",
                Some(&me.id),
                json!({"msg_id": msg_id, "rating": rating, "reason": reason, "by": me.id}),
            )
            .await?;
        self.out.push(crate::registry::Out::Side(side));
        Ok(json!({"changed": true}))
    }
}
