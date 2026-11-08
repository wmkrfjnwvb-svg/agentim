//! Skills and Connect for built-in agents (docs/en/design/assistants.md, A4–A5), against a
//! fake model that calls tools, a fake MCP server and a fake HTTP API. A
//! role names its skills and connections; the agent loads a skill when the
//! model asks, calls MCP tools and API operations (read-only ones unless
//! the connection allows writes), with the credential going only to the
//! connection. External agents get the same in their configuration.
//! Refusals leave no trace; things a role uses can't be deleted.

mod common;

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agentim_core::hub::Hub;
use agentim_core::model::*;
use agentim_core::server::{AppState, router};
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use common::*;
use serde_json::{Value, json};

backend_tests!(
    skills_are_loaded_when_needed,
    connections_give_tools,
    writes_wait_for_approval,
    approvals_by_operation_and_amount,
    cards_forms_and_steps,
    refusals_leave_no_trace,
);

const ANSWER: &str = "Here is what I found.";

#[derive(Default)]
struct World {
    /// Model requests seen.
    model: Vec<Value>,
    /// Tool calls the model makes, one per round, then the answer.
    plan: Vec<(String, Value)>,
    /// MCP requests (method, session header) and API requests (path, key).
    mcp: Vec<(String, String)>,
    api: Vec<(String, String)>,
    /// API requests that change something: method, path, `Idempotency-Key`.
    writes: Vec<(String, String, String)>,
    /// The MCP server refuses the credential.
    mcp_refuses: bool,
}

type S = Arc<Mutex<World>>;

async fn fake(
    State(st): State<S>,
    method: axum::http::Method,
    uri: Uri,
    h: HeaderMap,
    b: Bytes,
) -> Response {
    let path = uri.path().to_string();
    let body: Value = serde_json::from_slice(&b).unwrap_or(Value::Null);
    let header = |k: &str| {
        h.get(k)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string()
    };
    let mut w = st.lock().unwrap();
    if path.ends_with("/v1/models") {
        return (
            StatusCode::OK,
            json!({"data": [{"id": "deepseek-flash"}]}).to_string(),
        )
            .into_response();
    }
    if path.ends_with("/chat/completions") {
        w.model.push(body.clone());
        let tools_done = body["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|m| m["role"] == "tool")
            .count();
        let mut out = String::new();
        if tools_done < w.plan.len() {
            let (name, args) = w.plan[tools_done].clone();
            out += &format!(
                "data: {}\n\n",
                json!({"choices": [{"index": 0, "delta": {"tool_calls": [{"index": 0, "id": format!("call_{tools_done}"), "type": "function", "function": {"name": name, "arguments": args.to_string()}}]}, "finish_reason": "tool_calls"}]})
            );
        } else {
            out += &format!(
                "data: {}\n\n",
                json!({"choices": [{"index": 0, "delta": {"content": ANSWER}, "finish_reason": "stop"}]})
            );
        }
        out += &format!(
            "data: {}\n\ndata: [DONE]\n\n",
            json!({"choices": [], "usage": {"prompt_tokens": 100, "completion_tokens": 10}})
        );
        return (StatusCode::OK, [("content-type", "text/event-stream")], out).into_response();
    }
    if path == "/notjson" {
        return (StatusCode::OK, "hello").into_response();
    }
    if path == "/mcp" {
        if w.mcp_refuses || header("authorization") != "Bearer mcp-secret" {
            return (StatusCode::UNAUTHORIZED, "{}").into_response();
        }
        let method = body["method"].as_str().unwrap_or("").to_string();
        w.mcp.push((method.clone(), header("mcp-session-id")));
        let id = body["id"].clone();
        let result = match method.as_str() {
            "initialize" => {
                json!({"protocolVersion": "2025-06-18", "capabilities": {"tools": {}}, "serverInfo": {"name": "shop"}})
            }
            "notifications/initialized" => return StatusCode::ACCEPTED.into_response(),
            "tools/list" => json!({"tools": [
                {"name": "lookup_order", "description": "Finds an order", "inputSchema": {"type": "object", "properties": {"id": {"type": "string"}}}, "annotations": {"readOnlyHint": true}},
                {"name": "cancel_order", "description": "Cancels an order", "inputSchema": {"type": "object", "properties": {"id": {"type": "string"}}}},
            ]}),
            "tools/call" => {
                json!({"content": [{"type": "text", "text": format!("Order {} has shipped", body["params"]["arguments"]["id"].as_str().unwrap_or("?"))}]})
            }
            _ => return (StatusCode::BAD_REQUEST, "{}").into_response(),
        };
        // Answered as an event stream, with a session id.
        let ev = format!(
            "event: message\ndata: {}\n\n",
            json!({"jsonrpc": "2.0", "id": id, "result": result})
        );
        return (
            StatusCode::OK,
            [
                ("content-type", "text/event-stream"),
                ("mcp-session-id", "sess-1"),
            ],
            ev,
        )
            .into_response();
    }
    if let Some(rest) = path.strip_prefix("/api") {
        w.api.push((rest.to_string(), header("x-api-key")));
        if method != axum::http::Method::GET {
            w.writes.push((
                method.to_string(),
                rest.to_string(),
                header("idempotency-key"),
            ));
        }
        if header("x-api-key") != "api-secret" {
            return (StatusCode::UNAUTHORIZED, "{}").into_response();
        }
        return (
            StatusCode::OK,
            json!({"order": rest.trim_start_matches("/orders/"), "status": "delivered"})
                .to_string(),
        )
            .into_response();
    }
    (StatusCode::NOT_FOUND, "{}").into_response()
}

struct Env {
    base: String,
    fake: String,
    st: S,
    hub: Hub,
    http: reqwest::Client,
    dev: String,
}

const SPEC: &str = r#"{"openapi": "3.0.3", "paths": {
  "/orders/{id}": {"get": {"operationId": "getOrder", "summary": "One order", "parameters": [{"name": "id", "in": "path", "schema": {"type": "string"}}]},
                   "delete": {"operationId": "deleteOrder", "summary": "Delete", "parameters": [{"name": "id", "in": "path"}]}},
  "/refunds": {"post": {"operationId": "createRefund", "summary": "Refund an order", "parameters": [{"name": "order", "in": "query"}, {"name": "amount", "in": "query", "schema": {"type": "number"}}]}}}}"#;

