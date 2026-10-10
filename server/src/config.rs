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
