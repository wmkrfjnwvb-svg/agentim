//! Webhooks of each tenant (docs/en/design/multi-tenancy.md), stored and delivered in the
//! tenant's own database: each tenant receives only its own events,
//! signed with its own secret; one tenant's failing receiver does not delay
//! another's; new webhooks start
//! at the end; the receiver can turn a webhook off, and one failing too long
//! goes off; private addresses are refused when set up and when sending;
//! every refusal of the admin operations leaves no trace; the per-tenant
//! limit holds under concurrent creation; the outbox is cleaned per tenant.
//! Every backend.

mod common;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use agentim_core::config::{Config, TenancyConfig, TenantCluster};
use agentim_core::hub::{Hub, TenantHookReq};
use agentim_core::model::*;
use axum::{Router, http::HeaderMap, http::StatusCode, routing::post};
use common::*;
use serde_json::{Value, json};

backend_tests!(
    each_tenant_receives_only_its_own_events,
    a_failing_receiver_delays_only_its_tenant,
    a_new_webhook_starts_at_the_end,
    the_receiver_can_turn_a_webhook_off,
    a_webhook_failing_too_long_goes_off,
    private_addresses_are_refused,
    refused_changes_leave_no_trace,
    race_creations_for_the_last_places,
    the_outbox_is_cleaned_per_tenant,
    a_suspended_tenant_gets_nothing_until_resumed,
);

const ACME_SECRET: &str = "acme-secret-0123456789";
const GLOBEX_SECRET: &str = "globex-secret-0123456789";

#[derive(Clone, Default)]
struct Receiver {
    got: Arc<Mutex<Vec<(HeaderMap, String)>>>,
    down: Arc<Mutex<bool>>,
    /// Answer 200 with `webhook-delivery: disable`.
    stop: Arc<Mutex<bool>>,
}

impl Receiver {
    async fn start() -> (Receiver, String) {
        let r = Receiver::default();
        let st = r.clone();
        let app = Router::new().route(
            "/hook",
            post(move |h: HeaderMap, body: String| {
                let st = st.clone();
                async move { st.handle(h, body) }
            }),
        );
        let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = tcp.local_addr().unwrap().to_string();
        tokio::spawn(async move { axum::serve(tcp, app).await.unwrap() });
        (r, format!("http://{addr}/hook"))
    }

    fn handle(&self, h: HeaderMap, body: String) -> (StatusCode, HeaderMap) {
        let mut out = HeaderMap::new();
        if *self.down.lock().unwrap() {
            return (StatusCode::SERVICE_UNAVAILABLE, out);
        }
        if *self.stop.lock().unwrap() {
            out.insert("webhook-delivery", "disable".parse().unwrap());
            return (StatusCode::OK, out);
        }
        self.got.lock().unwrap().push((h, body));
        (StatusCode::NO_CONTENT, out)
    }

    fn bodies(&self) -> Vec<Value> {
        self.got
            .lock()
            .unwrap()
            .iter()
            .map(|(_, b)| serde_json::from_str(b).unwrap())
            .collect()
    }

    fn count(&self) -> usize {
        self.got.lock().unwrap().len()
    }
}

/// Tenants' databases next to the test's.
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
        host_suffix: "im.test".into(),
        webhook_private_addresses: true,
        webhook_disable_after_ms: 1_500,
        webhooks_per_tenant: 3,
        clusters: vec![TenantCluster {
            name: "free".into(),
            url: cluster_url(db),
            read_pool: 2,
        }],
    };
    c
}

async fn world(db: &TestDb) -> Hub {
    let hub = open(cfg(db)).await;
    for t in ["acme", "globex"] {
        hub.create_tenant(t, t, json!({})).await.unwrap();
    }
    hub
}

fn req(url: &str, secret: &str) -> TenantHookReq {
    serde_json::from_value(json!({"url": url, "secret": secret, "max_attempts": 20, "backoff_ms": 50, "timeout_ms": 2000})).unwrap()
}

/// The tenant's own instance.
async fn inst(hub: &Hub, tenant: &str) -> Hub {
    hub.tenant_hub(tenant).await.unwrap()
}

