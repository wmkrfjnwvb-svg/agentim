//! Event webhooks and the before-send hook (docs/en/design/messaging.md).
//!
//! Webhooks: every main-log event a webhook subscribes to is written to
//! `webhook_outbox` in the same transaction as the event, so a committed
//! event is never lost and a refused one is never sent. One delivery task per
//! webhook and database reads the outbox in order, signs each request
//! (Standard Webhooks), retries with backoff, and moves its cursor through the
//! writer. Delivery is at least once; receivers deduplicate by `webhook-id`.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine;
use hmac::{KeyInit, Mac};
use serde_json::{Value, json};
use sqlx::Row;
use tokio::sync::watch;

use crate::config::{BeforeSendConfig, HookFailure, WebhookConfig};
use crate::logic::Op;
use crate::metrics as m;
use crate::model::{ApiError, ApiResult, User};
use crate::shard::Shard;

/// `*`, `msg.*` or an exact type such as `msg.created`.
pub fn valid_pattern(p: &str) -> bool {
    let body = p.strip_suffix(".*").unwrap_or(p);
    p == "*"
        || (!body.is_empty()
            && body.len() <= 64
            && body
                .split('.')
                .all(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_lowercase() || c == '_')))
}

pub fn matches(patterns: &[String], kind: &str) -> bool {
    // Several joins at once are joins: `member.joined` and
    // `member.*` subscribers get them too.
    if kind == "members.joined" && matches(patterns, "member.joined") {
        return true;
    }
    patterns.iter().any(|p| {
        p == "*"
            || p == kind
            || p.strip_suffix(".*").is_some_and(|pre| {
                kind.len() > pre.len()
                    && kind.starts_with(pre)
                    && kind.as_bytes()[pre.len()] == b'.'
            })
    })
}

/// The HMAC key of a secret. A Standard Webhooks secret (`whsec_` and
/// base64) is the decoded bytes, as the official libraries take it; any
/// other secret is its own bytes. None: `whsec_` without valid base64.
pub fn key_of(secret: &str) -> Option<Vec<u8>> {
    match secret.strip_prefix("whsec_") {
        Some(b64) => base64::engine::general_purpose::STANDARD.decode(b64).ok(),
        None => Some(secret.as_bytes().to_vec()),
    }
}

/// Standard Webhooks signature: `v1,` + base64(HMAC-SHA256(id.timestamp.body)).
pub fn sign(secret: &str, id: &str, ts: i64, body: &str) -> String {
    let key = key_of(secret).unwrap_or_else(|| secret.as_bytes().to_vec());
    let mut mac = hmac::Hmac::<sha2::Sha256>::new_from_slice(&key).expect("any key");
    mac.update(format!("{id}.{ts}.{body}").as_bytes());
    format!(
        "v1,{}",
        base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes())
    )
}

fn now_s() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub fn client() -> reqwest::Client {
    // ring instead of aws-lc: no C toolchain or NASM needed on Windows.
    let _ = rustls::crypto::ring::default_provider().install_default();
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("http client")
}

async fn post(
    http: &reqwest::Client,
    url: &str,
    secret: &str,
    id: &str,
    body: &str,
    timeout: Duration,
) -> Result<reqwest::Response, String> {
    let r = send(http, url, secret, id, body, timeout).await?;
    if r.status().is_success() {
        Ok(r)
    } else {
        Err(format!("status {}", r.status().as_u16()))
    }
}

/// One signed request; any answer is returned (the caller judges it).
async fn send(
    http: &reqwest::Client,
    url: &str,
    secret: &str,
    id: &str,
    body: &str,
    timeout: Duration,
) -> Result<reqwest::Response, String> {
    let ts = now_s();
    let r = http
        .post(url)
        .timeout(timeout)
        .header("content-type", "application/json")
        .header("user-agent", concat!("agentim/", env!("CARGO_PKG_VERSION")))
        .header("webhook-id", id)
        .header("webhook-timestamp", ts.to_string())
        .header("webhook-signature", sign(secret, id, ts, body))
        // The trace of the write it reports (docs/en/design/tracing.md); no tracestate,
        // so a dye never leaves the server.
        .header(
            "traceparent",
            crate::trace::current().unwrap_or_default().traceparent(),
        )
        .body(body.to_string())
        .send()
        .await
        .map_err(|e| e.to_string())?;
    Ok(r)
}

