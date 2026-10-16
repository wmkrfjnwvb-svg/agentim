//! Sync-only clients (docs/08-sync-and-client.md): the server keeps each
//! device's position per conversation, sends what follows it in batches,
//! and moves it forward when the device acknowledges what it stored.

use std::collections::HashMap;
use std::time::Instant;

use serde_json::{Value, json};

use super::{Hub, Session, err};
use crate::logic::{Ctx, Op};
use crate::model::*;

/// Events per sync call across conversations (default and maximum).
const SYNC_LIMIT: i64 = 1000;
const SYNC_LIMIT_MAX: i64 = 5000;
/// A dissolved conversation is reported once per session.
const REPORTED: i64 = i64::MAX;

/// Where a device stands in one conversation when a sync call starts.
struct ConvState {
    conv_id: String,
    member: Option<Member>,
    last_seq: i64,
    /// Oldest seq still stored.
    floor: i64,
    /// Acknowledged (seq, side seq), if the device has a position.
    acked: Option<(i64, i64)>,
    /// When the member joined.
    joined_at: Option<i64>,
    read_seq: i64,
    /// Departed members get events up to this seq only.
    until: Option<i64>,
}

impl Hub {
    /// One sync round for a session (`sync`).
    pub(super) async fn sync(&self, sess: &Session, limit: Option<i64>) -> ApiResult<Value> {
        let me = &sess.user;
        sess.sync_allowed(&self.inner.sh.cfg.sync)?;
        let sh = &self.inner.sh;
        sh.reg.sync_enable(sess.sid);
        let cfg = sh.cfg.sync.clone();
        let mut budget = limit.unwrap_or(SYNC_LIMIT);
        if !(1..=SYNC_LIMIT_MAX).contains(&budget) {
            return Err(err(
                "bad_request",
                format!("limit must be 1..{SYNC_LIMIT_MAX}"),
            ));
        }
        let device_start = self.device_start(sess).await?;
        let silenced = self.read_silences(me).await?;
        let me_id = me.id.clone();
        let dev = sess.device_id.clone();
        let (convs, departures, told) = self
            .read(self.control(), move |db| {
                Box::pin(async move {
                    let ids = vec![me_id.clone()];
                    let convs: Vec<String> = db
                        .member_index_for(&ids)
                        .await?
                        .into_iter()
                        .map(|(c, _)| c)
                        .collect();
                    let deps = db.departures(&me_id).await?;
                    // Dissolutions this device was already told about.
                    let mut told = std::collections::HashSet::new();
                    if let Some(d) = &dev {
                        for (c, _, dissolved) in &deps {
                            if *dissolved && db.cursor(d, c).await?.is_some() {
                                told.insert(c.clone());
                            }
                        }
                    }
                    Ok((convs, deps, told))
                })
            })
            .await?;
        let now = now_ms();
        let mut out = vec![];
        let mut more = false;
        let mut targets: Vec<(String, Option<i64>)> =
            convs.into_iter().map(|c| (c, None)).collect();
        for (conv, seq, dissolved) in departures {
            if dissolved {
                if sh.reg.sync_sent(sess.sid, &conv).0 != Some(REPORTED) && !told.contains(&conv) {
                    sh.reg.sync_mark(sess.sid, &conv, REPORTED, REPORTED);
                    if let Some(d) = &sess.device_id {
                        self.control()
                            .call(Op::DissolvedSeen {
                                device_id: d.clone(),
                                user_id: me.id.clone(),
                                conv_id: conv.clone(),
                            })
                            .await?;
                    }
                    out.push(json!({"conv_id": conv, "dissolved": true}));
                }
                continue;
            }
            if !targets.iter().any(|(c, _)| *c == conv) {
                targets.push((conv, Some(seq)));
            }
        }
        for (conv, until) in targets {
            if budget == 0 {
                more = true;
                break;
            }
            let Ok(shard) = self.conv_shard(&conv) else {
                continue;
            };
            let st = self.conv_state(sess, &conv, until).await?;
            let Some(st) = st else { continue };
            // Departed: only devices that had the conversation catch up.
            if st.member.is_none()
                && st.acked.is_none()
                && sh.reg.sync_sent(sess.sid, &conv).0.is_none()
            {
                continue;
            }
            // From reading what was sent until the response is recorded, live
            // pushes of this conversation wait (they could repeat an event
            // the response carries); dropping the guard releases them.
            sh.reg.sync_begin(sess.sid, &conv);
            let _reading = Reading(&sh.reg, sess.sid, &conv);
            let (sent, sent_side) = sh.reg.sync_sent(sess.sid, &conv);
            let visible_from = st.member.as_ref().map(|m| m.visible_from).unwrap_or(1);
            let mut reset = None;
            let base = match (st.acked, sent) {
                (None, None) => None,
                (a, s) => Some(a.map(|a| a.0).unwrap_or(0).max(s.unwrap_or(0))),
            };
            let bootstrap = |db_start: i64| db_start.max(visible_from).max(st.floor);
            let ttl = cfg.device_ttl.as_millis() as i64;
            let start = match base {
                None => {
                    // Joined after this device existed (and recently enough
                    // that it would still have its position): everything it
                    // may see. Otherwise like a new device.
                    if st
                        .joined_at
                        .is_some_and(|t| t >= device_start.max(now - ttl))
                    {
                        visible_from.max(st.floor)
                    } else {
                        bootstrap(self.window(&conv, st.last_seq, &cfg).await?)
                    }
                }
                Some(b) if b + 1 < st.floor || st.last_seq - b > cfg.max_gap => {
                    let s = bootstrap(self.window(&conv, st.last_seq, &cfg).await?);
                    reset = Some(s);
                    sh.reg.sync_forget(sess.sid, &conv);
                    s
                }
                Some(b) => (b + 1).max(visible_from),
            };
            let head = st.until.map(|u| u.min(st.last_seq)).unwrap_or(st.last_seq);
            // A first sync (or a reset) records where this device starts, so
            // what it has not acknowledged is counted from there.
            let acked_seq = match (base, reset) {
                (Some(_), None) => st.acked.map(|a| a.0).unwrap_or(0),
                _ => {
                    self.start_position(sess, &conv, start - 1).await?;
                    start - 1
                }
            };
            let outstanding = (start - 1 - acked_seq).max(0);
            let room = (cfg.max_unacked - outstanding).max(0);
            let n = cfg
                .batch_size
                .min(room)
                .min(budget)
                .min((head - start + 1).max(0));
            let side_after = match (st.acked, sent_side, reset) {
                (_, _, Some(_)) | (None, None, _) => None,
                (a, s, None) => Some(a.map(|a| a.1).unwrap_or(0).max(s.unwrap_or(0))),
            };
            let member = st.member.clone();
            let (c, me_id) = (conv.clone(), me.id.clone());
            let read_seq = st.read_seq;
            let policy_sh = sh.clone();
            let side_cap = cfg.batch_size;
            let read = self
                .read(shard, move |db| {
                    Box::pin(async move {
                        let raw = if n > 0 {
                            db.pull(&c, start - 1, n).await?
                        } else {
                            vec![]
                        };
                        let upto = raw.last().map(|e| e.seq).unwrap_or(start - 1);
                        let mut events = vec![];
                        for e in raw {
                            if let Some(m) = &member
                                && (!m.sees(&e)
                                    || m.kind == UserKind::Agent
                                        && !Ctx::agent_can_see(db, m, &e).await?)
                            {
                                continue;
                            }
                            events.push(decorate(
                                &policy_sh,
                                member.as_ref(),
                                &me_id,
                                e,
                                read_seq,
                                silenced,
                                now,
                            ));
                        }
                        let side_all = db.pull_side(&c, side_after.unwrap_or(0)).await?;
                        let mut side = vec![];
                        let mut side_upto = side_after.unwrap_or(0);
                        for s in side_all {
                            if side.len() as i64 >= side_cap || s.anchor_seq > upto {
                                break;
                            }
                            side_upto = s.side_seq;
                            if side_after.is_none() && s.anchor_seq < start {
                                continue;
                            }
                            if s.audience.contains(&me_id) {
                                side.push(s);
                            }
                        }
                        Ok((events, side, upto, side_upto))
                    })
                })
                .await;
            let (events, side, upto, side_upto) = read?;
            sh.reg.sync_mark(sess.sid, &conv, upto, side_upto);
            budget -= upto - (start - 1);
            let has_more = upto < head;
            more |= has_more;
            // A departure is reported until the device's position passes it
            // (acknowledged, or already sent on this connection).
            let told = base.is_some_and(|b| b >= head);
            if events.is_empty()
                && side.is_empty()
                && reset.is_none()
                && !has_more
                && (st.until.is_none() || told)
            {
                continue;
            }
            let mut entry = json!({
                "conv_id": conv,
                "events": events,
                "side_events": side,
                "upto": upto,
                "side_upto": side_upto,
                "last_seq": st.last_seq,
                "has_more": has_more,
            });
            if let Some(from) = reset {
                entry["reset"] = json!({"from_seq": from});
            }
            if st.until.is_some() && !has_more {
                entry["left"] = json!(true);
            }
            out.push(entry);
        }
        Ok(json!({"convs": out, "more": more}))
    }

