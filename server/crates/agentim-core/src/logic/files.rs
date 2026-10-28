//! Files and images, write side (docs/02-architecture.md §2.7): who may
//! upload, the open upload's addresses, a file taken by exactly one
//! message, who may read it, and what is thrown away.

use serde_json::{Value, json};

use super::{Ctx, err, not_silenced};
use crate::db::FileRow;
use crate::filestore as fs;
use crate::model::*;

/// Message kinds that carry a file.
pub const KINDS: [&str; 2] = ["image", "file"];
/// Open uploads expire after this long; files no message took after this.
pub const OPEN_MS: i64 = 24 * 3_600_000;
pub const UNUSED_MS: i64 = 24 * 3_600_000;
/// Longest caption.
const CAPTION_MAX: usize = 4096;
/// Files thrown away per sweep.
const SWEEP_BATCH: i64 = 200;

/// `payload` of an image or file message as the client sends it.
pub fn check_payload(kind: &str, payload: &Option<Value>) -> ApiResult<()> {
    let p = payload
        .as_ref()
        .and_then(|p| p.as_object())
        .ok_or_else(|| {
            err(
                "bad_request",
                format!("a {kind} message needs payload {{file_id}}"),
            )
        })?;
    if !p.get("file_id").is_some_and(|f| f.is_string()) {
        return Err(err("bad_request", "payload.file_id is required"));
    }
    if let Some(c) = p.get("caption") {
        let ok = c.as_str().is_some_and(|c| c.len() <= CAPTION_MAX);
        if !ok {
            return Err(err(
                "bad_request",
                "payload.caption is text of at most 4096 bytes",
            ));
        }
    }
    if p.keys().any(|k| k != "file_id" && k != "caption") {
        return Err(err("bad_request", "payload takes file_id and caption"));
    }
    Ok(())
}

/// What messages show of a file.
pub fn file_view(f: &FileRow) -> Value {
    json!({
        "file_id": f.file_id,
        "name": f.name,
        "size": f.size,
        "mime": f.mime,
        "kind": f.kind,
        "width": f.width,
        "height": f.height,
        "thumb": f.thumb_size > 0,
        "ai_generated": f.ai,
    })
}

impl Ctx<'_, '_> {
    /// May `me` put something into this conversation now (as for sending).
    async fn may_post(&mut self, conv_id: &str, me: &User) -> ApiResult<()> {
        self.own_conv(conv_id)?;
        if self.sh.deactivated.read().unwrap().contains(&me.id) {
            return Err(err("forbidden", "the account is deactivated"));
        }
        let member = self.require_member(conv_id, &me.id).await?;
        not_silenced(&member)?;
        self.check_dm_block(conv_id, &me.id).await?;
        Ok(())
    }

