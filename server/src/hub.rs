//! The hub: opens the databases, routes every request to the writer (or read
//! pool) of the database that owns its conversation, merges cross-database
//! reads, and moves conversations between databases (docs/02-architecture.md
//! §4–§5.4).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::config::Config;
use crate::db::{Db, DirEntry, fingerprint, new_id};
use crate::logic::{Ctx, DirInfo, HubConfig, Op, SessCtx, Shared};
use crate::metrics as m;
use crate::model::*;
use crate::policy::PolicyChain;
use crate::registry::{Frame, Registry};
use crate::shard::Shard;

fn err(code: &'static str, msg: impl Into<String>) -> ApiError {
    ApiError::new(code, msg)
}

struct Inner {
    sh: Arc<Shared>,
    shards: Vec<Shard>,
    cfg: Config,
    tickers: Mutex<Vec<tokio::task::JoinHandle<()>>>,
}

#[derive(Clone)]
pub struct Hub {
    inner: Arc<Inner>,
}

pub struct Session {
    hub: Hub,
    pub sid: u64,
    pub user: User,
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Some((user_id, kind, still_online)) = self.hub.inner.sh.reg.remove(self.sid)
            && kind == UserKind::Agent
            && !still_online
        {
            for s in &self.hub.inner.shards {
                s.post(Op::AgentGone(user_id.clone()));
            }
        }
    }
}

impl Hub {
    pub async fn open(cfg: Config, hub_cfg: HubConfig) -> ApiResult<Hub> {
        cfg.validate().map_err(|e| err("config", e))?;
        m::init();
        sqlx::any::install_default_drivers();
        let db_names: Vec<String> = cfg
            .storage
            .databases
            .iter()
            .map(|d| d.name.clone())
            .collect();
        let control = db_names
            .iter()
            .position(|n| *n == cfg.storage.control)
            .unwrap();
        let sh = Arc::new(Shared {
            reg: Registry::new(hub_cfg.human_queue, hub_cfg.agent_queue),
            users: Default::default(),
            tokens: Default::default(),
            dir: Default::default(),
            stream_index: Default::default(),
            policy: PolicyChain::default_chain(),
            budgets: Default::default(),
            control,
            db_names: db_names.clone(),
            cfg: hub_cfg,
        });
        let mut shards = Vec::new();
        for (i, d) in cfg.storage.databases.iter().enumerate() {
            shards.push(Shard::open(i, d, &cfg.writer, sh.clone()).await?);
        }
        let hub = Hub {
            inner: Arc::new(Inner {
                sh,
                shards,
                cfg,
                tickers: Mutex::new(vec![]),
            }),
        };
        hub.load().await?;
        Ok(hub)
    }

    /// Rebuilds in-memory directories from the databases, repairs the
    /// conversation directory after a crash, and fails interrupted streams.
    async fn load(&self) -> ApiResult<()> {
        let sh = &self.inner.sh;
        let control = self.control();
        for (u, token) in self
            .read(control, |db| Box::pin(async move { db.load_users().await }))
            .await?
        {
            sh.tokens.write().unwrap().insert(token, u.id.clone());
            sh.users.write().unwrap().insert(u.id.clone(), u);
        }
        for u in &self.inner.cfg.units {
            control
                .call(Op::UpsertUnit {
                    name: u.name.clone(),
                    database: u.database.clone(),
                })
                .await?;
        }
        let entries = self
            .read(control, |db| Box::pin(async move { db.dir_all().await }))
            .await?;
        for e in entries {
            let Some(idx) = sh.db_names.iter().position(|n| *n == e.database) else {
                return Err(err(
                    "config",
                    format!(
                        "conversation {} lives in database {} which is not configured",
                        e.conv_id, e.database
                    ),
                ));
            };
            // A migration interrupted before the switch: the source is intact.
            if e.migrating {
                control
                    .call(Op::DirPut(DirEntry {
                        migrating: false,
                        ..e.clone()
                    }))
                    .await?;
            }
            sh.dir_set(
                &e.conv_id,
                Some(DirInfo {
                    db: idx,
                    unit: e.unit_id,
                    migrating: false,
                }),
            );
        }
        self.reconcile().await?;
        for s in &self.inner.shards {
            s.call(Op::Recover).await?;
        }
        Ok(())
    }

