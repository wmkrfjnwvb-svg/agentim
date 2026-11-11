//! Agents the server answers for itself (docs/en/design/assistants.md): an assistant is an
//! agent with a model service and model. One task per assistant holds an
//! in-process session (`connect`, the same protocol as the Agent SDK),
//! syncs, and answers each message that wakes it with a streamed reply.
//!
//! - Only the node that does the control database's upkeep runs them, so a
//!   cluster answers once; a reply's `client_msg_id` comes from the message
//!   it answers, so a retry or a second runner cannot post twice.
//! - Every model call is recorded (tokens and cost, never text); limits:
//!   replies per minute and per day per assistant, tokens per day per
//!   service.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::Hub;
use crate::ai::prices::Usage;
use crate::ai::provider::{AiError, Item, Request, Stop, ToolCall, ToolDef, ToolResult};
use crate::db::ai::{AssistantRow, UsageRow};
use crate::logic::{AdminOp, Op};
use crate::model::*;

/// Built-in roles (`builtin/agents/*.md`): enabled from the console.
pub use crate::library::ROLES as CATALOG;

/// How often the runner looks at what it should run.
const SUPERVISE_EVERY: Duration = Duration::from_secs(5);
/// Streamed text is sent in pieces of at least this much (or at the end).
const CHUNK: usize = 120;
/// Model calls in one reply (tool use in between), at most.
const MAX_ROUNDS: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    /// Most tokens in one reply.
    pub max_tokens: u32,
    /// Most tokens of the conversation sent as context.
    pub context_tokens: i64,
    pub replies_per_minute: u32,
    pub replies_per_day: u32,
    /// What it says when the model can't answer.
    pub fallback: String,
    /// Answer only from its knowledge files; with nothing relevant there,
    /// say so (and hand over to a person with `handoff`).
    pub only_knowledge: bool,
    pub handoff: bool,
    /// Shows each tool call as a step next to the reply (docs/en/design/chat-components.md).
    pub steps: bool,
    /// May ask the person with a form card (`ask_form`, docs/en/design/chat-components.md).
    pub forms: bool,
    /// May show data as a card (`show_card`, docs/en/design/chat-components.md).
    pub cards: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            max_tokens: 2048,
            context_tokens: 8000,
            replies_per_minute: 10,
            replies_per_day: 1000,
            fallback: "Sorry, I can't answer right now. Please try again in a moment.".into(),
            only_knowledge: false,
            handoff: false,
            steps: true,
            forms: true,
            cards: true,
        }
    }
}

impl Settings {
    fn check(&self) -> ApiResult<()> {
        let bad = |m: &str| Err(rule!("assistant.settings", "bad_request", m.to_string()));
        if !(256..=32_000).contains(&self.max_tokens) {
            return bad("max_tokens is 256..32000");
        }
        if !(256..=200_000).contains(&self.context_tokens) {
            return bad("context_tokens is 256..200000");
        }
        if !(1..=600).contains(&self.replies_per_minute) {
            return bad("replies_per_minute is 1..600");
        }
        if !(1..=1_000_000).contains(&self.replies_per_day) {
            return bad("replies_per_day is 1..1000000");
        }
        if self.fallback.trim().is_empty() || self.fallback.len() > 500 {
            return bad("fallback is 1..500 bytes");
        }
        Ok(())
    }
}

/// `PUT /admin/v1/app/agents/{id}/assistant`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssistantReq {
    pub service: String,
    pub model: String,
    #[serde(default)]
    pub settings: Option<Settings>,
}

/// `POST /admin/v1/app/assistants/catalog/{slug}`: a built-in role made
/// into a team agent the server runs.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnableReq {
    /// The person who looks after it.
    pub owner: String,
    pub service: String,
    pub model: String,
    /// The agent's id; the role's name by default.
    #[serde(default)]
    pub id: Option<String>,
}

/// Aborts a task when dropped (the runner's children go with it).
struct Task(tokio::task::JoinHandle<()>);

impl Drop for Task {
    fn drop(&mut self) {
        self.0.abort();
    }
}

fn day_of(ms: i64) -> String {
    super::tenants::utc_day(ms)
}

impl Hub {
    /// The built-in roles and whether each is running as an agent here.
    pub async fn assistant_catalog(&self) -> ApiResult<Vec<Value>> {
        let running = self
            .read(self.control(), |db| {
                Box::pin(async move { db.assistants().await })
            })
            .await?;
        let mut out = vec![];
        for (slug, md) in CATALOG {
            let r = crate::role::parse(slug, md).map_err(|(_, m)| ApiError::new("internal", m))?;
            let on = running.iter().any(|a| {
                self.inner
                    .sh
                    .user(&a.agent_id)
                    .is_some_and(|u| u.name == *slug)
            });
            out.push(json!({"slug": slug, "description": r.description, "tags": r.tags, "instructions": r.instructions, "enabled": on}));
        }
        Ok(out)
    }

    /// The model must be one the service lists, and with a built-in key
    /// one with a price.
    async fn check_model(&self, service: &str, model: &str) -> ApiResult<()> {
        let list = self.service_models(service).await?;
        let found = list["models"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|m| m["id"] == model);
        match found {
            None => Err(rule!(
                "assistant.model",
                "bad_request",
                format!("{service} has no model {model}")
            )),
            Some(m) if m["usable"] != true => Err(rule!(
                "assistant.model_unpriced",
                "bad_request",
                "this model has no known price, so the built-in key can't be used for it"
            )),
            Some(_) => Ok(()),
        }
    }

    fn assistant_agent(&self, agent_id: &str) -> ApiResult<User> {
        self.inner
            .sh
            .user(agent_id)
            .filter(|u| u.kind == UserKind::Agent && !self.is_destroyed(&u.id))
            .ok_or_else(|| rule!("agent.unknown", "not_found", "no such agent"))
    }

    pub async fn assistants(&self) -> ApiResult<Vec<Value>> {
        let rows = self
            .read(self.control(), |db| {
                Box::pin(async move { db.assistants().await })
            })
            .await?;
        Ok(rows
            .iter()
            .map(|a| {
                let settings: Value = serde_json::from_str(&a.settings).unwrap_or(json!({}));
                let name = self
                    .inner
                    .sh
                    .user(&a.agent_id)
                    .map(|u| u.name)
                    .unwrap_or_default();
                json!({"agent_id": a.agent_id, "name": name, "service": a.service, "model": a.model,
                    "settings": settings, "created_at": a.created_at, "updated_at": a.updated_at})
            })
            .collect())
    }

    /// Makes the server answer for an agent (or changes how).
    pub async fn put_assistant(&self, agent_id: &str, req: AssistantReq) -> ApiResult<Value> {
        let agent = self.assistant_agent(agent_id)?;
        let settings = req.settings.unwrap_or_default();
        settings.check()?;
        self.check_model(&req.service, &req.model).await?;
        let now = now_ms();
        let id = agent.id.clone();
        let old = self
            .read(self.control(), move |db| {
                Box::pin(async move { db.assistant(&id).await })
            })
            .await?;
        let row = AssistantRow {
            agent_id: agent.id.clone(),
            service: req.service,
            model: req.model,
            settings: serde_json::to_string(&settings).unwrap_or_else(|_| "{}".into()),
            created_at: old.map(|o| o.created_at).unwrap_or(now),
            updated_at: now,
        };
        let builtin = self
            .inner
            .cfg
            .ai
            .builtin
            .iter()
            .any(|b| b.name == row.service);
        let v = self
            .control()
            .call(Op::Admin(AdminOp::AssistantPut { row, builtin }))
            .await?;
        self.inner.ai.wake.notify_one();
        Ok(v)
    }

