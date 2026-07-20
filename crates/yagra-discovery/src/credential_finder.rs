// SPDX-License-Identifier: AGPL-3.0-only
//! Credential Finder rate limiting (ADR-018).
//!
//! When probing a device with candidate credentials, attempts must be **rate-limited per
//! device** so the finder never trips an account lockout. Defaults (all configurable):
//! serial attempts spaced ≥ 2 s, and after 3 consecutive failures a 15-minute cooldown —
//! deliberately below the common 5-attempt lockout threshold. This is the pure gate; the
//! actual auth attempts run through `yagra_transport`. Time is injected (Unix ms) so it is
//! deterministic and testable without a clock. Attempted credentials are never logged.

use std::collections::HashMap;
use std::hash::Hash;
use yagra_common::NodeId;

/// Tunables for credential probing.
#[derive(Debug, Clone, Copy)]
pub struct LimiterConfig {
    /// Minimum spacing between attempts on the same device.
    pub min_interval_ms: i64,
    /// Consecutive failures before a device enters cooldown.
    pub max_consecutive_failures: u32,
    /// How long a device stays in cooldown after tripping the failure limit.
    pub cooldown_ms: i64,
}

impl Default for LimiterConfig {
    fn default() -> Self {
        Self {
            min_interval_ms: 2_000,       // 2 s between attempts
            max_consecutive_failures: 3,  // below the usual 5-attempt lockout
            cooldown_ms: 15 * 60 * 1_000, // 15 min cooldown
        }
    }
}

/// Whether a probe attempt may proceed right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttemptDecision {
    /// Go ahead — the attempt has been recorded.
    Allow,
    /// Too soon since the last attempt; retry after `wait_ms`.
    TooSoon { wait_ms: i64 },
    /// Device is in lockout-protection cooldown until `until_ms`.
    CoolingDown { until_ms: i64 },
}

#[derive(Debug, Default, Clone, Copy)]
struct DeviceState {
    last_attempt_ms: Option<i64>,
    consecutive_failures: u32,
    cooldown_until_ms: Option<i64>,
}

/// Per-device probe limiter enforcing spacing and post-failure cooldown. Generic over the device
/// key so it serves both imported nodes (`NodeId`, the default) and in-flight discovery targets
/// keyed by `IpAddr` (the poller sweep has no `NodeId` yet).
#[derive(Debug, Clone)]
pub struct CredentialProbeLimiter<K = NodeId> {
    config: LimiterConfig,
    devices: HashMap<K, DeviceState>,
}

impl<K: Eq + Hash> CredentialProbeLimiter<K> {
    /// New limiter with the given config.
    #[must_use]
    pub fn new(config: LimiterConfig) -> Self {
        Self {
            config,
            devices: HashMap::new(),
        }
    }

    /// New limiter with default (conservative) config.
    #[must_use]
    pub fn with_defaults() -> Self {
        Self::new(LimiterConfig::default())
    }

    /// Decide whether a probe on `device` may proceed at `now_ms`. On [`AttemptDecision::Allow`]
    /// the attempt is recorded (advancing the spacing clock).
    pub fn begin_attempt(&mut self, device: K, now_ms: i64) -> AttemptDecision {
        let cfg = self.config;
        let state = self.devices.entry(device).or_default();

        if let Some(until) = state.cooldown_until_ms {
            if now_ms < until {
                return AttemptDecision::CoolingDown { until_ms: until };
            }
            // Cooldown elapsed — clear it and let the failure count reset.
            state.cooldown_until_ms = None;
            state.consecutive_failures = 0;
        }

        if let Some(last) = state.last_attempt_ms {
            let earliest = last + cfg.min_interval_ms;
            if now_ms < earliest {
                return AttemptDecision::TooSoon {
                    wait_ms: earliest - now_ms,
                };
            }
        }

        state.last_attempt_ms = Some(now_ms);
        AttemptDecision::Allow
    }

    /// Record that the last attempt on `device` failed. Trips cooldown at the failure limit.
    pub fn record_failure(&mut self, device: K, now_ms: i64) {
        let cfg = self.config;
        let state = self.devices.entry(device).or_default();
        state.consecutive_failures += 1;
        if state.consecutive_failures >= cfg.max_consecutive_failures {
            state.cooldown_until_ms = Some(now_ms + cfg.cooldown_ms);
        }
    }

    /// Record that a probe on `device` succeeded — clears failures and any cooldown. The
    /// finder stops on first success (ADR-018), so this also ends probing for the device.
    pub fn record_success(&mut self, device: K) {
        let state = self.devices.entry(device).or_default();
        state.consecutive_failures = 0;
        state.cooldown_until_ms = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limiter() -> CredentialProbeLimiter {
        CredentialProbeLimiter::with_defaults()
    }

    #[test]
    fn first_attempt_is_allowed() {
        let mut l = limiter();
        assert_eq!(l.begin_attempt(NodeId::new(), 0), AttemptDecision::Allow);
    }

    #[test]
    fn second_attempt_too_soon_then_allowed_after_interval() {
        let mut l = limiter();
        let d = NodeId::new();
        assert_eq!(l.begin_attempt(d, 0), AttemptDecision::Allow);
        assert_eq!(
            l.begin_attempt(d, 500),
            AttemptDecision::TooSoon { wait_ms: 1_500 }
        );
        assert_eq!(l.begin_attempt(d, 2_000), AttemptDecision::Allow);
    }

    #[test]
    fn cooldown_after_consecutive_failures() {
        let mut l = limiter();
        let d = NodeId::new();
        // Three spaced failed attempts trip the 15-min cooldown.
        for i in 0..3 {
            let now = i * 2_000;
            assert_eq!(l.begin_attempt(d, now), AttemptDecision::Allow);
            l.record_failure(d, now);
        }
        // Even after the spacing interval, the device is cooling down.
        match l.begin_attempt(d, 6_000) {
            AttemptDecision::CoolingDown { until_ms } => {
                assert_eq!(until_ms, 4_000 + 15 * 60 * 1_000);
            }
            other => panic!("expected cooldown, got {other:?}"),
        }
    }

    #[test]
    fn success_resets_failures() {
        let mut l = limiter();
        let d = NodeId::new();
        l.begin_attempt(d, 0);
        l.record_failure(d, 0);
        l.begin_attempt(d, 2_000);
        l.record_failure(d, 2_000);
        l.record_success(d); // matched a credential
                             // Next attempt is spaced but not in cooldown.
        assert_eq!(l.begin_attempt(d, 4_000), AttemptDecision::Allow);
    }

    #[test]
    fn cooldown_lifts_after_window() {
        let mut l = limiter();
        let d = NodeId::new();
        for i in 0..3 {
            let now = i * 2_000;
            l.begin_attempt(d, now);
            l.record_failure(d, now);
        }
        let cooldown_end = 4_000 + 15 * 60 * 1_000;
        assert_eq!(l.begin_attempt(d, cooldown_end), AttemptDecision::Allow);
    }
}
