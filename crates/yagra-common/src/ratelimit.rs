// SPDX-License-Identifier: AGPL-3.0-only
//! One token bucket (ADR-184).
//!
//! Four were written by hand — the edge listeners' per-source limiter, the webhook ingest gate,
//! the forwarding sender and the login throttle — each the same five lines of refill arithmetic,
//! two against the wall clock and two against `Instant`. What differs between them is policy (how
//! big a burst, what the key is, what happens when the map grows), and that stays with each caller.
//! What is here is the arithmetic, with the caller supplying `now_ms` so it is testable and so the
//! caller decides which clock it trusts: a limiter that guards against abuse should pass
//! [`crate::clock::monotonic_ms`], which a stepped wall clock cannot refill.

/// A token bucket: `rate_per_sec` tokens refilled continuously, holding at most `burst`, starting
/// full.
#[derive(Debug, Clone, Copy)]
pub struct TokenBucket {
    rate_per_sec: f64,
    burst: f64,
    tokens: f64,
    last_ms: i64,
}

impl TokenBucket {
    /// A full bucket. A rate or burst that is not positive is floored — a limiter configured to
    /// zero still lets its first request through rather than dividing by it.
    #[must_use]
    pub fn new(rate_per_sec: f64, burst: f64, now_ms: i64) -> Self {
        let burst = burst.max(1.0);
        Self {
            rate_per_sec: rate_per_sec.max(f64::MIN_POSITIVE),
            burst,
            tokens: burst,
            last_ms: now_ms,
        }
    }

    /// Refill for the time since the last call, then try to take one token. A `now_ms` earlier
    /// than the last one refills nothing rather than draining the bucket.
    pub fn take(&mut self, now_ms: i64) -> bool {
        let elapsed_ms = (now_ms - self.last_ms).max(0) as f64;
        self.tokens = (self.tokens + self.rate_per_sec * elapsed_ms / 1000.0).min(self.burst);
        self.last_ms = self.last_ms.max(now_ms);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    /// Whole seconds until the next token, at least 1 — the value a `Retry-After` header carries.
    #[must_use]
    pub fn retry_after_secs(&self) -> u64 {
        let missing = (1.0 - self.tokens).max(0.0);
        (missing / self.rate_per_sec).ceil().max(1.0) as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bucket_passes_its_burst_then_refuses_then_refills_to_the_burst_and_no_further() {
        let mut b = TokenBucket::new(10.0, 20.0, 0);
        for i in 0..20 {
            assert!(b.take(0), "burst request {i}");
        }
        assert!(!b.take(0), "the burst is spent");
        assert!(b.take(100), "100 ms at 10/s is one token");
        assert!(!b.take(100));
        for _ in 0..20 {
            assert!(b.take(1_000_000));
        }
        assert!(
            !b.take(1_000_000),
            "a long quiet period refills to the burst, not beyond"
        );
    }

    #[test]
    fn a_clock_that_goes_backwards_refills_nothing() {
        let mut b = TokenBucket::new(1.0, 1.0, 10_000);
        assert!(b.take(10_000));
        assert!(!b.take(0));
        assert!(
            !b.take(10_000),
            "going back did not earn a token or move the reference"
        );
        assert!(b.take(11_000));
    }

    #[test]
    fn a_rate_of_zero_still_lets_the_first_request_through() {
        let mut b = TokenBucket::new(0.0, 0.0, 0);
        assert!(b.take(0));
        assert!(!b.take(1_000));
    }

    #[test]
    fn the_retry_hint_is_the_time_to_the_next_token() {
        let mut b = TokenBucket::new(0.5, 1.0, 0);
        assert!(b.take(0));
        assert_eq!(b.retry_after_secs(), 2);
        let full = TokenBucket::new(10.0, 5.0, 0);
        assert_eq!(full.retry_after_secs(), 1, "never 0");
    }
}
