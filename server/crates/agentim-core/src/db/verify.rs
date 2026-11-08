//! What `agentim verify` reads (docs/en/design/tracing.md): one conversation's log and
//! state, read-only.

use serde_json::Value;
use sqlx::Row;

use super::Db;
use crate::model::*;

/// A message row as stored.
pub struct MsgRow {
    pub msg_id: String,
    pub created_seq: i64,
    pub sender_id: String,
    pub state: String,
    pub version: i64,
    pub updated_at: i64,
}

/// One conversation as stored.
pub struct ConvData {
    pub kind: String,
    pub last_seq: i64,
    pub events: Vec<Event>,
    pub members: Vec<Member>,
    pub messages: Vec<MsgRow>,
    /// (file_id, uploader, ref_id, state)
    pub files: Vec<(String, String, Option<String>, String)>,
    /// (sender, client_msg_id, result)
    pub dedup: Vec<(String, String, Value)>,
    /// (side_seq, anchor_seq, audience)
    pub side: Vec<(i64, i64, Vec<String>)>,
}

impl Db<'_> {
    /// Conversations of this database, those with an event since `since`
    /// (ms) when given.
    pub async fn verify_convs(&mut self, since: Option<i64>) -> ApiResult<Vec<String>> {
        let rows =
            match since {
                None => {
                    sqlx::query(self.d.q("SELECT id FROM conversations ORDER BY id"))
                        .fetch_all(&mut *self.c)
                        .await?
                }
                Some(t) => sqlx::query(self.d.q(
                    "SELECT DISTINCT conv_id FROM events WHERE created_at >= ? ORDER BY conv_id",
                ))
                .bind(t)
                .fetch_all(&mut *self.c)
                .await?,
            };
        rows.iter()
            .map(|r| r.try_get(0))
            .collect::<sqlx::Result<_>>()
            .map_err(Into::into)
    }

    /// Conversation ids that rows refer to but that no longer exist (a
    /// dissolution or a move deletes all of a conversation's rows).
    pub async fn verify_orphans(&mut self) -> ApiResult<Vec<(String, String)>> {
        let mut out = vec![];
        for t in ["events", "messages", "members"] {
            let rows = sqlx::query(self.d.q(&format!(
                "SELECT DISTINCT conv_id FROM {t} WHERE conv_id NOT IN (SELECT id FROM conversations)"
            )))
            .fetch_all(&mut *self.c)
            .await?;
            for r in rows {
                out.push((t.to_string(), r.try_get(0)?));
            }
        }
        Ok(out)
    }

    pub async fn verify_data(&mut self, conv_id: &str) -> ApiResult<Option<ConvData>> {
        let Some(conv) = self.conv(conv_id).await? else {
            return Ok(None);
        };
        let mut events = vec![];
        loop {
            let after = events.last().map_or(0, |e: &Event| e.seq);
            let page = self.pull(conv_id, after, 1000).await?;
            let done = page.len() < 1000;
            events.extend(page);
            if done {
                break;
            }
        }
        let members = self.members(conv_id).await?;
        let rows = sqlx::query(self.d.q(
            "SELECT msg_id, created_seq, sender_id, state, version, updated_at FROM messages WHERE conv_id = ? ORDER BY created_seq",
        ))
        .bind(conv_id)
        .fetch_all(&mut *self.c)
        .await?;
        let messages = rows
            .iter()
            .map(|r| {
                Ok(MsgRow {
                    msg_id: r.try_get(0)?,
                    created_seq: r.try_get(1)?,
                    sender_id: r.try_get(2)?,
                    state: r.try_get(3)?,
                    version: r.try_get(4)?,
                    updated_at: r.try_get(5)?,
                })
            })
            .collect::<sqlx::Result<Vec<_>>>()?;
        let rows = sqlx::query(self.d.q(
            "SELECT file_id, uploader_id, ref_id, state FROM files WHERE conv_id = ? ORDER BY file_id",
        ))
        .bind(conv_id)
        .fetch_all(&mut *self.c)
        .await?;
        let files = rows
            .iter()
            .map(|r| Ok((r.try_get(0)?, r.try_get(1)?, r.try_get(2)?, r.try_get(3)?)))
            .collect::<sqlx::Result<Vec<_>>>()?;
        let rows = sqlx::query(
            self.d
                .q("SELECT sender_id, client_msg_id, result FROM dedup WHERE conv_id = ?"),
        )
        .bind(conv_id)
        .fetch_all(&mut *self.c)
        .await?;
        let dedup = rows
            .iter()
            .map(|r| {
                let res: String = r.try_get(2)?;
                Ok((
                    r.try_get(0)?,
                    r.try_get(1)?,
                    serde_json::from_str(&res).unwrap_or(Value::Null),
                ))
            })
            .collect::<sqlx::Result<Vec<_>>>()?;
        let rows = sqlx::query(self.d.q(
            "SELECT side_seq, anchor_seq, audience FROM side_events WHERE conv_id = ? ORDER BY side_seq",
        ))
        .bind(conv_id)
        .fetch_all(&mut *self.c)
        .await?;
        let side = rows
            .iter()
            .map(|r| {
                let a: String = r.try_get(2)?;
                Ok((
                    r.try_get(0)?,
                    r.try_get(1)?,
                    serde_json::from_str(&a).unwrap_or_default(),
                ))
            })
            .collect::<sqlx::Result<Vec<_>>>()?;
        Ok(Some(ConvData {
            kind: conv.kind,
            last_seq: conv.last_seq,
            events,
            members,
            messages,
            files,
            dedup,
            side,
        }))
    }
}
