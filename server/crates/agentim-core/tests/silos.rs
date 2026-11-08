//! Tenants in their own databases (docs/en/design/multi-tenancy.md): each tenant is a
//! complete instance with its own database on its cluster; requests reach
//! it by host; the same ids in two tenants are different people; keys
//! work only on their tenant's host; a suspended tenant's connections end
//! and its host answers 404; data stays across restarts; concurrent first
//! uses open one instance. Every backend (SQLite files, PostgreSQL and
//! MySQL databases).

mod common;

use std::time::Duration;

use agentim_core::config::{Config, TenancyConfig, TenantCluster};
use agentim_core::hub::Hub;
use agentim_core::server::{AppState, app};
use common::*;
use serde_json::{Value, json};

backend_tests!(
    each_tenant_is_its_own_instance,
    keys_work_only_on_their_tenants_host,
    suspension_closes_the_instance,
    tenants_survive_a_restart,
    race_first_uses_open_one_instance,
    plan_limits_hold_in_the_tenants_instance,
    webhooks_live_in_the_tenants_database,
    deleting_a_tenant_drops_its_database,
);

const SUFFIX: &str = "im.test";

/// The cluster URL template for this backend: next to the test's database.
fn cluster_url(db: &TestDb) -> String {
    match &db.path {
        Some(p) => format!(
            "sqlite://{}/tenants/{{tenant}}.db",
            p.parent().unwrap().to_string_lossy().replace('\\', "/")
        ),
        None => format!("{}_{{tenant}}", db.url),
    }
}

fn cfg(db: &TestDb) -> Config {
    let mut c = single(&db.url);
    c.tenancy = TenancyConfig {
        enabled: true,
        host_suffix: SUFFIX.into(),
        clusters: vec![
            TenantCluster {
                name: "free".into(),
                url: cluster_url(db),
                read_pool: 2,
            },
            TenantCluster {
                name: "paid".into(),
                url: cluster_url(db).replace("{tenant}", "paid_{tenant}"),
                read_pool: 2,
            },
        ],
        ..Default::default()
    };
    c
}

