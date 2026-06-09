//! Alert severity.
//!
//! Severity is *ordered* — `Info < Warning < Critical` — so callers can compare,
//! take the max across grouped alerts, or filter "at least Warning". Kept distinct
//! from [`crate::state::NodeState`]: a state machine drives *what* a node is doing;
//! severity ranks *how bad* a resulting alert is.

use serde::{Deserialize, Serialize};
use std::fmt;

/// How serious an alert is. Variants are declared low → high so the derived
/// `Ord` ranks `Critical` above `Warning` above `Info`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// Informational — noteworthy but not a problem.
    Info,
    /// Warning — degraded or approaching a threshold.
    Warning,
    /// Critical — a hard failure or breached critical threshold.
    Critical,
}

impl Severity {
    /// Stable lowercase string for labels, API payloads, and logs.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Severity::Info => "info",
            Severity::Warning => "warning",
            Severity::Critical => "critical",
        }
    }
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn severity_orders_low_to_high() {
        assert!(Severity::Info < Severity::Warning);
        assert!(Severity::Warning < Severity::Critical);
        // `max` is how grouped alerts roll up to a single headline severity.
        assert_eq!(
            [Severity::Info, Severity::Critical, Severity::Warning]
                .into_iter()
                .max(),
            Some(Severity::Critical)
        );
    }

    #[test]
    fn severity_serializes_as_stable_lowercase() {
        assert_eq!(
            serde_json::to_string(&Severity::Critical).unwrap(),
            "\"critical\""
        );
        assert_eq!(Severity::Warning.to_string(), "warning");
    }
}
