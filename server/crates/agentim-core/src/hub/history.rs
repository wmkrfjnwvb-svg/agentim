//! The agent history service, read side (docs/03-agents.md §7.1): an agent reads
//! what it may see of a conversation by seq, as a context that fits a token
//! budget, or by searching. People sync instead (docs/08).

use std::time::Instant;

use serde_json::{Value, json};

use super::{Hub, err};
use crate::db::Db;
use crate::logic::{Ctx, Shared};
use crate::model::*;

pub const HISTORY_MAX: i64 = 200;
const HISTORY_DEFAULT: i64 = 50;
const BATCH: i64 = 200;
/// Most events one read looks at, visible or not.
const SCAN_CAP: usize = 5000;
const SEARCH_CONVS: usize = 20;
const SEARCH_MAX: i64 = 50;
const SEARCH_DEFAULT: i64 = 20;
const EXCERPT: usize = 80;
const CONTEXT_DEFAULT: i64 = 8000;
const CONTEXT_MIN: i64 = 256;
const CONTEXT_MAX: i64 = 200_000;

/// Where a scan starts: just after a seq going up, or just before one
/// going down.
#[derive(Clone, Copy)]
enum From {
    After(i64),
    Before(i64),
}

/// What a scan found: events oldest first, the seq range looked at, and
/// whether more may follow in the direction of the scan.
struct Scan {
    events: Vec<Value>,
    first: i64,
    last: i64,
    more: bool,
}

/// An event as the agent `m` receives it, or None when it may not see it.
pub(super) async fn agent_view(
    db: &mut Db<'_>,
    sh: &Shared,
    m: &Member,
    e: Event,
) -> ApiResult<Option<Value>> {
    if !m.sees(&e) || m.kind == UserKind::Agent && !Ctx::agent_can_see(db, m, &e).await? {
        return Ok(None);
    }
    let e = e.for_reader(&m.user_id).into_owned();
    let mut v = json!({"type": "event", "event": e});
    if m.kind == UserKind::Agent
        && e.kind == "msg.created"
        && let Some(msg) = &e.msg
        && msg.sender_id != m.user_id
    {
        let (t, flags) = sh.policy.inbound(
            &crate::logic::scan_text(&msg.content, msg.payload.as_ref()),
            msg.sender_type,
        );
        v["trust"] = json!(t.as_str());
        v["flags"] = json!(flags);
    }
    Ok(Some(v))
}

/// A rough token count (4 bytes a token) of one event for `context`.
fn tokens(v: &Value) -> i64 {
    let text = v["event"]["msg"]["content"].as_str().map_or(0, str::len);
    (text as i64 + 64) / 4
}

/// `text` around the first match of `needle` (lowercase), on char
/// boundaries.
/// Search filters are checked before anything is read.
fn check_filter(f: &SearchFilter) -> ApiResult<()> {
    if let Some(t) = &f.sender_type
        && t != "human"
        && t != "agent"
    {
        return Err(err("bad_request", "sender_type must be human or agent"));
    }
    if let (Some(a), Some(b)) = (f.since, f.until)
        && a > b
    {
        return Err(err("bad_request", "since is after until"));
    }
    for (name, v) in [
        ("sender_id", &f.sender_id),
        ("kind", &f.kind),
        ("intent", &f.intent),
        ("topic", &f.topic),
    ] {
        if v.as_ref().is_some_and(|v| v.is_empty() || v.len() > 64) {
            return Err(err("bad_request", format!("{name} must be 1..64 bytes")));
        }
    }
    Ok(())
}

/// Whether a message (its latest version) matches the filters.
fn passes(f: &SearchFilter, msg: &Value) -> bool {
    let env = &msg["envelope"];
    let at = msg["created_at"].as_i64();
    f.sender_id
        .as_ref()
        .is_none_or(|s| msg["sender_id"] == s.as_str())
        && f.sender_type
            .as_ref()
            .is_none_or(|s| msg["sender_type"] == s.as_str())
        && f.kind.as_ref().is_none_or(|s| msg["kind"] == s.as_str())
        && f.since.is_none_or(|t| at.is_some_and(|c| c >= t))
        && f.until.is_none_or(|t| at.is_some_and(|c| c <= t))
        && f.intent
            .as_ref()
            .is_none_or(|s| env["intent"] == s.as_str())
        && f.topic.as_ref().is_none_or(|s| {
            env["topics"]
                .as_array()
                .is_some_and(|a| a.iter().any(|x| x == s.as_str()))
        })
        && f.has_refs
            .is_none_or(|want| env["refs"].as_array().is_some_and(|a| !a.is_empty()) == want)
}

