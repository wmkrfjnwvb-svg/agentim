//! Open API `/v1` (docs/07-open-api.md): authentication, scopes, address
//! rules, problem details, every endpoint as a user and as the system with
//! the business rules of the client protocol, batches, idempotency, rate
//! limits, audit, and the OpenAPI document matching the router. Each test
//! runs a real server on every backend.

mod common;

use std::net::SocketAddr;
use std::time::Duration;

use agentim_core::config::Config;
use agentim_core::hub::Hub;
use agentim_core::model::*;
use agentim_core::server::{AppState, router};
use common::*;
use serde_json::{Value, json};

backend_tests!(
    authentication_and_scopes,
    address_rules,
    users_lifecycle,
    tokens_for_users,
    conversations_as_users_and_the_system,
    messages_as_users_and_the_system,
    batches_report_each_item,
    idempotency_keys,
    rate_limits_per_key,
    errors_are_problem_details,
    input_limits,
);

struct Api {
    base: String,
    hub: Hub,
    http: reqwest::Client,
    key: String,
}

const ALL: &[&str] = agentim_core::open_api::SCOPES;

async fn serve(cfg: Config) -> (String, Hub) {
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
    (format!("http://{addr}"), hub)
}

impl Api {
    async fn start(db: &TestDb) -> Api {
        Api::with(single(&db.url)).await
    }

    async fn with(cfg: Config) -> Api {
        let (base, hub) = serve(cfg).await;
        let scopes: Vec<String> = ALL.iter().map(|s| s.to_string()).collect();
        let (key, _) = hub
            .create_api_key("test", &scopes, None, None)
            .await
            .unwrap();
        Api {
            base,
            hub,
            http: reqwest::Client::new(),
            key,
        }
    }

    async fn key(&self, scopes: &[&str]) -> String {
        let s: Vec<String> = scopes.iter().map(|s| s.to_string()).collect();
        self.hub
            .create_api_key("k", &s, None, None)
            .await
            .unwrap()
            .0
    }

    async fn raw(
        &self,
        method: &str,
        path: &str,
        key: Option<&str>,
        body: Option<Value>,
        headers: &[(&str, &str)],
    ) -> (u16, reqwest::header::HeaderMap, Value) {
        let mut r = self
            .http
            .request(method.parse().unwrap(), format!("{}{path}", self.base));
        if let Some(k) = key {
            r = r.header("authorization", format!("Bearer {k}"));
        }
        for (h, v) in headers {
            r = r.header(*h, *v);
        }
        if let Some(b) = body {
            r = r
                .header("content-type", "application/json")
                .body(b.to_string());
        }
        let resp = r.send().await.unwrap();
        let status = resp.status().as_u16();
        let h = resp.headers().clone();
        let text = resp.text().await.unwrap();
        (
            status,
            h,
            serde_json::from_str(&text).unwrap_or(Value::String(text)),
        )
    }

    async fn call(&self, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        let (s, _, v) = self.raw(method, path, Some(&self.key), body, &[]).await;
        (s, v)
    }

    async fn ok(&self, method: &str, path: &str, body: Option<Value>) -> Value {
        let (s, v) = self.call(method, path, body.clone()).await;
        assert!(
            (200..300).contains(&s),
            "{method} {path} {body:?} -> {s} {v}"
        );
        v
    }

    async fn user(&self, id: &str) -> User {
        self.ok("PUT", &format!("/v1/users/{id}"), Some(json!({})))
            .await;
        self.hub.user_by_name(id).unwrap()
    }
}

fn code(v: &Value) -> &str {
    v["code"].as_str().unwrap_or("")
}

async fn authentication_and_scopes(db: TestDb) {
    let api = Api::start(&db).await;
    // No key, wrong keys.
    for k in [
        None,
        Some("nope"),
        Some("ak_kx_nope"),
        Some("atk_looks_like_a_client_token"),
    ] {
        let (s, h, v) = api.raw("GET", "/v1/users/alice", k, None, &[]).await;
        assert_eq!((s, code(&v)), (401, "unauthorized"), "{k:?}");
        assert_eq!(h["www-authenticate"], "Bearer");
        assert_eq!(h["content-type"], "application/problem+json");
    }
    // A client token is not an API key.
    let alice = api.user("alice").await;
    let t = client_token(&api.hub, &alice).await;
    assert_eq!(
        api.raw("GET", "/v1/users/alice", Some(&t), None, &[])
            .await
            .0,
        401
    );
    // Revoked and expired keys.
    let scopes = vec!["users:read".to_string()];
    let (k, row) = api
        .hub
        .create_api_key("r", &scopes, None, None)
        .await
        .unwrap();
    assert_eq!(
        api.raw("GET", "/v1/users/alice", Some(&k), None, &[])
            .await
            .0,
        200
    );
    assert!(api.hub.revoke_api_key(&row.key_id).await.unwrap());
    assert!(
        !api.hub.revoke_api_key(&row.key_id).await.unwrap(),
        "already revoked"
    );
    assert_eq!(
        api.raw("GET", "/v1/users/alice", Some(&k), None, &[])
            .await
            .0,
        401
    );
    let (k, _) = api
        .hub
        .create_api_key("e", &scopes, Some(30), None)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(60)).await;
    assert_eq!(
        api.raw("GET", "/v1/users/alice", Some(&k), None, &[])
            .await
            .0,
        401
    );
    // Many keys created at once get distinct ids (a regression: ids used to
    // repeat within one millisecond).
    let mut ids = std::collections::HashSet::new();
    for _ in 0..50 {
        let (_, row) = api
            .hub
            .create_api_key("burst", &scopes, None, None)
            .await
            .unwrap();
        assert!(ids.insert(row.key_id));
    }
    // Key creation input.
    for (name, sc, ttl, ip) in [
        ("", vec!["users:read"], None, None),
        ("x", vec![], None, None),
        ("x", vec!["admin"], None, None),
        ("x", vec!["users:read"], Some(0), None),
        (
            "x",
            vec!["users:read"],
            None,
            Some(vec!["300.1.1.1".to_string()]),
        ),
    ] {
        let sc: Vec<String> = sc.iter().map(|s| s.to_string()).collect();
        assert_eq!(
            api.hub
                .create_api_key(name, &sc, ttl, ip)
                .await
                .unwrap_err()
                .code,
            "bad_request"
        );
    }
    // Each endpoint needs its scope.
    let only_read = api.key(&["users:read"]).await;
    for (m, p, b, scope) in [
        ("PUT", "/v1/users/bob", Some(json!({})), "users:write"),
        ("DELETE", "/v1/users/alice/tokens", None, "tokens:revoke"),
        ("POST", "/v1/agents/bot/keys", None, "agents:write"),
        ("DELETE", "/v1/agents/bot/keys", None, "agents:write"),
        ("DELETE", "/v1/agents/bot/keys/d_x", None, "agents:write"),
        (
            "POST",
            "/v1/conversations",
            Some(json!({"as": "alice", "kind": "group", "title": "t"})),
            "conversations:write",
        ),
        (
            "GET",
            "/v1/users/alice/conversations",
            None,
            "conversations:read",
        ),
        (
            "POST",
            "/v1/users/alice/messages",
            Some(json!({"as": "alice", "text": "x"})),
            "messages:write",
        ),
        (
            "GET",
            "/v1/conversations/c_x/messages",
            None,
            "messages:read",
        ),
    ] {
        let (s, _, v) = api.raw(m, p, Some(&only_read), b, &[]).await;
        assert_eq!((s, code(&v)), (403, "missing_scope"), "{m} {p}");
        assert!(v["detail"].as_str().unwrap().contains(scope));
    }
    assert!(
        api.hub.user_by_name("bob").is_none(),
        "refused calls change nothing"
    );
    // The OpenAPI document needs no key and lists exactly the routes.
    let (s, _, doc) = api.raw("GET", "/v1/openapi.json", None, None, &[]).await;
    assert_eq!(s, 200);
    let mut listed: Vec<String> = vec![];
    for (path, ops) in doc["paths"].as_object().unwrap() {
        for m in ops.as_object().unwrap().keys() {
            listed.push(format!("{} {path}", m.to_uppercase()));
        }
    }
    listed.sort();
    let mut routes: Vec<String> = agentim_core::open_api::ROUTES
        .iter()
        .map(|s| s.to_string())
        .collect();
    routes.sort();
    assert_eq!(listed, routes);
    // Every declared route exists (reaches authentication).
    for r in agentim_core::open_api::ROUTES
        .iter()
        .filter(|r| !r.ends_with("openapi.json"))
    {
        let (m, p) = r.split_once(' ').unwrap();
        let p = p.replace("{id}", "x1").replace("{user}", "x2");
        let (s, _, _) = api.raw(m, &p, None, Some(json!({})), &[]).await;
        assert_eq!(s, 401, "{r}");
    }
    let (s, _, _) = api
        .raw("POST", "/v1/users/x1:explode", Some(&api.key), None, &[])
        .await;
    assert_eq!(s, 404, "unknown action");
}

