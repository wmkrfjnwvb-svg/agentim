//! Agent checkpoints (docs/03-agents.md §7.2): versioned values compared on write,
//! older versions, keys in byte order, limits, refusals that leave no
//! trace, restart, and runtimes racing on the same key.

mod common;

use agentim_core::hub::Hub;
use agentim_core::model::*;
use common::*;
use serde_json::{Value, json};

backend_tests!(
    values_are_versioned,
    keys_and_values_have_limits,
    keys_list_in_byte_order,
    state_refusals_leave_no_trace,
    state_survives_a_restart,
    race_one_writer_per_version,
    race_the_key_limit,
);

fn code(v: &Value) -> &str {
    v["error"]["code"].as_str().unwrap_or("")
}

fn put(key: &str, value: Value, version: i64) -> Value {
    json!({"op": "state_put", "key": key, "value": value, "version": version})
}

fn get(key: &str) -> Value {
    json!({"op": "state_get", "key": key})
}

async fn agent(hub: &Hub, name: &str) -> (User, Conn) {
    let owner = match hub.user_by_name("owner") {
        Some(o) => o,
        None => user(hub, "owner", UserKind::Human, None).await,
    };
    let a = user(hub, name, UserKind::Agent, Some(&owner)).await;
    let c = Conn::new(hub, &a);
    (a, c)
}

async fn values_are_versioned(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let (_, g) = agent(&hub, "bot").await;
    let (_, other) = agent(&hub, "other").await;
    assert_eq!(code(&g.call(get("plan")).await), "not_found");
    let r = g.ok(put("plan", json!({"step": 1}), 0)).await;
    assert_eq!(r["version"], 1);
    // Creating twice is a conflict: somebody else created it first.
    assert_eq!(
        code(&g.call(put("plan", json!({"step": 9}), 0)).await),
        "conflict"
    );
    g.ok(put("plan", json!({"step": 2}), 1)).await;
    let stale = g.call(put("plan", json!({"step": 3}), 1)).await;
    assert_eq!(code(&stale), "conflict");
    assert!(
        stale["error"]["message"]
            .as_str()
            .unwrap()
            .contains("version 2"),
        "{stale}"
    );
    let r = g.ok(get("plan")).await;
    assert_eq!(
        (&r["value"], &r["version"], &r["current_version"]),
        (&json!({"step": 2}), &json!(2), &json!(2))
    );
    let old = g
        .ok(json!({"op": "state_get", "key": "plan", "version": 1}))
        .await;
    assert_eq!(
        (&old["value"], &old["current_version"]),
        (&json!({"step": 1}), &json!(2))
    );
    // Each agent has its own keys.
    assert_eq!(code(&other.call(get("plan")).await), "not_found");
    other.ok(put("plan", json!("mine"), 0)).await;
    assert_eq!(g.ok(get("plan")).await["value"], json!({"step": 2}));
    // The latest 20 older versions are kept.
    for v in 2..30 {
        g.ok(put("plan", json!({"step": v + 1}), v)).await;
    }
    let ver = |v: i64| json!({"op": "state_get", "key": "plan", "version": v});
    assert_eq!(code(&g.call(ver(9)).await), "not_found");
    assert_eq!(g.ok(ver(10)).await["value"], json!({"step": 10}));
    assert_eq!(g.ok(ver(30)).await["value"], json!({"step": 30}));
    assert_eq!(code(&g.call(ver(31)).await), "not_found");
    // Delete: by its current version; then it starts over.
    let del = |v: i64| json!({"op": "state_delete", "key": "plan", "version": v});
    assert_eq!(code(&g.call(del(29)).await), "conflict");
    g.ok(del(30)).await;
    assert_eq!(code(&g.call(get("plan")).await), "not_found");
    assert_eq!(code(&g.call(ver(20)).await), "not_found");
    assert_eq!(code(&g.call(del(30)).await), "not_found");
    assert_eq!(g.ok(put("plan", json!(null), 0)).await["version"], 1);
    assert_eq!(other.ok(get("plan")).await["value"], json!("mine"));
}