    /// Records a device's starting position (first sync or reset).
    async fn start_position(&self, sess: &Session, conv: &str, seq: i64) -> ApiResult<()> {
        match &sess.device_id {
            None => self.inner.sh.reg.mem_ack(sess.sid, conv, seq, 0),
            Some(d) => {
                let shard = self.conv_shard(conv)?;
                shard
                    .call(Op::AckCursors {
                        device_id: d.clone(),
                        user_id: sess.user.id.clone(),
                        acks: vec![(conv.to_string(), seq, 0)],
                    })
                    .await?;
            }
        }
        Ok(())
    }

    /// The device's and member's position in a conversation.
    async fn conv_state(
        &self,
        sess: &Session,
        conv: &str,
        until: Option<i64>,
    ) -> ApiResult<Option<ConvState>> {
        let shard = self.conv_shard(conv)?;
        let device = sess.device_id.clone();
        let mem = self.inner.sh.reg.mem_acked(sess.sid, conv);
        let (c, me_id) = (conv.to_string(), sess.user.id.clone());
        self.read(shard, move |db| {
            Box::pin(async move {
                let Some(row) = db.conv(&c).await? else {
                    return Ok(None);
                };
                let member = db.member(&c, &me_id).await?;
                if member.is_none() && until.is_none() {
                    return Ok(None);
                }
                let acked = match &device {
                    Some(d) => db.cursor(d, &c).await?,
                    None => mem,
                };
                let joined_at = match &member {
                    Some(m) => db.event_time(&c, m.join_seq).await?,
                    None => None,
                };
                let floor = db.first_seq(&c).await?.unwrap_or(row.last_seq + 1);
                let read_seq = db.read_seq(&c, &me_id).await?;
                Ok(Some(ConvState {
                    conv_id: c,
                    member,
                    last_seq: row.last_seq,
                    floor,
                    acked,
                    joined_at,
                    read_seq,
                    until,
                }))
            })
        })
        .await
        .map(|o| o.inspect(|s| debug_assert!(!s.conv_id.is_empty())))
    }

