//! The native runtime (docs/12 §2.2): one thread owns the core and runs its
//! effects — WebSocket (rustls, the system's certificates), the SQLite
//! cache, timers. The app talks to it through [`Client`] (Rust) or the C
//! ABI; it hears back through one callback with JSON events, on the
//! runtime's thread (wrappers move them to their UI thread).

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;

use agentim_client_core::{ClientError, Config, Core, Effect, Input, PutResult};
use futures_util::{SinkExt, StreamExt, stream::SplitSink};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::{self, Message};

use crate::store::{Store, failure_kind};

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;
type Sink = SplitSink<Ws, Message>;

/// Receives every event as JSON (`{"type": …}`), on the runtime's thread.
pub type Listener = Box<dyn Fn(&Value) + Send + 'static>;

enum In {
    Command(u64, Value),
    Token(Result<String, bool>),
    Network(bool, u64),
    Foreground(bool),
    Shutdown,
    Opened(u64, Box<Sink>),
    Failed(u64, Option<u16>, Option<i64>, String),
    Frame(u64, String),
    Closed(u64, u16, String),
    /// A `send_file {path}` made ready for the core.
    Prepared(u64, Result<Value, ClientError>),
    PutDone(String, String, String, PutResult),
    PutProgress(String, String, String, u64),
}

