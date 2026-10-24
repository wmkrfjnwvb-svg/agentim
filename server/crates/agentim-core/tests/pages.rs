//! What the client SDK needs from the server (docs/08 §2.8, §2.9; docs/12
//! §2.14 S1): people paging back through a conversation (`history` with an
//! anchor), the application-level `ping`, the heartbeat in the welcome
//! frame, `goaway` with a wait before reconnecting, `retry_after_ms` on
//! temporary refusals, and 403 for a deactivated person.

mod common;

use std::net::SocketAddr;
use std::time::Duration;

use agentim_core::hub::Hub;
use agentim_core::logic::HubConfig;
use agentim_core::model::*;
use agentim_core::server::{AppState, router};
use common::*;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;

backend_tests!(
    pages_go_back_in_current_state,
    anchors_and_their_boundaries,
    only_what_the_member_may_see,
    retention_and_the_switch,
    pages_are_limited_and_leave_nothing_behind,
    race_pages_while_sending_and_editing,
);

fn code(r: &Value) -> &str {
    r["error"]["code"].as_str().unwrap_or("")
}

fn seqs_of(r: &Value) -> Vec<i64> {
    r["messages"]
        .as_array()
        .unwrap_or_else(|| panic!("{r}"))
        .iter()
        .map(|m| m["created_seq"].as_i64().unwrap())
        .collect()
}

fn page(conv: &str, anchor: Value, before: i64, after: i64) -> Value {
    json!({"op": "history", "conv_id": conv, "anchor": anchor, "before": before, "after": after})
}

async fn hub_with(
    db: &TestDb,
    f: impl FnOnce(&mut agentim_core::config::Config, &mut HubConfig),
) -> Hub {
    let mut cfg = single(&db.url);
    let mut h = hub_cfg();
    f(&mut cfg, &mut h);
    Hub::open(cfg, h).await.expect("open hub")
}

/// Sends `n` messages "m1".."mn" and returns their seqs.
async fn fill(c: &Conn, conv: &str, n: usize) -> Vec<i64> {
    let mut out = vec![];
    for i in 1..=n {
        out.push(
            c.send(conv, &format!("m{i}")).await["seq"]
                .as_i64()
                .unwrap(),
        );
    }
    out
}

async fn pages_go_back_in_current_state(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let a = Conn::new(&hub, &alice);
    let b = Conn::new(&hub, &bob);
    let conv = a.group(&[&bob]).await;
    let sent = fill(&a, &conv, 120).await;
    // The default page: the newest 50, oldest first.
    let r = b.ok(json!({"op": "history", "conv_id": conv})).await;
    assert_eq!(seqs_of(&r), sent[70..].to_vec());
    assert_eq!(
        (r["has_before"].clone(), r["has_after"].clone()),
        (json!(true), json!(false))
    );
    assert!(r.get("before_end").is_none());
    // Going back page by page reaches the start and says so.
    let mut got = seqs_of(&r);
    let mut anchor = got[0] - 1;
    loop {
        let r = b.ok(page(&conv, json!(anchor), 50, 0)).await;
        let s = seqs_of(&r);
        got.splice(0..0, s.clone());
        if r["has_before"] == false {
            assert_eq!(r["before_end"], "start");
            break;
        }
        anchor = s[0] - 1;
    }
    assert_eq!(got, sent, "every message once, in order");
    // Messages come in their current state: edited, recalled, streaming.
    let first = b.ok(page(&conv, json!(sent[0]), 1, 0)).await["messages"][0].clone();
    let id = first["msg_id"].as_str().unwrap().to_string();
    assert_eq!(first["content"], "m1");
    a.ok(json!({"op": "edit", "msg_id": id, "text": "m1 edited"}))
        .await;
    let second = b.ok(page(&conv, json!(sent[1]), 1, 0)).await["messages"][0].clone();
    a.ok(json!({"op": "recall", "msg_id": second["msg_id"]}))
        .await;
    let r = b.ok(page(&conv, json!(sent[1]), 2, 0)).await;
    assert_eq!(r["messages"][0]["content"], "m1 edited");
    assert_eq!(r["messages"][0]["version"], 2);
    assert_eq!(r["messages"][1]["state"], "recalled");
    assert_ne!(
        r["messages"][1]["content"], "m2",
        "recalled content is gone"
    );
    // The page is the reader's own: no client_msg_id of someone else.
    assert!(r["messages"][0].get("client_msg_id").is_none());
    // A reply that is still streaming comes with what it has so far.
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    a.ok(json!({"op": "add_member", "conv_id": conv, "user_id": bot.id}))
        .await;
    let g = Conn::new(&hub, &bot);
    let s = g
        .ok(json!({"op": "stream_start", "conv_id": conv, "client_msg_id": "s1"}))
        .await;
    g.ok(json!({"op": "stream_append", "msg_id": s["msg_id"], "idx": 0, "text": "half"}))
        .await;
    hub.tick().await;
    let r = b
        .ok(json!({"op": "history", "conv_id": conv, "before": 1}))
        .await;
    assert_eq!(r["messages"][0]["state"], "streaming");
    assert_eq!(r["messages"][0]["sender_type"], "agent");
}

