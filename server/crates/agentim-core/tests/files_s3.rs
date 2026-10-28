//! Files on an S3 compatible store (docs/02-architecture.md §2.7): clients
//! PUT to presigned addresses (one PUT, or parts of a multipart upload),
//! the server checks the objects, downloads come straight from the store,
//! and thrown away files are deleted there. Runs when AGENTIM_TEST_S3 is the
//! address of an S3 compatible server with a bucket `agentim-test` and the
//! keys `test` / `test` (CI: moto; MinIO works too). moto ignores the
//! `response-content-type` and `response-content-disposition` overrides AWS
//! and MinIO apply; set AGENTIM_TEST_S3_OVERRIDES=1 to check them too.

mod common;

use agentim_core::config::Config;
use agentim_core::hub::Hub;
use agentim_core::model::*;
use common::*;
use serde_json::{Value, json};

backend_tests!(files_go_straight_to_the_store);

fn s3() -> Option<String> {
    std::env::var("AGENTIM_TEST_S3")
        .ok()
        .filter(|s| !s.is_empty())
}

fn overrides() -> bool {
    std::env::var("AGENTIM_TEST_S3_OVERRIDES").is_ok_and(|v| v == "1")
}

fn cfg(db: &TestDb, endpoint: &str, prefix: &str) -> Config {
    // SAFETY: every test of this file sets the same values.
    unsafe {
        std::env::set_var("AGENTIM_S3_ACCESS_KEY_ID", "test");
        std::env::set_var("AGENTIM_S3_SECRET_ACCESS_KEY", "test");
    }
    let mut c = single(&db.url);
    c.files.storage = "s3".into();
    c.files.s3.endpoint = endpoint.into();
    c.files.s3.bucket = "agentim-test".into();
    c.files.s3.region = "us-east-1".into();
    c.files.s3.path_style = true;
    c.files.s3.prefix = prefix.into();
    c
}

fn png(w: u32, h: u32) -> Vec<u8> {
    let mut out = vec![];
    image::RgbImage::from_fn(w, h, |x, y| {
        image::Rgb([(x * 7 % 256) as u8, (y % 256) as u8, 1])
    })
    .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
    .unwrap();
    out
}

async fn put(http: &reqwest::Client, t: &Value, body: Vec<u8>) -> (u16, String) {
    let r = http
        .put(t["url"].as_str().unwrap())
        .body(body)
        .send()
        .await
        .unwrap();
    let etag = r
        .headers()
        .get("etag")
        .map(|v| v.to_str().unwrap().to_string())
        .unwrap_or_default();
    (r.status().as_u16(), etag)
}

async fn upload(
    http: &reqwest::Client,
    c: &Conn,
    conv: &str,
    data: &[u8],
    thumb: Option<&[u8]>,
) -> Value {
    let mut start =
        json!({"op": "upload_start", "conv_id": conv, "name": "x.png", "size": data.len()});
    if let Some(t) = thumb {
        start["thumb_size"] = json!(t.len());
    }
    let s = c.ok(start).await;
    assert_eq!(s["direct"], true);
    let part = s["part_size"].as_u64().unwrap() as usize;
    let mut tags = vec![];
    let mut from = 1;
    loop {
        let batch = if from == 1 {
            s.clone()
        } else {
            c.ok(json!({"op": "upload_parts", "conv_id": conv, "file_id": s["file_id"], "from": from}))
                .await
        };
        for t in batch["parts"].as_array().unwrap() {
            let n: usize = t["n"].as_str().unwrap().parse().unwrap();
            assert!(
                t["url"].as_str().unwrap().starts_with("http"),
                "an address of the store"
            );
            let (st, etag) = put(
                http,
                t,
                data[(n - 1) * part..(n * part).min(data.len())].to_vec(),
            )
            .await;
            assert_eq!(st, 200, "part {n}");
            tags.push(json!({"n": n, "etag": etag}));
            from = n + 1;
        }
        if from as u64 > s["parts_total"].as_u64().unwrap() {
            break;
        }
    }
    if let Some(t) = thumb {
        assert_eq!(put(http, &s["thumb"], t.to_vec()).await.0, 200);
    }
    c.call(
        json!({"op": "upload_complete", "conv_id": conv, "file_id": s["file_id"], "parts": tags}),
    )
    .await
}

