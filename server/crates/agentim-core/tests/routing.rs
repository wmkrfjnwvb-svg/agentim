//! Agent-to-agent routing (docs/09 §10.1): receive modes decide what an
//! agent receives and what wakes it, the envelope, the fan-out cap, streams
//! waking only when complete, drafts keeping their envelope, the root
//! budget under concurrency, restart and several databases.

mod common;

use std::collections::HashMap;

use agentim_core::hub::Hub;
use agentim_core::model::*;
use common::*;
use serde_json::{Value, json};

backend_tests!(
    receive_modes_decide_what_reaches_and_wakes_an_agent,
    sync_and_pushes_agree_on_wake,
    envelopes_are_checked_and_kept,
    mode_changes_are_checked,
    one_event_wakes_at_most_five_agents,
    streams_and_drafts_wake_when_they_are_final,
    race_the_root_budget_admits_exactly_its_size,
);

fn code(v: &Value) -> &str {
    v["error"]["code"].as_str().unwrap_or("")
}

async fn hub_with(db: &TestDb, root_messages: i64) -> Hub {
    let mut h = hub_cfg();
    h.guards.root_messages = root_messages;
    Hub::open(single(&db.url), h).await.expect("open hub")
}

fn policy(conv: &str, agent: &User, trigger: &str) -> Value {
    json!({"op": "set_agent_policy", "conv_id": conv, "agent_id": agent.id, "trigger": trigger})
}

/// The message events a connection got pushed: (content, wake).
fn got(c: &mut Conn) -> Vec<(String, bool)> {
    c.drain()
        .into_iter()
        .filter(|f| f["type"] == "event" && f["event"]["type"] == "msg.created")
        .map(|f| {
            (
                f["event"]["msg"]["content"].as_str().unwrap().to_string(),
                f["wake"] == true,
            )
        })
        .collect()
}

fn send_env(conv: &str, text: &str, env: Value) -> Value {
    let mut v = json!({"op": "send", "conv_id": conv, "client_msg_id": uuid(), "text": text});
    for (k, x) in env.as_object().unwrap() {
        v[k] = x.clone();
    }
    v
}

fn uuid() -> String {
    format!("{:x}", rand_u64())
}

fn rand_u64() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(1);
    N.fetch_add(1, Ordering::Relaxed)
        ^ (std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos() as u64)
}

async fn receive_modes_decide_what_reaches_and_wakes_an_agent(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let addr = user(&hub, "addr", UserKind::Agent, Some(&alice)).await;
    let ment = user(&hub, "ment", UserKind::Agent, Some(&alice)).await;
    let topi = user(&hub, "topi", UserKind::Agent, Some(&alice)).await;
    let obs = user(&hub, "obs", UserKind::Agent, Some(&alice)).await;
    let a = Conn::new(&hub, &alice);
    let conv = a.group(&[&addr, &ment, &topi, &obs]).await;
    a.ok(policy(&conv, &ment, "mention")).await;
    let mut p = policy(&conv, &topi, "topics");
    p["topics"] = json!(["billing", "eu.refunds"]);
    a.ok(p).await;
    a.ok(policy(&conv, &obs, "observe")).await;
    let mut c: HashMap<&str, Conn> = HashMap::new();
    for u in [&addr, &ment, &topi, &obs] {
        c.insert(u.name.as_str(), Conn::new(&hub, u));
    }
    for x in c.values_mut() {
        x.drain();
    }
    a.send(&conv, "hello everyone").await;
    a.send(&conv, "@addr please look").await;
    a.ok(send_env(
        &conv,
        "for addr and ment by name",
        json!({"to": ["addr", ment.id]}),
    ))
    .await;
    a.ok(send_env(
        &conv,
        "billing question",
        json!({"topics": ["billing"]}),
    ))
    .await;
    a.ok(send_env(&conv, "other topic", json!({"topics": ["sales"]})))
        .await;
    let from_ment = c["ment"].send(&conv, "ment speaks").await["msg_id"].clone();
    a.reply(&conv, "answer to ment", from_ment.as_str().unwrap())
        .await;
    let t = |s: &str, w: bool| (s.to_string(), w);
    assert_eq!(
        got(c.get_mut("addr").unwrap()),
        vec![
            t("@addr please look", true),
            t("for addr and ment by name", true)
        ]
    );
    assert_eq!(
        got(c.get_mut("ment").unwrap()),
        vec![t("ment speaks", false), t("answer to ment", true)],
        "mention mode ignores `to`; a reply counts"
    );
    assert_eq!(
        got(c.get_mut("topi").unwrap()),
        vec![t("billing question", true)]
    );
    let o = got(c.get_mut("obs").unwrap());
    assert_eq!(o.len(), 7, "observe receives everything: {o:?}");
    assert!(o.iter().all(|(_, w)| !w), "and is never woken");
    // People are unaffected: alice gets everything, without wake flags.
    let mine: Vec<Value> = Conn::new(&hub, &alice).pull(&conv, 0).await["events"]
        .as_array()
        .unwrap()
        .clone();
    assert!(mine.len() > 7);
    // A direct conversation with an agent: all by default.
    let dm = a
        .ok(json!({"op": "create_conv", "kind": "dm", "title": "dm", "members": [addr.id]}))
        .await["conv_id"]
        .as_str()
        .unwrap()
        .to_string();
    c.get_mut("addr").unwrap().drain();
    a.send(&dm, "no mention needed").await;
    assert_eq!(
        got(c.get_mut("addr").unwrap()),
        vec![t("no mention needed", true)]
    );
}

