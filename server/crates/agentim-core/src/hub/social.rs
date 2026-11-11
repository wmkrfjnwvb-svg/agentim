//! Threads, reactions and pinned messages, read side
//! (docs/en/design/messaging.md, MSG-19 to MSG-21): what a member sees of
//! them, read from the conversation's database without going through its
//! writer.

use serde_json::{Value, json};

use super::Hub;
use crate::db::Db;
use crate::db::social::ThreadRow;
use crate::logic::social::SUMMARY_PEOPLE;
use crate::model::*;

const THREADS_DEFAULT: i64 = 20;
const THREADS_MAX: i64 = 100;
const REPLIES_DEFAULT: i64 = 50;
const REPLIES_MAX: i64 = 200;
const SUMMARY_MAX: usize = 100;

fn limit_of(limit: Option<i64>, default: i64, max: i64) -> ApiResult<i64> {
    let l = limit.unwrap_or(default);
    if !(1..=max).contains(&l) {
        return Err(rule!(
            "social.limit",
            "bad_request",
            format!("limit must be 1..{max}")
        ));
    }
    Ok(l)
}

/// A thread's summary as `m` sees it.
async fn thread_view(
    db: &mut Db<'_>,
    conv: &str,
    m: &Member,
    t: &ThreadRow,
    root: Option<&MsgView>,
) -> ApiResult<Value> {
    let tm = db.thread_member(conv, &t.root_id, &m.user_id).await?;
    let read = tm
        .as_ref()
        .map_or(0, |x| x.read_seq)
        .max(m.visible_from - 1);
    let unread = db.thread_unread(conv, &t.root_id, &m.user_id, read).await?;
    let root_sender = root.map_or("", |r| r.sender_id.as_str());
    let (people, total) = db
        .thread_participants(conv, &t.root_id, root_sender, SUMMARY_PEOPLE + 1)
        .await?;
    let people: Vec<String> = people
        .into_iter()
        .filter(|p| !p.is_empty())
        .take(SUMMARY_PEOPLE)
        .collect();
    Ok(json!({
        "thread_id": t.root_id,
        "replies": t.replies,
        "last_seq": t.last_seq,
        "last_at": t.last_at,
        "last_sender": t.last_sender,
        "participants": people,
        "participants_count": total - i64::from(root_sender.is_empty()),
        "following": tm.as_ref().is_some_and(|x| x.following),
        "read_seq": tm.as_ref().map_or(0, |x| x.read_seq),
        "unread": unread,
    }))
}

impl Hub {
    /// The threads the caller follows that have replies, latest reply
    /// first, each with its unread count.
    pub(super) async fn list_threads(
        &self,
        me: &User,
        conv: Option<&str>,
        before: Option<i64>,
        limit: Option<i64>,
    ) -> ApiResult<Value> {
        let limit = limit_of(limit, THREADS_DEFAULT, THREADS_MAX)?;
        let before = before.unwrap_or(i64::MAX);
        let convs: Vec<String> = match conv {
            Some(c) => {
                self.reader_member(me, c, None).await?;
                vec![c.to_string()]
            }
            None => {
                let mine: std::collections::HashSet<String> =
                    self.convs_of(&me.id).await?.into_iter().collect();
                let mut followed = vec![];
                for s in &self.inner.shards {
                    let u = me.id.clone();
                    let cs = self
                        .read(s, move |db| {
                            Box::pin(async move { db.thread_convs_of(&u).await })
                        })
                        .await?;
                    followed.extend(cs.into_iter().filter(|c| mine.contains(c)));
                }
                followed
            }
        };
        let mut items = vec![];
        for c in convs {
            let Ok(m) = self.reader_member(me, &c, None).await else {
                continue;
            };
            let shard = self.conv_shard(&c)?;
            let page = self
                .read(shard, move |db| {
                    Box::pin(async move {
                        // The system (the open API without a member) sees every thread.
                        let rows = if m.user_id == SYSTEM_ID {
                            db.threads_page(&c, before, limit + 1).await?
                        } else {
                            db.followed_threads(&c, &m.user_id, before, limit + 1)
                                .await?
                                .into_iter()
                                .map(|(t, _)| t)
                                .collect()
                        };
                        let mut out = vec![];
                        for t in rows {
                            let root = db
                                .message(&t.root_id)
                                .await?
                                .filter(|r| r.created_seq >= m.visible_from);
                            let mut v = thread_view(db, &c, &m, &t, root.as_ref()).await?;
                            v["conv_id"] = json!(c);
                            v["root"] = json!(root);
                            out.push(v);
                        }
                        Ok(out)
                    })
                })
                .await?;
            items.extend(page);
        }
        items.sort_by(|a, b| {
            b["last_at"]
                .as_i64()
                .cmp(&a["last_at"].as_i64())
                .then_with(|| a["thread_id"].as_str().cmp(&b["thread_id"].as_str()))
        });
        let more = items.len() as i64 > limit;
        items.truncate(limit as usize);
        let unread = items
            .iter()
            .filter(|t| t["unread"].as_i64().unwrap_or(0) > 0)
            .count();
        Ok(json!({"threads": items, "more": more, "unread_threads": unread}))
    }

