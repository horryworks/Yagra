// SPDX-License-Identifier: AGPL-3.0-only
//! Which threshold applies, and what a check is called.
//!
//! The **rule index and scope resolution** half of the alert module (ADR-083). Given a
//! (node, port, metric) it answers which [`yagra_common::EffectiveThreshold`] governs, folding
//! scope inheritance (global → profile → tag group → folder → node → port) and the ADR-076
//! bucketing that keeps that answer from costing O(all rules) per sample. It also owns the
//! deterministic check ids, because an id is a function of scope, not of state.
//!
//! **Pure.** No I/O, no clock, no locks — [`super::engine`] holds all of that. That is what lets
//! `the_indexed_resolve_agrees_with_the_reference_implementation` run the fast path and a naive
//! one over the same inputs and demand the same answer.

use std::collections::{BTreeSet, HashMap};

use uuid::Uuid;
use yagra_alert::Subject;
use yagra_common::{
    resolve_effective, CheckId, Direction, EffectiveThreshold, IfIndex, NodeId, NodeState,
    ScopeLevel, ScopedThreshold,
};
use yagra_topology::Topology;

use crate::thresholds::StoredThreshold;

use super::NodeMeta;

/// Liveness check name (distinct from any metric name). `pub(crate)` so anything that has to
/// recognise the sentinel — the RCA prompt renders it as "liveness" rather than showing an operator
/// an internal token — tests against this rather than re-spelling the literal.
pub(crate) const LIVENESS: &str = "__liveness__";
/// Consecutive failed polls the **seeded fleet-default** liveness rule asks for (ADR-075).
///
/// This is the seed's value, not the engine's. The engine reads the dwell off whichever
/// `__liveness__` rule resolves for the node, so an operator can raise it for one site or delete
/// the rule and stop paging altogether. It stays 3 because that is what the removed hard-coded
/// constant was: an upgrade must not change how quickly an existing fleet pages.
///
/// It is also the fallback the **state machine** uses when no rule resolves. That is deliberate
/// and is not "the rule is still there": the committed state drives the Nodes page, the down-set
/// and dependency suppression, none of which an operator asked to switch off by deleting an
/// alert rule. Deleting the rule stops the paging, not the bookkeeping.
pub(crate) const DEFAULT_LIVENESS_DWELL: u32 = 3;
/// The fleet-default liveness rule every deployment is seeded with (`repo.rs`, ADR-075).
///
/// Test-only, and shared rather than re-spelled: up/down alerting is rule-driven now, so an
/// `AlertManager` with no config commits state and pages nobody. Any test that expects a node to
/// fire has to install this, and two modules needed it — a second copy would be a second chance
/// to disagree with what `repo.rs` actually seeds.
#[cfg(test)]
pub(crate) fn seeded_liveness_rule() -> StoredThreshold {
    StoredThreshold::new(
        uuid::Uuid::nil(),
        ScopeLevel::Global,
        Vec::new(),
        yagra_common::ThresholdRule::new(
            LIVENESS,
            yagra_common::ThresholdBounds::below(None, None),
            DEFAULT_LIVENESS_DWELL,
        ),
    )
}

/// A snapshot of thresholds + node metadata + dependency topology the engine evaluates
/// against. Rebuilt periodically from the database so threshold/topology edits take effect
/// without a restart.
#[derive(Debug, Clone, Default)]
pub struct AlertConfig {
    /// Thresholds bucketed by metric name so per-sample resolution scans only the rules for that
    /// one metric, not the fleet's entire threshold set (S19). Built once at construction; a poll
    /// with M samples then costs O(rules-for-those-M-metrics), not O(all thresholds) × M.
    ///
    /// Since ADR-076 increment 6 each bucket is itself indexed — see [`MetricRules`], which is
    /// what stops one metric's bucket being scanned in full for every port.
    pub(super) by_metric: HashMap<String, MetricRules>,
    pub(super) node_meta: HashMap<NodeId, NodeMeta>,
    pub(super) topology: Topology,
    /// Nodes currently inside an active maintenance window (resolved at refresh time).
    pub(super) maintenance: BTreeSet<NodeId>,
    /// Folder groups that have at least one node in each poller pool — what makes a pool-coverage
    /// alert visible to the group-scoped operator whose site went dark (`api/scope.rs`).
    ///
    /// Precomputed here rather than resolved per request because the answer needs each node's
    /// **effective** pool (own > nearest ancestor folder > default, `poolres.rs`), and doing that
    /// per SSE frame per subscriber would be a full-fleet walk on the hottest read path. The map is
    /// small — pools × groups — and is rebuilt with the rest of the snapshot, i.e. only when the
    /// config generation advances (S6/ADR-026). Ungrouped nodes contribute nothing: a scoped caller
    /// cannot see them anyway, so a pool that only holds ungrouped nodes stays admin-only.
    pub(super) pool_groups: HashMap<String, BTreeSet<Uuid>>,
    /// Metric names that publish **one series per interface**, so a sample of one needs its own
    /// check per port rather than sharing the node's (ADR-076 decision 1).
    ///
    /// 🚨 The membership comes from the **collection catalogue**
    /// (`yagra_common::item_publishes_per_interface`), never from whether a sample happens to
    /// carry an `ifindex`. ADR-011 gives a table walk exactly one row key, so a CPU number, an
    /// `entPhysicalIndex` and a PoE group all arrive spelled `ifindex`; on a chassis those
    /// collide with real port numbers (measured v0.2.15: 30 of 108 vendor readings). Splitting on
    /// the label would therefore invent a per-port check for a chassis-wide reading.
    ///
    /// Empty means "nothing is per-interface", which is the pre-ADR-076 behaviour — the safe
    /// direction for a config that failed to load.
    pub(super) per_interface: BTreeSet<String>,
}

/// One metric's rules, split by how narrowly each can be addressed (ADR-076 increment 6).
///
/// Bucketing by metric alone still made [`AlertConfig::resolve`] scan **every rule on that metric**
/// for **every** `(node, port)` it was asked about — a cost linear in a number the operator
/// controls. One 48-port switch carries 96 port rules, so a hundred switches is 9,600, and
/// measured 2026-08-20 that is **27 ns per (port × rule)**: a tick evaluating 100k candidate ports
/// spent 26 seconds of its 60-second budget inside this one function, per direction. The same
/// `resolve` also serves the ordinary poll path (one call per sample per port), where at fleet
/// scale it is called far more often than the interface watch calls it.
///
/// 🚨 **The index does not decide whether a rule applies.** [`threshold_applies`] still runs over
/// every candidate the buckets yield; they only shorten the list. Two answers to "does this rule
/// apply" is precisely the drift `extensibility.md` §2 exists for, and
/// `the_indexed_resolve_agrees_with_the_reference_implementation` is the test that pins the
/// buckets to the predicate.
#[derive(Debug, Clone, Default)]
pub(super) struct MetricRules {
    /// Rules whose scope cannot be reduced to a lookup key: `Global`, `Profile`, `Group` and
    /// `FolderGroup` — plus any `Node`/`Interface` rule any of whose targets is not spelled the way the
    /// keys below are built, so that `threshold_applies` keeps the last word on those.
    ///
    /// Small by construction: an operator writes these by hand, one per fleet / profile / tag /
    /// folder. This is the only bucket a resolve still scans linearly.
    pub(super) broad: Vec<Indexed>,
    /// `ScopeLevel::Node` rules, keyed by the node they name.
    pub(super) by_node: HashMap<Uuid, Vec<Indexed>>,
    /// `ScopeLevel::Interface` rules, keyed by the exact port they name.
    pub(super) by_port: HashMap<(Uuid, u32), Vec<Indexed>>,
}

/// A rule plus its position in the config's original order.
///
/// 🚨 The position is load-bearing, not bookkeeping. [`resolve_effective`] takes `direction` from
/// the **first** rule at the winning level, and the `thresholds` table has no unique constraint
/// over `(scope_level, scope_ids, metric)` — so two rules with opposite directions can sit at the
/// same level, and which one wins depends on which comes first. Bucketing reorders them; `seq`
/// puts them back. (That the answer depends on order at all is a pre-existing sharp edge; the goal
/// here is not to widen it.)
#[derive(Debug, Clone)]
pub(super) struct Indexed {
    pub(super) seq: u32,
    pub(super) t: StoredThreshold,
}

