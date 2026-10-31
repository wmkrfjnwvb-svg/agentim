//! Event webhooks and the before-send hook (docs/02-architecture.md §2.6).
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
    // Several joins at once (D128) are joins: `member.joined` and
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
    let ts = now_s();
    let r = http
        .post(url)
        .timeout(timeout)
        .header("content-type", "application/json")
        .header("user-agent", concat!("agentim/", env!("CARGO_PKG_VERSION")))
        .header("webhook-id", id)
        .header("webhook-timestamp", ts.to_string())
        .header("webhook-signature", sign(secret, id, ts, body))
        // The trace of the write it reports (docs/14 §4.1); no tracestate,
        // so a dye never leaves the server.
        .header(
            "traceparent",
            crate::trace::current().unwrap_or_default().traceparent(),
        )
        .body(body.to_string())
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if r.status().is_success() {
        Ok(r)
    } else {
        Err(format!("status {}", r.status().as_u16()))
    }
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
    let mut idle = false;
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
        }
        let rows = outbox_page(&shard, cursor).await;
        let rows: Vec<(i64, String, String, Option<crate::trace::Trace>)> = match rows {
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
        for (id, kind, body, trace) in rows {
            if matches(&wh.events, &kind) {
                // Delivered in the write's trace, as a child (a dye kept).
                let t = trace.map(|t| t.child()).unwrap_or_default();
                let wid = format!("evt_{}_{id}", shard.name);
                let mut attempt = 0;
                loop {
                    attempt += 1;
                    // A recall or dissolution meanwhile removes the row
                    // (D91): do not send what this page still holds.
                    if !still_queued(&shard, id).await {
                        break;
                    }
                    let sent = crate::trace::scope(t.clone(), async {
                        let sent = post(&http, &wh.url, &wh.secret, &wid, &body, timeout).await;
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
                    match sent {
                        Ok(_) => {
                            metrics::counter!(m::WEBHOOK_DELIVERIES, "webhook" => wh.name.clone(), "result" => "ok").increment(1);
                            break;
                        }
                        Err(e) => {
                            metrics::counter!(m::WEBHOOK_DELIVERIES, "webhook" => wh.name.clone(), "result" => "error").increment(1);
                            if attempt >= wh.max_attempts {
                                tracing::warn!(
                                    webhook = wh.name,
                                    id = wid,
                                    error = e,
                                    "webhook event dropped"
                                );
                                metrics::counter!(m::WEBHOOK_DROPPED, "webhook" => wh.name.clone())
                                    .increment(1);
                                dropped.push(wid.clone());
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

/// The next undelivered outbox rows, decrypted.
#[allow(clippy::type_complexity)]
async fn outbox_page(
    shard: &Shard,
    cursor: i64,
) -> ApiResult<Vec<(i64, String, String, Option<crate::trace::Trace>)>> {
    let mut c = shard
        .read
        .acquire()
        .await
        .map_err(|e| ApiError::new("storage_unavailable", e.to_string()))?;
    let rows =
        sqlx::query(shard.dialect.q(
            "SELECT id, type, body, trace FROM webhook_outbox WHERE id > ? ORDER BY id LIMIT 100",
        ))
        .bind(cursor)
        .fetch_all(&mut *c)
        .await?;
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
            Ok((id, r.try_get(1)?, body, trace))
        })
        .collect()
}

/// Moves the cursor through the writer (retrying until it sticks).
async fn ack(shard: &Shard, name: &str, upto: i64, dropped: &[String], all: &[String]) -> i64 {
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
