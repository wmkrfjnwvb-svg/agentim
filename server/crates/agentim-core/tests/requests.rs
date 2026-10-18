//! Requests for a human decision (docs/03-ai-native.md §9.3.4): every kind
//! and decision, who may ask, decide, see and cancel, the inbox with drafts,
//! expiry, removal, limits, refusals that leave no trace, restart, several
//! databases and moving a conversation, the AI label on approved drafts, and
//! decisions racing each other.

mod common;

use std::time::Duration;

use agentim_core::hub::Hub;
use agentim_core::logic::HubConfig;
use agentim_core::model::*;
use common::*;
use serde_json::{Value, json};
use sqlx::Connection;

backend_tests!(
    every_kind_is_asked_and_decided,
    refused_requests_leave_no_trace,
    only_the_right_people_decide_once,
    requests_are_seen_by_their_parties_and_can_be_awaited,
    the_inbox_lists_what_waits_for_me,
    requests_expire,
    removal_cancels_and_limits_hold,
    requests_survive_a_restart,
    approved_drafts_carry_the_approval,
    race_decisions_have_one_winner,
);

fn code(v: &Value) -> &str {
    v["error"]["code"].as_str().unwrap_or("")
}

async fn hub_with(db: &TestDb, f: impl FnOnce(&mut HubConfig)) -> Hub {
    let mut h = hub_cfg();
    f(&mut h);
    Hub::open(single(&db.url), h).await.expect("open hub")
}

/// alice owns `bot`; bob and carol are people; all four in one group.
struct World {
    hub: Hub,
    alice: User,
    bob: User,
    carol: User,
    bot: User,
    conv: String,
}

async fn world(hub: Hub) -> World {
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let carol = user(&hub, "carol", UserKind::Human, None).await;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    let conv = Conn::new(&hub, &alice).group(&[&bob, &carol, &bot]).await;
    World {
        hub,
        alice,
        bob,
        carol,
        bot,
        conv,
    }
}

fn ask(conv: &str, kind: &str, id: &str) -> Value {
    let mut v = json!({"op": "request_human", "conv_id": conv, "kind": kind, "title": "May I deploy?", "client_req_id": id});
    if kind == "choose" {
        v["options"] = json!(["blue", "green"]);
    }
    v
}

fn decide(id: &Value, decision: &str) -> Value {
    json!({"op": "decide", "request_id": id, "decision": decision})
}

async fn audits(hub: &Hub, action: &str) -> usize {
    hub.audit_log(10_000)
        .await
        .unwrap()
        .iter()
        .filter(|a| a["action"] == action)
        .count()
}

fn sides(c: &mut Conn, kind: &str) -> Vec<Value> {
    c.drain()
        .into_iter()
        .filter(|f| f["type"] == "side" && f["event"]["type"] == kind)
        .collect()
}

