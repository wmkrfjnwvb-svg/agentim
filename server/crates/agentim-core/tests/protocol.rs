//! Protocol versioning (docs/02-architecture.md §2.5): negotiation in
//! `hello` and on connect, client minimum versions, feature declaration,
//! `unknown_op` vs `bad_request`, and tolerance of unknown fields. Hub-level
//! tests run against every backend; WebSocket tests use SQLite.

mod common;

use agentim_core::config::sqlite_url;
use agentim_core::hub::Hub;
use agentim_core::model::*;
use agentim_core::server::{AppState, router};
use common::*;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;

backend_tests!(
    old_clients_without_negotiation_keep_working,
    hello_negotiates_the_protocol,
    clients_below_the_minimum_version_are_refused,
    client_descriptions_are_bounded,
    repeated_hello_replaces_the_client_state,
    unknown_operations_are_told_apart_from_bad_requests,
    unknown_fields_are_ignored,
);

async fn open_with_min(db: &TestDb, min: &[(&str, &str)]) -> Hub {
    let mut cfg = single(&db.url);
    for (n, v) in min {
        cfg.clients
            .min_versions
            .insert(n.to_string(), v.to_string());
    }
    open(cfg).await
}

fn hello(protocol: Option<Value>, client: Option<Value>, features: Option<Value>) -> Value {
    let mut h = json!({"op": "hello", "rid": 1});
    if let Some(p) = protocol {
        h["protocol"] = p;
    }
    if let Some(c) = client {
        h["client"] = c;
    }
    if let Some(f) = features {
        h["features"] = f;
    }
    h
}

fn client(name: &str, version: &str) -> Value {
    json!({"name": name, "version": version, "platform": "web"})
}

fn code(r: &Value) -> &str {
    r["error"]["code"].as_str().unwrap_or("")
}

async fn old_clients_without_negotiation_keep_working(db: TestDb) {
    let hub = open_with_min(&db, &[("agentim-js", "1.2.0")]).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    let a = Conn::new(&hub, &alice);
    let b = Conn::new(&hub, &bob);
    b.group(&[&alice]).await;
    // A bare hello (protocol v1 clients before negotiation existed) gets the
    // current version and everything it used to get.
    let r = a.ok(json!({"op": "hello"})).await;
    assert_eq!(r["protocol"], 1);
    assert_eq!(r["me"]["id"], alice.id.as_str());
    assert!(
        r["users"]
            .as_array()
            .unwrap()
            .iter()
            .any(|u| u["id"] == bob.id.as_str())
    );
    assert_eq!(r["my_agents"][0]["id"], bot.id.as_str());
    assert_eq!(r["protocols"], json!([1]));
    assert_eq!(r["server"]["name"], "agentim");
    assert_eq!(r["server"]["product"], "AgentIM");
    assert!(r["server"].get("edition").is_none(), "one product");
    assert!(!r["server"]["version"].as_str().unwrap().is_empty());
    let features: Vec<&str> = r["features"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f.as_str().unwrap())
        .collect();
    for f in [
        "side_channel",
        "streams",
        "approval",
        "membership",
        "resync",
    ] {
        assert!(features.contains(&f), "{f} is declared");
    }
    assert!(r["limits"]["message_bytes"].as_u64().unwrap() > 0);
    // A minimum only applies to clients that name themselves.
    let st = a.s.client();
    assert_eq!((st.protocol, st.info.unwrap().name), (1, String::new()));
    // Agents shake hands the same way.
    let g = Conn::new(&hub, &bot);
    assert_eq!(g.ok(json!({"op": "hello"})).await["protocol"], 1);
}

async fn hello_negotiates_the_protocol(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let a = Conn::new(&hub, &alice);
    // The newest version both speak; future versions the server lacks are skipped.
    for list in [json!([1]), json!([3, 2, 1]), json!([1, 2])] {
        let r = a.ok(hello(Some(list.clone()), None, None)).await;
        assert_eq!(r["protocol"], 1, "{list}");
    }
    for list in [json!([2]), json!([]), json!([0])] {
        let r = a.call(hello(Some(list.clone()), None, None)).await;
        assert_eq!(code(&r), "unsupported_protocol", "{list}: {r}");
        assert!(r["error"]["message"].as_str().unwrap().contains("[1]"));
        assert_eq!(r["rid"], 1);
    }
    // Wrong types are malformed requests, not unknown operations.
    for bad in [json!("1"), json!([-1]), json!([1.5]), json!({"v": 1})] {
        let r = a.call(hello(Some(bad.clone()), None, None)).await;
        assert_eq!(code(&r), "bad_request", "{bad}: {r}");
    }
    // A refused hello does not record anything about the client.
    assert!(
        a.s.client().info.is_some(),
        "the earlier successful hello stays"
    );
    let fresh = Conn::new(&hub, &alice);
    fresh
        .call(hello(Some(json!([2])), Some(client("x", "1.0.0")), None))
        .await;
    assert!(fresh.s.client().info.is_none());
    // The connection keeps working after a refused negotiation.
    let c = fresh.group(&[&alice]).await;
    assert_eq!(fresh.send(&c, "still here").await["ok"], true);
}

