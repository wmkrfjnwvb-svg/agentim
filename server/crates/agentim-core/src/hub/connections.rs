//! Connect (docs/en/design/assistants.md): the app's remote MCP servers and HTTP APIs. A
//! role names the ones it uses (`mcp: [...]`, `apis: [...]`); its agents
//! get them in their configuration, and a built-in agent gets their tools.
//! Saving a connection checks it (the MCP server lists its tools, the
//! OpenAPI document has operations); the credential is sealed and goes
//! only to the connection's own address, resolved and checked against
//! private addresses like a webhook's.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::Hub;
use crate::ai::connect::{Operation, Target, Tool, openapi, tool_name};
use crate::db::ai::ConnRow;
use crate::logic::{AdminOp, Op};
use crate::model::*;

pub const CONNECTIONS_PER_APP: usize = 20;
const OPERATIONS_MAX: usize = 200;
const TOOLS_TTL: Duration = Duration::from_secs(300);

/// `PUT /admin/v1/app/connections/{name}`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnReq {
    /// `mcp` or `api`.
    pub kind: String,
    pub url: String,
    #[serde(default)]
    pub auth_header: Option<String>,
    /// Kept when left out.
    #[serde(default)]
    pub secret: Option<String>,
    /// The OpenAPI 3 document of an API (JSON).
    #[serde(default)]
    pub spec: Option<Value>,
    /// `read_only` (default), `confirm` (write tools ask a person first,
    /// docs/en/design/assistants.md) or `allow`.
    #[serde(default)]
    pub writes: Option<String>,
    /// Who approves which operation (docs/en/design/assistants.md); replaced as a whole when
    /// given, kept when left out.
    #[serde(default)]
    pub approvals: Option<Vec<ApprovalRule>>,
}

/// One approval rule of a connection: an operation, optionally only above
/// an amount, approved by named people or run without asking.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ApprovalRule {
    /// The MCP tool or the OpenAPI `operationId`.
    pub tool: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amount_field: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub over: Option<f64>,
    /// User ids (names are resolved when saved).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approvers: Option<Vec<String>>,
    /// `false`: runs without asking.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirm: Option<bool>,
}

/// What a call of a tool needs (docs/en/design/assistants.md).
#[derive(Debug, Clone, PartialEq)]
pub enum Gate {
    Run,
    /// A person approves first: the named people (none: the person being
    /// answered), and the rule that decided.
    Ask {
        approvers: Option<Vec<String>>,
        rule: Option<ApprovalRule>,
    },
}

impl Bound {
    /// The first rule of this tool that applies to these arguments decides;
    /// with none, a write tool of a `confirm` connection asks.
    pub fn gate(&self, args: &Value) -> Gate {
        for r in &self.rules {
            let hit = match (&r.amount_field, r.over) {
                // Missing or not a number: counted as over (stricter).
                (Some(f), Some(o)) => args.get(f).and_then(Value::as_f64).is_none_or(|v| v > o),
                (Some(_), None) | (None, Some(_)) | (None, None) => true,
            };
            if hit {
                return if r.confirm == Some(false) {
                    Gate::Run
                } else {
                    Gate::Ask {
                        approvers: r.approvers.clone(),
                        rule: Some(r.clone()),
                    }
                };
            }
        }
        if self.confirm {
            Gate::Ask {
                approvers: None,
                rule: None,
            }
        } else {
            Gate::Run
        }
    }
}

/// A tool of a connection, as the model sees it.
#[derive(Clone)]
pub struct Bound {
    pub conn: String,
    pub tool: Tool,
    pub params: Vec<(String, String)>,
    /// A write tool of a `confirm` connection: a person approves each call.
    pub confirm: bool,
    /// The connection's rules for this tool, in order.
    pub rules: Vec<ApprovalRule>,
}

const APPROVAL_RULES_MAX: usize = 20;

fn unknown() -> ApiError {
    rule!("connection.unknown", "not_found", "no such connection")
}

