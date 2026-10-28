//! Files and the objects waiting to be deleted (docs/02-architecture.md §2.7).

use serde::{Deserialize, Serialize};
use sqlx::Row;

use super::Db;
use crate::model::*;

/// A file of a conversation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FileRow {
    pub file_id: String,
    pub conv_id: String,
    pub uploader_id: String,
    pub name: String,
    pub size: i64,
    pub mime: String,
    /// `image` or `file`.
    pub kind: String,
    pub width: Option<i64>,
    pub height: Option<i64>,
    /// Bytes of the thumbnail; 0: none.
    pub thumb_size: i64,
    pub thumb_mime: Option<String>,
    /// `pending` while uploading, `ready` once checked.
    pub state: String,
    /// S3 multipart upload in progress.
    pub upload_id: Option<String>,
    pub part_size: i64,
    /// The message (or draft) that carries it.
    pub ref_id: Option<String>,
    /// Uploaded by an agent (EU AI Act art. 50).
    pub ai: bool,
    pub created_at: i64,
    pub ready_at: Option<i64>,
}

const COLS: &str = "file_id, conv_id, uploader_id, name, size, mime, kind, width, height, thumb_size, thumb_mime, state, upload_id, part_size, ref_id, ai, created_at, ready_at";

fn row(r: &sqlx::any::AnyRow) -> ApiResult<FileRow> {
    Ok(FileRow {
        file_id: r.try_get(0)?,
        conv_id: r.try_get(1)?,
        uploader_id: r.try_get(2)?,
        name: r.try_get(3)?,
        size: r.try_get(4)?,
        mime: r.try_get(5)?,
        kind: r.try_get(6)?,
        width: r.try_get(7)?,
        height: r.try_get(8)?,
        thumb_size: r.try_get(9)?,
        thumb_mime: r.try_get(10)?,
        state: r.try_get(11)?,
        upload_id: r.try_get(12)?,
        part_size: r.try_get(13)?,
        ref_id: r.try_get(14)?,
        ai: r.try_get::<i64, _>(15)? != 0,
        created_at: r.try_get(16)?,
        ready_at: r.try_get(17)?,
    })
}

/// An object to delete, retried until it is gone.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TrashRow {
    pub file_id: String,
    pub conv_id: String,
    pub upload_id: Option<String>,
    pub attempts: i64,
}