impl MetricRules {
    /// File one rule into the bucket that can find it again.
    ///
    /// The keys are built so a lookup gives **exactly** the answer [`threshold_applies`] would:
    ///
    /// * `Node` — keyed by the parsed UUID, but **only when it round-trips to the same string**.
    ///   `threshold_applies` compares each target against `node.to_string()`, and `NodeId`'s
    ///   `Display` delegates to `Uuid`'s, so the canonical hyphenated lowercase form is the only
    ///   spelling that has ever matched. A braced, URN or upper-case id matches nothing today, so
    ///   the rule goes to `broad` — where `threshold_applies` still says no — rather than being
    ///   silently promoted into a rule that now fires.
    ///
    /// ⚠️ **All-or-nothing per rule, never per target** (ADR-078). A rule with one canonical
    ///   target and one malformed one goes wholly to `broad`: filing the good half under a
    ///   lookup key would leave the other half reachable only by the linear scan, so a lookup
    ///   would answer with a subset of what `threshold_applies` says — which is exactly the
    ///   disagreement the index is forbidden to introduce.
    /// * `Interface` — keyed by `parse_interface_scope_id`, which *is* the predicate
    ///   `threshold_applies` uses. An id it cannot parse matches nothing either way.
    fn insert(&mut self, seq: u32, t: StoredThreshold) {
        let e = Indexed { seq, t };
        match e.t.level {
            // ADR-078: a rule names a set, so it is filed under EVERY key it can be looked up
            // by. If any one target is not spelled the canonical way the whole rule goes to
            // `broad` instead — splitting it would file half of it under a lookup key and leave
            // the other half findable only by the linear scan, so a lookup would answer with a
            // subset of what `threshold_applies` says. One bucket per rule, never per target.
            ScopeLevel::Node => {
                let keys: Option<Vec<Uuid>> =
                    e.t.scope_ids
                        .iter()
                        .map(|s| Uuid::parse_str(s).ok().filter(|id| id.to_string() == *s))
                        .collect();
                match keys {
                    Some(keys) if !keys.is_empty() => {
                        for id in keys {
                            self.by_node.entry(id).or_default().push(e.clone());
                        }
                    }
                    _ => self.broad.push(e),
                }
            }
            ScopeLevel::Interface => {
                let keys: Option<Vec<(Uuid, u32)>> =
                    e.t.scope_ids
                        .iter()
                        .map(|s| yagra_common::parse_interface_scope_id(s))
                        .collect();
                match keys {
                    Some(keys) if !keys.is_empty() => {
                        for key in keys {
                            self.by_port.entry(key).or_default().push(e.clone());
                        }
                    }
                    _ => self.broad.push(e),
                }
            }
            // Listed rather than caught by `_` so a seventh level has to decide which bucket it
            // belongs in, instead of silently becoming a rule nothing can look up.
            ScopeLevel::Global
            | ScopeLevel::Profile
            | ScopeLevel::Group
            | ScopeLevel::FolderGroup => self.broad.push(e),
        }
    }

    /// Every rule on this metric, in the config's original order — for callers that need the whole
    /// set rather than one node's slice (query planning, not per-port resolution). Once per tick,
    /// so the sort is not on any hot path.
    fn all(&self) -> Vec<&StoredThreshold> {
        let mut all: Vec<&Indexed> = self
            .broad
            .iter()
            .chain(self.by_node.values().flatten())
            .chain(self.by_port.values().flatten())
            .collect();
        all.sort_unstable_by_key(|e| e.seq);
        all.into_iter().map(|e| &e.t).collect()
    }

    /// The rules that could possibly match this `(node, port)` — the whole point of the split.
    /// Returned in the config's original order, for the reason on [`Indexed::seq`].
    fn candidates(&self, node: NodeId, ifindex: Option<IfIndex>) -> Vec<&Indexed> {
        let mut out: Vec<&Indexed> = self.broad.iter().collect();
        if let Some(v) = self.by_node.get(&node.as_uuid()) {
            out.extend(v);
        }
        // A sample with no port cannot match an interface rule — the same thing
        // `threshold_applies` says, expressed as "do not even look".
        if let Some(v) = ifindex.and_then(|i| self.by_port.get(&(node.as_uuid(), i.0))) {
            out.extend(v);
        }
        out.sort_unstable_by_key(|e| e.seq);
        out
    }
}

impl AlertConfig {
    /// Build a config from the stored thresholds and node metadata (no dependency edges;
    /// add them with [`Self::with_topology`]).
    #[must_use]
    pub fn new(thresholds: Vec<StoredThreshold>, node_meta: HashMap<NodeId, NodeMeta>) -> Self {
        let mut by_metric: HashMap<String, MetricRules> = HashMap::new();
        for (seq, t) in thresholds.into_iter().enumerate() {
            // `seq` is the position in the caller's list, which is the order every resolve must
            // see the rules in — see [`Indexed`]. Saturating is unreachable (it would need 4
            // billion rules) and is spelled out rather than cast, so it cannot wrap silently.
            let seq = u32::try_from(seq).unwrap_or(u32::MAX);
            by_metric
                .entry(t.rule.metric.clone())
                .or_default()
                .insert(seq, t);
        }
        Self {
            by_metric,
            node_meta,
            topology: Topology::new(),
            maintenance: BTreeSet::new(),
            pool_groups: HashMap::new(),
            per_interface: BTreeSet::new(),
        }
    }

    /// Attach the dependency topology used for parent-down suppression / root-cause roll-up.
    #[must_use]
    pub fn with_topology(mut self, topology: Topology) -> Self {
        self.topology = topology;
        self
    }

    /// Attach the set of nodes currently inside an active maintenance window.
    #[must_use]
    pub fn with_maintenance(mut self, maintenance: BTreeSet<NodeId>) -> Self {
        self.maintenance = maintenance;
        self
    }

    /// Attach the pool → folder-group map used to scope pool-coverage alerts.
    #[must_use]
    pub fn with_pool_groups(mut self, pool_groups: HashMap<String, BTreeSet<Uuid>>) -> Self {
        self.pool_groups = pool_groups;
        self
    }

    /// Attach the set of metric names that publish one series per interface (ADR-076).
    #[must_use]
    pub fn with_per_interface(mut self, per_interface: BTreeSet<String>) -> Self {
        self.per_interface = per_interface;
        self
    }

    /// Whether `metric` publishes one series per interface, per the collection catalogue.
    #[must_use]
    pub fn is_per_interface(&self, metric: &str) -> bool {
        self.per_interface.contains(metric)
    }

