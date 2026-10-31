//! Requests for a human decision, read side (docs/03-agents.md §4.2):
//! one request (optionally waiting for its outcome) and the inbox of what
//! waits for a person across conversations.

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
    pub(super) async fn inbox(&self, me: &User) -> ApiResult<Value> {
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
                        let q = db.pending_requests_for(&me_id, &a).await?;
                        let d = db.pending_drafts_for(&a).await?;
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
                    json!({"type": "draft", "draft_id": d.draft_id, "conv_id": d.conv_id, "agent_id": d.agent_id, "content": d.content, "created_at": d.created_at}),
                ));
            }
        }
        items.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
        Ok(json!({"items": items.into_iter().map(|i| i.2).collect::<Vec<_>>()}))
    }
}
