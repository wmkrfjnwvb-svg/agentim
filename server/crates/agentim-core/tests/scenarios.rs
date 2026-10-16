//! Business scenarios, one area per test, run against every backend
//! (docs/05-operations.md §5.2). Each test covers the allowed path, the
//! refused paths (who may not do it, and with which error), and the state
//! afterwards: the main log, the side channel, pushes and what survives a
//! restart.

mod common;

use std::time::Duration;

use agentim_core::logic::HubConfig;
use agentim_core::model::*;
use common::*;
use serde_json::{Value, json};

backend_tests!(
    conversation_creation_is_validated,
    only_members_can_read_or_write_a_conversation,
    only_the_owner_adds_members_and_members_may_add_their_own_agents,
    late_agents_do_not_see_history_from_before_they_joined,
    message_size_is_limited_in_bytes_and_content_round_trips_exactly,
    retries_are_deduplicated_per_sender_and_conversation,
    replies_must_point_into_the_same_conversation,
    mentions_match_whole_names,
    on_behalf_of_is_only_for_an_agent_speaking_for_its_owner,
    ephemeral_messages_reach_only_their_audience,
    edits_are_versioned_and_restricted,
    recall_removes_the_content_from_history,
    recall_is_restricted_and_not_repeatable,
    pull_pages_through_the_log,
    read_cursors_drive_unread_counts,
    every_session_of_a_user_receives_pushes,
    stream_permissions_and_chunk_rules,
    streams_can_be_interrupted_by_the_right_people,
    stream_size_limit_is_enforced,
    overlong_streams_are_closed_by_the_watchdog,
    recalling_a_live_stream_cancels_it,
    late_focus_gets_the_stream_so_far,
    approval_drafts_are_private_until_decided,
    whisper_is_only_between_an_owner_and_its_agent,
    takeover_rules,
    agent_policy_is_owner_only,
    humans_are_not_rate_limited_but_agents_are,
    malformed_requests_are_rejected_without_side_effects,
    state_survives_a_restart,
);

fn code(r: &Value) -> &str {
    r["error"]["code"].as_str().unwrap_or("")
}

fn events(p: &Value) -> &Vec<Value> {
    p["events"].as_array().unwrap()
}

fn head(p: &Value) -> i64 {
    seqs(p).last().copied().unwrap_or(0)
}

fn pushed_events(frames: &[Value]) -> Vec<&Value> {
    frames.iter().filter(|f| f["type"] == "event").collect()
}

async fn conv_info(c: &Conn, conv: &str) -> Value {
    let r = c.ok(json!({"op": "list_convs"})).await;
    r["convs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["conv_id"] == conv)
        .cloned()
        .unwrap_or(Value::Null)
}

fn member<'a>(info: &'a Value, user: &User) -> &'a Value {
    info["members"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["user_id"] == user.id.as_str())
        .unwrap_or(&Value::Null)
}

async fn side_count(c: &Conn, conv: &str) -> usize {
    c.ok(json!({"op": "pull_side", "conv_id": conv})).await["side_events"]
        .as_array()
        .unwrap()
        .len()
}

async fn draft_count(c: &Conn) -> usize {
    c.ok(json!({"op": "list_drafts"})).await["drafts"]
        .as_array()
        .unwrap()
        .len()
}

async fn send_id(c: &Conn, conv: &str, text: &str) -> String {
    let r = c.send(conv, text).await;
    assert_eq!(r["ok"], true, "{r}");
    r["msg_id"].as_str().unwrap().to_string()
}

// ----------------------------------------------------------- membership

async fn conversation_creation_is_validated(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let carol = user(&hub, "carol", UserKind::Human, None).await;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    let a = Conn::new(&hub, &alice);
    let create = |kind: &str, title: &str, members: Vec<&str>| json!({"op": "create_conv", "kind": kind, "title": title, "members": members});
    assert_eq!(
        code(&a.call(create("room", "t", vec![&bob.id])).await),
        "bad_request"
    );
    assert_eq!(
        code(&a.call(create("group", "", vec![&bob.id])).await),
        "bad_request"
    );
    let long = "t".repeat(256);
    assert_eq!(
        code(&a.call(create("group", &long, vec![&bob.id])).await),
        "bad_request"
    );
    a.ok(create("group", &"t".repeat(255), vec![&bob.id])).await;
    assert_eq!(
        code(&a.call(create("group", "t", vec!["nobody"])).await),
        "not_found"
    );
    // An agent starts conversations only with known contacts (tests/agents.rs).
    let g = Conn::new(&hub, &bot);
    assert_eq!(
        code(&g.call(create("group", "t", vec![&bob.id])).await),
        "forbidden"
    );
    // A direct conversation has exactly one other member.
    assert_eq!(
        code(&a.call(create("dm", "t", vec![])).await),
        "bad_request"
    );
    assert_eq!(
        code(&a.call(create("dm", "t", vec![&bob.id, &carol.id])).await),
        "bad_request"
    );
    assert_eq!(
        code(&a.call(create("dm", "t", vec![&alice.id])).await),
        "bad_request"
    );
    a.ok(create("dm", "t", vec![&bob.id])).await;
    // Duplicates and the creator in the list are tolerated, not doubled.
    let conv = a
        .ok(create(
            "group",
            "dups",
            vec![&bob.id, &bob.id, &alice.id, "carol"],
        ))
        .await["conv_id"]
        .as_str()
        .unwrap()
        .to_string();
    let info = conv_info(&a, &conv).await;
    assert_eq!(info["members"].as_array().unwrap().len(), 3, "{info}");
    assert_eq!(member(&info, &alice)["role"], "owner");
    assert_eq!(member(&info, &bob)["role"], "member");
    // Nothing half-created is left behind by refused requests.
    let all = a.ok(json!({"op": "list_convs"})).await;
    assert_eq!(all["convs"].as_array().unwrap().len(), 3, "{all}");
}

