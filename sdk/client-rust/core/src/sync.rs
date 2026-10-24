//! Sync (docs/08, docs/12 §2.6): per conversation, events are applied in
//! seq order and once; what comes after a gap waits; nothing is
//! acknowledged before it is stored; a gap nothing fills, or a head the
//! server is ahead of, replaces the connection (the new one syncs from the
//! acknowledged position).

use serde_json::{Value, json};

use crate::*;

impl Core {
    pub(crate) fn request_sync(&mut self) {
        if self.phase != Phase::Handshake && self.phase != Phase::Live {
            return;
        }
        if self.syncing {
            self.sync_again = true;
            return;
        }
        self.syncing = true;
        self.request("sync", json!({"limit": 200}), Purpose::Sync);
    }

    pub(crate) fn on_sync_response(&mut self, f: &Value, err: Option<ClientError>) {
        self.syncing = false;
        if let Some(e) = err {
            self.note_failure("sync", e.to_json());
            if e.code == "rate_limited" {
                self.spend_budget();
            }
            return self.drop_connection("sync_failed");
        }
        let mut acks = vec![];
        for entry in f["convs"].as_array().cloned().unwrap_or_default() {
            let id = entry["conv_id"].as_str().unwrap_or("").to_string();
            if id.is_empty() {
                continue;
            }
            if let Some(from) = entry["reset"]["from_seq"].as_i64() {
                // The device was too far behind (or the log was cut): it
                // goes on from here; older messages come back page by page.
                let c = self.conv_mut(&id);
                c.last_seq = from - 1;
                c.based = true;
                c.ahead.retain(|s, _| *s >= from);
                c.segments.push((from, from - 1));
                c.dirty = true;
                self.emit(json!({"type": "reset", "conv_id": id, "from_seq": from}));
            }
            let events: Vec<Value> = entry["events"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .map(|e| e["event"].clone())
                .collect();
            {
                let c = self.conv_mut(&id);
                if !c.based
                    && let Some(first) = events.first().and_then(|e| e["seq"].as_i64())
                {
                    // New to this cache: the server decides where it starts.
                    c.last_seq = first - 1;
                    c.based = true;
                    c.segments.push((first, first - 1));
                    c.dirty = true;
                    self.list_changed.insert(id.clone());
                }
            }
            for e in events {
                self.place(&id, e);
            }
            self.drain(&id);
            for s in entry["side_events"].as_array().cloned().unwrap_or_default() {
                self.emit(json!({"type": "side", "event": s}));
            }
            for flag in ["left", "dissolved"] {
                if entry[flag] == true {
                    let c = self.conv_mut(&id);
                    c.info[flag] = json!(true);
                    c.dirty = true;
                    self.list_changed.insert(id.clone());
                }
            }
            let c = self.conv_mut(&id);
            if let Some(upto) = entry["upto"].as_i64()
                && c.based
            {
                let mut a = json!({"conv_id": id, "seq": upto.min(c.last_seq)});
                if let Some(s) = entry["side_upto"].as_i64() {
                    a["side_seq"] = json!(s);
                }
                acks.push(a);
            }
        }
        // Stored first, then acknowledged (a crash in between only means
        // the server sends it again).
        self.persist_now();
        self.ack(acks);
        if f["more"] == true || self.sync_again {
            self.sync_again = false;
            return self.request_sync();
        }
        if self.phase == Phase::Handshake {
            self.go_live();
        }
        // A gap after a push, or heads ahead of us, that this sync did not
        // close: the server never resends on the same connection.
        let gap = self.gap_check && self.convs.values().any(|c| !c.ahead.is_empty());
        let behind = self
            .heads_expect
            .iter()
            .any(|(id, h)| self.convs.get(id).is_some_and(|c| c.last_seq < *h));
        self.gap_check = false;
        self.heads_expect.clear();
        if gap || behind {
            self.drop_connection(if gap { "gap" } else { "behind" });
        }
    }

    pub(crate) fn on_push(&mut self, ev: &Value) {
        if self.phase != Phase::Live && self.phase != Phase::Handshake {
            return;
        }
        let id = ev["conv_id"].as_str().unwrap_or("").to_string();
        let Some(seq) = ev["seq"].as_i64() else {
            return;
        };
        if !self.convs.get(&id).is_some_and(|c| c.based) {
            // A conversation this device has no position in yet.
            return self.request_sync();
        }
        if self.convs[&id].last_seq >= seq {
            return;
        }
        self.place(&id, ev.clone());
        self.drain(&id);
        self.persist_now();
        let c = &self.convs[&id];
        let (last, gap) = (c.last_seq, !c.ahead.is_empty());
        self.ack(vec![json!({"conv_id": id, "seq": last})]);
        if gap {
            self.gap_check = true;
            self.request_sync();
        }
    }

    fn place(&mut self, id: &str, ev: Value) {
        let Some(seq) = ev["seq"].as_i64() else {
            return;
        };
        let c = self.conv_mut(id);
        if seq <= c.last_seq {
            return;
        }
        if seq == c.last_seq + 1 {
            self.apply(id, ev);
        } else {
            c.ahead.insert(seq, ev);
        }
    }

    fn drain(&mut self, id: &str) {
        loop {
            let c = self.conv_mut(id);
            let next = c.last_seq + 1;
            let Some(ev) = c.ahead.remove(&next) else {
                break;
            };
            self.apply(id, ev);
        }
        let c = self.conv_mut(id);
        let last = c.last_seq;
        c.ahead.retain(|s, _| *s > last);
    }

    /// One event, in order.
    fn apply(&mut self, id: &str, ev: Value) {
        let seq = ev["seq"].as_i64().unwrap_or(0);
        {
            let c = self.conv_mut(id);
            c.last_seq = seq;
            c.dirty = true;
            match c.segments.last_mut() {
                Some(s) if s.1 + 1 >= seq => s.1 = s.1.max(seq),
                _ => c.segments.push((seq, seq)),
            }
            c.info["last_seq"] = json!(seq);
        }
        let mine = self.me["id"].as_str().unwrap_or("").to_string();
        if let Some(m) = ev.get("msg").filter(|m| m.is_object()) {
            let mut m = m.clone();
            if let Some(o) = m.as_object_mut() {
                o.remove("client_msg_id_for");
            }
            let created = m["created_seq"].as_i64().unwrap_or(seq);
            let from_me = m["sender_id"] == mine.as_str();
            let new = ev["type"] == "msg.created";
            self.put_msg(id, m.clone(), true);
            self.settle_send(id, &m);
            let c = self.conv_mut(id);
            if created >= c.info["last_message_seq"].as_i64().unwrap_or(0) {
                c.info["last_message"] = m;
                c.info["last_message_seq"] = json!(created);
            }
            if new && !from_me {
                let u = c.info["unread"].as_i64().unwrap_or(0);
                c.info["unread"] = json!(u + 1);
            }
            self.list_changed.insert(id.to_string());
        }
        match ev["type"].as_str().unwrap_or("") {
            // A deleted account: what that person wrote is gone here too.
            "user.erased" => {
                if let Some(u) = ev["data"]["user_id"].as_str() {
                    let gone: Vec<Value> = self
                        .convs
                        .get(id)
                        .map(|c| {
                            c.msgs
                                .values()
                                .filter(|m| {
                                    m["sender_id"] == u && m["created_seq"].as_i64() < Some(seq)
                                })
                                .cloned()
                                .collect()
                        })
                        .unwrap_or_default();
                    for mut m in gone {
                        m["content"] = json!("");
                        m["payload"] = Value::Null;
                        m["state"] = json!("recalled");
                        m["state_reason"] = json!("account_deleted");
                        m["version"] = json!(m["version"].as_i64().unwrap_or(0) + 1);
                        self.put_msg(id, m, true);
                    }
                }
            }
            "conv.created" => {
                let c = self.conv_mut(id);
                for k in ["kind", "title"] {
                    if !ev["data"][k].is_null() {
                        c.info[k] = ev["data"][k].clone();
                    }
                }
                self.list_changed.insert(id.to_string());
            }
            "conv.updated" => {
                let c = self.conv_mut(id);
                if let Some(t) = ev["data"]["title"].as_str() {
                    c.info["title"] = json!(t);
                }
                self.list_changed.insert(id.to_string());
            }
            _ => {}
        }
        self.emit(json!({"type": "event", "event": ev}));
    }

    pub(crate) fn on_side(&mut self, ev: &Value) {
        self.emit(json!({"type": "side", "event": ev}));
        if let (Some(c), Some(s)) = (ev["conv_id"].as_str(), ev["side_seq"].as_i64()) {
            self.ack(vec![json!({"conv_id": c, "side_seq": s})]);
        }
    }

    /// Compares positions with the server now and then: a lost last push
    /// (nothing after it shows the gap) is found here.
    pub(crate) fn heads_check(&mut self) {
        let ids: Vec<String> = self
            .convs
            .values()
            .filter(|c| c.based && c.info["left"] != true && c.info["dissolved"] != true)
            .map(|c| c.id.clone())
            .take(200)
            .collect();
        if ids.is_empty() {
            return;
        }
        self.request(
            "heads",
            json!({"conv_ids": ids}),
            Purpose::Heads(ids.clone()),
        );
    }

    pub(crate) fn on_heads(&mut self, f: &Value, ids: &[String]) {
        let behind: Vec<(String, i64)> = ids
            .iter()
            .filter_map(|id| {
                let h = f["heads"][id].as_i64()?;
                (h > self.convs.get(id)?.last_seq).then(|| (id.clone(), h))
            })
            .collect();
        if !behind.is_empty() {
            self.heads_expect = behind;
            self.request_sync();
        }
    }

    pub(crate) fn on_list_convs(&mut self, f: &Value) {
        for c in f["convs"].as_array().cloned().unwrap_or_default() {
            let Some(id) = c["conv_id"].as_str().map(String::from) else {
                continue;
            };
            let conv = self.conv_mut(&id);
            for k in ["kind", "title", "unread", "members"] {
                if !c[k].is_null() {
                    conv.info[k] = c[k].clone();
                }
            }
            conv.info["server_last_seq"] = c["last_seq"].clone();
            conv.dirty = true;
            self.list_changed.insert(id);
        }
    }

    /// Read or mute changed on another of the user's devices.
    pub(crate) fn on_own_change(&mut self, f: &Value) {
        let Some(id) = f["conv_id"].as_str().map(String::from) else {
            return;
        };
        let c = self.conv_mut(&id);
        if f["type"] == "read" {
            c.info["unread"] = json!(0);
            c.info["read_seq"] = f["seq"].clone();
        } else {
            c.info["mute"] = json!({"mode": f["mode"], "until": f["until"]});
        }
        c.dirty = true;
        self.list_changed.insert(id);
        self.emit(f.clone());
    }

    /// A streamed reply grows (only for the focused conversation).
    pub(crate) fn on_delta(&mut self, f: &Value) {
        let (Some(id), Some(msg)) = (f["conv_id"].as_str(), f["msg_id"].as_str()) else {
            return;
        };
        let Some(c) = self.convs.get_mut(id) else {
            return;
        };
        let Some(seq) = c.seq_of.get(msg).copied() else {
            return;
        };
        let Some(m) = c.msgs.get_mut(&seq) else {
            return;
        };
        if m["state"] != "streaming" {
            return;
        }
        let text = f["text"].as_str().unwrap_or("");
        let cur = if f["reset"] == true {
            String::new()
        } else {
            m["streamed"].as_str().unwrap_or("").to_string()
        };
        m["streamed"] = json!(cur + text);
        let (id, msg) = (id.to_string(), msg.to_string());
        self.mark_changed(&id, &msg);
    }
}
