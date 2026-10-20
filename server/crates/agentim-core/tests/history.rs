//! The agent history service (docs/03-agents.md §7.1): reading by seq, context in a
//! token budget, search and summaries, all limited to what the agent may
//! see; refusals that leave no trace; audit; rate limits; restart,
//! dissolution, migration and encryption; reads racing writes.

mod common;

use agentim_core::hub::Hub;
use agentim_core::model::*;
use common::*;
use serde_json::{Value, json};

backend_tests!(
    history_reads_what_the_agent_may_see,
    history_refusals_leave_no_trace,
    context_starts_from_the_latest_summary,
    summaries_follow_the_rules,
    search_finds_only_what_the_agent_may_see,
    search_filters_narrow_the_hits,
    reads_are_rate_limited_per_agent,
    summaries_survive_a_restart_and_go_with_dissolution,
    race_paging_while_messages_arrive,
    race_summaries_and_context,
);

fn code(v: &Value) -> &str {
    v["error"]["code"].as_str().unwrap_or("")
}

fn hist(conv: &str) -> Value {
    json!({"op": "history", "conv_id": conv})
}

/// Texts of the message events of a read.
fn texts(v: &Value) -> Vec<String> {
    v["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["event"]["type"] == "msg.created")
        .map(|e| e["event"]["msg"]["content"].as_str().unwrap().to_string())
        .collect()
}

fn ev_seqs(v: &Value) -> Vec<i64> {
    v["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["event"]["seq"].as_i64().unwrap())
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

async fn history_reads_what_the_agent_may_see(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    let late = user(&hub, "late", UserKind::Agent, Some(&alice)).await;
    let watcher = user(&hub, "watcher", UserKind::Agent, Some(&alice)).await;
    let a = Conn::new(&hub, &alice);
    let conv = a.group(&[&bob, &bot, &watcher]).await;
    a.ok(json!({"op": "set_agent_policy", "conv_id": conv, "agent_id": watcher.id, "trigger": "observe"}))
        .await;
    for i in 0..6 {
        a.send(&conv, &format!("@bot task {i}")).await;
        a.send(&conv, &format!("chatter {i}")).await;
    }
    let g = Conn::new(&hub, &bot);
    // Latest first by default (returned oldest first); addressed: only
    // what names it.
    let r = g.ok(hist(&conv)).await;
    assert_eq!(
        texts(&r),
        (0..6).map(|i| format!("@bot task {i}")).collect::<Vec<_>>()
    );
    assert_eq!(r["has_more"], false);
    // Trust is marked on others' messages.
    let m = r["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["event"]["type"] == "msg.created")
        .unwrap();
    assert!(m["trust"].is_string(), "{m}");
    // Paging down and up by the scanned range.
    let p1 = g
        .ok(json!({"op": "history", "conv_id": conv, "limit": 2}))
        .await;
    assert_eq!(texts(&p1), vec!["@bot task 4", "@bot task 5"]);
    assert_eq!(p1["has_more"], true);
    let p2 = g
        .ok(json!({"op": "history", "conv_id": conv, "limit": 2, "before_seq": p1["first_seq"]}))
        .await;
    assert_eq!(texts(&p2), vec!["@bot task 2", "@bot task 3"]);
    let up = g
        .ok(json!({"op": "history", "conv_id": conv, "limit": 8, "after_seq": 0}))
        .await;
    assert_eq!(texts(&up)[0], "@bot task 0");
    let rest = g
        .ok(json!({"op": "history", "conv_id": conv, "limit": 200, "after_seq": up["last_seq"]}))
        .await;
    let mut all: Vec<i64> = ev_seqs(&up);
    all.extend(ev_seqs(&rest));
    assert_eq!(all, ev_seqs(&r), "paging up gives the same events once");
    assert_eq!(rest["has_more"], false);
    // Limits: 1 and 200 are fine.
    let one = g
        .ok(json!({"op": "history", "conv_id": conv, "limit": 1}))
        .await;
    assert_eq!(texts(&one), vec!["@bot task 5"]);
    g.ok(json!({"op": "history", "conv_id": conv, "limit": 200}))
        .await;
    // Observe sees everything.
    let w = Conn::new(&hub, &watcher)
        .ok(json!({"op": "history", "conv_id": conv, "limit": 200}))
        .await;
    assert_eq!(texts(&w).len(), 12);
    // A member added later sees from where it was let in.
    a.ok(json!({"op": "add_member", "conv_id": conv, "user_id": late.id}))
        .await;
    a.send(&conv, "@late welcome").await;
    let l = Conn::new(&hub, &late).ok(hist(&conv)).await;
    assert_eq!(texts(&l), vec!["@late welcome"]);
    // Recalled content is gone from history too.
    let last = a.send(&conv, "@bot secret").await["msg_id"].clone();
    a.ok(json!({"op": "recall", "msg_id": last})).await;
    let r = g.ok(hist(&conv)).await;
    assert!(!texts(&r).iter().any(|t| t.contains("secret")), "{r}");
    // Every read is audited once (not once per event).
    assert_eq!(audits(&hub, "agent.history").await, 10);
    // Removed: no longer readable.
    a.ok(json!({"op": "remove_member", "conv_id": conv, "user_id": bot.id}))
        .await;
    assert_eq!(code(&g.call(hist(&conv)).await), "forbidden");
}

async fn history_refusals_leave_no_trace(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    let other = user(&hub, "other", UserKind::Agent, Some(&alice)).await;
    let a = Conn::new(&hub, &alice);
    let conv = a.group(&[&bot]).await;
    a.send(&conv, "@bot hi").await;
    let g = Conn::new(&hub, &bot);
    let o = Conn::new(&hub, &other);
    let cases: Vec<(&Conn, Value, &str)> = vec![
        (&a, hist(&conv), "forbidden"),
        (&a, json!({"op": "context", "conv_id": conv}), "forbidden"),
        (&a, json!({"op": "search", "query": "hi"}), "forbidden"),
        (
            &a,
            json!({"op": "put_summary", "conv_id": conv, "from_seq": 1, "to_seq": 1, "text": "x"}),
            "forbidden",
        ),
        (
            &g,
            json!({"op": "history", "conv_id": "c_nope"}),
            "not_found",
        ),
        (&o, hist(&conv), "forbidden"),
        (&o, json!({"op": "context", "conv_id": conv}), "forbidden"),
        (
            &o,
            json!({"op": "search", "query": "hi", "conv_id": conv}),
            "forbidden",
        ),
        (
            &g,
            json!({"op": "history", "conv_id": conv, "limit": 0}),
            "bad_request",
        ),
        (
            &g,
            json!({"op": "history", "conv_id": conv, "limit": 201}),
            "bad_request",
        ),
        (
            &g,
            json!({"op": "history", "conv_id": conv, "after_seq": -1}),
            "bad_request",
        ),
        (
            &g,
            json!({"op": "history", "conv_id": conv, "before_seq": 0}),
            "bad_request",
        ),
        (
            &g,
            json!({"op": "history", "conv_id": conv, "after_seq": 1, "before_seq": 3}),
            "bad_request",
        ),
        (
            &g,
            json!({"op": "context", "conv_id": conv, "max_tokens": 255}),
            "bad_request",
        ),
        (
            &g,
            json!({"op": "context", "conv_id": conv, "max_tokens": 200001}),
            "bad_request",
        ),
        (&g, json!({"op": "search", "query": "h"}), "bad_request"),
        (&g, json!({"op": "search", "query": "  h  "}), "bad_request"),
        (
            &g,
            json!({"op": "search", "query": "x".repeat(201)}),
            "bad_request",
        ),
        (
            &g,
            json!({"op": "search", "query": "hi", "limit": 51}),
            "bad_request",
        ),
        (
            &g,
            json!({"op": "search", "query": "hi", "limit": 0}),
            "bad_request",
        ),
        (
            &g,
            json!({"op": "search", "query": "hi", "conv_id": "c_nope"}),
            "not_found",
        ),
    ];
    let head = seqs(&a.pull(&conv, 0).await).len();
    for (who, req, want) in cases {
        assert_eq!(code(&who.call(req.clone()).await), want, "{req}");
    }
    assert_eq!(seqs(&a.pull(&conv, 0).await).len(), head);
    for action in ["agent.history", "agent.context", "agent.search"] {
        assert_eq!(audits(&hub, action).await, 0, "{action}");
    }
    // Boundaries that are fine.
    g.ok(json!({"op": "context", "conv_id": conv, "max_tokens": 256}))
        .await;
    g.ok(json!({"op": "context", "conv_id": conv, "max_tokens": 200000}))
        .await;
    g.ok(json!({"op": "search", "query": "hi", "limit": 50}))
        .await;
    g.ok(json!({"op": "search", "query": "x".repeat(200)}))
        .await;
    // A dissolved conversation is gone.
    a.ok(json!({"op": "dissolve", "conv_id": conv})).await;
    assert_eq!(code(&g.call(hist(&conv)).await), "not_found");
}

async fn context_starts_from_the_latest_summary(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    let peer = user(&hub, "peer", UserKind::Agent, Some(&alice)).await;
    let a = Conn::new(&hub, &alice);
    let conv = a.group(&[&bot, &peer]).await;
    for g in [&bot, &peer] {
        a.ok(json!({"op": "set_agent_policy", "conv_id": conv, "agent_id": g.id, "trigger": "observe"}))
            .await;
    }
    for i in 0..10 {
        a.send(&conv, &format!("line {i} {}", "x".repeat(400)))
            .await;
    }
    let g = Conn::new(&hub, &bot);
    // No summary: the newest events that fit.
    let c = g.ok(json!({"op": "context", "conv_id": conv})).await;
    assert!(c["summary"].is_null());
    assert_eq!(c["truncated"], false);
    assert_eq!(texts(&c).len(), 10);
    let small = g
        .ok(json!({"op": "context", "conv_id": conv, "max_tokens": 256}))
        .await;
    assert_eq!(small["truncated"], true);
    let t = texts(&small);
    assert!(!t.is_empty() && t.len() < 10, "{t:?}");
    assert!(
        t.last().unwrap().starts_with("line 9"),
        "newest kept: {t:?}"
    );
    // With a summary: it plus what came after it.
    let last = *ev_seqs(&c).last().unwrap();
    g.ok(json!({"op": "put_summary", "conv_id": conv, "from_seq": 1, "to_seq": last - 3, "text": "first part"}))
        .await;
    let c = g.ok(json!({"op": "context", "conv_id": conv})).await;
    assert_eq!(c["summary"]["text"], "first part");
    assert_eq!(c["summary"]["derived"], true);
    assert!(ev_seqs(&c).iter().all(|s| *s > last - 3), "{c}");
    assert_eq!(texts(&c).len(), 3);
    // The summary reaching furthest wins, even when written earlier.
    g.ok(
        json!({"op": "put_summary", "conv_id": conv, "from_seq": 1, "to_seq": 2, "text": "older"}),
    )
    .await;
    let c = g.ok(json!({"op": "context", "conv_id": conv})).await;
    assert_eq!(c["summary"]["text"], "first part");
    // Another agent's summaries are not mine.
    let p = Conn::new(&hub, &peer)
        .ok(json!({"op": "context", "conv_id": conv}))
        .await;
    assert!(p["summary"].is_null());
    assert_eq!(audits(&hub, "agent.context").await, 5);
}

async fn summaries_follow_the_rules(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    let late = user(&hub, "late", UserKind::Agent, Some(&alice)).await;
    let a = Conn::new(&hub, &alice);
    let conv = a.group(&[&bot]).await;
    for i in 0..4 {
        a.send(&conv, &format!("@bot {i}")).await;
    }
    let last = *seqs(&a.pull(&conv, 0).await).last().unwrap();
    a.ok(json!({"op": "add_member", "conv_id": conv, "user_id": late.id}))
        .await;
    let joined = *seqs(&a.pull(&conv, 0).await).last().unwrap();
    let g = Conn::new(&hub, &bot);
    let l = Conn::new(&hub, &late);
    let put = |f: i64, t: i64, text: &str| json!({"op": "put_summary", "conv_id": conv, "from_seq": f, "to_seq": t, "text": text});
    let head = seqs(&a.pull(&conv, 0).await).len();
    for (who, req, want) in [
        (&g, put(0, 1, "x"), "bad_request"),
        (&g, put(3, 2, "x"), "bad_request"),
        (&g, put(1, joined + 1, "x"), "bad_request"),
        (&g, put(1, 1, ""), "bad_request"),
        (&g, put(1, 1, &"x".repeat(64 * 1024 + 1)), "bad_request"),
        (&g, put(1, 1, "a\0b"), "bad_request"),
        (&l, put(1, last, "before my time"), "forbidden"),
        (
            &g,
            json!({"op": "put_summary", "conv_id": "c_nope", "from_seq": 1, "to_seq": 1, "text": "x"}),
            "not_found",
        ),
    ] {
        assert_eq!(code(&who.call(req.clone()).await), want, "{req}");
    }
    assert_eq!(audits(&hub, "summary.written").await, 0);
    assert_eq!(seqs(&a.pull(&conv, 0).await).len(), head, "no events");
    // Boundaries that are fine: the whole log, 64KB, a single seq; the
    // late member from where it joined.
    g.ok(put(1, joined, &"y".repeat(64 * 1024))).await;
    g.ok(put(joined, joined, "one")).await;
    l.ok(put(joined, joined, "mine")).await;
    // Only the latest 20 are kept.
    for i in 0..25 {
        g.ok(put(1, 1 + i % 3, &format!("s{i}"))).await;
    }
    assert_eq!(audits(&hub, "summary.written").await, 28);
    let n = summary_count(&db, &bot.id).await;
    assert_eq!(n, 20);
}

async fn summary_count(db: &TestDb, agent: &str) -> i64 {
    use sqlx::{Connection, Row};
    sqlx::any::install_default_drivers();
    let mut c = sqlx::AnyConnection::connect(&db.url).await.unwrap();
    let q = if db.url.starts_with("postgres") {
        "SELECT COUNT(*) FROM summaries WHERE agent_id = $1"
    } else {
        "SELECT COUNT(*) FROM summaries WHERE agent_id = ?"
    };
    sqlx::query(q)
        .bind(agent)
        .fetch_one(&mut c)
        .await
        .unwrap()
        .get::<i64, _>(0)
}

/// A3 / D67: every filter narrows, they combine, bad ones are refused
/// before anything is read.
async fn search_filters_narrow_the_hits(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    let helper = user(&hub, "helper", UserKind::Agent, Some(&alice)).await;
    let a = Conn::new(&hub, &alice);
    let h = Conn::new(&hub, &helper);
    let conv = a.group(&[&bot, &helper]).await;
    let first = a.send(&conv, "@bot report one").await["msg_id"].clone();
    h.ok(
        json!({"op": "send", "conv_id": conv, "client_msg_id": "two", "text": "report two",
        "to": [bot.id], "intent": "status", "topics": ["billing"], "refs": [first]}),
    )
    .await;
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    let mid = agentim_core::model::now_ms();
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    a.send(&conv, "@bot report three").await;
    let g = Conn::new(&hub, &bot);
    let found = |f: Value| {
        let g = &g;
        let conv = conv.clone();
        async move {
            let mut q = json!({"op": "search", "query": "report", "conv_id": conv});
            for (k, v) in f.as_object().unwrap() {
                q[k] = v.clone();
            }
            let r = g.ok(q).await;
            let mut t: Vec<String> = r["hits"]
                .as_array()
                .unwrap()
                .iter()
                .map(|h| h["excerpt"].as_str().unwrap().replace("@bot ", ""))
                .collect();
            t.sort();
            t
        }
    };
    let all = ["report one", "report three", "report two"];
    assert_eq!(found(json!({})).await, all);
    assert_eq!(found(json!({"sender_type": "agent"})).await, ["report two"]);
    assert_eq!(
        found(json!({"sender_type": "human"})).await,
        ["report one", "report three"]
    );
    assert_eq!(
        found(json!({"sender_id": alice.id})).await,
        ["report one", "report three"]
    );
    assert_eq!(
        found(json!({"sender_id": "u_nobody"})).await,
        Vec::<String>::new()
    );
    assert_eq!(found(json!({"intent": "status"})).await, ["report two"]);
    assert_eq!(found(json!({"topic": "billing"})).await, ["report two"]);
    assert_eq!(found(json!({"topic": "other"})).await, Vec::<String>::new());
    assert_eq!(found(json!({"has_refs": true})).await, ["report two"]);
    assert_eq!(
        found(json!({"has_refs": false})).await,
        ["report one", "report three"]
    );
    assert_eq!(found(json!({"since": mid})).await, ["report three"]);
    assert_eq!(
        found(json!({"until": mid})).await,
        ["report one", "report two"]
    );
    assert_eq!(
        found(json!({"since": mid, "until": mid})).await,
        Vec::<String>::new()
    );
    assert_eq!(found(json!({"kind": "text"})).await, all);
    assert_eq!(found(json!({"kind": "card"})).await, Vec::<String>::new());
    assert_eq!(
        found(json!({"sender_type": "human", "until": mid})).await,
        ["report one"]
    );
    let long = "x".repeat(65);
    for bad in [
        json!({"sender_type": "robot"}),
        json!({"since": 2, "until": 1}),
        json!({"topic": ""}),
        json!({"topic": long}),
        json!({"intent": long}),
        json!({"kind": ""}),
        json!({"sender_id": long}),
    ] {
        let mut q = json!({"op": "search", "query": "report"});
        for (k, v) in bad.as_object().unwrap() {
            q[k] = v.clone();
        }
        let r = g.call(q).await;
        assert_eq!(r["error"]["code"], "bad_request", "{bad}: {r}");
    }
}

async fn search_finds_only_what_the_agent_may_see(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    let a = Conn::new(&hub, &alice);
    let g1 = a.group(&[&bot]).await;
    let g2 = a.group(&[&bot]).await;
    let dm = a
        .ok(json!({"op": "create_conv", "kind": "dm", "title": "dm", "members": [bot.id]}))
        .await["conv_id"]
        .as_str()
        .unwrap()
        .to_string();
    a.send(&g1, "@bot the Invoice is ready").await;
    a.send(&g1, "unaddressed invoice chatter").await;
    a.send(&g2, "@bot another INVOICE here").await;
    a.send(&dm, "Größe der Rechnung: invoice ünicode").await;
    let gone = a.send(&g2, "@bot invoice to recall").await["msg_id"].clone();
    a.ok(json!({"op": "recall", "msg_id": gone})).await;
    let g = Conn::new(&hub, &bot);
    let r = g.ok(json!({"op": "search", "query": "invoice"})).await;
    let hits = r["hits"].as_array().unwrap();
    let ex: Vec<&str> = hits
        .iter()
        .map(|h| h["excerpt"].as_str().unwrap())
        .collect();
    assert_eq!(hits.len(), 3, "{ex:?}");
    assert!(
        !ex.iter()
            .any(|e| e.contains("chatter") || e.contains("recall"))
    );
    assert!(ex.iter().any(|e| e.contains("ünicode")), "{ex:?}");
    assert_eq!(r["complete"], true);
    // One conversation only; the limit cuts and says so.
    let r = g
        .ok(json!({"op": "search", "query": "INVOICE", "conv_id": g2}))
        .await;
    assert_eq!(r["hits"].as_array().unwrap().len(), 1);
    assert_eq!(r["hits"][0]["conv_id"], json!(g2));
    let r = g
        .ok(json!({"op": "search", "query": "invoice", "limit": 1}))
        .await;
    assert_eq!(r["hits"].as_array().unwrap().len(), 1);
    assert_eq!(r["complete"], false);
    let r = g
        .ok(json!({"op": "search", "query": "nothing like this"}))
        .await;
    assert_eq!(r["hits"], json!([]));
    // An excerpt is cut around the match.
    a.send(
        &g1,
        &format!("@bot {} needle {}", "a".repeat(300), "b".repeat(300)),
    )
    .await;
    let r = g
        .ok(json!({"op": "search", "query": "needle", "conv_id": g1}))
        .await;
    let e = r["hits"][0]["excerpt"].as_str().unwrap();
    assert!(
        e.starts_with('…') && e.ends_with('…') && e.chars().count() < 200,
        "{e}"
    );
    // Audited per conversation with hits.
    assert!(audits(&hub, "agent.search").await >= 4);
}

async fn reads_are_rate_limited_per_agent(db: TestDb) {
    let mut cfg = single(&db.url);
    cfg.agents.history_per_minute = 3;
    cfg.agents.search_per_minute = 2;
    let hub = open(cfg).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    let other = user(&hub, "other", UserKind::Agent, Some(&alice)).await;
    let a = Conn::new(&hub, &alice);
    let conv = a.group(&[&bot, &other]).await;
    let g = Conn::new(&hub, &bot);
    g.ok(hist(&conv)).await;
    g.ok(hist(&conv)).await;
    g.ok(json!({"op": "context", "conv_id": conv})).await;
    let r = g.call(hist(&conv)).await;
    assert_eq!(code(&r), "rate_limited", "{r}");
    assert_eq!(
        code(&g.call(json!({"op": "context", "conv_id": conv})).await),
        "rate_limited"
    );
    // A refused read is not audited.
    assert_eq!(audits(&hub, "agent.history").await, 2);
    // Search has its own budget; other agents theirs.
    g.ok(json!({"op": "search", "query": "zz"})).await;
    g.ok(json!({"op": "search", "query": "zz"})).await;
    assert_eq!(
        code(&g.call(json!({"op": "search", "query": "zz"})).await),
        "rate_limited"
    );
    Conn::new(&hub, &other).ok(hist(&conv)).await;
    // Invalid requests do not spend the budget.
    let fresh = user(&hub, "fresh", UserKind::Agent, Some(&alice)).await;
    a.ok(json!({"op": "add_member", "conv_id": conv, "user_id": fresh.id}))
        .await;
    let f = Conn::new(&hub, &fresh);
    for _ in 0..5 {
        f.call(json!({"op": "history", "conv_id": conv, "limit": 0}))
            .await;
    }
    f.ok(hist(&conv)).await;
}

async fn summaries_survive_a_restart_and_go_with_dissolution(db: TestDb) {
    let (alice, bot, conv);
    {
        let hub = open(single(&db.url)).await;
        alice = user(&hub, "alice", UserKind::Human, None).await;
        bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
        let a = Conn::new(&hub, &alice);
        conv = a.group(&[&bot]).await;
        a.send(&conv, "@bot remember this").await;
        Conn::new(&hub, &bot)
            .ok(json!({"op": "put_summary", "conv_id": conv, "from_seq": 1, "to_seq": 2, "text": "kept across restarts"}))
            .await;
    }
    let hub = open(single(&db.url)).await;
    let g = Conn::new(&hub, &bot);
    let c = g.ok(json!({"op": "context", "conv_id": conv})).await;
    assert_eq!(c["summary"]["text"], "kept across restarts");
    let h = g.ok(hist(&conv)).await;
    assert_eq!(texts(&h), vec!["@bot remember this"]);
    assert_eq!(summary_count(&db, &bot.id).await, 1);
    Conn::new(&hub, &alice)
        .ok(json!({"op": "dissolve", "conv_id": conv}))
        .await;
    assert_eq!(summary_count(&db, &bot.id).await, 0);
}

/// Messages keep arriving while an agent pages forward: every visible
/// event comes exactly once, in order.
async fn race_paging_while_messages_arrive(db: TestDb) {
    let mut cfg = single(&db.url);
    // The reader polls as fast as the runner allows.
    cfg.agents.history_per_minute = 100_000;
    let hub = open(cfg).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    let a = Conn::new(&hub, &alice);
    let conv = a.group(&[&bot]).await;
    for round in 0..3 {
        let start = *seqs(&a.pull(&conv, 0).await).last().unwrap();
        let (h, al, c) = (hub.clone(), alice.clone(), conv.clone());
        let writer = tokio::spawn(async move {
            let a = Conn::new(&h, &al);
            for i in 0..60 {
                let t = if i % 2 == 0 { "@bot" } else { "noise" };
                a.send(&c, &format!("{t} {round}-{i}")).await;
            }
        });
        let g = Conn::new(&hub, &bot);
        let mut got = vec![];
        let mut after = start;
        loop {
            let r = g
                .ok(json!({"op": "history", "conv_id": conv, "after_seq": after, "limit": 7}))
                .await;
            got.extend(ev_seqs(&r));
            after = r["last_seq"].as_i64().unwrap().max(after);
            if writer.is_finished() && r["has_more"] == false {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(1100 / 60)).await;
        }
        writer.await.unwrap();
        // A final read catches whatever the last page raced with.
        let r = g
            .ok(json!({"op": "history", "conv_id": conv, "after_seq": after, "limit": 200}))
            .await;
        got.extend(ev_seqs(&r));
        let want = g
            .ok(json!({"op": "history", "conv_id": conv, "after_seq": start, "limit": 200}))
            .await;
        assert_eq!(got, ev_seqs(&want), "round {round}");
        assert_eq!(texts(&want).len(), 30, "round {round}");
        let mut sorted = got.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted, got, "round {round}: in order, once");
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
}

/// Summaries are written concurrently with context reads: each context
/// starts from a summary that was written, with only later events after
/// it; at most 20 are kept.
async fn race_summaries_and_context(db: TestDb) {
    let mut cfg = single(&db.url);
    cfg.agents.history_per_minute = 100_000;
    let hub = open(cfg).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    let a = Conn::new(&hub, &alice);
    let conv = a.group(&[&bot]).await;
    a.ok(json!({"op": "set_agent_policy", "conv_id": conv, "agent_id": bot.id, "trigger": "all"}))
        .await;
    for i in 0..30 {
        a.send(&conv, &format!("m{i}")).await;
    }
    let last = *seqs(&a.pull(&conv, 0).await).last().unwrap();
    for round in 0..5 {
        let mut tasks = vec![];
        for w in 0..4 {
            let (h, b, c) = (hub.clone(), bot.clone(), conv.clone());
            tasks.push(tokio::spawn(async move {
                let g = Conn::new(&h, &b);
                for k in 0..8 {
                    let to = 1 + (w * 8 + k + round) % last;
                    g.ok(json!({"op": "put_summary", "conv_id": c, "from_seq": 1, "to_seq": to, "text": format!("upto {to}")}))
                        .await;
                }
            }));
        }
        let (h, b, c) = (hub.clone(), bot.clone(), conv.clone());
        let reader = tokio::spawn(async move {
            let g = Conn::new(&h, &b);
            for _ in 0..20 {
                let r = g.ok(json!({"op": "context", "conv_id": c})).await;
                if let Some(s) = r["summary"].as_object() {
                    let to = s["to_seq"].as_i64().unwrap();
                    assert_eq!(s["text"], json!(format!("upto {to}")));
                    assert!(ev_seqs(&r).iter().all(|x| *x > to), "{r}");
                }
            }
        });
        for t in tasks {
            t.await.unwrap();
        }
        reader.await.unwrap();
        assert_eq!(summary_count(&db, &bot.id).await, 20, "round {round}");
    }
}

/// A unit's conversation takes its agents' summaries when it moves
/// database.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn summaries_move_with_their_conversation() {
    let main = new_db(Backend::Sqlite).await.unwrap();
    let secret = new_db(Backend::Sqlite).await.unwrap();
    let open_two = |unit_b: &'static str| {
        let (m, s) = (main.url.clone(), secret.url.clone());
        async move {
            let h = hub_cfg();
            Hub::open(two(&m, &s, unit_b), h).await.unwrap()
        }
    };
    let (bot, conv);
    {
        let hub = open_two("main").await;
        let carol = user_in(&hub, "carol", "b").await;
        bot = user(&hub, "bot", UserKind::Agent, Some(&carol)).await;
        let c = Conn::new(&hub, &carol);
        conv = c.group(&[&bot]).await;
        c.send(&conv, "@bot hello").await;
        Conn::new(&hub, &bot)
            .ok(json!({"op": "put_summary", "conv_id": conv, "from_seq": 1, "to_seq": 2, "text": "moved along"}))
            .await;
    }
    let hub = open_two("secret").await;
    assert_eq!(hub.migrate_unit("b").await.unwrap()["moved"], 1);
    let c = Conn::new(&hub, &bot)
        .ok(json!({"op": "context", "conv_id": conv}))
        .await;
    assert_eq!(c["summary"]["text"], "moved along");
    assert_eq!(summary_count(&main, &bot.id).await, 0);
    assert_eq!(summary_count(&secret, &bot.id).await, 1);
}