/// An address a tenant's webhook may not reach (docs/en/design/multi-tenancy.md): loopback,
/// private, link-local (cloud metadata), shared, unspecified, multicast,
/// broadcast, documentation and unique-local ranges.
pub fn blocked_ip(ip: std::net::IpAddr) -> bool {
    use std::net::IpAddr;
    match ip {
        IpAddr::V4(v) => {
            let o = v.octets();
            v.is_loopback()
                || v.is_private()
                || v.is_link_local()
                || v.is_unspecified()
                || v.is_multicast()
                || v.is_broadcast()
                || v.is_documentation()
                || o[0] == 0
                || (o[0] == 100 && (64..128).contains(&o[1]))
                || (o[0] == 192 && o[1] == 0 && o[2] == 0)
                || o[0] >= 240
        }
        IpAddr::V6(v) => {
            if let Some(m) = v.to_ipv4_mapped() {
                return blocked_ip(IpAddr::V4(m));
            }
            let s = v.segments();
            v.is_loopback()
                || v.is_unspecified()
                || v.is_multicast()
                || (s[0] & 0xfe00) == 0xfc00
                || (s[0] & 0xffc0) == 0xfe80
                || (s[0] == 0x2001 && s[1] == 0x0db8)
                || (s[0] == 0x64 && s[1] == 0xff9b)
        }
    }
}

/// Checks a tenant's webhook address without resolving it: `https` (or
/// `http` when private addresses are allowed), a host, and no literal
/// address it may not reach.
pub fn check_tenant_url(url: &str, private_ok: bool) -> Result<reqwest::Url, &'static str> {
    let u = reqwest::Url::parse(url).map_err(|_| "not a URL")?;
    match u.scheme() {
        "https" => {}
        "http" if private_ok => {}
        _ => return Err("the address must be https"),
    }
    if !u.username().is_empty() || u.password().is_some() {
        return Err("no user or password in the address");
    }
    let Some(host) = u.host_str() else {
        return Err("the address needs a host");
    };
    let literal = host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .parse::<std::net::IpAddr>()
        .ok();
    if literal.is_none()
        && !private_ok
        && (host.eq_ignore_ascii_case("localhost")
            || host.to_ascii_lowercase().ends_with(".localhost"))
    {
        return Err("the address may not be private or local");
    }
    if let Some(ip) = literal
        && !private_ok
        && blocked_ip(ip)
    {
        return Err("the address may not be private or local");
    }
    Ok(u)
}

/// Sends to a tenant's webhook: the host is resolved here, every address is
/// checked, and the request goes to those addresses only (a DNS answer that
/// changes in between cannot point it inside), with no redirects.
async fn send_guarded(
    url: &str,
    private_ok: bool,
    secret: &str,
    id: &str,
    body: &str,
    timeout: Duration,
) -> Result<reqwest::Response, String> {
    let (host, addrs) = guarded_addresses(url, private_ok, timeout).await?;
    let mut b = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none());
    if let Some(host) = host {
        b = b.resolve_to_addrs(&host, &addrs);
    }
    let http = b.build().map_err(|e| e.to_string())?;
    send(&http, url, secret, id, body, timeout).await
}

