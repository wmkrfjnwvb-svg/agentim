//! Fault tests for the cluster (docs/11 §12.5): paused writers, lost
//! answers, crashes under load, a node cut off from the database (blackhole
//! and reset), nodes cut off from each other, lost notifications, clock
//! jumps, restarts of every node; each ends with the total check. The
//! mutation tests switch one protection off and show the violation the
//! tests above would catch. PostgreSQL and MySQL.

#[path = "../../agentim-core/tests/common/mod.rs"]
mod common;

#[macro_use]
mod support;

use std::sync::Arc;
use std::time::{Duration, Instant};

use agentim_core::failpoint::{AFTER_COMMIT, Action, BEFORE_FENCE};
use agentim_core::model::*;
use common::*;
use serde_json::json;
use support::*;

cluster_tests!(
    a_batch_paused_before_its_check_leaves_no_trace,
    an_answer_lost_after_commit_is_found_by_the_retry,
    a_crash_under_concurrent_traffic_loses_nothing,
    a_holder_in_a_database_blackhole_stops_writing,
    a_holder_whose_database_connections_reset_stops_writing,
    nodes_cut_off_from_each_other_keep_one_holder,
    frames_lost_one_way_give_a_retryable_answer,
    revocations_reach_cut_off_nodes_within_3s,
    a_clock_jump_never_extends_holding,
    every_node_restarts,
    mutation_without_the_check_a_stale_holder_writes,
    mutation_without_the_progress_gate_a_stuck_writer_keeps_its_shards,
);

async fn people(hub: &agentim_core::hub::Hub) -> (User, User) {
    (
        user(hub, "alice", UserKind::Human, None).await,
        user(hub, "bob", UserKind::Human, None).await,
    )
}

async fn epoch(url: &str, conv: &str) -> i64 {
    use sqlx::{Connection, Row};
    let mut c = sqlx::AnyConnection::connect(url).await.unwrap();
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT epoch FROM shard_lease WHERE db = 'main' AND shard_id = {}",
        shard(conv)
    )))
    .fetch_one(&mut c)
    .await
    .unwrap()
    .try_get::<i64, _>(0)
    .unwrap()
}

async fn a_batch_paused_before_its_check_leaves_no_trace(db: TestDb) {
    let nodes = cluster(&db, 2).await;
    let (alice, bob) = people(&nodes[0].0).await;
    let a0 = Conn::new(&nodes[0].0, &alice);
    let conv = conv_held_by(&nodes, 1, &a0, &[&bob]).await;
    let before = epoch(&db.url, &conv).await;
    let h = History::default();
    let fp = &nodes[1].0.shared().failpoints;
    fp.set(BEFORE_FENCE, Action::Pause, Some(1));
    // The batch stalls before its lease check; past the stuck limit it is
    // abandoned with an unknown outcome.
    let a1 = Conn::new(&nodes[1].0, &alice);
    let o = h.send(&a1, &conv, "paused", Duration::from_secs(10)).await;
    assert!(matches!(o, Outcome::Unknown), "{o:?}");
    fp.release(BEFORE_FENCE);
    assert!(
        !a0.pull(&conv, 0).await.to_string().contains("\"paused\""),
        "no trace"
    );
    // The client retries; one message results, and the shard was taken
    // again under a new epoch.
    retry_until_ok(&h, &a0, &conv, "paused", Duration::from_secs(10)).await;
    for k in 0..5 {
        retry_until_ok(
            &h,
            &a1,
            &conv,
            &format!("after {k}"),
            Duration::from_secs(10),
        )
        .await;
    }
    assert!(
        epoch(&db.url, &conv).await > before,
        "taken again with a new epoch"
    );
    h.check(&a0).await;
}

