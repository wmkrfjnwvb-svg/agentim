//! Reactions (docs/en/design/messaging.md, MSG-20): one per message, person
//! and key; repeated and absent calls change nothing; keys are one emoji or
//! one of the app's names; limits per message and per person; single mode;
//! who may react; what agents hear; reactions as feedback; recall; restart;
//! and races at the limit, against a recall and from many devices. Run
//! against every backend.

mod common;

use agentim_core::config::MessagesConfig;
use agentim_core::hub::Hub;
use agentim_core::model::*;
use common::*;
use serde_json::{Value, json};

backend_tests!(
    reactions_are_added_once_and_summarised,
    reaction_keys_are_checked,
    who_may_react,
    reaction_limits_and_single_mode,
    agents_hear_of_reactions_to_their_own_messages,
    reactions_can_count_as_feedback,
    a_recall_takes_the_reactions,
    reactions_survive_a_restart,
    race_reactions_at_the_key_limit,
    race_reactions_and_recall,
    race_the_same_reaction_from_many_devices,
);

fn rule_of(v: &Value) -> &str {
    v["error"]["rule"].as_str().unwrap_or("")
}

fn code(v: &Value) -> &str {
    v["error"]["code"].as_str().unwrap_or("")
}

fn react(conv: &str, msg: &str, key: &str) -> Value {
    json!({"op": "react", "conv_id": conv, "msg_id": msg, "key": key})
}

fn unreact(conv: &str, msg: &str, key: &str) -> Value {
    json!({"op": "unreact", "conv_id": conv, "msg_id": msg, "key": key})
}

async fn with(db: &TestDb, f: impl FnOnce(&mut MessagesConfig)) -> Hub {
    let mut h = hub_cfg();
    f(&mut h.messages);
    Hub::open(single(&db.url), h).await.unwrap()
}

async fn head(c: &Conn, conv: &str) -> i64 {
    seqs(&c.pull(conv, 0).await).last().copied().unwrap_or(0)
}

async fn reactions(c: &Conn, conv: &str, msg: &str) -> Value {
    c.ok(json!({"op": "msg_summary", "conv_id": conv, "msg_ids": [msg]}))
        .await["messages"][msg]["reactions"]
        .clone()
}

/// The reactions of a message replayed from the log: (user, key) pairs.
async fn replayed(c: &Conn, conv: &str, msg: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = vec![];
    for e in c.pull(conv, 0).await["events"].as_array().unwrap() {
        let e = &e["event"];
        if e["data"]["msg_id"] != msg {
            continue;
        }
        let pair = (
            e["data"]["user_id"].as_str().unwrap().to_string(),
            e["data"]["key"].as_str().unwrap().to_string(),
        );
        match e["type"].as_str() {
            Some("reaction.added") => out.push(pair),
            Some("reaction.removed") => out.retain(|p| *p != pair),
            _ => {}
        }
    }
    out
}

