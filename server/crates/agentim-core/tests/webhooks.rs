//! Webhooks and the before-send hook (docs/02-architecture.md §2.6): order,
//! signatures, filters, retries, dropping after the last attempt, nothing for
//! refused or uncommitted writes, restart without loss, no replay for a new
//! webhook, outbox clean-up, several databases, concurrent senders, and every
//! answer of the before-send hook. Run against every backend.

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use agentim_core::config::{BeforeSendConfig, Config, HookFailure, WebhookConfig};
use agentim_core::hub::Hub;
use agentim_core::model::*;
use axum::{Router, http::HeaderMap, http::StatusCode, routing::post};
use common::*;
use serde_json::{Value, json};
use sqlx::Connection;

backend_tests!(
    events_are_delivered_in_order_and_signed,
    filters_choose_the_event_types,
    failed_deliveries_are_retried_in_order,
    an_event_is_dropped_after_the_last_attempt,
    refused_and_uncommitted_writes_send_nothing,
    a_restart_resumes_without_loss,
    a_new_webhook_does_not_replay_history,
    delivered_events_leave_the_outbox,
    events_of_every_database_are_delivered,
    concurrent_senders_are_all_delivered_once,
    before_send_allows_rejects_and_replaces,
    before_send_failures_follow_the_configuration,
);

const SECRET: &str = "whsec_test_secret_0123456789";

#[derive(Clone, Default)]
struct Receiver {
    got: Arc<Mutex<Vec<(HeaderMap, Value)>>>,
    /// Requests answered 500 before succeeding again.
    fail_next: Arc<Mutex<u32>>,
    /// Bodies containing this always fail.
    poison: Arc<Mutex<Option<String>>>,
    down: Arc<Mutex<bool>>,
}

impl Receiver {
    async fn start() -> (Receiver, String) {
        let r = Receiver::default();
        let st = r.clone();
        let app = Router::new()
            .route(
                "/hook",
                post(move |h: HeaderMap, body: String| {
                    let st = st.clone();
                    async move { st.handle(h, body) }
                }),
            )
            .route("/before", post(before_send_endpoint));
        let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = tcp.local_addr().unwrap().to_string();
        tokio::spawn(async move { axum::serve(tcp, app).await.unwrap() });
        (r, addr)
    }

    fn handle(&self, h: HeaderMap, body: String) -> StatusCode {
        if *self.down.lock().unwrap() {
            return StatusCode::SERVICE_UNAVAILABLE;
        }
        if let Some(p) = self.poison.lock().unwrap().as_deref()
            && body.contains(p)
        {
            return StatusCode::INTERNAL_SERVER_ERROR;
        }
        {
            let mut f = self.fail_next.lock().unwrap();
            if *f > 0 {
                *f -= 1;
                return StatusCode::INTERNAL_SERVER_ERROR;
            }
        }
        self.got
            .lock()
            .unwrap()
            .push((h, serde_json::from_str(&body).unwrap()));
        StatusCode::NO_CONTENT
    }

    fn bodies(&self) -> Vec<Value> {
        self.got
            .lock()
            .unwrap()
            .iter()
            .map(|(_, b)| b.clone())
            .collect()
    }
}