async fn an_answer_lost_after_commit_is_found_by_the_retry(db: TestDb) {
    let nodes = cluster(&db, 2).await;
    let (alice, bob) = people(&nodes[0].0).await;
    let a0 = Conn::new(&nodes[0].0, &alice);
    let conv = conv_held_by(&nodes, 1, &a0, &[&bob]).await;
    let fp = &nodes[1].0.shared().failpoints;
    fp.set(AFTER_COMMIT, Action::Pause, Some(1));
    // Committed, but the answer does not come: the writer is stuck.
    let a1 = Arc::new(Conn::new(&nodes[1].0, &alice));
    let first = {
        let (a1, conv) = (a1.clone(), conv.clone());
        tokio::spawn(async move {
            a1.call(json!({"op": "send", "conv_id": conv, "client_msg_id": "lost-answer", "text": "lost-answer"}))
                .await
        })
    };
    fp.wait_hits(AFTER_COMMIT, 1).await;
    // A stuck writer stops renewing; the other node takes the shard while
    // this node stays a live member (its heartbeat goes on).
    wait_for(
        "shard moves away from the stuck writer",
        Duration::from_secs(10),
        || nodes[0].1.held(0).contains(&shard(&conv)),
    )
    .await;
    assert!(!nodes[1].1.held(0).contains(&shard(&conv)));
    let h = History::default();
    let id = retry_until_ok(&h, &a0, &conv, "lost-answer", Duration::from_secs(10)).await;
    fp.release(AFTER_COMMIT);
    let r = first.await.unwrap();
    assert_eq!(
        r["msg_id"],
        id.as_str(),
        "the retry found the committed message: {r}"
    );
    h.check(&a0).await;
}

