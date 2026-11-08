//! Agents the server answers for (docs/en/design/assistants.md, A2), against a fake model
//! service (Chat Completions): a built-in role becomes a team agent; a
//! message that wakes it gets one streamed reply from the model, with the
//! role's instructions and the conversation as context; every call is
//! recorded with its tokens and cost and without text. Failures and
//! limits answer with a short notice. Refusals leave no trace. Races:
//! many messages at once and two runners at once still give exactly one
//! reply per message (mutation check: a random `client_msg_id` gives two).

mod common;

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agentim_core::hub::Hub;
use agentim_core::model::*;
use agentim_core::server::{AppState, router};
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use common::*;
use serde_json::{Value, json};

backend_tests!(
    a_built_in_agent_answers_in_the_chat,
    failures_and_limits_answer_with_a_notice,
    refusals_leave_no_trace,
    race_many_messages_and_two_runners,
    knowledge_files_answer_questions,
    knowledge_refusals_and_limits,
    hosted_projects_spend_their_ai_credit,
);

const KEY: &str = "sk-model";
const ANSWER: &str = "Hello from the model, with a reply long enough to arrive in more than one piece of streamed text.";

#[derive(Default)]
struct Model {
    /// Chat requests seen (their bodies).
    bodies: Vec<Value>,
    /// Answer with this status instead (503: down).
    fail: Option<u16>,
    /// Wait this long before answering.
    delay_ms: u64,
}

#[derive(Clone)]
struct Fake {
    base: String,
    st: Arc<Mutex<Model>>,
}

async fn model(State(st): State<Arc<Mutex<Model>>>, uri: Uri, b: Bytes) -> Response {
    if uri.path().ends_with("/models") {
        return (
            StatusCode::OK,
            json!({"data": [{"id": "deepseek-flash"}, {"id": "free-model"}]}).to_string(),
        )
            .into_response();
    }
    let (fail, delay) = {
        let mut s = st.lock().unwrap();
        s.bodies
            .push(serde_json::from_slice(&b).unwrap_or(Value::Null));
        (s.fail, s.delay_ms)
    };
    tokio::time::sleep(Duration::from_millis(delay)).await;
    if let Some(code) = fail {
        return (
            StatusCode::from_u16(code).unwrap(),
            json!({"error": {"message": "down"}}).to_string(),
        )
            .into_response();
    }
    let words: Vec<&str> = ANSWER.split_inclusive(' ').collect();
    let mut body = String::new();
    for w in words {
        body += &format!(
            "data: {}\n\n",
            json!({"choices": [{"index": 0, "delta": {"content": w}, "finish_reason": null}]})
        );
    }
    body += &format!(
        "data: {}\n\n",
        json!({"choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]})
    );
    body += &format!(
        "data: {}\n\n",
        json!({"choices": [], "usage": {"prompt_tokens": 3000, "completion_tokens": 400, "prompt_cache_hit_tokens": 1000}})
    );
    body += "data: [DONE]\n\n";
    (
        StatusCode::OK,
        [("content-type", "text/event-stream")],
        body,
    )
        .into_response()
}

impl Fake {
    async fn start() -> Fake {
        let st = Arc::new(Mutex::new(Model::default()));
        let app = axum::Router::new().fallback(model).with_state(st.clone());
        let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = tcp.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(tcp, app).await.unwrap() });
        Fake {
            base: format!("http://{addr}/v1"),
            st,
        }
    }
    fn calls(&self) -> usize {
        self.st.lock().unwrap().bodies.len()
    }
    fn last(&self) -> Value {
        self.st
            .lock()
            .unwrap()
            .bodies
            .last()
            .cloned()
            .unwrap_or(Value::Null)
    }
    fn fail(&self, f: Option<u16>) {
        self.st.lock().unwrap().fail = f;
    }
}

struct Srv {
    base: String,
    hub: Hub,
    http: reqwest::Client,
    dev: String,
}

impl Srv {
    async fn start(db: &TestDb) -> Srv {
        let mut c = single(&db.url);
        c.ai.private_addresses = true;
        let hub = Hub::open(c, hub_cfg()).await.unwrap();
        let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = tcp.local_addr().unwrap().to_string();
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
        let (dev, _) = hub.create_admin_key("dev", "developer").await.unwrap();
        Srv {
            base: format!("http://{addr}"),
            hub,
            http: reqwest::Client::new(),
            dev,
        }
    }

