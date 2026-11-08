//! Model providers (docs/en/design/assistants.md): three wire protocols cover them all.
//!
//! | provider | protocol |
//! |---|---|
//! | Claude (Anthropic) | Messages API, `x-api-key` + `anthropic-version` |
//! | OpenAI (Codex models too) | Responses API, stateless (`store: false`) |
//! | DeepSeek, Gemini, other compatible | Chat Completions |
//!
//! Every call streams (SSE); text arrives through `on_text` as it is
//! generated, and the turn ends with the tool calls, the provider-specific
//! content to send back next round (`raw`), the token usage and why it
//! stopped. Requests go straight to the provider with `reqwest`.

use std::time::Duration;

use serde_json::{Value, json};

use super::prices::Usage;

/// The Messages API version sent to Anthropic.
pub const ANTHROPIC_VERSION: &str = "2026-10-21";
/// How long one model call may take, start to end.
pub const CALL_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Anthropic,
    Openai,
    Gemini,
    Deepseek,
    /// Any other Chat Completions service, at its own address.
    Compatible,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Protocol {
    Messages,
    Responses,
    Chat,
}

impl Kind {
    pub fn parse(s: &str) -> Option<Kind> {
        Some(match s {
            "anthropic" => Kind::Anthropic,
            "openai" => Kind::Openai,
            "gemini" => Kind::Gemini,
            "deepseek" => Kind::Deepseek,
            "compatible" => Kind::Compatible,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Anthropic => "anthropic",
            Kind::Openai => "openai",
            Kind::Gemini => "gemini",
            Kind::Deepseek => "deepseek",
            Kind::Compatible => "compatible",
        }
    }

    /// Where `/models` lives; `None`: the service must give its address.
    pub fn default_base(self) -> Option<&'static str> {
        match self {
            Kind::Anthropic => Some("https://api.anthropic.com/v1"),
            Kind::Openai => Some("https://api.openai.com/v1"),
            Kind::Gemini => Some("https://generativelanguage.googleapis.com/v1beta/openai"),
            Kind::Deepseek => Some("https://api.deepseek.com"),
            Kind::Compatible => None,
        }
    }

    fn protocol(self) -> Protocol {
        match self {
            Kind::Anthropic => Protocol::Messages,
            Kind::Openai => Protocol::Responses,
            Kind::Gemini | Kind::Deepseek | Kind::Compatible => Protocol::Chat,
        }
    }
}

/// A provider to call: its kind, address and key.
#[derive(Clone)]
pub struct Endpoint {
    pub kind: Kind,
    pub base: String,
    pub key: String,
    pub http: reqwest::Client,
}