/// Replies to before-send requests according to the message text.
async fn before_send_endpoint(h: HeaderMap, body: String) -> (StatusCode, String) {
    let v: Value = serde_json::from_str(&body).unwrap();
    let ts: i64 = h["webhook-timestamp"].to_str().unwrap().parse().unwrap();
    let id = h["webhook-id"].to_str().unwrap();
    assert_eq!(
        h["webhook-signature"].to_str().unwrap(),
        agentim_core::webhook::sign(SECRET, id, ts, &body),
        "signed"
    );
    let text = v["text"].as_str().unwrap_or("");
    let ok = |b: Value| (StatusCode::OK, b.to_string());
    match text {
        t if t.starts_with("reject") => {
            ok(json!({"action": "reject", "reason": "not allowed here"}))
        }
        t if t.starts_with("replace-bad") => ok(json!({"action": "replace", "text": ""})),
        t if t.starts_with("replace-nothing") => ok(json!({"action": "replace"})),
        t if t.starts_with("replace") => ok(
            json!({"action": "replace", "text": "[filtered]", "payload": v["payload"].is_object().then(|| json!({"masked": true}))}),
        ),
        t if t.starts_with("slow") => {
            tokio::time::sleep(Duration::from_millis(800)).await;
            ok(json!({"action": "allow"}))
        }
        t if t.starts_with("garbage") => (StatusCode::OK, "not json".into()),
        t if t.starts_with("weird") => ok(json!({"action": "maybe"})),
        t if t.starts_with("error") => (StatusCode::INTERNAL_SERVER_ERROR, String::new()),
        _ => {
            assert!(
                v["sender"]["id"].is_string() && v["type"] == "message.before_send",
                "{v}"
            );
            ok(json!({"action": "allow"}))
        }
    }
}

fn webhook(addr: &str, name: &str, events: &[&str]) -> WebhookConfig {
    WebhookConfig {
        name: name.into(),
        url: format!("http://{addr}/hook"),
        secret: SECRET.into(),
        events: events.iter().map(|e| e.to_string()).collect(),
        max_attempts: 50,
        timeout_ms: 2000,
        backoff_ms: 10,
    }
}

fn with(mut cfg: Config, hooks: Vec<WebhookConfig>) -> Config {
    cfg.webhooks = hooks;
    cfg
}

async fn wait_until(what: &str, f: impl Fn() -> bool) {
    for _ in 0..500 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("timed out waiting for {what}");
}

fn seq_of(b: &Value) -> i64 {
    b["data"]["seq"].as_i64().unwrap()
}

async fn raw_count(db: &TestDb, sql: &str) -> i64 {
    sqlx::any::install_default_drivers();
    let mut c = sqlx::AnyConnection::connect(&db.url.replace("mariadb://", "mysql://"))
        .await
        .unwrap();
    sqlx::Row::get(
        &sqlx::query(sqlx::AssertSqlSafe(sql.to_string()))
            .fetch_one(&mut c)
            .await
            .unwrap(),
        0,
    )
}

async fn events_are_delivered_in_order_and_signed(db: TestDb) {
    let (rx, addr) = Receiver::start().await;
    let hub = open(with(single(&db.url), vec![webhook(&addr, "main", &["*"])])).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let a = Conn::new(&hub, &alice);
    let conv = a.group(&[&bob]).await;
    for i in 0..20 {
        a.send(&conv, &format!("m{i}")).await;
    }
    let head = *seqs(&a.pull(&conv, 0).await).last().unwrap();
    wait_until("all events", || rx.bodies().len() as i64 >= head).await;
    let got = rx.got.lock().unwrap().clone();
    // Exactly the log, in order: conv.created, member.joined x2, 20 messages.
    let s: Vec<i64> = got.iter().map(|(_, b)| seq_of(b)).collect();
    assert_eq!(s, (1..=head).collect::<Vec<_>>());
    assert_eq!(got[0].1["type"], "conv.created");
    assert_eq!(
        got.last().unwrap().1["data"]["event"]["msg"]["content"],
        "m19"
    );
    let mut ids = std::collections::HashSet::new();
    for (h, b) in &got {
        let id = h["webhook-id"].to_str().unwrap();
        let ts: i64 = h["webhook-timestamp"].to_str().unwrap().parse().unwrap();
        assert!(ids.insert(id.to_string()), "unique ids");
        assert!(
            (ts - std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs() as i64)
                .abs()
                < 60
        );
        assert_eq!(
            h["webhook-signature"].to_str().unwrap(),
            agentim_core::webhook::sign(SECRET, id, ts, &b.to_string()),
        );
        assert_eq!(b["data"]["conv_id"], conv.as_str());
        assert_eq!(b["database"], "main");
    }
}