    /// The directory (control database) and the conversation databases are
    /// written separately when they differ; repair what a crash in between
    /// may have left behind.
    async fn reconcile(&self) -> ApiResult<()> {
        let sh = &self.inner.sh;
        let mut present: HashMap<String, Vec<(usize, String)>> = HashMap::new();
        for s in &self.inner.shards {
            for (id, unit) in self
                .read(s, |db| Box::pin(async move { db.conv_ids().await }))
                .await?
            {
                present.entry(id).or_default().push((s.idx, unit));
            }
        }
        let control = self.control();
        for (conv, copies) in &present {
            let dir = sh.dir_get(conv);
            let owner = match &dir {
                Some(d) if copies.iter().any(|(i, _)| *i == d.db) => d.db,
                _ => {
                    // Missing or dangling entry: point it at an existing copy.
                    let (i, unit) = copies[0].clone();
                    let members = self.member_ids(i, conv).await?;
                    tracing::warn!(
                        conv,
                        database = sh.db_names[i],
                        "repairing conversation directory entry"
                    );
                    control
                        .call(Op::RegisterConv {
                            entry: DirEntry {
                                conv_id: conv.clone(),
                                database: sh.db_names[i].clone(),
                                unit_id: unit.clone(),
                                migrating: false,
                            },
                            member_ids: members,
                        })
                        .await?;
                    sh.dir_set(
                        conv,
                        Some(DirInfo {
                            db: i,
                            unit,
                            migrating: false,
                        }),
                    );
                    i
                }
            };
            for (i, _) in copies.iter().filter(|(i, _)| *i != owner) {
                tracing::warn!(
                    conv,
                    database = sh.db_names[*i],
                    "deleting stale copy left by an interrupted migration"
                );
                self.inner.shards[*i]
                    .call(Op::DeleteConv(conv.clone()))
                    .await?;
            }
            if owner != sh.control {
                for u in self.member_ids(owner, conv).await? {
                    control
                        .call(Op::MemberIndexAdd {
                            conv_id: conv.clone(),
                            user_id: u,
                        })
                        .await?;
                }
            }
        }
        let dangling: Vec<String> = sh
            .dir
            .read()
            .unwrap()
            .keys()
            .filter(|c| !present.contains_key(*c))
            .cloned()
            .collect();
        for c in dangling {
            tracing::warn!(
                conv = c,
                "removing directory entry for a conversation that no database holds"
            );
            control.call(Op::DirDelete(c.clone())).await?;
            sh.dir_set(&c, None);
        }
        Ok(())
    }

    async fn member_ids(&self, idx: usize, conv: &str) -> ApiResult<Vec<String>> {
        let conv = conv.to_string();
        let members = self
            .read(&self.inner.shards[idx], move |db| {
                Box::pin(async move { db.members(&conv).await })
            })
            .await?;
        Ok(members.into_iter().map(|m| m.user_id).collect())
    }

    /// Runs read-only queries on a pooled connection of one database.
    async fn read<T, F>(&self, shard: &Shard, f: F) -> ApiResult<T>
    where
        F: for<'a> FnOnce(
            &'a mut Db<'_>,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = ApiResult<T>> + Send + 'a>,
        >,
    {
        let mut c = shard
            .read
            .acquire()
            .await
            .map_err(|e| err("storage_unavailable", e.to_string()))?;
        let mut db = Db {
            c: &mut c,
            d: shard.dialect,
        };
        f(&mut db).await
    }

    fn control(&self) -> &Shard {
        &self.inner.shards[self.inner.sh.control]
    }

    pub fn config(&self) -> &Config {
        &self.inner.cfg
    }

    pub fn shared(&self) -> &Shared {
        &self.inner.sh
    }

    /// Periodic work per database: coalesced deltas, checkpoints, watchdog.
    pub fn spawn_background(&self) {
        let every = (self.inner.sh.cfg.coalesce / 2).max(std::time::Duration::from_millis(10));
        let mut t = self.inner.tickers.lock().unwrap();
        for i in 0..self.inner.shards.len() {
            let hub = self.clone();
            t.push(tokio::spawn(async move {
                let mut iv = tokio::time::interval(every);
                loop {
                    iv.tick().await;
                    hub.inner.shards[i].post(Op::Tick);
                }
            }));
        }
    }

    /// Runs one round of periodic work now (tests).
    pub async fn tick(&self) {
        for s in &self.inner.shards {
            let _ = s.call(Op::Tick).await;
        }
    }

    pub fn shutdown(&self) {
        for t in self.inner.tickers.lock().unwrap().drain(..) {
            t.abort();
        }
    }

    // ------------------------------------------------------------ sessions

    pub fn authenticate(&self, token: &str) -> Option<User> {
        let id = self.inner.sh.tokens.read().unwrap().get(token).cloned()?;
        self.inner.sh.user(&id)
    }