async fn reactions_are_added_once_and_summarised(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let carol = user(&hub, "carol", UserKind::Human, None).await;
    let mut a = Conn::new(&hub, &alice);
    let b = Conn::new(&hub, &bob);
    let c = Conn::new(&hub, &carol);
    let conv = a.group(&[&bob, &carol]).await;
    let m = a.send(&conv, "lunch at noon?").await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    a.drain();
    let r = b.ok(react(&conv, &m, "👍")).await;
    assert_eq!(r["changed"], true);
    // Pushed to the members as an event of the log, without a notification.
    let pushed: Vec<Value> = a
        .drain()
        .into_iter()
        .filter(|f| f["type"] == "event")
        .collect();
    assert_eq!(pushed.len(), 1);
    assert_eq!(pushed[0]["event"]["type"], "reaction.added");
    assert_eq!(pushed[0]["event"]["seq"], r["seq"]);
    assert_eq!(pushed[0]["event"]["data"]["key"], "👍");
    assert_eq!(pushed[0]["notify"], false);
    // Again: nothing changes, nothing is written.
    let h = head(&a, &conv).await;
    let again = b.ok(react(&conv, &m, "👍")).await;
    assert_eq!(again["changed"], false);
    assert!(again["seq"].is_null());
    assert_eq!(head(&a, &conv).await, h);
    // Reacting to one's own message is fine.
    a.ok(react(&conv, &m, "👍")).await;
    c.ok(react(&conv, &m, "❤️")).await;
    let s = reactions(&a, &conv, &m).await;
    assert_eq!(
        s,
        json!([
            {"key": "👍", "count": 2, "me": true, "users": [alice.id, bob.id]},
            {"key": "❤️", "count": 1, "me": false, "users": [carol.id]},
        ]),
        "{s}"
    );
    // Reactions are not unread messages.
    assert_eq!(c.ok(json!({"op": "unread"})).await["total"], 1);
    // Who reacted, oldest first, in pages.
    let l = a
        .ok(json!({"op": "list_reactions", "conv_id": conv, "msg_id": m, "key": "👍"}))
        .await;
    let users: Vec<&Value> = l["users"]
        .as_array()
        .unwrap()
        .iter()
        .map(|u| &u["user_id"])
        .collect();
    assert_eq!(users, vec![&json!(bob.id), &json!(alice.id)]);
    assert_eq!(l["users"][0]["name"], "bob");
    let p = a
        .ok(json!({"op": "list_reactions", "conv_id": conv, "msg_id": m, "key": "👍", "limit": 1}))
        .await;
    assert_eq!(p["more"], true);
    let p2 = a
        .ok(json!({"op": "list_reactions", "conv_id": conv, "msg_id": m, "key": "👍", "after_seq": p["users"][0]["seq"]}))
        .await;
    assert_eq!(p2["users"][0]["user_id"], alice.id.as_str());
    // Removing: once; a second time changes nothing.
    let r = b.ok(unreact(&conv, &m, "👍")).await;
    assert_eq!(r["changed"], true);
    let r = b.ok(unreact(&conv, &m, "👍")).await;
    assert_eq!(r["changed"], false);
    let r = b.ok(unreact(&conv, &m, "🎉")).await;
    assert_eq!(r["changed"], false);
    // The log replays to the table.
    let mut log = replayed(&a, &conv, &m).await;
    log.sort();
    let mut want = vec![
        (alice.id.clone(), "👍".to_string()),
        (carol.id.clone(), "❤️".to_string()),
    ];
    want.sort();
    assert_eq!(log, want);
}

async fn reaction_keys_are_checked(db: TestDb) {
    let hub = with(&db, |m| {
        m.reaction_names = vec!["party_parrot".into(), "+1".into()]
    })
    .await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let a = Conn::new(&hub, &alice);
    let conv = a.group(&[]).await;
    let m = a.send(&conv, "hi").await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    for ok in [
        "👍",
        "👍🏾",
        "❤️",
        "🇫🇷",
        "1️⃣",
        "👩‍💻",
        "🏳️‍🌈",
        ":party_parrot:",
        ":+1:",
    ] {
        let r = a.ok(react(&conv, &m, ok)).await;
        assert_eq!(r["changed"], true, "{ok}");
        a.ok(unreact(&conv, &m, ok)).await;
    }
    let before = head(&a, &conv).await;
    for bad in [
        "",
        "a",
        "ok",
        "👍👍",
        " 👍",
        ":unknown:",
        "party_parrot",
        "::",
        &"😀".repeat(20),
        "<b>",
    ] {
        let r = a.call(react(&conv, &m, bad)).await;
        assert_eq!(
            (rule_of(&r), code(&r)),
            ("reaction.key_invalid", "bad_request"),
            "{bad:?}: {r}"
        );
    }
    assert_eq!(head(&a, &conv).await, before, "refused keys leave nothing");
}

