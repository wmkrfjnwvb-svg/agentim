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
use serde_json::{Value, json};

use crate::hub::Hub;

#[derive(Clone)]
pub struct AppState {
    pub hub: Hub,
    /// Enables password-less login by user name (demo only).
    pub demo_login: bool,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/ws", get(ws))
        .route("/api/login", post(login))
        .route("/api/stats", get(stats))
        .route("/api/dev/users", post(dev_create_user))
        .route("/healthz", get(|| async { "ok" }))
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
    match st.hub.store(|s| s.token_by_name(&req.name)) {
        Ok(Some((user, token))) if user.kind == crate::model::UserKind::Human => {
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
        crate::model::UserKind::Agent
    } else {
        crate::model::UserKind::Human
    };
    match st
        .hub
        .store(|s| s.create_user(&req.name, kind, req.owner.as_deref()))
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
    let Some(_user) = bearer(&headers).and_then(|t| st.hub.authenticate(&t)) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    Json(st.hub.stats()).into_response()
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
    upgrade.on_upgrade(move |socket| run_socket(st.hub, user, socket))
}

async fn run_socket(hub: Hub, user: crate::model::User, socket: WebSocket) {
    let (session, mut rx) = hub.connect(user.clone());
    let (mut sink, mut stream) = socket.split();
    let (out_tx, mut out_rx) = tokio::sync::mpsc::unbounded_channel::<Value>();
    let writer = tokio::spawn(async move {
        loop {
            let v = tokio::select! {
                Some(v) = rx.recv() => v,
                Some(v) = out_rx.recv() => v,
                else => break,
            };
            if sink
                .send(Message::Text(v.to_string().into()))
                .await
                .is_err()
            {
                break;
            }
        }
    });
    let _ = out_tx.send(json!({"type": "welcome", "me": user}));
    while let Some(Ok(msg)) = stream.next().await {
        match msg {
            Message::Text(t) => {
                let res = session.handle_text(t.as_str());
                if out_tx.send(res).is_err() {
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
