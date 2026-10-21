//! What people may do in their client (docs/13 §4.1): the app's defaults
//! (`[clients.allow]`), each person's overrides set by the developer
//! (`PATCH /v1/users/{id}` `permissions`), what each switch refuses (in
//! the client and through the open API as that person), what is never
//! refused (an owner's interventions), the time windows for editing and
//! recalling, restarts, and creating agents racing a revocation.

mod common;

use std::net::SocketAddr;
use std::time::Duration;

use agentim_core::config::{AllowConfig, Config};
use agentim_core::hub::Hub;
use agentim_core::model::*;
use agentim_core::server::{AppState, router};
use common::*;
use serde_json::{Value, json};

backend_tests!(
    defaults_come_from_the_configuration,
    each_switch_refuses_what_it_names,
    overrides_per_person_and_back_to_the_defaults,
    taking_agents_away_keeps_the_interventions,
    edit_and_recall_windows,
    permissions_survive_a_restart,
    race_creating_agents_while_it_is_revoked,
);

/// A test configuration with the built-in client defaults.
fn single(url: &str) -> Config {
    let mut c = common::single(url);
    c.clients.allow = AllowConfig::default();
    c
}

fn code(r: &Value) -> &str {
    r["error"]["code"].as_str().unwrap_or("")
}

async fn api(hub: &Hub) -> (String, String) {
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
    (format!("http://{addr}"), key)
}