    async fn upload(&self, agent: &str, name: &str, bytes: Vec<u8>) -> (u16, Value) {
        let res = self
            .http
            .put(format!(
                "{}/admin/v1/app/agents/{agent}/knowledge/{name}",
                self.base
            ))
            .bearer_auth(&self.dev)
            .body(bytes)
            .send()
            .await
            .unwrap();
        let s = res.status().as_u16();
        let t = res.text().await.unwrap();
        (s, serde_json::from_str(&t).unwrap_or(Value::String(t)))
    }

    async fn call(&self, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        let mut r = self
            .http
            .request(
                method.parse().unwrap(),
                format!("{}/admin/v1/app{path}", self.base),
            )
            .bearer_auth(&self.dev);
        if let Some(b) = body {
            r = r
                .header("content-type", "application/json")
                .body(b.to_string());
        }
        let res = r.send().await.unwrap();
        let s = res.status().as_u16();
        let t = res.text().await.unwrap();
        (s, serde_json::from_str(&t).unwrap_or(Value::String(t)))
    }

    async fn ok(&self, method: &str, path: &str, body: Option<Value>) -> Value {
        let (s, v) = self.call(method, path, body.clone()).await;
        assert!(
            (200..300).contains(&s),
            "{method} {path} {body:?} -> {s} {v}"
        );
        v
    }

    async fn service(&self, f: &Fake) {
        self.ok(
            "PUT",
            "/model-services/local",
            Some(json!({"provider": "deepseek", "base_url": f.base, "api_key": KEY})),
        )
        .await;
    }

    async fn trace(&self) -> String {
        let a = self.hub.assistants().await.unwrap();
        let s = self.hub.model_services().await.unwrap();
        let roles = self.hub.roles().await.unwrap();
        let audit = self.hub.audit_log(10_000).await.unwrap().len();
        let users = ["support", "helper", "assistant"].map(|n| self.hub.user_by_name(n).is_some());
        format!("{a:?}|{s:?}|{roles:?}|{audit}|{users:?}")
    }

    async fn refused(
        &self,
        method: &str,
        path: &str,
        body: Option<Value>,
        status: u16,
        rule: &str,
    ) {
        let before = self.trace().await;
        let (s, v) = self.call(method, path, body.clone()).await;
        assert_eq!(
            (s, v["rule"].as_str()),
            (status, Some(rule)),
            "{method} {path} {body:?} -> {v}"
        );
        assert_eq!(
            self.trace().await,
            before,
            "a refusal left a trace: {method} {path}"
        );
    }
}

/// The assistant's replies to a message, as they are now: (text, state).
async fn replies(c: &Conn, conv: &str, agent: &User, to: &str) -> Vec<(String, String)> {
    let p = c.pull(conv, 0).await;
    let mut latest: std::collections::BTreeMap<String, Value> = Default::default();
    for e in p["events"].as_array().unwrap() {
        let m = &e["event"]["msg"];
        if m["sender_id"] == agent.id.as_str() && m["reply_to"] == to {
            latest.insert(m["msg_id"].as_str().unwrap().to_string(), m.clone());
        }
    }
    latest
        .values()
        .map(|m| {
            (
                m["content"].as_str().unwrap_or("").to_string(),
                m["state"].as_str().unwrap_or("").to_string(),
            )
        })
        .collect()
}

