//! A deterministic world for the core: a fake AgentIM server (the subset of
//! the protocol the client uses, with the same rules: per-device positions,
//! in-order pushes or `resync`, client_msg_id dedup), a fake store, a fake
//! clock and network with scripted or random faults. Effects are carried
//! out in order, as the runtimes do.

#![allow(dead_code)]

use std::cmp::Reverse;
use std::collections::{BTreeMap, BinaryHeap, HashMap, HashSet, VecDeque};

use agentim_client_core::*;
use serde_json::{Value, json};

pub const ME: &str = "u_alice";

// ---------------------------------------------------------------- server

pub struct Server {
    pub logs: BTreeMap<String, Vec<Value>>,
    /// Current state of every message.
    pub cur: HashMap<String, Value>,
    pub cms: HashMap<String, String>,
    /// The device's acknowledged position per conversation.
    pub acked: HashMap<String, i64>,
    /// What the current connection was sent, per conversation.
    pub sent: HashMap<String, i64>,
    pub synced: bool,
    pub conn: Option<u64>,
    pub batch: usize,
    pub heartbeat_ms: i64,
    /// Errors to answer instead of doing the op, per op name.
    pub reject: HashMap<String, VecDeque<Value>>,
    pub forbidden: HashSet<String>,
    pub next_id: u64,
    pub now: i64,
    /// Every send applied, by client_msg_id (to count duplicates).
    pub applied_cms: Vec<String>,
    pub acks_seen: Vec<(String, i64)>,
    pub requests: Vec<Value>,
    /// Pushes the server drops (its queue overflowed): it sends `resync`.
    pub overflow: usize,
    /// Member lists `list_convs` returns (none: the field is left out).
    pub members: HashMap<String, Value>,
}

impl Server {
    pub fn new() -> Server {
        Server {
            logs: BTreeMap::new(),
            cur: HashMap::new(),
            cms: HashMap::new(),
            acked: HashMap::new(),
            sent: HashMap::new(),
            synced: false,
            conn: None,
            batch: 200,
            heartbeat_ms: 25_000,
            reject: HashMap::new(),
            forbidden: HashSet::new(),
            next_id: 0,
            now: 0,
            applied_cms: vec![],
            acks_seen: vec![],
            requests: vec![],
            overflow: 0,
            members: HashMap::new(),
        }
    }

    pub fn conv(&mut self, id: &str) {
        if !self.logs.contains_key(id) {
            self.logs.insert(id.to_string(), vec![]);
            self.event(id, "conv.created", json!({"title": id}), None);
        }
    }

    pub fn last(&self, conv: &str) -> i64 {
        self.logs.get(conv).map(|l| l.len() as i64).unwrap_or(0)
    }

    /// Appends an event; returns it and the frames for the connection.
    fn event(
        &mut self,
        conv: &str,
        kind: &str,
        data: Value,
        msg: Option<Value>,
    ) -> (Value, Vec<String>) {
        let log = self.logs.get_mut(conv).unwrap();
        let seq = log.len() as i64 + 1;
        let mut ev = json!({"conv_id": conv, "seq": seq, "type": kind, "actor_id": ME, "data": data, "created_at": self.now});
        if let Some(m) = msg {
            ev["actor_id"] = m["sender_id"].clone();
            ev["msg"] = m;
        }
        log.push(ev.clone());
        let mut frames = vec![];
        if self.conn.is_some() && self.synced {
            match self.sent.get(conv) {
                Some(s) if *s == seq - 1 && self.overflow > 0 => {
                    self.overflow -= 1;
                    frames.push(json!({"type": "resync", "conv_id": conv}).to_string());
                }
                Some(s) if *s == seq - 1 => {
                    self.sent.insert(conv.to_string(), seq);
                    frames.push(json!({"type": "event", "event": ev}).to_string());
                }
                _ => frames.push(json!({"type": "resync", "conv_id": conv}).to_string()),
            }
        }
        (ev, frames)
    }

