//! Requests for a human decision (docs/03-agents.md §4.2): an agent
//! asks to approve, answer or choose; its owner or the people it names
//! decide; exactly one decision wins; the rest expire or are cancelled.

use std::collections::HashSet;

use serde_json::{Value, json};

use super::{Ctx, err};
use crate::db::Request;
use crate::model::*;
use crate::registry::Out;

/// Pending requests one agent may have at a time.
pub const MAX_PENDING: i64 = 100;
const TITLE_BYTES: usize = 4096;
const CONTEXT_BYTES: usize = 32 * 1024;
const ANSWER_BYTES: usize = 64 * 1024;
const MAX_OPTIONS: usize = 20;
const OPTION_BYTES: usize = 200;
const MAX_ASSIGNEES: usize = 20;
const TTL_DEFAULT: i64 = 86_400;
const TTL_MIN: i64 = 60;
const TTL_MAX: i64 = 30 * 86_400;

/// What an agent asks (`request_human`).
pub struct Ask {
    pub conv_id: String,
    pub kind: String,
    pub title: String,
    pub options: Option<Vec<String>>,
    pub context: Option<Value>,
    pub assignees: Option<Vec<String>>,
    pub ttl_secs: Option<i64>,
    pub client_req_id: String,
}

fn bad(msg: impl Into<String>) -> ApiError {
    rule!("request.input", "bad_request", msg)
}

fn text_ok(t: &str, max: usize) -> bool {
    !t.is_empty() && t.len() <= max && !t.contains('\0')
}

impl Ctx<'_, '_> {
    pub(super) async fn request_human(&mut self, me: &User, a: Ask) -> ApiResult<Value> {
        self.own_conv(&a.conv_id)?;
        if me.kind != UserKind::Agent {
            return Err(rule!(
                "request.agents_only",
                "forbidden",
                "only agents ask for a decision"
            ));
        }
        let owner = me.owner_id.clone().ok_or_else(|| {
            rule!(
                "request.ownerless",
                "forbidden",
                "an agent without an owner asks nobody"
            )
        })?;
        self.require_member(&a.conv_id, &me.id).await?;
        if !text_ok(&a.client_req_id, 128) {
            return Err(bad("client_req_id must be 1..128 bytes"));
        }
        // A retry returns the original request.
        if let Some(q) = self.db.request_by_client(&me.id, &a.client_req_id).await? {
            if q.conv_id != a.conv_id {
                return Err(rule!(
                    "request.id_other_conv",
                    "conflict",
                    "client_req_id already used in another conversation",
                ));
            }
            let mut v = q.view();
            v["existing"] = json!(true);
            return Ok(v);
        }
        if !text_ok(&a.title, TITLE_BYTES) {
            return Err(bad("title must be 1..4KB"));
        }
        let options = match (a.kind.as_str(), a.options) {
            ("choose", Some(o)) => {
                if !(2..=MAX_OPTIONS).contains(&o.len()) {
                    return Err(bad("choose needs 2..20 options"));
                }
                if o.iter().any(|x| !text_ok(x, OPTION_BYTES)) {
                    return Err(bad("each option must be 1..200 bytes"));
                }
                if o.iter().collect::<HashSet<_>>().len() != o.len() {
                    return Err(bad("options must be different"));
                }
                Some(json!(o))
            }
            ("choose", None) => return Err(bad("choose needs 2..20 options")),
            ("approve" | "answer", None) => None,
            ("approve" | "answer", Some(_)) => return Err(bad("options are only for choose")),
            _ => return Err(bad("kind must be approve, answer or choose")),
        };
        if a.context
            .as_ref()
            .is_some_and(|c| c.to_string().len() > CONTEXT_BYTES)
        {
            return Err(bad("context must be at most 32KB of JSON"));
        }
        let ttl = a.ttl_secs.unwrap_or(TTL_DEFAULT);
        if !(TTL_MIN..=TTL_MAX).contains(&ttl) {
            return Err(bad("ttl_secs must be 60..2592000"));
        }
        let mut assignees: Vec<String> = vec![];
        match a.assignees {
            None => assignees.push(owner.clone()),
            Some(list) => {
                if list.is_empty() || list.len() > MAX_ASSIGNEES {
                    return Err(bad("assignees must name 1..20 people"));
                }
                for k in list {
                    let u = self
                        .sh
                        .user(&k)
                        .or_else(|| self.sh.user_by_name(&k))
                        .ok_or_else(|| {
                            rule!("account.unknown_user", "not_found", format!("user {k}"))
                        })?;
                    if u.kind != UserKind::Human {
                        return Err(bad("only people decide"));
                    }
                    if self.db.member(&a.conv_id, &u.id).await?.is_none() {
                        return Err(bad(format!(
                            "{} is not a member of this conversation",
                            u.name
                        )));
                    }
                    if !assignees.contains(&u.id) {
                        assignees.push(u.id);
                    }
                }
            }
        }
        if self.db.pending_request_count(&me.id).await? >= MAX_PENDING {
            return Err(rule!(
                "request.too_many",
                "rate_limited",
                "too many requests waiting for a decision",
            ));
        }
        let now = now_ms();
        let q = Request {
            request_id: format!("q_{}", crate::db::new_id()),
            conv_id: a.conv_id.clone(),
            agent_id: me.id.clone(),
            client_req_id: a.client_req_id,
            kind: a.kind,
            title: a.title,
            options,
            context: a.context,
            state: "pending".into(),
            created_at: now,
            expires_at: now + ttl * 1000,
            decided_by: None,
            decided_at: None,
            response: None,
        };
        self.db.create_request(&q, &assignees).await?;
        let audience = audience(&q.agent_id, &owner, &assignees);
        let side = self
            .db
            .append_side(
                &q.conv_id,
                &audience,
                "request.created",
                Some(&me.id),
                json!({"request": q.view(), "assignees": assignees}),
            )
            .await?;
        self.out.push(Out::Side(side));
        self.db
            .audit(
                Some(&me.id),
                "request.created",
                Some(&q.conv_id),
                Some(&q.request_id),
                json!({"kind": q.kind, "assignees": assignees}),
            )
            .await?;
        Ok(q.view())
    }

