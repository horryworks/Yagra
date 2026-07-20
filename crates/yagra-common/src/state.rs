// SPDX-License-Identifier: AGPL-3.0-only
//! The node/check state machine.
//!
//! States are explicit and exhaustive — there is no implicit or ambiguous state
//! (monitoring-conventions). Transitions are deliberate and gated by hysteresis in
//! the alert engine; this type only names the states and their meaning.

use crate::severity::Severity;
use serde::{Deserialize, Serialize};
use std::fmt;

/// The current state of a monitored node or check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeState {
    /// Healthy / within thresholds.
    Ok,
    /// Degraded or a warning threshold is crossed.
    Warning,
    /// Hard failure or a critical threshold is crossed.
    Critical,
    /// State could not be determined (e.g. a poll timed out without a clear down).
    Unknown,
    /// The node did not respond and is considered down.
    Unreachable,
    /// In a planned maintenance window — alerting is suppressed.
    Maintenance,
}

impl NodeState {
    /// Stable lowercase string for labels, API payloads, and logs.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            NodeState::Ok => "ok",
            NodeState::Warning => "warning",
            NodeState::Critical => "critical",
            NodeState::Unknown => "unknown",
            NodeState::Unreachable => "unreachable",
            NodeState::Maintenance => "maintenance",
        }
    }

    /// Whether this state should be treated as an active problem worth alerting on.
    ///
    /// `Maintenance` is never a problem (alerts are suppressed there) and `Unknown`
    /// is deliberately *not* a problem on its own — it signals "investigate", not
    /// "page someone".
    #[must_use]
    pub const fn is_problem(&self) -> bool {
        matches!(
            self,
            NodeState::Warning | NodeState::Critical | NodeState::Unreachable
        )
    }

    /// The alert severity implied by this state, if any.
    ///
    /// `Unreachable` maps to `Critical` (the node is down). States that do not
    /// warrant an alert return `None`.
    #[must_use]
    pub const fn severity(&self) -> Option<Severity> {
        match self {
            NodeState::Warning => Some(Severity::Warning),
            NodeState::Critical | NodeState::Unreachable => Some(Severity::Critical),
            NodeState::Ok | NodeState::Unknown | NodeState::Maintenance => None,
        }
    }
}

impl fmt::Display for NodeState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn problem_states_are_exactly_warning_critical_unreachable() {
        assert!(NodeState::Warning.is_problem());
        assert!(NodeState::Critical.is_problem());
        assert!(NodeState::Unreachable.is_problem());

        assert!(!NodeState::Ok.is_problem());
        assert!(!NodeState::Unknown.is_problem());
        assert!(!NodeState::Maintenance.is_problem());
    }

    #[test]
    fn severity_mapping() {
        assert_eq!(NodeState::Unreachable.severity(), Some(Severity::Critical));
        assert_eq!(NodeState::Warning.severity(), Some(Severity::Warning));
        assert_eq!(NodeState::Ok.severity(), None);
        assert_eq!(NodeState::Maintenance.severity(), None);
    }

    #[test]
    fn state_serializes_as_stable_lowercase() {
        assert_eq!(
            serde_json::to_string(&NodeState::Unreachable).unwrap(),
            "\"unreachable\""
        );
    }
}