/// A running client. Dropping it stops the runtime.
pub struct Client {
    tx: mpsc::UnboundedSender<In>,
    thread: Option<std::thread::JoinHandle<()>>,
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

impl Client {
    /// Starts the runtime: the cache in `dir` (in memory without one).
    pub fn start(mut cfg: Config, dir: Option<PathBuf>, on_event: Listener) -> Client {
        if cfg.seed == 0 {
            let mut b = [0u8; 8];
            let _ = getrandom::fill(&mut b);
            cfg.seed = u64::from_le_bytes(b);
        }
        let (tx, rx) = mpsc::unbounded_channel();
        let tx2 = tx.clone();
        let thread = std::thread::Builder::new()
            .name("agentim-client".into())
            .spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("tokio runtime");
                rt.block_on(run(cfg, dir, on_event, rx, tx2));
            })
            .expect("runtime thread");
        Client {
            tx,
            thread: Some(thread),
        }
    }

    /// A command (`{"cmd": …}`); its answer comes as `{"type": "reply", "id": …}`.
    pub fn command(&self, id: u64, cmd: Value) {
        let _ = self.tx.send(In::Command(id, cmd));
    }

    /// The answer to `need_token`: a token, or the backend's refusal
    /// (`Err(true)`: stop) or failure (`Err(false)`: try again later).
    pub fn token(&self, t: Result<String, bool>) {
        let _ = self.tx.send(In::Token(t));
    }

    pub fn network(&self, online: bool, generation: u64) {
        let _ = self.tx.send(In::Network(online, generation));
    }

    pub fn foreground(&self, f: bool) {
        let _ = self.tx.send(In::Foreground(f));
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.tx.send(In::Shutdown);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn tls() -> Arc<rustls::ClientConfig> {
    let mut roots = rustls::RootCertStore::empty();
    for c in rustls_native_certs::load_native_certs().certs {
        let _ = roots.add(c);
    }
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    Arc::new(
        rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .expect("protocol versions")
            .with_root_certificates(roots)
            .with_no_client_auth(),
    )
}

async fn run(
    cfg: Config,
    dir: Option<PathBuf>,
    on_event: Listener,
    mut rx: mpsc::UnboundedReceiver<In>,
    tx: mpsc::UnboundedSender<In>,
) {
    let emit = |v: Value| {
        // A panicking listener must not take the runtime down.
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| on_event(&v)));
    };
    let mut core = Core::new(cfg);
    let mut store = match Store::open(dir.as_deref()) {
        Ok(s) => Some(s),
        Err(e) => {
            emit(json!({"type": "log", "message": format!("cache unavailable: {e}")}));
            None
        }
    };
    let mut first = vec![];
    let snapshot = match store.as_ref().map(|s| s.snapshot()) {
        Some(Ok(s)) => s,
        Some(Err(e)) => {
            // Damaged: rebuilt from the server.
            first.push(Input::StorageFailed {
                kind: failure_kind(&e).into(),
            });
            if let Some(s) = store.as_mut() {
                let _ = s.wipe();
            }
            Default::default()
        }
        None => {
            first.push(Input::StorageFailed {
                kind: "unavailable".into(),
            });
            Default::default()
        }
    };
    let tls = tls();
    let http = reqwest::Client::builder()
        .tls_backend_preconfigured((*tls).clone())
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(std::time::Duration::from_secs(15))
        .build()
        .expect("http client");
    // Copies made for uploads (without metadata, thumbnails), until the
    // core releases them; PUTs running, per operation.
    let uploads = dir
        .as_ref()
        .map(|d| d.join("uploads"))
        .unwrap_or_else(|| std::env::temp_dir().join("agentim-uploads"));
    let mut puts: HashMap<String, Vec<tokio::task::AbortHandle>> = HashMap::new();
    let mut owned: HashMap<String, HashSet<PathBuf>> = HashMap::new();
    let mut swept = false;
    let mut sink: Option<(u64, Box<Sink>)> = None;
    let mut conn: u64 = 0;
    let mut full = false;
    let mut writes = 0u64;
    let mut inputs: VecDeque<Input> = VecDeque::new();
    inputs.push_back(Input::Restored(snapshot));
    inputs.extend(first);
    loop {
        // Feed the core everything pending and carry out what it says.
        while let Some(i) = inputs.pop_front() {
            let fx = core.handle(i, now_ms());
            for e in fx {
                match e {
                    Effect::NeedToken => emit(json!({"type": "need_token"})),
                    Effect::Connect { url, token } => {
                        conn += 1;
                        let (c, tx, tls) = (conn, tx.clone(), tls.clone());
                        if let Some((_, mut s)) = sink.take() {
                            let _ = s.close().await;
                        }
                        tokio::spawn(async move { connect(c, url, token, tls, tx).await });
                    }
                    Effect::Send(text) => {
                        if let Some((_, s)) = sink.as_mut()
                            && s.send(Message::text(text)).await.is_err()
                        {
                            sink = None;
                            conn += 1;
                            inputs.push_back(Input::Closed {
                                code: 1006,
                                reason: String::new(),
                            });
                        }
                    }
                    Effect::Close { code, reason } => {
                        if let Some((_, mut s)) = sink.take() {
                            let frame = tungstenite::protocol::CloseFrame {
                                code: code.into(),
                                reason: reason.into(),
                            };
                            let _ = s.send(Message::Close(Some(frame))).await;
                        }
                        // Frames of that connection are ignored from now.
                        conn += 1;
                    }
                    Effect::Persist(b) => match store.as_mut().map(|s| s.write(&b)) {
                        Some(Ok(())) => {
                            writes += 1;
                            if full && let Some(s) = store.as_ref() {
                                s.ensure_reserve();
                                full = false;
                                inputs.push_back(Input::StorageRecovered);
                            }
                            if writes.is_multiple_of(200)
                                && let Some(s) = store.as_ref()
                            {
                                s.vacuum_some();
                            }
                            inputs.push_back(Input::Persisted(b.id));
                        }
                        Some(Err(e)) => {
                            let kind = failure_kind(&e);
                            full = kind == "full";
                            inputs.push_back(Input::StorageFailed { kind: kind.into() });
                        }
                        None => {}
                    },
                    Effect::LoadPage {
                        req,
                        conv_id,
                        before_seq,
                        limit,
                    } => {
                        let messages = store
                            .as_ref()
                            .and_then(|s| s.page(&conv_id, before_seq, limit).ok())
                            .unwrap_or_default();
                        inputs.push_back(Input::PageLoaded {
                            req,
                            conv_id,
                            messages,
                        });
                    }
                    Effect::WipeStore => {
                        if let Some(s) = store.as_mut() {
                            let _ = s.wipe();
                        }
                    }
                    Effect::Emit(v) => emit(v),
                    Effect::Put {
                        op_id,
                        file_id,
                        key,
                        url,
                        headers,
                        source,
                        offset,
                        len,
                    } => {
                        let p = PathBuf::from(&source);
                        if p.starts_with(&uploads) {
                            owned.entry(op_id.clone()).or_default().insert(p);
                        }
                        let (http, tx) = (http.clone(), tx.clone());
                        let (o, f, k) = (op_id.clone(), file_id.clone(), key.clone());
                        let progress = {
                            let (tx, o, f, k) = (tx.clone(), o.clone(), f.clone(), k.clone());
                            move |n| {
                                let _ =
                                    tx.send(In::PutProgress(o.clone(), f.clone(), k.clone(), n));
                            }
                        };
                        let h = tokio::spawn(async move {
                            let r = crate::files::put(
                                &http, &url, &headers, &source, offset, len, progress,
                            )
                            .await;
                            let _ = tx.send(In::PutDone(o, f, k, r));
                        });
                        let list = puts.entry(op_id).or_default();
                        list.retain(|a| !a.is_finished());
                        list.push(h.abort_handle());
                    }
                    Effect::ReleaseUpload { op_id } => {
                        for a in puts.remove(&op_id).unwrap_or_default() {
                            a.abort();
                        }
                        for p in owned.remove(&op_id).unwrap_or_default() {
                            let _ = std::fs::remove_file(p);
                        }
                    }
                    Effect::Reply { id, result } => emit(match result {
                        Ok(v) => json!({"type": "reply", "id": id, "ok": true, "result": v}),
                        Err(e) => {
                            json!({"type": "reply", "id": id, "ok": false, "error": e.to_json()})
                        }
                    }),
                }
            }
        }
        if !swept {
            // Copies left by an earlier run that no queued upload names.
            swept = true;
            sweep_uploads(&uploads, &core.debug_state());
        }
        let wait = core
            .next_deadline()
            .map(|d| std::time::Duration::from_millis((d - now_ms()).max(0) as u64));
        let msg = match wait {
            Some(w) => tokio::select! {
                m = rx.recv() => m,
                _ = tokio::time::sleep(w) => { inputs.push_back(Input::Tick); continue; }
            },
            None => rx.recv().await,
        };
        let Some(msg) = msg else { break };
        match msg {
            In::Shutdown => break,
            In::Command(id, cmd) if cmd["cmd"] == "send_file" && cmd.get("path").is_some() => {
                // Reading, removing metadata and the thumbnail take a while.
                let (tx, dir) = (tx.clone(), uploads.clone());
                tokio::task::spawn_blocking(move || {
                    let _ = tx.send(In::Prepared(id, crate::files::prepare(&cmd, &dir)));
                });
            }
            In::Command(id, cmd) => inputs.push_back(Input::Command { id, cmd }),
            In::Prepared(id, Ok(cmd)) => inputs.push_back(Input::Command { id, cmd }),
            In::Prepared(id, Err(e)) => {
                emit(json!({"type": "reply", "id": id, "ok": false, "error": e.to_json()}))
            }
            In::PutDone(op_id, file_id, key, result) => inputs.push_back(Input::PutDone {
                op_id,
                file_id,
                key,
                result,
            }),
            In::PutProgress(op_id, file_id, key, sent) => inputs.push_back(Input::PutProgress {
                op_id,
                file_id,
                key,
                sent,
            }),
            In::Token(t) => inputs.push_back(Input::Token(t)),
            In::Network(online, generation) => {
                inputs.push_back(Input::Network { online, generation })
            }
            In::Foreground(f) => inputs.push_back(Input::Foreground(f)),
            In::Opened(c, s) if c == conn => {
                sink = Some((c, s));
                inputs.push_back(Input::Opened);
            }
            In::Opened(_, mut s) => {
                let _ = s.close().await;
            }
            In::Failed(c, status, retry_after_ms, detail) if c == conn => {
                inputs.push_back(Input::ConnectFailed {
                    status,
                    retry_after_ms,
                    detail,
                })
            }
            In::Frame(c, f) if c == conn => inputs.push_back(Input::Frame(f)),
            In::Closed(c, code, reason) if c == conn => {
                sink = None;
                inputs.push_back(Input::Closed { code, reason });
            }
            _ => {}
        }
    }
    if let Some((_, mut s)) = sink.take() {
        let _ = s.close().await;
    }
}

