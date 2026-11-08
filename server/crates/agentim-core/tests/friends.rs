//! Friends (docs/en/design/agents.md): requests and answers, who may add whom (people,
//! the app's agents, a person's own assistant), blocks, the app's "friends
//! only" setting, the open API, a restart, and races. Every refusal leaves
//! no trace.

mod common;

use agentim_core::hub::Hub;
use agentim_core::model::*;
use common::*;
use serde_json::{Value, json};

backend_tests!(
    requests_answers_and_removals,
    who_may_add_whom,
    blocks_end_friendships,
    the_app_can_require_friends,
    the_backend_imports_friendships,
    friends_survive_a_restart,
    race_requests_both_ways_and_blocks,
);

fn code(v: &Value) -> (&str, &str) {
    (
        v["error"]["code"].as_str().unwrap_or(""),
        v["error"]["rule"].as_str().unwrap_or(""),
    )
}

/// What a refused request could have changed.
async fn trace(hub: &Hub, people: &[&User]) -> String {
    let mut out = String::new();
    for p in people {
        out += &hub.list_friends(p).await.unwrap().to_string();
    }
    out += &hub.audit_log(10_000).await.unwrap().len().to_string();
    out
}

fn ids(v: &Value, list: &str) -> Vec<String> {
    v[list]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["user_id"].as_str().unwrap().to_string())
        .collect()
}

