//! People paging back through a conversation (docs/08 §2.8, D108): a page
//! of messages in their current state around an anchor. It does not move
//! the device's sync position and is not audited (members reading their
//! own conversations).

use serde_json::{Value, json};

use super::{Hub, err};
use crate::model::*;

const DEFAULT_BEFORE: i64 = 50;

/// Where a page is anchored.
enum Anchor {
    Newest,
    FirstUnread,
    Seq(i64),
}

fn anchor_of(v: Option<&Value>) -> ApiResult<Anchor> {
    match v {
        None => Ok(Anchor::Newest),
        Some(Value::String(s)) if s == "newest" => Ok(Anchor::Newest),
        Some(Value::String(s)) if s == "first_unread" => Ok(Anchor::FirstUnread),
        Some(v) => match v.as_i64() {
            Some(n) if n >= 0 => Ok(Anchor::Seq(n)),
            _ => Err(err(
                "bad_request",
                "anchor is a seq (>= 0), \"newest\" or \"first_unread\"",
            )),
        },
    }
}

impl Hub {
    pub(super) async fn page(
        &self,
        me: &User,
        conv_id: &str,
        anchor: Option<&Value>,
        before: Option<i64>,
        after: Option<i64>,
    ) -> ApiResult<Value> {
        let cfg = &self.inner.cfg.history;
        let anchor = anchor_of(anchor)?;
        let (before, after) = (before.unwrap_or(DEFAULT_BEFORE), after.unwrap_or(0));
        if !(0..=cfg.page_max).contains(&before) || !(0..=cfg.page_max).contains(&after) {
            return Err(err(
                "bad_request",
                format!("before and after are 0..{}", cfg.page_max),
            ));
        }
        if before == 0 && after == 0 {
            return Err(err(
                "bad_request",
                "ask for messages before or after the anchor",
            ));
        }
        let m = self.reader_member(me, conv_id, None).await?;
        self.user_bucket(&me.id, "history", cfg.rate_per_second, cfg.burst)?;
        if !cfg.enabled {
            return Ok(json!({
                "conv_id": conv_id, "messages": [], "has_before": false,
                "has_after": false, "before_end": "disabled",
            }));
        }
        let shard = self.conv_shard(conv_id)?;
        let (conv, user, vf) = (conv_id.to_string(), me.id.clone(), m.visible_from);
        let (down, up, past, first) = self
            .read(shard, move |db| {
                Box::pin(async move {
                    let past = db.past_member(&conv, &user).await?;
                    let pivot = match anchor {
                        Anchor::Newest => i64::MAX,
                        Anchor::FirstUnread => db.read_seq(&conv, &user).await?,
                        Anchor::Seq(n) => n,
                    };
                    let down = if before > 0 {
                        db.messages_page(&conv, vf, past, pivot, true, before + 1)
                            .await?
                    } else {
                        // Only whether there is anything before.
                        db.messages_page(&conv, vf, past, pivot, true, 1).await?
                    };
                    let up = if pivot == i64::MAX {
                        vec![]
                    } else {
                        db.messages_page(&conv, vf, past, pivot, false, after + 1)
                            .await?
                    };
                    Ok((down, up, past, db.first_seq(&conv).await?))
                })
            })
            .await?;
        let has_before = down.len() as i64 > before;
        let has_after = up.len() as i64 > after;
        let mut messages: Vec<MsgView> = down.into_iter().take(before as usize).collect();
        messages.reverse();
        messages.extend(up.into_iter().take(after as usize));
        let mut v = json!({
            "conv_id": conv_id,
            "messages": messages,
            "has_before": has_before,
            "has_after": has_after,
        });
        if !has_before {
            // Why nothing comes before: the reader's first visible seq,
            // the oldest one still stored, or the start of the conversation.
            let lowest = past.map_or(vf, |(from, _)| from.min(vf));
            v["before_end"] = json!(match first {
                Some(f) if f > lowest => "retention",
                _ if lowest > 1 => "visible_from",
                _ => "start",
            });
        }
        Ok(v)
    }
}
