//! Membership lifecycle (docs/02-architecture.md §2.4): removing, leaving,
//! roles, ownership, dissolving, blocking, one direct conversation per pair
//! and the private user directory. Scenarios first, then races. Run against
//! every backend.

mod common;

use std::sync::Arc;

use agentim::model::*;
use common::*;
use serde_json::{Value, json};

backend_tests!(
    removed_members_lose_access_and_are_told,
    roles_decide_who_can_manage_members,
    leaving_and_transferring_ownership,
    removing_an_agent_stops_its_work,
    dissolving_deletes_the_conversation_for_everyone,
    blocking_stops_direct_contact,
    one_direct_conversation_per_pair,
    the_directory_shows_only_people_you_know,
    membership_changes_in_a_unit_database_update_the_directory,
    race_sends_against_removal,
    race_direct_conversation_creation,
    race_remove_and_leave,
    race_ownership_transfers,
    race_sends_against_dissolve,
);

fn code(r: &Value) -> &str {
    r["error"]["code"].as_str().unwrap_or("")
}

fn events(p: &Value) -> Vec<Value> {
    p["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["event"].clone())
        .collect()
}

async fn convs(c: &Conn) -> Vec<Value> {
    c.ok(json!({"op": "list_convs"})).await["convs"]
        .as_array()
        .unwrap()
        .clone()
}

async fn role_of(c: &Conn, conv: &str, u: &User) -> String {
    convs(c)
        .await
        .iter()
        .find(|v| v["conv_id"] == conv)
        .and_then(|v| {
            v["members"]
                .as_array()
                .unwrap()
                .iter()
                .find(|m| m["user_id"] == u.id.as_str())
                .map(|m| m["role"].as_str().unwrap().to_string())
        })
        .unwrap_or_default()
}

fn remove(conv: &str, u: &User) -> Value {
    json!({"op": "remove_member", "conv_id": conv, "user_id": u.id})
}

fn dm(u: &User) -> Value {
    json!({"op": "create_conv", "kind": "dm", "title": "dm", "members": [u.id]})
}

// ------------------------------------------------------------- scenarios

async fn removed_members_lose_access_and_are_told(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let carol = user(&hub, "carol", UserKind::Human, None).await;
    let a = Conn::new(&hub, &alice);
    let mut b = Conn::new(&hub, &bob);
    let mut c = Conn::new(&hub, &carol);
    let conv = a.group(&[&bob, &carol]).await;
    a.send(&conv, "before").await;
    b.ok(json!({"op": "focus", "conv_id": conv})).await;
    b.drain();
    c.drain();
    let r = a.ok(remove(&conv, &bob)).await;
    let told = |frames: Vec<Value>| {
        frames.iter().any(|f| {
            f["type"] == "event"
                && f["event"]["type"] == "member.removed"
                && f["event"]["data"]["user_id"] == bob.id.as_str()
                && f["event"]["seq"] == r["seq"]
        })
    };
    assert!(told(b.drain()), "the removed member is told");
    assert!(told(c.drain()), "the others see it");
    a.send(&conv, "after").await;
    assert!(b.drain().is_empty(), "no pushes after removal");
    for req in [
        json!({"op": "send", "conv_id": conv, "client_msg_id": "x", "text": "still here?"}),
        json!({"op": "pull", "conv_id": conv}),
        json!({"op": "pull_side", "conv_id": conv}),
        json!({"op": "read", "conv_id": conv, "seq": 1}),
        json!({"op": "focus", "conv_id": conv}),
    ] {
        assert_eq!(code(&b.call(req.clone()).await), "forbidden", "{req}");
    }
    assert!(
        convs(&b)
            .await
            .iter()
            .all(|v| v["conv_id"] != conv.as_str())
    );
    assert_eq!(code(&a.call(remove(&conv, &bob)).await), "not_found");
    // Added back, they see the conversation again.
    a.ok(json!({"op": "add_member", "conv_id": conv, "user_id": bob.id}))
        .await;
    assert!(b.pull(&conv, 0).await.to_string().contains("after"));
    let log = events(&a.pull(&conv, 0).await);
    let seqs: Vec<i64> = log.iter().map(|e| e["seq"].as_i64().unwrap()).collect();
    assert_eq!(seqs, (1..=seqs.len() as i64).collect::<Vec<_>>());
}

async fn roles_decide_who_can_manage_members(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let carol = user(&hub, "carol", UserKind::Human, None).await;
    let dave = user(&hub, "dave", UserKind::Human, None).await;
    let erin = user(&hub, "erin", UserKind::Human, None).await;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    let a = Conn::new(&hub, &alice);
    let b = Conn::new(&hub, &bob);
    let c = Conn::new(&hub, &carol);
    let conv = a.group(&[&bob, &carol, &dave, &bot]).await;
    let set_role = |u: &User, role: &str| json!({"op": "set_role", "conv_id": conv, "user_id": u.id, "role": role});
    assert_eq!(
        code(&b.call(remove(&conv, &carol)).await),
        "forbidden",
        "members cannot remove"
    );
    assert_eq!(code(&b.call(set_role(&carol, "admin")).await), "forbidden");
    assert_eq!(code(&a.call(set_role(&bob, "owner")).await), "bad_request");
    assert_eq!(
        code(&a.call(set_role(&bot, "admin")).await),
        "bad_request",
        "agents are not admins"
    );
    a.ok(set_role(&bob, "admin")).await;
    a.ok(set_role(&dave, "admin")).await;
    assert_eq!(role_of(&a, &conv, &bob).await, "admin");
    // An admin manages members, not the owner or other admins.
    b.ok(json!({"op": "add_member", "conv_id": conv, "user_id": erin.id}))
        .await;
    let m = c.send(&conv, "carol's message").await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    b.ok(json!({"op": "recall", "msg_id": m})).await;
    b.ok(remove(&conv, &carol)).await;
    assert_eq!(code(&b.call(remove(&conv, &alice)).await), "forbidden");
    assert_eq!(code(&b.call(remove(&conv, &dave)).await), "forbidden");
    assert_eq!(
        code(&b.call(set_role(&erin, "admin")).await),
        "forbidden",
        "only the owner changes roles"
    );
    a.ok(set_role(&bob, "member")).await;
    assert_eq!(
        code(&b.call(remove(&conv, &erin)).await),
        "forbidden",
        "no longer an admin"
    );
    let log = a.pull(&conv, 0).await.to_string();
    assert!(log.contains("member.role") && log.contains("member.removed"));
}

async fn leaving_and_transferring_ownership(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let carol = user(&hub, "carol", UserKind::Human, None).await;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    let a = Conn::new(&hub, &alice);
    let b = Conn::new(&hub, &bob);
    let c = Conn::new(&hub, &carol);
    let conv = a.group(&[&bob, &carol, &bot]).await;
    let leave = json!({"op": "leave", "conv_id": conv});
    let transfer = |u: &User| json!({"op": "transfer_owner", "conv_id": conv, "user_id": u.id});
    c.ok(leave.clone()).await;
    assert!(a.pull(&conv, 0).await.to_string().contains("member.left"));
    assert_eq!(
        code(&c.call(json!({"op": "pull", "conv_id": conv})).await),
        "forbidden"
    );
    assert_eq!(
        code(&a.call(leave.clone()).await),
        "conflict",
        "the owner hands over first"
    );
    assert_eq!(code(&b.call(transfer(&bob)).await), "forbidden");
    assert_eq!(
        code(&a.call(transfer(&bot)).await),
        "bad_request",
        "agents cannot own"
    );
    assert_eq!(
        code(&a.call(transfer(&carol)).await),
        "not_found",
        "carol left"
    );
    a.ok(transfer(&bob)).await;
    assert_eq!(role_of(&b, &conv, &bob).await, "owner");
    assert_eq!(role_of(&b, &conv, &alice).await, "admin");
    a.ok(leave).await;
    assert!(
        b.pull(&conv, 0)
            .await
            .to_string()
            .contains("owner.transferred")
    );
    // Roles survive a restart.
    drop((a, b, c));
    drop(hub);
    let hub = open(single(&db.url)).await;
    let b = Conn::new(&hub, &bob);
    assert_eq!(role_of(&b, &conv, &bob).await, "owner");
}

async fn removing_an_agent_stops_its_work(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    let helper = user(&hub, "helper", UserKind::Agent, Some(&alice)).await;
    let bobbot = user(&hub, "bobbot", UserKind::Agent, Some(&bob)).await;
    let a = Conn::new(&hub, &alice);
    let b = Conn::new(&hub, &bob);
    let mut g = Conn::new(&hub, &bot);
    let h = Conn::new(&hub, &helper);
    let conv = a.group(&[&bob, &bot, &helper, &bobbot]).await;
    let q = b.send(&conv, "@bot @helper please").await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    // bot is streaming; helper has a draft waiting for approval.
    let s = g
        .ok(json!({"op": "stream_start", "conv_id": conv, "client_msg_id": "s", "reply_to": q}))
        .await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    g.ok(json!({"op": "stream_append", "msg_id": s, "idx": 0, "text": "half an answ"}))
        .await;
    a.ok(
        json!({"op": "set_agent_policy", "conv_id": conv, "agent_id": helper.id, "approval": true}),
    )
    .await;
    let d = h.reply(&conv, "draft", &q).await["draft_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        code(&b.call(remove(&conv, &bot)).await),
        "forbidden",
        "not bob's agent"
    );
    b.ok(remove(&conv, &bobbot)).await;
    g.drain();
    a.ok(remove(&conv, &bot)).await;
    a.ok(remove(&conv, &helper)).await;
    let log = a.pull(&conv, 0).await;
    let m = msg_by_id(&log, &s).unwrap();
    assert_eq!(
        (m["state"].as_str(), m["state_reason"].as_str()),
        (Some("cancelled"), Some("removed"))
    );
    assert!(g.drain().iter().any(|f| f["type"] == "stream_cancelled"));
    assert_eq!(
        code(
            &g.call(json!({"op": "stream_append", "msg_id": s, "idx": 1, "text": "er"}))
                .await
        ),
        "not_found"
    );
    assert_eq!(code(&g.reply(&conv, "still here", &q).await), "forbidden");
    assert_eq!(
        code(&a.call(json!({"op": "approve", "draft_id": d})).await),
        "conflict",
        "the draft was discarded"
    );
    assert_eq!(
        a.ok(json!({"op": "list_drafts"})).await["drafts"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
}

async fn dissolving_deletes_the_conversation_for_everyone(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let a = Conn::new(&hub, &alice);
    let mut b = Conn::new(&hub, &bob);
    let conv = a.group(&[&bob]).await;
    let m = a.send(&conv, "dissolve-me-secret").await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    let dissolve = json!({"op": "dissolve", "conv_id": conv});
    assert_eq!(code(&b.call(dissolve.clone()).await), "forbidden");
    b.drain();
    a.ok(dissolve.clone()).await;
    assert!(
        b.drain()
            .iter()
            .any(|f| f["event"]["type"] == "conv.dissolved"),
        "members are told"
    );
    for c in [&a, &b] {
        assert_eq!(code(&c.send(&conv, "anyone?").await), "not_found");
        assert_eq!(
            code(&c.call(json!({"op": "pull", "conv_id": conv})).await),
            "not_found"
        );
        assert!(convs(c).await.is_empty());
    }
    assert_eq!(
        code(
            &a.call(json!({"op": "edit", "msg_id": m, "text": "x"}))
                .await
        ),
        "not_found"
    );
    assert_eq!(code(&a.call(dissolve.clone()).await), "not_found");
    let stats = hub.stats().await.unwrap().to_string();
    assert!(stats.contains("conv.dissolved"), "the audit record stays");
    assert!(!stats.contains("dissolve-me-secret"));
    drop((a, b));
    drop(hub);
    let hub = open(single(&db.url)).await;
    let a = Conn::new(&hub, &alice);
    assert!(convs(&a).await.is_empty(), "still gone after a restart");
    assert_eq!(
        code(&a.call(json!({"op": "pull", "conv_id": conv})).await),
        "not_found"
    );
}

async fn blocking_stops_direct_contact(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let carol = user(&hub, "carol", UserKind::Human, None).await;
    let a = Conn::new(&hub, &alice);
    let b = Conn::new(&hub, &bob);
    let c = Conn::new(&hub, &carol);
    let existing_dm = a.ok(dm(&bob)).await["conv_id"]
        .as_str()
        .unwrap()
        .to_string();
    let group = c.group(&[&alice, &bob]).await;
    assert_eq!(
        code(&b.call(json!({"op": "block", "user_id": bob.id})).await),
        "bad_request"
    );
    b.ok(json!({"op": "block", "user_id": alice.id})).await;
    assert_eq!(
        b.ok(json!({"op": "list_blocks"})).await["blocked"],
        json!([alice.id])
    );
    // No direct contact in either direction.
    assert_eq!(code(&a.send(&existing_dm, "hello?").await), "blocked");
    assert_eq!(code(&b.send(&existing_dm, "hello?").await), "blocked");
    let other = a.group(&[&carol]).await;
    assert_eq!(
        code(
            &a.call(json!({"op": "add_member", "conv_id": other, "user_id": bob.id}))
                .await
        ),
        "blocked"
    );
    assert_eq!(
        code(
            &a.call(
                json!({"op": "create_conv", "kind": "group", "title": "t", "members": [bob.id]})
            )
            .await
        ),
        "blocked"
    );
    let eph = json!({"op": "send", "conv_id": group, "client_msg_id": "e", "text": "psst", "to_user": bob.id});
    assert_eq!(code(&a.call(eph.clone()).await), "blocked");
    // Shared groups keep working.
    assert_eq!(a.send(&group, "group message").await["ok"], true);
    // Blocks survive a restart; unblocking restores contact.
    drop((a, b, c));
    drop(hub);
    let hub = open(single(&db.url)).await;
    let a = Conn::new(&hub, &alice);
    let b = Conn::new(&hub, &bob);
    assert_eq!(code(&a.send(&existing_dm, "now?").await), "blocked");
    b.ok(json!({"op": "unblock", "user_id": alice.id})).await;
    assert_eq!(a.send(&existing_dm, "thanks").await["ok"], true);
    assert_eq!(a.call(eph).await["ok"], true);
}

async fn one_direct_conversation_per_pair(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let a = Conn::new(&hub, &alice);
    let b = Conn::new(&hub, &bob);
    let first = a.ok(dm(&bob)).await;
    let again = a.ok(dm(&bob)).await;
    let reverse = b.ok(dm(&alice)).await;
    assert_eq!(again["conv_id"], first["conv_id"]);
    assert_eq!(again["existing"], true);
    assert_eq!(reverse["conv_id"], first["conv_id"]);
    assert_eq!(convs(&a).await.len(), 1);
    // After it is dissolved, a new one can be created.
    a.ok(json!({"op": "dissolve", "conv_id": first["conv_id"]}))
        .await;
    let fresh = a.ok(dm(&bob)).await;
    assert_ne!(fresh["conv_id"], first["conv_id"]);
    assert!(fresh["existing"].is_null());
}

async fn the_directory_shows_only_people_you_know(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let dave = user(&hub, "dave", UserKind::Human, None).await;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    let a = Conn::new(&hub, &alice);
    let d = Conn::new(&hub, &dave);
    a.group(&[&bob]).await;
    let names = |h: &Value| -> Vec<String> {
        let mut v: Vec<String> = h["users"]
            .as_array()
            .unwrap()
            .iter()
            .map(|u| u["name"].as_str().unwrap().to_string())
            .collect();
        v.sort();
        v
    };
    assert_eq!(
        names(&a.ok(json!({"op": "hello"})).await),
        vec!["alice", "bob", "bot"]
    );
    assert_eq!(
        names(&d.ok(json!({"op": "hello"})).await),
        vec!["dave"],
        "strangers see only themselves"
    );
    let found = d.ok(json!({"op": "find_user", "name": "bob"})).await;
    assert_eq!(found["user"]["id"], bob.id.as_str());
    assert!(found["user"].get("token").is_none());
    assert_eq!(
        code(&d.call(json!({"op": "find_user", "name": "nobody"})).await),
        "not_found"
    );
    let _ = bot;
}

async fn membership_changes_in_a_unit_database_update_the_directory(db: TestDb) {
    let secret = new_db(Backend::Sqlite).await.unwrap();
    let hub = open(two(&db.url, &secret.url, "secret")).await;
    let alice = user_in(&hub, "alice", "b").await;
    let bob = user_in(&hub, "bob", "b").await;
    let carol = user_in(&hub, "carol", "b").await;
    let a = Conn::new(&hub, &alice);
    let b = Conn::new(&hub, &bob);
    let c = Conn::new(&hub, &carol);
    let conv = a.group(&[&bob, &carol]).await;
    a.ok(remove(&conv, &bob)).await;
    c.ok(json!({"op": "leave", "conv_id": conv})).await;
    assert!(convs(&b).await.is_empty());
    assert!(convs(&c).await.is_empty());
    assert_eq!(convs(&a).await.len(), 1);
    a.ok(json!({"op": "dissolve", "conv_id": conv})).await;
    assert!(convs(&a).await.is_empty());
    // The directory forgot it too: after a restart nothing comes back.
    drop((a, b, c));
    drop(hub);
    let hub = open(two(&db.url, &secret.url, "secret")).await;
    assert!(convs(&Conn::new(&hub, &alice)).await.is_empty());
}

// ------------------------------------------------------------------ races

async fn race_sends_against_removal(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let a = Arc::new(Conn::new(&hub, &alice));
    for round in 0..10 {
        let bob = user(&hub, &format!("bob{round}"), UserKind::Human, None).await;
        let conv = a.group(&[&bob]).await;
        let senders: Vec<_> = (0..3)
            .map(|k| {
                let c = Arc::new(Conn::new(&hub, &bob));
                let conv = conv.clone();
                tokio::spawn(async move {
                    let mut out = vec![];
                    for i in 0..20 {
                        out.push(c.send(&conv, &format!("{k}-{i}")).await);
                    }
                    out
                })
            })
            .collect();
        tokio::task::yield_now().await;
        let removed = a.ok(remove(&conv, &bob)).await["seq"].as_i64().unwrap();
        for t in senders {
            for r in t.await.unwrap() {
                assert!(r["ok"] == true || code(&r) == "forbidden", "{r}");
            }
        }
        let log = events(&a.pull(&conv, 0).await);
        for e in &log {
            if e["type"] == "msg.created" && e["msg"]["sender_id"] == bob.id.as_str() {
                assert!(
                    e["seq"].as_i64().unwrap() < removed,
                    "message after removal at seq {}",
                    e["seq"]
                );
            }
        }
    }
}

async fn race_direct_conversation_creation(db: TestDb) {
    let hub = open(single(&db.url)).await;
    for round in 0..10 {
        let x = user(&hub, &format!("x{round}"), UserKind::Human, None).await;
        let y = user(&hub, &format!("y{round}"), UserKind::Human, None).await;
        let mut tasks = vec![];
        for k in 0..6 {
            let (me, other) = if k % 2 == 0 {
                (x.clone(), y.clone())
            } else {
                (y.clone(), x.clone())
            };
            let c = Conn::new(&hub, &me);
            tasks.push(tokio::spawn(async move {
                c.ok(dm(&other)).await["conv_id"]
                    .as_str()
                    .unwrap()
                    .to_string()
            }));
        }
        let mut ids = std::collections::HashSet::new();
        for t in tasks {
            ids.insert(t.await.unwrap());
        }
        assert_eq!(ids.len(), 1, "one direct conversation per pair");
        assert_eq!(convs(&Conn::new(&hub, &x)).await.len(), 1);
    }
}

async fn race_remove_and_leave(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let owner: Vec<Arc<Conn>> = (0..2).map(|_| Arc::new(Conn::new(&hub, &alice))).collect();
    let conv = owner[0].group(&[]).await;
    for round in 0..10 {
        let u = user(&hub, &format!("u{round}"), UserKind::Human, None).await;
        owner[0]
            .ok(json!({"op": "add_member", "conv_id": conv, "user_id": u.id}))
            .await;
        let me = Arc::new(Conn::new(&hub, &u));
        let reqs = vec![
            (owner[0].clone(), remove(&conv, &u)),
            (me.clone(), json!({"op": "leave", "conv_id": conv})),
            (owner[1].clone(), remove(&conv, &u)),
        ];
        let tasks: Vec<_> = reqs
            .into_iter()
            .map(|(c, v)| tokio::spawn(async move { c.call(v).await }))
            .collect();
        let mut ok = 0;
        for t in tasks {
            let r = t.await.unwrap();
            if r["ok"] == true {
                ok += 1;
            } else {
                assert!(matches!(code(&r), "not_found" | "forbidden"), "{r}");
            }
        }
        assert_eq!(ok, 1, "removed exactly once");
    }
    let log = events(&owner[0].pull(&conv, 0).await);
    let gone = log
        .iter()
        .filter(|e| e["type"] == "member.removed" || e["type"] == "member.left")
        .count();
    assert_eq!(gone, 10);
}

async fn race_ownership_transfers(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let others: Vec<User> = {
        let mut v = vec![];
        for i in 0..4 {
            v.push(user(&hub, &format!("o{i}"), UserKind::Human, None).await);
        }
        v
    };
    let refs: Vec<&User> = others.iter().collect();
    let first = Conn::new(&hub, &alice);
    let conv = first.group(&refs).await;
    // The owner's devices hand the conversation to four different people at once.
    let tasks: Vec<_> = others
        .iter()
        .map(|o| {
            let c = Conn::new(&hub, &alice);
            let req = json!({"op": "transfer_owner", "conv_id": conv, "user_id": o.id});
            tokio::spawn(async move { c.call(req).await })
        })
        .collect();
    let mut ok = 0;
    for t in tasks {
        let r = t.await.unwrap();
        if r["ok"] == true {
            ok += 1;
        } else {
            assert_eq!(code(&r), "forbidden", "{r}");
        }
    }
    assert_eq!(ok, 1);
    let list = convs(&first).await;
    let owners = list[0]["members"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["role"] == "owner")
        .count();
    assert_eq!(owners, 1, "exactly one owner");
}

async fn race_sends_against_dissolve(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    for round in 0..10 {
        let a = Conn::new(&hub, &alice);
        let conv = a.group(&[&bob]).await;
        let senders: Vec<_> = (0..3)
            .map(|k| {
                let c = Conn::new(&hub, &bob);
                let conv = conv.clone();
                tokio::spawn(async move {
                    let mut out = vec![];
                    for i in 0..20 {
                        out.push(c.send(&conv, &format!("r{round}-{k}-{i}")).await);
                    }
                    out
                })
            })
            .collect();
        tokio::task::yield_now().await;
        a.ok(json!({"op": "dissolve", "conv_id": conv})).await;
        for t in senders {
            // Once refused, a sender is refused for good: nothing lands after the dissolve.
            let mut refused = false;
            for r in t.await.unwrap() {
                if r["ok"] == true {
                    assert!(!refused, "a send succeeded after a refusal");
                } else {
                    assert!(matches!(code(&r), "not_found" | "forbidden"), "{r}");
                    refused = true;
                }
            }
        }
        assert_eq!(
            code(&a.call(json!({"op": "pull", "conv_id": conv})).await),
            "not_found"
        );
    }
}
