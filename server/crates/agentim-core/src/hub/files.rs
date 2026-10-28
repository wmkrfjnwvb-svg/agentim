//! Files and images (docs/02-architecture.md §2.7): what needs the store.
//! The writer decides who may upload and read; here the hub starts S3
//! multipart uploads, checks what was uploaded, and deletes thrown away
//! objects in the background.

use serde_json::{Value, json};

use super::{Hub, err};
use crate::db::FileRow;
use crate::filestore as fs;
use crate::logic::Op;
use crate::model::*;

/// Bytes read from the start of an upload to tell its type and size.
const HEAD_BYTES: u64 = 256 << 10;
/// Objects deleted per database per round, and the longest wait between tries.
const TRASH_BATCH: i64 = 50;
const TRASH_BACKOFF_MAX_MS: i64 = 3_600_000;
/// Largest file the server takes in one request (agents, MCP, backends).
pub const INGEST_MAX: usize = 64 << 20;

impl Hub {
    /// At start: the key of the server's own file addresses (shared by
    /// every node), and for S3, a probe that the bucket can be written.
    pub(crate) async fn open_files(&self) -> ApiResult<()> {
        use base64::Engine;
        let candidate = base64::engine::general_purpose::STANDARD.encode(
            ring::rand::generate::<[u8; 32]>(&ring::rand::SystemRandom::new())
                .map_err(|_| err("internal", "random"))?
                .expose(),
        );
        let v = self.control().call(Op::FilesKey { candidate }).await?;
        let key = base64::engine::general_purpose::STANDARD
            .decode(v["key"].as_str().unwrap_or_default())
            .map_err(|_| err("internal", "files_url_key is not base64"))?;
        self.inner.sh.files.set_url_key(key);
        if !self.inner.sh.files_cfg.usable(self.clustered()) {
            tracing::warn!(
                "files are off: a cluster needs files.storage = \"s3\" or a shared folder (files.shared = true)"
            );
        }
        if self.inner.sh.files.direct() {
            self.inner
                .sh
                .files
                .probe()
                .await
                .map_err(|e| err("config", e))?;
        }
        Ok(())
    }

    pub fn file_store(&self) -> &fs::Store {
        &self.inner.sh.files
    }

    pub(crate) async fn upload_start(
        &self,
        me: &User,
        conv_id: &str,
        name: &str,
        size: u64,
        mime: Option<String>,
        thumb_size: Option<u64>,
    ) -> ApiResult<Value> {
        let st = self.inner.sh.files.clone();
        let file_id = format!("f_{}", crate::db::new_id());
        let shard = self.conv_shard(conv_id)?;
        // Large S3 uploads go in parts: the store gives the upload an id
        // first. Refusals below abort it again.
        let max = self.inner.sh.files_cfg.max_bytes;
        let upload_id = if st.direct() && size > fs::SINGLE_PUT_MAX && size <= max {
            Some(
                st.create_multipart(conv_id, &file_id, &fs::clean_mime(mime.as_deref()))
                    .await?,
            )
        } else {
            None
        };
        let r = shard
            .call(Op::FileStart {
                conv_id: conv_id.to_string(),
                user: me.clone(),
                file_id: file_id.clone(),
                name: name.to_string(),
                size,
                mime,
                thumb_size,
                upload_id: upload_id.clone(),
            })
            .await;
        if r.is_err()
            && let Some(id) = upload_id.as_deref()
        {
            let _ = st.delete(conv_id, &file_id, Some(id)).await;
        }
        r
    }

    /// The client says every part is up: the store must have exactly that
    /// file, of the declared size; its type is told from its first bytes
    /// and an image's thumbnail must be a small JPEG or PNG. Then it is
    /// usable. Completed again, the same answer.
    pub(crate) async fn upload_complete(
        &self,
        me: &User,
        conv_id: &str,
        file_id: &str,
        parts: &[PartTag],
    ) -> ApiResult<Value> {
        let shard = self.conv_shard(conv_id)?;
        let v = shard
            .call(Op::FileCheck {
                conv_id: conv_id.to_string(),
                user: me.clone(),
                file_id: file_id.to_string(),
            })
            .await?;
        let f: FileRow = serde_json::from_value(v).map_err(|e| err("internal", e.to_string()))?;
        let ready = |mime: String, kind: String, size, thumb| Op::FileReady {
            conv_id: conv_id.to_string(),
            user: me.clone(),
            file_id: file_id.to_string(),
            mime,
            kind,
            size,
            thumb,
        };
        if f.state == "ready" {
            return shard
                .call(ready(f.mime.clone(), f.kind.clone(), None, None))
                .await;
        }
        match self.check_upload(&f, parts).await {
            Ok((mime, kind, size, thumb)) => shard.call(ready(mime, kind, size, thumb)).await,
            Err(e) if e.code == "bad_request" => {
                // Not what was declared: thrown away (the client starts over).
                shard
                    .call(Op::FileDiscard {
                        conv_id: conv_id.to_string(),
                        file_id: file_id.to_string(),
                    })
                    .await?;
                Err(e)
            }
            // The store failed: the upload stays, the client completes again.
            Err(e) => Err(e),
        }
    }

