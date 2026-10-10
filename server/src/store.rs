//! Embedded storage for the single-node edition (SQLite, WAL, synchronous=FULL).
//!
//! Every state change that a client can observe is written in one transaction
//! together with the main-log event that announces it, so a seq is never
//! assigned without the data it points to (and vice versa).

use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde_json::{Value, json};

use crate::model::*;

pub struct Store {
    conn: Connection,
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS users (
    id TEXT PRIMARY KEY,
    tenant_id TEXT NOT NULL,
    name TEXT NOT NULL UNIQUE,
    kind TEXT NOT NULL,
    owner_id TEXT,
    token TEXT NOT NULL UNIQUE,
    created_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS conversations (
    id TEXT PRIMARY KEY,
    tenant_id TEXT NOT NULL,
    kind TEXT NOT NULL,
    title TEXT NOT NULL,
    last_seq INTEGER NOT NULL DEFAULT 0,
    side_seq INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS members (
    conv_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    role TEXT NOT NULL,
    trigger_mode TEXT NOT NULL,
    approval INTEGER NOT NULL DEFAULT 0,
    join_seq INTEGER NOT NULL,
    PRIMARY KEY (conv_id, user_id)
);
CREATE INDEX IF NOT EXISTS members_by_user ON members(user_id);
CREATE TABLE IF NOT EXISTS events (
    conv_id TEXT NOT NULL,
    seq INTEGER NOT NULL,
    type TEXT NOT NULL,
    actor_id TEXT,
    msg_id TEXT,
    payload TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (conv_id, seq)
);
CREATE TABLE IF NOT EXISTS messages (
    msg_id TEXT PRIMARY KEY,
    conv_id TEXT NOT NULL,
    created_seq INTEGER NOT NULL,
    sender_id TEXT NOT NULL,
    on_behalf_of TEXT,
    kind TEXT NOT NULL,
    content TEXT NOT NULL,
    version INTEGER NOT NULL,
    state TEXT NOT NULL,
    state_reason TEXT,
    hop INTEGER NOT NULL,
    causation_id TEXT,
    reply_to TEXT,
    mentions TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS messages_by_conv ON messages(conv_id, created_seq);
CREATE TABLE IF NOT EXISTS side_events (
    conv_id TEXT NOT NULL,
    side_seq INTEGER NOT NULL,
    anchor_seq INTEGER NOT NULL,
    audience TEXT NOT NULL,
    type TEXT NOT NULL,
    actor_id TEXT,
    payload TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (conv_id, side_seq)
);
CREATE TABLE IF NOT EXISTS dedup (
    conv_id TEXT NOT NULL,
    sender_id TEXT NOT NULL,
    client_msg_id TEXT NOT NULL,
    result TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (conv_id, sender_id, client_msg_id)
);
CREATE TABLE IF NOT EXISTS read_cursors (
    conv_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    read_seq INTEGER NOT NULL,
    PRIMARY KEY (conv_id, user_id)
);
CREATE TABLE IF NOT EXISTS drafts (
    draft_id TEXT PRIMARY KEY,
    conv_id TEXT NOT NULL,
    agent_id TEXT NOT NULL,
    content TEXT NOT NULL,
    hop INTEGER NOT NULL,
    causation_id TEXT,
    reply_to TEXT,
    state TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    decided_by TEXT,
    decided_at INTEGER
);
CREATE TABLE IF NOT EXISTS takeovers (
    conv_id TEXT NOT NULL,
    agent_id TEXT NOT NULL,
    by_user TEXT NOT NULL,
    since INTEGER NOT NULL,
    PRIMARY KEY (conv_id, agent_id)
);
CREATE TABLE IF NOT EXISTS audit (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    ts INTEGER NOT NULL,
    actor_id TEXT,
    action TEXT NOT NULL,
    conv_id TEXT,
    target TEXT,
    detail TEXT NOT NULL
);
"#;

#[derive(Debug, Clone)]
pub struct ConvRow {
    pub id: String,
    pub kind: String,
    pub title: String,
    pub last_seq: i64,
}

#[derive(Debug, Clone)]
pub struct Draft {
    pub draft_id: String,
    pub conv_id: String,
    pub agent_id: String,
    pub content: String,
    pub hop: i64,
    pub causation_id: Option<String>,
    pub reply_to: Option<String>,
    pub state: String,
    pub created_at: i64,
}

/// Inputs for creating a message in the main log.
pub struct NewMsg<'a> {
    pub conv_id: &'a str,
    pub sender: &'a User,
    pub client_msg_id: Option<&'a str>,
    pub on_behalf_of: Option<String>,
    pub kind: &'a str,
    pub content: String,
    pub state: &'a str,
    pub hop: i64,
    pub causation_id: Option<String>,
    pub reply_to: Option<String>,
    pub mentions: Vec<String>,
}

#[allow(clippy::large_enum_variant)]
pub enum Committed {
    New(Event),
    /// Retry of an already committed client_msg_id: returns the original.
    Duplicate {
        seq: i64,
        msg_id: String,
    },
}

fn new_id() -> String {
    uuid::Uuid::now_v7().simple().to_string()
}

impl Store {
    pub fn open(path: &str) -> rusqlite::Result<Store> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "FULL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.execute_batch(SCHEMA)?;
        Ok(Store { conn })
    }

    pub fn is_empty(&self) -> rusqlite::Result<bool> {
        let n: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0))?;
        Ok(n == 0)
    }

