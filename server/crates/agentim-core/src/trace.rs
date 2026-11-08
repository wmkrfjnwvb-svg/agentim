//! Request tracing (docs/en/design/tracing.md): a W3C Trace Context for every
//! request, carried from the entry point through the writer, to other
//! nodes, into the message row and on to webhooks and agents.
//!
//! The context lives in a task-local while a request runs (`scope`), so
//! code deep in the writer reads it with `current()` instead of passing it
//! through every call. `SessCtx` carries it inside `Op` across the writer
//! queue and the cluster.

use std::fmt::Write as _;
use std::future::Future;

use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};

/// One request's place in a trace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Trace {
    /// 32 lowercase hex characters.
    pub trace_id: String,
    /// This request's span: 16 lowercase hex characters.
    pub span_id: String,
    /// The caller's span this one continues (trusted callers only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// Recorded in full (a trusted caller asked, or a dye rule matched).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub sampled: bool,
    /// The dye rule that matched (docs/en/design/tracing.md); never leaves the server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dye: Option<String>,
    /// An end user's client trace, linked but not continued.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client: Option<String>,
}

/// Who sent the incoming `traceparent`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Caller {
    /// The developer's backend, an agent, an admin: continued as is.
    Trusted,
    /// An end user's app: a new trace that links to theirs and ignores
    /// their sampled flag, so nobody can make the server record them in full.
    Client,
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

fn random_hex(n: usize) -> String {
    let mut b = vec![0u8; n];
    loop {
        SystemRandom::new().fill(&mut b).expect("random");
        // All zeros is invalid in both fields.
        if b.iter().any(|x| *x != 0) {
            return hex(&b);
        }
    }
}