impl std::fmt::Debug for Endpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Endpoint")
            .field("kind", &self.kind)
            .field("base", &self.base)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ModelInfo {
    pub id: String,
    pub name: String,
    /// Input context window, when the provider says.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    /// JSON Schema of the arguments (an object).
    pub schema: Value,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub input: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolResult {
    pub call_id: String,
    pub content: String,
    pub is_error: bool,
}

/// One entry of the conversation sent to the model.
#[derive(Debug, Clone, PartialEq)]
pub enum Item {
    User(String),
    /// A previous turn of the model: `raw` is what the provider returned
    /// (Anthropic content blocks, Responses output items), sent back as is
    /// so reasoning carries over; `Null` for Chat Completions.
    Assistant {
        text: String,
        calls: Vec<ToolCall>,
        raw: Value,
    },
    Results(Vec<ToolResult>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    pub model: String,
    pub system: String,
    pub items: Vec<Item>,
    pub tools: Vec<ToolDef>,
    pub max_tokens: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Stop {
    End,
    Tools,
    MaxTokens,
    /// The provider declined (Claude's `refusal`, a content filter).
    Refusal,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Turn {
    pub text: String,
    pub calls: Vec<ToolCall>,
    pub raw: Value,
    pub usage: Usage,
    pub stop: Stop,
}

impl Turn {
    pub fn item(&self) -> Item {
        Item::Assistant {
            text: self.text.clone(),
            calls: self.calls.clone(),
            raw: self.raw.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AiError {
    /// The key was refused (401, 403).
    Auth(String),
    RateLimited(String),
    /// Down, overloaded, timed out, unreachable.
    Unavailable(String),
    /// The provider refused the request (400, 404: unknown model, ...).
    Bad(String),
}

impl std::fmt::Display for AiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AiError::Auth(m) => write!(f, "the provider refused the key: {m}"),
            AiError::RateLimited(m) => write!(f, "the provider is rate limiting: {m}"),
            AiError::Unavailable(m) => write!(f, "the provider is unavailable: {m}"),
            AiError::Bad(m) => write!(f, "the provider refused the request: {m}"),
        }
    }
}

impl Endpoint {
    fn url(&self, path: &str) -> String {
        format!("{}/{path}", self.base.trim_end_matches('/'))
    }

    fn authed(&self, b: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        let b = b.header("user-agent", concat!("agentim/", env!("CARGO_PKG_VERSION")));
        match self.kind {
            Kind::Anthropic => b
                .header("x-api-key", &self.key)
                .header("anthropic-version", ANTHROPIC_VERSION),
            Kind::Openai | Kind::Gemini | Kind::Deepseek | Kind::Compatible => {
                b.bearer_auth(&self.key)
            }
        }
    }

    /// The models this key can use, from the provider itself; chat models
    /// only (no embeddings, images, audio).
    pub async fn models(&self) -> Result<Vec<ModelInfo>, AiError> {
        let path = match self.kind {
            Kind::Anthropic => "models?limit=1000",
            Kind::Openai | Kind::Gemini | Kind::Deepseek | Kind::Compatible => "models",
        };
        let res = self
            .authed(self.http.get(self.url(path)))
            .timeout(Duration::from_secs(20))
            .send()
            .await
            .map_err(net)?;
        let v = json_of(res).await?;
        let mut out = vec![];
        for m in v["data"].as_array().into_iter().flatten() {
            let Some(id) = m["id"].as_str() else { continue };
            let id = id.strip_prefix("models/").unwrap_or(id).to_string();
            if !chat_model(self.kind, &id) {
                continue;
            }
            let name = m["display_name"].as_str().unwrap_or(&id).to_string();
            out.push(ModelInfo {
                context: m["max_input_tokens"].as_u64(),
                id,
                name,
            });
        }
        out.sort_by(|a, b| a.id.cmp(&b.id));
        out.dedup_by(|a, b| a.id == b.id);
        Ok(out)
    }

    /// One model turn, streamed.
    pub async fn turn(
        &self,
        req: &Request,
        on_text: &mut (dyn FnMut(&str) + Send),
    ) -> Result<Turn, AiError> {
        let (path, body) = match self.kind.protocol() {
            Protocol::Messages => ("messages", messages_body(req)),
            Protocol::Responses => ("responses", responses_body(req)),
            Protocol::Chat => ("chat/completions", chat_body(req)),
        };
        let work = async {
            let res = self
                .authed(self.http.post(self.url(path)))
                .header("content-type", "application/json")
                .body(body.to_string())
                .send()
                .await
                .map_err(net)?;
            if !res.status().is_success() {
                return Err(json_of(res)
                    .await
                    .err()
                    .unwrap_or_else(|| AiError::Unavailable("unexpected answer".into())));
            }
            let mut sse = Sse::new(res);
            match self.kind.protocol() {
                Protocol::Messages => read_messages(&mut sse, on_text).await,
                Protocol::Responses => read_responses(&mut sse, on_text).await,
                Protocol::Chat => read_chat(&mut sse, on_text).await,
            }
        };
        tokio::time::timeout(CALL_TIMEOUT, work)
            .await
            .map_err(|_| AiError::Unavailable("the call took too long".into()))?
    }
}

fn chat_model(kind: Kind, id: &str) -> bool {
    const NOT_CHAT: &[&str] = &[
        "embedding",
        "embed",
        "whisper",
        "tts",
        "dall-e",
        "image",
        "moderation",
        "realtime",
        "audio",
        "transcribe",
        "search",
        "aqa",
        "imagen",
        "veo",
        "live",
    ];
    if NOT_CHAT.iter().any(|w| id.contains(w)) {
        return false;
    }
    match kind {
        Kind::Openai => {
            id.starts_with("gpt-")
                || id.starts_with("chatgpt-")
                || id.contains("codex")
                || (id.starts_with('o') && id[1..].starts_with(|c: char| c.is_ascii_digit()))
        }
        Kind::Gemini => id.starts_with("gemini-"),
        Kind::Anthropic => id.starts_with("claude-"),
        Kind::Deepseek | Kind::Compatible => true,
    }
}

fn net(e: reqwest::Error) -> AiError {
    AiError::Unavailable(if e.is_timeout() {
        "timed out".into()
    } else {
        "could not reach the provider".into()
    })
}

/// The body of a successful answer, or the provider's error as `AiError`.
async fn json_of(res: reqwest::Response) -> Result<Value, AiError> {
    let status = res.status().as_u16();
    let text = res.text().await.map_err(net)?;
    let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    if (200..300).contains(&status) {
        return Ok(v);
    }
    let msg = v["error"]["message"]
        .as_str()
        .or(v["message"].as_str())
        .or(v["error"].as_str())
        .unwrap_or("no details")
        .chars()
        .take(300)
        .collect::<String>();
    Err(match status {
        401 | 403 => AiError::Auth(msg),
        429 => AiError::RateLimited(msg),
        400..=499 => AiError::Bad(msg),
        _ => AiError::Unavailable(format!("status {status}: {msg}")),
    })
}

/// Server-sent events: the `data:` of each event, `[DONE]` ends it.
struct Sse {
    res: reqwest::Response,
    buf: Vec<u8>,
    done: bool,
}

impl Sse {
    fn new(res: reqwest::Response) -> Sse {
        Sse {
            res,
            buf: vec![],
            done: false,
        }
    }

    async fn next(&mut self) -> Result<Option<Value>, AiError> {
        loop {
            if let Some(end) = find_event_end(&self.buf) {
                let raw: Vec<u8> = self.buf.drain(..end.0).collect();
                self.buf.drain(..end.1);
                let text = String::from_utf8_lossy(&raw);
                let mut data = String::new();
                for line in text.lines() {
                    if let Some(d) = line.strip_prefix("data:") {
                        if !data.is_empty() {
                            data.push('\n');
                        }
                        data.push_str(d.strip_prefix(' ').unwrap_or(d));
                    }
                }
                if data.is_empty() {
                    continue;
                }
                if data == "[DONE]" {
                    return Ok(None);
                }
                return serde_json::from_str(&data)
                    .map(Some)
                    .map_err(|_| AiError::Unavailable("a streamed event was not JSON".into()));
            }
            if self.done {
                return Ok(None);
            }
            match self.res.chunk().await.map_err(net)? {
                Some(c) => self.buf.extend_from_slice(&c),
                None => {
                    self.done = true;
                    if !self.buf.is_empty() {
                        self.buf.extend_from_slice(b"\n\n");
                    }
                }
            }
        }
    }
}

/// (end of the event, length of the separator) of the first complete event.
fn find_event_end(b: &[u8]) -> Option<(usize, usize)> {
    for i in 0..b.len() {
        if b[i..].starts_with(b"\n\n") {
            return Some((i, 2));
        }
        if b[i..].starts_with(b"\r\n\r\n") {
            return Some((i, 4));
        }
    }
    None
}

fn stream_error(v: &Value) -> AiError {
    let e = &v["error"];
    let msg = e["message"].as_str().unwrap_or("error").to_string();
    match e["type"].as_str().or(e["code"].as_str()).unwrap_or("") {
        "rate_limit_error" | "rate_limit_exceeded" => AiError::RateLimited(msg),
        "authentication_error" | "permission_error" => AiError::Auth(msg),
        "invalid_request_error" => AiError::Bad(msg),
        _ => AiError::Unavailable(msg),
    }
}

// --- Anthropic Messages ---------------------------------------------------

fn messages_body(req: &Request) -> Value {
    let mut messages = vec![];
    for it in &req.items {
        match it {
            Item::User(t) => messages.push(json!({"role": "user", "content": t})),
            Item::Assistant { text, calls, raw } => {
                let content = if raw.is_array() {
                    raw.clone()
                } else {
                    let mut c = vec![];
                    if !text.is_empty() {
                        c.push(json!({"type": "text", "text": text}));
                    }
                    for k in calls {
                        c.push(json!({"type": "tool_use", "id": k.id, "name": k.name, "input": k.input}));
                    }
                    Value::Array(c)
                };
                messages.push(json!({"role": "assistant", "content": content}));
            }
            Item::Results(rs) => {
                let c: Vec<Value> = rs
                    .iter()
                    .map(|r| json!({"type": "tool_result", "tool_use_id": r.call_id, "content": r.content, "is_error": r.is_error}))
                    .collect();
                messages.push(json!({"role": "user", "content": c}));
            }
        }
    }
    let mut body = json!({
        "model": req.model,
        "max_tokens": req.max_tokens,
        "stream": true,
        "messages": messages,
    });
    // The instructions (and knowledge) are the stable prefix: cached.
    if !req.system.is_empty() {
        body["system"] =
            json!([{"type": "text", "text": req.system, "cache_control": {"type": "ephemeral"}}]);
    }
    if !req.tools.is_empty() {
        body["tools"] = Value::Array(
            req.tools
                .iter()
                .map(|t| json!({"name": t.name, "description": t.description, "input_schema": t.schema}))
                .collect(),
        );
    }
    body
}

async fn read_messages(
    sse: &mut Sse,
    on_text: &mut (dyn FnMut(&str) + Send),
) -> Result<Turn, AiError> {
    let mut blocks: Vec<Value> = vec![];
    let mut partial: Vec<String> = vec![];
    let mut usage = Usage::default();
    let mut stop = None;
    while let Some(v) = sse.next().await? {
        match v["type"].as_str().unwrap_or("") {
            "message_start" => {
                let u = &v["message"]["usage"];
                usage.input = u["input_tokens"].as_u64().unwrap_or(0);
                usage.cache_read = u["cache_read_input_tokens"].as_u64().unwrap_or(0);
                usage.cache_write = u["cache_creation_input_tokens"].as_u64().unwrap_or(0);
                usage.output = u["output_tokens"].as_u64().unwrap_or(0);
            }
            "content_block_start" => {
                let i = v["index"].as_u64().unwrap_or(blocks.len() as u64) as usize;
                while blocks.len() <= i {
                    blocks.push(Value::Null);
                    partial.push(String::new());
                }
                blocks[i] = v["content_block"].clone();
            }
            "content_block_delta" => {
                let i = v["index"].as_u64().unwrap_or(0) as usize;
                let Some(b) = blocks.get_mut(i) else { continue };
                let d = &v["delta"];
                match d["type"].as_str().unwrap_or("") {
                    "text_delta" => {
                        let t = d["text"].as_str().unwrap_or("");
                        on_text(t);
                        let old = b["text"].as_str().unwrap_or("").to_string();
                        b["text"] = json!(old + t);
                    }
                    "thinking_delta" => {
                        let old = b["thinking"].as_str().unwrap_or("").to_string();
                        b["thinking"] = json!(old + d["thinking"].as_str().unwrap_or(""));
                    }
                    "signature_delta" => b["signature"] = d["signature"].clone(),
                    "input_json_delta" => {
                        partial[i].push_str(d["partial_json"].as_str().unwrap_or(""))
                    }
                    _ => {}
                }
            }
            "content_block_stop" => {
                let i = v["index"].as_u64().unwrap_or(0) as usize;
                if let Some(b) = blocks.get_mut(i)
                    && b["type"] == "tool_use"
                {
                    let p = &partial[i];
                    b["input"] = if p.trim().is_empty() {
                        json!({})
                    } else {
                        serde_json::from_str(p).unwrap_or_else(|_| json!({}))
                    };
                }
            }
            "message_delta" => {
                if let Some(o) = v["usage"]["output_tokens"].as_u64() {
                    usage.output = usage.output.max(o);
                }
                if let Some(i) = v["usage"]["input_tokens"].as_u64() {
                    usage.input = usage.input.max(i);
                }
                if let Some(s) = v["delta"]["stop_reason"].as_str() {
                    stop = Some(s.to_string());
                }
            }
            "error" => return Err(stream_error(&v)),
            _ => {}
        }
    }
    blocks.retain(|b| !b.is_null());
    let text: String = blocks
        .iter()
        .filter(|b| b["type"] == "text")
        .filter_map(|b| b["text"].as_str())
        .collect();
    let calls: Vec<ToolCall> = blocks
        .iter()
        .filter(|b| b["type"] == "tool_use")
        .map(|b| ToolCall {
            id: b["id"].as_str().unwrap_or("").into(),
            name: b["name"].as_str().unwrap_or("").into(),
            input: b["input"].clone(),
        })
        .collect();
    let stop = match stop.as_deref() {
        Some("tool_use") => Stop::Tools,
        Some("max_tokens") => Stop::MaxTokens,
        Some("refusal") => Stop::Refusal,
        Some(_) => Stop::End,
        None => return Err(AiError::Unavailable("the stream ended early".into())),
    };
    Ok(Turn {
        text,
        calls,
        raw: Value::Array(blocks),
        usage,
        stop,
    })
}

// --- OpenAI Responses -----------------------------------------------------

fn responses_body(req: &Request) -> Value {
    let mut input = vec![];
    for it in &req.items {
        match it {
            Item::User(t) => input.push(json!({"role": "user", "content": t})),
            Item::Assistant { text, calls, raw } => {
                if let Some(items) = raw.as_array() {
                    input.extend(items.iter().cloned());
                } else {
                    if !text.is_empty() {
                        input.push(json!({"role": "assistant", "content": text}));
                    }
                    for k in calls {
                        input.push(json!({"type": "function_call", "call_id": k.id, "name": k.name, "arguments": k.input.to_string()}));
                    }
                }
            }
            Item::Results(rs) => {
                for r in rs {
                    input.push(json!({"type": "function_call_output", "call_id": r.call_id, "output": r.content}));
                }
            }
        }
    }
    let mut body = json!({
        "model": req.model,
        "input": input,
        "stream": true,
        // Nothing kept at OpenAI; reasoning comes back encrypted for the
        // next round instead.
        "store": false,
        "include": ["reasoning.encrypted_content"],
        "max_output_tokens": req.max_tokens,
    });
    if !req.system.is_empty() {
        body["instructions"] = json!(req.system);
    }
    if !req.tools.is_empty() {
        body["tools"] = Value::Array(
            req.tools
                .iter()
                .map(|t| json!({"type": "function", "name": t.name, "description": t.description, "parameters": t.schema}))
                .collect(),
        );
    }
    body
}

async fn read_responses(
    sse: &mut Sse,
    on_text: &mut (dyn FnMut(&str) + Send),
) -> Result<Turn, AiError> {
    let mut text = String::new();
    while let Some(v) = sse.next().await? {
        match v["type"].as_str().unwrap_or("") {
            "response.output_text.delta" => {
                let d = v["delta"].as_str().unwrap_or("");
                on_text(d);
                text.push_str(d);
            }
            "response.completed" | "response.incomplete" => {
                let r = &v["response"];
                let output = r["output"].clone();
                let mut calls = vec![];
                let mut refused = false;
                for item in output.as_array().into_iter().flatten() {
                    match item["type"].as_str().unwrap_or("") {
                        "function_call" => calls.push(ToolCall {
                            id: item["call_id"].as_str().unwrap_or("").into(),
                            name: item["name"].as_str().unwrap_or("").into(),
                            input: item["arguments"]
                                .as_str()
                                .and_then(|a| serde_json::from_str(a).ok())
                                .unwrap_or_else(|| json!({})),
                        }),
                        "message" => {
                            refused |= item["content"]
                                .as_array()
                                .is_some_and(|c| c.iter().any(|p| p["type"] == "refusal"));
                        }
                        _ => {}
                    }
                }
                let u = &r["usage"];
                let cached = u["input_tokens_details"]["cached_tokens"]
                    .as_u64()
                    .unwrap_or(0);
                let usage = Usage {
                    input: u["input_tokens"]
                        .as_u64()
                        .unwrap_or(0)
                        .saturating_sub(cached),
                    output: u["output_tokens"].as_u64().unwrap_or(0),
                    cache_read: cached,
                    cache_write: 0,
                };
                let stop = if !calls.is_empty() {
                    Stop::Tools
                } else if refused {
                    Stop::Refusal
                } else if r["status"] == "incomplete" {
                    if r["incomplete_details"]["reason"] == "content_filter" {
                        Stop::Refusal
                    } else {
                        Stop::MaxTokens
                    }
                } else {
                    Stop::End
                };
                return Ok(Turn {
                    text,
                    calls,
                    raw: output,
                    usage,
                    stop,
                });
            }
            "response.failed" => {
                let e = &v["response"]["error"];
                return Err(AiError::Unavailable(
                    e["message"].as_str().unwrap_or("failed").into(),
                ));
            }
            "error" => {
                return Err(stream_error(
                    &json!({"error": {"message": v["message"], "code": v["code"]}}),
                ));
            }
            _ => {}
        }
    }
    Err(AiError::Unavailable("the stream ended early".into()))
}

// --- Chat Completions (DeepSeek, Gemini, compatible) ----------------------

fn chat_body(req: &Request) -> Value {
    let mut messages = vec![];
    if !req.system.is_empty() {
        messages.push(json!({"role": "system", "content": req.system}));
    }
    for it in &req.items {
        match it {
            Item::User(t) => messages.push(json!({"role": "user", "content": t})),
            Item::Assistant { text, calls, .. } => {
                let mut m = json!({"role": "assistant", "content": if text.is_empty() { Value::Null } else { json!(text) }});
                if !calls.is_empty() {
                    m["tool_calls"] = Value::Array(
                        calls
                            .iter()
                            .map(|k| json!({"id": k.id, "type": "function", "function": {"name": k.name, "arguments": k.input.to_string()}}))
                            .collect(),
                    );
                }
                messages.push(m);
            }
            Item::Results(rs) => {
                for r in rs {
                    messages.push(
                        json!({"role": "tool", "tool_call_id": r.call_id, "content": r.content}),
                    );
                }
            }
        }
    }
    let mut body = json!({
        "model": req.model,
        "messages": messages,
        "stream": true,
        "stream_options": {"include_usage": true},
        "max_tokens": req.max_tokens,
    });
    if !req.tools.is_empty() {
        body["tools"] = Value::Array(
            req.tools
                .iter()
                .map(|t| json!({"type": "function", "function": {"name": t.name, "description": t.description, "parameters": t.schema}}))
                .collect(),
        );
    }
    body
}

async fn read_chat(sse: &mut Sse, on_text: &mut (dyn FnMut(&str) + Send)) -> Result<Turn, AiError> {
    let mut text = String::new();
    // index -> (id, name, arguments)
    let mut calls: std::collections::BTreeMap<u64, (String, String, String)> = Default::default();
    let mut finish = None;
    let mut usage = Usage::default();
    let mut ended = false;
    while let Some(v) = sse.next().await? {
        if v.get("error").is_some() {
            return Err(stream_error(&v));
        }
        if let Some(c) = v["choices"].get(0) {
            let d = &c["delta"];
            if let Some(t) = d["content"].as_str() {
                on_text(t);
                text.push_str(t);
            }
            for tc in d["tool_calls"].as_array().into_iter().flatten() {
                let i = tc["index"].as_u64().unwrap_or(0);
                let e = calls.entry(i).or_default();
                if let Some(id) = tc["id"].as_str() {
                    e.0 = id.into();
                }
                if let Some(n) = tc["function"]["name"].as_str() {
                    e.1.push_str(n);
                }
                if let Some(a) = tc["function"]["arguments"].as_str() {
                    e.2.push_str(a);
                }
            }
            if let Some(f) = c["finish_reason"].as_str() {
                finish = Some(f.to_string());
                ended = true;
            }
        }
        let u = &v["usage"];
        if u.is_object() {
            let prompt = u["prompt_tokens"].as_u64().unwrap_or(0);
            let cached = u["prompt_tokens_details"]["cached_tokens"]
                .as_u64()
                .or(u["prompt_cache_hit_tokens"].as_u64())
                .unwrap_or(0);
            usage = Usage {
                input: prompt.saturating_sub(cached),
                output: u["completion_tokens"].as_u64().unwrap_or(0),
                cache_read: cached,
                cache_write: 0,
            };
        }
    }
    if !ended {
        return Err(AiError::Unavailable("the stream ended early".into()));
    }
    let calls: Vec<ToolCall> = calls
        .into_values()
        .map(|(id, name, args)| ToolCall {
            id,
            name,
            input: if args.trim().is_empty() {
                json!({})
            } else {
                serde_json::from_str(&args).unwrap_or_else(|_| json!({}))
            },
        })
        .collect();
    let stop = match finish.as_deref() {
        _ if !calls.is_empty() => Stop::Tools,
        Some("length") => Stop::MaxTokens,
        Some("content_filter") => Stop::Refusal,
        _ => Stop::End,
    };
    Ok(Turn {
        text,
        calls,
        raw: Value::Null,
        usage,
        stop,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_models_only() {
        assert!(chat_model(Kind::Openai, "gpt-5.4"));
        assert!(chat_model(Kind::Openai, "gpt-5.3-codex"));
        assert!(chat_model(Kind::Openai, "o4-mini"));
        assert!(!chat_model(Kind::Openai, "text-embedding-3-large"));
        assert!(!chat_model(Kind::Openai, "gpt-image-1"));
        assert!(!chat_model(Kind::Openai, "gpt-4o-realtime-preview"));
        assert!(!chat_model(Kind::Openai, "omni-moderation-latest"));
        assert!(chat_model(Kind::Gemini, "gemini-3.8-flash"));
        assert!(!chat_model(Kind::Gemini, "gemini-embedding-001"));
        assert!(chat_model(Kind::Deepseek, "deepseek-flash"));
        assert!(chat_model(Kind::Anthropic, "claude-opus-5-5"));
    }

    #[test]
    fn kinds() {
        for k in ["anthropic", "openai", "gemini", "deepseek", "compatible"] {
            assert_eq!(Kind::parse(k).unwrap().as_str(), k);
        }
        assert_eq!(Kind::parse("Anthropic"), None);
        assert!(Kind::Compatible.default_base().is_none());
    }

    #[test]
    fn events_split_on_blank_lines() {
        assert_eq!(find_event_end(b"data: x\n\ndata: y"), Some((7, 2)));
        assert_eq!(find_event_end(b"data: x\r\n\r\n"), Some((7, 4)));
        assert_eq!(find_event_end(b"data: x\n"), None);
    }

    #[test]
    fn bodies() {
        let req = Request {
            model: "m".into(),
            system: "be brief".into(),
            items: vec![
                Item::User("hi".into()),
                Item::Assistant {
                    text: "".into(),
                    calls: vec![ToolCall {
                        id: "c1".into(),
                        name: "search".into(),
                        input: json!({"q": "x"}),
                    }],
                    raw: Value::Null,
                },
                Item::Results(vec![ToolResult {
                    call_id: "c1".into(),
                    content: "found".into(),
                    is_error: false,
                }]),
            ],
            tools: vec![ToolDef {
                name: "search".into(),
                description: "d".into(),
                schema: json!({"type": "object"}),
            }],
            max_tokens: 100,
        };
        let m = messages_body(&req);
        assert_eq!(m["system"][0]["cache_control"]["type"], "ephemeral");
        assert_eq!(m["messages"][1]["content"][0]["type"], "tool_use");
        assert_eq!(m["messages"][2]["content"][0]["tool_use_id"], "c1");
        assert_eq!(m["tools"][0]["input_schema"]["type"], "object");
        let r = responses_body(&req);
        assert_eq!(r["store"], false);
        assert_eq!(r["instructions"], "be brief");
        assert_eq!(r["input"][1]["type"], "function_call");
        assert_eq!(r["input"][1]["arguments"], "{\"q\":\"x\"}");
        assert_eq!(r["input"][2]["type"], "function_call_output");
        assert_eq!(r["tools"][0]["parameters"]["type"], "object");
        let c = chat_body(&req);
        assert_eq!(c["messages"][0]["role"], "system");
        assert_eq!(
            c["messages"][2]["tool_calls"][0]["function"]["name"],
            "search"
        );
        assert_eq!(c["messages"][3]["role"], "tool");
        assert_eq!(c["stream_options"]["include_usage"], true);
        // Without tools or instructions, no fields for them.
        let bare = Request {
            system: "".into(),
            tools: vec![],
            ..req
        };
        assert!(messages_body(&bare).get("system").is_none());
        assert!(responses_body(&bare).get("tools").is_none());
        assert!(chat_body(&bare).get("tools").is_none());
    }
}
