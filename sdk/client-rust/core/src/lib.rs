//! The AgentIM client core (docs/12 §2.2): every rule of the client SDK —
//! connection states and reconnection, network and failure handling, sync
//! and gaps, the offline queue, history and the local cache — with no IO.
//!
//! The platform runtime feeds [`Input`]s (what the app asked, what the
//! network and the store answered, the clock) into [`Core::handle`] and
//! carries out the [`Effect`]s it returns, in order. Time comes in with
//! every call; randomness from a seed. The same core runs natively (C ABI),
//! in the browser (wasm) and in the deterministic tests.
//!
//! Effects must be carried out in the order given. A `Persist` is
//! confirmed with `Input::Persisted(id)` (or `StorageFailed`): only then
//! is what it holds acknowledged to the server (nothing is acknowledged
//! before it is stored, docs/08 §4.2).

mod conn;
mod error;
mod history;
mod queue;
mod rng;
mod sync;
mod view;

use std::collections::{BTreeMap, HashMap, VecDeque};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub use error::{ClientError, is_transient};
pub use queue::Op;

/// How long a connection attempt may take, handshake and welcome included.
pub const CONNECT_TIMEOUT_MS: i64 = 10_000;
/// Disconnected this long: retry less often (`suspended`).
pub const SUSPEND_AFTER_MS: i64 = 120_000;
pub const SUSPENDED_RETRY_MS: i64 = 60_000;
pub const BACKOFF_BASE_MS: i64 = 500;
pub const BACKOFF_BASE_BUSY_MS: i64 = 1_000;
pub const BACKOFF_CAP_MS: i64 = 30_000;
/// Default heartbeat when the server does not say.
pub const HEARTBEAT_MS: i64 = 25_000;
/// How often positions are compared with the server.
pub const HEADS_EVERY_MS: i64 = 30_000;
/// Operations of the offline queue in flight at once, over all conversations.
pub const MAX_IN_FLIGHT: usize = 8;
/// Retry budget (gRPC style): tokens, cost of a retry, refund of a success.
pub const BUDGET_MAX: f64 = 10.0;
/// Messages per page (cache and server).
pub const PAGE: i64 = 50;
/// The server keeps `client_msg_id` this long; a retry after it gets a new one.
pub const DEDUP_MS: i64 = 7 * 24 * 3600 * 1000;

/// Settings of one client (one account on one device).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// `https://chat.example.com` (or `http://` for local development).
    pub url: String,
    /// The id the app chose for this device, kept across runs (docs/13 §4.2).
    pub device_id: String,
    #[serde(default)]
    pub device_name: Option<String>,
    #[serde(default)]
    pub platform: Option<String>,
    #[serde(default = "default_client_name")]
    pub client_name: String,
    #[serde(default = "default_client_version")]
    pub client_version: String,
    /// Seed of the jitter and local ids: random per process (the runtimes
    /// pass one); tests fix it to make runs repeatable.
    #[serde(default)]
    pub seed: u64,
}

fn default_client_name() -> String {
    "agentim-client".into()
}
fn default_client_version() -> String {
    env!("CARGO_PKG_VERSION").into()
}

/// What happens to the core.
#[derive(Debug, Clone)]
pub enum Input {
    /// The app asked something (`{"cmd": …}`); the answer is a `Reply`.
    Command {
        id: u64,
        cmd: Value,
    },
    /// The token provider answered (`Err(true)`: the backend refuses, stop).
    Token(Result<String, bool>),
    /// The WebSocket is open (the welcome frame comes next).
    Opened,
    Frame(String),
    Closed {
        code: u16,
        reason: String,
    },
    /// The connection could not be opened: the HTTP status of a refused
    /// handshake, if there was one.
    ConnectFailed {
        status: Option<u16>,
        retry_after_ms: Option<i64>,
        detail: String,
    },
    /// The platform's network view: up or not, and a number that changes
    /// whenever the route may have changed (Wi-Fi ↔ cellular, VPN, …).
    Network {
        online: bool,
        generation: u64,
    },
    Foreground(bool),
    /// Time passed (call at `next_deadline`, or any time).
    Tick,
    /// The cache, read at start: conversations, the queue, newest pages.
    Restored(Snapshot),
    /// An answer to `LoadPage`.
    PageLoaded {
        req: u64,
        conv_id: String,
        messages: Vec<Value>,
    },
    /// A `Persist` could not be written: `full`, `corrupt` or `unavailable`.
    StorageFailed {
        kind: String,
    },
    /// Writes work again (after `full`).
    StorageRecovered,
    /// A `Persist` was written (its batch id).
    Persisted(u64),
}