    /// What the rules in force for `metric` cover, for the interface evaluator's query planning
    /// (ADR-076 decision 3).
    ///
    /// Returns the smallest bound any of them names, and the nodes they name **when every rule is
    /// narrow enough to enumerate**. A `global`, `profile`, `group` or `group_id` rule covers a set
    /// the evaluator cannot enumerate without a fleet walk, so any of those collapses the node list
    /// to `None` = "the whole fleet" and the floor becomes the only bound on the query.
    ///
    /// Reading the snapshot the engine already holds, rather than re-querying `ThresholdStore`,
    /// is what keeps "the rules the query was planned for" and "the rules the classification uses"
    /// the same set — a plan built from a staler read would query ports nothing evaluates, or miss
    /// ports something does.
    #[must_use]
    pub fn interface_rule_coverage(&self, metric: &str) -> RuleCoverage {
        let Some(rules) = self.by_metric.get(metric).map(MetricRules::all) else {
            // An empty node set, not `Default` — `RuleCoverage::default()` leaves `nodes` at
            // `None`, which means **the whole fleet**, the opposite of what "nobody wrote a rule
            // for this metric" should say. Harmless while every caller checks `lowest_bound`
            // first, and a trap the moment one does not.
            return RuleCoverage {
                lowest_bound: None,
                has_below: false,
                nodes: Some(BTreeSet::new()),
            };
        };
        let mut lowest_bound: Option<f64> = None;
        let mut has_below = false;
        let mut nodes: Option<BTreeSet<Uuid>> = Some(BTreeSet::new());
        for t in rules {
            // The lowest bound named on **either side** of the band: whichever trips first is what
            // the floor must not exclude. A rule with no bound at all cannot fire and contributes
            // nothing. Since ADR-081 one rule can name up to four, so this spans them rather than
            // reading the primary side's two — reading only the primary side would leave the floor
            // above a bound that does fire, and the ports it excludes are never evaluated.
            let bounds = t.rule.bounds();
            if let Some(lowest) = bounds.lowest_bound() {
                lowest_bound = Some(lowest_bound.map_or(lowest, |b: f64| b.min(lowest)));
            }
            // Only a rule that can actually fire gets to widen the query, and a range rule widens
            // it as soon as *either* of its lower bounds is set. A rule with no bound would
            // otherwise force every port through the floorless path forever.
            if bounds.has_below() {
                has_below = true;
            }
            match t.level {
                ScopeLevel::Node => {
                    if let Some(set) = nodes.as_mut() {
                        for id in t.scope_ids.iter().filter_map(|s| Uuid::parse_str(s).ok()) {
                            set.insert(id);
                        }
                    }
                }
                ScopeLevel::Interface => {
                    if let Some(set) = nodes.as_mut() {
                        for (id, _) in t
                            .scope_ids
                            .iter()
                            .filter_map(|s| yagra_common::parse_interface_scope_id(s))
                        {
                            set.insert(id);
                        }
                    }
                }
                // Not enumerable without walking the fleet — and a rule scoped this broadly is one
                // whose author meant it to be.
                ScopeLevel::Global
                | ScopeLevel::Profile
                | ScopeLevel::Group
                | ScopeLevel::FolderGroup => nodes = None,
            }
        }
        RuleCoverage {
            lowest_bound,
            has_below,
            nodes: if lowest_bound.is_none() {
                // No bound anywhere ⇒ nothing can fire ⇒ nothing to query. Spelled as an empty
                // set rather than `None`, which would mean "the whole fleet".
                Some(BTreeSet::new())
            } else {
                nodes
            },
        }
    }

    /// Resolve the effective threshold for one (node, metric), honouring scope inheritance.
    ///
    /// `ifindex` names the port a per-interface sample came from, so an [`ScopeLevel::Interface`]
    /// rule can be matched against it (ADR-076). `None` means "not a per-port question" and makes
    /// every interface-scoped rule non-applicable, which is the correct answer for a node-wide
    /// metric: a port rule must not leak onto the node's own check.
    pub(super) fn resolve(
        &self,
        node: NodeId,
        ifindex: Option<IfIndex>,
        metric: &str,
    ) -> Option<EffectiveThreshold> {
        let rules = self.by_metric.get(metric)?;
        let meta = self.node_meta.get(&node);
        // `candidates` narrows by scope key; `threshold_applies` still decides. Keeping both means
        // the predicate has exactly one definition and the index is only ever an optimisation.
        let matched: Vec<&StoredThreshold> = rules
            .candidates(node, ifindex)
            .into_iter()
            .map(|e| &e.t)
            .filter(|t| threshold_applies(t, node, ifindex, meta))
            .collect();
        // A folder-group rule can match the node's own group *and* any group above it, so several
        // rules can arrive at the same `ScopeLevel::FolderGroup`. `resolve_effective` only knows
        // levels, and would merge them with "most restrictive wins" — which would silently ignore
        // a deliberately looser rule on an inner group. ADR-013's first rule is most-specific-wins,
        // and the chain is ordered, so the nearest group's rules are the only ones that survive.
        let nearest = nearest_folder_depth(&matched, meta);
        let scoped: Vec<ScopedThreshold> = matched
            .into_iter()
            .filter(|t| t.level != ScopeLevel::FolderGroup || folder_depth(t, meta) == nearest)
            .map(|t| ScopedThreshold::new(t.level, t.rule.clone()))
            .collect();
        resolve_effective(&scoped)
    }
}

/// Whether one stored rule applies to `(node, ifindex)`.
///
/// Free rather than a method, and `pub(crate)` rather than private, because two places have to
/// answer this identically: the engine, resolving a sample, and `GET
/// /nodes/{id}/interfaces/{ifindex}/thresholds`, showing an operator which rules reach a port
/// (ADR-076 決定 11). A second copy of scope inheritance is exactly the mirror `extensibility.md`
/// forbids — the first one would drift the day a level is added.
///
/// `meta` is the node's own metadata (profile, tag values, folder chain); `None` for a node the
/// snapshot has never seen, which matches only the global level.
pub(crate) fn threshold_applies(
    t: &StoredThreshold,
    node: NodeId,
    ifindex: Option<IfIndex>,
    meta: Option<&NodeMeta>,
) -> bool {
    match t.level {
        // The fleet default (ADR-075). It matches a node with no profile and no tags too —
        // which is the whole reason it exists, since a profile-scoped default cannot reach
        // one. `scope_ids` is unread here; the API pins it to the empty set.
        ScopeLevel::Global => true,
        // ADR-078: a rule names a SET of targets, so every level below asks "does any of them
        // match" rather than "does the one match". A set of one behaves exactly as before, which
        // is what keeps every pre-078 rule resolving identically.
        ScopeLevel::Node => {
            let want = node.to_string();
            t.scope_ids.contains(&want)
        }
        ScopeLevel::Profile => meta
            .and_then(|m| m.profile.as_deref())
            .is_some_and(|p| t.scope_ids.iter().any(|s| s == p)),
        // Tag values, deliberately — a `ScopeLevel::Group` threshold matches a node *tag*, not
        // the folder tree. See the `NodeMeta` docs for why the distinction is load-bearing.
        ScopeLevel::Group => {
            meta.is_some_and(|m| t.scope_ids.iter().any(|s| m.tag_groups.contains(s)))
        }
        // The folder tree (ADR-022), inherited downwards: the chain holds the node's own group
        // and every group above it. A rule on a parent therefore covers everything inside it,
        // the same way a maintenance window on a folder group does.
        ScopeLevel::FolderGroup => folder_depth(t, meta).is_some(),
        // One port of one node (ADR-076). Both halves must match, and a sample with no port
        // matches nothing here — an interface rule is never the fallback for a node check.
        ScopeLevel::Interface => ifindex.is_some_and(|idx| {
            t.scope_ids
                .iter()
                .any(|s| yagra_common::parse_interface_scope_id(s) == Some((node.as_uuid(), idx.0)))
        }),
    }
}

/// How far up the node's folder chain `t` sits — `0` is the node's own group. `None` when `t`
/// is not a folder-group rule, or names a group the node is not under (which `threshold_applies`
/// has already excluded, so in practice only the former).
pub(crate) fn folder_depth(t: &StoredThreshold, meta: Option<&NodeMeta>) -> Option<usize> {
    if t.level != ScopeLevel::FolderGroup {
        return None;
    }
    // The NEAREST of the folders this rule names (ADR-078). A rule naming both a site and the
    // rack inside it must count as the rack, or `nearest_folder_depth` would let a broader rule
    // at the same level win purely because it happened to list the outer folder too.
    let chain = &meta?.folder_chain;
    t.scope_ids
        .iter()
        .filter_map(|s| Uuid::parse_str(s).ok())
        .filter_map(|want| chain.iter().position(|g| *g == want))
        .min()
}

/// The smallest depth any matched folder-group rule sits at, or `None` when there are none.
pub(crate) fn nearest_folder_depth(
    matched: &[&StoredThreshold],
    meta: Option<&NodeMeta>,
) -> Option<usize> {
    matched.iter().filter_map(|t| folder_depth(t, meta)).min()
}

