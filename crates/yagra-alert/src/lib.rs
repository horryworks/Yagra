//! Yagra-alert — alert engine.
//!
//! Turns a stream of raw evaluated states into a clean alert signal: dwell-time
//! hysteresis ([`hysteresis`]), flapping detection ([`flapping`]), and dedup/grouping
//! ([`alert`]). Dependency suppression (parent-down roll-up) is computed against the
//! topology and recorded as an alert's `root_cause`. Escalation/on-call is
//! external — Yagra produces the quality signal and forwards its lifecycle (ADR-015).

pub mod alert;
pub mod flapping;
pub mod hysteresis;
pub mod notify;

pub use alert::{Alert, Breach, DedupKey, GroupKey};
pub use flapping::FlapDetector;
pub use hysteresis::DwellTracker;
pub use notify::{
    DispatchOutcome, Dispatcher, Notification, NotifyChannel, NotifyError, RetryPolicy,
};

use yagra_common::{CheckId, NodeId, NodeState};

/// A committed state transition for one check, with whether the check is flapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Transition {
    /// The newly committed state.
    pub state: NodeState,
    /// Whether the check is flapping as of the transition.
    pub flapping: bool,
}

impl Transition {
    /// Build an [`Alert`] for this transition if the committed state warrants one.
    ///
    /// `Ok`/`Unknown`/`Maintenance` produce no alert (they carry no severity). A problem
    /// state produces an alert tagged with the supplied `root_cause` (from dependency
    /// analysis) and flapping flag.
    #[must_use]
    pub fn to_alert(
        self,
        node: NodeId,
        check: CheckId,
        at_unix_ms: i64,
        root_cause: Option<NodeId>,
    ) -> Option<Alert> {
        self.state.severity().map(|severity| Alert {
            node,
            check,
            severity,
            state: self.state,
            at_unix_ms,
            root_cause,
            flapping: self.flapping,
            // Descriptive context is filled in by the engine (it knows the metric/breach).
            metric: String::new(),
            breach: None,
        })
    }
}

/// The committed state of one check: dwell hysteresis plus flapping detection.
///
/// Feed raw per-sample states (e.g. from threshold evaluation) and timestamps; it emits a
/// [`Transition`] only when the committed state actually changes, tagged with the current
/// flapping status.
#[derive(Debug, Clone)]
pub struct CheckState {
    dwell: DwellTracker,
    flap: FlapDetector,
}

impl CheckState {
    /// New check state starting in `initial`, requiring `dwell` consecutive samples to
    /// flip, and flagged as flapping at `flap_threshold`+ transitions per `flap_window_ms`.
    #[must_use]
    pub fn new(initial: NodeState, dwell: u32, flap_window_ms: i64, flap_threshold: usize) -> Self {
        Self {
            dwell: DwellTracker::new(initial, dwell),
            flap: FlapDetector::new(flap_window_ms, flap_threshold),
        }
    }

    /// The currently committed state.
    #[must_use]
    pub const fn committed(&self) -> NodeState {
        self.dwell.committed()
    }

    /// Feed one raw sample observed at `now_ms`. Returns a [`Transition`] iff the
    /// committed state changed.
    pub fn observe(&mut self, raw: NodeState, now_ms: i64) -> Option<Transition> {
        let new_state = self.dwell.observe(raw)?;
        self.flap.record(now_ms);
        Some(Transition {
            state: new_state,
            flapping: self.flap.is_flapping(now_ms),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use yagra_common::Severity;

    #[test]
    fn no_transition_until_dwell_satisfied() {
        let mut cs = CheckState::new(NodeState::Ok, 2, 60_000, 5);
        assert_eq!(cs.observe(NodeState::Critical, 0), None); // 1st
        let t = cs.observe(NodeState::Critical, 1_000).unwrap(); // 2nd → commit
        assert_eq!(t.state, NodeState::Critical);
        assert!(!t.flapping);
    }

    #[test]
    fn transition_to_problem_yields_alert_with_severity() {
        let mut cs = CheckState::new(NodeState::Ok, 1, 60_000, 5);
        let node = NodeId::new();
        let check = CheckId::new();
        let t = cs.observe(NodeState::Unreachable, 0).unwrap();
        let alert = t.to_alert(node, check, 0, Some(NodeId::new())).unwrap();
        assert_eq!(alert.severity, Severity::Critical);
        assert!(alert.root_cause.is_some());
    }

    #[test]
    fn recovery_to_ok_produces_no_alert() {
        let mut cs = CheckState::new(NodeState::Critical, 1, 60_000, 5);
        let t = cs.observe(NodeState::Ok, 0).unwrap();
        assert_eq!(t.state, NodeState::Ok);
        assert_eq!(t.to_alert(NodeId::new(), CheckId::new(), 0, None), None);
    }

    #[test]
    fn churn_marks_later_transitions_as_flapping() {
        // dwell 1, flapping at 3 transitions within 10s.
        let mut cs = CheckState::new(NodeState::Ok, 1, 10_000, 3);
        let a = cs.observe(NodeState::Critical, 1_000).unwrap();
        let b = cs.observe(NodeState::Ok, 2_000).unwrap();
        let c = cs.observe(NodeState::Critical, 3_000).unwrap();
        assert!(!a.flapping); // 1 transition
        assert!(!b.flapping); // 2 transitions
        assert!(c.flapping); // 3 transitions in window → flapping
    }
}