/// What the runtime must do.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    /// Ask the app's token provider; answer with `Input::Token`.
    NeedToken,
    /// Open a WebSocket to this URL (token included).
    Connect {
        url: String,
    },
    Send(String),
    Close {
        code: u16,
        reason: String,
    },
    /// Write to the cache, atomically.
    Persist(Batch),
    /// Read older messages of a conversation from the cache.
    LoadPage {
        req: u64,
        conv_id: String,
        before_seq: i64,
        limit: i64,
    },
    /// Delete the whole cache (damaged or another account).
    WipeStore,
    /// Tell the app (`{"type": …}`).
    Emit(Value),
    /// The answer to a command.
    Reply {
        id: u64,
        result: Result<Value, ClientError>,
    },
}

/// One atomic write to the cache.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Batch {
    /// Confirm with `Input::Persisted(id)` once written.
    #[serde(default)]
    pub id: u64,
    pub convs: Vec<ConvRow>,
    pub put_messages: Vec<Value>,
    /// `(conv_id, msg_id or local key)`.
    pub remove_messages: Vec<(String, String)>,
    pub put_ops: Vec<Value>,
    pub remove_ops: Vec<String>,
    pub meta: Option<Value>,
}

impl Batch {
    pub fn is_empty(&self) -> bool {
        self.convs.is_empty()
            && self.put_messages.is_empty()
            && self.remove_messages.is_empty()
            && self.put_ops.is_empty()
            && self.remove_ops.is_empty()
            && self.meta.is_none()
    }
}

/// A conversation's row in the cache.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ConvRow {
    pub conv_id: String,
    pub last_seq: i64,
    /// Ranges of seqs whose messages the cache holds completely.
    pub segments: Vec<(i64, i64)>,
    /// Why nothing older can be fetched (`start`, `visible_from`, …), once known.
    pub older_end: Option<String>,
    pub info: Value,
}

/// What `Input::Restored` brings back.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Snapshot {
    /// The account the cache belongs to (None: empty cache).
    pub account: Option<String>,
    pub convs: Vec<ConvRow>,
    pub ops: Vec<Value>,
    /// The newest page of each conversation.
    pub messages: Vec<Value>,
}

/// Connection states (docs/12 §2.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Closed,
    Connecting,
    Connected,
    Disconnected,
    Suspended,
    Failed,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            State::Closed => "closed",
            State::Connecting => "connecting",
            State::Connected => "connected",
            State::Disconnected => "disconnected",
            State::Suspended => "suspended",
            State::Failed => "failed",
        }
    }
}

/// Where the current connection is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Phase {
    /// No socket.
    Down,
    /// Waiting for the token provider.
    Token,
    /// Socket asked for, waiting for the welcome frame.
    Opening,
    /// Welcome received: hello, conversations, first sync.
    Handshake,
    /// Synced: live.
    Live,
}

/// Why a request was sent, to know what to do with its answer.
#[derive(Debug, Clone)]
pub(crate) enum Purpose {
    Hello,
    ListConvs,
    Sync,
    Ack,
    Ping,
    Heads(Vec<String>),
    Op(String),
    History {
        conv_id: String,
        anchor: i64,
        jump: bool,
        cmd: Option<u64>,
    },
    App(u64),
    Focus,
}

#[derive(Debug, Clone)]
pub(crate) struct Pending {
    pub purpose: Purpose,
    pub deadline: i64,
    pub sent_at: i64,
}

/// A conversation as the client knows it.
#[derive(Debug, Clone, Default)]
pub(crate) struct Conv {
    pub id: String,
    /// Highest seq applied (and stored).
    pub last_seq: i64,
    /// Whether `last_seq` continues a known log (a conversation new to the
    /// cache starts wherever the server starts it).
    pub based: bool,
    /// Pushes that came ahead of a gap.
    pub ahead: BTreeMap<i64, Value>,
    /// Messages known to the server, by seq.
    pub msgs: BTreeMap<i64, Value>,
    pub seq_of: HashMap<String, i64>,
    pub segments: Vec<(i64, i64)>,
    pub older_end: Option<String>,
    /// List information (title, kind, unread, last message, …).
    pub info: Value,
    pub loading_older: bool,
    pub dirty: bool,
}