    pub fn connect(&self, user: User) -> (Session, mpsc::Receiver<Frame>) {
        let (sid, rx) = self.inner.sh.reg.add(&user);
        tracing::debug!(
            conn_id = sid,
            user_id = user.id,
            kind = user.kind.as_str(),
            "connected"
        );
        (
            Session {
                hub: self.clone(),
                sid,
                user,
            },
            rx,
        )
    }

    pub async fn create_user(
        &self,
        name: &str,
        kind: UserKind,
        owner: Option<&str>,
        unit: Option<&str>,
    ) -> ApiResult<(User, String)> {
        let unit = unit.unwrap_or(UNIT_DEFAULT).to_string();
        if unit != UNIT_DEFAULT && !self.inner.cfg.units.iter().any(|u| u.name == unit) {
            return Err(err("bad_request", format!("unknown unit {unit}")));
        }
        if self.inner.sh.user_by_name(name).is_some() {
            return Err(err("conflict", "name taken"));
        }
        let user = User {
            id: format!("u_{}", new_id()),
            name: name.to_string(),
            kind,
            owner_id: owner.map(String::from),
            unit_id: unit,
        };
        let token = uuid::Uuid::new_v4().simple().to_string();
        self.control()
            .call(Op::CreateUser {
                user: user.clone(),
                token: token.clone(),
            })
            .await?;
        self.inner
            .sh
            .tokens
            .write()
            .unwrap()
            .insert(token.clone(), user.id.clone());
        self.inner
            .sh
            .users
            .write()
            .unwrap()
            .insert(user.id.clone(), user.clone());
        Ok((user, token))
    }

    pub fn token_of(&self, name: &str) -> Option<(User, String)> {
        let u = self.inner.sh.user_by_name(name)?;
        let t = self
            .inner
            .sh
            .tokens
            .read()
            .unwrap()
            .iter()
            .find(|(_, id)| **id == u.id)
            .map(|(t, _)| t.clone())?;
        Some((u, t))
    }

    pub fn users(&self) -> Vec<User> {
        let mut v: Vec<User> = self
            .inner
            .sh
            .users
            .read()
            .unwrap()
            .values()
            .cloned()
            .collect();
        v.sort_by(|a, b| a.id.cmp(&b.id));
        v
    }

    // ------------------------------------------------------------ requests

    pub async fn handle_text(&self, sess: &Session, text: &str) -> Value {
        match serde_json::from_str::<Envelope>(text) {
            Ok(env) => self.handle(sess, env).await,
            Err(e) => {
                let rid = serde_json::from_str::<Value>(text)
                    .ok()
                    .and_then(|v| v["rid"].as_u64());
                metrics::counter!(m::REQUESTS, "op" => "invalid", "result" => "bad_request")
                    .increment(1);
                json!({"type": "res", "rid": rid, "ok": false, "error": {"code": "bad_request", "message": e.to_string()}})
            }
        }
    }

    pub async fn handle(&self, sess: &Session, env: Envelope) -> Value {
        let op = env.msg.name();
        let t = Instant::now();
        let res = self.dispatch(sess, env.msg).await;
        let secs = t.elapsed().as_secs_f64();
        let code = match &res {
            Ok(_) => "ok",
            Err(e) => e.code,
        };
        metrics::counter!(m::REQUESTS, "op" => op, "result" => code).increment(1);
        metrics::histogram!(m::REQUEST_DURATION, "op" => op).record(secs);
        if res.is_ok() && matches!(op, "send" | "stream_start" | "approve") {
            metrics::histogram!(m::ACK_LATENCY).record(secs);
        }
        match &res {
            Err(e) if matches!(e.code, "storage" | "storage_unavailable" | "internal") => {
                tracing::warn!(
                    conn_id = sess.sid,
                    user_id = sess.user.id,
                    op,
                    rid = env.rid,
                    error.code = e.code,
                    error = e.message,
                    "request failed"
                )
            }
            _ => tracing::debug!(
                conn_id = sess.sid,
                user_id = sess.user.id,
                op,
                rid = env.rid,
                result = code,
                latency_ms = secs * 1000.0,
                "request"
            ),
        }
        match res {
            Ok(mut v) => {
                if !v.is_object() {
                    v = json!({ "value": v });
                }
                v["type"] = json!("res");
                v["rid"] = json!(env.rid);
                v["ok"] = json!(true);
                v
            }
            Err(e) => {
                json!({"type": "res", "rid": env.rid, "ok": false, "error": {"code": e.code, "message": e.message}})
            }
        }
    }