    fn new_msg(&mut self, conv: &str, sender: &str, text: &str, cm: Option<&str>) -> Value {
        self.next_id += 1;
        let seq = self.last(conv) + 1;
        let mut m = json!({
            "msg_id": format!("m{}", self.next_id), "conv_id": conv, "created_seq": seq,
            "sender_id": sender, "sender_name": sender, "sender_type": "human", "kind": "text",
            "content": text, "version": 1, "state": "committed", "created_at": self.now,
            "updated_at": self.now,
        });
        if let Some(c) = cm {
            m["client_msg_id"] = json!(c);
        }
        m
    }

    /// Someone else says something.
    pub fn other_says(&mut self, conv: &str, text: &str) -> Vec<String> {
        self.conv(conv);
        let m = self.new_msg(conv, "u_bob", text, None);
        self.cur
            .insert(m["msg_id"].as_str().unwrap().to_string(), m.clone());
        self.event(conv, "msg.created", json!({}), Some(m)).1
    }

    pub fn welcome(&self) -> String {
        json!({"type": "welcome", "me": {"id": ME, "name": "alice"}, "heartbeat_ms": self.heartbeat_ms, "idle_ms": 90_000, "device_id": "d_x"}).to_string()
    }

    pub fn open(&mut self, conn: u64) {
        self.conn = Some(conn);
        self.sent.clear();
        self.synced = false;
    }

    pub fn close(&mut self) {
        self.conn = None;
        self.sent.clear();
        self.synced = false;
    }

