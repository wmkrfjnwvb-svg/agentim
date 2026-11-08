//! Threads, reactions and pinned messages on the device
//! (docs/en/design/messaging.md, MSG-19 to MSG-21): sending in a thread
//! (queued like any send), the root's summary, unread counts and the
//! conversation's latest message, reactions added and removed (mine and
//! others'), pins, recalls, erased accounts, the user's other devices, and
//! everything kept across new versions of a message and restarts.

mod common;

use common::*;
use serde_json::{Value, json};

fn msg<'a>(view: &'a [Value], id: &str) -> &'a Value {
    view.iter()
        .find(|m| m["msg_id"] == id)
        .unwrap_or_else(|| panic!("{id} not in {view:?}"))
}

fn root(w: &mut World) -> String {
    w.server.conv("c1");
    w.connected();
    w.cmd(json!({"cmd": "messages", "conv_id": "c1"}));
    w.other_says("c1", "plan for friday?");
    w.run(100);
    w.view("c1").last().unwrap()["msg_id"]
        .as_str()
        .unwrap()
        .to_string()
}

fn unread(w: &mut World) -> Value {
    let l = w.ask(json!({"cmd": "conversations"})).unwrap();
    l["conversations"][0]["unread"].clone()
}

#[test]
fn sending_in_a_thread() {
    let mut w = World::new(1);
    let r = root(&mut w);
    // Shown at once in the thread, sent with it.
    w.cut(1006, "offline");
    let id = w.cmd(json!({"cmd": "send", "conv_id": "c1", "text": "3pm works", "thread_id": r}));
    w.run(10);
    let key = w.reply(id).unwrap().unwrap()["key"]
        .as_str()
        .unwrap()
        .to_string();
    let local = w.view("c1").into_iter().find(|m| m["key"] == key).unwrap();
    assert_eq!(
        (local["thread_id"].as_str(), local["status"].as_str()),
        (Some(r.as_str()), Some("sending"))
    );
    assert!(local.get("thread_shown").is_none());
    w.run(5_000);
    assert_eq!(w.state(), "connected");
    let sent = w
        .server
        .requests
        .iter()
        .find(|q| q["op"] == "send" && q["text"] == "3pm works")
        .unwrap()
        .clone();
    assert_eq!(sent["thread_id"], r.as_str());
    assert!(sent.get("also_in_conv").is_none());
    // Also in the conversation.
    w.cmd(json!({"cmd": "send", "conv_id": "c1", "text": "decided", "thread_id": r, "also_in_conv": true}));
    w.run(300);
    let sent = w
        .server
        .requests
        .iter()
        .find(|q| q["op"] == "send" && q["text"] == "decided")
        .unwrap()
        .clone();
    assert_eq!(sent["also_in_conv"], true);
    let view = w.view("c1");
    let shown = view.iter().find(|m| m["content"] == "decided").unwrap();
    assert_eq!(shown["thread_shown"], true);
    // The root counts my replies and says I follow.
    let t = &msg(&view, &r)["thread"];
    assert_eq!(t["replies"], 2, "{t}");
    assert_eq!(t["following"], true);
    assert_eq!(t["participants"], json!([common::ME]));
    w.assert_converged();
}

#[test]
fn others_replies_update_the_root_and_not_the_conversation() {
    let mut w = World::new(2);
    let r = root(&mut w);
    let before = unread(&mut w);
    w.other_replies("c1", &r, "one", false);
    w.other_replies("c1", &r, "two", false);
    w.run(100);
    let view = w.view("c1");
    let t = &msg(&view, &r)["thread"];
    assert_eq!(t["replies"], 2);
    assert_eq!(t["last_sender"], "u_bob");
    assert!(t["last_seq"].as_i64().unwrap() > 1);
    assert!(t.get("following").is_none(), "not mine");
    // Thread replies don't count, nor become the latest message.
    assert_eq!(unread(&mut w), before);
    let l = w.ask(json!({"cmd": "conversations"})).unwrap();
    assert_eq!(
        l["conversations"][0]["last_message"]["content"],
        "plan for friday?"
    );
    // Shown in the conversation too: it counts.
    w.other_replies("c1", &r, "summary for all", true);
    w.run(100);
    assert_eq!(unread(&mut w).as_i64(), before.as_i64().map(|n| n + 1));
    // A reply whose root isn't here changes nothing else.
    w.other_replies("c1", "m_unknown", "orphan", false);
    w.run(100);
    // A recalled reply leaves the count.
    let reply = w
        .view("c1")
        .iter()
        .find(|m| m["content"] == "one")
        .unwrap()
        .clone();
    let mut gone = w.server.msg(reply["msg_id"].as_str().unwrap());
    gone["state"] = json!("recalled");
    gone["content"] = json!("");
    gone["version"] = json!(2);
    w.push_event("c1", "msg.recalled", json!({}), Some(gone));
    w.run(100);
    assert_eq!(msg(&w.view("c1"), &r)["thread"]["replies"], 2);
    w.assert_converged();
}

