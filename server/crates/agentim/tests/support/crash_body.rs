// Shared by the AgentIM and AgentIM Lite crash tests; each defines `BIN`.
//
// Process crash recovery, portable across Linux and Windows: the server
// binary is killed (SIGKILL / TerminateProcess) while clients are sending,
// restarted on the same database, and every message that was acknowledged
// before the kill must be there, in a gap-free log.

use std::collections::HashMap;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_tungstenite::tungstenite::Message;

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn spawn(db: &str, port: u16, log: &std::path::Path) -> Child {
    let out = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)
        .unwrap();
    Command::new(BIN)
        .args([
            "--demo",
            "--db",
            db,
            "--addr",
            &format!("127.0.0.1:{port}"),
            "--no-exit-on-storage-failure",
        ])
        .env("RUST_LOG", "info")
        .stdout(Stdio::from(out.try_clone().unwrap()))
        .stderr(Stdio::from(out))
        .spawn()
        .expect("start server")
}

async fn http(addr: &str, method: &str, path: &str, body: &str) -> Option<(u16, String)> {
    let mut s = tokio::net::TcpStream::connect(addr).await.ok()?;
    let req = format!(
        "{method} {path} HTTP/1.1\r\nHost: x\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    s.write_all(req.as_bytes()).await.ok()?;
    let mut buf = Vec::new();
    s.read_to_end(&mut buf).await.ok()?;
    let t = String::from_utf8_lossy(&buf).to_string();
    Some((
        t.get(9..12)?.parse().ok()?,
        t.split("\r\n\r\n").nth(1).unwrap_or("").to_string(),
    ))
}

async fn wait_ready(addr: &str, child: &mut Child, log: &std::path::Path) {
    let t = Instant::now();
    while t.elapsed() < Duration::from_secs(60) {
        if let Some((200, _)) = http(addr, "GET", "/readyz", "").await {
            return;
        }
        if let Ok(Some(status)) = child.try_wait() {
            panic!(
                "server exited with {status}:\n{}",
                std::fs::read_to_string(log).unwrap_or_default()
            );
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!(
        "server did not become ready:\n{}",
        std::fs::read_to_string(log).unwrap_or_default()
    );
}

async fn dev_user(addr: &str, name: &str) -> (String, String) {
    let (_, body) = http(
        addr,
        "POST",
        "/api/dev/users",
        &json!({"name": name}).to_string(),
    )
    .await
    .unwrap();
    let v: Value = serde_json::from_str(&body).unwrap();
    (
        v["user"]["id"].as_str().unwrap().into(),
        v["token"].as_str().unwrap().into(),
    )
}

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn request(ws: &mut Ws, rid: u64, mut v: Value) -> Option<Value> {
    v["rid"] = json!(rid);
    ws.send(Message::Text(v.to_string().into())).await.ok()?;
    loop {
        match tokio::time::timeout(Duration::from_secs(10), ws.next()).await {
            Ok(Some(Ok(Message::Text(t)))) => {
                let f: Value = serde_json::from_str(t.as_str()).ok()?;
                if f["type"] == "res" && f["rid"] == rid {
                    return Some(f);
                }
            }
            _ => return None,
        }
    }
}

async fn connect(addr: &str, token: &str) -> Ws {
    tokio_tungstenite::connect_async_with_config(
        format!("ws://{addr}/ws?token={token}"),
        None,
        true,
    )
    .await
    .expect("ws connect")
    .0
}

async fn crash_round(crash_after: Duration) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("crash.db").to_string_lossy().to_string();
    let port = free_port();
    let addr = format!("127.0.0.1:{port}");
    let log = dir.path().join("server.log");
    let mut child = spawn(&db, port, &log);
    wait_ready(&addr, &mut child, &log).await;

    let (_, owner_token) = dev_user(&addr, "owner").await;
    let mut senders = vec![];
    for i in 0..8 {
        senders.push(dev_user(&addr, &format!("s{i}")).await);
    }
    let mut ows = connect(&addr, &owner_token).await;
    let ids: Vec<String> = senders.iter().map(|(id, _)| id.clone()).collect();
    let conv = request(
        &mut ows,
        1,
        json!({"op": "create_conv", "kind": "group", "title": "crash", "members": ids}),
    )
    .await
    .unwrap()["conv_id"]
        .as_str()
        .unwrap()
        .to_string();

    let acked: Arc<Mutex<HashMap<String, i64>>> = Arc::default();
    let mut tasks = vec![];
    for (i, (_, token)) in senders.iter().enumerate() {
        let (addr, token, conv, acked) = (addr.clone(), token.clone(), conv.clone(), acked.clone());
        tasks.push(tokio::spawn(async move {
            let mut ws = connect(&addr, &token).await;
            for n in 0u64.. {
                let r = request(&mut ws, n + 1, json!({"op": "send", "conv_id": conv, "client_msg_id": format!("{i}-{n}"), "text": format!("{i}-{n}")})).await;
                match r {
                    Some(r) if r["ok"] == true => {
                        acked.lock().unwrap().insert(r["msg_id"].as_str().unwrap().to_string(), r["seq"].as_i64().unwrap());
                    }
                    _ => return,
                }
            }
        }));
    }
    tokio::time::sleep(crash_after).await;
    child.kill().expect("kill server"); // SIGKILL on Unix, TerminateProcess on Windows
    let _ = child.wait();
    for t in tasks {
        let _ = t.await;
    }
    let acked = acked.lock().unwrap().clone();
    assert!(
        !acked.is_empty(),
        "nothing was acknowledged before the crash"
    );

    let mut child = spawn(&db, port, &log);
    wait_ready(&addr, &mut child, &log).await;
    let mut ows = connect(&addr, &owner_token).await;
    let mut seen: HashMap<String, i64> = HashMap::new();
    let mut seqs = vec![];
    // A client only syncs (docs/08): take what follows its position, store
    // it, acknowledge, until nothing more is owed.
    for round in 0..1000u64 {
        let p = request(&mut ows, 1000 + round, json!({"op": "sync"}))
            .await
            .unwrap();
        let mut acks = vec![];
        for c in p["convs"].as_array().unwrap() {
            if c["conv_id"] != conv.as_str() {
                continue;
            }
            for e in c["events"].as_array().unwrap() {
                seqs.push(e["event"]["seq"].as_i64().unwrap());
                if let Some(id) = e["event"]["msg"]["msg_id"].as_str() {
                    seen.insert(
                        id.to_string(),
                        e["event"]["msg"]["created_seq"].as_i64().unwrap(),
                    );
                }
            }
            acks.push(json!({"conv_id": conv, "seq": c["upto"], "side_seq": c["side_upto"]}));
        }
        if !acks.is_empty() {
            request(&mut ows, 5000 + round, json!({"op": "ack", "acks": acks}))
                .await
                .unwrap();
        }
        if p["more"] != true {
            break;
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    assert_eq!(
        seqs,
        (1..=seqs.len() as i64).collect::<Vec<_>>(),
        "log is gap-free after recovery"
    );
    let lost: Vec<_> = acked
        .iter()
        .filter(|(id, seq)| seen.get(*id) != Some(seq))
        .collect();
    assert!(
        lost.is_empty(),
        "{} acknowledged messages lost after kill: {:?}",
        lost.len(),
        lost.iter().take(3).collect::<Vec<_>>()
    );
    eprintln!(
        "crash after {crash_after:?}: {} acknowledged messages all survived",
        acked.len()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn acknowledged_messages_survive_kill() {
    for ms in [300, 1000, 2000] {
        crash_round(Duration::from_millis(ms)).await;
    }
}
