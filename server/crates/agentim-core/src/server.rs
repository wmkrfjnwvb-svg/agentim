//! HTTP + WebSocket front end.

use axum::{
    Json, Router,
    extract::{
        Query, State,
        ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade, close_code},
    },
    http::{HeaderMap, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::json;

use crate::hub::Hub;
use crate::model::{User, UserKind};

#[derive(Clone)]
pub struct AppState {
    pub hub: Hub,
    /// Enables password-less login by user name and user creation (demo/tests only).
    pub demo_login: bool,
    /// When set, `/metrics` requires `Authorization: Bearer <token>`.
    pub metrics_token: Option<String>,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .merge(crate::open_api::routes(state.clone()))
        .route("/", get(index))
        .route("/ws", get(ws))
        .route("/api/login", post(login))
        .route("/api/auth/login", post(password_login))
        .route("/api/auth/register", post(register))
        .route("/api/stats", get(stats))
        .route("/api/dev/users", post(dev_create_user))
        .route(
            "/mcp",
            post(crate::mcp::endpoint)
                .get(|| async { StatusCode::METHOD_NOT_ALLOWED })
                .delete(|| async { StatusCode::METHOD_NOT_ALLOWED }),
        )
        .route("/metrics", get(metrics))
        .route("/healthz", get(|| async { "ok" }))
        .route("/readyz", get(readyz))
        .with_state(state)
}

/// `scheme://host[:port]` with nothing after it.
pub fn valid_origin(o: &str) -> bool {
    let rest = o
        .strip_prefix("https://")
        .or_else(|| o.strip_prefix("http://"));
    rest.is_some_and(|r| {
        !r.is_empty()
            && r.chars()
                .all(|c| c.is_ascii_alphanumeric() || "-.:[]".contains(c))
    })
}

/// Browsers send `Origin`; a page on another site must not use a visitor's
/// session (cross-site WebSocket hijacking). Requests without `Origin`
/// (apps, SDKs, agents) are not affected.
pub(crate) fn origin_allowed(st: &AppState, headers: &HeaderMap) -> bool {
    let Some(origin) = headers.get("origin").and_then(|v| v.to_str().ok()) else {
        return true;
    };
    let host = headers
        .get("host")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let same = ["http://", "https://"]
        .iter()
        .any(|s| origin.eq_ignore_ascii_case(&format!("{s}{host}")));
    same || st
        .hub
        .config()
        .server
        .allowed_origins
        .iter()
        .any(|a| a.eq_ignore_ascii_case(origin))
}

fn origin_refused() -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(json!({"error": {"code": "forbidden", "message": "origin not allowed"}})),
    )
        .into_response()
}

#[derive(Deserialize)]
struct PasswordLogin {
    name: String,
    password: String,
    #[serde(default)]
    totp: Option<String>,
    #[serde(default)]
    device_name: Option<String>,
    #[serde(default)]
    platform: Option<String>,
}

fn auth_error(e: crate::model::ApiError) -> Response {
    let status = match e.code {
        "invalid_credentials" | "totp_required" => StatusCode::UNAUTHORIZED,
        "rate_limited" => StatusCode::TOO_MANY_REQUESTS,
        "forbidden" => StatusCode::FORBIDDEN,
        "conflict" => StatusCode::CONFLICT,
        "bad_request" => StatusCode::BAD_REQUEST,
        _ => StatusCode::SERVICE_UNAVAILABLE,
    };
    (
        status,
        Json(json!({"error": {"code": e.code, "message": e.message}})),
    )
        .into_response()
}

/// Built-in accounts: name + password (+ TOTP code) → a device token.
async fn password_login(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<PasswordLogin>,
) -> Response {
    if !origin_allowed(&st, &headers) {
        return origin_refused();
    }
    if req.password.len() > 1024 || req.name.len() > 255 {
        return auth_error(crate::model::ApiError::new("bad_request", "too long"));
    }
    match st
        .hub
        .login(
            &req.name,
            &req.password,
            req.totp.as_deref(),
            req.device_name.as_deref(),
            req.platform.as_deref(),
        )
        .await
    {
        Ok((token, device, user)) => {
            Json(json!({"token": token, "device": device.view(), "user": user})).into_response()
        }
        Err(e) => auth_error(e),
    }
}

#[derive(Deserialize)]
struct Register {
    name: String,
    password: String,
}

