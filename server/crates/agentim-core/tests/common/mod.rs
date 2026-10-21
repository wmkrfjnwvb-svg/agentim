//! Shared test helpers. Backends: SQLite always; PostgreSQL and MySQL when
//! `AGENTIM_TEST_PG` / `AGENTIM_TEST_MYSQL` hold a server URL without a
//! database name (e.g. `postgres://postgres@127.0.0.1`,
//! `mysql://agentim:agentim@127.0.0.1`). Each test gets a fresh database.
#![allow(dead_code)]

use std::time::Duration;

use agentim_core::config::{
    Config, DatabaseConfig, StorageConfig, UnitConfig, WriterConfig, sqlite_url,
};
use agentim_core::guard::GuardConfig;
use agentim_core::hub::{Hub, Session};
use agentim_core::logic::HubConfig;
use agentim_core::model::*;
use agentim_core::registry::Pushes;
use serde_json::{Value, json};
use sqlx::Connection;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Backend {
    Sqlite,
    Postgres,
    Mysql,
}

pub struct TestDb {
    pub url: String,
    pub path: Option<std::path::PathBuf>,
    _dir: Option<tempfile::TempDir>,
}

/// Server databases are dropped when the test ends, so local runs do not
/// fill the disk (CI starts from empty containers anyway).
impl Drop for TestDb {
    fn drop(&mut self) {
        if self.path.is_some() {
            return;
        }
        let url = self.url.clone();
        let Some((base, name)) = url.rsplit_once('/') else {
            return;
        };
        let (admin, sql) = if url.starts_with("postgres") {
            (
                format!("{base}/postgres"),
                format!("DROP DATABASE IF EXISTS {name} WITH (FORCE)"),
            )
        } else {
            (base.to_string(), format!("DROP DATABASE IF EXISTS {name}"))
        };
        let _ = std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async {
                if let Ok(mut c) = sqlx::AnyConnection::connect(&admin).await {
                    let _ = sqlx::query(sqlx::AssertSqlSafe(sql)).execute(&mut c).await;
                }
            });
        })
        .join();
    }
}

pub async fn new_db(b: Backend) -> Option<TestDb> {
    match b {
        Backend::Sqlite => {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("t.db");
            Some(TestDb {
                url: sqlite_url(&path.to_string_lossy()),
                path: Some(path),
                _dir: Some(dir),
            })
        }
        Backend::Postgres | Backend::Mysql => {
            let (var, admin_db) = if b == Backend::Postgres {
                ("AGENTIM_TEST_PG", "/postgres")
            } else {
                ("AGENTIM_TEST_MYSQL", "")
            };
            let base = std::env::var(var).ok()?;
            sqlx::any::install_default_drivers();
            let name = format!("t_{}", &uuid::Uuid::new_v4().simple().to_string()[..12]);
            let mut admin = sqlx::AnyConnection::connect(&format!("{base}{admin_db}"))
                .await
                .expect("admin connection");
            // PostgreSQL: a linguistic collation, as most installations have,
            // so tests catch code that assumes byte order.
            let icu = format!(
                "CREATE DATABASE {name} TEMPLATE template0 ENCODING 'UTF8' LOCALE_PROVIDER icu ICU_LOCALE 'en-US' LOCALE 'C.UTF-8'"
            );
            let created = admin_db == "/postgres"
                && sqlx::query(sqlx::AssertSqlSafe(icu))
                    .execute(&mut admin)
                    .await
                    .is_ok();
            if !created {
                sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {name}")))
                    .execute(&mut admin)
                    .await
                    .expect("create database");
            }
            Some(TestDb {
                url: format!("{base}/{name}"),
                path: None,
                _dir: None,
            })
        }
    }
}

pub fn hub_cfg() -> HubConfig {
    HubConfig {
        stream_idle: Duration::from_millis(200),
        checkpoint_every: Duration::from_millis(0),
        coalesce: Duration::from_millis(0),
        guards: GuardConfig {
            cooldown: Duration::from_secs(60),
            ..Default::default()
        },
        // Per-user limits are tested on their own; elsewhere they would
        // only slow tests that act quickly from many connections.
        sync: agentim_core::config::SyncConfig {
            rate: (1e6, 1e6),
            agent_rate: (1e6, 1e6),
            ..Default::default()
        },
        ..Default::default()
    }
}

