//! Database replicas (docs/11 §8, §12.5): with asynchronous replicas the
//! writer acknowledges only what the replicas have, reads stop there, and a
//! failover keeps every acknowledged message. Needs real replicas:
//! `AGENTIM_TEST_REPLICAS=1` and `ci/replicas.sh` (PostgreSQL binaries,
//! docker for MySQL); each backend runs alone (`pg`, `mysql`, or both).

mod common;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use agentim_core::config::ReplicationConfig;
use agentim_core::model::UserKind;
use common::*;
use serde_json::{Value, json};
use sqlx::Connection;

fn enabled(b: &str) -> bool {
    match std::env::var("AGENTIM_TEST_REPLICAS") {
        Ok(v) => v == "1" || v.split(',').any(|x| x == b),
        Err(_) => false,
    }
}

fn script(what: &str, b: &str) {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../ci/replicas.sh");
    let st = std::process::Command::new("bash")
        .args([path, what, b])
        .status()
        .unwrap();
    assert!(st.success(), "replicas.sh {what} {b}");
}

/// A TCP proxy whose target can move (the address a failover tool keeps
/// pointing at the primary).
struct Proxy {
    addr: std::net::SocketAddr,
    target: Arc<Mutex<String>>,
    conns: Arc<Mutex<Vec<tokio::task::AbortHandle>>>,
    down: Arc<AtomicBool>,
}

impl Proxy {
    async fn start(target: String) -> Proxy {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        let target = Arc::new(Mutex::new(target));
        let conns: Arc<Mutex<Vec<tokio::task::AbortHandle>>> = Default::default();
        let down = Arc::new(AtomicBool::new(false));
        let (t, cs, dn) = (target.clone(), conns.clone(), down.clone());
        tokio::spawn(async move {
            loop {
                let Ok((client, _)) = l.accept().await else {
                    continue;
                };
                if dn.load(Ordering::SeqCst) {
                    continue;
                }
                let to = t.lock().unwrap().clone();
                let task = tokio::spawn(async move {
                    let Ok(server) = tokio::net::TcpStream::connect(&to).await else {
                        return;
                    };
                    let (mut cr, mut cw) = client.into_split();
                    let (mut sr, mut sw) = server.into_split();
                    tokio::select! {
                        _ = tokio::io::copy(&mut cr, &mut sw) => {}
                        _ = tokio::io::copy(&mut sr, &mut cw) => {}
                    }
                });
                cs.lock().unwrap().push(task.abort_handle());
            }
        });
        Proxy {
            addr,
            target,
            conns,
            down,
        }
    }

    /// Points at another server and cuts every connection.
    fn retarget(&self, to: String) {
        *self.target.lock().unwrap() = to;
        for c in self.conns.lock().unwrap().drain(..) {
            c.abort();
        }
        self.down.store(false, Ordering::SeqCst);
    }
}

struct Setup {
    base: &'static str,
    primary: u16,
    replica: u16,
}

const PG: Setup = Setup {
    base: "postgres://postgres@127.0.0.1",
    primary: 5440,
    replica: 5441,
};
const MYSQL: Setup = Setup {
    base: "mysql://root:root@127.0.0.1",
    primary: 3310,
    replica: 3311,
};

async fn exec(url: &str, sql: &str) {
    sqlx::any::install_default_drivers();
    let mut c = sqlx::AnyConnection::connect(url).await.unwrap();
    for q in sql.split("; ") {
        sqlx::raw_sql(sqlx::AssertSqlSafe(q.to_string()))
            .execute(&mut c)
            .await
            .unwrap();
    }
}

