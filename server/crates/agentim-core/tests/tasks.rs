//! Task channels, claims and the floor (docs/03-agents.md §8): task states and who
//! may move them, an ended task silencing agents, claims with one winner,
//! coordinated agents speaking only with the floor, refusals that leave no
//! trace, restart, and the races between them.

mod common;

use agentim_core::model::*;
use common::*;
use serde_json::{Value, json};
use sqlx::Connection;

backend_tests!(
    tasks_move_through_their_states,
    an_ended_task_silences_agents,
    task_refusals_leave_no_trace,
    requests_are_claimed_once,
    coordinated_agents_speak_with_the_floor,
    race_one_claim_wins,
    race_nothing_from_agents_after_a_task_ends,
);

fn code(v: &Value) -> &str {
    v["error"]["code"].as_str().unwrap_or("")
}

fn task(title: &str, members: &[&User], assignee: Option<&User>) -> Value {
    let mut v = json!({"op": "create_conv", "kind": "task", "title": title, "members": members.iter().map(|u| u.id.clone()).collect::<Vec<_>>()});
    if let Some(a) = assignee {
        v["assignee"] = json!(a.id);
    }
    v
}

fn upd(conv: &str, state: &str) -> Value {
    json!({"op": "update_task", "conv_id": conv, "state": state})
}