async fn anchors_and_their_boundaries(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let a = Conn::new(&hub, &alice);
    let b = Conn::new(&hub, &bob);
    let conv = a.group(&[&bob]).await;
    let sent = fill(&a, &conv, 30).await;
    // A seq anchor: up to and including it before, strictly after it after.
    let r = b.ok(page(&conv, json!(sent[9]), 3, 4)).await;
    assert_eq!(seqs_of(&r), [&sent[7..10], &sent[10..14]].concat());
    assert_eq!(
        (r["has_before"].clone(), r["has_after"].clone()),
        (json!(true), json!(true))
    );
    // An anchor between messages (an event that is not a message) is fine.
    let r = b.ok(page(&conv, json!(sent[0] - 1), 0, 2)).await;
    assert_eq!(seqs_of(&r), sent[0..2].to_vec());
    assert_eq!(r["has_before"], false);
    assert_eq!(r["before_end"], "start");
    // Past the end, and anchor 0.
    let r = b.ok(page(&conv, json!(1_000_000), 2, 5)).await;
    assert_eq!(seqs_of(&r), sent[28..].to_vec());
    assert_eq!(r["has_after"], false);
    let r = b.ok(page(&conv, json!(0), 5, 2)).await;
    assert_eq!(seqs_of(&r), sent[0..2].to_vec());
    // The first unread: what bob read before it, what he has not after it.
    b.ok(json!({"op": "read", "conv_id": conv, "seq": sent[19]}))
        .await;
    let r = b.ok(page(&conv, json!("first_unread"), 2, 3)).await;
    assert_eq!(seqs_of(&r), [&sent[18..20], &sent[20..23]].concat());
    // Nothing read yet: everything is after the anchor.
    let carol = user(&hub, "carol", UserKind::Human, None).await;
    a.ok(json!({"op": "add_member", "conv_id": conv, "user_id": carol.id, "history": 100}))
        .await;
    let c = Conn::new(&hub, &carol);
    let r = c.ok(page(&conv, json!("first_unread"), 5, 2)).await;
    assert_eq!(seqs_of(&r), sent[0..2].to_vec());
    // Exactly the maximum on both sides.
    let r = b.ok(page(&conv, json!(sent[15]), 100, 100)).await;
    assert_eq!(seqs_of(&r), sent);
    // Refusals, with the business codes.
    for (req, want) in [
        (page(&conv, json!(5), 0, 0), "bad_request"),
        (page(&conv, json!(5), -1, 0), "bad_request"),
        (page(&conv, json!(5), 101, 0), "bad_request"),
        (page(&conv, json!(5), 0, 101), "bad_request"),
        (page(&conv, json!(-1), 5, 0), "bad_request"),
        (page(&conv, json!(1.5), 5, 0), "bad_request"),
        (page(&conv, json!("oldest"), 5, 0), "bad_request"),
        (page(&conv, json!(true), 5, 0), "bad_request"),
        (
            json!({"op": "history", "conv_id": conv, "after_seq": 0}),
            "bad_request",
        ),
        (
            json!({"op": "history", "conv_id": conv, "limit": 10}),
            "bad_request",
        ),
        (
            json!({"op": "history", "conv_id": "c_unknown"}),
            "not_found",
        ),
    ] {
        assert_eq!(code(&b.call(req.clone()).await), want, "{req}");
    }
    // Agents keep their own form; the people's form is refused for them.
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    a.ok(json!({"op": "add_member", "conv_id": conv, "user_id": bot.id}))
        .await;
    let g = Conn::new(&hub, &bot);
    assert_eq!(
        code(&g.call(page(&conv, json!(5), 5, 0)).await),
        "bad_request"
    );
    assert_eq!(
        g.call(json!({"op": "history", "conv_id": conv, "limit": 5}))
            .await["ok"],
        true
    );
}

