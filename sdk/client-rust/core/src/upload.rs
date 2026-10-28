//! Files and images from the app (docs/02-architecture.md §2.7). A file is
//! a send of the offline queue with an upload in front: `upload_start`,
//! then the runtime PUTs the parts (`Effect::Put`, at most `PUTS_AT_ONCE`
//! at a time) and the thumbnail, then `upload_complete`, then the message
//! is sent like any other. What was uploaded is stored with the operation,
//! so after a restart only the missing parts go up again.
//!
//! An upload does not hold back the conversation: messages written after
//! it go out while it uploads; the file message takes its place in the
//! order when it is complete (a large file would block everything behind
//! it otherwise; Matrix MSC2246 names the same problem).
//!
//! Metadata removal and the thumbnail happen in the runtime before the
//! command (`media::strip_metadata`, the platform's image decoder): the
//! core only sees ready sources.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::queue::Kind;
use crate::view::need_str;
use crate::*;

/// Parts of one file uploaded at the same time.
pub const PUTS_AT_ONCE: usize = 4;
/// Largest thumbnail the server takes, and its longest side.
pub const THUMB_MAX_BYTES: u64 = 512 << 10;
pub const THUMB_SIDE: u32 = 320;
/// A PUT address this close to expiring is asked for again.
const TARGET_MARGIN_MS: i64 = 60_000;
/// Times an upload the server lost (expired, swept) starts over by itself.
const MAX_RESTARTS: u32 = 3;
/// A `file_url` answer is reused until this long before it expires.
const URL_MARGIN_MS: i64 = 30_000;

/// The thumbnail the runtime made (JPEG or PNG, longest side ≤ 320).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThumbSource {
    pub source: String,
    pub size: u64,
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
}

/// Where to PUT one part (or the thumbnail), until `expires_at`.
#[derive(Debug, Clone, PartialEq)]
pub struct Target {
    pub url: String,
    pub headers: BTreeMap<String, String>,
    pub expires_at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Step {
    Start,
    Parts,
    Complete,
}

/// The upload of a file send.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Upload {
    /// The runtime's handle of the content (a path, an IndexedDB key).
    pub source: String,
    pub name: String,
    pub size: u64,
    #[serde(default)]
    pub mime: Option<String>,
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
    #[serde(default)]
    pub thumb: Option<ThumbSource>,
    #[serde(default)]
    pub caption: Option<String>,
    #[serde(default)]
    pub file_id: Option<String>,
    #[serde(default)]
    pub part_size: u64,
    #[serde(default)]
    pub parts_total: u32,
    /// Parts stored, with their ETag. Kept as `[[n, etag], …]`: number
    /// keys do not survive the flattened operation (serde reads them back
    /// as strings).
    #[serde(default, with = "parts")]
    pub done: BTreeMap<u32, String>,
    #[serde(default)]
    pub thumb_done: bool,
    /// The file as the server checked it: the upload is complete.
    #[serde(default)]
    pub file: Option<Value>,
    #[serde(default)]
    pub restarts: u32,
    #[serde(skip)]
    pub targets: BTreeMap<String, Target>,
    /// PUTs running: bytes sent so far.
    #[serde(skip)]
    pub putting: BTreeMap<String, u64>,
    #[serde(skip)]
    pub asked: Option<Step>,
}

impl Upload {
    fn part_len(&self, n: u32) -> u64 {
        let from = (n as u64 - 1) * self.part_size;
        self.part_size.min(self.size.saturating_sub(from))
    }

    /// Bytes of the original stored or on their way.
    pub fn sent(&self) -> u64 {
        let done: u64 = self.done.keys().map(|n| self.part_len(*n)).sum();
        let running: u64 = self
            .putting
            .iter()
            .filter(|(k, _)| k.as_str() != "thumb")
            .map(|(_, b)| *b)
            .sum();
        (done + running).min(self.size)
    }

    /// What still has to go up: part numbers as strings, and `thumb`.
    fn needed(&self) -> Vec<String> {
        let mut v = vec![];
        if self.thumb.is_some() && !self.thumb_done {
            v.push("thumb".to_string());
        }
        v.extend(
            (1..=self.parts_total)
                .filter(|n| !self.done.contains_key(n))
                .map(|n| n.to_string()),
        );
        v
    }

    /// Back to nothing uploaded (the server lost or refused the upload).
    fn reset(&mut self) {
        self.file_id = None;
        self.part_size = 0;
        self.parts_total = 0;
        self.done.clear();
        self.thumb_done = false;
        self.targets.clear();
        self.putting.clear();
        self.asked = None;
    }

