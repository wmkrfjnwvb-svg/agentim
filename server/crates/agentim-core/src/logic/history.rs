//! The agent history service, write side (docs/03-agents.md §7.1): an agent keeps
//! its own summaries of what it has seen; `context` starts from them.

use serde_json::{Value, json};

use super::Ctx;
use crate::model::*;

/// Summaries kept per agent per conversation.
const KEEP: usize = 20;
const TEXT_MAX: usize = 64 * 1024;

impl Ctx<'_, '_> {
    pub(super) async fn put_summary(
        &mut self,
        me: &User,
        conv_id: &str,
        from_seq: i64,
        to_seq: i64,
        text: &str,
    ) -> ApiResult<Value> {
        if me.kind != UserKind::Agent {
            return Err(rule!(
                "history.summary_agents_only",
                "forbidden",
                "summaries are for agents"
            ));
        }
        self.own_conv(conv_id)?;
        let mine = self.require_member(conv_id, &me.id).await?;
        if text.is_empty() || text.len() > TEXT_MAX || text.contains('\0') {
            return Err(rule!(
                "history.summary_text",
                "bad_request",
                "text must be 1 byte..64KB"
            ));
        }
        let last = self
            .db
            .conv(conv_id)
            .await?
            .map(|c| c.last_seq)
            .unwrap_or(0);
        if from_seq < 1 || from_seq > to_seq || to_seq > last {
            return Err(rule!(
                "history.summary_range",
                "bad_request",
                format!("need 1 <= from_seq <= to_seq <= {last}"),
            ));
        }
        if from_seq < mine.visible_from {
            return Err(rule!(
                "history.summary_not_visible",
                "forbidden",
                format!(
                    "you can see this conversation from seq {}",
                    mine.visible_from
                ),
            ));
        }
        let id = self
            .db
            .put_summary(conv_id, &me.id, from_seq, to_seq, text, KEEP)
            .await?;
        self.db
            .audit(
                Some(&me.id),
                "summary.written",
                Some(conv_id),
                Some(&id),
                json!({"from_seq": from_seq, "to_seq": to_seq, "bytes": text.len()}),
            )
            .await?;
        Ok(json!({"summary_id": id, "from_seq": from_seq, "to_seq": to_seq}))
    }
}