/// Where a tenant's webhook may be sent: its host name (none for a literal
/// address) and the addresses it resolves to, all of them allowed; an
/// error naming `webhook.address` when one is private or local.
pub async fn guarded_addresses(
    url: &str,
    private_ok: bool,
    timeout: Duration,
) -> Result<(Option<String>, Vec<std::net::SocketAddr>), String> {
    let u = check_tenant_url(url, private_ok).map_err(|e| e.to_string())?;
    let host = u.host_str().unwrap_or_default().to_string();
    let literal = host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .parse::<std::net::IpAddr>()
        .is_ok();
    if literal {
        return Ok((None, vec![]));
    }
    let port = u.port_or_known_default().unwrap_or(443);
    let addrs: Vec<std::net::SocketAddr> =
        tokio::time::timeout(timeout, tokio::net::lookup_host((host.as_str(), port)))
            .await
            .map_err(|_| "resolving the host timed out".to_string())?
            .map_err(|e| format!("resolving the host: {e}"))?
            .collect();
    if addrs.is_empty() {
        return Err("the host has no address".into());
    }
    allowed_addresses(&addrs, private_ok)?;
    Ok((Some(host), addrs))
}

/// Refuses resolved addresses a tenant's webhook may not reach (any one of
/// them: a host must not mix public and private answers).
pub fn allowed_addresses(addrs: &[std::net::SocketAddr], private_ok: bool) -> Result<(), String> {
    if !private_ok && addrs.iter().any(|a| blocked_ip(a.ip())) {
        metrics::counter!(m::WEBHOOK_DELIVERIES, "webhook" => "tenant", "result" => "address")
            .increment(1);
        return Err("webhook.address: the host resolves to a private or local address".into());
    }
    Ok(())
}

/// A tenant's webhook (docs/en/design/multi-tenancy.md), as stored (the secret opened).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct TenantHook {
    pub tenant: String,
    pub name: String,
    pub url: String,
    pub secret: String,
    pub events: Vec<String>,
    pub max_attempts: u32,
    pub timeout_ms: u64,
    pub backoff_ms: u64,
    pub enabled: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

impl TenantHook {
    /// What the admin API shows (never the secret).
    pub fn view(&self) -> Value {
        json!({"name": self.name, "url": self.url, "events": self.events,
            "max_attempts": self.max_attempts, "timeout_ms": self.timeout_ms,
            "backoff_ms": self.backoff_ms, "enabled": self.enabled,
            "created_at": self.created_at, "updated_at": self.updated_at})
    }

    /// Its outbox position's name (one per database).
    pub fn cursor_name(&self) -> String {
        format!("t:{}/{}", self.tenant, self.name)
    }

    pub fn config(&self) -> WebhookConfig {
        WebhookConfig {
            name: self.cursor_name(),
            url: self.url.clone(),
            secret: self.secret.clone(),
            events: self.events.clone(),
            max_attempts: self.max_attempts,
            timeout_ms: self.timeout_ms,
            backoff_ms: self.backoff_ms,
        }
    }
}

/// Delivery of a tenant's webhook: only its tenant's rows, guarded sends,
/// turned off after failing for `disable_after_ms` or when the receiver
/// answers `webhook-delivery: disable`.
#[derive(Clone)]
pub struct TenantScope {
    pub tenant: String,
    pub name: String,
    pub private_ok: bool,
    pub disable_after_ms: u64,
    pub control: Shard,
}

/// Meta key of a webhook's cursor (last handled outbox id).
pub fn cursor_key(name: &str) -> String {
    format!("webhook:{name}")
}

