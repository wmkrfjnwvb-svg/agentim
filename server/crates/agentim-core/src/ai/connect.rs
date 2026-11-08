//! Connect (docs/en/design/assistants.md): tools from remote MCP servers (Streamable HTTP,
//! JSON-RPC 2.0) and from HTTP APIs described by OpenAPI 3 (JSON). Each
//! becomes a function the model can call; only read-only ones unless the
//! connection allows writes.

use std::time::Duration;

use serde_json::{Value, json};

/// The MCP protocol version asked for.
pub const MCP_VERSION: &str = "2025-06-18";
/// A tool result sent back to the model, at most (characters).
pub const RESULT_MAX: usize = 20_000;
const CALL_TIMEOUT: Duration = Duration::from_secs(30);

/// One tool a connection offers.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Tool {
    pub name: String,
    pub description: String,
    pub schema: Value,
    /// Doesn't change anything (MCP `readOnlyHint`; GET and HEAD).
    pub read_only: bool,
    /// For an API operation: method and path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http: Option<(String, String)>,
}

/// Where and how to call a connection.
#[derive(Clone)]
pub struct Target {
    pub url: String,
    pub auth_header: String,
    pub secret: String,
    pub http: reqwest::Client,
}

fn truncate(mut s: String) -> String {
    if s.chars().count() > RESULT_MAX {
        s = s.chars().take(RESULT_MAX).collect::<String>() + "\n…(cut)";
    }
    s
}

impl Target {
    fn auth(&self, b: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        let b = b.header("user-agent", concat!("agentim/", env!("CARGO_PKG_VERSION")));
        if self.auth_header.is_empty() || self.secret.is_empty() {
            b
        } else {
            b.header(self.auth_header.as_str(), self.secret.as_str())
        }
    }