fn is_hex(s: &str, len: usize) -> bool {
    s.len() == len
        && s.bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

/// A parsed `traceparent`: (trace id, parent span id, sampled).
pub fn parse(header: &str) -> Option<(String, String, bool)> {
    let h = header.trim();
    let parts: Vec<&str> = h.splitn(5, '-').collect();
    if parts.len() < 4 {
        return None;
    }
    let (version, trace_id, span_id, flags) = (parts[0], parts[1], parts[2], parts[3]);
    // Version 00 has exactly four fields; later versions may add more after
    // a dash, and are read as 00. ff is invalid.
    if !is_hex(version, 2) || version == "ff" || (version == "00" && parts.len() == 5) {
        return None;
    }
    if !is_hex(trace_id, 32) || !is_hex(span_id, 16) || !is_hex(flags, 2) {
        return None;
    }
    if trace_id.bytes().all(|c| c == b'0') || span_id.bytes().all(|c| c == b'0') {
        return None;
    }
    let flags = u8::from_str_radix(flags, 16).ok()?;
    Some((trace_id.to_string(), span_id.to_string(), flags & 1 == 1))
}

impl Trace {
    /// A new trace (no caller context, or an invalid one).
    pub fn new() -> Trace {
        Trace {
            trace_id: random_hex(16),
            span_id: random_hex(8),
            parent: None,
            sampled: false,
            dye: None,
            client: None,
        }
    }

    /// The trace for a request whose caller sent `header` (may be absent or
    /// invalid: then a new trace).
    pub fn incoming(header: Option<&str>, caller: Caller) -> Trace {
        let Some((trace_id, parent, sampled)) = header.and_then(parse) else {
            return Trace::new();
        };
        match caller {
            Caller::Trusted => Trace {
                trace_id,
                span_id: random_hex(8),
                parent: Some(parent),
                sampled,
                dye: None,
                client: None,
            },
            Caller::Client => Trace {
                client: Some(trace_id),
                ..Trace::new()
            },
        }
    }

    /// Work caused by this request (a fan-out, a webhook, an agent's turn):
    /// the same trace, a new span, the dye inherited.
    /// A span in a trace known only by its stored id (a message's
    /// `trace_id`), for work that continues it later.
    pub fn stored(trace_id: &str) -> Trace {
        Trace {
            trace_id: trace_id.to_string(),
            ..Trace::new()
        }
    }

    pub fn child(&self) -> Trace {
        Trace {
            trace_id: self.trace_id.clone(),
            span_id: random_hex(8),
            parent: Some(self.span_id.clone()),
            sampled: self.sampled,
            dye: self.dye.clone(),
            client: None,
        }
    }

    /// The `traceparent` header for calls this span makes.
    pub fn traceparent(&self) -> String {
        format!(
            "00-{}-{}-{:02x}",
            self.trace_id,
            self.span_id,
            u8::from(self.sampled)
        )
    }

    /// Marks the trace dyed by `rule` (recorded in full from here on).
    pub fn dyed(mut self, rule: &str) -> Trace {
        self.sampled = true;
        self.dye = Some(rule.to_string());
        self
    }
}

impl Default for Trace {
    fn default() -> Self {
        Trace::new()
    }
}

tokio::task_local! {
    // A cell: the dye is known only once the caller is (after the trace
    // started), and it is set on the trace already current.
    static CURRENT: std::cell::RefCell<Trace>;
}

/// The span a trace runs under where it enters the server or a writer.
#[derive(Clone, Copy)]
pub enum Entry {
    /// A request on a connection (WebSocket, MCP, in-process).
    Req,
    /// An HTTP request.
    Http,
    /// A writer running a request's operation, here or on another node.
    Op,
}

/// Runs `fut` in trace `t` under its entry span, made inside the trace so
/// an exported span takes the trace's id (docs/en/design/tracing.md).
pub async fn run<F: std::future::Future>(t: Trace, entry: Entry, fut: F) -> F::Output {
    use tracing::Instrument;
    // Log lines get the trace id from the writer (`LogWriter`); a span is
    // only needed for export.
    if !crate::otel::enabled() {
        return scope(t, fut).await;
    }
    let parent = t.clone();
    scope(t, async move {
        let span = match entry {
            Entry::Req => {
                tracing::info_span!("req", trace_id = %parent.trace_id, span_id = %parent.span_id)
            }
            Entry::Http => {
                tracing::info_span!("http", trace_id = %parent.trace_id, span_id = %parent.span_id)
            }
            Entry::Op => {
                tracing::info_span!("op", trace_id = %parent.trace_id, span_id = %parent.span_id)
            }
        };
        crate::otel::set_parent(&span, &parent);
        fut.instrument(span).await
    })
    .await
}

/// Runs `fut` with `t` as the current trace.
pub async fn scope<F: Future>(t: Trace, fut: F) -> F::Output {
    CURRENT.scope(std::cell::RefCell::new(t), fut).await
}

/// The trace of the request this task is running, if any.
pub fn current() -> Option<Trace> {
    CURRENT.try_with(|t| t.borrow().clone()).ok()
}

/// The current trace id (for logs and the message row).
pub fn current_id() -> Option<String> {
    CURRENT.try_with(|t| t.borrow().trace_id.clone()).ok()
}

/// The dye rule of the current request (docs/en/design/tracing.md).
pub fn current_dye() -> Option<String> {
    CURRENT.try_with(|t| t.borrow().dye.clone()).ok().flatten()
}

/// Whether the current request is dyed.
pub fn current_dyed() -> bool {
    CURRENT
        .try_with(|t| t.try_borrow().is_ok_and(|t| t.dye.is_some()))
        .unwrap_or(false)
}

/// Dyes the current request by `rule` (a first match wins); false when no
/// trace is current.
pub fn dye(rule: &str) -> bool {
    CURRENT
        .try_with(|t| {
            let mut t = t.borrow_mut();
            if t.dye.is_none() {
                t.dye = Some(rule.to_string());
                t.sampled = true;
            }
        })
        .is_ok()
}

/// How many servers in this process have a dye rule in force (one in
/// production; several in tests and in-process clusters). While none has,
/// the log filter keeps tracing's per-call-site cache (a disabled `debug!`
/// costs nothing); while one has, DEBUG call sites are asked per event.
static DYED_SERVERS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// One server's part of `DYED_SERVERS`; dropped with the server.
#[derive(Default)]
pub struct DyeFlag(std::sync::atomic::AtomicBool);

impl DyeFlag {
    /// Called when this server's dye rules change.
    pub fn set(&self, on: bool) {
        use std::sync::atomic::Ordering;
        if self.0.swap(on, Ordering::SeqCst) == on {
            return;
        }
        let before = if on {
            DYED_SERVERS.fetch_add(1, Ordering::SeqCst)
        } else {
            DYED_SERVERS.fetch_sub(1, Ordering::SeqCst)
        };
        // From none to one, or back: the call-site cache changes.
        if before == usize::from(!on) {
            tracing::callsite::rebuild_interest_cache();
        }
    }
}

impl Drop for DyeFlag {
    fn drop(&mut self) {
        self.set(false);
    }
}

fn dyes_active() -> bool {
    DYED_SERVERS.load(std::sync::atomic::Ordering::Relaxed) > 0
}

/// The log filter: `env` (RUST_LOG, default INFO) for everything, the
/// request spans always (whatever is logged in them carries the trace id),
/// and DEBUG and up for dyed requests on every node (docs/en/design/tracing.md).
pub fn log_filter(env: tracing_subscriber::EnvFilter) -> LogFilter {
    LogFilter { env }
}

pub struct LogFilter {
    env: tracing_subscriber::EnvFilter,
}

fn request_span(m: &tracing::Metadata<'_>) -> bool {
    m.is_span() && matches!(m.name(), "req" | "op" | "http")
}

impl<S> tracing_subscriber::layer::Filter<S> for LogFilter
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    fn callsite_enabled(
        &self,
        m: &'static tracing::Metadata<'static>,
    ) -> tracing::subscriber::Interest {
        use tracing::subscriber::Interest;
        let env = tracing_subscriber::layer::Filter::<S>::callsite_enabled(&self.env, m);
        if env.is_always() || request_span(m) {
            Interest::always()
        } else if dyes_active() && *m.level() <= tracing::Level::DEBUG {
            Interest::sometimes()
        } else {
            env
        }
    }

    fn enabled(
        &self,
        m: &tracing::Metadata<'_>,
        cx: &tracing_subscriber::layer::Context<'_, S>,
    ) -> bool {
        request_span(m)
            || tracing_subscriber::layer::Filter::<S>::enabled(&self.env, m, cx)
            || (dyes_active() && *m.level() <= tracing::Level::DEBUG && current_dyed())
    }

    fn max_level_hint(&self) -> Option<tracing::level_filters::LevelFilter> {
        use tracing::level_filters::LevelFilter;
        let env = tracing_subscriber::layer::Filter::<S>::max_level_hint(&self.env)
            .unwrap_or(LevelFilter::TRACE);
        let floor = if dyes_active() {
            LevelFilter::DEBUG
        } else {
            LevelFilter::INFO
        };
        Some(env.max(floor))
    }

    fn on_new_span(
        &self,
        attrs: &tracing::span::Attributes<'_>,
        id: &tracing::span::Id,
        cx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        tracing_subscriber::layer::Filter::<S>::on_new_span(&self.env, attrs, id, cx)
    }

    fn on_record(
        &self,
        id: &tracing::span::Id,
        values: &tracing::span::Record<'_>,
        cx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        tracing_subscriber::layer::Filter::<S>::on_record(&self.env, id, values, cx)
    }

    fn on_enter(&self, id: &tracing::span::Id, cx: tracing_subscriber::layer::Context<'_, S>) {
        tracing_subscriber::layer::Filter::<S>::on_enter(&self.env, id, cx)
    }

    fn on_exit(&self, id: &tracing::span::Id, cx: tracing_subscriber::layer::Context<'_, S>) {
        tracing_subscriber::layer::Filter::<S>::on_exit(&self.env, id, cx)
    }

    fn on_close(&self, id: tracing::span::Id, cx: tracing_subscriber::layer::Context<'_, S>) {
        tracing_subscriber::layer::Filter::<S>::on_close(&self.env, id, cx)
    }
}

