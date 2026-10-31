//! Dye rules (docs/14 §4.2, D140): who may add them, the checks, the
//! limit, audit; a dyed request is logged in full while others are not; the
//! dye follows the request to the writer, to agents' replies and to webhook
//! deliveries, and never leaves the server.

mod common;

use std::io::Write;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use agentim_core::config::WebhookConfig;
use agentim_core::hub::Hub;
use agentim_core::model::*;
use agentim_core::server::{AppState, router};
use axum::Router;
use axum::http::HeaderMap;
use axum::routing::post;
use common::*;
use serde_json::{Value, json};

backend_tests!(
    rules_are_checked_limited_and_kept,
    the_admin_api_needs_an_operator_and_audits,
    only_dyed_requests_are_logged_in_full,
    the_dye_follows_to_agents_replies_and_webhooks,
);

// ------------------------------------------------------------ the log

/// Everything the server logs in this test binary, as JSON lines (at WARN,
/// plus dyed requests from DEBUG, like `agentim` itself).
fn log() -> &'static Arc<Mutex<Vec<u8>>> {
    static LOG: OnceLock<Arc<Mutex<Vec<u8>>>> = OnceLock::new();
    LOG.get_or_init(|| {
        let buf = Arc::new(Mutex::new(Vec::new()));
        let w = buf.clone();
        use tracing_subscriber::prelude::*;
        let layer = tracing_subscriber::fmt::layer()
            .json()
            .flatten_event(true)
            .with_current_span(true)
            .with_span_list(false)
            .with_writer(move || Sink(w.clone()))
            .with_filter(agentim_core::trace::log_filter(
                tracing_subscriber::EnvFilter::new("warn"),
            ));
        let _ = tracing_subscriber::registry().with(layer).try_init();
        buf
    })
}

struct Sink(Arc<Mutex<Vec<u8>>>);

impl Write for Sink {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// The log lines of one trace.
fn lines_of(trace_id: &str) -> Vec<Value> {
    let buf = log().lock().unwrap().clone();
    String::from_utf8_lossy(&buf)
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|v| v["span"]["trace_id"] == trace_id || v["trace_id"] == trace_id)
        .collect()
}

async fn sent(hub: &Hub, r: &Value) -> String {
    assert_eq!(r["ok"], true, "{r}");
    hub.message_trace_id(r["msg_id"].as_str().unwrap())
        .await
        .unwrap()
        .unwrap()
}

// ------------------------------------------------------------ the rules

async fn rules_are_checked_limited_and_kept(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let maya = user(&hub, "maya", UserKind::Human, None).await;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&maya)).await;
    let conv = Conn::new(&hub, &maya).group(&[&bot]).await;
    let add = |kind: &'static str, target: String, reason: &'static str, minutes: Option<i64>| {
        let hub = hub.clone();
        async move {
            hub.add_dye_rule(kind, &target, reason, minutes, "admin:t")
                .await
        }
    };
    // Each kind, by id or developer-facing name.
    let r = add("user", "maya".into(), "ticket 42", None).await.unwrap();
    assert!(r.id.starts_with("dye_"));
    assert_eq!(
        r.expires_at - r.created_at,
        60 * 60_000,
        "an hour by default"
    );
    add("agent", bot.id.clone(), "loops", Some(24 * 60))
        .await
        .unwrap();
    add("conv", conv.clone(), "lost messages", Some(1))
        .await
        .unwrap();
    add("device", "phone-1".into(), "x", None).await.unwrap();
    add("api_key", "k_123".into(), "x", None).await.unwrap();
    assert_eq!(hub.dye_rules().len(), 5);
    // Refused, leaving nothing.
    for (kind, target, reason, minutes, code) in [
        ("planet", "maya", "x", None, "bad_request"),
        ("user", "", "x", None, "bad_request"),
        ("user", "maya", "  ", None, "bad_request"),
        ("device", "d2", "x", Some(0), "bad_request"),
        ("device", "d2", "x", Some(24 * 60 + 1), "bad_request"),
        ("user", "nobody", "x", None, "not_found"),
        ("user", "bot", "x", None, "not_found"),
        ("agent", "maya", "x", None, "not_found"),
        ("conv", "c_nope", "x", None, "not_found"),
        ("user", "maya", "again", None, "conflict"),
    ] {
        let e = add(kind, target.into(), reason, minutes).await.unwrap_err();
        assert_eq!(e.code, code, "{kind} {target:?}: {}", e.message);
    }
    let long = "r".repeat(501);
    let e = hub
        .add_dye_rule("device", "d3", &long, None, "admin:t")
        .await
        .unwrap_err();
    assert_eq!(e.code, "bad_request");
    assert_eq!(hub.dye_rules().len(), 5);
    // At most 100 at once.
    for i in 5..agentim_core::trace::DYE_MAX_RULES {
        add("device", format!("d{i}"), "x", None).await.unwrap();
    }
    let e = add("device", "one-more".into(), "x", None)
        .await
        .unwrap_err();
    assert_eq!(e.code, "rate_limited");
    // Removing frees a place; an unknown one is not_found.
    hub.remove_dye_rule(&r.id).await.unwrap();
    assert_eq!(
        hub.remove_dye_rule(&r.id).await.unwrap_err().code,
        "not_found"
    );
    add("device", "one-more".into(), "x", None).await.unwrap();
    // Kept across a restart.
    let before = hub.dye_rules();
    drop(hub);
    let hub = open(single(&db.url)).await;
    assert_eq!(hub.dye_rules(), before);
}

