//! Pinned messages (docs/en/design/messaging.md, MSG-21): pin and unpin
//! once; who may pin (members or admins, direct conversations, agents); who
//! may unpin; what can be pinned; the limit; recall; members who joined
//! later; the audit; restart; and a race at the limit. Run against every
//! backend.

mod common;

use agentim_core::config::MessagesConfig;
use agentim_core::hub::Hub;
use agentim_core::model::*;
use common::*;
use serde_json::{Value, json};

backend_tests!(
    messages_are_pinned_once_and_listed,
    who_may_pin_and_unpin,
    what_can_be_pinned,
    agents_pin_only_when_the_app_allows,
    the_pin_limit,
    a_recall_unpins_and_pins_survive_a_restart,
    race_pins_at_the_limit,
);

fn rule_of(v: &Value) -> &str {
    v["error"]["rule"].as_str().unwrap_or("")
}

fn code(v: &Value) -> &str {
    v["error"]["code"].as_str().unwrap_or("")
}

fn pin(conv: &str, msg: &str) -> Value {
    json!({"op": "pin", "conv_id": conv, "msg_id": msg})
}

fn unpin(conv: &str, msg: &str) -> Value {
    json!({"op": "unpin", "conv_id": conv, "msg_id": msg})
}

async fn with(db: &TestDb, f: impl FnOnce(&mut MessagesConfig)) -> Hub {
    let mut h = hub_cfg();
    f(&mut h.messages);
    Hub::open(single(&db.url), h).await.unwrap()
}

async fn head(c: &Conn, conv: &str) -> i64 {
    seqs(&c.pull(conv, 0).await).last().copied().unwrap_or(0)
}

async fn pinned(c: &Conn, conv: &str) -> Vec<String> {
    c.ok(json!({"op": "list_pins", "conv_id": conv})).await["pins"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["msg_id"].as_str().unwrap().to_string())
        .collect()
}

async fn audits(hub: &Hub, action: &str) -> usize {
    hub.audit_log(1000)
        .await
        .unwrap()
        .iter()
        .filter(|a| a["action"] == action)
        .count()
}

async fn messages_are_pinned_once_and_listed(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let a = Conn::new(&hub, &alice);
    let mut b = Conn::new(&hub, &bob);
    let conv = a.group(&[&bob]).await;
    let rules = a.send(&conv, "house rules: be kind").await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    let link = a.send(&conv, "the plan: https://example.com/plan").await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    b.drain();
    let r = b.ok(pin(&conv, &rules)).await;
    assert_eq!(r["changed"], true);
    // An event of the log: shown as a notice, not a notification or unread.
    let pushed: Vec<Value> = b
        .drain()
        .into_iter()
        .filter(|f| f["type"] == "event")
        .collect();
    assert_eq!(pushed[0]["event"]["type"], "msg.pinned");
    assert_eq!(pushed[0]["event"]["data"]["by"], bob.id.as_str());
    assert_eq!(pushed[0]["notify"], false);
    let h = head(&a, &conv).await;
    assert_eq!(b.ok(pin(&conv, &rules)).await["changed"], false, "once");
    assert_eq!(head(&a, &conv).await, h, "and nothing written");
    a.ok(pin(&conv, &link)).await;
    // Latest pin first, with the message, the count and the policy.
    let l = a.ok(json!({"op": "list_pins", "conv_id": conv})).await;
    assert_eq!(pinned(&a, &conv).await, vec![link.clone(), rules.clone()]);
    assert_eq!(l["count"], 2);
    assert_eq!(l["pin_policy"], "members");
    assert_eq!(l["pins"][1]["msg"]["content"], "house rules: be kind");
    assert_eq!(l["pins"][1]["by"], bob.id.as_str());
    let page = a
        .ok(json!({"op": "list_pins", "conv_id": conv, "limit": 1}))
        .await;
    assert_eq!(page["more"], true);
    let page2 = a
        .ok(json!({"op": "list_pins", "conv_id": conv, "limit": 1, "before_seq": page["pins"][0]["seq"]}))
        .await;
    assert_eq!(page2["pins"][0]["msg_id"], rules.as_str());
    // The summary of a message says it is pinned.
    let s = a
        .ok(json!({"op": "msg_summary", "conv_id": conv, "msg_ids": [rules]}))
        .await;
    assert_eq!(s["messages"][&rules]["pinned"]["by"], bob.id.as_str());
    // Unpin once.
    assert_eq!(b.ok(unpin(&conv, &rules)).await["changed"], true);
    assert_eq!(b.ok(unpin(&conv, &rules)).await["changed"], false);
    assert_eq!(pinned(&a, &conv).await, vec![link]);
    assert_eq!(audits(&hub, "msg.pinned").await, 2);
    assert_eq!(audits(&hub, "msg.unpinned").await, 1);
}