// ------------------------------------------------------------ dye rules

/// What a dye rule marks (docs/en/design/tracing.md).
pub const DYE_KINDS: &[&str] = &["user", "device", "conv", "agent", "api_key"];
/// Longest a rule may last, and the default.
pub const DYE_MAX_MINUTES: i64 = 24 * 60;
pub const DYE_DEFAULT_MINUTES: i64 = 60;
/// Rules in force at once.
pub const DYE_MAX_RULES: usize = 100;

/// One dye rule: requests of its target are recorded in full until it
/// expires.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DyeRule {
    pub id: String,
    pub kind: String,
    pub target: String,
    pub reason: String,
    /// Who added it (`admin:<name>` or `admin:key:<name>`).
    pub by: String,
    pub created_at: i64,
    pub expires_at: i64,
}

/// Who a request comes from and what it is about, for matching rules.
#[derive(Debug, Default, Clone, Copy)]
pub struct Subject<'a> {
    /// The user acting (a person or, with `agent`, an agent): their id and
    /// their developer-facing name both match.
    pub user: Option<(&'a str, &'a str)>,
    pub agent: bool,
    pub device: Option<&'a str>,
    pub conv: Option<&'a str>,
    pub api_key: Option<&'a str>,
}

impl DyeRule {
    pub fn active(&self, now_ms: i64) -> bool {
        self.expires_at > now_ms
    }

