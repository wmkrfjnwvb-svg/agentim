//! Crash recovery of the `agentim` binary: killed while clients send,
//! restarted on the same database, every acknowledged message is there.

const BIN: &str = env!("CARGO_BIN_EXE_agentim");

include!("support/crash_body.rs");
