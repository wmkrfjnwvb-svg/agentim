//! The admin API `/admin/v1` (docs/12 §4): the one-time setup (once, from
//! this machine), console sessions (cookie + CSRF, same origin, expiry,
//! logout), password lockout and TOTP, admin keys, the role of every
//! route, what the app area changes (keys, people, agents, conversations
//! with reason-gated reading, reports, client defaults) through the same
//! business rules, the admin accounts (the last owner stays), audit of
//! every change and no trace of refusals, restarts, a second process on
//! the same databases, and races.

mod common;

use std::net::SocketAddr;

use agentim_core::hub::Hub;
use agentim_core::model::*;
use agentim_core::server::{AppState, router};
use common::*;
use serde_json::{Value, json};

backend_tests!(
    setup_once_and_only_from_this_machine,
    sessions_need_csrf_and_the_same_origin,
    logins_lock_and_need_the_second_factor,
    guesses_in_a_session_count_as_failed_logins,
    every_route_checks_the_role,
    admin_keys_and_sessions_survive_a_restart,
    the_app_area_follows_the_business_rules,
    reading_content_needs_a_reason_and_is_audited,
    client_defaults_apply_everywhere,
    admins_and_the_last_owner,
    another_process_sees_the_same,
    race_setup_and_owner_changes,
);

const PW: &str = "correct horse battery";

#[derive(Clone)]
enum Auth {
    None,
    Key(String),
    Session { cookie: String, csrf: String },
}

struct Admin {
    base: String,
    hub: Hub,
}

struct Resp {
    status: u16,
    body: Value,
    cookie: Option<String>,
}

impl Admin {
    async fn start(hub: &Hub) -> Admin {
        Admin {
            base: serve(hub, "127.0.0.1:0").await,
            hub: hub.clone(),
        }
    }

    async fn call(&self, auth: &Auth, method: &str, path: &str, body: Option<Value>) -> Resp {
        call_with(&self.base, auth, method, path, body, &[]).await
    }

    /// Sets the console up and returns the owner's session.
    async fn owner(&self) -> Auth {
        let t = self.hub.setup_token().await.unwrap().expect("no admin yet");
        let r = self
            .call(
                &Auth::None,
                "POST",
                "/admin/v1/setup",
                Some(json!({"token": t, "name": "root", "password": PW})),
            )
            .await;
        assert_eq!(r.status, 200, "{}", r.body);
        session_of(&r)
    }

    /// An admin key of a role, made by the owner.
    async fn key(&self, owner: &Auth, role: &str) -> Auth {
        let r = self
            .call(
                owner,
                "POST",
                "/admin/v1/deploy/admin-keys",
                Some(json!({"name": format!("{role} key"), "role": role})),
            )
            .await;
        assert_eq!(r.status, 201, "{}", r.body);
        Auth::Key(r.body["key"].as_str().unwrap().to_string())
    }

    async fn login(&self, name: &str, password: &str, totp: Option<&str>) -> Resp {
        let mut b = json!({"name": name, "password": password});
        if let Some(t) = totp {
            b["totp"] = json!(t);
        }
        self.call(&Auth::None, "POST", "/admin/v1/login", Some(b))
            .await
    }

    async fn audits(&self, prefix: &str) -> Vec<Value> {
        self.hub
            .audit_log(10_000)
            .await
            .unwrap()
            .into_iter()
            .filter(|a| a["action"].as_str().unwrap_or("").starts_with(prefix))
            .collect()
    }
}

async fn serve(hub: &Hub, addr: &str) -> String {
    let tcp = tokio::net::TcpListener::bind(addr).await.unwrap();
    let addr = tcp.local_addr().unwrap();
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
    format!("http://{addr}")
}

async fn call_with(
    base: &str,
    auth: &Auth,
    method: &str,
    path: &str,
    body: Option<Value>,
    headers: &[(&str, &str)],
) -> Resp {
    let mut r = reqwest::Client::new().request(method.parse().unwrap(), format!("{base}{path}"));
    match auth {
        Auth::None => {}
        Auth::Key(k) => r = r.header("authorization", format!("Bearer {k}")),
        Auth::Session { cookie, csrf } => {
            r = r
                .header("cookie", format!("agentim_admin={cookie}"))
                .header("x-csrf-token", csrf)
        }
    }
    for (k, v) in headers {
        r = r.header(*k, *v);
    }
    if let Some(b) = body {
        r = r
            .header("content-type", "application/json")
            .body(b.to_string());
    }
    let resp = r.send().await.unwrap();
    let status = resp.status().as_u16();
    let cookie = resp
        .headers()
        .get("set-cookie")
        .map(|v| v.to_str().unwrap().to_string());
    let t = resp.text().await.unwrap();
    Resp {
        status,
        body: serde_json::from_str(&t).unwrap_or(Value::String(t)),
        cookie,
    }
}

fn session_of(r: &Resp) -> Auth {
    let c = r.cookie.as_deref().expect("a session cookie");
    assert!(
        c.contains("HttpOnly") && c.contains("SameSite=Strict"),
        "{c}"
    );
    let token = c
        .strip_prefix("agentim_admin=")
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    Auth::Session {
        cookie: token,
        csrf: r.body["csrf"].as_str().unwrap().to_string(),
    }
}

fn no_csrf(a: &Auth) -> Auth {
    match a {
        Auth::Session { cookie, .. } => Auth::Session {
            cookie: cookie.clone(),
            csrf: "csrf_wrong".into(),
        },
        a => a.clone(),
    }
}

/// An address of this machine that is not loopback, if it has one.
fn lan_ip() -> Option<std::net::IpAddr> {
    let s = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("192.0.2.1:9").ok()?;
    let ip = s.local_addr().ok()?.ip();
    (!ip.is_loopback() && !ip.is_unspecified()).then_some(ip)
}

fn now_s() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

async fn sql(db: &TestDb, q: &str) {
    use sqlx::Connection;
    sqlx::any::install_default_drivers();
    let mut c = sqlx::AnyConnection::connect(&db.url.replace("mariadb://", "mysql://"))
        .await
        .unwrap();
    sqlx::raw_sql(sqlx::AssertSqlSafe(q.to_string()))
        .execute(&mut c)
        .await
        .unwrap();
}

// ------------------------------------------------------------- tests

