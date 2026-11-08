//! Model services (docs/en/design/assistants.md): the deployment's built-in ones (`[ai]`,
//! keys from the environment) and the app's own (keys sealed in its
//! database). Adding or replacing a service checks the key against the
//! provider, the way Dify validates credentials before saving them.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{Value, json};

use super::Hub;
use crate::ai::prices::Prices;
use crate::ai::provider::{AiError, Endpoint, Kind, ModelInfo};
use crate::db::ai::ServiceRow;
use crate::logic::{AdminOp, Op};
use crate::model::*;

/// An app's own model services, at most.
pub const SERVICES_PER_APP: usize = 10;
/// How long a provider's model list is reused.
const MODELS_TTL: Duration = Duration::from_secs(600);
const KEY_MAX: usize = 512;
const DAILY_MAX: u64 = 1_000_000_000_000;

#[derive(Default)]
pub struct AiState {
    /// The assistants' runner looks again now (one was added or changed).
    pub(super) wake: tokio::sync::Notify,
    /// Replies in the last minute, per assistant.
    pub(super) minute: Mutex<HashMap<String, Vec<Instant>>>,
    /// MCP servers' tools by connection (name and version), with when.
    pub(super) mcp_tools: Mutex<HashMap<String, (Instant, Vec<crate::ai::connect::Tool>)>>,
    /// Each assistant's knowledge, ready to search.
    pub(super) indexes: Mutex<HashMap<String, std::sync::Arc<super::knowledge::Index>>>,
    prices: OnceLock<Prices>,
    /// Service (name and version) → (when fetched, models).
    models: Mutex<HashMap<String, (Instant, Vec<ModelInfo>)>>,
}

/// `PUT /admin/v1/app/model-services/{name}`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceReq {
    pub provider: String,
    #[serde(default)]
    pub base_url: Option<String>,
    /// Required for a new service; keeps the stored key when left out.
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub daily_tokens: Option<u64>,
}

/// A service ready to call.
pub struct Resolved {
    pub name: String,
    pub builtin: bool,
    pub endpoint: Endpoint,
    pub daily_tokens: u64,
}

fn unknown() -> ApiError {
    rule!(
        "model_service.unknown",
        "not_found",
        "no such model service"
    )
}

fn ai_error(e: AiError) -> ApiError {
    match e {
        AiError::Auth(m) => rule!(
            "model_service.key_refused",
            "bad_request",
            format!("the provider refused the key: {m}")
        ),
        AiError::Bad(m) => rule!(
            "model_service.refused",
            "bad_request",
            format!("the provider refused the request: {m}")
        ),
        AiError::RateLimited(m) | AiError::Unavailable(m) => rule!(
            "model_service.unreachable",
            "bad_request",
            format!("the provider could not be reached to check the key: {m}")
        ),
    }
}

fn hint(key: &str) -> String {
    let n = key.chars().count();
    let tail: String = key.chars().skip(n.saturating_sub(4)).collect();
    format!("…{tail}")
}

impl Hub {
    pub fn prices(&self) -> &Prices {
        self.inner.ai.prices.get_or_init(|| {
            self.inner
                .cfg
                .ai
                .prices_file
                .as_ref()
                .and_then(|f| std::fs::read_to_string(f).ok())
                .and_then(|t| Prices::parse(&t).ok())
                .unwrap_or_else(Prices::builtin)
        })
    }

