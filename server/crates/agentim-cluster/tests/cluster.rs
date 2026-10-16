//! Several nodes in one process on one real database (docs/11 §12). Runs on
//! PostgreSQL and MySQL (`AGENTIM_TEST_PG`, `AGENTIM_TEST_MYSQL`).

#[path = "../../agentim-core/tests/common/mod.rs"]
mod common;

use std::collections::HashSet;
use std::time::{Duration, Instant};

use agentim_cluster::Node;
use agentim_core::cluster::shard_of;
use agentim_core::config::{ClusterConfig, Config};
use agentim_core::hub::Hub;
use agentim_core::model::*;
use common::*;
use serde_json::{Value, json};
use sqlx::Connection;

macro_rules! cluster_tests {
    ($($name:ident),* $(,)?) => {
        mod postgres { $( #[tokio::test(flavor = "multi_thread", worker_threads = 4)] async fn $name() { match $crate::common::new_db($crate::common::Backend::Postgres).await { Some(db) => super::$name(db).await, None => eprintln!("skipped: AGENTIM_TEST_PG not set") } } )* }
        mod mysql { $( #[tokio::test(flavor = "multi_thread", worker_threads = 4)] async fn $name() { match $crate::common::new_db($crate::common::Backend::Mysql).await { Some(db) => super::$name(db).await, None => eprintln!("skipped: AGENTIM_TEST_MYSQL not set") } } )* }
    };
}

cluster_tests!(
    requests_are_forwarded_and_pushes_reach_every_node,
    a_crashed_holder_is_replaced_without_loss_or_duplicates,
    a_stale_holder_commits_nothing,
    one_message_per_client_msg_id_across_nodes,
    refusals_are_the_same_through_forwarding,
    revocations_and_blocks_reach_every_node,
    a_joining_node_gets_its_share_while_traffic_flows,
    leaving_hands_shards_over_at_once,
    start_up_refuses_bad_setups,
);

const SHARDS: u32 = 16;

fn cluster_cfg(url: &str, node: &str) -> Config {
    let mut c = single(url);
    c.cluster = ClusterConfig {
        enabled: true,
        node_id: node.into(),
        listen: "127.0.0.1:0".into(),
        shards: SHARDS,
        lease_ms: 3_000,
        renew_ms: 500,
        stop_ms: 2_000,
        op_timeout_ms: 500,
        ..Default::default()
    };
    c
}

async fn start_with(cfg: Config) -> Result<(Hub, Node), String> {
    let hub = Hub::open_deferred(cfg, hub_cfg())
        .await
        .map_err(|e| e.message)?;
    let node = agentim_cluster::join(&hub).await?;
    node.start().await?;
    hub.finish_open().await.map_err(|e| e.message)?;
    Ok((hub, node))
}

async fn start(url: &str, node: &str) -> (Hub, Node) {
    start_with(cluster_cfg(url, node))
        .await
        .expect("node starts")
}

async fn cluster(db: &TestDb, n: usize) -> Vec<(Hub, Node)> {
    let mut v = vec![];
    for i in 0..n {
        v.push(start(&db.url, &format!("n{i}")).await);
    }
    settled(&v).await;
    v
}

/// Every shard (and the control and upkeep shards) held by exactly one of
/// the nodes, each node holding some.
async fn settled(nodes: &[(Hub, Node)]) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let mut all = vec![];
        for (_, n) in nodes {
            all.extend(n.held(0));
        }
        let distinct: HashSet<u32> = all.iter().copied().collect();
        let spread = nodes.iter().all(|(_, n)| !n.held(0).is_empty());
        if all.len() == (SHARDS + 2) as usize && distinct.len() == all.len() && spread {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "not settled: {:?}",
            nodes.iter().map(|(_, n)| n.held(0)).collect::<Vec<_>>()
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn holder(nodes: &[(Hub, Node)], conv: &str) -> Option<usize> {
    let s = shard_of(conv, SHARDS);
    nodes.iter().position(|(_, n)| n.held(0).contains(&s))
}

async fn wait_for<F: FnMut() -> bool>(what: &str, within: Duration, mut f: F) {
    let deadline = Instant::now() + within;
    while !f() {
        assert!(Instant::now() < deadline, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Retries a send with the same client_msg_id until it is accepted
/// (`unavailable` means: retry).
async fn send_until_ok(c: &Conn, conv: &str, cmid: &str, text: &str, within: Duration) -> Value {
    let deadline = Instant::now() + within;
    loop {
        let r = c
            .call(json!({"op": "send", "conv_id": conv, "client_msg_id": cmid, "text": text}))
            .await;
        if r["ok"] == true {
            return r;
        }
        assert_eq!(r["error"]["code"], "unavailable", "{r}");
        assert!(Instant::now() < deadline, "never accepted: {r}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// The checks after every multi-node test (docs/11 §12.7): every accepted
/// message is in the log once, sequences run 1..n without gaps, and each
/// client_msg_id appears at most once per sender.
async fn check_log(c: &Conn, conv: &str, accepted: &[String]) {
    let log = c.pull(conv, 0).await;
    let seqs = seqs(&log);
    assert_eq!(seqs, (1..=seqs.len() as i64).collect::<Vec<_>>(), "gapless");
    let ids: Vec<String> = log["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|e| e["event"]["msg"]["msg_id"].as_str().map(String::from))
        .collect();
    for a in accepted {
        assert_eq!(
            ids.iter().filter(|i| *i == a).count(),
            1,
            "accepted message {a} once"
        );
    }
    let mut seen = HashSet::new();
    for e in log["events"].as_array().unwrap() {
        if let (Some(s), Some(m)) = (
            e["event"]["msg"]["sender_id"].as_str(),
            e["event"]["msg"]["client_msg_id"].as_str(),
        ) {
            assert!(
                seen.insert((s.to_string(), m.to_string())),
                "client_msg_id {m} twice"
            );
        }
    }
}

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
    let nodes = cluster(&db, 2).await;
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
        Duration::from_secs(5),
        || nodes[stays].1.held(0).len() == (SHARDS + 2) as usize,
    )
    .await;
    assert!(
        t0.elapsed() < Duration::from_secs(3),
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
