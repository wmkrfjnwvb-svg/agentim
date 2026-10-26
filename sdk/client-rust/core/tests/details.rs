//! The remaining rules, one by one: frames the server pushes (signals,
//! side channel, streaming deltas, own reads and mutes, unknown frames,
//! garbage), events that change what is shown (erased accounts, renamed
//! or left conversations, resets), heads, hello and sync failures, the
//! store that cannot write at all, logout, the smaller commands and their
//! wrong arguments.

mod common;

use agentim_client_core::*;
use common::*;
use serde_json::{Value, json};

fn frame(w: &mut World, v: Value) {
    w.deliver(vec![v.to_string()]);
    w.run(100);
}

#[test]
fn pushed_frames_reach_the_app() {
    let mut w = World::new(1);
    w.server.conv("c1");
    w.connected();
    w.cmd(json!({"cmd": "messages", "conv_id": "c1"}));
    frame(
        &mut w,
        json!({"type": "signal", "conv_id": "c1", "kind": "typing", "from": "u_bob"}),
    );
    assert_eq!(w.emitted("signal").len(), 1);
    // The side channel is shown and acknowledged.
    let acks = w.sent_ops("ack");
    frame(
        &mut w,
        json!({"type": "side", "event": {"conv_id": "c1", "side_seq": 4, "type": "whisper"}}),
    );
    assert_eq!(w.emitted("side").len(), 1);
    assert_eq!(w.sent_ops("ack"), acks + 1);
    // Read and mute on another device of the user.
    frame(&mut w, json!({"type": "read", "conv_id": "c1", "seq": 1}));
    frame(
        &mut w,
        json!({"type": "mute", "conv_id": "c1", "mode": "all", "until": null}),
    );
    let list = w.ask(json!({"cmd": "conversations"})).unwrap();
    let c = &list["conversations"][0];
    assert_eq!(
        (c["unread"].clone(), c["mute"]["mode"].clone()),
        (json!(0), json!("all"))
    );
    // A newer server's frame: ignored. Garbage: the connection is replaced.
    frame(&mut w, json!({"type": "something_new"}));
    assert_eq!(w.connects.len(), 1);
    w.deliver(vec!["{not json".into()]);
    w.run(200);
    assert_eq!(w.connects.len(), 2);
    assert_eq!(w.state(), "connected");
    // An answer nobody asked for, or without an id: ignored.
    frame(&mut w, json!({"type": "res", "rid": 999_999, "ok": true}));
    frame(&mut w, json!({"type": "res", "ok": true}));
    assert_eq!(w.state(), "connected");
}

#[test]
fn streamed_replies_grow_until_final() {
    let mut w = World::new(1);
    w.server.conv("c1");
    w.connected();
    w.cmd(json!({"cmd": "messages", "conv_id": "c1"}));
    let seq = w.server.last("c1") + 1;
    let m = json!({"msg_id": "ms", "conv_id": "c1", "created_seq": seq, "sender_id": "u_bot", "sender_type": "agent",
                   "content": "", "version": 1, "state": "streaming", "created_at": 1});
    let ev = json!({"conv_id": "c1", "seq": seq, "type": "msg.created", "msg": m});
    w.server.logs.get_mut("c1").unwrap().push(ev.clone());
    w.server.sent.insert("c1".into(), seq);
    frame(&mut w, json!({"type": "event", "event": ev}));
    for (t, reset) in [
        ("Hel", false),
        ("lo", false),
        ("Hi", true),
        (" there", false),
    ] {
        frame(
            &mut w,
            json!({"type": "delta", "conv_id": "c1", "msg_id": "ms", "text": t, "reset": reset}),
        );
    }
    let v = w.view("c1");
    assert_eq!(v[0]["streamed"], "Hi there");
    // A newer version while still streaming keeps what was streamed.
    let mut m2 = m.clone();
    m2["version"] = json!(2);
    m2["content"] = json!("Hi");
    let evs = json!({"conv_id": "c1", "seq": seq + 1, "type": "msg.stream_progress", "msg": m2});
    w.server.logs.get_mut("c1").unwrap().push(evs.clone());
    w.server.sent.insert("c1".into(), seq + 1);
    frame(&mut w, json!({"type": "event", "event": evs}));
    assert_eq!(w.view("c1")[0]["streamed"], "Hi there");
    let seq = seq + 1;
    let mut m = m.clone();
    m["version"] = json!(2);
    // Deltas for unknown or finished messages: nothing.
    frame(
        &mut w,
        json!({"type": "delta", "conv_id": "c9", "msg_id": "ms", "text": "x"}),
    );
    frame(
        &mut w,
        json!({"type": "delta", "conv_id": "c1", "msg_id": "nope", "text": "x"}),
    );
    frame(&mut w, json!({"type": "delta"}));
    let mut fin = m.clone();
    fin["state"] = json!("committed");
    fin["content"] = json!("Hi there!");
    fin["version"] = json!(3);
    let ev2 = json!({"conv_id": "c1", "seq": seq + 1, "type": "msg.stream_ended", "msg": fin});
    w.server.logs.get_mut("c1").unwrap().push(ev2.clone());
    w.server.sent.insert("c1".into(), seq + 1);
    frame(&mut w, json!({"type": "event", "event": ev2}));
    frame(
        &mut w,
        json!({"type": "delta", "conv_id": "c1", "msg_id": "ms", "text": "late"}),
    );
    let v = w.view("c1");
    assert_eq!(v[0]["content"], "Hi there!");
    assert!(v[0].get("streamed").is_none_or(|s| s.is_null()));
    // An older version arriving late never replaces the newer one.
    let mut old = m.clone();
    old["content"] = json!("stale");
    let ev3 = json!({"conv_id": "c1", "seq": seq + 2, "type": "msg.edited", "msg": old});
    w.server.logs.get_mut("c1").unwrap().push(ev3.clone());
    w.server.sent.insert("c1".into(), seq + 2);
    frame(&mut w, json!({"type": "event", "event": ev3}));
    assert_eq!(w.view("c1")[0]["content"], "Hi there!");
}