async fn only_members_can_read_or_write_a_conversation(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let carol = user(&hub, "carol", UserKind::Human, None).await;
    let cbot = user(&hub, "cbot", UserKind::Agent, Some(&carol)).await;
    let a = Conn::new(&hub, &alice);
    let c = Conn::new(&hub, &carol);
    let cb = Conn::new(&hub, &cbot);
    let conv = a.group(&[&bob]).await;
    let m = send_id(&a, &conv, "hello").await;
    let before = a.pull(&conv, 0).await;

    let refused = [
        c.send(&conv, "let me in").await,
        c.call(json!({"op": "pull", "conv_id": conv})).await,
        c.call(json!({"op": "pull_side", "conv_id": conv})).await,
        c.call(json!({"op": "read", "conv_id": conv, "seq": 1})).await,
        c.call(json!({"op": "focus", "conv_id": conv})).await,
        c.call(json!({"op": "edit", "msg_id": m, "text": "x"})).await,
        c.call(json!({"op": "recall", "msg_id": m})).await,
        c.call(json!({"op": "add_member", "conv_id": conv, "user_id": carol.id}))
            .await,
        c.call(json!({"op": "whisper", "conv_id": conv, "agent_id": cbot.id, "text": "x"}))
            .await,
        c.call(json!({"op": "takeover", "conv_id": conv, "agent_id": cbot.id}))
            .await,
        c.call(json!({"op": "set_agent_policy", "conv_id": conv, "agent_id": cbot.id, "trigger": "all"}))
            .await,
        // Resetting a guard writes into the conversation: members only.
        c.call(json!({"op": "reset_guard", "conv_id": conv, "agent_id": cbot.id}))
            .await,
        cb.call(json!({"op": "stream_start", "conv_id": conv, "client_msg_id": "s"}))
            .await,
        cb.send(&conv, "agent let me in").await,
    ];
    for (i, r) in refused.iter().enumerate() {
        assert_eq!(r["ok"], false, "request {i} should be refused: {r}");
        assert_eq!(code(r), "forbidden", "request {i}: {r}");
    }
    assert_eq!(code(&c.send("c_unknown", "x").await), "not_found");
    assert_eq!(
        code(&c.call(json!({"op": "pull", "conv_id": "c_unknown"})).await),
        "not_found"
    );
    // Nothing reached the log.
    assert_eq!(seqs(&a.pull(&conv, 0).await), seqs(&before));
    assert!(
        conv_info(&c, &conv).await.is_null(),
        "outsiders do not see the conversation"
    );
}

async fn only_the_owner_adds_members_and_members_may_add_their_own_agents(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let carol = user(&hub, "carol", UserKind::Human, None).await;
    let bobbot = user(&hub, "bobbot", UserKind::Agent, Some(&bob)).await;
    let a = Conn::new(&hub, &alice);
    let mut b = Conn::new(&hub, &bob);
    let conv = a.group(&[&bob]).await;
    send_id(&a, &conv, "before carol").await;
    let add = |u: &str| json!({"op": "add_member", "conv_id": conv, "user_id": u});
    assert_eq!(code(&b.call(add(&carol.id)).await), "forbidden");
    b.ok(add(&bobbot.id)).await;
    b.drain();
    let r = a.ok(add(&carol.id)).await;
    let joined = pushed_events(&b.drain())
        .into_iter()
        .find(|e| e["event"]["type"] == "member.joined")
        .cloned()
        .expect("members are told about the new member");
    assert_eq!(joined["event"]["seq"], r["seq"]);
    assert_eq!(joined["event"]["data"]["user_id"], carol.id.as_str());
    assert_eq!(code(&a.call(add(&carol.id)).await), "conflict");
    assert_eq!(code(&a.call(add("nobody")).await), "not_found");
    // A new human member can read the history and write.
    let c = Conn::new(&hub, &carol);
    let p = c.pull(&conv, 0).await;
    assert!(
        events(&p)
            .iter()
            .any(|e| e["event"]["msg"]["content"] == "before carol")
    );
    send_id(&c, &conv, "hi all").await;
    assert!(
        pushed_events(&b.drain())
            .iter()
            .any(|e| e["event"]["msg"]["content"] == "hi all")
    );
    let info = conv_info(&a, &conv).await;
    assert_eq!(
        member(&info, &bobbot)["trigger"],
        "addressed",
        "group agents default to addressed mode"
    );
}

async fn late_agents_do_not_see_history_from_before_they_joined(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    let a = Conn::new(&hub, &alice);
    let conv = a.group(&[&bob]).await;
    send_id(&a, &conv, "@bot private plans from before").await;
    a.ok(json!({"op": "add_member", "conv_id": conv, "user_id": bot.id}))
        .await;
    a.ok(json!({"op": "set_agent_policy", "conv_id": conv, "agent_id": bot.id, "trigger": "all"}))
        .await;
    send_id(&a, &conv, "welcome bot").await;
    let g = Conn::new(&hub, &bot);
    let p = g.pull(&conv, 0).await;
    let texts: Vec<&str> = events(&p)
        .iter()
        .filter_map(|e| e["event"]["msg"]["content"].as_str())
        .collect();
    assert_eq!(texts, vec!["welcome bot"], "{p}");
    // The owner observing through the agent sees the agent's view, not more.
    let o = a
        .ok(json!({"op": "pull", "conv_id": conv, "after_seq": 0, "as_agent": bot.id}))
        .await;
    assert_eq!(seqs(&o), seqs(&p));
}

// ----------------------------------------------------------------- send

async fn message_size_is_limited_in_bytes_and_content_round_trips_exactly(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let a = Conn::new(&hub, &alice);
    let conv = a.group(&[&bob]).await;
    assert_eq!(code(&a.send(&conv, "").await), "bad_request");
    send_id(&a, &conv, &"a".repeat(64 * 1024)).await;
    assert_eq!(
        code(&a.send(&conv, &"a".repeat(64 * 1024 + 1)).await),
        "bad_request"
    );
    send_id(&a, &conv, &"中".repeat(21845)).await; // 65535 bytes
    assert_eq!(
        code(&a.send(&conv, &"中".repeat(21846)).await),
        "bad_request"
    );
    let tricky = "引号\"'\\ 换行\n制表\t emoji 👩‍👩‍👧 <script>alert(1)</script> ' OR 1=1; -- %s {{x}}";
    let id = send_id(&a, &conv, tricky).await;
    // A NUL byte cannot be stored by every database: refuse it cleanly.
    let nul = a.send(&conv, "a\u{0}b").await;
    assert!(
        nul["ok"] == true || code(&nul) == "bad_request",
        "NUL must be accepted or refused, not fail in storage: {nul}"
    );
    send_id(&a, &conv, "still working").await;
    assert!(hub.storage_failed().is_none());
    let p = a.pull(&conv, 0).await;
    assert_eq!(msg_by_id(&p, &id).unwrap()["content"], tricky);
    drop(a);
    drop(hub);
    let hub = open(single(&db.url)).await;
    let a = Conn::new(&hub, &alice);
    assert_eq!(
        msg_by_id(&a.pull(&conv, 0).await, &id).unwrap()["content"],
        tricky
    );
}

