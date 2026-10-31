//! Several nodes in one process on one real database (docs/11 §12). Runs on
//! PostgreSQL and MySQL (`AGENTIM_TEST_PG`, `AGENTIM_TEST_MYSQL`).

#[path = "../../agentim-core/tests/common/mod.rs"]
mod common;

use std::collections::HashSet;
use std::time::{Duration, Instant};

use agentim_cluster::Node;
use agentim_core::cluster::shard_of;
use agentim_core::hub::Hub;
use agentim_core::model::*;
use common::*;
use serde_json::json;
use sqlx::Connection;

#[macro_use]
mod support;

use support::*;

cluster_tests!(
    requests_are_forwarded_and_pushes_reach_every_node,
    a_crashed_holder_is_replaced_without_loss_or_duplicates,
    a_stale_holder_commits_nothing,
    one_message_per_client_msg_id_across_nodes,
    refusals_are_the_same_through_forwarding,
    revocations_and_blocks_reach_every_node,
    signed_tokens_and_logouts_reach_every_node,
    a_joining_node_gets_its_share_while_traffic_flows,
    leaving_hands_shards_over_at_once,
    start_up_refuses_bad_setups,
    the_trace_crosses_nodes,
    dye_rules_reach_every_node_and_follow_the_request,
);

async fn requests_are_forwarded_and_pushes_reach_every_node(db: TestDb) {
    let nodes = cluster(&db, 3).await;
    let (h0, h1, h2) = (&nodes[0].0, &nodes[1].0, &nodes[2].0);
    let alice = user(h0, "alice", UserKind::Human, None).await;
    let bob = user(h1, "bob", UserKind::Human, None).await;
    let carol = user(h2, "carol", UserKind::Human, None).await;
    let a = Conn::new(h1, &alice);
    let mut b = Conn::new(h2, &bob);
    let mut c = Conn::new(h0, &carol);
    let conv = a.group(&[&bob, &carol]).await;
    // Messages from every node, whoever holds the conversation.
    let mut accepted = vec![];
    for (i, who) in [&a, &b, &c].iter().enumerate() {
        for k in 0..5 {
            let r = who.send(&conv, &format!("m{i}-{k}")).await;
            assert_eq!(r["ok"], true, "{r}");
            accepted.push(r["msg_id"].as_str().unwrap().to_string());
        }
    }
    // Live pushes reach connections on the other nodes, in order.
    for conn in [&mut b, &mut c] {
        let mut got = vec![];
        let deadline = Instant::now() + Duration::from_secs(5);
        while got.len() < 15 && Instant::now() < deadline {
            for f in conn.drain() {
                if f["type"] == "event" && f["event"]["msg"]["msg_id"].is_string() {
                    got.push(f["event"]["seq"].as_i64().unwrap());
                }
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let mut sorted = got.clone();
        sorted.sort();
        assert_eq!(got, sorted, "in order");
        assert_eq!(got.len(), 15, "every message pushed once: {got:?}");
    }
    for (h, _) in &nodes {
        check_log(&Conn::new(h, &alice), &conv, &accepted).await;
    }
    assert!(holder(&nodes, &conv).is_some());
}

async fn a_crashed_holder_is_replaced_without_loss_or_duplicates(db: TestDb) {
    let nodes = cluster(&db, 3).await;
    let alice = user(&nodes[0].0, "alice", UserKind::Human, None).await;
    let bob = user(&nodes[0].0, "bob", UserKind::Human, None).await;
    let conv = Conn::new(&nodes[0].0, &alice).group(&[&bob]).await;
    let h = holder(&nodes, &conv).unwrap();
    // Write from a node that is not the holder, and does not hold the
    // control shard either (so it keeps working).
    let ctl = nodes
        .iter()
        .position(|(_, n)| n.held(0).contains(&SHARDS))
        .unwrap();
    let w = (0..3).find(|i| *i != h && *i != ctl).unwrap_or((h + 1) % 3);
    let a = Conn::new(&nodes[w].0, &alice);
    let mut accepted = vec![];
    for k in 0..5 {
        accepted.push(
            a.send(&conv, &format!("before {k}")).await["msg_id"]
                .as_str()
                .unwrap()
                .to_string(),
        );
    }
    let crashed_at = Instant::now();
    nodes[h].1.crash();
    // The same client_msg_id is retried until a new holder accepts it.
    let r = send_until_ok(&a, &conv, "after-crash", "after", Duration::from_secs(15)).await;
    let took = crashed_at.elapsed();
    accepted.push(r["msg_id"].as_str().unwrap().to_string());
    assert!(took < Duration::from_secs(8), "takeover took {took:?}");
    for k in 0..5 {
        let r = a.send(&conv, &format!("later {k}")).await;
        assert_eq!(r["ok"], true, "{r}");
        accepted.push(r["msg_id"].as_str().unwrap().to_string());
    }
    let again = a
        .call(
            json!({"op": "send", "conv_id": conv, "client_msg_id": "after-crash", "text": "after"}),
        )
        .await;
    assert_eq!(again["msg_id"], r["msg_id"], "a retry returns the original");
    check_log(&a, &conv, &accepted).await;
    let alive: Vec<&(Hub, Node)> = nodes
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != h)
        .map(|(_, n)| n)
        .collect();
    wait_for(
        "every shard has one holder",
        Duration::from_secs(10),
        || {
            let all: Vec<u32> = alive.iter().flat_map(|(_, n)| n.held(0)).collect();
            let d: HashSet<u32> = all.iter().copied().collect();
            all.len() == (SHARDS + 2) as usize && d.len() == all.len()
        },
    )
    .await;
}

async fn a_stale_holder_commits_nothing(db: TestDb) {
    let nodes = cluster(&db, 2).await;
    let alice = user(&nodes[0].0, "alice", UserKind::Human, None).await;
    let bob = user(&nodes[0].0, "bob", UserKind::Human, None).await;
    let conv = Conn::new(&nodes[0].0, &alice).group(&[&bob]).await;
    let h = holder(&nodes, &conv).unwrap();
    let a = Conn::new(&nodes[h].0, &alice);
    let before = seqs(&a.pull(&conv, 0).await).len();
    let shard = shard_of(&conv, SHARDS);
    // Someone else took the shard behind this node's back: the epoch moved.
    let mut c = sqlx::AnyConnection::connect(&db.url).await.unwrap();
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "UPDATE shard_lease SET epoch = epoch + 1 WHERE db = 'main' AND shard_id = {shard}"
    )))
    .execute(&mut c)
    .await
    .unwrap();
    let r = a.send(&conv, "must not land").await;
    assert_eq!(r["error"]["code"], "unavailable", "{r}");
    let log = a.pull(&conv, 0).await;
    assert_eq!(seqs(&log).len(), before, "no trace of the refused batch");
    assert!(!log.to_string().contains("must not land"));
    // The node notices and takes the shard again under the new epoch.
    let r = send_until_ok(&a, &conv, "retry", "lands", Duration::from_secs(10)).await;
    assert_eq!(r["ok"], true);
    check_log(&a, &conv, &[r["msg_id"].as_str().unwrap().to_string()]).await;
}

