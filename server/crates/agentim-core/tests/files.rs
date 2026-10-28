//! Files and images (docs/02-architecture.md §2.7) with the local folder:
//! uploads in parts through signed addresses, checks on completion, one
//! message per file, who may read it, edits, drafts, and what throws a file
//! away (recall, dissolve, account deletion, retention, expiry), on every
//! database; then races. The S3 store is in `files_s3.rs`.

mod common;

use std::net::SocketAddr;

use agentim_core::config::Config;
use agentim_core::hub::Hub;
use agentim_core::logic::Op;
use agentim_core::model::*;
use agentim_core::server::{AppState, router};
use common::*;
use serde_json::{Value, json};

backend_tests!(
    an_image_goes_up_with_its_thumbnail_and_down_to_members,
    refused_uploads_leave_nothing,
    completion_checks_what_was_uploaded,
    addresses_are_signed_and_short_lived,
    a_file_is_carried_by_one_message,
    files_of_drafts_wait_for_approval,
    what_throws_a_file_away,
    large_files_go_in_parts,
    files_survive_a_restart,
    race_messages_for_one_file,
    race_sweeps_against_sends,
    the_open_api_uploads_and_reads_files,
    whole_files_lose_their_location_and_get_a_thumbnail,
);

struct Srv {
    base: String,
    hub: Hub,
    http: reqwest::Client,
    dir: tempfile::TempDir,
}

fn cfg(db: &TestDb, dir: &std::path::Path) -> Config {
    let mut c = single(&db.url);
    c.files.dir = dir.join("files").to_string_lossy().into_owned();
    c
}

async fn serve(hub: Hub) -> String {
    let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = tcp.local_addr().unwrap().to_string();
    let app = router(AppState {
        hub,
        demo_login: false,
        metrics_token: None,
    });
    tokio::spawn(async move {
        axum::serve(tcp, app.into_make_service_with_connect_info::<SocketAddr>())
            .await
            .unwrap()
    });
    format!("http://{addr}")
}

async fn boot(db: &TestDb) -> Srv {
    start_with(db, |_| {}).await
}

async fn start_with(db: &TestDb, f: impl FnOnce(&mut Config)) -> Srv {
    let dir = tempfile::tempdir().unwrap();
    let mut c = cfg(db, dir.path());
    f(&mut c);
    let hub = open(c).await;
    let base = serve(hub.clone()).await;
    Srv {
        base,
        hub,
        http: agentim_core::webhook::client(),
        dir,
    }
}

fn png(w: u32, h: u32) -> Vec<u8> {
    let mut out = vec![];
    image::RgbImage::from_fn(w, h, |x, y| {
        image::Rgb([(x % 256) as u8, (y % 256) as u8, 90])
    })
    .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
    .unwrap();
    out
}

fn jpeg(w: u32, h: u32) -> Vec<u8> {
    let mut out = vec![];
    image::RgbImage::from_fn(w, h, |x, _| image::Rgb([(x % 256) as u8, 30, 200]))
        .write_to(
            &mut std::io::Cursor::new(&mut out),
            image::ImageFormat::Jpeg,
        )
        .unwrap();
    out
}

fn code(v: &Value) -> &str {
    v["error"]["code"].as_str().unwrap_or("")
}

impl Srv {
    /// PUTs one target with its body; the status and ETag.
    async fn put(&self, t: &Value, body: Vec<u8>) -> (u16, String) {
        let mut r = self
            .http
            .put(format!("{}{}", self.base, t["url"].as_str().unwrap()));
        for (k, v) in t["headers"].as_object().unwrap() {
            if k != "content-length" {
                r = r.header(k, v.as_str().unwrap());
            }
        }
        let resp = r.body(body).send().await.unwrap();
        let etag = resp
            .headers()
            .get("etag")
            .map(|v| v.to_str().unwrap().to_string())
            .unwrap_or_default();
        (resp.status().as_u16(), etag)
    }

    async fn get(
        &self,
        url: &str,
        range: Option<&str>,
    ) -> (u16, reqwest::header::HeaderMap, Vec<u8>) {
        let mut r = self.http.get(format!("{}{url}", self.base));
        if let Some(rg) = range {
            r = r.header("range", rg);
        }
        let resp = r.send().await.unwrap();
        let s = resp.status().as_u16();
        let h = resp.headers().clone();
        (s, h, resp.bytes().await.unwrap().to_vec())
    }

    /// Starts, PUTs every part and the thumbnail, completes. The last answer.
    async fn upload(
        &self,
        c: &Conn,
        conv: &str,
        name: &str,
        data: &[u8],
        thumb: Option<&[u8]>,
    ) -> Value {
        let mut start =
            json!({"op": "upload_start", "conv_id": conv, "name": name, "size": data.len()});
        if let Some(t) = thumb {
            start["thumb_size"] = json!(t.len());
        }
        let s = c.call(start).await;
        if s["ok"] != true {
            return s;
        }
        self.finish(c, conv, &s, data, thumb).await
    }

    async fn finish(
        &self,
        c: &Conn,
        conv: &str,
        s: &Value,
        data: &[u8],
        thumb: Option<&[u8]>,
    ) -> Value {
        let part = s["part_size"].as_u64().unwrap() as usize;
        let mut tags = vec![];
        for t in s["parts"].as_array().unwrap() {
            let n: usize = t["n"].as_str().unwrap().parse().unwrap();
            let body = data[(n - 1) * part..(n * part).min(data.len())].to_vec();
            let (st, etag) = self.put(t, body).await;
            assert_eq!(st, 200, "part {n}");
            tags.push(json!({"n": n, "etag": etag}));
        }
        if let Some(t) = thumb {
            let (st, _) = self.put(&s["thumb"], t.to_vec()).await;
            assert_eq!(st, 200, "thumb");
        }
        c.call(json!({"op": "upload_complete", "conv_id": conv, "file_id": s["file_id"], "parts": tags}))
            .await
    }

    fn stored(&self, conv: &str, file: &str) -> bool {
        self.dir.path().join("files").join(conv).join(file).exists()
    }
}

