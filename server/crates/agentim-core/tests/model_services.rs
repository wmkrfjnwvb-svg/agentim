//! Model services (docs/en/design/assistants.md–§2, A1): the deployment's built-in services
//! and the app's own, against a fake provider that speaks the Anthropic
//! Messages, OpenAI Responses and Chat Completions protocols. The key is
//! checked with the provider before it is saved, stored sealed, never
//! returned or audited, and sent only to its own address. Every refusal
//! leaves no trace; services survive a restart; racing creations never pass
//! the limit. The protocol adapters are tested on their own: streamed text,
//! tool calls, usage, stop reasons and errors.

mod common;

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use agentim_core::ai::provider::{
    AiError, Endpoint, Item, Kind, Request, Stop, ToolDef, ToolResult,
};
use agentim_core::hub::Hub;
use agentim_core::server::{AppState, router};
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use common::*;
use serde_json::{Value, json};

backend_tests!(
    services_are_added_checked_and_listed,
    refusals_leave_no_trace,
    built_in_services_use_the_deployment_key,
    services_survive_a_restart,
    race_creating_services_against_the_limit,
);

const GOOD: &str = "sk-good-1234";

// --- A fake provider --------------------------------------------------------

#[derive(Default)]
struct Seen {
    /// (path, key header) of every request.
    calls: Vec<(String, String)>,
    /// The scripted stream for the next turn, by protocol.
    script: Option<(u16, String)>,
    /// Everything answers 503 / the good key is refused too.
    down: bool,
    revoked: bool,
}

#[derive(Clone)]
struct Fake {
    base: String,
    st: Arc<Mutex<Seen>>,
}

fn key_of(h: &HeaderMap) -> String {
    h.get("x-api-key")
        .or(h.get("authorization"))
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .trim_start_matches("Bearer ")
        .to_string()
}

async fn fake(State(st): State<Arc<Mutex<Seen>>>, uri: Uri, h: HeaderMap, _b: Bytes) -> Response {
    let path = uri.path().to_string();
    let key = key_of(&h);
    let mut s = st.lock().unwrap();
    s.calls.push((path.clone(), key.clone()));
    if path.starts_with("/down/") || s.down {
        return (StatusCode::SERVICE_UNAVAILABLE, "{}").into_response();
    }
    if path.starts_with("/bad/") {
        return (
            StatusCode::NOT_FOUND,
            json!({"error": {"message": "no such route"}}).to_string(),
        )
            .into_response();
    }
    if key != GOOD || s.revoked {
        return (
            StatusCode::UNAUTHORIZED,
            json!({"error": {"type": "authentication_error", "message": "invalid key"}})
                .to_string(),
        )
            .into_response();
    }
    if path.starts_with("/anthropic/") && h.get("anthropic-version").is_none() {
        return (
            StatusCode::BAD_REQUEST,
            json!({"error": {"message": "no version"}}).to_string(),
        )
            .into_response();
    }
    if path.ends_with("/models") {
        let ids: &[&str] = if path.starts_with("/anthropic/") {
            &["claude-sonnet-5-5", "claude-unpriced-9"]
        } else if path.starts_with("/openai/") {
            &[
                "gpt-5.4",
                "text-embedding-3-large",
                "gpt-image-1",
                "gpt-5.3-codex",
            ]
        } else {
            &["deepseek-flash", "local-model"]
        };
        let data: Vec<Value> = ids
            .iter()
            .map(|i| json!({"id": i, "display_name": format!("{i} name"), "max_input_tokens": 1000000}))
            .collect();
        return (StatusCode::OK, json!({"data": data}).to_string()).into_response();
    }
    match s.script.take() {
        Some((code, body)) => (
            StatusCode::from_u16(code).unwrap(),
            [("content-type", "text/event-stream")],
            body,
        )
            .into_response(),
        None => (StatusCode::NOT_FOUND, "{}").into_response(),
    }
}

impl Fake {
    async fn start() -> Fake {
        let st = Arc::new(Mutex::new(Seen::default()));
        let app = axum::Router::new().fallback(fake).with_state(st.clone());
        let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = tcp.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(tcp, app).await.unwrap() });
        Fake {
            base: format!("http://{addr}"),
            st,
        }
    }
    fn url(&self, p: &str) -> String {
        format!("{}/{p}", self.base)
    }
    fn calls(&self) -> Vec<(String, String)> {
        self.st.lock().unwrap().calls.clone()
    }
    fn set(&self, down: bool, revoked: bool) {
        let mut s = self.st.lock().unwrap();
        s.down = down;
        s.revoked = revoked;
    }
    fn script(&self, code: u16, body: &str) {
        self.st.lock().unwrap().script = Some((code, body.to_string()));
    }
}

