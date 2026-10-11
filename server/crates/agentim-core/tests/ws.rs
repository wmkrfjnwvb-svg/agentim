//! End-to-end over a real socket: HTTP login, WebSocket auth, request/response
//! and pushes, plus a scripted demo agent answering with a stream.

use std::time::Duration;

use agentim_core::config::{Config, sqlite_url};
use agentim_core::hub::Hub;
use agentim_core::logic::HubConfig;
use agentim_core::server::{AppState, router};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_tungstenite::tungstenite::Message;

async fn start(demo_agents: bool) -> (String, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = Config::single_sqlite("unused");
    cfg.storage.databases[0].url = sqlite_url(&dir.path().join("db").to_string_lossy());
    let hub = Hub::open(cfg, HubConfig::default()).await.unwrap();
    agentim_core::demo::seed(&hub).await.unwrap();
    hub.spawn_background();
    if demo_agents {
        agentim_core::demo::spawn_agents(&hub);
    }
    let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = tcp.local_addr().unwrap().to_string();
    let listener = axum::serve::ListenerExt::tap_io(tcp, |t| {
        let _ = t.set_nodelay(true);
    });
    tokio::spawn(async move {
        axum::serve(
            listener,
            router(AppState {
                hub,
                demo_login: true,
                metrics_token: None,
            }),
        )
        .await
        .unwrap()
    });
    (addr, dir)
}

async fn http(addr: &str, method: &str, path: &str, body: &str) -> (u16, String) {
    let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
    let req = format!(
        "{method} {path} HTTP/1.1\r\nHost: x\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    s.write_all(req.as_bytes()).await.unwrap();
    let mut buf = Vec::new();
    s.read_to_end(&mut buf).await.unwrap();
    let t = String::from_utf8_lossy(&buf).to_string();
    let code = t[9..12].parse().unwrap();
    (code, t.split("\r\n\r\n").nth(1).unwrap_or("").to_string())
}

async fn login(addr: &str, name: &str) -> String {
    let (code, body) = http(
        addr,
        "POST",
        "/api/login",
        &json!({"name": name}).to_string(),
    )
    .await;
    assert_eq!(code, 200, "{body}");
    serde_json::from_str::<Value>(&body).unwrap()["token"]
        .as_str()
        .unwrap()
        .to_string()
}

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn recv_until(ws: &mut Ws, pred: impl Fn(&Value) -> bool) -> Value {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if let Some(Ok(Message::Text(t))) = ws.next().await {
                let v: Value = serde_json::from_str(t.as_str()).unwrap();
                if pred(&v) {
                    return v;
                }
            }
        }
    })
    .await
    .expect("timed out waiting for frame")
}

async fn req(ws: &mut Ws, rid: u64, v: Value) -> Value {
    let mut v = v;
    v["rid"] = json!(rid);
    ws.send(Message::Text(v.to_string().into())).await.unwrap();
    recv_until(ws, |f| f["type"] == "res" && f["rid"] == rid).await
}

/// Syncs and acknowledges until nothing more is owed (docs/08): after
/// that, live events arrive as pushes.
async fn sync_up(ws: &mut Ws, mut rid: u64) {
    loop {
        let r = req(ws, rid, json!({"op": "sync", "limit": 5000})).await;
        assert_eq!(r["ok"], true, "{r}");
        let acks: Vec<Value> = r["convs"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|c| c.get("upto").is_some())
            .map(|c| json!({"conv_id": c["conv_id"], "seq": c["upto"], "side_seq": c["side_upto"]}))
            .collect();
        if !acks.is_empty() {
            req(ws, rid + 1, json!({"op": "ack", "acks": acks})).await;
        }
        rid += 2;
        if r["more"] != true {
            return;
        }
    }
}

