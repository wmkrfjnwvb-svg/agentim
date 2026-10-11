//! Agent-to-agent routing (docs/09 §10): the message envelope, which agents
//! a message wakes, and the root budget that replaces the hop limit.

use serde_json::{Value, json};

use super::{Ctx, err};
use crate::model::*;

/// Agents one event may wake; more must use topics or a coordinator.
pub const MAX_WAKE: usize = 5;
/// `all` is allowed only with at most this many agents (or in a dm).
pub const MAX_AGENTS_FOR_ALL: usize = 3;
const MAX_TO: usize = 20;
const MAX_TOPICS: usize = 10;
const MAX_REFS: usize = 20;
const INTENTS: &[&str] = &["request", "proposal", "answer", "status", "done"];

/// What a sender puts on the envelope (`send`).
#[derive(Default)]
pub struct EnvelopeIn {
    pub to: Option<Vec<String>>,
    pub topics: Option<Vec<String>>,
    pub intent: Option<String>,
    pub refs: Option<Vec<String>>,
}

impl EnvelopeIn {
    fn is_empty(&self) -> bool {
        self.to.is_none() && self.topics.is_none() && self.intent.is_none() && self.refs.is_none()
    }
}

/// A topic name: 1..64 of `a-z 0-9 . _ -`.
pub fn valid_topic(t: &str) -> bool {
    !t.is_empty()
        && t.len() <= 64
        && t.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "._-".contains(c))
}

fn bad(msg: impl Into<String>) -> ApiError {
    err("bad_request", msg)
}

impl Ctx<'_, '_> {
    /// Validates an envelope; `to` names become member ids.
    pub(super) async fn envelope(
        &mut self,
        conv_id: &str,
        e: EnvelopeIn,
    ) -> ApiResult<Option<Value>> {
        if e.is_empty() {
            return Ok(None);
        }
        let mut out = json!({});
        if let Some(to) = e.to {
            if to.is_empty() || to.len() > MAX_TO {
                return Err(bad("to must name 1..20 members"));
            }
            let members = self.members(conv_id).await?;
            let mut ids: Vec<String> = vec![];
            for k in to {
                let m = members
                    .iter()
                    .find(|m| m.user_id == k || m.name == k)
                    .ok_or_else(|| bad(format!("{k} is not a member of this conversation")))?;
                if !ids.contains(&m.user_id) {
                    ids.push(m.user_id.clone());
                }
            }
            out["to"] = json!(ids);
        }
        if let Some(t) = e.topics {
            if t.is_empty() || t.len() > MAX_TOPICS || !t.iter().all(|x| valid_topic(x)) {
                return Err(bad("topics are 1..10 names of 1..64 of a-z 0-9 . _ -"));
            }
            let mut t = t;
            t.dedup();
            out["topics"] = json!(t);
        }
        if let Some(i) = e.intent {
            if !INTENTS.contains(&i.as_str()) {
                return Err(bad("intent is request, proposal, answer, status or done"));
            }
            out["intent"] = json!(i);
        }
        if let Some(r) = e.refs {
            if r.is_empty()
                || r.len() > MAX_REFS
                || r.iter()
                    .any(|x| x.is_empty() || x.len() > 256 || x.contains('\0'))
            {
                return Err(bad("refs are 1..20 references of 1..256 bytes"));
            }
            out["refs"] = json!(r);
        }
        Ok(Some(out))
    }

    /// The agents a new message wakes, in priority order (named in `to`,
    /// then mentioned, then the rest by membership), capped at MAX_WAKE;
    /// and how many were left out by the cap.
    pub(super) async fn wake_set(
        &mut self,
        conv_id: &str,
        sender_id: &str,
        reply_to: Option<&str>,
        mentions: &[String],
        envelope: Option<&Value>,
    ) -> ApiResult<(Vec<String>, usize)> {
        let members = self.members(conv_id).await?;
        let replied_to = match reply_to {
            Some(r) => self.db.message(r).await?.map(|p| p.sender_id),
            None => None,
        };
        let to = to_of(envelope);
        let mut woken: Vec<(usize, usize, String)> = vec![];
        for (order, m) in members.iter().enumerate() {
            if m.kind != UserKind::Agent || m.user_id == sender_id {
                continue;
            }
            if !wakes(m, mentions, envelope, replied_to.as_deref(), &to) {
                continue;
            }
            let rank = match to.iter().position(|x| *x == m.user_id) {
                Some(i) => i,
                None => match mentions.iter().position(|x| *x == m.user_id) {
                    Some(i) => MAX_TO + i,
                    None => 10_000 + order,
                },
            };
            woken.push((rank, order, m.user_id.clone()));
        }
        woken.sort();
        let capped = woken.len().saturating_sub(MAX_WAKE);
        Ok((
            woken.into_iter().take(MAX_WAKE).map(|w| w.2).collect(),
            capped,
        ))
    }