    pub(super) async fn decide(
        &mut self,
        me: &User,
        request_id: &str,
        decision: &str,
        text: Option<String>,
        choice: Option<String>,
    ) -> ApiResult<Value> {
        let q = self
            .db
            .request(request_id)
            .await?
            .ok_or_else(|| err("conv_moved", "request not in this database"))?;
        self.own_conv(&q.conv_id)?;
        let (owner, assignees) = self.deciders(&q).await?;
        if me.id != owner && !assignees.contains(&me.id) {
            return Err(rule!(
                "request.not_decider",
                "forbidden",
                "this request is not yours to decide"
            ));
        }
        if q.state != "pending" {
            return Err(rule!(
                "request.state",
                "conflict",
                format!("the request is {}", q.state)
            ));
        }
        if now_ms() >= q.expires_at {
            return Err(rule!(
                "request.expired",
                "conflict",
                "the request has expired"
            ));
        }
        let (state, response) = match (q.kind.as_str(), decision) {
            (_, "reject") => {
                if text.as_ref().is_some_and(|t| !text_ok(t, TITLE_BYTES)) {
                    return Err(bad("a reason must be 1..4KB"));
                }
                ("rejected", json!({"decision": "reject", "text": text}))
            }
            ("approve", "approve") => {
                if text.as_ref().is_some_and(|t| !text_ok(t, TITLE_BYTES)) {
                    return Err(bad("a comment must be 1..4KB"));
                }
                ("approved", json!({"decision": "approve", "text": text}))
            }
            ("answer", "answer") => match text {
                Some(t) if text_ok(&t, ANSWER_BYTES) => {
                    ("answered", json!({"decision": "answer", "text": t}))
                }
                _ => return Err(bad("an answer needs text of 1..64KB")),
            },
            ("choose", "choose") => {
                let options: Vec<String> = q
                    .options
                    .clone()
                    .and_then(|o| serde_json::from_value(o).ok())
                    .unwrap_or_default();
                match choice {
                    Some(c) if options.contains(&c) => {
                        ("chosen", json!({"decision": "choose", "choice": c}))
                    }
                    _ => return Err(bad("choice must be one of the options")),
                }
            }
            _ => {
                return Err(bad(format!(
                    "a {} request takes {}",
                    q.kind,
                    match q.kind.as_str() {
                        "approve" => "approve or reject",
                        "answer" => "answer or reject",
                        _ => "choose or reject",
                    }
                )));
            }
        };
        if !self
            .db
            .finish_request(&q, state, Some(&me.id), Some(&response))
            .await?
        {
            return Err(rule!(
                "request.decided",
                "conflict",
                "the request was already decided"
            ));
        }
        self.request_finished(&q, state, Some(&me.id), Some(response), &owner, &assignees)
            .await
    }

