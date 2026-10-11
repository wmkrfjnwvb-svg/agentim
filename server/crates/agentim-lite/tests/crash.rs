//! Crash recovery of the `agentim-lite` binary: killed while clients send,
//! restarted on the same database, every acknowledged message is there.

const BIN: &str = env!("CARGO_BIN_EXE_agentim-lite");

include!("../../agentim/tests/support/crash_body.rs");
