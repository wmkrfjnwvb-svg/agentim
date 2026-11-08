//! Agent roles and configuration (docs/en/design/agents.md K1): roles are Markdown
//! files in the app's library (open API, console API, the owner's client);
//! an agent gets its configuration at login, fetches it over WebSocket, MCP
//! and HTTP (ETag), and is told `config.updated` when it changes. Every
//! refusal leaves no trace; roles survive a restart; assigning and deleting
//! a role race without leaving an agent with a role that is gone.

mod common;

use std::net::SocketAddr;

use agentim_core::hub::Hub;
use agentim_core::model::*;
use agentim_core::server::{AppState, router};
use common::*;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;

backend_tests!(
    roles_are_markdown_files_in_the_library,
    agents_get_their_config_at_login_and_when_it_changes,
    owners_give_roles_from_their_client,
    roles_in_use_cannot_be_deleted,
    roles_survive_a_restart,
    race_role_assignment_against_deletion,
);

const SALES: &str = "---\nname: sales\ndescription: Answers leads about pricing\ntags: [sales, crm]\ntools: Read, Grep\nmodel: inherit\ncolor: blue\n---\nBe brief and friendly.\n";

struct Srv {
    base: String,
    hub: Hub,
    http: reqwest::Client,
    key: String,
}

impl Srv {
    async fn start(db: &TestDb) -> Srv {
        Srv::on(Hub::open(single(&db.url), hub_cfg()).await.unwrap()).await
    }

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
        let scopes: Vec<String> = agentim_core::open_api::SCOPES
            .iter()
            .map(|s| s.to_string())
            .collect();
        let (key, _) = hub
            .create_api_key("test", &scopes, None, None)
            .await
            .unwrap();
        Srv {
            base: format!("http://{addr}"),
            hub,
            http: reqwest::Client::new(),
            key,
        }
    }

    async fn req(
        &self,
        method: &str,
        path: &str,
        auth: Option<&str>,
        body: Option<(&str, String)>,
        headers: &[(&str, &str)],
    ) -> (u16, reqwest::header::HeaderMap, Value) {
        let mut r = self
            .http
            .request(method.parse().unwrap(), format!("{}{path}", self.base));
        if let Some(k) = auth {
            r = r.header("authorization", format!("Bearer {k}"));
        }
        for (h, v) in headers {
            r = r.header(*h, *v);
        }
        if let Some((ty, b)) = body {
            r = r.header("content-type", ty).body(b);
        }
        let res = r.send().await.unwrap();
        let s = res.status().as_u16();
        let h = res.headers().clone();
        let t = res.text().await.unwrap();
        (s, h, serde_json::from_str(&t).unwrap_or(Value::String(t)))
    }

    async fn api(&self, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        let (s, _, v) = self
            .req(
                method,
                path,
                Some(&self.key),
                body.map(|b| ("application/json", b.to_string())),
                &[],
            )
            .await;
        (s, v)
    }

    async fn put_md(&self, name: &str, md: &str) -> (u16, Value) {
        let (s, _, v) = self
            .req(
                "PUT",
                &format!("/v1/roles/{name}"),
                Some(&self.key),
                Some(("text/markdown", md.to_string())),
                &[],
            )
            .await;
        (s, v)
    }

    /// Everything a refused request could have touched.
    async fn trace(&self) -> String {
        let roles = self.hub.roles().await.unwrap();
        let mut agents = vec![];
        for a in ["helper", "other"] {
            if let Some(u) = self.hub.user_by_name(a) {
                agents.push(self.hub.agent_config(&u).await.unwrap()["version"].clone());
            }
        }
        let audit = self.hub.audit_log(10_000).await.unwrap().len();
        format!("{roles:?}|{agents:?}|{audit}")
    }

    async fn agent_with_key(&self, owner: &User, name: &str) -> (User, String) {
        let r = Conn::new(&self.hub, owner)
            .ok(json!({"op": "create_agent", "name": name}))
            .await;
        let id = r["agent"]["id"].as_str().unwrap();
        (
            self.hub.user(id).unwrap(),
            r["key"].as_str().unwrap().to_string(),
        )
    }

    async fn ws(&self, key: &str) -> (Ws, Value) {
        let url = format!("{}/ws", self.base.replace("http", "ws"));
        let mut req =
            tokio_tungstenite::tungstenite::client::IntoClientRequest::into_client_request(url)
                .unwrap();
        req.headers_mut()
            .insert("authorization", format!("Bearer {key}").parse().unwrap());
        let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
        let w = next(&mut ws, |f| f["type"] == "welcome").await;
        (ws, w)
    }
}

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn next(ws: &mut Ws, want: impl Fn(&Value) -> bool) -> Value {
    loop {
        let m = tokio::time::timeout(std::time::Duration::from_secs(5), ws.next())
            .await
            .expect("a frame in time")
            .unwrap()
            .unwrap();
        if let Message::Text(t) = m {
            let v: Value = serde_json::from_str(&t).unwrap();
            if want(&v) {
                return v;
            }
        }
    }
}

