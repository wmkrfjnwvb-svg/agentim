//! Where files live (docs/02-architecture.md §2.7): a local folder, written
//! through the server, or an S3 compatible store that clients write to
//! directly with presigned addresses. Both give the client the same thing:
//! a list of addresses to PUT the parts and the thumbnail to.
//!
//! Objects: `{prefix}{conv_id}/{file_id}/original` and `…/thumb`.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use rusty_s3::{Bucket, Credentials, S3Action, UrlStyle};
use serde::Serialize;
use serde_json::{Value, json};

use crate::model::{ApiError, ApiResult};

/// Size of each part a client uploads (the last one is shorter).
pub const PART_BYTES: u64 = 16 << 20;
/// Up to this size an S3 upload is a single PUT.
pub const SINGLE_PUT_MAX: u64 = 64 << 20;
/// Largest thumbnail, and its longest side.
pub const THUMB_MAX_BYTES: u64 = 512 << 10;
pub const THUMB_SIDE: u32 = 320;
/// Upload addresses are good this long, download addresses this long.
pub const UPLOAD_TTL_MS: i64 = 3_600_000;
pub const DOWNLOAD_TTL_MS: i64 = 300_000;
/// At most this many part addresses per answer (`upload_parts` for more).
pub const PARTS_PER_ANSWER: u32 = 64;

fn err(code: &'static str, m: impl Into<String>) -> ApiError {
    ApiError::new(code, m)
}

fn storage(e: impl std::fmt::Display) -> ApiError {
    tracing::warn!(error = %e, "file storage");
    err("storage", format!("file storage: {e}"))
}

/// One address the client PUTs to.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Target {
    /// The part number (from 1), or `thumb`.
    pub n: String,
    pub url: String,
    /// Headers to send with exactly these values.
    pub headers: BTreeMap<String, String>,
}

// One per server: the size difference does not matter.
#[allow(clippy::large_enum_variant)]
pub enum Backend {
    Local {
        dir: PathBuf,
    },
    S3 {
        bucket: Bucket,
        creds: Credentials,
        prefix: String,
        http: reqwest::Client,
    },
}

pub struct Store {
    pub backend: Backend,
    /// Signs the server's own upload and download addresses (local).
    url_key: std::sync::RwLock<Vec<u8>>,
}

pub fn key(prefix: &str, conv: &str, file: &str, variant: &str) -> String {
    format!("{prefix}{conv}/{file}/{variant}")
}

/// Number of parts of a file of `size` bytes.
pub fn parts_of(size: u64, part: u64) -> u32 {
    size.div_ceil(part).max(1) as u32
}

/// Bytes of part `n` (from 1).
pub fn part_len(size: u64, part: u64, n: u32) -> u64 {
    let total = parts_of(size, part);
    if n < total {
        part
    } else {
        size - part * (total as u64 - 1)
    }
}

/// `attachment` or `inline` with the name, ASCII fallback and UTF-8
/// (RFC 6266).
pub fn disposition(attachment: bool, name: &str) -> String {
    let ascii: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_graphic() && c != '"' && c != '\\' || c == ' ' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let mut enc = String::new();
    for b in name.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            enc.push(b as char);
        } else {
            enc.push_str(&format!("%{b:02X}"));
        }
    }
    format!(
        "{}; filename=\"{ascii}\"; filename*=UTF-8''{enc}",
        if attachment { "attachment" } else { "inline" }
    )
}

fn b64(b: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
}

