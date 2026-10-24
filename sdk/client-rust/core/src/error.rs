//! Errors the app sees (docs/12 §2.11): the server's codes plus the
//! client's own (`disconnected`, `timeout`, `storage`, `closed`,
//! `unauthenticated`, `upgrade_required`).

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClientError {
    pub code: String,
    pub message: String,
    /// Trying again may work (the same request, later).
    pub retryable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_after_ms: Option<i64>,
}

impl ClientError {
    pub fn new(code: &str, message: impl Into<String>) -> ClientError {
        ClientError {
            code: code.to_string(),
            message: message.into(),
            retryable: is_transient(code),
            retry_after_ms: None,
        }
    }

    /// From a response frame's `error`.
    pub fn from_frame(e: &Value) -> ClientError {
        let mut c = ClientError::new(
            e["code"].as_str().unwrap_or("internal"),
            e["message"].as_str().unwrap_or(""),
        );
        c.retry_after_ms = e["retry_after_ms"].as_i64();
        c
    }

    pub fn to_json(&self) -> Value {
        json!(self)
    }
}

/// Failures that say nothing about the request itself: the network, the
/// server being busy or restarting. The offline queue retries these
/// forever (D113); anything else is the server's business answer.
pub fn is_transient(code: &str) -> bool {
    matches!(
        code,
        "disconnected"
            | "timeout"
            | "overloaded"
            | "unavailable"
            | "storage_unavailable"
            | "storage"
            | "rate_limited"
            | "internal"
            | "conv_migrating"
            | "idempotency_in_progress"
    )
}
