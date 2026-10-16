//! Built-in accounts (docs/04-security-compliance.md §1): passwords,
//! lockout, two-factor login, registration, password changes, the HTTP
//! endpoints, restarts and races. Run against every backend.

mod common;

use std::sync::Arc;
use std::time::Duration;

use agentim_core::auth;
use agentim_core::config::Config;
use agentim_core::hub::Hub;
use agentim_core::model::*;
use common::*;
use serde_json::json;

backend_tests!(
    password_login_issues_a_device,
    changing_a_password,
    failed_logins_lock_the_account,
    two_factor_login,
    registration,
    race_failed_logins_are_all_counted,
    race_one_code_logs_in_once,
    two_factor_over_the_client_protocol,
    race_two_enables_with_one_setup,
    race_password_hashing_is_bounded,
);

const PW: &str = "correct horse battery";

async fn hub_with(db: &TestDb, f: impl FnOnce(&mut Config)) -> Hub {
    let mut c = single(&db.url);
    f(&mut c);
    open(c).await
}

async fn person(hub: &Hub, name: &str) -> User {
    let (u, _) = hub
        .create_user(name, UserKind::Human, None, None)
        .await
        .unwrap();
    hub.reset_password(&u, PW).await.unwrap();
    u
}

fn code(e: &ApiError) -> &str {
    e.code
}

async fn login(hub: &Hub, name: &str, pw: &str, totp: Option<&str>) -> Result<String, ApiError> {
    hub.login(name, pw, totp, Some("test"), None)
        .await
        .map(|(t, _, _)| t)
}

/// Waits so a test does not straddle a TOTP step boundary.
async fn fresh_step() -> i64 {
    let now = now_ms() / 1000;
    if now % auth::TOTP_STEP > auth::TOTP_STEP - 6 {
        tokio::time::sleep(Duration::from_secs(
            (auth::TOTP_STEP - now % auth::TOTP_STEP + 1) as u64,
        ))
        .await;
    }
    now_ms() / 1000 / auth::TOTP_STEP
}

async fn password_login_issues_a_device(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let (bob, _) = hub
        .create_user("bob", UserKind::Human, None, None)
        .await
        .unwrap();
    let alice = person(&hub, "alice").await;
    let bot = hub
        .create_user("bot", UserKind::Agent, Some(&alice.id), None)
        .await
        .unwrap()
        .0;
    // The reset logged out the first device.
    assert!(hub.devices(&alice.id).await.unwrap().is_empty());
    let (t, dev, u) = hub
        .login("alice", PW, None, Some("laptop"), Some("windows"))
        .await
        .unwrap();
    assert_eq!(u.id, alice.id);
    assert_eq!(hub.authenticate_device(&t).unwrap().1, dev.device_id);
    assert_eq!(dev.device_name.as_deref(), Some("laptop"));
    // Wrong password, unknown name, no password set, an agent: one answer.
    for (name, pw) in [
        ("alice", "wrong password!"),
        ("nobody", PW),
        ("bob", PW),
        ("bot", PW),
        ("_system", PW),
        ("alice", ""),
    ] {
        let e = login(&hub, name, pw, None).await.unwrap_err();
        assert_eq!(
            (code(&e), e.message.as_str()),
            ("invalid_credentials", "wrong name, password or code"),
            "{name}"
        );
    }
    assert!(
        hub.reset_password(&bot, PW).await.is_err(),
        "agents have no password"
    );
    assert_eq!(
        hub.devices(&bob.id).await.unwrap().len(),
        1,
        "failed logins create no device"
    );
    // Deactivated people cannot log in even with the right password.
    hub.set_deactivated(&alice.id, true).await.unwrap();
    assert_eq!(
        code(&login(&hub, "alice", PW, None).await.unwrap_err()),
        "forbidden"
    );
    hub.set_deactivated(&alice.id, false).await.unwrap();
    // Passwords survive a restart; no plaintext is stored.
    drop(hub);
    let hub = open(single(&db.url)).await;
    assert!(login(&hub, "alice", PW, None).await.is_ok());
    let audit = hub.audit_log(1000).await.unwrap();
    assert!(!format!("{audit:?}").contains(PW));
}

