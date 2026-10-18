//! Connections between nodes (docs/11 §6): TLS with the cluster's own
//! certificates on both ends, a HELLO each way, then frames. Each node
//! dials every other node and sends its requests and publishes on that
//! connection; the connection it accepts from a peer serves the peer's.
//! Writers only queue frames, so a slow peer never holds up a commit: a
//! full queue closes the connection instead.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncWriteExt, split};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Notify, mpsc, oneshot};

use crate::codec::{self, ForwardResp, Hello, Kind};

/// Frames queued per connection before it is closed.
pub const QUEUE: usize = 100_000;
pub const PING_EVERY: Duration = Duration::from_secs(2);
/// A connection that receives nothing for this long is closed.
pub const IDLE: Duration = Duration::from_secs(10);
const HANDSHAKE: Duration = Duration::from_secs(5);

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<ForwardResp>>>>;

/// Why a request got no answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unsent {
    /// Never left this node.
    NotSent,
    /// Sent; the outcome is unknown.
    Unknown,
}

/// Network faults for tests (docs/11 §12.1): frames to the nodes in
/// `cut_to` are dropped and no connection is made to them; frames from the
/// nodes in `cut_from` are ignored (no answers, no pongs).
#[derive(Default)]
pub struct Faults {
    pub cut_to: Mutex<std::collections::HashSet<String>>,
    pub cut_from: Mutex<std::collections::HashSet<String>>,
    /// Frames to these nodes wait this long before they are written (in
    /// order).
    pub delay_to: Mutex<HashMap<String, Duration>>,
}

impl Faults {
    pub fn is_cut_to(&self, node: &str) -> bool {
        lock(&self.cut_to).contains(node)
    }
    pub fn is_cut_from(&self, node: &str) -> bool {
        lock(&self.cut_from).contains(node)
    }
    pub fn delay(&self, node: &str) -> Option<Duration> {
        lock(&self.delay_to).get(node).copied()
    }
}

/// The outgoing connection to one peer (one boot of a node).
pub struct Peer {
    pub node_id: String,
    pub boot_id: String,
    pub addr: String,
    tx: Mutex<Option<mpsc::Sender<Vec<u8>>>>,
    pending: Pending,
    next: AtomicU64,
    kill: Notify,
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    faults: Arc<Faults>,
    /// Test fault: frames kept back (in order) until released.
    held: Mutex<Option<Vec<Vec<u8>>>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Peer {
    /// Starts dialing; `me` is this node's HELLO.
    pub fn start(
        node_id: String,
        boot_id: String,
        addr: String,
        me: Hello,
        tls: Arc<rustls::ClientConfig>,
        faults: Arc<Faults>,
    ) -> Arc<Peer> {
        let p = Arc::new(Peer {
            node_id,
            boot_id,
            addr,
            tx: Mutex::new(None),
            pending: Default::default(),
            next: AtomicU64::new(1),
            kill: Notify::new(),
            task: Mutex::new(None),
            faults,
            held: Mutex::new(None),
        });
        let t = tokio::spawn(p.clone().dial_loop(me, tls));
        *lock(&p.task) = Some(t);
        p
    }

    pub fn connected(&self) -> bool {
        lock(&self.tx).is_some()
    }

    pub fn stop(&self) {
        if let Some(t) = lock(&self.task).take() {
            t.abort();
        }
        lock(&self.tx).take();
        lock(&self.pending).clear();
    }

    /// Ends the current connection (it is dialed again).
    pub fn stop_session(&self) {
        lock(&self.tx).take();
        self.kill.notify_one();
    }

    /// Queues a frame; false when not connected. A full queue closes the
    /// connection (the peer resyncs its clients when it is back).
    /// Test fault: keep frames back from now on.
    pub fn hold(&self) {
        let mut h = lock(&self.held);
        if h.is_none() {
            *h = Some(vec![]);
        }
    }

    /// Lets held frames go, in order, before any later one.
    pub fn release(&self) {
        let mut h = lock(&self.held);
        for f in h.take().unwrap_or_default() {
            self.send_now(f);
        }
    }

    pub fn send(&self, frame: Vec<u8>) -> bool {
        if self.faults.is_cut_to(&self.node_id) {
            return false;
        }
        let mut h = lock(&self.held);
        if let Some(buf) = h.as_mut() {
            buf.push(frame);
            return true;
        }
        self.send_now(frame)
    }

    fn send_now(&self, frame: Vec<u8>) -> bool {
        let mut g = lock(&self.tx);
        let Some(tx) = g.as_ref() else { return false };
        match tx.try_send(frame) {
            Ok(()) => true,
            Err(mpsc::error::TrySendError::Full(_)) => {
                tracing::warn!(
                    peer = self.node_id,
                    "peer queue full; closing the connection"
                );
                metrics::counter!("agentim_cluster_peer_overflows_total").increment(1);
                g.take();
                self.kill.notify_one();
                false
            }
            Err(_) => {
                g.take();
                false
            }
        }
    }

    /// A request and its answer. `NotSent` (not connected) may be tried
    /// elsewhere; `Unknown` (connection lost or no answer in time) must not
    /// be resent, its outcome is open.
    pub async fn request(
        &self,
        db: usize,
        shard: u32,
        op: serde_json::Value,
        wait: Duration,
    ) -> Result<ForwardResp, Unsent> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        lock(&self.pending).insert(id, tx);
        let f = codec::encode_json(Kind::ForwardReq, &codec::ForwardReq { id, db, shard, op })
            .map_err(|_| Unsent::NotSent)?;
        if !self.send(f) {
            lock(&self.pending).remove(&id);
            return Err(Unsent::NotSent);
        }
        let r = tokio::time::timeout(wait, rx).await;
        lock(&self.pending).remove(&id);
        match r {
            Ok(Ok(r)) => Ok(r),
            _ => Err(Unsent::Unknown),
        }
    }