    pub fn matches(&self, s: &Subject) -> bool {
        let t = self.target.as_str();
        match self.kind.as_str() {
            "user" => s
                .user
                .is_some_and(|(id, name)| !s.agent && (id == t || name == t)),
            "agent" => s
                .user
                .is_some_and(|(id, name)| s.agent && (id == t || name == t)),
            "device" => s.device == Some(t),
            "conv" => s.conv == Some(t),
            "api_key" => s.api_key == Some(t),
            _ => false,
        }
    }
}

/// The first rule in force that matches.
pub fn matching<'a>(rules: &'a [DyeRule], s: &Subject, now_ms: i64) -> Option<&'a DyeRule> {
    rules.iter().find(|r| r.active(now_ms) && r.matches(s))
}

// ------------------------------------------------------------ log lines

/// Adds the current request's `trace_id` and `span_id` to every log line
/// (docs/en/design/tracing.md): JSON lines get them as fields, text lines at the end.
/// Lines outside a request are unchanged.
pub struct LogWriter<M> {
    inner: M,
    json: bool,
}

impl<M> LogWriter<M> {
    pub fn new(inner: M, json: bool) -> Self {
        LogWriter { inner, json }
    }
}

impl<'a, M: tracing_subscriber::fmt::MakeWriter<'a>> tracing_subscriber::fmt::MakeWriter<'a>
    for LogWriter<M>
{
    type Writer = LineWriter<M::Writer>;
    fn make_writer(&'a self) -> Self::Writer {
        LineWriter {
            inner: self.inner.make_writer(),
            json: self.json,
            buf: Vec::new(),
        }
    }
}

/// One log line, completed when dropped.
pub struct LineWriter<W: std::io::Write> {
    inner: W,
    json: bool,
    buf: Vec<u8>,
}

