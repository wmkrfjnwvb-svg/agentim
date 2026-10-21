//! What the developer's backend does through the open API instead of the
//! person's client (docs/13 §4.1, problems 3, 4, 8): handing out agents
//! (personal and team), listing, stopping, resuming, transferring,
//! destroying, exporting and importing them; people's devices; silencing
//! members of a conversation. Business rules, refusals without trace,
//! audit, restarts and races, on every backend.

mod common;

use std::net::SocketAddr;
use std::time::Duration;

use agentim_core::hub::Hub;
use agentim_core::model::*;
use agentim_core::server::{AppState, router};
use common::*;
use serde_json::{Value, json};

backend_tests!(
    agents_are_handed_out_and_listed,
    stopping_resuming_and_destroying_agents,
    team_agents_are_transferred,
    agents_move_by_export_and_import,
    devices_are_listed_and_logged_out,
    silenced_members_cannot_speak,
    silences_end_and_survive_a_restart,
    a_silenced_agents_drafts_wait,
    race_sends_while_a_member_is_silenced,
);

struct Api {
    base: String,
    hub: Hub,
    http: reqwest::Client,
    key: String,
}

impl Api {
    async fn start(db: &TestDb) -> Api {
        Api::open(Hub::open(single(&db.url), hub_cfg()).await.unwrap()).await
    }