    /// One JSON-RPC request; the answer is JSON or a stream of events.
    async fn rpc(
        &self,
        session: &mut Option<String>,
        method: &str,
        params: Value,
        id: Option<u64>,
    ) -> Result<Value, String> {
        let mut body = json!({"jsonrpc": "2.0", "method": method, "params": params});
        if let Some(i) = id {
            body["id"] = json!(i);
        }
        let mut b = self
            .auth(self.http.post(&self.url))
            .timeout(CALL_TIMEOUT)
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream")
            .header("mcp-protocol-version", MCP_VERSION);
        if let Some(s) = session.as_deref() {
            b = b.header("mcp-session-id", s);
        }
        let res = b.body(body.to_string()).send().await.map_err(|e| {
            if e.is_timeout() {
                "the MCP server timed out".to_string()
            } else {
                "could not reach the MCP server".to_string()
            }
        })?;
        if let Some(s) = res
            .headers()
            .get("mcp-session-id")
            .and_then(|v| v.to_str().ok())
        {
            *session = Some(s.to_string());
        }
        let status = res.status().as_u16();
        if id.is_none() {
            return if (200..300).contains(&status) {
                Ok(Value::Null)
            } else {
                Err(format!("the MCP server answered {status}"))
            };
        }
        if status == 401 || status == 403 {
            return Err("the MCP server refused the credential".into());
        }
        if !(200..300).contains(&status) {
            return Err(format!("the MCP server answered {status}"));
        }
        let sse = res
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|t| t.starts_with("text/event-stream"));
        let text = res
            .text()
            .await
            .map_err(|_| "the MCP server's answer broke off".to_string())?;
        let msg = if sse {
            text.split("\n\n")
                .filter_map(|ev| {
                    let data: String = ev
                        .lines()
                        .filter_map(|l| l.strip_prefix("data:"))
                        .map(|d| d.trim_start())
                        .collect();
                    serde_json::from_str::<Value>(&data).ok()
                })
                .find(|v| v["id"] == json!(id))
                .ok_or("the MCP server sent no answer")?
        } else {
            serde_json::from_str::<Value>(&text)
                .map_err(|_| "the MCP server's answer is not JSON".to_string())?
        };
        if let Some(e) = msg.get("error") {
            return Err(format!(
                "MCP error: {}",
                e["message"].as_str().unwrap_or("unknown")
            ));
        }
        Ok(msg["result"].clone())
    }

    async fn mcp_open(&self) -> Result<Option<String>, String> {
        let mut session = None;
        self.rpc(
            &mut session,
            "initialize",
            json!({"protocolVersion": MCP_VERSION, "capabilities": {}, "clientInfo": {"name": "agentim", "version": env!("CARGO_PKG_VERSION")}}),
            Some(1),
        )
        .await?;
        self.rpc(&mut session, "notifications/initialized", json!({}), None)
            .await?;
        Ok(session)
    }

    /// The tools an MCP server offers.
    pub async fn mcp_tools(&self) -> Result<Vec<Tool>, String> {
        let mut session = self.mcp_open().await?;
        let mut out = vec![];
        let mut cursor: Option<String> = None;
        for page in 0..20 {
            let params = match &cursor {
                Some(c) => json!({"cursor": c}),
                None => json!({}),
            };
            let r = self
                .rpc(&mut session, "tools/list", params, Some(2 + page))
                .await?;
            for t in r["tools"].as_array().into_iter().flatten() {
                let Some(name) = t["name"].as_str() else {
                    continue;
                };
                out.push(Tool {
                    name: name.to_string(),
                    description: t["description"]
                        .as_str()
                        .unwrap_or_default()
                        .chars()
                        .take(1000)
                        .collect(),
                    schema: if t["inputSchema"].is_object() {
                        t["inputSchema"].clone()
                    } else {
                        json!({"type": "object"})
                    },
                    read_only: t["annotations"]["readOnlyHint"] == true,
                    http: None,
                });
            }
            cursor = r["nextCursor"].as_str().map(String::from);
            if cursor.is_none() {
                break;
            }
        }
        Ok(out)
    }

    /// Calls an MCP tool; its text content.
    pub async fn mcp_call(&self, tool: &str, args: &Value) -> Result<String, String> {
        let mut session = self.mcp_open().await?;
        let r = self
            .rpc(
                &mut session,
                "tools/call",
                json!({"name": tool, "arguments": args}),
                Some(100),
            )
            .await?;
        let text: Vec<String> = r["content"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|c| match c["type"].as_str() {
                Some("text") => c["text"].as_str().unwrap_or_default().to_string(),
                _ => c.to_string(),
            })
            .collect();
        let out = truncate(text.join("\n"));
        if r["isError"] == true {
            Err(out)
        } else {
            Ok(out)
        }
    }

    /// Calls an API operation: path parameters filled in, the rest as the
    /// query (GET, DELETE) or the JSON body.
    pub async fn api_call(
        &self,
        method: &str,
        path: &str,
        args: &Value,
        params: &[(String, String)],
        idempotency_key: Option<&str>,
    ) -> Result<String, String> {
        let mut url_path = path.to_string();
        let mut query: Vec<(String, String)> = vec![];
        let mut headers: Vec<(String, String)> = vec![];
        for (name, place) in params {
            let Some(v) = args.get(name) else { continue };
            // Any JSON value a model sends: a string as it is, the rest as
            // JSON text (serde_json's shapes, not a business state).
            #[allow(clippy::wildcard_enum_match_arm)]
            let v = match v {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            match place.as_str() {
                "path" => url_path = url_path.replace(&format!("{{{name}}}"), &urlencode(&v)),
                "header" => headers.push((name.clone(), v)),
                _ => query.push((name.clone(), v)),
            }
        }
        if url_path.contains('{') {
            return Err("a path parameter is missing".into());
        }
        let mut url = format!("{}{}", self.url.trim_end_matches('/'), url_path);
        if !query.is_empty() {
            let q: Vec<String> = query
                .iter()
                .map(|(k, v)| format!("{}={}", urlencode(k), urlencode(v)))
                .collect();
            url = format!("{url}?{}", q.join("&"));
        }
        let m =
            reqwest::Method::from_bytes(method.as_bytes()).map_err(|_| "bad method".to_string())?;
        let mut b = self.auth(self.http.request(m, url)).timeout(CALL_TIMEOUT);
        for (k, v) in headers {
            b = b.header(k, v);
        }
        if let Some(k) = idempotency_key {
            b = b.header("idempotency-key", k);
        }
        if let Some(body) = args.get("body") {
            b = b
                .header("content-type", "application/json")
                .body(body.to_string());
        }
        let res = b.send().await.map_err(|e| {
            if e.is_timeout() {
                "the API timed out".to_string()
            } else {
                "could not reach the API".to_string()
            }
        })?;
        let status = res.status().as_u16();
        let text = truncate(res.text().await.unwrap_or_default());
        if (200..300).contains(&status) {
            Ok(text)
        } else {
            Err(format!("the API answered {status}: {text}"))
        }
    }
}

fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// An operation of an OpenAPI document: its tool and where its
/// parameters go.
#[derive(Debug, Clone, PartialEq)]
pub struct Operation {
    pub tool: Tool,
    pub params: Vec<(String, String)>,
}