/// No frame matching `want` within a moment.
async fn quiet(ws: &mut Ws, want: impl Fn(&Value) -> bool) {
    let end = tokio::time::Instant::now() + std::time::Duration::from_millis(400);
    while let Ok(Some(Ok(m))) = tokio::time::timeout_at(end, ws.next()).await {
        if let Message::Text(t) = m {
            let v: Value = serde_json::from_str(&t).unwrap();
            assert!(!want(&v), "unexpected {v}");
        }
    }
}

async fn ws_call(ws: &mut Ws, rid: u64, v: Value) -> Value {
    let mut v = v;
    v["rid"] = json!(rid);
    ws.send(Message::Text(v.to_string().into())).await.unwrap();
    next(ws, |f| f["type"] == "res" && f["rid"] == rid).await
}

async fn roles_are_markdown_files_in_the_library(db: TestDb) {
    let s = Srv::start(&db).await;
    assert_eq!(s.api("GET", "/v1/roles", None).await.1["roles"], json!([]));
    // A Claude Code subagent file, as written.
    let (st, v) = s.put_md("sales", SALES).await;
    assert_eq!((st, &v["version"]), (200, &json!(1)), "{v}");
    let (st, r) = s.api("GET", "/v1/roles/sales", None).await;
    assert_eq!(st, 200, "{r}");
    assert_eq!(r["description"], "Answers leads about pricing");
    assert_eq!(r["tags"], json!(["sales", "crm"]));
    assert_eq!(r["tools"], json!(["Read", "Grep"]));
    assert_eq!(r["model"], "inherit");
    assert_eq!(r["metadata"]["color"], "blue");
    assert_eq!(r["instructions"], "Be brief and friendly.");
    assert_eq!(r["markdown"], SALES);
    // Replaced (JSON form too): the version goes up.
    let md2 = SALES.replace("brief", "very brief");
    let (st, v) = s
        .api("PUT", "/v1/roles/sales", Some(json!({"markdown": md2})))
        .await;
    assert_eq!((st, &v["version"]), (200, &json!(2)), "{v}");
    let (_, l) = s.api("GET", "/v1/roles", None).await;
    assert_eq!(l["roles"].as_array().unwrap().len(), 1);
    assert_eq!(l["roles"][0]["version"], 2);
    assert!(
        l["roles"][0].get("markdown").is_none(),
        "the list has no files"
    );
    // Each refusal: its code and rule, nothing changed.
    let before = s.trace().await;
    let big = format!("---\ndescription: d\n---\n{}", "x".repeat(70_000));
    for (name, md, rule) in [
        ("Sales", SALES.to_string(), "role.name"),
        ("-x", "---\ndescription: d\n---\n".to_string(), "role.name"),
        ("other", SALES.to_string(), "role.name_mismatch"),
        ("x", "no header".to_string(), "role.header"),
        (
            "x",
            "---\ndescription: |\n  a\n---\n".to_string(),
            "role.header",
        ),
        (
            "x",
            "---\nname: x\n---\nbody".to_string(),
            "role.description",
        ),
        ("x", big, "role.size"),
    ] {
        let (st, v) = s.put_md(name, &md).await;
        assert_eq!((st, v["rule"].as_str()), (400, Some(rule)), "{name}: {v}");
    }
    let (st, _, v) = s
        .req(
            "PUT",
            "/v1/roles/x",
            Some(&s.key),
            Some(("text/markdown", String::new())),
            &[],
        )
        .await;
    assert_eq!((st, v["rule"].as_str()), (400, Some("role.header")));
    let (st, _, _) = s
        .req(
            "PUT",
            "/v1/roles/x",
            Some(&s.key),
            Some(("application/json", r#"{"markdown":"x","extra":1}"#.into())),
            &[],
        )
        .await;
    assert_eq!(st, 400);
    let (st, v) = s.api("GET", "/v1/roles/nope", None).await;
    assert_eq!((st, v["rule"].as_str()), (404, Some("role.unknown")));
    let (st, v) = s.api("DELETE", "/v1/roles/nope", None).await;
    assert_eq!((st, v["rule"].as_str()), (404, Some("role.unknown")));
    assert_eq!(s.trace().await, before, "refusals leave no trace");
    // Scopes: reading needs agents:read, writing agents:write.
    let ro = s
        .hub
        .create_api_key("ro", &["agents:read".to_string()], None, None)
        .await
        .unwrap()
        .0;
    let (st, _, _) = s.req("GET", "/v1/roles", Some(&ro), None, &[]).await;
    assert_eq!(st, 200);
    let (st, _, _) = s
        .req(
            "PUT",
            "/v1/roles/x",
            Some(&ro),
            Some(("text/markdown", SALES.into())),
            &[],
        )
        .await;
    assert_eq!(st, 403);
    // The console's API: the same library.
    let (adk, _) = s.hub.create_admin_key("t", "developer").await.unwrap();
    let (st, _, v) = s
        .req("GET", "/admin/v1/app/roles/sales", Some(&adk), None, &[])
        .await;
    assert_eq!((st, &v["version"]), (200, &json!(2)), "{v}");
    let (st, _, v) = s
        .req(
            "PUT",
            "/admin/v1/app/roles/support",
            Some(&adk),
            Some((
                "text/markdown",
                "---\ndescription: Support\n---\nHelp.".into(),
            )),
            &[],
        )
        .await;
    assert_eq!((st, &v["version"]), (200, &json!(1)), "{v}");
    let (st, _, _) = s
        .req(
            "DELETE",
            "/admin/v1/app/roles/support",
            Some(&adk),
            None,
            &[],
        )
        .await;
    assert_eq!(st, 200);
    let (modk, _) = s.hub.create_admin_key("m", "moderator").await.unwrap();
    let (st, _, _) = s
        .req("GET", "/admin/v1/app/roles", Some(&modk), None, &[])
        .await;
    assert_eq!(st, 200, "moderators read");
    let (st, _, _) = s
        .req(
            "PUT",
            "/admin/v1/app/roles/x",
            Some(&modk),
            Some(("text/markdown", SALES.into())),
            &[],
        )
        .await;
    assert_eq!(st, 403, "but do not write");
    let audits: Vec<String> = s
        .hub
        .audit_log(10_000)
        .await
        .unwrap()
        .iter()
        .map(|a| a["action"].as_str().unwrap_or("").to_string())
        .filter(|a| a.contains("roles."))
        .collect();
    assert_eq!(audits.len(), 4, "{audits:?}");
}

async fn agents_get_their_config_at_login_and_when_it_changes(db: TestDb) {
    let s = Srv::start(&db).await;
    let alice = user(&s.hub, "alice", UserKind::Human, None).await;
    let (helper, key) = s.agent_with_key(&alice, "helper").await;
    // Login: no role yet.
    let (mut ws, w) = s.ws(&key).await;
    let c0 = w["config"].clone();
    assert_eq!(c0["role"], Value::Null, "{w}");
    assert_eq!(
        (c0["skills"].clone(), c0["mcp"].clone(), c0["apis"].clone()),
        (json!([]), json!([]), json!([]))
    );
    let v0 = c0["version"].as_str().unwrap().to_string();
    assert_eq!(v0.len(), 16);
    // Given a role: told at once; the new config has the instructions.
    s.put_md("sales", SALES).await;
    let (st, v) = s
        .api(
            "PUT",
            &format!("/v1/agents/{}/role", helper.name),
            Some(json!({"role": "sales"})),
        )
        .await;
    assert_eq!((st, &v["changed"]), (200, &json!(true)), "{v}");
    next(&mut ws, |f| f["type"] == "config.updated").await;
    let c1 = ws_call(&mut ws, 1, json!({"op": "get_config"})).await;
    assert_eq!(c1["role"]["name"], "sales");
    assert_eq!(c1["role"]["instructions"], "Be brief and friendly.");
    assert_eq!(c1["role"]["version"], 1);
    let v1 = c1["version"].as_str().unwrap().to_string();
    assert_ne!(v1, v0);
    // The console lists it, and gives roles by the agent's name too.
    let (adk, _) = s.hub.create_admin_key("t", "developer").await.unwrap();
    let (_, _, l) = s
        .req("GET", "/admin/v1/app/agents", Some(&adk), None, &[])
        .await;
    let row = l["users"]
        .as_array()
        .unwrap()
        .iter()
        .find(|u| u["id"] == helper.name.as_str())
        .cloned()
        .unwrap();
    assert_eq!(row["role"], "sales", "{l}");
    let (st, _, v) = s
        .req(
            "PUT",
            &format!("/admin/v1/app/agents/{}/role", helper.name),
            Some(&adk),
            Some(("application/json", json!({"role": "sales"}).to_string())),
            &[],
        )
        .await;
    assert_eq!((st, &v["changed"]), (200, &json!(false)), "{v}");
    let (st, _, _) = s
        .req(
            "PUT",
            "/admin/v1/app/agents/nobody/role",
            Some(&adk),
            Some(("application/json", json!({"role": "sales"}).to_string())),
            &[],
        )
        .await;
    assert_eq!(st, 404);
    // The same role again: nothing changes, nobody is told.
    let (_, v) = s
        .api(
            "PUT",
            &format!("/v1/agents/{}/role", helper.name),
            Some(json!({"role": "sales"})),
        )
        .await;
    assert_eq!(v["changed"], false);
    quiet(&mut ws, |f| f["type"] == "config.updated").await;
    // Over HTTP with the agent's key: the same, with an ETag.
    let (st, h, v) = s
        .req("GET", "/api/agent/config", Some(&key), None, &[])
        .await;
    assert_eq!((st, v["version"].as_str()), (200, Some(v1.as_str())));
    let tag = h["etag"].to_str().unwrap().to_string();
    assert_eq!(tag, format!("\"{v1}\""));
    let (st, _, _) = s
        .req(
            "GET",
            "/api/agent/config",
            Some(&key),
            None,
            &[("if-none-match", &tag)],
        )
        .await;
    assert_eq!(st, 304);
    let (st, _, _) = s
        .req(
            "GET",
            "/api/agent/config",
            Some(&key),
            None,
            &[("if-none-match", "\"old\"")],
        )
        .await;
    assert_eq!(st, 200);
    let (st, _, _) = s.req("GET", "/api/agent/config", None, None, &[]).await;
    assert_eq!(st, 401);
    let (st, _, _) = s
        .req("GET", "/api/agent/config", Some("nope"), None, &[])
        .await;
    assert_eq!(st, 401);
    let alice_token = client_token(&s.hub, &alice).await;
    // A person's token is not an agent key (people have no configuration;
    // over WebSocket that is `config.agents_only`).
    let (st, _, _) = s
        .req("GET", "/api/agent/config", Some(&alice_token), None, &[])
        .await;
    assert_eq!(st, 401);
    // Over MCP.
    let meta = json!({"io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientInfo": {"name": "t", "version": "1"}, "io.modelcontextprotocol/clientCapabilities": {}});
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "get_config", "arguments": {}, "_meta": meta}});
    let (st, _, v) = s
        .req(
            "POST",
            "/mcp",
            Some(&key),
            Some(("application/json", body.to_string())),
            &[
                ("mcp-protocol-version", "2026-07-28"),
                ("mcp-method", "tools/call"),
                ("mcp-name", "get_config"),
                ("accept", "application/json, text/event-stream"),
            ],
        )
        .await;
    assert_eq!(st, 200, "{v}");
    let sc = &v["result"]["structuredContent"];
    assert_eq!(sc["version"].as_str(), Some(v1.as_str()), "{v}");
    assert!(
        v["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Be brief and friendly.")
    );
    // The role edited: its agents are told; the version moves.
    s.put_md("sales", &SALES.replace("friendly", "kind")).await;
    next(&mut ws, |f| f["type"] == "config.updated").await;
    let c2 = ws_call(&mut ws, 2, json!({"op": "get_config"})).await;
    assert_eq!(c2["role"]["instructions"], "Be brief and kind.");
    assert_ne!(c2["version"], json!(v1));
    // A new login gets the current one.
    let (_ws2, w2) = s.ws(&key).await;
    assert_eq!(w2["config"]["version"], c2["version"]);
    // Taken away: told; no role.
    s.api(
        "PUT",
        &format!("/v1/agents/{}/role", helper.name),
        Some(json!({"role": null})),
    )
    .await;
    next(&mut ws, |f| f["type"] == "config.updated").await;
    let c3 = ws_call(&mut ws, 3, json!({"op": "get_config"})).await;
    assert_eq!(c3["role"], Value::Null);
    assert_eq!(
        c3["version"],
        json!(v0),
        "the same content, the same version"
    );
    // Unknown agents and roles; people are not agents.
    let before = s.trace().await;
    let (st, v) = s
        .api(
            "PUT",
            "/v1/agents/nobody/role",
            Some(json!({"role": "sales"})),
        )
        .await;
    assert_eq!(st, 404, "{v}");
    let (st, v) = s
        .api(
            "PUT",
            &format!("/v1/agents/{}/role", alice.name),
            Some(json!({"role": "sales"})),
        )
        .await;
    assert_eq!(st, 404, "{v}");
    let (st, v) = s
        .api(
            "PUT",
            &format!("/v1/agents/{}/role", helper.name),
            Some(json!({"role": "nope"})),
        )
        .await;
    assert_eq!((st, v["rule"].as_str()), (404, Some("role.unknown")), "{v}");
    let (st, v) = s
        .api(
            "PUT",
            &format!("/v1/agents/{}/role", helper.name),
            Some(json!({"role": "Bad Name"})),
        )
        .await;
    assert_eq!((st, v["rule"].as_str()), (400, Some("role.name")), "{v}");
    let (st, _) = s
        .api(
            "PUT",
            &format!("/v1/agents/{}/role", helper.name),
            Some(json!({"rol": "x"})),
        )
        .await;
    assert_eq!(st, 400);
    assert_eq!(s.trace().await, before);
    quiet(&mut ws, |f| f["type"] == "config.updated").await;
}

