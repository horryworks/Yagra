// SPDX-License-Identifier: AGPL-3.0-only
//! Thresholds and their inheritance resolution.
//!
//! Thresholds resolve by **inheritance with override** (ADR-013, monitoring-conventions):
//! most-specific scope wins — `Node` > `FolderGroup` > `Group` > `Profile` > `Global`. `Global` is
//! the "system default" tier ADR-013 named from the start and ADR-075 finally implemented. When a
//! node matches several scopes at the *same* level (e.g. two tag groups), the tie-break is
//! **most-restrictive value wins**. Resolution lives *only here* so precedence logic is
//! never scattered. Hysteresis (dwell) is carried on the rule but applied over time by
//! the alert engine, not here.
//!
//! ⚠️ One half is **not** here: a `FolderGroup` rule matches the node's own folder *and every
//! folder above it*, so several can arrive at that one level. Which of them survives is decided by
//! depth — nearest wins — and depth is a fact about the node, not about the rule, so
//! `alerts.rs::resolve` filters before calling [`resolve_effective`] (ADR-075 増分 3).

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

/// The bounds one rule names, and the only place a breach is decided (ADR-081).
///
/// **Two rays, not one.** A value breaches when it falls to/below a `*_below` bound **or** rises
/// to/above an `*_above` bound. That is "outside the band" — the shape an optical receive level
/// needs, where both a dark link and an overdriven one are faults, and which the single
/// `Direction` this replaces could only ever express half of.
///
/// ⚠️ **The inside of the band is deliberately not expressible.** A union of two rays is the
/// complement of one interval, so "abnormal between 3 and 4" cannot be written. A metric that
/// wants that is usually an enum encoded as a number (`ciscoEnvMonState`), which wants set
/// membership rather than ordered comparison — a different feature, and ADR-081 says so rather
/// than half-solving it here.
///
/// Carries no `Serialize`/`ToSchema` on purpose: it is the shared *logic* over the four bounds,
/// while the wire shape lives on [`ThresholdRule`] and [`EffectiveThreshold`], which hold the four
/// as flat fields so the JSON the API already publishes stays flat.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ThresholdBounds {
    /// Value at/below which the state is `Warning`.
    pub warning_below: Option<f64>,
    /// Value at/below which the state is `Critical`.
    pub critical_below: Option<f64>,
    /// Value at/above which the state is `Warning`.
    pub warning_above: Option<f64>,
    /// Value at/above which the state is `Critical`.
    pub critical_above: Option<f64>,
}

impl ThresholdBounds {
    /// Bounds that trip on the way up only — the shape every pre-ADR-081 `above` rule had.
    #[must_use]
    pub const fn above(warning: Option<f64>, critical: Option<f64>) -> Self {
        Self {
            warning_below: None,
            critical_below: None,
            warning_above: warning,
            critical_above: critical,
        }
    }

    /// Bounds that trip on the way down only — the shape every pre-ADR-081 `below` rule had.
    #[must_use]
    pub const fn below(warning: Option<f64>, critical: Option<f64>) -> Self {
        Self {
            warning_below: warning,
            critical_below: critical,
            warning_above: None,
            critical_above: None,
        }
    }

    /// Read the pre-ADR-081 triple back into the four.
    ///
    /// The **only** conversion from the old shape, and it is reached from exactly two places: the
    /// database read and the config-bundle import. Keeping it to one function is what stops the
    /// old triple from becoming a second, quietly diverging statement of what a rule means.
    #[must_use]
    pub const fn from_legacy(
        direction: Direction,
        warning: Option<f64>,
        critical: Option<f64>,
    ) -> Self {
        match direction {
            Direction::Above => Self::above(warning, critical),
            Direction::Below => Self::below(warning, critical),
        }
    }

    /// Whether any bound trips on the way down.
    #[must_use]
    pub const fn has_below(&self) -> bool {
        self.warning_below.is_some() || self.critical_below.is_some()
    }

    /// Whether any bound trips on the way up.
    #[must_use]
    pub const fn has_above(&self) -> bool {
        self.warning_above.is_some() || self.critical_above.is_some()
    }

