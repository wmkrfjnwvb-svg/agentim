//! The AI message experience (docs/03-agents.md §3): handing a
//! conversation to a person and back, cards and their responses,
//! progress, sources and feedback, with refusals that leave no trace,
//! restart and races.

mod common;

use agentim_core::model::*;
use common::*;
use serde_json::{Value, json};

backend_tests!(
    an_agent_hands_off_and_gets_it_back,
    handoff_refusals_leave_no_trace,
    a_handoff_survives_a_restart,
    race_nothing_from_the_agent_while_handed_off,
    race_one_handback,
    cards_take_responses,
    card_refusals_leave_no_trace,
    progress_goes_on_the_side,
    sources_travel_with_the_message,
    feedback_reaches_the_agent_and_its_owner,
    race_once_cards_take_one_response,
);

fn code(v: &Value) -> &str {
    v["error"]["code"].as_str().unwrap_or("")
}

fn handoff(conv: &str, note: &str) -> Value {
    json!({"op": "handoff", "conv_id": conv, "note": note})
}

fn handback(conv: &str, agent: &User) -> Value {
    json!({"op": "handback", "conv_id": conv, "agent_id": agent.id, "note": "over to you again"})
}

fn say(conv: &str, text: &str) -> Value {
    json!({"op": "send", "conv_id": conv, "client_msg_id": uuid::Uuid::new_v4().to_string(), "text": text})
}