async fn keys_and_values_have_limits(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let (_, g) = agent(&hub, "bot").await;
    // Any JSON value up to 1 MiB serialized.
    for v in [
        json!(null),
        json!(0),
        json!(-1.5),
        json!("ünïcode ✓"),
        json!([1, [2, {"x": []}]]),
        json!({"nested": {"deep": true}}),
    ]
    .into_iter()
    .enumerate()
    {
        let (k, v) = (format!("v{}", v.0), v.1);
        g.ok(put(&k, v.clone(), 0)).await;
        assert_eq!(g.ok(get(&k)).await["value"], v);
    }
    let big = "x".repeat(1024 * 1024 - 2);
    g.ok(put("big", json!(big), 0)).await;
    assert_eq!(
        g.ok(get("big")).await["value"].as_str().unwrap().len(),
        big.len()
    );
    assert_eq!(
        code(&g.call(put("bigger", json!(format!("{big}y")), 0)).await),
        "bad_request"
    );
    // Keys: 1..255 bytes, any text but control characters, case matters.
    g.ok(put(&"k".repeat(255), json!(1), 0)).await;
    g.ok(put("ключ/路径 with spaces", json!(1), 0)).await;
    g.ok(put("Case", json!("upper"), 0)).await;
    g.ok(put("case", json!("lower"), 0)).await;
    assert_eq!(g.ok(get("Case")).await["value"], "upper");
    assert_eq!(g.ok(get("case")).await["value"], "lower");
    // The key limit (agents.state_keys).
    let mut cfg = single(&db.url);
    cfg.agents.state_keys = 3;
    drop(g);
    drop(hub);
    let hub = open(cfg).await;
    let (_, f) = agent(&hub, "fresh").await;
    for k in ["a", "b", "c"] {
        f.ok(put(k, json!(1), 0)).await;
    }
    assert_eq!(code(&f.call(put("d", json!(1), 0)).await), "rate_limited");
    // Updating an existing key is still fine; deleting frees a slot.
    f.ok(put("a", json!(2), 1)).await;
    f.ok(json!({"op": "state_delete", "key": "b", "version": 1}))
        .await;
    f.ok(put("d", json!(1), 0)).await;
}

async fn keys_list_in_byte_order(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let (_, g) = agent(&hub, "bot").await;
    let keys = [
        "b", "a/b", "a-c", "a.c", "A", "a/a", "a/", "a", "a/b/c", "ab", "é", "z",
    ];
    for k in keys {
        g.ok(put(k, json!(k), 0)).await;
    }
    g.ok(put("a/b", json!("again"), 1)).await;
    let mut want: Vec<&str> = keys.to_vec();
    want.sort_by(|x, y| x.as_bytes().cmp(y.as_bytes()));
    let list = |v: Value| {
        let g = &g;
        async move {
            let r = g.ok(v).await;
            let ks: Vec<String> = r["keys"]
                .as_array()
                .unwrap()
                .iter()
                .map(|k| k["key"].as_str().unwrap().to_string())
                .collect();
            (ks, r)
        }
    };
    let (all, r) = list(json!({"op": "state_list"})).await;
    assert_eq!(all, want);
    assert_eq!(r["more"], false);
    assert_eq!(
        r["keys"]
            .as_array()
            .unwrap()
            .iter()
            .find(|k| k["key"] == "a/b")
            .unwrap()["version"],
        2
    );
    // Prefix.
    let (p, _) = list(json!({"op": "state_list", "prefix": "a/"})).await;
    assert_eq!(p, vec!["a/", "a/a", "a/b", "a/b/c"]);
    let (p, _) = list(json!({"op": "state_list", "prefix": "nope"})).await;
    assert!(p.is_empty());
    // Paging by the last key, with and without a prefix.
    let mut got = vec![];
    let mut after = String::new();
    loop {
        let (page, r) = list(json!({"op": "state_list", "after": after, "limit": 5})).await;
        got.extend(page.clone());
        if r["more"] == false {
            break;
        }
        after = page.last().unwrap().clone();
    }
    assert_eq!(got, want);
    let (p, r) =
        list(json!({"op": "state_list", "prefix": "a/", "after": "a/a", "limit": 1})).await;
    assert_eq!((p, &r["more"]), (vec!["a/b".to_string()], &json!(true)));
    // Bounds.
    for l in [0, 501] {
        assert_eq!(
            code(&g.call(json!({"op": "state_list", "limit": l})).await),
            "bad_request"
        );
    }
    g.ok(json!({"op": "state_list", "limit": 500})).await;
    g.ok(json!({"op": "state_list", "limit": 1})).await;
}

