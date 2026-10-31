//! Every rule of docs/12 §2.3–§2.11, one scenario each, on a fake clock,
//! network, server and store.

mod common;

use agentim_client_core::*;
use common::*;
use serde_json::{Value, json};

fn conn_events(w: &World) -> Vec<(String, Option<String>)> {
    w.emitted("connection")
        .iter()
        .map(|e| {
            (
                e["state"].as_str().unwrap().to_string(),
                e["reason"].as_str().map(String::from),
            )
        })
        .collect()
}

// ------------------------------------------------------------ connection

#[test]
fn connects_with_a_token_hello_list_and_sync() {
    let mut w = World::new(1);
    w.server.conv("c1");
    w.other_says("c1", "hi");
    w.connected();
    assert_eq!(w.token_calls, 1);
    let url = &w.connects[0];
    assert!(
        url.starts_with("wss://im.example/ws?device=dev1&device_name=Phone"),
        "{url}"
    );
    // The token travels beside the URL (a header), never in it (D129).
    assert!(!url.contains("tok1"), "{url}");
    assert_eq!(w.connect_tokens, ["tok1"]);
    let ops: Vec<&str> = w
        .server
        .requests
        .iter()
        .map(|r| r["op"].as_str().unwrap())
        .collect();
    assert_eq!(&ops[..4], &["hello", "list_convs", "sync", "ack"]);
    assert_eq!(
        conn_events(&w),
        vec![("connecting".into(), None), ("connected".into(), None)]
    );
    // The conversation is known, acknowledged after it was stored.
    assert_eq!(w.store.convs["c1"].last_seq, 2);
    assert_eq!(w.server.acked["c1"], 2);
    w.assert_converged();
}

#[test]
fn nothing_happens_before_the_cache_is_read() {
    let mut w = World::new(1);
    w.cmd(json!({"cmd": "connect"}));
    w.run(5_000);
    assert!(w.connects.is_empty());
    w.start();
    w.run(1_000);
    assert_eq!(w.state(), "connected");
}

#[test]
fn backoff_is_full_jitter_growing_to_the_cap() {
    let mut w = World::new(7);
    w.start();
    for _ in 0..20 {
        w.plan.push_back(Outcome::Unreachable);
    }
    w.cmd(json!({"cmd": "connect"}));
    let mut waits = vec![];
    for n in 1..=9u32 {
        // Until the attempt has failed and the next one is planned.
        let at = loop {
            w.run(5);
            if let Some(at) = w.core.debug_state()["retry_at"].as_i64() {
                break at;
            }
        };
        let wait = at - w.now;
        let cap = (BACKOFF_BASE_MS << (n - 1)).min(BACKOFF_CAP_MS);
        assert!(wait <= cap, "attempt {n}: {wait} > {cap}");
        waits.push(wait);
        w.run(wait);
    }
    assert!(waits.iter().any(|x| *x > 2_000), "it grows: {waits:?}");
    assert!(waits.iter().all(|x| *x <= BACKOFF_CAP_MS));
}

#[test]
fn after_two_minutes_down_it_is_suspended_and_tries_once_a_minute() {
    let mut w = World::new(3);
    w.start();
    for _ in 0..200 {
        w.plan.push_back(Outcome::Unreachable);
    }
    w.cmd(json!({"cmd": "connect"}));
    w.run(SUSPEND_AFTER_MS + 31_000);
    assert_eq!(w.state(), "suspended");
    let before = w.connects.len();
    w.run(10 * 60_000);
    let tries = w.connects.len() - before;
    assert!((8..=13).contains(&tries), "about one a minute: {tries}");
    // Back online after all: connected at the next try.
    w.plan.clear();
    w.run(75_000);
    assert_eq!(w.state(), "connected");
}

#[test]
fn an_unauthorized_token_is_replaced_once_at_once() {
    let mut w = World::new(1);
    w.start();
    // The cached token is refused: a new one at once (no backoff).
    w.cmd(json!({"cmd": "connect"}));
    w.run(1_000);
    w.cut(1006, "");
    w.plan.push_back(Outcome::Refuse(401, None));
    w.run(200);
    assert_eq!(w.state(), "connected");
    assert_eq!(w.token_calls, 2, "one new token");
    // The retry carries the new token, not the refused one.
    let t = &w.connect_tokens;
    assert_eq!(t[t.len() - 2..], ["tok1".to_string(), "tok2".to_string()]);
    // A fresh token refused too: backoff, not a loop of token calls.
    w.cut(1006, "");
    w.plan.push_back(Outcome::Refuse(401, None));
    w.plan.push_back(Outcome::Refuse(401, None));
    w.run(100);
    assert_eq!(w.state(), "disconnected");
    let calls = w.token_calls;
    w.run(60_000);
    assert!(w.token_calls <= calls + 2);
    assert_eq!(w.state(), "connected");
}

#[test]
fn a_failure_without_a_status_gets_a_fresh_token_first() {
    // Browsers do not show the handshake's status: an expired token looks
    // like any failure. After a working connection, one fresh token first.
    let mut w = World::new(1);
    w.connected();
    w.cut(1006, "");
    w.plan.push_back(Outcome::Unreachable);
    w.run(200);
    assert_eq!(w.token_calls, 2, "a fresh token at once");
    assert_eq!(w.state(), "connected");
    // During an outage the fresh token is kept: plain backoff.
    w.cut(1006, "");
    w.plan.extend([
        Outcome::Unreachable,
        Outcome::Unreachable,
        Outcome::Unreachable,
    ]);
    w.run(60_000);
    assert_eq!(w.token_calls, 3);
    assert_eq!(w.state(), "connected");
}

