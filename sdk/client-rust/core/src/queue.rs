//! The offline queue (docs/12 §2.7): every change the user makes is stored
//! first, shown at once, and sent in order, one at a time per
//! conversation, at most `MAX_IN_FLIGHT` overall. Only the server's
//! business answer fails an operation; network and server trouble are
//! retried for as long as it takes (D113), with the same ids so nothing is
//! applied twice.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::view::need_str;
use crate::*;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Kind {
    Send {
        cm: String,
        text: String,
        #[serde(default)]
        msg_kind: Option<String>,
        #[serde(default)]
        payload: Option<Value>,
        #[serde(default)]
        reply_to: Option<String>,
        /// The stored message, once the server answered (kept until its
        /// event arrives, so the message does not blink).
        #[serde(default)]
        sent: Option<String>,
    },
    /// `target` is a msg_id, or `local:<client_msg_id>` of a send still
    /// in the queue.
    Edit {
        target: String,
        text: String,
    },
    Recall {
        target: String,
    },
    Read {
        seq: i64,
    },
    Feedback {
        msg_id: String,
        rating: String,
        #[serde(default)]
        reason: Option<String>,
    },
    Respond {
        msg_id: String,
        block_id: String,
        #[serde(default)]
        action: Option<String>,
        #[serde(default)]
        values: Option<Value>,
    },
}

