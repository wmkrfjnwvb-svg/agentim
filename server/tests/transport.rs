//! Transport security (docs/04-security-compliance.md §1): the Origin check
//! on browser-facing endpoints and built-in TLS. The certificate in
//! tests/fixtures is a self-signed test certificate for localhost only.

mod common;

use std::time::Duration;

use agentim::config::{Config, TlsConfig, sqlite_url};
use agentim::hub::Hub;
use agentim::model::UserKind;
use agentim::server::{AppState, router};
use common::*;
use tokio::io::AsyncWriteExt;

const CERT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/test-cert.pem");
const KEY: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/test-key.pem");
const CA: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/test-ca.pem");
const OTHER_KEY: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/other-key.pem");

fn cfg(dir: &tempfile::TempDir) -> Config {
    single(&sqlite_url(&dir.path().join("db").to_string_lossy()))
}

async fn plain(cfg: Config, demo: bool) -> (String, Hub) {
    let hub = Hub::open(cfg, hub_cfg()).await.unwrap();
    let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = tcp.local_addr().unwrap().to_string();
    let app = router(AppState {
        hub: hub.clone(),
        demo_login: demo,
        metrics_token: None,
    });
    tokio::spawn(async move {
        axum::serve(
            tcp,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap()
    });
    (addr, hub)
}

async fn ws_status(url: &str, origin: Option<&str>) -> u16 {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let mut req = url.into_client_request().unwrap();
    if let Some(o) = origin {
        req.headers_mut().insert("origin", o.parse().unwrap());
    }
    match tokio_tungstenite::connect_async(req).await {
        Ok(_) => 101,
        Err(tokio_tungstenite::tungstenite::Error::Http(r)) => r.status().as_u16(),
        Err(e) => panic!("{e}"),
    }
}

#[tokio::test]
async fn cross_site_browser_pages_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = cfg(&dir);
    c.server.allowed_origins = vec!["https://app.example.com".into()];
    let (addr, hub) = plain(c, true).await;
    let (_, token) = hub
        .create_user("alice", UserKind::Human, None, None)
        .await
        .unwrap();
    let url = format!("ws://{addr}/ws?token={token}");
    let same = format!("http://{addr}");
    for (origin, want) in [
        (None, 101),
        (Some(same.as_str()), 101),
        (Some("https://app.example.com"), 101),
        (Some("HTTPS://APP.EXAMPLE.COM"), 101),
        (Some("https://evil.example"), 403),
        (Some("https://app.example.com.evil.example"), 403),
        (Some("null"), 403),
    ] {
        assert_eq!(ws_status(&url, origin).await, want, "{origin:?}");
    }
    // Login is browser-facing too.
    let http = reqwest::Client::new();
    let login = |origin: Option<&'static str>| {
        let mut r = http
            .post(format!("http://{addr}/api/login"))
            .header("content-type", "application/json")
            .body(r#"{"name":"alice"}"#);
        if let Some(o) = origin {
            r = r.header("origin", o);
        }
        r.send()
    };
    assert_eq!(login(None).await.unwrap().status().as_u16(), 200);
    assert_eq!(
        login(Some("https://evil.example"))
            .await
            .unwrap()
            .status()
            .as_u16(),
        403
    );
    // A refused page created no device.
    let alice = hub.user_by_name("alice").unwrap();
    assert_eq!(
        hub.devices(&alice.id).await.unwrap().len(),
        2,
        "first device + one login"
    );
}

#[test]
fn origin_settings_are_checked() {
    let dir = tempfile::tempdir().unwrap();
    for bad in [
        "app.example.com",
        "https://",
        "https://x/path",
        "ftp://x",
        "https://a b",
    ] {
        let mut c = cfg(&dir);
        c.server.allowed_origins = vec![bad.into()];
        assert!(c.validate().is_err(), "{bad}");
    }
    let mut c = cfg(&dir);
    c.server.allowed_origins = vec![
        "https://app.example.com:8443".into(),
        "http://[::1]:3000".into(),
    ];
    assert!(c.validate().is_ok());
}

#[test]
fn certificates_are_checked_at_startup() {
    assert!(agentim::tls::server_config(CERT, KEY).is_ok());
    let e = agentim::tls::server_config(CERT, OTHER_KEY).unwrap_err();
    assert!(e.contains("do not match"), "{e}");
    assert!(agentim::tls::server_config("/nonexistent/cert.pem", KEY).is_err());
    assert!(
        agentim::tls::server_config(KEY, KEY)
            .unwrap_err()
            .contains("no certificate")
    );
}