    async fn dial_loop(self: Arc<Peer>, me: Hello, tls: Arc<rustls::ClientConfig>) {
        let mut backoff = Duration::from_millis(100);
        loop {
            match self.session(&me, &tls).await {
                Ok(()) => backoff = Duration::from_millis(100),
                Err(e) => tracing::debug!(peer = self.node_id, error = e, "peer connection ended"),
            }
            lock(&self.tx).take();
            lock(&self.pending).clear();
            let jitter = Duration::from_millis(u64::from(uuid::Uuid::new_v4().as_bytes()[0]) % 100);
            tokio::time::sleep(backoff + jitter).await;
            backoff = (backoff * 2).min(Duration::from_secs(5));
        }
    }

    async fn session(&self, me: &Hello, tls: &Arc<rustls::ClientConfig>) -> Result<(), String> {
        if self.faults.is_cut_to(&self.node_id) {
            return Err("cut (test fault)".into());
        }
        let tcp = tokio::time::timeout(HANDSHAKE, TcpStream::connect(&self.addr))
            .await
            .map_err(|_| "connect timed out")?
            .map_err(|e| e.to_string())?;
        let _ = tcp.set_nodelay(true);
        let conn = tokio_rustls::TlsConnector::from(tls.clone());
        let mut s = tokio::time::timeout(HANDSHAKE, conn.connect(crate::ca::server_name(), tcp))
            .await
            .map_err(|_| "TLS handshake timed out")?
            .map_err(|e| e.to_string())?;
        s.write_all(&codec::encode_json(Kind::Hello, me)?)
            .await
            .map_err(|e| e.to_string())?;
        let theirs = read_hello(&mut s).await?;
        if theirs.cluster_id != me.cluster_id
            || theirs.node_id != self.node_id
            || theirs.boot_id != self.boot_id
        {
            return Err(format!(
                "{} answered as {}:{} of cluster {}",
                self.addr, theirs.node_id, theirs.boot_id, theirs.cluster_id
            ));
        }
        codec::negotiate(
            (me.proto_min, me.proto_max),
            (theirs.proto_min, theirs.proto_max),
        )
        .ok_or("no common protocol version")?;
        let (mut rd, mut wr) = split(s);
        let (tx, mut rx) = mpsc::channel::<Vec<u8>>(QUEUE);
        *lock(&self.tx) = Some(tx);
        let writer = async {
            let mut ping = tokio::time::interval(PING_EVERY);
            loop {
                tokio::select! {
                    f = rx.recv() => match f {
                        Some(f) => {
                            if let Some(d) = self.faults.delay(&self.node_id) {
                                tokio::time::sleep(d).await;
                            }
                            wr.write_all(&f).await.map_err(|e| e.to_string())?
                        }
                        None => return Ok::<(), String>(()),
                    },
                    _ = ping.tick() => {
                        wr.write_all(&codec::encode(Kind::Ping, b"")?).await.map_err(|e| e.to_string())?;
                    }
                }
            }
        };
        let reader = async {
            loop {
                let f = tokio::time::timeout(IDLE, codec::read(&mut rd))
                    .await
                    .map_err(|_| "peer silent".to_string())??;
                let Some((kind, body)) = f else {
                    return Ok::<(), String>(());
                };
                if kind == Some(Kind::ForwardResp) {
                    let r: ForwardResp =
                        serde_json::from_slice(&body).map_err(|e| e.to_string())?;
                    if let Some(w) = lock(&self.pending).remove(&r.id) {
                        let _ = w.send(r);
                    }
                }
            }
        };
        tokio::select! {
            r = writer => r,
            r = reader => r,
            _ = self.kill.notified() => Err("closed".into()),
        }
    }
}

