//! Threads, reactions and pinned messages (docs/en/design/messaging.md,
//! MSG-19 to MSG-21). Every write here runs in the conversation's writer,
//! in the same transaction as the event that announces it.

use serde_json::{Value, json};
use sqlx::Row;

use super::Db;
use crate::model::*;

/// One row of `threads`: a root's replies and the latest of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadRow {
    pub root_id: String,
    pub replies: i64,
    pub last_seq: i64,
    pub last_at: i64,
    pub last_sender: Option<String>,
}

/// A member of a thread: how far they read it, whether they follow it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadMember {
    pub user_id: String,
    pub read_seq: i64,
    pub following: bool,
}

fn row_thread(r: &sqlx::any::AnyRow) -> sqlx::Result<ThreadRow> {
    Ok(ThreadRow {
        root_id: r.try_get("root_id")?,
        replies: r.try_get("replies")?,
        last_seq: r.try_get("last_seq")?,
        last_at: r.try_get("last_at")?,
        last_sender: r.try_get("last_sender")?,
    })
}

impl Db<'_> {
    // ------------------------------------------------------------- threads

    pub async fn thread(&mut self, conv_id: &str, root_id: &str) -> ApiResult<Option<ThreadRow>> {
        let r = sqlx::query(self.d.q(
            "SELECT root_id, replies, last_seq, last_at, last_sender FROM threads WHERE conv_id = ? AND root_id = ?",
        ))
        .bind(conv_id)
        .bind(root_id)
        .fetch_optional(&mut *self.c)
        .await?;
        Ok(r.as_ref().map(row_thread).transpose()?)
    }

    /// A reply was committed: one more, and it is the latest.
    pub async fn thread_replied(
        &mut self,
        conv_id: &str,
        root_id: &str,
        seq: i64,
        at: i64,
        sender: &str,
    ) -> ApiResult<()> {
        let n = sqlx::query(self.d.q(
            "UPDATE threads SET replies = replies + 1, last_seq = ?, last_at = ?, last_sender = ? WHERE conv_id = ? AND root_id = ?",
        ))
        .bind(seq)
        .bind(at)
        .bind(sender)
        .bind(conv_id)
        .bind(root_id)
        .execute(&mut *self.c)
        .await?
        .rows_affected();
        if n == 0 {
            sqlx::query(self.d.q(
                "INSERT INTO threads (conv_id, root_id, replies, last_seq, last_at, last_sender) VALUES (?, ?, 1, ?, ?, ?)",
            ))
            .bind(conv_id)
            .bind(root_id)
            .bind(seq)
            .bind(at)
            .bind(sender)
            .execute(&mut *self.c)
            .await?;
        }
        Ok(())
    }

    /// Counts a thread's replies again from the messages (after a reply
    /// was recalled).
    pub async fn thread_recount(&mut self, conv_id: &str, root_id: &str) -> ApiResult<()> {
        let r = sqlx::query(self.d.q(
            "SELECT COUNT(*), MAX(created_seq) FROM messages WHERE conv_id = ? AND thread_id = ? AND state <> ?",
        ))
        .bind(conv_id)
        .bind(root_id)
        .bind("recalled")
        .fetch_one(&mut *self.c)
        .await?;
        let replies: i64 = r.try_get(0)?;
        let last: Option<i64> = r.try_get(1)?;
        let (at, sender) = match last {
            Some(seq) => {
                let r = sqlx::query(self.d.q(
                    "SELECT created_at, sender_id FROM messages WHERE conv_id = ? AND created_seq = ?",
                ))
                .bind(conv_id)
                .bind(seq)
                .fetch_one(&mut *self.c)
                .await?;
                (r.try_get::<i64, _>(0)?, Some(r.try_get::<String, _>(1)?))
            }
            None => (0, None),
        };
        sqlx::query(self.d.q(
            "UPDATE threads SET replies = ?, last_seq = ?, last_at = ?, last_sender = ? WHERE conv_id = ? AND root_id = ?",
        ))
        .bind(replies)
        .bind(last.unwrap_or(0))
        .bind(at)
        .bind(sender)
        .bind(conv_id)
        .bind(root_id)
        .execute(&mut *self.c)
        .await?;
        Ok(())
    }

    pub async fn thread_member(
        &mut self,
        conv_id: &str,
        root_id: &str,
        user_id: &str,
    ) -> ApiResult<Option<ThreadMember>> {
        let r = sqlx::query(self.d.q(
            "SELECT user_id, read_seq, following FROM thread_members WHERE conv_id = ? AND root_id = ? AND user_id = ?",
        ))
        .bind(conv_id)
        .bind(root_id)
        .bind(user_id)
        .fetch_optional(&mut *self.c)
        .await?;
        Ok(r.map(|r| -> sqlx::Result<ThreadMember> {
            Ok(ThreadMember {
                user_id: r.try_get(0)?,
                read_seq: r.try_get(1)?,
                following: r.try_get::<i64, _>(2)? != 0,
            })
        })
        .transpose()?)
    }

    /// Joins a member to a thread, or updates them: `following` None keeps
    /// what they chose (a mention does not undo an unfollow), `read_seq`
    /// only moves forward.
    pub async fn thread_join(
        &mut self,
        conv_id: &str,
        root_id: &str,
        user_id: &str,
        following: Option<bool>,
        read_seq: Option<i64>,
    ) -> ApiResult<()> {
        match self.thread_member(conv_id, root_id, user_id).await? {
            Some(m) => {
                let f = following.unwrap_or(m.following);
                let r = read_seq.map_or(m.read_seq, |s| s.max(m.read_seq));
                if f != m.following || r != m.read_seq {
                    sqlx::query(self.d.q(
                        "UPDATE thread_members SET following = ?, read_seq = ? WHERE conv_id = ? AND root_id = ? AND user_id = ?",
                    ))
                    .bind(f as i64)
                    .bind(r)
                    .bind(conv_id)
                    .bind(root_id)
                    .bind(user_id)
                    .execute(&mut *self.c)
                    .await?;
                }
            }
            None => {
                sqlx::query(self.d.q(
                    "INSERT INTO thread_members (conv_id, root_id, user_id, read_seq, following) VALUES (?, ?, ?, ?, ?)",
                ))
                .bind(conv_id)
                .bind(root_id)
                .bind(user_id)
                .bind(read_seq.unwrap_or(0))
                .bind(following.unwrap_or(true) as i64)
                .execute(&mut *self.c)
                .await?;
            }
        }
        Ok(())
    }

    /// Members following a thread.
    pub async fn thread_followers(
        &mut self,
        conv_id: &str,
        root_id: &str,
    ) -> ApiResult<Vec<String>> {
        let rows = sqlx::query(self.d.q(
            "SELECT user_id FROM thread_members WHERE conv_id = ? AND root_id = ? AND following = 1 ORDER BY user_id",
        ))
        .bind(conv_id)
        .bind(root_id)
        .fetch_all(&mut *self.c)
        .await?;
        rows.iter()
            .map(|r| Ok(r.try_get(0)?))
            .collect::<ApiResult<_>>()
    }

    /// Distinct senders of a thread's replies (the root's sender first),
    /// at most `n`, and how many there are.
    pub async fn thread_participants(
        &mut self,
        conv_id: &str,
        root_id: &str,
        root_sender: &str,
        n: usize,
    ) -> ApiResult<(Vec<String>, i64)> {
        let rows = sqlx::query(self.d.q(
            "SELECT sender_id, MIN(created_seq) AS first FROM messages WHERE conv_id = ? AND thread_id = ? AND state <> ? GROUP BY sender_id ORDER BY first",
        ))
        .bind(conv_id)
        .bind(root_id)
        .bind("recalled")
        .fetch_all(&mut *self.c)
        .await?;
        let mut all = vec![root_sender.to_string()];
        for r in &rows {
            let s: String = r.try_get(0)?;
            if !all.contains(&s) {
                all.push(s);
            }
        }
        let total = all.len() as i64;
        all.truncate(n);
        Ok((all, total))
    }

    /// Unread replies of a thread for a member: others' replies after
    /// `read_seq`.
    pub async fn thread_unread(
        &mut self,
        conv_id: &str,
        root_id: &str,
        user_id: &str,
        read_seq: i64,
    ) -> ApiResult<i64> {
        let r = sqlx::query(self.d.q(
            "SELECT COUNT(*) FROM messages WHERE conv_id = ? AND thread_id = ? AND created_seq > ? AND sender_id <> ? AND state <> ?",
        ))
        .bind(conv_id)
        .bind(root_id)
        .bind(read_seq)
        .bind(user_id)
        .bind("recalled")
        .fetch_one(&mut *self.c)
        .await?;
        Ok(r.try_get(0)?)
    }

    /// The threads a user follows in a conversation that have replies,
    /// latest reply first, older than `before` (ms).
    pub async fn followed_threads(
        &mut self,
        conv_id: &str,
        user_id: &str,
        before: i64,
        limit: i64,
    ) -> ApiResult<Vec<(ThreadRow, i64)>> {
        let rows = sqlx::query(self.d.q(
            "SELECT t.root_id, t.replies, t.last_seq, t.last_at, t.last_sender, m.read_seq FROM threads t
             JOIN thread_members m ON m.conv_id = t.conv_id AND m.root_id = t.root_id
             WHERE t.conv_id = ? AND m.user_id = ? AND m.following = 1 AND t.replies > 0 AND t.last_at < ?
             ORDER BY t.last_at DESC, t.root_id LIMIT ?",
        ))
        .bind(conv_id)
        .bind(user_id)
        .bind(before)
        .bind(limit)
        .fetch_all(&mut *self.c)
        .await?;
        rows.iter()
            .map(|r| Ok((row_thread(r)?, r.try_get::<i64, _>("read_seq")?)))
            .collect::<ApiResult<_>>()
    }

    /// The threads a user follows in a conversation (export).
    pub async fn threads_followed_by(
        &mut self,
        conv_id: &str,
        user_id: &str,
    ) -> ApiResult<Vec<String>> {
        let rows = sqlx::query(self.d.q(
            "SELECT root_id FROM thread_members WHERE conv_id = ? AND user_id = ? AND following = 1 ORDER BY root_id",
        ))
        .bind(conv_id)
        .bind(user_id)
        .fetch_all(&mut *self.c)
        .await?;
        rows.iter()
            .map(|r| Ok(r.try_get(0)?))
            .collect::<ApiResult<_>>()
    }

    /// The conversations in this database where a user follows a thread.
    pub async fn thread_convs_of(&mut self, user_id: &str) -> ApiResult<Vec<String>> {
        let rows = sqlx::query(self.d.q(
            "SELECT DISTINCT conv_id FROM thread_members WHERE user_id = ? AND following = 1 ORDER BY conv_id",
        ))
        .bind(user_id)
        .fetch_all(&mut *self.c)
        .await?;
        rows.iter()
            .map(|r| Ok(r.try_get(0)?))
            .collect::<ApiResult<_>>()
    }

    /// Every thread of a conversation that has replies, latest reply
    /// first, older than `before` (ms).
    pub async fn threads_page(
        &mut self,
        conv_id: &str,
        before: i64,
        limit: i64,
    ) -> ApiResult<Vec<ThreadRow>> {
        let rows = sqlx::query(self.d.q(
            "SELECT root_id, replies, last_seq, last_at, last_sender FROM threads
             WHERE conv_id = ? AND replies > 0 AND last_at < ? ORDER BY last_at DESC, root_id LIMIT ?",
        ))
        .bind(conv_id)
        .bind(before)
        .bind(limit)
        .fetch_all(&mut *self.c)
        .await?;
        rows.iter()
            .map(|r| Ok(row_thread(r)?))
            .collect::<ApiResult<_>>()
    }

    /// Thread replies after a seq, oldest first.
    pub async fn thread_replies(
        &mut self,
        conv_id: &str,
        root_id: &str,
        after_seq: i64,
        limit: i64,
    ) -> ApiResult<Vec<MsgView>> {
        let rows = sqlx::query(self.d.q(&format!(
            "SELECT {} FROM messages WHERE conv_id = ? AND thread_id = ? AND created_seq > ? ORDER BY created_seq LIMIT ?",
            super::MSG_COLS
        )))
        .bind(conv_id)
        .bind(root_id)
        .bind(after_seq)
        .bind(limit)
        .fetch_all(&mut *self.c)
        .await?;
        self.preload(&rows, &["content", "payload"]).await?;
        rows.iter()
            .map(|r| Ok(super::row_msg(r, self.cx)?))
            .collect::<ApiResult<_>>()
    }

    /// Every thread of a conversation (the verifier compares them with the
    /// log).
    pub async fn threads_of(&mut self, conv_id: &str) -> ApiResult<Vec<ThreadRow>> {
        let rows = sqlx::query(self.d.q(
            "SELECT root_id, replies, last_seq, last_at, last_sender FROM threads WHERE conv_id = ? ORDER BY root_id",
        ))
        .bind(conv_id)
        .fetch_all(&mut *self.c)
        .await?;
        rows.iter()
            .map(|r| Ok(row_thread(r)?))
            .collect::<ApiResult<_>>()
    }

    // ----------------------------------------------------------- reactions

    /// The keys a user put on a message, in the order they were added.
    pub async fn my_reactions(&mut self, msg_id: &str, user_id: &str) -> ApiResult<Vec<String>> {
        let rows = sqlx::query(
            self.d
                .q("SELECT rkey FROM reactions WHERE msg_id = ? AND user_id = ? ORDER BY seq"),
        )
        .bind(msg_id)
        .bind(user_id)
        .fetch_all(&mut *self.c)
        .await?;
        rows.iter()
            .map(|r| Ok(r.try_get(0)?))
            .collect::<ApiResult<_>>()
    }

    /// Distinct keys on a message.
    pub async fn reaction_keys(&mut self, msg_id: &str) -> ApiResult<Vec<String>> {
        let rows = sqlx::query(self.d.q(
            "SELECT rkey, MIN(seq) AS first FROM reactions WHERE msg_id = ? GROUP BY rkey ORDER BY first",
        ))
        .bind(msg_id)
        .fetch_all(&mut *self.c)
        .await?;
        rows.iter()
            .map(|r| Ok(r.try_get(0)?))
            .collect::<ApiResult<_>>()
    }

    pub async fn add_reaction(
        &mut self,
        conv_id: &str,
        msg_id: &str,
        user_id: &str,
        key: &str,
        seq: i64,
    ) -> ApiResult<()> {
        sqlx::query(self.d.q(
            "INSERT INTO reactions (conv_id, msg_id, user_id, rkey, seq, created_at) VALUES (?, ?, ?, ?, ?, ?)",
        ))
        .bind(conv_id)
        .bind(msg_id)
        .bind(user_id)
        .bind(key)
        .bind(seq)
        .bind(now_ms())
        .execute(&mut *self.c)
        .await?;
        Ok(())
    }

    pub async fn remove_reaction(
        &mut self,
        msg_id: &str,
        user_id: &str,
        key: &str,
    ) -> ApiResult<u64> {
        Ok(sqlx::query(
            self.d
                .q("DELETE FROM reactions WHERE msg_id = ? AND user_id = ? AND rkey = ?"),
        )
        .bind(msg_id)
        .bind(user_id)
        .bind(key)
        .execute(&mut *self.c)
        .await?
        .rows_affected())
    }

    /// Removes every reaction of a message (it was recalled).
    pub async fn clear_reactions(&mut self, msg_id: &str) -> ApiResult<u64> {
        Ok(
            sqlx::query(self.d.q("DELETE FROM reactions WHERE msg_id = ?"))
                .bind(msg_id)
                .execute(&mut *self.c)
                .await?
                .rows_affected(),
        )
    }

    /// The reactions of a user in a conversation (account deletion, export).
    pub async fn reactions_of_user(
        &mut self,
        conv_id: &str,
        user_id: &str,
    ) -> ApiResult<Vec<(String, String, i64)>> {
        let rows = sqlx::query(self.d.q(
            "SELECT msg_id, rkey, created_at FROM reactions WHERE conv_id = ? AND user_id = ? ORDER BY seq",
        ))
        .bind(conv_id)
        .bind(user_id)
        .fetch_all(&mut *self.c)
        .await?;
        rows.iter()
            .map(|r| Ok((r.try_get(0)?, r.try_get(1)?, r.try_get(2)?)))
            .collect::<ApiResult<_>>()
    }

    /// The conversations a user reacted in.
    pub async fn reaction_convs_of(&mut self, user_id: &str) -> ApiResult<Vec<String>> {
        let rows = sqlx::query(
            self.d
                .q("SELECT DISTINCT conv_id FROM reactions WHERE user_id = ? ORDER BY conv_id"),
        )
        .bind(user_id)
        .fetch_all(&mut *self.c)
        .await?;
        rows.iter()
            .map(|r| Ok(r.try_get(0)?))
            .collect::<ApiResult<_>>()
    }

    /// The summary of each message's reactions: per key the count, whether
    /// `viewer` is among them, and the latest `n` people.
    pub async fn reaction_summary(
        &mut self,
        msg_ids: &[String],
        viewer: &str,
        n: usize,
    ) -> ApiResult<std::collections::HashMap<String, Vec<Value>>> {
        let mut out = std::collections::HashMap::new();
        for id in msg_ids {
            let rows = sqlx::query(
                self.d
                    .q("SELECT rkey, user_id, seq FROM reactions WHERE msg_id = ? ORDER BY seq"),
            )
            .bind(id)
            .fetch_all(&mut *self.c)
            .await?;
            // Keys in the order they first appeared.
            let mut keys: Vec<(String, Vec<String>)> = vec![];
            for r in &rows {
                let (k, u): (String, String) = (r.try_get(0)?, r.try_get(1)?);
                match keys.iter_mut().find(|(x, _)| *x == k) {
                    Some((_, us)) => us.push(u),
                    None => keys.push((k, vec![u])),
                }
            }
            if keys.is_empty() {
                continue;
            }
            let v = keys
                .into_iter()
                .map(|(k, us)| {
                    let latest: Vec<&String> = us.iter().rev().take(n).collect();
                    json!({"key": k, "count": us.len(), "me": us.iter().any(|u| u == viewer), "users": latest})
                })
                .collect();
            out.insert(id.clone(), v);
        }
        Ok(out)
    }

    /// People who reacted with a key, oldest first, after a seq.
    pub async fn reaction_users(
        &mut self,
        msg_id: &str,
        key: &str,
        after_seq: i64,
        limit: i64,
    ) -> ApiResult<Vec<(String, i64, i64)>> {
        let rows = sqlx::query(self.d.q(
            "SELECT user_id, seq, created_at FROM reactions WHERE msg_id = ? AND rkey = ? AND seq > ? ORDER BY seq LIMIT ?",
        ))
        .bind(msg_id)
        .bind(key)
        .bind(after_seq)
        .bind(limit)
        .fetch_all(&mut *self.c)
        .await?;
        rows.iter()
            .map(|r| Ok((r.try_get(0)?, r.try_get(1)?, r.try_get(2)?)))
            .collect::<ApiResult<_>>()
    }

    /// Every reaction of a conversation as (msg_id, user_id, key) (the
    /// verifier compares them with the log).
    pub async fn reactions_of(
        &mut self,
        conv_id: &str,
    ) -> ApiResult<Vec<(String, String, String)>> {
        let rows = sqlx::query(self.d.q(
            "SELECT msg_id, user_id, rkey FROM reactions WHERE conv_id = ? ORDER BY msg_id, user_id, rkey",
        ))
        .bind(conv_id)
        .fetch_all(&mut *self.c)
        .await?;
        rows.iter()
            .map(|r| Ok((r.try_get(0)?, r.try_get(1)?, r.try_get(2)?)))
            .collect::<ApiResult<_>>()
    }

    // ---------------------------------------------------------------- pins

    /// Who pinned a message, when, and the seq of the pin.
    pub async fn pin_of(
        &mut self,
        conv_id: &str,
        msg_id: &str,
    ) -> ApiResult<Option<(String, i64, i64)>> {
        let r = sqlx::query(
            self.d
                .q("SELECT pinned_by, pinned_at, seq FROM pins WHERE conv_id = ? AND msg_id = ?"),
        )
        .bind(conv_id)
        .bind(msg_id)
        .fetch_optional(&mut *self.c)
        .await?;
        Ok(
            r.map(|r| -> sqlx::Result<_> { Ok((r.try_get(0)?, r.try_get(1)?, r.try_get(2)?)) })
                .transpose()?,
        )
    }

    pub async fn pin_count(&mut self, conv_id: &str) -> ApiResult<i64> {
        let r = sqlx::query(self.d.q("SELECT COUNT(*) FROM pins WHERE conv_id = ?"))
            .bind(conv_id)
            .fetch_one(&mut *self.c)
            .await?;
        Ok(r.try_get(0)?)
    }

    pub async fn add_pin(
        &mut self,
        conv_id: &str,
        msg_id: &str,
        by: &str,
        seq: i64,
    ) -> ApiResult<()> {
        sqlx::query(self.d.q(
            "INSERT INTO pins (conv_id, msg_id, pinned_by, pinned_at, seq) VALUES (?, ?, ?, ?, ?)",
        ))
        .bind(conv_id)
        .bind(msg_id)
        .bind(by)
        .bind(now_ms())
        .bind(seq)
        .execute(&mut *self.c)
        .await?;
        Ok(())
    }

    pub async fn remove_pin(&mut self, conv_id: &str, msg_id: &str) -> ApiResult<u64> {
        Ok(sqlx::query(
            self.d
                .q("DELETE FROM pins WHERE conv_id = ? AND msg_id = ?"),
        )
        .bind(conv_id)
        .bind(msg_id)
        .execute(&mut *self.c)
        .await?
        .rows_affected())
    }

    /// Pins latest first, older than `before_seq`: (msg_id, by, at, seq).
    pub async fn pins(
        &mut self,
        conv_id: &str,
        before_seq: i64,
        limit: i64,
    ) -> ApiResult<Vec<(String, String, i64, i64)>> {
        let rows = sqlx::query(self.d.q(
            "SELECT msg_id, pinned_by, pinned_at, seq FROM pins WHERE conv_id = ? AND seq < ? ORDER BY seq DESC LIMIT ?",
        ))
        .bind(conv_id)
        .bind(before_seq)
        .bind(limit)
        .fetch_all(&mut *self.c)
        .await?;
        rows.iter()
            .map(|r| Ok((r.try_get(0)?, r.try_get(1)?, r.try_get(2)?, r.try_get(3)?)))
            .collect::<ApiResult<_>>()
    }

    pub async fn pin_policy(&mut self, conv_id: &str) -> ApiResult<String> {
        let r = sqlx::query(
            self.d
                .q("SELECT pin_policy FROM conversations WHERE id = ?"),
        )
        .bind(conv_id)
        .fetch_optional(&mut *self.c)
        .await?;
        Ok(
            r.and_then(|r| r.try_get::<Option<String>, _>(0).ok().flatten())
                .unwrap_or_else(|| "members".into()),
        )
    }

    pub async fn set_pin_policy(&mut self, conv_id: &str, policy: &str) -> ApiResult<()> {
        sqlx::query(
            self.d
                .q("UPDATE conversations SET pin_policy = ? WHERE id = ?"),
        )
        .bind(policy)
        .bind(conv_id)
        .execute(&mut *self.c)
        .await?;
        Ok(())
    }

    /// What derives from messages older than `seq` (retention): reactions,
    /// pins and thread rows of the deleted messages.
    pub async fn purge_social_before(&mut self, conv_id: &str, seq: i64) -> ApiResult<()> {
        for sql in [
            "DELETE FROM reactions WHERE conv_id = ? AND msg_id IN (SELECT msg_id FROM messages WHERE conv_id = ? AND created_seq < ?)",
            "DELETE FROM pins WHERE conv_id = ? AND msg_id IN (SELECT msg_id FROM messages WHERE conv_id = ? AND created_seq < ?)",
            "DELETE FROM thread_members WHERE conv_id = ? AND root_id IN (SELECT msg_id FROM messages WHERE conv_id = ? AND created_seq < ?)",
            "DELETE FROM threads WHERE conv_id = ? AND root_id IN (SELECT msg_id FROM messages WHERE conv_id = ? AND created_seq < ?)",
        ] {
            sqlx::query(self.d.q(sql))
                .bind(conv_id)
                .bind(conv_id)
                .bind(seq)
                .execute(&mut *self.c)
                .await?;
        }
        Ok(())
    }
}
