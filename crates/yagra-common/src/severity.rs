// SPDX-License-Identifier: AGPL-3.0-only
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
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    PartialOrd,
    Ord,
    Serialize,
    Deserialize,
    utoipa::ToSchema,
)]
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
    /// Every severity, low → high.
    pub const ALL: [Severity; 3] = [Severity::Info, Severity::Warning, Severity::Critical];

    /// Stable lowercase string for labels, API payloads, and logs.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Severity::Info => "info",
            Severity::Warning => "warning",
            Severity::Critical => "critical",
        }
    }

    /// The inverse of [`Self::as_str`]: an exact token, or `None`.
    ///
    /// Callers differ in what an unrecognised token means — a stored value degrades, an operator's
    /// input is rejected — so this answers only "is it one of ours" and leaves that decision where
    /// it belongs. What it removes is the *token list* being written out again at each of those
    /// sites, which is the part that drifts.
    #[must_use]
    pub fn from_token(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.as_str() == s)
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

    #[test]
    fn every_variant_round_trips_through_its_token_and_through_serde() {
        // `as_str` and serde's `rename_all` are two spellings of the same tokens; the DB columns
        // and the API payloads use one each, so they have to agree.
        for s in Severity::ALL {
            assert_eq!(Severity::from_token(s.as_str()), Some(s));
            assert_eq!(
                serde_json::to_string(&s).unwrap(),
                format!("\"{}\"", s.as_str())
            );
        }
        assert_eq!(Severity::from_token("nope"), None);
        // Exact match only — normalizing is the caller's decision, not this function's.
        assert_eq!(Severity::from_token("Critical"), None);
    }
}
