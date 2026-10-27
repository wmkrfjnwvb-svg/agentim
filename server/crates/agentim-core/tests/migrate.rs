//! Moving agents (docs/03-agents.md §7.3): a new environment on the same server
//! with a new key, and an agent package exported from one server and
//! imported into another: what goes, what never goes, tampering, limits,
//! and concurrent imports.

mod common;

use agentim_core::hub::Hub;
use agentim_core::model::*;
use common::*;
use serde_json::{Value, json};

backend_tests!(
    a_new_environment_carries_on_with_a_new_key,
    a_package_moves_an_agent_to_another_server,
    export_includes_only_what_the_owner_may_see,
    import_refusals_create_nothing,
    big_conversations_are_archived_in_chunks,
    race_one_import_per_name,
    packages_are_signed_and_unverifiable_ones_need_consent,
);

fn code(v: &Value) -> &str {
    v["error"]["code"].as_str().unwrap_or("")
}

/// A second, empty database of the same kind.
async fn another(db: &TestDb) -> TestDb {
    let b = if db.url.starts_with("postgres") {
        Backend::Postgres
    } else if db.url.starts_with("mysql") {
        Backend::Mysql
    } else {
        Backend::Sqlite
    };
    new_db(b).await.unwrap()
}

async fn audits(hub: &Hub, action: &str) -> usize {
    hub.audit_log(10_000)
        .await
        .unwrap()
        .iter()
        .filter(|a| a["action"] == action)
        .count()
}

/// A server with alice, her agent `bot` (key returned), bob, a group of
/// alice, bob and bot, a direct conversation of bob and bot, checkpoints
/// and a summary.
async fn source(db: &TestDb) -> (Hub, User, User, User, String, String, String) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let made = hub.create_agent(&alice, "bot").await.unwrap();
    let key = made["key"].as_str().unwrap().to_string();
    let bot = hub.user_by_name("bot").unwrap();
    let a = Conn::new(&hub, &alice);
    let group = a.group(&[&bob, &bot]).await;
    let dm = Conn::new(&hub, &bob)
        .ok(json!({"op": "create_conv", "kind": "dm", "title": "dm", "members": [bot.id]}))
        .await["conv_id"]
        .as_str()
        .unwrap()
        .to_string();
    a.send(&group, "@bot plan the launch").await;
    a.send(&group, "not addressed to the bot").await;
    Conn::new(&hub, &bob)
        .send(&dm, "bob's private question")
        .await;
    let g = Conn::new(&hub, &bot);
    g.ok(
        json!({"op": "send", "conv_id": group, "client_msg_id": "1", "text": "launch plan ready"}),
    )
    .await;
    g.ok(json!({"op": "state_put", "key": "plan", "value": {"step": 3, "notes": ["ü", 1]}, "version": 0}))
        .await;
    g.ok(json!({"op": "state_put", "key": "plan", "value": {"step": 4}, "version": 1}))
        .await;
    g.ok(json!({"op": "state_put", "key": "cfg/model", "value": "x-large", "version": 0}))
        .await;
    g.ok(json!({"op": "put_summary", "conv_id": group, "from_seq": 1, "to_seq": 4, "text": "launch talk so far"}))
        .await;
    (hub, alice, bob, bot, key, group, dm)
}

