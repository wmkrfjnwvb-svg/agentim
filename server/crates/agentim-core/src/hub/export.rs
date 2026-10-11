//! Moving an agent to another server (docs/09 §11.4): an owner exports an
//! agent package (JSON) and imports it elsewhere as a new agent. Never
//! exported: keys, credentials, grants. Conversation content is included
//! only on request and only where the owner is a member too (D46).

use serde_json::{Map, Value, json};

use super::history::agent_view;
use super::{Hub, err};
use crate::logic::Op;
use crate::model::*;

pub const FORMAT: &str = "agentim.agent/1";
/// Room left for JSON around a chunk of archived events.
const CHUNK_BYTES: usize = 900 * 1024;
const PULL_BATCH: i64 = 1000;

fn sha256_hex(bytes: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, bytes)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The hash covers the package without its `sha256` field.
fn digest(pkg: &Value) -> String {
    let mut p = pkg.clone();
    if let Some(o) = p.as_object_mut() {
        o.remove("sha256");
    }
    sha256_hex(p.to_string().as_bytes())
}

fn bad(msg: impl Into<String>) -> ApiError {
    err("bad_request", msg)
}

impl Hub {
    pub async fn export_agent(
        &self,
        me: &User,
        agent_id: &str,
        with_conversations: bool,
    ) -> ApiResult<Value> {
        let agent = self.live_agent(me, agent_id)?;
        // Checkpoints: current versions.
        let mut state = vec![];
        let mut after = String::new();
        loop {
            let (a, from) = (agent.id.clone(), after.clone());
            let (keys, more) = self
                .read(self.control(), move |db| {
                    Box::pin(async move { db.list_state(&a, "", &from, 500).await })
                })
                .await?;
            for (k, _, _) in &keys {
                let (a, key) = (agent.id.clone(), k.clone());
                if let Some((v, value, at, _)) = self
                    .read(self.control(), move |db| {
                        Box::pin(async move { db.state(&a, &key, None).await })
                    })
                    .await?
                {
                    state.push(json!({"key": k, "version": v, "value": value, "updated_at": at}));
                }
            }
            match keys.last() {
                Some((k, _, _)) if more => after = k.clone(),
                _ => break,
            }
        }
        let convs = self.convs_of(&agent.id).await?;
        let mut summaries = vec![];
        let mut conversations = vec![];
        for c in &convs {
            let Ok(shard) = self.conv_shard(c) else {
                continue;
            };
            let (cid, a, o) = (c.clone(), agent.id.clone(), me.id.clone());
            let (row, mine, owner_in) = self
                .read(shard, move |db| {
                    Box::pin(async move {
                        Ok((
                            db.conv(&cid).await?,
                            db.summaries_of(&cid, &a).await?,
                            db.member(&cid, &o).await?.is_some(),
                        ))
                    })
                })
                .await?;
            let Some(row) = row else { continue };
            for s in mine {
                let mut s = s;
                s["conv_id"] = json!(c);
                s["conv_title"] = json!(row.title);
                summaries.push(s);
            }
            if !(with_conversations && owner_in) {
                continue;
            }
            let member = self.reader_member(&agent, c, None).await?;
            let mut events = vec![];
            let mut pos = member.visible_from - 1;
            loop {
                let (cid, m, sh) = (c.clone(), member.clone(), self.inner.sh.clone());
                let (batch, last, done) = self
                    .read(shard, move |db| {
                        Box::pin(async move {
                            let evs = db.pull(&cid, pos, PULL_BATCH).await?;
                            let done = (evs.len() as i64) < PULL_BATCH;
                            let last = evs.last().map(|e| e.seq).unwrap_or(pos);
                            let mut out = vec![];
                            for e in evs {
                                if let Some(v) = agent_view(db, &sh, &m, e).await? {
                                    out.push(v);
                                }
                            }
                            Ok((out, last, done))
                        })
                    })
                    .await?;
                for v in batch {
                    let ev = &v["event"];
                    let kind = ev["type"].as_str().unwrap_or("");
                    if !matches!(kind, "msg.created" | "msg.edited") {
                        continue;
                    }
                    let msg = &ev["msg"];
                    events.push(json!({
                        "seq": ev["seq"],
                        "type": kind,
                        "msg_id": msg["msg_id"],
                        "sender": msg["sender_name"],
                        "sender_type": msg["sender_type"],
                        "content": msg["content"],
                        "created_at": ev["created_at"],
                    }));
                }
                pos = last;
                if done {
                    break;
                }
            }
            conversations.push(
                json!({"conv_id": c, "title": row.title, "kind": row.kind, "events": events}),
            );
        }
        let owner = self.user(&me.id).map(|u| u.name).unwrap_or_default();
        let mut pkg = json!({
            "format": FORMAT,
            "source": {"server": crate::protocol::server_info(self.edition()), "exported_at": now_ms()},
            "agent": {"id": agent.id, "name": agent.name, "owner": owner},
            "state": state,
            "summaries": summaries,
            "conversations": if with_conversations { json!(conversations) } else { Value::Null },
        });
        pkg["sha256"] = json!(digest(&pkg));
        self.audit_user(
            me,
            "agent.exported",
            &agent.id,
            json!({"state": pkg["state"].as_array().map_or(0, Vec::len), "summaries": pkg["summaries"].as_array().map_or(0, Vec::len), "conversations": conversations.len()}),
        )
        .await;
        Ok(pkg)
    }

