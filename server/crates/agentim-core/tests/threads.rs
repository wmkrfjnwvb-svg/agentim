//! Threads (docs/en/design/messaging.md, MSG-19): replies to a root message
//! in the conversation's log, a summary on the root, who follows a thread,
//! its read position and unread count, notifications, agents, drafts,
//! recall, new members, restart and concurrent replies. Run against every
//! backend.

mod common;

use agentim_core::model::*;
use common::*;
use serde_json::{Value, json};

backend_tests!(
    replies_go_into_a_thread_with_a_summary,
    the_root_of_a_thread_is_checked,
    thread_replies_count_and_notify_only_followers,
    reading_and_following_threads,
    recall_and_retries_in_threads,
    agents_answer_in_the_thread,
    new_members_see_replies_but_not_older_roots,
    threads_survive_a_restart,
    race_replies_in_one_thread,
);

fn rule_of(v: &Value) -> &str {
    v["error"]["rule"].as_str().unwrap_or("")
}

fn code(v: &Value) -> &str {
    v["error"]["code"].as_str().unwrap_or("")
}

fn in_thread(conv: &str, root: &str, text: &str) -> Value {
    json!({"op": "send", "conv_id": conv, "client_msg_id": uuid::Uuid::new_v4().to_string(), "text": text, "thread_id": root})
}

async fn head(c: &Conn, conv: &str) -> i64 {
    seqs(&c.pull(conv, 0).await).last().copied().unwrap_or(0)
}

async fn summary(c: &Conn, conv: &str, msg: &str) -> Value {
    c.ok(json!({"op": "msg_summary", "conv_id": conv, "msg_ids": [msg]}))
        .await["messages"][msg]
        .clone()
}

async fn replies_go_into_a_thread_with_a_summary(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let carol = user(&hub, "carol", UserKind::Human, None).await;
    let a = Conn::new(&hub, &alice);
    let mut b = Conn::new(&hub, &bob);
    let c = Conn::new(&hub, &carol);
    let conv = a.group(&[&bob, &carol]).await;
    let root = a.send(&conv, "plan for friday?").await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    let r1 = b.ok(in_thread(&conv, &root, "I can do 3pm")).await;
    b.drain();
    let r2 = c.ok(in_thread(&conv, &root, "4pm for me")).await;
    // The reply is an ordinary event of the log, with its thread.
    let pushed: Vec<Value> = b
        .drain()
        .into_iter()
        .filter(|f| f["type"] == "event")
        .collect();
    assert_eq!(pushed.len(), 1, "{pushed:?}");
    assert_eq!(pushed[0]["event"]["msg"]["thread_id"], root.as_str());
    assert_eq!(pushed[0]["event"]["seq"], r2["seq"]);
    let log = a.pull(&conv, 0).await;
    let reply = msg_by_id(&log, r1["msg_id"].as_str().unwrap()).unwrap();
    assert_eq!(reply["thread_id"], root.as_str());
    assert!(reply.get("thread_shown").is_none(), "{reply}");
    // The root's summary: two replies, the latest, who took part.
    let s = summary(&a, &conv, &root).await;
    assert_eq!(s["thread"]["replies"], 2, "{s}");
    assert_eq!(s["thread"]["last_seq"], r2["seq"]);
    assert_eq!(s["thread"]["last_sender"], carol.id.as_str());
    assert_eq!(
        s["thread"]["participants"],
        json!([alice.id, bob.id, carol.id])
    );
    // pull_thread: the root, then the replies in order.
    let t = a
        .ok(json!({"op": "pull_thread", "conv_id": conv, "thread_id": root}))
        .await;
    assert_eq!(t["root"]["msg_id"], root.as_str());
    let ids: Vec<&Value> = t["replies"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| &m["msg_id"])
        .collect();
    assert_eq!(ids, vec![&r1["msg_id"], &r2["msg_id"]]);
    let page = a
        .ok(json!({"op": "pull_thread", "conv_id": conv, "thread_id": root, "after_seq": r1["seq"], "limit": 1}))
        .await;
    assert_eq!(page["replies"][0]["msg_id"], r2["msg_id"]);
    assert_eq!(page["more"], false);
    // A message without replies has no thread in its summary.
    let lone = a.send(&conv, "unrelated").await["msg_id"].clone();
    let s = summary(&a, &conv, lone.as_str().unwrap()).await;
    assert!(s["thread"].is_null(), "{s}");
    // A reply may quote any message, in the thread or outside it.
    let q = b
        .ok(json!({"op": "send", "conv_id": conv, "client_msg_id": "q", "text": "see above", "thread_id": root, "reply_to": lone}))
        .await;
    assert!(q["msg_id"].is_string());
    // Also in the conversation: a flag on the message.
    let shown = c
        .ok(json!({"op": "send", "conv_id": conv, "client_msg_id": "s", "text": "decided: 3pm", "thread_id": root, "also_in_conv": true}))
        .await;
    let m = msg_by_id(&a.pull(&conv, 0).await, shown["msg_id"].as_str().unwrap()).unwrap();
    assert_eq!(m["thread_shown"], true);
    // Limits of reading.
    for bad in [0, 201] {
        let r = a
            .call(json!({"op": "pull_thread", "conv_id": conv, "thread_id": root, "limit": bad}))
            .await;
        assert_eq!(code(&r), "bad_request", "{r}");
    }
    let r = a
        .call(json!({"op": "msg_summary", "conv_id": conv, "msg_ids": []}))
        .await;
    assert_eq!(code(&r), "bad_request");
    let many: Vec<String> = (0..101).map(|i| format!("m{i}")).collect();
    let r = a
        .call(json!({"op": "msg_summary", "conv_id": conv, "msg_ids": many}))
        .await;
    assert_eq!(code(&r), "bad_request");
    // Unknown ids are left out of a summary.
    let r = a
        .ok(json!({"op": "msg_summary", "conv_id": conv, "msg_ids": ["m_nope", root]}))
        .await;
    assert!(r["messages"]["m_nope"].is_null());
    assert!(r["messages"][&root].is_object());
}