async fn who_may_pin_and_unpin(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let carol = user(&hub, "carol", UserKind::Human, None).await;
    let eve = user(&hub, "eve", UserKind::Human, None).await;
    let a = Conn::new(&hub, &alice);
    let b = Conn::new(&hub, &bob);
    let c = Conn::new(&hub, &carol);
    let e = Conn::new(&hub, &eve);
    let conv = a.group(&[&bob, &carol]).await;
    a.ok(json!({"op": "set_role", "conv_id": conv, "user_id": carol.id, "role": "admin"}))
        .await;
    let m1 = a.send(&conv, "one").await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    let m2 = a.send(&conv, "two").await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    // Anyone may pin by default; someone else's pin only the owner and admins unpin.
    b.ok(pin(&conv, &m1)).await;
    let before = head(&a, &conv).await;
    let r = e.call(pin(&conv, &m2)).await;
    assert_eq!(code(&r), "forbidden", "not a member: {r}");
    a.ok(pin(&conv, &m2)).await;
    let r = b.call(unpin(&conv, &m2)).await;
    assert_eq!(
        (rule_of(&r), code(&r)),
        ("pin.unpin_forbidden", "forbidden"),
        "{r}"
    );
    c.ok(unpin(&conv, &m2)).await;
    // The policy: only the owner and admins change it; values are checked.
    let r = b
        .call(json!({"op": "update_conv", "conv_id": conv, "pin_policy": "admins"}))
        .await;
    assert_eq!(code(&r), "forbidden");
    let r = a
        .call(json!({"op": "update_conv", "conv_id": conv, "pin_policy": "everyone"}))
        .await;
    assert_eq!((rule_of(&r), code(&r)), ("pin.policy", "bad_request"));
    a.ok(json!({"op": "update_conv", "conv_id": conv, "pin_policy": "admins"}))
        .await;
    assert!(
        a.pull(&conv, before)
            .await
            .to_string()
            .contains("\"pin_policy\":\"admins\""),
        "conv.updated says so"
    );
    let r = b.call(pin(&conv, &m2)).await;
    assert_eq!(
        (rule_of(&r), code(&r)),
        ("pin.forbidden", "forbidden"),
        "{r}"
    );
    c.ok(pin(&conv, &m2)).await;
    // Under admins, a member still unpins their own earlier pin.
    b.ok(unpin(&conv, &m1)).await;
    // A direct conversation: both pin and unpin, no policy.
    let dm = a
        .ok(json!({"op": "create_conv", "kind": "dm", "title": "dm", "members": [bob.id]}))
        .await["conv_id"]
        .as_str()
        .unwrap()
        .to_string();
    let d = a.send(&dm, "ours").await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    a.ok(pin(&dm, &d)).await;
    b.ok(unpin(&dm, &d)).await;
    let r = a
        .call(json!({"op": "update_conv", "conv_id": dm, "pin_policy": "admins"}))
        .await;
    assert_eq!(rule_of(&r), "pin.policy_dm", "{r}");
    // Blocked: no pinning in that direct conversation.
    b.ok(json!({"op": "block", "user_id": alice.id})).await;
    let r = a.call(pin(&dm, &d)).await;
    assert_eq!(code(&r), "blocked", "{r}");
}

