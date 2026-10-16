//! Open API `/v1` (docs/07-open-api.md): what a developer's backend calls
//! to operate the IM. Every call is authenticated with an API key, checked
//! against the key's scopes, rate limited, optionally idempotent, and runs
//! through the same hub operations as a client, as a user or as the system.

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, SocketAddr};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use axum::{
    Extension, Json, Router,
    body::{Body, Bytes},
    extract::{ConnectInfo, Path, Query, Request, State},
    http::{HeaderValue, Method, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post, put},
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::db::ApiKeyRow;
use crate::hub::Hub;
use crate::model::*;
use crate::server::AppState;

pub const SCOPES: &[&str] = &[
    "tokens:issue",
    "users:write",
    "users:read",
    "conversations:write",
    "conversations:read",
    "messages:write",
    "messages:read",
];

pub const OPENAPI: &str = include_str!("../openapi.json");

const MAX_BATCH_USERS: usize = 100;
const MAX_BATCH_SEND: usize = 500;
const MAX_BODY: usize = 1 << 20;

// ------------------------------------------------------------------ errors

/// RFC 9457 problem details with the same codes as the WebSocket protocol.
#[derive(Debug)]
pub struct Problem {
    pub status: StatusCode,
    pub code: String,
    pub detail: String,
}

impl Problem {
    pub fn new(status: StatusCode, code: &str, detail: impl Into<String>) -> Problem {
        Problem {
            status,
            code: code.into(),
            detail: detail.into(),
        }
    }
}

pub fn status_of(code: &str) -> StatusCode {
    match code {
        "bad_request" | "unknown_op" => StatusCode::BAD_REQUEST,
        "unauthorized" => StatusCode::UNAUTHORIZED,
        "forbidden" | "blocked" | "policy_blocked" | "missing_scope" | "https_required"
        | "ip_not_allowed" => StatusCode::FORBIDDEN,
        "not_found" => StatusCode::NOT_FOUND,
        "conflict" | "idempotency_in_progress" | "conv_migrating" => StatusCode::CONFLICT,
        "idempotency_mismatch" => StatusCode::UNPROCESSABLE_ENTITY,
        "rate_limited" => StatusCode::TOO_MANY_REQUESTS,
        "payload_too_large" => StatusCode::PAYLOAD_TOO_LARGE,
        "overloaded" | "storage_unavailable" | "storage" => StatusCode::SERVICE_UNAVAILABLE,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

impl From<ApiError> for Problem {
    fn from(e: ApiError) -> Problem {
        // Internal failures are not explained to callers.
        let detail = if status_of(e.code).is_server_error() {
            "the request could not be completed; retry with the same Idempotency-Key".to_string()
        } else {
            e.message
        };
        Problem::new(status_of(e.code), e.code, detail)
    }
}

impl IntoResponse for Problem {
    fn into_response(self) -> Response {
        let title = self.status.canonical_reason().unwrap_or("Error");
        let body = json!({
            "type": format!("https://agentim.dev/errors/{}", self.code),
            "title": title,
            "status": self.status.as_u16(),
            "detail": self.detail,
            "code": self.code,
        });
        let mut r = (self.status, body.to_string()).into_response();
        r.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/problem+json"),
        );
        r
    }
}

type R = Result<Response, Problem>;

fn bad(detail: impl Into<String>) -> Problem {
    Problem::new(StatusCode::BAD_REQUEST, "bad_request", detail)
}

fn ok(v: Value) -> R {
    Ok(Json(v).into_response())
}

fn created(v: Value) -> R {
    Ok((StatusCode::CREATED, Json(v)).into_response())
}

// ---------------------------------------------------------- network rules

/// `1.2.3.4`, `10.0.0.0/8`, `::1`, `fd00::/8`.
pub fn parse_net(s: &str) -> Option<(IpAddr, u8)> {
    let (ip, bits) = match s.split_once('/') {
        Some((ip, b)) => (ip.parse::<IpAddr>().ok()?, b.parse::<u8>().ok()?),
        None => {
            let ip: IpAddr = s.parse().ok()?;
            (ip, if ip.is_ipv4() { 32 } else { 128 })
        }
    };
    let max = if ip.is_ipv4() { 32 } else { 128 };
    (bits <= max).then_some((ip, bits))
}

fn net_contains((net, bits): (IpAddr, u8), ip: IpAddr) -> bool {
    match (net, ip) {
        (IpAddr::V4(n), IpAddr::V4(i)) => {
            let m = if bits == 0 {
                0
            } else {
                u32::MAX << (32 - bits)
            };
            u32::from(n) & m == u32::from(i) & m
        }
        (IpAddr::V6(n), IpAddr::V6(i)) => {
            let m = if bits == 0 {
                0
            } else {
                u128::MAX << (128 - bits)
            };
            u128::from(n) & m == u128::from(i) & m
        }
        _ => false,
    }
}

/// The caller's address: the TCP peer, or behind proxies the
/// `X-Forwarded-For` entry the outermost trusted proxy added (M9: the
/// first entry is whatever the client sent).
fn client_ip(hub: &Hub, req: &Request) -> Option<IpAddr> {
    let cfg = hub.open_api_config();
    if cfg.behind_proxy {
        let hops: Vec<&str> = req
            .headers()
            .get_all("x-forwarded-for")
            .iter()
            .filter_map(|v| v.to_str().ok())
            .flat_map(|v| v.split(','))
            .map(str::trim)
            .collect();
        return hops
            .len()
            .checked_sub(cfg.proxy_hops.max(1))
            .and_then(|i| hops[i].parse().ok());
    }
    peer_ip(req)
}

/// The TCP peer, from either kind of connect info.
fn peer_ip(req: &Request) -> Option<IpAddr> {
    req.extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0.ip())
        .or_else(|| {
            req.extensions()
                .get::<ConnectInfo<crate::tls::PeerAddr>>()
                .map(|c| c.0.0.ip())
        })
}

// -------------------------------------------------------- rate limiting

struct Bucket {
    tokens: f64,
    at: Instant,
}

fn buckets() -> &'static Mutex<HashMap<String, Bucket>> {
    static B: OnceLock<Mutex<HashMap<String, Bucket>>> = OnceLock::new();
    B.get_or_init(Default::default)
}