    /// Where a new device starts in a conversation (docs/08 §2.4).
    async fn window(
        &self,
        conv: &str,
        last_seq: i64,
        cfg: &crate::config::SyncConfig,
    ) -> ApiResult<i64> {
        let shard = self.conv_shard(conv)?;
        let c = conv.to_string();
        let since = now_ms() - cfg.window.as_millis() as i64;
        let (max, older, older_n) = (cfg.max_messages, cfg.older_enabled, cfg.older_messages);
        self.read(shard, move |db| {
            Box::pin(async move {
                let s = db.window_start(&c, last_seq, max, since).await?;
                if s > last_seq && older {
                    // Nothing recent: a few older events, if allowed.
                    return db.window_start(&c, last_seq, older_n, 0).await;
                }
                Ok(s)
            })
        })
        .await
    }

    /// When the session's device was registered (sessions without a
    /// device: when they connected).
    async fn device_start(&self, sess: &Session) -> ApiResult<i64> {
        if let Some(t) = *sess.device_created.lock().unwrap() {
            return Ok(t);
        }
        let t = match &sess.device_id {
            Some(d) => {
                let d = d.clone();
                self.read(self.control(), move |db| {
                    Box::pin(async move { db.device_created(&d).await })
                })
                .await?
                .unwrap_or_else(now_ms)
            }
            None => self.inner.sh.reg.started(sess.sid).unwrap_or_else(now_ms),
        };
        *sess.device_created.lock().unwrap() = Some(t);
        Ok(t)
    }