async fn changing_a_password(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = person(&hub, "alice").await;
    let t1 = login(&hub, "alice", PW, None).await.unwrap();
    let t2 = login(&hub, "alice", PW, None).await.unwrap();
    let (u, d1) = hub.authenticate_device(&t1).unwrap();
    let c = Conn::device(&hub, &u, &d1);
    let set = |current: Option<&str>, new: &str| json!({"op": "set_password", "current": current, "new": new});
    for (cur, new, want) in [
        (None, "brand new password", "forbidden"),
        (Some("wrong one"), "brand new password", "forbidden"),
        (Some(PW), "short", "bad_request"),
        (Some(PW), "ALICE", "bad_request"),
        (Some(PW), &"x".repeat(129) as &str, "bad_request"),
    ] {
        let r = c.call(set(cur, new)).await;
        assert_eq!(r["error"]["code"], want, "{cur:?} {new}");
    }
    assert!(hub.authenticate(&t2).is_some(), "refusals change nothing");
    c.ok(set(Some(PW), "brand new password")).await;
    // This device stays, the others are logged out; the old password is gone.
    assert!(hub.authenticate(&t1).is_some());
    assert!(hub.authenticate(&t2).is_none());
    assert_eq!(
        code(&login(&hub, "alice", PW, None).await.unwrap_err()),
        "invalid_credentials"
    );
    assert!(
        login(&hub, "alice", "brand new password", None)
            .await
            .is_ok()
    );
    // A person without a password sets the first one without `current`.
    let (bob, tb) = hub
        .create_user("bob", UserKind::Human, None, None)
        .await
        .unwrap();
    let (u, d) = hub.authenticate_device(&tb).unwrap();
    Conn::device(&hub, &u, &d)
        .ok(set(None, "bobs first password"))
        .await;
    assert!(
        login(&hub, "bob", "bobs first password", None)
            .await
            .is_ok()
    );
    let _ = (alice, bob);
}

async fn failed_logins_lock_the_account(db: TestDb) {
    let hub = hub_with(&db, |c| {
        c.auth.max_failures = 3;
        c.auth.lock_minutes = 1;
    })
    .await;
    person(&hub, "alice").await;
    person(&hub, "bob").await;
    // Successes reset the count.
    for _ in 0..2 {
        login(&hub, "alice", "bad password", None)
            .await
            .unwrap_err();
    }
    login(&hub, "alice", PW, None).await.unwrap();
    for _ in 0..2 {
        login(&hub, "alice", "bad password", None)
            .await
            .unwrap_err();
    }
    assert!(
        login(&hub, "alice", PW, None).await.is_ok(),
        "not locked yet"
    );
    for _ in 0..3 {
        login(&hub, "alice", "bad password", None)
            .await
            .unwrap_err();
    }
    let e = login(&hub, "alice", PW, None).await.unwrap_err();
    assert_eq!(
        code(&e),
        "rate_limited",
        "locked, even with the right password"
    );
    assert!(e.message.contains("retry in"));
    assert!(
        login(&hub, "bob", PW, None).await.is_ok(),
        "other accounts are not affected"
    );
    // The lock survives a restart.
    drop(hub);
    let hub = open(single(&db.url)).await;
    assert_eq!(
        code(&login(&hub, "alice", PW, None).await.unwrap_err()),
        "rate_limited"
    );
}