async fn http(
    base: &str,
    key: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let mut r = reqwest::Client::new()
        .request(method.parse().unwrap(), format!("{base}{path}"))
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

fn with_allow(url: &str, f: impl FnOnce(&mut AllowConfig)) -> Config {
    let mut c = single(url);
    f(&mut c.clients.allow);
    c
}

async fn defaults_come_from_the_configuration(db: TestDb) {
    // The built-in defaults: everything but creating agents.
    let d = AllowConfig::default();
    assert!(d.create_groups && d.create_direct && d.add_members && d.find_users);
    assert!(!d.create_agents);
    assert_eq!((d.edit_minutes, d.recall_minutes), (None, None));
    let hub = open(with_allow(&db.url, |a| {
        a.create_groups = false;
        a.edit_minutes = Some(15);
    }))
    .await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let a = Conn::new(&hub, &alice);
    // hello tells the app what to show.
    let p = &a.ok(json!({"op": "hello"})).await["me"]["permissions"];
    assert_eq!(
        p,
        &json!({"create_groups": false, "create_direct": true, "add_members": true,
                "find_users": true, "create_agents": false, "edit_minutes": 15,
                "recall_minutes": null})
    );
    // Agents are not told (their owner's permissions bound them).
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    let g = Conn::new(&hub, &bot);
    assert!(g.ok(json!({"op": "hello"})).await["me"]["permissions"].is_null());
    // A configuration out of range is refused at start-up.
    for f in [
        |a: &mut AllowConfig| a.edit_minutes = Some(-1),
        |a: &mut AllowConfig| a.recall_minutes = Some(525_601),
    ] {
        let c = with_allow(&new_db(Backend::Sqlite).await.unwrap().url, f);
        assert!(c.validate().is_err());
    }
}

async fn each_switch_refuses_what_it_names(db: TestDb) {
    let hub = open(with_allow(&db.url, |a| {
        a.create_groups = false;
        a.create_direct = false;
        a.add_members = false;
        a.find_users = false;
    }))
    .await;
    let (base, key) = api(&hub).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let carol = user(&hub, "carol", UserKind::Human, None).await;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    let a = Conn::new(&hub, &alice);
    let g = Conn::new(&hub, &bot);
    let before = hub.audit_log(1000).await.unwrap().len();
    for (who, req) in [
        (
            &a,
            json!({"op": "create_conv", "kind": "group", "title": "t", "members": [bob.id]}),
        ),
        (
            &a,
            json!({"op": "create_conv", "kind": "dm", "title": "d", "members": [bob.id]}),
        ),
        (&a, json!({"op": "find_user", "name": "bob"})),
        (&a, json!({"op": "create_agent", "name": "helper"})),
        (&a, json!({"op": "agent_key", "agent_id": bot.id})),
        (
            &a,
            json!({"op": "destroy_agent", "agent_id": bot.id, "confirm": "bot"}),
        ),
        // An agent is bound by its owner's permissions.
        (&g, json!({"op": "find_user", "name": "bob"})),
        (
            &g,
            json!({"op": "create_conv", "kind": "group", "title": "t", "members": [alice.id]}),
        ),
    ] {
        let r = who.call(req.clone()).await;
        assert_eq!(code(&r), "forbidden", "{req}: {r}");
        assert!(
            r["error"]["message"]
                .as_str()
                .unwrap()
                .contains("the app does not allow")
        );
    }
    assert!(hub.user_by_name("helper").is_none());
    assert_eq!(hub.audit_log(1000).await.unwrap().len(), before, "no trace");
    // The open API acting as alice follows her permissions...
    let (s, v) = http(
        &base,
        &key,
        "POST",
        "/v1/conversations",
        Some(json!({"as": "alice", "kind": "group", "title": "t"})),
    )
    .await;
    assert_eq!((s, v["code"].as_str()), (403, Some("forbidden")));
    // ...the system does not.
    let (s, c) = http(
        &base,
        &key,
        "POST",
        "/v1/conversations",
        Some(json!({"as": "system", "owner": "alice", "kind": "group", "title": "t", "members": ["bob"]})),
    )
    .await;
    assert_eq!(s, 201, "{c}");
    let conv = c["id"].as_str().unwrap();
    // Adding members (alice owns the group the system made for her).
    let r = a
        .call(json!({"op": "add_member", "conv_id": conv, "user_id": carol.id}))
        .await;
    assert_eq!(code(&r), "forbidden", "{r}");
    let (s, _) = http(
        &base,
        &key,
        "POST",
        &format!("/v1/conversations/{conv}/members"),
        Some(json!({"as": "system", "users": ["carol"]})),
    )
    .await;
    assert_eq!(s, 200);
    // Talking is never limited.
    assert_eq!(a.send(conv, "hi").await["ok"], true);
}

async fn overrides_per_person_and_back_to_the_defaults(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let (base, key) = api(&hub).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let mut a = Conn::new(&hub, &alice);
    let b = Conn::new(&hub, &bob);
    a.drain();
    // Alice may create agents, bob may not.
    let (s, v) = http(
        &base,
        &key,
        "PATCH",
        "/v1/users/alice",
        Some(json!({"permissions": {"create_agents": true, "recall_minutes": 2}})),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["permissions"]["create_agents"], true);
    assert_eq!(
        v["permission_overrides"],
        json!({"create_agents": true, "recall_minutes": 2})
    );
    // Her connected devices are told.
    let told = a.drain();
    assert!(
        told.iter()
            .any(|f| f["type"] == "user.updated" && f["permissions"]["create_agents"] == true),
        "{told:?}"
    );
    assert_eq!(
        a.call(json!({"op": "create_agent", "name": "helper"}))
            .await["ok"],
        true
    );
    assert_eq!(
        code(&b.call(json!({"op": "create_agent", "name": "x"})).await),
        "forbidden"
    );
    // One key back to the default, then all.
    let (_, v) = http(
        &base,
        &key,
        "PATCH",
        "/v1/users/alice",
        Some(json!({"permissions": {"create_agents": null}})),
    )
    .await;
    assert_eq!(v["permission_overrides"], json!({"recall_minutes": 2}));
    assert_eq!(
        code(&a.call(json!({"op": "create_agent", "name": "y"})).await),
        "forbidden"
    );
    let (_, v) = http(
        &base,
        &key,
        "PATCH",
        "/v1/users/alice",
        Some(json!({"permissions": null})),
    )
    .await;
    assert_eq!(v["permission_overrides"], json!({}));
    assert_eq!(v["permissions"]["recall_minutes"], Value::Null);
    // Refusals change nothing.
    for body in [
        json!({"permissions": {"fly": true}}),
        json!({"permissions": {"create_agents": "yes"}}),
        json!({"permissions": {"edit_minutes": -1}}),
        json!({"permissions": {"edit_minutes": 525_601}}),
        json!({"permissions": [1]}),
        json!({"permissions": {"create_agents": true}, "display_name": ""}),
    ] {
        let (s, v) = http(&base, &key, "PATCH", "/v1/users/alice", Some(body.clone())).await;
        assert_eq!(
            (s, v["code"].as_str()),
            (400, Some("bad_request")),
            "{body}"
        );
    }
    let (_, v) = http(&base, &key, "GET", "/v1/users/alice", None).await;
    assert_eq!(v["permission_overrides"], json!({}));
    // Agents have none of their own.
    let (s, _) = http(
        &base,
        &key,
        "PATCH",
        "/v1/users/helper",
        Some(json!({"permissions": {"create_groups": false}})),
    )
    .await;
    assert_eq!(s, 400);
    // Only users:write may change them.
    let reader = hub
        .create_api_key("r", &["users:read".to_string()], None, None)
        .await
        .unwrap()
        .0;
    let (s, _) = http(
        &base,
        &reader,
        "PATCH",
        "/v1/users/alice",
        Some(json!({"permissions": {"create_agents": true}})),
    )
    .await;
    assert_eq!(s, 403);
}

async fn taking_agents_away_keeps_the_interventions(db: TestDb) {
    let hub = open(with_allow(&db.url, |a| a.create_agents = true)).await;
    let (base, key) = api(&hub).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let a = Conn::new(&hub, &alice);
    let made = a.ok(json!({"op": "create_agent", "name": "helper"})).await;
    let id = made["agent"]["id"].as_str().unwrap().to_string();
    let device = made["device_id"].as_str().unwrap().to_string();
    let helper = hub.user_by_name("helper").unwrap();
    let conv = a.group(&[&helper]).await;
    a.ok(json!({"op": "set_agent_policy", "conv_id": conv, "agent_id": id, "approval": true, "trigger": "all"}))
        .await;
    // The developer takes it away from alice.
    http(
        &base,
        &key,
        "PATCH",
        "/v1/users/alice",
        Some(json!({"permissions": {"create_agents": false}})),
    )
    .await;
    // Her agent keeps working and stays hers...
    let g = Conn::new(&hub, &helper);
    let draft = g.send(&conv, "a draft").await["draft_id"]
        .as_str()
        .unwrap()
        .to_string();
    // ...she can no longer create, rotate, revoke keys or destroy...
    for req in [
        json!({"op": "create_agent", "name": "another"}),
        json!({"op": "agent_key", "agent_id": id}),
        json!({"op": "revoke_agent_key", "agent_id": id, "device_id": device}),
        json!({"op": "destroy_agent", "agent_id": id, "confirm": "helper"}),
    ] {
        assert_eq!(code(&a.call(req.clone()).await), "forbidden", "{req}");
    }
    assert_eq!(hub.devices(&helper.id).await.unwrap().len(), 1);
    // ...but every intervention stays: list, approve, stop, resume.
    assert_eq!(
        a.ok(json!({"op": "list_agents"})).await["agents"][0]["agent"]["id"],
        id.as_str()
    );
    a.ok(json!({"op": "approve", "draft_id": draft})).await;
    a.ok(json!({"op": "stop_agent", "agent_id": id})).await;
    a.ok(json!({"op": "resume_agent", "agent_id": id})).await;
    // The developer still manages it.
    let (s, _) = http(&base, &key, "POST", "/v1/agents/helper/keys", None).await;
    assert_eq!(s, 201);
}

async fn edit_and_recall_windows(db: TestDb) {
    let hub = open(with_allow(&db.url, |a| {
        a.edit_minutes = Some(10);
        a.recall_minutes = Some(2);
    }))
    .await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let a = Conn::new(&hub, &alice);
    let b = Conn::new(&hub, &bob);
    let conv = a.group(&[&bob]).await;
    let id = |r: Value| r["msg_id"].as_str().unwrap().to_string();
    let fresh = id(b.send(&conv, "fresh").await);
    let old = id(b.send(&conv, "five minutes old").await);
    let older = id(b.send(&conv, "an hour old").await);
    let age = |m: &str, minutes: i64| {
        let (url, m) = (db.url.clone(), m.to_string());
        async move {
            use sqlx::Connection;
            sqlx::any::install_default_drivers();
            let mut c = sqlx::AnyConnection::connect(&url.replace("mariadb://", "mysql://"))
                .await
                .unwrap();
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "UPDATE messages SET created_at = {} WHERE msg_id = '{m}'",
                now_ms() - minutes * 60_000
            )))
            .execute(&mut c)
            .await
            .unwrap();
        }
    };
    age(&old, 5).await;
    age(&older, 60).await;
    // The writer reads the message from the database; a restart drops any
    // cached copy.
    drop((a, b, hub));
    let hub = open(with_allow(&db.url, |a| {
        a.edit_minutes = Some(10);
        a.recall_minutes = Some(2);
    }))
    .await;
    let a = Conn::new(&hub, &alice);
    let b = Conn::new(&hub, &bob);
    let head = seqs(&a.pull(&conv, 0).await).len();
    let edit = |m: &str| json!({"op": "edit", "msg_id": m, "text": "changed"});
    let recall = |m: &str| json!({"op": "recall", "msg_id": m});
    assert_eq!(
        code(&b.call(recall(&old)).await),
        "forbidden",
        "past 2 minutes"
    );
    assert_eq!(
        code(&b.call(edit(&older)).await),
        "forbidden",
        "past 10 minutes"
    );
    assert_eq!(seqs(&a.pull(&conv, 0).await).len(), head, "no trace");
    assert_eq!(b.call(edit(&old)).await["ok"], true, "within 10 minutes");
    assert_eq!(b.call(recall(&fresh)).await["ok"], true, "within 2 minutes");
    // The group's owner recalls others' messages whatever their age.
    assert_eq!(a.call(recall(&older)).await["ok"], true);
}

