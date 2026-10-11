//! MCP endpoint `/mcp` (docs/10-agent-ecosystem.md §5.1) on a real server:
//! both protocol eras and their header rules, authentication (agent keys
//! only), every tool with the business rules of the client protocol,
//! updates delivered once per key also under concurrent calls, rate limits
//! that leave no trace, and restart.

mod common;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::Duration;

use agentim_core::config::Config;
use agentim_core::hub::Hub;
use agentim_core::model::*;
use agentim_core::server::{AppState, router};
use common::*;
use serde_json::{Value, json};

backend_tests!(
    discovery_versions_and_headers,
    only_agent_keys_get_in,
    tools_follow_the_business_rules,
    updates_arrive_once_in_order,
    rate_limits_leave_no_trace,
    keys_and_positions_survive_a_restart,
    race_concurrent_updates_deliver_each_event_once,
);

const V: &str = "2026-07-28";

struct Mcp {
    url: String,
    hub: Hub,
    http: reqwest::Client,
}

async fn serve(cfg: Config) -> Mcp {
    let hub = Hub::open(cfg, hub_cfg()).await.unwrap();
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
    Mcp {
        url: format!("http://{addr}/mcp"),
        hub,
        http: reqwest::Client::new(),
    }
}

fn meta() -> Value {
    json!({"io.modelcontextprotocol/protocolVersion": V, "io.modelcontextprotocol/clientInfo": {"name": "test", "version": "1"}, "io.modelcontextprotocol/clientCapabilities": {}})
}