/// The client core.
pub struct Core {
    pub(crate) cfg: Config,
    pub(crate) now: i64,
    pub(crate) rng: rng::Rng,
    pub(crate) out: Vec<Effect>,
    // connection
    pub(crate) state: State,
    pub(crate) reason: Option<String>,
    pub(crate) phase: Phase,
    pub(crate) want: bool,
    pub(crate) online: bool,
    pub(crate) generation: u64,
    pub(crate) foreground: bool,
    pub(crate) attempt: u32,
    pub(crate) retry_at: Option<i64>,
    pub(crate) down_since: Option<i64>,
    pub(crate) opening_deadline: Option<i64>,
    pub(crate) token: Option<String>,
    pub(crate) token_fresh: bool,
    pub(crate) goaway_wait: Option<i64>,
    pub(crate) heartbeat_ms: i64,
    pub(crate) last_rx: i64,
    pub(crate) last_tx: i64,
    pub(crate) probe_deadline: Option<i64>,
    pub(crate) srtt: Option<f64>,
    pub(crate) rttvar: f64,
    pub(crate) rid: u64,
    pub(crate) pending: HashMap<u64, Pending>,
    pub(crate) me: Value,
    pub(crate) budget: f64,
    pub(crate) paused_until: Option<i64>,
    pub(crate) failures: VecDeque<Value>,
    // sync
    pub(crate) syncing: bool,
    pub(crate) sync_again: bool,
    pub(crate) gap_check: bool,
    /// Heads the server reported ahead of us: after the next sync we must
    /// have them, or the connection is replaced.
    pub(crate) heads_expect: Vec<(String, i64)>,
    pub(crate) next_heads: i64,
    pub(crate) convs: BTreeMap<String, Conv>,
    pub(crate) focused: Option<String>,
    // queue
    pub(crate) ops: Vec<Op>,
    pub(crate) op_n: u64,
    pub(crate) sent_ids: HashMap<String, String>,
    // views
    pub(crate) watched: std::collections::HashSet<String>,
    pub(crate) changed: BTreeMap<String, std::collections::BTreeSet<String>>,
    pub(crate) list_changed: std::collections::BTreeSet<String>,
    // storage
    pub(crate) restored: bool,
    pub(crate) account: Option<String>,
    pub(crate) storage: &'static str,
    pub(crate) batch: Batch,
    pub(crate) next_batch: u64,
    /// The last batch written but not confirmed yet.
    pub(crate) unconfirmed: Option<u64>,
    /// Acknowledgements waiting for their batch.
    pub(crate) acks_waiting: Vec<(u64, Vec<Value>)>,
    pub(crate) next_req: u64,
    pub(crate) page_reqs: HashMap<u64, (String, Option<u64>)>,
}

impl Core {
    pub fn new(cfg: Config) -> Core {
        let seed = cfg.seed;
        Core {
            cfg,
            now: 0,
            rng: rng::Rng::new(seed),
            out: vec![],
            state: State::Closed,
            reason: None,
            phase: Phase::Down,
            want: false,
            online: true,
            generation: 0,
            foreground: true,
            attempt: 0,
            retry_at: None,
            down_since: None,
            opening_deadline: None,
            token: None,
            token_fresh: false,
            goaway_wait: None,
            heartbeat_ms: HEARTBEAT_MS,
            last_rx: 0,
            last_tx: 0,
            probe_deadline: None,
            srtt: None,
            rttvar: 0.0,
            rid: 0,
            pending: HashMap::new(),
            me: Value::Null,
            budget: BUDGET_MAX,
            paused_until: None,
            failures: VecDeque::new(),
            syncing: false,
            sync_again: false,
            gap_check: false,
            heads_expect: vec![],
            next_heads: 0,
            convs: BTreeMap::new(),
            focused: None,
            ops: vec![],
            op_n: 0,
            sent_ids: HashMap::new(),
            watched: Default::default(),
            changed: BTreeMap::new(),
            list_changed: Default::default(),
            restored: false,
            account: None,
            storage: "ok",
            batch: Batch::default(),
            next_batch: 0,
            unconfirmed: None,
            acks_waiting: vec![],
            next_req: 0,
            page_reqs: HashMap::new(),
        }
    }

    /// Feeds one input at time `now` (ms since 1970) and returns what to do.
    pub fn handle(&mut self, input: Input, now: i64) -> Vec<Effect> {
        self.now = now.max(self.now);
        match input {
            Input::Command { id, cmd } => self.command(id, cmd),
            Input::Token(t) => self.on_token(t),
            Input::Opened => {}
            Input::Frame(f) => self.on_frame(&f),
            Input::Closed { code, reason } => self.on_closed(code, &reason),
            Input::ConnectFailed {
                status,
                retry_after_ms,
                detail,
            } => self.on_connect_failed(status, retry_after_ms, &detail),
            Input::Network { online, generation } => self.on_network(online, generation),
            Input::Foreground(f) => self.on_foreground(f),
            Input::Tick => {}
            Input::Restored(s) => self.on_restored(s),
            Input::PageLoaded { req, messages, .. } => self.on_page_loaded(req, messages),
            Input::StorageFailed { kind } => self.on_storage_failed(&kind),
            Input::StorageRecovered => self.on_storage_recovered(),
            Input::Persisted(id) => self.on_persisted(id),
        }
        self.timers();
        self.pump_queue();
        self.flush();
        std::mem::take(&mut self.out)
    }

