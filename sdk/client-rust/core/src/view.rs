//! What the app shows (docs/12 §2.7, §2.11): the server's copy with the
//! offline queue laid over it (Firestore's overlay), as changes
//! (`upsert` / `remove` by key) after a first full snapshot.

use serde_json::{Value, json};

use crate::queue::Kind;
use crate::*;

/// Local messages sort after every stored one, in queue order.
const LOCAL_ORDER: i64 = 1 << 52;

impl Core {
    pub(crate) fn conv_mut(&mut self, id: &str) -> &mut Conv {
        self.convs.entry(id.to_string()).or_insert_with(|| Conv {
            id: id.to_string(),
            info: json!({}),
            ..Default::default()
        })
    }

    pub(crate) fn mark_changed(&mut self, conv: &str, key: &str) {
        if self.watched.contains(conv) {
            self.changed
                .entry(conv.to_string())
                .or_default()
                .insert(key.to_string());
        }
    }

    /// Stores the server's version of a message (an older version never
    /// replaces a newer one).
    pub(crate) fn put_msg(&mut self, conv: &str, m: Value, persist: bool) {
        let (Some(seq), Some(id)) = (
            m["created_seq"].as_i64(),
            m["msg_id"].as_str().map(String::from),
        ) else {
            return;
        };
        let c = self.conv_mut(conv);
        if let Some(old) = c.msgs.get(&seq)
            && old["version"].as_i64() > m["version"].as_i64()
        {
            return;
        }
        // Streaming text received so far stays until the final version.
        let mut m = m;
        if m["state"] == "streaming"
            && let Some(s) = c.msgs.get(&seq).and_then(|o| o.get("streamed")).cloned()
        {
            m["streamed"] = s;
        }
        c.msgs.insert(seq, m.clone());
        c.seq_of.insert(id.clone(), seq);
        if persist {
            if let Some(o) = m.as_object_mut() {
                o.remove("streamed");
            }
            self.batch.put_messages.push(m);
        }
        self.mark_changed(conv, &id);
    }

    /// The message under `key` as the app sees it, or None (gone).
    pub(crate) fn render(&self, conv: &str, key: &str) -> Option<Value> {
        if let Some(cm) = key.strip_prefix("local:") {
            // Until the message itself arrives, also once the server took
            // it (its answer can come before the message).
            let op = self
                .ops
                .iter()
                .find(|o| matches!(&o.kind, Kind::Send { cm: c, .. } if c == cm))?;
            let Kind::Send {
                text,
                msg_kind,
                payload,
                reply_to,
                sent,
                upload,
                ..
            } = &op.kind
            else {
                return None;
            };
            let uploading = upload.as_ref().filter(|u| u.file.is_none());
            let mut m = json!({
                "key": key, "msg_id": sent.as_deref().unwrap_or(key), "client_msg_id": cm, "conv_id": conv,
                "sender_id": self.me["id"], "sender_name": self.me["name"], "sender_type": "human",
                "kind": msg_kind.clone().unwrap_or_else(|| "text".into()),
                "content": text, "payload": payload, "reply_to": reply_to,
                "created_at": op.created_at, "version": 0, "state": "committed",
                "status": if sent.is_some() { "sent" } else if op.failed.is_some() { "failed" } else if uploading.is_some() { "uploading" } else { "sending" },
                "order": LOCAL_ORDER + op.n as i64,
            });
            // Until the server has the file: what the app picked, and how
            // far the upload is; then the file as the server checked it.
            if let Some(u) = upload {
                let mut p = u.preview();
                if let Some(f) = &u.file
                    && let (Some(o), Some(fo)) = (p.as_object_mut(), f.as_object())
                {
                    o.extend(fo.clone());
                }
                m["payload"] = p;
                m["upload"] = json!({"sent": if u.file.is_some() { u.size } else { u.sent() }, "size": u.size});
            }
            if let Some(e) = &op.failed {
                m["error"] = e.to_json();
            }
            return Some(m);
        }
        let c = self.convs.get(conv)?;
        let seq = *c.seq_of.get(key)?;
        let mut m = c.msgs.get(&seq)?.clone();
        m["key"] = json!(key);
        m["status"] = json!("sent");
        m["order"] = json!(seq);
        // The queue on top: edits and recalls not confirmed yet.
        for o in &self.ops {
            match &o.kind {
                Kind::Edit { target, text } if target == key => {
                    m["content"] = json!(text);
                    m["pending"] = json!("edit");
                }
                Kind::Recall { target } if target == key => {
                    m["state"] = json!("recalled");
                    m["content"] = json!("");
                    m["pending"] = json!("recall");
                }
                _ => {}
            }
        }
        Some(m)
    }

