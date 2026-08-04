// SPDX-License-Identifier: AGPL-3.0-only
//! How the derived dependency graph is allowed to affect alerting (ADR-043 決定 5, migration 0067).
//!
//! The derivation can be wrong, and the way it is wrong matters asymmetrically: a **missing** edge
//! only fails to suppress an alert that would have been suppressed — noise. A **wrong** edge
//! suppresses a real outage — silence. Silence is the failure an NMS cannot recover from, so the
//! derived graph is never allowed to reach the alert engine on the strength of the code having
//! shipped. An operator turns it on, having seen what it would have done.
//!
//! Three states, deployment-wide because 決定 5 sets the unit of approval at the mode: approving
//! edges one at a time is the input cost this whole ADR exists to remove, and reintroducing it under
//! another name would leave the feature exactly as unused as `nodes.parent_id` was.

use serde::{Deserialize, Serialize};

/// Which dependency graph the alert engine uses.
//
// ⚠️ Published verbatim to API clients through the generated OpenAPI document — keep the `///`
// outward-facing and put reasoning in `//` like this.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TopologyMode {
    /// Suppression follows the hand-authored parent of each node. Nothing is derived.
    #[default]
    Manual,
    /// Suppression still follows the hand-authored parents, and the derived graph is computed
    /// alongside so the difference can be reviewed. Alerting is unchanged.
    Shadow,
    /// Suppression follows the derived graph.
    Derived,
}

impl TopologyMode {
    /// Every mode, for the coverage test below.
    //
    // `#[cfg(test)]` because that test is genuinely its only consumer: production parses one token
    // at a time and never enumerates. Stating that is better than an `#[allow(dead_code)]` that
    // reads as "someone will use this later" — and a new variant still has to be added here, or the
    // round-trip test stops covering it.
    #[cfg(test)]
    pub const ALL: [TopologyMode; 3] = [
        TopologyMode::Manual,
        TopologyMode::Shadow,
        TopologyMode::Derived,
    ];

    /// The stable token stored in `app_settings.topology_mode`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            TopologyMode::Manual => "manual",
            TopologyMode::Shadow => "shadow",
            TopologyMode::Derived => "derived",
        }
    }

    /// Parse a submitted token. `None` for anything else — the API edge rejects it, which is the
    /// only validation this column has (the migration deliberately carries no `CHECK`).
    #[must_use]
    pub fn from_token(s: &str) -> Option<Self> {
        match s {
            "manual" => Some(TopologyMode::Manual),
            "shadow" => Some(TopologyMode::Shadow),
            "derived" => Some(TopologyMode::Derived),
            _ => None,
        }
    }

    /// Parse a token read back from the database, falling back to [`TopologyMode::Manual`].
    ///
    /// Separate from [`Self::from_token`] because the two failures want opposite answers. A bad
    /// value *submitted* must be rejected so the operator learns. A bad value *stored* — a row
    /// written by a newer core, or hand-edited — must resolve to the mode that changes nothing,
    /// because the alternative is that an unreadable setting decides how the fleet alerts.
    #[must_use]
    pub fn from_stored(s: &str) -> Self {
        Self::from_token(s).unwrap_or(TopologyMode::Manual)
    }

    /// Whether the alert engine takes its dependency graph from the derivation.
    #[must_use]
    pub const fn uses_derived(self) -> bool {
        matches!(self, TopologyMode::Derived)
    }
}
// There is deliberately no `computes_shadow()`. The comparison is computed by the read-side
// endpoint on demand, not by the alert-config reloader, so nothing in the engine's path branches on
// "is this shadow" — which is what makes it structurally impossible for a shadow graph to suppress
// anything. A predicate here would be the first step back toward the engine knowing about it.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_mode_round_trips_through_its_token_and_through_serde() {
        for m in TopologyMode::ALL {
            assert_eq!(TopologyMode::from_token(m.as_str()), Some(m));
            assert_eq!(
                serde_json::to_string(&m).unwrap(),
                format!("\"{}\"", m.as_str()),
                "the DB token and the JSON tag come from two different mechanisms"
            );
        }
        assert_eq!(TopologyMode::from_token("auto"), None);
    }

    #[test]
    fn an_unreadable_stored_value_falls_back_to_the_mode_that_changes_nothing() {
        // A downgrade reads a token it does not know. Defaulting to `derived` there would hand the
        // fleet's suppression to a graph the running binary may not even produce the same way.
        assert_eq!(TopologyMode::from_stored(""), TopologyMode::Manual);
        assert_eq!(
            TopologyMode::from_stored("derived_v2"),
            TopologyMode::Manual
        );
        assert_eq!(TopologyMode::from_stored("derived"), TopologyMode::Derived);
        assert_eq!(TopologyMode::default(), TopologyMode::Manual);
    }

    #[test]
    fn only_derived_reaches_the_alert_engine() {
        // The structural guarantee this enum exists to express: shadow must be indistinguishable
        // from manual as far as suppression is concerned.
        assert!(TopologyMode::Derived.uses_derived());
        assert!(!TopologyMode::Shadow.uses_derived());
        assert!(!TopologyMode::Manual.uses_derived());
    }
}