async fn retries_are_deduplicated_per_sender_and_conversation(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let a = Conn::new(&hub, &alice);
    let b = Conn::new(&hub, &bob);
    let conv = a.group(&[&bob]).await;
    let other = a.group(&[&bob]).await;
    let send = |conv: &str, id: &str, text: &str| json!({"op": "send", "conv_id": conv, "client_msg_id": id, "text": text});
    let first = a.ok(send(&conv, "x", "original")).await;
    let retry = a.ok(send(&conv, "x", "changed on retry")).await;
    assert_eq!(retry["duplicate"], true);
    assert_eq!(retry["msg_id"], first["msg_id"]);
    assert_eq!(retry["seq"], first["seq"]);
    // Another sender, or another conversation, is a different message.
    let bobs = b.ok(send(&conv, "x", "bob's own x")).await;
    assert_ne!(bobs["msg_id"], first["msg_id"]);
    let elsewhere = a.ok(send(&other, "x", "other conv")).await;
    assert_ne!(elsewhere["msg_id"], first["msg_id"]);
    let p = a.pull(&conv, 0).await;
    let created: Vec<&Value> = events(&p)
        .iter()
        .filter(|e| e["event"]["type"] == "msg.created")
        .collect();
    assert_eq!(created.len(), 2);
    assert_eq!(
        msg_by_id(&p, first["msg_id"].as_str().unwrap()).unwrap()["content"],
        "original"
    );
    // Deduplication survives a restart.
    drop((a, b));
    drop(hub);
    let hub = open(single(&db.url)).await;
    let a = Conn::new(&hub, &alice);
    assert_eq!(
        a.ok(send(&conv, "x", "after restart")).await["duplicate"],
        true
    );
}

async fn replies_must_point_into_the_same_conversation(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let a = Conn::new(&hub, &alice);
    let conv = a.group(&[&bob]).await;
    let other = a.group(&[&bob]).await;
    let there = send_id(&a, &other, "elsewhere").await;
    let here = send_id(&a, &conv, "question").await;
    assert_eq!(code(&a.reply(&conv, "x", "m_unknown").await), "bad_request");
    assert_eq!(code(&a.reply(&conv, "x", &there).await), "bad_request");
    let r = a.reply(&conv, "answer", &here).await;
    let id = r["msg_id"].as_str().unwrap();
    let m = msg_by_id(&a.pull(&conv, 0).await, id).unwrap();
    assert_eq!(m["reply_to"], here.as_str());
    assert_eq!(m["causation_id"], here.as_str());
}

