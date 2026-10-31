//! Admin API `/admin/v1` (docs/12 §4): what the console and `agentim
//! admin` call. Two areas with their own roles: `app/…` for the developer
//! (owner, developer; moderator for reports and conversations) and
//! `deploy/…` for the deployer (owner, operator). A console session is a
//! cookie plus a CSRF token on every change; `agentim admin` uses an admin
//! key. Every change goes through the same business logic and codes as
//! the client and the open API, and is audited; refusals leave no trace.

use axum::{
    Extension, Json, Router,
    body::Bytes,
    extract::{Path, Query, Request, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::hub::Hub;
use crate::hub::admin::Admin;
use crate::logic::Op;
use crate::model::*;
use crate::open_api::Problem;
use crate::server::AppState;

const COOKIE: &str = "agentim_admin";

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

fn not_found(what: &str) -> Problem {
    Problem::new(StatusCode::NOT_FOUND, "not_found", what.to_string())
}

fn body<T: for<'de> Deserialize<'de>>(b: &Bytes) -> Result<T, Problem> {
    serde_json::from_slice(b).map_err(|e| bad(format!("invalid JSON body: {e}")))
}

fn body_or_default<T: for<'de> Deserialize<'de> + Default>(b: &Bytes) -> Result<T, Problem> {
    if b.is_empty() {
        Ok(T::default())
    } else {
        body(b)
    }
}

/// Which roles may call a route.
fn need(a: &Admin, roles: &[&str]) -> Result<(), Problem> {
    if roles.contains(&a.role.as_str()) {
        Ok(())
    } else {
        Err(Problem::new(
            StatusCode::FORBIDDEN,
            "forbidden",
            format!("this needs the role {}", roles.join(" or ")),
        ))
    }
}

const APP: &[&str] = &["owner", "developer"];
const APP_READ: &[&str] = &["owner", "developer", "moderator"];
const DEPLOY: &[&str] = &["owner", "operator"];
const OWNER: &[&str] = &["owner"];

async fn audit(hub: &Hub, a: &Admin, action: &str, target: Option<&str>, detail: Value) {
    let _ = hub
        .control_call(Op::Audit {
            actor: a.actor(),
            action: format!("admin.{action}"),
            conv_id: String::new(),
            target: target.map(String::from),
            detail,
        })
        .await;
}

fn cookie_of(headers: &HeaderMap) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .map(str::trim)
        .find_map(|c| c.strip_prefix(&format!("{COOKIE}=")))
        .map(String::from)
}

fn set_cookie(hub: &Hub, token: &str, max_age: i64) -> HeaderValue {
    let secure = if hub.config().server.tls.is_some() || hub.open_api_config().behind_proxy {
        "; Secure"
    } else {
        ""
    };
    HeaderValue::from_str(&format!(
        "{COOKIE}={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age={max_age}{secure}"
    ))
    .expect("cookie value")
}

fn session_answer(hub: &Hub, a: &Admin, token: &str) -> Response {
    let csrf = a.session.as_ref().map(|s| s.1.clone());
    let mut r =
        Json(json!({"admin": {"name": a.name, "role": a.role}, "csrf": csrf})).into_response();
    r.headers_mut().insert(
        header::SET_COOKIE,
        set_cookie(hub, token, crate::hub::admin::SESSION_MAX_MS / 1000),
    );
    r
}

/// The console is served by this server: a session is only used from a
/// page of the same origin (the client apps' `allowed_origins` do not
/// count, so a script on the app's site cannot drive the console).
fn same_origin(headers: &HeaderMap) -> bool {
    let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) else {
        return true;
    };
    let host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    ["http://", "https://"]
        .iter()
        .any(|s| origin.eq_ignore_ascii_case(&format!("{s}{host}")))
}

fn loopback(req_peer: Option<std::net::IpAddr>) -> bool {
    req_peer.is_some_and(|p| p.is_loopback())
}

// ------------------------------------------------------------- routes