    // ---------------------------------------------------------------- users

    pub fn create_user(
        &mut self,
        name: &str,
        kind: UserKind,
        owner_id: Option<&str>,
    ) -> ApiResult<(User, String)> {
        let id = format!("u_{}", new_id());
        let token = uuid::Uuid::new_v4().simple().to_string();
        self.conn
            .execute(
                "INSERT INTO users (id, tenant_id, name, kind, owner_id, token, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![id, TENANT_DEFAULT, name, kind.as_str(), owner_id, token, now_ms()],
            )
            .map_err(|e| ApiError::new("conflict", e.to_string()))?;
        Ok((
            User {
                id,
                name: name.to_string(),
                kind,
                owner_id: owner_id.map(String::from),
            },
            token,
        ))
    }

    fn row_user(r: &rusqlite::Row) -> rusqlite::Result<User> {
        Ok(User {
            id: r.get(0)?,
            name: r.get(1)?,
            kind: UserKind::parse(&r.get::<_, String>(2)?),
            owner_id: r.get(3)?,
        })
    }

    pub fn user_by_token(&self, token: &str) -> ApiResult<Option<User>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id, name, kind, owner_id FROM users WHERE token = ?1",
                [token],
                Self::row_user,
            )
            .optional()?)
    }

    pub fn user(&self, id: &str) -> ApiResult<Option<User>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id, name, kind, owner_id FROM users WHERE id = ?1",
                [id],
                Self::row_user,
            )
            .optional()?)
    }

    pub fn token_by_name(&self, name: &str) -> ApiResult<Option<(User, String)>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id, name, kind, owner_id, token FROM users WHERE name = ?1",
                [name],
                |r| Ok((Self::row_user(r)?, r.get::<_, String>(4)?)),
            )
            .optional()?)
    }

    pub fn list_users(&self) -> ApiResult<Vec<User>> {
        let mut st = self
            .conn
            .prepare("SELECT id, name, kind, owner_id FROM users ORDER BY created_at")?;
        let rows = st
            .query_map([], Self::row_user)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn agents_owned_by(&self, owner: &str) -> ApiResult<Vec<User>> {
        let mut st = self.conn.prepare(
            "SELECT id, name, kind, owner_id FROM users WHERE owner_id = ?1 ORDER BY created_at",
        )?;
        let rows = st
            .query_map([owner], Self::row_user)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    // -------------------------------------------------------- conversations

    pub fn conv(&self, id: &str) -> ApiResult<Option<ConvRow>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id, kind, title, last_seq FROM conversations WHERE id = ?1",
                [id],
                |r| {
                    Ok(ConvRow {
                        id: r.get(0)?,
                        kind: r.get(1)?,
                        title: r.get(2)?,
                        last_seq: r.get(3)?,
                    })
                },
            )
            .optional()?)
    }

    /// Creates a conversation and announces every initial member with a
    /// `member.joined` event (membership changes take a seq).
    pub fn create_conv(
        &mut self,
        kind: &str,
        title: &str,
        creator: &User,
        members: &[User],
    ) -> ApiResult<(String, Vec<Event>)> {
        let id = format!("c_{}", new_id());
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO conversations (id, tenant_id, kind, title, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id, TENANT_DEFAULT, kind, title, now_ms()],
        )?;
        let mut events = vec![append_event(
            &tx,
            &id,
            "conv.created",
            Some(&creator.id),
            None,
            json!({"kind": kind, "title": title}),
        )?];
        let mut all: Vec<&User> = vec![creator];
        all.extend(members.iter().filter(|m| m.id != creator.id));
        for u in all {
            let role = if u.id == creator.id {
                "owner"
            } else {
                "member"
            };
            events.push(add_member_tx(&tx, &id, kind, u, role, &creator.id)?);
        }
        tx.commit()?;
        Ok((id, events))
    }

    pub fn add_member(&mut self, conv_id: &str, user: &User, actor: &str) -> ApiResult<Event> {
        let conv = self
            .conv(conv_id)?
            .ok_or_else(|| ApiError::new("not_found", "conversation"))?;
        if self.member(conv_id, &user.id)?.is_some() {
            return Err(ApiError::new("conflict", "already a member"));
        }
        let tx = self.conn.transaction()?;
        let ev = add_member_tx(&tx, conv_id, &conv.kind, user, "member", actor)?;
        tx.commit()?;
        Ok(ev)
    }

    pub fn members(&self, conv_id: &str) -> ApiResult<Vec<Member>> {
        let mut st = self.conn.prepare(
            "SELECT m.user_id, u.name, u.kind, u.owner_id, m.role, m.trigger_mode, m.approval, m.join_seq
             FROM members m JOIN users u ON u.id = m.user_id WHERE m.conv_id = ?1 ORDER BY m.join_seq",
        )?;
        let rows = st
            .query_map([conv_id], row_member)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn member(&self, conv_id: &str, user_id: &str) -> ApiResult<Option<Member>> {
        Ok(self
            .conn
            .query_row(
                "SELECT m.user_id, u.name, u.kind, u.owner_id, m.role, m.trigger_mode, m.approval, m.join_seq
                 FROM members m JOIN users u ON u.id = m.user_id WHERE m.conv_id = ?1 AND m.user_id = ?2",
                [conv_id, user_id],
                row_member,
            )
            .optional()?)
    }

    pub fn set_agent_policy(
        &mut self,
        conv_id: &str,
        agent_id: &str,
        approval: Option<bool>,
        trigger: Option<Trigger>,
        actor: &str,
    ) -> ApiResult<Event> {
        let tx = self.conn.transaction()?;
        if let Some(a) = approval {
            tx.execute(
                "UPDATE members SET approval = ?3 WHERE conv_id = ?1 AND user_id = ?2",
                params![conv_id, agent_id, a as i64],
            )?;
        }
        if let Some(t) = trigger {
            tx.execute(
                "UPDATE members SET trigger_mode = ?3 WHERE conv_id = ?1 AND user_id = ?2",
                params![conv_id, agent_id, t.as_str()],
            )?;
        }
        let (approval, trigger): (i64, String) = tx.query_row(
            "SELECT approval, trigger_mode FROM members WHERE conv_id = ?1 AND user_id = ?2",
            [conv_id, agent_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let ev = append_event(
            &tx,
            conv_id,
            "member.policy",
            Some(actor),
            None,
            json!({"user_id": agent_id, "approval": approval != 0, "trigger": trigger}),
        )?;
        tx.commit()?;
        Ok(ev)
    }

    /// Conversations the user belongs to, plus conversations their agents
    /// belong to (observable by the owner through the agent's read scope).
    pub fn convs_for(&self, user_id: &str) -> ApiResult<Vec<(ConvRow, Option<String>)>> {
        let mut out = Vec::new();
        let mut st = self.conn.prepare(
            "SELECT c.id, c.kind, c.title, c.last_seq FROM conversations c JOIN members m ON m.conv_id = c.id
             WHERE m.user_id = ?1 ORDER BY c.created_at",
        )?;
        for r in st.query_map([user_id], |r| {
            Ok(ConvRow {
                id: r.get(0)?,
                kind: r.get(1)?,
                title: r.get(2)?,
                last_seq: r.get(3)?,
            })
        })? {
            out.push((r?, None));
        }
        let mut st = self.conn.prepare(
            "SELECT c.id, c.kind, c.title, c.last_seq, u.id FROM conversations c JOIN members m ON m.conv_id = c.id
             JOIN users u ON u.id = m.user_id
             WHERE u.owner_id = ?1 AND c.id NOT IN (SELECT conv_id FROM members WHERE user_id = ?1)
             ORDER BY c.created_at",
        )?;
        for r in st.query_map([user_id], |r| {
            Ok((
                ConvRow {
                    id: r.get(0)?,
                    kind: r.get(1)?,
                    title: r.get(2)?,
                    last_seq: r.get(3)?,
                },
                r.get::<_, String>(4)?,
            ))
        })? {
            let (c, a) = r?;
            if !out.iter().any(|(x, _)| x.id == c.id) {
                out.push((c, Some(a)));
            }
        }
        Ok(out)
    }

    // ------------------------------------------------------------ messages

    /// Dedup + seq assignment + persistence in one transaction.
    pub fn commit_message(&mut self, m: NewMsg) -> ApiResult<Committed> {
        let tx = self.conn.transaction()?;
        if let Some(cid) = m.client_msg_id {
            let prev: Option<String> = tx
                .query_row(
                    "SELECT result FROM dedup WHERE conv_id = ?1 AND sender_id = ?2 AND client_msg_id = ?3",
                    [m.conv_id, &m.sender.id, cid],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(prev) = prev {
                let v: Value = serde_json::from_str(&prev).unwrap_or(Value::Null);
                return Ok(Committed::Duplicate {
                    seq: v["seq"].as_i64().unwrap_or(0),
                    msg_id: v["msg_id"].as_str().unwrap_or("").to_string(),
                });
            }
        }
        let seq = next_seq(&tx, m.conv_id)?;
        let ts = now_ms();
        let view = MsgView {
            msg_id: format!("m_{}", new_id()),
            conv_id: m.conv_id.to_string(),
            created_seq: seq,
            sender_id: m.sender.id.clone(),
            sender_name: m.sender.name.clone(),
            sender_type: m.sender.kind,
            on_behalf_of: m.on_behalf_of,
            kind: m.kind.to_string(),
            content: m.content,
            version: 1,
            state: m.state.to_string(),
            state_reason: None,
            hop: m.hop,
            causation_id: m.causation_id,
            reply_to: m.reply_to,
            mentions: m.mentions,
            created_at: ts,
            updated_at: ts,
        };
        tx.execute(
            "INSERT INTO messages (msg_id, conv_id, created_seq, sender_id, on_behalf_of, kind, content, version, state,
             state_reason, hop, causation_id, reply_to, mentions, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
            params![
                view.msg_id,
                view.conv_id,
                seq,
                view.sender_id,
                view.on_behalf_of,
                view.kind,
                view.content,
                view.version,
                view.state,
                view.state_reason,
                view.hop,
                view.causation_id,
                view.reply_to,
                serde_json::to_string(&view.mentions).unwrap(),
                ts,
                ts
            ],
        )?;
        if let Some(cid) = m.client_msg_id {
            tx.execute(
                "INSERT INTO dedup (conv_id, sender_id, client_msg_id, result, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![m.conv_id, m.sender.id, cid, json!({"seq": seq, "msg_id": view.msg_id}).to_string(), ts],
            )?;
        }
        let sender_id = view.sender_id.clone();
        let ev = write_event(
            &tx,
            m.conv_id,
            seq,
            "msg.created",
            Some(&sender_id),
            Some(view),
            json!({}),
            ts,
        )?;
        tx.commit()?;
        Ok(Committed::New(ev))
    }

    pub fn message(&self, msg_id: &str) -> ApiResult<Option<MsgView>> {
        Ok(self
            .conn
            .query_row(
                &format!("{MSG_SELECT} WHERE m.msg_id = ?1"),
                [msg_id],
                row_msg,
            )
            .optional()?)
    }

    /// Applies a change to a message and announces it with a new event.
    /// `kind` is `msg.updated` or `msg.recalled`.
    pub fn update_message(
        &mut self,
        msg_id: &str,
        kind: &str,
        actor: &str,
        content: Option<&str>,
        state: Option<&str>,
        reason: Option<&str>,
    ) -> ApiResult<Event> {
        let tx = self.conn.transaction()?;
        let ts = now_ms();
        tx.execute(
            "UPDATE messages SET content = COALESCE(?2, content), state = COALESCE(?3, state),
             state_reason = COALESCE(?4, state_reason), version = version + 1, updated_at = ?5 WHERE msg_id = ?1",
            params![msg_id, content, state, reason, ts],
        )?;
        let view = tx.query_row(
            &format!("{MSG_SELECT} WHERE m.msg_id = ?1"),
            [msg_id],
            row_msg,
        )?;
        let conv_id = view.conv_id.clone();
        let ev = append_event(&tx, &conv_id, kind, Some(actor), Some(view), json!({}))?;
        tx.commit()?;
        Ok(ev)
    }

    /// Streaming checkpoint: overwrites partial content without an event.
    pub fn checkpoint(&mut self, msg_id: &str, content: &str) -> ApiResult<()> {
        self.conn.execute(
            "UPDATE messages SET content = ?2, updated_at = ?3 WHERE msg_id = ?1",
            params![msg_id, content, now_ms()],
        )?;
        Ok(())
    }

    pub fn streaming_messages(&self) -> ApiResult<Vec<MsgView>> {
        let mut st = self
            .conn
            .prepare(&format!("{MSG_SELECT} WHERE m.state = 'streaming'"))?;
        let rows = st.query_map([], row_msg)?.collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Most recent message in the conversation not sent by `user_id`.
    pub fn latest_other_message(&self, conv_id: &str, user_id: &str) -> ApiResult<Option<MsgView>> {
        Ok(self
            .conn
            .query_row(
                &format!("{MSG_SELECT} WHERE m.conv_id = ?1 AND m.sender_id != ?2 ORDER BY m.created_seq DESC LIMIT 1"),
                [conv_id, user_id],
                row_msg,
            )
            .optional()?)
    }

    pub fn recent_contents(
        &self,
        conv_id: &str,
        sender_id: &str,
        n: i64,
    ) -> ApiResult<Vec<String>> {
        let mut st = self.conn.prepare(
            "SELECT content FROM messages WHERE conv_id = ?1 AND sender_id = ?2 ORDER BY created_seq DESC LIMIT ?3",
        )?;
        let rows = st
            .query_map(params![conv_id, sender_id, n], |r| r.get(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    // ------------------------------------------------------------- reading

    pub fn pull(&self, conv_id: &str, after_seq: i64, limit: i64) -> ApiResult<Vec<Event>> {
        let mut st = self.conn.prepare(
            "SELECT conv_id, seq, type, actor_id, payload, created_at FROM events
             WHERE conv_id = ?1 AND seq > ?2 ORDER BY seq LIMIT ?3",
        )?;
        let rows = st
            .query_map(params![conv_id, after_seq, limit], row_event)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn pull_side(&self, conv_id: &str, after: i64) -> ApiResult<Vec<SideEvent>> {
        let mut st = self.conn.prepare(
            "SELECT conv_id, side_seq, anchor_seq, audience, type, actor_id, payload, created_at FROM side_events
             WHERE conv_id = ?1 AND side_seq > ?2 ORDER BY side_seq",
        )?;
        let rows = st
            .query_map(params![conv_id, after], row_side)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn set_read(&mut self, conv_id: &str, user_id: &str, seq: i64) -> ApiResult<()> {
        self.conn.execute(
            "INSERT INTO read_cursors (conv_id, user_id, read_seq) VALUES (?1, ?2, ?3)
             ON CONFLICT(conv_id, user_id) DO UPDATE SET read_seq = MAX(read_seq, excluded.read_seq)",
            params![conv_id, user_id, seq],
        )?;
        Ok(())
    }

    pub fn read_seq(&self, conv_id: &str, user_id: &str) -> ApiResult<i64> {
        Ok(self
            .conn
            .query_row(
                "SELECT read_seq FROM read_cursors WHERE conv_id = ?1 AND user_id = ?2",
                [conv_id, user_id],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(0))
    }

    /// Unread counts only new messages from others (not edits, recalls or
    /// system events), see docs/05-review.md P1-1.
    pub fn unread(&self, conv_id: &str, user_id: &str) -> ApiResult<i64> {
        let read = self.read_seq(conv_id, user_id)?;
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM messages WHERE conv_id = ?1 AND created_seq > ?2 AND sender_id != ?3 AND state != 'recalled'",
            params![conv_id, read, user_id],
            |r| r.get(0),
        )?)
    }

    // --------------------------------------------------------- side channel

    pub fn append_side(
        &mut self,
        conv_id: &str,
        audience: &[String],
        kind: &str,
        actor: Option<&str>,
        data: Value,
    ) -> ApiResult<SideEvent> {
        let tx = self.conn.transaction()?;
        let ev = append_side_tx(&tx, conv_id, audience, kind, actor, data)?;
        tx.commit()?;
        Ok(ev)
    }

    // --------------------------------------------------------------- drafts

    #[allow(clippy::too_many_arguments)]
    pub fn create_draft(
        &mut self,
        conv_id: &str,
        agent: &User,
        content: &str,
        hop: i64,
        causation_id: Option<String>,
        reply_to: Option<String>,
        audience: &[String],
    ) -> ApiResult<(Draft, SideEvent)> {
        let tx = self.conn.transaction()?;
        let d = Draft {
            draft_id: format!("d_{}", new_id()),
            conv_id: conv_id.to_string(),
            agent_id: agent.id.clone(),
            content: content.to_string(),
            hop,
            causation_id,
            reply_to,
            state: "pending".into(),
            created_at: now_ms(),
        };
        tx.execute(
            "INSERT INTO drafts (draft_id, conv_id, agent_id, content, hop, causation_id, reply_to, state, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'pending', ?8)",
            params![d.draft_id, d.conv_id, d.agent_id, d.content, d.hop, d.causation_id, d.reply_to, d.created_at],
        )?;
        let side = append_side_tx(
            &tx,
            conv_id,
            audience,
            "draft.created",
            Some(&agent.id),
            json!({"draft_id": d.draft_id, "agent_id": agent.id, "agent_name": agent.name, "content": content}),
        )?;
        tx.commit()?;
        Ok((d, side))
    }

    pub fn draft(&self, id: &str) -> ApiResult<Option<Draft>> {
        Ok(self
            .conn
            .query_row(
                "SELECT draft_id, conv_id, agent_id, content, hop, causation_id, reply_to, state, created_at FROM drafts WHERE draft_id = ?1",
                [id],
                row_draft,
            )
            .optional()?)
    }

    pub fn pending_drafts_for_owner(&self, owner: &str) -> ApiResult<Vec<Draft>> {
        let mut st = self.conn.prepare(
            "SELECT d.draft_id, d.conv_id, d.agent_id, d.content, d.hop, d.causation_id, d.reply_to, d.state, d.created_at
             FROM drafts d JOIN users u ON u.id = d.agent_id WHERE u.owner_id = ?1 AND d.state = 'pending' ORDER BY d.created_at",
        )?;
        let rows = st
            .query_map([owner], row_draft)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn decide_draft(
        &mut self,
        id: &str,
        state: &str,
        by: &str,
        audience: &[String],
        conv_id: &str,
    ) -> ApiResult<SideEvent> {
        let tx = self.conn.transaction()?;
        let n = tx.execute(
            "UPDATE drafts SET state = ?2, decided_by = ?3, decided_at = ?4 WHERE draft_id = ?1 AND state = 'pending'",
            params![id, state, by, now_ms()],
        )?;
        if n == 0 {
            return Err(ApiError::new("conflict", "draft already decided"));
        }
        let side = append_side_tx(
            &tx,
            conv_id,
            audience,
            "draft.decided",
            Some(by),
            json!({"draft_id": id, "state": state}),
        )?;
        tx.commit()?;
        Ok(side)
    }

    // ------------------------------------------------------------ takeovers

    pub fn takeover(&self, conv_id: &str, agent_id: &str) -> ApiResult<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT by_user FROM takeovers WHERE conv_id = ?1 AND agent_id = ?2",
                [conv_id, agent_id],
                |r| r.get(0),
            )
            .optional()?)
    }

    pub fn set_takeover(
        &mut self,
        conv_id: &str,
        agent_id: &str,
        by: Option<&str>,
    ) -> ApiResult<Event> {
        let tx = self.conn.transaction()?;
        match by {
            Some(u) => {
                tx.execute(
                    "INSERT OR REPLACE INTO takeovers (conv_id, agent_id, by_user, since) VALUES (?1, ?2, ?3, ?4)",
                    params![conv_id, agent_id, u, now_ms()],
                )?;
            }
            None => {
                tx.execute(
                    "DELETE FROM takeovers WHERE conv_id = ?1 AND agent_id = ?2",
                    [conv_id, agent_id],
                )?;
            }
        }
        let state = if by.is_some() { "taken_over" } else { "active" };
        let actor = by.unwrap_or(agent_id);
        let ev = append_event(
            &tx,
            conv_id,
            "agent.state",
            Some(actor),
            None,
            json!({"agent_id": agent_id, "state": state, "by": by}),
        )?;
        tx.commit()?;
        Ok(ev)
    }

    pub fn system_event(
        &mut self,
        conv_id: &str,
        kind: &str,
        actor: Option<&str>,
        data: Value,
    ) -> ApiResult<Event> {
        let tx = self.conn.transaction()?;
        let ev = append_event(&tx, conv_id, kind, actor, None, data)?;
        tx.commit()?;
        Ok(ev)
    }

    // ---------------------------------------------------------------- audit

    pub fn audit(
        &mut self,
        actor: Option<&str>,
        action: &str,
        conv_id: Option<&str>,
        target: Option<&str>,
        detail: Value,
    ) {
        let _ = self.conn.execute(
            "INSERT INTO audit (ts, actor_id, action, conv_id, target, detail) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![now_ms(), actor, action, conv_id, target, detail.to_string()],
        );
    }

    pub fn audit_log(&self, limit: i64) -> ApiResult<Vec<Value>> {
        let mut st = self.conn.prepare(
            "SELECT a.ts, a.actor_id, u.name, a.action, a.conv_id, a.target, a.detail FROM audit a
             LEFT JOIN users u ON u.id = a.actor_id ORDER BY a.id DESC LIMIT ?1",
        )?;
        let rows = st
            .query_map([limit], |r| {
                Ok(json!({
                    "ts": r.get::<_, i64>(0)?,
                    "actor_id": r.get::<_, Option<String>>(1)?,
                    "actor": r.get::<_, Option<String>>(2)?,
                    "action": r.get::<_, String>(3)?,
                    "conv_id": r.get::<_, Option<String>>(4)?,
                    "target": r.get::<_, Option<String>>(5)?,
                    "detail": serde_json::from_str::<Value>(&r.get::<_, String>(6)?).unwrap_or(Value::Null),
                }))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn counts(&self) -> ApiResult<Value> {
        let q = |sql: &str| -> rusqlite::Result<i64> { self.conn.query_row(sql, [], |r| r.get(0)) };
        Ok(json!({
            "users": q("SELECT COUNT(*) FROM users WHERE kind = 'human'")?,
            "agents": q("SELECT COUNT(*) FROM users WHERE kind = 'agent'")?,
            "conversations": q("SELECT COUNT(*) FROM conversations")?,
            "events": q("SELECT COUNT(*) FROM events")?,
            "messages": q("SELECT COUNT(*) FROM messages")?,
            "agent_messages": q("SELECT COUNT(*) FROM messages m JOIN users u ON u.id = m.sender_id WHERE u.kind = 'agent'")?,
            "side_events": q("SELECT COUNT(*) FROM side_events")?,
            "pending_drafts": q("SELECT COUNT(*) FROM drafts WHERE state = 'pending'")?,
        }))
    }
}

// ------------------------------------------------------------------ helpers

const MSG_SELECT: &str = "SELECT m.msg_id, m.conv_id, m.created_seq, m.sender_id, u.name, u.kind, m.on_behalf_of, m.kind,
    m.content, m.version, m.state, m.state_reason, m.hop, m.causation_id, m.reply_to, m.mentions, m.created_at, m.updated_at
    FROM messages m JOIN users u ON u.id = m.sender_id";

fn row_msg(r: &rusqlite::Row) -> rusqlite::Result<MsgView> {
    Ok(MsgView {
        msg_id: r.get(0)?,
        conv_id: r.get(1)?,
        created_seq: r.get(2)?,
        sender_id: r.get(3)?,
        sender_name: r.get(4)?,
        sender_type: UserKind::parse(&r.get::<_, String>(5)?),
        on_behalf_of: r.get(6)?,
        kind: r.get(7)?,
        content: r.get(8)?,
        version: r.get(9)?,
        state: r.get(10)?,
        state_reason: r.get(11)?,
        hop: r.get(12)?,
        causation_id: r.get(13)?,
        reply_to: r.get(14)?,
        mentions: serde_json::from_str(&r.get::<_, String>(15)?).unwrap_or_default(),
        created_at: r.get(16)?,
        updated_at: r.get(17)?,
    })
}

fn row_member(r: &rusqlite::Row) -> rusqlite::Result<Member> {
    Ok(Member {
        user_id: r.get(0)?,
        name: r.get(1)?,
        kind: UserKind::parse(&r.get::<_, String>(2)?),
        owner_id: r.get(3)?,
        role: r.get(4)?,
        trigger: Trigger::parse(&r.get::<_, String>(5)?),
        approval: r.get::<_, i64>(6)? != 0,
        join_seq: r.get(7)?,
    })
}

fn row_event(r: &rusqlite::Row) -> rusqlite::Result<Event> {
    let payload: Value = serde_json::from_str(&r.get::<_, String>(4)?).unwrap_or(Value::Null);
    Ok(Event {
        conv_id: r.get(0)?,
        seq: r.get(1)?,
        kind: r.get(2)?,
        actor_id: r.get(3)?,
        msg: serde_json::from_value(payload["msg"].clone()).ok(),
        data: payload["data"].clone(),
        created_at: r.get(5)?,
    })
}

fn row_side(r: &rusqlite::Row) -> rusqlite::Result<SideEvent> {
    Ok(SideEvent {
        conv_id: r.get(0)?,
        side_seq: r.get(1)?,
        anchor_seq: r.get(2)?,
        audience: serde_json::from_str(&r.get::<_, String>(3)?).unwrap_or_default(),
        kind: r.get(4)?,
        actor_id: r.get(5)?,
        data: serde_json::from_str(&r.get::<_, String>(6)?).unwrap_or(Value::Null),
        created_at: r.get(7)?,
    })
}

fn row_draft(r: &rusqlite::Row) -> rusqlite::Result<Draft> {
    Ok(Draft {
        draft_id: r.get(0)?,
        conv_id: r.get(1)?,
        agent_id: r.get(2)?,
        content: r.get(3)?,
        hop: r.get(4)?,
        causation_id: r.get(5)?,
        reply_to: r.get(6)?,
        state: r.get(7)?,
        created_at: r.get(8)?,
    })
}

fn next_seq(tx: &Transaction, conv_id: &str) -> ApiResult<i64> {
    let n = tx.execute(
        "UPDATE conversations SET last_seq = last_seq + 1 WHERE id = ?1",
        [conv_id],
    )?;
    if n == 0 {
        return Err(ApiError::new("not_found", "conversation"));
    }
    Ok(tx.query_row(
        "SELECT last_seq FROM conversations WHERE id = ?1",
        [conv_id],
        |r| r.get(0),
    )?)
}

fn append_event(
    tx: &Transaction,
    conv_id: &str,
    kind: &str,
    actor: Option<&str>,
    msg: Option<MsgView>,
    data: Value,
) -> ApiResult<Event> {
    let seq = next_seq(tx, conv_id)?;
    write_event(tx, conv_id, seq, kind, actor, msg, data, now_ms())
}

#[allow(clippy::too_many_arguments)]
fn write_event(
    tx: &Transaction,
    conv_id: &str,
    seq: i64,
    kind: &str,
    actor: Option<&str>,
    msg: Option<MsgView>,
    data: Value,
    ts: i64,
) -> ApiResult<Event> {
    let payload = json!({"msg": msg, "data": data});
    tx.execute(
        "INSERT INTO events (conv_id, seq, type, actor_id, msg_id, payload, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![conv_id, seq, kind, actor, msg.as_ref().map(|m| m.msg_id.clone()), payload.to_string(), ts],
    )?;
    Ok(Event {
        conv_id: conv_id.to_string(),
        seq,
        kind: kind.to_string(),
        actor_id: actor.map(String::from),
        msg,
        data,
        created_at: ts,
    })
}

fn append_side_tx(
    tx: &Transaction,
    conv_id: &str,
    audience: &[String],
    kind: &str,
    actor: Option<&str>,
    data: Value,
) -> ApiResult<SideEvent> {
    tx.execute(
        "UPDATE conversations SET side_seq = side_seq + 1 WHERE id = ?1",
        [conv_id],
    )?;
    let (side_seq, anchor): (i64, i64) = tx.query_row(
        "SELECT side_seq, last_seq FROM conversations WHERE id = ?1",
        [conv_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let ts = now_ms();
    tx.execute(
        "INSERT INTO side_events (conv_id, side_seq, anchor_seq, audience, type, actor_id, payload, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![conv_id, side_seq, anchor, serde_json::to_string(audience).unwrap(), kind, actor, data.to_string(), ts],
    )?;
    Ok(SideEvent {
        conv_id: conv_id.to_string(),
        side_seq,
        anchor_seq: anchor,
        audience: audience.to_vec(),
        kind: kind.to_string(),
        actor_id: actor.map(String::from),
        data,
        created_at: ts,
    })
}

fn add_member_tx(
    tx: &Transaction,
    conv_id: &str,
    conv_kind: &str,
    u: &User,
    role: &str,
    actor: &str,
) -> ApiResult<Event> {
    // Agents in groups only hear what is addressed to them by default.
    let trigger = if u.kind == UserKind::Agent && conv_kind == "group" {
        Trigger::Mention
    } else {
        Trigger::All
    };
    let ev = append_event(
        tx,
        conv_id,
        "member.joined",
        Some(actor),
        None,
        json!({"user_id": u.id, "name": u.name, "kind": u.kind, "owner_id": u.owner_id, "role": role, "trigger": trigger}),
    )?;
    tx.execute(
        "INSERT INTO members (conv_id, user_id, role, trigger_mode, approval, join_seq) VALUES (?1, ?2, ?3, ?4, 0, ?5)",
        params![conv_id, u.id, role, trigger.as_str(), ev.seq],
    )?;
    Ok(ev)
}
