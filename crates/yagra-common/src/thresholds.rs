// SPDX-License-Identifier: AGPL-3.0-only
//! Thresholds and their inheritance resolution.
//!
//! Thresholds resolve by **inheritance with override** (ADR-013, monitoring-conventions):
//! most-specific scope wins — `Node` > `Group` > `Profile` > system default. When a
//! node matches several scopes at the *same* level (e.g. two groups), the tie-break is
//! **most-restrictive value wins**. Resolution lives *only here* so precedence logic is
//! never scattered. Hysteresis (dwell) is carried on the rule but applied over time by
//! the alert engine, not here.

use crate::state::NodeState;
use serde::{Deserialize, Serialize};
use std::fmt;

/// Which way a metric breaches its bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    /// Breaches when the value rises to/above the bound (e.g. CPU %, temperature).
    Above,
    /// Breaches when the value falls to/below the bound (e.g. free memory, battery).
    Below,
}

impl Direction {
    /// Both directions.
    pub const ALL: [Direction; 2] = [Direction::Above, Direction::Below];

    /// Stable lowercase string for API payloads, DB columns, and logs.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Direction::Above => "above",
            Direction::Below => "below",
        }
    }

    /// The inverse of [`Self::as_str`]: an exact token, or `None`. See
    /// [`crate::severity::Severity::from_token`] for why this does not decide what a miss means.
    #[must_use]
    pub fn from_token(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.as_str() == s)
    }

    /// Of two bounds, the more restrictive one (the one that trips earlier).
    ///
    /// For `Above`, the lower bound is stricter; for `Below`, the higher bound is.
    #[must_use]
    pub(crate) fn more_restrictive(self, a: f64, b: f64) -> f64 {
        match self {
            Direction::Above => a.min(b),
            Direction::Below => a.max(b),
        }
    }
}

/// Matches `Severity` and `NodeState`. Direction was the one shared enum without it, which is part
/// of why its carriers reached for a `String` field instead of the type.
impl fmt::Display for Direction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The scope a threshold is defined at, ordered least → most specific so the derived
/// `Ord` makes `Node` win over `Group` win over `Profile`.
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
pub enum ScopeLevel {
    /// Defined on a device-class/profile (broadest).
    Profile,
    /// Defined on a group (site/region/role/…).
    Group,
    /// Defined directly on the node (narrowest).
    Node,
}

impl ScopeLevel {
    /// Every level, broadest first — the order the threshold list is sorted in and the order a
    /// picker should offer them.
    pub const ALL: [ScopeLevel; 3] = [ScopeLevel::Profile, ScopeLevel::Group, ScopeLevel::Node];

    /// Stable lowercase string for API payloads, DB columns, and logs.
    ///
    /// The `thresholds.scope_level` column holds exactly these three, written verbatim from the
    /// create request and read back by `ThresholdStore::parse_level`, so anything that binds a
    /// level into SQL must go through here rather than formatting the enum.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            ScopeLevel::Profile => "profile",
            ScopeLevel::Group => "group",
            ScopeLevel::Node => "node",
        }
    }
}

/// A threshold rule for a single metric.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ThresholdRule {
    /// Stable metric name this rule applies to (e.g. `cpu_util`).
    pub metric: String,
    /// Which direction trips the threshold.
    pub direction: Direction,
    /// Value at/over which the node is `Warning`. `None` = no warning level.
    pub warning: Option<f64>,
    /// Value at/over which the node is `Critical`. `None` = no critical level.
    pub critical: Option<f64>,
    /// Hysteresis: consecutive samples the breach must hold before transitioning,
    /// to damp oscillation at the threshold. `0`/`1` = transition immediately.
    pub dwell_samples: u32,
}

/// A [`ThresholdRule`] tagged with the scope it came from, for resolution.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScopedThreshold {
    /// Scope this rule was defined at.
    pub level: ScopeLevel,
    /// The rule itself.
    pub rule: ThresholdRule,
}

impl ScopedThreshold {
    /// Convenience constructor.
    #[must_use]
    pub fn new(level: ScopeLevel, rule: ThresholdRule) -> Self {
        Self { level, rule }
    }
}