/// Takes one request from the key's bucket: `Ok(remaining)` or
/// `Err(seconds until the next request is allowed)`.
fn take(key_id: &str, rate: f64, burst: f64) -> Result<u64, u64> {
    let mut g = buckets().lock().unwrap();
    let now = Instant::now();
    let b = g.entry(key_id.to_string()).or_insert(Bucket {
        tokens: burst,
        at: now,
    });
    b.tokens = (b.tokens + now.duration_since(b.at).as_secs_f64() * rate).min(burst);
    b.at = now;
    if b.tokens < 1.0 {
        return Err(((1.0 - b.tokens) / rate).ceil().max(1.0) as u64);
    }
    b.tokens -= 1.0;
    Ok(b.tokens.floor() as u64)
}

fn in_flight() -> &'static Mutex<HashSet<(String, String)>> {
    static S: OnceLock<Mutex<HashSet<(String, String)>>> = OnceLock::new();
    S.get_or_init(Default::default)
}

// ------------------------------------------------------------ middleware

/// Authentication, transport and address rules, rate limit, idempotency.
async fn guard(State(st): State<AppState>, req: Request, next: Next) -> Response {
    match guard_inner(st, req, next).await {
        Ok(r) | Err(r) => r,
    }
}

// Err is an early answer, not a failure; boxing it buys nothing here.
#[allow(clippy::result_large_err)]
async fn guard_inner(st: AppState, req: Request, next: Next) -> Result<Response, Response> {
    let hub = &st.hub;
    let cfg = hub.open_api_config().clone();
    let peer = peer_ip(&req);
    // There is no built-in TLS yet: plaintext only from this machine, or
    // behind a TLS-terminating proxy.
    let tls = hub.config().server.tls.is_some();
    if !tls && !cfg.behind_proxy && !peer.is_some_and(|p| p.is_loopback()) {
        return Err(Problem::new(
            StatusCode::FORBIDDEN,
            "https_required",
            "use HTTPS through a TLS proxy (open_api.behind_proxy) or call from this machine",
        )
        .into_response());
    }
    let secret = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|s| s.trim().to_string())
        .filter(|s| s.starts_with("ak_"));
    let key = match secret {
        Some(s) => hub.api_key(&s).await,
        None => None,
    };
    let key = key.ok_or_else(|| {
        let mut r = Problem::new(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "missing, unknown, revoked or expired API key",
        )
        .into_response();
        r.headers_mut()
            .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        r
    })?;
    if let Some(allow) = &key.ip_allow {
        let ip = client_ip(hub, &req);
        let ok = ip.is_some_and(|ip| {
            allow
                .iter()
                .filter_map(|n| parse_net(n))
                .any(|n| net_contains(n, ip))
        });
        if !ok {
            return Err(Problem::new(
                StatusCode::FORBIDDEN,
                "ip_not_allowed",
                "this key may not be used from this address",
            )
            .into_response());
        }
    }
    let policy = format!("\"default\";q={};w=1", cfg.burst as u64);
    let remaining = match take(&key.key_id, cfg.rate_per_second, cfg.burst) {
        Ok(r) => r,
        Err(wait) => {
            let mut r = Problem::new(
                StatusCode::TOO_MANY_REQUESTS,
                "rate_limited",
                "too many requests for this key",
            )
            .into_response();
            let h = r.headers_mut();
            h.insert(header::RETRY_AFTER, HeaderValue::from(wait));
            h.insert("ratelimit-policy", HeaderValue::from_str(&policy).unwrap());
            h.insert(
                "ratelimit",
                HeaderValue::from_str(&format!("\"default\";r=0;t={wait}")).unwrap(),
            );
            return Err(r);
        }
    };
    let idem = req
        .headers()
        .get("idempotency-key")
        .map(|v| v.to_str().map(String::from).unwrap_or_default());
    let mut req = req;
    req.extensions_mut().insert(key.clone());
    let mut resp = match idem {
        Some(i) if req.method() == Method::POST => idempotent(hub, &key, i, req, next).await?,
        _ => next.run(req).await,
    };
    let h = resp.headers_mut();
    h.insert("ratelimit-policy", HeaderValue::from_str(&policy).unwrap());
    h.insert(
        "ratelimit",
        HeaderValue::from_str(&format!("\"default\";r={remaining};t=1")).unwrap(),
    );
    Ok(resp)
}