async fn one_message_per_client_msg_id_across_nodes(db: TestDb) {
    let nodes = cluster(&db, 3).await;
    let alice = user(&nodes[0].0, "alice", UserKind::Human, None).await;
    let bob = user(&nodes[0].0, "bob", UserKind::Human, None).await;
    let conv = Conn::new(&nodes[0].0, &alice).group(&[&bob]).await;
    let base = seqs(&Conn::new(&nodes[0].0, &alice).pull(&conv, 0).await).len();
    for round in 0..10 {
        let cmid = format!("same-{round}");
        let tasks: Vec<_> = nodes
            .iter()
            .map(|(h, _)| {
                let (h, alice, conv, cmid) = (h.clone(), alice.clone(), conv.clone(), cmid.clone());
                tokio::spawn(async move {
                    let c = Conn::new(&h, &alice);
                    c.call(
                        json!({"op": "send", "conv_id": conv, "client_msg_id": cmid, "text": "x"}),
                    )
                    .await
                })
            })
            .collect();
        let mut ids = HashSet::new();
        for t in tasks {
            let r = t.await.unwrap();
            assert_eq!(r["ok"], true, "{r}");
            ids.insert(r["msg_id"].as_str().unwrap().to_string());
        }
        assert_eq!(ids.len(), 1, "one message for one client_msg_id");
    }
    let a = Conn::new(&nodes[1].0, &alice);
    assert_eq!(
        seqs(&a.pull(&conv, 0).await).len(),
        base + 10,
        "ten messages"
    );
    check_log(&a, &conv, &[]).await;
}