async fn address_rules(db: TestDb) {
    let api = Api::start(&db).await;
    api.user("alice").await;
    let sc = vec!["users:read".to_string()];
    let (far, _) = api
        .hub
        .create_api_key("far", &sc, None, Some(vec!["10.0.0.0/8".into()]))
        .await
        .unwrap();
    let (near, _) = api
        .hub
        .create_api_key(
            "near",
            &sc,
            None,
            Some(vec!["127.0.0.1".into(), "::1".into()]),
        )
        .await
        .unwrap();
    let (s, _, v) = api
        .raw("GET", "/v1/users/alice", Some(&far), None, &[])
        .await;
    assert_eq!((s, code(&v)), (403, "ip_not_allowed"));
    assert_eq!(
        api.raw("GET", "/v1/users/alice", Some(&near), None, &[])
            .await
            .0,
        200
    );
    // X-Forwarded-For is ignored unless the server is behind a proxy.
    let spoof = [("x-forwarded-for", "10.1.2.3")];
    assert_eq!(
        api.raw("GET", "/v1/users/alice", Some(&far), None, &spoof)
            .await
            .0,
        403
    );
    drop(api);
    let db2 = new_db(Backend::Sqlite).await.unwrap();
    let mut cfg = single(&db2.url);
    cfg.open_api.behind_proxy = true;
    let api = Api::with(cfg).await;
    api.user("alice").await;
    let (far, _) = api
        .hub
        .create_api_key("far", &sc, None, Some(vec!["10.0.0.0/8".into()]))
        .await
        .unwrap();
    assert_eq!(
        api.raw("GET", "/v1/users/alice", Some(&far), None, &spoof)
            .await
            .0,
        200
    );
    let other = [("x-forwarded-for", "192.0.2.1, 10.1.2.3")];
    assert_eq!(
        api.raw("GET", "/v1/users/alice", Some(&far), None, &other)
            .await
            .0,
        200,
        "the entry the proxy added counts"
    );
    // A client forging an allowed address in front of its real one.
    let forged = [("x-forwarded-for", "10.1.2.3, 192.0.2.1")];
    assert_eq!(
        api.raw("GET", "/v1/users/alice", Some(&far), None, &forged)
            .await
            .0,
        403,
        "a forged first entry does not count"
    );
    // No usable entry behind a proxy: no address, so the list refuses.
    for h in [[("x-forwarded-for", "garbage")], [("x-forwarded-for", "")]] {
        assert_eq!(
            api.raw("GET", "/v1/users/alice", Some(&far), None, &h)
                .await
                .0,
            403
        );
    }
}

