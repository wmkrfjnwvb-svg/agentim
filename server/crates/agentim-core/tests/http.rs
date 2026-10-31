//! HTTP and WebSocket endpoints outside the demo: what each one allows,
//! refuses and reports (docs/05-operations.md §5.5).

mod common;

use agentim_core::config::{Config, sqlite_url};
use agentim_core::hub::Hub;
use agentim_core::logic::HubConfig;
use agentim_core::model::UserKind;
use agentim_core::server::{AppState, router};
use common::ws_token;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_tungstenite::tungstenite::Message;

struct Server {
    addr: String,
    hub: Hub,
    _dir: tempfile::TempDir,
}

async fn start(demo: bool, metrics_token: Option<&str>) -> Server {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = Config::single_sqlite("unused");
    cfg.storage.databases[0].url = sqlite_url(&dir.path().join("db").to_string_lossy());
    let hub = Hub::open(cfg, HubConfig::default()).await.unwrap();
    if demo {
        // As `--demo` does: the demo signs its own client tokens.
        hub.create_signing_key("demo").await.unwrap();
    }
    let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = tcp.local_addr().unwrap().to_string();
    let state = AppState {
        hub: hub.clone(),
        demo_login: demo,
        metrics_token: metrics_token.map(String::from),
    };
    tokio::spawn(async move {
        axum::serve(
            tcp,
            router(state).into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap()
    });
    Server {
        addr,
        hub,
        _dir: dir,
    }
}

async fn http(addr: &str, method: &str, path: &str, headers: &str, body: &str) -> (u16, String) {
    let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
    let req = format!(
        "{method} {path} HTTP/1.1\r\nHost: x\r\nContent-Type: application/json\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    s.write_all(req.as_bytes()).await.unwrap();
    let mut buf = Vec::new();
    s.read_to_end(&mut buf).await.unwrap();
    let t = String::from_utf8_lossy(&buf).to_string();
    (
        t[9..12].parse().unwrap(),
        t.split("\r\n\r\n").nth(1).unwrap_or("").to_string(),
    )
}

#[tokio::test]
async fn outside_the_demo_login_user_creation_and_stats_are_closed() {
    let srv = start(false, None).await;
    let alice = srv
        .hub
        .create_user("alice", UserKind::Human, None, None)
        .await
        .unwrap()
        .0;
    let token = common::client_token(&srv.hub, &alice).await;
    let (c, _) = http(&srv.addr, "POST", "/api/login", "", r#"{"name":"alice"}"#).await;
    assert_eq!(c, 403, "password-less login only in the demo");
    let (c, _) = http(
        &srv.addr,
        "POST",
        "/api/dev/users",
        "",
        r#"{"name":"mallory"}"#,
    )
    .await;
    assert_eq!(c, 403, "anyone creating users only in the demo");
    assert!(srv.hub.token_of("mallory").await.is_none());
    // Built-in accounts and deployment stats are gone (docs/13 problems 2, 5).
    let auth = format!("Authorization: Bearer {token}\r\n");
    for (m, p, b) in [
        ("GET", "/api/stats", ""),
        (
            "POST",
            "/api/auth/login",
            r#"{"name":"alice","password":"x"}"#,
        ),
        (
            "POST",
            "/api/auth/register",
            r#"{"name":"x","password":"y"}"#,
        ),
    ] {
        let (c, _) = http(&srv.addr, m, p, &auth, b).await;
        assert_eq!(c, 404, "{p}");
    }
    let _ = alice;
}

#[tokio::test]
async fn demo_endpoints_work_in_the_demo() {
    let srv = start(true, None).await;
    // A page on another site cannot create users through a visitor's
    // browser (docs/13 problem 6).
    let (c, _) = http(
        &srv.addr,
        "POST",
        "/api/dev/users",
        "Origin: https://evil.example\r\n",
        r#"{"name":"dana"}"#,
    )
    .await;
    assert_eq!(c, 403);
    assert!(srv.hub.user_by_name("dana").is_none());
    let (c, body) = http(
        &srv.addr,
        "POST",
        "/api/dev/users",
        "",
        r#"{"name":"dana"}"#,
    )
    .await;
    assert_eq!(c, 200, "{body}");
    let (c, _) = http(
        &srv.addr,
        "POST",
        "/api/dev/users",
        "",
        r#"{"name":"dana"}"#,
    )
    .await;
    assert_eq!(c, 409, "duplicate names are refused");
    let (c, body) = http(&srv.addr, "POST", "/api/login", "", r#"{"name":"dana"}"#).await;
    assert_eq!(c, 200);
    let token = serde_json::from_str::<Value>(&body).unwrap()["token"]
        .as_str()
        .unwrap()
        .to_string();
    let (c, _) = http(&srv.addr, "POST", "/api/login", "", r#"{"name":"nobody"}"#).await;
    assert_eq!(c, 404);
    let (c, _) = http(&srv.addr, "POST", "/api/login", "", "not json").await;
    assert!((400..500).contains(&c), "malformed body: {c}");
    let _ = token;
}

#[tokio::test]
async fn metrics_require_the_token_when_configured() {
    let srv = start(false, Some("s3cret")).await;
    assert_eq!(http(&srv.addr, "GET", "/metrics", "", "").await.0, 401);
    assert_eq!(
        http(
            &srv.addr,
            "GET",
            "/metrics",
            "Authorization: Bearer wrong\r\n",
            ""
        )
        .await
        .0,
        401
    );
    let (c, body) = http(
        &srv.addr,
        "GET",
        "/metrics",
        "Authorization: Bearer s3cret\r\n",
        "",
    )
    .await;
    assert_eq!(c, 200);
    assert!(body.contains("agentim_"), "{body}");
    let open = start(false, None).await;
    assert_eq!(
        http(&open.addr, "GET", "/metrics", "", "").await.0,
        200,
        "open when no token is set"
    );
}

/// A refusal names its rule (docs/15) to open API callers and is counted
/// by rule in `/metrics`; a request that is allowed is not counted.
#[tokio::test]
async fn refusals_name_their_rule_and_are_counted() {
    let srv = start(false, None).await;
    let hub = &srv.hub;
    let alice = hub
        .create_user("alice", UserKind::Human, None, None)
        .await
        .unwrap()
        .0;
    let bob = hub
        .create_user("bob", UserKind::Human, None, None)
        .await
        .unwrap()
        .0;
    hub.create_user("eve", UserKind::Human, None, None)
        .await
        .unwrap();
    let (s, _) = hub.connect(alice.clone());
    let r: Value = serde_json::from_value(
        s.handle_text(
            &json!({"op": "create_conv", "kind": "group", "title": "g", "members": [bob.id]})
                .to_string(),
        )
        .await,
    )
    .unwrap();
    let conv = r["conv_id"].as_str().unwrap().to_string();
    let (key, _) = hub
        .create_api_key("t", &["messages:write".to_string()], None, None)
        .await
        .unwrap();
    let auth = format!("Authorization: Bearer {key}\r\n");
    let path = format!("/v1/conversations/{conv}/messages");
    let (c, body) = http(
        &srv.addr,
        "POST",
        &path,
        &auth,
        &json!({"as": "eve", "text": "hi"}).to_string(),
    )
    .await;
    assert_eq!(c, 403, "{body}");
    let v: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        (v["code"].as_str(), v["rule"].as_str()),
        (Some("forbidden"), Some("member.required")),
        "{v}"
    );
    let (c, body) = http(
        &srv.addr,
        "POST",
        &path,
        &auth,
        &json!({"as": "alice", "text": ""}).to_string(),
    )
    .await;
    assert_eq!(c, 400, "{body}");
    assert!(body.contains("\"rule\":\"msg.text_size\""), "{body}");
    // An error that is not a business rule has none.
    let (c, body) = http(
        &srv.addr,
        "POST",
        "/v1/conversations/c_x/messages",
        "Authorization: Bearer wrong\r\n",
        "{}",
    )
    .await;
    assert_eq!(c, 401);
    assert!(!body.contains("\"rule\""), "{body}");
    let (c, ok) = http(
        &srv.addr,
        "POST",
        &path,
        &auth,
        &json!({"as": "alice", "text": "fine"}).to_string(),
    )
    .await;
    assert_eq!(c, 201, "{ok}");
    let (_, m) = http(&srv.addr, "GET", "/metrics", "", "").await;
    let line = m
        .lines()
        .find(|l| l.starts_with("agentim_refusals_total") && l.contains("rule=\"member.required\""))
        .unwrap_or_else(|| panic!("no refusal counted: {m}"));
    assert!(
        line.contains("code=\"forbidden\"") && line.contains("op=\"send\""),
        "{line}"
    );
    assert!(
        m.lines().any(
            |l| l.starts_with("agentim_refusals_total") && l.contains("rule=\"msg.text_size\"")
        )
    );
}

#[tokio::test]
async fn readiness_reports_a_storage_failure() {
    let srv = start(false, None).await;
    assert_eq!(http(&srv.addr, "GET", "/healthz", "", "").await.0, 200);
    let (c, body) = http(&srv.addr, "GET", "/readyz", "", "").await;
    assert_eq!(c, 200, "{body}");
    srv.hub.inject_commit_failures("main", 1);
    let _ = srv.hub.create_user("x", UserKind::Human, None, None).await;
    let (c, body) = http(&srv.addr, "GET", "/readyz", "", "").await;
    assert_eq!(c, 503, "{body}");
    assert!(body.contains("injected commit failure"));
    assert_eq!(
        http(&srv.addr, "GET", "/healthz", "", "").await.0,
        200,
        "alive, but not ready"
    );
}

#[tokio::test]
async fn websocket_rejects_bad_tokens_oversized_frames_and_bad_requests() {
    let srv = start(false, None).await;
    let alice_ = srv
        .hub
        .create_user("alice", UserKind::Human, None, None)
        .await
        .unwrap()
        .0;
    let token = ws_token(&srv.hub, &alice_).await;
    for url in [
        format!("ws://{}/ws?token=wrong", srv.addr),
        format!("ws://{}/ws", srv.addr),
    ] {
        assert!(
            tokio_tungstenite::connect_async(url).await.is_err(),
            "refused"
        );
    }
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{}/ws?{token}", srv.addr))
        .await
        .unwrap();
    let welcome = ws.next().await.unwrap().unwrap();
    assert!(welcome.to_text().unwrap().contains("welcome"));
    ws.send(Message::Text("{\"op\":\"nope\",\"rid\":3}".into()))
        .await
        .unwrap();
    let r: Value =
        serde_json::from_str(ws.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
    assert_eq!(
        (r["ok"].as_bool(), r["rid"].as_i64()),
        (Some(false), Some(3))
    );
    // Larger than the 1 MiB frame limit: the connection is closed.
    let huge = json!({"op": "hello", "pad": "x".repeat(2 << 20)}).to_string();
    let _ = ws.send(Message::Text(huge.into())).await;
    let closed = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            match ws.next().await {
                None | Some(Err(_)) | Some(Ok(Message::Close(_))) => return true,
                Some(Ok(_)) => {}
            }
        }
    })
    .await
    .unwrap_or(false);
    assert!(closed, "an oversized frame closes the connection");
    // The server is unaffected.
    let (mut ws2, _) = tokio_tungstenite::connect_async(format!("ws://{}/ws?{token}", srv.addr))
        .await
        .unwrap();
    assert!(
        ws2.next()
            .await
            .unwrap()
            .unwrap()
            .to_text()
            .unwrap()
            .contains("welcome")
    );
}