async fn register(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<Register>,
) -> Response {
    if !origin_allowed(&st, &headers) {
        return origin_refused();
    }
    if req.password.len() > 1024 {
        return auth_error(crate::model::ApiError::new("bad_request", "too long"));
    }
    match st.hub.register(&req.name, &req.password).await {
        Ok(u) => (StatusCode::CREATED, Json(json!({"user": u}))).into_response(),
        Err(e) => auth_error(e),
    }
}

async fn index() -> Html<&'static str> {
    Html(include_str!("../web/index.html"))
}

#[derive(Deserialize)]
struct LoginReq {
    name: String,
}

async fn login(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<LoginReq>,
) -> Response {
    if !origin_allowed(&st, &headers) {
        return origin_refused();
    }
    if !st.demo_login {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error": "demo login disabled"})),
        )
            .into_response();
    }
    match st.hub.token_of(&req.name).await {
        Some((user, token)) if user.kind == UserKind::Human => {
            Json(json!({"user": user, "token": token})).into_response()
        }
        _ => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "no such human user"})),
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
struct DevUserReq {
    name: String,
    #[serde(default)]
    agent: bool,
    #[serde(default)]
    owner: Option<String>,
    #[serde(default)]
    unit: Option<String>,
}

/// Demo/test only: creates a user and returns its token (used by the load test).
async fn dev_create_user(State(st): State<AppState>, Json(req): Json<DevUserReq>) -> Response {
    if !st.demo_login {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error": "demo mode only"})),
        )
            .into_response();
    }
    let kind = if req.agent {
        UserKind::Agent
    } else {
        UserKind::Human
    };
    match st
        .hub
        .create_user(&req.name, kind, req.owner.as_deref(), req.unit.as_deref())
        .await
    {
        Ok((user, token)) => Json(json!({"user": user, "token": token})).into_response(),
        Err(e) => (StatusCode::CONFLICT, Json(json!({"error": e.message}))).into_response(),
    }
}

fn bearer(headers: &HeaderMap) -> Option<String> {
    headers
        .get("authorization")?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .map(String::from)
}

async fn stats(State(st): State<AppState>, headers: HeaderMap) -> Response {
    if bearer(&headers)
        .and_then(|t| st.hub.authenticate(&t))
        .is_none()
    {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if !st.hub.stats_open() {
        return StatusCode::FORBIDDEN.into_response();
    }
    match st.hub.stats().await {
        Ok(v) => Json(v).into_response(),
        Err(e) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": e.message})),
        )
            .into_response(),
    }
}

async fn metrics(State(st): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(t) = &st.metrics_token
        && bearer(&headers).as_deref() != Some(t.as_str())
    {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    (
        [("content-type", "text/plain; version=0.0.4")],
        crate::metrics::render(),
    )
        .into_response()
}

async fn readyz(State(st): State<AppState>) -> Response {
    let h = st.hub.health();
    let code = if h["ready"] == true {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (code, Json(h)).into_response()
}

#[derive(Deserialize)]
struct WsQuery {
    token: String,
    /// Protocol versions the client speaks, comma separated (`v=2,1`).
    #[serde(default)]
    v: Option<String>,
}

/// `v=2,1` -> the version both sides speak; errors are answered before the
/// upgrade so an incompatible client never gets a half-working socket.
fn ws_protocol(v: Option<&str>) -> Result<u32, Box<Response>> {
    let Some(v) = v else {
        return Ok(crate::protocol::PROTOCOL_VERSIONS[0]);
    };
    let list: Option<Vec<u32>> = v
        .split(',')
        .map(|p| p.trim().parse().ok())
        .collect::<Option<Vec<u32>>>()
        .filter(|l| !l.is_empty() && l.len() <= 16);
    let refuse = |code: &str, message: String| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": {"code": code, "message": message}, "protocols": crate::protocol::PROTOCOL_VERSIONS})),
        )
            .into_response()
            .into()
    };
    let Some(list) = list else {
        return Err(refuse(
            "bad_request",
            "v must be a comma separated list of protocol versions".into(),
        ));
    };
    crate::protocol::negotiate(Some(&list)).map_err(|e| refuse(e.code, e.message))
}