async fn permissions_survive_a_restart(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    hub.set_permissions(&alice, &json!({"create_agents": true, "find_users": false}))
        .await
        .unwrap();
    drop(hub);
    let hub = open(single(&db.url)).await;
    let p = hub.permissions(&alice);
    assert!(p.create_agents && !p.find_users && p.create_groups);
    let a = Conn::new(&hub, &alice);
    assert_eq!(
        code(&a.call(json!({"op": "find_user", "name": "x"})).await),
        "forbidden"
    );
    assert_eq!(
        a.call(json!({"op": "create_agent", "name": "helper"}))
            .await["ok"],
        true
    );
}

/// Committed agents of a person, read from the database.
async fn agents_in_db(db: &TestDb, owner: &str) -> i64 {
    use sqlx::{Connection, Row};
    sqlx::any::install_default_drivers();
    let mut c = sqlx::AnyConnection::connect(&db.url.replace("mariadb://", "mysql://"))
        .await
        .unwrap();
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT COUNT(*) FROM users WHERE owner_id = '{owner}' AND kind = 'agent'"
    )))
    .fetch_one(&mut c)
    .await
    .unwrap()
    .get::<i64, _>(0)
}

/// Alice keeps creating agents in her client while the developer takes
/// that away: once the revocation returned, no agent of hers appears.
async fn race_creating_agents_while_it_is_revoked(db: TestDb) {
    let mut cfg = single(&db.url);
    cfg.agents.max_per_owner = 1000;
    let hub = open(cfg).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    for round in 0..8 {
        hub.set_permissions(&alice, &json!({"create_agents": true}))
            .await
            .unwrap();
        let mut tasks = vec![];
        for i in 0..10 {
            let (h, u) = (hub.clone(), alice.clone());
            tasks.push(tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(i)).await;
                Conn::new(&h, &u)
                    .call(json!({"op": "create_agent", "name": format!("a{round}_{i}")}))
                    .await
            }));
        }
        tokio::time::sleep(Duration::from_millis(round * 2)).await;
        hub.set_permissions(&alice, &json!({"create_agents": false}))
            .await
            .unwrap();
        let committed = agents_in_db(&db, &alice.id).await;
        for t in tasks {
            let r = t.await.unwrap();
            assert!(r["ok"] == true || code(&r) == "forbidden", "{r}");
        }
        assert_eq!(
            agents_in_db(&db, &alice.id).await,
            committed,
            "round {round}: an agent was created after the revocation"
        );
    }
}