    fn add_targets(&mut self, f: &Value) {
        let exp = f["expires_at"].as_i64().unwrap_or(0);
        let one = |t: &Value| Target {
            url: t["url"].as_str().unwrap_or("").to_string(),
            headers: t["headers"]
                .as_object()
                .map(|h| {
                    h.iter()
                        .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string()))
                        .collect()
                })
                .unwrap_or_default(),
            expires_at: exp,
        };
        for t in f["parts"].as_array().into_iter().flatten() {
            if let Some(n) = t["n"].as_str() {
                self.targets.insert(n.to_string(), one(t));
            }
        }
        if f["thumb"].is_object() {
            self.targets.insert("thumb".into(), one(&f["thumb"]));
        }
    }

    /// The local copy as the app shows it until the server has the file.
    pub fn preview(&self) -> Value {
        json!({
            "name": self.name, "size": self.size, "mime": self.mime,
            "width": self.width, "height": self.height, "thumb": self.thumb.is_some(),
            "caption": self.caption,
            "local_source": self.source,
            "local_thumb": self.thumb.as_ref().map(|t| t.source.clone()),
        })
    }
}

/// How a PUT ended, as the runtime reports it.
#[derive(Debug, Clone, PartialEq)]
pub enum PutResult {
    /// Stored; the ETag the store returned (may be empty).
    Done(String),
    /// Refused or failed: the HTTP status if there was an answer.
    Failed { status: Option<u16>, detail: String },
    /// The source can no longer be read (deleted, moved): the send fails.
    Unreadable(String),
}

impl Op {
    /// A file send whose upload is not complete.
    pub fn uploading(&self) -> bool {
        matches!(&self.kind, Kind::Send { upload: Some(u), .. } if u.file.is_none())
    }

    pub(crate) fn upload_mut(&mut self) -> Option<&mut Upload> {
        match &mut self.kind {
            Kind::Send {
                upload: Some(u), ..
            } => Some(u),
            _ => None,
        }
    }
}

fn label(kind: &str, name: &str) -> String {
    if kind == "image" {
        "[Image]".into()
    } else {
        format!("[File] {name}")
    }
}

impl Core {
    /// The server's addresses are relative for its own folder.
    fn absolute(&self, url: &str) -> String {
        if url.starts_with('/') {
            format!("{}{url}", self.cfg.url.trim_end_matches('/'))
        } else {
            url.to_string()
        }
    }

    /// `send_file {conv_id, file: {source, name, size, mime?, width?,
    /// height?}, thumb?: {source, size, width?, height?}, caption?,
    /// reply_to?, client_msg_id?}`: shown at once as uploading.
    pub(crate) fn cmd_send_file(&mut self, cmd: &Value) -> Result<Value, ClientError> {
        let conv = need_str(cmd, "conv_id")?;
        let f = &cmd["file"];
        let source = need_str(f, "source").map_err(|_| bad("file.source is required"))?;
        let name = need_str(f, "name").map_err(|_| bad("file.name is required"))?;
        let size = f["size"]
            .as_u64()
            .filter(|s| *s > 0)
            .ok_or_else(|| bad("file.size must be more than 0"))?;
        let thumb = match cmd.get("thumb").filter(|t| !t.is_null()) {
            None => None,
            Some(t) => {
                let t: ThumbSource = serde_json::from_value(t.clone())
                    .map_err(|_| bad("thumb takes source, size, width, height"))?;
                if t.size == 0 || t.size > THUMB_MAX_BYTES {
                    return Err(bad(format!("thumb.size is 1..={THUMB_MAX_BYTES} bytes")));
                }
                if t.width.max(t.height).is_some_and(|s| s > THUMB_SIDE) {
                    return Err(bad(format!(
                        "a thumbnail is at most {THUMB_SIDE} pixels on its longest side"
                    )));
                }
                Some(t)
            }
        };
        let caption = cmd["caption"]
            .as_str()
            .filter(|c| !c.is_empty())
            .map(String::from);
        if caption.as_ref().is_some_and(|c| c.len() > 4096) {
            return Err(bad("caption is at most 4096 bytes"));
        }
        let mime = f["mime"].as_str().map(String::from);
        let image = thumb.is_some()
            || (mime.as_deref().is_some_and(|m| m.starts_with("image/"))
                && f["width"].as_u64().is_some());
        let kind = if image { "image" } else { "file" };
        let upload = Upload {
            source,
            name: name.clone(),
            size,
            mime,
            width: f["width"].as_u64().map(|w| w as u32),
            height: f["height"].as_u64().map(|h| h as u32),
            thumb,
            caption,
            file_id: None,
            part_size: 0,
            parts_total: 0,
            done: BTreeMap::new(),
            thumb_done: false,
            file: None,
            restarts: 0,
            targets: BTreeMap::new(),
            putting: BTreeMap::new(),
            asked: None,
        };
        self.queue_send(
            &conv,
            cmd["client_msg_id"].as_str(),
            label(kind, &name),
            Some(kind.to_string()),
            None,
            cmd["reply_to"].as_str().map(String::from),
            Some(Box::new(upload)),
        )
    }

