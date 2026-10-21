//! Logging in (docs/13 §4.2): people connect with client tokens their
//! developer signs (JWT, HS256), agents with keys. Signing keys, refusals
//! of every malformed or revoked token, first-login creation, devices
//! chosen by the app, logging out one device or all, restarts, and races
//! between revocation and connecting. Devices: listing, the new-device
//! notice, read sync.

mod common;

use std::time::Duration;

use agentim_core::auth::{jwt_sign, looks_like_jwt};
use agentim_core::config::sqlite_url;
use agentim_core::hub::Hub;
use agentim_core::hub::login::Login;
use agentim_core::model::*;
use agentim_core::server::{AppState, router};
use common::*;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use sqlx::Connection;
use tokio_tungstenite::tungstenite::Message;

backend_tests!(
    signing_keys_are_kept_encrypted_and_listed_without_secrets,
    a_signed_token_logs_in_and_registers_the_device,
    malformed_and_forged_tokens_are_refused_without_trace,
    first_login_creates_the_person_once,
    refused_subjects,
    agents_use_keys_and_people_use_signed_tokens,
    revoking_a_signing_key,
    logging_out_one_device_or_all,
    devices_and_cutoffs_survive_a_restart,
    a_device_unused_too_long_is_forgotten,
    reads_sync_to_the_users_other_devices,
    race_revocations_and_connections,
    race_first_logins_of_one_new_person,
);

fn code(r: &Value) -> &str {
    r["error"]["code"].as_str().unwrap_or("")
}

fn now_s() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

async fn raw(db: &TestDb) -> sqlx::AnyConnection {
    sqlx::any::install_default_drivers();
    sqlx::AnyConnection::connect(&db.url.replace("mariadb://", "mysql://"))
        .await
        .unwrap()
}

/// Every string stored in the control database's tables.
async fn all_text(db: &TestDb, tables: &[&str]) -> String {
    let mut c = raw(db).await;
    let mut out = String::new();
    for t in tables {
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!("SELECT * FROM {t}")))
            .fetch_all(&mut c)
            .await
            .unwrap();
        for r in rows {
            for i in 0..sqlx::Row::len(&r) {
                if let Ok(Some(v)) = sqlx::Row::try_get::<Option<String>, _>(&r, i) {
                    out.push_str(&v);
                    out.push('\n');
                }
            }
        }
    }
    out
}

async fn key(hub: &Hub) -> (String, String) {
    let v = hub.create_signing_key("backend").await.unwrap();
    (
        v["kid"].as_str().unwrap().to_string(),
        v["secret"].as_str().unwrap().to_string(),
    )
}

async fn login(hub: &Hub, token: &str, device: &str) -> Result<Login, ApiError> {
    hub.authenticate_client(token, Some(device), Some("phone"), Some("ios"))
        .await
}

async fn signing_keys_are_kept_encrypted_and_listed_without_secrets(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let (kid, secret) = key(&hub).await;
    assert!(kid.starts_with("sk_"));
    let (kid2, secret2) = key(&hub).await;
    assert_ne!(secret, secret2);
    let list = hub.signing_keys().await.unwrap();
    assert_eq!(list.len(), 2);
    let text = json!(list).to_string();
    assert!(text.contains(&kid) && text.contains(&kid2));
    assert!(!text.contains(&secret) && !text.contains(&secret2));
    // Refused names create nothing.
    for bad in ["", &"x".repeat(65), "a\0b"] {
        assert_eq!(
            hub.create_signing_key(bad).await.unwrap_err().code,
            "bad_request"
        );
    }
    assert_eq!(hub.signing_keys().await.unwrap().len(), 2);
    drop(hub);
    // Encryption at rest is on in tests: the secrets are not in the
    // database, only wrapped by the master key.
    let stored = all_text(&db, &["signing_keys", "meta", "audit"]).await;
    assert!(!stored.contains(&secret) && !stored.contains(&secret2));
    assert!(stored.contains("k:"));
    // After a restart the keys still verify.
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let t = jwt_sign(&kid, &secret, &alice.name, now_s(), now_s() + 60);
    assert_eq!(login(&hub, &t, "p1").await.unwrap().user.id, alice.id);
    // Without encryption a secret is stored as it is.
    let plain = new_db(Backend::Sqlite).await.unwrap();
    let mut cfg = single(&plain.url);
    cfg.encryption = Default::default();
    cfg.encryption.mode = agentim_core::config::EncryptionMode::Off;
    let hub = open(cfg).await;
    let (_, s) = key(&hub).await;
    drop(hub);
    assert!(all_text(&plain, &["signing_keys"]).await.contains(&s));
}