async fn what_can_be_pinned(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let carol = user(&hub, "carol", UserKind::Human, None).await;
    let a = Conn::new(&hub, &alice);
    let b = Conn::new(&hub, &bob);
    let c = Conn::new(&hub, &carol);
    let conv = a.group(&[&bob]).await;
    let other = a.group(&[&bob]).await;
    let old = a.send(&conv, "before carol").await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    a.ok(pin(&conv, &old)).await;
    let gone = a.send(&conv, "gone").await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    a.ok(json!({"op": "recall", "msg_id": gone})).await;
    let elsewhere = a.send(&other, "x").await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
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
    for (msg, rule, c) in [
        ("m_unknown", "pin.unknown_message", "not_found"),
        (elsewhere.as_str(), "pin.unknown_message", "not_found"),
        (gone.as_str(), "pin.recalled", "conflict"),
        (sys.as_str(), "pin.not_pinnable", "bad_request"),
    ] {
        let r = b.call(pin(&conv, msg)).await;
        assert_eq!((rule_of(&r), code(&r)), (rule, c), "{msg}: {r}");
    }
    assert_eq!(head(&a, &conv).await, before, "refusals leave nothing");
    // A thread reply can be pinned.
    let root = a.send(&conv, "root").await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    let reply = b
        .ok(json!({"op": "send", "conv_id": conv, "client_msg_id": "r", "text": "answer", "thread_id": root}))
        .await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    a.ok(pin(&conv, &reply)).await;
    // Someone who joined later sees only pins of messages they can see.
    a.ok(json!({"op": "add_member", "conv_id": conv, "user_id": carol.id, "history": 0}))
        .await;
    let seen = pinned(&c, &conv).await;
    assert!(!seen.contains(&old), "{seen:?}");
    let r = c.call(unpin(&conv, &old)).await;
    assert_eq!(code(&r), "not_found");
}

async fn agents_pin_only_when_the_app_allows(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    let a = Conn::new(&hub, &alice);
    let g = Conn::new(&hub, &bot);
    let conv = a.group(&[&bot]).await;
    let m = a.send(&conv, "decision: ship friday").await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    let r = g.call(pin(&conv, &m)).await;
    assert_eq!((rule_of(&r), code(&r)), ("pin.agents", "forbidden"), "{r}");
    a.ok(pin(&conv, &m)).await;
    let r = g.call(unpin(&conv, &m)).await;
    assert_eq!(rule_of(&r), "pin.agents");
    // Agents still read the pins.
    assert_eq!(pinned(&g, &conv).await, vec![m.clone()]);
    drop((a, g));
    drop(hub);
    let hub = with(&db, |c| c.agents_pin = true).await;
    let a = Conn::new(&hub, &alice);
    let g = Conn::new(&hub, &bot);
    // Allowed now, but like a member: someone else's pin stays theirs.
    let r = g.call(unpin(&conv, &m)).await;
    assert_eq!(rule_of(&r), "pin.unpin_forbidden", "{r}");
    a.ok(unpin(&conv, &m)).await;
    g.ok(pin(&conv, &m)).await;
    g.ok(unpin(&conv, &m)).await;
}

async fn the_pin_limit(db: TestDb) {
    let hub = with(&db, |c| c.pins_per_conv = 3).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let a = Conn::new(&hub, &alice);
    let conv = a.group(&[]).await;
    let mut ids = vec![];
    for i in 0..4 {
        ids.push(
            a.send(&conv, &format!("m{i}")).await["msg_id"]
                .as_str()
                .unwrap()
                .to_string(),
        );
    }
    for m in &ids[..3] {
        a.ok(pin(&conv, m)).await;
    }
    let r = a.call(pin(&conv, &ids[3])).await;
    assert_eq!((rule_of(&r), code(&r)), ("pin.limit", "conflict"), "{r}");
    // Pinning one already pinned is still fine at the limit.
    assert_eq!(a.ok(pin(&conv, &ids[0])).await["changed"], false);
    a.ok(unpin(&conv, &ids[0])).await;
    a.ok(pin(&conv, &ids[3])).await;
}

