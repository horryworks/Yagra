// SPDX-License-Identifier: AGPL-3.0-only
//! Alert engine wiring (Workstream B).
//!
//! Drives the tested [`yagra_alert`] state machine from live poll results: each node has
//! one ICMP-liveness check whose raw state (reachable→ok, unreachable→unreachable,
//! error→unknown) is fed through dwell-time **hysteresis** + **flapping** detection
//! ([`CheckState`]). A committed transition into a problem state fires an [`Alert`];
//! recovery resolves it. Active alerts are held in memory and broadcast to SSE
//! subscribers; transitions are forwarded to a [`Notifier`] (Webhook) with the engine's
//! dedup + retry.
//!
//! More quality features are wired here on top of liveness:
//! - **Threshold alerting** — each poll sample with a resolved [`EffectiveThreshold`]
//!   (scope inheritance via [`AlertConfig`]) is evaluated and fed through the same
//!   hysteresis/flapping machinery as liveness.
//! - **Dependency suppression** — a node's committed liveness is tracked per node; when a
//!   node goes down and *every* upstream is also down (per the [`Topology`]), its alert is
//!   attributed to the highest down ancestor (`root_cause`) and the downstream
//!   notification is suppressed (rolled up into the parent incident, ADR-015). The alert
//!   still fires for the UI/history — only the duplicate page is suppressed.
//! - **Maintenance windows** — nodes covered by an active window (snapshot in
//!   [`AlertConfig`]) observe `Maintenance` instead of their real state, so no alert can
//!   fire during the window and existing alerts resolve (after the usual dwell). When the
//!   window ends the real state flows again and surviving problems re-commit.
//! - **Mutes** — the [`Notifier`] skips delivery for alerts matching an unexpired mute
//!   (one node, optionally one check). The alert still fires for the UI/history — a mute
//!   only silences the page.

