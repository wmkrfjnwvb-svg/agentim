//! MCP endpoint `POST /mcp` (docs/03-agents.md §2.1–§2.3): agents use
//! AgentIM as tools from any MCP client. Stateless per the 2026-07-28
//! revision (version, client info and capabilities in `_meta` on every
//! request, mirrored headers checked against the body, `server/discover`),
//! and still answers the `initialize` handshake of 2025-03-26..2025-11-25
//! clients, without session ids. Every tool call runs as one client request
//! of the agent's key, so permissions, guards and audit are the same as on
//! the WebSocket.

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use base64::Engine;
use serde_json::{Map, Value, json};

use crate::model::*;
use crate::server::{AppState, origin_allowed};

/// Revisions with per-request metadata.
pub const MODERN: &[&str] = &["2026-07-28"];
/// Revisions with the `initialize` handshake.
pub const LEGACY: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26"];

const META_VERSION: &str = "io.modelcontextprotocol/protocolVersion";
const META_SERVER: &str = "io.modelcontextprotocol/serverInfo";

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const HEADER_MISMATCH: i64 = -32020;
const UNSUPPORTED_VERSION: i64 = -32022;

const INSTRUCTIONS: &str = "AgentIM is the conversation layer you share with people and other agents. Call get_updates to receive what is new in your conversations (each call moves your position forward), send_message to answer. Everything you send is labelled as written by an AI agent.";

fn all_versions() -> Vec<&'static str> {
    MODERN.iter().chain(LEGACY).copied().collect()
}

fn rpc_error(
    status: StatusCode,
    id: &Value,
    code: i64,
    message: &str,
    data: Option<Value>,
) -> Response {
    let mut e = json!({"code": code, "message": message});
    if let Some(d) = data {
        e["data"] = d;
    }
    let mut body = json!({"jsonrpc": "2.0", "error": e});
    if !id.is_null() {
        body["id"] = id.clone();
    }
    (status, axum::Json(body)).into_response()
}

fn rpc_result(id: &Value, result: Value) -> Response {
    axum::Json(json!({"jsonrpc": "2.0", "id": id, "result": result})).into_response()
}

fn header_str<'a>(h: &'a HeaderMap, name: &str) -> Option<&'a str> {
    h.get(name).and_then(|v| v.to_str().ok())
}

/// `Mcp-Name` may carry `=?base64?...?=` (2026-07-28 value encoding).
fn decode_header(v: &str) -> Option<String> {
    match v
        .strip_prefix("=?base64?")
        .and_then(|r| r.strip_suffix("?="))
    {
        Some(b) => base64::engine::general_purpose::STANDARD
            .decode(b)
            .ok()
            .and_then(|x| String::from_utf8(x).ok()),
        None => Some(v.to_string()),
    }
}

pub async fn endpoint(State(st): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let hub = &st.hub;
    if !hub.config().mcp.enabled {
        return StatusCode::NOT_FOUND.into_response();
    }
    if !origin_allowed(&st, &headers) {
        return rpc_error(
            StatusCode::FORBIDDEN,
            &Value::Null,
            INVALID_REQUEST,
            "origin not allowed",
            None,
        );
    }
    let token = header_str(&headers, "authorization")
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");
    let Some((user, device)) = (if token.is_empty() {
        None
    } else {
        hub.authenticate_or_load(token).await
    }) else {
        return (
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, "Bearer")],
            axum::Json(json!({"jsonrpc": "2.0", "error": {"code": INVALID_REQUEST, "message": "an agent key is required (Authorization: Bearer <key>)"}})),
        )
            .into_response();
    };
    if user.kind != UserKind::Agent {
        // What arrives here comes from a model and is labelled as AI; a
        // person's token must not be used to send it as that person.
        return rpc_error(
            StatusCode::FORBIDDEN,
            &Value::Null,
            INVALID_REQUEST,
            "MCP is for agent keys; people use the app",
            None,
        );
    }
    let req: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => {
            return rpc_error(
                StatusCode::BAD_REQUEST,
                &Value::Null,
                PARSE_ERROR,
                "invalid JSON",
                None,
            );
        }
    };
    let Some(obj) = req.as_object() else {
        return rpc_error(
            StatusCode::BAD_REQUEST,
            &Value::Null,
            INVALID_REQUEST,
            "one JSON-RPC request per POST (batches are not supported)",
            None,
        );
    };
    let id = obj.get("id").cloned().unwrap_or(Value::Null);
    let Some(method) = obj.get("method").and_then(Value::as_str) else {
        return rpc_error(
            StatusCode::BAD_REQUEST,
            &id,
            INVALID_REQUEST,
            "method is required",
            None,
        );
    };
    if obj.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return rpc_error(
            StatusCode::BAD_REQUEST,
            &id,
            INVALID_REQUEST,
            "jsonrpc must be \"2.0\"",
            None,
        );
    }
    let params = obj.get("params").cloned().unwrap_or_else(|| json!({}));
    if !params.is_object() {
        return rpc_error(
            StatusCode::BAD_REQUEST,
            &id,
            INVALID_PARAMS,
            "params must be an object",
            None,
        );
    }
    if !obj.contains_key("id") {
        // Notifications (e.g. notifications/initialized): nothing to do.
        return StatusCode::ACCEPTED.into_response();
    }
    if !(id.is_string() || id.is_i64() || id.is_u64()) {
        return rpc_error(
            StatusCode::BAD_REQUEST,
            &Value::Null,
            INVALID_REQUEST,
            "id must be a string or an integer",
            None,
        );
    }

    // ---------------------------------------------------- which revision
    let header_version = header_str(&headers, "mcp-protocol-version");
    let meta_version = params
        .get("_meta")
        .and_then(|m| m.get(META_VERSION))
        .and_then(Value::as_str);
    let modern = if method == "initialize" {
        false
    } else if let Some(v) = meta_version {
        if header_version != Some(v) {
            return rpc_error(
                StatusCode::BAD_REQUEST,
                &id,
                HEADER_MISMATCH,
                "MCP-Protocol-Version header must match _meta protocolVersion",
                None,
            );
        }
        if !MODERN.contains(&v) {
            return unsupported(&id, v);
        }
        if header_str(&headers, "mcp-method") != Some(method) {
            return rpc_error(
                StatusCode::BAD_REQUEST,
                &id,
                HEADER_MISMATCH,
                "Mcp-Method header must match the method",
                None,
            );
        }
        if matches!(method, "tools/call" | "resources/read" | "prompts/get") {
            let body_name = params
                .get("name")
                .or_else(|| params.get("uri"))
                .and_then(Value::as_str);
            let header_name = header_str(&headers, "mcp-name").and_then(decode_header);
            if body_name.is_none() || header_name.as_deref() != body_name {
                return rpc_error(
                    StatusCode::BAD_REQUEST,
                    &id,
                    HEADER_MISMATCH,
                    "Mcp-Name header must match the name",
                    None,
                );
            }
        }
        true
    } else {
        match header_version {
            // Before 2025-06-18 there was no header.
            None => false,
            Some(v) if LEGACY.contains(&v) => false,
            Some(v) if MODERN.contains(&v) => {
                return rpc_error(
                    StatusCode::BAD_REQUEST,
                    &id,
                    HEADER_MISMATCH,
                    "this revision needs _meta protocolVersion on every request",
                    None,
                );
            }
            Some(v) => return unsupported(&id, v),
        }
    };
    let complete = |mut r: Value| {
        if modern {
            r["resultType"] = json!("complete");
        }
        r
    };
    let server_info = json!({"name": "agentim", "version": env!("CARGO_PKG_VERSION")});

    match method {
        "initialize" => {
            let asked = params.get("protocolVersion").and_then(Value::as_str);
            let v = asked.filter(|v| LEGACY.contains(v)).unwrap_or(LEGACY[0]);
            rpc_result(
                &id,
                json!({
                    "protocolVersion": v,
                    "capabilities": {"tools": {}},
                    "serverInfo": server_info,
                    "instructions": INSTRUCTIONS,
                }),
            )
        }
        "server/discover" => rpc_result(
            &id,
            complete(json!({
                "supportedVersions": all_versions(),
                "capabilities": {"tools": {}},
                "_meta": {META_SERVER: server_info},
                "instructions": INSTRUCTIONS,
                "ttlMs": 3_600_000,
                "cacheScope": "public",
            })),
        ),
        "ping" => rpc_result(&id, complete(json!({}))),
        "tools/list" => {
            let mut r = json!({"tools": tools()});
            if modern {
                r["ttlMs"] = json!(300_000);
                r["cacheScope"] = json!("public");
            }
            rpc_result(&id, complete(r))
        }
        "tools/call" => {
            let Some(name) = params.get("name").and_then(Value::as_str) else {
                return rpc_error(
                    StatusCode::OK,
                    &id,
                    INVALID_PARAMS,
                    "name is required",
                    None,
                );
            };
            if !TOOLS.contains(&name) {
                return rpc_error(
                    StatusCode::OK,
                    &id,
                    INVALID_PARAMS,
                    &format!("unknown tool: {name}"),
                    None,
                );
            }
            let args = match params.get("arguments") {
                None | Some(Value::Null) => Map::new(),
                Some(Value::Object(m)) => m.clone(),
                Some(_) => {
                    return rpc_error(
                        StatusCode::OK,
                        &id,
                        INVALID_PARAMS,
                        "arguments must be an object",
                        None,
                    );
                }
            };
            let out = if hub.mcp_allowed(&device) {
                call_tool(&st, user, device, name, &args).await
            } else {
                Err(ApiError::new(
                    "rate_limited",
                    "too many tool calls; wait a moment",
                ))
            };
            rpc_result(&id, complete(tool_result(out)))
        }
        _ if modern => rpc_error(
            StatusCode::NOT_FOUND,
            &id,
            METHOD_NOT_FOUND,
            &format!("unknown method: {method}"),
            None,
        ),
        _ => rpc_error(
            StatusCode::OK,
            &id,
            METHOD_NOT_FOUND,
            &format!("unknown method: {method}"),
            None,
        ),
    }
}