/// Deletes files in `dir` that no queued upload uses.
fn sweep_uploads(dir: &std::path::Path, state: &Value) {
    let mut keep: HashSet<PathBuf> = HashSet::new();
    for o in state["queue"].as_array().into_iter().flatten() {
        for s in [&o["upload"]["source"], &o["upload"]["thumb"]["source"]] {
            if let Some(s) = s.as_str() {
                keep.insert(PathBuf::from(s));
            }
        }
    }
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        if !keep.contains(&e.path()) {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// Opens one WebSocket and forwards what it receives.
async fn connect(
    c: u64,
    url: String,
    token: String,
    tls: Arc<rustls::ClientConfig>,
    tx: mpsc::UnboundedSender<In>,
) {
    use tungstenite::client::IntoClientRequest;
    let cfg = tungstenite::protocol::WebSocketConfig::default().max_message_size(Some(16 << 20));
    // The token in a header, never in the URL (D129).
    let req = url.as_str().into_client_request().and_then(|mut r| {
        let v = format!("Bearer {token}").parse().map_err(
            |e: tungstenite::http::header::InvalidHeaderValue| {
                tungstenite::Error::HttpFormat(e.into())
            },
        )?;
        r.headers_mut().insert("authorization", v);
        Ok(r)
    });
    let r = match req {
        Ok(req) => {
            tokio_tungstenite::connect_async_tls_with_config(
                req,
                Some(cfg),
                true,
                Some(tokio_tungstenite::Connector::Rustls(tls)),
            )
            .await
        }
        Err(e) => Err(e),
    };
    let ws = match r {
        Ok((ws, _)) => ws,
        Err(tungstenite::Error::Http(resp)) => {
            let status = resp.status().as_u16();
            let retry = resp
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse::<i64>().ok())
                .map(|s| s * 1000);
            let _ = tx.send(In::Failed(c, Some(status), retry, format!("HTTP {status}")));
            return;
        }
        Err(e) => {
            let _ = tx.send(In::Failed(c, None, None, e.to_string()));
            return;
        }
    };
    let (sink, mut stream) = ws.split();
    if tx.send(In::Opened(c, Box::new(sink))).is_err() {
        return;
    }
    while let Some(m) = stream.next().await {
        match m {
            Ok(Message::Text(t)) => {
                if tx.send(In::Frame(c, t.to_string())).is_err() {
                    return;
                }
            }
            Ok(Message::Close(f)) => {
                let (code, reason) = f
                    .map(|f| (u16::from(f.code), f.reason.to_string()))
                    .unwrap_or((1005, String::new()));
                let _ = tx.send(In::Closed(c, code, reason));
                return;
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
    let _ = tx.send(In::Closed(c, 1006, String::new()));
}