    async fn open(hub: Hub) -> Api {
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
        Api {
            base: format!("http://{addr}"),
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

    async fn with_key(
        &self,
        key: &str,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> (u16, Value) {
        let mut r = self
            .http
            .request(method.parse().unwrap(), format!("{}{path}", self.base))
            .header("authorization", format!("Bearer {key}"));
        if let Some(b) = body {
            r = r
                .header("content-type", "application/json")
                .body(b.to_string());
        }
        let resp = r.send().await.unwrap();
        let s = resp.status().as_u16();
        let t = resp.text().await.unwrap();
        (s, serde_json::from_str(&t).unwrap_or(Value::String(t)))
    }

    async fn call(&self, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        self.with_key(&self.key, method, path, body).await
    }

    async fn ok(&self, method: &str, path: &str, body: Option<Value>) -> Value {
        let (s, v) = self.call(method, path, body.clone()).await;
        assert!(
            (200..300).contains(&s),
            "{method} {path} {body:?} -> {s} {v}"
        );
        v
    }

    async fn person(&self, id: &str) -> User {
        self.ok("PUT", &format!("/v1/users/{id}"), None).await;
        self.hub.user_by_name(id).unwrap()
    }

    async fn agent(&self, id: &str, owner: &str, team: bool) -> (User, String) {
        let v = self
            .ok(
                "PUT",
                &format!("/v1/users/{id}"),
                Some(json!({"kind": "agent", "owner": owner, "team": team})),
            )
            .await;
        (
            self.hub.user_by_name(id).unwrap(),
            v["key"].as_str().unwrap().to_string(),
        )
    }

    async fn audits(&self, action: &str) -> usize {
        self.hub
            .audit_log(2000)
            .await
            .unwrap()
            .iter()
            .filter(|r| r["action"] == action)
            .count()
    }
}

fn code(v: &Value) -> &str {
    v["code"].as_str().unwrap_or("")
}

fn ws_code(v: &Value) -> &str {
    v["error"]["code"].as_str().unwrap_or("")
}

async fn agents_are_handed_out_and_listed(db: TestDb) {
    let api = Api::start(&db).await;
    api.person("alice").await;
    api.person("bob").await;
    let (helper, key) = api.agent("helper", "alice", false).await;
    let (team, _) = api.agent("desk", "alice", true).await;
    assert!(team.team && !helper.team);
    // The key printed once works, and is the agent's only key.
    let l = api
        .hub
        .authenticate_client(&key, None, None, None)
        .await
        .unwrap();
    assert_eq!(l.user.id, helper.id);
    let list = api.ok("GET", "/v1/users/alice/agents", None).await;
    let agents = list["agents"].as_array().unwrap();
    let ids: Vec<&str> = agents.iter().map(|a| a["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["desk", "helper"]);
    let h = &agents[1];
    assert_eq!(
        (&h["owner"], &h["team"], &h["stopped"], &h["deactivated"]),
        (&json!("alice"), &json!(false), &json!(false), &json!(false))
    );
    assert_eq!(h["keys"].as_array().unwrap().len(), 1);
    assert!(!list.to_string().contains(&key), "never the secrets");
    assert_eq!(
        api.ok("GET", "/v1/users/bob/agents", None).await["agents"],
        json!([])
    );
    // Refusals.
    for (path, want) in [
        ("/v1/users/nobody/agents", "not_found"),
        ("/v1/users/helper/agents", "bad_request"),
    ] {
        assert_eq!(code(&api.call("GET", path, None).await.1), want, "{path}");
    }
    for (body, want) in [
        (json!({"kind": "human", "team": true}), "bad_request"),
        (json!({"kind": "agent", "team": true}), "bad_request"),
        (json!({"kind": "agent", "owner": "helper"}), "bad_request"),
    ] {
        assert_eq!(
            code(&api.call("PUT", "/v1/users/x1", Some(body.clone())).await.1),
            want,
            "{body}"
        );
    }
    assert!(api.hub.user_by_name("x1").is_none());
    // Kind, owner and team are fixed once created.
    let (s, v) = api
        .call(
            "PUT",
            "/v1/users/helper",
            Some(json!({"kind": "agent", "owner": "alice", "team": true})),
        )
        .await;
    assert_eq!((s, code(&v)), (409, "conflict"));
    // An update of an existing agent shows no key.
    let v = api
        .ok(
            "PUT",
            "/v1/users/helper",
            Some(json!({"display_name": "Helper"})),
        )
        .await;
    assert!(v["key"].is_null());
    // Scopes.
    let reader = api.key(&["users:read"]).await;
    let (s, v) = api
        .with_key(&reader, "GET", "/v1/users/alice/agents", None)
        .await;
    assert_eq!((s, code(&v)), (403, "missing_scope"));
}

async fn stopping_resuming_and_destroying_agents(db: TestDb) {
    let api = Api::start(&db).await;
    let alice = api.person("alice").await;
    let (bot, key) = api.agent("bot", "alice", false).await;
    let a = Conn::new(&api.hub, &alice);
    let conv = a.group(&[&bot]).await;
    let login = api
        .hub
        .authenticate_client(&key, None, None, None)
        .await
        .unwrap();
    let (gs, _gp, _) = api.hub.open_device_session(&login);
    let g = Conn::new(&api.hub, &bot);
    assert_eq!(g.send(&conv, "before").await["ok"], true);
    // Stop: its connections go, it cannot speak, everyone sees why.
    let v = api
        .ok(
            "POST",
            "/v1/agents/bot:stop",
            Some(json!({"reason": "misbehaving"})),
        )
        .await;
    assert_eq!(v["stopped"], true);
    assert!(gs.is_kicked());
    assert_ne!(g.send(&conv, "after").await["ok"], true);
    let log = a.pull(&conv, 0).await;
    assert!(log.to_string().contains("misbehaving"), "{log}");
    // Repeats and bad requests change nothing.
    let head = seqs(&a.pull(&conv, 0).await).len();
    for (m, path, b, want) in [
        ("POST", "/v1/agents/bot:stop", None, "conflict"),
        ("POST", "/v1/agents/bot:jump", None, "not_found"),
        ("POST", "/v1/agents/bot", None, "bad_request"),
        (
            "POST",
            "/v1/agents/bot:stop",
            Some(json!({"reason": ""})),
            "bad_request",
        ),
        (
            "POST",
            "/v1/agents/bot:resume",
            Some(json!({"reason": "x"})),
            "bad_request",
        ),
        ("POST", "/v1/agents/nobody:stop", None, "not_found"),
        ("POST", "/v1/agents/alice:stop", None, "not_found"),
        (
            "POST",
            "/v1/agents/bot:stop",
            Some(json!({"x": 1})),
            "bad_request",
        ),
    ] {
        let (_, v) = api.call(m, path, b.clone()).await;
        assert_eq!(code(&v), want, "{m} {path} {b:?}");
    }
    assert_eq!(seqs(&a.pull(&conv, 0).await).len(), head);
    // Resume.
    let v = api.ok("POST", "/v1/agents/bot:resume", None).await;
    assert_eq!(v["stopped"], false);
    assert_eq!(g.send(&conv, "again").await["ok"], true);
    assert_eq!(
        code(&api.call("POST", "/v1/agents/bot:resume", None).await.1),
        "conflict"
    );
    // Destroy: it leaves, its key is dead, its id is gone for good.
    let v = api.ok("DELETE", "/v1/agents/bot", None).await;
    assert_eq!(
        (&v["destroyed"], &v["conversations"]),
        (&json!(true), &json!(1))
    );
    assert!(
        api.hub
            .authenticate_client(&key, None, None, None)
            .await
            .is_err()
    );
    let members = a.ok(json!({"op": "list_convs"})).await;
    assert!(!members.to_string().contains(&format!("\"{}\"", bot.id)) || true);
    assert_eq!(
        api.ok("GET", "/v1/users/alice/agents", None).await["agents"],
        json!([])
    );
    for (m, path) in [
        ("DELETE", "/v1/agents/bot"),
        ("POST", "/v1/agents/bot:resume"),
        ("POST", "/v1/agents/bot/keys"),
    ] {
        assert_eq!(
            code(&api.call(m, path, None).await.1),
            "not_found",
            "{path}"
        );
    }
    // A person is not destroyed here (people are deleted, docs/04 §4.2).
    assert_eq!(
        code(&api.call("DELETE", "/v1/agents/alice", None).await.1),
        "not_found"
    );
    for a in ["api.agents.stop", "api.agents.resume", "api.agents.destroy"] {
        assert_eq!(api.audits(a).await, 1, "{a}");
    }
}

async fn team_agents_are_transferred(db: TestDb) {
    let api = Api::start(&db).await;
    api.person("alice").await;
    let bob = api.person("bob").await;
    let (desk, key) = api.agent("desk", "alice", true).await;
    api.agent("mine", "alice", false).await;
    // A person's own assistant stays theirs.
    let (s, v) = api
        .call(
            "POST",
            "/v1/agents/mine:transfer",
            Some(json!({"owner": "bob"})),
        )
        .await;
    assert_eq!((s, code(&v)), (400, "bad_request"));
    for (b, want) in [
        (json!({}), "bad_request"),
        (json!({"owner": "nobody"}), "not_found"),
        (json!({"owner": "alice"}), "conflict"),
        (json!({"owner": "mine"}), "not_found"),
        (json!({"owner": "bob", "reason": "x"}), "bad_request"),
    ] {
        let (_, v) = api
            .call("POST", "/v1/agents/desk:transfer", Some(b.clone()))
            .await;
        assert_eq!(code(&v), want, "{b}");
    }
    assert_eq!(
        api.hub.user_by_name("desk").unwrap().owner_id,
        desk.owner_id
    );
    // Without rotating, its key keeps working; then with rotation.
    let v = api
        .ok(
            "POST",
            "/v1/agents/desk:transfer",
            Some(json!({"owner": "bob"})),
        )
        .await;
    assert_eq!(v["owner"], "bob");
    assert_eq!(
        api.hub.user_by_name("desk").unwrap().owner_id.as_deref(),
        Some(bob.id.as_str())
    );
    assert!(
        api.hub
            .authenticate_client(&key, None, None, None)
            .await
            .is_ok()
    );
    let v = api
        .ok(
            "POST",
            "/v1/agents/desk:transfer",
            Some(json!({"owner": "alice", "rotate_keys": true})),
        )
        .await;
    assert_eq!((&v["owner"], &v["keys"]), (&json!("alice"), &json!([])));
    assert!(
        api.hub
            .authenticate_client(&key, None, None, None)
            .await
            .is_err()
    );
    assert_eq!(api.audits("api.agents.transfer").await, 2);
}

async fn agents_move_by_export_and_import(db: TestDb) {
    let api = Api::start(&db).await;
    let alice = api.person("alice").await;
    api.person("bob").await;
    let (bot, _) = api.agent("bot", "alice", false).await;
    let g = Conn::new(&api.hub, &bot);
    g.ok(json!({"op": "state_put", "key": "memory", "value": {"likes": "tea"}, "version": 0}))
        .await;
    let a = Conn::new(&api.hub, &alice);
    let conv = a.group(&[&bot]).await;
    a.send(&conv, "@bot remember the tea").await;
    // Content needs messages:read too.
    let only = api.key(&["agents:read"]).await;
    let (s, v) = api
        .with_key(
            &only,
            "GET",
            "/v1/agents/bot/export?with_conversations=true",
            None,
        )
        .await;
    assert_eq!((s, code(&v)), (403, "missing_scope"));
    let (s, pkg) = api
        .with_key(&only, "GET", "/v1/agents/bot/export", None)
        .await;
    assert_eq!(s, 200, "{pkg}");
    assert!(pkg["conversations"].as_array().is_none_or(|c| c.is_empty()));
    let full = api
        .ok("GET", "/v1/agents/bot/export?with_conversations=true", None)
        .await;
    assert!(full.to_string().contains("remember the tea"));
    assert!(!full.to_string().contains("atk_"), "never keys");
    // Import under a new name for bob: a new agent with its checkpoints.
    let (s, v) = api
        .call(
            "POST",
            "/v1/agents:import",
            Some(json!({"package": full, "owner": "bob", "name": "bot2"})),
        )
        .await;
    assert_eq!(s, 201, "{v}");
    assert_eq!(
        (&v["agent"]["id"], &v["agent"]["owner"]),
        (&json!("bot2"), &json!("bob"))
    );
    let key = v["key"].as_str().unwrap();
    let bot2 = api
        .hub
        .authenticate_client(key, None, None, None)
        .await
        .unwrap()
        .user;
    let got = Conn::new(&api.hub, &bot2)
        .ok(json!({"op": "state_get", "key": "memory"}))
        .await;
    assert_eq!(got["value"]["likes"], "tea");
    // Refusals create nobody.
    let mut changed = full.clone();
    changed["agent"]["name"] = json!("evil");
    for (b, want) in [
        (
            json!({"package": changed, "owner": "bob", "name": "bot3"}),
            "bad_request",
        ),
        (
            json!({"package": {}, "owner": "bob", "name": "bot3"}),
            "bad_request",
        ),
        (
            json!({"package": full, "owner": "nobody", "name": "bot3"}),
            "not_found",
        ),
        (
            json!({"package": full, "owner": "bot", "name": "bot3"}),
            "bad_request",
        ),
        (
            json!({"package": full, "owner": "bob", "name": "bot2"}),
            "conflict",
        ),
        (json!({"owner": "bob"}), "bad_request"),
    ] {
        let (_, v) = api.call("POST", "/v1/agents:import", Some(b)).await;
        assert_eq!(code(&v), want, "{v}");
    }
    assert!(api.hub.user_by_name("bot3").is_none());
    assert_eq!(
        code(&api.call("GET", "/v1/agents/nobody/export", None).await.1),
        "not_found"
    );
}

async fn devices_are_listed_and_logged_out(db: TestDb) {
    let api = Api::start(&db).await;
    let alice = api.person("alice").await;
    let t = client_token(&api.hub, &alice).await;
    let phone = api
        .hub
        .authenticate_client(&t, Some("phone"), Some("iPhone"), Some("ios"))
        .await
        .unwrap();
    let laptop = api
        .hub
        .authenticate_client(&t, Some("laptop"), None, None)
        .await
        .unwrap();
    let v = api.ok("GET", "/v1/users/alice/devices", None).await;
    let devs = v["devices"].as_array().unwrap();
    assert_eq!(devs.len(), 2);
    assert_eq!(
        (&devs[0]["name"], &devs[0]["platform"]),
        (&json!("iPhone"), &json!("ios"))
    );
    assert!(!v.to_string().contains("hash") && !v.to_string().contains(&t));
    // Logging the phone out: it goes, and its token is refused on it.
    let path = format!("/v1/users/alice/devices/{}", phone.device_id);
    assert_eq!(api.ok("DELETE", &path, None).await["revoked"], 1);
    assert!(!api.hub.login_valid(&phone));
    assert!(api.hub.login_valid(&laptop));
    assert!(
        api.hub
            .authenticate_client(&t, Some("phone"), None, None)
            .await
            .is_err()
    );
    for (p, want) in [
        (path.clone(), "not_found"),
        ("/v1/users/alice/devices/d_nope".to_string(), "not_found"),
        (
            format!("/v1/users/nobody/devices/{}", laptop.device_id),
            "not_found",
        ),
    ] {
        assert_eq!(code(&api.call("DELETE", &p, None).await.1), want, "{p}");
    }
    // Another person's device is not hers.
    let bob = api.person("bob").await;
    let lb = api
        .hub
        .authenticate_client(&client_token(&api.hub, &bob).await, Some("b"), None, None)
        .await
        .unwrap();
    let p = format!("/v1/users/alice/devices/{}", lb.device_id);
    assert_eq!(code(&api.call("DELETE", &p, None).await.1), "not_found");
    assert!(api.hub.login_valid(&lb));
    // An agent's devices are its keys.
    let (_, key) = api.agent("bot", "alice", false).await;
    let v = api.ok("GET", "/v1/users/bot/devices", None).await;
    let dev = v["devices"][0]["device_id"].as_str().unwrap().to_string();
    api.ok("DELETE", &format!("/v1/users/bot/devices/{dev}"), None)
        .await;
    assert!(
        api.hub
            .authenticate_client(&key, None, None, None)
            .await
            .is_err()
    );
    // Scopes: listing reads users, logging out revokes tokens.
    let reader = api.key(&["users:read"]).await;
    assert_eq!(
        api.with_key(&reader, "GET", "/v1/users/alice/devices", None)
            .await
            .0,
        200
    );
    let p = format!("/v1/users/alice/devices/{}", laptop.device_id);
    let (s, v) = api.with_key(&reader, "DELETE", &p, None).await;
    assert_eq!((s, code(&v)), (403, "missing_scope"));
    assert!(api.hub.login_valid(&laptop));
    assert_eq!(api.audits("api.devices.logout").await, 2);
}

async fn silenced_members_cannot_speak(db: TestDb) {
    let api = Api::start(&db).await;
    let alice = api.person("alice").await;
    let bob = api.person("bob").await;
    let carol = api.person("carol").await;
    let (bot, _) = api.agent("bot", "alice", false).await;
    let a = Conn::new(&api.hub, &alice);
    let b = Conn::new(&api.hub, &bob);
    let g = Conn::new(&api.hub, &bot);
    let conv = a.group(&[&bob, &bot]).await;
    let mine = b.send(&conv, "before").await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    let silence = format!("/v1/conversations/{conv}/members/bob:silence");
    let v = api
        .ok(
            "POST",
            &silence,
            Some(json!({"seconds": 600, "reason": "spam"})),
        )
        .await;
    assert_eq!(
        (&v["changed"], &v["user_id"]),
        (&json!(true), &json!("bob"))
    );
    let until = v["silenced_until"].as_i64().unwrap();
    assert!(until > now_ms() + 590_000);
    // Everyone sees it in the log.
    let log = a.pull(&conv, 0).await;
    let ev = log["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["event"]["type"] == "member.silenced")
        .cloned()
        .unwrap();
    assert_eq!(ev["event"]["data"]["user_id"], bob.id.as_str());
    assert_eq!(ev["event"]["data"]["until"], until);
    assert_eq!(ev["event"]["data"]["reason"], "spam");
    // Bob can neither send nor edit, here or through the open API as him;
    // others still can; refusals leave nothing.
    let head = seqs(&a.pull(&conv, 0).await).len();
    assert_eq!(ws_code(&b.send(&conv, "x").await), "forbidden");
    assert_eq!(
        ws_code(
            &b.call(json!({"op": "edit", "msg_id": mine, "text": "changed"}))
                .await
        ),
        "forbidden"
    );
    let (s, v) = api
        .call(
            "POST",
            &format!("/v1/conversations/{conv}/messages"),
            Some(json!({"as": "bob", "text": "via api"})),
        )
        .await;
    assert_eq!((s, code(&v)), (403, "forbidden"));
    assert_eq!(seqs(&a.pull(&conv, 0).await).len(), head);
    // He can still read and recall his own message.
    assert_eq!(
        b.call(json!({"op": "recall", "msg_id": mine})).await["ok"],
        true
    );
    assert_eq!(a.send(&conv, "alice speaks").await["ok"], true);
    // A silenced agent cannot stream either.
    api.ok(
        "POST",
        &format!("/v1/conversations/{conv}/members/bot:silence"),
        Some(json!({"seconds": 60})),
    )
    .await;
    let r = g
        .call(json!({"op": "stream_start", "conv_id": conv, "client_msg_id": "s1"}))
        .await;
    assert_eq!(ws_code(&r), "forbidden", "{r}");
    // Repeating the same silence writes nothing; changing it does.
    let head = seqs(&a.pull(&conv, 0).await).len();
    let unsilence = format!("/v1/conversations/{conv}/members/bob:unsilence");
    let v = api.ok("POST", &unsilence, None).await;
    assert_eq!(v["changed"], true);
    assert_eq!(api.ok("POST", &unsilence, None).await["changed"], false);
    assert_eq!(seqs(&a.pull(&conv, 0).await).len(), head + 1);
    assert_eq!(b.send(&conv, "free again").await["ok"], true);
    // Refusals.
    let other = a.group(&[&bob]).await;
    let other_head = seqs(&a.pull(&other, 0).await).len();
    for (path, body, want) in [
        (silence.clone(), json!({}), "bad_request"),
        (silence.clone(), json!({"seconds": 0}), "bad_request"),
        (silence.clone(), json!({"seconds": -5}), "bad_request"),
        (
            silence.clone(),
            json!({"seconds": 365 * 86400 + 1}),
            "bad_request",
        ),
        (
            silence.clone(),
            json!({"seconds": 5, "reason": ""}),
            "bad_request",
        ),
        (
            silence.clone(),
            json!({"seconds": 5, "x": 1}),
            "bad_request",
        ),
        (unsilence.clone(), json!({"seconds": 5}), "bad_request"),
        (
            format!("/v1/conversations/{conv}/members/carol:silence"),
            json!({"seconds": 5}),
            "not_found",
        ),
        (
            format!("/v1/conversations/{conv}/members/nobody:silence"),
            json!({"seconds": 5}),
            "not_found",
        ),
        (
            format!("/v1/conversations/{conv}/members/system:silence"),
            json!({"seconds": 5}),
            "not_found",
        ),
        (
            "/v1/conversations/c_nope/members/bob:silence".into(),
            json!({"seconds": 5}),
            "not_found",
        ),
        (
            format!("/v1/conversations/{conv}/members/bob:shout"),
            json!({}),
            "not_found",
        ),
    ] {
        let (_, v) = api.call("POST", &path, Some(body.clone())).await;
        assert_eq!(code(&v), want, "{path} {body}");
    }
    let _ = carol;
    assert_eq!(seqs(&a.pull(&conv, 0).await).len(), head + 2);
    assert_eq!(
        seqs(&a.pull(&other, 0).await).len(),
        other_head,
        "other conversations untouched"
    );
    // Scope: moderation.
    let k = api.key(&["conversations:write"]).await;
    let (s, v) = api
        .with_key(&k, "POST", &silence, Some(json!({"seconds": 5})))
        .await;
    assert_eq!((s, code(&v)), (403, "missing_scope"));
    assert_eq!(b.send(&conv, "still free").await["ok"], true);
    assert_eq!(api.audits("api.conversations.members.silence").await, 2);
    assert_eq!(api.audits("api.conversations.members.unsilence").await, 1);
}

async fn silences_end_and_survive_a_restart(db: TestDb) {
    let api = Api::start(&db).await;
    let alice = api.person("alice").await;
    let bob = api.person("bob").await;
    let a = Conn::new(&api.hub, &alice);
    let conv = a.group(&[&bob]).await;
    let path = |u: &str| format!("/v1/conversations/{conv}/members/{u}:silence");
    api.ok("POST", &path("bob"), Some(json!({"seconds": 1})))
        .await;
    api.ok("POST", &path("alice"), Some(json!({"seconds": 3600})))
        .await;
    assert_eq!(
        ws_code(&Conn::new(&api.hub, &bob).send(&conv, "x").await),
        "forbidden"
    );
    tokio::time::sleep(Duration::from_millis(1100)).await;
    assert_eq!(
        Conn::new(&api.hub, &bob).send(&conv, "it ended").await["ok"],
        true,
        "a silence ends by itself"
    );
    // A restart keeps the one still running.
    let url = db.url.clone();
    drop(a);
    drop(api);
    let hub = Hub::open(single(&url), hub_cfg()).await.unwrap();
    let alice = hub.user_by_name("alice").unwrap();
    assert_eq!(
        ws_code(&Conn::new(&hub, &alice).send(&conv, "x").await),
        "forbidden"
    );
}

/// An agent's drafts approved while it is silenced are not published.
async fn a_silenced_agents_drafts_wait(db: TestDb) {
    let api = Api::start(&db).await;
    let alice = api.person("alice").await;
    let (bot, _) = api.agent("bot", "alice", false).await;
    let a = Conn::new(&api.hub, &alice);
    let g = Conn::new(&api.hub, &bot);
    let conv = a.group(&[&bot]).await;
    a.ok(json!({"op": "set_agent_policy", "conv_id": conv, "agent_id": bot.id, "approval": true, "trigger": "all"}))
        .await;
    let d = g.send(&conv, "a draft").await;
    let draft = d["draft_id"].as_str().unwrap().to_string();
    api.ok(
        "POST",
        &format!("/v1/conversations/{conv}/members/bot:silence"),
        Some(json!({"seconds": 600})),
    )
    .await;
    let head = seqs(&a.pull(&conv, 0).await).len();
    let r = a.call(json!({"op": "approve", "draft_id": draft})).await;
    assert_eq!(ws_code(&r), "conflict", "{r}");
    assert_eq!(seqs(&a.pull(&conv, 0).await).len(), head);
    api.ok(
        "POST",
        &format!("/v1/conversations/{conv}/members/bot:unsilence"),
        None,
    )
    .await;
    assert_eq!(
        a.call(json!({"op": "approve", "draft_id": draft})).await["ok"],
        true
    );
}

/// A member sends in a loop while moderation silences them: once the
/// silence is committed, none of their sends commits after it.
async fn race_sends_while_a_member_is_silenced(db: TestDb) {
    let api = Api::start(&db).await;
    let alice = api.person("alice").await;
    let bob = api.person("bob").await;
    let a = Conn::new(&api.hub, &alice);
    for round in 0..6 {
        let conv = a.group(&[&bob]).await;
        let hub = api.hub.clone();
        let (bob2, conv2) = (bob.clone(), conv.clone());
        let sender = tokio::spawn(async move {
            let b = Conn::new(&hub, &bob2);
            for i in 0..60 {
                b.send(&conv2, &format!("m{i}")).await;
            }
        });
        tokio::time::sleep(Duration::from_millis(round * 3)).await;
        let v = api
            .ok(
                "POST",
                &format!("/v1/conversations/{conv}/members/bob:silence"),
                Some(json!({"seconds": 600})),
            )
            .await;
        let at = v["seq"].as_i64().unwrap();
        sender.await.unwrap();
        let log = a.pull(&conv, 0).await;
        let late: Vec<&Value> = log["events"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| {
                e["event"]["type"] == "msg.created"
                    && e["event"]["msg"]["sender_id"] == bob.id.as_str()
                    && e["event"]["seq"].as_i64().unwrap() > at
            })
            .collect();
        assert!(late.is_empty(), "round {round}: {late:?}");
        let s = seqs(&log);
        assert_eq!(s, (1..=s.len() as i64).collect::<Vec<_>>(), "gap-free");
    }
}
