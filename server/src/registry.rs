//! Online sessions and fan-out.
//!
//! Fan-out runs after commit, outside any database work. Each event is
//! serialised once and shared. Every session has a bounded queue: when it is
//! full the push is dropped and the session is told to `resync` that
//! conversation (pull is the source of truth), so a slow client can never
//! exhaust server memory.

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

struct Entry {
    user_id: String,
    kind: UserKind,
    tx: mpsc::Sender<Frame>,
    focus: Option<(String, Option<String>)>,
    resync: HashSet<String>,
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

    pub fn add(&self, user: &User) -> (u64, mpsc::Receiver<Frame>) {
        let cap = if user.kind == UserKind::Agent {
            self.agent_queue
        } else {
            self.human_queue
        };
        let (tx, rx) = mpsc::channel(cap);
        let mut g = self.lock();
        g.next += 1;
        let sid = g.next;
        g.sessions.insert(
            sid,
            Entry {
                user_id: user.id.clone(),
                kind: user.kind,
                tx,
                focus: None,
                resync: HashSet::new(),
            },
        );
        g.by_user.entry(user.id.clone()).or_default().push(sid);
        metrics::gauge!(m::WS_CONNECTIONS, "kind" => user.kind.as_str()).increment(1.0);
        metrics::counter!(m::WS_CONNECTS, "kind" => user.kind.as_str()).increment(1);
        (sid, rx)
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

    pub fn is_online(&self, user_id: &str) -> bool {
        self.lock().by_user.contains_key(user_id)
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
    if !e.resync.is_empty() {
        let pending: Vec<String> = e.resync.iter().cloned().collect();
        for c in pending {
            let r: Frame = json!({"type": "resync", "conv_id": c}).to_string().into();
            if e.tx.try_send(r).is_ok() {
                e.resync.remove(&c);
                metrics::counter!(m::RESYNC).increment(1);
            } else {
                break;
            }
        }
    }
    if e.resync.is_empty() && e.tx.try_send(f).is_ok() {
        metrics::counter!(m::PUSH).increment(1);
        return;
    }
    metrics::counter!(m::PUSH_DROPPED).increment(1);
    if let Some(c) = resync_conv {
        e.resync.insert(c.to_string());
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
        let got: Vec<String> = std::iter::from_fn(|| rx.try_recv().ok())
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
}