async fn the_admin_api_needs_an_operator_and_audits(db: TestDb) {
    let hub = open(single(&db.url)).await;
    user(&hub, "maya", UserKind::Human, None).await;
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
    let http = reqwest::Client::new();
    let call = |key: String, method: reqwest::Method, path: String, body: Option<Value>| {
        let http = http.clone();
        let base = base.clone();
        async move {
            let mut r = http
                .request(method, format!("{base}{path}"))
                .bearer_auth(key);
            if let Some(b) = body {
                r = r
                    .header("content-type", "application/json")
                    .body(b.to_string());
            }
            let res = r.send().await.unwrap();
            let s = res.status().as_u16();
            let v: Value = serde_json::from_str(&res.text().await.unwrap()).unwrap_or(Value::Null);
            (s, v)
        }
    };
    let (op_key, _) = hub.create_admin_key("ops", "operator").await.unwrap();
    let (dev_key, _) = hub.create_admin_key("dev", "developer").await.unwrap();
    let (mod_key, _) = hub.create_admin_key("mod", "moderator").await.unwrap();
    let rule =
        json!({"kind": "user", "target": "maya", "reason": "support ticket 42", "minutes": 30});
    // Developers and moderators may not.
    for k in [&dev_key, &mod_key] {
        let (s, _) = call(
            k.clone(),
            reqwest::Method::POST,
            "/admin/v1/deploy/dye-rules".into(),
            Some(rule.clone()),
        )
        .await;
        assert_eq!(s, 403);
        let (s, _) = call(
            k.clone(),
            reqwest::Method::GET,
            "/admin/v1/deploy/dye-rules".into(),
            None,
        )
        .await;
        assert_eq!(s, 403);
    }
    assert!(hub.dye_rules().is_empty(), "refusals leave nothing");
    // An operator may, with a reason; unknown fields are refused.
    let (s, v) = call(
        op_key.clone(),
        reqwest::Method::POST,
        "/admin/v1/deploy/dye-rules".into(),
        Some(json!({"kind": "user", "target": "maya", "reason": "x", "extra": 1})),
    )
    .await;
    assert_eq!(s, 400, "{v}");
    let (s, v) = call(
        op_key.clone(),
        reqwest::Method::POST,
        "/admin/v1/deploy/dye-rules".into(),
        Some(rule.clone()),
    )
    .await;
    assert_eq!(s, 201, "{v}");
    let id = v["id"].as_str().unwrap().to_string();
    assert_eq!(v["by"], "admin:key:ops");
    assert_eq!(
        v["expires_at"].as_i64().unwrap() - v["created_at"].as_i64().unwrap(),
        30 * 60_000
    );
    let (s, v) = call(
        op_key.clone(),
        reqwest::Method::GET,
        "/admin/v1/deploy/dye-rules".into(),
        None,
    )
    .await;
    assert_eq!(
        (s, v["rules"].as_array().unwrap().len(), v["max"].as_u64()),
        (200, 1, Some(100))
    );
    let (s, _) = call(
        op_key.clone(),
        reqwest::Method::DELETE,
        format!("/admin/v1/deploy/dye-rules/{id}"),
        None,
    )
    .await;
    assert_eq!(s, 200);
    let (s, v) = call(
        op_key.clone(),
        reqwest::Method::DELETE,
        format!("/admin/v1/deploy/dye-rules/{id}"),
        None,
    )
    .await;
    assert_eq!((s, v["code"].as_str()), (404, Some("not_found")));
    // Both changes audited, with the reason; the failed removal not.
    let audit = hub.audit_log(50).await.unwrap();
    let mine: Vec<&Value> = audit
        .iter()
        .filter(|a| {
            a["action"]
                .as_str()
                .is_some_and(|x| x.starts_with("admin.dye_rules."))
        })
        .collect();
    assert_eq!(mine.len(), 2, "{mine:?}");
    assert!(
        mine.iter().any(|a| a["action"] == "admin.dye_rules.add"
            && a["detail"]["reason"] == "support ticket 42")
    );
    assert!(
        mine.iter()
            .any(|a| a["action"] == "admin.dye_rules.remove" && a["target"] == id.as_str())
    );
}

// ------------------------------------------------------------ effects