#[test]
fn events_that_change_what_is_shown() {
    let mut w = World::new(1);
    w.server.conv("c1");
    w.connected();
    w.cmd(json!({"cmd": "messages", "conv_id": "c1"}));
    w.other_says("c1", "from bob");
    w.run(100);
    // Bob's account is deleted: his messages are gone here too.
    let seq = w.server.last("c1") + 1;
    let ev =
        json!({"conv_id": "c1", "seq": seq, "type": "user.erased", "data": {"user_id": "u_bob"}});
    w.server.logs.get_mut("c1").unwrap().push(ev.clone());
    w.server.sent.insert("c1".into(), seq);
    frame(&mut w, json!({"type": "event", "event": ev}));
    let v = w.view("c1");
    assert_eq!(
        (v[0]["content"].as_str(), v[0]["state_reason"].as_str()),
        (Some(""), Some("account_deleted"))
    );
    // Renamed.
    let ev = json!({"conv_id": "c1", "seq": seq + 1, "type": "conv.updated", "data": {"title": "New name"}});
    w.server.logs.get_mut("c1").unwrap().push(ev.clone());
    w.server.sent.insert("c1".into(), seq + 1);
    frame(&mut w, json!({"type": "event", "event": ev}));
    let list = w.ask(json!({"cmd": "conversations"})).unwrap();
    assert_eq!(list["conversations"][0]["title"], "New name");
    assert_eq!(list["conversations"][0]["unread"], 1);
}