async fn the_root_of_a_thread_is_checked(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let eve = user(&hub, "eve", UserKind::Human, None).await;
    let a = Conn::new(&hub, &alice);
    let b = Conn::new(&hub, &bob);
    let e = Conn::new(&hub, &eve);
    let conv = a.group(&[&bob]).await;
    let other = a.group(&[&bob]).await;
    let root = a.send(&conv, "root").await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    let reply = b.ok(in_thread(&conv, &root, "reply")).await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    let elsewhere = a.send(&other, "elsewhere").await["msg_id"]
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
                json!({"op": "send", "conv_id": conv, "client_msg_id": "sys", "text": "notice"}),
            )
            .unwrap(),
        )
        .await
        .unwrap()["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    let before = head(&a, &conv).await;
    for (thread, rule, c) in [
        ("m_unknown", "thread.root_unknown", "not_found"),
        (elsewhere.as_str(), "thread.root_other_conv", "bad_request"),
        (reply.as_str(), "thread.nested", "bad_request"),
        (gone.as_str(), "thread.root_recalled", "conflict"),
        (sys.as_str(), "thread.root_system", "bad_request"),
    ] {
        let r = b.call(in_thread(&conv, thread, "x")).await;
        assert_eq!((rule_of(&r), code(&r)), (rule, c), "{thread}: {r}");
        // A stream into it is refused the same way (agents stream).
    }
    let r = b
        .call(json!({"op": "send", "conv_id": conv, "client_msg_id": "n", "text": "x", "also_in_conv": true}))
        .await;
    assert_eq!(rule_of(&r), "thread.shown_without_thread", "{r}");
    // Not a member: no thread anywhere.
    let r = e.call(in_thread(&conv, &root, "let me in")).await;
    assert_eq!(code(&r), "forbidden", "{r}");
    for op in [
        json!({"op": "pull_thread", "conv_id": conv, "thread_id": root}),
        json!({"op": "read_thread", "conv_id": conv, "thread_id": root, "seq": 1}),
        json!({"op": "follow_thread", "conv_id": conv, "thread_id": root}),
        json!({"op": "list_threads", "conv_id": conv}),
        json!({"op": "msg_summary", "conv_id": conv, "msg_ids": [root]}),
    ] {
        let r = e.call(op.clone()).await;
        assert_eq!(code(&r), "forbidden", "{op}: {r}");
    }
    // A reply is not a thread to follow or read.
    let r = a
        .call(json!({"op": "follow_thread", "conv_id": conv, "thread_id": reply}))
        .await;
    assert_eq!(rule_of(&r), "thread.unknown", "{r}");
    let r = a
        .call(json!({"op": "pull_thread", "conv_id": conv, "thread_id": elsewhere}))
        .await;
    assert_eq!(code(&r), "not_found", "{r}");
    // Refusals left nothing in the log.
    assert_eq!(head(&a, &conv).await, before);
}

