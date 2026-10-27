//! The connection (docs/12 §2.3–§2.5): states, token, handshake,
//! heartbeat, failure classes, backoff with full jitter, the retry budget,
//! network and foreground signals.

use serde_json::{Value, json};

use crate::*;

/// The server takes device and client labels of 1..=64 bytes (anything
/// longer refuses the connection): a longer one is cut at a character
/// boundary, an empty one is left out.
pub(crate) fn label(s: &str) -> Option<&str> {
    let mut end = s.len().min(64);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    Some(&s[..end]).filter(|v| !v.is_empty())
}

fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

impl Core {
    pub(crate) fn set_state(&mut self, s: State, reason: Option<&str>) {
        let reason = reason.map(String::from);
        if self.state == s && self.reason == reason {
            return;
        }
        self.state = s;
        self.reason = reason;
        self.emit(json!({
            "type": "connection", "state": s.as_str(), "reason": self.reason,
            "retry_at": self.retry_at, "attempt": self.attempt,
        }));
    }

    pub(crate) fn connect(&mut self) {
        self.want = true;
        if self.phase == Phase::Down && self.retry_at.is_none() {
            self.attempt = 0;
            self.start_attempt();
        }
    }

    pub(crate) fn close(&mut self) {
        self.want = false;
        self.retry_at = None;
        if self.phase != Phase::Down && self.phase != Phase::Token {
            self.out.push(Effect::Close {
                code: 1000,
                reason: "closed".into(),
            });
        }
        self.lost("closed");
        self.set_state(State::Closed, None);
    }

    /// Starts one connection attempt now.
    pub(crate) fn start_attempt(&mut self) {
        if !self.want || !self.restored {
            return;
        }
        self.retry_at = None;
        if !self.online {
            self.down_since.get_or_insert(self.now);
            self.set_state(State::Suspended, Some("offline"));
            return;
        }
        self.down_since.get_or_insert(self.now);
        if self.state != State::Suspended {
            self.set_state(State::Connecting, None);
        }
        match self.token.clone() {
            Some(_) => self.open(),
            None => {
                self.phase = Phase::Token;
                self.out.push(Effect::NeedToken);
            }
        }
    }

    pub(crate) fn on_token(&mut self, t: Result<String, bool>) {
        if self.phase != Phase::Token {
            return;
        }
        match t {
            Ok(t) => {
                self.token = Some(t);
                self.token_fresh = true;
                self.open();
            }
            // The backend refuses this user: nothing to retry.
            Err(true) => {
                self.phase = Phase::Down;
                self.fail("unauthenticated");
            }
            // The backend could not be reached: like the server.
            Err(false) => {
                self.phase = Phase::Down;
                self.note_failure("token", json!("the token provider failed"));
                self.schedule_retry(false, None, "token_unavailable");
            }
        }
    }

    fn open(&mut self) {
        let base = self.cfg.url.trim_end_matches('/');
        let base = if let Some(r) = base.strip_prefix("https://") {
            format!("wss://{r}")
        } else if let Some(r) = base.strip_prefix("http://") {
            format!("ws://{r}")
        } else {
            base.to_string()
        };
        let mut url = format!("{base}/ws?device={}", enc(&self.cfg.device_id));
        if let Some(n) = self.cfg.device_name.as_deref().and_then(label) {
            url.push_str(&format!("&device_name={}", enc(n)));
        }
        if let Some(p) = self.cfg.platform.as_deref().and_then(label) {
            url.push_str(&format!("&platform={}", enc(p)));
        }
        self.phase = Phase::Opening;
        self.opening_deadline = Some(self.now + CONNECT_TIMEOUT_MS);
        self.goaway_wait = None;
        let token = self.token.clone().unwrap_or_default();
        self.out.push(Effect::Connect { url, token });
    }

