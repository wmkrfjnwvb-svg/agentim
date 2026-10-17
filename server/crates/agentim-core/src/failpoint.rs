//! Named fault points for tests (docs/11 §12.1): a test makes a point
//! return an error, wait, or pause until released. The writer's points act
//! only on batches that carry client requests. One set per hub, so a
//! test with several nodes in one process can fault one node. With no
//! point set, a hit costs one atomic load.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// Points the writer passes in every batch.
pub const BEFORE_FENCE: &str = "batch.before_fence";
pub const BEFORE_COMMIT: &str = "batch.before_commit";
pub const AFTER_COMMIT: &str = "batch.after_commit";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// The step fails as if the database had answered with an error.
    Error,
    Delay(Duration),
    /// Waits until `release`.
    Pause,
}

struct Point {
    action: Action,
    /// Hits left before the point turns itself off (None: until cleared).
    times: Option<u32>,
    hits: u64,
    gate: Arc<tokio::sync::Notify>,
}

#[derive(Default)]
pub struct Failpoints {
    any: AtomicBool,
    points: Mutex<HashMap<String, Point>>,
    hits: Mutex<HashMap<String, u64>>,
    hit: tokio::sync::Notify,
}

impl Failpoints {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Point>> {
        self.points.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Arms a point; `times` limits how many hits it acts on.
    pub fn set(&self, name: &str, action: Action, times: Option<u32>) {
        self.lock().insert(
            name.to_string(),
            Point {
                action,
                times,
                hits: 0,
                gate: Arc::new(tokio::sync::Notify::new()),
            },
        );
        self.any.store(true, Ordering::SeqCst);
    }

    /// Disarms a point and lets everything paused there go on.
    pub fn release(&self, name: &str) {
        let mut g = self.lock();
        if let Some(p) = g.remove(name) {
            p.gate.notify_waiters();
        }
        self.any.store(!g.is_empty(), Ordering::SeqCst);
    }

    /// How often a point acted.
    pub fn hits(&self, name: &str) -> u64 {
        *self
            .hits
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(name)
            .unwrap_or(&0)
    }

    /// Waits until a point has acted at least `n` times.
    pub async fn wait_hits(&self, name: &str, n: u64) {
        loop {
            let w = self.hit.notified();
            if self.hits(name) >= n {
                return;
            }
            w.await;
        }
    }

    /// Passes the point: Err when it is set to fail.
    pub async fn hit(&self, name: &str) -> Result<(), String> {
        if !self.any.load(Ordering::Relaxed) {
            return Ok(());
        }
        let (action, gate) = {
            let mut g = self.lock();
            let Some(p) = g.get_mut(name) else {
                return Ok(());
            };
            if p.times == Some(0) {
                // Used up (a pause stays armed for those already waiting).
                return Ok(());
            }
            p.hits += 1;
            let r = (p.action, p.gate.clone());
            if let Some(t) = &mut p.times {
                *t = t.saturating_sub(1);
                if *t == 0 && p.action != Action::Pause {
                    g.remove(name);
                }
            }
            r
        };
        *self
            .hits
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(name.to_string())
            .or_default() += 1;
        self.hit.notify_waiters();
        match action {
            Action::Error => Err(format!("injected failure at {name}")),
            Action::Delay(d) => {
                tokio::time::sleep(d).await;
                Ok(())
            }
            Action::Pause => {
                loop {
                    let n = gate.notified();
                    if !self
                        .lock()
                        .get(name)
                        .is_some_and(|p| Arc::ptr_eq(&p.gate, &gate))
                    {
                        break;
                    }
                    n.await;
                }
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn points_fail_wait_and_pause() {
        let f = Arc::new(Failpoints::default());
        assert!(f.hit("x").await.is_ok());
        f.set("x", Action::Error, Some(2));
        assert!(f.hit("x").await.is_err());
        assert!(f.hit("x").await.is_err());
        assert!(f.hit("x").await.is_ok(), "turns itself off after two hits");
        assert_eq!(f.hits("x"), 2);
        f.set("p", Action::Pause, None);
        let f2 = f.clone();
        let t = tokio::spawn(async move { f2.hit("p").await });
        f.wait_hits("p", 1).await;
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(!t.is_finished(), "paused");
        f.release("p");
        assert!(t.await.unwrap().is_ok());
        assert!(f.hit("p").await.is_ok(), "released points are off");
        f.set("d", Action::Delay(Duration::from_millis(30)), Some(1));
        let t0 = std::time::Instant::now();
        f.hit("d").await.unwrap();
        assert!(t0.elapsed() >= Duration::from_millis(30));
    }
}