fn excerpt(text: &str, needle: &str) -> Option<String> {
    let lower = text.to_lowercase();
    let at = lower.find(needle)?;
    // Lowercasing can change byte lengths; map back by chars.
    let start_chars = lower[..at].chars().count();
    let len_chars = needle.chars().count();
    let chars: Vec<char> = text.chars().collect();
    let from = start_chars.saturating_sub(EXCERPT);
    let to = (start_chars + len_chars + EXCERPT).min(chars.len());
    let mut s: String = chars[from..to].iter().collect();
    if from > 0 {
        s.insert(0, '…');
    }
    if to < chars.len() {
        s.push('…');
    }
    Some(s)
}

impl Hub {
    fn agents_only(me: &User) -> ApiResult<()> {
        if me.kind != UserKind::Agent {
            return Err(err(
                "forbidden",
                "reading history is for agents; people sync (docs/08)",
            ));
        }
        Ok(())
    }

    /// Reads per agent per minute (`history` covers `context` too).
    fn read_allowed(&self, agent: &str, kind: &'static str) -> ApiResult<()> {
        let cfg = &self.inner.cfg.agents;
        let n = if kind == "search" {
            cfg.search_per_minute
        } else {
            cfg.history_per_minute
        } as f64;
        let now = Instant::now();
        let mut b = self.inner.read_buckets.lock().unwrap();
        let (tokens, at) = b.entry((agent.to_string(), kind)).or_insert((n, now));
        *tokens = (*tokens + now.duration_since(*at).as_secs_f64() * n / 60.0).min(n);
        *at = now;
        if *tokens < 1.0 {
            return Err(err(
                "rate_limited",
                format!("too many {kind} reads (at most {n} a minute)"),
            ));
        }
        *tokens -= 1.0;
        Ok(())
    }

    /// Visible events of `conv` for `m` from a position in one direction,
    /// until `want` are found, the budget of tokens is spent, the log (or
    /// the agent's visible part of it) ends, or SCAN_CAP events were looked
    /// at. Going down never passes `floor`.
    async fn scan(
        &self,
        m: &Member,
        conv: &str,
        from: From,
        want: usize,
        budget: Option<i64>,
        floor: i64,
    ) -> ApiResult<Scan> {
        let shard = self.conv_shard(conv)?;
        let (m, conv, sh) = (m.clone(), conv.to_string(), self.inner.sh.clone());
        self.read(shard, move |db| {
            Box::pin(async move {
                let floor = floor.max(m.visible_from - 1);
                let mut out = vec![];
                let mut spent = 0i64;
                let mut looked = 0usize;
                let (mut first, mut last) = (i64::MAX, i64::MIN);
                let mut pos = match from {
                    From::After(a) => a.max(floor),
                    From::Before(b) => b,
                };
                let more = 'scan: loop {
                    let batch = match from {
                        From::After(_) => db.pull(&conv, pos, BATCH).await?,
                        From::Before(_) => db.pull_before(&conv, pos, floor, BATCH).await?,
                    };
                    let exhausted = (batch.len() as i64) < BATCH;
                    let n = batch.len();
                    for (i, e) in batch.into_iter().enumerate() {
                        if looked >= SCAN_CAP {
                            break 'scan true;
                        }
                        let seq = e.seq;
                        let Some(v) = agent_view(db, &sh, &m, e).await? else {
                            looked += 1;
                            first = first.min(seq);
                            last = last.max(seq);
                            pos = seq;
                            continue;
                        };
                        if let Some(b) = budget {
                            let t = tokens(&v);
                            if spent + t > b {
                                break 'scan true;
                            }
                            spent += t;
                        }
                        looked += 1;
                        first = first.min(seq);
                        last = last.max(seq);
                        pos = seq;
                        out.push(v);
                        if out.len() >= want {
                            break 'scan i + 1 < n || !exhausted;
                        }
                    }
                    if exhausted {
                        break false;
                    }
                };
                if matches!(from, From::Before(_)) {
                    out.reverse();
                }
                if first == i64::MAX {
                    (first, last) = (0, 0);
                }
                Ok(Scan {
                    events: out,
                    first,
                    last,
                    more,
                })
            })
        })
        .await
    }
}

