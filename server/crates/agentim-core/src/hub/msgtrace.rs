//! A message's trace (docs/14 §4.3, D141): what happened to one message,
//! put together from what the databases keep, so it works without a
//! tracing backend and without a dye. Metadata only: never the content.

use std::collections::{HashMap, HashSet};

use serde_json::{Value, json};

use super::*;

/// At most this many devices are listed one by one (the counts are complete).
const LISTED: usize = 100;

impl Hub {
    pub async fn message_trace(&self, msg_id: &str) -> ApiResult<Value> {
        if msg_id.is_empty() || msg_id.len() > 128 {
            return Err(rule!("msg.unknown", "not_found", "message"));
        }
        let hooks: Vec<String> = self
            .inner
            .cfg
            .webhooks
            .iter()
            .map(|w| w.name.clone())
            .collect();
        let mut found = None;
        for s in &self.inner.shards {
            let (id, h) = (msg_id.to_string(), hooks.clone());
            if let Some(f) = self
                .read(s, move |db| {
                    Box::pin(async move { db.msg_facts(&id, &h).await })
                })
                .await?
            {
                found = Some((s.name.clone(), f));
                break;
            }
        }
        let (database, f) = found.ok_or_else(|| rule!("msg.unknown", "not_found", "message"))?;
        let sh = &self.inner.sh;
        let name = |id: &str| {
            sh.user(id)
                .map(|u| u.name)
                .unwrap_or_else(|| id.to_string())
        };
        let m = &f.msg;
        let seq = m.created_seq;

        // Who can see it: members whose visible stretch covers its seq.
        let mut audience: Vec<(String, UserKind)> = f
            .members
            .iter()
            .filter(|x| x.visible_from <= seq)
            .map(|x| (x.user_id.clone(), x.kind))
            .collect();
        let current: HashSet<String> = audience.iter().map(|a| a.0.clone()).collect();
        let mut removed = 0;
        for (u, from, until) in &f.past {
            if *from <= seq && seq <= *until && !current.contains(u) {
                let kind = sh.user(u).map_or(UserKind::Human, |u| u.kind);
                audience.push((u.clone(), kind));
                removed += 1;
            }
        }
        let people = audience.iter().filter(|a| a.1 != UserKind::Agent).count();

        // Delivery: each device of the audience, by its acknowledged position.
        let users: Vec<String> = audience.iter().map(|a| a.0.clone()).collect();
        let devices = self
            .read(self.control(), move |db| {
                Box::pin(async move { db.devices_of(&users).await })
            })
            .await?;
        let acked: HashMap<(&str, &str), (i64, i64)> = f
            .cursors
            .iter()
            .map(|(d, u, s, at)| ((d.as_str(), u.as_str()), (*s, *at)))
            .collect();
        // Devices with a token, then devices with a position whose token is
        // gone (logged out after syncing).
        let mut list: Vec<(&str, &str, Option<&str>, Option<i64>)> = devices
            .iter()
            .map(|d| {
                (
                    d.user_id.as_str(),
                    d.device_id.as_str(),
                    d.platform.as_deref(),
                    d.last_seen,
                )
            })
            .collect();
        let members: HashSet<&str> = audience.iter().map(|a| a.0.as_str()).collect();
        for (d, u, _, _) in &f.cursors {
            if members.contains(u.as_str()) {
                list.push((u.as_str(), d.as_str(), None, None));
            }
        }
        let mut seen = HashSet::new();
        let (mut delivered, mut pending) = (0, vec![]);
        let mut by_user: HashMap<&str, (usize, usize)> = HashMap::new();
        for (user, device, platform, last_seen) in list {
            if !seen.insert((user, device)) {
                continue;
            }
            let pos = acked.get(&(device, user));
            let e = by_user.entry(user).or_default();
            if pos.is_some_and(|p| p.0 >= seq) {
                delivered += 1;
                e.0 += 1;
            } else {
                e.1 += 1;
                pending.push(json!({
                    "user": name(user),
                    "device_id": device,
                    "platform": platform,
                    "acked_seq": pos.map(|p| p.0),
                    "acked_at": pos.map(|p| p.1),
                    "last_seen": last_seen,
                }));
            }
        }
        let undelivered = pending.len();
        pending.truncate(LISTED);

        // Agents that can see it, and what they did about it.
        let agents: Vec<Value> = audience
            .iter()
            .filter(|a| a.1 == UserKind::Agent && a.0 != m.sender_id)
            .map(|(id, _)| {
                let member = f.members.iter().find(|x| &x.user_id == id);
                let (got, missing) = by_user.get(id.as_str()).copied().unwrap_or_default();
                json!({
                    "agent": name(id),
                    "trigger": member.map(|x| x.trigger),
                    "stopped": self.is_stopped(id),
                    "devices_delivered": got,
                    "devices_pending": missing,
                    "replies": f.replies.iter().filter(|r| &r.1 == id).map(|r| &r.0).collect::<Vec<_>>(),
                    "drafts": f.drafts.iter().filter(|d| &d.1 == id).map(|d| &d.0).collect::<Vec<_>>(),
                })
            })
            .collect();
        let replies: Vec<Value> = f
            .replies
            .iter()
            .map(|(id, by, s, state, at)| json!({"msg_id": id, "sender": name(by), "seq": s, "state": state, "at": at}))
            .collect();
        let drafts: Vec<Value> = f
            .drafts
            .iter()
            .map(|(id, by, state, at)| json!({"draft_id": id, "agent": name(by), "state": state, "at": at}))
            .collect();

        // Webhooks: each of its events, for each webhook that takes it.
        let mut webhooks = vec![];
        for (s, kind, _, _) in &f.events {
            for w in &self.inner.cfg.webhooks {
                if !crate::webhook::matches(&w.events, kind) {
                    continue;
                }
                let cursor = f
                    .hook_cursors
                    .iter()
                    .find(|c| c.0 == w.name)
                    .map_or(0, |c| c.1);
                let row = f.outbox.iter().find(|o| o.1 == *s);
                let dropped = f.dropped.iter().any(|d| d.0 == w.name && d.1 == *s);
                let state = match row {
                    Some((id, _)) if *id > cursor => "pending",
                    _ if dropped => "dropped",
                    // A recall takes back what was not sent yet (D91).
                    None if m.state == "recalled" && kind == "msg.created" => {
                        "delivered_or_withdrawn"
                    }
                    _ => "delivered",
                };
                let behind = match row {
                    Some((id, _)) if state == "pending" => f
                        .ahead
                        .iter()
                        .find(|a| a.0 == w.name && a.1 == *id)
                        .map(|a| a.2),
                    _ => None,
                };
                webhooks.push(json!({"webhook": w.name, "seq": s, "type": kind, "state": state, "queued_ahead": behind}));
            }
        }

        let events: Vec<Value> = f
            .events
            .iter()
            .map(|(s, kind, actor, at)| {
                json!({"seq": s, "type": kind, "actor": actor.as_deref().map(name), "at": at})
            })
            .collect();
        let id = msg_id.to_string();
        let reports = self
            .read(self.control(), move |db| {
                Box::pin(async move { db.msg_reports(&id).await })
            })
            .await?;
        let reports: Vec<Value> = reports
            .iter()
            .map(|(id, cat, state)| json!({"id": id, "category": cat, "state": state}))
            .collect();
        let files: Vec<Value> = f
            .files
            .iter()
            .map(|(id, kind, state, size)| json!({"file_id": id, "kind": kind, "state": state, "size": size}))
            .collect();
        let expires_at = sh
            .cfg
            .sync
            .retention
            .map(|r| m.created_at + r.as_millis() as i64);
        let trace_url = match (&f.trace_id, &self.inner.cfg.tracing.trace_ui_url) {
            (Some(t), Some(u)) => Some(u.replace("{trace_id}", t)),
            _ => None,
        };
        Ok(json!({
            "message": {
                "msg_id": m.msg_id,
                "conv_id": m.conv_id,
                "database": database,
                "seq": seq,
                "sender": {"id": m.sender_id, "name": m.sender_name, "type": m.sender_type},
                "on_behalf_of": m.on_behalf_of.as_deref().map(name),
                "client_msg_id": f.client_msg_id,
                "kind": m.kind,
                "state": m.state,
                "state_reason": m.state_reason,
                "version": m.version,
                "hop": m.hop,
                "causation_id": m.causation_id,
                "reply_to": m.reply_to,
                "root_id": m.root_id,
                "created_at": m.created_at,
                "updated_at": m.updated_at,
                "expires_at": expires_at,
            },
            "trace": {"trace_id": f.trace_id, "url": trace_url},
            "events": events,
            "audience": {"people": people, "agents": audience.len() - people, "removed_since": removed, "side_events": f.side},
            "delivery": {"devices": delivered + undelivered, "delivered": delivered, "pending": undelivered, "pending_devices": pending},
            "agents": agents,
            "replies": replies,
            "drafts": drafts,
            "webhooks": webhooks,
            "files": files,
            "reports": reports,
        }))
    }
}
