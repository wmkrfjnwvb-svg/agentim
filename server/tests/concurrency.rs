//! The business scenarios of tests/scenarios.rs under contention: the same
//! operation from several sessions at once, conflicting operations racing
//! each other, and writes while membership or control state changes. Each
//! test repeats its race many times and then checks the invariants: exactly
//! one winner, a linear history, no state change after a terminal one.
//! Run against every backend.

mod common;

use std::sync::Arc;

use agentim::hub::Hub;
use agentim::model::*;
use common::*;
use serde_json::{Value, json};

backend_tests!(
    a_retry_from_many_devices_creates_one_message,
    concurrent_edits_keep_a_linear_version_history,
    recall_racing_edits_leaves_no_content_and_nothing_after_it,
    one_recall_wins,
    approve_and_reject_race_has_one_outcome,
    adding_the_same_member_concurrently_adds_them_once,
    members_added_during_traffic_get_every_later_event,
    stream_end_racing_cancel_finishes_once,
    duplicate_chunks_from_two_connections_are_applied_once,
    takeover_racing_agent_messages_is_consistent,
    concurrent_read_cursors_keep_the_maximum,
    many_conversations_across_databases_stay_independent,
);

const ROUNDS: usize = 20;

/// The agent rate limit would cut the races short; it has its own tests.
async fn open_unlimited(cfg: agentim::config::Config) -> Hub {
    let mut h = hub_cfg();
    h.guards.rate_capacity = 1e9;
    h.guards.rate_per_sec = 1e9;
    Hub::open(cfg, h).await.expect("open hub")
}

fn code(r: &Value) -> &str {
    r["error"]["code"].as_str().unwrap_or("")
}