async fn only_what_the_member_may_see(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let eve = user(&hub, "eve", UserKind::Human, None).await;
    let a = Conn::new(&hub, &alice);
    let b = Conn::new(&hub, &bob);
    let e = Conn::new(&hub, &eve);
    let conv = a.group(&[]).await;
    let early = fill(&a, &conv, 5).await;
    // Bob joins without history: he sees from his join on.
    a.ok(json!({"op": "add_member", "conv_id": conv, "user_id": bob.id, "history": 0}))
        .await;
    let during = fill(&a, &conv, 3).await;
    let r = b.ok(json!({"op": "history", "conv_id": conv})).await;
    assert_eq!(seqs_of(&r), during);
    assert_eq!(r["before_end"], "visible_from");
    // An older edit does not reveal the old message either.
    let old = a.ok(page(&conv, json!(early[0]), 1, 0)).await["messages"][0]["msg_id"].clone();
    a.ok(json!({"op": "edit", "msg_id": old, "text": "edited later"}))
        .await;
    let r = b.ok(page(&conv, json!(early[4]), 10, 0)).await;
    assert!(seqs_of(&r).is_empty());
    // Not a member, or no longer one: forbidden, whatever the anchor.
    assert_eq!(
        code(&e.call(json!({"op": "history", "conv_id": conv})).await),
        "forbidden"
    );
    a.ok(json!({"op": "remove_member", "conv_id": conv, "user_id": bob.id}))
        .await;
    let gap = fill(&a, &conv, 2).await;
    assert_eq!(
        code(&b.call(json!({"op": "history", "conv_id": conv})).await),
        "forbidden"
    );
    // Added back without history: his earlier part again, not the gap.
    a.ok(json!({"op": "add_member", "conv_id": conv, "user_id": bob.id, "history": 0}))
        .await;
    let after = fill(&a, &conv, 2).await;
    let r = b.ok(page(&conv, json!("newest"), 100, 0)).await;
    let got = seqs_of(&r);
    assert_eq!(
        got,
        [during.clone(), after.clone()].concat(),
        "not the gap {gap:?}"
    );
    assert_eq!(r["before_end"], "visible_from");
    // Whispers and drafts are side entries: never in pages.
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    a.ok(json!({"op": "add_member", "conv_id": conv, "user_id": bot.id}))
        .await;
    a.ok(json!({"op": "whisper", "conv_id": conv, "agent_id": bot.id, "text": "secret hint"}))
        .await;
    let r = a.ok(page(&conv, json!("newest"), 100, 0)).await;
    assert!(!r.to_string().contains("secret hint"));
}

async fn retention_and_the_switch(db: TestDb) {
    {
        let hub = open(single(&db.url)).await;
        let alice = user(&hub, "alice", UserKind::Human, None).await;
        let a = Conn::new(&hub, &alice);
        let conv = a.group(&[]).await;
        fill(&a, &conv, 3).await;
        drop(a);
        drop(hub);
    }
    tokio::time::sleep(Duration::from_millis(40)).await;
    let hub = hub_with(&db, |_, h| {
        h.gc_every = Duration::ZERO;
        h.sync.retention = Some(Duration::from_millis(20));
    })
    .await;
    hub.tick().await;
    let alice = hub.user_by_name("alice").unwrap();
    let a = Conn::new(&hub, &alice);
    let conv = a.ok(json!({"op": "list_convs"})).await["convs"][0]["conv_id"]
        .as_str()
        .unwrap()
        .to_string();
    let new = fill(&a, &conv, 2).await;
    let r = a.ok(json!({"op": "history", "conv_id": conv})).await;
    assert_eq!(seqs_of(&r), new);
    assert_eq!(r["before_end"], "retention", "{r}");
    drop((a, hub));
    // Switched off: empty pages that say so; membership is still checked.
    let hub = hub_with(&db, |c, _| c.history.enabled = false).await;
    let a = Conn::new(&hub, &alice);
    let r = a.ok(json!({"op": "history", "conv_id": conv})).await;
    assert_eq!(r["messages"], json!([]));
    assert_eq!(r["before_end"], "disabled");
    let eve = user(&hub, "eve", UserKind::Human, None).await;
    let e = Conn::new(&hub, &eve);
    assert_eq!(
        code(&e.call(json!({"op": "history", "conv_id": conv})).await),
        "forbidden"
    );
    assert_eq!(
        code(&a.call(page(&conv, json!(1), 0, 0)).await),
        "bad_request"
    );
}

