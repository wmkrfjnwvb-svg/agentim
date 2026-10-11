//! Client tokens and devices (docs/07-open-api.md §5, 04 §1): only hashes
//! are stored, one token per device, listing and logging out devices,
//! expiry, revocation with immediate disconnect, persistence across restarts,
//! the new-device notice and read sync between devices.

mod common;

use std::time::Duration;

use agentim_core::config::sqlite_url;
use agentim_core::hub::Hub;
use agentim_core::model::*;
use agentim_core::server::{AppState, router};
use common::*;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use sqlx::Connection;
use tokio_tungstenite::tungstenite::Message;

backend_tests!(
    tokens_are_stored_only_as_hashes,
    each_login_is_a_device,
    issuing_tokens_checks_its_input,
    expired_and_revoked_tokens_stop_working,
    tokens_survive_a_restart_and_revocations_too,
    reads_sync_to_the_users_other_devices,
);

fn code(r: &Value) -> &str {
    r["error"]["code"].as_str().unwrap_or("")
}

async fn raw(db: &TestDb) -> (sqlx::AnyConnection, agentim_core::storage::Dialect) {
    sqlx::any::install_default_drivers();
    let d = agentim_core::storage::Dialect::from_url(&db.url).unwrap();
    (
        sqlx::AnyConnection::connect(&db.url.replace("mariadb://", "mysql://"))
            .await
            .unwrap(),
        d,
    )
}

async fn tokens_are_stored_only_as_hashes(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let (alice, t1) = hub
        .create_user("alice", UserKind::Human, None, None)
        .await
        .unwrap();
    let (t2, _) = hub
        .issue_token(&alice.id, Some("phone"), Some("ios"), None, "test")
        .await
        .unwrap();
    drop(hub);
    let (mut c, _) = raw(&db).await;
    let rows = sqlx::query("SELECT hash FROM tokens")
        .fetch_all(&mut c)
        .await
        .unwrap();
    let hashes: Vec<String> = rows.iter().map(|r| sqlx::Row::get(r, 0)).collect();
    assert_eq!(hashes.len(), 2);
    for t in [&t1, &t2] {
        assert!(t.starts_with("atk_") && t.len() == 47, "{t}");
        assert!(hashes.contains(&agentim_core::auth::hash(t)));
    }
    // Neither token appears anywhere in the database.
    for table in ["users", "tokens", "audit", "meta"] {
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!("SELECT * FROM {table}")))
            .fetch_all(&mut c)
            .await
            .unwrap();
        for r in rows {
            for i in 0..sqlx::Row::len(&r) {
                if let Ok(Some(v)) = sqlx::Row::try_get::<Option<String>, _>(&r, i) {
                    assert!(
                        !v.contains(&t1) && !v.contains(&t2),
                        "{table} holds a token"
                    );
                }
            }
        }
    }
}