async fn clients_below_the_minimum_version_are_refused(db: TestDb) {
    let hub = open_with_min(&db, &[("agentim-js", "1.2.0"), ("agentim-ios", "2")]).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let cases = [
        ("agentim-js", "1.2.0", ""),
        ("agentim-js", "1.2", ""),
        ("agentim-js", "1.10.0", ""),
        ("agentim-js", "2.0.0-beta.1", ""),
        ("agentim-js", "1.1.9", "client_too_old"),
        ("agentim-js", "0.9.0", "client_too_old"),
        ("agentim-js", "1.2.0-rc.1", ""),
        ("agentim-ios", "1.99.99", "client_too_old"),
        ("agentim-ios", "2.0.0", ""),
        // A client with a minimum must send a comparable version.
        ("agentim-js", "", "bad_request"),
        ("agentim-js", "latest", "bad_request"),
        // No minimum configured: any version string is accepted.
        ("my-bot", "anything", ""),
        ("AgentIM-JS", "0.0.1", ""),
    ];
    for (name, version, want) in cases {
        let a = Conn::new(&hub, &alice);
        let r = a.call(hello(None, Some(client(name, version)), None)).await;
        if want.is_empty() {
            assert_eq!(r["ok"], true, "{name} {version}: {r}");
            assert_eq!(a.s.client().info.unwrap().version, version);
        } else {
            assert_eq!(code(&r), want, "{name} {version}: {r}");
            assert!(a.s.client().info.is_none(), "refused: nothing recorded");
        }
        if want == "client_too_old" {
            let m = r["error"]["message"].as_str().unwrap();
            assert!(m.contains("upgrade"), "{m}");
        }
    }
}

async fn client_descriptions_are_bounded(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let a = Conn::new(&hub, &alice);
    let n64 = "n".repeat(64);
    let n65 = "n".repeat(65);
    let ok = |c: Value, f: Value| hello(Some(json!([1])), Some(c), Some(f));
    assert_eq!(a.call(ok(client(&n64, "1"), json!([]))).await["ok"], true);
    for c in [
        client(&n65, "1"),
        client("x", &n65),
        json!({"name": "x", "version": "1", "platform": n65}),
    ] {
        assert_eq!(
            code(&a.call(ok(c.clone(), json!([]))).await),
            "bad_request",
            "{c}"
        );
    }
    let hundred: Vec<String> = (0..100).map(|i| format!("x.example.f{i}")).collect();
    let r = a.call(ok(client("x", "1"), json!(hundred))).await;
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(a.s.client().features.len(), 100);
    let more: Vec<String> = (0..101).map(|i| format!("f{i}")).collect();
    for f in [json!(more), json!([""]), json!([n65]), json!([1])] {
        assert_eq!(
            code(&a.call(ok(client("x", "1"), f.clone())).await),
            "bad_request"
        );
    }
    // Missing parts of the description default to empty.
    let r = a.call(hello(None, Some(json!({})), None)).await;
    assert_eq!(r["ok"], true, "{r}");
}

async fn repeated_hello_replaces_the_client_state(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let a = Conn::new(&hub, &alice);
    let other = Conn::new(&hub, &alice);
    a.ok(hello(
        None,
        Some(client("agentim-js", "1.0.0")),
        Some(json!(["streams"])),
    ))
    .await;
    a.ok(hello(None, Some(client("agentim-js", "1.1.0")), None))
        .await;
    let st = a.s.client();
    assert_eq!(st.info.unwrap().version, "1.1.0");
    assert!(st.features.is_empty(), "the latest hello wins");
    // Each connection of the same user has its own state.
    assert!(other.s.client().info.is_none());
}