use std::collections::{BTreeSet, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use async_trait::async_trait;
use tokio::sync::broadcast;
use uuid::Uuid;
use yagra_alert::CheckState;
use yagra_alert::{
    Alert, Breach, Dispatcher, Notification, NotifyChannel, NotifyError, RetryPolicy, Subject,
};
use yagra_bus::{CheckOutcome, PollResult, Sample};
use yagra_common::{
    is_ssrf_blocked, resolve_effective, CheckId, Direction, EffectiveThreshold, IfIndex,
    MetricKind, NodeId, NodeState, ScopeLevel, ScopedThreshold, Severity,
};
use yagra_topology::Topology;

use crate::notifications::{ChannelConfig, OpenChannel, RoutingRule};
use crate::notify_facts::{context_for, node_ids_for, AlertFactsSource};
use crate::notify_render::{body_must_be_json, render_with_fallback, ChannelTemplate};
use crate::thresholds::StoredThreshold;
use yagra_common::{AlertFacts, NotifyEvent};

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

/// Flapping detection window and threshold.
const FLAP_WINDOW_MS: i64 = 600_000;
const FLAP_THRESHOLD: usize = 5;
/// One SSE frame: the subject it concerns, beside the already-serialized JSON body.
///
/// The subject travels *alongside* the payload rather than being parsed back out of it, because the
/// only consumer that needs it is the group-scope filter on the stream handler (ADR-014) and
/// deserializing every frame per subscriber to recover a field the sender already had would be pure
/// waste. The body stays shared rather than owned: `broadcast` clones the value once **per
/// receiver**, and a full sweep can emit one node-state frame per node, so with many dashboards open
/// this is the difference between cloning a pointer and cloning the JSON N times.
///
/// It is a [`Subject`] rather than a `NodeId` so a pool-coverage alert can be streamed at all. The
/// node-state channel only ever carries `Subject::Node` — a rolled-up display state belongs to a
/// node by definition — and shares the type so both streams go through one scope filter rather
/// than two copies of it.
pub type StreamFrame = (Subject, Arc<str>);

/// SSE broadcast buffer. Sized generously so a briefly-slow subscriber doesn't lag past the
/// window and miss events; if one does lag, the stream handler logs it and emits a `resync`
/// hint so the client can re-fetch the active-alert list (see `stream_alerts` in api.rs).
const EVENT_BUFFER: usize = 1024;
/// Node-state SSE buffer (S14). Larger than the alert buffer because a full poll sweep can emit
/// one state event per node (first observation plus genuine transitions). A subscriber that
/// overflows this gets a `resync` hint and re-seeds from REST, so the bound is a soft backstop.
const NODE_EVENT_BUFFER: usize = 4096;

/// What the manager wants done about a committed transition.
#[derive(Debug, Clone)]
pub enum NotifyAction {
    /// A new problem alert fired.
    Fire(Alert),
    /// An alert recovered (carries the previously-active alert so it can be logged and its
    /// dedup state cleared).
    Resolve(Alert),
    /// A still-active downstream alert was rolled up under a newly-down upstream (event-driven
    /// dependency suppression). It had been paging standalone, so its remote incident must be
    /// **closed** — but unlike [`Self::Resolve`] the node has *not* recovered; it stays live in
    /// the UI, now grouped under its root cause. Carries the alert with its new `root_cause` set.
    Suppress(Alert),
}

/// Per-node metadata used to resolve threshold scope, and to answer "may this caller see this
/// node" without a database round-trip (`api/scope.rs`).
///
/// ⚠️ **The two group fields are different concepts and must not be confused.** [`Self::tag_groups`]
/// holds *tag values* — free-form labels an operator puts on a node, which `ScopeLevel::Group`
/// thresholds match against. [`Self::folder_group`] is the node's row in the inventory folder tree,
/// which is what RBAC visibility is defined over (ADR-014).
///
/// The field was called `groups` until group scoping landed, and the collision was a live hazard:
/// `Scope::allows` takes a `BTreeSet<String>`, so `principal.can_see(&meta.groups)` compiled, ran,
/// and would have scoped visibility by *threshold tags* — failing **open** for any node whose tags
/// happened to match, with nothing to catch it. Hence the rename and
/// `node_meta_group_is_the_folder_group_not_a_tag_value`.
#[derive(Debug, Clone, Default)]
pub struct NodeMeta {
    /// Profile id (as text) the node belongs to, if any.
    pub profile: Option<String>,
    /// Node **tag values**, for group-scoped thresholds. Not the folder tree — see the type docs.
    pub tag_groups: BTreeSet<String>,
    /// The node's folder group (`nodes.group_id`), for RBAC visibility. `None` = ungrouped, which
    /// a scoped principal may **not** see.
    pub folder_group: Option<Uuid>,
    /// The node's folder group and every group above it, **nearest first** — the chain a
    /// `ScopeLevel::FolderGroup` threshold is matched against (ADR-075 増分 3).
    ///
    /// Ordered, not a set, because the position *is* the specificity: a rule on the node's own
    /// group must beat one on its grandparent. `folder_group` stays separate and stays first here;
    /// RBAC is defined over the node's own group only, and widening it to the chain would let a
    /// principal scoped to a parent see a child it was not granted.
    pub folder_chain: Vec<Uuid>,
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
    by_metric: HashMap<String, MetricRules>,
    node_meta: HashMap<NodeId, NodeMeta>,
    topology: Topology,
    /// Nodes currently inside an active maintenance window (resolved at refresh time).
    maintenance: BTreeSet<NodeId>,
    /// Folder groups that have at least one node in each poller pool — what makes a pool-coverage
    /// alert visible to the group-scoped operator whose site went dark (`api/scope.rs`).
    ///
    /// Precomputed here rather than resolved per request because the answer needs each node's
    /// **effective** pool (own > nearest ancestor folder > default, `poolres.rs`), and doing that
    /// per SSE frame per subscriber would be a full-fleet walk on the hottest read path. The map is
    /// small — pools × groups — and is rebuilt with the rest of the snapshot, i.e. only when the
    /// config generation advances (S6/ADR-026). Ungrouped nodes contribute nothing: a scoped caller
    /// cannot see them anyway, so a pool that only holds ungrouped nodes stays admin-only.
    pool_groups: HashMap<String, BTreeSet<Uuid>>,
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
    per_interface: BTreeSet<String>,
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
struct MetricRules {
    /// Rules whose scope cannot be reduced to a lookup key: `Global`, `Profile`, `Group` and
    /// `FolderGroup` — plus any `Node`/`Interface` rule any of whose targets is not spelled the way the
    /// keys below are built, so that `threshold_applies` keeps the last word on those.
    ///
    /// Small by construction: an operator writes these by hand, one per fleet / profile / tag /
    /// folder. This is the only bucket a resolve still scans linearly.
    broad: Vec<Indexed>,
    /// `ScopeLevel::Node` rules, keyed by the node they name.
    by_node: HashMap<Uuid, Vec<Indexed>>,
    /// `ScopeLevel::Interface` rules, keyed by the exact port they name.
    by_port: HashMap<(Uuid, u32), Vec<Indexed>>,
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
struct Indexed {
    seq: u32,
    t: StoredThreshold,
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
    fn resolve(
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
struct ThresholdEval {
    /// Observed sample value.
    value: f64,
    /// Breach direction of the rule.
    direction: Direction,
    /// Effective warning bound, if any.
    warning: Option<f64>,
    /// Effective critical bound, if any.
    critical: Option<f64>,
}

/// Everything identifying the check being evaluated for one sample: its stable id, the metric
/// it measures, the dwell, whether it's the liveness check, whether it may page, and (for
/// thresholds) the breach eval.
/// Bundled so [`AlertManager::process_check`] takes one descriptor instead of a long arg list.
struct CheckSpec<'a> {
    check: CheckId,
    metric: &'a str,
    dwell: u32,
    is_liveness: bool,
    /// Whether a committed transition may raise an alert (ADR-075).
    ///
    /// A threshold check is always `true` — the rule's existence *is* the check. Liveness is
    /// `false` when no `__liveness__` rule resolves for the node, which means the state machine
    /// still runs (display state, down-set, dependency suppression) and nobody is paged.
    alerting: bool,
    eval: Option<ThresholdEval>,
    /// The port this check is about, for a per-interface metric (ADR-076). Descriptive only —
    /// identity already lives in `check`, which [`interface_check_id`] built from the same port.
    ifindex: Option<IfIndex>,
}

/// Deterministic check id for a (node, check-name) pair, so the same logical check keeps a
/// stable dedup identity across restarts. Also used by the event pipeline (`events.rs`)
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
type ResolveKey<'a> = (&'a str, Option<IfIndex>);

/// Build a [`ResolveKey`]. The port is kept only when the **catalogue** says the metric is
/// per-interface — never merely because the sample carries an `ifindex`, which on a chassis is an
/// `entPhysicalIndex` or a CPU number rather than a port (ADR-011).
///
/// One function because the key is built twice per result — once to fill the memo under the config
/// lock, once to read it back — and two spellings that drifted would silently miss the memo and
/// resolve every sample again.
fn resolve_key<'a>(
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
fn severity_rank(state: NodeState) -> u8 {
    match state {
        NodeState::Critical => 5,
        NodeState::Unreachable => 4,
        NodeState::Warning => 3,
        NodeState::Unknown => 2,
        NodeState::Maintenance => 1,
        NodeState::Ok => 0,
    }
}

/// In-memory alert engine: per-check state, active alerts, an SSE broadcast, the committed
/// per-node liveness map (inventory roll-up + suppression down-set), and the
/// threshold/metadata/topology config snapshot.
pub struct AlertManager {
    states: Mutex<HashMap<CheckId, CheckState>>,
    active: Mutex<HashMap<CheckId, Alert>>,
    /// Committed liveness state per node — the source of truth for the inventory's display
    /// state and for the suppression down-set. Updated on every liveness observation.
    live: Mutex<HashMap<NodeId, NodeState>>,
    /// The suppression down-set, maintained **incrementally** (== `{n : live[n] == Unreachable}`).
    /// Kept in sync with `live` on every liveness flip so `down_set()` is O(down) rather than a full
    /// O(live) scan on each transition — the hot path during a parent-down cascade (S3).
    down: Mutex<BTreeSet<NodeId>>,
    tx: broadcast::Sender<StreamFrame>,
    /// Incremental node-state (rolled-up display state) change stream for the WebUI (S14) — a
    /// dedicated channel so the inventory/topology views patch one node live instead of re-fetching
    /// the whole fleet every 15s. Kept separate from `tx` so the two event schemas don't mix.
    node_tx: broadcast::Sender<StreamFrame>,
    config: RwLock<AlertConfig>,
}

impl AlertManager {
    /// New manager with an empty config (no thresholds until [`Self::set_config`]).
    #[must_use]
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(EVENT_BUFFER);
        let (node_tx, _) = broadcast::channel(NODE_EVENT_BUFFER);
        Self {
            states: Mutex::new(HashMap::new()),
            active: Mutex::new(HashMap::new()),
            live: Mutex::new(HashMap::new()),
            down: Mutex::new(BTreeSet::new()),
            tx,
            node_tx,
            config: RwLock::new(AlertConfig::default()),
        }
    }

    /// Replace the threshold/metadata snapshot (called by the periodic refresh task).
    pub fn set_config(&self, config: AlertConfig) {
        *self.config.write().expect("config rwlock poisoned") = config;
    }

    /// Subscribe to the live alert event stream ([`StreamFrame`]s; `resolved` flag included in the
    /// JSON body).
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<StreamFrame> {
        self.tx.subscribe()
    }

    /// Subscribe to the incremental node-state stream (S14): JSON `{node_id, state, at_unix_ms}`
    /// per rolled-up display-state change, for the inventory/topology live-patch views.
    #[must_use]
    pub fn subscribe_node_states(&self) -> broadcast::Receiver<StreamFrame> {
        self.node_tx.subscribe()
    }

    /// Emit one incremental node-state change to the node-state SSE subscribers. Fire-and-forget —
    /// no subscribers is not an error.
    fn broadcast_node_state(&self, node: NodeId, state: NodeState, at_unix_ms: i64) {
        let event = serde_json::json!({
            "node_id": node.as_uuid(),
            "state": state,
            "at_unix_ms": at_unix_ms,
        });
        let _ = self
            .node_tx
            .send((Subject::Node(node), Arc::from(event.to_string())));
    }

    /// Snapshot of currently active alerts.
    #[must_use]
    pub fn active_alerts(&self) -> Vec<Alert> {
        self.active
            .lock()
            .expect("alerts mutex poisoned")
            .values()
            .cloned()
            .collect()
    }

    /// The rolled-up display state per known node: the worst of its committed liveness
    /// state and any active alert on it (so a reachable node breaching a threshold reads
    /// `warning`/`critical`, not `ok`). Nodes the engine has never observed are absent —
    /// the caller maps those to `unknown` (or a store-derived fallback).
    ///
    /// Alerts whose subject is not a node (pool-coverage alerts) are skipped: they belong to no
    /// node's display state.
    #[must_use]
    pub fn node_states(&self) -> HashMap<NodeId, NodeState> {
        let mut out = self.live.lock().expect("live mutex poisoned").clone();
        for alert in self.active.lock().expect("alerts mutex poisoned").values() {
            let Some(node) = alert.node() else { continue };
            out.entry(node)
                .and_modify(|s| {
                    if severity_rank(alert.state) > severity_rank(*s) {
                        *s = alert.state;
                    }
                })
                .or_insert(alert.state);
        }
        out
    }

    /// The rolled-up display state for one node, if the engine has observed it. Resolves the one
    /// node directly (its committed liveness rolled up with any active alert on it) instead of
    /// cloning the whole fleet's state map just to index one entry (S17) — the node-detail endpoint
    /// calls this per request, so at fleet scale the clone was pure waste.
    #[must_use]
    pub fn node_state(&self, node: NodeId) -> Option<NodeState> {
        let base = self
            .live
            .lock()
            .expect("live mutex poisoned")
            .get(&node)
            .copied();
        self.active
            .lock()
            .expect("alerts mutex poisoned")
            .values()
            .filter(|a| a.subject.is_node(node))
            .fold(base, |acc, alert| match acc {
                Some(s) if severity_rank(s) >= severity_rank(alert.state) => Some(s),
                _ => Some(alert.state),
            })
    }

    /// Count of **observed** nodes by rolled-up display state (same rollup as [`Self::node_states`]) —
    /// the fleet-summary source, so the dashboard's status/health/down numbers are computed over the
    /// whole fleet server-side, not a paged slice (S12). Never-observed nodes are absent; the caller
    /// adds `total_inventory − observed` as `Unknown`.
    #[must_use]
    pub fn node_state_counts(&self) -> HashMap<NodeState, usize> {
        let mut counts: HashMap<NodeState, usize> = HashMap::new();
        for state in self.node_states().values() {
            *counts.entry(*state).or_insert(0) += 1;
        }
        counts
    }

    /// The active alerts currently attributed to one node (its own problems plus any
    /// suppressed-but-shown downstream entry).
    #[must_use]
    pub fn alerts_for(&self, node: NodeId) -> Vec<Alert> {
        self.active
            .lock()
            .expect("alerts mutex poisoned")
            .values()
            .filter(|a| a.subject.is_node(node))
            .cloned()
            .collect()
    }

    /// The set of nodes currently committed `Unreachable` — the suppression down-set.
    ///
    /// Public so ADR-043's shadow preview can ask *this* engine what is down rather than deriving
    /// its own answer: the preview exists to predict what suppression would do, and a second
    /// definition of "down" would let it predict something the engine would never actually do.
    pub fn down_set(&self) -> BTreeSet<NodeId> {
        // Incrementally maintained in `process_check`, so this is O(down) not an O(live) scan.
        self.down.lock().expect("down mutex poisoned").clone()
    }

    /// Feed one poll result through the engine: a liveness check from the outcome plus a
    /// threshold check per sample that has a resolved threshold. Returns notify actions for
    /// every committed transition (also broadcast to SSE subscribers here).
    pub fn observe(&self, result: &PollResult) -> Vec<NotifyAction> {
        let node = result.node_id;
        // Rolled-up display state before this observation. Only this node's own state can move in
        // one `observe` (suppression re-attributes other nodes' alerts but leaves their committed
        // liveness — and thus their display state — unchanged), so a single before/after diff
        // captures every node-state SSE event this call should emit (S14).
        let state_before = self.node_state(node);
        let mut actions = Vec::new();

        // One config read for the whole result — the maintenance flag plus every sample's resolved
        // threshold — instead of one acquisition per sample.
        //
        // Two things make that worth doing at fleet sample rates. An SNMP *table* poll emits one
        // sample per interface per column, so `if_hc_in_octets` arrives a hundred times in one
        // result; `resolve` is a pure function of (node, metric, config snapshot), and so is
        // `check_id`, so resolving once per **distinct metric name** is identical work with the
        // repeats removed — a hundred `RwLock` acquisitions, `Vec<ScopedThreshold>` builds, rule
        // clones and UUIDv5 (SHA-1) hashes collapse to one. And the guard is dropped before any
        // `process_check` call: `process_check` and `resweep_suppression` take `config.read()`
        // themselves, and `std::sync::RwLock` gives no re-entrancy guarantee — a writer arriving
        // between the two acquisitions can deadlock the thread against itself.
        //
        // A small `Vec` with a linear scan rather than a map: the distinct-metric count is a handful
        // even for a wide table, and this way a sample-free result (ICMP liveness, the common case)
        // allocates nothing at all.
        //
        // ⚠️ The memo key is the metric name, but the **check id is not**: a per-interface metric
        // gets one check per port (ADR-076). Resolution is still per metric — a threshold rule
        // scopes to a node, not to a port, at this increment — so the memo stays one entry per
        // distinct name and the port is applied when the id is built, below.
        let mut resolved: Vec<(ResolveKey<'_>, Option<EffectiveThreshold>)> = Vec::new();
        // The metric names in this result that the catalogue calls per-interface. Captured while
        // the config lock is held so the second pass can rebuild the same memo key without
        // re-acquiring it (`process_check` takes the lock itself, and `std::sync::RwLock` offers no
        // re-entrancy guarantee — a writer arriving in between deadlocks the thread against itself).
        let mut per_if_metrics: Vec<&str> = Vec::new();
        // The liveness rule (ADR-075), resolved under the same lock as the sample thresholds.
        // `None` = no rule anywhere in this node's scope chain ⇒ commit state, page nobody.
        let liveness_dwell: Option<u32>;
        // Inside an active maintenance window every check observes `Maintenance` instead of
        // its real state: no alert can fire (Maintenance carries no severity) and existing
        // alerts resolve after the usual dwell. The real state flows again when the window
        // ends, re-committing any surviving problem.
        let in_maintenance = {
            let config = self.config.read().expect("config rwlock poisoned");
            liveness_dwell = config
                .resolve(node, None, LIVENESS)
                .map(|eff| eff.dwell_samples);
            for sample in &result.samples {
                if config.is_per_interface(&sample.metric)
                    && !per_if_metrics.contains(&sample.metric.as_str())
                {
                    per_if_metrics.push(sample.metric.as_str());
                }
                // ⚠️ The memo key is the metric name **plus the port**, because since ADR-076 a rule
                // can be scoped to one port: two ports of one metric can resolve to different
                // bounds, and memoizing on the name alone would apply the first port's rule to
                // every port. A node-wide metric keys on `None` and collapses to one entry, as
                // before — the repeats a table walk produces are still resolved once.
                let key = resolve_key(&sample.metric, sample.ifindex, &per_if_metrics);
                if !resolved.iter().any(|(k, _)| *k == key) {
                    let eff = config.resolve(node, key.1, &sample.metric);
                    resolved.push((key, eff));
                }
            }
            config.maintenance.contains(&node)
        };

        // Liveness from the reachability outcome.
        let raw = if in_maintenance {
            NodeState::Maintenance
        } else {
            match result.outcome {
                CheckOutcome::Reachable => NodeState::Ok,
                CheckOutcome::Unreachable => NodeState::Unreachable,
                CheckOutcome::Error => NodeState::Unknown,
            }
        };
        actions.extend(self.process_check(
            node,
            raw,
            result.at_unix_ms,
            CheckSpec {
                check: check_id(node, LIVENESS),
                metric: LIVENESS,
                // No rule ⇒ the state machine keeps its usual cadence so the Nodes page and the
                // down-set behave exactly as before; only `alerting` changes.
                dwell: liveness_dwell.unwrap_or(DEFAULT_LIVENESS_DWELL),
                is_liveness: true,
                alerting: liveness_dwell.is_some(),
                eval: None,
                ifindex: None,
            },
        ));

        // Threshold checks per sample. Two shapes, and the split is ADR-077 decision 1.
        //
        // A **per-interface** metric gets one check per port (ADR-076), so each sample is observed
        // on a dwell window of its own. Everything else shares one node-wide check — including a
        // table walk whose rows are CPUs, sensors, filesystems, PSUs or battery lines — so this
        // result's samples for it are folded to a **single** observation before the state machine
        // sees any of them.
        //
        // Without the fold, a metric arriving N times per poll pushed N observations into one dwell
        // window, and it failed in both directions: one bad row among good ones had its candidate
        // reset by the very next sample and could never reach the dwell (`huawei_cpu_usage` arrives
        // 15 times, `juniper_cpu_1min` 53 — those rules were **inert**), while N bad rows satisfied
        // a 3-sample dwell inside a single poll. That is exactly the ADR-076 bug, on the rows
        // ADR-076 did not split.
        //
        // ⚠️ The memo above is still keyed per (metric, port) and is read, not rebuilt: folding
        // changes how many times a resolved threshold is *observed*, never how it resolves.
        let mut folded: Vec<(&str, &Sample, &EffectiveThreshold)> = Vec::new();
        for sample in &result.samples {
            let key = resolve_key(&sample.metric, sample.ifindex, &per_if_metrics);
            let Some(eff) = resolved
                .iter()
                .find(|(k, _)| *k == key)
                .and_then(|(_, eff)| eff.as_ref())
            else {
                continue;
            };
            match key.1 {
                // One port, one check, one observation (ADR-076).
                Some(idx) => actions.extend(self.observe_threshold_sample(
                    node,
                    result.at_unix_ms,
                    in_maintenance,
                    sample,
                    eff,
                    Some(idx),
                )),
                // Node-wide: keep the worst sample **in this rule's own direction**, observe below.
                None => match folded.iter_mut().find(|(m, _, _)| *m == sample.metric) {
                    Some(slot) => {
                        if eff.is_worse(sample.value, slot.1.value) {
                            slot.1 = sample;
                        }
                    }
                    None => folded.push((sample.metric.as_str(), sample, eff)),
                },
            }
        }
        for (_, sample, eff) in folded {
            actions.extend(self.observe_threshold_sample(
                node,
                result.at_unix_ms,
                in_maintenance,
                sample,
                eff,
                None,
            ));
        }

        // Push an incremental node-state event only when the rolled-up display state actually moved
        // (including this node's first observation) — subscribers patch the one node live instead of
        // re-fetching the whole fleet (S14). `after` is `Some` whenever a liveness check ran.
        let state_after = self.node_state(node);
        if state_after != state_before {
            if let Some(state) = state_after {
                self.broadcast_node_state(node, state, result.at_unix_ms);
            }
        }
        actions
    }

    /// Run one raw state through a check's hysteresis and emit a fire/resolve action on a
    /// committed transition. `is_liveness` checks also update the per-node committed-state
    /// map and apply dependency suppression (root-cause attribution) on a problem
    /// transition.
    /// Feed one already-resolved sample through the state machine as a threshold check.
    ///
    /// Shared by both shapes in [`Self::observe`] — a port's own check and a node-wide folded one —
    /// since they differ only in which id the check carries. Written once so the counter rule, the
    /// maintenance substitution and the [`ThresholdEval`] that describes the breach cannot drift
    /// between them.
    fn observe_threshold_sample(
        &self,
        node: NodeId,
        at_unix_ms: i64,
        in_maintenance: bool,
        sample: &Sample,
        eff: &EffectiveThreshold,
        ifindex: Option<IfIndex>,
    ) -> Vec<NotifyAction> {
        let check = match ifindex {
            Some(idx) => interface_check_id(node, idx, &sample.metric),
            None => check_id(node, &sample.metric),
        };
        let raw = if in_maintenance {
            NodeState::Maintenance
        } else if sample.kind == MetricKind::Counter {
            // A raw monotonic counter has no meaningful fixed bound: `above` latches
            // permanently once crossed and `below` fires across every reboot's counter
            // reset — rates are derived at query time instead (ADR-012). Creation now
            // rejects counter metrics; observing `Ok` here (rather than skipping) lets
            // a rule that predates that rejection drain its latched alert through the
            // normal recovery path.
            NodeState::Ok
        } else {
            eff.evaluate(sample.value)
        };
        // The side the operator is told about. For a band rule this is the primary side, which is
        // not necessarily the side that just tripped — a genuine narrowing, and the reason the
        // breach detail is worth revisiting when a screen wants to say *which* bound was crossed.
        let eval = ThresholdEval {
            value: sample.value,
            direction: eff.direction(),
            warning: eff.warning(),
            critical: eff.critical(),
        };
        self.process_check(
            node,
            raw,
            at_unix_ms,
            CheckSpec {
                check,
                metric: &sample.metric,
                dwell: eff.dwell_samples,
                is_liveness: false,
                // A threshold check exists only because a rule resolved for it.
                alerting: true,
                eval: Some(eval),
                ifindex,
            },
        )
    }

    fn process_check(
        &self,
        node: NodeId,
        raw: NodeState,
        at_unix_ms: i64,
        spec: CheckSpec<'_>,
    ) -> Vec<NotifyAction> {
        let CheckSpec {
            check,
            metric,
            dwell,
            is_liveness,
            alerting,
            eval,
            ifindex,
        } = spec;
        let (transition, committed) = {
            let mut states = self.states.lock().expect("states mutex poisoned");
            let cs = states.entry(check).or_insert_with(|| {
                CheckState::new(NodeState::Ok, dwell.max(1), FLAP_WINDOW_MS, FLAP_THRESHOLD)
            });
            // A check's state lives for the process, so the dwell captured at first observation
            // would otherwise outlive every edit to the rule that set it — an operator raising
            // "3 breaches" to "5" would see no effect until the next core restart, with the UI
            // showing 5. Re-point it every observation instead (ADR-075).
            cs.set_dwell(dwell.max(1));
            let t = cs.observe(raw, at_unix_ms);
            (t, cs.committed())
        };

        // Keep the per-node committed liveness current even when nothing transitioned (a
        // node's first reachable poll commits `ok` with no transition, but the inventory
        // still needs to read it as `ok`). Capture whether this node's *down-set membership*
        // flipped (entered or left `Unreachable`) — that's exactly when downstream dependency
        // suppression must be re-evaluated (a parent going down/up changes its children's roll-up).
        let down_set_changed = if is_liveness {
            let previous = self
                .live
                .lock()
                .expect("live mutex poisoned")
                .insert(node, committed);
            let flipped = matches!(previous, Some(NodeState::Unreachable))
                != matches!(committed, NodeState::Unreachable);
            if flipped {
                // Keep the incremental down-set in lockstep with `live` (its only mutation site).
                let mut down = self.down.lock().expect("down mutex poisoned");
                if matches!(committed, NodeState::Unreachable) {
                    down.insert(node);
                } else {
                    down.remove(&node);
                }
            }
            flipped
        } else {
            false
        };

        // No rule ⇒ no paging (ADR-075). Everything above still ran: the committed state, the
        // down-set and the re-sweep below are what the Nodes page, the fleet summary and
        // dependency suppression read, and deleting an *alert rule* does not ask for those to
        // stop. What it does ask for is that an alert already open on this check be closed —
        // otherwise deleting the rule strands it forever, active in the UI and open in whatever
        // external tool its dedup key reached. Resolving here rather than at config-reload time
        // keeps it to one code path: the poll loop is already visiting every node.
        if !alerting {
            let stranded = self
                .active
                .lock()
                .expect("alerts mutex poisoned")
                .remove(&check);
            let mut actions = Vec::new();
            if let Some(alert) = stranded {
                self.broadcast(&alert, true);
                actions.push(NotifyAction::Resolve(alert));
            }
            if down_set_changed {
                actions.extend(self.resweep_suppression(node));
            }
            return actions;
        }

        let Some(t) = transition else {
            return Vec::new();
        };

        // Dependency suppression (liveness only): if this node is down and every upstream is
        // also down, attribute the alert to the highest down ancestor so it groups under
        // that incident and its own notification is suppressed (ADR-015).
        let root_cause = if is_liveness && t.state.is_problem() {
            let down = self.down_set();
            self.config
                .read()
                .expect("config rwlock poisoned")
                .topology
                .root_cause(node, &down)
        } else {
            None
        };

        let mut actions = match t.to_alert(Subject::Node(node), check, at_unix_ms, root_cause) {
            Some(mut alert) => {
                // Tag the alert with what it measured so the history log / notification is
                // human-readable. The crossed bound depends on the committed severity, now known.
                alert.metric = metric.to_string();
                // Which port, for a per-interface metric. Purely descriptive — `check` already
                // carries it — but it is the only way History, the API and a notification can name
                // the port, since the check id is a one-way hash (ADR-076).
                alert.ifindex = ifindex;
                if let Some(ev) = eval {
                    let threshold = match alert.severity {
                        Severity::Critical => ev.critical,
                        Severity::Warning => ev.warning,
                        Severity::Info => ev.warning.or(ev.critical),
                    };
                    alert.breach = Some(Breach {
                        value: ev.value,
                        threshold,
                        direction: ev.direction,
                    });
                }
                self.active
                    .lock()
                    .expect("alerts mutex poisoned")
                    .insert(check, alert.clone());
                self.broadcast(&alert, false);
                vec![NotifyAction::Fire(alert)]
            }
            None => {
                let prev = self
                    .active
                    .lock()
                    .expect("alerts mutex poisoned")
                    .remove(&check);
                match prev {
                    Some(alert) => {
                        self.broadcast(&alert, true);
                        vec![NotifyAction::Resolve(alert)]
                    }
                    None => Vec::new(),
                }
            }
        };

        // Event-driven dependency roll-up: this node just entered or left the down-set, so
        // reconcile suppression for every *other* node's active liveness alert. Closes the
        // ordering gap where a child that fired before its parent went down never got rolled up
        // (and, symmetrically, re-pages a child left suppressed after its parent recovered).
        if down_set_changed {
            actions.extend(self.resweep_suppression(node));
        }
        actions
    }

    /// Re-evaluate dependency suppression for active liveness alerts after `changed`'s down-set
    /// membership flipped. For each *other* node's active liveness alert whose root-cause
    /// attribution changed, update the active alert (and notify subscribers), then:
    ///
    /// - **newly suppressed** (`None → Some`): it had been paging standalone → emit
    ///   [`NotifyAction::Suppress`] to close its remote incident (rolled up under the parent).
    /// - **no longer suppressed but still down** (`Some → None`): emit [`NotifyAction::Fire`] so
    ///   it pages on its own now that its upstream is back.
    /// - **re-attributed** (`Some → Some`): never paged; just refresh the attribution.
    ///
    /// Liveness only (threshold alerts are never dependency-suppressed). Bounded by the current
    /// active-alert count; runs only when a node actually entered/left `Unreachable`.
    fn resweep_suppression(&self, changed: NodeId) -> Vec<NotifyAction> {
        let down = self.down_set();
        // A flip of `changed` can only change the root-cause attribution of nodes with `changed` on
        // an ancestor path — its descendants. Scope the re-sweep to them (S3): before, every flip
        // re-ran `root_cause` for the *entire* active liveness set, so a parent-down cascade cost
        // O(down × active). `descendants` excludes `changed` itself, matching the old `!= changed`.
        let affected = self
            .config
            .read()
            .expect("config rwlock poisoned")
            .topology
            .descendants(changed);
        // Snapshot the liveness alerts to reconsider, then release the lock before the
        // per-alert topology read / broadcast (keeps lock ordering flat, no nesting).
        let candidates: Vec<Alert> = {
            let active = self.active.lock().expect("alerts mutex poisoned");
            active
                .values()
                // Node subjects only: the dependency graph is a graph of nodes, so a
                // pool-coverage alert has no ancestor to be attributed to.
                .filter(|a| a.metric == LIVENESS && a.node().is_some_and(|n| affected.contains(&n)))
                .cloned()
                .collect()
        };
        let mut actions = Vec::new();
        for alert in candidates {
            let Some(alert_node) = alert.node() else {
                continue;
            };
            let new_rc = self
                .config
                .read()
                .expect("config rwlock poisoned")
                .topology
                .root_cause(alert_node, &down);
            if new_rc == alert.root_cause {
                continue; // attribution unchanged
            }
            // Persist the new attribution on the still-active alert, then refresh subscribers.
            let updated = {
                let mut active = self.active.lock().expect("alerts mutex poisoned");
                let Some(cur) = active.get_mut(&alert.check) else {
                    continue; // resolved concurrently — nothing to reconcile
                };
                cur.root_cause = new_rc;
                cur.clone()
            };
            self.broadcast(&updated, false);
            match (alert.root_cause, new_rc) {
                (None, Some(_)) => actions.push(NotifyAction::Suppress(updated)),
                (Some(_), None) => actions.push(NotifyAction::Fire(updated)),
                _ => {}
            }
        }
        actions
    }

    /// Whether `node` is inside an active maintenance window (per the config snapshot).
    /// Used by the event pipeline to suppress event alerts the same way poll alerts are.
    #[must_use]
    pub fn in_maintenance(&self, node: NodeId) -> bool {
        self.config
            .read()
            .expect("config rwlock poisoned")
            .maintenance
            .contains(&node)
    }

    /// The node's **folder group** (`nodes.group_id`) per the config snapshot, for RBAC visibility
    /// (`api/scope.rs`). Not a tag value — see the [`NodeMeta`] docs.
    ///
    /// Returns `None` both for a genuinely ungrouped node and for one the snapshot has never seen
    /// (created since the last config-generation refresh). The caller treats those the same and
    /// hides the node from a scoped principal, which is the fail-closed direction: a node can be
    /// briefly invisible to its owner, never briefly visible to someone outside its scope.
    #[must_use]
    pub fn node_folder_group(&self, node: NodeId) -> Option<Uuid> {
        self.config
            .read()
            .expect("config rwlock poisoned")
            .node_meta
            .get(&node)
            .and_then(|m| m.folder_group)
    }

    /// Whether any node in `pool` sits in one of `visible` — the group-scope question for a
    /// pool-coverage alert (`api/scope.rs::allows_subject`).
    ///
    /// Fail-closed on the two ways this can be empty: a pool the snapshot has never seen (created
    /// since the last config generation, or holding only ungrouped nodes) answers `false`, so a
    /// scoped caller is briefly denied rather than briefly shown someone else's site — the same
    /// rule `allows_node` follows for an unknown node.
    #[must_use]
    pub fn pool_is_in_any_group(&self, pool: &str, visible: &[Uuid]) -> bool {
        self.config
            .read()
            .expect("config rwlock poisoned")
            .pool_groups
            .get(pool)
            .is_some_and(|groups| visible.iter().any(|g| groups.contains(g)))
    }

    /// Insert an event-rule alert into the active set and broadcast it. Event alerts are
    /// edge-triggered (no `CheckState`/dwell — the rule's min-count/window gate and TTL do
    /// the damping upstream in `events.rs`), so this bypasses `process_check` on purpose.
    /// Dependency suppression is also skipped by design: a device that just emitted an
    /// event is demonstrably reachable, so `root_cause` stays `None`.
    ///
    /// Returns `Fire` only when the check wasn't already active at the same severity
    /// (a severity change replaces the entry and re-fires).
    pub fn raise_event_alert(&self, alert: Alert) -> Option<NotifyAction> {
        {
            let mut active = self.active.lock().expect("alerts mutex poisoned");
            if active
                .get(&alert.check)
                .is_some_and(|a| a.severity == alert.severity)
            {
                return None;
            }
            active.insert(alert.check, alert.clone());
        }
        self.broadcast(&alert, false);
        Some(NotifyAction::Fire(alert))
    }

    /// Remove an event alert from the active set (TTL expiry / clear-pattern / manual
    /// close), broadcast the resolution, and return the `Resolve` action carrying the
    /// previously-active alert. `None` if the check wasn't active.
    pub fn resolve_event_alert(&self, check: CheckId) -> Option<NotifyAction> {
        let prev = self
            .active
            .lock()
            .expect("alerts mutex poisoned")
            .remove(&check)?;
        self.broadcast(&prev, true);
        Some(NotifyAction::Resolve(prev))
    }

    /// Raise the coverage alert for a poller pool that has nodes and no live poller.
    ///
    /// Built on [`Self::raise_event_alert`] because that path is already subject-agnostic — keyed
    /// by `CheckId`, no dwell (the caller owns its own debounce), and `root_cause: None`, which is
    /// correct here for a stronger reason than for an event alert: the dependency graph has no pool
    /// vertices, so there is nothing this could be attributed to.
    ///
    /// `Critical` because an entire site's monitoring has stopped, which is a strictly larger blast
    /// radius than one device being down — and because an existing `critical → PagerDuty` routing
    /// rule is what ADR-009 asks this to reach.
    ///
    /// **A maintenance window does not silence this, including the fleet-wide one an upgrade opens
    /// (ADR-050 decision 12), and that is deliberate.** The gate lives in [`Self::observe`] and
    /// tests a *node* set, so a [`Subject::Pool`] could never fall in it by accident; the question
    /// is whether to add a second gate here, and the answer is no on three counts. The debounce is
    /// already the mechanism for this exact case — [`crate::pool_coverage::DEFAULT_RAISE_AFTER`] is
    /// 300s precisely so an ordinary restart cannot page anyone, against a measured 65s upgrade. A
    /// window long enough to matter would be hiding the one outcome worth paging about, "the
    /// upgrade left a site unmonitored", during the exact window in which it just became true. And
    /// the gate would have to sit in [`Self::raise_event_alert`], which also carries every
    /// syslog/trap-derived alert — silencing far more than the upgrade ever asked for.
    pub fn raise_pool_coverage_alert(&self, pool: &str, at_unix_ms: i64) -> Option<NotifyAction> {
        let subject = Subject::Pool(pool.to_owned());
        let check = subject_check_id(&subject, crate::pool_coverage::COVERAGE_METRIC);
        self.raise_event_alert(Alert {
            subject,
            check,
            severity: Severity::Critical,
            state: NodeState::Critical,
            at_unix_ms,
            root_cause: None,
            flapping: false,
            metric: crate::pool_coverage::COVERAGE_METRIC.to_owned(),
            breach: Some(Breach {
                value: 0.0,
                threshold: Some(1.0),
                direction: Direction::Below,
            }),
            // A pool is not a port.
            ifindex: None,
        })
    }

    /// Feed one **derived** per-interface reading through the ordinary threshold machinery
    /// (ADR-076 decision 3).
    ///
    /// A thin seam onto [`Self::process_check`] rather than a second engine: dwell, flap damping,
    /// dependency suppression, dedup, mutes and the SSE broadcast are the *same code* the poll path
    /// runs. A copy would be a second place alert quality is decided, and the copy is the one that
    /// gets a fix late.
    ///
    /// `None` means **no rule is in force on this port for this metric** — distinct from
    /// `Some(vec![])`, which means a rule looked and nothing changed. The metric is computed for
    /// every candidate the store returns, because computing it is far cheaper than asking whether
    /// it is wanted; the distinction is what lets the caller stop *remembering* the ports nobody
    /// wrote a rule for (ADR-076 increment 6d). Before it, `TrackedChecks` grew to every busy port
    /// in the fleet — and, since that set has no other way to shrink, stayed there.
    ///
    /// # What the caller must decide before calling
    ///
    /// 🚨 **A node whose liveness is not `Ok` must not be observed at all** — not `Ok` (which would
    /// resolve a real congestion alert the moment the device went unreachable) and not `Unknown`
    /// (a problem state, which would raise "utilisation unknown" noise on top of the outage the
    /// liveness check is already paging about). Freezing is what leaves the port's alert open and
    /// honest while the node's own alert does the paging.
    ///
    /// [`Self::node_liveness`] is the question, read through
    /// [`crate::interface_util::may_observe_ports`] — **never [`Self::node_state`]**, which folds in
    /// the very alert this call is about to raise and therefore freezes the evaluator on its own
    /// output (ADR-076 増分 7). A maintenance window is let through rather than frozen, because the
    /// substitution below is exactly what a window is supposed to do to an open port alert.
    ///
    /// Maintenance is handled here rather than by the caller, because `observe` handles it here too
    /// and the two must not disagree about what a window means.
    pub fn observe_interface_metric(
        &self,
        node: NodeId,
        ifindex: IfIndex,
        metric: &'static str,
        value: f64,
        at_unix_ms: i64,
    ) -> Option<Vec<NotifyAction>> {
        let (eff, in_maintenance) = {
            let config = self.config.read().expect("config rwlock poisoned");
            (
                config.resolve(node, Some(ifindex), metric),
                config.maintenance.contains(&node),
            )
        };
        // `None`, not an empty vector: the caller distinguishes "nobody is watching this port"
        // from "somebody is watching and nothing happened".
        let eff = eff?;
        let raw = if in_maintenance {
            NodeState::Maintenance
        } else {
            eff.evaluate(value)
        };
        Some(self.process_check(
            node,
            raw,
            at_unix_ms,
            CheckSpec {
                check: interface_check_id(node, ifindex, metric),
                metric,
                dwell: eff.dwell_samples,
                is_liveness: false,
                alerting: true,
                eval: Some(ThresholdEval {
                    value,
                    direction: eff.direction(),
                    warning: eff.warning(),
                    critical: eff.critical(),
                }),
                ifindex: Some(ifindex),
            },
        ))
    }

    /// What the interface-threshold rules in force for `metric` cover — the evaluator plans its
    /// query from this rather than re-reading `ThresholdStore`, so the rules the query was built
    /// for and the rules the classification uses are the same snapshot.
    #[must_use]
    pub fn interface_rule_coverage(&self, metric: &str) -> RuleCoverage {
        self.config
            .read()
            .expect("config rwlock poisoned")
            .interface_rule_coverage(metric)
    }

    /// The rules that reach `(node, ifindex)`, each flagged with whether it is in force
    /// (ADR-076 決定 11).
    ///
    /// The **rules** come from the caller — `GET /nodes/{id}/interfaces/{ifindex}/thresholds`
    /// reads them straight from PostgreSQL — while the **node metadata** comes from the snapshot
    /// held here. The split is deliberate: the snapshot refreshes on the config generation, so a
    /// rule saved a second ago is not in it yet, and a list that omitted the operator's own new
    /// rule would fail at exactly the moment they are looking. A node's profile, tags and folder
    /// chain do not change under them in the same way.
    ///
    /// A node the snapshot has never seen resolves against `None` metadata, which matches only
    /// global rules — the same answer the engine would give for it.
    #[must_use]
    pub fn matching_rules(
        &self,
        rules: &[StoredThreshold],
        node: NodeId,
        ifindex: Option<IfIndex>,
    ) -> Vec<(StoredThreshold, bool)> {
        let config = self.config.read().expect("config rwlock poisoned");
        matching_rules(rules, node, ifindex, config.node_meta.get(&node))
    }

    /// A node's committed **liveness** state — what its liveness check settled on, with no alert
    /// rolled into it. `None` when the engine has never observed the node, which every caller must
    /// treat as "we have no opinion", not as "fine".
    ///
    /// 🚨 **This is not [`Self::node_state`], and confusing the two is the bug ADR-076 増分 7 had to
    /// fix.** `node_state` is the *display* roll-up: the worse of liveness and every active alert on
    /// the node. The interface evaluator gated on it, so the instant a port alert fired the node
    /// stopped reading as `Ok` — and the evaluator, which is also the only thing that can ever
    /// resolve that alert, skipped the node from then on. **A port alert froze its own evaluator**,
    /// and on real hardware nothing ever cleared: 12 fires and 0 resolves in one day.
    ///
    /// Ask this when the question is "is the device there". Ask `node_state` only when the question
    /// is "what colour is this row".
    #[must_use]
    pub fn node_liveness(&self, node: NodeId) -> Option<NodeState> {
        self.live
            .lock()
            .expect("live mutex poisoned")
            .get(&node)
            .copied()
    }

    /// Resolve every active **derived** per-interface alert whose rule no longer resolves.
    ///
    /// The poll path already closes a stranded alert when its rule is deleted ([`Self::observe`]'s
    /// `!alerting` branch), but it can only close checks it visits — and it never visits
    /// `metric@ifindex` for a derived metric, because nothing polls `if_in_util_pct`. Without this
    /// sweep, deleting a port rule left its alert open in the UI and its incident open in whatever
    /// external tool the dedup key reached, for the life of the process.
    ///
    /// **Derived metrics only.** A *collected* per-interface metric (`if_oper_status@7`) does arrive
    /// on the poll path, so it already has an owner; touching it here would give one alert two
    /// closers racing each other.
    ///
    /// 🚨 **Safe only because a failed config load no longer degrades to "no rules" (ADR-080).**
    /// Before that, "the rule was deleted" and "the ruleset could not be read" were the same
    /// observation, and this sweep would have resolved every port alert in the fleet — sending a
    /// recovery for each — on any database blip. Do not reorder those two changes.
    pub fn resolve_orphaned_interface_alerts(&self) -> Vec<NotifyAction> {
        // Collect under the locks, resolve outside them: `resolve_event_alert` takes `active`
        // itself. The order taken here is config → active, the same as `process_check`.
        let orphans: Vec<CheckId> = {
            let config = self.config.read().expect("config rwlock poisoned");
            let active = self.active.lock().expect("alerts mutex poisoned");
            active
                .values()
                .filter_map(|a| {
                    let node = a.node()?;
                    let ifindex = a.ifindex?;
                    let metric = crate::interface_util::derived_metric_name(&a.metric)?;
                    config
                        .resolve(node, Some(ifindex), metric)
                        .is_none()
                        .then_some(a.check)
                })
                .collect()
        };
        orphans
            .into_iter()
            .filter_map(|check| {
                // 🚨 Drop the dwell/flap bookkeeping along with the alert, or the check goes
                // permanently silent. Resolving only the alert leaves the state machine committed
                // at `Warning` while nothing is active, so a rule recreated on the same port
                // observes `Warning → Warning`, sees no transition, and **never fires again** for
                // the life of the process.
                //
                // Found on the test server, not by the unit test that shipped with this sweep: the
                // alert closed on deletion, the rule was recreated at 1%, the port sat at 6.7% for
                // eight minutes and nothing happened. The test only asserted the sweep was
                // idempotent — which it was, on a check that could no longer do anything.
                //
                // Removing the entry rather than resetting it is the honest form: the rule that
                // set this check's dwell is gone, so its window carries no meaning to preserve.
                self.states
                    .lock()
                    .expect("states mutex poisoned")
                    .remove(&check);
                self.resolve_event_alert(check)
            })
            .collect()
    }

    /// Resolve a pool's coverage alert. `None` if it was not active.
    pub fn resolve_pool_coverage_alert(&self, pool: &str) -> Option<NotifyAction> {
        self.resolve_event_alert(subject_check_id(
            &Subject::Pool(pool.to_owned()),
            crate::pool_coverage::COVERAGE_METRIC,
        ))
    }

    fn broadcast(&self, alert: &Alert, resolved: bool) {
        self.send_frame(alert, "resolved", resolved.into());
    }

    /// Send one alert frame to the SSE fan-out, with the lifecycle key that distinguishes it.
    ///
    /// Every subject is broadcast. `node` carries the flat subject form — a bare UUID for a node,
    /// `pool:<name>` otherwise — and `subject_kind`/`subject_name` beside it are what a client
    /// branches on; `web/src/services/sse.ts` gates frame validity on `node` being a string, so
    /// that field stays present and stays a string for every subject.
    //
    // One builder on purpose: this was two hand-written copies of the same object differing only
    // in the lifecycle key, and the object is the contract the WebUI parses — so a field added to
    // one and not the other was a live-feed bug with nothing to compile against.
    fn send_frame(&self, alert: &Alert, lifecycle: &str, value: serde_json::Value) {
        // Wire shape the WebUI consumes (Alert fields + the subject decomposition + one lifecycle
        // key). Kept in step with `ActiveAlertView` in `api/alerts.rs` — the stream patches the
        // list that endpoint seeded, so a client parses both with one reader.
        let mut event = serde_json::json!({
            "node": alert.subject,
            "subject_kind": alert.subject.kind(),
            "subject_name": alert.subject.name(),
            "check": alert.check,
            "severity": alert.severity,
            "state": alert.state,
            "at_unix_ms": alert.at_unix_ms,
            "root_cause": alert.root_cause,
            "flapping": alert.flapping,
            "metric": alert.metric,
            "breach": alert.breach,
        });
        event[lifecycle] = value;
        // Fire-and-forget: no subscribers is not an error.
        let _ = self
            .tx
            .send((alert.subject.clone(), Arc::from(event.to_string())));
    }

    /// Broadcast an inbound ack-state change for one alert so subscribers update the read-only
    /// acked indicator live (ADR-015). Finds the matching active alert by its dedup identity
    /// `(subject, check, severity)` and re-sends its wire shape with `acked` attached (the external
    /// tool's view as a JSON value, or `null` when cleared). No `resolved` flag ⇒ the client
    /// treats it as an upsert, not a recovery. If the alert isn't currently active there's
    /// nothing on screen to update, so this is a no-op (History reflects it on next fetch).
    pub fn broadcast_acked(
        &self,
        subject: &Subject,
        check: Uuid,
        severity: Severity,
        acked: Option<serde_json::Value>,
    ) {
        let active = self.active.lock().expect("alerts mutex poisoned");
        let Some(alert) = active.values().find(|a| {
            &a.subject == subject && a.check.as_uuid() == check && a.severity == severity
        }) else {
            return;
        };
        self.send_frame(alert, "acked", acked.unwrap_or(serde_json::Value::Null));
    }

    /// Push a frame onto the alert stream directly, for tests of the stream *plumbing*.
    ///
    /// The SSE scope filter is a property of the transport, not of the alert logic, so its tests
    /// need to control which subject each frame names without first driving a real alert to dwell —
    /// including naming a node the engine has never observed, which is precisely the fail-closed
    /// case worth covering.
    #[cfg(test)]
    pub(crate) fn broadcast_test_frame(&self, subject: Subject, body: &str) {
        let _ = self.tx.send((subject, Arc::from(body)));
    }
}

impl Default for AlertManager {
    fn default() -> Self {
        Self::new()
    }
}

/// A Webhook [`NotifyChannel`]: POSTs the alert JSON to a configured URL.
pub struct WebhookChannel {
    http: reqwest::Client,
    url: String,
}

impl WebhookChannel {
    #[must_use]
    pub fn new(url: String) -> Self {
        // Hardened client: a bounded timeout and — importantly for SSRF — NO redirect following.
        // A webhook endpoint that 30x-redirects to a loopback/metadata address is an escalation
        // vector, so core never follows a redirect on the notification path. (The config is static,
        // so building the client cannot fail at runtime; the fallback keeps the no-redirect policy.)
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent("Yagra-core")
            .build()
            .unwrap_or_default();
        Self { http, url }
    }
}

/// Whether a webhook target must be refused (SSRF, runtime/defense-in-depth alongside the API-edge
/// [`crate::api`] check). An IP-literal host is judged directly; a hostname is resolved and refused
/// only if **every** answer is blocked. A DNS failure is *not* treated as blocked — the POST then
/// fails naturally and is reported as a delivery error.
async fn webhook_target_blocked(url: &reqwest::Url) -> bool {
    let Some(host) = url.host_str() else {
        return true;
    };
    if let Some(ip) = yagra_common::host_ip(host) {
        return is_ssrf_blocked(ip);
    }
    let port = url
        .port_or_known_default()
        .unwrap_or(if url.scheme() == "https" { 443 } else { 80 });
    match tokio::net::lookup_host((host, port)).await {
        Ok(addrs) => {
            let addrs: Vec<_> = addrs.collect();
            !addrs.is_empty() && addrs.iter().all(|a| is_ssrf_blocked(a.ip()))
        }
        Err(_) => false,
    }
}

#[async_trait]
impl NotifyChannel for WebhookChannel {
    async fn deliver(&self, notification: &Notification) -> Result<(), NotifyError> {
        // SSRF guard at delivery time (the API edge validates the configured URL, but DNS can
        // change between config and delivery): refuse a target whose every resolved address is
        // blocked before any request leaves core.
        if let Ok(url) = reqwest::Url::parse(&self.url) {
            if webhook_target_blocked(&url).await {
                return Err(NotifyError::Delivery(
                    "webhook target address is not allowed (SSRF)".to_owned(),
                ));
            }
        }
        self.http
            .post(&self.url)
            .header("content-type", "application/json")
            .body(notification.payload.clone())
            .send()
            .await
            .map_err(|e| NotifyError::Delivery(e.to_string()))?
            .error_for_status()
            .map_err(|e| NotifyError::Delivery(e.to_string()))?;
        Ok(())
    }
}

/// The dedup identity string sent to lifecycle-aware vendors: PagerDuty `dedup_key` and
/// JSM `alias`. Stable across restarts (check ids are UUIDv5), so a resolve always finds
/// the incident its fire created.
///
/// `pub(crate)` because a notification template exposes it as `{{ dedup_key }}` (ADR-039): an
/// operator correlating what Yagra sent with what the vendor shows needs the same string, and two
/// spellings of it would drift.
pub(crate) fn dedup_string(key: &yagra_alert::DedupKey) -> String {
    // `Subject`'s Display renders a node as a bare UUID, so a node alert's dedup string is
    // byte-identical to what it was before subjects existed — an incident opened by an older
    // core still closes. A pool renders as `pool:<name>`.
    format!(
        "yagra:{}:{}:{}",
        key.subject,
        key.check,
        key.severity.as_str()
    )
}

/// The hardened outbound client shared by the vendor channels: bounded timeout, **no
/// redirect following** (SSRF — same policy as [`WebhookChannel`]).
fn hardened_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent("Yagra-core")
        .build()
        .unwrap_or_default()
}

/// Map a vendor API response to the channel result. 429 waits out `Retry-After` (capped
/// at 10s) and then returns `Err` so the dispatcher's retry policy counts the attempt.
/// `also_ok` admits one vendor-specific extra status (e.g. JSM close → 404 = already
/// closed, which must read as success for idempotency).
async fn vendor_response(
    resp: reqwest::Response,
    also_ok: Option<reqwest::StatusCode>,
) -> Result<(), NotifyError> {
    let status = resp.status();
    if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        let wait_secs = resp
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.trim().parse::<u64>().ok())
            .unwrap_or(2);
        tokio::time::sleep(std::time::Duration::from_secs(wait_secs.min(10))).await;
        return Err(NotifyError::Delivery("rate limited (429)".to_owned()));
    }
    if status.is_success() || also_ok.is_some_and(|s| s == status) {
        return Ok(());
    }
    Err(NotifyError::Delivery(format!("unexpected status {status}")))
}