    /// When `handle(Input::Tick)` should be called next, if ever.
    pub fn next_deadline(&self) -> Option<i64> {
        let mut d: Vec<i64> = vec![];
        d.extend(self.retry_at);
        d.extend(self.opening_deadline);
        d.extend(self.probe_deadline);
        d.extend(self.pending.values().map(|p| p.deadline));
        if self.phase == Phase::Live {
            d.push(self.last_tx + self.heartbeat_ms);
            d.push(self.last_rx + 2 * self.heartbeat_ms + 5_000);
            d.push(self.next_heads);
        }
        d.extend(self.paused_until);
        if self.phase == Phase::Live {
            d.extend(self.ops.iter().filter_map(|o| o.not_before));
        }
        // Whatever was due is handled at the end of every input; what
        // remains due is waiting for something else (a connection).
        d.into_iter().filter(|t| *t > self.now).min()
    }

    pub fn state(&self) -> State {
        self.state
    }

    /// Everything that helps a developer report a problem (`debug_state`).
    pub fn debug_state(&self) -> Value {
        json!({
            "state": self.state.as_str(),
            "reason": self.reason,
            "phase": format!("{:?}", self.phase),
            "attempt": self.attempt,
            "retry_at": self.retry_at,
            "online": self.online,
            "foreground": self.foreground,
            "heartbeat_ms": self.heartbeat_ms,
            "rtt_ms": self.srtt,
            "budget": self.budget,
            "storage": self.storage,
            "me": self.me,
            "convs": self.convs.values().map(|c| json!({
                "conv_id": c.id, "last_seq": c.last_seq, "ahead": c.ahead.len(),
                "segments": c.segments, "older_end": c.older_end, "loaded": c.msgs.len(),
            })).collect::<Vec<_>>(),
            "queue": self.ops.iter().map(|o| o.to_json()).collect::<Vec<_>>(),
            "recent_failures": self.failures,
        })
    }

    pub(crate) fn emit(&mut self, v: Value) {
        self.out.push(Effect::Emit(v));
    }

    pub(crate) fn reply(&mut self, id: u64, result: Result<Value, ClientError>) {
        self.out.push(Effect::Reply { id, result });
    }

    pub(crate) fn note_failure(&mut self, what: &str, detail: Value) {
        if self.failures.len() >= 20 {
            self.failures.pop_front();
        }
        self.failures
            .push_back(json!({"at": self.now, "what": what, "detail": detail}));
    }

    /// One command from the app.
    fn command(&mut self, id: u64, cmd: Value) {
        let name = cmd["cmd"].as_str().unwrap_or("").to_string();
        let r = match name.as_str() {
            "connect" => {
                self.connect();
                Ok(json!({}))
            }
            "close" => {
                self.close();
                Ok(json!({}))
            }
            "logout" => {
                self.close();
                self.wipe();
                Ok(json!({}))
            }
            "send" => self.cmd_send(&cmd),
            "edit" => self.cmd_edit(&cmd),
            "recall" => self.cmd_recall(&cmd),
            "read" => self.cmd_read(&cmd),
            "feedback" => self.cmd_feedback(&cmd),
            "respond" => self.cmd_respond(&cmd),
            "retry" => self.cmd_retry(&cmd),
            "discard" => self.cmd_discard(&cmd),
            "pending" => {
                Ok(json!({"ops": self.ops.iter().map(|o| o.to_json()).collect::<Vec<_>>()}))
            }
            "messages" => self.cmd_messages(&cmd),
            "unwatch" => {
                if let Some(c) = cmd["conv_id"].as_str() {
                    self.watched.remove(c);
                }
                Ok(json!({}))
            }
            "conversations" => Ok(self.conversations()),
            "load_older" => return self.cmd_load_older(id, &cmd),
            "jump_to" => return self.cmd_jump_to(id, &cmd),
            "focus" => self.cmd_focus(&cmd),
            "call" => return self.cmd_call(id, &cmd),
            "debug_state" => Ok(self.debug_state()),
            "state" => Ok(
                json!({"state": self.state.as_str(), "reason": self.reason, "storage": self.storage}),
            ),
            _ => Err(ClientError::new(
                "bad_request",
                format!("unknown command {name:?}"),
            )),
        };
        self.reply(id, r);
    }
}