/// Same key + same request = the first result; a different request with the
/// same key is refused (422); a duplicate while the first runs gets 409.
#[allow(clippy::result_large_err)]
async fn idempotent(
    hub: &Hub,
    key: &ApiKeyRow,
    idem: String,
    req: Request,
    next: Next,
) -> Result<Response, Response> {
    if idem.is_empty() || idem.len() > 255 || !idem.chars().all(|c| c.is_ascii_graphic()) {
        return Err(bad("Idempotency-Key must be 1..255 visible ASCII characters").into_response());
    }
    let (parts, body) = req.into_parts();
    let bytes = axum::body::to_bytes(body, MAX_BODY).await.map_err(|_| {
        Problem::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "payload_too_large",
            "at most 1 MiB",
        )
        .into_response()
    })?;
    let hash = crate::auth::hash(&format!(
        "{} {}\n{}",
        parts.method,
        parts.uri.path(),
        String::from_utf8_lossy(&bytes)
    ));
    let slot = (key.key_id.clone(), idem.clone());
    if !in_flight().lock().unwrap().insert(slot.clone()) {
        return Err(Problem::new(
            StatusCode::CONFLICT,
            "idempotency_in_progress",
            "a request with this Idempotency-Key is still running",
        )
        .into_response());
    }
    let release = Release(slot);
    if let Some((h, status, body)) = hub
        .idem_get(&key.key_id, &idem)
        .await
        .map_err(|e| Problem::from(e).into_response())?
    {
        drop(release);
        if h != hash {
            return Err(Problem::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "idempotency_mismatch",
                "this Idempotency-Key was used with a different request",
            )
            .into_response());
        }
        let mut r = (
            StatusCode::from_u16(status as u16).unwrap_or(StatusCode::OK),
            body,
        )
            .into_response();
        let ct = if status >= 400 {
            "application/problem+json"
        } else {
            "application/json"
        };
        r.headers_mut()
            .insert(header::CONTENT_TYPE, HeaderValue::from_static(ct));
        r.headers_mut()
            .insert("idempotent-replayed", HeaderValue::from_static("true"));
        return Ok(r);
    }
    let mut req = Request::from_parts(parts, Body::from(bytes));
    req.extensions_mut().insert(IdemKey(idem.clone()));
    let resp = next.run(req).await;
    let status = resp.status();
    // Server errors and rate limits are not final: a retry runs again.
    if status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS {
        return Ok(resp);
    }
    let (p, b) = resp.into_parts();
    let bytes: Bytes = axum::body::to_bytes(b, usize::MAX)
        .await
        .unwrap_or_default();
    let stored = hub
        .idem_put(
            &key.key_id,
            &idem,
            &hash,
            status.as_u16() as i64,
            String::from_utf8_lossy(&bytes).to_string(),
        )
        .await;
    drop(release);
    if let Err(e) = stored {
        tracing::warn!(error = e.message, "idempotency result not stored");
    }
    Ok(Response::from_parts(p, Body::from(bytes)))
}

struct Release((String, String));

impl Drop for Release {
    fn drop(&mut self) {
        in_flight().lock().unwrap().remove(&self.0);
    }
}

/// The request's Idempotency-Key, used as the default `client_msg_id`.
#[derive(Clone)]
struct IdemKey(String);

fn need(key: &ApiKeyRow, scope: &str) -> Result<(), Problem> {
    if key.scopes.iter().any(|s| s == scope) {
        Ok(())
    } else {
        Err(Problem::new(
            StatusCode::FORBIDDEN,
            "missing_scope",
            format!("this key lacks the {scope} scope"),
        ))
    }
}

// --------------------------------------------------------------- helpers

/// Developer-chosen user id (docs/07 §3.3).
pub fn valid_user_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && !id.starts_with('_')
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_-.@:".contains(c))
}

fn user(hub: &Hub, id: &str) -> Result<User, Problem> {
    hub.user_by_name(id)
        .filter(|u| u.kind != UserKind::System)
        .ok_or_else(|| Problem::new(StatusCode::NOT_FOUND, "not_found", format!("user {id}")))
}

/// Who a request acts as: `"system"` or a user id.
fn actor(hub: &Hub, who: Option<&str>) -> Result<User, Problem> {
    match who {
        None => Err(bad("\"as\" is required: a user id or \"system\"")),
        Some("system") => Ok(hub.system_user()),
        Some(id) => user(hub, id),
    }
}

fn body<T: for<'de> Deserialize<'de>>(b: &Bytes) -> Result<T, Problem> {
    serde_json::from_slice(b).map_err(|e| bad(format!("invalid JSON body: {e}")))
}

fn check_fields(f: &Option<Value>) -> Result<(), Problem> {
    if let Some(f) = f {
        crate::logic::check_json_object(f, "fields", crate::logic::MAX_FIELDS_BYTES)?;
    }
    Ok(())
}

fn check_profile(display_name: &Option<String>, avatar: &Option<String>) -> Result<(), Problem> {
    if display_name
        .as_ref()
        .is_some_and(|d| d.is_empty() || d.len() > 255 || d.contains('\0'))
    {
        return Err(bad("display_name must be 1..255 bytes"));
    }
    if avatar
        .as_ref()
        .is_some_and(|a| a.len() > 1024 || !(a.starts_with("https://") || a.starts_with("http://")))
    {
        return Err(bad("avatar must be an http(s) URL of at most 1024 bytes"));
    }
    Ok(())
}

/// Internal ids in results are replaced by developer ids.
fn external(hub: &Hub, internal: &str) -> String {
    hub.shared()
        .user(internal)
        .map(|u| u.name)
        .unwrap_or_else(|| internal.to_string())
}

// ---------------------------------------------------------------- routes

pub fn routes(state: AppState) -> Router<AppState> {
    Router::new()
        .route(
            "/v1/users/{id}/tokens",
            post(issue_token).delete(revoke_tokens),
        )
        .route(
            "/v1/users/{id}",
            put(put_user)
                .get(get_user)
                .patch(patch_user)
                .post(user_action),
        )
        .route("/v1/users:batch", post(batch_users))
        .route("/v1/users/{id}/conversations", get(user_conversations))
        .route("/v1/users/{id}/messages", post(message_user))
        .route("/v1/conversations", post(create_conversation))
        .route(
            "/v1/conversations/{id}",
            get(get_conversation)
                .patch(patch_conversation)
                .delete(dissolve_conversation),
        )
        .route("/v1/conversations/{id}/members", post(add_members))
        .route(
            "/v1/conversations/{id}/members/{user}",
            axum::routing::delete(remove_member).patch(patch_member),
        )
        .route(
            "/v1/conversations/{id}/messages",
            post(send_message).get(history),
        )
        .route("/v1/conversations/{id}/read", post(mark_read))
        .route("/v1/messages:batch-send", post(batch_send))
        .route(
            "/v1/messages/{id}",
            axum::routing::patch(edit_message).post(message_action),
        )
        .layer(middleware::from_fn_with_state(state, guard))
        .route(
            "/v1/openapi.json",
            get(|| async { ([(header::CONTENT_TYPE, "application/json")], OPENAPI) }),
        )
}