    pub async fn delete_assistant(&self, agent_id: &str) -> ApiResult<Value> {
        let v = self
            .control()
            .call(Op::Admin(AdminOp::AssistantDelete {
                agent_id: agent_id.to_string(),
            }))
            .await?;
        self.inner.ai.wake.notify_one();
        Ok(v)
    }

    /// Turns a built-in role into a team agent the server runs: the role
    /// goes into the library (unless one by that name is there), the agent
    /// is created with the owner as its maintainer and gets the role.
    pub async fn enable_builtin(&self, slug: &str, req: EnableReq) -> ApiResult<Value> {
        CATALOG
            .iter()
            .find(|(s, _)| *s == slug)
            .ok_or_else(|| rule!("assistant.catalog", "not_found", "no such built-in agent"))?;
        let owner = self
            .user_by_name(&req.owner)
            .filter(|u| u.kind == UserKind::Human)
            .ok_or_else(|| {
                rule!(
                    "assistant.owner",
                    "bad_request",
                    "owner is a person of this app"
                )
            })?;
        let id = req.id.clone().unwrap_or_else(|| slug.to_string());
        if self.user_by_name(&id).is_some() {
            return Err(rule!(
                "assistant.exists",
                "conflict",
                format!("{id} is taken")
            ));
        }
        // Everything that can be refused is checked before anything is written.
        self.check_model(&req.service, &req.model).await?;
        let s = slug.to_string();
        let have_role = self
            .read(self.control(), move |db| {
                Box::pin(async move { db.role_get(&s).await })
            })
            .await?
            .is_some();
        if !have_role {
            // With the Skills the role names that the library lacks.
            self.install_library_role(slug, false).await?;
        }
        let made = self.create_agent_with(&owner, &id, true).await?;
        let agent = self
            .user_by_name(&id)
            .ok_or_else(|| ApiError::new("internal", "the new agent is not loaded"))?;
        self.set_agent_role(&agent.id, Some(slug)).await?;
        let v = self
            .put_assistant(
                &agent.id,
                AssistantReq {
                    service: req.service,
                    model: req.model,
                    settings: None,
                },
            )
            .await?;
        Ok(json!({"agent": made["agent"], "assistant": v}))
    }

    /// Starts the runner (part of the background work).
    pub fn start_assistants(&self) {
        let hub = self.clone();
        self.inner
            .tickers
            .lock()
            .unwrap()
            .push(tokio::spawn(async move { hub.supervise().await }));
    }

    async fn supervise(&self) {
        let mut running: HashMap<String, (i64, Task)> = HashMap::new();
        loop {
            let want: Vec<AssistantRow> = if self.control().holds_upkeep() {
                self.read(self.control(), |db| {
                    Box::pin(async move { db.assistants().await })
                })
                .await
                .unwrap_or_default()
            } else {
                vec![]
            };
            running
                .retain(|id, (v, _)| want.iter().any(|a| &a.agent_id == id && a.updated_at == *v));
            for a in want {
                if running.contains_key(&a.agent_id) {
                    continue;
                }
                let Some(user) = self.inner.sh.user(&a.agent_id) else {
                    continue;
                };
                if self.is_destroyed(&user.id) || self.is_stopped(&user.id) {
                    continue;
                }
                let hub = self.clone();
                let row = a.clone();
                let t = tokio::spawn(async move { hub.run_assistant(user, row).await });
                running.insert(a.agent_id.clone(), (a.updated_at, Task(t)));
            }
            tokio::select! {
                _ = self.inner.ai.wake.notified() => {}
                _ = tokio::time::sleep(SUPERVISE_EVERY) => {}
            }
        }
    }

    async fn call_as(&self, sess: &super::Session, v: Value) -> Value {
        match serde_json::from_value::<Envelope>(v) {
            Ok(env) => self.handle(sess, env).await,
            Err(e) => {
                json!({"ok": false, "error": {"code": "bad_request", "message": e.to_string()}})
            }
        }
    }

    /// One assistant's session: sync what it missed, then answer pushes,
    /// acknowledging each event after it was handled. Reconnects after a
    /// failure.
    async fn run_assistant(&self, user: User, row: AssistantRow) {
        loop {
            let (sess, mut pushes) = self.connect(user.clone());
            self.call_as(&sess, json!({"op": "hello", "rid": 1, "client": {"name": "agentim-assistant", "version": env!("CARGO_PKG_VERSION")}}))
                .await;
            let ok = self.assistant_sync(&sess, &row).await;
            if ok {
                while let Some(f) = pushes.recv().await {
                    let Ok(v) = serde_json::from_str::<Value>(&f) else {
                        continue;
                    };
                    let ok = match v["type"].as_str() {
                        Some("event") => {
                            self.assistant_item(&sess, &row, &v).await;
                            let ack = json!({"op": "ack", "rid": 2, "acks": [{"conv_id": v["event"]["conv_id"], "seq": v["event"]["seq"]}]});
                            self.call_as(&sess, ack).await["ok"] == true
                        }
                        Some("side") => {
                            self.assistant_side(&sess, &row, &v["event"]).await;
                            true
                        }
                        Some("resync") => self.assistant_sync(&sess, &row).await,
                        _ => true,
                    };
                    if !ok {
                        break;
                    }
                }
            }
            drop(sess);
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }

    async fn assistant_sync(&self, sess: &super::Session, row: &AssistantRow) -> bool {
        for _ in 0..1000 {
            let r = self
                .call_as(sess, json!({"op": "sync", "rid": 3, "limit": 200}))
                .await;
            if r["ok"] != true {
                return false;
            }
            let mut acks = vec![];
            for c in r["convs"].as_array().into_iter().flatten() {
                for item in c["events"].as_array().into_iter().flatten() {
                    self.assistant_item(sess, row, item).await;
                }
                for side in c["side_events"].as_array().into_iter().flatten() {
                    self.assistant_side(sess, row, side).await;
                }
                if !c["upto"].is_null() {
                    acks.push(json!({"conv_id": c["conv_id"], "seq": c["upto"], "side_seq": c["side_upto"].as_i64().unwrap_or(0)}));
                }
            }
            if !acks.is_empty()
                && self
                    .call_as(sess, json!({"op": "ack", "rid": 4, "acks": acks}))
                    .await["ok"]
                    != true
            {
                return false;
            }
            if r["more"] != true {
                return true;
            }
        }
        true
    }

    /// Answers one event when it is a message that woke the assistant.
    async fn assistant_item(&self, sess: &super::Session, row: &AssistantRow, item: &Value) {
        let ev = &item["event"];
        if ev["type"] == "card.response" && ev["data"]["to"] == sess.user.id.as_str() {
            if let Some(conv_id) = ev["conv_id"].as_str()
                && let Err(e) = self.after_form(sess, row, conv_id, &ev["data"]).await
            {
                tracing::warn!(
                    agent = sess.user.id,
                    conv_id,
                    error = e.message,
                    "form answers"
                );
            }
            return;
        }
        if item["wake"] != true || ev["type"] != "msg.created" {
            return;
        }
        let msg = &ev["msg"];
        let (Some(conv_id), Some(msg_id)) = (ev["conv_id"].as_str(), msg["msg_id"].as_str()) else {
            return;
        };
        if msg["sender_id"] == sess.user.id.as_str()
            || msg["content"].as_str().is_none_or(|t| t.trim().is_empty())
        {
            return;
        }
        if let Err(e) = self.answer(sess, row, conv_id, msg_id, msg).await {
            tracing::warn!(
                agent = sess.user.id,
                conv_id,
                error = e.message,
                "assistant reply"
            );
        }
    }

    /// A hosted project (its limits from the cloud carry `ai`) uses a
    /// built-in key only for the provider its organization's credit is
    /// for, and until the credit is spent (docs/en/design/assistants.md). Self-hosted: no
    /// limit.
    async fn builtin_credit(&self, provider: &str) -> ApiResult<Option<&'static str>> {
        let ai = self.inner.own_limits.read().unwrap().get("ai").cloned();
        let Some(ai) = ai else { return Ok(None) };
        if ai["provider"].as_str() != Some(provider) {
            return Ok(Some(
                "The built-in AI isn't set up for this app. Ask its owner to add AI credit.",
            ));
        }
        let until = ai["allow_until_nanos"].as_i64().unwrap_or(0);
        let spent = self
            .read(self.control(), |db| {
                Box::pin(async move { db.builtin_cost_since(0).await })
            })
            .await?;
        Ok((spent >= until)
            .then_some("This app's AI credit is used up. Ask its owner to top it up."))
    }