fn log_events(p: &Value) -> Vec<Value> {
    p["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["event"].clone())
        .collect()
}

async fn full_log(c: &Conn, conv: &str) -> Vec<Value> {
    let mut out = vec![];
    let mut after = 0;
    loop {
        let p = c.pull(conv, after).await;
        out.extend(log_events(&p));
        after = p["upto_seq"].as_i64().unwrap();
        if p["has_more"] != true {
            return out;
        }
    }
}

fn assert_gap_free(log: &[Value]) {
    let seqs: Vec<i64> = log.iter().map(|e| e["seq"].as_i64().unwrap()).collect();
    assert_eq!(
        seqs,
        (1..=seqs.len() as i64).collect::<Vec<_>>(),
        "log is gap-free"
    );
}

fn conns(hub: &Hub, u: &User, n: usize) -> Vec<Arc<Conn>> {
    (0..n).map(|_| Arc::new(Conn::new(hub, u))).collect()
}

/// Runs the requests at the same time, one task each; results in order.
async fn race(reqs: Vec<(Arc<Conn>, Value)>) -> Vec<Value> {
    let tasks: Vec<_> = reqs
        .into_iter()
        .map(|(c, v)| tokio::spawn(async move { c.call(v).await }))
        .collect();
    let mut out = vec![];
    for t in tasks {
        out.push(t.await.unwrap());
    }
    out
}

fn oks(rs: &[Value]) -> usize {
    rs.iter().filter(|r| r["ok"] == true).count()
}

// ------------------------------------------------------------------ tests

async fn a_retry_from_many_devices_creates_one_message(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let devices = conns(&hub, &alice, 8);
    let conv = devices[0].group(&[&bob]).await;
    for round in 0..ROUNDS {
        let req = json!({"op": "send", "conv_id": conv, "client_msg_id": format!("same-{round}"), "text": format!("once {round}")});
        let rs = race(devices.iter().map(|d| (d.clone(), req.clone())).collect()).await;
        assert_eq!(oks(&rs), devices.len(), "{rs:?}");
        let ids: std::collections::HashSet<&str> =
            rs.iter().map(|r| r["msg_id"].as_str().unwrap()).collect();
        assert_eq!(ids.len(), 1, "every device gets the same message back");
        assert_eq!(
            rs.iter().filter(|r| r["duplicate"] != true).count(),
            1,
            "exactly one created it"
        );
    }
    let log = full_log(&devices[0], &conv).await;
    assert_gap_free(&log);
    assert_eq!(
        log.iter().filter(|e| e["type"] == "msg.created").count(),
        ROUNDS
    );
}

async fn concurrent_edits_keep_a_linear_version_history(db: TestDb) {
    let hub = open_unlimited(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    let owner = conns(&hub, &alice, 3);
    let agent = conns(&hub, &bot, 3);
    let conv = owner[0].group(&[&bot]).await;
    let q = owner[0].send(&conv, "@bot hi").await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    let m = agent[0].reply(&conv, "v0", &q).await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    let mut ok = 0;
    for round in 0..ROUNDS {
        // The agent and its owner edit the same message at the same time.
        let mut reqs = vec![];
        for (i, c) in owner.iter().chain(agent.iter()).enumerate() {
            reqs.push((
                c.clone(),
                json!({"op": "edit", "msg_id": m, "text": format!("r{round}-c{i}")}),
            ));
        }
        ok += oks(&race(reqs).await);
    }
    let log = full_log(&owner[0], &conv).await;
    assert_gap_free(&log);
    let versions: Vec<i64> = log
        .iter()
        .filter(|e| e["msg"]["msg_id"] == m.as_str())
        .map(|e| e["msg"]["version"].as_i64().unwrap())
        .collect();
    assert_eq!(
        versions,
        (1..=versions.len() as i64).collect::<Vec<_>>(),
        "versions go up by one"
    );
    assert_eq!(versions.len(), ok + 1, "one event per accepted edit");
    let last = log
        .iter()
        .rev()
        .find(|e| e["msg"]["msg_id"] == m.as_str())
        .unwrap();
    let current = msg_by_id(&owner[0].pull(&conv, 0).await, &m).unwrap();
    assert_eq!(
        current["content"], last["msg"]["content"],
        "the latest event is the current state"
    );
}

async fn recall_racing_edits_leaves_no_content_and_nothing_after_it(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let senders = conns(&hub, &bob, 4);
    let a = Arc::new(Conn::new(&hub, &alice));
    let conv = a.group(&[&bob]).await;
    for round in 0..ROUNDS {
        let m = senders[0]
            .send(&conv, &format!("secret-{round}-orig"))
            .await["msg_id"]
            .as_str()
            .unwrap()
            .to_string();
        let mut reqs: Vec<(Arc<Conn>, Value)> = senders
            .iter()
            .enumerate()
            .map(|(i, c)| {
                (
                    c.clone(),
                    json!({"op": "edit", "msg_id": m, "text": format!("secret-{round}-edit{i}")}),
                )
            })
            .collect();
        reqs.insert(2, (a.clone(), json!({"op": "recall", "msg_id": m})));
        let rs = race(reqs).await;
        assert_eq!(
            rs[2]["ok"], true,
            "the owner's recall always succeeds: {}",
            rs[2]
        );
        for r in &rs {
            assert!(r["ok"] == true || code(r) == "conflict", "{r}");
        }
        let log = full_log(&a, &conv).await;
        let mine: Vec<&Value> = log
            .iter()
            .filter(|e| e["msg"]["msg_id"] == m.as_str())
            .collect();
        assert_eq!(
            mine.last().unwrap()["type"],
            "msg.recalled",
            "nothing happens to it after the recall"
        );
        assert_eq!(
            mine.iter().filter(|e| e["type"] == "msg.recalled").count(),
            1
        );
    }
    let log = full_log(&a, &conv).await;
    assert_gap_free(&log);
    let text = serde_json::to_string(&log).unwrap();
    assert!(
        !text.contains("secret-"),
        "no recalled or edited content survives"
    );
}

async fn one_recall_wins(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let owner = conns(&hub, &alice, 3);
    let sender = conns(&hub, &bob, 3);
    let conv = owner[0].group(&[&bob]).await;
    for round in 0..ROUNDS {
        let m = sender[0].send(&conv, &format!("m{round}")).await["msg_id"]
            .as_str()
            .unwrap()
            .to_string();
        let req = json!({"op": "recall", "msg_id": m});
        let rs = race(
            owner
                .iter()
                .chain(sender.iter())
                .map(|c| (c.clone(), req.clone()))
                .collect(),
        )
        .await;
        assert_eq!(oks(&rs), 1, "{rs:?}");
        assert!(rs.iter().all(|r| r["ok"] == true || code(r) == "conflict"));
    }
    let log = full_log(&owner[0], &conv).await;
    assert_gap_free(&log);
    assert_eq!(
        log.iter().filter(|e| e["type"] == "msg.recalled").count(),
        ROUNDS
    );
}

async fn approve_and_reject_race_has_one_outcome(db: TestDb) {
    let hub = open_unlimited(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    let owner = conns(&hub, &alice, 4);
    let g = Conn::new(&hub, &bot);
    let b = Conn::new(&hub, &bob);
    let conv = owner[0].group(&[&bob, &bot]).await;
    owner[0]
        .ok(json!({"op": "set_agent_policy", "conv_id": conv, "agent_id": bot.id, "approval": true}))
        .await;
    let q = b.send(&conv, "@bot help").await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    let mut approved = vec![];
    for round in 0..ROUNDS {
        let text = format!("answer-{round}");
        let d = g.reply(&conv, &text, &q).await["draft_id"]
            .as_str()
            .unwrap()
            .to_string();
        // Two approvals and two rejections from the owner's devices at once.
        let reqs = owner
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let op = if i % 2 == 0 { "approve" } else { "reject" };
                (c.clone(), json!({"op": op, "draft_id": d}))
            })
            .collect();
        let rs = race(reqs).await;
        assert_eq!(oks(&rs), 1, "exactly one decision: {rs:?}");
        assert!(
            rs.iter().all(|r| r["ok"] == true || code(r) == "conflict"),
            "{rs:?}"
        );
        let winner = rs.iter().position(|r| r["ok"] == true).unwrap();
        if winner % 2 == 0 {
            approved.push(text);
        }
    }
    let log = full_log(&b, &conv).await;
    assert_gap_free(&log);
    let published: Vec<String> = log
        .iter()
        .filter(|e| e["type"] == "msg.created" && e["msg"]["sender_id"] == bot.id.as_str())
        .map(|e| e["msg"]["content"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        published, approved,
        "each approved draft published once, rejected ones never"
    );
    assert_eq!(
        owner[0].ok(json!({"op": "list_drafts"})).await["drafts"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
}

async fn adding_the_same_member_concurrently_adds_them_once(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let owner = conns(&hub, &alice, 5);
    let conv = owner[0].group(&[]).await;
    let mut newcomers = vec![];
    for round in 0..ROUNDS {
        let u = user(&hub, &format!("u{round}"), UserKind::Human, None).await;
        let req = json!({"op": "add_member", "conv_id": conv, "user_id": u.id});
        let rs = race(owner.iter().map(|c| (c.clone(), req.clone())).collect()).await;
        assert_eq!(oks(&rs), 1, "{rs:?}");
        assert!(rs.iter().all(|r| r["ok"] == true || code(r) == "conflict"));
        newcomers.push(u);
    }
    // Different users added at once all get in.
    let more: Vec<User> = {
        let mut v = vec![];
        for i in 0..5 {
            v.push(user(&hub, &format!("w{i}"), UserKind::Human, None).await);
        }
        v
    };
    let rs = race(
        owner
            .iter()
            .zip(&more)
            .map(|(c, u)| {
                (
                    c.clone(),
                    json!({"op": "add_member", "conv_id": conv, "user_id": u.id}),
                )
            })
            .collect(),
    )
    .await;
    assert_eq!(oks(&rs), more.len(), "{rs:?}");
    let log = full_log(&owner[0], &conv).await;
    assert_gap_free(&log);
    assert_eq!(
        log.iter().filter(|e| e["type"] == "member.joined").count(),
        1 + ROUNDS + more.len(),
        "creator plus one join per user"
    );
    let info = owner[0].ok(json!({"op": "list_convs"})).await;
    assert_eq!(
        info["convs"][0]["members"].as_array().unwrap().len(),
        1 + ROUNDS + more.len()
    );
}

async fn members_added_during_traffic_get_every_later_event(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let senders: Vec<User> = {
        let mut v = vec![];
        for i in 0..4 {
            v.push(user(&hub, &format!("s{i}"), UserKind::Human, None).await);
        }
        v
    };
    let a = Arc::new(Conn::new(&hub, &alice));
    let refs: Vec<&User> = senders.iter().collect();
    let conv = a.group(&refs).await;
    let mut tasks = vec![];
    for s in &senders {
        let c = Arc::new(Conn::new(&hub, s));
        let conv = conv.clone();
        tasks.push(tokio::spawn(async move {
            for i in 0..50 {
                assert_eq!(c.send(&conv, &format!("t{i}")).await["ok"], true);
            }
        }));
    }
    // New members connect first, then are added while the senders write.
    let mut joiners = vec![];
    for i in 0..6 {
        let u = user(&hub, &format!("late{i}"), UserKind::Human, None).await;
        let conn = Conn::new(&hub, &u);
        let r = a
            .ok(json!({"op": "add_member", "conv_id": conv, "user_id": u.id}))
            .await;
        joiners.push((conn, r["seq"].as_i64().unwrap()));
        tokio::task::yield_now().await;
    }
    for t in tasks {
        t.await.unwrap();
    }
    let log = full_log(&a, &conv).await;
    assert_gap_free(&log);
    let head = log.len() as i64;
    for (mut c, join) in joiners {
        let pushed: Vec<i64> = c
            .drain()
            .iter()
            .filter(|f| f["type"] == "event")
            .map(|f| f["event"]["seq"].as_i64().unwrap())
            .collect();
        let want: Vec<i64> = (join..=head).collect();
        assert_eq!(
            pushed, want,
            "a member gets every event from its join on, in order"
        );
    }
}

async fn stream_end_racing_cancel_finishes_once(db: TestDb) {
    let hub = open_unlimited(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    let a = Arc::new(Conn::new(&hub, &alice));
    let b = Arc::new(Conn::new(&hub, &bob));
    let g = Arc::new(Conn::new(&hub, &bot));
    let conv = a.group(&[&bob, &bot]).await;
    let q = b.send(&conv, "@bot go").await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    for round in 0..ROUNDS {
        let s = g
            .ok(json!({"op": "stream_start", "conv_id": conv, "client_msg_id": format!("s{round}"), "reply_to": q}))
            .await["msg_id"]
            .as_str()
            .unwrap()
            .to_string();
        g.ok(json!({"op": "stream_append", "msg_id": s, "idx": 0, "text": "part"}))
            .await;
        let rs = race(vec![
            (g.clone(), json!({"op": "stream_end", "msg_id": s})),
            (b.clone(), json!({"op": "stream_cancel", "msg_id": s})),
            (a.clone(), json!({"op": "stream_cancel", "msg_id": s})),
            (
                g.clone(),
                json!({"op": "stream_append", "msg_id": s, "idx": 1, "text": "late"}),
            ),
        ])
        .await;
        assert_eq!(oks(&rs[..3]), 1, "one terminal transition: {rs:?}");
        let log = full_log(&a, &conv).await;
        let ends: Vec<&Value> = log
            .iter()
            .filter(|e| e["msg"]["msg_id"] == s.as_str() && e["type"] == "msg.updated")
            .collect();
        assert_eq!(ends.len(), 1, "finished exactly once");
        let st = ends[0]["msg"]["state"].as_str().unwrap();
        assert!(st == "complete" || st == "cancelled", "{st}");
        let content = ends[0]["msg"]["content"].as_str().unwrap();
        assert!(content == "part" || content == "partlate", "{content}");
    }
    assert_gap_free(&full_log(&a, &conv).await);
}

async fn duplicate_chunks_from_two_connections_are_applied_once(db: TestDb) {
    let hub = open_unlimited(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    let a = Conn::new(&hub, &alice);
    let g = conns(&hub, &bot, 2);
    let conv = a.group(&[&bot]).await;
    let q = a.send(&conv, "@bot count").await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    let s = g[0]
        .ok(json!({"op": "stream_start", "conv_id": conv, "client_msg_id": "s", "reply_to": q}))
        .await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    let mut expected = String::new();
    for i in 0..60u64 {
        let part = format!("[{i}]");
        expected.push_str(&part);
        // Both connections send the same chunk (a retry racing the original).
        let req = json!({"op": "stream_append", "msg_id": s, "idx": i, "text": part});
        let rs = race(vec![(g[0].clone(), req.clone()), (g[1].clone(), req)]).await;
        assert_eq!(oks(&rs), 2, "{rs:?}");
        assert_eq!(rs.iter().filter(|r| r["duplicate"] == true).count(), 1);
    }
    g[1].ok(json!({"op": "stream_end", "msg_id": s})).await;
    let m = msg_by_id(&a.pull(&conv, 0).await, &s).unwrap();
    assert_eq!(m["content"].as_str().unwrap(), expected);
}

async fn takeover_racing_agent_messages_is_consistent(db: TestDb) {
    let hub = open_unlimited(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    let owner = conns(&hub, &alice, 2);
    let g = Arc::new(Conn::new(&hub, &bot));
    let b = Conn::new(&hub, &bob);
    let conv = owner[0].group(&[&bob, &bot]).await;
    let mut prompts = vec![];
    for i in 0..ROUNDS {
        prompts.push(
            b.send(&conv, &format!("@bot q{i}")).await["msg_id"]
                .as_str()
                .unwrap()
                .to_string(),
        );
    }
    for (round, q) in prompts.iter().enumerate() {
        let take = json!({"op": "takeover", "conv_id": conv, "agent_id": bot.id});
        let release = json!({"op": "release", "conv_id": conv, "agent_id": bot.id});
        let rs = race(vec![
            (owner[0].clone(), take.clone()),
            (g.clone(), json!({"op": "send", "conv_id": conv, "client_msg_id": format!("a{round}"), "text": format!("agent says {round}"), "reply_to": q})),
            (owner[1].clone(), take),
            (owner[0].clone(), release),
        ])
        .await;
        // take, release, take may all succeed in that order; the log replay
        // below checks that they alternate.
        for r in [&rs[0], &rs[2], &rs[3]] {
            assert!(r["ok"] == true || code(r) == "conflict", "{r}");
        }
        assert!(
            rs[1]["ok"] == true || code(&rs[1]) == "taken_over",
            "{}",
            rs[1]
        );
        // Leave the agent released for the next round.
        let _ = owner[0]
            .call(json!({"op": "release", "conv_id": conv, "agent_id": bot.id}))
            .await;
    }
    // Replay the log: an agent message never lands while it is taken over,
    // and takeovers and releases alternate.
    let log = full_log(&b, &conv).await;
    assert_gap_free(&log);
    let mut taken = false;
    for e in &log {
        if e["type"] == "msg.created" && e["msg"]["sender_id"] == bot.id.as_str() {
            assert!(!taken, "agent message at seq {} while taken over", e["seq"]);
        }
        if e["type"] == "agent.state" {
            let now = e["data"]["state"] == "taken_over";
            assert_ne!(
                now, taken,
                "takeover and release alternate (seq {})",
                e["seq"]
            );
            taken = now;
        }
    }
}

async fn concurrent_read_cursors_keep_the_maximum(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let devices = conns(&hub, &alice, 6);
    let b = Conn::new(&hub, &bob);
    let conv = devices[0].group(&[&bob]).await;
    for i in 0..40 {
        b.send(&conv, &format!("{i}")).await;
    }
    let head = full_log(&b, &conv).await.len() as i64;
    let mut reqs = vec![];
    for (i, d) in devices.iter().enumerate() {
        for k in 0..10 {
            let seq = ((i * 7 + k * 3) as i64) % head + 1;
            reqs.push((
                d.clone(),
                json!({"op": "read", "conv_id": conv, "seq": seq}),
            ));
        }
    }
    let top = reqs
        .iter()
        .map(|(_, v)| v["seq"].as_i64().unwrap())
        .max()
        .unwrap();
    assert_eq!(oks(&race(reqs).await), 60);
    let info = devices[0].ok(json!({"op": "list_convs"})).await;
    let unread = info["convs"][0]["unread"].as_i64().unwrap();
    // Messages after the highest cursor any device reported stay unread.
    let log = full_log(&b, &conv).await;
    let after_top = log
        .iter()
        .filter(|e| e["type"] == "msg.created" && e["seq"].as_i64().unwrap() > top)
        .count() as i64;
    assert_eq!(unread, after_top);
}

async fn many_conversations_across_databases_stay_independent(db: TestDb) {
    let secret = new_db(Backend::Sqlite).await.unwrap();
    let hub = open(two(&db.url, &secret.url, "secret")).await;
    let mut tasks = vec![];
    for i in 0..8 {
        let unit = if i % 2 == 0 { "default" } else { "b" };
        let a = hub
            .create_user(&format!("a{i}"), UserKind::Human, None, Some(unit))
            .await
            .unwrap()
            .0;
        let b = hub
            .create_user(&format!("b{i}"), UserKind::Human, None, Some(unit))
            .await
            .unwrap()
            .0;
        let hub = hub.clone();
        tasks.push(tokio::spawn(async move {
            let ca = Conn::new(&hub, &a);
            let cb = Conn::new(&hub, &b);
            let conv = ca.group(&[&b]).await;
            for k in 0..30 {
                let (c, who) = if k % 2 == 0 { (&ca, "a") } else { (&cb, "b") };
                assert_eq!(c.send(&conv, &format!("{who}{i}-{k}")).await["ok"], true);
            }
            (a, conv)
        }));
    }
    for t in tasks {
        let (a, conv) = t.await.unwrap();
        let c = Conn::new(&hub, &a);
        let log = full_log(&c, &conv).await;
        assert_gap_free(&log);
        let msgs: Vec<&str> = log
            .iter()
            .filter(|e| e["type"] == "msg.created")
            .map(|e| e["msg"]["content"].as_str().unwrap())
            .collect();
        assert_eq!(msgs.len(), 30);
        let own = &a.name[1..];
        assert!(
            msgs.iter().all(|m| m[1..].starts_with(&format!("{own}-"))),
            "only its own messages: {msgs:?}"
        );
        let list = c.ok(json!({"op": "list_convs"})).await;
        assert_eq!(
            list["convs"].as_array().unwrap().len(),
            1,
            "each user sees only its conversation"
        );
    }
}