/// Waits until the message has a finished reply; returns all replies.
async fn answered(c: &Conn, conv: &str, agent: &User, to: &str) -> Vec<(String, String)> {
    for _ in 0..800 {
        let r = replies(c, conv, agent, to).await;
        if r.iter().any(|(_, s)| s != "streaming") {
            return r;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("no reply to {to}: {:?}", replies(c, conv, agent, to).await);
}

async fn dm(c: &Conn, other: &User) -> String {
    c.ok(json!({"op": "create_conv", "kind": "dm", "title": "dm", "members": [other.id]}))
        .await["conv_id"]
        .as_str()
        .unwrap()
        .to_string()
}

async fn send(c: &Conn, conv: &str, text: &str) -> String {
    let r = c.send(conv, text).await;
    assert_eq!(r["ok"], true, "{r}");
    r["msg_id"].as_str().unwrap().to_string()
}

async fn a_built_in_agent_answers_in_the_chat(db: TestDb) {
    let f = Fake::start().await;
    let s = Srv::start(&db).await;
    let alice = user(&s.hub, "alice", UserKind::Human, None).await;
    s.service(&f).await;
    // The catalog: five roles, none enabled.
    let cat = s.ok("GET", "/assistants/catalog", None).await;
    let slugs: Vec<_> = cat["catalog"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["slug"].as_str().unwrap())
        .collect();
    assert_eq!(
        slugs,
        ["assistant", "translator", "notes", "support", "moderator"]
    );
    assert!(
        cat["catalog"]
            .as_array()
            .unwrap()
            .iter()
            .all(|c| c["enabled"] == false)
    );
    // Enabling one: the role, a team agent with alice as maintainer, the assistant.
    let v = s
        .ok(
            "POST",
            "/assistants/catalog/support",
            Some(json!({"owner": "alice", "service": "local", "model": "deepseek-flash"})),
        )
        .await;
    assert_eq!(v["assistant"]["created"], true, "{v}");
    let agent = s.hub.user_by_name("support").unwrap();
    assert!(agent.team);
    assert_eq!(agent.owner_id.as_deref(), Some(alice.id.as_str()));
    let cfg = s.hub.agent_config(&agent).await.unwrap();
    assert_eq!(cfg["role"]["name"], "support");
    let cat = s.ok("GET", "/assistants/catalog", None).await;
    assert_eq!(cat["catalog"][3]["enabled"], true);
    let l = s.ok("GET", "/assistants", None).await;
    assert_eq!(l["assistants"][0]["name"], "support");
    assert_eq!(l["assistants"][0]["settings"]["max_tokens"], 2048);
    // It answers in a direct conversation, streamed, as a reply.
    s.hub.start_assistants();
    let c = Conn::new(&s.hub, &alice);
    let conv = dmed(&c, &agent).await;
    let q1 = send(&c, &conv, "Do you ship to Canada?").await;
    let r = answered(&c, &conv, &agent, &q1).await;
    assert_eq!(r, vec![(ANSWER.to_string(), "complete".to_string())]);
    // The model got the role's instructions and the question.
    let body = f.last();
    assert_eq!(body["model"], "deepseek-flash");
    assert!(
        body["messages"][0]["content"]
            .as_str()
            .unwrap()
            .contains("customer support agent")
    );
    assert!(
        body["messages"][1]["content"]
            .as_str()
            .unwrap()
            .contains("alice: Do you ship to Canada?")
    );
    // A second question carries the first exchange as context.
    let q2 = send(&c, &conv, "And to Mexico?").await;
    answered(&c, &conv, &agent, &q2).await;
    let ctx = f.last()["messages"][1]["content"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        ctx.contains("Do you ship to Canada?")
            && ctx.contains("Hello from the model")
            && ctx.ends_with("alice: And to Mexico?"),
        "{ctx}"
    );
    // Every call is recorded: tokens and cost, no text.
    usage_rows(&s.hub, 2).await;
    let u = s.ok("GET", "/assistants/usage", None).await;
    let rows = u["usage"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(
        (
            rows[0]["input"].as_i64(),
            rows[0]["cache_read"].as_i64(),
            rows[0]["output"].as_i64()
        ),
        (Some(2000), Some(1000), Some(400))
    );
    assert_eq!(rows[0]["builtin"], false);
    assert!(
        rows[0]["cost_nanos"].as_i64().unwrap() > 0,
        "deepseek-flash has a price"
    );
    assert!(!u.to_string().contains("Canada") && !u.to_string().contains("Hello from"));
    // Its own messages and a message that doesn't wake it get no answer.
    let n = f.calls();
    let g = c.group(&[&agent]).await;
    send(&c, &g, "talking to the group, not to it").await;
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(f.calls(), n, "not woken: no model call");
    // After a restart it is still running and doesn't answer again.
    s.hub.shutdown();
    let s2 = Srv::start(&db).await;
    s2.hub.start_assistants();
    let c2 = Conn::new(&s2.hub, &alice);
    let q3 = send(&c2, &conv, "Still there?").await;
    answered(&c2, &conv, &agent, &q3).await;
    assert_eq!(
        replies(&c2, &conv, &agent, &q1).await.len(),
        1,
        "the first question still has one reply"
    );
    // Turning it off: messages get no reply.
    s2.ok("DELETE", "/agents/support/assistant", None).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let n = f.calls();
    send(&c2, &conv, "hello?").await;
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert_eq!(f.calls(), n);
}

async fn dmed(c: &Conn, agent: &User) -> String {
    dm(c, agent).await
}

async fn failures_and_limits_answer_with_a_notice(db: TestDb) {
    let f = Fake::start().await;
    let s = Srv::start(&db).await;
    let alice = user(&s.hub, "alice", UserKind::Human, None).await;
    let (bot, _) = s
        .hub
        .create_user_with("helper", UserKind::Agent, Some(&alice.id), None, true)
        .await
        .unwrap();
    s.service(&f).await;
    s.ok(
        "PUT",
        "/agents/helper/assistant",
        Some(json!({"service": "local", "model": "free-model",
        "settings": {"replies_per_minute": 2, "fallback": "Not now, sorry."}})),
    )
    .await;
    s.hub.start_assistants();
    let c = Conn::new(&s.hub, &alice);
    let conv = dm(&c, &bot).await;
    // The model is down: the fallback, and the call is still recorded.
    f.fail(Some(503));
    let q = send(&c, &conv, "one").await;
    assert_eq!(
        answered(&c, &conv, &bot, &q).await,
        vec![("Not now, sorry.".to_string(), "complete".to_string())]
    );
    // Rate limited by the provider.
    f.fail(Some(429));
    let q = send(&c, &conv, "two").await;
    assert!(answered(&c, &conv, &bot, &q).await[0].0.contains("busy"));
    // Three in a minute with a limit of two: the third is told to wait,
    // without a model call.
    f.fail(None);
    let n = f.calls();
    let q = send(&c, &conv, "three").await;
    let r = answered(&c, &conv, &bot, &q).await;
    assert!(r[0].0.contains("a lot of questions"), "{r:?}");
    assert_eq!(f.calls(), n);
    // An unpriced model with an app's own key costs nothing on our side.
    let u = s.ok("GET", "/assistants/usage", None).await;
    assert!(
        u["usage"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["cost_nanos"] == 0)
    );
    // The service's tokens per day: used up after one answered call.
    s.ok(
        "PUT",
        "/model-services/local",
        Some(json!({"provider": "deepseek", "base_url": f.base, "daily_tokens": 1})),
    )
    .await;
    s.ok(
        "PUT",
        "/agents/helper/assistant",
        Some(json!({"service": "local", "model": "free-model"})),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let q = send(&c, &conv, "four").await;
    assert_eq!(answered(&c, &conv, &bot, &q).await[0].0, ANSWER);
    usage_rows(&s.hub, 3).await;
    let q = send(&c, &conv, "five").await;
    assert!(
        answered(&c, &conv, &bot, &q).await[0]
            .0
            .contains("usage limit")
    );
}

async fn refusals_leave_no_trace(db: TestDb) {
    let f = Fake::start().await;
    let s = Srv::start(&db).await;
    let alice = user(&s.hub, "alice", UserKind::Human, None).await;
    s.hub
        .create_user_with("helper", UserKind::Agent, Some(&alice.id), None, true)
        .await
        .unwrap();
    s.service(&f).await;
    let ok = json!({"service": "local", "model": "deepseek-flash"});
    // An unknown agent: the console's own 404 (no rule), nothing written.
    let before = s.trace().await;
    assert_eq!(
        s.call("PUT", "/agents/nobody/assistant", Some(ok.clone()))
            .await
            .0,
        404
    );
    assert_eq!(s.trace().await, before);
    s.refused(
        "PUT",
        "/agents/alice/assistant",
        Some(ok.clone()),
        404,
        "agent.unknown",
    )
    .await;
    s.refused(
        "PUT",
        "/agents/helper/assistant",
        Some(json!({"service": "nope", "model": "m"})),
        404,
        "model_service.unknown",
    )
    .await;
    s.refused(
        "PUT",
        "/agents/helper/assistant",
        Some(json!({"service": "local", "model": "gpt-9"})),
        400,
        "assistant.model",
    )
    .await;
    for settings in [
        json!({"max_tokens": 10}),
        json!({"max_tokens": 32001}),
        json!({"context_tokens": 100}),
        json!({"replies_per_minute": 0}),
        json!({"replies_per_day": 0}),
        json!({"fallback": " "}),
        json!({"fallback": "x".repeat(501)}),
    ] {
        s.refused(
            "PUT",
            "/agents/helper/assistant",
            Some(json!({"service": "local", "model": "deepseek-flash", "settings": settings})),
            400,
            "assistant.settings",
        )
        .await;
    }
    s.refused(
        "DELETE",
        "/agents/helper/assistant",
        None,
        404,
        "assistant.unknown",
    )
    .await;
    let enable = |extra: Value| {
        let mut b = json!({"owner": "alice", "service": "local", "model": "deepseek-flash"});
        for (k, v) in extra.as_object().unwrap() {
            b[k] = v.clone();
        }
        b
    };
    s.refused(
        "POST",
        "/assistants/catalog/wizard",
        Some(enable(json!({}))),
        404,
        "assistant.catalog",
    )
    .await;
    s.refused(
        "POST",
        "/assistants/catalog/support",
        Some(enable(json!({"owner": "helper"}))),
        400,
        "assistant.owner",
    )
    .await;
    s.refused(
        "POST",
        "/assistants/catalog/support",
        Some(enable(json!({"owner": "ghost"}))),
        400,
        "assistant.owner",
    )
    .await;
    s.refused(
        "POST",
        "/assistants/catalog/support",
        Some(enable(json!({"id": "helper"}))),
        409,
        "assistant.exists",
    )
    .await;
    s.refused(
        "POST",
        "/assistants/catalog/support",
        Some(enable(json!({"model": "gpt-9"}))),
        400,
        "assistant.model",
    )
    .await;
    s.refused(
        "GET",
        "/assistants/usage?limit=0",
        None,
        400,
        "assistant.usage_limit",
    )
    .await;
    // A service an assistant uses can't be deleted.
    s.ok("PUT", "/agents/helper/assistant", Some(ok)).await;
    s.refused(
        "DELETE",
        "/model-services/local",
        None,
        409,
        "model_service.in_use",
    )
    .await;
}

async fn race_many_messages_and_two_runners(db: TestDb) {
    let f = Fake::start().await;
    f.st.lock().unwrap().delay_ms = 100;
    let s = Srv::start(&db).await;
    let alice = user(&s.hub, "alice", UserKind::Human, None).await;
    let (bot, _) = s
        .hub
        .create_user_with("helper", UserKind::Agent, Some(&alice.id), None, true)
        .await
        .unwrap();
    s.service(&f).await;
    s.ok(
        "PUT",
        "/agents/helper/assistant",
        Some(json!({"service": "local", "model": "deepseek-flash",
        "settings": {"replies_per_minute": 600}})),
    )
    .await;
    // Two runners on one node (as two nodes would be in a hand-over).
    s.hub.start_assistants();
    s.hub.start_assistants();
    let c = Arc::new(Conn::new(&s.hub, &alice));
    let conv = dm(&c, &bot).await;
    for round in 0..3 {
        let mut tasks = vec![];
        for i in 0..6 {
            let c = c.clone();
            let conv = conv.clone();
            tasks.push(tokio::spawn(async move {
                send(&c, &conv, &format!("question {round}.{i}")).await
            }));
        }
        let mut ids = vec![];
        for t in tasks {
            ids.push(t.await.unwrap());
        }
        for q in &ids {
            let r = answered(&c, &conv, &bot, q).await;
            assert_eq!(r.len(), 1, "round {round}: exactly one reply to {q}: {r:?}");
            assert_eq!(r[0].1, "complete");
        }
    }
    // One usage row per answered message.
    usage_rows(&s.hub, 18).await;
    let u = s.ok("GET", "/assistants/usage", None).await;
    let mut msgs: Vec<_> = u["usage"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["msg_id"].as_str().unwrap().to_string())
        .collect();
    let n = msgs.len();
    msgs.sort();
    msgs.dedup();
    assert_eq!((n, msgs.len()), (18, 18));
}

const SHIPPING: &str =
    "# Shipping\n\nWe ship to Canada and Mexico. Delivery takes 3 to 5 business days.\n";
const RETURNS: &str =
    "# Returns\n\nReturns are free within 30 days of delivery. Use the return label in the box.\n";

async fn knowledge_files_answer_questions(db: TestDb) {
    let f = Fake::start().await;
    let s = Srv::start(&db).await;
    let alice = user(&s.hub, "alice", UserKind::Human, None).await;
    let (bot, _) = s
        .hub
        .create_user_with("helper", UserKind::Agent, Some(&alice.id), None, true)
        .await
        .unwrap();
    s.service(&f).await;
    let (st, v) = s.upload("helper", "shipping.md", SHIPPING.into()).await;
    assert_eq!((st, v["created"].as_bool()), (200, Some(true)), "{v}");
    let (st, _) = s.upload("helper", "returns.md", RETURNS.into()).await;
    assert_eq!(st, 200);
    let l = s.ok("GET", "/agents/helper/knowledge", None).await;
    assert_eq!(
        l["files"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["returns.md", "shipping.md"]
    );
    assert_eq!(l["files"][1]["bytes"], SHIPPING.len());
    // Stored sealed: the text is not in the database as written.
    sqlx::any::install_default_drivers();
    let pool = sqlx::AnyPool::connect(&db.url).await.unwrap();
    let bodies: Vec<String> = sqlx::query_scalar("SELECT body FROM knowledge_docs")
        .fetch_all(&pool)
        .await
        .unwrap();
    pool.close().await;
    assert!(bodies.iter().all(|b| !b.contains("Canada")), "{bodies:?}");
    s.ok(
        "PUT",
        "/agents/helper/assistant",
        Some(json!({"service": "local", "model": "deepseek-flash",
        "settings": {"only_knowledge": true, "handoff": true}})),
    )
    .await;
    s.hub.start_assistants();
    let c = Conn::new(&s.hub, &alice);
    let conv = dm(&c, &bot).await;
    // A question the files answer: the matching piece is in the prompt, first.
    let q = send(&c, &conv, "How long does delivery to Canada take?").await;
    answered(&c, &conv, &bot, &q).await;
    let system = f.last()["messages"][0]["content"]
        .as_str()
        .unwrap()
        .to_string();
    let ship = system
        .find("<document name=\"shipping.md\">")
        .expect("shipping piece in the prompt");
    assert!(system.contains("3 to 5 business days"));
    assert!(
        system
            .find("<document name=\"returns.md\">")
            .is_none_or(|r| r > ship),
        "best first"
    );
    assert!(system.contains("Answer only from these documents"));
    // One nothing in the files covers: no model call, said so, handed over.
    let n = f.calls();
    let q = send(&c, &conv, "What is the meaning of life?").await;
    let r = answered(&c, &conv, &bot, &q).await;
    assert!(
        r[0].0.contains("don't have information") && r[0].0.contains("colleague"),
        "{r:?}"
    );
    assert_eq!(f.calls(), n);
    let mut handed = false;
    for _ in 0..50 {
        let p = c.pull(&conv, 0).await;
        if p.to_string().contains("handoff") {
            handed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(handed, "handed over to a person");
    // While a person has the conversation the agent stays quiet; its owner
    // hands it back.
    c.ok(json!({"op": "handback", "conv_id": conv, "agent_id": bot.id}))
        .await;
    // Replacing a file changes what is found; deleting removes it.
    let (st, v) = s
        .upload(
            "helper",
            "shipping.md",
            "# Shipping\n\nWe now also ship to Brazil.\n".into(),
        )
        .await;
    assert_eq!((st, v["created"].as_bool()), (200, Some(false)));
    let q = send(&c, &conv, "Do you ship to Brazil?").await;
    answered(&c, &conv, &bot, &q).await;
    assert!(
        f.last()["messages"][0]["content"]
            .as_str()
            .unwrap()
            .contains("Brazil")
    );
    s.ok("DELETE", "/agents/helper/knowledge/shipping.md", None)
        .await;
    assert_eq!(
        s.ok("GET", "/agents/helper/knowledge", None).await["files"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    // Without "only knowledge", an uncovered question goes to the model.
    s.ok(
        "PUT",
        "/agents/helper/assistant",
        Some(json!({"service": "local", "model": "deepseek-flash"})),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let n = f.calls();
    let q = send(&c, &conv, "Tell me a joke").await;
    assert_eq!(answered(&c, &conv, &bot, &q).await[0].0, ANSWER);
    assert_eq!(f.calls(), n + 1);
    // Files survive a restart and still open.
    s.hub.shutdown();
    let s2 = Srv::start(&db).await;
    let l = s2.ok("GET", "/agents/helper/knowledge", None).await;
    assert_eq!(l["files"][0]["name"], "returns.md");
    s2.hub.start_assistants();
    let c2 = Conn::new(&s2.hub, &alice);
    let q = send(&c2, &conv, "Are returns free?").await;
    answered(&c2, &conv, &bot, &q).await;
    assert!(
        f.last()["messages"][0]["content"]
            .as_str()
            .unwrap()
            .contains("Returns are free")
    );
}

async fn knowledge_refusals_and_limits(db: TestDb) {
    let s = Srv::start(&db).await;
    let alice = user(&s.hub, "alice", UserKind::Human, None).await;
    s.hub
        .create_user_with("helper", UserKind::Agent, Some(&alice.id), None, true)
        .await
        .unwrap();
    let trace = || async {
        format!(
            "{:?}|{}",
            s.hub
                .knowledge_list(&s.hub.user_by_name("helper").unwrap().id)
                .await
                .unwrap(),
            s.hub.audit_log(10_000).await.unwrap().len()
        )
    };
    let before = trace().await;
    for (name, bytes, status, rule) in [
        ("a.exe", b"x".to_vec(), 400, "knowledge.type"),
        ("noext", b"x".to_vec(), 400, "knowledge.type"),
        (".env", b"x".to_vec(), 400, "knowledge.name"),
        ("a.txt", vec![0xff, 0xfe, 0x00], 400, "knowledge.encoding"),
        ("a.txt", b"  \n ".to_vec(), 400, "knowledge.empty"),
        (
            "a.pdf",
            b"%PDF-1.4 broken".to_vec(),
            400,
            "knowledge.unreadable",
        ),
        (
            "big.txt",
            vec![b'a'; 5 * 1024 * 1024 + 1],
            400,
            "knowledge.size",
        ),
    ] {
        let (st, v) = s.upload("helper", name, bytes).await;
        assert_eq!(
            (st, v["rule"].as_str()),
            (status, Some(rule)),
            "{name}: {v}"
        );
    }
    let (st, v) = s.upload("alice", "a.md", b"x".to_vec()).await;
    assert_eq!((st, v["rule"].as_str()), (404, Some("agent.unknown")));
    let (st, v) = s
        .call("DELETE", "/agents/helper/knowledge/none.md", None)
        .await;
    assert_eq!((st, v["rule"].as_str()), (404, Some("knowledge.unknown")));
    assert_eq!(trace().await, before, "refusals leave no trace");
    // 2 million characters in all: three files of 700,000 don't fit.
    let big = "word ".repeat(140_000);
    assert_eq!(s.upload("helper", "a.txt", big.clone().into()).await.0, 200);
    assert_eq!(s.upload("helper", "b.txt", big.clone().into()).await.0, 200);
    let before = trace().await;
    let (st, v) = s.upload("helper", "c.txt", big.clone().into()).await;
    assert_eq!(
        (st, v["rule"].as_str()),
        (429, Some("knowledge.total")),
        "{v}"
    );
    assert_eq!(trace().await, before);
    s.ok("DELETE", "/agents/helper/knowledge/a.txt", None).await;
    s.ok("DELETE", "/agents/helper/knowledge/b.txt", None).await;
    // At most 50 files, raced: 56 at once, exactly 50 kept.
    let srv = Arc::new(s);
    let mut tasks = vec![];
    for i in 0..56 {
        let srv = srv.clone();
        tasks.push(tokio::spawn(async move {
            srv.upload("helper", &format!("f{i}.md"), format!("file {i}").into())
                .await
        }));
    }
    let mut ok = 0;
    for t in tasks {
        match t.await.unwrap() {
            (200, _) => ok += 1,
            (429, v) => assert_eq!(v["rule"], "knowledge.limit"),
            other => panic!("{other:?}"),
        }
    }
    assert_eq!(ok, 50);
    assert_eq!(
        srv.ok("GET", "/agents/helper/knowledge", None).await["files"]
            .as_array()
            .unwrap()
            .len(),
        50
    );
}

async fn hosted_projects_spend_their_ai_credit(db: TestDb) {
    let f = Fake::start().await;
    let var = format!("AGENTIM_TEST_BUILTIN_{}", db.url.len() % 97);
    // SAFETY: tests set only their own, uniquely named variables.
    unsafe { std::env::set_var(&var, KEY) };
    let mut c = single(&db.url);
    c.ai.private_addresses = true;
    c.ai.builtin = vec![agentim_core::config::BuiltinModelService {
        name: "ours".into(),
        provider: "deepseek".into(),
        key_env: var,
        base_url: Some(f.base.clone()),
        daily_tokens: 0,
    }];
    let hub = Hub::open(c, hub_cfg()).await.unwrap();
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let (bot, _) = hub
        .create_user_with("helper", UserKind::Agent, Some(&alice.id), None, true)
        .await
        .unwrap();
    hub.put_assistant(
        &bot.id,
        agentim_core::hub::assistants::AssistantReq {
            service: "ours".into(),
            model: "deepseek-flash".into(),
            settings: None,
        },
    )
    .await
    .unwrap();
    hub.start_assistants();
    let conv_c = Conn::new(&hub, &alice);
    let conv = dm(&conv_c, &bot).await;
    // Self-hosted (no `ai` in the limits): the built-in key just works.
    let q = send(&conv_c, &conv, "one").await;
    assert_eq!(answered(&conv_c, &conv, &bot, &q).await[0].0, ANSWER);
    let spent = usage_rows(&hub, 1).await;
    assert!(spent > 0);
    // Hosted, credit for another provider: told, no model call.
    hub.set_own_limits(
        json!({"ai": {"provider": "openai", "allow_until_nanos": 1_000_000_000_000i64}}),
    );
    let n = f.calls();
    let q = send(&conv_c, &conv, "two").await;
    assert!(
        answered(&conv_c, &conv, &bot, &q).await[0]
            .0
            .contains("isn't set up")
    );
    assert_eq!(f.calls(), n);
    // Free plan (nothing allowed): the same.
    hub.set_own_limits(json!({"ai": {"provider": null, "allow_until_nanos": 0}}));
    let q = send(&conv_c, &conv, "three").await;
    assert!(
        answered(&conv_c, &conv, &bot, &q).await[0]
            .0
            .contains("isn't set up")
    );
    // Credit for this provider, a little left: one more answer, then used up.
    hub.set_own_limits(json!({"ai": {"provider": "deepseek", "allow_until_nanos": spent + 1}}));
    let q = send(&conv_c, &conv, "four").await;
    assert_eq!(answered(&conv_c, &conv, &bot, &q).await[0].0, ANSWER);
    usage_rows(&hub, 2).await;
    let q = send(&conv_c, &conv, "five").await;
    assert!(
        answered(&conv_c, &conv, &bot, &q).await[0]
            .0
            .contains("used up")
    );
    assert_eq!(f.calls(), n + 1);
}

/// Waits until `n` model calls are recorded (written just after the reply
/// ends); what they cost.
async fn usage_rows(hub: &Hub, n: usize) -> i64 {
    for _ in 0..200 {
        let u = hub.ai_usage(0, 100).await.unwrap();
        if u.len() >= n {
            return u.iter().map(|r| r.cost_nanos).sum();
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("{n} calls were not recorded");
}