    /// A thread's root, its summary and the replies after a seq.
    pub(super) async fn pull_thread(
        &self,
        me: &User,
        conv: &str,
        thread_id: &str,
        after_seq: i64,
        limit: Option<i64>,
    ) -> ApiResult<Value> {
        let limit = limit_of(limit, REPLIES_DEFAULT, REPLIES_MAX)?;
        let m = self.reader_member(me, conv, None).await?;
        let shard = self.conv_shard(conv)?;
        let (conv, id) = (conv.to_string(), thread_id.to_string());
        self.read(shard, move |db| {
            Box::pin(async move {
                let root = db.message(&id).await?;
                let Some(root) = root.filter(|r| r.conv_id == conv && r.thread_id.is_none())
                else {
                    return Err(rule!("thread.unknown", "not_found", "no such thread"));
                };
                let visible = root.created_seq >= m.visible_from;
                let after = after_seq.max(m.visible_from - 1);
                let mut replies = db.thread_replies(&conv, &id, after, limit + 1).await?;
                let more = replies.len() as i64 > limit;
                replies.truncate(limit as usize);
                let summary = match db.thread(&conv, &id).await? {
                    Some(t) => thread_view(db, &conv, &m, &t, visible.then_some(&root)).await?,
                    None => json!({"thread_id": id, "replies": 0, "participants": [], "unread": 0, "following": false}),
                };
                Ok(json!({
                    "conv_id": conv,
                    "root": visible.then_some(root),
                    "thread": summary,
                    "replies": replies,
                    "more": more,
                }))
            })
        })
        .await
    }

    /// Who reacted to a message with a key, oldest first.
    pub(super) async fn list_reactions(
        &self,
        me: &User,
        conv: &str,
        msg_id: &str,
        key: &str,
        after_seq: i64,
        limit: Option<i64>,
    ) -> ApiResult<Value> {
        let limit = limit_of(limit, REPLIES_DEFAULT, REPLIES_MAX)?;
        let m = self.reader_member(me, conv, None).await?;
        let shard = self.conv_shard(conv)?;
        let (conv, id, key) = (conv.to_string(), msg_id.to_string(), key.to_string());
        let rows = self
            .read(shard, move |db| {
                Box::pin(async move {
                    match db.message(&id).await? {
                        Some(x) if x.conv_id == conv && x.created_seq >= m.visible_from => {}
                        _ => {
                            return Err(rule!(
                                "reaction.unknown_message",
                                "not_found",
                                "no such message"
                            ));
                        }
                    }
                    db.reaction_users(&id, &key, after_seq, limit + 1).await
                })
            })
            .await?;
        let more = rows.len() as i64 > limit;
        let users: Vec<Value> = rows
            .into_iter()
            .take(limit as usize)
            .map(|(u, seq, at)| {
                let name = self.inner.sh.user(&u).map(|x| x.name);
                json!({"user_id": u, "name": name, "seq": seq, "at": at})
            })
            .collect();
        Ok(json!({"users": users, "more": more}))
    }