async fn start(c: &Conn, conv: &str, size: u64, thumb: Option<u64>) -> Value {
    let mut v = json!({"op": "upload_start", "conv_id": conv, "name": "x.bin", "size": size});
    if let Some(t) = thumb {
        v["thumb_size"] = json!(t);
    }
    c.call(v).await
}

async fn send_file(
    c: &Conn,
    conv: &str,
    kind: &str,
    file_id: &str,
    caption: Option<&str>,
) -> Value {
    let mut p = json!({"file_id": file_id});
    if let Some(cap) = caption {
        p["caption"] = json!(cap);
    }
    c.call(json!({"op": "send", "conv_id": conv, "client_msg_id": uuid::Uuid::new_v4().to_string(), "text": "[Image]", "kind": kind, "payload": p}))
        .await
}

async fn url(c: &Conn, conv: &str, file: &str, variant: &str) -> Value {
    c.call(json!({"op": "file_url", "conv_id": conv, "file_id": file, "variant": variant}))
        .await
}

async fn an_image_goes_up_with_its_thumbnail_and_down_to_members(db: TestDb) {
    let srv = boot(&db).await;
    let hub = &srv.hub;
    let alice = user(hub, "alice", UserKind::Human, None).await;
    let bob = user(hub, "bob", UserKind::Human, None).await;
    let carol = user(hub, "carol", UserKind::Human, None).await;
    let (a, b, c) = (
        Conn::new(hub, &alice),
        Conn::new(hub, &bob),
        Conn::new(hub, &carol),
    );
    let conv = a.group(&[&bob]).await;
    let (orig, thumb) = (png(800, 600), jpeg(320, 240));
    let done = srv
        .upload(&a, &conv, "../sunset.png", &orig, Some(&thumb))
        .await;
    assert_eq!(done["ok"], true, "{done}");
    assert_eq!(done["kind"], "image");
    assert_eq!(
        (done["width"].as_i64(), done["height"].as_i64()),
        (Some(800), Some(600))
    );
    assert_eq!(done["thumb"], true);
    assert_eq!(done["name"], "sunset.png", "no path in names");
    assert_eq!(done["mime"], "image/png");
    assert_eq!(done["size"], orig.len());
    assert_eq!(done["ai_generated"], false);
    let fid = done["file_id"].as_str().unwrap().to_string();
    // Completing again: the same answer.
    let again = a
        .call(json!({"op": "upload_complete", "conv_id": conv, "file_id": fid, "parts": []}))
        .await;
    assert_eq!(again["file_id"], json!(fid), "{again}");
    // Before any message carries it, only the uploader reads it.
    assert_eq!(code(&url(&b, &conv, &fid, "thumb").await), "forbidden");
    let sent = send_file(&a, &conv, "image", &fid, Some("Golden hour")).await;
    assert_eq!(sent["ok"], true, "{sent}");
    let log = b.pull(&conv, 0).await;
    let m = log["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["event"]["type"] == "msg.created" && e["event"]["msg"]["kind"] == "image")
        .expect("the image message")["event"]["msg"]
        .clone();
    let p = &m["payload"];
    assert_eq!(p["file_id"], json!(fid));
    assert_eq!(p["caption"], "Golden hour");
    assert_eq!(
        (p["width"].as_i64(), p["thumb"].as_bool()),
        (Some(800), Some(true))
    );
    // A member reads both; the bytes are what was uploaded.
    let t = url(&b, &conv, &fid, "thumb").await;
    assert_eq!(t["variant"], "thumb");
    let (s, h, body) = srv.get(t["url"].as_str().unwrap(), None).await;
    assert_eq!((s, body), (200, thumb.clone()));
    assert_eq!(h["content-type"], "image/jpeg");
    assert_eq!(h["x-content-type-options"], "nosniff");
    assert!(h["cache-control"].to_str().unwrap().contains("immutable"));
    let o = url(&b, &conv, &fid, "original").await;
    let (s, h, body) = srv.get(o["url"].as_str().unwrap(), None).await;
    assert_eq!((s, body == orig), (200, true));
    assert!(
        h["content-disposition"]
            .to_str()
            .unwrap()
            .starts_with("inline; "),
        "images show inline"
    );
    // A range, as players and resumable downloads ask.
    let (s, h, body) = srv.get(o["url"].as_str().unwrap(), Some("bytes=0-7")).await;
    assert_eq!((s, body.as_slice()), (206, &orig[..8]));
    assert_eq!(
        h["content-range"],
        format!("bytes 0-7/{}", orig.len()).as_str()
    );
    let (s, _, _) = srv
        .get(o["url"].as_str().unwrap(), Some("bytes=999999999-"))
        .await;
    assert_eq!(s, 416);
    // Not a member: refused; unknown file or variant.
    assert_eq!(code(&url(&c, &conv, &fid, "original").await), "forbidden");
    assert_eq!(
        code(&url(&b, &conv, "f_nope", "original").await),
        "not_found"
    );
    assert_eq!(code(&url(&b, &conv, &fid, "huge").await), "bad_request");
    // A plain file downloads as an attachment, whatever it claims to be.
    let pdf = b"%PDF-1.7 a report".to_vec();
    let start = a
        .ok(json!({"op": "upload_start", "conv_id": conv, "name": "Q3 report.pdf", "size": pdf.len(), "mime": "application/pdf"}))
        .await;
    let f = srv.finish(&a, &conv, &start, &pdf, None).await;
    assert_eq!(
        (f["kind"].as_str(), f["mime"].as_str()),
        (Some("file"), Some("application/pdf"))
    );
    send_file(&a, &conv, "file", f["file_id"].as_str().unwrap(), None).await;
    let o = url(&b, &conv, f["file_id"].as_str().unwrap(), "thumb").await;
    assert_eq!(o["variant"], "original", "no thumbnail: the file itself");
    let (_, h, _) = srv.get(o["url"].as_str().unwrap(), None).await;
    assert!(
        h["content-disposition"]
            .to_str()
            .unwrap()
            .starts_with("attachment; filename=\"Q3 report.pdf\"")
    );
    assert_eq!(h["content-security-policy"], "sandbox; default-src 'none'");
    // An image sent as a file is fine; a file is never an image.
    let fake = srv
        .upload(&a, &conv, "cat.jpg", b"<svg onload=alert(1)>", None)
        .await;
    assert_eq!(
        (fake["kind"].as_str(), fake["mime"].as_str()),
        (Some("file"), Some("application/octet-stream"))
    );
    let r = send_file(&a, &conv, "image", fake["file_id"].as_str().unwrap(), None).await;
    assert_eq!(code(&r), "bad_request", "{r}");
    // An agent's file says so.
    let bot = user(hub, "bot", UserKind::Agent, Some(&alice)).await;
    a.ok(json!({"op": "add_member", "conv_id": conv, "user_id": bot.id}))
        .await;
    let g = Conn::new(hub, &bot);
    let made = srv.upload(&g, &conv, "chart.png", &png(10, 10), None).await;
    assert_eq!(made["ai_generated"], true, "{made}");
}

async fn refused_uploads_leave_nothing(db: TestDb) {
    let srv = start_with(&db, |c| {
        c.files.max_bytes = 1000;
        c.files.max_open_per_user = 2;
        c.files.daily_bytes_per_user = 2500;
    })
    .await;
    let hub = &srv.hub;
    let alice = user(hub, "alice", UserKind::Human, None).await;
    let bob = user(hub, "bob", UserKind::Human, None).await;
    let eve = user(hub, "eve", UserKind::Human, None).await;
    let (a, e) = (Conn::new(hub, &alice), Conn::new(hub, &eve));
    let conv = a.group(&[&bob]).await;
    for (c, cv, size, thumb, want) in [
        (&e, conv.as_str(), 10, None, "forbidden"),
        (&a, conv.as_str(), 0, None, "bad_request"),
        (&a, conv.as_str(), 1001, None, "bad_request"),
        (&a, conv.as_str(), 10, Some((512 << 10) + 1), "bad_request"),
        (&a, "c_nope", 10, None, "not_found"),
    ] {
        let r = start(c, cv, size, thumb).await;
        assert_eq!(code(&r), want, "{size} {thumb:?}: {r}");
    }
    assert_eq!(
        code(
            &a.call(json!({"op": "upload_start", "conv_id": conv, "name": "x"}))
                .await
        ),
        "bad_request"
    );
    // Exactly the limit is fine; then two open at once is the most.
    let one = start(&a, &conv, 1000, None).await;
    assert_eq!(one["ok"], true);
    assert_eq!(start(&a, &conv, 10, None).await["ok"], true);
    assert_eq!(code(&start(&a, &conv, 10, None).await), "rate_limited");
    // Finishing one frees a slot; the day's 2500 bytes run out next.
    let f = srv.finish(&a, &conv, &one, &[7u8; 1000], None).await;
    assert_eq!(f["ok"], true, "{f}");
    assert_eq!(start(&a, &conv, 1000, None).await["ok"], true);
    assert_eq!(code(&start(&a, &conv, 1000, None).await), "rate_limited");
    // Blocked in a direct conversation; dissolved; removed.
    let dm = a
        .ok(json!({"op": "create_conv", "kind": "dm", "title": "dm", "members": [bob.id]}))
        .await["conv_id"]
        .as_str()
        .unwrap()
        .to_string();
    let b = Conn::new(hub, &bob);
    b.ok(json!({"op": "block", "user_id": alice.id})).await;
    assert_eq!(code(&start(&b, &dm, 10, None).await), "blocked");
    let gone = b.group(&[&alice]).await;
    b.ok(json!({"op": "remove_member", "conv_id": gone, "user_id": alice.id}))
        .await;
    assert_eq!(code(&start(&a, &gone, 10, None).await), "forbidden");
    b.ok(json!({"op": "dissolve", "conv_id": gone})).await;
    assert_eq!(code(&start(&b, &gone, 10, None).await), "not_found");
    // None of the refusals left a file or folder behind.
    let left = std::fs::read_dir(srv.dir.path().join("files"))
        .map(|d| d.count())
        .unwrap_or(0);
    assert!(left <= 1, "only the one conversation's folder");
}

async fn completion_checks_what_was_uploaded(db: TestDb) {
    let srv = boot(&db).await;
    let hub = &srv.hub;
    let alice = user(hub, "alice", UserKind::Human, None).await;
    let bob = user(hub, "bob", UserKind::Human, None).await;
    let (a, b) = (Conn::new(hub, &alice), Conn::new(hub, &bob));
    let conv = a.group(&[&bob]).await;
    let begin = |thumb: Option<usize>, size: usize| {
        let mut v = json!({"op": "upload_start", "conv_id": conv, "name": "p.png", "size": size});
        if let Some(t) = thumb {
            v["thumb_size"] = json!(t);
        }
        a.ok(v)
    };
    // Nothing uploaded yet.
    let s = begin(None, 100).await;
    let fid = s["file_id"].as_str().unwrap().to_string();
    // Someone else cannot complete or extend it.
    let r = b
        .call(json!({"op": "upload_complete", "conv_id": conv, "file_id": fid, "parts": []}))
        .await;
    assert_eq!(code(&r), "forbidden");
    assert_eq!(
        code(
            &b.call(json!({"op": "upload_parts", "conv_id": conv, "file_id": fid}))
                .await
        ),
        "forbidden"
    );
    let r = a
        .call(json!({"op": "upload_complete", "conv_id": conv, "file_id": fid, "parts": []}))
        .await;
    assert_eq!(code(&r), "bad_request", "{r}");
    // Thrown away: its addresses take nothing more, and it cannot be sent.
    let (st, _) = srv.put(&s["parts"][0], vec![0; 100]).await;
    assert_eq!(st, 404);
    assert_eq!(
        code(&send_file(&a, &conv, "file", &fid, None).await),
        "not_found"
    );
    // A part of the wrong length is refused at the door.
    let s = begin(None, 100).await;
    let (st, _) = srv.put(&s["parts"][0], vec![0; 99]).await;
    assert_eq!(st, 400);
    let (st, _) = srv.put(&s["parts"][0], vec![0; 101]).await;
    assert_eq!(st, 400);
    // A thumbnail is for an image, a JPEG or PNG, and at most 320 pixels.
    let pdf = b"%PDF-1.4 hello".to_vec();
    let t = jpeg(100, 100);
    let s = begin(Some(t.len()), pdf.len()).await;
    let r = srv.finish(&a, &conv, &s, &pdf, Some(&t)).await;
    assert_eq!(code(&r), "bad_request", "{r}");
    let img = png(600, 600);
    for bad in [
        jpeg(400, 300),
        b"GIF89a not a thumb".to_vec(),
        b"plain text".to_vec(),
    ] {
        let s = begin(Some(bad.len()), img.len()).await;
        let r = srv.finish(&a, &conv, &s, &img, Some(&bad)).await;
        assert_eq!(code(&r), "bad_request", "{r}");
    }
    // A thumbnail that was declared but not uploaded.
    let s = begin(Some(t.len()), img.len()).await;
    let r = srv.finish(&a, &conv, &s, &img, None).await;
    assert_eq!(code(&r), "bad_request");
    // A PNG thumbnail of exactly 320 is fine.
    let ok = png(320, 320);
    let s = begin(Some(ok.len()), img.len()).await;
    let r = srv.finish(&a, &conv, &s, &img, Some(&ok)).await;
    assert_eq!(r["thumb"], true, "{r}");
    let u = url(&a, &conv, r["file_id"].as_str().unwrap(), "thumb").await;
    let (_, h, _) = srv.get(u["url"].as_str().unwrap(), None).await;
    assert_eq!(h["content-type"], "image/png");
    // Unknown and other conversations' uploads.
    let other = a.group(&[]).await;
    let r = a
        .call(json!({"op": "upload_complete", "conv_id": other, "file_id": r["file_id"], "parts": []}))
        .await;
    assert_eq!(code(&r), "not_found");
}

async fn addresses_are_signed_and_short_lived(db: TestDb) {
    let srv = boot(&db).await;
    let hub = &srv.hub;
    let alice = user(hub, "alice", UserKind::Human, None).await;
    let a = Conn::new(hub, &alice);
    let conv = a.group(&[]).await;
    let s = a
        .ok(json!({"op": "upload_start", "conv_id": conv, "name": "a.bin", "size": 10}))
        .await;
    let t = &s["parts"][0];
    let u = t["url"].as_str().unwrap();
    let tamper = |from: &str, to: &str| json!({"url": u.replacen(from, to, 1), "headers": {}});
    // Another file, part, length or signature: refused.
    let sig = u.split("sig=").nth(1).unwrap();
    for bad in [
        tamper(s["file_id"].as_str().unwrap(), "f_other"),
        tamper("/1?", "/2?"),
        tamper("len=10", "len=11"),
        tamper(sig, "AAAA"),
        tamper("exp=", "exp=1"),
    ] {
        let (st, _) = srv.put(&bad, vec![1; 10]).await;
        assert_eq!(st, 403, "{bad}");
    }
    let (st, etag) = srv.put(t, vec![1; 10]).await;
    assert_eq!((st, etag.as_str()), (200, "\"1\""));
    // Written again (a retry): replaced.
    assert_eq!(srv.put(t, vec![2; 10]).await.0, 200);
    let done = a
        .ok(json!({"op": "upload_complete", "conv_id": conv, "file_id": s["file_id"], "parts": [{"n": 1, "etag": etag}]}))
        .await;
    let fid = done["file_id"].as_str().unwrap();
    let d = url(&a, &conv, fid, "original").await;
    let du = d["url"].as_str().unwrap();
    assert_eq!(srv.get(du, None).await.2, vec![2; 10]);
    assert!(d["expires_at"].as_i64().unwrap() - now_ms() <= 300_000);
    for bad in [
        du.replacen("original", "thumb", 1),
        du.replacen("exp=", "exp=1", 1),
        du.replacen("type=", "type=text%2Fhtml%3B", 1),
    ] {
        assert_eq!(srv.get(&bad, None).await.0, 403, "{bad}");
    }
    // Browsers on another site may use them (the signature is the permission).
    let pre = srv
        .http
        .request(reqwest::Method::OPTIONS, format!("{}{}", srv.base, u))
        .header("origin", "https://app.example")
        .header("access-control-request-method", "PUT")
        .send()
        .await
        .unwrap();
    assert_eq!(pre.status().as_u16(), 204);
    assert_eq!(pre.headers()["access-control-allow-origin"], "*");
    let (_, h, _) = srv.get(du, None).await;
    assert!(
        h["access-control-expose-headers"]
            .to_str()
            .unwrap()
            .contains("ETag")
    );
}

async fn a_file_is_carried_by_one_message(db: TestDb) {
    let srv = boot(&db).await;
    let hub = &srv.hub;
    let alice = user(hub, "alice", UserKind::Human, None).await;
    let bob = user(hub, "bob", UserKind::Human, None).await;
    let (a, b) = (Conn::new(hub, &alice), Conn::new(hub, &bob));
    let conv = a.group(&[&bob]).await;
    let other = a.group(&[&bob]).await;
    let f = srv.upload(&a, &conv, "a.png", &png(20, 20), None).await;
    let fid = f["file_id"].as_str().unwrap().to_string();
    // Not theirs, not here, not a payload.
    assert_eq!(
        code(&send_file(&b, &conv, "image", &fid, None).await),
        "forbidden"
    );
    assert_eq!(
        code(&send_file(&a, &other, "image", &fid, None).await),
        "forbidden"
    );
    assert_eq!(
        code(&send_file(&a, &conv, "image", "f_nope", None).await),
        "not_found"
    );
    for bad in [
        json!(null),
        json!({}),
        json!({"file_id": 5}),
        json!({"file_id": fid, "extra": 1}),
        json!({"file_id": fid, "caption": "x".repeat(4097)}),
    ] {
        let r = a
            .call(json!({"op": "send", "conv_id": conv, "client_msg_id": uuid::Uuid::new_v4().to_string(), "text": "x", "kind": "image", "payload": bad}))
            .await;
        assert_eq!(code(&r), "bad_request", "{bad}");
    }
    let r = a
        .call(json!({"op": "send", "conv_id": conv, "client_msg_id": "c1", "text": "x", "kind": "image", "payload": {"file_id": fid}, "to_user": bob.id}))
        .await;
    assert_eq!(code(&r), "bad_request", "not to one member");
    let first = a
        .ok(json!({"op": "send", "conv_id": conv, "client_msg_id": "c2", "text": "[Image]", "kind": "image", "payload": {"file_id": fid, "caption": "one"}}))
        .await;
    // The same send again (a retry) is the same message; another is refused.
    let retry = a
        .ok(json!({"op": "send", "conv_id": conv, "client_msg_id": "c2", "text": "[Image]", "kind": "image", "payload": {"file_id": fid}}))
        .await;
    assert_eq!(retry["msg_id"], first["msg_id"]);
    assert_eq!(
        code(&send_file(&a, &conv, "image", &fid, None).await),
        "conflict"
    );
    // Edits change the caption only.
    let mid = first["msg_id"].as_str().unwrap();
    a.ok(json!({"op": "edit", "msg_id": mid, "text": "[Image]", "payload": {"file_id": fid, "caption": "better"}}))
        .await;
    let log = b.pull(&conv, 0).await;
    let m = msg_by_id(&log, mid).unwrap();
    assert_eq!(
        (
            m["payload"]["caption"].as_str(),
            m["payload"]["width"].as_i64()
        ),
        (Some("better"), Some(20))
    );
    let g = srv.upload(&a, &conv, "b.png", &png(5, 5), None).await;
    let r = a
        .call(
            json!({"op": "edit", "msg_id": mid, "text": "x", "payload": {"file_id": g["file_id"]}}),
        )
        .await;
    assert_eq!(code(&r), "bad_request");
    a.ok(json!({"op": "edit", "msg_id": mid, "text": "[Image]", "payload": {"file_id": fid}}))
        .await;
    assert!(msg_by_id(&b.pull(&conv, 0).await, mid).unwrap()["payload"]["caption"].is_null());
}

async fn files_of_drafts_wait_for_approval(db: TestDb) {
    let srv = boot(&db).await;
    let hub = &srv.hub;
    let alice = user(hub, "alice", UserKind::Human, None).await;
    let bob = user(hub, "bob", UserKind::Human, None).await;
    let bot = user(hub, "bot", UserKind::Agent, Some(&alice)).await;
    let (a, b, g) = (
        Conn::new(hub, &alice),
        Conn::new(hub, &bob),
        Conn::new(hub, &bot),
    );
    let conv = a.group(&[&bob, &bot]).await;
    a.ok(json!({"op": "set_agent_policy", "conv_id": conv, "agent_id": bot.id, "approval": true}))
        .await;
    for approve in [true, false] {
        let f = srv.upload(&g, &conv, "chart.png", &png(30, 30), None).await;
        let fid = f["file_id"].as_str().unwrap().to_string();
        let d = send_file(&g, &conv, "image", &fid, None).await;
        assert_eq!(d["state"], "pending_approval", "{d}");
        let draft = d["draft_id"].as_str().unwrap();
        // The approver sees it; the other member does not, yet.
        assert_eq!(url(&a, &conv, &fid, "original").await["ok"], true);
        assert_eq!(code(&url(&b, &conv, &fid, "original").await), "forbidden");
        if approve {
            a.ok(json!({"op": "approve", "draft_id": draft})).await;
            assert_eq!(url(&b, &conv, &fid, "original").await["ok"], true);
            let log = b.pull(&conv, 0).await;
            assert!(log.to_string().contains(&fid));
        } else {
            a.ok(json!({"op": "reject", "draft_id": draft})).await;
            assert_eq!(code(&url(&a, &conv, &fid, "original").await), "not_found");
            hub.files_upkeep().await.unwrap();
            assert!(
                !srv.stored(&conv, &fid),
                "the rejected draft's file is deleted"
            );
        }
    }
}

async fn what_throws_a_file_away(db: TestDb) {
    let srv = boot(&db).await;
    let hub = &srv.hub;
    let alice = user(hub, "alice", UserKind::Human, None).await;
    let bob = user(hub, "bob", UserKind::Human, None).await;
    let (a, b) = (Conn::new(hub, &alice), Conn::new(hub, &bob));
    let conv = a.group(&[&bob]).await;
    let sent = |c: &'static str| {
        let (a, srv, conv) = (&a, &srv, conv.clone());
        async move {
            let f = srv.upload(a, &conv, c, &png(8, 8), None).await;
            let fid = f["file_id"].as_str().unwrap().to_string();
            let m = send_file(a, &conv, "image", &fid, None).await;
            (fid, m["msg_id"].as_str().unwrap_or_default().to_string())
        }
    };
    // Recall: no address works any more, the objects go.
    let (fid, mid) = sent("r.png").await;
    let old = url(&b, &conv, &fid, "original").await["url"]
        .as_str()
        .unwrap()
        .to_string();
    a.ok(json!({"op": "recall", "msg_id": mid})).await;
    assert_eq!(code(&url(&b, &conv, &fid, "original").await), "not_found");
    assert!(srv.stored(&conv, &fid));
    assert!(hub.files_upkeep().await.unwrap() >= 1);
    assert!(!srv.stored(&conv, &fid));
    assert_eq!(
        srv.get(&old, None).await.0,
        404,
        "an address handed out before is dead"
    );
    // Expiry: an open upload and a file nobody sent, a day later.
    let open = a
        .ok(json!({"op": "upload_start", "conv_id": conv, "name": "o", "size": 5}))
        .await;
    srv.put(&open["parts"][0], vec![1; 5]).await;
    let unsent = srv.upload(&a, &conv, "u.png", &png(8, 8), None).await;
    let (kept, _) = sent("kept.png").await;
    let later = now_ms() + 25 * 3_600_000;
    hub.shard(0)
        .unwrap()
        .call(Op::FileSweep { now: later })
        .await
        .unwrap();
    hub.files_upkeep().await.unwrap();
    assert!(!srv.stored(&conv, open["file_id"].as_str().unwrap()));
    assert!(!srv.stored(&conv, unsent["file_id"].as_str().unwrap()));
    assert!(srv.stored(&conv, &kept), "a sent file stays");
    assert_eq!(url(&b, &conv, &kept, "original").await["ok"], true);
    // Account deletion: what they uploaded goes.
    let (bf, _) = {
        let f = srv.upload(&b, &conv, "bob.png", &png(8, 8), None).await;
        let fid = f["file_id"].as_str().unwrap().to_string();
        let m = send_file(&b, &conv, "image", &fid, None).await;
        (fid, m)
    };
    hub.delete_account(&bob, None).await.unwrap();
    for _ in 0..50 {
        if !srv.stored(&conv, &bf) {
            break;
        }
        hub.files_upkeep().await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert!(!srv.stored(&conv, &bf), "a deleted account's files go");
    assert!(srv.stored(&conv, &kept), "others' stay");
    // Dissolve: everything of the conversation goes.
    a.ok(json!({"op": "dissolve", "conv_id": conv})).await;
    hub.files_upkeep().await.unwrap();
    assert!(!srv.stored(&conv, &kept));
}

async fn large_files_go_in_parts(db: TestDb) {
    let srv = boot(&db).await;
    let hub = &srv.hub;
    let alice = user(hub, "alice", UserKind::Human, None).await;
    let bob = user(hub, "bob", UserKind::Human, None).await;
    let (a, b) = (Conn::new(hub, &alice), Conn::new(hub, &bob));
    let conv = a.group(&[&bob]).await;
    let part = agentim_core::filestore::PART_BYTES as usize;
    let data: Vec<u8> = (0..part * 2 + 1000).map(|i| (i % 251) as u8).collect();
    let s = a
        .ok(json!({"op": "upload_start", "conv_id": conv, "name": "big.bin", "size": data.len()}))
        .await;
    assert_eq!(s["parts_total"], 3);
    assert_eq!(s["parts"].as_array().unwrap().len(), 3);
    // Parts out of order, one twice; more addresses on request.
    let more = a
        .ok(json!({"op": "upload_parts", "conv_id": conv, "file_id": s["file_id"], "from": 3}))
        .await;
    assert_eq!(more["parts"].as_array().unwrap().len(), 1);
    assert_eq!(more["parts"][0]["n"], "3");
    assert_eq!(
        code(
            &a.call(
                json!({"op": "upload_parts", "conv_id": conv, "file_id": s["file_id"], "from": 4})
            )
            .await
        ),
        "bad_request"
    );
    assert_eq!(
        code(
            &a.call(
                json!({"op": "upload_parts", "conv_id": conv, "file_id": s["file_id"], "from": 0})
            )
            .await
        ),
        "bad_request"
    );
    let slice = |n: usize| data[(n - 1) * part..(n * part).min(data.len())].to_vec();
    srv.put(&more["parts"][0], slice(3)).await;
    srv.put(&s["parts"][0], slice(1)).await;
    // Completing with a part missing: refused, and the upload is gone.
    let r = a
        .call(
            json!({"op": "upload_complete", "conv_id": conv, "file_id": s["file_id"], "parts": []}),
        )
        .await;
    assert_eq!(code(&r), "bad_request");
    // Again, whole.
    let r = srv.upload(&a, &conv, "big.bin", &data, None).await;
    assert_eq!(r["size"], data.len(), "{r}");
    let fid = r["file_id"].as_str().unwrap().to_string();
    assert_eq!(
        code(
            &a.call(json!({"op": "upload_parts", "conv_id": conv, "file_id": fid}))
                .await
        ),
        "conflict"
    );
    send_file(&a, &conv, "file", &fid, None).await;
    let u = url(&b, &conv, &fid, "original").await;
    let (_, _, body) = srv.get(u["url"].as_str().unwrap(), None).await;
    assert!(body == data, "the parts join up");
    assert!(
        !srv.dir
            .path()
            .join("files")
            .join(&conv)
            .join(&fid)
            .join("parts")
            .exists()
    );
}

async fn files_survive_a_restart(db: TestDb) {
    let dir = tempfile::tempdir().unwrap();
    let (alice, bob, conv, fid, open_id) = {
        let hub = open(cfg(&db, dir.path())).await;
        let base = serve(hub.clone()).await;
        let srv = Srv {
            base,
            hub: hub.clone(),
            http: agentim_core::webhook::client(),
            dir: tempfile::tempdir().unwrap(),
        };
        let alice = user(&hub, "alice", UserKind::Human, None).await;
        let bob = user(&hub, "bob", UserKind::Human, None).await;
        let a = Conn::new(&hub, &alice);
        let conv = a.group(&[&bob]).await;
        let f = srv.upload(&a, &conv, "s.png", &png(9, 9), None).await;
        let fid = f["file_id"].as_str().unwrap().to_string();
        send_file(&a, &conv, "image", &fid, None).await;
        let o = a
            .ok(json!({"op": "upload_start", "conv_id": conv, "name": "half", "size": 4}))
            .await;
        srv.put(&o["parts"][0], vec![1; 4]).await;
        (alice, bob, conv, fid, o)
    };
    let hub = open(cfg(&db, dir.path())).await;
    let base = serve(hub.clone()).await;
    let srv = Srv {
        base,
        hub: hub.clone(),
        http: agentim_core::webhook::client(),
        dir,
    };
    let (a, b) = (Conn::new(&hub, &alice), Conn::new(&hub, &bob));
    // The same signing key: addresses of before still work; reads work.
    let u = url(&b, &conv, &fid, "original").await;
    assert_eq!(srv.get(u["url"].as_str().unwrap(), None).await.0, 200);
    // The open upload carries on.
    let r = a
        .call(json!({"op": "upload_complete", "conv_id": conv, "file_id": open_id["file_id"], "parts": [{"n": 1}]}))
        .await;
    assert_eq!(r["size"], 4, "{r}");
}

/// Two messages for one file, many times: exactly one takes it.
async fn race_messages_for_one_file(db: TestDb) {
    let srv = boot(&db).await;
    let hub = &srv.hub;
    let alice = user(hub, "alice", UserKind::Human, None).await;
    let a = Conn::new(hub, &alice);
    let conv = a.group(&[]).await;
    for round in 0..15 {
        let f = srv.upload(&a, &conv, "r.png", &png(4, 4), None).await;
        let fid = f["file_id"].as_str().unwrap().to_string();
        let tasks: Vec<_> = (0..4)
            .map(|_| {
                let (h, u, conv, fid) = (hub.clone(), alice.clone(), conv.clone(), fid.clone());
                tokio::spawn(async move {
                    send_file(&Conn::new(&h, &u), &conv, "image", &fid, None).await
                })
            })
            .collect();
        let mut won = 0;
        for t in tasks {
            let r = t.await.unwrap();
            if r["ok"] == true {
                won += 1;
            } else {
                assert_eq!(code(&r), "conflict", "round {round}: {r}");
            }
        }
        assert_eq!(won, 1, "round {round}");
        let log = a.pull(&conv, 0).await;
        let carrying = log["events"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| {
                e["event"]["type"] == "msg.created"
                    && e["event"]["msg"]["payload"]["file_id"] == json!(fid)
            })
            .count();
        assert_eq!(carrying, 1, "round {round}: one message in the log");
    }
}

/// A sweep a day later and a send at the same moment: the file is either
/// sent and kept, or thrown away and the send refused; never sent and gone.
async fn race_sweeps_against_sends(db: TestDb) {
    let srv = boot(&db).await;
    let hub = &srv.hub;
    let alice = user(hub, "alice", UserKind::Human, None).await;
    let a = Conn::new(hub, &alice);
    let conv = a.group(&[]).await;
    let (mut sent, mut swept) = (0, 0);
    for round in 0..20 {
        let f = srv.upload(&a, &conv, "s.png", &png(4, 4), None).await;
        let fid = f["file_id"].as_str().unwrap().to_string();
        // Who is likely first changes from round to round.
        let (send_wait, sweep_wait) = [(3u64, 0u64), (0, 3), (0, 0)][round % 3];
        let send = {
            let (h, u, conv, fid) = (hub.clone(), alice.clone(), conv.clone(), fid.clone());
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(send_wait)).await;
                send_file(&Conn::new(&h, &u), &conv, "image", &fid, None).await
            })
        };
        let sweep = {
            let h = hub.clone();
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(sweep_wait)).await;
                h.shard(0)
                    .unwrap()
                    .call(Op::FileSweep {
                        now: now_ms() + 25 * 3_600_000,
                    })
                    .await
            })
        };
        let r = send.await.unwrap();
        sweep.await.unwrap().unwrap();
        hub.files_upkeep().await.unwrap();
        let readable = url(&a, &conv, &fid, "original").await["ok"] == true;
        if r["ok"] == true {
            sent += 1;
            assert!(
                readable && srv.stored(&conv, &fid),
                "round {round}: a sent file stays"
            );
        } else {
            swept += 1;
            assert_eq!(code(&r), "not_found", "round {round}: {r}");
            assert!(!readable && !srv.stored(&conv, &fid), "round {round}");
        }
    }
    assert_eq!(sent + swept, 20);
    eprintln!("sent {sent}, swept {swept}");
}