// --- The server -------------------------------------------------------------

struct Srv {
    base: String,
    hub: Hub,
    http: reqwest::Client,
    dev: String,
    moderator: String,
}

fn cfg(db: &TestDb) -> agentim_core::config::Config {
    let mut c = single(&db.url);
    c.ai.private_addresses = true;
    c
}

impl Srv {
    async fn on(hub: Hub) -> Srv {
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
        let (moderator, _) = hub.create_admin_key("mod", "moderator").await.unwrap();
        Srv {
            base: format!("http://{addr}"),
            hub,
            http: reqwest::Client::new(),
            dev,
            moderator,
        }
    }

    async fn call(&self, key: &str, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        let mut r = self
            .http
            .request(method.parse().unwrap(), format!("{}{path}", self.base))
            .bearer_auth(key);
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

    async fn put(&self, name: &str, body: Value) -> (u16, Value) {
        self.call(
            &self.dev.clone(),
            "PUT",
            &format!("/admin/v1/app/model-services/{name}"),
            Some(body),
        )
        .await
    }

    async fn list(&self) -> Vec<Value> {
        let (s, v) = self
            .call(
                &self.dev.clone(),
                "GET",
                "/admin/v1/app/model-services",
                None,
            )
            .await;
        assert_eq!(s, 200, "{v}");
        v["services"].as_array().unwrap().clone()
    }

    /// Everything a refused request could have touched.
    async fn trace(&self) -> String {
        let services = self.hub.model_services().await.unwrap();
        let audit = self.hub.audit_log(10_000).await.unwrap().len();
        format!("{services:?}|{audit}")
    }

    async fn refused(
        &self,
        key: &str,
        method: &str,
        path: &str,
        body: Option<Value>,
        status: u16,
        rule: &str,
    ) {
        let before = self.trace().await;
        let (s, v) = self.call(key, method, path, body.clone()).await;
        assert_eq!(
            (s, v["rule"].as_str()),
            (status, Some(rule)),
            "{method} {path} {body:?} -> {v}"
        );
        assert_eq!(
            self.trace().await,
            before,
            "a refusal left a trace: {method} {path} {body:?}"
        );
    }
}

/// The stored key column of a service, as in the database.
async fn stored_key(db: &TestDb, name: &str) -> String {
    sqlx::any::install_default_drivers();
    let pool = sqlx::AnyPool::connect(&db.url).await.unwrap();
    let q = if db.url.starts_with("postgres") {
        "SELECT api_key FROM model_services WHERE name = $1"
    } else {
        "SELECT api_key FROM model_services WHERE name = ?"
    };
    let v: String = sqlx::query_scalar(q)
        .bind(name)
        .fetch_one(&pool)
        .await
        .unwrap();
    pool.close().await;
    v
}

async fn services_are_added_checked_and_listed(db: TestDb) {
    let f = Fake::start().await;
    let s = Srv::on(Hub::open(cfg(&db), hub_cfg()).await.unwrap()).await;
    assert_eq!(s.list().await, Vec::<Value>::new());
    // Claude: the key is checked with the provider before it is saved.
    let (st, v) = s
        .put("claude", json!({"provider": "anthropic", "base_url": f.url("anthropic/v1"), "api_key": GOOD, "daily_tokens": 500000}))
        .await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(
        (v["created"].as_bool(), v["models"].as_i64()),
        (Some(true), Some(2))
    );
    assert_eq!(
        f.calls().last().unwrap(),
        &("/anthropic/v1/models".to_string(), GOOD.to_string())
    );
    // Listed with a hint of the key, never the key; sealed in the database.
    let l = s.list().await;
    assert_eq!(l.len(), 1);
    assert_eq!(
        (
            l[0]["name"].as_str(),
            l[0]["provider"].as_str(),
            l[0]["builtin"].as_bool()
        ),
        (Some("claude"), Some("anthropic"), Some(false))
    );
    assert_eq!(l[0]["key_hint"], "…1234");
    assert_eq!(l[0]["daily_tokens"], 500000);
    assert!(!l.iter().any(|x| x.to_string().contains(GOOD)));
    let sealed = stored_key(&db, "claude").await;
    assert!(
        !sealed.contains(GOOD),
        "the key is encrypted at rest: {sealed}"
    );
    let audit = s.hub.audit_log(100).await.unwrap();
    assert!(
        !format!("{audit:?}").contains(GOOD),
        "the key is not audited"
    );
    assert!(format!("{audit:?}").contains("model_services.put"));
    // Its models, each saying whether it has a price; an app's own key can
    // use all of them.
    let (st, m) = s
        .call(
            &s.dev.clone(),
            "GET",
            "/admin/v1/app/model-services/claude/models",
            None,
        )
        .await;
    assert_eq!(st, 200, "{m}");
    let ms = m["models"].as_array().unwrap();
    assert_eq!(ms.len(), 2);
    let sonnet = ms.iter().find(|x| x["id"] == "claude-sonnet-5-5").unwrap();
    assert_eq!(sonnet["price"]["input"], 2.0);
    assert_eq!(sonnet["usable"], true);
    let other = ms.iter().find(|x| x["id"] == "claude-unpriced-9").unwrap();
    assert!(other["price"].is_null());
    assert_eq!(other["usable"], true);
    // A moderator reads, the second listing comes from the cache.
    let n = f.calls().len();
    let (st, _) = s
        .call(
            &s.moderator.clone(),
            "GET",
            "/admin/v1/app/model-services/claude/models",
            None,
        )
        .await;
    assert_eq!(st, 200);
    assert_eq!(f.calls().len(), n, "cached");
    // OpenAI and a compatible service: chat models only.
    let (st, v) = s
        .put(
            "gpt",
            json!({"provider": "openai", "base_url": f.url("openai/v1"), "api_key": GOOD}),
        )
        .await;
    assert_eq!((st, v["models"].as_i64()), (200, Some(2)), "{v}");
    let (st, v) = s
        .put(
            "local",
            json!({"provider": "compatible", "base_url": f.url("chat/v1/"), "api_key": GOOD}),
        )
        .await;
    assert_eq!(st, 200, "{v}");
    let l = s.list().await;
    assert_eq!(
        l.iter()
            .map(|x| x["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["claude", "gpt", "local"]
    );
    assert_eq!(l[2]["base_url"], f.url("chat/v1"), "trailing slash dropped");
    // Replacing without a key keeps the stored one (and checks it again).
    let (st, v) = s
        .put(
            "claude",
            json!({"provider": "anthropic", "base_url": f.url("anthropic/v1")}),
        )
        .await;
    assert_eq!((st, v["created"].as_bool()), (200, Some(false)), "{v}");
    assert_eq!(s.list().await[0]["daily_tokens"], 0);
    // The same request again changes nothing but the time.
    let (st, _) = s
        .put(
            "claude",
            json!({"provider": "anthropic", "base_url": f.url("anthropic/v1")}),
        )
        .await;
    assert_eq!(st, 200);
    assert_eq!(s.list().await.len(), 3);
    // Deleting: gone, then not found.
    let (st, _) = s
        .call(
            &s.dev.clone(),
            "DELETE",
            "/admin/v1/app/model-services/gpt",
            None,
        )
        .await;
    assert_eq!(st, 200);
    assert_eq!(s.list().await.len(), 2);
    s.refused(
        &s.dev.clone(),
        "DELETE",
        "/admin/v1/app/model-services/gpt",
        None,
        404,
        "model_service.unknown",
    )
    .await;
    s.refused(
        &s.dev.clone(),
        "GET",
        "/admin/v1/app/model-services/gpt/models",
        None,
        404,
        "model_service.unknown",
    )
    .await;
    // The provider goes down, or stops taking the key, after it was saved.
    f.set(true, false);
    s.refused(
        &s.dev.clone(),
        "GET",
        "/admin/v1/app/model-services/local/models",
        None,
        409,
        "model_service.provider_down",
    )
    .await;
    f.set(false, true);
    s.refused(
        &s.dev.clone(),
        "GET",
        "/admin/v1/app/model-services/local/models",
        None,
        409,
        "model_service.key_rejected",
    )
    .await;
    f.set(false, false);
    // Every key the fake saw went to the fake: none elsewhere, none other.
    assert!(f.calls().iter().all(|(_, k)| k == GOOD));
}

async fn refusals_leave_no_trace(db: TestDb) {
    let f = Fake::start().await;
    let s = Srv::on(Hub::open(cfg(&db), hub_cfg()).await.unwrap()).await;
    let dev = s.dev.clone();
    let p = "/admin/v1/app/model-services";
    let ok = |extra: Value| {
        let mut b =
            json!({"provider": "anthropic", "base_url": f.url("anthropic/v1"), "api_key": GOOD});
        for (k, v) in extra.as_object().unwrap() {
            if v.is_null() {
                b.as_object_mut().unwrap().remove(k);
            } else {
                b[k] = v.clone();
            }
        }
        b
    };
    for (name, body, status, rule) in [
        ("Bad Name", ok(json!({})), 400, "model_service.name"),
        ("-x", ok(json!({})), 400, "model_service.name"),
        (
            &"a".repeat(65)[..],
            ok(json!({})),
            400,
            "model_service.name",
        ),
        (
            "x",
            ok(json!({"provider": "Anthropic"})),
            400,
            "model_service.provider",
        ),
        (
            "x",
            ok(json!({"provider": "compatible", "base_url": null})),
            400,
            "model_service.url",
        ),
        (
            "x",
            ok(json!({"base_url": "ftp://x"})),
            400,
            "model_service.url",
        ),
        (
            "x",
            ok(json!({"base_url": "not a url"})),
            400,
            "model_service.url",
        ),
        ("x", ok(json!({"api_key": null})), 400, "model_service.key"),
        ("x", ok(json!({"api_key": ""})), 400, "model_service.key"),
        (
            "x",
            ok(json!({"api_key": "sk bad"})),
            400,
            "model_service.key",
        ),
        (
            "x",
            ok(json!({"api_key": "k".repeat(513)})),
            400,
            "model_service.key",
        ),
        (
            "x",
            ok(json!({"daily_tokens": 1_000_000_000_001u64})),
            400,
            "model_service.daily_tokens",
        ),
        (
            "x",
            ok(json!({"api_key": "sk-wrong"})),
            400,
            "model_service.key_refused",
        ),
        (
            "x",
            ok(json!({"base_url": f.url("down/v1")})),
            400,
            "model_service.unreachable",
        ),
        (
            "x",
            ok(json!({"base_url": f.url("bad/v1")})),
            400,
            "model_service.refused",
        ),
        (
            "x",
            ok(json!({"base_url": "http://127.0.0.1:9/v1"})),
            400,
            "model_service.unreachable",
        ),
    ] {
        s.refused(
            &dev,
            "PUT",
            &format!("{p}/{name}"),
            Some(body),
            status,
            rule,
        )
        .await;
    }
    // Unknown fields and broken bodies.
    let before = s.trace().await;
    let (st, _) = s
        .call(
            &dev,
            "PUT",
            &format!("{p}/x"),
            Some(ok(json!({"extra": 1}))),
        )
        .await;
    assert_eq!(st, 400);
    let (st, _) = s
        .call(&dev, "PUT", &format!("{p}/x"), Some(json!("nope")))
        .await;
    assert_eq!(st, 400);
    assert_eq!(s.trace().await, before);
    // Roles: a moderator only reads; no key at all is 401.
    let m = s.moderator.clone();
    let before = s.trace().await;
    let (st, _) = s
        .call(&m, "PUT", &format!("{p}/x"), Some(ok(json!({}))))
        .await;
    assert_eq!(st, 403);
    let (st, _) = s.call("nope", "GET", p, None).await;
    assert_eq!(st, 401);
    assert_eq!(s.trace().await, before);
    // The limit: ten, the eleventh refused; replacing one still works.
    for i in 0..10 {
        let (st, v) = s.put(&format!("s{i}"), ok(json!({}))).await;
        assert_eq!(st, 200, "{v}");
    }
    s.refused(
        &dev,
        "PUT",
        &format!("{p}/s10"),
        Some(ok(json!({}))),
        429,
        "model_service.limit",
    )
    .await;
    let (st, _) = s.put("s3", ok(json!({}))).await;
    assert_eq!(st, 200);
    // A private address is refused unless the deployment allows it.
    drop(s);
    let mut c = single(&db.url);
    c.ai.private_addresses = false;
    let s = Srv::on(Hub::open(c, hub_cfg()).await.unwrap()).await;
    let dev = s.dev.clone();
    s.refused(
        &dev,
        "PUT",
        &format!("{p}/s3"),
        Some(ok(json!({}))),
        400,
        "model_service.url",
    )
    .await;
    s.refused(
        &dev,
        "PUT",
        &format!("{p}/y"),
        Some(ok(json!({"base_url": "https://127.0.0.1/v1"}))),
        400,
        "model_service.url",
    )
    .await;
}

async fn built_in_services_use_the_deployment_key(db: TestDb) {
    let f = Fake::start().await;
    // A variable per backend, so tests running together don't share one.
    let var = format!("AGENTIM_TEST_AI_KEY_{}", db.url.len() % 97);
    let missing = format!("AGENTIM_TEST_AI_MISSING_{}", db.url.len() % 97);
    // SAFETY: tests set only their own, uniquely named variables.
    unsafe { std::env::set_var(&var, GOOD) };
    let mut c = cfg(&db);
    c.ai.builtin = vec![
        agentim_core::config::BuiltinModelService {
            name: "claude".into(),
            provider: "anthropic".into(),
            key_env: var.clone(),
            base_url: Some(f.url("anthropic/v1")),
            daily_tokens: 1000,
        },
        agentim_core::config::BuiltinModelService {
            name: "deepseek".into(),
            provider: "deepseek".into(),
            key_env: missing.clone(),
            base_url: Some(f.url("chat")),
            daily_tokens: 0,
        },
    ];
    let s = Srv::on(Hub::open(c, hub_cfg()).await.unwrap()).await;
    let l = s.list().await;
    assert_eq!(l.len(), 2);
    assert_eq!(
        (l[0]["builtin"].as_bool(), l[0]["ready"].as_bool()),
        (Some(true), Some(true))
    );
    assert_eq!(l[1]["ready"], false, "no key in the environment");
    assert!(l[0].get("key_hint").is_none());
    // With the deployment's key, only priced models are usable.
    let (st, m) = s
        .call(
            &s.dev.clone(),
            "GET",
            "/admin/v1/app/model-services/claude/models",
            None,
        )
        .await;
    assert_eq!(st, 200, "{m}");
    assert_eq!(m["builtin"], true);
    let usable: Vec<_> = m["models"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|x| x["usable"] == true)
        .map(|x| x["id"].as_str().unwrap())
        .collect();
    assert_eq!(usable, ["claude-sonnet-5-5"]);
    s.refused(
        &s.dev.clone(),
        "GET",
        "/admin/v1/app/model-services/deepseek/models",
        None,
        409,
        "model_service.not_ready",
    )
    .await;
    // An assistant on the built-in key needs a priced model.
    let alice = user(&s.hub, "alice", agentim_core::model::UserKind::Human, None).await;
    s.hub
        .create_user_with(
            "helper",
            agentim_core::model::UserKind::Agent,
            Some(&alice.id),
            None,
            true,
        )
        .await
        .unwrap();
    let before = s.hub.assistants().await.unwrap();
    let (st, v) = s
        .call(
            &s.dev.clone(),
            "PUT",
            "/admin/v1/app/agents/helper/assistant",
            Some(json!({"service": "claude", "model": "claude-unpriced-9"})),
        )
        .await;
    assert_eq!(
        (st, v["rule"].as_str()),
        (400, Some("assistant.model_unpriced")),
        "{v}"
    );
    assert_eq!(s.hub.assistants().await.unwrap(), before);
    let (st, v) = s
        .call(
            &s.dev.clone(),
            "PUT",
            "/admin/v1/app/agents/helper/assistant",
            Some(json!({"service": "claude", "model": "claude-sonnet-5-5"})),
        )
        .await;
    assert_eq!(st, 200, "{v}");
    // Built-in names can't be taken or deleted by the app.
    let body = json!({"provider": "anthropic", "base_url": f.url("anthropic/v1"), "api_key": GOOD});
    s.refused(
        &s.dev.clone(),
        "PUT",
        "/admin/v1/app/model-services/claude",
        Some(body),
        409,
        "model_service.builtin",
    )
    .await;
    s.refused(
        &s.dev.clone(),
        "DELETE",
        "/admin/v1/app/model-services/claude",
        None,
        400,
        "model_service.builtin_fixed",
    )
    .await;
    // Bad built-in configuration is refused at start.
    for (name, provider, base) in [
        ("Bad", "anthropic", None),
        ("x", "nope", None),
        ("x", "compatible", None),
    ] {
        let mut c = cfg(&db);
        c.ai.builtin = vec![agentim_core::config::BuiltinModelService {
            name: name.into(),
            provider: provider.into(),
            key_env: var.clone(),
            base_url: base,
            daily_tokens: 0,
        }];
        assert!(c.validate().is_err(), "{name} {provider}");
    }
}

async fn services_survive_a_restart(db: TestDb) {
    let f = Fake::start().await;
    let s = Srv::on(Hub::open(cfg(&db), hub_cfg()).await.unwrap()).await;
    let (st, _) = s
        .put(
            "claude",
            json!({"provider": "anthropic", "base_url": f.url("anthropic/v1"), "api_key": GOOD}),
        )
        .await;
    assert_eq!(st, 200);
    s.hub.shutdown();
    drop(s);
    let s = Srv::on(Hub::open(cfg(&db), hub_cfg()).await.unwrap()).await;
    assert_eq!(s.list().await.len(), 1);
    // The key still opens: the provider gets it.
    let (st, m) = s
        .call(
            &s.dev.clone(),
            "GET",
            "/admin/v1/app/model-services/claude/models",
            None,
        )
        .await;
    assert_eq!(st, 200, "{m}");
    assert_eq!(f.calls().last().unwrap().1, GOOD);
}

async fn race_creating_services_against_the_limit(db: TestDb) {
    let f = Fake::start().await;
    let s = Arc::new(Srv::on(Hub::open(cfg(&db), hub_cfg()).await.unwrap()).await);
    for round in 0..5 {
        for x in s.list().await {
            let n = x["name"].as_str().unwrap().to_string();
            s.call(
                &s.dev.clone(),
                "DELETE",
                &format!("/admin/v1/app/model-services/{n}"),
                None,
            )
            .await;
        }
        let body =
            json!({"provider": "anthropic", "base_url": f.url("anthropic/v1"), "api_key": GOOD});
        // 14 different names and 4 writes of one more name at once.
        let mut tasks = vec![];
        for i in 0..18 {
            let s = s.clone();
            let b = body.clone();
            let name = if i < 14 {
                format!("r{i}")
            } else {
                "same".to_string()
            };
            tasks.push(tokio::spawn(async move { s.put(&name, b).await }));
        }
        let mut ok = 0;
        let mut limited = 0;
        for t in tasks {
            match t.await.unwrap() {
                (200, _) => ok += 1,
                (429, v) => {
                    assert_eq!(v["rule"], "model_service.limit");
                    limited += 1
                }
                other => panic!("round {round}: {other:?}"),
            }
        }
        let l = s.list().await;
        assert_eq!(l.len(), 10, "round {round}: exactly the limit");
        assert_eq!(ok + limited, 18);
        let distinct: std::collections::BTreeSet<_> =
            l.iter().map(|x| x["name"].as_str().unwrap()).collect();
        assert_eq!(distinct.len(), 10);
        // Each created service is audited once per successful write.
        let audits = s.hub.audit_log(100_000).await.unwrap();
        assert!(format!("{audits:?}").matches("model_services.put").count() >= ok);
    }
}

// --- The protocol adapters --------------------------------------------------

fn endpoint(f: &Fake, kind: Kind, path: &str) -> Endpoint {
    Endpoint {
        kind,
        base: f.url(path),
        key: GOOD.into(),
        http: agentim_core::webhook::client(),
    }
}

fn ask() -> Request {
    Request {
        model: "m".into(),
        system: "be brief".into(),
        items: vec![Item::User("hi".into())],
        tools: vec![ToolDef {
            name: "search".into(),
            description: "find".into(),
            schema: json!({"type": "object"}),
        }],
        max_tokens: 1000,
    }
}

fn sse(events: &[Value]) -> String {
    events
        .iter()
        .map(|e| {
            format!(
                "event: {}\ndata: {e}\n\n",
                e["type"].as_str().unwrap_or("x")
            )
        })
        .collect()
}

async fn run(
    ep: &Endpoint,
    req: &Request,
) -> (Result<agentim_core::ai::provider::Turn, AiError>, String) {
    let mut streamed = String::new();
    let r = ep.turn(req, &mut |t| streamed.push_str(t)).await;
    (r, streamed)
}

#[tokio::test]
async fn anthropic_messages_stream() {
    let f = Fake::start().await;
    let ep = endpoint(&f, Kind::Anthropic, "anthropic/v1");
    f.script(200, &sse(&[
        json!({"type": "message_start", "message": {"usage": {"input_tokens": 10, "cache_read_input_tokens": 100, "cache_creation_input_tokens": 5, "output_tokens": 1}}}),
        json!({"type": "content_block_start", "index": 0, "content_block": {"type": "thinking", "thinking": ""}}),
        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "signature_delta", "signature": "sig"}}),
        json!({"type": "content_block_stop", "index": 0}),
        json!({"type": "content_block_start", "index": 1, "content_block": {"type": "text", "text": ""}}),
        json!({"type": "content_block_delta", "index": 1, "delta": {"type": "text_delta", "text": "Let me "}}),
        json!({"type": "content_block_delta", "index": 1, "delta": {"type": "text_delta", "text": "look."}}),
        json!({"type": "content_block_stop", "index": 1}),
        json!({"type": "content_block_start", "index": 2, "content_block": {"type": "tool_use", "id": "toolu_1", "name": "search", "input": {}}}),
        json!({"type": "content_block_delta", "index": 2, "delta": {"type": "input_json_delta", "partial_json": "{\"q\": "}}),
        json!({"type": "content_block_delta", "index": 2, "delta": {"type": "input_json_delta", "partial_json": "\"price\"}"}}),
        json!({"type": "content_block_stop", "index": 2}),
        json!({"type": "message_delta", "delta": {"stop_reason": "tool_use"}, "usage": {"output_tokens": 42}}),
        json!({"type": "message_stop"}),
    ]));
    let (t, streamed) = run(&ep, &ask()).await;
    let t = t.unwrap();
    assert_eq!(
        (streamed.as_str(), t.text.as_str()),
        ("Let me look.", "Let me look.")
    );
    assert_eq!(t.stop, Stop::Tools);
    assert_eq!(t.calls.len(), 1);
    assert_eq!(
        (t.calls[0].id.as_str(), t.calls[0].name.as_str()),
        ("toolu_1", "search")
    );
    assert_eq!(t.calls[0].input, json!({"q": "price"}));
    assert_eq!(
        (
            t.usage.input,
            t.usage.cache_read,
            t.usage.cache_write,
            t.usage.output
        ),
        (10, 100, 5, 42)
    );
    // The thinking block (with its signature) goes back unchanged.
    assert_eq!(t.raw[0]["type"], "thinking");
    assert_eq!(t.raw[0]["signature"], "sig");
    // Refusal and max tokens.
    for (reason, stop) in [
        ("refusal", Stop::Refusal),
        ("max_tokens", Stop::MaxTokens),
        ("end_turn", Stop::End),
    ] {
        f.script(200, &sse(&[
            json!({"type": "message_start", "message": {"usage": {"input_tokens": 1}}}),
            json!({"type": "message_delta", "delta": {"stop_reason": reason}, "usage": {"output_tokens": 0}}),
        ]));
        assert_eq!(run(&ep, &ask()).await.0.unwrap().stop, stop);
    }
    // Errors: in the stream, ended early, refused key, overloaded.
    f.script(
        200,
        &sse(&[json!({"type": "error", "error": {"type": "overloaded_error", "message": "busy"}})]),
    );
    assert!(matches!(
        run(&ep, &ask()).await.0,
        Err(AiError::Unavailable(_))
    ));
    f.script(
        200,
        &sse(&[json!({"type": "message_start", "message": {"usage": {}}})]),
    );
    assert!(matches!(
        run(&ep, &ask()).await.0,
        Err(AiError::Unavailable(_))
    ));
    f.script(429, &json!({"error": {"message": "slow down"}}).to_string());
    assert_eq!(
        run(&ep, &ask()).await.0,
        Err(AiError::RateLimited("slow down".into()))
    );
    f.script(529, "{}");
    assert!(matches!(
        run(&ep, &ask()).await.0,
        Err(AiError::Unavailable(_))
    ));
    f.script(
        400,
        &json!({"error": {"message": "unknown model"}}).to_string(),
    );
    assert_eq!(
        run(&ep, &ask()).await.0,
        Err(AiError::Bad("unknown model".into()))
    );
    let bad = Endpoint {
        key: "sk-wrong".into(),
        ..ep.clone()
    };
    assert!(matches!(run(&bad, &ask()).await.0, Err(AiError::Auth(_))));
    let gone = Endpoint {
        base: "http://127.0.0.1:9/v1".into(),
        ..ep
    };
    assert!(matches!(
        run(&gone, &ask()).await.0,
        Err(AiError::Unavailable(_))
    ));
}