async fn thread_replies_count_and_notify_only_followers(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let carol = user(&hub, "carol", UserKind::Human, None).await;
    let dan = user(&hub, "dan", UserKind::Human, None).await;
    let mut a = Conn::new(&hub, &alice);
    let b = Conn::new(&hub, &bob);
    let mut c = Conn::new(&hub, &carol);
    let mut d = Conn::new(&hub, &dan);
    let conv = a.group(&[&bob, &carol, &dan]).await;
    let root = a.send(&conv, "root").await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    for x in [&a, &c, &d] {
        let h = head(x, &conv).await;
        x.ok(json!({"op": "read", "conv_id": conv, "seq": h})).await;
    }
    a.drain();
    c.drain();
    d.drain();
    b.ok(in_thread(&conv, &root, "a reply")).await;
    // The root's sender follows and is notified; the others get the event,
    // quietly.
    let notified = |frames: Vec<Value>| {
        frames
            .iter()
            .filter(|f| f["type"] == "event" && f["event"]["type"] == "msg.created")
            .map(|f| f["notify"] == true)
            .collect::<Vec<_>>()
    };
    assert_eq!(notified(a.drain()), vec![true]);
    assert_eq!(notified(c.drain()), vec![false]);
    assert_eq!(notified(d.drain()), vec![false]);
    // The conversation's unread count leaves it out; the thread's has it.
    let u = c.ok(json!({"op": "unread"})).await;
    assert_eq!(u["total"], 0, "{u}");
    let threads = a.ok(json!({"op": "list_threads"})).await;
    assert_eq!(
        threads["threads"][0]["thread_id"],
        root.as_str(),
        "{threads}"
    );
    assert_eq!(threads["threads"][0]["unread"], 1);
    assert_eq!(threads["unread_threads"], 1);
    // A mention in a thread notifies the person named, who then follows it.
    b.ok(in_thread(&conv, &root, "@carol what do you think?"))
        .await;
    assert_eq!(notified(c.drain()), vec![true]);
    assert_eq!(notified(d.drain()), vec![false]);
    let ct = c.ok(json!({"op": "list_threads", "conv_id": conv})).await;
    assert_eq!(ct["threads"].as_array().unwrap().len(), 1, "{ct}");
    // Shown in the conversation too: counts and notifies like any message.
    b.ok(json!({"op": "send", "conv_id": conv, "client_msg_id": "shown", "text": "summary for all", "thread_id": root, "also_in_conv": true}))
        .await;
    assert_eq!(notified(d.drain()), vec![true]);
    assert_eq!(d.ok(json!({"op": "unread"})).await["total"], 1);
    // Writing in a thread reads the thread, not the conversation.
    let before = d.ok(json!({"op": "unread"})).await["total"].clone();
    d.ok(in_thread(&conv, &root, "me too")).await;
    assert_eq!(d.ok(json!({"op": "unread"})).await["total"], before);
    let dt = d.ok(json!({"op": "list_threads", "conv_id": conv})).await;
    assert_eq!(dt["threads"][0]["unread"], 0, "{dt}");
    assert_eq!(dt["threads"][0]["following"], true);
    // Muting the conversation silences thread notifications too.
    a.ok(json!({"op": "set_mute", "conv_id": conv, "mode": "all"}))
        .await;
    a.drain();
    b.ok(in_thread(&conv, &root, "muted?")).await;
    assert_eq!(notified(a.drain()), vec![false]);
}