/// The rules that reach `(node, ifindex)`, each flagged with whether it is the one in force.
///
/// "In force" is **the level that wins**, resolved exactly as [`AlertConfig::resolve`] does it:
/// most specific level, and among folder-group rules only the nearest group in the chain
/// (ADR-013 + ADR-075 決定 11). Per metric, because precedence is per metric.
///
/// Several rules can be in force for one metric at once — `resolve_effective` merges rules at the
/// winning level by keeping the more restrictive bound of each severity. So this flag says "this
/// rule contributes", never "this rule *is* the effective bound"; the caller that shows it says so.
///
/// Takes the rules as an argument rather than reading a snapshot, because the API wants the
/// *current* ruleset — a rule created two seconds ago is not in the engine's snapshot yet, and a
/// list that omitted it would be a list that answers the question wrongly for the person who just
/// pressed Save.
pub(crate) fn matching_rules(
    rules: &[StoredThreshold],
    node: NodeId,
    ifindex: Option<IfIndex>,
    meta: Option<&NodeMeta>,
) -> Vec<(StoredThreshold, bool)> {
    let matched: Vec<&StoredThreshold> = rules
        .iter()
        .filter(|t| threshold_applies(t, node, ifindex, meta))
        .collect();
    // The winning level per metric, and the nearest folder depth among that metric's own matches.
    let mut winner: HashMap<&str, ScopeLevel> = HashMap::new();
    let mut nearest: HashMap<&str, Option<usize>> = HashMap::new();
    for t in &matched {
        let m = t.rule.metric.as_str();
        winner
            .entry(m)
            .and_modify(|w| *w = (*w).max(t.level))
            .or_insert(t.level);
        let d = folder_depth(t, meta);
        nearest
            .entry(m)
            .and_modify(|n| {
                *n = match (*n, d) {
                    (Some(a), Some(b)) => Some(a.min(b)),
                    (Some(a), None) => Some(a),
                    (None, b) => b,
                };
            })
            .or_insert(d);
    }
    matched
        .into_iter()
        .map(|t| {
            let m = t.rule.metric.as_str();
            let in_force = winner.get(m) == Some(&t.level)
                && (t.level != ScopeLevel::FolderGroup
                    || nearest.get(m).copied().flatten() == folder_depth(t, meta));
            (t.clone(), in_force)
        })
        .collect()
}

/// What the interface-threshold rules for one metric cover (ADR-076 decisions 3 and 10).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RuleCoverage {
    /// The smallest warning-or-critical bound any rule names, **in the metric's own unit** —
    /// percent for `if_*_util_pct`, bits per second for `if_*_bps`. `None` = no rule that can
    /// fire, so the evaluator runs no query at all.
    ///
    /// It was called `lowest_bound_pct` while percentages were the only derived metric; the
    /// absolute ones (ADR-076 決定 9) made that name a lie about half the callers.
    pub lowest_bound: Option<f64>,
    /// Whether any of those rules is a `below` rule.
    ///
    /// The candidate query selects the ports **at or above** a floor, which assumes every rule is
    /// breached from above. A `below` rule inverts that: the ports it is about are exactly the
    /// ones a floor removes. The evaluator drops the floor to zero when this is set — see
    /// `interface_util::query_floor_bps`, which is where the consequence is spelled out.
    pub has_below: bool,
    /// The nodes the rules name, or `None` when at least one rule is scoped too broadly to
    /// enumerate (global / profile / tag group / folder group) and the query must cover the fleet.
    pub nodes: Option<BTreeSet<Uuid>>,
}

/// Threshold evaluation context for one sample, used to describe *what* fired (metric, value,
/// crossed bound, direction) when a transition commits. Only present for threshold checks;
/// liveness up/down carries no numeric breach.
#[derive(Debug, Clone, Copy)]
pub(super) struct ThresholdEval {
    /// Observed sample value.
    pub(super) value: f64,
    /// The side this sample crossed — **not** the rule's primary side. See where this is built.
    pub(super) direction: Direction,
    /// That side's warning bound, if it has one.
    pub(super) warning: Option<f64>,
    /// That side's critical bound, if it has one.
    pub(super) critical: Option<f64>,
}

/// Everything identifying the check being evaluated for one sample: its stable id, the metric
/// it measures, the dwell, whether it's the liveness check, whether it may page, and (for
/// thresholds) the breach eval.
/// Bundled so [`AlertManager::process_check`] takes one descriptor instead of a long arg list.
pub(super) struct CheckSpec<'a> {
    pub(super) check: CheckId,
    pub(super) metric: &'a str,
    pub(super) dwell: u32,
    pub(super) is_liveness: bool,
    /// Whether a committed transition may raise an alert (ADR-075).
    ///
    /// A threshold check is always `true` — the rule's existence *is* the check. Liveness is
    /// `false` when no `__liveness__` rule resolves for the node, which means the state machine
    /// still runs (display state, down-set, dependency suppression) and nobody is paged.
    pub(super) alerting: bool,
    pub(super) eval: Option<ThresholdEval>,
    /// The port this check is about, for a per-interface metric (ADR-076). Descriptive only —
    /// identity already lives in `check`, which [`interface_check_id`] built from the same port.
    pub(super) ifindex: Option<IfIndex>,
}

/// Deterministic check id for a (node, check-name) pair, so the same logical check keeps a
/// stable dedup identity across restarts. Also used by the event pipeline (`events/engine.rs`)
/// with `event:<rule-id>` names, keeping event alerts in the same identity space.
pub(crate) fn check_id(node: NodeId, name: &str) -> CheckId {
    subject_check_id(&Subject::Node(node), name)
}

/// Deterministic check id for any alert subject.
///
/// The hashed string is `"{subject}:{name}"`, and [`Subject`]'s `Display` is what keeps the two
/// namespaces apart: a node renders as a bare UUID, a pool as `pool:<name>`.
//
// The discriminating prefix has to sit on the *subject* side, never the name side: `name` is
// operator-authored free text on the mute path (`mutes.check_name`) and is any metric a poller
// emits on the threshold path, so a prefix there would be forgeable. A `NodeId` renders as a bare
// hyphenated UUID and contains no `:`, so no node can impersonate a pool or vice versa.
pub(crate) fn subject_check_id(subject: &Subject, name: &str) -> CheckId {
    CheckId::from(Uuid::new_v5(
        &Uuid::NAMESPACE_OID,
        format!("{subject}:{name}").as_bytes(),
    ))
}

/// What one sample's threshold resolves against: its metric, and its port when the metric is
/// collected per interface.
pub(super) type ResolveKey<'a> = (&'a str, Option<IfIndex>);

/// Build a [`ResolveKey`]. The port is kept only when the **catalogue** says the metric is
/// per-interface — never merely because the sample carries an `ifindex`, which on a chassis is an
/// `entPhysicalIndex` or a CPU number rather than a port (ADR-011).
///
/// One function because the key is built twice per result — once to fill the memo under the config
/// lock, once to read it back — and two spellings that drifted would silently miss the memo and
/// resolve every sample again.
pub(super) fn resolve_key<'a>(
    metric: &'a str,
    ifindex: Option<IfIndex>,
    per_if_metrics: &[&str],
) -> ResolveKey<'a> {
    let port = per_if_metrics
        .contains(&metric)
        .then_some(ifindex)
        .flatten();
    (metric, port)
}

// The fold comparison that used to live here is now `EffectiveThreshold::is_worse`, beside the
// bounds it reads (ADR-081) — a band has no single "further into breach" to measure along, so the
// answer stopped being a function of one `Direction`.
//
// ⚠️ **Why any of this is direction-aware at all, kept here because the product carries the note:**
// `query_metrics` collapses an `entity` metric to its maximum, and its own response admits the
// consequence — for a metric where low is the fault (an optical receive level) the maximum is the
// *healthiest* series rather than the worst. Folding a `below` rule with `max` would report the
// coolest sensor, the fullest battery and the idlest CPU, and alert on none of them.