    /// A rule with no bound at all cannot fire. Worth naming: such a rule is stored, listed and
    /// silently inert, which is the failure ADR-081 exists to stop rather than to reproduce.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        !self.has_below() && !self.has_above()
    }

    /// The **primary side**, for the API field, the legacy database column and the fold order.
    ///
    /// ⚠️ When both sides are set this answers `Above`, and that choice is arbitrary. It decides
    /// what an N-1 core enforces after a rollback — half of a range rule rather than none of it.
    /// Acceptable because a range rule can only have been written on a core that supports ranges,
    /// so seeing half of it is a rollback window rather than a steady state; a rule naming one
    /// side keeps that side exactly, which is the case a rollback actually meets.
    #[must_use]
    pub const fn direction(&self) -> Direction {
        if self.has_above() || !self.has_below() {
            Direction::Above
        } else {
            Direction::Below
        }
    }

    /// The primary side's warning bound — what the legacy `warning` column and API field carry.
    #[must_use]
    pub const fn warning(&self) -> Option<f64> {
        match self.direction() {
            Direction::Above => self.warning_above,
            Direction::Below => self.warning_below,
        }
    }

    /// The primary side's critical bound — what the legacy `critical` column and API field carry.
    #[must_use]
    pub const fn critical(&self) -> Option<f64> {
        match self.direction() {
            Direction::Above => self.critical_above,
            Direction::Below => self.critical_below,
        }
    }

    /// The smallest bound named on any side, for the interface evaluator's speed floor.
    #[must_use]
    pub fn lowest_bound(&self) -> Option<f64> {
        [
            self.warning_below,
            self.critical_below,
            self.warning_above,
            self.critical_above,
        ]
        .into_iter()
        .flatten()
        .fold(None, |acc: Option<f64>, b| {
            Some(acc.map_or(b, |a: f64| a.min(b)))
        })
    }

    /// Classify one value: how bad it is, **and which side said so**.
    ///
    /// The one walk behind [`Self::evaluate`] and [`Self::breaching_side`]. Splitting it was the
    /// original mistake: the first version answered only "how bad", so the alert had to name a
    /// bound from somewhere else and named the *primary* side's — which for a band is routinely
    /// the side the value did not cross. Observed on the live deployment 2026-08-21: a value of
    /// 0.909 that tripped `critical_below: 1.0` was published as
    /// `threshold: 5000.0, direction: above`. Keep the two answers in one function so a future
    /// edit cannot make them disagree again.
    fn classify(&self, value: f64) -> Option<(NodeState, Direction)> {
        if matches!(self.critical_below, Some(c) if value <= c) {
            return Some((NodeState::Critical, Direction::Below));
        }
        if matches!(self.critical_above, Some(c) if value >= c) {
            return Some((NodeState::Critical, Direction::Above));
        }
        if matches!(self.warning_below, Some(w) if value <= w) {
            return Some((NodeState::Warning, Direction::Below));
        }
        if matches!(self.warning_above, Some(w) if value >= w) {
            return Some((NodeState::Warning, Direction::Above));
        }
        None
    }

    /// Classify one value. `Critical` beats `Warning`, and either side can raise either.
    #[must_use]
    pub fn evaluate(&self, value: f64) -> NodeState {
        self.classify(value)
            .map_or(NodeState::Ok, |(state, _)| state)
    }

    /// The side `value` actually crossed, if it crossed one.
    ///
    /// `None` means the value is inside the band (or the rule names no bound). Callers describing
    /// a breach to an operator must use this rather than [`Self::direction`]: `direction` answers
    /// "which side is this rule *filed* under", which exists for the legacy column and the N-1
    /// rollback, and is not a claim about any particular sample.
    #[must_use]
    pub fn breaching_side(&self, value: f64) -> Option<Direction> {
        self.classify(value).map(|(_, side)| side)
    }

    /// One side's warning bound.
    #[must_use]
    pub const fn warning_on(&self, side: Direction) -> Option<f64> {
        match side {
            Direction::Below => self.warning_below,
            Direction::Above => self.warning_above,
        }
    }

    /// One side's critical bound.
    #[must_use]
    pub const fn critical_on(&self, side: Direction) -> Option<f64> {
        match side {
            Direction::Below => self.critical_below,
            Direction::Above => self.critical_above,
        }
    }

    /// Combine two rules' bounds, keeping the more restrictive on **each side separately**.
    ///
    /// 🚨 "More restrictive" points opposite ways on the two sides — a lower upper bound trips
    /// earlier, a *higher* lower bound trips earlier — and that is not a new judgement:
    /// [`Direction::more_restrictive`] already knows it. Each side is folded under its own
    /// direction, so this adds no rule of its own.
    #[must_use]
    pub fn restrictive_with(self, other: Self) -> Self {
        Self {
            warning_below: restrictive(self.warning_below, other.warning_below, Direction::Below),
            critical_below: restrictive(
                self.critical_below,
                other.critical_below,
                Direction::Below,
            ),
            warning_above: restrictive(self.warning_above, other.warning_above, Direction::Above),
            critical_above: restrictive(
                self.critical_above,
                other.critical_above,
                Direction::Above,
            ),
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

/// The scope a threshold is defined at, ordered least → most specific: `Interface` (one port)
/// wins over `Node`, which wins over a folder group, which wins over `Group`, which wins over
/// `Profile`, which wins over `Global` (every node).
//
// The two notes below are deliberately `//`, not `///`: this type derives `ToSchema`, so a `///`
// line is published verbatim to every API client (see `openapi.json`). They are for whoever edits
// this enum, not for whoever calls the API.
//
// WARNING: variant order is load-bearing. `resolve_effective` takes `.max()` over the levels
// present, so a variant declared in the wrong position silently changes which rule wins.
// `Global` must stay first and `Interface` must stay last.
//
// WARNING: shared with `collection_items`, which has no `Global` scope. A collection item's level
// is decided by the endpoint that writes it (profile or node), never by client input, so the
// variant is unreachable there rather than a hole.
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
    /// Every node, with no scope id (broadest). `scope_id` is the empty string.
    Global,
    /// Defined on a device-class/profile.
    Profile,
    /// Defined on a **node tag value** (site/region/role/…), matched as a string against any of the
    /// node's tags. Legacy: nothing in the product writes `nodes.tags` except a config-bundle
    /// import, so `FolderGroup` is what a new rule uses. Kept because existing rules resolve
    /// through it.
    Group,
    /// Defined on a **folder group** (the inventory tree, ADR-022), and inherited by every group
    /// inside it. Serialized as `group_id` — the same spelling `WindowScope::FolderGroup` uses — so
    /// it cannot be confused with the tag-based `Group` above.
    #[serde(rename = "group_id")]
    FolderGroup,
    /// Defined directly on the node.
    Node,
    /// Defined on **one interface of one node** (narrowest, ADR-076). `scope_id` is
    /// `<node-uuid>:<ifindex>` — build and read it with [`interface_scope_id`] and
    /// [`parse_interface_scope_id`] rather than formatting it, so the resolver and the API
    /// validator cannot disagree about the shape.
    ///
    /// Only meaningful for a metric collected once per interface; a rule at this level on a
    /// node-wide metric matches nothing, because the engine only reaches for a per-port check
    /// when the collection catalogue says the metric is per-interface.
    Interface,
}

impl ScopeLevel {
    /// Every level, broadest first — the order the threshold list is sorted in and the order a
    /// picker should offer them.
    pub const ALL: [ScopeLevel; 6] = [
        ScopeLevel::Global,
        ScopeLevel::Profile,
        ScopeLevel::Group,
        ScopeLevel::FolderGroup,
        ScopeLevel::Node,
        ScopeLevel::Interface,
    ];

    /// Stable lowercase string for API payloads, DB columns, and logs.
    ///
    /// The `thresholds.scope_level` column holds exactly these six, written verbatim from the
    /// create request and read back by `ThresholdStore::parse_level`, so anything that binds a
    /// level into SQL must go through here rather than formatting the enum.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            ScopeLevel::Global => "global",
            ScopeLevel::Profile => "profile",
            ScopeLevel::Group => "group",
            ScopeLevel::FolderGroup => "group_id",
            ScopeLevel::Node => "node",
            ScopeLevel::Interface => "interface",
        }
    }

    /// The inverse of [`Self::as_str`]: an exact token, or `None`. Mirrors
    /// [`Direction::from_token`] — the API edge decides what a miss means (a 400), never this.
    #[must_use]
    pub fn from_token(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.as_str() == s)
    }
}