    #[allow(clippy::type_complexity)]
    async fn check_upload(
        &self,
        f: &FileRow,
        parts: &[PartTag],
    ) -> ApiResult<(String, String, Option<(i64, i64)>, Option<(i64, String)>)> {
        let st = &self.inner.sh.files;
        let tags: Vec<(u32, String)> = parts.iter().map(|p| (p.n, p.etag.clone())).collect();
        st.complete(
            &f.conv_id,
            &f.file_id,
            f.upload_id.as_deref(),
            &tags,
            f.size as u64,
            f.part_size as u64,
        )
        .await?;
        let head = st
            .head_bytes(&f.conv_id, &f.file_id, "original", HEAD_BYTES)
            .await?;
        let (mime, kind, size) = match fs::sniff_image(&head) {
            Some(m) => match fs::image_size(&head) {
                Some((w, h)) => (m.to_string(), "image", Some((w as i64, h as i64))),
                // An image whose size cannot be read: shown as a file.
                None => (m.to_string(), "file", None),
            },
            // Not an image, whatever it was called.
            None if f.mime.starts_with("image/") => {
                ("application/octet-stream".to_string(), "file", None)
            }
            None => (f.mime.clone(), "file", None),
        };
        let thumb = if f.thumb_size > 0 {
            if kind != "image" {
                return Err(err("bad_request", "a thumbnail is for images"));
            }
            let have = st.size(&f.conv_id, &f.file_id, "thumb").await?;
            if have != Some(f.thumb_size as u64) {
                return Err(err(
                    "bad_request",
                    "the thumbnail was not uploaded with its declared size",
                ));
            }
            let t = st
                .head_bytes(&f.conv_id, &f.file_id, "thumb", fs::THUMB_MAX_BYTES)
                .await?;
            let tm = match fs::sniff_image(&t) {
                Some(m @ ("image/jpeg" | "image/png")) => m,
                _ => return Err(err("bad_request", "a thumbnail is a JPEG or PNG")),
            };
            match fs::image_size(&t) {
                Some((w, h)) if w.max(h) <= fs::THUMB_SIDE && w > 0 && h > 0 => {}
                _ => {
                    return Err(err(
                        "bad_request",
                        format!(
                            "a thumbnail is at most {} pixels on its longest side",
                            fs::THUMB_SIDE
                        ),
                    ));
                }
            }
            Some((f.thumb_size, tm.to_string()))
        } else {
            None
        };
        Ok((mime, kind.to_string(), size, thumb))
    }

    /// Local folder: whether a part may still be written (the upload is open).
    pub async fn upload_open(&self, conv_id: &str, file_id: &str) -> bool {
        let Ok(shard) = self.conv_shard(conv_id) else {
            return false;
        };
        let id = file_id.to_string();
        let f = self
            .read(shard, move |db| Box::pin(async move { db.file(&id).await }))
            .await;
        matches!(f, Ok(Some(f)) if f.conv_id == conv_id && f.state == "pending")
    }

    /// Background: throws away expired uploads and files, then deletes the
    /// objects of thrown away files (retrying with backoff until gone).
    pub async fn files_upkeep(&self) -> ApiResult<usize> {
        let now = now_ms();
        let st = self.inner.sh.files.clone();
        let mut deleted = 0;
        for shard in self.inner.shards.iter() {
            shard.call(Op::FileSweep { now }).await?;
            let due = self
                .read(shard, move |db| {
                    Box::pin(async move { db.trash_due(now, TRASH_BATCH).await })
                })
                .await?;
            for t in due {
                let retry_at = match st
                    .delete(&t.conv_id, &t.file_id, t.upload_id.as_deref())
                    .await
                {
                    Ok(()) => {
                        deleted += 1;
                        None
                    }
                    Err(_) => Some(now + (1000i64 << t.attempts.min(20)).min(TRASH_BACKOFF_MAX_MS)),
                };
                shard
                    .call(Op::TrashDone {
                        file_id: t.file_id,
                        retry_at,
                    })
                    .await?;
            }
        }
        Ok(deleted)
    }

    /// A whole file the server received itself (agents, MCP, a backend):
    /// its EXIF and GPS removed, a thumbnail made, stored and checked like
    /// any upload. At most `INGEST_MAX` bytes.
    pub async fn ingest_file(
        &self,
        me: &User,
        conv_id: &str,
        name: &str,
        mime: Option<String>,
        data: Vec<u8>,
    ) -> ApiResult<Value> {
        if data.len() > INGEST_MAX {
            return Err(err(
                "bad_request",
                format!("send files over {INGEST_MAX} bytes in parts (upload_start)"),
            ));
        }
        let data = crate::media::strip_metadata(&data).unwrap_or(data);
        let thumb = fs::make_thumbnail(&data);
        let started = self
            .upload_start(
                me,
                conv_id,
                name,
                data.len() as u64,
                mime.clone(),
                thumb.as_ref().map(|t| t.0.len() as u64),
            )
            .await?;
        let file_id = started["file_id"].as_str().unwrap_or_default().to_string();
        let st = self.inner.sh.files.clone();
        let m = fs::clean_mime(mime.as_deref());
        let stored = async {
            st.put_object(conv_id, &file_id, "original", data, &m)
                .await?;
            if let Some((t, tm)) = thumb {
                st.put_object(conv_id, &file_id, "thumb", t, tm).await?;
            }
            Ok::<_, ApiError>(())
        }
        .await;
        if let Err(e) = stored {
            let shard = self.conv_shard(conv_id)?;
            shard
                .call(Op::FileDiscard {
                    conv_id: conv_id.to_string(),
                    file_id: file_id.clone(),
                })
                .await?;
            return Err(e);
        }
        // The server wrote the whole object: S3 multipart is not used here.
        self.upload_complete(me, conv_id, &file_id, &[]).await
    }

    /// For the operator: where files go.
    pub fn files_info(&self) -> Value {
        let c = &self.inner.sh.files_cfg;
        let mut v = self.inner.sh.files.describe();
        v["max_bytes"] = json!(c.max_bytes);
        v["retention_days"] = json!(c.retention_days);
        v
    }
}
