//! Exporting one's data, deleting an account, reports and the moderation
//! queue (docs/04-security-compliance.md §4.2).

use serde_json::{Value, json};

use super::{Hub, err};
use crate::logic::Op;
use crate::model::*;

/// Report categories; the first four are severe (handled first).
pub const CATEGORIES: &[&str] = &[
    "child_safety",
    "self_harm",
    "violence",
    "illegal",
    "spam",
    "harassment",
    "hate",
    "sexual",
    "impersonation",
    "other",
];
const SEVERE: usize = 4;
/// Fault point after each conversation of an erasure (tests).
pub const ERASURE_STEP: &str = "erasure.after_conv";
/// Mutation check of the race tests: a deleted person could still send.
pub const DELETED_CAN_SEND: &str = "mutation.deleted_can_send";
const REPORT_TEXT_MAX: usize = 1000;
/// Messages per export page read from a database.
const EXPORT_PAGE: i64 = 500;

impl Hub {
    // ------------------------------------------------------------ deletion

    /// Deletes a person's account: at once they can no longer sign in, be
    /// added or send; their content is then erased conversation by
    /// conversation in the background. Repeatable. `by`: the API key that
    /// asked (the developer's backend decides, docs/04 §4.2).
    pub async fn delete_account(&self, user: &User, by: Option<&str>) -> ApiResult<Value> {
        if user.kind != UserKind::Human {
            return Err(err(
                "bad_request",
                "only a person's account is deleted; agents are destroyed",
            ));
        }
        let e = self
            .control()
            .call(Op::EraseStart {
                user_id: user.id.clone(),
                by: by.map(str::to_string),
            })
            .await?;
        self.inner
            .sh
            .deactivated
            .write()
            .unwrap()
            .insert(user.id.clone());
        self.revoke_tokens(&user.id, None).await?;
        if e["state"] == "running" {
            self.spawn_erasure(user.clone());
        }
        Ok(e)
    }

    pub async fn erasure_status(&self, user_id: &str) -> ApiResult<Option<Value>> {
        let id = user_id.to_string();
        self.read(self.control(), move |db| {
            Box::pin(async move { db.erasure(&id).await })
        })
        .await
    }

    fn spawn_erasure(&self, user: User) {
        if !self.inner.erasing.lock().unwrap().insert(user.id.clone()) {
            return;
        }
        let hub = self.clone();
        tokio::spawn(async move {
            let mut wait = std::time::Duration::from_millis(200);
            loop {
                match hub.run_erasure(&user).await {
                    Ok(()) => break,
                    Err(e) => {
                        tracing::warn!(
                            user = user.id,
                            error = e.message,
                            "account erasure failed; retrying"
                        );
                        tokio::time::sleep(wait).await;
                        wait = (wait * 2).min(std::time::Duration::from_secs(30));
                    }
                }
            }
            hub.inner.erasing.lock().unwrap().remove(&user.id);
        });
    }

