//! A tiny in-memory fixed-window rate limiter.
//!
//! Good enough for a single instance protecting login/register from brute
//! force. With several app instances behind a load balancer each instance
//! would keep its own counters; the article "Rate limiting" explains how to
//! move this into Redis for a shared limit.

use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, Instant},
};

#[derive(Default)]
pub struct RateLimiter {
    windows: Mutex<HashMap<String, Window>>,
}

struct Window {
    started: Instant,
    count: u32,
}

const MAX_TRACKED_KEYS: usize = 50_000;

impl RateLimiter {
    /// Records one hit for `key` and returns whether it is still within
    /// `limit` hits per `window`.
    pub fn check(&self, key: &str, limit: u32, window: Duration) -> bool {
        let now = Instant::now();
        let mut map = self.windows.lock().unwrap_or_else(|e| e.into_inner());

        // Opportunistic cleanup so the map cannot grow without bound.
        if map.len() >= MAX_TRACKED_KEYS {
            map.retain(|_, w| now.duration_since(w.started) < window);
        }

        let entry = map.entry(key.to_string()).or_insert(Window { started: now, count: 0 });
        if now.duration_since(entry.started) >= window {
            entry.started = now;
            entry.count = 0;
        }
        entry.count += 1;
        entry.count <= limit
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_up_to_limit_then_blocks() {
        let rl = RateLimiter::default();
        let w = Duration::from_secs(60);
        assert!(rl.check("k", 2, w));
        assert!(rl.check("k", 2, w));
        assert!(!rl.check("k", 2, w));
        assert!(rl.check("other", 2, w));
    }

    #[test]
    fn window_resets() {
        let rl = RateLimiter::default();
        let w = Duration::from_millis(1);
        assert!(rl.check("k", 1, w));
        std::thread::sleep(Duration::from_millis(5));
        assert!(rl.check("k", 1, w));
    }
}