/// Runs one webhook against one database until aborted.
pub async fn deliver_loop(
    shard: Shard,
    wh: WebhookConfig,
    all: Vec<String>,
    mut wake: watch::Receiver<i64>,
    http: reqwest::Client,
    scope: Option<TenantScope>,
) {
    let key = cursor_key(&wh.name);
    let read_cursor = || async {
        loop {
            let row = sqlx::query(shard.dialect.q("SELECT v FROM meta WHERE k = ?"))
                .bind(key.clone())
                .fetch_optional(&shard.read)
                .await;
            match row {
                Ok(r) => {
                    break r
                        .and_then(|r| r.try_get::<String, _>(0).ok())
                        .and_then(|v| v.parse::<i64>().ok())
                        .unwrap_or(0);
                }
                Err(_) => tokio::time::sleep(Duration::from_secs(1)).await,
            }
        }
    };
    let mut cursor = read_cursor().await;
    let timeout = Duration::from_millis(wh.timeout_ms);
    let mut idle = true;
    // Since when every attempt failed (a tenant's webhook goes off after
    // `disable_after_ms` of it).
    let mut failing_since: Option<std::time::Instant> = None;
    loop {
        // In a cluster, one node delivers: the one doing the database's
        // upkeep; it continues from the stored position.
        if !shard.holds_upkeep() {
            idle = true;
            tokio::time::sleep(Duration::from_secs(1)).await;
            continue;
        }
        if idle {
            idle = false;
            cursor = read_cursor().await;
            // A tenant's new webhook starts at the end (no history).
            if scope.is_some() && cursor == 0 && !cursor_stored(&shard, &key).await {
                let end = outbox_end(&shard).await;
                cursor = ack(&shard, &wh.name, end, &[], &all).await;
            }
        }
        let rows = outbox_page(&shard, cursor, scope.as_ref().map(|s| s.tenant.as_str())).await;
        let rows: Vec<OutboxRow> = match rows {
            Ok(rows) => rows,
            Err(_) => {
                tokio::time::sleep(Duration::from_secs(1)).await;
                continue;
            }
        };
        if rows.is_empty() {
            // Woken by the writer after a commit, or poll now and then.
            let _ = tokio::time::timeout(Duration::from_secs(1), wake.changed()).await;
            continue;
        }
        let mut upto = cursor;
        let mut dropped = vec![];
        for (id, kind, body, trace, _tenant) in rows {
            if matches(&wh.events, &kind) {
                // Delivered in the write's trace, as a child (a dye kept).
                let t = trace.map(|t| t.child()).unwrap_or_default();
                let wid = format!("evt_{}_{id}", shard.name);
                let mut attempt = 0;
                loop {
                    attempt += 1;
                    // A recall or dissolution meanwhile removes the row
                    //: do not send what this page still holds.
                    if !still_queued(&shard, id).await {
                        break;
                    }
                    let sent = crate::trace::scope(t.clone(), async {
                        let sent = match &scope {
                            None => post(&http, &wh.url, &wh.secret, &wid, &body, timeout).await,
                            Some(sc) => {
                                match send_guarded(
                                    &wh.url,
                                    sc.private_ok,
                                    &wh.secret,
                                    &wid,
                                    &body,
                                    timeout,
                                )
                                .await
                                {
                                    Ok(r) => {
                                        // The receiver asks to stop (Svix's header).
                                        let stop = r
                                            .headers()
                                            .get("webhook-delivery")
                                            .and_then(|v| v.to_str().ok())
                                            .is_some_and(|v| v.eq_ignore_ascii_case("disable"));
                                        if stop {
                                            Err("disable".to_string())
                                        } else if r.status().is_success() {
                                            Ok(r)
                                        } else {
                                            Err(format!("status {}", r.status().as_u16()))
                                        }
                                    }
                                    Err(e) => Err(e),
                                }
                            }
                        };
                        if t.dye.is_some() {
                            tracing::info!(
                                dye = t.dye.as_deref(),
                                trace_id = %t.trace_id,
                                webhook = wh.name,
                                id = wid,
                                attempt,
                                ok = sent.is_ok(),
                                "dyed webhook delivery"
                            );
                        }
                        sent
                    })
                    .await;
                    let label = if scope.is_some() {
                        "tenant".to_string()
                    } else {
                        wh.name.clone()
                    };
                    match sent {
                        Ok(_) => {
                            metrics::counter!(m::WEBHOOK_DELIVERIES, "webhook" => label, "result" => "ok").increment(1);
                            failing_since = None;
                            break;
                        }
                        Err(e) => {
                            if let Some(sc) = &scope {
                                let since =
                                    *failing_since.get_or_insert_with(std::time::Instant::now);
                                let long =
                                    since.elapsed() >= Duration::from_millis(sc.disable_after_ms);
                                if e == "disable" || long {
                                    if upto > cursor {
                                        ack(&shard, &wh.name, upto, &[], &all).await;
                                    }
                                    disable_tenant_hook(
                                        sc,
                                        if e == "disable" { "asked" } else { "failing" },
                                    )
                                    .await;
                                    return;
                                }
                            }
                            metrics::counter!(m::WEBHOOK_DELIVERIES, "webhook" => label, "result" => "error").increment(1);
                            if attempt >= wh.max_attempts {
                                tracing::warn!(
                                    webhook = wh.name,
                                    id = wid,
                                    error = e,
                                    "webhook event dropped"
                                );
                                metrics::counter!(m::WEBHOOK_DROPPED, "webhook" => if scope.is_some() { "tenant".to_string() } else { wh.name.clone() })
                                    .increment(1);
                                let b: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
                                dropped.push((
                                    wid.clone(),
                                    b["data"]["conv_id"]
                                        .as_str()
                                        .unwrap_or_default()
                                        .to_string(),
                                    b["data"]["seq"].as_i64().unwrap_or(0),
                                ));
                                break;
                            }
                            // Record progress before waiting, so a restart
                            // does not resend what was already delivered.
                            if upto > cursor {
                                cursor = ack(&shard, &wh.name, upto, &[], &all).await;
                            }
                            let back = wh
                                .backoff_ms
                                .saturating_mul(1 << (attempt - 1).min(20))
                                .min(300_000);
                            tokio::time::sleep(Duration::from_millis(back)).await;
                        }
                    }
                }
            }
            upto = id;
        }
        cursor = ack(&shard, &wh.name, upto, &dropped, &all).await;
    }
}

