//! Request tracing (docs/14 §4.1, D138–D139): every request runs in a W3C
//! trace that reaches the writer and the message row; end users' traces are
//! linked, not continued; trusted callers (agents, the developer's backend)
//! are continued; invalid headers start a new trace.

mod common;

use std::net::SocketAddr;

use agentim_core::hub::Hub;
use agentim_core::model::*;
use agentim_core::server::{AppState, router};
use common::*;
use serde_json::json;

backend_tests!(
    people_get_a_new_trace_that_links_theirs,
    agents_and_backends_are_continued,
    invalid_or_missing_headers_start_a_new_trace,
    a_retry_keeps_the_first_trace,
);

const TRACE: &str = "4bf92f3577b34da6a3ce929d0e0e4736";
const PARENT: &str = "00f067aa0ba902b7";

fn header(sampled: bool) -> String {
    format!("00-{TRACE}-{PARENT}-{}", if sampled { "01" } else { "00" })
}

fn is_trace_id(s: &str) -> bool {
    s.len() == 32 && s.bytes().all(|c| c.is_ascii_hexdigit()) && s != "0".repeat(32)
}

async fn sent_trace(hub: &Hub, r: &serde_json::Value) -> String {
    assert_eq!(r["ok"], true, "{r}");
    let id = r["msg_id"].as_str().unwrap();
    hub.message_trace_id(id).await.unwrap().expect("stored")
}

async fn people_get_a_new_trace_that_links_theirs(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let a = Conn::new(&hub, &alice);
    let conv = a.group(&[&bob]).await;
    // Sampled or not, a person's trace is not continued (D139).
    for sampled in [true, false] {
        let r = a
            .call(json!({"op": "send", "conv_id": conv, "text": "hi", "client_msg_id": format!("c{sampled}"), "trace": header(sampled)}))
            .await;
        let t = sent_trace(&hub, &r).await;
        assert!(is_trace_id(&t), "{t}");
        assert_ne!(t, TRACE, "an end user's trace is linked, not continued");
    }
    // Every request gets its own.
    let x = sent_trace(&hub, &a.send(&conv, "one").await).await;
    let y = sent_trace(&hub, &a.send(&conv, "two").await).await;
    assert_ne!(x, y);
    // The trace never reaches the other members.
    let mut b = Conn::new(&hub, &bob);
    a.send(&conv, "three").await;
    let pulled = b
        .call(json!({"op": "pull", "conv_id": conv, "after_seq": 0}))
        .await;
    assert!(!pulled.to_string().contains(TRACE));
    assert!(!pulled.to_string().contains(&x));
    for f in b.drain() {
        assert!(f.get("trace").is_none(), "{f}");
    }
}

async fn agents_and_backends_are_continued(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    let a = Conn::new(&hub, &alice);
    let conv = a.group(&[&bot]).await;
    // An agent's trace is continued.
    let g = Conn::new(&hub, &bot);
    let r = g
        .call(json!({"op": "send", "conv_id": conv, "text": "on it", "client_msg_id": "g1", "trace": header(false)}))
        .await;
    assert_eq!(sent_trace(&hub, &r).await, TRACE);

    // The developer's backend over HTTP: the traceparent header is continued.
    let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", tcp.local_addr().unwrap());
    let app = router(AppState {
        hub: hub.clone(),
        demo_login: false,
        metrics_token: None,
    });
    tokio::spawn(async move {
        axum::serve(tcp, app.into_make_service_with_connect_info::<SocketAddr>())
            .await
            .unwrap()
    });
    let (key, _) = hub
        .create_api_key("t", &["messages:write".to_string()], None, None)
        .await
        .unwrap();
    let res = reqwest::Client::new()
        .post(format!("{base}/v1/conversations/{conv}/messages"))
        .bearer_auth(&key)
        .header("traceparent", header(true))
        .header("content-type", "application/json")
        .body(json!({"as": "alice", "text": "from the backend", "client_msg_id": "b1"}).to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 201);
    let v: serde_json::Value = serde_json::from_str(&res.text().await.unwrap()).unwrap();
    let id = v["msg_id"].as_str().unwrap();
    assert_eq!(
        hub.message_trace_id(id).await.unwrap().as_deref(),
        Some(TRACE)
    );
    // Without the header: a new one.
    let res = reqwest::Client::new()
        .post(format!("{base}/v1/conversations/{conv}/messages"))
        .bearer_auth(&key)
        .header("content-type", "application/json")
        .body(json!({"as": "alice", "text": "again", "client_msg_id": "b2"}).to_string())
        .send()
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&res.text().await.unwrap()).unwrap();
    let t = hub
        .message_trace_id(v["msg_id"].as_str().unwrap())
        .await
        .unwrap()
        .unwrap();
    assert!(is_trace_id(&t) && t != TRACE);
    // An unknown message.
    assert_eq!(
        hub.message_trace_id("m_nope").await.unwrap_err().code,
        "not_found"
    );
}

async fn invalid_or_missing_headers_start_a_new_trace(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    let a = Conn::new(&hub, &alice);
    let g = Conn::new(&hub, &bot);
    for (i, bad) in [
        json!("garbage"),
        json!(format!("ff-{TRACE}-{PARENT}-01")),
        json!(format!("00-{}-{PARENT}-01", "0".repeat(32))),
        json!(format!("00-{}-{PARENT}-01", TRACE.to_uppercase())),
        json!(""),
    ]
    .into_iter()
    .enumerate()
    {
        // A conversation each: the agent's per-conversation rate limit.
        let conv = a.group(&[&bot]).await;
        let r = g
            .call(json!({"op": "send", "conv_id": conv, "text": format!("x{i}"), "client_msg_id": format!("bad{i}"), "trace": bad}))
            .await;
        let t = sent_trace(&hub, &r).await;
        assert!(is_trace_id(&t) && t != TRACE, "{bad}: {t}");
    }
    // Not a string: the request itself is malformed.
    let conv = a.group(&[&bot]).await;
    let r = g
        .call(json!({"op": "send", "conv_id": conv, "text": "x", "client_msg_id": "n", "trace": 7}))
        .await;
    assert_eq!(r["error"]["code"], "bad_request", "{r}");
}

async fn a_retry_keeps_the_first_trace(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    let a = Conn::new(&hub, &alice);
    let conv = a.group(&[&bot]).await;
    let g = Conn::new(&hub, &bot);
    let first = g
        .call(json!({"op": "send", "conv_id": conv, "text": "x", "client_msg_id": "same", "trace": header(false)}))
        .await;
    // The retry has another trace; the stored message keeps the first.
    let other = format!("00-{}-{PARENT}-00", "a".repeat(32));
    let again = g
        .call(json!({"op": "send", "conv_id": conv, "text": "x", "client_msg_id": "same", "trace": other}))
        .await;
    assert_eq!(again["msg_id"], first["msg_id"], "{again}");
    assert_eq!(sent_trace(&hub, &first).await, TRACE);
}
