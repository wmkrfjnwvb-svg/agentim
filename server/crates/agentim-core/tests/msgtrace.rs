//! A message's trace (docs/14 §4.3, D141): put together from stored data —
//! delivery per device, who can see it, agents' replies and drafts, edits
//! and recalls, webhook states, reports and files — metadata only; for the
//! owner, the operator and the moderator; audited.

mod common;

use std::net::SocketAddr;
use std::time::Duration;

use agentim_core::config::WebhookConfig;
use agentim_core::hub::Hub;
use agentim_core::model::*;
use agentim_core::server::{AppState, router};
use axum::Router;
use axum::routing::post;
use common::*;
use serde_json::{Value, json};

backend_tests!(
    delivery_is_counted_per_device,
    who_could_see_it_follows_the_members,
    replies_drafts_edits_recall_and_reports,
    webhook_states_while_queued,
    webhook_states_after_the_outbox_is_cleaned,
    who_may_read_a_trace_and_it_is_audited,
);

async fn sent(r: Value) -> String {
    assert_eq!(r["ok"], true, "{r}");
    r["msg_id"].as_str().unwrap().to_string()
}

/// A device of a user that syncs and acknowledges everything.
async fn synced(hub: &Hub, u: &User, device: &str) {
    let c = Conn::device(hub, u, device);
    let s = c.ok(json!({"op": "sync"})).await;
    let acks: Vec<Value> = s["convs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| json!({"conv_id": c["conv_id"], "seq": c["upto"]}))
        .collect();
    c.ok(json!({"op": "ack", "acks": acks})).await;
}

async fn delivery_is_counted_per_device(db: TestDb) {
    let Some(secret) = new_db(Backend::Sqlite).await else {
        return;
    };
    // The conversation lives in the second database.
    let hub = open(two(&db.url, &secret.url, "secret")).await;
    let alice = user_in(&hub, "alice", "b").await;
    let bob = user_in(&hub, "bob", "b").await;
    let (_, d1) = hub
        .issue_token(&bob.id, Some("phone"), Some("ios"), None, "t")
        .await
        .unwrap();
    let (_, d2) = hub
        .issue_token(&bob.id, Some("laptop"), Some("web"), None, "t")
        .await
        .unwrap();
    let a = Conn::new(&hub, &alice);
    let conv = a.group(&[&bob]).await;
    let id = sent(
        a.call(json!({"op": "send", "conv_id": conv, "text": "the secret plan", "client_msg_id": "c-1"}))
            .await,
    )
    .await;
    // Not delivered to any device yet.
    let t = hub.message_trace(&id).await.unwrap();
    assert_eq!(t["message"]["database"], "secret");
    assert_eq!(t["message"]["client_msg_id"], "c-1");
    assert_eq!(t["message"]["sender"]["name"], "alice");
    assert_eq!(t["delivery"]["devices"], 2, "{t}");
    assert_eq!(t["delivery"]["delivered"], 0);
    // The phone syncs; the laptop stays offline.
    synced(&hub, &bob, &d1.device_id).await;
    let t = hub.message_trace(&id).await.unwrap();
    assert_eq!(t["delivery"]["delivered"], 1, "{t}");
    assert_eq!(t["delivery"]["pending"], 1);
    let p = &t["delivery"]["pending_devices"][0];
    assert_eq!(p["device_id"], d2.device_id.as_str());
    assert_eq!(p["platform"], "web");
    assert_eq!(p["user"], "bob");
    assert!(p["acked_seq"].is_null(), "never synced: {p}");
    // A device that synced before the message is pending with its position.
    synced(&hub, &bob, &d2.device_id).await;
    let id2 = sent(a.send(&conv, "later").await).await;
    synced(&hub, &bob, &d1.device_id).await;
    let t = hub.message_trace(&id2).await.unwrap();
    let p = &t["delivery"]["pending_devices"][0];
    assert_eq!(p["device_id"], d2.device_id.as_str());
    assert_eq!(
        p["acked_seq"].as_i64().unwrap(),
        t["message"]["seq"].as_i64().unwrap() - 1
    );
    // Everything delivered.
    synced(&hub, &bob, &d2.device_id).await;
    let t = hub.message_trace(&id2).await.unwrap();
    assert_eq!(
        (
            t["delivery"]["delivered"].as_i64(),
            t["delivery"]["pending"].as_i64()
        ),
        (Some(2), Some(0))
    );
    // A logged-out device that had synced still counts (by its position).
    hub.revoke_tokens(&bob.id, Some(&d1.device_id))
        .await
        .unwrap();
    let t = hub.message_trace(&id2).await.unwrap();
    assert!(t["delivery"]["devices"].as_i64().unwrap() >= 1);
    // Metadata only: the content never appears.
    assert!(!t.to_string().contains("later"));
    assert!(
        !hub.message_trace(&id)
            .await
            .unwrap()
            .to_string()
            .contains("secret plan")
    );
    // The trace id is there; with a UI template, a link to it.
    let tid = t["trace"]["trace_id"].as_str().unwrap();
    assert_eq!(tid.len(), 32);
    assert!(t["trace"]["url"].is_null());
    // Unknown ids.
    for bad in ["m_nope", "", &"x".repeat(200)] {
        assert_eq!(
            hub.message_trace(bad).await.unwrap_err().code,
            "not_found",
            "{bad}"
        );
    }
}

async fn who_could_see_it_follows_the_members(db: TestDb) {
    let mut cfg = single(&db.url);
    cfg.tracing.trace_ui_url = Some("https://jaeger.example.com/trace/{trace_id}".into());
    let hub = open(cfg).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let carol = user(&hub, "carol", UserKind::Human, None).await;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    let a = Conn::new(&hub, &alice);
    let conv = a.group(&[&bob, &bot]).await;
    let id = sent(a.send(&conv, "hello").await).await;
    let t = hub.message_trace(&id).await.unwrap();
    assert_eq!(t["audience"]["people"], 2, "{t}");
    assert_eq!(t["audience"]["agents"], 1);
    assert_eq!(t["agents"][0]["agent"], "bot");
    assert_eq!(t["agents"][0]["stopped"], false);
    let tid = t["trace"]["trace_id"].as_str().unwrap();
    assert_eq!(
        t["trace"]["url"],
        format!("https://jaeger.example.com/trace/{tid}")
    );
    // Someone added later sees it (the group shares its history); someone
    // removed later did see it.
    a.ok(json!({"op": "add_member", "conv_id": conv, "user_id": carol.id}))
        .await;
    a.ok(json!({"op": "remove_member", "conv_id": conv, "user_id": bob.id}))
        .await;
    let t = hub.message_trace(&id).await.unwrap();
    assert_eq!(t["audience"]["people"], 3, "{t}");
    assert_eq!(t["audience"]["removed_since"], 1);
    // A stopped agent shows as stopped.
    a.ok(json!({"op": "stop_agent", "agent_id": bot.id})).await;
    let t = hub.message_trace(&id).await.unwrap();
    assert_eq!(t["agents"][0]["stopped"], true, "{t}");
    // A whisper anchored at it is counted, not shown.
    let id2 = sent(a.send(&conv, "two").await).await;
    a.ok(json!({"op": "send", "conv_id": conv, "text": "psst", "client_msg_id": "w", "to_user": carol.id}))
        .await;
    let t = hub.message_trace(&id2).await.unwrap();
    assert_eq!(t["audience"]["side_events"], 1, "{t}");
    assert!(!t.to_string().contains("psst"));
}

async fn replies_drafts_edits_recall_and_reports(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    let helper = user(&hub, "helper", UserKind::Agent, Some(&alice)).await;
    let a = Conn::new(&hub, &alice);
    let conv = a.group(&[&bob, &bot, &helper]).await;
    // The helper's messages wait for approval.
    a.ok(
        json!({"op": "set_agent_policy", "conv_id": conv, "agent_id": helper.id, "approval": true}),
    )
    .await;
    let id = sent(a.send(&conv, "question").await).await;
    let g = Conn::new(&hub, &bot);
    let reply = sent(
        g.call(json!({"op": "send", "conv_id": conv, "text": "answer", "client_msg_id": "r", "causation_id": id}))
            .await,
    )
    .await;
    let h = Conn::new(&hub, &helper);
    let d = h
        .call(json!({"op": "send", "conv_id": conv, "text": "draft", "client_msg_id": "d", "causation_id": id}))
        .await;
    assert!(d["draft_id"].is_string(), "{d}");
    let t = hub.message_trace(&id).await.unwrap();
    assert_eq!(t["replies"][0]["msg_id"], reply.as_str(), "{t}");
    assert_eq!(t["replies"][0]["sender"], "bot");
    assert_eq!(t["drafts"][0]["agent"], "helper");
    assert_eq!(t["drafts"][0]["state"], "pending");
    let bot_row = t["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["agent"] == "bot")
        .unwrap();
    assert_eq!(bot_row["replies"], json!([reply]));
    // The reply's own trace points back.
    let rt = hub.message_trace(&reply).await.unwrap();
    assert_eq!(rt["message"]["causation_id"], id.as_str());
    // A report, an edit, a recall.
    let b = Conn::new(&hub, &bob);
    b.ok(json!({"op": "report", "target_kind": "message", "target_id": id, "category": "spam"}))
        .await;
    a.ok(json!({"op": "edit", "msg_id": id, "text": "question (edited)"}))
        .await;
    a.ok(json!({"op": "recall", "msg_id": id})).await;
    let t = hub.message_trace(&id).await.unwrap();
    let kinds: Vec<&str> = t["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["type"].as_str().unwrap())
        .collect();
    assert_eq!(kinds.first(), Some(&"msg.created"), "{t}");
    assert!(
        kinds.contains(&"msg.updated") && kinds.last() == Some(&"msg.recalled"),
        "{kinds:?}"
    );
    assert_eq!(
        t["events"].as_array().unwrap().last().unwrap()["actor"],
        "alice"
    );
    assert_eq!(t["message"]["state"], "recalled");
    assert_eq!(t["reports"][0]["category"], "spam");
    assert_eq!(t["reports"][0]["state"], "open");
    assert!(!t.to_string().contains("question"));
}

/// A receiver: `ok` answers 204, `fail` 500, `hang` never.
async fn receiver() -> String {
    let app = Router::new()
        .route("/ok", post(|| async { axum::http::StatusCode::NO_CONTENT }))
        .route(
            "/fail",
            post(|| async { axum::http::StatusCode::INTERNAL_SERVER_ERROR }),
        )
        .route(
            "/hang",
            post(|| async {
                tokio::time::sleep(Duration::from_secs(300)).await;
                axum::http::StatusCode::NO_CONTENT
            }),
        );
    let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = tcp.local_addr().unwrap().to_string();
    tokio::spawn(async move { axum::serve(tcp, app).await.unwrap() });
    addr
}

fn hook(name: &str, addr: &str, events: &str) -> WebhookConfig {
    WebhookConfig {
        name: name.into(),
        url: format!("http://{addr}/{name}"),
        secret: "0123456789abcdef0123".into(),
        events: vec![events.into()],
        max_attempts: 2,
        timeout_ms: 300_000,
        backoff_ms: 10,
    }
}

/// The webhook states of a message's events, once none is `except`.
async fn states(hub: &Hub, id: &str, until: impl Fn(&Value) -> bool) -> Value {
    for _ in 0..200 {
        let t = hub.message_trace(id).await.unwrap();
        if until(&t["webhooks"]) {
            return t["webhooks"].clone();
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("{}", hub.message_trace(id).await.unwrap());
}

fn state_of<'a>(w: &'a Value, hook: &str, kind: &str) -> &'a str {
    w.as_array()
        .unwrap()
        .iter()
        .find(|x| x["webhook"] == hook && x["type"] == kind)
        .map_or("absent", |x| x["state"].as_str().unwrap())
}

async fn webhook_states_while_queued(db: TestDb) {
    let addr = receiver().await;
    let mut cfg = single(&db.url);
    cfg.webhooks = vec![
        hook("ok", &addr, "msg.*"),
        hook("fail", &addr, "msg.created"),
        hook("hang", &addr, "msg.created"),
    ];
    let hub = open(cfg).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let a = Conn::new(&hub, &alice);
    let conv = a.group(&[&bob]).await;
    let first = sent(a.send(&conv, "one").await).await;
    let second = sent(a.send(&conv, "two").await).await;
    // `hang` holds the outbox rows: delivered, dropped and pending side by side.
    let w = states(&hub, &first, |w| {
        state_of(w, "ok", "msg.created") == "delivered"
            && state_of(w, "fail", "msg.created") == "dropped"
    })
    .await;
    assert_eq!(state_of(&w, "hang", "msg.created"), "pending", "{w}");
    let w2 = states(&hub, &second, |w| {
        state_of(w, "fail", "msg.created") == "dropped"
    })
    .await;
    let hang = w2
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["webhook"] == "hang")
        .unwrap();
    assert_eq!(hang["state"], "pending");
    assert_eq!(
        hang["queued_ahead"], 1,
        "the first message is before it: {w2}"
    );
    // A webhook that does not take the type is not listed.
    a.ok(json!({"op": "edit", "msg_id": first, "text": "one!"}))
        .await;
    let w = states(&hub, &first, |w| {
        state_of(w, "ok", "msg.updated") == "delivered"
    })
    .await;
    assert_eq!(state_of(&w, "fail", "msg.updated"), "absent", "{w}");
    // The drop is in the audit log with its conversation and seq.
    let audit = hub
        .admin_audit(Some("webhook.dropped"), None, 50)
        .await
        .unwrap();
    let r = &audit["records"][0];
    assert_eq!(r["conv_id"], conv.as_str(), "{audit}");
    assert_eq!(r["detail"]["webhook"], "fail");
    assert!(r["detail"]["seq"].as_i64().unwrap() > 0);
}

async fn webhook_states_after_the_outbox_is_cleaned(db: TestDb) {
    let addr = receiver().await;
    let mut cfg = single(&db.url);
    cfg.webhooks = vec![
        hook("ok", &addr, "msg.*"),
        hook("fail", &addr, "msg.created"),
    ];
    let hub = open(cfg).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let a = Conn::new(&hub, &alice);
    let conv = a.group(&[&bob]).await;
    let id = sent(a.send(&conv, "one").await).await;
    // Both webhooks are past it, so the row is gone: the states come from
    // the audit log.
    let w = states(&hub, &id, |w| {
        state_of(w, "fail", "msg.created") == "dropped"
            && state_of(w, "ok", "msg.created") == "delivered"
    })
    .await;
    assert_eq!(w.as_array().unwrap().len(), 2, "{w}");
    // Recalled after delivery: the created event may have been withdrawn.
    a.ok(json!({"op": "recall", "msg_id": id})).await;
    let w = states(&hub, &id, |w| {
        state_of(w, "ok", "msg.recalled") == "delivered"
    })
    .await;
    assert_eq!(
        state_of(&w, "ok", "msg.created"),
        "delivered_or_withdrawn",
        "{w}"
    );
    assert_eq!(state_of(&w, "fail", "msg.created"), "dropped");
}

async fn who_may_read_a_trace_and_it_is_audited(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let a = Conn::new(&hub, &alice);
    let conv = a.group(&[&bob]).await;
    let id = sent(a.send(&conv, "hi").await).await;
    let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", tcp.local_addr().unwrap());
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
    let get = |key: String, id: String| {
        let base = base.clone();
        async move {
            let res = reqwest::Client::new()
                .get(format!("{base}/admin/v1/messages/{id}/trace"))
                .bearer_auth(key)
                .send()
                .await
                .unwrap();
            let s = res.status().as_u16();
            (
                s,
                serde_json::from_str::<Value>(&res.text().await.unwrap()).unwrap_or(Value::Null),
            )
        }
    };
    let audited = || {
        let hub = hub.clone();
        async move {
            hub.admin_audit(Some("admin.messages.trace"), None, 50)
                .await
                .unwrap()["records"]
                .as_array()
                .unwrap()
                .len()
        }
    };
    let (owner, _) = hub.create_admin_key("o", "owner").await.unwrap();
    let (op, _) = hub.create_admin_key("ops", "operator").await.unwrap();
    let (md, _) = hub.create_admin_key("mod", "moderator").await.unwrap();
    let (dev, _) = hub.create_admin_key("dev", "developer").await.unwrap();
    // Refused: the developer; an unknown message. No audit record.
    let (s, v) = get(dev.clone(), id.clone()).await;
    assert_eq!((s, v["code"].as_str()), (403, Some("forbidden")), "{v}");
    let (s, v) = get(op.clone(), "m_nope".into()).await;
    assert_eq!((s, v["code"].as_str()), (404, Some("not_found")), "{v}");
    let (s, _) = get("aim_admin_wrong".into(), id.clone()).await;
    assert_eq!(s, 401);
    assert_eq!(audited().await, 0);
    // Allowed: each one audited, with the conversation.
    for (n, key) in [owner, op, md].into_iter().enumerate() {
        let (s, v) = get(key, id.clone()).await;
        assert_eq!(s, 200, "{v}");
        assert_eq!(v["message"]["msg_id"], id.as_str());
        assert_eq!(audited().await, n + 1);
    }
    let r = &hub
        .admin_audit(Some("admin.messages.trace"), None, 1)
        .await
        .unwrap()["records"][0];
    assert_eq!(r["target"], id.as_str());
    assert_eq!(r["detail"]["conv_id"], conv.as_str());
}