    pub(super) async fn cancel_request(&mut self, me: &User, request_id: &str) -> ApiResult<Value> {
        let q = self
            .db
            .request(request_id)
            .await?
            .ok_or_else(|| err("conv_moved", "request not in this database"))?;
        self.own_conv(&q.conv_id)?;
        if q.agent_id != me.id {
            return Err(rule!(
                "request.cancel_not_asker",
                "forbidden",
                "only the agent that asked cancels"
            ));
        }
        if q.state != "pending" {
            return Err(rule!(
                "request.state",
                "conflict",
                format!("the request is {}", q.state)
            ));
        }
        let (owner, assignees) = self.deciders(&q).await?;
        if !self
            .db
            .finish_request(&q, "cancelled", Some(&me.id), None)
            .await?
        {
            return Err(rule!(
                "request.decided",
                "conflict",
                "the request was already decided"
            ));
        }
        self.request_finished(&q, "cancelled", Some(&me.id), None, &owner, &assignees)
            .await
    }

    /// Pending requests past their time become `expired` (timer).
    pub(super) async fn expire_requests(&mut self) -> ApiResult<()> {
        for q in self.db.expired_requests(now_ms()).await? {
            if self.sh.dir_get(&q.conv_id).is_none_or(|d| d.db != self.idx)
                || !self.owns(&q.conv_id)
            {
                continue;
            }
            let (owner, assignees) = self.deciders(&q).await?;
            if self.db.finish_request(&q, "expired", None, None).await? {
                self.request_finished(&q, "expired", None, None, &owner, &assignees)
                    .await?;
            }
        }
        Ok(())
    }

    /// An agent leaves a conversation: what it still waits for is cancelled.
    pub(super) async fn cancel_requests_of(
        &mut self,
        conv_id: &str,
        agent: &User,
    ) -> ApiResult<()> {
        for q in self
            .db
            .pending_requests_for("", std::slice::from_ref(&agent.id))
            .await?
        {
            if q.conv_id != conv_id {
                continue;
            }
            let (owner, assignees) = self.deciders(&q).await?;
            if self.db.finish_request(&q, "cancelled", None, None).await? {
                self.request_finished(&q, "cancelled", None, None, &owner, &assignees)
                    .await?;
            }
        }
        Ok(())
    }

    async fn deciders(&mut self, q: &Request) -> ApiResult<(String, Vec<String>)> {
        let owner = self
            .sh
            .user(&q.agent_id)
            .and_then(|a| a.owner_id)
            .unwrap_or_default();
        let assignees = self.db.request_assignees(&q.request_id).await?;
        Ok((owner, assignees))
    }

    async fn request_finished(
        &mut self,
        q: &Request,
        state: &str,
        by: Option<&str>,
        response: Option<Value>,
        owner: &str,
        assignees: &[String],
    ) -> ApiResult<Value> {
        let kind = match state {
            "expired" => "request.expired",
            "cancelled" => "request.cancelled",
            _ => "request.decided",
        };
        let data = json!({"request_id": q.request_id, "state": state, "response": response, "decided_by": by});
        let side = self
            .db
            .append_side(
                &q.conv_id,
                &audience(&q.agent_id, owner, assignees),
                kind,
                by,
                data.clone(),
            )
            .await?;
        self.out.push(Out::Side(side));
        self.db
            .audit(
                by,
                kind,
                Some(&q.conv_id),
                Some(&q.request_id),
                json!({"state": state}),
            )
            .await?;
        Ok(data)
    }
}

fn audience(agent: &str, owner: &str, assignees: &[String]) -> Vec<String> {
    let mut a = vec![agent.to_string()];
    for u in std::iter::once(&owner.to_string()).chain(assignees) {
        if !u.is_empty() && !a.contains(u) {
            a.push(u.clone());
        }
    }
    a
}
