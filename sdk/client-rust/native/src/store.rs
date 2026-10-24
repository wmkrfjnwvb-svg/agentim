//! The native cache (docs/12 §2.9): SQLite, one account per directory.
//! The queue lives in its own small database file, so rebuilding a damaged
//! cache never loses what the user has not sent yet. Prepared for running
//! out of space: incremental vacuum set at creation, a bounded WAL, a
//! 20 MB reserve file given up when the disk is full (so the queue and the
//! clean-up can still be written), and a failed write is rolled back and
//! never confirmed.

use std::path::{Path, PathBuf};

use agentim_client_core::{Batch, ConvRow, PAGE, Snapshot};
use rusqlite::{Connection, ErrorCode, OptionalExtension, params};
use serde_json::Value;

const RESERVE_BYTES: u64 = 20 * 1024 * 1024;

pub struct Store {
    dir: Option<PathBuf>,
    cache: Connection,
    queue: Connection,
}

/// Why a write failed, as the core names it.
pub fn failure_kind(e: &rusqlite::Error) -> &'static str {
    match e.sqlite_error_code() {
        Some(ErrorCode::DiskFull) => "full",
        Some(ErrorCode::DatabaseCorrupt) | Some(ErrorCode::NotADatabase) => "corrupt",
        _ => "unavailable",
    }
}

fn open_db(path: Option<&Path>, queue: bool) -> rusqlite::Result<Connection> {
    let c = match path {
        Some(p) => Connection::open(p)?,
        None => Connection::open_in_memory()?,
    };
    // Before any table: it cannot be turned on later without a full rewrite.
    c.execute_batch(
        "PRAGMA auto_vacuum = INCREMENTAL;
         PRAGMA journal_mode = WAL;
         PRAGMA journal_size_limit = 67108864;
         PRAGMA temp_store = MEMORY;",
    )?;
    c.execute_batch(if queue {
        "PRAGMA synchronous = FULL;
         CREATE TABLE IF NOT EXISTS ops (id TEXT PRIMARY KEY, n INTEGER NOT NULL, body TEXT NOT NULL);"
    } else {
        "PRAGMA synchronous = NORMAL;
         CREATE TABLE IF NOT EXISTS meta (k TEXT PRIMARY KEY, v TEXT NOT NULL);
         CREATE TABLE IF NOT EXISTS convs (conv_id TEXT PRIMARY KEY, body TEXT NOT NULL);
         CREATE TABLE IF NOT EXISTS messages (
             conv_id TEXT NOT NULL, msg_id TEXT NOT NULL, seq INTEGER NOT NULL, body TEXT NOT NULL,
             PRIMARY KEY (conv_id, msg_id));
         CREATE INDEX IF NOT EXISTS messages_seq ON messages (conv_id, seq);"
    })?;
    Ok(c)
}

impl Store {
    /// Opens (or creates) the cache in `dir`; in memory without one.
    pub fn open(dir: Option<&Path>) -> rusqlite::Result<Store> {
        if let Some(d) = dir {
            let _ = std::fs::create_dir_all(d);
        }
        let cache = open_db(dir.map(|d| d.join("cache.db")).as_deref(), false)?;
        let queue = open_db(dir.map(|d| d.join("queue.db")).as_deref(), true)?;
        let s = Store {
            dir: dir.map(Path::to_path_buf),
            cache,
            queue,
        };
        s.ensure_reserve();
        Ok(s)
    }

    fn reserve(&self) -> Option<PathBuf> {
        self.dir.as_ref().map(|d| d.join("space.reserve"))
    }

    /// Puts the reserve back when there is room for it.
    pub fn ensure_reserve(&self) {
        if let Some(p) = self.reserve()
            && !p.exists()
            && let Ok(f) = std::fs::File::create(&p)
            && f.set_len(RESERVE_BYTES).is_err()
        {
            let _ = std::fs::remove_file(&p);
        }
    }

    /// Gives up the reserve so a write can go through; true if there was one.
    fn release_reserve(&self) -> bool {
        self.reserve()
            .is_some_and(|p| p.exists() && std::fs::remove_file(&p).is_ok())
    }