fn events(c: &mut Conn) -> Vec<(String, String)> {
    c.drain()
        .into_iter()
        .filter(|f| f["type"] == "friend")
        .map(|f| {
            (
                f["event"].as_str().unwrap().to_string(),
                f["user_id"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

async fn requests_answers_and_removals(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let carol = user(&hub, "carol", UserKind::Human, None).await;
    let (mut a, mut b) = (Conn::new(&hub, &alice), Conn::new(&hub, &bob));
    // A request with a message: bob is told, it waits for him.
    let r = a
        .ok(json!({"op": "friend_request", "user_id": bob.id, "message": "hi, it's Alice"}))
        .await;
    assert_eq!(
        (r["friends"].clone(), r["requested"].clone()),
        (json!(false), json!(true))
    );
    assert_eq!(events(&mut b), vec![("requested".into(), alice.id.clone())]);
    let l = b.ok(json!({"op": "list_friends"})).await;
    assert_eq!(ids(&l, "incoming"), vec![alice.id.clone()]);
    assert_eq!(l["incoming"][0]["message"], "hi, it's Alice");
    assert_eq!(l["incoming"][0]["name"], "alice");
    assert_eq!(
        ids(&a.ok(json!({"op": "list_friends"})).await, "outgoing"),
        vec![bob.id.clone()]
    );
    // Refusals, none leaving a trace.
    let before = trace(&hub, &[&alice, &bob, &carol]).await;
    for (who, msg, want) in [
        (
            &alice,
            json!({"op": "friend_request", "user_id": alice.id}),
            ("bad_request", "friend.self"),
        ),
        (
            &alice,
            json!({"op": "friend_request", "user_id": "nobody"}),
            ("not_found", "account.unknown_user"),
        ),
        (
            &alice,
            json!({"op": "friend_request", "user_id": bob.id}),
            ("conflict", "friend.pending"),
        ),
        (
            &alice,
            json!({"op": "friend_request", "user_id": carol.id, "message": "x".repeat(501)}),
            ("bad_request", "friend.message"),
        ),
        (
            &alice,
            json!({"op": "friend_accept", "user_id": bob.id}),
            ("not_found", "friend.no_request"),
        ),
        (
            &carol,
            json!({"op": "friend_decline", "user_id": alice.id}),
            ("not_found", "friend.no_request"),
        ),
        (
            &carol,
            json!({"op": "friend_remove", "user_id": alice.id}),
            ("not_found", "friend.not_friends"),
        ),
        (
            &alice,
            json!({"op": "set_friend_policy", "policy": "everyone"}),
            ("bad_request", "friend.policy"),
        ),
    ] {
        let r = Conn::new(&hub, who).call(msg.clone()).await;
        assert_eq!(code(&r), want, "{msg}: {r}");
    }
    assert_eq!(trace(&hub, &[&alice, &bob, &carol]).await, before);
    // 500 characters is fine.
    a.ok(json!({"op": "friend_request", "user_id": carol.id, "message": "x".repeat(500)}))
        .await;
    // Accepted: both are told; both see the friendship.
    a.drain();
    b.ok(json!({"op": "friend_accept", "user_id": alice.id}))
        .await;
    assert_eq!(events(&mut a), vec![("accepted".into(), bob.id.clone())]);
    assert_eq!(events(&mut b), vec![("accepted".into(), alice.id.clone())]);
    assert_eq!(
        ids(&a.ok(json!({"op": "list_friends"})).await, "friends"),
        vec![bob.id.clone()]
    );
    let l = b.ok(json!({"op": "list_friends"})).await;
    assert_eq!(ids(&l, "friends"), vec![alice.id.clone()]);
    assert_eq!(l["incoming"], json!([]));
    // hello lists them, and friends are among the users one sees.
    let h = b.ok(json!({"op": "hello"})).await;
    assert_eq!(ids(&h, "friends"), vec![alice.id.clone()]);
    assert!(
        h["users"]
            .as_array()
            .unwrap()
            .iter()
            .any(|u| u["id"] == alice.id.as_str())
    );
    let r = a
        .call(json!({"op": "friend_request", "user_id": bob.id}))
        .await;
    assert_eq!(code(&r), ("conflict", "friend.already"));
    // Carol declines; Alice is told; she may ask again.
    let c = Conn::new(&hub, &carol);
    c.ok(json!({"op": "friend_decline", "user_id": alice.id}))
        .await;
    assert_eq!(events(&mut a), vec![("declined".into(), carol.id.clone())]);
    a.ok(json!({"op": "friend_request", "user_id": carol.id}))
        .await;
    // Withdrawn by Alice: Carol no longer has it.
    let r = a
        .ok(json!({"op": "friend_remove", "user_id": carol.id}))
        .await;
    assert_eq!(r["withdrawn"], true);
    assert_eq!(
        c.ok(json!({"op": "list_friends"})).await["incoming"],
        json!([])
    );
    // Removed: both told, gone on both sides; the direct chat is not touched.
    let dm = a
        .ok(json!({"op": "create_conv", "kind": "dm", "title": "dm", "members": [bob.id]}))
        .await["conv_id"]
        .as_str()
        .unwrap()
        .to_string();
    a.drain();
    b.drain();
    b.ok(json!({"op": "friend_remove", "user_id": alice.id}))
        .await;
    assert_eq!(events(&mut a), vec![("removed".into(), bob.id.clone())]);
    assert_eq!(events(&mut b), vec![("removed".into(), alice.id.clone())]);
    assert_eq!(
        a.ok(json!({"op": "list_friends"})).await["friends"],
        json!([])
    );
    a.send(&dm, "still here").await;
    let r = b
        .call(json!({"op": "friend_remove", "user_id": alice.id}))
        .await;
    assert_eq!(code(&r), ("not_found", "friend.not_friends"));
    // At most 100 requests waiting at once; one answered makes room.
    let dave = user(&hub, "dave", UserKind::Human, None).await;
    let d = Conn::new(&hub, &dave);
    let mut asked = vec![];
    for i in 0..agentim_core::hub::FRIEND_REQUESTS_MAX {
        let u = user(&hub, &format!("p{i}"), UserKind::Human, None).await;
        d.ok(json!({"op": "friend_request", "user_id": u.id})).await;
        asked.push(u);
    }
    let extra = user(&hub, "extra", UserKind::Human, None).await;
    let before = trace(&hub, &[&dave, &extra]).await;
    let r = d
        .call(json!({"op": "friend_request", "user_id": extra.id}))
        .await;
    assert_eq!(code(&r), ("rate_limited", "friend.requests_limit"));
    assert_eq!(trace(&hub, &[&dave, &extra]).await, before);
    Conn::new(&hub, &asked[0])
        .ok(json!({"op": "friend_decline", "user_id": dave.id}))
        .await;
    d.ok(json!({"op": "friend_request", "user_id": extra.id}))
        .await;
}

async fn who_may_add_whom(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let a = Conn::new(&hub, &alice);
    let b = Conn::new(&hub, &bob);
    // Nobody: refused; anyone: friends at once.
    b.ok(json!({"op": "set_friend_policy", "policy": "nobody"}))
        .await;
    let r = a
        .call(json!({"op": "friend_request", "user_id": bob.id}))
        .await;
    assert_eq!(code(&r), ("forbidden", "friend.nobody"));
    b.ok(json!({"op": "set_friend_policy", "policy": "anyone"}))
        .await;
    assert_eq!(
        b.ok(json!({"op": "list_friends"})).await["friend_policy"],
        "anyone"
    );
    let r = a
        .ok(json!({"op": "friend_request", "user_id": bob.id}))
        .await;
    assert_eq!(r["friends"], true);
    // Agents do not manage friends.
    let mine = a.ok(json!({"op": "create_agent", "name": "helper"})).await;
    let helper = hub.user(mine["agent"]["id"].as_str().unwrap()).unwrap();
    let h = Conn::new(&hub, &helper);
    for msg in [
        json!({"op": "friend_request", "user_id": alice.id}),
        json!({"op": "list_friends"}),
        json!({"op": "set_friend_policy", "policy": "anyone"}),
    ] {
        assert_eq!(
            code(&h.call(msg.clone()).await),
            ("forbidden", "friend.people_only"),
            "{msg}"
        );
    }
    // A person's own assistant: its owner adds it at once, others cannot.
    assert_eq!(
        a.ok(json!({"op": "friend_request", "user_id": helper.id}))
            .await["friends"],
        true
    );
    let r = b
        .call(json!({"op": "friend_request", "user_id": helper.id}))
        .await;
    assert_eq!(code(&r), ("forbidden", "friend.nobody"));
    // The app's agent (a team agent): anyone adds it; it is in the square
    // with its role, and leaves it when stopped.
    let owner = user(&hub, "ops", UserKind::Human, None).await;
    let made = hub
        .create_agent_with(&owner, "concierge", true)
        .await
        .unwrap();
    let concierge = hub.user(made["agent"]["id"].as_str().unwrap()).unwrap();
    hub.put_role(
        "concierge",
        "---\ndescription: Answers questions about the app\ntags: [help]\n---\nBe kind.",
    )
    .await
    .unwrap();
    hub.set_agent_role(&concierge.id, Some("concierge"))
        .await
        .unwrap();
    let sq = b.ok(json!({"op": "list_app_agents"})).await;
    let names: Vec<&str> = sq["agents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["concierge"], "{sq}");
    assert_eq!(
        sq["agents"][0]["description"],
        "Answers questions about the app"
    );
    assert_eq!(sq["agents"][0]["tags"], json!(["help"]));
    assert_eq!(
        b.ok(json!({"op": "friend_request", "user_id": concierge.id}))
            .await["friends"],
        true
    );
    hub.stop_agent_as(&concierge, "admin", None).await.unwrap();
    assert_eq!(
        b.ok(json!({"op": "list_app_agents"})).await["agents"],
        json!([])
    );
    // A deactivated person cannot be found.
    let gone = user(&hub, "gone", UserKind::Human, None).await;
    hub.set_deactivated(&gone.id, true).await.unwrap();
    let r = a
        .call(json!({"op": "friend_request", "user_id": gone.id}))
        .await;
    assert_eq!(code(&r), ("not_found", "account.unknown_user"));
}

async fn blocks_end_friendships(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let carol = user(&hub, "carol", UserKind::Human, None).await;
    let (mut a, b) = (Conn::new(&hub, &alice), Conn::new(&hub, &bob));
    // Both ask at once (one after the other): friends, no request left.
    a.ok(json!({"op": "friend_request", "user_id": bob.id}))
        .await;
    let r = b
        .ok(json!({"op": "friend_request", "user_id": alice.id}))
        .await;
    assert_eq!(r["friends"], true);
    let l = a.ok(json!({"op": "list_friends"})).await;
    assert_eq!(
        (ids(&l, "friends"), l["outgoing"].clone()),
        (vec![bob.id.clone()], json!([]))
    );
    // Bob blocks Alice: the friendship ends, both told.
    a.drain();
    b.ok(json!({"op": "block", "user_id": alice.id})).await;
    assert_eq!(events(&mut a), vec![("removed".into(), bob.id.clone())]);
    assert_eq!(
        a.ok(json!({"op": "list_friends"})).await["friends"],
        json!([])
    );
    // Neither can ask the other while blocked.
    for (c, t) in [(&a, &bob), (&b, &alice)] {
        let r = c
            .call(json!({"op": "friend_request", "user_id": t.id}))
            .await;
        assert_eq!(code(&r), ("forbidden", "friend.blocked"));
    }
    // A waiting request goes too.
    let cc = Conn::new(&hub, &carol);
    cc.ok(json!({"op": "friend_request", "user_id": alice.id}))
        .await;
    a.ok(json!({"op": "block", "user_id": carol.id})).await;
    assert_eq!(
        a.ok(json!({"op": "list_friends"})).await["incoming"],
        json!([])
    );
    assert_eq!(
        cc.ok(json!({"op": "list_friends"})).await["outgoing"],
        json!([])
    );
    // Unblocking does not bring the friendship back.
    b.ok(json!({"op": "unblock", "user_id": alice.id})).await;
    assert_eq!(
        b.ok(json!({"op": "list_friends"})).await["friends"],
        json!([])
    );
    assert_eq!(
        b.ok(json!({"op": "friend_request", "user_id": alice.id}))
            .await["requested"],
        true
    );
}

async fn the_app_can_require_friends(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let carol = user(&hub, "carol", UserKind::Human, None).await;
    let a = Conn::new(&hub, &alice);
    // By default anyone may be talked to.
    a.ok(json!({"op": "create_conv", "kind": "dm", "title": "dm", "members": [carol.id]}))
        .await;
    let helper = a.ok(json!({"op": "create_agent", "name": "helper"})).await["agent"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    hub.set_client_defaults(&json!({"direct_to_non_friends": false}))
        .await
        .unwrap();
    for msg in [
        json!({"op": "create_conv", "kind": "dm", "title": "dm", "members": [bob.id]}),
        json!({"op": "create_conv", "kind": "group", "title": "g", "members": [bob.id, helper]}),
    ] {
        let r = a.call(msg.clone()).await;
        assert_eq!(code(&r), ("forbidden", "friend.required"), "{msg}");
    }
    let g = a
        .ok(json!({"op": "create_conv", "kind": "group", "title": "g", "members": [helper]}))
        .await["conv_id"]
        .as_str()
        .unwrap()
        .to_string();
    let r = a
        .call(json!({"op": "add_member", "conv_id": g, "user_id": bob.id}))
        .await;
    assert_eq!(code(&r), ("forbidden", "friend.required"));
    // Friends now: both work.
    hub.set_friends(&alice, &bob, true).await.unwrap();
    a.ok(json!({"op": "create_conv", "kind": "dm", "title": "dm", "members": [bob.id]}))
        .await;
    a.ok(json!({"op": "add_member", "conv_id": g, "user_id": bob.id}))
        .await;
    // The open API as the system is not limited.
    hub.set_client_defaults(&json!({"direct_to_non_friends": true}))
        .await
        .unwrap();
    a.ok(json!({"op": "create_conv", "kind": "dm", "title": "dm", "members": [carol.id]}))
        .await;
}

async fn the_backend_imports_friendships(db: TestDb) {
    use std::net::SocketAddr;
    let hub = Hub::open(single(&db.url), hub_cfg()).await.unwrap();
    let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", tcp.local_addr().unwrap());
    let app = agentim_core::server::router(agentim_core::server::AppState {
        hub: hub.clone(),
        demo_login: false,
        metrics_token: None,
    });
    tokio::spawn(async move {
        axum::serve(tcp, app.into_make_service_with_connect_info::<SocketAddr>())
            .await
            .unwrap()
    });
    let scopes: Vec<String> = ["users:write", "users:read"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let (key, _) = hub.create_api_key("t", &scopes, None, None).await.unwrap();
    let http = reqwest::Client::new();
    let call = |m: &str, p: &str| {
        http.request(m.parse().unwrap(), format!("{base}{p}"))
            .header("authorization", format!("Bearer {key}"))
            .send()
    };
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let r = call("PUT", "/v1/users/alice/friends/bob").await.unwrap();
    assert_eq!(r.status(), 200);
    let v: Value = r.text().await.unwrap().parse().unwrap();
    assert_eq!(
        (v["friends"].clone(), v["changed"].clone()),
        (json!(true), json!(true))
    );
    let v: Value = call("PUT", "/v1/users/alice/friends/bob")
        .await
        .unwrap()
        .text()
        .await
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(v["changed"], false, "again: no change");
    let v: Value = call("GET", "/v1/users/bob/friends")
        .await
        .unwrap()
        .text()
        .await
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(v["friends"][0]["user_id"], "alice", "{v}");
    assert!(hub.are_friends(&alice.id, &bob.id).await.unwrap());
    for (m, p, want) in [
        ("PUT", "/v1/users/alice/friends/alice", 400),
        ("PUT", "/v1/users/alice/friends/nobody", 404),
        ("GET", "/v1/users/nobody/friends", 404),
    ] {
        assert_eq!(call(m, p).await.unwrap().status(), want, "{m} {p}");
    }
    Conn::new(&hub, &bob)
        .ok(json!({"op": "block", "user_id": alice.id}))
        .await;
    let r = call("PUT", "/v1/users/alice/friends/bob").await.unwrap();
    assert_eq!(r.status(), 403);
    let v: Value = r.text().await.unwrap().parse().unwrap();
    assert_eq!(v["rule"], "friend.blocked");
    Conn::new(&hub, &bob)
        .ok(json!({"op": "unblock", "user_id": alice.id}))
        .await;
    call("PUT", "/v1/users/alice/friends/bob").await.unwrap();
    let v: Value = call("DELETE", "/v1/users/alice/friends/bob")
        .await
        .unwrap()
        .text()
        .await
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(v["changed"], true);
    assert!(!hub.are_friends(&alice.id, &bob.id).await.unwrap());
    let n = hub
        .audit_log(10_000)
        .await
        .unwrap()
        .iter()
        .filter(|a| a["action"].as_str().unwrap_or("").contains("friends."))
        .count();
    assert_eq!(n, 3, "two puts that changed, one delete");
}

async fn friends_survive_a_restart(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let carol = user(&hub, "carol", UserKind::Human, None).await;
    hub.set_friends(&alice, &bob, true).await.unwrap();
    hub.friend_request(&carol, &alice.id, "hello")
        .await
        .unwrap();
    hub.set_friend_policy(&bob, "nobody").await.unwrap();
    let before = hub.list_friends(&alice).await.unwrap();
    hub.close().await;
    drop(hub);
    let hub = open(single(&db.url)).await;
    assert_eq!(hub.list_friends(&alice).await.unwrap(), before);
    assert_eq!(
        hub.list_friends(&bob).await.unwrap()["friend_policy"],
        "nobody"
    );
}

/// Two people asking each other at once while one of them may block: never
/// requests waiting both ways, never friends while blocked, and the
/// requests that are left match what happened.
async fn race_requests_both_ways_and_blocks(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let mut became = 0;
    for round in 0..20 {
        let block = round % 3 == 2;
        let mut tasks = vec![];
        for (me, other) in [(alice.clone(), bob.clone()), (bob.clone(), alice.clone())] {
            let h = hub.clone();
            tasks.push(tokio::spawn(async move {
                h.friend_request(&me, &other.id, "")
                    .await
                    .map(|v| v["friends"] == true)
                    .map_err(|e| e.rule)
            }));
        }
        if block {
            let h = hub.clone();
            let (a, b) = (alice.clone(), bob.clone());
            tasks.push(tokio::spawn(async move {
                Conn::new(&h, &b)
                    .ok(json!({"op": "block", "user_id": a.id}))
                    .await;
                Ok(false)
            }));
        }
        let mut results = vec![];
        for t in tasks {
            results.push(t.await.unwrap());
        }
        let la = hub.list_friends(&alice).await.unwrap();
        let lb = hub.list_friends(&bob).await.unwrap();
        let friends = !la["friends"].as_array().unwrap().is_empty();
        assert_eq!(
            friends,
            !lb["friends"].as_array().unwrap().is_empty(),
            "round {round}: one-sided"
        );
        let out_a = !la["outgoing"].as_array().unwrap().is_empty();
        let out_b = !lb["outgoing"].as_array().unwrap().is_empty();
        assert!(
            !(out_a && out_b),
            "round {round}: requests both ways: {la} {lb}"
        );
        if friends {
            assert!(
                !out_a && !out_b,
                "round {round}: friends with a request left"
            );
            became += 1;
        }
        if block {
            assert!(
                !friends && !out_a && !out_b,
                "round {round}: friends or a request while blocked: {results:?}"
            );
        } else {
            assert!(
                friends,
                "round {round}: two requests that cross make friends: {results:?}"
            );
        }
        // Reset.
        if block {
            Conn::new(&hub, &bob)
                .ok(json!({"op": "unblock", "user_id": alice.id}))
                .await;
        }
        hub.set_friends(&alice, &bob, false).await.unwrap();
    }
    assert!(became >= 13, "{became}");
}