async fn serve(hub: &Hub) -> String {
    let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = tcp.local_addr().unwrap().to_string();
    let a = app(AppState {
        hub: hub.clone(),
        demo_login: false,
        metrics_token: None,
    });
    tokio::spawn(async move {
        axum::serve(
            tcp,
            a.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap()
    });
    addr
}

async fn call(
    addr: &str,
    host: &str,
    key: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let mut r = reqwest::Client::new()
        .request(method.parse().unwrap(), format!("http://{addr}{path}"))
        .header("host", host)
        .header("authorization", format!("Bearer {key}"));
    if let Some(b) = body {
        r = r
            .header("content-type", "application/json")
            .body(b.to_string());
    }
    let res = r.send().await.unwrap();
    let s = res.status().as_u16();
    let t = res.text().await.unwrap();
    (s, serde_json::from_str(&t).unwrap_or(Value::String(t)))
}

fn scopes() -> Vec<String> {
    agentim_core::open_api::SCOPES
        .iter()
        .map(|s| s.to_string())
        .collect()
}

/// Drops the tenants' databases a PostgreSQL or MySQL test made.
async fn cleanup(hub: &Hub, tenants: &[&str]) {
    for t in tenants {
        let _ = hub.drop_tenant_database(t).await;
    }
}

/// Same ids in two tenants are two people; each tenant's data is in its
/// own database (on the cluster it was created on); usage counts its own.
async fn each_tenant_is_its_own_instance(db: TestDb) {
    let hub = open(cfg(&db)).await;
    hub.create_tenant_in("acme", "Acme", json!({}), None)
        .await
        .unwrap();
    hub.create_tenant_in("globex", "Globex", json!({}), Some("paid"))
        .await
        .unwrap();
    let e = hub
        .create_tenant_in("other", "x", json!({}), Some("nope"))
        .await
        .unwrap_err();
    assert_eq!((e.code, e.rule), ("bad_request", Some("tenant.cluster")));
    assert!(
        hub.tenants()
            .await
            .unwrap()
            .iter()
            .all(|t| t["id"] != "other"),
        "no trace"
    );
    let addr = serve(&hub).await;
    let mut keys = std::collections::HashMap::new();
    for t in ["acme", "globex"] {
        let (k, _) = hub
            .create_api_key_in(t, "backend", &scopes(), None, None)
            .await
            .unwrap();
        keys.insert(t, k);
    }
    for (t, name) in [("acme", "Alice of Acme"), ("globex", "Alice of Globex")] {
        let (s, v) = call(
            &addr,
            &format!("{t}.{SUFFIX}"),
            &keys[t],
            "PUT",
            "/v1/users/alice",
            Some(json!({"display_name": name})),
        )
        .await;
        assert!((200..300).contains(&s), "{t}: {s} {v}");
    }
    for (t, name) in [("acme", "Alice of Acme"), ("globex", "Alice of Globex")] {
        let (s, v) = call(
            &addr,
            &format!("{t}.{SUFFIX}"),
            &keys[t],
            "GET",
            "/v1/users/alice",
            None,
        )
        .await;
        assert_eq!((s, v["display_name"].as_str()), (200, Some(name)), "{v}");
    }
    // Each instance holds only its own people; the deployment none of them.
    let a = hub.tenant_hub("acme").await.unwrap();
    let g = hub.tenant_hub("globex").await.unwrap();
    assert!(!a.same(&g));
    assert!(a.user_by_name("alice").is_some() && g.user_by_name("alice").is_some());
    assert_ne!(
        a.user_by_name("alice").unwrap().id,
        g.user_by_name("alice").unwrap().id
    );
    assert!(
        hub.user_by_name("alice").is_none(),
        "nothing in the deployment's database"
    );
    // Their databases are where their clusters say.
    let acme_db = &a.config().storage.databases[0].url;
    let globex_db = &g.config().storage.databases[0].url;
    assert_eq!(
        acme_db,
        &agentim_core::hub::silo_url(&cluster_url(&db), "acme")
    );
    assert!(globex_db.contains("paid_globex"), "{globex_db}");
    let tenants = hub.tenants().await.unwrap();
    let cluster = |id: &str| tenants.iter().find(|t| t["id"] == id).unwrap()["cluster"].clone();
    assert_eq!(
        (cluster("acme"), cluster("globex")),
        (json!("free"), json!("paid"))
    );
    // Usage is each tenant's own.
    let day = agentim_core::hub::utc_day(agentim_core::model::now_ms());
    let u = hub.tenant_usage("acme", &day).await.unwrap();
    assert_eq!(u["tenant"], "acme");
    // The day's peak of connections at once stays after people leave
    // (hosted plans count it); another day has its own.
    let before = u["connections"].as_i64().unwrap();
    let al = a.user_by_name("alice").unwrap();
    let one = a.connect(al.clone());
    let two = a.connect(al.clone());
    drop(two);
    let u = hub.tenant_usage("acme", &day).await.unwrap();
    assert_eq!(u["connections"].as_i64(), Some(before + 1), "{u}");
    assert!(u["peak_connections"].as_i64().unwrap() >= before + 2, "{u}");
    assert_eq!(
        hub.tenant_usage("acme", "2000-01-01").await.unwrap()["peak_connections"],
        0
    );
    assert_eq!(
        hub.tenant_usage("globex", &day).await.unwrap()["peak_connections"].as_i64(),
        Some(g.tenant_connections("default")),
        "each tenant its own"
    );
    drop(one);
    // Unknown tenants' hosts 404; the deployment's host is the deployment.
    let (s, _) = call(
        &addr,
        &format!("nope.{SUFFIX}"),
        &keys["acme"],
        "GET",
        "/v1/users/alice",
        None,
    )
    .await;
    assert_eq!(s, 404);
    let (s, _) = call(&addr, "localhost", &keys["acme"], "GET", "/healthz", None).await;
    assert_eq!(s, 200);
    cleanup(&hub, &["acme", "globex"]).await;
}

/// An API key or a client token of one tenant is unknown on another's host.
async fn keys_work_only_on_their_tenants_host(db: TestDb) {
    let hub = open(cfg(&db)).await;
    for t in ["acme", "globex"] {
        hub.create_tenant_in(t, t, json!({}), None).await.unwrap();
    }
    let addr = serve(&hub).await;
    let (acme_key, _) = hub
        .create_api_key_in("acme", "b", &scopes(), None, None)
        .await
        .unwrap();
    let (s, _) = call(
        &addr,
        &format!("acme.{SUFFIX}"),
        &acme_key,
        "PUT",
        "/v1/users/bob",
        Some(json!({"display_name": "Bob"})),
    )
    .await;
    assert!((200..300).contains(&s));
    let (s, _) = call(
        &addr,
        &format!("globex.{SUFFIX}"),
        &acme_key,
        "GET",
        "/v1/users/bob",
        None,
    )
    .await;
    assert_eq!(s, 401, "another tenant's key");
    // Signing keys too: a token signed with acme's key logs in on acme only.
    let k = hub.create_signing_key_in("acme", "app").await.unwrap();
    let now = agentim_core::model::now_ms() / 1000;
    let tok = agentim_core::auth::jwt_sign(
        k["kid"].as_str().unwrap(),
        k["secret"].as_str().unwrap(),
        "carol",
        now,
        now + 600,
    );
    let a = hub.tenant_hub("acme").await.unwrap();
    let g = hub.tenant_hub("globex").await.unwrap();
    assert!(
        a.authenticate_client_from(&tok, Some("d1"), None, None, None, "default")
            .await
            .is_ok()
    );
    assert!(
        g.authenticate_client_from(&tok, Some("d1"), None, None, None, "default")
            .await
            .is_err()
    );
    assert!(g.user_by_name("carol").is_none(), "refused leaves nothing");
    cleanup(&hub, &["acme", "globex"]).await;
}

/// Suspended: the instance closes, its sessions end, its host 404s, the
/// other tenant goes on; resumed, its data is back.
async fn suspension_closes_the_instance(db: TestDb) {
    let hub = open(cfg(&db)).await;
    for t in ["acme", "globex"] {
        hub.create_tenant_in(t, t, json!({}), None).await.unwrap();
    }
    let addr = serve(&hub).await;
    let (k, _) = hub
        .create_api_key_in("acme", "b", &scopes(), None, None)
        .await
        .unwrap();
    let (gk, _) = hub
        .create_api_key_in("globex", "b", &scopes(), None, None)
        .await
        .unwrap();
    call(
        &addr,
        &format!("acme.{SUFFIX}"),
        &k,
        "PUT",
        "/v1/users/dan",
        Some(json!({"display_name": "Dan"})),
    )
    .await;
    let a = hub.tenant_hub("acme").await.unwrap();
    let dan = a.user_by_name("dan").unwrap();
    let (session, _) = a.connect(dan.clone());
    hub.update_tenant("acme", None, None, Some(true))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), session.kicked())
        .await
        .expect("its connections end");
    assert_eq!(
        call(
            &addr,
            &format!("acme.{SUFFIX}"),
            &k,
            "GET",
            "/v1/users/dan",
            None
        )
        .await
        .0,
        404
    );
    assert_eq!(
        hub.tenant_hub("acme").await.err().unwrap().code,
        "not_found"
    );
    assert_eq!(
        call(
            &addr,
            &format!("globex.{SUFFIX}"),
            &gk,
            "PUT",
            "/v1/users/eve",
            Some(json!({"display_name": "Eve"}))
        )
        .await
        .0 / 100,
        2,
        "the other tenant goes on"
    );
    hub.update_tenant("acme", None, None, Some(false))
        .await
        .unwrap();
    let (s, v) = call(
        &addr,
        &format!("acme.{SUFFIX}"),
        &k,
        "GET",
        "/v1/users/dan",
        None,
    )
    .await;
    assert_eq!((s, v["display_name"].as_str()), (200, Some("Dan")));
    drop(session);
    cleanup(&hub, &["acme", "globex"]).await;
}

