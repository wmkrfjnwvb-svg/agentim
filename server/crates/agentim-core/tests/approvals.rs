//! Approval rules (docs/en/design/agents.md, K4): which of an agent's messages wait for
//! approval, the two layers (its owner's rules and what the conversation's
//! managers require), who approves, the reviewing agent, deadlines.
//! Refused settings leave no trace; one decision wins.

mod common;

use std::time::Duration;

use agentim_core::hub::Hub;
use agentim_core::model::*;
use common::*;
use serde_json::{Value, json};
use sqlx::Connection;

backend_tests!(
    rules_pick_the_messages,
    managers_require_approval,
    a_reviewer_goes_first,
    deadlines,
    one_decision_wins,
);

fn code(v: &Value) -> &str {
    v["error"]["code"].as_str().unwrap_or("")
}

fn rule_of(v: &Value) -> &str {
    v["error"]["rule"].as_str().unwrap_or("")
}

/// carol owns the group; alice owns `bot` and `rev` (agents); bob is a
/// member.
struct World {
    hub: Hub,
    alice: User,
    bob: User,
    carol: User,
    bot: User,
    rev: User,
    conv: String,
}

/// Agents here send many messages quickly.
async fn fast(db: &TestDb) -> Hub {
    let mut h = hub_cfg();
    h.guards.rate_capacity = 1000.0;
    h.guards.rate_per_sec = 1000.0;
    h.expire_every = Duration::from_millis(100);
    Hub::open(single(&db.url), h).await.expect("open hub")
}

async fn world(hub: Hub) -> World {
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let carol = user(&hub, "carol", UserKind::Human, None).await;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    let rev = user(&hub, "rev", UserKind::Agent, Some(&alice)).await;
    let conv = Conn::new(&hub, &carol)
        .group(&[&alice, &bob, &bot, &rev])
        .await;
    World {
        hub,
        alice,
        bob,
        carol,
        bot,
        rev,
        conv,
    }
}

fn policy(w: &World, field: &str, v: Value) -> Value {
    let mut p = json!({"op": "set_agent_policy", "conv_id": w.conv, "agent_id": w.bot.id});
    p[field] = v;
    p
}

/// Sends as the agent: `Some(draft_id)` when it waits for approval.
async fn say(g: &Conn, conv: &str, text: &str, extra: Value) -> Option<String> {
    let mut v = json!({"op": "send", "conv_id": conv, "client_msg_id": uuid::Uuid::new_v4().to_string(), "text": text});
    for (k, x) in extra.as_object().unwrap() {
        v[k] = x.clone();
    }
    let r = g.ok(v).await;
    r["draft_id"].as_str().map(String::from)
}

fn sides(c: &mut Conn, kind: &str) -> Vec<Value> {
    c.drain()
        .into_iter()
        .filter(|f| f["type"] == "side" && f["event"]["type"] == kind)
        .collect()
}