fn unsupported(id: &Value, v: &str) -> Response {
    rpc_error(
        StatusCode::BAD_REQUEST,
        id,
        UNSUPPORTED_VERSION,
        "Unsupported protocol version",
        Some(json!({"supported": all_versions(), "requested": v})),
    )
}

/// A tool's outcome as a result: business errors are tool errors the model
/// can read and correct (`isError`), never protocol errors.
fn tool_result(out: ApiResult<(String, Value)>) -> Value {
    match out {
        Ok((text, data)) => json!({
            "content": [{"type": "text", "text": text}],
            "structuredContent": data,
            "isError": false,
        }),
        Err(e) => {
            let e = e.public();
            json!({
                "content": [{"type": "text", "text": format!("{}: {}", e.code, e.message)}],
                "structuredContent": {"error": {"code": e.code, "message": e.message}},
                "isError": true,
            })
        }
    }
}

// ------------------------------------------------------------------ tools

pub const TOOLS: &[&str] = &[
    "whoami",
    "list_conversations",
    "get_updates",
    "send_message",
    "edit_message",
    "recall_message",
    "find_user",
    "open_direct",
    "create_group",
    "add_member",
    "request_human",
    "get_request",
    "cancel_request",
    "create_task",
    "update_task",
    "claim",
    "release_claim",
    "floor_grant",
    "floor_release",
    "get_history",
    "get_context",
    "search",
    "put_summary",
    "state_get",
    "state_put",
    "state_list",
    "state_delete",
    "handoff",
    "send_card",
    "progress",
];