/// Drops the tenants' databases a PostgreSQL or MySQL test made.
async fn cleanup(hub: &Hub) {
    for t in ["acme", "globex"] {
        let _ = hub.drop_tenant_database(t).await;
    }
}

/// A group of two in a tenant's instance, and a way to send in it.
async fn group(hub: &Hub, tenant: &str) -> (Conn, String) {
    let h = inst(hub, tenant).await;
    let a = user(&h, "alice", UserKind::Human, None).await;
    let b = user(&h, "bob", UserKind::Human, None).await;
    let c = Conn::new(&h, &a);
    let conv = c.group(&[&b]).await;
    (c, conv)
}

/// A tenant's webhooks as the deployment lists them.
async fn hooks(hub: &Hub, tenant: &str) -> Vec<Value> {
    hub.tenant_webhooks_of(tenant).await.unwrap()
}

/// Waits until the tenant's webhook is off (its instance sees the change
/// at its next refresh).
async fn until_off(hub: &Hub, tenant: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let _ = inst(hub, tenant).await.refresh_auth().await;
        if hooks(hub, tenant).await[0]["enabled"] == false {
            return;
        }
        assert!(Instant::now() < deadline, "turned off");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn until(what: &str, within: Duration, mut f: impl FnMut() -> bool) {
    let deadline = Instant::now() + within;
    while !f() {
        assert!(Instant::now() < deadline, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn texts(bodies: &[Value]) -> Vec<String> {
    bodies
        .iter()
        .filter(|b| b["type"] == "msg.created")
        .filter_map(|b| {
            b["data"]["event"]["msg"]["content"]
                .as_str()
                .map(str::to_string)
        })
        .collect()
}

fn signed(h: &HeaderMap, body: &str, secret: &str) -> bool {
    let ts: i64 = h["webhook-timestamp"].to_str().unwrap().parse().unwrap();
    let id = h["webhook-id"].to_str().unwrap();
    h["webhook-signature"].to_str().unwrap() == agentim_core::webhook::sign(secret, id, ts, body)
}

/// Same names in two tenants; each tenant's webhook gets only its tenant's
/// events (filtered by its own patterns), without the separator, named,
/// signed with its own secret; the deployment's webhook gets both.
async fn each_tenant_receives_only_its_own_events(db: TestDb) {
    let hub = world(&db).await;
    let (acme, acme_url) = Receiver::start().await;
    let (globex, globex_url) = Receiver::start().await;
    let mut only_msgs = req(&globex_url, GLOBEX_SECRET);
    only_msgs.events = Some(vec!["msg.created".into()]);
    hub.put_tenant_webhook("acme", "main", req(&acme_url, ACME_SECRET))
        .await
        .unwrap();
    hub.put_tenant_webhook("globex", "main", only_msgs)
        .await
        .unwrap();
    let (ca, conv_a) = group(&hub, "acme").await;
    let (cg, conv_g) = group(&hub, "globex").await;
    for i in 0..3 {
        ca.ok(json!({"op": "send", "conv_id": conv_a, "client_msg_id": format!("a{i}"), "text": format!("acme {i}")}))
            .await;
        cg.ok(json!({"op": "send", "conv_id": conv_g, "client_msg_id": format!("g{i}"), "text": format!("globex {i}")}))
            .await;
    }
    until("acme's three", Duration::from_secs(10), || {
        texts(&acme.bodies()).len() == 3
    })
    .await;
    until("globex's three", Duration::from_secs(10), || {
        texts(&globex.bodies()).len() == 3
    })
    .await;
    assert_eq!(
        texts(&acme.bodies()),
        ["acme 0", "acme 1", "acme 2"],
        "in order"
    );
    assert_eq!(
        texts(&globex.bodies()),
        ["globex 0", "globex 1", "globex 2"]
    );
    assert!(
        acme.bodies().iter().any(|b| b["type"] == "conv.created"),
        "acme takes every type"
    );
    assert!(
        globex.bodies().iter().all(|b| b["type"] == "msg.created"),
        "globex only msg.created"
    );
    for (r, _tenant, secret, conv) in [
        (&acme, "acme", ACME_SECRET, &conv_a),
        (&globex, "globex", GLOBEX_SECRET, &conv_g),
    ] {
        for (h, body) in r.got.lock().unwrap().iter() {
            assert!(
                !body.contains("\\u001f") && !body.contains('\u{1f}'),
                "{body}"
            );
            assert!(signed(h, body, secret), "signed with the tenant's secret");
            let v: Value = serde_json::from_str(body).unwrap();
            assert_eq!(v["data"]["conv_id"], conv.as_str(), "only its conversation");
        }
    }
    // The list never shows the secret.
    let list = json!(hooks(&hub, "acme").await).to_string();
    assert!(
        list.contains(&acme_url) && !list.contains(ACME_SECRET),
        "{list}"
    );
    cleanup(&hub).await;
}

/// Acme's receiver is down; globex's events still arrive at once; when
/// acme's comes back it gets everything, in order.
async fn a_failing_receiver_delays_only_its_tenant(db: TestDb) {
    let hub = world(&db).await;
    let (acme, acme_url) = Receiver::start().await;
    let (globex, globex_url) = Receiver::start().await;
    *acme.down.lock().unwrap() = true;
    hub.put_tenant_webhook("acme", "main", req(&acme_url, ACME_SECRET))
        .await
        .unwrap();
    hub.put_tenant_webhook("globex", "main", req(&globex_url, GLOBEX_SECRET))
        .await
        .unwrap();
    let (ca, conv_a) = group(&hub, "acme").await;
    let (cg, conv_g) = group(&hub, "globex").await;
    for i in 0..5 {
        ca.send(&conv_a, &format!("a{i}")).await;
        let t = Instant::now();
        cg.send(&conv_g, &format!("g{i}")).await;
        until("globex in time", Duration::from_secs(3), || {
            texts(&globex.bodies()).len() == i + 1
        })
        .await;
        assert!(t.elapsed() < Duration::from_secs(3));
    }
    assert!(texts(&acme.bodies()).is_empty());
    *acme.down.lock().unwrap() = false;
    until("acme catches up", Duration::from_secs(10), || {
        texts(&acme.bodies()).len() == 5
    })
    .await;
    assert_eq!(texts(&acme.bodies()), ["a0", "a1", "a2", "a3", "a4"]);
    cleanup(&hub).await;
}

/// Events before a webhook existed are not replayed to it.
async fn a_new_webhook_starts_at_the_end(db: TestDb) {
    let hub = world(&db).await;
    let (ca, conv) = group(&hub, "acme").await;
    ca.send(&conv, "before").await;
    let (acme, url) = Receiver::start().await;
    hub.put_tenant_webhook("acme", "main", req(&url, ACME_SECRET))
        .await
        .unwrap();
    // The deliverer may take a moment to start at the end.
    tokio::time::sleep(Duration::from_millis(300)).await;
    ca.send(&conv, "after").await;
    until("after", Duration::from_secs(10), || {
        texts(&acme.bodies()) == ["after"]
    })
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(texts(&acme.bodies()), ["after"], "nothing from before");
    cleanup(&hub).await;
}

async fn audits(hub: &Hub, action: &str) -> usize {
    hub.admin_audit(Some(action), None, 100).await.unwrap()["records"]
        .as_array()
        .map_or(0, |a| a.iter().filter(|r| r["action"] == action).count())
}

/// `webhook-delivery: disable` turns the webhook off for good (audited);
/// later events are not sent.
async fn the_receiver_can_turn_a_webhook_off(db: TestDb) {
    let hub = world(&db).await;
    let (acme, url) = Receiver::start().await;
    *acme.stop.lock().unwrap() = true;
    hub.put_tenant_webhook("acme", "main", req(&url, ACME_SECRET))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let (ca, conv) = group(&hub, "acme").await;
    ca.send(&conv, "one").await;
    until_off(&hub, "acme").await;
    assert_eq!(
        audits(&inst(&hub, "acme").await, "webhook.disabled").await,
        1
    );
    *acme.stop.lock().unwrap() = false;
    ca.send(&conv, "two").await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(acme.count(), 0, "nothing after it went off");
    cleanup(&hub).await;
}

/// Failing for longer than `webhook_disable_after_ms` (1.5 s here) turns it
/// off; another tenant's keeps working.
async fn a_webhook_failing_too_long_goes_off(db: TestDb) {
    let hub = world(&db).await;
    let (acme, url) = Receiver::start().await;
    let (globex, gurl) = Receiver::start().await;
    *acme.down.lock().unwrap() = true;
    hub.put_tenant_webhook("acme", "main", req(&url, ACME_SECRET))
        .await
        .unwrap();
    hub.put_tenant_webhook("globex", "main", req(&gurl, GLOBEX_SECRET))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let (ca, conv) = group(&hub, "acme").await;
    ca.send(&conv, "lost cause").await;
    until_off(&hub, "acme").await;
    assert_eq!(hooks(&hub, "globex").await[0]["enabled"], true);
    let (cg, gconv) = group(&hub, "globex").await;
    cg.send(&gconv, "fine").await;
    until("globex", Duration::from_secs(10), || {
        texts(&globex.bodies()) == ["fine"]
    })
    .await;
    // Turned on again by the deployment, it continues.
    *acme.down.lock().unwrap() = false;
    hub.put_tenant_webhook("acme", "main", req(&url, ACME_SECRET))
        .await
        .unwrap();
    ca.send(&conv, "back").await;
    until("acme again", Duration::from_secs(10), || {
        texts(&acme.bodies()).contains(&"back".to_string())
    })
    .await;
    cleanup(&hub).await;
}

/// Without `webhook_private_addresses`: http, loopback, private, link-local
/// (cloud metadata), localhost and unique-local addresses are refused when
/// set up; a host that resolves to one is refused when sending.
async fn private_addresses_are_refused(db: TestDb) {
    let mut c = cfg(&db);
    c.tenancy.webhook_private_addresses = false;
    let hub = open(c).await;
    hub.create_tenant("acme", "acme", json!({})).await.unwrap();
    for url in [
        "http://example.com/hook",
        "https://127.0.0.1/hook",
        "https://10.1.2.3/hook",
        "https://172.16.0.1/hook",
        "https://192.168.1.1/hook",
        "https://169.254.169.254/latest/meta-data",
        "https://100.64.0.1/hook",
        "https://0.0.0.0/hook",
        "https://[::1]/hook",
        "https://[fd00::1]/hook",
        "https://[fe80::1]/hook",
        "https://[::ffff:127.0.0.1]/hook",
        "https://localhost/hook",
        "https://a.localhost/hook",
        "https://user:pw@example.com/hook",
        "ftp://example.com/hook",
        "not a url",
    ] {
        let e = hub
            .put_tenant_webhook("acme", "main", req(url, ACME_SECRET))
            .await
            .unwrap_err();
        assert_eq!(
            (e.code, e.rule),
            ("bad_request", Some("webhook.url")),
            "{url}"
        );
    }
    assert!(hooks(&hub, "acme").await.is_empty(), "nothing stored");
    // A host that does not resolve fails at sending and goes off.
    let (r, url) = Receiver::start().await;
    let port = url.rsplit(':').next().unwrap().trim_end_matches("/hook");
    hub.put_tenant_webhook(
        "acme",
        "main",
        req(&format!("https://nowhere.invalid:{port}/hook"), ACME_SECRET),
    )
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let (ca, conv) = group(&hub, "acme").await;
    ca.send(&conv, "inside").await;
    until_off(&hub, "acme").await;
    assert_eq!(r.count(), 0, "nothing arrived");
    // The check at sending, on resolved addresses: one private answer
    // among public ones is enough to refuse.
    let public: std::net::SocketAddr = "93.184.216.34:443".parse().unwrap();
    for bad in [
        "127.0.0.1:443",
        "10.0.0.7:443",
        "169.254.169.254:80",
        "[::1]:443",
    ] {
        let e = agentim_core::webhook::allowed_addresses(&[public, bad.parse().unwrap()], false)
            .unwrap_err();
        assert!(e.starts_with("webhook.address"), "{bad}: {e}");
        assert!(agentim_core::webhook::allowed_addresses(&[bad.parse().unwrap()], true).is_ok());
    }
    assert!(agentim_core::webhook::allowed_addresses(&[public], false).is_ok());
    assert!(agentim_core::webhook::blocked_ip(
        "169.254.169.254".parse().unwrap()
    ));
    assert!(!agentim_core::webhook::blocked_ip(
        "93.184.216.34".parse().unwrap()
    ));
    assert!(!agentim_core::webhook::blocked_ip(
        "2606:4700::1111".parse().unwrap()
    ));
    cleanup(&hub).await;
}

/// Every refusal answers its rule and changes nothing.
async fn refused_changes_leave_no_trace(db: TestDb) {
    let hub = world(&db).await;
    let (_r, url) = Receiver::start().await;
    hub.put_tenant_webhook("acme", "keep", req(&url, ACME_SECRET))
        .await
        .unwrap();
    let before = json!(hooks(&hub, "acme").await);
    let with = |f: &dyn Fn(&mut Value)| {
        let mut v = json!({"url": url, "secret": ACME_SECRET});
        f(&mut v);
        serde_json::from_value::<TenantHookReq>(v).unwrap()
    };
    let cases: Vec<(&str, TenantHookReq, &str)> = vec![
        ("bad name", req(&url, ACME_SECRET), "webhook.name"),
        (
            "ok",
            with(&|v| v["secret"] = json!("short")),
            "webhook.secret",
        ),
        (
            "ok",
            with(&|v| v["secret"] = json!("whsec_!!!")),
            "webhook.secret",
        ),
        ("ok", with(&|v| v["events"] = json!([])), "webhook.events"),
        (
            "ok",
            with(&|v| v["events"] = json!(["Msg.*"])),
            "webhook.events",
        ),
        (
            "ok",
            with(&|v| v["max_attempts"] = json!(0)),
            "webhook.retries",
        ),
        (
            "ok",
            with(&|v| v["max_attempts"] = json!(21)),
            "webhook.retries",
        ),
        (
            "ok",
            with(&|v| v["timeout_ms"] = json!(99)),
            "webhook.retries",
        ),
        (
            "ok",
            with(&|v| v["backoff_ms"] = json!(300_001)),
            "webhook.retries",
        ),
    ];
    for (name, r, rule) in cases {
        let e = hub.put_tenant_webhook("acme", name, r).await.unwrap_err();
        assert_eq!(
            (e.code, e.rule),
            ("bad_request", Some(rule)),
            "{name} {rule}"
        );
    }
    let long = "x".repeat(65);
    let e = hub
        .put_tenant_webhook("acme", &long, req(&url, ACME_SECRET))
        .await
        .unwrap_err();
    assert_eq!(e.rule, Some("webhook.name"));
    let e = hub.delete_tenant_webhook("acme", "nope").await.unwrap_err();
    assert_eq!((e.code, e.rule), ("not_found", Some("webhook.unknown")));
    // Another tenant's webhook of the same name is not this one's.
    let e = hub
        .delete_tenant_webhook("globex", "keep")
        .await
        .unwrap_err();
    assert_eq!(e.code, "not_found");
    assert_eq!(json!(hooks(&hub, "acme").await), before, "no trace");
    assert!(hooks(&hub, "globex").await.is_empty());
    // Replacing one keeps its place and creation time; deleting it twice.
    let created = before[0]["created_at"].clone();
    let mut r = req(&url, ACME_SECRET);
    r.events = Some(vec!["msg.*".into()]);
    hub.put_tenant_webhook("acme", "keep", r).await.unwrap();
    let now = hooks(&hub, "acme").await;
    assert_eq!(
        (now.len(), &now[0]["created_at"], &now[0]["events"]),
        (1, &created, &json!(["msg.*"]))
    );
    hub.delete_tenant_webhook("acme", "keep").await.unwrap();
    assert_eq!(
        hub.delete_tenant_webhook("acme", "keep")
            .await
            .unwrap_err()
            .code,
        "not_found"
    );
    assert!(hooks(&hub, "acme").await.is_empty());
    cleanup(&hub).await;
}

/// Ten creations of different names race for a tenant's three places
/// (5 rounds): exactly three exist, a replacement of one of them always
/// succeeds.
async fn race_creations_for_the_last_places(db: TestDb) {
    let hub = world(&db).await;
    let (_r, url) = Receiver::start().await;
    for round in 0..5 {
        let mut tasks = vec![];
        for i in 0..10 {
            let (hub, url) = (hub.clone(), url.clone());
            tasks.push(tokio::spawn(async move {
                hub.put_tenant_webhook("acme", &format!("h{round}-{i}"), req(&url, ACME_SECRET))
                    .await
            }));
        }
        let mut won = 0;
        for t in tasks {
            match t.await.unwrap() {
                Ok(_) => won += 1,
                Err(e) => assert_eq!((e.code, e.rule), ("conflict", Some("webhook.limit"))),
            }
        }
        assert_eq!(won, 3, "round {round}");
        let list = hooks(&hub, "acme").await;
        assert_eq!(list.len(), 3);
        let name = list[0]["name"].as_str().unwrap().to_string();
        hub.put_tenant_webhook("acme", &name, req(&url, ACME_SECRET))
            .await
            .expect("a replacement takes no new place");
        for h in list {
            hub.delete_tenant_webhook("acme", h["name"].as_str().unwrap())
                .await
                .unwrap();
        }
    }
    cleanup(&hub).await;
}

async fn queued(hub: &Hub) -> i64 {
    let mut n = 0;
    for t in ["acme", "globex"] {
        n += inst(hub, t).await.admin_webhooks().await.unwrap()["queued"]
            .as_i64()
            .unwrap();
    }
    n
}

/// Rows delivered to their tenant go; rows another tenant's receiver still
/// waits for stay until it gets them.
async fn the_outbox_is_cleaned_per_tenant(db: TestDb) {
    let hub = world(&db).await;
    let (acme, url) = Receiver::start().await;
    let (globex, gurl) = Receiver::start().await;
    *globex.down.lock().unwrap() = true;
    hub.put_tenant_webhook("acme", "main", req(&url, ACME_SECRET))
        .await
        .unwrap();
    hub.put_tenant_webhook("globex", "main", req(&gurl, GLOBEX_SECRET))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let (ca, conv) = group(&hub, "acme").await;
    let (cg, gconv) = group(&hub, "globex").await;
    for i in 0..3 {
        ca.send(&conv, &format!("a{i}")).await;
        cg.send(&gconv, &format!("g{i}")).await;
    }
    until("acme's", Duration::from_secs(10), || {
        texts(&acme.bodies()).len() == 3
    })
    .await;
    // Globex's rows (as many as acme received: the same conversation's
    // events) stay; acme's go.
    let globex_rows = acme.count() as i64;
    let deadline = Instant::now() + Duration::from_secs(10);
    while queued(&hub).await != globex_rows {
        assert!(
            Instant::now() < deadline,
            "acme's rows cleaned: {}",
            queued(&hub).await
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    *globex.down.lock().unwrap() = false;
    until("globex's", Duration::from_secs(10), || {
        texts(&globex.bodies()).len() == 3
    })
    .await;
    let deadline = Instant::now() + Duration::from_secs(10);
    while queued(&hub).await != 0 {
        assert!(Instant::now() < deadline, "all cleaned");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    cleanup(&hub).await;
}

/// A suspended tenant's webhook stops; resumed, it gets what was queued.
async fn a_suspended_tenant_gets_nothing_until_resumed(db: TestDb) {
    let hub = world(&db).await;
    let (acme, url) = Receiver::start().await;
    hub.put_tenant_webhook("acme", "main", req(&url, ACME_SECRET))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let (ca, conv) = group(&hub, "acme").await;
    ca.send(&conv, "one").await;
    until("one", Duration::from_secs(10), || {
        texts(&acme.bodies()) == ["one"]
    })
    .await;
    // Queued while the receiver is down, then the tenant is suspended (its
    // users can no longer write).
    *acme.down.lock().unwrap() = true;
    assert_eq!(ca.send(&conv, "two").await["ok"], true);
    hub.update_tenant("acme", None, None, Some(true))
        .await
        .unwrap();
    *acme.down.lock().unwrap() = false;
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert_eq!(texts(&acme.bodies()), ["one"], "nothing while suspended");
    hub.update_tenant("acme", None, None, Some(false))
        .await
        .unwrap();
    until("two after resuming", Duration::from_secs(10), || {
        texts(&acme.bodies()) == ["one", "two"]
    })
    .await;
    cleanup(&hub).await;
}