    pub(crate) fn on_connect_failed(
        &mut self,
        status: Option<u16>,
        retry_after: Option<i64>,
        detail: &str,
    ) {
        if self.phase != Phase::Opening && self.phase != Phase::Handshake {
            return;
        }
        self.phase = Phase::Down;
        self.opening_deadline = None;
        self.note_failure("connect", json!({"status": status, "detail": detail}));
        match status {
            // The token is not (or no longer) good: one new token at once,
            // then the usual backoff.
            Some(401) => {
                let was_fresh = self.token_fresh;
                self.token = None;
                self.token_fresh = false;
                if was_fresh {
                    self.schedule_retry(false, None, "unauthorized");
                } else {
                    self.start_attempt();
                }
            }
            // Deactivated or deleted: do not retry.
            Some(403) => self.fail("unauthenticated"),
            Some(400) => self.fail("protocol"),
            Some(429) | Some(503) => {
                self.spend_budget();
                self.schedule_retry(true, retry_after, "server_busy");
            }
            Some(_) => self.schedule_retry(false, retry_after, "server_error"),
            // No status (browsers never show the handshake's): the token
            // may be what failed. A cached one is replaced once, at once.
            None if self.token.is_some() && !self.token_fresh => {
                self.token = None;
                self.start_attempt();
            }
            None => self.schedule_retry(false, None, "server_unreachable"),
        }
    }

    pub(crate) fn fail(&mut self, reason: &str) {
        self.retry_at = None;
        self.phase = Phase::Down;
        self.set_state(State::Failed, Some(reason));
    }

    /// Plans the next attempt: full jitter `random(0, min(cap, base·2^n))`,
    /// at least what the server asked; after 2 minutes down, once a minute.
    pub(crate) fn schedule_retry(&mut self, busy: bool, at_least: Option<i64>, reason: &str) {
        if !self.want {
            return;
        }
        if !self.online {
            self.set_state(State::Suspended, Some("offline"));
            return;
        }
        let since = *self.down_since.get_or_insert(self.now);
        self.attempt += 1;
        let base = if busy {
            BACKOFF_BASE_BUSY_MS
        } else {
            BACKOFF_BASE_MS
        };
        let exp = (base.saturating_mul(1i64 << (self.attempt - 1).min(16))).min(BACKOFF_CAP_MS);
        let mut wait = self.rng.upto(exp);
        let suspended = self.now - since >= SUSPEND_AFTER_MS;
        if suspended {
            wait = SUSPENDED_RETRY_MS * 4 / 5 + self.rng.upto(SUSPENDED_RETRY_MS * 2 / 5);
        }
        // At least 1 ms: the next input comes from the clock.
        wait = wait.max(at_least.unwrap_or(0)).max(1);
        self.retry_at = Some(self.now + wait);
        let s = if suspended {
            State::Suspended
        } else {
            State::Disconnected
        };
        // Re-announce: the retry time changed.
        self.state = State::Closed;
        self.set_state(s, Some(reason));
    }

    /// The connection is gone: what was in flight is answered or requeued.
    pub(crate) fn lost(&mut self, why: &str) {
        self.phase = Phase::Down;
        self.opening_deadline = None;
        self.probe_deadline = None;
        self.syncing = false;
        self.sync_again = false;
        self.gap_check = false;
        // Acknowledgements belong to the connection that delivered; the
        // next one resends from the acknowledged position.
        self.acks_waiting.clear();
        let pending: Vec<(u64, Pending)> = self.pending.drain().collect();
        for (_, p) in pending {
            match p.purpose {
                Purpose::App(id) => self.reply(
                    id,
                    Err(ClientError::new(
                        "disconnected",
                        format!("connection lost ({why}); the result is unknown"),
                    )),
                ),
                Purpose::Op(op) => self.op_unknown(&op),
                Purpose::History { conv_id, cmd, .. } => {
                    if let Some(c) = self.convs.get_mut(&conv_id) {
                        c.loading_older = false;
                    }
                    if let Some(id) = cmd {
                        self.reply(id, Err(ClientError::new("disconnected", "connection lost")));
                    }
                }
                _ => {}
            }
        }
    }

    /// Drops the current connection and plans the next attempt.
    pub(crate) fn drop_connection(&mut self, reason: &str) {
        let was_live = self.phase == Phase::Live;
        if self.phase != Phase::Down && self.phase != Phase::Token {
            self.out.push(Effect::Close {
                code: 4000,
                reason: reason.into(),
            });
        }
        self.note_failure("dropped", json!(reason));
        self.lost(reason);
        if was_live {
            // A connection that worked: one immediate retry first.
            self.down_since = Some(self.now);
            self.attempt = 0;
            self.set_state(State::Disconnected, Some(reason));
            self.start_attempt();
        } else {
            self.schedule_retry(false, None, reason);
        }
    }

