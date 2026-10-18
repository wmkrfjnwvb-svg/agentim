//! Planned operations without users noticing (docs/11 C2): nodes joining
//! and leaving under traffic, readiness when leases cannot be renewed,
//! held and delayed notifications, adjacent versions side by side, and the
//! position check. PostgreSQL and MySQL.

#[path = "../../agentim-core/tests/common/mod.rs"]
mod common;

#[macro_use]
mod support;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use agentim_core::failpoint::Action;
use agentim_core::model::*;
use common::*;
use serde_json::{Value, json};
use support::*;

cluster_tests!(
    nodes_joining_under_traffic_cost_no_request,
    a_node_leaving_under_traffic_costs_no_request,
    failing_renewals_stop_a_node_taking_clients,
    held_and_delayed_notifications_arrive_in_order,
    outputs_of_a_newer_node_are_skipped_one_by_one,
    commands_this_version_does_not_know_are_unsupported,
    heads_from_any_node_show_where_conversations_stand,
    mutation_without_the_cache_poll_a_cut_off_node_keeps_a_revoked_token,
);

/// Senders that send each message once (no retry): every answer must be
/// a success, or the operation was noticed.
fn senders(
    hubs: &[agentim_core::hub::Hub],
    alice: &User,
    convs: &[String],
    stop: Arc<AtomicBool>,
    h: Arc<History>,
) -> Vec<tokio::task::JoinHandle<Vec<Outcome>>> {
    hubs.iter()
        .enumerate()
        .map(|(k, hub)| {
            let (hub, alice, convs, stop, h) = (
                hub.clone(),
                alice.clone(),
                convs.to_vec(),
                stop.clone(),
                h.clone(),
            );
            tokio::spawn(async move {
                let c = Conn::new(&hub, &alice);
                let mut bad = vec![];
                let mut n = 0;
                while !stop.load(Ordering::SeqCst) {
                    let o = h
                        .send(
                            &c,
                            &convs[n % convs.len()],
                            &format!("h{k}-{n}"),
                            Duration::from_secs(10),
                        )
                        .await;
                    if !matches!(o, Outcome::Ok(_)) {
                        bad.push(o);
                    }
                    n += 1;
                }
                bad
            })
        })
        .collect()
}

async fn nodes_joining_under_traffic_cost_no_request(db: TestDb) {
    let first = start(&db.url, "n0").await;
    let alice = user(&first.0, "alice", UserKind::Human, None).await;
    let bob = user(&first.0, "bob", UserKind::Human, None).await;
    let a = Conn::new(&first.0, &alice);
    let mut convs = vec![];
    for _ in 0..8 {
        convs.push(a.group(&[&bob]).await);
    }
    let stop = Arc::new(AtomicBool::new(false));
    let h = Arc::new(History::default());
    let tasks = senders(
        std::slice::from_ref(&first.0),
        &alice,
        &convs,
        stop.clone(),
        h.clone(),
    );
    let second = start(&db.url, "n1").await;
    let third = start(&db.url, "n2").await;
    let nodes = vec![first, second, third];
    settled(&nodes).await;
    // A while longer, so hand-overs happen under traffic.
    tokio::time::sleep(Duration::from_secs(1)).await;
    stop.store(true, Ordering::SeqCst);
    for t in tasks {
        let bad = t.await.unwrap();
        assert!(bad.is_empty(), "requests noticed the hand-overs: {bad:?}");
    }
    assert!(h.accepted() > 20, "{}", h.accepted());
    h.check(&a).await;
}

