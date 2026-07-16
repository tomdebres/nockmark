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
        // If everything is still fresh (active spray within one window), the
        // retain above removed nothing — clear outright. Tradeoff: counters
        // reset, so a sprayer can at most double another key's allowance per
        // window; memory stays bounded at ~10k entries regardless.
        if hits.len() > 10_000 {
            hits.clear();
        }
        // Legit keys are IPs (<=45 chars for IPv6); cap so spoofed
        // X-Forwarded-For values can't make entries arbitrarily large.
        let key: String = if key.len() > 45 {
            key.chars().take(45).collect()
        } else {
            key.to_string()
        };
        let entry = hits.entry(key).or_insert((now, 0));
        if now.duration_since(entry.0) >= self.window {
            *entry = (now, 0);
        }
        entry.1 += 1;
        entry.1 <= self.max_per_window
    }

    #[cfg(test)]
    pub(crate) fn tracked_keys(&self) -> usize {
        self.hits.lock().unwrap().len()
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

    #[test]
    fn spraying_distinct_keys_cannot_grow_the_map_unboundedly() {
        let rl = RateLimiter::new(10, Duration::from_secs(60));
        for i in 0..12_000 {
            rl.check(&format!("key-{i}"));
        }
        assert!(rl.tracked_keys() <= 10_001);
    }
}
