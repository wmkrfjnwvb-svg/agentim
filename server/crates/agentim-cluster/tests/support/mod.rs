//! Shared helpers of the cluster tests: several nodes in one process on one
//! real database, and the checks every multi-node test ends with.
#![allow(dead_code)]

use std::collections::HashSet;
use std::time::{Duration, Instant};

use agentim_cluster::Node;
use agentim_core::cluster::shard_of;
use agentim_core::config::{ClusterConfig, Config};
use agentim_core::hub::Hub;
use serde_json::{Value, json};

use crate::common::*;

#[macro_export]
macro_rules! cluster_tests {
    ($($name:ident),* $(,)?) => {
        mod postgres { $( #[tokio::test(flavor = "multi_thread", worker_threads = 4)] async fn $name() { let _slot = $crate::support::slot(); match $crate::common::new_db($crate::common::Backend::Postgres).await { Some(db) => super::$name(db).await, None => eprintln!("skipped: AGENTIM_TEST_PG not set") } } )* }
        mod mysql { $( #[tokio::test(flavor = "multi_thread", worker_threads = 4)] async fn $name() { let _slot = $crate::support::slot(); match $crate::common::new_db($crate::common::Backend::Mysql).await { Some(db) => super::$name(db).await, None => eprintln!("skipped: AGENTIM_TEST_MYSQL not set") } } )* }
    };
}

pub const SHARDS: u32 = 16;

pub fn cluster_cfg(url: &str, node: &str) -> Config {
    let mut c = single(url);
    // Several nodes per test: fewer connections each.
    c.storage.databases[0].read_pool = Some(2);
    c.cluster = ClusterConfig {
        enabled: true,
        node_id: node.into(),
        listen: "127.0.0.1:0".into(),
        shards: SHARDS,
        lease_ms: 3_000,
        renew_ms: 500,
        stop_ms: 2_000,
        op_timeout_ms: 500,
        ..Default::default()
    };
    c
}

/// `AGENTIM_TEST_LOG=agentim_cluster=debug` prints the nodes' logs.
pub fn logs() {
    if let Ok(f) = std::env::var("AGENTIM_TEST_LOG") {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(f)
            .with_test_writer()
            .try_init();
    }
}

pub async fn start_with(cfg: Config) -> Result<(Hub, Node), String> {
    logs();
    let hub = Hub::open_deferred(cfg, hub_cfg())
        .await
        .map_err(|e| e.message)?;
    let node = agentim_cluster::join(&hub).await?;
    node.start().await?;
    hub.finish_open().await.map_err(|e| e.message)?;
    Ok((hub, node))
}

pub async fn start(url: &str, node: &str) -> (Hub, Node) {
    start_with(cluster_cfg(url, node))
        .await
        .expect("node starts")
}

pub async fn cluster(db: &TestDb, n: usize) -> Vec<(Hub, Node)> {
    let mut v = vec![];
    for i in 0..n {
        v.push(start(&db.url, &format!("n{i}")).await);
    }
    settled(&v).await;
    v
}

/// Every shard (and the control and upkeep shards) held by exactly one of
/// the nodes, each node holding some.
pub async fn settled(nodes: &[(Hub, Node)]) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let mut all = vec![];
        for (_, n) in nodes {
            all.extend(n.held(0));
        }
        let distinct: HashSet<u32> = all.iter().copied().collect();
        let spread = nodes.iter().all(|(_, n)| !n.held(0).is_empty());
        if all.len() == (SHARDS + 2) as usize && distinct.len() == all.len() && spread {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "not settled: {:?}",
            nodes.iter().map(|(_, n)| n.held(0)).collect::<Vec<_>>()
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

pub fn holder(nodes: &[(Hub, Node)], conv: &str) -> Option<usize> {
    let s = shard_of(conv, SHARDS);
    nodes.iter().position(|(_, n)| n.held(0).contains(&s))
}

pub async fn wait_for<F: FnMut() -> bool>(what: &str, within: Duration, mut f: F) {
    let deadline = Instant::now() + within;
    while !f() {
        assert!(Instant::now() < deadline, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Retries a send with the same client_msg_id until it is accepted
/// (`unavailable` means: retry).
pub async fn send_until_ok(
    c: &Conn,
    conv: &str,
    cmid: &str,
    text: &str,
    within: Duration,
) -> Value {
    let deadline = Instant::now() + within;
    loop {
        let r = c
            .call(json!({"op": "send", "conv_id": conv, "client_msg_id": cmid, "text": text}))
            .await;
        if r["ok"] == true {
            return r;
        }
        assert_eq!(r["error"]["code"], "unavailable", "{r}");
        assert!(Instant::now() < deadline, "never accepted: {r}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// The checks after every multi-node test (docs/11 §12.7): every accepted
/// message is in the log once, sequences run 1..n without gaps, and each
/// client_msg_id appears at most once per sender.
pub async fn check_log(c: &Conn, conv: &str, accepted: &[String]) {
    let log = c.pull(conv, 0).await;
    let seqs = seqs(&log);
    assert_eq!(seqs, (1..=seqs.len() as i64).collect::<Vec<_>>(), "gapless");
    let ids: Vec<String> = log["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|e| e["event"]["msg"]["msg_id"].as_str().map(String::from))
        .collect();
    for a in accepted {
        assert_eq!(
            ids.iter().filter(|i| *i == a).count(),
            1,
            "accepted message {a} once"
        );
    }
    let mut seen = HashSet::new();
    for e in log["events"].as_array().unwrap() {
        if let (Some(s), Some(m)) = (
            e["event"]["msg"]["sender_id"].as_str(),
            e["event"]["msg"]["client_msg_id"].as_str(),
        ) {
            assert!(
                seen.insert((s.to_string(), m.to_string())),
                "client_msg_id {m} twice"
            );
        }
    }
}

// ------------------------------------------------------------ fault tools

/// A TCP proxy between one node and the database (in place of toxiproxy,
/// docs/11 §12.1): passes traffic, swallows it (`blackhole`: connections
/// hang) or cuts every connection (`reset`).
pub struct Proxy {
    pub addr: std::net::SocketAddr,
    mode: std::sync::Arc<std::sync::atomic::AtomicU8>,
    conns: std::sync::Arc<std::sync::Mutex<Vec<tokio::task::AbortHandle>>>,
    accept: tokio::task::JoinHandle<()>,
}

const PASS: u8 = 0;
const BLACKHOLE: u8 = 1;
const RESET: u8 = 2;

impl Proxy {
    pub async fn start(target: String) -> Proxy {
        use std::sync::atomic::Ordering::SeqCst;
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        let mode = std::sync::Arc::new(std::sync::atomic::AtomicU8::new(PASS));
        let conns: std::sync::Arc<std::sync::Mutex<Vec<tokio::task::AbortHandle>>> =
            Default::default();
        let (m, cs) = (mode.clone(), conns.clone());
        let accept = tokio::spawn(async move {
            loop {
                let Ok((client, _)) = l.accept().await else {
                    continue;
                };
                if m.load(SeqCst) == RESET {
                    continue; // dropped: connection refused in effect
                }
                let (m, target) = (m.clone(), target.clone());
                let t = tokio::spawn(async move {
                    let Ok(server) = tokio::net::TcpStream::connect(&target).await else {
                        return;
                    };
                    let (cr, cw) = client.into_split();
                    let (sr, sw) = server.into_split();
                    tokio::select! {
                        _ = pump(cr, sw, m.clone()) => {}
                        _ = pump(sr, cw, m) => {}
                    }
                });
                cs.lock().unwrap().push(t.abort_handle());
            }
        });
        Proxy {
            addr,
            mode,
            conns,
            accept,
        }
    }

    /// A database URL that goes through this proxy.
    pub async fn wrap(url: &str) -> (String, Proxy) {
        let at = url.find('@').unwrap() + 1;
        let end = url[at..].find('/').map(|i| at + i).unwrap_or(url.len());
        let mut target = url[at..end].to_string();
        if !target.contains(':') {
            target.push_str(if url.starts_with("postgres") {
                ":5432"
            } else {
                ":3306"
            });
        }
        let p = Proxy::start(target).await;
        (format!("{}{}{}", &url[..at], p.addr, &url[end..]), p)
    }

    pub fn pass(&self) {
        self.mode.store(PASS, std::sync::atomic::Ordering::SeqCst);
    }

    pub fn blackhole(&self) {
        self.mode
            .store(BLACKHOLE, std::sync::atomic::Ordering::SeqCst);
    }

    pub fn reset(&self) {
        self.mode.store(RESET, std::sync::atomic::Ordering::SeqCst);
        for c in self.conns.lock().unwrap().drain(..) {
            c.abort();
        }
    }
}

impl Drop for Proxy {
    fn drop(&mut self) {
        self.accept.abort();
        for c in self.conns.lock().unwrap().drain(..) {
            c.abort();
        }
    }
}

async fn pump(
    mut r: tokio::net::tcp::OwnedReadHalf,
    mut w: tokio::net::tcp::OwnedWriteHalf,
    mode: std::sync::Arc<std::sync::atomic::AtomicU8>,
) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = match r.read(&mut buf).await {
            Ok(0) | Err(_) => return,
            Ok(n) => n,
        };
        match mode.load(std::sync::atomic::Ordering::SeqCst) {
            PASS => {
                if w.write_all(&buf[..n]).await.is_err() {
                    return;
                }
            }
            BLACKHOLE => {} // swallowed
            _ => return,
        }
    }
}

/// What a client saw for each send (docs/11 §12.1): accepted (with the
/// message id), refused, or unknown (timed out / retryable).
#[derive(Debug, Clone)]
pub enum Outcome {
    Ok(String),
    Refused(String),
    Unknown,
}

#[derive(Default)]
pub struct History {
    ops: std::sync::Mutex<Vec<(String, String, Outcome)>>,
}

impl History {
    /// Sends and records the outcome; no answer within `wait` is unknown.
    pub async fn send(&self, c: &Conn, conv: &str, cmid: &str, wait: Duration) -> Outcome {
        let call =
            c.call(json!({"op": "send", "conv_id": conv, "client_msg_id": cmid, "text": cmid}));
        let o = match tokio::time::timeout(wait, call).await {
            Err(_) => Outcome::Unknown,
            Ok(r) if r["ok"] == true => Outcome::Ok(r["msg_id"].as_str().unwrap().to_string()),
            Ok(r) => match r["error"]["code"].as_str().unwrap_or("") {
                "unavailable" | "overloaded" | "storage_unavailable" => Outcome::Unknown,
                code => Outcome::Refused(code.to_string()),
            },
        };
        self.ops
            .lock()
            .unwrap()
            .push((conv.to_string(), cmid.to_string(), o.clone()));
        o
    }

    pub fn accepted(&self) -> usize {
        self.ops
            .lock()
            .unwrap()
            .iter()
            .filter(|o| matches!(o.2, Outcome::Ok(_)))
            .count()
    }

    /// The total check (docs/11 §12.7): every accepted message is in the
    /// log once and under its id, every client_msg_id appears at most once
    /// (also those whose outcome is unknown), refused ones not at all, and
    /// sequences have no gaps.
    pub async fn check(&self, c: &Conn) {
        let ops = self.ops.lock().unwrap().clone();
        let convs: HashSet<String> = ops.iter().map(|o| o.0.clone()).collect();
        for conv in convs {
            let log = c.pull(&conv, 0).await;
            let seqs = seqs(&log);
            assert_eq!(seqs, (1..=seqs.len() as i64).collect::<Vec<_>>(), "gapless");
            let mut by_cmid: std::collections::HashMap<String, Vec<String>> = Default::default();
            for e in log["events"].as_array().unwrap() {
                let m = &e["event"]["msg"];
                if let (Some(cmid), Some(id)) = (m["content"].as_str(), m["msg_id"].as_str()) {
                    by_cmid
                        .entry(cmid.to_string())
                        .or_default()
                        .push(id.to_string());
                }
            }
            for (_, cmid, o) in ops.iter().filter(|o| o.0 == conv) {
                let found = by_cmid.get(cmid).cloned().unwrap_or_default();
                assert!(found.len() <= 1, "{cmid} stored {} times", found.len());
                match o {
                    Outcome::Ok(id) => {
                        assert_eq!(found, vec![id.clone()], "accepted {cmid} is stored once")
                    }
                    Outcome::Refused(code) => {
                        assert!(found.is_empty(), "{cmid} refused ({code}) but stored")
                    }
                    Outcome::Unknown => {}
                }
            }
        }
    }
}

/// Retries one client_msg_id until it is accepted, as a client would.
pub async fn retry_until_ok(
    h: &History,
    c: &Conn,
    conv: &str,
    cmid: &str,
    within: Duration,
) -> String {
    let deadline = Instant::now() + within;
    loop {
        if let Outcome::Ok(id) = h.send(c, conv, cmid, Duration::from_secs(3)).await {
            return id;
        }
        assert!(Instant::now() < deadline, "{cmid} never accepted");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// The node index holding the conversation's shard, waiting for one.
pub async fn holder_of(nodes: &[(Hub, Node)], conv: &str, within: Duration) -> usize {
    let deadline = Instant::now() + within;
    loop {
        if let Some(h) = holder(nodes, conv) {
            return h;
        }
        assert!(Instant::now() < deadline, "nobody holds {conv}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// No shard is held by two nodes at once.
pub fn single_holders(nodes: &[&Node]) {
    let mut seen = HashSet::new();
    for n in nodes {
        for s in n.held(0) {
            assert!(seen.insert(s), "shard {s} held twice");
        }
    }
}

/// A group whose shard the given node holds.
pub async fn conv_held_by(
    nodes: &[(Hub, Node)],
    idx: usize,
    owner: &Conn,
    members: &[&agentim_core::model::User],
) -> String {
    for _ in 0..64 {
        let c = owner.group(members).await;
        if holder_of(nodes, &c, Duration::from_secs(5)).await == idx {
            return c;
        }
    }
    panic!("no conversation landed on node {idx}");
}

pub fn shard(conv: &str) -> u32 {
    shard_of(conv, SHARDS)
}

/// At most this many multi-node tests at once: each node holds about ten
/// database connections, and servers allow 100–150 by default.
pub struct Slot;

static SLOTS: std::sync::Mutex<usize> = std::sync::Mutex::new(0);
static FREED: std::sync::Condvar = std::sync::Condvar::new();

pub fn slot() -> Slot {
    let mut n = SLOTS.lock().unwrap_or_else(|e| e.into_inner());
    while *n >= 3 {
        n = FREED.wait(n).unwrap_or_else(|e| e.into_inner());
    }
    *n += 1;
    Slot
}

impl Drop for Slot {
    fn drop(&mut self) {
        *SLOTS.lock().unwrap_or_else(|e| e.into_inner()) -= 1;
        FREED.notify_one();
    }
}
