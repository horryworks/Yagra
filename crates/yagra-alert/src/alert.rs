// SPDX-License-Identifier: AGPL-3.0-only
//! Alert identity: dedup and grouping keys.
//!
//! Dedup collapses duplicate alerts for the same `(node, check, severity)`; grouping rolls
//! related alerts up under their root-cause node so a parent outage shows as one incident
//! plus its children, not N pages (ADR-015). The alert *lifecycle* is then forwarded to an
//! external tool — Yagra owns the quality, not the escalation.

use serde::{Deserialize, Serialize};
use yagra_common::{CheckId, NodeId, NodeState, Severity};

/// Dedup key: two alerts with the same key are the same alert.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DedupKey {
    /// Affected node.
    pub node: NodeId,
    /// The check that fired.
    pub check: CheckId,
    /// Severity of the alert.
    pub severity: Severity,
}

/// Numeric breach detail for a threshold alert (absent for a liveness up/down alert).
/// Carried for the history log + notification payload — not part of alert identity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Breach {
    /// Observed sample value that committed the transition.
    pub value: f64,
    /// The bound crossed for the committed severity, if the rule defines one at that level.
    pub threshold: Option<f64>,
    /// Breach direction: `"above"` or `"below"`.
    pub direction: String,
}

/// A single alert produced by the engine.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Alert {
    /// Affected node.
    pub node: NodeId,
    /// The check that produced it.
    pub check: CheckId,
    /// Severity (derived from the committed state).
    pub severity: Severity,
    /// The committed state that triggered the alert.
    pub state: NodeState,
    /// When it fired (Unix ms, UTC).
    pub at_unix_ms: i64,
    /// Root-cause node, if this alert was attributed upstream by dependency analysis.
    pub root_cause: Option<NodeId>,
    /// Whether the underlying check is currently flapping.
    pub flapping: bool,
    /// Metric the check measured (e.g. `"icmp_rtt_ms"`; the liveness sentinel for up/down).
    /// Carried for the history log + notification payload so a human can read *what* fired —
    /// not part of alert identity (dedup/grouping ignore it).
    pub metric: String,
    /// Numeric breach detail for a threshold alert; `None` for a liveness alert.
    pub breach: Option<Breach>,
}

impl Alert {
    /// The dedup key for this alert.
    #[must_use]
    pub fn dedup_key(&self) -> DedupKey {
        DedupKey {
            node: self.node,
            check: self.check,
            severity: self.severity,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alert(node: NodeId, root: Option<NodeId>) -> Alert {
        Alert {
            node,
            check: CheckId::from(uuid_nil()),
            severity: Severity::Critical,
            state: NodeState::Critical,
            at_unix_ms: 0,
            root_cause: root,
            flapping: false,
            metric: "__liveness__".to_string(),
            breach: None,
        }
    }

    fn uuid_nil() -> uuid::Uuid {
        uuid::Uuid::nil()
    }

    #[test]
    fn dedup_key_ignores_timestamp_and_flapping() {
        let n = NodeId::new();
        let mut a = alert(n, None);
        let mut b = alert(n, None);
        a.at_unix_ms = 100;
        b.at_unix_ms = 999;
        b.flapping = true;
        assert_eq!(a.dedup_key(), b.dedup_key());
    }
}