#[test]
fn refusals_that_retrying_cannot_fix_stop() {
    for (outcome, reason) in [
        (Outcome::Refuse(403, None), "unauthenticated"),
        (Outcome::Refuse(400, None), "protocol"),
    ] {
        let mut w = World::new(1);
        w.start();
        w.plan.push_back(outcome);
        w.cmd(json!({"cmd": "connect"}));
        w.run(10 * 60_000);
        assert_eq!(
            (w.state(), w.core.debug_state()["reason"].as_str()),
            ("failed", Some(reason))
        );
        assert_eq!(w.connects.len(), 1, "no retry");
        // The app handles it and connects again.
        w.cmd(json!({"cmd": "connect"}));
        w.run(1_000);
        assert_eq!(w.state(), "connected");
    }
    // The token provider says no.
    let mut w = World::new(1);
    w.start();
    w.tokens.push_back(Err(true));
    w.cmd(json!({"cmd": "connect"}));
    w.run(60_000);
    assert_eq!(w.state(), "failed");
    assert!(w.connects.is_empty());
    // The provider could not be reached: retried.
    let mut w = World::new(1);
    w.start();
    w.tokens.push_back(Err(false));
    w.cmd(json!({"cmd": "connect"}));
    w.run(60_000);
    assert_eq!(w.state(), "connected");
}

#[test]
fn a_busy_server_is_waited_for_at_least_as_long_as_it_says() {
    let mut w = World::new(1);
    w.start();
    w.plan.push_back(Outcome::Refuse(503, Some(4_000)));
    w.cmd(json!({"cmd": "connect"}));
    w.run(100);
    let at = w.core.debug_state()["retry_at"].as_i64().unwrap();
    assert!(at - w.now >= 3_900, "{}", at - w.now);
    assert!(w.core.debug_state()["budget"].as_f64().unwrap() < BUDGET_MAX);
    w.run(5_000);
    assert_eq!(w.state(), "connected");
}

#[test]
fn a_leaving_node_is_left_after_the_wait_it_gives() {
    let mut w = World::new(1);
    w.connected();
    w.deliver(vec![
        json!({"type": "goaway", "reason": "server_restart", "retry_after_ms": 3_000}).to_string(),
    ]);
    w.run(100);
    w.cut(1012, "server_restart");
    w.run(100);
    assert_eq!(
        conn_events(&w).last().unwrap(),
        &(
            "disconnected".to_string(),
            Some("server_restart".to_string())
        )
    );
    let n = w.connects.len();
    w.run(2_700);
    assert_eq!(w.connects.len(), n, "not before the wait");
    w.run(400);
    assert_eq!(w.state(), "connected");
    // Without a goaway frame: a random wait of at most 5 s.
    w.cut(1012, "server_restart");
    w.run(5_200);
    assert_eq!(w.state(), "connected");
}

#[test]
fn logged_out_and_too_old_stop_reconnecting() {
    for (reason, want) in [
        ("logged_out", "logged_out"),
        ("client_too_old", "upgrade_required"),
    ] {
        let mut w = World::new(1);
        w.connected();
        w.cut(1008, reason);
        w.run(10 * 60_000);
        assert_eq!(w.state(), "failed");
        assert_eq!(w.core.debug_state()["reason"], want);
        assert_eq!(w.connects.len(), 1);
    }
}

#[test]
fn a_dropped_connection_that_worked_is_retried_at_once() {
    let mut w = World::new(1);
    w.connected();
    w.cut(1006, "");
    w.run(100);
    assert_eq!(w.state(), "connected");
    assert_eq!(w.connects.len(), 2);
}

#[test]
fn heartbeat_pings_when_quiet_and_drops_a_dead_connection() {
    let mut w = World::new(1);
    w.server.heartbeat_ms = 10_000;
    w.connected();
    let pings = w.sent_ops("ping");
    w.run(10_500);
    assert_eq!(
        w.sent_ops("ping"),
        pings + 1,
        "one ping after 10 s of quiet"
    );
    // The server stops answering anything: dead after 2 × 10 + 5 s.
    w.lose_answers = 1_000_000;
    let n = w.connects.len();
    w.run(26_000);
    assert!(w.connects.len() > n, "replaced");
    assert!(w.events.iter().any(|e| e["type"] == "connection"
        && (e["reason"] == "heartbeat_timeout" || e["reason"] == "request_timeout")));
    w.lose_answers = 0;
    w.run(60_000);
    assert_eq!(w.state(), "connected");
}

#[test]
fn an_attempt_that_hangs_times_out_after_ten_seconds() {
    let mut w = World::new(1);
    w.start();
    w.plan.push_back(Outcome::Hang);
    w.cmd(json!({"cmd": "connect"}));
    w.run(CONNECT_TIMEOUT_MS - 100);
    assert_eq!(w.connects.len(), 1);
    w.run(200);
    assert_eq!(w.state(), "disconnected");
    w.run(30_000);
    assert_eq!(w.state(), "connected");
}