fn tools() -> Value {
    let id = |d: &str| json!({"type": "string", "minLength": 1, "description": d});
    let none = json!({"type": "object", "additionalProperties": false});
    let ro = json!({"readOnlyHint": true, "destructiveHint": false, "openWorldHint": false});
    json!([
        {
            "name": "whoami",
            "title": "Who am I",
            "description": "Your agent identity, your owner (the person responsible for you) and your limits.",
            "inputSchema": none,
            "annotations": ro,
        },
        {
            "name": "list_conversations",
            "title": "List conversations",
            "description": "Conversations you are a member of, with title, kind, members, last seq and unread count.",
            "inputSchema": none,
            "annotations": ro,
        },
        {
            "name": "get_updates",
            "title": "Get updates",
            "description": "What is new in your conversations since your last call, in order: messages, edits, recalls, membership changes, and private notes from your owner. Calling again confirms you received the previous answer, so the same events are not returned twice; if an answer was lost, they come again. If `more` is true, call again.",
            "inputSchema": {
                "type": "object",
                "properties": {"limit": {"type": "integer", "minimum": 1, "maximum": 500, "description": "Most events to return (default 100)."}, "ack": {"type": "string", "description": "The `ack` of the answer you are confirming (optional; without it, the last answer counts as received)."}},
                "additionalProperties": false,
            },
            "annotations": {"readOnlyHint": false, "destructiveHint": false, "idempotentHint": false, "openWorldHint": false},
        },
        {
            "name": "send_message",
            "title": "Send a message",
            "description": "Sends a message to a conversation you are a member of. Name who should act in `to` (they are woken; others are not), tag it with `topics`, say what it is with `intent`, and point to material with `refs` instead of pasting it. Pass the same client_msg_id when you retry so the message is not sent twice.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "conv_id": id("Conversation id (from list_conversations or get_updates)."),
                    "text": {"type": "string", "minLength": 1, "description": "The message."},
                    "reply_to": id("msg_id of the message you answer (optional)."),
                    "client_msg_id": {"type": "string", "minLength": 1, "maxLength": 128, "description": "Your id for this message; repeat it on retries (optional)."},
                    "to": {"type": "array", "items": {"type": "string", "minLength": 1}, "minItems": 1, "maxItems": 20, "description": "Members who should act (names or ids)."},
                    "topics": {"type": "array", "items": {"type": "string", "pattern": "^[a-z0-9._-]{1,64}$"}, "minItems": 1, "maxItems": 10},
                    "intent": {"type": "string", "enum": ["request", "proposal", "answer", "status", "done"]},
                    "refs": {"type": "array", "items": {"type": "string", "minLength": 1, "maxLength": 256}, "minItems": 1, "maxItems": 20, "description": "References to documents, artifacts or messages."},
                    "sources": {"type": "array", "minItems": 1, "maxItems": 20, "description": "Sources your answer is based on; people see them under the message.", "items": {"type": "object", "properties": {"title": {"type": "string", "minLength": 1, "maxLength": 200}, "url": {"type": "string", "format": "uri", "maxLength": 2048}, "excerpt": {"type": "string", "maxLength": 500}}, "required": ["title", "url"], "additionalProperties": false}},
                },
                "required": ["conv_id", "text"],
                "additionalProperties": false,
            },
            "annotations": {"readOnlyHint": false, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false},
        },
        {
            "name": "edit_message",
            "title": "Edit a message",
            "description": "Replaces the text of a message you sent.",
            "inputSchema": {
                "type": "object",
                "properties": {"msg_id": id("The message."), "text": {"type": "string", "minLength": 1}},
                "required": ["msg_id", "text"],
                "additionalProperties": false,
            },
            "annotations": {"readOnlyHint": false, "destructiveHint": true, "idempotentHint": true, "openWorldHint": false},
        },
        {
            "name": "recall_message",
            "title": "Recall a message",
            "description": "Withdraws a message you sent; its content is deleted for everyone.",
            "inputSchema": {
                "type": "object",
                "properties": {"msg_id": id("The message.")},
                "required": ["msg_id"],
                "additionalProperties": false,
            },
            "annotations": {"readOnlyHint": false, "destructiveHint": true, "idempotentHint": false, "openWorldHint": false},
        },
        {
            "name": "find_user",
            "title": "Find a user",
            "description": "Looks up a person or agent by exact user name.",
            "inputSchema": {
                "type": "object",
                "properties": {"name": id("Exact user name.")},
                "required": ["name"],
                "additionalProperties": false,
            },
            "annotations": ro,
        },
        {
            "name": "open_direct",
            "title": "Open a direct conversation",
            "description": "Opens (or returns the existing) one-to-one conversation with a person or agent: your owner, your owner's agents, or someone you already share a conversation with.",
            "inputSchema": {
                "type": "object",
                "properties": {"user": id("User name or id.")},
                "required": ["user"],
                "additionalProperties": false,
            },
            "annotations": {"readOnlyHint": false, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false},
        },
        {
            "name": "create_group",
            "title": "Create a group",
            "description": "Creates a group with people or agents you may contact (see open_direct). Your owner becomes its owner and you manage it as an admin.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "title": {"type": "string", "minLength": 1, "maxLength": 255},
                    "members": {"type": "array", "items": {"type": "string", "minLength": 1}, "maxItems": 500, "description": "User names or ids."},
                },
                "required": ["title", "members"],
                "additionalProperties": false,
            },
            "annotations": {"readOnlyHint": false, "destructiveHint": false, "idempotentHint": false, "openWorldHint": false},
        },
        {
            "name": "add_member",
            "title": "Add a member",
            "description": "Adds a person or agent you may contact to a group you manage.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "conv_id": id("The group."),
                    "user": id("User name or id."),
                    "history": {"type": "integer", "minimum": 0, "description": "How many recent messages the new member sees (optional)."},
                },
                "required": ["conv_id", "user"],
                "additionalProperties": false,
            },
            "annotations": {"readOnlyHint": false, "destructiveHint": false, "idempotentHint": false, "openWorldHint": false},
        },
        {
            "name": "request_human",
            "title": "Ask a person to decide",
            "description": "Asks your owner (or named members of the conversation) to approve or reject something, answer a question, or choose an option, before you act. Returns a request_id; call get_request with wait_secs to wait for the decision. Use it before anything irreversible or costly.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "conv_id": id("Conversation the request belongs to."),
                    "kind": {"type": "string", "enum": ["approve", "answer", "choose"]},
                    "title": {"type": "string", "minLength": 1, "maxLength": 4096, "description": "What is to be decided, in plain words."},
                    "options": {"type": "array", "items": {"type": "string", "minLength": 1, "maxLength": 200}, "minItems": 2, "maxItems": 20, "description": "For kind=choose."},
                    "context": {"description": "Details for the decider, e.g. the tool call arguments (JSON, at most 32KB)."},
                    "assignees": {"type": "array", "items": {"type": "string", "minLength": 1}, "minItems": 1, "maxItems": 20, "description": "People in the conversation who may decide (default: your owner)."},
                    "ttl_secs": {"type": "integer", "minimum": 60, "maximum": 2592000, "description": "How long the request stays open (default one day)."},
                    "client_req_id": {"type": "string", "minLength": 1, "maxLength": 128, "description": "Your id for this request; repeat it on retries."},
                },
                "required": ["conv_id", "kind", "title"],
                "additionalProperties": false,
            },
            "annotations": {"readOnlyHint": false, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false},
        },
        {
            "name": "get_request",
            "title": "Get a decision",
            "description": "The state and outcome of a request: pending, approved, rejected, answered, chosen, expired or cancelled. With wait_secs it waits up to that long for a decision.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "request_id": id("From request_human."),
                    "wait_secs": {"type": "integer", "minimum": 0, "maximum": 25},
                },
                "required": ["request_id"],
                "additionalProperties": false,
            },
            "annotations": ro,
        },
        {
            "name": "cancel_request",
            "title": "Cancel a request",
            "description": "Withdraws a request you made that is still pending.",
            "inputSchema": {
                "type": "object",
                "properties": {"request_id": id("From request_human.")},
                "required": ["request_id"],
                "additionalProperties": false,
            },
            "annotations": {"readOnlyHint": false, "destructiveHint": false, "idempotentHint": false, "openWorldHint": false},
        },
        {
            "name": "create_task",
            "title": "Create a task channel",
            "description": "Creates a task channel for one piece of work with people or agents you may contact; optionally names the member responsible (assignee), who is woken to work on it. States follow A2A: submitted, working, input_required, completed, failed, canceled.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "title": {"type": "string", "minLength": 1, "maxLength": 255},
                    "members": {"type": "array", "items": {"type": "string", "minLength": 1}, "maxItems": 500},
                    "assignee": id("Member responsible for the task (name or id)."),
                },
                "required": ["title", "members"],
                "additionalProperties": false,
            },
            "annotations": {"readOnlyHint": false, "destructiveHint": false, "idempotentHint": false, "openWorldHint": false},
        },
        {
            "name": "update_task",
            "title": "Update a task",
            "description": "Moves a task to a state (working, input_required, completed, failed, canceled) and/or reassigns it, with an optional note. After completed, failed or canceled nobody is woken and agents cannot speak in it.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "conv_id": id("The task channel."),
                    "state": {"type": "string", "enum": ["submitted", "working", "input_required", "completed", "failed", "canceled"]},
                    "assignee": id("New assignee (creator or manager only)."),
                    "note": {"type": "string", "minLength": 1, "maxLength": 4096},
                },
                "required": ["conv_id"],
                "additionalProperties": false,
            },
            "annotations": {"readOnlyHint": false, "destructiveHint": false, "idempotentHint": false, "openWorldHint": false},
        },
        {
            "name": "claim",
            "title": "Claim a request",
            "description": "Takes a message sent with intent=request for yourself. The first claim wins; others get a conflict. Release it if you cannot do it.",
            "inputSchema": {
                "type": "object",
                "properties": {"msg_id": id("The request message.")},
                "required": ["msg_id"],
                "additionalProperties": false,
            },
            "annotations": {"readOnlyHint": false, "destructiveHint": false, "idempotentHint": false, "openWorldHint": false},
        },
        {
            "name": "release_claim",
            "title": "Release a claim",
            "description": "Gives back a request you claimed so somebody else can take it.",
            "inputSchema": {
                "type": "object",
                "properties": {"msg_id": id("The request message.")},
                "required": ["msg_id"],
                "additionalProperties": false,
            },
            "annotations": {"readOnlyHint": false, "destructiveHint": false, "idempotentHint": false, "openWorldHint": false},
        },
        {
            "name": "floor_grant",
            "title": "Give the floor",
            "description": "As a manager of the conversation (e.g. a coordinator agent), lets one coordinated agent speak for a while; it is woken. Only one holder at a time.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "conv_id": id("The conversation."),
                    "agent": id("The agent (name or id)."),
                    "ttl_secs": {"type": "integer", "minimum": 10, "maximum": 3600},
                },
                "required": ["conv_id", "agent"],
                "additionalProperties": false,
            },
            "annotations": {"readOnlyHint": false, "destructiveHint": false, "idempotentHint": false, "openWorldHint": false},
        },
        {
            "name": "floor_release",
            "title": "Release the floor",
            "description": "Ends the current turn (the holder or a manager).",
            "inputSchema": {
                "type": "object",
                "properties": {"conv_id": id("The conversation.")},
                "required": ["conv_id"],
                "additionalProperties": false,
            },
            "annotations": {"readOnlyHint": false, "destructiveHint": false, "idempotentHint": false, "openWorldHint": false},
        },
        {
            "name": "get_history",
            "title": "Read history",
            "description": "Reads what you can see of a conversation by position: the latest messages by default, or before_seq / after_seq to page (use first_seq / last_seq of the previous answer). Up to 200 events.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "conv_id": id("The conversation."),
                    "after_seq": {"type": "integer", "minimum": 0},
                    "before_seq": {"type": "integer", "minimum": 1},
                    "limit": {"type": "integer", "minimum": 1, "maximum": 200},
                },
                "required": ["conv_id"],
                "additionalProperties": false,
            },
            "annotations": ro,
        },
        {
            "name": "get_context",
            "title": "Get context",
            "description": "What you need to continue in a conversation, within a token budget: your latest summary of it plus the newer events, oldest first. Use it after a restart or on a new machine.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "conv_id": id("The conversation."),
                    "max_tokens": {"type": "integer", "minimum": 256, "maximum": 200000},
                },
                "required": ["conv_id"],
                "additionalProperties": false,
            },
            "annotations": ro,
        },
        {
            "name": "search",
            "title": "Search messages",
            "description": "Finds messages you can see containing a text (case-insensitive), in one conversation or your 20 most recently active ones. `complete` is false when not everything was searched.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": {"type": "string", "minLength": 2, "maxLength": 200},
                    "conv_id": id("Only this conversation (optional)."),
                    "limit": {"type": "integer", "minimum": 1, "maximum": 50},
                    "sender_id": {"type": "string", "description": "Only messages from this user."},
                    "sender_type": {"type": "string", "enum": ["human", "agent"]},
                    "kind": {"type": "string", "description": "Only this message kind (text, card, a custom type)."},
                    "since": {"type": "integer", "description": "From this time (ms since 1970)."},
                    "until": {"type": "integer", "description": "Up to this time (ms since 1970)."},
                    "intent": {"type": "string", "maxLength": 64},
                    "topic": {"type": "string", "maxLength": 64},
                    "has_refs": {"type": "boolean", "description": "Only messages citing others (true) or not (false)."},
                },
                "required": ["query"],
                "additionalProperties": false,
            },
            "annotations": ro,
        },
        {
            "name": "put_summary",
            "title": "Write a summary",
            "description": "Stores your own summary of a range of a conversation (from_seq..to_seq, which you must be able to see). get_context then starts from it. Only you can read it.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "conv_id": id("The conversation."),
                    "from_seq": {"type": "integer", "minimum": 1},
                    "to_seq": {"type": "integer", "minimum": 1},
                    "text": {"type": "string", "minLength": 1, "maxLength": 65536},
                },
                "required": ["conv_id", "from_seq", "to_seq", "text"],
                "additionalProperties": false,
            },
            "annotations": {"readOnlyHint": false, "destructiveHint": false, "idempotentHint": false, "openWorldHint": false},
        },
        {
            "name": "state_get",
            "title": "Read a checkpoint",
            "description": "Reads a value you stored with state_put (the current version, or an older one by number). Keys under archive/ hold what was imported with you from another server.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "key": {"type": "string", "minLength": 1, "maxLength": 255},
                    "version": {"type": "integer", "minimum": 1},
                },
                "required": ["key"],
                "additionalProperties": false,
            },
            "annotations": ro,
        },
        {
            "name": "state_put",
            "title": "Store a checkpoint",
            "description": "Stores any JSON value (up to 1 MiB) under a key on the server, so you can carry on after a restart or on another machine. Pass the version you read (0 to create); if somebody wrote in between you get a conflict and should read again. Returns the new version.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "key": {"type": "string", "minLength": 1, "maxLength": 255},
                    "value": {},
                    "version": {"type": "integer", "minimum": 0},
                },
                "required": ["key", "value", "version"],
                "additionalProperties": false,
            },
            "annotations": {"readOnlyHint": false, "destructiveHint": false, "idempotentHint": false, "openWorldHint": false},
        },
        {
            "name": "state_list",
            "title": "List checkpoints",
            "description": "Your keys in byte order, optionally only those starting with a prefix; page with `after` (the last key you got).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "prefix": {"type": "string", "maxLength": 255},
                    "after": {"type": "string", "maxLength": 255},
                    "limit": {"type": "integer", "minimum": 1, "maximum": 500},
                },
                "additionalProperties": false,
            },
            "annotations": ro,
        },
        {
            "name": "state_delete",
            "title": "Delete a checkpoint",
            "description": "Deletes a key with all its versions; pass the current version.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "key": {"type": "string", "minLength": 1, "maxLength": 255},
                    "version": {"type": "integer", "minimum": 1},
                },
                "required": ["key", "version"],
                "additionalProperties": false,
            },
            "annotations": {"readOnlyHint": false, "destructiveHint": true, "idempotentHint": false, "openWorldHint": false},
        },
        {
            "name": "handoff",
            "title": "Hand off to a person",
            "description": "Hands this conversation to a person (default: your owner) with a note on what they need to know. You are silent and not woken there until they hand it back; then you get their note.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "conv_id": id("The conversation."),
                    "to": id("The person (name or id; default your owner)."),
                    "note": {"type": "string", "minLength": 1, "maxLength": 4096},
                },
                "required": ["conv_id", "note"],
                "additionalProperties": false,
            },
            "annotations": {"readOnlyHint": false, "destructiveHint": false, "idempotentHint": false, "openWorldHint": false},
        },
        {
            "name": "send_card",
            "title": "Send a card",
            "description": "Sends a card people answer by clicking a button or filling a form; their response comes back to you in get_updates. `card` is {title?, blocks: [...], once?}; a block is {type: text, text}, {type: buttons, id, options: [{id, label, style?: primary|danger}]} or {type: form, id, submit, fields: [{id, label, type: text|number|select|checkbox, required?, options?, max?}]}. Ids are a-z 0-9 _ -. `text` is what clients without cards show.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "conv_id": id("The conversation."),
                    "text": {"type": "string", "minLength": 1, "description": "Fallback text."},
                    "card": {"type": "object"},
                    "reply_to": id("The message this answers (optional)."),
                },
                "required": ["conv_id", "text", "card"],
                "additionalProperties": false,
            },
            "annotations": {"readOnlyHint": false, "destructiveHint": false, "idempotentHint": false, "openWorldHint": false},
        },
        {
            "name": "progress",
            "title": "Report progress",
            "description": "Shows people what you are doing (thinking, tool_call, tool_result, plan, step, error) next to the conversation without a message: nobody is woken and it is not part of the history. At most 10 a second.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "conv_id": id("The conversation."),
                    "kind": {"type": "string", "enum": ["thinking", "tool_call", "tool_result", "plan", "step", "error"]},
                    "text": {"type": "string", "minLength": 1, "maxLength": 4096},
                    "msg_id": id("Your message (e.g. a streamed reply) this belongs to."),
                    "data": {"description": "Any JSON, up to 8KB."},
                },
                "required": ["conv_id", "kind", "text"],
                "additionalProperties": false,
            },
            "annotations": {"readOnlyHint": false, "destructiveHint": false, "idempotentHint": false, "openWorldHint": false},
        },
    ])
}