async fn read_hello<S: tokio::io::AsyncRead + Unpin>(s: &mut S) -> Result<Hello, String> {
    match tokio::time::timeout(HANDSHAKE, codec::read(s)).await {
        Ok(Ok(Some((Some(Kind::Hello), body)))) => {
            serde_json::from_slice(&body).map_err(|e| e.to_string())
        }
        Ok(Err(e)) => Err(e),
        Ok(_) => Err("expected HELLO".into()),
        Err(_) => Err("no HELLO in time".into()),
    }
}

/// What the accepting side does with a peer's frames.
pub trait Serve: Send + Sync + 'static {
    /// A peer connected (true when the same node was connected before:
    /// pushes from it may have been lost).
    fn connected(&self, hello: &Hello);
    fn forward(&self, req: codec::ForwardReq) -> crate::BoxFuture<ForwardResp>;
    fn publish(&self, body: &[u8]);
    fn check(&self);
    /// Test fault: frames from this node are ignored.
    fn cut_from(&self, node: &str) -> bool;
}

/// Accepts peers' connections.
pub async fn serve(
    listener: TcpListener,
    me: Hello,
    tls: Arc<rustls::ServerConfig>,
    h: Arc<dyn Serve>,
) {
    let acceptor = tokio_rustls::TlsAcceptor::from(tls);
    loop {
        let Ok((tcp, from)) = listener.accept().await else {
            tokio::time::sleep(Duration::from_millis(50)).await;
            continue;
        };
        let _ = tcp.set_nodelay(true);
        let (acceptor, me, h) = (acceptor.clone(), me.clone(), h.clone());
        tokio::spawn(async move {
            if let Err(e) = accepted(tcp, acceptor, me, h).await {
                tracing::debug!(peer = %from, error = e, "peer connection ended");
            }
        });
    }
}

async fn accepted(
    tcp: TcpStream,
    acceptor: tokio_rustls::TlsAcceptor,
    me: Hello,
    h: Arc<dyn Serve>,
) -> Result<(), String> {
    let mut s = tokio::time::timeout(HANDSHAKE, acceptor.accept(tcp))
        .await
        .map_err(|_| "TLS handshake timed out")?
        .map_err(|e| e.to_string())?;
    let theirs = read_hello(&mut s).await?;
    if theirs.cluster_id != me.cluster_id {
        return Err(format!("a node of cluster {} connected", theirs.cluster_id));
    }
    codec::negotiate(
        (me.proto_min, me.proto_max),
        (theirs.proto_min, theirs.proto_max),
    )
    .ok_or("no common protocol version")?;
    s.write_all(&codec::encode_json(Kind::Hello, &me)?)
        .await
        .map_err(|e| e.to_string())?;
    h.connected(&theirs);
    let (mut rd, mut wr) = split(s);
    let (tx, mut rx) = mpsc::channel::<Vec<u8>>(QUEUE);
    let writer = async {
        while let Some(f) = rx.recv().await {
            wr.write_all(&f).await.map_err(|e| e.to_string())?;
        }
        Ok::<(), String>(())
    };
    let reader = async {
        loop {
            let f = tokio::time::timeout(IDLE, codec::read(&mut rd))
                .await
                .map_err(|_| "peer silent".to_string())??;
            let Some((kind, body)) = f else {
                return Ok::<(), String>(());
            };
            if h.cut_from(&theirs.node_id) {
                continue;
            }
            match kind {
                Some(Kind::ForwardReq) => {
                    let req: codec::ForwardReq =
                        serde_json::from_slice(&body).map_err(|e| e.to_string())?;
                    let (h, tx) = (h.clone(), tx.clone());
                    tokio::spawn(async move {
                        let r = h.forward(req).await;
                        if let Ok(f) = codec::encode_json(Kind::ForwardResp, &r) {
                            let _ = tx.try_send(f);
                        }
                    });
                }
                Some(Kind::Publish) => h.publish(&body),
                Some(Kind::Ping) => {
                    let _ = tx.try_send(codec::encode(Kind::Pong, b"")?);
                }
                Some(Kind::Check) => h.check(),
                Some(_) => {}
                None => metrics::counter!("agentim_cluster_unknown_frames_total").increment(1),
            }
        }
    };
    tokio::select! {
        r = writer => r,
        r = reader => r,
    }
}
