//! `agentim admin …`: the admin API of a running server from the command
//! line (docs/12 §4), with an admin key. It only makes HTTP calls; it never
//! opens the database.

use clap::Subcommand;
use serde_json::{Value, json};

#[derive(Subcommand, Debug, Clone)]
pub enum AdminCmd {
    /// The app at a glance: people, agents, conversations, open reports.
    Overview,
    /// People and agents (`--q` part of the id, `--kind human|agent`).
    Users {
        #[arg(long)]
        q: Option<String>,
        #[arg(long)]
        kind: Option<String>,
        #[arg(long)]
        after: Option<String>,
    },
    /// One user: profile, devices, agents, permissions.
    User {
        id: String,
    },
    /// Sets what a person may do in their client, e.g.
    /// '{"create_agents": true}' (null keys follow the app's defaults).
    Permissions {
        id: String,
        json: String,
    },
    Deactivate {
        id: String,
    },
    Reactivate {
        id: String,
    },
    /// Agents.
    Agents {
        #[arg(long)]
        q: Option<String>,
    },
    /// Creates an agent owned by a person and prints its key once.
    AgentCreate {
        id: String,
        #[arg(long)]
        owner: String,
        #[arg(long)]
        team: bool,
    },
    /// A new key for an agent (printed once).
    AgentKey {
        id: String,
        #[arg(long)]
        revoke_others: bool,
    },
    AgentStop {
        id: String,
        #[arg(long)]
        reason: Option<String>,
    },
    AgentResume {
        id: String,
    },
    /// Gives a team agent a new owner.
    AgentTransfer {
        id: String,
        #[arg(long)]
        owner: String,
        #[arg(long)]
        rotate_keys: bool,
    },
    AgentDestroy {
        id: String,
    },
    /// Conversations (metadata).
    Conversations {
        #[arg(long)]
        q: Option<String>,
    },
    /// The reports queue (`--state open|resolved`).
    Reports {
        #[arg(long)]
        state: Option<String>,
    },
    /// Decides a report: none, remove_content, suspend_user or ban_user.
    Resolve {
        id: String,
        #[arg(long)]
        action: String,
        #[arg(long)]
        reason: String,
        #[arg(long)]
        days: Option<i64>,
    },
    /// Audit records, newest first (`--prefix admin.` for instance).
    Audit {
        #[arg(long)]
        prefix: Option<String>,
        #[arg(long, default_value_t = 50)]
        limit: i64,
    },
    ApiKeys,
    ApiKeyCreate {
        #[arg(long)]
        name: String,
        /// Comma separated, e.g. users:write,messages:write
        #[arg(long)]
        scopes: String,
    },
    ApiKeyRevoke {
        key_id: String,
    },
    SigningKeys,
    SigningKeyCreate {
        #[arg(long)]
        name: String,
    },
    SigningKeyRevoke {
        kid: String,
    },
    /// The deployment: databases, writers, encryption, version.
    Status,
    /// Cluster nodes and shards.
    Cluster,
    /// Takes a cluster node out of service.
    Drain {
        node: String,
    },
    /// Console accounts.
    Admins,
    /// Creates a console account; its password is read from standard input.
    AdminCreate {
        name: String,
        #[arg(long)]
        role: String,
    },
}

fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn query(pairs: &[(&str, Option<String>)]) -> String {
    let q: Vec<String> = pairs
        .iter()
        .filter_map(|(k, v)| v.as_ref().map(|v| format!("{k}={}", enc(v))))
        .collect();
    if q.is_empty() {
        String::new()
    } else {
        format!("?{}", q.join("&"))
    }
}

fn read_password() -> Result<String, String> {
    let mut pw = String::new();
    std::io::stdin()
        .read_line(&mut pw)
        .map_err(|e| e.to_string())?;
    Ok(pw.trim_end_matches(['\r', '\n']).to_string())
}

