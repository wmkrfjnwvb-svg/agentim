//! Which product this process is (docs/01-product.md §6). AgentIM Lite is
//! the communication and record layer; AgentIM adds the platform modules.
//! Protocol, schema and configuration are the same, so moving between them
//! is replacing the binary.

use crate::config::Config;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Edition {
    Lite,
    #[default]
    Full,
}

impl Edition {
    pub fn binary(self) -> &'static str {
        match self {
            Edition::Lite => "agentim-lite",
            Edition::Full => "agentim",
        }
    }

    pub fn product(self) -> &'static str {
        match self {
            Edition::Lite => "AgentIM Lite",
            Edition::Full => "AgentIM",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Edition::Lite => "lite",
            Edition::Full => "full",
        }
    }

    /// Refuses configurations that need modules this product does not have.
    pub fn check(self, cfg: &Config) -> Result<(), String> {
        if self == Edition::Lite && cfg.storage.databases.len() > 1 {
            return Err("AgentIM Lite runs on one database; organisation units in their own databases need AgentIM (same data: replace the binary)".into());
        }
        Ok(())
    }
}
