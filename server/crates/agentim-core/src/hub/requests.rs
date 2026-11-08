//! Requests for a human decision, read side (docs/en/design/agents.md):
//! one request (optionally waiting for its outcome) and the inbox of what
//! waits for a person across conversations.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::Hub;
use crate::db::Request;
use crate::model::*;

/// Longest `get_request` wait (below common HTTP and MCP client timeouts).
pub const MAX_WAIT_SECS: i64 = 25;

impl Hub {
    /// A request with its assignees; visible to the asking agent, its owner
    /// and the assignees.
    pub(super) async fn get_request(
        &self,
        me: &User,
        request_id: &str,
        wait_secs: Option<i64>,
    ) -> ApiResult<Value> {
        let wait = wait_secs.unwrap_or(0);
        if !(0..=MAX_WAIT_SECS).contains(&wait) {
            return Err(rule!(
                "request.wait_secs",
                "bad_request",
                "wait_secs must be 0..25"
            ));
        }
        let deadline = Instant::now() + Duration::from_secs(wait as u64);
        loop {
            let (q, assignees) = self.find_request(request_id).await?;
            let owner = self.inner.sh.user(&q.agent_id).and_then(|a| a.owner_id);
            if me.id != q.agent_id
                && owner.as_deref() != Some(me.id.as_str())
                && !assignees.contains(&me.id)
            {
                return Err(rule!("request.not_yours", "forbidden", "not your request"));
            }
            if q.state != "pending" || Instant::now() >= deadline {
                let mut v = q.view();
                v["assignees"] = json!(assignees);
                return Ok(v);
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    async fn find_request(&self, request_id: &str) -> ApiResult<(Request, Vec<String>)> {
        for s in &self.inner.shards {
            let id = request_id.to_string();
            let found = self
                .read(s, move |db| {
                    Box::pin(async move {
                        match db.request(&id).await? {
                            Some(q) => {
                                let a = db.request_assignees(&id).await?;
                                Ok(Some((q, a)))
                            }
                            None => Ok(None),
                        }
                    })
                })
                .await?;
            if let Some(f) = found {
                return Ok(f);
            }
        }
        Err(rule!("request.unknown", "not_found", "request"))
    }

    /// Requests waiting for me (assigned to me, or asked by my agents) and
    /// my agents' drafts, oldest first.
    /// `card_answers` (docs/en/design/chat-components.md): a card's answers from the log, so a
    /// card that was answered shows it after a reload too.
    pub(super) async fn card_answers(&self, me: &User, msg_id: &str) -> ApiResult<Value> {
        let id = msg_id.to_string();
        let conv = self
            .find_conv_of(|db| {
                Box::pin({
                    let id = id.clone();
                    async move { db.msg_conv(&id).await }
                })
            })
            .await
            .map_err(|_| rule!("msg.unknown", "not_found", "message"))?;
        self.reader_member(me, &conv, None).await?;
        let (c, i) = (conv.clone(), id.clone());
        let (msg, events) = self
            .read(self.conv_shard(&conv)?, move |db| {
                Box::pin(async move {
                    let m = db.message(&i).await?;
                    let from = m.as_ref().map(|m| m.created_seq).unwrap_or(0);
                    let e = db.card_answers(&c, from, &i).await?;
                    Ok((m, e))
                })
            })
            .await?;
        let m = msg
            .filter(|m| m.kind == crate::logic::cards::KIND)
            .ok_or_else(|| rule!("card.unknown", "not_found", "no such card"))?;
        let answers: Vec<Value> = events
            .iter()
            .map(|e| {
                let by = e.data["by"].as_str().unwrap_or_default();
                let mut v = json!({"block_id": e.data["block_id"], "by": by,
                    "by_name": self.inner.sh.user(by).map(|u| u.name),
                    "action": e.data["action"], "at": e.created_at, "seq": e.seq});
                if by == me.id || m.sender_id == me.id {
                    v["values"] = e.data["values"].clone();
                }
                v
            })
            .collect();
        Ok(json!({"msg_id": msg_id, "answers": answers}))
    }

    pub(super) async fn inbox(&self, me: &User, limit: Option<i64>) -> ApiResult<Value> {
        let limit = limit.unwrap_or(100);
        if !(1..=500).contains(&limit) {
            return Err(rule!(
                "request.inbox_limit",
                "bad_request",
                "limit is 1..500"
            ));
        }
        let agents: Vec<String> = self
            .inner
            .sh
            .agents_of(&me.id)
            .into_iter()
            .map(|a| a.id)
            .collect();
        let mut items: Vec<(i64, String, Value)> = vec![];
        for s in &self.inner.shards {
            let (a, me_id) = (agents.clone(), me.id.clone());
            let (requests, drafts, handoffs) = self
                .read(s, move |db| {
                    Box::pin(async move {
                        // A request that names its deciders is theirs, not the
                        // owner's to act on (docs/en/design/chat-components.md).
                        let mut q = vec![];
                        for r in db.pending_requests_for(&me_id, &a).await? {
                            let who = db.request_assignees(&r.request_id).await?;
                            if who.is_empty() || who.contains(&me_id) {
                                q.push(r);
                            }
                        }
                        let mut d = db.pending_drafts_for(&a).await?;
                        for x in db.pending_drafts_of_approver(&me_id).await? {
                            if !d.iter().any(|y| y.draft_id == x.draft_id) {
                                d.push(x);
                            }
                        }
                        let h = db.handoffs_to(&me_id).await?;
                        Ok((q, d, h))
                    })
                })
                .await?;
            for (conv_id, agent_id, since, note) in handoffs {
                items.push((
                    since,
                    format!("{conv_id}/{agent_id}"),
                    json!({"type": "handoff", "conv_id": conv_id, "agent_id": agent_id, "note": note, "created_at": since}),
                ));
            }
            for q in requests {
                if q.expires_at <= now_ms() {
                    continue;
                }
                let mut v = q.view();
                v["type"] = json!("request");
                items.push((q.created_at, q.request_id.clone(), v));
            }
            for d in drafts {
                items.push((
                    d.created_at,
                    d.draft_id.clone(),
                    json!({"type": "draft", "draft_id": d.draft_id, "conv_id": d.conv_id, "agent_id": d.agent_id, "content": d.content, "created_at": d.created_at, "stage": d.stage, "reviewer": d.reviewer, "expires_at": d.expires_at}),
                ));
            }
        }
        items.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
        let more = items.len() as i64 > limit;
        items.truncate(limit as usize);
        // What a row shows without asking again: the conversation, the agent,
        // the person it is for.
        let mut titles: HashMap<String, String> = HashMap::new();
        for (_, _, v) in &mut items {
            let conv = v["conv_id"].as_str().unwrap_or_default().to_string();
            if !titles.contains_key(&conv) {
                let c = conv.clone();
                let t = match self.conv_shard(&conv) {
                    Ok(s) => self
                        .read(s, move |db| Box::pin(async move { db.conv(&c).await }))
                        .await
                        .ok()
                        .flatten()
                        .map(|r| r.title)
                        .unwrap_or_default(),
                    Err(_) => String::new(),
                };
                titles.insert(conv.clone(), t);
            }
            v["conv_title"] = json!(titles[&conv]);
            if let Some(a) = v["agent_id"].as_str().and_then(|id| self.inner.sh.user(id)) {
                v["agent_name"] = json!(a.name);
            }
            let asker = v["context"]["tool_call"]["msg"]["sender_name"].clone();
            if !asker.is_null() {
                v["asker_name"] = asker;
            }
        }
        Ok(json!({"items": items.into_iter().map(|i| i.2).collect::<Vec<_>>(), "more": more}))
    }
}
