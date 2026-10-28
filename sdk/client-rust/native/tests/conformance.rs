//! The native runtime against a real `agentim` (docs/12 §2.13 protocol
//! conformance): two people talk, the offline queue survives a process
//! restart and is sent once, the client rides out a server restart,
//! history comes back page by page on a new device, a deactivated person
//! stops retrying, and the C ABI works from C.
//!
//! The server binary: `AGENTIM_BIN` (default `../../../server/target/debug/agentim`).

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use agentim_client::{Client, Config};
use serde_json::{Value, json};

fn bin() -> PathBuf {
    std::env::var("AGENTIM_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../server/target/debug/agentim")
        })
}

struct Server {
    child: Option<Child>,
    db: PathBuf,
    port: u16,
    _dir: tempfile::TempDir,
}

impl Server {
    fn start() -> Server {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("s.db");
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let mut s = Server {
            child: None,
            db,
            port,
            _dir: dir,
        };
        s.run();
        s.cli(&["signing-key", "create", "--name", "tests"]);
        s
    }

    fn run(&mut self) {
        // In its own folder: files go to ./data/files there.
        let child = Command::new(bin())
            .current_dir(self._dir.path())
            .args([
                "--db",
                self.db.to_str().unwrap(),
                "--addr",
                &format!("127.0.0.1:{}", self.port),
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("agentim binary (build the server first)");
        self.child = Some(child);
        let end = Instant::now() + Duration::from_secs(20);
        while std::net::TcpStream::connect(("127.0.0.1", self.port)).is_err() {
            assert!(Instant::now() < end, "server did not start");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn stop(&mut self) {
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }

    fn cli(&self, args: &[&str]) -> Value {
        let out = Command::new(bin())
            .args(args)
            .args(["--db", self.db.to_str().unwrap()])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).unwrap_or(Value::Null)
    }

    fn token(&self, user: &str) -> String {
        let out = Command::new(bin())
            .args([
                "token",
                "sign",
                "--user",
                user,
                "--db",
                self.db.to_str().unwrap(),
            ])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop();
    }
}

/// A person's app: the client plus everything it was told.
struct App {
    client: Client,
    rx: mpsc::Receiver<Value>,
    seen: Vec<Value>,
    next: u64,
    user: String,
    token: String,
}

impl App {
    fn new(s: &Server, user: &str, device: &str, dir: Option<PathBuf>) -> App {
        let (tx, rx) = mpsc::channel();
        let client = Client::start(
            Config {
                url: s.url(),
                device_id: device.into(),
                device_name: Some(format!("{user} test")),
                platform: Some("test".into()),
                client_name: "conformance".into(),
                client_version: "1".into(),
                seed: 0,
            },
            dir,
            Box::new(move |v: &Value| {
                let _ = tx.send(v.clone());
            }),
        );
        App {
            client,
            rx,
            seen: vec![],
            next: 0,
            user: user.into(),
            token: s.token(user),
        }
    }

    /// Takes events in, answering token requests, until `f` holds.
    fn until(&mut self, what: &str, secs: u64, f: impl Fn(&[Value]) -> bool) {
        let end = Instant::now() + Duration::from_secs(secs);
        loop {
            if f(&self.seen) {
                return;
            }
            let left = end.saturating_duration_since(Instant::now());
            assert!(
                !left.is_zero(),
                "{} timed out waiting for {what}; last events: {:?}",
                self.user,
                self.seen.iter().rev().take(6).collect::<Vec<_>>()
            );
            if let Ok(v) = self.rx.recv_timeout(left.min(Duration::from_millis(200))) {
                if v["type"] == "need_token" {
                    self.client.token(Ok(self.token.clone()));
                }
                self.seen.push(v);
            }
        }
    }

    fn cmd(&mut self, c: Value) -> Result<Value, Value> {
        self.next += 1;
        let id = self.next;
        self.client.command(id, c.clone());
        self.until(&format!("reply to {c}"), 20, |s| {
            s.iter().any(|e| e["type"] == "reply" && e["id"] == id)
        });
        let r = self
            .seen
            .iter()
            .find(|e| e["type"] == "reply" && e["id"] == id)
            .unwrap()
            .clone();
        if r["ok"] == true {
            Ok(r["result"].clone())
        } else {
            Err(r["error"].clone())
        }
    }

    fn connect(&mut self) {
        self.cmd(json!({"cmd": "connect"})).unwrap();
        self.until("connected", 20, |s| {
            s.iter()
                .rev()
                .find(|e| e["type"] == "connection")
                .is_some_and(|e| e["state"] == "connected")
        });
    }

    fn state(&self) -> String {
        self.seen
            .iter()
            .rev()
            .find(|e| e["type"] == "connection")
            .map(|e| e["state"].as_str().unwrap().to_string())
            .unwrap_or_default()
    }

    fn me(&mut self) -> String {
        self.cmd(json!({"cmd": "debug_state"})).unwrap()["me"]["id"]
            .as_str()
            .unwrap()
            .to_string()
    }

    /// The conversation as the app shows it: the snapshot plus changes.
    fn view(&self, conv: &str) -> Vec<Value> {
        let mut m: std::collections::BTreeMap<String, Value> = Default::default();
        for e in &self.seen {
            if e["type"] == "reply"
                && e["result"]["conv_id"] == conv
                && e["result"]["messages"].is_array()
            {
                m.clear();
                for x in e["result"]["messages"].as_array().unwrap() {
                    m.insert(x["key"].as_str().unwrap().into(), x.clone());
                }
            }
            if e["type"] == "messages" && e["conv_id"] == conv {
                for ch in e["changes"].as_array().unwrap() {
                    if ch["op"] == "upsert" {
                        m.insert(
                            ch["message"]["key"].as_str().unwrap().into(),
                            ch["message"].clone(),
                        );
                    } else {
                        m.remove(ch["key"].as_str().unwrap());
                    }
                }
            }
        }
        let mut v: Vec<Value> = m.into_values().collect();
        v.sort_by_key(|x| x["order"].as_i64());
        v
    }

    fn texts(&self, conv: &str) -> Vec<(String, String)> {
        self.view(conv)
            .iter()
            .map(|m| {
                (
                    m["content"].as_str().unwrap_or("").to_string(),
                    m["status"].as_str().unwrap_or("").to_string(),
                )
            })
            .collect()
    }
}

fn wait_texts(a: &mut App, conv: &str, want: &[&str]) {
    let want: Vec<String> = want.iter().map(|s| s.to_string()).collect();
    let c = conv.to_string();
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let got: Vec<String> = a
            .texts(&c)
            .into_iter()
            .filter(|(_, s)| s == "sent")
            .map(|(t, _)| t)
            .collect();
        if got == want {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "{} sees {got:?}, want {want:?}",
            a.user
        );
        if let Ok(v) = a.rx.recv_timeout(Duration::from_millis(100)) {
            if v["type"] == "need_token" {
                a.client.token(Ok(a.token.clone()));
            }
            a.seen.push(v);
        }
    }
}

fn group(a: &mut App, members: &[String]) -> String {
    let r = a
        .cmd(json!({"cmd": "call", "op": "create_conv", "args": {"kind": "group", "title": "t", "members": members}}))
        .unwrap();
    r["conv_id"].as_str().unwrap().to_string()
}

#[test]
fn two_people_talk_edit_recall_and_read() {
    let s = Server::start();
    let mut alice = App::new(&s, "alice", "a1", None);
    let mut bob = App::new(&s, "bob", "b1", None);
    alice.connect();
    bob.connect();
    let bob_id = bob.me();
    let conv = group(&mut alice, &[bob_id]);
    alice
        .cmd(json!({"cmd": "messages", "conv_id": conv}))
        .unwrap();
    bob.until("the new conversation", 10, |s| {
        s.iter()
            .any(|e| e["type"] == "conversations" && e.to_string().contains(&conv))
    });
    bob.cmd(json!({"cmd": "messages", "conv_id": conv}))
        .unwrap();
    let r = alice
        .cmd(json!({"cmd": "send", "conv_id": conv, "text": "hello bob"}))
        .unwrap();
    assert_eq!(r["key"].as_str().unwrap().split(':').next(), Some("local"));
    wait_texts(&mut alice, &conv, &["hello bob"]);
    bob.cmd(json!({"cmd": "send", "conv_id": conv, "text": "hi alice"}))
        .unwrap();
    wait_texts(&mut alice, &conv, &["hello bob", "hi alice"]);
    wait_texts(&mut bob, &conv, &["hello bob", "hi alice"]);
    // Edits and recalls reach the other side.
    let mine = alice
        .view(&conv)
        .into_iter()
        .find(|m| m["content"] == "hello bob")
        .unwrap();
    alice
        .cmd(
            json!({"cmd": "edit", "conv_id": conv, "msg_id": mine["msg_id"], "text": "hello, bob"}),
        )
        .unwrap();
    wait_texts(&mut bob, &conv, &["hello, bob", "hi alice"]);
    let theirs = bob
        .view(&conv)
        .into_iter()
        .find(|m| m["content"] == "hi alice")
        .unwrap();
    bob.cmd(json!({"cmd": "recall", "conv_id": conv, "msg_id": theirs["msg_id"]}))
        .unwrap();
    wait_texts(&mut alice, &conv, &["hello, bob", ""]);
    assert_eq!(alice.view(&conv)[1]["state"], "recalled");
    // Read: the queue sends it; the list shows nothing unread.
    alice.cmd(json!({"cmd": "read", "conv_id": conv})).unwrap();
    alice.until("read sent", 10, |s| {
        s.iter()
            .any(|e| e["type"] == "operation" && e["kind"] == "read" && e["result"] == "done")
    });
    let list = alice.cmd(json!({"cmd": "conversations"})).unwrap();
    let c = list["conversations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["conv_id"] == conv.as_str())
        .unwrap()
        .clone();
    assert_eq!(c["unread"], 0);
    // Online-only requests go straight to the server.
    let p = alice.cmd(json!({"cmd": "call", "op": "ping"})).unwrap();
    assert!(p["t"].is_number());
    let e = alice
        .cmd(json!({"cmd": "call", "op": "get_request", "args": {"request_id": "nope"}}))
        .unwrap_err();
    assert_eq!(e["code"], "not_found");
}

/// A JPEG with EXIF: orientation 6 and a GPS block.
fn photo(w: u32, h: u32) -> Vec<u8> {
    let mut out = vec![];
    let img = image::RgbImage::from_fn(w, h, |x, y| {
        image::Rgb([(x % 256) as u8, (y % 256) as u8, 9])
    });
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 80)
        .encode_image(&img)
        .unwrap();
    let mut tiff = b"MM\0\x2a\0\0\0\x08".to_vec();
    tiff.extend_from_slice(&[0, 2]);
    tiff.extend_from_slice(&[0x01, 0x12, 0, 3, 0, 0, 0, 1, 0, 6, 0, 0]);
    tiff.extend_from_slice(&[0x88, 0x25, 0, 4, 0, 0, 0, 1, 0, 0, 0, 0x26]);
    tiff.extend_from_slice(&[0, 0, 0, 0]);
    tiff.extend_from_slice(b"GPS 51.5N 0.12W");
    let mut seg = b"Exif\0\0".to_vec();
    seg.extend_from_slice(&tiff);
    let mut j = out[..2].to_vec();
    j.extend_from_slice(&[0xFF, 0xE1]);
    j.extend_from_slice(&((seg.len() + 2) as u16).to_be_bytes());
    j.extend_from_slice(&seg);
    j.extend_from_slice(&out[2..]);
    j
}

fn download(url: &str) -> Vec<u8> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let r = reqwest::get(url).await.unwrap();
            assert!(r.status().is_success(), "{url}: {}", r.status());
            r.bytes().await.unwrap().to_vec()
        })
}