#[tokio::test]
async fn openai_responses_stream() {
    let f = Fake::start().await;
    let ep = endpoint(&f, Kind::Openai, "openai/v1");
    let output = json!([
        {"type": "reasoning", "id": "rs_1", "encrypted_content": "enc"},
        {"type": "message", "content": [{"type": "output_text", "text": "Checking."}]},
        {"type": "function_call", "call_id": "call_1", "name": "search", "arguments": "{\"q\":\"x\"}"},
    ]);
    f.script(200, &sse(&[
        json!({"type": "response.created"}),
        json!({"type": "response.output_text.delta", "delta": "Check"}),
        json!({"type": "response.output_text.delta", "delta": "ing."}),
        json!({"type": "response.completed", "response": {"status": "completed", "output": output,
            "usage": {"input_tokens": 1000, "input_tokens_details": {"cached_tokens": 800}, "output_tokens": 50}}}),
    ]));
    let (t, streamed) = run(&ep, &ask()).await;
    let t = t.unwrap();
    assert_eq!(streamed, "Checking.");
    assert_eq!(t.stop, Stop::Tools);
    assert_eq!(t.calls[0].input, json!({"q": "x"}));
    assert_eq!(
        (t.usage.input, t.usage.cache_read, t.usage.output),
        (200, 800, 50)
    );
    // Next round: the output items go back, then the tool output.
    let mut next = ask();
    next.items.push(t.item());
    next.items.push(Item::Results(vec![ToolResult {
        call_id: "call_1".into(),
        content: "found".into(),
        is_error: false,
    }]));
    f.script(200, &sse(&[json!({"type": "response.completed", "response": {"status": "incomplete",
        "incomplete_details": {"reason": "max_output_tokens"}, "output": [], "usage": {"input_tokens": 1, "output_tokens": 1}}})]));
    assert_eq!(run(&ep, &next).await.0.unwrap().stop, Stop::MaxTokens);
    f.script(200, &sse(&[json!({"type": "response.completed", "response": {"status": "completed",
        "output": [{"type": "message", "content": [{"type": "refusal", "refusal": "no"}]}], "usage": {}}})]));
    assert_eq!(run(&ep, &ask()).await.0.unwrap().stop, Stop::Refusal);
    f.script(
        200,
        &sse(&[json!({"type": "response.failed", "response": {"error": {"message": "boom"}}})]),
    );
    assert!(matches!(
        run(&ep, &ask()).await.0,
        Err(AiError::Unavailable(_))
    ));
    f.script(
        200,
        &sse(&[json!({"type": "response.output_text.delta", "delta": "half"})]),
    );
    assert!(matches!(
        run(&ep, &ask()).await.0,
        Err(AiError::Unavailable(_))
    ));
}

