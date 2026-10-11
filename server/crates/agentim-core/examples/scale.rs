//! Scale tests against a running server (`agentim --demo`), run nightly in CI.
//!
//!   cargo run --release --example scale -- --scenario connections --users 20000 --storm
//!
//! Scenarios (docs/05-operations.md §4):
//! - `connections`: many mostly idle WebSocket connections in groups, about 1%
//!   of users sending. With `--storm` every receiving connection is dropped and
//!   re-opened at once halfway through, while the senders keep writing.
//! - `biggroup`: one large group; end-to-end delivery latency.
//! - `slow`: a share of the members stops reading. Their pushes are dropped and
//!   they are told to resync; everybody else must be unaffected.
//!
//! Every scenario checks correctness, not just speed. Each receiver must end up
//! with every event of its conversation — pushed, or pulled after a `resync`
//! notice or a reconnect — and pushes on one connection must arrive in seq
//! order. A gap the client was never told about fails the run, as does an
//! event that cannot be pulled at all.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use futures_util::stream::{self, SplitSink};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpSocket, TcpStream};
use tokio::sync::{Notify, oneshot};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;

// ------------------------------------------------------------------ args

struct Args {
    addr: String,
    scenario: String,
    users: usize,
    group_size: usize,
    senders: usize,
    /// Total messages per second across all senders; 0 = as fast as ACKs allow.
    rate: f64,
    duration: f64,
    payload: usize,
    storm: bool,
    slow_share: f64,
    /// Local source addresses 127.0.0.1..=N, to get past one address's port range.
    sources: u8,
    server_pid: Option<u32>,
    connect_concurrency: usize,
    /// Fails the run if the p99 end-to-end delivery latency exceeds this (ms).
    max_p99_ms: f64,
    out: Option<String>,
}

fn parse_args() -> Args {
    let mut a = Args {
        addr: "127.0.0.1:8080".into(),
        scenario: "connections".into(),
        users: 2000,
        group_size: 0,
        senders: 0,
        rate: 0.0,
        duration: 30.0,
        payload: 0,
        storm: false,
        slow_share: 0.1,
        sources: 1,
        server_pid: None,
        connect_concurrency: 256,
        max_p99_ms: f64::INFINITY,
        out: None,
    };
    let v: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < v.len() {
        if v[i] == "--storm" {
            a.storm = true;
            i += 1;
            continue;
        }
        let val = v.get(i + 1).cloned().unwrap_or_default();
        match v[i].as_str() {
            "--addr" => a.addr = val,
            "--scenario" => a.scenario = val,
            "--users" => a.users = val.parse().unwrap(),
            "--group-size" => a.group_size = val.parse().unwrap(),
            "--senders" => a.senders = val.parse().unwrap(),
            "--rate" => a.rate = val.parse().unwrap(),
            "--duration" => a.duration = val.parse().unwrap(),
            "--payload" => a.payload = val.parse().unwrap(),
            "--slow-share" => a.slow_share = val.parse().unwrap(),
            "--sources" => a.sources = val.parse().unwrap(),
            "--server-pid" => a.server_pid = val.parse().ok(),
            "--connect-concurrency" => a.connect_concurrency = val.parse().unwrap(),
            "--max-p99-ms" => a.max_p99_ms = val.parse().unwrap(),
            "--out" => a.out = Some(val),
            other => panic!("unknown argument {other}"),
        }
        i += 2;
    }
    match a.scenario.as_str() {
        "connections" => {
            if a.group_size == 0 {
                a.group_size = 100;
            }
            if a.senders == 0 {
                a.senders = (a.users / 100).max(1);
            }
        }
        "biggroup" | "slow" => {
            a.group_size = a.users;
            if a.senders == 0 {
                a.senders = 20.min(a.users);
            }
        }
        s => panic!("unknown scenario {s}"),
    }
    a
}

// ------------------------------------------------------------ keep-alive HTTP

/// HTTP/1.1 with keep-alive: creating tens of thousands of users over
/// `Connection: close` would use up the local port range with TIME_WAIT.
struct Http {
    r: BufReader<TcpStream>,
    host: String,
}