/// Runs one command; the server's answer, or why it failed.
pub async fn run(url: &str, key: &str, cmd: &AdminCmd) -> Result<Value, String> {
    use AdminCmd::*;
    let (method, path, body): (&str, String, Option<Value>) = match cmd {
        Overview => ("GET", "/app/overview".into(), None),
        Users { q, kind, after } => (
            "GET",
            format!(
                "/app/users{}",
                query(&[
                    ("q", q.clone()),
                    ("kind", kind.clone()),
                    ("after", after.clone())
                ])
            ),
            None,
        ),
        User { id } => ("GET", format!("/app/users/{}", enc(id)), None),
        Permissions { id, json } => {
            let v: Value = serde_json::from_str(json).map_err(|e| format!("permissions: {e}"))?;
            (
                "PATCH",
                format!("/app/users/{}", enc(id)),
                Some(json!({"permissions": v})),
            )
        }
        Deactivate { id } => ("POST", format!("/app/users/{}:deactivate", enc(id)), None),
        Reactivate { id } => ("POST", format!("/app/users/{}:reactivate", enc(id)), None),
        Agents { q } => (
            "GET",
            format!("/app/agents{}", query(&[("q", q.clone())])),
            None,
        ),
        AgentCreate { id, owner, team } => (
            "POST",
            "/app/agents".into(),
            Some(json!({"id": id, "owner": owner, "team": team})),
        ),
        AgentKey { id, revoke_others } => (
            "POST",
            format!("/app/agents/{}/keys", enc(id)),
            Some(json!({"revoke_others": revoke_others})),
        ),
        AgentStop { id, reason } => (
            "POST",
            format!("/app/agents/{}:stop", enc(id)),
            Some(json!({"reason": reason})),
        ),
        AgentResume { id } => ("POST", format!("/app/agents/{}:resume", enc(id)), None),
        AgentTransfer {
            id,
            owner,
            rotate_keys,
        } => (
            "POST",
            format!("/app/agents/{}:transfer", enc(id)),
            Some(json!({"owner": owner, "rotate_keys": rotate_keys})),
        ),
        AgentDestroy { id } => ("DELETE", format!("/app/agents/{}", enc(id)), None),
        Conversations { q } => (
            "GET",
            format!("/app/conversations{}", query(&[("q", q.clone())])),
            None,
        ),
        Reports { state } => (
            "GET",
            format!("/app/reports{}", query(&[("state", state.clone())])),
            None,
        ),
        Resolve {
            id,
            action,
            reason,
            days,
        } => (
            "POST",
            format!("/app/reports/{}:resolve", enc(id)),
            Some(json!({"action": action, "reason": reason, "days": days})),
        ),
        Audit { prefix, limit } => (
            "GET",
            format!(
                "/app/audit{}",
                query(&[
                    ("prefix", prefix.clone()),
                    ("limit", Some(limit.to_string()))
                ])
            ),
            None,
        ),
        ApiKeys => ("GET", "/app/api-keys".into(), None),
        ApiKeyCreate { name, scopes } => (
            "POST",
            "/app/api-keys".into(),
            Some(json!({
                "name": name,
                "scopes": scopes.split(',').map(str::trim).filter(|s| !s.is_empty()).collect::<Vec<_>>(),
            })),
        ),
        ApiKeyRevoke { key_id } => ("DELETE", format!("/app/api-keys/{}", enc(key_id)), None),
        SigningKeys => ("GET", "/app/signing-keys".into(), None),
        SigningKeyCreate { name } => (
            "POST",
            "/app/signing-keys".into(),
            Some(json!({"name": name})),
        ),
        SigningKeyRevoke { kid } => ("DELETE", format!("/app/signing-keys/{}", enc(kid)), None),
        Status => ("GET", "/deploy/status".into(), None),
        Cluster => ("GET", "/deploy/cluster".into(), None),
        Drain { node } => (
            "POST",
            format!("/deploy/cluster/nodes/{}:drain", enc(node)),
            None,
        ),
        Admins => ("GET", "/deploy/admins".into(), None),
        AdminCreate { name, role } => (
            "POST",
            "/deploy/admins".into(),
            Some(json!({"name": name, "role": role, "password": read_password()?})),
        ),
    };
    let http = crate::webhook::client();
    let mut r = http
        .request(
            method.parse().map_err(|e| format!("{e}"))?,
            format!("{}/admin/v1{path}", url.trim_end_matches('/')),
        )
        .header("authorization", format!("Bearer {key}"));
    if let Some(b) = body {
        r = r
            .header("content-type", "application/json")
            .body(b.to_string());
    }
    let resp = r.send().await.map_err(|e| e.to_string())?;
    let status = resp.status();
    let text = resp.text().await.map_err(|e| e.to_string())?;
    let v: Value = serde_json::from_str(&text).unwrap_or(Value::String(text));
    if status.is_success() {
        Ok(v)
    } else {
        Err(format!(
            "{} {}: {}",
            status.as_u16(),
            v["code"].as_str().unwrap_or("error"),
            v["detail"].as_str().unwrap_or("")
        ))
    }
}
