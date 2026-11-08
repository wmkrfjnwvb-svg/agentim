//! The deployment's administration of tenants (docs/en/design/multi-tenancy.md): every tenant has
//! its own database. Tenant ids, names and limits are checked and
//! every refusal leaves nothing; the deployment API is for the deployment's
//! roles only; a tenant made over the API works at once on its host with
//! the keys made for it, not on another's; suspension and deletion over the
//! API; webhooks of a tenant over the API; a single-tenant server has no
//! tenants API; tenancy without database clusters does not start.

mod common;

use agentim_core::config::{Config, TenancyConfig, TenantCluster};
use agentim_core::hub::Hub;
use agentim_core::server::{AppState, app};
use common::*;
use serde_json::{Value, json};

backend_tests!(
    tenant_ids_names_and_changes_are_checked,
    the_deployment_api_provisions_tenants,
    the_deployment_api_manages_tenant_webhooks,
    a_single_tenant_server_has_no_tenants_api,
);

const SUFFIX: &str = "im.test";

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
        clusters: vec![TenantCluster {
            name: "free".into(),
            url: cluster_url(db),
            read_pool: 2,
        }],
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

async fn http(
    addr: &str,
    host: Option<&str>,
    key: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let mut r = reqwest::Client::new()
        .request(method.parse().unwrap(), format!("http://{addr}{path}"))
        .header("authorization", format!("Bearer {key}"));
    if let Some(h) = host {
        r = r.header("host", h);
    }
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

async fn cleanup(hub: &Hub, tenants: &[&str]) {
    for t in tenants {
        let _ = hub.drop_tenant_database(t).await;
    }
}

/// Bad ids, taken ids, bad names and limits, unknown tenants and empty
/// changes are refused, and create nothing; a configuration with tenancy
/// but no database cluster does not start.
async fn tenant_ids_names_and_changes_are_checked(db: TestDb) {
    let hub = open(cfg(&db)).await;
    hub.create_tenant("acme", "Acme", json!({})).await.unwrap();
    for bad in ["", "default", "Acme", "-a", "a-", "a_b", &"a".repeat(16)] {
        let e = hub.create_tenant(bad, "x", json!({})).await.unwrap_err();
        assert_eq!(
            (e.code, e.rule),
            ("bad_request", Some("tenant.id")),
            "{bad}"
        );
    }
    let e = hub
        .create_tenant("acme", "again", json!({}))
        .await
        .unwrap_err();
    assert_eq!((e.code, e.rule), ("conflict", Some("tenant.exists")));
    for (name, limits) in [("", json!({})), ("x", json!([1])), ("a\0b", json!({}))] {
        let e = hub.create_tenant("newco", name, limits).await.unwrap_err();
        assert_eq!(e.code, "bad_request");
    }
    assert_eq!(
        hub.tenants().await.unwrap().len(),
        1,
        "refusals created nothing"
    );
    assert_eq!(
        hub.update_tenant("nope", Some("x"), None, None)
            .await
            .unwrap_err()
            .code,
        "not_found"
    );
    assert_eq!(
        hub.update_tenant("acme", None, None, None)
            .await
            .unwrap_err()
            .code,
        "bad_request"
    );
    let v = hub
        .update_tenant("acme", Some("Acme Corp"), Some(json!({"mau": 2000})), None)
        .await
        .unwrap();
    assert_eq!(
        (v["name"].as_str(), v["limits"]["mau"].as_i64()),
        (Some("Acme Corp"), Some(2000))
    );
    // Tenancy needs somewhere to put each tenant.
    let mut shared = single(&db.url);
    shared.tenancy.enabled = true;
    shared.tenancy.host_suffix = SUFFIX.into();
    let e = shared.validate().unwrap_err();
    assert!(e.contains("tenancy.clusters"), "{e}");
    cleanup(&hub, &["acme"]).await;
}

/// The cloud's calls, over HTTP: only the deployment's roles; every
/// refusal leaves nothing; a new tenant works at once on its host with its
/// API key and its signing key, not on another's; suspended it is 404,
/// resumed it is back; deleted it is gone.
async fn the_deployment_api_provisions_tenants(db: TestDb) {
    let hub = open(cfg(&db)).await;
    hub.create_tenant("acme", "Acme", json!({})).await.unwrap();
    let addr = serve(&hub).await;
    let (ops, _) = hub.create_admin_key("cloud", "operator").await.unwrap();
    let (dev, _) = hub.create_admin_key("dev", "developer").await.unwrap();
    let a = |key: &str, m: &str, p: &str, b: Option<Value>| {
        let (addr, key, m, p) = (addr.clone(), key.to_string(), m.to_string(), p.to_string());
        async move { http(&addr, None, &key, &m, &p, b).await }
    };
    assert_eq!(
        a(&dev, "GET", "/admin/v1/deploy/tenants", None).await.0,
        403
    );
    let (s, v) = a(&ops, "GET", "/admin/v1/deploy/tenants", None).await;
    assert_eq!(
        (s, v["tenants"].as_array().map(Vec::len)),
        (200, Some(1)),
        "{v}"
    );
    for (body, code) in [
        (json!({"id": "Bad", "name": "x"}), 400),
        (json!({"id": "default", "name": "x"}), 400),
        (json!({"id": "acme", "name": "again"}), 409),
        (json!({"id": "newco", "name": ""}), 400),
        (json!({"id": "newco", "name": "x", "limits": [1]}), 400),
        (json!({"id": "newco", "name": "x", "cluster": "nope"}), 400),
        (json!({"id": "newco", "name": "x", "extra": 1}), 400),
    ] {
        let (s, v) = a(&ops, "POST", "/admin/v1/deploy/tenants", Some(body.clone())).await;
        assert_eq!(s, code, "{body} -> {v}");
    }
    assert_eq!(hub.tenants().await.unwrap().len(), 1, "no trace");
    let (s, v) = a(
        &ops,
        "POST",
        "/admin/v1/deploy/tenants",
        Some(json!({"id": "newco", "name": "New Co", "limits": {"mau": 2000}, "cluster": "free"})),
    )
    .await;
    assert_eq!(
        (s, v["id"].as_str(), v["cluster"].as_str()),
        (201, Some("newco"), Some("free")),
        "{v}"
    );
    // Keys inside it, returned once.
    assert_eq!(
        a(
            &ops,
            "POST",
            "/admin/v1/deploy/tenants/nope/api-keys",
            Some(json!({"name": "k"}))
        )
        .await
        .0,
        404
    );
    assert_eq!(
        a(
            &ops,
            "POST",
            "/admin/v1/deploy/tenants/newco/api-keys",
            Some(json!({"name": "k", "scopes": ["root"]}))
        )
        .await
        .0,
        400
    );
    let (s, key) = a(
        &ops,
        "POST",
        "/admin/v1/deploy/tenants/newco/api-keys",
        Some(json!({"name": "backend"})),
    )
    .await;
    assert_eq!(s, 201, "{key}");
    let secret = key["secret"].as_str().unwrap().to_string();
    let (s, sk) = a(
        &ops,
        "POST",
        "/admin/v1/deploy/tenants/newco/signing-keys",
        Some(json!({"name": "app"})),
    )
    .await;
    assert_eq!(s, 201, "{sk}");
    // Works at once on its host, not on another's.
    let host = format!("newco.{SUFFIX}");
    let put = |h: String| {
        let (addr, secret) = (addr.clone(), secret.clone());
        async move {
            http(
                &addr,
                Some(&h),
                &secret,
                "PUT",
                "/v1/users/zoe",
                Some(json!({})),
            )
            .await
            .0
        }
    };
    assert_eq!(put(host.clone()).await, 201);
    assert_eq!(put(format!("acme.{SUFFIX}")).await, 401);
    let now = agentim_core::model::now_ms() / 1000;
    let token = agentim_core::auth::jwt_sign(
        sk["kid"].as_str().unwrap(),
        sk["secret"].as_str().unwrap(),
        "zoe",
        now,
        now + 600,
    );
    let t = hub.tenant_hub("newco").await.unwrap();
    let login = t
        .authenticate_client_from(&token, Some("d1"), None, None, None, "default")
        .await
        .unwrap();
    assert_eq!(login.user.name, "zoe");
    drop(t);
    // Suspended over the API: 404; resumed: back.
    let (s, v) = a(
        &ops,
        "PATCH",
        "/admin/v1/deploy/tenants/newco",
        Some(json!({"suspended": true})),
    )
    .await;
    assert_eq!((s, v["suspended"].as_bool()), (200, Some(true)), "{v}");
    assert_eq!(put(host.clone()).await, 404);
    assert_eq!(
        a(
            &ops,
            "PATCH",
            "/admin/v1/deploy/tenants/nope",
            Some(json!({"name": "x"}))
        )
        .await
        .0,
        404
    );
    assert_eq!(
        a(
            &ops,
            "PATCH",
            "/admin/v1/deploy/tenants/newco",
            Some(json!({}))
        )
        .await
        .0,
        400
    );
    a(
        &ops,
        "PATCH",
        "/admin/v1/deploy/tenants/newco",
        Some(json!({"suspended": false})),
    )
    .await;
    assert_eq!(put(host.clone()).await, 200, "zoe exists now");
    // Usage for a day: the date must be YYYY-MM-DD; unknown tenants 404.
    for bad in ["2026-13-01", "yesterday", "2026-1-1", ""] {
        let (s, v) = a(
            &ops,
            "GET",
            &format!("/admin/v1/deploy/tenants/newco/usage?day={bad}"),
            None,
        )
        .await;
        assert_eq!(
            (s, v["rule"].as_str()),
            (400, Some("tenant.day")),
            "{bad}: {v}"
        );
    }
    let (s, v) = a(
        &ops,
        "GET",
        "/admin/v1/deploy/tenants/newco/usage?day=2026-11-03",
        None,
    )
    .await;
    assert_eq!((s, v["messages"].as_i64()), (200, Some(0)), "{v}");
    let (s, v) = a(
        &ops,
        "GET",
        "/admin/v1/deploy/tenants/nope/usage?day=2026-11-03",
        None,
    )
    .await;
    assert_eq!(
        (s, v["rule"].as_str()),
        (404, Some("tenant.unknown")),
        "{v}"
    );
    // Deleted over the API: gone; only the deployment's roles may.
    assert_eq!(
        a(&dev, "DELETE", "/admin/v1/deploy/tenants/newco", None)
            .await
            .0,
        403
    );
    assert_eq!(
        a(&ops, "DELETE", "/admin/v1/deploy/tenants/newco", None)
            .await
            .0,
        200
    );
    assert_eq!(
        a(&ops, "DELETE", "/admin/v1/deploy/tenants/newco", None)
            .await
            .0,
        404
    );
    assert_eq!(put(host).await, 404);
    cleanup(&hub, &["acme"]).await;
}

/// A tenant's webhooks over the API: deployment roles only, the tenant must
/// exist, refusals leave nothing, the secret never comes back, changes are
/// audited, and a webhook is only its tenant's.
async fn the_deployment_api_manages_tenant_webhooks(db: TestDb) {
    let hub = open(cfg(&db)).await;
    for t in ["acme", "globex"] {
        hub.create_tenant(t, t, json!({})).await.unwrap();
    }
    let addr = serve(&hub).await;
    let (ops, _) = hub.create_admin_key("cloud", "operator").await.unwrap();
    let (dev, _) = hub.create_admin_key("dev", "developer").await.unwrap();
    let path = |t: &str, n: &str| format!("/admin/v1/deploy/tenants/{t}/webhooks/{n}");
    let hook = json!({"url": "https://hooks.example.com/im", "secret": "acme-secret-0123456789", "events": ["msg.*"]});
    assert_eq!(
        http(
            &addr,
            None,
            &dev,
            "PUT",
            &path("acme", "main"),
            Some(hook.clone())
        )
        .await
        .0,
        403
    );
    for (t, n, body, code, rule) in [
        ("nope", "main", hook.clone(), 404, Some("tenant.unknown")),
        ("acme", "bad name", hook.clone(), 400, Some("webhook.name")),
        (
            "acme",
            "main",
            json!({"url": "https://10.0.0.1/x", "secret": "acme-secret-0123456789"}),
            400,
            Some("webhook.url"),
        ),
        (
            "acme",
            "main",
            json!({"url": "https://hooks.example.com/im", "secret": "short"}),
            400,
            Some("webhook.secret"),
        ),
        (
            "acme",
            "main",
            json!({"url": "https://hooks.example.com/im", "secret": "acme-secret-0123456789", "extra": 1}),
            400,
            None,
        ),
    ] {
        let (s, v) = http(&addr, None, &ops, "PUT", &path(t, n), Some(body.clone())).await;
        assert_eq!(s, code, "{t}/{n} {body}: {v}");
        if let Some(r) = rule {
            assert_eq!(v["rule"], r, "{v}");
        }
    }
    let list = |t: &str| {
        let (addr, ops) = (addr.clone(), ops.clone());
        let p = format!("/admin/v1/deploy/tenants/{t}/webhooks");
        async move { http(&addr, None, &ops, "GET", &p, None).await }
    };
    assert_eq!(list("acme").await.1["webhooks"], json!([]), "no trace");
    assert_eq!(list("nope").await.0, 404);
    let (s, v) = http(
        &addr,
        None,
        &ops,
        "PUT",
        &path("acme", "main"),
        Some(hook.clone()),
    )
    .await;
    assert_eq!((s, &v["events"]), (200, &json!(["msg.*"])), "{v}");
    assert!(v.get("secret").is_none());
    let (_, l) = list("acme").await;
    assert_eq!(l["webhooks"].as_array().unwrap().len(), 1);
    assert!(!l.to_string().contains("acme-secret"), "{l}");
    assert_eq!(
        list("globex").await.1["webhooks"],
        json!([]),
        "not globex's"
    );
    let (s, v) = http(&addr, None, &ops, "DELETE", &path("globex", "main"), None).await;
    assert_eq!((s, v["rule"].as_str()), (404, Some("webhook.unknown")));
    assert_eq!(
        http(&addr, None, &ops, "DELETE", &path("acme", "main"), None)
            .await
            .0,
        200
    );
    assert_eq!(
        http(&addr, None, &ops, "DELETE", &path("acme", "main"), None)
            .await
            .0,
        404
    );
    let audit = hub
        .admin_audit(Some("admin.tenants.webhooks"), None, 50)
        .await
        .unwrap();
    let actions: Vec<&str> = audit["records"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|r| r["action"].as_str())
        .collect();
    assert_eq!(
        (
            actions
                .iter()
                .filter(|a| **a == "admin.tenants.webhooks.put")
                .count(),
            actions
                .iter()
                .filter(|a| **a == "admin.tenants.webhooks.delete")
                .count()
        ),
        (1, 1),
        "only the changes that happened: {actions:?}"
    );
    cleanup(&hub, &["acme", "globex"]).await;
}

/// Without tenancy the tenants API answers `409 tenant.disabled`.
async fn a_single_tenant_server_has_no_tenants_api(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let addr = serve(&hub).await;
    let (ops, _) = hub.create_admin_key("ops", "operator").await.unwrap();
    for (m, p, b) in [
        ("GET", "/admin/v1/deploy/tenants", None),
        (
            "POST",
            "/admin/v1/deploy/tenants",
            Some(json!({"id": "x", "name": "x"})),
        ),
        (
            "POST",
            "/admin/v1/deploy/tenants/x/api-keys",
            Some(json!({"name": "k"})),
        ),
        ("DELETE", "/admin/v1/deploy/tenants/x", None),
        ("GET", "/admin/v1/deploy/tenants/x/webhooks", None),
        (
            "PUT",
            "/admin/v1/deploy/tenants/x/webhooks/main",
            Some(json!({"url": "https://example.com/h", "secret": "0123456789abcdef"})),
        ),
    ] {
        let (s, v) = http(&addr, None, &ops, m, p, b).await;
        assert_eq!(
            (s, v["rule"].as_str()),
            (409, Some("tenant.disabled")),
            "{m} {p}: {v}"
        );
    }
    assert_eq!(
        hub.tenant_for_host(Some("anything.example")).as_deref(),
        Some(agentim_core::tenant::DEFAULT)
    );
}
