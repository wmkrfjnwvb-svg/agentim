//! The single-node message core: send pipeline, fan-out, streaming state
//! machine, side channel, human intervention and guardrails.
//!
//! Locking: every operation takes the store lock, then the runtime lock, and
//! holds both. Pushes are queued while the locks are held, so a session
//! receives events in commit order. Push is still only a hint: clients detect
//! gaps by seq and pull.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use crate::guard::{GuardConfig, Guards, Verdict};
use crate::model::*;
use crate::policy::PolicyChain;
use crate::store::{Committed, NewMsg, Store};

pub const MAX_MESSAGE_BYTES: usize = 64 * 1024;
const MODERATED_PLACEHOLDER: &str = "[该内容已被策略移除 / removed by policy]";

#[derive(Debug, Clone)]
pub struct HubConfig {
    pub stream_idle: Duration,
    pub stream_max: Duration,
    pub stream_max_bytes: usize,
    pub checkpoint_every: Duration,
    pub coalesce: Duration,
    pub guards: GuardConfig,
}

impl Default for HubConfig {
    fn default() -> Self {
        HubConfig {
            stream_idle: Duration::from_secs(30),
            stream_max: Duration::from_secs(600),
            stream_max_bytes: 256 * 1024,
            checkpoint_every: Duration::from_secs(1),
            coalesce: Duration::from_millis(60),
            guards: GuardConfig::default(),
        }
    }
}

struct Session {
    user: User,
    tx: UnboundedSender<Value>,
    /// Foreground conversation (receives stream deltas), optionally observed
    /// through one of the user's agents.
    focus: Option<(String, Option<String>)>,
}

#[derive(Clone)]
enum Target {
    /// Visible stream in the main log (already holds a conv_seq).
    Main,
    /// Approval mode: streamed only to the approvers, becomes a draft.
    Draft { audience: Vec<String> },
}

struct Stream {
    msg_id: String,
    conv_id: String,
    sender: User,
    target: Target,
    buf: String,
    pending: String,
    next_idx: u64,
    started: Instant,
    last_append: Instant,
    checkpointed: usize,
    hop: i64,
    causation_id: Option<String>,
    reply_to: Option<String>,
}

struct Runtime {
    sessions: HashMap<u64, Session>,
    next_session: u64,
    streams: HashMap<String, Stream>,
    guards: Guards,
    last_checkpoint: Instant,
    last_flush: Instant,
}

struct Inner {
    store: Mutex<Store>,
    rt: Mutex<Runtime>,
    policy: PolicyChain,
    cfg: HubConfig,
}

#[derive(Clone)]
pub struct Hub {
    inner: Arc<Inner>,
}

struct Ctx<'a> {
    store: &'a mut Store,
    rt: &'a mut Runtime,
    policy: &'a PolicyChain,
    cfg: &'a HubConfig,
}

fn err(code: &'static str, msg: impl Into<String>) -> ApiError {
    ApiError::new(code, msg)
}

impl Hub {
    pub fn new(store: Store, cfg: HubConfig) -> Hub {
        let rt = Runtime {
            sessions: HashMap::new(),
            next_session: 1,
            streams: HashMap::new(),
            guards: Guards::new(cfg.guards.clone()),
            last_checkpoint: Instant::now(),
            last_flush: Instant::now(),
        };
        let hub = Hub {
            inner: Arc::new(Inner {
                store: Mutex::new(store),
                rt: Mutex::new(rt),
                policy: PolicyChain::default_chain(),
                cfg,
            }),
        };
        hub.recover();
        hub
    }

    fn with<R>(&self, f: impl FnOnce(&mut Ctx) -> R) -> R {
        let mut store = self.inner.store.lock().unwrap_or_else(|e| e.into_inner());
        let mut rt = self.inner.rt.lock().unwrap_or_else(|e| e.into_inner());
        let mut ctx = Ctx {
            store: &mut store,
            rt: &mut rt,
            policy: &self.inner.policy,
            cfg: &self.inner.cfg,
        };
        f(&mut ctx)
    }

    /// Streams interrupted by a crash or restart end as `failed` with the
    /// content of their last checkpoint.
    fn recover(&self) {
        self.with(|c| {
            if let Ok(list) = c.store.streaming_messages() {
                for m in list {
                    let _ = c.store.update_message(
                        &m.msg_id,
                        "msg.updated",
                        &m.sender_id,
                        None,
                        Some("failed"),
                        Some("server_restart"),
                    );
                }
            }
        });
    }

    pub fn store<R>(&self, f: impl FnOnce(&mut Store) -> R) -> R {
        let mut store = self.inner.store.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut store)
    }

    pub fn authenticate(&self, token: &str) -> Option<User> {
        self.store(|s| s.user_by_token(token).ok().flatten())
    }

    pub fn connect(&self, user: User) -> (SessionHandle, UnboundedReceiver<Value>) {
        let (tx, rx) = unbounded_channel();
        let id = self.with(|c| {
            let id = c.rt.next_session;
            c.rt.next_session += 1;
            c.rt.sessions.insert(
                id,
                Session {
                    user: user.clone(),
                    tx,
                    focus: None,
                },
            );
            id
        });
        (
            SessionHandle {
                hub: self.clone(),
                id,
                user,
            },
            rx,
        )
    }

    fn disconnect(&self, id: u64) {
        self.with(|c| {
            let Some(s) = c.rt.sessions.remove(&id) else {
                return;
            };
            if s.user.kind == UserKind::Agent
                && !c.rt.sessions.values().any(|o| o.user.id == s.user.id)
            {
                let ids: Vec<String> =
                    c.rt.streams
                        .values()
                        .filter(|st| st.sender.id == s.user.id)
                        .map(|st| st.msg_id.clone())
                        .collect();
                for mid in ids {
                    c.finish_stream(&mid, "failed", Some("disconnected"), &s.user.id);
                }
            }
        });
    }

    /// Periodic work: coalesced delta flush, checkpoints, stream watchdog.
    pub fn tick(&self) {
        self.with(|c| c.tick());
    }

    pub fn spawn_background(&self) {
        let hub = self.clone();
        let every = (self.inner.cfg.coalesce / 2).max(Duration::from_millis(10));
        tokio::spawn(async move {
            let mut iv = tokio::time::interval(every);
            loop {
                iv.tick().await;
                hub.tick();
            }
        });
    }

    pub fn stats(&self) -> Value {
        self.with(|c| c.stats())
    }
}

