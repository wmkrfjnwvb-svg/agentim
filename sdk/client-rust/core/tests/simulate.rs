//! Random simulation (docs/12 §2.13): many seeds, each a random mix of
//! user actions, other people talking and faults (lost connections,
//! leaving nodes, lost answers and pushes, overflowing push queues, busy
//! servers, going offline, a full store, process restarts). Then the
//! faults stop and the world runs on; it must converge:
//!
//! - every message the user sent (and did not take back before it went
//!   out) is stored on the server exactly once, in the order sent per
//!   conversation; refused ones show as failed;
//! - the client's position and messages equal the server's;
//! - nothing was ever acknowledged before it was stored;
//! - the app's view has each key once, in order.
//!
//! A failure prints its seed: `AGENTIM_SIM_SEED=<n> cargo test simulate`.
//! `AGENTIM_SIM_SEEDS` sets how many seeds run (default 300).

mod common;

use std::collections::{BTreeMap, HashSet};

use agentim_client_core::*;
use common::*;
use serde_json::{Value, json};

struct Dice(u64);

impl Dice {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    fn chance(&mut self, pct: u64) -> bool {
        self.below(100) < pct
    }
}

const CONVS: [&str; 3] = ["c1", "c2", "c3"];
/// The user is not a member here: sends are refused.
const FORBIDDEN: &str = "c3";