async fn filters_choose_the_event_types(db: TestDb) {
    let (rx, addr) = Receiver::start().await;
    let (rx2, addr2) = Receiver::start().await;
    let hub = open(with(
        single(&db.url),
        vec![
            webhook(&addr, "msgs", &["msg.*"]),
            webhook(&addr2, "joins", &["member.joined", "conv.created"]),
        ],
    ))
    .await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let a = Conn::new(&hub, &alice);
    let conv = a.group(&[&bob]).await;
    let id = a.send(&conv, "x").await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    a.ok(json!({"op": "edit", "msg_id": id, "text": "y"})).await;
    a.ok(json!({"op": "recall", "msg_id": id})).await;
    a.ok(json!({"op": "remove_member", "conv_id": conv, "user_id": bob.id}))
        .await;
    wait_until("msg events", || rx.bodies().len() >= 3).await;
    wait_until("join events", || rx2.bodies().len() >= 3).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let t = |r: &Receiver| {
        r.bodies()
            .iter()
            .map(|b| b["type"].as_str().unwrap().to_string())
            .collect::<Vec<_>>()
    };
    assert_eq!(t(&rx), ["msg.created", "msg.updated", "msg.recalled"]);
    assert_eq!(t(&rx2), ["conv.created", "member.joined", "member.joined"]);
    // A recalled message's content is not in the recall event.
    assert_eq!(rx.bodies()[2]["data"]["event"]["msg"]["content"], "");
}