async fn a_recall_unpins_and_pins_survive_a_restart(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let (conv, keep, list) = {
        let a = Conn::new(&hub, &alice);
        let b = Conn::new(&hub, &bob);
        let conv = a.group(&[&bob]).await;
        let keep = a.send(&conv, "keep").await["msg_id"]
            .as_str()
            .unwrap()
            .to_string();
        let gone = b.send(&conv, "gone").await["msg_id"]
            .as_str()
            .unwrap()
            .to_string();
        a.ok(pin(&conv, &keep)).await;
        a.ok(pin(&conv, &gone)).await;
        b.ok(json!({"op": "recall", "msg_id": gone})).await;
        assert_eq!(pinned(&a, &conv).await, vec![keep.clone()]);
        let list = a.ok(json!({"op": "list_pins", "conv_id": conv})).await;
        (conv, keep, list)
    };
    drop(hub);
    let hub = open(single(&db.url)).await;
    let a = Conn::new(&hub, &alice);
    assert_eq!(
        a.ok(json!({"op": "list_pins", "conv_id": conv})).await,
        list
    );
    assert_eq!(a.ok(pin(&conv, &keep)).await["changed"], false);
}

async fn race_pins_at_the_limit(db: TestDb) {
    let hub = with(&db, |c| c.pins_per_conv = 3).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let mut people = vec![];
    for i in 0..6 {
        people.push(user(&hub, &format!("p{i}"), UserKind::Human, None).await);
    }
    let a = Conn::new(&hub, &alice);
    let refs: Vec<&User> = people.iter().collect();
    let check = |mutated: bool| {
        let (hub, people, alice, refs) = (
            hub.clone(),
            people.clone(),
            alice.clone(),
            refs.iter().map(|u| (*u).clone()).collect::<Vec<User>>(),
        );
        async move {
            let mut wrong = 0;
            for _ in 0..8 {
                let a = Conn::new(&hub, &alice);
                let r: Vec<&User> = refs.iter().collect();
                let conv = a.group(&r).await;
                let mut msgs = vec![];
                for i in 0..6 {
                    msgs.push(
                        a.send(&conv, &format!("m{i}")).await["msg_id"]
                            .as_str()
                            .unwrap()
                            .to_string(),
                    );
                }
                // Six people pin six messages at once; a conversation holds three.
                let mut tasks = vec![];
                for (p, m) in people.iter().zip(msgs) {
                    let (hub, p, conv) = (hub.clone(), p.clone(), conv.clone());
                    tasks.push(tokio::spawn(async move {
                        Conn::new(&hub, &p).call(pin(&conv, &m)).await
                    }));
                }
                let mut won = 0;
                for t in tasks {
                    let r = t.await.unwrap();
                    if r["ok"] == true {
                        won += 1;
                    } else {
                        assert_eq!(rule_of(&r), "pin.limit", "{r}");
                    }
                }
                let n = pinned(&a, &conv).await.len();
                assert_eq!(n, won, "the table holds what was accepted");
                if n != 3 {
                    wrong += 1;
                }
            }
            assert!(mutated || wrong == 0, "{wrong} rounds past the limit");
            wrong
        }
    };
    let _ = a;
    assert_eq!(check(false).await, 0);
    let fp = &hub.shared().failpoints;
    fp.set(
        agentim_core::logic::social::MUT_PIN_LIMIT,
        agentim_core::failpoint::Action::Error,
        None,
    );
    assert!(check(true).await > 0, "the mutation must be caught");
    fp.release(agentim_core::logic::social::MUT_PIN_LIMIT);
}
