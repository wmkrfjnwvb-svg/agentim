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
    #[serde(default)]
    pub auth: AuthConfig,
    #[serde(default)]
    pub encryption: EncryptionConfig,
    /// Sync-only clients (docs/08-sync-and-client.md).
    #[serde(default)]
    pub sync: SyncFile,
    #[serde(default)]
    pub retention: RetentionFile,
    /// Agent registration (docs/10-agent-ecosystem.md §5.1).
    #[serde(default)]
    pub agents: AgentsConfig,
    /// The MCP endpoint `/mcp` (docs/10-agent-ecosystem.md §5.1).
    #[serde(default)]
    pub mcp: McpConfig,
}

/// `[agents]`.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct AgentsConfig {
    /// Agents one person may own.
    pub max_per_owner: usize,
}

impl Default for AgentsConfig {
    fn default() -> Self {
        AgentsConfig { max_per_owner: 20 }
    }
}

/// `[mcp]`.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct McpConfig {
    pub enabled: bool,
    /// Tool calls per agent key (token bucket).
    pub rate_per_second: f64,
    pub burst: f64,
}

impl Default for McpConfig {
    fn default() -> Self {
        McpConfig {
            enabled: true,
            rate_per_second: 5.0,
            burst: 20.0,
        }
    }
}

/// `[sync]` in the configuration file; days and counts.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct SyncFile {
    pub batch_size: i64,
    pub max_unacked: i64,
    pub max_gap: i64,
    pub device_ttl_days: i64,
    pub window_days: i64,
    pub max_messages: i64,
    pub older_enabled: bool,
    pub older_messages: i64,
    pub older_max: i64,
    pub member_messages: i64,
    pub member_days: i64,
    pub share_max: i64,
    pub rate_per_second: f64,
    pub burst: f64,
    pub agent_rate_per_second: f64,
    pub agent_burst: f64,
}

impl Default for SyncFile {
    fn default() -> Self {
        let d = SyncConfig::default();
        SyncFile {
            batch_size: d.batch_size,
            max_unacked: d.max_unacked,
            max_gap: d.max_gap,
            device_ttl_days: 30,
            window_days: 90,
            max_messages: d.max_messages,
            older_enabled: d.older_enabled,
            older_messages: d.older_messages,
            older_max: d.older_max,
            member_messages: d.member_messages,
            member_days: 30,
            share_max: d.share_max,
            rate_per_second: d.rate.0,
            burst: d.rate.1,
            agent_rate_per_second: d.agent_rate.0,
            agent_burst: d.agent_rate.1,
        }
    }
}

/// `[retention]`: how long the server keeps conversation events.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct RetentionFile {
    /// 0 keeps everything.
    pub days: i64,
}

impl Default for RetentionFile {
    fn default() -> Self {
        RetentionFile { days: 180 }
    }
}

/// The longest history window a device or member may get (docs/08 §2.4).
pub const MAX_WINDOW_DAYS: i64 = 90;
const DAY: std::time::Duration = std::time::Duration::from_secs(86_400);

/// Sync settings as the hub uses them.
#[derive(Debug, Clone)]
pub struct SyncConfig {
    /// Events per conversation per sync round.
    pub batch_size: i64,
    /// Events sent to a device and not acknowledged yet, per conversation.
    pub max_unacked: i64,
    /// A device further behind than this starts over like a new device.
    pub max_gap: i64,
    /// Devices that acknowledge nothing for this long lose their positions.
    pub device_ttl: std::time::Duration,
    /// New devices: the last `max_messages` events within `window`.
    pub window: std::time::Duration,
    pub max_messages: i64,
    /// Conversations with nothing in the window: none, or the last few events.
    pub older_enabled: bool,
    pub older_messages: i64,
    pub older_max: i64,
    /// New people in a group: the last `member_messages` within `member_window`.
    pub member_messages: i64,
    pub member_window: std::time::Duration,
    /// Most history whoever adds someone may share.
    pub share_max: i64,
    /// Sync rounds per second and burst, for people and for agents.
    pub rate: (f64, f64),
    pub agent_rate: (f64, f64),
    /// Events older than this are deleted; None keeps everything.
    pub retention: Option<std::time::Duration>,
}