/// The message under `key` once it has `status` (`sent`, `failed`, …).
fn wait_status(a: &mut App, conv: &str, pred: impl Fn(&Value) -> bool, what: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if let Some(m) = a.view(conv).into_iter().find(|m| pred(m)) {
            return m;
        }
        assert!(
            Instant::now() < deadline,
            "{} timed out waiting for {what}: {:?}",
            a.user,
            a.view(conv)
        );
        if let Ok(v) = a.rx.recv_timeout(Duration::from_millis(100)) {
            if v["type"] == "need_token" {
                a.client.token(Ok(a.token.clone()));
            }
            a.seen.push(v);
        }
    }
}

#[test]
fn photos_and_files_go_up_without_location_and_resume_after_a_restart() {
    let s = Server::start();
    let store = tempfile::tempdir().unwrap();
    let pics = tempfile::tempdir().unwrap();
    let mut bob = App::new(&s, "bob", "b1", None);
    bob.connect();
    let bob_id = bob.me();
    let conv;
    // A 17 MiB file: two parts of 16 MiB; queued offline, then the
    // process ends before anything went up.
    let big: Vec<u8> = (0..(17u32 << 20)).map(|i| (i % 251) as u8).collect();
    let big_path = pics.path().join("data.bin");
    std::fs::write(&big_path, &big).unwrap();
    {
        let mut alice = App::new(&s, "alice", "a1", Some(store.path().into()));
        alice.connect();
        conv = group(&mut alice, &[bob_id]);
        alice.cmd(json!({"cmd": "close"})).unwrap();
        alice
            .cmd(json!({"cmd": "send_file", "conv_id": conv, "path": big_path, "mime": "application/octet-stream"}))
            .unwrap();
    }
    let mut alice = App::new(&s, "alice", "a1", Some(store.path().into()));
    alice
        .cmd(json!({"cmd": "messages", "conv_id": conv}))
        .unwrap();
    alice.connect();
    // A photo with its location, sent now (it does not wait for the big file).
    let p = pics.path().join("IMG_0001.jpg");
    std::fs::write(&p, photo(1200, 900)).unwrap();
    let r = alice
        .cmd(json!({"cmd": "send_file", "conv_id": conv, "path": p, "caption": "the view"}))
        .unwrap();
    let key = r["key"].as_str().unwrap().to_string();
    let local = alice
        .view(&conv)
        .into_iter()
        .find(|m| m["key"] == key.as_str())
        .unwrap();
    assert_eq!(local["kind"], "image");
    assert!(
        ["uploading", "sending", "sent"].contains(&local["status"].as_str().unwrap()),
        "{local}"
    );
    bob.cmd(json!({"cmd": "messages", "conv_id": conv}))
        .unwrap();
    let img = wait_status(&mut bob, &conv, |m| m["kind"] == "image", "the photo");
    let pl = &img["payload"];
    assert_eq!(pl["caption"], "the view");
    assert_eq!(
        (pl["width"].as_u64(), pl["height"].as_u64()),
        (Some(900), Some(1200)),
        "upright: {pl}"
    );
    assert_eq!(pl["thumb"], true);
    let fid = pl["file_id"].as_str().unwrap();
    // Bob reads it: the location is gone, the orientation stays.
    let u = bob
        .cmd(json!({"cmd": "file_url", "conv_id": conv, "file_id": fid}))
        .unwrap();
    let original = download(u["url"].as_str().unwrap());
    assert!(original.starts_with(&[0xFF, 0xD8]));
    assert!(!original.windows(3).any(|w| w == b"GPS"), "no location");
    assert_eq!(agentim_client_core::media::orientation(&original), Some(6));
    let t = bob
        .cmd(json!({"cmd": "file_url", "conv_id": conv, "file_id": fid, "variant": "thumb"}))
        .unwrap();
    let thumb = image::load_from_memory(&download(t["url"].as_str().unwrap())).unwrap();
    assert_eq!((thumb.width(), thumb.height()), (240, 320));
    // The big file arrives too, whole.
    let f = wait_status(&mut bob, &conv, |m| m["kind"] == "file", "the big file");
    assert_eq!(f["payload"]["size"], big.len() as u64);
    let u = bob
        .cmd(json!({"cmd": "file_url", "conv_id": conv, "file_id": f["payload"]["file_id"]}))
        .unwrap();
    assert!(
        download(u["url"].as_str().unwrap()) == big,
        "the parts join up"
    );
    // Alice's side settles; her copies without metadata are deleted.
    wait_status(
        &mut alice,
        &conv,
        |m| {
            m["kind"] == "image"
                && m["status"] == "sent"
                && !m["key"].as_str().unwrap().starts_with("local:")
        },
        "alice's photo stored",
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while std::fs::read_dir(store.path().join("uploads"))
        .map(|d| d.count())
        .unwrap_or(0)
        > 0
    {
        assert!(Instant::now() < deadline, "copies left in uploads/");
        std::thread::sleep(Duration::from_millis(100));
    }
    // A file that cannot be read is refused at once.
    let e = alice
        .cmd(json!({"cmd": "send_file", "conv_id": conv, "path": pics.path().join("nope")}))
        .unwrap_err();
    assert_eq!(e["code"], "bad_request");
    // Bob was never sent the original location (the source still has it).
    assert!(std::fs::read(&p).unwrap().windows(3).any(|w| w == b"GPS"));
}

#[test]
fn the_offline_queue_survives_a_restart_and_is_sent_once() {
    let s = Server::start();
    let store = tempfile::tempdir().unwrap();
    let mut bob = App::new(&s, "bob", "b1", None);
    bob.connect();
    let bob_id = bob.me();
    let conv;
    {
        let mut alice = App::new(&s, "alice", "a1", Some(store.path().into()));
        alice.connect();
        conv = group(&mut alice, &[bob_id]);
        alice.cmd(json!({"cmd": "close"})).unwrap();
        for t in ["one", "two", "three"] {
            alice
                .cmd(json!({"cmd": "send", "conv_id": conv, "text": t}))
                .unwrap();
        }
        // The process ends with the messages unsent.
    }
    let mut alice = App::new(&s, "alice", "a1", Some(store.path().into()));
    let r = alice
        .cmd(json!({"cmd": "messages", "conv_id": conv}))
        .unwrap();
    let shown: Vec<&str> = r["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["content"].as_str().unwrap())
        .collect();
    assert_eq!(
        shown,
        ["one", "two", "three"],
        "from the cache, before connecting"
    );
    assert!(
        r["messages"]
            .as_array()
            .unwrap()
            .iter()
            .all(|m| m["status"] == "sending")
    );
    alice.connect();
    wait_texts(&mut alice, &conv, &["one", "two", "three"]);
    bob.cmd(json!({"cmd": "messages", "conv_id": conv}))
        .unwrap();
    wait_texts(&mut bob, &conv, &["one", "two", "three"]);
    // Once more after another restart: nothing is sent again.
    drop(alice);
    let mut alice = App::new(&s, "alice", "a1", Some(store.path().into()));
    alice.connect();
    alice
        .cmd(json!({"cmd": "messages", "conv_id": conv}))
        .unwrap();
    std::thread::sleep(Duration::from_millis(500));
    wait_texts(&mut bob, &conv, &["one", "two", "three"]);
    assert_eq!(
        alice.cmd(json!({"cmd": "pending"})).unwrap()["ops"],
        json!([])
    );
}

#[test]
fn a_server_restart_is_ridden_out() {
    let mut s = Server::start();
    let mut alice = App::new(&s, "alice", "a1", None);
    let mut bob = App::new(&s, "bob", "b1", None);
    alice.connect();
    bob.connect();
    let bob_id = bob.me();
    let conv = group(&mut alice, &[bob_id]);
    alice
        .cmd(json!({"cmd": "messages", "conv_id": conv}))
        .unwrap();
    s.stop();
    alice.until("disconnected", 10, |s| {
        s.iter()
            .rev()
            .find(|e| e["type"] == "connection")
            .is_some_and(|e| e["state"] != "connected")
    });
    // Written while the server is down: queued.
    alice
        .cmd(json!({"cmd": "send", "conv_id": conv, "text": "during the outage"}))
        .unwrap();
    s.run();
    // Both come back by themselves; what was queued goes out.
    let back = |s: &[Value]| {
        s.iter()
            .filter(|e| e["type"] == "connection" && e["state"] == "connected")
            .count()
            >= 2
            && s.iter()
                .rev()
                .find(|e| e["type"] == "connection")
                .is_some_and(|e| e["state"] == "connected")
    };
    alice.until("alice back", 90, back);
    bob.until("bob back", 90, back);
    wait_texts(&mut alice, &conv, &["during the outage"]);
    bob.cmd(json!({"cmd": "send", "conv_id": conv, "text": "back again"}))
        .unwrap();
    wait_texts(&mut alice, &conv, &["during the outage", "back again"]);
    assert_eq!(alice.state(), "connected");
}

#[test]
fn a_new_device_pages_back_through_history() {
    let s = Server::start();
    let mut alice = App::new(&s, "alice", "a1", None);
    alice.connect();
    let conv = group(&mut alice, &[]);
    for i in 0..120 {
        alice
            .cmd(json!({"cmd": "send", "conv_id": conv, "text": format!("m{i}")}))
            .unwrap();
    }
    alice.until("all sent", 60, |s| {
        s.iter()
            .filter(|e| e["type"] == "operation" && e["kind"] == "send")
            .count()
            >= 120
    });
    // Another device of alice: a fresh cache.
    let mut other = App::new(&s, "alice", "a2", None);
    other.connect();
    let r = other
        .cmd(json!({"cmd": "messages", "conv_id": conv}))
        .unwrap();
    assert_eq!(
        r["messages"].as_array().unwrap().len(),
        30,
        "a new device gets the last 30"
    );
    assert_eq!(r["has_older"], true);
    loop {
        let r = other
            .cmd(json!({"cmd": "load_older", "conv_id": conv}))
            .unwrap();
        if r["has_older"] == false {
            assert_eq!(r["older_end"], "start");
            break;
        }
    }
    let texts: Vec<String> = other.texts(&conv).into_iter().map(|(t, _)| t).collect();
    let want: Vec<String> = (0..120).map(|i| format!("m{i}")).collect();
    assert_eq!(texts, want);
}

#[test]
fn a_deactivated_person_stops_retrying() {
    let s = Server::start();
    let key = s.cli(&["admin-key", "create", "--name", "t", "--role", "developer"])["key"]
        .as_str()
        .unwrap()
        .to_string();
    let mut alice = App::new(&s, "alice", "a1", None);
    alice.connect();
    let out = Command::new(bin())
        .args([
            "admin",
            "--url",
            &s.url(),
            "--key",
            &key,
            "deactivate",
            "alice",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // The server closes the connection with `logged_out`: no retry.
    alice.until("failed", 30, |s| {
        s.iter()
            .any(|e| e["type"] == "connection" && e["state"] == "failed")
    });
    let last = alice
        .seen
        .iter()
        .rev()
        .find(|e| e["type"] == "connection")
        .unwrap()
        .clone();
    assert_eq!(last["reason"], "logged_out", "{last}");
    // Connecting again is refused with 403: failed, not retried.
    alice.seen.clear();
    alice.cmd(json!({"cmd": "connect"})).unwrap();
    alice.until("failed again", 30, |s| {
        s.iter()
            .any(|e| e["type"] == "connection" && e["state"] == "failed")
    });
    let last = alice
        .seen
        .iter()
        .rev()
        .find(|e| e["type"] == "connection")
        .unwrap()
        .clone();
    assert_eq!(last["reason"], "unauthenticated", "{last}");
    std::thread::sleep(Duration::from_secs(2));
    assert_eq!(alice.state(), "failed");
}

#[test]
fn the_c_abi_from_c() {
    let s = Server::start();
    // `cargo test` builds the library as rlib only: build the shared one.
    let built = Command::new(env!("CARGO"))
        .args(["build", "-p", "agentim-client"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .status()
        .expect("cargo");
    assert!(built.success(), "cargo build -p agentim-client");
    let lib_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../target/debug");
    let out_dir = tempfile::tempdir().unwrap();
    let exe = out_dir.path().join("abi_test");
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/abi_test.c");
    let inc = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("include");
    let cc = Command::new("cc")
        .arg(&src)
        .arg("-I")
        .arg(&inc)
        .arg("-L")
        .arg(&lib_dir)
        .args(["-lagentim_client", "-lpthread", "-ldl", "-lm", "-o"])
        .arg(&exe)
        .output();
    let Ok(cc) = cc else {
        eprintln!("skipped: no C compiler");
        return;
    };
    assert!(
        cc.status.success(),
        "{}",
        String::from_utf8_lossy(&cc.stderr)
    );
    let run = Command::new(&exe)
        .args([&s.url(), &s.token("carol")])
        .env("LD_LIBRARY_PATH", &lib_dir)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        run.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(stdout.contains("connected"), "{stdout}");
    assert!(stdout.contains("sent"), "{stdout}");
    assert!(stdout.contains("dart connected"), "{stdout}");
}