/// Client limits generous enough for tests that send in bulk.
pub fn test_clients() -> agentim_core::config::ClientsConfig {
    agentim_core::config::ClientsConfig {
        send_rate: 1e6,
        send_burst: 1e6,
        ..Default::default()
    }
}

/// A fixed master key, so reopening a test database finds its content.
pub const TEST_MASTER_KEY: &str = "dGVzdC1tYXN0ZXIta2V5LWZvci1hZ2VudGltLXRlc3Q=";

pub fn test_encryption() -> agentim_core::config::EncryptionConfig {
    agentim_core::config::EncryptionConfig {
        key: Some(TEST_MASTER_KEY.into()),
        ..Default::default()
    }
}

pub fn single(url: &str) -> Config {
    Config {
        storage: StorageConfig {
            control: "main".into(),
            databases: vec![DatabaseConfig {
                name: "main".into(),
                url: url.into(),
                read_pool: Some(4),
                unsafe_durability: false,
                replication: Default::default(),
            }],
        },
        units: vec![],
        writer: WriterConfig::default(),
        clients: test_clients(),
        webhooks: vec![],
        before_send: None,
        open_api: Default::default(),
        server: Default::default(),
        auth: Default::default(),
        encryption: test_encryption(),
        sync: Default::default(),
        retention: Default::default(),
        agents: Default::default(),
        mcp: Default::default(),
        moderation: Default::default(),
        cluster: Default::default(),
    }
}

/// `main` holds the directory and the default unit; `secret` holds unit `b`.
pub fn two(main: &str, secret: &str, unit_b_in: &str) -> Config {
    Config {
        storage: StorageConfig {
            control: "main".into(),
            databases: vec![
                DatabaseConfig {
                    name: "main".into(),
                    url: main.into(),
                    read_pool: Some(4),
                    unsafe_durability: false,
                    replication: Default::default(),
                },
                DatabaseConfig {
                    name: "secret".into(),
                    url: secret.into(),
                    read_pool: Some(4),
                    unsafe_durability: false,
                    replication: Default::default(),
                },
            ],
        },
        units: vec![UnitConfig {
            name: "b".into(),
            database: unit_b_in.into(),
        }],
        writer: WriterConfig::default(),
        clients: test_clients(),
        webhooks: vec![],
        before_send: None,
        open_api: Default::default(),
        server: Default::default(),
        auth: Default::default(),
        encryption: test_encryption(),
        sync: Default::default(),
        retention: Default::default(),
        agents: Default::default(),
        mcp: Default::default(),
        moderation: Default::default(),
        cluster: Default::default(),
    }
}

pub async fn open(cfg: Config) -> Hub {
    Hub::open(cfg, hub_cfg()).await.expect("open hub")
}

pub async fn user(hub: &Hub, name: &str, kind: UserKind, owner: Option<&User>) -> User {
    hub.create_user(name, kind, owner.map(|o| o.id.as_str()), None)
        .await
        .unwrap()
        .0
}

/// A client token for a person, signed the way a developer's backend
/// would (a signing key is created when the deployment has none).
pub async fn client_token(hub: &Hub, u: &User) -> String {
    if hub.sign_token(&u.name, 3600).is_err() {
        hub.create_signing_key("tests").await.unwrap();
    }
    hub.sign_token(&u.name, 3600).unwrap()
}

/// `ws_query` for a person's first device.
pub async fn ws_token(hub: &Hub, u: &User) -> String {
    ws_query(hub, u, "d1").await
}

/// A person's credential for `/ws`: `token=…&device=…`.
pub async fn ws_query(hub: &Hub, u: &User, device: &str) -> String {
    format!("token={}&device={device}", client_token(hub, u).await)
}

pub async fn user_in(hub: &Hub, name: &str, unit: &str) -> User {
    hub.create_user(name, UserKind::Human, None, Some(unit))
        .await
        .unwrap()
        .0
}

pub struct Conn {
    pub s: Session,
    pub rx: Pushes,
    hub: Hub,
}

