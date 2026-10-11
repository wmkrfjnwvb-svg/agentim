//! Protocol versioning (docs/02-architecture.md §2.5): which protocol major
//! versions this server speaks, the features it declares, and the client
//! information exchanged in the handshake.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::model::ApiError;

/// Protocol major versions this server speaks, newest first.
pub const PROTOCOL_VERSIONS: &[u32] = &[1];

/// Features clients may rely on. A feature not listed is not supported.
/// Experimental features use `x.<domain>.<name>`.
pub const FEATURES: &[&str] = &[
    "side_channel",
    "streams",
    "approval",
    "intervention",
    "membership",
    "blocks",
    "resync",
    "observe",
    "custom_kinds",
    "signals",
    "devices",
];

/// Every operation of protocol v1, so an unknown one can be told apart from
/// a malformed known one (`unknown_op` vs `bad_request`).
pub const OPS: &[&str] = &[
    "hello",
    "list_convs",
    "create_conv",
    "add_member",
    "remove_member",
    "leave",
    "set_role",
    "transfer_owner",
    "dissolve",
    "update_conv",
    "block",
    "unblock",
    "list_blocks",
    "find_user",
    "send",
    "edit",
    "signal",
    "recall",
    "pull",
    "pull_side",
    "read",
    "focus",
    "stream_start",
    "stream_append",
    "stream_end",
    "stream_cancel",
    "whisper",
    "approve",
    "reject",
    "takeover",
    "release",
    "set_agent_policy",
    "reset_guard",
    "list_drafts",
    "stats",
    "list_devices",
    "logout_device",
    "sync",
    "ack",
    "set_mute",
    "unread",
    "set_prefs",
    "observe",
    "set_password",
    "totp_setup",
    "totp_enable",
    "totp_disable",
    "create_agent",
    "list_agents",
    "agent_key",
    "revoke_agent_key",
    "request_human",
    "decide",
    "cancel_request",
    "get_request",
    "inbox",
];

pub const MAX_CLIENT_FEATURES: usize = 100;

/// What a client says about itself in `hello`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ClientInfo {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub platform: String,
}

/// A semantic version reduced to (major, minor, patch); pre-release and
/// build suffixes are ignored.
pub fn parse_version(v: &str) -> Option<(u64, u64, u64)> {
    let core = v.split(['-', '+']).next()?;
    let mut parts = core.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().unwrap_or("0").parse().ok()?;
    let patch = parts.next().unwrap_or("0").parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

/// The protocol both sides speak: the newest version in both lists.
/// `None` from the client means an old client that predates negotiation.
pub fn negotiate(requested: Option<&[u32]>) -> Result<u32, ApiError> {
    match requested {
        None => Ok(PROTOCOL_VERSIONS[0]),
        Some(list) => PROTOCOL_VERSIONS
            .iter()
            .copied()
            .find(|v| list.contains(v))
            .ok_or_else(|| {
                ApiError::new(
                    "unsupported_protocol",
                    format!("this server speaks protocol {PROTOCOL_VERSIONS:?}"),
                )
            }),
    }
}

/// Checks the client's description against configured minimum versions.
pub fn check_client(
    client: &ClientInfo,
    features: &[String],
    min_versions: &std::collections::BTreeMap<String, String>,
) -> Result<(), ApiError> {
    let bad = |m: &str| ApiError::new("bad_request", m.to_string());
    if client.name.len() > 64 || client.version.len() > 64 || client.platform.len() > 64 {
        return Err(bad(
            "client name, version and platform are at most 64 bytes",
        ));
    }
    if features.len() > MAX_CLIENT_FEATURES || features.iter().any(|f| f.is_empty() || f.len() > 64)
    {
        return Err(bad("at most 100 features of 1..64 bytes"));
    }
    if let Some(min) = min_versions.get(&client.name) {
        let have = parse_version(&client.version)
            .ok_or_else(|| bad("client version must look like 1.2.3"))?;
        let need = parse_version(min).unwrap_or((0, 0, 0));
        if have < need {
            return Err(ApiError::new(
                "client_too_old",
                format!(
                    "{} {} is no longer supported; upgrade to {min} or later",
                    client.name, client.version
                ),
            ));
        }
    }
    Ok(())
}

/// Label value for metrics: client names are user input, so only a bounded
/// set of well-formed names is kept.
pub fn metric_client_name(name: &str) -> String {
    let ok = !name.is_empty()
        && name.len() <= 32
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "._-".contains(c));
    if ok {
        name.to_string()
    } else {
        "other".to_string()
    }
}