async fn owners_give_roles_from_their_client(db: TestDb) {
    let s = Srv::start(&db).await;
    let alice = user(&s.hub, "alice", UserKind::Human, None).await;
    let bob = user(&s.hub, "bob", UserKind::Human, None).await;
    let (helper, _) = s.agent_with_key(&alice, "helper").await;
    s.put_md("sales", SALES).await;
    let a = Conn::new(&s.hub, &alice);
    let mut h = Conn::new(&s.hub, &helper);
    let r = a.ok(json!({"op": "list_roles"})).await;
    assert_eq!(r["roles"][0]["name"], "sales");
    let before = s.trace().await;
    for (who, msg, code) in [
        (
            &bob,
            json!({"op": "set_agent_role", "agent_id": helper.id, "role": "sales"}),
            "forbidden",
        ),
        (
            &alice,
            json!({"op": "set_agent_role", "agent_id": "nobody", "role": "sales"}),
            "not_found",
        ),
        (
            &alice,
            json!({"op": "set_agent_role", "agent_id": helper.id, "role": "nope"}),
            "not_found",
        ),
        (
            &alice,
            json!({"op": "set_agent_role", "agent_id": helper.id, "role": "-"}),
            "bad_request",
        ),
        (
            &helper,
            json!({"op": "set_agent_role", "agent_id": helper.id, "role": "sales"}),
            "forbidden",
        ),
        (&helper, json!({"op": "list_roles"}), "forbidden"),
        (&alice, json!({"op": "get_config"}), "forbidden"),
    ] {
        let r = Conn::new(&s.hub, who).call(msg.clone()).await;
        assert_eq!(r["error"]["code"], code, "{msg}: {r}");
    }
    assert_eq!(s.trace().await, before);
    h.drain();
    a.ok(json!({"op": "set_agent_role", "agent_id": helper.id, "role": "sales"}))
        .await;
    assert!(h.drain().iter().any(|f| f["type"] == "config.updated"));
    let c = h.ok(json!({"op": "get_config"})).await;
    assert_eq!(c["role"]["name"], "sales");
    // Audited once per change.
    a.ok(json!({"op": "set_agent_role", "agent_id": helper.id, "role": "sales"}))
        .await;
    let n = s
        .hub
        .audit_log(10_000)
        .await
        .unwrap()
        .iter()
        .filter(|x| x["action"] == "agent.role")
        .count();
    assert_eq!(n, 1);
    // A destroyed agent: gone.
    a.ok(json!({"op": "destroy_agent", "agent_id": helper.id, "confirm": helper.name}))
        .await;
    let r = a
        .call(json!({"op": "set_agent_role", "agent_id": helper.id, "role": null}))
        .await;
    assert_eq!(r["error"]["code"], "not_found", "{r}");
}