/// The `scope_id` of an [`ScopeLevel::Interface`] rule: `<node-uuid>:<ifindex>`.
///
/// One function so the API validator, the alert engine's `applies` and the WebUI's prefill cannot
/// disagree about the separator. A UUID contains no `:`, so the split is unambiguous.
#[must_use]
pub fn interface_scope_id(node: uuid::Uuid, ifindex: u32) -> String {
    format!("{node}:{ifindex}")
}

/// The inverse of [`interface_scope_id`]. `None` for anything that is not exactly that shape.
///
/// Strict on purpose: a scope id that does not parse is a rule that matches no port, and the API
/// refuses it at write time rather than storing something that looks configured and does nothing
/// (the failure ADR-075 決定 12 closed for the other levels). `rsplit_once` rather than
/// `split_once` would accept a UUID with a stray colon; there are none, but the whole point of a
/// single codec is that the reader and the writer agree without either having to be careful.
#[must_use]
pub fn parse_interface_scope_id(s: &str) -> Option<(uuid::Uuid, u32)> {
    let (node, idx) = s.split_once(':')?;
    // The digits are checked before parsing, because `u32::from_str` **accepts a leading `+`** —
    // so `<uuid>:+7` and `<uuid>:7` would both resolve to port 7 and the same rule could be stored
    // twice under two ids, one of which renders as "+7" in the list. One canonical spelling per
    // port is what makes the id usable as a key at all. (A unit test caught this; the comment
    // here used to claim the opposite.)
    if idx.is_empty() || !idx.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((uuid::Uuid::parse_str(node).ok()?, idx.parse::<u32>().ok()?))
}