fn arg_str(args: &Map<String, Value>, k: &str, required: bool) -> ApiResult<Option<String>> {
    match args.get(k) {
        None | Some(Value::Null) if !required => Ok(None),
        Some(Value::String(s)) if !s.is_empty() => Ok(Some(s.clone())),
        _ => Err(ApiError::new(
            "bad_request",
            format!("{k} must be a non-empty string"),
        )),
    }
}

fn need(args: &Map<String, Value>, k: &str) -> ApiResult<String> {
    arg_str(args, k, true).map(|v| v.unwrap_or_default())
}

/// An optional integer argument.
fn opt_int(args: &Map<String, Value>, k: &str) -> ApiResult<Option<i64>> {
    match args.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_i64()
            .map(Some)
            .ok_or_else(|| ApiError::new("bad_request", format!("{k} must be an integer"))),
    }
}

/// Refuses arguments a tool does not take, so typos are not ignored.
fn only(args: &Map<String, Value>, allowed: &[&str]) -> ApiResult<()> {
    match args.keys().find(|k| !allowed.contains(&k.as_str())) {
        Some(k) => Err(ApiError::new(
            "bad_request",
            format!("unknown argument {k}"),
        )),
        None => Ok(()),
    }
}

async fn call_tool(
    st: &AppState,
    user: User,
    device: String,
    name: &str,
    args: &Map<String, Value>,
) -> ApiResult<(String, Value)> {
    let hub = &st.hub;
    // One request of this key, as on a WebSocket of the same device.
    let (sess, _pushes) = hub.connect_device(user.clone(), Some(device.clone()));
    let run = |msg: Value| async {
        let msg: ClientMsg =
            serde_json::from_value(msg).map_err(|e| ApiError::new("bad_request", e.to_string()))?;
        hub.request(&sess, None, msg).await
    };
    match name {
        "whoami" => {
            only(args, &[])?;
            let owner = user
                .owner_id
                .as_deref()
                .and_then(|o| hub.user(o))
                .map(|o| json!({"id": o.id, "name": o.name}));
            let cfg = &hub.config().mcp;
            let v = json!({
                "agent": {"id": user.id, "name": user.name},
                "owner": owner,
                "limits": {"tool_calls_per_second": cfg.rate_per_second, "burst": cfg.burst},
                "server": crate::protocol::server_info(),
                "stopped": hub.is_stopped(&user.id),
            });
            let mut text = format!(
                "You are {} ({}), owned by {}.",
                user.name,
                user.id,
                owner
                    .as_ref()
                    .and_then(|o| o["name"].as_str())
                    .unwrap_or("nobody")
            );
            if hub.is_stopped(&user.id) {
                text.push_str(
                    " Your owner stopped you: every other tool fails until you are resumed.",
                );
            }
            Ok((text, v))
        }
        "list_conversations" => {
            only(args, &[])?;
            let v = run(json!({"op": "list_convs"})).await?;
            let convs = v["convs"].as_array().cloned().unwrap_or_default();
            let mut text = format!("{} conversation(s)", convs.len());
            for c in &convs {
                text.push_str(&format!(
                    "\n- {} \"{}\" ({})",
                    c["conv_id"].as_str().unwrap_or(""),
                    c["title"].as_str().unwrap_or(""),
                    c["kind"].as_str().unwrap_or("")
                ));
            }
            Ok((text, v))
        }
        "get_updates" => {
            only(args, &["limit", "ack"])?;
            let ack = arg_str(args, "ack", false)?;
            let limit = match args.get("limit") {
                None | Some(Value::Null) => 100,
                Some(v) => v
                    .as_i64()
                    .filter(|l| (1..=500).contains(l))
                    .ok_or_else(|| ApiError::new("bad_request", "limit must be 1..500"))?,
            };
            let _one_at_a_time = hub.device_lock(&device).await;
            // This call shows the previous answer arrived: acknowledge that
            // batch now, not when it was sent (D96), so a lost answer is
            // sent again.
            let prev = hub.take_mcp_pending(&device, ack.as_deref());
            if !prev.is_empty() {
                hub.store_acks(&sess, prev).await?;
            }
            let mut v = run(json!({"op": "sync", "limit": limit})).await?;
            let convs = v["convs"].as_array().cloned().unwrap_or_default();
            let acks: Vec<(String, i64, i64)> = convs
                .iter()
                .filter(|c| c.get("upto").is_some())
                .map(|c| {
                    (
                        c["conv_id"].as_str().unwrap_or_default().to_string(),
                        c["upto"].as_i64().unwrap_or(0),
                        c["side_upto"].as_i64().unwrap_or(0),
                    )
                })
                .collect();
            v["ack"] = json!(hub.put_mcp_pending(&device, acks));
            Ok((render_updates(&convs, v["more"] == true), v))
        }
        "send_message" => {
            only(
                args,
                &[
                    "conv_id",
                    "text",
                    "reply_to",
                    "client_msg_id",
                    "to",
                    "topics",
                    "intent",
                    "refs",
                    "sources",
                ],
            )?;
            let cmid = arg_str(args, "client_msg_id", false)?
                .unwrap_or_else(|| format!("mcp-{}", uuid::Uuid::new_v4().simple()));
            let mut m = json!({
                "op": "send",
                "conv_id": need(args, "conv_id")?,
                "text": need(args, "text")?,
                "client_msg_id": cmid,
            });
            if let Some(r) = arg_str(args, "reply_to", false)? {
                m["reply_to"] = json!(r);
            }
            if let Some(i) = arg_str(args, "intent", false)? {
                m["intent"] = json!(i);
            }
            for k in ["to", "topics", "refs"] {
                match args.get(k) {
                    None | Some(Value::Null) => {}
                    Some(Value::Array(a)) if a.iter().all(Value::is_string) => m[k] = json!(a),
                    _ => {
                        return Err(ApiError::new(
                            "bad_request",
                            format!("{k} must be a list of strings"),
                        ));
                    }
                }
            }
            match args.get("sources") {
                None | Some(Value::Null) => {}
                Some(Value::Array(a)) => m["sources"] = json!(a),
                _ => {
                    return Err(ApiError::new(
                        "bad_request",
                        "sources must be a list of {title, url, excerpt?}",
                    ));
                }
            }
            let v = run(m).await?;
            let text = match v["draft_id"].as_str() {
                Some(d) => format!("Waiting for your owner's approval (draft {d})."),
                None => format!(
                    "Sent as {} (seq {}).",
                    v["msg_id"].as_str().unwrap_or(""),
                    v["seq"]
                ),
            };
            Ok((text, v))
        }
        "edit_message" => {
            only(args, &["msg_id", "text"])?;
            let v = run(
                json!({"op": "edit", "msg_id": need(args, "msg_id")?, "text": need(args, "text")?}),
            )
            .await?;
            Ok(("Edited.".into(), v))
        }
        "recall_message" => {
            only(args, &["msg_id"])?;
            let v = run(json!({"op": "recall", "msg_id": need(args, "msg_id")?})).await?;
            Ok(("Recalled.".into(), v))
        }
        "find_user" => {
            only(args, &["name"])?;
            let v = run(json!({"op": "find_user", "name": need(args, "name")?})).await?;
            Ok((
                format!("Found {}.", v["user"]["id"].as_str().unwrap_or("")),
                v,
            ))
        }
        "open_direct" => {
            only(args, &["user"])?;
            let other = need(args, "user")?;
            let v = run(json!({"op": "create_conv", "kind": "dm", "title": format!("{} and {other}", user.name), "members": [other]})).await?;
            Ok((
                format!(
                    "Direct conversation {}.",
                    v["conv_id"].as_str().unwrap_or("")
                ),
                v,
            ))
        }
        "create_group" => {
            only(args, &["title", "members"])?;
            let members = match args.get("members") {
                Some(Value::Array(a))
                    if a.len() <= 500
                        && a.iter().all(|m| m.as_str().is_some_and(|x| !x.is_empty())) =>
                {
                    a.clone()
                }
                _ => {
                    return Err(ApiError::new(
                        "bad_request",
                        "members must be a list of up to 500 user names or ids",
                    ));
                }
            };
            let v = run(json!({"op": "create_conv", "kind": "group", "title": need(args, "title")?, "members": members})).await?;
            Ok((
                format!("Group {} created.", v["conv_id"].as_str().unwrap_or("")),
                v,
            ))
        }
        "add_member" => {
            only(args, &["conv_id", "user", "history"])?;
            let mut m = json!({"op": "add_member", "conv_id": need(args, "conv_id")?, "user_id": need(args, "user")?});
            match args.get("history") {
                None | Some(Value::Null) => {}
                Some(h) => {
                    m["history"] = json!(h.as_i64().ok_or_else(|| ApiError::new(
                        "bad_request",
                        "history must be an integer"
                    ))?);
                }
            }
            let v = run(m).await?;
            Ok(("Added.".into(), v))
        }
        "request_human" => {
            only(
                args,
                &[
                    "conv_id",
                    "kind",
                    "title",
                    "options",
                    "context",
                    "assignees",
                    "ttl_secs",
                    "client_req_id",
                ],
            )?;
            let cid = arg_str(args, "client_req_id", false)?
                .unwrap_or_else(|| format!("mcp-{}", uuid::Uuid::new_v4().simple()));
            let mut m = json!({
                "op": "request_human",
                "conv_id": need(args, "conv_id")?,
                "kind": need(args, "kind")?,
                "title": need(args, "title")?,
                "client_req_id": cid,
            });
            for k in ["options", "assignees"] {
                match args.get(k) {
                    None | Some(Value::Null) => {}
                    Some(Value::Array(a)) if a.iter().all(Value::is_string) => m[k] = json!(a),
                    _ => {
                        return Err(ApiError::new(
                            "bad_request",
                            format!("{k} must be a list of strings"),
                        ));
                    }
                }
            }
            if let Some(c) = args.get("context").filter(|c| !c.is_null()) {
                m["context"] = c.clone();
            }
            match args.get("ttl_secs") {
                None | Some(Value::Null) => {}
                Some(t) => {
                    m["ttl_secs"] = json!(t.as_i64().ok_or_else(|| ApiError::new(
                        "bad_request",
                        "ttl_secs must be an integer"
                    ))?)
                }
            }
            let v = run(m).await?;
            Ok((
                format!(
                    "Asked: request {} is {}. Call get_request with wait_secs to wait for the decision.",
                    v["request_id"].as_str().unwrap_or(""),
                    v["state"].as_str().unwrap_or("")
                ),
                v,
            ))
        }
        "get_request" => {
            only(args, &["request_id", "wait_secs"])?;
            let mut m = json!({"op": "get_request", "request_id": need(args, "request_id")?});
            match args.get("wait_secs") {
                None | Some(Value::Null) => {}
                Some(w) => {
                    m["wait_secs"] = json!(w.as_i64().ok_or_else(|| ApiError::new(
                        "bad_request",
                        "wait_secs must be an integer"
                    ))?)
                }
            }
            let v = run(m).await?;
            let state = v["state"].as_str().unwrap_or("");
            let text = match state {
                "pending" => "Still waiting for a decision.".to_string(),
                "approved" => "Approved.".to_string(),
                "rejected" => format!(
                    "Rejected{}.",
                    v["response"]["text"]
                        .as_str()
                        .map(|t| format!(": {t}"))
                        .unwrap_or_default()
                ),
                "answered" => format!("Answer: {}", v["response"]["text"].as_str().unwrap_or("")),
                "chosen" => format!("Chosen: {}", v["response"]["choice"].as_str().unwrap_or("")),
                s => format!("The request is {s}."),
            };
            Ok((text, v))
        }
        "cancel_request" => {
            only(args, &["request_id"])?;
            let v = run(json!({"op": "cancel_request", "request_id": need(args, "request_id")?}))
                .await?;
            Ok(("Cancelled.".into(), v))
        }
        "create_task" => {
            only(args, &["title", "members", "assignee"])?;
            let members = match args.get("members") {
                Some(Value::Array(a))
                    if a.len() <= 500
                        && a.iter().all(|m| m.as_str().is_some_and(|x| !x.is_empty())) =>
                {
                    a.clone()
                }
                _ => {
                    return Err(ApiError::new(
                        "bad_request",
                        "members must be a list of up to 500 user names or ids",
                    ));
                }
            };
            let mut m = json!({"op": "create_conv", "kind": "task", "title": need(args, "title")?, "members": members});
            if let Some(a) = arg_str(args, "assignee", false)? {
                m["assignee"] = json!(a);
            }
            let v = run(m).await?;
            Ok((
                format!("Task {} created.", v["conv_id"].as_str().unwrap_or("")),
                v,
            ))
        }
        "update_task" => {
            only(args, &["conv_id", "state", "assignee", "note"])?;
            let mut m = json!({"op": "update_task", "conv_id": need(args, "conv_id")?});
            for k in ["state", "assignee", "note"] {
                if let Some(x) = arg_str(args, k, false)? {
                    m[k] = json!(x);
                }
            }
            let v = run(m).await?;
            Ok((
                format!("The task is {}.", v["state"].as_str().unwrap_or("")),
                v,
            ))
        }
        "claim" => {
            only(args, &["msg_id"])?;
            let v = run(json!({"op": "claim", "msg_id": need(args, "msg_id")?})).await?;
            Ok(("Claimed: it is yours to do.".into(), v))
        }
        "release_claim" => {
            only(args, &["msg_id"])?;
            let v = run(json!({"op": "release_claim", "msg_id": need(args, "msg_id")?})).await?;
            Ok(("Released.".into(), v))
        }
        "floor_grant" => {
            only(args, &["conv_id", "agent", "ttl_secs"])?;
            let mut m = json!({"op": "floor_grant", "conv_id": need(args, "conv_id")?, "agent_id": need(args, "agent")?});
            match args.get("ttl_secs") {
                None | Some(Value::Null) => {}
                Some(t) => {
                    m["ttl_secs"] = json!(t.as_i64().ok_or_else(|| ApiError::new(
                        "bad_request",
                        "ttl_secs must be an integer"
                    ))?)
                }
            }
            let v = run(m).await?;
            Ok(("The floor is given.".into(), v))
        }
        "floor_release" => {
            only(args, &["conv_id"])?;
            let v = run(json!({"op": "floor_release", "conv_id": need(args, "conv_id")?})).await?;
            Ok(("The floor is free.".into(), v))
        }
        "get_history" => {
            only(args, &["conv_id", "after_seq", "before_seq", "limit"])?;
            let mut m = json!({"op": "history", "conv_id": need(args, "conv_id")?});
            for k in ["after_seq", "before_seq", "limit"] {
                if let Some(x) = opt_int(args, k)? {
                    m[k] = json!(x);
                }
            }
            let v = run(m).await?;
            Ok((render_read(&v), v))
        }
        "get_context" => {
            only(args, &["conv_id", "max_tokens"])?;
            let mut m = json!({"op": "context", "conv_id": need(args, "conv_id")?});
            if let Some(x) = opt_int(args, "max_tokens")? {
                m["max_tokens"] = json!(x);
            }
            let v = run(m).await?;
            Ok((render_read(&v), v))
        }
        "search" => {
            only(
                args,
                &[
                    "query",
                    "conv_id",
                    "limit",
                    "sender_id",
                    "sender_type",
                    "kind",
                    "since",
                    "until",
                    "intent",
                    "topic",
                    "has_refs",
                ],
            )?;
            let mut m = json!({"op": "search", "query": need(args, "query")?});
            for k in [
                "sender_id",
                "sender_type",
                "kind",
                "since",
                "until",
                "intent",
                "topic",
                "has_refs",
            ] {
                if let Some(v) = args.get(k) {
                    m[k] = v.clone();
                }
            }
            if let Some(c) = arg_str(args, "conv_id", false)? {
                m["conv_id"] = json!(c);
            }
            if let Some(x) = opt_int(args, "limit")? {
                m["limit"] = json!(x);
            }
            let v = run(m).await?;
            Ok((render_hits(&v), v))
        }
        "put_summary" => {
            only(args, &["conv_id", "from_seq", "to_seq", "text"])?;
            let (from, to) = (opt_int(args, "from_seq")?, opt_int(args, "to_seq")?);
            let (Some(from), Some(to)) = (from, to) else {
                return Err(ApiError::new(
                    "bad_request",
                    "from_seq and to_seq are required integers",
                ));
            };
            let v = run(json!({"op": "put_summary", "conv_id": need(args, "conv_id")?, "from_seq": from, "to_seq": to, "text": need(args, "text")?})).await?;
            Ok(("Summary stored; get_context starts from it.".into(), v))
        }
        "state_get" => {
            only(args, &["key", "version"])?;
            let mut m = json!({"op": "state_get", "key": need(args, "key")?});
            if let Some(v) = opt_int(args, "version")? {
                m["version"] = json!(v);
            }
            let v = run(m).await?;
            Ok((
                format!(
                    "{} (version {}): {}",
                    v["key"].as_str().unwrap_or(""),
                    v["version"],
                    v["value"]
                ),
                v,
            ))
        }
        "state_put" => {
            only(args, &["key", "value", "version"])?;
            let Some(value) = args.get("value") else {
                return Err(ApiError::new("bad_request", "value is required"));
            };
            let Some(version) = opt_int(args, "version")? else {
                return Err(ApiError::new(
                    "bad_request",
                    "version is required (0 creates)",
                ));
            };
            let v = run(json!({"op": "state_put", "key": need(args, "key")?, "value": value, "version": version})).await?;
            Ok((format!("Stored as version {}.", v["version"]), v))
        }
        "state_list" => {
            only(args, &["prefix", "after", "limit"])?;
            let mut m = json!({"op": "state_list"});
            for k in ["prefix", "after"] {
                match args.get(k) {
                    None | Some(Value::Null) => {}
                    Some(Value::String(x)) => m[k] = json!(x),
                    _ => {
                        return Err(ApiError::new(
                            "bad_request",
                            format!("{k} must be a string"),
                        ));
                    }
                }
            }
            if let Some(l) = opt_int(args, "limit")? {
                m["limit"] = json!(l);
            }
            let v = run(m).await?;
            let mut text: Vec<String> = v["keys"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|k| {
                    format!(
                        "{} (version {})",
                        k["key"].as_str().unwrap_or(""),
                        k["version"]
                    )
                })
                .collect();
            if text.is_empty() {
                text.push("No keys.".into());
            }
            if v["more"] == true {
                text.push("More: call again with after = the last key.".into());
            }
            Ok((text.join("\n"), v))
        }
        "state_delete" => {
            only(args, &["key", "version"])?;
            let Some(version) = opt_int(args, "version")? else {
                return Err(ApiError::new("bad_request", "version is required"));
            };
            let v =
                run(json!({"op": "state_delete", "key": need(args, "key")?, "version": version}))
                    .await?;
            Ok(("Deleted.".into(), v))
        }
        "handoff" => {
            only(args, &["conv_id", "to", "note"])?;
            let mut m = json!({"op": "handoff", "conv_id": need(args, "conv_id")?, "note": need(args, "note")?});
            if let Some(t) = arg_str(args, "to", false)? {
                m["to"] = json!(t);
            }
            let v = run(m).await?;
            Ok(("Handed off: wait until it is handed back.".into(), v))
        }
        "send_card" => {
            only(args, &["conv_id", "text", "card", "reply_to"])?;
            let card = args
                .get("card")
                .filter(|c| c.is_object())
                .ok_or_else(|| ApiError::new("bad_request", "card must be an object"))?;
            let mut m = json!({"op": "send", "conv_id": need(args, "conv_id")?, "text": need(args, "text")?, "kind": "agentim.card", "payload": card, "client_msg_id": format!("mcp-{}", uuid::Uuid::new_v4().simple())});
            if let Some(r) = arg_str(args, "reply_to", false)? {
                m["reply_to"] = json!(r);
            }
            let v = run(m).await?;
            let text = match v["draft_id"].as_str() {
                Some(d) => format!("Waiting for your owner's approval (draft {d})."),
                None => format!("Card sent as {}.", v["msg_id"].as_str().unwrap_or("")),
            };
            Ok((text, v))
        }
        "progress" => {
            only(args, &["conv_id", "kind", "text", "msg_id", "data"])?;
            let mut m = json!({"op": "progress", "conv_id": need(args, "conv_id")?, "kind": need(args, "kind")?, "text": need(args, "text")?});
            if let Some(x) = arg_str(args, "msg_id", false)? {
                m["msg_id"] = json!(x);
            }
            if let Some(d) = args.get("data").filter(|d| !d.is_null()) {
                m["data"] = d.clone();
            }
            let v = run(m).await?;
            Ok(("Shown.".into(), v))
        }
        _ => Err(ApiError::new("bad_request", "unknown tool")),
    }
}