async fn roles_in_use_cannot_be_deleted(db: TestDb) {
    let s = Srv::start(&db).await;
    let alice = user(&s.hub, "alice", UserKind::Human, None).await;
    let (helper, _) = s.agent_with_key(&alice, "helper").await;
    let (other, _) = s.agent_with_key(&alice, "other").await;
    s.put_md("sales", SALES).await;
    for a in [&helper, &other] {
        s.hub.set_agent_role(&a.id, Some("sales")).await.unwrap();
    }
    let before = s.trace().await;
    let (st, v) = s.api("DELETE", "/v1/roles/sales", None).await;
    assert_eq!((st, v["rule"].as_str()), (409, Some("role.in_use")), "{v}");
    assert!(v["detail"].as_str().unwrap().contains("2 agent"));
    assert_eq!(s.trace().await, before);
    s.hub.set_agent_role(&helper.id, None).await.unwrap();
    // A destroyed agent does not hold a role.
    Conn::new(&s.hub, &alice)
        .ok(json!({"op": "destroy_agent", "agent_id": other.id, "confirm": other.name}))
        .await;
    let (st, v) = s.api("DELETE", "/v1/roles/sales", None).await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(s.hub.roles().await.unwrap(), Vec::<Value>::new());
}

async fn roles_survive_a_restart(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let r = Conn::new(&hub, &alice)
        .ok(json!({"op": "create_agent", "name": "helper"}))
        .await;
    let id = r["agent"]["id"].as_str().unwrap().to_string();
    hub.put_role("sales", SALES).await.unwrap();
    hub.set_agent_role(&id, Some("sales")).await.unwrap();
    let before = hub.agent_config(&hub.user(&id).unwrap()).await.unwrap();
    hub.close().await;
    drop(hub);
    let hub = open(single(&db.url)).await;
    let after = hub.agent_config(&hub.user(&id).unwrap()).await.unwrap();
    assert_eq!(after, before);
    assert_eq!(after["role"]["instructions"], "Be brief and friendly.");
    assert_eq!(hub.role("sales").await.unwrap()["markdown"], SALES);
}