impl Hub {
    async fn target(&self, c: &ConnRow) -> ApiResult<Target> {
        let private_ok = self.inner.cfg.ai.private_addresses;
        let (host, addrs) =
            crate::webhook::guarded_addresses(&c.url, private_ok, Duration::from_secs(10))
                .await
                .map_err(|e| rule!("connection.url", "bad_request", format!("url: {e}")))?;
        let _ = rustls::crypto::ring::default_provider().install_default();
        let mut b = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10));
        if let Some(h) = host {
            b = b.resolve_to_addrs(&h, &addrs);
        }
        Ok(Target {
            url: c.url.clone(),
            auth_header: c.auth_header.clone(),
            secret: c.secret.clone(),
            http: b
                .build()
                .map_err(|e| ApiError::new("internal", e.to_string()))?,
        })
    }

    /// A connection's tools (an MCP server's cached five minutes).
    async fn tools_of(&self, c: &ConnRow, fresh: bool) -> Result<Vec<Bound>, String> {
        let rules: Vec<ApprovalRule> = serde_json::from_str(&c.approvals).unwrap_or_default();
        let bind = |t: Tool, params| Bound {
            conn: c.name.clone(),
            confirm: c.writes == "confirm" && !t.read_only,
            rules: rules.iter().filter(|r| r.tool == t.name).cloned().collect(),
            tool: t,
            params,
        };
        if c.kind == "api" {
            let spec: Value = serde_json::from_str(&c.spec).unwrap_or(Value::Null);
            let ops = openapi(&spec, OPERATIONS_MAX)?;
            return Ok(ops
                .into_iter()
                .map(|o: Operation| bind(o.tool, o.params))
                .collect());
        }
        let key = format!("{}@{}", c.name, c.updated_at);
        if !fresh
            && let Some((at, t)) = self.inner.ai.mcp_tools.lock().unwrap().get(&key)
            && at.elapsed() < TOOLS_TTL
        {
            return Ok(t.iter().cloned().map(|t| bind(t, vec![])).collect());
        }
        let target = self.target(c).await.map_err(|e| e.message)?;
        let tools = target.mcp_tools().await?;
        self.inner
            .ai
            .mcp_tools
            .lock()
            .unwrap()
            .insert(key, (Instant::now(), tools.clone()));
        Ok(tools.into_iter().map(|t| bind(t, vec![])).collect())
    }

    pub async fn connections(&self) -> ApiResult<Vec<Value>> {
        let rows = self
            .read(self.control(), |db| {
                Box::pin(async move { db.connections(false).await })
            })
            .await?;
        Ok(rows
            .iter()
            .map(|c| {
                json!({"name": c.name, "kind": c.kind, "url": c.url, "auth_header": c.auth_header,
                "writes": c.writes, "approvals": serde_json::from_str::<Value>(&c.approvals).unwrap_or(json!([])),
                "created_at": c.created_at, "updated_at": c.updated_at})
            })
            .collect())
    }

    pub async fn put_connection(&self, name: &str, req: ConnReq) -> ApiResult<Value> {
        if !crate::role::valid_name(name) {
            return Err(rule!(
                "connection.name",
                "bad_request",
                "names are 1..64 of a-z 0-9 - (not starting or ending with -)"
            ));
        }
        if req.kind != "mcp" && req.kind != "api" {
            return Err(rule!(
                "connection.kind",
                "bad_request",
                "kind is mcp or api"
            ));
        }
        let url = req.url.trim().to_string();
        if url.len() > 2048 {
            return Err(rule!("connection.url", "bad_request", "url is too long"));
        }
        crate::webhook::check_tenant_url(&url, self.inner.cfg.ai.private_addresses)
            .map_err(|e| rule!("connection.url", "bad_request", format!("url: {e}")))?;
        let header = req.auth_header.clone().unwrap_or_default();
        if header.len() > 64
            || !header
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-')
        {
            return Err(rule!(
                "connection.header",
                "bad_request",
                "auth_header is a header name (letters, digits, -)"
            ));
        }
        let writes = req.writes.clone().unwrap_or_else(|| "read_only".into());
        if !matches!(writes.as_str(), "read_only" | "confirm" | "allow") {
            return Err(rule!(
                "connection.writes",
                "bad_request",
                "writes is read_only, confirm or allow"
            ));
        }
        let n = name.to_string();
        let old = self
            .read(self.control(), move |db| {
                Box::pin(async move { db.connection(&n).await })
            })
            .await?;
        let secret = match (req.secret, &old) {
            (Some(s), _) => s,
            (None, Some(o)) => o.secret.clone(),
            (None, None) => String::new(),
        };
        if secret.len() > 4096 || secret.chars().any(|c| c.is_control()) {
            return Err(rule!(
                "connection.secret",
                "bad_request",
                "secret is up to 4096 characters without control characters"
            ));
        }
        if !secret.is_empty() && header.is_empty() {
            return Err(rule!(
                "connection.header",
                "bad_request",
                "a secret needs auth_header (e.g. Authorization)"
            ));
        }
        let spec = match (&req.kind[..], req.spec) {
            ("api", Some(s)) => {
                let s = if let Some(t) = s.as_str() {
                    serde_json::from_str(t).unwrap_or(Value::Null)
                } else {
                    s
                };
                openapi(&s, OPERATIONS_MAX)
                    .map_err(|e| rule!("connection.spec", "bad_request", e))?;
                s.to_string()
            }
            ("api", None) => match old.as_ref().filter(|o| o.kind == "api") {
                Some(o) => o.spec.clone(),
                None => {
                    return Err(rule!(
                        "connection.spec",
                        "bad_request",
                        "an api needs its OpenAPI document (spec)"
                    ));
                }
            },
            (_, Some(_)) => {
                return Err(rule!("connection.spec", "bad_request", "spec is for apis"));
            }
            _ => String::new(),
        };
        let now = now_ms();
        let row = ConnRow {
            name: name.to_string(),
            kind: req.kind.clone(),
            url,
            auth_header: header,
            secret,
            spec,
            writes,
            created_at: old.as_ref().map(|o| o.created_at).unwrap_or(now),
            updated_at: now,
            approvals: "[]".into(),
        };
        // Left out: the saved rules stay (as the secret and the spec do),
        // checked again against the tools as they are now.
        let rules = match (req.approvals, &old) {
            (Some(r), _) => r,
            (None, Some(o)) => serde_json::from_str(&o.approvals).unwrap_or_default(),
            (None, None) => vec![],
        };
        if rules.len() > APPROVAL_RULES_MAX {
            return Err(rule!(
                "connection.approvals",
                "bad_request",
                "at most 20 approval rules"
            ));
        }
        // An MCP server must answer before it is saved.
        let tools = self.tools_of(&row, true).await.map_err(|e| {
            if e.contains("refused") {
                rule!("connection.refused", "bad_request", e)
            } else if e.contains("reach") || e.contains("timed out") || e.contains("answered") {
                rule!("connection.unreachable", "bad_request", e)
            } else {
                rule!("connection.invalid", "bad_request", e)
            }
        })?;
        let mut row = row;
        row.approvals = serde_json::to_string(&self.check_rules(rules, &tools)?).unwrap();
        let mut v = self
            .control()
            .call(Op::Admin(AdminOp::ConnectionPut(row)))
            .await?;
        v["tools"] = json!(tools.len());
        Ok(v)
    }

    /// Checks approval rules against the connection's tools; people named
    /// become user ids.
    fn check_rules(
        &self,
        rules: Vec<ApprovalRule>,
        tools: &[Bound],
    ) -> ApiResult<Vec<ApprovalRule>> {
        let mut out = vec![];
        for mut r in rules {
            if !tools.iter().any(|b| b.tool.name == r.tool) {
                return Err(rule!(
                    "connection.approval_tool",
                    "bad_request",
                    format!("{} is not an operation of this connection", r.tool)
                ));
            }
            match (&r.amount_field, r.over) {
                (Some(f), Some(o)) => {
                    if f.is_empty() || f.len() > 64 || !o.is_finite() || o < 0.0 {
                        return Err(rule!(
                            "connection.approval_amount",
                            "bad_request",
                            "amount_field is 1..64 characters and over a number of at least 0"
                        ));
                    }
                }
                (None, None) => {}
                (Some(_), None) | (None, Some(_)) => {
                    return Err(rule!(
                        "connection.approval_amount",
                        "bad_request",
                        "amount_field and over go together"
                    ));
                }
            }
            if let Some(list) = r.approvers.take() {
                if r.confirm == Some(false) {
                    return Err(rule!(
                        "connection.approvers",
                        "bad_request",
                        "approvers are for operations that ask (not confirm: false)"
                    ));
                }
                if list.is_empty() || list.len() > 20 {
                    return Err(rule!(
                        "connection.approvers",
                        "bad_request",
                        "1..20 approvers"
                    ));
                }
                let mut ids = vec![];
                for k in list {
                    let u = self
                        .user(&k)
                        .or_else(|| self.user_by_name(&k))
                        .ok_or_else(|| {
                            rule!("account.unknown_user", "not_found", format!("user {k}"))
                        })?;
                    if u.kind != UserKind::Human {
                        return Err(rule!(
                            "connection.approvers",
                            "bad_request",
                            "approvers are people"
                        ));
                    }
                    if !ids.contains(&u.id) {
                        ids.push(u.id);
                    }
                }
                r.approvers = Some(ids);
            }
            out.push(r);
        }
        Ok(out)
    }

    pub async fn delete_connection(&self, name: &str) -> ApiResult<Value> {
        if !crate::role::valid_name(name) {
            return Err(unknown());
        }
        self.control()
            .call(Op::Admin(AdminOp::ConnectionDelete {
                name: name.to_string(),
            }))
            .await
    }

    /// A connection's tools, for the console.
    pub async fn connection_tools(&self, name: &str) -> ApiResult<Value> {
        if !crate::role::valid_name(name) {
            return Err(unknown());
        }
        let n = name.to_string();
        let c = self
            .read(self.control(), move |db| {
                Box::pin(async move { db.connection(&n).await })
            })
            .await?
            .ok_or_else(unknown)?;
        let tools = self
            .tools_of(&c, true)
            .await
            .map_err(|e| rule!("connection.down", "conflict", e))?;
        let allowed = c.writes != "read_only";
        Ok(
            json!({"name": name, "tools": tools.iter().map(|b| json!({"name": b.tool.name, "description": b.tool.description,
            "read_only": b.tool.read_only, "usable": allowed || b.tool.read_only, "confirm": b.confirm,
            // Its parameters (name, JSON type): the console offers the numeric ones as an amount.
            "params": b.tool.schema["properties"].as_object().map(|p| p.iter().map(|(k, v)| json!({"name": k, "type": v["type"]})).collect::<Vec<_>>()).unwrap_or_default()})).collect::<Vec<_>>()}),
        )
    }

    /// The connections a role names, for its agents' configuration: the
    /// credential goes to the agent's runtime.
    pub(super) async fn connections_named(
        &self,
        mcp: &[String],
        apis: &[String],
    ) -> ApiResult<(Vec<Value>, Vec<Value>)> {
        let rows = self
            .read(self.control(), |db| {
                Box::pin(async move { db.connections(true).await })
            })
            .await?;
        let view = |c: &ConnRow| {
            let mut v =
                json!({"name": c.name, "url": c.url, "writes": c.writes, "version": c.updated_at});
            if !c.auth_header.is_empty() {
                v["headers"] = json!({c.auth_header.clone(): c.secret});
            }
            if c.kind == "api" {
                v["spec"] = serde_json::from_str(&c.spec).unwrap_or(Value::Null);
            }
            v
        };
        let m = rows
            .iter()
            .filter(|c| c.kind == "mcp" && mcp.contains(&c.name))
            .map(view)
            .collect();
        let a = rows
            .iter()
            .filter(|c| c.kind == "api" && apis.contains(&c.name))
            .map(view)
            .collect();
        Ok((m, a))
    }

    /// The tools a built-in agent gets from its role's connections: the
    /// read-only ones, and the others where the connection allows writes
    /// (with or without a person confirming each call).
    /// A connection that doesn't answer is left out of this reply.
    pub(super) async fn bound_tools(
        &self,
        mcp: &[String],
        apis: &[String],
    ) -> ApiResult<HashMap<String, Bound>> {
        let rows = self
            .read(self.control(), |db| {
                Box::pin(async move { db.connections(true).await })
            })
            .await?;
        let mut out = HashMap::new();
        for c in rows.iter().filter(|c| {
            (c.kind == "mcp" && mcp.contains(&c.name))
                || (c.kind == "api" && apis.contains(&c.name))
        }) {
            let Ok(tools) = self.tools_of(c, false).await else {
                continue;
            };
            for b in tools {
                if c.writes != "read_only" || b.tool.read_only {
                    out.insert(tool_name(&c.name, &b.tool.name), b);
                }
            }
        }
        Ok(out)
    }

    /// Calls a bound tool; `idempotency_key` goes to an API as
    /// `Idempotency-Key` (an approved call, docs/en/design/assistants.md).
    pub(super) async fn call_bound(
        &self,
        b: &Bound,
        args: &Value,
        idempotency_key: Option<&str>,
    ) -> Result<String, String> {
        let n = b.conn.clone();
        let c = self
            .read(self.control(), move |db| {
                Box::pin(async move { db.connection(&n).await })
            })
            .await
            .map_err(|e| e.message)?
            .ok_or("the connection is gone")?;
        let t = self.target(&c).await.map_err(|e| e.message)?;
        match &b.tool.http {
            Some((method, path)) => {
                t.api_call(method, path, args, &b.params, idempotency_key)
                    .await
            }
            None => t.mcp_call(&b.tool.name, args).await,
        }
    }
}
