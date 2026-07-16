//! Fixed-window per-key rate limiting for the two expensive POST routes.
//! In-app because Railway fronts us with no edge rate limiter (M2 review
//! carry-forward). Fixed-window (not sliding) is enough: the goal is to
//! bound verifier CPU (~0.5 s/proof), not to be fair at the margin.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub struct RateLimiter {
    max_per_window: u32,
    window: Duration,
    hits: Mutex<HashMap<String, (Instant, u32)>>,
}

impl RateLimiter {
    pub fn new(max_per_window: u32, window: Duration) -> Self {
        Self { max_per_window, window, hits: Mutex::new(HashMap::new()) }
    }

    /// Record a hit for `key`; false = over the limit for this window.
    pub fn check(&self, key: &str) -> bool {
        let now = Instant::now();
        let mut hits = self.hits.lock().unwrap();
        // Bound memory under key-spraying: drop expired windows once large.
        if hits.len() > 10_000 {
            hits.retain(|_, (t0, _)| now.duration_since(*t0) < self.window);
        }
        let entry = hits.entry(key.to_string()).or_insert((now, 0));
        if now.duration_since(entry.0) >= self.window {
            *entry = (now, 0);
        }
        entry.1 += 1;
        entry.1 <= self.max_per_window
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_up_to_max_then_blocks() {
        let rl = RateLimiter::new(2, Duration::from_secs(60));
        assert!(rl.check("a"));
        assert!(rl.check("a"));
        assert!(!rl.check("a"));
        assert!(rl.check("b"), "keys are independent");
    }

    #[test]
    fn window_expiry_resets_the_count() {
        let rl = RateLimiter::new(1, Duration::from_millis(50));
        assert!(rl.check("a"));
        assert!(!rl.check("a"));
        std::thread::sleep(Duration::from_millis(60));
        assert!(rl.check("a"), "new window after expiry");
    }
}