/// The fields of a non-message event a model needs (task state, who holds
/// a claim or the floor), as ` key=value` pairs.
fn event_detail(data: &Value) -> String {
    let mut out = String::new();
    for k in [
        "state", "assignee", "msg_id", "agent_id", "by", "note", "block_id", "action",
    ] {
        if let Some(v) = data.get(k).and_then(Value::as_str) {
            out.push_str(&format!(" {k}={v}"));
        }
    }
    if let Some(v) = data.get("values").filter(|v| v.is_object()) {
        out.push_str(&format!(" values={v}"));
    }
    // Who joined: one (member.joined) or several at once (members.joined).
    if let Some(n) = data.get("name").and_then(Value::as_str) {
        out.push_str(&format!(" name={n}"));
    }
    if let Some(ms) = data.get("members").and_then(Value::as_array) {
        let names: Vec<&str> = ms.iter().filter_map(|m| m["name"].as_str()).collect();
        let shown = names
            .iter()
            .take(20)
            .copied()
            .collect::<Vec<_>>()
            .join(", ");
        let more = names.len().saturating_sub(20);
        out.push_str(&format!(" members={shown}"));
        if more > 0 {
            out.push_str(&format!(" (+{more} more)"));
        }
    }
    out
}

/// One event as a line a model reads easily.
fn render_event(out: &mut String, conv: &str, e: &Value) {
    let ev = &e["event"];
    let seq = ev["seq"].as_i64().unwrap_or(0);
    let kind = ev["type"].as_str().unwrap_or("");
    // What asks you to act is marked; the rest is context.
    let act = if e["wake"] == true {
        "→ for you "
    } else {
        ""
    };
    match ev["msg"].as_object() {
        Some(m) if kind == "msg.created" => {
            let trust = e["trust"]
                .as_str()
                .map(|t| format!(", trust {t}"))
                .unwrap_or_default();
            out.push_str(&format!(
                "{act}[{conv} #{seq}] {} ({}{trust}) [{}]: {}\n",
                m["sender_name"].as_str().unwrap_or(""),
                m["sender_type"].as_str().unwrap_or(""),
                m["msg_id"].as_str().unwrap_or(""),
                m["content"].as_str().unwrap_or("")
            ));
        }
        Some(m) => out.push_str(&format!(
            "[{conv} #{seq}] {kind} {} [{}]: {}\n",
            m["sender_name"].as_str().unwrap_or(""),
            m["msg_id"].as_str().unwrap_or(""),
            m["content"].as_str().unwrap_or("")
        )),
        None => out.push_str(&format!(
            "{act}[{conv} #{seq}] {kind}{}\n",
            event_detail(&ev["data"])
        )),
    }
}