async fn who_may_react(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let eve = user(&hub, "eve", UserKind::Human, None).await;
    let carol = user(&hub, "carol", UserKind::Human, None).await;
    let a = Conn::new(&hub, &alice);
    let b = Conn::new(&hub, &bob);
    let e = Conn::new(&hub, &eve);
    let c = Conn::new(&hub, &carol);
    let conv = a.group(&[&bob]).await;
    let other = a.group(&[&bob]).await;
    let m = a.send(&conv, "hello").await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    let elsewhere = a.send(&other, "x").await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    let gone = a.send(&conv, "gone").await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    a.ok(json!({"op": "recall", "msg_id": gone})).await;
    let sys = hub
        .act_as(
            &hub.system_user(),
            serde_json::from_value(
                json!({"op": "send", "conv_id": conv, "client_msg_id": "s", "text": "notice"}),
            )
            .unwrap(),
        )
        .await
        .unwrap()["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    let before = head(&a, &conv).await;
    let cases = [
        (&e, m.as_str(), "member.required", "forbidden"),
        (&b, "m_unknown", "reaction.unknown_message", "not_found"),
        (
            &b,
            elsewhere.as_str(),
            "reaction.unknown_message",
            "not_found",
        ),
        (&b, gone.as_str(), "reaction.recalled", "conflict"),
        (&b, sys.as_str(), "reaction.not_reactable", "bad_request"),
    ];
    for (who, msg, rule, c) in cases {
        let r = who.call(react(&conv, msg, "👍")).await;
        assert_eq!((rule_of(&r), code(&r)), (rule, c), "{msg}: {r}");
    }
    // A member who joined later does not see older messages.
    a.ok(json!({"op": "add_member", "conv_id": conv, "user_id": carol.id, "history": 0}))
        .await;
    let joined = head(&a, &conv).await;
    let r = c.call(react(&conv, &m, "👍")).await;
    assert_eq!(code(&r), "not_found", "{r}");
    let r = c
        .call(json!({"op": "list_reactions", "conv_id": conv, "msg_id": m, "key": "👍"}))
        .await;
    assert_eq!(code(&r), "not_found");
    // Blocked in a direct conversation: blocked.
    let dm = a
        .ok(json!({"op": "create_conv", "kind": "dm", "title": "dm", "members": [bob.id]}))
        .await["conv_id"]
        .as_str()
        .unwrap()
        .to_string();
    let dm_msg = a.send(&dm, "hey").await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    b.ok(json!({"op": "block", "user_id": alice.id})).await;
    let r = a.call(react(&dm, &dm_msg, "👍")).await;
    assert_eq!(code(&r), "blocked", "{r}");
    // Nothing of the refusals reached the log.
    assert_eq!(head(&a, &conv).await, joined);
    assert!(joined > before);
}

async fn reaction_limits_and_single_mode(db: TestDb) {
    let hub = with(&db, |m| {
        m.reaction_keys = 3;
        m.reactions_per_user = 2;
    })
    .await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let a = Conn::new(&hub, &alice);
    let b = Conn::new(&hub, &bob);
    let conv = a.group(&[&bob]).await;
    let m = a.send(&conv, "vote").await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    a.ok(react(&conv, &m, "👍")).await;
    a.ok(react(&conv, &m, "👎")).await;
    // Per person: the third is one too many.
    let r = a.call(react(&conv, &m, "🎉")).await;
    assert_eq!(
        (rule_of(&r), code(&r)),
        ("reaction.too_many_per_user", "conflict")
    );
    // Per message: 3 different keys; a key already there still takes more people.
    b.ok(react(&conv, &m, "🎉")).await;
    let r = b.call(react(&conv, &m, "🚀")).await;
    assert_eq!(
        (rule_of(&r), code(&r)),
        ("reaction.too_many_keys", "conflict")
    );
    b.ok(react(&conv, &m, "👍")).await;
    // Freeing a key makes room.
    b.ok(unreact(&conv, &m, "🎉")).await;
    b.ok(react(&conv, &m, "🚀")).await;
    drop((a, b));
    drop(hub);

    // A user reacts at most so often (messages.reactions_per_second).
    let hub = with(&db, |m| m.reactions_per_second = 2.0).await;
    let a = Conn::new(&hub, &alice);
    let mut limited = None;
    for k in ["😀", "😃", "😄", "😁"] {
        let r = a.call(react(&conv, &m, k)).await;
        if r["ok"] != true {
            limited = Some(r);
            break;
        }
        a.call(unreact(&conv, &m, k)).await;
    }
    let r = limited.expect("rate limited");
    assert_eq!(code(&r), "rate_limited", "{r}");
    drop(a);
    drop(hub);

    // Single mode: a new reaction replaces the person's old one.
    let hub = with(&db, |m| m.reaction_mode = "single".into()).await;
    let a = Conn::new(&hub, &alice);
    let conv2 = a.group(&[&bob]).await;
    let m2 = a.send(&conv2, "pick one").await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    a.ok(react(&conv2, &m2, "👍")).await;
    let r = a.ok(react(&conv2, &m2, "❤️")).await;
    assert_eq!(r["changed"], true);
    let s = reactions(&a, &conv2, &m2).await;
    assert_eq!(
        s,
        json!([{"key": "❤️", "count": 1, "me": true, "users": [alice.id]}])
    );
    assert_eq!(
        replayed(&a, &conv2, &m2).await,
        vec![(alice.id.clone(), "❤️".to_string())]
    );
}

async fn agents_hear_of_reactions_to_their_own_messages(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    let other = user(&hub, "other", UserKind::Agent, Some(&alice)).await;
    let a = Conn::new(&hub, &alice);
    let mut g = Conn::new(&hub, &bot);
    let mut o = Conn::new(&hub, &other);
    let conv = a.group(&[&bot, &other]).await;
    for x in [&bot, &other] {
        a.ok(
            json!({"op": "set_agent_policy", "conv_id": conv, "agent_id": x.id, "trigger": "all"}),
        )
        .await;
    }
    let q = a.send(&conv, "status?").await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    let ans = g.reply(&conv, "all green", &q).await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    g.drain();
    o.drain();
    a.ok(react(&conv, &ans, "🙏")).await;
    a.ok(react(&conv, &q, "👀")).await;
    let heard: Vec<Value> = g
        .drain()
        .into_iter()
        .filter(|f| f["type"] == "event" && f["event"]["type"] == "reaction.added")
        .collect();
    assert_eq!(heard.len(), 1, "only the reaction to its own message");
    assert_eq!(heard[0]["event"]["data"]["msg_id"], ans.as_str());
    assert_ne!(heard[0]["wake"], true, "a reaction does not wake it");
    assert!(
        o.drain()
            .iter()
            .all(|f| f["event"]["type"] != "reaction.added"),
        "other agents hear nothing"
    );
    // Agents react too (e.g. 👀 on a request), and see it in their history.
    let r = g.ok(react(&conv, &q, "✅")).await;
    assert_eq!(r["changed"], true);
    let h = g.ok(json!({"op": "history", "conv_id": conv})).await;
    assert!(h.to_string().contains("reaction.added"));
}

async fn reactions_can_count_as_feedback(db: TestDb) {
    let hub = with(&db, |m| m.reactions_as_feedback = true).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    let a = Conn::new(&hub, &alice);
    let b = Conn::new(&hub, &bob);
    let g = Conn::new(&hub, &bot);
    let conv = a.group(&[&bob, &bot]).await;
    a.ok(json!({"op": "set_agent_policy", "conv_id": conv, "agent_id": bot.id, "trigger": "all"}))
        .await;
    let q = b.send(&conv, "help").await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    let ans = g.reply(&conv, "here", &q).await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    let fb = || async {
        a.ok(json!({"op": "list_feedback", "agent_id": bot.id}))
            .await
    };
    b.ok(react(&conv, &ans, "👍🏽")).await;
    let f = fb().await;
    assert_eq!(
        (f["up"].as_i64(), f["down"].as_i64()),
        (Some(1), Some(0)),
        "{f}"
    );
    // Other keys are not feedback; the latest thumb decides; removing takes it back.
    b.ok(react(&conv, &ans, "🎉")).await;
    b.ok(react(&conv, &ans, "👎")).await;
    let f = fb().await;
    assert_eq!(
        (f["up"].as_i64(), f["down"].as_i64()),
        (Some(0), Some(1)),
        "{f}"
    );
    b.ok(unreact(&conv, &ans, "👎")).await;
    let f = fb().await;
    assert_eq!(
        (f["up"].as_i64(), f["down"].as_i64()),
        (Some(1), Some(0)),
        "{f}"
    );
    b.ok(unreact(&conv, &ans, "👍🏽")).await;
    let f = fb().await;
    assert_eq!(
        (f["up"].as_i64(), f["down"].as_i64()),
        (Some(0), Some(0)),
        "{f}"
    );
    // A person's message is no agent to rate.
    b.ok(react(&conv, &q, "👍")).await;
    assert_eq!(fb().await["up"], 0);
}

async fn a_recall_takes_the_reactions(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let a = Conn::new(&hub, &alice);
    let b = Conn::new(&hub, &bob);
    let conv = a.group(&[&bob]).await;
    let m = a.send(&conv, "oops").await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    b.ok(react(&conv, &m, "😂")).await;
    a.ok(json!({"op": "recall", "msg_id": m})).await;
    assert!(
        reactions(&a, &conv, &m)
            .await
            .as_array()
            .is_none_or(Vec::is_empty)
    );
    let r = b
        .ok(json!({"op": "list_reactions", "conv_id": conv, "msg_id": m, "key": "😂"}))
        .await;
    assert_eq!(r["users"], json!([]));
    // Removing what the recall took changes nothing.
    assert_eq!(b.ok(unreact(&conv, &m, "😂")).await["changed"], false);
}

async fn reactions_survive_a_restart(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let (conv, m, before) = {
        let a = Conn::new(&hub, &alice);
        let b = Conn::new(&hub, &bob);
        let conv = a.group(&[&bob]).await;
        let m = a.send(&conv, "keep").await["msg_id"]
            .as_str()
            .unwrap()
            .to_string();
        b.ok(react(&conv, &m, "👍")).await;
        a.ok(react(&conv, &m, "🔥")).await;
        let s = reactions(&a, &conv, &m).await;
        (conv, m, s)
    };
    drop(hub);
    let hub = open(single(&db.url)).await;
    let a = Conn::new(&hub, &alice);
    let b = Conn::new(&hub, &bob);
    assert_eq!(reactions(&a, &conv, &m).await, before);
    assert_eq!(b.ok(react(&conv, &m, "👍")).await["changed"], false);
}

async fn race_reactions_at_the_key_limit(db: TestDb) {
    let hub = with(&db, |m| m.reaction_keys = 4).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let mut people = vec![];
    for i in 0..8 {
        people.push(user(&hub, &format!("p{i}"), UserKind::Human, None).await);
    }
    let a = Conn::new(&hub, &alice);
    let refs: Vec<&User> = people.iter().collect();
    let conv = a.group(&refs).await;
    const KEYS: [&str; 8] = ["😀", "😃", "😄", "😁", "😆", "😅", "🤣", "😂"];
    let check = |mutated: bool| {
        let (hub, people, conv, alice) = (hub.clone(), people.clone(), conv.clone(), alice.clone());
        async move {
            let mut wrong = 0;
            for round in 0..10 {
                let a = Conn::new(&hub, &alice);
                let m = a.send(&conv, &format!("r{round}")).await["msg_id"]
                    .as_str()
                    .unwrap()
                    .to_string();
                let mut tasks = vec![];
                for (i, p) in people.iter().enumerate() {
                    let (hub, p, conv, m) = (hub.clone(), p.clone(), conv.clone(), m.clone());
                    tasks.push(tokio::spawn(async move {
                        Conn::new(&hub, &p).call(react(&conv, &m, KEYS[i])).await
                    }));
                }
                let mut won = 0;
                for t in tasks {
                    let r = t.await.unwrap();
                    match r["ok"].as_bool() {
                        Some(true) => won += 1,
                        _ => assert_eq!(rule_of(&r), "reaction.too_many_keys", "{r}"),
                    }
                }
                let keys = reactions(&a, &conv, &m)
                    .await
                    .as_array()
                    .map_or(0, Vec::len);
                if won != 4 || keys != 4 {
                    wrong += 1;
                }
                assert_eq!(keys, won, "the table holds what was accepted");
                assert_eq!(
                    replayed(&a, &conv, &m).await.len(),
                    won,
                    "replay equals state"
                );
            }
            assert!(mutated || wrong == 0, "{wrong} rounds past the limit");
            wrong
        }
    };
    assert_eq!(check(false).await, 0);
    let fp = &hub.shared().failpoints;
    fp.set(
        agentim_core::logic::social::MUT_REACTION_LIMIT,
        agentim_core::failpoint::Action::Error,
        None,
    );
    assert!(check(true).await > 0, "the mutation must be caught");
    fp.release(agentim_core::logic::social::MUT_REACTION_LIMIT);
}

async fn race_reactions_and_recall(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let mut people = vec![];
    for i in 0..6 {
        people.push(user(&hub, &format!("p{i}"), UserKind::Human, None).await);
    }
    let a = Conn::new(&hub, &alice);
    let refs: Vec<&User> = people.iter().collect();
    let conv = a.group(&refs).await;
    let check = |mutated: bool| {
        let (hub, people, conv, alice) = (hub.clone(), people.clone(), conv.clone(), alice.clone());
        async move {
            let mut wrong = 0;
            for round in 0..10 {
                let a = Conn::new(&hub, &alice);
                let m = a.send(&conv, &format!("r{round}")).await["msg_id"]
                    .as_str()
                    .unwrap()
                    .to_string();
                let mut tasks = vec![];
                for p in &people {
                    let (hub, p, conv, m) = (hub.clone(), p.clone(), conv.clone(), m.clone());
                    tasks.push(tokio::spawn(async move {
                        Conn::new(&hub, &p).call(react(&conv, &m, "👍")).await
                    }));
                }
                let recall = {
                    let (hub, alice, m) = (hub.clone(), alice.clone(), m.clone());
                    tokio::spawn(async move {
                        Conn::new(&hub, &alice)
                            .ok(json!({"op": "recall", "msg_id": m}))
                            .await
                    })
                };
                for t in tasks {
                    let r = t.await.unwrap();
                    if r["ok"] != true {
                        assert_eq!(rule_of(&r), "reaction.recalled", "{r}");
                    }
                }
                let recalled_at = recall.await.unwrap()["seq"].as_i64().unwrap();
                // Nothing after the terminal event; nothing left in the table.
                let after = a.pull(&conv, recalled_at).await["events"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|e| e["event"]["data"]["msg_id"] == m)
                    .count();
                assert_eq!(after, 0, "a reaction after the recall");
                let left = a
                    .ok(json!({"op": "list_reactions", "conv_id": conv, "msg_id": m, "key": "👍"}))
                    .await["users"]
                    .as_array()
                    .unwrap()
                    .len();
                if left != 0 {
                    wrong += 1;
                }
            }
            assert!(
                mutated || wrong == 0,
                "{wrong} rounds kept reactions of a recalled message"
            );
            wrong
        }
    };
    assert_eq!(check(false).await, 0);
    let fp = &hub.shared().failpoints;
    fp.set(
        agentim_core::logic::social::MUT_RECALL_KEEPS,
        agentim_core::failpoint::Action::Error,
        None,
    );
    assert!(check(true).await > 0, "the mutation must be caught");
    fp.release(agentim_core::logic::social::MUT_RECALL_KEEPS);
    let found = hub.verify(Some(&conv), None).await.unwrap();
    assert!(
        found.iter().any(|f| f.check == "reaction.state"),
        "{found:?}"
    );
}

async fn race_the_same_reaction_from_many_devices(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let a = Conn::new(&hub, &alice);
    let conv = a.group(&[&bob]).await;
    for round in 0..10 {
        let m = a.send(&conv, &format!("r{round}")).await["msg_id"]
            .as_str()
            .unwrap()
            .to_string();
        let mut tasks = vec![];
        for _ in 0..6 {
            let (hub, bob, conv, m) = (hub.clone(), bob.clone(), conv.clone(), m.clone());
            tasks.push(tokio::spawn(async move {
                Conn::new(&hub, &bob).ok(react(&conv, &m, "👍")).await
            }));
        }
        let mut changed = 0;
        for t in tasks {
            if t.await.unwrap()["changed"] == true {
                changed += 1;
            }
        }
        assert_eq!(changed, 1, "exactly one winner");
        assert_eq!(replayed(&a, &conv, &m).await.len(), 1, "one event");
    }
}
