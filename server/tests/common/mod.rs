//! Shared test helpers. Backends: SQLite always; PostgreSQL and MySQL when
//! `AGENTIM_TEST_PG` / `AGENTIM_TEST_MYSQL` hold a server URL without a
//! database name (e.g. `postgres://postgres@127.0.0.1`,
//! `mysql://agentim:agentim@127.0.0.1`). Each test gets a fresh database.
#![allow(dead_code)]

use std::time::Duration;

use agentim::config::{
    Config, DatabaseConfig, StorageConfig, UnitConfig, WriterConfig, sqlite_url,
};
use agentim::guard::GuardConfig;
use agentim::hub::{Hub, Session};
use agentim::logic::HubConfig;
use agentim::model::*;
use agentim::registry::Pushes;
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
            sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {name}")))
                .execute(&mut admin)
                .await
                .expect("create database");
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
            }],
        },
        units: vec![],
        writer: WriterConfig::default(),
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
                },
                DatabaseConfig {
                    name: "secret".into(),
                    url: secret.into(),
                    read_pool: Some(4),
                    unsafe_durability: false,
                },
            ],
        },
        units: vec![UnitConfig {
            name: "b".into(),
            database: unit_b_in.into(),
        }],
        writer: WriterConfig::default(),
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

pub async fn user_in(hub: &Hub, name: &str, unit: &str) -> User {
    hub.create_user(name, UserKind::Human, None, Some(unit))
        .await
        .unwrap()
        .0
}

pub struct Conn {
    pub s: Session,
    pub rx: Pushes,
}

impl Conn {
    pub fn new(hub: &Hub, u: &User) -> Conn {
        let (s, rx) = hub.connect(u.clone());
        Conn { s, rx }
    }
    pub async fn call(&self, v: Value) -> Value {
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