async fn a_new_environment_carries_on_with_a_new_key(db: TestDb) {
    let (hub, alice, _bob, bot, old_key, group, _dm) = source(&db).await;
    // The owner issues a key for the new environment and revokes the old.
    let r = hub.agent_key(&alice, &bot.id, true).await.unwrap();
    let new_key = r["key"].as_str().unwrap().to_string();
    assert!(hub.authenticate_device(&old_key).is_none());
    let (u, device) = hub.authenticate_device(&new_key).unwrap();
    assert_eq!(u.id, bot.id);
    // Everything is on the server: checkpoints, context, recent messages.
    let g = Conn::device(&hub, &bot, &device);
    let s = g.ok(json!({"op": "state_get", "key": "plan"})).await;
    assert_eq!(
        (&s["value"], &s["version"]),
        (&json!({"step": 4}), &json!(2))
    );
    let c = g.ok(json!({"op": "context", "conv_id": group})).await;
    assert_eq!(c["summary"]["text"], "launch talk so far");
    let synced = g.ok(json!({"op": "sync"})).await.to_string();
    assert!(synced.contains("launch plan ready"), "{synced}");
    // It carries on where the old one left off.
    g.ok(json!({"op": "state_put", "key": "plan", "value": {"step": 5}, "version": 2}))
        .await;
}

async fn a_package_moves_an_agent_to_another_server(db: TestDb) {
    let (hub, alice, _bob, bot, key, _group, _dm) = source(&db).await;
    let pkg = hub.export_agent(&alice, &bot.id, true).await.unwrap();
    assert_eq!(pkg["format"], "agentim.agent/1");
    assert_eq!(pkg["agent"]["name"], "bot");
    assert_eq!(audits(&hub, "agent.exported").await, 1);
    // Never: keys or credentials.
    let text = pkg.to_string();
    assert!(!text.contains(&key), "the key leaked");
    for d in hub.devices(&bot.id).await.unwrap() {
        assert!(!text.contains(&d.hash), "a token hash leaked");
    }
    // Into another server, for carol.
    let db2 = another(&db).await;
    let hub2 = open(single(&db2.url)).await;
    let carol = user(&hub2, "carol", UserKind::Human, None).await;
    let r = hub2.import_agent(&carol, &pkg, None, true).await.unwrap();
    assert_eq!(r["agent"]["name"], "bot");
    assert_eq!(r["agent"]["owner_id"], json!(carol.id));
    assert_eq!(r["imported"]["state"], 2);
    let new_key = r["key"].as_str().unwrap();
    let (moved, _) = hub2.authenticate_device(new_key).unwrap();
    assert_ne!(moved.id, bot.id, "a new identity on the new server");
    assert!(hub2.authenticate_device(&key).is_none());
    let g = Conn::new(&hub2, &moved);
    let s = g.ok(json!({"op": "state_get", "key": "plan"})).await;
    assert_eq!(
        (&s["value"], &s["version"]),
        (&json!({"step": 4}), &json!(1))
    );
    assert_eq!(
        g.ok(json!({"op": "state_get", "key": "cfg/model"})).await["value"],
        "x-large"
    );
    // Summaries and conversation content are read-only archive.
    let sm = g
        .ok(json!({"op": "state_get", "key": "archive/summaries/0"}))
        .await;
    assert_eq!(sm["value"][0]["text"], "launch talk so far");
    let listed = g
        .ok(json!({"op": "state_list", "prefix": "archive/conv/"}))
        .await;
    let keys: Vec<&str> = listed["keys"]
        .as_array()
        .unwrap()
        .iter()
        .map(|k| k["key"].as_str().unwrap())
        .collect();
    assert_eq!(keys.len(), 2, "{keys:?}");
    assert!(keys.iter().any(|k| k.ends_with("/meta")));
    let chunk = keys.iter().find(|k| k.ends_with("/0")).unwrap();
    let ev = g.ok(json!({"op": "state_get", "key": chunk})).await;
    let ev = ev["value"].to_string();
    assert!(ev.contains("launch plan ready") && ev.contains("@bot plan the launch"));
    assert_eq!(
        code(
            &g.call(json!({"op": "state_put", "key": chunk, "value": 1, "version": 1}))
                .await
        ),
        "bad_request"
    );
    assert_eq!(audits(&hub2, "agent.imported").await, 1);
    // Round trip: exported again, the checkpoints are the same.
    let again = hub2.export_agent(&carol, &moved.id, false).await.unwrap();
    let plan = |p: &Value| {
        p["state"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["key"] == "plan")
            .unwrap()["value"]
            .clone()
    };
    assert_eq!(plan(&again), plan(&pkg));
    // A new name on import; the original stays importable once.
    let r = hub2
        .import_agent(&carol, &pkg, Some("bot-copy"), true)
        .await
        .unwrap();
    assert_eq!(r["agent"]["name"], "bot-copy");
    assert_eq!(
        hub2.import_agent(&carol, &pkg, None, true)
            .await
            .unwrap_err()
            .code,
        "conflict"
    );
}

