//! Agent checkpoints, write side (docs/09 §11.2): versions are compared in
//! the control database's writer, so of two runtimes writing the same
//! version exactly one wins.

use serde_json::{Value, json};

use super::{Ctx, err};
use crate::model::*;

/// Older versions kept per key.
const KEEP: i64 = 20;

impl Ctx<'_, '_> {
    pub(super) async fn state_put(
        &mut self,
        agent_id: &str,
        key: &str,
        value: &str,
        expected: i64,
        max_keys: i64,
    ) -> ApiResult<Value> {
        let head = self.db.state_head(agent_id, key).await?;
        if head != expected {
            return Err(err(
                "conflict",
                format!("{key} is at version {head}; read it and retry"),
            ));
        }
        if head == 0 && self.db.state_keys(agent_id).await? >= max_keys {
            return Err(err(
                "rate_limited",
                format!("at most {max_keys} keys per agent (agents.state_keys)"),
            ));
        }
        self.db
            .put_state(agent_id, key, head + 1, value, KEEP)
            .await?;
        Ok(json!({"key": key, "version": head + 1}))
    }

    pub(super) async fn state_delete(
        &mut self,
        agent_id: &str,
        key: &str,
        expected: i64,
    ) -> ApiResult<Value> {
        let head = self.db.state_head(agent_id, key).await?;
        if head == 0 {
            return Err(err("not_found", format!("no key {key}")));
        }
        if head != expected {
            return Err(err(
                "conflict",
                format!("{key} is at version {head}; read it and retry"),
            ));
        }
        self.db.delete_state(agent_id, key).await?;
        Ok(json!({"key": key, "deleted": true}))
    }
}