fn states(log: &Value) -> Vec<Value> {
    log["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["event"]["type"] == "agent.state")
        .map(|e| e["event"]["data"].clone())
        .collect()
}

fn inbox_handoffs(v: &Value) -> Vec<Value> {
    v["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["type"] == "handoff")
        .cloned()
        .collect()
}

async fn an_agent_hands_off_and_gets_it_back(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    let a = Conn::new(&hub, &alice);
    let mut ac = Conn::new(&hub, &alice);
    let b = Conn::new(&hub, &bob);
    let conv = a.group(&[&bob, &bot]).await;
    let mut g = Conn::new(&hub, &bot);
    // A reply in progress is cut short by the handoff.
    let q = b.send(&conv, "@bot I want a refund").await["msg_id"].clone();
    let st = g
        .ok(json!({"op": "stream_start", "conv_id": conv, "client_msg_id": "s", "reply_to": q}))
        .await["msg_id"]
        .clone();
    ac.drain();
    let r = g.ok(handoff(&conv, "refund over 100 needs a person")).await;
    assert_eq!(r["to"], json!(alice.id), "the owner by default");
    let log = a.pull(&conv, 0).await;
    let s = states(&log);
    assert_eq!(s.len(), 1);
    assert_eq!(
        (&s[0]["state"], &s[0]["handoff"], &s[0]["by"], &s[0]["note"]),
        (
            &json!("taken_over"),
            &json!(true),
            &json!(alice.id),
            &json!("refund over 100 needs a person")
        )
    );
    assert_eq!(
        msg_by_id(&log, st.as_str().unwrap()).unwrap()["state"],
        "cancelled"
    );
    // The person is told on their own channel and in their inbox.
    assert!(
        ac.drain()
            .iter()
            .any(|f| f["type"] == "side" && f["event"]["type"] == "handoff"),
        "side notice"
    );
    let inbox = a.ok(json!({"op": "inbox"})).await;
    let h = inbox_handoffs(&inbox);
    assert_eq!(h.len(), 1);
    assert_eq!(
        (&h[0]["agent_id"], &h[0]["note"]),
        (&json!(bot.id), &json!("refund over 100 needs a person"))
    );
    assert!(inbox_handoffs(&b.ok(json!({"op": "inbox"})).await).is_empty());
    // Meanwhile the agent is silent and not woken.
    assert_eq!(code(&g.call(say(&conv, "let me")).await), "taken_over");
    g.drain();
    let r = b.send(&conv, "@bot hello?").await;
    assert_eq!(r["wake"], json!([]));
    // Bob may not hand it back; alice does, and the agent wakes with the
    // note.
    assert_eq!(code(&b.call(handback(&conv, &bot)).await), "forbidden");
    a.ok(handback(&conv, &bot)).await;
    let woke = g
        .drain()
        .into_iter()
        .find(|f| f["type"] == "event" && f["event"]["type"] == "agent.state")
        .unwrap();
    assert_eq!(woke["wake"], true);
    assert_eq!(woke["event"]["data"]["note"], "over to you again");
    assert_eq!(woke["event"]["data"]["handback"], true);
    g.ok(say(&conv, "thanks, continuing")).await;
    assert!(inbox_handoffs(&a.ok(json!({"op": "inbox"})).await).is_empty());
    // To somebody named; that person hands it back.
    let r = g
        .ok(json!({"op": "handoff", "conv_id": conv, "to": "bob", "note": "your call"}))
        .await;
    assert_eq!(r["to"], json!(bob.id));
    assert_eq!(inbox_handoffs(&b.ok(json!({"op": "inbox"})).await).len(), 1);
    b.ok(handback(&conv, &bot)).await;
    // The owner's release also ends a handoff.
    g.ok(handoff(&conv, "again")).await;
    a.ok(json!({"op": "release", "conv_id": conv, "agent_id": bot.id}))
        .await;
    g.ok(say(&conv, "back")).await;
    // Audited.
    let audit = hub.audit_log(1000).await.unwrap();
    let n = |x: &str| audit.iter().filter(|e| e["action"] == x).count();
    assert_eq!((n("agent.handoff"), n("agent.handback")), (3, 2));
}

async fn handoff_refusals_leave_no_trace(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let carol = user(&hub, "carol", UserKind::Human, None).await;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    let peer = user(&hub, "peer", UserKind::Agent, Some(&alice)).await;
    let a = Conn::new(&hub, &alice);
    let conv = a.group(&[&bob, &bot, &peer]).await;
    // A conversation without the owner: bob's group with the bot.
    let b = Conn::new(&hub, &bob);
    let away = b.group(&[&bot]).await;
    let g = Conn::new(&hub, &bot);
    let long = "n".repeat(4097);
    let head = |c: &str| {
        let a = Conn::new(&hub, &alice);
        let b = Conn::new(&hub, &bob);
        let c = c.to_string();
        let away = away.clone();
        async move {
            if c == away {
                seqs(&b.pull(&c, 0).await).len()
            } else {
                seqs(&a.pull(&c, 0).await).len()
            }
        }
    };
    let (h1, h2) = (head(&conv).await, head(&away).await);
    let cases: Vec<(&Conn, Value, &str)> = vec![
        (&a, handoff(&conv, "x"), "forbidden"),
        (&g, handoff(&conv, ""), "bad_request"),
        (&g, handoff(&conv, &long), "bad_request"),
        (&g, handoff(&conv, "a\0b"), "bad_request"),
        (
            &g,
            json!({"op": "handoff", "conv_id": conv, "to": "nobody", "note": "x"}),
            "bad_request",
        ),
        (
            &g,
            json!({"op": "handoff", "conv_id": conv, "to": carol.id, "note": "x"}),
            "bad_request",
        ),
        (
            &g,
            json!({"op": "handoff", "conv_id": conv, "to": "peer", "note": "x"}),
            "bad_request",
        ),
        (&g, handoff(&away, "owner is not here"), "bad_request"),
        (&g, handoff("c_nope", "x"), "not_found"),
        (&a, handback(&conv, &bot), "conflict"),
        (
            &a,
            json!({"op": "handback", "conv_id": conv, "agent_id": "u_nope"}),
            "not_found",
        ),
        (
            &a,
            json!({"op": "handback", "conv_id": conv, "agent_id": bob.id}),
            "not_found",
        ),
    ];
    for (who, req, want) in cases {
        assert_eq!(code(&who.call(req.clone()).await), want, "{req}");
    }
    assert_eq!((head(&conv).await, head(&away).await), (h1, h2));
    // Boundaries that work: 4KB, to bob by id in his own group.
    g.ok(json!({"op": "handoff", "conv_id": away, "to": bob.id, "note": "n".repeat(4096)}))
        .await;
    assert_eq!(
        code(
            &g.call(json!({"op": "handoff", "conv_id": away, "to": "bob", "note": "twice"}))
                .await
        ),
        "conflict"
    );
    // An outsider cannot hand back; a long note is refused, the state stays.
    let c = Conn::new(&hub, &carol);
    assert_eq!(code(&c.call(handback(&away, &bot)).await), "forbidden");
    assert_eq!(
        code(
            &b.call(json!({"op": "handback", "conv_id": away, "agent_id": bot.id, "note": long}))
                .await
        ),
        "bad_request"
    );
    assert_eq!(
        code(&g.call(say(&away, "still handed off")).await),
        "taken_over"
    );
    let audit = hub.audit_log(1000).await.unwrap();
    assert_eq!(
        audit
            .iter()
            .filter(|e| e["action"] == "agent.handoff")
            .count(),
        1
    );
}

async fn a_handoff_survives_a_restart(db: TestDb) {
    let (alice, bot, conv);
    {
        let hub = open(single(&db.url)).await;
        alice = user(&hub, "alice", UserKind::Human, None).await;
        bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
        let a = Conn::new(&hub, &alice);
        conv = a.group(&[&bot]).await;
        Conn::new(&hub, &bot)
            .ok(handoff(&conv, "kept across restarts"))
            .await;
    }
    let hub = open(single(&db.url)).await;
    let a = Conn::new(&hub, &alice);
    let h = inbox_handoffs(&a.ok(json!({"op": "inbox"})).await);
    assert_eq!(h[0]["note"], "kept across restarts");
    let g = Conn::new(&hub, &bot);
    assert_eq!(code(&g.call(say(&conv, "x")).await), "taken_over");
    a.ok(handback(&conv, &bot)).await;
    g.ok(say(&conv, "back")).await;
}

/// The agent keeps sending while it hands off: nothing of it comes after
/// the handoff event until the handback.
async fn race_nothing_from_the_agent_while_handed_off(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    let a = Conn::new(&hub, &alice);
    let conv = a.group(&[&bot]).await;
    for round in 0..10 {
        let (h, b, c) = (hub.clone(), bot.clone(), conv.clone());
        let sender = tokio::spawn(async move {
            let g = Conn::new(&h, &b);
            for k in 0..6 {
                let r = g.call(say(&c, &format!("{round}-{k}"))).await;
                if r["ok"] != true {
                    assert!(["taken_over", "rate_limited"].contains(&code(&r)), "{r}");
                }
            }
        });
        Conn::new(&hub, &bot)
            .ok(handoff(&conv, &format!("round {round}")))
            .await;
        sender.await.unwrap();
        let log = a.pull(&conv, 0).await;
        let evs = log["events"].as_array().unwrap();
        let from = evs
            .iter()
            .filter(|e| {
                e["event"]["type"] == "agent.state" && e["event"]["data"]["state"] == "taken_over"
            })
            .map(|e| e["event"]["seq"].as_i64().unwrap())
            .max()
            .unwrap();
        let late: Vec<&Value> = evs
            .iter()
            .filter(|e| {
                e["event"]["seq"].as_i64().unwrap() > from
                    && e["event"]["msg"]["sender_id"] == json!(bot.id)
            })
            .collect();
        assert!(late.is_empty(), "round {round}: {late:?}");
        a.ok(handback(&conv, &bot)).await;
        tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    }
}

/// The owner and the person handed to both hand back at once: one wins.
async fn race_one_handback(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    let a = Conn::new(&hub, &alice);
    let conv = a.group(&[&bob, &bot]).await;
    for round in 0..15 {
        Conn::new(&hub, &bot)
            .ok(json!({"op": "handoff", "conv_id": conv, "to": "bob", "note": "x"}))
            .await;
        let mut tasks = vec![];
        for u in [&alice, &bob, &alice, &bob] {
            let (h, u, c, b) = (hub.clone(), u.clone(), conv.clone(), bot.clone());
            tasks.push(tokio::spawn(async move {
                Conn::new(&h, &u).call(handback(&c, &b)).await
            }));
        }
        let mut ok = 0;
        for t in tasks {
            let r = t.await.unwrap();
            if r["ok"] == true {
                ok += 1;
            } else {
                assert_eq!(code(&r), "conflict", "{r}");
            }
        }
        assert_eq!(ok, 1, "round {round}");
        let s = states(&a.pull(&conv, 0).await);
        assert_eq!(s.len(), 2 * (round + 1), "round {round}");
    }
}

fn card() -> Value {
    json!({
        "title": "Refund request",
        "blocks": [
            {"type": "text", "text": "Approve a refund of 120 EUR?"},
            {"type": "buttons", "id": "decision", "options": [
                {"id": "yes", "label": "Approve", "style": "primary"},
                {"id": "no", "label": "Decline", "style": "danger"}
            ]},
            {"type": "form", "id": "details", "submit": "Send", "fields": [
                {"id": "amount", "label": "Amount", "type": "number", "required": true},
                {"id": "why", "label": "Reason", "type": "text", "max": 20},
                {"id": "size", "label": "Size", "type": "select", "options": ["S", "M"]},
                {"id": "urgent", "label": "Urgent", "type": "checkbox"}
            ]}
        ]
    })
}

fn send_card(conv: &str, payload: Value) -> Value {
    let id = uuid::Uuid::new_v4().to_string();
    // Distinct texts without digit runs (a random run can pass as a card
    // number and be blocked by the DLP filter).
    let tag: String = id
        .chars()
        .map(|c| c.to_digit(10).map_or(c, |d| (b'g' + d as u8) as char))
        .collect();
    json!({"op": "send", "conv_id": conv, "client_msg_id": id, "text": format!("Refund request {tag}: please answer in the app"), "kind": "agentim.card", "payload": payload})
}

fn respond(msg: &Value, block: &str, action: Option<&str>, values: Option<Value>) -> Value {
    let mut v = json!({"op": "respond", "msg_id": msg, "block_id": block});
    if let Some(a) = action {
        v["action"] = json!(a);
    }
    if let Some(x) = values {
        v["values"] = x;
    }
    v
}

fn responses(log: &Value) -> Vec<Value> {
    log["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["event"]["type"] == "card.response")
        .map(|e| e["event"]["data"].clone())
        .collect()
}

async fn cards_take_responses(db: TestDb) {
    let (alice, bob, carol, bot, conv, msg);
    {
        let hub = open(single(&db.url)).await;
        alice = user(&hub, "alice", UserKind::Human, None).await;
        bob = user(&hub, "bob", UserKind::Human, None).await;
        carol = user(&hub, "carol", UserKind::Human, None).await;
        bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
        let a = Conn::new(&hub, &alice);
        conv = a.group(&[&bob, &carol, &bot]).await;
        let mut g = Conn::new(&hub, &bot);
        msg = g.ok(send_card(&conv, card())).await["msg_id"].clone();
        let log = a.pull(&conv, 0).await;
        let m = msg_by_id(&log, msg.as_str().unwrap()).unwrap();
        assert_eq!(m["kind"], "agentim.card");
        assert_eq!(m["payload"]["blocks"][1]["options"][0]["label"], "Approve");
        g.drain();
        // A button and a form; each wakes the agent with the answer.
        let b = Conn::new(&hub, &bob);
        b.ok(respond(&msg, "decision", Some("yes"), None)).await;
        let f = g.drain();
        let ev = f
            .iter()
            .find(|x| x["type"] == "event" && x["event"]["type"] == "card.response")
            .unwrap();
        assert_eq!(ev["wake"], true);
        assert_eq!(
            (&ev["event"]["data"]["action"], &ev["event"]["data"]["by"]),
            (&json!("yes"), &json!(bob.id))
        );
        Conn::new(&hub, &alice)
            .ok(respond(
                &msg,
                "details",
                None,
                Some(json!({"amount": 120.5, "why": "broken", "size": "M", "urgent": true})),
            ))
            .await;
        // Optional fields may be left out; the boundaries pass.
        Conn::new(&hub, &carol)
            .ok(respond(
                &msg,
                "details",
                None,
                Some(json!({"amount": 0, "why": "x".repeat(20)})),
            ))
            .await;
        let r = responses(&a.pull(&conv, 0).await);
        assert_eq!(r.len(), 3);
        assert_eq!(r[1]["values"]["size"], "M");
        // Once per person.
        assert_eq!(
            code(
                &b.call(respond(&msg, "details", None, Some(json!({"amount": 1}))))
                    .await
            ),
            "conflict"
        );
    }
    // After a restart the responses still count.
    let hub = open(single(&db.url)).await;
    let b = Conn::new(&hub, &bob);
    assert_eq!(
        code(&b.call(respond(&msg, "decision", Some("no"), None)).await),
        "conflict"
    );
    // A card that takes one response overall.
    let g = Conn::new(&hub, &bot);
    let mut once = card();
    once["once"] = json!(true);
    let m2 = g.ok(send_card(&conv, once)).await["msg_id"].clone();
    b.ok(respond(&m2, "decision", Some("no"), None)).await;
    assert_eq!(
        code(
            &Conn::new(&hub, &carol)
                .call(respond(&m2, "decision", Some("yes"), None))
                .await
        ),
        "conflict"
    );
    // The sender edits its card (still checked); a recalled card takes no
    // response.
    assert_eq!(
        code(
            &g.call(json!({"op": "edit", "msg_id": m2, "text": "x", "payload": {"blocks": []}}))
                .await
        ),
        "bad_request"
    );
    g.ok(json!({"op": "edit", "msg_id": m2, "text": "closed", "payload": {"blocks": [{"type": "text", "text": "Closed"}]}}))
        .await;
    let m3 = g.ok(send_card(&conv, card())).await["msg_id"].clone();
    g.ok(json!({"op": "recall", "msg_id": m3})).await;
    assert_eq!(
        code(&b.call(respond(&m3, "decision", Some("yes"), None)).await),
        "bad_request"
    );
    let _ = alice;
}

async fn card_refusals_leave_no_trace(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let outsider = user(&hub, "outsider", UserKind::Human, None).await;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    let a = Conn::new(&hub, &alice);
    let conv = a.group(&[&bob, &bot]).await;
    let g = Conn::new(&hub, &bot);
    let b = Conn::new(&hub, &bob);
    let head = || {
        let a = Conn::new(&hub, &alice);
        let c = conv.clone();
        async move { seqs(&a.pull(&c, 0).await).len() }
    };
    // Cards that are not sent.
    let mut bad_cards = vec![
        json!({"blocks": []}),
        json!({"blocks": [{"type": "script", "src": "x"}]}),
        json!({"blocks": [{"type": "text", "text": ""}]}),
        json!({"blocks": [{"type": "text", "text": "x"}], "extra": 1}),
        json!({"blocks": [{"type": "text", "text": "x"}], "once": "yes"}),
        json!({"blocks": [{"type": "buttons", "id": "a", "options": []}]}),
        json!({"blocks": [{"type": "buttons", "id": "Bad Id", "options": [{"id": "x", "label": "X"}]}]}),
        json!({"blocks": [{"type": "buttons", "id": "a", "options": [{"id": "x", "label": "X"}, {"id": "x", "label": "Y"}]}]}),
        json!({"blocks": [{"type": "buttons", "id": "a", "options": [{"id": "x", "label": ""}]}]}),
        json!({"blocks": [{"type": "buttons", "id": "a", "options": [{"id": "x", "label": "X", "style": "pink"}]}]}),
        json!({"blocks": [{"type": "buttons", "id": "a", "options": [{"id": "x", "label": "X"}]}, {"type": "form", "id": "a", "submit": "s", "fields": [{"id": "f", "label": "F", "type": "text"}]}]}),
        json!({"blocks": [{"type": "form", "id": "f", "submit": "s", "fields": [{"id": "f", "label": "F", "type": "date"}]}]}),
        json!({"blocks": [{"type": "form", "id": "f", "submit": "s", "fields": [{"id": "f", "label": "F", "type": "text", "options": ["a"]}]}]}),
        json!({"blocks": [{"type": "form", "id": "f", "submit": "s", "fields": [{"id": "f", "label": "F", "type": "select", "options": ["a", "a"]}]}]}),
        json!({"blocks": [{"type": "form", "id": "f", "submit": "s", "fields": [{"id": "f", "label": "F", "type": "text", "max": 1001}]}]}),
        json!({"blocks": [{"type": "form", "id": "f", "fields": [{"id": "f", "label": "F", "type": "text"}]}]}),
        json!({"title": "t".repeat(101), "blocks": [{"type": "text", "text": "x"}]}),
        json!({"blocks": (0..21).map(|_| json!({"type": "text", "text": "x"})).collect::<Vec<_>>()}),
        json!({"blocks": (0..5).map(|_| json!({"type": "text", "text": "x".repeat(4000)})).collect::<Vec<_>>()}),
    ];
    let eleven: Vec<Value> = (0..11)
        .map(|i| json!({"id": format!("o{i}"), "label": "L"}))
        .collect();
    bad_cards.push(json!({"blocks": [{"type": "buttons", "id": "a", "options": eleven}]}));
    let h = head().await;
    for c in bad_cards {
        assert_eq!(
            code(&g.call(send_card(&conv, c.clone())).await),
            "bad_request",
            "{c}"
        );
    }
    let mut no_payload = send_card(&conv, json!({}));
    no_payload.as_object_mut().unwrap().remove("payload");
    assert_eq!(code(&g.call(no_payload).await), "bad_request");
    let mut other = send_card(&conv, card());
    other["kind"] = json!("agentim.other");
    assert_eq!(code(&g.call(other).await), "bad_request");
    assert_eq!(head().await, h);
    // Boundaries that are fine: 20 blocks, 10 options.
    let twenty: Vec<Value> = (0..20)
        .map(|_| json!({"type": "text", "text": "x"}))
        .collect();
    g.ok(send_card(&conv, json!({"blocks": twenty}))).await;
    let ten: Vec<Value> = (0..10)
        .map(|i| json!({"id": format!("o{i}"), "label": "L"}))
        .collect();
    g.ok(send_card(
        &conv,
        json!({"blocks": [{"type": "buttons", "id": "a", "options": ten}]}),
    ))
    .await;
    // Responses that are refused.
    let msg = g.ok(send_card(&conv, card())).await["msg_id"].clone();
    let plain = a.send(&conv, "plain").await["msg_id"].clone();
    let h = head().await;
    let cases: Vec<(&Conn, Value, &str)> = vec![
        (
            &g,
            respond(&msg, "decision", Some("yes"), None),
            "bad_request",
        ),
        (
            &b,
            respond(&plain, "decision", Some("yes"), None),
            "bad_request",
        ),
        (&b, respond(&msg, "nope", Some("yes"), None), "bad_request"),
        (
            &b,
            respond(&msg, "decision", Some("maybe"), None),
            "bad_request",
        ),
        (&b, respond(&msg, "decision", None, None), "bad_request"),
        (
            &b,
            respond(&msg, "decision", Some("yes"), Some(json!({}))),
            "bad_request",
        ),
        (
            &b,
            respond(&msg, "details", Some("yes"), None),
            "bad_request",
        ),
        (
            &b,
            respond(&msg, "details", None, Some(json!({}))),
            "bad_request",
        ),
        (
            &b,
            respond(&msg, "details", None, Some(json!({"amount": "12"}))),
            "bad_request",
        ),
        (
            &b,
            respond(
                &msg,
                "details",
                None,
                Some(json!({"amount": 1, "size": "XL"})),
            ),
            "bad_request",
        ),
        (
            &b,
            respond(
                &msg,
                "details",
                None,
                Some(json!({"amount": 1, "why": "x".repeat(21)})),
            ),
            "bad_request",
        ),
        (
            &b,
            respond(
                &msg,
                "details",
                None,
                Some(json!({"amount": 1, "urgent": "yes"})),
            ),
            "bad_request",
        ),
        (
            &b,
            respond(
                &msg,
                "details",
                None,
                Some(json!({"amount": 1, "extra": 1})),
            ),
            "bad_request",
        ),
        (
            &b,
            respond(&json!("m_nope"), "decision", Some("yes"), None),
            "not_found",
        ),
    ];
    for (who, req, want) in cases {
        assert_eq!(code(&who.call(req.clone()).await), want, "{req}");
    }
    assert_eq!(
        code(
            &Conn::new(&hub, &outsider)
                .call(respond(&msg, "decision", Some("yes"), None))
                .await
        ),
        "forbidden"
    );
    assert_eq!(head().await, h);
    // Refusals did not use up the person's one response.
    b.ok(respond(&msg, "decision", Some("yes"), None)).await;
}

async fn progress_goes_on_the_side(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let mut slow = single(&db.url);
    slow.agents.progress_per_second = 1;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    let peer = user(&hub, "peer", UserKind::Agent, Some(&alice)).await;
    let a = Conn::new(&hub, &alice);
    let conv = a.group(&[&bob, &bot]).await;
    let mut b = Conn::new(&hub, &bob);
    let g = Conn::new(&hub, &bot);
    let mine = g.ok(say(&conv, "working on it")).await["msg_id"].clone();
    let theirs = a.send(&conv, "hi").await["msg_id"].clone();
    b.drain();
    let head = seqs(&a.pull(&conv, 0).await).len();
    let p = |kind: &str, text: &str| json!({"op": "progress", "conv_id": conv, "kind": kind, "text": text});
    let mut with_msg = p("tool_call", "search(\"refund policy\")");
    with_msg["msg_id"] = mine.clone();
    with_msg["data"] = json!({"tool": "search", "args": {"q": "refund policy"}});
    g.ok(with_msg).await;
    for k in ["thinking", "tool_result", "plan", "step", "error"] {
        g.ok(p(k, "x")).await;
    }
    // Members see it on the side; the log does not change.
    let got: Vec<Value> = b
        .drain()
        .into_iter()
        .filter(|f| f["type"] == "side" && f["event"]["type"] == "progress")
        .collect();
    assert_eq!(got.len(), 6);
    assert_eq!(got[0]["event"]["data"]["data"]["tool"], "search");
    assert_eq!(got[0]["event"]["data"]["msg_id"], mine);
    assert_eq!(seqs(&a.pull(&conv, 0).await).len(), head);
    // Refusals.
    let mut bad_msg = p("step", "x");
    bad_msg["msg_id"] = theirs.clone();
    let mut big = p("step", "x");
    big["data"] = json!("d".repeat(8 * 1024));
    for (who, req, want) in [
        (&g, p("dreaming", "x"), "bad_request"),
        (&g, p("step", ""), "bad_request"),
        (&g, p("step", &"x".repeat(4097)), "bad_request"),
        (&g, bad_msg, "bad_request"),
        (&g, big, "bad_request"),
        (&a, p("step", "x"), "forbidden"),
        (&Conn::new(&hub, &peer), p("step", "x"), "forbidden"),
    ] {
        assert_eq!(code(&who.call(req.clone()).await), want, "{req}");
    }
    assert!(
        b.drain().iter().all(|f| f["type"] != "side"),
        "nothing for refusals"
    );
    // 4KB text and 8KB of data are fine.
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    let mut edge = p("step", &"x".repeat(4096));
    edge["data"] = json!("d".repeat(8 * 1024 - 2));
    g.ok(edge).await;
    // The limit (agents.progress_per_second; here 1): the second report
    // at once is refused and sends nothing, a second later it is fine.
    drop((g, b));
    drop(hub);
    let hub = open(slow).await;
    let g = Conn::new(&hub, &bot);
    let mut b = Conn::new(&hub, &bob);
    g.ok(p("step", "one")).await;
    assert_eq!(code(&g.call(p("step", "two")).await), "rate_limited");
    let got = b
        .drain()
        .into_iter()
        .filter(|f| f["type"] == "side" && f["event"]["type"] == "progress")
        .count();
    assert_eq!(got, 1);
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    g.ok(p("step", "three")).await;
}

async fn sources_travel_with_the_message(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    let a = Conn::new(&hub, &alice);
    let conv = a.group(&[&bot]).await;
    let g = Conn::new(&hub, &bot);
    let src = json!([
        {"title": "Refund policy", "url": "https://example.com/refunds", "excerpt": "within 30 days"},
        {"title": "FAQ", "url": "http://example.com/faq"}
    ]);
    let r = g
        .ok(json!({"op": "send", "conv_id": conv, "client_msg_id": "s1", "text": "Refunds are possible within 30 days.", "sources": src}))
        .await;
    let log = a.pull(&conv, 0).await;
    let m = msg_by_id(&log, r["msg_id"].as_str().unwrap()).unwrap();
    assert_eq!(m["envelope"]["sources"], src);
    let s = |v: Value| json!({"op": "send", "conv_id": conv, "client_msg_id": uuid::Uuid::new_v4().to_string(), "text": "x", "sources": v});
    let one = |t: &str, u: &str| json!([{"title": t, "url": u}]);
    let head = seqs(&a.pull(&conv, 0).await).len();
    let twenty_one: Vec<Value> = (0..21)
        .map(|i| json!({"title": "t", "url": format!("https://x.org/{i}")}))
        .collect();
    for bad in [
        json!([]),
        json!(twenty_one),
        json!([{"url": "https://x.org"}]),
        one("", "https://x.org"),
        one(&"t".repeat(201), "https://x.org"),
        one("t", "ftp://x.org"),
        one("t", "https://x.org/a b"),
        one("t", "javascript:alert(1)"),
        one("t", &format!("https://x.org/{}", "a".repeat(2040))),
        json!([{"title": "t", "url": "https://x.org", "excerpt": "e".repeat(501)}]),
        json!([{"title": "t", "url": "https://x.org", "extra": 1}]),
        json!(["https://x.org"]),
    ] {
        assert_eq!(code(&g.call(s(bad.clone())).await), "bad_request", "{bad}");
    }
    assert_eq!(seqs(&a.pull(&conv, 0).await).len(), head);
    let twenty: Vec<Value> = (0..20).map(|i| json!({"title": "t".repeat(200), "url": format!("https://x.org/{i}"), "excerpt": "e".repeat(500)})).collect();
    g.ok(s(json!(twenty))).await;
    // A draft keeps its sources until it is approved.
    a.ok(json!({"op": "set_agent_policy", "conv_id": conv, "agent_id": bot.id, "approval": true}))
        .await;
    let d = g.ok(s(one("Doc", "https://x.org/doc"))).await;
    let r = a
        .ok(json!({"op": "approve", "draft_id": d["draft_id"]}))
        .await;
    let log = a.pull(&conv, 0).await;
    let ev = log["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["event"]["seq"] == r["seq"])
        .unwrap()
        .clone();
    assert_eq!(ev["event"]["msg"]["envelope"]["sources"][0]["title"], "Doc");
}

async fn feedback_reaches_the_agent_and_its_owner(db: TestDb) {
    let (alice, bob, bot, conv, answer);
    {
        let hub = open(single(&db.url)).await;
        alice = user(&hub, "alice", UserKind::Human, None).await;
        bob = user(&hub, "bob", UserKind::Human, None).await;
        let carol = user(&hub, "carol", UserKind::Human, None).await;
        bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
        let a = Conn::new(&hub, &alice);
        conv = a.group(&[&bob, &bot]).await;
        let mut g = Conn::new(&hub, &bot);
        let mut ac = Conn::new(&hub, &alice);
        let mut b = Conn::new(&hub, &bob);
        answer = g.ok(say(&conv, "the answer is 42")).await["msg_id"].clone();
        let human = b.send(&conv, "thanks").await["msg_id"].clone();
        for c in [&mut g, &mut ac, &mut b] {
            c.drain();
        }
        let fb = |rating: Value, reason: Option<&str>| {
            let mut v = json!({"op": "feedback", "msg_id": answer, "rating": rating});
            if let Some(r) = reason {
                v["reason"] = json!(r);
            }
            v
        };
        let sides = |c: &mut Conn| -> usize {
            c.drain()
                .iter()
                .filter(|f| f["type"] == "side" && f["event"]["type"] == "feedback")
                .count()
        };
        let head = seqs(&a.pull(&conv, 0).await).len();
        assert_eq!(b.ok(fb(json!("up"), None)).await["changed"], true);
        assert_eq!((sides(&mut g), sides(&mut ac), sides(&mut b)), (1, 1, 0));
        // The same again changes nothing and says nothing.
        assert_eq!(b.ok(fb(json!("up"), None)).await["changed"], false);
        assert_eq!(sides(&mut g), 0);
        assert_eq!(
            b.ok(fb(json!("down"), Some("wrong number"))).await["changed"],
            true
        );
        assert_eq!(sides(&mut g), 1);
        assert_eq!(b.ok(fb(Value::Null, None)).await["changed"], true);
        assert_eq!(b.ok(fb(Value::Null, None)).await["changed"], false);
        a.ok(fb(json!("up"), Some(&"r".repeat(500)))).await;
        b.ok(fb(json!("down"), Some("still wrong"))).await;
        assert_eq!(seqs(&a.pull(&conv, 0).await).len(), head, "not in the log");
        sides(&mut g);
        // Refusals.
        let mut on_human = fb(json!("up"), None);
        on_human["msg_id"] = human.clone();
        for (who, req, want) in [
            (&b, on_human, "bad_request"),
            (&g, fb(json!("up"), None), "forbidden"),
            (&b, fb(json!("meh"), None), "bad_request"),
            (&b, fb(Value::Null, Some("why")), "bad_request"),
            (&b, fb(json!("up"), Some("")), "bad_request"),
            (&b, fb(json!("up"), Some(&"r".repeat(501))), "bad_request"),
            (&Conn::new(&hub, &carol), fb(json!("up"), None), "forbidden"),
        ] {
            assert_eq!(code(&who.call(req.clone()).await), want, "{req}");
        }
        assert_eq!(sides(&mut g), 0);
    }
    // The owner's summary, also after a restart.
    let hub = open(single(&db.url)).await;
    let a = Conn::new(&hub, &alice);
    let l = a
        .ok(json!({"op": "list_feedback", "agent_id": bot.id}))
        .await;
    assert_eq!((&l["up"], &l["down"]), (&json!(1), &json!(1)));
    let items = l["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    let bobs = items.iter().find(|i| i["by"] == json!(bob.id)).unwrap();
    assert_eq!(
        (&bobs["rating"], &bobs["reason"], &bobs["msg_id"]),
        (&json!("down"), &json!("still wrong"), &answer)
    );
    let one = a
        .ok(json!({"op": "list_feedback", "agent_id": bot.id, "limit": 1}))
        .await;
    assert_eq!(one["items"].as_array().unwrap().len(), 1);
    for (who, req, want) in [
        (
            &Conn::new(&hub, &bob),
            json!({"op": "list_feedback", "agent_id": bot.id}),
            "forbidden",
        ),
        (
            &a,
            json!({"op": "list_feedback", "agent_id": "u_nope"}),
            "not_found",
        ),
        (
            &a,
            json!({"op": "list_feedback", "agent_id": bot.id, "limit": 0}),
            "bad_request",
        ),
        (
            &a,
            json!({"op": "list_feedback", "agent_id": bot.id, "limit": 101}),
            "bad_request",
        ),
    ] {
        assert_eq!(code(&who.call(req.clone()).await), want, "{req}");
    }
    let _ = conv;
}

/// Six people answer a card that takes one response: one wins.
async fn race_once_cards_take_one_response(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    let mut people = vec![];
    for i in 0..6 {
        people.push(user(&hub, &format!("p{i}"), UserKind::Human, None).await);
    }
    let mut members: Vec<&User> = people.iter().collect();
    members.push(&bot);
    let a = Conn::new(&hub, &alice);
    let conv = a.group(&members).await;
    let g = Conn::new(&hub, &bot);
    for round in 0..10 {
        let mut c = card();
        c["once"] = json!(true);
        let msg = g.ok(send_card(&conv, c)).await["msg_id"].clone();
        let mut tasks = vec![];
        for p in &people {
            let (h, p, m) = (hub.clone(), p.clone(), msg.clone());
            tasks.push(tokio::spawn(async move {
                Conn::new(&h, &p)
                    .call(respond(&m, "decision", Some("yes"), None))
                    .await
            }));
        }
        let mut ok = 0;
        for t in tasks {
            let r = t.await.unwrap();
            if r["ok"] == true {
                ok += 1;
            } else {
                assert_eq!(code(&r), "conflict", "{r}");
            }
        }
        assert_eq!(ok, 1, "round {round}");
        let n = responses(&a.pull(&conv, 0).await)
            .iter()
            .filter(|r| r["msg_id"] == msg)
            .count();
        assert_eq!(n, 1, "round {round}");
        tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    }
}