/// A developer's backend uploads as a member or the system, sends the file
/// and reads it back (audited); scopes and actions are checked.
async fn the_open_api_uploads_and_reads_files(db: TestDb) {
    let srv = boot(&db).await;
    let hub = &srv.hub;
    let alice = user(hub, "alice", UserKind::Human, None).await;
    let a = Conn::new(hub, &alice);
    let conv = a.group(&[]).await;
    let all = ["messages:write", "messages:read"].map(String::from);
    let (key, _) = hub.create_api_key("files", &all, None, None).await.unwrap();
    let (read_only, _) = hub
        .create_api_key("r", &["messages:read".to_string()], None, None)
        .await
        .unwrap();
    let call = |method: &str, path: String, key: &str, body: Option<Value>| {
        let mut r = srv
            .http
            .request(method.parse().unwrap(), format!("{}{path}", srv.base))
            .header("authorization", format!("Bearer {key}"));
        if let Some(b) = body {
            r = r
                .header("content-type", "application/json")
                .body(b.to_string());
        }
        async move {
            let resp = r.send().await.unwrap();
            let st = resp.status().as_u16();
            let text = resp.text().await.unwrap_or_default();
            (
                st,
                serde_json::from_str::<Value>(&text).unwrap_or(Value::Null),
            )
        }
    };
    let data = png(40, 30);
    let (st, s) = call(
        "POST",
        format!("/v1/conversations/{conv}/uploads"),
        &key,
        Some(json!({"as": alice.name, "name": "p.png", "size": data.len()})),
    )
    .await;
    assert_eq!(st, 201, "{s}");
    let fid = s["file_id"].as_str().unwrap().to_string();
    let (st, more) = call(
        "GET",
        format!(
            "/v1/conversations/{conv}/uploads/{fid}?as={}&from=1",
            alice.name
        ),
        &key,
        None,
    )
    .await;
    assert_eq!((st, more["parts"].as_array().map(Vec::len)), (200, Some(1)));
    let (_, etag) = srv.put(&s["parts"][0], data.clone()).await;
    let (st, f) = call(
        "POST",
        format!("/v1/conversations/{conv}/uploads/{fid}:complete"),
        &key,
        Some(json!({"as": alice.name, "parts": [{"n": 1, "etag": etag}]})),
    )
    .await;
    assert_eq!((st, f["kind"].as_str()), (200, Some("image")), "{f}");
    let (st, m) = call(
        "POST",
        format!("/v1/conversations/{conv}/messages"),
        &key,
        Some(json!({"as": alice.name, "text": "[Image]", "kind": "image", "payload": {"file_id": fid}})),
    )
    .await;
    assert_eq!(st, 201, "{m}");
    // The system reads any file; it is audited.
    let (st, u) = call(
        "GET",
        format!("/v1/conversations/{conv}/files/{fid}?variant=original"),
        &read_only,
        None,
    )
    .await;
    assert_eq!(st, 200, "{u}");
    assert_eq!(srv.get(u["url"].as_str().unwrap(), None).await.2, data);
    let log = hub.audit_log(100).await.unwrap();
    assert!(log.iter().any(|e| e["action"] == "api.files.read"));
    assert!(log.iter().any(|e| e["action"] == "api.files.upload"));
    // Scopes, unknown actions, unknown files.
    let (st, _) = call(
        "POST",
        format!("/v1/conversations/{conv}/uploads"),
        &read_only,
        Some(json!({"as": alice.name, "name": "x", "size": 1})),
    )
    .await;
    assert_eq!(st, 403);
    let (st, _) = call(
        "POST",
        format!("/v1/conversations/{conv}/uploads/{fid}:explode"),
        &key,
        Some(json!({"as": alice.name})),
    )
    .await;
    assert_eq!(st, 404);
    let (st, e) = call(
        "GET",
        format!("/v1/conversations/{conv}/files/f_nope"),
        &key,
        None,
    )
    .await;
    assert_eq!((st, e["code"].as_str()), (404, Some("not_found")));
    let (st, _) = call(
        "POST",
        format!("/v1/conversations/{conv}/uploads"),
        &key,
        Some(json!({"name": "x", "size": 1})),
    )
    .await;
    assert_eq!(st, 400, "as is required");
}