async fn setup_once_and_only_from_this_machine(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let ad = Admin::start(&hub).await;
    // No link printed yet: nothing to set up with.
    let r = ad.call(&Auth::None, "GET", "/admin/v1/setup", None).await;
    assert_eq!(r.body["needed"], false);
    let t = hub.setup_token().await.unwrap().unwrap();
    let r = ad.call(&Auth::None, "GET", "/admin/v1/setup", None).await;
    assert_eq!(r.body["needed"], true);
    let setup =
        |token: &str, name: &str, pw: &str| json!({"token": token, "name": name, "password": pw});
    // Refusals keep the link usable and leave no trace.
    for (b, status, code) in [
        (setup("setup_wrong", "root", PW), 403, "forbidden"),
        (setup(&t, "root", "short"), 400, "bad_request"),
        (setup(&t, "root user", PW), 400, "bad_request"),
        (setup(&t, "", PW), 400, "bad_request"),
        (
            setup(&t, "rootrootroot", "ROOTROOTROOT"),
            400,
            "bad_request",
        ),
    ] {
        let r = ad
            .call(&Auth::None, "POST", "/admin/v1/setup", Some(b))
            .await;
        assert_eq!(
            (r.status, r.body["code"].as_str().unwrap()),
            (status, code),
            "{}",
            r.body
        );
    }
    let r = ad
        .call(
            &Auth::None,
            "POST",
            "/admin/v1/setup",
            Some(json!({"token": t})),
        )
        .await;
    assert_eq!(r.status, 400, "missing fields");
    // From another machine: refused even with the right link.
    if let Some(ip) = lan_ip() {
        let base = serve(&hub, &format!("{ip}:0")).await;
        let r = call_with(
            &base,
            &Auth::None,
            "POST",
            "/admin/v1/setup",
            Some(setup(&t, "root", PW)),
            &[],
        )
        .await;
        assert_eq!(
            (r.status, r.body["code"].as_str().unwrap()),
            (403, "forbidden"),
            "{}",
            r.body
        );
    }
    assert!(hub.admins().await.unwrap().is_empty());
    assert!(
        ad.audits("admin.").await.is_empty(),
        "refusals leave no trace"
    );
    // The link works once.
    let r = ad
        .call(
            &Auth::None,
            "POST",
            "/admin/v1/setup",
            Some(setup(&t, "root", PW)),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["admin"], json!({"name": "root", "role": "owner"}));
    let owner = session_of(&r);
    let r = ad.call(&owner, "GET", "/admin/v1/me", None).await;
    assert_eq!(r.body["admin"]["role"], "owner");
    let r = ad
        .call(
            &Auth::None,
            "POST",
            "/admin/v1/setup",
            Some(setup(&t, "root2", PW)),
        )
        .await;
    assert_eq!(
        (r.status, r.body["code"].as_str().unwrap()),
        (403, "forbidden")
    );
    assert_eq!(
        ad.call(&Auth::None, "GET", "/admin/v1/setup", None)
            .await
            .body["needed"],
        false
    );
    // Once there is an admin, no new link either.
    assert!(hub.setup_token().await.unwrap().is_none());
    assert_eq!(hub.admins().await.unwrap().len(), 1);
    assert_eq!(ad.audits("admin.setup").await.len(), 1);
    assert_eq!(ad.audits("admin.setup").await[0]["actor_id"], "admin:root");
}

async fn sessions_need_csrf_and_the_same_origin(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let ad = Admin::start(&hub).await;
    let owner = ad.owner().await;
    // Without credentials, with a made-up cookie or a non-admin bearer.
    for auth in [
        Auth::None,
        Auth::Session {
            cookie: "ads_made_up".into(),
            csrf: "x".into(),
        },
        Auth::Key("adk_made_up".into()),
        Auth::Key("atk_not_an_admin_key".into()),
    ] {
        let r = ad.call(&auth, "GET", "/admin/v1/app/overview", None).await;
        assert_eq!(
            (r.status, r.body["code"].as_str().unwrap()),
            (401, "unauthorized")
        );
    }
    // Unknown routes under the area answer 404, not 401.
    let r = ad.call(&Auth::None, "GET", "/admin/v1/nothing", None).await;
    assert_eq!(r.status, 404);
    // Reading needs no CSRF token, changing does.
    assert_eq!(
        ad.call(&no_csrf(&owner), "GET", "/admin/v1/app/overview", None)
            .await
            .status,
        200
    );
    let before = ad.audits("admin.").await.len();
    let r = ad
        .call(
            &no_csrf(&owner),
            "POST",
            "/admin/v1/app/signing-keys",
            Some(json!({"name": "web"})),
        )
        .await;
    assert_eq!(
        (r.status, r.body["code"].as_str().unwrap()),
        (403, "forbidden")
    );
    // A page on another site cannot use the session, even an origin the
    // client apps are allowed from.
    let base = &ad.base;
    for origin in ["https://evil.example", "http://127.0.0.1:1"] {
        let r = call_with(
            base,
            &owner,
            "POST",
            "/admin/v1/app/signing-keys",
            Some(json!({"name": "web"})),
            &[("origin", origin)],
        )
        .await;
        assert_eq!(
            (r.status, r.body["code"].as_str().unwrap()),
            (403, "forbidden"),
            "{origin}"
        );
        let r = call_with(
            base,
            &Auth::None,
            "POST",
            "/admin/v1/login",
            Some(json!({"name": "root", "password": PW})),
            &[("origin", origin)],
        )
        .await;
        assert_eq!(r.status, 403);
    }
    assert!(hub.signing_keys().await.unwrap().is_empty());
    assert_eq!(ad.audits("admin.").await.len(), before, "no trace");
    // The same origin is fine.
    let host = base.strip_prefix("http://").unwrap();
    let r = call_with(
        base,
        &owner,
        "POST",
        "/admin/v1/app/signing-keys",
        Some(json!({"name": "web"})),
        &[("origin", base)],
    )
    .await;
    assert_eq!(r.status, 201, "{} {host}", r.body);
    // Logout ends the session; the cookie is cleared.
    let r = ad.call(&owner, "POST", "/admin/v1/logout", None).await;
    assert_eq!(r.status, 200);
    assert!(r.cookie.unwrap().contains("Max-Age=0"));
    assert_eq!(
        ad.call(&owner, "GET", "/admin/v1/me", None).await.status,
        401
    );
    // A session unused for 12 hours or older than 7 days ends.
    let s1 = session_of(&ad.login("root", PW, None).await);
    let s2 = session_of(&ad.login("root", PW, None).await);
    let s3 = session_of(&ad.login("root", PW, None).await);
    assert_eq!(ad.call(&s1, "GET", "/admin/v1/me", None).await.status, 200);
    let hash = |a: &Auth| match a {
        Auth::Session { cookie, .. } => agentim_core::auth::hash(cookie),
        _ => unreachable!(),
    };
    let now = now_s() * 1000;
    sql(
        &db,
        &format!(
            "UPDATE admin_sessions SET last_seen = {} WHERE hash = '{}'",
            now - 13 * 3_600_000,
            hash(&s1)
        ),
    )
    .await;
    sql(
        &db,
        &format!(
            "UPDATE admin_sessions SET created_at = {} WHERE hash = '{}'",
            now - 8 * 86_400_000,
            hash(&s2)
        ),
    )
    .await;
    assert_eq!(ad.call(&s1, "GET", "/admin/v1/me", None).await.status, 401);
    // The ticker removes the other one without a request.
    assert_eq!(hub.expire_admin_sessions().await.unwrap(), 1);
    assert_eq!(ad.call(&s2, "GET", "/admin/v1/me", None).await.status, 401);
    assert_eq!(ad.call(&s3, "GET", "/admin/v1/me", None).await.status, 200);
    assert_eq!(hub.expire_admin_sessions().await.unwrap(), 0, "idempotent");
}