/// PagerDuty Events API v2 [`NotifyChannel`]: `trigger` on fire, `resolve` on recovery,
/// correlated by `dedup_key`. The routing key is a secret — never logged.
pub struct PagerDutyChannel {
    http: reqwest::Client,
    url: String,
    routing_key: String,
}

/// Default (US) Events API v2 endpoint; EU tenants override via the channel config.
const PAGERDUTY_DEFAULT_URL: &str = "https://events.pagerduty.com/v2/enqueue";

impl PagerDutyChannel {
    #[must_use]
    pub fn new(routing_key: String, api_url: Option<String>) -> Self {
        Self {
            http: hardened_client(),
            url: api_url.unwrap_or_else(|| PAGERDUTY_DEFAULT_URL.to_owned()),
            routing_key,
        }
    }

    async fn send_event(
        &self,
        action: &str,
        notification: &Notification,
        with_payload: bool,
    ) -> Result<(), NotifyError> {
        if let Ok(url) = reqwest::Url::parse(&self.url) {
            if webhook_target_blocked(&url).await {
                return Err(NotifyError::Delivery(
                    "PagerDuty target address is not allowed (SSRF)".to_owned(),
                ));
            }
        }
        let body = pagerduty_body(&self.routing_key, action, notification, with_payload);
        let resp = self
            .http
            .post(&self.url)
            .json(&body)
            .send()
            .await
            .map_err(|e| NotifyError::Delivery(e.to_string()))?;
        vendor_response(resp, None).await
    }
}

/// The Events API v2 request body (pure — unit-tested against the wire contract).
fn pagerduty_body(
    routing_key: &str,
    action: &str,
    notification: &Notification,
    with_payload: bool,
) -> serde_json::Value {
    let mut body = serde_json::json!({
        "routing_key": routing_key,
        "event_action": action,
        "dedup_key": dedup_string(&notification.dedup_key),
    });
    if with_payload {
        // custom_details carries the full alert JSON (payload is pre-rendered JSON text).
        let details: serde_json::Value =
            serde_json::from_str(&notification.payload).unwrap_or(serde_json::Value::Null);
        body["payload"] = serde_json::json!({
            "summary": truncate_chars(&notification.summary, 1024),
            "source": notification.dedup_key.subject.to_string(),
            "severity": notification.severity.as_str(),
            "custom_details": details,
        });
    }
    body
}

#[async_trait]
impl NotifyChannel for PagerDutyChannel {
    async fn deliver(&self, notification: &Notification) -> Result<(), NotifyError> {
        self.send_event("trigger", notification, true).await
    }

    async fn deliver_resolve(&self, notification: &Notification) -> Result<(), NotifyError> {
        // Resolve needs only the dedup_key; PD ignores unknown keys (idempotent).
        self.send_event("resolve", notification, false).await
    }
}

/// JSM Alerts (Opsgenie-compatible) [`NotifyChannel`]: create alert on fire (dedup via
/// `alias`), close-by-alias on recovery. The GenieKey is a secret — never logged.
pub struct JsmChannel {
    http: reqwest::Client,
    api_url: String,
    api_key: String,
}

impl JsmChannel {
    #[must_use]
    pub fn new(api_url: String, api_key: String) -> Self {
        Self {
            http: hardened_client(),
            api_url: api_url.trim_end_matches('/').to_owned(),
            api_key,
        }
    }

    async fn guard(&self, url: &str) -> Result<(), NotifyError> {
        if let Ok(url) = reqwest::Url::parse(url) {
            if webhook_target_blocked(&url).await {
                return Err(NotifyError::Delivery(
                    "JSM target address is not allowed (SSRF)".to_owned(),
                ));
            }
        }
        Ok(())
    }
}

#[async_trait]
impl NotifyChannel for JsmChannel {
    async fn deliver(&self, notification: &Notification) -> Result<(), NotifyError> {
        let url = format!("{}/alerts", self.api_url);
        self.guard(&url).await?;
        let resp = self
            .http
            .post(&url)
            .header("authorization", format!("GenieKey {}", self.api_key))
            .json(&jsm_create_body(notification))
            .send()
            .await
            .map_err(|e| NotifyError::Delivery(e.to_string()))?;
        vendor_response(resp, None).await
    }

    async fn deliver_resolve(&self, notification: &Notification) -> Result<(), NotifyError> {
        let url = jsm_close_url(&self.api_url, notification);
        self.guard(&url).await?;
        let resp = self
            .http
            .post(&url)
            .header("authorization", format!("GenieKey {}", self.api_key))
            .json(&serde_json::json!({ "source": "yagra" }))
            .send()
            .await
            .map_err(|e| NotifyError::Delivery(e.to_string()))?;
        // 404 = no open alert with that alias (already closed / never created) — success,
        // so a resolve is idempotent and never dangles on retry.
        vendor_response(resp, Some(reqwest::StatusCode::NOT_FOUND)).await
    }
}

/// The JSM/Opsgenie create-alert body (pure — unit-tested against the wire contract).
fn jsm_create_body(notification: &Notification) -> serde_json::Value {
    let priority = match notification.severity {
        Severity::Critical => "P1",
        Severity::Warning => "P3",
        Severity::Info => "P5",
    };
    serde_json::json!({
        "message": truncate_chars(&notification.summary, 130),
        "alias": dedup_string(&notification.dedup_key),
        "priority": priority,
        "description": notification.payload,
        "source": "yagra",
    })
}

/// The JSM/Opsgenie close-by-alias URL.
///
/// The alias is percent-encoded as one path segment. A node alias is UUID hex, dashes and
/// colons, none of which that encoding touches — so the URL is byte-identical to the one an
/// older core built, and an incident opened before this change still closes.
//
// It stopped being safe to interpolate raw once a pool subject entered the alias: a pool name is
// operator-authored free text and may hold a space or a `/`, which would silently address the
// wrong resource or produce an unparseable URL — and a close that never lands is the dangling
// incident `Dispatcher::dispatch_resolve` exists to prevent. `Url::path_segments_mut` is the url
// crate reqwest already carries; no new dependency.
fn jsm_close_url(api_url: &str, notification: &Notification) -> String {
    let alias = dedup_string(&notification.dedup_key);
    let encoded = reqwest::Url::parse(api_url)
        .ok()
        .and_then(|mut url| {
            url.path_segments_mut().ok()?.pop_if_empty().push(&alias);
            Some(url)
        })
        .and_then(|url| {
            url.path_segments()?
                .next_back()
                .map(std::borrow::ToOwned::to_owned)
        })
        // A non-base or unparseable `api_url` is a misconfiguration the delivery guard already
        // rejects; fall back to the raw alias rather than dropping the close.
        .unwrap_or(alias);
    format!("{api_url}/alerts/{encoded}/close?identifierType=alias")
}

/// Clip to at most `max` characters on a char boundary (vendor field limits).
fn truncate_chars(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((idx, _)) => text[..idx].to_owned(),
        None => text.to_owned(),
    }
}

/// An email [`NotifyChannel`] over SMTP (`lettre`, async + rustls).
pub struct EmailChannel {
    mailer: lettre::AsyncSmtpTransport<lettre::Tokio1Executor>,
    from: lettre::message::Mailbox,
    to: lettre::message::Mailbox,
}

impl EmailChannel {
    /// Build from explicit SMTP params. Returns `None` if host/from/to are malformed.
    pub fn new(
        host: &str,
        port: Option<u16>,
        from: &str,
        to: &str,
        user: Option<&str>,
        pass: Option<&str>,
    ) -> Option<Self> {
        use lettre::transport::smtp::authentication::Credentials;
        if host.is_empty() {
            return None;
        }
        let from = from.parse().ok()?;
        let to = to.parse().ok()?;
        let mut builder = lettre::AsyncSmtpTransport::<lettre::Tokio1Executor>::relay(host).ok()?;
        if let Some(port) = port {
            builder = builder.port(port);
        }
        if let (Some(user), Some(pass)) = (user, pass) {
            builder = builder.credentials(Credentials::new(user.to_owned(), pass.to_owned()));
        }
        Some(Self {
            mailer: builder.build(),
            from,
            to,
        })
    }

    /// Build from env (`YAGRA_SMTP_HOST`, `_FROM`, `_TO`, optional `_PORT`/`_USER`/`_PASS`).
    /// Returns `None` if the required vars are missing or malformed.
    pub fn from_env() -> Option<Self> {
        let host = std::env::var("YAGRA_SMTP_HOST")
            .ok()
            .filter(|s| !s.is_empty())?;
        let from = std::env::var("YAGRA_SMTP_FROM").ok()?;
        let to = std::env::var("YAGRA_SMTP_TO").ok()?;
        let port = std::env::var("YAGRA_SMTP_PORT")
            .ok()
            .and_then(|p| p.parse::<u16>().ok());
        let user = std::env::var("YAGRA_SMTP_USER").ok();
        let pass = std::env::var("YAGRA_SMTP_PASS").ok();
        Self::new(&host, port, &from, &to, user.as_deref(), pass.as_deref())
    }
}

/// Build a live delivery channel from a stored channel config (None if email params are bad).
fn build_channel(config: &ChannelConfig) -> Option<Arc<dyn NotifyChannel>> {
    match config {
        ChannelConfig::Webhook { url } => {
            Some(Arc::new(WebhookChannel::new(url.clone())) as Arc<dyn NotifyChannel>)
        }
        ChannelConfig::Email {
            host,
            port,
            from,
            to,
            user,
            pass,
        } => EmailChannel::new(host, *port, from, to, user.as_deref(), pass.as_deref())
            .map(|c| Arc::new(c) as Arc<dyn NotifyChannel>),
        ChannelConfig::PagerDuty {
            routing_key,
            api_url,
        } => Some(
            Arc::new(PagerDutyChannel::new(routing_key.clone(), api_url.clone()))
                as Arc<dyn NotifyChannel>,
        ),
        ChannelConfig::Jsm { api_url, api_key } => {
            Some(Arc::new(JsmChannel::new(api_url.clone(), api_key.clone()))
                as Arc<dyn NotifyChannel>)
        }
    }
}

#[async_trait]
impl NotifyChannel for EmailChannel {
    async fn deliver(&self, notification: &Notification) -> Result<(), NotifyError> {
        use lettre::AsyncTransport;
        let email = lettre::Message::builder()
            .from(self.from.clone())
            .to(self.to.clone())
            .subject(notification.summary.clone())
            .body(notification.payload.clone())
            .map_err(|e| NotifyError::Delivery(e.to_string()))?;
        self.mailer
            .send(email)
            .await
            .map_err(|e| NotifyError::Delivery(e.to_string()))?;
        Ok(())
    }
}

/// Fan-out channel: deliver to every configured channel; fails if any fails (so the
/// dispatcher's retry covers a transient outage on any of them).
pub struct MultiChannel {
    channels: Vec<Box<dyn NotifyChannel>>,
}

#[async_trait]
impl NotifyChannel for MultiChannel {
    async fn deliver(&self, notification: &Notification) -> Result<(), NotifyError> {
        for channel in &self.channels {
            channel.deliver(notification).await?;
        }
        Ok(())
    }

    // Must forward (not inherit the no-op default) or a lifecycle-aware child channel
    // would never see its resolve.
    async fn deliver_resolve(&self, notification: &Notification) -> Result<(), NotifyError> {
        for channel in &self.channels {
            channel.deliver_resolve(notification).await?;
        }
        Ok(())
    }
}

/// An unexpired mute, resolved for matching: the node plus the precomputed [`CheckId`]
/// (mutes are stored by check *name*, but an [`Alert`] only carries the id — the v5 hash
/// is recomputed here at load time). `check: None` mutes every check on the node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveMute {
    pub node: NodeId,
    pub check: Option<CheckId>,
    /// The stored check name verbatim, so a per-interface metric's alerts match too (ADR-076).
    /// See [`mute_matches`] for why the id alone is not enough.
    pub metric: Option<String>,
}

impl ActiveMute {
    /// Build from a stored mute row (node uuid + optional check name).
    #[must_use]
    pub fn new(node: Uuid, check_name: Option<&str>) -> Self {
        let node = NodeId::from(node);
        Self {
            node,
            check: check_name.map(|name| check_id(node, name)),
            metric: check_name.map(str::to_owned),
        }
    }
}

