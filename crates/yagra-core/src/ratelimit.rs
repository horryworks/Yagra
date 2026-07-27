// SPDX-License-Identifier: AGPL-3.0-only
//! Admission control shared by the endpoints that launch bounded, expensive work.
//!
//! Two of these exist now — Troubleshoot analyses (ADR-028 Increment 2 WS-A) and AI-assisted RCA
//! (ADR-029) — and both reach the same conclusion: the guard rail for "this is a read, but an
//! expensive one" is a *cap*, not a role. A View-scoped client may legitimately ask for either; what
//! must be bounded is how many run at once and how fast new ones arrive.
//!
//! Both pieces are pure, which is the point: the interesting behaviour (pruning, the boundary at the
//! cap) is testable without a `PgPool`, a provider, or a clock that actually advances.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// Read a positive-`usize` cap from an env var, falling back to `default` when unset, unparseable or
/// zero. A zero is treated as "not configured" rather than "admit nothing": a typo in a deployment
/// should degrade to the documented default, not silently disable a feature.
pub(crate) fn env_cap(var: &str, default: usize) -> usize {
    std::env::var(var)
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(default)
}

/// Charge one admission against a sliding window: prune entries older than `window`, then admit iff
/// fewer than `max` remain (pushing `now` on admission).
///
/// `now` is a parameter rather than read from the clock so a test can advance time without sleeping.
pub(crate) fn charge_window(
    q: &mut VecDeque<Instant>,
    now: Instant,
    window: Duration,
    max: usize,
) -> bool {
    while let Some(&front) = q.front() {
        if now.duration_since(front) >= window {
            q.pop_front();
        } else {
            break;
        }
    }
    if q.len() >= max {
        return false;
    }
    q.push_back(now);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn charge_window_admits_under_cap_and_rejects_at_cap() {
        let mut q = VecDeque::new();
        let win = Duration::from_secs(60);
        let t0 = Instant::now();
        assert!(charge_window(&mut q, t0, win, 2), "1st admitted");
        assert!(charge_window(&mut q, t0, win, 2), "2nd admitted");
        assert!(!charge_window(&mut q, t0, win, 2), "3rd at cap rejected");
        assert_eq!(q.len(), 2);
    }

    #[test]
    fn charge_window_prunes_expired_then_readmits() {
        let mut q = VecDeque::new();
        let win = Duration::from_secs(60);
        let t0 = Instant::now();
        assert!(charge_window(&mut q, t0, win, 1));
        assert!(
            !charge_window(&mut q, t0, win, 1),
            "still within window ⇒ full"
        );
        // Advance past the window: the old entry is pruned and a new one is admitted.
        let t1 = t0 + Duration::from_secs(61);
        assert!(
            charge_window(&mut q, t1, win, 1),
            "expired entry pruned ⇒ readmit"
        );
        assert_eq!(q.len(), 1);
    }

    #[test]
    fn a_rejected_admission_is_not_charged() {
        // Otherwise a client hammering a full window would keep pushing its own expiry out and stay
        // locked out long after the legitimate traffic aged away.
        let mut q = VecDeque::new();
        let win = Duration::from_secs(60);
        let t0 = Instant::now();
        assert!(charge_window(&mut q, t0, win, 1));
        for _ in 0..5 {
            assert!(!charge_window(&mut q, t0 + Duration::from_secs(30), win, 1));
        }
        assert_eq!(q.len(), 1, "only the admitted call is recorded");
        assert!(charge_window(&mut q, t0 + Duration::from_secs(61), win, 1));
    }

    #[test]
    fn env_cap_falls_back_when_unset() {
        // An unset var yields the default; a zero/invalid value would too (filtered out).
        assert_eq!(env_cap("YAGRA_CAP_DEFINITELY_UNSET_XYZ", 4), 4);
    }
}
