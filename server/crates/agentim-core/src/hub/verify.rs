//! `agentim verify` (docs/en/design/tracing.md): replays each conversation's log
//! against its stored state and reports what breaks a business invariant.
//! Read-only; safe to run against a live database.

use std::collections::{BTreeMap, HashMap, HashSet};

use serde::Serialize;
use serde_json::{Value, json};

use super::*;
use crate::db::verify::ConvData;

/// One broken invariant.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Finding {
    pub database: String,
    pub conv_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seq: Option<i64>,
    /// The invariant, e.g. `order.gap` (docs/en/design/tracing.md).
    pub check: &'static str,
    pub detail: String,
}

/// Every check the verifier runs, for its docs and tests.
pub const CHECKS: &[(&str, &str)] = &[
    (
        "order.gap",
        "seqs run without gaps from the first kept event to the conversation's last seq",
    ),
    (
        "order.created",
        "the log starts with conv.created, and has it once",
    ),
    (
        "order.orphan",
        "no events, messages or members of a conversation that no longer exists",
    ),
    (
        "member.sender",
        "every message's sender was a member when it was sent",
    ),
    ("member.silenced", "nobody sent while silenced"),
    (
        "member.state",
        "replaying the joins and removals gives the members table",
    ),
    (
        "member.dm",
        "a direct conversation never has had more than two people (one may remain after an account deletion)",
    ),
    (
        "msg.event",
        "every message row has its msg.created event, and every msg.created its row",
    ),
    (
        "msg.after_recall",
        "nothing changes a message after its recall",
    ),
    (
        "msg.editor",
        "only the sender, the sender's owner or the system changes a message; recalls also by an owner or admin",
    ),
    (
        "msg.version",
        "versions only go up, and the row has the last one",
    ),
    ("msg.recalled", "a recalled message's row says recalled"),
    (
        "dedup.target",
        "every remembered client_msg_id names an existing message of that sender",
    ),
    (
        "file.ref",
        "a file is carried by a message of the same conversation sent by its uploader",
    ),
    (
        "stream.stuck",
        "no reply has been streaming for more than an hour",
    ),
    (
        "side.audience",
        "side-channel events go only to members at their anchor",
    ),
    (
        "thread.root",
        "a thread reply's root is a message of the conversation that is not itself a reply",
    ),
    (
        "thread.summary",
        "each thread's reply count and latest reply match its replies that are not recalled",
    ),
    (
        "reaction.state",
        "replaying the reactions (and recalls and erasures) gives the reactions table",
    ),
    (
        "reaction.after_recall",
        "no reaction is added to a message after its recall",
    ),
    (
        "pin.state",
        "replaying the pins (and recalls and erasures) gives the pins table",
    ),
];

const STUCK_MS: i64 = 3_600_000;

impl Hub {
    /// Checks every conversation (or one), on every database. `since`: only
    /// conversations with events at or after this time (ms).
    pub async fn verify(&self, conv: Option<&str>, since: Option<i64>) -> ApiResult<Vec<Finding>> {
        let mut out = vec![];
        for s in &self.inner.shards {
            let ids = match conv {
                Some(c) => vec![c.to_string()],
                None => {
                    self.read(s, move |db| {
                        Box::pin(async move { db.verify_convs(since).await })
                    })
                    .await?
                }
            };
            if conv.is_none() && since.is_none() {
                let orphans = self
                    .read(s, |db| Box::pin(async move { db.verify_orphans().await }))
                    .await?;
                for (table, c) in orphans {
                    out.push(Finding {
                        database: s.name.clone(),
                        conv_id: c,
                        seq: None,
                        check: "order.orphan",
                        detail: format!("{table} rows of a conversation that does not exist"),
                    });
                }
            }
            for id in ids {
                let c = id.clone();
                let data = self
                    .read(s, move |db| {
                        Box::pin(async move { db.verify_data(&c).await })
                    })
                    .await?;
                if let Some(d) = data {
                    out.extend(self.check_conv(&s.name, &id, &d));
                }
            }
        }
        Ok(out)
    }