async fn users_lifecycle(db: TestDb) {
    let api = Api::start(&db).await;
    let (s, v) = api
        .call("PUT", "/v1/users/user_1", Some(json!({"display_name": "Ann", "avatar": "https://x/a.png", "fields": {"tier": "gold"}})))
        .await;
    assert_eq!(s, 201, "{v}");
    assert_eq!(
        (
            &v["id"],
            &v["kind"],
            &v["display_name"],
            &v["fields"]["tier"],
            &v["deactivated"]
        ),
        (
            &json!("user_1"),
            &json!("human"),
            &json!("Ann"),
            &json!("gold"),
            &json!(false)
        )
    );
    // PUT again updates (idempotent create).
    let (s, v) = api
        .call(
            "PUT",
            "/v1/users/user_1",
            Some(json!({"display_name": "Anna"})),
        )
        .await;
    assert_eq!(
        (s, &v["display_name"], &v["fields"]["tier"]),
        (200, &json!("Anna"), &json!("gold"))
    );
    assert_eq!(
        api.ok("GET", "/v1/users/user_1", None).await["display_name"],
        "Anna"
    );
    let v = api
        .ok(
            "PATCH",
            "/v1/users/user_1",
            Some(json!({"fields": {"tier": "silver"}})),
        )
        .await;
    assert_eq!(v["fields"]["tier"], "silver");
    // Agents need a human owner.
    let (s, v) = api
        .call(
            "PUT",
            "/v1/users/bot_1",
            Some(json!({"kind": "agent", "owner": "user_1"})),
        )
        .await;
    // The owner by the developer's id; the agent's first key shown once.
    assert_eq!(
        (s, &v["owner"], &v["team"]),
        (201, &json!("user_1"), &json!(false))
    );
    assert!(v["key"].as_str().unwrap().starts_with("atk_"));
    assert!(v["connect"]["mcp_url"].is_string());
    for (path, b, want) in [
        ("/v1/users/bad%20id", json!({}), "bad_request"),
        ("/v1/users/_system", json!({}), "bad_request"),
        (
            &format!("/v1/users/{}", "a".repeat(65)) as &str,
            json!({}),
            "bad_request",
        ),
        ("/v1/users/bot_2", json!({"kind": "agent"}), "bad_request"),
        (
            "/v1/users/bot_3",
            json!({"kind": "agent", "owner": "bot_1"}),
            "bad_request",
        ),
        (
            "/v1/users/bot_4",
            json!({"kind": "agent", "owner": "nobody"}),
            "not_found",
        ),
        ("/v1/users/h_1", json!({"owner": "user_1"}), "bad_request"),
        ("/v1/users/h_2", json!({"kind": "robot"}), "bad_request"),
        (
            "/v1/users/h_3",
            json!({"avatar": "javascript:alert(1)"}),
            "bad_request",
        ),
        ("/v1/users/h_4", json!({"fields": [1]}), "bad_request"),
        (
            "/v1/users/h_5",
            json!({"fields": {"x": "y".repeat(5000)}}),
            "bad_request",
        ),
        ("/v1/users/h_6", json!({"display_name": ""}), "bad_request"),
        ("/v1/users/h_7", json!({"id": "other"}), "bad_request"),
        (
            "/v1/users/user_1",
            json!({"kind": "agent", "owner": "bot_1"}),
            "bad_request",
        ),
        ("/v1/users/bot_1", json!({"kind": "human"}), "conflict"),
        ("/v1/users/user_1", json!({"unit": "elsewhere"}), "conflict"),
    ] {
        let (s, v) = api.call("PUT", path, Some(b.clone())).await;
        assert_eq!(code(&v), want, "{path} {b} -> {s} {v}");
    }
    for h in [
        "h_1", "h_2", "h_3", "h_4", "h_5", "h_6", "h_7", "bot_2", "bot_3", "bot_4",
    ] {
        assert!(api.hub.user_by_name(h).is_none(), "{h} not created");
    }
    assert_eq!(
        code(&api.call("GET", "/v1/users/nobody", None).await.1),
        "not_found"
    );
    assert_eq!(
        code(&api.call("GET", "/v1/users/_system", None).await.1),
        "not_found",
        "the system user is not exposed"
    );
    assert_eq!(
        code(
            &api.call("PATCH", "/v1/users/user_1", Some(json!({})))
                .await
                .1
        ),
        "bad_request"
    );
    // Deactivation: devices dropped, cannot act, cannot log in.
    let u1 = api.hub.user_by_name("user_1").unwrap();
    let t = client_token(&api.hub, &u1).await;
    let l = api
        .hub
        .authenticate_client(&t, Some("d1"), None, None)
        .await
        .unwrap();
    let v = api.ok("POST", "/v1/users/user_1:deactivate", None).await;
    assert_eq!(v["deactivated"], true);
    assert!(!api.hub.login_valid(&l));
    assert!(
        api.hub
            .authenticate_client(&t, Some("d1"), None, None)
            .await
            .is_err()
    );
    let (s, v) = api
        .call(
            "POST",
            "/v1/conversations",
            Some(json!({"as": "user_1", "kind": "group", "title": "t"})),
        )
        .await;
    assert_eq!((s, code(&v)), (403, "forbidden"));
    assert!(api.hub.devices(&u1.id).await.unwrap().is_empty());
    let v = api.ok("POST", "/v1/users/user_1:reactivate", None).await;
    assert_eq!(v["deactivated"], false);
    assert!(
        api.hub
            .authenticate_client(&t, Some("d1"), None, None)
            .await
            .is_ok()
    );
    // Deactivation survives a restart.
    api.ok("POST", "/v1/users/bot_1:deactivate", None).await;
    let url = db.url.clone();
    drop(api);
    let hub = Hub::open(single(&url), hub_cfg()).await.unwrap();
    assert!(hub.is_deactivated(&hub.user_by_name("bot_1").unwrap().id));
    assert!(!hub.is_deactivated(&u1.id));
}