/// The `welcome` frame and the protocol part of `hello` responses.
pub fn server_info(edition: crate::edition::Edition) -> Value {
    json!({
        "protocol": PROTOCOL_VERSIONS[0],
        "protocols": PROTOCOL_VERSIONS,
        "server": {"name": edition.binary(), "product": edition.product(), "edition": edition.as_str(), "version": env!("CARGO_PKG_VERSION")},
        "features": FEATURES,
        "limits": {
            "message_bytes": crate::logic::MAX_MESSAGE_BYTES,
            "frame_bytes": 1 << 20,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_parse_and_compare() {
        assert_eq!(parse_version("1.2.3"), Some((1, 2, 3)));
        assert_eq!(parse_version("2"), Some((2, 0, 0)));
        assert_eq!(parse_version("1.4"), Some((1, 4, 0)));
        assert_eq!(parse_version("1.2.3-beta.1+build5"), Some((1, 2, 3)));
        assert_eq!(parse_version(""), None);
        assert_eq!(parse_version("1.x"), None);
        assert_eq!(parse_version("1.2.3.4"), None);
        assert!(parse_version("1.10.0") > parse_version("1.9.9"));
    }

    #[test]
    fn negotiation_picks_the_newest_common_version() {
        assert_eq!(
            negotiate(None).unwrap(),
            1,
            "old clients get the current version"
        );
        assert_eq!(negotiate(Some(&[1, 2])).unwrap(), 1);
        assert_eq!(
            negotiate(Some(&[2])).unwrap_err().code,
            "unsupported_protocol"
        );
        assert_eq!(
            negotiate(Some(&[])).unwrap_err().code,
            "unsupported_protocol"
        );
    }

    #[test]
    fn client_checks() {
        let mut min = std::collections::BTreeMap::new();
        min.insert("agentim-js".to_string(), "1.2.0".to_string());
        let c = |name: &str, v: &str| ClientInfo {
            name: name.into(),
            version: v.into(),
            platform: "web".into(),
        };
        assert!(check_client(&c("agentim-js", "1.2.0"), &[], &min).is_ok());
        assert!(check_client(&c("agentim-js", "2.0.0"), &[], &min).is_ok());
        assert_eq!(
            check_client(&c("agentim-js", "1.1.9"), &[], &min)
                .unwrap_err()
                .code,
            "client_too_old"
        );
        assert_eq!(
            check_client(&c("agentim-js", "garbage"), &[], &min)
                .unwrap_err()
                .code,
            "bad_request"
        );
        assert!(
            check_client(&c("other-client", "0.0.1"), &[], &min).is_ok(),
            "no minimum configured"
        );
        assert!(check_client(&c(&"n".repeat(65), "1"), &[], &min).is_err());
        let many: Vec<String> = (0..101).map(|i| format!("f{i}")).collect();
        assert!(check_client(&c("x", "1"), &many, &min).is_err());
        assert!(check_client(&c("x", "1"), &["".into()], &min).is_err());
    }

    #[test]
    fn metric_labels_are_bounded() {
        assert_eq!(metric_client_name("agentim-js"), "agentim-js");
        assert_eq!(metric_client_name("Evil Name{}"), "other");
        assert_eq!(metric_client_name(&"a".repeat(33)), "other");
        assert_eq!(metric_client_name(""), "other");
    }

    #[test]
    fn ops_match_the_request_parser_exactly() {
        // serde names every accepted op in its "unknown variant" error; OPS
        // must be exactly that set, so a new ClientMsg variant can't be
        // answered with `unknown_op`, nor a removed one with `bad_request`.
        let e = serde_json::from_value::<crate::model::ClientMsg>(json!({"op": "no_such_op"}))
            .unwrap_err()
            .to_string();
        let accepted: std::collections::BTreeSet<&str> = e
            .split("expected one of")
            .nth(1)
            .unwrap_or_else(|| panic!("unexpected serde message: {e}"))
            .split(',')
            .map(|p| {
                p.trim()
                    .trim_matches(|c: char| c == '`' || c.is_whitespace())
            })
            .map(|p| p.split('`').next().unwrap_or(p))
            .filter(|p| !p.is_empty())
            .collect();
        let listed: std::collections::BTreeSet<&str> = OPS.iter().copied().collect();
        assert_eq!(accepted, listed, "serde says: {e}");
        assert_eq!(listed.len(), OPS.len(), "OPS has duplicates");
    }
}