async fn a_signed_token_logs_in_and_registers_the_device(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let (kid, secret) = key(&hub).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let t = jwt_sign(&kid, &secret, "alice", now_s(), now_s() + 3600);
    assert!(looks_like_jwt(&t));
    let l = login(&hub, &t, "iphone-1").await.unwrap();
    assert_eq!(l.user.id, alice.id);
    assert!(l.device_id.starts_with("d_"));
    assert!(hub.login_valid(&l));
    // The same app device again is the same device; another is new and
    // announced to the first.
    let mut c1 = Conn::device(&hub, &alice, &l.device_id);
    let again = login(&hub, &t, "iphone-1").await.unwrap();
    assert_eq!(again.device_id, l.device_id);
    assert!(c1.drain().is_empty(), "no notice for a known device");
    let l2 = hub
        .authenticate_client(&t, Some("laptop"), Some("laptop"), Some("windows"))
        .await
        .unwrap();
    assert_ne!(l2.device_id, l.device_id);
    let notice = c1.drain();
    assert_eq!(notice.len(), 1, "{notice:?}");
    assert_eq!(notice[0]["type"], "device.added");
    assert_eq!(notice[0]["device"]["device_id"], l2.device_id.as_str());
    assert!(!notice[0].to_string().contains(&t));
    // Another person choosing the same app device id gets their own device.
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let tb = jwt_sign(&kid, &secret, "bob", now_s(), now_s() + 60);
    let lb = login(&hub, &tb, "iphone-1").await.unwrap();
    assert_eq!(lb.user.id, bob.id);
    assert_ne!(lb.device_id, l.device_id);
    let list = c1.ok(json!({"op": "list_devices"})).await;
    let devs = list["devices"].as_array().unwrap();
    assert_eq!(devs.len(), 2, "{list}");
    assert_eq!(devs[1]["name"], "laptop");
    assert_eq!(devs[1]["platform"], "windows");
    assert!(!list.to_string().contains("hash"));
    // Device names, platforms and ids are checked; nothing is registered.
    let long = "x".repeat(65);
    for (dev, name, plat) in [
        (None, None, None),
        (Some(""), None, None),
        (Some(long.as_str()), None, None),
        (Some("a b"), None, None),
        (Some("a/b"), None, None),
        (Some("ok"), Some(""), None),
        (Some("ok"), Some(long.as_str()), None),
        (Some("ok"), None, Some("a\0b")),
    ] {
        let e = hub
            .authenticate_client(&t, dev, name, plat)
            .await
            .unwrap_err();
        assert_eq!(e.code, "bad_request", "{dev:?} {name:?} {plat:?}");
    }
    let ok64 = "x".repeat(64);
    assert!(
        hub.authenticate_client(&t, Some(&ok64), Some(&ok64), Some(&ok64))
            .await
            .is_ok()
    );
    assert_eq!(hub.devices(&alice.id).await.unwrap().len(), 3);
    // Device rows never hold the token.
    drop((c1, hub));
    let stored = all_text(&db, &["tokens", "users", "audit", "meta"]).await;
    assert!(!stored.contains(&t));
}