    fn conv_shard(&self, conv: &str) -> ApiResult<&Shard> {
        let d = self
            .inner
            .sh
            .dir_get(conv)
            .ok_or_else(|| err("not_found", "conversation"))?;
        Ok(&self.inner.shards[d.db])
    }

    async fn on_conv(&self, sess: &Session, conv: &str, msg: ClientMsg) -> ApiResult<Value> {
        // A conversation can move between routing and execution; the writer
        // refuses it (`conv_moved`) and we route again with the new directory.
        for _ in 0..3 {
            let res = self
                .conv_shard(conv)?
                .call(Op::Client {
                    sess: SessCtx {
                        sid: sess.sid,
                        user: sess.user.clone(),
                    },
                    msg: msg.clone(),
                })
                .await;
            match res {
                Err(e) if e.code == "conv_moved" => continue,
                other => return other,
            }
        }
        Err(err(
            "conv_migrating",
            "conversation is being moved; retry shortly",
        ))
    }

    async fn find_conv_of<F>(&self, f: F) -> ApiResult<String>
    where
        F: for<'a> Fn(
                &'a mut Db<'_>,
            ) -> std::pin::Pin<
                Box<dyn std::future::Future<Output = ApiResult<Option<String>>> + Send + 'a>,
            > + Copy,
    {
        for s in &self.inner.shards {
            if let Some(c) = self.read(s, f).await? {
                return Ok(c);
            }
        }
        Err(err("not_found", "not found"))
    }

    async fn dispatch(&self, sess: &Session, msg: ClientMsg) -> ApiResult<Value> {
        let me = &sess.user;
        match msg {
            ClientMsg::Hello => Ok(
                json!({"me": me, "users": self.users(), "my_agents": self.inner.sh.agents_of(&me.id)}),
            ),
            ClientMsg::ListConvs => self.list_convs(me).await,
            ClientMsg::CreateConv {
                kind,
                title,
                members,
            } => self.create_conv(me, &kind, &title, &members).await,
            ClientMsg::AddMember { conv_id, user_id } => {
                let user = self
                    .inner
                    .sh
                    .user(&user_id)
                    .or_else(|| self.inner.sh.user_by_name(&user_id))
                    .ok_or_else(|| err("not_found", "user"))?;
                let mut attempt = 0;
                let (v, shard) = loop {
                    let shard = self.conv_shard(&conv_id)?;
                    match shard
                        .call(Op::AddMember {
                            conv_id: conv_id.clone(),
                            actor: me.clone(),
                            user: user.clone(),
                        })
                        .await
                    {
                        Err(e) if e.code == "conv_moved" && attempt < 3 => attempt += 1,
                        res => break (res?, shard),
                    }
                };
                if shard.idx != self.inner.sh.control {
                    self.control()
                        .call(Op::MemberIndexAdd {
                            conv_id,
                            user_id: user.id,
                        })
                        .await?;
                }
                Ok(v)
            }
            ClientMsg::Pull {
                conv_id,
                after_seq,
                limit,
                as_agent,
            } => {
                self.pull(sess, &conv_id, after_seq, limit.unwrap_or(200), as_agent)
                    .await
            }
            ClientMsg::PullSide {
                conv_id,
                after_side_seq,
            } => self.pull_side(me, &conv_id, after_side_seq).await,
            ClientMsg::Focus { conv_id, as_agent } => match &conv_id {
                Some(c) => {
                    self.reader_member(me, c, as_agent.as_deref()).await?;
                    self.inner
                        .sh
                        .reg
                        .set_focus(sess.sid, Some((c.clone(), as_agent.clone())));
                    let c = c.clone();
                    self.on_conv(
                        sess,
                        &c,
                        ClientMsg::Focus {
                            conv_id: Some(c.clone()),
                            as_agent,
                        },
                    )
                    .await
                }
                None => {
                    self.inner.sh.reg.set_focus(sess.sid, None);
                    Ok(json!({}))
                }
            },
            ClientMsg::Send { ref conv_id, .. }
            | ClientMsg::Read { ref conv_id, .. }
            | ClientMsg::StreamStart { ref conv_id, .. }
            | ClientMsg::Whisper { ref conv_id, .. }
            | ClientMsg::Takeover { ref conv_id, .. }
            | ClientMsg::Release { ref conv_id, .. }
            | ClientMsg::SetAgentPolicy { ref conv_id, .. }
            | ClientMsg::ResetGuard { ref conv_id, .. } => {
                let c = conv_id.clone();
                self.on_conv(sess, &c, msg).await
            }
            ClientMsg::Edit { ref msg_id, .. } | ClientMsg::Recall { ref msg_id, .. } => {
                let id = msg_id.clone();
                let conv = self
                    .find_conv_of(|db| {
                        Box::pin({
                            let id = id.clone();
                            async move { db.msg_conv(&id).await }
                        })
                    })
                    .await
                    .map_err(|_| err("not_found", "message"))?;
                self.on_conv(sess, &conv, msg).await
            }
            ClientMsg::StreamAppend { ref msg_id, .. }
            | ClientMsg::StreamEnd { ref msg_id }
            | ClientMsg::StreamCancel { ref msg_id } => {
                let idx = self
                    .inner
                    .sh
                    .stream_index
                    .lock()
                    .unwrap()
                    .get(msg_id)
                    .copied()
                    .ok_or_else(|| err("not_found", "no active stream"))?;
                self.inner.shards[idx]
                    .call(Op::Client {
                        sess: SessCtx {
                            sid: sess.sid,
                            user: me.clone(),
                        },
                        msg,
                    })
                    .await
            }
            ClientMsg::Approve { ref draft_id, .. } | ClientMsg::Reject { ref draft_id } => {
                let id = draft_id.clone();
                let conv = self
                    .find_conv_of(|db| {
                        Box::pin({
                            let id = id.clone();
                            async move { Ok(db.draft(&id).await?.map(|d| d.conv_id)) }
                        })
                    })
                    .await
                    .map_err(|_| err("not_found", "draft"))?;
                self.on_conv(sess, &conv, msg).await
            }
            ClientMsg::ListDrafts => {
                let agents: Vec<String> = self
                    .inner
                    .sh
                    .agents_of(&me.id)
                    .into_iter()
                    .map(|a| a.id)
                    .collect();
                let mut list = vec![];
                for s in &self.inner.shards {
                    let a = agents.clone();
                    for d in self
                        .read(s, move |db| {
                            Box::pin(async move { db.pending_drafts_for(&a).await })
                        })
                        .await?
                    {
                        list.push((d.created_at, json!({"draft_id": d.draft_id, "conv_id": d.conv_id, "agent_id": d.agent_id, "content": d.content, "created_at": d.created_at})));
                    }
                }
                list.sort_by_key(|(t, _)| *t);
                Ok(json!({"drafts": list.into_iter().map(|(_, v)| v).collect::<Vec<_>>()}))
            }
            ClientMsg::Stats => self.stats().await,
        }
    }

    /// The member whose read scope applies: the user, or (for an owner
    /// observing) the user's agent.
    async fn reader_member(
        &self,
        me: &User,
        conv: &str,
        as_agent: Option<&str>,
    ) -> ApiResult<Member> {
        let shard = self.conv_shard(conv)?;
        let who = match as_agent {
            Some(a) => {
                let agent = self
                    .inner
                    .sh
                    .user(a)
                    .ok_or_else(|| err("not_found", "agent"))?;
                if agent.kind != UserKind::Agent
                    || agent.owner_id.as_deref() != Some(me.id.as_str())
                {
                    return Err(err("forbidden", "you do not own this agent"));
                }
                agent.id
            }
            None => me.id.clone(),
        };
        let conv = conv.to_string();
        self.read(shard, move |db| {
            Box::pin(async move { db.member(&conv, &who).await })
        })
        .await?
        .ok_or_else(|| err("forbidden", "not a member of this conversation"))
    }

    async fn pull(
        &self,
        sess: &Session,
        conv: &str,
        after: i64,
        limit: i64,
        as_agent: Option<String>,
    ) -> ApiResult<Value> {
        let me = &sess.user;
        let reader = self.reader_member(me, conv, as_agent.as_deref()).await?;
        let shard = self.conv_shard(conv)?;
        if let Some(a) = &as_agent {
            shard
                .call(Op::Audit {
                    actor: me.id.clone(),
                    action: "observe".into(),
                    conv_id: conv.to_string(),
                    target: Some(a.clone()),
                    detail: json!({"after_seq": after}),
                })
                .await?;
        }
        let limit = limit.clamp(1, 1000);
        let conv_s = conv.to_string();
        let sh = self.inner.sh.clone();
        let (events, upto, has_more) = self
            .read(shard, move |db| {
                Box::pin(async move {
                    let events = db.pull(&conv_s, after, limit).await?;
                    let upto = events.last().map(|e| e.seq).unwrap_or(after);
                    let has_more = events.len() as i64 == limit;
                    let mut out = vec![];
                    for e in events {
                        if reader.kind == UserKind::Agent
                            && !Ctx::agent_can_see(db, &reader, &e).await?
                        {
                            continue;
                        }
                        let mut v = json!({"type": "event", "event": e});
                        if reader.kind == UserKind::Agent
                            && e.kind == "msg.created"
                            && let Some(msg) = &e.msg
                            && msg.sender_id != reader.user_id
                        {
                            let (t, flags) = sh.policy.inbound(&msg.content, msg.sender_type);
                            v["trust"] = json!(t.as_str());
                            v["flags"] = json!(flags);
                        }
                        out.push(v);
                    }
                    Ok((out, upto, has_more))
                })
            })
            .await?;
        let filtered = as_agent.is_some() || me.kind == UserKind::Agent;
        // `upto_seq` lets filtered readers (agents) advance without gap checks.
        Ok(
            json!({"conv_id": conv, "events": events, "upto_seq": upto, "has_more": has_more, "filtered": filtered}),
        )
    }

    async fn pull_side(&self, me: &User, conv: &str, after: i64) -> ApiResult<Value> {
        let shard = self.conv_shard(conv)?;
        let c = conv.to_string();
        let members = self
            .read(shard, move |db| {
                Box::pin(async move { db.members(&c).await })
            })
            .await?;
        let is_member = members.iter().any(|m| m.user_id == me.id);
        let owns_member = members
            .iter()
            .any(|m| m.owner_id.as_deref() == Some(me.id.as_str()));
        if !is_member && !owns_member {
            return Err(err("forbidden", "no access"));
        }
        let c = conv.to_string();
        let me_id = me.id.clone();
        let list: Vec<SideEvent> = self
            .read(shard, move |db| {
                Box::pin(async move { db.pull_side(&c, after).await })
            })
            .await?
            .into_iter()
            .filter(|s| s.audience.contains(&me_id))
            .collect();
        Ok(json!({"conv_id": conv, "side_events": list}))
    }

    async fn list_convs(&self, me: &User) -> ApiResult<Value> {
        let sh = &self.inner.sh;
        let mut ids = vec![me.id.clone()];
        ids.extend(sh.agents_of(&me.id).into_iter().map(|a| a.id));
        let pairs = self
            .read(self.control(), move |db| {
                Box::pin(async move { db.member_index_for(&ids).await })
            })
            .await?;
        // conv → None (direct member) or Some(agent) (observable through an agent)
        let mut convs: BTreeMap<String, Option<String>> = BTreeMap::new();
        for (conv, user) in pairs {
            let via = if user == me.id { None } else { Some(user) };
            match convs.get(&conv) {
                Some(None) => {}
                _ => {
                    if via.is_none() || !convs.contains_key(&conv) {
                        convs.insert(conv, via);
                    }
                }
            }
        }
        let mut out = vec![];
        for (conv, via) in convs {
            let Some(d) = sh.dir_get(&conv) else { continue };
            let shard = &self.inner.shards[d.db];
            let (c, me_id, direct) = (conv.clone(), me.id.clone(), via.is_none());
            let row = self
                .read(shard, move |db| {
                    Box::pin(async move {
                        let Some(row) = db.conv(&c).await? else {
                            return Ok(None);
                        };
                        let members = db.members(&c).await?;
                        let unread = if direct {
                            db.unread(&c, &me_id).await?
                        } else {
                            0
                        };
                        Ok(Some((row, members, unread)))
                    })
                })
                .await?;
            let Some((row, members, unread)) = row else {
                continue;
            };
            let members: Vec<Value> = members
                .iter()
                .map(|m| json!({"user_id": m.user_id, "name": m.name, "kind": m.kind, "owner_id": m.owner_id, "role": m.role, "trigger": m.trigger, "approval": m.approval}))
                .collect();
            out.push(json!({"conv_id": row.id, "kind": row.kind, "title": row.title, "unit": row.unit_id, "database": sh.db_names[d.db], "last_seq": row.last_seq, "unread": unread, "members": members, "observer_via": via}));
        }
        Ok(json!({"convs": out}))
    }

    async fn create_conv(
        &self,
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
        if title.is_empty() || title.len() > 255 {
            return Err(err("bad_request", "title must be 1..255 bytes"));
        }
        let sh = &self.inner.sh;
        let users = members
            .iter()
            .map(|k| {
                sh.user(k)
                    .or_else(|| sh.user_by_name(k))
                    .ok_or_else(|| err("not_found", format!("user {k}")))
            })
            .collect::<ApiResult<Vec<_>>>()?;
        // New conversations live in the database of the creator's unit.
        let db_name = self.inner.cfg.unit_database(&me.unit_id).to_string();
        let idx = sh.db_names.iter().position(|n| *n == db_name).unwrap();
        let id = format!("c_{}", new_id());
        // Routable before its first events are pushed.
        sh.dir_set(
            &id,
            Some(DirInfo {
                db: idx,
                unit: me.unit_id.clone(),
                migrating: false,
            }),
        );
        let res = self.inner.shards[idx]
            .call(Op::CreateConv {
                id: id.clone(),
                kind: kind.into(),
                title: title.into(),
                unit: me.unit_id.clone(),
                creator: me.clone(),
                members: users.clone(),
            })
            .await;
        if let Err(e) = res {
            sh.dir_set(&id, None);
            return Err(e);
        }
        if idx != sh.control {
            let mut ids: Vec<String> = vec![me.id.clone()];
            ids.extend(users.iter().map(|u| u.id.clone()));
            ids.dedup();
            self.control()
                .call(Op::RegisterConv {
                    entry: DirEntry {
                        conv_id: id.clone(),
                        database: db_name,
                        unit_id: me.unit_id.clone(),
                        migrating: false,
                    },
                    member_ids: ids,
                })
                .await?;
        }
        Ok(json!({"conv_id": id}))
    }

    // ------------------------------------------------------------- operate

    /// Readiness: every database accepts writes and no queue is near full.
    pub fn health(&self) -> Value {
        let mut ready = true;
        let dbs: Vec<Value> = self
            .inner
            .shards
            .iter()
            .map(|s| {
                let failed = s.health.failed();
                let saturated = s.health.depth() * 10 >= s.queue_capacity * 9;
                if failed.is_some() || saturated {
                    ready = false;
                }
                json!({"database": s.name, "failed": failed, "queue_depth": s.health.depth(), "saturated": saturated})
            })
            .collect();
        json!({"ready": ready, "databases": dbs})
    }

    pub fn storage_failed(&self) -> Option<String> {
        self.inner
            .shards
            .iter()
            .find_map(|s| s.health.failed().map(|r| format!("{}: {r}", s.name)))
    }

    pub async fn stats(&self) -> ApiResult<Value> {
        let mut counts: BTreeMap<String, i64> = BTreeMap::new();
        let mut streams = vec![];
        let mut audit = vec![];
        let mut writers = vec![];
        for s in &self.inner.shards {
            let v = s.call(Op::Stats).await?;
            if let Some(o) = v["counts"].as_object() {
                for (k, n) in o {
                    *counts.entry(k.clone()).or_default() += n.as_i64().unwrap_or(0);
                }
            }
            streams.extend(v["active_streams"].as_array().cloned().unwrap_or_default());
            audit.extend(v["audit"].as_array().cloned().unwrap_or_default());
            writers.push(s.writer_stats());
        }
        let users = self.users();
        counts.insert(
            "users".into(),
            users.iter().filter(|u| u.kind == UserKind::Human).count() as i64,
        );
        counts.insert(
            "agents".into(),
            users.iter().filter(|u| u.kind == UserKind::Agent).count() as i64,
        );
        audit.sort_by_key(|a| std::cmp::Reverse(a["ts"].as_i64().unwrap_or(0)));
        audit.truncate(50);
        let names: HashMap<String, String> = users
            .iter()
            .map(|u| (u.id.clone(), u.name.clone()))
            .collect();
        for a in audit.iter_mut() {
            let n = a["actor_id"].as_str().and_then(|id| names.get(id)).cloned();
            a["actor"] = json!(n);
        }
        let online: Vec<String> = self
            .inner
            .sh
            .reg
            .online_users()
            .iter()
            .filter_map(|id| names.get(id).cloned())
            .collect();
        Ok(
            json!({"counts": counts, "active_streams": streams, "online": online, "sessions": self.inner.sh.reg.session_count(), "audit": audit, "writers": writers, "health": self.health()}),
        )
    }

    pub fn writer_stats(&self) -> Vec<Value> {
        self.inner.shards.iter().map(|s| s.writer_stats()).collect()
    }

    /// Fault injection for tests: the next `n` commits of `database` fail.
    pub fn inject_commit_failures(&self, database: &str, n: u32) {
        if let Some(s) = self.inner.shards.iter().find(|s| s.name == database) {
            s.health.fail_next_commits.store(n, Ordering::SeqCst);
        }
    }

    // ----------------------------------------------------------- migration

    /// Moves one conversation to another database (docs/02-architecture.md
    /// §5.4): read-only → copy → verify → switch directory → delete source.
    /// Writes to the conversation are refused with `conv_migrating` meanwhile.
    pub async fn migrate_conv(&self, conv: &str, target: &str) -> ApiResult<Value> {
        let sh = &self.inner.sh;
        let info = sh
            .dir_get(conv)
            .ok_or_else(|| err("not_found", "conversation"))?;
        let dst = sh
            .db_names
            .iter()
            .position(|n| n == target)
            .ok_or_else(|| err("bad_request", format!("unknown database {target}")))?;
        if info.db == dst {
            return Ok(json!({"conv_id": conv, "moved": false}));
        }
        let (src_s, dst_s) = (&self.inner.shards[info.db], &self.inner.shards[dst]);
        let entry = |db: usize, migrating: bool| DirEntry {
            conv_id: conv.to_string(),
            database: sh.db_names[db].clone(),
            unit_id: info.unit.clone(),
            migrating,
        };
        sh.dir_set(
            conv,
            Some(DirInfo {
                migrating: true,
                ..info.clone()
            }),
        );
        let result: ApiResult<String> = async {
            self.control()
                .call(Op::DirPut(entry(info.db, true)))
                .await?;
            src_s.call(Op::StopConvStreams(conv.to_string())).await?;
            src_s.call(Op::Barrier).await?;
            let c = conv.to_string();
            let rows = self
                .read(src_s, move |db| {
                    Box::pin(async move { db.export_conv(&c).await })
                })
                .await?;
            let want = fingerprint(&rows);
            dst_s
                .call(Op::ImportConv {
                    conv_id: conv.to_string(),
                    rows,
                })
                .await?;
            let c = conv.to_string();
            let got = fingerprint(
                &self
                    .read(dst_s, move |db| {
                        Box::pin(async move { db.export_conv(&c).await })
                    })
                    .await?,
            );
            if got != want {
                return Err(err("migration_failed", "copy does not match the source"));
            }
            self.control().call(Op::DirPut(entry(dst, false))).await?;
            Ok(want)
        }
        .await;
        match result {
            Ok(fp) => {
                sh.dir_set(
                    conv,
                    Some(DirInfo {
                        db: dst,
                        unit: info.unit.clone(),
                        migrating: false,
                    }),
                );
                if let Err(e) = src_s.call(Op::DeleteConv(conv.to_string())).await {
                    // Reconcile removes the stale copy at the next start.
                    tracing::warn!(conv, error = e.message, "source copy not deleted");
                }
                if dst != sh.control {
                    for u in self.member_ids(dst, conv).await? {
                        self.control()
                            .call(Op::MemberIndexAdd {
                                conv_id: conv.to_string(),
                                user_id: u,
                            })
                            .await?;
                    }
                }
                metrics::counter!(m::MIGRATIONS, "result" => "ok").increment(1);
                tracing::info!(
                    conv,
                    from = sh.db_names[info.db],
                    to = target,
                    "conversation migrated"
                );
                Ok(
                    json!({"conv_id": conv, "moved": true, "from": sh.db_names[info.db], "to": target, "fingerprint": fp}),
                )
            }
            Err(e) => {
                metrics::counter!(m::MIGRATIONS, "result" => "failed").increment(1);
                let _ = dst_s.call(Op::DeleteConv(conv.to_string())).await;
                let _ = self.control().call(Op::DirPut(entry(info.db, false))).await;
                sh.dir_set(
                    conv,
                    Some(DirInfo {
                        migrating: false,
                        ..info
                    }),
                );
                Err(e)
            }
        }
    }

    /// Moves every conversation of `unit` into the database the
    /// configuration now assigns to it.
    pub async fn migrate_unit(&self, unit: &str) -> ApiResult<Value> {
        let target = self.inner.cfg.unit_database(unit).to_string();
        let sh = &self.inner.sh;
        let tidx = sh.db_names.iter().position(|n| *n == target).unwrap();
        let convs: Vec<String> = sh
            .dir
            .read()
            .unwrap()
            .iter()
            .filter(|(_, d)| d.unit == unit && d.db != tidx)
            .map(|(c, _)| c.clone())
            .collect();
        let mut moved = vec![];
        for c in &convs {
            self.migrate_conv(c, &target).await?;
            moved.push(c.clone());
        }
        let _: HashSet<&String> = moved.iter().collect();
        Ok(json!({"unit": unit, "database": target, "moved": moved.len()}))
    }
}

impl Session {
    pub async fn handle_text(&self, text: &str) -> Value {
        self.hub.handle_text(self, text).await
    }
}