/// A threshold rule for a single metric.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ThresholdRule {
    /// Stable metric name this rule applies to (e.g. `cpu_util`).
    pub metric: String,
    /// Value at/below which the node is `Warning`. `None` = no lower warning bound.
    #[serde(default)]
    pub warning_below: Option<f64>,
    /// Value at/below which the node is `Critical`. `None` = no lower critical bound.
    #[serde(default)]
    pub critical_below: Option<f64>,
    /// Value at/above which the node is `Warning`. `None` = no upper warning bound.
    #[serde(default)]
    pub warning_above: Option<f64>,
    /// Value at/above which the node is `Critical`. `None` = no upper critical bound.
    #[serde(default)]
    pub critical_above: Option<f64>,
    /// Hysteresis: consecutive samples the breach must hold before transitioning,
    /// to damp oscillation at the threshold. `0`/`1` = transition immediately.
    pub dwell_samples: u32,
}

impl ThresholdRule {
    /// Build a rule from its metric, bounds and dwell.
    ///
    /// ⚠️ The `direction` / `warning` / `critical` this type used to carry are **not** fields any
    /// more, they are [`Self::bounds`] accessors. That is the whole point of ADR-081: a struct
    /// literal could set the old triple and the new four to different things, and this repo has
    /// shipped exactly that kind of divergence before (`extensibility.md` §2). One carrier, and
    /// the compiler visits every construction site because the field set changed.
    #[must_use]
    pub fn new(metric: impl Into<String>, bounds: ThresholdBounds, dwell_samples: u32) -> Self {
        Self {
            metric: metric.into(),
            warning_below: bounds.warning_below,
            critical_below: bounds.critical_below,
            warning_above: bounds.warning_above,
            critical_above: bounds.critical_above,
            dwell_samples,
        }
    }

    /// The four bounds as one value — where every breach decision is made.
    #[must_use]
    pub const fn bounds(&self) -> ThresholdBounds {
        ThresholdBounds {
            warning_below: self.warning_below,
            critical_below: self.critical_below,
            warning_above: self.warning_above,
            critical_above: self.critical_above,
        }
    }

    /// The primary side. See [`ThresholdBounds::direction`] for why a range rule answers `Above`.
    #[must_use]
    pub const fn direction(&self) -> Direction {
        self.bounds().direction()
    }

    /// The primary side's warning bound — the legacy column and API field.
    #[must_use]
    pub const fn warning(&self) -> Option<f64> {
        self.bounds().warning()
    }

    /// The primary side's critical bound — the legacy column and API field.
    #[must_use]
    pub const fn critical(&self) -> Option<f64> {
        self.bounds().critical()
    }
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
    /// Effective lower warning bound, if any.
    pub warning_below: Option<f64>,
    /// Effective lower critical bound, if any.
    pub critical_below: Option<f64>,
    /// Effective upper warning bound, if any.
    pub warning_above: Option<f64>,
    /// Effective upper critical bound, if any.
    pub critical_above: Option<f64>,
    /// Effective dwell (hysteresis) in consecutive samples.
    pub dwell_samples: u32,
}

impl EffectiveThreshold {
    /// The four resolved bounds as one value.
    #[must_use]
    pub const fn bounds(&self) -> ThresholdBounds {
        ThresholdBounds {
            warning_below: self.warning_below,
            critical_below: self.critical_below,
            warning_above: self.warning_above,
            critical_above: self.critical_above,
        }
    }

    /// The primary side. See [`ThresholdBounds::direction`] for why a range rule answers `Above`.
    #[must_use]
    pub const fn direction(&self) -> Direction {
        self.bounds().direction()
    }

    /// The primary side's resolved warning bound.
    #[must_use]
    pub const fn warning(&self) -> Option<f64> {
        self.bounds().warning()
    }

    /// The primary side's resolved critical bound.
    #[must_use]
    pub const fn critical(&self) -> Option<f64> {
        self.bounds().critical()
    }

    /// Classify a single value, ignoring hysteresis (the alert engine applies dwell
    /// across samples). Returns `Critical`, `Warning`, or `Ok`.
    #[must_use]
    pub fn evaluate(&self, value: f64) -> NodeState {
        self.bounds().evaluate(value)
    }

    /// Whether `candidate` sits further into breach than `incumbent` for these bounds.
    ///
    /// 🚨 **Severity first, then the primary side** — and the order matters because a range rule
    /// has no single "further" to measure along. Two samples outside opposite ends of the band are
    /// both breaches; asking which is *more* below is meaningless when one of them is above.
    /// Comparing the classification first answers that, and falls back to the old directional
    /// comparison only to break a tie.
    ///
    /// For a rule naming one side this is **byte-identical to the comparison it replaces**:
    /// severity is monotone along that side, so it can only agree, and the tie-break restores the
    /// original answer wherever it does not decide. NaN never displaces an incumbent — it
    /// evaluates `Ok` and loses every comparison, so the first sample stands.
    #[must_use]
    pub fn is_worse(&self, candidate: f64, incumbent: f64) -> bool {
        let bounds = self.bounds();
        let (a, b) = (bounds.evaluate(candidate), bounds.evaluate(incumbent));
        if a != b {
            return state_rank(a) > state_rank(b);
        }
        match bounds.direction() {
            Direction::Above => candidate > incumbent,
            Direction::Below => candidate < incumbent,
        }
    }
}