/// Sync delivers the same events with the same wake flags as pushes.
async fn sync_and_pushes_agree_on_wake(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    let a = Conn::new(&hub, &alice);
    let conv = a.group(&[&bot]).await;
    let (_, dev) = hub
        .issue_token(&bot.id, None, None, None, "t")
        .await
        .map(|(k, r)| (k, r.device_id))
        .unwrap();
    a.send(&conv, "not for bot").await;
    a.send(&conv, "@bot for bot").await;
    a.ok(send_env(
        &conv,
        "to bot",
        json!({"to": ["bot"], "intent": "request"}),
    ))
    .await;
    let d = Conn::device(&hub, &bot, &dev);
    let r = d.ok(json!({"op": "sync"})).await;
    let evs: Vec<(String, bool)> = r["convs"][0]["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["event"]["type"] == "msg.created")
        .map(|e| {
            (
                e["event"]["msg"]["content"].as_str().unwrap().to_string(),
                e["wake"] == true,
            )
        })
        .collect();
    assert_eq!(
        evs,
        vec![
            ("@bot for bot".to_string(), true),
            ("to bot".to_string(), true)
        ]
    );
    let to_bot = r["convs"][0]["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["event"]["msg"]["content"] == "to bot")
        .unwrap();
    assert_eq!(to_bot["event"]["msg"]["envelope"]["intent"], "request");
    assert_eq!(to_bot["event"]["msg"]["envelope"]["to"], json!([bot.id]));
}

async fn envelopes_are_checked_and_kept(db: TestDb) {
    let ok_env = json!({"to": ["bob"], "topics": ["a.b-c_1"], "intent": "proposal", "refs": ["doc://plan/1", "x".repeat(256)]});
    let (alice, conv, msg_id);
    {
        let hub = open(single(&db.url)).await;
        alice = user(&hub, "alice", UserKind::Human, None).await;
        let bob = user(&hub, "bob", UserKind::Human, None).await;
        let carol = user(&hub, "carol", UserKind::Human, None).await;
        let a = Conn::new(&hub, &alice);
        conv = a.group(&[&bob]).await;
        let head = seqs(&a.pull(&conv, 0).await).len();
        let many: Vec<String> = (0..21).map(|i| format!("t{i}")).collect();
        let cases = vec![
            json!({"to": ["carol"]}),
            json!({"to": ["nobody"]}),
            json!({"to": []}),
            json!({"to": (0..21).map(|_| "bob").collect::<Vec<_>>()}),
            json!({"topics": []}),
            json!({"topics": ["Upper"]}),
            json!({"topics": ["a b"]}),
            json!({"topics": ["x".repeat(65)]}),
            json!({"topics": many[..11].to_vec()}),
            json!({"intent": "shout"}),
            json!({"refs": []}),
            json!({"refs": ["x".repeat(257)]}),
            json!({"refs": many.clone()}),
            json!({"refs": [""]}),
            json!({"to": ["bob"], "to_user": "bob"}),
        ];
        for env in cases {
            let r = a.call(send_env(&conv, "x", env.clone())).await;
            assert_eq!(code(&r), "bad_request", "{env}: {r}");
        }
        assert_eq!(seqs(&a.pull(&conv, 0).await).len(), head, "no trace");
        let _ = carol;
        // Boundaries: 10 topics, 20 refs, every intent.
        let mut big = ok_env.clone();
        big["topics"] = json!(many[..10].to_vec());
        big["refs"] = json!(many[..20].to_vec());
        a.ok(send_env(&conv, "big", big)).await;
        for i in ["request", "proposal", "answer", "status", "done"] {
            a.ok(send_env(&conv, i, json!({"intent": i}))).await;
        }
        msg_id = a.ok(send_env(&conv, "kept", ok_env.clone())).await["msg_id"].clone();
    }
    // Stored (resolved to ids) and returned after a restart.
    let hub = open(single(&db.url)).await;
    let log = Conn::new(&hub, &alice).pull(&conv, 0).await;
    let m = msg_by_id(&log, msg_id.as_str().unwrap()).unwrap();
    let bob = hub.user_by_name("bob").unwrap();
    assert_eq!(m["envelope"]["to"], json!([bob.id]));
    assert_eq!(m["envelope"]["topics"], ok_env["topics"]);
    assert_eq!(m["envelope"]["intent"], "proposal");
    assert_eq!(m["envelope"]["refs"], ok_env["refs"]);
    assert_eq!(m["root_id"], msg_id, "a person's message starts a chain");
}

async fn mode_changes_are_checked(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let agents: Vec<User> = {
        let mut v = vec![];
        for i in 0..4 {
            v.push(user(&hub, &format!("g{i}"), UserKind::Agent, Some(&alice)).await);
        }
        v
    };
    let a = Conn::new(&hub, &alice);
    let small = a.group(&[&agents[0], &agents[1], &agents[2]]).await;
    let big = a
        .group(&[&agents[0], &agents[1], &agents[2], &agents[3]])
        .await;
    // `all` only with at most three agents (or in a dm).
    a.ok(policy(&small, &agents[0], "all")).await;
    assert_eq!(
        code(&a.call(policy(&big, &agents[0], "all")).await),
        "conflict"
    );
    // `topics` needs topics; topic names are checked.
    assert_eq!(
        code(&a.call(policy(&big, &agents[1], "topics")).await),
        "bad_request"
    );
    for bad in [
        json!([]),
        json!(["Bad"]),
        json!((0..21).map(|i| format!("t{i}")).collect::<Vec<_>>()),
    ] {
        let mut p = policy(&big, &agents[1], "topics");
        p["topics"] = bad.clone();
        assert_eq!(code(&a.call(p).await), "bad_request", "{bad}");
    }
    assert_eq!(
        code(&a.call(policy(&big, &agents[1], "loud")).await),
        "bad_request"
    );
    // Only the owner sets it.
    assert_eq!(
        code(
            &Conn::new(&hub, &bob)
                .call(policy(&big, &agents[1], "observe"))
                .await
        ),
        "forbidden"
    );
    // Refusals leave the mode as it was; a valid change is logged.
    let mut p = policy(&big, &agents[1], "topics");
    p["topics"] = json!(["ops"]);
    let before = seqs(&a.pull(&big, 0).await).len();
    a.ok(p).await;
    let log = a.pull(&big, 0).await;
    assert_eq!(seqs(&log).len(), before + 1);
    let last = log["events"].as_array().unwrap().last().unwrap();
    assert_eq!(last["event"]["type"], "member.policy");
    assert_eq!(last["event"]["data"]["trigger"], "topics");
    assert_eq!(last["event"]["data"]["topics"], json!(["ops"]));
    // Topics kept: switching back to topics later needs none.
    a.ok(policy(&big, &agents[1], "addressed")).await;
    a.ok(policy(&big, &agents[1], "topics")).await;
}

async fn one_event_wakes_at_most_five_agents(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let mut agents = vec![];
    for i in 0..7 {
        agents.push(user(&hub, &format!("w{i}"), UserKind::Agent, Some(&alice)).await);
    }
    let refs: Vec<&User> = agents.iter().collect();
    let a = Conn::new(&hub, &alice);
    let conv = a.group(&refs).await;
    let mut conns: Vec<Conn> = agents.iter().map(|g| Conn::new(&hub, g)).collect();
    // Named in `to` first (reverse order), then mentioned.
    let to: Vec<String> = agents[4..].iter().rev().map(|g| g.name.clone()).collect();
    let r = a
        .ok(send_env(
            &conv,
            "@w0 @w1 @w2 @w3 everyone",
            json!({"to": to}),
        ))
        .await;
    let woken: Vec<String> = serde_json::from_value(r["wake"].clone()).unwrap();
    let id = |n: &str| agents.iter().find(|g| g.name == n).unwrap().id.clone();
    assert_eq!(
        woken,
        vec![id("w6"), id("w5"), id("w4"), id("w0"), id("w1")]
    );
    let log = a.pull(&conv, 0).await;
    let last = log["events"].as_array().unwrap().last().unwrap();
    assert_eq!(last["event"]["data"]["wake_capped"], 2);
    for (g, c) in agents.iter().zip(conns.iter_mut()) {
        let mine = got(c);
        assert_eq!(mine.len(), 1, "{} still receives it", g.name);
        assert_eq!(mine[0].1, woken.contains(&g.id), "{}", g.name);
    }
    // Exactly five: no cap recorded.
    let r = a
        .ok(send_env(&conv, "@w0 @w1 @w2 @w3 @w4", json!({})))
        .await;
    assert_eq!(r["wake"].as_array().unwrap().len(), 5);
    let log = a.pull(&conv, 0).await;
    assert!(
        log["events"].as_array().unwrap().last().unwrap()["event"]["data"]
            .get("wake_capped")
            .is_none()
    );
}

async fn streams_and_drafts_wake_when_they_are_final(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let writer = user(&hub, "writer", UserKind::Agent, Some(&alice)).await;
    let reader = user(&hub, "reader", UserKind::Agent, Some(&alice)).await;
    let a = Conn::new(&hub, &alice);
    let conv = a.group(&[&writer, &reader]).await;
    let w = Conn::new(&hub, &writer);
    let mut r = Conn::new(&hub, &reader);
    // The reader asks; the writer streams the answer to it.
    let q = r.send(&conv, "@writer what is 2+2").await["msg_id"].clone();
    let s = w
        .ok(json!({"op": "stream_start", "conv_id": conv, "client_msg_id": "s1", "reply_to": q}))
        .await;
    let sid = s["msg_id"].clone();
    w.ok(json!({"op": "stream_append", "msg_id": sid, "idx": 0, "text": "4"}))
        .await;
    w.ok(json!({"op": "stream_end", "msg_id": sid})).await;
    let frames: Vec<Value> = r
        .drain()
        .into_iter()
        .filter(|f| f["type"] == "event" && f["event"]["msg"]["msg_id"] == sid)
        .collect();
    let states: Vec<(String, bool)> = frames
        .iter()
        .map(|f| {
            (
                f["event"]["msg"]["state"].as_str().unwrap().to_string(),
                f["wake"] == true,
            )
        })
        .collect();
    assert_eq!(
        states,
        vec![
            ("streaming".to_string(), false),
            ("complete".to_string(), true)
        ],
        "woken by the finished answer, not the start"
    );
    // A draft keeps its envelope and wakes when approved.
    a.ok(
        json!({"op": "set_agent_policy", "conv_id": conv, "agent_id": writer.id, "approval": true}),
    )
    .await;
    let d = w
        .ok(send_env(
            &conv,
            "handing over",
            json!({"to": ["reader"], "intent": "done"}),
        ))
        .await;
    assert!(got(&mut r).is_empty(), "nothing before approval");
    let ok = a
        .ok(json!({"op": "approve", "draft_id": d["draft_id"]}))
        .await;
    assert!(ok["seq"].is_number());
    let after = got(&mut r);
    assert_eq!(after, vec![("handing over".to_string(), true)]);
    let log = a.pull(&conv, 0).await;
    let m = log["events"].as_array().unwrap().last().unwrap();
    assert_eq!(m["event"]["msg"]["envelope"]["intent"], "done");
}

/// Agents of one chain racing past its budget: exactly `budget` messages
/// get in, the stop is told once, and the counter matches the log.
async fn race_the_root_budget_admits_exactly_its_size(db: TestDb) {
    let budget = 10;
    let hub = hub_with(&db, budget).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let mut agents = vec![];
    for i in 0..4 {
        agents.push(user(&hub, &format!("r{i}"), UserKind::Agent, Some(&alice)).await);
    }
    let refs: Vec<&User> = agents.iter().collect();
    let a = Conn::new(&hub, &alice);
    let conv = a.group(&refs).await;
    for round in 0..3 {
        let root = a.send(&conv, &format!("question {round}")).await["msg_id"]
            .as_str()
            .unwrap()
            .to_string();
        let mut tasks = vec![];
        for g in &agents {
            for i in 0..5 {
                let (hub, g, conv, root) = (hub.clone(), g.clone(), conv.clone(), root.clone());
                tasks.push(tokio::spawn(async move {
                    Conn::new(&hub, &g)
                        .reply(&conv, &format!("{} {round} {i}", g.name), &root)
                        .await
                }));
            }
        }
        let mut ok = 0;
        for t in tasks {
            let r = t.await.unwrap();
            if r["ok"] == true {
                ok += 1;
                assert_eq!(r["root_id"], json!(root));
            } else {
                assert!(
                    ["rate_limited", "circuit_open"].contains(&code(&r)),
                    "round {round}: {r}"
                );
            }
        }
        assert_eq!(ok, budget, "round {round}");
        let log = a.pull(&conv, 0).await;
        let in_chain = log["events"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| {
                e["event"]["type"] == "msg.created"
                    && e["event"]["msg"]["sender_type"] == "agent"
                    && e["event"]["msg"]["root_id"] == json!(root)
            })
            .count();
        assert_eq!(in_chain as i64, budget, "round {round}");
        let stops = log["events"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| {
                e["event"]["data"]["reason"] == "root_budget"
                    && e["event"]["data"]["root_id"] == json!(root)
            })
            .count();
        assert_eq!(stops, 1, "round {round}");
        // Let the per-agent rate limit refill for the next round.
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    }
}

/// A unit's conversation keeps its chain budgets when it moves database.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn root_budgets_move_with_their_conversation() {
    let main = new_db(Backend::Sqlite).await.unwrap();
    let secret = new_db(Backend::Sqlite).await.unwrap();
    let open_two = |unit_b: &'static str| {
        let (m, s) = (main.url.clone(), secret.url.clone());
        async move {
            let mut h = hub_cfg();
            h.guards.root_messages = 2;
            h.edition = agentim_core::edition::Edition::Full;
            Hub::open(two(&m, &s, unit_b), h).await.unwrap()
        }
    };
    let (carol, bot, conv, root);
    {
        let hub = open_two("main").await;
        carol = user_in(&hub, "carol", "b").await;
        bot = user(&hub, "bot", UserKind::Agent, Some(&carol)).await;
        let c = Conn::new(&hub, &carol);
        conv = c.group(&[&bot]).await;
        root = c.send(&conv, "@bot go").await["msg_id"]
            .as_str()
            .unwrap()
            .to_string();
        let g = Conn::new(&hub, &bot);
        g.ok(json!({"op": "send", "conv_id": conv, "client_msg_id": "1", "text": "one", "reply_to": root}))
            .await;
        g.ok(json!({"op": "send", "conv_id": conv, "client_msg_id": "2", "text": "two", "reply_to": root}))
            .await;
    }
    let hub = open_two("secret").await;
    assert_eq!(hub.migrate_unit("b").await.unwrap()["moved"], 1);
    let r = Conn::new(&hub, &bot)
        .call(json!({"op": "send", "conv_id": conv, "client_msg_id": "3", "text": "three", "reply_to": root}))
        .await;
    assert_eq!(code(&r), "rate_limited", "{r}");
    let _ = carol;
}