async fn two_factor_login(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = person(&hub, "alice").await;
    assert_eq!(
        hub.totp_enable(&alice, "123456").await.unwrap_err().code,
        "conflict",
        "no setup yet"
    );
    assert_eq!(
        hub.totp_disable(&alice, "123456").await.unwrap_err().code,
        "conflict",
        "not on"
    );
    let v = hub.totp_setup(&alice).await.unwrap();
    let secret = v["secret"].as_str().unwrap().to_string();
    assert!(v["uri"].as_str().unwrap().contains(&secret));
    // Pending: logging in does not need a code yet.
    assert!(login(&hub, "alice", PW, None).await.is_ok());
    let step = fresh_step().await;
    let at = |s: i64| auth::totp_at(&secret, s).unwrap();
    assert_eq!(
        hub.totp_enable(&alice, "000000").await.unwrap_err().code,
        "bad_request"
    );
    assert_eq!(
        hub.totp_enable(&alice, "abc").await.unwrap_err().code,
        "bad_request"
    );
    hub.totp_enable(&alice, &at(step - 1)).await.unwrap();
    assert_eq!(hub.totp_setup(&alice).await.unwrap_err().code, "conflict");
    // Now a code is needed; wrong and reused codes fail.
    assert_eq!(
        code(&login(&hub, "alice", PW, None).await.unwrap_err()),
        "totp_required"
    );
    assert_eq!(
        code(
            &login(&hub, "alice", "bad password", Some(&at(step)))
                .await
                .unwrap_err()
        ),
        "invalid_credentials"
    );
    assert_eq!(
        code(&login(&hub, "alice", PW, Some("000000")).await.unwrap_err()),
        "invalid_credentials"
    );
    assert_eq!(
        code(
            &login(&hub, "alice", PW, Some(&at(step - 1)))
                .await
                .unwrap_err()
        ),
        "invalid_credentials",
        "used by the setup"
    );
    assert_eq!(
        code(
            &login(&hub, "alice", PW, Some(&at(step + 3)))
                .await
                .unwrap_err()
        ),
        "invalid_credentials",
        "outside the window"
    );
    assert!(login(&hub, "alice", PW, Some(&at(step))).await.is_ok());
    assert_eq!(
        code(&login(&hub, "alice", PW, Some(&at(step))).await.unwrap_err()),
        "invalid_credentials",
        "single use"
    );
    // Turning it off needs a fresh code too.
    assert_eq!(
        hub.totp_disable(&alice, &at(step)).await.unwrap_err().code,
        "bad_request"
    );
    hub.totp_disable(&alice, &at(step + 1)).await.unwrap();
    assert!(login(&hub, "alice", PW, None).await.is_ok());
    // Over the client protocol.
    let t = login(&hub, "alice", PW, None).await.unwrap();
    let (u, d) = hub.authenticate_device(&t).unwrap();
    let c = Conn::device(&hub, &u, &d);
    let v = c.ok(json!({"op": "totp_setup"})).await;
    assert!(v["secret"].is_string());
    let r = c.call(json!({"op": "totp_enable", "code": "999999"})).await;
    assert_eq!(r["error"]["code"], "bad_request");
}

async fn registration(db: TestDb) {
    let hub = open(single(&db.url)).await;
    assert_eq!(
        hub.register("newbie", PW).await.unwrap_err().code,
        "forbidden",
        "closed by default"
    );
    drop(hub);
    let hub = hub_with(&db, |c| c.auth.open_registration = true).await;
    let u = hub.register("newbie", PW).await.unwrap();
    assert!(
        hub.devices(&u.id).await.unwrap().is_empty(),
        "no device until login"
    );
    assert!(login(&hub, "newbie", PW, None).await.is_ok());
    for (name, pw, want) in [
        ("newbie", PW, "conflict"),
        ("_admin", PW, "bad_request"),
        ("bad name", PW, "bad_request"),
        ("weak", "short", "bad_request"),
        ("samename1", "samename1", "bad_request"),
    ] {
        assert_eq!(
            hub.register(name, pw).await.unwrap_err().code,
            want,
            "{name}"
        );
    }
    assert!(
        hub.user_by_name("weak").is_none(),
        "refused registrations leave nothing"
    );
}

async fn race_failed_logins_are_all_counted(db: TestDb) {
    let hub = Arc::new(hub_with(&db, |c| c.auth.max_failures = 5).await);
    person(&hub, "alice").await;
    let mut tasks = vec![];
    for _ in 0..12 {
        let hub = hub.clone();
        tasks.push(tokio::spawn(async move {
            login(&hub, "alice", "bad password", None).await
        }));
    }
    for t in tasks {
        assert!(t.await.unwrap().is_err());
    }
    assert_eq!(
        code(&login(&hub, "alice", PW, None).await.unwrap_err()),
        "rate_limited"
    );
}

async fn race_one_code_logs_in_once(db: TestDb) {
    let hub = Arc::new(open(single(&db.url)).await);
    // One account per round, so rounds need no new time step.
    for round in 0..10 {
        let u = person(&hub, &format!("u{round}")).await;
        let step = fresh_step().await;
        let secret = hub.totp_setup(&u).await.unwrap()["secret"]
            .as_str()
            .unwrap()
            .to_string();
        hub.totp_enable(&u, &auth::totp_at(&secret, step - 1).unwrap())
            .await
            .unwrap();
        let code_now = auth::totp_at(&secret, step).unwrap();
        let mut tasks = vec![];
        for _ in 0..6 {
            let (hub, c, name) = (hub.clone(), code_now.clone(), u.name.clone());
            tasks.push(tokio::spawn(async move {
                login(&hub, &name, PW, Some(&c)).await
            }));
        }
        let mut ok = 0;
        for t in tasks {
            match t.await.unwrap() {
                Ok(_) => ok += 1,
                Err(e) => assert_eq!(e.code, "invalid_credentials"),
            }
        }
        assert_eq!(ok, 1, "round {round}: exactly one login per code");
        assert_eq!(hub.devices(&u.id).await.unwrap().len(), 1);
    }
}

