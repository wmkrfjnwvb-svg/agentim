//! Agent registration and keys (docs/10-agent-ecosystem.md §5.1): creation by
//! the owner, the per-owner cap, rotation (one write, exactly one key left),
//! revocation, refusals that leave no trace, keys made by another process,
//! restart, and the races between them.

mod common;

use std::collections::HashSet;
use std::time::Duration;

use agentim_core::config::Config;
use agentim_core::hub::Hub;
use agentim_core::model::*;
use common::*;
use serde_json::{Value, json};

backend_tests!(
    a_person_creates_an_agent_with_a_working_key,
    refused_creations_leave_no_trace,
    keys_are_rotated_and_revoked_by_the_owner_only,
    key_limits_and_deactivated_agents,
    keys_made_by_another_process_are_found,
    keys_and_rotations_survive_a_restart,
    race_rotations_leave_exactly_one_key,
    race_creations_respect_the_cap,
);

fn code(v: &Value) -> &str {
    v["error"]["code"].as_str().unwrap_or("")
}

fn cfg_with(db: &TestDb, f: impl FnOnce(&mut Config)) -> Config {
    let mut c = single(&db.url);
    f(&mut c);
    c
}

async fn audit_count(hub: &Hub, action: &str) -> usize {
    hub.audit_log(10_000)
        .await
        .unwrap()
        .iter()
        .filter(|a| a["action"] == action)
        .count()
}