async fn pages_are_limited_and_leave_nothing_behind(db: TestDb) {
    let hub = hub_with(&db, |c, _| {
        c.history.rate_per_second = 1.0;
        c.history.burst = 3.0;
        c.history.page_max = 10;
    })
    .await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let dev = Conn::device(&hub, &alice, "d1");
    let conv = dev.group(&[]).await;
    fill(&dev, &conv, 12).await;
    let before = hub.audit_log(1000).await.unwrap().len();
    // The device is up to date before it pages back.
    let r = dev.ok(json!({"op": "sync"})).await;
    let upto = r["convs"][0]["upto"].clone();
    dev.ok(json!({"op": "ack", "acks": [{"conv_id": conv, "seq": upto}]}))
        .await;
    // The configured maximum.
    assert_eq!(
        code(&dev.call(page(&conv, json!(5), 11, 0)).await),
        "bad_request"
    );
    for _ in 0..3 {
        assert_eq!(
            dev.call(page(&conv, json!("newest"), 10, 0)).await["ok"],
            true
        );
    }
    let r = dev.call(page(&conv, json!("newest"), 10, 0)).await;
    assert_eq!(code(&r), "rate_limited");
    let wait = r["error"]["retry_after_ms"]
        .as_u64()
        .expect("retry_after_ms");
    assert!((1..=1000).contains(&wait), "{wait}");
    // The limit is per person, over every connection.
    let other = Conn::device(&hub, &alice, "d2");
    assert_eq!(
        code(&other.call(page(&conv, json!("newest"), 1, 0)).await),
        "rate_limited"
    );
    tokio::time::sleep(Duration::from_millis(wait + 50)).await;
    assert_eq!(
        dev.call(page(&conv, json!("newest"), 1, 0)).await["ok"],
        true
    );
    // Pages do not audit, and do not move the device's sync position.
    assert_eq!(hub.audit_log(1000).await.unwrap().len(), before);
    let next = dev.send(&conv, "after paging").await["seq"].clone();
    let again = dev.ok(json!({"op": "sync"})).await;
    assert_eq!(
        again["convs"][0]["upto"], next,
        "sync goes on from where the device was: {again}"
    );
    assert!(again["convs"][0].get("reset").is_none());
    // A stopped agent may still ping; ping answers the server time.
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    dev.ok(json!({"op": "stop_agent", "agent_id": bot.id}))
        .await;
    let g = Conn::new(&hub, &bot);
    let p = g.ok(json!({"op": "ping"})).await;
    let now = now_ms();
    assert!(
        (now - 5000..=now + 5000).contains(&p["t"].as_i64().unwrap()),
        "{p}"
    );
    assert_eq!(code(&g.call(json!({"op": "list_convs"})).await), "conflict");
}