/// A read of history or context as lines; the events are in
/// `structuredContent`.
fn render_read(v: &Value) -> String {
    let conv = v["conv_id"].as_str().unwrap_or("");
    let mut out = String::new();
    if let Some(s) = v["summary"].as_object() {
        out.push_str(&format!(
            "[{conv} summary of #{}..#{}, yours, derived]: {}\n",
            s["from_seq"],
            s["to_seq"],
            s["text"].as_str().unwrap_or("")
        ));
    }
    for e in v["events"].as_array().into_iter().flatten() {
        render_event(&mut out, conv, e);
    }
    if out.is_empty() {
        out.push_str("Nothing you can see here.\n");
    }
    if v["has_more"] == true {
        out.push_str(&format!(
            "More: call again with before_seq={} (older) or after_seq={} (newer).\n",
            v["first_seq"], v["last_seq"]
        ));
    }
    if v["truncated"] == true {
        out.push_str("Older events did not fit max_tokens; write a summary (put_summary) or read them with get_history.\n");
    }
    out.trim_end().to_string()
}

/// Search hits as lines.
fn render_hits(v: &Value) -> String {
    let mut out = String::new();
    for h in v["hits"].as_array().into_iter().flatten() {
        out.push_str(&format!(
            "[{} #{}] {} ({}) [{}]: {}\n",
            h["conv_id"].as_str().unwrap_or(""),
            h["seq"],
            h["sender_name"].as_str().unwrap_or(""),
            h["sender_type"].as_str().unwrap_or(""),
            h["msg_id"].as_str().unwrap_or(""),
            h["excerpt"].as_str().unwrap_or("")
        ));
    }
    if out.is_empty() {
        out.push_str("No match.\n");
    }
    if v["complete"] != true {
        out.push_str("Not everything was searched: narrow it with conv_id.\n");
    }
    out.trim_end().to_string()
}