async fn reading_and_following_threads(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let carol = user(&hub, "carol", UserKind::Human, None).await;
    let a = Conn::new(&hub, &alice);
    let mut a2 = Conn::new(&hub, &alice);
    let b = Conn::new(&hub, &bob);
    let mut c = Conn::new(&hub, &carol);
    let conv = a.group(&[&bob, &carol]).await;
    let root = a.send(&conv, "root").await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    let r1 = b.ok(in_thread(&conv, &root, "one")).await;
    let r2 = b.ok(in_thread(&conv, &root, "two")).await;
    a2.drain();
    // Reading a thread: the position, on every device, never past the end.
    let r = a
        .ok(json!({"op": "read_thread", "conv_id": conv, "thread_id": root, "seq": r1["seq"]}))
        .await;
    assert_eq!(r["seq"], r1["seq"]);
    let told = a2.drain();
    assert!(
        told.iter()
            .any(|f| f["type"] == "thread_read" && f["seq"] == r1["seq"]),
        "{told:?}"
    );
    let t = a.ok(json!({"op": "list_threads", "conv_id": conv})).await;
    assert_eq!(t["threads"][0]["unread"], 1);
    let r = a
        .ok(json!({"op": "read_thread", "conv_id": conv, "thread_id": root, "seq": 1_000_000}))
        .await;
    assert_eq!(r["seq"], r2["seq"], "clamped to the latest reply");
    // Going back does not unread.
    let r = a
        .ok(json!({"op": "read_thread", "conv_id": conv, "thread_id": root, "seq": 0}))
        .await;
    assert_eq!(r["seq"], r2["seq"]);
    let t = a.ok(json!({"op": "list_threads", "conv_id": conv})).await;
    assert_eq!(t["threads"][0]["unread"], 0);
    // Following: carol is not a participant, follows, then gets notified.
    let none = c.ok(json!({"op": "list_threads"})).await;
    assert_eq!(none["threads"], json!([]));
    let r = c
        .ok(json!({"op": "unfollow_thread", "conv_id": conv, "thread_id": root}))
        .await;
    assert_eq!(r["changed"], false, "not following yet");
    let r = c
        .ok(json!({"op": "follow_thread", "conv_id": conv, "thread_id": root}))
        .await;
    assert_eq!(r["changed"], true);
    let r = c
        .ok(json!({"op": "follow_thread", "conv_id": conv, "thread_id": root}))
        .await;
    assert_eq!(r["changed"], false, "following twice changes nothing");
    c.drain();
    b.ok(in_thread(&conv, &root, "three")).await;
    assert!(
        c.drain()
            .iter()
            .any(|f| f["event"]["type"] == "msg.created" && f["notify"] == true)
    );
    // Unfollowing: no notifications, out of the list, even when mentioned
    // later the choice stays (the mention itself still notifies).
    c.ok(json!({"op": "unfollow_thread", "conv_id": conv, "thread_id": root}))
        .await;
    b.ok(in_thread(&conv, &root, "four")).await;
    assert!(c.drain().iter().all(|f| f["notify"] != true));
    assert_eq!(
        c.ok(json!({"op": "list_threads"})).await["threads"],
        json!([])
    );
    b.ok(in_thread(&conv, &root, "@carol five")).await;
    assert!(c.drain().iter().any(|f| f["notify"] == true));
    assert_eq!(
        c.ok(json!({"op": "list_threads"})).await["threads"],
        json!([])
    );
    // Replying follows again.
    c.ok(in_thread(&conv, &root, "ok I'm back")).await;
    let t = c.ok(json!({"op": "list_threads"})).await;
    assert_eq!(t["threads"].as_array().unwrap().len(), 1);
    // Paging the list by time; limits.
    let root2 = a.send(&conv, "second root").await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    b.ok(in_thread(&conv, &root2, "x")).await;
    let p1 = a.ok(json!({"op": "list_threads", "limit": 1})).await;
    assert_eq!(p1["threads"][0]["thread_id"], root2.as_str());
    assert_eq!(p1["more"], true);
    let p2 = a
        .ok(json!({"op": "list_threads", "limit": 1, "before": p1["threads"][0]["last_at"]}))
        .await;
    assert_eq!(p2["threads"][0]["thread_id"], root.as_str(), "{p2}");
    for bad in [0, 101] {
        let r = a.call(json!({"op": "list_threads", "limit": bad})).await;
        assert_eq!(code(&r), "bad_request");
    }
    // Unknown thread.
    let r = a
        .call(json!({"op": "read_thread", "conv_id": conv, "thread_id": "m_x", "seq": 1}))
        .await;
    assert_eq!((rule_of(&r), code(&r)), ("thread.unknown", "not_found"));
}