    /// What the uploads need now: requests (when connected) and PUTs
    /// (any time their addresses are valid).
    pub(crate) fn pump_uploads(&mut self) {
        let live = self.phase == Phase::Live && self.paused_until.is_none();
        let now = self.now;
        for i in 0..self.ops.len() {
            let o = &self.ops[i];
            if !o.uploading() || o.failed.is_some() || o.not_before.is_some_and(|t| t > now) {
                continue;
            }
            let asking = o.in_flight.is_some();
            let op_id = o.id.clone();
            let conv = o.conv_id.clone();
            let Kind::Send {
                upload: Some(u), ..
            } = &o.kind
            else {
                continue;
            };
            let Some(file_id) = u.file_id.clone() else {
                if live && !asking {
                    let mut a = json!({"conv_id": conv, "name": u.name, "size": u.size});
                    if let Some(m) = &u.mime {
                        a["mime"] = json!(m);
                    }
                    if let Some(t) = &u.thumb {
                        a["thumb_size"] = json!(t.size);
                    }
                    self.upload_request(i, "upload_start", a, Step::Start);
                }
                continue;
            };
            let needed = u.needed();
            if needed.is_empty() {
                if live && !asking && u.putting.is_empty() {
                    let parts: Vec<Value> = u
                        .done
                        .iter()
                        .map(|(n, e)| json!({"n": n, "etag": e}))
                        .collect();
                    let a = json!({"conv_id": conv, "file_id": file_id, "parts": parts});
                    self.upload_request(i, "upload_complete", a, Step::Complete);
                }
                continue;
            }
            let mut puts = vec![];
            let mut missing: Option<u32> = None;
            let mut running = u.putting.len();
            for key in &needed {
                if u.putting.contains_key(key) {
                    continue;
                }
                match u.targets.get(key) {
                    Some(t) if t.expires_at - TARGET_MARGIN_MS > now => {
                        if running < PUTS_AT_ONCE {
                            running += 1;
                            let (source, offset, len) = if key == "thumb" {
                                let t = u.thumb.as_ref().expect("needed only with a thumbnail");
                                (t.source.clone(), 0, t.size)
                            } else {
                                let n: u32 = key.parse().unwrap_or(1);
                                (
                                    u.source.clone(),
                                    (n as u64 - 1) * u.part_size,
                                    u.part_len(n),
                                )
                            };
                            puts.push(Effect::Put {
                                op_id: op_id.clone(),
                                file_id: file_id.clone(),
                                key: key.clone(),
                                url: self.absolute(&t.url),
                                headers: t.headers.clone(),
                                source,
                                offset,
                                len,
                            });
                        }
                    }
                    _ => {
                        let n = key.parse().unwrap_or(1);
                        missing = Some(missing.map_or(n, |m: u32| m.min(n)));
                    }
                }
            }
            for p in puts {
                if let (Some(u), Effect::Put { key, .. }) = (self.ops[i].upload_mut(), &p) {
                    u.putting.insert(key.clone(), 0);
                }
                self.out.push(p);
            }
            if let Some(from) = missing
                && live
                && !asking
            {
                let a = json!({"conv_id": conv, "file_id": file_id, "from": from});
                self.upload_request(i, "upload_parts", a, Step::Parts);
            }
        }
    }

    fn upload_request(&mut self, i: usize, op: &str, args: Value, step: Step) {
        let rid = self.request(op, args, Purpose::Op(self.ops[i].id.clone()));
        self.ops[i].in_flight = Some(rid);
        if let Some(u) = self.ops[i].upload_mut() {
            u.asked = Some(step);
        }
    }

