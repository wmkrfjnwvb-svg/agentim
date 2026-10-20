//! Account deletion, export and reports (docs/04-security-compliance.md §4.2).

use serde_json::{Value, json};
use sqlx::Row;

use super::{Db, MSG_COLS, dec_opt, new_id, row_msg};
use crate::model::*;

/// Messages erased per writer command (keeps each transaction short).
pub const ERASE_BATCH: i64 = 200;

impl Db<'_> {
    // ------------------------------------------------------------ erasure

    /// Marks an account deleted and records the erasure; repeatable. Returns
    /// the erasure row (also when it already existed).
    pub async fn erasure_start(&mut self, user_id: &str, by: Option<&str>) -> ApiResult<Value> {
        if let Some(e) = self.erasure(user_id).await? {
            return Ok(e);
        }
        let now = now_ms();
        sqlx::query(self.d.q(
            "UPDATE users SET erased_at = ?, deactivated_at = COALESCE(deactivated_at, ?), suspended_until = NULL WHERE id = ?",
        ))
        .bind(now)
        .bind(now)
        .bind(user_id)
        .execute(&mut *self.c)
        .await?;
        sqlx::query(self.d.q(
            "INSERT INTO erasures (user_id, by_id, state, requested_at, finished_at, convs_done) VALUES (?, ?, 'running', ?, NULL, 0)",
        ))
        .bind(user_id)
        .bind(by)
        .bind(now)
        .execute(&mut *self.c)
        .await?;
        Ok(self.erasure(user_id).await?.unwrap_or_default())
    }

    pub async fn erasure(&mut self, user_id: &str) -> ApiResult<Option<Value>> {
        let r = sqlx::query(self.d.q(
            "SELECT state, requested_at, finished_at, convs_done FROM erasures WHERE user_id = ?",
        ))
        .bind(user_id)
        .fetch_optional(&mut *self.c)
        .await?;
        r.map(|r| -> ApiResult<Value> {
            Ok(json!({
                "user_id": user_id,
                "state": r.try_get::<String, _>(0)?,
                "requested_at": r.try_get::<i64, _>(1)?,
                "finished_at": r.try_get::<Option<i64>, _>(2)?,
                "conversations_done": r.try_get::<i64, _>(3)?,
            }))
        })
        .transpose()
    }

    /// Erasures a crash interrupted.
    pub async fn erasures_running(&mut self) -> ApiResult<Vec<(String, Option<String>)>> {
        let rows = sqlx::query(self.d.q(
            "SELECT user_id, by_id FROM erasures WHERE state = 'running' ORDER BY requested_at",
        ))
        .fetch_all(&mut *self.c)
        .await?;
        rows.iter()
            .map(|r| Ok((r.try_get(0)?, r.try_get(1)?)))
            .collect()
    }

    pub async fn erasure_progress(&mut self, user_id: &str) -> ApiResult<()> {
        sqlx::query(
            self.d
                .q("UPDATE erasures SET convs_done = convs_done + 1 WHERE user_id = ?"),
        )
        .bind(user_id)
        .execute(&mut *self.c)
        .await?;
        Ok(())
    }

    /// The control database's part: profile, secrets, devices, settings,
    /// blocks. The user row stays (ids are never reused).
    pub async fn erasure_finish(&mut self, user_id: &str) -> ApiResult<()> {
        sqlx::query(self.d.q(
            "UPDATE users SET display_name = NULL, avatar = NULL, fields = NULL, prefs = NULL WHERE id = ?",
        ))
        .bind(user_id)
        .execute(&mut *self.c)
        .await?;
        for q in [
            "DELETE FROM credentials WHERE user_id = ?",
            "DELETE FROM tokens WHERE user_id = ?",
            "DELETE FROM device_cursors WHERE user_id = ?",
            "DELETE FROM departures WHERE user_id = ?",
            "DELETE FROM member_index WHERE user_id = ?",
        ] {
            sqlx::query(self.d.q(q))
                .bind(user_id)
                .execute(&mut *self.c)
                .await?;
        }
        sqlx::query(
            self.d
                .q("DELETE FROM blocks WHERE user_id = ? OR blocked_id = ?"),
        )
        .bind(user_id)
        .bind(user_id)
        .execute(&mut *self.c)
        .await?;
        sqlx::query(
            self.d
                .q("UPDATE erasures SET state = 'done', finished_at = ? WHERE user_id = ?"),
        )
        .bind(now_ms())
        .bind(user_id)
        .execute(&mut *self.c)
        .await?;
        Ok(())
    }

    pub async fn is_erased(&mut self, user_id: &str) -> ApiResult<bool> {
        let r = sqlx::query(self.d.q("SELECT erased_at FROM users WHERE id = ?"))
            .bind(user_id)
            .fetch_optional(&mut *self.c)
            .await?;
        Ok(match r {
            Some(r) => r.try_get::<Option<i64>, _>(0)?.is_some(),
            None => false,
        })
    }

    /// Clears up to `ERASE_BATCH` of a sender's messages in a conversation,
    /// as a recall does (content, payload, envelope, the content earlier
    /// events embed, undelivered webhook bodies). Returns how many.
    pub async fn erase_sender_batch(&mut self, conv_id: &str, user_id: &str) -> ApiResult<i64> {
        let rows = sqlx::query(self.d.q(
            "SELECT msg_id, created_seq FROM messages WHERE conv_id = ? AND sender_id = ? AND (state <> 'recalled' OR state_reason IS NULL OR state_reason <> 'account_deleted') ORDER BY created_seq LIMIT ?",
        ))
        .bind(conv_id)
        .bind(user_id)
        .bind(ERASE_BATCH)
        .fetch_all(&mut *self.c)
        .await?;
        let now = now_ms();
        for r in &rows {
            let msg_id: String = r.try_get(0)?;
            let seq: i64 = r.try_get(1)?;
            sqlx::query(self.d.q(
                "UPDATE messages SET content = '', payload = NULL, envelope = NULL, mentions = '[]', state = 'recalled', state_reason = 'account_deleted', updated_at = ? WHERE msg_id = ?",
            ))
            .bind(now)
            .bind(&msg_id)
            .execute(&mut *self.c)
            .await?;
            self.scrub_message(conv_id, &msg_id, seq).await?;
            self.forget_outbox(conv_id, Some(&msg_id)).await?;
        }
        Ok(rows.len() as i64)
    }

    /// What the user left in a conversation besides messages: feedback,
    /// card answers, whispers and other side events they wrote.
    pub async fn erase_user_extras(&mut self, conv_id: &str, user_id: &str) -> ApiResult<()> {
        for q in [
            "DELETE FROM feedback WHERE conv_id = ? AND user_id = ?",
            "DELETE FROM card_responses WHERE conv_id = ? AND user_id = ?",
        ] {
            sqlx::query(self.d.q(q))
                .bind(conv_id)
                .bind(user_id)
                .execute(&mut *self.c)
                .await?;
        }
        let rows = sqlx::query(
            self.d
                .q("SELECT side_seq FROM side_events WHERE conv_id = ? AND actor_id = ?"),
        )
        .bind(conv_id)
        .bind(user_id)
        .fetch_all(&mut *self.c)
        .await?;
        for r in rows {
            let seq: i64 = r.try_get(0)?;
            let sealed = self
                .seal(
                    conv_id,
                    &format!("side_events.payload:{conv_id}:{seq}"),
                    r#"{"erased":true}"#,
                )
                .await?;
            sqlx::query(
                self.d
                    .q("UPDATE side_events SET payload = ? WHERE conv_id = ? AND side_seq = ?"),
            )
            .bind(sealed)
            .bind(conv_id)
            .bind(seq)
            .execute(&mut *self.c)
            .await?;
        }
        Ok(())
    }

    /// Conversations a user ever took part in that this database knows:
    /// current memberships and departures (they may have left messages).
    pub async fn convs_touched(&mut self, user_id: &str) -> ApiResult<Vec<String>> {
        let rows = sqlx::query(self.d.q(
            "SELECT conv_id FROM member_index WHERE user_id = ? UNION SELECT conv_id FROM departures WHERE user_id = ?",
        ))
        .bind(user_id)
        .bind(user_id)
        .fetch_all(&mut *self.c)
        .await?;
        let mut out: Vec<String> = rows
            .iter()
            .map(|r| r.try_get(0))
            .collect::<Result<_, _>>()?;
        out.sort();
        out.dedup();
        Ok(out)
    }

    // ------------------------------------------------------------- export

    /// A page of a sender's messages in a conversation, oldest first.
    pub async fn messages_by_sender(
        &mut self,
        conv_id: &str,
        user_id: &str,
        after_seq: i64,
        limit: i64,
    ) -> ApiResult<Vec<MsgView>> {
        let mut sql = format!(
            "SELECT {MSG_COLS} FROM messages WHERE conv_id = ? AND sender_id = ? AND created_seq > ?"
        );
        if self.cap {
            sql.push_str(
                " AND created_seq <= (SELECT confirmed_seq FROM conversations WHERE id = ?)",
            );
        }
        sql.push_str(" ORDER BY created_seq LIMIT ?");
        let mut q = sqlx::query(self.d.q(&sql))
            .bind(conv_id)
            .bind(user_id)
            .bind(after_seq);
        if self.cap {
            q = q.bind(conv_id);
        }
        let rows = q.bind(limit).fetch_all(&mut *self.c).await?;
        self.preload(&rows, &["content", "payload"]).await?;
        rows.iter()
            .map(|r| row_msg(r, self.cx).map_err(Into::into))
            .collect()
    }

    /// Feedback and card answers a user gave in a conversation.
    pub async fn user_answers(&mut self, conv_id: &str, user_id: &str) -> ApiResult<Value> {
        let fb = sqlx::query(self.d.q(
            "SELECT msg_id, rating, reason, created_at FROM feedback WHERE conv_id = ? AND user_id = ? ORDER BY created_at",
        ))
        .bind(conv_id)
        .bind(user_id)
        .fetch_all(&mut *self.c)
        .await?;
        self.preload(&fb, &["reason"]).await?;
        let mut feedback = vec![];
        for r in &fb {
            let msg_id: String = r.try_get(0)?;
            feedback.push(json!({
                "msg_id": msg_id,
                "rating": r.try_get::<String, _>(1)?,
                "reason": dec_opt(self.cx, r, "reason", &format!("feedback.reason:{msg_id}:{user_id}"))?,
                "created_at": r.try_get::<i64, _>(3)?,
            }));
        }
        let cards = sqlx::query(self.d.q(
            "SELECT msg_id, created_at FROM card_responses WHERE conv_id = ? AND user_id = ? ORDER BY created_at",
        ))
        .bind(conv_id)
        .bind(user_id)
        .fetch_all(&mut *self.c)
        .await?;
        let cards: Vec<Value> = cards
            .iter()
            .map(|r| -> ApiResult<Value> {
                Ok(json!({"msg_id": r.try_get::<String, _>(0)?, "created_at": r.try_get::<i64, _>(1)?}))
            })
            .collect::<ApiResult<_>>()?;
        Ok(json!({"feedback": feedback, "card_responses": cards}))
    }

    /// Profile, devices and blocks of a user, for the export.
    pub async fn user_export_head(&mut self, user_id: &str) -> ApiResult<Value> {
        let devices = sqlx::query(self.d.q(
            "SELECT device_id, device_name, platform, created_at, last_seen FROM tokens WHERE user_id = ? ORDER BY created_at",
        ))
        .bind(user_id)
        .fetch_all(&mut *self.c)
        .await?;
        let devices: Vec<Value> = devices
            .iter()
            .map(|r| -> ApiResult<Value> {
                Ok(json!({
                    "device_id": r.try_get::<String, _>(0)?,
                    "name": r.try_get::<Option<String>, _>(1)?,
                    "platform": r.try_get::<Option<String>, _>(2)?,
                    "created_at": r.try_get::<i64, _>(3)?,
                    "last_seen": r.try_get::<Option<i64>, _>(4)?,
                }))
            })
            .collect::<ApiResult<_>>()?;
        let blocks =
            sqlx::query(self.d.q(
                "SELECT blocked_id, created_at FROM blocks WHERE user_id = ? ORDER BY created_at",
            ))
            .bind(user_id)
            .fetch_all(&mut *self.c)
            .await?;
        let blocks: Vec<Value> = blocks
            .iter()
            .map(|r| -> ApiResult<Value> {
                Ok(json!({"user_id": r.try_get::<String, _>(0)?, "since": r.try_get::<i64, _>(1)?}))
            })
            .collect::<ApiResult<_>>()?;
        let agents = sqlx::query(self.d.q(
            "SELECT id, name, created_at FROM users WHERE owner_id = ? AND destroyed_at IS NULL ORDER BY created_at",
        ))
        .bind(user_id)
        .fetch_all(&mut *self.c)
        .await?;
        let agents: Vec<Value> = agents
            .iter()
            .map(|r| -> ApiResult<Value> {
                Ok(json!({"id": r.try_get::<String, _>(0)?, "name": r.try_get::<String, _>(1)?, "created_at": r.try_get::<i64, _>(2)?}))
            })
            .collect::<ApiResult<_>>()?;
        Ok(json!({"devices": devices, "blocks": blocks, "agents": agents}))
    }

    // ------------------------------------------------------------ reports

    /// An open report by this reporter on this target, if any.
    pub async fn open_report_by(
        &mut self,
        reporter: &str,
        kind: &str,
        target: &str,
    ) -> ApiResult<Option<String>> {
        let r = sqlx::query(self.d.q(
            "SELECT id FROM reports WHERE target_kind = ? AND target_id = ? AND state = 'open' AND reporter_id = ?",
        ))
        .bind(kind)
        .bind(target)
        .bind(reporter)
        .fetch_optional(&mut *self.c)
        .await?;
        Ok(r.map(|r| r.try_get(0)).transpose()?)
    }

    pub async fn reports_since(&mut self, reporter: &str, since: i64) -> ApiResult<i64> {
        let r = sqlx::query(
            self.d
                .q("SELECT COUNT(*) FROM reports WHERE reporter_id = ? AND created_at >= ?"),
        )
        .bind(reporter)
        .bind(since)
        .fetch_one(&mut *self.c)
        .await?;
        Ok(r.try_get(0)?)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn report_put(
        &mut self,
        reporter: &str,
        kind: &str,
        target: &str,
        conv_id: Option<&str>,
        category: &str,
        severity: i64,
        body: &Value,
        due_at: i64,
    ) -> ApiResult<String> {
        let id = new_id();
        let sealed = self
            .seal("", &format!("reports.body:{id}"), &body.to_string())
            .await?;
        sqlx::query(self.d.q(
            "INSERT INTO reports (id, reporter_id, target_kind, target_id, conv_id, category, severity, body, state, created_at, due_at, resolved_at, decision) VALUES (?, ?, ?, ?, ?, ?, ?, ?, 'open', ?, ?, NULL, NULL)",
        ))
        .bind(&id)
        .bind(reporter)
        .bind(kind)
        .bind(target)
        .bind(conv_id)
        .bind(category)
        .bind(severity)
        .bind(sealed)
        .bind(now_ms())
        .bind(due_at)
        .execute(&mut *self.c)
        .await?;
        Ok(id)
    }

    const REPORT_COLS: &'static str = "id, reporter_id, target_kind, target_id, conv_id, category, severity, body, state, created_at, due_at, resolved_at, decision";

    fn report_row(&self, r: &sqlx::any::AnyRow) -> ApiResult<Value> {
        let id: String = r.try_get("id")?;
        let body = dec_opt(self.cx, r, "body", &format!("reports.body:{id}"))?
            .and_then(|s| serde_json::from_str::<Value>(&s).ok())
            .unwrap_or(Value::Null);
        let decision = r
            .try_get::<Option<String>, _>("decision")?
            .and_then(|s| serde_json::from_str::<Value>(&s).ok());
        Ok(json!({
            "id": id,
            "reporter_id": r.try_get::<String, _>("reporter_id")?,
            "target_kind": r.try_get::<String, _>("target_kind")?,
            "target_id": r.try_get::<String, _>("target_id")?,
            "conv_id": r.try_get::<Option<String>, _>("conv_id")?,
            "category": r.try_get::<String, _>("category")?,
            "severity": r.try_get::<i64, _>("severity")?,
            "text": body["text"],
            "evidence": body["evidence"],
            "state": r.try_get::<String, _>("state")?,
            "created_at": r.try_get::<i64, _>("created_at")?,
            "due_at": r.try_get::<i64, _>("due_at")?,
            "resolved_at": r.try_get::<Option<i64>, _>("resolved_at")?,
            "decision": decision,
        }))
    }

    pub async fn report(&mut self, id: &str) -> ApiResult<Option<Value>> {
        let r = sqlx::query(self.d.q(&format!(
            "SELECT {} FROM reports WHERE id = ?",
            Self::REPORT_COLS
        )))
        .bind(id)
        .fetch_optional(&mut *self.c)
        .await?;
        self.preload(r.as_slice(), &["body"]).await?;
        r.as_ref().map(|r| self.report_row(r)).transpose()
    }

    /// Open reports: most severe first, then the earliest deadline; or the
    /// latest resolved. `after` is the last id of the previous page.
    pub async fn reports(&mut self, open: bool, limit: i64, offset: i64) -> ApiResult<Vec<Value>> {
        let sql = if open {
            format!(
                "SELECT {} FROM reports WHERE state = 'open' ORDER BY severity DESC, due_at, id LIMIT ? OFFSET ?",
                Self::REPORT_COLS
            )
        } else {
            format!(
                "SELECT {} FROM reports WHERE state = 'resolved' ORDER BY resolved_at DESC, id LIMIT ? OFFSET ?",
                Self::REPORT_COLS
            )
        };
        let rows = sqlx::query(self.d.q(&sql))
            .bind(limit)
            .bind(offset)
            .fetch_all(&mut *self.c)
            .await?;
        self.preload(&rows, &["body"]).await?;
        rows.iter().map(|r| self.report_row(r)).collect()
    }

    /// Other reports on the same target (for the reviewer).
    pub async fn reports_on(&mut self, kind: &str, target: &str) -> ApiResult<Vec<Value>> {
        let rows = sqlx::query(self.d.q(&format!(
            "SELECT {} FROM reports WHERE target_kind = ? AND target_id = ? ORDER BY created_at",
            Self::REPORT_COLS
        )))
        .bind(kind)
        .bind(target)
        .fetch_all(&mut *self.c)
        .await?;
        self.preload(&rows, &["body"]).await?;
        rows.iter().map(|r| self.report_row(r)).collect()
    }

    /// Resolves every open report on a target with one decision; returns
    /// their ids (empty when none is open any more).
    pub async fn reports_resolve(
        &mut self,
        kind: &str,
        target: &str,
        decision: &Value,
    ) -> ApiResult<Vec<String>> {
        let rows = sqlx::query(self.d.q(
            "SELECT id FROM reports WHERE target_kind = ? AND target_id = ? AND state = 'open' ORDER BY created_at",
        ))
        .bind(kind)
        .bind(target)
        .fetch_all(&mut *self.c)
        .await?;
        let ids: Vec<String> = rows
            .iter()
            .map(|r| r.try_get(0))
            .collect::<Result<_, _>>()?;
        let now = now_ms();
        for id in &ids {
            sqlx::query(self.d.q(
                "UPDATE reports SET state = 'resolved', resolved_at = ?, decision = ? WHERE id = ? AND state = 'open'",
            ))
            .bind(now)
            .bind(decision.to_string())
            .bind(id)
            .execute(&mut *self.c)
            .await?;
        }
        Ok(ids)
    }

    /// Resolved reports older than the keep time go (with their evidence).
    pub async fn reports_purge(&mut self, before: i64) -> ApiResult<u64> {
        let r = sqlx::query(
            self.d
                .q("DELETE FROM reports WHERE state = 'resolved' AND resolved_at < ?"),
        )
        .bind(before)
        .execute(&mut *self.c)
        .await?;
        Ok(r.rows_affected())
    }

    /// A user's reports (for the export).
    pub async fn reports_by(&mut self, reporter: &str) -> ApiResult<Vec<Value>> {
        let rows = sqlx::query(self.d.q(&format!(
            "SELECT {} FROM reports WHERE reporter_id = ? ORDER BY created_at",
            Self::REPORT_COLS
        )))
        .bind(reporter)
        .fetch_all(&mut *self.c)
        .await?;
        self.preload(&rows, &["body"]).await?;
        rows.iter()
            .map(|r| {
                let mut v = self.report_row(r)?;
                // The evidence is about others.
                v.as_object_mut().map(|o| o.remove("evidence"));
                Ok(v)
            })
            .collect()
    }

    // --------------------------------------------------------- suspension

    pub async fn set_suspended(&mut self, user_id: &str, until: Option<i64>) -> ApiResult<()> {
        sqlx::query(self.d.q(
            "UPDATE users SET suspended_until = ?, deactivated_at = ? WHERE id = ? AND erased_at IS NULL",
        ))
        .bind(until)
        .bind(until.map(|_| now_ms()))
        .bind(user_id)
        .execute(&mut *self.c)
        .await?;
        Ok(())
    }

    /// Suspensions that ended: the accounts are active again.
    pub async fn suspensions_ended(&mut self, now: i64) -> ApiResult<Vec<String>> {
        let rows = sqlx::query(self.d.q(
            "SELECT id FROM users WHERE suspended_until IS NOT NULL AND suspended_until <= ? AND erased_at IS NULL",
        ))
        .bind(now)
        .fetch_all(&mut *self.c)
        .await?;
        let ids: Vec<String> = rows
            .iter()
            .map(|r| r.try_get(0))
            .collect::<Result<_, _>>()?;
        for id in &ids {
            sqlx::query(
                self.d.q(
                    "UPDATE users SET suspended_until = NULL, deactivated_at = NULL WHERE id = ?",
                ),
            )
            .bind(id)
            .execute(&mut *self.c)
            .await?;
        }
        Ok(ids)
    }
}