    /// One request; the frames to send back (the answer last, pushes first).
    pub fn handle(&mut self, text: &str) -> Vec<String> {
        let r: Value = serde_json::from_str(text).unwrap();
        self.requests.push(r.clone());
        let op = r["op"].as_str().unwrap().to_string();
        let rid = r["rid"].clone();
        let ok = |mut v: Value| {
            v["type"] = json!("res");
            v["rid"] = rid.clone();
            v["ok"] = json!(true);
            v.to_string()
        };
        let fail =
            |e: Value| json!({"type": "res", "rid": rid, "ok": false, "error": e}).to_string();
        if let Some(e) = self.reject.get_mut(&op).and_then(|q| q.pop_front()) {
            return vec![fail(e)];
        }
        let mut pushes = vec![];
        let res = match op.as_str() {
            "hello" => ok(json!({"me": {"id": ME, "name": "alice", "permissions": {}}})),
            "list_convs" => {
                let convs: Vec<Value> = self
                    .logs
                    .keys()
                    .map(|c| {
                        let mut v = json!({"conv_id": c, "kind": "group", "title": c, "last_seq": self.last(c), "unread": 0});
                        if let Some(m) = self.members.get(c) {
                            v["members"] = m.clone();
                        }
                        v
                    })
                    .collect();
                ok(json!({"convs": convs}))
            }
            "ping" => ok(json!({"t": self.now})),
            "focus" | "feedback" | "respond" => ok(json!({})),
            "sync" => {
                self.synced = true;
                let mut convs = vec![];
                let mut more = false;
                let ids: Vec<String> = self.logs.keys().cloned().collect();
                for c in ids {
                    let last = self.last(&c);
                    let from = match self.sent.get(&c) {
                        Some(s) => *s,
                        None => match self.acked.get(&c) {
                            Some(a) => *a,
                            None => (last - 30).max(0),
                        },
                    };
                    if from >= last && self.sent.contains_key(&c) {
                        continue;
                    }
                    let upto = (from + self.batch as i64).min(last);
                    more |= upto < last;
                    let events: Vec<Value> = self.logs[&c][from as usize..upto as usize]
                        .iter()
                        .map(|e| json!({"event": e}))
                        .collect();
                    self.sent.insert(c.clone(), upto);
                    convs.push(json!({"conv_id": c, "events": events, "upto": upto, "side_upto": 0, "last_seq": last}));
                }
                ok(json!({"convs": convs, "more": more}))
            }
            "ack" => {
                for a in r["acks"].as_array().unwrap() {
                    let c = a["conv_id"].as_str().unwrap().to_string();
                    if let Some(s) = a["seq"].as_i64() {
                        let sent = self.sent.get(&c).copied().unwrap_or(0);
                        assert!(
                            s <= sent,
                            "acknowledged {s} beyond what was sent ({sent}) in {c}"
                        );
                        let cur = self.acked.entry(c.clone()).or_insert(0);
                        *cur = (*cur).max(s);
                        self.acks_seen.push((c, s));
                    }
                }
                ok(json!({}))
            }
            "heads" => {
                let mut h = json!({});
                for c in r["conv_ids"].as_array().unwrap() {
                    let c = c.as_str().unwrap();
                    h[c] = json!(self.last(c));
                }
                ok(json!({"heads": h}))
            }
            "send" => {
                let conv = r["conv_id"].as_str().unwrap().to_string();
                let cm = r["client_msg_id"].as_str().unwrap().to_string();
                if self.forbidden.contains(&conv) || !self.logs.contains_key(&conv) {
                    fail(json!({"code": "forbidden", "message": "not a member"}))
                } else if let Some(id) = self.cms.get(&cm) {
                    let m = &self.cur[id];
                    ok(json!({"msg_id": id, "seq": m["created_seq"], "duplicate": true}))
                } else {
                    let m = self.new_msg(&conv, ME, r["text"].as_str().unwrap_or(""), Some(&cm));
                    let id = m["msg_id"].as_str().unwrap().to_string();
                    self.cms.insert(cm.clone(), id.clone());
                    self.applied_cms.push(cm);
                    let mut stored = m.clone();
                    stored.as_object_mut().unwrap().remove("client_msg_id");
                    self.cur.insert(id.clone(), stored);
                    let seq = m["created_seq"].clone();
                    pushes = self.event(&conv, "msg.created", json!({}), Some(m)).1;
                    ok(json!({"msg_id": id, "seq": seq}))
                }
            }
            "edit" | "recall" => {
                let id = r["msg_id"].as_str().unwrap().to_string();
                match self.cur.get(&id).cloned() {
                    None => fail(json!({"code": "not_found", "message": "no such message"})),
                    Some(m) if m["state"] == "recalled" => {
                        fail(json!({"code": "conflict", "message": "recalled"}))
                    }
                    Some(mut m) => {
                        m["version"] = json!(m["version"].as_i64().unwrap() + 1);
                        let kind = if op == "edit" {
                            m["content"] = r["text"].clone();
                            "msg.edited"
                        } else {
                            m["content"] = json!("");
                            m["state"] = json!("recalled");
                            "msg.recalled"
                        };
                        self.cur.insert(id.clone(), m.clone());
                        let conv = m["conv_id"].as_str().unwrap().to_string();
                        pushes = self.event(&conv, kind, json!({}), Some(m)).1;
                        ok(json!({"msg_id": id}))
                    }
                }
            }
            "read" => ok(json!({})),
            "history" => {
                let conv = r["conv_id"].as_str().unwrap().to_string();
                let anchor = match &r["anchor"] {
                    Value::Number(n) => n.as_i64().unwrap(),
                    _ => i64::MAX,
                };
                let before = r["before"].as_i64().unwrap_or(50);
                let after = r["after"].as_i64().unwrap_or(0);
                let mut all: Vec<Value> = self
                    .cur
                    .values()
                    .filter(|m| m["conv_id"] == conv.as_str())
                    .cloned()
                    .collect();
                all.sort_by_key(|m| m["created_seq"].as_i64());
                let down: Vec<Value> = all
                    .iter()
                    .filter(|m| m["created_seq"].as_i64().unwrap() <= anchor)
                    .cloned()
                    .collect();
                let up: Vec<Value> = all
                    .iter()
                    .filter(|m| m["created_seq"].as_i64().unwrap() > anchor)
                    .cloned()
                    .collect();
                let has_before = down.len() as i64 > before;
                let has_after = up.len() as i64 > after;
                let mut page: Vec<Value> =
                    down[down.len().saturating_sub(before as usize)..].to_vec();
                page.extend(up.into_iter().take(after as usize));
                let mut v = json!({"conv_id": conv, "messages": page, "has_before": has_before, "has_after": has_after});
                if !has_before {
                    v["before_end"] = json!("start");
                }
                ok(v)
            }
            _ => fail(json!({"code": "unknown_op", "message": op})),
        };
        pushes.push(res);
        pushes
    }
}