impl Default for SyncConfig {
    fn default() -> Self {
        SyncConfig {
            batch_size: 200,
            max_unacked: 1000,
            max_gap: 10_000,
            device_ttl: DAY * 30,
            window: DAY * 90,
            max_messages: 200,
            older_enabled: false,
            older_messages: 20,
            older_max: 100,
            member_messages: 50,
            member_window: DAY * 30,
            share_max: 200,
            rate: (10.0, 20.0),
            agent_rate: (2.0, 5.0),
            retention: Some(DAY * 180),
        }
    }
}

impl Config {
    /// The sync settings of this configuration.
    pub fn sync_config(&self) -> SyncConfig {
        let f = &self.sync;
        let days = |n: i64| DAY * n.max(0) as u32;
        SyncConfig {
            batch_size: f.batch_size,
            max_unacked: f.max_unacked,
            max_gap: f.max_gap,
            device_ttl: days(f.device_ttl_days),
            window: days(f.window_days),
            max_messages: f.max_messages,
            older_enabled: f.older_enabled,
            older_messages: f.older_messages,
            older_max: f.older_max,
            member_messages: f.member_messages,
            member_window: days(f.member_days),
            share_max: f.share_max,
            rate: (f.rate_per_second, f.burst),
            agent_rate: (f.agent_rate_per_second, f.agent_burst),
            retention: (self.retention.days > 0).then(|| days(self.retention.days)),
        }
    }
}

/// Encryption at rest (docs/04-security-compliance.md §2.6). On by default.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct EncryptionConfig {
    #[serde(default)]
    pub mode: EncryptionMode,
    /// File holding the base64 master key.
    #[serde(default)]
    pub key_file: Option<String>,
    /// The base64 master key itself (tests; prefer key_file or the
    /// AGENTIM_MASTER_KEY environment variable).
    #[serde(default)]
    pub key: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EncryptionMode {
    #[default]
    Managed,
    Off,
}

impl Config {
    /// The master key, in order: `key`, `key_file`, `AGENTIM_MASTER_KEY`, and
    /// for a single SQLite database a generated `<database>.key` next to it.
    /// None when encryption is off.
    pub fn master_key(&self) -> Result<Option<String>, String> {
        let e = &self.encryption;
        if e.mode == EncryptionMode::Off {
            return Ok(None);
        }
        if let Some(k) = &e.key {
            return Ok(Some(k.clone()));
        }
        if let Some(f) = &e.key_file {
            return std::fs::read_to_string(f)
                .map(|s| Some(s.trim().to_string()))
                .map_err(|err| format!("encryption.key_file {f}: {err}"));
        }
        if let Ok(k) = std::env::var("AGENTIM_MASTER_KEY")
            && !k.trim().is_empty()
        {
            return Ok(Some(k.trim().to_string()));
        }
        if let [db] = self.storage.databases.as_slice()
            && let Some(path) = sqlite_path(&db.url)
        {
            if path == ":memory:" {
                return Ok(Some(crate::crypto::Kek::generate_base64()));
            }
            let f = format!("{path}.key");
            if let Ok(s) = std::fs::read_to_string(&f) {
                return Ok(Some(s.trim().to_string()));
            }
            let k = crate::crypto::Kek::generate_base64();
            write_private(&f, &k).map_err(|err| format!("{f}: {err}"))?;
            tracing::warn!(
                file = f,
                "generated the master key for encryption at rest; back it up together with the database"
            );
            return Ok(Some(k));
        }
        Err("encryption is on (the default) but no master key is configured: set encryption.key_file or AGENTIM_MASTER_KEY, or encryption.mode = \"off\"".into())
    }
}