async fn export_includes_only_what_the_owner_may_see(db: TestDb) {
    let (hub, alice, bob, bot, _key, group, dm) = source(&db).await;
    let pkg = hub.export_agent(&alice, &bot.id, true).await.unwrap();
    let convs = pkg["conversations"].as_array().unwrap();
    // Alice is not in bob's direct conversation with the bot.
    assert_eq!(convs.len(), 1);
    assert_eq!(convs[0]["conv_id"], json!(group));
    assert!(!pkg.to_string().contains("bob's private question"));
    assert!(!pkg.to_string().contains(&dm));
    // Only what the agent could see (addressed).
    let texts: Vec<&str> = convs[0]["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["content"].as_str().unwrap())
        .collect();
    assert_eq!(texts, vec!["@bot plan the launch", "launch plan ready"]);
    // Without the flag: no conversation content at all.
    let lean = hub.export_agent(&alice, &bot.id, false).await.unwrap();
    assert!(lean["conversations"].is_null());
    assert!(!lean.to_string().contains("launch plan ready"));
    assert_eq!(lean["summaries"][0]["text"], "launch talk so far");
    // Only the owner exports; a destroyed agent cannot be.
    assert_eq!(
        hub.export_agent(&bob, &bot.id, false)
            .await
            .unwrap_err()
            .code,
        "forbidden"
    );
    hub.destroy_agent(&alice, &bot.id, "bot").await.unwrap();
    assert_eq!(
        hub.export_agent(&alice, &bot.id, false)
            .await
            .unwrap_err()
            .code,
        "not_found"
    );
}

async fn import_refusals_create_nothing(db: TestDb) {
    let (hub, alice, _bob, bot, _key, _group, _dm) = source(&db).await;
    let pkg = hub.export_agent(&alice, &bot.id, true).await.unwrap();
    let db2 = another(&db).await;
    let mut cfg = single(&db2.url);
    cfg.agents.state_keys = 3;
    cfg.agents.max_per_owner = 1;
    let hub2 = open(cfg).await;
    let carol = user(&hub2, "carol", UserKind::Human, None).await;
    let mut tampered = pkg.clone();
    tampered["state"][0]["value"] = json!("evil");
    let mut wrong = pkg.clone();
    wrong["format"] = json!("other/1");
    let cases: Vec<(Value, Option<&str>, &str)> = vec![
        (tampered, None, "bad_request"),
        (wrong, None, "bad_request"),
        (json!({}), None, "bad_request"),
        (pkg.clone(), Some("_bad name"), "bad_request"),
        // 2 checkpoints + summaries + meta + one chunk = 5 > 3.
        (pkg.clone(), None, "rate_limited"),
    ];
    for (p, name, want) in cases {
        assert_eq!(
            hub2.import_agent(&carol, &p, name, true)
                .await
                .unwrap_err()
                .code,
            want,
            "{name:?}"
        );
    }
    let agents = hub2.list_agents(&carol).await.unwrap();
    assert_eq!(agents["agents"], json!([]), "nothing was created");
    assert_eq!(audits(&hub2, "agent.imported").await, 0);
    // An agent cannot import; the owner's cap applies.
    let lean = hub.export_agent(&alice, &bot.id, false).await.unwrap();
    hub2.create_agent(&carol, "first").await.unwrap();
    assert_eq!(
        hub2.import_agent(&carol, &lean, None, true)
            .await
            .unwrap_err()
            .code,
        "conflict"
    );
    let first = hub2.user_by_name("first").unwrap();
    assert_eq!(
        hub2.import_agent(&first, &lean, None, true)
            .await
            .unwrap_err()
            .code,
        "forbidden"
    );
}

async fn big_conversations_are_archived_in_chunks(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    let a = Conn::new(&hub, &alice);
    let conv = a.group(&[&bot]).await;
    let line = "x".repeat(3000);
    for i in 0..400 {
        a.send(&conv, &format!("@bot {i} {line}")).await;
    }
    let pkg = hub.export_agent(&alice, &bot.id, true).await.unwrap();
    let events = pkg["conversations"][0]["events"].as_array().unwrap();
    assert_eq!(events.len(), 400);
    let seqs: Vec<i64> = events.iter().map(|e| e["seq"].as_i64().unwrap()).collect();
    let mut sorted = seqs.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(seqs, sorted, "in order, once");
    let db2 = another(&db).await;
    let hub2 = open(single(&db2.url)).await;
    let carol = user(&hub2, "carol", UserKind::Human, None).await;
    let r = hub2.import_agent(&carol, &pkg, None, true).await.unwrap();
    let (moved, _) = hub2
        .authenticate_device(r["key"].as_str().unwrap())
        .unwrap();
    let g = Conn::new(&hub2, &moved);
    let listed = g
        .ok(json!({"op": "state_list", "prefix": "archive/conv/"}))
        .await;
    let chunks: Vec<String> = listed["keys"]
        .as_array()
        .unwrap()
        .iter()
        .map(|k| k["key"].as_str().unwrap().to_string())
        .filter(|k| !k.ends_with("/meta"))
        .collect();
    assert!(chunks.len() >= 2, "{chunks:?}");
    let mut back = vec![];
    for k in &chunks {
        let v = g.ok(json!({"op": "state_get", "key": k})).await;
        assert!(v["value"].to_string().len() <= 1024 * 1024);
        back.extend(v["value"].as_array().unwrap().clone());
    }
    back.sort_by_key(|e| e["seq"].as_i64().unwrap());
    assert_eq!(&back, events);
}

/// The same package imported concurrently under one name: exactly one
/// agent, with all its checkpoints.
async fn race_one_import_per_name(db: TestDb) {
    let (hub, alice, _bob, bot, _key, _group, _dm) = source(&db).await;
    let pkg = hub.export_agent(&alice, &bot.id, false).await.unwrap();
    let db2 = another(&db).await;
    let hub2 = open(single(&db2.url)).await;
    let carol = user(&hub2, "carol", UserKind::Human, None).await;
    for round in 0..8 {
        let name = format!("bot{round}");
        let mut tasks = vec![];
        for _ in 0..4 {
            let (h, c, p, n) = (hub2.clone(), carol.clone(), pkg.clone(), name.clone());
            tasks.push(tokio::spawn(async move {
                h.import_agent(&c, &p, Some(&n), true).await
            }));
        }
        let mut ok = 0;
        for t in tasks {
            match t.await.unwrap() {
                Ok(_) => ok += 1,
                Err(e) => assert_eq!(e.code, "conflict", "{}", e.message),
            }
        }
        assert_eq!(ok, 1, "round {round}");
        let a = hub2.user_by_name(&name).unwrap();
        let g = Conn::new(&hub2, &a);
        let n = g.ok(json!({"op": "state_list"})).await["keys"]
            .as_array()
            .unwrap()
            .len();
        assert_eq!(n, 3, "round {round}: plan, cfg/model, summaries");
    }
}

/// D131: with a signing key the package is signed (HMAC-SHA256 over its
/// hash); the same deployment imports it as verified. A package changed and
/// re-hashed is refused; one this deployment cannot verify (unsigned, from
/// another deployment, or its key revoked) needs `allow_unverified`.
async fn packages_are_signed_and_unverifiable_ones_need_consent(db: TestDb) {
    let (hub, alice, _bob, bot, _key, _group, _dm) = source(&db).await;
    // No signing key yet: unsigned.
    let unsigned = hub.export_agent(&alice, &bot.id, false).await.unwrap();
    assert!(unsigned.get("signature").is_none());
    let kid = hub.create_signing_key("app").await.unwrap()["kid"]
        .as_str()
        .unwrap()
        .to_string();
    let pkg = hub.export_agent(&alice, &bot.id, false).await.unwrap();
    assert_eq!(pkg["signature"]["alg"], "HMAC-SHA256");
    assert_eq!(pkg["signature"]["kid"], json!(kid));
    // A newer key signs from now on.
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    let kid2 = hub.create_signing_key("app2").await.unwrap()["kid"]
        .as_str()
        .unwrap()
        .to_string();
    let pkg2 = hub.export_agent(&alice, &bot.id, false).await.unwrap();
    assert_eq!(pkg2["signature"]["kid"], json!(kid2));
    // Refusals first, so the name stays free: they create nothing.
    let mut forged = pkg.clone();
    forged["state"][0]["value"] = json!("evil");
    let rehash = |p: &mut Value| {
        let mut bare = p.clone();
        let o = bare.as_object_mut().unwrap();
        o.remove("sha256");
        o.remove("signature");
        let d = ring::digest::digest(&ring::digest::SHA256, bare.to_string().as_bytes());
        p["sha256"] = json!(
            d.as_ref()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        );
    };
    rehash(&mut forged);
    let mut bad_sig = pkg.clone();
    bad_sig["signature"]["value"] = json!("AAAA");
    let mut bad_alg = pkg.clone();
    bad_alg["signature"]["alg"] = json!("none");
    for (p, allow, want) in [
        (&forged, true, "bad_request"),
        (&bad_sig, true, "bad_request"),
        (&bad_alg, true, "bad_request"),
        (&unsigned, false, "forbidden"),
    ] {
        let e = hub
            .import_agent(&alice, p, Some("copy"), allow)
            .await
            .unwrap_err();
        assert_eq!(e.code, want, "{}", e.message);
    }
    assert_eq!(
        audits(&hub, "agent.imported").await,
        0,
        "refusals leave no trace"
    );
    assert!(hub.user_by_name("copy").is_none());
    // Verified without consent; the audit records it.
    let r = hub
        .import_agent(&alice, &pkg, Some("copy"), false)
        .await
        .unwrap();
    assert_eq!(r["verified"], true);
    let log = hub.audit_log(1000).await.unwrap();
    let imported = log
        .iter()
        .find(|a| a["action"] == "agent.imported")
        .unwrap();
    assert_eq!(imported["detail"]["verified"], true, "{imported}");
    // Unsigned, with consent: imported, marked unverified.
    let r = hub
        .import_agent(&alice, &unsigned, Some("copy2"), true)
        .await
        .unwrap();
    assert_eq!(r["verified"], false);
    // Another deployment does not have the key: unverifiable.
    let db2 = another(&db).await;
    let hub2 = open(single(&db2.url)).await;
    let carol = user(&hub2, "carol", UserKind::Human, None).await;
    assert_eq!(
        hub2.import_agent(&carol, &pkg, None, false)
            .await
            .unwrap_err()
            .code,
        "forbidden"
    );
    assert_eq!(
        hub2.import_agent(&carol, &pkg, None, true).await.unwrap()["verified"],
        false
    );
    // A revoked key no longer vouches for what it signed.
    hub.revoke_signing_key(&kid).await.unwrap();
    assert_eq!(
        hub.import_agent(&alice, &pkg, Some("copy3"), false)
            .await
            .unwrap_err()
            .code,
        "forbidden"
    );
    assert_eq!(
        hub.import_agent(&alice, &pkg2, Some("copy3"), false)
            .await
            .unwrap()["verified"],
        true
    );
}