async fn logins_lock_and_need_the_second_factor(db: TestDb) {
    let mut cfg = single(&db.url);
    cfg.auth.max_failures = 3;
    let hub = open(cfg).await;
    let ad = Admin::start(&hub).await;
    let owner = ad.owner().await;
    // Unknown names and wrong passwords look the same.
    for (n, p) in [("nobody", PW), ("root", "wrong password!")] {
        let r = ad.login(n, p, None).await;
        assert_eq!(
            (r.status, r.body["code"].as_str().unwrap()),
            (401, "invalid_credentials")
        );
        assert!(r.cookie.is_none());
    }
    let r = ad.login(&"x".repeat(300), PW, None).await;
    assert_eq!(r.status, 400);
    // Two more failures lock root out, even with the right password.
    for _ in 0..2 {
        assert_eq!(ad.login("root", "wrong password!", None).await.status, 401);
    }
    let r = ad.login("root", PW, None).await;
    assert_eq!(
        (r.status, r.body["code"].as_str().unwrap()),
        (429, "rate_limited")
    );
    // A name that is no admin locks after as many failures: answers do
    // not tell names apart (B9).
    for _ in 0..2 {
        assert_eq!(ad.login("nobody", PW, None).await.status, 401);
    }
    let r = ad.login("nobody", PW, None).await;
    assert_eq!(
        (r.status, r.body["code"].as_str().unwrap()),
        (429, "rate_limited")
    );
    assert_eq!(
        ad.login("someone-else", PW, None).await.status,
        401,
        "per name"
    );
    // The existing session goes on; the lock is not in the audit log.
    assert_eq!(
        ad.call(&owner, "GET", "/admin/v1/me", None).await.status,
        200
    );
    assert_eq!(ad.audits("admin.login").await.len(), 0);
    // A second admin turns on TOTP.
    let r = ad
        .call(
            &owner,
            "POST",
            "/admin/v1/deploy/admins",
            Some(json!({"name": "dev", "role": "developer", "password": PW})),
        )
        .await;
    assert_eq!(r.status, 201, "{}", r.body);
    let dev = session_of(&ad.login("dev", PW, None).await);
    let r = ad
        .call(
            &dev,
            "POST",
            "/admin/v1/me/totp:enable",
            Some(json!({"code": "123456"})),
        )
        .await;
    assert_eq!(
        (r.status, r.body["code"].as_str().unwrap()),
        (409, "conflict"),
        "not set up yet"
    );
    let r = ad.call(&dev, "POST", "/admin/v1/me/totp:setup", None).await;
    assert_eq!(r.status, 200);
    let secret = r.body["secret"].as_str().unwrap().to_string();
    assert!(
        r.body["uri"]
            .as_str()
            .unwrap()
            .starts_with("otpauth://totp/")
    );
    let code = |s: i64| agentim_core::auth::totp_at(&secret, s).unwrap();
    // Three steps in a row are used below; start early in a 30 s step so
    // all of them stay within the accepted window (now ± 1).
    if now_s() % 30 > 20 {
        tokio::time::sleep(std::time::Duration::from_secs((31 - now_s() % 30) as u64)).await;
    }
    let step = now_s() / 30;
    for (b, status) in [
        (json!({}), 400),
        (json!({"code": "12345"}), 400),
        (
            json!({"code": if code(step) == "000000" { "111111" } else { "000000" }}),
            400,
        ),
    ] {
        assert_eq!(
            ad.call(&dev, "POST", "/admin/v1/me/totp:enable", Some(b))
                .await
                .status,
            status
        );
    }
    let r = ad
        .call(
            &dev,
            "POST",
            "/admin/v1/me/totp:enable",
            Some(json!({"code": code(step - 1)})),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(
        ad.call(&dev, "GET", "/admin/v1/me", None).await.body["totp"],
        true
    );
    let r = ad.call(&dev, "POST", "/admin/v1/me/totp:setup", None).await;
    assert_eq!(r.status, 409, "already on");
    // Now the password is not enough; a used code is refused.
    let r = ad.login("dev", PW, None).await;
    assert_eq!(
        (r.status, r.body["code"].as_str().unwrap()),
        (401, "totp_required")
    );
    assert_eq!(
        ad.login("dev", PW, Some(&code(step - 1))).await.status,
        401,
        "used once"
    );
    let r = ad.login("dev", PW, Some(&code(step))).await;
    assert_eq!(r.status, 200, "{}", r.body);
    let dev2 = session_of(&r);
    let r = ad
        .call(
            &dev2,
            "POST",
            "/admin/v1/me/totp:disable",
            Some(json!({"code": code(step)})),
        )
        .await;
    assert_eq!(r.status, 400, "used once");
    let r = ad
        .call(
            &dev2,
            "POST",
            "/admin/v1/me/totp:disable",
            Some(json!({"code": code(step + 1)})),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(ad.login("dev", PW, None).await.status, 200);
    let r = ad
        .call(&dev2, "POST", "/admin/v1/me/totp:bogus", None)
        .await;
    assert_eq!(r.status, 404);
    // Changing one's password needs the current one and ends every session.
    let r = ad
        .call(
            &dev2,
            "POST",
            "/admin/v1/me/password",
            Some(json!({"current": "nope nope nope", "new": "another long one"})),
        )
        .await;
    assert_eq!(
        (r.status, r.body["code"].as_str().unwrap()),
        (403, "forbidden")
    );
    let r = ad
        .call(
            &dev2,
            "POST",
            "/admin/v1/me/password",
            Some(json!({"current": PW, "new": "short"})),
        )
        .await;
    assert_eq!(r.status, 400);
    let r = ad
        .call(
            &dev2,
            "POST",
            "/admin/v1/me/password",
            Some(json!({"current": PW, "new": "another long one"})),
        )
        .await;
    assert_eq!(r.status, 200);
    assert_eq!(
        ad.call(&dev2, "GET", "/admin/v1/me", None).await.status,
        401
    );
    assert_eq!(ad.call(&dev, "GET", "/admin/v1/me", None).await.status, 401);
    assert_eq!(ad.login("dev", PW, None).await.status, 401);
    assert_eq!(ad.login("dev", "another long one", None).await.status, 200);
    // Keys have neither a password nor TOTP.
    let k = ad.key(&owner, "developer").await;
    assert_eq!(
        ad.call(&k, "POST", "/admin/v1/me/totp:setup", None)
            .await
            .status,
        400
    );
    assert_eq!(
        ad.call(
            &k,
            "POST",
            "/admin/v1/me/password",
            Some(json!({"current": PW, "new": "another long one"}))
        )
        .await
        .status,
        400
    );
    let acts: Vec<String> = ad
        .audits("admin.")
        .await
        .iter()
        .map(|a| a["action"].as_str().unwrap().to_string())
        .collect();
    for a in [
        "admin.totp.enable",
        "admin.totp.disable",
        "admin.password",
        "admin.login",
    ] {
        assert!(acts.iter().any(|x| x == a), "{a} in {acts:?}");
    }
}

async fn every_route_checks_the_role(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let ad = Admin::start(&hub).await;
    let owner = ad.owner().await;
    let roles = ["owner", "developer", "operator", "moderator"];
    let mut keys = vec![];
    for r in roles {
        keys.push((r, ad.key(&owner, r).await));
    }
    let before = ad.audits("admin.").await.len();
    // Reads in each area and who may make them.
    let app = ["owner", "developer"];
    let app_read = ["owner", "developer", "moderator"];
    let deploy = ["owner", "operator"];
    let owner_only = ["owner"];
    let cases: Vec<(&str, &str, &[&str])> = vec![
        ("GET", "/admin/v1/app/overview", &app_read),
        ("GET", "/admin/v1/app/users", &app_read),
        ("GET", "/admin/v1/app/conversations", &app_read),
        ("GET", "/admin/v1/app/reports", &app_read),
        ("GET", "/admin/v1/app/agents", &app),
        ("GET", "/admin/v1/app/webhooks", &app),
        ("GET", "/admin/v1/app/audit", &app),
        ("GET", "/admin/v1/app/settings/clients", &app),
        ("GET", "/admin/v1/app/api-keys", &app),
        ("GET", "/admin/v1/app/signing-keys", &app),
        ("GET", "/admin/v1/deploy/status", &deploy),
        ("GET", "/admin/v1/deploy/admins", &owner_only),
        ("GET", "/admin/v1/deploy/admin-keys", &owner_only),
    ];
    for (m, p, allowed) in &cases {
        for (role, k) in &keys {
            let r = ad.call(k, m, p, None).await;
            let want = if allowed.contains(role) { 200 } else { 403 };
            assert_eq!(r.status, want, "{role} {m} {p}: {}", r.body);
            if want == 403 {
                assert_eq!(r.body["code"], "forbidden");
            }
        }
    }
    // The cluster view answers bad_request on a single server (after the role).
    for (role, k) in &keys {
        let r = ad.call(k, "GET", "/admin/v1/deploy/cluster", None).await;
        let want = if deploy.contains(role) { 400 } else { 403 };
        assert_eq!(r.status, want, "{role}");
    }
    // Changes refused by role: nothing happens, nothing is logged.
    let changes: Vec<(&str, &str, Value, &[&str])> = vec![
        (
            "POST",
            "/admin/v1/app/signing-keys",
            json!({"name": "x"}),
            &app,
        ),
        (
            "POST",
            "/admin/v1/app/api-keys",
            json!({"name": "x", "scopes": ["users:read"]}),
            &app,
        ),
        (
            "PUT",
            "/admin/v1/app/settings/clients",
            json!({"create_groups": false}),
            &app,
        ),
        (
            "POST",
            "/admin/v1/deploy/admins",
            json!({"name": "x", "role": "operator", "password": PW}),
            &owner_only,
        ),
        (
            "POST",
            "/admin/v1/deploy/admin-keys",
            json!({"name": "x", "role": "owner"}),
            &owner_only,
        ),
    ];
    for (m, p, b, allowed) in &changes {
        for (role, k) in &keys {
            if allowed.contains(role) {
                continue;
            }
            let r = ad.call(k, m, p, Some(b.clone())).await;
            assert_eq!(
                (r.status, r.body["code"].as_str().unwrap()),
                (403, "forbidden"),
                "{role} {m} {p}"
            );
        }
    }
    assert!(hub.signing_keys().await.unwrap().is_empty());
    assert!(hub.api_keys().is_empty());
    assert!(hub.client_defaults().create_groups);
    assert_eq!(hub.admins().await.unwrap().len(), 1);
    assert_eq!(hub.admin_keys().await.unwrap().len(), 4);
    assert_eq!(
        ad.audits("admin.").await.len(),
        before,
        "refusals leave no trace"
    );
    // Every listed route exists (answers something other than 404/405).
    for r in agentim_core::admin_api::ROUTES {
        let (m, p) = r.split_once(' ').unwrap();
        let s = ad.call(&keys[0].1, m, p, None).await.status;
        assert!(s != 404 && s != 405, "{r}: {s}");
    }
    // A revoked key stops working at once; revoking again is not_found.
    let id = hub
        .admin_keys()
        .await
        .unwrap()
        .iter()
        .find(|k| k.role == "moderator")
        .unwrap()
        .key_id
        .clone();
    assert_eq!(
        ad.call(
            &owner,
            "DELETE",
            &format!("/admin/v1/deploy/admin-keys/{id}"),
            None
        )
        .await
        .status,
        200
    );
    assert_eq!(
        ad.call(&keys[3].1, "GET", "/admin/v1/app/overview", None)
            .await
            .status,
        401
    );
    assert_eq!(
        ad.call(
            &owner,
            "DELETE",
            &format!("/admin/v1/deploy/admin-keys/{id}"),
            None
        )
        .await
        .status,
        404
    );
    let r = ad
        .call(
            &owner,
            "POST",
            "/admin/v1/deploy/admin-keys",
            Some(json!({"name": "x", "role": "root"})),
        )
        .await;
    assert_eq!(r.status, 400);
    let r = ad
        .call(
            &owner,
            "POST",
            "/admin/v1/deploy/admin-keys",
            Some(json!({"name": "", "role": "owner"})),
        )
        .await;
    assert_eq!(r.status, 400);
    // The key's secret is shown once, never listed.
    let list = ad
        .call(&owner, "GET", "/admin/v1/deploy/admin-keys", None)
        .await
        .body;
    for (_, k) in &keys {
        let Auth::Key(k) = k else { unreachable!() };
        assert!(!list.to_string().contains(k.as_str()));
    }
}

async fn admin_keys_and_sessions_survive_a_restart(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let ad = Admin::start(&hub).await;
    let owner = ad.owner().await;
    let k = ad.key(&owner, "operator").await;
    hub.shutdown();
    drop(ad);
    drop(hub);
    let hub = open(single(&db.url)).await;
    let ad = Admin::start(&hub).await;
    assert_eq!(
        ad.call(&owner, "GET", "/admin/v1/me", None).await.status,
        200
    );
    assert_eq!(
        ad.call(&k, "GET", "/admin/v1/deploy/status", None)
            .await
            .status,
        200
    );
    assert!(hub.setup_token().await.unwrap().is_none(), "set up already");
    let s = ad
        .call(&k, "GET", "/admin/v1/deploy/status", None)
        .await
        .body;
    assert!(
        s["database_details"]
            .as_array()
            .is_some_and(|d| !d.is_empty()),
        "{s}"
    );
    assert_eq!(s["clustered"], false);
}

async fn the_app_area_follows_the_business_rules(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let ad = Admin::start(&hub).await;
    let owner = ad.owner().await;
    let dev = ad.key(&owner, "developer").await;
    let modr = ad.key(&owner, "moderator").await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    // Keys: an API key works on the open API; a signing key logs people in.
    let r = ad
        .call(
            &dev,
            "POST",
            "/admin/v1/app/api-keys",
            Some(json!({"name": "backend", "scopes": ["users:read"]})),
        )
        .await;
    assert_eq!(r.status, 201, "{}", r.body);
    let api_key = r.body["key"].as_str().unwrap().to_string();
    let key_id = r.body["key_id"].as_str().unwrap().to_string();
    let o = reqwest::Client::new()
        .get(format!("{}/v1/users/alice", ad.base))
        .bearer_auth(&api_key)
        .send()
        .await
        .unwrap();
    assert_eq!(o.status(), 200);
    for b in [
        json!({"name": "x", "scopes": ["nope"]}),
        json!({"name": "", "scopes": ["users:read"]}),
        json!({"name": "x", "scopes": ["users:read"], "ttl_days": 0}),
        json!({"name": "x"}),
    ] {
        assert_eq!(
            ad.call(&dev, "POST", "/admin/v1/app/api-keys", Some(b))
                .await
                .status,
            400
        );
    }
    let r = ad.call(&dev, "GET", "/admin/v1/app/api-keys", None).await;
    assert!(
        !r.body.to_string().contains(&api_key),
        "secrets are never listed"
    );
    assert_eq!(
        ad.call(
            &dev,
            "DELETE",
            &format!("/admin/v1/app/api-keys/{key_id}"),
            None
        )
        .await
        .status,
        200
    );
    assert_eq!(
        ad.call(
            &dev,
            "DELETE",
            &format!("/admin/v1/app/api-keys/{key_id}"),
            None
        )
        .await
        .status,
        404
    );
    let r = ad
        .call(
            &dev,
            "POST",
            "/admin/v1/app/signing-keys",
            Some(json!({"name": "web"})),
        )
        .await;
    assert_eq!(r.status, 201);
    let kid = r.body["kid"].as_str().unwrap().to_string();
    let secret = r.body["secret"].as_str().unwrap().to_string();
    let t = agentim_core::auth::jwt_sign(&kid, &secret, "carol", now_s(), now_s() + 60);
    assert!(
        hub.authenticate_client(&t, Some("d1"), None, None)
            .await
            .is_ok()
    );
    assert_eq!(
        ad.call(
            &dev,
            "DELETE",
            &format!("/admin/v1/app/signing-keys/{kid}"),
            None
        )
        .await
        .status,
        200
    );
    assert!(
        hub.authenticate_client(&t, Some("d1"), None, None)
            .await
            .is_err()
    );
    assert_eq!(
        ad.call(&dev, "DELETE", "/admin/v1/app/signing-keys/sk_none", None)
            .await
            .status,
        404
    );
    // People: list, detail, permissions (pushed), deactivate.
    let r = ad
        .call(&modr, "GET", "/admin/v1/app/users?kind=human&limit=1", None)
        .await;
    assert_eq!(r.body["users"].as_array().unwrap().len(), 1);
    assert_eq!(r.body["has_more"], true);
    let r = ad
        .call(
            &modr,
            "GET",
            "/admin/v1/app/users?kind=human&after=alice",
            None,
        )
        .await;
    let ids: Vec<&str> = r.body["users"]
        .as_array()
        .unwrap()
        .iter()
        .map(|u| u["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["bob", "carol"]);
    for q in ["kind=robot", "limit=0", "limit=201"] {
        assert_eq!(
            ad.call(&modr, "GET", &format!("/admin/v1/app/users?{q}"), None)
                .await
                .status,
            400,
            "{q}"
        );
    }
    assert_eq!(
        ad.call(&modr, "GET", "/admin/v1/app/users/nobody", None)
            .await
            .status,
        404
    );
    assert_eq!(
        ad.call(&modr, "GET", "/admin/v1/app/users/system", None)
            .await
            .status,
        404
    );
    let mut a = Conn::new(&hub, &alice);
    a.drain();
    let r = ad
        .call(
            &dev,
            "PATCH",
            "/admin/v1/app/users/alice",
            Some(json!({"permissions": {"create_groups": false}})),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["permissions"]["create_groups"], false);
    assert!(a.drain().iter().any(|f| f["type"] == "user.updated"));
    assert_eq!(
        a.call(json!({"op": "create_conv", "kind": "group", "title": "g", "members": []}))
            .await["error"]["code"],
        "forbidden"
    );
    assert_eq!(
        ad.call(
            &dev,
            "PATCH",
            "/admin/v1/app/users/alice",
            Some(json!({"permissions": {"fly": true}}))
        )
        .await
        .status,
        400
    );
    assert_eq!(
        ad.call(
            &modr,
            "PATCH",
            "/admin/v1/app/users/alice",
            Some(json!({"permissions": {}}))
        )
        .await
        .status,
        403
    );
    let r = ad
        .call(&dev, "POST", "/admin/v1/app/users/bob:deactivate", None)
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["deactivated"], true);
    assert!(
        hub.authenticate_client(&client_token(&hub, &bob).await, Some("d1"), None, None)
            .await
            .is_err()
    );
    assert_eq!(
        ad.call(&dev, "POST", "/admin/v1/app/users/bob:reactivate", None)
            .await
            .status,
        200
    );
    assert_eq!(
        ad.call(&dev, "POST", "/admin/v1/app/users/bob:explode", None)
            .await
            .status,
        404
    );
    // A device logged out from the console.
    let l = hub
        .authenticate_client(&client_token(&hub, &alice).await, Some("phone"), None, None)
        .await
        .unwrap();
    let r = ad
        .call(&dev, "GET", "/admin/v1/app/users/alice", None)
        .await;
    assert_eq!(r.body["devices"].as_array().unwrap().len(), 1, "{}", r.body);
    let path = format!("/admin/v1/app/users/alice/devices/{}", l.device_id);
    assert_eq!(ad.call(&dev, "DELETE", &path, None).await.status, 200);
    assert!(!hub.login_valid(&l));
    assert_eq!(
        ad.call(
            &dev,
            "DELETE",
            "/admin/v1/app/users/alice/devices/d_none",
            None
        )
        .await
        .status,
        404
    );
    // Agents: created for a person with a key, stopped, resumed, moved, destroyed.
    let r = ad
        .call(
            &dev,
            "POST",
            "/admin/v1/app/agents",
            Some(json!({"id": "helper", "owner": "alice"})),
        )
        .await;
    assert_eq!(r.status, 201, "{}", r.body);
    let key = r.body["key"].as_str().unwrap().to_string();
    assert!(key.starts_with("atk_"));
    for (b, s) in [
        (json!({"id": "helper", "owner": "alice"}), 409),
        (json!({"id": "x", "owner": "helper"}), 400),
        (json!({"id": "x", "owner": "nobody"}), 404),
        (json!({"id": "bad id", "owner": "alice"}), 400),
    ] {
        let r = ad
            .call(&dev, "POST", "/admin/v1/app/agents", Some(b.clone()))
            .await;
        assert_eq!(r.status, s, "{b}: {}", r.body);
    }
    let r = ad
        .call(
            &dev,
            "POST",
            "/admin/v1/app/agents/helper:stop",
            Some(json!({"reason": "loop"})),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["stopped"], true);
    assert_eq!(
        ad.call(&dev, "POST", "/admin/v1/app/agents/helper:stop", None)
            .await
            .status,
        409,
        "already stopped"
    );
    assert_eq!(
        ad.call(&dev, "POST", "/admin/v1/app/agents/helper:resume", None)
            .await
            .status,
        200
    );
    assert_eq!(
        ad.call(&dev, "POST", "/admin/v1/app/agents/helper:transfer", None)
            .await
            .status,
        400
    );
    let r = ad
        .call(
            &dev,
            "POST",
            "/admin/v1/app/agents/helper/keys",
            Some(json!({"revoke_others": true})),
        )
        .await;
    assert_eq!(r.status, 201, "{}", r.body);
    assert!(
        hub.authenticate_client(&key, None, None, None)
            .await
            .is_err(),
        "the old key is revoked"
    );
    let r = ad
        .call(&modr, "POST", "/admin/v1/app/agents/helper:stop", None)
        .await;
    assert_eq!(r.status, 403);
    let r = ad
        .call(&dev, "DELETE", "/admin/v1/app/agents/helper", None)
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(
        ad.call(&dev, "DELETE", "/admin/v1/app/agents/helper", None)
            .await
            .status,
        404
    );
    assert_eq!(
        ad.call(&dev, "POST", "/admin/v1/app/agents/alice:stop", None)
            .await
            .status,
        404,
        "not an agent"
    );
    // Reports: a moderator decides.
    let b = Conn::new(&hub, &bob);
    let conv = b.group(&[&alice]).await;
    let msg = b.send(&conv, "nasty").await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    a.ok(json!({"op": "report", "target_kind": "message", "target_id": msg, "category": "spam"}))
        .await;
    let r = ad.call(&modr, "GET", "/admin/v1/app/reports", None).await;
    let rid = r.body["reports"][0]["id"].as_str().unwrap().to_string();
    assert_eq!(
        ad.call(&modr, "GET", &format!("/admin/v1/app/reports/{rid}"), None)
            .await
            .status,
        200
    );
    assert_eq!(
        ad.call(&modr, "GET", "/admin/v1/app/reports/rep_none", None)
            .await
            .status,
        404
    );
    assert_eq!(
        ad.call(&modr, "GET", "/admin/v1/app/reports?state=maybe", None)
            .await
            .status,
        400
    );
    let path = format!("/admin/v1/app/reports/{rid}:resolve");
    assert_eq!(
        ad.call(
            &modr,
            "POST",
            &path,
            Some(json!({"action": "remove_content", "reason": ""}))
        )
        .await
        .status,
        400
    );
    let r = ad
        .call(
            &modr,
            "POST",
            &path,
            Some(json!({"action": "remove_content", "reason": "spam"})),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(
        ad.call(
            &modr,
            "POST",
            &path,
            Some(json!({"action": "none", "reason": "again"}))
        )
        .await
        .status,
        409
    );
    let r = ad
        .call(&modr, "GET", "/admin/v1/app/reports?state=resolved", None)
        .await;
    assert_eq!(r.body["reports"].as_array().unwrap().len(), 1);
    // Webhooks and the overview.
    let r = ad.call(&dev, "GET", "/admin/v1/app/webhooks", None).await;
    assert!(r.body["queued"].as_i64().is_some());
    let r = ad.call(&dev, "GET", "/admin/v1/app/overview", None).await;
    assert_eq!(r.body["people"], 3, "{}", r.body);
    assert_eq!(r.body["agents"], 0, "destroyed agents are not counted");
    assert_eq!(r.body["conversations"], 1);
    // Every change is in the audit log with the admin, newest first.
    let r = ad
        .call(
            &dev,
            "GET",
            "/admin/v1/app/audit?prefix=admin.agents.&limit=200",
            None,
        )
        .await;
    let acts: Vec<&str> = r.body["records"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["action"].as_str().unwrap())
        .collect();
    assert_eq!(
        acts,
        [
            "admin.agents.destroy",
            "admin.agents.key",
            "admin.agents.resume",
            "admin.agents.stop",
            "admin.agents.create"
        ]
    );
    assert_eq!(r.body["records"][0]["actor"], "admin:key:developer key");
    let r = ad
        .call(
            &dev,
            "GET",
            "/admin/v1/app/audit?prefix=admin.&limit=2",
            None,
        )
        .await;
    assert_eq!(r.body["has_more"], true);
    let before = r.body["records"][1]["id"].as_str().unwrap().to_string();
    let r2 = ad
        .call(
            &dev,
            "GET",
            &format!("/admin/v1/app/audit?prefix=admin.&limit=2&before={before}"),
            None,
        )
        .await;
    assert!(r2.body["records"][0]["id"].as_str().unwrap() < before.as_str());
    assert_eq!(
        ad.call(&dev, "GET", "/admin/v1/app/audit?limit=0", None)
            .await
            .status,
        400
    );
    // Wildcards in the prefix are plain text.
    let r = ad
        .call(&dev, "GET", "/admin/v1/app/audit?prefix=admin_", None)
        .await;
    assert_eq!(r.body["records"], json!([]));
    let r = ad
        .call(&dev, "GET", "/admin/v1/app/audit?prefix=%25", None)
        .await;
    assert_eq!(r.body["records"], json!([]));
}

async fn reading_content_needs_a_reason_and_is_audited(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let ad = Admin::start(&hub).await;
    let owner = ad.owner().await;
    let modr = ad.key(&owner, "moderator").await;
    let op = ad.key(&owner, "operator").await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let a = Conn::new(&hub, &alice);
    let conv = a.group(&[&bob]).await;
    a.send(&conv, "hello bob").await;
    // Metadata without a reason: members, no content.
    let r = ad
        .call(&modr, "GET", "/admin/v1/app/conversations", None)
        .await;
    assert_eq!(
        r.body["conversations"].as_array().unwrap().len(),
        1,
        "{}",
        r.body
    );
    assert_eq!(r.body["conversations"][0]["members"], 2);
    assert!(!r.body.to_string().contains("hello bob"));
    let r = ad
        .call(
            &modr,
            "GET",
            &format!("/admin/v1/app/conversations/{conv}"),
            None,
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert!(!r.body.to_string().contains("hello bob"));
    assert!(
        r.body["members"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["user"] == "bob")
    );
    assert_eq!(
        ad.call(&modr, "GET", "/admin/v1/app/conversations/c_none", None)
            .await
            .status,
        404
    );
    // Paging by id (byte order on every database) and search by title or id.
    let second = a.group(&[]).await;
    let mut ids = [conv.clone(), second.clone()];
    ids.sort();
    let r = ad
        .call(&modr, "GET", "/admin/v1/app/conversations?limit=1", None)
        .await;
    assert_eq!(
        (
            r.body["conversations"][0]["id"].as_str().unwrap(),
            &r.body["has_more"]
        ),
        (ids[0].as_str(), &json!(true))
    );
    let r = ad
        .call(
            &modr,
            "GET",
            &format!("/admin/v1/app/conversations?after={}", ids[0]),
            None,
        )
        .await;
    assert_eq!(r.body["conversations"].as_array().unwrap().len(), 1);
    assert_eq!(r.body["conversations"][0]["id"], ids[1].as_str());
    let r = ad
        .call(
            &modr,
            "GET",
            &format!("/admin/v1/app/conversations?q={second}"),
            None,
        )
        .await;
    assert_eq!(r.body["conversations"].as_array().unwrap().len(), 1);
    let r = ad
        .call(&modr, "GET", "/admin/v1/app/conversations?q=%25", None)
        .await;
    assert_eq!(r.body["conversations"], json!([]), "a literal %");
    let before = ad.audits("admin.").await.len();
    let path = format!("/admin/v1/app/conversations/{conv}/messages");
    for q in [
        "",
        "?reason=",
        "?reason=%20%20",
        &format!("?reason={}", "x".repeat(501)),
        "?reason=ok&after_seq=-1",
        "?reason=ok&limit=0",
    ] {
        let r = ad.call(&modr, "GET", &format!("{path}{q}"), None).await;
        assert_eq!(
            (r.status, r.body["code"].as_str().unwrap()),
            (400, "bad_request"),
            "{q}"
        );
    }
    assert_eq!(
        ad.call(&op, "GET", &format!("{path}?reason=x"), None)
            .await
            .status,
        403
    );
    assert_eq!(
        ad.call(
            &modr,
            "GET",
            "/admin/v1/app/conversations/c_none/messages?reason=x",
            None
        )
        .await
        .status,
        404
    );
    assert_eq!(
        ad.audits("admin.").await.len(),
        before,
        "refusals leave no trace"
    );
    let r = ad
        .call(&modr, "GET", &format!("{path}?reason=report%20rep_1"), None)
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert!(r.body.to_string().contains("hello bob"));
    let reads = ad.audits("admin.conversations.read").await;
    assert_eq!(reads.len(), 1);
    assert_eq!(reads[0]["detail"]["reason"], "report rep_1");
    assert_eq!(reads[0]["target"], conv.as_str());
    // Reading does not make the admin a member or tell anyone.
    let r = ad
        .call(
            &modr,
            "GET",
            &format!("/admin/v1/app/conversations/{conv}"),
            None,
        )
        .await;
    assert_eq!(r.body["members"].as_array().unwrap().len(), 2);
}

async fn client_defaults_apply_everywhere(db: TestDb) {
    let mut cfg = single(&db.url);
    cfg.clients.allow = Default::default();
    let hub = open(cfg.clone()).await;
    let ad = Admin::start(&hub).await;
    let owner = ad.owner().await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let r = ad
        .call(&owner, "GET", "/admin/v1/app/settings/clients", None)
        .await;
    assert_eq!(r.body["create_agents"], false);
    for b in [
        json!({"fly": true}),
        json!({"edit_minutes": -1}),
        json!({"recall_minutes": 600_000}),
        json!([]),
    ] {
        let r = ad
            .call(
                &owner,
                "PUT",
                "/admin/v1/app/settings/clients",
                Some(b.clone()),
            )
            .await;
        assert_eq!(r.status, 400, "{b}");
    }
    let r = ad
        .call(
            &owner,
            "PUT",
            "/admin/v1/app/settings/clients",
            Some(json!({"create_agents": true, "find_users": false})),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(
        r.body["create_groups"], true,
        "missing keys take the built-in default"
    );
    let a = Conn::new(&hub, &alice);
    assert_eq!(
        a.ok(json!({"op": "hello"})).await["me"]["permissions"]["create_agents"],
        true
    );
    assert_eq!(
        a.call(json!({"op": "find_user", "name": "x"})).await["error"]["code"],
        "forbidden"
    );
    assert_eq!(
        a.call(json!({"op": "create_agent", "name": "helper"}))
            .await["ok"],
        true
    );
    // A person's override still wins.
    hub.set_permissions(&alice, &json!({"find_users": true}))
        .await
        .unwrap();
    assert_eq!(
        a.call(json!({"op": "find_user", "name": "bob"})).await["error"]["code"],
        "not_found"
    );
    // The configuration file is only the start; the console's value stays.
    hub.shutdown();
    drop((a, ad, hub));
    let hub = open(cfg).await;
    assert!(hub.client_defaults().create_agents);
    assert!(!hub.client_defaults().find_users);
    assert_eq!(
        hub.audit_log(1000)
            .await
            .unwrap()
            .iter()
            .filter(|r| r["action"] == "admin.settings.clients")
            .count(),
        1
    );
}

async fn admins_and_the_last_owner(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let ad = Admin::start(&hub).await;
    let owner = ad.owner().await;
    let mk = |n: &str, role: &str| json!({"name": n, "role": role, "password": PW});
    for (b, s) in [
        (mk("ops", "operator"), 201),
        (mk("ops", "operator"), 409),
        (mk("x", "root"), 400),
        (mk("a b", "operator"), 400),
        (
            json!({"name": "y", "role": "operator", "password": "short"}),
            400,
        ),
        (json!({"name": "y", "role": "operator"}), 400),
    ] {
        let r = ad
            .call(&owner, "POST", "/admin/v1/deploy/admins", Some(b.clone()))
            .await;
        assert_eq!(r.status, s, "{b}: {}", r.body);
    }
    // Admins never see each other's secrets.
    let r = ad
        .call(&owner, "GET", "/admin/v1/deploy/admins", None)
        .await;
    assert_eq!(r.body["admins"].as_array().unwrap().len(), 2);
    assert!(!r.body.to_string().contains("argon2"));
    // The only owner cannot demote or disable themselves.
    for b in [json!({"role": "developer"}), json!({"disabled": true})] {
        let r = ad
            .call(&owner, "PATCH", "/admin/v1/deploy/admins/root", Some(b))
            .await;
        assert_eq!(
            (r.status, r.body["code"].as_str().unwrap()),
            (409, "conflict")
        );
    }
    assert_eq!(
        ad.call(
            &owner,
            "PATCH",
            "/admin/v1/deploy/admins/root",
            Some(json!({}))
        )
        .await
        .status,
        400
    );
    assert_eq!(
        ad.call(
            &owner,
            "PATCH",
            "/admin/v1/deploy/admins/nobody",
            Some(json!({"role": "owner"}))
        )
        .await
        .status,
        404
    );
    // Disabling ends the admin's sessions; enabling lets them log in again.
    let ops = session_of(&ad.login("ops", PW, None).await);
    assert_eq!(
        ad.call(&ops, "GET", "/admin/v1/deploy/status", None)
            .await
            .status,
        200
    );
    assert_eq!(
        ad.call(&ops, "GET", "/admin/v1/app/overview", None)
            .await
            .status,
        403
    );
    let r = ad
        .call(
            &owner,
            "PATCH",
            "/admin/v1/deploy/admins/ops",
            Some(json!({"disabled": true})),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(ad.call(&ops, "GET", "/admin/v1/me", None).await.status, 401);
    let r = ad.login("ops", PW, None).await;
    assert_eq!(
        (r.status, r.body["code"].as_str().unwrap()),
        (403, "forbidden")
    );
    ad.call(
        &owner,
        "PATCH",
        "/admin/v1/deploy/admins/ops",
        Some(json!({"disabled": false})),
    )
    .await;
    // Promoted to owner: now root may step down, and the role applies at once.
    let ops = session_of(&ad.login("ops", PW, None).await);
    ad.call(
        &owner,
        "PATCH",
        "/admin/v1/deploy/admins/ops",
        Some(json!({"role": "owner"})),
    )
    .await;
    assert_eq!(
        ad.call(&ops, "GET", "/admin/v1/app/overview", None)
            .await
            .status,
        200
    );
    let r = ad
        .call(
            &owner,
            "PATCH",
            "/admin/v1/deploy/admins/root",
            Some(json!({"role": "developer"})),
        )
        .await;
    assert_eq!(r.status, 200);
    assert_eq!(
        ad.call(&owner, "GET", "/admin/v1/deploy/admins", None)
            .await
            .status,
        403
    );
    // Resetting a password ends that admin's sessions.
    let path = "/admin/v1/deploy/admins/root:reset-password";
    assert_eq!(
        ad.call(&ops, "POST", path, Some(json!({"password": "short"})))
            .await
            .status,
        400
    );
    assert_eq!(
        ad.call(
            &ops,
            "POST",
            "/admin/v1/deploy/admins/nobody:reset-password",
            Some(json!({"password": PW}))
        )
        .await
        .status,
        404
    );
    assert_eq!(
        ad.call(
            &ops,
            "POST",
            path,
            Some(json!({"password": "brand new password"}))
        )
        .await
        .status,
        200
    );
    assert_eq!(
        ad.call(&owner, "GET", "/admin/v1/me", None).await.status,
        401
    );
    assert_eq!(
        ad.login("root", "brand new password", None).await.status,
        200
    );
    let acts: Vec<String> = ad
        .audits("admin.admins.")
        .await
        .iter()
        .map(|a| a["action"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        acts.iter().filter(|a| *a == "admin.admins.create").count(),
        1
    );
    assert_eq!(
        acts.iter().filter(|a| *a == "admin.admins.update").count(),
        4
    );
    assert_eq!(
        acts.iter()
            .filter(|a| *a == "admin.admins.reset_password")
            .count(),
        1
    );
}

async fn another_process_sees_the_same(db: TestDb) {
    let h1 = open(single(&db.url)).await;
    let h2 = open(single(&db.url)).await;
    let a1 = Admin::start(&h1).await;
    let a2 = Admin::start(&h2).await;
    let owner = a1.owner().await;
    // A session and a key made on one node work on the other.
    assert_eq!(
        a2.call(&owner, "GET", "/admin/v1/me", None).await.status,
        200
    );
    let k = a1.key(&owner, "developer").await;
    assert_eq!(
        a2.call(&k, "GET", "/admin/v1/app/overview", None)
            .await
            .status,
        200
    );
    // The second node offers no setup.
    assert!(h2.setup_token().await.unwrap().is_none());
    // Client defaults changed on one node reach the other.
    let r = a1
        .call(
            &k,
            "PUT",
            "/admin/v1/app/settings/clients",
            Some(json!({"create_groups": false})),
        )
        .await;
    assert_eq!(r.status, 200);
    h2.refresh_auth().await.unwrap();
    assert!(!h2.client_defaults().create_groups);
    // Logging out on one node ends the session on both.
    a1.call(&owner, "POST", "/admin/v1/logout", None).await;
    assert_eq!(
        a2.call(&owner, "GET", "/admin/v1/me", None).await.status,
        401
    );
}

async fn race_setup_and_owner_changes(db: TestDb) {
    for round in 0..4 {
        if round > 0 {
            sql(&db, "DELETE FROM admin_sessions").await;
            sql(&db, "DELETE FROM admins").await;
        }
        let hub = open(single(&db.url)).await;
        let ad = std::sync::Arc::new(Admin::start(&hub).await);
        // Many tabs use the same setup link: exactly one owner.
        let t = hub.setup_token().await.unwrap().unwrap();
        let mut tasks = vec![];
        for i in 0..8 {
            let (ad, t) = (ad.clone(), t.clone());
            tasks.push(tokio::spawn(async move {
                ad.call(
                    &Auth::None,
                    "POST",
                    "/admin/v1/setup",
                    Some(json!({"token": t, "name": format!("o{i}"), "password": PW})),
                )
                .await
                .status
            }));
        }
        let mut won = 0;
        for t in tasks {
            match t.await.unwrap() {
                200 => won += 1,
                s => assert!(s == 403 || s == 409, "{s}"),
            }
        }
        assert_eq!(won, 1);
        assert_eq!(hub.admins().await.unwrap().len(), 1);
        assert_eq!(ad.audits("admin.setup").await.len(), round + 1);
        // Two owners demote each other at once: one owner is left.
        let name = hub.admins().await.unwrap()[0].name.clone();
        let o = session_of(&ad.login(&name, PW, None).await);
        ad.call(
            &o,
            "POST",
            "/admin/v1/deploy/admins",
            Some(json!({"name": "second", "role": "owner", "password": PW})),
        )
        .await;
        let mut tasks = vec![];
        for n in [name.clone(), "second".to_string()] {
            let (ad, o) = (ad.clone(), o.clone());
            tasks.push(tokio::spawn(async move {
                ad.call(
                    &o,
                    "PATCH",
                    &format!("/admin/v1/deploy/admins/{n}"),
                    Some(json!({"disabled": true})),
                )
                .await
                .status
            }));
        }
        let codes: Vec<u16> = futures_join(tasks).await;
        assert_eq!(codes.iter().filter(|c| **c == 200).count(), 1, "{codes:?}");
        let owners = hub
            .admins()
            .await
            .unwrap()
            .into_iter()
            .filter(|a| a.role == "owner" && a.disabled_at.is_none())
            .count();
        assert_eq!(owners, 1);
        // The same name created many times: one admin.
        let o2 = session_of(
            &ad.login(if codes[0] == 200 { "second" } else { &name }, PW, None)
                .await,
        );
        let mut tasks = vec![];
        for _ in 0..6 {
            let (ad, o2) = (ad.clone(), o2.clone());
            tasks.push(tokio::spawn(async move {
                ad.call(
                    &o2,
                    "POST",
                    "/admin/v1/deploy/admins",
                    Some(json!({"name": "dup", "role": "moderator", "password": PW})),
                )
                .await
                .status
            }));
        }
        let codes = futures_join(tasks).await;
        assert_eq!(codes.iter().filter(|c| **c == 201).count(), 1, "{codes:?}");
        assert!(codes.iter().all(|c| *c == 201 || *c == 409));
        hub.shutdown();
    }
}

async fn futures_join(tasks: Vec<tokio::task::JoinHandle<u16>>) -> Vec<u16> {
    let mut out = vec![];
    for t in tasks {
        out.push(t.await.unwrap());
    }
    out
}

/// B9: a wrong current password or two-factor code given in a session
/// counts like a failed login, so a stolen session cannot guess freely.
async fn guesses_in_a_session_count_as_failed_logins(db: TestDb) {
    let mut cfg = single(&db.url);
    cfg.auth.max_failures = 3;
    let hub = open(cfg).await;
    let ad = Admin::start(&hub).await;
    let owner = ad.owner().await;
    for _ in 0..2 {
        let r = ad
            .call(
                &owner,
                "POST",
                "/admin/v1/me/password",
                Some(json!({"current": "a wrong guess!", "new": "another long one"})),
            )
            .await;
        assert_eq!(r.status, 403);
    }
    // The third failure, here at the login, locks.
    assert_eq!(ad.login("root", "wrong password!", None).await.status, 401);
    let r = ad.login("root", PW, None).await;
    assert_eq!(
        (r.status, r.body["code"].as_str().unwrap()),
        (429, "rate_limited")
    );
}
