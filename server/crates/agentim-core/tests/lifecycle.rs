//! An owner's emergency stop, resume and destroy of an agent (docs/09
//! §11.3): every path the agent acts through is closed, every conversation
//! records it, refusals leave no trace, restart and an interrupted destroy,
//! and the races with an agent that keeps acting.

mod common;

use std::time::Duration;

use agentim_core::hub::Hub;
use agentim_core::model::*;
use common::*;
use serde_json::{Value, json};

backend_tests!(
    a_stopped_agent_can_do_nothing,
    stop_refusals_leave_no_trace,
    stop_survives_a_restart,
    destroying_removes_everything_but_the_record,
    an_interrupted_destroy_is_finished_at_start,
    race_nothing_after_the_stop,
    race_one_stop_one_destroy,
);

fn code(v: &Value) -> &str {
    v["error"]["code"].as_str().unwrap_or("")
}

fn stop(a: &User) -> Value {
    json!({"op": "stop_agent", "agent_id": a.id})
}

fn resume(a: &User) -> Value {
    json!({"op": "resume_agent", "agent_id": a.id})
}

fn destroy(a: &User) -> Value {
    json!({"op": "destroy_agent", "agent_id": a.id, "confirm": a.name})
}

fn say(conv: &str, text: &str) -> Value {
    json!({"op": "send", "conv_id": conv, "client_msg_id": uuid::Uuid::new_v4().to_string(), "text": text})
}