/// Logging a person out everywhere and agent keys (docs/13 §4.1–§4.2):
/// client tokens are signed by the developer, so there is no endpoint
/// issuing them.
async fn tokens_for_users(db: TestDb) {
    let api = Api::start(&db).await;
    let alice = api.user("alice").await;
    let (s, _) = api.call("POST", "/v1/users/alice/tokens", None).await;
    assert_eq!(s, 405, "client tokens are not issued here");
    let t = client_token(&api.hub, &alice).await;
    let mut logins = vec![];
    for d in ["phone", "laptop"] {
        logins.push(
            api.hub
                .authenticate_client(&t, Some(d), None, None)
                .await
                .unwrap(),
        );
    }
    assert_eq!(
        code(&api.call("DELETE", "/v1/users/nobody/tokens", None).await.1),
        "not_found"
    );
    let v = api.ok("DELETE", "/v1/users/alice/tokens", None).await;
    assert_eq!(v["revoked"], 2);
    for l in &logins {
        assert!(!api.hub.login_valid(l));
    }
    assert!(
        api.hub
            .authenticate_client(&t, Some("phone"), None, None)
            .await
            .is_err(),
        "tokens issued before are refused"
    );
    assert_eq!(
        api.ok("DELETE", "/v1/users/alice/tokens", None).await["revoked"],
        0
    );
    // Agent keys.
    api.ok(
        "PUT",
        "/v1/users/bot",
        Some(json!({"kind": "agent", "owner": "alice"})),
    )
    .await;
    let bot = api.hub.user_by_name("bot").unwrap();
    let (s, k1) = api.call("POST", "/v1/agents/bot/keys", None).await;
    assert_eq!(s, 201, "{k1}");
    let key1 = k1["key"].as_str().unwrap().to_string();
    assert!(key1.starts_with("atk_"));
    assert_eq!(
        api.hub
            .authenticate_client(&key1, None, None, None)
            .await
            .unwrap()
            .user
            .id,
        bot.id
    );
    assert!(k1["connect"]["mcp_url"].as_str().unwrap().ends_with("/mcp"));
    let before = api.hub.devices(&bot.id).await.unwrap().len();
    // Refusals change nothing.
    for (m, p, b, want) in [
        ("POST", "/v1/agents/nobody/keys", None, "not_found"),
        ("POST", "/v1/agents/alice/keys", None, "not_found"),
        (
            "POST",
            "/v1/agents/bot/keys",
            Some(json!({"other": 1})),
            "bad_request",
        ),
        ("DELETE", "/v1/agents/bot/keys/d_nope", None, "not_found"),
        ("DELETE", "/v1/agents/alice/keys", None, "not_found"),
    ] {
        let (_, v) = api.call(m, p, b.clone()).await;
        assert_eq!(code(&v), want, "{m} {p} {b:?}");
    }
    assert_eq!(api.hub.devices(&bot.id).await.unwrap().len(), before);
    // Rotating: a new key, the others revoked in the same write.
    let k2 = api
        .ok(
            "POST",
            "/v1/agents/bot/keys",
            Some(json!({"revoke_others": true})),
        )
        .await;
    assert_eq!(k2["revoked"].as_array().unwrap().len(), before);
    assert!(api.hub.authenticate_device(&key1).is_none());
    let dev2 = k2["device_id"].as_str().unwrap();
    let v = api
        .ok("DELETE", &format!("/v1/agents/bot/keys/{dev2}"), None)
        .await;
    assert_eq!(v["revoked"], 1);
    api.ok("POST", "/v1/agents/bot/keys", None).await;
    api.ok("POST", "/v1/agents/bot/keys", None).await;
    assert_eq!(
        api.ok("DELETE", "/v1/agents/bot/keys", None).await["revoked"],
        2
    );
    assert!(api.hub.devices(&bot.id).await.unwrap().is_empty());
    // A deactivated agent gets no key.
    api.ok("POST", "/v1/users/bot:deactivate", None).await;
    let (s, v) = api.call("POST", "/v1/agents/bot/keys", None).await;
    assert_eq!((s, code(&v)), (409, "conflict"));
    // Every call is audited with the key that made it.
    let log = api.hub.audit_log(200).await.unwrap();
    for a in [
        "api.tokens.revoke",
        "api.agents.key",
        "api.agents.revoke_key",
        "api.agents.revoke_keys",
    ] {
        assert!(log.iter().any(|r| r["action"] == a), "{a}");
    }
    assert!(
        !json!(log).to_string().contains(&key1),
        "keys are not audited"
    );
}

async fn conversations_as_users_and_the_system(db: TestDb) {
    let api = Api::start(&db).await;
    let alice = api.user("alice").await;
    let bob = api.user("bob").await;
    api.user("carol").await;
    let mut b = Conn::new(&api.hub, &bob);
    // As a user.
    let (s, c) = api
        .call("POST", "/v1/conversations", Some(json!({"as": "alice", "kind": "group", "title": "Team", "members": ["bob"], "fields": {"project": "x"}})))
        .await;
    assert_eq!(s, 201, "{c}");
    let conv = c["id"].as_str().unwrap().to_string();
    assert_eq!(c["fields"]["project"], "x");
    let roles: Vec<(String, String)> = c["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| {
            (
                m["user"].as_str().unwrap().into(),
                m["role"].as_str().unwrap().into(),
            )
        })
        .collect();
    assert_eq!(
        roles,
        [
            ("alice".into(), "owner".into()),
            ("bob".into(), "member".into())
        ]
    );
    assert!(
        b.drain()
            .iter()
            .any(|f| f["event"]["type"] == "member.joined"),
        "pushed like a client action"
    );
    // As the system: needs an owner, who becomes the owner.
    let (s, v) = api
        .call(
            "POST",
            "/v1/conversations",
            Some(json!({"as": "system", "kind": "group", "title": "Ops"})),
        )
        .await;
    assert_eq!((s, code(&v)), (400, "bad_request"));
    let (s, v) = api
        .call(
            "POST",
            "/v1/conversations",
            Some(json!({"as": "alice", "kind": "group", "title": "x", "owner": "bob"})),
        )
        .await;
    assert_eq!((s, code(&v)), (400, "bad_request"));
    let sys = api.ok("POST", "/v1/conversations", Some(json!({"as": "system", "owner": "carol", "kind": "group", "title": "Ops", "members": ["alice", "bob"]}))).await;
    let ops = sys["id"].as_str().unwrap().to_string();
    assert_eq!(
        sys["members"][0],
        json!({"user": "carol", "kind": "human", "role": "owner"})
    );
    // One direct conversation per pair: the second create returns it.
    let d1 = api
        .ok(
            "POST",
            "/v1/conversations",
            Some(json!({"as": "alice", "kind": "dm", "title": "dm", "members": ["bob"]})),
        )
        .await;
    let (s, d2) = api
        .call(
            "POST",
            "/v1/conversations",
            Some(json!({"as": "bob", "kind": "dm", "title": "dm", "members": ["alice"]})),
        )
        .await;
    assert_eq!(
        (s, &d2["id"], &d2["existing"]),
        (200, &d1["id"], &json!(true))
    );
    // Reading.
    assert_eq!(
        api.ok("GET", &format!("/v1/conversations/{conv}"), None)
            .await["title"],
        "Team"
    );
    assert_eq!(
        code(&api.call("GET", "/v1/conversations/c_nope", None).await.1),
        "not_found"
    );
    let list = api.ok("GET", "/v1/users/bob/conversations", None).await;
    assert_eq!(list["convs"].as_array().unwrap().len(), 3, "{list}");
    // Changing: owner yes, a plain member no, the system yes; nothing to change is refused.
    let path = format!("/v1/conversations/{conv}");
    api.ok(
        "PATCH",
        &path,
        Some(json!({"as": "alice", "title": "Team A"})),
    )
    .await;
    let (s, v) = api
        .call("PATCH", &path, Some(json!({"as": "bob", "title": "mine"})))
        .await;
    assert_eq!((s, code(&v)), (403, "forbidden"));
    let (s, v) = api
        .call(
            "PATCH",
            &path,
            Some(json!({"as": "carol", "title": "mine"})),
        )
        .await;
    assert_eq!((s, code(&v)), (403, "forbidden"), "not a member");
    let v = api
        .ok(
            "PATCH",
            &path,
            Some(json!({"as": "system", "fields": {"project": "y"}})),
        )
        .await;
    assert_eq!(
        (&v["title"], &v["fields"]["project"]),
        (&json!("Team A"), &json!("y"))
    );
    assert_eq!(
        code(
            &api.call("PATCH", &path, Some(json!({"as": "alice"})))
                .await
                .1
        ),
        "bad_request"
    );
    let (s, v) = api
        .call(
            "PATCH",
            &format!("/v1/conversations/{}", d1["id"].as_str().unwrap()),
            Some(json!({"as": "alice", "title": "x"})),
        )
        .await;
    assert_eq!(
        (s, code(&v)),
        (400, "bad_request"),
        "a direct conversation has no title"
    );
    assert!(
        b.drain()
            .iter()
            .any(|f| f["event"]["type"] == "conv.updated")
    );
    // Members: the system adds, changes roles, transfers ownership, removes.
    let members = format!("{path}/members");
    let v = api
        .ok(
            "POST",
            &members,
            Some(json!({"as": "system", "users": ["carol", "nobody"]})),
        )
        .await;
    assert_eq!(v["results"][0]["ok"], true);
    assert_eq!(v["results"][1]["code"], "not_found");
    api.ok(
        "PATCH",
        &format!("{members}/bob"),
        Some(json!({"as": "system", "role": "admin"})),
    )
    .await;
    let v = api
        .ok(
            "PATCH",
            &format!("{members}/carol"),
            Some(json!({"as": "system", "role": "owner"})),
        )
        .await;
    let role = |v: &Value, u: &str| {
        v["members"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["user"] == u)
            .unwrap()["role"]
            .clone()
    };
    assert_eq!(
        (role(&v, "carol"), role(&v, "alice"), role(&v, "bob")),
        (json!("owner"), json!("admin"), json!("admin"))
    );
    let (s, v) = api
        .call("DELETE", &format!("{members}/carol?as=system"), None)
        .await;
    assert_eq!((s, code(&v)), (409, "conflict"), "the owner is not removed");
    api.ok("DELETE", &format!("{members}/bob?as=system"), None)
        .await;
    // A user removing themself leaves.
    api.ok("DELETE", &format!("{members}/alice?as=alice"), None)
        .await;
    let v = api.ok("GET", &path, None).await;
    assert_eq!(v["members"].as_array().unwrap().len(), 1);
    let (s, v) = api
        .call("DELETE", &format!("{members}/alice?as=alice"), None)
        .await;
    assert_eq!((s, code(&v)), (403, "forbidden"), "already gone");
    let (s, v) = api
        .call(
            "PATCH",
            &format!("{members}/alice"),
            Some(json!({"as": "system", "role": "boss"})),
        )
        .await;
    assert_eq!(s, 400, "{v}");
    // Dissolving as the system.
    let (s, v) = api
        .call("DELETE", &format!("/v1/conversations/{ops}?as=bob"), None)
        .await;
    assert_eq!((s, code(&v)), (403, "forbidden"));
    api.ok(
        "DELETE",
        &format!("/v1/conversations/{ops}?as=system"),
        None,
    )
    .await;
    assert_eq!(
        code(
            &api.call("GET", &format!("/v1/conversations/{ops}"), None)
                .await
                .1
        ),
        "not_found"
    );
    let (s, v) = api
        .call("DELETE", &format!("/v1/conversations/{ops}"), None)
        .await;
    assert_eq!((s, code(&v)), (400, "bad_request"), "as is required");
    let _ = alice;
}