impl Hub {
    pub(super) async fn history(
        &self,
        me: &User,
        conv_id: &str,
        after_seq: Option<i64>,
        before_seq: Option<i64>,
        limit: Option<i64>,
    ) -> ApiResult<Value> {
        Self::agents_only(me)?;
        let limit = limit.unwrap_or(HISTORY_DEFAULT);
        if !(1..=HISTORY_MAX).contains(&limit) {
            return Err(err("bad_request", "limit must be 1..200"));
        }
        let from = match (after_seq, before_seq) {
            (Some(_), Some(_)) => {
                return Err(err("bad_request", "give after_seq or before_seq, not both"));
            }
            (Some(a), None) if a >= 0 => From::After(a),
            (None, Some(b)) if b >= 1 => From::Before(b),
            (None, None) => From::Before(i64::MAX),
            _ => {
                return Err(err(
                    "bad_request",
                    "after_seq must be >= 0 and before_seq >= 1",
                ));
            }
        };
        let m = self.reader_member(me, conv_id, None).await?;
        self.read_allowed(&me.id, "history")?;
        let s = self
            .scan(&m, conv_id, from, limit as usize, None, 0)
            .await?;
        self.audit_read(
            me,
            conv_id,
            "agent.history",
            json!({"from_seq": s.first, "to_seq": s.last, "count": s.events.len()}),
        )
        .await?;
        Ok(
            json!({"conv_id": conv_id, "events": s.events, "has_more": s.more, "first_seq": s.first, "last_seq": s.last}),
        )
    }

    pub(super) async fn context(
        &self,
        me: &User,
        conv_id: &str,
        max_tokens: Option<i64>,
    ) -> ApiResult<Value> {
        Self::agents_only(me)?;
        let budget = max_tokens.unwrap_or(CONTEXT_DEFAULT);
        if !(CONTEXT_MIN..=CONTEXT_MAX).contains(&budget) {
            return Err(err("bad_request", "max_tokens must be 256..200000"));
        }
        let m = self.reader_member(me, conv_id, None).await?;
        self.read_allowed(&me.id, "history")?;
        let shard = self.conv_shard(conv_id)?;
        let (c, a) = (conv_id.to_string(), me.id.clone());
        let summary = self
            .read(shard, move |db| {
                Box::pin(async move { db.latest_summary(&c, &a).await })
            })
            .await?;
        let mut left = budget;
        let mut floor = 0;
        if let Some(s) = &summary {
            left -= (s["text"].as_str().map_or(0, str::len) as i64 + 64) / 4;
            floor = s["to_seq"].as_i64().unwrap_or(0);
        }
        let s = self
            .scan(
                &m,
                conv_id,
                From::Before(i64::MAX),
                usize::MAX,
                Some(left.max(0)),
                floor,
            )
            .await?;
        self.audit_read(
            me,
            conv_id,
            "agent.context",
            json!({"from_seq": s.first, "to_seq": s.last, "count": s.events.len(), "summary": summary.as_ref().map(|s| s["summary_id"].clone())}),
        )
        .await?;
        Ok(
            json!({"conv_id": conv_id, "summary": summary, "events": s.events, "truncated": s.more, "max_tokens": budget}),
        )
    }

