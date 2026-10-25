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

fn run(args: &[&str]) -> (i32, String, String) {
    let out = Command::new(BIN).args(args).output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// `agentim admin …` calls the admin API of a running server with a key
/// made by `agentim admin-key create` on the same database (docs/12 §4);
/// the server prints the one-time console setup link while there is no
/// admin.
#[test]
fn admin_commands_reach_a_running_server() {
    use std::io::{BufRead, BufReader};
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("a.db");
    let db = db.to_str().unwrap();
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let addr = format!("127.0.0.1:{port}");
    let mut server = Command::new(BIN)
        .args(["--db", db, "--addr", &addr])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(server.stdout.take().unwrap()).lines();
    let mut setup = None;
    for l in lines.by_ref() {
        let l = l.unwrap();
        if l.contains("/console/setup?token=setup_") {
            setup = Some(l);
            break;
        }
    }
    let setup = setup.expect("a setup link");
    assert!(
        setup.contains(&format!("http://{addr}/console/setup")),
        "{setup}"
    );
    let url = format!("http://{addr}");
    let (code, out, err) = run(&[
        "admin-key",
        "create",
        "--name",
        "ci",
        "--role",
        "developer",
        "--db",
        db,
    ]);
    assert_eq!(code, 0, "{err}");
    let key = serde_json::from_str::<serde_json::Value>(&out).unwrap()["key"]
        .as_str()
        .unwrap()
        .to_string();
    let admin = |args: &[&str]| {
        let mut a = vec!["admin", "--url", &url, "--key", &key];
        a.extend_from_slice(args);
        run(&a)
    };
    let (code, out, err) = admin(&["overview"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&out).unwrap()["people"],
        0
    );
    let (code, out, err) = admin(&["signing-key-create", "--name", "web"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("\"kid\""), "{out}");
    // Business errors come back with their code; the exit status is 1.
    let (code, _, err) = admin(&["user", "nobody"]);
    assert_eq!(code, 1);
    assert!(err.contains("404 not_found"), "{err}");
    let (code, _, err) = admin(&["status"]);
    assert_eq!(code, 1);
    assert!(
        err.contains("403 forbidden"),
        "a developer key has no deploy area: {err}"
    );
    let (code, _, err) = run(&["admin", "--url", &url, "--key", "adk_wrong", "overview"]);
    assert_eq!(code, 1);
    assert!(err.contains("401 unauthorized"), "{err}");
    let _ = server.kill();
    let _ = server.wait();
}

/// `agentim` with no arguments in an empty folder (docs/12 §1): it makes
/// the database and its master key, says where the key is and to back it
/// up, and prints the address and the console setup link. The second start
/// reuses the key; a key others can read stops it (Unix).
#[test]
fn the_first_run_needs_nothing() {
    use std::io::{BufRead, BufReader};
    let dir = tempfile::tempdir().unwrap();
    let start = || {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let addr = format!("127.0.0.1:{port}");
        let mut p = Command::new(BIN)
            .args(["--addr", &addr])
            .current_dir(dir.path())
            .env_remove("AGENTIM_MASTER_KEY")
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let mut seen = vec![];
        for l in BufReader::new(p.stdout.take().unwrap()).lines() {
            let l = l.unwrap();
            let done = l.contains("/console/setup?token=");
            seen.push(l);
            if done {
                break;
            }
        }
        let _ = p.kill();
        let _ = p.wait();
        (addr, seen.join("\n"))
    };
    let (addr, out) = start();
    let key = dir.path().join("agentim.db.key");
    assert!(dir.path().join("agentim.db").exists());
    assert!(
        out.contains(&format!("listening on http://{addr}")),
        "{out}"
    );
    assert!(
        out.contains("Created the master key")
            && out.contains("agentim.db.key")
            && out.contains("back it up"),
        "{out}"
    );
    assert!(
        out.contains(&format!("http://{addr}/console/setup?token=setup_")),
        "{out}"
    );
    let k = std::fs::read_to_string(&key).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&key).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    // Again: the same key, nothing created.
    let (_, out) = start();
    assert!(!out.contains("Created the master key"), "{out}");
    assert_eq!(std::fs::read_to_string(&key).unwrap(), k);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o644)).unwrap();
        let out = Command::new(BIN)
            .args(["--addr", "127.0.0.1:0"])
            .current_dir(dir.path())
            .env_remove("AGENTIM_MASTER_KEY")
            .output()
            .unwrap();
        assert_ne!(out.status.code(), Some(0));
        let err = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(err.contains("chmod 600"), "{err}");
        assert_eq!(std::fs::read_to_string(&key).unwrap(), k, "the key is kept");
    }
}
