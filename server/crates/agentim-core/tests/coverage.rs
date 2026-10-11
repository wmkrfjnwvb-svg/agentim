//! Gaps found by the coverage audit (docs/05-operations.md §5.5): paths and
//! boundaries of existing features that no other test exercised. Run against
//! every backend.

mod common;

use std::sync::Arc;
use std::time::Duration;

use agentim_core::config::Config;
use agentim_core::guard::GuardConfig;
use agentim_core::hub::Hub;
use agentim_core::logic::HubConfig;
use agentim_core::model::*;
use common::*;
use serde_json::{Value, json};

backend_tests!(
    one_failing_command_does_not_spoil_its_batch,
    a_full_write_queue_refuses_cleanly_and_retries_succeed,
    expired_dedup_records_are_collected,
    agent_daily_budget_counts_messages_and_streamed_text,
    budget_is_shared_across_conversations_and_databases_under_concurrency,
    a_tripped_guard_recovers_after_cooldown_and_reset_needs_a_trip,
    dlp_covers_agent_edits_and_ephemerals_but_not_people,
    observing_through_an_agent_is_owner_only_and_audited,
    focus_through_an_agent_and_focus_errors,
    streamed_drafts_stay_private_until_approved,
    approving_while_taken_over_is_refused,
    race_approval_against_takeover,
    agent_policy_targets_only_your_own_agents,
    mentions_ignore_non_members_and_ephemerals_to_yourself,
    edit_boundaries,
    user_and_conversation_listing_edges,
    stats_are_not_open_to_users,
    side_channel_paging_and_read_errors,
);

fn code(r: &Value) -> &str {
    r["error"]["code"].as_str().unwrap_or("")
}