#[tokio::test]
async fn websocket_auth_request_response_and_push() {
    let (addr, _dir) = start(false).await;
    // Bad token is refused; agents cannot use demo login.
    match tokio_tungstenite::connect_async_with_config(
        format!("ws://{addr}/ws?token=nope"),
        None,
        true,
    )
    .await
    {
        Err(tokio_tungstenite::tungstenite::Error::Http(resp)) => assert_eq!(resp.status(), 401),
        other => panic!("expected 401, got {:?}", other.map(|_| ())),
    }
    let (code, _) = http(&addr, "POST", "/api/login", r#"{"name":"Aria"}"#).await;
    assert_eq!(code, 404);

    let (mut a, _) = tokio_tungstenite::connect_async_with_config(
        format!("ws://{addr}/ws?token={}", login(&addr, "alice").await),
        None,
        true,
    )
    .await
    .unwrap();
    let (mut b, _) = tokio_tungstenite::connect_async_with_config(
        format!("ws://{addr}/ws?token={}", login(&addr, "bob").await),
        None,
        true,
    )
    .await
    .unwrap();
    let w = recv_until(&mut a, |f| f["type"] == "welcome").await;
    assert_eq!(w["me"]["name"], "alice");
    // Before its first sync a device is only told to sync; after it,
    // live events are pushed.
    sync_up(&mut b, 100).await;
    let convs = req(&mut a, 1, json!({"op": "list_convs"})).await;
    let group = convs["convs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["title"] == "产品讨论群")
        .unwrap()["conv_id"]
        .as_str()
        .unwrap()
        .to_string();
    let r = req(
        &mut a,
        2,
        json!({"op": "send", "conv_id": group, "client_msg_id": "w1", "text": "over the wire"}),
    )
    .await;
    assert_eq!(r["ok"], true);
    let pushed = recv_until(&mut b, |f| {
        f["type"] == "event" && f["event"]["type"] == "msg.created"
    })
    .await;
    assert_eq!(pushed["event"]["msg"]["content"], "over the wire");
    assert_eq!(pushed["event"]["seq"], r["seq"]);
    // Malformed frames get an error response, not a dropped connection.
    a.send(Message::Text(r#"{"op":"nope","rid":9}"#.into()))
        .await
        .unwrap();
    let e = recv_until(&mut a, |f| f["type"] == "res" && f["rid"] == 9).await;
    assert_eq!(e["error"]["code"], "unknown_op");
    let r = req(&mut a, 10, json!({"op": "hello"})).await;
    assert_eq!(r["ok"], true);
}

#[tokio::test]
async fn demo_agent_streams_a_reply_with_coalesced_deltas() {
    let (addr, _dir) = start(true).await;
    let (mut a, _) = tokio_tungstenite::connect_async_with_config(
        format!("ws://{addr}/ws?token={}", login(&addr, "alice").await),
        None,
        true,
    )
    .await
    .unwrap();
    let convs = req(&mut a, 1, json!({"op": "list_convs"})).await;
    let dm = convs["convs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["title"] == "Alice · Aria")
        .unwrap()["conv_id"]
        .as_str()
        .unwrap()
        .to_string();
    sync_up(&mut a, 100).await;
    req(&mut a, 2, json!({"op": "focus", "conv_id": dm})).await;
    req(
        &mut a,
        3,
        json!({"op": "send", "conv_id": dm, "client_msg_id": "q1", "text": "hello"}),
    )
    .await;
    let start = recv_until(&mut a, |f| {
        f["type"] == "event" && f["event"]["msg"]["state"] == "streaming"
    })
    .await;
    let id = start["event"]["msg"]["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    let mut text = String::new();
    let mut deltas = 0;
    let done = loop {
        let f = recv_until(&mut a, |f| {
            f["type"] == "delta" || (f["type"] == "event" && f["event"]["type"] == "msg.updated")
        })
        .await;
        if f["type"] == "delta" {
            assert_eq!(f["msg_id"], id.as_str());
            text.push_str(f["text"].as_str().unwrap());
            deltas += 1;
        } else {
            break f;
        }
    };
    assert_eq!(done["event"]["msg"]["state"], "complete");
    assert_eq!(done["event"]["msg"]["sender_type"], "agent");
    assert_eq!(
        done["event"]["msg"]["content"].as_str().unwrap(),
        text,
        "deltas must add up to the final content"
    );
    let chunks = text.chars().count().div_ceil(2);
    assert!(
        deltas > 1 && deltas < chunks,
        "expected coalescing: {deltas} deltas for {chunks} chunks"
    );
}
