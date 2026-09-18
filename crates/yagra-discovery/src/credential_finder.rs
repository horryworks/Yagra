// SPDX-License-Identifier: AGPL-3.0-only
//! Credential Finder rate limiting (ADR-018).
//!
//! When probing a device with candidate credentials, attempts must be **rate-limited per
//! device** so the finder never trips an account lockout. This is the pure gate; the
//! actual auth attempts run through `yagra_transport`. Time is injected (Unix ms) so it is
//! deterministic and testable without a clock. Attempted credentials are never logged.
//!
//! **Two mechanisms, and they protect against different things** (ADR-161):
//!
//! * **Spacing** (`min_interval_ms`, 2 s) paces attempts at one device. It applies to every
//!   credential kind and is what `security.md` asks for.
//! * **Cooldown** (`max_consecutive_failures` + `cooldown_ms`) stops probing a device
//!   altogether. It exists for a credential kind whose repeated failures **lock an account
//!   out** — SSH/CLI login, which ADR-018 anticipated and which does not exist yet. The 3
//!   attempts / 15 minutes are sized to sit below the common 5-attempt lockout threshold.
//!
//! 🚨 **SNMP has no account to lock out, so its sweep passes `max_consecutive_failures: None`**
//! ([`LimiterConfig::snmp_sweep`]). A v2c community is not a login and USM has no standard
//! lockout, so a cooldown there deletes candidate credentials and buys nothing. It shipped as
//! `Some(3)` and silently dropped every credential after the third — the operator's correct
//! community among them (ADR-161).
//!
//! 🚨 **A cooldown is only worth anything if the limiter outlives what it is protecting.**
//! `yagra-poller`'s `probe_one` builds one **per target** and drops it on return, so a
//! cooldown set there could never span two sweeps, two targets, or even one re-run a second
//! later. When SSH/CLI probing lands, its limiter has to be hoisted out of the per-target
//! call — otherwise it will be exactly as inert, and look exactly as protective.

use std::collections::HashMap;
use std::hash::Hash;
use yagra_common::NodeId;

/// Tunables for credential probing.
#[derive(Debug, Clone, Copy)]
pub struct LimiterConfig {
    /// Minimum spacing between attempts on the same device.
    pub min_interval_ms: i64,
    /// Consecutive failures before a device enters cooldown, or `None` when this credential
    /// kind **cannot lock an account out** and so must never have its candidate list cut
    /// short. See the module doc; `None` is what SNMP uses (ADR-161).
    ///
    /// Deliberately an `Option` rather than a large sentinel: `u32::MAX` says "a very big
    /// number" where the truth is "this question does not apply", and the test helper that
    /// already spelled it that way is how the shipped value of `3` went unexercised.
    pub max_consecutive_failures: Option<u32>,
    /// How long a device stays in cooldown after tripping the failure limit. Unused when
    /// `max_consecutive_failures` is `None`.
    pub cooldown_ms: i64,
}

impl Default for LimiterConfig {
    /// The lockout-protecting profile: for a credential kind where repeated failures lock an
    /// account (SSH/CLI, ADR-018). **Not what the SNMP sweep uses** — see
    /// [`LimiterConfig::snmp_sweep`].
    fn default() -> Self {
        Self {
            min_interval_ms: 2_000,            // 2 s between attempts
            max_consecutive_failures: Some(3), // below the usual 5-attempt lockout
            cooldown_ms: 15 * 60 * 1_000,      // 15 min cooldown
        }
    }
}

impl LimiterConfig {
    /// The profile the discovery sweep uses: spacing, and **no cooldown** (ADR-161).
    ///
    /// SNMP has nothing to lock out — a v2c community is not an account, and USM defines no
    /// lockout — so a consecutive-failure limit here does not protect the device, it only
    /// stops trying the operator's remaining credentials. And every non-answer looks alike to
    /// the prober: a dropped packet, an ACL on 161 and a wrong community all arrive as
    /// silence, so a budget of 3 was spent on things that say nothing about the credential.
    #[must_use]
    pub fn snmp_sweep() -> Self {
        Self {
            min_interval_ms: 2_000,
            max_consecutive_failures: None,
            cooldown_ms: 0,
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

    /// Record that the last attempt on `device` failed. Trips cooldown at the failure limit,
    /// if this config has one — a `None` limit means failures are counted and never acted on,
    /// so the caller's whole candidate list is tried (ADR-161).
    pub fn record_failure(&mut self, device: K, now_ms: i64) {
        let cfg = self.config;
        let state = self.devices.entry(device).or_default();
        state.consecutive_failures += 1;
        if let Some(max) = cfg.max_consecutive_failures {
            if state.consecutive_failures >= max {
                state.cooldown_until_ms = Some(now_ms + cfg.cooldown_ms);
            }
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
        CredentialProbeLimiter::new(LimiterConfig::default())
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

    /// ADR-161: the cooldown mechanism is still alive for the credential kind it exists for
    /// (SSH/CLI, which locks accounts out). Stated on its own so that making SNMP exempt cannot
    /// quietly turn the whole thing into dead code.
    #[test]
    fn a_config_with_a_failure_limit_still_backs_off() {
        let mut l = CredentialProbeLimiter::<NodeId>::new(LimiterConfig {
            min_interval_ms: 0,
            max_consecutive_failures: Some(2),
            cooldown_ms: 60_000,
        });
        let d = NodeId::new();
        for i in 0..2 {
            assert_eq!(l.begin_attempt(d, i), AttemptDecision::Allow);
            l.record_failure(d, i);
        }
        assert!(
            matches!(l.begin_attempt(d, 2), AttemptDecision::CoolingDown { .. }),
            "two failures under a limit of two must back the device off"
        );
    }

    /// ADR-161: `None` means the failures are counted and never acted on — the caller gets its
    /// whole candidate list. This is what the SNMP sweep relies on, and it is the half that
    /// shipped wrong: `Some(3)` deleted every credential after the third.
    #[test]
    fn a_config_with_no_failure_limit_never_backs_off() {
        let mut l = CredentialProbeLimiter::<NodeId>::new(LimiterConfig {
            min_interval_ms: 0,
            max_consecutive_failures: None,
            cooldown_ms: 60_000,
        });
        let d = NodeId::new();
        for i in 0..50 {
            assert_eq!(
                l.begin_attempt(d, i),
                AttemptDecision::Allow,
                "attempt {i} must be allowed: nothing here can lock out"
            );
            l.record_failure(d, i);
        }
    }

    /// The shipped SNMP profile, named rather than reconstructed — a test that builds its own
    /// config proves nothing about the one `probe_one` passes.
    #[test]
    fn the_snmp_sweep_profile_spaces_attempts_and_has_no_cooldown() {
        let cfg = LimiterConfig::snmp_sweep();
        assert_eq!(cfg.max_consecutive_failures, None);
        assert!(
            cfg.min_interval_ms > 0,
            "spacing is the half that does protect the device, and must survive"
        );
        let mut l = CredentialProbeLimiter::<NodeId>::new(cfg);
        let d = NodeId::new();
        let mut now = 0;
        for _ in 0..10 {
            loop {
                match l.begin_attempt(d, now) {
                    AttemptDecision::Allow => break,
                    AttemptDecision::TooSoon { wait_ms } => now += wait_ms,
                    AttemptDecision::CoolingDown { .. } => {
                        panic!("the SNMP profile must never cool down")
                    }
                }
            }
            l.record_failure(d, now);
        }
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