/// Deterministic check id for one **port's** metric on a node (ADR-076 decision 1).
///
/// Before this existed, every interface's sample of one metric advanced the *same* check: on a
/// 48-port switch a `below` rule saw 47 `Ok`s interleaved with one `Critical` every poll, so a
/// 3-sample dwell was never satisfied and the rule **never fired at all** while the flap detector
/// churned. The fix is to give each port its own identity, not to change what a node check is.
///
/// # Why the port goes on the *name* side, when [`subject_check_id`]'s doc forbids a prefix there
///
/// That rule is about *forgeable* prefixes: the name is operator-authored on the mute path and is
/// any metric a poller emits on the threshold path, so a caller-supplied `pool:`-style prefix could
/// impersonate another subject. `@` cannot do that. [`yagra_common::is_valid_metric_name`] admits
/// only `[a-zA-Z_:][a-zA-Z0-9_:]*`, and it is enforced at **both** edges a name can enter from —
/// the API (operator input) and the TSDB write path (poller-supplied sample names) — so no metric
/// name can ever contain `@`. The suffix is therefore unreachable from either input, and
/// `check_id(node, metric)` keeps returning exactly the bytes it always did: ADR-075 decision 2
/// hangs dependency-suppression selection and the PagerDuty/JSM dedup key off that value, and an
/// open external incident that stops matching is one nothing will ever close.
pub(crate) fn interface_check_id(node: NodeId, ifindex: IfIndex, metric: &str) -> CheckId {
    subject_check_id(&Subject::Node(node), &format!("{metric}@{ifindex}"))
}