#[test]
fn reactions_follow_the_log() {
    let mut w = World::new(3);
    let r = root(&mut w);
    let react = |w: &mut World, kind: &str, user: &str, key: &str| {
        w.push_event(
            "c1",
            kind,
            json!({"msg_id": r, "msg_seq": 1, "key": key, "user_id": user}),
            None,
        );
        w.run(50);
    };
    react(&mut w, "reaction.added", "u_bob", "👍");
    react(&mut w, "reaction.added", common::ME, "👍");
    react(&mut w, "reaction.added", "u_carol", "🎉");
    let rs = msg(&w.view("c1"), &r)["reactions"].clone();
    assert_eq!(
        rs,
        json!([
            {"key": "👍", "count": 2, "me": true, "users": [common::ME, "u_bob"]},
            {"key": "🎉", "count": 1, "me": false, "users": ["u_carol"]},
        ])
    );
    // Removing: mine, then the last of a key.
    react(&mut w, "reaction.removed", common::ME, "👍");
    react(&mut w, "reaction.removed", "u_carol", "🎉");
    react(&mut w, "reaction.removed", "u_dan", "🚀");
    let rs = msg(&w.view("c1"), &r)["reactions"].clone();
    assert_eq!(
        rs,
        json!([{"key": "👍", "count": 1, "me": false, "users": ["u_bob"]}])
    );
    // At most 5 people are kept per key; the count is exact.
    for i in 0..7 {
        react(&mut w, "reaction.added", &format!("u_{i}"), "👀");
    }
    let rs = msg(&w.view("c1"), &r)["reactions"].clone();
    let eyes = rs
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["key"] == "👀")
        .unwrap();
    assert_eq!(eyes["count"], 7);
    assert_eq!(eyes["users"].as_array().unwrap().len(), 5);
    assert_eq!(eyes["users"][0], "u_6", "latest first");
    // A reaction to a message not here: nothing.
    w.push_event(
        "c1",
        "reaction.added",
        json!({"msg_id": "m_nope", "key": "👍", "user_id": "u_bob"}),
        None,
    );
    w.run(50);
    // A new version of the message keeps them.
    let mut edited = w.server.msg(&r);
    edited["content"] = json!("plan for saturday?");
    edited["version"] = json!(2);
    w.push_event("c1", "msg.updated", json!({}), Some(edited));
    w.run(50);
    let m = msg(&w.view("c1"), &r).clone();
    assert_eq!(m["content"], "plan for saturday?");
    assert_eq!(m["reactions"].as_array().unwrap().len(), 2);
    // A restart keeps them too.
    w.restart(4);
    w.cmd(json!({"cmd": "messages", "conv_id": "c1"}));
    w.run(500);
    assert_eq!(
        msg(&w.view("c1"), &r)["reactions"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    w.assert_converged();
}

#[test]
fn pins_recalls_and_erased_accounts() {
    let mut w = World::new(5);
    let r = root(&mut w);
    w.other_says("c1", "rules: be kind");
    w.run(100);
    let rules = w.view("c1").last().unwrap()["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    w.push_event(
        "c1",
        "msg.pinned",
        json!({"msg_id": rules, "msg_seq": 2, "by": common::ME}),
        None,
    );
    w.run(50);
    let p = msg(&w.view("c1"), &rules)["pinned"].clone();
    assert_eq!(p["by"], common::ME);
    assert!(p["seq"].is_i64());
    w.push_event(
        "c1",
        "msg.unpinned",
        json!({"msg_id": rules, "by": common::ME}),
        None,
    );
    w.run(50);
    assert!(msg(&w.view("c1"), &rules)["pinned"].is_null());
    // The policy of who pins.
    w.push_event("c1", "conv.updated", json!({"pin_policy": "admins"}), None);
    w.push_event("c1", "conv.updated", json!({"title": "renamed"}), None);
    w.run(50);
    let l = w.ask(json!({"cmd": "conversations"})).unwrap();
    assert_eq!(l["conversations"][0]["pin_policy"], "admins");
    // A recall takes the reactions and the pin.
    w.push_event(
        "c1",
        "msg.pinned",
        json!({"msg_id": rules, "by": common::ME}),
        None,
    );
    w.push_event(
        "c1",
        "reaction.added",
        json!({"msg_id": rules, "key": "👍", "user_id": "u_bob"}),
        None,
    );
    w.run(50);
    let mut gone = w.server.msg(&rules);
    gone["state"] = json!("recalled");
    gone["content"] = json!("");
    gone["version"] = json!(2);
    w.push_event("c1", "msg.recalled", json!({}), Some(gone));
    w.run(50);
    let m = msg(&w.view("c1"), &rules).clone();
    assert!(m["pinned"].is_null() && m["reactions"].is_null(), "{m}");
    // An erased account: its reactions go, and the pins and reactions of
    // its messages.
    for (u, k) in [("u_bob", "👍"), ("u_carol", "👍"), ("u_bob", "🎉")] {
        w.push_event(
            "c1",
            "reaction.added",
            json!({"msg_id": r, "key": k, "user_id": u}),
            None,
        );
    }
    w.push_event(
        "c1",
        "msg.pinned",
        json!({"msg_id": r, "by": "u_carol"}),
        None,
    );
    w.other_says("c1", "carol's own words");
    w.run(50);
    w.push_event("c1", "user.erased", json!({"user_id": "u_carol"}), None);
    w.push_event("c1", "user.erased", json!({}), None);
    w.run(50);
    let m = msg(&w.view("c1"), &r).clone();
    assert_eq!(
        m["reactions"],
        json!([{"key": "👍", "count": 1, "me": false, "users": ["u_bob"]}, {"key": "🎉", "count": 1, "me": false, "users": ["u_bob"]}]),
        "{m}"
    );
    assert_eq!(
        m["pinned"]["by"], "u_carol",
        "a pin of someone else's message stays"
    );
    w.push_event("c1", "user.erased", json!({"user_id": "u_bob"}), None);
    w.run(50);
    let m = msg(&w.view("c1"), &r).clone();
    assert!(
        m["reactions"].is_null() && m["pinned"].is_null(),
        "bob wrote the root: {m}"
    );
    // (Not compared with the test server: it does not erase accounts.)
}

#[test]
fn threads_read_and_followed_on_another_device() {
    let mut w = World::new(6);
    let r = root(&mut w);
    w.other_replies("c1", &r, "one", false);
    w.run(50);
    w.deliver(vec![
        json!({"type": "thread_read", "conv_id": "c1", "thread_id": r, "seq": 2}).to_string(),
    ]);
    w.deliver(vec![
        json!({"type": "thread_follow", "conv_id": "c1", "thread_id": r, "following": false})
            .to_string(),
    ]);
    // Missing ids or an unknown thread: ignored.
    w.deliver(vec![
        json!({"type": "thread_read", "conv_id": "c1"}).to_string(),
    ]);
    w.deliver(vec![
        json!({"type": "thread_read", "conv_id": "c1", "thread_id": "m_x", "seq": 1}).to_string(),
    ]);
    w.run(50);
    let t = msg(&w.view("c1"), &r)["thread"].clone();
    assert_eq!(
        (t["read_seq"].clone(), t["following"].clone()),
        (json!(2), json!(false))
    );
    assert_eq!(w.emitted("thread_read").len(), 2);
    assert_eq!(w.emitted("thread_follow").len(), 1);
}