    pub(crate) fn on_closed(&mut self, code: u16, reason: &str) {
        if self.phase == Phase::Down || self.phase == Phase::Token {
            return;
        }
        let was_live = self.phase == Phase::Live;
        self.note_failure("closed", json!({"code": code, "reason": reason}));
        self.lost(reason);
        match reason {
            "logged_out" => return self.fail("logged_out"),
            "client_too_old" => return self.fail("upgrade_required"),
            _ => {}
        }
        if code == 1012 || reason == "server_restart" {
            // Another node takes over: come back after the wait the server
            // gave (or a random one), without growing the backoff.
            let wait = self
                .goaway_wait
                .take()
                .unwrap_or_else(|| self.rng.upto(5_000));
            self.down_since.get_or_insert(self.now);
            self.retry_at = Some(self.now + wait);
            self.state = State::Closed;
            self.set_state(State::Disconnected, Some("server_restart"));
            if wait == 0 {
                self.start_attempt();
            }
        } else if was_live {
            self.down_since = Some(self.now);
            self.attempt = 0;
            self.set_state(State::Disconnected, Some("connection_lost"));
            self.start_attempt();
        } else {
            self.schedule_retry(false, None, "connection_lost");
        }
    }

    pub(crate) fn on_network(&mut self, online: bool, generation: u64) {
        let changed = generation != self.generation;
        self.generation = generation;
        if !online {
            self.online = false;
            if self.phase != Phase::Down && self.phase != Phase::Token {
                self.out.push(Effect::Close {
                    code: 4000,
                    reason: "offline".into(),
                });
            }
            self.lost("offline");
            self.retry_at = None;
            if self.want {
                self.down_since.get_or_insert(self.now);
                self.set_state(State::Suspended, Some("offline"));
            }
            return;
        }
        let was_offline = !self.online;
        self.online = true;
        if !self.want {
            return;
        }
        if self.phase == Phase::Down {
            if was_offline || changed {
                // Back online or a new route: try now, backoff from zero.
                self.attempt = 0;
                self.start_attempt();
            }
        } else if changed && self.phase == Phase::Live {
            // The route may have changed under the connection: check it.
            self.probe(3_000);
        }
    }

    pub(crate) fn on_foreground(&mut self, f: bool) {
        self.foreground = f;
        if !f || !self.want {
            return;
        }
        match self.phase {
            Phase::Live => {
                self.probe(5_000);
                self.request_sync();
            }
            Phase::Down if self.state != State::Failed => {
                self.attempt = 0;
                self.start_attempt();
            }
            _ => {}
        }
    }

    /// A ping that must come back in `ms`, or the connection is dropped.
    fn probe(&mut self, ms: i64) {
        self.request("ping", json!({}), Purpose::Ping);
        self.probe_deadline = Some(self.now + ms);
    }

    /// Request timeout: `max(10 s, srtt + 4·rttvar)`, at most 30 s.
    fn timeout(&self) -> i64 {
        let t = self.srtt.map(|s| s + 4.0 * self.rttvar).unwrap_or(0.0) as i64;
        t.clamp(10_000, 30_000)
    }

    pub(crate) fn request(&mut self, op: &str, args: Value, purpose: Purpose) -> u64 {
        self.rid += 1;
        let rid = self.rid;
        let mut v = if args.is_object() { args } else { json!({}) };
        v["op"] = json!(op);
        v["rid"] = json!(rid);
        self.out.push(Effect::Send(v.to_string()));
        self.last_tx = self.now;
        let deadline = self.now + self.timeout();
        self.pending.insert(
            rid,
            Pending {
                purpose,
                deadline,
                sent_at: self.now,
            },
        );
        rid
    }

    pub(crate) fn spend_budget(&mut self) {
        self.budget -= 1.0;
        if self.budget < BUDGET_MAX / 2.0 {
            // Too many retries for server trouble: the queue waits.
            self.paused_until = Some(self.now + 60_000);
        }
    }

    pub(crate) fn earn_budget(&mut self) {
        self.budget = (self.budget + 0.1).min(BUDGET_MAX);
    }

    pub(crate) fn on_frame(&mut self, text: &str) {
        self.last_rx = self.now;
        let Ok(f) = serde_json::from_str::<Value>(text) else {
            self.note_failure("frame", json!("unparsable frame"));
            return self.drop_connection("protocol");
        };
        match f["type"].as_str().unwrap_or("") {
            "welcome" => self.on_welcome(&f),
            "res" => self.on_response(&f),
            "event" => self.on_push(&f["event"]),
            "side" => self.on_side(&f["event"]),
            "delta" => self.on_delta(&f),
            "signal" => {
                let mut v = f.clone();
                v["type"] = json!("signal");
                self.emit(v);
            }
            "resync" => self.request_sync(),
            "read" | "mute" => self.on_own_change(&f),
            "goaway" => self.goaway_wait = f["retry_after_ms"].as_i64(),
            // Something a newer server sends.
            _ => {}
        }
    }