async fn every_kind_is_asked_and_decided(db: TestDb) {
    let w = world(open(single(&db.url)).await).await;
    let mut a = Conn::new(&w.hub, &w.alice);
    let mut b = Conn::new(&w.hub, &w.bob);
    let mut g = Conn::new(&w.hub, &w.bot);
    for (kind, decision, extra, state) in [
        ("approve", "approve", json!({}), "approved"),
        (
            "approve",
            "approve",
            json!({"text": "ok, after 6pm"}),
            "approved",
        ),
        (
            "approve",
            "reject",
            json!({"text": "not today"}),
            "rejected",
        ),
        (
            "answer",
            "answer",
            json!({"text": "use region eu-west"}),
            "answered",
        ),
        ("answer", "reject", json!({}), "rejected"),
        ("choose", "choose", json!({"choice": "green"}), "chosen"),
        ("choose", "reject", json!({}), "rejected"),
    ] {
        let id = format!("{kind}-{decision}-{state}-{}", extra.to_string().len());
        let mut req = ask(&w.conv, kind, &id);
        req["context"] = json!({"tool": "deploy", "args": {"env": "prod"}});
        let q = g.ok(req).await;
        assert_eq!(q["state"], "pending");
        assert_eq!(q["kind"], kind);
        assert_eq!(q["context"]["tool"], "deploy");
        let rid = q["request_id"].clone();
        // The owner (default assignee) is told; other members are not.
        let created = sides(&mut a, "request.created");
        assert_eq!(created.len(), 1, "{kind}");
        assert_eq!(
            created[0]["event"]["data"]["request"]["title"],
            "May I deploy?"
        );
        assert!(sides(&mut b, "request.created").is_empty());
        let mut d = decide(&rid, decision);
        for (k, v) in extra.as_object().unwrap() {
            d[k] = v.clone();
        }
        let r = a.ok(d).await;
        assert_eq!(r["state"], state);
        // The agent learns the outcome.
        let got = sides(&mut g, "request.decided");
        assert_eq!(got.len(), 1, "{kind} {decision}");
        assert_eq!(got[0]["event"]["data"]["state"], state);
        assert_eq!(got[0]["event"]["data"]["decided_by"], json!(w.alice.id));
        let v = g.ok(json!({"op": "get_request", "request_id": rid})).await;
        assert_eq!(v["state"], state);
        assert_eq!(v["decided_by"], json!(w.alice.id));
        assert_eq!(v["response"]["decision"], decision);
        if let Some(t) = extra.get("text") {
            assert_eq!(&v["response"]["text"], t);
        }
        if let Some(c) = extra.get("choice") {
            assert_eq!(&v["response"]["choice"], c);
        }
    }
    assert_eq!(audits(&w.hub, "request.created").await, 7);
    assert_eq!(audits(&w.hub, "request.decided").await, 7);
    // Nothing went to the main log.
    let log = a.pull(&w.conv, 0).await;
    assert!(!log.to_string().contains("May I deploy?"));
}

async fn refused_requests_leave_no_trace(db: TestDb) {
    let w = world(open(single(&db.url)).await).await;
    let g = Conn::new(&w.hub, &w.bot);
    let a = Conn::new(&w.hub, &w.alice);
    let stranger = user(&w.hub, "stranger", UserKind::Agent, Some(&w.alice)).await;
    let dave = user(&w.hub, "dave", UserKind::Human, None).await;
    let other = Conn::new(&w.hub, &w.alice).group(&[&w.bot]).await;
    let first = g.ok(ask(&w.conv, "approve", "same")).await;
    let base = |f: &dyn Fn(&mut Value)| {
        let mut v = ask(&w.conv, "approve", "x1");
        f(&mut v);
        v
    };
    let long_title = "t".repeat(4097);
    let big = json!({"blob": "x".repeat(33 * 1024)});
    let cases: Vec<(Value, &str)> = vec![
        (base(&|v| v["kind"] = json!("vote")), "bad_request"),
        (base(&|v| v["title"] = json!("")), "bad_request"),
        (base(&|v| v["title"] = json!(long_title)), "bad_request"),
        (base(&|v| v["title"] = json!("a\u{0}b")), "bad_request"),
        (base(&|v| v["options"] = json!(["a", "b"])), "bad_request"),
        (base(&|v| v["kind"] = json!("choose")), "bad_request"),
        (
            base(&|v| {
                v["kind"] = json!("choose");
                v["options"] = json!(["only"]);
            }),
            "bad_request",
        ),
        (
            base(&|v| {
                v["kind"] = json!("choose");
                v["options"] = json!((0..21).map(|i| i.to_string()).collect::<Vec<_>>());
            }),
            "bad_request",
        ),
        (
            base(&|v| {
                v["kind"] = json!("choose");
                v["options"] = json!(["a", "a"]);
            }),
            "bad_request",
        ),
        (
            base(&|v| {
                v["kind"] = json!("choose");
                v["options"] = json!(["a", ""]);
            }),
            "bad_request",
        ),
        (base(&|v| v["context"] = big.clone()), "bad_request"),
        (base(&|v| v["ttl_secs"] = json!(59)), "bad_request"),
        (base(&|v| v["ttl_secs"] = json!(2_592_001)), "bad_request"),
        (base(&|v| v["client_req_id"] = json!("")), "bad_request"),
        (
            base(&|v| v["client_req_id"] = json!("c".repeat(129))),
            "bad_request",
        ),
        (base(&|v| v["assignees"] = json!([])), "bad_request"),
        (
            base(&|v| v["assignees"] = json!((0..21).map(|_| "bob").collect::<Vec<_>>())),
            "bad_request",
        ),
        (base(&|v| v["assignees"] = json!(["nobody"])), "not_found"),
        (
            base(&|v| v["assignees"] = json!([stranger.id])),
            "bad_request",
        ),
        (base(&|v| v["assignees"] = json!([dave.id])), "bad_request"),
        (base(&|v| v["conv_id"] = json!("c_nope")), "not_found"),
        // The same client_req_id in another conversation.
        (
            json!({"op": "request_human", "conv_id": other, "kind": "approve", "title": "t", "client_req_id": "same"}),
            "conflict",
        ),
        (
            json!({"op": "request_human", "conv_id": w.conv}),
            "bad_request",
        ),
    ];
    for (req, want) in &cases {
        let r = g.call(req.clone()).await;
        assert_eq!(code(&r), *want, "{req}: {r}");
    }
    // People do not ask; agents outside the conversation cannot.
    let r = a.call(ask(&w.conv, "approve", "p")).await;
    assert_eq!(code(&r), "forbidden", "{r}");
    let r = Conn::new(&w.hub, &stranger)
        .call(ask(&w.conv, "approve", "s"))
        .await;
    assert_eq!(code(&r), "forbidden", "{r}");
    // A retry returns the original.
    let again = g.ok(ask(&w.conv, "approve", "same")).await;
    assert_eq!(again["request_id"], first["request_id"]);
    assert_eq!(again["existing"], true);
    // Only the first one exists.
    let inbox = a.ok(json!({"op": "inbox"})).await;
    assert_eq!(inbox["items"].as_array().unwrap().len(), 1);
    assert_eq!(audits(&w.hub, "request.created").await, 1);
    // Valid boundaries.
    let mut ok = ask(&w.conv, "choose", "edge");
    ok["options"] = json!((0..20).map(|i| "o".repeat(200 - i)).collect::<Vec<_>>());
    ok["title"] = json!("t".repeat(4096));
    ok["ttl_secs"] = json!(60);
    ok["assignees"] = json!(["bob", w.carol.id, "bob"]);
    let q = g.ok(ok).await;
    let v = g
        .ok(json!({"op": "get_request", "request_id": q["request_id"]}))
        .await;
    assert_eq!(v["assignees"].as_array().unwrap().len(), 2, "deduplicated");
    let mut ok = ask(&w.conv, "approve", &"k".repeat(128));
    ok["ttl_secs"] = json!(2_592_000);
    g.ok(ok).await;
}