impl Conn {
    pub fn new(hub: &Hub, u: &User) -> Conn {
        let (s, rx) = hub.connect(u.clone());
        Conn {
            s,
            rx,
            hub: hub.clone(),
        }
    }
    /// A connection of a registered device (sync positions persist).
    pub fn device(hub: &Hub, u: &User, device_id: &str) -> Conn {
        let (s, rx) = hub.connect_device(u.clone(), Some(device_id.to_string()));
        Conn {
            s,
            rx,
            hub: hub.clone(),
        }
    }
    /// Sends a request. Tests read the server log with `pull` / `pull_side`
    /// as before, but that goes to the server-side read (`Hub::read_log`):
    /// clients themselves can only sync (docs/08).
    pub async fn call(&self, v: Value) -> Value {
        let op = v["op"].as_str().unwrap_or("");
        if (op == "pull" && v["as_agent"].is_null()) || op == "pull_side" {
            let conv = v["conv_id"].as_str().unwrap_or("");
            let r = if op == "pull" {
                self.hub
                    .read_log(
                        &self.s.user,
                        conv,
                        v["after_seq"].as_i64().unwrap_or(0),
                        v["limit"].as_i64().unwrap_or(200),
                    )
                    .await
            } else {
                self.hub
                    .read_side(
                        &self.s.user,
                        conv,
                        v["after_side_seq"].as_i64().unwrap_or(0),
                    )
                    .await
            };
            return match r {
                Ok(mut x) => {
                    x["ok"] = json!(true);
                    x["type"] = json!("res");
                    x
                }
                Err(e) => {
                    json!({"ok": false, "type": "res", "error": {"code": e.code, "message": e.message}})
                }
            };
        }
        self.s.handle_text(&v.to_string()).await
    }
    pub async fn ok(&self, v: Value) -> Value {
        let r = self.call(v.clone()).await;
        assert_eq!(r["ok"], true, "request {v} failed: {r}");
        r
    }
    pub fn drain(&mut self) -> Vec<Value> {
        let mut out = vec![];
        while let Some(f) = self.rx.try_recv() {
            out.push(serde_json::from_str(&f).unwrap());
        }
        out
    }
    pub async fn send(&self, conv: &str, text: &str) -> Value {
        self.call(json!({"op": "send", "conv_id": conv, "client_msg_id": uuid::Uuid::new_v4().to_string(), "text": text})).await
    }
    pub async fn reply(&self, conv: &str, text: &str, to: &str) -> Value {
        self.call(json!({"op": "send", "conv_id": conv, "client_msg_id": uuid::Uuid::new_v4().to_string(), "text": text, "reply_to": to})).await
    }
    pub async fn pull(&self, conv: &str, after: i64) -> Value {
        self.ok(json!({"op": "pull", "conv_id": conv, "after_seq": after, "limit": 1000}))
            .await
    }
    pub async fn group(&self, members: &[&User]) -> String {
        let ids: Vec<&str> = members.iter().map(|u| u.id.as_str()).collect();
        self.ok(json!({"op": "create_conv", "kind": "group", "title": "g", "members": ids}))
            .await["conv_id"]
            .as_str()
            .unwrap()
            .to_string()
    }
}

pub fn seqs(pull: &Value) -> Vec<i64> {
    pull["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["event"]["seq"].as_i64().unwrap())
        .collect()
}

pub fn msg_by_id(pull: &Value, msg_id: &str) -> Option<Value> {
    pull["events"]
        .as_array()
        .unwrap()
        .iter()
        .rev()
        .find(|e| e["event"]["msg"]["msg_id"] == msg_id)
        .map(|e| e["event"]["msg"].clone())
}

/// Generates one test per backend for each listed `async fn name(db: TestDb)`.
#[macro_export]
macro_rules! backend_tests {
    ($($name:ident),* $(,)?) => {
        mod sqlite { $( #[tokio::test(flavor = "multi_thread", worker_threads = 4)] async fn $name() { if let Some(db) = $crate::common::new_db($crate::common::Backend::Sqlite).await { super::$name(db).await } } )* }
        mod postgres { $( #[tokio::test(flavor = "multi_thread", worker_threads = 4)] async fn $name() { match $crate::common::new_db($crate::common::Backend::Postgres).await { Some(db) => super::$name(db).await, None => eprintln!("skipped: AGENTIM_TEST_PG not set") } } )* }
        mod mysql { $( #[tokio::test(flavor = "multi_thread", worker_threads = 4)] async fn $name() { match $crate::common::new_db($crate::common::Backend::Mysql).await { Some(db) => super::$name(db).await, None => eprintln!("skipped: AGENTIM_TEST_MYSQL not set") } } )* }
    };
}