/// Severity ordering over [`NodeState`] for rolling several states up to one headline.
/// Worse states rank higher; ties never matter (they map to the same display).
pub(super) fn severity_rank(state: NodeState) -> u8 {
    match state {
        NodeState::Critical => 5,
        NodeState::Unreachable => 4,
        NodeState::Warning => 3,
        NodeState::Unknown => 2,
        NodeState::Maintenance => 1,
        NodeState::Ok => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::super::testkit::*;
    use super::*;
    use yagra_common::ThresholdBounds;
    #[test]
    fn a_folder_group_rule_covers_that_group_and_every_group_inside_it() {
        // Both directions on purpose: a matcher that answered `false` for everything would pass a
        // test that only checked the unrelated group.
        let node = NodeId::new();
        let own = Uuid::from_u128(0xf001);
        let parent = Uuid::from_u128(0xf002);
        let unrelated = Uuid::from_u128(0xf003);
        let meta = in_folder(node, vec![own, parent]);

        let warn = |c: AlertConfig| c.resolve(node, None, "cpu_util").and_then(|e| e.warning());
        // The node's own group.
        assert_eq!(
            warn(cfg(vec![folder_rule(own, 10.0)], meta.clone())),
            Some(10.0)
        );
        // A group above it — inherited downwards, the way a maintenance window on a folder is.
        assert_eq!(
            warn(cfg(vec![folder_rule(parent, 20.0)], meta.clone())),
            Some(20.0)
        );
        // A group the node is not under.
        assert_eq!(warn(cfg(vec![folder_rule(unrelated, 30.0)], meta)), None);
        // And a node the snapshot has no metadata for is under no folder at all.
        let orphan = cfg(vec![folder_rule(own, 10.0)], HashMap::new());
        assert_eq!(
            orphan
                .resolve(node, None, "cpu_util")
                .and_then(|e| e.warning()),
            None
        );
    }

    #[test]
    fn the_nearest_folder_group_wins_even_when_its_bound_is_looser() {
        // ADR-013's first rule is most-specific-wins and the folder chain is ordered, so an inner
        // group's rule replaces its parent's rather than merging with it. The looser inner bound is
        // the whole point: "most restrictive wins" would answer 50 here and silently discard a
        // deliberate relaxation for one site.
        let node = NodeId::new();
        let own = Uuid::from_u128(0xf011);
        let parent = Uuid::from_u128(0xf012);
        let meta = in_folder(node, vec![own, parent]);
        let c = cfg(
            vec![folder_rule(parent, 50.0), folder_rule(own, 90.0)],
            meta.clone(),
        );
        assert_eq!(
            c.resolve(node, None, "cpu_util").and_then(|e| e.warning()),
            Some(90.0),
            "the node's own group must beat its parent"
        );

        // Two rules at the *same* depth have no order between them, so the old tie-break still
        // applies there: the most restrictive value wins (ADR-013).
        let c = cfg(
            vec![folder_rule(own, 90.0), folder_rule(own, 60.0)],
            meta.clone(),
        );
        assert_eq!(
            c.resolve(node, None, "cpu_util").and_then(|e| e.warning()),
            Some(60.0),
            "same depth ⇒ strictest wins"
        );

        // And a node-scoped rule still outranks every folder group, however near.
        let c = cfg(
            vec![
                folder_rule(own, 90.0),
                StoredThreshold::new(
                    Uuid::from_u128(0xf013),
                    ScopeLevel::Node,
                    vec![node.to_string()],
                    yagra_common::ThresholdRule::new(
                        "cpu_util",
                        yagra_common::ThresholdBounds::above(Some(99.0), None),
                        1,
                    ),
                ),
            ],
            meta,
        );
        assert_eq!(
            c.resolve(node, None, "cpu_util").and_then(|e| e.warning()),
            Some(99.0)
        );
    }

    /// A node check id must never move: it is the PagerDuty/JSM dedup key (ADR-075 decision 2).
    ///
    /// Pinned to literals rather than recomputed, because a test that derives the expected value
    /// the same way the code does would follow any change the code made and prove nothing. If this
    /// fails, every open external incident on every deployment stops matching and nothing will
    /// ever close it.
    #[test]
    fn a_node_check_id_is_unchanged_by_the_interface_split() {
        let node = NodeId::from(
            Uuid::parse_str("6f1c9d2a-0b3e-4a71-9c8d-2e5f7a1b4c60").expect("literal uuid"),
        );
        assert_eq!(
            check_id(node, LIVENESS).as_uuid().to_string(),
            "ef1b3ae6-8d6a-577f-b62c-a0182ee5872d".to_owned(),
            "the liveness check id moved — dependency suppression and every external dedup key \
             are derived from it"
        );
        assert_eq!(
            check_id(node, "icmp_rtt_ms").as_uuid().to_string(),
            "e85d51fd-4eb1-554b-b7c5-9de3491b5b45".to_owned(),
            "a threshold check id moved"
        );
    }

    /// The per-port id is distinct from the node id, and no metric name can be typed to forge one.
    #[test]
    fn an_interface_check_id_never_collides_with_a_node_check_id() {
        use yagra_common::IfIndex;

        let node = NodeId::new();
        // The port must actually change the identity — otherwise the whole increment is a no-op.
        assert_ne!(
            interface_check_id(node, IfIndex(0), "if_in_util_pct"),
            check_id(node, "if_in_util_pct"),
            "port 0 is a real port, not 'no port'"
        );
        assert_ne!(
            interface_check_id(node, IfIndex(7), "if_in_util_pct"),
            interface_check_id(node, IfIndex(8), "if_in_util_pct"),
        );
        assert_eq!(
            interface_check_id(node, IfIndex(7), "if_in_util_pct"),
            interface_check_id(node, IfIndex(7), "if_in_util_pct"),
            "the id must be stable across calls, or it survives no restart"
        );

        // ── The forgery question, stated exactly ──────────────────────────────────────────
        // `interface_check_id` hashes the same string `check_id` would for a metric *named*
        // `if_in_util_pct@7`, so as values these two ARE equal. That is not a collision, because
        // no such metric can exist: `is_valid_metric_name` admits only `[a-zA-Z_:][a-zA-Z0-9_:]*`
        // and is enforced at both edges a name enters from (the threshold API and the TSDB write
        // path). The safety property is therefore the *charset*, not the hash — so that is what
        // this pins. If someone ever widens the charset to admit `@`, this fails here rather than
        // silently merging one port's check with a node-level one.
        assert_eq!(
            interface_check_id(node, IfIndex(7), "if_in_util_pct"),
            check_id(node, "if_in_util_pct@7"),
            "the composed form is what is hashed — see the charset assertion below"
        );
        assert!(
            !yagra_common::is_valid_metric_name("if_in_util_pct@7"),
            "'@' must stay unspellable in a metric name, or the port suffix becomes forgeable"
        );
        assert!(yagra_common::is_valid_metric_name("if_in_util_pct"));
    }

    /// `matching_rules` is what the port's rule list is built from, so its two jobs are separate:
    /// which rules *reach* the port at all (across six scope levels), and which of those are in
    /// force. Getting the first wrong shows an empty list about a port that is alerting; getting
    /// the second wrong tells the operator the wrong rule is the one to edit.
    #[test]
    fn matching_rules_lists_every_level_and_marks_the_winner() {
        use yagra_common::{IfIndex, ThresholdRule};

        let node = NodeId::new();
        let other = NodeId::new();
        let profile = "cisco-switch";
        let parent = Uuid::new_v4();
        let child = Uuid::new_v4();

        let rule = |level: ScopeLevel, scope_id: String, metric: &str| {
            StoredThreshold::new(
                Uuid::new_v4(),
                level,
                vec![scope_id],
                ThresholdRule::new(metric, ThresholdBounds::above(None, Some(90.0)), 3),
            )
        };
        let rules = vec![
            rule(ScopeLevel::Global, String::new(), "if_in_util_pct"),
            rule(ScopeLevel::Profile, profile.to_owned(), "if_in_util_pct"),
            rule(
                ScopeLevel::FolderGroup,
                parent.to_string(),
                "if_in_util_pct",
            ),
            rule(ScopeLevel::FolderGroup, child.to_string(), "if_in_util_pct"),
            rule(ScopeLevel::Node, node.to_string(), "if_in_util_pct"),
            rule(
                ScopeLevel::Interface,
                yagra_common::interface_scope_id(node.as_uuid(), 7),
                "if_in_util_pct",
            ),
            // Another port of the same node, and another node entirely: neither reaches port 7.
            rule(
                ScopeLevel::Interface,
                yagra_common::interface_scope_id(node.as_uuid(), 8),
                "if_in_util_pct",
            ),
            rule(
                ScopeLevel::Interface,
                yagra_common::interface_scope_id(other.as_uuid(), 7),
                "if_in_util_pct",
            ),
            // A second metric, governed only from the node level — its winner is the node rule,
            // because precedence is per metric and not per port.
            rule(ScopeLevel::Node, node.to_string(), "if_out_util_pct"),
            rule(ScopeLevel::Global, String::new(), "if_out_util_pct"),
        ];
        let meta = NodeMeta {
            profile: Some(profile.to_owned()),
            folder_chain: vec![child, parent],
            ..NodeMeta::default()
        };

        let got = matching_rules(&rules, node, Some(IfIndex(7)), Some(&meta));
        // Six reach this port for the first metric, two for the second; the three rules aimed at
        // another port or another node reach nothing.
        assert_eq!(got.len(), 8, "{got:#?}");
        let in_force: Vec<(ScopeLevel, &str)> = got
            .iter()
            .filter(|(_, f)| *f)
            .map(|(t, _)| (t.level, t.rule.metric.as_str()))
            .collect();
        assert_eq!(
            in_force,
            vec![
                (ScopeLevel::Interface, "if_in_util_pct"),
                (ScopeLevel::Node, "if_out_util_pct"),
            ],
            "the port rule wins its metric; the other metric is still decided at the node"
        );
        // The nearer folder group is listed, and so is the grandparent — an operator has to be
        // able to see the rule that would take over if the near one were deleted — but only the
        // nearer one would be in force if no narrower level existed.
        let folders = matching_rules(
            &rules
                .iter()
                .filter(|t| t.level == ScopeLevel::FolderGroup)
                .cloned()
                .collect::<Vec<_>>(),
            node,
            Some(IfIndex(7)),
            Some(&meta),
        );
        assert_eq!(folders.len(), 2);
        let winners: Vec<&str> = folders
            .iter()
            .filter(|(_, f)| *f)
            .map(|(t, _)| t.scope_ids[0].as_str())
            .collect();
        assert_eq!(winners, vec![child.to_string().as_str()]);
    }

    /// A node the engine's snapshot has never seen — added seconds ago, or the snapshot has not
    /// refreshed yet. It must still get an honest answer rather than an empty one.
    #[test]
    fn a_node_with_no_metadata_still_matches_the_levels_that_do_not_need_it() {
        use yagra_common::{IfIndex, ThresholdRule};

        let node = NodeId::new();
        let rule = |level: ScopeLevel, scope_id: String| {
            StoredThreshold::new(
                Uuid::new_v4(),
                level,
                vec![scope_id],
                ThresholdRule::new(
                    "if_in_util_pct",
                    ThresholdBounds::above(None, Some(90.0)),
                    3,
                ),
            )
        };
        let rules = vec![
            rule(ScopeLevel::Global, String::new()),
            rule(ScopeLevel::Profile, "cisco-switch".to_owned()),
            rule(ScopeLevel::FolderGroup, Uuid::new_v4().to_string()),
            rule(ScopeLevel::Node, node.to_string()),
            rule(
                ScopeLevel::Interface,
                yagra_common::interface_scope_id(node.as_uuid(), 7),
            ),
        ];
        let got = matching_rules(&rules, node, Some(IfIndex(7)), None);
        // Global, node and interface need no metadata; profile and folder group do, and cannot
        // match without it.
        let levels: Vec<ScopeLevel> = got.iter().map(|(t, _)| t.level).collect();
        assert_eq!(
            levels,
            vec![ScopeLevel::Global, ScopeLevel::Node, ScopeLevel::Interface]
        );
        assert!(got.iter().filter(|(_, f)| *f).count() == 1);

        // And a node-wide question (no port) must not pick up the port rule — an interface rule is
        // never the fallback for a node check.
        let node_wide = matching_rules(&rules, node, None, None);
        assert!(node_wide
            .iter()
            .all(|(t, _)| t.level != ScopeLevel::Interface));
    }

    /// What the evaluator plans its query from. The three facts it needs are the lowest bound,
    /// whether any rule reads downwards, and whether the nodes can be enumerated at all.
    #[test]
    fn interface_rule_coverage_reports_the_lowest_bound_and_the_nodes() {
        use yagra_common::{ThresholdBounds, ThresholdRule};

        let a = NodeId::new();
        let b = NodeId::new();
        let rule = |level: ScopeLevel, scope_id: String, crit: Option<f64>, warn: Option<f64>| {
            StoredThreshold::new(
                Uuid::new_v4(),
                level,
                vec![scope_id],
                ThresholdRule::new("if_in_util_pct", ThresholdBounds::above(warn, crit), 3),
            )
        };
        let cov = AlertConfig::new(
            vec![
                rule(ScopeLevel::Node, a.to_string(), Some(90.0), None),
                // The warning bound is lower than either critical, and it is what trips first.
                rule(
                    ScopeLevel::Interface,
                    yagra_common::interface_scope_id(b.as_uuid(), 7),
                    Some(95.0),
                    Some(70.0),
                ),
            ],
            HashMap::new(),
        )
        .interface_rule_coverage("if_in_util_pct");
        assert_eq!(cov.lowest_bound, Some(70.0));
        assert!(!cov.has_below);
        assert_eq!(
            cov.nodes,
            Some([a.as_uuid(), b.as_uuid()].into_iter().collect())
        );

        // A metric nobody wrote a rule for asks for no query at all — spelled as an empty node
        // set rather than `None`, which would mean the whole fleet.
        let empty = AlertConfig::default().interface_rule_coverage("if_in_util_pct");
        assert_eq!(empty.lowest_bound, None);
        assert_eq!(empty.nodes, Some(BTreeSet::new()));

        // One broadly-scoped rule collapses the node list: it cannot be enumerated without a
        // fleet walk, and its author meant it that way.
        let wide = AlertConfig::new(
            vec![
                rule(ScopeLevel::Node, a.to_string(), Some(90.0), None),
                rule(ScopeLevel::Global, String::new(), Some(80.0), None),
            ],
            HashMap::new(),
        )
        .interface_rule_coverage("if_in_util_pct");
        assert_eq!(wide.lowest_bound, Some(80.0));
        assert_eq!(wide.nodes, None);
    }

    /// The `below` flag exists so the evaluator can drop its floor. Before it, a `below` rule on a
    /// port metric never fired: the candidate query returns the ports **above** a floor, so the
    /// quiet ports such a rule is about were never evaluated at all.
    #[test]
    fn a_below_rule_is_reported_so_the_floor_can_be_dropped() {
        use yagra_common::{Direction, ThresholdRule};

        let node = NodeId::new();
        let with = |direction: Direction, crit: Option<f64>| {
            AlertConfig::new(
                vec![StoredThreshold::new(
                    Uuid::new_v4(),
                    ScopeLevel::Node,
                    vec![node.to_string()],
                    ThresholdRule::new(
                        "if_in_bps",
                        ThresholdBounds::from_legacy(direction, None, crit),
                        3,
                    ),
                )],
                HashMap::new(),
            )
            .interface_rule_coverage("if_in_bps")
        };
        assert!(with(Direction::Below, Some(1_000_000.0)).has_below);
        assert!(!with(Direction::Above, Some(1_000_000.0)).has_below);
        // A rule with neither bound cannot fire, so it must not widen the query either — it would
        // otherwise pin every port of that direction to the floorless path forever.
        let inert = with(Direction::Below, None);
        assert!(!inert.has_below);
        assert_eq!(inert.lowest_bound, None);
    }

    /// Every check id in the field was derived from `"{node-uuid}:{name}"`. Introducing `Subject`
    /// must not have moved a single one, or every active alert would re-fire as a new identity and
    /// every open PagerDuty/JSM incident would be orphaned on the next resolve.
    #[test]
    fn a_node_check_id_is_unchanged_by_the_subject_split() {
        let node = NodeId::from(Uuid::from_u128(0x5eed));
        for name in [LIVENESS, "icmp_rtt_ms", "event:abc"] {
            let expected = CheckId::from(Uuid::new_v5(
                &Uuid::NAMESPACE_OID,
                format!("{node}:{name}").as_bytes(),
            ));
            assert_eq!(check_id(node, name), expected, "check id moved for {name}");
            assert_eq!(subject_check_id(&Subject::Node(node), name), expected);
        }
    }

    #[test]
    fn a_pool_can_never_collide_with_a_node_in_the_check_namespace() {
        // A pool name is operator free text, so it may be spelled exactly like a node id. The
        // `pool:` prefix sits on the subject, which is why that cannot forge a node's identity.
        let node = NodeId::new();
        assert_ne!(
            subject_check_id(&Subject::Pool(node.to_string()), LIVENESS),
            subject_check_id(&Subject::Node(node), LIVENESS),
        );
    }

    /// ADR-078, the case the feature exists for: one rule covering the two Cisco IOS profiles.
    ///
    /// Both halves matter. Without the accepting side a resolver that matched nothing would pass
    /// the rejection; without the rejecting side one that matched every profile would pass the
    /// acceptance (`rejection-only tests pass when everything rejects`).
    #[test]
    fn a_profile_rule_naming_two_profiles_resolves_for_both_and_not_for_a_third() {
        let ios_router = NodeId::from(Uuid::new_v4());
        let catalyst = NodeId::from(Uuid::new_v4());
        let juniper = NodeId::from(Uuid::new_v4());
        let rule = rule_at_many(
            ScopeLevel::Profile,
            &[
                "Cisco IOS/IOS-XE router",
                "Cisco Catalyst switch (IOS/IOS-XE)",
            ],
            Direction::Above,
            90.0,
        );
        let meta = |p: &str| NodeMeta {
            profile: Some(p.to_string()),
            ..NodeMeta::default()
        };
        let mut node_meta = HashMap::new();
        node_meta.insert(ios_router, meta("Cisco IOS/IOS-XE router"));
        node_meta.insert(catalyst, meta("Cisco Catalyst switch (IOS/IOS-XE)"));
        node_meta.insert(juniper, meta("Juniper MX router"));
        let config = AlertConfig::new(vec![rule], node_meta);
        for who in [ios_router, catalyst] {
            assert_eq!(
                config
                    .resolve(who, None, "cpu_util")
                    .and_then(|e| e.critical()),
                Some(90.0),
                "a named profile must resolve"
            );
        }
        assert!(
            config.resolve(juniper, None, "cpu_util").is_none(),
            "a profile the rule does not name must resolve to nothing"
        );
    }

    /// A rule naming an outer folder AND the folder inside it is judged by the inner one.
    ///
    /// The two rules here sit at the same `ScopeLevel`, so `resolve_effective` would merge them
    /// most-restrictively (`above` keeps the lower bound, 70) if they were read as the same depth.
    /// Only the nearest depth survives, so reading the pair as its INNER folder makes it override
    /// outright — which is the answer an operator who scoped a rule to the rack expects.
    #[test]
    fn a_folder_rule_naming_an_outer_and_an_inner_group_is_judged_by_the_inner_one() {
        let node = NodeId::from(Uuid::new_v4());
        let outer = Uuid::new_v4();
        let inner = Uuid::new_v4();
        let broad = rule_at(
            ScopeLevel::FolderGroup,
            &outer.to_string(),
            Direction::Above,
            70.0,
        );
        let pair = rule_at_many(
            ScopeLevel::FolderGroup,
            &[&outer.to_string(), &inner.to_string()],
            Direction::Above,
            95.0,
        );
        let mut node_meta = HashMap::new();
        node_meta.insert(
            node,
            NodeMeta {
                folder_group: Some(inner),
                folder_chain: vec![inner, outer],
                ..NodeMeta::default()
            },
        );
        let config = AlertConfig::new(vec![broad, pair], node_meta);
        assert_eq!(
            config
                .resolve(node, None, "cpu_util")
                .and_then(|e| e.critical()),
            Some(95.0),
            "the pair-naming rule sits at the inner folder, so it overrides rather than merging"
        );
    }

    /// The same rule expressed as one target behaves exactly as it did before ADR-078.
    ///
    /// The regression that matters: every rule already in a deployment is a set of one, so if the
    /// set-shaped predicate answered differently for those, the upgrade would silently re-scope
    /// the whole existing ruleset.
    #[test]
    fn a_single_target_rule_resolves_exactly_as_it_did_before() {
        let node = NodeId::from(Uuid::new_v4());
        let other = NodeId::from(Uuid::new_v4());
        let rule = rule_at(ScopeLevel::Node, &node.to_string(), Direction::Above, 42.0);
        let config = AlertConfig::new(vec![rule], HashMap::new());
        assert_eq!(
            config
                .resolve(node, None, "cpu_util")
                .and_then(|e| e.critical()),
            Some(42.0)
        );
        assert!(config.resolve(other, None, "cpu_util").is_none());
    }

    #[test]
    fn the_indexed_resolve_agrees_with_the_reference_implementation() {
        // The corpus is hand-built rather than random so each row names the case it covers, and a
        // failure says which one broke. Every row is a rule on the SAME metric, because that is
        // the bucket the index splits.
        let node = NodeId::from(Uuid::new_v4());
        let other = NodeId::from(Uuid::new_v4());
        let parent = Uuid::new_v4();
        let child = Uuid::new_v4();

        let rules = vec![
            rule_at(ScopeLevel::Global, "", Direction::Above, 90.0),
            rule_at(ScopeLevel::Profile, "switch", Direction::Above, 85.0),
            rule_at(ScopeLevel::Group, "prod", Direction::Above, 80.0),
            // Two folder-group rules at different depths — only the nearest may survive.
            rule_at(
                ScopeLevel::FolderGroup,
                &parent.to_string(),
                Direction::Above,
                75.0,
            ),
            rule_at(
                ScopeLevel::FolderGroup,
                &child.to_string(),
                Direction::Above,
                70.0,
            ),
            // This node, and another one that must never be picked up.
            rule_at(ScopeLevel::Node, &node.to_string(), Direction::Above, 65.0),
            rule_at(ScopeLevel::Node, &other.to_string(), Direction::Above, 10.0),
            // 🚨 Two rules at the SAME level with OPPOSITE directions. `resolve_effective` takes
            // `direction` from the first, so this is the row that fails if `seq` is dropped.
            rule_at(ScopeLevel::Node, &node.to_string(), Direction::Below, 5.0),
            // A node scope_id that is a real UUID but not the canonical spelling — matched nothing
            // before the index and must still match nothing.
            rule_at(
                ScopeLevel::Node,
                &format!("{{{}}}", node.as_uuid()),
                Direction::Above,
                1.0,
            ),
            rule_at(
                ScopeLevel::Node,
                &node.to_string().to_uppercase(),
                Direction::Above,
                2.0,
            ),
            // Ports: this node's port 7, this node's port 8, another node's port 7.
            rule_at(
                ScopeLevel::Interface,
                &format!("{}:7", node.as_uuid()),
                Direction::Above,
                60.0,
            ),
            rule_at(
                ScopeLevel::Interface,
                &format!("{}:8", node.as_uuid()),
                Direction::Above,
                55.0,
            ),
            rule_at(
                ScopeLevel::Interface,
                &format!("{}:7", other.as_uuid()),
                Direction::Above,
                3.0,
            ),
            // Unparseable port scope ids — `+7` is the one `parse_interface_scope_id` refuses.
            rule_at(
                ScopeLevel::Interface,
                &format!("{}:+7", node.as_uuid()),
                Direction::Above,
                4.0,
            ),
            rule_at(
                ScopeLevel::Interface,
                "not-a-scope-id",
                Direction::Above,
                6.0,
            ),
            // ── ADR-078: rules naming several targets at once ──────────────────────────
            // Both nodes in one rule — it must be findable under BOTH lookup keys.
            rule_at_many(
                ScopeLevel::Node,
                &[&node.to_string(), &other.to_string()],
                Direction::Above,
                64.0,
            ),
            // One good target and one that parses to nothing. The whole rule must fall to the
            // linear scan: filing half of it would make a lookup answer with a subset.
            rule_at_many(
                ScopeLevel::Node,
                &[&node.to_string(), "not-a-uuid"],
                Direction::Above,
                63.0,
            ),
            // Two ports of the same node, one of which nothing else in the corpus names.
            rule_at_many(
                ScopeLevel::Interface,
                &[
                    &format!("{}:7", node.as_uuid()),
                    &format!("{}:9", node.as_uuid()),
                ],
                Direction::Above,
                59.0,
            ),
            // Two profiles — the row the whole feature exists for.
            rule_at_many(
                ScopeLevel::Profile,
                &["switch", "router"],
                Direction::Above,
                84.0,
            ),
            // An outer folder and an inner one in ONE rule: `folder_depth` must report the
            // inner one, or this rule would beat a nearer rule purely by listing the outer.
            rule_at_many(
                ScopeLevel::FolderGroup,
                &[&parent.to_string(), &child.to_string()],
                Direction::Above,
                69.0,
            ),
        ];

        let deep = NodeMeta {
            profile: Some("switch".to_string()),
            tag_groups: ["prod".to_string()].into_iter().collect(),
            folder_group: Some(child),
            folder_chain: vec![child, parent],
        };
        let shallow = NodeMeta {
            profile: Some("router".to_string()),
            folder_chain: vec![parent],
            ..NodeMeta::default()
        };

        // Every combination of "which rules are loaded" × "who is asking" × "about which port".
        let subsets: Vec<Vec<StoredThreshold>> = vec![
            Vec::new(),
            rules.clone(),
            rules.iter().skip(1).cloned().collect(),
            rules
                .iter()
                .filter(|t| t.level == ScopeLevel::Interface)
                .cloned()
                .collect(),
            rules
                .iter()
                .filter(|t| t.level == ScopeLevel::Node)
                .cloned()
                .collect(),
            rules.iter().rev().cloned().collect(),
        ];
        let metas: Vec<(&str, Option<NodeMeta>)> = vec![
            ("deep", Some(deep)),
            ("shallow", Some(shallow)),
            ("bare", Some(NodeMeta::default())),
            // A node the config has never heard of — `node_meta.get` returns None.
            ("absent", None),
        ];
        let ports = [None, Some(IfIndex(7)), Some(IfIndex(8)), Some(IfIndex(9))];

        let mut compared = 0usize;
        let mut answered = 0usize;
        for subset in &subsets {
            for (meta_name, meta) in &metas {
                let mut node_meta = HashMap::new();
                if let Some(m) = meta {
                    node_meta.insert(node, m.clone());
                }
                let config = AlertConfig::new(subset.clone(), node_meta);
                for who in [node, other] {
                    for port in ports {
                        let want = resolve_reference(subset, who, port, config.node_meta.get(&who));
                        let got = config.resolve(who, port, "cpu_util");
                        assert_eq!(
                            got,
                            want,
                            "indexed resolve disagreed: subset of {} rules, meta={meta_name}, \
                             port={port:?}, node={}",
                            subset.len(),
                            if who == node {
                                "the one with rules"
                            } else {
                                "the other"
                            }
                        );
                        compared += 1;
                        if got.is_some() {
                            answered += 1;
                        }
                    }
                }
            }
        }
        // 🚨 The counts are the load-bearing half. A differential test where BOTH sides return
        // `None` everywhere agrees perfectly and proves nothing — the same trap as a suite made
        // only of rejection cases.
        assert!(
            compared > 150,
            "only {compared} comparisons — the corpus shrank"
        );
        assert!(
            answered > 40,
            "only {answered} of {compared} comparisons resolved to a rule; a corpus that never \
             matches would agree no matter what the index did"
        );
    }

    #[test]
    fn the_index_files_each_rule_where_a_lookup_can_find_it() {
        let node = NodeId::from(Uuid::new_v4());
        let group = Uuid::new_v4();
        let config = AlertConfig::new(
            vec![
                rule_at(ScopeLevel::Global, "", Direction::Above, 1.0),
                rule_at(ScopeLevel::Profile, "switch", Direction::Above, 2.0),
                rule_at(ScopeLevel::Group, "prod", Direction::Above, 3.0),
                rule_at(
                    ScopeLevel::FolderGroup,
                    &group.to_string(),
                    Direction::Above,
                    4.0,
                ),
                rule_at(ScopeLevel::Node, &node.to_string(), Direction::Above, 5.0),
                rule_at(
                    ScopeLevel::Interface,
                    &format!("{}:7", node.as_uuid()),
                    Direction::Above,
                    6.0,
                ),
            ],
            HashMap::new(),
        );
        let m = config.by_metric.get("cpu_util").expect("the metric bucket");
        // The four levels that cannot be reduced to a key are the only ones scanned linearly.
        assert_eq!(
            m.broad.len(),
            4,
            "broad should hold exactly global/profile/group/folder"
        );
        assert_eq!(m.by_node.len(), 1);
        assert!(m.by_node.contains_key(&node.as_uuid()));
        assert_eq!(m.by_port.len(), 1);
        assert!(m.by_port.contains_key(&(node.as_uuid(), 7)));
        // `all()` must still see everything, in the original order.
        assert_eq!(m.all().len(), 6);
        let bounds: Vec<Option<f64>> = m.all().iter().map(|t| t.rule.critical()).collect();
        assert_eq!(
            bounds,
            vec![
                Some(1.0),
                Some(2.0),
                Some(3.0),
                Some(4.0),
                Some(5.0),
                Some(6.0)
            ],
            "all() must return the config's original order, not the bucket order"
        );
    }

    #[test]
    fn a_scope_id_the_keys_cannot_be_built_from_stays_in_the_broad_bucket() {
        // The equivalence the index rests on: a rule the lookup key cannot represent must fall
        // back to `threshold_applies`, not be dropped and not be promoted. Both of these matched
        // nothing before ADR-076 increment 6, and must still match nothing.
        let node = NodeId::from(Uuid::new_v4());
        let config = AlertConfig::new(
            vec![
                rule_at(
                    ScopeLevel::Node,
                    &format!("{{{}}}", node.as_uuid()),
                    Direction::Above,
                    1.0,
                ),
                rule_at(
                    ScopeLevel::Interface,
                    &format!("{}:+7", node.as_uuid()),
                    Direction::Above,
                    2.0,
                ),
            ],
            HashMap::new(),
        );
        let m = config.by_metric.get("cpu_util").expect("the metric bucket");
        assert_eq!(m.broad.len(), 2, "neither id can key a bucket");
        assert!(m.by_node.is_empty());
        assert!(m.by_port.is_empty());
        // And they still resolve to nothing, which is what they did before.
        assert!(config.resolve(node, None, "cpu_util").is_none());
        assert!(config.resolve(node, Some(IfIndex(7)), "cpu_util").is_none());
    }
}