async fn a_crash_under_concurrent_traffic_loses_nothing(db: TestDb) {
    let nodes = cluster(&db, 3).await;
    let (alice, bob) = people(&nodes[0].0).await;
    let a0 = Conn::new(&nodes[0].0, &alice);
    let ctl = nodes
        .iter()
        .position(|(_, n)| n.held(0).contains(&SHARDS))
        .unwrap();
    let victim = (ctl + 1) % 3;
    let conv = conv_held_by(&nodes, victim, &a0, &[&bob]).await;
    let other = a0.group(&[&bob]).await;
    let h = Arc::new(History::default());
    let survivors: Vec<usize> = (0..3).filter(|i| *i != victim).collect();
    let mut senders = vec![];
    for (k, i) in survivors.iter().enumerate() {
        let (hub, alice, h) = (nodes[*i].0.clone(), alice.clone(), h.clone());
        let convs = [conv.clone(), other.clone()];
        senders.push(tokio::spawn(async move {
            let c = Conn::new(&hub, &alice);
            for n in 0..40 {
                h.send(
                    &c,
                    &convs[n % 2],
                    &format!("s{k}-{n}"),
                    Duration::from_secs(3),
                )
                .await;
            }
        }));
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    nodes[victim].1.crash();
    for s in senders {
        s.await.unwrap();
    }
    assert!(h.accepted() > 40, "most sends accepted: {}", h.accepted());
    let check = Conn::new(&nodes[survivors[0]].0, &alice);
    h.check(&check).await;
    let alive: Vec<&agentim_cluster::Node> = survivors.iter().map(|i| &nodes[*i].1).collect();
    single_holders(&alive);
}

/// A node whose database traffic goes through a proxy that then fails.
async fn cut_off_from_the_database(db: TestDb, blackhole: bool) {
    let first = start(&db.url, "n0").await;
    let (url, proxy) = Proxy::wrap(&db.url).await;
    let second = start_with(cluster_cfg(&url, "n1"))
        .await
        .expect("starts through the proxy");
    let nodes = vec![first, second];
    settled(&nodes).await;
    let (alice, bob) = people(&nodes[0].0).await;
    let a0 = Conn::new(&nodes[0].0, &alice);
    let conv = conv_held_by(&nodes, 1, &a0, &[&bob]).await;
    let h = History::default();
    retry_until_ok(&h, &a0, &conv, "before", Duration::from_secs(5)).await;
    let t0 = Instant::now();
    if blackhole {
        proxy.blackhole();
    } else {
        proxy.reset();
    }
    // It stops writing within the stop time and the other node takes over.
    wait_for(
        "the cut-off node stops writing",
        Duration::from_secs(4),
        || nodes[1].1.held(0).is_empty(),
    )
    .await;
    assert!(
        t0.elapsed() <= Duration::from_millis(2_600),
        "{:?}",
        t0.elapsed()
    );
    for k in 0..5 {
        retry_until_ok(
            &h,
            &a0,
            &conv,
            &format!("during {k}"),
            Duration::from_secs(15),
        )
        .await;
    }
    assert!(nodes[0].1.held(0).contains(&shard(&conv)));
    proxy.pass();
    // Back: it takes part again, and nothing was written twice.
    wait_for(
        "the node holds shards again",
        Duration::from_secs(20),
        || !nodes[1].1.held(0).is_empty(),
    )
    .await;
    let a1 = Conn::new(&nodes[1].0, &alice);
    for k in 0..5 {
        retry_until_ok(
            &h,
            &a1,
            &conv,
            &format!("after {k}"),
            Duration::from_secs(15),
        )
        .await;
    }
    single_holders(&[&nodes[0].1, &nodes[1].1]);
    h.check(&a0).await;
}

async fn a_holder_in_a_database_blackhole_stops_writing(db: TestDb) {
    cut_off_from_the_database(db, true).await;
}

async fn a_holder_whose_database_connections_reset_stops_writing(db: TestDb) {
    cut_off_from_the_database(db, false).await;
}

async fn nodes_cut_off_from_each_other_keep_one_holder(db: TestDb) {
    let nodes = cluster(&db, 2).await;
    let (alice, bob) = people(&nodes[0].0).await;
    let a0 = Conn::new(&nodes[0].0, &alice);
    let a1 = Conn::new(&nodes[1].0, &alice);
    let conv = conv_held_by(&nodes, 1, &a0, &[&bob]).await;
    nodes[0].1.cut("n1");
    nodes[1].1.cut("n0");
    let h = History::default();
    // Forwarding fails with a retryable answer, never a second holder.
    let o = h.send(&a0, &conv, "across", Duration::from_secs(10)).await;
    assert!(matches!(o, Outcome::Unknown), "{o:?}");
    for k in 0..3 {
        retry_until_ok(
            &h,
            &a1,
            &conv,
            &format!("local {k}"),
            Duration::from_secs(5),
        )
        .await;
    }
    tokio::time::sleep(Duration::from_secs(4)).await;
    assert!(
        nodes[1].1.held(0).contains(&shard(&conv)),
        "the holder keeps its lease"
    );
    single_holders(&[&nodes[0].1, &nodes[1].1]);
    nodes[0].1.heal();
    nodes[1].1.heal();
    retry_until_ok(&h, &a0, &conv, "across", Duration::from_secs(15)).await;
    h.check(&a0).await;
}

async fn frames_lost_one_way_give_a_retryable_answer(db: TestDb) {
    let nodes = cluster(&db, 2).await;
    let (alice, bob) = people(&nodes[0].0).await;
    let a0 = Conn::new(&nodes[0].0, &alice);
    let a1 = Conn::new(&nodes[1].0, &alice);
    let on1 = conv_held_by(&nodes, 1, &a0, &[&bob]).await;
    let on0 = conv_held_by(&nodes, 0, &a0, &[&bob]).await;
    nodes[0].1.cut_to("n1");
    let h = History::default();
    let t0 = Instant::now();
    let o = h.send(&a0, &on1, "one-way", Duration::from_secs(10)).await;
    assert!(matches!(o, Outcome::Unknown), "{o:?}");
    assert!(
        t0.elapsed() < Duration::from_secs(7),
        "answered in time: {:?}",
        t0.elapsed()
    );
    // The other direction still works.
    retry_until_ok(&h, &a1, &on0, "other way", Duration::from_secs(5)).await;
    nodes[0].1.heal();
    retry_until_ok(&h, &a0, &on1, "one-way", Duration::from_secs(15)).await;
    h.check(&a0).await;
}

async fn revocations_reach_cut_off_nodes_within_3s(db: TestDb) {
    let nodes = cluster(&db, 3).await;
    let (_, bob) = people(&nodes[0].0).await;
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
    // Nobody hears anybody: the notification is lost, the poll remains.
    for (i, (_, n)) in nodes.iter().enumerate() {
        for j in 0..3 {
            if i != j {
                n.cut(&format!("n{j}"));
            }
        }
    }
    let ctl = nodes
        .iter()
        .position(|(_, n)| n.held(0).contains(&SHARDS))
        .unwrap();
    nodes[ctl].0.revoke_tokens(&bob.id, None).await.unwrap();
    let t0 = Instant::now();
    for (h, _) in &nodes {
        let (h, t) = (h.clone(), token.clone());
        wait_for("revoked everywhere", Duration::from_secs(4), || {
            h.authenticate_device(&t).is_none()
        })
        .await;
    }
    assert!(
        t0.elapsed() <= Duration::from_millis(3_200),
        "{:?}",
        t0.elapsed()
    );
}

async fn a_clock_jump_never_extends_holding(db: TestDb) {
    let nodes = cluster(&db, 2).await;
    let (alice, bob) = people(&nodes[0].0).await;
    let a0 = Conn::new(&nodes[0].0, &alice);
    let conv = conv_held_by(&nodes, 1, &a0, &[&bob]).await;
    nodes[1].1.jump_clock(Duration::from_secs(3600));
    // Its deadlines are behind it at once.
    assert!(
        nodes[1].1.held(0).is_empty(),
        "a forward jump stops writing at once"
    );
    let h = History::default();
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut k = 0;
    while Instant::now() < deadline {
        single_holders(&[&nodes[0].1, &nodes[1].1]);
        retry_until_ok(&h, &a0, &conv, &format!("j{k}"), Duration::from_secs(10)).await;
        k += 1;
    }
    h.check(&a0).await;
}

async fn every_node_restarts(db: TestDb) {
    let nodes = cluster(&db, 2).await;
    let (alice, bob) = people(&nodes[0].0).await;
    let conv = Conn::new(&nodes[0].0, &alice).group(&[&bob]).await;
    let h = History::default();
    for k in 0..5 {
        retry_until_ok(
            &h,
            &Conn::new(&nodes[k % 2].0, &alice),
            &conv,
            &format!("a{k}"),
            Duration::from_secs(5),
        )
        .await;
    }
    for (_, n) in &nodes {
        n.crash();
    }
    drop(nodes);
    let again = vec![start(&db.url, "n0").await, start(&db.url, "n1").await];
    settled(&again).await;
    for k in 0..5 {
        retry_until_ok(
            &h,
            &Conn::new(&again[k % 2].0, &alice),
            &conv,
            &format!("b{k}"),
            Duration::from_secs(15),
        )
        .await;
    }
    h.check(&Conn::new(&again[0].0, &alice)).await;
}

async fn mutation_without_the_check_a_stale_holder_writes(db: TestDb) {
    let nodes = cluster(&db, 2).await;
    let (alice, bob) = people(&nodes[0].0).await;
    let a0 = Conn::new(&nodes[0].0, &alice);
    let conv = conv_held_by(&nodes, 1, &a0, &[&bob]).await;
    nodes[1]
        .0
        .shared()
        .failpoints
        .set("mutation.no_fence", Action::Error, None);
    // Another holder took the shard (the epoch moved): with the check off
    // the stale batch commits, which `a_stale_holder_commits_nothing` and
    // the tests above would report.
    use sqlx::Connection;
    let mut c = sqlx::AnyConnection::connect(&db.url).await.unwrap();
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "UPDATE shard_lease SET epoch = epoch + 1 WHERE db = 'main' AND shard_id = {}",
        shard(&conv)
    )))
    .execute(&mut c)
    .await
    .unwrap();
    let r = Conn::new(&nodes[1].0, &alice).send(&conv, "stale").await;
    assert_eq!(
        r["ok"], true,
        "without the check the stale write lands: {r}"
    );
}