async fn messages_as_users_and_the_system(db: TestDb) {
    let api = Api::start(&db).await;
    let alice = api.user("alice").await;
    let bob = api.user("bob").await;
    api.user("eve").await;
    let mut a2 = Conn::new(&api.hub, &alice);
    let mut b = Conn::new(&api.hub, &bob);
    let conv = api
        .ok(
            "POST",
            "/v1/conversations",
            Some(json!({"as": "alice", "kind": "group", "title": "t", "members": ["bob"]})),
        )
        .await["id"]
        .as_str()
        .unwrap()
        .to_string();
    a2.drain();
    b.drain();
    let msgs = format!("/v1/conversations/{conv}/messages");
    // As a user: pushed to the others and to the user's own devices.
    let (s, v) = api
        .call(
            "POST",
            &msgs,
            Some(json!({"as": "alice", "text": "hello", "client_msg_id": "m1"})),
        )
        .await;
    assert_eq!(s, 201, "{v}");
    let msg_id = v["msg_id"].as_str().unwrap().to_string();
    assert!(
        b.drain()
            .iter()
            .any(|f| f["event"]["msg"]["content"] == "hello")
    );
    assert!(
        a2.drain()
            .iter()
            .any(|f| f["event"]["msg"]["content"] == "hello")
    );
    // The same client_msg_id is a retry.
    let v = api
        .ok(
            "POST",
            &msgs,
            Some(json!({"as": "alice", "text": "hello", "client_msg_id": "m1"})),
        )
        .await;
    assert_eq!(
        (&v["duplicate"], &v["msg_id"]),
        (&json!(true), &json!(msg_id))
    );
    // The routing envelope and sources (docs/03-agents.md §6.2–§6.3, docs/03-agents.md §3).
    let (s, v) = api
        .call(
            "POST",
            &msgs,
            Some(json!({"as": "alice", "text": "please review", "to": ["bob"], "intent": "request", "topics": ["review"], "refs": ["doc://1"], "sources": [{"title": "Spec", "url": "https://example.com/spec"}]})),
        )
        .await;
    assert_eq!(s, 201, "{v}");
    let pushed = b.drain();
    let env = pushed
        .iter()
        .find(|f| f["event"]["msg"]["content"] == "please review")
        .unwrap()["event"]["msg"]["envelope"]
        .clone();
    assert_eq!(
        env,
        json!({"to": [bob.id], "intent": "request", "topics": ["review"], "refs": ["doc://1"], "sources": [{"title": "Spec", "url": "https://example.com/spec"}]})
    );
    for bad in [
        json!({"as": "alice", "text": "x", "intent": "shout"}),
        json!({"as": "alice", "text": "x", "to": ["eve"]}),
        json!({"as": "alice", "text": "x", "sources": [{"title": "t", "url": "ftp://x"}]}),
        json!({"as": "alice", "text": "x", "to": "bob"}),
    ] {
        let (s, v) = api.call("POST", &msgs, Some(bad.clone())).await;
        assert_eq!(s, 400, "{bad}: {v}");
    }
    assert!(b.drain().is_empty(), "refused sends push nothing");
    // Custom kinds and targeted messages go through too.
    api.ok("POST", &msgs, Some(json!({"as": "alice", "text": "[order]", "kind": "com.acme.order", "payload": {"id": 1}}))).await;
    let v = api
        .ok(
            "POST",
            &msgs,
            Some(json!({"as": "alice", "text": "psst", "to_user": "bob"})),
        )
        .await;
    assert!(v["side_seq"].is_i64());
    // The client rules apply to the acting user.
    for (who, want) in [("eve", "forbidden"), ("nobody", "not_found")] {
        let (_, v) = api
            .call("POST", &msgs, Some(json!({"as": who, "text": "x"})))
            .await;
        assert_eq!(code(&v), want, "{who}");
    }
    assert_eq!(
        code(&api.call("POST", &msgs, Some(json!({"text": "x"}))).await.1),
        "bad_request"
    );
    assert_eq!(
        code(
            &api.call("POST", &msgs, Some(json!({"as": "alice", "text": ""})))
                .await
                .1
        ),
        "bad_request"
    );
    // A system notice: no membership needed, marked as from the system.
    let v = api
        .ok(
            "POST",
            &msgs,
            Some(json!({"as": "system", "text": "Maintenance tonight"})),
        )
        .await;
    let notice = v["msg_id"].as_str().unwrap().to_string();
    let pushed = b.drain();
    let m = pushed
        .iter()
        .find(|f| f["event"]["msg"]["msg_id"] == notice.as_str())
        .unwrap();
    assert_eq!(m["event"]["msg"]["sender_type"], "system");
    // Editing: the sender; recalling: the system recalls anything.
    let mpath = format!("/v1/messages/{msg_id}");
    let (s, v) = api
        .call("PATCH", &mpath, Some(json!({"as": "bob", "text": "x"})))
        .await;
    assert_eq!((s, code(&v)), (403, "forbidden"));
    api.ok(
        "PATCH",
        &mpath,
        Some(json!({"as": "alice", "text": "hello!"})),
    )
    .await;
    let (s, v) = api
        .call("PATCH", &mpath, Some(json!({"as": "system", "text": "x"})))
        .await;
    assert_eq!(
        (s, code(&v)),
        (403, "forbidden"),
        "the system does not put words in people's mouths"
    );
    api.ok(
        "POST",
        &format!("{mpath}:recall"),
        Some(json!({"as": "system"})),
    )
    .await;
    let (s, v) = api
        .call(
            "POST",
            &format!("{mpath}:recall"),
            Some(json!({"as": "system"})),
        )
        .await;
    assert_eq!((s, code(&v)), (409, "conflict"));
    // To a person: the direct conversation is opened once.
    let v1 = api
        .ok(
            "POST",
            "/v1/users/bob/messages",
            Some(json!({"as": "alice", "text": "hi bob"})),
        )
        .await;
    let v2 = api
        .ok(
            "POST",
            "/v1/users/bob/messages",
            Some(json!({"as": "alice", "text": "again"})),
        )
        .await;
    assert_eq!(v1["conv_id"], v2["conv_id"]);
    assert_eq!(
        code(
            &api.call(
                "POST",
                "/v1/users/bob/messages",
                Some(json!({"as": "system", "text": "x"}))
            )
            .await
            .1
        ),
        "bad_request"
    );
    assert_eq!(
        code(
            &api.call(
                "POST",
                "/v1/users/alice/messages",
                Some(json!({"as": "alice", "text": "x"}))
            )
            .await
            .1
        ),
        "bad_request"
    );
    b.ok(json!({"op": "block", "user_id": alice.id})).await;
    let (s, v) = api
        .call(
            "POST",
            "/v1/users/bob/messages",
            Some(json!({"as": "alice", "text": "x"})),
        )
        .await;
    assert_eq!((s, code(&v)), (403, "blocked"));
    // History needs messages:read, uses developer ids, and is audited.
    let h = api
        .ok("GET", &format!("{msgs}?after_seq=0&limit=200"), None)
        .await;
    let evs = h["events"].as_array().unwrap();
    assert!(evs.iter().any(|e| e["msg"]["sender_id"] == "alice"));
    assert!(evs.iter().any(|e| e["msg"]["sender_id"] == "_system"));
    assert!(
        !h.to_string().contains("psst"),
        "targeted messages are not in the log"
    );
    assert_eq!(h["has_more"], false);
    let page = api
        .ok("GET", &format!("{msgs}?after_seq=0&limit=2"), None)
        .await;
    assert_eq!(
        (
            page["events"].as_array().unwrap().len(),
            &page["has_more"],
            &page["next_after_seq"]
        ),
        (2, &json!(true), &json!(2))
    );
    for q in ["limit=0", "limit=201", "after_seq=-1"] {
        assert_eq!(
            code(&api.call("GET", &format!("{msgs}?{q}"), None).await.1),
            "bad_request",
            "{q}"
        );
    }
    assert_eq!(
        code(
            &api.call("GET", "/v1/conversations/c_nope/messages", None)
                .await
                .1
        ),
        "not_found"
    );
    let audit = api.hub.audit_log(1000).await.unwrap();
    assert!(
        audit
            .iter()
            .any(|a| a["action"] == "api.messages.read" && a["detail"]["key_id"].is_string()),
        "{audit:?}"
    );
    assert!(audit.iter().any(|a| a["action"] == "api.messages.send"));
    // Read cursors for a user sync to their devices.
    a2.drain();
    api.ok(
        "POST",
        &format!("/v1/conversations/{conv}/read"),
        Some(json!({"as": "alice", "seq": 3})),
    )
    .await;
    assert!(
        a2.drain()
            .iter()
            .any(|f| f["type"] == "read" && f["seq"] == 3)
    );
    assert_eq!(
        code(
            &api.call(
                "POST",
                &format!("/v1/conversations/{conv}/read"),
                Some(json!({"as": "system", "seq": 3}))
            )
            .await
            .1
        ),
        "bad_request"
    );
}