pub struct SessionHandle {
    hub: Hub,
    id: u64,
    pub user: User,
}

impl SessionHandle {
    pub fn handle(&self, env: Envelope) -> Value {
        let rid = env.rid;
        let res = self.hub.with(|c| c.dispatch(self.id, &self.user, env.msg));
        match res {
            Ok(mut v) => {
                if !v.is_object() {
                    v = json!({ "value": v });
                }
                v["type"] = json!("res");
                v["rid"] = json!(rid);
                v["ok"] = json!(true);
                v
            }
            Err(e) => {
                json!({"type": "res", "rid": rid, "ok": false, "error": {"code": e.code, "message": e.message}})
            }
        }
    }

    pub fn handle_text(&self, text: &str) -> Value {
        match serde_json::from_str::<Envelope>(text) {
            Ok(env) => self.handle(env),
            Err(e) => {
                let rid = serde_json::from_str::<Value>(text)
                    .ok()
                    .and_then(|v| v["rid"].as_u64());
                json!({"type": "res", "rid": rid, "ok": false, "error": {"code": "bad_request", "message": e.to_string()}})
            }
        }
    }
}

impl Drop for SessionHandle {
    fn drop(&mut self) {
        self.hub.disconnect(self.id);
    }
}

impl Ctx<'_> {
    fn dispatch(&mut self, sid: u64, me: &User, msg: ClientMsg) -> ApiResult<Value> {
        match msg {
            ClientMsg::Hello => self.hello(me),
            ClientMsg::ListConvs => self.list_convs(me),
            ClientMsg::CreateConv {
                kind,
                title,
                members,
            } => self.create_conv(me, &kind, &title, &members),
            ClientMsg::AddMember { conv_id, user_id } => self.add_member(me, &conv_id, &user_id),
            ClientMsg::Send {
                conv_id,
                client_msg_id,
                text,
                reply_to,
                causation_id,
                on_behalf_of,
                to_user,
            } => self.send(
                me,
                &conv_id,
                &client_msg_id,
                &text,
                reply_to,
                causation_id,
                on_behalf_of,
                to_user,
            ),
            ClientMsg::Edit { msg_id, text } => self.edit(me, &msg_id, &text),
            ClientMsg::Recall { msg_id } => self.recall(me, &msg_id),
            ClientMsg::Pull {
                conv_id,
                after_seq,
                limit,
                as_agent,
            } => self.pull(me, &conv_id, after_seq, limit.unwrap_or(200), as_agent),
            ClientMsg::PullSide {
                conv_id,
                after_side_seq,
            } => self.pull_side(me, &conv_id, after_side_seq),
            ClientMsg::Read { conv_id, seq } => {
                self.require_member(&conv_id, &me.id)?;
                self.store.set_read(&conv_id, &me.id, seq)?;
                Ok(json!({}))
            }
            ClientMsg::Focus { conv_id, as_agent } => self.focus(sid, me, conv_id, as_agent),
            ClientMsg::StreamStart {
                conv_id,
                client_msg_id,
                reply_to,
                causation_id,
            } => self.stream_start(me, &conv_id, &client_msg_id, reply_to, causation_id),
            ClientMsg::StreamAppend { msg_id, idx, text } => {
                self.stream_append(me, &msg_id, idx, &text)
            }
            ClientMsg::StreamEnd { msg_id } => {
                let st = self
                    .rt
                    .streams
                    .get(&msg_id)
                    .ok_or_else(|| err("not_found", "no active stream"))?;
                if st.sender.id != me.id {
                    return Err(err("forbidden", "not your stream"));
                }
                self.finish_stream(&msg_id, "complete", None, &me.id);
                Ok(json!({"msg_id": msg_id}))
            }
            ClientMsg::StreamCancel { msg_id } => self.stream_cancel(me, &msg_id),
            ClientMsg::Whisper {
                conv_id,
                agent_id,
                text,
            } => self.whisper(me, &conv_id, &agent_id, &text),
            ClientMsg::Approve { draft_id, text } => self.approve(me, &draft_id, text),
            ClientMsg::Reject { draft_id } => self.reject(me, &draft_id),
            ClientMsg::Takeover { conv_id, agent_id } => {
                self.takeover(me, &conv_id, &agent_id, true)
            }
            ClientMsg::Release { conv_id, agent_id } => {
                self.takeover(me, &conv_id, &agent_id, false)
            }
            ClientMsg::SetAgentPolicy {
                conv_id,
                agent_id,
                approval,
                trigger,
            } => {
                let agent = self.owned_agent(me, &agent_id)?;
                self.require_member(&conv_id, &agent.id)?;
                let ev = self
                    .store
                    .set_agent_policy(&conv_id, &agent.id, approval, trigger, &me.id)?;
                self.store.audit(
                    Some(&me.id),
                    "agent.policy",
                    Some(&conv_id),
                    Some(&agent.id),
                    json!({"approval": approval, "trigger": trigger}),
                );
                self.fanout(&ev);
                Ok(json!({"seq": ev.seq}))
            }
            ClientMsg::ResetGuard { conv_id, agent_id } => {
                let agent = self.owned_agent(me, &agent_id)?;
                self.rt.guards.reset(&agent.id, &conv_id);
                let ev = self.store.system_event(
                    &conv_id,
                    "agent.state",
                    Some(&me.id),
                    json!({"agent_id": agent.id, "state": "active", "reason": "guard_reset"}),
                )?;
                self.store.audit(
                    Some(&me.id),
                    "guard.reset",
                    Some(&conv_id),
                    Some(&agent.id),
                    json!({}),
                );
                self.fanout(&ev);
                Ok(json!({}))
            }
            ClientMsg::ListDrafts => {
                let ds = self.store.pending_drafts_for_owner(&me.id)?;
                let list: Vec<Value> = ds
                    .iter()
                    .map(|d| json!({"draft_id": d.draft_id, "conv_id": d.conv_id, "agent_id": d.agent_id, "content": d.content, "created_at": d.created_at}))
                    .collect();
                Ok(json!({"drafts": list}))
            }
            ClientMsg::Stats => Ok(self.stats()),
        }
    }

    // -------------------------------------------------------------- basics

    fn hello(&mut self, me: &User) -> ApiResult<Value> {
        let users = self.store.list_users()?;
        let agents = self.store.agents_owned_by(&me.id)?;
        Ok(json!({"me": me, "users": users, "my_agents": agents}))
    }

    fn require_member(&self, conv_id: &str, user_id: &str) -> ApiResult<Member> {
        self.store
            .member(conv_id, user_id)?
            .ok_or_else(|| err("forbidden", "not a member of this conversation"))
    }

    fn owned_agent(&self, me: &User, agent_id: &str) -> ApiResult<User> {
        let a = self
            .store
            .user(agent_id)?
            .ok_or_else(|| err("not_found", "agent"))?;
        if a.kind != UserKind::Agent || a.owner_id.as_deref() != Some(me.id.as_str()) {
            return Err(err("forbidden", "you do not own this agent"));
        }
        Ok(a)
    }

    fn resolve_user(&self, key: &str) -> ApiResult<User> {
        if let Some(u) = self.store.user(key)? {
            return Ok(u);
        }
        self.store
            .token_by_name(key)?
            .map(|(u, _)| u)
            .ok_or_else(|| err("not_found", format!("user {key}")))
    }

    fn list_convs(&mut self, me: &User) -> ApiResult<Value> {
        let mut out = Vec::new();
        for (c, via) in self.store.convs_for(&me.id)? {
            let members: Vec<Value> = self
                .store
                .members(&c.id)?
                .iter()
                .map(|m| json!({"user_id": m.user_id, "name": m.name, "kind": m.kind, "owner_id": m.owner_id, "role": m.role, "trigger": m.trigger, "approval": m.approval}))
                .collect();
            let unread = if via.is_none() {
                self.store.unread(&c.id, &me.id)?
            } else {
                0
            };
            out.push(json!({"conv_id": c.id, "kind": c.kind, "title": c.title, "last_seq": c.last_seq, "unread": unread, "members": members, "observer_via": via}));
        }
        Ok(json!({"convs": out}))
    }

    fn create_conv(
        &mut self,
        me: &User,
        kind: &str,
        title: &str,
        members: &[String],
    ) -> ApiResult<Value> {
        if me.kind != UserKind::Human {
            return Err(err("forbidden", "only humans create conversations"));
        }
        if kind != "dm" && kind != "group" {
            return Err(err("bad_request", "kind must be dm or group"));
        }
        let users = members
            .iter()
            .map(|k| self.resolve_user(k))
            .collect::<ApiResult<Vec<_>>>()?;
        let (id, events) = self.store.create_conv(kind, title, me, &users)?;
        for e in &events {
            self.fanout(e);
        }
        Ok(json!({"conv_id": id}))
    }

    fn add_member(&mut self, me: &User, conv_id: &str, user_key: &str) -> ApiResult<Value> {
        let mine = self.require_member(conv_id, &me.id)?;
        let u = self.resolve_user(user_key)?;
        let own_agent = u.owner_id.as_deref() == Some(me.id.as_str());
        if mine.role != "owner" && !own_agent {
            return Err(err(
                "forbidden",
                "only the conversation owner can add members",
            ));
        }
        let ev = self.store.add_member(conv_id, &u, &me.id)?;
        if u.kind == UserKind::Agent {
            self.store.audit(
                Some(&me.id),
                "agent.added",
                Some(conv_id),
                Some(&u.id),
                json!({}),
            );
        }
        self.fanout(&ev);
        Ok(json!({"seq": ev.seq}))
    }

    fn mentions(&self, conv_id: &str, text: &str) -> ApiResult<Vec<String>> {
        Ok(self
            .store
            .members(conv_id)?
            .into_iter()
            .filter(|m| text.contains(&format!("@{}", m.name)))
            .map(|m| m.user_id)
            .collect())
    }

    /// Causation and hop are computed by the server; agent-supplied values are
    /// only used to pick which message in this conversation is being answered.
    fn causation(
        &self,
        conv_id: &str,
        sender: &User,
        causation_id: Option<String>,
        reply_to: &Option<String>,
    ) -> ApiResult<(Option<String>, i64)> {
        let explicit = causation_id.or_else(|| reply_to.clone());
        let cause = match explicit {
            Some(id) => {
                let m = self
                    .store
                    .message(&id)?
                    .ok_or_else(|| err("bad_request", "unknown causation message"))?;
                if m.conv_id != conv_id {
                    return Err(err(
                        "bad_request",
                        "causation message is in another conversation",
                    ));
                }
                Some(m)
            }
            None => self.store.latest_other_message(conv_id, &sender.id)?,
        };
        if sender.kind != UserKind::Agent {
            return Ok((cause.map(|m| m.msg_id), 0));
        }
        let hop = cause.as_ref().map(|m| m.hop + 1).unwrap_or(1);
        Ok((cause.map(|m| m.msg_id), hop))
    }

    /// Takeover, breaker, rate, budget and loop checks for an agent message.
    fn agent_gate(&mut self, agent: &User, conv_id: &str, hop: i64, text: &str) -> ApiResult<()> {
        if let Some(by) = self.store.takeover(conv_id, &agent.id)? {
            return Err(err(
                "taken_over",
                format!("conversation is taken over by {by}"),
            ));
        }
        let recent =
            self.store
                .recent_contents(conv_id, &agent.id, self.rt.guards.cfg.dup_window)?;
        match self.rt.guards.check(&agent.id, conv_id, hop, text, &recent) {
            Verdict::Allow => Ok(()),
            Verdict::Reject(code, reason) => {
                self.store.audit(
                    Some(&agent.id),
                    "guard.reject",
                    Some(conv_id),
                    None,
                    json!({"code": code, "reason": reason}),
                );
                Err(err(code, reason))
            }
            Verdict::Trip(code, reason) => {
                let ev = self.store.system_event(
                    conv_id,
                    "agent.state",
                    Some(&agent.id),
                    json!({"agent_id": agent.id, "state": "tripped", "reason": code, "detail": reason}),
                )?;
                self.store.audit(
                    Some(&agent.id),
                    "guard.trip",
                    Some(conv_id),
                    None,
                    json!({"code": code, "reason": reason, "hop": hop}),
                );
                self.fanout(&ev);
                self.notify_owner(
                    conv_id,
                    agent,
                    "guard.tripped",
                    json!({"agent_id": agent.id, "code": code, "reason": reason}),
                )?;
                Err(err(code, reason))
            }
        }
    }

    fn agent_audience(&self, agent: &User) -> Vec<String> {
        let mut a = vec![agent.id.clone()];
        if let Some(o) = &agent.owner_id {
            a.push(o.clone());
        }
        a
    }

    fn notify_owner(
        &mut self,
        conv_id: &str,
        agent: &User,
        kind: &str,
        data: Value,
    ) -> ApiResult<()> {
        let audience = self.agent_audience(agent);
        let side = self
            .store
            .append_side(conv_id, &audience, kind, Some(&agent.id), data)?;
        self.fanout_side(&side);
        Ok(())
    }

    // --------------------------------------------------------------- send

    #[allow(clippy::too_many_arguments)]
    fn send(
        &mut self,
        me: &User,
        conv_id: &str,
        client_msg_id: &str,
        text: &str,
        reply_to: Option<String>,
        causation_id: Option<String>,
        on_behalf_of: Option<String>,
        to_user: Option<String>,
    ) -> ApiResult<Value> {
        let member = self.require_member(conv_id, &me.id)?;
        if text.is_empty() || text.len() > MAX_MESSAGE_BYTES {
            return Err(err("bad_request", "message must be 1..64KB"));
        }
        if let Some(o) = &on_behalf_of {
            // Community rule: an agent may only speak on behalf of its owner.
            if me.kind != UserKind::Agent || me.owner_id.as_deref() != Some(o.as_str()) {
                return Err(err("forbidden", "on_behalf_of must be the agent's owner"));
            }
        }
        let (causation_id, hop) = self.causation(conv_id, me, causation_id, &reply_to)?;
        if me.kind == UserKind::Agent {
            self.agent_gate(me, conv_id, hop, text)?;
            if let Some((hook, reason)) = self.policy.outbound(text) {
                self.store.audit(
                    Some(&me.id),
                    "policy.block",
                    Some(conv_id),
                    None,
                    json!({"hook": hook, "reason": reason}),
                );
                self.notify_owner(
                    conv_id,
                    me,
                    "policy.blocked",
                    json!({"agent_id": me.id, "hook": hook, "reason": reason}),
                )?;
                return Err(err("policy_blocked", reason));
            }
        }

        // Targeted visibility goes to the side channel and takes no conv_seq.
        if let Some(target) = to_user {
            let t = self.resolve_user(&target)?;
            self.require_member(conv_id, &t.id)?;
            let mut audience = self.agent_audience(me);
            if !audience.contains(&t.id) {
                audience.push(t.id.clone());
            }
            let side = self.store.append_side(
                conv_id,
                &audience,
                "ephemeral",
                Some(&me.id),
                json!({"text": text, "from": me.id, "from_name": me.name, "from_type": me.kind, "to": t.id, "hop": hop}),
            )?;
            self.fanout_side(&side);
            return Ok(json!({"side_seq": side.side_seq}));
        }

        if me.kind == UserKind::Agent && member.approval {
            let audience = self.agent_audience(me);
            let (d, side) = self.store.create_draft(
                conv_id,
                me,
                text,
                hop,
                causation_id,
                reply_to,
                &audience,
            )?;
            self.store.audit(
                Some(&me.id),
                "draft.created",
                Some(conv_id),
                Some(&d.draft_id),
                json!({}),
            );
            self.fanout_side(&side);
            return Ok(json!({"draft_id": d.draft_id, "state": "pending_approval"}));
        }

        let mentions = self.mentions(conv_id, text)?;
        let res = self.store.commit_message(NewMsg {
            conv_id,
            sender: me,
            client_msg_id: Some(client_msg_id),
            on_behalf_of,
            kind: "text",
            content: text.to_string(),
            state: "committed",
            hop,
            causation_id,
            reply_to,
            mentions,
        })?;
        match res {
            Committed::Duplicate { seq, msg_id } => {
                Ok(json!({"seq": seq, "msg_id": msg_id, "duplicate": true}))
            }
            Committed::New(ev) => {
                self.fanout(&ev);
                let m = ev.msg.as_ref().unwrap();
                Ok(json!({"seq": ev.seq, "msg_id": m.msg_id, "hop": m.hop}))
            }
        }
    }

    /// May `me` modify this message? The sender, the sending agent's owner,
    /// or (recall only) the conversation owner.
    fn can_modify(&self, me: &User, m: &MsgView, recall: bool) -> ApiResult<bool> {
        if m.sender_id == me.id {
            return Ok(true);
        }
        if let Some(sender) = self.store.user(&m.sender_id)?
            && sender.owner_id.as_deref() == Some(me.id.as_str())
        {
            return Ok(true);
        }
        if recall && let Some(mem) = self.store.member(&m.conv_id, &me.id)? {
            return Ok(mem.role == "owner");
        }
        Ok(false)
    }

    fn edit(&mut self, me: &User, msg_id: &str, text: &str) -> ApiResult<Value> {
        let m = self
            .store
            .message(msg_id)?
            .ok_or_else(|| err("not_found", "message"))?;
        if !self.can_modify(me, &m, false)? {
            return Err(err("forbidden", "cannot edit this message"));
        }
        if m.state == "streaming" || m.state == "recalled" {
            return Err(err("conflict", format!("message is {}", m.state)));
        }
        if me.kind == UserKind::Agent
            && let Some((_, reason)) = self.policy.outbound(text)
        {
            return Err(err("policy_blocked", reason));
        }
        let ev =
            self.store
                .update_message(msg_id, "msg.updated", &me.id, Some(text), None, None)?;
        if m.sender_id != me.id {
            self.store.audit(
                Some(&me.id),
                "msg.revised",
                Some(&m.conv_id),
                Some(msg_id),
                json!({}),
            );
        }
        self.fanout(&ev);
        Ok(json!({"seq": ev.seq}))
    }

    fn recall(&mut self, me: &User, msg_id: &str) -> ApiResult<Value> {
        let m = self
            .store
            .message(msg_id)?
            .ok_or_else(|| err("not_found", "message"))?;
        if !self.can_modify(me, &m, true)? {
            return Err(err("forbidden", "cannot recall this message"));
        }
        if m.state == "streaming" {
            self.finish_stream(msg_id, "cancelled", Some("recalled"), &me.id);
        }
        let ev = self.store.update_message(
            msg_id,
            "msg.recalled",
            &me.id,
            Some(""),
            Some("recalled"),
            None,
        )?;
        if m.sender_id != me.id {
            self.store.audit(
                Some(&me.id),
                "msg.recalled",
                Some(&m.conv_id),
                Some(msg_id),
                json!({}),
            );
        }
        self.fanout(&ev);
        Ok(json!({"seq": ev.seq}))
    }

    // --------------------------------------------------------------- read

    fn observer_agent(&self, me: &User, conv_id: &str, as_agent: &str) -> ApiResult<Member> {
        let agent = self.owned_agent(me, as_agent)?;
        self.require_member(conv_id, &agent.id)
    }

    /// What an agent member may read (bounded by its trigger mode).
    fn agent_can_see(&self, m: &Member, ev: &Event) -> bool {
        if ev.seq < m.join_seq {
            return false;
        }
        let Some(msg) = &ev.msg else { return true };
        if m.trigger == Trigger::All
            || msg.sender_id == m.user_id
            || msg.mentions.contains(&m.user_id)
        {
            return true;
        }
        if let Some(r) = &msg.reply_to
            && let Ok(Some(parent)) = self.store.message(r)
        {
            return parent.sender_id == m.user_id;
        }
        false
    }

    fn pull(
        &mut self,
        me: &User,
        conv_id: &str,
        after: i64,
        limit: i64,
        as_agent: Option<String>,
    ) -> ApiResult<Value> {
        let reader = match &as_agent {
            Some(a) => {
                let m = self.observer_agent(me, conv_id, a)?;
                self.store.audit(
                    Some(&me.id),
                    "observe",
                    Some(conv_id),
                    Some(a),
                    json!({"after_seq": after}),
                );
                m
            }
            None => self.require_member(conv_id, &me.id)?,
        };
        let limit = limit.clamp(1, 1000);
        let events = self.store.pull(conv_id, after, limit)?;
        let upto = events.last().map(|e| e.seq).unwrap_or(after);
        let has_more = events.len() as i64 == limit;
        let filtered = reader.kind == UserKind::Agent;
        let events: Vec<Value> = events
            .into_iter()
            .filter(|e| !filtered || self.agent_can_see(&reader, e))
            .map(|e| self.event_json(&e, Some(&reader)))
            .collect();
        // `upto_seq` lets filtered readers (agents) advance without gap checks.
        Ok(
            json!({"conv_id": conv_id, "events": events, "upto_seq": upto, "has_more": has_more, "filtered": filtered}),
        )
    }

    fn pull_side(&mut self, me: &User, conv_id: &str, after: i64) -> ApiResult<Value> {
        let is_member = self.store.member(conv_id, &me.id)?.is_some();
        let owns_member = self
            .store
            .members(conv_id)?
            .iter()
            .any(|m| m.owner_id.as_deref() == Some(me.id.as_str()));
        if !is_member && !owns_member {
            return Err(err("forbidden", "no access"));
        }
        let list: Vec<SideEvent> = self
            .store
            .pull_side(conv_id, after)?
            .into_iter()
            .filter(|s| s.audience.contains(&me.id))
            .collect();
        Ok(json!({"conv_id": conv_id, "side_events": list}))
    }

    fn event_json(&self, ev: &Event, reader: Option<&Member>) -> Value {
        let mut v = json!({"type": "event", "event": ev});
        if let (Some(r), Some(msg)) = (reader, &ev.msg)
            && r.kind == UserKind::Agent
            && msg.sender_id != r.user_id
            && ev.kind == "msg.created"
        {
            let (trust, flags) = self.policy.inbound(&msg.content, msg.sender_type);
            v["trust"] = json!(trust.as_str());
            v["flags"] = json!(flags);
        }
        v
    }

    fn focus(
        &mut self,
        sid: u64,
        me: &User,
        conv_id: Option<String>,
        as_agent: Option<String>,
    ) -> ApiResult<Value> {
        if let Some(c) = &conv_id {
            match &as_agent {
                Some(a) => {
                    self.observer_agent(me, c, a)?;
                }
                None => {
                    self.require_member(c, &me.id)?;
                }
            }
        }
        if let Some(s) = self.rt.sessions.get_mut(&sid) {
            s.focus = conv_id.clone().map(|c| (c, as_agent.clone()));
        }
        // Late joiners get the accumulated content of in-flight streams.
        if let Some(c) = conv_id {
            let snaps: Vec<Value> = self
                .rt
                .streams
                .values()
                .filter(|st| st.conv_id == c && matches!(st.target, Target::Main))
                .map(|st| json!({"type": "delta", "conv_id": st.conv_id, "msg_id": st.msg_id, "reset": true, "text": st.buf}))
                .collect();
            if let Some(s) = self.rt.sessions.get(&sid) {
                for v in snaps {
                    let _ = s.tx.send(v);
                }
            }
        }
        Ok(json!({}))
    }

    // ------------------------------------------------------------ fan-out

    fn fanout(&mut self, ev: &Event) {
        let Ok(members) = self.store.members(&ev.conv_id) else {
            return;
        };
        let by_id: HashMap<&str, &Member> =
            members.iter().map(|m| (m.user_id.as_str(), m)).collect();
        let mut out: Vec<(UnboundedSender<Value>, Value)> = Vec::new();
        for s in self.rt.sessions.values() {
            if let Some(m) = by_id.get(s.user.id.as_str()) {
                if m.kind == UserKind::Agent && !self.agent_can_see(m, ev) {
                    continue;
                }
                out.push((s.tx.clone(), self.event_json(ev, Some(m))));
            } else if let Some((fc, Some(agent))) = &s.focus {
                // Owner observing through an agent: same scope as the agent.
                if fc == &ev.conv_id
                    && let Some(m) = by_id.get(agent.as_str())
                    && m.owner_id.as_deref() == Some(s.user.id.as_str())
                    && self.agent_can_see(m, ev)
                {
                    let mut v = self.event_json(ev, None);
                    v["observing"] = json!(agent);
                    out.push((s.tx.clone(), v));
                }
            }
        }
        for (tx, v) in out {
            let _ = tx.send(v);
        }
    }

    fn fanout_side(&mut self, side: &SideEvent) {
        let v = json!({"type": "side", "event": side});
        for s in self.rt.sessions.values() {
            if side.audience.contains(&s.user.id) {
                let _ = s.tx.send(v.clone());
            }
        }
    }

    fn send_to_user(&self, user_id: &str, v: Value) {
        for s in self.rt.sessions.values() {
            if s.user.id == user_id {
                let _ = s.tx.send(v.clone());
            }
        }
    }

    // ---------------------------------------------------------- streaming

    fn stream_start(
        &mut self,
        me: &User,
        conv_id: &str,
        client_msg_id: &str,
        reply_to: Option<String>,
        causation_id: Option<String>,
    ) -> ApiResult<Value> {
        if me.kind != UserKind::Agent {
            return Err(err("forbidden", "only agents stream"));
        }
        let member = self.require_member(conv_id, &me.id)?;
        let active = self
            .rt
            .streams
            .values()
            .filter(|s| s.sender.id == me.id && s.conv_id == conv_id)
            .count();
        if active >= 2 {
            return Err(err(
                "rate_limited",
                "too many concurrent streams in this conversation",
            ));
        }
        let (causation_id, hop) = self.causation(conv_id, me, causation_id, &reply_to)?;
        self.agent_gate(me, conv_id, hop, "")?;
        let now = Instant::now();
        let (msg_id, target, seq) = if member.approval {
            (
                format!("d_{}", uuid::Uuid::now_v7().simple()),
                Target::Draft {
                    audience: self.agent_audience(me),
                },
                None,
            )
        } else {
            let res = self.store.commit_message(NewMsg {
                conv_id,
                sender: me,
                client_msg_id: Some(client_msg_id),
                on_behalf_of: None,
                kind: "text",
                content: String::new(),
                state: "streaming",
                hop,
                causation_id: causation_id.clone(),
                reply_to: reply_to.clone(),
                mentions: vec![],
            })?;
            match res {
                Committed::Duplicate { seq, msg_id } => {
                    return Ok(json!({"msg_id": msg_id, "seq": seq, "duplicate": true}));
                }
                Committed::New(ev) => {
                    self.fanout(&ev);
                    (
                        ev.msg.as_ref().unwrap().msg_id.clone(),
                        Target::Main,
                        Some(ev.seq),
                    )
                }
            }
        };
        self.rt.streams.insert(
            msg_id.clone(),
            Stream {
                msg_id: msg_id.clone(),
                conv_id: conv_id.to_string(),
                sender: me.clone(),
                target: target.clone(),
                buf: String::new(),
                pending: String::new(),
                next_idx: 0,
                started: now,
                last_append: now,
                checkpointed: 0,
                hop,
                causation_id,
                reply_to,
            },
        );
        let pending_approval = matches!(target, Target::Draft { .. });
        Ok(json!({"msg_id": msg_id, "seq": seq, "pending_approval": pending_approval, "hop": hop}))
    }

    fn stream_append(&mut self, me: &User, msg_id: &str, idx: u64, text: &str) -> ApiResult<Value> {
        let max_bytes = self.cfg.stream_max_bytes;
        let st = self
            .rt
            .streams
            .get_mut(msg_id)
            .ok_or_else(|| err("not_found", "no active stream"))?;
        if st.sender.id != me.id {
            return Err(err("forbidden", "not your stream"));
        }
        if idx < st.next_idx {
            return Ok(json!({"duplicate": true}));
        }
        if idx > st.next_idx {
            return Err(err(
                "out_of_order",
                format!("expected chunk {}", st.next_idx),
            ));
        }
        st.next_idx += 1;
        st.last_append = Instant::now();
        st.buf.push_str(text);
        st.pending.push_str(text);
        let over = st.buf.len() > max_bytes;
        let buf = st.buf.clone();
        let conv_id = st.conv_id.clone();
        if !self
            .rt
            .guards
            .charge_chars(&me.id, text.chars().count() as u64)
        {
            self.finish_stream(msg_id, "cancelled", Some("budget_exceeded"), &me.id);
            return Err(err("budget_exceeded", "daily agent budget exhausted"));
        }
        // Streaming filter: check the accumulated text chunk by chunk.
        if let Some((hook, reason)) = self.policy.outbound(&buf) {
            self.store.audit(
                Some(&me.id),
                "policy.block",
                Some(&conv_id),
                Some(msg_id),
                json!({"hook": hook, "reason": reason, "streaming": true}),
            );
            self.moderate_stream(msg_id, &reason);
            return Err(err("policy_blocked", reason));
        }
        if over {
            self.finish_stream(msg_id, "complete", Some("size_limit"), &me.id);
            return Err(err(
                "size_limit",
                "stream exceeded the maximum size and was closed",
            ));
        }
        Ok(json!({}))
    }

    fn stream_cancel(&mut self, me: &User, msg_id: &str) -> ApiResult<Value> {
        let st = self
            .rt
            .streams
            .get(msg_id)
            .ok_or_else(|| err("not_found", "no active stream"))?;
        let sender = st.sender.clone();
        let conv_id = st.conv_id.clone();
        let is_owner = sender.owner_id.as_deref() == Some(me.id.as_str());
        let human_member =
            me.kind == UserKind::Human && self.store.member(&conv_id, &me.id)?.is_some();
        if sender.id != me.id && !is_owner && !human_member {
            return Err(err("forbidden", "cannot interrupt this stream"));
        }
        let reason = format!("cancelled_by:{}", me.name);
        self.finish_stream(msg_id, "cancelled", Some(&reason), &me.id);
        if sender.id != me.id {
            self.store.audit(
                Some(&me.id),
                "stream.interrupt",
                Some(&conv_id),
                Some(msg_id),
                json!({"agent_id": sender.id}),
            );
        }
        Ok(json!({"msg_id": msg_id}))
    }

    /// Policy hit mid-stream: stop the stream and replace what was already
    /// pushed with a placeholder (a new version of the same message).
    fn moderate_stream(&mut self, msg_id: &str, reason: &str) {
        let Some(st) = self.rt.streams.remove(msg_id) else {
            return;
        };
        let r = format!("policy: {reason}");
        match &st.target {
            Target::Main => {
                if let Ok(ev) = self.store.update_message(
                    msg_id,
                    "msg.updated",
                    &st.sender.id,
                    Some(MODERATED_PLACEHOLDER),
                    Some("cancelled"),
                    Some(&r),
                ) {
                    self.fanout(&ev);
                }
            }
            Target::Draft { audience } => {
                if let Ok(side) = self.store.append_side(
                    &st.conv_id,
                    audience,
                    "draft.discarded",
                    Some(&st.sender.id),
                    json!({"draft_id": msg_id, "reason": r}),
                ) {
                    self.fanout_side(&side);
                }
            }
        }
        self.send_to_user(
            &st.sender.id,
            json!({"type": "stream_cancelled", "msg_id": msg_id, "reason": r}),
        );
        let _ = self.notify_owner(
            &st.conv_id,
            &st.sender,
            "policy.blocked",
            json!({"agent_id": st.sender.id, "msg_id": msg_id, "reason": reason}),
        );
    }

    /// Terminal transition of a stream: complete | cancelled | failed.
    fn finish_stream(&mut self, msg_id: &str, state: &str, reason: Option<&str>, actor: &str) {
        self.flush_one(msg_id);
        let Some(st) = self.rt.streams.remove(msg_id) else {
            return;
        };
        match &st.target {
            Target::Main => {
                if let Ok(ev) = self.store.update_message(
                    msg_id,
                    "msg.updated",
                    actor,
                    Some(&st.buf),
                    Some(state),
                    reason,
                ) {
                    self.fanout(&ev);
                }
            }
            Target::Draft { audience } => {
                if state == "complete" && !st.buf.is_empty() {
                    if let Ok((d, side)) = self.store.create_draft(
                        &st.conv_id,
                        &st.sender,
                        &st.buf,
                        st.hop,
                        st.causation_id.clone(),
                        st.reply_to.clone(),
                        audience,
                    ) {
                        self.store.audit(
                            Some(&st.sender.id),
                            "draft.created",
                            Some(&st.conv_id),
                            Some(&d.draft_id),
                            json!({"streamed": true}),
                        );
                        self.fanout_side(&side);
                    }
                } else if let Ok(side) = self.store.append_side(
                    &st.conv_id,
                    audience,
                    "draft.discarded",
                    Some(actor),
                    json!({"draft_id": msg_id, "reason": reason.unwrap_or(state)}),
                ) {
                    self.fanout_side(&side);
                }
            }
        }
        if state != "complete" && actor != st.sender.id {
            self.send_to_user(&st.sender.id, json!({"type": "stream_cancelled", "msg_id": msg_id, "reason": reason.unwrap_or(state)}));
        }
    }

    /// Pushes a stream's pending delta to the sessions that are looking at it.
    fn flush_one(&mut self, msg_id: &str) {
        let Some(st) = self.rt.streams.get_mut(msg_id) else {
            return;
        };
        if st.pending.is_empty() {
            return;
        }
        let text = std::mem::take(&mut st.pending);
        let v = json!({"type": "delta", "conv_id": st.conv_id, "msg_id": st.msg_id, "reset": false, "text": text, "draft": matches!(st.target, Target::Draft{..})});
        let conv_id = st.conv_id.clone();
        let target = st.target.clone();
        let sender = st.sender.id.clone();
        for s in self.rt.sessions.values() {
            let focused = s
                .focus
                .as_ref()
                .map(|(c, _)| c == &conv_id)
                .unwrap_or(false);
            let to = match &target {
                Target::Main => focused || s.user.id == sender,
                Target::Draft { audience } => audience.contains(&s.user.id),
            };
            if to {
                let _ = s.tx.send(v.clone());
            }
        }
    }

    fn tick(&mut self) {
        let now = Instant::now();
        if now.duration_since(self.rt.last_flush) >= self.cfg.coalesce {
            self.rt.last_flush = now;
            let ids: Vec<String> = self.rt.streams.keys().cloned().collect();
            for id in ids {
                self.flush_one(&id);
            }
        }
        if now.duration_since(self.rt.last_checkpoint) >= self.cfg.checkpoint_every {
            self.rt.last_checkpoint = now;
            let dirty: Vec<(String, String)> = self
                .rt
                .streams
                .values_mut()
                .filter(|s| matches!(s.target, Target::Main) && s.buf.len() != s.checkpointed)
                .map(|s| {
                    s.checkpointed = s.buf.len();
                    (s.msg_id.clone(), s.buf.clone())
                })
                .collect();
            for (id, content) in dirty {
                let _ = self.store.checkpoint(&id, &content);
            }
        }
        let expired: Vec<(String, &'static str, &'static str, String)> = self
            .rt
            .streams
            .values()
            .filter_map(|s| {
                if now.duration_since(s.last_append) > self.cfg.stream_idle {
                    Some((s.msg_id.clone(), "failed", "timeout", s.sender.id.clone()))
                } else if now.duration_since(s.started) > self.cfg.stream_max {
                    Some((
                        s.msg_id.clone(),
                        "complete",
                        "duration_limit",
                        s.sender.id.clone(),
                    ))
                } else {
                    None
                }
            })
            .collect();
        for (id, state, reason, sender) in expired {
            self.finish_stream(&id, state, Some(reason), &sender);
        }
    }

    // ------------------------------------------------------- intervention

    fn whisper(
        &mut self,
        me: &User,
        conv_id: &str,
        agent_id: &str,
        text: &str,
    ) -> ApiResult<Value> {
        let agent = self.owned_agent(me, agent_id)?;
        self.require_member(conv_id, &agent.id)?;
        let audience = vec![agent.id.clone(), me.id.clone()];
        let side = self.store.append_side(
            conv_id,
            &audience,
            "whisper",
            Some(&me.id),
            json!({"text": text, "from": me.id, "from_name": me.name, "to": agent.id}),
        )?;
        self.store.audit(
            Some(&me.id),
            "whisper",
            Some(conv_id),
            Some(&agent.id),
            json!({}),
        );
        self.fanout_side(&side);
        Ok(json!({"side_seq": side.side_seq, "anchor_seq": side.anchor_seq}))
    }

    fn approve(&mut self, me: &User, draft_id: &str, text: Option<String>) -> ApiResult<Value> {
        let d = self
            .store
            .draft(draft_id)?
            .ok_or_else(|| err("not_found", "draft"))?;
        let agent = self.owned_agent(me, &d.agent_id)?;
        if d.state != "pending" {
            return Err(err("conflict", "draft already decided"));
        }
        let audience = self.agent_audience(&agent);
        let side = self
            .store
            .decide_draft(draft_id, "approved", &me.id, &audience, &d.conv_id)?;
        let content = text.unwrap_or(d.content.clone());
        let mentions = self.mentions(&d.conv_id, &content)?;
        let res = self.store.commit_message(NewMsg {
            conv_id: &d.conv_id,
            sender: &agent,
            client_msg_id: Some(draft_id),
            on_behalf_of: None,
            kind: "text",
            content,
            state: "committed",
            hop: d.hop,
            causation_id: d.causation_id.clone(),
            reply_to: d.reply_to.clone(),
            mentions,
        })?;
        self.store.audit(
            Some(&me.id),
            "draft.approved",
            Some(&d.conv_id),
            Some(draft_id),
            json!({"edited": d.content}),
        );
        self.fanout_side(&side);
        if let Committed::New(ev) = res {
            self.fanout(&ev);
            return Ok(json!({"seq": ev.seq}));
        }
        Ok(json!({}))
    }

    fn reject(&mut self, me: &User, draft_id: &str) -> ApiResult<Value> {
        let d = self
            .store
            .draft(draft_id)?
            .ok_or_else(|| err("not_found", "draft"))?;
        let agent = self.owned_agent(me, &d.agent_id)?;
        let audience = self.agent_audience(&agent);
        let side = self
            .store
            .decide_draft(draft_id, "rejected", &me.id, &audience, &d.conv_id)?;
        self.store.audit(
            Some(&me.id),
            "draft.rejected",
            Some(&d.conv_id),
            Some(draft_id),
            json!({}),
        );
        self.fanout_side(&side);
        Ok(json!({}))
    }

    fn takeover(&mut self, me: &User, conv_id: &str, agent_id: &str, on: bool) -> ApiResult<Value> {
        let agent = self.owned_agent(me, agent_id)?;
        self.require_member(conv_id, &agent.id)?;
        let current = self.store.takeover(conv_id, &agent.id)?;
        if on == current.is_some() {
            return Err(err(
                "conflict",
                if on {
                    "already taken over"
                } else {
                    "not taken over"
                },
            ));
        }
        if on {
            let ids: Vec<String> = self
                .rt
                .streams
                .values()
                .filter(|s| s.sender.id == agent.id && s.conv_id == conv_id)
                .map(|s| s.msg_id.clone())
                .collect();
            for id in ids {
                self.finish_stream(&id, "cancelled", Some("taken_over"), &me.id);
            }
        }
        let ev =
            self.store
                .set_takeover(conv_id, &agent.id, if on { Some(&me.id) } else { None })?;
        self.store.audit(
            Some(&me.id),
            if on { "takeover" } else { "release" },
            Some(conv_id),
            Some(&agent.id),
            json!({}),
        );
        self.fanout(&ev);
        Ok(json!({"seq": ev.seq}))
    }

    fn stats(&self) -> Value {
        let counts = self.store.counts().unwrap_or(Value::Null);
        let streams: Vec<Value> = self
            .rt
            .streams
            .values()
            .map(|s| json!({"msg_id": s.msg_id, "conv_id": s.conv_id, "agent": s.sender.name, "bytes": s.buf.len(), "age_ms": s.started.elapsed().as_millis() as u64, "draft": matches!(s.target, Target::Draft{..})}))
            .collect();
        let mut online: Vec<String> = self
            .rt
            .sessions
            .values()
            .map(|s| s.user.name.clone())
            .collect();
        online.sort();
        online.dedup();
        let audit = self.store.audit_log(50).unwrap_or_default();
        json!({"counts": counts, "active_streams": streams, "online": online, "sessions": self.rt.sessions.len(), "audit": audit})
    }
}