// ---------------------------------------------------------------- store

#[derive(Default, Clone)]
pub struct Store {
    pub account: Option<String>,
    pub convs: BTreeMap<String, ConvRow>,
    pub msgs: BTreeMap<(String, String), Value>,
    pub ops: BTreeMap<String, Value>,
    pub writes: usize,
}

impl Store {
    pub fn apply(&mut self, b: &Batch) {
        self.writes += 1;
        for c in &b.convs {
            self.convs.insert(c.conv_id.clone(), c.clone());
        }
        for m in &b.put_messages {
            let k = (
                m["conv_id"].as_str().unwrap().to_string(),
                m["msg_id"].as_str().unwrap().to_string(),
            );
            self.msgs.insert(k, m.clone());
        }
        for k in &b.remove_messages {
            self.msgs.remove(k);
        }
        for o in &b.put_ops {
            self.ops
                .insert(o["id"].as_str().unwrap().to_string(), o.clone());
        }
        for o in &b.remove_ops {
            self.ops.remove(o);
        }
        if let Some(m) = &b.meta {
            self.account = m["account"].as_str().map(String::from);
        }
    }

    pub fn snapshot(&self) -> Snapshot {
        let mut messages = vec![];
        for c in self.convs.keys() {
            let mut ms: Vec<Value> = self
                .msgs
                .iter()
                .filter(|((cc, _), _)| cc == c)
                .map(|(_, m)| m.clone())
                .collect();
            ms.sort_by_key(|m| m["created_seq"].as_i64());
            let n = ms.len().saturating_sub(PAGE as usize);
            messages.extend(ms.drain(n..));
        }
        Snapshot {
            account: self.account.clone(),
            convs: self.convs.values().cloned().collect(),
            ops: self.ops.values().cloned().collect(),
            messages,
        }
    }

    pub fn page(&self, conv: &str, before: i64, limit: i64) -> Vec<Value> {
        let mut ms: Vec<Value> = self
            .msgs
            .iter()
            .filter(|((c, _), m)| c == conv && m["created_seq"].as_i64().unwrap() < before)
            .map(|(_, m)| m.clone())
            .collect();
        ms.sort_by_key(|m| m["created_seq"].as_i64());
        let n = ms.len().saturating_sub(limit as usize);
        ms.drain(n..).collect()
    }
}

// ---------------------------------------------------------------- world

#[derive(Debug, Clone)]
pub enum Outcome {
    Accept,
    Refuse(u16, Option<i64>),
    /// Nothing happens (the attempt times out).
    Hang,
    Unreachable,
}

enum Delivery {
    Input(Input),
    Frame(u64, String),
    Closed(u64, u16, String),
}

pub struct World {
    pub core: Core,
    pub now: i64,
    pub server: Server,
    pub store: Store,
    queue: BinaryHeap<Reverse<(i64, u64, usize)>>,
    items: HashMap<usize, Delivery>,
    n: u64,
    pub latency: i64,
    pub connects: Vec<String>,
    /// The token each connect carried (beside the URL, never in it).
    pub connect_tokens: Vec<String>,
    pub plan: VecDeque<Outcome>,
    pub tokens: VecDeque<Result<String, bool>>,
    pub token_calls: usize,
    pub conn: u64,
    pub open: bool,
    pub events: Vec<Value>,
    pub replies: HashMap<u64, Result<Value, ClientError>>,
    pub next_cmd: u64,
    /// Push frames to lose (a gap), counted down.
    pub lose_pushes: usize,
    /// Answers to lose (a request without a reply).
    pub lose_answers: usize,
    /// The store refuses writes with this kind.
    pub store_fail: Option<String>,
    /// The app's copy of watched conversations, from the events.
    pub views: HashMap<String, BTreeMap<String, Value>>,
    pub effects: Vec<Effect>,
    /// Acknowledgements seen while the store held less (must stay empty).
    pub early_acks: Vec<String>,
}