async fn a_node_leaving_under_traffic_costs_no_request(db: TestDb) {
    let nodes = cluster(&db, 3).await;
    let alice = user(&nodes[0].0, "alice", UserKind::Human, None).await;
    let bob = user(&nodes[0].0, "bob", UserKind::Human, None).await;
    let a = Conn::new(&nodes[0].0, &alice);
    let mut convs = vec![];
    for _ in 0..8 {
        convs.push(a.group(&[&bob]).await);
    }
    assert!(
        convs.iter().any(|c| holder(&nodes, c) == Some(2)),
        "the leaving node holds some"
    );
    let stop = Arc::new(AtomicBool::new(false));
    let h = Arc::new(History::default());
    let tasks = senders(
        &[nodes[0].0.clone(), nodes[1].0.clone()],
        &alice,
        &convs,
        stop.clone(),
        h.clone(),
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    let leaving = nodes[2].0.clone();
    let away = tokio::spawn(async move { leaving.going_away().await });
    let t0 = Instant::now();
    nodes[2].1.leave().await;
    // Its clients were told to reconnect elsewhere, and it is not ready.
    tokio::time::timeout(Duration::from_secs(1), away)
        .await
        .unwrap()
        .unwrap();
    assert!(nodes[2].1.ready().is_err());
    assert_eq!(nodes[2].0.health()["ready"], false);
    wait_for("the others hold everything", Duration::from_secs(3), || {
        nodes[0].1.held(0).len() + nodes[1].1.held(0).len() == (SHARDS + 2) as usize
    })
    .await;
    assert!(
        t0.elapsed() < Duration::from_secs(4),
        "no lease ran out: {:?}",
        t0.elapsed()
    );
    tokio::time::sleep(Duration::from_millis(500)).await;
    stop.store(true, Ordering::SeqCst);
    for t in tasks {
        let bad = t.await.unwrap();
        assert!(bad.is_empty(), "requests noticed the node leaving: {bad:?}");
    }
    single_holders(&[&nodes[0].1, &nodes[1].1]);
    h.check(&a).await;
}

async fn failing_renewals_stop_a_node_taking_clients(db: TestDb) {
    let first = start(&db.url, "n0").await;
    let (url, proxy) = Proxy::wrap(&db.url).await;
    let second = start_with(cluster_cfg(&url, "n1")).await.expect("starts");
    let nodes = vec![first, second];
    settled(&nodes).await;
    assert!(nodes[1].1.ready().is_ok());
    assert_eq!(nodes[1].0.health()["ready"], true);
    let hub = nodes[1].0.clone();
    let away = tokio::spawn(async move { hub.going_away().await });
    proxy.blackhole();
    // Five rounds in a row without a renewal: not ready, clients sent away.
    // Rounds take up to about 2 s here while the database does not answer.
    wait_for("not ready", Duration::from_secs(20), || {
        nodes[1].1.ready().is_err()
    })
    .await;
    assert_eq!(nodes[1].0.health()["ready"], false);
    assert!(
        nodes[1].0.health()["cluster"]
            .as_str()
            .unwrap()
            .contains("could not renew")
    );
    tokio::time::timeout(Duration::from_secs(1), away)
        .await
        .unwrap()
        .unwrap();
    // The other node is not affected.
    assert!(nodes[0].1.ready().is_ok());
    proxy.pass();
    wait_for("ready again", Duration::from_secs(20), || {
        nodes[1].1.ready().is_ok()
    })
    .await;
    wait_for("health ready again", Duration::from_secs(2), || {
        nodes[1].0.health()["ready"] == true
    })
    .await;
}

async fn held_and_delayed_notifications_arrive_in_order(db: TestDb) {
    let nodes = cluster(&db, 2).await;
    let alice = user(&nodes[0].0, "alice", UserKind::Human, None).await;
    let bob = user(&nodes[0].0, "bob", UserKind::Human, None).await;
    let a = Conn::new(&nodes[0].0, &alice);
    let conv = conv_held_by(&nodes, 0, &a, &[&bob]).await;
    let mut b = Conn::new(&nodes[1].0, &bob);
    let seqs_of = |b: &mut Conn| -> Vec<i64> {
        b.drain()
            .into_iter()
            .filter(|f| f["type"] == "event" && f["event"]["msg"]["msg_id"].is_string())
            .map(|f| f["event"]["seq"].as_i64().unwrap())
            .collect()
    };
    b.drain();
    nodes[0].1.hold_to("n1");
    for k in 0..10 {
        assert_eq!(a.send(&conv, &format!("held {k}")).await["ok"], true);
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(seqs_of(&mut b).is_empty(), "held back");
    nodes[0].1.release_to("n1");
    let mut got = vec![];
    let deadline = Instant::now() + Duration::from_secs(5);
    while got.len() < 10 && Instant::now() < deadline {
        got.extend(seqs_of(&mut b));
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let mut sorted = got.clone();
    sorted.sort();
    assert_eq!(got, sorted, "in order");
    assert_eq!(got.len(), 10);
    // Delayed: later, still in order and complete.
    nodes[0].1.delay_to("n1", Duration::from_millis(100));
    for k in 0..5 {
        assert_eq!(a.send(&conv, &format!("slow {k}")).await["ok"], true);
    }
    let mut got = vec![];
    let deadline = Instant::now() + Duration::from_secs(5);
    while got.len() < 5 && Instant::now() < deadline {
        got.extend(seqs_of(&mut b));
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let mut sorted = got.clone();
    sorted.sort();
    assert_eq!(got, sorted, "in order");
    assert_eq!(got.len(), 5);
    nodes[0].1.heal();
}

async fn outputs_of_a_newer_node_are_skipped_one_by_one(db: TestDb) {
    let nodes = cluster(&db, 2).await;
    let known = json!({"Dir": {"conv_id": "c_from_newer", "info": {"db": 0, "unit": "default", "migrating": false}}});
    let body =
        serde_json::to_vec(&json!({"db": 0, "outs": [{"SomethingNew": {"x": 1}}, known]})).unwrap();
    nodes[1].1.receive_publish(&body);
    assert!(
        nodes[1].0.shared().dir_get("c_from_newer").is_some(),
        "what this version knows still applies"
    );
    // Not even JSON: nothing applies, nothing breaks.
    nodes[1].1.receive_publish(b"not json");
    assert!(nodes[1].1.ready().is_ok());
}

async fn commands_this_version_does_not_know_are_unsupported(db: TestDb) {
    let nodes = cluster(&db, 2).await;
    let r = nodes[0]
        .1
        .receive_forward(0, 0, json!({"Teleport": {"to": "mars"}}))
        .await;
    assert_eq!(r.err.unwrap().code, "unsupported_op");
    let r = nodes[0].1.receive_forward(9, 0, json!("Barrier")).await;
    assert_eq!(r.err.unwrap().code, "bad_request", "unknown database");
}

async fn heads_from_any_node_show_where_conversations_stand(db: TestDb) {
    let nodes = cluster(&db, 2).await;
    let alice = user(&nodes[0].0, "alice", UserKind::Human, None).await;
    let bob = user(&nodes[0].0, "bob", UserKind::Human, None).await;
    let eve = user(&nodes[0].0, "eve", UserKind::Human, None).await;
    let a = Conn::new(&nodes[0].0, &alice);
    let conv = conv_held_by(&nodes, 0, &a, &[&bob]).await;
    for k in 0..3 {
        a.send(&conv, &format!("m{k}")).await;
    }
    let head = seqs(&a.pull(&conv, 0).await).last().copied().unwrap();
    // Asked on the node that does not hold it.
    let b = Conn::new(&nodes[1].0, &bob);
    let r = b
        .ok(json!({"op": "heads", "conv_ids": [conv, conv, "c_unknown"]}))
        .await;
    assert_eq!(r["heads"], json!({conv.clone(): head}));
    // Not a member: left out, nothing leaks.
    let e = Conn::new(&nodes[1].0, &eve);
    let r = e.ok(json!({"op": "heads", "conv_ids": [conv]})).await;
    assert_eq!(r["heads"], json!({}));
    let many: Vec<String> = (0..201).map(|i| format!("c_{i}")).collect();
    let r = b.call(json!({"op": "heads", "conv_ids": many})).await;
    assert_eq!(r["error"]["code"], "bad_request");
    let _: Value = r;
}

async fn mutation_without_the_cache_poll_a_cut_off_node_keeps_a_revoked_token(db: TestDb) {
    let nodes = cluster(&db, 2).await;
    let bob = user(&nodes[0].0, "bob", UserKind::Human, None).await;
    let (token, _) = nodes[0]
        .0
        .issue_token(&bob.id, Some("phone"), None, None, "test")
        .await
        .unwrap();
    for (h, _) in &nodes {
        let deadline = Instant::now() + Duration::from_secs(3);
        while h.authenticate_or_load(&token).await.is_none() {
            assert!(Instant::now() < deadline);
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    let ctl = nodes
        .iter()
        .position(|(_, n)| n.held(0).contains(&SHARDS))
        .unwrap();
    let other = 1 - ctl;
    nodes[other]
        .0
        .shared()
        .failpoints
        .set("mutation.no_cache_poll", Action::Error, None);
    nodes[0].1.cut("n1");
    nodes[1].1.cut("n0");
    nodes[ctl].0.revoke_tokens(&bob.id, None).await.unwrap();
    // `revocations_reach_cut_off_nodes_within_3s` would catch this.
    tokio::time::sleep(Duration::from_secs(4)).await;
    assert!(
        nodes[other].0.authenticate_device(&token).is_some(),
        "without the poll the revocation never arrives"
    );
    nodes[0].1.heal();
    nodes[1].1.heal();
}