    /// Waits for running erasures (tests and shutdown).
    pub async fn erasures_idle(&self) {
        while !self.inner.erasing.lock().unwrap().is_empty() {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }

    async fn run_erasure(&self, user: &User) -> ApiResult<()> {
        // Their agents are destroyed (and leave every conversation).
        let agents: Vec<User> = self
            .inner
            .sh
            .users
            .read()
            .unwrap()
            .values()
            .filter(|u| u.kind == UserKind::Agent && u.owner_id.as_deref() == Some(&user.id))
            .cloned()
            .collect();
        for a in agents {
            if self.inner.sh.destroyed.read().unwrap().contains(&a.id) {
                continue;
            }
            self.destroy_agent_now(&a, &user.id, true).await?;
        }
        let id = user.id.clone();
        let convs = self
            .read(self.control(), move |db| {
                Box::pin(async move { db.convs_touched(&id).await })
            })
            .await?;
        for c in convs {
            self.erase_in(&c, user).await?;
            self.control()
                .call(Op::EraseProgress {
                    user_id: user.id.clone(),
                })
                .await?;
            // Tests stop here to check that a restart finishes the work.
            self.inner
                .sh
                .failpoints
                .hit(ERASURE_STEP)
                .await
                .map_err(|e| err("unavailable", e))?;
        }
        self.control()
            .call(Op::EraseFinish {
                user_id: user.id.clone(),
                name: user.name.clone(),
            })
            .await?;
        Ok(())
    }

    async fn erase_in(&self, conv_id: &str, user: &User) -> ApiResult<()> {
        let _members = self.member_lock(conv_id).await;
        loop {
            let shard_idx = match self.conv_shard(conv_id) {
                Ok(s) => s.idx,
                // Dissolved meanwhile: its content is gone already.
                Err(e) if e.code == "not_found" => return Ok(()),
                Err(e) => return Err(e),
            };
            let v = match self
                .on_conv_op(conv_id, || Op::EraseInConv {
                    conv_id: conv_id.to_string(),
                    user: user.clone(),
                })
                .await
            {
                Ok(v) => v,
                Err(e) if e.code == "not_found" => return Ok(()),
                Err(e) => return Err(e),
            };
            if v["done"] != true {
                continue;
            }
            let control = self.inner.sh.control;
            if v["dissolved"] == true {
                if shard_idx != control {
                    self.control()
                        .call(Op::ForgetConv(conv_id.to_string()))
                        .await?;
                    for u in v["members"].as_array().into_iter().flatten() {
                        if let Some(u) = u.as_str() {
                            self.control()
                                .call(Op::Departure {
                                    user_id: u.to_string(),
                                    conv_id: conv_id.to_string(),
                                    seq: 0,
                                    dissolved: true,
                                })
                                .await?;
                        }
                    }
                }
                self.inner.sh.dir_set(conv_id, None);
            } else if v["removed"] == true && shard_idx != control {
                self.control()
                    .call(Op::MemberIndexRemove {
                        conv_id: conv_id.to_string(),
                        user_id: user.id.clone(),
                    })
                    .await?;
            }
            return Ok(());
        }
    }

    /// Finishes erasures a crash or restart interrupted.
    pub(super) async fn resume_erasures(&self) -> ApiResult<()> {
        let running = self
            .read(self.control(), |db| {
                Box::pin(async move { db.erasures_running().await })
            })
            .await?;
        for (id, _) in running {
            if let Some(u) = self.inner.sh.user(&id) {
                self.spawn_erasure(u);
            }
        }
        Ok(())
    }

    // -------------------------------------------------------------- export

    /// A person's own data as NDJSON lines (docs/04 §4.2), sent as it is
    /// read. `after`: continue after this conversation id.
    pub async fn export_user(
        &self,
        user: &User,
        after: Option<String>,
    ) -> ApiResult<tokio::sync::mpsc::Receiver<ApiResult<String>>> {
        let (tx, rx) = tokio::sync::mpsc::channel(16);
        let hub = self.clone();
        let user = user.clone();
        tokio::spawn(async move {
            if let Err(e) = hub.export_lines(&user, after, &tx).await {
                let _ = tx.send(Err(e)).await;
            }
        });
        Ok(rx)
    }

    async fn export_lines(
        &self,
        user: &User,
        after: Option<String>,
        tx: &tokio::sync::mpsc::Sender<ApiResult<String>>,
    ) -> ApiResult<()> {
        let line = |v: Value| Ok(format!("{v}\n"));
        let send = |v: Value| {
            let tx = tx.clone();
            async move {
                tx.send(line(v))
                    .await
                    .map_err(|_| err("bad_request", "the export was cancelled"))
            }
        };
        let id = user.id.clone();
        if after.is_none() {
            let profile = self.user_profile(&user.id).await?.unwrap_or_default();
            send(json!({"type": "export", "format": "agentim.export/1", "user_id": user.name, "created_at": now_ms()})).await?;
            send(json!({"type": "profile", "profile": profile})).await?;
            let uid = id.clone();
            let head = self
                .read(self.control(), move |db| {
                    Box::pin(async move {
                        let mut h = db.user_export_head(&uid).await?;
                        h["reports"] = json!(db.reports_by(&uid).await?);
                        h["prefs"] = db
                            .user_profile(&uid)
                            .await?
                            .map(|p| p["prefs"].clone())
                            .unwrap_or(Value::Null);
                        Ok(h)
                    })
                })
                .await?;
            for k in ["devices", "blocks", "agents", "reports"] {
                send(json!({"type": k, k: head[k]})).await?;
            }
        }
        let uid = id.clone();
        let mut convs = self
            .read(self.control(), move |db| {
                Box::pin(async move { db.convs_touched(&uid).await })
            })
            .await?;
        if let Some(a) = &after {
            convs.retain(|c| c > a);
        }
        for c in convs {
            let Ok(shard) = self.conv_shard(&c) else {
                continue;
            };
            let (cid, uid) = (c.clone(), id.clone());
            let (row, member) = self
                .read(shard, move |db| {
                    Box::pin(
                        async move { Ok((db.conv(&cid).await?, db.member(&cid, &uid).await?)) },
                    )
                })
                .await?;
            let Some(row) = row else { continue };
            send(json!({
                "type": "conversation",
                "cursor": c,
                "conv_id": c,
                "kind": row.kind,
                "title": row.title,
                "member": member.as_ref().map(|m| json!({"role": m.role, "joined_seq": m.join_seq})),
            }))
            .await?;
            let mut seq = 0;
            loop {
                let (cid, uid) = (c.clone(), id.clone());
                let page = self
                    .read(shard, move |db| {
                        Box::pin(async move {
                            db.messages_by_sender(&cid, &uid, seq, EXPORT_PAGE).await
                        })
                    })
                    .await?;
                for m in &page {
                    let recalled = m.state == "recalled";
                    send(json!({
                        "type": "message",
                        "conv_id": c,
                        "msg_id": m.msg_id,
                        "seq": m.created_seq,
                        "created_at": m.created_at,
                        "updated_at": m.updated_at,
                        "kind": m.kind,
                        "content": if recalled { Value::Null } else { json!(m.content) },
                        "payload": if recalled { Value::Null } else { json!(m.payload) },
                        "state": m.state,
                        "reply_to": m.reply_to,
                    }))
                    .await?;
                }
                let n = page.len() as i64;
                if let Some(last) = page.last() {
                    seq = last.created_seq;
                }
                if n < EXPORT_PAGE {
                    break;
                }
            }
            let (cid, uid) = (c.clone(), id.clone());
            let answers = self
                .read(shard, move |db| {
                    Box::pin(async move { db.user_answers(&cid, &uid).await })
                })
                .await?;
            send(json!({"type": "answers", "conv_id": c, "feedback": answers["feedback"], "card_responses": answers["card_responses"]})).await?;
        }
        send(json!({"type": "end"})).await?;
        Ok(())
    }

    // ------------------------------------------------------------- reports

    /// A person reports a message, a user or a conversation.
    pub async fn report(
        &self,
        me: &User,
        target_kind: &str,
        target_id: &str,
        category: &str,
        text: Option<&str>,
    ) -> ApiResult<Value> {
        let Some(cat) = CATEGORIES.iter().position(|c| *c == category) else {
            return Err(err(
                "bad_request",
                format!("category must be one of {}", CATEGORIES.join(", ")),
            ));
        };
        if let Some(t) = text
            && (t.chars().count() > REPORT_TEXT_MAX || t.contains('\0'))
        {
            return Err(err("bad_request", "text must be at most 1000 characters"));
        }
        let (conv_id, evidence) = match target_kind {
            "message" => {
                let id = target_id.to_string();
                let conv = self
                    .find_conv_of(|db| {
                        Box::pin({
                            let id = id.clone();
                            async move { db.msg_conv(&id).await }
                        })
                    })
                    .await
                    .map_err(|_| err("not_found", "no such message"))?;
                let shard = self.conv_shard(&conv)?;
                let (c, m, u) = (conv.clone(), id.clone(), me.id.clone());
                let (msg, member) = self
                    .read(shard, move |db| {
                        Box::pin(
                            async move { Ok((db.message(&m).await?, db.member(&c, &u).await?)) },
                        )
                    })
                    .await?;
                let msg = msg.ok_or_else(|| err("not_found", "no such message"))?;
                let member = member
                    .ok_or_else(|| err("forbidden", "you are not a member of this conversation"))?;
                if msg.created_seq < member.visible_from {
                    return Err(err("not_found", "no such message"));
                }
                if msg.sender_id == me.id {
                    return Err(err("bad_request", "you cannot report your own message"));
                }
                let sender = self.inner.sh.user(&msg.sender_id);
                (
                    Some(conv.clone()),
                    json!({
                        "user_id": msg.sender_id,
                        "user": sender.map(|s| s.name),
                        "sender_type": msg.sender_type,
                        "conv_id": conv,
                        "msg_id": msg.msg_id,
                        "seq": msg.created_seq,
                        "kind": msg.kind,
                        "content": msg.content,
                        "payload": msg.payload,
                        "state": msg.state,
                        "created_at": msg.created_at,
                    }),
                )
            }
            "user" => {
                let u = self
                    .user_or_load(target_id)
                    .await?
                    .filter(|u| u.kind != UserKind::System)
                    .ok_or_else(|| err("not_found", "no such user"))?;
                if u.id == me.id {
                    return Err(err("bad_request", "you cannot report yourself"));
                }
                let profile = self.user_profile(&u.id).await?.unwrap_or_default();
                (
                    None,
                    json!({"user_id": u.id, "user": u.name, "kind": u.kind, "profile": profile}),
                )
            }
            "conversation" => {
                let shard = self
                    .conv_shard(target_id)
                    .map_err(|_| err("not_found", "no such conversation"))?;
                let (c, u) = (target_id.to_string(), me.id.clone());
                let (row, member) = self
                    .read(shard, move |db| {
                        Box::pin(async move { Ok((db.conv(&c).await?, db.member(&c, &u).await?)) })
                    })
                    .await?;
                let row = row.ok_or_else(|| err("not_found", "no such conversation"))?;
                if member.is_none() {
                    return Err(err(
                        "forbidden",
                        "you are not a member of this conversation",
                    ));
                }
                (
                    Some(target_id.to_string()),
                    json!({"conv_id": target_id, "kind": row.kind, "title": row.title}),
                )
            }
            _ => {
                return Err(err(
                    "bad_request",
                    "target_kind must be message, user or conversation",
                ));
            }
        };
        let m = &self.inner.cfg.moderation;
        let target = if target_kind == "user" {
            evidence["user_id"]
                .as_str()
                .unwrap_or(target_id)
                .to_string()
        } else {
            target_id.to_string()
        };
        self.control()
            .call(Op::ReportPut {
                reporter: me.id.clone(),
                kind: target_kind.to_string(),
                target,
                conv_id,
                category: category.to_string(),
                severity: if cat < SEVERE { 2 } else { 1 },
                body: json!({"text": text, "evidence": evidence}),
                due_at: now_ms() + m.due_hours as i64 * 3_600_000,
                per_hour: m.report_per_hour,
            })
            .await
    }

    pub async fn reports(&self, open: bool, limit: i64, offset: i64) -> ApiResult<Vec<Value>> {
        self.read(self.control(), move |db| {
            Box::pin(async move { db.reports(open, limit, offset).await })
        })
        .await
    }

    /// One report with the other reports on the same target.
    pub async fn report_detail(&self, id: &str) -> ApiResult<Option<Value>> {
        let id = id.to_string();
        self.read(self.control(), move |db| {
            Box::pin(async move {
                let Some(mut r) = db.report(&id).await? else {
                    return Ok(None);
                };
                let kind = r["target_kind"].as_str().unwrap_or_default().to_string();
                let target = r["target_id"].as_str().unwrap_or_default().to_string();
                let others: Vec<Value> = db
                    .reports_on(&kind, &target)
                    .await?
                    .into_iter()
                    .filter(|o| o["id"] != r["id"])
                    .collect();
                r["same_target"] = json!(others);
                Ok(Some(r))
            })
        })
        .await
    }

    /// A moderator's decision (DSA art. 17 statement of reasons) for every
    /// open report on the report's target, and its effect.
    #[allow(clippy::too_many_arguments)]
    pub async fn resolve_report(
        &self,
        id: &str,
        action: &str,
        reason: &str,
        ground: Option<&str>,
        automated: bool,
        days: Option<i64>,
        by: &str,
    ) -> ApiResult<Value> {
        if reason.trim().is_empty() || reason.chars().count() > 2000 {
            return Err(err("bad_request", "reason must be 1..2000 characters"));
        }
        let r = self
            .report_detail(id)
            .await?
            .ok_or_else(|| err("not_found", "no such report"))?;
        let has_user = r["evidence"]["user_id"].is_string();
        let (suspend_until, ban) = match action {
            "none" => (None, false),
            "remove_content" => {
                if r["target_kind"] != "message" {
                    return Err(err("bad_request", "remove_content applies to a message"));
                }
                (None, false)
            }
            "suspend_user" => {
                let d = days.ok_or_else(|| err("bad_request", "suspend_user needs days"))?;
                if !(1..=3650).contains(&d) {
                    return Err(err("bad_request", "days must be 1..3650"));
                }
                if !has_user {
                    return Err(err("bad_request", "this report names no user"));
                }
                (Some(now_ms() + d * 86_400_000), false)
            }
            "ban_user" => {
                if !has_user {
                    return Err(err("bad_request", "this report names no user"));
                }
                (None, true)
            }
            _ => {
                return Err(err(
                    "bad_request",
                    "action must be none, remove_content, suspend_user or ban_user",
                ));
            }
        };
        let m = &self.inner.cfg.moderation;
        let decision = json!({
            "action": action,
            "facts": reason,
            "ground": ground,
            "automated": automated,
            "days": days,
            "until": suspend_until,
            "appeal": if m.appeal_url.is_empty() { Value::Null } else { json!(m.appeal_url) },
            "decided_by": by,
            "decided_at": now_ms(),
        });
        let v = self
            .control()
            .call(Op::ReportResolve {
                report_id: id.to_string(),
                decision,
                by: by.to_string(),
                suspend_until,
                ban,
            })
            .await?;
        if let Some(u) = v["user_id"].as_str()
            && (ban || suspend_until.is_some())
        {
            self.inner
                .sh
                .deactivated
                .write()
                .unwrap()
                .insert(u.to_string());
            self.revoke_tokens(u, None).await?;
            self.stop_agents_of(u).await?;
        }
        if action == "remove_content" {
            let msg = r["target_id"].as_str().unwrap_or_default().to_string();
            match self
                .act_as(&self.system_user(), ClientMsg::Recall { msg_id: msg })
                .await
            {
                // Already recalled or gone: the content is not there either way.
                Ok(_) => {}
                Err(e) if e.code == "conflict" || e.code == "not_found" => {}
                Err(e) => return Err(e),
            }
        }
        Ok(json!({"resolved": v["resolved"], "action": action}))
    }

    /// Ends suspensions that are over; deletes old resolved reports.
    pub async fn moderation_upkeep(&self) -> ApiResult<()> {
        let keep = self.inner.cfg.moderation.keep_days as i64 * 86_400_000;
        let v = self
            .control()
            .call(Op::ModerationUpkeep {
                keep_before: now_ms() - keep,
            })
            .await?;
        if let Some(ids) = v["reactivated"].as_array() {
            let mut d = self.inner.sh.deactivated.write().unwrap();
            for id in ids.iter().filter_map(|i| i.as_str()) {
                d.remove(id);
            }
        }
        Ok(())
    }
}