async fn only_dyed_requests_are_logged_in_full(db: TestDb) {
    log();
    let hub = open(single(&db.url)).await;
    let maya = user(&hub, "maya", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let m = Conn::new(&hub, &maya);
    let b = Conn::new(&hub, &bob);
    let conv = m.group(&[&bob]).await;
    let rule = hub
        .add_dye_rule("user", "maya", "ticket", None, "admin:t")
        .await
        .unwrap();
    let dyed = sent(&hub, &m.send(&conv, "hello").await).await;
    let plain = sent(&hub, &b.send(&conv, "hi").await).await;
    // Maya's request: the debug request line and the dyed summary, with the
    // rule; the writer ran in the same trace.
    let lines = lines_of(&dyed);
    assert!(
        lines.iter().any(|l| l["message"] == "dyed request"
            && l["dye"] == rule.id.as_str()
            && l["op"] == "send"
            && l["result"] == "ok"),
        "{lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l["level"] == "DEBUG" && l["message"] == "request"),
        "{lines:?}"
    );
    // No content in the log.
    assert!(!lines.iter().any(|l| l.to_string().contains("hello")));
    // Bob's: nothing at this level.
    assert!(lines_of(&plain).is_empty(), "{:?}", lines_of(&plain));
    // A refused dyed request is logged with its code.
    let r = m
        .call(json!({"op": "send", "conv_id": "c_nope", "text": "x", "client_msg_id": "z", "trace": null}))
        .await;
    assert_eq!(r["ok"], false);
    let buf = String::from_utf8_lossy(&log().lock().unwrap()).to_string();
    assert!(
        buf.lines().any(|l| l.contains("\"dyed request\"")
            && l.contains(&rule.id)
            && l.contains("\"result\":\"not_found\"")),
        "the refusal is in the log"
    );
    // A conversation rule dyes everyone's requests in it.
    hub.remove_dye_rule(&rule.id).await.unwrap();
    hub.add_dye_rule("conv", &conv, "lost", None, "admin:t")
        .await
        .unwrap();
    let t = sent(&hub, &b.send(&conv, "now bob").await).await;
    assert!(lines_of(&t).iter().any(|l| l["message"] == "dyed request"));
}

async fn the_dye_follows_to_agents_replies_and_webhooks(db: TestDb) {
    log();
    // A webhook receiver that keeps the traceparent headers.
    let got: Arc<Mutex<Vec<(HeaderMap, String)>>> = Arc::default();
    let g = got.clone();
    let app = Router::new().route(
        "/hook",
        post(move |h: HeaderMap, body: String| {
            let g = g.clone();
            async move {
                g.lock().unwrap().push((h, body));
                axum::http::StatusCode::NO_CONTENT
            }
        }),
    );
    let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = tcp.local_addr().unwrap().to_string();
    tokio::spawn(async move { axum::serve(tcp, app).await.unwrap() });
    let mut cfg = single(&db.url);
    cfg.webhooks = vec![WebhookConfig {
        name: "w".into(),
        url: format!("http://{addr}/hook"),
        secret: "0123456789abcdef0123".into(),
        events: vec!["msg.created".into()],
        max_attempts: 3,
        timeout_ms: 2000,
        backoff_ms: 50,
    }];
    let hub = open(cfg).await;
    let maya = user(&hub, "maya", UserKind::Human, None).await;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&maya)).await;
    let m = Conn::new(&hub, &maya);
    let mut g = Conn::new(&hub, &bot);
    let conv = m.group(&[&bot]).await;
    let rule = hub
        .add_dye_rule("user", "maya", "ticket", None, "admin:t")
        .await
        .unwrap();
    g.drain();
    let t = sent(
        &hub,
        &m.call(json!({"op": "send", "conv_id": conv, "text": "@bot hi", "client_msg_id": "q"}))
            .await,
    )
    .await;
    // The agent's push carries the trace (not the dye); people's never do.
    let mut pushed = None;
    for _ in 0..50 {
        if let Some(f) = g
            .drain()
            .into_iter()
            .find(|f| f["type"] == "event" && f["event"]["type"] == "msg.created")
        {
            pushed = Some(f);
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let pushed = pushed.expect("pushed to the agent");
    let tp = pushed["trace"].as_str().expect("trace").to_string();
    assert!(tp.starts_with(&format!("00-{t}-")), "{tp}");
    assert!(tp.ends_with("-01"), "sampled: {tp}");
    assert!(
        !pushed.to_string().contains(&rule.id),
        "the dye stays inside"
    );
    // The agent replies in that trace: same trace, still dyed (the bot is
    // not dyed itself).
    let reply = g
        .call(json!({"op": "send", "conv_id": conv, "text": "hello maya", "client_msg_id": "r", "trace": tp}))
        .await;
    assert_eq!(sent(&hub, &reply).await, t);
    assert!(
        lines_of(&t)
            .iter()
            .any(|l| l["message"] == "dyed request" && l["user_id"] == bot.id.as_str()),
        "the reply is dyed"
    );
    // Webhooks: delivered with the traceparent of the write, no tracestate,
    // no rule id; the dyed delivery logged.
    for _ in 0..100 {
        if got.lock().unwrap().len() >= 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let got = got.lock().unwrap().clone();
    assert!(got.len() >= 2, "{}", got.len());
    for (h, body) in &got {
        let tp = h["traceparent"].to_str().unwrap();
        assert!(tp.starts_with(&format!("00-{t}-")), "{tp}");
        assert!(h.get("tracestate").is_none());
        assert!(!body.contains(&rule.id));
    }
    assert!(
        lines_of(&t)
            .iter()
            .any(|l| l["message"] == "dyed webhook delivery")
    );
}