async fn ws(
    State(st): State<AppState>,
    Query(q): Query<WsQuery>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    if !origin_allowed(&st, &headers) {
        return origin_refused();
    }
    let Some((user, device)) = st.hub.authenticate_or_load(&q.token).await else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let version = match ws_protocol(q.v.as_deref()) {
        Ok(v) => v,
        Err(r) => return *r,
    };
    // The library defaults (128 KiB read and 128 KiB write buffer per
    // connection) cost ~140 KB per idle connection; frames are small and every
    // push is flushed on its own, so small buffers lose nothing.
    upgrade
        .read_buffer_size(4 * 1024)
        .write_buffer_size(0)
        .max_message_size(1 << 20)
        .on_upgrade(move |socket| run_socket(st.hub, user, device, q.token, version, socket))
}

async fn run_socket(
    hub: Hub,
    user: User,
    device: String,
    token: String,
    version: u32,
    socket: WebSocket,
) {
    let (session, mut pushes, refused) = hub.open_device_session(user.clone(), device, &token);
    session.set_protocol(version);
    let idle = std::time::Duration::from_secs(hub.clients_config().idle_secs.max(3));
    let (mut sink, mut stream) = socket.split();
    let (res_tx, mut res_rx) = tokio::sync::mpsc::channel::<Message>(64);
    let writer = tokio::spawn(async move {
        loop {
            let frame = tokio::select! {
                // Responses first: a refusal must go out before the close.
                biased;
                Some(m) = res_rx.recv() => m,
                Some(f) = pushes.recv() => Message::Text(f.to_string().into()),
                else => break,
            };
            let closing = matches!(frame, Message::Close(_));
            if sink.send(frame).await.is_err() || closing {
                break;
            }
        }
    });
    let mut welcome = crate::protocol::server_info(hub.edition());
    welcome["type"] = json!("welcome");
    welcome["protocol"] = json!(version);
    welcome["me"] = json!(user);
    if let Some(reason) = refused {
        let _ = res_tx
            .send(Message::Close(Some(CloseFrame {
                code: close_code::POLICY,
                reason: reason.into(),
            })))
            .await;
    } else {
        let _ = res_tx.send(Message::Text(welcome.to_string().into())).await;
    }
    // A peer that sends nothing, not even a pong, is closed (M16).
    let mut ping = tokio::time::interval(idle / 3);
    ping.tick().await;
    let mut heard = tokio::time::Instant::now();
    // Requests of one connection are processed one at a time, in order.
    while refused.is_none() {
        let msg = tokio::select! {
            m = stream.next() => match m {
                Some(Ok(m)) => {
                    heard = tokio::time::Instant::now();
                    m
                }
                _ => break,
            },
            _ = ping.tick() => {
                if heard.elapsed() >= idle {
                    let _ = res_tx
                        .send(Message::Close(Some(CloseFrame {
                            code: close_code::AWAY,
                            reason: "idle".into(),
                        })))
                        .await;
                    break;
                }
                let _ = res_tx.send(Message::Ping(Vec::new().into())).await;
                continue;
            }
            // This node is leaving: reconnect to another one.
            _ = hub.going_away() => {
                let _ = res_tx
                    .send(Message::Close(Some(CloseFrame {
                        code: close_code::RESTART,
                        reason: "server_restart".into(),
                    })))
                    .await;
                break;
            }
            // Logged out: tell the client why, then close.
            _ = session.kicked() => {
                let _ = res_tx
                    .send(Message::Close(Some(CloseFrame {
                        code: close_code::POLICY,
                        reason: "logged_out".into(),
                    })))
                    .await;
                break;
            }
        };
        match msg {
            Message::Text(t) => {
                let res = session.handle_text(t.as_str()).await;
                let too_old = res["error"]["code"] == "client_too_old";
                if res_tx
                    .send(Message::Text(res.to_string().into()))
                    .await
                    .is_err()
                {
                    break;
                }
                if too_old {
                    // A refused client is not left running half compatible.
                    let _ = res_tx
                        .send(Message::Close(Some(CloseFrame {
                            code: close_code::POLICY,
                            reason: "client_too_old".into(),
                        })))
                        .await;
                    break;
                }
            }
            Message::Close(_) => break,
            _ => {}
        }
    }
    drop(session);
    drop(res_tx);
    // Let queued responses (and a close frame) go out, but never hang on a
    // dead peer.
    let abort = writer.abort_handle();
    if tokio::time::timeout(std::time::Duration::from_secs(2), writer)
        .await
        .is_err()
    {
        abort.abort();
    }
}