    pub(super) async fn search(
        &self,
        me: &User,
        query: &str,
        conv_id: Option<&str>,
        limit: Option<i64>,
        filter: &SearchFilter,
    ) -> ApiResult<Value> {
        Self::agents_only(me)?;
        check_filter(filter)?;
        let limit = limit.unwrap_or(SEARCH_DEFAULT);
        if !(1..=SEARCH_MAX).contains(&limit) {
            return Err(err("bad_request", "limit must be 1..50"));
        }
        let q = query.trim();
        if q.len() < 2 || q.len() > 200 || q.contains('\0') {
            return Err(err("bad_request", "query must be 2..200 bytes"));
        }
        let needle = q.to_lowercase();
        let mut convs: Vec<(String, Member)> = vec![];
        let mut complete = true;
        match conv_id {
            Some(c) => convs.push((c.to_string(), self.reader_member(me, c, None).await?)),
            None => {
                let mine = self.my_convs_by_activity(me).await?;
                complete = mine.len() <= SEARCH_CONVS;
                for c in mine.into_iter().take(SEARCH_CONVS) {
                    if let Ok(m) = self.reader_member(me, &c, None).await {
                        convs.push((c, m));
                    }
                }
            }
        }
        self.read_allowed(&me.id, "search")?;
        let mut hits = vec![];
        let mut found_in = vec![];
        for (conv, m) in &convs {
            let s = self
                .scan(m, conv, From::Before(i64::MAX), usize::MAX, None, 0)
                .await?;
            complete &= !s.more;
            let before = hits.len();
            // Newest first: the latest version of each message decides
            // (edits and completed streams are `msg.updated`, M4).
            let mut seen = std::collections::HashSet::new();
            for v in s.events.iter().rev() {
                let ev = &v["event"];
                if !matches!(
                    ev["type"].as_str(),
                    Some("msg.created" | "msg.updated" | "msg.recalled")
                ) || !seen.insert(ev["msg"]["msg_id"].to_string())
                {
                    continue;
                }
                let Some(text) = ev["msg"]["content"].as_str() else {
                    continue;
                };
                if !passes(filter, &ev["msg"]) {
                    continue;
                }
                if let Some(x) = excerpt(text, &needle) {
                    hits.push(json!({"conv_id": conv, "seq": ev["msg"]["created_seq"], "msg_id": ev["msg"]["msg_id"], "sender_id": ev["msg"]["sender_id"], "sender_name": ev["msg"]["sender_name"], "sender_type": ev["msg"]["sender_type"], "excerpt": x, "trust": v["trust"]}));
                    if hits.len() as i64 >= limit {
                        break;
                    }
                }
            }
            if hits.len() > before {
                found_in.push(conv.clone());
            }
            if hits.len() as i64 >= limit {
                complete = false;
                break;
            }
        }
        for c in &found_in {
            self.audit_read(
                me,
                c,
                "agent.search",
                json!({"hits": hits.iter().filter(|h| h["conv_id"] == json!(c)).count()}),
            )
            .await?;
        }
        Ok(json!({"hits": hits, "complete": complete}))
    }

    /// The agent's conversations, most recently active first.
    async fn my_convs_by_activity(&self, me: &User) -> ApiResult<Vec<String>> {
        let ids = vec![me.id.clone()];
        let pairs = self
            .read(self.control(), move |db| {
                Box::pin(async move { db.member_index_for(&ids).await })
            })
            .await?;
        let mut out = vec![];
        for (conv, _) in pairs {
            let Ok(shard) = self.conv_shard(&conv) else {
                continue;
            };
            let c = conv.clone();
            let at = self
                .read(shard, move |db| {
                    Box::pin(async move { db.last_event_at(&c).await })
                })
                .await?;
            out.push((at, conv));
        }
        out.sort_by(|a, b| b.cmp(a));
        Ok(out.into_iter().map(|(_, c)| c).collect())
    }

    async fn audit_read(
        &self,
        me: &User,
        conv: &str,
        action: &str,
        detail: Value,
    ) -> ApiResult<()> {
        self.conv_shard(conv)?
            .call(crate::logic::Op::Audit {
                actor: me.id.clone(),
                action: action.into(),
                conv_id: conv.to_string(),
                target: None,
                detail,
            })
            .await
            .map(|_| ())
    }
}