impl Http {
    async fn connect(addr: &str) -> Http {
        let s = TcpStream::connect(addr).await.expect("http connect");
        s.set_nodelay(true).ok();
        Http {
            r: BufReader::new(s),
            host: addr.into(),
        }
    }

    async fn request(&mut self, method: &str, path: &str, body: &str) -> (u16, String) {
        let req = format!(
            "{method} {path} HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            self.host,
            body.len()
        );
        self.r.get_mut().write_all(req.as_bytes()).await.unwrap();
        let mut line = String::new();
        self.r.read_line(&mut line).await.unwrap();
        let status: u16 = line.get(9..12).and_then(|s| s.parse().ok()).unwrap_or(0);
        let mut len = 0usize;
        loop {
            line.clear();
            self.r.read_line(&mut line).await.unwrap();
            let l = line.trim_end();
            if l.is_empty() {
                break;
            }
            if let Some((k, v)) = l.split_once(':')
                && k.eq_ignore_ascii_case("content-length")
            {
                len = v.trim().parse().unwrap_or(0);
            }
        }
        let mut buf = vec![0; len];
        self.r.read_exact(&mut buf).await.unwrap();
        (status, String::from_utf8_lossy(&buf).into_owned())
    }
}

#[derive(Clone)]
struct User {
    id: String,
    token: String,
}

async fn create_users(addr: &str, prefix: &str, n: usize) -> Vec<User> {
    let next = Arc::new(AtomicUsize::new(0));
    let out: Arc<Mutex<Vec<Option<User>>>> = Arc::new(Mutex::new(vec![None; n]));
    let mut tasks = vec![];
    for _ in 0..32.min(n.max(1)) {
        let (addr, prefix, next, out) = (
            addr.to_string(),
            prefix.to_string(),
            next.clone(),
            out.clone(),
        );
        tasks.push(tokio::spawn(async move {
            let mut h = Http::connect(&addr).await;
            loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                if i >= n {
                    return;
                }
                let body = json!({"name": format!("{prefix}{i}")}).to_string();
                let (st, b) = h.request("POST", "/api/dev/users", &body).await;
                assert_eq!(st, 200, "create user: {b}");
                let v: Value = serde_json::from_str(&b).unwrap();
                out.lock().unwrap()[i] = Some(User {
                    id: v["user"]["id"].as_str().unwrap().into(),
                    token: v["token"].as_str().unwrap().into(),
                });
            }
        }));
    }
    for t in tasks {
        t.await.unwrap();
    }
    let v = std::mem::take(&mut *out.lock().unwrap());
    v.into_iter().map(Option::unwrap).collect()
}

// --------------------------------------------------------------- latency

/// 1ms buckets up to 120s: cheap enough to record every single delivery.
struct Hist {
    b: Vec<AtomicU64>,
}

impl Hist {
    fn new() -> Hist {
        Hist {
            b: (0..120_001).map(|_| AtomicU64::new(0)).collect(),
        }
    }
    fn record_ms(&self, ms: f64) {
        let i = (ms.max(0.0) as usize).min(self.b.len() - 1);
        self.b[i].fetch_add(1, Ordering::Relaxed);
    }
    fn count(&self) -> u64 {
        self.b.iter().map(|x| x.load(Ordering::Relaxed)).sum()
    }
    /// Upper bound of the bucket holding the p-th quantile.
    fn pct(&self, p: f64) -> f64 {
        let n = self.count();
        if n == 0 {
            return 0.0;
        }
        let target = ((n as f64 - 1.0) * p).round() as u64 + 1;
        let mut acc = 0;
        for (i, x) in self.b.iter().enumerate() {
            acc += x.load(Ordering::Relaxed);
            if acc >= target {
                return i as f64 + 1.0;
            }
        }
        self.b.len() as f64
    }
}

fn now_us() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_micros() as u64
}

// ------------------------------------------------------------- connections

/// What one receiver has seen of its conversation: a bitset of seqs.
#[derive(Default)]
struct View {
    seen: Vec<u64>,
    /// Highest seq pushed so far (order check).
    last_push: i64,
    pushes: u64,
    out_of_order: u64,
    resync: bool,
    reconnected: bool,
}