#[test]
fn offline_waits_without_retrying_and_online_connects_at_once() {
    let mut w = World::new(1);
    w.connected();
    w.input(Input::Network {
        online: false,
        generation: 1,
    });
    assert_eq!(w.state(), "suspended");
    let n = w.connects.len();
    w.run(30 * 60_000);
    assert_eq!(w.connects.len(), n, "no retries while offline");
    w.input(Input::Network {
        online: true,
        generation: 2,
    });
    w.run(500);
    assert_eq!(w.state(), "connected");
    assert_eq!(w.connects.len(), n + 1);
}

#[test]
fn a_new_route_is_probed_and_a_silent_connection_replaced() {
    let mut w = World::new(1);
    w.connected();
    let pings = w.sent_ops("ping");
    w.input(Input::Network {
        online: true,
        generation: 5,
    });
    w.run(100);
    assert_eq!(w.sent_ops("ping"), pings + 1);
    assert_eq!(w.connects.len(), 1, "it answered: kept");
    // The next change: the old route is dead (no answer in 3 s).
    w.lose_answers = 1;
    w.input(Input::Network {
        online: true,
        generation: 6,
    });
    w.run(3_200);
    assert_eq!(w.connects.len(), 2);
    assert_eq!(w.state(), "connected");
}

#[test]
fn back_in_the_foreground_it_checks_the_connection_and_syncs() {
    let mut w = World::new(1);
    w.connected();
    let (pings, syncs) = (w.sent_ops("ping"), w.sent_ops("sync"));
    w.input(Input::Foreground(false));
    w.input(Input::Foreground(true));
    w.run(100);
    assert_eq!(
        (w.sent_ops("ping"), w.sent_ops("sync")),
        (pings + 1, syncs + 1)
    );
}

#[test]
fn back_in_the_foreground_while_down_it_tries_at_once_unless_failed() {
    let mut w = World::new(4);
    w.start();
    for _ in 0..200 {
        w.plan.push_back(Outcome::Unreachable);
    }
    w.cmd(json!({"cmd": "connect"}));
    // Down for long: retries are a minute apart now.
    w.run(SUSPEND_AFTER_MS + 31_000);
    assert_eq!(w.state(), "suspended");
    w.plan.clear();
    w.input(Input::Foreground(false));
    let before = w.connects.len();
    w.input(Input::Foreground(true));
    w.run(500);
    assert_eq!(
        w.connects.len(),
        before + 1,
        "tried at once, not at the next minute"
    );
    assert_eq!(w.state(), "connected");
    // Failed (the token was refused): coming back does not retry.
    let mut f = World::new(5);
    f.start();
    f.tokens.push_back(Err(true));
    f.cmd(json!({"cmd": "connect"}));
    f.run(1_000);
    assert_eq!(f.state(), "failed");
    let before = f.connects.len();
    f.input(Input::Foreground(false));
    f.input(Input::Foreground(true));
    f.run(60_000);
    assert_eq!(f.connects.len(), before);
    assert_eq!(f.state(), "failed");
}

#[test]
fn close_stops_everything() {
    let mut w = World::new(1);
    w.connected();
    w.cmd(json!({"cmd": "close"}));
    assert_eq!(w.state(), "closed");
    let n = w.connects.len();
    w.run(10 * 60_000);
    assert_eq!(w.connects.len(), n);
    assert!(w.core.next_deadline().is_none(), "no timers left");
}

// ------------------------------------------------------------------ sync

#[test]
fn pushes_apply_in_order_and_a_gap_is_filled_by_sync() {
    let mut w = World::new(1);
    w.server.conv("c1");
    w.connected();
    w.cmd(json!({"cmd": "messages", "conv_id": "c1"}));
    // The server's push queue overflowed: it says resync.
    w.server.overflow = 1;
    w.other_says("c1", "one");
    w.other_says("c1", "two");
    w.run(500);
    let texts: Vec<String> = w
        .view("c1")
        .iter()
        .map(|m| m["content"].as_str().unwrap().into())
        .collect();
    assert_eq!(texts, ["one", "two"]);
    assert_eq!(w.connects.len(), 1, "the sync filled it, no reconnect");
    // A push lost in a way sync cannot repair on this connection: the
    // gap stays, so the connection is replaced and the new one resends.
    w.lose_pushes = 1;
    w.other_says("c1", "three");
    w.other_says("c1", "four");
    w.run(1_000);
    let texts: Vec<String> = w
        .view("c1")
        .iter()
        .map(|m| m["content"].as_str().unwrap().into())
        .collect();
    assert_eq!(texts, ["one", "two", "three", "four"]);
    assert_eq!(w.connects.len(), 2);
    assert!(w.emitted("connection").iter().any(|e| e["reason"] == "gap"));
    w.assert_converged();
}

#[test]
fn a_lost_last_push_is_found_by_the_position_check() {
    let mut w = World::new(1);
    w.server.conv("c1");
    w.connected();
    w.lose_pushes = 1;
    w.other_says("c1", "lonely");
    w.run(1_000);
    assert_eq!(w.core.debug_state()["convs"][0]["last_seq"], 1);
    w.run(HEADS_EVERY_MS + 1_000);
    w.assert_converged();
}

#[test]
fn many_events_come_in_batches() {
    let mut w = World::new(1);
    w.server.batch = 7;
    w.server.conv("c1");
    for i in 0..25 {
        w.other_says("c1", &format!("m{i}"));
    }
    w.connected();
    w.assert_converged();
    assert!(w.sent_ops("sync") >= 4);
}