/// Every route as `METHOD path`, for the OpenAPI consistency test.
pub const ROUTES: &[&str] = &[
    "POST /v1/users/{id}/tokens",
    "DELETE /v1/users/{id}/tokens",
    "PUT /v1/users/{id}",
    "GET /v1/users/{id}",
    "PATCH /v1/users/{id}",
    "POST /v1/users/{id}:deactivate",
    "POST /v1/users/{id}:reactivate",
    "POST /v1/users:batch",
    "GET /v1/users/{id}/conversations",
    "POST /v1/users/{id}/messages",
    "POST /v1/conversations",
    "GET /v1/conversations/{id}",
    "PATCH /v1/conversations/{id}",
    "DELETE /v1/conversations/{id}",
    "POST /v1/conversations/{id}/members",
    "DELETE /v1/conversations/{id}/members/{user}",
    "PATCH /v1/conversations/{id}/members/{user}",
    "POST /v1/conversations/{id}/messages",
    "GET /v1/conversations/{id}/messages",
    "POST /v1/conversations/{id}/read",
    "POST /v1/messages:batch-send",
    "PATCH /v1/messages/{id}",
    "POST /v1/messages/{id}:recall",
    "GET /v1/openapi.json",
];

type K = Extension<ApiKeyRow>;

// ---- tokens

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct TokenReq {
    device_name: Option<String>,
    platform: Option<String>,
    ttl_seconds: Option<i64>,
}

async fn issue_token(
    State(st): State<AppState>,
    Extension(k): K,
    Path(id): Path<String>,
    b: Bytes,
) -> R {
    need(&k, "tokens:issue")?;
    let u = user(&st.hub, &id)?;
    let req: TokenReq = if b.is_empty() {
        TokenReq::default()
    } else {
        body(&b)?
    };
    if st.hub.is_deactivated(&u.id) {
        return Err(Problem::new(
            StatusCode::FORBIDDEN,
            "forbidden",
            "the user is deactivated",
        ));
    }
    if req
        .ttl_seconds
        .is_some_and(|t| t <= 0 || t > 10 * 365 * 86400)
    {
        return Err(bad("ttl_seconds must be 1..315360000"));
    }
    let (token, row) = st
        .hub
        .issue_token(
            &u.id,
            req.device_name.as_deref(),
            req.platform.as_deref(),
            req.ttl_seconds.map(|t| t * 1000),
            &format!("api:{}", k.key_id),
        )
        .await?;
    st.hub
        .audit_api(
            &k.key_id,
            SYSTEM_ID,
            "tokens.issue",
            Some(&u.name),
            json!({"device_id": row.device_id}),
        )
        .await;
    created(json!({"token": token, "device": row.view()}))
}

async fn revoke_tokens(State(st): State<AppState>, Extension(k): K, Path(id): Path<String>) -> R {
    need(&k, "tokens:issue")?;
    let u = user(&st.hub, &id)?;
    let n = st.hub.revoke_tokens(&u.id, None).await?;
    st.hub
        .audit_api(
            &k.key_id,
            SYSTEM_ID,
            "tokens.revoke",
            Some(&u.name),
            json!({"revoked": n}),
        )
        .await;
    ok(json!({"revoked": n}))
}

// ---- users

#[derive(Deserialize, Default, Clone)]
#[serde(deny_unknown_fields)]
struct UserReq {
    #[serde(default)]
    id: Option<String>,
    kind: Option<String>,
    owner: Option<String>,
    unit: Option<String>,
    display_name: Option<String>,
    avatar: Option<String>,
    fields: Option<Value>,
}

/// Creates or updates; `(profile, created)`.
async fn upsert_user(
    hub: &Hub,
    key: &ApiKeyRow,
    id: &str,
    req: UserReq,
) -> Result<(Value, bool), Problem> {
    if !valid_user_id(id) {
        return Err(bad(
            "user ids are 1..64 of A-Z a-z 0-9 _ - . @ : and do not start with _",
        ));
    }
    check_profile(&req.display_name, &req.avatar)?;
    check_fields(&req.fields)?;
    let kind = match req.kind.as_deref() {
        None | Some("human") => UserKind::Human,
        Some("agent") => UserKind::Agent,
        Some(k) => return Err(bad(format!("kind must be human or agent, not {k}"))),
    };
    let owner = match &req.owner {
        Some(o) => Some(user(hub, o)?),
        None => None,
    };
    if kind == UserKind::Agent && owner.as_ref().is_none_or(|o| o.kind != UserKind::Human) {
        return Err(bad("an agent needs an owner who is a person"));
    }
    if kind == UserKind::Human && owner.is_some() {
        return Err(bad("only agents have an owner"));
    }
    let created = match hub.user_by_name(id) {
        Some(u) => {
            // Identity and placement are fixed once created.
            if u.kind != kind && req.kind.is_some()
                || req.owner.is_some() && u.owner_id != owner.as_ref().map(|o| o.id.clone())
                || req.unit.as_ref().is_some_and(|x| *x != u.unit_id)
            {
                return Err(Problem::new(
                    StatusCode::CONFLICT,
                    "conflict",
                    "kind, owner and unit cannot change",
                ));
            }
            false
        }
        None => {
            hub.create_user(
                id,
                kind,
                owner.as_ref().map(|o| o.id.as_str()),
                req.unit.as_deref(),
            )
            .await?;
            true
        }
    };
    let u = user(hub, id)?;
    if req.display_name.is_some() || req.avatar.is_some() || req.fields.is_some() {
        hub.update_user(&u.id, req.display_name, req.avatar, req.fields)
            .await?;
    }
    hub.audit_api(
        &key.key_id,
        SYSTEM_ID,
        if created {
            "users.create"
        } else {
            "users.update"
        },
        Some(id),
        json!({}),
    )
    .await;
    let p = hub.user_profile(&u.id).await?.unwrap_or_default();
    Ok((p, created))
}

async fn put_user(
    State(st): State<AppState>,
    Extension(k): K,
    Path(id): Path<String>,
    b: Bytes,
) -> R {
    need(&k, "users:write")?;
    let req: UserReq = if b.is_empty() {
        UserReq::default()
    } else {
        body(&b)?
    };
    if req.id.as_ref().is_some_and(|i| *i != id) {
        return Err(bad("the body's id differs from the path"));
    }
    let (p, c) = upsert_user(&st.hub, &k, &id, req).await?;
    if c { created(p) } else { ok(p) }
}