async fn recall_and_retries_in_threads(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let a = Conn::new(&hub, &alice);
    let b = Conn::new(&hub, &bob);
    let conv = a.group(&[&bob]).await;
    let root = a.send(&conv, "root").await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    // A retry is one reply.
    let first = b
        .ok(json!({"op": "send", "conv_id": conv, "client_msg_id": "r", "text": "once", "thread_id": root}))
        .await;
    let again = b
        .ok(json!({"op": "send", "conv_id": conv, "client_msg_id": "r", "text": "once", "thread_id": root}))
        .await;
    assert_eq!(again["duplicate"], true);
    assert_eq!(again["msg_id"], first["msg_id"]);
    let last = b.ok(in_thread(&conv, &root, "last")).await;
    assert_eq!(summary(&a, &conv, &root).await["thread"]["replies"], 2);
    // Recalling the latest reply: one less, the latest is the one before.
    b.ok(json!({"op": "recall", "msg_id": last["msg_id"]}))
        .await;
    let s = summary(&a, &conv, &root).await;
    assert_eq!(s["thread"]["replies"], 1, "{s}");
    assert_eq!(s["thread"]["last_seq"], first["seq"]);
    // Recalling the root: the replies stay, nobody replies any more.
    a.ok(json!({"op": "recall", "msg_id": root})).await;
    let t = a
        .ok(json!({"op": "pull_thread", "conv_id": conv, "thread_id": root}))
        .await;
    assert_eq!(t["root"]["state"], "recalled");
    assert_eq!(t["root"]["content"], "");
    assert_eq!(t["replies"].as_array().unwrap().len(), 2);
    let r = b.call(in_thread(&conv, &root, "too late")).await;
    assert_eq!(rule_of(&r), "thread.root_recalled");
    // A retry of the reply sent before the recall still gets its result.
    let again = b
        .ok(json!({"op": "send", "conv_id": conv, "client_msg_id": "r", "text": "once", "thread_id": root}))
        .await;
    assert_eq!(again["msg_id"], first["msg_id"]);
}