fn enc(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

impl Store {
    pub fn from_config(
        cfg: &crate::config::FilesConfig,
        http: reqwest::Client,
    ) -> Result<Store, String> {
        let backend = match cfg.storage.as_str() {
            "s3" => {
                let s = &cfg.s3;
                let endpoint = s
                    .endpoint
                    .parse()
                    .map_err(|e| format!("files.s3.endpoint: {e}"))?;
                let style = if s.path_style {
                    UrlStyle::Path
                } else {
                    UrlStyle::VirtualHost
                };
                let region = if s.region.is_empty() {
                    "us-east-1".to_string()
                } else {
                    s.region.clone()
                };
                let bucket = Bucket::new(endpoint, style, s.bucket.clone(), region)
                    .map_err(|e| format!("files.s3: {e}"))?;
                let id = std::env::var("AGENTIM_S3_ACCESS_KEY_ID").unwrap_or_default();
                let secret = std::env::var("AGENTIM_S3_SECRET_ACCESS_KEY").unwrap_or_default();
                if id.is_empty() || secret.is_empty() {
                    return Err("files.storage = s3 needs AGENTIM_S3_ACCESS_KEY_ID and AGENTIM_S3_SECRET_ACCESS_KEY".into());
                }
                let mut prefix = s.prefix.trim_matches('/').to_string();
                if !prefix.is_empty() {
                    prefix.push('/');
                }
                Backend::S3 {
                    bucket,
                    creds: Credentials::new(id, secret),
                    prefix,
                    http,
                }
            }
            _ => Backend::Local {
                dir: PathBuf::from(&cfg.dir),
            },
        };
        Ok(Store {
            backend,
            url_key: std::sync::RwLock::new(vec![]),
        })
    }

    /// The key that signs the server's own addresses (shared by every
    /// node: it is kept in the control database).
    pub fn set_url_key(&self, k: Vec<u8>) {
        *self.url_key.write().unwrap() = k;
    }

    /// Whether clients upload straight to the store.
    pub fn direct(&self) -> bool {
        matches!(self.backend, Backend::S3 { .. })
    }

    fn sign(&self, msg: &str) -> String {
        let k = self.url_key.read().unwrap();
        let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, &k);
        b64(ring::hmac::sign(&key, msg.as_bytes()).as_ref())
    }

    /// Checks a signature of the server's own addresses (constant time).
    pub fn verify(&self, msg: &str, sig: &str, exp: i64, now: i64) -> bool {
        use base64::Engine;
        if exp < now {
            return false;
        }
        let Ok(tag) = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(sig) else {
            return false;
        };
        let k = self.url_key.read().unwrap();
        let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, &k);
        ring::hmac::verify(&key, msg.as_bytes(), &tag).is_ok()
    }

    /// What an upload address signs: the object, the part, its size and expiry.
    pub fn upload_msg(conv: &str, file: &str, part: &str, len: u64, exp: i64) -> String {
        format!("u\n{conv}\n{file}\n{part}\n{len}\n{exp}")
    }

    pub fn download_msg(
        conv: &str,
        file: &str,
        variant: &str,
        mime: &str,
        disp: &str,
        exp: i64,
    ) -> String {
        format!("d\n{conv}\n{file}\n{variant}\n{mime}\n{disp}\n{exp}")
    }

    /// Writes and deletes a small object: the store is usable.
    pub async fn probe(&self) -> Result<(), String> {
        match &self.backend {
            Backend::Local { dir } => {
                tokio::fs::create_dir_all(dir)
                    .await
                    .map_err(|e| format!("files.dir {}: {e}", dir.display()))?;
                let p = dir.join(".probe");
                tokio::fs::write(&p, b"ok")
                    .await
                    .map_err(|e| format!("files.dir {} is not writable: {e}", dir.display()))?;
                let _ = tokio::fs::remove_file(&p).await;
                Ok(())
            }
            Backend::S3 {
                bucket,
                creds,
                prefix,
                http,
            } => {
                let k = format!("{prefix}.probe");
                let url = bucket
                    .put_object(Some(creds), &k)
                    .sign(Duration::from_secs(60));
                let r = http
                    .put(url.as_str())
                    .body("ok")
                    .send()
                    .await
                    .map_err(|e| format!("files.s3: {e}"))?;
                if !r.status().is_success() {
                    return Err(format!(
                        "files.s3: writing a probe object answered {}",
                        r.status()
                    ));
                }
                let url = bucket
                    .delete_object(Some(creds), &k)
                    .sign(Duration::from_secs(60));
                let _ = http.delete(url.as_str()).send().await;
                Ok(())
            }
        }
    }

    /// S3, files above one PUT: starts a multipart upload.
    pub async fn create_multipart(&self, conv: &str, file: &str, mime: &str) -> ApiResult<String> {
        let Backend::S3 {
            bucket,
            creds,
            prefix,
            http,
        } = &self.backend
        else {
            return Err(err("internal", "multipart is for S3"));
        };
        let k = key(prefix, conv, file, "original");
        let mut a = bucket.create_multipart_upload(Some(creds), &k);
        a.headers_mut().insert("content-type", mime.to_string());
        let url = a.sign(Duration::from_secs(60));
        let r = http
            .post(url.as_str())
            .header("content-type", mime)
            .send()
            .await
            .map_err(storage)?;
        let status = r.status();
        let body = r.text().await.map_err(storage)?;
        if !status.is_success() {
            return Err(storage(format!("create multipart: {status} {body}")));
        }
        let resp =
            rusty_s3::actions::CreateMultipartUpload::parse_response(&body).map_err(storage)?;
        Ok(resp.upload_id().to_string())
    }

    /// Addresses of parts `from..from+count` (1-based) of a file of `size`.
    #[allow(clippy::too_many_arguments)]
    pub fn part_targets(
        &self,
        conv: &str,
        file: &str,
        upload_id: Option<&str>,
        size: u64,
        part: u64,
        from: u32,
        now: i64,
    ) -> Vec<Target> {
        let total = parts_of(size, part);
        let last = (from + PARTS_PER_ANSWER - 1).min(total);
        let exp = now + UPLOAD_TTL_MS;
        (from.max(1)..=last)
            .map(|n| {
                let len = part_len(size, part, n);
                self.target(conv, file, &n.to_string(), "original", upload_id, len, exp)
            })
            .collect()
    }

    pub fn thumb_target(&self, conv: &str, file: &str, len: u64, now: i64) -> Target {
        self.target(conv, file, "thumb", "thumb", None, len, now + UPLOAD_TTL_MS)
    }

    #[allow(clippy::too_many_arguments)]
    fn target(
        &self,
        conv: &str,
        file: &str,
        n: &str,
        variant: &str,
        upload_id: Option<&str>,
        len: u64,
        exp: i64,
    ) -> Target {
        let mut headers = BTreeMap::new();
        let url = match &self.backend {
            Backend::Local { .. } => {
                let sig = self.sign(&Self::upload_msg(conv, file, n, len, exp));
                format!("/files/u/{conv}/{file}/{n}?len={len}&exp={exp}&sig={sig}")
            }
            Backend::S3 {
                bucket,
                creds,
                prefix,
                ..
            } => {
                let k = key(prefix, conv, file, variant);
                let ttl = Duration::from_millis((exp - crate::model::now_ms()).max(1000) as u64);
                // The length is signed: the store refuses anything else.
                headers.insert("content-length".to_string(), len.to_string());
                match (upload_id, n.parse::<u16>()) {
                    (Some(id), Ok(p)) => {
                        let mut a = bucket.upload_part(Some(creds), &k, p, id);
                        a.headers_mut().insert("content-length", len.to_string());
                        a.sign(ttl).to_string()
                    }
                    _ => {
                        let mut a = bucket.put_object(Some(creds), &k);
                        a.headers_mut().insert("content-length", len.to_string());
                        a.sign(ttl).to_string()
                    }
                }
            }
        };
        Target {
            n: n.to_string(),
            url,
            headers,
        }
    }

    /// Joins the parts: S3 completes the multipart upload (a single PUT
    /// needs nothing), the local folder concatenates them. Then the stored
    /// size must be `size`. Done twice, the second finds it done.
    pub async fn complete(
        &self,
        conv: &str,
        file: &str,
        upload_id: Option<&str>,
        etags: &[(u32, String)],
        size: u64,
        part: u64,
    ) -> ApiResult<()> {
        let total = parts_of(size, part);
        match &self.backend {
            Backend::Local { dir } => {
                let base = dir.join(conv).join(file);
                let original = base.join("original");
                if tokio::fs::metadata(&original).await.is_err() {
                    let mut out = Vec::with_capacity(total as usize);
                    for n in 1..=total {
                        let p = base.join("parts").join(n.to_string());
                        let len = tokio::fs::metadata(&p).await.map(|m| m.len()).ok();
                        if len != Some(part_len(size, part, n)) {
                            return Err(err(
                                "bad_request",
                                format!("part {n} is missing or has the wrong size"),
                            ));
                        }
                        out.push(p);
                    }
                    let tmp = base.join("original.tmp");
                    let mut f = tokio::fs::File::create(&tmp).await.map_err(storage)?;
                    for p in &out {
                        let mut r = tokio::fs::File::open(p).await.map_err(storage)?;
                        tokio::io::copy(&mut r, &mut f).await.map_err(storage)?;
                    }
                    f.sync_all().await.map_err(storage)?;
                    drop(f);
                    tokio::fs::rename(&tmp, &original).await.map_err(storage)?;
                    let _ = tokio::fs::remove_dir_all(base.join("parts")).await;
                }
            }
            Backend::S3 {
                bucket,
                creds,
                prefix,
                http,
            } => {
                if let Some(id) = upload_id {
                    let mut tags: Vec<(u32, &str)> =
                        etags.iter().map(|(n, e)| (*n, e.as_str())).collect();
                    tags.sort_by_key(|t| t.0);
                    tags.dedup_by_key(|t| t.0);
                    if tags.len() != total as usize
                        || tags.iter().enumerate().any(|(i, t)| t.0 != i as u32 + 1)
                    {
                        return Err(err(
                            "bad_request",
                            format!("give the ETag of each of the {total} parts"),
                        ));
                    }
                    let k = key(prefix, conv, file, "original");
                    let a = bucket.complete_multipart_upload(
                        Some(creds),
                        &k,
                        id,
                        tags.iter().map(|t| t.1),
                    );
                    let url = a.sign(Duration::from_secs(60));
                    let body = a.body();
                    let r = http
                        .post(url.as_str())
                        .body(body)
                        .send()
                        .await
                        .map_err(storage)?;
                    let status = r.status();
                    let text = r.text().await.unwrap_or_default();
                    // Completed before (a retry): the object is there.
                    let done = status.as_u16() == 404
                        && self.size(conv, file, "original").await?.is_some();
                    if !(status.is_success() && !text.contains("<Error>")) && !done {
                        if status.is_client_error() || text.contains("<Error>") {
                            return Err(err(
                                "bad_request",
                                "the parts could not be joined (missing or wrong ETags)",
                            ));
                        }
                        return Err(storage(format!("complete multipart: {status}")));
                    }
                }
            }
        }
        match self.size(conv, file, "original").await? {
            Some(s) if s == size => Ok(()),
            Some(s) => Err(err(
                "bad_request",
                format!("the file has {s} bytes, not the {size} declared"),
            )),
            None => Err(err("bad_request", "nothing was uploaded")),
        }
    }

    /// Size of a stored object; None when it is not there.
    pub async fn size(&self, conv: &str, file: &str, variant: &str) -> ApiResult<Option<u64>> {
        match &self.backend {
            Backend::Local { dir } => {
                Ok(tokio::fs::metadata(dir.join(conv).join(file).join(variant))
                    .await
                    .ok()
                    .map(|m| m.len()))
            }
            Backend::S3 {
                bucket,
                creds,
                prefix,
                http,
            } => {
                let k = key(prefix, conv, file, variant);
                let url = bucket
                    .head_object(Some(creds), &k)
                    .sign(Duration::from_secs(60));
                let r = http.head(url.as_str()).send().await.map_err(storage)?;
                if r.status().as_u16() == 404 {
                    return Ok(None);
                }
                if !r.status().is_success() {
                    return Err(storage(format!("head: {}", r.status())));
                }
                Ok(r.headers()
                    .get("content-length")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse().ok()))
            }
        }
    }

    /// The first `n` bytes of an object (all of it when shorter).
    pub async fn head_bytes(
        &self,
        conv: &str,
        file: &str,
        variant: &str,
        n: u64,
    ) -> ApiResult<Vec<u8>> {
        match &self.backend {
            Backend::Local { dir } => {
                use tokio::io::AsyncReadExt;
                let f = tokio::fs::File::open(dir.join(conv).join(file).join(variant))
                    .await
                    .map_err(storage)?;
                let mut out = vec![];
                f.take(n).read_to_end(&mut out).await.map_err(storage)?;
                Ok(out)
            }
            Backend::S3 {
                bucket,
                creds,
                prefix,
                http,
            } => {
                let k = key(prefix, conv, file, variant);
                let url = bucket
                    .get_object(Some(creds), &k)
                    .sign(Duration::from_secs(60));
                let r = http
                    .get(url.as_str())
                    .header("range", format!("bytes=0-{}", n.saturating_sub(1)))
                    .send()
                    .await
                    .map_err(storage)?;
                if !r.status().is_success() {
                    return Err(storage(format!("get: {}", r.status())));
                }
                let b = r.bytes().await.map_err(storage)?;
                Ok(b[..b.len().min(n as usize)].to_vec())
            }
        }
    }

    /// Removes both objects and any unfinished upload. Gone already is fine.
    pub async fn delete(&self, conv: &str, file: &str, upload_id: Option<&str>) -> ApiResult<()> {
        match &self.backend {
            Backend::Local { dir } => {
                match tokio::fs::remove_dir_all(dir.join(conv).join(file)).await {
                    Ok(()) => Ok(()),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                    Err(e) => Err(storage(e)),
                }
            }
            Backend::S3 {
                bucket,
                creds,
                prefix,
                http,
            } => {
                if let Some(id) = upload_id {
                    let k = key(prefix, conv, file, "original");
                    let url = bucket
                        .abort_multipart_upload(Some(creds), &k, id)
                        .sign(Duration::from_secs(60));
                    let r = http.delete(url.as_str()).send().await.map_err(storage)?;
                    if !(r.status().is_success() || r.status().as_u16() == 404) {
                        return Err(storage(format!("abort multipart: {}", r.status())));
                    }
                }
                for v in ["original", "thumb"] {
                    let k = key(prefix, conv, file, v);
                    let url = bucket
                        .delete_object(Some(creds), &k)
                        .sign(Duration::from_secs(60));
                    let r = http.delete(url.as_str()).send().await.map_err(storage)?;
                    if !(r.status().is_success() || r.status().as_u16() == 404) {
                        return Err(storage(format!("delete: {}", r.status())));
                    }
                }
                Ok(())
            }
        }
    }

    /// A short-lived address to read one variant. `attachment`: the browser
    /// saves it rather than showing it.
    #[allow(clippy::too_many_arguments)]
    pub fn download_url(
        &self,
        conv: &str,
        file: &str,
        variant: &str,
        name: &str,
        mime: &str,
        attachment: bool,
        now: i64,
    ) -> String {
        let exp = now + DOWNLOAD_TTL_MS;
        let disp = disposition(attachment, name);
        match &self.backend {
            Backend::Local { .. } => {
                let sig = self.sign(&Self::download_msg(conv, file, variant, mime, &disp, exp));
                format!(
                    "/files/d/{conv}/{file}/{variant}?type={}&disp={}&exp={exp}&sig={sig}",
                    enc(mime),
                    enc(&disp)
                )
            }
            Backend::S3 {
                bucket,
                creds,
                prefix,
                ..
            } => {
                let k = key(prefix, conv, file, variant);
                let mut a = bucket.get_object(Some(creds), &k);
                a.query_mut().insert("response-content-disposition", disp);
                a.query_mut()
                    .insert("response-content-type", mime.to_string());
                a.sign(Duration::from_millis(DOWNLOAD_TTL_MS as u64))
                    .to_string()
            }
        }
    }

    /// Local folder: where a part (or the thumbnail) goes.
    pub fn local_path(&self, conv: &str, file: &str, part: &str) -> Option<PathBuf> {
        let Backend::Local { dir } = &self.backend else {
            return None;
        };
        let ok = |s: &str| {
            !s.is_empty()
                && s.len() <= 64
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        };
        if !ok(conv) || !ok(file) || !ok(part) {
            return None;
        }
        let base = dir.join(conv).join(file);
        Some(if part == "thumb" || part == "original" {
            base.join(part)
        } else {
            base.join("parts").join(part)
        })
    }

    /// Settings shown to the operator (never the keys).
    pub fn describe(&self) -> Value {
        match &self.backend {
            Backend::Local { dir } => json!({"storage": "local", "dir": dir.display().to_string()}),
            Backend::S3 { bucket, prefix, .. } => {
                json!({"storage": "s3", "bucket": bucket.name(), "region": bucket.region(), "prefix": prefix})
            }
        }
    }
}