/// Pages read while messages are sent, edited and recalled: every page is
/// in order, has no duplicate, and agrees with the final state.
async fn race_pages_while_sending_and_editing(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let a = Conn::new(&hub, &alice);
    let conv = a.group(&[&bob]).await;
    for round in 0..5 {
        let sender = {
            let (hub, alice, conv) = (hub.clone(), alice.clone(), conv.clone());
            tokio::spawn(async move {
                let a = Conn::new(&hub, &alice);
                for i in 0..30 {
                    let r = a.send(&conv, &format!("r{round} {i}")).await;
                    if i % 7 == 0 {
                        a.ok(json!({"op": "edit", "msg_id": r["msg_id"], "text": format!("r{round} {i} e")}))
                            .await;
                    }
                    if i % 11 == 0 {
                        a.ok(json!({"op": "recall", "msg_id": r["msg_id"]})).await;
                    }
                }
            })
        };
        let reader = {
            let (hub, bob, conv) = (hub.clone(), bob.clone(), conv.clone());
            tokio::spawn(async move {
                let b = Conn::new(&hub, &bob);
                let mut pages = vec![];
                for _ in 0..15 {
                    let r = b.ok(page(&conv, json!("newest"), 40, 0)).await;
                    pages.push(r);
                    tokio::task::yield_now().await;
                }
                pages
            })
        };
        sender.await.unwrap();
        let pages = reader.await.unwrap();
        let b = Conn::new(&hub, &bob);
        let all = b.ok(page(&conv, json!("newest"), 100, 0)).await;
        let finals: std::collections::HashMap<i64, Value> = all["messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| (m["created_seq"].as_i64().unwrap(), m.clone()))
            .collect();
        for p in pages {
            let s = seqs_of(&p);
            assert!(
                s.windows(2).all(|w| w[0] < w[1]),
                "in order, no duplicate: {s:?}"
            );
            for m in p["messages"].as_array().unwrap() {
                let seq = m["created_seq"].as_i64().unwrap();
                if let Some(f) = finals.get(&seq) {
                    assert_eq!(m["msg_id"], f["msg_id"]);
                    assert!(
                        m["version"].as_i64() <= f["version"].as_i64(),
                        "never ahead of the final state"
                    );
                }
            }
        }
    }
}

// ------------------------------------------------------------ over a socket

async fn serve(hub: &Hub) -> String {
    let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = tcp.local_addr().unwrap().to_string();
    let app = router(AppState {
        hub: hub.clone(),
        demo_login: false,
        metrics_token: None,
    });
    tokio::spawn(async move {
        axum::serve(tcp, app.into_make_service_with_connect_info::<SocketAddr>())
            .await
            .unwrap()
    });
    addr
}

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn next_text(ws: &mut Ws) -> Value {
    loop {
        match tokio::time::timeout(Duration::from_secs(10), ws.next())
            .await
            .expect("a frame")
        {
            Some(Ok(Message::Text(t))) => return serde_json::from_str(t.as_str()).unwrap(),
            Some(Ok(_)) => continue,
            other => panic!("{other:?}"),
        }
    }
}

/// The welcome frame tells the heartbeat, `ping` works over the socket,
/// a leaving node says how long to wait before reconnecting, and a
/// deactivated person gets 403 (do not retry) rather than 401.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn heartbeat_goaway_and_refusals_over_a_socket() {
    let db = new_db(Backend::Sqlite).await.unwrap();
    let hub = open(single(&db.url)).await;
    let addr = serve(&hub).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let q = ws_query(&hub, &alice, "d1").await;
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws?{q}"))
        .await
        .unwrap();
    let w = next_text(&mut ws).await;
    assert_eq!(w["type"], "welcome");
    let idle = w["idle_ms"].as_u64().unwrap();
    let hb = w["heartbeat_ms"].as_u64().unwrap();
    assert!(hb > 0 && hb <= idle / 3, "{w}");
    ws.send(Message::Text(
        json!({"op": "ping", "rid": 1}).to_string().into(),
    ))
    .await
    .unwrap();
    let p = next_text(&mut ws).await;
    assert_eq!(
        (p["type"].clone(), p["ok"].clone()),
        (json!("res"), json!(true))
    );
    assert!(p["t"].as_i64().is_some());
    // The node leaves: a goaway frame with a wait of 0–5 s, then the close.
    hub.go_away();
    let g = next_text(&mut ws).await;
    assert_eq!(g["type"], "goaway");
    assert_eq!(g["reason"], "server_restart");
    assert!(g["retry_after_ms"].as_u64().unwrap() < 5000);
    match tokio::time::timeout(Duration::from_secs(5), ws.next())
        .await
        .unwrap()
    {
        Some(Ok(Message::Close(Some(f)))) => assert_eq!(f.reason.as_str(), "server_restart"),
        other => panic!("{other:?}"),
    }
    hub.come_back();
    // Deactivated: 403; a bad token stays 401.
    hub.set_deactivated(&alice.id, true).await.unwrap();
    let q = ws_query(&hub, &alice, "d1").await;
    match tokio_tungstenite::connect_async(format!("ws://{addr}/ws?{q}")).await {
        Err(tokio_tungstenite::tungstenite::Error::Http(r)) => assert_eq!(r.status(), 403),
        other => panic!("{:?}", other.map(|_| ())),
    }
    match tokio_tungstenite::connect_async(format!("ws://{addr}/ws?token=nope&device=d1")).await {
        Err(tokio_tungstenite::tungstenite::Error::Http(r)) => assert_eq!(r.status(), 401),
        other => panic!("{:?}", other.map(|_| ())),
    }
}
