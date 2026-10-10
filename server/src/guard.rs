//! Agent guardrails (community edition): hop limit, loop/duplicate breaker,
//! per-conversation rate limit and per-agent daily budget.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct GuardConfig {
    pub max_hop: i64,
    /// Trip when the same normalized content appears this many times among
    /// the agent's recent messages in the conversation (including the new one).
    pub dup_threshold: usize,
    pub dup_window: i64,
    pub rate_capacity: f64,
    pub rate_per_sec: f64,
    pub daily_messages: u64,
    pub daily_chars: u64,
    pub cooldown: Duration,
}

impl Default for GuardConfig {
    fn default() -> Self {
        GuardConfig {
            max_hop: 6,
            dup_threshold: 3,
            dup_window: 5,
            rate_capacity: 5.0,
            rate_per_sec: 1.0,
            daily_messages: 500,
            daily_chars: 200_000,
            cooldown: Duration::from_secs(60),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Allow,
    /// Rejected; the agent may retry later.
    Reject(&'static str, String),
    /// Rejected and the breaker for (agent, conversation) is now open.
    Trip(&'static str, String),
}

struct Bucket {
    tokens: f64,
    at: Instant,
}

#[derive(Default)]
pub struct Budget {
    day: i64,
    messages: u64,
    chars: u64,
}

pub type Budgets = Arc<Mutex<HashMap<String, Budget>>>;

pub struct Guards {
    pub cfg: GuardConfig,
    buckets: HashMap<(String, String), Bucket>,
    /// Daily budgets are per agent across all databases, so they are shared.
    budgets: Budgets,
    tripped: HashMap<(String, String), (Instant, String)>,
}

pub fn normalize(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric() || (*c as u32) > 0x2E80)
        .flat_map(|c| c.to_lowercase())
        .collect()
}

impl Guards {
    pub fn new(cfg: GuardConfig) -> Self {
        Self::with_budgets(cfg, Budgets::default())
    }

    pub fn with_budgets(cfg: GuardConfig, budgets: Budgets) -> Self {
        Guards {
            cfg,
            buckets: HashMap::new(),
            budgets,
            tripped: HashMap::new(),
        }
    }

    pub fn is_tripped(&mut self, agent: &str, conv: &str) -> Option<String> {
        let key = (agent.to_string(), conv.to_string());
        match self.tripped.get(&key) {
            Some((until, reason)) if Instant::now() < *until => Some(reason.clone()),
            Some(_) => {
                self.tripped.remove(&key);
                None
            }
            None => None,
        }
    }

    pub fn reset(&mut self, agent: &str, conv: &str) {
        self.tripped.remove(&(agent.to_string(), conv.to_string()));
    }

    /// Checks a new agent message. `recent` holds the agent's latest message
    /// contents in this conversation, newest first.
    pub fn check(
        &mut self,
        agent: &str,
        conv: &str,
        hop: i64,
        text: &str,
        recent: &[String],
    ) -> Verdict {
        if let Some(reason) = self.is_tripped(agent, conv) {
            return Verdict::Reject("circuit_open", reason);
        }
        if hop > self.cfg.max_hop {
            return self.trip(
                agent,
                conv,
                "hop_limit",
                format!("hop {hop} exceeds limit {}", self.cfg.max_hop),
            );
        }
        let norm = normalize(text);
        if !norm.is_empty() {
            let same = recent
                .iter()
                .take(self.cfg.dup_window as usize)
                .filter(|c| normalize(c) == norm)
                .count()
                + 1;
            if same >= self.cfg.dup_threshold {
                return self.trip(
                    agent,
                    conv,
                    "loop_detected",
                    format!("same content repeated {same} times"),
                );
            }
        }
        // Rate limit per (agent, conversation).
        let now = Instant::now();
        let b = self
            .buckets
            .entry((agent.to_string(), conv.to_string()))
            .or_insert(Bucket {
                tokens: self.cfg.rate_capacity,
                at: now,
            });
        b.tokens = (b.tokens + now.duration_since(b.at).as_secs_f64() * self.cfg.rate_per_sec)
            .min(self.cfg.rate_capacity);
        b.at = now;
        if b.tokens < 1.0 {
            return Verdict::Reject("rate_limited", "too many messages, slow down".into());
        }
        // Daily budget per agent.
        let day = crate::model::now_ms() / 86_400_000;
        let mut all = self.budgets.lock().unwrap_or_else(|e| e.into_inner());
        let bud = all.entry(agent.to_string()).or_default();
        if bud.day != day {
            *bud = Budget {
                day,
                ..Default::default()
            };
        }
        let chars = text.chars().count() as u64;
        if bud.messages + 1 > self.cfg.daily_messages || bud.chars + chars > self.cfg.daily_chars {
            return Verdict::Reject("budget_exceeded", "daily agent budget exhausted".into());
        }
        b.tokens -= 1.0;
        bud.messages += 1;
        bud.chars += chars;
        Verdict::Allow
    }

    /// Accounts streamed characters against the daily budget.
    pub fn charge_chars(&mut self, agent: &str, chars: u64) -> bool {
        let day = crate::model::now_ms() / 86_400_000;
        let mut all = self.budgets.lock().unwrap_or_else(|e| e.into_inner());
        let bud = all.entry(agent.to_string()).or_default();
        if bud.day != day {
            *bud = Budget {
                day,
                ..Default::default()
            };
        }
        bud.chars += chars;
        bud.chars <= self.cfg.daily_chars
    }

    fn trip(&mut self, agent: &str, conv: &str, code: &'static str, reason: String) -> Verdict {
        self.tripped.insert(
            (agent.to_string(), conv.to_string()),
            (
                Instant::now() + self.cfg.cooldown,
                format!("{code}: {reason}"),
            ),
        );
        Verdict::Trip(code, reason)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hop_limit_trips_and_stays_open() {
        let mut g = Guards::new(GuardConfig::default());
        assert_eq!(g.check("a", "c", 6, "x", &[]), Verdict::Allow);
        assert!(matches!(
            g.check("a", "c", 7, "y", &[]),
            Verdict::Trip("hop_limit", _)
        ));
        assert!(matches!(
            g.check("a", "c", 1, "z", &[]),
            Verdict::Reject("circuit_open", _)
        ));
        g.reset("a", "c");
        assert_eq!(g.check("a", "c", 1, "z", &[]), Verdict::Allow);
    }

    #[test]
    fn duplicate_content_trips() {
        let mut g = Guards::new(GuardConfig::default());
        let recent = vec!["Hello, world!".to_string(), "hello world".to_string()];
        assert!(matches!(
            g.check("a", "c", 1, "HELLO world", &recent),
            Verdict::Trip("loop_detected", _)
        ));
    }

    #[test]
    fn rate_limit_rejects_bursts() {
        let mut g = Guards::new(GuardConfig {
            rate_capacity: 2.0,
            rate_per_sec: 0.0001,
            ..Default::default()
        });
        assert_eq!(g.check("a", "c", 1, "1", &[]), Verdict::Allow);
        assert_eq!(g.check("a", "c", 1, "2", &[]), Verdict::Allow);
        assert!(matches!(
            g.check("a", "c", 1, "3", &[]),
            Verdict::Reject("rate_limited", _)
        ));
    }
}