impl View {
    fn mark(&mut self, seq: i64) {
        let (w, b) = ((seq / 64) as usize, seq % 64);
        if self.seen.len() <= w {
            self.seen.resize(w + 1, 0);
        }
        self.seen[w] |= 1 << b;
    }
    fn has(&self, seq: i64) -> bool {
        let (w, b) = ((seq / 64) as usize, seq % 64);
        self.seen.get(w).is_some_and(|x| x & (1 << b) != 0)
    }
    fn missing(&self, from: i64, to: i64) -> Vec<i64> {
        (from..=to).filter(|s| !self.has(*s)).collect()
    }
}

#[derive(Deserialize)]
struct InFrame<'a> {
    #[serde(rename = "type", borrow)]
    ty: &'a str,
    rid: Option<u64>,
    event: Option<InEvent>,
}

#[derive(Deserialize)]
struct InEvent {
    seq: i64,
    msg: Option<InMsg>,
}

#[derive(Deserialize)]
struct InMsg {
    content: Option<String>,
}

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>>;

struct Totals {
    e2e: Hist,
    ack: Hist,
    pushes: AtomicU64,
    /// Paused readers wait for this (slow consumers).
    resume: Notify,
}

struct Conn {
    sink: tokio::sync::Mutex<SplitSink<WebSocketStream<TcpStream>, Message>>,
    pending: Pending,
    rid: AtomicU64,
    reader: tokio::task::JoinHandle<()>,
}

impl Drop for Conn {
    fn drop(&mut self) {
        self.reader.abort();
    }
}

/// How a connection's reader treats pushes.
#[derive(Clone)]
struct Recv {
    view: Arc<Mutex<View>>,
    /// While set, the reader stops reading the socket.
    paused: Option<Arc<AtomicBool>>,
    record_latency: bool,
}

async fn ws_connect(
    addr: &str,
    token: &str,
    source: u8,
) -> Result<WebSocketStream<TcpStream>, String> {
    let server: SocketAddr = addr.parse().map_err(|e| format!("{e}"))?;
    let sock = TcpSocket::new_v4().map_err(|e| e.to_string())?;
    if source > 1 {
        sock.bind(SocketAddr::new(
            IpAddr::V4(Ipv4Addr::new(127, 0, 0, source)),
            0,
        ))
        .map_err(|e| e.to_string())?;
    }
    let tcp = sock.connect(server).await.map_err(|e| e.to_string())?;
    tcp.set_nodelay(true).ok();
    let (ws, _) = tokio_tungstenite::client_async_with_config(
        format!("ws://{addr}/ws?token={token}"),
        tcp,
        None,
    )
    .await
    .map_err(|e| e.to_string())?;
    Ok(ws)
}

impl Conn {
    async fn open(
        addr: &str,
        token: &str,
        source: u8,
        recv: Option<Recv>,
        totals: Arc<Totals>,
    ) -> Result<Conn, String> {
        let ws = ws_connect(addr, token, source).await?;
        let (sink, mut rx) = ws.split();
        let pending: Pending = Arc::default();
        let p2 = pending.clone();
        let reader = tokio::spawn(async move {
            loop {
                if let Some(p) = recv.as_ref().and_then(|r| r.paused.as_ref())
                    && p.load(Ordering::Acquire)
                {
                    // Created before the re-check, so a notify in between is not lost.
                    let n = totals.resume.notified();
                    if p.load(Ordering::Acquire) {
                        n.await;
                    }
                }
                let Some(Ok(m)) = rx.next().await else { break };
                let Message::Text(t) = m else { continue };
                let Ok(f) = serde_json::from_str::<InFrame>(t.as_str()) else {
                    continue;
                };
                match (f.ty, &recv) {
                    ("res", _) => {
                        let tx = p2.lock().unwrap().remove(&f.rid.unwrap_or(0));
                        if let Some(tx) = tx {
                            let _ = tx.send(serde_json::from_str(t.as_str()).unwrap());
                        }
                    }
                    ("event", Some(r)) => {
                        let Some(ev) = f.event else { continue };
                        if r.record_latency
                            && let Some(us) = ev
                                .msg
                                .as_ref()
                                .and_then(|m| m.content.as_deref())
                                .and_then(|c| c.strip_prefix("t="))
                                .and_then(|c| c.split('|').next())
                                .and_then(|c| c.parse::<u64>().ok())
                        {
                            totals
                                .e2e
                                .record_ms(now_us().saturating_sub(us) as f64 / 1000.0);
                        }
                        totals.pushes.fetch_add(1, Ordering::Relaxed);
                        let mut v = r.view.lock().unwrap();
                        v.pushes += 1;
                        if ev.seq <= v.last_push {
                            v.out_of_order += 1;
                        } else {
                            v.last_push = ev.seq;
                        }
                        v.mark(ev.seq);
                    }
                    ("resync", Some(r)) => r.view.lock().unwrap().resync = true,
                    _ => {}
                }
            }
            p2.lock().unwrap().clear(); // fail outstanding calls
        });
        Ok(Conn {
            sink: tokio::sync::Mutex::new(sink),
            pending,
            rid: AtomicU64::new(0),
            reader,
        })
    }

