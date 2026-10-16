//! Problems other IM servers had, checked against ours
//! (docs/research/im-issues.md): each test reproduces one of them and
//! fails without its fix.

mod common;

use agentim_core::model::*;
use common::*;
use serde_json::{Value, json};

backend_tests!(
    deactivating_an_owner_stops_their_agents,
    deactivated_people_cannot_be_added,
    an_own_agent_sees_no_more_than_its_adder,
    a_persons_agents_leave_with_them,
    unread_counts_start_at_what_a_member_can_see,
    edits_of_older_messages_stay_hidden_from_new_members,
    a_recalled_message_leaves_no_draft_copy,
);

fn code(v: &Value) -> &str {
    v["error"]["code"].as_str().unwrap_or("")
}

async fn last_seq(c: &Conn, conv: &str) -> i64 {
    let p = c.pull(conv, 0).await;
    seqs(&p).last().copied().unwrap_or(0)
}

/// H1: the agents' keys are in the deactivated person's hands.
async fn deactivating_an_owner_stops_their_agents(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    let other = user(&hub, "other", UserKind::Agent, Some(&bob)).await;
    let a = Conn::new(&hub, &alice);
    let conv = a.group(&[&bob, &bot]).await;
    let g = Conn::new(&hub, &bot);
    g.ok(json!({"op": "send", "conv_id": conv, "client_msg_id": "1", "text": "hi"}))
        .await;
    hub.set_deactivated(&alice.id, true).await.unwrap();
    assert!(hub.is_stopped(&bot.id), "the owner's agent is stopped");
    assert!(!hub.is_stopped(&other.id), "someone else's agent is not");
    let g = Conn::new(&hub, &bot);
    let r = g
        .call(json!({"op": "send", "conv_id": conv, "client_msg_id": "2", "text": "still here"}))
        .await;
    assert_eq!(code(&r), "conflict", "{r}");
    // Deactivating again, and reactivating, leave it stopped: the owner
    // decides when it runs again.
    hub.set_deactivated(&alice.id, true).await.unwrap();
    hub.set_deactivated(&alice.id, false).await.unwrap();
    assert!(hub.is_stopped(&bot.id));
    // It survives a restart.
    drop((a, g));
    let hub = open(single(&db.url)).await;
    assert!(hub.is_stopped(&bot.id));
}

async fn deactivated_people_cannot_be_added(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let carol = user(&hub, "carol", UserKind::Human, None).await;
    let a = Conn::new(&hub, &alice);
    let conv = a.group(&[&bob]).await;
    hub.set_deactivated(&carol.id, true).await.unwrap();
    let head = last_seq(&a, &conv).await;
    let r = a
        .call(json!({"op": "add_member", "conv_id": conv, "user_id": carol.id}))
        .await;
    assert_eq!(code(&r), "not_found", "{r}");
    assert_eq!(last_seq(&a, &conv).await, head, "a refusal leaves no trace");
    hub.set_deactivated(&carol.id, false).await.unwrap();
    a.ok(json!({"op": "add_member", "conv_id": conv, "user_id": carol.id}))
        .await;
}