    /// Whether reading on one device silences the others (default yes).
    async fn read_silences(&self, me: &User) -> ApiResult<bool> {
        let id = me.id.clone();
        let p = self
            .read(self.control(), move |db| {
                Box::pin(async move { db.prefs(&id).await })
            })
            .await?;
        Ok(p["read_silences_others"].as_bool().unwrap_or(true))
    }

    /// Stores what a device acknowledged (`ack`). Every position is checked
    /// before anything is written: none may pass what was sent.
    pub(super) async fn ack(&self, sess: &Session, acks: Vec<AckItem>) -> ApiResult<Value> {
        let sh = &self.inner.sh;
        if acks.is_empty() || acks.len() > 1000 {
            return Err(err("bad_request", "1..1000 acks per call"));
        }
        let mut by_shard: HashMap<usize, Vec<(String, i64, i64)>> = HashMap::new();
        for a in &acks {
            if a.seq < 0 || a.side_seq < 0 {
                return Err(err("bad_request", "positions are >= 0"));
            }
            let (s, ss) = sh.reg.sync_sent(sess.sid, &a.conv_id);
            if a.seq > s.unwrap_or(0) || a.side_seq > ss.unwrap_or(0) {
                return Err(err(
                    "bad_request",
                    format!(
                        "{}: acknowledged more than this session was sent",
                        a.conv_id
                    ),
                ));
            }
        }
        for a in acks {
            let Some(d) = sh.dir_get(&a.conv_id) else {
                continue;
            };
            match &sess.device_id {
                None => sh.reg.mem_ack(sess.sid, &a.conv_id, a.seq, a.side_seq),
                Some(_) => by_shard
                    .entry(d.db)
                    .or_default()
                    .push((a.conv_id, a.seq, a.side_seq)),
            }
        }
        let mut stored = 0;
        if let Some(device) = &sess.device_id {
            for (idx, acks) in by_shard {
                let v = self.inner.shards[idx]
                    .call(Op::AckCursors {
                        device_id: device.clone(),
                        user_id: sess.user.id.clone(),
                        acks,
                    })
                    .await?;
                stored += v["stored"].as_i64().unwrap_or(0);
            }
        }
        Ok(json!({"stored": stored}))
    }

    /// Unread counts (`unread`): per conversation and in total; muted
    /// conversations are counted but left out of `total_unmuted`.
    pub(super) async fn unread_summary(&self, me: &User) -> ApiResult<Value> {
        let me_id = me.id.clone();
        let convs: Vec<String> = self
            .read(self.control(), move |db| {
                Box::pin(async move { db.member_index_for(&[me_id]).await })
            })
            .await?
            .into_iter()
            .map(|(c, _)| c)
            .collect();
        let (mut total, mut unmuted, mut mentions) = (0i64, 0i64, 0i64);
        let mut out = vec![];
        let now = now_ms();
        for conv in convs {
            let Ok(shard) = self.conv_shard(&conv) else {
                continue;
            };
            let (c, me_id) = (conv.clone(), me.id.clone());
            let r = self
                .read(shard, move |db| {
                    Box::pin(async move {
                        let Some(m) = db.member(&c, &me_id).await? else {
                            return Ok(None);
                        };
                        Ok(Some((
                            db.unread(&c, &me_id).await?,
                            db.unread_mentions(&c, &me_id).await?,
                            m,
                        )))
                    })
                })
                .await?;
            let Some((u, men, m)) = r else { continue };
            let muted = m.mute_mode != "none" && m.mute_until.is_none_or(|t| t > now);
            total += u;
            mentions += men;
            if !muted || (m.mute_mode == "mentions" && men > 0) {
                unmuted += if muted { men } else { u };
            }
            if u > 0 || men > 0 {
                out.push(json!({"conv_id": conv, "unread": u, "mentions": men, "muted": muted, "mute_mode": m.mute_mode}));
            }
        }
        Ok(json!({"total": total, "total_unmuted": unmuted, "mentions": mentions, "convs": out}))
    }