pub fn routes(state: AppState) -> Router<AppState> {
    let authed = Router::new()
        .route("/admin/v1/logout", post(logout))
        .route("/admin/v1/me", get(me))
        .route("/admin/v1/me/password", post(me_password))
        .route("/admin/v1/me/{action}", post(me_totp))
        // app
        .route("/admin/v1/app/overview", get(overview))
        .route(
            "/admin/v1/app/api-keys",
            get(list_api_keys).post(create_api_key),
        )
        .route(
            "/admin/v1/app/api-keys/{id}",
            axum::routing::delete(revoke_api_key),
        )
        .route(
            "/admin/v1/app/signing-keys",
            get(list_signing_keys).post(create_signing_key),
        )
        .route(
            "/admin/v1/app/signing-keys/{kid}",
            axum::routing::delete(revoke_signing_key),
        )
        .route("/admin/v1/app/users", get(list_users))
        .route(
            "/admin/v1/app/users/{id}",
            get(get_user).patch(patch_user).post(user_action),
        )
        .route(
            "/admin/v1/app/users/{id}/devices/{device}",
            axum::routing::delete(logout_device),
        )
        .route("/admin/v1/app/conversations", get(list_conversations))
        .route("/admin/v1/app/conversations/{id}", get(get_conversation))
        .route(
            "/admin/v1/app/conversations/{id}/messages",
            get(read_messages),
        )
        .route("/admin/v1/app/agents", get(list_agents).post(create_agent))
        .route(
            "/admin/v1/app/agents/{id}",
            post(agent_action).delete(destroy_agent),
        )
        .route("/admin/v1/app/agents/{id}/keys", post(agent_key))
        .route(
            "/admin/v1/app/agents/{id}/keys/{device}",
            axum::routing::delete(revoke_agent_key),
        )
        .route("/admin/v1/app/reports", get(list_reports))
        .route(
            "/admin/v1/app/reports/{id}",
            get(get_report).post(report_action),
        )
        .route("/admin/v1/app/webhooks", get(webhooks))
        .route("/admin/v1/app/audit", get(audit_log))
        .route(
            "/admin/v1/app/settings/clients",
            get(get_client_defaults).put(put_client_defaults),
        )
        // deploy
        .route("/admin/v1/deploy/status", get(deploy_status))
        .route("/admin/v1/deploy/cluster", get(cluster_status))
        .route(
            "/admin/v1/deploy/cluster/nodes/{node}",
            post(cluster_node_action),
        )
        .route(
            "/admin/v1/deploy/admins",
            get(list_admins).post(create_admin),
        )
        .route(
            "/admin/v1/deploy/admins/{name}",
            axum::routing::patch(patch_admin).post(admin_action),
        )
        .route(
            "/admin/v1/deploy/admin-keys",
            get(list_admin_keys).post(create_admin_key),
        )
        .route(
            "/admin/v1/deploy/admin-keys/{id}",
            axum::routing::delete(revoke_admin_key),
        )
        .route(
            "/admin/v1/deploy/dye-rules",
            get(list_dye_rules).post(add_dye_rule),
        )
        .route(
            "/admin/v1/deploy/dye-rules/{id}",
            axum::routing::delete(remove_dye_rule),
        )
        .route_layer(middleware::from_fn_with_state(state, guard));
    Router::new()
        .route("/admin/v1/setup", get(setup_state).post(setup))
        .route("/admin/v1/login", post(login))
        .merge(authed)
}

/// Who calls: a console session (cookie; changes need `X-CSRF-Token`) or
/// an admin key (`Authorization: Bearer adk…`).
async fn guard(State(st): State<AppState>, mut req: Request, next: Next) -> Response {
    let unauthorized = || {
        Problem::new(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "log in to the console or use an admin key",
        )
        .into_response()
    };
    let bearer = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|s| s.trim().to_string());
    let admin = match bearer {
        Some(k) if k.starts_with("adk") => st.hub.admin_from_key(&k).await,
        Some(_) => return unauthorized(),
        None => match cookie_of(req.headers()) {
            Some(c) => {
                // A page on another site cannot use the session.
                if !same_origin(req.headers()) {
                    return Problem::new(StatusCode::FORBIDDEN, "forbidden", "origin not allowed")
                        .into_response();
                }
                st.hub.admin_from_session(&c).await
            }
            None => return unauthorized(),
        },
    };
    let admin = match admin {
        Ok(Some(a)) => a,
        Ok(None) => return unauthorized(),
        Err(e) => return Problem::from(e).into_response(),
    };
    if let Some((_, csrf)) = &admin.session
        && req.method() != axum::http::Method::GET
    {
        let sent = req
            .headers()
            .get("x-csrf-token")
            .and_then(|v| v.to_str().ok());
        if sent != Some(csrf.as_str()) {
            return Problem::new(
                StatusCode::FORBIDDEN,
                "forbidden",
                "missing or wrong X-CSRF-Token",
            )
            .into_response();
        }
    }
    req.extensions_mut().insert(admin);
    next.run(req).await
}