#[tokio::test]
async fn the_server_speaks_tls_itself() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let dir = tempfile::tempdir().unwrap();
    let mut c = cfg(&dir);
    c.server.tls = Some(TlsConfig {
        cert: CERT.into(),
        key: KEY.into(),
    });
    let hub = Hub::open(c, hub_cfg()).await.unwrap();
    let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = tcp.local_addr().unwrap();
    let l = agentim::tls::TlsListener::new(tcp, agentim::tls::server_config(CERT, KEY).unwrap())
        .unwrap();
    let app = router(AppState {
        hub: hub.clone(),
        demo_login: false,
        metrics_token: None,
    });
    tokio::spawn(async move {
        axum::serve(
            l,
            app.into_make_service_with_connect_info::<agentim::tls::PeerAddr>(),
        )
        .await
        .unwrap()
    });
    // A client that connects and never handshakes does not block others.
    let mut silent = tokio::net::TcpStream::connect(addr).await.unwrap();
    silent.write_all(b"").await.unwrap();
    let ca = reqwest::Certificate::from_pem(&std::fs::read(CA).unwrap()).unwrap();
    let https = reqwest::Client::builder()
        .add_root_certificate(ca)
        .build()
        .unwrap();
    let url = format!("https://localhost:{}", addr.port());
    let r = tokio::time::timeout(
        Duration::from_secs(5),
        https.get(format!("{url}/healthz")).send(),
    )
    .await
    .expect("not blocked by the silent client")
    .unwrap();
    assert_eq!(r.status().as_u16(), 200);
    // The open API reports the client address through TLS connections too.
    let scopes = vec!["users:read".to_string()];
    let (key, _) = hub
        .create_api_key("tls", &scopes, None, Some(vec!["127.0.0.1".into()]))
        .await
        .unwrap();
    hub.create_user("alice", UserKind::Human, None, None)
        .await
        .unwrap();
    let r = https
        .get(format!("{url}/v1/users/alice"))
        .bearer_auth(&key)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);
    let (far, _) = hub
        .create_api_key("far", &scopes, None, Some(vec!["10.0.0.0/8".into()]))
        .await
        .unwrap();
    let r = https
        .get(format!("{url}/v1/users/alice"))
        .bearer_auth(&far)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 403);
    // Plain HTTP to the TLS port gets nothing useful.
    let plain = reqwest::Client::new()
        .get(format!("http://127.0.0.1:{}/healthz", addr.port()))
        .timeout(Duration::from_secs(5))
        .send()
        .await;
    assert!(plain.is_err() || !plain.unwrap().status().is_success());
    // An untrusted certificate is rejected by a normal client.
    assert!(
        reqwest::Client::new()
            .get(format!("{url}/healthz"))
            .send()
            .await
            .is_err()
    );
    drop(silent);
}

#[tokio::test]
async fn password_endpoints_over_http() {
    let dir = tempfile::tempdir().unwrap();
    let (addr, hub) = plain(cfg(&dir), false).await;
    let (alice, _) = hub
        .create_user("alice", UserKind::Human, None, None)
        .await
        .unwrap();
    hub.reset_password(&alice, "correct horse battery")
        .await
        .unwrap();
    let http = reqwest::Client::new();
    let post = |path: &str, body: serde_json::Value, origin: Option<&'static str>| {
        let mut r = http
            .post(format!("http://{addr}{path}"))
            .header("content-type", "application/json")
            .body(body.to_string());
        if let Some(o) = origin {
            r = r.header("origin", o);
        }
        r.send()
    };
    let r = post(
        "/api/auth/login",
        serde_json::json!({"name": "alice", "password": "nope nope nope"}),
        None,
    )
    .await
    .unwrap();
    assert_eq!(r.status().as_u16(), 401);
    let v: serde_json::Value = serde_json::from_str(&r.text().await.unwrap()).unwrap();
    assert_eq!(v["error"]["code"], "invalid_credentials");
    let r = post("/api/auth/login", serde_json::json!({"name": "alice", "password": "correct horse battery", "device_name": "web"}), None).await.unwrap();
    assert_eq!(r.status().as_u16(), 200);
    let v: serde_json::Value = serde_json::from_str(&r.text().await.unwrap()).unwrap();
    assert!(hub.authenticate(v["token"].as_str().unwrap()).is_some());
    assert_eq!(v["device"]["name"], "web");
    assert!(!v.to_string().contains("hash"));
    let r = post(
        "/api/auth/login",
        serde_json::json!({"name": "alice", "password": "correct horse battery"}),
        Some("https://evil.example"),
    )
    .await
    .unwrap();
    assert_eq!(r.status().as_u16(), 403);
    let r = post(
        "/api/auth/register",
        serde_json::json!({"name": "bob", "password": "correct horse battery"}),
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        r.status().as_u16(),
        403,
        "registration is closed by default"
    );
    let r = post(
        "/api/auth/login",
        serde_json::json!({"name": "alice", "password": "x".repeat(2000)}),
        None,
    )
    .await
    .unwrap();
    assert_eq!(r.status().as_u16(), 400);
}