    /// The answer to an upload request of operation `i` (`in_flight` cleared).
    pub(crate) fn on_upload_answer(
        &mut self,
        i: usize,
        step: Step,
        f: &Value,
        err: Option<ClientError>,
    ) {
        let conv = self.ops[i].conv_id.clone();
        let key = match &self.ops[i].kind {
            Kind::Send { cm, .. } => format!("local:{cm}"),
            _ => return,
        };
        self.mark_changed(&conv, &key);
        let Some(e) = err else {
            self.earn_budget();
            let Kind::Send {
                upload: Some(u),
                text,
                msg_kind,
                payload,
                ..
            } = &mut self.ops[i].kind
            else {
                return;
            };
            match step {
                Step::Start => {
                    u.file_id = f["file_id"].as_str().map(String::from);
                    u.part_size = f["part_size"].as_u64().unwrap_or(u.size);
                    u.parts_total = f["parts_total"].as_u64().unwrap_or(1) as u32;
                    u.add_targets(f);
                }
                Step::Parts => u.add_targets(f),
                Step::Complete => {
                    // The server decides what it is (an "image" that is
                    // not one is sent as a file).
                    let kind = f["kind"].as_str().unwrap_or("file").to_string();
                    let mut p = json!({"file_id": u.file_id});
                    if let Some(c) = &u.caption {
                        p["caption"] = json!(c);
                    }
                    *text = label(&kind, &u.name);
                    *msg_kind = Some(kind);
                    *payload = Some(p);
                    let mut file = f.clone();
                    if let Some(o) = file.as_object_mut() {
                        for k in ["type", "rid", "ok"] {
                            o.remove(k);
                        }
                    }
                    u.file = Some(file);
                    u.targets.clear();
                    let op_id = self.ops[i].id.clone();
                    self.out.push(Effect::ReleaseUpload { op_id });
                }
            }
            self.save_op(i);
            return;
        };
        if e.retryable {
            return self.back_off(i, &e);
        }
        let u = self.ops[i].upload_mut().expect("an upload");
        match (step, e.code.as_str()) {
            // Completed already (its answer was lost): complete again.
            (Step::Parts, "conflict") => {
                for n in 1..=u.parts_total {
                    u.done.entry(n).or_default();
                }
                u.thumb_done = true;
                self.save_op(i);
            }
            // The server no longer has it (expired, swept): start over.
            (Step::Parts | Step::Complete, "not_found") if u.restarts < MAX_RESTARTS => {
                u.restarts += 1;
                u.reset();
                self.note_failure("upload_restarted", e.to_json());
                self.save_op(i);
            }
            // Not what was declared: thrown away by the server; a retry
            // uploads it again.
            (Step::Complete, _) => {
                u.reset();
                self.fail_send(i, e);
            }
            _ => self.fail_send(i, e),
        }
    }

    /// The runtime finished a PUT. Results of an upload that was started
    /// over (another `file_id`) are ignored.
    pub(crate) fn on_put_done(&mut self, op_id: &str, file_id: &str, key: &str, r: PutResult) {
        let Some(i) = self.ops.iter().position(|o| o.id == op_id) else {
            return;
        };
        let conv = self.ops[i].conv_id.clone();
        let local = match &self.ops[i].kind {
            Kind::Send { cm, .. } => format!("local:{cm}"),
            _ => return,
        };
        let ours = self.ops[i].upload_mut().is_some_and(|u| {
            u.file_id.as_deref() == Some(file_id) && u.putting.remove(key).is_some()
        });
        if !ours {
            return;
        }
        self.mark_changed(&conv, &local);
        let u = self.ops[i].upload_mut().expect("an upload");
        match r {
            PutResult::Done(etag) => {
                if key == "thumb" {
                    u.thumb_done = true;
                } else if let Ok(n) = key.parse() {
                    u.done.insert(n, etag);
                }
                self.ops[i].attempts = 0;
                self.save_op(i);
            }
            PutResult::Unreadable(detail) => self.fail_send(
                i,
                ClientError::new(
                    "bad_request",
                    format!("the file can no longer be read: {detail}"),
                ),
            ),
            // The body is larger than a proxy in front takes: no retry helps.
            PutResult::Failed {
                status: Some(413), ..
            } => self.fail_send(
                i,
                ClientError::new(
                    "bad_request",
                    "a proxy refused the part as too large (raise its body limit to the part size)",
                ),
            ),
            PutResult::Failed { status, detail } => {
                // The address expired or the upload is closed: ask again
                // (upload_parts tells if the upload is gone).
                if matches!(status, Some(400 | 401 | 403 | 404 | 410)) {
                    u.targets.clear();
                }
                let e = ClientError::new(
                    "unavailable",
                    format!("upload of part {key} failed ({status:?}): {detail}"),
                );
                self.back_off(i, &e);
            }
        }
    }