async fn batches_report_each_item(db: TestDb) {
    let api = Api::start(&db).await;
    let v = api
        .ok("POST", "/v1/users:batch", Some(json!({"users": [{"id": "u1"}, {"id": "bad id"}, {"id": "u2", "display_name": "Two"}, {"id": "u1"}, {"id": "u3", "nope": 1}]})))
        .await;
    let r = v["results"].as_array().unwrap();
    let summary: Vec<(bool, Value)> = r
        .iter()
        .map(|x| (x["ok"].as_bool().unwrap(), x["created"].clone()))
        .collect();
    assert_eq!(
        summary,
        [
            (true, json!(true)),
            (false, Value::Null),
            (true, json!(true)),
            (true, json!(false)),
            (false, Value::Null)
        ]
    );
    assert_eq!(r[1]["code"], "bad_request");
    let many: Vec<Value> = (0..101).map(|i| json!({"id": format!("m{i}")})).collect();
    assert_eq!(
        code(
            &api.call("POST", "/v1/users:batch", Some(json!({"users": many})))
                .await
                .1
        ),
        "bad_request"
    );
    assert_eq!(
        code(
            &api.call("POST", "/v1/users:batch", Some(json!({"users": []})))
                .await
                .1
        ),
        "bad_request"
    );
    // Batch send: one sender, many people, each its own direct message.
    let alice = api.user("alice").await;
    let mut targets = vec![];
    for i in 0..5 {
        targets.push(api.user(&format!("t{i}")).await);
    }
    let mut t0 = Conn::new(&api.hub, &targets[0]);
    t0.ok(json!({"op": "block", "user_id": alice.id})).await;
    let v = api
        .ok("POST", "/v1/messages:batch-send", Some(json!({"as": "alice", "to": ["t0", "t1", "t2", "nobody", "t1", "t3", "t4"], "text": "promo", "client_msg_id": "camp-1"})))
        .await;
    let r: Vec<(String, bool, String)> = v["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| {
            (
                x["to"].as_str().unwrap().into(),
                x["ok"].as_bool().unwrap(),
                x["code"].as_str().unwrap_or("").into(),
            )
        })
        .collect();
    assert_eq!(
        r,
        [
            ("t0".into(), false, "blocked".into()),
            ("t1".into(), true, "".into()),
            ("t2".into(), true, "".into()),
            ("nobody".into(), false, "not_found".into()),
            ("t1".into(), false, "bad_request".into()),
            ("t3".into(), true, "".into()),
            ("t4".into(), true, "".into()),
        ]
    );
    // Retrying the campaign sends nothing twice.
    let v = api
        .ok("POST", "/v1/messages:batch-send", Some(json!({"as": "alice", "to": ["t1", "t2"], "text": "promo", "client_msg_id": "camp-1"})))
        .await;
    assert!(
        v["results"]
            .as_array()
            .unwrap()
            .iter()
            .all(|x| x["duplicate"] == true),
        "{v}"
    );
    let to: Vec<String> = (0..501).map(|i| format!("x{i}")).collect();
    assert_eq!(
        code(
            &api.call(
                "POST",
                "/v1/messages:batch-send",
                Some(json!({"as": "alice", "to": to, "text": "x"}))
            )
            .await
            .1
        ),
        "bad_request"
    );
    assert!(
        t0.drain()
            .iter()
            .all(|f| f["event"]["msg"]["content"] != "promo")
    );
}