/// Whether an alert is covered by any active mute (separate fn for unit testing).
///
/// A mute names a node, so an alert with a non-node subject is never muted — a pool-coverage
/// alert cannot be silenced from the UI in this increment. That is a gap, not a decision: giving
/// a mute a pool target belongs with the rest of the scope-and-surface work (Increment 2).
///
/// # Why the metric is matched as well as the check id (ADR-076 decision 5)
///
/// A mute stores a *check name* and [`ActiveMute::new`] turns it into `check_id(node, name)` — the
/// **node-level** id. Since ADR-076 a per-interface metric's alerts carry
/// `interface_check_id(node, ifindex, name)` instead, so an id-only comparison would match none of
/// them: the operator picks `if_oper_status` from the metric picker (ADR-075 decision 18 put the
/// same picker on this form), saves, and the mute silently silences nothing. Matching the metric
/// name too makes a node-level mute cover **every port's** alerts for that metric, which is what
/// picking a per-interface metric on a node-scoped form plainly means.
///
/// ⚠️ Muting **one port** is still impossible: `api/maintenance.rs` validates `check_name` with
/// [`yagra_common::is_valid_metric_name`], which cannot express the `metric@ifindex` form. Written
/// down rather than worked around — the form has no port field to fill in either.
#[must_use]
fn mute_matches(mutes: &[ActiveMute], alert: &Alert) -> bool {
    let Some(node) = alert.node() else {
        return false;
    };
    mutes.iter().any(|m| {
        m.node == node
            && match (&m.metric, m.check) {
                // A mute with no check name covers the whole node, as it always has.
                (None, _) => true,
                (Some(metric), check) => {
                    check.is_some_and(|c| c == alert.check) || *metric == alert.metric
                }
            }
    })
}

/// A channel's notification-template override plus the one thing rendering needs to know about
/// the channel itself (ADR-039).
///
/// Held next to the dispatchers rather than inside the built [`NotifyChannel`] so a template edit
/// takes effect on the next routing refresh without rebuilding the channel — which would reset its
/// dedup state and re-page every active alert.
struct ChannelOverride {
    template: ChannelTemplate,
    /// Whether this channel carries the body as JSON (webhook/PagerDuty) — see
    /// [`crate::notify_render::body_must_be_json`].
    needs_json: bool,
}

/// The live routing snapshot: the always-on env default route, the DB-configured channels
/// (each with its own dedup+retry dispatcher), and the rules that select channels per alert.
struct Routes {
    /// Env-configured channels (`YAGRA_WEBHOOK_URL`/`YAGRA_SMTP_*`) — fire for *every* alert,
    /// preserving the pre-routing behaviour. `None` if no env channel is set.
    ///
    /// **Always the built-in format.** It has no channel id and no database row, so there is
    /// nothing for a per-channel override to hang off (ADR-039 decision 1); a deployment that
    /// wants templated notifications configures a channel in the UI.
    default: Option<Dispatcher<MultiChannel>>,
    /// DB channels by id, each with its own dedup state (preserved across config refresh).
    channels: HashMap<Uuid, Dispatcher<Arc<dyn NotifyChannel>>>,
    /// Per-channel template overrides, for the channels that have one. Absent = built-in format.
    overrides: HashMap<Uuid, ChannelOverride>,
    /// Routing rules (severity → channel ids).
    rules: Vec<RoutingRule>,
    /// Unexpired mutes — matching alerts are not delivered (UI/history unaffected).
    mutes: Vec<ActiveMute>,
}

/// Forwards alert lifecycle to the configured channels with the engine's dedup + retry
/// (ADR-015). Channels + rules come from the database (refreshed periodically via
/// [`Self::set_routing`]); env channels remain an always-on default route.
pub struct Notifier {
    routes: tokio::sync::Mutex<Routes>,
    /// Resolves node names/group/profile for a template's context (ADR-039). `None` in skeleton
    /// mode and before startup wiring, in which case a template sees ids instead of names.
    facts: RwLock<Option<Arc<dyn AlertFactsSource>>>,
    /// Whether *any* channel currently has a template.
    ///
    /// Read without the routing lock so that a deployment with no templates — which is every
    /// deployment until someone writes one — does exactly what it did before this feature landed,
    /// including issuing no extra query to resolve names nobody is going to interpolate.
    any_templates: AtomicBool,
}

impl Notifier {
    /// Build a notifier with the env default route (a Webhook via `YAGRA_WEBHOOK_URL` and/or
    /// email via `YAGRA_SMTP_*`). DB channels/rules are layered on later via `set_routing`.
    #[must_use]
    pub fn from_env() -> Self {
        let mut channels: Vec<Box<dyn NotifyChannel>> = Vec::new();
        if let Ok(url) = std::env::var("YAGRA_WEBHOOK_URL") {
            if !url.is_empty() {
                channels.push(Box::new(WebhookChannel::new(url)));
            }
        }
        if let Some(email) = EmailChannel::from_env() {
            channels.push(Box::new(email));
        }
        let default = (!channels.is_empty()).then(|| {
            tracing::info!(
                channels = channels.len(),
                "alert notifier default route enabled"
            );
            Dispatcher::new(MultiChannel { channels }, RetryPolicy::default())
        });
        Self {
            routes: tokio::sync::Mutex::new(Routes {
                default,
                channels: HashMap::new(),
                overrides: HashMap::new(),
                rules: Vec::new(),
                mutes: Vec::new(),
            }),
            facts: RwLock::new(None),
            any_templates: AtomicBool::new(false),
        }
    }

    /// Attach the source that resolves node names/group/profile for a template's context
    /// (ADR-039). Called once at startup; a core with no write side never calls it and its
    /// templates render ids instead of names.
    pub fn set_facts_source(&self, source: Arc<dyn AlertFactsSource>) {
        *self.facts.write().expect("notifier facts lock poisoned") = Some(source);
    }

    /// Replace the DB routing snapshot. Channels that still exist keep their dispatcher (so the
    /// periodic refresh doesn't reset dedup and re-page active alerts); new channels get a
    /// fresh dispatcher; removed channels are dropped.
    ///
    /// A channel's **connection config** is treated as immutable — changing it means delete +
    /// recreate, because the live channel object is what holds it. Its **notification template**
    /// is not: it lives beside the dispatcher rather than inside the channel, so it is replaced
    /// wholesale here and an edit takes effect on the next refresh with no restart and without
    /// resetting dedup (ADR-039).
    pub async fn set_routing(&self, channels: Vec<OpenChannel>, rules: Vec<RoutingRule>) {
        let mut routes = self.routes.lock().await;
        let mut old = std::mem::take(&mut routes.channels);
        let mut next = HashMap::new();
        let mut overrides = HashMap::new();
        for ch in channels {
            if !ch.template.is_builtin() {
                overrides.insert(
                    ch.id,
                    ChannelOverride {
                        needs_json: body_must_be_json(ch.config.kind()),
                        template: ch.template,
                    },
                );
            }
            if let Some(disp) = old.remove(&ch.id) {
                next.insert(ch.id, disp); // preserve dedup
            } else if let Some(channel) = build_channel(&ch.config) {
                next.insert(ch.id, Dispatcher::new(channel, RetryPolicy::default()));
            }
        }
        // Only keep an override for a channel that actually has a live dispatcher, so the flag
        // below cannot be set by a channel whose config failed to build.
        overrides.retain(|id, _| next.contains_key(id));
        self.any_templates
            .store(!overrides.is_empty(), Ordering::Relaxed);
        routes.channels = next;
        routes.overrides = overrides;
        routes.rules = rules;
    }

    /// Replace the unexpired-mute snapshot (refreshed alongside routing).
    pub async fn set_mutes(&self, mutes: Vec<ActiveMute>) {
        self.routes.lock().await.mutes = mutes;
    }

    /// Resolve the template context for an alert, or `None` when no channel has a template.
    ///
    /// Deliberately **before** the routing lock is taken: this is the one part of delivery that
    /// touches the database, and holding the lock across it would add a query to the window that
    /// already serializes every notification.
    async fn context(&self, alert: &Alert, event: NotifyEvent) -> Option<AlertFacts> {
        if !self.any_templates.load(Ordering::Relaxed) {
            return None;
        }
        // Every subject renders through a template now. The vocabulary carries `subject_kind` and
        // an always-present `subject_name` so a template can read correctly for both kinds; a
        // template written before those existed still renders, because `node_id`/`node_name` fall
        // back to the subject's own identifier rather than to a nil UUID (`notify_facts`).
        let source = self
            .facts
            .read()
            .expect("notifier facts lock poisoned")
            .clone();
        let resolved = match source {
            Some(src) => src.facts(&node_ids_for(alert)).await,
            None => HashMap::new(),
        };
        Some(context_for(alert, event, &resolved))
    }

    /// Apply one notify action (deliver a fire, or resolve/clear a recovered alert).
    ///
    /// The `routes` mutex is held across delivery (including PagerDuty/JSM resolve
    /// requests with retry/backoff). This serializes all delivery — a wedged vendor
    /// endpoint can delay other notifications for up to the retry budget. This matches
    /// the pre-existing `Fire` path (which has always dispatched under this lock) and is
    /// an accepted tradeoff for keeping per-channel dedup state consistent; decoupling
    /// delivery from the routing snapshot is a future refactor.
    pub async fn handle(&self, action: NotifyAction) {
        // Resolving names is I/O, so it happens outside the lock. A muted or rolled-up alert pays
        // for a lookup it will not use, which the facts cache makes negligible and which is worth
        // not restructuring the suppression checks around.
        let facts = match &action {
            NotifyAction::Fire(a) => self.context(a, NotifyEvent::Fire).await,
            NotifyAction::Resolve(a) => self.context(a, NotifyEvent::Resolve).await,
            NotifyAction::Suppress(a) => self.context(a, NotifyEvent::Suppress).await,
        };
        let mut routes = self.routes.lock().await;
        match action {
            NotifyAction::Fire(alert) => {
                // Suppressed downstream alert: it's attributed to an upstream root cause and
                // rolled into that incident, so we don't page for it separately (the root
                // cause's own alert — root_cause: None — is what notifies). It still fired
                // for the UI/history; only the duplicate notification is suppressed.
                if let Some(root) = alert.root_cause {
                    tracing::debug!(subject = %alert.subject, %root, "suppressing downstream alert notification (rolled up under root cause)");
                    return;
                }
                // Muted: the operator asked for silence on this node/check until the mute
                // expires. The alert itself stays live in the UI/history.
                if mute_matches(&routes.mutes, &alert) {
                    tracing::debug!(subject = %alert.subject, "suppressing muted alert notification");
                    return;
                }
                let notification = builtin_notification(&alert, NotifyEvent::Fire);

                // Channels selected by the routing rules (severity match; None = any).
                let matched: BTreeSet<Uuid> = routes
                    .rules
                    .iter()
                    .filter(|r| r.enabled && rule_matches_severity(r.severity, alert.severity))
                    .flat_map(|r| r.channel_ids.iter().copied())
                    .collect();

                let Routes {
                    default,
                    channels,
                    overrides,
                    ..
                } = &mut *routes;
                if let Some(d) = default.as_mut() {
                    let outcome = d.dispatch(notification.clone()).await;
                    tracing::info!(?outcome, subject = %alert.subject, route = "default", "alert notification dispatched");
                }
                for id in matched {
                    if let Some(d) = channels.get_mut(&id) {
                        let n = for_channel(id, overrides, facts.as_ref(), &notification);
                        let outcome = d.dispatch(n).await;
                        tracing::info!(?outcome, subject = %alert.subject, channel = %id, "alert notification dispatched");
                    }
                }
            }
            NotifyAction::Resolve(alert) => {
                let key = alert.dedup_key();
                // A root-cause-suppressed alert never delivered its fire, so there is no
                // remote incident to close — just clear local dedup (mirror of the fire path).
                if alert.root_cause.is_some() {
                    if let Some(d) = routes.default.as_mut() {
                        d.mark_resolved(&key);
                    }
                    for d in routes.channels.values_mut() {
                        d.mark_resolved(&key);
                    }
                    return;
                }
                // Deliver the resolve to the same channels the fire was routed to (same
                // severity match) so lifecycle-aware channels (PagerDuty/JSM) close their
                // incident; webhook/email keep their no-op default. Deliberately NOT
                // mute-filtered: a mute placed after the fire must not leave a remote
                // incident dangling open (vendor resolves are idempotent).
                let notification = builtin_notification(&alert, NotifyEvent::Resolve);

                let matched: BTreeSet<Uuid> = routes
                    .rules
                    .iter()
                    .filter(|r| r.enabled && rule_matches_severity(r.severity, alert.severity))
                    .flat_map(|r| r.channel_ids.iter().copied())
                    .collect();

                let Routes {
                    default,
                    channels,
                    overrides,
                    ..
                } = &mut *routes;
                if let Some(d) = default.as_mut() {
                    let outcome = d.dispatch_resolve(notification.clone()).await;
                    tracing::info!(?outcome, subject = %alert.subject, route = "default", "alert resolve dispatched");
                }
                let ids: Vec<Uuid> = channels.keys().copied().collect();
                for id in ids {
                    if let Some(d) = channels.get_mut(&id) {
                        if matched.contains(&id) {
                            let n = for_channel(id, overrides, facts.as_ref(), &notification);
                            let outcome = d.dispatch_resolve(n).await;
                            tracing::info!(?outcome, subject = %alert.subject, channel = %id, "alert resolve dispatched");
                        } else {
                            d.mark_resolved(&key);
                        }
                    }
                }
            }
            NotifyAction::Suppress(alert) => {
                // A downstream alert that had been paging standalone is now rolled up under its
                // upstream root cause: close its remote incident so on-call isn't left with a
                // separate open page. Mirrors the (non-root-cause) resolve close path — the alert
                // itself stays live in the UI grouped under the root cause. Vendor resolves are
                // idempotent, so a repeat close is harmless.
                let key = alert.dedup_key();
                let notification = builtin_notification(&alert, NotifyEvent::Suppress);

                let matched: BTreeSet<Uuid> = routes
                    .rules
                    .iter()
                    .filter(|r| r.enabled && rule_matches_severity(r.severity, alert.severity))
                    .flat_map(|r| r.channel_ids.iter().copied())
                    .collect();

                let Routes {
                    default,
                    channels,
                    overrides,
                    ..
                } = &mut *routes;
                if let Some(d) = default.as_mut() {
                    let outcome = d.dispatch_resolve(notification.clone()).await;
                    tracing::info!(?outcome, subject = %alert.subject, route = "default", "downstream alert rolled up (incident closed)");
                }
                let ids: Vec<Uuid> = channels.keys().copied().collect();
                for id in ids {
                    if let Some(d) = channels.get_mut(&id) {
                        if matched.contains(&id) {
                            let n = for_channel(id, overrides, facts.as_ref(), &notification);
                            let outcome = d.dispatch_resolve(n).await;
                            tracing::info!(?outcome, subject = %alert.subject, channel = %id, "downstream alert rolled up (incident closed)");
                        } else {
                            d.mark_resolved(&key);
                        }
                    }
                }
            }
        }
    }
}

/// The notification Yagra sends when a channel has no template — and the fallback when its
/// template cannot be used.
///
/// **Deliberately a `format!` and not a built-in template** (ADR-039 decision 3). This is what
/// every failure path lands on, so it must not depend on the machinery that just failed. It is
/// also the reason the wording lives in exactly one place: the three lifecycle points used to
/// spell it out at three separate call sites inside `handle`, which is how two of them would
/// eventually stop agreeing.
pub(crate) fn builtin_notification(alert: &Alert, event: NotifyEvent) -> Notification {
    let summary = match (&alert.subject, event) {
        (Subject::Node(node), NotifyEvent::Fire) => format!("node {node} is {}", alert.state),
        (Subject::Node(node), NotifyEvent::Resolve) => format!("resolved: node {node} recovered"),
        (Subject::Node(node), NotifyEvent::Suppress) => {
            format!("rolled up: node {node} suppressed under upstream")
        }
        (Subject::Pool(pool), NotifyEvent::Fire) => {
            format!("poller pool \"{pool}\" has no live poller — its nodes are not being monitored")
        }
        (Subject::Pool(pool), NotifyEvent::Resolve) => {
            format!("resolved: poller pool \"{pool}\" has a live poller again")
        }
        // Unreachable today — a pool alert is raised through `raise_event_alert`, which sets
        // `root_cause: None`, and the dependency graph a roll-up walks is a graph of nodes. Spelled
        // out anyway so a future suppression path cannot silently emit node-shaped wording.
        (Subject::Pool(pool), NotifyEvent::Suppress) => {
            format!("rolled up: poller pool \"{pool}\" suppressed")
        }
    };
    let payload = serde_json::to_string(alert).unwrap_or_else(|_| "{}".to_owned());
    Notification::for_alert(alert, summary, payload)
}

/// Counter for a template that could not be used and fell back to the built-in format (ADR-039).
///
/// The `reason` label is the point: `compile` means a template stored before it could be validated,
/// `render` a runtime failure, `too_large` an output past the cap, and `not_json` a body a JSON
/// channel would have mangled. They send an operator to four different places.
const M_TEMPLATE_ERR: &str = "yagra_notification_template_errors_total";

/// What one channel actually receives: its template's output, or — if it has no template, or the
/// template could not be used — the built-in text unchanged.
///
/// **This function cannot fail.** A template is operator-authored text that runs for the first time
/// during an outage; letting a mistake in it swallow the page would make the feature worse than not
/// having it (ADR-039 decision 5). Fallback is per field, so a typo in the body does not also
/// discard a subject that was written correctly.
fn for_channel(
    channel: Uuid,
    overrides: &HashMap<Uuid, ChannelOverride>,
    facts: Option<&AlertFacts>,
    builtin: &Notification,
) -> Notification {
    let (Some(over), Some(facts)) = (overrides.get(&channel), facts) else {
        return builtin.clone();
    };
    let rendered = render_with_fallback(
        Some(&over.template),
        facts,
        over.needs_json,
        &builtin.summary,
        &builtin.payload,
    );
    for failure in &rendered.failures {
        metrics::counter!(M_TEMPLATE_ERR, "reason" => failure.kind.as_str()).increment(1);
        tracing::warn!(
            channel = %channel,
            field = failure.field.as_str(),
            reason = failure.kind.as_str(),
            detail = %failure.message,
            "notification template unusable; sent the built-in format instead"
        );
    }
    Notification {
        summary: rendered.subject,
        payload: rendered.body,
        ..builtin.clone()
    }
}

/// Match a routing rule's severity against an alert's (separate fn for unit testing).
#[must_use]
fn rule_matches_severity(rule_severity: Option<Severity>, alert_severity: Severity) -> bool {
    rule_severity.is_none_or(|s| s == alert_severity)
}

#[cfg(test)]
mod template_tests {
    use super::*;
    use crate::notify_facts::tests::threshold_alert;
    use crate::notify_render::FailureKind;
    use yagra_common::{sample_facts, NodeId};

    fn over(subject: Option<&str>, body: Option<&str>, needs_json: bool) -> ChannelOverride {
        ChannelOverride {
            template: ChannelTemplate {
                subject: subject.map(str::to_owned),
                body: body.map(str::to_owned),
            },
            needs_json,
        }
    }

    /// The exact text every deployment receives today. **A change here is a change to every
    /// operator's inbox**, so it is pinned rather than described: the whole N-1 story of ADR-039
    /// is that a channel with no template sends what it sent before, byte for byte.
    #[test]
    fn the_built_in_wording_is_unchanged_for_every_lifecycle_point() {
        let node = NodeId::new();
        let alert = threshold_alert(node);
        for (event, want) in [
            (NotifyEvent::Fire, format!("node {node} is critical")),
            (
                NotifyEvent::Resolve,
                format!("resolved: node {node} recovered"),
            ),
            (
                NotifyEvent::Suppress,
                format!("rolled up: node {node} suppressed under upstream"),
            ),
        ] {
            let n = builtin_notification(&alert, event);
            assert_eq!(n.summary, want);
            // The payload has always been the whole alert as JSON.
            assert_eq!(n.payload, serde_json::to_string(&alert).unwrap());
            assert_eq!(n.dedup_key, alert.dedup_key());
            assert_eq!(n.severity, alert.severity);
        }
    }

    /// A channel with no override gets the built-in notification untouched — same object, not a
    /// re-render that happens to agree.
    #[test]
    fn a_channel_without_a_template_receives_the_built_in_notification() {
        let id = Uuid::new_v4();
        let builtin = builtin_notification(&threshold_alert(NodeId::new()), NotifyEvent::Fire);
        let facts = sample_facts(NotifyEvent::Fire);
        assert_eq!(
            for_channel(id, &HashMap::new(), Some(&facts), &builtin),
            builtin
        );
    }

    #[test]
    fn a_template_replaces_the_subject_and_body_for_that_channel_only() {
        let templated = Uuid::new_v4();
        let plain = Uuid::new_v4();
        let mut overrides = HashMap::new();
        overrides.insert(
            templated,
            over(Some("{{ severity }} on {{ node_name }}"), None, false),
        );
        let builtin = builtin_notification(&threshold_alert(NodeId::new()), NotifyEvent::Fire);
        let facts = sample_facts(NotifyEvent::Fire);

        let a = for_channel(templated, &overrides, Some(&facts), &builtin);
        assert_eq!(a.summary, "critical on core-sw-01");
        assert_eq!(a.payload, builtin.payload, "the body was not overridden");

        let b = for_channel(plain, &overrides, Some(&facts), &builtin);
        assert_eq!(b, builtin, "one channel's template must not reach another");
    }

    /// The property the whole module is built around: a template that fails at render time costs
    /// the customisation, never the notification.
    #[test]
    fn a_failing_template_still_sends_the_built_in_text() {
        let id = Uuid::new_v4();
        let mut overrides = HashMap::new();
        overrides.insert(
            id,
            over(Some("{{ nope.attr }}"), Some("{{ also.bad }}"), false),
        );
        let builtin = builtin_notification(&threshold_alert(NodeId::new()), NotifyEvent::Fire);
        let out = for_channel(
            id,
            &overrides,
            Some(&sample_facts(NotifyEvent::Fire)),
            &builtin,
        );
        assert_eq!(out, builtin);
    }

    /// A JSON channel is the case where a "successful" render is still wrong: PagerDuty parses the
    /// body with `unwrap_or(Null)`, so an unescaped quote would page on-call with no detail.
    #[test]
    fn a_json_channel_rejects_a_body_that_is_not_json() {
        let id = Uuid::new_v4();
        let builtin = builtin_notification(&threshold_alert(NodeId::new()), NotifyEvent::Fire);
        let facts = sample_facts(NotifyEvent::Fire);

        let mut overrides = HashMap::new();
        overrides.insert(id, over(None, Some("{{ node_name }} is down"), true));
        assert_eq!(
            for_channel(id, &overrides, Some(&facts), &builtin).payload,
            builtin.payload
        );

        // The same template is fine where the body is plain text.
        let mut overrides = HashMap::new();
        overrides.insert(id, over(None, Some("{{ node_name }} is down"), false));
        assert_eq!(
            for_channel(id, &overrides, Some(&facts), &builtin).payload,
            "core-sw-01 is down"
        );
    }

    /// Without a facts source there is no context to render against, so the built-in text stands.
    /// A half-rendered notification full of blanks would be worse than the plain one.
    #[test]
    fn no_resolved_context_means_no_rendering() {
        let id = Uuid::new_v4();
        let mut overrides = HashMap::new();
        overrides.insert(id, over(Some("{{ node_name }}"), None, false));
        let builtin = builtin_notification(&threshold_alert(NodeId::new()), NotifyEvent::Fire);
        assert_eq!(for_channel(id, &overrides, None, &builtin), builtin);
    }

    /// Which channel kinds demand JSON is decided once, in `notify_render`, and read from there —
    /// `set_routing` must not grow a second opinion.
    #[test]
    fn the_json_rule_comes_from_the_channel_kind() {
        for (kind, want) in [
            (crate::notifications::ChannelKind::Webhook, true),
            (crate::notifications::ChannelKind::PagerDuty, true),
            (crate::notifications::ChannelKind::Jsm, false),
            (crate::notifications::ChannelKind::Email, false),
        ] {
            assert_eq!(body_must_be_json(kind), want);
        }
    }