async fn each_login_is_a_device(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let (alice, t1) = hub
        .create_user("alice", UserKind::Human, None, None)
        .await
        .unwrap();
    let (bob, _) = hub
        .create_user("bob", UserKind::Human, None, None)
        .await
        .unwrap();
    let (u, d1) = hub.authenticate_device(&t1).unwrap();
    assert_eq!(u.id, alice.id);
    let (s1, p1) = hub.connect_device(alice.clone(), Some(d1.clone()));
    let mut c1 = Conn { s: s1, rx: p1 };
    // A second login: a new device, announced to the first.
    let (t2, row) = hub
        .issue_token(&alice.id, Some("laptop"), Some("windows"), None, "test")
        .await
        .unwrap();
    let notice: Vec<Value> = c1.drain();
    assert_eq!(notice.len(), 1, "{notice:?}");
    assert_eq!(
        (&notice[0]["type"], &notice[0]["device"]["device_id"]),
        (&json!("device.added"), &json!(row.device_id))
    );
    assert!(
        !notice[0].to_string().contains("hash"),
        "no secrets in the notice"
    );
    let (_, d2) = hub.authenticate_device(&t2).unwrap();
    assert_ne!(d1, d2);
    let list = c1.ok(json!({"op": "list_devices"})).await;
    let devs = list["devices"].as_array().unwrap();
    assert_eq!(devs.len(), 2);
    let cur: Vec<bool> = devs
        .iter()
        .map(|d| d["current"].as_bool().unwrap())
        .collect();
    assert_eq!(cur, [true, false]);
    assert_eq!(devs[1]["name"], "laptop");
    assert!(!list.to_string().contains("hash") && !list.to_string().contains(&t2));
    // Logging out a device that is not yours, or does not exist.
    let (_, bob_dev) = hub
        .issue_token(&bob.id, None, None, None, "test")
        .await
        .unwrap();
    for d in [bob_dev.device_id.as_str(), "d_nope", ""] {
        let r = c1
            .call(json!({"op": "logout_device", "device_id": d}))
            .await;
        assert_eq!(code(&r), "not_found", "{d}");
    }
    assert_eq!(
        hub.devices(&bob.id).await.unwrap().len(),
        2,
        "bob's device untouched"
    );
    // Logging out the laptop: its token stops working; this device keeps going.
    let r = c1.ok(json!({"op": "logout_device", "device_id": d2})).await;
    assert_eq!(r["revoked"], 1);
    assert!(hub.authenticate(&t2).is_none());
    assert!(hub.authenticate(&t1).is_some());
    assert_eq!(
        c1.ok(json!({"op": "list_devices"})).await["devices"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

async fn issuing_tokens_checks_its_input(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let (alice, _) = hub
        .create_user("alice", UserKind::Human, None, None)
        .await
        .unwrap();
    let long = "x".repeat(65);
    let ok64 = "x".repeat(64);
    for (name, platform, ttl, want) in [
        (Some(""), None, None, "bad_request"),
        (Some(long.as_str()), None, None, "bad_request"),
        (None, Some(long.as_str()), None, "bad_request"),
        (Some("a\0b"), None, None, "bad_request"),
        (None, None, Some(0), "bad_request"),
        (None, None, Some(-5), "bad_request"),
    ] {
        let e = hub
            .issue_token(&alice.id, name, platform, ttl, "test")
            .await
            .unwrap_err();
        assert_eq!(e.code, want, "{name:?} {platform:?} {ttl:?}");
    }
    assert!(
        hub.issue_token(&alice.id, Some(&ok64), Some(&ok64), Some(1), "test")
            .await
            .is_ok()
    );
    assert_eq!(
        hub.issue_token("u_nobody", None, None, None, "test")
            .await
            .unwrap_err()
            .code,
        "not_found"
    );
    // Refusals created no devices.
    assert_eq!(hub.devices(&alice.id).await.unwrap().len(), 2);
}

async fn expired_and_revoked_tokens_stop_working(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let (alice, t1) = hub
        .create_user("alice", UserKind::Human, None, None)
        .await
        .unwrap();
    let (short, _) = hub
        .issue_token(&alice.id, None, None, Some(1000), "test")
        .await
        .unwrap();
    // Long enough for a slow CI machine to check it before it expires.
    assert!(hub.authenticate(&short).is_some());
    tokio::time::sleep(Duration::from_millis(1100)).await;
    assert!(hub.authenticate(&short).is_none(), "expired");
    assert!(hub.authenticate("atk_not_a_token").is_none());
    assert!(hub.authenticate("").is_none());
    // Revoking everything; a second revoke finds nothing.
    assert_eq!(hub.revoke_tokens(&alice.id, None).await.unwrap(), 2);
    assert!(hub.authenticate(&t1).is_none());
    assert_eq!(hub.revoke_tokens(&alice.id, None).await.unwrap(), 0);
    assert!(hub.devices(&alice.id).await.unwrap().is_empty());
    // A new login works again.
    let (t3, _) = hub
        .issue_token(&alice.id, None, None, None, "test")
        .await
        .unwrap();
    assert!(hub.authenticate(&t3).is_some());
}

async fn tokens_survive_a_restart_and_revocations_too(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let (alice, t1) = hub
        .create_user("alice", UserKind::Human, None, None)
        .await
        .unwrap();
    let (t2, row) = hub
        .issue_token(&alice.id, Some("tab"), None, Some(3_600_000), "test")
        .await
        .unwrap();
    let (t3, gone) = hub
        .issue_token(&alice.id, None, None, None, "test")
        .await
        .unwrap();
    hub.revoke_tokens(&alice.id, Some(&gone.device_id))
        .await
        .unwrap();
    drop(hub);
    let hub = open(single(&db.url)).await;
    assert_eq!(hub.authenticate_device(&t2).unwrap().1, row.device_id);
    assert!(hub.authenticate(&t1).is_some());
    assert!(hub.authenticate(&t3).is_none(), "still revoked");
    let devs = hub.devices(&alice.id).await.unwrap();
    assert_eq!(devs.len(), 2);
    assert_eq!(devs[1].expires_at.unwrap() - devs[1].created_at, 3_600_000);
}

async fn reads_sync_to_the_users_other_devices(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let mut a1 = Conn::new(&hub, &alice);
    let mut a2 = Conn::new(&hub, &alice);
    let mut b = Conn::new(&hub, &bob);
    let conv = b.group(&[&alice]).await;
    let s = b.send(&conv, "hi").await["seq"].as_i64().unwrap();
    for c in [&mut a1, &mut a2, &mut b] {
        c.drain();
    }
    a1.ok(json!({"op": "read", "conv_id": conv, "seq": s}))
        .await;
    assert_eq!(
        a2.drain(),
        [json!({"type": "read", "conv_id": conv, "seq": s})]
    );
    assert!(a1.drain().is_empty(), "not echoed to the reader");
    assert!(b.drain().is_empty(), "other users are not told");
    // Clamped to the head, and the synced value is the clamped one.
    a1.ok(json!({"op": "read", "conv_id": conv, "seq": s + 100}))
        .await;
    assert_eq!(a2.drain()[0]["seq"], s);
    // Refused reads sync nothing.
    let eve = user(&hub, "eve", UserKind::Human, None).await;
    let mut e2 = Conn::new(&hub, &eve);
    let e1 = Conn::new(&hub, &eve);
    assert_eq!(
        code(
            &e1.call(json!({"op": "read", "conv_id": conv, "seq": 1}))
                .await
        ),
        "forbidden"
    );
    assert!(e2.drain().is_empty());
}

// ---- over the WebSocket ---------------------------------------------------

async fn start() -> (String, Hub, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let hub = open(single(&sqlite_url(
        &dir.path().join("db").to_string_lossy(),
    )))
    .await;
    let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = tcp.local_addr().unwrap().to_string();
    let state = AppState {
        hub: hub.clone(),
        demo_login: false,
        metrics_token: None,
    };
    tokio::spawn(async move { axum::serve(tcp, router(state)).await.unwrap() });
    (addr, hub, dir)
}

async fn closed_with(
    ws: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) -> Option<String> {
    let r = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match ws.next().await {
                Some(Ok(Message::Close(f))) => return f.map(|f| f.reason.to_string()),
                Some(Ok(_)) => {}
                _ => return None,
            }
        }
    })
    .await;
    r.expect("closed promptly")
}