async fn events_of(hub: &Hub, u: &User, conv: &str, kind: &str) -> usize {
    hub.read_log(u, conv, 0, 1000).await.unwrap()["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["event"]["type"] == kind)
        .count()
}

async fn rules_pick_the_messages(db: TestDb) {
    let w = world(fast(&db).await).await;
    let a = Conn::new(&w.hub, &w.alice);
    let g = Conn::new(&w.hub, &w.bot);
    a.ok(policy(
        &w,
        "approval",
        json!({"mode": "rules", "rules": ["first_message", "links", "mentions"], "topics": ["payment"]}),
    ))
    .await;
    let c = &w.conv;
    assert!(
        say(&g, c, "hello", json!({})).await.is_some(),
        "its first message"
    );
    // A draft is not a message: the first one sent is still to come.
    assert!(say(&g, c, "hello again", json!({})).await.is_some());
    let d = a.ok(json!({"op": "list_drafts"})).await["drafts"][0]["draft_id"].clone();
    a.ok(json!({"op": "approve", "draft_id": d})).await;
    assert!(
        say(&g, c, "plain text", json!({})).await.is_none(),
        "no rule matches"
    );
    assert!(
        say(&g, c, "see https://example.com", json!({}))
            .await
            .is_some()
    );
    assert!(say(&g, c, "see WWW.example.com", json!({})).await.is_some());
    assert!(say(&g, c, "ask @bob", json!({})).await.is_some());
    assert!(
        say(&g, c, "an email@bob.com is not a mention", json!({}))
            .await
            .is_none()
    );
    assert!(
        say(&g, c, "pay", json!({"topics": ["payment"]}))
            .await
            .is_some(),
        "a topic"
    );
    assert!(
        say(&g, c, "ship", json!({"topics": ["shipping"]}))
            .await
            .is_none()
    );
    // Streams: content unknown at the start, so links and mentions draft it.
    let s = g
        .ok(json!({"op": "stream_start", "conv_id": c, "client_msg_id": "s1"}))
        .await;
    assert_eq!(s["pending_approval"], true);
    a.ok(policy(
        &w,
        "approval",
        json!({"mode": "rules", "rules": ["first_message", "files"]}),
    ))
    .await;
    let s = g
        .ok(json!({"op": "stream_start", "conv_id": c, "client_msg_id": "s2"}))
        .await;
    assert_eq!(
        s["pending_approval"], false,
        "files can't be streamed; not its first"
    );
    g.ok(json!({"op": "stream_end", "msg_id": s["msg_id"]}))
        .await;
    // off, and the old switch.
    a.ok(policy(&w, "approval", json!({"mode": "off"}))).await;
    assert!(say(&g, c, "https://x.io", json!({})).await.is_none());
    a.ok(policy(&w, "approval", json!(true))).await;
    assert!(say(&g, c, "anything", json!({})).await.is_some());
    a.ok(policy(&w, "approval", json!(false))).await;
    assert!(say(&g, c, "anything else", json!({})).await.is_none());
    // The setting is in the log for everyone, and read back.
    let m = w.hub.read_log(&w.bob, c, 0, 1000).await.unwrap()["events"]
        .as_array()
        .unwrap()
        .iter()
        .rev()
        .find(|e| e["event"]["type"] == "member.policy")
        .unwrap()["event"]["data"]
        .clone();
    assert_eq!(m["approval"], false);
    assert_eq!(m["approval_rules"]["mode"], "off");
    // Refused settings: no trace.
    let before = events_of(&w.hub, &w.alice, c, "member.policy").await;
    for (v, rule) in [
        (json!({"mode": "some"}), "approval.mode"),
        (json!({"mode": "rules"}), "approval.rule"),
        (
            json!({"mode": "rules", "rules": ["emoji"]}),
            "approval.rule",
        ),
        (
            json!({"mode": "all", "topics": vec!["t"; 21]}),
            "approval.topics",
        ),
        (
            json!({"mode": "all", "approvers": [w.carol.name.clone(), "nobody-here"]}),
            "account.unknown_user",
        ),
        (
            json!({"mode": "all", "approvers": [w.rev.id.clone()]}),
            "approval.approvers",
        ),
        (
            json!({"mode": "all", "reviewer": w.bot.id.clone()}),
            "approval.reviewer",
        ),
        (
            json!({"mode": "all", "reviewer": w.bob.id.clone()}),
            "approval.reviewer",
        ),
        (json!({"mode": "all", "ttl_secs": 59}), "approval.ttl"),
        (
            json!({"mode": "all", "review_secs": 86_401}),
            "approval.ttl",
        ),
    ] {
        let r = a.call(policy(&w, "approval", v.clone())).await;
        assert_eq!(rule_of(&r), rule, "{v}: {r}");
    }
    assert_eq!(
        events_of(&w.hub, &w.alice, c, "member.policy").await,
        before
    );
    // Someone else's agent: not theirs to set.
    let b = Conn::new(&w.hub, &w.bob);
    assert_eq!(
        code(&b.call(policy(&w, "approval", json!({"mode": "off"}))).await),
        "forbidden"
    );
}

async fn managers_require_approval(db: TestDb) {
    let w = world(fast(&db).await).await;
    let a = Conn::new(&w.hub, &w.alice);
    let b = Conn::new(&w.hub, &w.bob);
    let k = Conn::new(&w.hub, &w.carol);
    let g = Conn::new(&w.hub, &w.bot);
    // Only the conversation's owner and admins require approval.
    for c in [&a, &b] {
        let r = c
            .call(policy(&w, "required_approval", json!({"mode": "all"})))
            .await;
        assert_eq!(rule_of(&r), "approval.managers_only", "{r}");
    }
    // A required rule names an agent, not a person.
    let mut p = policy(&w, "required_approval", json!({"mode": "all"}));
    p["agent_id"] = json!(w.bob.id);
    let r = k.call(p).await;
    assert_eq!(rule_of(&r), "agent.not_agent", "{r}");
    k.ok(policy(
        &w,
        "required_approval",
        json!({"mode": "all", "approvers": ["managers"]}),
    ))
    .await;
    // The owner's own rules can't lift it.
    a.ok(policy(&w, "approval", json!({"mode": "off"}))).await;
    let d = say(&g, &w.conv, "hi", json!({})).await.expect("required");
    // The required layer's approvers decide: carol, not alice.
    let r = a.call(json!({"op": "approve", "draft_id": d})).await;
    assert_eq!(rule_of(&r), "draft.not_approver");
    assert!(
        k.ok(json!({"op": "inbox"})).await["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["draft_id"] == d.as_str()),
        "in the approver's inbox"
    );
    assert_eq!(
        k.ok(json!({"op": "list_drafts"})).await["drafts"][0]["draft_id"],
        d.as_str()
    );
    k.ok(json!({"op": "approve", "draft_id": d})).await;
    let msg = w.hub.read_log(&w.bob, &w.conv, 0, 1000).await.unwrap()["events"]
        .as_array()
        .unwrap()
        .iter()
        .rev()
        .find(|e| e["event"]["type"] == "msg.created")
        .unwrap()["event"]["data"]["approval"]
        .clone();
    assert_eq!(
        (msg["by"].as_str(), msg["by_type"].as_str()),
        (Some(w.carol.id.as_str()), Some("human"))
    );
    // Named people, who must be members when the message is sent.
    k.ok(policy(
        &w,
        "required_approval",
        json!({"mode": "all", "approvers": [w.bob.name.clone()]}),
    ))
    .await;
    let d = say(&g, &w.conv, "for bob", json!({})).await.unwrap();
    assert_eq!(
        rule_of(&k.call(json!({"op": "reject", "draft_id": d})).await),
        "draft.not_approver"
    );
    b.ok(json!({"op": "reject", "draft_id": d})).await;
    k.ok(json!({"op": "remove_member", "conv_id": w.conv, "user_id": w.bob.id}))
        .await;
    let r = g
        .call(json!({"op": "send", "conv_id": w.conv, "client_msg_id": "x1", "text": "nobody"}))
        .await;
    assert_eq!(rule_of(&r), "draft.no_approver");
    // Lifted by the managers: the owner's rules apply again.
    k.ok(policy(&w, "required_approval", json!({"mode": "off"})))
        .await;
    assert!(say(&g, &w.conv, "free", json!({})).await.is_none());
}

async fn a_reviewer_goes_first(db: TestDb) {
    let w = world(fast(&db).await).await;
    let mut a = Conn::new(&w.hub, &w.alice);
    let mut r = Conn::new(&w.hub, &w.rev);
    let g = Conn::new(&w.hub, &w.bot);
    a.ok(policy(
        &w,
        "approval",
        json!({"mode": "all", "reviewer": w.rev.name.clone()}),
    ))
    .await;
    // Two agents don't review each other.
    let cyc = a
        .call(json!({"op": "set_agent_policy", "conv_id": w.conv, "agent_id": w.rev.id, "approval": {"mode": "all", "reviewer": w.bot.id}}))
        .await;
    assert_eq!(rule_of(&cyc), "approval.reviewer_cycle");
    a.drain();
    r.drain();
    let d = say(&g, &w.conv, "reviewed text", json!({})).await.unwrap();
    let created = sides(&mut r, "draft.created");
    assert_eq!(created.len(), 1, "the reviewer gets the draft");
    assert_eq!(created[0]["event"]["data"]["stage"], "review");
    assert_eq!(sides(&mut a, "draft.created").len(), 1, "people see it too");
    // A reviewer neither rejects nor edits.
    assert_eq!(
        rule_of(&r.call(json!({"op": "reject", "draft_id": d})).await),
        "draft.reviewer_reject"
    );
    assert_eq!(
        rule_of(
            &r.call(json!({"op": "approve", "draft_id": d, "text": "changed"}))
                .await
        ),
        "draft.reviewer_edit"
    );
    // Only the reviewer escalates.
    assert_eq!(
        rule_of(&a.call(json!({"op": "escalate", "draft_id": d})).await),
        "draft.not_reviewer"
    );
    r.ok(json!({"op": "approve", "draft_id": d})).await;
    let approval = w.hub.read_log(&w.bob, &w.conv, 0, 1000).await.unwrap()["events"]
        .as_array()
        .unwrap()
        .iter()
        .rev()
        .find(|e| e["event"]["type"] == "msg.created")
        .unwrap()["event"]["data"]["approval"]
        .clone();
    assert_eq!(approval["by_type"], "agent", "approved by the AI reviewer");
    // Escalated: with the people now; the reviewer is done.
    let d = say(&g, &w.conv, "unsure", json!({})).await.unwrap();
    r.ok(json!({"op": "escalate", "draft_id": d, "note": "not sure about the tone"}))
        .await;
    let esc = sides(&mut a, "draft.escalated");
    assert_eq!(esc[0]["event"]["data"]["note"], "not sure about the tone");
    assert_eq!(
        rule_of(&r.call(json!({"op": "escalate", "draft_id": d})).await),
        "draft.stage"
    );
    assert_eq!(
        rule_of(&r.call(json!({"op": "approve", "draft_id": d})).await),
        "draft.stage"
    );
    assert_eq!(
        rule_of(
            &r.call(json!({"op": "escalate", "draft_id": d, "note": ""}))
                .await
        ),
        "draft.note"
    );
    a.ok(json!({"op": "approve", "draft_id": d, "text": "edited by alice"}))
        .await;
    // A person must approve after the reviewer.
    a.ok(policy(
        &w,
        "approval",
        json!({"mode": "all", "reviewer": w.rev.id.clone(), "human_required": true}),
    ))
    .await;
    let d = say(&g, &w.conv, "needs a person", json!({})).await.unwrap();
    assert_eq!(
        r.ok(json!({"op": "approve", "draft_id": d})).await["stage"],
        "human"
    );
    assert_eq!(
        events_of(&w.hub, &w.bob, &w.conv, "msg.created").await,
        events_of(&w.hub, &w.bob, &w.conv, "msg.created").await
    );
    let before = events_of(&w.hub, &w.bob, &w.conv, "msg.created").await;
    a.ok(json!({"op": "approve", "draft_id": d})).await;
    assert_eq!(
        events_of(&w.hub, &w.bob, &w.conv, "msg.created").await,
        before + 1
    );
    let approval = w.hub.read_log(&w.bob, &w.conv, 0, 1000).await.unwrap()["events"]
        .as_array()
        .unwrap()
        .iter()
        .rev()
        .find(|e| e["event"]["type"] == "msg.created")
        .unwrap()["event"]["data"]["approval"]
        .clone();
    assert_eq!(
        (
            approval["by_type"].as_str(),
            approval["reviewed_by"].as_str()
        ),
        (Some("human"), Some(w.rev.id.as_str()))
    );
    // A person may decide while it is still in review.
    let d = say(&g, &w.conv, "early", json!({})).await.unwrap();
    a.ok(json!({"op": "reject", "draft_id": d})).await;
    assert_eq!(
        rule_of(&r.call(json!({"op": "approve", "draft_id": d})).await),
        "draft.decided"
    );
}

/// Moves a draft's deadline into the past, as time would.
async fn age(db: &TestDb, draft_id: &str, column: &str) {
    sqlx::any::install_default_drivers();
    let d = agentim_core::storage::Dialect::from_url(&db.url).unwrap();
    let mut c = sqlx::AnyConnection::connect(&db.url.replace("mariadb://", "mysql://"))
        .await
        .unwrap();
    sqlx::query(d.q(&format!(
        "UPDATE drafts SET {column} = 1 WHERE draft_id = ?"
    )))
    .bind(draft_id.to_string())
    .execute(&mut c)
    .await
    .unwrap();
}

async fn deadlines(db: TestDb) {
    let w = world(fast(&db).await).await;
    let mut a = Conn::new(&w.hub, &w.alice);
    let mut g = Conn::new(&w.hub, &w.bot);
    a.ok(policy(
        &w,
        "approval",
        json!({"mode": "all", "reviewer": w.rev.id.clone(), "ttl_secs": 3600, "review_secs": 60}),
    ))
    .await;
    let d = say(&g, &w.conv, "slow review", json!({})).await.unwrap();
    let keep = say(&g, &w.conv, "untouched", json!({})).await.unwrap();
    a.drain();
    age(&db, &d, "review_until").await;
    let mut stage = String::new();
    for _ in 0..100 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        w.hub.tick().await;
        let esc = sides(&mut a, "draft.escalated");
        if let Some(e) = esc.first() {
            stage = e["event"]["data"]["reason"].as_str().unwrap().to_string();
            break;
        }
    }
    assert_eq!(stage, "timeout", "an overdue review goes to the people");
    let r = Conn::new(&w.hub, &w.rev);
    assert_eq!(
        rule_of(&r.call(json!({"op": "approve", "draft_id": d})).await),
        "draft.stage"
    );
    // Past its time a draft can't be approved, and the timer drops it.
    age(&db, &d, "expires_at").await;
    assert_eq!(
        rule_of(&a.call(json!({"op": "approve", "draft_id": d})).await),
        "draft.expired"
    );
    g.drain();
    let mut gone = false;
    for _ in 0..100 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        w.hub.tick().await;
        if sides(&mut g, "draft.decided")
            .iter()
            .any(|s| s["event"]["data"]["state"] == "expired")
        {
            gone = true;
            break;
        }
    }
    assert!(gone, "the agent hears it expired");
    assert!(
        !a.ok(json!({"op": "list_drafts"})).await["drafts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x["draft_id"] == d.as_str())
    );
    assert!(
        w.hub
            .audit_log(1000)
            .await
            .unwrap()
            .iter()
            .any(|x| x["action"] == "draft.expired")
    );
    // The other one waits on.
    a.ok(json!({"op": "approve", "draft_id": keep})).await;
}

/// A person and the reviewer approve the same draft at once, many times:
/// exactly one message each time.
async fn one_decision_wins(db: TestDb) {
    let w = world(fast(&db).await).await;
    let a = Conn::new(&w.hub, &w.alice);
    let g = Conn::new(&w.hub, &w.bot);
    a.ok(policy(
        &w,
        "approval",
        json!({"mode": "all", "reviewer": w.rev.id.clone()}),
    ))
    .await;
    for i in 0..20 {
        let before = events_of(&w.hub, &w.bob, &w.conv, "msg.created").await;
        let d = say(&g, &w.conv, &format!("race {i}"), json!({}))
            .await
            .unwrap();
        let (h1, h2) = (w.hub.clone(), w.hub.clone());
        let (alice, rev) = (w.alice.clone(), w.rev.clone());
        let (d1, d2) = (d.clone(), d.clone());
        let x = tokio::spawn(async move {
            Conn::new(&h1, &alice)
                .call(json!({"op": "approve", "draft_id": d1}))
                .await
        });
        let y = tokio::spawn(async move {
            Conn::new(&h2, &rev)
                .call(json!({"op": "approve", "draft_id": d2}))
                .await
        });
        let (x, y) = (x.await.unwrap(), y.await.unwrap());
        let wins = [&x, &y].iter().filter(|r| r["ok"] == true).count();
        assert_eq!(wins, 1, "{x} {y}");
        let lost = if x["ok"] == true { &y } else { &x };
        assert_eq!(code(lost), "conflict", "{lost}");
        assert_eq!(
            events_of(&w.hub, &w.bob, &w.conv, "msg.created").await,
            before + 1
        );
    }
}
