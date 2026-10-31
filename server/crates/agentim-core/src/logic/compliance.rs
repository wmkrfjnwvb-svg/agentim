//! Account deletion, reports and moderation decisions (docs/04-security-compliance.md
//! §4.2). The account is marked deleted in the control database first, so
//! nothing new can be sent; each conversation's writer then clears what
//! the person wrote and takes them out, serialized with every other write
//! of that conversation.

use std::sync::Arc;

use serde_json::{Value, json};

use super::{Ctx, err};
use crate::db::{AUTH_EPOCH, ERASE_BATCH};
use crate::model::*;
use crate::registry::Out;

impl Ctx<'_, '_> {
    /// An event about the deployment rather than a conversation: only
    /// webhooks receive it (`user.deleted`, `report.*`).
    fn deployment_event(&mut self, kind: &str, data: Value) {
        self.out.push(Out::Event {
            ev: Event {
                conv_id: String::new(),
                seq: 0,
                kind: kind.to_string(),
                actor_id: None,
                msg: None,
                data,
                created_at: now_ms(),
            },
            recips: Arc::new(vec![]),
            trace: crate::trace::current().map(|t| t.child()),
        });
    }

    pub(super) async fn erase_start(
        &mut self,
        user_id: &str,
        by: Option<&str>,
    ) -> ApiResult<Value> {
        let fresh = self.db.erasure(user_id).await?.is_none();
        let e = self.db.erasure_start(user_id, by).await?;
        if fresh {
            self.db.bump_meta(AUTH_EPOCH).await?;
            self.db
                .audit(by, "account.deleted", None, Some(user_id), json!({}))
                .await?;
            self.out.push(Out::Invalidate);
        }
        self.sh
            .deactivated
            .write()
            .unwrap()
            .insert(user_id.to_string());
        Ok(e)
    }

    pub(super) async fn erase_finish(&mut self, user_id: &str, name: &str) -> ApiResult<Value> {
        let was_running = self
            .db
            .erasure(user_id)
            .await?
            .is_some_and(|e| e["state"] == "running");
        self.db.erasure_finish(user_id).await?;
        if was_running {
            self.db
                .audit(None, "account.erased", None, Some(user_id), json!({}))
                .await?;
            self.deployment_event("user.deleted", json!({"user_id": name}));
        }
        Ok(self.db.erasure(user_id).await?.unwrap_or_default())
    }