async fn malformed_and_forged_tokens_are_refused_without_trace(db: TestDb) {
    let mut cfg = single(&db.url);
    cfg.auth.max_token_days = 7;
    let hub = open(cfg).await;
    let (kid, secret) = key(&hub).await;
    user(&hub, "alice", UserKind::Human, None).await;
    let now = now_s();
    let day = 86400;
    let bodies = [
        // (sub, iat, exp)
        ("alice", now - 100, now - 1),     // expired
        ("alice", now + 120, now + 1000),  // issued in the future
        ("alice", now, now + 7 * day + 1), // longer than the most
        ("alice", now, now),               // empty lifetime
        ("", now, now + 60),               // empty subject
        ("_reserved", now, now + 60),      // reserved id
        ("a b", now, now + 60),            // not an id
        (&*"x".repeat(65), now, now + 60), // too long
    ];
    let mut tokens: Vec<String> = bodies
        .iter()
        .map(|(s, i, e)| jwt_sign(&kid, &secret, s, *i, *e))
        .collect();
    tokens.push(jwt_sign("sk_unknown", &secret, "alice", now, now + 60));
    tokens.push(jwt_sign(&kid, "wrong secret", "alice", now, now + 60));
    let b64 = |s: &str| {
        use base64::Engine;
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(s)
    };
    let body = b64(&json!({"sub": "alice", "iat": now, "exp": now + 60}).to_string());
    tokens.push(format!("{}.{body}.", b64(r#"{"alg":"none","kid":"x"}"#)));
    tokens.push(format!(
        "{}.{body}.c2ln",
        b64(&format!(r#"{{"alg":"RS256","kid":"{kid}"}}"#))
    ));
    tokens.push("not.a.jwt".into());
    tokens.push("atk_made_up".into());
    tokens.push(String::new());
    // A token for a new person, refused: nobody is created either.
    tokens.push(jwt_sign(&kid, "wrong secret", "mallory", now, now + 60));
    let before = hub.audit_log(1000).await.unwrap().len();
    for t in &tokens {
        let e = login(&hub, t, "dev1").await.unwrap_err();
        assert_eq!(e.code, "unauthorized", "{t}: {}", e.message);
    }
    assert!(hub.user_by_name("mallory").is_none());
    let alice = hub.user_by_name("alice").unwrap();
    assert!(
        hub.devices(&alice.id).await.unwrap().is_empty(),
        "no device"
    );
    assert_eq!(hub.audit_log(1000).await.unwrap().len(), before);
    // The boundaries themselves are accepted.
    for (i, e) in [
        (now + 60, now + 600),
        (now, now + 7 * day),
        (now - 10, now + 30),
    ] {
        let t = jwt_sign(&kid, &secret, "alice", i, e);
        assert!(login(&hub, &t, "dev1").await.is_ok(), "{i} {e}");
    }
}

async fn first_login_creates_the_person_once(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let (kid, secret) = key(&hub).await;
    let t = jwt_sign(&kid, &secret, "dana@example", now_s(), now_s() + 60);
    let l = login(&hub, &t, "dev1").await.unwrap();
    let dana = hub.user_by_name("dana@example").unwrap();
    assert_eq!(l.user.id, dana.id);
    assert_eq!(dana.kind, UserKind::Human);
    let l2 = login(&hub, &t, "dev2").await.unwrap();
    assert_eq!(l2.user.id, dana.id, "the same person");
    assert_eq!(
        hub.users()
            .iter()
            .filter(|u| u.name == "dana@example")
            .count(),
        1
    );
    // They are a person like any other: reachable and able to chat.
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let b = Conn::new(&hub, &bob);
    let conv = b.group(&[&dana]).await;
    let d = Conn::device(&hub, &dana, &l.device_id);
    assert_eq!(d.send(&conv, "hi").await["ok"], true);
    // And survive a restart.
    drop((b, d, hub));
    let hub = open(single(&db.url)).await;
    assert!(hub.user_by_name("dana@example").is_some());
}

async fn refused_subjects(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let (kid, secret) = key(&hub).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    let gone = user(&hub, "gone", UserKind::Human, None).await;
    let off = user(&hub, "off", UserKind::Human, None).await;
    hub.delete_account(&gone, None).await.unwrap();
    hub.erasures_idle().await;
    hub.set_deactivated(&off.id, true).await.unwrap();
    let sign = |s: &str| jwt_sign(&kid, &secret, s, now_s(), now_s() + 60);
    for sub in [
        bot.name.as_str(), // agents use keys
        "system",          // the system user
        "gone",            // deleted: ids are not reused
        "off",             // deactivated
        alice.id.as_str(), // an internal id is not a user id
    ] {
        let e = login(&hub, &sign(sub), "dev1").await.unwrap_err();
        assert_eq!(e.code, "unauthorized", "{sub}");
    }
    assert!(
        hub.devices(&bot.id).await.unwrap().len() == 1,
        "only its key"
    );
    assert!(hub.devices(&off.id).await.unwrap().is_empty());
    // Reactivated: logs in again.
    hub.set_deactivated(&off.id, false).await.unwrap();
    assert!(login(&hub, &sign("off"), "dev1").await.is_ok());
}

async fn agents_use_keys_and_people_use_signed_tokens(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let (bot, key) = hub
        .create_user("bot", UserKind::Agent, Some(&alice.id), None)
        .await
        .unwrap();
    let key = key.unwrap();
    assert!(key.starts_with("atk_"));
    let l = hub
        .authenticate_client(&key, None, None, None)
        .await
        .unwrap();
    assert_eq!(l.user.id, bot.id);
    assert!(hub.login_valid(&l));
    // People have no key: create_user gives them none, and a person's
    // key-shaped credential (made up, or from before) is refused.
    let (bob, none) = hub
        .create_user("bob", UserKind::Human, None, None)
        .await
        .unwrap();
    assert!(none.is_none());
    assert!(hub.devices(&bob.id).await.unwrap().is_empty());
    let (t, _) = hub
        .issue_token(&bob.id, None, None, None, "test")
        .await
        .unwrap();
    let e = hub
        .authenticate_client(&t, None, None, None)
        .await
        .unwrap_err();
    assert_eq!(e.code, "unauthorized");
    // Revoking the agent's key ends its login.
    hub.revoke_tokens(&bot.id, None).await.unwrap();
    assert!(!hub.login_valid(&l));
    assert!(
        hub.authenticate_client(&key, None, None, None)
            .await
            .is_err()
    );
}

async fn revoking_a_signing_key(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let (k1, s1) = key(&hub).await;
    let (k2, s2) = key(&hub).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let t1 = jwt_sign(&k1, &s1, "alice", now_s(), now_s() + 60);
    let t2 = jwt_sign(&k2, &s2, "alice", now_s(), now_s() + 60);
    let l1 = login(&hub, &t1, "p1").await.unwrap();
    let l2 = login(&hub, &t2, "p2").await.unwrap();
    let c1 = Conn::device(&hub, &alice, &l1.device_id);
    hub.revoke_signing_key(&k1).await.unwrap();
    // Its tokens stop working at once; the other key's do not.
    assert!(!hub.login_valid(&l1));
    assert!(hub.login_valid(&l2));
    assert_eq!(
        login(&hub, &t1, "p1").await.unwrap_err().code,
        "unauthorized"
    );
    assert!(login(&hub, &t2, "p2").await.is_ok());
    assert!(c1.s.is_kicked(), "the device that used it is disconnected");
    // The device itself (its sync position) stays: a token signed with
    // the new key logs it in again.
    let t1b = jwt_sign(&k2, &s2, "alice", now_s(), now_s() + 60);
    assert_eq!(
        login(&hub, &t1b, "p1").await.unwrap().device_id,
        l1.device_id
    );
    // Revoking again, or an unknown key: not_found.
    for k in [k1.as_str(), "sk_nope"] {
        assert_eq!(
            hub.revoke_signing_key(k).await.unwrap_err().code,
            "not_found"
        );
    }
    let list = hub.signing_keys().await.unwrap();
    assert!(
        list.iter()
            .any(|k| k["kid"] == k1.as_str() && !k["revoked_at"].is_null())
    );
    // Signing locally uses a key in force.
    let t = hub.sign_token("alice", 60).unwrap();
    assert!(login(&hub, &t, "p3").await.is_ok());
    hub.revoke_signing_key(&k2).await.unwrap();
    assert_eq!(hub.sign_token("alice", 60).unwrap_err().code, "conflict");
    // A key created by another process (the CLI) is found at once (the
    // keys are read again for an unknown key id, at most once a second).
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let cli = open(single(&db.url)).await;
    let v = cli.create_signing_key("from the cli").await.unwrap();
    let t4 = jwt_sign(
        v["kid"].as_str().unwrap(),
        v["secret"].as_str().unwrap(),
        "alice",
        now_s(),
        now_s() + 60,
    );
    assert!(login(&hub, &t4, "p5").await.is_ok());
    // A server that was running notices a revocation made by another
    // process (the CLI) within its refresh.
    let (k3, s3) = key(&hub).await;
    let t3 = jwt_sign(&k3, &s3, "alice", now_s(), now_s() + 60);
    let l3 = login(&hub, &t3, "p4").await.unwrap();
    let c3 = Conn::device(&hub, &alice, &l3.device_id);
    cli.revoke_signing_key(&k3).await.unwrap();
    assert!(hub.login_valid(&l3), "not yet noticed");
    assert!(hub.refresh_auth().await.unwrap());
    assert!(!hub.login_valid(&l3));
    assert!(c3.s.is_kicked());
    assert_eq!(
        login(&hub, &t3, "p4").await.unwrap_err().code,
        "unauthorized"
    );
}

async fn logging_out_one_device_or_all(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let (kid, secret) = key(&hub).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let old = jwt_sign(&kid, &secret, "alice", now_s() - 5, now_s() + 600);
    let phone = login(&hub, &old, "phone").await.unwrap();
    let laptop = login(&hub, &old, "laptop").await.unwrap();
    let lb = login(
        &hub,
        &jwt_sign(&kid, &secret, "bob", now_s(), now_s() + 60),
        "x",
    )
    .await
    .unwrap();
    let c = Conn::device(&hub, &alice, &laptop.device_id);
    let cp = Conn::device(&hub, &alice, &phone.device_id);
    // Not yours, unknown, empty: not_found, nothing changes.
    for d in [lb.device_id.as_str(), "d_nope", ""] {
        let r = c.call(json!({"op": "logout_device", "device_id": d})).await;
        assert_eq!(code(&r), "not_found", "{d}");
    }
    assert_eq!(hub.devices(&bob.id).await.unwrap().len(), 1);
    // Logging out the phone from the laptop.
    let r = c
        .ok(json!({"op": "logout_device", "device_id": phone.device_id}))
        .await;
    assert_eq!(r["revoked"], 1);
    assert!(cp.s.is_kicked());
    assert!(!hub.login_valid(&phone));
    assert!(hub.login_valid(&laptop), "the laptop keeps going");
    // The token the phone still holds does not log it back in…
    assert_eq!(
        login(&hub, &old, "phone").await.unwrap_err().code,
        "unauthorized"
    );
    // …and is still good on the laptop (only the phone was logged out).
    assert!(login(&hub, &old, "laptop").await.is_ok());
    // A token issued from the logout on works again: a new device.
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let fresh = jwt_sign(&kid, &secret, "alice", now_s(), now_s() + 600);
    let again = login(&hub, &fresh, "phone").await.unwrap();
    assert_eq!(again.device_id, phone.device_id, "same app device id");
    // Logging out everywhere (the open API's DELETE /tokens).
    let n = hub.log_out(&alice.id, None).await.unwrap();
    assert_eq!(n, 2);
    assert!(c.s.is_kicked());
    for (t, d) in [(&old, "laptop"), (&fresh, "phone"), (&fresh, "new-device")] {
        assert_eq!(
            login(&hub, t, d).await.unwrap_err().code,
            "unauthorized",
            "{d}"
        );
    }
    assert!(hub.devices(&alice.id).await.unwrap().is_empty());
    assert!(hub.login_valid(&lb), "bob is not affected");
    // A second logout finds nothing.
    assert_eq!(hub.log_out(&alice.id, None).await.unwrap(), 0);
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let next = jwt_sign(&kid, &secret, "alice", now_s(), now_s() + 600);
    assert!(login(&hub, &next, "laptop").await.is_ok());
}

async fn devices_and_cutoffs_survive_a_restart(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let (kid, secret) = key(&hub).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let t = jwt_sign(&kid, &secret, "alice", now_s() - 5, now_s() + 600);
    let keep = login(&hub, &t, "keep").await.unwrap();
    let gone = login(&hub, &t, "gone").await.unwrap();
    hub.log_out(&alice.id, Some(&gone.device_id)).await.unwrap();
    drop(hub);
    let hub = open(single(&db.url)).await;
    assert!(hub.login_valid(&keep));
    assert_eq!(
        login(&hub, &t, "keep").await.unwrap().device_id,
        keep.device_id
    );
    assert_eq!(
        login(&hub, &t, "gone").await.unwrap_err().code,
        "unauthorized"
    );
    let devs = hub.devices(&alice.id).await.unwrap();
    assert_eq!(devs.len(), 1);
    assert_eq!(devs[0].device_name.as_deref(), Some("phone"));
}

async fn a_device_unused_too_long_is_forgotten(db: TestDb) {
    let mut cfg = single(&db.url);
    cfg.auth.token_idle_days = 1;
    let hub = open(cfg).await;
    let (kid, secret) = key(&hub).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let t = jwt_sign(&kid, &secret, "alice", now_s(), now_s() + 600);
    let l = login(&hub, &t, "old").await.unwrap();
    let mut c = raw(&db).await;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "UPDATE tokens SET last_seen = 1, created_at = 1 WHERE device_id = '{}'",
        l.device_id
    )))
    .execute(&mut c)
    .await
    .unwrap();
    assert_eq!(hub.expire_tokens().await.unwrap(), 1);
    assert!(!hub.login_valid(&l));
    assert!(hub.devices(&alice.id).await.unwrap().is_empty());
    // The token itself is still good: the next connection is a new device.
    let again = login(&hub, &t, "old").await.unwrap();
    assert!(hub.login_valid(&again));
    assert_eq!(hub.expire_tokens().await.unwrap(), 0);
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
    a1.ok(json!({"op": "read", "conv_id": conv, "seq": s + 100}))
        .await;
    assert_eq!(a2.drain()[0]["seq"], s);
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

/// Logging out (one device, everywhere) and revoking the key race people
/// connecting: whatever the order, once a revocation returned no session
/// of a revoked token is left open and none opens afterwards.
async fn race_revocations_and_connections(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    for round in 0..8 {
        let (kid, secret) = key(&hub).await;
        let t = jwt_sign(&kid, &secret, "alice", now_s(), now_s() + 600);
        let first = login(&hub, &t, "d0").await.unwrap();
        let mut tasks = vec![];
        for i in 0..12 {
            let (h, t) = (hub.clone(), t.clone());
            tasks.push(tokio::spawn(async move {
                let l = login(&h, &t, &format!("d{}", i % 3)).await.ok()?;
                let (s, _p, refused) = h.open_device_session(&l);
                Some((l, s, refused))
            }));
        }
        match round % 3 {
            0 => hub.log_out(&alice.id, None).await.map(|_| ()).unwrap(),
            1 => hub.revoke_signing_key(&kid).await.unwrap(),
            _ => hub
                .log_out(&alice.id, Some(&first.device_id))
                .await
                .map(|_| ())
                .unwrap(),
        }
        let mut open_after = 0;
        for task in tasks {
            if let Some((l, s, refused)) = task.await.unwrap() {
                let revoked = round % 3 != 2 || l.device_id == first.device_id;
                if revoked && refused.is_none() && !s.is_kicked() && hub.login_valid(&l) {
                    open_after += 1;
                }
            }
        }
        assert_eq!(open_after, 0, "round {round}");
        // Nothing logs in with the revoked token afterwards.
        let after = login(&hub, &t, "d0").await;
        assert!(after.is_err(), "round {round}");
        tokio::time::sleep(Duration::from_millis(1100)).await;
    }
}

/// Many first connections of one new person at once create one person.
async fn race_first_logins_of_one_new_person(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let (kid, secret) = key(&hub).await;
    for round in 0..5 {
        let name = format!("new{round}");
        let t = jwt_sign(&kid, &secret, &name, now_s(), now_s() + 60);
        let tasks: Vec<_> = (0..10)
            .map(|i| {
                let (h, t) = (hub.clone(), t.clone());
                tokio::spawn(async move { login(&h, &t, &format!("d{i}")).await })
            })
            .collect();
        let mut ids = std::collections::HashSet::new();
        for t in tasks {
            ids.insert(t.await.unwrap().unwrap().user.id);
        }
        assert_eq!(ids.len(), 1, "round {round}");
        assert_eq!(hub.users().iter().filter(|u| u.name == name).count(), 1);
        let id = ids.into_iter().next().unwrap();
        assert_eq!(hub.devices(&id).await.unwrap().len(), 10);
    }
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

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn closed_with(ws: &mut Ws) -> Option<String> {
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

async fn status(url: &str, auth: Option<&str>) -> u16 {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let mut req = url.into_client_request().unwrap();
    if let Some(a) = auth {
        req.headers_mut()
            .insert("authorization", format!("Bearer {a}").parse().unwrap());
    }
    match tokio_tungstenite::connect_async(req).await {
        Ok(_) => 101,
        Err(tokio_tungstenite::tungstenite::Error::Http(r)) => r.status().as_u16(),
        Err(e) => panic!("{e}"),
    }
}

#[tokio::test]
async fn over_the_websocket() {
    let (addr, hub, _dir) = start().await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let t = client_token(&hub, &alice).await;
    let url = |q: &str| format!("ws://{addr}/ws?{q}");
    // The welcome names the user and the server's id of the device.
    let (mut w, _) = tokio_tungstenite::connect_async(url(&format!(
        "token={t}&device=web-1&device_name=Chrome&platform=web"
    )))
    .await
    .unwrap();
    let m = w.next().await.unwrap().unwrap();
    let welcome: Value = serde_json::from_str(m.to_text().unwrap()).unwrap();
    assert_eq!(welcome["type"], "welcome");
    assert_eq!(welcome["me"]["name"], "alice");
    let dev = welcome["device_id"].as_str().unwrap().to_string();
    assert_eq!(hub.devices(&alice.id).await.unwrap()[0].device_id, dev);
    // Native clients may send the token as a header.
    assert_eq!(status(&url("device=native-1"), Some(&t)).await, 101);
    // Refusals: no token, a bad one, no device for a person.
    assert_eq!(status(&url("device=x"), None).await, 401);
    assert_eq!(status(&url("token=a.b.c&device=x"), None).await, 401);
    assert_eq!(status(&url(&format!("token={t}")), None).await, 400);
    assert_eq!(
        status(&url(&format!("token={t}&device=a%20b")), None).await,
        400
    );
    // An agent connects with its key and no device.
    let (_, k) = hub
        .create_user("bot", UserKind::Agent, Some(&alice.id), None)
        .await
        .unwrap();
    assert_eq!(
        status(&url(&format!("token={}", k.unwrap())), None).await,
        101
    );
    // Logging out the device from elsewhere closes it at once; the same
    // token cannot reconnect it.
    hub.log_out(&alice.id, Some(&dev)).await.unwrap();
    assert_eq!(closed_with(&mut w).await.as_deref(), Some("logged_out"));
    assert_eq!(
        status(&url(&format!("token={t}&device=web-1")), None).await,
        401
    );
    // Two tabs of one device, then logging out everywhere.
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let t2 = client_token(&hub, &alice).await;
    let mut tabs = vec![];
    for _ in 0..2 {
        let (mut ws, _) =
            tokio_tungstenite::connect_async(url(&format!("token={t2}&device=web-2")))
                .await
                .unwrap();
        ws.next().await;
        tabs.push(ws);
    }
    tabs[0]
        .send(Message::Text(
            json!({"op": "list_devices", "rid": 1}).to_string().into(),
        ))
        .await
        .unwrap();
    hub.log_out(&alice.id, None).await.unwrap();
    for ws in &mut tabs {
        assert_eq!(closed_with(ws).await.as_deref(), Some("logged_out"));
    }
}