async fn failed_deliveries_are_retried_in_order(db: TestDb) {
    let (rx, addr) = Receiver::start().await;
    *rx.fail_next.lock().unwrap() = 5;
    let hub = open(with(
        single(&db.url),
        vec![webhook(&addr, "main", &["msg.created"])],
    ))
    .await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let a = Conn::new(&hub, &alice);
    let conv = a.group(&[&alice]).await;
    for i in 0..10 {
        a.send(&conv, &format!("m{i}")).await;
    }
    wait_until("10 messages", || rx.bodies().len() >= 10).await;
    let contents: Vec<String> = rx
        .bodies()
        .iter()
        .map(|b| {
            b["data"]["event"]["msg"]["content"]
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect();
    assert_eq!(
        contents,
        (0..10).map(|i| format!("m{i}")).collect::<Vec<_>>()
    );
    assert_eq!(*rx.fail_next.lock().unwrap(), 0, "the failures happened");
}

async fn an_event_is_dropped_after_the_last_attempt(db: TestDb) {
    let (rx, addr) = Receiver::start().await;
    *rx.poison.lock().unwrap() = Some("poison".into());
    let mut wh = webhook(&addr, "main", &["msg.created"]);
    wh.max_attempts = 3;
    let hub = open(with(single(&db.url), vec![wh])).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let a = Conn::new(&hub, &alice);
    let conv = a.group(&[&alice]).await;
    a.send(&conv, "before").await;
    a.send(&conv, "poison pill").await;
    a.send(&conv, "after").await;
    wait_until("the event after", || rx.bodies().len() >= 2).await;
    let c: Vec<Value> = rx
        .bodies()
        .iter()
        .map(|b| b["data"]["event"]["msg"]["content"].clone())
        .collect();
    assert_eq!(c, [json!("before"), json!("after")]);
    // The drop is audited, and the outbox does not keep it.
    drop(a);
    wait_until_db(
        &db,
        "SELECT COUNT(*) FROM audit WHERE action = 'webhook.dropped'",
        1,
    )
    .await;
    wait_until_db(&db, "SELECT COUNT(*) FROM webhook_outbox", 0).await;
    drop(hub);
}

async fn wait_until_db(db: &TestDb, sql: &str, want: i64) {
    for _ in 0..250 {
        if raw_count(db, sql).await == want {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!(
        "{sql} never became {want} (is {})",
        raw_count(db, sql).await
    );
}

async fn refused_and_uncommitted_writes_send_nothing(db: TestDb) {
    let (rx, addr) = Receiver::start().await;
    let cfg = with(single(&db.url), vec![webhook(&addr, "main", &["msg.*"])]);
    let hub = open(cfg.clone()).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let eve = user(&hub, "eve", UserKind::Human, None).await;
    let a = Conn::new(&hub, &alice);
    let e = Conn::new(&hub, &eve);
    let conv = a.group(&[&alice]).await;
    // Refused requests.
    assert_eq!(e.send(&conv, "intruder").await["ok"], false);
    assert_eq!(a.send(&conv, "").await["ok"], false);
    // A write whose commit fails.
    hub.inject_commit_failures("main", 1);
    assert_eq!(a.send(&conv, "never committed").await["ok"], false);
    drop(a);
    drop(e);
    drop(hub);
    let hub = open(cfg).await;
    let a = Conn::new(&hub, &alice);
    a.send(&conv, "committed").await;
    wait_until("the committed message", || !rx.bodies().is_empty()).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let c: Vec<Value> = rx
        .bodies()
        .iter()
        .map(|b| b["data"]["event"]["msg"]["content"].clone())
        .collect();
    assert_eq!(c, [json!("committed")]);
}

async fn a_restart_resumes_without_loss(db: TestDb) {
    let (rx, addr) = Receiver::start().await;
    let cfg = with(
        single(&db.url),
        vec![webhook(&addr, "main", &["msg.created"])],
    );
    let hub = open(cfg.clone()).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let a = Conn::new(&hub, &alice);
    let conv = a.group(&[&alice]).await;
    for i in 0..5 {
        a.send(&conv, &format!("m{i}")).await;
    }
    wait_until("first five", || rx.bodies().len() >= 5).await;
    tokio::time::sleep(Duration::from_millis(200)).await; // cursor written
    *rx.down.lock().unwrap() = true;
    for i in 5..10 {
        a.send(&conv, &format!("m{i}")).await;
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    drop(a);
    drop(hub);
    *rx.down.lock().unwrap() = false;
    let hub = open(cfg).await;
    wait_until("all ten", || rx.bodies().len() >= 10).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let c: Vec<String> = rx
        .bodies()
        .iter()
        .map(|b| {
            b["data"]["event"]["msg"]["content"]
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect();
    assert_eq!(
        c,
        (0..10).map(|i| format!("m{i}")).collect::<Vec<_>>(),
        "in order, nothing twice"
    );
    drop(hub);
}

async fn a_new_webhook_does_not_replay_history(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let conv = Conn::new(&hub, &alice).group(&[&alice]).await;
    Conn::new(&hub, &alice).send(&conv, "old").await;
    drop(hub);
    // With no webhook nothing was queued.
    assert_eq!(
        raw_count(&db, "SELECT COUNT(*) FROM webhook_outbox").await,
        0
    );
    let (rx, addr) = Receiver::start().await;
    let hub = open(with(
        single(&db.url),
        vec![webhook(&addr, "late", &["msg.created"])],
    ))
    .await;
    Conn::new(&hub, &alice).send(&conv, "new").await;
    wait_until("the new message", || !rx.bodies().is_empty()).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(rx.bodies().len(), 1);
    assert_eq!(rx.bodies()[0]["data"]["event"]["msg"]["content"], "new");
}

async fn delivered_events_leave_the_outbox(db: TestDb) {
    let (rx, addr) = Receiver::start().await;
    let (rx2, addr2) = Receiver::start().await;
    *rx2.down.lock().unwrap() = true;
    let hub = open(with(
        single(&db.url),
        vec![
            webhook(&addr, "a", &["msg.created"]),
            webhook(&addr2, "b", &["msg.created"]),
        ],
    ))
    .await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let a = Conn::new(&hub, &alice);
    let conv = a.group(&[&alice]).await;
    for i in 0..3 {
        a.send(&conv, &format!("m{i}")).await;
    }
    wait_until("webhook a", || rx.bodies().len() >= 3).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    // Kept while webhook b has not received them.
    assert_eq!(
        raw_count(&db, "SELECT COUNT(*) FROM webhook_outbox").await,
        3
    );
    *rx2.down.lock().unwrap() = false;
    wait_until("webhook b", || rx2.bodies().len() >= 3).await;
    wait_until_db(&db, "SELECT COUNT(*) FROM webhook_outbox", 0).await;
    drop(hub);
}

async fn events_of_every_database_are_delivered(db: TestDb) {
    let Some(secret) = new_db(Backend::Sqlite).await else {
        return;
    };
    let (rx, addr) = Receiver::start().await;
    let hub = open(with(
        two(&db.url, &secret.url, "secret"),
        vec![webhook(&addr, "main", &["msg.created"])],
    ))
    .await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bea = user_in(&hub, "bea", "b").await;
    let shared = Conn::new(&hub, &alice).group(&[&alice]).await;
    let unit = Conn::new(&hub, &bea).group(&[&bea]).await;
    Conn::new(&hub, &alice).send(&shared, "in main").await;
    Conn::new(&hub, &bea).send(&unit, "in secret").await;
    wait_until("both", || rx.bodies().len() >= 2).await;
    let mut by_db: Vec<(String, String)> = rx
        .bodies()
        .iter()
        .map(|b| {
            (
                b["database"].as_str().unwrap().to_string(),
                b["data"]["event"]["msg"]["content"]
                    .as_str()
                    .unwrap()
                    .to_string(),
            )
        })
        .collect();
    by_db.sort();
    assert_eq!(
        by_db,
        [
            ("main".into(), "in main".into()),
            ("secret".into(), "in secret".into())
        ]
    );
}

async fn concurrent_senders_are_all_delivered_once(db: TestDb) {
    let (rx, addr) = Receiver::start().await;
    let hub = open(with(
        single(&db.url),
        vec![webhook(&addr, "main", &["msg.created"])],
    ))
    .await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let conv = Conn::new(&hub, &alice).group(&[&bob]).await;
    let mut tasks = vec![];
    for (k, u) in [&alice, &bob, &alice, &bob].into_iter().enumerate() {
        let (hub, u, conv) = (hub.clone(), u.clone(), conv.clone());
        tasks.push(tokio::spawn(async move {
            let c = Conn::new(&hub, &u);
            for i in 0..25 {
                assert_eq!(c.send(&conv, &format!("{k}-{i}")).await["ok"], true);
            }
        }));
    }
    for t in tasks {
        t.await.unwrap();
    }
    wait_until("100 messages", || rx.bodies().len() >= 100).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let b = rx.bodies();
    assert_eq!(b.len(), 100);
    let s: Vec<i64> = b.iter().map(seq_of).collect();
    assert!(s.windows(2).all(|w| w[0] < w[1]), "log order");
    let mut contents: Vec<String> = b
        .iter()
        .map(|x| {
            x["data"]["event"]["msg"]["content"]
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect();
    contents.sort();
    contents.dedup();
    assert_eq!(contents.len(), 100, "each exactly once");
}

fn before(addr: &str, on_error: HookFailure, timeout_ms: u64) -> BeforeSendConfig {
    BeforeSendConfig {
        url: format!("http://{addr}/before"),
        secret: SECRET.into(),
        timeout_ms,
        on_error,
    }
}

async fn before_send_allows_rejects_and_replaces(db: TestDb) {
    let (_rx, addr) = Receiver::start().await;
    let mut cfg = single(&db.url);
    cfg.before_send = Some(before(&addr, HookFailure::Allow, 2000));
    let hub = open(cfg).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let a = Conn::new(&hub, &alice);
    let mut b = Conn::new(&hub, &bob);
    let conv = a.group(&[&bob]).await;
    let head = seqs(&a.pull(&conv, 0).await).len();
    b.drain();
    let code = |r: &Value| r["error"]["code"].as_str().unwrap_or("").to_string();
    // Allowed.
    let ok = a.send(&conv, "hello").await;
    assert_eq!(ok["ok"], true);
    // Rejected, with the hook's reason or a default one; nothing written or pushed.
    let r = a.send(&conv, "reject this").await;
    assert_eq!(
        (code(&r).as_str(), r["error"]["message"].as_str()),
        ("policy_blocked", Some("not allowed here"))
    );
    // Replaced text, and payload for a custom kind.
    let r = a.send(&conv, "replace my words").await;
    let m = msg_by_id(&a.pull(&conv, 0).await, r["msg_id"].as_str().unwrap()).unwrap();
    assert_eq!(m["content"], "[filtered]");
    let r = a
        .ok(json!({"op": "send", "conv_id": conv, "client_msg_id": "k1", "text": "replace card", "kind": "com.acme.card", "payload": {"pan": "1234"}}))
        .await;
    let m = msg_by_id(&a.pull(&conv, 0).await, r["msg_id"].as_str().unwrap()).unwrap();
    assert_eq!(
        (&m["content"], &m["payload"]),
        (&json!("[filtered]"), &json!({"masked": true}))
    );
    // A replacement must still be a valid message.
    assert_eq!(code(&a.send(&conv, "replace-bad").await), "bad_request");
    // Invalid requests are refused before the hook is asked.
    assert_eq!(code(&a.send(&conv, "").await), "bad_request");
    let r = a
        .call(json!({"op": "send", "conv_id": conv, "client_msg_id": "k2", "text": "t", "payload": {}}))
        .await;
    assert_eq!(code(&r), "bad_request");
    // Membership is still checked after the hook.
    let eve = user(&hub, "eve", UserKind::Human, None).await;
    assert_eq!(
        code(&Conn::new(&hub, &eve).send(&conv, "hello").await),
        "forbidden"
    );
    let msgs: Vec<String> = b
        .drain()
        .iter()
        .filter(|f| f["type"] == "event")
        .map(|f| f["event"]["msg"]["content"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(msgs, ["hello", "[filtered]", "[filtered]"]);
    assert_eq!(seqs(&a.pull(&conv, 0).await).len(), head + 3);
    // Edits and streams do not go through the hook.
    a.ok(json!({"op": "edit", "msg_id": ok["msg_id"], "text": "reject edits are fine"}))
        .await;
}

async fn before_send_failures_follow_the_configuration(db: TestDb) {
    let (_rx, addr) = Receiver::start().await;
    for on_error in [HookFailure::Allow, HookFailure::Reject] {
        let db = new_db(Backend::Sqlite).await.unwrap();
        let mut cfg = single(&db.url);
        cfg.before_send = Some(before(&addr, on_error, 300));
        let hub = open(cfg).await;
        let alice = user(&hub, "alice", UserKind::Human, None).await;
        let a = Conn::new(&hub, &alice);
        let conv = a.group(&[&alice]).await;
        for text in [
            "slow answer",
            "garbage answer",
            "weird answer",
            "error answer",
            "replace-nothing",
        ] {
            let r = a.send(&conv, text).await;
            match on_error {
                HookFailure::Allow => assert_eq!(r["ok"], true, "{text}: {r}"),
                HookFailure::Reject => {
                    assert_eq!(r["error"]["code"], "policy_blocked", "{text}: {r}")
                }
            }
        }
    }
    // An unreachable hook behaves the same.
    let mut cfg = single(&db.url);
    cfg.before_send = Some(BeforeSendConfig {
        url: "http://127.0.0.1:9/nothing".into(),
        ..before(&addr, HookFailure::Reject, 300)
    });
    let hub = open(cfg).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let a = Conn::new(&hub, &alice);
    let conv = a.group(&[&alice]).await;
    assert_eq!(a.send(&conv, "hi").await["error"]["code"], "policy_blocked");
    let _: Option<Hub> = None;
}