    /// The root budget: agent messages per source of a chain. Refuses an
    /// agent message past the budget; the first refusal tells the
    /// conversation and the owner.
    pub(super) async fn root_gate(
        &mut self,
        agent: &User,
        conv_id: &str,
        root: Option<&str>,
    ) -> ApiResult<()> {
        let Some(root) = root else { return Ok(()) };
        let limit = self.st.guards.cfg.root_messages;
        let used = self.db.root_messages(conv_id, root).await?;
        if used < limit {
            return Ok(());
        }
        if used == limit {
            // Counted past the limit so this is told once per chain.
            self.db.charge_root(conv_id, root).await?;
            let ev = self
                .db
                .append_event(
                    conv_id,
                    "agent.state",
                    Some(&agent.id),
                    None,
                    json!({"agent_id": agent.id, "state": "stopped", "reason": "root_budget", "root_id": root, "detail": format!("{limit} agent messages since the last human message")}),
                )
                .await?;
            self.emit(ev).await?;
            self.notify_owner(
                conv_id,
                agent,
                "guard.tripped",
                json!({"agent_id": agent.id, "code": "root_budget", "root_id": root}),
            )
            .await?;
            self.db
                .audit(
                    Some(&agent.id),
                    "guard.root_budget",
                    Some(conv_id),
                    Some(root),
                    json!({"limit": limit}),
                )
                .await?;
            self.commit_on_error = true;
        }
        Err(err(
            "rate_limited",
            format!(
                "root budget: {limit} agent messages since the last human message; wait for a person"
            ),
        ))
    }

    /// The source of the chain an agent message joins: that of the message
    /// it answers (None: it starts one). People always start one.
    pub(super) async fn root_of(
        &mut self,
        sender: &User,
        cause: Option<&str>,
    ) -> ApiResult<Option<String>> {
        if sender.kind != UserKind::Agent {
            return Ok(None);
        }
        let Some(c) = cause else { return Ok(None) };
        Ok(self
            .db
            .message(c)
            .await?
            .map(|m| m.root_id.unwrap_or(m.msg_id)))
    }

    /// Charges a committed agent message to its chain's source.
    pub(super) async fn charge(&mut self, msg: &MsgView) -> ApiResult<()> {
        if msg.sender_type == UserKind::Agent
            && let Some(r) = &msg.root_id
        {
            self.db.charge_root(&msg.conv_id, r).await?;
        }
        Ok(())
    }

    /// Validates a receive mode change (`set_agent_policy`).
    pub(super) async fn check_mode(
        &mut self,
        conv_id: &str,
        agent_id: &str,
        trigger: Option<Trigger>,
        topics: Option<&[String]>,
    ) -> ApiResult<()> {
        if let Some(t) = topics
            && (t.is_empty() || t.len() > 20 || !t.iter().all(|x| valid_topic(x)))
        {
            return Err(bad("topics are 1..20 names of 1..64 of a-z 0-9 . _ -"));
        }
        let members = self.members(conv_id).await?;
        let me = members.iter().find(|m| m.user_id == agent_id);
        match trigger {
            Some(Trigger::All) => {
                let conv = self
                    .db
                    .conv(conv_id)
                    .await?
                    .ok_or_else(|| err("not_found", "conversation"))?;
                let agents = members.iter().filter(|m| m.kind == UserKind::Agent).count();
                if conv.kind != "dm" && agents > MAX_AGENTS_FOR_ALL {
                    return Err(err(
                        "conflict",
                        format!(
                            "all is for direct conversations or at most {MAX_AGENTS_FOR_ALL} agents; use addressed or topics"
                        ),
                    ));
                }
            }
            Some(Trigger::Topics) => {
                let has = topics.is_some_and(|t| !t.is_empty())
                    || me.is_some_and(|m| !m.topics.is_empty());
                if !has {
                    return Err(bad("topics mode needs topics"));
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// Wake flags of agent recipients from what the event recorded.
    pub(super) fn push_wake(ev: &Event, recips: &mut [crate::registry::Recip]) {
        let woken: Vec<&str> = ev.data["wake"]
            .as_array()
            .map(|a| a.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        for r in recips.iter_mut() {
            r.wake = woken.contains(&r.user_id.as_str());
        }
    }
}

/// Members named in an envelope's `to`.
pub fn to_of(envelope: Option<&Value>) -> Vec<String> {
    envelope
        .and_then(|e| e["to"].as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// Whether a message is for an agent member in its receive mode.
pub fn wakes(
    m: &Member,
    mentions: &[String],
    envelope: Option<&Value>,
    replied_to: Option<&str>,
    to: &[String],
) -> bool {
    let mentioned = mentions.contains(&m.user_id) || replied_to == Some(m.user_id.as_str());
    let addressed = mentioned || to.contains(&m.user_id);
    match m.trigger {
        Trigger::All => true,
        Trigger::Mention => mentioned,
        Trigger::Addressed => addressed,
        Trigger::Topics => {
            addressed
                || envelope
                    .and_then(|e| e["topics"].as_array())
                    .is_some_and(|t| {
                        t.iter()
                            .any(|x| x.as_str().is_some_and(|x| m.topics.iter().any(|s| s == x)))
                    })
        }
        Trigger::Observe => false,
    }
}