async fn only_the_right_people_decide_once(db: TestDb) {
    let w = world(open(single(&db.url)).await).await;
    let (a, b, c, g) = (
        Conn::new(&w.hub, &w.alice),
        Conn::new(&w.hub, &w.bob),
        Conn::new(&w.hub, &w.carol),
        Conn::new(&w.hub, &w.bot),
    );
    let mut req = ask(&w.conv, "approve", "r1");
    req["assignees"] = json!(["bob"]);
    let q = g.ok(req).await;
    let id = q["request_id"].clone();
    // Not assigned, the agent itself, unknown ids.
    for (who, want) in [(&c, "forbidden"), (&g, "forbidden")] {
        assert_eq!(code(&who.call(decide(&id, "approve")).await), want);
    }
    assert_eq!(
        code(&a.call(decide(&json!("q_nope"), "approve")).await),
        "not_found"
    );
    // Decisions that do not fit the kind.
    for d in ["answer", "choose", "maybe"] {
        assert_eq!(code(&b.call(decide(&id, d)).await), "bad_request", "{d}");
    }
    let mut long = decide(&id, "approve");
    long["text"] = json!("x".repeat(4097));
    assert_eq!(code(&b.call(long).await), "bad_request");
    // Nothing was decided by the refusals.
    let v = g.ok(json!({"op": "get_request", "request_id": id})).await;
    assert_eq!(v["state"], "pending");
    assert_eq!(audits(&w.hub, "request.decided").await, 0);
    // The assignee decides; nobody after that.
    b.ok(decide(&id, "approve")).await;
    for who in [&a, &b] {
        assert_eq!(code(&who.call(decide(&id, "reject")).await), "conflict");
    }
    assert_eq!(
        code(
            &g.call(json!({"op": "cancel_request", "request_id": id}))
                .await
        ),
        "conflict"
    );
    // The owner always decides, even when others are named.
    let mut req = ask(&w.conv, "approve", "r2");
    req["assignees"] = json!(["carol"]);
    let id2 = g.ok(req).await["request_id"].clone();
    a.ok(decide(&id2, "reject")).await;
    // answer and choose need their value.
    let id3 = g.ok(ask(&w.conv, "answer", "r3")).await["request_id"].clone();
    assert_eq!(code(&a.call(decide(&id3, "answer")).await), "bad_request");
    let mut d = decide(&id3, "answer");
    d["text"] = json!("");
    assert_eq!(code(&a.call(d).await), "bad_request");
    let id4 = g.ok(ask(&w.conv, "choose", "r4")).await["request_id"].clone();
    let mut d = decide(&id4, "choose");
    d["choice"] = json!("red");
    assert_eq!(code(&a.call(d).await), "bad_request");
    // Cancel: only the agent, only while pending.
    let id5 = g.ok(ask(&w.conv, "approve", "r5")).await["request_id"].clone();
    let cancel = json!({"op": "cancel_request", "request_id": id5});
    assert_eq!(code(&a.call(cancel.clone()).await), "forbidden");
    let r = g.ok(cancel.clone()).await;
    assert_eq!(r["state"], "cancelled");
    assert_eq!(code(&g.call(cancel).await), "conflict");
    assert_eq!(code(&a.call(decide(&id5, "approve")).await), "conflict");
    assert_eq!(
        code(
            &g.call(json!({"op": "cancel_request", "request_id": "q_nope"}))
                .await
        ),
        "not_found"
    );
    assert_eq!(audits(&w.hub, "request.cancelled").await, 1);
}