    fn check_conv(&self, database: &str, conv_id: &str, d: &ConvData) -> Vec<Finding> {
        let mut out = vec![];
        let mut f = |seq: Option<i64>, check: &'static str, detail: String| {
            out.push(Finding {
                database: database.to_string(),
                conv_id: conv_id.to_string(),
                seq,
                check,
                detail,
            })
        };
        let is_system = |id: &str| id == SYSTEM_ID;

        // Order.
        let first = d.events.first().map(|e| e.seq);
        for w in d.events.windows(2) {
            if w[1].seq != w[0].seq + 1 {
                f(
                    Some(w[1].seq),
                    "order.gap",
                    format!("after seq {}", w[0].seq),
                );
            }
        }
        let last = d.events.last().map_or(0, |e| e.seq);
        if last != d.last_seq && !d.events.is_empty() {
            f(
                None,
                "order.gap",
                format!("the log ends at {last}, the conversation at {}", d.last_seq),
            );
        }
        let created: Vec<i64> = d
            .events
            .iter()
            .filter(|e| e.kind == "conv.created")
            .map(|e| e.seq)
            .collect();
        if created.len() > 1 || (first == Some(1) && created != [1]) {
            f(
                None,
                "order.created",
                format!("conv.created at {created:?}"),
            );
        }
        // The rest replays from the start; a log whose start the retention
        // deleted is checked from what is left only where that suffices.
        let full = first == Some(1);

