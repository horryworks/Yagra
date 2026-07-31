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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, utoipa::ToSchema)]
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
    /// Every state, in **display order** — best first, then problems by severity, then the two
    /// that are neither.
    ///
    /// This is the enumeration for anything that must present all six: a per-state tally whose keys
    /// are always present, a pivoted timeline's series set, a report's status table. Those existed
    /// as three separate hand-written arrays in `yagra-core`, in two different orders, so a
    /// seventh state would have appeared in some tallies and silently not others.
    ///
    /// Note this is display order, **not** severity order — `is_problem`/`severity` are the
    /// predicates for ranking, and the WebUI keeps its own orders in `lib/nodeState.ts`.
    pub const ALL: [NodeState; 6] = [
        NodeState::Ok,
        NodeState::Warning,
        NodeState::Critical,
        NodeState::Unreachable,
        NodeState::Unknown,
        NodeState::Maintenance,
    ];

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

    /// The inverse of [`Self::as_str`]: an exact token, or `None`. See
    /// [`crate::severity::Severity::from_token`] for why this does not decide what a miss means.
    #[must_use]
    pub fn from_token(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.as_str() == s)
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
    fn all_lists_every_state_exactly_once() {
        // `as_str` is an exhaustive match, so a new variant is forced to get a token — but nothing
        // forces it into `ALL`, and a state missing from `ALL` drops out of every full tally
        // (fleet summary, state-history series, the report status table) without an error. Pinning
        // the tokens against `ALL` fails either way round: a variant added to `ALL` without this
        // list fails on the length, and one added here without `ALL` fails on the lookup.
        let expected = [
            "ok",
            "warning",
            "critical",
            "unreachable",
            "unknown",
            "maintenance",
        ];
        assert_eq!(NodeState::ALL.len(), expected.len());
        let tokens: Vec<&str> = NodeState::ALL.iter().map(NodeState::as_str).collect();
        assert_eq!(tokens, expected, "ALL is the display order");
        // No duplicates: a copy-paste slip in `ALL` would otherwise double-count one state.
        let mut sorted = tokens.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), NodeState::ALL.len());
    }

    #[test]
    fn state_serializes_as_stable_lowercase() {
        assert_eq!(
            serde_json::to_string(&NodeState::Unreachable).unwrap(),
            "\"unreachable\""
        );
    }
}