    pub fn snapshot(&self) -> rusqlite::Result<Snapshot> {
        let account: Option<String> = self
            .cache
            .query_row("SELECT v FROM meta WHERE k = 'account'", [], |r| r.get(0))
            .optional()?;
        let mut convs = vec![];
        let mut st = self.cache.prepare("SELECT body FROM convs")?;
        for row in st.query_map([], |r| r.get::<_, String>(0))? {
            if let Ok(c) = serde_json::from_str::<ConvRow>(&row?) {
                convs.push(c);
            }
        }
        let mut messages = vec![];
        let mut st = self
            .cache
            .prepare("SELECT body FROM messages WHERE conv_id = ?1 ORDER BY seq DESC LIMIT ?2")?;
        for c in &convs {
            let mut page: Vec<Value> = st
                .query_map(params![c.conv_id, PAGE], |r| r.get::<_, String>(0))?
                .filter_map(|b| b.ok().and_then(|b| serde_json::from_str(&b).ok()))
                .collect();
            page.reverse();
            messages.extend(page);
        }
        let mut ops = vec![];
        let mut st = self.queue.prepare("SELECT body FROM ops ORDER BY n")?;
        for row in st.query_map([], |r| r.get::<_, String>(0))? {
            if let Ok(v) = serde_json::from_str(&row?) {
                ops.push(v);
            }
        }
        Ok(Snapshot {
            account: account.filter(|a| a != "null"),
            convs,
            ops,
            messages,
        })
    }

    pub fn page(&self, conv: &str, before: i64, limit: i64) -> rusqlite::Result<Vec<Value>> {
        let mut st = self.cache.prepare(
            "SELECT body FROM messages WHERE conv_id = ?1 AND seq < ?2 ORDER BY seq DESC LIMIT ?3",
        )?;
        let mut v: Vec<Value> = st
            .query_map(params![conv, before, limit], |r| r.get::<_, String>(0))?
            .filter_map(|b| b.ok().and_then(|b| serde_json::from_str(&b).ok()))
            .collect();
        v.reverse();
        Ok(v)
    }

    /// One batch: the queue part first (it must survive), then the cache;
    /// each atomic. On a full disk the reserve is given up and the write
    /// tried once more.
    pub fn write(&mut self, b: &Batch) -> rusqlite::Result<()> {
        match self.write_once(b) {
            Err(e) if failure_kind(&e) == "full" && self.release_reserve() => self.write_once(b),
            r => r,
        }
    }

    fn write_once(&mut self, b: &Batch) -> rusqlite::Result<()> {
        if !b.put_ops.is_empty() || !b.remove_ops.is_empty() {
            let tx = self.queue.transaction()?;
            for o in &b.put_ops {
                tx.execute(
                    "INSERT OR REPLACE INTO ops (id, n, body) VALUES (?1, ?2, ?3)",
                    params![
                        o["id"].as_str().unwrap_or(""),
                        o["n"].as_i64().unwrap_or(0),
                        o.to_string()
                    ],
                )?;
            }
            for id in &b.remove_ops {
                tx.execute("DELETE FROM ops WHERE id = ?1", params![id])?;
            }
            tx.commit()?;
        }
        if b.convs.is_empty()
            && b.put_messages.is_empty()
            && b.remove_messages.is_empty()
            && b.meta.is_none()
        {
            return Ok(());
        }
        let tx = self.cache.transaction()?;
        if let Some(m) = &b.meta {
            tx.execute(
                "INSERT OR REPLACE INTO meta (k, v) VALUES ('account', ?1)",
                params![m["account"].as_str().unwrap_or("null")],
            )?;
        }
        for c in &b.convs {
            tx.execute(
                "INSERT OR REPLACE INTO convs (conv_id, body) VALUES (?1, ?2)",
                params![c.conv_id, serde_json::to_string(c).unwrap_or_default()],
            )?;
        }
        for m in &b.put_messages {
            tx.execute(
                "INSERT OR REPLACE INTO messages (conv_id, msg_id, seq, body) VALUES (?1, ?2, ?3, ?4)",
                params![
                    m["conv_id"].as_str().unwrap_or(""),
                    m["msg_id"].as_str().unwrap_or(""),
                    m["created_seq"].as_i64().unwrap_or(0),
                    m.to_string()
                ],
            )?;
        }
        for (c, k) in &b.remove_messages {
            tx.execute(
                "DELETE FROM messages WHERE conv_id = ?1 AND msg_id = ?2",
                params![c, k],
            )?;
        }
        // An explicit rollback on error is done by dropping `tx`.
        tx.commit()
    }

    /// Empties everything (log out, another account, a damaged cache).
    pub fn wipe(&mut self) -> rusqlite::Result<()> {
        self.cache.execute_batch(
            "DELETE FROM meta; DELETE FROM convs; DELETE FROM messages; PRAGMA incremental_vacuum;",
        )?;
        self.queue
            .execute_batch("DELETE FROM ops; PRAGMA incremental_vacuum;")?;
        Ok(())
    }

    /// Gives free pages back to the system, a bit at a time.
    pub fn vacuum_some(&self) {
        let _ = self
            .cache
            .execute_batch("PRAGMA incremental_vacuum(256); PRAGMA wal_checkpoint(TRUNCATE);");
    }
}
