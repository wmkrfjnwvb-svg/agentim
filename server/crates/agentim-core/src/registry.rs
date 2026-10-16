//! Online sessions and fan-out.
//!
//! A session that has called `sync` (docs/08-sync-and-client.md) gets live
//! events of a conversation only while they continue what it was sent
//! before: a push that would leave a gap is held back and the session is
//! told to `resync` that conversation, so each session sees every event of a
//! conversation once and in order. Sessions that never sync get every
//! visible event as it happens (pushes as hints).
//!
//! Fan-out runs after commit, outside any database work. Each event is
//! serialised once and shared. Every session has a bounded queue: when it is
//! full the push is dropped and the session is told to `resync` that
//! conversation (pull is the source of truth), so a slow client can never
//! exhaust server memory. An owed notice goes out as soon as the queue has
//! drained, even if nothing else is pushed afterwards.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::metrics as m;
use crate::model::*;

pub type Frame = Arc<str>;

/// Something to deliver once the batch that produced it has committed.
#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
pub enum Out {
    Event {
        ev: Event,
        recips: Arc<Vec<Recip>>,
    },
    Side(SideEvent),
    ToUser {
        user_id: String,
        frame: Value,
    },
    ToSession {
        sid: u64,
        frame: Value,
    },
    /// Stop live deltas of a conversation for a member who lost access
    /// (`user_id`, also when observing through that agent), or for everyone.
    Unfocus {
        conv_id: String,
        user_id: Option<String>,
    },
    Delta {
        conv_id: String,
        msg_id: String,
        text: String,
        reset: bool,
        sender_id: String,
        audience: Option<Vec<String>>,
    },
    /// Token cache changes, applied by the writer after commit in write
    /// order (so a rotation and a revocation cannot be reordered), then
    /// the connections of revoked devices (`None`: all of the user) closed.
    Tokens {
        add: Vec<(String, crate::logic::TokenInfo)>,
        remove: Vec<String>,
        kick: Vec<(String, Option<String>)>,
    },
}

/// A conversation member as seen by fan-out for one event.
#[derive(Debug, Clone)]
pub struct Recip {
    pub user_id: String,
    pub kind: UserKind,
    pub owner_id: Option<String>,
    /// Whether this member may see the event (agents are filtered by trigger mode).
    pub visible: bool,
    /// Trust label and flags for agents receiving a message from someone else.
    pub trust: Option<(String, Vec<String>)>,
    /// Whether the event should notify this member (not their own, not muted).
    pub notify: bool,
    /// For agents: whether the event asks it to act (docs/09 §10).
    pub wake: bool,
}

/// Conversations a session must be told to resync. Shared between fan-out
/// (which adds to it when the queue is full) and the session's [`Pushes`].
/// Conversations a session must be told to sync again, and a wake-up for
/// its push stream (a notice is owed even when no frame follows).
#[derive(Default)]
struct OwedInner {
    set: Mutex<HashSet<String>>,
    wake: tokio::sync::Notify,
}

type Owed = Arc<OwedInner>;

fn lock_owed(o: &Owed) -> std::sync::MutexGuard<'_, HashSet<String>> {
    o.set.lock().unwrap_or_else(|e| e.into_inner())
}

/// Owes a resync notice and wakes the push stream to send it.
fn owe(o: &Owed, conv_id: &str) {
    lock_owed(o).insert(conv_id.to_string());
    o.wake.notify_one();
}

fn resync_frame(conv_id: &str) -> Frame {
    json!({"type": "resync", "conv_id": conv_id})
        .to_string()
        .into()
}

struct Entry {
    user_id: String,
    device_id: Option<String>,
    /// Closes the connection (device logged out).
    kick: Arc<tokio::sync::Notify>,
    kind: UserKind,
    tx: mpsc::Sender<Frame>,
    focus: Option<(String, Option<String>)>,
    resync: Owed,
    /// Sync mode: highest seq and side seq sent per conversation.
    sync: Option<SyncState>,
    /// When the session started (ms), for sessions without a device.
    started: i64,
}