/// A resolved, effective threshold for one metric — what the alert engine evaluates.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EffectiveThreshold {
    /// Metric this applies to.
    pub metric: String,
    /// Breach direction.
    pub direction: Direction,
    /// Effective warning bound, if any.
    pub warning: Option<f64>,
    /// Effective critical bound, if any.
    pub critical: Option<f64>,
    /// Effective dwell (hysteresis) in consecutive samples.
    pub dwell_samples: u32,
}

impl EffectiveThreshold {
    /// Classify a single value, ignoring hysteresis (the alert engine applies dwell
    /// across samples). Returns `Critical`, `Warning`, or `Ok`.
    #[must_use]
    pub fn evaluate(&self, value: f64) -> NodeState {
        let breaches = |bound: f64| match self.direction {
            Direction::Above => value >= bound,
            Direction::Below => value <= bound,
        };
        if let Some(c) = self.critical {
            if breaches(c) {
                return NodeState::Critical;
            }
        }
        if let Some(w) = self.warning {
            if breaches(w) {
                return NodeState::Warning;
            }
        }
        NodeState::Ok
    }
}

/// Resolve a set of scoped rules **for a single metric** into one effective threshold.
///
/// Precedence: only the most-specific [`ScopeLevel`] present is considered. Among rules
/// at that level, bounds combine **most-restrictively** per field (the `Group`-vs-`Group`
/// tie-break of ADR-013); dwell takes the longest (most damping). Returns `None` if the
/// input is empty.
///
/// Mixing rules for different metrics is a caller error — pass one metric's rules.
#[must_use]
pub fn resolve_effective(rules: &[ScopedThreshold]) -> Option<EffectiveThreshold> {
    let winning_level = rules.iter().map(|r| r.level).max()?;
    let mut winners = rules.iter().filter(|r| r.level == winning_level).peekable();

    // Direction is assumed consistent per metric; take it from the first winner.
    let direction = winners.peek().expect("max() found a level").rule.direction;

    let mut warning: Option<f64> = None;
    let mut critical: Option<f64> = None;
    let mut dwell: u32 = 0;
    let mut metric = String::new();

    for ScopedThreshold { rule, .. } in winners {
        metric = rule.metric.clone();
        warning = restrictive(warning, rule.warning, direction);
        critical = restrictive(critical, rule.critical, direction);
        dwell = dwell.max(rule.dwell_samples);
    }

    Some(EffectiveThreshold {
        metric,
        direction,
        warning,
        critical,
        dwell_samples: dwell,
    })
}

