//! The `agentim` command line: start-up refusals.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_agentim");

/// `--demo` lets anyone log in as anyone, so it only listens on localhost
/// unless `--demo-public` is given (docs/13 problem 6).
#[test]
fn the_demo_refuses_a_public_address() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("a.db");
    let out = Command::new(BIN)
        .args(["--demo", "--addr", "0.0.0.0:0", "--db"])
        .arg(&db)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("--demo-public"), "{err}");
    assert!(!db.exists(), "refused before opening the database");
}