/// A JPEG with EXIF (a GPS position and orientation 6) inserted after SOI.
fn jpeg_with_gps(w: u32, h: u32) -> Vec<u8> {
    let plain = jpeg(w, h);
    let mut tiff = b"MM\0\x2a\0\0\0\x08".to_vec();
    tiff.extend_from_slice(&2u16.to_be_bytes());
    for (tag, ty, val) in [(0x8825u16, 4u16, 99u32), (0x0112, 3, 6 << 16)] {
        tiff.extend_from_slice(&tag.to_be_bytes());
        tiff.extend_from_slice(&ty.to_be_bytes());
        tiff.extend_from_slice(&1u32.to_be_bytes());
        tiff.extend_from_slice(&val.to_be_bytes());
    }
    tiff.extend_from_slice(&0u32.to_be_bytes());
    tiff.extend_from_slice(b"GPSLatitude 37.7749");
    let mut seg = vec![0xFF, 0xE1];
    seg.extend_from_slice(&((2 + 6 + tiff.len()) as u16).to_be_bytes());
    seg.extend_from_slice(b"Exif\0\0");
    seg.extend_from_slice(&tiff);
    let mut out = plain[..2].to_vec();
    out.extend(seg);
    out.extend_from_slice(&plain[2..]);
    out
}