async fn agents_answer_in_the_thread(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    let a = Conn::new(&hub, &alice);
    let b = Conn::new(&hub, &bob);
    let mut g = Conn::new(&hub, &bot);
    let conv = a.group(&[&bob, &bot]).await;
    let noise = b.send(&conv, "unrelated chatter").await;
    let root = b.send(&conv, "who handles refunds?").await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    g.drain();
    // Mentioned in a thread: the agent is woken and sees the message with
    // its thread.
    let q = b
        .ok(in_thread(&conv, &root, "@bot can you check order 42?"))
        .await;
    let woke: Vec<Value> = g
        .drain()
        .into_iter()
        .filter(|f| f["wake"] == true)
        .collect();
    assert_eq!(woke.len(), 1, "{woke:?}");
    assert_eq!(woke[0]["event"]["msg"]["thread_id"], root.as_str());
    // Its context for that thread: the root and the replies only.
    let ctx = g
        .ok(json!({"op": "context", "conv_id": conv, "thread_id": root}))
        .await;
    let texts: Vec<&str> = ctx["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|e| e["event"]["msg"]["content"].as_str())
        .collect();
    assert!(texts.contains(&"who handles refunds?"), "{texts:?}");
    assert!(texts.contains(&"@bot can you check order 42?"));
    assert!(!texts.contains(&"unrelated chatter"), "{noise}");
    // It answers in the thread, streaming.
    let s = g
        .ok(json!({"op": "stream_start", "conv_id": conv, "client_msg_id": "s1", "reply_to": q["msg_id"], "thread_id": root}))
        .await;
    g.ok(json!({"op": "stream_append", "msg_id": s["msg_id"], "idx": 0, "text": "Order 42 is refunded."}))
        .await;
    g.ok(json!({"op": "stream_end", "msg_id": s["msg_id"]}))
        .await;
    let t = a
        .ok(json!({"op": "pull_thread", "conv_id": conv, "thread_id": root}))
        .await;
    let last = t["replies"].as_array().unwrap().last().unwrap().clone();
    assert_eq!(last["content"], "Order 42 is refunded.");
    assert_eq!(last["sender_id"], bot.id.as_str());
    // The agent took part: later replies there reach it without a mention.
    g.drain();
    b.ok(in_thread(&conv, &root, "thanks!")).await;
    assert!(
        g.drain()
            .iter()
            .any(|f| f["event"]["msg"]["content"] == "thanks!"),
        "a thread it follows is visible to it"
    );
    // A draft for approval keeps its thread.
    a.ok(json!({"op": "set_agent_policy", "conv_id": conv, "agent_id": bot.id, "approval": true}))
        .await;
    let d = g
        .ok(json!({"op": "send", "conv_id": conv, "client_msg_id": "d1", "text": "drafted answer", "thread_id": root}))
        .await;
    let draft = d["draft_id"].as_str().unwrap().to_string();
    let replies_before = summary(&a, &conv, &root).await["thread"]["replies"].clone();
    a.ok(json!({"op": "approve", "draft_id": draft})).await;
    let s2 = summary(&a, &conv, &root).await;
    assert_eq!(
        s2["thread"]["replies"].as_i64(),
        replies_before.as_i64().map(|n| n + 1)
    );
    // A draft whose root was recalled meanwhile cannot be approved.
    let d = g
        .ok(json!({"op": "send", "conv_id": conv, "client_msg_id": "d2", "text": "late answer", "thread_id": root}))
        .await;
    b.ok(json!({"op": "recall", "msg_id": root})).await;
    let before = head(&a, &conv).await;
    let r = a
        .call(json!({"op": "approve", "draft_id": d["draft_id"]}))
        .await;
    assert_eq!(
        (rule_of(&r), code(&r)),
        ("thread.root_recalled", "conflict"),
        "{r}"
    );
    assert_eq!(head(&a, &conv).await, before);
    a.ok(json!({"op": "reject", "draft_id": d["draft_id"]}))
        .await;
}

async fn new_members_see_replies_but_not_older_roots(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let carol = user(&hub, "carol", UserKind::Human, None).await;
    let a = Conn::new(&hub, &alice);
    let b = Conn::new(&hub, &bob);
    let c = Conn::new(&hub, &carol);
    let conv = a.group(&[&bob]).await;
    let root = a.send(&conv, "before carol").await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    b.ok(in_thread(&conv, &root, "early reply")).await;
    a.ok(json!({"op": "add_member", "conv_id": conv, "user_id": carol.id, "history": 0}))
        .await;
    let late = b.ok(in_thread(&conv, &root, "after carol joined")).await;
    // Carol sees the later reply; the root and earlier reply are hidden.
    let t = c
        .ok(json!({"op": "pull_thread", "conv_id": conv, "thread_id": root}))
        .await;
    assert!(t["root"].is_null(), "{t}");
    let ids: Vec<&Value> = t["replies"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| &m["msg_id"])
        .collect();
    assert_eq!(ids, vec![&late["msg_id"]]);
    // She can answer there; the thread then lists without its root.
    c.ok(in_thread(&conv, &root, "hello")).await;
    let l = c.ok(json!({"op": "list_threads"})).await;
    assert!(l["threads"][0]["root"].is_null(), "{l}");
    // The summary of a message she cannot see is not hers to have.
    let s = c
        .ok(json!({"op": "msg_summary", "conv_id": conv, "msg_ids": [root]}))
        .await;
    assert!(s["messages"][&root].is_null());
}

async fn threads_survive_a_restart(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let (conv, root, before_a, before_b) = {
        let a = Conn::new(&hub, &alice);
        let b = Conn::new(&hub, &bob);
        let conv = a.group(&[&bob]).await;
        let root = a.send(&conv, "root").await["msg_id"]
            .as_str()
            .unwrap()
            .to_string();
        b.ok(in_thread(&conv, &root, "one")).await;
        b.ok(in_thread(&conv, &root, "two")).await;
        let r = a.ok(json!({"op": "list_threads"})).await;
        b.ok(json!({"op": "unfollow_thread", "conv_id": conv, "thread_id": root}))
            .await;
        let rb = b.ok(json!({"op": "list_threads"})).await;
        (conv, root, r, rb)
    };
    drop(hub);
    let hub = open(single(&db.url)).await;
    let a = Conn::new(&hub, &alice);
    let b = Conn::new(&hub, &bob);
    assert_eq!(
        a.ok(json!({"op": "list_threads"})).await["threads"],
        before_a["threads"]
    );
    assert_eq!(
        b.ok(json!({"op": "list_threads"})).await["threads"],
        before_b["threads"]
    );
    // New replies continue the count.
    b.ok(in_thread(&conv, &root, "three")).await;
    assert_eq!(summary(&a, &conv, &root).await["thread"]["replies"], 3);
}

async fn race_replies_in_one_thread(db: TestDb) {
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
        let hub = hub.clone();
        let people = people.clone();
        let conv = conv.clone();
        let alice = alice.clone();
        async move {
            let mut wrong = 0;
            for round in 0..8 {
                let a = Conn::new(&hub, &alice);
                let root = a.send(&conv, &format!("root {round}")).await["msg_id"]
                    .as_str()
                    .unwrap()
                    .to_string();
                let mut tasks = vec![];
                for p in &people {
                    let (hub, p, conv, root) = (hub.clone(), p.clone(), conv.clone(), root.clone());
                    tasks.push(tokio::spawn(async move {
                        let c = Conn::new(&hub, &p);
                        let mut last = 0;
                        for i in 0..5 {
                            let r = c.ok(in_thread(&conv, &root, &format!("{i}"))).await;
                            last = r["seq"].as_i64().unwrap();
                        }
                        last
                    }));
                }
                let mut max = 0;
                for t in tasks {
                    max = max.max(t.await.unwrap());
                }
                let s = a
                    .ok(json!({"op": "msg_summary", "conv_id": conv, "msg_ids": [root]}))
                    .await["messages"][&root]["thread"]
                    .clone();
                // Exactly one count per reply, the latest is the highest seq.
                if s["replies"] != 30 || s["last_seq"] != max {
                    wrong += 1;
                }
                let t = a
                    .ok(json!({"op": "pull_thread", "conv_id": conv, "thread_id": root, "limit": 200}))
                    .await;
                let seqs: Vec<i64> = t["replies"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|m| m["created_seq"].as_i64().unwrap())
                    .collect();
                assert_eq!(seqs.len(), 30, "every reply is there once");
                assert!(seqs.windows(2).all(|w| w[0] < w[1]), "in log order");
            }
            assert!(mutated || wrong == 0, "{wrong} rounds with a wrong summary");
            wrong
        }
    };
    assert_eq!(check(false).await, 0);
    // Mutation check: without counting replies the summary is wrong.
    let fp = &hub.shared().failpoints;
    fp.set(
        agentim_core::logic::social::MUT_THREAD_COUNT,
        agentim_core::failpoint::Action::Error,
        None,
    );
    assert!(check(true).await > 0, "the mutation must be caught");
    fp.release(agentim_core::logic::social::MUT_THREAD_COUNT);
    // The mutated rounds left wrong summaries behind: the verifier sees them.
    let found = hub.verify(Some(&conv), None).await.unwrap();
    assert!(
        found.iter().any(|f| f.check == "thread.summary"),
        "{found:?}"
    );
}