#[test]
fn a_full_store_withholds_acknowledgements_until_it_recovers() {
    let mut w = World::new(1);
    w.server.conv("c1");
    w.connected();
    let acked = w.server.acked["c1"];
    w.store_fail = Some("full".into());
    w.other_says("c1", "while full");
    w.run(500);
    assert_eq!(w.server.acked["c1"], acked, "not acknowledged");
    assert!(w.emitted("storage").iter().any(|e| e["state"] == "full"));
    w.store_fail = None;
    w.input(Input::StorageRecovered);
    w.run(500);
    assert!(w.server.acked["c1"] > acked);
    assert_eq!(w.store.convs["c1"].last_seq, w.server.last("c1"));
    w.assert_converged();
}

#[test]
fn a_damaged_cache_is_rebuilt_and_the_queue_kept() {
    let mut w = World::new(1);
    w.server.conv("c1");
    w.connected();
    w.cmd(json!({"cmd": "close"}));
    w.ask(json!({"cmd": "send", "conv_id": "c1", "text": "queued"}))
        .unwrap();
    w.input(Input::StorageFailed {
        kind: "corrupt".into(),
    });
    assert!(w.emitted("storage").iter().any(|e| e["reset"] == "corrupt"));
    assert_eq!(w.store.ops.len(), 1, "the queue is written again");
    w.cmd(json!({"cmd": "connect"}));
    w.run(1_000);
    assert_eq!(w.server.applied_cms.len(), 1);
    w.assert_converged();
}

#[test]
fn another_account_never_sees_the_cache() {
    let mut w = World::new(1);
    w.store.account = Some("u_someone_else".into());
    w.store.convs.insert(
        "old".into(),
        ConvRow {
            conv_id: "old".into(),
            last_seq: 3,
            ..Default::default()
        },
    );
    w.connected();
    assert!(w.effects.contains(&Effect::WipeStore));
    assert!(!w.store.convs.contains_key("old"));
    assert_eq!(w.store.account.as_deref(), Some(ME));
}

// ----------------------------------------------------------------- queue

#[test]
fn messages_say_whether_they_are_mine_also_from_the_cache_before_connecting() {
    let mut w = World::new(1);
    w.server.conv("c1");
    w.connected();
    w.server.other_says("c1", "from bob");
    w.run(1_000);
    w.ask(json!({"cmd": "send", "conv_id": "c1", "text": "from me", "client_msg_id": "cmM"}))
        .unwrap();
    w.run(1_000);
    let check = |w: &mut World| {
        w.cmd(json!({"cmd": "messages", "conv_id": "c1"}));
        let mine: Vec<(String, bool)> = w
            .view("c1")
            .iter()
            .map(|m| (m["content"].as_str().unwrap().into(), m["mine"] == true))
            .collect();
        assert_eq!(mine, [("from bob".into(), false), ("from me".into(), true)]);
        let list = w.ask(json!({"cmd": "conversations"})).unwrap();
        assert_eq!(list["conversations"][0]["last_message"]["mine"], true);
    };
    check(&mut w);
    // A new process, offline: the cache's account says who I am.
    w.restart(2);
    w.cmd(json!({"cmd": "close"}));
    check(&mut w);
    // Queued before the connection: mine, with my id.
    let r = w
        .ask(json!({"cmd": "send", "conv_id": "c1", "text": "queued", "client_msg_id": "cmQ"}))
        .unwrap();
    let v = w.view("c1");
    let q = v.iter().find(|m| m["key"] == r["key"]).unwrap();
    assert_eq!(
        (q["mine"].clone(), q["sender_id"].clone()),
        (json!(true), json!(ME))
    );
}

#[test]
fn sends_offline_show_at_once_and_go_out_once() {
    let mut w = World::new(1);
    w.server.conv("c1");
    w.start();
    w.cmd(json!({"cmd": "messages", "conv_id": "c1"}));
    let r = w
        .ask(json!({"cmd": "send", "conv_id": "c1", "text": "hello", "client_msg_id": "cm1"}))
        .unwrap();
    assert_eq!(r["key"], "local:cm1");
    let v = w.view("c1");
    assert_eq!(
        (v[0]["status"].as_str(), v[0]["content"].as_str()),
        (Some("sending"), Some("hello"))
    );
    assert_eq!(w.store.ops.len(), 1, "stored before it is sent");
    // The same client_msg_id again is the same message.
    w.ask(json!({"cmd": "send", "conv_id": "c1", "text": "hello", "client_msg_id": "cm1"}))
        .unwrap();
    assert_eq!(w.view("c1").len(), 1);
    w.cmd(json!({"cmd": "connect"}));
    w.run(1_000);
    let v = w.view("c1");
    assert_eq!(v.len(), 1, "{v:?}");
    assert_eq!(v[0]["status"], "sent");
    assert!(w.store.ops.is_empty());
    assert_eq!(w.server.applied_cms, ["cm1"]);
    assert!(
        w.emitted("operation")
            .iter()
            .any(|e| e["result"] == "done" && e["kind"] == "send")
    );
    w.assert_converged();
}

