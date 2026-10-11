//! Concurrency and consistency test against a running server (`agentim --demo`).
//!
//!   cargo run --release --example loadtest -- --addr 127.0.0.1:8080 --clients 50 --messages 100 --streams 10
//!
//! Many clients send concurrently into one group (with retries that reuse the
//! same client_msg_id) while agents stream concurrently. Afterwards every
//! client's view is checked: contiguous seqs, exactly-once messages, per-sender
//! order, in-order pushes, identical logs across clients, complete streams.
//!
//! Crash mode: `--ack-log FILE` keeps sending until the server dies and records
//! every ACKed message; `--verify-ack-log FILE` (after a restart) checks that
//! every ACKed message survived.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Write;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;

struct Args {
    addr: String,
    clients: usize,
    messages: usize,
    streams: usize,
    chunks: usize,
    ack_log: Option<String>,
    verify_ack_log: Option<String>,
    /// Fail unless acked msg/s is at least this.
    min_throughput: f64,
    /// Fail if the p99 ACK latency exceeds this (ms).
    max_p99_ms: f64,
    /// Write-path mode: every client sends into its own conversation (no fan-out).
    solo: bool,
}

fn parse_args() -> Args {
    let mut a = Args {
        addr: "127.0.0.1:8080".into(),
        clients: 20,
        messages: 50,
        streams: 5,
        chunks: 40,
        ack_log: None,
        verify_ack_log: None,
        min_throughput: 0.0,
        max_p99_ms: f64::INFINITY,
        solo: false,
    };
    let v: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < v.len() {
        if v[i] == "--solo" {
            a.solo = true;
            i += 1;
            continue;
        }
        let val = v.get(i + 1).cloned().unwrap_or_default();
        match v[i].as_str() {
            "--addr" => a.addr = val,
            "--clients" => a.clients = val.parse().unwrap(),
            "--messages" => a.messages = val.parse().unwrap(),
            "--streams" => a.streams = val.parse().unwrap(),
            "--chunks" => a.chunks = val.parse().unwrap(),
            "--ack-log" => a.ack_log = Some(val),
            "--verify-ack-log" => a.verify_ack_log = Some(val),
            "--min-throughput" => a.min_throughput = val.parse().unwrap(),
            "--max-p99-ms" => a.max_p99_ms = val.parse().unwrap(),
            other => panic!("unknown argument {other}"),
        }
        i += 2;
    }
    a
}

async fn http_post(addr: &str, path: &str, body: Value) -> Value {
    let mut s = tokio::net::TcpStream::connect(addr)
        .await
        .expect("connect http");
    let b = body.to_string();
    let req = format!(
        "POST {path} HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{b}",
        b.len()
    );
    s.write_all(req.as_bytes()).await.unwrap();
    let mut buf = Vec::new();
    s.read_to_end(&mut buf).await.unwrap();
    let text = String::from_utf8_lossy(&buf);
    let body = text.split("\r\n\r\n").nth(1).unwrap_or("");
    serde_json::from_str(body).unwrap_or_else(|_| panic!("bad http response: {text}"))
}

struct User {
    id: String,
    name: String,
    token: String,
}

async fn create_user(addr: &str, name: &str, agent: bool, owner: Option<&str>) -> User {
    let r = http_post(
        addr,
        "/api/dev/users",
        json!({"name": name, "agent": agent, "owner": owner}),
    )
    .await;
    User {
        id: r["user"]["id"].as_str().expect("user id").into(),
        name: name.into(),
        token: r["token"].as_str().unwrap().into(),
    }
}

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>>;

struct Client {
    out: mpsc::UnboundedSender<Message>,
    pending: Pending,
    rid: std::sync::atomic::AtomicU64,
    pushes: Arc<Mutex<Vec<Value>>>,
    /// The client's local log per conversation, by seq: what sync returned
    /// and what was pushed live (a push counts as delivered).
    logs: Logs,
}

type Logs = Arc<Mutex<HashMap<String, BTreeMap<i64, Value>>>>;

async fn keep(logs: &Logs, ev: &Value) {
    let (Some(conv), Some(seq)) = (ev["conv_id"].as_str(), ev["seq"].as_i64()) else {
        return;
    };
    logs.lock()
        .await
        .entry(conv.to_string())
        .or_default()
        .insert(seq, ev.clone());
}