    /// Whether the assistant may answer now (per minute, per day, the
    /// service's tokens today); the refusal it says otherwise.
    async fn assistant_allowed(
        &self,
        row: &AssistantRow,
        settings: &Settings,
        daily_tokens: u64,
    ) -> ApiResult<Option<&'static str>> {
        {
            let mut m = self.inner.ai.minute.lock().unwrap();
            let q = m.entry(row.agent_id.clone()).or_default();
            q.retain(|t| t.elapsed() < Duration::from_secs(60));
            if q.len() >= settings.replies_per_minute as usize {
                return Ok(Some(
                    "I'm getting a lot of questions right now. Please ask again in a minute.",
                ));
            }
            q.push(Instant::now());
        }
        let day = day_of(now_ms());
        let (d, a, s) = (day.clone(), row.agent_id.clone(), row.service.clone());
        let (replies, tokens) = self
            .read(self.control(), move |db| {
                Box::pin(async move {
                    Ok((
                        db.agent_replies(&d, &a).await?,
                        db.service_tokens(&d, &s).await?,
                    ))
                })
            })
            .await?;
        if replies >= settings.replies_per_day as i64 {
            return Ok(Some(
                "I've answered as many questions as I can today. Please try again tomorrow.",
            ));
        }
        if daily_tokens > 0 && tokens >= daily_tokens as i64 {
            return Ok(Some(
                "Today's usage limit for this assistant is used up. Please try again tomorrow.",
            ));
        }
        Ok(None)
    }

    async fn answer(
        &self,
        sess: &super::Session,
        row: &AssistantRow,
        conv_id: &str,
        msg_id: &str,
        msg: &Value,
    ) -> ApiResult<()> {
        match self
            .claim_reply(
                sess,
                conv_id,
                &format!("ai-{msg_id}"),
                msg_id,
                msg["thread_id"].as_str(),
            )
            .await?
        {
            Some(reply_id) => {
                self.respond(sess, row, conv_id, msg_id, msg, &reply_id, None)
                    .await
            }
            None => Ok(()),
        }
    }

    /// Starts the reply `client_msg_id` once: `None` when it was started
    /// before (a second start returns the first). The server slows agents
    /// down; the reply waits rather than being lost.
    async fn claim_reply(
        &self,
        sess: &super::Session,
        conv_id: &str,
        client_msg_id: &str,
        reply_to: &str,
        // A message in a thread is answered in that thread (MSG-19).
        thread: Option<&str>,
    ) -> ApiResult<Option<String>> {
        let mut wait = 500u64;
        let start = loop {
            let r = self
                .call_as(sess, json!({"op": "stream_start", "rid": 5, "conv_id": conv_id, "client_msg_id": client_msg_id, "reply_to": reply_to, "thread_id": thread}))
                .await;
            if r["error"]["code"] != "rate_limited" || wait > 16_000 {
                break r;
            }
            tokio::time::sleep(Duration::from_millis(wait)).await;
            wait *= 2;
        };
        if start["ok"] != true {
            return Err(ApiError::new(
                "conflict",
                format!("stream_start: {}", start["error"]),
            ));
        }
        if start["duplicate"] == true {
            return Ok(None);
        }
        Ok(Some(
            start["msg_id"].as_str().unwrap_or_default().to_string(),
        ))
    }

    /// Writes the reply `reply_id` to `msg`. `note` (after a person decided
    /// a tool call, docs/en/design/assistants.md) goes into the instructions instead of
    /// searching the knowledge.
    #[allow(clippy::too_many_arguments)]
    async fn respond(
        &self,
        sess: &super::Session,
        row: &AssistantRow,
        conv_id: &str,
        msg_id: &str,
        msg: &Value,
        reply_id: &str,
        note: Option<&str>,
    ) -> ApiResult<()> {
        let settings: Settings = serde_json::from_str(&row.settings).unwrap_or_default();
        let service = self.resolve_model_service(&row.service).await;
        let mut idx = 0u64;
        let say = |text: String, idx: u64| json!({"op": "stream_append", "rid": 6, "msg_id": reply_id, "idx": idx, "text": text});
        let service = match service {
            Ok(s) => s,
            Err(e) => {
                self.call_as(sess, say(settings.fallback.clone(), 0)).await;
                self.call_as(
                    sess,
                    json!({"op": "stream_end", "rid": 7, "msg_id": reply_id}),
                )
                .await;
                return Err(e);
            }
        };
        let credit = if service.builtin {
            self.builtin_credit(service.endpoint.kind.as_str()).await?
        } else {
            None
        };
        if let Some(why) = credit {
            self.call_as(sess, say(why.to_string(), 0)).await;
            self.call_as(
                sess,
                json!({"op": "stream_end", "rid": 7, "msg_id": reply_id}),
            )
            .await;
            return Ok(());
        }
        if let Some(why) = self
            .assistant_allowed(row, &settings, service.daily_tokens)
            .await?
        {
            self.call_as(sess, say(why.to_string(), 0)).await;
            self.call_as(
                sess,
                json!({"op": "stream_end", "rid": 7, "msg_id": reply_id}),
            )
            .await;
            return Ok(());
        }
        let question = msg["content"].as_str().unwrap_or_default();
        let found = match (note, self.knowledge_index(&row.agent_id).await?) {
            (None, Some(i)) => i.search(question, super::knowledge::TOP),
            _ => vec![],
        };
        if note.is_none() && settings.only_knowledge && found.is_empty() {
            let mut text = "I don't have information about that in my documents.".to_string();
            if settings.handoff {
                text.push_str(" I'm passing this to a colleague.");
            }
            self.call_as(sess, say(text, 0)).await;
            self.call_as(
                sess,
                json!({"op": "stream_end", "rid": 7, "msg_id": reply_id}),
            )
            .await;
            if settings.handoff {
                self.call_as(
                    sess,
                    json!({"op": "handoff", "rid": 8, "conv_id": conv_id, "note": "The question is not covered by my documents."}),
                )
                .await;
            }
            return Ok(());
        }
        let mut req = self
            .assistant_request(&sess.user, row, &settings, conv_id, msg)
            .await?;
        if !found.is_empty() {
            let mut k = String::from(
                "\n\nDocuments you may use (cite a document by its name in brackets, e.g. [faq.md], when you use it):\n",
            );
            for (name, piece, _) in &found {
                k.push_str(&format!(
                    "\n<document name=\"{name}\">\n{piece}\n</document>\n"
                ));
            }
            if settings.only_knowledge {
                k.push_str("\nAnswer only from these documents. If they don't contain the answer, say you don't know.");
            }
            req.system.push_str(&k);
        }
        // Text arrives on a channel and is streamed into the reply as it
        // comes; the model call runs alongside.
        if let Some(n) = note {
            req.system.push_str("\n\n");
            req.system.push_str(n);
        }
        let tools = self.assistant_tools(&sess.user, &settings).await?;
        req.tools = tools.defs.clone();
        req.system.push_str(&tools.prompt);
        let asking = Asking {
            conv_id,
            msg,
            reply_id,
            handoff: std::sync::Mutex::new(None),
        };
        let mut usage = Usage::default();
        let mut buf = String::new();
        let mut sent_any = false;
        let mut tail = None;
        for round in 0..MAX_ROUNDS {
            // Text arrives on a channel and is streamed into the reply as
            // it comes; the model call runs alongside.
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
            let endpoint = service.endpoint.clone();
            let r = req.clone();
            let call = tokio::spawn(async move {
                let mut on_text = |t: &str| {
                    let _ = tx.send(t.to_string());
                };
                endpoint.turn(&r, &mut on_text).await
            });
            while let Some(t) = rx.recv().await {
                buf.push_str(&t);
                if buf.len() >= CHUNK {
                    self.call_as(sess, say(std::mem::take(&mut buf), idx)).await;
                    idx += 1;
                    sent_any = true;
                }
            }
            let turn = call
                .await
                .map_err(|e| ApiError::new("internal", e.to_string()))?;
            match turn {
                Ok(t) => {
                    usage.add(t.usage);
                    if t.stop == Stop::Tools && !t.calls.is_empty() && round + 1 < MAX_ROUNDS {
                        req.items.push(t.item());
                        let results = self.run_tools(sess, &tools, &t.calls, &asking).await;
                        req.items.push(Item::Results(results));
                        continue;
                    }
                    let said = sent_any || !buf.trim().is_empty();
                    tail = match t.stop {
                        Stop::Refusal if !said => Some("I can't help with that.".to_string()),
                        _ if !said => Some(settings.fallback.clone()),
                        Stop::End | Stop::Tools | Stop::MaxTokens | Stop::Refusal => None,
                    };
                }
                Err(e) => {
                    tracing::warn!(agent = row.agent_id, service = row.service, error = %e, "model call failed");
                    let t = match e {
                        AiError::RateLimited(_) => {
                            "I'm busy right now. Please ask again in a moment.".to_string()
                        }
                        AiError::Auth(_) | AiError::Unavailable(_) | AiError::Bad(_) => {
                            settings.fallback.clone()
                        }
                    };
                    tail = Some(if sent_any || !buf.is_empty() {
                        format!("\n\n{t}")
                    } else {
                        t
                    });
                }
            }
            break;
        }
        if let Some(t) = tail {
            buf.push_str(&t);
        }
        if !buf.is_empty() {
            self.call_as(sess, say(buf, idx)).await;
        }
        self.call_as(
            sess,
            json!({"op": "stream_end", "rid": 7, "msg_id": reply_id}),
        )
        .await;
        // Handing over ends the agent's open replies, so only after this one.
        let handoff = asking.handoff.lock().unwrap().take();
        if let Some(note) = handoff {
            let r = self
                .call_as(
                    sess,
                    json!({"op": "handoff", "rid": 11, "conv_id": conv_id, "note": note}),
                )
                .await;
            if r["ok"] != true {
                tracing::warn!(agent = row.agent_id, conv = conv_id, error = %r["error"], "hand-off refused");
            }
        }
        self.record_usage(
            row,
            service.builtin,
            service.endpoint.kind.as_str(),
            usage,
            conv_id,
            msg_id,
        )
        .await
    }

    async fn record_usage(
        &self,
        row: &AssistantRow,
        builtin: bool,
        provider: &str,
        u: Usage,
        conv_id: &str,
        msg_id: &str,
    ) -> ApiResult<()> {
        let now = now_ms();
        let cost = self
            .prices()
            .cost(provider, &row.model, &u, now)
            .unwrap_or(0);
        let r = UsageRow {
            id: format!("u_{}", crate::db::new_id()),
            at: now,
            day: day_of(now),
            agent_id: row.agent_id.clone(),
            service: row.service.clone(),
            model: row.model.clone(),
            builtin,
            input: u.input as i64,
            output: u.output as i64,
            cache_read: u.cache_read as i64,
            cache_write: u.cache_write as i64,
            cost_nanos: cost,
            conv_id: conv_id.to_string(),
            msg_id: msg_id.to_string(),
        };
        self.control().call(Op::Admin(AdminOp::AiUsage(r))).await?;
        Ok(())
    }

    /// The instructions (the agent's role) and the conversation so far as
    /// one transcript, ending with the message to answer.
    async fn assistant_request(
        &self,
        me: &User,
        row: &AssistantRow,
        settings: &Settings,
        conv_id: &str,
        msg: &Value,
    ) -> ApiResult<Request> {
        let config = self.agent_config(me).await?;
        let instructions = config["role"]["instructions"]
            .as_str()
            .unwrap_or("You are a helpful assistant.");
        let system = format!(
            "{instructions}\n\nYou are {name}, an AI assistant taking part in a chat. Messages are shown as \"Name: text\". Reply only with your answer to the last message, without a name prefix.",
            name = me.name
        );
        let ctx = self
            .context(
                me,
                conv_id,
                Some(settings.context_tokens),
                msg["thread_id"].as_str(),
            )
            .await?;
        let mut lines = vec![];
        if let Some(s) = ctx["summary"]["text"].as_str() {
            lines.push(format!("(Summary of earlier messages: {s})"));
        }
        let target = msg["msg_id"].as_str().unwrap_or_default();
        // Each message as it is now (a streamed reply shows up again when
        // it is finished), in the order they were posted.
        let mut order: Vec<String> = vec![];
        let mut latest: HashMap<String, Value> = HashMap::new();
        for e in ctx["events"].as_array().into_iter().flatten() {
            let m = &e["event"]["msg"];
            let Some(id) = m["msg_id"].as_str() else {
                continue;
            };
            if id == target {
                continue;
            }
            if !latest.contains_key(id) {
                order.push(id.to_string());
            }
            latest.insert(id.to_string(), m.clone());
        }
        for id in order {
            let m = &latest[&id];
            let text = m["content"].as_str().unwrap_or_default();
            if text.trim().is_empty() || m["state"] == "recalled" {
                continue;
            }
            lines.push(format!("{}: {}", self.speaker(m), text));
        }
        let question = format!(
            "{}: {}",
            self.speaker(msg),
            msg["content"].as_str().unwrap_or_default()
        );
        let user = if lines.is_empty() {
            question
        } else {
            format!(
                "The conversation so far:\n{}\n\nAnswer this message:\n{question}",
                lines.join("\n")
            )
        };
        Ok(Request {
            model: row.model.clone(),
            system,
            items: vec![Item::User(user)],
            tools: vec![],
            max_tokens: settings.max_tokens,
        })
    }

    fn speaker(&self, m: &Value) -> String {
        if let Some(n) = m["sender_name"].as_str() {
            return n.to_string();
        }
        let id = m["sender_id"].as_str().unwrap_or_default();
        self.inner
            .sh
            .user(id)
            .map(|u| u.name)
            .unwrap_or_else(|| id.to_string())
    }

    /// Model calls since `since` (ms), for the console and billing.
    pub async fn ai_usage(&self, since: i64, limit: i64) -> ApiResult<Vec<UsageRow>> {
        if !(1..=10_000).contains(&limit) {
            return Err(rule!(
                "assistant.usage_limit",
                "bad_request",
                "limit is 1..10000"
            ));
        }
        self.read(self.control(), move |db| {
            Box::pin(async move { db.usage_since(since, limit).await })
        })
        .await
    }

    /// The tools an assistant has: its role's skills (and connections).
    async fn assistant_tools(&self, me: &User, settings: &Settings) -> ApiResult<Tools> {
        let config = self.agent_config(me).await?;
        let skills: Vec<Value> = config["skills"].as_array().cloned().unwrap_or_default();
        let names = |k: &str| -> Vec<String> {
            config[k]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|c| c["name"].as_str().map(String::from))
                .collect()
        };
        let bound = self.bound_tools(&names("mcp"), &names("apis")).await?;
        let mut t = Tools {
            defs: vec![],
            prompt: String::new(),
            skills: vec![],
            bound: HashMap::new(),
            steps: settings.steps,
        };
        let mut keys: Vec<&String> = bound.keys().collect();
        keys.sort();
        for k in keys {
            let b = &bound[k];
            let mut description = b.tool.description.clone();
            if b.confirm || b.rules.iter().any(|r| r.confirm != Some(false)) {
                description.push_str(" (A person may have to approve a call before it runs.)");
            }
            t.defs.push(ToolDef {
                name: k.clone(),
                description,
                schema: b.tool.schema.clone(),
            });
        }
        t.bound = bound;
        if !skills.is_empty() {
            t.prompt.push_str(
                "\n\nSkills you can use (load one with load_skill when the question needs it):\n",
            );
            for sk in &skills {
                let name = sk["name"].as_str().unwrap_or_default().to_string();
                t.prompt.push_str(&format!(
                    "- {name}: {}\n",
                    sk["description"].as_str().unwrap_or_default()
                ));
                t.skills.push(name);
            }
            t.defs.push(ToolDef {
                name: "load_skill".into(),
                description: "Loads a skill's instructions and lists its reference files.".into(),
                schema: json!({"type": "object", "properties": {"name": {"type": "string", "description": "The skill's name"}}, "required": ["name"]}),
            });
            t.defs.push(ToolDef {
                name: "read_skill_file".into(),
                description: "Reads one of a skill's reference files.".into(),
                schema: json!({"type": "object", "properties": {"name": {"type": "string"}, "path": {"type": "string"}}, "required": ["name", "path"]}),
            });
        }
        // Data as a card, questions as a form (docs/en/design/chat-components.md).
        if settings.cards {
            t.defs.push(ToolDef {
                name: SHOW_CARD_TOOL.into(),
                description: "Shows data to the user as a card instead of a long text: an order's details (facts), several records (table) or a short list. The user sees it right away; afterwards add at most one short sentence and don't repeat what the card shows.".into(),
                schema: json!({"type": "object", "properties": {
                    "title": {"type": "string", "description": "A short title, e.g. Order 1002"},
                    "blocks": {"type": "array", "description": "1 to 5 blocks", "items": {"type": "object", "properties": {
                        "type": {"type": "string", "enum": ["text", "facts", "table", "list"]},
                        "text": {"type": "string", "description": "text: the text"},
                        "items": {"type": "array", "description": "facts: [{label, value, format?, tone?}]; list: [{id, title, subtitle?, meta?, badge?: {label, tone?}}]", "items": {"type": "object"}},
                        "columns": {"type": "array", "description": "table: [{label, align?: start|center|end, format?}]", "items": {"type": "object"}},
                        "rows": {"type": "array", "description": "table: one array of values per row", "items": {"type": "array"}}
                    }, "required": ["type"]}}
                }, "required": ["blocks"]}),
            });
            t.prompt.push_str("\n\nValues in cards: format is text, number, currency:USD (any ISO code), date (YYYY-MM-DD), datetime or percent; tone is neutral, success, warning, danger or info. The user's app formats them for their language.");
        }
        if settings.forms {
            t.defs.push(ToolDef {
                name: ASK_FORM_TOOL.into(),
                description: "Asks the user for several details at once with a form card (e.g. a refund reason and a pickup date) instead of asking one question at a time. Only the user you are answering can fill it in. You continue once they send it; until then say briefly that the form is waiting.".into(),
                schema: json!({"type": "object", "properties": {
                    "title": {"type": "string", "description": "What the form is for"},
                    "intro": {"type": "string", "description": "One sentence above the fields (optional)"},
                    "submit": {"type": "string", "description": "The button's label (default Send)"},
                    "fields": {"type": "array", "description": "1 to 10 fields", "items": {"type": "object", "properties": {
                        "id": {"type": "string", "description": "a-z 0-9 _ -"},
                        "label": {"type": "string"},
                        "type": {"type": "string", "enum": ["text", "textarea", "number", "select", "checkbox", "date", "time", "email"]},
                        "required": {"type": "boolean"},
                        "options": {"type": "array", "items": {"type": "string"}, "description": "select: the choices"},
                        "placeholder": {"type": "string"},
                        "hint": {"type": "string"},
                        "min": {"description": "number, date (YYYY-MM-DD) or time (HH:MM)"},
                        "max": {"description": "number, date or time; text: most characters"}
                    }, "required": ["id", "label", "type"]}}
                }, "required": ["title", "fields"]}),
            });
        }
        // Handing over to a person when asked or stuck (docs/en/design/assistants.md).
        if settings.handoff {
            t.defs.push(ToolDef {
                name: HANDOFF_TOOL.into(),
                description: "Hands the conversation to a person on the team: when the user asks for a person, or you cannot help. The note tells them what it is about.".into(),
                schema: json!({"type": "object", "properties": {"note": {"type": "string", "description": "What the person should know"}}, "required": ["note"]}),
            });
        }
        Ok(t)
    }

    async fn run_tools(
        &self,
        sess: &super::Session,
        tools: &Tools,
        calls: &[ToolCall],
        asking: &Asking<'_>,
    ) -> Vec<ToolResult> {
        let mut out = vec![];
        for c in calls {
            let title = self.step_title(tools, &c.name);
            if tools.steps {
                self.step(
                    sess,
                    asking.conv_id,
                    asking.reply_id,
                    &c.id,
                    &title,
                    "running",
                    None,
                )
                .await;
            }
            let (content, is_error) = match self.run_tool(sess, tools, c, asking).await {
                Ok(text) => (text, false),
                Err(text) => (text, true),
            };
            if tools.steps {
                let (state, detail) = if is_error {
                    ("failed", Some("It didn't work"))
                } else if content.starts_with("Not done yet") {
                    ("waiting", Some("Waiting for approval"))
                } else if content.starts_with(FORM_SENT) {
                    ("waiting", Some("Waiting for your answers"))
                } else {
                    ("done", None)
                };
                self.step(
                    sess,
                    asking.conv_id,
                    asking.reply_id,
                    &c.id,
                    &title,
                    state,
                    detail,
                )
                .await;
            }
            out.push(ToolResult {
                call_id: c.id.clone(),
                content,
                is_error,
            });
        }
        out
    }

    async fn run_tool(
        &self,
        sess: &super::Session,
        tools: &Tools,
        c: &ToolCall,
        asking: &Asking<'_>,
    ) -> Result<String, String> {
        let arg = |k: &str| {
            c.input[k]
                .as_str()
                .map(str::to_string)
                .ok_or_else(|| format!("{k} is required"))
        };
        match c.name.as_str() {
            "load_skill" => {
                let name = arg("name")?;
                if !tools.skills.contains(&name) {
                    return Err(format!("no skill named {name}"));
                }
                let s = self
                    .skills_named(std::slice::from_ref(&name))
                    .await
                    .map_err(|e| e.message)?;
                let s = s.first().ok_or("the skill is gone")?;
                let files: Vec<&str> = s["files"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|f| f.as_str())
                    .collect();
                Ok(format!(
                    "{}\n\nReference files: {}",
                    s["instructions"].as_str().unwrap_or_default(),
                    if files.is_empty() {
                        "none".to_string()
                    } else {
                        files.join(", ")
                    }
                ))
            }
            "read_skill_file" => {
                let (name, path) = (arg("name")?, arg("path")?);
                if !tools.skills.contains(&name) {
                    return Err(format!("no skill named {name}"));
                }
                self.skill_file_text(&name, &path)
                    .await
                    .map_err(|e| e.message)?
                    .ok_or_else(|| format!("{name} has no file {path}"))
            }
            SHOW_CARD_TOOL if tools.defs.iter().any(|d| d.name == SHOW_CARD_TOOL) => {
                self.show_card(sess, c, asking).await
            }
            ASK_FORM_TOOL if tools.defs.iter().any(|d| d.name == ASK_FORM_TOOL) => {
                self.ask_form(sess, c, asking).await
            }
            HANDOFF_TOOL if tools.defs.iter().any(|d| d.name == HANDOFF_TOOL) => {
                let mut note = arg("note")?;
                note.truncate(note.floor_char_boundary(1_000));
                // The owner takes it; they must be in the conversation.
                let owner = sess
                    .user
                    .owner_id
                    .as_deref()
                    .and_then(|id| self.inner.sh.user(id))
                    .map(|u| u.name);
                let present = match (owner, self.conv_detail(asking.conv_id).await) {
                    (Some(o), Ok(d)) => d["members"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .any(|m| m["user"] == o.as_str()),
                    _ => false,
                };
                if !present {
                    return Err(
                        "could not hand over: no person of the team is in this conversation".into(),
                    );
                }
                // Done when this reply ends (handing over ends it).
                *asking.handoff.lock().unwrap() = Some(note);
                Ok("A person on the team takes the conversation over as soon as you finish this reply. Tell the user so briefly.".into())
            }
            other => match tools.bound.get(other) {
                Some(b) => match b.gate(&c.input) {
                    super::connections::Gate::Run => self.call_bound(b, &c.input, None).await,
                    super::connections::Gate::Ask { approvers, rule } => {
                        self.ask_to_run(sess, b, other, c, asking, approvers, rule)
                            .await
                    }
                },
                None => Err(format!("unknown tool {other}")),
            },
        }
    }

    /// A call that needs approval (docs/en/design/assistants.md): instead of calling it,
    /// asks the people a rule names (those in the conversation), else the
    /// person being answered, else the agent's owner; the call itself and
    /// what the card shows are kept in the request.
    /// A step's title: the tool's summary, never its arguments.
    fn step_title(&self, tools: &Tools, name: &str) -> String {
        let t = match name {
            "load_skill" | "read_skill_file" => "Read the instructions".to_string(),
            SHOW_CARD_TOOL => "Show the details".to_string(),
            ASK_FORM_TOOL => "Ask for details".to_string(),
            HANDOFF_TOOL => "Hand over to a person".to_string(),
            other => tools
                .bound
                .get(other)
                .map(|b| {
                    let d = b.tool.description.trim();
                    let first = d.split(['.', '\n']).next().unwrap_or(d);
                    // Not the HTTP route an OpenAPI tool's description ends with.
                    let first = first.split(" (").next().unwrap_or(first).trim();
                    if first.is_empty() {
                        b.tool.name.clone()
                    } else {
                        first.to_string()
                    }
                })
                .unwrap_or_else(|| other.to_string()),
        };
        let mut t = t;
        if t.len() > 80 {
            let mut end = 77;
            while !t.is_char_boundary(end) {
                end -= 1;
            }
            t.truncate(end);
            t.push('…');
        }
        t
    }

    /// One step of a reply (docs/en/design/chat-components.md): a `progress` event on the side.
    #[allow(clippy::too_many_arguments)]
    async fn step(
        &self,
        sess: &super::Session,
        conv_id: &str,
        reply_id: &str,
        id: &str,
        title: &str,
        state: &str,
        detail: Option<&str>,
    ) {
        let mut data = json!({"id": id, "state": state, "title": title});
        if let Some(d) = detail {
            data["detail"] = json!(d);
        }
        let r = self
            .call_as(sess, json!({"op": "progress", "rid": 12, "conv_id": conv_id, "msg_id": reply_id, "kind": "step", "text": title, "data": data}))
            .await;
        if r["ok"] != true {
            tracing::debug!(conv_id, error = %r["error"], "step not shown");
        }
    }

    /// Sends, waiting when the server slows the agent down (as replies do).
    async fn send_patiently(&self, sess: &super::Session, v: Value) -> Value {
        let mut wait = 500u64;
        loop {
            let r = self.call_as(sess, v.clone()).await;
            if r["error"]["code"] != "rate_limited" || wait > 16_000 {
                return r;
            }
            tokio::time::sleep(Duration::from_millis(wait)).await;
            wait *= 2;
        }
    }

    /// `show_card`: a display-only card (facts, table, list, text).
    async fn show_card(
        &self,
        sess: &super::Session,
        c: &ToolCall,
        asking: &Asking<'_>,
    ) -> Result<String, String> {
        let blocks = c.input["blocks"].as_array().cloned().unwrap_or_default();
        if blocks.is_empty() || blocks.len() > 5 {
            return Err("blocks: 1 to 5".into());
        }
        for b in &blocks {
            match b["type"].as_str() {
                Some("text" | "facts" | "table") => {}
                Some("list") if b["select"] != true => {}
                _ => {
                    return Err(
                        "show_card shows text, facts, table or list blocks (use ask_form to ask)"
                            .into(),
                    );
                }
            }
        }
        let mut card = json!({"blocks": blocks});
        let title = c.input["title"].as_str().map(str::to_string);
        if let Some(t) = &title {
            card["title"] = json!(t);
        }
        crate::logic::cards::check_card(&card).map_err(|e| e.message)?;
        let msg_id = asking.msg["msg_id"].as_str().unwrap_or_default();
        let r = self
            .send_patiently(
                sess,
                json!({"op": "send", "rid": 13, "conv_id": asking.conv_id,
                "client_msg_id": format!("ai-card-{msg_id}-{}", c.id), "reply_to": msg_id, "thread_id": asking.msg["thread_id"],
                "kind": crate::logic::cards::KIND, "payload": card,
                "text": title.unwrap_or_else(|| "Details".into())}),
            )
            .await;
        if r["ok"] != true {
            return Err(format!(
                "could not show the card: {}",
                r["error"]["message"].as_str().unwrap_or("refused")
            ));
        }
        Ok(
            "The user sees the card now. Don't repeat its content; add at most one short sentence."
                .into(),
        )
    }

    /// `ask_form`: a form card only the person being answered may fill in;
    /// their answers start the next reply (`card.response`).
    async fn ask_form(
        &self,
        sess: &super::Session,
        c: &ToolCall,
        asking: &Asking<'_>,
    ) -> Result<String, String> {
        let asker = asking.msg["sender_id"]
            .as_str()
            .and_then(|id| self.inner.sh.user(id))
            .filter(|u| u.kind == UserKind::Human)
            .ok_or("only a person can fill in a form")?;
        let title = c.input["title"]
            .as_str()
            .filter(|t| !t.trim().is_empty())
            .ok_or("title is required")?;
        let mut blocks = vec![];
        if let Some(i) = c.input["intro"].as_str().filter(|i| !i.trim().is_empty()) {
            blocks.push(json!({"type": "text", "text": i}));
        }
        // The model's fields, kept to what a form field takes.
        let keep = [
            "id",
            "label",
            "type",
            "required",
            "options",
            "placeholder",
            "hint",
            "min",
            "max",
            "default",
        ];
        let fields: Vec<Value> = c.input["fields"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|f| {
                let mut o = serde_json::Map::new();
                for k in keep {
                    if let Some(v) = f.get(k).filter(|v| !v.is_null()) {
                        o.insert(k.into(), v.clone());
                    }
                }
                Value::Object(o)
            })
            .collect();
        let submit = c.input["submit"]
            .as_str()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or("Send");
        blocks.push(json!({"type": "form", "id": "form", "submit": submit, "fields": fields}));
        let card = json!({"title": title, "to": [asker.id], "once": true, "blocks": blocks});
        crate::logic::cards::check_card(&card)
            .map_err(|e| format!("the form is not valid: {}", e.message))?;
        let msg_id = asking.msg["msg_id"].as_str().unwrap_or_default();
        let r = self
            .send_patiently(
                sess,
                json!({"op": "send", "rid": 14, "conv_id": asking.conv_id,
                // The reply and the step it belongs to: the step is done when the answers come.
                "client_msg_id": format!("ai-form-{}-{}", asking.reply_id, c.id), "reply_to": msg_id, "thread_id": asking.msg["thread_id"],
                "kind": crate::logic::cards::KIND, "payload": card,
                "text": format!("{title} (please fill in the form)")}),
            )
            .await;
        if r["ok"] != true {
            return Err(format!(
                "could not send the form: {}",
                r["error"]["message"].as_str().unwrap_or("refused")
            ));
        }
        Ok(format!(
            "{FORM_SENT} {} sees the form \"{title}\" now. You continue when they send it. Say in one short sentence that the form is waiting; don't ask the same questions in text.",
            asker.name
        ))
    }

    /// The person sent a form this assistant asked with: the next reply
    /// goes on from their answers (once, whatever is delivered twice).
    async fn after_form(
        &self,
        sess: &super::Session,
        row: &AssistantRow,
        conv_id: &str,
        data: &Value,
    ) -> ApiResult<()> {
        let (Some(card_id), Some(by)) = (data["msg_id"].as_str(), data["by"].as_str()) else {
            return Ok(());
        };
        let id = card_id.to_string();
        let card = self
            .read(self.conv_shard(conv_id)?, move |db| {
                Box::pin(async move { db.message(&id).await })
            })
            .await?;
        let Some(card) =
            card.filter(|m| m.sender_id == sess.user.id && m.kind == crate::logic::cards::KIND)
        else {
            return Ok(());
        };
        let payload = card.payload.clone().unwrap_or(Value::Null);
        let Some(form) = payload["blocks"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|b| b["type"] == "form" && b["id"] == data["block_id"])
        else {
            return Ok(());
        };
        let Some(reply_id) = self
            .claim_reply(
                sess,
                conv_id,
                &format!("ai-f-{card_id}-{by}"),
                card_id,
                card.thread_id.as_deref(),
            )
            .await?
        else {
            return Ok(());
        };
        // The step that waited for the answers (docs/en/design/chat-components.md).
        let settings: Settings = serde_json::from_str(&row.settings).unwrap_or_default();
        let (c, seq) = (conv_id.to_string(), card.created_seq);
        let own_id = self
            .read(self.conv_shard(conv_id)?, move |db| {
                Box::pin(async move { db.client_msg_id_at(&c, seq).await })
            })
            .await
            .ok()
            .flatten();
        if settings.steps
            && let Some((step_reply, step)) = own_id
                .as_deref()
                .and_then(|c| c.strip_prefix("ai-form-"))
                .and_then(|r| r.split_once('-'))
        {
            self.step(
                sess,
                conv_id,
                step_reply,
                step,
                "Ask for details",
                "done",
                None,
            )
            .await;
        }
        let mut lines = vec![];
        for f in form["fields"].as_array().into_iter().flatten() {
            let v = &data["values"][f["id"].as_str().unwrap_or_default()];
            let shown = match v {
                Value::Null => "(left empty)".to_string(),
                Value::String(s) => s.clone(),
                other
                @ (Value::Bool(_) | Value::Number(_) | Value::Array(_) | Value::Object(_)) => {
                    other.to_string()
                }
            };
            lines.push(format!(
                "- {}: {shown}",
                f["label"].as_str().unwrap_or_default()
            ));
        }
        let title = payload["title"].as_str().unwrap_or("the form");
        let who = self
            .inner
            .sh
            .user(by)
            .map(|u| u.name)
            .unwrap_or_else(|| "The user".into());
        let content = format!("(filled in the form \"{title}\")\n{}", lines.join("\n"));
        let msg =
            json!({"msg_id": card_id, "sender_id": by, "sender_name": who, "content": content});
        let note = format!(
            "{who} filled in the form \"{title}\" you asked with:\n{}\nGo on with their request using these answers.",
            lines.join("\n")
        );
        self.respond(sess, row, conv_id, card_id, &msg, &reply_id, Some(&note))
            .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn ask_to_run(
        &self,
        sess: &super::Session,
        b: &super::connections::Bound,
        name: &str,
        c: &ToolCall,
        asking: &Asking<'_>,
        approvers: Option<Vec<String>>,
        rule: Option<super::connections::ApprovalRule>,
    ) -> Result<String, String> {
        let msg = asking.msg;
        let msg_id = msg["msg_id"].as_str().unwrap_or_default();
        let members: Vec<String> = match self.conv_detail(asking.conv_id).await {
            Ok(d) => d["members"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|m| m["user"].as_str().map(String::from))
                .collect(),
            Err(_) => vec![],
        };
        let named: Vec<User> = approvers
            .iter()
            .flatten()
            .filter_map(|id| self.inner.sh.user(id))
            .filter(|u| members.contains(&u.name))
            .collect();
        let asker = msg["sender_id"]
            .as_str()
            .and_then(|id| self.inner.sh.user(id))
            .filter(|u| u.kind == UserKind::Human);
        // A rule names its approvers; the person asking does not decide then.
        let deciders: Vec<User> = match &approvers {
            Some(_) => named,
            None => asker.into_iter().collect(),
        };
        let amount = rule
            .as_ref()
            .and_then(|r| r.amount_field.as_ref())
            .map(|f| c.input.get(f).cloned().unwrap_or(Value::Null));
        // The card: the operation and its plain arguments.
        let fields: Vec<Value> = c
            .input
            .as_object()
            .into_iter()
            .flatten()
            .filter(|(_, v)| v.is_string() || v.is_number() || v.is_boolean())
            .take(10)
            .map(|(k, v)| json!({"name": k, "value": v}))
            .collect();
        let mut args = serde_json::to_string_pretty(&c.input).unwrap_or_default();
        if args.len() > 2_000 {
            let mut end = 2_000;
            while !args.is_char_boundary(end) {
                end -= 1;
            }
            args.truncate(end);
            args.push('…');
        }
        let mut content = msg["content"].as_str().unwrap_or_default().to_string();
        if content.len() > 8_000 {
            let mut end = 8_000;
            while !content.is_char_boundary(end) {
                end -= 1;
            }
            content.truncate(end);
        }
        let mut v = json!({
            "op": "request_human", "rid": 9, "conv_id": asking.conv_id, "kind": "approve",
            "title": format!("{} wants to run {} on {}:\n{args}", sess.user.name, b.tool.name, b.conn),
            "context": {"tool_call": {"tool": name, "connection": b.conn, "args": c.input,
                "msg": {"msg_id": msg_id, "sender_id": msg["sender_id"], "sender_name": self.speaker(msg), "content": content},
                "step": {"id": c.id, "reply": asking.reply_id}},
                "operation": {"name": b.tool.name, "description": b.tool.description, "connection": b.conn,
                    "fields": fields, "amount": amount, "rule": rule}},
            "ttl_secs": TOOL_APPROVAL_SECS,
            "client_req_id": format!("tool-{msg_id}-{}", c.id),
        });
        if !deciders.is_empty() {
            v["assignees"] = json!(deciders.iter().map(|u| u.id.clone()).collect::<Vec<_>>());
        }
        let r = self.call_as(sess, v).await;
        if r["ok"] != true {
            return Err(format!(
                "could not ask for approval: {}",
                r["error"]["message"].as_str().unwrap_or("refused")
            ));
        }
        let who = if deciders.is_empty() {
            "the agent's owner".to_string()
        } else {
            deciders
                .iter()
                .map(|u| u.name.clone())
                .collect::<Vec<_>>()
                .join(", ")
        };
        let mine = deciders
            .iter()
            .any(|u| Some(u.id.as_str()) == msg["sender_id"].as_str());
        Ok(if mine {
            format!(
                "Not done yet: {who} has been asked to approve this call, and it runs only after they approve. Tell them what will happen once they approve; don't say it is done."
            )
        } else {
            format!(
                "Not done yet: this needs approval from {who} (not the person you are talking to); it runs only after they approve. Tell the person it is waiting for approval; don't say it is done."
            )
        })
    }

    /// A request about one of its tool calls was decided, expired or
    /// cancelled: runs the approved call once, then tells the person.
    async fn assistant_side(&self, sess: &super::Session, row: &AssistantRow, ev: &Value) {
        if !matches!(
            ev["type"].as_str(),
            Some("request.decided" | "request.expired" | "request.cancelled")
        ) {
            return;
        }
        let (Some(conv_id), Some(rid)) =
            (ev["conv_id"].as_str(), ev["data"]["request_id"].as_str())
        else {
            return;
        };
        let q = self
            .call_as(
                sess,
                json!({"op": "get_request", "rid": 10, "request_id": rid}),
            )
            .await;
        if q["ok"] != true || q["agent_id"] != sess.user.id.as_str() {
            return;
        }
        let call = &q["context"]["tool_call"];
        if call["tool"].as_str().is_none() {
            return;
        }
        if let Err(e) = self.after_decision(sess, row, conv_id, rid, &q, call).await {
            tracing::warn!(
                agent = sess.user.id,
                conv_id,
                request = rid,
                error = e.message,
                "approved tool call"
            );
        }
    }

    async fn after_decision(
        &self,
        sess: &super::Session,
        row: &AssistantRow,
        conv_id: &str,
        rid: &str,
        q: &Value,
        call: &Value,
    ) -> ApiResult<()> {
        let msg = &call["msg"];
        let msg_id = msg["msg_id"].as_str().unwrap_or_default();
        // The reply is claimed before anything runs: a second delivery of
        // the decision finds it taken, so the call runs at most once.
        let Some(reply_id) = self
            .claim_reply(
                sess,
                conv_id,
                &format!("ai-q-{rid}"),
                msg_id,
                msg["thread_id"].as_str(),
            )
            .await?
        else {
            return Ok(());
        };
        let tool = call["tool"].as_str().unwrap_or_default();
        let who = q["decided_by"]
            .as_str()
            .and_then(|id| self.inner.sh.user(id))
            .map(|u| u.name)
            .unwrap_or_else(|| "The person".into());
        // The step that waited on this decision (docs/en/design/chat-components.md).
        let settings: Settings = serde_json::from_str(&row.settings).unwrap_or_default();
        if settings.steps
            && let (Some(step), Some(reply)) =
                (call["step"]["id"].as_str(), call["step"]["reply"].as_str())
        {
            let (state, detail) = match q["state"].as_str() {
                Some("approved") => ("done", format!("Approved by {who}")),
                Some("rejected") => ("skipped", format!("Declined by {who}")),
                Some("expired") => ("skipped", "The approval expired".to_string()),
                _ => ("skipped", "Cancelled".to_string()),
            };
            let tools = self.assistant_tools(&sess.user, &settings).await?;
            let title = self.step_title(&tools, tool);
            self.step(sess, conv_id, reply, step, &title, state, Some(&detail))
                .await;
        }
        let note = match q["state"].as_str() {
            Some("approved") => {
                let settings: Settings = serde_json::from_str(&row.settings).unwrap_or_default();
                let tools = self.assistant_tools(&sess.user, &settings).await?;
                let outcome = match tools
                    .bound
                    .get(tool)
                    .filter(|b| call["connection"] == b.conn.as_str())
                {
                    None => {
                        self.audit_user(
                            &sess.user,
                            "tool.not_run",
                            rid,
                            json!({"tool": tool, "why": "unavailable"}),
                        )
                        .await;
                        "it was not run: that tool is no longer available to you.".to_string()
                    }
                    Some(b) => match self.call_bound(b, &call["args"], Some(rid)).await {
                        Ok(text) => {
                            self.audit_user(
                                &sess.user,
                                "tool.called",
                                rid,
                                json!({"tool": tool, "connection": b.conn, "ok": true}),
                            )
                            .await;
                            format!("it ran. The result:\n{text}")
                        }
                        Err(e) => {
                            self.audit_user(
                                &sess.user,
                                "tool.called",
                                rid,
                                json!({"tool": tool, "connection": b.conn, "ok": false}),
                            )
                            .await;
                            format!("it failed: {e}")
                        }
                    },
                };
                format!(
                    "Earlier you asked to run the tool {tool}. {who} approved it and {outcome}\nTell them the outcome now."
                )
            }
            Some("rejected") => {
                let reason = q["response"]["text"]
                    .as_str()
                    .map(|r| format!(" Their reason: {r}"))
                    .unwrap_or_default();
                format!(
                    "Earlier you asked to run the tool {tool}. {who} declined, so it was not run.{reason}\nAcknowledge this; don't try it again unless they ask."
                )
            }
            state => {
                let what = if state == Some("expired") {
                    "expired"
                } else {
                    "was cancelled"
                };
                let text = format!("The approval to run {tool} {what}, so I didn't do it.");
                self.call_as(sess, json!({"op": "stream_append", "rid": 6, "msg_id": reply_id, "idx": 0, "text": text}))
                    .await;
                self.call_as(
                    sess,
                    json!({"op": "stream_end", "rid": 7, "msg_id": reply_id}),
                )
                .await;
                return Ok(());
            }
        };
        self.respond(sess, row, conv_id, msg_id, msg, &reply_id, Some(&note))
            .await
    }
}

/// Shows data as a card (docs/en/design/chat-components.md).
const SHOW_CARD_TOOL: &str = "show_card";
/// Asks with a form card (docs/en/design/chat-components.md).
const ASK_FORM_TOOL: &str = "ask_form";
/// How the result of `ask_form` starts (a step waits on it).
const FORM_SENT: &str = "Form sent:";

/// The tool that hands the conversation to a person (docs/en/design/assistants.md).
const HANDOFF_TOOL: &str = "handoff_to_person";

/// How long a person has to approve a tool call (docs/en/design/assistants.md).
const TOOL_APPROVAL_SECS: i64 = 3_600;

/// The message a reply answers, for tools that ask a person.
struct Asking<'a> {
    conv_id: &'a str,
    msg: &'a Value,
    /// The reply being written (steps hang on it).
    reply_id: &'a str,
    /// A hand-off asked for during the reply (its note), done after it.
    handoff: std::sync::Mutex<Option<String>>,
}

/// What an assistant can call during a reply.
pub(super) struct Tools {
    defs: Vec<ToolDef>,
    prompt: String,
    skills: Vec<String>,
    bound: HashMap<String, super::connections::Bound>,
    /// Report each call as a step (docs/en/design/chat-components.md).
    steps: bool,
}