async fn until<F, Fut>(what: &str, secs: u64, mut f: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let end = Instant::now() + Duration::from_secs(secs);
    while !f().await {
        assert!(Instant::now() < end, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn texts(pull: &Value) -> Vec<String> {
    pull["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| &e["event"])
        .filter(|e| e["type"] == "msg.created")
        .map(|e| e["msg"]["content"].as_str().unwrap_or("").to_string())
        .collect()
}

fn pushed(c: &mut Conn, text: &str) -> bool {
    c.drain()
        .iter()
        .any(|f| f["event"]["msg"]["content"] == text)
}

async fn send_as(c: &Conn, conv: &str, cm: &str, text: &str) -> Value {
    c.call(json!({"op": "send", "conv_id": conv, "client_msg_id": cm, "text": text}))
        .await
}

fn code(r: &Value) -> &str {
    r["error"]["code"].as_str().unwrap_or("")
}

async fn scenario(name: &str, s: Setup) {
    script("down", name);
    script("up", name);
    let admin = if name == "pg" {
        format!("{}:{}/postgres", s.base, s.primary)
    } else {
        format!("{}:{}/mysql", s.base, s.primary)
    };
    exec(&admin, "CREATE DATABASE agentim_rep").await;
    let replica_db = format!("{}:{}/agentim_rep", s.base, s.replica);
    // The database exists on the replica before the replica check needs it.
    until("database on the replica", 30, || async {
        sqlx::AnyConnection::connect(&replica_db).await.is_ok()
    })
    .await;
    let proxy = Proxy::start(format!("127.0.0.1:{}", s.primary)).await;
    let url = format!(
        "{}{}/agentim_rep",
        &s.base[..s.base.find('@').unwrap() + 1],
        proxy.addr
    );
    let mut cfg = single(&url);
    cfg.storage.databases[0].replication = ReplicationConfig {
        wait_ms: 1000,
        // Both servers: whichever is not the primary is checked.
        replica_urls: if name == "mysql" {
            vec![
                replica_db.clone(),
                format!("{}:{}/agentim_rep", s.base, s.primary),
            ]
        } else {
            vec![]
        },
        ..Default::default()
    };
    let hub = open(cfg).await;
    assert_eq!(hub.writer_stats()[0]["replication"], "app_wait");
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let a = Conn::new(&hub, &alice);
    let mut b = Conn::new(&hub, &bob);
    let conv = a.group(&[&bob]).await;
    let mut acked = vec![];
    for i in 0..5 {
        let t = format!("before {i}");
        a.ok(json!({"op": "send", "conv_id": conv, "client_msg_id": format!("cm-{t}"), "text": t}))
            .await;
        acked.push(t);
    }

    // The replica stops applying: a write is not acknowledged, not
    // delivered and not readable; it is once the replica catches up.
    script("pause", name);
    let r = send_as(&a, &conv, "cm-x", "x").await;
    assert_eq!(code(&r), "unavailable", "{r}");
    assert!(
        !pushed(&mut b, "x"),
        "not delivered before the replicas have it"
    );
    assert!(!texts(&a.pull(&conv, 0).await).contains(&"x".to_string()));
    assert_eq!(hub.writer_stats()[0]["unconfirmed"], 1);
    script("resume", name);
    until("held write delivered", 20, || {
        let found = pushed(&mut b, "x");
        async move { found }
    })
    .await;
    assert!(texts(&a.pull(&conv, 0).await).contains(&"x".to_string()));
    let again = send_as(&a, &conv, "cm-x", "x").await;
    assert_eq!(again["ok"], true, "the retry finds the write: {again}");
    acked.push("x".into());

    // Mutation check: without the wait the paused replica goes unnoticed,
    // so the assertion above would fail.
    let fp = &hub.shared().failpoints;
    script("pause", name);
    fp.set(
        "mutation.no_replica_wait",
        agentim_core::failpoint::Action::Error,
        None,
    );
    let r = send_as(&a, &conv, "cm-m", "m").await;
    assert_eq!(r["ok"], true, "the mutation acknowledges too early: {r}");
    fp.release("mutation.no_replica_wait");
    script("resume", name);
    acked.push("m".into());

    // The primary turns read-only: the batch fails as a whole, the writer
    // finds no writable primary until it is writable again.
    let ro_on = if name == "pg" {
        "ALTER SYSTEM SET default_transaction_read_only = on; SELECT pg_reload_conf()"
    } else {
        "SET GLOBAL super_read_only = ON"
    };
    let ro_off = if name == "pg" {
        "ALTER SYSTEM RESET default_transaction_read_only; SELECT pg_reload_conf()"
    } else {
        "SET GLOBAL super_read_only = OFF; SET GLOBAL read_only = OFF"
    };
    exec(&admin, ro_on).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let r = send_as(&a, &conv, "cm-ro", "ro").await;
    assert_eq!(code(&r), "unavailable", "{r}");
    let r = send_as(&a, &conv, "cm-ro", "ro").await;
    assert_eq!(code(&r), "unavailable", "still read-only: {r}");
    exec(&admin, ro_off).await;
    until("writable again", 20, || async {
        send_as(&a, &conv, "cm-ro", "ro").await["ok"] == true
    })
    .await;
    acked.push("ro".into());

    // Failover: a write the replica never got is not acknowledged; the
    // promoted replica has every acknowledged write. With no replica of
    // its own the new primary takes no acknowledged write (no silent
    // downgrade) until one is rebuilt.
    script("pause", name);
    let r = send_as(&a, &conv, "cm-z", "z").await;
    assert_eq!(code(&r), "unavailable", "{r}");
    script("failover", name);
    proxy.retarget(format!("127.0.0.1:{}", s.replica));
    let r = send_as(&a, &conv, "cm-w", "w").await;
    assert_eq!(code(&r), "unavailable", "no replica yet: {r}");
    let seen = texts(&a.pull(&conv, 0).await);
    for t in &acked {
        assert!(
            seen.contains(t),
            "acknowledged {t:?} survived the failover: {seen:?}"
        );
    }
    script("rebuild", name);
    until("writes after the rebuild", 60, || async {
        send_as(&a, &conv, "cm-w", "w").await["ok"] == true
    })
    .await;
    let r = send_as(&a, &conv, "cm-z", "z").await;
    assert_eq!(r["ok"], true, "{r}");
    let seen = texts(&a.pull(&conv, 0).await);
    for t in acked.iter().chain([&"w".to_string(), &"z".to_string()]) {
        assert_eq!(
            seen.iter().filter(|s| *s == t).count(),
            1,
            "{t:?} exactly once: {seen:?}"
        );
    }
    hub.shutdown();
    script("down", name);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn postgres_replicas_keep_acknowledged_writes() {
    if !enabled("pg") {
        return;
    }
    scenario("pg", PG).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mysql_replicas_keep_acknowledged_writes() {
    if !enabled("mysql") {
        return;
    }
    scenario("mysql", MYSQL).await;
}