    async fn call(&self, mut v: Value) -> Result<Value, String> {
        let rid = self.rid.fetch_add(1, Ordering::Relaxed) + 1;
        v["rid"] = json!(rid);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(rid, tx);
        self.sink
            .lock()
            .await
            .send(Message::Text(v.to_string().into()))
            .await
            .map_err(|e| e.to_string())?;
        match tokio::time::timeout(Duration::from_secs(60), rx).await {
            Ok(Ok(r)) if r["ok"] == true => Ok(r),
            Ok(Ok(r)) => Err(r.to_string()),
            Ok(Err(_)) => Err("connection closed".into()),
            Err(_) => Err("timeout".into()),
        }
    }

    /// Every seq after `after` up to the head, pulled page by page.
    async fn pull_seqs(&self, conv: &str, mut after: i64) -> Result<Vec<i64>, String> {
        let mut out = vec![];
        loop {
            let r = self
                .call(json!({"op": "pull", "conv_id": conv, "after_seq": after, "limit": 1000}))
                .await?;
            for e in r["events"].as_array().unwrap() {
                out.push(e["event"]["seq"].as_i64().unwrap());
            }
            after = r["upto_seq"].as_i64().unwrap();
            if r["has_more"] != true {
                return Ok(out);
            }
        }
    }
}

async fn open_retry(
    addr: &str,
    token: &str,
    source: u8,
    recv: Option<Recv>,
    totals: Arc<Totals>,
) -> Result<Conn, String> {
    let mut last = String::new();
    for attempt in 0..6u32 {
        match Conn::open(addr, token, source, recv.clone(), totals.clone()).await {
            Ok(c) => return Ok(c),
            Err(e) => last = e,
        }
        tokio::time::sleep(Duration::from_millis(200 * 2u64.pow(attempt))).await;
    }
    Err(last)
}

// ------------------------------------------------------------- server side

fn rss_mb(pid: Option<u32>) -> Option<f64> {
    let s = std::fs::read_to_string(format!("/proc/{}/status", pid?)).ok()?;
    let kb: f64 = s
        .lines()
        .find(|l| l.starts_with("VmRSS:"))?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()?;
    Some(kb / 1024.0)
}

async fn server_counters(addr: &str) -> Value {
    let (_, text) = Http::connect(addr)
        .await
        .request("GET", "/metrics", "")
        .await;
    let mut out = serde_json::Map::new();
    for l in text.lines() {
        for name in [
            "agentim_push_total",
            "agentim_push_dropped_total",
            "agentim_resync_total",
            "agentim_ws_connections",
            "agentim_write_rejected_total",
        ] {
            if let Some(rest) = l.strip_prefix(name)
                && let Some(v) = rest.split_whitespace().last()
                && let Ok(v) = v.parse::<f64>()
            {
                let e = out.entry(name.to_string()).or_insert(json!(0.0));
                *e = json!(e.as_f64().unwrap() + v);
            }
        }
    }
    Value::Object(out)
}

// ------------------------------------------------------------------ main

struct Receiver {
    user: usize,
    group: usize,
    recv: Recv,
    conn: Option<Conn>,
}