type A = Extension<Admin>;

// ------------------------------------------------------------- accounts

async fn setup_state(State(st): State<AppState>) -> R {
    ok(json!({"needed": st.hub.setup_needed().await?}))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetupReq {
    token: String,
    name: String,
    password: String,
}

async fn setup(State(st): State<AppState>, req: Request) -> R {
    // Only from this machine: the link is printed in the server's log.
    if !loopback(crate::open_api::peer_ip(&req)) {
        return Err(Problem::new(
            StatusCode::FORBIDDEN,
            "forbidden",
            "set up the console from the machine it runs on",
        ));
    }
    let b = axum::body::to_bytes(req.into_body(), 1 << 16)
        .await
        .map_err(|_| bad("body too large"))?;
    let r: SetupReq = body(&b)?;
    let (a, token) = st.hub.admin_setup(&r.token, &r.name, &r.password).await?;
    audit(&st.hub, &a, "setup", Some(&a.name), json!({})).await;
    Ok(session_answer(&st.hub, &a, &token))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LoginReq {
    name: String,
    password: String,
    #[serde(default)]
    totp: Option<String>,
}

async fn login(State(st): State<AppState>, headers: HeaderMap, b: Bytes) -> R {
    if !same_origin(&headers) {
        return Err(Problem::new(
            StatusCode::FORBIDDEN,
            "forbidden",
            "origin not allowed",
        ));
    }
    let r: LoginReq = body(&b)?;
    match st
        .hub
        .admin_login(&r.name, &r.password, r.totp.as_deref())
        .await
    {
        Ok((a, token)) => {
            audit(&st.hub, &a, "login", Some(&a.name), json!({})).await;
            Ok(session_answer(&st.hub, &a, &token))
        }
        Err(e) if e.code == "invalid_credentials" || e.code == "totp_required" => {
            Err(Problem::new(StatusCode::UNAUTHORIZED, e.code, e.message))
        }
        Err(e) => Err(e.into()),
    }
}

async fn logout(State(st): State<AppState>, Extension(a): A) -> R {
    st.hub.admin_logout(&a).await?;
    let mut r = Json(json!({})).into_response();
    r.headers_mut()
        .insert(header::SET_COOKIE, set_cookie(&st.hub, "", 0));
    Ok(r)
}

async fn me(State(st): State<AppState>, Extension(a): A) -> R {
    let totp = if a.session.is_some() {
        st.hub.admin_totp_on(&a.id).await?
    } else {
        false
    };
    ok(json!({
        "admin": {"name": a.name, "role": a.role},
        "csrf": a.session.as_ref().map(|s| s.1.clone()),
        "totp": totp,
    }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PasswordReq {
    current: String,
    new: String,
}

async fn me_password(State(st): State<AppState>, Extension(a): A, b: Bytes) -> R {
    if a.session.is_none() {
        return Err(bad("an admin key has no password"));
    }
    let r: PasswordReq = body(&b)?;
    st.hub.admin_change_password(&a, &r.current, &r.new).await?;
    audit(&st.hub, &a, "password", Some(&a.name), json!({})).await;
    ok(json!({"logged_out_everywhere": true}))
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct CodeReq {
    code: Option<String>,
}

/// `POST /admin/v1/me/totp:setup`, `:enable {code}`, `:disable {code}`.
async fn me_totp(
    State(st): State<AppState>,
    Extension(a): A,
    Path(action): Path<String>,
    b: Bytes,
) -> R {
    if a.session.is_none() {
        return Err(bad("an admin key has no two-factor login"));
    }
    let r: CodeReq = body_or_default(&b)?;
    let code = || r.code.clone().ok_or_else(|| bad("code is required"));
    let v = match action.as_str() {
        "totp:setup" => st.hub.admin_totp_setup(&a).await?,
        "totp:enable" => {
            st.hub.admin_totp_enable(&a, &code()?).await?;
            json!({"totp": true})
        }
        "totp:disable" => {
            st.hub.admin_totp_disable(&a, &code()?).await?;
            json!({"totp": false})
        }
        _ => return Err(not_found("unknown action")),
    };
    if action != "totp:setup" {
        audit(
            &st.hub,
            &a,
            &action.replace(':', "."),
            Some(&a.name),
            json!({}),
        )
        .await;
    }
    ok(v)
}

// ------------------------------------------------------------- app

async fn overview(State(st): State<AppState>, Extension(a): A) -> R {
    need(&a, APP_READ)?;
    ok(st.hub.admin_overview().await?)
}

async fn list_api_keys(State(st): State<AppState>, Extension(a): A) -> R {
    need(&a, APP)?;
    let keys: Vec<Value> = st.hub.api_keys().iter().map(|k| k.view()).collect();
    ok(json!({"keys": keys, "scopes": crate::open_api::SCOPES}))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ApiKeyReq {
    name: String,
    scopes: Vec<String>,
    ip_allow: Option<Vec<String>>,
    ttl_days: Option<i64>,
}

async fn create_api_key(State(st): State<AppState>, Extension(a): A, b: Bytes) -> R {
    need(&a, APP)?;
    let r: ApiKeyReq = body(&b)?;
    if r.ttl_days.is_some_and(|d| !(1..=3650).contains(&d)) {
        return Err(bad("ttl_days is 1..3650"));
    }
    let (secret, row) = st
        .hub
        .create_api_key(
            &r.name,
            &r.scopes,
            r.ttl_days.map(|d| d * 86_400_000),
            r.ip_allow,
        )
        .await?;
    audit(
        &st.hub,
        &a,
        "api_keys.create",
        Some(&row.key_id),
        json!({"scopes": r.scopes}),
    )
    .await;
    let mut v = row.view();
    v["key"] = json!(secret);
    created(v)
}

async fn revoke_api_key(State(st): State<AppState>, Extension(a): A, Path(id): Path<String>) -> R {
    need(&a, APP)?;
    if !st.hub.revoke_api_key(&id).await? {
        return Err(not_found("no such key in force"));
    }
    audit(&st.hub, &a, "api_keys.revoke", Some(&id), json!({})).await;
    ok(json!({"revoked": id}))
}

async fn list_signing_keys(State(st): State<AppState>, Extension(a): A) -> R {
    need(&a, APP)?;
    ok(json!({"keys": st.hub.signing_keys().await?}))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NameReq {
    name: String,
}

async fn create_signing_key(State(st): State<AppState>, Extension(a): A, b: Bytes) -> R {
    need(&a, APP)?;
    let r: NameReq = body(&b)?;
    let v = st.hub.create_signing_key(&r.name).await?;
    audit(
        &st.hub,
        &a,
        "signing_keys.create",
        v["kid"].as_str(),
        json!({}),
    )
    .await;
    created(v)
}

async fn revoke_signing_key(
    State(st): State<AppState>,
    Extension(a): A,
    Path(kid): Path<String>,
) -> R {
    need(&a, APP)?;
    st.hub.revoke_signing_key(&kid).await?;
    audit(&st.hub, &a, "signing_keys.revoke", Some(&kid), json!({})).await;
    ok(json!({"revoked": kid}))
}

// ------------------------------------------------------------ dye rules

async fn list_dye_rules(State(st): State<AppState>, Extension(a): A) -> R {
    need(&a, DEPLOY)?;
    ok(json!({"rules": st.hub.dye_rules(), "max": crate::trace::DYE_MAX_RULES}))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DyeReq {
    kind: String,
    target: String,
    reason: String,
    minutes: Option<i64>,
}

/// Dyes a user, device, conversation, agent or API key (docs/14 §4.2):
/// with a reason, audited, for at most 24 hours.
async fn add_dye_rule(State(st): State<AppState>, Extension(a): A, b: Bytes) -> R {
    need(&a, DEPLOY)?;
    let r: DyeReq = body(&b)?;
    let rule = st
        .hub
        .add_dye_rule(&r.kind, &r.target, &r.reason, r.minutes, &a.actor())
        .await?;
    audit(
        &st.hub,
        &a,
        "dye_rules.add",
        Some(&rule.id),
        json!({"kind": rule.kind, "target": rule.target, "reason": rule.reason, "expires_at": rule.expires_at}),
    )
    .await;
    created(json!(rule))
}

async fn remove_dye_rule(State(st): State<AppState>, Extension(a): A, Path(id): Path<String>) -> R {
    need(&a, DEPLOY)?;
    st.hub.remove_dye_rule(&id).await?;
    audit(&st.hub, &a, "dye_rules.remove", Some(&id), json!({})).await;
    ok(json!({"removed": id}))
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct PageQuery {
    q: Option<String>,
    after: Option<String>,
    limit: Option<i64>,
    kind: Option<String>,
}

fn limit_of(l: Option<i64>) -> Result<i64, Problem> {
    let l = l.unwrap_or(50);
    if !(1..=200).contains(&l) {
        return Err(bad("limit is 1..200"));
    }
    Ok(l)
}

async fn list_users(State(st): State<AppState>, Extension(a): A, Query(q): Query<PageQuery>) -> R {
    need(&a, APP_READ)?;
    let kind = match q.kind.as_deref() {
        None => None,
        Some("human") => Some(UserKind::Human),
        Some("agent") => Some(UserKind::Agent),
        Some(_) => return Err(bad("kind is human or agent")),
    };
    let limit = limit_of(q.limit)?;
    ok(st
        .hub
        .admin_users(q.q.as_deref(), kind, q.after.as_deref(), limit))
}

fn user_of(hub: &Hub, id: &str) -> Result<User, Problem> {
    hub.user_by_name(id)
        .filter(|u| u.kind != UserKind::System)
        .ok_or_else(|| not_found(&format!("user {id}")))
}

async fn get_user(State(st): State<AppState>, Extension(a): A, Path(id): Path<String>) -> R {
    need(&a, APP_READ)?;
    let u = user_of(&st.hub, &id)?;
    ok(st.hub.admin_user(&u).await?)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PatchUserReq {
    permissions: Value,
}

async fn patch_user(
    State(st): State<AppState>,
    Extension(a): A,
    Path(id): Path<String>,
    b: Bytes,
) -> R {
    need(&a, APP)?;
    let r: PatchUserReq = body(&b)?;
    let u = user_of(&st.hub, &id)?;
    let v = st.hub.set_permissions(&u, &r.permissions).await?;
    audit(
        &st.hub,
        &a,
        "users.permissions",
        Some(&id),
        json!({"permissions": r.permissions}),
    )
    .await;
    ok(v)
}

async fn user_action(State(st): State<AppState>, Extension(a): A, Path(seg): Path<String>) -> R {
    need(&a, APP)?;
    let (id, on) = match seg.rsplit_once(':') {
        Some((id, "deactivate")) => (id, true),
        Some((id, "reactivate")) => (id, false),
        _ => return Err(not_found("unknown action")),
    };
    let u = user_of(&st.hub, id)?;
    st.hub.set_deactivated(&u.id, on).await?;
    audit(
        &st.hub,
        &a,
        if on {
            "users.deactivate"
        } else {
            "users.reactivate"
        },
        Some(id),
        json!({}),
    )
    .await;
    ok(st.hub.admin_user(&u).await?)
}

async fn logout_device(
    State(st): State<AppState>,
    Extension(a): A,
    Path((id, device)): Path<(String, String)>,
) -> R {
    need(&a, APP)?;
    let u = user_of(&st.hub, &id)?;
    if !st
        .hub
        .devices(&u.id)
        .await?
        .iter()
        .any(|d| d.device_id == device)
    {
        return Err(not_found("no such device"));
    }
    let n = if u.kind == UserKind::Agent {
        st.hub.revoke_tokens(&u.id, Some(&device)).await?
    } else {
        st.hub.log_out(&u.id, Some(&device)).await?
    };
    audit(
        &st.hub,
        &a,
        "devices.logout",
        Some(&id),
        json!({"device_id": device}),
    )
    .await;
    ok(json!({"revoked": n}))
}

async fn list_conversations(
    State(st): State<AppState>,
    Extension(a): A,
    Query(q): Query<PageQuery>,
) -> R {
    need(&a, APP_READ)?;
    let limit = limit_of(q.limit)?;
    ok(st
        .hub
        .admin_conversations(q.q.as_deref(), q.after.as_deref(), limit)
        .await?)
}

async fn get_conversation(
    State(st): State<AppState>,
    Extension(a): A,
    Path(id): Path<String>,
) -> R {
    need(&a, APP_READ)?;
    ok(st.hub.admin_conversation(&id).await?)
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct ReadQuery {
    reason: Option<String>,
    after_seq: Option<i64>,
    limit: Option<i64>,
}

/// Reading a conversation's content: a reason is required and every read
/// is audited (GDPR purpose limitation, docs/12 §4).
async fn read_messages(
    State(st): State<AppState>,
    Extension(a): A,
    Path(id): Path<String>,
    Query(q): Query<ReadQuery>,
) -> R {
    need(&a, APP_READ)?;
    let reason = q
        .reason
        .filter(|r| !r.trim().is_empty() && r.len() <= 500)
        .ok_or_else(|| bad("a reason (1..500 bytes) is required to read content"))?;
    let after = q.after_seq.unwrap_or(0);
    if after < 0 || q.limit.is_some_and(|l| !(1..=200).contains(&l)) {
        return Err(bad("after_seq >= 0 and limit 1..200"));
    }
    let sys = st.hub.system_user();
    let v = st
        .hub
        .act_as(
            &sys,
            ClientMsg::Pull {
                conv_id: id.clone(),
                after_seq: after,
                limit: Some(q.limit.unwrap_or(50)),
                as_agent: None,
            },
        )
        .await?;
    audit(
        &st.hub,
        &a,
        "conversations.read",
        Some(&id),
        json!({"reason": reason, "after_seq": after}),
    )
    .await;
    ok(v)
}

async fn list_agents(State(st): State<AppState>, Extension(a): A, Query(q): Query<PageQuery>) -> R {
    need(&a, APP)?;
    let limit = limit_of(q.limit)?;
    ok(st.hub.admin_users(
        q.q.as_deref(),
        Some(UserKind::Agent),
        q.after.as_deref(),
        limit,
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateAgentReq {
    id: String,
    owner: String,
    #[serde(default)]
    team: bool,
}

async fn create_agent(State(st): State<AppState>, Extension(a): A, b: Bytes) -> R {
    need(&a, APP)?;
    let r: CreateAgentReq = body(&b)?;
    let owner = user_of(&st.hub, &r.owner)?;
    if owner.kind != UserKind::Human {
        return Err(bad("an agent's owner is a person"));
    }
    let v = st.hub.create_agent_with(&owner, &r.id, r.team).await?;
    audit(
        &st.hub,
        &a,
        "agents.create",
        Some(&r.id),
        json!({"owner": r.owner, "team": r.team}),
    )
    .await;
    created(
        json!({"id": r.id, "key": v["key"], "device_id": v["device_id"], "connect": v["connect"]}),
    )
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct AgentActionReq {
    reason: Option<String>,
    owner: Option<String>,
    #[serde(default)]
    rotate_keys: bool,
}

async fn agent_action(
    State(st): State<AppState>,
    Extension(a): A,
    Path(seg): Path<String>,
    b: Bytes,
) -> R {
    need(&a, APP)?;
    let Some((id, action)) = seg.rsplit_once(':') else {
        return Err(not_found("POST …/agents/{id}:stop, :resume or :transfer"));
    };
    let r: AgentActionReq = body_or_default(&b)?;
    let agent = st.hub.live_agent_any(id)?;
    let by = a.actor();
    match action {
        "stop" => st.hub.stop_agent_as(&agent, &by, r.reason.clone()).await?,
        "resume" => st.hub.resume_agent_as(&agent, &by).await?,
        "transfer" => {
            let o = r.owner.as_deref().ok_or_else(|| bad("owner is required"))?;
            let owner = user_of(&st.hub, o)?;
            st.hub
                .transfer_agent(&agent.id, &owner.id, r.rotate_keys)
                .await?
        }
        _ => return Err(not_found("unknown action")),
    };
    audit(
        &st.hub,
        &a,
        &format!("agents.{action}"),
        Some(id),
        json!({"reason": r.reason, "owner": r.owner}),
    )
    .await;
    let agent = st.hub.shared().user(&agent.id).unwrap_or(agent);
    ok(st.hub.admin_user(&agent).await?)
}

async fn destroy_agent(State(st): State<AppState>, Extension(a): A, Path(id): Path<String>) -> R {
    need(&a, APP)?;
    let agent = st.hub.live_agent_any(&id)?;
    let v = st.hub.destroy_agent_as(&agent, &a.actor()).await?;
    audit(&st.hub, &a, "agents.destroy", Some(&id), json!({})).await;
    ok(json!({"id": id, "destroyed": true, "conversations": v["conversations"]}))
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct KeyReq {
    #[serde(default)]
    revoke_others: bool,
}

async fn agent_key(
    State(st): State<AppState>,
    Extension(a): A,
    Path(id): Path<String>,
    b: Bytes,
) -> R {
    need(&a, APP)?;
    let r: KeyReq = body_or_default(&b)?;
    let agent = st.hub.live_agent_any(&id)?;
    let v = st
        .hub
        .issue_agent_key(&agent, &a.actor(), r.revoke_others)
        .await?;
    audit(
        &st.hub,
        &a,
        "agents.key",
        Some(&id),
        json!({"device_id": v["device_id"]}),
    )
    .await;
    created(v)
}

async fn revoke_agent_key(
    State(st): State<AppState>,
    Extension(a): A,
    Path((id, device)): Path<(String, String)>,
) -> R {
    need(&a, APP)?;
    let agent = st.hub.live_agent_any(&id)?;
    if !st
        .hub
        .devices(&agent.id)
        .await?
        .iter()
        .any(|d| d.device_id == device)
    {
        return Err(not_found("no such key"));
    }
    let n = st.hub.revoke_tokens(&agent.id, Some(&device)).await?;
    audit(
        &st.hub,
        &a,
        "agents.revoke_key",
        Some(&id),
        json!({"device_id": device}),
    )
    .await;
    ok(json!({"revoked": n}))
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct ReportsQuery {
    state: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
}

async fn list_reports(
    State(st): State<AppState>,
    Extension(a): A,
    Query(q): Query<ReportsQuery>,
) -> R {
    need(&a, APP_READ)?;
    let open = match q.state.as_deref() {
        None | Some("open") => true,
        Some("resolved") => false,
        Some(_) => return Err(bad("state is open or resolved")),
    };
    let limit = limit_of(q.limit)?;
    let offset = q.offset.unwrap_or(0);
    if offset < 0 {
        return Err(bad("offset >= 0"));
    }
    let items = st.hub.reports(open, limit, offset).await?;
    let more = items.len() as i64 == limit;
    ok(json!({"reports": items, "has_more": more}))
}

async fn get_report(State(st): State<AppState>, Extension(a): A, Path(id): Path<String>) -> R {
    need(&a, APP_READ)?;
    match st.hub.report_detail(&id).await? {
        Some(r) => ok(r),
        None => Err(not_found("no such report")),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResolveReq {
    action: String,
    reason: String,
    ground: Option<String>,
    #[serde(default)]
    automated: bool,
    days: Option<i64>,
}

async fn report_action(
    State(st): State<AppState>,
    Extension(a): A,
    Path(seg): Path<String>,
    b: Bytes,
) -> R {
    need(&a, APP_READ)?;
    let Some((id, "resolve")) = seg.rsplit_once(':') else {
        return Err(not_found("unknown action"));
    };
    let r: ResolveReq = body(&b)?;
    let v = st
        .hub
        .resolve_report(
            id,
            &r.action,
            &r.reason,
            r.ground.as_deref(),
            r.automated,
            r.days,
            &a.actor(),
        )
        .await?;
    audit(
        &st.hub,
        &a,
        "reports.resolve",
        Some(id),
        json!({"action": r.action}),
    )
    .await;
    ok(v)
}

async fn webhooks(State(st): State<AppState>, Extension(a): A) -> R {
    need(&a, APP)?;
    ok(st.hub.admin_webhooks().await?)
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct AuditQuery {
    prefix: Option<String>,
    before: Option<String>,
    limit: Option<i64>,
}

async fn audit_log(State(st): State<AppState>, Extension(a): A, Query(q): Query<AuditQuery>) -> R {
    need(&a, APP)?;
    let limit = limit_of(q.limit)?;
    ok(st
        .hub
        .admin_audit(q.prefix.as_deref(), q.before.as_deref(), limit)
        .await?)
}

async fn get_client_defaults(State(st): State<AppState>, Extension(a): A) -> R {
    need(&a, APP)?;
    ok(json!(st.hub.client_defaults()))
}

async fn put_client_defaults(State(st): State<AppState>, Extension(a): A, b: Bytes) -> R {
    need(&a, APP)?;
    let v: Value = body(&b)?;
    let set = st.hub.set_client_defaults(&v).await?;
    audit(&st.hub, &a, "settings.clients", None, json!(set)).await;
    ok(json!(set))
}

// ------------------------------------------------------------- deploy

async fn deploy_status(State(st): State<AppState>, Extension(a): A) -> R {
    need(&a, DEPLOY)?;
    ok(st.hub.admin_deploy_status())
}

async fn cluster_status(State(st): State<AppState>, Extension(a): A) -> R {
    need(&a, DEPLOY)?;
    ok(st.hub.cluster_admin(crate::cli::ClusterCmd::Status).await?)
}

async fn cluster_node_action(
    State(st): State<AppState>,
    Extension(a): A,
    Path(seg): Path<String>,
) -> R {
    need(&a, DEPLOY)?;
    let Some((node, "drain")) = seg.rsplit_once(':') else {
        return Err(not_found("unknown action"));
    };
    let v = st
        .hub
        .cluster_admin(crate::cli::ClusterCmd::Drain {
            node: node.to_string(),
        })
        .await?;
    audit(&st.hub, &a, "cluster.drain", Some(node), json!({})).await;
    ok(v)
}

async fn list_admins(State(st): State<AppState>, Extension(a): A) -> R {
    need(&a, OWNER)?;
    let list: Vec<Value> = st.hub.admins().await?.iter().map(|x| x.view()).collect();
    ok(json!({"admins": list, "roles": crate::hub::admin::ROLES}))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AdminReq {
    name: String,
    role: String,
    password: String,
}

async fn create_admin(State(st): State<AppState>, Extension(a): A, b: Bytes) -> R {
    need(&a, OWNER)?;
    let r: AdminReq = body(&b)?;
    let row = st.hub.create_admin(&r.name, &r.role, &r.password).await?;
    audit(
        &st.hub,
        &a,
        "admins.create",
        Some(&r.name),
        json!({"role": r.role}),
    )
    .await;
    created(row.view())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PatchAdminReq {
    role: Option<String>,
    disabled: Option<bool>,
}

async fn patch_admin(
    State(st): State<AppState>,
    Extension(a): A,
    Path(name): Path<String>,
    b: Bytes,
) -> R {
    need(&a, OWNER)?;
    let r: PatchAdminReq = body(&b)?;
    let row = st
        .hub
        .update_admin(&name, r.role.as_deref(), r.disabled)
        .await?;
    audit(
        &st.hub,
        &a,
        "admins.update",
        Some(&name),
        json!({"role": r.role, "disabled": r.disabled}),
    )
    .await;
    ok(row.view())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResetReq {
    password: String,
}

async fn admin_action(
    State(st): State<AppState>,
    Extension(a): A,
    Path(seg): Path<String>,
    b: Bytes,
) -> R {
    need(&a, OWNER)?;
    let Some((name, "reset-password")) = seg.rsplit_once(':') else {
        return Err(not_found("unknown action"));
    };
    let r: ResetReq = body(&b)?;
    st.hub.reset_admin_password(name, &r.password).await?;
    audit(&st.hub, &a, "admins.reset_password", Some(name), json!({})).await;
    ok(json!({"name": name, "logged_out_everywhere": true}))
}

async fn list_admin_keys(State(st): State<AppState>, Extension(a): A) -> R {
    need(&a, OWNER)?;
    let keys: Vec<Value> = st
        .hub
        .admin_keys()
        .await?
        .iter()
        .map(|k| k.view())
        .collect();
    ok(json!({"keys": keys}))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AdminKeyReq {
    name: String,
    role: String,
}

async fn create_admin_key(State(st): State<AppState>, Extension(a): A, b: Bytes) -> R {
    need(&a, OWNER)?;
    let r: AdminKeyReq = body(&b)?;
    let (secret, row) = st.hub.create_admin_key(&r.name, &r.role).await?;
    audit(
        &st.hub,
        &a,
        "admin_keys.create",
        Some(&row.key_id),
        json!({"role": r.role}),
    )
    .await;
    let mut v = row.view();
    v["key"] = json!(secret);
    created(v)
}

async fn revoke_admin_key(
    State(st): State<AppState>,
    Extension(a): A,
    Path(id): Path<String>,
) -> R {
    need(&a, OWNER)?;
    st.hub.revoke_admin_key(&id).await?;
    audit(&st.hub, &a, "admin_keys.revoke", Some(&id), json!({})).await;
    ok(json!({"revoked": id}))
}

/// Every admin route as `METHOD path`, for the consistency test.
pub const ROUTES: &[&str] = &[
    "GET /admin/v1/setup",
    "POST /admin/v1/setup",
    "POST /admin/v1/login",
    "POST /admin/v1/logout",
    "GET /admin/v1/me",
    "POST /admin/v1/me/password",
    "POST /admin/v1/me/totp:setup",
    "GET /admin/v1/app/overview",
    "GET /admin/v1/app/users",
    "GET /admin/v1/app/conversations",
    "GET /admin/v1/app/agents",
    "GET /admin/v1/app/reports",
    "GET /admin/v1/app/webhooks",
    "GET /admin/v1/app/audit",
    "GET /admin/v1/app/settings/clients",
    "GET /admin/v1/app/api-keys",
    "GET /admin/v1/app/signing-keys",
    "GET /admin/v1/deploy/status",
    "GET /admin/v1/deploy/cluster",
    "GET /admin/v1/deploy/admins",
    "GET /admin/v1/deploy/admin-keys",
];
