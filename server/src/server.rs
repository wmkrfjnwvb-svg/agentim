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
        .route("/", get(index))
        .route("/ws", get(ws))
        .route("/api/login", post(login))
        .route("/api/stats", get(stats))
        .route("/api/dev/users", post(dev_create_user))
        .route("/metrics", get(metrics))
        .route("/healthz", get(|| async { "ok" }))
        .route("/readyz", get(readyz))
        .with_state(state)
}

async fn index() -> Html<&'static str> {
    Html(include_str!("../web/index.html"))
}

#[derive(Deserialize)]
struct LoginReq {
    name: String,
}

async fn login(State(st): State<AppState>, Json(req): Json<LoginReq>) -> Response {
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
    upgrade: WebSocketUpgrade,
) -> Response {
    let Some((user, device)) = st.hub.authenticate_device(&q.token) else {
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
        .on_upgrade(move |socket| run_socket(st.hub, user, device, version, socket))
}

async fn run_socket(hub: Hub, user: User, device: String, version: u32, socket: WebSocket) {
    let (session, mut pushes) = hub.connect_device(user.clone(), Some(device));
    session.set_protocol(version);
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
    let _ = res_tx.send(Message::Text(welcome.to_string().into())).await;
    // Requests of one connection are processed one at a time, in order.
    loop {
        let msg = tokio::select! {
            m = stream.next() => match m {
                Some(Ok(m)) => m,
                _ => break,
            },
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