#[derive(Default)]
struct SyncState {
    sent: HashMap<String, i64>,
    sent_side: HashMap<String, i64>,
    /// Acknowledged positions of sessions without a device (memory only).
    acked: HashMap<String, (i64, i64)>,
    /// Conversations a sync call is reading now: live pushes wait (they
    /// could otherwise repeat an event the response carries)...
    busy: HashSet<String>,
    /// ...and a held-back push makes the session owe a resync.
    held: HashSet<String>,
}

/// A session's push stream: queued frames in order, then any owed resync
/// notices once the queue is empty. Without the latter, a client whose pushes
/// were dropped near the end of a burst would never learn it has a gap.
pub struct Pushes {
    rx: mpsc::Receiver<Frame>,
    owed: Owed,
}

impl Pushes {
    fn owed_frame(&self) -> Option<Frame> {
        let mut o = lock_owed(&self.owed);
        let c = o.iter().next().cloned()?;
        o.remove(&c);
        metrics::counter!(m::RESYNC).increment(1);
        Some(resync_frame(&c))
    }

    pub async fn recv(&mut self) -> Option<Frame> {
        loop {
            if self.rx.is_empty()
                && let Some(f) = self.owed_frame()
            {
                return Some(f);
            }
            tokio::select! {
                f = self.rx.recv() => return f,
                _ = self.owed.wake.notified() => {}
            }
        }
    }

    pub fn try_recv(&mut self) -> Option<Frame> {
        match self.rx.try_recv() {
            Ok(f) => Some(f),
            Err(_) => self.owed_frame(),
        }
    }
}

#[derive(Default)]
struct Inner {
    next: u64,
    sessions: HashMap<u64, Entry>,
    by_user: HashMap<String, Vec<u64>>,
    by_focus: HashMap<String, HashSet<u64>>,
}

pub struct Registry {
    inner: Mutex<Inner>,
    human_queue: usize,
    agent_queue: usize,
}