impl World {
    pub fn new(seed: u64) -> World {
        let core = Core::new(Config {
            url: "https://im.example".into(),
            device_id: "dev1".into(),
            device_name: Some("Phone".into()),
            platform: None,
            client_name: "test".into(),
            client_version: "1".into(),
            seed,
        });
        let mut w = World {
            core,
            now: 1_000_000,
            server: Server::new(),
            store: Store::default(),
            queue: BinaryHeap::new(),
            items: HashMap::new(),
            n: 0,
            latency: 20,
            connects: vec![],
            connect_tokens: vec![],
            plan: VecDeque::new(),
            tokens: VecDeque::new(),
            token_calls: 0,
            conn: 0,
            open: false,
            events: vec![],
            replies: HashMap::new(),
            next_cmd: 0,
            lose_pushes: 0,
            lose_answers: 0,
            store_fail: None,
            views: HashMap::new(),
            effects: vec![],
            early_acks: vec![],
        };
        w.server.now = w.now;
        w
    }

    /// Starts like an app: the cache is read, then `connect`.
    pub fn start(&mut self) {
        let s = self.store.snapshot();
        self.input(Input::Restored(s));
    }

    /// A new process on the same store (the old core is gone).
    pub fn restart(&mut self, seed: u64) {
        self.queue.clear();
        self.items.clear();
        if self.open {
            self.server.close();
            self.open = false;
        }
        self.core = Core::new(Config {
            url: "https://im.example".into(),
            device_id: "dev1".into(),
            device_name: Some("Phone".into()),
            platform: None,
            client_name: "test".into(),
            client_version: "1".into(),
            seed,
        });
        self.views.clear();
        self.start();
    }

    fn later(&mut self, delay: i64, d: Delivery) {
        self.n += 1;
        let k = self.n as usize;
        self.items.insert(k, d);
        self.queue.push(Reverse((self.now + delay, self.n, k)));
    }

    pub fn input(&mut self, i: Input) {
        let fx = self.core.handle(i, self.now);
        self.carry_out(fx);
    }

    pub fn cmd(&mut self, c: Value) -> u64 {
        self.next_cmd += 1;
        let id = self.next_cmd;
        let watch = (c["cmd"] == "messages")
            .then(|| c["conv_id"].as_str().map(String::from))
            .flatten();
        self.input(Input::Command { id, cmd: c });
        // The app starts its copy from the snapshot, then applies changes.
        if let (Some(conv), Some(Ok(r))) = (watch, self.replies.get(&id)) {
            let view = self.views.entry(conv).or_default();
            view.clear();
            for m in r["messages"].as_array().unwrap() {
                view.insert(m["key"].as_str().unwrap().to_string(), m.clone());
            }
        }
        id
    }

    /// A command answered at once (local ones are).
    pub fn ask(&mut self, c: Value) -> Result<Value, ClientError> {
        let id = self.cmd(c);
        self.replies.remove(&id).expect("answered at once")
    }