/// H6: a plain member who sees nothing from before they joined cannot read
/// it through an agent they add with history.
async fn an_own_agent_sees_no_more_than_its_adder(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let carol = user(&hub, "carol", UserKind::Human, None).await;
    let bobbot = user(&hub, "bobbot", UserKind::Agent, Some(&bob)).await;
    let alicebot = user(&hub, "alicebot", UserKind::Agent, Some(&alice)).await;
    let a = Conn::new(&hub, &alice);
    let conv = a.group(&[&carol]).await;
    for i in 0..5 {
        a.send(&conv, &format!("before bob {i}")).await;
    }
    a.ok(json!({"op": "add_member", "conv_id": conv, "user_id": bob.id, "history": 0}))
        .await;
    let joined = last_seq(&a, &conv).await;
    let b = Conn::new(&hub, &bob);
    b.ok(json!({"op": "add_member", "conv_id": conv, "user_id": bobbot.id, "history": 200}))
        .await;
    let g = Conn::new(&hub, &bobbot);
    let h = g.ok(json!({"op": "history", "conv_id": conv})).await;
    let early: Vec<&Value> = h["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["event"]["seq"].as_i64().unwrap() < joined)
        .collect();
    assert!(early.is_empty(), "{early:?}");
    // The owner (a manager) can still share history with their agent.
    a.ok(json!({"op": "add_member", "conv_id": conv, "user_id": alicebot.id, "history": 200}))
        .await;
    let h = Conn::new(&hub, &alicebot)
        .ok(json!({"op": "history", "conv_id": conv}))
        .await;
    assert!(
        h["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["event"]["seq"].as_i64().unwrap() < joined)
    );
}

/// M3: a new member's unread and mention counts start where they can see.
async fn unread_counts_start_at_what_a_member_can_see(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let carol = user(&hub, "carol", UserKind::Human, None).await;
    let dave = user(&hub, "dave", UserKind::Human, None).await;
    let a = Conn::new(&hub, &alice);
    let conv = a.group(&[&bob]).await;
    for i in 0..5 {
        a.send(&conv, &format!("old {i}")).await;
    }
    a.ok(json!({"op": "add_member", "conv_id": conv, "user_id": carol.id, "history": 0}))
        .await;
    a.ok(json!({"op": "add_member", "conv_id": conv, "user_id": dave.id, "history": 2}))
        .await;
    let c = Conn::new(&hub, &carol);
    let u = c.ok(json!({"op": "unread"})).await;
    assert_eq!(
        (u["total"].clone(), u["mentions"].clone()),
        (json!(0), json!(0)),
        "{u}"
    );
    let d = Conn::new(&hub, &dave);
    let u = d.ok(json!({"op": "unread"})).await;
    assert!(u["total"].as_i64().unwrap() <= 2, "{u}");
    // New messages count as usual.
    a.send(&conv, "new").await;
    assert_eq!(c.ok(json!({"op": "unread"})).await["total"], 1);
    // Removed, mentioned while away, and added back without history.
    a.ok(json!({"op": "remove_member", "conv_id": conv, "user_id": carol.id}))
        .await;
    a.send(&conv, "@carol while you were away").await;
    a.ok(json!({"op": "add_member", "conv_id": conv, "user_id": carol.id, "history": 0}))
        .await;
    let u = c.ok(json!({"op": "unread"})).await;
    assert_eq!(
        (u["total"].clone(), u["mentions"].clone()),
        (json!(0), json!(0)),
        "{u}"
    );
}

/// H6: removed or gone, a person can no longer read or speak through their
/// agents in that conversation.
async fn a_persons_agents_leave_with_them(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let carol = user(&hub, "carol", UserKind::Human, None).await;
    let bobbot = user(&hub, "bobbot", UserKind::Agent, Some(&bob)).await;
    let carolbot = user(&hub, "carolbot", UserKind::Agent, Some(&carol)).await;
    let alicebot = user(&hub, "alicebot", UserKind::Agent, Some(&alice)).await;
    let a = Conn::new(&hub, &alice);
    let conv = a
        .group(&[&bob, &carol, &bobbot, &carolbot, &alicebot])
        .await;
    let r = a
        .ok(json!({"op": "remove_member", "conv_id": conv, "user_id": bob.id}))
        .await;
    assert_eq!(r["agents"].as_array().unwrap().len(), 1, "{r}");
    Conn::new(&hub, &carol)
        .ok(json!({"op": "leave", "conv_id": conv}))
        .await;
    let members: Vec<String> = a.ok(json!({"op": "list_convs"})).await["convs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["conv_id"] == conv)
        .unwrap()["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["user_id"].as_str().unwrap().to_string())
        .collect();
    assert!(
        !members.contains(&bobbot.id) && !members.contains(&carolbot.id),
        "{members:?}"
    );
    assert!(members.contains(&alicebot.id), "other people's agents stay");
    for bot in [&bobbot, &carolbot] {
        let r = Conn::new(&hub, bot)
            .call(json!({"op": "history", "conv_id": conv}))
            .await;
        assert_eq!(code(&r), "forbidden", "{r}");
    }
    // The log says why the agents left.
    let p = a.pull(&conv, 0).await;
    let reasons = p["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| {
            e["event"]["type"] == "member.removed" && e["event"]["data"]["reason"] == "owner_left"
        })
        .count();
    assert_eq!(reasons, 2);
}

/// H3: editing or recalling a message from before someone joined (without
/// history) tells them nothing: not live, not in sync, not in reads, and
/// not to an agent that hears everything.
async fn edits_of_older_messages_stay_hidden_from_new_members(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let (carol, token) = hub
        .create_user("carol", UserKind::Human, None, None)
        .await
        .unwrap();
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    let a = Conn::new(&hub, &alice);
    let conv = a.group(&[&bob]).await;
    let old = a.send(&conv, "secret plan").await["msg_id"].clone();
    let gone = a.send(&conv, "another secret").await["msg_id"].clone();
    for u in [&carol, &bot] {
        a.ok(json!({"op": "add_member", "conv_id": conv, "user_id": u.id, "history": 0}))
            .await;
    }
    a.ok(json!({"op": "set_agent_policy", "conv_id": conv, "agent_id": bot.id, "trigger": "all"}))
        .await;
    let mut c = Conn::new(&hub, &carol);
    let mut g = Conn::new(&hub, &bot);
    c.drain();
    g.drain();
    let new = a.send(&conv, "after carol").await["msg_id"].clone();
    a.ok(json!({"op": "edit", "msg_id": old, "text": "secret plan v2"}))
        .await;
    a.ok(json!({"op": "recall", "msg_id": gone})).await;
    a.ok(json!({"op": "edit", "msg_id": new, "text": "after carol v2"}))
        .await;
    let ids = |evs: Vec<Value>| -> Vec<String> {
        evs.iter()
            .filter(|f| f["type"] == "event")
            .filter_map(|f| f["event"]["msg"]["msg_id"].as_str().map(String::from))
            .collect()
    };
    let want = vec![new.as_str().unwrap().to_string(); 2];
    assert_eq!(ids(c.drain()), want, "live, person");
    assert_eq!(ids(g.drain()), want, "live, agent with trigger all");
    // A device syncs, the agent syncs, reads go through the server.
    let (_, device) = hub.authenticate_device(&token).unwrap();
    for (who, conn) in [
        ("sync, person", Conn::device(&hub, &carol, &device)),
        ("sync, agent", Conn::new(&hub, &bot)),
    ] {
        let s = conn.ok(json!({"op": "sync"})).await;
        let evs: Vec<Value> = s["convs"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|c| c["events"].as_array().unwrap().clone())
            .collect();
        assert!(ids(evs).iter().all(|i| *i == new), "{who}: {s}");
    }
    for conn in [Conn::new(&hub, &carol), Conn::new(&hub, &bot)] {
        let p = conn.pull(&conv, 0).await;
        let evs = p["events"].as_array().unwrap().clone();
        assert!(ids(evs).iter().all(|i| *i == new), "{p}");
    }
    let h = Conn::new(&hub, &bot)
        .ok(json!({"op": "history", "conv_id": conv}))
        .await;
    assert!(!h.to_string().contains("secret"), "{h}");
    // Members who could see the message still get the edit.
    let p = Conn::new(&hub, &bob).pull(&conv, 0).await;
    assert!(p.to_string().contains("secret plan v2"));
}

/// H4: recalling an approved agent message also empties its draft, in the
/// side channel the owner and the agent sync from.
async fn a_recalled_message_leaves_no_draft_copy(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    let a = Conn::new(&hub, &alice);
    let conv = a.group(&[&bob, &bot]).await;
    a.ok(json!({"op": "set_agent_policy", "conv_id": conv, "agent_id": bot.id, "approval": true}))
        .await;
    let g = Conn::new(&hub, &bot);
    let mut drafts = vec![];
    for text in ["recall me", "keep me"] {
        let d = g
            .ok(json!({"op": "send", "conv_id": conv, "client_msg_id": text, "text": text}))
            .await["draft_id"]
            .clone();
        a.ok(json!({"op": "approve", "draft_id": d})).await;
        drafts.push(d);
    }
    let p = a.pull(&conv, 0).await;
    let id = |t: &str| {
        p["events"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["event"]["msg"]["content"] == t)
            .unwrap()["event"]["msg"]["msg_id"]
            .clone()
    };
    let gone = id("recall me");
    // Recalled by its owner, and by the agent itself, are both covered.
    a.ok(json!({"op": "recall", "msg_id": gone})).await;
    for c in [&a, &g] {
        let side = c
            .ok(json!({"op": "pull_side", "conv_id": conv, "after_side_seq": 0}))
            .await
            .to_string();
        assert!(!side.contains("recall me"), "{side}");
        assert!(side.contains("keep me"), "other drafts stay: {side}");
    }
    let s = Conn::new(&hub, &bot)
        .ok(json!({"op": "sync"}))
        .await
        .to_string();
    assert!(!s.contains("recall me") && s.contains("keep me"), "{s}");
    // Recalling again is a conflict and changes nothing.
    assert_eq!(
        code(&a.call(json!({"op": "recall", "msg_id": gone})).await),
        "conflict"
    );
}