        // Members, roles and silences, replayed.
        let mut members: HashSet<String> = HashSet::new();
        let mut roles: HashMap<String, String> = HashMap::new();
        let mut silenced: HashMap<String, i64> = HashMap::new();
        // Kind and owner of everyone who ever joined (from the join events).
        let mut people: HashMap<String, (String, Option<String>)> = HashMap::new();
        let person = |people: &mut HashMap<String, (String, Option<String>)>, m: &Value| {
            if let Some(u) = m["user_id"].as_str() {
                people.insert(
                    u.to_string(),
                    (
                        m["kind"].as_str().unwrap_or("human").to_string(),
                        m["owner_id"].as_str().map(String::from),
                    ),
                );
            }
        };
        let mut at_seq: BTreeMap<i64, HashSet<String>> = BTreeMap::new();
        // Per message: (recalled, last version seen).
        let mut msgs: HashMap<String, (bool, i64)> = HashMap::new();
        for e in &d.events {
            let user = e.data["user_id"].as_str().map(String::from);
            match e.kind.as_str() {
                "member.joined" => {
                    person(&mut people, &e.data);
                    if let Some(u) = user {
                        roles.insert(
                            u.clone(),
                            e.data["role"].as_str().unwrap_or("member").into(),
                        );
                        members.insert(u);
                    }
                }
                "members.joined" => {
                    for m in e.data["members"].as_array().into_iter().flatten() {
                        person(&mut people, m);
                        if let Some(u) = m["user_id"].as_str() {
                            roles.insert(u.into(), m["role"].as_str().unwrap_or("member").into());
                            members.insert(u.into());
                        }
                    }
                }
                "member.left" | "member.removed" => {
                    if let Some(u) = user {
                        members.remove(&u);
                        roles.remove(&u);
                        silenced.remove(&u);
                    }
                }
                "member.role" => {
                    if let (Some(u), Some(r)) = (user, e.data["role"].as_str()) {
                        roles.insert(u, r.into());
                    }
                }
                "owner.transferred" => {
                    if let (Some(a), Some(b)) = (e.data["from"].as_str(), e.data["to"].as_str()) {
                        roles.insert(a.into(), "admin".into());
                        roles.insert(b.into(), "owner".into());
                    }
                }
                "member.silenced" => {
                    if let (Some(u), Some(until)) = (user, e.data["until"].as_i64()) {
                        silenced.insert(u, until);
                    }
                }
                "member.unsilenced" => {
                    if let Some(u) = user {
                        silenced.remove(&u);
                    }
                }
                _ => {}
            }
            if let Some(m) = &e.msg {
                let entry = msgs.entry(m.msg_id.clone()).or_insert((false, 0));
                let actor = e.actor_id.as_deref().unwrap_or("");
                match e.kind.as_str() {
                    "msg.created" => {
                        if full
                            && !members.contains(&m.sender_id)
                            && m.sender_type != UserKind::System
                        {
                            f(
                                Some(e.seq),
                                "member.sender",
                                format!("{} was not a member", m.sender_id),
                            );
                        }
                        if silenced
                            .get(&m.sender_id)
                            .is_some_and(|u| *u > e.created_at)
                        {
                            f(
                                Some(e.seq),
                                "member.silenced",
                                format!("{} sent while silenced", m.sender_id),
                            );
                        }
                    }
                    kind if entry.0 && kind != "msg.recalled" => {
                        f(
                            Some(e.seq),
                            "msg.after_recall",
                            format!("{kind} on recalled {}", m.msg_id),
                        );
                    }
                    "msg.updated" | "msg.revised" | "msg.recalled" => {
                        let owner = people.get(&m.sender_id).and_then(|p| p.1.as_deref());
                        let human_member = members.contains(actor)
                            && people.get(actor).is_some_and(|p| p.0 == "human");
                        // A stream ends (cancelled, failed, complete) when
                        // any person in the conversation interrupts it.
                        let ends_stream = e.kind == "msg.updated"
                            && m.state != "committed"
                            && m.state != "streaming";
                        let allowed = actor.is_empty()
                            || actor == m.sender_id
                            || owner == Some(actor)
                            || is_system(actor)
                            || (ends_stream && human_member)
                            || (e.kind == "msg.recalled"
                                && matches!(
                                    roles.get(actor).map(String::as_str),
                                    Some("owner" | "admin")
                                ));
                        if full && !allowed {
                            f(Some(e.seq), "msg.editor", format!("{} by {actor}", e.kind));
                        }
                    }
                    _ => {}
                }
                if m.version < entry.1 {
                    f(
                        Some(e.seq),
                        "msg.version",
                        format!("{} went from {} to {}", m.msg_id, entry.1, m.version),
                    );
                }
                entry.1 = entry.1.max(m.version);
                if e.kind == "msg.recalled" {
                    entry.0 = true;
                }
            }
            at_seq.insert(e.seq, members.clone());
        }

