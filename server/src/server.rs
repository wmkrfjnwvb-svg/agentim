//! HTTP + WebSocket front end.

use axum::{
    Json, Router,
    extract::{
        Query, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
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
    match st.hub.token_of(&req.name) {
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
}

async fn ws(
    State(st): State<AppState>,
    Query(q): Query<WsQuery>,
    upgrade: WebSocketUpgrade,
) -> Response {
    let Some(user) = st.hub.authenticate(&q.token) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    upgrade
        .max_message_size(1 << 20)
        .on_upgrade(move |socket| run_socket(st.hub, user, socket))
}

async fn run_socket(hub: Hub, user: User, socket: WebSocket) {
    let (session, mut pushes) = hub.connect(user.clone());
    let (mut sink, mut stream) = socket.split();
    let (res_tx, mut res_rx) = tokio::sync::mpsc::channel::<String>(64);
    let writer = tokio::spawn(async move {
        loop {
            let text: String = tokio::select! {
                Some(v) = res_rx.recv() => v,
                Some(f) = pushes.recv() => f.to_string(),
                else => break,
            };
            if sink.send(Message::Text(text.into())).await.is_err() {
                break;
            }
        }
    });
    let _ = res_tx
        .send(json!({"type": "welcome", "me": user}).to_string())
        .await;
    // Requests of one connection are processed one at a time, in order.
    while let Some(Ok(msg)) = stream.next().await {
        match msg {
            Message::Text(t) => {
                let res = session.handle_text(t.as_str()).await;
                if res_tx.send(res.to_string()).await.is_err() {
                    break;
                }
            }
            Message::Close(_) => break,
            _ => {}
        }
    }
    drop(session);
    writer.abort();
}