#[test]
fn member_lists_follow_joins_and_departures() {
    let mut w = World::new(1);
    w.server.conv("c1");
    w.server.members.insert("c1".into(), json!([{"user_id": "u_me", "name": "me", "kind": "human", "role": "owner", "approval": false}]));
    w.server.conv("c2");
    w.connected();
    let members = |w: &mut World, c: &str| -> Vec<String> {
        let list = w.ask(json!({"cmd": "conversations"})).unwrap();
        let conv = list["conversations"]
            .as_array()
            .unwrap()
            .iter()
            .find(|x| x["conv_id"] == c)
            .unwrap()
            .clone();
        conv["members"]
            .as_array()
            .map(|m| {
                m.iter()
                    .map(|x| x["user_id"].as_str().unwrap().to_string())
                    .collect()
            })
            .unwrap_or_default()
    };
    let push = |w: &mut World, c: &str, kind: &str, data: Value| {
        let seq = w.server.last(c) + 1;
        let ev = json!({"conv_id": c, "seq": seq, "type": kind, "data": data});
        w.server.logs.get_mut(c).unwrap().push(ev.clone());
        w.server.sent.insert(c.into(), seq);
        frame(w, json!({"type": "event", "event": ev}));
    };
    assert_eq!(members(&mut w, "c1"), ["u_me"]);
    // A batch of joins (D128) is one change; one join is another.
    let batch: Vec<Value> = (0..3).map(|i| json!({"user_id": format!("u_{i}"), "name": format!("p{i}"), "kind": "human", "role": "member", "trigger": "all"})).collect();
    let updates = w.emitted("conversations").len();
    push(&mut w, "c1", "members.joined", json!({"members": batch}));
    assert_eq!(
        w.emitted("conversations").len(),
        updates + 1,
        "one list update for the batch"
    );
    push(
        &mut w,
        "c1",
        "member.joined",
        json!({"user_id": "u_agent", "name": "bot", "kind": "agent", "role": "member", "trigger": "addressed"}),
    );
    assert_eq!(
        members(&mut w, "c1"),
        ["u_me", "u_0", "u_1", "u_2", "u_agent"]
    );
    // Joining again (a re-added member) replaces, never duplicates.
    push(
        &mut w,
        "c1",
        "member.joined",
        json!({"user_id": "u_1", "name": "p1", "kind": "human", "role": "admin", "trigger": "all"}),
    );
    let list = w.ask(json!({"cmd": "conversations"})).unwrap();
    let c1 = list["conversations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["conv_id"] == "c1")
        .unwrap()
        .clone();
    assert_eq!(c1["members"].as_array().unwrap().len(), 5);
    assert_eq!(c1["members"][2]["role"], "admin");
    // Left and removed members go; an unknown one changes nothing.
    push(&mut w, "c1", "member.left", json!({"user_id": "u_0"}));
    push(
        &mut w,
        "c1",
        "member.removed",
        json!({"user_id": "u_agent"}),
    );
    push(
        &mut w,
        "c1",
        "member.removed",
        json!({"user_id": "u_nobody"}),
    );
    assert_eq!(members(&mut w, "c1"), ["u_me", "u_1", "u_2"]);
    // Without a member list yet (none loaded): nothing invented; an empty
    // batch or one without ids changes nothing either.
    push(
        &mut w,
        "c2",
        "members.joined",
        json!({"members": [{"user_id": "u_9"}]}),
    );
    assert!(members(&mut w, "c2").is_empty());
    push(&mut w, "c1", "members.joined", json!({"members": []}));
    push(
        &mut w,
        "c1",
        "members.joined",
        json!({"members": [{"name": "no id"}]}),
    );
    push(&mut w, "c1", "members.joined", json!({}));
    assert_eq!(members(&mut w, "c1"), ["u_me", "u_1", "u_2"]);
    // Every one of these events still reaches the app as an event.
    assert!(
        w.emitted("event")
            .iter()
            .any(|e| e["event"]["type"] == "members.joined")
    );
}

#[test]
fn resets_and_departures_from_sync() {
    let mut w = World::new(1);
    w.server.conv("c1");
    w.server.conv("c2");
    w.connected();
    w.cmd(json!({"cmd": "messages", "conv_id": "c1"}));
    // The device was too far behind in c1: it goes on from seq 50; it left c2.
    for i in 0..60 {
        w.server.other_says("c1", &format!("x{i}"));
    }
    let tail: Vec<Value> = w.server.logs["c1"][49..]
        .iter()
        .map(|e| json!({"event": e}))
        .collect();
    let resp = json!({"convs": [
        {"conv_id": "c1", "reset": {"from_seq": 50}, "events": tail, "upto": w.server.last("c1")},
        {"conv_id": "c2", "left": true, "events": [], "upto": 1, "side_events": [{"conv_id": "c2", "side_seq": 1, "type": "whisper"}]},
    ], "more": false});
    w.server.sent.insert("c1".into(), w.server.last("c1"));
    let fx = w
        .core
        .handle(Input::Frame(json!({"type": "resync"}).to_string()), w.now);
    let rid = fx
        .iter()
        .find_map(|e| match e {
            Effect::Send(t) if t.contains("\"sync\"") => serde_json::from_str::<Value>(t)
                .ok()
                .map(|v| v["rid"].clone()),
            _ => None,
        })
        .expect("a sync");
    let mut answer = resp.clone();
    answer["type"] = json!("res");
    answer["rid"] = rid;
    answer["ok"] = json!(true);
    w.input(Input::Frame(answer.to_string()));
    assert!(
        w.emitted("reset")
            .iter()
            .any(|e| e["conv_id"] == "c1" && e["from_seq"] == 50)
    );
    let list = w.ask(json!({"cmd": "conversations"})).unwrap();
    let ids: Vec<&str> = list["conversations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["conv_id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["c1"], "c2 was left");
    let d = w.core.debug_state();
    assert_eq!(
        d["convs"][0]["last_seq"].as_i64().unwrap(),
        w.server.last("c1")
    );
}

#[test]
fn heads_ahead_after_a_sync_replace_the_connection() {
    let mut w = World::new(1);
    w.server.conv("c1");
    w.connected();
    // The server reports more than it then delivers (a stale node).
    let fx = w.core.handle(Input::Tick, w.now + HEADS_EVERY_MS + 1);
    w.now += HEADS_EVERY_MS + 1;
    let rid = fx
        .iter()
        .find_map(|e| match e {
            Effect::Send(t) if t.contains("\"heads\"") => serde_json::from_str::<Value>(t)
                .ok()
                .map(|v| v["rid"].clone()),
            _ => None,
        })
        .expect("a heads check");
    w.input(Input::Frame(
        json!({"type": "res", "rid": rid, "ok": true, "heads": {"c1": 999}}).to_string(),
    ));
    w.run(500);
    assert!(
        w.emitted("connection")
            .iter()
            .any(|e| e["reason"] == "behind")
    );
    assert_eq!(w.state(), "connected");
    // Nothing to check: no request.
    let mut w = World::new(1);
    w.connected();
    let n = w.sent_ops("heads");
    w.run(HEADS_EVERY_MS + 1_000);
    assert_eq!(w.sent_ops("heads"), n);
}

#[test]
fn hello_and_sync_failures() {
    // An old SDK: failed, no retry.
    let mut w = World::new(1);
    w.server
        .reject
        .entry("hello".into())
        .or_default()
        .push_back(json!({"code": "client_too_old", "message": "upgrade"}));
    w.start();
    w.cmd(json!({"cmd": "connect"}));
    w.run(10_000);
    assert_eq!(
        (w.state(), w.core.debug_state()["reason"].as_str()),
        ("failed", Some("upgrade_required"))
    );
    // Another hello failure, or a sync refused: retried.
    let mut w = World::new(1);
    w.server
        .reject
        .entry("hello".into())
        .or_default()
        .push_back(json!({"code": "internal", "message": "x"}));
    w.server.reject.entry("sync".into()).or_default().extend([
        json!({"code": "rate_limited", "message": "slow"}),
        json!({"code": "storage_unavailable", "message": "db"}),
    ]);
    w.start();
    w.cmd(json!({"cmd": "connect"}));
    w.run(60_000);
    assert_eq!(w.state(), "connected");
    assert!(w.connects.len() >= 4);
}

#[test]
fn a_store_that_cannot_write_keeps_the_app_working_in_memory() {
    let mut w = World::new(1);
    w.server.conv("c1");
    w.start();
    w.input(Input::StorageFailed {
        kind: "unavailable".into(),
    });
    assert!(
        w.emitted("storage")
            .iter()
            .any(|e| e["state"] == "memory_only")
    );
    w.cmd(json!({"cmd": "connect"}));
    w.run(1_000);
    let writes = w.store.writes;
    w.ask(json!({"cmd": "send", "conv_id": "c1", "text": "still works"}))
        .unwrap();
    w.run(1_000);
    assert_eq!(w.store.writes, writes, "nothing written");
    assert_eq!(w.server.applied_cms.len(), 1);
    assert!(w.server.acked["c1"] >= 1, "acknowledged without a store");
    assert_eq!(
        w.ask(json!({"cmd": "state"})).unwrap()["storage"],
        "memory_only"
    );
}

#[test]
fn logout_wipes_everything() {
    let mut w = World::new(1);
    w.server.conv("c1");
    w.connected();
    w.cmd(json!({"cmd": "messages", "conv_id": "c1"}));
    w.ask(json!({"cmd": "close"})).unwrap();
    w.ask(json!({"cmd": "send", "conv_id": "c1", "text": "never"}))
        .unwrap();
    w.ask(json!({"cmd": "logout"})).unwrap();
    assert!(w.store.convs.is_empty() && w.store.ops.is_empty());
    assert!(
        w.emitted("conversations")
            .iter()
            .any(|e| e["reset"] == true)
    );
    assert!(w.emitted("messages").iter().any(|e| e["reset"] == true));
    assert_eq!(w.ask(json!({"cmd": "pending"})).unwrap()["ops"], json!([]));
}

#[test]
fn the_smaller_commands() {
    let mut w = World::new(1);
    w.server.conv("c1");
    w.connected();
    w.cmd(json!({"cmd": "messages", "conv_id": "c1"}));
    // Feedback: the last one wins; respond: already answered is done.
    w.ask(json!({"cmd": "close"})).unwrap();
    w.other_says("c1", "rate me");
    w.ask(json!({"cmd": "feedback", "conv_id": "c1", "msg_id": "m1", "rating": "up"}))
        .unwrap();
    w.ask(json!({"cmd": "feedback", "conv_id": "c1", "msg_id": "m1", "rating": "down", "reason": "wrong"})).unwrap();
    w.ask(json!({"cmd": "respond", "conv_id": "c1", "msg_id": "m1", "block_id": "b", "action": "yes", "values": {"a": 1}})).unwrap();
    w.server
        .reject
        .entry("respond".into())
        .or_default()
        .push_back(json!({"code": "conflict", "message": "answered"}));
    w.cmd(json!({"cmd": "connect"}));
    w.run(1_000);
    let fb: Vec<&Value> = w
        .server
        .requests
        .iter()
        .filter(|r| r["op"] == "feedback")
        .collect();
    assert_eq!(fb.len(), 1);
    assert_eq!(
        (fb[0]["rating"].as_str(), fb[0]["reason"].as_str()),
        (Some("down"), Some("wrong"))
    );
    assert!(
        w.emitted("operation")
            .iter()
            .any(|e| e["kind"] == "respond" && e["result"] == "done")
    );
    // A refused feedback: failed, removed.
    w.server
        .reject
        .entry("feedback".into())
        .or_default()
        .push_back(json!({"code": "forbidden", "message": "no"}));
    w.ask(json!({"cmd": "feedback", "conv_id": "c1", "msg_id": "m1", "rating": "up"}))
        .unwrap();
    w.run(500);
    assert!(
        w.emitted("operation")
            .iter()
            .any(|e| e["kind"] == "feedback" && e["result"] == "failed")
    );
    // Sends with kind, payload and reply; edit by client_msg_id once sent.
    w.ask(json!({"cmd": "send", "conv_id": "c1", "text": "card", "kind": "x.card", "payload": {"a": 1}, "reply_to": "m1", "client_msg_id": "k"})).unwrap();
    w.run(500);
    let s = w
        .server
        .requests
        .iter()
        .rev()
        .find(|r| r["op"] == "send")
        .unwrap()
        .clone();
    assert_eq!(
        (s["kind"].as_str(), s["reply_to"].as_str()),
        (Some("x.card"), Some("m1"))
    );
    w.ask(json!({"cmd": "edit", "conv_id": "c1", "client_msg_id": "k", "text": "card 2"}))
        .unwrap();
    w.run(500);
    let id = w.server.cms["k"].clone();
    assert_eq!(w.server.cur[&id]["content"], "card 2");
    // Focus (sent now and after reconnects), unwatch, state, debug.
    w.ask(json!({"cmd": "focus", "conv_id": "c1"})).unwrap();
    assert_eq!(w.sent_ops("focus"), 1);
    w.cut(1006, "");
    w.run(500);
    assert_eq!(w.sent_ops("focus"), 2);
    w.ask(json!({"cmd": "unwatch", "conv_id": "c1"})).unwrap();
    w.ask(json!({"cmd": "unwatch"})).unwrap();
    let n = w.events.iter().filter(|e| e["type"] == "messages").count();
    w.other_says("c1", "unwatched");
    w.run(200);
    assert_eq!(
        w.events.iter().filter(|e| e["type"] == "messages").count(),
        n
    );
    assert_eq!(
        w.ask(json!({"cmd": "state"})).unwrap()["state"],
        "connected"
    );
    let d = w.ask(json!({"cmd": "debug_state"})).unwrap();
    assert!(d["recent_failures"].as_array().unwrap().len() <= 20);
    // Connect while connected: nothing new.
    let n = w.connects.len();
    w.ask(json!({"cmd": "connect"})).unwrap();
    w.run(200);
    assert_eq!(w.connects.len(), n);
}

#[test]
fn wrong_arguments_are_bad_requests() {
    let mut w = World::new(1);
    w.start();
    for c in [
        json!({"cmd": "messages"}),
        json!({"cmd": "read"}),
        json!({"cmd": "feedback", "conv_id": "c1"}),
        json!({"cmd": "respond", "conv_id": "c1", "msg_id": "m"}),
        json!({"cmd": "retry", "conv_id": "c1"}),
        json!({"cmd": "discard", "conv_id": "c1"}),
        json!({"cmd": "recall", "conv_id": "c1"}),
        json!({"cmd": "call"}),
        json!({"cmd": "load_older"}),
        json!({"cmd": "jump_to", "conv_id": "c1"}),
        json!({"cmd": "jump_to", "conv_id": "c1", "seq": -1}),
    ] {
        assert_eq!(w.ask(c.clone()).unwrap_err().code, "bad_request", "{c}");
    }
    assert_eq!(
        w.ask(json!({"cmd": "retry", "conv_id": "c1", "client_msg_id": "x"}))
            .unwrap_err()
            .code,
        "not_found"
    );
    assert_eq!(
        w.ask(json!({"cmd": "jump_to", "conv_id": "c1", "seq": 5}))
            .unwrap_err()
            .code,
        "disconnected"
    );
}

#[test]
fn retrying_after_a_week_gets_a_new_id() {
    let mut w = World::new(1);
    w.server.conv("c1");
    w.server.forbidden.insert("c1".into());
    w.connected();
    w.cmd(json!({"cmd": "messages", "conv_id": "c1"}));
    w.ask(json!({"cmd": "send", "conv_id": "c1", "text": "x", "client_msg_id": "old"}))
        .unwrap();
    w.run(500);
    w.server.forbidden.clear();
    w.now += DEDUP_MS + 1;
    let r = w
        .ask(json!({"cmd": "retry", "conv_id": "c1", "client_msg_id": "old"}))
        .unwrap();
    assert_ne!(r["key"], "local:old");
    w.run(500);
    assert_eq!(w.server.applied_cms.len(), 1);
    assert_ne!(w.server.applied_cms[0], "old");
    // Discarding while in flight is refused.
    w.lose_answers = 1;
    w.ask(json!({"cmd": "send", "conv_id": "c1", "text": "y", "client_msg_id": "fly"}))
        .unwrap();
    assert_eq!(
        w.ask(json!({"cmd": "discard", "conv_id": "c1", "client_msg_id": "fly"}))
            .unwrap_err()
            .code,
        "conflict"
    );
}

#[test]
fn history_edges() {
    let mut w = World::new(1);
    w.server.conv("c1");
    for i in 0..400 {
        w.other_says("c1", &format!("m{i}"));
    }
    w.connected();
    w.cmd(json!({"cmd": "messages", "conv_id": "c1"}));
    // Twice at once: the second is refused.
    let a = w.cmd(json!({"cmd": "load_older", "conv_id": "c1"}));
    let b = w.ask(json!({"cmd": "load_older", "conv_id": "c1"}));
    assert_eq!(b.unwrap_err().code, "conflict");
    w.run(200);
    assert!(w.reply(a).unwrap().is_ok());
    // A refused page is reported; offline only the cache.
    w.server
        .reject
        .entry("history".into())
        .or_default()
        .push_back(json!({"code": "rate_limited", "message": "slow"}));
    let a = w.cmd(json!({"cmd": "load_older", "conv_id": "c1"}));
    w.run(200);
    assert_eq!(w.reply(a).unwrap().unwrap_err().code, "rate_limited");
    w.ask(json!({"cmd": "close"})).unwrap();
    let a = w.cmd(json!({"cmd": "load_older", "conv_id": "c1"}));
    w.run(200);
    assert_eq!(w.reply(a).unwrap().unwrap()["offline"], true);
    // Lost on the way: the connection drops and the request is answered.
    w.cmd(json!({"cmd": "connect"}));
    w.run(500);
    w.lose_answers = 1;
    let a = w.cmd(json!({"cmd": "load_older", "conv_id": "c1"}));
    let j = w.cmd(json!({"cmd": "jump_to", "conv_id": "c1", "seq": 3}));
    w.cut(1006, "");
    w.run(200);
    assert_eq!(w.reply(a).unwrap().unwrap_err().code, "disconnected");
    assert!(w.reply(j).is_some());
    // Unanswered: a timeout.
    w.lose_answers = 1;
    let a = w.cmd(json!({"cmd": "load_older", "conv_id": "c1"}));
    w.run(31_000);
    assert_eq!(w.reply(a).unwrap().unwrap_err().code, "timeout");
    // To the start, then nothing more; a cached jump.
    loop {
        let a = w.cmd(json!({"cmd": "load_older", "conv_id": "c1"}));
        w.run(200);
        if w.reply(a).unwrap().unwrap()["has_older"] == false {
            break;
        }
    }
    let r = w
        .ask(json!({"cmd": "load_older", "conv_id": "c1"}))
        .unwrap();
    assert_eq!(r["older_end"], "start");
    let r = w
        .ask(json!({"cmd": "jump_to", "conv_id": "c1", "seq": 10}))
        .unwrap();
    assert_eq!(r["cached"], true);
    // A page answer for a request nobody made: ignored.
    w.input(Input::PageLoaded {
        req: 999,
        conv_id: "c1".into(),
        messages: vec![],
    });
}

#[test]
fn urls_and_odd_inputs() {
    for (url, want) in [("http://h:1/", "ws://h:1/ws?"), ("wss://h", "wss://h/ws?")] {
        let mut w = World::new(1);
        w.core = Core::new(Config {
            url: url.into(),
            device_id: "d d".into(),
            device_name: None,
            platform: Some("iOS".into()),
            client_name: "t".into(),
            client_version: "1".into(),
            seed: 1,
        });
        w.connected();
        assert!(w.connects[0].starts_with(want), "{}", w.connects[0]);
        assert!(w.connects[0].contains("device=d%20d&platform=iOS"));
    }
    // A config from JSON takes defaults.
    let c: Config = serde_json::from_value(json!({"url": "https://x", "device_id": "d"})).unwrap();
    assert_eq!(c.client_name, "agentim-client");
    assert!(!c.client_version.is_empty());
    let mut w = World::new(1);
    w.start();
    // A token nobody asked for, a close while down, background: nothing.
    w.input(Input::Token(Ok("t".into())));
    w.input(Input::Closed {
        code: 1006,
        reason: String::new(),
    });
    w.input(Input::ConnectFailed {
        status: Some(500),
        retry_after_ms: None,
        detail: "x".into(),
    });
    w.input(Input::Foreground(false));
    w.input(Input::StorageRecovered);
    w.input(Input::Persisted(42));
    assert_eq!(w.state(), "closed");
    // A server error status: retried.
    w.plan.push_back(Outcome::Refuse(500, None));
    w.cmd(json!({"cmd": "connect"}));
    w.run(100);
    assert_eq!(w.state(), "disconnected");
    // Offline while waiting for the token; back in the foreground retries.
    w.input(Input::Network {
        online: false,
        generation: 1,
    });
    w.run(60_000);
    assert_eq!(w.state(), "suspended");
    w.input(Input::Network {
        online: true,
        generation: 1,
    });
    w.run(1_000);
    assert_eq!(w.state(), "connected");
    w.cut(1006, "");
    w.plan.extend([Outcome::Unreachable, Outcome::Unreachable]);
    w.run(50);
    w.input(Input::Foreground(true));
    w.run(1_000);
    assert_eq!(w.state(), "connected");
}

#[test]
fn labels_longer_than_the_server_takes_are_cut() {
    // The server refuses labels over 64 bytes (a platform string such as
    // macOS's kernel description is longer): cut at a character, empty left out.
    let mut w = World::new(1);
    w.core = Core::new(Config {
        url: "https://h".into(),
        device_id: "d".into(),
        device_name: Some(String::new()),
        platform: Some("é".repeat(40)),
        client_name: "n".repeat(70),
        client_version: "v".repeat(64),
        seed: 1,
    });
    w.connected();
    let url = &w.connects[0];
    assert!(!url.contains("device_name"), "{url}");
    let p = url.split("&platform=").nth(1).unwrap();
    assert_eq!(p, "%C3%A9".repeat(32), "64 bytes, whole characters");
    let hello = w
        .server
        .requests
        .iter()
        .find(|r| r["op"] == "hello")
        .unwrap();
    assert_eq!(hello["client"]["name"], "n".repeat(64));
    assert_eq!(hello["client"]["version"], "v".repeat(64));
    assert_eq!(w.state(), "connected");
}

#[test]
fn more_connection_edges() {
    // A request in flight when the connection goes: answered `disconnected`.
    let mut w = World::new(1);
    w.connected();
    w.lose_answers = 1;
    let id = w.cmd(json!({"cmd": "call", "op": "ping"}));
    w.cut(1006, "");
    w.run(100);
    assert_eq!(w.reply(id).unwrap().unwrap_err().code, "disconnected");
    // A server error answer comes back as is.
    let id = w.cmd(json!({"cmd": "call", "op": "no_such_op"}));
    w.run(100);
    assert_eq!(w.reply(id).unwrap().unwrap_err().code, "unknown_op");
    // goaway with no wait: back at once.
    w.deliver(vec![
        json!({"type": "goaway", "retry_after_ms": 0}).to_string(),
    ]);
    w.run(50);
    let n = w.connects.len();
    w.cut(1012, "server_restart");
    w.run(100);
    assert_eq!(w.connects.len(), n + 1);
    // A second welcome on a live connection: ignored.
    w.deliver(vec![w.server.welcome()]);
    w.run(100);
    assert_eq!(w.state(), "connected");
    // Back in the foreground while connecting: nothing extra.
    w.cut(1006, "");
    w.plan.push_back(Outcome::Hang);
    w.run(30);
    w.input(Input::Foreground(true));
    w.run(20_000);
    assert_eq!(w.state(), "connected");
}

#[test]
fn the_retry_budget_pauses_the_queue_then_resumes() {
    let mut w = World::new(1);
    w.server.conv("c1");
    w.connected();
    for _ in 0..8 {
        w.server
            .reject
            .entry("send".into())
            .or_default()
            .push_back(json!({"code": "overloaded", "message": "busy"}));
    }
    w.ask(json!({"cmd": "send", "conv_id": "c1", "text": "patient"}))
        .unwrap();
    w.run(30_000);
    assert!(w.core.debug_state()["budget"].as_f64().unwrap() < BUDGET_MAX / 2.0 + 1.0);
    // Each pause lasts a minute; the message still goes out in the end.
    w.run(400_000);
    assert_eq!(w.server.applied_cms.len(), 1);
}

#[test]
fn queue_edges() {
    let mut w = World::new(1);
    w.server.conv("c1");
    w.server.conv("c2");
    w.server.forbidden.insert("c2".into());
    w.connected();
    w.cmd(json!({"cmd": "messages", "conv_id": "c2"}));
    // An edit and a recall waiting on a send that is then refused fail too.
    w.ask(json!({"cmd": "send", "conv_id": "c2", "text": "x", "client_msg_id": "dep"}))
        .unwrap();
    w.ask(json!({"cmd": "edit", "conv_id": "c2", "client_msg_id": "dep", "text": "y"}))
        .unwrap();
    w.run(500);
    let failed: Vec<&Value> = w
        .emitted("operation")
        .into_iter()
        .filter(|e| e["result"] == "failed")
        .collect();
    assert!(
        failed.iter().any(|e| e["kind"] == "send") && failed.iter().any(|e| e["kind"] == "edit")
    );
    // A refused read is dropped quietly (it is only a position).
    w.server
        .reject
        .entry("read".into())
        .or_default()
        .push_back(json!({"code": "forbidden", "message": "no"}));
    w.ask(json!({"cmd": "read", "conv_id": "c1", "seq": 1}))
        .unwrap();
    w.run(500);
    assert!(
        w.emitted("operation")
            .iter()
            .any(|e| e["kind"] == "read" && e["result"] == "done")
    );
    // A damaged cache with a watched conversation: the view is reset.
    w.cmd(json!({"cmd": "messages", "conv_id": "c1"}));
    w.input(Input::StorageFailed {
        kind: "corrupt".into(),
    });
    assert!(
        w.emitted("messages")
            .iter()
            .any(|e| e["reset"] == true && e["conv_id"] == "c1")
    );
}

#[test]
fn jumping_near_the_end() {
    let mut w = World::new(1);
    w.server.conv("c1");
    for i in 0..300 {
        w.other_says("c1", &format!("m{i}"));
    }
    w.connected();
    w.cmd(json!({"cmd": "messages", "conv_id": "c1"}));
    let id = w.cmd(json!({"cmd": "jump_to", "conv_id": "c1", "seq": 260}));
    w.run(200);
    assert!(w.reply(id).unwrap().is_ok());
    let segs = w.core.debug_state()["convs"][0]["segments"].clone();
    assert_eq!(
        segs.as_array().unwrap().len(),
        1,
        "joined the synced range: {segs}"
    );
}

#[test]
fn pushes_for_unknown_or_old_positions_and_two_conversations() {
    let mut w = World::new(1);
    w.server.conv("c1");
    w.server.conv("c2");
    w.other_says("c2", "older");
    w.connected();
    w.other_says("c1", "newer");
    w.run(200);
    // Most recent activity first.
    let list = w.ask(json!({"cmd": "conversations"})).unwrap();
    let ids: Vec<&str> = list["conversations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["conv_id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["c1", "c2"]);
    // A push already applied: ignored; one for a conversation the device
    // has no position in: a sync.
    let old = w.server.logs["c1"][0].clone();
    let syncs = w.sent_ops("sync");
    frame(&mut w, json!({"type": "event", "event": old}));
    assert_eq!(w.sent_ops("sync"), syncs);
    frame(
        &mut w,
        json!({"type": "event", "event": {"conv_id": "c_new", "seq": 1, "type": "conv.created"}}),
    );
    assert_eq!(w.sent_ops("sync"), syncs + 1);
    frame(&mut w, json!({"type": "event", "event": {"conv_id": "c1"}}));
    w.assert_converged();
}