/// Assigning a role while it is being deleted: either the assignment wins
/// (the role stays, `role.in_use`) or the deletion does (the assignment is
/// `role.unknown`); never an agent with a role that is gone.
async fn race_role_assignment_against_deletion(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let mut agents = vec![];
    for i in 0..4 {
        let r = Conn::new(&hub, &alice)
            .ok(json!({"op": "create_agent", "name": format!("a{i}")}))
            .await;
        agents.push(r["agent"]["id"].as_str().unwrap().to_string());
    }
    let (mut deleted, mut kept) = (0, 0);
    for round in 0..20 {
        for a in &agents {
            hub.set_agent_role(a, None).await.unwrap();
        }
        hub.put_role("sales", SALES).await.unwrap();
        // Who starts first alternates, so both orders happen.
        let h = hub.clone();
        let del_first = round % 2 == 0;
        let start_del = move || {
            let h = h.clone();
            tokio::spawn(
                async move { h.delete_role("sales").await.map(|_| ()).map_err(|e| e.rule) },
            )
        };
        let early = del_first.then(&start_del);
        let mut tasks = vec![];
        for a in agents.clone() {
            let h = hub.clone();
            tasks.push(tokio::spawn(async move {
                h.set_agent_role(&a, Some("sales"))
                    .await
                    .map(|_| ())
                    .map_err(|e| e.rule)
            }));
        }
        let del = match early {
            Some(d) => d,
            None => start_del(),
        };
        let mut results = vec![];
        for t in tasks {
            results.push(t.await.unwrap());
        }
        let del = del.await.unwrap();
        let exists = hub.role("sales").await.is_ok();
        let mut with = 0;
        for a in &agents {
            let c = hub.agent_config(&hub.user(a).unwrap()).await.unwrap();
            if c["role"]["name"] == "sales" {
                with += 1;
                assert!(exists, "round {round}: {a} has a deleted role");
            }
        }
        match del {
            Ok(()) => {
                deleted += 1;
                assert!(!exists);
                assert_eq!(with, 0);
                assert!(
                    results.iter().all(|r| r.is_err()),
                    "assigned after deletion: {results:?}"
                );
            }
            Err(r) => {
                kept += 1;
                assert_eq!(r, Some("role.in_use"), "round {round}");
                assert!(exists && with > 0);
                let ok = results.iter().filter(|r| r.is_ok()).count();
                assert_eq!(ok, with, "round {round}");
            }
        }
        if exists {
            for a in &agents {
                hub.set_agent_role(a, None).await.unwrap();
            }
            hub.delete_role("sales").await.unwrap();
        }
    }
    eprintln!("deleted first {deleted}, assigned first {kept}");
    assert!(deleted > 0 && kept > 0, "both orders: {deleted} {kept}");
}