async fn files_go_straight_to_the_store(db: TestDb) {
    let Some(endpoint) = s3() else {
        eprintln!("skipped: AGENTIM_TEST_S3 not set");
        return;
    };
    let prefix = format!("t{}", uuid::Uuid::new_v4().simple());
    let hub: Hub = open(cfg(&db, &endpoint, &prefix)).await;
    let http = agentim_core::webhook::client();
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let (a, b) = (Conn::new(&hub, &alice), Conn::new(&hub, &bob));
    let conv = a.group(&[&bob]).await;
    // A small image in one PUT, with its thumbnail.
    let (orig, thumb) = (png(500, 400), png(320, 256));
    let f = upload(&http, &a, &conv, &orig, Some(&thumb)).await;
    assert_eq!(
        (f["kind"].as_str(), f["width"].as_i64()),
        (Some("image"), Some(500)),
        "{f}"
    );
    let fid = f["file_id"].as_str().unwrap().to_string();
    a.ok(json!({"op": "send", "conv_id": conv, "client_msg_id": "c1", "text": "[Image]", "kind": "image", "payload": {"file_id": fid}}))
        .await;
    // Downloads come from the store, with the type and name.
    for (variant, want) in [("thumb", &thumb), ("original", &orig)] {
        let u = b
            .ok(json!({"op": "file_url", "conv_id": conv, "file_id": fid, "variant": variant}))
            .await;
        let url = u["url"].as_str().unwrap();
        assert!(url.starts_with(&endpoint), "{url}");
        let r = http.get(url).send().await.unwrap();
        assert_eq!(r.status().as_u16(), 200);
        if overrides() {
            assert_eq!(r.headers()["content-type"], "image/png");
            assert!(
                r.headers()["content-disposition"]
                    .to_str()
                    .unwrap()
                    .starts_with("inline")
            );
        }
        assert!(r.bytes().await.unwrap().to_vec() == *want, "{variant}");
    }
    // A length other than the signed one: AWS refuses the PUT; a lenient
    // store takes it, and completion refuses it then.
    let s = a
        .ok(json!({"op": "upload_start", "conv_id": conv, "name": "y", "size": 10}))
        .await;
    let (st, etag) = put(&http, &s["parts"][0], vec![1; 11]).await;
    if st == 200 {
        let r = a
            .call(json!({"op": "upload_complete", "conv_id": conv, "file_id": s["file_id"], "parts": [{"n": 1, "etag": etag}]}))
            .await;
        assert_eq!(r["error"]["code"], "bad_request", "{r}");
    }
    // A large file in parts (multipart), joined by the store.
    let big: Vec<u8> = (0..(64 << 20) + 5).map(|i| (i % 253) as u8).collect();
    let g = upload(&http, &a, &conv, &big, None).await;
    assert_eq!(
        (g["size"].as_u64(), g["kind"].as_str()),
        (Some(big.len() as u64), Some("file")),
        "{g}"
    );
    let gid = g["file_id"].as_str().unwrap().to_string();
    a.ok(json!({"op": "send", "conv_id": conv, "client_msg_id": "c2", "text": "[File]", "kind": "file", "payload": {"file_id": gid}}))
        .await;
    let u = b
        .ok(json!({"op": "file_url", "conv_id": conv, "file_id": gid}))
        .await;
    let r = http.get(u["url"].as_str().unwrap()).send().await.unwrap();
    if overrides() {
        assert!(
            r.headers()["content-disposition"]
                .to_str()
                .unwrap()
                .starts_with("attachment")
        );
    }
    // The address asks for them either way.
    assert!(
        u["url"]
            .as_str()
            .unwrap()
            .contains("response-content-disposition=attachment")
    );
    assert!(
        r.bytes().await.unwrap().to_vec() == big,
        "the parts join up"
    );
    // Dissolved: the objects are deleted from the store.
    a.ok(json!({"op": "dissolve", "conv_id": conv})).await;
    hub.files_upkeep().await.unwrap();
    let st = hub.file_store();
    for id in [&fid, &gid] {
        assert_eq!(st.size(&conv, id, "original").await.unwrap(), None, "{id}");
    }
    assert_eq!(st.size(&conv, &fid, "thumb").await.unwrap(), None);
}