    fn carry_out(&mut self, fx: Vec<Effect>) {
        for e in fx {
            self.effects.push(e.clone());
            match e {
                Effect::NeedToken => {
                    self.token_calls += 1;
                    let t = self
                        .tokens
                        .pop_front()
                        .unwrap_or(Ok(format!("tok{}", self.token_calls)));
                    self.later(self.latency, Delivery::Input(Input::Token(t)));
                }
                Effect::Connect { url, token } => {
                    self.connects.push(url);
                    self.connect_tokens.push(token);
                    match self.plan.pop_front().unwrap_or(Outcome::Accept) {
                        Outcome::Accept => {
                            self.conn += 1;
                            let c = self.conn;
                            self.server.open(c);
                            self.open = true;
                            let w = self.server.welcome();
                            self.later(self.latency, Delivery::Input(Input::Opened));
                            self.later(self.latency, Delivery::Frame(c, w));
                        }
                        Outcome::Refuse(s, ra) => self.later(
                            self.latency,
                            Delivery::Input(Input::ConnectFailed {
                                status: Some(s),
                                retry_after_ms: ra,
                                detail: "refused".into(),
                            }),
                        ),
                        Outcome::Unreachable => self.later(
                            self.latency,
                            Delivery::Input(Input::ConnectFailed {
                                status: None,
                                retry_after_ms: None,
                                detail: "unreachable".into(),
                            }),
                        ),
                        Outcome::Hang => {}
                    }
                }
                Effect::Send(text) => {
                    if !self.open {
                        continue;
                    }
                    self.server.now = self.now;
                    let frames = self.server.handle(&text);
                    self.check_acks();
                    let c = self.conn;
                    let n = frames.len();
                    for (i, f) in frames.into_iter().enumerate() {
                        let is_answer = i + 1 == n;
                        if is_answer && self.lose_answers > 0 {
                            self.lose_answers -= 1;
                            continue;
                        }
                        if !is_answer && f.contains("\"type\":\"event\"") && self.lose_pushes > 0 {
                            self.lose_pushes -= 1;
                            continue;
                        }
                        self.later(self.latency, Delivery::Frame(c, f));
                    }
                }
                Effect::Close { .. } => {
                    if self.open {
                        self.server.close();
                        self.open = false;
                    }
                }
                Effect::Persist(b) => match self.store_fail.clone() {
                    Some(kind) => {
                        // The queue still writes (its own small store).
                        if !b.put_ops.is_empty() || !b.remove_ops.is_empty() {
                            let q = Batch {
                                put_ops: b.put_ops.clone(),
                                remove_ops: b.remove_ops.clone(),
                                ..Default::default()
                            };
                            self.store.apply(&q);
                        }
                        let fx = self.core.handle(Input::StorageFailed { kind }, self.now);
                        self.carry_out(fx);
                    }
                    None => {
                        self.store.apply(&b);
                        let fx = self.core.handle(Input::Persisted(b.id), self.now);
                        self.carry_out(fx);
                    }
                },
                Effect::LoadPage {
                    req,
                    conv_id,
                    before_seq,
                    limit,
                } => {
                    let messages = self.store.page(&conv_id, before_seq, limit);
                    self.later(
                        1,
                        Delivery::Input(Input::PageLoaded {
                            req,
                            conv_id,
                            messages,
                        }),
                    );
                }
                Effect::WipeStore => self.store = Store::default(),
                Effect::Emit(v) => {
                    if v["type"] == "messages" {
                        let view = self
                            .views
                            .entry(v["conv_id"].as_str().unwrap().to_string())
                            .or_default();
                        if v["reset"] == true {
                            view.clear();
                        }
                        for ch in v["changes"].as_array().unwrap() {
                            if ch["op"] == "upsert" {
                                view.insert(
                                    ch["message"]["key"].as_str().unwrap().to_string(),
                                    ch["message"].clone(),
                                );
                            } else {
                                view.remove(ch["key"].as_str().unwrap());
                            }
                        }
                    }
                    self.events.push(v);
                }
                Effect::Reply { id, result } => {
                    self.replies.insert(id, result);
                }
            }
        }
    }

    /// Nothing is acknowledged that the store does not hold (G2).
    fn check_acks(&mut self) {
        for (c, s) in std::mem::take(&mut self.server.acks_seen) {
            let stored = self.store.convs.get(&c).map(|r| r.last_seq).unwrap_or(0);
            if s > stored {
                self.early_acks
                    .push(format!("{c}: acknowledged {s}, stored {stored}"));
            }
        }
    }

    /// Server-side activity: frames to the client.
    pub fn deliver(&mut self, frames: Vec<String>) {
        let c = self.conn;
        for f in frames {
            if f.contains("\"type\":\"event\"") && self.lose_pushes > 0 {
                self.lose_pushes -= 1;
                continue;
            }
            if self.open {
                self.later(self.latency, Delivery::Frame(c, f));
            }
        }
    }

