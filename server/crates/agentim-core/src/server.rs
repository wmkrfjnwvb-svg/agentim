//! HTTP + WebSocket front end.

use axum::{
    Json, Router,
    extract::{
        Query, State,
        ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade, close_code},
    },
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::json;

use crate::hub::Hub;
use crate::model::UserKind;

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
        .merge(crate::admin_api::routes(state.clone()))
        .merge(crate::console::routes())
        .merge(crate::demo_web::routes())
        .route("/ws", get(ws))
        .route("/api/login", post(login))
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
async fn dev_create_user(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<DevUserReq>,
) -> Response {
    if !origin_allowed(&st, &headers) {
        return origin_refused();
    }
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
        Ok((user, key)) => {
            // An agent's key, or a person's signed client token (connect
            // with `device=`).
            let token = key.or_else(|| st.hub.sign_token(&user.name, 86400).ok());
            Json(json!({"user": user, "token": token})).into_response()
        }
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
    /// A person's signed client token or an agent's key; native clients
    /// may send it as `Authorization: Bearer` instead (browsers cannot).
    #[serde(default)]
    token: Option<String>,
    /// The id the app chose for this device (people; docs/13 §4.2).
    #[serde(default)]
    device: Option<String>,
    #[serde(default)]
    device_name: Option<String>,
    #[serde(default)]
    platform: Option<String>,
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
    let Some(token) = q.token.clone().or_else(|| bearer(&headers)) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let login = match st
        .hub
        .authenticate_client(
            &token,
            q.device.as_deref(),
            q.device_name.as_deref(),
            q.platform.as_deref(),
        )
        .await
    {
        Ok(l) => l,
        Err(e) if e.code == "bad_request" => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": {"code": e.code, "message": e.message}})),
            )
                .into_response();
        }
        Err(e) if e.code == "unauthorized" => {
            tracing::debug!(reason = e.message, "client token refused");
            return StatusCode::UNAUTHORIZED.into_response();
        }
        Err(e) if e.code == "forbidden" => {
            return (
                StatusCode::FORBIDDEN,
                Json(json!({"error": {"code": e.code, "message": e.message}})),
            )
                .into_response();
        }
        Err(e) => {
            // Busy or storage trouble: come back later, not all at once.
            let (status, wait) = if e.code == "rate_limited" {
                (
                    StatusCode::TOO_MANY_REQUESTS,
                    e.retry_after_ms.unwrap_or(1000),
                )
            } else {
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    e.retry_after_ms.unwrap_or_else(spread_ms),
                )
            };
            let mut r = (status, Json(json!({"error": e.to_json()}))).into_response();
            r.headers_mut().insert(
                axum::http::header::RETRY_AFTER,
                axum::http::HeaderValue::from(wait.div_ceil(1000).max(1)),
            );
            return r;
        }
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
        .on_upgrade(move |socket| run_socket(st.hub, login, version, socket))
}

/// The application-level heartbeat clients are asked for (docs/08 §2.9).
const HEARTBEAT_MS: u64 = 25_000;

/// A random wait of 0–5 s that spreads reconnections and retries.
fn spread_ms() -> u64 {
    (uuid::Uuid::new_v4().as_u128() % 5000) as u64
}

async fn run_socket(hub: Hub, login: crate::hub::login::Login, version: u32, socket: WebSocket) {
    let user = login.user.clone();
    let (session, mut pushes, refused) = hub.open_device_session(&login);
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
    let mut welcome = crate::protocol::server_info();
    welcome["type"] = json!("welcome");
    welcome["protocol"] = json!(version);
    welcome["me"] = json!(user);
    welcome["device_id"] = json!(login.device_id);
    // How often the client should ping when nothing else is sent, and how
    // long the server waits for any frame before closing (docs/08 §2.9).
    welcome["heartbeat_ms"] = json!(HEARTBEAT_MS.min(idle.as_millis() as u64 / 3));
    welcome["idle_ms"] = json!(idle.as_millis() as u64);
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
            // This node is leaving: reconnect to another one, after a
            // random wait so the clients do not all come back at once.
            _ = hub.going_away() => {
                let goaway = json!({"type": "goaway", "reason": "server_restart", "retry_after_ms": spread_ms()});
                let _ = res_tx.send(Message::Text(goaway.to_string().into())).await;
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