#[tokio::test]
async fn chat_completions_stream() {
    let f = Fake::start().await;
    let ep = endpoint(&f, Kind::Deepseek, "chat");
    let chunk = |delta: Value, finish: Value| json!({"choices": [{"index": 0, "delta": delta, "finish_reason": finish}]});
    let body = [
        chunk(json!({"role": "assistant", "content": "Sure"}), Value::Null),
        chunk(json!({"content": ", one moment."}), Value::Null),
        chunk(json!({"tool_calls": [{"index": 0, "id": "c1", "type": "function", "function": {"name": "sea", "arguments": "{\"q\""}}]}), Value::Null),
        chunk(json!({"tool_calls": [{"index": 0, "function": {"name": "rch", "arguments": ":1}"}}]}), Value::Null),
        chunk(json!({}), json!("tool_calls")),
        json!({"choices": [], "usage": {"prompt_tokens": 500, "completion_tokens": 20, "prompt_cache_hit_tokens": 300}}),
    ]
    .iter()
    .map(|e| format!("data: {e}\n\n"))
    .collect::<String>()
        + "data: [DONE]\n\n";
    f.script(200, &body);
    let (t, streamed) = run(&ep, &ask()).await;
    let t = t.unwrap();
    assert_eq!(streamed, "Sure, one moment.");
    assert_eq!(t.stop, Stop::Tools);
    assert_eq!(
        (t.calls[0].id.as_str(), t.calls[0].name.as_str()),
        ("c1", "search")
    );
    assert_eq!(t.calls[0].input, json!({"q": 1}));
    assert_eq!(
        (t.usage.input, t.usage.cache_read, t.usage.output),
        (200, 300, 20)
    );
    // OpenAI-style cached tokens (Gemini), length and filter stops.
    let ep = endpoint(&f, Kind::Gemini, "chat");
    for (finish, stop) in [
        ("length", Stop::MaxTokens),
        ("content_filter", Stop::Refusal),
        ("stop", Stop::End),
    ] {
        let b = format!(
            "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
            chunk(json!({"content": "x"}), json!(finish)),
            json!({"choices": [], "usage": {"prompt_tokens": 10, "completion_tokens": 1, "prompt_tokens_details": {"cached_tokens": 4}}})
        );
        f.script(200, &b);
        let t = run(&ep, &ask()).await.0.unwrap();
        assert_eq!((t.stop, t.usage.input, t.usage.cache_read), (stop, 6, 4));
    }
    // No finish reason: the stream was cut.
    f.script(
        200,
        &format!("data: {}\n\n", chunk(json!({"content": "x"}), Value::Null)),
    );
    assert!(matches!(
        run(&ep, &ask()).await.0,
        Err(AiError::Unavailable(_))
    ));
    f.script(200, "data: {not json}\n\n");
    assert!(matches!(
        run(&ep, &ask()).await.0,
        Err(AiError::Unavailable(_))
    ));
}
