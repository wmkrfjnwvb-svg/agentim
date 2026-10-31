#![recursion_limit = "512"]

/// A refusal by a named business rule (docs/14 §5.1): `rule!("msg.edit_version",
/// "conflict", message)`. The id is stable and listed in
/// docs/15-business-rules.md, which is generated from these calls.
macro_rules! rule {
    ($id:literal, $code:literal, $msg:expr $(,)?) => {
        $crate::model::ApiError::rule($id, $code, $msg)
    };
}

pub mod admin_api;
pub mod admin_cli;
pub mod auth;
pub mod auth_limit;
pub mod cli;
pub mod cluster;
pub mod config;
pub mod console;
pub mod crypto;
pub mod db;
pub mod demo;
pub mod demo_web;
pub mod failpoint;
pub mod files_http;
pub mod filestore;
pub mod guard;
pub mod hub;
pub mod logic;
pub mod mcp;
pub mod media;
pub mod metrics;
pub mod model;
pub mod open_api;
pub mod policy;
pub mod protocol;
pub mod registry;
pub mod replication;
pub mod server;
pub mod shard;
pub mod storage;
pub mod tls;
pub mod trace;
pub mod webhook;