    #[test]
    fn the_failure_reasons_are_the_metric_labels() {
        // Guards the label set the dashboards and the ADR name.
        assert_eq!(M_TEMPLATE_ERR, "yagra_notification_template_errors_total");
        assert_eq!(FailureKind::NotJson.as_str(), "not_json");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;
    use yagra_common::NodeId;
    use yagra_common::ThresholdBounds;

    /// The fleet-default liveness rule every deployment is seeded with (ADR-075, `repo.rs`).
    ///
    /// Up/down alerting is rule-driven now, so a manager with no config commits state and pages
    /// nobody. Tests that exercise firing must install this; the ones that assert the opposite
    /// deliberately leave it out.
    fn liveness_rule() -> StoredThreshold {
        seeded_liveness_rule()
    }

    /// `AlertConfig::new` with the seeded liveness rule already in it — what a real deployment
    /// looks like. Take this rather than `AlertConfig::new` unless the test is about its absence.
    fn cfg(mut thresholds: Vec<StoredThreshold>, meta: HashMap<NodeId, NodeMeta>) -> AlertConfig {
        thresholds.push(liveness_rule());
        AlertConfig::new(thresholds, meta)
    }

    /// A manager configured the way a seeded deployment is.
    fn manager() -> AlertManager {
        let mgr = AlertManager::new();
        mgr.set_config(cfg(Vec::new(), HashMap::new()));
        mgr
    }

    fn result(node: NodeId, outcome: CheckOutcome, at: i64) -> PollResult {
        PollResult {
            job_id: Uuid::nil(),
            node_id: node,
            at_unix_ms: at,
            outcome,
            samples: Vec::new(),
            interfaces: Vec::new(),
            sys_descr: None,
            dns_chain: None,
            neighbors: None,
            l3: None,
            arp: None,
            routing: None,
            observational: false,
            poller_id: None,
            trace_context: Default::default(),
        }
    }

    #[test]
    fn routing_rule_severity_match() {
        // None severity ⇒ matches every alert severity.
        assert!(rule_matches_severity(None, Severity::Critical));
        assert!(rule_matches_severity(None, Severity::Warning));
        // A specific severity matches only that one.
        assert!(rule_matches_severity(
            Some(Severity::Critical),
            Severity::Critical
        ));
        assert!(!rule_matches_severity(
            Some(Severity::Critical),
            Severity::Warning
        ));
    }

    #[test]
    fn build_channel_makes_webhook() {
        let ch = build_channel(&ChannelConfig::Webhook {
            url: "http://example.test/hook".to_owned(),
        });
        assert!(ch.is_some());
    }

    #[test]
    fn build_channel_makes_pagerduty_and_jsm() {
        assert!(build_channel(&ChannelConfig::PagerDuty {
            routing_key: "rk".to_owned(),
            api_url: None,
        })
        .is_some());
        assert!(build_channel(&ChannelConfig::Jsm {
            api_url: "https://api.atlassian.com/jsm/ops/integration/v2".to_owned(),
            api_key: "key".to_owned(),
        })
        .is_some());
    }

    fn vendor_notification(severity: Severity) -> Notification {
        let alert = Alert {
            subject: Subject::Node(NodeId::from(Uuid::nil())),
            check: yagra_common::CheckId::from(Uuid::nil()),
            severity,
            state: NodeState::Critical,
            at_unix_ms: 1,
            root_cause: None,
            flapping: false,
            metric: "event:test".to_owned(),
            breach: None,
            ifindex: None,
        };
        Notification::for_alert(&alert, "node down", r#"{"metric":"event:test"}"#)
    }

    #[test]
    fn pagerduty_body_matches_events_v2_contract() {
        let n = vendor_notification(Severity::Critical);
        let body = pagerduty_body("rk-secret", "trigger", &n, true);
        assert_eq!(body["routing_key"], "rk-secret");
        assert_eq!(body["event_action"], "trigger");
        let dedup = body["dedup_key"].as_str().unwrap();
        assert!(dedup.starts_with("yagra:"));
        assert!(dedup.ends_with(":critical"));
        assert_eq!(body["payload"]["summary"], "node down");
        assert_eq!(body["payload"]["severity"], "critical");
        // custom_details is the parsed alert JSON, not a double-encoded string.
        assert_eq!(body["payload"]["custom_details"]["metric"], "event:test");

        // Resolve carries only the correlation fields (payload omitted).
        let resolve = pagerduty_body("rk-secret", "resolve", &n, false);
        assert_eq!(resolve["event_action"], "resolve");
        assert_eq!(resolve["dedup_key"], body["dedup_key"]);
        assert!(resolve.get("payload").is_none());
    }

    #[test]
    fn jsm_body_and_close_url_match_opsgenie_contract() {
        let n = vendor_notification(Severity::Warning);
        let body = jsm_create_body(&n);
        assert_eq!(body["message"], "node down");
        assert_eq!(body["priority"], "P3"); // warning → P3 (critical P1, info P5)
        assert_eq!(body["source"], "yagra");
        let alias = body["alias"].as_str().unwrap().to_owned();
        assert!(alias.starts_with("yagra:"));

        let url = jsm_close_url("https://api.atlassian.com/jsm/ops/integration/v2", &n);
        assert_eq!(
            url,
            format!(
                "https://api.atlassian.com/jsm/ops/integration/v2/alerts/{alias}/close?identifierType=alias"
            )
        );

        // Severity → priority mapping extremes.
        assert_eq!(
            jsm_create_body(&vendor_notification(Severity::Critical))["priority"],
            "P1"
        );
        assert_eq!(
            jsm_create_body(&vendor_notification(Severity::Info))["priority"],
            "P5"
        );

        // JSM's message field caps at 130 chars.
        let mut long = vendor_notification(Severity::Warning);
        long.summary = "x".repeat(500);
        assert_eq!(
            jsm_create_body(&long)["message"].as_str().unwrap().len(),
            130
        );
    }

    fn synth_response(status: u16, headers: &[(&str, &str)]) -> reqwest::Response {
        let mut builder = axum::http::Response::builder().status(status);
        for (k, v) in headers {
            builder = builder.header(*k, *v);
        }
        reqwest::Response::from(builder.body("").unwrap())
    }

    #[tokio::test]
    async fn vendor_response_handles_success_failure_429_and_extra_ok() {
        // 202 Accepted (both vendors' success status).
        assert!(vendor_response(synth_response(202, &[]), None)
            .await
            .is_ok());
        // Hard failure surfaces as a delivery error (dispatcher retries).
        assert!(vendor_response(synth_response(400, &[]), None)
            .await
            .is_err());
        // 429 waits out Retry-After then errs so the retry policy counts the attempt.
        let start = std::time::Instant::now();
        let r = vendor_response(synth_response(429, &[("retry-after", "0")]), None).await;
        assert!(r.is_err());
        assert!(start.elapsed() < std::time::Duration::from_secs(5));
        // JSM close treats 404 (already closed) as success — resolve stays idempotent.
        let ok404 = vendor_response(
            synth_response(404, &[]),
            Some(reqwest::StatusCode::NOT_FOUND),
        )
        .await;
        assert!(ok404.is_ok());
        assert!(vendor_response(synth_response(404, &[]), None)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn webhook_target_blocked_for_metadata_literal_allows_private() {
        async fn blocked(u: &str) -> bool {
            webhook_target_blocked(&reqwest::Url::parse(u).unwrap()).await
        }
        // SSRF-escalation surface (resolved before any request leaves core).
        assert!(blocked("http://169.254.169.254/hook").await);
        assert!(blocked("http://127.0.0.1/hook").await);
        assert!(blocked("http://[::ffff:169.254.169.254]/").await);
        // A legitimate internal (private-range) webhook stays allowed.
        assert!(!blocked("http://10.0.0.5/hook").await);
    }

    /// ADR-075, the half that is easy to get wrong: with no `__liveness__` rule the engine must
    /// still commit the node's state, keep the down-set current and run dependency suppression —
    /// only the paging stops. Deleting an *alert rule* is not a request to blank the Nodes page.
    #[test]
    fn without_a_liveness_rule_the_state_still_commits_and_nobody_is_paged() {
        let mgr = AlertManager::new();
        mgr.set_config(AlertConfig::new(Vec::new(), HashMap::new()));
        let node = NodeId::new();
        for i in 0..=i64::from(DEFAULT_LIVENESS_DWELL) {
            assert!(
                mgr.observe(&result(node, CheckOutcome::Unreachable, i))
                    .is_empty(),
                "no rule ⇒ no alert, at sample {i}"
            );
        }
        assert!(mgr.active_alerts().is_empty());
        // …but everything the UI and the suppression graph read is current.
        assert_eq!(mgr.node_state(node), Some(NodeState::Unreachable));
        assert!(mgr.down_set().contains(&node));
    }

    /// The receiving side of the same rule, which a rejection-only test would miss entirely: with
    /// the seeded rule installed the node fires exactly as it did before ADR-075.
    #[test]
    fn with_the_seeded_rule_a_down_node_fires_at_the_same_cadence_as_before() {
        let mgr = manager();
        let node = NodeId::new();
        for i in 0..(DEFAULT_LIVENESS_DWELL - 1) {
            assert!(mgr
                .observe(&result(node, CheckOutcome::Unreachable, i64::from(i)))
                .is_empty());
        }
        let actions = mgr.observe(&result(node, CheckOutcome::Unreachable, 100));
        assert!(matches!(actions.as_slice(), [NotifyAction::Fire(_)]));
        // The alert still carries the sentinel, which is what `check_id`, the dedup key, the
        // history rows and dependency suppression are all keyed on (ADR-075 decision 2).
        assert_eq!(mgr.active_alerts()[0].metric, LIVENESS);
    }

    /// Deleting the rule while an alert is open must close it. Without this the alert is stranded:
    /// active in the UI forever, and open in whatever external tool its dedup key reached, with no
    /// remaining code path that could ever resolve it.
    #[test]
    fn deleting_the_liveness_rule_resolves_the_alert_it_had_already_raised() {
        let mgr = manager();
        let node = NodeId::new();
        for i in 0..i64::from(DEFAULT_LIVENESS_DWELL) {
            mgr.observe(&result(node, CheckOutcome::Unreachable, i));
        }
        assert_eq!(mgr.active_alerts().len(), 1);

        mgr.set_config(AlertConfig::new(Vec::new(), HashMap::new()));
        let actions = mgr.observe(&result(node, CheckOutcome::Unreachable, 100));
        assert!(
            matches!(actions.as_slice(), [NotifyAction::Resolve(_)]),
            "the open alert is closed once, on the first poll after the rule went away"
        );
        assert!(mgr.active_alerts().is_empty());
        // Once, not on every subsequent poll — a resolve per poll would be a notification storm.
        assert!(mgr
            .observe(&result(node, CheckOutcome::Unreachable, 101))
            .is_empty());
    }

    /// The dwell is read off the rule, and an edit takes effect without a core restart. The check
    /// state outlives any one config snapshot, so this is the property that makes the number the
    /// UI shows the number the engine uses.
    #[test]
    fn the_liveness_dwell_comes_from_the_rule_and_an_edit_applies_live() {
        let with_dwell = |n: u32| {
            let mut r = liveness_rule();
            r.rule.dwell_samples = n;
            AlertConfig::new(vec![r], HashMap::new())
        };
        let mgr = AlertManager::new();
        mgr.set_config(with_dwell(1));
        let node = NodeId::new();
        let actions = mgr.observe(&result(node, CheckOutcome::Unreachable, 0));
        assert!(
            matches!(actions.as_slice(), [NotifyAction::Fire(_)]),
            "dwell 1 fires on the first failed poll"
        );

        // Recover, then widen the window on the live manager: the next two failures must not fire.
        mgr.observe(&result(node, CheckOutcome::Reachable, 1));
        assert!(mgr.active_alerts().is_empty());
        mgr.set_config(with_dwell(3));
        assert!(mgr
            .observe(&result(node, CheckOutcome::Unreachable, 2))
            .is_empty());
        assert!(mgr
            .observe(&result(node, CheckOutcome::Unreachable, 3))
            .is_empty());
        assert!(matches!(
            mgr.observe(&result(node, CheckOutcome::Unreachable, 4))
                .as_slice(),
            [NotifyAction::Fire(_)]
        ));
    }
    #[test]
    fn fires_after_dwell_then_resolves_on_recovery() {
        let mgr = manager();
        let node = NodeId::new();

        // Unreachable must persist DEFAULT_LIVENESS_DWELL times before it commits/fires.
        for i in 0..(DEFAULT_LIVENESS_DWELL - 1) {
            let actions = mgr.observe(&result(node, CheckOutcome::Unreachable, i64::from(i)));
            assert!(actions.is_empty(), "should not fire before dwell satisfied");
        }
        let actions = mgr.observe(&result(node, CheckOutcome::Unreachable, 100));
        assert!(matches!(actions.as_slice(), [NotifyAction::Fire(_)]));
        assert_eq!(mgr.active_alerts().len(), 1);

        // Recovery is symmetric: it also needs DEFAULT_LIVENESS_DWELL consecutive reachable
        // samples before the alert resolves (anti-flap on the way back too).
        for i in 0..(DEFAULT_LIVENESS_DWELL - 1) {
            let actions = mgr.observe(&result(node, CheckOutcome::Reachable, 200 + i64::from(i)));
            assert!(
                actions.is_empty(),
                "should not resolve before dwell satisfied"
            );
        }
        let actions = mgr.observe(&result(node, CheckOutcome::Reachable, 300));
        assert!(matches!(actions.as_slice(), [NotifyAction::Resolve(_)]));
        assert!(mgr.active_alerts().is_empty());
    }

    #[test]
    fn observe_broadcasts_node_state_changes_only() {
        // S14: the node-state SSE stream carries one event per rolled-up display-state change
        // (including the first observation), and nothing while the state is steady.
        let mgr = manager();
        let node = NodeId::new();
        let mut rx = mgr.subscribe_node_states();

        // First observation commits Ok and emits the initial node-state event.
        mgr.observe(&result(node, CheckOutcome::Reachable, 0));
        let (who, ev) = rx.try_recv().expect("first observe emits state");
        assert_eq!(
            who,
            Subject::Node(node),
            "the frame names the node it concerns"
        );
        assert!(ev.contains("\"ok\""), "state ok in payload: {ev}");
        assert!(
            ev.contains(&node.as_uuid().to_string()),
            "carries node id: {ev}"
        );

        // Steady Ok: no state change ⇒ no further events.
        mgr.observe(&result(node, CheckOutcome::Reachable, 1));
        assert!(rx.try_recv().is_err(), "steady Ok must not emit");

        // Drive Unreachable up to the dwell threshold; only the committing observe changes state.
        for i in 0..(DEFAULT_LIVENESS_DWELL - 1) {
            mgr.observe(&result(node, CheckOutcome::Unreachable, 10 + i64::from(i)));
            assert!(rx.try_recv().is_err(), "pre-dwell must not emit");
        }
        mgr.observe(&result(node, CheckOutcome::Unreachable, 100));
        let (_, ev) = rx.try_recv().expect("dwell-crossing observe emits");
        assert!(ev.contains("\"unreachable\""), "state unreachable: {ev}");
        assert!(rx.try_recv().is_err(), "exactly one event per real change");
    }

    // ⚠️ The guard for the rename described in the `NodeMeta` docs. `tag_groups` (threshold scope)
    // and `folder_group` (RBAC visibility) are different facts about a node, and `Scope::allows`
    // takes a `BTreeSet<String>` — so wiring visibility to the tag set compiles and runs. This
    // asserts the two stay independent: a node can carry tags and no folder, or a folder and no
    // tags, and neither may be read as the other.
    #[test]
    fn node_meta_group_is_the_folder_group_not_a_tag_value() {
        let node = NodeId::new();
        let folder = Uuid::from_u128(42);
        let mut meta = HashMap::new();
        meta.insert(
            node,
            NodeMeta {
                profile: None,
                tag_groups: BTreeSet::from(["tokyo".to_owned()]),
                folder_group: Some(folder),
                folder_chain: vec![folder],
            },
        );
        let mgr = manager();
        mgr.set_config(cfg(Vec::new(), meta));

        // The folder group is what visibility reads, and it is a uuid — never the tag string.
        assert_eq!(mgr.node_folder_group(node), Some(folder));
        // A node the snapshot has never seen resolves to `None` (⇒ invisible to a scoped caller),
        // and so does a node carrying tags but sitting in no folder.
        assert_eq!(mgr.node_folder_group(NodeId::new()), None);

        let mut tagged_only = HashMap::new();
        tagged_only.insert(
            node,
            NodeMeta {
                profile: None,
                tag_groups: BTreeSet::from(["tokyo".to_owned()]),
                folder_group: None,
                folder_chain: Vec::new(),
            },
        );
        let mgr2 = manager();
        mgr2.set_config(cfg(Vec::new(), tagged_only));
        assert_eq!(
            mgr2.node_folder_group(node),
            None,
            "a tag value must never be read as a folder group"
        );
    }

    fn folder_rule(group: Uuid, warning: f64) -> StoredThreshold {
        StoredThreshold::new(
            Uuid::from_u128(u128::from(warning as u64) + 1),
            ScopeLevel::FolderGroup,
            vec![group.to_string()],
            yagra_common::ThresholdRule::new(
                "cpu_util",
                yagra_common::ThresholdBounds::above(Some(warning), None),
                1,
            ),
        )
    }

    fn in_folder(node: NodeId, chain: Vec<Uuid>) -> HashMap<NodeId, NodeMeta> {
        let mut meta = HashMap::new();
        meta.insert(
            node,
            NodeMeta {
                folder_group: chain.first().copied(),
                folder_chain: chain,
                ..NodeMeta::default()
            },
        );
        meta
    }

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

    #[test]
    fn steady_reachable_never_fires() {
        let mgr = manager();
        let node = NodeId::new();
        for i in 0..10 {
            assert!(mgr
                .observe(&result(node, CheckOutcome::Reachable, i))
                .is_empty());
        }
        assert!(mgr.active_alerts().is_empty());
    }

    #[test]
    fn node_threshold_breach_fires_metric_alert() {
        use yagra_bus::Sample;
        use yagra_common::{ThresholdBounds, ThresholdRule};

        let node = NodeId::new();
        let mgr = manager();
        // Node-scoped threshold: icmp_rtt_ms critical at/above 100ms, no dwell.
        let mut meta = HashMap::new();
        meta.insert(node, NodeMeta::default());
        mgr.set_config(cfg(
            vec![StoredThreshold::new(
                Uuid::nil(),
                ScopeLevel::Node,
                vec![node.to_string()],
                ThresholdRule::new(
                    "icmp_rtt_ms",
                    ThresholdBounds::above(Some(50.0), Some(100.0)),
                    1,
                ),
            )],
            meta,
        ));

        let mut reachable_high = result(node, CheckOutcome::Reachable, 0);
        reachable_high.samples = vec![Sample::gauge("icmp_rtt_ms", 150.0)];
        let actions = mgr.observe(&reachable_high);
        // Reachable ⇒ no liveness alert; rtt 150 ≥ 100 ⇒ one critical metric alert.
        assert!(matches!(actions.as_slice(), [NotifyAction::Fire(_)]));
        let alerts = mgr.active_alerts();
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].state, NodeState::Critical);

        // Back under threshold ⇒ resolves.
        let mut reachable_ok = result(node, CheckOutcome::Reachable, 1_000);
        reachable_ok.samples = vec![Sample::gauge("icmp_rtt_ms", 5.0)];
        let actions = mgr.observe(&reachable_ok);
        assert!(matches!(actions.as_slice(), [NotifyAction::Resolve(_)]));
        assert!(mgr.active_alerts().is_empty());
    }

    /// An interface-scoped rule beats the node's, and applies to that port only (ADR-076).
    #[test]
    fn an_interface_rule_wins_on_its_port_and_nowhere_else() {
        use yagra_bus::Sample;
        use yagra_common::{
            interface_scope_id, IfIndex, MetricKind, ThresholdBounds, ThresholdRule,
        };

        let node = NodeId::new();
        let mgr = manager();
        let mut meta = HashMap::new();
        meta.insert(node, NodeMeta::default());

        let rule = |level: ScopeLevel, scope_id: String, critical: f64| {
            StoredThreshold::new(
                Uuid::new_v4(),
                level,
                vec![scope_id],
                ThresholdRule::new(
                    "if_in_util_pct",
                    ThresholdBounds::above(None, Some(critical)),
                    1,
                ),
            )
        };
        mgr.set_config(
            cfg(
                vec![
                    // The node says 90 for every port; port 7 is allowed to run hotter.
                    rule(ScopeLevel::Node, node.to_string(), 90.0),
                    rule(
                        ScopeLevel::Interface,
                        interface_scope_id(node.as_uuid(), 7),
                        99.0,
                    ),
                ],
                meta,
            )
            .with_per_interface(["if_in_util_pct".to_owned()].into_iter().collect()),
        );

        // 95% on both ports: port 8 breaches the node rule, port 7 does not breach its own.
        // Note this is *looser* than the node rule — most-specific-wins, not most-restrictive-wins,
        // is what makes an exception for one uplink expressible at all.
        let mut res = result(node, CheckOutcome::Reachable, 0);
        res.samples = vec![
            Sample::interface("if_in_util_pct", IfIndex(7), 95.0, MetricKind::Gauge),
            Sample::interface("if_in_util_pct", IfIndex(8), 95.0, MetricKind::Gauge),
        ];
        let actions = mgr.observe(&res);
        assert_eq!(actions.len(), 1, "only port 8 is over its own bound");
        let NotifyAction::Fire(alert) = &actions[0] else {
            panic!("expected a fire");
        };
        assert_eq!(alert.ifindex, Some(IfIndex(8)));

        // And port 7 does fire once it passes its own, looser bound.
        let mut res = result(node, CheckOutcome::Reachable, 1_000);
        res.samples = vec![Sample::interface(
            "if_in_util_pct",
            IfIndex(7),
            99.5,
            MetricKind::Gauge,
        )];
        let actions = mgr.observe(&res);
        assert_eq!(actions.len(), 1);
        let NotifyAction::Fire(alert) = &actions[0] else {
            panic!("expected a fire");
        };
        assert_eq!(alert.ifindex, Some(IfIndex(7)));
    }

    /// An interface rule must not leak onto a node-wide metric's check.
    #[test]
    fn an_interface_rule_never_applies_to_a_node_level_check() {
        use yagra_bus::Sample;
        use yagra_common::{interface_scope_id, ThresholdRule};

        let node = NodeId::new();
        let mgr = manager();
        let mut meta = HashMap::new();
        meta.insert(node, NodeMeta::default());
        // The metric is deliberately absent from the per-interface set, so its samples resolve
        // with no port — the interface rule then has nothing to match against.
        mgr.set_config(cfg(
            vec![StoredThreshold::new(
                Uuid::nil(),
                ScopeLevel::Interface,
                vec![interface_scope_id(node.as_uuid(), 7)],
                ThresholdRule::new("icmp_rtt_ms", ThresholdBounds::above(None, Some(1.0)), 1),
            )],
            meta,
        ));

        let mut res = result(node, CheckOutcome::Reachable, 0);
        res.samples = vec![Sample::gauge("icmp_rtt_ms", 999.0)];
        assert!(
            mgr.observe(&res).is_empty(),
            "a port-scoped rule must not fire on the node's own metric"
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

    /// A node-scoped mute silences every port's alerts for that metric (ADR-076 decision 5).
    ///
    /// Before this, `ActiveMute::new` built `check_id(node, name)` — the node-level id — so once
    /// per-interface alerts carried a per-port id, a mute created from the metric picker matched
    /// nothing at all. Silently: the operator saw the mute listed and kept being paged.
    #[test]
    fn a_node_mute_on_a_per_interface_metric_covers_every_port() {
        use yagra_common::IfIndex;

        let node = NodeId::new();
        let mute = ActiveMute::new(node.as_uuid(), Some("if_in_util_pct"));

        let port_alert = |idx: u32| Alert {
            subject: Subject::Node(node),
            check: interface_check_id(node, IfIndex(idx), "if_in_util_pct"),
            severity: Severity::Critical,
            state: NodeState::Critical,
            at_unix_ms: 0,
            root_cause: None,
            flapping: false,
            metric: "if_in_util_pct".to_owned(),
            breach: None,
            ifindex: Some(IfIndex(idx)),
        };
        assert!(mute_matches(std::slice::from_ref(&mute), &port_alert(7)));
        assert!(mute_matches(std::slice::from_ref(&mute), &port_alert(48)));

        // It must not spill onto a different metric on the same node.
        let other = Alert {
            metric: "icmp_rtt_ms".to_owned(),
            check: check_id(node, "icmp_rtt_ms"),
            ifindex: None,
            ..port_alert(7)
        };
        assert!(!mute_matches(std::slice::from_ref(&mute), &other));

        // Nor onto another node.
        let elsewhere = Alert {
            subject: Subject::Node(NodeId::new()),
            ..port_alert(7)
        };
        assert!(!mute_matches(std::slice::from_ref(&mute), &elsewhere));

        // A mute with no check name still covers the whole node, as it always did.
        let whole_node = ActiveMute::new(node.as_uuid(), None);
        assert!(mute_matches(
            std::slice::from_ref(&whole_node),
            &port_alert(7)
        ));
        assert!(mute_matches(std::slice::from_ref(&whole_node), &other));
    }

    /// The ADR-076 regression: a per-interface rule used to be **inert**, not merely coarse.
    ///
    /// Before the split, all 48 ports fed one `check_id(node, metric)`. A rule with a 3-sample
    /// dwell therefore saw `Ok, Ok, …, Critical, Ok, …` every poll, the dwell never reached three
    /// consecutive problem samples, and the alert **never fired at all** while the flap detector
    /// churned. Asserting "one port fires" is not enough on its own — assert the other 47 stay
    /// silent too, or a change that fired one alert per sample would also pass.
    #[test]
    fn one_breaching_port_among_many_fires_exactly_one_alert() {
        use yagra_bus::Sample;
        use yagra_common::{IfIndex, MetricKind, ThresholdRule};

        let node = NodeId::new();
        let mgr = manager();
        let mut meta = HashMap::new();
        meta.insert(node, NodeMeta::default());
        mgr.set_config(
            cfg(
                vec![StoredThreshold::new(
                    Uuid::nil(),
                    ScopeLevel::Node,
                    vec![node.to_string()],
                    ThresholdRule::new(
                        "if_oper_status",
                        // 1 = up, 2 = down: "not up" is `above 1.5`, not `below 0.5` — ifOperStatus
                        // never reports 0, so a `below` rule on it can never fire at all.
                        ThresholdBounds::above(None, Some(1.5)),
                        3,
                    ),
                )],
                meta,
            )
            .with_per_interface(["if_oper_status".to_owned()].into_iter().collect()),
        );

        // 48 ports; port 7 is down, the rest are up. Three polls, i.e. exactly the dwell.
        let mut fired = Vec::new();
        for (poll, at) in [0_i64, 1_000, 2_000].into_iter().enumerate() {
            let mut res = result(node, CheckOutcome::Reachable, at);
            res.samples = (1..=48)
                .map(|idx| {
                    let up = if idx == 7 { 2.0 } else { 1.0 };
                    Sample::interface("if_oper_status", IfIndex(idx), up, MetricKind::Gauge)
                })
                .collect();
            let actions = mgr.observe(&res);
            if poll < 2 {
                assert!(
                    actions.is_empty(),
                    "poll {poll} must not satisfy a three-sample dwell yet"
                );
            }
            fired.extend(actions);
        }

        // Exactly one alert, and it names the port that was actually down.
        assert_eq!(fired.len(), 1, "one down port must raise exactly one alert");
        let NotifyAction::Fire(alert) = &fired[0] else {
            panic!("expected a fire, got {:?}", fired[0]);
        };
        assert_eq!(alert.ifindex, Some(IfIndex(7)));
        assert_eq!(alert.metric, "if_oper_status");
        assert_eq!(
            alert.check,
            interface_check_id(node, IfIndex(7), "if_oper_status")
        );
        assert_eq!(mgr.active_alerts().len(), 1);

        // Port 7 coming back resolves exactly one alert and leaves nothing active.
        for at in [3_000_i64, 4_000, 5_000] {
            let mut res = result(node, CheckOutcome::Reachable, at);
            res.samples = (1..=48)
                .map(|idx| {
                    Sample::interface("if_oper_status", IfIndex(idx), 1.0, MetricKind::Gauge)
                })
                .collect();
            fired.extend(mgr.observe(&res));
        }
        assert_eq!(fired.len(), 2, "recovery must resolve exactly once");
        assert!(matches!(fired[1], NotifyAction::Resolve(_)));
        assert!(mgr.active_alerts().is_empty());
    }

    // ── ADR-077: node-wide metrics with several rows per poll ──────────────────────────────
    //
    // These use `Sample::interface` **without** `with_per_interface`, which is exactly what an
    // `entity` metric is: a table walk puts the row's last OID sub-identifier into the `ifindex`
    // label, but the catalogue does not call the metric per-interface, so the rows are CPUs,
    // sensors or battery lines rather than ports (ADR-011). All of them share one node-wide check.

    /// The ADR-077 regression, and the mirror of
    /// `one_breaching_port_among_many_fires_exactly_one_alert`.
    ///
    /// Before the fold, one hot CPU among fourteen idle ones had its dwell candidate reset by the
    /// very next sample in the same poll, so a 3-sample rule **never fired at all** —
    /// `huawei_cpu_usage` arrives 15 times per poll and `juniper_cpu_1min` 53 times.
    #[test]
    fn one_breaching_row_among_many_fires_after_the_dwell() {
        use yagra_bus::Sample;
        use yagra_common::{IfIndex, MetricKind, ThresholdRule};

        let node = NodeId::new();
        let mgr = manager();
        let mut meta = HashMap::new();
        meta.insert(node, NodeMeta::default());
        mgr.set_config(cfg(
            vec![StoredThreshold::new(
                Uuid::nil(),
                ScopeLevel::Global,
                Vec::new(),
                ThresholdRule::new(
                    "huawei_cpu_usage",
                    ThresholdBounds::above(Some(80.0), Some(90.0)),
                    3,
                ),
            )],
            meta,
        ));

        // 15 rows; row 4 is hot, the rest idle. Three polls — exactly the dwell.
        let mut fired = Vec::new();
        for (poll, at) in [0_i64, 1_000, 2_000].into_iter().enumerate() {
            let mut res = result(node, CheckOutcome::Reachable, at);
            res.samples = (1..=15)
                .map(|idx| {
                    let v = if idx == 4 { 95.0 } else { 10.0 };
                    Sample::interface("huawei_cpu_usage", IfIndex(idx), v, MetricKind::Gauge)
                })
                .collect();
            let actions = mgr.observe(&res);
            if poll < 2 {
                assert!(
                    actions.is_empty(),
                    "poll {poll} must not satisfy a three-sample dwell yet"
                );
            }
            fired.extend(actions);
        }

        // Exactly one alert — not one per row, and not none.
        assert_eq!(fired.len(), 1, "one hot row must raise exactly one alert");
        let NotifyAction::Fire(alert) = &fired[0] else {
            panic!("expected a fire, got {:?}", fired[0]);
        };
        assert_eq!(alert.metric, "huawei_cpu_usage");
        assert_eq!(alert.severity, Severity::Critical);
        // The check stays the node-wide id: folding changes how often a check is observed, never
        // which check it is. A per-row id would be a new identity no open alert could close.
        assert_eq!(alert.check, check_id(node, "huawei_cpu_usage"));
        assert_eq!(alert.ifindex, None);
        // The breach reports the row that actually breached, not whichever arrived last.
        assert_eq!(alert.breach.as_ref().map(|b| b.value), Some(95.0));
        assert_eq!(mgr.active_alerts().len(), 1);

        // The hot row cooling resolves it, once.
        for at in [3_000_i64, 4_000, 5_000] {
            let mut res = result(node, CheckOutcome::Reachable, at);
            res.samples = (1..=15)
                .map(|idx| {
                    Sample::interface("huawei_cpu_usage", IfIndex(idx), 10.0, MetricKind::Gauge)
                })
                .collect();
            fired.extend(mgr.observe(&res));
        }
        assert_eq!(fired.len(), 2, "recovery must resolve exactly once");
        assert!(matches!(fired[1], NotifyAction::Resolve(_)));
        assert!(mgr.active_alerts().is_empty());
    }

    /// The accepting half's opposite: a healthy fleet of rows stays silent.
    ///
    /// On its own this proves nothing — an engine that refused every sample would also pass it —
    /// which is why it sits beside the tests above and below that demand a fire.
    #[test]
    fn every_row_healthy_raises_nothing() {
        use yagra_bus::Sample;
        use yagra_common::{IfIndex, MetricKind, ThresholdRule};

        let node = NodeId::new();
        let mgr = manager();
        let mut meta = HashMap::new();
        meta.insert(node, NodeMeta::default());
        mgr.set_config(cfg(
            vec![StoredThreshold::new(
                Uuid::nil(),
                ScopeLevel::Global,
                Vec::new(),
                ThresholdRule::new(
                    "cisco_env_temp",
                    ThresholdBounds::above(Some(70.0), Some(80.0)),
                    2,
                ),
            )],
            meta,
        ));

        for at in [0_i64, 1_000, 2_000, 3_000] {
            let mut res = result(node, CheckOutcome::Reachable, at);
            res.samples = (1..=3)
                .map(|idx| {
                    Sample::interface("cisco_env_temp", IfIndex(idx), 54.0, MetricKind::Gauge)
                })
                .collect();
            assert!(mgr.observe(&res).is_empty());
        }
        assert!(mgr.active_alerts().is_empty());
    }

    /// The other direction of the same bug: N breaching rows must not satisfy an N-sample dwell
    /// inside **one** poll.
    ///
    /// Before the fold a 3-sample rule on a metric with three or more rows fired on the first poll,
    /// which is the dwell silently becoming "three rows" instead of "three polls" — the opposite
    /// failure to the inert one, and just as wrong.
    #[test]
    fn every_row_breaching_still_needs_the_whole_dwell() {
        use yagra_bus::Sample;
        use yagra_common::{IfIndex, MetricKind, ThresholdRule};

        let node = NodeId::new();
        let mgr = manager();
        let mut meta = HashMap::new();
        meta.insert(node, NodeMeta::default());
        mgr.set_config(cfg(
            vec![StoredThreshold::new(
                Uuid::nil(),
                ScopeLevel::Global,
                Vec::new(),
                ThresholdRule::new(
                    "juniper_temp",
                    ThresholdBounds::above(Some(70.0), Some(80.0)),
                    3,
                ),
            )],
            meta,
        ));

        let poll = |at: i64| {
            let mut res = result(node, CheckOutcome::Reachable, at);
            res.samples = (1..=12)
                .map(|idx| Sample::interface("juniper_temp", IfIndex(idx), 85.0, MetricKind::Gauge))
                .collect();
            res
        };

        assert!(
            mgr.observe(&poll(0)).is_empty(),
            "twelve breaching rows in one poll are one observation, not twelve"
        );
        assert!(mgr.observe(&poll(1_000)).is_empty());
        let fired = mgr.observe(&poll(2_000));
        assert_eq!(fired.len(), 1, "the third poll completes the dwell");
        assert!(matches!(fired[0], NotifyAction::Fire(_)));
    }

    /// A `below` rule must fold to the **minimum**, and this is the test that catches folding with
    /// `max` — which is what the rest of the product does.
    ///
    /// `query_metrics` collapses an entity metric to its maximum and its own response says the
    /// consequence out loud: where low is the fault, the maximum is the *healthiest* series. A UPS
    /// with one string at 15% and two at 80/90% is in trouble; folded with `max` it reports 90 and
    /// alerts on nothing.
    #[test]
    fn a_below_rule_folds_to_the_worst_row_not_the_healthiest() {
        use yagra_bus::Sample;
        use yagra_common::{IfIndex, MetricKind, ThresholdRule};

        let node = NodeId::new();
        let mgr = manager();
        let mut meta = HashMap::new();
        meta.insert(node, NodeMeta::default());
        mgr.set_config(cfg(
            vec![StoredThreshold::new(
                Uuid::nil(),
                ScopeLevel::Global,
                Vec::new(),
                ThresholdRule::new(
                    "ups_charge_remaining_pct",
                    ThresholdBounds::below(Some(50.0), Some(20.0)),
                    2,
                ),
            )],
            meta,
        ));

        // The depleted row is deliberately **first**, so "the last sample wins" also fails here.
        let poll = |at: i64| {
            let mut res = result(node, CheckOutcome::Reachable, at);
            res.samples = [15.0, 80.0, 90.0]
                .into_iter()
                .enumerate()
                .map(|(i, v)| {
                    #[allow(clippy::cast_possible_truncation)]
                    Sample::interface(
                        "ups_charge_remaining_pct",
                        IfIndex(i as u32 + 1),
                        v,
                        MetricKind::Gauge,
                    )
                })
                .collect();
            res
        };

        assert!(mgr.observe(&poll(0)).is_empty());
        let fired = mgr.observe(&poll(1_000));
        assert_eq!(fired.len(), 1, "the depleted row must fire");
        let NotifyAction::Fire(alert) = &fired[0] else {
            panic!("expected a fire, got {:?}", fired[0]);
        };
        assert_eq!(alert.severity, Severity::Critical);
        assert_eq!(
            alert.breach.as_ref().map(|b| b.value),
            Some(15.0),
            "folding a below rule with max would report 90.0 and alert on nothing"
        );
    }

    /// The regression that matters most: a metric with exactly one series must behave as it did
    /// before ADR-077, byte for byte.
    ///
    /// Every rule that shipped before this change — `snmp_up`, `icmp_loss_pct`, `http_up`, the
    /// three ADR-075 defaults — is single-series, so "the fold is transparent when there is nothing
    /// to fold" is what keeps an upgrade from changing how an existing fleet pages.
    #[test]
    fn a_single_series_metric_is_unchanged_by_the_fold() {
        use yagra_bus::Sample;
        use yagra_common::{ThresholdBounds, ThresholdRule};

        let node = NodeId::new();
        let mgr = manager();
        let mut meta = HashMap::new();
        meta.insert(node, NodeMeta::default());
        mgr.set_config(cfg(
            vec![StoredThreshold::new(
                Uuid::nil(),
                ScopeLevel::Global,
                Vec::new(),
                ThresholdRule::new(
                    yagra_common::METRIC_SNMP_UP,
                    ThresholdBounds::below(None, Some(0.5)),
                    2,
                ),
            )],
            meta,
        ));

        let poll = |at: i64, up: f64| {
            let mut res = result(node, CheckOutcome::Reachable, at);
            res.samples = vec![Sample::gauge(yagra_common::METRIC_SNMP_UP, up)];
            res
        };

        assert!(
            mgr.observe(&poll(0, 0.0)).is_empty(),
            "one sample, one dwell step"
        );
        let fired = mgr.observe(&poll(1_000, 0.0));
        assert_eq!(fired.len(), 1);
        let NotifyAction::Fire(alert) = &fired[0] else {
            panic!("expected a fire, got {:?}", fired[0]);
        };
        assert_eq!(alert.check, check_id(node, yagra_common::METRIC_SNMP_UP));
        assert_eq!(alert.ifindex, None);
        assert_eq!(alert.breach.as_ref().map(|b| b.value), Some(0.0));

        // And it recovers on the same cadence.
        assert!(mgr.observe(&poll(2_000, 1.0)).is_empty());
        let back = mgr.observe(&poll(3_000, 1.0));
        assert_eq!(back.len(), 1);
        assert!(matches!(back[0], NotifyAction::Resolve(_)));
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

    /// Two ports keep independent dwell windows, so one cannot commit on the other's samples.
    #[test]
    fn two_ports_on_one_node_are_two_independent_checks() {
        use yagra_bus::Sample;
        use yagra_common::{IfIndex, MetricKind, ThresholdRule};

        let node = NodeId::new();
        let mgr = manager();
        let mut meta = HashMap::new();
        meta.insert(node, NodeMeta::default());
        mgr.set_config(
            cfg(
                vec![StoredThreshold::new(
                    Uuid::nil(),
                    ScopeLevel::Node,
                    vec![node.to_string()],
                    ThresholdRule::new(
                        "if_in_util_pct",
                        ThresholdBounds::above(None, Some(90.0)),
                        2,
                    ),
                )],
                meta,
            )
            .with_per_interface(["if_in_util_pct".to_owned()].into_iter().collect()),
        );

        // Port 1 breaches on both polls; port 2 only on the second. Port 1 must commit (two
        // consecutive) and port 2 must not (one) — impossible if they shared a window.
        let mut res = result(node, CheckOutcome::Reachable, 0);
        res.samples = vec![
            Sample::interface("if_in_util_pct", IfIndex(1), 95.0, MetricKind::Gauge),
            Sample::interface("if_in_util_pct", IfIndex(2), 10.0, MetricKind::Gauge),
        ];
        assert!(mgr.observe(&res).is_empty());

        let mut res = result(node, CheckOutcome::Reachable, 1_000);
        res.samples = vec![
            Sample::interface("if_in_util_pct", IfIndex(1), 96.0, MetricKind::Gauge),
            Sample::interface("if_in_util_pct", IfIndex(2), 99.0, MetricKind::Gauge),
        ];
        let actions = mgr.observe(&res);
        assert_eq!(actions.len(), 1, "only port 1 has two consecutive breaches");
        let NotifyAction::Fire(alert) = &actions[0] else {
            panic!("expected a fire");
        };
        assert_eq!(alert.ifindex, Some(IfIndex(1)));
    }

    /// A metric the catalogue does not call per-interface keeps the node-level check, even when
    /// its samples carry an `ifindex` — the label is a row key, not a port number (ADR-011).
    #[test]
    fn a_row_key_that_is_not_a_port_does_not_split_the_check() {
        use yagra_bus::Sample;
        use yagra_common::{IfIndex, MetricKind, ThresholdRule};

        let node = NodeId::new();
        let mgr = manager();
        let mut meta = HashMap::new();
        meta.insert(node, NodeMeta::default());
        // Note the empty per-interface set: the catalogue says this metric is chassis-wide, so the
        // ifindex on its samples is an entPhysicalIndex or a CPU number, not a port.
        mgr.set_config(cfg(
            vec![StoredThreshold::new(
                Uuid::nil(),
                ScopeLevel::Node,
                vec![node.to_string()],
                ThresholdRule::new(
                    "cisco_env_temp",
                    ThresholdBounds::above(None, Some(70.0)),
                    2,
                ),
            )],
            meta,
        ));

        // Two "rows" breaching in one poll are ONE observation, not two (ADR-077). They share a
        // check, so they share a dwell window — and a two-sample dwell therefore means two polls.
        // This assertion used to read the other way: it demanded that two rows satisfy the dwell
        // inside a single poll, which is the bug ADR-077 names (the dwell quietly becoming "two
        // rows" instead of "two polls"). What the test is really for — that a chassis row key does
        // not split the check — is unchanged and still asserted below.
        let poll = |at: i64| {
            let mut res = result(node, CheckOutcome::Reachable, at);
            res.samples = vec![
                Sample::interface("cisco_env_temp", IfIndex(17), 80.0, MetricKind::Gauge),
                Sample::interface("cisco_env_temp", IfIndex(18), 81.0, MetricKind::Gauge),
            ];
            res
        };
        assert!(
            mgr.observe(&poll(0)).is_empty(),
            "two chassis rows in one poll are one observation, not two"
        );
        let actions = mgr.observe(&poll(1_000));
        assert_eq!(actions.len(), 1, "chassis rows share one check");
        let NotifyAction::Fire(alert) = &actions[0] else {
            panic!("expected a fire");
        };
        assert_eq!(alert.ifindex, None, "a chassis reading names no port");
        assert_eq!(alert.check, check_id(node, "cisco_env_temp"));
    }

    /// One metric's repeated samples in a single poll are **one** observation (ADR-077).
    ///
    /// This test used to assert the opposite, and its own comment explained why: `observe` memoizes
    /// the *resolution* per metric name, and deduplicating the observations too "would quietly turn
    /// a three-sample dwell into a three-*poll* dwell". That reasoning was inverted. A three-sample
    /// dwell was always meant to mean "the problem persisted across three polls"; counting rows
    /// made it mean "three rows breached at once", so a 48-port table satisfied any dwell instantly
    /// while a single bad row among good ones satisfied none of them, ever.
    ///
    /// The property the old test protected — that the memo must not collapse *distinct* checks —
    /// is still covered, by the per-port tests: ADR-076 gives each port its own check and therefore
    /// its own dwell window, which is where per-sample counting actually belongs.
    #[test]
    fn repeated_samples_of_one_metric_are_a_single_observation() {
        use yagra_bus::Sample;
        use yagra_common::{ThresholdBounds, ThresholdRule};

        let node = NodeId::new();
        let mgr = manager();
        let mut meta = HashMap::new();
        meta.insert(node, NodeMeta::default());
        mgr.set_config(cfg(
            vec![StoredThreshold::new(
                Uuid::nil(),
                ScopeLevel::Node,
                vec![node.to_string()],
                ThresholdRule::new("if_util_pct", ThresholdBounds::above(None, Some(90.0)), 3),
            )],
            meta,
        ));

        // Three breaching rows in ONE result: one observation, nowhere near a three-sample dwell.
        let mut three_rows = result(node, CheckOutcome::Reachable, 0);
        three_rows.samples = vec![
            Sample::gauge("if_util_pct", 95.0),
            Sample::gauge("if_util_pct", 97.0),
            Sample::gauge("if_util_pct", 99.0),
        ];
        assert!(
            mgr.observe(&three_rows).is_empty(),
            "three rows in one poll are one observation, not three"
        );

        // Two more polls complete the dwell — three polls, as the rule reads.
        let mut second = result(node, CheckOutcome::Reachable, 1_000);
        second.samples = vec![Sample::gauge("if_util_pct", 96.0)];
        assert!(mgr.observe(&second).is_empty());

        let mut third = result(node, CheckOutcome::Reachable, 2_000);
        third.samples = vec![Sample::gauge("if_util_pct", 98.0)];
        assert!(
            matches!(mgr.observe(&third).as_slice(), [NotifyAction::Fire(_)]),
            "the third poll of the metric commits the transition"
        );
    }

    #[test]
    fn counter_sample_never_fires_and_drains_a_latched_alert() {
        use yagra_bus::Sample;
        use yagra_common::{ThresholdBounds, ThresholdRule};

        let node = NodeId::new();
        let mgr = manager();
        // A rule that predates the create-side counter rejection: octets "above 1000".
        let mut meta = HashMap::new();
        meta.insert(node, NodeMeta::default());
        mgr.set_config(cfg(
            vec![StoredThreshold::new(
                Uuid::nil(),
                ScopeLevel::Node,
                vec![node.to_string()],
                ThresholdRule::new(
                    "if_hc_in_octets",
                    ThresholdBounds::above(None, Some(1000.0)),
                    1,
                ),
            )],
            meta,
        ));

        // Simulate the pre-fix latched alert: the same metric observed as a gauge breaches.
        let mut latched = result(node, CheckOutcome::Reachable, 0);
        latched.samples = vec![Sample::gauge("if_hc_in_octets", 5_000.0)];
        assert!(matches!(
            mgr.observe(&latched).as_slice(),
            [NotifyAction::Fire(_)]
        ));

        // A counter observation reads Ok at any magnitude — it resolves the latched alert
        // through the normal recovery path instead of firing or zombie-ing.
        let mut counter = result(node, CheckOutcome::Reachable, 1_000);
        counter.samples = vec![Sample::counter("if_hc_in_octets", 1.0e12)];
        assert!(matches!(
            mgr.observe(&counter).as_slice(),
            [NotifyAction::Resolve(_)]
        ));
        assert!(mgr.active_alerts().is_empty());

        // And it stays quiet from then on, monotonic growth and all.
        let mut counter2 = result(node, CheckOutcome::Reachable, 2_000);
        counter2.samples = vec![Sample::counter("if_hc_in_octets", 2.0e12)];
        assert!(mgr.observe(&counter2).is_empty());
    }

    #[test]
    fn fired_threshold_alert_carries_metric_and_breach() {
        use yagra_bus::Sample;
        use yagra_common::{Direction, ThresholdRule};

        let node = NodeId::new();
        let mgr = manager();
        let mut meta = HashMap::new();
        meta.insert(node, NodeMeta::default());
        mgr.set_config(cfg(
            vec![StoredThreshold::new(
                Uuid::nil(),
                ScopeLevel::Node,
                vec![node.to_string()],
                ThresholdRule::new(
                    "icmp_rtt_ms",
                    ThresholdBounds::above(Some(50.0), Some(100.0)),
                    1,
                ),
            )],
            meta,
        ));

        let mut high = result(node, CheckOutcome::Reachable, 0);
        high.samples = vec![Sample::gauge("icmp_rtt_ms", 150.0)];
        let action = mgr.observe(&high).into_iter().next().expect("one fire");
        let NotifyAction::Fire(alert) = action else {
            panic!("expected a fire");
        };
        // History must read *what* fired, not the opaque (node, metric) hash.
        assert_eq!(alert.metric, "icmp_rtt_ms");
        let breach = alert.breach.expect("threshold alert carries a breach");
        assert_eq!(breach.value, 150.0);
        assert_eq!(breach.threshold, Some(100.0)); // committed severity is critical
        assert_eq!(breach.direction, Direction::Above);
    }

    #[test]
    fn fired_liveness_alert_carries_sentinel_metric_and_no_breach() {
        let node = NodeId::new();
        let mgr = manager();
        // Drive unreachable past the dwell so liveness commits and fires.
        let mut fired = None;
        for i in 0..=i64::from(DEFAULT_LIVENESS_DWELL) {
            for action in mgr.observe(&result(node, CheckOutcome::Unreachable, i)) {
                if let NotifyAction::Fire(a) = action {
                    fired = Some(a);
                }
            }
        }
        let alert = fired.expect("liveness fire after dwell");
        assert_eq!(alert.metric, LIVENESS);
        assert!(alert.breach.is_none());
    }

    #[test]
    fn metric_without_threshold_is_ignored() {
        let node = NodeId::new();
        let mgr = manager();
        let mut r = result(node, CheckOutcome::Reachable, 0);
        r.samples = vec![yagra_bus::Sample::gauge("icmp_rtt_ms", 9999.0)];
        // No thresholds configured ⇒ no metric alert (and reachable ⇒ no liveness alert).
        assert!(mgr.observe(&r).is_empty());
    }

    #[test]
    fn node_states_reflect_liveness_and_threshold_rollup() {
        use yagra_bus::Sample;
        use yagra_common::{ThresholdBounds, ThresholdRule};

        let mgr = manager();
        let node = NodeId::new();

        // A node never observed has no rolled-up state.
        assert_eq!(mgr.node_state(node), None);

        // First reachable poll commits `ok` with no transition, but the inventory must still
        // read it as `ok` (the whole point of ① — surfacing the live state).
        assert!(mgr
            .observe(&result(node, CheckOutcome::Reachable, 0))
            .is_empty());
        assert_eq!(mgr.node_state(node), Some(NodeState::Ok));

        // A reachable node breaching a critical threshold rolls up to `critical` even though
        // its liveness is still `ok` underneath.
        let mut meta = HashMap::new();
        meta.insert(node, NodeMeta::default());
        mgr.set_config(cfg(
            vec![StoredThreshold::new(
                Uuid::nil(),
                ScopeLevel::Node,
                vec![node.to_string()],
                ThresholdRule::new(
                    "icmp_rtt_ms",
                    ThresholdBounds::above(Some(50.0), Some(100.0)),
                    1,
                ),
            )],
            meta,
        ));
        let mut high = result(node, CheckOutcome::Reachable, 1);
        high.samples = vec![Sample::gauge("icmp_rtt_ms", 150.0)];
        let _ = mgr.observe(&high);
        assert_eq!(mgr.node_state(node), Some(NodeState::Critical));
        let alerts = mgr.alerts_for(node);
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].state, NodeState::Critical);
    }

    #[test]
    fn node_state_counts_tally_the_whole_fleet_and_match_node_states() {
        // The fleet-summary source (S12): counts every observed node by rolled-up state, over the
        // whole engine — not a paged slice. Must agree with counting `node_states()` (its source).
        let mgr = manager();
        let up = NodeId::new();
        let down = NodeId::new();
        for i in 0..DEFAULT_LIVENESS_DWELL {
            mgr.observe(&result(up, CheckOutcome::Reachable, i64::from(i)));
            mgr.observe(&result(down, CheckOutcome::Unreachable, i64::from(i)));
        }
        let counts = mgr.node_state_counts();
        assert_eq!(counts.get(&NodeState::Ok).copied().unwrap_or(0), 1);
        assert_eq!(counts.get(&NodeState::Unreachable).copied().unwrap_or(0), 1);

        let mut manual: HashMap<NodeState, usize> = HashMap::new();
        for s in mgr.node_states().values() {
            *manual.entry(*s).or_insert(0) += 1;
        }
        assert_eq!(counts, manual, "summary tally must match node_states()");
    }

    #[test]
    fn maintenance_node_never_fires_and_existing_alert_resolves() {
        let mgr = manager();
        let node = NodeId::new();

        // Drive the node down until its liveness alert commits.
        let mut fired = false;
        for i in 0..DEFAULT_LIVENESS_DWELL {
            for action in mgr.observe(&result(node, CheckOutcome::Unreachable, i64::from(i))) {
                if matches!(action, NotifyAction::Fire(_)) {
                    fired = true;
                }
            }
        }
        assert!(fired);
        assert_eq!(mgr.active_alerts().len(), 1);

        // The node enters a maintenance window: the active alert resolves after dwell and
        // the display state flips to `maintenance`.
        let mut maintenance = BTreeSet::new();
        maintenance.insert(node);
        mgr.set_config(cfg(Vec::new(), HashMap::new()).with_maintenance(maintenance));
        let mut resolved = false;
        for i in 0..DEFAULT_LIVENESS_DWELL {
            for action in mgr.observe(&result(node, CheckOutcome::Unreachable, 100 + i64::from(i)))
            {
                if matches!(action, NotifyAction::Resolve(_)) {
                    resolved = true;
                }
            }
        }
        assert!(resolved, "entering maintenance should resolve the alert");
        assert!(mgr.active_alerts().is_empty());
        assert_eq!(mgr.node_state(node), Some(NodeState::Maintenance));

        // Still down while in maintenance ⇒ no new alert can fire.
        for i in 0..10 {
            assert!(mgr
                .observe(&result(node, CheckOutcome::Unreachable, 200 + i))
                .is_empty());
        }

        // The window ends: the real (down) state flows again and re-commits after dwell.
        mgr.set_config(cfg(Vec::new(), HashMap::new()));
        let mut refired = false;
        for i in 0..DEFAULT_LIVENESS_DWELL {
            for action in mgr.observe(&result(node, CheckOutcome::Unreachable, 300 + i64::from(i)))
            {
                if matches!(action, NotifyAction::Fire(_)) {
                    refired = true;
                }
            }
        }
        assert!(refired, "surviving problem should re-fire after the window");
    }

    #[test]
    fn maintenance_suppresses_threshold_alerts_too() {
        use yagra_bus::Sample;
        use yagra_common::{ThresholdBounds, ThresholdRule};

        let node = NodeId::new();
        let mut meta = HashMap::new();
        meta.insert(node, NodeMeta::default());
        let thresholds = vec![StoredThreshold::new(
            Uuid::nil(),
            ScopeLevel::Node,
            vec![node.to_string()],
            ThresholdRule::new(
                "icmp_rtt_ms",
                ThresholdBounds::above(Some(50.0), Some(100.0)),
                1,
            ),
        )];

        let mgr = manager();
        let mut maintenance = BTreeSet::new();
        maintenance.insert(node);
        mgr.set_config(cfg(thresholds, meta).with_maintenance(maintenance));

        // A breaching sample during maintenance must not fire.
        let mut high = result(node, CheckOutcome::Reachable, 0);
        high.samples = vec![Sample::gauge("icmp_rtt_ms", 150.0)];
        assert!(mgr.observe(&high).is_empty());
        assert!(mgr.active_alerts().is_empty());
    }

    #[test]
    fn mute_matches_node_and_check() {
        let node = NodeId::new();
        let other = NodeId::new();
        let alert = Alert {
            subject: Subject::Node(node),
            check: check_id(node, "icmp_rtt_ms"),
            severity: Severity::Critical,
            state: NodeState::Critical,
            at_unix_ms: 0,
            root_cause: None,
            flapping: false,
            metric: "icmp_rtt_ms".to_string(),
            breach: None,
            ifindex: None,
        };

        // Whole-node mute matches any check on the node; another node's mute doesn't.
        assert!(mute_matches(
            &[ActiveMute::new(node.as_uuid(), None)],
            &alert
        ));
        assert!(!mute_matches(
            &[ActiveMute::new(other.as_uuid(), None)],
            &alert
        ));

        // Check-scoped mute matches only that check name (ids recomputed from the name).
        assert!(mute_matches(
            &[ActiveMute::new(node.as_uuid(), Some("icmp_rtt_ms"))],
            &alert
        ));
        assert!(!mute_matches(
            &[ActiveMute::new(
                node.as_uuid(),
                Some("snmp_sys_uptime_ticks")
            )],
            &alert
        ));
    }

    #[test]
    fn parent_down_suppresses_child_and_attributes_root_cause() {
        let parent = NodeId::new();
        let child = NodeId::new();
        let mut topo = Topology::new();
        topo.add_dependency(child, parent);

        let mgr = manager();
        mgr.set_config(cfg(Vec::new(), HashMap::new()).with_topology(topo));

        // Helper: drive a node Unreachable until it commits and return the fired alert.
        let drive_down = |node: NodeId, base: i64| -> Alert {
            let mut fired = None;
            for i in 0..DEFAULT_LIVENESS_DWELL {
                for action in mgr.observe(&result(
                    node,
                    CheckOutcome::Unreachable,
                    base + i64::from(i),
                )) {
                    if let NotifyAction::Fire(alert) = action {
                        fired = Some(alert);
                    }
                }
            }
            fired.expect("node should fire after dwell")
        };

        // Parent goes down first: it is a root (no upstream), so it carries no root cause and
        // would be the one that pages.
        let parent_alert = drive_down(parent, 0);
        assert_eq!(parent_alert.root_cause, None);

        // Child goes down with its only parent already down ⇒ attributed to the parent and its
        // own notification is suppressed (root_cause is what `Notifier` keys the skip on).
        let child_alert = drive_down(child, 100);
        assert_eq!(child_alert.root_cause, Some(parent));

        // Inventory roll-up still shows both down (the signal is kept; only the page is rolled
        // up).
        let states = mgr.node_states();
        assert_eq!(states.get(&parent), Some(&NodeState::Unreachable));
        assert_eq!(states.get(&child), Some(&NodeState::Unreachable));
    }

    #[test]
    fn child_down_before_parent_rolls_up_when_parent_falls() {
        // The ordering gap: a child that goes down *before* its parent fires standalone, then must
        // be rolled up (its standalone incident closed) once the parent falls — event-driven.
        let parent = NodeId::new();
        let child = NodeId::new();
        let mut topo = Topology::new();
        topo.add_dependency(child, parent);

        let mgr = manager();
        mgr.set_config(cfg(Vec::new(), HashMap::new()).with_topology(topo));

        // Collect every action produced while driving `node` to `outcome` across the dwell window.
        let drive = |node: NodeId, outcome: CheckOutcome, base: i64| -> Vec<NotifyAction> {
            let mut out = Vec::new();
            for i in 0..DEFAULT_LIVENESS_DWELL {
                out.extend(mgr.observe(&result(node, outcome, base + i64::from(i))));
            }
            out
        };

        // Child falls first, while its parent is still up ⇒ it pages standalone (no root cause).
        let child_actions = drive(child, CheckOutcome::Unreachable, 0);
        let child_fire = child_actions
            .iter()
            .find_map(|a| match a {
                NotifyAction::Fire(al) if al.subject.is_node(child) => Some(al.clone()),
                _ => None,
            })
            .expect("child fires standalone");
        assert_eq!(child_fire.root_cause, None);

        // Parent now falls: the re-sweep rolls the child up and emits a Suppress to close the
        // child's standalone incident (parent itself pages as the root cause).
        let parent_actions = drive(parent, CheckOutcome::Unreachable, 100);
        assert!(
            parent_actions.iter().any(|a| matches!(
                a,
                NotifyAction::Fire(al) if al.subject.is_node(parent) && al.root_cause.is_none()
            )),
            "parent fires as the root cause"
        );
        let suppressed = parent_actions
            .iter()
            .find_map(|a| match a {
                NotifyAction::Suppress(al) if al.subject.is_node(child) => Some(al.clone()),
                _ => None,
            })
            .expect("child rolled up under the parent");
        assert_eq!(suppressed.root_cause, Some(parent));

        // The child stays active (still down) but is now attributed to the parent.
        let child_active = mgr
            .active_alerts()
            .into_iter()
            .find(|a| a.subject.is_node(child))
            .expect("child still active");
        assert_eq!(child_active.root_cause, Some(parent));
    }

    #[test]
    fn parent_recovery_re_pages_still_down_child() {
        // Symmetric case: a child suppressed under a down parent must page on its own again once
        // the parent recovers while the child is still down.
        let parent = NodeId::new();
        let child = NodeId::new();
        let mut topo = Topology::new();
        topo.add_dependency(child, parent);

        let mgr = manager();
        mgr.set_config(cfg(Vec::new(), HashMap::new()).with_topology(topo));

        let drive = |node: NodeId, outcome: CheckOutcome, base: i64| -> Vec<NotifyAction> {
            let mut out = Vec::new();
            for i in 0..DEFAULT_LIVENESS_DWELL {
                out.extend(mgr.observe(&result(node, outcome, base + i64::from(i))));
            }
            out
        };

        // Parent down first, then child ⇒ the child is suppressed under the parent from the start.
        drive(parent, CheckOutcome::Unreachable, 0);
        drive(child, CheckOutcome::Unreachable, 100);
        let child_active = mgr
            .active_alerts()
            .into_iter()
            .find(|a| a.subject.is_node(child))
            .expect("child active");
        assert_eq!(child_active.root_cause, Some(parent));

        // Parent recovers while the child is still down ⇒ the child must now page standalone.
        let recovery = drive(parent, CheckOutcome::Reachable, 200);
        assert!(
            recovery.iter().any(|a| matches!(
                a,
                NotifyAction::Fire(al) if al.subject.is_node(child) && al.root_cause.is_none()
            )),
            "child re-pages standalone once its upstream is back"
        );
        let child_active = mgr
            .active_alerts()
            .into_iter()
            .find(|a| a.subject.is_node(child))
            .expect("child still active");
        assert_eq!(child_active.root_cause, None);
    }

    #[test]
    fn grandparent_fall_reattributes_grandchild_through_resweep() {
        // Transitive roll-up (S3 descendant scoping): gp → p → c. A re-sweep triggered by `gp`
        // falling must reach the *grandchild* c, not just its direct child p — so c's attribution
        // climbs to the new topmost cause. Guards against a non-transitive descendant scope.
        let gp = NodeId::new();
        let parent = NodeId::new();
        let child = NodeId::new();
        let mut topo = Topology::new();
        topo.add_dependency(parent, gp);
        topo.add_dependency(child, parent);

        let mgr = manager();
        mgr.set_config(cfg(Vec::new(), HashMap::new()).with_topology(topo));

        let drive = |node: NodeId, outcome: CheckOutcome, base: i64| {
            for i in 0..DEFAULT_LIVENESS_DWELL {
                mgr.observe(&result(node, outcome, base + i64::from(i)));
            }
        };

        // c falls first (all upstream up) → pages standalone. Then p falls → c rolls under p.
        drive(child, CheckOutcome::Unreachable, 0);
        drive(parent, CheckOutcome::Unreachable, 100);
        let c_active = mgr
            .active_alerts()
            .into_iter()
            .find(|a| a.subject.is_node(child))
            .expect("child active");
        assert_eq!(c_active.root_cause, Some(parent));

        // gp falls: the re-sweep from gp must reach the grandchild c (transitively) and re-attribute
        // it to gp — the new topmost down, unsuppressed ancestor. p rolls up too.
        drive(gp, CheckOutcome::Unreachable, 200);
        let c_active = mgr
            .active_alerts()
            .into_iter()
            .find(|a| a.subject.is_node(child))
            .expect("child still active");
        assert_eq!(
            c_active.root_cause,
            Some(gp),
            "grandchild re-attributed to the grandparent via the transitive re-sweep"
        );
        let p_active = mgr
            .active_alerts()
            .into_iter()
            .find(|a| a.subject.is_node(parent))
            .expect("parent still active");
        assert_eq!(p_active.root_cause, Some(gp));
    }

    #[tokio::test]
    async fn broadcast_acked_emits_upsert_for_active_alert() {
        // Fire a liveness alert, then mirror an inbound ack for it (ADR-015).
        let mgr = manager();
        let node = NodeId::new();
        for i in 0..DEFAULT_LIVENESS_DWELL {
            mgr.observe(&result(node, CheckOutcome::Unreachable, i64::from(i)));
        }
        let active = mgr.active_alerts();
        let alert = active.first().expect("one active alert after dwell");

        // Subscribe *after* the fire so only the ack event is observed.
        let mut rx = mgr.subscribe();
        mgr.broadcast_acked(
            &alert.subject.clone(),
            alert.check.as_uuid(),
            alert.severity,
            Some(serde_json::json!({ "by": "pd-user", "source": "pagerduty" })),
        );

        let (who, msg) = rx.try_recv().expect("ack event broadcast");
        let v: serde_json::Value = serde_json::from_str(&msg).unwrap();
        assert_eq!(v["acked"]["by"], "pd-user");
        // No `resolved` flag ⇒ the client upserts (keeps the alert), it doesn't clear it.
        assert!(v.get("resolved").is_none());
        assert_eq!(v["node"], serde_json::to_value(node).unwrap());
        assert_eq!(
            who,
            Subject::Node(node),
            "the frame names the node it concerns"
        );
    }

    #[test]
    fn every_stream_frame_names_the_node_its_body_describes() {
        // The scope filter on both SSE streams trusts the frame's node id and never looks inside
        // the JSON. If a sender ever attached the wrong id — or a placeholder — the filter would
        // silently pass an out-of-scope alert to a scoped subscriber, or hide an in-scope one, with
        // the payload looking perfectly correct either way. So the two must agree at the source.
        let mgr = manager();
        let node = NodeId::new();
        let mut alerts = mgr.subscribe();
        let mut states = mgr.subscribe_node_states();
        for i in 0..DEFAULT_LIVENESS_DWELL {
            mgr.observe(&result(node, CheckOutcome::Unreachable, i64::from(i)));
        }

        // The alert stream: the fire frame's id must match the `node` field of its own body.
        let (who, body) = alerts.try_recv().expect("dwell commits a liveness alert");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(serde_json::to_value(who).unwrap(), v["node"]);

        // The node-state stream writes the id as `node_id`; drain to the last frame it emitted.
        let mut last = None;
        while let Ok(frame) = states.try_recv() {
            last = Some(frame);
        }
        let (who, body) = last.expect("liveness changes emit node-state frames");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        let node_of_frame = who
            .node()
            .expect("a node-state frame is always about a node");
        assert_eq!(
            serde_json::to_value(node_of_frame.as_uuid()).unwrap(),
            v["node_id"]
        );
    }

    #[tokio::test]
    async fn broadcast_acked_is_noop_when_alert_not_active() {
        let mgr = manager();
        let mut rx = mgr.subscribe();
        // No matching active alert ⇒ nothing on screen to update, so no event is sent.
        mgr.broadcast_acked(
            &Subject::Node(NodeId::from(Uuid::from_u128(1))),
            Uuid::from_u128(2),
            Severity::Critical,
            None,
        );
        assert!(rx.try_recv().is_err());
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

    #[test]
    fn a_node_dedup_string_is_unchanged_by_the_subject_split() {
        // The vendor-facing identity: PagerDuty's `dedup_key` and JSM's `alias`. A change here
        // silently orphans every incident opened by a previous release.
        let node = NodeId::from(Uuid::from_u128(7));
        let check = CheckId::from(Uuid::from_u128(8));
        let key = yagra_alert::DedupKey {
            subject: Subject::Node(node),
            check,
            severity: Severity::Critical,
        };
        assert_eq!(dedup_string(&key), format!("yagra:{node}:{check}:critical"));
    }

    /// A pool name may contain a space or a slash, which would break the close-by-alias URL. A
    /// close that never lands is the dangling incident the resolve path exists to prevent.
    #[test]
    fn the_jsm_close_url_encodes_a_pool_name_and_leaves_a_node_alias_alone() {
        let notification = |subject: Subject| Notification {
            dedup_key: yagra_alert::DedupKey {
                subject,
                check: CheckId::from(Uuid::from_u128(2)),
                severity: Severity::Critical,
            },
            severity: Severity::Critical,
            summary: String::new(),
            payload: String::new(),
        };
        let node = NodeId::from(Uuid::from_u128(1));
        let url = jsm_close_url("https://api.example/v2", &notification(Subject::Node(node)));
        assert!(
            url.contains(&format!("yagra:{node}:")) && !url.contains('%'),
            "a node alias must be byte-identical to what an older core sent: {url}"
        );

        let url = jsm_close_url(
            "https://api.example/v2",
            &notification(Subject::Pool("tokyo dc/2".to_owned())),
        );
        assert!(
            url.contains("tokyo%20dc%2F2"),
            "pool name not encoded: {url}"
        );
    }

    /// The Increment 1 exclusion, pinned. A pool-coverage alert is delivered over the notification
    /// channels only — it must not appear in any node-keyed view, or it rolls into some node's
    /// display state and shows up on a page it does not belong to.
    #[test]
    fn a_pool_coverage_alert_stays_out_of_every_node_keyed_view() {
        let mgr = manager();
        let node = NodeId::new();
        assert!(mgr
            .raise_pool_coverage_alert("tokyo", 1_000)
            .is_some_and(|a| matches!(a, NotifyAction::Fire(_))));

        assert!(mgr.node_states().is_empty(), "no node's display state");
        assert!(mgr.node_state(node).is_none());
        assert!(mgr.alerts_for(node).is_empty());
        assert!(mgr.node_state_counts().is_empty());
        // It *is* active, so a second raise dedups and a resolve can find it.
        assert_eq!(mgr.active_alerts().len(), 1);
        assert!(mgr.raise_pool_coverage_alert("tokyo", 2_000).is_none());
        assert!(mgr
            .resolve_pool_coverage_alert("tokyo")
            .is_some_and(|a| matches!(a, NotifyAction::Resolve(_))));
        assert!(mgr.active_alerts().is_empty());
    }

    #[tokio::test]
    async fn a_pool_coverage_alert_streams_with_its_subject_decomposed() {
        // `web/src/services/sse.ts` gates frame validity on `typeof obj.node === 'string'`, so
        // `node` must stay present and stay a string for *every* subject — getting that wrong is a
        // silently dead live feed, not a visible error. `subject_kind`/`subject_name` beside it are
        // what let the client render a pool as a pool rather than as an unresolvable node.
        let mgr = manager();
        let mut rx = mgr.subscribe();
        mgr.raise_pool_coverage_alert("tokyo", 1_000);

        let (who, body) = rx.try_recv().expect("a coverage alert reaches the stream");
        assert_eq!(who, Subject::Pool("tokyo".to_owned()));
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["node"], "pool:tokyo", "the validity gate needs a string");
        assert_eq!(v["subject_kind"], "pool");
        assert_eq!(v["subject_name"], "tokyo");
        assert_eq!(v["resolved"], false);

        mgr.resolve_pool_coverage_alert("tokyo");
        let (_, body) = rx.try_recv().expect("the clear reaches it too");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["resolved"], true);
    }

    #[test]
    fn a_pool_alert_is_visible_to_the_scope_that_owns_a_node_in_that_pool() {
        // The whole reason the subject is a sum type: the operator scoped to the site that went
        // dark is exactly the person who must see this, and a synthetic node id would have hidden
        // it from them (and only them). Answered from the config snapshot, so no I/O per frame.
        let mgr = manager();
        let (mine, theirs) = (Uuid::from_u128(1), Uuid::from_u128(2));
        mgr.set_config(
            cfg(Vec::new(), HashMap::new()).with_pool_groups(HashMap::from([(
                "tokyo".to_owned(),
                BTreeSet::from([mine]),
            )])),
        );
        assert!(mgr.pool_is_in_any_group("tokyo", &[mine]));
        assert!(mgr.pool_is_in_any_group("tokyo", &[theirs, mine]));
        assert!(!mgr.pool_is_in_any_group("tokyo", &[theirs]));
        // Fail-closed on both empties: a pool the snapshot has not seen, and a scope naming no
        // group at all. Either answering `true` would show one site's outage to another's operator.
        assert!(!mgr.pool_is_in_any_group("osaka", &[mine]));
        assert!(!mgr.pool_is_in_any_group("tokyo", &[]));
    }

    #[test]
    fn a_pool_coverage_alert_cannot_be_muted_by_a_node_mute() {
        // Documented gap rather than a decision — a mute names a node. Pinned so the behaviour is
        // deliberate rather than discovered.
        let alert = match mgr_alert() {
            NotifyAction::Fire(a) => a,
            other => panic!("expected a fire, got {other:?}"),
        };
        let mutes = vec![ActiveMute::new(Uuid::from_u128(1), None)];
        assert!(!mute_matches(&mutes, &alert));
    }

    fn mgr_alert() -> NotifyAction {
        manager()
            .raise_pool_coverage_alert("tokyo", 1_000)
            .expect("a fresh manager raises")
    }

    #[test]
    fn the_built_in_wording_for_a_pool_names_the_pool_and_not_a_node() {
        let alert = match mgr_alert() {
            NotifyAction::Fire(a) => a,
            other => panic!("expected a fire, got {other:?}"),
        };
        for (event, expected) in [
            (
                NotifyEvent::Fire,
                "poller pool \"tokyo\" has no live poller",
            ),
            (
                NotifyEvent::Resolve,
                "resolved: poller pool \"tokyo\" has a live poller again",
            ),
        ] {
            let n = builtin_notification(&alert, event);
            assert!(n.summary.starts_with(expected), "got {:?}", n.summary);
            assert!(
                !n.summary.contains("node "),
                "a pool must not be described as a node: {:?}",
                n.summary
            );
        }
    }
    // ── ADR-076 increment 6b: the rule index ────────────────────────────────────────────────

    /// The **reference implementation**: `AlertConfig::resolve`'s body exactly as it stood before
    /// the rules were indexed, working from the flat per-metric list in its original order.
    ///
    /// 🚨 **Do not "improve" this.** Its whole value is being the slow, obvious version — the one
    /// that scans every rule and asks `threshold_applies` about each. If it is ever optimised to
    /// resemble the indexed implementation, the differential test below stops comparing two
    /// things and starts comparing one thing to itself.
    fn resolve_reference(
        candidates: &[StoredThreshold],
        node: NodeId,
        ifindex: Option<IfIndex>,
        meta: Option<&NodeMeta>,
    ) -> Option<EffectiveThreshold> {
        let matched: Vec<&StoredThreshold> = candidates
            .iter()
            .filter(|t| threshold_applies(t, node, ifindex, meta))
            .collect();
        let nearest = nearest_folder_depth(&matched, meta);
        let scoped: Vec<ScopedThreshold> = matched
            .into_iter()
            .filter(|t| t.level != ScopeLevel::FolderGroup || folder_depth(t, meta) == nearest)
            .map(|t| ScopedThreshold::new(t.level, t.rule.clone()))
            .collect();
        resolve_effective(&scoped)
    }

    fn rule_at(level: ScopeLevel, scope_id: &str, dir: Direction, crit: f64) -> StoredThreshold {
        StoredThreshold::new(
            Uuid::new_v4(),
            level,
            vec![scope_id.to_string()],
            yagra_common::ThresholdRule::new(
                "cpu_util",
                yagra_common::ThresholdBounds::from_legacy(dir, None, Some(crit)),
                3,
            ),
        )
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

    /// The same, naming several targets at once (ADR-078).
    fn rule_at_many(level: ScopeLevel, ids: &[&str], dir: Direction, crit: f64) -> StoredThreshold {
        StoredThreshold::new(
            Uuid::new_v4(),
            level,
            ids.iter().map(|s| (*s).to_string()).collect(),
            yagra_common::ThresholdRule::new(
                "cpu_util",
                yagra_common::ThresholdBounds::from_legacy(dir, None, Some(crit)),
                3,
            ),
        )
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

    #[test]
    fn an_indexed_port_rule_still_fires() {
        // 🚨 The acceptance case. Every other test here checks that something does NOT match, and
        // a suite of only-rejections passes just as happily when the index drops everything.
        let node = NodeId::from(Uuid::new_v4());
        let mgr = AlertManager::new();
        mgr.set_config(cfg(
            vec![StoredThreshold::new(
                Uuid::new_v4(),
                ScopeLevel::Interface,
                vec![format!("{}:7", node.as_uuid())],
                yagra_common::ThresholdRule::new(
                    "if_in_util_pct",
                    yagra_common::ThresholdBounds::above(None, Some(90.0)),
                    1,
                ),
            )],
            HashMap::new(),
        ));
        // The port that has the rule breaches it.
        let actions = mgr
            .observe_interface_metric(node, IfIndex(7), "if_in_util_pct", 95.0, 1_000)
            .expect("a rule is in force on this port");
        assert!(
            actions.iter().any(
                |a| matches!(a, NotifyAction::Fire(alert) if alert.severity == Severity::Critical)
            ),
            "an indexed interface rule must still raise: {actions:?}"
        );
        // The port next door has no rule and must resolve to nothing at all.
        assert!(
            mgr.observe_interface_metric(node, IfIndex(8), "if_in_util_pct", 99.0, 1_000)
                .is_none(),
            "port 8 has no rule, so nothing should be observed for it"
        );
    }
    /// **A benchmark, not a guard** — `#[ignore]`d because a timing assertion on a shared CI box is
    /// a flaky test, and a flaky test gets deleted. Run it by hand when this path changes:
    ///
    /// ```text
    /// cargo test --profile ci-fast -p yagra-core --bin yagra-core \
    ///     one_interface_watch_tick_scales_with_rules -- --ignored --nocapture
    /// ```
    ///
    /// What it measures: one direction of one `run_interface_utilization_watch` tick, i.e. one
    /// `observe_interface_metric` per candidate port, against a config holding N rules on that
    /// metric. Correctness is the differential test's job; this only says how much it costs.
    ///
    /// **Baseline — before the rule index (2026-08-20, Ryzen 9 8945HS, `ci-fast`, 24,000 ports):**
    ///
    /// | rules | per port | total |
    /// |---|---|---|
    /// | 1 | 653 ns | 15.7 ms |
    /// | 101 | 3.3 µs | 79.3 ms |
    /// | 1,001 | 28.0 µs | 671.7 ms |
    /// | 10,001 | 271.2 µs | 6.5 s |
    ///
    /// Perfectly linear in rules × ports — ~27 ns per (port × rule). A hundred 48-port switches is
    /// 9,600 port rules, so the 10,001 row is not a hypothetical.
    ///
    /// **After the rule index — same machine, same harness, same day:**
    ///
    /// | rules | per port | total |
    /// |---|---|---|
    /// | 1 | 665 ns | 16.0 ms |
    /// | 101 | 726 ns | 17.4 ms |
    /// | 1,001 | 693 ns | 16.6 ms |
    /// | 10,001 | 693 ns | **16.6 ms** |
    ///
    /// Flat in the rule count, and 391× faster at the 10,001 row. What is left is the ~665 ns/port
    /// floor, which is `process_check`'s own bookkeeping (two mutexes and the dwell window) — not
    /// rule resolution. Anyone attacking this path next should attack that, not the lookup.
    #[test]
    #[ignore = "benchmark: run by hand with --ignored --nocapture"]
    fn one_interface_watch_tick_scales_with_rules() {
        const NODES: usize = 500;
        const PORTS: usize = 48;

        println!("\n=== one direction of one watch tick, every port resolving ===");
        for n_rules in [0usize, 100, 1_000, 10_000] {
            let nodes: Vec<NodeId> = (0..NODES).map(|_| NodeId::from(Uuid::new_v4())).collect();
            let meta: HashMap<NodeId, NodeMeta> =
                nodes.iter().map(|n| (*n, NodeMeta::default())).collect();

            // One global rule so every port resolves to *something* — the expensive path. An early
            // `None` would flatter the numbers by skipping the work being measured.
            let mut rules = vec![StoredThreshold::new(
                Uuid::new_v4(),
                ScopeLevel::Global,
                Vec::new(),
                yagra_common::ThresholdRule::new(
                    "if_in_util_pct",
                    yagra_common::ThresholdBounds::above(Some(70.0), Some(90.0)),
                    3,
                ),
            )];
            for i in 0..n_rules {
                rules.push(StoredThreshold::new(
                    Uuid::new_v4(),
                    ScopeLevel::Interface,
                    vec![format!(
                        "{}:{}",
                        nodes[i % nodes.len()].as_uuid(),
                        (i % PORTS) + 1
                    )],
                    yagra_common::ThresholdRule::new(
                        "if_in_util_pct",
                        yagra_common::ThresholdBounds::above(Some(70.0), Some(90.0)),
                        3,
                    ),
                ));
            }

            let mgr = AlertManager::new();
            mgr.set_config(AlertConfig::new(rules, meta));

            let t0 = std::time::Instant::now();
            let mut observed = 0usize;
            for node in &nodes {
                for p in 1..=PORTS {
                    if mgr
                        .observe_interface_metric(
                            *node,
                            IfIndex(p as u32),
                            "if_in_util_pct",
                            42.0,
                            0,
                        )
                        .is_some()
                    {
                        observed += 1;
                    }
                }
            }
            let el = t0.elapsed();
            let ports = NODES * PORTS;
            println!(
                "rules={:<7} ports={:<8} elapsed={:>9.1?}  per_port={:>8.1?}  observed={observed}",
                n_rules + 1,
                ports,
                el,
                el / u32::try_from(ports).unwrap_or(1),
            );
            // Not a timing assertion — just proof the loop did the work rather than short-circuiting.
            assert_eq!(
                observed, ports,
                "every port must resolve, or the number means nothing"
            );
        }
    }

    // ---- ADR-076 増分 7: the freeze gate and the orphan sweep -------------------------------

    fn meta_for(node: NodeId) -> HashMap<NodeId, NodeMeta> {
        let mut m = HashMap::new();
        m.insert(node, NodeMeta::default());
        m
    }

    /// A port-scoped `if_in_util_pct above <warning>` rule, dwell 1.
    fn port_rule(node: NodeId, idx: IfIndex, warning: f64) -> StoredThreshold {
        use yagra_common::{ThresholdBounds, ThresholdRule};
        StoredThreshold::new(
            Uuid::nil(),
            ScopeLevel::Interface,
            vec![format!("{node}:{}", idx.0)],
            ThresholdRule::new(
                "if_in_util_pct",
                ThresholdBounds::above(Some(warning), Some(90.0)),
                1,
            ),
        )
    }

    /// The gate `run_interface_utilization_watch` applies, assembled from its two halves so a test
    /// asks exactly the question the loop asks.
    fn may_observe(mgr: &AlertManager, node: NodeId) -> bool {
        crate::interface_util::may_observe_ports(mgr.node_liveness(node))
    }

    /// 🚨 The bug ADR-076 増分 7 fixes, in the smallest form that shows it.
    ///
    /// Before the fix the last assertion failed. The gate read `node_state` — the display roll-up,
    /// which folds in every active alert on the node — so the instant a port alert fired, the
    /// evaluator stopped visiting that node. Since the same evaluator is the only thing that can
    /// resolve a port alert, nothing ever cleared: on the test server, 12 fires and 0 resolves in
    /// one day, at any traffic level and any threshold.
    #[test]
    fn a_port_alert_does_not_freeze_its_own_evaluator() {
        let mgr = manager();
        let node = NodeId::new();
        let idx = IfIndex(7);
        mgr.set_config(cfg(vec![port_rule(node, idx, 1.0)], meta_for(node)));

        let _ = mgr.observe(&result(node, CheckOutcome::Reachable, 0));
        assert!(may_observe(&mgr, node), "a reachable node starts un-frozen");

        let acts = mgr
            .observe_interface_metric(node, idx, "if_in_util_pct", 8.19, 1)
            .expect("a rule is in force");
        assert!(acts.iter().any(|a| matches!(a, NotifyAction::Fire(_))));

        // The roll-up moves, and that is correct — it is what the Nodes page paints.
        assert_eq!(mgr.node_state(node), Some(NodeState::Warning));
        // Liveness does not, and liveness is what the evaluator must ask.
        assert_eq!(mgr.node_liveness(node), Some(NodeState::Ok));
        assert!(
            may_observe(&mgr, node),
            "the port alert froze the only loop that can ever resolve it"
        );
    }

    /// The same trap reached from the other side: any alert at all used to freeze bandwidth
    /// evaluation for the whole node, so a router carrying a latency alert was silently
    /// unmonitored for congestion.
    #[test]
    fn an_unrelated_alert_does_not_freeze_the_interface_evaluator() {
        use yagra_bus::Sample;
        use yagra_common::{ThresholdBounds, ThresholdRule};

        let mgr = manager();
        let node = NodeId::new();
        mgr.set_config(cfg(
            vec![StoredThreshold::new(
                Uuid::nil(),
                ScopeLevel::Node,
                vec![node.to_string()],
                ThresholdRule::new("icmp_rtt_ms", ThresholdBounds::above(Some(50.0), None), 1),
            )],
            meta_for(node),
        ));

        let mut slow = result(node, CheckOutcome::Reachable, 0);
        slow.samples = vec![Sample::gauge("icmp_rtt_ms", 150.0)];
        let _ = mgr.observe(&slow);

        assert_eq!(mgr.node_state(node), Some(NodeState::Warning));
        assert!(
            may_observe(&mgr, node),
            "a latency alert must not stop bandwidth being evaluated on the same node"
        );
    }

    /// The rejecting half. Without it, "the gate now accepts everything" would pass every test
    /// above — and accepting an unreachable node is the failure decision 3 wrote the gate for.
    #[test]
    fn an_unreachable_node_is_still_frozen() {
        let mgr = manager();
        let node = NodeId::new();
        for i in 0..DEFAULT_LIVENESS_DWELL {
            let _ = mgr.observe(&result(node, CheckOutcome::Unreachable, i64::from(i)));
        }
        assert_eq!(mgr.node_liveness(node), Some(NodeState::Unreachable));
        assert!(
            !may_observe(&mgr, node),
            "feeding a down device its ports would page about a link on a box already down"
        );
    }

    /// A node the engine has never observed has no opinion behind it, which is not the same as
    /// "fine".
    #[test]
    fn a_never_observed_node_is_frozen() {
        let mgr = manager();
        let node = NodeId::new();
        assert_eq!(mgr.node_liveness(node), None);
        assert!(!may_observe(&mgr, node));
    }

    /// Decision 3's other half, which the old gate also blocked: inside a maintenance window the
    /// evaluator must keep observing, so an open port alert resolves the way a node-level one
    /// does. Before this, a port alert was the only kind a window could not silence.
    #[test]
    fn a_window_reaches_a_port_alert_because_maintenance_is_not_frozen() {
        let mgr = manager();
        let node = NodeId::new();
        let idx = IfIndex(7);
        mgr.set_config(cfg(vec![port_rule(node, idx, 1.0)], meta_for(node)));
        let _ = mgr.observe(&result(node, CheckOutcome::Reachable, 0));
        let acts = mgr
            .observe_interface_metric(node, idx, "if_in_util_pct", 8.19, 1)
            .expect("a rule is in force");
        assert!(acts.iter().any(|a| matches!(a, NotifyAction::Fire(_))));

        let mut window = BTreeSet::new();
        window.insert(node);
        mgr.set_config(
            cfg(vec![port_rule(node, idx, 1.0)], meta_for(node)).with_maintenance(window),
        );
        for i in 0..DEFAULT_LIVENESS_DWELL {
            let _ = mgr.observe(&result(node, CheckOutcome::Reachable, 100 + i64::from(i)));
        }
        assert_eq!(mgr.node_liveness(node), Some(NodeState::Maintenance));
        assert!(may_observe(&mgr, node), "a window must not freeze the loop");

        let acts = mgr
            .observe_interface_metric(node, idx, "if_in_util_pct", 8.19, 200)
            .expect("the rule is still there");
        assert!(acts.iter().any(|a| matches!(a, NotifyAction::Resolve(_))));
        assert!(mgr.active_alerts().is_empty());
    }

    /// Deleting a port rule must close its alert. The poll path cannot do it: that branch only
    /// visits checks something polls, and nothing polls `if_in_util_pct`.
    #[test]
    fn the_orphan_sweep_closes_a_port_alert_whose_rule_was_deleted() {
        let mgr = manager();
        let node = NodeId::new();
        let idx = IfIndex(7);
        mgr.set_config(cfg(vec![port_rule(node, idx, 1.0)], meta_for(node)));
        let _ = mgr.observe(&result(node, CheckOutcome::Reachable, 0));
        let acts = mgr
            .observe_interface_metric(node, idx, "if_in_util_pct", 8.19, 1)
            .expect("a rule is in force");
        assert!(acts.iter().any(|a| matches!(a, NotifyAction::Fire(_))));

        // The accepting half, and it is load-bearing: a sweep that resolved everything would pass
        // the rest of this test.
        assert!(
            mgr.resolve_orphaned_interface_alerts().is_empty(),
            "a rule that still exists is not an orphan"
        );

        mgr.set_config(cfg(Vec::new(), meta_for(node)));
        let swept = mgr.resolve_orphaned_interface_alerts();
        assert_eq!(swept.len(), 1);
        assert!(matches!(swept[0], NotifyAction::Resolve(_)));
        assert!(mgr.active_alerts().is_empty());
        assert!(
            mgr.resolve_orphaned_interface_alerts().is_empty(),
            "the sweep runs every 60s for the life of the process; it must be idempotent"
        );

        // 🚨 And the port must be able to alert again. This assertion is here because the first
        // version of this test stopped at "idempotent" — which a check that can no longer do
        // anything also satisfies. Closing the alert without dropping the state machine left it
        // committed at `Warning`, so a recreated rule saw `Warning → Warning`, no transition, and
        // the port went silent for the life of the process. Found on the test server: rule
        // recreated at 1%, port at 6.7%, nothing for eight minutes.
        mgr.set_config(cfg(vec![port_rule(node, idx, 1.0)], meta_for(node)));
        let acts = mgr
            .observe_interface_metric(node, idx, "if_in_util_pct", 8.19, 3)
            .expect("the recreated rule is in force");
        assert!(
            acts.iter().any(|a| matches!(a, NotifyAction::Fire(_))),
            "a port whose rule was deleted and recreated must alert again, got {acts:?}"
        );
    }

    /// A *collected* per-interface metric arrives on the poll path, so that path already closes it
    /// when its rule goes. Two closers on one alert is a race, not a belt and braces.
    #[test]
    fn the_orphan_sweep_leaves_collected_port_alerts_to_the_poll_path() {
        use yagra_bus::Sample;
        use yagra_common::{MetricKind, ThresholdRule};

        let mgr = manager();
        let node = NodeId::new();
        let idx = IfIndex(7);
        let rule = StoredThreshold::new(
            Uuid::nil(),
            ScopeLevel::Interface,
            vec![format!("{node}:{}", idx.0)],
            ThresholdRule::new("if_oper_status", ThresholdBounds::below(None, Some(0.5)), 1),
        );
        let per_if: BTreeSet<String> = ["if_oper_status".to_owned()].into_iter().collect();
        mgr.set_config(cfg(vec![rule], meta_for(node)).with_per_interface(per_if.clone()));

        let mut down = result(node, CheckOutcome::Reachable, 0);
        down.samples = vec![Sample::interface(
            "if_oper_status",
            idx,
            0.0,
            MetricKind::Gauge,
        )];
        let _ = mgr.observe(&down);
        assert_eq!(mgr.active_alerts().len(), 1);

        mgr.set_config(cfg(Vec::new(), meta_for(node)).with_per_interface(per_if));
        assert!(
            mgr.resolve_orphaned_interface_alerts().is_empty(),
            "the poll path owns this one"
        );
        assert_eq!(mgr.active_alerts().len(), 1);
    }
}