    fn on_welcome(&mut self, f: &Value) {
        if self.phase != Phase::Opening {
            return;
        }
        self.opening_deadline = None;
        self.phase = Phase::Handshake;
        self.me = f["me"].clone();
        if let Some(hb) = f["heartbeat_ms"].as_i64().filter(|h| *h > 0) {
            self.heartbeat_ms = hb;
        }
        let account = self.me["id"].as_str().map(String::from);
        if self.account.is_some() && self.account != account {
            // Another account's cache: never shown to this one.
            self.wipe();
            self.emit(json!({"type": "storage", "state": self.storage, "reset": "account"}));
        }
        if self.account != account {
            self.account = account.clone();
            self.batch.meta = Some(json!({"account": account}));
        }
        self.request(
            "hello",
            json!({"client": {
                "name": label(&self.cfg.client_name).unwrap_or(""),
                "version": label(&self.cfg.client_version).unwrap_or(""),
            }}),
            Purpose::Hello,
        );
    }

    fn on_response(&mut self, f: &Value) {
        let Some(rid) = f["rid"].as_u64() else { return };
        let Some(p) = self.pending.remove(&rid) else {
            return;
        };
        // Round-trip time, smoothed (RFC 6298).
        let rtt = (self.now - p.sent_at) as f64;
        match self.srtt {
            None => {
                self.srtt = Some(rtt);
                self.rttvar = rtt / 2.0;
            }
            Some(s) => {
                self.rttvar = 0.75 * self.rttvar + 0.25 * (s - rtt).abs();
                self.srtt = Some(0.875 * s + 0.125 * rtt);
            }
        }
        let ok = f["ok"] == true;
        let err = (!ok).then(|| ClientError::from_frame(&f["error"]));
        match p.purpose {
            Purpose::Hello => match err {
                None => {
                    // The full profile (permissions included).
                    if f["me"]["id"] == self.me["id"] {
                        self.me = f["me"].clone();
                    }
                    self.request("list_convs", json!({}), Purpose::ListConvs);
                }
                Some(e) if e.code == "client_too_old" => self.fail("upgrade_required"),
                Some(_) => self.drop_connection("hello_failed"),
            },
            Purpose::ListConvs => {
                if ok {
                    self.on_list_convs(f);
                }
                self.request_sync();
            }
            Purpose::Sync => self.on_sync_response(f, err),
            Purpose::Ack => {}
            Purpose::Ping => self.probe_deadline = None,
            Purpose::Focus => {}
            Purpose::Heads(ids) => self.on_heads(f, &ids),
            Purpose::Op(op) => self.on_op_result(&op, f, err),
            Purpose::History {
                conv_id,
                anchor,
                jump,
                cmd,
            } => self.on_history(&conv_id, anchor, jump, cmd, f, err),
            Purpose::App(id) => {
                let r = match err {
                    None => {
                        let mut v = f.clone();
                        if let Some(o) = v.as_object_mut() {
                            o.remove("type");
                            o.remove("rid");
                            o.remove("ok");
                        }
                        Ok(v)
                    }
                    Some(e) => Err(e),
                };
                self.reply(id, r);
            }
        }
    }

    /// The first sync of a connection is done: live.
    pub(crate) fn go_live(&mut self) {
        self.phase = Phase::Live;
        self.attempt = 0;
        self.down_since = None;
        self.retry_at = None;
        self.token_fresh = false;
        self.paused_until = None;
        self.earn_budget();
        self.next_heads = self.now + HEADS_EVERY_MS;
        self.last_tx = self.now;
        if let Some(c) = self.focused.clone() {
            self.request("focus", json!({"conv_id": c}), Purpose::Focus);
        }
        self.set_state(State::Connected, None);
    }

