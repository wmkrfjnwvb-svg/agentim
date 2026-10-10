//! Configuration: databases, organisation units and write-path tuning.
//!
//! ```toml
//! [storage]
//! control = "main"                       # database holding users, units and the conversation directory
//!
//! [[storage.databases]]
//! name = "main"
//! url = "sqlite://agentim.db"
//!
//! [[storage.databases]]
//! name = "unit-b"
//! url = "postgres://agentim@db-eu.internal/agentim_b"
//!
//! [[units]]
//! name = "subsidiary-b"
//! database = "unit-b"                    # new conversations of this unit go here
//! ```

use std::path::Path;

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub storage: StorageConfig,
    #[serde(default)]
    pub units: Vec<UnitConfig>,
    #[serde(default)]
    pub writer: WriterConfig,
    #[serde(default)]
    pub clients: ClientsConfig,
    /// Event webhooks (docs/02-architecture.md §2.6).
    #[serde(default)]
    pub webhooks: Vec<WebhookConfig>,
    /// Synchronous hook asked before a message is sent.
    #[serde(default)]
    pub before_send: Option<BeforeSendConfig>,
    #[serde(default)]
    pub open_api: OpenApiConfig,
    #[serde(default)]
    pub server: ServerConfig,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ServerConfig {
    /// Browser origins allowed besides the server's own, e.g.
    /// `https://app.example.com` (docs/04-security-compliance.md §1).
    #[serde(default)]
    pub allowed_origins: Vec<String>,
    /// Serve HTTPS / WSS directly.
    #[serde(default)]
    pub tls: Option<TlsConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TlsConfig {
    /// PEM certificate chain and private key files.
    pub cert: String,
    pub key: String,
}

/// Open API transport and limits (docs/07-open-api.md §5).
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct OpenApiConfig {
    /// Behind a TLS-terminating proxy: accept plain HTTP from anywhere and
    /// take the client address from `X-Forwarded-For`.
    pub behind_proxy: bool,
    /// Requests per second per key, and the burst allowed.
    pub rate_per_second: f64,
    pub burst: f64,
}

impl Default for OpenApiConfig {
    fn default() -> Self {
        OpenApiConfig {
            behind_proxy: false,
            rate_per_second: 100.0,
            burst: 200.0,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct WebhookConfig {
    pub name: String,
    pub url: String,
    /// HMAC key for the `webhook-signature` header (at least 16 bytes).
    pub secret: String,
    /// Event types to deliver: exact (`msg.created`), a prefix (`msg.*`) or `*`.
    #[serde(default = "all_events")]
    pub events: Vec<String>,
    /// Attempts per event before it is dropped (and audited).
    #[serde(default = "default_attempts")]
    pub max_attempts: u32,
    #[serde(default = "default_webhook_timeout")]
    pub timeout_ms: u64,
    /// First retry delay; doubles per attempt, at most 5 minutes.
    #[serde(default = "default_backoff")]
    pub backoff_ms: u64,
}

fn all_events() -> Vec<String> {
    vec!["*".into()]
}
fn default_attempts() -> u32 {
    8
}
fn default_webhook_timeout() -> u64 {
    5000
}
fn default_backoff() -> u64 {
    1000
}

#[derive(Debug, Clone, Deserialize)]
pub struct BeforeSendConfig {
    pub url: String,
    pub secret: String,
    #[serde(default = "default_hook_timeout")]
    pub timeout_ms: u64,
    /// What to do when the hook cannot be reached or answers badly.
    #[serde(default)]
    pub on_error: HookFailure,
}

fn default_hook_timeout() -> u64 {
    2000
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HookFailure {
    #[default]
    Allow,
    Reject,
}

/// Client compatibility (docs/02-architecture.md §2.5).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ClientsConfig {
    /// Lowest supported version per client name, e.g. `agentim-js = "1.2.0"`.
    /// Older clients are refused in `hello` with `client_too_old`.
    #[serde(default)]
    pub min_versions: std::collections::BTreeMap<String, String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StorageConfig {
    pub control: String,
    pub databases: Vec<DatabaseConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DatabaseConfig {
    pub name: String,
    pub url: String,
    #[serde(default)]
    pub read_pool: Option<u32>,
    /// Start even if the database is not configured for full durability.
    #[serde(default)]
    pub unsafe_durability: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UnitConfig {
    pub name: String,
    pub database: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct WriterConfig {
    /// Maximum commands committed in one transaction (one fsync).
    pub max_batch: usize,
    /// Bounded command queue per database; a full queue answers `overloaded`.
    pub queue_capacity: usize,
    /// Optional wait before committing to collect a larger batch. 0 = never wait.
    pub commit_delay_us: u64,
}

impl Default for WriterConfig {
    fn default() -> Self {
        WriterConfig {
            max_batch: 512,
            queue_capacity: 10_000,
            commit_delay_us: 0,
        }
    }
}

/// SQLite URL for a file path. Uses the `sqlite:<path>` form, which keeps
/// Windows drive letters (`sqlite://C:/..` would read `C` as a host name).
/// The driver percent-decodes the path, so reserved characters are encoded.
pub fn sqlite_url(path: &str) -> String {
    let mut p = String::with_capacity(path.len());
    for c in path.chars() {
        match c {
            '\\' => p.push('/'),
            '%' => p.push_str("%25"),
            '?' => p.push_str("%3F"),
            '#' => p.push_str("%23"),
            ' ' => p.push_str("%20"),
            c => p.push(c),
        }
    }
    format!("sqlite:{p}?mode=rwc")
}

impl Config {
    /// One SQLite database holding everything (the `--db` shortcut).
    pub fn single_sqlite(path: &str) -> Config {
        Config {
            storage: StorageConfig {
                control: "main".into(),
                databases: vec![DatabaseConfig {
                    name: "main".into(),
                    url: sqlite_url(path),
                    read_pool: None,
                    unsafe_durability: false,
                }],
            },
            units: vec![],
            writer: WriterConfig::default(),
            clients: Default::default(),
            webhooks: vec![],
            before_send: None,
            open_api: Default::default(),
            server: Default::default(),
        }
    }

    pub fn load(path: &Path) -> Result<Config, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let cfg: Config = toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn validate(&self) -> Result<(), String> {
        let names: Vec<&str> = self
            .storage
            .databases
            .iter()
            .map(|d| d.name.as_str())
            .collect();
        if names.is_empty() {
            return Err("storage.databases is empty".into());
        }
        for (i, n) in names.iter().enumerate() {
            if names[..i].contains(n) {
                return Err(format!("duplicate database name {n}"));
            }
        }
        if !names.contains(&self.storage.control.as_str()) {
            return Err(format!(
                "storage.control = {} is not a configured database",
                self.storage.control
            ));
        }
        for u in &self.units {
            if !names.contains(&u.database.as_str()) {
                return Err(format!(
                    "unit {} uses unknown database {}",
                    u.name, u.database
                ));
            }
        }
        if self.writer.max_batch == 0 || self.writer.queue_capacity == 0 {
            return Err("writer.max_batch and writer.queue_capacity must be > 0".into());
        }
        let http = |u: &str| u.starts_with("http://") || u.starts_with("https://");
        for (i, w) in self.webhooks.iter().enumerate() {
            let ok_name = !w.name.is_empty()
                && w.name.len() <= 64
                && w.name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "_-".contains(c));
            if !ok_name {
                return Err(format!("webhooks: bad name {:?}", w.name));
            }
            if self.webhooks[..i].iter().any(|o| o.name == w.name) {
                return Err(format!("webhooks: duplicate name {}", w.name));
            }
            if !http(&w.url) {
                return Err(format!("webhooks.{}: url must be http(s)", w.name));
            }
            if w.secret.len() < 16 {
                return Err(format!(
                    "webhooks.{}: secret must be at least 16 bytes",
                    w.name
                ));
            }
            if w.events.is_empty() || w.events.iter().any(|e| !crate::webhook::valid_pattern(e)) {
                return Err(format!(
                    "webhooks.{}: events must be like msg.created, msg.* or *",
                    w.name
                ));
            }
            if w.max_attempts == 0 || w.timeout_ms == 0 {
                return Err(format!(
                    "webhooks.{}: max_attempts and timeout_ms must be > 0",
                    w.name
                ));
            }
        }
        if let Some(b) = &self.before_send {
            if !http(&b.url) {
                return Err("before_send.url must be http(s)".into());
            }
            if b.secret.len() < 16 {
                return Err("before_send.secret must be at least 16 bytes".into());
            }
            if b.timeout_ms == 0 {
                return Err("before_send.timeout_ms must be > 0".into());
            }
        }
        for o in &self.server.allowed_origins {
            if !crate::server::valid_origin(o) {
                return Err(format!(
                    "server.allowed_origins: {o:?} is not like https://host[:port]"
                ));
            }
        }
        if !(self.open_api.rate_per_second > 0.0 && self.open_api.burst >= 1.0) {
            return Err("open_api.rate_per_second must be > 0 and burst >= 1".into());
        }
        for (name, v) in &self.clients.min_versions {
            if name.is_empty() || name.len() > 64 {
                return Err(format!("clients.min_versions: bad client name {name:?}"));
            }
            if crate::protocol::parse_version(v).is_none() {
                return Err(format!(
                    "clients.min_versions.{name} = {v:?} is not a version like 1.2.3"
                ));
            }
        }
        Ok(())
    }

    /// Database new conversations of `unit` are created in; units that are not
    /// configured (including the default unit) use the control database.
    pub fn unit_database(&self, unit: &str) -> &str {
        self.units
            .iter()
            .find(|u| u.name == unit)
            .map(|u| u.database.as_str())
            .unwrap_or(self.storage.control.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(dbs: &[&str], control: &str, units: &[(&str, &str)]) -> Config {
        Config {
            storage: StorageConfig {
                control: control.into(),
                databases: dbs
                    .iter()
                    .map(|n| DatabaseConfig {
                        name: n.to_string(),
                        url: sqlite_url(&format!("{n}.db")),
                        read_pool: None,
                        unsafe_durability: false,
                    })
                    .collect(),
            },
            units: units
                .iter()
                .map(|(u, d)| UnitConfig {
                    name: u.to_string(),
                    database: d.to_string(),
                })
                .collect(),
            writer: WriterConfig::default(),
            clients: Default::default(),
            webhooks: vec![],
            before_send: None,
            open_api: Default::default(),
            server: Default::default(),
        }
    }

    #[test]
    fn invalid_configurations_are_refused_with_a_reason() {
        assert!(cfg(&["main"], "main", &[]).validate().is_ok());
        let cases = [
            (cfg(&[], "main", &[]), "empty"),
            (cfg(&["a", "a"], "a", &[]), "duplicate database name a"),
            (cfg(&["a"], "main", &[]), "storage.control = main"),
            (
                cfg(&["main"], "main", &[("u", "elsewhere")]),
                "unit u uses unknown database elsewhere",
            ),
        ];
        for (c, want) in cases {
            let e = c.validate().unwrap_err();
            assert!(e.contains(want), "{e} should mention {want}");
        }
        let mut c = cfg(&["main"], "main", &[]);
        c.writer.max_batch = 0;
        assert!(c.validate().is_err());
        let mut c = cfg(&["main"], "main", &[]);
        c.writer.queue_capacity = 0;
        assert!(c.validate().is_err());
        for (name, v, want) in [
            ("agentim-js", "1.x", "not a version"),
            ("agentim-js", "", "not a version"),
            ("", "1.0.0", "bad client name"),
        ] {
            let mut c = cfg(&["main"], "main", &[]);
            c.clients.min_versions.insert(name.into(), v.into());
            let e = c.validate().unwrap_err();
            assert!(e.contains(want), "{e} should mention {want}");
        }
        let mut c = cfg(&["main"], "main", &[]);
        c.clients
            .min_versions
            .insert("agentim-js".into(), "1.2".into());
        assert!(c.validate().is_ok());
    }

    #[test]
    fn client_minimum_versions_parse_from_toml() {
        let c: Config = toml::from_str(
            r#"
            [storage]
            control = "main"
            databases = [{ name = "main", url = "sqlite::memory:" }]
            [clients.min_versions]
            agentim-js = "1.2.0"
            "#,
        )
        .unwrap();
        assert_eq!(c.clients.min_versions["agentim-js"], "1.2.0");
        let c: Config = toml::from_str(
            r#"
            [storage]
            control = "main"
            databases = [{ name = "main", url = "sqlite::memory:" }]
            "#,
        )
        .unwrap();
        assert!(c.clients.min_versions.is_empty(), "optional section");
    }

    #[test]
    fn units_default_to_the_control_database() {
        let c = cfg(&["main", "eu"], "main", &[("acme-eu", "eu")]);
        assert_eq!(c.unit_database("acme-eu"), "eu");
        assert_eq!(c.unit_database("default"), "main");
        assert_eq!(c.unit_database("unconfigured"), "main");
    }

    #[test]
    fn configuration_files_are_parsed_and_checked() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.toml");
        std::fs::write(&p, "[storage]\ncontrol = \"main\"\n[[storage.databases]]\nname = \"main\"\nurl = \"sqlite://x.db\"\n").unwrap();
        assert!(Config::load(&p).is_ok());
        std::fs::write(&p, "[storage]\ncontrol = \"nope\"\n[[storage.databases]]\nname = \"main\"\nurl = \"sqlite://x.db\"\n").unwrap();
        assert!(Config::load(&p).unwrap_err().contains("storage.control"));
        std::fs::write(&p, "not = [valid").unwrap();
        assert!(Config::load(&p).is_err());
        assert!(Config::load(&dir.path().join("missing.toml")).is_err());
    }
}