impl Client {
    async fn connect(addr: &str, token: &str) -> Result<Client, String> {
        let (ws, _) = tokio_tungstenite::connect_async_with_config(
            format!("ws://{addr}/ws?token={token}"),
            None,
            true,
        )
        .await
        .map_err(|e| e.to_string())?;
        let (mut sink, mut stream) = ws.split();
        let (out, mut out_rx) = mpsc::unbounded_channel::<Message>();
        tokio::spawn(async move {
            while let Some(m) = out_rx.recv().await {
                if sink.send(m).await.is_err() {
                    break;
                }
            }
        });
        let pending: Pending = Arc::default();
        let pushes: Arc<Mutex<Vec<Value>>> = Arc::default();
        let logs: Logs = Arc::default();
        let (p2, pu2, l2) = (pending.clone(), pushes.clone(), logs.clone());
        tokio::spawn(async move {
            while let Some(Ok(Message::Text(t))) = stream.next().await {
                let v: Value = serde_json::from_str(t.as_str()).unwrap();
                if v["type"] == "res" {
                    if let Some(tx) = p2.lock().await.remove(&v["rid"].as_u64().unwrap_or(0)) {
                        let _ = tx.send(v);
                    }
                } else {
                    if v["type"] == "event" {
                        keep(&l2, &v["event"]).await;
                    }
                    pu2.lock().await.push(v);
                }
            }
            p2.lock().await.clear(); // fail outstanding calls
        });
        Ok(Client {
            out,
            pending,
            rid: Default::default(),
            pushes,
            logs,
        })
    }

    async fn call(&self, mut v: Value) -> Result<Value, String> {
        let rid = self.rid.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        v["rid"] = json!(rid);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(rid, tx);
        self.out
            .send(Message::Text(v.to_string().into()))
            .map_err(|_| "closed".to_string())?;
        match tokio::time::timeout(Duration::from_secs(30), rx).await {
            Ok(Ok(r)) => Ok(r),
            Ok(Err(_)) => Err("connection closed".into()),
            Err(_) => Err("timeout".into()),
        }
    }

    async fn ok(&self, v: Value) -> Value {
        let r = self
            .call(v.clone())
            .await
            .unwrap_or_else(|e| panic!("{v}: {e}"));
        assert_eq!(r["ok"], true, "request {v} failed: {r}");
        r
    }

    /// The client's copy of a conversation's log. Clients only sync
    /// (docs/08): take what follows the device position, keep it, acknowledge,
    /// until nothing more is owed.
    async fn pull_all(&self, conv: &str) -> Vec<Value> {
        loop {
            let r = self.call(json!({"op": "sync"})).await.expect("sync");
            if r["error"]["code"] == "rate_limited" {
                // A client waits and syncs again.
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
            assert_eq!(r["ok"], true, "sync failed: {r}");
            let mut acks = vec![];
            for c in r["convs"].as_array().unwrap() {
                for e in c["events"].as_array().unwrap() {
                    keep(&self.logs, &e["event"]).await;
                }
                acks.push(
                    json!({"conv_id": c["conv_id"], "seq": c["upto"], "side_seq": c["side_upto"]}),
                );
            }
            if !acks.is_empty() {
                self.ok(json!({"op": "ack", "acks": acks})).await;
            }
            if r["more"] != true {
                break;
            }
        }
        let logs = self.logs.lock().await;
        logs.get(conv)
            .map(|l| l.values().cloned().collect())
            .unwrap_or_default()
    }
}

fn pct(v: &mut [f64], p: f64) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[((v.len() as f64 - 1.0) * p).round() as usize]
}