    /// Deadlines: retries, the opening, requests, heartbeat, probes.
    pub(crate) fn timers(&mut self) {
        let now = self.now;
        if self.retry_at.is_some_and(|t| t <= now) && self.phase == Phase::Down {
            self.retry_at = None;
            self.start_attempt();
        }
        if self.opening_deadline.is_some_and(|t| t <= now) {
            self.opening_deadline = None;
            self.out.push(Effect::Close {
                code: 4000,
                reason: "connect_timeout".into(),
            });
            self.on_connect_failed(None, None, "connect timeout");
        }
        let late: Vec<u64> = self
            .pending
            .iter()
            .filter(|(_, p)| p.deadline <= now)
            .map(|(r, _)| *r)
            .collect();
        for rid in late {
            let Some(p) = self.pending.remove(&rid) else {
                continue;
            };
            match p.purpose {
                Purpose::App(id) => self.reply(
                    id,
                    Err(ClientError::new(
                        "timeout",
                        "no answer in time; the result is unknown",
                    )),
                ),
                Purpose::Op(op) => {
                    self.op_unknown(&op);
                    self.drop_connection("request_timeout");
                    return;
                }
                Purpose::History { conv_id, cmd, .. } => {
                    if let Some(c) = self.convs.get_mut(&conv_id) {
                        c.loading_older = false;
                    }
                    if let Some(id) = cmd {
                        self.reply(id, Err(ClientError::new("timeout", "no answer in time")));
                    }
                }
                Purpose::Ack | Purpose::Focus => {}
                _ => {
                    self.drop_connection("request_timeout");
                    return;
                }
            }
        }
        if self.probe_deadline.is_some_and(|t| t <= now) {
            self.probe_deadline = None;
            return self.drop_connection("probe_timeout");
        }
        if self.phase == Phase::Live {
            if now - self.last_rx > 2 * self.heartbeat_ms + 5_000 {
                return self.drop_connection("heartbeat_timeout");
            }
            if now - self.last_tx >= self.heartbeat_ms {
                self.request("ping", json!({}), Purpose::Ping);
            }
            if now >= self.next_heads {
                self.next_heads = now + HEADS_EVERY_MS;
                self.heads_check();
            }
        }
        if self.paused_until.is_some_and(|t| t <= now) {
            self.paused_until = None;
            self.budget = self.budget.max(BUDGET_MAX / 2.0);
        }
    }

    pub(crate) fn on_restored(&mut self, s: Snapshot) {
        self.restored = true;
        self.account = s.account;
        for row in s.convs {
            let c = self.conv_mut(&row.conv_id);
            c.last_seq = row.last_seq;
            c.based = true;
            c.segments = row.segments;
            c.older_end = row.older_end;
            c.info = row.info;
        }
        for m in s.messages {
            let conv = m["conv_id"].as_str().unwrap_or("").to_string();
            self.put_msg(&conv, m, false);
        }
        for o in s.ops {
            if let Some(op) = Op::from_json(&o) {
                self.ops.push(op);
            }
        }
        self.ops.sort_by_key(|o| o.n);
        self.op_n = self.ops.iter().map(|o| o.n).max().unwrap_or(0);
        // What was in flight when the app stopped is retried (same ids).
        for o in &mut self.ops {
            o.in_flight = None;
        }
        self.emit(json!({"type": "storage", "state": self.storage}));
        if self.want {
            self.start_attempt();
        }
    }

    pub(crate) fn on_storage_failed(&mut self, kind: &str) {
        self.note_failure("storage", json!(kind));
        match kind {
            // Nothing new is acknowledged until writes work again (the
            // server sends it again), the queue still writes.
            "full" => {
                self.storage = "full";
                self.acks_waiting.clear();
                self.unconfirmed = None;
            }
            // Rebuilt from the server; the queue is written again.
            "corrupt" => {
                self.wipe_cache_keep_queue();
                self.emit(json!({"type": "storage", "state": self.storage, "reset": "corrupt"}));
                return;
            }
            _ => self.storage = "memory_only",
        }
        self.emit(json!({"type": "storage", "state": self.storage}));
    }

    pub(crate) fn on_storage_recovered(&mut self) {
        if self.storage == "ok" {
            return;
        }
        self.storage = "ok";
        let ids: Vec<String> = self.convs.keys().cloned().collect();
        for id in ids {
            self.store_conv_all(&id);
        }
        for o in self.ops.clone() {
            self.batch.put_ops.push(o.to_json());
        }
        self.emit(json!({"type": "storage", "state": "ok"}));
        if self.phase == Phase::Live {
            // What was applied while nothing could be stored is
            // acknowledged once it is (this connection delivered it).
            self.persist_now();
            let acks: Vec<Value> = self
                .convs
                .values()
                .filter(|c| c.based && c.last_seq > 0)
                .map(|c| json!({"conv_id": c.id, "seq": c.last_seq}))
                .collect();
            self.ack(acks);
        }
    }

