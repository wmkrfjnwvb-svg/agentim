//! Agent checkpoints (docs/09 §11.2): versioned keys an agent keeps on the
//! server so another runtime can carry on. Writes compare versions in the
//! control database's writer; reads go to its readers.

use serde_json::{Value, json};

use super::{Hub, err};
use crate::logic::Op;
use crate::model::*;

pub const KEY_MAX: usize = 255;
pub const VALUE_MAX: usize = 1024 * 1024;
const LIST_MAX: i64 = 500;
const LIST_DEFAULT: i64 = 100;
/// Written only by an import (docs/09 §11.4).
pub const ARCHIVE: &str = "archive/";

fn check_key(key: &str) -> ApiResult<()> {
    if key.is_empty() || key.len() > KEY_MAX || key.chars().any(char::is_control) {
        return Err(err(
            "bad_request",
            "a key is 1..255 bytes without control characters",
        ));
    }
    Ok(())
}

impl Hub {
    fn state_agent(me: &User) -> ApiResult<()> {
        if me.kind != UserKind::Agent {
            return Err(err("bad_request", "checkpoints are for agents"));
        }
        Ok(())
    }

    pub(super) async fn state_get(
        &self,
        me: &User,
        key: &str,
        version: Option<i64>,
    ) -> ApiResult<Value> {
        Self::state_agent(me)?;
        check_key(key)?;
        if version.is_some_and(|v| v < 1) {
            return Err(err("bad_request", "version must be >= 1"));
        }
        let (a, k) = (me.id.clone(), key.to_string());
        let r = self
            .read(self.control(), move |db| {
                Box::pin(async move { db.state(&a, &k, version).await })
            })
            .await?;
        let (v, value, at, cur) = r.ok_or_else(|| {
            err(
                "not_found",
                match version {
                    Some(v) => format!("no version {v} of {key}"),
                    None => format!("no key {key}"),
                },
            )
        })?;
        Ok(
            json!({"key": key, "version": v, "current_version": cur, "value": value, "updated_at": at}),
        )
    }

    pub(super) async fn state_put(
        &self,
        me: &User,
        key: &str,
        value: Value,
        version: i64,
    ) -> ApiResult<Value> {
        Self::state_agent(me)?;
        check_key(key)?;
        if key.starts_with(ARCHIVE) {
            return Err(err("bad_request", "archive/ is read-only"));
        }
        if version < 0 {
            return Err(err("bad_request", "version must be >= 0 (0 creates)"));
        }
        let text = value.to_string();
        if text.len() > VALUE_MAX {
            return Err(err("bad_request", "a value is at most 1 MiB as JSON"));
        }
        self.control()
            .call(Op::StatePut {
                agent_id: me.id.clone(),
                key: key.to_string(),
                value: text,
                version,
                max_keys: self.inner.cfg.agents.state_keys as i64,
            })
            .await
    }

    pub(super) async fn state_delete(
        &self,
        me: &User,
        key: &str,
        version: i64,
    ) -> ApiResult<Value> {
        Self::state_agent(me)?;
        check_key(key)?;
        if key.starts_with(ARCHIVE) {
            return Err(err("bad_request", "archive/ is read-only"));
        }
        if version < 1 {
            return Err(err("bad_request", "version must be >= 1"));
        }
        self.control()
            .call(Op::StateDelete {
                agent_id: me.id.clone(),
                key: key.to_string(),
                version,
            })
            .await
    }

    pub(super) async fn state_list(
        &self,
        me: &User,
        prefix: Option<String>,
        after: Option<String>,
        limit: Option<i64>,
    ) -> ApiResult<Value> {
        Self::state_agent(me)?;
        let limit = limit.unwrap_or(LIST_DEFAULT);
        if !(1..=LIST_MAX).contains(&limit) {
            return Err(err("bad_request", "limit must be 1..500"));
        }
        let prefix = prefix.unwrap_or_default();
        let after = after.unwrap_or_default();
        if prefix.len() > KEY_MAX || after.len() > KEY_MAX {
            return Err(err("bad_request", "prefix and after are at most 255 bytes"));
        }
        let a = me.id.clone();
        let (keys, more) = self
            .read(self.control(), move |db| {
                Box::pin(async move { db.list_state(&a, &prefix, &after, limit).await })
            })
            .await?;
        let keys: Vec<Value> = keys
            .into_iter()
            .map(|(k, v, at)| json!({"key": k, "version": v, "updated_at": at}))
            .collect();
        Ok(json!({"keys": keys, "more": more}))
    }
}