/// Agents (`/api/files`) and backends (`POST /v1/conversations/{id}/files`)
/// send whole files: the position goes, the orientation stays, a thumbnail
/// is made; people cannot use the agent door; the body is refused before
/// it is read without a key.
async fn whole_files_lose_their_location_and_get_a_thumbnail(db: TestDb) {
    let srv = boot(&db).await;
    let hub = &srv.hub;
    let alice = user(hub, "alice", UserKind::Human, None).await;
    let a = Conn::new(hub, &alice);
    let made = hub.create_agent(&alice, "bot").await.unwrap();
    let key = made["key"].as_str().unwrap().to_string();
    let bot = hub.user_by_name("bot").unwrap();
    let conv = a.group(&[&bot]).await;
    let photo = jpeg_with_gps(800, 600);
    assert!(photo.windows(11).any(|w| w == b"GPSLatitude"));
    let post = |key: &str, q: String, body: Vec<u8>| {
        let r = srv
            .http
            .post(format!("{}/api/files?{q}", srv.base))
            .header("authorization", format!("Bearer {key}"))
            .body(body);
        async move {
            let resp = r.send().await.unwrap();
            let st = resp.status().as_u16();
            (
                st,
                serde_json::from_str::<Value>(&resp.text().await.unwrap()).unwrap_or(Value::Null),
            )
        }
    };
    let (st, f) = post(
        &key,
        format!("conv_id={conv}&name=photo.jpg"),
        photo.clone(),
    )
    .await;
    assert_eq!(st, 201, "{f}");
    assert_eq!(
        (
            f["kind"].as_str(),
            f["thumb"].as_bool(),
            f["ai_generated"].as_bool()
        ),
        (Some("image"), Some(true), Some(true))
    );
    let fid = f["file_id"].as_str().unwrap().to_string();
    let g = Conn::new(hub, &bot);
    send_file(&g, &conv, "image", &fid, None).await;
    let u = url(&a, &conv, &fid, "original").await;
    let (_, _, stored) = srv.get(u["url"].as_str().unwrap(), None).await;
    assert!(
        !stored.windows(11).any(|w| w == b"GPSLatitude"),
        "no position stored"
    );
    assert_eq!(
        agentim_core::media::orientation(&stored),
        Some(6),
        "still the right way up"
    );
    assert_eq!(
        stored.len(),
        photo.len() - (photo.len() - jpeg(800, 600).len()) + 36,
        "only the EXIF block changed"
    );
    let t = url(&a, &conv, &fid, "thumb").await;
    let (_, _, th) = srv.get(t["url"].as_str().unwrap(), None).await;
    // Turned upright: 600x800 shown, fit into 320.
    assert_eq!(agentim_core::filestore::image_size(&th), Some((240, 320)));
    // A person's token, no key, a wrong key: refused.
    let tok = client_token(hub, &alice).await;
    assert_eq!(
        post(&tok, format!("conv_id={conv}&name=x"), vec![1])
            .await
            .0,
        401
    );
    assert_eq!(
        post("", format!("conv_id={conv}&name=x"), vec![1]).await.0,
        401
    );
    assert_eq!(
        post("atk_wrong", format!("conv_id={conv}&name=x"), vec![1])
            .await
            .0,
        401
    );
    // Too large for one request: refused.
    let (st, _) = post(
        &key,
        format!("conv_id={conv}&name=big"),
        vec![0; (64 << 20) + 1],
    )
    .await;
    assert_eq!(st, 413);
    // A backend through the open API.
    let (api, _) = hub
        .create_api_key("b", &["messages:write".to_string()], None, None)
        .await
        .unwrap();
    let r = srv
        .http
        .post(format!(
            "{}/v1/conversations/{conv}/files?as=alice&name=scan.jpg",
            srv.base
        ))
        .header("authorization", format!("Bearer {api}"))
        .body(photo)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 201);
    let v: Value = serde_json::from_str(&r.text().await.unwrap()).unwrap();
    assert_eq!(
        (v["kind"].as_str(), v["ai_generated"].as_bool()),
        (Some("image"), Some(false))
    );
}