async fn two_factor_over_the_client_protocol(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = person(&hub, "alice").await;
    let bot = hub
        .create_user("bot", UserKind::Agent, Some(&alice.id), None)
        .await
        .unwrap()
        .0;
    let a = Conn::new(&hub, &alice);
    let r = a.call(json!({"op": "totp_enable", "code": "123456"})).await;
    assert_eq!(r["error"]["code"], "conflict", "no setup yet");
    let v = a.ok(json!({"op": "totp_setup"})).await;
    let secret = v["secret"].as_str().unwrap().to_string();
    let step = fresh_step().await;
    let at = |s: i64| auth::totp_at(&secret, s).unwrap();
    a.ok(json!({"op": "totp_enable", "code": at(step - 1)}))
        .await;
    assert!(
        login(&hub, "alice", PW, None).await.is_err(),
        "a code is needed now"
    );
    let r = a
        .call(json!({"op": "totp_disable", "code": "000000"}))
        .await;
    assert_eq!(r["error"]["code"], "bad_request");
    a.ok(json!({"op": "totp_disable", "code": at(step)})).await;
    assert!(login(&hub, "alice", PW, None).await.is_ok(), "off again");
    // Agents have no passwords.
    let g = Conn::new(&hub, &bot);
    let r = g
        .call(json!({"op": "set_password", "new": "another long password"}))
        .await;
    assert_eq!(r["error"]["code"], "bad_request");
    assert_eq!(
        hub.reset_password(&bot, "another long password")
            .await
            .unwrap_err()
            .code,
        "bad_request"
    );
}

async fn race_two_enables_with_one_setup(db: TestDb) {
    let hub = open(single(&db.url)).await;
    for round in 0..10 {
        let u = person(&hub, &format!("r{round}")).await;
        let secret = hub.totp_setup(&u).await.unwrap()["secret"]
            .as_str()
            .unwrap()
            .to_string();
        let step = fresh_step().await;
        let mut tasks = vec![];
        for s in [step - 1, step] {
            let (hub, u, code) = (hub.clone(), u.clone(), auth::totp_at(&secret, s).unwrap());
            tasks.push(tokio::spawn(
                async move { hub.totp_enable(&u, &code).await },
            ));
        }
        let mut ok = 0;
        for t in tasks {
            match t.await.unwrap() {
                Ok(()) => ok += 1,
                Err(e) => assert_eq!(
                    e.code, "conflict",
                    "the setup was used by the other: {}",
                    e.message
                ),
            }
        }
        assert_eq!(ok, 1, "round {round}: exactly one enable wins");
        assert!(hub.totp_setup(&u).await.is_err(), "two-factor login is on");
    }
}

/// H2 (docs/research/im-issues.md): a flood of logins cannot run unbounded
/// password hashes; the excess is refused and leaves no failure count.
async fn race_password_hashing_is_bounded(db: TestDb) {
    let hub = hub_with(&db, |c| {
        c.auth.max_concurrent_hashes = 1;
        c.auth.max_failures = 40;
    })
    .await;
    person(&hub, "alice").await;
    let tries: Vec<_> = (0..40)
        .map(|i| {
            let hub = hub.clone();
            let name = if i % 2 == 0 { "alice" } else { "nobody" };
            tokio::spawn(async move { login(&hub, name, "bad password", None).await })
        })
        .collect();
    let mut refused = 0;
    for t in tries {
        match t.await.unwrap().unwrap_err().code {
            "rate_limited" => refused += 1,
            "invalid_credentials" => {}
            c => panic!("{c}"),
        }
    }
    assert!(refused > 0, "at most four callers per slot get in");
    // Refused tries were not counted: 40 failures would have locked alice.
    assert!(login(&hub, "alice", PW, None).await.is_ok());
}
