//! Online sessions and fan-out.
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
}

/// Conversations a session must be told to resync. Shared between fan-out
/// (which adds to it when the queue is full) and the session's [`Pushes`].
type Owed = Arc<Mutex<HashSet<String>>>;

fn lock_owed(o: &Owed) -> std::sync::MutexGuard<'_, HashSet<String>> {
    o.lock().unwrap_or_else(|e| e.into_inner())
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
        if self.rx.is_empty()
            && let Some(f) = self.owed_frame()
        {
            return Some(f);
        }
        self.rx.recv().await
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
                            send(&mut g, sid, f.clone(), None);
                        }
                    }
                }
                Out::ToUser { user_id, frame } => {
                    let f: Frame = frame.to_string().into();
                    for sid in sessions_of(&g, &user_id) {
                        send(&mut g, sid, f.clone(), None);
                    }
                }
                Out::ToSession { sid, frame } => send(&mut g, sid, frame.to_string().into(), None),
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

fn send(g: &mut Inner, sid: u64, f: Frame, resync_conv: Option<&str>) {
    let Some(e) = g.sessions.get_mut(&sid) else {
        return;
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
        return;
    }
    metrics::counter!(m::PUSH_DROPPED).increment(1);
    if let Some(c) = resync_conv {
        owed.insert(c.to_string());
    }
}

fn deliver_event(g: &mut Inner, ev: &Event, recips: &[Recip]) {
    let base: Frame = json!({"type": "event", "event": ev}).to_string().into();
    let members: HashSet<&str> = recips.iter().map(|r| r.user_id.as_str()).collect();
    for r in recips {
        if !r.visible {
            continue;
        }
        let frame = match &r.trust {
            Some((trust, flags)) => {
                json!({"type": "event", "event": ev, "trust": trust, "flags": flags})
                    .to_string()
                    .into()
            }
            None => base.clone(),
        };
        for sid in sessions_of(g, &r.user_id) {
            send(g, sid, frame.clone(), Some(&ev.conv_id));
        }
        // Owners observing through this agent get the agent's scope.
        if r.kind == UserKind::Agent
            && let Some(owner) = &r.owner_id
        {
            if members.contains(owner.as_str()) {
                continue;
            }
            let observing: Vec<u64> = sessions_of(g, owner)
                .into_iter()
                .filter(|sid| {
                    g.sessions
                        .get(sid)
                        .and_then(|e| e.focus.as_ref())
                        .map(|(c, a)| c == &ev.conv_id && a.as_deref() == Some(r.user_id.as_str()))
                        .unwrap_or(false)
                })
                .collect();
            if !observing.is_empty() {
                let f: Frame = json!({"type": "event", "event": ev, "observing": r.user_id})
                    .to_string()
                    .into();
                for sid in observing {
                    send(g, sid, f.clone(), Some(&ev.conv_id));
                }
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
