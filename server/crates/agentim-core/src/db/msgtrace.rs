//! What the database knows about one message, for its trace (docs/14 §4.3).
//! Metadata only: no content, no whisper bodies.

use serde_json::Value;
use sqlx::Row;

use super::Db;
use crate::model::*;

/// The facts of one message from its conversation's database.
pub struct MsgFacts {
    pub msg: MsgView,
    pub trace_id: Option<String>,
    /// The message's events: (seq, type, actor, at).
    pub events: Vec<(i64, String, Option<String>, i64)>,
    pub members: Vec<Member>,
    /// Removed members' visible stretches: (user, from, until).
    pub past: Vec<(String, i64, i64)>,
    /// Device positions in the conversation: (device, user, acked_seq, at).
    pub cursors: Vec<(String, String, i64, i64)>,
    /// Messages caused by it: (msg_id, sender, seq, state, at).
    pub replies: Vec<(String, String, i64, String, i64)>,
    /// Drafts caused by it: (draft_id, agent, state, at).
    pub drafts: Vec<(String, String, String, i64)>,
    /// Outbox rows of its events: (row id, seq).
    pub outbox: Vec<(i64, i64)>,
    /// Each webhook's position: (name, cursor).
    pub hook_cursors: Vec<(String, i64)>,
    /// Rows queued before each of its rows a webhook has not passed yet:
    /// (webhook, row id, rows ahead).
    pub ahead: Vec<(String, i64, i64)>,
    /// Webhook events given up on: (webhook, seq).
    pub dropped: Vec<(String, i64)>,
    pub client_msg_id: Option<String>,
    /// Side-channel events anchored at its seq.
    pub side: i64,
    /// Files it refers to: (file_id, kind, state, size).
    pub files: Vec<(String, String, String, i64)>,
}