async fn unknown_operations_are_told_apart_from_bad_requests(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let mut a = Conn::new(&hub, &alice);
    let c = a.group(&[&alice]).await;
    a.drain();
    let before = seqs(&a.pull(&c, 0).await).len();
    let raw = |t: &'static str| async { a.s.handle_text(t).await };
    for (text, want, rid) in [
        (
            r#"{"op":"reactions_add","rid":7,"conv_id":"x"}"#,
            "unknown_op",
            Some(7),
        ),
        (r#"{"op":"","rid":8}"#, "unknown_op", Some(8)),
        (r#"{"op":"HELLO","rid":9}"#, "unknown_op", Some(9)),
        // Known operations with missing or wrong fields.
        (r#"{"op":"send","rid":10}"#, "bad_request", Some(10)),
        (
            r#"{"op":"pull","conv_id":5,"rid":11}"#,
            "bad_request",
            Some(11),
        ),
        // Not a request at all.
        (r#"{"rid":12}"#, "bad_request", Some(12)),
        (r#"{"op":3,"rid":13}"#, "bad_request", Some(13)),
        ("not json", "bad_request", None),
        ("[]", "bad_request", None),
    ] {
        let r = raw(text).await;
        assert_eq!((code(&r), r["rid"].as_i64()), (want, rid), "{text}: {r}");
        assert_eq!(r["ok"], false);
    }
    // Refusals leave no trace and the connection keeps working.
    let p = a.pull(&c, 0).await;
    assert_eq!(seqs(&p).len(), before, "nothing was written: {p}");
    assert!(a.drain().is_empty(), "nothing was pushed");
    assert_eq!(a.send(&c, "after").await["ok"], true);
}

async fn unknown_fields_are_ignored(db: TestDb) {
    // Forward compatibility inside a major version: a newer client may send
    // optional fields this server does not know yet.
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let a = Conn::new(&hub, &alice);
    let c = a.group(&[&alice]).await;
    let r = a
        .ok(json!({"op": "send", "conv_id": c, "client_msg_id": "m1", "text": "hi", "x.example.color": "red", "silent": true}))
        .await;
    let p = a.pull(&c, 0).await;
    let m = msg_by_id(&p, r["msg_id"].as_str().unwrap()).unwrap();
    assert_eq!(m["content"], "hi");
    assert!(m.get("x.example.color").is_none(), "not stored");
    a.ok(json!({"op": "hello", "future": {"a": 1}})).await;
}

// ---- WebSocket ----------------------------------------------------------

struct Server {
    addr: String,
    hub: Hub,
    _dir: tempfile::TempDir,
}

async fn start(min: &[(&str, &str)]) -> Server {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = single(&sqlite_url(&dir.path().join("db").to_string_lossy()));
    for (n, v) in min {
        cfg.clients
            .min_versions
            .insert(n.to_string(), v.to_string());
    }
    let hub = open(cfg).await;
    let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = tcp.local_addr().unwrap().to_string();
    let state = AppState {
        hub: hub.clone(),
        demo_login: false,
        metrics_token: None,
    };
    tokio::spawn(async move { axum::serve(tcp, router(state)).await.unwrap() });
    Server {
        addr,
        hub,
        _dir: dir,
    }
}

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn next_json(ws: &mut Ws) -> Value {
    loop {
        match ws.next().await.unwrap().unwrap() {
            Message::Text(t) => return serde_json::from_str(t.as_str()).unwrap(),
            Message::Ping(_) | Message::Pong(_) => {}
            other => panic!("expected a text frame, got {other:?}"),
        }
    }
}

/// Status and body of a refused upgrade.
async fn refused(url: String) -> (u16, Value) {
    match tokio_tungstenite::connect_async(url).await {
        Err(tokio_tungstenite::tungstenite::Error::Http(r)) => {
            let body = r.body().clone().unwrap_or_default();
            (
                r.status().as_u16(),
                serde_json::from_slice(&body).unwrap_or(Value::Null),
            )
        }
        Err(e) => panic!("unexpected error {e}"),
        Ok(_) => panic!("upgrade should be refused"),
    }
}

#[tokio::test]
async fn welcome_declares_the_protocol_and_features() {
    let srv = start(&[]).await;
    let (_, token) = srv
        .hub
        .create_user("alice", UserKind::Human, None, None)
        .await
        .unwrap();
    for (q, want) in [("", 1), ("&v=1", 1), ("&v=2,1", 1), ("&v=%201%20,3", 1)] {
        let (mut ws, _) =
            tokio_tungstenite::connect_async(format!("ws://{}/ws?token={token}{q}", srv.addr))
                .await
                .unwrap();
        let w = next_json(&mut ws).await;
        assert_eq!(w["type"], "welcome", "{q}");
        assert_eq!(w["protocol"], want, "{q}");
        assert_eq!(w["me"]["name"], "alice");
        assert_eq!(w["protocols"], json!([1]));
        assert!(
            w["features"]
                .as_array()
                .unwrap()
                .contains(&json!("streams"))
        );
        assert_eq!(w["limits"]["frame_bytes"], 1 << 20);
    }
}

#[tokio::test]
async fn incompatible_versions_are_refused_before_the_upgrade() {
    let srv = start(&[]).await;
    let (_, token) = srv
        .hub
        .create_user("alice", UserKind::Human, None, None)
        .await
        .unwrap();
    for (q, want) in [
        ("v=2", "unsupported_protocol"),
        ("v=0", "unsupported_protocol"),
        ("v=", "bad_request"),
        ("v=abc", "bad_request"),
        ("v=1,x", "bad_request"),
        ("v=-1", "bad_request"),
        ("v=1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17", "bad_request"),
    ] {
        let (status, body) = refused(format!("ws://{}/ws?token={token}&{q}", srv.addr)).await;
        assert_eq!(status, 400, "{q}");
        assert_eq!(body["error"]["code"], want, "{q}: {body}");
        assert_eq!(
            body["protocols"],
            json!([1]),
            "tells the client what it can use"
        );
    }
    // Authentication is still checked first.
    let (status, _) = refused(format!("ws://{}/ws?token=wrong&v=2", srv.addr)).await;
    assert_eq!(status, 401);
    // Sixteen versions are allowed.
    let list: Vec<String> = (1..=16).map(|i| i.to_string()).collect();
    let (mut ws, _) = tokio_tungstenite::connect_async(format!(
        "ws://{}/ws?token={token}&v={}",
        srv.addr,
        list.join(",")
    ))
    .await
    .unwrap();
    assert_eq!(next_json(&mut ws).await["protocol"], 1);
}

#[tokio::test]
async fn a_too_old_client_gets_the_reason_and_is_disconnected() {
    let srv = start(&[("agentim-js", "1.2.0")]).await;
    let (_, token) = srv
        .hub
        .create_user("alice", UserKind::Human, None, None)
        .await
        .unwrap();
    let url = format!("ws://{}/ws?token={token}", srv.addr);
    let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    next_json(&mut ws).await;
    ws.send(Message::Text(
        json!({"op": "hello", "rid": 1, "client": client("agentim-js", "1.0.0")})
            .to_string()
            .into(),
    ))
    .await
    .unwrap();
    let r = next_json(&mut ws).await;
    assert_eq!((code(&r), r["rid"].as_i64()), ("client_too_old", Some(1)));
    let close = tokio::time::timeout(std::time::Duration::from_secs(5), ws.next())
        .await
        .expect("closed promptly");
    match close {
        Some(Ok(Message::Close(Some(f)))) => {
            assert_eq!(u16::from(f.code), 1008);
            assert_eq!(f.reason.as_str(), "client_too_old");
        }
        other => panic!("expected a close frame, got {other:?}"),
    }
    // An up-to-date client on the same account is unaffected.
    let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    next_json(&mut ws).await;
    ws.send(Message::Text(
        json!({"op": "hello", "rid": 2, "client": client("agentim-js", "1.2.0")})
            .to_string()
            .into(),
    ))
    .await
    .unwrap();
    assert_eq!(next_json(&mut ws).await["ok"], true);
    // Other refusals keep the connection open.
    ws.send(Message::Text(r#"{"op":"nope","rid":3}"#.into()))
        .await
        .unwrap();
    assert_eq!(code(&next_json(&mut ws).await), "unknown_op");
    ws.send(Message::Text(
        r#"{"op":"hello","rid":4,"protocol":[9]}"#.into(),
    ))
    .await
    .unwrap();
    assert_eq!(code(&next_json(&mut ws).await), "unsupported_protocol");
    ws.send(Message::Text(r#"{"op":"list_convs","rid":5}"#.into()))
        .await
        .unwrap();
    assert_eq!(next_json(&mut ws).await["ok"], true);
}

#[tokio::test]
async fn handshakes_are_counted_by_protocol_and_client() {
    let srv = start(&[]).await;
    let (alice, _) = srv
        .hub
        .create_user("alice", UserKind::Human, None, None)
        .await
        .unwrap();
    let a = Conn::new(&srv.hub, &alice);
    a.ok(hello(None, Some(client("agentim-rust", "0.1.0")), None))
        .await;
    a.ok(hello(None, Some(client("Evil {label}", "1")), None))
        .await;
    let text = agentim_core::metrics::render();
    let line = |c: &str| {
        text.lines().any(|l| {
            l.starts_with("agentim_clients_total") && l.contains(&format!("client=\"{c}\""))
        })
    };
    assert!(line("agentim-rust"), "{text}");
    assert!(line("other"), "untrusted names are folded");
    assert!(!text.contains("Evil"), "labels never carry raw input");
}
