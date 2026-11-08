//! Span export over OTLP (docs/en/design/tracing.md, `[tracing] otlp_endpoint`): with
//! `sample_ratio = 0`, a dyed request and a trace a trusted caller marked
//! sampled reach the collector under their own trace ids; other requests
//! do not.

mod common;

use std::sync::{Arc, Mutex};

use agentim_core::config::TracingConfig;
use agentim_core::model::*;
use axum::Router;
use axum::body::Bytes;
use axum::routing::post;
use common::*;
use serde_json::json;

/// What the collector received, as raw protobuf bodies.
type Bodies = Arc<Mutex<Vec<Vec<u8>>>>;

async fn collector() -> (String, Bodies) {
    let got: Bodies = Arc::default();
    let g = got.clone();
    let app = Router::new().route(
        "/v1/traces",
        post(move |body: Bytes| {
            let g = g.clone();
            async move {
                g.lock().unwrap().push(body.to_vec());
                axum::http::StatusCode::OK
            }
        }),
    );
    let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = tcp.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(tcp, app).await.unwrap() });
    (format!("http://{addr}/v1/traces"), got)
}

fn exported(got: &Bodies, trace_id: &str) -> bool {
    let id = hex::decode(trace_id).unwrap();
    got.lock()
        .unwrap()
        .iter()
        .any(|b| b.windows(16).any(|w| w == id.as_slice()))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sampled_and_dyed_traces_reach_the_collector() {
    let (endpoint, got) = collector().await;
    let cfg = TracingConfig {
        otlp_endpoint: Some(endpoint),
        sample_ratio: 0.0,
        ..Default::default()
    };
    use tracing_subscriber::prelude::*;
    let (layer, provider) = agentim_core::otel::layer(&cfg).unwrap().unwrap();
    tracing_subscriber::registry().with(layer).init();

    let db = new_db(Backend::Sqlite).await.unwrap();
    let mut c = single(&db.url);
    c.tracing = cfg;
    let hub = open(c).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    let a = Conn::new(&hub, &alice);
    let b = Conn::new(&hub, &bob);
    let conv = a.group(&[&bob, &bot]).await;
    let sent = |r: serde_json::Value| {
        let hub = hub.clone();
        async move {
            assert_eq!(r["ok"], true, "{r}");
            hub.message_trace_id(r["msg_id"].as_str().unwrap())
                .await
                .unwrap()
                .unwrap()
        }
    };
    // Not sampled: a person, and a person who asks for sampling.
    let plain = sent(b.send(&conv, "plain").await).await;
    let asked = sent(
        b.call(
            json!({"op": "send", "conv_id": conv, "text": "asked", "client_msg_id": "x",
            "trace": format!("00-{}-00f067aa0ba902b7-01", "c".repeat(32))}),
        )
        .await,
    )
    .await;
    // Dyed: alice.
    hub.add_dye_rule("user", "alice", "ticket 7", None, "admin:t")
        .await
        .unwrap();
    let dyed = sent(a.send(&conv, "dyed").await).await;
    // A trusted caller (the agent) that marked its trace sampled.
    let agent_trace = "4bf92f3577b34da6a3ce929d0e0e4736";
    let g = Conn::new(&hub, &bot);
    let continued = sent(
        g.call(
            json!({"op": "send", "conv_id": conv, "text": "on it", "client_msg_id": "g",
            "trace": format!("00-{agent_trace}-00f067aa0ba902b7-01")}),
        )
        .await,
    )
    .await;
    assert_eq!(continued, agent_trace);

    let p = provider.clone();
    tokio::task::spawn_blocking(move || p.force_flush())
        .await
        .unwrap()
        .unwrap();
    assert!(exported(&got, &dyed), "the dyed request is exported");
    assert!(
        exported(&got, agent_trace),
        "the sampled agent trace is exported"
    );
    assert!(!exported(&got, &plain), "others are not");
    assert!(!exported(&got, &asked), "a person cannot ask for it");
    assert!(
        !exported(&got, &"c".repeat(32)),
        "nor is their own trace id exported"
    );
}