async fn refusals_are_the_same_through_forwarding(db: TestDb) {
    let nodes = cluster(&db, 2).await;
    let alice = user(&nodes[0].0, "alice", UserKind::Human, None).await;
    let bob = user(&nodes[0].0, "bob", UserKind::Human, None).await;
    let eve = user(&nodes[0].0, "eve", UserKind::Human, None).await;
    let conv = Conn::new(&nodes[0].0, &alice).group(&[&bob]).await;
    let h = holder(&nodes, &conv).unwrap();
    let other = 1 - h;
    let base = seqs(&Conn::new(&nodes[other].0, &alice).pull(&conv, 0).await).len();
    for i in [h, other] {
        let e = Conn::new(&nodes[i].0, &eve);
        let r = e.send(&conv, "not a member").await;
        assert_eq!(r["error"]["code"], "forbidden", "on node {i}: {r}");
        // The rule id comes back too, from the node that holds it.
        assert_eq!(r["error"]["rule"], "member.required", "on node {i}: {r}");
        let r = e.send("c_unknown", "nowhere").await;
        assert!(
            ["not_found", "forbidden"].contains(&r["error"]["code"].as_str().unwrap()),
            "{r}"
        );
        let a = Conn::new(&nodes[i].0, &alice);
        let r = a
            .call(json!({"op": "send", "conv_id": conv, "client_msg_id": "x", "text": ""}))
            .await;
        assert_eq!(r["error"]["code"], "bad_request", "{r}");
        assert_eq!(r["error"]["rule"], "msg.text_size", "{r}");
    }
    let log = Conn::new(&nodes[other].0, &alice).pull(&conv, 0).await;
    assert_eq!(seqs(&log).len(), base, "refusals leave no trace");
}