impl Db<'_> {
    pub async fn file_insert(&mut self, f: &FileRow) -> ApiResult<()> {
        sqlx::query(self.d.q(&format!(
            "INSERT INTO files ({COLS}) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
        )))
        .bind(&f.file_id)
        .bind(&f.conv_id)
        .bind(&f.uploader_id)
        .bind(&f.name)
        .bind(f.size)
        .bind(&f.mime)
        .bind(&f.kind)
        .bind(f.width)
        .bind(f.height)
        .bind(f.thumb_size)
        .bind(f.thumb_mime.clone())
        .bind(&f.state)
        .bind(f.upload_id.clone())
        .bind(f.part_size)
        .bind(f.ref_id.clone())
        .bind(f.ai as i64)
        .bind(f.created_at)
        .bind(f.ready_at)
        .execute(&mut *self.c)
        .await?;
        Ok(())
    }

    pub async fn file(&mut self, file_id: &str) -> ApiResult<Option<FileRow>> {
        let r = sqlx::query(
            self.d
                .q(&format!("SELECT {COLS} FROM files WHERE file_id = ?")),
        )
        .bind(file_id)
        .fetch_optional(&mut *self.c)
        .await?;
        r.as_ref().map(row).transpose()
    }

    /// Checked: the type and size as stored.
    pub async fn file_ready(
        &mut self,
        file_id: &str,
        mime: &str,
        kind: &str,
        width: Option<i64>,
        height: Option<i64>,
        thumb: Option<(i64, &str)>,
    ) -> ApiResult<()> {
        sqlx::query(self.d.q(
            "UPDATE files SET state = 'ready', mime = ?, kind = ?, width = ?, height = ?, thumb_size = ?, thumb_mime = ?, upload_id = NULL, ready_at = ? WHERE file_id = ?",
        ))
        .bind(mime)
        .bind(kind)
        .bind(width)
        .bind(height)
        .bind(thumb.map_or(0, |t| t.0))
        .bind(thumb.map(|t| t.1.to_string()))
        .bind(now_ms())
        .bind(file_id)
        .execute(&mut *self.c)
        .await?;
        Ok(())
    }

    /// Ties the file to a message or draft; false when something else has it.
    pub async fn file_take(
        &mut self,
        file_id: &str,
        ref_id: &str,
        from: Option<&str>,
    ) -> ApiResult<bool> {
        let n = match from {
            None => sqlx::query(self.d.q(
                "UPDATE files SET ref_id = ? WHERE file_id = ? AND ref_id IS NULL AND state = 'ready'",
            ))
            .bind(ref_id)
            .bind(file_id)
            .execute(&mut *self.c)
            .await?
            .rows_affected(),
            Some(f) => sqlx::query(self.d.q("UPDATE files SET ref_id = ? WHERE file_id = ? AND ref_id = ?"))
                .bind(ref_id)
                .bind(file_id)
                .bind(f)
                .execute(&mut *self.c)
                .await?
                .rows_affected(),
        };
        Ok(n == 1)
    }

    /// Lets go of a file a message did not end up taking.
    pub async fn file_release(&mut self, file_id: &str, from: &str) -> ApiResult<()> {
        sqlx::query(
            self.d
                .q("UPDATE files SET ref_id = NULL WHERE file_id = ? AND ref_id = ?"),
        )
        .bind(file_id)
        .bind(from)
        .execute(&mut *self.c)
        .await?;
        Ok(())
    }

    pub async fn files_by_ref(&mut self, ref_id: &str) -> ApiResult<Vec<FileRow>> {
        let rows = sqlx::query(
            self.d
                .q(&format!("SELECT {COLS} FROM files WHERE ref_id = ?")),
        )
        .bind(ref_id)
        .fetch_all(&mut *self.c)
        .await?;
        rows.iter().map(row).collect()
    }

    /// Files carried by messages of a conversation from before `seq`.
    pub async fn files_of_messages_before(
        &mut self,
        conv_id: &str,
        seq: i64,
    ) -> ApiResult<Vec<FileRow>> {
        let rows = sqlx::query(self.d.q(&format!(
            "SELECT {COLS} FROM files WHERE conv_id = ? AND ref_id IN (SELECT msg_id FROM messages WHERE conv_id = ? AND created_seq < ?)"
        )))
        .bind(conv_id)
        .bind(conv_id)
        .bind(seq)
        .fetch_all(&mut *self.c)
        .await?;
        rows.iter().map(row).collect()
    }

    pub async fn files_of_conv(&mut self, conv_id: &str) -> ApiResult<Vec<FileRow>> {
        let rows = sqlx::query(
            self.d
                .q(&format!("SELECT {COLS} FROM files WHERE conv_id = ?")),
        )
        .bind(conv_id)
        .fetch_all(&mut *self.c)
        .await?;
        rows.iter().map(row).collect()
    }

    pub async fn files_of_uploader(
        &mut self,
        conv_id: &str,
        user_id: &str,
    ) -> ApiResult<Vec<FileRow>> {
        let rows = sqlx::query(self.d.q(&format!(
            "SELECT {COLS} FROM files WHERE conv_id = ? AND uploader_id = ?"
        )))
        .bind(conv_id)
        .bind(user_id)
        .fetch_all(&mut *self.c)
        .await?;
        rows.iter().map(row).collect()
    }

    /// Uploads of a user still open, and bytes they started since `since`
    /// (this database).
    pub async fn file_usage(&mut self, user_id: &str, since: i64) -> ApiResult<(i64, i64)> {
        let open: i64 = sqlx::query(
            self.d
                .q("SELECT COUNT(*) FROM files WHERE uploader_id = ? AND state = 'pending'"),
        )
        .bind(user_id)
        .fetch_one(&mut *self.c)
        .await?
        .try_get(0)?;
        // Summed here: SUM's type differs between the databases.
        let bytes: i64 = sqlx::query(
            self.d
                .q("SELECT size FROM files WHERE uploader_id = ? AND created_at >= ?"),
        )
        .bind(user_id)
        .bind(since)
        .fetch_all(&mut *self.c)
        .await?
        .iter()
        .map(|r| r.try_get::<i64, _>(0).unwrap_or(0))
        .sum();
        Ok((open, bytes))
    }

    /// The file goes; its objects are deleted in the background.
    pub async fn file_trash(&mut self, f: &FileRow) -> ApiResult<()> {
        sqlx::query(self.d.q("DELETE FROM files WHERE file_id = ?"))
            .bind(&f.file_id)
            .execute(&mut *self.c)
            .await?;
        let have = sqlx::query(self.d.q("SELECT 1 FROM file_trash WHERE file_id = ?"))
            .bind(&f.file_id)
            .fetch_optional(&mut *self.c)
            .await?;
        if have.is_none() {
            sqlx::query(self.d.q(
                "INSERT INTO file_trash (file_id, conv_id, upload_id, attempts, next_at) VALUES (?, ?, ?, 0, 0)",
            ))
            .bind(&f.file_id)
            .bind(&f.conv_id)
            .bind(f.upload_id.clone())
            .execute(&mut *self.c)
            .await?;
        }
        Ok(())
    }

    /// Files to throw away now: uploads open too long, files no message
    /// took, files past the retention.
    pub async fn files_expired(
        &mut self,
        open_before: i64,
        unused_before: i64,
        kept_before: Option<i64>,
        limit: i64,
    ) -> ApiResult<Vec<FileRow>> {
        let mut out = vec![];
        let rows = sqlx::query(self.d.q(&format!(
            "SELECT {COLS} FROM files WHERE (state = 'pending' AND created_at < ?) OR (state = 'ready' AND ref_id IS NULL AND ready_at < ?) ORDER BY created_at LIMIT ?"
        )))
        .bind(open_before)
        .bind(unused_before)
        .bind(limit)
        .fetch_all(&mut *self.c)
        .await?;
        for r in &rows {
            out.push(row(r)?);
        }
        // Files of drafts that are gone (discarded with their agent).
        let rows = sqlx::query(self.d.q(&format!(
            "SELECT {COLS} FROM files WHERE ref_id LIKE 'd!_%' ESCAPE '!' AND NOT EXISTS (SELECT 1 FROM drafts WHERE drafts.draft_id = files.ref_id AND drafts.state = 'pending') LIMIT ?"
        )))
        .bind(limit)
        .fetch_all(&mut *self.c)
        .await?;
        for r in &rows {
            out.push(row(r)?);
        }
        if let Some(k) = kept_before {
            let rows = sqlx::query(self.d.q(&format!(
                "SELECT {COLS} FROM files WHERE created_at < ? ORDER BY created_at LIMIT ?"
            )))
            .bind(k)
            .bind(limit)
            .fetch_all(&mut *self.c)
            .await?;
            for r in &rows {
                let f = row(r)?;
                if !out.iter().any(|o| o.file_id == f.file_id) {
                    out.push(f);
                }
            }
        }
        Ok(out)
    }

    pub async fn trash_due(&mut self, now: i64, limit: i64) -> ApiResult<Vec<TrashRow>> {
        let rows = sqlx::query(self.d.q(
            "SELECT file_id, conv_id, upload_id, attempts FROM file_trash WHERE next_at <= ? ORDER BY next_at LIMIT ?",
        ))
        .bind(now)
        .bind(limit)
        .fetch_all(&mut *self.c)
        .await?;
        rows.iter()
            .map(|r| {
                Ok(TrashRow {
                    file_id: r.try_get(0)?,
                    conv_id: r.try_get(1)?,
                    upload_id: r.try_get(2)?,
                    attempts: r.try_get(3)?,
                })
            })
            .collect()
    }

    pub async fn trash_done(&mut self, file_id: &str) -> ApiResult<()> {
        sqlx::query(self.d.q("DELETE FROM file_trash WHERE file_id = ?"))
            .bind(file_id)
            .execute(&mut *self.c)
            .await?;
        Ok(())
    }

    pub async fn trash_retry(&mut self, file_id: &str, next_at: i64) -> ApiResult<()> {
        sqlx::query(
            self.d
                .q("UPDATE file_trash SET attempts = attempts + 1, next_at = ? WHERE file_id = ?"),
        )
        .bind(next_at)
        .bind(file_id)
        .execute(&mut *self.c)
        .await?;
        Ok(())
    }

    /// Text in `meta`, written once: the first value stays.
    pub async fn meta_once(&mut self, k: &str, v: &str) -> ApiResult<String> {
        let have = sqlx::query(self.d.q("SELECT v FROM meta WHERE k = ?"))
            .bind(k)
            .fetch_optional(&mut *self.c)
            .await?;
        if let Some(r) = have {
            return Ok(r.try_get(0)?);
        }
        sqlx::query(self.d.q("INSERT INTO meta (k, v) VALUES (?, ?)"))
            .bind(k)
            .bind(v)
            .execute(&mut *self.c)
            .await?;
        Ok(v.to_string())
    }
}