#[test]
fn a_send_answered_before_its_message_arrives_stays_shown() {
    let mut w = World::new(1);
    w.server.conv("c1");
    w.connected();
    w.cmd(json!({"cmd": "messages", "conv_id": "c1"}));
    // The server's answer comes first: the push of the message is lost and
    // only found again later (the position check, then sync).
    w.lose_pushes = 1;
    w.ask(json!({"cmd": "send", "conv_id": "c1", "text": "hi", "client_msg_id": "cmS"}))
        .unwrap();
    w.run(200);
    let v = w.view("c1");
    assert_eq!(v.len(), 1, "still shown: {v:?}");
    assert_eq!(v[0]["key"], "local:cmS");
    assert_eq!(v[0]["status"], "sent");
    let id = v[0]["msg_id"].as_str().unwrap().to_string();
    assert!(id.starts_with("m_") || !id.starts_with("local:"), "{id}");
    w.run(40_000);
    let v = w.view("c1");
    assert_eq!(v.len(), 1, "{v:?}");
    assert_eq!(
        (v[0]["key"].as_str(), v[0]["status"].as_str()),
        (Some(id.as_str()), Some("sent"))
    );
    // At no point did the app see it go: every event that removes the
    // local copy puts the stored one in its place.
    for e in w.emitted("messages") {
        let ch = e["changes"].as_array().unwrap();
        if ch
            .iter()
            .any(|c| c["op"] == "remove" && c["key"] == "local:cmS")
        {
            assert!(
                ch.iter()
                    .any(|c| c["op"] == "upsert" && c["message"]["key"] == id.as_str()),
                "{e}"
            );
        }
    }
    w.assert_converged();
}

#[test]
fn a_message_that_arrives_before_its_answer_replaces_the_local_copy_at_once() {
    let mut w = World::new(1);
    w.server.conv("c1");
    w.connected();
    w.cmd(json!({"cmd": "messages", "conv_id": "c1"}));
    // The server pushes the message before it answers the send (as it does).
    w.ask(json!({"cmd": "send", "conv_id": "c1", "text": "hi", "client_msg_id": "cmP"}))
        .unwrap();
    w.run(1_000);
    let v = w.view("c1");
    assert_eq!(v.len(), 1, "{v:?}");
    let id = v[0]["msg_id"].as_str().unwrap().to_string();
    // The stored copy came in the same change that took the local one away
    // (World also checks after every change that nothing shows twice).
    let swap = w.emitted("messages").into_iter().find(|e| {
        e["changes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["op"] == "upsert" && c["message"]["key"] == id.as_str())
    });
    let ch = swap.expect("the stored copy was shown")["changes"].clone();
    assert!(
        ch.as_array()
            .unwrap()
            .iter()
            .any(|c| c["op"] == "remove" && c["key"] == "local:cmP"),
        "{ch}"
    );
    w.assert_converged();
}

fn trace_parts(r: &Value) -> (String, String) {
    let t = r["trace"].as_str().expect("every attempt has a trace");
    let p: Vec<&str> = t.split('-').collect();
    assert_eq!(p.len(), 4, "{t}");
    assert_eq!(
        (p[0], p[1].len(), p[2].len(), p[3]),
        ("00", 32, 16, "00"),
        "{t}"
    );
    assert!(
        p[1].bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)),
        "{t}"
    );
    (p[1].to_string(), p[2].to_string())
}

#[test]
fn every_attempt_of_an_operation_carries_its_trace() {
    let mut w = World::new(1);
    w.server.conv("c1");
    w.connected();
    // The answer is lost twice: three attempts.
    w.lose_answers = 2;
    w.ask(json!({"cmd": "send", "conv_id": "c1", "text": "x", "client_msg_id": "cmT"}))
        .unwrap();
    w.run(60_000);
    let sends: Vec<(String, String)> = w
        .server
        .requests
        .iter()
        .filter(|r| r["op"] == "send")
        .map(trace_parts)
        .collect();
    assert!(sends.len() >= 2, "{sends:?}");
    // One trace id; a new span each attempt.
    assert!(sends.iter().all(|s| s.0 == sends[0].0), "{sends:?}");
    let spans: std::collections::HashSet<&String> = sends.iter().map(|s| &s.1).collect();
    assert_eq!(spans.len(), sends.len(), "{sends:?}");
    assert_eq!(w.server.applied_cms, ["cmT"]);
    // Another operation, another trace; pings carry none.
    w.ask(json!({"cmd": "send", "conv_id": "c1", "text": "y", "client_msg_id": "cmU"}))
        .unwrap();
    w.run(2_000);
    let other = w
        .server
        .requests
        .iter()
        .rev()
        .find(|r| r["op"] == "send")
        .unwrap();
    assert_ne!(trace_parts(other).0, sends[0].0);
    assert!(
        w.server
            .requests
            .iter()
            .filter(|r| r["op"] == "ping")
            .all(|r| r.get("trace").is_none())
    );
    w.assert_converged();
}