fn run_seed(seed: u64) {
    let mut d = Dice(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let mut w = World::new(seed);
    for c in CONVS {
        w.server.conv(c);
    }
    w.server.forbidden.insert(FORBIDDEN.into());
    w.server.batch = 1 + d.below(40) as usize;
    w.start();
    w.cmd(json!({"cmd": "connect"}));
    for c in CONVS {
        w.cmd(json!({"cmd": "messages", "conv_id": c}));
    }
    // What the user sent, per conversation, in order; and what they took back.
    let mut sent: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut dropped: HashSet<String> = HashSet::new();
    let mut n = 0u64;
    let mut online = true;
    let mut generation = 0u64;
    let mut store_full = false;
    let mut open = true;
    for _ in 0..120 {
        match d.below(20) {
            0..=4 => {
                n += 1;
                let c = CONVS[d.below(3) as usize];
                let cm = format!("s{seed}_{n}");
                w.ask(json!({"cmd": "send", "conv_id": c, "text": format!("t{n}"), "client_msg_id": cm})).unwrap();
                sent.entry(c.to_string()).or_default().push(cm);
            }
            5 => {
                // Edit or take back one of ours, sent or not.
                let c = CONVS[d.below(2) as usize];
                let mine: Vec<String> = sent.get(c).cloned().unwrap_or_default();
                if let Some(cm) = mine.get(d.below(mine.len().max(1) as u64) as usize) {
                    if d.chance(30) {
                        let was_local = w.core.debug_state()["queue"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .any(|o| {
                                o["kind"] == "send" && o["cm"] == cm.as_str() && o["sent"].is_null()
                            });
                        let r = w.ask(json!({"cmd": "recall", "conv_id": c, "client_msg_id": cm}));
                        if r.is_ok() && was_local && !w.server.cms.contains_key(cm) {
                            // Dropped before it went out, unless it is in flight.
                            let still = w.core.debug_state()["queue"]
                                .as_array()
                                .unwrap()
                                .iter()
                                .any(|o| o["kind"] == "send" && o["cm"] == cm.as_str());
                            if !still {
                                dropped.insert(cm.clone());
                            }
                        }
                    } else {
                        let _ = w.ask(json!({"cmd": "edit", "conv_id": c, "client_msg_id": cm, "text": format!("e{}", d.below(1000))}));
                    }
                }
            }
            6 => {
                let c = CONVS[d.below(2) as usize];
                w.ask(json!({"cmd": "read", "conv_id": c})).unwrap();
            }
            7..=9 => {
                let c = CONVS[d.below(3) as usize];
                w.other_says(c, &format!("o{}", d.below(1_000_000)));
            }
            10 => w.cut(1006, ""),
            11 => {
                w.deliver(vec![json!({"type": "goaway", "reason": "server_restart", "retry_after_ms": d.below(5000)}).to_string()]);
                w.cut(1012, "server_restart");
            }
            12 => w.lose_answers += 1,
            13 => {
                if d.chance(50) {
                    w.lose_pushes += 1
                } else {
                    w.server.overflow += 1
                }
            }
            14 => {
                let e = if d.chance(50) {
                    json!({"code": "overloaded", "message": "busy", "retry_after_ms": d.below(3000)})
                } else {
                    json!({"code": "rate_limited", "message": "slow"})
                };
                let op = ["send", "edit", "sync", "recall"][d.below(4) as usize];
                w.server.reject.entry(op.into()).or_default().push_back(e);
            }
            15 => {
                online = !online;
                generation += 1;
                w.input(Input::Network { online, generation });
            }
            16 => {
                store_full = !store_full;
                if store_full {
                    w.store_fail = Some("full".into());
                } else {
                    w.store_fail = None;
                    w.input(Input::StorageRecovered);
                }
            }
            17 if d.chance(25) => {
                w.restart(seed + n);
                for c in CONVS {
                    w.cmd(json!({"cmd": "messages", "conv_id": c}));
                }
                if open {
                    w.cmd(json!({"cmd": "connect"}));
                }
            }
            18 => {
                open = !open;
                w.cmd(json!({"cmd": if open { "connect" } else { "close" }}));
            }
            _ => {}
        }
        w.run(d.below(4_000) as i64);
    }
    // The faults stop.
    w.lose_answers = 0;
    w.lose_pushes = 0;
    w.server.overflow = 0;
    w.server.reject.clear();
    w.store_fail = None;
    w.input(Input::StorageRecovered);
    generation += 1;
    w.input(Input::Network {
        online: true,
        generation,
    });
    w.cmd(json!({"cmd": "connect"}));
    w.run(15 * 60_000);
    assert_eq!(w.state(), "connected", "seed {seed}");
    w.assert_converged();
    // Every send, once, in order; refused ones failed and still shown.
    for (c, cms) in &sent {
        let applied: Vec<&String> = w
            .server
            .applied_cms
            .iter()
            .filter(|cm| cms.contains(cm))
            .collect();
        let want: Vec<&String> = cms.iter().filter(|cm| !dropped.contains(*cm)).collect();
        if c == FORBIDDEN {
            assert!(applied.is_empty());
            let failed = w.view(c).iter().filter(|m| m["status"] == "failed").count();
            assert_eq!(
                failed,
                want.len(),
                "seed {seed}: refused sends show as failed"
            );
        } else {
            assert_eq!(applied, want, "seed {seed}: sends to {c} once, in order");
        }
    }
    // The queue holds only what the server refused.
    for o in w.core.debug_state()["queue"].as_array().unwrap() {
        assert!(
            !o["failed"].is_null(),
            "seed {seed}: left in the queue: {o}"
        );
    }
    // The view: each key once, in order, sent ones equal to the server.
    for c in CONVS {
        let v = w.view(c);
        let orders: Vec<i64> = v.iter().map(|m| m["order"].as_i64().unwrap()).collect();
        assert!(
            orders.windows(2).all(|p| p[0] < p[1]),
            "seed {seed}: {c} out of order"
        );
        let keys: HashSet<&str> = v.iter().map(|m| m["key"].as_str().unwrap()).collect();
        assert_eq!(keys.len(), v.len());
        // What the core shows is what the app was told.
        let r = w.ask(json!({"cmd": "messages", "conv_id": c})).unwrap();
        let fresh: Vec<Value> = r["messages"].as_array().unwrap().clone();
        let mut fresh_keys: Vec<&str> = fresh.iter().map(|m| m["key"].as_str().unwrap()).collect();
        let mut told: Vec<&str> = v.iter().map(|m| m["key"].as_str().unwrap()).collect();
        fresh_keys.sort();
        told.sort();
        assert_eq!(
            fresh_keys, told,
            "seed {seed}: the app's copy of {c} drifted"
        );
    }
    // The store matches the core.
    for c in CONVS {
        if let Some(row) = w.store.convs.get(c) {
            assert_eq!(
                row.last_seq,
                w.server.last(c),
                "seed {seed}: stored position of {c}"
            );
        }
    }
}

#[test]
fn simulate() {
    if let Ok(s) = std::env::var("AGENTIM_SIM_SEED") {
        return run_seed(s.parse().unwrap());
    }
    let seeds: u64 = std::env::var("AGENTIM_SIM_SEEDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(300);
    for seed in 1..=seeds {
        let r = std::panic::catch_unwind(|| run_seed(seed));
        if let Err(e) = r {
            let msg = e
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_default();
            panic!(
                "seed {seed} failed: {msg}\nrerun: AGENTIM_SIM_SEED={seed} cargo test --test simulate"
            );
        }
    }
}