impl<W: std::io::Write> std::io::Write for LineWriter<W> {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.buf.extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<W: std::io::Write> Drop for LineWriter<W> {
    fn drop(&mut self) {
        let mut line = std::mem::take(&mut self.buf);
        if let Some(t) = current() {
            let nl = line.ends_with(b"\n");
            if nl {
                line.pop();
            }
            if self.json && line.ends_with(b"}") {
                line.pop();
                line.extend_from_slice(
                    format!(
                        ",\"trace_id\":\"{}\",\"span_id\":\"{}\"}}",
                        t.trace_id, t.span_id
                    )
                    .as_bytes(),
                );
            } else if !self.json {
                line.extend_from_slice(
                    format!(" trace_id={} span_id={}", t.trace_id, t.span_id).as_bytes(),
                );
            }
            if nl {
                line.push(b'\n');
            }
        }
        let _ = self.inner.write_all(&line);
        let _ = self.inner.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID: &str = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01";

    #[test]
    fn parses_the_spec_example() {
        assert_eq!(
            parse(VALID),
            Some((
                "4bf92f3577b34da6a3ce929d0e0e4736".into(),
                "00f067aa0ba902b7".into(),
                true
            ))
        );
        assert!(!parse(&VALID.replace("-01", "-00")).unwrap().2);
    }

    #[test]
    fn invalid_headers_are_ignored() {
        for bad in [
            "",
            "00",
            "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7",
            "ff-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
            "00-00000000000000000000000000000000-00f067aa0ba902b7-01",
            "00-4bf92f3577b34da6a3ce929d0e0e4736-0000000000000000-01",
            "00-4BF92F3577B34DA6A3CE929D0E0E4736-00f067aa0ba902b7-01",
            "00-4bf92f3577b34da6a3ce929d0e0e473-00f067aa0ba902b7-01",
            "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01-extra",
            "0x-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
        ] {
            assert_eq!(parse(bad), None, "{bad:?}");
            // A request with it gets a new trace.
            let t = Trace::incoming(Some(bad), Caller::Trusted);
            assert!(t.parent.is_none() && t.client.is_none() && !t.sampled);
        }
        // A later version is read as 00, extra fields ignored.
        assert!(parse("01-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01-what").is_some());
    }

    #[test]
    fn trusted_callers_are_continued() {
        let t = Trace::incoming(Some(VALID), Caller::Trusted);
        assert_eq!(t.trace_id, "4bf92f3577b34da6a3ce929d0e0e4736");
        assert_eq!(t.parent.as_deref(), Some("00f067aa0ba902b7"));
        assert!(t.sampled);
        assert_ne!(t.span_id, "00f067aa0ba902b7");
        assert!(
            t.traceparent()
                .starts_with("00-4bf92f3577b34da6a3ce929d0e0e4736-")
        );
        assert!(t.traceparent().ends_with("-01"));
    }

    #[test]
    fn client_traces_are_linked_not_continued_and_their_sampling_ignored() {
        let t = Trace::incoming(Some(VALID), Caller::Client);
        assert_ne!(t.trace_id, "4bf92f3577b34da6a3ce929d0e0e4736");
        assert_eq!(
            t.client.as_deref(),
            Some("4bf92f3577b34da6a3ce929d0e0e4736")
        );
        assert!(t.parent.is_none());
        assert!(!t.sampled, "a client cannot make the server sample it");
    }

    #[test]
    fn children_keep_the_trace_and_the_dye() {
        let t = Trace::new().dyed("dye_1");
        let c = t.child();
        assert_eq!(c.trace_id, t.trace_id);
        assert_eq!(c.parent.as_deref(), Some(t.span_id.as_str()));
        assert_eq!(c.dye.as_deref(), Some("dye_1"));
        assert!(c.sampled);
        assert_ne!(c.span_id, t.span_id);
    }

    #[test]
    fn ids_are_well_formed_and_random() {
        let a = Trace::new();
        let b = Trace::new();
        assert!(is_hex(&a.trace_id, 32) && is_hex(&a.span_id, 16));
        assert_ne!(a.trace_id, b.trace_id);
        assert!(parse(&a.traceparent()).is_some());
    }

    fn rule(kind: &str, target: &str, expires_at: i64) -> DyeRule {
        DyeRule {
            id: format!("dye_{kind}"),
            kind: kind.into(),
            target: target.into(),
            reason: "r".into(),
            by: "admin:a".into(),
            created_at: 0,
            expires_at,
        }
    }

    #[test]
    fn rules_match_their_kind_only_while_in_force() {
        let person = Subject {
            user: Some(("u_1", "maya")),
            device: Some("phone"),
            conv: Some("c_1"),
            ..Default::default()
        };
        let agent = Subject {
            user: Some(("u_2", "helper")),
            agent: true,
            conv: Some("c_1"),
            ..Default::default()
        };
        let key = Subject {
            api_key: Some("k_1"),
            ..Default::default()
        };
        assert!(rule("user", "maya", 10).matches(&person));
        assert!(rule("user", "u_1", 10).matches(&person));
        assert!(
            !rule("user", "helper", 10).matches(&agent),
            "an agent is not a user rule"
        );
        assert!(rule("agent", "helper", 10).matches(&agent));
        assert!(!rule("agent", "maya", 10).matches(&person));
        assert!(rule("device", "phone", 10).matches(&person));
        assert!(rule("conv", "c_1", 10).matches(&agent));
        assert!(!rule("conv", "c_2", 10).matches(&agent));
        assert!(rule("api_key", "k_1", 10).matches(&key));
        assert!(!rule("api_key", "k_1", 10).matches(&person));
        assert!(!rule("nonsense", "maya", 10).matches(&person));
        let rules = [rule("user", "bob", 10), rule("user", "maya", 5)];
        assert_eq!(
            matching(&rules, &person, 4).map(|r| r.id.as_str()),
            Some("dye_user")
        );
        assert!(matching(&rules, &person, 5).is_none(), "expired");
    }

    #[tokio::test]
    async fn dyeing_marks_the_current_trace() {
        assert!(!dye("x"), "no trace current");
        let (dyed, first) = scope(Trace::new(), async {
            assert!(!current_dyed());
            assert!(dye("dye_a"));
            dye("dye_b");
            (current().unwrap(), current_dye())
        })
        .await;
        assert_eq!(first.as_deref(), Some("dye_a"), "the first match wins");
        assert!(dyed.sampled);
    }

    #[tokio::test]
    async fn the_current_trace_is_scoped_to_the_task() {
        assert!(current().is_none());
        let t = Trace::new();
        let id = t.trace_id.clone();
        let seen = scope(t, async { current_id() }).await;
        assert_eq!(seen, Some(id));
        assert!(current().is_none());
    }
}