#[tokio::test]
async fn logging_out_a_device_disconnects_it_at_once() {
    let (addr, hub, _dir) = start().await;
    let (alice, t1) = hub
        .create_user("alice", UserKind::Human, None, None)
        .await
        .unwrap();
    let (t2, row) = hub
        .issue_token(&alice.id, Some("laptop"), None, None, "test")
        .await
        .unwrap();
    let connect = |t: String| {
        let addr = addr.clone();
        async move {
            let (ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws?token={t}"))
                .await
                .unwrap();
            ws
        }
    };
    let mut w1 = connect(t1.clone()).await;
    let mut w2 = connect(t2.clone()).await;
    let mut w2b = connect(t2.clone()).await; // same device, two tabs
    w1.next().await;
    w2.next().await;
    w2b.next().await;
    w1.send(Message::Text(
        json!({"op": "logout_device", "rid": 1, "device_id": row.device_id})
            .to_string()
            .into(),
    ))
    .await
    .unwrap();
    assert_eq!(closed_with(&mut w2).await.as_deref(), Some("logged_out"));
    assert_eq!(closed_with(&mut w2b).await.as_deref(), Some("logged_out"));
    // w1 is fine and got its answer.
    let r: Value = loop {
        let m = w1.next().await.unwrap().unwrap();
        let v: Value = serde_json::from_str(m.to_text().unwrap()).unwrap();
        if v["rid"] == 1 {
            break v;
        }
    };
    assert_eq!(r["ok"], true, "{r}");
    // The revoked token cannot reconnect.
    assert!(
        tokio_tungstenite::connect_async(format!("ws://{addr}/ws?token={t2}"))
            .await
            .is_err()
    );
    // Revoking everything (as the open API does) closes the last one too.
    hub.revoke_tokens(&alice.id, None).await.unwrap();
    assert_eq!(closed_with(&mut w1).await.as_deref(), Some("logged_out"));
}