/// Updates as lines a model reads easily; the full events are in
/// `structuredContent`.
fn render_updates(convs: &[Value], more: bool) -> String {
    let mut out = String::new();
    let mut n = 0;
    for c in convs {
        let conv = c["conv_id"].as_str().unwrap_or("");
        if c["dissolved"] == true {
            out.push_str(&format!("[{conv}] the conversation was dissolved\n"));
            continue;
        }
        if c["left"] == true {
            out.push_str(&format!("[{conv}] you are no longer a member\n"));
        }
        for e in c["events"].as_array().into_iter().flatten() {
            n += 1;
            render_event(&mut out, conv, e);
        }
        for s in c["side_events"].as_array().into_iter().flatten() {
            n += 1;
            let ev = s.get("event").unwrap_or(s);
            out.push_str(&format!(
                "[{conv} private] {}: {}\n",
                ev["type"].as_str().unwrap_or(""),
                ev["msg"]["content"]
                    .as_str()
                    .or_else(|| ev["data"]["text"].as_str())
                    .unwrap_or("")
            ));
        }
    }
    if n == 0 && out.is_empty() {
        out.push_str("Nothing new.");
    }
    if more {
        out.push_str("More updates are waiting: call get_updates again.");
    }
    out.trim_end().to_string()
}

#[cfg(test)]
mod error_tests {
    use crate::model::ApiError;

    #[test]
    fn internal_failures_are_not_explained_and_others_are() {
        let r = super::tool_result(Err(ApiError::new(
            "storage",
            "UNIQUE constraint failed: messages.client_msg_id",
        )));
        assert!(!r.to_string().contains("UNIQUE"), "{r}");
        assert_eq!(r["structuredContent"]["error"]["code"], "storage");
        assert_eq!(r["isError"], true);
        let r = super::tool_result(Err(ApiError::new("forbidden", "not a member")));
        assert_eq!(r["structuredContent"]["error"]["message"], "not a member");
    }
}