impl Env {
    async fn start(db: &TestDb) -> Env {
        let st: S = Arc::new(Mutex::new(World::default()));
        let app = axum::Router::new().fallback(fake).with_state(st.clone());
        let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let fake_addr = tcp.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(tcp, app).await.unwrap() });
        let mut c = single(&db.url);
        c.ai.private_addresses = true;
        let hub = Hub::open(c, hub_cfg()).await.unwrap();
        let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = tcp.local_addr().unwrap().to_string();
        let r = router(AppState {
            hub: hub.clone(),
            demo_login: false,
            metrics_token: None,
        });
        tokio::spawn(async move {
            axum::serve(tcp, r.into_make_service_with_connect_info::<SocketAddr>())
                .await
                .unwrap()
        });
        let (dev, _) = hub.create_admin_key("dev", "developer").await.unwrap();
        Env {
            base: format!("http://{addr}"),
            fake: format!("http://{fake_addr}"),
            st,
            hub,
            http: reqwest::Client::new(),
            dev,
        }
    }

    async fn call(&self, method: &str, path: &str, body: Option<(&str, String)>) -> (u16, Value) {
        let mut r = self
            .http
            .request(
                method.parse().unwrap(),
                format!("{}/admin/v1/app{path}", self.base),
            )
            .bearer_auth(&self.dev);
        if let Some((ty, b)) = body {
            r = r.header("content-type", ty).body(b);
        }
        let res = r.send().await.unwrap();
        let s = res.status().as_u16();
        let t = res.text().await.unwrap();
        (s, serde_json::from_str(&t).unwrap_or(Value::String(t)))
    }
    async fn json(&self, method: &str, path: &str, v: Value) -> (u16, Value) {
        self.call(method, path, Some(("application/json", v.to_string())))
            .await
    }
    async fn md(&self, path: &str, md: &str) -> (u16, Value) {
        self.call("PUT", path, Some(("text/markdown", md.to_string())))
            .await
    }
    async fn ok(&self, r: (u16, Value)) -> Value {
        assert!((200..300).contains(&r.0), "{r:?}");
        r.1
    }

    /// A team agent the server runs, with a role.
    async fn agent(&self, role_md: &str) -> (User, User) {
        let alice = user(&self.hub, "alice", UserKind::Human, None).await;
        let (bot, _) = self
            .hub
            .create_user_with("helper", UserKind::Agent, Some(&alice.id), None, true)
            .await
            .unwrap();
        self.ok(self.json("PUT", "/model-services/local", json!({"provider": "deepseek", "base_url": format!("{}/v1", self.fake), "api_key": "k"})).await).await;
        self.ok(self.md("/roles/helper", role_md).await).await;
        self.ok(self
            .json("PUT", "/agents/helper/role", json!({"role": "helper"}))
            .await)
            .await;
        self.ok(self
            .json(
                "PUT",
                "/agents/helper/assistant",
                json!({"service": "local", "model": "deepseek-flash"}),
            )
            .await)
            .await;
        (alice, bot)
    }

    fn plan(&self, p: Vec<(&str, Value)>) {
        self.st.lock().unwrap().plan = p.into_iter().map(|(n, a)| (n.to_string(), a)).collect();
    }
    fn last_model(&self) -> Value {
        self.st.lock().unwrap().model.last().cloned().unwrap()
    }
}