    pub(crate) fn on_put_progress(&mut self, op_id: &str, file_id: &str, key: &str, sent: u64) {
        let Some(i) = self.ops.iter().position(|o| o.id == op_id) else {
            return;
        };
        let conv = self.ops[i].conv_id.clone();
        let local = match &self.ops[i].kind {
            Kind::Send { cm, .. } => format!("local:{cm}"),
            _ => return,
        };
        if let Some(u) = self.ops[i].upload_mut()
            && u.file_id.as_deref() == Some(file_id)
            && let Some(b) = u.putting.get_mut(key)
        {
            *b = sent;
            self.mark_changed(&conv, &local);
        }
    }

    /// `file_url {conv_id, file_id, variant?}`: an absolute address to
    /// read a file, reused while it is valid.
    pub(crate) fn cmd_file_url(&mut self, id: u64, cmd: &Value) {
        let (conv, file) = match (need_str(cmd, "conv_id"), need_str(cmd, "file_id")) {
            (Ok(c), Ok(f)) => (c, f),
            (Err(e), _) | (_, Err(e)) => return self.reply(id, Err(e)),
        };
        let variant = cmd["variant"].as_str().unwrap_or("original").to_string();
        let key = format!("{file}/{variant}");
        if let Some(v) = self.file_urls.get(&key)
            && v["expires_at"].as_i64().unwrap_or(0) - URL_MARGIN_MS > self.now
        {
            let v = v.clone();
            return self.reply(id, Ok(v));
        }
        if self.phase != Phase::Live {
            return self.reply(id, Err(ClientError::new("disconnected", "not connected")));
        }
        self.request(
            "file_url",
            json!({"conv_id": conv, "file_id": file, "variant": variant}),
            Purpose::FileUrl { cmd: id, key },
        );
    }

    pub(crate) fn on_file_url(
        &mut self,
        id: u64,
        key: String,
        f: &Value,
        err: Option<ClientError>,
    ) {
        let r = match err {
            Some(e) => Err(e),
            None => {
                let mut v = json!({
                    "url": self.absolute(f["url"].as_str().unwrap_or("")),
                    "variant": f["variant"], "expires_at": f["expires_at"], "file": f["file"],
                });
                if v["file"].is_null()
                    && let Some(o) = v.as_object_mut()
                {
                    o.remove("file");
                }
                self.file_urls.insert(key, v.clone());
                Ok(v)
            }
        };
        self.reply(id, r);
    }
}

mod parts {
    use std::collections::BTreeMap;

    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(m: &BTreeMap<u32, String>, s: S) -> Result<S::Ok, S::Error> {
        s.collect_seq(m.iter())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<BTreeMap<u32, String>, D::Error> {
        Ok(Vec::<(u32, String)>::deserialize(d)?.into_iter().collect())
    }
}

fn bad(m: impl Into<String>) -> ClientError {
    ClientError::new("bad_request", m)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_upload_survives_the_store() {
        let mut done = BTreeMap::new();
        done.insert(1, "\"e1\"".to_string());
        done.insert(12, String::new());
        let op = Op {
            id: "o1".into(),
            conv_id: "c".into(),
            kind: Kind::Send {
                cm: "cm".into(),
                text: "[File] a".into(),
                msg_kind: Some("file".into()),
                payload: None,
                reply_to: None,
                sent: None,
                upload: Some(Box::new(Upload {
                    source: "/a".into(),
                    name: "a".into(),
                    size: 30 << 20,
                    mime: None,
                    width: None,
                    height: None,
                    thumb: Some(ThumbSource {
                        source: "/t".into(),
                        size: 9,
                        width: Some(320),
                        height: Some(200),
                    }),
                    caption: Some("c".into()),
                    file_id: Some("f_1".into()),
                    part_size: 16 << 20,
                    parts_total: 2,
                    done,
                    thumb_done: true,
                    file: None,
                    restarts: 1,
                    targets: BTreeMap::new(),
                    putting: BTreeMap::new(),
                    asked: None,
                })),
            },
            created_at: 5,
            n: 3,
            attempts: 0,
            failed: None,
            in_flight: None,
            not_before: None,
        };
        let back = Op::from_json(&op.to_json()).expect("readable");
        assert_eq!(back, op);
        // Through text too, as the stores keep it.
        let text = op.to_json().to_string();
        assert_eq!(
            Op::from_json(&serde_json::from_str(&text).unwrap()),
            Some(op)
        );
    }
}