#[test]
fn a_queued_operation_keeps_its_trace_across_a_restart() {
    let mut w = World::new(1);
    w.server.conv("c1");
    w.start();
    w.ask(json!({"cmd": "send", "conv_id": "c1", "text": "x", "client_msg_id": "cmR"}))
        .unwrap();
    let stored = w.store.ops.values().next().unwrap()["trace"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(stored.len(), 32);
    w.restart(7);
    w.cmd(json!({"cmd": "connect"}));
    w.run(2_000);
    let sent = w
        .server
        .requests
        .iter()
        .find(|r| r["op"] == "send")
        .unwrap();
    assert_eq!(trace_parts(sent).0, stored);
    // An operation stored by an older version (no trace) gets one.
    let mut w = World::new(3);
    w.server.conv("c1");
    w.start();
    w.ask(json!({"cmd": "send", "conv_id": "c1", "text": "x", "client_msg_id": "cmO"}))
        .unwrap();
    for v in w.store.ops.values_mut() {
        v.as_object_mut().unwrap().remove("trace");
    }
    w.restart(4);
    w.cmd(json!({"cmd": "connect"}));
    w.run(2_000);
    let sent = w
        .server
        .requests
        .iter()
        .find(|r| r["op"] == "send")
        .unwrap();
    trace_parts(sent);
    assert_eq!(w.server.applied_cms, ["cmO"]);
}

#[test]
fn a_lost_answer_is_resent_with_the_same_ids_and_applied_once() {
    let mut w = World::new(1);
    w.server.conv("c1");
    w.connected();
    w.lose_answers = 1;
    w.ask(json!({"cmd": "send", "conv_id": "c1", "text": "x", "client_msg_id": "cmA"}))
        .unwrap();
    w.run(40_000);
    assert_eq!(w.server.applied_cms, ["cmA"]);
    assert!(
        w.server
            .requests
            .iter()
            .filter(|r| r["op"] == "send")
            .count()
            >= 2,
        "sent again"
    );
    w.assert_converged();
}

#[test]
fn server_trouble_is_retried_and_business_refusals_fail() {
    let mut w = World::new(1);
    w.server.conv("c1");
    w.server.conv("c2");
    w.server.forbidden.insert("c2".into());
    w.connected();
    w.cmd(json!({"cmd": "messages", "conv_id": "c2"}));
    w.server.reject.entry("send".into()).or_default().extend([
        json!({"code": "overloaded", "message": "busy", "retry_after_ms": 1500}),
        json!({"code": "rate_limited", "message": "slow"}),
    ]);
    w.ask(json!({"cmd": "send", "conv_id": "c1", "text": "eventually", "client_msg_id": "ok1"}))
        .unwrap();
    w.run(20_000);
    assert_eq!(w.server.applied_cms, ["ok1"]);
    // Refused by the rules: failed, shown with the code, later ones not blocked.
    w.ask(json!({"cmd": "send", "conv_id": "c2", "text": "no", "client_msg_id": "bad"}))
        .unwrap();
    w.run(1_000);
    // A failed message can still be changed before trying again.
    w.ask(json!({"cmd": "edit", "conv_id": "c2", "client_msg_id": "bad", "text": "no!"}))
        .unwrap();
    let v = w.view("c2");
    let failed = v.iter().find(|m| m["key"] == "local:bad").unwrap();
    assert_eq!(failed["status"], "failed");
    assert_eq!(failed["content"], "no!", "the edit changed the unsent text");
    assert_eq!(failed["error"]["code"], "forbidden");
    // Retry (same id): refused again. Discard: gone.
    w.ask(json!({"cmd": "retry", "conv_id": "c2", "client_msg_id": "bad"}))
        .unwrap();
    w.run(1_000);
    assert_eq!(
        w.view("c2")
            .iter()
            .find(|m| m["key"] == "local:bad")
            .unwrap()["status"],
        "failed"
    );
    w.ask(json!({"cmd": "discard", "conv_id": "c2", "client_msg_id": "bad"}))
        .unwrap();
    assert!(w.view("c2").iter().all(|m| m["key"] != "local:bad"));
    assert!(w.store.ops.is_empty());
    assert_eq!(
        w.ask(json!({"cmd": "discard", "conv_id": "c2", "client_msg_id": "bad"}))
            .unwrap_err()
            .code,
        "not_found"
    );
    w.assert_converged();
}

#[test]
fn a_change_to_a_message_whose_send_is_refused_fails_too() {
    let mut w = World::new(1);
    w.server.conv("c2");
    w.server.forbidden.insert("c2".into());
    w.connected();
    w.cmd(json!({"cmd": "messages", "conv_id": "c2"}));
    w.ask(json!({"cmd": "send", "conv_id": "c2", "text": "no", "client_msg_id": "bad"}))
        .unwrap();
    // The send is on its way: the edit waits behind it as its own operation.
    w.ask(json!({"cmd": "edit", "conv_id": "c2", "client_msg_id": "bad", "text": "no!"}))
        .unwrap();
    assert_eq!(w.core.debug_state()["queue"].as_array().unwrap().len(), 2);
    w.run(1_000);
    let results: Vec<(String, String)> = w
        .events
        .iter()
        .filter(|e| e["type"] == "operation" && e["result"] == "failed")
        .map(|e| {
            (
                e["kind"].as_str().unwrap().to_string(),
                e["error"]["code"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    // The edit fails with the reason its message did.
    assert_eq!(
        results,
        [
            ("send".to_string(), "forbidden".to_string()),
            ("edit".to_string(), "forbidden".to_string())
        ]
    );
    // The failed send stays for retry or discard; the edit is gone; nothing reached the server.
    assert_eq!(w.core.debug_state()["queue"].as_array().unwrap().len(), 1);
    assert!(w.server.requests.iter().all(|r| r["op"] != "edit"));
    w.assert_converged();
}

#[test]
fn queued_changes_merge_before_they_go() {
    let mut w = World::new(1);
    w.server.conv("c1");
    w.start();
    w.cmd(json!({"cmd": "messages", "conv_id": "c1"}));
    // Unsent: edit changes the send, recall drops it.
    w.ask(json!({"cmd": "send", "conv_id": "c1", "text": "draft", "client_msg_id": "a"}))
        .unwrap();
    w.ask(json!({"cmd": "edit", "conv_id": "c1", "client_msg_id": "a", "text": "final"}))
        .unwrap();
    w.ask(json!({"cmd": "send", "conv_id": "c1", "text": "oops", "client_msg_id": "b"}))
        .unwrap();
    w.ask(json!({"cmd": "recall", "conv_id": "c1", "client_msg_id": "b"}))
        .unwrap();
    // Reads: one, the furthest.
    w.ask(json!({"cmd": "read", "conv_id": "c1", "seq": 1}))
        .unwrap();
    w.ask(json!({"cmd": "read", "conv_id": "c1", "seq": 3}))
        .unwrap();
    w.ask(json!({"cmd": "read", "conv_id": "c1", "seq": 2}))
        .unwrap();
    assert_eq!(w.core.debug_state()["queue"].as_array().unwrap().len(), 2);
    let v = w.view("c1");
    assert_eq!(v.len(), 1);
    assert_eq!(v[0]["content"], "final");
    w.cmd(json!({"cmd": "connect"}));
    w.run(1_000);
    assert_eq!(w.server.applied_cms, ["a"]);
    let reads: Vec<_> = w
        .server
        .requests
        .iter()
        .filter(|r| r["op"] == "read")
        .collect();
    assert_eq!(reads.len(), 1);
    assert_eq!(reads[0]["seq"], 3);
    let id = w.server.cms["a"].clone();
    assert_eq!(w.server.cur[&id]["content"], "final");
    // Sent ones: edits in a row keep the last, recall drops pending edits.
    w.cmd(json!({"cmd": "close"}));
    w.ask(json!({"cmd": "edit", "conv_id": "c1", "msg_id": id, "text": "e1"}))
        .unwrap();
    w.ask(json!({"cmd": "edit", "conv_id": "c1", "msg_id": id, "text": "e2"}))
        .unwrap();
    assert_eq!(w.view("c1")[0]["content"], "e2");
    assert_eq!(w.view("c1")[0]["pending"], "edit");
    w.cmd(json!({"cmd": "connect"}));
    w.run(1_000);
    assert_eq!(
        w.server
            .requests
            .iter()
            .filter(|r| r["op"] == "edit")
            .count(),
        1
    );
    assert_eq!(w.server.cur[&id]["content"], "e2");
    w.ask(json!({"cmd": "recall", "conv_id": "c1", "msg_id": id}))
        .unwrap();
    w.run(1_000);
    assert_eq!(w.server.cur[&id]["state"], "recalled");
    // Recalling twice: the second is already done.
    w.ask(json!({"cmd": "recall", "conv_id": "c1", "msg_id": id}))
        .unwrap();
    w.run(1_000);
    assert!(w.store.ops.is_empty());
    w.assert_converged();
}

#[test]
fn one_operation_per_conversation_and_eight_overall_in_flight() {
    let mut w = World::new(1);
    for i in 0..12 {
        w.server.conv(&format!("c{i}"));
    }
    w.connected();
    w.lose_answers = 1_000;
    for i in 0..12 {
        for j in 0..3 {
            w.ask(json!({"cmd": "send", "conv_id": format!("c{i}"), "text": format!("{i}-{j}")}))
                .unwrap();
        }
    }
    let q = w.core.debug_state()["queue"].clone();
    let sent_now = w
        .server
        .requests
        .iter()
        .filter(|r| r["op"] == "send")
        .count();
    assert_eq!(sent_now, MAX_IN_FLIGHT, "{q}");
    let convs: std::collections::HashSet<_> = w
        .server
        .requests
        .iter()
        .filter(|r| r["op"] == "send")
        .map(|r| r["conv_id"].clone())
        .collect();
    assert_eq!(convs.len(), MAX_IN_FLIGHT, "one per conversation");
    w.lose_answers = 0;
    w.run(120_000);
    assert_eq!(w.server.applied_cms.len(), 36);
    // In order within each conversation.
    for i in 0..12 {
        let c = format!("c{i}");
        let texts: Vec<String> = w.server.logs[&c]
            .iter()
            .filter_map(|e| e["msg"]["content"].as_str().map(String::from))
            .collect();
        assert_eq!(
            texts,
            [format!("{i}-0"), format!("{i}-1"), format!("{i}-2")]
        );
    }
    w.assert_converged();
}

#[test]
fn the_queue_survives_a_restart_and_is_sent_once() {
    let mut w = World::new(1);
    w.server.conv("c1");
    w.connected();
    w.lose_answers = 1;
    w.ask(json!({"cmd": "send", "conv_id": "c1", "text": "before crash", "client_msg_id": "k1"}))
        .unwrap();
    w.cmd(json!({"cmd": "close"}));
    w.ask(json!({"cmd": "send", "conv_id": "c1", "text": "offline", "client_msg_id": "k2"}))
        .unwrap();
    w.restart(2);
    w.cmd(json!({"cmd": "messages", "conv_id": "c1"}));
    let v = w.view("c1");
    assert!(
        v.iter().any(|m| m["key"] == "local:k2"),
        "shown from the cache: {v:?}"
    );
    w.cmd(json!({"cmd": "connect"}));
    w.run(2_000);
    let mut cms = w.server.applied_cms.clone();
    cms.sort();
    assert_eq!(cms, ["k1", "k2"]);
    w.assert_converged();
}

#[test]
fn an_edit_queued_behind_its_send_survives_a_restart() {
    let mut w = World::new(1);
    w.server.conv("c1");
    w.connected();
    w.cmd(json!({"cmd": "messages", "conv_id": "c1"}));
    w.ask(json!({"cmd": "send", "conv_id": "c1", "text": "draft", "client_msg_id": "a"}))
        .unwrap();
    // The send is on its way: the edit is queued after it, by the local key.
    w.ask(json!({"cmd": "edit", "conv_id": "c1", "client_msg_id": "a", "text": "final"}))
        .unwrap();
    // The send's answer arrives; the app stops before the edit is answered.
    for _ in 0..100 {
        w.run(5);
        if w.core.debug_state()["queue"].as_array().unwrap().len() == 1 {
            break;
        }
    }
    assert_eq!(w.server.applied_cms, ["a"]);
    w.cmd(json!({"cmd": "close"}));
    w.restart(2);
    w.cmd(json!({"cmd": "messages", "conv_id": "c1"}));
    w.cmd(json!({"cmd": "connect"}));
    w.run(2_000);
    let id = w.server.cms["a"].clone();
    assert_eq!(
        w.server.cur[&id]["content"], "final",
        "the edit names the sent message after the restart"
    );
    assert!(
        w.events
            .iter()
            .all(|e| !(e["type"] == "operation" && e["result"] == "failed"))
    );
    assert!(w.store.ops.is_empty());
    w.assert_converged();
}

#[test]
fn online_only_requests_fail_at_once_offline() {
    let mut w = World::new(1);
    w.start();
    let e = w
        .ask(json!({"cmd": "call", "op": "decide", "args": {"request_id": "r"}}))
        .unwrap_err();
    assert_eq!((e.code.as_str(), e.retryable), ("disconnected", true));
    w.cmd(json!({"cmd": "connect"}));
    w.run(1_000);
    let id = w.cmd(json!({"cmd": "call", "op": "ping"}));
    w.run(100);
    assert!(w.reply(id).unwrap().unwrap()["t"].is_number());
    // No answer in time: `timeout`, result unknown.
    w.lose_answers = 1;
    let id = w.cmd(json!({"cmd": "call", "op": "ping"}));
    w.run(11_000);
    assert_eq!(w.reply(id).unwrap().unwrap_err().code, "timeout");
    assert_eq!(
        w.ask(json!({"cmd": "nope"})).unwrap_err().code,
        "bad_request"
    );
    assert_eq!(
        w.ask(json!({"cmd": "send", "conv_id": "c1"}))
            .unwrap_err()
            .code,
        "bad_request"
    );
    assert_eq!(
        w.ask(json!({"cmd": "edit", "conv_id": "c1", "text": "x"}))
            .unwrap_err()
            .code,
        "bad_request"
    );
    assert_eq!(
        w.ask(json!({"cmd": "edit", "conv_id": "c1", "client_msg_id": "nope", "text": "x"}))
            .unwrap_err()
            .code,
        "not_found"
    );
}

// --------------------------------------------------------------- history

#[test]
fn older_messages_come_from_the_cache_then_the_server() {
    let mut w = World::new(1);
    w.server.conv("c1");
    for i in 0..200 {
        w.other_says("c1", &format!("m{i}"));
    }
    w.connected();
    let r = w.ask(json!({"cmd": "messages", "conv_id": "c1"})).unwrap();
    // A new device gets the last 30 (the fake server's window).
    assert_eq!(r["messages"].as_array().unwrap().len(), 30);
    assert_eq!(r["has_older"], true);
    let mut pages = 0;
    loop {
        let id = w.cmd(json!({"cmd": "load_older", "conv_id": "c1"}));
        w.run(200);
        let r = w.reply(id).unwrap().unwrap();
        pages += 1;
        if r["has_older"] == false {
            assert_eq!(r["older_end"], "start");
            break;
        }
        assert!(pages < 20);
    }
    let texts: Vec<String> = w
        .view("c1")
        .iter()
        .map(|m| m["content"].as_str().unwrap().into())
        .collect();
    let want: Vec<String> = (0..200).map(|i| format!("m{i}")).collect();
    assert_eq!(texts, want);
    // After a restart the cache answers first (no server request).
    w.cmd(json!({"cmd": "close"}));
    w.restart(5);
    let r = w.ask(json!({"cmd": "messages", "conv_id": "c1"})).unwrap();
    assert_eq!(r["messages"].as_array().unwrap().len(), PAGE as usize);
    let before = w.server.requests.len();
    for _ in 0..3 {
        let id = w.cmd(json!({"cmd": "load_older", "conv_id": "c1"}));
        w.run(50);
        assert!(w.reply(id).unwrap().is_ok());
    }
    assert_eq!(w.server.requests.len(), before, "offline, from the cache");
    assert_eq!(w.view("c1").len(), 200);
}

#[test]
fn jumping_to_a_message_makes_an_island() {
    let mut w = World::new(1);
    w.server.conv("c1");
    for i in 0..300 {
        w.other_says("c1", &format!("m{i}"));
    }
    w.connected();
    w.cmd(json!({"cmd": "messages", "conv_id": "c1"}));
    let id = w.cmd(json!({"cmd": "jump_to", "conv_id": "c1", "seq": 100}));
    w.run(200);
    let r = w.reply(id).unwrap().unwrap();
    assert_eq!(r["added"], 50);
    let segs = w.core.debug_state()["convs"][0]["segments"].clone();
    assert_eq!(
        segs.as_array().unwrap().len(),
        2,
        "an island and the synced range: {segs}"
    );
    assert!(w.view("c1").iter().any(|m| m["created_seq"] == 100));
}
