// SPDX-License-Identifier: AGPL-3.0-only
//! Per-source + global token-bucket rate limiting for the edge listeners.
//!
//! Storm protection layer 1 (see the event-pipeline design): a chatty or misbehaving
//! device is capped per source IP, and a spoofed many-source flood is capped globally.
//! The per-IP bucket map is bounded (FIFO eviction at [`MAX_TRACKED_SOURCES`]) so an
//! attacker cycling source addresses can't exhaust memory. Pure logic — the caller
//! injects `now_ms`, so refill behaviour is fully unit-testable.

use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;

/// Upper bound on tracked per-source buckets. Beyond this the oldest-inserted bucket is
/// evicted (FIFO, not strict LRU — O(1) and good enough for abuse bounding; a re-inserted
/// source simply starts with a fresh full bucket).
pub const MAX_TRACKED_SOURCES: usize = 10_000;

/// A single token bucket.
#[derive(Debug, Clone, Copy)]
struct Bucket {
    tokens: f64,
    last_refill_ms: i64,
}

impl Bucket {
    fn new(burst: f64, now_ms: i64) -> Self {
        Self {
            tokens: burst,
            last_refill_ms: now_ms,
        }
    }

    /// Refill for elapsed time, then try to take one token.
    fn allow(&mut self, rate_per_sec: f64, burst: f64, now_ms: i64) -> bool {
        let elapsed_ms = (now_ms - self.last_refill_ms).max(0) as f64;
        self.tokens = (self.tokens + rate_per_sec * elapsed_ms / 1000.0).min(burst);
        self.last_refill_ms = now_ms;
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

/// Rate limiter combining a per-source-IP bucket with a global bucket. An event passes
/// only if both allow it.
pub struct SourceLimiter {
    per_source_rate: f64,
    per_source_burst: f64,
    global_rate: f64,
    global_burst: f64,
    global: Bucket,
    sources: HashMap<IpAddr, Bucket>,
    insertion_order: VecDeque<IpAddr>,
}

impl SourceLimiter {
    /// New limiter. Bursts are fixed at 2× the sustained rate (matching the listener
    /// env-var contract: `YAGRA_EVENT_RATE_PER_SOURCE` / `YAGRA_EVENT_RATE_GLOBAL`),
    /// floored at one whole token so a very low rate still passes *something*.
    #[must_use]
    pub fn new(per_source_rate: f64, global_rate: f64, now_ms: i64) -> Self {
        let per_source_rate = per_source_rate.max(0.1);
        let global_rate = global_rate.max(0.1);
        let per_source_burst = (per_source_rate * 2.0).max(1.0);
        let global_burst = (global_rate * 2.0).max(1.0);
        Self {
            per_source_rate,
            per_source_burst,
            global_rate,
            global_burst,
            global: Bucket::new(global_burst, now_ms),
            sources: HashMap::new(),
            insertion_order: VecDeque::new(),
        }
    }

    /// Whether an event from `source` may pass at `now_ms`. Consumes one token from the
    /// source's bucket and, if that allows, one from the global bucket.
    pub fn allow(&mut self, source: IpAddr, now_ms: i64) -> bool {
        if !self.sources.contains_key(&source) {
            if self.sources.len() >= MAX_TRACKED_SOURCES {
                if let Some(oldest) = self.insertion_order.pop_front() {
                    self.sources.remove(&oldest);
                }
            }
            self.sources
                .insert(source, Bucket::new(self.per_source_burst, now_ms));
            self.insertion_order.push_back(source);
        }
        let bucket = self
            .sources
            .get_mut(&source)
            .expect("bucket inserted above");
        if !bucket.allow(self.per_source_rate, self.per_source_burst, now_ms) {
            return false;
        }
        self.global
            .allow(self.global_rate, self.global_burst, now_ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    fn ip(last: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(10, 0, 0, last))
    }

    fn ip_from(n: u32) -> IpAddr {
        IpAddr::V4(Ipv4Addr::from(0x0A00_0000 + n))
    }

    #[test]
    fn burst_then_throttle_then_refill() {
        // 10/s per source (burst 20), roomy global.
        let mut limiter = SourceLimiter::new(10.0, 100_000.0, 0);
        // The full burst passes...
        for i in 0..20 {
            assert!(limiter.allow(ip(1), 0), "burst event {i} should pass");
        }
        // ...then the bucket is empty.
        assert!(!limiter.allow(ip(1), 0));
        // 100ms later one token has refilled (10/s = 1 per 100ms).
        assert!(limiter.allow(ip(1), 100));
        assert!(!limiter.allow(ip(1), 100));
        // A long quiet period refills back to the burst cap, no further.
        for i in 0..20 {
            assert!(limiter.allow(ip(1), 60_000), "refilled event {i}");
        }
        assert!(!limiter.allow(ip(1), 60_000));
    }

    #[test]
    fn sources_are_isolated() {
        let mut limiter = SourceLimiter::new(1.0, 100_000.0, 0);
        assert!(limiter.allow(ip(1), 0));
        assert!(limiter.allow(ip(1), 0));
        assert!(!limiter.allow(ip(1), 0)); // ip1 exhausted (burst 2)
        assert!(limiter.allow(ip(2), 0)); // ip2 unaffected
    }

    #[test]
    fn global_bucket_caps_many_sources() {
        // Per-source generous, global tiny: 1/s (burst 2).
        let mut limiter = SourceLimiter::new(1000.0, 1.0, 0);
        assert!(limiter.allow(ip(1), 0));
        assert!(limiter.allow(ip(2), 0));
        assert!(!limiter.allow(ip(3), 0), "global bucket exhausted");
    }

    #[test]
    fn tracked_sources_are_bounded_with_fifo_eviction() {
        let mut limiter = SourceLimiter::new(10.0, 1e9, 0);
        for n in 0..(MAX_TRACKED_SOURCES as u32 + 500) {
            let _ = limiter.allow(ip_from(n), 0);
        }
        assert!(limiter.sources.len() <= MAX_TRACKED_SOURCES);
        assert_eq!(limiter.sources.len(), limiter.insertion_order.len());
    }

    #[test]
    fn zero_or_negative_rates_are_clamped_sane() {
        let mut limiter = SourceLimiter::new(0.0, -5.0, 0);
        // Clamped to a tiny positive rate — first (burst) events still pass, no div-by-zero.
        assert!(limiter.allow(ip(1), 0));
    }

    #[test]
    fn clock_going_backwards_is_tolerated() {
        let mut limiter = SourceLimiter::new(1.0, 1000.0, 10_000);
        assert!(limiter.allow(ip(1), 10_000));
        // now < last_refill: no refill (and no negative-token explosion), burst still holds.
        assert!(limiter.allow(ip(1), 5_000));
        assert!(!limiter.allow(ip(1), 5_000));
    }
}