/// The deployment restarted: tenants open again with their data.
async fn tenants_survive_a_restart(db: TestDb) {
    let hub = open(cfg(&db)).await;
    hub.create_tenant_in("acme", "acme", json!({}), None)
        .await
        .unwrap();
    let (k, _) = hub
        .create_api_key_in("acme", "b", &scopes(), None, None)
        .await
        .unwrap();
    let a = hub.tenant_hub("acme").await.unwrap();
    agentim_core::hub::Hub::create_user(
        &a,
        "fay",
        agentim_core::model::UserKind::Human,
        None,
        None,
    )
    .await
    .unwrap();
    drop(a);
    drop(hub);
    let hub = open(cfg(&db)).await;
    let addr = serve(&hub).await;
    let a = hub.tenant_hub("acme").await.unwrap();
    assert!(a.user_by_name("fay").is_some());
    let (s, _) = call(
        &addr,
        &format!("acme.{SUFFIX}"),
        &k,
        "GET",
        "/v1/users/fay",
        None,
    )
    .await;
    assert_ne!(s, 401, "its key is still its");
    cleanup(&hub, &["acme"]).await;
}

/// Twenty first uses at once (5 rounds, new tenants each): one instance.
async fn race_first_uses_open_one_instance(db: TestDb) {
    let hub = open(cfg(&db)).await;
    let mut made = vec![];
    for round in 0..5 {
        let t = format!("t{round}");
        hub.create_tenant_in(&t, &t, json!({}), None).await.unwrap();
        let mut tasks = vec![];
        for _ in 0..20 {
            let (h, t) = (hub.clone(), t.clone());
            tasks.push(tokio::spawn(async move { h.tenant_hub(&t).await.unwrap() }));
        }
        let mut hubs = vec![];
        for x in tasks {
            hubs.push(x.await.unwrap());
        }
        assert!(hubs.iter().all(|h| h.same(&hubs[0])), "round {round}");
        made.push(t);
    }
    let names: Vec<&str> = made.iter().map(String::as_str).collect();
    cleanup(&hub, &names).await;
}