impl Mcp {
    /// A raw POST: status and JSON body (Null when empty).
    async fn post(&self, headers: &[(&str, &str)], body: &str) -> (u16, Value) {
        let mut r = self
            .http
            .post(&self.url)
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream");
        for (k, v) in headers {
            r = r.header(*k, *v);
        }
        let res = r.body(body.to_string()).send().await.unwrap();
        let status = res.status().as_u16();
        let text = res.text().await.unwrap();
        (status, serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    /// A modern request with correct headers.
    async fn rpc(&self, key: &str, method: &str, mut params: Value) -> (u16, Value) {
        params["_meta"] = meta();
        let name = params["name"].as_str().map(String::from);
        let auth = format!("Bearer {key}");
        let mut h = vec![
            ("authorization", auth.as_str()),
            ("mcp-protocol-version", V),
            ("mcp-method", method),
        ];
        if let Some(n) = &name {
            h.push(("mcp-name", n.as_str()));
        }
        let body = json!({"jsonrpc": "2.0", "id": 7, "method": method, "params": params});
        self.post(&h, &body.to_string()).await
    }

    /// A tool call; returns the result object.
    async fn tool(&self, key: &str, name: &str, args: Value) -> Value {
        let (st, v) = self
            .rpc(key, "tools/call", json!({"name": name, "arguments": args}))
            .await;
        assert_eq!(st, 200, "{name}: {v}");
        assert_eq!(v["result"]["resultType"], "complete", "{v}");
        v["result"].clone()
    }

    async fn ok(&self, key: &str, name: &str, args: Value) -> Value {
        let r = self.tool(key, name, args.clone()).await;
        assert_eq!(r["isError"], false, "{name} {args}: {r}");
        r["structuredContent"].clone()
    }

    async fn tool_err(&self, key: &str, name: &str, args: Value) -> String {
        let r = self.tool(key, name, args.clone()).await;
        assert_eq!(r["isError"], true, "{name} {args}: {r}");
        let c = r["structuredContent"]["error"]["code"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(r["content"][0]["text"].as_str().unwrap().starts_with(&c));
        c
    }
}

/// alice (a person), her agent `helper` with a key, and a group of both.
async fn setup(m: &Mcp) -> (User, User, String, String) {
    let alice = user(&m.hub, "alice", UserKind::Human, None).await;
    let r = m.hub.create_agent(&alice, "helper").await.unwrap();
    let key = r["key"].as_str().unwrap().to_string();
    let helper = m.hub.user_by_name("helper").unwrap();
    let conv = Conn::new(&m.hub, &alice).group(&[&helper]).await;
    (alice, helper, key, conv)
}

fn seqs_of(updates: &Value, conv: &str) -> Vec<i64> {
    updates["convs"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["conv_id"] == conv)
        .flat_map(|c| c["events"].as_array().unwrap().clone())
        .map(|e| e["event"]["seq"].as_i64().unwrap())
        .collect()
}

async fn discovery_versions_and_headers(db: TestDb) {
    let m = serve(single(&db.url)).await;
    let (_, _, key, _) = setup(&m).await;
    let auth = format!("Bearer {key}");
    let a = auth.as_str();

    // Modern discovery.
    let (st, v) = m.rpc(&key, "server/discover", json!({})).await;
    assert_eq!(st, 200);
    let r = &v["result"];
    assert_eq!(v["id"], 7);
    assert_eq!(r["resultType"], "complete");
    assert_eq!(
        r["supportedVersions"],
        json!(["2026-07-28", "2025-11-25", "2025-06-18", "2025-03-26"])
    );
    assert!(r["capabilities"]["tools"].is_object());
    assert_eq!(
        r["_meta"]["io.modelcontextprotocol/serverInfo"]["name"],
        edition().binary()
    );
    assert!(r["instructions"].as_str().unwrap().contains("get_updates"));
    let (_, v) = m.rpc(&key, "ping", json!({})).await;
    assert_eq!(v["result"], json!({"resultType": "complete"}));

    // tools/list: the documented tools, in a fixed order, valid schemas.
    let (_, v) = m.rpc(&key, "tools/list", json!({})).await;
    let tools = v["result"]["tools"].as_array().unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names, agentim_core::mcp::TOOLS);
    for t in tools {
        assert_eq!(t["inputSchema"]["type"], "object", "{t}");
        assert!(t["description"].as_str().unwrap().len() > 10);
    }
    assert_eq!(v["result"]["ttlMs"], 300_000);
    let (_, again) = m.rpc(&key, "tools/list", json!({})).await;
    assert_eq!(again, v);

    let body = |method: &str, params: Value| {
        json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).to_string()
    };
    let disc = body("server/discover", json!({"_meta": meta()}));
    // Version header missing, different from _meta, or unsupported.
    let (st, v) = m
        .post(
            &[("authorization", a), ("mcp-method", "server/discover")],
            &disc,
        )
        .await;
    assert_eq!((st, v["error"]["code"].clone()), (400, json!(-32020)));
    let (st, v) = m
        .post(
            &[
                ("authorization", a),
                ("mcp-protocol-version", "2025-11-25"),
                ("mcp-method", "server/discover"),
            ],
            &disc,
        )
        .await;
    assert_eq!((st, v["error"]["code"].clone()), (400, json!(-32020)));
    let mut old = meta();
    old["io.modelcontextprotocol/protocolVersion"] = json!("1900-01-01");
    let (st, v) = m
        .post(
            &[
                ("authorization", a),
                ("mcp-protocol-version", "1900-01-01"),
                ("mcp-method", "server/discover"),
            ],
            &body("server/discover", json!({"_meta": old})),
        )
        .await;
    assert_eq!((st, v["error"]["code"].clone()), (400, json!(-32022)));
    assert_eq!(v["error"]["data"]["requested"], "1900-01-01");
    assert_eq!(v["error"]["data"]["supported"][0], V);
    // Mcp-Method and Mcp-Name must match the body.
    let (st, v) = m
        .post(
            &[
                ("authorization", a),
                ("mcp-protocol-version", V),
                ("mcp-method", "tools/list"),
            ],
            &disc,
        )
        .await;
    assert_eq!((st, v["error"]["code"].clone()), (400, json!(-32020)));
    let call = body(
        "tools/call",
        json!({"name": "whoami", "arguments": {}, "_meta": meta()}),
    );
    for name in [None, Some("find_user"), Some("=?base64?bm9wZQ==?=")] {
        let mut h = vec![
            ("authorization", a),
            ("mcp-protocol-version", V),
            ("mcp-method", "tools/call"),
        ];
        if let Some(n) = name {
            h.push(("mcp-name", n));
        }
        let (st, v) = m.post(&h, &call).await;
        assert_eq!(
            (st, v["error"]["code"].clone()),
            (400, json!(-32020)),
            "{name:?}"
        );
    }
    // The base64 form of the right name is accepted.
    let (st, v) = m
        .post(
            &[
                ("authorization", a),
                ("mcp-protocol-version", V),
                ("mcp-method", "tools/call"),
                ("mcp-name", "=?base64?d2hvYW1p?="),
            ],
            &call,
        )
        .await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(v["result"]["isError"], false);
    // A modern version header without _meta.
    let (st, v) = m
        .post(
            &[
                ("authorization", a),
                ("mcp-protocol-version", V),
                ("mcp-method", "ping"),
            ],
            &body("ping", json!({})),
        )
        .await;
    assert_eq!((st, v["error"]["code"].clone()), (400, json!(-32020)));
    // Unknown method: 404 for modern requests.
    let (st, v) = m.rpc(&key, "resources/list", json!({})).await;
    assert_eq!((st, v["error"]["code"].clone()), (404, json!(-32601)));

    // Legacy handshake without sessions.
    let init = |v: &str| {
        body(
            "initialize",
            json!({"protocolVersion": v, "capabilities": {}, "clientInfo": {"name": "old", "version": "1"}}),
        )
    };
    let (st, v) = m.post(&[("authorization", a)], &init("2025-06-18")).await;
    assert_eq!(st, 200);
    assert_eq!(v["result"]["protocolVersion"], "2025-06-18");
    assert!(v["result"].get("resultType").is_none());
    assert_eq!(v["result"]["serverInfo"]["name"], edition().binary());
    let (_, v) = m.post(&[("authorization", a)], &init("2024-01-01")).await;
    assert_eq!(v["result"]["protocolVersion"], "2025-11-25");
    let res = m
        .http
        .post(&m.url)
        .header("authorization", a)
        .header("content-type", "application/json")
        .body(init("2025-03-26"))
        .send()
        .await
        .unwrap();
    assert!(res.headers().get("mcp-session-id").is_none());
    // initialized notification and other notifications: 202, no body.
    let (st, v) = m
        .post(
            &[("authorization", a)],
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        )
        .await;
    assert_eq!((st, v), (202, Value::Null));
    // Legacy requests: no header (2025-03-26) or a legacy header.
    for h in [None, Some("2025-06-18"), Some("2025-11-25")] {
        let mut hs = vec![("authorization", a)];
        if let Some(x) = h {
            hs.push(("mcp-protocol-version", x));
        }
        let (st, v) = m
            .post(
                &hs,
                &body("tools/call", json!({"name": "whoami", "arguments": {}})),
            )
            .await;
        assert_eq!(st, 200, "{h:?}: {v}");
        assert_eq!(v["result"]["isError"], false);
        assert!(v["result"].get("resultType").is_none());
    }
    let (st, v) = m
        .post(
            &[("authorization", a), ("mcp-protocol-version", "2024-11-05")],
            &body("tools/list", json!({})),
        )
        .await;
    assert_eq!((st, v["error"]["code"].clone()), (400, json!(-32022)));
    // Legacy unknown method: a JSON-RPC error.
    let (st, v) = m
        .post(&[("authorization", a)], &body("prompts/list", json!({})))
        .await;
    assert_eq!((st, v["error"]["code"].clone()), (200, json!(-32601)));

    // Malformed requests.
    for (b, code) in [
        ("{not json", -32700),
        (r#"[{"jsonrpc":"2.0","id":1,"method":"ping"}]"#, -32600),
        (r#"{"jsonrpc":"2.0","id":1}"#, -32600),
        (r#"{"jsonrpc":"1.0","id":1,"method":"ping"}"#, -32600),
        (r#"{"jsonrpc":"2.0","id":{"x":1},"method":"ping"}"#, -32600),
        (
            r#"{"jsonrpc":"2.0","id":1,"method":"ping","params":[1]}"#,
            -32602,
        ),
    ] {
        let (st, v) = m.post(&[("authorization", a)], b).await;
        assert_eq!((st, v["error"]["code"].clone()), (400, json!(code)), "{b}");
    }
    // Other HTTP methods.
    for method in [reqwest::Method::GET, reqwest::Method::DELETE] {
        let res = m
            .http
            .request(method, &m.url)
            .header("authorization", a)
            .send()
            .await
            .unwrap();
        assert_eq!(res.status().as_u16(), 405);
    }
    // Unknown tool and malformed arguments are protocol errors.
    let (st, v) = m
        .rpc(&key, "tools/call", json!({"name": "nope", "arguments": {}}))
        .await;
    assert_eq!((st, v["error"]["code"].clone()), (200, json!(-32602)));
    let (_, v) = m
        .rpc(
            &key,
            "tools/call",
            json!({"name": "whoami", "arguments": [1]}),
        )
        .await;
    assert_eq!(v["error"]["code"], -32602);
}

async fn only_agent_keys_get_in(db: TestDb) {
    let m = serve(single(&db.url)).await;
    let (alice, helper, key, _) = setup(&m).await;
    let disc = json!({"jsonrpc": "2.0", "id": 1, "method": "server/discover", "params": {"_meta": meta()}}).to_string();
    let hdr = |auth: String| {
        vec![
            ("authorization".to_string(), auth),
            ("mcp-protocol-version".into(), V.into()),
            ("mcp-method".into(), "server/discover".into()),
        ]
    };
    let send = |h: Vec<(String, String)>| {
        let disc = disc.clone();
        let m = &m;
        async move {
            let hs: Vec<(&str, &str)> = h.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
            m.post(&hs, &disc).await
        }
    };
    // No key, a wrong key, a non-bearer header.
    let res = m.http.post(&m.url).body(disc.clone()).send().await.unwrap();
    assert_eq!(res.status().as_u16(), 401);
    assert_eq!(res.headers()["www-authenticate"], "Bearer");
    for bad in ["Bearer atk_wrong", "Basic abc", "Bearer "] {
        tokio::time::sleep(Duration::from_millis(1100)).await;
        assert_eq!(send(hdr(bad.into())).await.0, 401, "{bad}");
    }
    // A person's token: 403 (what arrives here is labelled as AI).
    let (human_key, _) = m
        .hub
        .issue_token(&alice.id, None, None, None, "t")
        .await
        .unwrap();
    let (st, v) = send(hdr(format!("Bearer {human_key}"))).await;
    assert_eq!(st, 403, "{v}");
    // The agent's key works; other sites may not use it from a browser.
    assert_eq!(send(hdr(format!("Bearer {key}"))).await.0, 200);
    let mut h = hdr(format!("Bearer {key}"));
    h.push(("origin".into(), "https://evil.example".into()));
    assert_eq!(send(h).await.0, 403);
    let mut h = hdr(format!("Bearer {key}"));
    h.push((
        "origin".into(),
        format!(
            "http://{}",
            m.url.trim_start_matches("http://").trim_end_matches("/mcp")
        ),
    ));
    assert_eq!(send(h).await.0, 200);
    // Revoked or deactivated: 401.
    let r = m.hub.agent_key(&alice, &helper.id, false).await.unwrap();
    let key2 = r["key"].as_str().unwrap().to_string();
    m.hub
        .revoke_agent_key(&alice, &helper.id, r["device_id"].as_str().unwrap())
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(1100)).await;
    assert_eq!(send(hdr(format!("Bearer {key2}"))).await.0, 401);
    m.hub.set_deactivated(&helper.id, true).await.unwrap();
    tokio::time::sleep(Duration::from_millis(1100)).await;
    assert_eq!(send(hdr(format!("Bearer {key}"))).await.0, 401);
    // Turned off: not there at all.
    let mut cfg = single(&db.url);
    cfg.mcp.enabled = false;
    let off = serve(cfg).await;
    let res = off
        .http
        .post(&off.url)
        .body(disc.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(res.status().as_u16(), 404);
}

async fn tools_follow_the_business_rules(db: TestDb) {
    let mut cfg = single(&db.url);
    cfg.mcp.burst = 1000.0;
    let m = serve(cfg).await;
    let (alice, helper, key, conv) = setup(&m).await;
    let a = Conn::new(&m.hub, &alice);

    let w = m.ok(&key, "whoami", json!({})).await;
    assert_eq!(w["agent"]["id"], json!(helper.id));
    assert_eq!(w["owner"]["name"], "alice");
    assert_eq!(w["server"]["server"]["edition"], edition().as_str());
    let r = m.tool(&key, "whoami", json!({})).await;
    assert!(
        r["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("owned by alice")
    );

    let l = m.ok(&key, "list_conversations", json!({})).await;
    assert_eq!(l["convs"][0]["conv_id"], json!(conv));

    // Send: labelled as the agent, idempotent with client_msg_id.
    let s1 = m
        .ok(
            &key,
            "send_message",
            json!({"conv_id": conv, "text": "hello", "client_msg_id": "m1"}),
        )
        .await;
    let s2 = m
        .ok(
            &key,
            "send_message",
            json!({"conv_id": conv, "text": "hello again", "client_msg_id": "m1"}),
        )
        .await;
    assert_eq!(s1["msg_id"], s2["msg_id"]);
    assert_eq!(s1["seq"], s2["seq"]);
    let msg_id = s1["msg_id"].as_str().unwrap().to_string();
    let log = a.pull(&conv, 0).await;
    let m1 = msg_by_id(&log, &msg_id).unwrap();
    assert_eq!(m1["sender_type"], "agent");
    assert_eq!(m1["content"], "hello");
    // Without client_msg_id each call is a new message; reply_to works.
    let s3 = m
        .ok(
            &key,
            "send_message",
            json!({"conv_id": conv, "text": "answer", "reply_to": msg_id}),
        )
        .await;
    let s4 = m
        .ok(
            &key,
            "send_message",
            json!({"conv_id": conv, "text": "answer"}),
        )
        .await;
    assert_ne!(s3["msg_id"], s4["msg_id"]);
    let log = a.pull(&conv, 0).await;
    assert_eq!(
        msg_by_id(&log, s3["msg_id"].as_str().unwrap()).unwrap()["reply_to"],
        json!(msg_id)
    );

    // Edit and recall own messages.
    m.ok(
        &key,
        "edit_message",
        json!({"msg_id": msg_id, "text": "hello!"}),
    )
    .await;
    let log = a.pull(&conv, 0).await;
    assert_eq!(msg_by_id(&log, &msg_id).unwrap()["content"], "hello!");
    m.ok(&key, "recall_message", json!({"msg_id": s4["msg_id"]}))
        .await;

    // find_user.
    let f = m.ok(&key, "find_user", json!({"name": "alice"})).await;
    assert_eq!(f["user"]["id"], json!(alice.id));

    // Conversations: contacts only, the person owns the groups.
    let d = m.ok(&key, "open_direct", json!({"user": "alice"})).await;
    let d2 = m.ok(&key, "open_direct", json!({"user": alice.id})).await;
    assert_eq!(d["conv_id"], d2["conv_id"]);
    let g = m
        .ok(
            &key,
            "create_group",
            json!({"title": "plans", "members": []}),
        )
        .await;
    let gid = g["conv_id"].as_str().unwrap().to_string();
    let roles: Vec<(String, String)> = m.hub.conv_detail(&gid).await.unwrap()["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| {
            (
                x["user"].as_str().unwrap().into(),
                x["role"].as_str().unwrap().into(),
            )
        })
        .collect();
    assert!(
        roles.contains(&("alice".into(), "owner".into())),
        "{roles:?}"
    );
    assert!(
        roles.contains(&("helper".into(), "admin".into())),
        "{roles:?}"
    );
    let helper2 = user(&m.hub, "helper2", UserKind::Agent, Some(&alice)).await;
    m.ok(
        &key,
        "add_member",
        json!({"conv_id": gid, "user": "helper2", "history": 0}),
    )
    .await;
    let _ = helper2;

    // Business refusals are tool errors with the protocol's codes, and leave
    // nothing in the log.
    let head = seqs(&a.pull(&conv, 0).await).last().copied().unwrap();
    let human_msg = a.send(&conv, "mine").await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    let head = head + 1;
    let bob = user(&m.hub, "bob", UserKind::Human, None).await;
    let bc = Conn::new(&m.hub, &bob);
    let other = bc.group(&[]).await;
    let long = "x".repeat(70_000);
    let cases = [
        (
            "send_message",
            json!({"conv_id": other, "text": "x"}),
            "forbidden",
        ),
        (
            "send_message",
            json!({"conv_id": "c_nope", "text": "x"}),
            "not_found",
        ),
        (
            "send_message",
            json!({"conv_id": conv, "text": long}),
            "bad_request",
        ),
        ("send_message", json!({"conv_id": conv}), "bad_request"),
        (
            "send_message",
            json!({"conv_id": conv, "text": ""}),
            "bad_request",
        ),
        (
            "send_message",
            json!({"conv_id": conv, "text": 5}),
            "bad_request",
        ),
        (
            "send_message",
            json!({"conv_id": conv, "text": "x", "colour": "red"}),
            "bad_request",
        ),
        (
            "send_message",
            json!({"conv_id": conv, "text": "x", "client_msg_id": "y".repeat(129)}),
            "bad_request",
        ),
        (
            "edit_message",
            json!({"msg_id": human_msg, "text": "x"}),
            "forbidden",
        ),
        (
            "edit_message",
            json!({"msg_id": "m_nope", "text": "x"}),
            "not_found",
        ),
        ("recall_message", json!({"msg_id": human_msg}), "forbidden"),
        (
            "recall_message",
            json!({"msg_id": s4["msg_id"]}),
            "conflict",
        ),
        ("find_user", json!({"name": "nobody"}), "not_found"),
        ("whoami", json!({"x": 1}), "bad_request"),
        ("get_updates", json!({"limit": 0}), "bad_request"),
        ("get_updates", json!({"limit": 501}), "bad_request"),
        ("get_updates", json!({"limit": "ten"}), "bad_request"),
        ("open_direct", json!({"user": "bob"}), "forbidden"),
        ("open_direct", json!({"user": "nobody"}), "not_found"),
        ("open_direct", json!({}), "bad_request"),
        (
            "create_group",
            json!({"title": "x", "members": ["bob"]}),
            "forbidden",
        ),
        (
            "create_group",
            json!({"title": "x", "members": "bob"}),
            "bad_request",
        ),
        (
            "create_group",
            json!({"title": "x", "members": [""]}),
            "bad_request",
        ),
        (
            "create_group",
            json!({"title": "", "members": []}),
            "bad_request",
        ),
        (
            "add_member",
            json!({"conv_id": gid, "user": "bob"}),
            "forbidden",
        ),
        (
            "add_member",
            json!({"conv_id": gid, "user": "alice"}),
            "conflict",
        ),
        (
            "add_member",
            json!({"conv_id": gid, "user": "helper2", "history": "all"}),
            "bad_request",
        ),
        (
            "add_member",
            json!({"conv_id": other, "user": "alice"}),
            "forbidden",
        ),
    ];
    for (tool, args, want) in cases {
        assert_eq!(
            m.tool_err(&key, tool, args.clone()).await,
            want,
            "{tool} {args}"
        );
    }
    let after = a.pull(&conv, 0).await;
    assert_eq!(
        seqs(&after).last().copied().unwrap(),
        head,
        "refusals wrote nothing"
    );
    let other_head = seqs(&bc.pull(&other, 0).await).len();
    assert!(other_head >= 1);
    assert!(!bc.pull(&other, 0).await.to_string().contains("\"x\""));

    // The owner's approval mode: a send becomes a draft.
    a.ok(
        json!({"op": "set_agent_policy", "agent_id": helper.id, "conv_id": conv, "approval": true}),
    )
    .await;
    let r = m
        .tool(
            &key,
            "send_message",
            json!({"conv_id": conv, "text": "needs approval"}),
        )
        .await;
    assert_eq!(r["isError"], false, "{r}");
    assert!(
        r["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("approval"),
        "{r}"
    );
    assert!(r["structuredContent"]["draft_id"].is_string());
    // Removed from the conversation: forbidden.
    a.ok(json!({"op": "remove_member", "conv_id": conv, "user_id": helper.id}))
        .await;
    assert_eq!(
        m.tool_err(&key, "send_message", json!({"conv_id": conv, "text": "x"}))
            .await,
        "forbidden"
    );
}

async fn updates_arrive_once_in_order(db: TestDb) {
    let m = serve(single(&db.url)).await;
    let (alice, helper, key, conv) = setup(&m).await;
    let a = Conn::new(&m.hub, &alice);
    // Mention mode: the agent sees what mentions it.
    for i in 0..5 {
        a.send(&conv, &format!("@helper {i}")).await;
    }
    a.send(&conv, "not for the agent").await;
    let u = m.ok(&key, "get_updates", json!({})).await;
    let got = seqs_of(&u, &conv);
    assert!(!got.is_empty());
    assert!(got.windows(2).all(|w| w[0] < w[1]));
    let texts: Vec<&str> = u["convs"][0]["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|e| e["event"]["msg"]["content"].as_str())
        .collect();
    assert_eq!(
        texts,
        vec![
            "@helper 0",
            "@helper 1",
            "@helper 2",
            "@helper 3",
            "@helper 4"
        ]
    );
    let r = m.tool(&key, "get_updates", json!({})).await;
    assert_eq!(r["content"][0]["text"], "Nothing new.");
    // Pages: limit 2 returns `more` and continues without repeats.
    for i in 5..10 {
        a.send(&conv, &format!("@helper {i}")).await;
    }
    let mut all = vec![];
    loop {
        let r = m.tool(&key, "get_updates", json!({"limit": 2})).await;
        let v = &r["structuredContent"];
        let page = seqs_of(v, &conv);
        assert!(page.len() <= 2);
        all.extend(page);
        if v["more"] != true {
            break;
        }
        assert!(
            r["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("call get_updates again")
        );
    }
    assert!(all.windows(2).all(|w| w[0] < w[1]), "{all:?}");
    assert_eq!(all.len(), 5);
    // The rendered text names conversation, seq, sender and kind.
    a.send(&conv, "@helper rendered").await;
    let r = m.tool(&key, "get_updates", json!({})).await;
    let text = r["content"][0]["text"].as_str().unwrap();
    assert!(
        text.contains(&format!("[{conv} #")) && text.contains("alice (human"),
        "{text}"
    );
    // A private note from the owner (whisper) arrives as a side event.
    a.ok(json!({"op": "whisper", "conv_id": conv, "agent_id": helper.id, "text": "be brief"}))
        .await;
    let r = m.tool(&key, "get_updates", json!({})).await;
    assert!(
        r["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("be brief"),
        "{r}"
    );
    // Each key is its own device with its own position.
    let r2 = m.hub.agent_key(&alice, &helper.id, false).await.unwrap();
    let key2 = r2["key"].as_str().unwrap();
    let u2 = m.ok(key2, "get_updates", json!({"limit": 500})).await;
    assert!(seqs_of(&u2, &conv).len() >= 11);
    // Removal: the key learns it left and gets nothing more.
    a.ok(json!({"op": "remove_member", "conv_id": conv, "user_id": helper.id}))
        .await;
    a.send(&conv, "@helper after removal").await;
    let r = m.tool(&key, "get_updates", json!({})).await;
    let v = &r["structuredContent"];
    assert!(!r.to_string().contains("after removal"), "{r}");
    assert!(
        v["convs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["left"] == true)
            || r["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("no longer"),
        "{r}"
    );
    // Told once: the next call does not repeat it.
    let r = m.tool(&key, "get_updates", json!({})).await;
    assert_eq!(r["content"][0]["text"], "Nothing new.", "{r}");
    // Dissolved: reported once.
    let conv2 = a.group(&[&helper]).await;
    a.send(&conv2, "@helper x").await;
    m.ok(&key, "get_updates", json!({})).await;
    a.ok(json!({"op": "dissolve", "conv_id": conv2})).await;
    let r = m.tool(&key, "get_updates", json!({})).await;
    assert!(
        r["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("dissolved"),
        "{r}"
    );
    let r = m.tool(&key, "get_updates", json!({})).await;
    assert!(
        !r["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("dissolved"),
        "{r}"
    );
}

async fn rate_limits_leave_no_trace(db: TestDb) {
    let mut cfg = single(&db.url);
    cfg.mcp.rate_per_second = 0.5;
    cfg.mcp.burst = 2.0;
    let m = serve(cfg).await;
    let (alice, _, key, conv) = setup(&m).await;
    m.ok(&key, "send_message", json!({"conv_id": conv, "text": "1"}))
        .await;
    m.ok(&key, "send_message", json!({"conv_id": conv, "text": "2"}))
        .await;
    assert_eq!(
        m.tool_err(&key, "send_message", json!({"conv_id": conv, "text": "3"}))
            .await,
        "rate_limited"
    );
    let log = Conn::new(&m.hub, &alice).pull(&conv, 0).await;
    let texts: Vec<Value> = log["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|e| e["event"]["msg"]["content"].as_str().map(|s| json!(s)))
        .collect();
    assert_eq!(texts, vec![json!("1"), json!("2")]);
    // Discovery and listing are not tool calls and not limited.
    assert_eq!(m.rpc(&key, "tools/list", json!({})).await.0, 200);
    // Refills over time.
    tokio::time::sleep(Duration::from_millis(2100)).await;
    m.ok(&key, "whoami", json!({})).await;
}

async fn keys_and_positions_survive_a_restart(db: TestDb) {
    let (key, conv, alice);
    {
        let m = serve(single(&db.url)).await;
        let s = setup(&m).await;
        alice = s.0;
        key = s.2;
        conv = s.3;
        Conn::new(&m.hub, &alice).send(&conv, "@helper one").await;
        let r = m.tool(&key, "get_updates", json!({})).await;
        assert!(
            r["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("@helper one")
        );
    }
    let m = serve(single(&db.url)).await;
    Conn::new(&m.hub, &alice).send(&conv, "@helper two").await;
    let u = m.ok(&key, "get_updates", json!({})).await;
    let texts: Vec<&str> = u["convs"][0]["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|e| e["event"]["msg"]["content"].as_str())
        .collect();
    assert_eq!(texts, vec!["@helper two"]);
}

/// Concurrent get_updates on one key while people send: every event is
/// handed out exactly once across all calls.
async fn race_concurrent_updates_deliver_each_event_once(db: TestDb) {
    let mut cfg = single(&db.url);
    cfg.mcp.rate_per_second = 10_000.0;
    cfg.mcp.burst = 10_000.0;
    let m = std::sync::Arc::new(serve(cfg).await);
    let (alice, _, key, conv) = setup(&m).await;
    for round in 0..5 {
        let sender = {
            let (hub, alice, conv) = (m.hub.clone(), alice.clone(), conv.clone());
            tokio::spawn(async move {
                let a = Conn::new(&hub, &alice);
                for i in 0..20 {
                    a.send(&conv, &format!("@helper r{round} {i}")).await;
                }
            })
        };
        let mut readers = vec![];
        for _ in 0..4 {
            let (m, key, conv) = (m.clone(), key.clone(), conv.clone());
            readers.push(tokio::spawn(async move {
                let mut got = vec![];
                for _ in 0..10 {
                    let u = m.ok(&key, "get_updates", json!({"limit": 7})).await;
                    got.extend(seqs_of(&u, &conv));
                }
                got
            }));
        }
        sender.await.unwrap();
        let mut seen: HashMap<i64, usize> = HashMap::new();
        for r in readers {
            for s in r.await.unwrap() {
                *seen.entry(s).or_default() += 1;
            }
        }
        // Drain the rest.
        loop {
            let u = m.ok(&key, "get_updates", json!({"limit": 500})).await;
            let s = seqs_of(&u, &conv);
            if s.is_empty() {
                break;
            }
            for x in s {
                *seen.entry(x).or_default() += 1;
            }
        }
        let dup: Vec<_> = seen.iter().filter(|(_, n)| **n > 1).collect();
        assert!(dup.is_empty(), "round {round}: delivered twice: {dup:?}");
        let log = Conn::new(&m.hub, &alice).pull(&conv, 0).await;
        let mine: Vec<i64> = log["events"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| {
                e["event"]["msg"]["content"]
                    .as_str()
                    .is_some_and(|t| t.starts_with(&format!("@helper r{round} ")))
            })
            .map(|e| e["event"]["seq"].as_i64().unwrap())
            .collect();
        assert_eq!(mine.len(), 20);
        for s in mine {
            assert_eq!(seen.get(&s), Some(&1), "round {round}: seq {s}");
        }
    }
}