/// Combine two optional bounds, keeping the more restrictive. A present bound always
/// beats `None` (a missing bound imposes no limit, so it is the least restrictive).
fn restrictive(a: Option<f64>, b: Option<f64>, dir: Direction) -> Option<f64> {
    match (a, b) {
        (Some(x), Some(y)) => Some(dir.more_restrictive(x, y)),
        (Some(x), None) | (None, Some(x)) => Some(x),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_scope_level_agrees_with_its_serde_tag_and_covers_the_enum() {
        // The same value is both a DB column (`as_str`) and a JSON field (`#[serde(rename_all)]`),
        // produced by two different mechanisms with nothing making them agree — so a disagreement
        // means the API writes rules the store reads back as a *different scope*, which is a
        // node-level override silently behaving as a fleet-wide profile rule (testing.md).
        for level in ScopeLevel::ALL {
            let json = serde_json::to_string(&level).expect("serializable");
            assert_eq!(json, format!("\"{}\"", level.as_str()), "{level:?}");
            let back: ScopeLevel =
                serde_json::from_str(&json).expect("round-trips through its own tag");
            assert_eq!(back, level);
        }
        // ALL is the whole enum, not a list someone forgot to extend when a level was added.
        assert_eq!(ScopeLevel::ALL.len(), 3);
        assert_eq!(
            ScopeLevel::ALL.map(ScopeLevel::as_str),
            ["profile", "group", "node"],
            "ALL is ordered broadest-first — the threshold list and its filter both rely on it"
        );
    }

    fn rule(level: ScopeLevel, warning: f64, critical: f64) -> ScopedThreshold {
        ScopedThreshold::new(
            level,
            ThresholdRule {
                metric: "cpu_util".into(),
                direction: Direction::Above,
                warning: Some(warning),
                critical: Some(critical),
                dwell_samples: 3,
            },
        )
    }

    #[test]
    fn most_specific_level_wins() {
        // Profile 80/95, Group 70/90, Node 60/85 → Node wins outright.
        let rules = [
            rule(ScopeLevel::Profile, 80.0, 95.0),
            rule(ScopeLevel::Group, 70.0, 90.0),
            rule(ScopeLevel::Node, 60.0, 85.0),
        ];
        let eff = resolve_effective(&rules).unwrap();
        assert_eq!(eff.warning, Some(60.0));
        assert_eq!(eff.critical, Some(85.0));
    }

    #[test]
    fn same_level_tiebreak_takes_most_restrictive_above() {
        // Two groups; "above" → smaller bound is stricter.
        let rules = [
            rule(ScopeLevel::Group, 70.0, 90.0),
            rule(ScopeLevel::Group, 65.0, 92.0),
        ];
        let eff = resolve_effective(&rules).unwrap();
        assert_eq!(eff.warning, Some(65.0)); // stricter warning
        assert_eq!(eff.critical, Some(90.0)); // stricter critical
    }

    #[test]
    fn same_level_tiebreak_takes_most_restrictive_below() {
        // "below" → larger bound is stricter (trips earlier as value falls).
        let mk = |w: f64, c: f64| {
            ScopedThreshold::new(
                ScopeLevel::Group,
                ThresholdRule {
                    metric: "free_mem".into(),
                    direction: Direction::Below,
                    warning: Some(w),
                    critical: Some(c),
                    dwell_samples: 1,
                },
            )
        };
        let eff = resolve_effective(&[mk(20.0, 10.0), mk(25.0, 8.0)]).unwrap();
        assert_eq!(eff.warning, Some(25.0));
        assert_eq!(eff.critical, Some(10.0));
    }

    #[test]
    fn present_bound_beats_missing() {
        let with = ScopedThreshold::new(
            ScopeLevel::Node,
            ThresholdRule {
                metric: "cpu_util".into(),
                direction: Direction::Above,
                warning: Some(70.0),
                critical: None,
                dwell_samples: 2,
            },
        );
        let without = ScopedThreshold::new(
            ScopeLevel::Node,
            ThresholdRule {
                metric: "cpu_util".into(),
                direction: Direction::Above,
                warning: None,
                critical: Some(90.0),
                dwell_samples: 5,
            },
        );
        let eff = resolve_effective(&[with, without]).unwrap();
        assert_eq!(eff.warning, Some(70.0));
        assert_eq!(eff.critical, Some(90.0));
        assert_eq!(eff.dwell_samples, 5); // longest dwell wins
    }

    #[test]
    fn evaluate_classifies_above() {
        let eff = EffectiveThreshold {
            metric: "cpu_util".into(),
            direction: Direction::Above,
            warning: Some(70.0),
            critical: Some(90.0),
            dwell_samples: 3,
        };
        assert_eq!(eff.evaluate(50.0), NodeState::Ok);
        assert_eq!(eff.evaluate(70.0), NodeState::Warning);
        assert_eq!(eff.evaluate(95.0), NodeState::Critical);
    }

    #[test]
    fn evaluate_classifies_below() {
        let eff = EffectiveThreshold {
            metric: "free_mem".into(),
            direction: Direction::Below,
            warning: Some(25.0),
            critical: Some(10.0),
            dwell_samples: 1,
        };
        assert_eq!(eff.evaluate(40.0), NodeState::Ok);
        assert_eq!(eff.evaluate(20.0), NodeState::Warning);
        assert_eq!(eff.evaluate(5.0), NodeState::Critical);
    }

    #[test]
    fn boolean_up_gauge_below_half_only_trips_when_down() {
        // `http_up` is a 0/1 gauge. Because "below" is inclusive (`value <= bound`), the default
        // bound must sit between the states (0.5) — a bound of 1.0 would mis-fire on the healthy
        // value 1. Regression guard for the "URL monitor permanently Critical" bug.
        let eff = EffectiveThreshold {
            metric: "http_up".into(),
            direction: Direction::Below,
            warning: None,
            critical: Some(0.5),
            dwell_samples: 2,
        };
        assert_eq!(eff.evaluate(1.0), NodeState::Ok); // up + status OK
        assert_eq!(eff.evaluate(0.0), NodeState::Critical); // down / wrong status
    }

    #[test]
    fn empty_resolves_to_none() {
        assert_eq!(resolve_effective(&[]), None);
    }
}