// ------------------------------------------------------------ content

/// What the first bytes say a file is: an image type we show, or None.
pub fn sniff_image(head: &[u8]) -> Option<&'static str> {
    if head.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if head.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if head.starts_with(b"GIF87a") || head.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if head.len() >= 12 && &head[..4] == b"RIFF" && &head[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

/// Width and height from an image's first bytes, if they are enough.
pub fn image_size(head: &[u8]) -> Option<(u32, u32)> {
    image::ImageReader::new(std::io::Cursor::new(head))
        .with_guessed_format()
        .ok()?
        .into_dimensions()
        .ok()
}

/// A declared type clients may give for a non-image file: `type/subtype`
/// of token characters; anything else is `application/octet-stream`.
pub fn clean_mime(m: Option<&str>) -> String {
    let tok = |s: &str| {
        !s.is_empty()
            && s.len() <= 64
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"!#$&-^_.+".contains(&b))
    };
    match m.map(|m| m.trim().to_ascii_lowercase()) {
        Some(m) => match m.split_once('/') {
            Some((a, b)) if tok(a) && tok(b) => m,
            _ => "application/octet-stream".into(),
        },
        None => "application/octet-stream".into(),
    }
}

/// A file name as stored: no path, no control characters, at most 255
/// bytes (whole characters); empty becomes `file`.
pub fn clean_name(n: &str) -> String {
    let base = n.rsplit(['/', '\\']).next().unwrap_or("");
    let mut out = String::new();
    for c in base.chars().filter(|c| !c.is_control()) {
        if out.len() + c.len_utf8() > 255 {
            break;
        }
        out.push(c);
    }
    let out = out.trim().to_string();
    if out.is_empty() || out == "." || out == ".." {
        "file".into()
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parts_and_lengths() {
        assert_eq!(parts_of(1, 10), 1);
        assert_eq!(parts_of(10, 10), 1);
        assert_eq!(parts_of(11, 10), 2);
        assert_eq!(part_len(25, 10, 1), 10);
        assert_eq!(part_len(25, 10, 3), 5);
        assert_eq!(part_len(20, 10, 2), 10);
        assert_eq!(parts_of(4 << 30, PART_BYTES), 256);
    }

    #[test]
    fn names_types_and_dispositions() {
        assert_eq!(clean_name("../../etc/passwd"), "passwd");
        assert_eq!(clean_name("C:\\Users\\me\\a\u{0}b.txt"), "ab.txt");
        assert_eq!(clean_name("  "), "file");
        assert_eq!(clean_name(".."), "file");
        let long = "é".repeat(200);
        assert!(clean_name(&long).len() <= 255);
        assert_eq!(clean_mime(Some("Application/PDF")), "application/pdf");
        assert_eq!(
            clean_mime(Some("text/html; charset=x")),
            "application/octet-stream"
        );
        assert_eq!(clean_mime(Some("nope")), "application/octet-stream");
        assert_eq!(clean_mime(None), "application/octet-stream");
        assert_eq!(
            disposition(true, "报告 \"Q3\".pdf"),
            "attachment; filename=\"__ _Q3_.pdf\"; filename*=UTF-8''%E6%8A%A5%E5%91%8A%20%22Q3%22.pdf"
        );
        assert!(disposition(false, "a.png").starts_with("inline; "));
    }

    #[test]
    fn images_are_told_by_their_first_bytes() {
        assert_eq!(sniff_image(&[0xFF, 0xD8, 0xFF, 0xE0]), Some("image/jpeg"));
        assert_eq!(sniff_image(b"\x89PNG\r\n\x1a\nrest"), Some("image/png"));
        assert_eq!(sniff_image(b"GIF89a.."), Some("image/gif"));
        assert_eq!(sniff_image(b"RIFF\0\0\0\0WEBPVP8 "), Some("image/webp"));
        assert_eq!(sniff_image(b"<svg xmlns"), None);
        assert_eq!(sniff_image(b"%PDF-1.7"), None);
        assert_eq!(sniff_image(b""), None);
        // A 3x2 PNG.
        let mut png = vec![];
        image::RgbImage::new(3, 2)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        assert_eq!(image_size(&png), Some((3, 2)));
        assert_eq!(image_size(b"not an image"), None);
    }

    #[test]
    fn local_signatures_bind_everything() {
        let s = Store::from_config(
            &crate::config::FilesConfig::default(),
            crate::webhook::client(),
        )
        .unwrap();
        s.set_url_key(vec![7; 32]);
        let t = s.part_targets("c_1", "f_1", None, 25, 10, 1, 1000);
        assert_eq!(t.len(), 3);
        assert_eq!(t[2].n, "3");
        let q = |u: &str, k: &str| {
            u.split(['?', '&'])
                .find_map(|p| p.strip_prefix(&format!("{k}=")))
                .unwrap()
                .to_string()
        };
        let u = &t[2].url;
        assert!(u.starts_with("/files/u/c_1/f_1/3?len=5&"), "{u}");
        let (exp, sig) = (q(u, "exp").parse::<i64>().unwrap(), q(u, "sig"));
        let msg = Store::upload_msg("c_1", "f_1", "3", 5, exp);
        assert!(s.verify(&msg, &sig, exp, 1000));
        assert!(!s.verify(&msg, &sig, exp, exp + 1), "expired");
        assert!(
            !s.verify(
                &Store::upload_msg("c_1", "f_1", "2", 5, exp),
                &sig,
                exp,
                1000
            ),
            "another part"
        );
        assert!(
            !s.verify(
                &Store::upload_msg("c_1", "f_1", "3", 6, exp),
                &sig,
                exp,
                1000
            ),
            "another length"
        );
        assert!(!s.verify(&msg, "AAAA", exp, 1000));
        assert!(!s.verify(&msg, "%%", exp, 1000));
        s.set_url_key(vec![8; 32]);
        assert!(!s.verify(&msg, &sig, exp, 1000), "another key");
        assert!(s.local_path("c_1", "f_1", "../x").is_none());
        assert!(
            s.local_path("c_1", "f_1", "3")
                .unwrap()
                .ends_with("c_1/f_1/parts/3")
        );
    }
}