        // State against the replay.
        if full {
            let stored: HashSet<String> = d.members.iter().map(|m| m.user_id.clone()).collect();
            for u in stored.difference(&members) {
                f(
                    None,
                    "member.state",
                    format!("{u} is in members but never joined (or left)"),
                );
            }
            for u in members.difference(&stored) {
                f(
                    None,
                    "member.state",
                    format!("{u} joined but is not in members"),
                );
            }
        }
        if d.kind == "dm" && (d.members.len() > 2 || (full && people.len() > 2)) {
            f(
                None,
                "member.dm",
                format!("{} members, {} ever", d.members.len(), people.len()),
            );
        }
        let created_events: HashMap<&str, i64> = d
            .events
            .iter()
            .filter(|e| e.kind == "msg.created")
            .filter_map(|e| e.msg.as_ref().map(|m| (m.msg_id.as_str(), e.seq)))
            .collect();
        let rows: HashMap<&str, &crate::db::verify::MsgRow> =
            d.messages.iter().map(|m| (m.msg_id.as_str(), m)).collect();
        let now = now_ms();
        for m in &d.messages {
            if first.is_some_and(|s| m.created_seq >= s)
                && created_events.get(m.msg_id.as_str()) != Some(&m.created_seq)
            {
                f(
                    Some(m.created_seq),
                    "msg.event",
                    format!("{} has no msg.created at its seq", m.msg_id),
                );
            }
            if let Some((recalled, v)) = msgs.get(&m.msg_id) {
                if *recalled && m.state != "recalled" {
                    f(
                        Some(m.created_seq),
                        "msg.recalled",
                        format!("{} is {}", m.msg_id, m.state),
                    );
                }
                if m.version != *v {
                    f(
                        Some(m.created_seq),
                        "msg.version",
                        format!("{} row {} last event {v}", m.msg_id, m.version),
                    );
                }
            }
            if m.state == "streaming" && now - m.updated_at > STUCK_MS {
                f(
                    Some(m.created_seq),
                    "stream.stuck",
                    format!("{} since {}", m.msg_id, m.updated_at),
                );
            }
        }
        for (id, seq) in &created_events {
            if !rows.contains_key(id) {
                f(
                    Some(*seq),
                    "msg.event",
                    format!("{id} has an event but no row"),
                );
            }
        }
        for (sender, cid, res) in &d.dedup {
            // A message waiting for approval is remembered as its draft.
            if let Some(id) = res["msg_id"].as_str()
                && !id.starts_with("d_")
                && rows.get(id).is_none_or(|m| &m.sender_id != sender)
                && full
            {
                f(
                    None,
                    "dedup.target",
                    format!("{cid} of {sender} names {id}"),
                );
            }
        }
        for (file, uploader, ref_id, _) in &d.files {
            let Some(r) = ref_id else { continue };
            if r.starts_with("d_") {
                continue;
            }
            match rows.get(r.as_str()) {
                None => f(
                    None,
                    "file.ref",
                    format!("{file} names {r}, not a message here"),
                ),
                Some(m) if &m.sender_id != uploader => f(
                    Some(m.created_seq),
                    "file.ref",
                    format!("{file} uploaded by {uploader}, sent by {}", m.sender_id),
                ),
                _ => {}
            }
        }
        self.check_social(d, full, &rows, &mut f);
        if full {
            for (side_seq, anchor, audience) in &d.side {
                let Some((_, at)) = at_seq.range(..=*anchor).next_back() else {
                    continue;
                };
                for u in audience {
                    if !at.contains(u) && !is_system(u) {
                        f(
                            Some(*anchor),
                            "side.audience",
                            format!("side {side_seq} to {u}, not a member then"),
                        );
                    }
                }
            }
        }
        out
    }
}