async fn ask(c: &Conn, conv: &str, bot: &User, text: &str) -> String {
    let q = c.send(conv, text).await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    for _ in 0..400 {
        let p = c.pull(conv, 0).await;
        if let Some(m) = p["events"]
            .as_array()
            .unwrap()
            .iter()
            .rev()
            .map(|e| &e["event"]["msg"])
            .find(|m| {
                m["sender_id"] == bot.id.as_str()
                    && m["reply_to"] == q.as_str()
                    && m["state"] == "complete"
            })
        {
            return m["content"].as_str().unwrap().to_string();
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("no reply to {text}");
}

const ROLE_SKILLS: &str = "---\nname: helper\ndescription: Answers refund questions\nskills: [refunds, missing]\n---\nBe helpful.\n";
const SKILL: &str = "---\nname: refunds\ndescription: How refunds work and who approves them\n---\nRefunds over $100 need a manager. See policy.md.\n";

async fn skills_are_loaded_when_needed(db: TestDb) {
    let e = Env::start(&db).await;
    let v = e.ok(e.md("/skills/refunds", SKILL).await).await;
    assert_eq!(v["version"], 1);
    let v = e
        .ok(e
            .call(
                "PUT",
                "/skills/refunds/files/docs/policy.md",
                Some(("text/markdown", "Policy: 30 days.".into())),
            )
            .await)
        .await;
    assert_eq!(v["created"], true);
    let s = e.ok(e.call("GET", "/skills/refunds", None).await).await;
    assert_eq!(
        (s["version"].as_i64(), s["files"].clone()),
        (Some(2), json!(["docs/policy.md"])),
        "a file bumps the version"
    );
    assert!(
        s["instructions"]
            .as_str()
            .unwrap()
            .starts_with("Refunds over $100")
    );
    let (_, bot) = e.agent(ROLE_SKILLS).await;
    // External agents get the skill (an unknown name in the role is left out).
    let cfg = e.hub.agent_config(&bot).await.unwrap();
    assert_eq!(cfg["skills"].as_array().unwrap().len(), 1);
    assert_eq!(cfg["skills"][0]["name"], "refunds");
    assert_eq!(cfg["skills"][0]["files"], json!(["docs/policy.md"]));
    assert_eq!(
        cfg["skills"][0]["markdown"], SKILL,
        "the SKILL.md as written"
    );
    // Its reference files over HTTP with the agent's key (the SDKs write
    // them to disk, docs/en/design/assistants.md): only for an agent whose role names the skill.
    let alice = e.hub.user_by_name("alice").unwrap();
    let key = e.hub.issue_agent_key(&bot, &alice.id, false).await.unwrap()["key"]
        .as_str()
        .unwrap()
        .to_string();
    let other = e.hub.create_agent(&alice, "other").await.unwrap()["key"]
        .as_str()
        .unwrap()
        .to_string();
    let file = |path: &str, key: Option<&str>| {
        let mut r = e.http.get(format!("{}/api/agent/skills/{path}", e.base));
        if let Some(k) = key {
            r = r.bearer_auth(k);
        }
        async move {
            let r = r.send().await.unwrap();
            (r.status().as_u16(), r.text().await.unwrap())
        }
    };
    assert_eq!(
        file("refunds/files/docs/policy.md", Some(&key)).await,
        (200, "Policy: 30 days.".to_string())
    );
    for (path, key, status, rule) in [
        (
            "refunds/files/docs/policy.md",
            Some(other.as_str()),
            404,
            "skill.unknown",
        ),
        (
            "missing/files/a.md",
            Some(key.as_str()),
            404,
            "skill.unknown",
        ),
        (
            "refunds/files/nope.md",
            Some(key.as_str()),
            404,
            "skill.file_unknown",
        ),
        ("refunds/files/docs/policy.md", Some("nope"), 401, ""),
        ("refunds/files/docs/policy.md", None, 401, ""),
    ] {
        let (st, body) = file(path, key).await;
        assert_eq!(st, status, "{path} {body}");
        assert!(body.contains(rule), "{path} {body}");
    }
    // The built-in agent: skills listed, loaded and read when the model asks.
    e.plan(vec![
        ("load_skill", json!({"name": "refunds"})),
        (
            "read_skill_file",
            json!({"name": "refunds", "path": "docs/policy.md"}),
        ),
    ]);
    e.hub.start_assistants();
    let c = Conn::new(&e.hub, &alice);
    let conv = c
        .ok(json!({"op": "create_conv", "kind": "dm", "title": "d", "members": [bot.id]}))
        .await["conv_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        ask(&c, &conv, &bot, "Can I get a refund of $150?").await,
        ANSWER
    );
    let last = e.last_model();
    assert!(
        last["messages"][0]["content"]
            .as_str()
            .unwrap()
            .contains("- refunds: How refunds work")
    );
    let tool_names: Vec<_> = last["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["function"]["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        tool_names,
        ["load_skill", "read_skill_file", "show_card", "ask_form"]
    );
    let results: Vec<_> = last["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["role"] == "tool")
        .map(|m| m["content"].as_str().unwrap().to_string())
        .collect();
    assert!(
        results[0].contains("need a manager") && results[0].contains("docs/policy.md"),
        "{results:?}"
    );
    assert_eq!(results[1], "Policy: 30 days.");
    // A skill the role doesn't name, a missing file: errors back to the model.
    e.plan(vec![
        ("load_skill", json!({"name": "other"})),
        (
            "read_skill_file",
            json!({"name": "refunds", "path": "nope.md"}),
        ),
    ]);
    ask(&c, &conv, &bot, "again").await;
    let results: Vec<_> = e.last_model()["messages"]
        .as_array()
        .unwrap()
        .iter()
        .rev()
        .filter(|m| m["role"] == "tool")
        .take(2)
        .map(|m| m["content"].as_str().unwrap().to_string())
        .collect();
    assert!(
        results[1].contains("no skill named other") && results[0].contains("no file nope.md"),
        "{results:?}"
    );
    // A skill a role names can't be deleted; once the role drops it, it can.
    let (st, v) = e.call("DELETE", "/skills/refunds", None).await;
    assert_eq!((st, v["rule"].as_str()), (409, Some("skill.in_use")));
    e.ok(e
        .md(
            "/roles/helper",
            "---\nname: helper\ndescription: d\n---\nx\n",
        )
        .await)
        .await;
    let (st, _) = file("refunds/files/docs/policy.md", Some(&key)).await;
    assert_eq!(st, 404, "the role no longer names it");
    e.ok(e
        .call("DELETE", "/skills/refunds/files/docs/policy.md", None)
        .await)
        .await;
    e.ok(e.call("DELETE", "/skills/refunds", None).await).await;
    let (st, _) = e.call("GET", "/skills/refunds", None).await;
    assert_eq!(st, 404);
}

async fn connections_give_tools(db: TestDb) {
    let e = Env::start(&db).await;
    let mcp = json!({"kind": "mcp", "url": format!("{}/mcp", e.fake), "auth_header": "Authorization", "secret": "Bearer mcp-secret"});
    let v = e
        .ok(e.json("PUT", "/connections/shop", mcp.clone()).await)
        .await;
    assert_eq!(
        (v["created"].as_bool(), v["tools"].as_i64()),
        (Some(true), Some(2))
    );
    let api = json!({"kind": "api", "url": format!("{}/api", e.fake), "auth_header": "X-Api-Key", "secret": "api-secret", "spec": serde_json::from_str::<Value>(SPEC).unwrap()});
    e.ok(e.json("PUT", "/connections/orders", api).await).await;
    let l = e.ok(e.call("GET", "/connections", None).await).await;
    assert!(
        !l.to_string().contains("secret\":")
            && !l.to_string().contains("mcp-secret")
            && !l.to_string().contains("api-secret"),
        "{l}"
    );
    let t = e
        .ok(e.call("GET", "/connections/shop/tools", None).await)
        .await;
    let usable: Vec<_> = t["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|x| x["usable"] == true)
        .map(|x| x["name"].as_str().unwrap())
        .collect();
    assert_eq!(usable, ["lookup_order"], "read-only by default");
    let audit = e.hub.audit_log(1000).await.unwrap();
    assert!(!format!("{audit:?}").contains("mcp-secret"));
    let (_, bot) = e.agent("---\nname: helper\ndescription: Order help\nmcp: [shop]\napis: [orders]\n---\nHelp with orders.\n").await;
    // External agents get the connections with their credential.
    let cfg = e.hub.agent_config(&bot).await.unwrap();
    assert_eq!(
        cfg["mcp"][0]["headers"]["Authorization"],
        "Bearer mcp-secret"
    );
    assert_eq!(cfg["apis"][0]["spec"]["openapi"], "3.0.3");
    // The built-in agent: the read-only tools of both.
    e.plan(vec![
        ("shop__lookup_order", json!({"id": "42"})),
        ("orders__getOrder", json!({"id": "42"})),
    ]);
    e.hub.start_assistants();
    let alice = e.hub.user_by_name("alice").unwrap();
    let c = Conn::new(&e.hub, &alice);
    let conv = c
        .ok(json!({"op": "create_conv", "kind": "dm", "title": "d", "members": [bot.id]}))
        .await["conv_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(ask(&c, &conv, &bot, "Where is order 42?").await, ANSWER);
    let last = e.last_model();
    let tool_names: Vec<_> = last["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["function"]["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        tool_names,
        [
            "orders__getOrder",
            "shop__lookup_order",
            "show_card",
            "ask_form"
        ]
    );
    let results: Vec<_> = last["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["role"] == "tool")
        .map(|m| m["content"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(results[0], "Order 42 has shipped");
    assert!(results[1].contains("delivered"));
    let (mcp_calls, api_calls) = {
        let w = e.st.lock().unwrap();
        (w.mcp.clone(), w.api.clone())
    };
    assert!(
        mcp_calls
            .iter()
            .any(|(m, s)| m == "tools/call" && s == "sess-1"),
        "the session id is sent back"
    );
    assert_eq!(
        api_calls.last().unwrap(),
        &("/orders/42".to_string(), "api-secret".to_string())
    );
    // A write tool is not offered, so a model asking for it gets an error.
    e.plan(vec![("shop__cancel_order", json!({"id": "42"}))]);
    ask(&c, &conv, &bot, "cancel it").await;
    let r: Vec<_> = e.last_model()["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["role"] == "tool")
        .map(|m| m["content"].as_str().unwrap().to_string())
        .collect();
    assert!(r.last().unwrap().contains("unknown tool"));
    // Allowing writes offers them.
    let mut allow = mcp.clone();
    allow["writes"] = json!("allow");
    allow.as_object_mut().unwrap().remove("secret");
    e.ok(e.json("PUT", "/connections/shop", allow).await).await;
    e.plan(vec![("shop__cancel_order", json!({"id": "7"}))]);
    ask(&c, &conv, &bot, "cancel order 7").await;
    let last = e.last_model();
    assert!(last["tools"].to_string().contains("shop__cancel_order"));
    assert_eq!(
        last["messages"]
            .as_array()
            .unwrap()
            .iter()
            .rev()
            .find(|m| m["role"] == "tool")
            .unwrap()["content"],
        "Order 7 has shipped",
        "the kept secret still works"
    );
    // In use: can't delete.
    let (st, v) = e.call("DELETE", "/connections/shop", None).await;
    assert_eq!((st, v["rule"].as_str()), (409, Some("connection.in_use")));
    // The MCP server goes away: the agent still answers, without its tools.
    e.st.lock().unwrap().mcp_refuses = true;
    e.ok(e
        .json(
            "PUT",
            "/connections/orders",
            json!({"kind": "api", "url": format!("{}/api", e.fake), "auth_header": "X-Api-Key"}),
        )
        .await)
        .await;
    e.plan(vec![]);
    let (st, v) = e.call("GET", "/connections/shop/tools", None).await;
    assert_eq!(
        (st, v["rule"].as_str()),
        (409, Some("connection.down")),
        "checked live"
    );
}

/// Sends `text` and waits for `n` finished replies of `bot` to it.
async fn replies(c: &Conn, conv: &str, bot: &User, q: &str, n: usize) -> Vec<String> {
    for _ in 0..400 {
        let p = c.pull(conv, 0).await;
        let mut seen: Vec<(String, String)> = vec![];
        for m in p["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| &e["event"]["msg"])
        {
            if m["sender_id"] == bot.id.as_str() && m["reply_to"] == q && m["state"] == "complete" {
                let id = m["msg_id"].as_str().unwrap().to_string();
                let text = m["content"].as_str().unwrap().to_string();
                match seen.iter_mut().find(|(i, _)| *i == id) {
                    Some(x) => x.1 = text,
                    None => seen.push((id, text)),
                }
            }
        }
        if seen.len() >= n {
            return seen.into_iter().map(|x| x.1).collect();
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("no {n} replies to {q}");
}

/// The pending request in `c`'s inbox about a call of `tool`.
async fn tool_request(c: &Conn, tool: &str, args: &Value) -> Value {
    for _ in 0..200 {
        let inbox = c.ok(json!({"op": "inbox"})).await;
        if let Some(q) = inbox["items"].as_array().unwrap().iter().find(|i| {
            i["type"] == "request"
                && i["context"]["tool_call"]["tool"] == tool
                && i["context"]["tool_call"]["args"] == *args
        }) {
            return q.clone();
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("no request for {tool} {args}");
}

/// Write tools of a `confirm` connection: offered, but each call waits
/// for the person being answered, runs once after approval with the stored
/// arguments, and not at all after a refusal or when the connection no
/// longer allows it (docs/en/design/assistants.md).
async fn writes_wait_for_approval(db: TestDb) {
    let e = Env::start(&db).await;
    let api = json!({"kind": "api", "url": format!("{}/api", e.fake), "auth_header": "X-Api-Key", "secret": "api-secret", "writes": "confirm", "spec": serde_json::from_str::<Value>(SPEC).unwrap()});
    e.ok(e.json("PUT", "/connections/orders", api).await).await;
    let t = e
        .ok(e.call("GET", "/connections/orders/tools", None).await)
        .await;
    let del = t["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["name"] == "deleteOrder")
        .unwrap()
        .clone();
    assert_eq!(
        (&del["usable"], &del["confirm"]),
        (&json!(true), &json!(true))
    );
    // Parameters with their types (the console offers numbers as an amount).
    let refund = t["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["name"] == "createRefund")
        .unwrap();
    assert!(
        refund["params"]
            .as_array()
            .unwrap()
            .contains(&json!({"name": "amount", "type": "number"})),
        "{refund}"
    );
    let (_, bot) = e
        .agent(
            "---\nname: helper\ndescription: Order help\napis: [orders]\n---\nHelp with orders.\n",
        )
        .await;
    e.hub.start_assistants();
    let alice = e.hub.user_by_name("alice").unwrap();
    let c = Conn::new(&e.hub, &alice);
    let conv = c
        .ok(json!({"op": "create_conv", "kind": "dm", "title": "d", "members": [bot.id]}))
        .await["conv_id"]
        .as_str()
        .unwrap()
        .to_string();
    let writes = || e.st.lock().unwrap().writes.clone();
    let tool_result = |m: &Value| {
        m["messages"]
            .as_array()
            .unwrap()
            .iter()
            .rev()
            .find(|m| m["role"] == "tool")
            .map(|m| m["content"].as_str().unwrap().to_string())
            .unwrap_or_default()
    };
    // The call: not run, a request to the person asking instead.
    e.plan(vec![("orders__deleteOrder", json!({"id": "9"}))]);
    let q = c.send(&conv, "Delete order 9").await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(replies(&c, &conv, &bot, &q, 1).await, [ANSWER]);
    let last = e.last_model();
    assert!(
        last["tools"]
            .to_string()
            .contains("A person may have to approve")
    );
    let r = tool_result(&last);
    assert!(r.starts_with("Not done yet") && r.contains("alice"), "{r}");
    assert!(writes().is_empty(), "nothing ran yet");
    let req = tool_request(&c, "orders__deleteOrder", &json!({"id": "9"})).await;
    let rid = req["request_id"].as_str().unwrap().to_string();
    assert_eq!(req["kind"], "approve");
    assert_eq!(req["context"]["tool_call"]["connection"], "orders");
    assert!(req["title"].as_str().unwrap().contains("deleteOrder"));
    assert!(
        !req.to_string().contains("api-secret"),
        "no credential in the request"
    );
    let got = c.ok(json!({"op": "get_request", "request_id": rid})).await;
    assert_eq!(
        got["assignees"],
        json!([alice.id]),
        "the person being answered decides"
    );
    // Approved: runs once, with the stored arguments and the request as
    // the idempotency key; the model tells the result.
    e.plan(vec![]);
    c.ok(json!({"op": "decide", "request_id": rid, "decision": "approve"}))
        .await;
    let both = replies(&c, &conv, &bot, &q, 2).await;
    assert_eq!(both, [ANSWER, ANSWER]);
    let sys = e.last_model()["messages"][0]["content"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        sys.contains("alice approved it and it ran") && sys.contains("delivered"),
        "{sys}"
    );
    assert_eq!(
        writes(),
        [("DELETE".to_string(), "/orders/9".to_string(), rid.clone())]
    );
    // Another runner sees the decision again: nothing runs twice.
    e.hub.start_assistants();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(writes().len(), 1, "at most once");
    assert_eq!(replies(&c, &conv, &bot, &q, 2).await.len(), 2);
    // Rejected: not run; the model hears the reason.
    e.plan(vec![("orders__deleteOrder", json!({"id": "10"}))]);
    let q = c.send(&conv, "Delete order 10").await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    replies(&c, &conv, &bot, &q, 1).await;
    let req = tool_request(&c, "orders__deleteOrder", &json!({"id": "10"})).await;
    e.plan(vec![]);
    c.ok(json!({"op": "decide", "request_id": req["request_id"], "decision": "reject", "text": "orders are kept for audit"}))
        .await;
    replies(&c, &conv, &bot, &q, 2).await;
    let sys = e.last_model()["messages"][0]["content"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        sys.contains("alice declined") && sys.contains("orders are kept for audit"),
        "{sys}"
    );
    assert_eq!(writes().len(), 1, "a refused call never runs");
    // The connection is made read-only before the approval: not run.
    e.plan(vec![("orders__deleteOrder", json!({"id": "11"}))]);
    let q = c.send(&conv, "Delete order 11").await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    replies(&c, &conv, &bot, &q, 1).await;
    let req = tool_request(&c, "orders__deleteOrder", &json!({"id": "11"})).await;
    e.ok(e
        .json(
            "PUT",
            "/connections/orders",
            json!({"kind": "api", "url": format!("{}/api", e.fake), "auth_header": "X-Api-Key", "writes": "read_only", "spec": serde_json::from_str::<Value>(SPEC).unwrap()}),
        )
        .await)
        .await;
    e.plan(vec![]);
    c.ok(json!({"op": "decide", "request_id": req["request_id"], "decision": "approve"}))
        .await;
    replies(&c, &conv, &bot, &q, 2).await;
    let sys = e.last_model()["messages"][0]["content"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(sys.contains("no longer available"), "{sys}");
    assert_eq!(writes().len(), 1, "checked again before running");
    // Audited: the call and the one not run, without the result.
    let audit = format!("{:?}", e.hub.audit_log(1000).await.unwrap());
    assert!(audit.contains("tool.called") && audit.contains("tool.not_run"));
    assert!(!audit.contains("delivered"));
    // An unknown writes value is refused.
    let (st, v) = e
        .json(
            "PUT",
            "/connections/orders",
            json!({"kind": "api", "url": format!("{}/api", e.fake), "writes": "ask"}),
        )
        .await;
    assert_eq!((st, v["rule"].as_str()), (400, Some("connection.writes")));
}

/// Who decides a request.
async fn assignees(c: &Conn, rid: &Value) -> Value {
    c.ok(json!({"op": "get_request", "request_id": rid})).await["assignees"].clone()
}

/// Risk-tiered approval (docs/en/design/assistants.md): a refund up to the amount is
/// confirmed by the person asking, above it (or without an amount) by the
/// people a rule names; an operation a rule lets through runs at once;
/// named approvers not in the conversation leave it to the owner. Bad rules
/// are refused without a trace. The hand-off tool gives the conversation to
/// a person.
async fn approvals_by_operation_and_amount(db: TestDb) {
    let e = Env::start(&db).await;
    let (alice, bot) = e
        .agent(
            "---\nname: helper\ndescription: Order help\napis: [orders]\n---\nHelp with orders.\n",
        )
        .await;
    let bob = user(&e.hub, "bob", UserKind::Human, None).await;
    let sam = user(&e.hub, "sam", UserKind::Human, None).await;
    user(&e.hub, "zoe", UserKind::Human, None).await;
    let spec = serde_json::from_str::<Value>(SPEC).unwrap();
    let conn = |approvals: Value| json!({"kind": "api", "url": format!("{}/api", e.fake), "auth_header": "X-Api-Key", "secret": "api-secret", "writes": "confirm", "spec": spec.clone(), "approvals": approvals});
    // Bad rules: refused, nothing saved.
    for (rules, rule, status) in [
        (json!([{"tool": "nope"}]), "connection.approval_tool", 400),
        (
            json!([{"tool": "createRefund", "amount_field": "amount"}]),
            "connection.approval_amount",
            400,
        ),
        (
            json!([{"tool": "createRefund", "amount_field": "amount", "over": -1}]),
            "connection.approval_amount",
            400,
        ),
        (
            json!([{"tool": "createRefund", "approvers": []}]),
            "connection.approvers",
            400,
        ),
        (
            json!([{"tool": "createRefund", "approvers": ["nobody"]}]),
            "account.unknown_user",
            404,
        ),
        (
            json!([{"tool": "createRefund", "approvers": ["helper"]}]),
            "connection.approvers",
            400,
        ),
        (
            json!([{"tool": "createRefund", "confirm": false, "approvers": ["sam"]}]),
            "connection.approvers",
            400,
        ),
        (
            json!(vec![json!({"tool": "getOrder"}); 21]),
            "connection.approvals",
            400,
        ),
        (json!([{"tool": "getOrder", "extra": 1}]), "", 400),
    ] {
        let (st, v) = e
            .json("PUT", "/connections/orders", conn(rules.clone()))
            .await;
        assert_eq!(st, status, "{rules}: {v}");
        assert!(v.to_string().contains(rule), "{rules}: {v}");
    }
    assert!(
        e.ok(e.call("GET", "/connections", None).await)
            .await
            .to_string()
            .find("orders")
            .is_none(),
        "nothing saved"
    );
    e.ok(e
        .json(
            "PUT",
            "/connections/orders",
            conn(json!([
                {"tool": "createRefund", "amount_field": "amount", "over": 30, "approvers": ["sam"]},
                {"tool": "deleteOrder", "confirm": false},
            ])),
        )
        .await)
        .await;
    let l = e.ok(e.call("GET", "/connections", None).await).await;
    assert_eq!(
        l["connections"][0]["approvals"][0]["approvers"],
        json!([sam.id]),
        "names become ids: {l}"
    );
    // Saved again without `approvals` (the console changing the URL): the
    // rules stay; `[]` clears them.
    let keep = json!({"kind": "api", "url": format!("{}/api", e.fake), "auth_header": "X-Api-Key", "writes": "confirm"});
    e.ok(e.json("PUT", "/connections/orders", keep.clone()).await)
        .await;
    let l2 = e.ok(e.call("GET", "/connections", None).await).await;
    assert_eq!(
        l2["connections"][0]["approvals"], l["connections"][0]["approvals"],
        "{l2}"
    );
    let mut clear = keep.clone();
    clear["approvals"] = json!([]);
    e.ok(e.json("PUT", "/connections/orders", clear).await)
        .await;
    assert_eq!(
        e.ok(e.call("GET", "/connections", None).await).await["connections"][0]["approvals"],
        json!([])
    );
    e.ok(e
        .json(
            "PUT",
            "/connections/orders",
            conn(json!([
                {"tool": "createRefund", "amount_field": "amount", "over": 30, "approvers": ["sam"]},
                {"tool": "deleteOrder", "confirm": false},
            ])),
        )
        .await)
        .await;
    // Bob asks in a group with Sam (staff) and the agent.
    let a = Conn::new(&e.hub, &alice);
    let conv = a
        .ok(json!({"op": "create_conv", "kind": "group", "title": "g", "members": [bob.id, sam.id, bot.id]}))
        .await["conv_id"]
        .as_str()
        .unwrap()
        .to_string();
    e.hub.start_assistants();
    let (b, s) = (Conn::new(&e.hub, &bob), Conn::new(&e.hub, &sam));
    let writes = || e.st.lock().unwrap().writes.clone();
    let said = |m: &Value| {
        m["messages"]
            .as_array()
            .unwrap()
            .iter()
            .rev()
            .find(|m| m["role"] == "tool")
            .map(|m| m["content"].as_str().unwrap().to_string())
            .unwrap_or_default()
    };
    // Up to the amount: Bob confirms it himself; the card shows the operation.
    e.plan(vec![(
        "orders__createRefund",
        json!({"order": "1", "amount": 20}),
    )]);
    let q = b.send(&conv, "@helper refund order 1").await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    replies(&b, &conv, &bot, &q, 1).await;
    let r = tool_request(
        &b,
        "orders__createRefund",
        &json!({"order": "1", "amount": 20}),
    )
    .await;
    assert_eq!(assignees(&b, &r["request_id"]).await, json!([bob.id]));
    let op = &r["context"]["operation"];
    assert_eq!(
        (op["name"].as_str(), op["amount"].as_i64()),
        (Some("createRefund"), None),
        "no rule applied: {op}"
    );
    assert!(op["fields"].to_string().contains("\"amount\""), "{op}");
    // Above it: Sam decides, not Bob.
    e.plan(vec![(
        "orders__createRefund",
        json!({"order": "2", "amount": 120}),
    )]);
    let q = b.send(&conv, "@helper refund order 2").await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    replies(&b, &conv, &bot, &q, 1).await;
    assert!(
        said(&e.last_model()).contains("approval from sam (not the person"),
        "{}",
        said(&e.last_model())
    );
    let r = tool_request(
        &s,
        "orders__createRefund",
        &json!({"order": "2", "amount": 120}),
    )
    .await;
    assert_eq!(assignees(&s, &r["request_id"]).await, json!([sam.id]));
    assert_eq!(r["context"]["operation"]["amount"], 120);
    assert_eq!(r["context"]["operation"]["rule"]["over"], 30.0);
    // The inbox (docs/en/design/chat-components.md): Sam's row says what it is about; the owner
    // doesn't get a request named to someone else; limit and more.
    let inbox = s.ok(json!({"op": "inbox"})).await;
    let row = inbox["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["request_id"] == r["request_id"])
        .unwrap()
        .clone();
    assert_eq!(
        (
            row["conv_title"].as_str(),
            row["agent_name"].as_str(),
            row["asker_name"].as_str()
        ),
        (Some("g"), Some("helper"), Some("bob")),
        "{row}"
    );
    assert_eq!(inbox["more"], false);
    let owner = a.ok(json!({"op": "inbox"})).await;
    assert!(
        !owner["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["request_id"] == r["request_id"]),
        "the owner doesn't act on Sam's: {owner}"
    );
    let bobs = b.ok(json!({"op": "inbox", "limit": 1})).await;
    assert_eq!(bobs["items"].as_array().unwrap().len(), 1);
    assert_eq!(bobs["more"], false, "{bobs}");
    for l in [0, 501] {
        let v = b.call(json!({"op": "inbox", "limit": l})).await;
        assert_eq!(v["error"]["rule"], "request.inbox_limit", "{v}");
    }
    let no = b
        .call(json!({"op": "decide", "request_id": r["request_id"], "decision": "approve"}))
        .await;
    assert_eq!(
        no["error"]["code"], "forbidden",
        "Bob is not the approver: {no}"
    );
    e.plan(vec![]);
    s.ok(json!({"op": "decide", "request_id": r["request_id"], "decision": "approve"}))
        .await;
    replies(&b, &conv, &bot, &q, 2).await;
    assert_eq!(writes().len(), 1);
    assert_eq!(writes()[0].1, "/refunds");
    // No amount at all: counted as over.
    e.plan(vec![("orders__createRefund", json!({"order": "3"}))]);
    let q = b.send(&conv, "@helper refund order 3").await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    replies(&b, &conv, &bot, &q, 1).await;
    let r = tool_request(&s, "orders__createRefund", &json!({"order": "3"})).await;
    assert_eq!(assignees(&s, &r["request_id"]).await, json!([sam.id]));
    // An operation a rule lets through: runs at once, even on `confirm`.
    e.plan(vec![("orders__deleteOrder", json!({"id": "5"}))]);
    let q = b.send(&conv, "@helper drop order 5").await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    replies(&b, &conv, &bot, &q, 1).await;
    assert_eq!(writes().last().unwrap().0, "DELETE");
    assert_eq!(writes().last().unwrap().1, "/orders/5");
    assert!(!said(&e.last_model()).contains("Not done yet"));
    // Approvers not in the conversation: the owner decides.
    e.ok(e
        .json(
            "PUT",
            "/connections/orders",
            conn(json!([{"tool": "createRefund", "approvers": ["zoe"]}])),
        )
        .await)
        .await;
    e.plan(vec![(
        "orders__createRefund",
        json!({"order": "6", "amount": 5}),
    )]);
    let q = b.send(&conv, "@helper refund order 6").await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    replies(&b, &conv, &bot, &q, 1).await;
    let r = tool_request(
        &a,
        "orders__createRefund",
        &json!({"order": "6", "amount": 5}),
    )
    .await;
    assert_eq!(assignees(&a, &r["request_id"]).await, json!([alice.id]));
    // The hand-off tool (with the handoff setting): a person has it now.
    e.ok(e
        .json(
            "PUT",
            "/agents/helper/assistant",
            json!({"service": "local", "model": "deepseek-flash", "settings": {"handoff": true}}),
        )
        .await)
        .await;
    e.plan(vec![(
        "handoff_to_person",
        json!({"note": "Bob wants a manager"}),
    )]);
    b.send(&conv, "@helper I want a person").await;
    let mut handed = false;
    for _ in 0..200 {
        let inbox = a.ok(json!({"op": "inbox"})).await;
        if inbox["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["type"] == "handoff" && i["note"] == "Bob wants a manager")
        {
            handed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(handed, "the owner has the conversation");
    // Alice has the order-6 request and the hand-off: one at a time.
    let one = a.ok(json!({"op": "inbox", "limit": 1})).await;
    assert_eq!(
        (
            one["items"].as_array().unwrap().len(),
            one["more"].as_bool()
        ),
        (1, Some(true)),
        "{one}"
    );
    // The reply that says so is not cut off by the hand-off.
    let p = b.pull(&conv, 0).await;
    let last = p["events"]
        .as_array()
        .unwrap()
        .iter()
        .rev()
        .map(|e| &e["event"]["msg"])
        .find(|m| m["sender_id"] == bot.id.as_str())
        .cloned()
        .unwrap();
    assert_eq!(last["state"], "complete", "{last}");
    assert!(said(&e.last_model()).contains("takes the conversation over"));
    assert!(
        e.last_model()["tools"]
            .to_string()
            .contains("handoff_to_person")
    );
}

async fn refusals_leave_no_trace(db: TestDb) {
    let e = Env::start(&db).await;
    let trace = || async {
        format!(
            "{:?}|{:?}|{}",
            e.hub.skills().await.unwrap(),
            e.hub.connections().await.unwrap(),
            e.hub.audit_log(10_000).await.unwrap().len()
        )
    };
    let before = trace().await;
    let f = e.fake.clone();
    let ok_mcp = json!({"kind": "mcp", "url": format!("{f}/mcp"), "auth_header": "Authorization", "secret": "Bearer mcp-secret"});
    let with = |base: &Value, k: &str, v: Value| {
        let mut b = base.clone();
        if v.is_null() {
            b.as_object_mut().unwrap().remove(k);
        } else {
            b[k] = v;
        }
        b
    };
    for (name, body, rule) in [
        ("Bad", ok_mcp.clone(), "connection.name"),
        ("x", with(&ok_mcp, "kind", json!("grpc")), "connection.kind"),
        (
            "x",
            with(&ok_mcp, "url", json!("ftp://x")),
            "connection.url",
        ),
        (
            "x",
            with(&ok_mcp, "auth_header", json!("Bad Header")),
            "connection.header",
        ),
        (
            "x",
            with(&ok_mcp, "auth_header", Value::Null),
            "connection.header",
        ),
        (
            "x",
            with(&ok_mcp, "writes", json!("sometimes")),
            "connection.writes",
        ),
        (
            "x",
            with(&ok_mcp, "secret", json!("a\nb")),
            "connection.secret",
        ),
        (
            "x",
            with(&ok_mcp, "secret", json!("Bearer wrong")),
            "connection.refused",
        ),
        (
            "x",
            with(&ok_mcp, "url", json!("http://127.0.0.1:9/mcp")),
            "connection.unreachable",
        ),
        (
            "x",
            with(&ok_mcp, "url", json!(format!("{f}/notjson"))),
            "connection.invalid",
        ),
        (
            "x",
            with(&ok_mcp, "spec", json!({"openapi": "3.0.0"})),
            "connection.spec",
        ),
        (
            "x",
            json!({"kind": "api", "url": format!("{f}/api")}),
            "connection.spec",
        ),
        (
            "x",
            json!({"kind": "api", "url": format!("{f}/api"), "spec": {"swagger": "2.0"}}),
            "connection.spec",
        ),
    ] {
        let (st, v) = e
            .json("PUT", &format!("/connections/{name}"), body.clone())
            .await;
        assert_eq!((st, v["rule"].as_str()), (400, Some(rule)), "{body}: {v}");
    }
    let (st, v) = e.call("DELETE", "/connections/none", None).await;
    assert_eq!((st, v["rule"].as_str()), (404, Some("connection.unknown")));
    let (st, v) = e.call("GET", "/connections/none/tools", None).await;
    assert_eq!((st, v["rule"].as_str()), (404, Some("connection.unknown")));
    for (name, md, rule) in [
        ("Bad", SKILL, "skill.name"),
        ("refunds", "no header", "skill.header"),
        (
            "refunds",
            "---\nname: other\ndescription: d\n---\nx",
            "skill.name_mismatch",
        ),
        ("refunds", "---\nname: refunds\n---\nx", "skill.description"),
    ] {
        let (st, v) = e.md(&format!("/skills/{name}"), md).await;
        assert_eq!((st, v["rule"].as_str()), (400, Some(rule)), "{md}: {v}");
    }
    let (st, v) = e
        .md("/skills/refunds", &format!("{SKILL}{}", "x".repeat(70_000)))
        .await;
    assert_eq!((st, v["rule"].as_str()), (400, Some("skill.size")));
    for (path, rule) in [
        ("/skills/none/files/a.md", "skill.unknown"),
        ("/skills/none", "skill.unknown"),
    ] {
        let (st, v) = e.call("DELETE", path, None).await;
        assert_eq!((st, v["rule"].as_str()), (404, Some(rule)), "{path}");
    }
    assert_eq!(trace().await, before, "refusals leave no trace");
    // Files of a skill: path, size, encoding, the twenty-first.
    e.ok(e.md("/skills/refunds", SKILL).await).await;
    let before = trace().await;
    for (path, body, rule) in [
        ("x~1.md", b"x".to_vec(), "skill.path"),
        ("a b.md", b"x".to_vec(), "skill.path"),
        ("big.md", vec![b'a'; 512 * 1024 + 1], "skill.file_size"),
        ("bin.md", vec![0xff, 0xfe], "skill.file_encoding"),
    ] {
        if rule == "skill.file_encoding" {
            // Sent as raw bytes instead (a lossy string would be valid UTF-8).
            let res = e
                .http
                .put(format!(
                    "{}/admin/v1/app/skills/refunds/files/{path}",
                    e.base
                ))
                .bearer_auth(&e.dev)
                .body(body)
                .send()
                .await
                .unwrap();
            let s = res.status().as_u16();
            let v: Value = serde_json::from_str(&res.text().await.unwrap()).unwrap();
            assert_eq!((s, v["rule"].as_str()), (400, Some(rule)));
            continue;
        }
        let (st, v) = e
            .call(
                "PUT",
                &format!("/skills/refunds/files/{path}"),
                Some(("text/plain", String::from_utf8_lossy(&body).into_owned())),
            )
            .await;
        assert_eq!((st, v["rule"].as_str()), (400, Some(rule)), "{path}: {v}");
    }
    let (st, v) = e
        .call("DELETE", "/skills/refunds/files/none.md", None)
        .await;
    assert_eq!((st, v["rule"].as_str()), (404, Some("skill.file_unknown")));
    assert_eq!(trace().await, before);
    for i in 0..20 {
        e.ok(e
            .call(
                "PUT",
                &format!("/skills/refunds/files/f{i}.md"),
                Some(("text/plain", "x".into())),
            )
            .await)
            .await;
    }
    let (st, v) = e
        .call(
            "PUT",
            "/skills/refunds/files/f20.md",
            Some(("text/plain", "x".into())),
        )
        .await;
    assert_eq!((st, v["rule"].as_str()), (429, Some("skill.files")));
    // Twenty connections at most.
    for i in 0..20 {
        e.ok(e
            .json("PUT", &format!("/connections/c{i}"), ok_mcp.clone())
            .await)
            .await;
    }
    let (st, v) = e.json("PUT", "/connections/c20", ok_mcp.clone()).await;
    assert_eq!((st, v["rule"].as_str()), (429, Some("connection.limit")));
}

/// The built-in assistant's cards, forms and steps (docs/en/design/chat-components.md–§5): data
/// shown as a card, a form only the asker can fill in whose answers start
/// the next reply once, each tool call as a step (waiting on an approval,
/// then done), and the settings that turn each off.
async fn cards_forms_and_steps(db: TestDb) {
    let e = Env::start(&db).await;
    let (alice, bot) = e
        .agent(
            "---\nname: helper\ndescription: Order help\napis: [orders]\n---\nHelp with orders.\n",
        )
        .await;
    let bob = user(&e.hub, "bob", UserKind::Human, None).await;
    let spec = serde_json::from_str::<Value>(SPEC).unwrap();
    e.ok(e.json("PUT", "/connections/orders", json!({"kind": "api", "url": format!("{}/api", e.fake), "auth_header": "X-Api-Key", "secret": "api-secret", "writes": "confirm", "spec": spec})).await).await;
    let a = Conn::new(&e.hub, &alice);
    let group = || async {
        a.ok(json!({"op": "create_conv", "kind": "group", "title": "g", "members": [bob.id, bot.id]}))
            .await["conv_id"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let mut conv = group().await;
    e.hub.start_assistants();
    let mut b = Conn::new(&e.hub, &bob);
    b.drain();
    let msgs = |p: &Value| -> Vec<Value> {
        p["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["event"]["msg"].clone())
            .filter(|m| !m.is_null())
            .collect()
    };
    let mut steps: Vec<Value> = vec![];
    let collect = |b: &mut Conn, steps: &mut Vec<Value>| {
        for f in b.drain() {
            if f["type"] == "side" && f["event"]["type"] == "progress" {
                steps.push(f["event"]["data"].clone());
            }
        }
    };
    let tool_said = |m: &Value| -> Vec<String> {
        m["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|m| m["role"] == "tool")
            .map(|m| m["content"].as_str().unwrap_or_default().to_string())
            .collect()
    };
    // Data as a card, after looking the order up.
    e.plan(vec![
        ("orders__getOrder", json!({"id": "1"})),
        ("show_card", json!({"title": "Order 1", "blocks": [{"type": "facts", "items": [{"label": "Total", "value": 25, "format": "currency:USD"}, {"label": "Status", "value": "Delivered", "tone": "success"}]}]})),
    ]);
    let q = b.send(&conv, "@helper where is order 1").await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    let reply = replies(&b, &conv, &bot, &q, 1).await;
    assert!(!reply.is_empty());
    let all = msgs(&b.pull(&conv, 0).await);
    let card = all
        .iter()
        .find(|m| m["kind"] == "agentim.card" && m["reply_to"] == q.as_str())
        .expect("a card");
    assert_eq!(card["payload"]["title"], "Order 1");
    assert_eq!(card["payload"]["blocks"][0]["type"], "facts");
    assert_eq!(card["sender_id"], bot.id.as_str());
    let reply_id = all
        .iter()
        .find(|m| {
            m["sender_id"] == bot.id.as_str() && m["kind"] == "text" && m["reply_to"] == q.as_str()
        })
        .unwrap()["msg_id"]
        .clone();
    for _ in 0..100 {
        collect(&mut b, &mut steps);
        if steps
            .iter()
            .filter(|s| s["data"]["state"] == "done")
            .count()
            >= 2
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let mine: Vec<&Value> = steps.iter().filter(|s| s["msg_id"] == reply_id).collect();
    assert_eq!(mine.len(), 4, "running and done for each call: {steps:?}");
    assert_eq!(
        (
            mine[0]["data"]["state"].as_str(),
            mine[1]["data"]["state"].as_str()
        ),
        (Some("running"), Some("done"))
    );
    assert_eq!(mine[0]["data"]["title"], "One order", "the tool's summary");
    assert_eq!(mine[2]["data"]["title"], "Show the details");
    assert_eq!(mine[0]["data"]["id"], mine[1]["data"]["id"]);
    assert_ne!(mine[0]["data"]["id"], mine[2]["data"]["id"]);
    assert!(
        !steps.iter().any(|s| s.to_string().contains("\"1\"")
            && s["kind"] == "step"
            && s["text"].as_str().unwrap_or("").contains('1')),
        "no arguments in steps"
    );
    conv = group().await;
    // A card the model gets wrong: the error goes back to it; the step fails.
    steps.clear();
    e.plan(vec![(
        "show_card",
        json!({"blocks": [{"type": "form", "id": "f", "submit": "s", "fields": []}]}),
    )]);
    let q = b.send(&conv, "@helper show it").await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    replies(&b, &conv, &bot, &q, 1).await;
    assert!(
        tool_said(&e.last_model())
            .last()
            .unwrap()
            .contains("show_card shows text, facts, table or list")
    );
    let bad = json!({"blocks": [{"type": "table", "columns": [{"label": "A"}], "rows": [[1, 2]]}]});
    e.plan(vec![("show_card", bad)]);
    conv = group().await;
    let q = b.send(&conv, "@helper show the table").await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    replies(&b, &conv, &bot, &q, 1).await;
    assert!(
        tool_said(&e.last_model())
            .last()
            .unwrap()
            .contains("one value per column")
    );
    for _ in 0..100 {
        collect(&mut b, &mut steps);
        if steps
            .iter()
            .filter(|s| s["data"]["state"] == "failed")
            .count()
            >= 2
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(
        steps
            .iter()
            .filter(|s| s["data"]["state"] == "failed")
            .count(),
        2,
        "{steps:?}"
    );
    conv = group().await;
    // A form for the asker; their answers start the next reply, once.
    steps.clear();
    e.plan(vec![("ask_form", json!({"title": "Refund", "intro": "A few details", "fields": [
        {"id": "reason", "label": "Reason", "type": "select", "options": ["Broken", "Late"], "required": true},
        {"id": "day", "label": "Pickup", "type": "date", "min": "2026-01-01"},
        {"id": "more", "label": "More", "type": "textarea", "unknown": "dropped"}]}))]);
    let q = b.send(&conv, "@helper I want a refund").await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    replies(&b, &conv, &bot, &q, 1).await;
    let said = tool_said(&e.last_model());
    assert!(
        said.last()
            .unwrap()
            .starts_with("Form sent: bob sees the form"),
        "{said:?}"
    );
    let all = msgs(&b.pull(&conv, 0).await);
    let form = all
        .iter()
        .find(|m| m["kind"] == "agentim.card" && m["reply_to"] == q.as_str())
        .expect("a form")
        .clone();
    assert_eq!(form["payload"]["to"], json!([bob.id]));
    assert_eq!(form["payload"]["once"], true);
    assert_eq!(
        form["payload"]["blocks"][1]["fields"][2],
        json!({"id": "more", "label": "More", "type": "textarea"})
    );
    for _ in 0..100 {
        collect(&mut b, &mut steps);
        if steps.iter().any(|s| s["data"]["state"] == "waiting") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        steps
            .iter()
            .any(|s| s["data"]["state"] == "waiting" && s["data"]["title"] == "Ask for details"),
        "{steps:?}"
    );
    e.plan(vec![]);
    let fid = form["msg_id"].clone();
    let no = a.call(json!({"op": "respond", "msg_id": fid, "block_id": "form", "values": {"reason": "Late"}})).await;
    assert_eq!(no["error"]["code"], "forbidden", "{no}");
    b.ok(json!({"op": "respond", "msg_id": fid, "block_id": "form", "values": {"reason": "Broken", "day": "2026-03-02"}})).await;
    let fid = fid.as_str().unwrap().to_string();
    replies(&b, &conv, &bot, &fid, 1).await;
    let m = e.last_model();
    let sys = m["messages"][0]["content"]
        .as_str()
        .unwrap_or_default()
        .to_string()
        + &m.to_string();
    assert!(
        sys.contains("filled in the form \\\"Refund\\\"")
            || sys.contains("filled in the form \"Refund\""),
        "{m}"
    );
    assert!(
        sys.contains("- Reason: Broken")
            && sys.contains("- Pickup: 2026-03-02")
            && sys.contains("- More: (left empty)"),
        "{m}"
    );
    // The step that waited for the answers is done.
    for _ in 0..100 {
        collect(&mut b, &mut steps);
        if steps
            .iter()
            .any(|s| s["data"]["title"] == "Ask for details" && s["data"]["state"] == "done")
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let w = steps
        .iter()
        .find(|s| s["data"]["state"] == "waiting" && s["data"]["title"] == "Ask for details")
        .unwrap();
    let d = steps
        .iter()
        .find(|s| s["data"]["state"] == "done" && s["data"]["title"] == "Ask for details")
        .expect("the form step is done");
    assert_eq!(
        (&w["data"]["id"], &w["msg_id"]),
        (&d["data"]["id"], &d["msg_id"]),
        "the same step"
    );
    let again = b.call(json!({"op": "respond", "msg_id": fid, "block_id": "form", "values": {"reason": "Late"}})).await;
    assert_eq!(again["error"]["code"], "conflict");
    tokio::time::sleep(Duration::from_millis(300)).await;
    let n = msgs(&b.pull(&conv, 0).await)
        .iter()
        .filter(|m| m["reply_to"] == fid.as_str() && m["sender_id"] == bot.id.as_str())
        .map(|m| m["msg_id"].clone())
        .collect::<std::collections::HashSet<_>>()
        .len();
    assert_eq!(n, 1, "one reply to the answers");
    conv = group().await;
    // A call that waits for approval: the step waits, then is done.
    steps.clear();
    e.plan(vec![("orders__deleteOrder", json!({"id": "5"}))]);
    let q = b.send(&conv, "@helper drop order 5").await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    replies(&b, &conv, &bot, &q, 1).await;
    let r = tool_request(&b, "orders__deleteOrder", &json!({"id": "5"})).await;
    e.plan(vec![]);
    b.ok(json!({"op": "decide", "request_id": r["request_id"], "decision": "approve"}))
        .await;
    replies(&b, &conv, &bot, &q, 2).await;
    for _ in 0..100 {
        collect(&mut b, &mut steps);
        if steps
            .iter()
            .any(|s| s["data"]["detail"] == "Approved by bob")
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let w = steps
        .iter()
        .find(|s| s["data"]["state"] == "waiting")
        .expect("waiting");
    let d = steps
        .iter()
        .find(|s| s["data"]["detail"] == "Approved by bob")
        .expect("approved");
    assert_eq!(
        (w["data"]["id"].clone(), w["msg_id"].clone()),
        (d["data"]["id"].clone(), d["msg_id"].clone()),
        "the same step is updated"
    );
    assert_eq!(d["data"]["state"], "done");
    conv = group().await;
    // Each off.
    e.ok(e.json("PUT", "/agents/helper/assistant", json!({"service": "local", "model": "deepseek-flash", "settings": {"steps": false, "forms": false, "cards": false}})).await).await;
    steps.clear();
    b.drain();
    e.plan(vec![("orders__getOrder", json!({"id": "2"}))]);
    let q = b.send(&conv, "@helper and order 2").await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    replies(&b, &conv, &bot, &q, 1).await;
    let tools = e.last_model()["tools"].to_string();
    assert!(
        !tools.contains("show_card") && !tools.contains("ask_form"),
        "{tools}"
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    collect(&mut b, &mut steps);
    assert!(steps.is_empty(), "no steps: {steps:?}");
    conv = group().await;
    e.plan(vec![(
        "ask_form",
        json!({"title": "x", "fields": [{"id": "a", "label": "A", "type": "text"}]}),
    )]);
    let q = b.send(&conv, "@helper form please").await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    replies(&b, &conv, &bot, &q, 1).await;
    assert!(
        tool_said(&e.last_model())
            .last()
            .unwrap()
            .contains("unknown tool")
    );
}