    /// A new upload: checks and records it, then gives its addresses.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn file_start(
        &mut self,
        me: &User,
        conv_id: &str,
        file_id: &str,
        name: &str,
        size: u64,
        mime: Option<&str>,
        thumb_size: Option<u64>,
        upload_id: Option<String>,
    ) -> ApiResult<Value> {
        self.may_post(conv_id, me).await?;
        let cfg = &self.sh.files_cfg;
        if size == 0 {
            return Err(err("bad_request", "the file is empty"));
        }
        if size > cfg.max_bytes {
            return Err(err(
                "bad_request",
                format!(
                    "files are at most {} bytes (files.max_bytes)",
                    cfg.max_bytes
                ),
            ));
        }
        let thumb = thumb_size.unwrap_or(0);
        if thumb > fs::THUMB_MAX_BYTES {
            return Err(err(
                "bad_request",
                format!("a thumbnail is at most {} bytes", fs::THUMB_MAX_BYTES),
            ));
        }
        let (open, today) = self.db.file_usage(&me.id, now_ms() - 86_400_000).await?;
        if open >= cfg.max_open_per_user as i64 {
            return Err(err(
                "rate_limited",
                format!("at most {} uploads open at once", cfg.max_open_per_user),
            ));
        }
        if cfg.daily_bytes_per_user > 0 && today as u64 + size > cfg.daily_bytes_per_user {
            return Err(err(
                "rate_limited",
                "the daily upload allowance is used up (files.daily_bytes_per_user)",
            ));
        }
        let now = now_ms();
        let row = FileRow {
            file_id: file_id.to_string(),
            conv_id: conv_id.to_string(),
            uploader_id: me.id.clone(),
            name: fs::clean_name(name),
            size: size as i64,
            mime: fs::clean_mime(mime),
            kind: "file".into(),
            width: None,
            height: None,
            thumb_size: thumb as i64,
            thumb_mime: None,
            state: "pending".into(),
            upload_id,
            part_size: fs::PART_BYTES as i64,
            ref_id: None,
            ai: me.kind == UserKind::Agent,
            created_at: now,
            ready_at: None,
        };
        self.db.file_insert(&row).await?;
        Ok(self.upload_answer(&row, 1, true))
    }

    fn upload_answer(&self, f: &FileRow, from: u32, with_thumb: bool) -> Value {
        let st = &self.sh.files;
        let parts = st.part_targets(
            &f.conv_id,
            &f.file_id,
            f.upload_id.as_deref(),
            f.size as u64,
            f.part_size as u64,
            from,
            now_ms(),
        );
        let total = fs::parts_of(f.size as u64, f.part_size as u64);
        let mut v = json!({
            "file_id": f.file_id,
            "part_size": f.part_size,
            "parts_total": total,
            "parts": parts,
            "expires_at": now_ms() + fs::UPLOAD_TTL_MS,
            "direct": st.direct(),
        });
        if with_thumb && f.thumb_size > 0 {
            v["thumb"] =
                json!(st.thumb_target(&f.conv_id, &f.file_id, f.thumb_size as u64, now_ms()));
        }
        v
    }

    /// The uploader's open upload in this conversation.
    async fn open_upload(&mut self, me: &User, conv_id: &str, file_id: &str) -> ApiResult<FileRow> {
        self.own_conv(conv_id)?;
        match self.db.file(file_id).await? {
            Some(f) if f.conv_id == conv_id && f.uploader_id == me.id => Ok(f),
            Some(f) if f.conv_id == conv_id => Err(err("forbidden", "another member's upload")),
            _ => Err(err("not_found", "no such upload")),
        }
    }

    pub(super) async fn upload_parts(
        &mut self,
        me: &User,
        conv_id: &str,
        file_id: &str,
        from: Option<u32>,
    ) -> ApiResult<Value> {
        let f = self.open_upload(me, conv_id, file_id).await?;
        if f.state != "pending" {
            return Err(err("conflict", "the upload is complete"));
        }
        let total = fs::parts_of(f.size as u64, f.part_size as u64);
        let from = from.unwrap_or(1);
        if from == 0 || from > total {
            return Err(err("bad_request", format!("parts are 1..={total}")));
        }
        Ok(self.upload_answer(&f, from, from == 1))
    }

    /// The upload as the hub needs it to check the stored objects.
    pub(super) async fn upload_check(
        &mut self,
        me: &User,
        conv_id: &str,
        file_id: &str,
    ) -> ApiResult<Value> {
        let f = self.open_upload(me, conv_id, file_id).await?;
        Ok(json!(f))
    }

    /// Checked by the hub: the file is usable. Repeated, the same answer.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn file_ready(
        &mut self,
        me: &User,
        conv_id: &str,
        file_id: &str,
        mime: &str,
        kind: &str,
        size: Option<(i64, i64)>,
        thumb: Option<(i64, String)>,
    ) -> ApiResult<Value> {
        let f = self.open_upload(me, conv_id, file_id).await?;
        if f.state == "pending" {
            let t = thumb.as_ref().map(|(n, m)| (*n, m.as_str()));
            self.db
                .file_ready(file_id, mime, kind, size.map(|s| s.0), size.map(|s| s.1), t)
                .await?;
        }
        let f = self
            .db
            .file(file_id)
            .await?
            .ok_or_else(|| err("not_found", "no such upload"))?;
        Ok(file_view(&f))
    }

    /// An upload that failed its checks: thrown away.
    pub(super) async fn file_discard(&mut self, conv_id: &str, file_id: &str) -> ApiResult<Value> {
        if let Some(f) = self.db.file(file_id).await?
            && f.conv_id == conv_id
            && f.state == "pending"
        {
            self.db.file_trash(&f).await?;
        }
        Ok(json!({}))
    }

    /// The file a new message (or draft) carries: one of `me`'s, of this
    /// conversation, ready and taken by nothing else. Returns the payload
    /// the message stores: what the client gave, plus the file's details.
    pub(super) async fn attach(
        &mut self,
        conv_id: &str,
        me: &User,
        kind: &str,
        payload: &Value,
        ref_id: &str,
    ) -> ApiResult<Value> {
        let file_id = payload["file_id"].as_str().unwrap_or_default();
        let f = match self.db.file(file_id).await? {
            Some(f) if f.conv_id == conv_id => f,
            Some(_) => return Err(err("forbidden", "the file belongs to another conversation")),
            None => return Err(err("not_found", "no such file (expired or never uploaded)")),
        };
        if f.uploader_id != me.id {
            return Err(err("forbidden", "another member's file"));
        }
        if f.state != "ready" {
            return Err(err("not_found", "the upload is not complete"));
        }
        if kind == "image" && f.kind != "image" {
            return Err(err(
                "bad_request",
                "the file is not an image; send it as kind file",
            ));
        }
        if !self.db.file_take(file_id, ref_id, None).await? {
            return Err(err("conflict", "another message already carries this file"));
        }
        let mut p = file_view(&f);
        if let Some(c) = payload.get("caption") {
            p["caption"] = c.clone();
        }
        Ok(p)
    }

    /// A draft became a message: its file goes with it.
    pub(super) async fn reattach(
        &mut self,
        payload: &Option<Value>,
        draft_id: &str,
        msg_id: &str,
    ) -> ApiResult<()> {
        if let Some(fid) = payload.as_ref().and_then(|p| p["file_id"].as_str()) {
            self.db.file_take(fid, msg_id, Some(draft_id)).await?;
        }
        Ok(())
    }

    /// Files a recalled message, a discarded draft or similar carried go.
    pub(super) async fn trash_ref(&mut self, ref_id: &str) -> ApiResult<()> {
        for f in self.db.files_by_ref(ref_id).await? {
            self.db.file_trash(&f).await?;
        }
        Ok(())
    }

    /// Every file of a conversation goes (dissolved).
    pub(super) async fn trash_conv(&mut self, conv_id: &str) -> ApiResult<()> {
        for f in self.db.files_of_conv(conv_id).await? {
            self.db.file_trash(&f).await?;
        }
        Ok(())
    }

    /// The files one person uploaded to a conversation go (account deleted).
    pub(crate) async fn trash_uploader(
        &mut self,
        conv_id: &str,
        user_id: &str,
    ) -> ApiResult<usize> {
        let files = self.db.files_of_uploader(conv_id, user_id).await?;
        for f in &files {
            self.db.file_trash(f).await?;
        }
        Ok(files.len())
    }

    /// May `me` read this file now? Then a short-lived address for it.
    pub(super) async fn file_url(
        &mut self,
        me: &User,
        conv_id: &str,
        file_id: &str,
        variant: Option<&str>,
    ) -> ApiResult<Value> {
        self.own_conv(conv_id)?;
        let variant = variant.unwrap_or("original");
        if variant != "original" && variant != "thumb" {
            return Err(err("bad_request", "variant is thumb or original"));
        }
        let f = match self.db.file(file_id).await? {
            Some(f) if f.conv_id == conv_id && f.state == "ready" => f,
            _ => return Err(err("not_found", "no such file")),
        };
        if !self.may_read_file(me, &f).await? {
            return Err(err(
                "forbidden",
                "you cannot see the message with this file",
            ));
        }
        let (variant, mime) = if variant == "thumb" && f.thumb_size > 0 {
            (
                "thumb",
                f.thumb_mime.clone().unwrap_or_else(|| "image/jpeg".into()),
            )
        } else {
            ("original", f.mime.clone())
        };
        let attachment = f.kind != "image";
        let url = self.sh.files.download_url(
            conv_id,
            file_id,
            variant,
            &f.name,
            &mime,
            attachment,
            now_ms(),
        );
        Ok(
            json!({"url": url, "variant": variant, "expires_at": now_ms() + fs::DOWNLOAD_TTL_MS, "file": file_view(&f)}),
        )
    }

    async fn may_read_file(&mut self, me: &User, f: &FileRow) -> ApiResult<bool> {
        if me.kind == UserKind::System || f.uploader_id == me.id {
            return Ok(true);
        }
        let Some(r) = f.ref_id.as_deref() else {
            return Ok(false);
        };
        let Ok(member) = self.require_member(&f.conv_id, &me.id).await else {
            return Ok(false);
        };
        if r.starts_with("d_") {
            // A draft before approval: its approvers only.
            let Some(agent) = self.sh.user(&f.uploader_id) else {
                return Ok(false);
            };
            return Ok(self.approvers(&f.conv_id, &agent).await?.contains(&me.id));
        }
        match self.db.message(r).await? {
            Some(m) => Ok(m.state != "recalled" && m.created_seq >= member.visible_from),
            None => Ok(false),
        }
    }

    /// Throws away open uploads past a day, files no message took within a
    /// day, and files past the retention (maintenance writer).
    pub(super) async fn file_sweep(&mut self, now: i64) -> ApiResult<Value> {
        let keep = self.sh.files_cfg.retention_days;
        let kept_before = (keep > 0).then(|| now - keep as i64 * 86_400_000);
        let gone = self
            .db
            .files_expired(now - OPEN_MS, now - UNUSED_MS, kept_before, SWEEP_BATCH)
            .await?;
        for f in &gone {
            self.db.file_trash(f).await?;
        }
        Ok(json!({"trashed": gone.len()}))
    }
}