#[tokio::main]
async fn main() {
    let a = parse_args();
    let tag = &uuid::Uuid::new_v4().simple().to_string()[..8];
    let totals = Arc::new(Totals {
        e2e: Hist::new(),
        ack: Hist::new(),
        pushes: AtomicU64::new(0),
        resume: Notify::new(),
    });
    let mut report = serde_json::Map::new();
    let mut failures: Vec<String> = vec![];
    report.insert("scenario".into(), json!(a.scenario));

    // ---- users and groups
    let t = Instant::now();
    let users = create_users(&a.addr, &format!("sc-{tag}-"), a.users).await;
    let groups_n = a.users.div_ceil(a.group_size);
    println!(
        "setup: {} users in {:.1}s; {} group(s) of up to {}",
        users.len(),
        t.elapsed().as_secs_f64(),
        groups_n,
        a.group_size
    );
    let rss_base = rss_mb(a.server_pid);
    let source = |i: usize| -> u8 { 1 + (i % a.sources.max(1) as usize) as u8 };
    let mut convs: Vec<String> = vec![];
    for g in 0..groups_n {
        let members: Vec<&str> = users
            .iter()
            .skip(g * a.group_size)
            .take(a.group_size)
            .map(|u| u.id.as_str())
            .collect();
        let owner = &users[g * a.group_size];
        let c = open_retry(&a.addr, &owner.token, 1, None, totals.clone())
            .await
            .expect("owner connect");
        let r = c
            .call(json!({"op": "create_conv", "kind": "group", "title": format!("scale {g}"), "members": &members[1..]}))
            .await
            .expect("create group");
        convs.push(r["conv_id"].as_str().unwrap().to_string());
    }

    // ---- receivers: every user, mostly idle
    let slow_n = if a.scenario == "slow" {
        ((a.users as f64) * a.slow_share).round() as usize
    } else {
        0
    };
    let paused = Arc::new(AtomicBool::new(false));
    let mut receivers: Vec<Receiver> = (0..a.users)
        .map(|i| {
            // Slow consumers are the last members, never senders.
            let slow = i >= a.users - slow_n;
            Receiver {
                user: i,
                group: i / a.group_size,
                recv: Recv {
                    view: Arc::default(),
                    paused: slow.then(|| paused.clone()),
                    record_latency: !slow,
                },
                conn: None,
            }
        })
        .collect();
    let t = Instant::now();
    let failed_connects = connect_all(&a, &users, &mut receivers, &totals, source).await;
    let connect_s = t.elapsed().as_secs_f64();
    let rss_conn = rss_mb(a.server_pid);
    println!(
        "connect: {} connections in {:.1}s ({} failed)",
        a.users, connect_s, failed_connects
    );
    report.insert("connections".into(), json!(a.users));
    report.insert("connect_seconds".into(), json!(connect_s));
    if failed_connects > 0 {
        failures.push(format!("{failed_connects} connections could not be opened"));
    }
    if let (Some(b), Some(c)) = (rss_base, rss_conn) {
        let per = (c - b) * 1024.0 / a.users as f64;
        println!(
            "server memory: {b:.0} MB before, {c:.0} MB connected ({per:.1} KB per connection)"
        );
        report.insert("server_kb_per_connection".into(), json!(per));
    }
    tokio::time::sleep(Duration::from_secs(1)).await;

    // Head of every conversation before traffic: receivers must see all after it.
    let probe = open_retry(&a.addr, &users[0].token, 1, None, totals.clone())
        .await
        .unwrap();
    let mut base: Vec<i64> = vec![];
    for (g, conv) in convs.iter().enumerate() {
        let owner = &users[g * a.group_size];
        let c = if g == 0 {
            &probe
        } else {
            &open_retry(&a.addr, &owner.token, 1, None, totals.clone())
                .await
                .unwrap()
        };
        base.push(
            c.pull_seqs(conv, 0)
                .await
                .unwrap()
                .last()
                .copied()
                .unwrap_or(0),
        );
    }

    // ---- senders: their own connections, spread over the groups
    let step = (a.users / a.senders).max(1);
    let sender_ids: Vec<usize> = (0..a.senders).map(|k| (k * step) % a.users).collect();
    let mut senders = vec![];
    for &i in &sender_ids {
        senders.push((
            i,
            open_retry(&a.addr, &users[i].token, 1, None, totals.clone())
                .await
                .expect("sender connect"),
        ));
    }
    if slow_n > 0 {
        paused.store(true, Ordering::Release);
    }
    let padding = "x".repeat(a.payload);
    let started = Instant::now();
    let deadline = started + Duration::from_secs_f64(a.duration);
    let sent = Arc::new(AtomicU64::new(0));
    let errors: Arc<Mutex<Vec<String>>> = Arc::default();
    let mut tasks = vec![];
    for (i, c) in senders {
        let (conv, totals, sent, errors, padding) = (
            convs[i / a.group_size].clone(),
            totals.clone(),
            sent.clone(),
            errors.clone(),
            padding.clone(),
        );
        let interval = (a.rate > 0.0).then(|| Duration::from_secs_f64(a.senders as f64 / a.rate));
        // Senders start spread over one interval, not in lock-step bursts.
        let offset = interval.map(|d| d.mul_f64(tasks.len() as f64 / a.senders as f64));
        tasks.push(tokio::spawn(async move {
            let mut tick = interval.zip(offset).map(|(d, off)| {
                let mut t = tokio::time::interval_at(tokio::time::Instant::now() + off, d);
                t.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                t
            });
            let mut n = 0u64;
            while Instant::now() < deadline {
                if let Some(t) = &mut tick {
                    t.tick().await;
                }
                let t0 = Instant::now();
                let req = json!({"op": "send", "conv_id": conv, "client_msg_id": format!("{i}-{n}"), "text": format!("t={}|{i}-{n} {padding}", now_us())});
                match c.call(req).await {
                    Ok(_) => {
                        totals.ack.record_ms(t0.elapsed().as_secs_f64() * 1000.0);
                        sent.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(e) => errors.lock().unwrap().push(e),
                }
                n += 1;
            }
        }));
    }

    // ---- reconnect storm halfway through
    if a.storm {
        tokio::time::sleep(Duration::from_secs_f64(a.duration / 2.0)).await;
        let t = Instant::now();
        for r in receivers.iter_mut() {
            r.conn = None; // abrupt close, like a network drop
            r.recv.view.lock().unwrap().reconnected = true;
        }
        let failed = connect_all(&a, &users, &mut receivers, &totals, source).await;
        let storm_s = t.elapsed().as_secs_f64();
        println!(
            "storm: {} connections dropped and re-opened in {:.1}s ({} failed) while senders kept writing",
            a.users, storm_s, failed
        );
        report.insert("storm_reconnect_seconds".into(), json!(storm_s));
        if failed > 0 {
            failures.push(format!("{failed} connections could not be re-opened"));
        }
    }
    for t in tasks {
        t.await.unwrap();
    }
    let send_s = started.elapsed().as_secs_f64();
    let sent = sent.load(Ordering::Relaxed);

    // ---- let slow consumers read again, then wait until pushes settle
    if slow_n > 0 {
        tokio::time::sleep(Duration::from_secs(1)).await;
        paused.store(false, Ordering::Release);
        totals.resume.notify_waiters();
    }
    let mut last = u64::MAX;
    let settle = Instant::now();
    while settle.elapsed() < Duration::from_secs(60) {
        tokio::time::sleep(Duration::from_secs(2)).await;
        let now = totals.pushes.load(Ordering::Relaxed);
        if now == last {
            break;
        }
        last = now;
    }
    let rss_end = rss_mb(a.server_pid);

    // ---- verify every receiver
    let mut heads = vec![];
    for (g, conv) in convs.iter().enumerate() {
        let c = open_retry(
            &a.addr,
            &users[g * a.group_size].token,
            1,
            None,
            totals.clone(),
        )
        .await
        .unwrap();
        heads.push(
            c.pull_seqs(conv, 0)
                .await
                .unwrap()
                .last()
                .copied()
                .unwrap_or(0),
        );
    }
    let silent = AtomicU64::new(0);
    let lost = AtomicU64::new(0);
    let healed = AtomicU64::new(0);
    let disorder = AtomicU64::new(0);
    let fast_gaps = AtomicU64::new(0);
    let slow_resynced = AtomicU64::new(0);
    let first_problem: Mutex<Option<String>> = Mutex::new(None);
    let note = |m: String| {
        let mut f = first_problem.lock().unwrap();
        if f.is_none() {
            *f = Some(m);
        }
    };
    stream::iter(receivers.iter())
        .for_each_concurrent(256, |r| {
            let (conv, from, to) = (&convs[r.group], base[r.group] + 1, heads[r.group]);
            let (silent, lost, healed, disorder, fast_gaps, slow_resynced, note) = (
                &silent,
                &lost,
                &healed,
                &disorder,
                &fast_gaps,
                &slow_resynced,
                &note,
            );
            async move {
                let (missing, told, ooo) = {
                    let v = r.recv.view.lock().unwrap();
                    (
                        v.missing(from, to),
                        v.resync || v.reconnected,
                        v.out_of_order,
                    )
                };
                let slow = r.recv.paused.is_some();
                if ooo > 0 {
                    disorder.fetch_add(1, Ordering::Relaxed);
                    note(format!("user {}: {ooo} pushes out of order", r.user));
                }
                if slow && r.recv.view.lock().unwrap().resync {
                    slow_resynced.fetch_add(1, Ordering::Relaxed);
                }
                if missing.is_empty() {
                    return;
                }
                if !slow && !r.recv.view.lock().unwrap().reconnected {
                    fast_gaps.fetch_add(1, Ordering::Relaxed);
                }
                if !told {
                    silent.fetch_add(1, Ordering::Relaxed);
                    note(format!(
                        "user {}: {} events missing (first seq {}) and no resync notice",
                        r.user,
                        missing.len(),
                        missing[0]
                    ));
                    return;
                }
                // Told to resync (or reconnected): pulling must fill the gap.
                let conn = r.conn.as_ref().unwrap();
                match conn.pull_seqs(conv, missing[0] - 1).await {
                    Ok(seqs) => {
                        let mut v = r.recv.view.lock().unwrap();
                        for s in seqs {
                            v.mark(s);
                        }
                        let still = v.missing(from, to);
                        if still.is_empty() {
                            healed.fetch_add(1, Ordering::Relaxed);
                        } else {
                            lost.fetch_add(1, Ordering::Relaxed);
                            note(format!(
                                "user {}: {} events missing even after a pull (first {})",
                                r.user,
                                still.len(),
                                still[0]
                            ));
                        }
                    }
                    Err(e) => {
                        lost.fetch_add(1, Ordering::Relaxed);
                        note(format!("user {}: pull failed: {e}", r.user));
                    }
                }
            }
        })
        .await;

    // ---- report
    let errors = errors.lock().unwrap().clone();
    let events: i64 = heads.iter().zip(&base).map(|(h, b)| h - b).sum();
    let deliveries = totals.e2e.count();
    let e2e = (
        totals.e2e.pct(0.5),
        totals.e2e.pct(0.99),
        totals.e2e.pct(1.0),
    );
    let ack = (
        totals.ack.pct(0.5),
        totals.ack.pct(0.99),
        totals.ack.pct(1.0),
    );
    println!(
        "traffic: {} senders, {} messages acked in {:.1}s ({:.0} msg/s), {} send errors",
        a.senders,
        sent,
        send_s,
        sent as f64 / send_s,
        errors.len()
    );
    println!(
        "         {} events in {} conversation(s); {} pushes received",
        events,
        convs.len(),
        totals.pushes.load(Ordering::Relaxed)
    );
    println!(
        "ack latency:          p50 {:.0}ms  p99 {:.0}ms  max {:.0}ms",
        ack.0, ack.1, ack.2
    );
    println!(
        "end-to-end delivery:  p50 {:.0}ms  p99 {:.0}ms  max {:.0}ms  ({} deliveries measured{})",
        e2e.0,
        e2e.1,
        e2e.2,
        deliveries,
        if slow_n > 0 {
            ", slow consumers excluded"
        } else {
            ""
        }
    );
    let (silent, lost, healed, disorder, fast_gaps, slow_resynced) = (
        silent.into_inner(),
        lost.into_inner(),
        healed.into_inner(),
        disorder.into_inner(),
        fast_gaps.into_inner(),
        slow_resynced.into_inner(),
    );
    println!(
        "consistency: {} receivers complete via push alone, {} completed by pull after resync/reconnect, {} silent gaps, {} with events lost, {} with out-of-order pushes",
        receivers.len() as u64 - healed - silent - lost,
        healed,
        silent,
        lost,
        disorder
    );
    if slow_n > 0 {
        println!(
            "slow consumers: {slow_n} stopped reading; {slow_resynced} were told to resync; {fast_gaps} other receivers had gaps"
        );
        report.insert("slow_consumers".into(), json!(slow_n));
        report.insert("slow_resynced".into(), json!(slow_resynced));
    }
    if let (Some(b), Some(e)) = (rss_base, rss_end) {
        println!("server memory at the end: {e:.0} MB (started at {b:.0} MB)");
        report.insert("server_rss_mb_end".into(), json!(e));
    }
    let counters = server_counters(&a.addr).await;
    println!("server counters: {counters}");
    if let Some(p) = first_problem.lock().unwrap().clone() {
        println!("first problem: {p}");
    }

    if !errors.is_empty() {
        failures.push(format!(
            "{} sends failed, first: {}",
            errors.len(),
            errors[0]
        ));
    }
    if silent > 0 {
        failures.push(format!(
            "{silent} receivers missed events without a resync notice"
        ));
    }
    if lost > 0 {
        failures.push(format!("{lost} receivers could not recover all events"));
    }
    if disorder > 0 {
        failures.push(format!("{disorder} receivers got pushes out of order"));
    }
    if fast_gaps > 0 {
        failures.push(format!(
            "{fast_gaps} receivers that kept reading lost pushes (slow consumers must only affect themselves)"
        ));
    }
    if sent == 0 {
        failures.push("nothing was sent".into());
    }
    if e2e.1 > a.max_p99_ms {
        failures.push(format!("p99 delivery {:.0}ms > {}ms", e2e.1, a.max_p99_ms));
    }
    for (k, v) in [
        ("senders", json!(a.senders)),
        ("acked", json!(sent)),
        ("send_seconds", json!(send_s)),
        ("events", json!(events)),
        ("deliveries_measured", json!(deliveries)),
        ("ack_ms", json!({"p50": ack.0, "p99": ack.1, "max": ack.2})),
        ("e2e_ms", json!({"p50": e2e.0, "p99": e2e.1, "max": e2e.2})),
        ("healed_by_pull", json!(healed)),
        ("silent_gaps", json!(silent)),
        ("lost", json!(lost)),
        ("out_of_order", json!(disorder)),
        ("server", counters),
        ("failures", json!(failures)),
    ] {
        report.insert(k.into(), v);
    }
    if let Some(p) = &a.out {
        std::fs::write(
            p,
            serde_json::to_string_pretty(&Value::Object(report)).unwrap(),
        )
        .unwrap();
    }
    if failures.is_empty() {
        println!("OK");
    } else {
        for f in &failures {
            println!("FAIL: {f}");
        }
        std::process::exit(1);
    }
}

/// Opens a connection for every receiver that has none; returns the failures.
async fn connect_all(
    a: &Args,
    users: &[User],
    receivers: &mut [Receiver],
    totals: &Arc<Totals>,
    source: impl Fn(usize) -> u8,
) -> usize {
    let jobs: Vec<(usize, String, u8, Recv)> = receivers
        .iter()
        .enumerate()
        .filter(|(_, r)| r.conn.is_none())
        .map(|(k, r)| {
            (
                k,
                users[r.user].token.clone(),
                source(r.user),
                r.recv.clone(),
            )
        })
        .collect();
    let results: Vec<(usize, Result<Conn, String>)> = stream::iter(jobs)
        .map(|(k, token, src, recv)| {
            let (addr, totals) = (a.addr.clone(), totals.clone());
            async move { (k, open_retry(&addr, &token, src, Some(recv), totals).await) }
        })
        .buffer_unordered(a.connect_concurrency)
        .collect()
        .await;
    let mut failed = 0;
    for (k, r) in results {
        match r {
            Ok(c) => receivers[k].conn = Some(c),
            Err(e) => {
                if failed == 0 {
                    eprintln!("connect failed: {e}");
                }
                failed += 1;
            }
        }
    }
    failed
}