/// The operations of an OpenAPI 3 document (JSON), at most `max`.
pub fn openapi(spec: &Value, max: usize) -> Result<Vec<Operation>, String> {
    if !spec["openapi"].as_str().is_some_and(|v| v.starts_with('3')) {
        return Err("an OpenAPI 3 document (JSON) is expected".into());
    }
    let paths = spec["paths"]
        .as_object()
        .ok_or("the document has no paths")?;
    let mut out = vec![];
    for (path, item) in paths {
        for method in ["get", "post", "put", "patch", "delete", "head"] {
            let Some(op) = item.get(method).filter(|o| o.is_object()) else {
                continue;
            };
            let id = op["operationId"]
                .as_str()
                .map(String::from)
                .unwrap_or_else(|| {
                    format!(
                        "{method}_{}",
                        path.trim_matches('/').replace(['/', '{', '}'], "_")
                    )
                });
            let mut props = serde_json::Map::new();
            let mut required = vec![];
            let mut params = vec![];
            let all = item["parameters"]
                .as_array()
                .into_iter()
                .flatten()
                .chain(op["parameters"].as_array().into_iter().flatten());
            for p in all {
                let p = resolve(spec, p);
                let (Some(name), Some(place)) = (p["name"].as_str(), p["in"].as_str()) else {
                    continue;
                };
                if place == "cookie" {
                    continue;
                }
                let mut schema = resolve(spec, &p["schema"]);
                if !schema.is_object() {
                    schema = json!({"type": "string"});
                }
                if let Some(d) = p["description"].as_str() {
                    schema["description"] = json!(d);
                }
                props.insert(name.to_string(), schema);
                if p["required"] == true || place == "path" {
                    required.push(json!(name));
                }
                params.push((name.to_string(), place.to_string()));
            }
            let body = resolve(spec, &op["requestBody"]);
            let bschema = resolve(spec, &body["content"]["application/json"]["schema"]);
            if bschema.is_object() {
                props.insert("body".into(), bschema);
                if body["required"] == true {
                    required.push(json!("body"));
                }
            }
            let description = op["summary"]
                .as_str()
                .or(op["description"].as_str())
                .unwrap_or_default()
                .chars()
                .take(1000)
                .collect::<String>();
            out.push(Operation {
                tool: Tool {
                    name: id,
                    description: format!("{} ({} {path})", description, method.to_uppercase())
                        .trim()
                        .to_string(),
                    schema: json!({"type": "object", "properties": props, "required": required}),
                    read_only: matches!(method, "get" | "head"),
                    http: Some((method.to_uppercase(), path.clone())),
                },
                params,
            });
            if out.len() > max {
                return Err(format!("the document has more than {max} operations"));
            }
        }
    }
    if out.is_empty() {
        return Err("the document has no operations".into());
    }
    Ok(out)
}

/// Follows a local `$ref` (`#/components/...`), a few levels deep.
fn resolve(spec: &Value, v: &Value) -> Value {
    let mut v = v.clone();
    for _ in 0..8 {
        let Some(r) = v["$ref"].as_str().and_then(|r| r.strip_prefix("#/")) else {
            break;
        };
        let mut target = spec;
        for part in r.split('/') {
            target = &target[part.replace("~1", "/").replace("~0", "~")];
        }
        v = target.clone();
    }
    v
}

/// A tool name every provider accepts: `[a-zA-Z0-9_-]{1,64}`.
pub fn tool_name(prefix: &str, name: &str) -> String {
    let raw = format!("{prefix}__{name}");
    let clean: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    clean.chars().take(64).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPEC: &str = r##"{
      "openapi": "3.0.3",
      "paths": {
        "/orders/{id}": {
          "parameters": [{"name": "id", "in": "path", "schema": {"type": "string"}}],
          "get": {"operationId": "getOrder", "summary": "One order", "parameters": [{"name": "fields", "in": "query"}]},
          "delete": {"summary": "Cancel"}
        },
        "/orders": {
          "post": {"operationId": "createOrder", "requestBody": {"required": true, "content": {"application/json": {"schema": {"$ref": "#/components/schemas/Order"}}}}}
        }
      },
      "components": {"schemas": {"Order": {"type": "object", "properties": {"sku": {"type": "string"}}}}}
    }"##;

    #[test]
    fn openapi_operations() {
        let ops = openapi(&serde_json::from_str(SPEC).unwrap(), 100).unwrap();
        let get = ops.iter().find(|o| o.tool.name == "getOrder").unwrap();
        assert!(get.tool.read_only);
        assert_eq!(get.tool.schema["required"], json!(["id"]));
        assert_eq!(
            get.params,
            vec![
                ("id".into(), "path".into()),
                ("fields".into(), "query".into())
            ]
        );
        let del = ops
            .iter()
            .find(|o| o.tool.http.as_ref().unwrap().0 == "DELETE")
            .unwrap();
        assert_eq!(del.tool.name, "delete_orders__id_");
        assert!(!del.tool.read_only);
        let post = ops.iter().find(|o| o.tool.name == "createOrder").unwrap();
        assert_eq!(
            post.tool.schema["properties"]["body"]["properties"]["sku"]["type"],
            "string"
        );
        assert_eq!(post.tool.schema["required"], json!(["body"]));
        assert!(openapi(&json!({"swagger": "2.0"}), 10).is_err());
        assert!(openapi(&json!({"openapi": "3.1.0", "paths": {}}), 10).is_err());
        assert!(openapi(&serde_json::from_str(SPEC).unwrap(), 1).is_err());
    }

    #[test]
    fn names_and_encoding() {
        assert_eq!(tool_name("crm", "find.contact"), "crm__find_contact");
        assert_eq!(tool_name("x", &"y".repeat(100)).len(), 64);
        assert_eq!(urlencode("a b/ü"), "a%20b%2F%C3%BC");
    }
}