async fn requests_are_seen_by_their_parties_and_can_be_awaited(db: TestDb) {
    let w = world(open(single(&db.url)).await).await;
    let (a, b, c, g) = (
        Conn::new(&w.hub, &w.alice),
        Conn::new(&w.hub, &w.bob),
        Conn::new(&w.hub, &w.carol),
        Conn::new(&w.hub, &w.bot),
    );
    let mut req = ask(&w.conv, "approve", "s1");
    req["assignees"] = json!(["bob"]);
    let id = g.ok(req).await["request_id"].clone();
    let get = |wait: Value| json!({"op": "get_request", "request_id": id, "wait_secs": wait});
    for who in [&a, &b, &g] {
        assert_eq!(who.ok(get(json!(0))).await["state"], "pending");
    }
    assert_eq!(code(&c.call(get(json!(0))).await), "forbidden");
    assert_eq!(
        code(
            &a.call(json!({"op": "get_request", "request_id": "q_nope"}))
                .await
        ),
        "not_found"
    );
    for bad in [json!(-1), json!(26)] {
        assert_eq!(
            code(&g.call(get(bad.clone())).await),
            "bad_request",
            "{bad}"
        );
    }
    // A wait ends as soon as somebody decides.
    let (hub, bob, id2) = (w.hub.clone(), w.bob.clone(), id.clone());
    let t = std::time::Instant::now();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        Conn::new(&hub, &bob).ok(decide(&id2, "approve")).await;
    });
    let v = g.ok(get(json!(10))).await;
    assert_eq!(v["state"], "approved");
    assert!(t.elapsed() < Duration::from_secs(5), "{:?}", t.elapsed());
    // A wait without a decision returns pending after the time.
    let id3 = g.ok(ask(&w.conv, "approve", "s3")).await["request_id"].clone();
    let t = std::time::Instant::now();
    let v = g
        .ok(json!({"op": "get_request", "request_id": id3, "wait_secs": 1}))
        .await;
    assert_eq!(v["state"], "pending");
    assert!(t.elapsed() >= Duration::from_millis(900));
}