/// The file path of a `sqlite:` URL.
fn sqlite_path(url: &str) -> Option<String> {
    let rest = url.strip_prefix("sqlite:")?;
    let rest = rest.strip_prefix("//").unwrap_or(rest);
    let path = rest.split('?').next().unwrap_or(rest);
    let path = percent_decode(path);
    (!path.is_empty()).then_some(path)
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16)
        {
            out.push(v);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Writes a secret readable only by this user (on Unix).
fn write_private(path: &str, content: &str) -> std::io::Result<()> {
    use std::io::Write;
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    let mut f = o.open(path)?;
    f.write_all(content.as_bytes())?;
    f.sync_all()
}

/// Built-in accounts (docs/04-security-compliance.md §1).
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct AuthConfig {
    /// Anyone may create an account at /api/auth/register.
    pub open_registration: bool,
    /// Failed logins before the account is locked, and for how long.
    pub max_failures: i64,
    pub lock_minutes: i64,
}

impl Default for AuthConfig {
    fn default() -> Self {
        AuthConfig {
            open_registration: false,
            max_failures: 10,
            lock_minutes: 15,
        }
    }
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
    /// How clients reach this server, e.g. `https://im.example.com`; used in
    /// the connection settings printed for new agents.
    #[serde(default)]
    pub public_url: Option<String>,
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
            auth: Default::default(),
            encryption: Default::default(),
            sync: Default::default(),
            retention: Default::default(),
            agents: Default::default(),
            mcp: Default::default(),
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
        if self.auth.max_failures < 1 || self.auth.lock_minutes < 1 {
            return Err("auth.max_failures and auth.lock_minutes must be >= 1".into());
        }
        for o in &self.server.allowed_origins {
            if !crate::server::valid_origin(o) {
                return Err(format!(
                    "server.allowed_origins: {o:?} is not like https://host[:port]"
                ));
            }
        }
        let sy = &self.sync;
        if !(1..=1000).contains(&sy.batch_size) || sy.max_unacked < sy.batch_size {
            return Err(
                "sync.batch_size must be 1..1000 and sync.max_unacked >= batch_size".into(),
            );
        }
        if sy.max_gap < sy.max_messages || sy.device_ttl_days < 1 {
            return Err(
                "sync.max_gap must be >= sync.max_messages and sync.device_ttl_days >= 1".into(),
            );
        }
        if !(1..=MAX_WINDOW_DAYS).contains(&sy.window_days)
            || !(1..=MAX_WINDOW_DAYS).contains(&sy.member_days)
        {
            return Err(format!(
                "sync.window_days and sync.member_days must be 1..{MAX_WINDOW_DAYS}"
            ));
        }
        if !(1..=1000).contains(&sy.max_messages)
            || !(0..=sy.max_messages).contains(&sy.member_messages)
            || !(0..=1000).contains(&sy.share_max)
        {
            return Err("sync.max_messages must be 1..1000, member_messages 0..max_messages, share_max 0..1000".into());
        }
        if !(1..=1000).contains(&sy.older_max) || !(0..=sy.older_max).contains(&sy.older_messages) {
            return Err(
                "sync.older_max must be 1..1000 and sync.older_messages 0..older_max".into(),
            );
        }
        if !(sy.rate_per_second > 0.0
            && sy.burst >= 1.0
            && sy.agent_rate_per_second > 0.0
            && sy.agent_burst >= 1.0)
        {
            return Err("sync rates must be > 0 and bursts >= 1".into());
        }
        if self.retention.days < 0
            || (self.retention.days > 0 && self.retention.days < sy.window_days)
        {
            return Err(
                "retention.days must be 0 (keep everything) or at least sync.window_days".into(),
            );
        }
        if !(1..=10_000).contains(&self.agents.max_per_owner) {
            return Err("agents.max_per_owner must be 1..10000".into());
        }
        if !(self.mcp.rate_per_second > 0.0 && self.mcp.burst >= 1.0) {
            return Err("mcp.rate_per_second must be > 0 and burst >= 1".into());
        }
        if let Some(u) = &self.server.public_url
            && !(u.starts_with("http://") || u.starts_with("https://"))
        {
            return Err(format!(
                "server.public_url: {u:?} is not like https://host[:port]"
            ));
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
            auth: Default::default(),
            encryption: Default::default(),
            sync: Default::default(),
            retention: Default::default(),
            agents: Default::default(),
            mcp: Default::default(),
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

    #[test]
    fn hook_auth_and_api_settings_are_checked() {
        let base = "[storage]\ncontrol = \"main\"\ndatabases = [{ name = \"main\", url = \"sqlite::memory:\" }]\n";
        let parse = |extra: &str| -> Result<Config, String> {
            let c: Config = toml::from_str(&format!("{base}{extra}")).map_err(|e| e.to_string())?;
            c.validate().map(|_| c)
        };
        let hook = |name: &str, url: &str, secret: &str, rest: &str| {
            format!(
                "[[webhooks]]\nname = \"{name}\"\nurl = \"{url}\"\nsecret = \"{secret}\"\n{rest}\n"
            )
        };
        let good = hook("crm", "https://x.example/h", "0123456789abcdef", "");
        // Defaults: every event, retries, timeouts.
        let c = parse(&good).unwrap();
        assert_eq!(c.webhooks[0].events, vec!["*".to_string()]);
        assert!(c.webhooks[0].max_attempts > 0 && c.webhooks[0].timeout_ms > 0);
        let cases = [
            (hook("", "https://x/h", "0123456789abcdef", ""), "bad name"),
            (hook("a b", "https://x/h", "0123456789abcdef", ""), "bad name"),
            (hook(&"n".repeat(65), "https://x/h", "0123456789abcdef", ""), "bad name"),
            (format!("{good}{good}"), "duplicate name"),
            (hook("crm", "ftp://x/h", "0123456789abcdef", ""), "url must be http(s)"),
            (hook("crm", "https://x/h", "short", ""), "secret must be at least 16 bytes"),
            (hook("crm", "https://x/h", "0123456789abcdef", "events = []"), "events must be"),
            (hook("crm", "https://x/h", "0123456789abcdef", "events = [\"msg.*.x\"]"), "events must be"),
            (hook("crm", "https://x/h", "0123456789abcdef", "max_attempts = 0"), "max_attempts"),
            (hook("crm", "https://x/h", "0123456789abcdef", "timeout_ms = 0"), "max_attempts"),
            ("[before_send]\nurl = \"x\"\nsecret = \"0123456789abcdef\"\n".to_string(), "before_send.url"),
            ("[before_send]\nurl = \"https://x\"\nsecret = \"short\"\n".to_string(), "before_send.secret"),
            ("[before_send]\nurl = \"https://x\"\nsecret = \"0123456789abcdef\"\ntimeout_ms = 0\n".to_string(), "before_send.timeout_ms"),
            ("[auth]\nmax_failures = 0\n".to_string(), "auth.max_failures"),
            ("[auth]\nlock_minutes = 0\n".to_string(), "auth.max_failures"),
            ("[open_api]\nrate_per_second = 0.0\n".to_string(), "open_api.rate_per_second"),
            ("[open_api]\nburst = 0.5\n".to_string(), "open_api.rate_per_second"),
        ];
        for (extra, want) in cases {
            let e = parse(&extra)
                .err()
                .unwrap_or_else(|| panic!("accepted: {extra}"));
            assert!(e.contains(want), "{extra} -> {e}");
        }
        let c =
            parse("[before_send]\nurl = \"https://x\"\nsecret = \"0123456789abcdef\"\n").unwrap();
        assert!(c.before_send.unwrap().timeout_ms > 0, "default timeout");
    }

    #[test]
    fn the_master_key_source() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a b%.db");
        let mut c = Config::single_sqlite(&path.to_string_lossy());
        // Generated next to the database (also through a percent-encoded URL).
        let k = c.master_key().unwrap().unwrap();
        assert_eq!(
            std::fs::read_to_string(format!("{}.key", path.to_string_lossy())).unwrap(),
            k
        );
        assert_eq!(
            c.master_key().unwrap().unwrap(),
            k,
            "read back, not regenerated"
        );
        // An explicit file wins; a missing file is an error.
        let f = dir.path().join("k");
        std::fs::write(&f, " other \n").unwrap();
        c.encryption.key_file = Some(f.to_string_lossy().into());
        assert_eq!(c.master_key().unwrap().unwrap(), "other");
        c.encryption.key_file = Some(dir.path().join("none").to_string_lossy().into());
        assert!(c.master_key().is_err());
        c.encryption.mode = EncryptionMode::Off;
        assert_eq!(c.master_key().unwrap(), None);
        // In memory: a fresh key each time. Several databases: must be configured.
        let mut m = Config::single_sqlite(":memory:");
        m.storage.databases[0].url = "sqlite::memory:".into();
        assert!(m.master_key().unwrap().is_some());
        let mut two = m.clone();
        two.storage.databases.push(two.storage.databases[0].clone());
        if std::env::var("AGENTIM_MASTER_KEY").is_err() {
            assert!(two.master_key().unwrap_err().contains("no master key"));
        }
        assert_eq!(percent_decode("a%20b%zz%2"), "a b%zz%2");
    }
}