/// Severity order for [`EffectiveThreshold::is_worse`].
///
/// Exhaustive rather than `_ => 0`, per the repo's ban on wildcards over a domain enum: `evaluate`
/// only ever returns three of the six, but a seventh state added later must be *decided about*
/// here rather than silently ranked as healthy.
const fn state_rank(state: NodeState) -> u8 {
    match state {
        NodeState::Critical => 2,
        NodeState::Warning => 1,
        // Not classifications `evaluate` produces. They reach this only if a caller folds samples
        // under a state the state machine owns, where "no breach" is the honest rank.
        NodeState::Ok | NodeState::Unknown | NodeState::Unreachable | NodeState::Maintenance => 0,
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
    let winners = rules.iter().filter(|r| r.level == winning_level);

    // ADR-081: no direction is read off the first winner any more. Before ranges existed this
    // function took `direction` from whichever rule happened to be first in the index and folded
    // every other winner's bounds under it — so a rule written the other way round had its bounds
    // compared the wrong way and vanished, while still being stored and listed. Each side now
    // folds under its own direction and neither can swallow the other.
    let mut bounds = ThresholdBounds::default();
    let mut dwell: u32 = 0;
    let mut metric = String::new();

    for ScopedThreshold { rule, .. } in winners {
        metric = rule.metric.clone();
        bounds = bounds.restrictive_with(rule.bounds());
        dwell = dwell.max(rule.dwell_samples);
    }

    Some(EffectiveThreshold {
        metric,
        warning_below: bounds.warning_below,
        critical_below: bounds.critical_below,
        warning_above: bounds.warning_above,
        critical_above: bounds.critical_above,
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
            // And the third mechanism: `from_token` is what the API edge and the store's reader
            // both go through, so it has to invert `as_str` for every level too. `FolderGroup` is
            // where all three would disagree by default — serde would say `folder_group`.
            assert_eq!(
                ScopeLevel::from_token(level.as_str()),
                Some(level),
                "{level:?}"
            );
        }
        // ALL is the whole enum, not a list someone forgot to extend when a level was added.
        assert_eq!(ScopeLevel::ALL.len(), 6);
        assert_eq!(
            ScopeLevel::ALL.map(ScopeLevel::as_str),
            [
                "global",
                "profile",
                "group",
                "group_id",
                "node",
                "interface"
            ],
            "ALL is ordered broadest-first — the threshold list and its filter both rely on it"
        );
        // The order is also the precedence (`resolve_effective` takes `.max()`), so assert it as
        // an ordering rather than trusting the array to be read that way.
        assert!(ScopeLevel::Global < ScopeLevel::Profile);
        assert!(ScopeLevel::Profile < ScopeLevel::Group);
        assert!(ScopeLevel::Group < ScopeLevel::FolderGroup);
        assert!(ScopeLevel::FolderGroup < ScopeLevel::Node);
        assert!(ScopeLevel::Node < ScopeLevel::Interface);
    }

    /// The interface scope id round-trips, and everything that is not exactly its shape is refused.
    ///
    /// The rejection half is the load-bearing one: a scope id that parses to nothing is a rule that
    /// is stored, listed, and matches no port — the exact failure ADR-075 決定 12 closed for the
    /// other levels. Note it includes an accepting case, so "reject everything" cannot pass.
    #[test]
    fn an_interface_scope_id_round_trips_and_rejects_everything_else() {
        let node = uuid::Uuid::parse_str("6f1c9d2a-0b3e-4a71-9c8d-2e5f7a1b4c60").expect("literal");
        for idx in [0_u32, 7, 4_294_967_295] {
            let id = interface_scope_id(node, idx);
            assert_eq!(
                parse_interface_scope_id(&id),
                Some((node, idx)),
                "{id} does not round-trip"
            );
        }
        // Port 0 is a real ifIndex on some agents, so it must survive as a value rather than
        // reading as "no port".
        assert_eq!(
            parse_interface_scope_id(&interface_scope_id(node, 0)),
            Some((node, 0))
        );

        for bad in [
            "",
            "6f1c9d2a-0b3e-4a71-9c8d-2e5f7a1b4c60",  // no port
            "6f1c9d2a-0b3e-4a71-9c8d-2e5f7a1b4c60:", // empty port
            "6f1c9d2a-0b3e-4a71-9c8d-2e5f7a1b4c60:-1", // negative
            "6f1c9d2a-0b3e-4a71-9c8d-2e5f7a1b4c60:+7", // signed
            "6f1c9d2a-0b3e-4a71-9c8d-2e5f7a1b4c60: 7", // padded
            "6f1c9d2a-0b3e-4a71-9c8d-2e5f7a1b4c60:x", // not a number
            "6f1c9d2a-0b3e-4a71-9c8d-2e5f7a1b4c60:4294967296", // one past u32
            "not-a-uuid:7",
            ":7",
        ] {
            assert_eq!(parse_interface_scope_id(bad), None, "{bad} was accepted");
        }
    }

    fn rule(level: ScopeLevel, warning: f64, critical: f64) -> ScopedThreshold {
        ScopedThreshold::new(
            level,
            ThresholdRule::new(
                "cpu_util",
                ThresholdBounds::above(Some(warning), Some(critical)),
                3,
            ),
        )
    }

    fn bounded(level: ScopeLevel, bounds: ThresholdBounds) -> ScopedThreshold {
        ScopedThreshold::new(level, ThresholdRule::new("rx_dbm", bounds, 3))
    }

    fn effective(metric: &str, bounds: ThresholdBounds, dwell: u32) -> EffectiveThreshold {
        EffectiveThreshold {
            metric: metric.into(),
            warning_below: bounds.warning_below,
            critical_below: bounds.critical_below,
            warning_above: bounds.warning_above,
            critical_above: bounds.critical_above,
            dwell_samples: dwell,
        }
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
        assert_eq!(eff.warning(), Some(60.0));
        assert_eq!(eff.critical(), Some(85.0));
    }

    #[test]
    fn a_global_rule_is_the_weakest_and_applies_when_nothing_else_does() {
        // ADR-075: `Global` is the "system default" tier ADR-013 named. It must lose to every
        // other level — a fleet default that overrode a node override would be the opposite of an
        // override — and must still resolve on its own when it is the only rule.
        let eff = resolve_effective(&[
            rule(ScopeLevel::Global, 90.0, 99.0),
            rule(ScopeLevel::Profile, 80.0, 95.0),
        ])
        .unwrap();
        assert_eq!(eff.warning(), Some(80.0), "profile beats global");

        let alone = resolve_effective(&[rule(ScopeLevel::Global, 90.0, 99.0)]).unwrap();
        assert_eq!(alone.warning(), Some(90.0));
        assert_eq!(alone.critical(), Some(99.0));
    }
    #[test]
    fn same_level_tiebreak_takes_most_restrictive_above() {
        // Two groups; "above" → smaller bound is stricter.
        let rules = [
            rule(ScopeLevel::Group, 70.0, 90.0),
            rule(ScopeLevel::Group, 65.0, 92.0),
        ];
        let eff = resolve_effective(&rules).unwrap();
        assert_eq!(eff.warning(), Some(65.0)); // stricter warning
        assert_eq!(eff.critical(), Some(90.0)); // stricter critical
    }

    #[test]
    fn same_level_tiebreak_takes_most_restrictive_below() {
        // "below" → larger bound is stricter (trips earlier as value falls).
        let mk = |w: f64, c: f64| {
            ScopedThreshold::new(
                ScopeLevel::Group,
                ThresholdRule::new("free_mem", ThresholdBounds::below(Some(w), Some(c)), 1),
            )
        };
        let eff = resolve_effective(&[mk(20.0, 10.0), mk(25.0, 8.0)]).unwrap();
        assert_eq!(eff.warning(), Some(25.0));
        assert_eq!(eff.critical(), Some(10.0));
    }

    #[test]
    fn present_bound_beats_missing() {
        let with = ScopedThreshold::new(
            ScopeLevel::Node,
            ThresholdRule::new("cpu_util", ThresholdBounds::above(Some(70.0), None), 2),
        );
        let without = ScopedThreshold::new(
            ScopeLevel::Node,
            ThresholdRule::new("cpu_util", ThresholdBounds::above(None, Some(90.0)), 5),
        );
        let eff = resolve_effective(&[with, without]).unwrap();
        assert_eq!(eff.warning(), Some(70.0));
        assert_eq!(eff.critical(), Some(90.0));
        assert_eq!(eff.dwell_samples, 5); // longest dwell wins
    }

    #[test]
    fn evaluate_classifies_above() {
        let eff = effective(
            "cpu_util",
            ThresholdBounds::above(Some(70.0), Some(90.0)),
            3,
        );
        assert_eq!(eff.evaluate(50.0), NodeState::Ok);
        assert_eq!(eff.evaluate(70.0), NodeState::Warning);
        assert_eq!(eff.evaluate(95.0), NodeState::Critical);
    }

    #[test]
    fn evaluate_classifies_below() {
        let eff = effective(
            "free_mem",
            ThresholdBounds::below(Some(25.0), Some(10.0)),
            1,
        );
        assert_eq!(eff.evaluate(40.0), NodeState::Ok);
        assert_eq!(eff.evaluate(20.0), NodeState::Warning);
        assert_eq!(eff.evaluate(5.0), NodeState::Critical);
    }

    #[test]
    fn boolean_up_gauge_below_half_only_trips_when_down() {
        // `http_up` is a 0/1 gauge. Because "below" is inclusive (`value <= bound`), the default
        // bound must sit between the states (0.5) — a bound of 1.0 would mis-fire on the healthy
        // value 1. Regression guard for the "URL monitor permanently Critical" bug.
        let eff = effective("http_up", ThresholdBounds::below(None, Some(0.5)), 2);
        assert_eq!(eff.evaluate(1.0), NodeState::Ok); // up + status OK
        assert_eq!(eff.evaluate(0.0), NodeState::Critical); // down / wrong status
    }

    #[test]
    fn empty_resolves_to_none() {
        assert_eq!(resolve_effective(&[]), None);
    }

    // ---- ADR-081: the four bounds ----

    #[test]
    fn a_band_rule_trips_on_either_side_and_is_ok_between() {
        // An optical receive level: dark below -20 dBm, overdriven above -3 dBm.
        let eff = effective(
            "rx_dbm",
            ThresholdBounds {
                warning_below: Some(-18.0),
                critical_below: Some(-20.0),
                warning_above: Some(-5.0),
                critical_above: Some(-3.0),
            },
            3,
        );
        assert_eq!(eff.evaluate(-25.0), NodeState::Critical, "dark");
        assert_eq!(eff.evaluate(-19.0), NodeState::Warning, "dimming");
        assert_eq!(eff.evaluate(-12.0), NodeState::Ok, "inside the band");
        assert_eq!(eff.evaluate(-4.0), NodeState::Warning, "getting hot");
        assert_eq!(eff.evaluate(-1.0), NodeState::Critical, "overdriven");
    }

    /// A breach must be described by the bound it crossed, not by the rule's primary side.
    ///
    /// `direction()` answers "which side is this rule filed under" — an artefact of the legacy
    /// column and the N-1 rollback, and `Above` for every band. Describing a breach with it made
    /// an alert state something false: on the live deployment 2026-08-21 a value of 0.909 that had
    /// crossed `critical_below: 1.0` went out as `threshold: 5000.0, direction: above`. Nothing
    /// caught it because every test in the suite used a one-sided rule, where the two coincide.
    #[test]
    fn the_reported_bound_is_the_one_that_was_crossed() {
        let bounds = ThresholdBounds {
            warning_below: Some(-18.0),
            critical_below: Some(-20.0),
            warning_above: Some(-5.0),
            critical_above: Some(-3.0),
        };
        // The rule is filed under `above`, and every answer below has to survive that.
        assert_eq!(bounds.direction(), Direction::Above);

        for (value, side, warning, critical) in [
            (-25.0, Direction::Below, Some(-18.0), Some(-20.0)),
            (-19.0, Direction::Below, Some(-18.0), Some(-20.0)),
            (-4.0, Direction::Above, Some(-5.0), Some(-3.0)),
            (-1.0, Direction::Above, Some(-5.0), Some(-3.0)),
        ] {
            assert_eq!(bounds.breaching_side(value), Some(side), "value {value}");
            assert_eq!(bounds.warning_on(side), warning, "value {value}");
            assert_eq!(bounds.critical_on(side), critical, "value {value}");
        }
        assert_eq!(
            bounds.breaching_side(-12.0),
            None,
            "inside the band nothing was crossed, and no bound may be named"
        );
    }

    /// The two answers come out of one walk, so they can never disagree — pinned across the band
    /// and past both ends, because the failure this guards is silent: a value classified `Critical`
    /// while no side admits to having been crossed would publish an alert with no threshold at all.
    #[test]
    fn evaluate_and_breaching_side_always_agree() {
        let bounds = ThresholdBounds {
            warning_below: Some(-18.0),
            critical_below: Some(-20.0),
            warning_above: Some(-5.0),
            critical_above: Some(-3.0),
        };
        let mut v = -30.0;
        while v <= 5.0 {
            let breached = bounds.breaching_side(v).is_some();
            assert_eq!(
                breached,
                bounds.evaluate(v) != NodeState::Ok,
                "value {v}: evaluate and breaching_side disagree"
            );
            v += 0.25;
        }
        // A one-sided rule keeps naming its own side and nothing else.
        let up = ThresholdBounds::above(Some(70.0), Some(90.0));
        assert_eq!(up.breaching_side(95.0), Some(Direction::Above));
        assert_eq!(up.breaching_side(1.0), None, "low values are not a breach");
        assert_eq!(up.warning_on(Direction::Below), None);
    }

    #[test]
    fn one_sided_rules_keep_their_side_and_report_it() {
        let up = effective("cpu_util", ThresholdBounds::above(Some(70.0), None), 1);
        assert_eq!(up.direction(), Direction::Above);
        assert_eq!(up.warning(), Some(70.0));
        assert_eq!(
            up.evaluate(10.0),
            NodeState::Ok,
            "a low value is not a breach"
        );

        let down = effective("free_mem", ThresholdBounds::below(Some(20.0), None), 1);
        assert_eq!(down.direction(), Direction::Below);
        assert_eq!(down.warning(), Some(20.0));
        assert_eq!(
            down.evaluate(99.0),
            NodeState::Ok,
            "a high value is not a breach"
        );
    }

    #[test]
    fn restrictive_reverses_per_side_at_the_same_level() {
        // 🚨 The case ADR-081 exists for. Two rules at ONE level, each naming both sides:
        // the winner is the *higher* lower bound and the *lower* upper bound — opposite
        // directions, resolved in a single pass. Before ADR-081 the second rule's bounds were
        // folded under the first rule's direction and one side was silently discarded.
        let wide = ThresholdBounds {
            warning_below: Some(-18.0),
            critical_below: Some(-22.0),
            warning_above: Some(-4.0),
            critical_above: Some(-2.0),
        };
        let narrow = ThresholdBounds {
            warning_below: Some(-16.0), // stricter: trips earlier on the way down
            critical_below: Some(-20.0),
            warning_above: Some(-6.0), // stricter: trips earlier on the way up
            critical_above: Some(-3.0),
        };
        let eff = resolve_effective(&[
            bounded(ScopeLevel::Group, wide),
            bounded(ScopeLevel::Group, narrow),
        ])
        .unwrap();
        assert_eq!(eff.warning_below, Some(-16.0));
        assert_eq!(eff.critical_below, Some(-20.0));
        assert_eq!(eff.warning_above, Some(-6.0));
        assert_eq!(eff.critical_above, Some(-3.0));
    }

    #[test]
    fn opposite_facing_rules_at_one_level_both_survive() {
        // The defect ADR-081 names: an operator writes "below 10" and "above 90" as two rules
        // because one rule could not hold both. The second used to be folded under the first's
        // direction and vanish. Now each side keeps its own.
        let eff = resolve_effective(&[
            bounded(ScopeLevel::Node, ThresholdBounds::below(None, Some(10.0))),
            bounded(ScopeLevel::Node, ThresholdBounds::above(None, Some(90.0))),
        ])
        .unwrap();
        assert_eq!(eff.critical_below, Some(10.0));
        assert_eq!(eff.critical_above, Some(90.0));
        assert_eq!(eff.evaluate(5.0), NodeState::Critical);
        assert_eq!(eff.evaluate(50.0), NodeState::Ok);
        assert_eq!(eff.evaluate(95.0), NodeState::Critical);
    }

    #[test]
    fn is_worse_ranks_by_severity_then_by_the_primary_side() {
        // A band rule has no single "further into breach", so severity decides first.
        let band = effective(
            "rx_dbm",
            ThresholdBounds {
                warning_below: Some(-18.0),
                critical_below: Some(-20.0),
                warning_above: Some(-5.0),
                critical_above: Some(-3.0),
            },
            1,
        );
        assert!(
            band.is_worse(-25.0, -4.0),
            "critical beats warning, downward"
        );
        assert!(band.is_worse(-1.0, -19.0), "critical beats warning, upward");
        assert!(!band.is_worse(-12.0, -19.0), "ok never displaces a warning");

        // One-sided rules keep the comparison they always had.
        let up = effective(
            "cpu_util",
            ThresholdBounds::above(Some(70.0), Some(90.0)),
            1,
        );
        assert!(up.is_worse(95.0, 92.0), "further above wins a critical tie");
        assert!(!up.is_worse(92.0, 95.0));
        let down = effective(
            "free_mem",
            ThresholdBounds::below(Some(25.0), Some(10.0)),
            1,
        );
        assert!(down.is_worse(2.0, 5.0), "further below wins a critical tie");
        assert!(!down.is_worse(5.0, 2.0));
    }

    #[test]
    fn nan_never_displaces_an_incumbent() {
        let up = effective("cpu_util", ThresholdBounds::above(Some(70.0), None), 1);
        assert!(!up.is_worse(f64::NAN, 80.0));
    }

    #[test]
    fn from_legacy_round_trips_through_the_primary_side() {
        // The rollback contract: whatever an older core wrote as (direction, warning, critical)
        // must read back as exactly that, and a one-sided rule must report the same triple back.
        for direction in Direction::ALL {
            let bounds = ThresholdBounds::from_legacy(direction, Some(30.0), Some(40.0));
            assert_eq!(bounds.direction(), direction);
            assert_eq!(bounds.warning(), Some(30.0));
            assert_eq!(bounds.critical(), Some(40.0));
        }
        // A rule with no bound at all still answers a direction rather than panicking, and says
        // it cannot fire.
        let empty = ThresholdBounds::default();
        assert!(empty.is_empty());
        assert_eq!(empty.direction(), Direction::Above);
        assert_eq!(empty.evaluate(0.0), NodeState::Ok);
    }

    #[test]
    fn lowest_bound_spans_both_sides() {
        let band = ThresholdBounds {
            warning_below: Some(-18.0),
            critical_below: Some(-20.0),
            warning_above: Some(-5.0),
            critical_above: Some(-3.0),
        };
        assert_eq!(band.lowest_bound(), Some(-20.0));
        assert_eq!(ThresholdBounds::default().lowest_bound(), None);
    }
}