async fn idempotency_keys(db: TestDb) {
    let api = Api::start(&db).await;
    api.user("alice").await;
    let conv = api
        .ok(
            "POST",
            "/v1/conversations",
            Some(json!({"as": "alice", "kind": "group", "title": "t"})),
        )
        .await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let msgs = format!("/v1/conversations/{conv}/messages");
    let b = json!({"as": "alice", "text": "once"});
    let k = api.key.clone();
    let h = [("idempotency-key", "op-1")];
    let (s1, h1, v1) = api.raw("POST", &msgs, Some(&k), Some(b.clone()), &h).await;
    let (s2, h2, v2) = api.raw("POST", &msgs, Some(&k), Some(b.clone()), &h).await;
    assert_eq!((s1, s2), (201, 201));
    assert_eq!(v1, v2);
    assert!(h1.get("idempotent-replayed").is_none());
    assert_eq!(h2["idempotent-replayed"], "true");
    // A different body with the same key.
    let (s, _, v) = api
        .raw(
            "POST",
            &msgs,
            Some(&k),
            Some(json!({"as": "alice", "text": "other"})),
            &h,
        )
        .await;
    assert_eq!((s, code(&v)), (422, "idempotency_mismatch"));
    // Keys are per API key: another key may reuse the name.
    let k2 = api.key(&["messages:write"]).await;
    let (s, _, v) = api.raw("POST", &msgs, Some(&k2), Some(b.clone()), &h).await;
    assert_eq!(s, 201);
    assert_ne!(v["msg_id"], v1["msg_id"]);
    // Errors are remembered too (4xx), so a retry gets the same answer.
    let bad = [("idempotency-key", "op-bad")];
    let (s, _, _) = api
        .raw(
            "POST",
            &msgs,
            Some(&k),
            Some(json!({"as": "nobody", "text": "x"})),
            &bad,
        )
        .await;
    let (s2, h2, _) = api
        .raw(
            "POST",
            &msgs,
            Some(&k),
            Some(json!({"as": "nobody", "text": "x"})),
            &bad,
        )
        .await;
    assert_eq!(
        (s, s2, &h2["idempotent-replayed"]),
        (404, 404, &reqwest::header::HeaderValue::from_static("true"))
    );
    // Invalid keys.
    let long = "k".repeat(256);
    for key in ["", long.as_str(), "with space"] {
        let (s, _, v) = api
            .raw(
                "POST",
                &msgs,
                Some(&k),
                Some(b.clone()),
                &[("idempotency-key", key)],
            )
            .await;
        assert_eq!((s, code(&v)), (400, "bad_request"), "{key:?}");
    }
    // Concurrent retries with one key: exactly one message.
    let mut tasks = vec![];
    for _ in 0..8 {
        let (http, url, k, b) = (
            api.http.clone(),
            format!("{}{msgs}", api.base),
            k.clone(),
            b.clone(),
        );
        tasks.push(tokio::spawn(async move {
            http.post(url)
                .header("authorization", format!("Bearer {k}"))
                .header("idempotency-key", "race-1")
                .header("content-type", "application/json")
                .body(b.to_string())
                .send()
                .await
                .unwrap()
                .status()
                .as_u16()
        }));
    }
    let mut st = vec![];
    for t in tasks {
        st.push(t.await.unwrap());
    }
    assert!(st.iter().all(|s| *s == 201 || *s == 409), "{st:?}");
    assert!(st.contains(&201));
    let h = api.ok("GET", &format!("{msgs}?limit=200"), None).await;
    let once = h["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["msg"]["content"] == "once")
        .count();
    assert_eq!(once, 3, "op-1, the second key's op-1, and race-1: one each");
    // Without an Idempotency-Key a resend is a new message.
    api.ok("POST", &msgs, Some(b.clone())).await;
    let h = api.ok("GET", &format!("{msgs}?limit=200"), None).await;
    assert_eq!(
        h["events"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["msg"]["content"] == "once")
            .count(),
        4
    );
}

async fn rate_limits_per_key(db: TestDb) {
    let mut cfg = single(&db.url);
    cfg.open_api.rate_per_second = 0.5;
    cfg.open_api.burst = 3.0;
    let api = Api::with(cfg).await;
    api.hub
        .create_user("alice", UserKind::Human, None, None)
        .await
        .unwrap();
    let mut last = None;
    for i in 0..3 {
        let (s, h, _) = api
            .raw("GET", "/v1/users/alice", Some(&api.key), None, &[])
            .await;
        assert_eq!(s, 200, "request {i}");
        assert_eq!(h["ratelimit-policy"], "\"default\";q=3;w=1");
        last = Some(h["ratelimit"].to_str().unwrap().to_string());
    }
    assert_eq!(last.as_deref(), Some("\"default\";r=0;t=1"));
    let (s, h, v) = api
        .raw("GET", "/v1/users/alice", Some(&api.key), None, &[])
        .await;
    assert_eq!((s, code(&v)), (429, "rate_limited"));
    assert!(h["retry-after"].to_str().unwrap().parse::<u64>().unwrap() >= 1);
    // Another key has its own budget; refusals before authentication cost nothing.
    let other = api.key(&["users:read"]).await;
    assert_eq!(
        api.raw("GET", "/v1/users/alice", Some(&other), None, &[])
            .await
            .0,
        200
    );
    assert_eq!(
        api.raw("GET", "/v1/users/alice", Some("ak_bad"), None, &[])
            .await
            .0,
        401
    );
}

async fn errors_are_problem_details(db: TestDb) {
    let api = Api::start(&db).await;
    api.user("alice").await;
    for (m, p, raw_body) in [
        ("PUT", "/v1/users/x", "{not json"),
        ("POST", "/v1/conversations", "[]"),
        (
            "POST",
            "/v1/conversations",
            r#"{"as":"alice","kind":"group","title":"t","surprise":1}"#,
        ),
    ] {
        let resp = api
            .http
            .request(m.parse().unwrap(), format!("{}{p}", api.base))
            .header("authorization", format!("Bearer {}", api.key))
            .body(raw_body)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 400, "{p} {raw_body}");
        assert_eq!(resp.headers()["content-type"], "application/problem+json");
        let v: Value = serde_json::from_str(&resp.text().await.unwrap()).unwrap();
        assert_eq!(
            (&v["type"], &v["status"], &v["code"]),
            (
                &json!("https://agentim.dev/errors/bad_request"),
                &json!(400),
                &json!("bad_request")
            )
        );
        assert!(v["detail"].is_string() && v["title"] == "Bad Request");
    }
    // Storage failures are 503 without internal details.
    api.hub.inject_commit_failures("main", 1);
    let (s, v) = api.call("PUT", "/v1/users/new_one", Some(json!({}))).await;
    assert_eq!(s, 503, "{v}");
    assert!(!v["detail"].as_str().unwrap().contains("injected"), "{v}");
}

