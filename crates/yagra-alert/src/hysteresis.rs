//! Dwell-time hysteresis.
//!
//! A raw evaluated state must persist for `dwell` consecutive samples before the
//! committed state flips (Prometheus `for:`-style, ADR-015). This is what stops a metric
//! oscillating right at a threshold from emitting an alert per sample. Any sample that
//! returns to the committed state resets the candidate.

use yagra_common::NodeState;

/// Tracks the committed state of one check, applying dwell hysteresis to transitions.
#[derive(Debug, Clone)]
pub struct DwellTracker {
    committed: NodeState,
    /// The pending candidate state and how many consecutive samples it has held.
    candidate: Option<(NodeState, u32)>,
    /// Consecutive samples a new state must hold to commit (clamped to >= 1).
    dwell: u32,
}

impl DwellTracker {
    /// New tracker starting in `initial`, requiring `dwell` consecutive samples to flip.
    #[must_use]
    pub fn new(initial: NodeState, dwell: u32) -> Self {
        Self {
            committed: initial,
            candidate: None,
            dwell: dwell.max(1),
        }
    }

    /// The currently committed state.
    #[must_use]
    pub const fn committed(&self) -> NodeState {
        self.committed
    }

    /// Feed one raw sample. Returns `Some(new_state)` exactly when a transition commits.
    pub fn observe(&mut self, raw: NodeState) -> Option<NodeState> {
        if raw == self.committed {
            self.candidate = None;
            return None;
        }
        let count = match &mut self.candidate {
            Some((state, n)) if *state == raw => {
                *n += 1;
                *n
            }
            _ => {
                self.candidate = Some((raw, 1));
                1
            }
        };
        if count >= self.dwell {
            self.committed = raw;
            self.candidate = None;
            Some(raw)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dwell_one_transitions_immediately() {
        let mut t = DwellTracker::new(NodeState::Ok, 1);
        assert_eq!(t.observe(NodeState::Critical), Some(NodeState::Critical));
        assert_eq!(t.committed(), NodeState::Critical);
    }

    #[test]
    fn requires_consecutive_samples_to_commit() {
        let mut t = DwellTracker::new(NodeState::Ok, 3);
        assert_eq!(t.observe(NodeState::Warning), None); // 1
        assert_eq!(t.observe(NodeState::Warning), None); // 2
        assert_eq!(t.observe(NodeState::Warning), Some(NodeState::Warning)); // 3 → commit
        assert_eq!(t.committed(), NodeState::Warning);
    }

    #[test]
    fn oscillation_at_threshold_never_commits() {
        // Flip-flop OK/Warning each sample with dwell 3 → candidate keeps resetting.
        let mut t = DwellTracker::new(NodeState::Ok, 3);
        for _ in 0..10 {
            assert_eq!(t.observe(NodeState::Warning), None);
            assert_eq!(t.observe(NodeState::Ok), None);
        }
        assert_eq!(t.committed(), NodeState::Ok);
    }

    #[test]
    fn interrupted_streak_restarts_the_count() {
        let mut t = DwellTracker::new(NodeState::Ok, 3);
        assert_eq!(t.observe(NodeState::Warning), None); // 1
        assert_eq!(t.observe(NodeState::Critical), None); // different candidate → 1
        assert_eq!(t.observe(NodeState::Critical), None); // 2
        assert_eq!(t.observe(NodeState::Critical), Some(NodeState::Critical)); // 3
    }
}
