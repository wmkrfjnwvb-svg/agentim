//! Dye rules (docs/14 §4.2, D140): an operator marks a user, device,
//! conversation, agent or API key, and its requests are recorded in full on
//! every node until the rule expires. Rules need a reason, are audited by
//! the admin API, last at most 24 hours and stay inside the server.

use super::*;
use crate::logic::AdminOp;
use crate::trace::{DYE_DEFAULT_MINUTES, DYE_KINDS, DYE_MAX_MINUTES, DyeRule, Subject};

impl Hub {
    /// The rules in force.
    pub fn dye_rules(&self) -> Vec<DyeRule> {
        let now = now_ms();
        self.inner
            .sh
            .dyes
            .read()
            .unwrap()
            .iter()
            .filter(|r| r.active(now))
            .cloned()
            .collect()
    }

    /// Adds a rule; `by` is who (for the record).
    pub async fn add_dye_rule(
        &self,
        kind: &str,
        target: &str,
        reason: &str,
        minutes: Option<i64>,
        by: &str,
    ) -> ApiResult<DyeRule> {
        if !DYE_KINDS.contains(&kind) {
            return Err(rule!(
                "dye.kind",
                "bad_request",
                format!("kind must be one of {}", DYE_KINDS.join(", ")),
            ));
        }
        if target.is_empty() || target.len() > 128 || target.chars().any(char::is_control) {
            return Err(rule!(
                "dye.target",
                "bad_request",
                "target must be 1..128 bytes"
            ));
        }
        let reason = reason.trim();
        if reason.is_empty() || reason.len() > 500 {
            return Err(rule!(
                "dye.reason",
                "bad_request",
                "a reason of 1..500 bytes is required"
            ));
        }
        let minutes = minutes.unwrap_or(DYE_DEFAULT_MINUTES);
        if !(1..=DYE_MAX_MINUTES).contains(&minutes) {
            return Err(rule!(
                "dye.minutes",
                "bad_request",
                format!("minutes must be 1..{DYE_MAX_MINUTES} (24 hours at most)"),
            ));
        }
        // The things that can be looked up must exist.
        let who = || self.user(target).or_else(|| self.user_by_name(target));
        match kind {
            "user" if !who().is_some_and(|u| u.kind == UserKind::Human) => {
                return Err(rule!("dye.unknown_target", "not_found", "no such person"));
            }
            "agent" if !who().is_some_and(|u| u.kind == UserKind::Agent) => {
                return Err(rule!("dye.unknown_target", "not_found", "no such agent"));
            }
            "conv" if self.inner.sh.dir_get(target).is_none() => {
                return Err(rule!(
                    "dye.unknown_target",
                    "not_found",
                    "no such conversation"
                ));
            }
            _ => {}
        }
        if self
            .dye_rules()
            .iter()
            .any(|r| r.kind == kind && r.target == target)
        {
            return Err(rule!(
                "dye.duplicate",
                "conflict",
                "this target is already dyed"
            ));
        }
        let now = now_ms();
        let rule = DyeRule {
            id: format!("dye_{}", new_id()),
            kind: kind.to_string(),
            target: target.to_string(),
            reason: reason.to_string(),
            by: by.to_string(),
            created_at: now,
            expires_at: now + minutes * 60_000,
        };
        self.control_call(Op::Admin(AdminOp::DyeAdd(rule.clone())))
            .await?;
        Ok(rule)
    }

    pub async fn remove_dye_rule(&self, id: &str) -> ApiResult<()> {
        self.control_call(Op::Admin(AdminOp::DyeRemove { id: id.to_string() }))
            .await?;
        Ok(())
    }

    /// Dyes the current request when a rule matches it.
    pub(crate) fn dye_request(&self, s: &Subject) {
        let rules = self.inner.sh.dyes.read().unwrap();
        if rules.is_empty() {
            return;
        }
        if let Some(r) = crate::trace::matching(&rules, s, now_ms()) {
            crate::trace::dye(&r.id);
        }
    }
}