    /// Every key of a conversation's loaded window, in order.
    fn keys(&self, conv: &str) -> Vec<String> {
        let mut k: Vec<String> = self
            .convs
            .get(conv)
            .map(|c| {
                c.msgs
                    .values()
                    .filter_map(|m| m["msg_id"].as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        k.extend(self.ops.iter().filter_map(|o| match &o.kind {
            Kind::Send { cm, .. } if o.conv_id == conv => Some(format!("local:{cm}")),
            _ => None,
        }));
        k
    }

    pub(crate) fn view(&self, conv: &str) -> Vec<Value> {
        self.keys(conv)
            .iter()
            .filter_map(|k| self.render(conv, k))
            .collect()
    }

    pub(crate) fn snapshot_changes(&self, conv: &str) -> Vec<Value> {
        self.view(conv)
            .into_iter()
            .map(|m| json!({"op": "upsert", "message": m}))
            .collect()
    }

    /// The changes since the last input, per watched conversation, and the
    /// conversation list.
    pub(crate) fn emit_changes(&mut self) {
        for (conv, keys) in std::mem::take(&mut self.changed) {
            let changes: Vec<Value> = keys
                .iter()
                .map(|k| match self.render(&conv, k) {
                    Some(m) => json!({"op": "upsert", "message": m}),
                    None => json!({"op": "remove", "key": k}),
                })
                .collect();
            self.emit(json!({"type": "messages", "conv_id": conv, "changes": changes}));
        }
        if !self.list_changed.is_empty() {
            let ids = std::mem::take(&mut self.list_changed);
            let changes: Vec<Value> = ids
                .iter()
                .map(|id| json!({"op": "upsert", "conversation": self.summary(id)}))
                .collect();
            self.emit(json!({"type": "conversations", "changes": changes}));
        }
    }

    /// A conversation's line in the list.
    pub(crate) fn summary(&self, id: &str) -> Value {
        let Some(c) = self.convs.get(id) else {
            return json!({"conv_id": id});
        };
        let mut v = c.info.clone();
        if !v.is_object() {
            v = json!({});
        }
        v["conv_id"] = json!(id);
        v["last_seq"] = json!(c.last_seq);
        let local = self
            .ops
            .iter()
            .filter(|o| o.conv_id == id && matches!(o.kind, Kind::Send { sent: None, .. }))
            .count();
        v["sending"] = json!(local);
        v
    }

    pub(crate) fn conversations(&self) -> Value {
        let mut list: Vec<Value> = self
            .convs
            .keys()
            .map(|id| self.summary(id))
            // Conversations this person left stay, read-only, with `ended`.
            .collect();
        list.sort_by_key(|v| {
            std::cmp::Reverse(v["last_message"]["created_at"].as_i64().unwrap_or(0))
        });
        json!({"conversations": list})
    }

    /// `messages {conv_id}`: the window now; changes follow as events.
    pub(crate) fn cmd_messages(&mut self, cmd: &Value) -> Result<Value, ClientError> {
        let conv = need_str(cmd, "conv_id")?;
        self.watched.insert(conv.clone());
        self.conv_mut(&conv);
        Ok(json!({
            "conv_id": conv,
            "messages": self.view(&conv),
            "has_older": self.has_older(&conv),
            "older_end": self.convs[&conv].older_end,
        }))
    }

    pub(crate) fn cmd_focus(&mut self, cmd: &Value) -> Result<Value, ClientError> {
        self.focused = cmd["conv_id"].as_str().map(String::from);
        if self.phase == Phase::Live {
            self.request("focus", json!({"conv_id": self.focused}), Purpose::Focus);
        }
        Ok(json!({}))
    }

    /// `call {op, args}`: any request that needs the server now (decisions,
    /// approvals, queries). Not queued: offline it fails at once.
    pub(crate) fn cmd_call(&mut self, id: u64, cmd: &Value) {
        let Some(op) = cmd["op"].as_str() else {
            return self.reply(id, Err(ClientError::new("bad_request", "op is required")));
        };
        if self.phase != Phase::Live {
            return self.reply(id, Err(ClientError::new("disconnected", "not connected")));
        }
        let args = if cmd["args"].is_object() {
            cmd["args"].clone()
        } else {
            json!({})
        };
        self.request(op, args, Purpose::App(id));
    }
}

pub(crate) fn need_str(cmd: &Value, k: &str) -> Result<String, ClientError> {
    cmd[k]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(String::from)
        .ok_or_else(|| ClientError::new("bad_request", format!("{k} is required")))
}