async fn the_inbox_lists_what_waits_for_me(db: TestDb) {
    let w = world(open(single(&db.url)).await).await;
    let (a, b, c, g) = (
        Conn::new(&w.hub, &w.alice),
        Conn::new(&w.hub, &w.bob),
        Conn::new(&w.hub, &w.carol),
        Conn::new(&w.hub, &w.bot),
    );
    let items = |v: Value| -> Vec<Value> { v["items"].as_array().unwrap().clone() };
    assert!(items(a.ok(json!({"op": "inbox"})).await).is_empty());
    let q1 = g.ok(ask(&w.conv, "approve", "i1")).await["request_id"].clone();
    let mut req = ask(&w.conv, "answer", "i2");
    req["assignees"] = json!(["bob"]);
    let q2 = g.ok(req).await["request_id"].clone();
    // A draft of the owner's agent is in the same inbox.
    a.ok(json!({"op": "set_agent_policy", "conv_id": w.conv, "agent_id": w.bot.id, "approval": true}))
        .await;
    let draft = g.send(&w.conv, "draft text").await["draft_id"].clone();
    let mine = items(a.ok(json!({"op": "inbox"})).await);
    let kinds: Vec<(String, Value)> = mine
        .iter()
        .map(|i| {
            (
                i["type"].as_str().unwrap().to_string(),
                i.get("request_id").or(i.get("draft_id")).cloned().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        kinds,
        vec![
            ("request".to_string(), q1.clone()),
            ("request".to_string(), q2.clone()),
            ("draft".to_string(), draft.clone()),
        ],
        "oldest first; the owner also sees what its agent asked others"
    );
    let bobs = items(b.ok(json!({"op": "inbox"})).await);
    assert_eq!(bobs.len(), 1);
    assert_eq!(bobs[0]["request_id"], q2);
    assert!(items(c.ok(json!({"op": "inbox"})).await).is_empty());
    // Decided items leave the inbox.
    let mut d = decide(&q2, "answer");
    d["text"] = json!("42");
    b.ok(d).await;
    a.ok(decide(&q1, "approve")).await;
    a.ok(json!({"op": "approve", "draft_id": draft})).await;
    assert!(items(a.ok(json!({"op": "inbox"})).await).is_empty());
    assert!(items(b.ok(json!({"op": "inbox"})).await).is_empty());
}

/// Moves a request's expiry into the past, as time would.
async fn age(db: &TestDb, request_id: &Value) {
    sqlx::any::install_default_drivers();
    let d = agentim_core::storage::Dialect::from_url(&db.url).unwrap();
    let mut c = sqlx::AnyConnection::connect(&db.url.replace("mariadb://", "mysql://"))
        .await
        .unwrap();
    sqlx::query(d.q("UPDATE requests SET expires_at = 1 WHERE request_id = ?"))
        .bind(request_id.as_str().unwrap().to_string())
        .execute(&mut c)
        .await
        .unwrap();
}

async fn requests_expire(db: TestDb) {
    let hub = hub_with(&db, |h| h.expire_every = Duration::from_millis(100)).await;
    let w = world(hub).await;
    let (a, mut g) = (Conn::new(&w.hub, &w.alice), Conn::new(&w.hub, &w.bot));
    let id = g.ok(ask(&w.conv, "approve", "e1")).await["request_id"].clone();
    let keep = g.ok(ask(&w.conv, "approve", "e2")).await["request_id"].clone();
    age(&db, &id).await;
    // Past its time it cannot be decided, even before the timer runs...
    assert_eq!(code(&a.call(decide(&id, "approve")).await), "conflict");
    // ...and leaves the inbox.
    let inbox = a.ok(json!({"op": "inbox"})).await;
    assert_eq!(inbox["items"].as_array().unwrap().len(), 1);
    // The timer marks it and tells the agent.
    let mut state = Value::Null;
    for _ in 0..100 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        w.hub.tick().await;
        state = g.ok(json!({"op": "get_request", "request_id": id})).await["state"].clone();
        if state == "expired" {
            break;
        }
    }
    assert_eq!(state, "expired");
    assert_eq!(sides(&mut g, "request.expired").len(), 1);
    assert_eq!(audits(&w.hub, "request.expired").await, 1);
    // The other one is untouched.
    assert_eq!(
        g.ok(json!({"op": "get_request", "request_id": keep})).await["state"],
        "pending"
    );
}

async fn removal_cancels_and_limits_hold(db: TestDb) {
    let w = world(open(single(&db.url)).await).await;
    let (a, mut g) = (Conn::new(&w.hub, &w.alice), Conn::new(&w.hub, &w.bot));
    let max = agentim_core::logic::MAX_PENDING_REQUESTS;
    for i in 0..max {
        g.ok(ask(&w.conv, "approve", &format!("l{i}"))).await;
    }
    let r = g.call(ask(&w.conv, "approve", "over")).await;
    assert_eq!(code(&r), "rate_limited", "{r}");
    // A retry of an existing one is still answered.
    assert_eq!(g.ok(ask(&w.conv, "approve", "l0")).await["existing"], true);
    // Removing the agent cancels what it waits for.
    g.drain();
    a.ok(json!({"op": "remove_member", "conv_id": w.conv, "user_id": w.bot.id}))
        .await;
    let inbox = a.ok(json!({"op": "inbox"})).await;
    assert!(inbox["items"].as_array().unwrap().is_empty());
    assert_eq!(audits(&w.hub, "request.cancelled").await as i64, max);
    // Once nothing is pending, it may ask again elsewhere.
    let conv2 = a.group(&[&w.bot]).await;
    g.ok(ask(&conv2, "approve", "after")).await;
}

async fn requests_survive_a_restart(db: TestDb) {
    let (alice, conv, id);
    {
        let w = world(open(single(&db.url)).await).await;
        alice = w.alice.clone();
        conv = w.conv.clone();
        let mut req = ask(&w.conv, "choose", "restart");
        req["context"] = json!({"k": "v"});
        id = Conn::new(&w.hub, &w.bot).ok(req).await["request_id"].clone();
    }
    let hub = open(single(&db.url)).await;
    let a = Conn::new(&hub, &alice);
    let inbox = a.ok(json!({"op": "inbox"})).await;
    assert_eq!(inbox["items"][0]["request_id"], id);
    assert_eq!(inbox["items"][0]["context"]["k"], "v");
    assert_eq!(inbox["items"][0]["conv_id"], json!(conv));
    let mut d = decide(&id, "choose");
    d["choice"] = json!("blue");
    assert_eq!(a.ok(d).await["state"], "chosen");
}

async fn approved_drafts_carry_the_approval(db: TestDb) {
    let w = world(open(single(&db.url)).await).await;
    let (a, g) = (Conn::new(&w.hub, &w.alice), Conn::new(&w.hub, &w.bot));
    a.ok(json!({"op": "set_agent_policy", "conv_id": w.conv, "agent_id": w.bot.id, "approval": true}))
        .await;
    let d1 = g.send(&w.conv, "as written").await["draft_id"].clone();
    let d2 = g.send(&w.conv, "to be edited").await["draft_id"].clone();
    a.ok(json!({"op": "approve", "draft_id": d1})).await;
    a.ok(json!({"op": "approve", "draft_id": d2, "text": "edited by alice"}))
        .await;
    // An empty edit is refused before anything is decided.
    let d3 = g.send(&w.conv, "third").await["draft_id"].clone();
    let r = a
        .call(json!({"op": "approve", "draft_id": d3, "text": ""}))
        .await;
    assert_eq!(code(&r), "bad_request");
    a.ok(json!({"op": "approve", "draft_id": d3, "text": "third"}))
        .await;
    let log = a.pull(&w.conv, 0).await;
    let created: Vec<&Value> = log["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| &e["event"])
        .filter(|e| e["type"] == "msg.created" && e["msg"]["sender_id"] == json!(w.bot.id))
        .collect();
    assert_eq!(created.len(), 3);
    for (e, text, edited) in [
        (created[0], "as written", false),
        (created[1], "edited by alice", true),
        (created[2], "third", false),
    ] {
        assert_eq!(e["msg"]["content"], text);
        assert_eq!(
            e["msg"]["sender_type"], "agent",
            "still the agent's message"
        );
        assert_eq!(e["data"]["approval"]["by"], json!(w.alice.id));
        assert_eq!(e["data"]["approval"]["edited"], edited, "{text}");
    }
    // D97: the audit records whether it was edited and a hash of the
    // draft, never the draft's text.
    let audit = w.hub.audit_log(50).await.unwrap();
    let approved: Vec<&Value> = audit
        .iter()
        .filter(|r| r["action"] == "draft.approved")
        .collect();
    assert_eq!(approved.len(), 3);
    let all = serde_json::to_string(&audit).unwrap();
    for t in ["as written", "to be edited", "third"] {
        assert!(!all.contains(t), "draft text in the audit: {t}");
    }
    let edits: Vec<bool> = approved
        .iter()
        .rev()
        .map(|r| r["detail"]["edited"].as_bool().unwrap())
        .collect();
    assert_eq!(edits, [false, true, false]);
    for r in &approved {
        assert_eq!(r["detail"]["draft_sha256"].as_str().unwrap().len(), 64);
    }
}

/// The owner and every assignee decide the same request at once, also
/// against the agent cancelling: exactly one wins, the others get
/// `conflict`, and exactly one outcome is told.
async fn race_decisions_have_one_winner(db: TestDb) {
    let w = world(open(single(&db.url)).await).await;
    let mut g = Conn::new(&w.hub, &w.bot);
    for round in 0..15 {
        let mut req = ask(&w.conv, "approve", &format!("race{round}"));
        req["assignees"] = json!(["bob", "carol"]);
        let id = g.ok(req).await["request_id"].clone();
        g.drain();
        let mut tasks = vec![];
        for (i, who) in [&w.alice, &w.bob, &w.carol, &w.bot].into_iter().enumerate() {
            let (hub, who, id) = (w.hub.clone(), who.clone(), id.clone());
            tasks.push(tokio::spawn(async move {
                let c = Conn::new(&hub, &who);
                let req = if who.kind == UserKind::Agent {
                    json!({"op": "cancel_request", "request_id": id})
                } else {
                    decide(&id, if i % 2 == 0 { "approve" } else { "reject" })
                };
                c.call(req).await
            }));
        }
        let mut winners = vec![];
        for t in tasks {
            let r = t.await.unwrap();
            if r["ok"] == true {
                winners.push(r["state"].clone());
            } else {
                assert_eq!(code(&r), "conflict", "round {round}: {r}");
            }
        }
        assert_eq!(winners.len(), 1, "round {round}");
        let v = g.ok(json!({"op": "get_request", "request_id": id})).await;
        assert_eq!(v["state"], winners[0], "round {round}");
        let told: Vec<Value> = g
            .drain()
            .into_iter()
            .filter(|f| {
                f["type"] == "side"
                    && (f["event"]["type"] == "request.decided"
                        || f["event"]["type"] == "request.cancelled")
            })
            .collect();
        assert_eq!(told.len(), 1, "round {round}: {told:?}");
    }
}

/// A unit's conversation lives in its own database: its requests are found,
/// listed and decided there, and move with it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn requests_live_with_their_conversation_across_databases() {
    let main = new_db(Backend::Sqlite).await.unwrap();
    let secret = new_db(Backend::Sqlite).await.unwrap();
    let (carol, bot, conv, q1, q2);
    {
        let hub = open(two(&main.url, &secret.url, "main")).await;
        carol = user_in(&hub, "carol", "b").await;
        bot = user(&hub, "bot", UserKind::Agent, Some(&carol)).await;
        conv = Conn::new(&hub, &carol).group(&[&bot]).await;
        let g = Conn::new(&hub, &bot);
        q1 = g.ok(ask(&conv, "approve", "m1")).await["request_id"].clone();
        q2 = g.ok(ask(&conv, "approve", "m2")).await["request_id"].clone();
        Conn::new(&hub, &carol).ok(decide(&q1, "approve")).await;
    }
    // The configuration moves unit b to its own database.
    let hub = open(two(&main.url, &secret.url, "secret")).await;
    let r = hub.migrate_unit("b").await.unwrap();
    assert_eq!(r["moved"], 1, "{r}");
    let c = Conn::new(&hub, &carol);
    let inbox = c.ok(json!({"op": "inbox"})).await;
    assert_eq!(inbox["items"].as_array().unwrap().len(), 1);
    assert_eq!(inbox["items"][0]["request_id"], q2);
    assert_eq!(
        c.ok(json!({"op": "get_request", "request_id": q1})).await["state"],
        "approved"
    );
    c.ok(decide(&q2, "reject")).await;
    let g = Conn::new(&hub, &bot);
    assert_eq!(
        g.ok(json!({"op": "get_request", "request_id": q2})).await["state"],
        "rejected"
    );
    // A new request in the moved conversation is written to its new home.
    g.ok(ask(&conv, "approve", "m3")).await;
    assert_eq!(
        c.ok(json!({"op": "inbox"})).await["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}