async fn users_named(hub: &Hub, owner: &User) -> Vec<String> {
    let v = Conn::new(hub, owner).ok(json!({"op": "list_agents"})).await;
    v["agents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["agent"]["name"].as_str().unwrap().to_string())
        .collect()
}

async fn a_person_creates_an_agent_with_a_working_key(db: TestDb) {
    let hub = open(cfg_with(&db, |c| {
        c.server.public_url = Some("https://im.example.com/".into())
    }))
    .await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let a = Conn::new(&hub, &alice);
    let r = a.ok(json!({"op": "create_agent", "name": "helper"})).await;
    let agent = &r["agent"];
    assert_eq!(agent["kind"], "agent");
    assert_eq!(agent["owner_id"], json!(alice.id));
    assert_eq!(agent["unit_id"], json!(alice.unit_id));
    let key = r["key"].as_str().unwrap();
    let (who, device) = hub.authenticate_device(key).expect("the key works");
    assert_eq!(who.id, agent["id"].as_str().unwrap());
    assert_eq!(device, r["device_id"].as_str().unwrap());
    // Ready-to-paste settings from server.public_url.
    let c = &r["connect"];
    assert_eq!(c["mcp_url"], "https://im.example.com/mcp");
    assert_eq!(
        c["websocket_url"],
        format!("wss://im.example.com/ws?token={key}")
    );
    assert_eq!(
        c["mcp_config"]["mcpServers"]["agentim"]["headers"]["Authorization"],
        format!("Bearer {key}")
    );
    assert_eq!(audit_count(&hub, "agent.create").await, 1);
    // Listed with its key, never the secret.
    let l = a.ok(json!({"op": "list_agents"})).await;
    assert_eq!(l["agents"].as_array().unwrap().len(), 1);
    assert_eq!(l["agents"][0]["keys"].as_array().unwrap().len(), 1);
    assert!(!l.to_string().contains(key));
    assert!(!l.to_string().contains("atk_"));
    // Somebody else lists only their own.
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    assert!(users_named(&hub, &bob).await.is_empty());
    // The agent works with its key like any client: it is found by name,
    // can be added to a conversation and syncs there.
    let helper = hub.user(agent["id"].as_str().unwrap()).unwrap();
    let f = a.ok(json!({"op": "find_user", "name": "helper"})).await;
    assert_eq!(f["user"]["id"], json!(helper.id));
    let conv = a.group(&[&helper]).await;
    a.send(&conv, "hi @helper").await;
    let ag = Conn::device(&hub, &helper, &device);
    let s = ag.ok(json!({"op": "sync"})).await;
    let got: Vec<&str> = s["convs"][0]["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|e| e["event"]["msg"]["content"].as_str())
        .collect();
    assert_eq!(got, vec!["hi @helper"]);
    // Without public_url the addresses are relative.
    drop(hub);
    let hub = open(single(&db.url)).await;
    let r = Conn::new(&hub, &alice)
        .ok(json!({"op": "agent_key", "agent_id": helper.id}))
        .await;
    assert_eq!(r["connect"]["mcp_url"], "/mcp");
}

async fn refused_creations_leave_no_trace(db: TestDb) {
    let hub = open(cfg_with(&db, |c| c.agents.max_per_owner = 2)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let a = Conn::new(&hub, &alice);
    a.ok(json!({"op": "create_agent", "name": "one"})).await;
    let one = hub.user_by_name("one").unwrap();
    let long = "x".repeat(65);
    let cases: Vec<(Value, &str)> = vec![
        (json!(""), "bad_request"),
        (json!("_reserved"), "bad_request"),
        (json!(long), "bad_request"),
        (json!("two words"), "bad_request"),
        (json!("nul\u{0}"), "bad_request"),
        (json!("bob"), "conflict"),
        (json!("one"), "conflict"),
    ];
    for (name, want) in cases {
        let r = a.call(json!({"op": "create_agent", "name": name})).await;
        assert_eq!(code(&r), want, "{name}: {r}");
    }
    // 64 bytes is allowed; then the cap of 2 is reached.
    a.ok(json!({"op": "create_agent", "name": "y".repeat(64)}))
        .await;
    let r = a.call(json!({"op": "create_agent", "name": "three"})).await;
    assert_eq!(code(&r), "conflict", "{r}");
    // The cap is per owner, and also holds on the open API path.
    let b = Conn::new(&hub, &bob);
    b.ok(json!({"op": "create_agent", "name": "bobs"})).await;
    let r = hub
        .create_user("viaapi", UserKind::Agent, Some(&alice.id), None)
        .await;
    assert_eq!(r.unwrap_err().code, "conflict");
    // Agents do not create agents.
    let (key, _) = hub
        .issue_token(&one.id, None, None, None, "test")
        .await
        .unwrap();
    let (agent_user, dev) = hub.authenticate_device(&key).unwrap();
    let r = Conn::device(&hub, &agent_user, &dev)
        .call(json!({"op": "create_agent", "name": "child"}))
        .await;
    assert_eq!(code(&r), "forbidden", "{r}");
    // No trace: no users, no audit beyond the 3 successful creations.
    for n in ["", "three", "viaapi", "child", "two words"] {
        assert!(hub.user_by_name(n).is_none(), "{n}");
    }
    assert_eq!(users_named(&hub, &alice).await.len(), 2);
    assert_eq!(audit_count(&hub, "agent.create").await, 3);
    // Unknown fields and a missing name are protocol errors.
    let r = a.call(json!({"op": "create_agent"})).await;
    assert_eq!(code(&r), "bad_request", "{r}");
}

async fn keys_are_rotated_and_revoked_by_the_owner_only(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let a = Conn::new(&hub, &alice);
    let r = a.ok(json!({"op": "create_agent", "name": "helper"})).await;
    let id = r["agent"]["id"].as_str().unwrap().to_string();
    let k1 = r["key"].as_str().unwrap().to_string();
    let helper = hub.user(&id).unwrap();
    // A second key: both work (two environments).
    let r2 = a.ok(json!({"op": "agent_key", "agent_id": id})).await;
    let k2 = r2["key"].as_str().unwrap().to_string();
    assert_eq!(r2["revoked"], json!([]));
    assert!(hub.authenticate(&k1).is_some() && hub.authenticate(&k2).is_some());
    // A connection on key 1 with a sync position.
    let (_, d1) = hub.authenticate_device(&k1).unwrap();
    let conv = a.group(&[&helper]).await;
    a.send(&conv, "before @helper").await;
    let c1 = Conn::device(&hub, &helper, &d1);
    let s = c1.ok(json!({"op": "sync"})).await;
    let upto = s["convs"][0]["upto"].clone();
    c1.ok(json!({"op": "ack", "acks": [{"conv_id": conv, "seq": upto}]}))
        .await;
    // Moving: a new key, every other one revoked in the same write.
    let r3 = a
        .ok(json!({"op": "agent_key", "agent_id": id, "revoke_others": true}))
        .await;
    let k3 = r3["key"].as_str().unwrap().to_string();
    let revoked: HashSet<String> = serde_json::from_value(r3["revoked"].clone()).unwrap();
    assert_eq!(revoked.len(), 2);
    assert!(revoked.contains(&d1));
    assert!(hub.authenticate(&k1).is_none() && hub.authenticate(&k2).is_none());
    assert!(hub.authenticate(&k3).is_some());
    tokio::time::timeout(Duration::from_secs(5), c1.s.kicked())
        .await
        .expect("the old key's connection is closed");
    // A revoked key is not found in the database either.
    assert!(hub.authenticate_or_load(&k1).await.is_none());
    // The new key is a new device: it does not inherit key 1's position
    // (key 1 had acknowledged everything).
    let (_, d3) = hub.authenticate_device(&k3).unwrap();
    let s = Conn::device(&hub, &helper, &d3)
        .ok(json!({"op": "sync"}))
        .await;
    assert!(
        s["convs"][0]["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["event"]["msg"]["content"] == "before @helper")
    );
    let _ = upto;
    // Revoke one key.
    let r4 = a.ok(json!({"op": "agent_key", "agent_id": id})).await;
    let d4 = r4["device_id"].as_str().unwrap();
    let r = a
        .ok(json!({"op": "revoke_agent_key", "agent_id": id, "device_id": d4}))
        .await;
    assert_eq!(r["revoked"], 1);
    assert!(hub.authenticate(r4["key"].as_str().unwrap()).is_none());
    assert!(hub.authenticate(&k3).is_some());
    // Refusals, none of which writes anything.
    let before = audit_count(&hub, "agent.key").await;
    let keys_before = hub.devices(&id).await.unwrap().len();
    let b = Conn::new(&hub, &bob);
    for (who, req, want) in [
        (&b, json!({"op": "agent_key", "agent_id": id}), "forbidden"),
        (
            &b,
            json!({"op": "revoke_agent_key", "agent_id": id, "device_id": d3}),
            "forbidden",
        ),
        (
            &a,
            json!({"op": "agent_key", "agent_id": "u_nobody"}),
            "not_found",
        ),
        (
            &a,
            json!({"op": "agent_key", "agent_id": alice.id}),
            "not_found",
        ),
        (
            &a,
            json!({"op": "agent_key", "agent_id": bob.id}),
            "not_found",
        ),
        (
            &a,
            json!({"op": "revoke_agent_key", "agent_id": id, "device_id": d4}),
            "not_found",
        ),
        (
            &a,
            json!({"op": "revoke_agent_key", "agent_id": id, "device_id": "d_nope"}),
            "not_found",
        ),
        (&a, json!({"op": "agent_key"}), "bad_request"),
    ] {
        let r = who.call(req.clone()).await;
        assert_eq!(code(&r), want, "{req}: {r}");
    }
    // Another person's device id is not one of this agent's keys.
    let bob_dev = hub.devices(&bob.id).await.unwrap()[0].device_id.clone();
    let r = a
        .call(json!({"op": "revoke_agent_key", "agent_id": id, "device_id": bob_dev}))
        .await;
    assert_eq!(code(&r), "not_found");
    assert!(!hub.devices(&bob.id).await.unwrap().is_empty());
    assert_eq!(audit_count(&hub, "agent.key").await, before);
    assert_eq!(hub.devices(&id).await.unwrap().len(), keys_before);
    // The agent itself cannot manage keys (it owns nothing).
    let (_, d3) = hub.authenticate_device(&k3).unwrap();
    let r = Conn::device(&hub, &helper, &d3)
        .call(json!({"op": "agent_key", "agent_id": id}))
        .await;
    assert_eq!(code(&r), "forbidden", "{r}");
}

async fn key_limits_and_deactivated_agents(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let a = Conn::new(&hub, &alice);
    let r = a.ok(json!({"op": "create_agent", "name": "helper"})).await;
    let id = r["agent"]["id"].as_str().unwrap().to_string();
    let max = agentim_core::hub::MAX_AGENT_KEYS;
    for _ in 1..max {
        a.ok(json!({"op": "agent_key", "agent_id": id})).await;
    }
    assert_eq!(hub.devices(&id).await.unwrap().len(), max);
    let r = a.call(json!({"op": "agent_key", "agent_id": id})).await;
    assert_eq!(code(&r), "conflict", "{r}");
    assert_eq!(hub.devices(&id).await.unwrap().len(), max);
    // Rotating at the limit is still possible and leaves one.
    let r = a
        .ok(json!({"op": "agent_key", "agent_id": id, "revoke_others": true}))
        .await;
    assert_eq!(r["revoked"].as_array().unwrap().len(), max);
    assert_eq!(hub.devices(&id).await.unwrap().len(), 1);
    // A deactivated agent gets no new key and its key stops working.
    let key = r["key"].as_str().unwrap().to_string();
    hub.set_deactivated(&id, true).await.unwrap();
    assert!(hub.authenticate(&key).is_none());
    let r = a.call(json!({"op": "agent_key", "agent_id": id})).await;
    assert_eq!(code(&r), "conflict", "{r}");
    // Deactivation revoked its keys; the refused request added none.
    assert!(hub.devices(&id).await.unwrap().is_empty());
}

async fn keys_made_by_another_process_are_found(db: TestDb) {
    if db.url.contains(":memory:") {
        return;
    }
    let server = open(single(&db.url)).await;
    let alice = user(&server, "alice", UserKind::Human, None).await;
    // The `agent` command opens the same database in another process.
    let cli = open(single(&db.url)).await;
    let owner = cli.user_by_name("alice").unwrap();
    let r = cli.create_agent(&owner, "helper").await.unwrap();
    let key = r["key"].as_str().unwrap().to_string();
    let r2 = cli.create_agent(&owner, "second").await.unwrap();
    let key2 = r2["key"].as_str().unwrap().to_string();
    assert!(server.authenticate(&key).is_none(), "not in memory yet");
    let (u, _) = server
        .authenticate_or_load(&key)
        .await
        .expect("looked up in the database");
    assert_eq!(u.name, "helper");
    assert_eq!(u.owner_id.as_deref(), Some(alice.id.as_str()));
    assert!(server.user_by_name("helper").is_some());
    // At most one lookup per second: made-up tokens cannot load the database.
    assert!(server.authenticate_or_load(&key2).await.is_none());
    assert!(server.authenticate_or_load("atk_madeup").await.is_none());
    tokio::time::sleep(Duration::from_millis(1100)).await;
    assert!(server.authenticate_or_load(&key2).await.is_some());
    tokio::time::sleep(Duration::from_millis(1100)).await;
    assert!(server.authenticate_or_load("atk_madeup").await.is_none());
    // Now in memory: no lookup needed any more.
    assert!(server.authenticate_or_load(&key).await.is_some());
    // A key revoked through the server stays revoked.
    let agent = server.user_by_name("helper").unwrap();
    let (_, dev) = server.authenticate_device(&key).unwrap();
    Conn::new(&server, &alice)
        .ok(json!({"op": "revoke_agent_key", "agent_id": agent.id, "device_id": dev}))
        .await;
    tokio::time::sleep(Duration::from_millis(1100)).await;
    assert!(server.authenticate_or_load(&key).await.is_none());
}

async fn keys_and_rotations_survive_a_restart(db: TestDb) {
    let (alice, id, old, new);
    {
        let hub = open(single(&db.url)).await;
        alice = user(&hub, "alice", UserKind::Human, None).await;
        let a = Conn::new(&hub, &alice);
        let r = a.ok(json!({"op": "create_agent", "name": "helper"})).await;
        id = r["agent"]["id"].as_str().unwrap().to_string();
        old = r["key"].as_str().unwrap().to_string();
        let r = a
            .ok(json!({"op": "agent_key", "agent_id": id, "revoke_others": true}))
            .await;
        new = r["key"].as_str().unwrap().to_string();
    }
    let hub = open(single(&db.url)).await;
    assert!(hub.authenticate(&old).is_none());
    assert_eq!(hub.authenticate(&new).unwrap().id, id);
    let l = Conn::new(&hub, &alice)
        .ok(json!({"op": "list_agents"}))
        .await;
    assert_eq!(l["agents"][0]["keys"].as_array().unwrap().len(), 1);
}

/// Rotations of one agent racing each other: every one runs as one write,
/// so exactly one key is left, in the database and in memory, and it is one
/// of the keys handed out.
async fn race_rotations_leave_exactly_one_key(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let r = Conn::new(&hub, &alice)
        .ok(json!({"op": "create_agent", "name": "helper"}))
        .await;
    let id = r["agent"]["id"].as_str().unwrap().to_string();
    let mut all_keys = vec![r["key"].as_str().unwrap().to_string()];
    for round in 0..20 {
        let mut tasks = vec![];
        for _ in 0..6 {
            let (hub, alice, id) = (hub.clone(), alice.clone(), id.clone());
            tasks.push(tokio::spawn(async move {
                Conn::new(&hub, &alice)
                    .ok(json!({"op": "agent_key", "agent_id": id, "revoke_others": true}))
                    .await["key"]
                    .as_str()
                    .unwrap()
                    .to_string()
            }));
        }
        for t in tasks {
            all_keys.push(t.await.unwrap());
        }
        let stored = hub.devices(&id).await.unwrap();
        assert_eq!(stored.len(), 1, "round {round}: one key in the database");
        let working: Vec<&String> = all_keys
            .iter()
            .filter(|k| hub.authenticate(k).is_some())
            .collect();
        assert_eq!(working.len(), 1, "round {round}: one key works");
        assert_eq!(
            hub.authenticate_device(working[0]).unwrap().1,
            stored[0].device_id,
            "round {round}: memory and database agree"
        );
    }
}

/// Creations racing for the owner's last places: the cap holds exactly.
async fn race_creations_respect_the_cap(db: TestDb) {
    let hub = open(cfg_with(&db, |c| c.agents.max_per_owner = 3)).await;
    for round in 0..5 {
        let owner = user(&hub, &format!("owner{round}"), UserKind::Human, None).await;
        let mut tasks = vec![];
        for i in 0..10 {
            let (hub, owner) = (hub.clone(), owner.clone());
            tasks.push(tokio::spawn(async move {
                Conn::new(&hub, &owner)
                    .call(json!({"op": "create_agent", "name": format!("a{round}x{i}")}))
                    .await
            }));
        }
        let mut ok = 0;
        for t in tasks {
            let r = t.await.unwrap();
            if r["ok"] == true {
                ok += 1;
            } else {
                assert_eq!(code(&r), "conflict", "{r}");
            }
        }
        assert_eq!(ok, 3, "round {round}");
        assert_eq!(users_named(&hub, &owner).await.len(), 3);
        // The same name raced by two owners: one wins.
        let other = user(&hub, &format!("other{round}"), UserKind::Human, None).await;
        let (h1, h2) = (hub.clone(), hub.clone());
        let (o1, o2) = (owner.clone(), other.clone());
        let name = format!("same{round}");
        let (n1, n2) = (name.clone(), name.clone());
        let t1 = tokio::spawn(async move {
            Conn::new(&h1, &o2)
                .call(json!({"op": "create_agent", "name": n1}))
                .await
        });
        let t2 = tokio::spawn(async move {
            Conn::new(&h2, &o1)
                .call(json!({"op": "create_agent", "name": n2}))
                .await
        });
        let (r1, r2) = (t1.await.unwrap(), t2.await.unwrap());
        // `owner` is at the cap, so only `other` can win here; repeat with
        // two free owners.
        assert_eq!(r1["ok"], true, "{r1}");
        assert_eq!(code(&r2), "conflict", "{r2}");
        let third = user(&hub, &format!("third{round}"), UserKind::Human, None).await;
        let fourth = user(&hub, &format!("fourth{round}"), UserKind::Human, None).await;
        let name = format!("contested{round}");
        let mut tasks = vec![];
        for o in [third, fourth] {
            let (hub, name) = (hub.clone(), name.clone());
            tasks.push(tokio::spawn(async move {
                Conn::new(&hub, &o)
                    .call(json!({"op": "create_agent", "name": name}))
                    .await
            }));
        }
        let mut wins = 0;
        for t in tasks {
            let r = t.await.unwrap();
            if r["ok"] == true {
                wins += 1;
            } else {
                assert_eq!(code(&r), "conflict", "{r}");
            }
        }
        assert_eq!(wins, 1, "round {round}: one owner gets the name");
    }
}