async fn mutation_without_the_progress_gate_a_stuck_writer_keeps_its_shards(db: TestDb) {
    let nodes = cluster(&db, 2).await;
    let (alice, bob) = people(&nodes[0].0).await;
    let a0 = Conn::new(&nodes[0].0, &alice);
    let conv = conv_held_by(&nodes, 1, &a0, &[&bob]).await;
    let fp = &nodes[1].0.shared().failpoints;
    fp.set("mutation.no_progress_gate", Action::Error, None);
    fp.set(AFTER_COMMIT, Action::Pause, Some(1));
    let a1 = Arc::new(Conn::new(&nodes[1].0, &alice));
    let stuck = {
        let (a1, conv) = (a1.clone(), conv.clone());
        tokio::spawn(async move { a1.send(&conv, "stuck").await })
    };
    fp.wait_hits(AFTER_COMMIT, 1).await;
    // Leases are renewed although nothing is written: the shard never
    // moves, which `an_answer_lost_after_commit_is_found_by_the_retry`
    // would report.
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert!(
        nodes[1].1.held(0).contains(&shard(&conv)),
        "kept while stuck"
    );
    fp.release(AFTER_COMMIT);
    fp.release("mutation.no_progress_gate");
    assert_eq!(stuck.await.unwrap()["ok"], true);
}
