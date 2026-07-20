// SPDX-License-Identifier: AGPL-3.0-only
//! Flapping detection.
//!
//! Rapid state churn (a "flapping" check) is damped rather than alerted on per flip
//! (monitoring-conventions). This counts committed transitions in a sliding time window;
//! when the count reaches a threshold the check is considered flapping. Time is passed in
//! (Unix ms) so the detector is deterministic and unit-testable without a clock.

use std::collections::VecDeque;

/// Sliding-window transition counter.
#[derive(Debug, Clone)]
pub struct FlapDetector {
    window_ms: i64,
    threshold: usize,
    transitions: VecDeque<i64>,
}

impl FlapDetector {
    /// Detect flapping when `threshold`+ transitions occur within `window_ms`.
    #[must_use]
    pub fn new(window_ms: i64, threshold: usize) -> Self {
        Self {
            window_ms: window_ms.max(0),
            threshold,
            transitions: VecDeque::new(),
        }
    }

    /// Record a transition at `at_ms`, pruning anything older than the window.
    pub fn record(&mut self, at_ms: i64) {
        self.transitions.push_back(at_ms);
        self.prune(at_ms);
    }

    /// Whether the check is currently flapping as of `now_ms`.
    #[must_use]
    pub fn is_flapping(&self, now_ms: i64) -> bool {
        let cutoff = now_ms - self.window_ms;
        let recent = self.transitions.iter().filter(|&&t| t >= cutoff).count();
        recent >= self.threshold
    }

    fn prune(&mut self, now_ms: i64) {
        let cutoff = now_ms - self.window_ms;
        while let Some(&front) = self.transitions.front() {
            if front < cutoff {
                self.transitions.pop_front();
            } else {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rapid_transitions_within_window_flap() {
        let mut f = FlapDetector::new(10_000, 3); // 3 flips in 10s
        f.record(1_000);
        f.record(2_000);
        f.record(3_000);
        assert!(f.is_flapping(3_000));
    }

    #[test]
    fn sparse_transitions_do_not_flap() {
        let mut f = FlapDetector::new(10_000, 3);
        f.record(0);
        f.record(20_000);
        f.record(40_000);
        // Only the last one is inside the 10s window ending at 40s.
        assert!(!f.is_flapping(40_000));
    }

    #[test]
    fn old_transitions_age_out_of_the_window() {
        let mut f = FlapDetector::new(5_000, 2);
        f.record(0);
        f.record(1_000);
        assert!(f.is_flapping(1_000));
        // 10s later the early flips have aged out.
        assert!(!f.is_flapping(11_000));
    }
}