    /// One step of erasing a person in a conversation: a batch of their
    /// messages; once none are left, what else they wrote, then they
    /// leave (ownership passes on; alone, the conversation is dissolved).
    /// Repeatable: a restart finishes what a crash interrupted.
    pub(super) async fn erase_in_conv(&mut self, conv_id: &str, user: &User) -> ApiResult<Value> {
        self.own_conv(conv_id)?;
        self.touch(conv_id);
        let n = self.db.erase_sender_batch(conv_id, &user.id).await?;
        if n >= ERASE_BATCH {
            return Ok(json!({"done": false}));
        }
        self.db.erase_user_extras(conv_id, &user.id).await?;
        // And the files they uploaded (deleted from the store in the background).
        self.trash_uploader(conv_id, &user.id).await?;
        // Devices clear their local copies of what this person wrote.
        let ev = self
            .db
            .append_event(
                conv_id,
                "user.erased",
                None,
                None,
                json!({"user_id": user.id}),
            )
            .await?;
        self.emit(ev).await?;
        let members = self.members(conv_id).await?;
        let Some(mine) = members.iter().find(|m| m.user_id == user.id).cloned() else {
            return Ok(json!({"done": true}));
        };
        // Mutation check of the race tests: staying a member lets later
        // sends through.
        if self
            .sh
            .failpoints
            .hit(crate::hub::DELETED_CAN_SEND)
            .await
            .is_err()
        {
            return Ok(json!({"done": true}));
        }
        if mine.role == "owner" {
            let next = members
                .iter()
                .filter(|m| m.user_id != user.id && m.kind == UserKind::Human)
                .min_by_key(|m| (m.role != "admin", m.join_seq))
                .cloned();
            match next {
                None => {
                    let v = self.dissolve(conv_id, user).await?;
                    return Ok(json!({"done": true, "dissolved": true, "members": v["members"]}));
                }
                Some(n) => {
                    self.db.set_role(conv_id, &n.user_id, "owner").await?;
                    let ev = self
                        .db
                        .append_event(
                            conv_id,
                            "owner.transferred",
                            None,
                            None,
                            json!({"from": user.id, "to": n.user_id, "reason": "account_deleted"}),
                        )
                        .await?;
                    self.emit(ev).await?;
                }
            }
        }
        let ev = self
            .db
            .append_event(
                conv_id,
                "member.removed",
                None,
                None,
                json!({"user_id": user.id, "name": user.name, "kind": user.kind, "reason": "account_deleted"}),
            )
            .await?;
        self.emit(ev).await?;
        self.db.remove_member(conv_id, &user.id).await?;
        self.st.members.remove(conv_id);
        self.st.recips.remove(conv_id);
        self.out.push(Out::Unfocus {
            conv_id: conv_id.to_string(),
            user_id: Some(user.id.clone()),
        });
        if self.idx == self.sh.control {
            self.db.member_index_remove(conv_id, &user.id).await?;
        }
        Ok(json!({"done": true, "removed": true}))
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn report_put(
        &mut self,
        reporter: &str,
        kind: &str,
        target: &str,
        conv_id: Option<&str>,
        category: &str,
        severity: i64,
        body: &Value,
        due_at: i64,
        per_hour: u32,
    ) -> ApiResult<Value> {
        if let Some(id) = self.db.open_report_by(reporter, kind, target).await? {
            return Ok(json!({"report_id": id, "duplicate": true}));
        }
        if self
            .db
            .reports_since(reporter, now_ms() - 3_600_000)
            .await?
            >= per_hour as i64
        {
            return Err(err("rate_limited", "too many reports; try again later"));
        }
        let id = self
            .db
            .report_put(
                reporter, kind, target, conv_id, category, severity, body, due_at,
            )
            .await?;
        self.deployment_event(
            "report.created",
            json!({"report_id": id, "target_kind": kind, "target_id": target, "conv_id": conv_id, "category": category, "severity": severity, "due_at": due_at}),
        );
        Ok(json!({"report_id": id, "duplicate": false}))
    }

    /// One decision for every open report on the report's target. A
    /// suspension or ban is part of the same write.
    pub(super) async fn report_resolve(
        &mut self,
        report_id: &str,
        decision: &Value,
        by: &str,
        suspend_until: Option<i64>,
        ban: bool,
    ) -> ApiResult<Value> {
        let r = self
            .db
            .report(report_id)
            .await?
            .ok_or_else(|| err("not_found", "no such report"))?;
        if r["state"] != "open" {
            return Err(err("conflict", "this report was already resolved"));
        }
        let kind = r["target_kind"].as_str().unwrap_or_default().to_string();
        let target = r["target_id"].as_str().unwrap_or_default().to_string();
        let ids = self.db.reports_resolve(&kind, &target, decision).await?;
        if ids.is_empty() {
            return Err(err("conflict", "this report was already resolved"));
        }
        let user = r["evidence"]["user_id"].as_str().map(str::to_string);
        if (suspend_until.is_some() || ban)
            && let Some(u) = &user
        {
            if ban {
                self.db.set_suspended(u, None).await?;
                self.db.set_deactivated(u, true).await?;
            } else {
                self.db.set_suspended(u, suspend_until).await?;
                self.db.bump_meta(AUTH_EPOCH).await?;
            }
            self.sh.deactivated.write().unwrap().insert(u.clone());
            self.out.push(Out::Invalidate);
        }
        self.db
            .audit(
                Some(by),
                "report.resolved",
                r["conv_id"].as_str(),
                Some(&target),
                json!({"reports": ids, "action": decision["action"]}),
            )
            .await?;
        self.deployment_event(
            "report.resolved",
            json!({"report_ids": ids, "target_kind": kind, "target_id": target, "decision": decision}),
        );
        Ok(
            json!({"resolved": ids, "target_kind": kind, "target_id": target, "conv_id": r["conv_id"], "user_id": user}),
        )
    }

    /// Ends suspensions that are over and deletes old resolved reports.
    pub(super) async fn moderation_upkeep(&mut self, keep_before: i64) -> ApiResult<Value> {
        let ended = self.db.suspensions_ended(now_ms()).await?;
        if !ended.is_empty() {
            let mut d = self.sh.deactivated.write().unwrap();
            for u in &ended {
                d.remove(u);
            }
        }
        if !ended.is_empty() {
            self.db.bump_meta(AUTH_EPOCH).await?;
            self.out.push(Out::Invalidate);
        }
        let purged = self.db.reports_purge(keep_before).await?;
        Ok(json!({"reactivated": ended, "purged": purged}))
    }
}