    /// Imports a package as a new agent of `me`: checkpoints as they were
    /// (version 1), summaries and conversation content as read-only
    /// `archive/` checkpoints. Returns the new agent's key.
    pub async fn import_agent(
        &self,
        me: &User,
        pkg: &Value,
        name: Option<&str>,
    ) -> ApiResult<Value> {
        if pkg["format"] != FORMAT {
            return Err(bad(format!("not an agent package ({FORMAT})")));
        }
        if pkg["sha256"].as_str() != Some(digest(pkg).as_str()) {
            return Err(bad("the package was changed: its sha256 does not match"));
        }
        let name = match name {
            Some(n) => n.to_string(),
            None => pkg["agent"]["name"]
                .as_str()
                .ok_or_else(|| bad("the package has no agent name"))?
                .to_string(),
        };
        if !crate::open_api::valid_user_id(&name) {
            return Err(bad(
                "agent names are 1..64 of A-Z a-z 0-9 _ - . @ : and do not start with _",
            ));
        }
        // Everything to write, checked before anything is created.
        let mut writes: Vec<(String, String)> = vec![];
        for s in pkg["state"]
            .as_array()
            .ok_or_else(|| bad("state must be a list"))?
        {
            let key = s["key"]
                .as_str()
                .ok_or_else(|| bad("a state entry has no key"))?;
            if key.is_empty()
                || key.len() > super::state::KEY_MAX
                || key.chars().any(char::is_control)
            {
                return Err(bad(format!("invalid key {key:?}")));
            }
            let value = s["value"].to_string();
            if value.len() > super::state::VALUE_MAX {
                return Err(bad(format!("{key} is over 1 MiB")));
            }
            writes.push((key.to_string(), value));
        }
        let source = pkg["source"]["server"]["name"]
            .as_str()
            .unwrap_or("source")
            .to_string();
        let summaries = pkg["summaries"].as_array().cloned().unwrap_or_default();
        if !summaries.is_empty() {
            for (i, chunk) in chunks(&summaries).into_iter().enumerate() {
                writes.push((format!("{}summaries/{i}", super::state::ARCHIVE), chunk));
            }
        }
        for c in pkg["conversations"].as_array().into_iter().flatten() {
            let id = c["conv_id"]
                .as_str()
                .ok_or_else(|| bad("a conversation has no id"))?;
            let events = c["events"].as_array().cloned().unwrap_or_default();
            let mut meta = Map::new();
            meta.insert("conv_id".into(), json!(id));
            meta.insert("title".into(), c["title"].clone());
            meta.insert("kind".into(), c["kind"].clone());
            meta.insert("source".into(), json!(source));
            writes.push((
                format!("{}conv/{id}/meta", super::state::ARCHIVE),
                Value::Object(meta).to_string(),
            ));
            for (i, chunk) in chunks(&events).into_iter().enumerate() {
                writes.push((format!("{}conv/{id}/{i}", super::state::ARCHIVE), chunk));
            }
        }
        for (k, _) in &writes {
            if k.len() > super::state::KEY_MAX {
                return Err(bad(format!("key too long after import: {k}")));
            }
        }
        let max = self.inner.cfg.agents.state_keys as usize;
        if writes.len() > max {
            return Err(err(
                "rate_limited",
                format!(
                    "{} keys; at most {max} per agent (agents.state_keys)",
                    writes.len()
                ),
            ));
        }
        let made = self.create_agent(me, &name).await?;
        let agent_id = made["agent"]["id"]
            .as_str()
            .or_else(|| made["agent_id"].as_str())
            .unwrap_or_default()
            .to_string();
        for (key, value) in &writes {
            if let Err(e) = self
                .control()
                .call(Op::StatePut {
                    agent_id: agent_id.clone(),
                    key: key.clone(),
                    value: value.clone(),
                    version: 0,
                    max_keys: max as i64,
                })
                .await
            {
                // No half-imported agent stays behind.
                let _ = self.destroy_agent(me, &agent_id, &name).await;
                return Err(e);
            }
        }
        self.audit_user(
            me,
            "agent.imported",
            &agent_id,
            json!({"from": pkg["agent"]["id"], "keys": writes.len()}),
        )
        .await;
        let mut out = made;
        out["imported"] = json!({
            "state": pkg["state"].as_array().map_or(0, Vec::len),
            "summaries": summaries.len(),
            "conversations": pkg["conversations"].as_array().map_or(0, Vec::len),
        });
        Ok(out)
    }
}

/// Splits items into JSON arrays of at most CHUNK_BYTES each.
fn chunks(items: &[Value]) -> Vec<String> {
    let mut out = vec![];
    let mut cur: Vec<&Value> = vec![];
    let mut size = 2;
    for it in items {
        let n = it.to_string().len() + 1;
        if !cur.is_empty() && size + n > CHUNK_BYTES {
            out.push(serde_json::to_string(&cur).unwrap_or_default());
            cur.clear();
            size = 2;
        }
        cur.push(it);
        size += n;
    }
    if !cur.is_empty() {
        out.push(serde_json::to_string(&cur).unwrap_or_default());
    }
    out
}