    /// The conversation's pinned messages, latest pin first, with the
    /// messages as the caller sees them.
    pub(super) async fn list_pins(
        &self,
        me: &User,
        conv: &str,
        before_seq: Option<i64>,
        limit: Option<i64>,
    ) -> ApiResult<Value> {
        let limit = limit_of(limit, THREADS_DEFAULT, THREADS_MAX)?;
        let m = self.reader_member(me, conv, None).await?;
        let shard = self.conv_shard(conv)?;
        let conv = conv.to_string();
        self.read(shard, move |db| {
            Box::pin(async move {
                let before = before_seq.unwrap_or(i64::MAX);
                let rows = db.pins(&conv, before, limit + 1).await?;
                let more = rows.len() as i64 > limit;
                let mut pins = vec![];
                for (msg_id, by, at, seq) in rows.into_iter().take(limit as usize) {
                    let Some(msg) = db
                        .message(&msg_id)
                        .await?
                        .filter(|x| x.created_seq >= m.visible_from)
                    else {
                        continue;
                    };
                    pins.push(
                        json!({"msg_id": msg_id, "by": by, "at": at, "seq": seq, "msg": msg}),
                    );
                }
                Ok(json!({
                    "pins": pins,
                    "more": more,
                    "count": db.pin_count(&conv).await?,
                    "pin_policy": db.pin_policy(&conv).await?,
                }))
            })
        })
        .await
    }

    /// The thread summary, reactions and pin of messages, as the caller
    /// sees them; unknown or hidden messages are left out.
    pub(super) async fn msg_summary(
        &self,
        me: &User,
        conv: &str,
        msg_ids: &[String],
    ) -> ApiResult<Value> {
        if msg_ids.is_empty() || msg_ids.len() > SUMMARY_MAX {
            return Err(rule!(
                "social.summary_size",
                "bad_request",
                format!("1..{SUMMARY_MAX} message ids")
            ));
        }
        let m = self.reader_member(me, conv, None).await?;
        let shard = self.conv_shard(conv)?;
        let (conv, ids) = (conv.to_string(), msg_ids.to_vec());
        self.read(shard, move |db| {
            Box::pin(async move {
                let mut seen = vec![];
                for id in &ids {
                    if seen.contains(id) {
                        continue;
                    }
                    match db.message(id).await? {
                        Some(x) if x.conv_id == conv && x.created_seq >= m.visible_from => {
                            seen.push(id.clone());
                        }
                        _ => {}
                    }
                }
                let reactions = db
                    .reaction_summary(&seen, &m.user_id, SUMMARY_PEOPLE)
                    .await?;
                let mut out = serde_json::Map::new();
                for id in &seen {
                    let thread = match db.thread(&conv, id).await? {
                        Some(t) if t.replies > 0 => {
                            let root = db.message(id).await?;
                            Some(thread_view(db, &conv, &m, &t, root.as_ref()).await?)
                        }
                        _ => None,
                    };
                    let pin = db
                        .pin_of(&conv, id)
                        .await?
                        .map(|(by, at, seq)| json!({"by": by, "at": at, "seq": seq}));
                    out.insert(
                        id.clone(),
                        json!({
                            "thread": thread,
                            "reactions": reactions.get(id).cloned().unwrap_or_default(),
                            "pinned": pin,
                        }),
                    );
                }
                Ok(json!({"conv_id": conv, "messages": out}))
            })
        })
        .await
    }
}