async fn revocations_and_blocks_reach_every_node(db: TestDb) {
    let nodes = cluster(&db, 3).await;
    let alice = user(&nodes[0].0, "alice", UserKind::Human, None).await;
    let bob = user(&nodes[0].0, "bob", UserKind::Human, None).await;
    let (token, _) = nodes[0]
        .0
        .issue_token(&bob.id, Some("phone"), None, None, "test")
        .await
        .unwrap();
    // Every node accepts the token (published with the commit).
    for (h, _) in &nodes {
        let t = token.clone();
        let h2 = h.clone();
        let deadline = Instant::now() + Duration::from_secs(3);
        while h2.authenticate_or_load(&t).await.is_none() {
            assert!(Instant::now() < deadline, "token unknown on a node");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    nodes[1].0.revoke_tokens(&bob.id, None).await.unwrap();
    let t0 = Instant::now();
    for (h, _) in &nodes {
        let h = h.clone();
        let t = token.clone();
        wait_for("revocation everywhere", Duration::from_secs(3), || {
            h.authenticate_device(&t).is_none()
        })
        .await;
    }
    assert!(t0.elapsed() <= Duration::from_secs(3));
    // A block written on one node refuses a direct message sent on another
    // at once (checked in the holder's transaction).
    let a2 = Conn::new(&nodes[2].0, &alice);
    let b0 = Conn::new(&nodes[0].0, &bob);
    let dm = a2
        .ok(json!({"op": "create_conv", "kind": "dm", "title": "d", "members": [bob.id]}))
        .await["conv_id"]
        .as_str()
        .unwrap()
        .to_string();
    b0.ok(json!({"op": "block", "user_id": alice.id})).await;
    for (h, _) in &nodes {
        let r = Conn::new(h, &alice).send(&dm, "hi").await;
        assert_eq!(r["error"]["code"], "blocked", "{r}");
    }
}

/// docs/13 §4.2: a signing key created on one node verifies on every node
/// (they reload it when told), a person's first login and device made on
/// one node are known on the others, and a logout or a revoked key on any
/// node refuses the token everywhere and disconnects its devices.
async fn signed_tokens_and_logouts_reach_every_node(db: TestDb) {
    let nodes = cluster(&db, 3).await;
    let v = nodes[0].0.create_signing_key("backend").await.unwrap();
    let (kid, secret) = (
        v["kid"].as_str().unwrap().to_string(),
        v["secret"].as_str().unwrap().to_string(),
    );
    let now = || agentim_core::model::now_ms() / 1000;
    let t = agentim_core::auth::jwt_sign(&kid, &secret, "dana", now(), now() + 600);
    // The key reaches every node.
    for (h, _) in &nodes {
        let (h, t) = (h.clone(), t.clone());
        let deadline = Instant::now() + Duration::from_secs(3);
        while h
            .authenticate_client(&t, Some("probe"), None, None)
            .await
            .is_err()
        {
            assert!(Instant::now() < deadline, "key unknown on a node");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    // One person, created once, with one device per app device id.
    let dana = nodes[0].0.user_by_name("dana").unwrap();
    for (h, _) in &nodes {
        assert_eq!(h.user_by_name("dana").unwrap().id, dana.id);
    }
    let mut logins = vec![];
    for (i, (h, _)) in nodes.iter().enumerate() {
        let l = h
            .authenticate_client(&t, Some(&format!("dev{i}")), None, None)
            .await
            .unwrap();
        let s = h.open_device_session(&l);
        assert!(s.2.is_none());
        logins.push((h.clone(), l, s.0));
    }
    // Logging one device out on node 1 cuts it off on every node.
    let dev0 = logins[0].1.device_id.clone();
    nodes[1].0.log_out(&dana.id, Some(&dev0)).await.unwrap();
    wait_for("device logout everywhere", Duration::from_secs(3), || {
        logins[0].2.is_kicked()
    })
    .await;
    for (h, _) in &nodes {
        let (h, t) = (h.clone(), t.clone());
        let deadline = Instant::now() + Duration::from_secs(3);
        while h
            .authenticate_client(&t, Some("dev0"), None, None)
            .await
            .is_ok()
        {
            assert!(Instant::now() < deadline, "logout not seen on a node");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    assert!(logins[1].0.login_valid(&logins[1].1), "other devices go on");
    // Revoking the key on node 2: refused everywhere, devices closed.
    nodes[2].0.revoke_signing_key(&kid).await.unwrap();
    for (h, l, _) in &logins[1..] {
        let (h, l) = (h.clone(), l.clone());
        wait_for("key revocation everywhere", Duration::from_secs(3), || {
            !h.login_valid(&l)
        })
        .await;
    }
    for (h, _) in &nodes {
        assert!(
            h.authenticate_client(&t, Some("dev1"), None, None)
                .await
                .is_err()
        );
    }
}

async fn a_joining_node_gets_its_share_while_traffic_flows(db: TestDb) {
    let first = start(&db.url, "n0").await;
    let alice = user(&first.0, "alice", UserKind::Human, None).await;
    let bob = user(&first.0, "bob", UserKind::Human, None).await;
    let a = Conn::new(&first.0, &alice);
    let mut convs = vec![];
    for _ in 0..6 {
        convs.push(a.group(&[&bob]).await);
    }
    // Traffic on every conversation while a second and third node join.
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let sender = {
        let (h, alice, convs, stop) = (first.0.clone(), alice.clone(), convs.clone(), stop.clone());
        tokio::spawn(async move {
            let c = Conn::new(&h, &alice);
            let mut accepted = vec![];
            let mut k = 0;
            while !stop.load(std::sync::atomic::Ordering::SeqCst) {
                for conv in &convs {
                    let r = send_until_ok(&c, conv, &format!("j{k}"), "x", Duration::from_secs(10))
                        .await;
                    accepted.push((conv.clone(), r["msg_id"].as_str().unwrap().to_string()));
                    k += 1;
                }
            }
            accepted
        })
    };
    let second = start(&db.url, "n1").await;
    let third = start(&db.url, "n2").await;
    let nodes = vec![first, second, third];
    settled(&nodes).await;
    stop.store(true, std::sync::atomic::Ordering::SeqCst);
    let accepted = sender.await.unwrap();
    assert!(accepted.len() > 6);
    for conv in &convs {
        let mine: Vec<String> = accepted
            .iter()
            .filter(|(c, _)| c == conv)
            .map(|(_, m)| m.clone())
            .collect();
        check_log(&Conn::new(&nodes[2].0, &alice), conv, &mine).await;
    }
}

async fn leaving_hands_shards_over_at_once(db: TestDb) {
    // A long lease, so taking over well before it ends cannot be expiry.
    let mut nodes = vec![];
    for i in 0..2 {
        let mut cfg = cluster_cfg(&db.url, &format!("n{i}"));
        cfg.cluster.lease_ms = 15_000;
        cfg.cluster.stop_ms = 10_000;
        nodes.push(start_with(cfg).await.expect("node starts"));
    }
    settled(&nodes).await;
    let alice = user(&nodes[0].0, "alice", UserKind::Human, None).await;
    let bob = user(&nodes[0].0, "bob", UserKind::Human, None).await;
    let mut convs = vec![];
    for _ in 0..8 {
        convs.push(Conn::new(&nodes[0].0, &alice).group(&[&bob]).await);
    }
    let leaving = holder(&nodes, &convs[0]).unwrap();
    let stays = 1 - leaving;
    let t0 = Instant::now();
    nodes[leaving].1.leave().await;
    nodes[stays].1.nudge();
    wait_for(
        "the other node takes everything",
        Duration::from_secs(10),
        || nodes[stays].1.held(0).len() == (SHARDS + 2) as usize,
    )
    .await;
    assert!(
        t0.elapsed() < Duration::from_secs(8),
        "no wait for expiry: {:?}",
        t0.elapsed()
    );
    let a = Conn::new(&nodes[stays].0, &alice);
    for conv in &convs {
        let r = a.send(conv, "after leave").await;
        assert_eq!(r["ok"], true, "{r}");
        check_log(&a, conv, &[]).await;
    }
}

async fn start_up_refuses_bad_setups(db: TestDb) {
    let (_h, n) = start(&db.url, "n0").await;
    // The shard count is fixed once the cluster exists.
    let mut cfg = cluster_cfg(&db.url, "n1");
    cfg.cluster.shards = SHARDS * 2;
    let e = start_with(cfg).await.err().unwrap();
    assert!(e.contains("cannot change"), "{e}");
    // A node id in use by a running node.
    let t0 = Instant::now();
    let e = start_with(cluster_cfg(&db.url, "n0")).await.err().unwrap();
    assert!(e.contains("in use"), "{e}");
    assert!(t0.elapsed() < Duration::from_secs(10));
    // A wrong cluster secret cannot open the CA.
    let mut cfg = cluster_cfg(&db.url, "n2");
    cfg.cluster.secret = Some("another secret".into());
    let e = start_with(cfg).await.err().unwrap();
    assert!(e.contains("secret"), "{e}");
    // Timing rules.
    let mut cfg = cluster_cfg(&db.url, "n3");
    cfg.cluster.stop_ms = cfg.cluster.lease_ms;
    assert!(start_with(cfg).await.is_err());
    // After the node stops for good, its id can be used again at once.
    n.leave().await;
    let (_h2, n2) = start(&db.url, "n0").await;
    wait_for("all shards", Duration::from_secs(5), || {
        n2.held(0).len() == (SHARDS + 2) as usize
    })
    .await;
    assert_eq!(n2.slot(), n.slot(), "a node keeps its slot");
}

/// docs/14 §4.1: a request forwarded to the holder keeps its trace; the
/// writer on the other node stores it with the message.
async fn the_trace_crosses_nodes(db: TestDb) {
    let nodes = cluster(&db, 3).await;
    let alice = user(&nodes[0].0, "alice", UserKind::Human, None).await;
    let bot = user(&nodes[0].0, "bot", UserKind::Agent, Some(&alice)).await;
    let a = Conn::new(&nodes[0].0, &alice);
    let conv = conv_held_by(&nodes, 2, &a, &[&bot]).await;
    let g = Conn::new(&nodes[0].0, &bot);
    let trace = "4bf92f3577b34da6a3ce929d0e0e4736";
    let r = g
        .call(
            json!({"op": "send", "conv_id": conv, "text": "forwarded", "client_msg_id": "t1",
            "trace": format!("00-{trace}-00f067aa0ba902b7-01")}),
        )
        .await;
    assert_eq!(r["ok"], true, "{r}");
    let id = r["msg_id"].as_str().unwrap();
    // Read from every node: the same stored trace.
    for (h, _) in &nodes {
        assert_eq!(
            h.message_trace_id(id).await.unwrap().as_deref(),
            Some(trace)
        );
    }
}

/// docs/14 §4.2: a rule added on one node is in force on all; a dyed
/// request forwarded to the holder keeps its dye, and the event published
/// to a third node reaches the agent there sampled.
async fn dye_rules_reach_every_node_and_follow_the_request(db: TestDb) {
    let nodes = cluster(&db, 3).await;
    let maya = user(&nodes[0].0, "maya", UserKind::Human, None).await;
    let bot = user(&nodes[0].0, "bot", UserKind::Agent, Some(&maya)).await;
    let rule = nodes[0]
        .0
        .add_dye_rule("user", "maya", "ticket", None, "admin:t")
        .await
        .unwrap();
    for (h, _) in &nodes {
        let h = h.clone();
        let id = rule.id.clone();
        wait_for(
            "the rule on every node",
            Duration::from_secs(10),
            move || h.dye_rules().iter().any(|r| r.id == id),
        )
        .await;
    }
    let m = Conn::new(&nodes[1].0, &maya);
    let conv = conv_held_by(&nodes, 2, &m, &[&bot]).await;
    let mut g = Conn::new(&nodes[0].0, &bot);
    g.drain();
    let r = m.send(&conv, "@bot hi").await;
    assert_eq!(r["ok"], true, "{r}");
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut trace = None;
    while trace.is_none() && Instant::now() < deadline {
        for f in g.drain() {
            if f["type"] == "event" && f["event"]["msg"]["msg_id"] == r["msg_id"] {
                trace = f["trace"].as_str().map(String::from);
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let trace = trace.expect("pushed with its trace");
    assert!(trace.ends_with("-01"), "dyed, so sampled: {trace}");
    assert!(!trace.contains(&rule.id));
    // Removed on one node, gone on all.
    nodes[2].0.remove_dye_rule(&rule.id).await.unwrap();
    for (h, _) in &nodes {
        let h = h.clone();
        wait_for(
            "the rule gone everywhere",
            Duration::from_secs(10),
            move || h.dye_rules().is_empty(),
        )
        .await;
    }
}