impl Db<'_> {
    /// None: no such message in this database.
    pub async fn msg_facts(
        &mut self,
        msg_id: &str,
        hooks: &[String],
    ) -> ApiResult<Option<MsgFacts>> {
        let Some(msg) = self.message(msg_id).await? else {
            return Ok(None);
        };
        let conv = msg.conv_id.clone();
        let trace_id = self.msg_trace(msg_id).await?.and_then(|t| t.1);
        let rows = sqlx::query(self.d.q(
            "SELECT seq, type, actor_id, created_at FROM events WHERE conv_id = ? AND msg_id = ? ORDER BY seq",
        ))
        .bind(&conv)
        .bind(msg_id)
        .fetch_all(&mut *self.c)
        .await?;
        let events = rows
            .iter()
            .map(|r| Ok((r.try_get(0)?, r.try_get(1)?, r.try_get(2)?, r.try_get(3)?)))
            .collect::<sqlx::Result<Vec<_>>>()?;
        let members = self.members(&conv).await?;
        let rows = sqlx::query(
            self.d
                .q("SELECT user_id, visible_from, until_seq FROM past_members WHERE conv_id = ?"),
        )
        .bind(&conv)
        .fetch_all(&mut *self.c)
        .await?;
        let past = rows
            .iter()
            .map(|r| Ok((r.try_get(0)?, r.try_get(1)?, r.try_get(2)?)))
            .collect::<sqlx::Result<Vec<_>>>()?;
        let rows = sqlx::query(self.d.q(
            "SELECT device_id, user_id, acked_seq, updated_at FROM device_cursors WHERE conv_id = ?",
        ))
        .bind(&conv)
        .fetch_all(&mut *self.c)
        .await?;
        let cursors = rows
            .iter()
            .map(|r| Ok((r.try_get(0)?, r.try_get(1)?, r.try_get(2)?, r.try_get(3)?)))
            .collect::<sqlx::Result<Vec<_>>>()?;
        let rows = sqlx::query(self.d.q(
            "SELECT msg_id, sender_id, created_seq, state, created_at FROM messages WHERE conv_id = ? AND causation_id = ? ORDER BY created_seq",
        ))
        .bind(&conv)
        .bind(msg_id)
        .fetch_all(&mut *self.c)
        .await?;
        let replies = rows
            .iter()
            .map(|r| {
                Ok((
                    r.try_get(0)?,
                    r.try_get(1)?,
                    r.try_get(2)?,
                    r.try_get(3)?,
                    r.try_get(4)?,
                ))
            })
            .collect::<sqlx::Result<Vec<_>>>()?;
        let rows = sqlx::query(self.d.q(
            "SELECT draft_id, agent_id, state, created_at FROM drafts WHERE conv_id = ? AND causation_id = ? ORDER BY created_at",
        ))
        .bind(&conv)
        .bind(msg_id)
        .fetch_all(&mut *self.c)
        .await?;
        let drafts = rows
            .iter()
            .map(|r| Ok((r.try_get(0)?, r.try_get(1)?, r.try_get(2)?, r.try_get(3)?)))
            .collect::<sqlx::Result<Vec<_>>>()?;
        // The outbox holds the rows not every webhook has handled yet.
        let seqs: Vec<i64> = events.iter().map(|e| e.0).collect();
        let rows = sqlx::query(
            self.d
                .q("SELECT id, body FROM webhook_outbox WHERE conv_id = ? ORDER BY id"),
        )
        .bind(&conv)
        .fetch_all(&mut *self.c)
        .await?;
        self.preload(&rows, &["body"]).await?;
        let mut outbox = vec![];
        for r in &rows {
            let id: i64 = r.try_get(0)?;
            let body = super::dec(self.cx, r, "body", &format!("webhook_outbox.body:{id}"))?;
            let seq = serde_json::from_str::<Value>(&body)
                .ok()
                .and_then(|b| b["data"]["seq"].as_i64());
            if let Some(seq) = seq.filter(|s| seqs.contains(s)) {
                outbox.push((id, seq));
            }
        }
        let mut hook_cursors = vec![];
        for h in hooks {
            let at = self
                .meta_i64(&crate::webhook::cursor_key(h))
                .await?
                .unwrap_or(0);
            hook_cursors.push((h.clone(), at));
        }
        let mut ahead = vec![];
        for (h, at) in &hook_cursors {
            for (id, _) in outbox.iter().filter(|o| o.0 > *at) {
                let n: i64 = sqlx::query(
                    self.d
                        .q("SELECT COUNT(*) FROM webhook_outbox WHERE id > ? AND id < ?"),
                )
                .bind(at)
                .bind(id)
                .fetch_one(&mut *self.c)
                .await?
                .try_get(0)?;
                ahead.push((h.clone(), *id, n));
            }
        }
        let rows = sqlx::query(
            self.d
                .q("SELECT id, detail FROM audit WHERE action = 'webhook.dropped' AND conv_id = ?"),
        )
        .bind(&conv)
        .fetch_all(&mut *self.c)
        .await?;
        self.preload(&rows, &["detail"]).await?;
        let mut dropped = vec![];
        for r in &rows {
            let id: String = r.try_get(0)?;
            let d = super::dec(self.cx, r, "detail", &format!("audit.detail:{id}"))?;
            let d: Value = serde_json::from_str(&d).unwrap_or(Value::Null);
            if let (Some(w), Some(s)) = (d["webhook"].as_str(), d["seq"].as_i64())
                && seqs.contains(&s)
            {
                dropped.push((w.to_string(), s));
            }
        }
        // The dedup record (kept for the dedup window) names the sender's id.
        let rows = sqlx::query(self.d.q(
            "SELECT client_msg_id, result FROM dedup WHERE conv_id = ? AND sender_id = ? AND result LIKE ? ESCAPE '!'",
        ))
        .bind(&conv)
        .bind(&msg.sender_id)
        .bind(format!("%{}%", super::like_escape(msg_id)))
        .fetch_all(&mut *self.c)
        .await?;
        let mut client_msg_id = None;
        for r in &rows {
            let res: String = r.try_get(1)?;
            if serde_json::from_str::<Value>(&res).is_ok_and(|v| v["msg_id"] == msg_id) {
                client_msg_id = Some(r.try_get(0)?);
            }
        }
        let side: i64 = sqlx::query(
            self.d
                .q("SELECT COUNT(*) FROM side_events WHERE conv_id = ? AND anchor_seq = ?"),
        )
        .bind(&conv)
        .bind(msg.created_seq)
        .fetch_one(&mut *self.c)
        .await?
        .try_get(0)?;
        let rows =
            sqlx::query(self.d.q(
                "SELECT file_id, kind, state, size FROM files WHERE ref_id = ? ORDER BY file_id",
            ))
            .bind(msg_id)
            .fetch_all(&mut *self.c)
            .await?;
        let files = rows
            .iter()
            .map(|r| Ok((r.try_get(0)?, r.try_get(1)?, r.try_get(2)?, r.try_get(3)?)))
            .collect::<sqlx::Result<Vec<_>>>()?;
        Ok(Some(MsgFacts {
            msg,
            trace_id,
            events,
            members,
            past,
            cursors,
            replies,
            drafts,
            outbox,
            hook_cursors,
            ahead,
            dropped,
            client_msg_id,
            side,
            files,
        }))
    }

    /// Reports about a message: (id, category, state) (control database).
    pub async fn msg_reports(&mut self, msg_id: &str) -> ApiResult<Vec<(String, String, String)>> {
        let rows = sqlx::query(self.d.q(
            "SELECT id, category, state FROM reports WHERE target_kind = 'message' AND target_id = ? ORDER BY id",
        ))
        .bind(msg_id)
        .fetch_all(&mut *self.c)
        .await?;
        rows.iter()
            .map(|r| Ok((r.try_get(0)?, r.try_get(1)?, r.try_get(2)?)))
            .collect::<sqlx::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    /// Devices of these users (control database).
    pub async fn devices_of(&mut self, users: &[String]) -> ApiResult<Vec<super::TokenRow>> {
        let mut out = vec![];
        for u in users {
            out.extend(self.devices(u).await?);
        }
        Ok(out)
    }
}