#[tokio::main]
async fn main() {
    let a = parse_args();
    if let Some(path) = &a.verify_ack_log {
        return verify_ack_log(&a.addr, path).await;
    }
    let tag = &uuid::Uuid::new_v4().simple().to_string()[..8];
    if a.solo {
        return solo_mode(&a, tag).await;
    }
    let owner = create_user(&a.addr, &format!("lt-{tag}-owner"), false, None).await;
    let mut humans = vec![];
    for i in 0..a.clients {
        humans.push(create_user(&a.addr, &format!("lt-{tag}-u{i}"), false, None).await);
    }
    let mut agents = vec![];
    for i in 0..a.streams {
        agents.push(create_user(&a.addr, &format!("lt-{tag}-a{i}"), true, Some(&owner.id)).await);
    }
    let oc = Client::connect(&a.addr, &owner.token).await.unwrap();
    let members: Vec<&str> = humans
        .iter()
        .chain(agents.iter())
        .map(|u| u.id.as_str())
        .collect();
    let conv = oc.ok(json!({"op": "create_conv", "kind": "group", "title": format!("loadtest {tag}"), "members": members})).await["conv_id"].as_str().unwrap().to_string();

    if let Some(path) = &a.ack_log {
        return ack_log_mode(&a, &conv, &owner, &humans, path).await;
    }

    // Everyone focuses the conversation so stream deltas are pushed too.
    let mut clients = vec![];
    for u in &humans {
        let c = Client::connect(&a.addr, &u.token).await.unwrap();
        c.ok(json!({"op": "focus", "conv_id": conv})).await;
        // A device gets live events after its first sync (docs/08).
        c.pull_all(&conv).await;
        clients.push(Arc::new(c));
    }
    // Agents answer a human prompt explicitly (reply_to), as real agents do;
    // unprompted agent messages would be chained by the loop guard.
    let prompt = oc.ok(json!({"op": "send", "conv_id": conv, "client_msg_id": "prompt", "text": "please answer"})).await["msg_id"].as_str().unwrap().to_string();
    let base_seq = oc.pull_all(&conv).await.len() as i64;
    let started = Instant::now();

    let mut tasks = vec![];
    for (c, u) in clients.iter().zip(humans.iter()) {
        let (c, conv, name, n) = (c.clone(), conv.clone(), u.name.clone(), a.messages);
        tasks.push(tokio::spawn(async move {
            let mut lat = vec![];
            let mut acks = vec![];
            for i in 0..n {
                let req = json!({"op": "send", "conv_id": conv, "client_msg_id": format!("{name}-{i}"), "text": format!("{name} #{i}")});
                let t = Instant::now();
                let r = c.ok(req.clone()).await;
                lat.push(t.elapsed().as_secs_f64() * 1000.0);
                if i % 5 == 0 {
                    let r2 = c.ok(req).await;
                    assert_eq!(r["seq"], r2["seq"], "retry must return the original seq");
                    assert_eq!(r2["duplicate"], true);
                }
                acks.push(r["seq"].as_i64().unwrap());
            }
            assert!(acks.windows(2).all(|w| w[0] < w[1]), "per-sender order violated for {name}");
            lat
        }));
    }
    let mut expected_streams: HashMap<String, String> = HashMap::new();
    let mut stream_tasks = vec![];
    for (i, u) in agents.iter().enumerate() {
        let c = Client::connect(&a.addr, &u.token).await.unwrap();
        let (conv, chunks, prompt) = (conv.clone(), a.chunks, prompt.clone());
        stream_tasks.push(tokio::spawn(async move {
            let r = c.ok(json!({"op": "stream_start", "conv_id": conv, "client_msg_id": format!("stream-{i}"), "reply_to": prompt})).await;
            let id = r["msg_id"].as_str().unwrap().to_string();
            let mut text = String::new();
            for k in 0..chunks {
                let part = format!("[a{i}:{k}]");
                text.push_str(&part);
                c.ok(json!({"op": "stream_append", "msg_id": id, "idx": k, "text": part})).await;
                if k % 7 == 0 {
                    // Retransmitted chunk must be ignored.
                    c.ok(json!({"op": "stream_append", "msg_id": id, "idx": k, "text": part})).await;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            c.ok(json!({"op": "stream_end", "msg_id": id})).await;
            (id, text)
        }));
    }
    let mut lat: Vec<f64> = vec![];
    for t in tasks {
        lat.extend(t.await.expect("sender task"));
    }
    for t in stream_tasks {
        let (id, text) = t.await.expect("stream task");
        expected_streams.insert(id, text);
    }
    let elapsed = started.elapsed();
    tokio::time::sleep(Duration::from_millis(300)).await;

    // ------------------------------------------------------------ verify
    let reference = oc.pull_all(&conv).await;
    let seqs: Vec<i64> = reference
        .iter()
        .map(|e| e["seq"].as_i64().unwrap())
        .collect();
    assert_eq!(
        seqs,
        (1..=seqs.len() as i64).collect::<Vec<_>>(),
        "main log not contiguous"
    );
    let mut created: BTreeMap<String, Value> = BTreeMap::new();
    let mut latest: HashMap<String, Value> = HashMap::new();
    for e in &reference {
        if let Some(m) = e["msg"].as_object() {
            let id = m["msg_id"].as_str().unwrap().to_string();
            if e["type"] == "msg.created" {
                assert!(
                    created.insert(id.clone(), e.clone()).is_none(),
                    "message created twice"
                );
            }
            latest.insert(id, e["msg"].clone());
        }
    }
    let expected_msgs = a.clients * a.messages + a.streams + 1;
    assert_eq!(created.len(), expected_msgs, "message count");
    let texts: HashSet<String> = latest
        .values()
        .map(|m| m["content"].as_str().unwrap().to_string())
        .collect();
    for u in &humans {
        for i in 0..a.messages {
            assert!(
                texts.contains(&format!("{} #{i}", u.name)),
                "missing message {} #{i}",
                u.name
            );
        }
    }
    for (id, text) in &expected_streams {
        let m = &latest[id];
        assert_eq!(m["state"], "complete", "stream {id} state");
        assert_eq!(m["content"].as_str().unwrap(), text, "stream {id} content");
    }
    let fingerprint = |evs: &[Value]| -> Vec<(i64, String, String, i64)> {
        evs.iter()
            .map(|e| {
                (
                    e["seq"].as_i64().unwrap(),
                    e["type"].as_str().unwrap().to_string(),
                    e["msg"]["msg_id"].as_str().unwrap_or("").to_string(),
                    e["msg"]["version"].as_i64().unwrap_or(0),
                )
            })
            .collect()
    };
    let ref_fp = fingerprint(&reference);
    let mut pushed_total = 0usize;
    let mut deltas_total = 0usize;
    for c in &clients {
        assert_eq!(
            fingerprint(&c.pull_all(&conv).await),
            ref_fp,
            "clients disagree on the log"
        );
        let pushes = c.pushes.lock().await;
        let pushed: Vec<i64> = pushes
            .iter()
            .filter(|v| v["type"] == "event")
            .map(|v| v["event"]["seq"].as_i64().unwrap())
            .collect();
        assert!(
            pushed.windows(2).all(|w| w[0] < w[1]),
            "pushes out of order"
        );
        // Pushes plus a gap pull reconstruct the full log.
        let missing = ref_fp
            .iter()
            .filter(|f| f.0 > base_seq && !pushed.contains(&f.0))
            .count();
        assert_eq!(missing, 0, "a client missed {missing} pushed events");
        pushed_total += pushed.len();
        deltas_total += pushes.iter().filter(|v| v["type"] == "delta").count();
    }
    let total_sends = a.clients * a.messages;
    println!("OK  conv={conv}");
    println!(
        "    {} clients x {} messages (+{} retries) and {} concurrent streams x {} chunks in {:.2}s",
        a.clients,
        a.messages,
        a.clients * a.messages.div_ceil(5),
        a.streams,
        a.chunks,
        elapsed.as_secs_f64()
    );
    println!(
        "    throughput: {:.0} acked msg/s",
        total_sends as f64 / elapsed.as_secs_f64()
    );
    println!(
        "    ack latency: p50 {:.2}ms  p99 {:.2}ms  max {:.2}ms",
        pct(&mut lat, 0.5),
        pct(&mut lat, 0.99),
        pct(&mut lat, 1.0)
    );
    println!(
        "    main log: {} events contiguous; {} pushed events and {} coalesced deltas delivered in order",
        seqs.len(),
        pushed_total,
        deltas_total
    );
    let throughput = total_sends as f64 / elapsed.as_secs_f64();
    let p99 = pct(&mut lat, 0.99);
    assert!(
        throughput >= a.min_throughput,
        "performance gate: {throughput:.0} msg/s < {}",
        a.min_throughput
    );
    assert!(
        p99 <= a.max_p99_ms,
        "performance gate: p99 {p99:.1}ms > {}ms",
        a.max_p99_ms
    );
}

async fn ack_log_mode(a: &Args, conv: &str, owner: &User, humans: &[User], path: &str) {
    let file = Arc::new(std::sync::Mutex::new(std::fs::File::create(path).unwrap()));
    writeln!(file.lock().unwrap(), "conv {conv} {}", owner.token).unwrap();
    let mut tasks = vec![];
    for u in humans {
        let c = match Client::connect(&a.addr, &u.token).await {
            Ok(c) => c,
            Err(_) => continue,
        };
        let (conv, name, file) = (conv.to_string(), u.name.clone(), file.clone());
        tasks.push(tokio::spawn(async move {
            let mut n = 0u64;
            loop {
                let r = c.call(json!({"op": "send", "conv_id": conv, "client_msg_id": format!("{name}-{n}"), "text": format!("{name} #{n}")})).await;
                match r {
                    Ok(r) if r["ok"] == true => {
                        let mut f = file.lock().unwrap();
                        writeln!(f, "{}", r["msg_id"].as_str().unwrap()).unwrap();
                        f.flush().unwrap();
                        n += 1;
                    }
                    _ => return n,
                }
            }
        }));
    }
    let mut total = 0;
    for t in tasks {
        total += t.await.unwrap_or(0);
    }
    println!("server went away after {total} ACKed messages (log: {path})");
}

async fn verify_ack_log(addr: &str, path: &str) {
    let text = std::fs::read_to_string(path).unwrap();
    let mut lines = text.lines();
    let head: Vec<&str> = lines.next().unwrap().split(' ').collect();
    let (conv, token) = (head[1], head[2]);
    let acked: Vec<&str> = lines.filter(|l| !l.is_empty()).collect();
    let c = Client::connect(addr, token)
        .await
        .expect("reconnect after restart");
    let events = c.pull_all(conv).await;
    let seqs: Vec<i64> = events.iter().map(|e| e["seq"].as_i64().unwrap()).collect();
    assert_eq!(
        seqs,
        (1..=seqs.len() as i64).collect::<Vec<_>>(),
        "main log not contiguous after restart"
    );
    let present: HashSet<&str> = events
        .iter()
        .filter_map(|e| e["msg"]["msg_id"].as_str())
        .collect();
    let lost: Vec<&&str> = acked.iter().filter(|id| !present.contains(**id)).collect();
    assert!(!acked.is_empty(), "no messages were acked before the crash");
    assert!(
        lost.is_empty(),
        "{} ACKed messages lost after crash: {:?}",
        lost.len(),
        &lost[..lost.len().min(5)]
    );
    println!(
        "OK  all {} ACKed messages survived kill -9 ({} events in log, contiguous)",
        acked.len(),
        seqs.len()
    );
}

/// Every client writes into its own conversation: measures the write path
/// (group commit) without fan-out cost on either side.
async fn solo_mode(a: &Args, tag: &str) {
    let mut users = vec![];
    for i in 0..a.clients {
        users.push(create_user(&a.addr, &format!("lt-{tag}-solo{i}"), false, None).await);
    }
    let mut clients = vec![];
    for u in &users {
        let c = Client::connect(&a.addr, &u.token).await.unwrap();
        let conv = c
            .ok(json!({"op": "create_conv", "kind": "group", "title": "solo", "members": []}))
            .await["conv_id"]
            .as_str()
            .unwrap()
            .to_string();
        clients.push((Arc::new(c), conv, u.name.clone()));
    }
    let started = Instant::now();
    let mut tasks = vec![];
    for (c, conv, name) in &clients {
        let (c, conv, name) = (c.clone(), conv.clone(), name.clone());
        let n = a.messages;
        tasks.push(tokio::spawn(async move {
            let mut lat = vec![];
            for i in 0..n {
                let req = json!({"op": "send", "conv_id": conv, "client_msg_id": format!("{name}-{i}"), "text": format!("{name} #{i}")});
                let t = Instant::now();
                let r = c.ok(req.clone()).await;
                lat.push(t.elapsed().as_secs_f64() * 1000.0);
                if i % 5 == 0 {
                    assert_eq!(c.ok(req).await["seq"], r["seq"], "retry must return the original seq");
                }
            }
            lat
        }));
    }
    let mut lat = vec![];
    for t in tasks {
        lat.extend(t.await.unwrap());
    }
    let elapsed = started.elapsed();
    for (c, conv, _) in &clients {
        let evs = c.pull_all(conv).await;
        let seqs: Vec<i64> = evs.iter().map(|e| e["seq"].as_i64().unwrap()).collect();
        assert_eq!(
            seqs,
            (1..=seqs.len() as i64).collect::<Vec<_>>(),
            "contiguous"
        );
        assert_eq!(
            evs.iter().filter(|e| e["type"] == "msg.created").count(),
            a.messages,
            "exactly once"
        );
    }
    let total = a.clients * a.messages;
    let throughput = total as f64 / elapsed.as_secs_f64();
    println!(
        "OK  solo write path: {} clients x {} messages (+{} retries) in {:.2}s",
        a.clients,
        a.messages,
        a.clients * a.messages.div_ceil(5),
        elapsed.as_secs_f64()
    );
    println!("    throughput: {throughput:.0} acked msg/s");
    let p99 = pct(&mut lat, 0.99);
    println!(
        "    ack latency: p50 {:.2}ms  p99 {:.2}ms  max {:.2}ms",
        pct(&mut lat, 0.5),
        p99,
        pct(&mut lat, 1.0)
    );
    assert!(
        throughput >= a.min_throughput,
        "performance gate: {throughput:.0} msg/s < {}",
        a.min_throughput
    );
    assert!(
        p99 <= a.max_p99_ms,
        "performance gate: p99 {p99:.1}ms > {}ms",
        a.max_p99_ms
    );
}
