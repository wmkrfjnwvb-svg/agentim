//! Threads, reactions and pinned messages on the device
//! (docs/en/design/messaging.md, MSG-19 to MSG-21): the events of the log
//! keep each message's `thread` summary, `reactions` and `pinned` up to
//! date, so the app draws them without asking (`null` when there are
//! none). `msg_summary` gives the
//! server's numbers for messages loaded from history.

use serde_json::{Value, json};

use crate::*;

/// People kept with a reaction key or a thread summary.
const PEOPLE: usize = 5;

impl Core {
    /// A message by id, as stored here.
    fn stored(&self, conv: &str, msg: &str) -> Option<Value> {
        let c = self.convs.get(conv)?;
        let seq = c.seq_of.get(msg)?;
        c.msgs.get(seq).cloned()
    }

    /// Changes a stored message (its version stays) and shows the change.
    fn change(&mut self, conv: &str, msg: &str, f: impl FnOnce(&mut Value)) {
        let Some(mut m) = self.stored(conv, msg) else {
            return;
        };
        f(&mut m);
        self.put_msg(conv, m, true);
        self.mark_changed(conv, msg);
    }

    /// A reply only in a thread: it is not the conversation's latest
    /// message, nor unread there.
    pub(crate) fn only_in_thread(m: &Value) -> bool {
        m["thread_id"].is_string() && m["thread_shown"] != true
    }

    /// What an event of the log changes in threads, reactions and pins.
    pub(crate) fn apply_social(&mut self, conv: &str, ev: &Value) {
        let mine = self.me["id"].as_str().unwrap_or("").to_string();
        let d = &ev["data"];
        let target = d["msg_id"].as_str().unwrap_or("").to_string();
        match ev["type"].as_str().unwrap_or("") {
            "msg.created" => {
                let m = &ev["msg"];
                let Some(root) = m["thread_id"].as_str() else {
                    return;
                };
                let (seq, at, sender) = (
                    m["created_seq"].clone(),
                    m["created_at"].clone(),
                    m["sender_id"].clone(),
                );
                let me_too = sender == mine.as_str();
                self.change(conv, root, |r| {
                    let t = &mut r["thread"];
                    let n = t["replies"].as_i64().unwrap_or(0);
                    t["replies"] = json!(n + 1);
                    t["last_seq"] = seq;
                    t["last_at"] = at;
                    t["last_sender"] = sender.clone();
                    let mut people: Vec<Value> =
                        t["participants"].as_array().cloned().unwrap_or_default();
                    if !people.contains(&sender) && people.len() < PEOPLE {
                        people.push(sender);
                    }
                    t["participants"] = json!(people);
                    if me_too {
                        t["following"] = json!(true);
                    }
                });
            }
            "msg.recalled" => {
                let m = &ev["msg"];
                let id = m["msg_id"].as_str().unwrap_or("").to_string();
                // Its reactions and pin go with it.
                self.change(conv, &id, |x| {
                    x["reactions"] = Value::Null;
                    x["pinned"] = Value::Null;
                });
                if let Some(root) = m["thread_id"].as_str() {
                    self.change(conv, root, |r| {
                        let n = r["thread"]["replies"].as_i64().unwrap_or(1);
                        r["thread"]["replies"] = json!((n - 1).max(0));
                    });
                }
            }
            "reaction.added" | "reaction.removed" => {
                let added = ev["type"] == "reaction.added";
                let (key, user) = (d["key"].clone(), d["user_id"].clone());
                let me = user == mine.as_str();
                self.change(conv, &target, |m| {
                    let mut list: Vec<Value> =
                        m["reactions"].as_array().cloned().unwrap_or_default();
                    let i = match list.iter().position(|r| r["key"] == key) {
                        Some(i) => i,
                        None if added => {
                            list.push(json!({"key": key, "count": 0, "me": false, "users": []}));
                            list.len() - 1
                        }
                        None => return,
                    };
                    let r = &mut list[i];
                    let n = r["count"].as_i64().unwrap_or(0);
                    let mut users: Vec<Value> = r["users"].as_array().cloned().unwrap_or_default();
                    users.retain(|u| *u != user);
                    if added {
                        r["count"] = json!(n + 1);
                        users.insert(0, user);
                        users.truncate(PEOPLE);
                    } else {
                        r["count"] = json!((n - 1).max(0));
                    }
                    r["users"] = json!(users);
                    if me {
                        r["me"] = json!(added);
                    }
                    list.retain(|r| r["count"].as_i64() > Some(0));
                    m["reactions"] = if list.is_empty() {
                        Value::Null
                    } else {
                        json!(list)
                    };
                });
            }
            "msg.pinned" => {
                let pin = json!({"by": d["by"], "at": ev["created_at"], "seq": ev["seq"]});
                self.change(conv, &target, |m| m["pinned"] = pin);
            }
            "msg.unpinned" => {
                self.change(conv, &target, |m| m["pinned"] = Value::Null);
            }
            "conv.updated" => {
                if let Some(p) = d["pin_policy"].as_str() {
                    let c = self.conv_mut(conv);
                    c.info["pin_policy"] = json!(p);
                    c.dirty = true;
                }
            }
            // A deleted account: its reactions go, and the pins of its messages.
            "user.erased" => {
                let Some(u) = d["user_id"].as_str() else {
                    return;
                };
                let ids: Vec<String> = self
                    .convs
                    .get(conv)
                    .map(|c| {
                        c.msgs
                            .values()
                            .filter(|m| m["reactions"].is_array() || m["pinned"].is_object())
                            .filter_map(|m| m["msg_id"].as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();
                for id in ids {
                    let theirs = self.stored(conv, &id).is_some_and(|m| m["sender_id"] == u);
                    self.change(conv, &id, |m| {
                        if theirs {
                            m["reactions"] = Value::Null;
                            m["pinned"] = Value::Null;
                            return;
                        }
                        let mut list: Vec<Value> =
                            m["reactions"].as_array().cloned().unwrap_or_default();
                        for r in &mut list {
                            let had = r["users"]
                                .as_array()
                                .is_some_and(|us| us.iter().any(|x| x == u));
                            if had {
                                let n = r["count"].as_i64().unwrap_or(1);
                                r["count"] = json!(n - 1);
                                if let Some(us) = r["users"].as_array_mut() {
                                    us.retain(|x| x != u);
                                }
                            }
                        }
                        list.retain(|r| r["count"].as_i64() > Some(0));
                        m["reactions"] = if list.is_empty() {
                            Value::Null
                        } else {
                            json!(list)
                        };
                    });
                }
            }
            _ => {}
        }
    }

    /// The user read or (un)followed a thread on another device.
    pub(crate) fn on_thread_change(&mut self, f: &Value) {
        let (Some(conv), Some(root)) = (f["conv_id"].as_str(), f["thread_id"].as_str()) else {
            return;
        };
        let (conv, root) = (conv.to_string(), root.to_string());
        let f2 = f.clone();
        self.change(&conv, &root, |r| {
            if f2["type"] == "thread_read" {
                r["thread"]["read_seq"] = f2["seq"].clone();
            } else {
                r["thread"]["following"] = f2["following"].clone();
            }
        });
        self.emit(f.clone());
    }
}