    pub fn other_says(&mut self, conv: &str, text: &str) {
        self.server.now = self.now;
        let f = self.server.other_says(conv, text);
        self.deliver(f);
    }

    /// The server drops the connection (or the network does).
    pub fn cut(&mut self, code: u16, reason: &str) {
        if self.open {
            self.server.close();
            self.open = false;
            let c = self.conn;
            self.later(self.latency, Delivery::Closed(c, code, reason.into()));
        }
    }

    /// Runs the world for `ms` of virtual time.
    pub fn run(&mut self, ms: i64) {
        let end = self.now + ms;
        loop {
            let next_q = self.queue.peek().map(|Reverse((t, _, _))| *t);
            let next_d = self.core.next_deadline();
            let next = match (next_q, next_d) {
                (Some(a), Some(b)) => a.min(b),
                (Some(a), None) => a,
                (None, Some(b)) => b,
                (None, None) => break,
            };
            if next > end {
                break;
            }
            self.now = self.now.max(next);
            if next_q == Some(next) {
                let Reverse((_, _, k)) = self.queue.pop().unwrap();
                match self.items.remove(&k).unwrap() {
                    Delivery::Input(i) => self.input(i),
                    Delivery::Frame(c, f) => {
                        if c == self.conn && self.open {
                            self.input(Input::Frame(f))
                        }
                    }
                    Delivery::Closed(c, code, reason) => {
                        if c == self.conn {
                            self.input(Input::Closed { code, reason })
                        }
                    }
                }
            } else {
                self.input(Input::Tick);
            }
        }
        self.now = end;
        self.input(Input::Tick);
    }

    pub fn state(&self) -> &'static str {
        self.core.state().as_str()
    }

    pub fn connected(&mut self) {
        self.start();
        self.cmd(json!({"cmd": "connect"}));
        self.run(1_000);
        assert_eq!(
            self.state(),
            "connected",
            "{:?}",
            self.events.iter().rev().take(5).collect::<Vec<_>>()
        );
    }

    pub fn emitted(&self, ty: &str) -> Vec<&Value> {
        self.events.iter().filter(|e| e["type"] == ty).collect()
    }

    pub fn reply(&mut self, id: u64) -> Option<Result<Value, ClientError>> {
        self.replies.remove(&id)
    }

    /// The app's view of a conversation, in order.
    pub fn view(&self, conv: &str) -> Vec<Value> {
        let mut v: Vec<Value> = self
            .views
            .get(conv)
            .map(|m| m.values().cloned().collect())
            .unwrap_or_default();
        v.sort_by_key(|m| m["order"].as_i64());
        v
    }

    pub fn sent_ops(&self, op: &str) -> usize {
        self.server
            .requests
            .iter()
            .filter(|r| r["op"] == op)
            .count()
    }

    /// Converged with the server: the same last seq per conversation, every
    /// message the client holds equal to the server's, nothing waiting.
    pub fn assert_converged(&mut self) {
        assert!(self.early_acks.is_empty(), "{:?}", self.early_acks);
        let d = self.core.debug_state();
        for c in d["convs"].as_array().unwrap() {
            let id = c["conv_id"].as_str().unwrap();
            assert_eq!(
                c["last_seq"].as_i64().unwrap(),
                self.server.last(id),
                "last seq of {id}: {d}"
            );
            assert_eq!(c["ahead"], 0, "{id}");
        }
        for (k, m) in self.views.clone() {
            for v in m.values() {
                if v["status"] == "sent" {
                    let s = &self.server.cur[v["msg_id"].as_str().unwrap()];
                    assert_eq!(v["content"], s["content"], "{k}: {v} vs {s}");
                    assert_eq!(v["version"], s["version"]);
                }
            }
        }
        // Every applied send once.
        let mut seen = HashSet::new();
        for cm in &self.server.applied_cms {
            assert!(seen.insert(cm.clone()), "{cm} applied twice");
        }
    }
}