    pub(super) async fn set_prefs(
        &self,
        me: &User,
        read_silences_others: Option<bool>,
    ) -> ApiResult<Value> {
        let id = me.id.clone();
        let mut p = self
            .read(self.control(), move |db| {
                Box::pin(async move { db.prefs(&id).await })
            })
            .await?;
        if let Some(v) = read_silences_others {
            p["read_silences_others"] = json!(v);
        }
        self.control()
            .call(Op::SetPrefs {
                user_id: me.id.clone(),
                prefs: p.clone(),
            })
            .await?;
        Ok(json!({"prefs": p}))
    }
}

/// An event as a device receives it: trust labels for agents, and whether
/// it should notify (not the reader's own, not muted, not read elsewhere).
fn decorate(
    sh: &crate::logic::Shared,
    member: Option<&Member>,
    me_id: &str,
    e: Event,
    read_seq: i64,
    silenced: bool,
    now: i64,
) -> Value {
    let mut v = json!({"type": "event", "event": e});
    let notify = match (&e.msg, e.kind.as_str(), member) {
        (Some(msg), "msg.created", Some(m)) => {
            m.notified_by(msg, now) && !(silenced && e.seq <= read_seq)
        }
        _ => false,
    };
    v["notify"] = json!(notify);
    if let (Some(m), Some(msg)) = (member, &e.msg)
        && m.kind == UserKind::Agent
        && e.kind == "msg.created"
        && msg.sender_id != me_id
    {
        let (t, flags) = sh.policy.inbound(
            &crate::logic::scan_text(&msg.content, msg.payload.as_ref()),
            msg.sender_type,
        );
        v["trust"] = json!(t.as_str());
        v["flags"] = json!(flags);
    }
    if member.is_some_and(|m| m.kind == UserKind::Agent) {
        // As recorded when the message was written (docs/09 §10).
        v["wake"] = json!(
            e.data["wake"]
                .as_array()
                .is_some_and(|w| w.iter().any(|x| x == me_id))
        );
    }
    v
}

impl Session {
    /// Sync rounds are rate limited per session (D38); agents more strictly.
    fn sync_allowed(&self, cfg: &crate::config::SyncConfig) -> ApiResult<()> {
        let (rate, burst) = if self.user.kind == UserKind::Agent {
            cfg.agent_rate
        } else {
            cfg.rate
        };
        let mut b = self.sync_bucket.lock().unwrap();
        let now = Instant::now();
        let tokens = match b.as_ref() {
            Some((t, at)) => (t + now.duration_since(*at).as_secs_f64() * rate).min(burst),
            None => burst,
        };
        if tokens < 1.0 {
            let wait = ((1.0 - tokens) / rate * 1000.0).ceil() as i64;
            *b = Some((tokens, now));
            return Err(err(
                "rate_limited",
                format!("too many sync calls; retry in {wait} ms"),
            ));
        }
        *b = Some((tokens - 1.0, now));
        Ok(())
    }
}

/// Ends a sync read of one conversation however the call leaves.
struct Reading<'a>(&'a crate::registry::Registry, u64, &'a str);

impl Drop for Reading<'_> {
    fn drop(&mut self) {
        self.0.sync_end(self.1, self.2);
    }
}