impl Kind {
    fn name(&self) -> &'static str {
        match self {
            Kind::Send { .. } => "send",
            Kind::Edit { .. } => "edit",
            Kind::Recall { .. } => "recall",
            Kind::Read { .. } => "read",
            Kind::Feedback { .. } => "feedback",
            Kind::Respond { .. } => "respond",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Op {
    pub id: String,
    pub conv_id: String,
    #[serde(flatten)]
    pub kind: Kind,
    pub created_at: i64,
    /// Creation order (stable: local messages sort by it).
    #[serde(default)]
    pub n: u64,
    #[serde(default)]
    pub attempts: u32,
    #[serde(default)]
    pub failed: Option<ClientError>,
    #[serde(skip)]
    pub in_flight: Option<u64>,
    #[serde(default)]
    pub not_before: Option<i64>,
}

impl Op {
    pub fn to_json(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }

    pub fn from_json(v: &Value) -> Option<Op> {
        serde_json::from_value(v.clone()).ok()
    }

    fn waiting(&self) -> bool {
        self.in_flight.is_none() && self.failed.is_none()
    }
}

fn local(cm: &str) -> String {
    format!("local:{cm}")
}

impl Core {
    fn push_op(&mut self, conv: &str, kind: Kind) -> String {
        self.op_n += 1;
        let op = Op {
            id: self.rng.id(self.now),
            conv_id: conv.to_string(),
            kind,
            created_at: self.now,
            n: self.op_n,
            attempts: 0,
            failed: None,
            in_flight: None,
            not_before: None,
        };
        self.batch.put_ops.push(op.to_json());
        let id = op.id.clone();
        self.ops.push(op);
        id
    }

    fn save_op(&mut self, i: usize) {
        let v = self.ops[i].to_json();
        self.batch.put_ops.push(v);
    }

    fn remove_op(&mut self, i: usize) -> Op {
        let op = self.ops.remove(i);
        self.batch.remove_ops.push(op.id.clone());
        op
    }

    fn find_send(&self, cm: &str) -> Option<usize> {
        self.ops
            .iter()
            .position(|o| matches!(&o.kind, Kind::Send { cm: c, .. } if c == cm))
    }

    /// The key a command names: `msg_id`, or `client_msg_id` of a send.
    fn target(&self, cmd: &Value) -> Result<String, ClientError> {
        if let Some(m) = cmd["msg_id"].as_str().filter(|s| !s.is_empty()) {
            return Ok(m.to_string());
        }
        let cm = need_str(cmd, "client_msg_id")
            .map_err(|_| ClientError::new("bad_request", "msg_id or client_msg_id is required"))?;
        // Sent already: the stored message.
        if let Some(id) = self.sent_ids.get(&cm) {
            return Ok(id.clone());
        }
        match self.find_send(&cm) {
            Some(i) => match &self.ops[i].kind {
                Kind::Send { sent: Some(id), .. } => Ok(id.clone()),
                _ => Ok(local(&cm)),
            },
            None => Err(ClientError::new("not_found", "no such message")),
        }
    }

    pub(crate) fn cmd_send(&mut self, cmd: &Value) -> Result<Value, ClientError> {
        let conv = need_str(cmd, "conv_id")?;
        let text = cmd["text"].as_str().unwrap_or("").to_string();
        let msg_kind = cmd["kind"].as_str().map(String::from);
        if text.is_empty() && msg_kind.is_none() {
            return Err(ClientError::new("bad_request", "text is required"));
        }
        let cm = match cmd["client_msg_id"].as_str() {
            Some(c) if !c.is_empty() => c.to_string(),
            _ => self.rng.id(self.now),
        };
        // The same client_msg_id twice is the same message.
        if self.find_send(&cm).is_none() && !self.sent_ids.contains_key(&cm) {
            self.push_op(
                &conv,
                Kind::Send {
                    cm: cm.clone(),
                    text,
                    msg_kind,
                    payload: cmd.get("payload").filter(|p| !p.is_null()).cloned(),
                    reply_to: cmd["reply_to"].as_str().map(String::from),
                    sent: None,
                },
            );
            self.conv_mut(&conv);
            self.mark_changed(&conv, &local(&cm));
            self.list_changed.insert(conv.clone());
        }
        Ok(json!({"client_msg_id": cm, "key": local(&cm)}))
    }

    pub(crate) fn cmd_edit(&mut self, cmd: &Value) -> Result<Value, ClientError> {
        let conv = need_str(cmd, "conv_id")?;
        let text = need_str(cmd, "text")?;
        let target = self.target(cmd)?;
        if let Some(cm) = target.strip_prefix("local:") {
            // Not sent yet: the send itself changes.
            if let Some(i) = self.find_send(cm)
                && self.ops[i].in_flight.is_none()
            {
                if let Kind::Send { text: t, .. } = &mut self.ops[i].kind {
                    *t = text;
                }
                self.save_op(i);
                self.mark_changed(&conv, &target);
                return Ok(json!({"key": target}));
            }
        }
        // The same message edited again before it went out: the last wins.
        if let Some(i) = self.ops.iter().position(|o| {
            o.waiting() && matches!(&o.kind, Kind::Edit { target: t, .. } if *t == target)
        }) {
            if let Kind::Edit { text: t, .. } = &mut self.ops[i].kind {
                *t = text;
            }
            self.save_op(i);
        } else {
            self.push_op(
                &conv,
                Kind::Edit {
                    target: target.clone(),
                    text,
                },
            );
        }
        self.mark_changed(&conv, &target);
        Ok(json!({"key": target}))
    }

    pub(crate) fn cmd_recall(&mut self, cmd: &Value) -> Result<Value, ClientError> {
        let conv = need_str(cmd, "conv_id")?;
        let target = self.target(cmd)?;
        if let Some(cm) = target.strip_prefix("local:").map(String::from)
            && let Some(i) = self.find_send(&cm)
            && self.ops[i].in_flight.is_none()
        {
            // Never sent: it and its edits are dropped.
            self.drop_with_dependents(&cm);
            self.mark_changed(&conv, &target);
            self.list_changed.insert(conv);
            return Ok(json!({"key": target}));
        }
        // Pending edits of it are moot.
        let edits: Vec<usize> = self
            .ops
            .iter()
            .enumerate()
            .filter(|(_, o)| {
                o.waiting() && matches!(&o.kind, Kind::Edit { target: t, .. } if *t == target)
            })
            .map(|(i, _)| i)
            .rev()
            .collect();
        for i in edits {
            self.remove_op(i);
        }
        self.push_op(
            &conv,
            Kind::Recall {
                target: target.clone(),
            },
        );
        self.mark_changed(&conv, &target);
        Ok(json!({"key": target}))
    }

    pub(crate) fn cmd_read(&mut self, cmd: &Value) -> Result<Value, ClientError> {
        let conv = need_str(cmd, "conv_id")?;
        let seq = match cmd["seq"].as_i64() {
            Some(s) => s,
            None => self.convs.get(&conv).map(|c| c.last_seq).unwrap_or(0),
        };
        // One read per conversation, the furthest.
        if let Some(i) = self
            .ops
            .iter()
            .position(|o| o.conv_id == conv && o.waiting() && matches!(o.kind, Kind::Read { .. }))
        {
            if let Kind::Read { seq: s } = &mut self.ops[i].kind {
                *s = (*s).max(seq);
            }
            self.save_op(i);
        } else {
            self.push_op(&conv, Kind::Read { seq });
        }
        let c = self.conv_mut(&conv);
        c.info["unread"] = json!(0);
        c.dirty = true;
        self.list_changed.insert(conv);
        Ok(json!({"seq": seq}))
    }

    pub(crate) fn cmd_feedback(&mut self, cmd: &Value) -> Result<Value, ClientError> {
        let conv = need_str(cmd, "conv_id")?;
        let msg_id = need_str(cmd, "msg_id")?;
        let rating = need_str(cmd, "rating")?;
        let reason = cmd["reason"].as_str().map(String::from);
        if let Some(i) = self.ops.iter().position(|o| {
            o.waiting() && matches!(&o.kind, Kind::Feedback { msg_id: m, .. } if *m == msg_id)
        }) {
            self.ops[i].kind = Kind::Feedback {
                msg_id: msg_id.clone(),
                rating,
                reason,
            };
            self.save_op(i);
        } else {
            self.push_op(
                &conv,
                Kind::Feedback {
                    msg_id: msg_id.clone(),
                    rating,
                    reason,
                },
            );
        }
        Ok(json!({"msg_id": msg_id}))
    }

    pub(crate) fn cmd_respond(&mut self, cmd: &Value) -> Result<Value, ClientError> {
        let conv = need_str(cmd, "conv_id")?;
        let msg_id = need_str(cmd, "msg_id")?;
        let block_id = need_str(cmd, "block_id")?;
        let id = self.push_op(
            &conv,
            Kind::Respond {
                msg_id,
                block_id,
                action: cmd["action"].as_str().map(String::from),
                values: cmd.get("values").filter(|v| !v.is_null()).cloned(),
            },
        );
        Ok(json!({"op_id": id}))
    }

    pub(crate) fn cmd_retry(&mut self, cmd: &Value) -> Result<Value, ClientError> {
        let conv = need_str(cmd, "conv_id")?;
        let cm = need_str(cmd, "client_msg_id")?;
        let i = self
            .find_send(&cm)
            .filter(|i| self.ops[*i].failed.is_some())
            .ok_or_else(|| {
                ClientError::new("not_found", "no failed message with this client_msg_id")
            })?;
        let op = &mut self.ops[i];
        op.failed = None;
        op.attempts = 0;
        op.not_before = None;
        // The server forgets a client_msg_id after 7 days: a new one then.
        let mut key = local(&cm);
        if self.now - op.created_at > DEDUP_MS {
            let fresh = self.rng.id(self.now);
            if let Kind::Send { cm: c, .. } = &mut self.ops[i].kind {
                *c = fresh.clone();
            }
            self.ops[i].created_at = self.now;
            self.mark_changed(&conv, &key);
            key = local(&fresh);
        }
        self.save_op(i);
        self.mark_changed(&conv, &key);
        Ok(json!({"key": key}))
    }

    pub(crate) fn cmd_discard(&mut self, cmd: &Value) -> Result<Value, ClientError> {
        let conv = need_str(cmd, "conv_id")?;
        let cm = need_str(cmd, "client_msg_id")?;
        let i = self
            .find_send(&cm)
            .ok_or_else(|| ClientError::new("not_found", "no such message waiting"))?;
        if self.ops[i].in_flight.is_some() {
            return Err(ClientError::new(
                "conflict",
                "being sent; wait for the result",
            ));
        }
        self.drop_with_dependents(&cm);
        self.mark_changed(&conv, &local(&cm));
        self.list_changed.insert(conv);
        Ok(json!({}))
    }

    /// Drops a send and every operation on it.
    fn drop_with_dependents(&mut self, cm: &str) {
        let key = local(cm);
        let gone: Vec<usize> = self
            .ops
            .iter()
            .enumerate()
            .filter(|(_, o)| match &o.kind {
                Kind::Send { cm: c, .. } => c == cm,
                Kind::Edit { target, .. } | Kind::Recall { target } => *target == key,
                _ => false,
            })
            .map(|(i, _)| i)
            .rev()
            .collect();
        for i in gone {
            self.remove_op(i);
        }
    }

    /// Sends what may go now: per conversation the first waiting
    /// operation, if nothing of that conversation is in flight.
    pub(crate) fn pump_queue(&mut self) {
        if self.phase != Phase::Live || self.paused_until.is_some() {
            return;
        }
        let mut in_flight = self.ops.iter().filter(|o| o.in_flight.is_some()).count();
        let mut busy: std::collections::HashSet<String> = self
            .ops
            .iter()
            .filter(|o| o.in_flight.is_some())
            .map(|o| o.conv_id.clone())
            .collect();
        let mut i = 0;
        while i < self.ops.len() && in_flight < MAX_IN_FLIGHT {
            let o = &self.ops[i];
            let conv = o.conv_id.clone();
            let skip = o.failed.is_some() || matches!(o.kind, Kind::Send { sent: Some(_), .. });
            if skip || busy.contains(&conv) {
                i += 1;
                continue;
            }
            // In order within a conversation: the first waiting one decides.
            busy.insert(conv.clone());
            if o.not_before.is_some_and(|t| t > self.now) {
                i += 1;
                continue;
            }
            let Some((name, args)) = self.op_request(i) else {
                // Its target failed: it fails too.
                let op = self.remove_op(i);
                self.emit_op(
                    &op,
                    Some(&ClientError::new(
                        "conflict",
                        "the message it applies to was not sent",
                    )),
                );
                continue;
            };
            let rid = self.request(name, args, Purpose::Op(self.ops[i].id.clone()));
            self.ops[i].in_flight = Some(rid);
            in_flight += 1;
            i += 1;
        }
    }

    /// The request of an operation, or None when it can no longer be made.
    fn op_request(&self, i: usize) -> Option<(&'static str, Value)> {
        let o = &self.ops[i];
        let resolve = |t: &str| -> Option<String> {
            match t.strip_prefix("local:") {
                None => Some(t.to_string()),
                Some(cm) => self.sent_ids.get(cm).cloned().or_else(|| {
                    self.ops.iter().find_map(|x| match &x.kind {
                        Kind::Send {
                            cm: c,
                            sent: Some(id),
                            ..
                        } if c == cm => Some(id.clone()),
                        _ => None,
                    })
                }),
            }
        };
        let v = match &o.kind {
            Kind::Send {
                cm,
                text,
                msg_kind,
                payload,
                reply_to,
                ..
            } => {
                let mut v = json!({"conv_id": o.conv_id, "client_msg_id": cm, "text": text});
                if let Some(k) = msg_kind {
                    v["kind"] = json!(k);
                }
                if let Some(p) = payload {
                    v["payload"] = p.clone();
                }
                if let Some(r) = reply_to {
                    v["reply_to"] = json!(r);
                }
                v
            }
            Kind::Edit { target, text } => json!({"msg_id": resolve(target)?, "text": text}),
            Kind::Recall { target } => json!({"msg_id": resolve(target)?}),
            Kind::Read { seq } => json!({"conv_id": o.conv_id, "seq": seq}),
            Kind::Feedback {
                msg_id,
                rating,
                reason,
            } => {
                let mut v = json!({"msg_id": msg_id, "rating": rating});
                if let Some(r) = reason {
                    v["reason"] = json!(r);
                }
                v
            }
            Kind::Respond {
                msg_id,
                block_id,
                action,
                values,
            } => {
                let mut v = json!({"msg_id": msg_id, "block_id": block_id});
                if let Some(a) = action {
                    v["action"] = json!(a);
                }
                if let Some(x) = values {
                    v["values"] = x.clone();
                }
                v
            }
        };
        Some((o.kind.name(), v))
    }

    fn emit_op(&mut self, op: &Op, err: Option<&ClientError>) {
        self.emit(json!({
            "type": "operation", "op_id": op.id, "kind": op.kind.name(), "conv_id": op.conv_id,
            "result": if err.is_some() { "failed" } else { "done" },
            "error": err.map(|e| e.to_json()),
        }));
    }

    fn op_key(op: &Op) -> Option<String> {
        match &op.kind {
            Kind::Send { cm, .. } => Some(local(cm)),
            Kind::Edit { target, .. } | Kind::Recall { target } => Some(target.clone()),
            _ => None,
        }
    }

    pub(crate) fn on_op_result(&mut self, op_id: &str, f: &Value, err: Option<ClientError>) {
        let Some(i) = self.ops.iter().position(|o| o.id == op_id) else {
            return;
        };
        self.ops[i].in_flight = None;
        let conv = self.ops[i].conv_id.clone();
        let key = Self::op_key(&self.ops[i]);
        let err = err.and_then(|e| {
            // Repeating something that already happened is success.
            let done = matches!(
                (&self.ops[i].kind, e.code.as_str()),
                (Kind::Recall { .. }, "conflict")
                    | (Kind::Respond { .. }, "conflict")
                    | (Kind::Read { .. }, _)
            );
            (!done).then_some(e)
        });
        match err {
            None => {
                self.earn_budget();
                if let Kind::Send { cm, sent, .. } = &mut self.ops[i].kind {
                    let id = f["msg_id"].as_str().unwrap_or("").to_string();
                    let cm = cm.clone();
                    *sent = Some(id.clone());
                    self.sent_ids.insert(cm.clone(), id.clone());
                    // Its event may have come first.
                    let arrived = self
                        .convs
                        .get(&conv)
                        .is_some_and(|c| c.seq_of.contains_key(&id));
                    if arrived {
                        let op = self.remove_op(i);
                        self.emit_op(&op, None);
                    } else {
                        self.save_op(i);
                        let op = self.ops[i].clone();
                        self.emit_op(&op, None);
                    }
                    self.mark_changed(&conv, &local(&cm));
                    self.mark_changed(&conv, &id);
                } else {
                    let op = self.remove_op(i);
                    self.emit_op(&op, None);
                }
            }
            Some(e) if e.retryable => {
                self.note_failure("operation", e.to_json());
                // A limit the server sets is waited out; trouble on its
                // side costs retry budget.
                if e.code != "rate_limited" {
                    self.spend_budget();
                }
                let op = &mut self.ops[i];
                op.attempts += 1;
                let exp = (BACKOFF_BASE_MS << op.attempts.min(6)).min(BACKOFF_CAP_MS);
                let wait = self.rng.upto(exp).max(e.retry_after_ms.unwrap_or(0));
                self.ops[i].not_before = Some(self.now + wait);
            }
            Some(e) => {
                if let Kind::Send { cm, .. } = &self.ops[i].kind {
                    let cm = cm.clone();
                    // Shown as failed; the app may retry or discard it.
                    self.ops[i].failed = Some(e.clone());
                    self.save_op(i);
                    let op = self.ops[i].clone();
                    self.emit_op(&op, Some(&e));
                    // What depends on it fails with it.
                    let target = local(&cm);
                    let deps: Vec<usize> = self
                        .ops
                        .iter()
                        .enumerate()
                        .filter(|(_, o)| matches!(&o.kind, Kind::Edit { target: t, .. } | Kind::Recall { target: t } if *t == target))
                        .map(|(j, _)| j)
                        .rev()
                        .collect();
                    for j in deps {
                        let d = self.remove_op(j);
                        self.emit_op(&d, Some(&e));
                    }
                } else {
                    let op = self.remove_op(i);
                    self.emit_op(&op, Some(&e));
                }
                self.list_changed.insert(conv.clone());
            }
        }
        if let Some(k) = key {
            self.mark_changed(&conv, &k);
        }
    }

    /// The connection went while an operation was in flight: it is sent
    /// again with the same ids.
    pub(crate) fn op_unknown(&mut self, op_id: &str) {
        if let Some(o) = self.ops.iter_mut().find(|o| o.id == op_id) {
            o.in_flight = None;
        }
    }

    /// A stored message arrived: the local copy of a send of ours is done.
    pub(crate) fn settle_send(&mut self, conv: &str, m: &Value) {
        let Some(id) = m["msg_id"].as_str() else {
            return;
        };
        let by_id = self
            .ops
            .iter()
            .position(|o| matches!(&o.kind, Kind::Send { sent: Some(s), .. } if s == id));
        let by_cm = m["client_msg_id"].as_str().and_then(|cm| {
            self.ops.iter().position(|o| {
                o.in_flight.is_none()
                    && matches!(&o.kind, Kind::Send { cm: c, sent: None, .. } if c == cm)
                    && o.conv_id == conv
            })
        });
        if let Some(i) = by_id.or(by_cm) {
            let op = self.remove_op(i);
            if let Kind::Send { cm, .. } = &op.kind {
                self.sent_ids.insert(cm.clone(), id.to_string());
                self.mark_changed(conv, &local(cm));
            }
            if by_cm.is_some() && by_id.is_none() {
                self.emit_op(&op, None);
            }
            self.list_changed.insert(conv.to_string());
        }
    }
}