/// The plan's limits hold inside the tenant's own instance (connections,
/// monthly active people, requests per second) and change at once when the
/// deployment changes them; another tenant is not affected.
async fn plan_limits_hold_in_the_tenants_instance(db: TestDb) {
    let hub = open(cfg(&db)).await;
    hub.create_tenant_in(
        "acme",
        "acme",
        json!({"connections": 1, "mau": 1, "requests_per_second": 1}),
        None,
    )
    .await
    .unwrap();
    hub.create_tenant_in("globex", "globex", json!({}), None)
        .await
        .unwrap();
    let a = hub.tenant_hub("acme").await.unwrap();
    let g = hub.tenant_hub("globex").await.unwrap();
    let k = hub.create_signing_key_in("acme", "app").await.unwrap();
    let now = agentim_core::model::now_ms() / 1000;
    let token = |sub: &str| {
        agentim_core::auth::jwt_sign(
            k["kid"].as_str().unwrap(),
            k["secret"].as_str().unwrap(),
            sub,
            now,
            now + 600,
        )
    };
    // The first person of the month gets in; the second waits.
    let first = a
        .authenticate_client_from(&token("p1"), Some("d1"), None, None, None, "default")
        .await
        .unwrap();
    let e = a
        .authenticate_client_from(&token("p2"), Some("d2"), None, None, None, "default")
        .await
        .err()
        .unwrap();
    assert_eq!((e.code, e.rule), ("rate_limited", Some("tenant.mau")));
    assert!(a.user_by_name("p2").is_none(), "refused leaves nothing");
    // One connection at once.
    let (_s, _) = a.connect(first.user.clone());
    let e = a.tenant_connection_allowed("default").unwrap_err();
    assert_eq!(e.rule, Some("tenant.connections"));
    assert!(
        g.tenant_connection_allowed("default").is_ok(),
        "globex has no limit"
    );
    // Requests: a burst of twice the rate, then refused.
    let mut refused = 0;
    for _ in 0..5 {
        if a.tenant_request_allowed("default").is_err() {
            refused += 1;
        }
    }
    assert!(refused >= 2, "{refused}");
    // Raised by the deployment: the open instance takes it at once.
    hub.update_tenant(
        "acme",
        None,
        Some(json!({"connections": 10, "mau": 10})),
        None,
    )
    .await
    .unwrap();
    assert!(a.tenant_connection_allowed("default").is_ok());
    assert!(
        a.authenticate_client_from(&token("p2"), Some("d2"), None, None, None, "default")
            .await
            .is_ok()
    );
    cleanup(&hub, &["acme", "globex"]).await;
}

async fn receiver() -> (std::sync::Arc<std::sync::Mutex<Vec<Value>>>, String) {
    let got: std::sync::Arc<std::sync::Mutex<Vec<Value>>> = Default::default();
    let g = got.clone();
    let app = axum::Router::new().route(
        "/hook",
        axum::routing::post(move |body: String| {
            let g = g.clone();
            async move {
                g.lock()
                    .unwrap()
                    .push(serde_json::from_str::<Value>(&body).unwrap());
                axum::http::StatusCode::NO_CONTENT
            }
        }),
    );
    let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/hook", tcp.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(tcp, app).await.unwrap() });
    (got, url)
}