/// Task events of a conversation's log: (state, by).
async fn task_events(c: &Conn, conv: &str) -> Vec<(String, String)> {
    c.pull(conv, 0).await["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["event"]["type"] == "task.updated")
        .map(|e| {
            (
                e["event"]["data"]["state"].as_str().unwrap().to_string(),
                e["event"]["data"]["by"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

fn wakes(c: &mut Conn, kind: &str) -> Vec<bool> {
    c.drain()
        .into_iter()
        .filter(|f| f["type"] == "event" && f["event"]["type"] == kind)
        .map(|f| f["wake"] == true)
        .collect()
}

async fn tasks_move_through_their_states(db: TestDb) {
    let (alice, conv);
    {
        let hub = open(single(&db.url)).await;
        alice = user(&hub, "alice", UserKind::Human, None).await;
        let bob = user(&hub, "bob", UserKind::Human, None).await;
        let worker = user(&hub, "worker", UserKind::Agent, Some(&alice)).await;
        let a = Conn::new(&hub, &alice);
        let b = Conn::new(&hub, &bob);
        let mut w = Conn::new(&hub, &worker);
        conv = a.ok(task("ship v1", &[&bob, &worker], Some(&worker))).await["conv_id"]
            .as_str()
            .unwrap()
            .to_string();
        // The assignee is woken by the assignment.
        assert_eq!(wakes(&mut w, "task.updated"), vec![true]);
        let list = a.ok(json!({"op": "list_convs"})).await;
        let c = list["convs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["conv_id"] == json!(conv))
            .unwrap()
            .clone();
        assert_eq!(c["kind"], "task");
        // Any member moves it between open states; the agent's own change
        // does not wake it.
        w.ok(upd(&conv, "working")).await;
        assert_eq!(wakes(&mut w, "task.updated"), vec![false]);
        b.ok(upd(&conv, "input_required")).await;
        assert_eq!(wakes(&mut w, "task.updated"), vec![true]);
        // Ending it: the assignee may, another member may not.
        assert_eq!(code(&b.call(upd(&conv, "completed")).await), "forbidden");
        let mut n = upd(&conv, "completed");
        n["note"] = json!("released as v1.0");
        w.ok(n).await;
        assert_eq!(
            task_events(&a, &conv).await,
            vec![
                ("submitted".into(), alice.id.clone()),
                ("working".into(), worker.id.clone()),
                ("input_required".into(), bob.id.clone()),
                ("completed".into(), worker.id.clone()),
            ]
        );
    }
    // The state survives a restart.
    let hub = open(single(&db.url)).await;
    let worker = hub.user_by_name("worker").unwrap();
    let r = Conn::new(&hub, &worker)
        .call(json!({"op": "send", "conv_id": conv, "client_msg_id": "late", "text": "late"}))
        .await;
    assert_eq!(code(&r), "conflict", "{r}");
    // The creator reopens it.
    Conn::new(&hub, &alice).ok(upd(&conv, "working")).await;
    Conn::new(&hub, &worker)
        .ok(json!({"op": "send", "conv_id": conv, "client_msg_id": "back", "text": "back to work"}))
        .await;
}

async fn an_ended_task_silences_agents(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let worker = user(&hub, "worker", UserKind::Agent, Some(&alice)).await;
    let helper = user(&hub, "helper", UserKind::Agent, Some(&alice)).await;
    let a = Conn::new(&hub, &alice);
    let b = Conn::new(&hub, &bob);
    let mut w = Conn::new(&hub, &worker);
    let conv = a
        .ok(task("t", &[&bob, &worker, &helper], Some(&worker)))
        .await["conv_id"]
        .as_str()
        .unwrap()
        .to_string();
    let q = a.send(&conv, "@worker status?").await["msg_id"].clone();
    // A draft written before the end is not published after it.
    a.ok(
        json!({"op": "set_agent_policy", "conv_id": conv, "agent_id": helper.id, "approval": true}),
    )
    .await;
    let draft = Conn::new(&hub, &helper)
        .ok(json!({"op": "send", "conv_id": conv, "client_msg_id": "d", "text": "drafted"}))
        .await["draft_id"]
        .clone();
    a.ok(upd(&conv, "canceled")).await;
    let head = seqs(&a.pull(&conv, 0).await).len();
    let approve = json!({"op": "approve", "draft_id": draft});
    assert_eq!(code(&a.call(approve.clone()).await), "conflict");
    assert_eq!(seqs(&a.pull(&conv, 0).await).len(), head);
    w.drain();
    // Agents cannot speak, stream or claim; people still talk.
    for req in [
        json!({"op": "send", "conv_id": conv, "client_msg_id": "x", "text": "x"}),
        json!({"op": "stream_start", "conv_id": conv, "client_msg_id": "s", "reply_to": q}),
    ] {
        assert_eq!(code(&w.call(req.clone()).await), "conflict", "{req}");
    }
    let r = b.send(&conv, "@worker are you there").await;
    assert_eq!(r["wake"], json!([]), "nobody is woken");
    let got = w.drain();
    assert!(
        got.iter()
            .filter(|f| f["type"] == "event")
            .all(|f| f["wake"] == false),
        "{got:?}"
    );
    // An agent does not reopen it; a person in charge does, others not.
    assert_eq!(code(&w.call(upd(&conv, "working")).await), "conflict");
    assert_eq!(code(&b.call(upd(&conv, "working")).await), "forbidden");
    a.ok(upd(&conv, "working")).await;
    let r = b.send(&conv, "@worker now?").await;
    assert_eq!(r["wake"], json!([worker.id]));
    a.ok(approve).await;
}

async fn task_refusals_leave_no_trace(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let carol = user(&hub, "carol", UserKind::Human, None).await;
    let worker = user(&hub, "worker", UserKind::Agent, Some(&alice)).await;
    let a = Conn::new(&hub, &alice);
    let b = Conn::new(&hub, &bob);
    // Creation: an assignee only for tasks, and only a member.
    let mut g = json!({"op": "create_conv", "kind": "group", "title": "g", "members": [bob.id], "assignee": bob.id});
    assert_eq!(code(&a.call(g.clone()).await), "bad_request");
    g["kind"] = json!("task");
    g["assignee"] = json!(carol.id);
    assert_eq!(code(&a.call(g).await), "bad_request");
    assert_eq!(
        a.ok(json!({"op": "list_convs"})).await["convs"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    let conv = a.ok(task("t", &[&bob, &worker], None)).await["conv_id"]
        .as_str()
        .unwrap()
        .to_string();
    let group = a.group(&[&bob]).await;
    let head = seqs(&a.pull(&conv, 0).await).len();
    let long = "n".repeat(4097);
    let cases: Vec<(&Conn, Value, &str)> = vec![
        (&a, upd(&group, "working"), "bad_request"),
        (&a, upd(&conv, "done"), "bad_request"),
        (
            &a,
            json!({"op": "update_task", "conv_id": conv}),
            "bad_request",
        ),
        (
            &a,
            json!({"op": "update_task", "conv_id": conv, "state": "working", "note": long}),
            "bad_request",
        ),
        (
            &a,
            json!({"op": "update_task", "conv_id": conv, "state": "working", "note": ""}),
            "bad_request",
        ),
        (&a, upd(&conv, "submitted"), "conflict"),
        (
            &a,
            json!({"op": "update_task", "conv_id": conv, "assignee": carol.id}),
            "bad_request",
        ),
        (
            &b,
            json!({"op": "update_task", "conv_id": conv, "assignee": worker.id}),
            "forbidden",
        ),
        (&a, upd("c_nope", "working"), "not_found"),
    ];
    for (who, req, want) in cases {
        assert_eq!(code(&who.call(req.clone()).await), want, "{req}");
    }
    let r = Conn::new(&hub, &carol).call(upd(&conv, "working")).await;
    assert_eq!(code(&r), "forbidden");
    assert_eq!(seqs(&a.pull(&conv, 0).await).len(), head, "no trace");
    // Reassigning (creator) and assigning the same again.
    a.ok(json!({"op": "update_task", "conv_id": conv, "assignee": "worker"}))
        .await;
    let r = a
        .call(json!({"op": "update_task", "conv_id": conv, "assignee": worker.id}))
        .await;
    assert_eq!(code(&r), "conflict");
}

async fn requests_are_claimed_once(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let outsider = user(&hub, "outsider", UserKind::Human, None).await;
    let asker = user(&hub, "asker", UserKind::Agent, Some(&alice)).await;
    let w1 = user(&hub, "w1", UserKind::Agent, Some(&alice)).await;
    let w2 = user(&hub, "w2", UserKind::Agent, Some(&alice)).await;
    let a = Conn::new(&hub, &alice);
    let conv = a.group(&[&asker, &w1, &w2]).await;
    let (ca, c1, c2) = (
        Conn::new(&hub, &asker),
        Conn::new(&hub, &w1),
        Conn::new(&hub, &w2),
    );
    let req = ca
        .ok(json!({"op": "send", "conv_id": conv, "client_msg_id": "r", "text": "who can translate this?", "intent": "request", "topics": ["translate"]}))
        .await["msg_id"]
        .clone();
    let plain = a.send(&conv, "just chatting").await["msg_id"].clone();
    let claim = |m: &Value| json!({"op": "claim", "msg_id": m});
    let release = |m: &Value| json!({"op": "release_claim", "msg_id": m});
    for (who, r, want) in [
        (&c1, claim(&plain), "bad_request"),
        (&ca, claim(&req), "bad_request"),
        (&c1, claim(&json!("m_nope")), "not_found"),
        (&c1, release(&req), "not_found"),
    ] {
        assert_eq!(code(&who.call(r.clone()).await), want, "{r}");
    }
    assert_eq!(
        code(&Conn::new(&hub, &outsider).call(claim(&req)).await),
        "forbidden"
    );
    c1.ok(claim(&req)).await;
    assert_eq!(code(&c2.call(claim(&req)).await), "conflict");
    assert_eq!(code(&c1.call(claim(&req)).await), "conflict");
    assert_eq!(code(&c2.call(release(&req)).await), "forbidden");
    let log = a.pull(&conv, 0).await;
    let claimed: Vec<&Value> = log["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["event"]["type"] == "msg.claimed")
        .collect();
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0]["event"]["data"]["by"], json!(w1.id));
    // Given back, somebody else takes it; people can claim too.
    c1.ok(release(&req)).await;
    assert_eq!(code(&c1.call(release(&req)).await), "not_found");
    c2.ok(claim(&req)).await;
    let req2 = ca
        .ok(json!({"op": "send", "conv_id": conv, "client_msg_id": "r2", "text": "second", "intent": "request"}))
        .await["msg_id"]
        .clone();
    a.ok(claim(&req2)).await;
    // A recalled request cannot be claimed.
    let req3 = ca
        .ok(json!({"op": "send", "conv_id": conv, "client_msg_id": "r3", "text": "third", "intent": "request"}))
        .await["msg_id"]
        .clone();
    ca.ok(json!({"op": "recall", "msg_id": req3})).await;
    assert_eq!(code(&c1.call(claim(&req3)).await), "bad_request");
}

async fn coordinated_agents_speak_with_the_floor(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let x = user(&hub, "x", UserKind::Agent, Some(&alice)).await;
    let y = user(&hub, "y", UserKind::Agent, Some(&alice)).await;
    let coord = user(&hub, "coord", UserKind::Agent, Some(&alice)).await;
    let a = Conn::new(&hub, &alice);
    let conv = a.group(&[&bob, &x, &y]).await;
    for g in [&x, &y] {
        a.ok(json!({"op": "set_agent_policy", "conv_id": conv, "agent_id": g.id, "trigger": "coordinated"}))
            .await;
    }
    let mut cx = Conn::new(&hub, &x);
    let cy = Conn::new(&hub, &y);
    let say = |t: &str| json!({"op": "send", "conv_id": conv, "client_msg_id": format!("{t}-{}", t.len()), "text": t});
    // Everything arrives, nothing wakes, nobody speaks without the floor.
    a.send(&conv, "@x hello").await;
    let f = cx.drain();
    assert!(
        f.iter()
            .any(|e| e["event"]["msg"]["content"] == "@x hello" && e["wake"] == false),
        "{f:?}"
    );
    assert_eq!(code(&cx.call(say("unasked")).await), "conflict");
    // ...except to answer what is addressed to it.
    let q = a.send(&conv, "@y one question").await["msg_id"].clone();
    cy.ok(json!({"op": "send", "conv_id": conv, "client_msg_id": "ans", "text": "an answer", "reply_to": q}))
        .await;
    // The floor: granted by a manager, wakes the holder, one holder at once.
    let grant = |g: &User, ttl: Value| json!({"op": "floor_grant", "conv_id": conv, "agent_id": g.id, "ttl_secs": ttl});
    a.ok(grant(&x, json!(60))).await;
    assert_eq!(wakes(&mut cx, "floor.granted"), vec![true]);
    cx.ok(say("my turn")).await;
    assert_eq!(code(&cy.call(say("me too")).await), "conflict");
    a.ok(grant(&y, json!(60))).await;
    assert_eq!(code(&cx.call(say("still mine?")).await), "conflict");
    cy.ok(say("now me")).await;
    // Releasing: the holder or a manager; not twice.
    let release = json!({"op": "floor_release", "conv_id": conv});
    assert_eq!(code(&cx.call(release.clone()).await), "forbidden");
    cy.ok(release.clone()).await;
    assert_eq!(code(&a.call(release.clone()).await), "conflict");
    assert_eq!(code(&cy.call(say("after")).await), "conflict");
    // Refusals.
    let b = Conn::new(&hub, &bob);
    assert_eq!(code(&b.call(grant(&x, json!(60))).await), "forbidden");
    assert_eq!(code(&a.call(grant(&bob, json!(60))).await), "bad_request");
    assert_eq!(
        code(
            &a.call(json!({"op": "floor_grant", "conv_id": conv, "agent_id": "nobody"}))
                .await
        ),
        "not_found"
    );
    for ttl in [json!(9), json!(3601)] {
        assert_eq!(
            code(&a.call(grant(&x, ttl.clone())).await),
            "bad_request",
            "{ttl}"
        );
    }
    // Expired floors are no floors.
    a.ok(grant(&x, json!(60))).await;
    expire_floor(&db, &conv).await;
    assert_eq!(code(&cx.call(say("too late")).await), "conflict");
    // A coordinator agent: the agent-started group makes it admin.
    let cc = Conn::new(&hub, &coord);
    let g2 = cc
        .ok(json!({"op": "create_conv", "kind": "group", "title": "panel", "members": [x.id, y.id]}))
        .await["conv_id"]
        .as_str()
        .unwrap()
        .to_string();
    a.ok(json!({"op": "set_agent_policy", "conv_id": g2, "agent_id": x.id, "trigger": "coordinated"}))
        .await;
    cc.ok(json!({"op": "floor_grant", "conv_id": g2, "agent_id": x.id}))
        .await;
    cx.ok(json!({"op": "send", "conv_id": g2, "client_msg_id": "p1", "text": "speaking in the panel"}))
        .await;
}

async fn expire_floor(db: &TestDb, conv: &str) {
    sqlx::any::install_default_drivers();
    let d = agentim_core::storage::Dialect::from_url(&db.url).unwrap();
    let mut c = sqlx::AnyConnection::connect(&db.url.replace("mariadb://", "mysql://"))
        .await
        .unwrap();
    sqlx::query(d.q("UPDATE floors SET expires_at = 1 WHERE conv_id = ?"))
        .bind(conv.to_string())
        .execute(&mut c)
        .await
        .unwrap();
}

/// Many claimers at once: exactly one wins, one claim is recorded.
async fn race_one_claim_wins(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let asker = user(&hub, "asker", UserKind::Agent, Some(&alice)).await;
    let mut workers = vec![];
    for i in 0..6 {
        workers.push(user(&hub, &format!("w{i}"), UserKind::Agent, Some(&alice)).await);
    }
    let mut members: Vec<&User> = workers.iter().collect();
    members.push(&asker);
    let a = Conn::new(&hub, &alice);
    let conv = a.group(&members).await;
    let ca = Conn::new(&hub, &asker);
    for round in 0..15 {
        let req = ca
            .ok(json!({"op": "send", "conv_id": conv, "client_msg_id": format!("r{round}"), "text": format!("job {round}"), "intent": "request"}))
            .await["msg_id"]
            .clone();
        let mut tasks = vec![];
        for w in &workers {
            let (hub, w, req) = (hub.clone(), w.clone(), req.clone());
            tasks.push(tokio::spawn(async move {
                Conn::new(&hub, &w)
                    .call(json!({"op": "claim", "msg_id": req}))
                    .await
            }));
        }
        let mut wins = 0;
        for t in tasks {
            let r = t.await.unwrap();
            if r["ok"] == true {
                wins += 1;
            } else {
                assert_eq!(code(&r), "conflict", "round {round}: {r}");
            }
        }
        assert_eq!(wins, 1, "round {round}");
        let log = a.pull(&conv, 0).await;
        let n = log["events"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["event"]["type"] == "msg.claimed" && e["event"]["data"]["msg_id"] == req)
            .count();
        assert_eq!(n, 1, "round {round}");
        // Let the asker's rate limit refill.
        tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    }
}

/// Agents keep sending while the assignee ends the task: nothing from an
/// agent comes after the terminal event.
async fn race_nothing_from_agents_after_a_task_ends(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let mut agents = vec![];
    for i in 0..4 {
        agents.push(user(&hub, &format!("g{i}"), UserKind::Agent, Some(&alice)).await);
    }
    let refs: Vec<&User> = agents.iter().collect();
    let a = Conn::new(&hub, &alice);
    for round in 0..10 {
        let conv = a
            .ok(task(&format!("t{round}"), &refs, Some(&agents[0])))
            .await["conv_id"]
            .as_str()
            .unwrap()
            .to_string();
        let mut tasks = vec![];
        for (i, g) in agents.iter().enumerate().skip(1) {
            let (hub, g, conv) = (hub.clone(), g.clone(), conv.clone());
            tasks.push(tokio::spawn(async move {
                let c = Conn::new(&hub, &g);
                for k in 0..3 {
                    let _ = c
                        .call(json!({"op": "send", "conv_id": conv, "client_msg_id": format!("{i}-{k}"), "text": format!("{i} {k}")}))
                        .await;
                }
            }));
        }
        let (hub2, end, conv2) = (hub.clone(), agents[0].clone(), conv.clone());
        tasks.push(tokio::spawn(async move {
            Conn::new(&hub2, &end).ok(upd(&conv2, "completed")).await;
        }));
        for t in tasks {
            t.await.unwrap();
        }
        let log = a.pull(&conv, 0).await;
        let evs = log["events"].as_array().unwrap();
        let end_seq = evs
            .iter()
            .find(|e| {
                e["event"]["type"] == "task.updated" && e["event"]["data"]["state"] == "completed"
            })
            .unwrap()["event"]["seq"]
            .as_i64()
            .unwrap();
        let late: Vec<&Value> = evs
            .iter()
            .filter(|e| {
                e["event"]["seq"].as_i64().unwrap() > end_seq
                    && e["event"]["msg"]["sender_type"] == "agent"
            })
            .collect();
        assert!(late.is_empty(), "round {round}: {late:?}");
        tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    }
}