impl Registry {
    pub fn new(human_queue: usize, agent_queue: usize) -> Registry {
        Registry {
            inner: Mutex::new(Inner::default()),
            human_queue,
            agent_queue,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn add(&self, user: &User) -> (u64, Pushes) {
        let (sid, p, _) = self.add_device(user, None);
        (sid, p)
    }

    /// A session of one device; the returned notify fires when that device
    /// is logged out.
    pub fn add_device(
        &self,
        user: &User,
        device_id: Option<String>,
    ) -> (u64, Pushes, Arc<tokio::sync::Notify>) {
        let kick = Arc::new(tokio::sync::Notify::new());
        let cap = if user.kind == UserKind::Agent {
            self.agent_queue
        } else {
            self.human_queue
        };
        let (tx, rx) = mpsc::channel(cap);
        let owed = Owed::default();
        // A device receives live events only in order after its sync
        // position (docs/08 §2.2): until its first sync it is only told to
        // sync, so nothing pushed earlier can come again in that sync.
        let sync = device_id.is_some().then(SyncState::default);
        let mut g = self.lock();
        g.next += 1;
        let sid = g.next;
        g.sessions.insert(
            sid,
            Entry {
                user_id: user.id.clone(),
                device_id,
                kick: kick.clone(),
                kind: user.kind,
                tx,
                focus: None,
                resync: owed.clone(),
                sync,
                started: now_ms(),
            },
        );
        g.by_user.entry(user.id.clone()).or_default().push(sid);
        metrics::gauge!(m::WS_CONNECTIONS, "kind" => user.kind.as_str()).increment(1.0);
        metrics::counter!(m::WS_CONNECTS, "kind" => user.kind.as_str()).increment(1);
        (sid, Pushes { rx, owed }, kick)
    }

    /// Disconnects a user's sessions: all of them, or one device's.
    pub fn kick(&self, user_id: &str, device_id: Option<&str>) -> usize {
        let g = self.lock();
        let mut n = 0;
        for sid in g.by_user.get(user_id).into_iter().flatten() {
            if let Some(e) = g.sessions.get(sid)
                && (device_id.is_none() || e.device_id.as_deref() == device_id)
            {
                e.kick.notify_one();
                n += 1;
            }
        }
        n
    }

    /// Sessions of a user other than `except`.
    pub fn others(&self, user_id: &str, except: u64) -> Vec<u64> {
        let g = self.lock();
        g.by_user
            .get(user_id)
            .map(|v| v.iter().copied().filter(|s| *s != except).collect())
            .unwrap_or_default()
    }

    /// Removes a session; returns whether the user still has other sessions.
    pub fn remove(&self, sid: u64) -> Option<(String, UserKind, bool)> {
        let mut g = self.lock();
        let e = g.sessions.remove(&sid)?;
        if let Some((c, _)) = &e.focus
            && let Some(s) = g.by_focus.get_mut(c)
        {
            s.remove(&sid);
        }
        let still = match g.by_user.get_mut(&e.user_id) {
            Some(v) => {
                v.retain(|x| *x != sid);
                !v.is_empty()
            }
            None => false,
        };
        if !still {
            g.by_user.remove(&e.user_id);
        }
        metrics::gauge!(m::WS_CONNECTIONS, "kind" => e.kind.as_str()).decrement(1.0);
        metrics::counter!(m::WS_DISCONNECTS, "kind" => e.kind.as_str()).increment(1);
        Some((e.user_id, e.kind, still))
    }

    /// Puts a session in sync mode (idempotent).
    pub fn sync_enable(&self, sid: u64) {
        if let Some(e) = self.lock().sessions.get_mut(&sid) {
            e.sync.get_or_insert_with(SyncState::default);
        }
    }

    /// Highest (seq, side seq) sent to a session for a conversation.
    pub fn sync_sent(&self, sid: u64, conv_id: &str) -> (Option<i64>, Option<i64>) {
        let g = self.lock();
        match g.sessions.get(&sid).and_then(|e| e.sync.as_ref()) {
            Some(st) => (
                st.sent.get(conv_id).copied(),
                st.sent_side.get(conv_id).copied(),
            ),
            None => (None, None),
        }
    }

    /// A sync call starts reading a conversation: live pushes of it wait
    /// until `sync_mark` or `sync_end`.
    pub fn sync_begin(&self, sid: u64, conv_id: &str) {
        let mut g = self.lock();
        if let Some(st) = g.sessions.get_mut(&sid).and_then(|e| e.sync.as_mut()) {
            st.busy.insert(conv_id.to_string());
        }
    }

    /// The read is over (also when it failed): pushes flow again, and a
    /// held-back one makes the session owe a resync.
    pub fn sync_end(&self, sid: u64, conv_id: &str) {
        let mut g = self.lock();
        let Some(e) = g.sessions.get_mut(&sid) else {
            return;
        };
        let Some(st) = e.sync.as_mut() else { return };
        st.busy.remove(conv_id);
        if st.held.remove(conv_id) {
            owe(&e.resync, conv_id);
        }
    }

    /// Records what a sync response sends (positions only move forward),
    /// and ends the read.
    pub fn sync_mark(&self, sid: u64, conv_id: &str, seq: i64, side_seq: i64) {
        {
            let mut g = self.lock();
            let Some(st) = g.sessions.get_mut(&sid).and_then(|e| e.sync.as_mut()) else {
                return;
            };
            let s = st.sent.entry(conv_id.to_string()).or_insert(seq);
            *s = (*s).max(seq);
            let s = st.sent_side.entry(conv_id.to_string()).or_insert(side_seq);
            *s = (*s).max(side_seq);
        }
        self.sync_end(sid, conv_id);
    }

    /// Forgets what was sent for a conversation (reset, lost access).
    pub fn sync_forget(&self, sid: u64, conv_id: &str) {
        let mut g = self.lock();
        if let Some(st) = g.sessions.get_mut(&sid).and_then(|e| e.sync.as_mut()) {
            st.sent.remove(conv_id);
            st.sent_side.remove(conv_id);
            st.acked.remove(conv_id);
        }
    }

    /// Acknowledged positions kept in memory for a session without a device.
    pub fn mem_acked(&self, sid: u64, conv_id: &str) -> Option<(i64, i64)> {
        let g = self.lock();
        g.sessions
            .get(&sid)
            .and_then(|e| e.sync.as_ref())
            .and_then(|st| st.acked.get(conv_id).copied())
    }

    pub fn mem_ack(&self, sid: u64, conv_id: &str, seq: i64, side_seq: i64) {
        let mut g = self.lock();
        if let Some(st) = g.sessions.get_mut(&sid).and_then(|e| e.sync.as_mut()) {
            let a = st.acked.entry(conv_id.to_string()).or_insert((0, 0));
            a.0 = a.0.max(seq);
            a.1 = a.1.max(side_seq);
        }
    }

    /// When a session started (ms).
    pub fn started(&self, sid: u64) -> Option<i64> {
        self.lock().sessions.get(&sid).map(|e| e.started)
    }

    pub fn set_focus(&self, sid: u64, focus: Option<(String, Option<String>)>) {
        let mut g = self.lock();
        let old = match g.sessions.get_mut(&sid) {
            Some(e) => std::mem::replace(&mut e.focus, focus.clone()),
            None => return,
        };
        if let Some((c, _)) = old
            && let Some(s) = g.by_focus.get_mut(&c)
        {
            s.remove(&sid);
        }
        if let Some((c, _)) = focus {
            g.by_focus.entry(c).or_default().insert(sid);
        }
    }

    pub fn online_users(&self) -> Vec<String> {
        let g = self.lock();
        let mut v: Vec<String> = g.by_user.keys().cloned().collect();
        v.sort();
        v
    }

    pub fn session_count(&self) -> usize {
        self.lock().sessions.len()
    }

    /// Delivers committed output, in order.
    pub fn deliver(&self, outs: Vec<Out>) {
        if outs.is_empty() {
            return;
        }
        let mut g = self.lock();
        for o in outs {
            match o {
                Out::Event { ev, recips } => deliver_event(&mut g, &ev, &recips),
                Out::Side(side) => {
                    let f: Frame = json!({"type": "side", "event": side}).to_string().into();
                    for u in &side.audience {
                        for sid in sessions_of(&g, u) {
                            if in_order(&mut g, sid, &side.conv_id, side.side_seq, true) {
                                send(&mut g, sid, f.clone(), Some(&side.conv_id));
                            }
                        }
                    }
                }
                Out::ToUser { user_id, frame } => {
                    let f: Frame = frame.to_string().into();
                    for sid in sessions_of(&g, &user_id) {
                        send(&mut g, sid, f.clone(), None);
                    }
                }
                Out::ToSession { sid, frame } => {
                    send(&mut g, sid, frame.to_string().into(), None);
                }
                Out::Unfocus { conv_id, user_id } => {
                    let sids: Vec<u64> = g
                        .by_focus
                        .get(&conv_id)
                        .map(|s| s.iter().copied().collect())
                        .unwrap_or_default();
                    for sid in sids {
                        let Some(e) = g.sessions.get_mut(&sid) else {
                            continue;
                        };
                        let hit = match &user_id {
                            None => true,
                            Some(u) => {
                                e.user_id == *u
                                    || e.focus.as_ref().and_then(|f| f.1.as_deref()) == Some(u)
                            }
                        };
                        if hit {
                            e.focus = None;
                            if let Some(s) = g.by_focus.get_mut(&conv_id) {
                                s.remove(&sid);
                            }
                        }
                    }
                }
                Out::Tokens { kick, .. } => {
                    for (user_id, device_id) in kick {
                        for sid in g.by_user.get(&user_id).into_iter().flatten() {
                            if let Some(e) = g.sessions.get(sid)
                                && (device_id.is_none() || e.device_id == device_id)
                            {
                                e.kick.notify_one();
                            }
                        }
                    }
                }
                Out::Delta {
                    conv_id,
                    msg_id,
                    text,
                    reset,
                    sender_id,
                    audience,
                } => {
                    let draft = audience.is_some();
                    let f: Frame = json!({"type": "delta", "conv_id": conv_id, "msg_id": msg_id, "reset": reset, "text": text, "draft": draft})
                        .to_string()
                        .into();
                    let mut targets: HashSet<u64> = HashSet::new();
                    match &audience {
                        Some(a) => a.iter().for_each(|u| targets.extend(sessions_of(&g, u))),
                        None => {
                            if let Some(s) = g.by_focus.get(&conv_id) {
                                targets.extend(s.iter().copied());
                            }
                            targets.extend(sessions_of(&g, &sender_id));
                        }
                    }
                    for sid in targets {
                        // Deltas are best effort: the final version arrives as an event.
                        if let Some(e) = g.sessions.get(&sid)
                            && e.tx.try_send(f.clone()).is_ok()
                        {
                            metrics::counter!(m::STREAM_DELTAS).increment(1);
                        }
                    }
                }
            }
        }
    }
}

fn sessions_of(g: &Inner, user_id: &str) -> Vec<u64> {
    g.by_user.get(user_id).cloned().unwrap_or_default()
}

/// For a session in sync mode: whether a live event continues what was sent
/// (then it counts as sent); otherwise the session is told to sync. Sessions
/// not in sync mode take every event.
fn in_order(g: &mut Inner, sid: u64, conv_id: &str, seq: i64, side: bool) -> bool {
    let Some(e) = g.sessions.get_mut(&sid) else {
        return false;
    };
    let Some(st) = e.sync.as_mut() else {
        return true;
    };
    if st.busy.contains(conv_id) {
        st.held.insert(conv_id.to_string());
        return false;
    }
    let map = if side {
        &mut st.sent_side
    } else {
        &mut st.sent
    };
    match map.get_mut(conv_id) {
        Some(sent) if seq == *sent + 1 => {
            *sent = seq;
            true
        }
        Some(sent) if seq <= *sent => false,
        _ => {
            owe(&e.resync, conv_id);
            false
        }
    }
}

fn send(g: &mut Inner, sid: u64, f: Frame, resync_conv: Option<&str>) -> bool {
    let Some(e) = g.sessions.get_mut(&sid) else {
        return false;
    };
    // Owed resync notices go first, as soon as there is room.
    let mut owed = lock_owed(&e.resync);
    while let Some(c) = owed.iter().next().cloned() {
        if e.tx.try_send(resync_frame(&c)).is_err() {
            break;
        }
        owed.remove(&c);
        metrics::counter!(m::RESYNC).increment(1);
    }
    if owed.is_empty() && e.tx.try_send(f).is_ok() {
        metrics::counter!(m::PUSH).increment(1);
        return true;
    }
    metrics::counter!(m::PUSH_DROPPED).increment(1);
    drop(owed);
    if let Some(c) = resync_conv {
        owe(&e.resync, c);
        // A dropped push was not sent: in sync mode the next sync resends it.
        if let Some(st) = e.sync.as_mut() {
            st.sent.remove(c);
            st.sent_side.remove(c);
        }
    }
    false
}

fn deliver_event(g: &mut Inner, ev: &Event, recips: &[Recip]) {
    let quiet: Frame = json!({"type": "event", "event": ev, "notify": false})
        .to_string()
        .into();
    let loud: Frame = json!({"type": "event", "event": ev, "notify": true})
        .to_string()
        .into();
    for r in recips {
        if !r.visible {
            continue;
        }
        let frame = if r.kind == UserKind::Agent {
            // Agents also get the trust label and whether to act.
            let mut v = json!({"type": "event", "event": ev, "notify": r.notify, "wake": r.wake});
            if let Some((trust, flags)) = &r.trust {
                v["trust"] = json!(trust);
                v["flags"] = json!(flags);
            }
            v.to_string().into()
        } else if r.notify {
            loud.clone()
        } else {
            quiet.clone()
        };
        for sid in sessions_of(g, &r.user_id) {
            if in_order(g, sid, &ev.conv_id, ev.seq, false) {
                send(g, sid, frame.clone(), Some(&ev.conv_id));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(id: &str) -> User {
        User {
            id: id.into(),
            name: id.into(),
            kind: UserKind::Human,
            owner_id: None,
            unit_id: UNIT_DEFAULT.into(),
            team: false,
        }
    }

    fn ev(seq: i64) -> Event {
        Event {
            conv_id: "c".into(),
            seq,
            kind: "conv.updated".into(),
            actor_id: None,
            msg: None,
            data: json!({}),
            created_at: 0,
        }
    }

    #[test]
    fn full_queue_drops_and_owes_a_resync() {
        let reg = Registry::new(2, 2);
        let (_sid, mut rx) = reg.add(&user("a"));
        let recips = Arc::new(vec![Recip {
            user_id: "a".into(),
            kind: UserKind::Human,
            owner_id: None,
            visible: true,
            trust: None,
            notify: true,
            wake: false,
        }]);
        for s in 1..=5 {
            reg.deliver(vec![Out::Event {
                ev: ev(s),
                recips: recips.clone(),
            }]);
        }
        let got: Vec<String> = std::iter::from_fn(|| rx.rx.try_recv().ok())
            .map(|f| f.to_string())
            .collect();
        assert_eq!(got.len(), 2, "queue is bounded");
        reg.deliver(vec![Out::Event { ev: ev(6), recips }]);
        let next = rx.try_recv().unwrap();
        assert!(
            next.contains("resync"),
            "resync is sent before new pushes: {next}"
        );
    }

    #[test]
    fn owed_resync_is_sent_once_the_queue_drains_without_further_pushes() {
        let reg = Registry::new(2, 2);
        let (_sid, mut rx) = reg.add(&user("a"));
        let recips = Arc::new(vec![Recip {
            user_id: "a".into(),
            kind: UserKind::Human,
            owner_id: None,
            visible: true,
            trust: None,
            notify: true,
            wake: false,
        }]);
        for s in 1..=5 {
            reg.deliver(vec![Out::Event {
                ev: ev(s),
                recips: recips.clone(),
            }]);
        }
        // Nothing else is pushed: the client still learns about the gap,
        // after the frames that were queued before it.
        let got: Vec<String> = std::iter::from_fn(|| rx.try_recv())
            .map(|f| f.to_string())
            .collect();
        assert_eq!(got.len(), 3, "{got:?}");
        assert!(got[..2].iter().all(|f| f.contains("\"event\"")));
        assert!(got[2].contains("resync"), "{}", got[2]);
        assert!(rx.try_recv().is_none(), "the notice is sent once");
    }
}

#[cfg(test)]
mod sync_tests {
    use super::*;

    fn user() -> User {
        User {
            id: "a".into(),
            name: "a".into(),
            kind: UserKind::Human,
            owner_id: None,
            unit_id: UNIT_DEFAULT.into(),
            team: false,
        }
    }

    fn deliver(reg: &Registry, seq: i64) {
        reg.deliver(vec![Out::Event {
            ev: Event {
                conv_id: "c".into(),
                seq,
                kind: "conv.updated".into(),
                actor_id: None,
                msg: None,
                data: json!({}),
                created_at: 0,
            },
            recips: Arc::new(vec![Recip {
                user_id: "a".into(),
                kind: UserKind::Human,
                owner_id: None,
                visible: true,
                trust: None,
                notify: false,
                wake: false,
            }]),
        }]);
    }

    fn drain(rx: &mut Pushes) -> Vec<Value> {
        std::iter::from_fn(|| rx.try_recv())
            .map(|f| serde_json::from_str(&f).unwrap())
            .collect()
    }

    #[test]
    fn a_syncing_session_gets_live_events_only_in_order() {
        let reg = Registry::new(100, 100);
        let (sid, mut rx) = reg.add(&user());
        reg.sync_enable(sid);
        // Never synced this conversation: told to sync, nothing pushed.
        deliver(&reg, 7);
        let f = drain(&mut rx);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0]["type"], "resync");
        // After a sync that sent up to 7: 8 is pushed, a jump to 10 is not.
        reg.sync_mark(sid, "c", 7, 0);
        deliver(&reg, 8);
        deliver(&reg, 8);
        deliver(&reg, 10);
        let f = drain(&mut rx);
        assert_eq!(f.len(), 2, "{f:?}");
        assert_eq!(f[0]["event"]["seq"], 8);
        assert_eq!(
            f[1]["type"], "resync",
            "the duplicate is dropped, the gap is reported"
        );
        assert_eq!(reg.sync_sent(sid, "c").0, Some(8));
    }

    #[test]
    fn a_dropped_push_is_not_counted_as_sent() {
        let reg = Registry::new(1, 1);
        let (sid, mut rx) = reg.add(&user());
        reg.sync_enable(sid);
        reg.sync_mark(sid, "c", 0, 0);
        deliver(&reg, 1);
        deliver(&reg, 2); // queue full: dropped
        assert_eq!(
            reg.sync_sent(sid, "c").0,
            None,
            "the next sync resends from the ack"
        );
        let f = drain(&mut rx);
        assert_eq!(f[0]["event"]["seq"], 1);
        assert_eq!(f[1]["type"], "resync");
    }

    /// While a sync call reads a conversation, a live push of it (which the
    /// response may also carry) waits; afterwards the session is told to
    /// sync again, so nothing is lost or delivered twice.
    #[test]
    fn pushes_wait_while_a_sync_reads_the_conversation() {
        let reg = Registry::new(100, 100);
        let (sid, mut rx) = reg.add(&user());
        reg.sync_enable(sid);
        reg.sync_mark(sid, "c", 5, 0);
        reg.sync_begin(sid, "c");
        deliver(&reg, 6); // also in the response being read
        assert!(drain(&mut rx).is_empty(), "held back");
        reg.sync_mark(sid, "c", 9, 0);
        deliver(&reg, 10);
        let f = drain(&mut rx);
        assert_eq!(f.len(), 2, "{f:?}");
        assert_eq!(f[0]["type"], "resync");
        assert_eq!(f[1]["event"]["seq"], 10);
        // A failed read also releases, with the resync owed.
        reg.sync_begin(sid, "c");
        deliver(&reg, 11);
        reg.sync_end(sid, "c");
        let f = drain(&mut rx);
        assert_eq!(f.len(), 1, "{f:?}");
        assert_eq!(f[0]["type"], "resync");
        assert_eq!(reg.sync_sent(sid, "c").0, Some(10), "11 was not sent");
        // Without a held push nothing is owed.
        reg.sync_begin(sid, "c");
        reg.sync_mark(sid, "c", 11, 0);
        deliver(&reg, 12);
        let f = drain(&mut rx);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0]["event"]["seq"], 12);
    }

    /// A resync notice owed while the stream waits for frames is sent at
    /// once, not with the next frame (which may never come).
    #[tokio::test]
    async fn an_owed_resync_wakes_a_waiting_stream() {
        let reg = Arc::new(Registry::new(100, 100));
        let (sid, mut rx, _) = reg.add_device(&user(), Some("d".into()));
        let waiting = tokio::spawn(async move { rx.recv().await });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        // Not synced yet: the event is held back and a resync owed.
        deliver(&reg, 1);
        let f = tokio::time::timeout(std::time::Duration::from_secs(2), waiting)
            .await
            .expect("the waiting stream is woken")
            .unwrap()
            .unwrap();
        let v: Value = serde_json::from_str(&f).unwrap();
        assert_eq!(v["type"], "resync");
        assert_eq!(reg.sync_sent(sid, "c").0, None);
    }
}