fn log(p: &Value) -> Vec<Value> {
    p["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["event"].clone())
        .collect()
}

fn gap_free(events: &[Value]) {
    let s: Vec<i64> = events.iter().map(|e| e["seq"].as_i64().unwrap()).collect();
    assert_eq!(s, (1..=s.len() as i64).collect::<Vec<_>>(), "gap-free");
}

async fn open_with(cfg: Config, f: impl FnOnce(&mut HubConfig)) -> Hub {
    let mut h = hub_cfg();
    if cfg.storage.databases.len() > 1 {
        h.edition = agentim_core::edition::Edition::Full;
    }
    f(&mut h);
    Hub::open(cfg, h).await.expect("open hub")
}

fn start(conv: &str, id: &str, reply_to: &str) -> Value {
    json!({"op": "stream_start", "conv_id": conv, "client_msg_id": id, "reply_to": reply_to})
}

// ------------------------------------------------------------ write path

async fn one_failing_command_does_not_spoil_its_batch(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let a = Arc::new(Conn::new(&hub, &alice));
    let conv = a.group(&[&bob]).await;
    hub.inject_job_failures("main", 3);
    let tasks: Vec<_> = (0..30)
        .map(|i| {
            let a = a.clone();
            let conv = conv.clone();
            tokio::spawn(async move {
                let r = a
                    .call(json!({"op": "send", "conv_id": conv, "client_msg_id": format!("m{i}"), "text": format!("t{i}")}))
                    .await;
                (i, r)
            })
        })
        .collect();
    let mut failed = vec![];
    let mut acked = vec![];
    for t in tasks {
        let (i, r) = t.await.unwrap();
        if r["ok"] == true {
            acked.push(format!("t{i}"));
        } else {
            assert_eq!(code(&r), "storage", "{r}");
            failed.push(i);
        }
    }
    assert_eq!(failed.len(), 3, "exactly the injected commands fail");
    assert!(
        hub.storage_failed().is_none(),
        "a statement failure is not a storage failure"
    );
    let events = log(&a.pull(&conv, 0).await);
    gap_free(&events);
    let texts: Vec<String> = events
        .iter()
        .filter_map(|e| e["msg"]["content"].as_str().map(String::from))
        .collect();
    for t in &acked {
        assert!(texts.contains(t), "acknowledged {t} is stored");
    }
    for i in &failed {
        assert!(
            !texts.contains(&format!("t{i}")),
            "a failed command leaves no trace"
        );
    }
    // Retrying a failed command with the same id succeeds once.
    let i = failed[0];
    let r = a
        .ok(json!({"op": "send", "conv_id": conv, "client_msg_id": format!("m{i}"), "text": format!("t{i}")}))
        .await;
    assert!(r["duplicate"].is_null());
}

async fn a_full_write_queue_refuses_cleanly_and_retries_succeed(db: TestDb) {
    let mut cfg = single(&db.url);
    cfg.writer.queue_capacity = 4;
    let hub = open(cfg).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let a = Arc::new(Conn::new(&hub, &alice));
    let conv = a.group(&[&bob]).await;
    let send = |i: usize| json!({"op": "send", "conv_id": conv, "client_msg_id": format!("q{i}"), "text": format!("q{i}")});
    let tasks: Vec<_> = (0..200)
        .map(|i| {
            let (a, req) = (a.clone(), send(i));
            tokio::spawn(async move { (i, a.call(req).await) })
        })
        .collect();
    let mut refused = vec![];
    for t in tasks {
        let (i, r) = t.await.unwrap();
        if r["ok"] != true {
            assert_eq!(code(&r), "overloaded", "{r}");
            refused.push(i);
        }
    }
    assert!(
        !refused.is_empty(),
        "a queue of 4 refuses part of a burst of 200"
    );
    for i in &refused {
        a.ok(send(*i)).await;
    }
    let events = log(&a.pull(&conv, 0).await);
    gap_free(&events);
    assert_eq!(
        events.iter().filter(|e| e["type"] == "msg.created").count(),
        200,
        "each message exactly once"
    );
}

async fn expired_dedup_records_are_collected(db: TestDb) {
    let hub = open_with(single(&db.url), |h| {
        h.dedup_ttl = Duration::from_millis(1);
        h.gc_every = Duration::from_millis(1);
    })
    .await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let a = Conn::new(&hub, &alice);
    let conv = a.group(&[]).await;
    let req = json!({"op": "send", "conv_id": conv, "client_msg_id": "x", "text": "hi"});
    let first = a.ok(req.clone()).await;
    assert_eq!(
        a.ok(req.clone()).await["duplicate"],
        true,
        "within the window"
    );
    tokio::time::sleep(Duration::from_millis(20)).await;
    hub.tick().await;
    let late = a.ok(req).await;
    assert!(
        late["duplicate"].is_null(),
        "after the window a retry is a new message"
    );
    assert_ne!(late["msg_id"], first["msg_id"]);
}

// ---------------------------------------------------------------- guards

async fn agent_daily_budget_counts_messages_and_streamed_text(db: TestDb) {
    let hub = open_with(single(&db.url), |h| {
        h.guards = GuardConfig {
            daily_messages: 3,
            daily_chars: 50,
            rate_capacity: 100.0,
            ..h.guards.clone()
        };
    })
    .await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    let other = user(&hub, "other", UserKind::Agent, Some(&alice)).await;
    let a = Conn::new(&hub, &alice);
    let g = Conn::new(&hub, &bot);
    let o = Conn::new(&hub, &other);
    let conv = a.group(&[&bot, &other]).await;
    let q = a.send(&conv, "@bot @other hi").await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    for i in 0..3 {
        assert_eq!(g.reply(&conv, &format!("answer {i}"), &q).await["ok"], true);
    }
    assert_eq!(
        code(&g.reply(&conv, "one too many", &q).await),
        "budget_exceeded"
    );
    assert!(
        hub.stats().await.unwrap()["audit"]
            .to_string()
            .contains("guard.reject"),
        "rejections are audited"
    );
    // Budgets are per agent: another agent is unaffected.
    assert_eq!(o.reply(&conv, "still fine", &q).await["ok"], true);
    // Streamed characters count against the character budget.
    let s = o.ok(start(&conv, "s", &q)).await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    o.ok(json!({"op": "stream_append", "msg_id": s, "idx": 0, "text": "x".repeat(20)}))
        .await;
    let r = o
        .call(json!({"op": "stream_append", "msg_id": s, "idx": 1, "text": "y".repeat(40)}))
        .await;
    assert_eq!(code(&r), "budget_exceeded");
    let m = msg_by_id(&a.pull(&conv, 0).await, &s).unwrap();
    assert_eq!(
        (m["state"].as_str(), m["state_reason"].as_str()),
        (Some("cancelled"), Some("budget_exceeded"))
    );
}

async fn budget_is_shared_across_conversations_and_databases_under_concurrency(db: TestDb) {
    let secret = new_db(Backend::Sqlite).await.unwrap();
    let hub = open_with(two(&db.url, &secret.url, "secret"), |h| {
        h.guards = GuardConfig {
            daily_messages: 10,
            rate_capacity: 1000.0,
            dup_threshold: 1000,
            ..h.guards.clone()
        };
    })
    .await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let zed = user_in(&hub, "zed", "b").await;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    let a = Conn::new(&hub, &alice);
    let z = Conn::new(&hub, &zed);
    let main_conv = a.group(&[&bot]).await;
    let secret_conv = z.group(&[&bot]).await; // lives in the other database
    let qa = a.send(&main_conv, "@bot go").await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    let qz = z.send(&secret_conv, "@bot go").await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    let tasks: Vec<_> = (0..30)
        .map(|i| {
            let c = Conn::new(&hub, &bot);
            let (conv, q) = if i % 2 == 0 {
                (main_conv.clone(), qa.clone())
            } else {
                (secret_conv.clone(), qz.clone())
            };
            tokio::spawn(async move { c.reply(&conv, &format!("reply {i}"), &q).await })
        })
        .collect();
    let mut ok = 0;
    for t in tasks {
        let r = t.await.unwrap();
        if r["ok"] == true {
            ok += 1;
        } else {
            assert_eq!(code(&r), "budget_exceeded", "{r}");
        }
    }
    assert_eq!(
        ok, 10,
        "the daily budget holds exactly across both databases"
    );
}

async fn a_tripped_guard_recovers_after_cooldown_and_reset_needs_a_trip(db: TestDb) {
    let hub = open_with(single(&db.url), |h| {
        h.guards = GuardConfig {
            cooldown: Duration::from_millis(300),
            rate_capacity: 100.0,
            ..h.guards.clone()
        };
    })
    .await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    let a = Conn::new(&hub, &alice);
    let g = Conn::new(&hub, &bot);
    let conv = a.group(&[&bot]).await;
    let reset = json!({"op": "reset_guard", "conv_id": conv, "agent_id": bot.id});
    assert_eq!(
        code(&a.call(reset.clone()).await),
        "conflict",
        "nothing to reset"
    );
    let q = a.send(&conv, "@bot repeat").await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    for _ in 0..2 {
        g.reply(&conv, "same words", &q).await;
    }
    assert_eq!(
        code(&g.reply(&conv, "same words", &q).await),
        "loop_detected"
    );
    assert_eq!(
        code(&g.reply(&conv, "different now", &q).await),
        "circuit_open"
    );
    tokio::time::sleep(Duration::from_millis(350)).await;
    assert_eq!(
        g.reply(&conv, "after the cooldown", &q).await["ok"],
        true,
        "the breaker closes by itself"
    );
    // Trip again, then the owner resets it at once.
    for _ in 0..2 {
        g.reply(&conv, "again and again", &q).await;
    }
    g.reply(&conv, "again and again", &q).await;
    a.ok(reset.clone()).await;
    assert_eq!(g.reply(&conv, "reset worked", &q).await["ok"], true);
    assert_eq!(code(&a.call(reset).await), "conflict");
}

// ---------------------------------------------------------------- policy

async fn dlp_covers_agent_edits_and_ephemerals_but_not_people(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    let a = Conn::new(&hub, &alice);
    let b = Conn::new(&hub, &bob);
    let g = Conn::new(&hub, &bot);
    let conv = a.group(&[&bob, &bot]).await;
    let card = "4111 1111 1111 1111";
    let q = b.send(&conv, "@bot hi").await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    let m = g.reply(&conv, "your order is ready", &q).await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    let edit = g
        .call(json!({"op": "edit", "msg_id": m, "text": format!("card {card}")}))
        .await;
    assert_eq!(code(&edit), "policy_blocked");
    let eph = g
        .call(json!({"op": "send", "conv_id": conv, "client_msg_id": "e", "text": "key sk_live_abcdefghijklmnopqrstuv", "to_user": bob.id}))
        .await;
    assert_eq!(code(&eph), "policy_blocked");
    let history = a.pull(&conv, 0).await.to_string();
    assert!(!history.contains("4111") && !history.contains("sk_live"));
    assert!(
        !b.ok(json!({"op": "pull_side", "conv_id": conv}))
            .await
            .to_string()
            .contains("sk_live")
    );
    assert_eq!(
        msg_by_id(&a.pull(&conv, 0).await, &m).unwrap()["content"],
        "your order is ready"
    );
    // People are not filtered (the filter is for agent output).
    assert_eq!(
        b.send(&conv, &format!("my card is {card}")).await["ok"],
        true
    );
}

// ----------------------------------------------------- observing and focus

async fn observing_through_an_agent_is_owner_only_and_audited(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let carol = user(&hub, "carol", UserKind::Human, None).await;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    let b = Conn::new(&hub, &bob);
    let a = Conn::new(&hub, &alice);
    let c = Conn::new(&hub, &carol);
    let conv = b.group(&[&bot]).await; // alice is not a member, her agent is
    b.send(&conv, "@bot hello").await;
    let observe = |who: &str| json!({"op": "pull", "conv_id": conv, "as_agent": who});
    let seen = a.ok(observe(&bot.id)).await;
    assert_eq!(seen["filtered"], true);
    assert!(seen.to_string().contains("@bot hello"));
    assert_eq!(
        code(&c.call(observe(&bot.id)).await),
        "forbidden",
        "not the owner"
    );
    assert_eq!(
        code(&a.call(observe(&bob.id)).await),
        "forbidden",
        "not an agent"
    );
    assert_eq!(code(&a.call(observe("nobody")).await), "not_found");
    assert_eq!(
        code(&a.call(json!({"op": "pull", "conv_id": conv})).await),
        "forbidden",
        "not a member herself"
    );
    assert!(
        hub.stats().await.unwrap()["audit"]
            .to_string()
            .contains("observe"),
        "observing is audited"
    );
    let listed = a.ok(json!({"op": "list_convs"})).await;
    let entry = listed["convs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["conv_id"] == conv.as_str())
        .unwrap()
        .clone();
    assert_eq!(entry["observer_via"], bot.id.as_str());
    assert_eq!(entry["unread"], 0, "unread counts only for direct members");
}

async fn focus_through_an_agent_and_focus_errors(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    let mut a = Conn::new(&hub, &alice);
    let b = Conn::new(&hub, &bob);
    let g = Conn::new(&hub, &bot);
    let conv = b.group(&[&bot]).await;
    let q = b.send(&conv, "@bot tell me").await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        code(&a.call(json!({"op": "focus", "conv_id": conv})).await),
        "forbidden"
    );
    assert_eq!(
        code(&a.call(json!({"op": "focus", "conv_id": "c_unknown"})).await),
        "not_found"
    );
    a.ok(json!({"op": "focus", "conv_id": conv, "as_agent": bot.id}))
        .await;
    a.drain();
    let s = g.ok(start(&conv, "s", &q)).await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    g.ok(json!({"op": "stream_append", "msg_id": s, "idx": 0, "text": "watching"}))
        .await;
    hub.tick().await;
    assert!(
        a.drain()
            .iter()
            .any(|f| f["type"] == "delta" && f["text"] == "watching"),
        "the owner watches through the agent"
    );
    a.ok(json!({"op": "focus", "conv_id": null})).await;
}

// ---------------------------------------------------------------- approval

async fn streamed_drafts_stay_private_until_approved(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    let mut a = Conn::new(&hub, &alice);
    let mut b = Conn::new(&hub, &bob);
    let g = Conn::new(&hub, &bot);
    let conv = a.group(&[&bob, &bot]).await;
    a.ok(json!({"op": "set_agent_policy", "conv_id": conv, "agent_id": bot.id, "approval": true}))
        .await;
    let q = b.send(&conv, "@bot answer").await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    let head = log(&a.pull(&conv, 0).await).len();
    a.ok(json!({"op": "focus", "conv_id": conv})).await;
    b.ok(json!({"op": "focus", "conv_id": conv})).await;
    a.drain();
    b.drain();
    let r = g.ok(start(&conv, "s", &q)).await;
    assert_eq!(r["pending_approval"], true);
    assert!(r["seq"].is_null(), "a streamed draft takes no seq");
    let d = r["msg_id"].as_str().unwrap().to_string();
    g.ok(json!({"op": "stream_append", "msg_id": d, "idx": 0, "text": "draft text"}))
        .await;
    hub.tick().await;
    assert!(
        a.drain()
            .iter()
            .any(|f| f["type"] == "delta" && f["text"] == "draft text"),
        "the owner sees the draft live"
    );
    assert!(
        !b.drain()
            .iter()
            .any(|f| f.to_string().contains("draft text")),
        "others see nothing"
    );
    g.ok(json!({"op": "stream_end", "msg_id": d})).await;
    assert_eq!(log(&a.pull(&conv, 0).await).len(), head);
    assert_eq!(
        a.ok(json!({"op": "list_drafts"})).await["drafts"][0]["content"],
        "draft text"
    );
    a.ok(json!({"op": "approve", "draft_id": d})).await;
    assert!(b.pull(&conv, 0).await.to_string().contains("draft text"));
    // A cancelled or empty streamed draft is discarded.
    let d2 = g.ok(start(&conv, "s2", &q)).await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    g.ok(json!({"op": "stream_append", "msg_id": d2, "idx": 0, "text": "abandon"}))
        .await;
    a.ok(json!({"op": "stream_cancel", "msg_id": d2})).await;
    let d3 = g.ok(start(&conv, "s3", &q)).await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    g.ok(json!({"op": "stream_end", "msg_id": d3})).await;
    assert_eq!(
        a.ok(json!({"op": "list_drafts"})).await["drafts"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    let side = a
        .ok(json!({"op": "pull_side", "conv_id": conv}))
        .await
        .to_string();
    assert_eq!(side.matches("draft.discarded").count(), 2, "{side}");
    assert!(!b.pull(&conv, 0).await.to_string().contains("abandon"));
}

async fn approving_while_taken_over_is_refused(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    let a = Conn::new(&hub, &alice);
    let g = Conn::new(&hub, &bot);
    let conv = a.group(&[&bot]).await;
    a.ok(json!({"op": "set_agent_policy", "conv_id": conv, "agent_id": bot.id, "approval": true}))
        .await;
    let q = a.send(&conv, "@bot hi").await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    let d = g.reply(&conv, "a draft", &q).await["draft_id"]
        .as_str()
        .unwrap()
        .to_string();
    a.ok(json!({"op": "takeover", "conv_id": conv, "agent_id": bot.id}))
        .await;
    assert_eq!(
        code(&a.call(json!({"op": "approve", "draft_id": d})).await),
        "conflict"
    );
    a.ok(json!({"op": "release", "conv_id": conv, "agent_id": bot.id}))
        .await;
    a.ok(json!({"op": "approve", "draft_id": d})).await;
}

async fn race_approval_against_takeover(db: TestDb) {
    let hub = open_with(single(&db.url), |h| h.guards.rate_capacity = 1e9).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    let owner: Vec<Arc<Conn>> = (0..2).map(|_| Arc::new(Conn::new(&hub, &alice))).collect();
    let g = Conn::new(&hub, &bot);
    let conv = owner[0].group(&[&bot]).await;
    owner[0].ok(json!({"op": "set_agent_policy", "conv_id": conv, "agent_id": bot.id, "approval": true})).await;
    let q = owner[0].send(&conv, "@bot go").await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    for round in 0..15 {
        let d = g.reply(&conv, &format!("draft {round}"), &q).await["draft_id"]
            .as_str()
            .unwrap()
            .to_string();
        let (o0, o1) = (owner[0].clone(), owner[1].clone());
        let conv2 = conv.clone();
        let bot_id = bot.id.clone();
        let t1 =
            tokio::spawn(async move { o0.call(json!({"op": "approve", "draft_id": d})).await });
        let t2 = tokio::spawn(async move {
            o1.call(json!({"op": "takeover", "conv_id": conv2, "agent_id": bot_id}))
                .await
        });
        let (r1, r2) = (t1.await.unwrap(), t2.await.unwrap());
        assert!(r1["ok"] == true || code(&r1) == "conflict", "{r1}");
        assert_eq!(r2["ok"], true, "{r2}");
        owner[0]
            .ok(json!({"op": "release", "conv_id": conv, "agent_id": bot.id}))
            .await;
    }
    // No agent message ever lands while it is taken over.
    let mut taken = false;
    for e in log(&owner[0].pull(&conv, 0).await) {
        if e["type"] == "agent.state" {
            taken = e["data"]["state"] == "taken_over";
        }
        if e["type"] == "msg.created" && e["msg"]["sender_id"] == bot.id.as_str() {
            assert!(
                !taken,
                "agent message at seq {} during a takeover",
                e["seq"]
            );
        }
    }
}

// ------------------------------------------------------- policy and misc

async fn agent_policy_targets_only_your_own_agents(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let bobbot = user(&hub, "bobbot", UserKind::Agent, Some(&bob)).await;
    let a = Conn::new(&hub, &alice);
    let conv = a.group(&[&bob, &bobbot]).await;
    let policy = |who: &str| json!({"op": "set_agent_policy", "conv_id": conv, "agent_id": who, "trigger": "all"});
    assert_eq!(
        code(&a.call(policy(&bob.id)).await),
        "forbidden",
        "a person is not an agent"
    );
    assert_eq!(
        code(&a.call(policy(&bobbot.id)).await),
        "forbidden",
        "not her agent, even as the owner of the conversation"
    );
    assert_eq!(code(&a.call(policy("nobody")).await), "not_found");
    let bad = a.call(json!({"op": "set_agent_policy", "conv_id": conv, "agent_id": bobbot.id, "trigger": "sometimes"})).await;
    assert_eq!(code(&bad), "bad_request");
}

async fn mentions_ignore_non_members_and_ephemerals_to_yourself(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let _dave = user(&hub, "dave", UserKind::Human, None).await;
    let a = Conn::new(&hub, &alice);
    let mut b = Conn::new(&hub, &bob);
    let conv = a.group(&[&bob]).await;
    let id = a.send(&conv, "@dave are you here? @bob").await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    let m = msg_by_id(&a.pull(&conv, 0).await, &id).unwrap();
    assert_eq!(m["mentions"], json!([bob.id]), "only members are mentioned");
    b.drain();
    let r = a.ok(json!({"op": "send", "conv_id": conv, "client_msg_id": "self", "text": "note to self", "to_user": alice.id})).await;
    assert!(r["side_seq"].is_number());
    assert!(
        b.drain().is_empty(),
        "a note to yourself reaches nobody else"
    );
}

async fn edit_boundaries(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    let a = Conn::new(&hub, &alice);
    let g = Conn::new(&hub, &bot);
    let conv = a.group(&[&bot]).await;
    let m = a.send(&conv, "short").await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    let edit = |id: &str, t: String| json!({"op": "edit", "msg_id": id, "text": t});
    a.ok(edit(&m, "a".repeat(64 * 1024))).await;
    assert_eq!(
        code(&a.call(edit(&m, "a".repeat(64 * 1024 + 1))).await),
        "bad_request"
    );
    assert_eq!(
        code(&a.call(edit(&m, "a\u{0}b".into())).await),
        "bad_request"
    );
    // A stream that ended early can be completed by its agent or owner.
    let q = a.send(&conv, "@bot write").await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    let s = g.ok(start(&conv, "s", &q)).await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    g.ok(json!({"op": "stream_append", "msg_id": s, "idx": 0, "text": "half"}))
        .await;
    a.ok(json!({"op": "stream_cancel", "msg_id": s})).await;
    a.ok(edit(&s, "completed by hand".into())).await;
    let fixed = msg_by_id(&a.pull(&conv, 0).await, &s).unwrap();
    assert_eq!(
        (fixed["content"].as_str(), fixed["state"].as_str()),
        (Some("completed by hand"), Some("cancelled"))
    );
}

async fn user_and_conversation_listing_edges(db: TestDb) {
    let hub = open(single(&db.url)).await;
    user(&hub, "alice", UserKind::Human, None).await;
    let dup = hub.create_user("alice", UserKind::Human, None, None).await;
    assert!(dup.is_err(), "user names are unique");
    let alice = hub.token_of("alice").await.unwrap().0;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    let g = Conn::new(&hub, &bot);
    let a = Conn::new(&hub, &alice);
    let conv = a.group(&[&bot]).await;
    let listed = g.ok(json!({"op": "list_convs"})).await;
    assert_eq!(
        listed["convs"][0]["conv_id"],
        conv.as_str(),
        "agents list their conversations"
    );
    assert!(hub.authenticate("not-a-token").is_none());
}

async fn stats_are_not_open_to_users(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let a = Conn::new(&hub, &alice);
    assert_eq!(code(&a.call(json!({"op": "stats"})).await), "forbidden");
    let demo = open_with(single(&new_db(Backend::Sqlite).await.unwrap().url), |h| {
        h.open_stats = true
    })
    .await;
    let u = user(&demo, "u", UserKind::Human, None).await;
    assert_eq!(
        Conn::new(&demo, &u).call(json!({"op": "stats"})).await["ok"],
        true,
        "open in the demo"
    );
}

async fn side_channel_paging_and_read_errors(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    let a = Conn::new(&hub, &alice);
    let conv = a.group(&[&bot]).await;
    for i in 0..5 {
        a.ok(
            json!({"op": "whisper", "conv_id": conv, "agent_id": bot.id, "text": format!("w{i}")}),
        )
        .await;
    }
    let all = a.ok(json!({"op": "pull_side", "conv_id": conv})).await["side_events"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(all.len(), 5);
    let third = all[2]["side_seq"].as_i64().unwrap();
    let rest = a
        .ok(json!({"op": "pull_side", "conv_id": conv, "after_side_seq": third}))
        .await;
    assert_eq!(rest["side_events"].as_array().unwrap().len(), 2);
    assert_eq!(
        code(
            &a.call(json!({"op": "pull_side", "conv_id": "c_unknown"}))
                .await
        ),
        "not_found"
    );
    assert_eq!(
        code(
            &a.call(json!({"op": "read", "conv_id": "c_unknown", "seq": 1}))
                .await
        ),
        "not_found"
    );
    a.ok(json!({"op": "read", "conv_id": conv, "seq": -5}))
        .await; // harmless no-op
}

/// PostgreSQL only: the setting can be changed for one database without
/// touching the others. (MySQL's equivalents are global, see docs/05 §5.5.)
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unsafe_postgres_durability_is_refused_unless_allowed() {
    use sqlx::Connection;
    let Some(db) = new_db(Backend::Postgres).await else {
        eprintln!("skipped: AGENTIM_TEST_PG not set");
        return;
    };
    let name = db.url.rsplit('/').next().unwrap().to_string();
    let base = std::env::var("AGENTIM_TEST_PG").unwrap();
    let mut admin = sqlx::AnyConnection::connect(&format!("{base}/postgres"))
        .await
        .unwrap();
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "ALTER DATABASE {name} SET synchronous_commit = off"
    )))
    .execute(&mut admin)
    .await
    .unwrap();
    let err = Hub::open(single(&db.url), hub_cfg())
        .await
        .err()
        .expect("must refuse");
    assert_eq!(err.code, "unsafe_durability", "{}", err.message);
    let mut cfg = single(&db.url);
    cfg.storage.databases[0].unsafe_durability = true;
    assert!(
        Hub::open(cfg, hub_cfg()).await.is_ok(),
        "an explicit override is honoured"
    );
}

/// SQLite: a server starting while another connection is writing (an old
/// process still shutting down) waits for the lock instead of failing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn startup_waits_for_a_concurrent_sqlite_writer() {
    use sqlx::Connection;
    for round in 0..5 {
        let db = new_db(Backend::Sqlite).await.unwrap();
        let hub = open(single(&db.url)).await;
        user(&hub, "seed", UserKind::Human, None).await;
        drop(hub);
        let mut other = sqlx::AnyConnection::connect(&db.url).await.unwrap();
        sqlx::query("PRAGMA busy_timeout = 5000")
            .execute(&mut other)
            .await
            .unwrap();
        sqlx::query("BEGIN IMMEDIATE")
            .execute(&mut other)
            .await
            .unwrap();
        sqlx::query("UPDATE meta SET v = v WHERE k = 'schema_version'")
            .execute(&mut other)
            .await
            .unwrap();
        let opening = tokio::spawn({
            let url = db.url.clone();
            async move { Hub::open(single(&url), hub_cfg()).await.map(|_| ()) }
        });
        tokio::time::sleep(Duration::from_millis(300)).await;
        sqlx::query("COMMIT").execute(&mut other).await.unwrap();
        let res = opening.await.unwrap();
        assert!(
            res.is_ok(),
            "round {round}: startup failed: {:?}",
            res.err()
        );
    }
}