async fn input_limits(db: TestDb) {
    let api = Api::start(&db).await;
    let code = |v: &Value| v["code"].as_str().unwrap_or("").to_string();
    api.user("alice").await;
    let long_id = "r".repeat(64);
    api.user(&long_id).await;
    // Users per batch: 1..500.
    let many: Vec<Value> = (0..501).map(|i| json!({"id": format!("b{i}")})).collect();
    let (s, v) = api
        .call("POST", "/v1/users:batch", Some(json!({"users": many})))
        .await;
    assert_eq!((s, code(&v)), (400, "bad_request".into()));
    assert!(
        api.hub.user_by_name("b0").is_none(),
        "a refused batch creates nobody"
    );
    // client_msg_id: 1..128 bytes, also with the recipient appended.
    let send = |id: String| json!({"as": "alice", "text": "hi", "client_msg_id": id});
    let (s, v) = api
        .call("POST", "/v1/messages:batch-send", Some(json!({"as": "alice", "to": [long_id], "text": "hi", "client_msg_id": "c".repeat(64)})))
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["results"][0]["ok"], false, "{v}");
    assert_eq!(
        v["results"][0]["code"], "bad_request",
        "id plus recipient over 128 bytes"
    );
    let conv = api
        .ok(
            "POST",
            "/v1/conversations",
            Some(json!({"as": "alice", "kind": "group", "title": "g", "members": []})),
        )
        .await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let path = format!("/v1/conversations/{conv}/messages");
    for bad in ["".to_string(), "m".repeat(129)] {
        let (s, v) = api.call("POST", &path, Some(send(bad))).await;
        assert_eq!((s, code(&v)), (400, "bad_request".into()));
    }
    let ok = api.ok("POST", &path, Some(send("m".repeat(128)))).await;
    assert!(ok["msg_id"].is_string(), "{ok}");
    // Members and recipients per call: 1..500.
    let many: Vec<String> = (0..501).map(|i| format!("x{i}")).collect();
    for (path, body) in [
        (
            format!("/v1/conversations/{conv}/members"),
            json!({"as": "alice", "users": []}),
        ),
        (
            format!("/v1/conversations/{conv}/members"),
            json!({"as": "alice", "users": many}),
        ),
        (
            "/v1/messages:batch-send".to_string(),
            json!({"as": "alice", "to": [], "text": "x"}),
        ),
        (
            "/v1/messages:batch-send".to_string(),
            json!({"as": "alice", "to": many, "text": "x"}),
        ),
    ] {
        let (s, v) = api.call("POST", &path, Some(body)).await;
        assert_eq!((s, code(&v)), (400, "bad_request".into()), "{path}");
    }
    // Unknown actions on a message.
    let id = ok["msg_id"].as_str().unwrap();
    let (s, v) = api
        .call(
            "POST",
            &format!("/v1/messages/{id}:explode"),
            Some(json!({})),
        )
        .await;
    assert_eq!((s, code(&v)), (404, "not_found".into()));
}
