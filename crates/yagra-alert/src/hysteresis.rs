// SPDX-License-Identifier: AGPL-3.0-only
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
    /// Whether `committed` is something this tracker has actually *observed*, rather than the value
    /// it was seeded with at construction.
    ///
    /// 🚨 This distinction is the whole of ADR-097. A tracker is created on a check's **first**
    /// sample, and until that sample has been weighed it has no opinion — but it must hold *some*
    /// state, and the seed is [`NodeState::Ok`] because a transition away from it is what makes an
    /// alert fire at all. Reporting the seed as though it were an observation is how a core restart
    /// made the whole fleet read `ok`: every check was rebuilt from the seed, so a dead device read
    /// `ok` until it had failed `dwell` times. Measured on the test server — five minutes after a
    /// restart, 15 of 22 stopped devices were being reported healthy.
    confirmed: bool,
    /// The pending candidate state and how many consecutive samples it has held.
    candidate: Option<(NodeState, u32)>,
    /// Consecutive samples a new state must hold to commit (clamped to >= 1).
    dwell: u32,
}

impl DwellTracker {
    /// New tracker **seeded** with `initial`, requiring `dwell` consecutive samples to flip.
    ///
    /// The seed is not an observation — see [`Self::observed`]. Use [`Self::restored`] when the
    /// state came from somewhere that actually knew it.
    #[must_use]
    pub fn new(initial: NodeState, dwell: u32) -> Self {
        Self {
            committed: initial,
            confirmed: false,
            candidate: None,
            dwell: dwell.max(1),
        }
    }

    /// New tracker holding a state **read back from persisted history**, not guessed.
    ///
    /// Confirmed from the outset, because it is what this check had settled on before the process
    /// restarted: the next agreeing sample must produce no transition, and the next disagreeing run
    /// must dwell and then commit — which is exactly what a tracker that never lost its state would
    /// have done (ADR-097 decision 2).
    #[must_use]
    pub fn restored(state: NodeState, dwell: u32) -> Self {
        Self {
            committed: state,
            confirmed: true,
            candidate: None,
            dwell: dwell.max(1),
        }
    }

    /// The currently committed state, seed or not.
    ///
    /// ⚠️ Anything that *displays* a state wants [`Self::observed`]. This one is for the machine's
    /// own bookkeeping and for tests that drive it directly.
    #[must_use]
    pub const fn committed(&self) -> NodeState {
        self.committed
    }

    /// The committed state **iff this tracker has observed it** — `None` while it still holds the
    /// seed it was constructed with.
    ///
    /// Two things confirm a seed and only two: a transition commits (the state was measured), or a
    /// sample arrives that agrees with it (the seed happened to be right). The second is
    /// deliberately **one** sample rather than `dwell` of them — dwell exists to stop a state
    /// flipping back and forth, not to delay agreeing with reality. A device that answers its first
    /// poll is up, and making it wait would replace the fleet-wide `ok` window after a restart with
    /// an equally wrong `unknown` one.
    #[must_use]
    pub const fn observed(&self) -> Option<NodeState> {
        if self.confirmed {
            Some(self.committed)
        } else {
            None
        }
    }

    /// Change how many consecutive samples a flip needs, keeping the committed state and any
    /// candidate run in place.
    ///
    /// A tracker is created once per check and lives for the process, so without this an edited
    /// rule would need a core restart to take effect — which makes "you can tune the dwell" a
    /// claim the UI states and the engine does not honour. Lowering it commits on the next
    /// sample if the candidate has already run long enough (`observe` compares `>=`); raising it
    /// makes the current run wait longer. The candidate is deliberately **not** reset: an
    /// operator widening the window while a device is already failing should not restart the
    /// count and delay the alert twice over.
    pub fn set_dwell(&mut self, dwell: u32) {
        self.dwell = dwell.max(1);
    }

    /// Feed one raw sample. Returns `Some(new_state)` exactly when a transition commits.
    ///
    /// Both exits also confirm the state ([`Self::observed`]): agreeing with it proves the seed was
    /// right, and committing a transition replaces the seed with a measured value.
    pub fn observe(&mut self, raw: NodeState) -> Option<NodeState> {
        if raw == self.committed {
            self.confirmed = true;
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
            self.confirmed = true;
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

    /// The accepting half of ADR-097, and the one that decides the shape: a device that answers its
    /// first poll agrees with the seed, so it is confirmed **immediately**. Without this the fix
    /// would trade a fleet-wide `ok` window after a restart for an equally wrong `unknown` one.
    #[test]
    fn one_agreeing_sample_confirms_the_seed() {
        let mut t = DwellTracker::new(NodeState::Ok, 3);
        assert_eq!(t.observed(), None, "a fresh tracker has observed nothing");
        assert_eq!(t.observe(NodeState::Ok), None, "no transition");
        assert_eq!(t.observed(), Some(NodeState::Ok));
    }

    /// The rejecting half: while a check is dwelling on a state that disagrees with its seed, it has
    /// not observed anything, and `committed()` is still the seed. Reporting that seed is the defect
    /// — a stopped device read `ok` for three polls.
    #[test]
    fn a_disagreeing_run_leaves_the_seed_unobserved() {
        let mut t = DwellTracker::new(NodeState::Ok, 3);
        for _ in 0..2 {
            assert_eq!(t.observe(NodeState::Unreachable), None);
            assert_eq!(
                t.committed(),
                NodeState::Ok,
                "the seed is still what is held"
            );
            assert_eq!(t.observed(), None, "but it was never observed");
        }
        assert_eq!(
            t.observe(NodeState::Unreachable),
            Some(NodeState::Unreachable)
        );
        assert_eq!(t.observed(), Some(NodeState::Unreachable));
    }

    /// A restored tracker is confirmed from the outset and behaves exactly like one that never lost
    /// its state: the agreeing sample is silent, and recovery still costs a full dwell.
    #[test]
    fn a_restored_state_is_observed_and_still_dwells_on_recovery() {
        let mut t = DwellTracker::restored(NodeState::Unreachable, 3);
        assert_eq!(t.observed(), Some(NodeState::Unreachable));
        assert_eq!(
            t.observe(NodeState::Unreachable),
            None,
            "still down ⇒ nothing happens"
        );
        assert_eq!(t.observe(NodeState::Ok), None); // 1
        assert_eq!(t.observe(NodeState::Ok), None); // 2
        assert_eq!(
            t.observe(NodeState::Ok),
            Some(NodeState::Ok),
            "3 → recovery"
        );
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