async fn state_refusals_leave_no_trace(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let (_, g) = agent(&hub, "bot").await;
    let a = Conn::new(&hub, &alice);
    g.ok(put("k", json!(1), 0)).await;
    for req in [
        get("k"),
        put("k", json!(2), 1),
        json!({"op": "state_list"}),
        json!({"op": "state_delete", "key": "k", "version": 1}),
    ] {
        assert_eq!(code(&a.call(req.clone()).await), "forbidden", "{req}");
    }
    let long = "k".repeat(256);
    for req in [
        put("", json!(1), 0),
        put(&long, json!(1), 0),
        put("a\nb", json!(1), 0),
        put("a\u{7f}", json!(1), 0),
        put("k", json!(1), -1),
        put("archive/x", json!(1), 0),
        get(""),
        get(&long),
        json!({"op": "state_get", "key": "k", "version": 0}),
        json!({"op": "state_delete", "key": "k", "version": 0}),
        json!({"op": "state_delete", "key": "archive/x", "version": 1}),
        json!({"op": "state_list", "prefix": long}),
        json!({"op": "state_list", "after": long}),
        json!({"op": "state_put", "key": "k", "version": 1}),
    ] {
        assert_eq!(code(&g.call(req.clone()).await), "bad_request", "{req}");
    }
    for req in [put("k", json!(2), 5), put("k", json!(2), 0)] {
        assert_eq!(code(&g.call(req.clone()).await), "conflict", "{req}");
    }
    let r = g.ok(get("k")).await;
    assert_eq!((&r["value"], &r["version"]), (&json!(1), &json!(1)));
    let (keys, _) = (
        g.ok(json!({"op": "state_list"})).await["keys"]
            .as_array()
            .unwrap()
            .len(),
        (),
    );
    assert_eq!(keys, 1);
}

async fn state_survives_a_restart(db: TestDb) {
    let bot;
    {
        let hub = open(single(&db.url)).await;
        let (b, g) = agent(&hub, "bot").await;
        bot = b;
        g.ok(put("plan", json!({"step": 1}), 0)).await;
        g.ok(put("plan", json!({"step": 2}), 1)).await;
    }
    let hub = open(single(&db.url)).await;
    let g = Conn::new(&hub, &bot);
    let r = g.ok(get("plan")).await;
    assert_eq!(
        (&r["value"], &r["version"]),
        (&json!({"step": 2}), &json!(2))
    );
    assert_eq!(
        g.ok(json!({"op": "state_get", "key": "plan", "version": 1}))
            .await["value"],
        json!({"step": 1})
    );
    g.ok(put("plan", json!({"step": 3}), 2)).await;
}

/// Runtimes that read the same version and write it back: exactly one
/// wins each round, and the value is the winner's.
async fn race_one_writer_per_version(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let (bot, g) = agent(&hub, "bot").await;
    g.ok(put("counter", json!(0), 0)).await;
    for round in 1..=20 {
        let mut tasks = vec![];
        for w in 0..6 {
            let (h, b) = (hub.clone(), bot.clone());
            tasks.push(tokio::spawn(async move {
                let r = Conn::new(&h, &b)
                    .call(put("counter", json!(format!("{round}-{w}")), round))
                    .await;
                (w, r)
            }));
        }
        let mut winners = vec![];
        for t in tasks {
            let (w, r) = t.await.unwrap();
            if r["ok"] == true {
                winners.push(w);
            } else {
                assert_eq!(code(&r), "conflict", "round {round}: {r}");
            }
        }
        assert_eq!(winners.len(), 1, "round {round}");
        let r = g.ok(get("counter")).await;
        assert_eq!(r["version"], round + 1);
        assert_eq!(r["value"], json!(format!("{round}-{}", winners[0])));
    }
}

/// Concurrent creates at the key limit: exactly the free slots are taken.
async fn race_the_key_limit(db: TestDb) {
    let mut cfg = single(&db.url);
    cfg.agents.state_keys = 3;
    let hub = open(cfg).await;
    for round in 0..10 {
        let (bot, g) = agent(&hub, &format!("bot{round}")).await;
        let mut tasks = vec![];
        for w in 0..8 {
            let (h, b) = (hub.clone(), bot.clone());
            tasks.push(tokio::spawn(async move {
                Conn::new(&h, &b)
                    .call(put(&format!("k{w}"), json!(w), 0))
                    .await
            }));
        }
        let mut ok = 0;
        for t in tasks {
            let r = t.await.unwrap();
            if r["ok"] == true {
                ok += 1;
            } else {
                assert_eq!(code(&r), "rate_limited", "{r}");
            }
        }
        assert_eq!(ok, 3, "round {round}");
        let n = g.ok(json!({"op": "state_list"})).await["keys"]
            .as_array()
            .unwrap()
            .len();
        assert_eq!(n, 3, "round {round}");
    }
}