async fn get_user(State(st): State<AppState>, Extension(k): K, Path(id): Path<String>) -> R {
    need(&k, "users:read")?;
    let u = user(&st.hub, &id)?;
    ok(st.hub.user_profile(&u.id).await?.unwrap_or_default())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PatchUser {
    display_name: Option<String>,
    avatar: Option<String>,
    fields: Option<Value>,
}

async fn patch_user(
    State(st): State<AppState>,
    Extension(k): K,
    Path(id): Path<String>,
    b: Bytes,
) -> R {
    need(&k, "users:write")?;
    let u = user(&st.hub, &id)?;
    let req: PatchUser = body(&b)?;
    check_profile(&req.display_name, &req.avatar)?;
    check_fields(&req.fields)?;
    if req.display_name.is_none() && req.avatar.is_none() && req.fields.is_none() {
        return Err(bad("nothing to change"));
    }
    st.hub
        .update_user(&u.id, req.display_name, req.avatar, req.fields)
        .await?;
    st.hub
        .audit_api(&k.key_id, SYSTEM_ID, "users.update", Some(&id), json!({}))
        .await;
    ok(st.hub.user_profile(&u.id).await?.unwrap_or_default())
}

/// `POST /v1/users/{id}:deactivate` and `:reactivate`.
async fn user_action(State(st): State<AppState>, Extension(k): K, Path(seg): Path<String>) -> R {
    need(&k, "users:write")?;
    let (id, on) = match seg.rsplit_once(':') {
        Some((id, "deactivate")) => (id, true),
        Some((id, "reactivate")) => (id, false),
        _ => {
            return Err(Problem::new(
                StatusCode::NOT_FOUND,
                "not_found",
                "unknown action",
            ));
        }
    };
    let u = user(&st.hub, id)?;
    st.hub.set_deactivated(&u.id, on).await?;
    st.hub
        .audit_api(
            &k.key_id,
            SYSTEM_ID,
            if on {
                "users.deactivate"
            } else {
                "users.reactivate"
            },
            Some(id),
            json!({}),
        )
        .await;
    ok(st.hub.user_profile(&u.id).await?.unwrap_or_default())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BatchUsers {
    users: Vec<Value>,
}

async fn batch_users(State(st): State<AppState>, Extension(k): K, b: Bytes) -> R {
    need(&k, "users:write")?;
    let req: BatchUsers = body(&b)?;
    if req.users.is_empty() || req.users.len() > MAX_BATCH_USERS {
        return Err(bad(format!("1..{MAX_BATCH_USERS} users per call")));
    }
    let mut results = vec![];
    for item in req.users {
        let id = item["id"].as_str().unwrap_or("").to_string();
        let r = match serde_json::from_value::<UserReq>(item) {
            Err(e) => Err(bad(e.to_string())),
            Ok(u) => upsert_user(&st.hub, &k, &id, u).await,
        };
        results.push(match r {
            Ok((p, c)) => json!({"id": id, "ok": true, "created": c, "user": p}),
            Err(p) => json!({"id": id, "ok": false, "code": p.code, "detail": p.detail}),
        });
    }
    ok(json!({"results": results}))
}

async fn user_conversations(
    State(st): State<AppState>,
    Extension(k): K,
    Path(id): Path<String>,
) -> R {
    need(&k, "conversations:read")?;
    let u = user(&st.hub, &id)?;
    let mut v = st.hub.act_as(&u, ClientMsg::ListConvs).await?;
    v.as_object_mut().map(|o| {
        o.remove("ok");
        o.remove("type");
        o.remove("rid")
    });
    ok(v)
}

// ---- conversations

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateConv {
    #[serde(rename = "as")]
    who: Option<String>,
    kind: String,
    title: String,
    #[serde(default)]
    members: Vec<String>,
    owner: Option<String>,
    fields: Option<Value>,
}

async fn create_conversation(State(st): State<AppState>, Extension(k): K, b: Bytes) -> R {
    need(&k, "conversations:write")?;
    let req: CreateConv = body(&b)?;
    check_fields(&req.fields)?;
    let who = actor(&st.hub, req.who.as_deref())?;
    // As the system, the conversation is created by its owner, whose rules
    // (blocks, one direct conversation per pair) apply.
    let creator = if who.kind == UserKind::System {
        user(
            &st.hub,
            req.owner
                .as_deref()
                .ok_or_else(|| bad("as system, give an owner"))?,
        )?
    } else {
        if req.owner.as_ref().is_some_and(|o| *o != who.name) {
            return Err(bad("as a user, the owner is that user"));
        }
        who.clone()
    };
    let mut members = vec![];
    for m in &req.members {
        members.push(user(&st.hub, m)?.id);
    }
    let v = st
        .hub
        .act_as(
            &creator,
            ClientMsg::CreateConv {
                kind: req.kind,
                title: req.title,
                members,
                assignee: None,
            },
        )
        .await?;
    let conv = v["conv_id"].as_str().unwrap_or_default().to_string();
    if let Some(f) = req.fields
        && v["existing"] != true
    {
        st.hub
            .act_as(
                &creator,
                ClientMsg::UpdateConv {
                    conv_id: conv.clone(),
                    title: None,
                    fields: Some(f),
                },
            )
            .await?;
    }
    st.hub
        .audit_api(
            &k.key_id,
            &who.id,
            "conversations.create",
            Some(&conv),
            json!({}),
        )
        .await;
    let detail = st.hub.conv_detail(&conv).await?;
    if v["existing"] == true {
        let mut d = detail;
        d["existing"] = json!(true);
        ok(d)
    } else {
        created(detail)
    }
}

async fn get_conversation(
    State(st): State<AppState>,
    Extension(k): K,
    Path(id): Path<String>,
) -> R {
    need(&k, "conversations:read")?;
    ok(st.hub.conv_detail(&id).await?)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PatchConv {
    #[serde(rename = "as")]
    who: Option<String>,
    title: Option<String>,
    fields: Option<Value>,
}

async fn patch_conversation(
    State(st): State<AppState>,
    Extension(k): K,
    Path(id): Path<String>,
    b: Bytes,
) -> R {
    need(&k, "conversations:write")?;
    let req: PatchConv = body(&b)?;
    let who = actor(&st.hub, req.who.as_deref())?;
    st.hub
        .act_as(
            &who,
            ClientMsg::UpdateConv {
                conv_id: id.clone(),
                title: req.title,
                fields: req.fields,
            },
        )
        .await?;
    st.hub
        .audit_api(
            &k.key_id,
            &who.id,
            "conversations.update",
            Some(&id),
            json!({}),
        )
        .await;
    ok(st.hub.conv_detail(&id).await?)
}

#[derive(Deserialize)]
struct AsQuery {
    #[serde(rename = "as")]
    who: Option<String>,
}

async fn dissolve_conversation(
    State(st): State<AppState>,
    Extension(k): K,
    Path(id): Path<String>,
    Query(q): Query<AsQuery>,
) -> R {
    need(&k, "conversations:write")?;
    let who = actor(&st.hub, q.who.as_deref())?;
    st.hub
        .act_as(
            &who,
            ClientMsg::Dissolve {
                conv_id: id.clone(),
            },
        )
        .await?;
    st.hub
        .audit_api(
            &k.key_id,
            &who.id,
            "conversations.dissolve",
            Some(&id),
            json!({}),
        )
        .await;
    ok(json!({"dissolved": true}))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AddMembers {
    #[serde(rename = "as")]
    who: Option<String>,
    users: Vec<String>,
    /// Earlier messages the new members may see (docs/08 §2.4).
    #[serde(default)]
    history: Option<i64>,
}

async fn add_members(
    State(st): State<AppState>,
    Extension(k): K,
    Path(id): Path<String>,
    b: Bytes,
) -> R {
    need(&k, "conversations:write")?;
    let req: AddMembers = body(&b)?;
    let who = actor(&st.hub, req.who.as_deref())?;
    if req.users.is_empty() || req.users.len() > MAX_BATCH_SEND {
        return Err(bad(format!("1..{MAX_BATCH_SEND} users per call")));
    }
    let mut results = vec![];
    for u in &req.users {
        let r = match user(&st.hub, u) {
            Err(p) => Err(p),
            Ok(x) => st
                .hub
                .act_as(
                    &who,
                    ClientMsg::AddMember {
                        conv_id: id.clone(),
                        user_id: x.id,
                        history: req.history,
                    },
                )
                .await
                .map_err(Problem::from),
        };
        results.push(match r {
            Ok(_) => json!({"user": u, "ok": true}),
            Err(p) => json!({"user": u, "ok": false, "code": p.code, "detail": p.detail}),
        });
    }
    st.hub
        .audit_api(
            &k.key_id,
            &who.id,
            "conversations.members.add",
            Some(&id),
            json!({"users": req.users}),
        )
        .await;
    ok(json!({"results": results}))
}

async fn remove_member(
    State(st): State<AppState>,
    Extension(k): K,
    Path((id, u)): Path<(String, String)>,
    Query(q): Query<AsQuery>,
) -> R {
    need(&k, "conversations:write")?;
    let who = actor(&st.hub, q.who.as_deref())?;
    let target = user(&st.hub, &u)?;
    let msg = if target.id == who.id {
        ClientMsg::Leave {
            conv_id: id.clone(),
        }
    } else {
        ClientMsg::RemoveMember {
            conv_id: id.clone(),
            user_id: target.id,
        }
    };
    st.hub.act_as(&who, msg).await?;
    st.hub
        .audit_api(
            &k.key_id,
            &who.id,
            "conversations.members.remove",
            Some(&id),
            json!({"user": u}),
        )
        .await;
    ok(json!({"removed": true}))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PatchMember {
    #[serde(rename = "as")]
    who: Option<String>,
    role: String,
}

async fn patch_member(
    State(st): State<AppState>,
    Extension(k): K,
    Path((id, u)): Path<(String, String)>,
    b: Bytes,
) -> R {
    need(&k, "conversations:write")?;
    let req: PatchMember = body(&b)?;
    let who = actor(&st.hub, req.who.as_deref())?;
    let target = user(&st.hub, &u)?;
    let msg = if req.role == "owner" {
        ClientMsg::TransferOwner {
            conv_id: id.clone(),
            user_id: target.id,
        }
    } else {
        ClientMsg::SetRole {
            conv_id: id.clone(),
            user_id: target.id,
            role: req.role.clone(),
        }
    };
    st.hub.act_as(&who, msg).await?;
    st.hub
        .audit_api(
            &k.key_id,
            &who.id,
            "conversations.members.role",
            Some(&id),
            json!({"user": u, "role": req.role}),
        )
        .await;
    ok(st.hub.conv_detail(&id).await?)
}

// ---- messages

#[derive(Deserialize, Clone)]
#[serde(deny_unknown_fields)]
struct SendReq {
    #[serde(rename = "as")]
    who: Option<String>,
    text: String,
    kind: Option<String>,
    payload: Option<Value>,
    client_msg_id: Option<String>,
    reply_to: Option<String>,
    to_user: Option<String>,
    /// Routing envelope and sources (docs/09 §10.1, docs/03 §9.6.1.1).
    to: Option<Vec<String>>,
    topics: Option<Vec<String>>,
    intent: Option<String>,
    refs: Option<Vec<String>>,
    sources: Option<Vec<Value>>,
}

/// The request's `client_msg_id`, else one derived from the Idempotency-Key
/// (scoped to the API key, like the key itself), else a fresh one.
fn client_msg_id(
    req: &SendReq,
    key: &ApiKeyRow,
    idem: Option<&IdemKey>,
) -> Result<String, Problem> {
    let id = req
        .client_msg_id
        .clone()
        .or_else(|| idem.map(|i| format!("idem:{}:{}", key.key_id, i.0)))
        .unwrap_or_else(|| uuid::Uuid::now_v7().to_string());
    if id.is_empty() || id.len() > 128 {
        return Err(bad("client_msg_id must be 1..128 bytes"));
    }
    Ok(id)
}

async fn send_as(
    hub: &Hub,
    who: &User,
    conv: &str,
    req: SendReq,
    cid: String,
) -> Result<Value, Problem> {
    let to_user = match &req.to_user {
        Some(t) => Some(user(hub, t)?.id),
        None => None,
    };
    let v = hub
        .act_as(
            who,
            ClientMsg::Send {
                conv_id: conv.to_string(),
                client_msg_id: cid,
                text: req.text,
                kind: req.kind,
                payload: req.payload,
                reply_to: req.reply_to,
                causation_id: None,
                on_behalf_of: None,
                to_user,
                to: req.to,
                topics: req.topics,
                intent: req.intent,
                refs: req.refs,
                sources: req.sources,
            },
        )
        .await?;
    let mut out = json!({"conv_id": conv});
    for f in [
        "seq",
        "msg_id",
        "duplicate",
        "draft_id",
        "state",
        "side_seq",
    ] {
        if !v[f].is_null() {
            out[f] = v[f].clone();
        }
    }
    Ok(out)
}

async fn send_message(
    State(st): State<AppState>,
    Extension(k): K,
    idem: Option<Extension<IdemKey>>,
    Path(id): Path<String>,
    b: Bytes,
) -> R {
    need(&k, "messages:write")?;
    let req: SendReq = body(&b)?;
    let who = actor(&st.hub, req.who.as_deref())?;
    let cid = client_msg_id(&req, &k, idem.as_ref().map(|e| &e.0))?;
    let v = send_as(&st.hub, &who, &id, req, cid).await?;
    st.hub
        .audit_api(
            &k.key_id,
            &who.id,
            "messages.send",
            Some(&id),
            json!({"msg_id": v["msg_id"]}),
        )
        .await;
    created(v)
}

/// Opens (or reuses) the direct conversation between `who` and `to`.
async fn dm(hub: &Hub, who: &User, to: &User) -> Result<String, Problem> {
    if who.kind == UserKind::System {
        return Err(bad("the system sends to conversations, not to people"));
    }
    if who.id == to.id {
        return Err(bad("cannot message yourself"));
    }
    let v = hub
        .act_as(
            who,
            ClientMsg::CreateConv {
                kind: "dm".into(),
                title: format!("{} · {}", who.name, to.name),
                members: vec![to.id.clone()],
                assignee: None,
            },
        )
        .await?;
    Ok(v["conv_id"].as_str().unwrap_or_default().to_string())
}

async fn message_user(
    State(st): State<AppState>,
    Extension(k): K,
    idem: Option<Extension<IdemKey>>,
    Path(id): Path<String>,
    b: Bytes,
) -> R {
    need(&k, "messages:write")?;
    let req: SendReq = body(&b)?;
    let who = actor(&st.hub, req.who.as_deref())?;
    let to = user(&st.hub, &id)?;
    let conv = dm(&st.hub, &who, &to).await?;
    let cid = client_msg_id(&req, &k, idem.as_ref().map(|e| &e.0))?;
    let v = send_as(&st.hub, &who, &conv, req, cid).await?;
    st.hub
        .audit_api(
            &k.key_id,
            &who.id,
            "messages.send",
            Some(&conv),
            json!({"msg_id": v["msg_id"]}),
        )
        .await;
    created(v)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BatchSend {
    #[serde(rename = "as")]
    who: Option<String>,
    to: Vec<String>,
    text: String,
    kind: Option<String>,
    payload: Option<Value>,
    client_msg_id: Option<String>,
}

async fn batch_send(
    State(st): State<AppState>,
    Extension(k): K,
    idem: Option<Extension<IdemKey>>,
    b: Bytes,
) -> R {
    need(&k, "messages:write")?;
    let req: BatchSend = body(&b)?;
    let who = actor(&st.hub, req.who.as_deref())?;
    if req.to.is_empty() || req.to.len() > MAX_BATCH_SEND {
        return Err(bad(format!("1..{MAX_BATCH_SEND} recipients per call")));
    }
    let base = SendReq {
        who: None,
        text: req.text,
        kind: req.kind,
        payload: req.payload,
        client_msg_id: req.client_msg_id,
        reply_to: None,
        to_user: None,
        to: None,
        topics: None,
        intent: None,
        refs: None,
        sources: None,
    };
    let root = client_msg_id(&base, &k, idem.as_ref().map(|e| &e.0))?;
    let mut results = vec![];
    let mut seen = HashSet::new();
    for t in &req.to {
        if !seen.insert(t.clone()) {
            results.push(json!({"to": t, "ok": false, "code": "bad_request", "detail": "repeated recipient"}));
            continue;
        }
        // Each recipient is its own message, deduplicated per recipient.
        let r = async {
            let to = user(&st.hub, t)?;
            let conv = dm(&st.hub, &who, &to).await?;
            let cid = format!("{root}:{t}");
            if cid.len() > 128 {
                return Err(bad(
                    "client_msg_id plus recipient id must be at most 128 bytes",
                ));
            }
            send_as(&st.hub, &who, &conv, base.clone(), cid).await
        }
        .await;
        results.push(match r {
            Ok(mut v) => {
                v["to"] = json!(t);
                v["ok"] = json!(true);
                v
            }
            Err(p) => json!({"to": t, "ok": false, "code": p.code, "detail": p.detail}),
        });
    }
    st.hub
        .audit_api(
            &k.key_id,
            &who.id,
            "messages.batch_send",
            None,
            json!({"recipients": req.to.len()}),
        )
        .await;
    ok(json!({"results": results}))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EditReq {
    #[serde(rename = "as")]
    who: Option<String>,
    text: String,
    payload: Option<Value>,
}

async fn edit_message(
    State(st): State<AppState>,
    Extension(k): K,
    Path(id): Path<String>,
    b: Bytes,
) -> R {
    need(&k, "messages:write")?;
    let req: EditReq = body(&b)?;
    let who = actor(&st.hub, req.who.as_deref())?;
    let v = st
        .hub
        .act_as(
            &who,
            ClientMsg::Edit {
                msg_id: id.clone(),
                text: req.text,
                payload: req.payload,
            },
        )
        .await?;
    st.hub
        .audit_api(&k.key_id, &who.id, "messages.edit", Some(&id), json!({}))
        .await;
    ok(json!({"seq": v["seq"]}))
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct AsBody {
    #[serde(rename = "as")]
    who: Option<String>,
}

/// `POST /v1/messages/{id}:recall`.
async fn message_action(
    State(st): State<AppState>,
    Extension(k): K,
    Path(seg): Path<String>,
    b: Bytes,
) -> R {
    need(&k, "messages:write")?;
    let Some(id) = seg.strip_suffix(":recall") else {
        return Err(Problem::new(
            StatusCode::NOT_FOUND,
            "not_found",
            "unknown action",
        ));
    };
    let req: AsBody = if b.is_empty() {
        AsBody::default()
    } else {
        body(&b)?
    };
    let who = actor(&st.hub, req.who.as_deref())?;
    let v = st
        .hub
        .act_as(
            &who,
            ClientMsg::Recall {
                msg_id: id.to_string(),
            },
        )
        .await?;
    st.hub
        .audit_api(&k.key_id, &who.id, "messages.recall", Some(id), json!({}))
        .await;
    ok(json!({"seq": v["seq"]}))
}

#[derive(Deserialize)]
struct HistoryQuery {
    #[serde(default)]
    after_seq: i64,
    limit: Option<i64>,
}

async fn history(
    State(st): State<AppState>,
    Extension(k): K,
    Path(id): Path<String>,
    Query(q): Query<HistoryQuery>,
) -> R {
    need(&k, "messages:read")?;
    if q.after_seq < 0 || q.limit.is_some_and(|l| !(1..=200).contains(&l)) {
        return Err(bad("after_seq >= 0 and limit 1..200"));
    }
    let sys = st.hub.system_user();
    let v = st
        .hub
        .act_as(
            &sys,
            ClientMsg::Pull {
                conv_id: id.clone(),
                after_seq: q.after_seq,
                limit: Some(q.limit.unwrap_or(50)),
                as_agent: None,
            },
        )
        .await?;
    // Reading content is audited (who read whose messages).
    st.hub
        .audit_api(
            &k.key_id,
            SYSTEM_ID,
            "messages.read",
            Some(&id),
            json!({"after_seq": q.after_seq}),
        )
        .await;
    let events: Vec<Value> = v["events"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|mut e| e["event"].take())
        .map(|mut ev| {
            if let Some(a) = ev["actor_id"].as_str() {
                ev["actor_id"] = json!(external(&st.hub, a));
            }
            if let Some(s) = ev["msg"]["sender_id"].as_str() {
                ev["msg"]["sender_id"] = json!(external(&st.hub, s));
            }
            ev
        })
        .collect();
    ok(json!({"events": events, "next_after_seq": v["upto_seq"], "has_more": v["has_more"]}))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadReq {
    #[serde(rename = "as")]
    who: Option<String>,
    seq: i64,
}

async fn mark_read(
    State(st): State<AppState>,
    Extension(k): K,
    Path(id): Path<String>,
    b: Bytes,
) -> R {
    need(&k, "messages:write")?;
    let req: ReadReq = body(&b)?;
    let who = actor(&st.hub, req.who.as_deref())?;
    if who.kind == UserKind::System {
        return Err(bad("read cursors belong to users"));
    }
    st.hub
        .act_as(
            &who,
            ClientMsg::Read {
                conv_id: id.clone(),
                seq: req.seq,
            },
        )
        .await?;
    ok(json!({}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn networks() {
        let n = parse_net("10.0.0.0/8").unwrap();
        assert!(net_contains(n, "10.1.2.3".parse().unwrap()));
        assert!(!net_contains(n, "11.0.0.1".parse().unwrap()));
        let one = parse_net("192.168.1.5").unwrap();
        assert!(net_contains(one, "192.168.1.5".parse().unwrap()));
        assert!(!net_contains(one, "192.168.1.6".parse().unwrap()));
        assert!(net_contains(
            parse_net("::1").unwrap(),
            "::1".parse().unwrap()
        ));
        assert!(net_contains(
            parse_net("0.0.0.0/0").unwrap(),
            "8.8.8.8".parse().unwrap()
        ));
        assert!(!net_contains(
            parse_net("0.0.0.0/0").unwrap(),
            "::1".parse().unwrap()
        ));
        for bad in ["", "10.0.0.0/33", "x", "::/129", "1.2.3"] {
            assert!(parse_net(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn user_ids() {
        for ok in ["user_12345", "a", "x@y.com", "tenant:42", &"a".repeat(64)] {
            assert!(valid_user_id(ok), "{ok}");
        }
        for bad in ["", "_system", "a b", "ü", &"a".repeat(65), "a/b"] {
            assert!(!valid_user_id(bad), "{bad}");
        }
    }

    #[test]
    fn status_codes() {
        assert_eq!(status_of("forbidden"), StatusCode::FORBIDDEN);
        assert_eq!(status_of("not_found"), StatusCode::NOT_FOUND);
        assert_eq!(status_of("conflict"), StatusCode::CONFLICT);
        assert_eq!(status_of("blocked"), StatusCode::FORBIDDEN);
        assert_eq!(status_of("rate_limited"), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(status_of("overloaded"), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(status_of("whatever"), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn buckets_refill() {
        let id = format!("t{}", uuid::Uuid::new_v4());
        for _ in 0..3 {
            assert!(take(&id, 1000.0, 3.0).is_ok());
        }
        let slow = format!("s{}", uuid::Uuid::new_v4());
        for _ in 0..3 {
            assert!(take(&slow, 1e-6, 3.0).is_ok());
        }
        assert!(take(&slow, 1e-6, 3.0).is_err(), "burst used up");
    }
}