/// `agent.state` events of a conversation: (state, reason).
async fn states(c: &Conn, conv: &str) -> Vec<(String, String)> {
    c.pull(conv, 0).await["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["event"]["type"] == "agent.state")
        .map(|e| {
            let d = &e["event"]["data"];
            (
                d["state"].as_str().unwrap().to_string(),
                d["reason"].as_str().unwrap_or("").to_string(),
            )
        })
        .collect()
}

async fn audits(hub: &Hub, action: &str) -> usize {
    hub.audit_log(10_000)
        .await
        .unwrap()
        .iter()
        .filter(|a| a["action"] == action)
        .count()
}

async fn count(db: &TestDb, sql: &str) -> i64 {
    use sqlx::{Connection, Row};
    sqlx::any::install_default_drivers();
    let mut c = sqlx::AnyConnection::connect(&db.url).await.unwrap();
    sqlx::query(sqlx::AssertSqlSafe(sql.to_string()))
        .fetch_one(&mut c)
        .await
        .unwrap()
        .get::<i64, _>(0)
}

async fn a_stopped_agent_can_do_nothing(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    let peer = user(&hub, "peer", UserKind::Agent, Some(&alice)).await;
    let a = Conn::new(&hub, &alice);
    let g1 = a.group(&[&bob, &bot, &peer]).await;
    let g2 = a.group(&[&bot]).await;
    let dm = Conn::new(&hub, &bob)
        .ok(json!({"op": "create_conv", "kind": "dm", "title": "dm", "members": [bot.id]}))
        .await["conv_id"]
        .as_str()
        .unwrap()
        .to_string();
    let g = Conn::new(&hub, &bot);
    // Before: a live stream, the floor, a draft, a claim, a checkpoint.
    let q = a.send(&g1, "@bot write it").await["msg_id"].clone();
    let st = g
        .ok(json!({"op": "stream_start", "conv_id": g1, "client_msg_id": "s", "reply_to": q}))
        .await["msg_id"]
        .clone();
    a.ok(json!({"op": "set_agent_policy", "conv_id": g1, "agent_id": bot.id, "trigger": "coordinated"}))
        .await;
    a.ok(json!({"op": "floor_grant", "conv_id": g1, "agent_id": bot.id}))
        .await;
    a.ok(json!({"op": "set_agent_policy", "conv_id": g2, "agent_id": bot.id, "approval": true}))
        .await;
    let draft = g.ok(say(&g2, "drafted")).await["draft_id"].clone();
    let req = Conn::new(&hub, &peer)
        .ok(json!({"op": "send", "conv_id": g1, "client_msg_id": "r", "text": "who?", "intent": "request"}))
        .await["msg_id"]
        .clone();
    g.ok(json!({"op": "claim", "msg_id": req})).await;
    g.ok(json!({"op": "state_put", "key": "k", "value": 1, "version": 0}))
        .await;
    // Stop: connections go at once.
    let r = a
        .ok(json!({"op": "stop_agent", "agent_id": bot.id, "reason": "runaway costs"}))
        .await;
    assert_eq!(
        (r["stopped"].clone(), r["conversations"].clone()),
        (json!(true), json!(3))
    );
    // The kick is remembered even when nobody waits for it yet.
    tokio::time::timeout(Duration::from_secs(5), g.s.kicked())
        .await
        .expect("disconnected");
    // Every path is closed.
    let g = Conn::new(&hub, &bot);
    for req in [
        say(&g1, "still here"),
        json!({"op": "sync"}),
        json!({"op": "history", "conv_id": g1}),
        json!({"op": "state_get", "key": "k"}),
        json!({"op": "state_put", "key": "k", "value": 2, "version": 1}),
        json!({"op": "stream_start", "conv_id": g1, "client_msg_id": "s2"}),
        json!({"op": "request_human", "conv_id": g1, "kind": "approve", "title": "t", "client_req_id": "x"}),
        json!({"op": "list_convs"}),
        json!({"op": "release_claim", "msg_id": req}),
    ] {
        assert_eq!(code(&g.call(req.clone()).await), "conflict", "{req}");
    }
    g.ok(json!({"op": "hello"})).await;
    // Through the open path too (the system acting as the agent).
    let r = hub
        .act_as(&bot, serde_json::from_value(say(&g2, "via api")).unwrap())
        .await;
    assert_eq!(r.unwrap_err().code, "conflict");
    // Every conversation records it; the stream was cancelled and the
    // floor freed; the draft stays but cannot be published while stopped.
    for c in [&g1, &g2, &dm] {
        assert_eq!(
            states(&a_or_bob(&hub, &alice, &bob, c, &dm), c).await,
            vec![("stopped".to_string(), "owner".to_string())],
            "{c}"
        );
    }
    let log = a.pull(&g1, 0).await;
    let ev = log["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["event"]["type"] == "agent.state")
        .unwrap()
        .clone();
    assert_eq!(ev["event"]["data"]["note"], "runaway costs");
    assert_eq!(
        msg_by_id(&log, st.as_str().unwrap()).unwrap()["state"],
        "cancelled"
    );
    assert!(
        log["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["event"]["type"] == "floor.released"),
        "{log}"
    );
    assert_eq!(
        code(&a.call(json!({"op": "approve", "draft_id": draft})).await),
        "conflict"
    );
    let agents = a.ok(json!({"op": "list_agents"})).await;
    let me = agents["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["agent"]["id"] == json!(bot.id))
        .unwrap()
        .clone();
    assert_eq!(
        (me["stopped"].clone(), me["destroyed"].clone()),
        (json!(true), json!(false))
    );
    // People and other agents carry on; the stopped agent is not woken.
    assert_eq!(a.send(&g2, "@bot are you there").await["wake"], json!([]));
    a.send(&g1, "people talk").await;
    Conn::new(&hub, &peer).ok(say(&g1, "peer talks")).await;
    // Resume: everything works again; the claim and draft are still its.
    a.ok(resume(&bot)).await;
    for c in [&g1, &g2] {
        assert_eq!(
            states(&a, c).await.last().unwrap(),
            &("running".to_string(), "owner".to_string())
        );
    }
    g.ok(json!({"op": "release_claim", "msg_id": req})).await;
    a.ok(json!({"op": "approve", "draft_id": draft})).await;
    assert_eq!(
        a.send(&g2, "@bot welcome back").await["wake"],
        json!([bot.id])
    );
    assert_eq!(
        g.ok(json!({"op": "state_get", "key": "k"})).await["value"],
        1
    );
    a.ok(json!({"op": "set_agent_policy", "conv_id": g1, "agent_id": bot.id, "trigger": "addressed"}))
        .await;
    g.ok(say(&g1, "back")).await;
    assert_eq!(audits(&hub, "agent.stopped").await, 1);
    assert_eq!(audits(&hub, "agent.resumed").await, 1);
}

/// The person who can read a conversation's log in the test.
fn a_or_bob(hub: &Hub, alice: &User, bob: &User, conv: &str, dm: &str) -> Conn {
    Conn::new(hub, if conv == dm { bob } else { alice })
}

async fn stop_refusals_leave_no_trace(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    let other = user(&hub, "other", UserKind::Agent, Some(&bob)).await;
    let a = Conn::new(&hub, &alice);
    let conv = a.group(&[&bot]).await;
    let head = seqs(&a.pull(&conv, 0).await).len();
    let long = "r".repeat(501);
    let cases: Vec<(Conn, Value, &str)> = vec![
        (Conn::new(&hub, &bob), stop(&bot), "forbidden"),
        (Conn::new(&hub, &bot), stop(&bot), "forbidden"),
        (Conn::new(&hub, &alice), stop(&other), "forbidden"),
        (Conn::new(&hub, &alice), stop(&bob), "not_found"),
        (
            Conn::new(&hub, &alice),
            json!({"op": "stop_agent", "agent_id": "u_nope"}),
            "not_found",
        ),
        (
            Conn::new(&hub, &alice),
            json!({"op": "stop_agent", "agent_id": bot.id, "reason": ""}),
            "bad_request",
        ),
        (
            Conn::new(&hub, &alice),
            json!({"op": "stop_agent", "agent_id": bot.id, "reason": long}),
            "bad_request",
        ),
        (Conn::new(&hub, &alice), resume(&bot), "conflict"),
        (Conn::new(&hub, &bob), destroy(&bot), "forbidden"),
        (
            Conn::new(&hub, &alice),
            json!({"op": "destroy_agent", "agent_id": bot.id, "confirm": "Bot"}),
            "bad_request",
        ),
        (
            Conn::new(&hub, &alice),
            json!({"op": "destroy_agent", "agent_id": bot.id, "confirm": ""}),
            "bad_request",
        ),
    ];
    for (who, req, want) in cases {
        assert_eq!(code(&who.call(req.clone()).await), want, "{req}");
    }
    assert_eq!(seqs(&a.pull(&conv, 0).await).len(), head);
    for action in ["agent.stopped", "agent.resumed", "agent.destroyed"] {
        assert_eq!(audits(&hub, action).await, 0, "{action}");
    }
    assert!(!hub.is_stopped(&bot.id) && !hub.is_destroyed(&bot.id));
    Conn::new(&hub, &bot).ok(say(&conv, "fine")).await;
    // Boundaries that are fine; stopping twice is a conflict.
    a.ok(json!({"op": "stop_agent", "agent_id": bot.id, "reason": "r".repeat(500)}))
        .await;
    assert_eq!(code(&a.call(stop(&bot)).await), "conflict");
    assert_eq!(states(&a, &conv).await.len(), 1);
}

async fn stop_survives_a_restart(db: TestDb) {
    let (alice, bot, conv);
    {
        let hub = open(single(&db.url)).await;
        alice = user(&hub, "alice", UserKind::Human, None).await;
        bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
        let a = Conn::new(&hub, &alice);
        conv = a.group(&[&bot]).await;
        a.ok(stop(&bot)).await;
    }
    let hub = open(single(&db.url)).await;
    assert!(hub.is_stopped(&bot.id));
    let g = Conn::new(&hub, &bot);
    assert_eq!(code(&g.call(say(&conv, "x")).await), "conflict");
    let a = Conn::new(&hub, &alice);
    a.ok(resume(&bot)).await;
    g.ok(say(&conv, "after restart")).await;
    drop(hub);
    let hub = open(single(&db.url)).await;
    assert!(!hub.is_stopped(&bot.id));
}

async fn destroying_removes_everything_but_the_record(db: TestDb) {
    let mut cfg = single(&db.url);
    cfg.agents.max_per_owner = 2;
    let hub = open(cfg).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let made = hub.create_agent(&alice, "bot").await.unwrap();
    let key = made["key"].as_str().unwrap().to_string();
    let bot = hub.user_by_name("bot").unwrap();
    let peer = user(&hub, "peer", UserKind::Agent, Some(&alice)).await;
    let a = Conn::new(&hub, &alice);
    let g1 = a.group(&[&bot, &peer]).await;
    let g2 = a.group(&[&bot]).await;
    let g = Conn::new(&hub, &bot);
    let said = g.ok(say(&g1, "my last words")).await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    let request = g
        .ok(json!({"op": "request_human", "conv_id": g1, "kind": "approve", "title": "may I?", "client_req_id": "q"}))
        .await["request_id"]
        .clone();
    let job = Conn::new(&hub, &peer)
        .ok(json!({"op": "send", "conv_id": g1, "client_msg_id": "r", "text": "job", "intent": "request"}))
        .await["msg_id"]
        .clone();
    g.ok(json!({"op": "claim", "msg_id": job})).await;
    g.ok(
        json!({"op": "put_summary", "conv_id": g1, "from_seq": 1, "to_seq": 2, "text": "summary"}),
    )
    .await;
    g.ok(json!({"op": "state_put", "key": "k", "value": "secret plan", "version": 0}))
        .await;
    a.ok(json!({"op": "set_agent_policy", "conv_id": g2, "agent_id": bot.id, "approval": true}))
        .await;
    g.ok(say(&g2, "a draft")).await;
    assert!(
        count(
            &db,
            &format!(
                "SELECT COUNT(*) FROM data_keys WHERE conv_id = '{}'",
                bot.id
            )
        )
        .await
            > 0
    );
    assert!(hub.authenticate_device(&key).is_some());
    let r = a.ok(destroy(&bot)).await;
    assert_eq!(r["conversations"], 2);
    // Keys, connections, memberships: gone.
    assert!(hub.authenticate_device(&key).is_none());
    for c in [&g1, &g2] {
        let log = a.pull(c, 0).await;
        let removed: Vec<&Value> = log["events"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["event"]["type"] == "member.removed")
            .collect();
        assert_eq!(removed.len(), 1, "{c}");
        assert_eq!(removed[0]["event"]["data"]["reason"], "destroyed");
        let members = a.ok(json!({"op": "list_convs"})).await["convs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|x| x["conv_id"] == json!(c))
            .unwrap()["members"]
            .clone();
        assert!(!members.to_string().contains(&bot.id), "{members}");
    }
    // Its messages stay: they are the conversation's record.
    let log = a.pull(&g1, 0).await;
    assert_eq!(msg_by_id(&log, &said).unwrap()["content"], "my last words");
    // Its request was cancelled, its claim released, its work deleted.
    let q = a
        .ok(json!({"op": "get_request", "request_id": request}))
        .await;
    assert_eq!(q["state"], "cancelled");
    Conn::new(&hub, &alice)
        .ok(json!({"op": "claim", "msg_id": job}))
        .await;
    for (t, col) in [
        ("summaries", "agent_id"),
        ("agent_state", "agent_id"),
        ("drafts", "agent_id"),
        ("data_keys", "conv_id"),
    ] {
        assert_eq!(
            count(
                &db,
                &format!("SELECT COUNT(*) FROM {t} WHERE {col} = '{}'", bot.id)
            )
            .await,
            0,
            "{t}"
        );
    }
    assert_eq!(audits(&hub, "agent.destroyed").await, 1);
    // Gone for good: further calls find nothing; the name stays taken; the
    // owner may have another agent in its place.
    for req in [destroy(&bot), stop(&bot), resume(&bot)] {
        assert_eq!(code(&a.call(req.clone()).await), "not_found", "{req}");
    }
    assert_eq!(
        code(&a.call(json!({"op": "agent_key", "agent_id": bot.id})).await),
        "not_found"
    );
    let agents = a.ok(json!({"op": "list_agents"})).await;
    assert!(
        agents.to_string().contains("\"destroyed\":true"),
        "{agents}"
    );
    assert!(hub.create_agent(&alice, "bot").await.is_err());
    hub.create_agent(&alice, "bot2").await.unwrap();
    // A restart keeps it destroyed.
    drop(g);
    drop(a);
    drop(hub);
    let hub = open(single(&db.url)).await;
    assert!(hub.is_destroyed(&bot.id));
    assert!(hub.authenticate_device(&key).is_none());
}

async fn an_interrupted_destroy_is_finished_at_start(db: TestDb) {
    let (alice, bot, convs);
    {
        let hub = open(single(&db.url)).await;
        alice = user(&hub, "alice", UserKind::Human, None).await;
        bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
        let a = Conn::new(&hub, &alice);
        convs = vec![a.group(&[&bot]).await, a.group(&[&bot]).await];
    }
    // As if the server crashed right after recording the destroy.
    {
        use sqlx::Connection;
        let mut c = sqlx::AnyConnection::connect(&db.url).await.unwrap();
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "UPDATE users SET destroyed_at = 1, deactivated_at = 1 WHERE id = '{}'",
            bot.id
        )))
        .execute(&mut c)
        .await
        .unwrap();
    }
    let hub = open(single(&db.url)).await;
    let a = Conn::new(&hub, &alice);
    for c in &convs {
        let log = a.pull(c, 0).await;
        assert!(
            log["events"]
                .as_array()
                .unwrap()
                .iter()
                .any(|e| e["event"]["type"] == "member.removed"
                    && e["event"]["data"]["reason"] == "destroyed"),
            "{log}"
        );
    }
    // Repeatable: another start changes nothing.
    let n = seqs(&a.pull(&convs[0], 0).await).len();
    drop(a);
    drop(hub);
    let hub = open(single(&db.url)).await;
    assert_eq!(
        seqs(&Conn::new(&hub, &alice).pull(&convs[0], 0).await).len(),
        n
    );
}

/// Agents keep sending while their owner stops them: in each conversation
/// nothing of theirs comes after the stop event.
async fn race_nothing_after_the_stop(db: TestDb) {
    let mut cfg = single(&db.url);
    cfg.mcp.burst = 1000.0;
    let hub = open(cfg).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bots: Vec<User> = {
        let mut v = vec![];
        for i in 0..3 {
            v.push(user(&hub, &format!("b{i}"), UserKind::Agent, Some(&alice)).await);
        }
        v
    };
    let a = Conn::new(&hub, &alice);
    let refs: Vec<&User> = bots.iter().collect();
    let convs = vec![a.group(&refs).await, a.group(&refs).await];
    for c in &convs {
        for b in &bots {
            a.ok(
                json!({"op": "set_agent_policy", "conv_id": c, "agent_id": b.id, "trigger": "all"}),
            )
            .await;
        }
    }
    let target = bots[0].clone();
    for round in 0..8 {
        let mut tasks = vec![];
        for b in &bots {
            for c in &convs {
                let (h, b, c) = (hub.clone(), b.clone(), c.clone());
                tasks.push(tokio::spawn(async move {
                    let g = Conn::new(&h, &b);
                    for k in 0..4 {
                        let r = g.call(say(&c, &format!("{round}-{k}"))).await;
                        if r["ok"] != true {
                            assert!(["conflict", "rate_limited"].contains(&code(&r)), "{r}");
                        }
                    }
                }));
            }
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
        a.ok(stop(&target)).await;
        for t in tasks {
            t.await.unwrap();
        }
        for c in &convs {
            let log = a.pull(c, 0).await;
            let evs = log["events"].as_array().unwrap();
            let stop_seq = evs
                .iter()
                .filter(|e| {
                    e["event"]["type"] == "agent.state" && e["event"]["data"]["state"] == "stopped"
                })
                .map(|e| e["event"]["seq"].as_i64().unwrap())
                .max()
                .unwrap();
            let late: Vec<&Value> = evs
                .iter()
                .filter(|e| {
                    e["event"]["seq"].as_i64().unwrap() > stop_seq
                        && e["event"]["msg"]["sender_id"] == json!(target.id)
                })
                .collect();
            assert!(late.is_empty(), "round {round}: {late:?}");
        }
        a.ok(resume(&target)).await;
        tokio::time::sleep(Duration::from_millis(1100)).await;
    }
}

/// Owners racing: of concurrent stops one wins, of concurrent destroys one
/// wins, and a stop racing a destroy leaves it destroyed.
async fn race_one_stop_one_destroy(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    for round in 0..8 {
        let bot = user(&hub, &format!("bot{round}"), UserKind::Agent, Some(&alice)).await;
        let a = Conn::new(&hub, &alice);
        a.group(&[&bot]).await;
        let run = |req: Value| {
            let (h, al) = (hub.clone(), alice.clone());
            tokio::spawn(async move { Conn::new(&h, &al).call(req).await })
        };
        let stops: Vec<_> = (0..4).map(|_| run(stop(&bot))).collect();
        let mut ok = 0;
        for t in stops {
            let r = t.await.unwrap();
            if r["ok"] == true {
                ok += 1;
            } else {
                assert_eq!(code(&r), "conflict", "{r}");
            }
        }
        assert_eq!(ok, 1, "round {round}: stops");
        let mut tasks: Vec<_> = (0..3).map(|_| run(destroy(&bot))).collect();
        tasks.push(run(resume(&bot)));
        let mut destroyed = 0;
        for t in tasks {
            let r = t.await.unwrap();
            if r["ok"] == true {
                destroyed += (r["destroyed"] == true) as i32;
            } else {
                assert!(["not_found", "conflict"].contains(&code(&r)), "{r}");
            }
        }
        assert_eq!(destroyed, 1, "round {round}: destroys");
        assert!(hub.is_destroyed(&bot.id) && !hub.is_stopped(&bot.id));
    }
}