/// Whether an outbox row is still there (true when the read fails: the
/// retry then decides).
async fn still_queued(shard: &Shard, id: i64) -> bool {
    let Ok(mut c) = shard.read.acquire().await else {
        return true;
    };
    sqlx::query(shard.dialect.q("SELECT 1 FROM webhook_outbox WHERE id = ?"))
        .bind(id)
        .fetch_optional(&mut *c)
        .await
        .map_or(true, |r| r.is_some())
}

/// Turns a tenant's webhook off through the control database (retrying
/// until it sticks); every node then stops delivering it.
async fn disable_tenant_hook(sc: &TenantScope, why: &str) {
    tracing::warn!(
        tenant = sc.tenant,
        webhook = sc.name,
        why,
        "tenant webhook turned off"
    );
    loop {
        let r = sc
            .control
            .call(Op::Admin(crate::logic::AdminOp::TenantWebhookDisable {
                tenant: sc.tenant.clone(),
                name: sc.name.clone(),
                why: why.to_string(),
            }))
            .await;
        if r.is_ok() {
            return;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

/// Whether a cursor was ever stored.
async fn cursor_stored(shard: &Shard, key: &str) -> bool {
    sqlx::query(shard.dialect.q("SELECT 1 FROM meta WHERE k = ?"))
        .bind(key.to_string())
        .fetch_optional(&shard.read)
        .await
        .map_or(true, |r| r.is_some())
}

/// The last outbox id (0 when empty).
pub(crate) async fn outbox_end(shard: &Shard) -> i64 {
    sqlx::query("SELECT MAX(id) FROM webhook_outbox")
        .fetch_one(&shard.read)
        .await
        .ok()
        .and_then(|r| r.try_get::<Option<i64>, _>(0).ok().flatten())
        .unwrap_or(0)
}

/// An outbox row: id, type, body, trace, tenant.
type OutboxRow = (
    i64,
    String,
    String,
    Option<crate::trace::Trace>,
    Option<String>,
);

/// The next undelivered outbox rows, decrypted (only `tenant`'s, if given).
async fn outbox_page(
    shard: &Shard,
    cursor: i64,
    tenant: Option<&str>,
) -> ApiResult<Vec<OutboxRow>> {
    let mut c = shard
        .read
        .acquire()
        .await
        .map_err(|e| ApiError::new("storage_unavailable", e.to_string()))?;
    let rows = match tenant {
        None => {
            sqlx::query(shard.dialect.q(
                "SELECT id, type, body, trace, tenant_id FROM webhook_outbox WHERE id > ? ORDER BY id LIMIT 100",
            ))
            .bind(cursor)
            .fetch_all(&mut *c)
            .await?
        }
        Some(t) => {
            sqlx::query(shard.dialect.q(
                "SELECT id, type, body, trace, tenant_id FROM webhook_outbox WHERE id > ? AND COALESCE(tenant_id, 'default') = ? ORDER BY id LIMIT 100",
            ))
            .bind(cursor)
            .bind(t.to_string())
            .fetch_all(&mut *c)
            .await?
        }
    };
    let mut db = crate::db::Db {
        c: &mut c,
        d: shard.dialect,
        cx: &shard.crypto,
        cap: false,
        confirm_now: false,
    };
    db.preload(&rows, &["body"]).await?;
    rows.iter()
        .map(|r| {
            let id: i64 = r.try_get(0)?;
            let body: String = r.try_get(2)?;
            let body = shard
                .crypto
                .open(&format!("webhook_outbox.body:{id}"), &body)
                .map_err(|e| ApiError::new("storage", e))?;
            let trace: Option<String> = r.try_get(3)?;
            let trace = trace.and_then(|t| serde_json::from_str(&t).ok());
            Ok((id, r.try_get(1)?, body, trace, r.try_get(4)?))
        })
        .collect()
}

/// Moves the cursor through the writer (retrying until it sticks).
async fn ack(
    shard: &Shard,
    name: &str,
    upto: i64,
    dropped: &[(String, String, i64)],
    all: &[String],
) -> i64 {
    loop {
        let r = shard
            .call(Op::WebhookAck {
                name: name.to_string(),
                upto,
                dropped: dropped.to_vec(),
                all: all.to_vec(),
            })
            .await;
        if r.is_ok() {
            return upto;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

/// Body of an outbox row (Standard Webhooks payload shape).
pub fn event_body(db: &str, ev: &crate::model::Event) -> Value {
    // Outside a conversation (friends): its data is the event.
    if ev.conv_id.is_empty() {
        return json!({"type": ev.kind, "timestamp": ev.created_at, "database": db, "data": ev.data});
    }
    json!({"type": ev.kind, "timestamp": ev.created_at, "database": db, "data": {"conv_id": ev.conv_id, "seq": ev.seq, "event": ev}})
}

/// What the before-send hook decided.
pub enum Verdict {
    Allow,
    Replace {
        text: String,
        payload: Option<Value>,
    },
}

/// Asks the before-send hook about a message. `Err` is a refusal.
pub async fn before_send(
    http: &reqwest::Client,
    cfg: &BeforeSendConfig,
    sender: &User,
    request: Value,
) -> ApiResult<Verdict> {
    let id = format!("bs_{}", uuid::Uuid::now_v7().simple());
    let mut body = request;
    body["type"] = json!("message.before_send");
    body["sender"] = json!({"id": sender.id, "name": sender.name, "kind": sender.kind});
    let body = body.to_string();
    let failed = |why: String| {
        metrics::counter!(m::BEFORE_SEND, "result" => "error").increment(1);
        tracing::warn!(error = why, "before-send hook failed");
        match cfg.on_error {
            HookFailure::Allow => Ok(Verdict::Allow),
            HookFailure::Reject => Err(ApiError::new(
                "policy_blocked",
                "the message could not be checked; try again later",
            )),
        }
    };
    let resp = match post(
        http,
        &cfg.url,
        &cfg.secret,
        &id,
        &body,
        Duration::from_millis(cfg.timeout_ms),
    )
    .await
    {
        Ok(r) => r,
        Err(e) => return failed(e),
    };
    let v: Value = match resp
        .text()
        .await
        .map_err(|e| e.to_string())
        .and_then(|t| serde_json::from_str(&t).map_err(|e| e.to_string()))
    {
        Ok(v) => v,
        Err(e) => return failed(format!("bad response: {e}")),
    };
    match v["action"].as_str() {
        Some("allow") => {
            metrics::counter!(m::BEFORE_SEND, "result" => "allow").increment(1);
            Ok(Verdict::Allow)
        }
        Some("reject") => {
            metrics::counter!(m::BEFORE_SEND, "result" => "reject").increment(1);
            let reason = v["reason"]
                .as_str()
                .filter(|r| !r.is_empty() && r.len() <= 256)
                .unwrap_or("rejected by the before-send hook");
            Err(ApiError::new("policy_blocked", reason.to_string()))
        }
        Some("replace") => match v["text"].as_str() {
            Some(t) => {
                metrics::counter!(m::BEFORE_SEND, "result" => "replace").increment(1);
                Ok(Verdict::Replace {
                    text: t.to_string(),
                    payload: v.get("payload").filter(|p| !p.is_null()).cloned(),
                })
            }
            None => failed("replace without text".into()),
        },
        _ => failed(format!("unknown action in {v}")),
    }
}

/// Wakes delivery tasks after outbox rows were committed.
pub type Wake = Arc<watch::Sender<i64>>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patterns() {
        for p in ["*", "msg.*", "msg.created", "member.joined", "conv.*"] {
            assert!(valid_pattern(p), "{p}");
        }
        for p in ["", "msg.", ".*", "msg.**", "Msg.created", "msg..x", "m sg"] {
            assert!(!valid_pattern(p), "{p}");
        }
        let ps = vec!["msg.*".to_string(), "member.joined".to_string()];
        assert!(matches(&ps, "msg.created"));
        assert!(matches(&ps, "member.joined"));
        assert!(!matches(&ps, "member.left"));
        // A batch of joins goes to whoever asked for joins.
        assert!(matches(&ps, "members.joined"));
        assert!(matches(&["member.*".to_string()], "members.joined"));
        assert!(matches(&["members.joined".to_string()], "members.joined"));
        assert!(!matches(
            &["member.left".to_string(), "msg.*".to_string()],
            "members.joined"
        ));
        assert!(!matches(&ps, "msg"), "a prefix needs the dot");
        assert!(!matches(&ps, "msgx.created"));
        assert!(matches(&["*".to_string()], "anything"));
    }

    #[test]
    fn a_whsec_secret_signs_as_the_official_library_does() {
        // standardwebhooks 1.1.1 (npm), new Webhook(secret).sign(id, date, body):
        // the key is the base64 after `whsec_` (B9).
        assert_eq!(
            sign(
                "whsec_MfKQ9r8GKYqrTwjUPD8ILPZIo2LaLaSw",
                "msg_p5jXN8AQM9LWM0D4loKWxJek",
                1_614_265_330,
                r#"{"test": 2432232314}"#
            ),
            "v1,g0hM9SsE+OTPJTGt/tmIKtSyZlE3uFJELVlNIOLJ1OE="
        );
        assert_eq!(key_of("whsec_AAEC"), Some(vec![0, 1, 2]));
        assert_eq!(key_of("whsec_not base64!"), None);
        assert_eq!(key_of("plain-secret").unwrap(), b"plain-secret");
    }

    #[test]
    fn signature_follows_standard_webhooks() {
        // A secret without the whsec_ prefix: its bytes are the key
        // (HMAC-SHA256, base64), computed independently.
        let s = sign(
            "whsec_test_secret_123",
            "msg_1",
            1_700_000_000,
            r#"{"a":1}"#,
        );
        assert_eq!(s, "v1,Do26VFEdkFjCezZMJIDmLozibyg5NZnL8MMgrTv383g=");
        assert_ne!(
            s,
            sign(
                "whsec_test_secret_124",
                "msg_1",
                1_700_000_000,
                r#"{"a":1}"#
            )
        );
        assert_ne!(
            s,
            sign(
                "whsec_test_secret_123",
                "msg_1",
                1_700_000_001,
                r#"{"a":1}"#
            )
        );
    }
}