async fn mentions_match_whole_names(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let bobby = user(&hub, "bobby", UserKind::Human, None).await;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    let bot2 = user(&hub, "bot2", UserKind::Agent, Some(&alice)).await;
    let a = Conn::new(&hub, &alice);
    let conv = a.group(&[&bob, &bobby, &bot, &bot2]).await;
    let mentions = |p: &Value, id: &str| -> Vec<String> {
        let mut v: Vec<String> = msg_by_id(p, id).unwrap()["mentions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x.as_str().unwrap().to_string())
            .collect();
        v.sort();
        v
    };
    let m1 = send_id(&a, &conv, "@bob hi").await;
    let m2 = send_id(&a, &conv, "@bobby hi").await;
    let m3 = send_id(&a, &conv, "ping @bot2, and mail bob@example.com").await;
    let m4 = send_id(&a, &conv, "@bob and @bobby").await;
    let p = a.pull(&conv, 0).await;
    assert_eq!(mentions(&p, &m1), vec![bob.id.clone()]);
    assert_eq!(mentions(&p, &m2), vec![bobby.id.clone()]);
    assert_eq!(mentions(&p, &m3), vec![bot2.id.clone()]);
    let mut both = vec![bob.id.clone(), bobby.id.clone()];
    both.sort();
    assert_eq!(mentions(&p, &m4), both);
    // A mention-mode agent is not woken by a longer name that starts like its own.
    let g = Conn::new(&hub, &bot);
    let seen = g.pull(&conv, 0).await;
    assert!(
        !events(&seen)
            .iter()
            .any(|e| e["event"]["msg"]["msg_id"] == m3.as_str()),
        "@bot2 must not reach bot"
    );
}

async fn on_behalf_of_is_only_for_an_agent_speaking_for_its_owner(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    let a = Conn::new(&hub, &alice);
    let g = Conn::new(&hub, &bot);
    let conv = a.group(&[&bob, &bot]).await;
    let q = send_id(&a, &conv, "@bot reply for me").await;
    let send = |who: &str, id: &str| json!({"op": "send", "conv_id": conv, "client_msg_id": id, "text": "on behalf", "on_behalf_of": who, "reply_to": q});
    assert_eq!(code(&a.call(send(&alice.id, "h1")).await), "forbidden");
    assert_eq!(code(&g.call(send(&bob.id, "g1")).await), "forbidden");
    let r = g.ok(send(&alice.id, "g2")).await;
    let m = msg_by_id(&a.pull(&conv, 0).await, r["msg_id"].as_str().unwrap()).unwrap();
    assert_eq!(m["on_behalf_of"], alice.id.as_str());
    assert_eq!(m["sender_id"], bot.id.as_str());
    assert_eq!(
        m["sender_type"], "agent",
        "the agent stays visible as the author"
    );
}

async fn ephemeral_messages_reach_only_their_audience(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let carol = user(&hub, "carol", UserKind::Human, None).await;
    let dave = user(&hub, "dave", UserKind::Human, None).await;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    let mut a = Conn::new(&hub, &alice);
    let mut b = Conn::new(&hub, &bob);
    let mut c = Conn::new(&hub, &carol);
    let g = Conn::new(&hub, &bot);
    let conv = a.group(&[&bob, &carol, &bot]).await;
    let before = head(&a.pull(&conv, 0).await);
    a.drain();
    b.drain();
    c.drain();
    let eph = |to: &str| json!({"op": "send", "conv_id": conv, "client_msg_id": uuid::Uuid::new_v4().to_string(), "text": "just for you", "to_user": to});
    let r = g.ok(eph(&bob.id)).await;
    assert!(r["side_seq"].is_number() && r["seq"].is_null(), "{r}");
    let saw = |frames: Vec<Value>| {
        frames
            .iter()
            .any(|f| f["type"] == "side" && f["event"]["data"]["text"] == "just for you")
    };
    assert!(saw(b.drain()), "the target sees it");
    assert!(saw(a.drain()), "the agent's owner sees it");
    assert!(!saw(c.drain()), "other members do not");
    assert_eq!(side_count(&b, &conv).await, 1);
    assert_eq!(side_count(&c, &conv).await, 0);
    assert_eq!(code(&g.call(eph(&dave.id)).await), "forbidden");
    assert_eq!(code(&g.call(eph("nobody")).await), "not_found");
    // Humans can send ephemeral messages too.
    a.ok(eph(&carol.id)).await;
    assert_eq!(side_count(&c, &conv).await, 1);
    assert_eq!(side_count(&b, &conv).await, 1);
    assert_eq!(
        head(&a.pull(&conv, 0).await),
        before,
        "the main log is untouched"
    );
}

// ------------------------------------------------------- edit and recall

async fn edits_are_versioned_and_restricted(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    let a = Conn::new(&hub, &alice);
    let b = Conn::new(&hub, &bob);
    let g = Conn::new(&hub, &bot);
    let conv = a.group(&[&bob, &bot]).await;
    let m = send_id(&b, &conv, "v1").await;
    let edit = |id: &str, text: &str| json!({"op": "edit", "msg_id": id, "text": text});
    assert_eq!(
        code(&a.call(edit(&m, "hijack")).await),
        "forbidden",
        "the conversation owner cannot edit others"
    );
    assert_eq!(code(&b.call(edit(&m, "")).await), "bad_request");
    assert_eq!(code(&b.call(edit("m_unknown", "x")).await), "not_found");
    let r = b.ok(edit(&m, "v2")).await;
    let p = a.pull(&conv, 0).await;
    let latest = msg_by_id(&p, &m).unwrap();
    assert_eq!(
        (latest["content"].as_str(), latest["version"].as_i64()),
        (Some("v2"), Some(2))
    );
    assert_eq!(events(&p).last().unwrap()["event"]["seq"], r["seq"]);
    assert_eq!(events(&p).last().unwrap()["event"]["type"], "msg.updated");
    // An owner may revise what its agent said.
    let q = send_id(&a, &conv, "@bot hi").await;
    let gm = g.reply(&conv, "draft answer", &q).await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    a.ok(edit(&gm, "revised answer")).await;
    assert_eq!(
        msg_by_id(&a.pull(&conv, 0).await, &gm).unwrap()["content"],
        "revised answer"
    );
    // Recalled messages cannot be edited back.
    b.ok(json!({"op": "recall", "msg_id": m})).await;
    assert_eq!(code(&b.call(edit(&m, "resurrect")).await), "conflict");
}

async fn recall_removes_the_content_from_history(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let a = Conn::new(&hub, &alice);
    let b = Conn::new(&hub, &bob);
    let conv = a.group(&[&bob]).await;
    let m = send_id(&b, &conv, "secret-111").await;
    b.ok(json!({"op": "edit", "msg_id": m, "text": "secret-222"}))
        .await;
    send_id(&b, &conv, "unrelated").await;
    b.ok(json!({"op": "recall", "msg_id": m})).await;
    let p = a.pull(&conv, 0).await;
    assert!(
        !p.to_string().contains("secret-"),
        "recalled content must not be in the history: {p}"
    );
    let latest = msg_by_id(&p, &m).unwrap();
    assert_eq!(latest["state"], "recalled");
    assert!(
        events(&p)
            .iter()
            .any(|e| e["event"]["msg"]["content"] == "unrelated")
    );
    drop((a, b));
    drop(hub);
    let hub = open(single(&db.url)).await;
    let a = Conn::new(&hub, &alice);
    assert!(!a.pull(&conv, 0).await.to_string().contains("secret-"));
    if let Some(path) = &db.path {
        // Not in the database file either: secure_delete overwrites freed
        // space, and once the WAL is checkpointed no old frame remains.
        use sqlx::Connection;
        let mut c = sqlx::AnyConnection::connect(&db.url).await.unwrap();
        sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
            .execute(&mut c)
            .await
            .unwrap();
        drop(c);
        let bytes = std::fs::read(path).unwrap();
        let wal = std::fs::read(path.with_extension("db-wal")).unwrap_or_default();
        let hit = |b: &[u8]| {
            b.windows(10)
                .any(|w| w == b"secret-111" || w == b"secret-222")
        };
        assert!(
            !hit(&bytes) && !hit(&wal),
            "recalled content left in the SQLite file"
        );
    }
}

async fn recall_is_restricted_and_not_repeatable(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let carol = user(&hub, "carol", UserKind::Human, None).await;
    let a = Conn::new(&hub, &alice);
    let b = Conn::new(&hub, &bob);
    let c = Conn::new(&hub, &carol);
    let conv = a.group(&[&bob, &carol]).await;
    let recall = |id: &str| json!({"op": "recall", "msg_id": id});
    let cm = send_id(&c, &conv, "carol's").await;
    assert_eq!(
        code(&b.call(recall(&cm)).await),
        "forbidden",
        "members cannot recall others"
    );
    a.ok(recall(&cm)).await; // the conversation owner can (moderation)
    let n = head(&a.pull(&conv, 0).await);
    assert_eq!(
        code(&c.call(recall(&cm)).await),
        "conflict",
        "already recalled"
    );
    assert_eq!(
        head(&a.pull(&conv, 0).await),
        n,
        "a repeated recall adds nothing to the log"
    );
    assert_eq!(code(&a.call(recall("m_unknown")).await), "not_found");
    // Recalled messages do not count as unread.
    send_id(&b, &conv, "one").await;
    let bm = send_id(&b, &conv, "two").await;
    assert_eq!(conv_info(&c, &conv).await["unread"], 2);
    b.ok(recall(&bm)).await;
    assert_eq!(conv_info(&c, &conv).await["unread"], 1);
}

// -------------------------------------------------------------- syncing

async fn pull_pages_through_the_log(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let a = Conn::new(&hub, &alice);
    let conv = a.group(&[&bob]).await;
    for i in 0..25 {
        send_id(&a, &conv, &format!("m{i}")).await;
    }
    let total = head(&a.pull(&conv, 0).await);
    let page = |after: i64, limit: i64| json!({"op": "pull", "conv_id": conv, "after_seq": after, "limit": limit});
    let mut after = 0;
    let mut all = vec![];
    loop {
        let p = a.ok(page(after, 10)).await;
        let s = seqs(&p);
        assert!(s.len() <= 10);
        all.extend(s);
        after = p["upto_seq"].as_i64().unwrap();
        if p["has_more"] != true {
            break;
        }
    }
    assert_eq!(all, (1..=total).collect::<Vec<_>>());
    let past = a.ok(page(total + 100, 10)).await;
    assert!(events(&past).is_empty());
    assert_eq!(past["has_more"], false);
    assert_eq!(
        seqs(&a.ok(page(0, 0)).await).len(),
        1,
        "limit is clamped to at least 1"
    );
    assert_eq!(seqs(&a.ok(page(0, -5)).await).len(), 1);
    assert_eq!(seqs(&a.ok(page(-10, 3)).await), vec![1, 2, 3]);
}

async fn read_cursors_drive_unread_counts(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let a = Conn::new(&hub, &alice);
    let b = Conn::new(&hub, &bob);
    let conv = a.group(&[&bob]).await;
    for t in ["1", "2", "3"] {
        send_id(&b, &conv, t).await;
    }
    assert_eq!(conv_info(&a, &conv).await["unread"], 3);
    let last = head(&a.pull(&conv, 0).await);
    let read = |seq: i64| json!({"op": "read", "conv_id": conv, "seq": seq});
    a.ok(read(last)).await;
    assert_eq!(conv_info(&a, &conv).await["unread"], 0);
    a.ok(read(1)).await;
    assert_eq!(
        conv_info(&a, &conv).await["unread"],
        0,
        "the cursor never moves back"
    );
    send_id(&a, &conv, "mine").await;
    assert_eq!(
        conv_info(&a, &conv).await["unread"],
        0,
        "own messages are not unread"
    );
    send_id(&b, &conv, "4").await;
    assert_eq!(conv_info(&a, &conv).await["unread"], 1);
    // A cursor past the end must not hide future messages.
    let now = head(&a.pull(&conv, 0).await);
    a.ok(read(now + 1000)).await;
    send_id(&b, &conv, "5").await;
    assert_eq!(
        conv_info(&a, &conv).await["unread"],
        1,
        "read beyond the head is clamped"
    );
    // Cursors are per user and survive a restart; bob wrote the last
    // message, which counts as reading up to it (D93).
    assert_eq!(conv_info(&b, &conv).await["unread"], 0);
    drop((a, b));
    drop(hub);
    let hub = open(single(&db.url)).await;
    assert_eq!(
        conv_info(&Conn::new(&hub, &alice), &conv).await["unread"],
        1
    );
}

async fn every_session_of_a_user_receives_pushes(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let a = Conn::new(&hub, &alice);
    let mut phone = Conn::new(&hub, &bob);
    let mut laptop = Conn::new(&hub, &bob);
    let conv = a.group(&[&bob]).await;
    phone.drain();
    laptop.drain();
    send_id(&a, &conv, "to both devices").await;
    let got = |f: Vec<Value>| {
        f.iter()
            .any(|e| e["event"]["msg"]["content"] == "to both devices")
    };
    assert!(got(phone.drain()) && got(laptop.drain()));
    // A device that goes away catches up by pulling from its last seq.
    let last_seen = head(&laptop.pull(&conv, 0).await);
    drop(laptop);
    send_id(&a, &conv, "while offline").await;
    assert!(
        phone
            .drain()
            .iter()
            .any(|e| e["event"]["msg"]["content"] == "while offline")
    );
    let laptop = Conn::new(&hub, &bob);
    let p = laptop.pull(&conv, last_seen).await;
    assert_eq!(events(&p).len(), 1);
    assert_eq!(events(&p)[0]["event"]["msg"]["content"], "while offline");
    // The sender's own other session sees its message too.
    let mut a2 = Conn::new(&hub, &alice);
    send_id(&a, &conv, "from desktop").await;
    assert!(
        a2.drain()
            .iter()
            .any(|e| e["event"]["msg"]["content"] == "from desktop")
    );
}

// ------------------------------------------------------------ streaming

fn start(conv: &str, id: &str, reply_to: &str) -> Value {
    json!({"op": "stream_start", "conv_id": conv, "client_msg_id": id, "reply_to": reply_to})
}

fn append(msg: &str, idx: u64, text: &str) -> Value {
    json!({"op": "stream_append", "msg_id": msg, "idx": idx, "text": text})
}

async fn stream_permissions_and_chunk_rules(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    let other = user(&hub, "other", UserKind::Agent, Some(&bob)).await;
    let a = Conn::new(&hub, &alice);
    let g = Conn::new(&hub, &bot);
    let o = Conn::new(&hub, &other);
    let conv = a.group(&[&bob, &bot, &other]).await;
    let q = send_id(&a, &conv, "@bot @other go").await;
    assert_eq!(
        code(&a.call(start(&conv, "h", &q)).await),
        "forbidden",
        "humans do not stream"
    );
    let s1 = g.ok(start(&conv, "s1", &q)).await;
    let id = s1["msg_id"].as_str().unwrap().to_string();
    assert_eq!(
        g.ok(start(&conv, "s1", &q)).await["msg_id"],
        id.as_str(),
        "start is idempotent"
    );
    g.ok(start(&conv, "s2", &q)).await;
    assert_eq!(
        code(&g.call(start(&conv, "s3", &q)).await),
        "rate_limited",
        "two streams per agent and conversation"
    );
    g.ok(append(&id, 0, "a")).await;
    assert_eq!(g.ok(append(&id, 0, "a")).await["duplicate"], true);
    assert_eq!(code(&g.call(append(&id, 5, "z")).await), "out_of_order");
    g.ok(append(&id, 1, "b")).await;
    assert_eq!(code(&o.call(append(&id, 2, "x")).await), "forbidden");
    assert_eq!(
        code(&a.call(json!({"op": "stream_end", "msg_id": id})).await),
        "forbidden"
    );
    assert_eq!(
        code(&o.call(json!({"op": "stream_end", "msg_id": id})).await),
        "forbidden"
    );
    g.ok(json!({"op": "stream_end", "msg_id": id})).await;
    assert_eq!(code(&g.call(append(&id, 2, "late")).await), "not_found");
    assert_eq!(
        code(&g.call(json!({"op": "stream_end", "msg_id": id})).await),
        "not_found"
    );
    let m = msg_by_id(&a.pull(&conv, 0).await, &id).unwrap();
    assert_eq!(
        (m["state"].as_str(), m["content"].as_str()),
        (Some("complete"), Some("ab"))
    );
}

async fn streams_can_be_interrupted_by_the_right_people(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let carol = user(&hub, "carol", UserKind::Human, None).await;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    let other = user(&hub, "other", UserKind::Agent, Some(&bob)).await;
    let a = Conn::new(&hub, &alice);
    let b = Conn::new(&hub, &bob);
    let c = Conn::new(&hub, &carol);
    let mut g = Conn::new(&hub, &bot);
    let o = Conn::new(&hub, &other);
    let conv = a.group(&[&bob, &bot, &other]).await;
    let q = send_id(&a, &conv, "@bot talk").await;
    let cancel = |id: &str| json!({"op": "stream_cancel", "msg_id": id});
    let s = g.ok(start(&conv, "s1", &q)).await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    g.ok(append(&s, 0, "partial")).await;
    assert_eq!(code(&c.call(cancel(&s)).await), "forbidden", "not a member");
    assert_eq!(
        code(&o.call(cancel(&s)).await),
        "forbidden",
        "another agent"
    );
    g.drain();
    b.ok(cancel(&s)).await;
    let m = msg_by_id(&a.pull(&conv, 0).await, &s).unwrap();
    assert_eq!(m["state"], "cancelled");
    assert_eq!(m["content"], "partial", "what was said stays");
    assert_eq!(m["state_reason"], "cancelled_by:bob");
    assert!(
        g.drain()
            .iter()
            .any(|f| f["type"] == "stream_cancelled" && f["msg_id"] == s.as_str())
    );
    let s2 = g.ok(start(&conv, "s2", &q)).await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    a.ok(cancel(&s2)).await;
    let s3 = g.ok(start(&conv, "s3", &q)).await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    g.drain();
    g.ok(cancel(&s3)).await;
    assert!(
        !g.drain().iter().any(|f| f["type"] == "stream_cancelled"),
        "no notice for your own cancel"
    );
    assert_eq!(code(&b.call(cancel(&s3)).await), "not_found");
}

async fn stream_size_limit_is_enforced(db: TestDb) {
    let hub = agentim_core::hub::Hub::open(
        single(&db.url),
        HubConfig {
            stream_max_bytes: 1000,
            ..hub_cfg()
        },
    )
    .await
    .unwrap();
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    let a = Conn::new(&hub, &alice);
    let g = Conn::new(&hub, &bot);
    let conv = a.group(&[&bot]).await;
    let q = send_id(&a, &conv, "@bot write a lot").await;
    let s = g.ok(start(&conv, "s", &q)).await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    g.ok(append(&s, 0, &"x".repeat(600))).await;
    let r = g.call(append(&s, 1, &"y".repeat(600))).await;
    assert_eq!(code(&r), "size_limit");
    let m = msg_by_id(&a.pull(&conv, 0).await, &s).unwrap();
    assert_eq!(m["state"], "complete");
    assert_eq!(m["state_reason"], "size_limit");
    assert!(
        m["content"].as_str().unwrap().len() <= 1000,
        "a stored stream never exceeds the limit ({} bytes)",
        m["content"].as_str().unwrap().len()
    );
}

async fn overlong_streams_are_closed_by_the_watchdog(db: TestDb) {
    let hub = agentim_core::hub::Hub::open(
        single(&db.url),
        HubConfig {
            stream_idle: Duration::from_secs(30),
            stream_max: Duration::from_millis(300),
            ..hub_cfg()
        },
    )
    .await
    .unwrap();
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    let a = Conn::new(&hub, &alice);
    let g = Conn::new(&hub, &bot);
    let conv = a.group(&[&bot]).await;
    let q = send_id(&a, &conv, "@bot ramble").await;
    let s = g.ok(start(&conv, "s", &q)).await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    for i in 0..5 {
        g.ok(append(&s, i, "word ")).await;
        tokio::time::sleep(Duration::from_millis(80)).await;
    }
    hub.tick().await;
    let m = msg_by_id(&a.pull(&conv, 0).await, &s).unwrap();
    assert_eq!(
        (m["state"].as_str(), m["state_reason"].as_str()),
        (Some("complete"), Some("duration_limit"))
    );
    assert_eq!(m["content"], "word word word word word ");
    assert_eq!(code(&g.call(append(&s, 5, "more")).await), "not_found");
}

async fn recalling_a_live_stream_cancels_it(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    let a = Conn::new(&hub, &alice);
    let g = Conn::new(&hub, &bot);
    let conv = a.group(&[&bot]).await;
    let q = send_id(&a, &conv, "@bot speak").await;
    let s = g.ok(start(&conv, "s", &q)).await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    g.ok(append(&s, 0, "oops-secret")).await;
    a.ok(json!({"op": "recall", "msg_id": s})).await; // the owner pulls it back
    let p = a.pull(&conv, 0).await;
    assert_eq!(msg_by_id(&p, &s).unwrap()["state"], "recalled");
    assert!(!p.to_string().contains("oops-secret"), "{p}");
    assert_eq!(code(&g.call(append(&s, 1, "more")).await), "not_found");
}

async fn late_focus_gets_the_stream_so_far(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    let a = Conn::new(&hub, &alice);
    let mut b = Conn::new(&hub, &bob);
    let g = Conn::new(&hub, &bot);
    let conv = a.group(&[&bob, &bot]).await;
    let q = send_id(&a, &conv, "@bot story").await;
    let s = g.ok(start(&conv, "s", &q)).await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    g.ok(append(&s, 0, "once upon ")).await;
    g.ok(append(&s, 1, "a time")).await;
    b.drain();
    b.ok(json!({"op": "focus", "conv_id": conv})).await;
    // Apply deltas as a client does: `reset` replaces, otherwise append.
    let mut text = String::new();
    let mut apply = |frames: Vec<Value>| {
        for f in frames
            .iter()
            .filter(|f| f["type"] == "delta" && f["msg_id"] == s.as_str())
        {
            if f["reset"] == true {
                text.clear();
            }
            text.push_str(f["text"].as_str().unwrap());
        }
    };
    apply(b.drain());
    g.ok(append(&s, 2, "!")).await;
    hub.tick().await;
    apply(b.drain());
    assert_eq!(
        text, "once upon a time!",
        "a late viewer sees the text exactly once"
    );
    b.ok(json!({"op": "focus", "conv_id": null})).await;
    g.ok(append(&s, 3, "?")).await;
    hub.tick().await;
    assert!(
        !b.drain().iter().any(|f| f["type"] == "delta"),
        "no deltas after unfocus"
    );
}

// ------------------------------------------------------------- approval

async fn approval_drafts_are_private_until_decided(db: TestDb) {
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
    let q = send_id(&b, &conv, "@bot refund me").await;
    let before = head(&a.pull(&conv, 0).await);
    a.drain();
    b.drain();
    let d = g.reply(&conv, "refund of $20 approved", &q).await;
    assert_eq!(d["state"], "pending_approval", "{d}");
    let draft = d["draft_id"].as_str().unwrap().to_string();
    assert!(
        !b.drain()
            .iter()
            .any(|f| f.to_string().contains("refund of")),
        "bob sees nothing yet"
    );
    assert!(
        a.drain()
            .iter()
            .any(|f| f["type"] == "side" && f.to_string().contains(&draft)),
        "the owner is told"
    );
    assert_eq!(draft_count(&a).await, 1);
    assert_eq!(draft_count(&b).await, 0);
    let side = b.ok(json!({"op": "pull_side", "conv_id": conv})).await;
    assert!(!side.to_string().contains("refund of"), "{side}");
    assert_eq!(head(&a.pull(&conv, 0).await), before, "drafts take no seq");
    let approve =
        |id: &str, text: Option<&str>| json!({"op": "approve", "draft_id": id, "text": text});
    assert_eq!(code(&b.call(approve(&draft, None)).await), "forbidden");
    assert_eq!(
        code(&b.call(json!({"op": "reject", "draft_id": draft})).await),
        "forbidden"
    );
    let r = a.ok(approve(&draft, Some("refund of $15 approved"))).await;
    let p = a.pull(&conv, 0).await;
    let m = &events(&p).last().unwrap()["event"]["msg"];
    assert_eq!(
        p["events"].as_array().unwrap().last().unwrap()["event"]["seq"],
        r["seq"]
    );
    assert_eq!(
        (
            m["content"].as_str(),
            m["sender_id"].as_str(),
            m["sender_type"].as_str()
        ),
        (
            Some("refund of $15 approved"),
            Some(bot.id.as_str()),
            Some("agent")
        )
    );
    assert!(
        b.drain()
            .iter()
            .any(|f| f["event"]["msg"]["content"] == "refund of $15 approved")
    );
    assert_eq!(code(&a.call(approve(&draft, None)).await), "conflict");
    assert_eq!(
        code(&a.call(json!({"op": "reject", "draft_id": draft})).await),
        "conflict"
    );
    assert_eq!(draft_count(&a).await, 0);
    // Rejected drafts never reach the log, and stay rejected.
    let d2 = g.reply(&conv, "second try", &q).await["draft_id"]
        .as_str()
        .unwrap()
        .to_string();
    a.ok(json!({"op": "reject", "draft_id": d2})).await;
    assert_eq!(code(&a.call(approve(&d2, None)).await), "conflict");
    assert!(!a.pull(&conv, 0).await.to_string().contains("second try"));
    assert_eq!(code(&a.call(approve("d_unknown", None)).await), "not_found");
}

// --------------------------------------------------------- intervention

async fn whisper_is_only_between_an_owner_and_its_agent(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    let a = Conn::new(&hub, &alice);
    let mut b = Conn::new(&hub, &bob);
    let mut g = Conn::new(&hub, &bot);
    let conv = a.group(&[&bob, &bot]).await;
    let before = head(&a.pull(&conv, 0).await);
    b.drain();
    g.drain();
    let whisper =
        |text: &str| json!({"op": "whisper", "conv_id": conv, "agent_id": bot.id, "text": text});
    assert_eq!(code(&b.call(whisper("do as I say")).await), "forbidden");
    assert_eq!(code(&a.call(whisper("")).await), "bad_request");
    a.ok(whisper("offer 10% max")).await;
    assert!(
        g.drain()
            .iter()
            .any(|f| f["type"] == "side" && f["event"]["data"]["text"] == "offer 10% max")
    );
    assert!(b.drain().is_empty(), "other members get nothing");
    assert!(
        !b.ok(json!({"op": "pull_side", "conv_id": conv}))
            .await
            .to_string()
            .contains("10%")
    );
    assert_eq!(head(&a.pull(&conv, 0).await), before);
}

async fn takeover_rules(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    let a = Conn::new(&hub, &alice);
    let b = Conn::new(&hub, &bob);
    let g = Conn::new(&hub, &bot);
    let conv = a.group(&[&bob, &bot]).await;
    let q = send_id(&b, &conv, "@bot help").await;
    let s = g.ok(start(&conv, "s", &q)).await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    g.ok(append(&s, 0, "let me")).await;
    let take = |on: bool| json!({"op": if on { "takeover" } else { "release" }, "conv_id": conv, "agent_id": bot.id});
    assert_eq!(code(&b.call(take(true)).await), "forbidden");
    assert_eq!(
        code(&a.call(take(false)).await),
        "conflict",
        "not taken over yet"
    );
    a.ok(take(true)).await;
    let m = msg_by_id(&a.pull(&conv, 0).await, &s).unwrap();
    assert_eq!(
        (m["state"].as_str(), m["state_reason"].as_str()),
        (Some("cancelled"), Some("taken_over"))
    );
    assert_eq!(code(&a.call(take(true)).await), "conflict");
    assert_eq!(code(&g.reply(&conv, "me again", &q).await), "taken_over");
    assert_eq!(code(&g.call(start(&conv, "s2", &q)).await), "taken_over");
    send_id(&a, &conv, "a human here").await;
    // The takeover survives a restart.
    drop((a, b, g));
    drop(hub);
    let hub = open(single(&db.url)).await;
    let a = Conn::new(&hub, &alice);
    let g = Conn::new(&hub, &bot);
    assert_eq!(
        code(&g.reply(&conv, "after restart", &q).await),
        "taken_over"
    );
    a.ok(take(false)).await;
    assert_eq!(code(&a.call(take(false)).await), "conflict");
    assert_eq!(g.reply(&conv, "back", &q).await["ok"], true);
}

async fn agent_policy_is_owner_only(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    let a = Conn::new(&hub, &alice);
    let b = Conn::new(&hub, &bob);
    let conv = a.group(&[&bob, &bot]).await;
    let policy =
        json!({"op": "set_agent_policy", "conv_id": conv, "agent_id": bot.id, "trigger": "all"});
    assert_eq!(code(&b.call(policy.clone()).await), "forbidden");
    send_id(&b, &conv, "not addressed").await;
    let g = Conn::new(&hub, &bot);
    assert!(!g.pull(&conv, 0).await.to_string().contains("not addressed"));
    a.ok(policy).await;
    send_id(&b, &conv, "now heard").await;
    assert!(g.pull(&conv, 0).await.to_string().contains("now heard"));
    drop((a, b, g));
    drop(hub);
    let hub = open(single(&db.url)).await;
    let info = conv_info(&Conn::new(&hub, &alice), &conv).await;
    assert_eq!(member(&info, &bot)["trigger"], "all");
}

async fn humans_are_not_rate_limited_but_agents_are(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    let a = Conn::new(&hub, &alice);
    let g = Conn::new(&hub, &bot);
    let conv = a.group(&[&bot]).await;
    let mut prompts = vec![];
    for i in 0..30 {
        prompts.push(send_id(&a, &conv, &format!("@bot question {i}")).await);
    }
    let mut limited = 0;
    for (i, q) in prompts.iter().enumerate() {
        let r = g.reply(&conv, &format!("answer number {i}"), q).await;
        if code(&r) == "rate_limited" {
            limited += 1;
        } else {
            assert_eq!(r["ok"], true, "{r}");
        }
    }
    assert!(limited > 0, "a burst of agent messages is limited");
    // Being rate limited is not a trip: the agent keeps working afterwards.
    tokio::time::sleep(Duration::from_millis(1100)).await;
    assert_eq!(
        g.reply(&conv, "after a pause", &prompts[0]).await["ok"],
        true
    );
}

// ------------------------------------------------------------- protocol

async fn malformed_requests_are_rejected_without_side_effects(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let a = Conn::new(&hub, &alice);
    let conv = a.group(&[&bob]).await;
    let before = head(&a.pull(&conv, 0).await);
    let raw = |t: &str| {
        let t = t.to_string();
        let s = &a.s;
        async move { s.handle_text(&t).await }
    };
    let r = raw("not json").await;
    assert_eq!((r["ok"].as_bool(), code(&r)), (Some(false), "bad_request"));
    let r = raw(r#"{"op":"nope","rid":7}"#).await;
    assert_eq!((code(&r), r["rid"].as_i64()), ("unknown_op", Some(7)));
    let r = raw(r#"{"op":"send","rid":8}"#).await;
    assert_eq!((code(&r), r["rid"].as_i64()), ("bad_request", Some(8)));
    let bad_type =
        json!({"op": "send", "rid": 9, "conv_id": conv, "client_msg_id": "x", "text": 5})
            .to_string();
    assert_eq!(code(&raw(&bad_type).await), "bad_request");
    let r = raw(r#"{"op":"pull","conv_id":"x","after_seq":"zero"}"#).await;
    assert_eq!(code(&r), "bad_request");
    assert_eq!(head(&a.pull(&conv, 0).await), before);
    assert!(hub.storage_failed().is_none());
}

// --------------------------------------------------------------- restart

async fn state_survives_a_restart(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    let a = Conn::new(&hub, &alice);
    let b = Conn::new(&hub, &bob);
    let g = Conn::new(&hub, &bot);
    let conv = a.group(&[&bob, &bot]).await;
    a.ok(json!({"op": "set_agent_policy", "conv_id": conv, "agent_id": bot.id, "trigger": "all", "approval": true}))
        .await;
    let q = send_id(&b, &conv, "question").await;
    let draft = g.reply(&conv, "pending answer", &q).await["draft_id"]
        .as_str()
        .unwrap()
        .to_string();
    a.ok(json!({"op": "whisper", "conv_id": conv, "agent_id": bot.id, "text": "keep it short"}))
        .await;
    send_id(&b, &conv, "second").await;
    b.ok(json!({"op": "read", "conv_id": conv, "seq": 1})).await;
    let log = a.pull(&conv, 0).await;
    let unread_bob = conv_info(&b, &conv).await["unread"].clone();
    drop((a, b, g));
    drop(hub);

    let hub = open(single(&db.url)).await;
    let a = Conn::new(&hub, &alice);
    let b = Conn::new(&hub, &bob);
    let g = Conn::new(&hub, &bot);
    assert_eq!(
        a.pull(&conv, 0).await["events"],
        log["events"],
        "identical log"
    );
    let info = conv_info(&a, &conv).await;
    assert_eq!(
        (
            member(&info, &bot)["trigger"].as_str(),
            member(&info, &bot)["approval"].as_bool()
        ),
        (Some("all"), Some(true))
    );
    assert_eq!(conv_info(&b, &conv).await["unread"], unread_bob);
    let drafts = a.ok(json!({"op": "list_drafts"})).await;
    assert_eq!(drafts["drafts"][0]["draft_id"], draft.as_str());
    assert!(
        g.ok(json!({"op": "pull_side", "conv_id": conv}))
            .await
            .to_string()
            .contains("keep it short")
    );
    a.ok(json!({"op": "approve", "draft_id": draft})).await;
    assert!(
        b.pull(&conv, 0)
            .await
            .to_string()
            .contains("pending answer")
    );
    // New writes continue the same sequence.
    let r = b.send(&conv, "after restart").await;
    assert_eq!(r["seq"].as_i64().unwrap(), head(&log) + 2);
}