    /// An HTTP client for a provider address: for an app's own address,
    /// resolved and checked here (no private or local addresses) and pinned,
    /// with no redirects.
    async fn provider_client(&self, base: &str, guarded: bool) -> ApiResult<reqwest::Client> {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let mut b = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10));
        if guarded {
            let private_ok = self.inner.cfg.ai.private_addresses;
            let (host, addrs) =
                crate::webhook::guarded_addresses(base, private_ok, Duration::from_secs(10))
                    .await
                    .map_err(|e| {
                        rule!("model_service.url", "bad_request", format!("base_url: {e}"))
                    })?;
            if let Some(h) = host {
                b = b.resolve_to_addrs(&h, &addrs);
            }
        }
        b.build()
            .map_err(|e| ApiError::new("internal", e.to_string()))
    }

    /// The built-in services and the app's own, without keys.
    pub async fn model_services(&self) -> ApiResult<Vec<Value>> {
        let mut out = vec![];
        for b in &self.inner.cfg.ai.builtin {
            let base = b
                .base_url
                .clone()
                .or_else(|| {
                    Kind::parse(&b.provider)
                        .and_then(|k| k.default_base())
                        .map(String::from)
                })
                .unwrap_or_default();
            let ready = std::env::var(&b.key_env).is_ok_and(|k| !k.trim().is_empty());
            out.push(
                json!({"name": b.name, "provider": b.provider, "builtin": true, "base_url": base,
                "ready": ready, "daily_tokens": b.daily_tokens}),
            );
        }
        let rows = self
            .read(self.control(), |db| {
                Box::pin(async move { db.model_services().await })
            })
            .await?;
        for r in rows {
            out.push(json!({"name": r.name, "provider": r.provider, "builtin": false, "base_url": r.base_url,
                "ready": true, "key_hint": r.key_hint, "daily_tokens": r.daily_tokens,
                "created_at": r.created_at, "updated_at": r.updated_at}));
        }
        Ok(out)
    }

    /// Creates or replaces one of the app's services after the provider
    /// accepted the key.
    pub async fn put_model_service(&self, name: &str, req: ServiceReq) -> ApiResult<Value> {
        if !crate::role::valid_name(name) {
            return Err(rule!(
                "model_service.name",
                "bad_request",
                "names are 1..64 of a-z 0-9 - (not starting or ending with -)"
            ));
        }
        if self.inner.cfg.ai.builtin.iter().any(|b| b.name == name) {
            return Err(rule!(
                "model_service.builtin",
                "conflict",
                "a built-in service has this name"
            ));
        }
        let kind = Kind::parse(&req.provider).ok_or_else(|| {
            rule!(
                "model_service.provider",
                "bad_request",
                "provider is anthropic, openai, gemini, deepseek or compatible"
            )
        })?;
        let base = match (&req.base_url, kind.default_base()) {
            (Some(u), _) => u.trim().trim_end_matches('/').to_string(),
            (None, Some(d)) => d.to_string(),
            (None, None) => {
                return Err(rule!(
                    "model_service.url",
                    "bad_request",
                    "a compatible service needs base_url"
                ));
            }
        };
        if base.len() > 2048 {
            return Err(rule!(
                "model_service.url",
                "bad_request",
                "base_url is too long"
            ));
        }
        crate::webhook::check_tenant_url(&base, self.inner.cfg.ai.private_addresses)
            .map_err(|e| rule!("model_service.url", "bad_request", format!("base_url: {e}")))?;
        let daily = req.daily_tokens.unwrap_or(0);
        if daily > DAILY_MAX {
            return Err(rule!(
                "model_service.daily_tokens",
                "bad_request",
                "daily_tokens is at most 10^12"
            ));
        }
        let n = name.to_string();
        let old = self
            .read(self.control(), move |db| {
                Box::pin(async move { db.model_service(&n).await })
            })
            .await?;
        let key = match (req.api_key, &old) {
            (Some(k), _) => k,
            (None, Some(o)) => o.api_key.clone(),
            (None, None) => {
                return Err(rule!(
                    "model_service.key",
                    "bad_request",
                    "api_key is required for a new service"
                ));
            }
        };
        if key.is_empty()
            || key.len() > KEY_MAX
            || key.chars().any(|c| c.is_whitespace() || c.is_control())
        {
            return Err(rule!(
                "model_service.key",
                "bad_request",
                "api_key is 1..512 characters without spaces"
            ));
        }
        // The key goes only to the address it belongs to.
        let endpoint = Endpoint {
            kind,
            base: base.clone(),
            key: key.clone(),
            http: self.provider_client(&base, true).await?,
        };
        let models = endpoint.models().await.map_err(ai_error)?;
        let now = now_ms();
        let row = ServiceRow {
            name: name.to_string(),
            provider: kind.as_str().to_string(),
            base_url: base,
            key_hint: hint(&key),
            api_key: key,
            daily_tokens: daily as i64,
            created_at: old.as_ref().map(|o| o.created_at).unwrap_or(now),
            updated_at: now,
        };
        let mut v = self
            .control()
            .call(Op::Admin(AdminOp::ModelServicePut(row)))
            .await?;
        self.inner
            .ai
            .models
            .lock()
            .unwrap()
            .retain(|k, _| !k.starts_with(&format!("{name}@")));
        v["models"] = json!(models.len());
        Ok(v)
    }

    pub async fn delete_model_service(&self, name: &str) -> ApiResult<Value> {
        if self.inner.cfg.ai.builtin.iter().any(|b| b.name == name) {
            return Err(rule!(
                "model_service.builtin_fixed",
                "bad_request",
                "built-in services are set in the deployment's configuration"
            ));
        }
        if !crate::role::valid_name(name) {
            return Err(unknown());
        }
        self.control()
            .call(Op::Admin(AdminOp::ModelServiceDelete {
                name: name.to_string(),
            }))
            .await
    }

    /// A service by name, ready to call.
    pub async fn resolve_model_service(&self, name: &str) -> ApiResult<Resolved> {
        if let Some(b) = self.inner.cfg.ai.builtin.iter().find(|b| b.name == name) {
            let kind = Kind::parse(&b.provider).ok_or_else(unknown)?;
            let key = std::env::var(&b.key_env).unwrap_or_default();
            if key.trim().is_empty() {
                return Err(rule!(
                    "model_service.not_ready",
                    "conflict",
                    format!("the built-in service has no key ({} is not set)", b.key_env)
                ));
            }
            let base = b
                .base_url
                .clone()
                .or(kind.default_base().map(String::from))
                .unwrap_or_default();
            return Ok(Resolved {
                name: b.name.clone(),
                builtin: true,
                endpoint: Endpoint {
                    kind,
                    base: base.clone(),
                    key,
                    http: self.provider_client(&base, false).await?,
                },
                daily_tokens: b.daily_tokens,
            });
        }
        if !crate::role::valid_name(name) {
            return Err(unknown());
        }
        let n = name.to_string();
        let row = self
            .read(self.control(), move |db| {
                Box::pin(async move { db.model_service(&n).await })
            })
            .await?
            .ok_or_else(unknown)?;
        let kind = Kind::parse(&row.provider).ok_or_else(unknown)?;
        Ok(Resolved {
            name: row.name.clone(),
            builtin: false,
            endpoint: Endpoint {
                kind,
                base: row.base_url.clone(),
                key: row.api_key.clone(),
                http: self.provider_client(&row.base_url, true).await?,
            },
            daily_tokens: row.daily_tokens.max(0) as u64,
        })
    }

    /// The models a service can use, from its provider (cached ten
    /// minutes); each says whether it has a price, and with a built-in key
    /// only priced models are usable (their cost must be known).
    pub async fn service_models(&self, name: &str) -> ApiResult<Value> {
        let s = self.resolve_model_service(name).await?;
        let version = if s.builtin {
            String::new()
        } else {
            let n = name.to_string();
            self.read(self.control(), move |db| {
                Box::pin(async move { db.model_service(&n).await })
            })
            .await?
            .map(|r| r.updated_at.to_string())
            .unwrap_or_default()
        };
        let key = format!("{name}@{version}");
        let cached = self
            .inner
            .ai
            .models
            .lock()
            .unwrap()
            .get(&key)
            .filter(|(at, _)| at.elapsed() < MODELS_TTL)
            .map(|(_, m)| m.clone());
        let models = match cached {
            Some(m) => m,
            None => {
                let m = s.endpoint.models().await.map_err(|e| match e {
                    AiError::Auth(m) => rule!(
                        "model_service.key_rejected",
                        "conflict",
                        format!("the provider no longer accepts the key: {m}")
                    ),
                    other @ AiError::RateLimited(_)
                    | other @ AiError::Unavailable(_)
                    | other @ AiError::Bad(_) => {
                        rule!("model_service.provider_down", "conflict", other.to_string())
                    }
                })?;
                self.inner
                    .ai
                    .models
                    .lock()
                    .unwrap()
                    .insert(key, (Instant::now(), m.clone()));
                m
            }
        };
        let now = now_ms();
        let provider = s.endpoint.kind.as_str();
        let list: Vec<Value> = models
            .iter()
            .map(|m| {
                let p = self.prices().find(provider, &m.id, now);
                json!({"id": m.id, "name": m.name, "context": m.context,
                    "price": p.map(|p| json!({"input": p.input, "output": p.output, "cache_read": p.cache_read, "as_of": p.as_of})),
                    "usable": !s.builtin || p.is_some()})
            })
            .collect();
        Ok(json!({"service": name, "provider": provider, "builtin": s.builtin, "models": list}))
    }
}