/// SQLite: when another connection writes while the server's writer has
/// already read (e.g. an old process finishing during a restart), the batch
/// waits for the lock instead of failing with SQLITE_BUSY.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn writes_wait_for_a_concurrent_sqlite_writer() {
    use sqlx::Connection;
    for round in 0..5 {
        let db = new_db(Backend::Sqlite).await.unwrap();
        let hub = open(single(&db.url)).await;
        let alice = user(&hub, "alice", UserKind::Human, None).await;
        let a = Arc::new(Conn::new(&hub, &alice));
        let conv = a.group(&[]).await;
        let mut other = sqlx::AnyConnection::connect(&db.url).await.unwrap();
        sqlx::query("PRAGMA busy_timeout = 5000")
            .execute(&mut other)
            .await
            .unwrap();
        sqlx::query("BEGIN IMMEDIATE")
            .execute(&mut other)
            .await
            .unwrap();
        sqlx::query("UPDATE meta SET v = v WHERE k = 'schema_version'")
            .execute(&mut other)
            .await
            .unwrap();
        let sending = tokio::spawn({
            let (a, conv) = (a.clone(), conv.clone());
            async move { a.send(&conv, "while someone else writes").await }
        });
        tokio::time::sleep(Duration::from_millis(300)).await;
        sqlx::query("COMMIT").execute(&mut other).await.unwrap();
        let r = sending.await.unwrap();
        assert_eq!(r["ok"], true, "round {round}: {r}");
        assert!(hub.storage_failed().is_none());
    }
}