impl Hub {
    /// Threads, reactions and pins (docs/en/design/messaging.md, MSG-19 to
    /// MSG-21) against the log.
    fn check_social(
        &self,
        d: &ConvData,
        full: bool,
        rows: &HashMap<&str, &crate::db::verify::MsgRow>,
        f: &mut impl FnMut(Option<i64>, &'static str, String),
    ) {
        let first = d.events.first().map_or(1, |e| e.seq);
        // Threads: roots, and the summary against the replies.
        let mut counted: BTreeMap<&str, (i64, i64)> = BTreeMap::new();
        for m in &d.messages {
            let Some(t) = m.thread_id.as_deref() else {
                continue;
            };
            match rows.get(t) {
                Some(r) if r.thread_id.is_some() => f(
                    Some(m.created_seq),
                    "thread.root",
                    format!("{} replies in {t}, itself a reply", m.msg_id),
                ),
                Some(r) if r.created_seq >= m.created_seq => f(
                    Some(m.created_seq),
                    "thread.root",
                    format!("{} replies in {t}, which came later", m.msg_id),
                ),
                // A root the retention deleted is fine.
                None if full => f(
                    Some(m.created_seq),
                    "thread.root",
                    format!("{} replies in {t}, not a message here", m.msg_id),
                ),
                _ => {}
            }
            if m.state != "recalled" {
                let c = counted.entry(t).or_insert((0, 0));
                c.0 += 1;
                c.1 = c.1.max(m.created_seq);
            }
        }
        for t in &d.threads {
            let (n, last) = counted.get(t.root_id.as_str()).copied().unwrap_or((0, 0));
            // With retention, replies older than the kept log are gone.
            let exact = full || rows.contains_key(t.root_id.as_str());
            if exact && (t.replies != n || (n > 0 && t.last_seq != last)) {
                f(
                    None,
                    "thread.summary",
                    format!(
                        "{}: {} replies up to {}, stored {} up to {}",
                        t.root_id, n, last, t.replies, t.last_seq
                    ),
                );
            }
        }
        for (t, (n, _)) in &counted {
            if *n > 0 && !d.threads.iter().any(|x| x.root_id == *t) {
                f(
                    None,
                    "thread.summary",
                    format!("{t} has replies but no summary"),
                );
            }
        }
        // Reactions and pins, replayed.
        let mut senders: HashMap<String, String> = HashMap::new();
        let mut recalled: HashSet<String> = HashSet::new();
        let mut reactions: HashSet<(String, String, String)> = HashSet::new();
        let mut pins: HashSet<String> = HashSet::new();
        for e in &d.events {
            let msg = e.data["msg_id"].as_str().map(String::from);
            let key = e.data["key"].as_str().map(String::from);
            let user = e.data["user_id"].as_str().map(String::from);
            match e.kind.as_str() {
                "msg.created" => {
                    if let Some(m) = &e.msg {
                        senders.insert(m.msg_id.clone(), m.sender_id.clone());
                    }
                }
                "msg.recalled" => {
                    if let Some(m) = &e.msg {
                        recalled.insert(m.msg_id.clone());
                        reactions.retain(|r| r.0 != m.msg_id);
                        pins.remove(&m.msg_id);
                    }
                }
                "reaction.added" => {
                    if let (Some(m), Some(u), Some(k)) = (msg, user, key) {
                        if recalled.contains(&m) {
                            f(
                                Some(e.seq),
                                "reaction.after_recall",
                                format!("{k} on recalled {m}"),
                            );
                        }
                        reactions.insert((m, u, k));
                    }
                }
                "reaction.removed" => {
                    if let (Some(m), Some(u), Some(k)) = (msg, user, key) {
                        reactions.remove(&(m, u, k));
                    }
                }
                "msg.pinned" => {
                    if let Some(m) = msg {
                        pins.insert(m);
                    }
                }
                "msg.unpinned" => {
                    if let Some(m) = msg {
                        pins.remove(&m);
                    }
                }
                "user.erased" => {
                    if let Some(u) = user {
                        reactions.retain(|r| r.1 != u && senders.get(&r.0).is_none_or(|s| *s != u));
                        pins.retain(|p| senders.get(p).is_none_or(|s| *s != u));
                    }
                }
                _ => {}
            }
        }
        // Messages the retention deleted take what derives from them along.
        let kept = |m: &str| rows.get(m).is_some_and(|r| r.created_seq >= first);
        let stored: HashSet<(String, String, String)> = d.reactions.iter().cloned().collect();
        if full || !d.events.is_empty() {
            for r in stored.symmetric_difference(&reactions) {
                if !full && !kept(&r.0) {
                    continue;
                }
                let (what, side) = if stored.contains(r) {
                    ("stored but not in the log", &r.2)
                } else {
                    ("in the log but not stored", &r.2)
                };
                f(
                    None,
                    "reaction.state",
                    format!("{side} by {} on {}: {what}", r.1, r.0),
                );
            }
            let stored: HashSet<String> = d.pins.iter().cloned().collect();
            for p in stored.symmetric_difference(&pins) {
                if !full && !kept(p) {
                    continue;
                }
                let what = if stored.contains(p) {
                    "pinned but not in the log"
                } else {
                    "pinned in the log but not stored"
                };
                f(None, "pin.state", format!("{p}: {what}"));
            }
        }
    }
}

/// The findings as the CLI prints them.
pub fn report(findings: &[Finding]) -> Value {
    json!({"ok": findings.is_empty(), "findings": findings})
}