/// A tenant's webhook is stored in its own database and receives its own
/// events only; the deployment's database holds none of it.
async fn webhooks_live_in_the_tenants_database(db: TestDb) {
    let mut c = cfg(&db);
    c.tenancy.webhook_private_addresses = true;
    let hub = open(c).await;
    for t in ["acme", "globex"] {
        hub.create_tenant_in(t, t, json!({}), None).await.unwrap();
    }
    let (got_a, url_a) = receiver().await;
    let (got_g, url_g) = receiver().await;
    for (t, url) in [("acme", &url_a), ("globex", &url_g)] {
        let r: agentim_core::hub::TenantHookReq = serde_json::from_value(
            json!({"url": url, "secret": "a-secret-0123456789", "events": ["msg.created"], "backoff_ms": 50}),
        )
        .unwrap();
        hub.put_tenant_webhook(t, "main", r).await.unwrap();
    }
    assert_eq!(hub.tenant_webhooks_of("acme").await.unwrap().len(), 1);
    assert!(
        hub.tenant_webhooks("acme").is_empty(),
        "nothing in the deployment's database"
    );
    let a = hub.tenant_hub("acme").await.unwrap();
    let alice = user(&a, "alice", agentim_core::model::UserKind::Human, None).await;
    let bob = user(&a, "bob", agentim_core::model::UserKind::Human, None).await;
    let conn = Conn::new(&a, &alice);
    let conv = conn.group(&[&bob]).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    conn.send(&conv, "hello acme").await;
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while got_a.lock().unwrap().is_empty() {
        assert!(std::time::Instant::now() < deadline, "acme's webhook");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let b = got_a.lock().unwrap()[0].clone();
    assert_eq!(b["data"]["event"]["msg"]["content"], "hello acme");
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        got_g.lock().unwrap().is_empty(),
        "globex gets nothing of acme"
    );
    // Removed through the deployment: gone from the tenant's database.
    hub.delete_tenant_webhook("acme", "main").await.unwrap();
    assert!(hub.tenant_webhooks_of("acme").await.unwrap().is_empty());
    cleanup(&hub, &["acme", "globex"]).await;
}

/// Deleting a tenant ends its connections, drops its database and its row;
/// again: not_found; the other tenant stays.
async fn deleting_a_tenant_drops_its_database(db: TestDb) {
    let hub = open(cfg(&db)).await;
    for t in ["acme", "globex"] {
        hub.create_tenant_in(t, t, json!({}), None).await.unwrap();
    }
    let addr = serve(&hub).await;
    let (k, _) = hub
        .create_api_key_in("acme", "b", &scopes(), None, None)
        .await
        .unwrap();
    let a = hub.tenant_hub("acme").await.unwrap();
    let gil = user(&a, "gil", agentim_core::model::UserKind::Human, None).await;
    let (session, _) = a.connect(gil);
    let url = a.config().storage.databases[0].url.clone();
    drop(a);
    hub.delete_tenant("acme").await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), session.kicked())
        .await
        .expect("its connections end");
    assert!(
        hub.tenants()
            .await
            .unwrap()
            .iter()
            .all(|t| t["id"] != "acme")
    );
    assert_eq!(
        call(
            &addr,
            &format!("acme.{SUFFIX}"),
            &k,
            "GET",
            "/v1/users/gil",
            None
        )
        .await
        .0,
        404
    );
    assert_eq!(
        hub.delete_tenant("acme").await.unwrap_err().code,
        "not_found"
    );
    // Its database is gone.
    if let Some(p) = url.strip_prefix("sqlite://") {
        let p = p.split('?').next().unwrap();
        assert!(!std::path::Path::new(p).exists(), "{p}");
        // And nothing holds it open (Windows could not have deleted it).
        #[cfg(target_os = "linux")]
        {
            let open: Vec<_> = std::fs::read_dir("/proc/self/fd")
                .unwrap()
                .filter_map(|e| std::fs::read_link(e.ok()?.path()).ok())
                .filter(|l| l.to_string_lossy().contains(p))
                .collect();
            assert!(open.is_empty(), "still open: {open:?}");
        }
    } else {
        let (server, name) = url.rsplit_once('/').unwrap();
        let admin = if url.starts_with("postgres") {
            format!("{server}/postgres")
        } else {
            server.to_string()
        };
        let mut c = <sqlx::AnyConnection as sqlx::Connection>::connect(&admin)
            .await
            .unwrap();
        let sql = if url.starts_with("postgres") {
            format!("SELECT COUNT(*) FROM pg_database WHERE datname = '{name}'")
        } else {
            format!("SELECT COUNT(*) FROM information_schema.schemata WHERE schema_name = '{name}'")
        };
        let n: i64 = sqlx::Row::try_get(
            &sqlx::query(sqlx::AssertSqlSafe(sql))
                .fetch_one(&mut c)
                .await
                .unwrap(),
            0,
        )
        .unwrap();
        assert_eq!(n, 0, "{name} dropped");
    }
    assert!(hub.tenant_hub("globex").await.is_ok(), "globex stays");
    drop(session);
    cleanup(&hub, &["globex"]).await;
}