    /// Logs out or another account: nothing of the cache stays.
    pub(crate) fn wipe(&mut self) {
        self.out.push(Effect::WipeStore);
        self.batch = Batch::default();
        let watched: Vec<String> = self.watched.iter().cloned().collect();
        self.convs.clear();
        self.ops.clear();
        self.sent_ids.clear();
        for w in watched {
            self.emit(json!({"type": "messages", "conv_id": w, "reset": true, "changes": []}));
        }
        self.list_changed.clear();
        self.emit(json!({"type": "conversations", "reset": true, "changes": []}));
    }

    fn wipe_cache_keep_queue(&mut self) {
        self.out.push(Effect::WipeStore);
        self.batch = Batch::default();
        for c in self.convs.values_mut() {
            // Sync goes on from the server's position; older messages come
            // back page by page.
            c.msgs.clear();
            c.seq_of.clear();
            c.segments = vec![(c.last_seq + 1, c.last_seq)];
            c.older_end = None;
            c.dirty = true;
        }
        self.batch.meta = Some(json!({"account": self.account}));
        for o in self.ops.clone() {
            self.batch.put_ops.push(o.to_json());
        }
        let watched: Vec<String> = self.watched.iter().cloned().collect();
        for w in watched {
            self.emit(json!({"type": "messages", "conv_id": w, "reset": true, "changes": self.snapshot_changes(&w)}));
        }
    }

    /// Writes what is pending now (before acknowledging it).
    pub(crate) fn persist_now(&mut self) {
        for c in self.convs.values_mut() {
            if c.dirty {
                c.dirty = false;
                self.batch.convs.push(ConvRow {
                    conv_id: c.id.clone(),
                    last_seq: c.last_seq,
                    segments: c.segments.clone(),
                    older_end: c.older_end.clone(),
                    info: c.info.clone(),
                });
            }
        }
        if self.batch.is_empty() {
            return;
        }
        let mut b = std::mem::take(&mut self.batch);
        if self.storage != "memory_only" {
            self.next_batch += 1;
            b.id = self.next_batch;
            self.unconfirmed = Some(b.id);
            self.out.push(Effect::Persist(b));
        }
    }

    /// Acknowledges once what was applied is stored: at once when nothing
    /// is waiting to be written, else when the store confirms the write.
    pub(crate) fn ack(&mut self, acks: Vec<Value>) {
        if acks.is_empty() || self.storage == "full" {
            return;
        }
        match self.unconfirmed {
            None => {
                self.request("ack", json!({"acks": acks}), Purpose::Ack);
            }
            Some(id) => self.acks_waiting.push((id, acks)),
        }
    }

    pub(crate) fn on_persisted(&mut self, id: u64) {
        if self.unconfirmed.is_some_and(|u| u <= id) {
            self.unconfirmed = None;
        }
        let ready: Vec<Vec<Value>> = {
            let (r, keep): (Vec<_>, Vec<_>) = std::mem::take(&mut self.acks_waiting)
                .into_iter()
                .partition(|(b, _)| *b <= id);
            self.acks_waiting = keep;
            r.into_iter().map(|(_, a)| a).collect()
        };
        if self.phase != Phase::Handshake && self.phase != Phase::Live {
            return;
        }
        // One ack for all, the furthest per conversation.
        let mut best: std::collections::BTreeMap<String, Value> = Default::default();
        for a in ready.into_iter().flatten() {
            let c = a["conv_id"].as_str().unwrap_or("").to_string();
            let e = best.entry(c).or_insert_with(|| json!({}));
            for k in ["conv_id", "seq", "side_seq"] {
                if !a[k].is_null()
                    && (e[k].is_null() || a[k].as_i64() > e[k].as_i64() || k == "conv_id")
                {
                    e[k] = a[k].clone();
                }
            }
        }
        if !best.is_empty() {
            let acks: Vec<Value> = best.into_values().collect();
            self.request("ack", json!({"acks": acks}), Purpose::Ack);
        }
    }

    /// End of every input: views, then the cache.
    pub(crate) fn flush(&mut self) {
        self.emit_changes();
        self.persist_now();
    }
}
