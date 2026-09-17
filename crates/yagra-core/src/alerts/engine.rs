// SPDX-License-Identifier: AGPL-3.0-only
//! Has anything changed? — the state-machine half of the alert module (ADR-083).
//!
//! Drives the tested [`yagra_alert`] machine from live poll results: dwell-time hysteresis,
//! flapping detection, dependency suppression, maintenance windows, the in-memory active set and
//! the SSE broadcast. Everything here is stateful and lock-bearing; the pure "which rule applies"
//! half is [`super::rules`] and the "who gets told" half is [`super::notify`].
//!
//! 🚨 This module names **no** delivery type. An alert leaves here as a [`super::NotifyAction`]
//! and nothing more — the module doc on [`super`] says why that boundary is load-bearing.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::{Arc, Mutex, RwLock};

use tokio::sync::broadcast;
use uuid::Uuid;
use yagra_alert::CheckState;
use yagra_alert::{Alert, Breach, Subject};
use yagra_bus::{CheckOutcome, PollResult, RowName, Sample};
use yagra_common::{
    CheckId, Direction, EffectiveThreshold, IfIndex, MetricKind, NodeId, NodeState, Severity,
};

use crate::poll_interval::{self, PollIntervals};
use crate::thresholds::StoredThreshold;

use super::rules::*;
use super::{NotifyAction, StreamFrame};

/// How many transitions inside the flap window make a check flapping. The window itself follows the
/// node's poll interval ([`crate::poll_interval::flap_window_ms`], ADR-144).
const FLAP_THRESHOLD: usize = 5;

/// SSE broadcast buffer. Sized generously so a briefly-slow subscriber doesn't lag past the
/// window and miss events; if one does lag, the stream handler logs it and emits a `resync`
/// hint so the client can re-fetch the active-alert list (see `stream_alerts` in api.rs).
const EVENT_BUFFER: usize = 1024;
/// Node-state SSE buffer (S14). Larger than the alert buffer because a full poll sweep can emit
/// one state event per node (first observation plus genuine transitions). A subscriber that
/// overflows this gets a `resync` hint and re-seeds from REST, so the bound is a soft backstop.
const NODE_EVENT_BUFFER: usize = 4096;

/// Which check one threshold sample is observed on.
///
/// Three shapes, one per dimension a metric can be collected in, and a metric is only ever one of
/// them: the node as a whole, one port (ADR-076), or one row of a vendor table (ADR-143). An enum
/// rather than an `Option` for the port and another for the row, because "a port and a row at once"
/// is not a state any sample is in, and two options would let a caller spell it.
#[derive(Debug, Clone, Copy)]
enum CheckOn<'a> {
    /// The node's one check for the metric.
    Node,
    /// One port's check.
    Port(IfIndex),
    /// One table row's check, with the row's name when one has been read.
    Row(u32, Option<&'a str>),
}

/// Whether a sample is a reading, or the placeholder a vendor answers for a row with no reading at
/// all (ADR-156).
///
/// A placeholder never reaches a rule as a value. On a table row that already holds a state it is
/// **evidence**: the device itself says the row has nothing to measure, so the row is observed as
/// `Ok` through its dwell and an alert on it recovers the ordinary way. Everywhere else it is
/// nothing — a row with no state stays without one, and a placeholder on a port or on a node-wide
/// check says nothing about that check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reading {
    /// A measured value, judged against the rule.
    Value,
    /// The column's no-reading placeholder, taken out of the result by
    /// [`crate::no_reading_filter::NoReadingHandle::admit`].
    NoReading,
}

/// What each vendor-table row is called, per node: node → metric → row key → name (ADR-143).
type RowNamesByNode = HashMap<NodeId, HashMap<String, HashMap<u32, String>>>;

/// One table row handed to `observe_rows`: the sample, its resolved rule, the row key, the row's
/// name, and whether the sample is a reading or a placeholder (ADR-143, ADR-156).
type RowObservation<'a> = (
    &'a Sample,
    &'a EffectiveThreshold,
    u32,
    Option<&'a str>,
    Reading,
);

/// When one batch of samples was observed, and how often such a batch arrives (ADR-144).
///
/// Bundled because every threshold observation needs all four together and the functions that pass
/// them along were already at clippy's argument limit.
#[derive(Debug, Clone, Copy)]
struct Moment {
    at_unix_ms: i64,
    in_maintenance: bool,
    cadence: Cadence,
    /// The node's poll interval, read once for the whole batch.
    interval: Option<u32>,
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
    /// What each vendor-table row is called: node → metric → row key → name (ADR-143).
    ///
    /// Filled from poll results ([`Self::record_row_names`]) and, before those start, from PostgreSQL
    /// ([`Self::seed_row_names`]). Read when a rule on the metric is scoped to a row name, and to put
    /// the name on an alert when it fires.
    row_names: RwLock<RowNamesByNode>,
    /// The table rows whose check holds a state: node → metric → rows (ADR-143 decision 5).
    ///
    /// 🚨 **A healthy row with no state is not observed at all**, so this index decides whether a
    /// healthy sample is worth a check id. A row missing from it while its check holds a state would
    /// never see its own recovery — which is why [`Self::restore`] fills it from the restored alerts.
    /// An entry whose state has since gone is harmless: the row is observed once more and holds a
    /// fresh `Ok` state.
    row_states: Mutex<HashMap<NodeId, HashMap<String, HashSet<u32>>>>,
    /// Node-wide threshold checks restored from before a table row alerted on its own (ADR-143
    /// decision 6). The first per-row observation of the same metric closes the matching one here.
    legacy_node_checks: Mutex<HashSet<CheckId>>,
    /// How far apart each node's polls are, as the scheduler last published it (ADR-144). Read once
    /// per observation to size the flap window and, for a check read once a tick, the dwell.
    intervals: PollIntervals,
}

/// One open alert a store can still be asked about, and the series that would answer.
///
/// Produced by [`AlertManager::freshness_candidates`] and consumed by `alerts::stale`, which is
/// where the windows and the safety canary live. The split is what makes the judgement testable
/// without a TSDB: choosing *what to ask* is pure, and asking is not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FreshnessCandidate {
    pub check: CheckId,
    pub node: NodeId,
    /// The alert's own metric — for logging and for tests to name a candidate by.
    pub metric: String,
    /// The series whose presence proves something is still measuring this check.
    ///
    /// 🚨 **Not always `metric`.** A derived metric is computed and never stored (ADR-105), so it
    /// answers with `Formula::inputs` instead; asking for its own name would report every healthy
    /// derived alert as stranded. One entry when the formula repeats its input, two otherwise.
    pub inputs: Vec<String>,
}

impl AlertManager {
    /// New manager with an empty config (no thresholds until [`Self::set_config`]) and no poll
    /// intervals, so every dwell and flap window is what it was before ADR-144.
    #[must_use]
    pub fn new() -> Self {
        Self::with_poll_intervals(PollIntervals::unknown())
    }

    /// New manager that reads each node's poll interval from `intervals` — the handle the scheduler
    /// publishes into (ADR-144).
    #[must_use]
    pub fn with_poll_intervals(intervals: PollIntervals) -> Self {
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
            row_names: RwLock::new(HashMap::new()),
            row_states: Mutex::new(HashMap::new()),
            legacy_node_checks: Mutex::new(HashSet::new()),
            intervals,
        }
    }

    /// Replace the threshold/metadata snapshot (called by the periodic refresh task).
    pub fn set_config(&self, config: AlertConfig) {
        *self.config.write().expect("config rwlock poisoned") = config;
    }

    /// Remember what one node's vendor-table rows are called, from the poll result that read them
    /// (ADR-143). Called before that result's own samples are observed, so a rule scoped to a row
    /// name can match on the very poll that learned the name.
    ///
    /// Cleaned and capped here rather than trusted (ADR-143 Inc.2): the bus message says the poller
    /// already did both, and nothing on this side held it to that. A name kept here becomes an
    /// alert's `row_name`, which reaches every notification and `alert_history`.
    pub fn record_row_names(&self, node: NodeId, names: &[RowName]) {
        let names = RowName::cleaned(names);
        if names.is_empty() {
            return;
        }
        let mut map = self.row_names.write().expect("row names rwlock poisoned");
        let per_node = map.entry(node).or_default();
        for n in names {
            per_node.entry(n.metric).or_default().insert(n.row, n.name);
        }
    }

    /// Load the stored row names before results start arriving (ADR-143 decision 3). Returns how
    /// many it took. Never overwrites a name a poll has already delivered.
    pub fn seed_row_names(
        &self,
        rows: impl IntoIterator<Item = (NodeId, String, u32, String)>,
    ) -> usize {
        let mut map = self.row_names.write().expect("row names rwlock poisoned");
        let mut taken = 0usize;
        for (node, metric, row, name) in rows {
            map.entry(node)
                .or_default()
                .entry(metric)
                .or_default()
                .entry(row)
                .or_insert_with(|| {
                    taken += 1;
                    name
                });
        }
        taken
    }

    /// Seed the engine with the alerts that were open when the previous process stopped
    /// (ADR-097 decision 2). Returns how many it took.
    ///
    /// 🚨 **Restoring is not firing.** This returns no [`NotifyAction`] and broadcasts nothing:
    /// these incidents are already open in whatever external tool their dedup key reached. What it
    /// buys is that the *next* poll behaves the way it would have if the process had never stopped
    /// — a still-broken check produces no transition (so no duplicate incident), and a check whose
    /// device recovered while core was down produces a `Resolve` on its way back. Before this,
    /// neither happened: measured on the test server, `alert_history` held 1,356 transitions of
    /// which only 18 were clears, and one continuously-down device had eight `__liveness__` fires
    /// and no clear inside 24 hours.
    ///
    /// Idempotent, and deliberately so rather than merely defensively: every insert is
    /// `or_insert`, so anything the engine has already observed wins. That makes calling this after
    /// results have started flowing harmless instead of destructive.
    ///
    /// ⚠️ The dwell it seeds each [`CheckState`] with is arbitrary. `process_check` re-points it
    /// from the rule on the very first observation (ADR-075) and nothing reads it before then, so
    /// there is no need to resolve the config here — which is fortunate, because at startup the
    /// config has not been loaded yet.
    pub fn restore(&self, alerts: Vec<Alert>) -> usize {
        if alerts.is_empty() {
            return 0;
        }
        self.seed_states(&alerts);
        // 🚨 `live` and `down` move together or not at all — `process_check` calls `live` the
        // down-set's "only mutation site", and seeding one without the other would break that in a
        // way nothing downstream could detect. So the nodes that actually landed in `live` are
        // collected here and are the only ones `down` hears about; a node the engine has already
        // observed keeps its observation and contributes nothing.
        let mut newly_down: Vec<NodeId> = Vec::new();
        {
            let mut live = self.live.lock().expect("live mutex poisoned");
            for a in &alerts {
                if a.metric != LIVENESS {
                    continue;
                }
                let Some(node) = a.node() else { continue };
                if live.contains_key(&node) {
                    continue;
                }
                live.insert(node, a.state);
                if matches!(a.state, NodeState::Unreachable) {
                    newly_down.push(node);
                }
            }
        }
        {
            let mut down = self.down.lock().expect("down mutex poisoned");
            down.extend(newly_down);
        }
        // The authoritative down set, read back rather than rebuilt from the rows: ADR-087's rule
        // is "is this node down *now*", and a node can be in it because of an observation this
        // restore did not make.
        let down = self.down_set();
        self.seed_active(alerts, &down)
    }

    /// Seed the alerts that were open about nodes the inventory **no longer holds**, so that
    /// [`Self::forget_deleted_nodes`] can close them (ADR-097 Increment 5).
    ///
    /// Decision 4 used to drop these rows on the way in, on the reasoning that nothing polls a
    /// deleted node so nothing could ever resolve the restored alert. Increment 4 built that path,
    /// which made the reasoning false and turned the exclusion into the only thing keeping the rows
    /// open: the sweep reads `active`, and the restore had made sure they were never in it.
    /// Measured 2026-08-31 — 43,227 rows left permanently open by deleting 15,000 nodes against a
    /// stopped core, with their external incidents still open too.
    ///
    /// # 🚨 Why this does not seed `live` / `down`, and [`Self::restore`] does
    ///
    /// **`down` is the dependency-suppression set.** A node in it suppresses the alerts of
    /// everything topology says sits behind it, and a *deleted* node has no business suppressing
    /// anything — least of all for the minute before the sweep notices. `live` moves with `down` or
    /// not at all ([`Self::restore`] says why), so neither is seeded.
    ///
    /// ⚠️ **This does not keep the deleted node out of the fleet's per-state breakdown**, and it
    /// was written believing it would. [`Self::node_states`] rolls up `live` **unioned with every
    /// active alert's node**, so an alert restored here still contributes a node the inventory does
    /// not have, and `api::fleet::state_tally` — whose total comes from PostgreSQL — can therefore
    /// report a breakdown that sums to more than its own total. It lasts until the first sweep
    /// after the config loads (bounded by [`crate::alerts::deleted`]'s startup cadence, seconds
    /// rather than the steady-state minute) and then corrects itself. The alternative was leaving
    /// the incidents open forever, so the transient is the price and it is written down rather than
    /// smoothed over.
    ///
    /// `root_cause` is not re-derived either, for the same reason as `down` and one more:
    /// attribution says "this alert is part of that node's outage", and the node is about to stop
    /// existing in the engine entirely.
    ///
    /// ⚠️ **Nothing here closes anything.** Like [`Self::restore`] it returns no [`NotifyAction`]
    /// and broadcasts nothing; it makes the rows *visible to the sweep*, which is what closes them,
    /// with the notification the incident's external tool is waiting for.
    pub fn restore_deleted(&self, alerts: Vec<Alert>) -> usize {
        if alerts.is_empty() {
            return 0;
        }
        self.seed_states(&alerts);
        self.seed_active(alerts, &BTreeSet::new())
    }

    /// Seed each alert's dwell/flap bookkeeping. One lock, taken in the order `process_check` takes
    /// it — this runs before the ingest starts, but a restore that could deadlock against a poll
    /// result would be a trap laid for whoever moves the call.
    fn seed_states(&self, alerts: &[Alert]) {
        {
            let mut states = self.states.lock().expect("states mutex poisoned");
            for a in alerts {
                states.entry(a.check).or_insert_with(|| {
                    // Both the dwell and the flap window are re-pointed by the first observation.
                    CheckState::restored(
                        a.state,
                        DEFAULT_LIVENESS_DWELL,
                        poll_interval::flap_window_ms(None),
                        FLAP_THRESHOLD,
                    )
                });
            }
        }
        // ADR-143. A restored row alert's check holds a state now, so its row goes into the index —
        // without it a healthy sample of that row would be skipped and the alert would never
        // resolve. A restored node-wide threshold alert may be about a metric whose rows alert on
        // their own since this version, so it is remembered for the first per-row observation to
        // close. The locks are taken one after the other, never nested.
        {
            let mut rows = self.row_states.lock().expect("row states mutex poisoned");
            for a in alerts {
                if let (Some(node), Some(row)) = (a.node(), a.row) {
                    rows.entry(node)
                        .or_default()
                        .entry(a.metric.clone())
                        .or_default()
                        .insert(row);
                }
            }
        }
        let mut legacy = self
            .legacy_node_checks
            .lock()
            .expect("legacy checks mutex poisoned");
        for a in alerts {
            if a.node().is_some()
                && a.row.is_none()
                && a.ifindex.is_none()
                && Self::is_threshold_alert(a)
            {
                legacy.insert(a.check);
            }
        }
    }

    /// Put the alerts into the active set, attributing each to `down` where ADR-087 says it belongs.
    /// Returns the size of the active set afterwards.
    ///
    /// `down` is empty for a deleted node's alerts — see [`Self::restore_deleted`].
    fn seed_active(&self, alerts: Vec<Alert>, down: &BTreeSet<NodeId>) -> usize {
        let mut active = self.active.lock().expect("alerts mutex poisoned");
        for mut a in alerts {
            // The attribution is not stored (`alert_history` has no `root_cause` column), so it is
            // re-derived rather than dropped. Without this, every restart erased the "part of this
            // node's outage" marker ADR-087 put on an alert until that node next transitioned —
            // and a node that stays down never transitions again.
            if a.metric != LIVENESS {
                if let Some(node) = a.node().filter(|n| down.contains(n)) {
                    a.root_cause = Some(node);
                }
            }
            active.entry(a.check).or_insert(a);
        }
        active.len()
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

    /// The rolled-up display state for a PAGE of nodes — [`Self::node_states`] without the
    /// whole-fleet clone (ADR-125).
    ///
    /// `node_states` copies the entire live map on every call, and `display_states` goes through it
    /// to look up a few dozen entries — on the hottest read in the product (every inventory page,
    /// every lazy folder load, every debounced search keystroke). At a few thousand nodes that is a
    /// fresh allocation and copy per request, made while holding the `live` lock that the poll
    /// ingest path also takes (`process_check`). This walks the page instead: **O(page + active)**
    /// rather than O(fleet + active).
    ///
    /// 🚨 **The two locks are taken one after the other, never nested** — the shape
    /// [`Self::node_state`] already uses. `node_states` is the one that nests them, and copying its
    /// body here would have carried that over silently.
    ///
    /// ⚠️ **`node_states` stays.** [`Self::node_state_counts`] asks about the whole fleet, and the
    /// answer for a page is not a smaller version of that question.
    #[must_use]
    pub fn node_states_for(&self, nodes: &[NodeId]) -> HashMap<NodeId, NodeState> {
        let mut out: HashMap<NodeId, NodeState> = {
            let live = self.live.lock().expect("live mutex poisoned");
            nodes
                .iter()
                .filter_map(|n| live.get(n).map(|s| (*n, *s)))
                .collect()
        };
        // Same rollup as `node_states`: the worst of the committed liveness and any active alert on
        // the node, and alerts whose subject is not a node belong to no node's display state.
        let wanted: BTreeSet<NodeId> = nodes.iter().copied().collect();
        for alert in self.active.lock().expect("alerts mutex poisoned").values() {
            let Some(node) = alert.node() else { continue };
            if !wanted.contains(&node) {
                continue;
            }
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

    /// Tests' shorthand for [`Self::observe_with_no_reading`] on a result that carried no vendor
    /// placeholder — every result but the ones ADR-156 is about. Production always goes through the
    /// ingest boundary, which is why this does not exist outside tests.
    #[cfg(test)]
    pub fn observe(&self, result: &PollResult) -> Vec<NotifyAction> {
        self.observe_with_no_reading(result, &[])
    }

    /// Feed one poll result through the engine: a liveness check from the outcome plus a
    /// threshold check per sample that has a resolved threshold. Returns notify actions for
    /// every committed transition (also broadcast to SSE subscribers here).
    ///
    /// This is the engine's entry point from `result_ingest`, for a result whose vendor placeholders
    /// were taken out at ingest (ADR-156).
    ///
    /// `no_reading` is what [`crate::no_reading_filter::NoReadingHandle::admit`] removed. Each one is
    /// resolved exactly like a sample, and then observed only where it is evidence: on a table row
    /// that already holds a state, as `Ok` through the rule's dwell. 🚨 **A row that is merely absent
    /// from the result is not evidence and closes nothing** (ADR-156 決定 3) — a poller defect, a
    /// truncated walk or a changed SNMP view all make a row absent, and closing on absence would take
    /// that fault off the screen.
    pub fn observe_with_no_reading(
        &self,
        result: &PollResult,
        no_reading: &[Sample],
    ) -> Vec<NotifyAction> {
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
        let mut resolved: Vec<(ResolveKey<'_>, Option<&str>, Option<EffectiveThreshold>)> =
            Vec::new();
        // Each sample's row name where it changes which rule resolves (ADR-143), kept by position so
        // the second pass — which runs after the config lock is dropped — rebuilds the same memo key.
        let mut sample_names: Vec<Option<&str>> =
            Vec::with_capacity(result.samples.len() + no_reading.len());
        // Held for the whole call. Nothing below takes this lock for writing, and `record_row_names`
        // runs before `observe` on the same task, so it cannot be waiting on it.
        let names = self.row_names.read().expect("row names rwlock poisoned");
        let node_names = names.get(&node);
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
                .resolve(node, None, None, LIVENESS)
                .map(|eff| eff.dwell_samples);
            // The placeholders resolve too, in the same memo: a row that holds a state needs its
            // rule's dwell to recover through (ADR-156 決定 4).
            for sample in result.samples.iter().chain(no_reading) {
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
                let name = Self::resolving_row_name(&config, node_names, sample, key.1);
                sample_names.push(name);
                if !resolved.iter().any(|(k, n, _)| *k == key && *n == name) {
                    let eff = config.resolve(node, key.1, name, &sample.metric);
                    resolved.push((key, name, eff));
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
        // One read of the node's poll interval for the whole result (ADR-144). A poll result is
        // one poll, so its checks count polls as they are.
        let moment = Moment {
            at_unix_ms: result.at_unix_ms,
            in_maintenance,
            cadence: Cadence::EveryPoll,
            interval: self.intervals.for_node(node.as_uuid()),
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
                row: None,
                row_name: None,
                cadence: moment.cadence,
                interval: moment.interval,
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
        // ⚠️ ADR-143 decision 4 takes the table rows back out of that fold: a sample that carries a
        // row key on a metric that is not per-interface is a memory pool, a CPU or a sensor, and gets
        // a check of its own. They are gathered here and observed together below, where the index of
        // rows holding a state decides which of them are worth observing at all (decision 5).
        let mut rows: Vec<RowObservation<'_>> = Vec::new();
        for (position, sample) in result.samples.iter().chain(no_reading).enumerate() {
            // Positions past the readings are the placeholders, in the order the first pass saw them.
            let reading = if position < result.samples.len() {
                Reading::Value
            } else {
                Reading::NoReading
            };
            let key = resolve_key(&sample.metric, sample.ifindex, &per_if_metrics);
            let name = sample_names.get(position).copied().flatten();
            let Some(eff) = resolved
                .iter()
                .find(|(k, n, _)| *k == key && *n == name)
                .and_then(|(_, _, eff)| eff.as_ref())
            else {
                continue;
            };
            match (key.1, sample.ifindex) {
                // A placeholder is evidence only about a table row (ADR-156 決定 5). A port's check
                // and a node-wide fold are left exactly as a missing sample would leave them.
                (Some(_), _) | (None, None) if reading == Reading::NoReading => {}
                // One port, one check, one observation (ADR-076).
                (Some(idx), _) => actions.extend(self.observe_threshold_sample(
                    node,
                    moment,
                    sample,
                    eff,
                    CheckOn::Port(idx),
                    Reading::Value,
                )),
                // One table row, one check (ADR-143). The name goes on the alert whether or not a
                // rule needed it to resolve, so it is looked up here rather than taken from `name`.
                (None, Some(row)) => {
                    let display = node_names
                        .and_then(|m| m.get(sample.metric.as_str()))
                        .and_then(|m| m.get(&row.0))
                        .map(String::as_str);
                    rows.push((sample, eff, row.0, display, reading));
                }
                // Node-wide: keep the worst sample **in this rule's own direction**, observe below.
                (None, None) => match folded.iter_mut().find(|(m, _, _)| *m == sample.metric) {
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
                moment,
                sample,
                eff,
                CheckOn::Node,
                Reading::Value,
            ));
        }
        if !rows.is_empty() {
            actions.extend(self.observe_rows(node, moment, rows));
        }

        // Push an incremental node-state event only when the rolled-up display state actually moved
        // (including this node's first observation) — subscribers patch the one node live instead of
        // re-fetching the whole fleet (S14).
        //
        // ⚠️ `after` is **not** `Some` merely because a liveness check ran — since ADR-097 a check
        // that has not yet confirmed anything writes no state, so both sides can be `None` and this
        // emits nothing. That is the correct silence: a device whose first poll after a restart
        // failed has told the engine nothing, and the old code broadcast `ok` for it.
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
    /// Shared by both shapes in [`Self::observe_with_no_reading`] — a port's own check and a node-wide folded one —
    /// since they differ only in which id the check carries. Written once so the counter rule, the
    /// maintenance substitution and the [`ThresholdEval`] that describes the breach cannot drift
    /// between them.
    fn observe_threshold_sample(
        &self,
        node: NodeId,
        moment: Moment,
        sample: &Sample,
        eff: &EffectiveThreshold,
        on: CheckOn<'_>,
        reading: Reading,
    ) -> Vec<NotifyAction> {
        let (check, ifindex, row, row_name) = match on {
            CheckOn::Port(idx) => (
                interface_check_id(node, idx, &sample.metric),
                Some(idx),
                None,
                None,
            ),
            CheckOn::Row(r, name) => (row_check_id(node, r, &sample.metric), None, Some(r), name),
            CheckOn::Node => {
                let id = check_id(node, &sample.metric);
                // Observed as a node-wide check, so it is not a leftover from before table rows
                // alerted on their own (ADR-143 decision 6).
                self.forget_legacy(id);
                (id, None, None, None)
            }
        };
        let raw = if moment.in_maintenance {
            NodeState::Maintenance
        } else if reading == Reading::NoReading {
            // The device says this row has nothing to measure (ADR-156 決定 4). Observed as `Ok`
            // rather than resolved on the spot, the same shape as the counter arm below: a sensor
            // that answers 85 and its placeholder on alternate polls must still reach its dwell,
            // and an open alert recovers through the ordinary path with its notification.
            NodeState::Ok
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
        // 🚨 The side the operator is told about is the side this sample actually crossed, never
        // the rule's primary side — which for a band is routinely the other one. Publishing the
        // primary side made the alert contradict itself, and it reached the test deployment before
        // anyone saw it: a value of 0.909 that tripped `critical_below: 1.0` was published as
        // `threshold: 5000.0, direction: above` (2026-08-21). 2,600 green tests did not catch it
        // because every one of them used a one-sided rule, where the two sides are the same side.
        //
        // An in-band sample falls back to the primary side. Nothing fires from one, but a resolve
        // commits with this eval in hand and a side is still required.
        let bounds = eff.bounds();
        let side = bounds
            .breaching_side(sample.value)
            .unwrap_or(eff.direction());
        let eval = ThresholdEval {
            value: sample.value,
            direction: side,
            warning: bounds.warning_on(side),
            critical: bounds.critical_on(side),
        };
        self.process_check(
            node,
            raw,
            moment.at_unix_ms,
            CheckSpec {
                check,
                metric: &sample.metric,
                dwell: eff.dwell_samples,
                is_liveness: false,
                // A threshold check exists only because a rule resolved for it.
                alerting: true,
                // A placeholder is not a value, so it describes no breach. `Ok` cannot fire, and a
                // resolve does not read this.
                eval: (reading == Reading::Value).then_some(eval),
                ifindex,
                row,
                row_name,
                cadence: moment.cadence,
                interval: moment.interval,
            },
        )
    }

    /// The row name a sample resolves under (ADR-143): only a table row — a row key on a metric that
    /// is not per-interface — and only when some rule on its metric is scoped to a row name. Every
    /// other sample resolves exactly as it did, with no lookup.
    fn resolving_row_name<'n>(
        config: &AlertConfig,
        node_names: Option<&'n HashMap<String, HashMap<u32, String>>>,
        sample: &Sample,
        port: Option<IfIndex>,
    ) -> Option<&'n str> {
        if port.is_some() || !config.has_row_rules(&sample.metric) {
            return None;
        }
        let row = sample.ifindex?;
        node_names?
            .get(sample.metric.as_str())?
            .get(&row.0)
            .map(String::as_str)
    }

    /// Observe one result's table rows, each on its own check (ADR-143 decisions 4–6).
    ///
    /// 🚨 **A healthy row that holds no state is skipped**, and that is what keeps this affordable: a
    /// Huawei stack reports 306 entity rows per metric, nearly all of them zero, and a state per row
    /// per metric per node is tens of millions at fleet scale. Skipping is exact rather than an
    /// approximation — a fresh state is `Ok`, and observing `Ok` on it commits nothing — so the only
    /// thing that has to be right is the index of rows that do hold one.
    fn observe_rows(
        &self,
        node: NodeId,
        moment: Moment,
        rows: Vec<RowObservation<'_>>,
    ) -> Vec<NotifyAction> {
        let mut actions = Vec::new();
        // Decision 6: the node-wide alert a metric had before its rows alerted on their own is closed
        // by the first per-row observation of that metric. Asked once per metric.
        let mut metrics_seen: Vec<&str> = Vec::new();
        for (sample, ..) in &rows {
            if !metrics_seen.contains(&sample.metric.as_str()) {
                metrics_seen.push(sample.metric.as_str());
                actions.extend(self.retire_legacy_node_check(node, &sample.metric));
            }
        }
        // Decision 5: which rows already hold a state. One lock, released before any observation.
        let held: Vec<bool> = {
            let states = self.row_states.lock().expect("row states mutex poisoned");
            let node_rows = states.get(&node);
            rows.iter()
                .map(|(sample, _, row, _, _)| {
                    node_rows
                        .and_then(|m| m.get(sample.metric.as_str()))
                        .is_some_and(|set| set.contains(row))
                })
                .collect()
        };
        let mut newly_held: Vec<(String, u32)> = Vec::new();
        for ((sample, eff, row, name, reading), held) in rows.into_iter().zip(held) {
            // A counter is never evaluated (ADR-012), maintenance breaches nothing, and a placeholder
            // is not a value (ADR-156), so none of them is a reason to create a state — only to keep
            // feeding one that exists.
            let breaching = reading == Reading::Value
                && !moment.in_maintenance
                && sample.kind != MetricKind::Counter
                && eff.evaluate(sample.value) != NodeState::Ok;
            if !breaching && !held {
                continue;
            }
            actions.extend(self.observe_threshold_sample(
                node,
                moment,
                sample,
                eff,
                CheckOn::Row(row, name),
                reading,
            ));
            if !held {
                newly_held.push((sample.metric.clone(), row));
            }
        }
        if !newly_held.is_empty() {
            let mut states = self.row_states.lock().expect("row states mutex poisoned");
            let node_rows = states.entry(node).or_default();
            for (metric, row) in newly_held {
                node_rows.entry(metric).or_default().insert(row);
            }
        }
        actions
    }

    /// Close the node-wide alert `metric` had on `node` before its table rows alerted on their own,
    /// if one was restored (ADR-143 decision 6).
    ///
    /// The close is an ordinary resolve, so an external incident opened on the old check id is
    /// closed too rather than left open forever. A row that is still breaching then fires on its own
    /// check. Costs one lock and nothing else once no restored node-wide alert is left.
    fn retire_legacy_node_check(&self, node: NodeId, metric: &str) -> Vec<NotifyAction> {
        let legacy = {
            let mut set = self
                .legacy_node_checks
                .lock()
                .expect("legacy checks mutex poisoned");
            if set.is_empty() {
                return Vec::new();
            }
            let id = check_id(node, metric);
            if !set.remove(&id) {
                return Vec::new();
            }
            id
        };
        self.resolve_orphans(vec![legacy])
    }

    /// A node-wide check was observed as one, so it is not a leftover waiting to be retired.
    fn forget_legacy(&self, check: CheckId) {
        let mut set = self
            .legacy_node_checks
            .lock()
            .expect("legacy checks mutex poisoned");
        if !set.is_empty() {
            set.remove(&check);
        }
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
            row,
            row_name,
            cadence,
            interval,
        } = spec;
        // The rule's dwell counts polls. A check read once a tick counts enough ticks to span that
        // many polls, and the flap window spans twenty of them (ADR-144). This is the one place
        // either conversion happens; with no interval published both come out as they always were.
        let dwell = match cadence {
            Cadence::EveryPoll => dwell,
            Cadence::EveryTick(tick) => poll_interval::dwell_ticks(dwell, interval, tick),
        };
        let flap_window = poll_interval::flap_window_ms(interval);
        let (transition, observed) = {
            let mut states = self.states.lock().expect("states mutex poisoned");
            let cs = states.entry(check).or_insert_with(|| {
                CheckState::new(NodeState::Ok, dwell.max(1), flap_window, FLAP_THRESHOLD)
            });
            // A check's state lives for the process, so the dwell captured at first observation
            // would otherwise outlive every edit to the rule that set it — an operator raising
            // "3 breaches" to "5" would see no effect until the next core restart, with the UI
            // showing 5. Re-point it every observation instead (ADR-075). The flap window follows
            // the node's poll interval the same way.
            cs.set_dwell(dwell.max(1));
            cs.set_flap_window_ms(flap_window);
            let t = cs.observe(raw, at_unix_ms);
            (t, cs.observed())
        };

        // Keep the per-node committed liveness current even when nothing transitioned (a
        // node's first reachable poll commits `ok` with no transition, but the inventory
        // still needs to read it as `ok`). Capture whether this node's *down-set membership*
        // flipped (entered or left `Unreachable`) — that's exactly when downstream dependency
        // suppression must be re-evaluated (a parent going down/up changes its children's roll-up).
        //
        // 🚨 `observed()` rather than `committed()`, and that one word is ADR-097. A check the
        // engine has never seen conclude still *holds* a state — the `Ok` seed `CheckState::new`
        // has to start from, because a transition away from it is what fires an alert. Writing that
        // seed here published it as the node's display state, so after a core restart every node
        // read `ok` until it had failed `dwell` times: measured five minutes after a restart, 15 of
        // 22 stopped devices were reported healthy, and `/flashdeploy`'s own health check runs
        // inside that window. An unconfirmed check writes nothing at all, which leaves the node
        // absent from `live` — exactly the state `nodes::state_or_fallback` already answers for
        // ("a recent liveness sample means ok, silence means unknown"), so no caller changes.
        let down_set_changed = match (is_liveness, observed) {
            (true, Some(committed)) => {
                let previous = self
                    .live
                    .lock()
                    .expect("live mutex poisoned")
                    .insert(node, committed);
                let flipped = matches!(previous, Some(NodeState::Unreachable))
                    != matches!(committed, NodeState::Unreachable);
                if flipped {
                    // Keep the incremental down-set in lockstep with `live` (its only mutation
                    // site).
                    let mut down = self.down.lock().expect("down mutex poisoned");
                    if matches!(committed, NodeState::Unreachable) {
                        down.insert(node);
                    } else {
                        down.remove(&node);
                    }
                }
                flipped
            }
            // Not a liveness check, or a liveness check still holding its seed. Neither can move
            // the down-set: an unconfirmed check is `Ok` only because it had to start somewhere.
            _ => false,
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

        // Who this alert's incident belongs to (ADR-015, widened by ADR-087).
        //
        // Two cases, and they are the same idea one level apart:
        //
        // - **liveness**: if this node is down and every upstream is also down, attribute it to the
        //   highest down ancestor, so it groups under *that* incident.
        // - **anything else on a node that is already down** (ADR-087): attribute it to **the node
        //   itself**. The incident is "node X is down", and `snmp_up` going to 0 is part of that
        //   outage rather than a second one. Before this, a single device falling over opened two
        //   incidents in PagerDuty/JSM — `dedup_string` carries the check id, so they do not merge —
        //   and 13 nodes were in exactly that state when this was measured. `repo.rs`'s built-in
        //   rule table has always said two criticals for one outage is a notification flood and
        //   that this project treats that as a bug.
        //
        // ⚠️ `root_cause` therefore no longer means "an *upstream* node". It means "the node whose
        // outage this alert is part of", which may be this node. Nothing downstream had to change:
        // `Notifier` keys its skip on `Some(_)` without looking at which node, and the close-on-
        // rollup path (`NotifyAction::Suppress`) is the same either way.
        let root_cause = if is_liveness {
            t.state
                .is_problem()
                .then(|| {
                    let down = self.down_set();
                    self.config
                        .read()
                        .expect("config rwlock poisoned")
                        .topology
                        .root_cause(node, &down)
                })
                .flatten()
        } else {
            self.down_set().contains(&node).then_some(node)
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
                // Which table row, and what it was called when this fired (ADR-143). Descriptive, like
                // the port: the check id already names the row.
                alert.row = row;
                alert.row_name = row_name.map(str::to_owned);
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
    /// Liveness alerts of `changed`'s descendants, **plus `changed`'s own non-liveness alerts**
    /// (ADR-087). Bounded by the current active-alert count; runs only when a node actually
    /// entered/left `Unreachable`.
    ///
    /// The second half is what makes ADR-087 work in both directions. A node's `snmp_up` alert can
    /// commit *before* its liveness does — measured across 13 down nodes, `snmp_up` won 7 times,
    /// liveness 4, and they tied twice — so attributing at fire time alone would leave the earlier
    /// one paging standalone forever. Reconsidering it here turns that into "page once, then close",
    /// which is exactly what a child alert that beat its parent's dwell already does.
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
                // pool-coverage alert has no ancestor to be attributed to — and `changed`s own
                // roll-up is about `changed` as a node too.
                .filter(|a| {
                    a.node().is_some_and(|n| {
                        if a.metric == LIVENESS {
                            affected.contains(&n)
                        } else {
                            // ADR-087: everything else this node is complaining about belongs to
                            // this node's outage. Only `changed` 's own — a sibling's threshold
                            // alert is unaffected by `changed` flipping.
                            n == changed
                        }
                    })
                })
                .cloned()
                .collect()
        };
        let mut actions = Vec::new();
        for alert in candidates {
            let Some(alert_node) = alert.node() else {
                continue;
            };
            // A liveness alert climbs the dependency graph; anything else rolls up into its own
            // node's outage, which is present or absent exactly as that node is in the down set.
            let new_rc = if alert.metric == LIVENESS {
                self.config
                    .read()
                    .expect("config rwlock poisoned")
                    .topology
                    .root_cause(alert_node, &down)
            } else {
                down.contains(&alert_node).then_some(alert_node)
            };
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
    /// the damping upstream in `events/engine.rs`), so this bypasses `process_check` on purpose.
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
    /// (ADR-050 decision 12), and that is deliberate.** The gate lives in [`Self::observe_with_no_reading`] and
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
            // A pool is not a port, nor a table row.
            ifindex: None,
            row: None,
            row_name: None,
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
                config.resolve(node, Some(ifindex), None, metric),
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
                row: None,
                row_name: None,
                // Read once a tick by the utilisation evaluator, not once a poll (ADR-144).
                cadence: Cadence::EveryTick(crate::interface_util::WATCH_TICK),
                interval: self.intervals.for_node(node.as_uuid()),
            },
        ))
    }

    /// Feed a **derived node metric** through the state machine (ADR-105).
    ///
    /// The node-dimension twin of [`Self::observe_interface_metric`]: same maintenance handling,
    /// same "`None` means nobody is watching", but a node-wide [`check_id`] and no port.
    ///
    /// `values` is every row the evaluator computed for this node — a filesystem each for
    /// `hr_storage_used_pct`, a memory pool each for `cisco_mem_used_pct`, one entry for a scalar.
    /// They are folded to **one** observation here, under this rule's own direction, exactly as
    /// [`Self::observe_with_no_reading`] folds a table walk's samples (ADR-077 decision 1). 🚨 The caller must not
    /// fold and must not call once per row: N observations in one dwell window is the ADR-076 bug —
    /// one bad row among good ones has its candidate reset by the next row and never reaches the
    /// dwell, while N bad rows satisfy a 3-sample dwell inside a single tick.
    ///
    /// # What the caller must decide before calling
    ///
    /// 🚨 A node whose **liveness** is not `Ok` must not be observed at all, for the reasons spelled
    /// out on [`Self::observe_interface_metric`]. Read [`Self::node_liveness`] through
    /// [`crate::interface_util::may_observe_ports`] — **never [`Self::node_state`]**, which folds in
    /// the very alert this call is about to raise.
    pub fn observe_derived_metric(
        &self,
        node: NodeId,
        metric: &'static str,
        rows: &[(i64, f64)],
        at_unix_ms: i64,
    ) -> Option<Vec<NotifyAction>> {
        if !crate::interface_util::may_observe_ports(self.node_liveness(node)) {
            // Frozen, not observed. Feeding a value would either resolve a real alert the moment
            // the device went unreachable, or page about memory on a box that is already down.
            return None;
        }
        // A table metric is one check per row (ADR-143); only a metric computed from scalars still
        // folds its rows — which is one row — into the node's check below.
        if crate::derived::derived_node_metric(metric).is_some_and(|d| d.per_row) {
            return self.observe_derived_rows(node, metric, rows, at_unix_ms);
        }
        let values: Vec<f64> = rows.iter().map(|(_, v)| *v).collect();
        let (eff, in_maintenance) = {
            let config = self.config.read().expect("config rwlock poisoned");
            (
                config.resolve(node, None, None, metric),
                config.maintenance.contains(&node),
            )
        };
        // `None`, not an empty vector: the caller distinguishes "nobody is watching this metric"
        // from "somebody is watching and nothing happened".
        let eff = eff?;
        // The worst row wins, ranked by the rule's own bounds rather than by magnitude — "highest
        // is worst" is false for any rule whose fault direction is `below` (ADR-081).
        let value = values.iter().copied().reduce(|incumbent, candidate| {
            if eff.is_worse(candidate, incumbent) {
                candidate
            } else {
                incumbent
            }
        })?;
        let raw = if in_maintenance {
            NodeState::Maintenance
        } else {
            eff.evaluate(value)
        };
        let check = check_id(node, metric);
        self.forget_legacy(check);
        Some(self.process_check(
            node,
            raw,
            at_unix_ms,
            CheckSpec {
                check,
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
                ifindex: None,
                row: None,
                row_name: None,
                // Read once a tick by the derived-metric evaluator, not once a poll (ADR-144).
                cadence: Cadence::EveryTick(crate::derived::WATCH_TICK),
                interval: self.intervals.for_node(node.as_uuid()),
            },
        ))
    }

    /// The per-row half of [`Self::observe_derived_metric`] (ADR-143): each row resolves on its own
    /// — under its name, when a rule on the metric is scoped to one — and is observed on its own
    /// check through the same [`Self::observe_rows`] the poll path uses. A row's name is the name of
    /// the same row of the formula's first input, which is the series the poller named.
    fn observe_derived_rows(
        &self,
        node: NodeId,
        metric: &'static str,
        rows: &[(i64, f64)],
        at_unix_ms: i64,
    ) -> Option<Vec<NotifyAction>> {
        let input = crate::derived::derived_node_metric(metric).map(|d| d.formula.inputs()[0])?;
        let names = self.row_names.read().expect("row names rwlock poisoned");
        let input_names = names.get(&node).and_then(|m| m.get(input));
        // A key outside `u32` cannot have come from a walk; it is dropped rather than wrapped onto
        // another row's check.
        let samples: Vec<(Sample, u32, Option<&str>)> = rows
            .iter()
            .filter_map(|&(row, value)| {
                let row = u32::try_from(row).ok()?;
                let name = input_names.and_then(|m| m.get(&row)).map(String::as_str);
                Some((Sample::gauge(metric, value), row, name))
            })
            .collect();
        let (in_maintenance, effs) = {
            let config = self.config.read().expect("config rwlock poisoned");
            let by_name = config.has_row_rules(metric);
            let mut memo: Vec<(Option<&str>, Option<EffectiveThreshold>)> = Vec::new();
            let effs: Vec<Option<EffectiveThreshold>> = samples
                .iter()
                .map(|(_, _, name)| {
                    let key = if by_name { *name } else { None };
                    if let Some((_, eff)) = memo.iter().find(|(k, _)| *k == key) {
                        return eff.clone();
                    }
                    let eff = config.resolve(node, None, key, metric);
                    memo.push((key, eff.clone()));
                    eff
                })
                .collect();
            (config.maintenance.contains(&node), effs)
        };
        // `None` still means "nobody is watching this metric on this node", as for the node-wide form.
        if effs.iter().all(Option::is_none) {
            return None;
        }
        let observed: Vec<RowObservation<'_>> = samples
            .iter()
            .zip(&effs)
            .filter_map(|((sample, row, name), eff)| {
                Some((sample, eff.as_ref()?, *row, *name, Reading::Value))
            })
            .collect();
        let moment = Moment {
            at_unix_ms,
            in_maintenance,
            // Read once a tick by the derived-metric evaluator, not once a poll (ADR-144).
            cadence: Cadence::EveryTick(crate::derived::WATCH_TICK),
            interval: self.intervals.for_node(node.as_uuid()),
        };
        Some(self.observe_rows(node, moment, observed))
    }

    /// What the threshold rules in force for `metric` cover — an evaluator plans its query from
    /// this rather than re-reading `ThresholdStore`, so the rules the query was built for and the
    /// rules the classification uses are the same snapshot.
    ///
    /// **Not interface-specific**, and never was: it reads the rule index by metric name and asks
    /// each rule's scope level which nodes it reaches. ADR-076 named it after its first caller;
    /// ADR-105 gave it a second one (the node-level derived-metric evaluator) and dropped the
    /// prefix rather than shipping a byte-identical copy under another name.
    #[must_use]
    pub fn rule_coverage(&self, metric: &str) -> RuleCoverage {
        self.config
            .read()
            .expect("config rwlock poisoned")
            .rule_coverage(metric)
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
    /// Nothing polls `if_in_util_pct`, so [`Self::observe_with_no_reading`] never visits `metric@ifindex` for a
    /// derived metric and no poll can close one. Without this sweep, deleting a port rule left its
    /// alert open in the UI and its incident open in whatever external tool the dedup key reached,
    /// for the life of the process.
    ///
    /// **Derived metrics only, and this sweep owns the per-port dimension.** A *collected* per-port
    /// alert whose rule was deleted is left to [`Self::resolve_orphaned_collected_alerts`] — see the
    /// ownership table there, which is the whole answer to "exactly one closer per alert".
    ///
    /// 🚨 **This doc used to say the poll path already closed a collected metric's alert. It does
    /// not, and never did** (ADR-097 Increment 6). `observe` `continue`s a sample whose threshold
    /// does not resolve *before* `process_check` is reached, and `observe_threshold_sample` hard-
    /// codes `alerting: true`, so the `!alerting` close branch is reachable only by the liveness
    /// check. The two tests that encoded that belief asserted only that this sweep refuses the
    /// alert; neither re-polled, so neither could see that nobody else took it.
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
                        .resolve(node, Some(ifindex), None, metric)
                        .is_none()
                        .then_some(a.check)
                })
                .collect()
        };
        self.resolve_orphans(orphans)
    }

    /// The same sweep for a **node-level** derived metric (ADR-105).
    ///
    /// Same reason as its interface sibling and the same shape, but the two filters are genuinely
    /// different — one asks about `metric@ifindex`, the other about a node-wide check — so they
    /// stay two functions over one tail rather than one function with a dimension argument. Each
    /// watch sweeps its own dimension, which is what keeps exactly one closer per alert.
    ///
    /// Found on the deployment, not in a test: the verification rule for `huawei_mem_used_pct` was
    /// deleted and its warning stayed open for the life of the process, because nothing polls a
    /// derived metric and so [`Self::observe_with_no_reading`]'s `!alerting` branch never visits its check.
    pub fn resolve_orphaned_node_derived_alerts(&self) -> Vec<NotifyAction> {
        let orphans: Vec<CheckId> = {
            let config = self.config.read().expect("config rwlock poisoned");
            let active = self.active.lock().expect("alerts mutex poisoned");
            active
                .values()
                .filter_map(|a| {
                    let node = a.node()?;
                    // A node-wide check. An alert carrying a port belongs to the other sweep, and
                    // a node-derived metric never carries one.
                    if a.ifindex.is_some() {
                        return None;
                    }
                    let metric = crate::derived::derived_node_metric(&a.metric)?.name;
                    // A row alert asks under the name it fired under (ADR-143): a rule scoped to
                    // that name is still a rule for it, and asking with no name would call it gone.
                    config
                        .resolve(node, None, a.row_name.as_deref(), metric)
                        .is_none()
                        .then_some(a.check)
                })
                .collect()
        };
        self.resolve_orphans(orphans)
    }

    /// Resolve every active alert about a **collected** metric whose rule no longer resolves
    /// (ADR-097 Increment 6).
    ///
    /// # The defect this closes, which the tree believed was already closed
    ///
    /// [`Self::observe_with_no_reading`] `continue`s a sample whose threshold does not resolve *before*
    /// [`Self::process_check`] is reached, and `observe_threshold_sample` hard-codes
    /// `alerting: true`. So `process_check`'s `!alerting` branch — the one whose own doc says it
    /// exists to close a stranded alert — is reachable **only** by the liveness check. Deleting a
    /// threshold rule for `snmp_up`, `icmp_loss_pct` or any other collected metric left its alert
    /// open in the UI, and its incident open in whatever external tool the dedup key reached, for
    /// the life of the process. Nothing closed it. Two doc comments and two tests said otherwise;
    /// both tests asserted only that the *derived* sweeps refuse the alert, and neither re-polled,
    /// so neither could see that no other closer existed.
    ///
    /// # Which sweep closes what, and why there is exactly one of each
    ///
    /// | closer | closes because | over |
    /// |---|---|---|
    /// | `process_check`'s `!alerting` branch | rule gone | `__liveness__` — the only check that reaches it |
    /// | [`Self::resolve_orphaned_interface_alerts`] | rule gone | derived per-port |
    /// | [`Self::resolve_orphaned_node_derived_alerts`] | rule gone | derived node-wide |
    /// | **this** | rule gone | **collected, both dimensions** |
    /// | the freshness sweep (`alerts::stale`) | **data gone** | collected + derived node-wide |
    /// | the poll path, as `Ok` through dwell | **the device answered its no-reading placeholder** (ADR-156) | collected table rows |
    /// | nobody, **deliberately** | a table row stopped arriving while its metric did not | collected table rows (ADR-156 決定 3: absence is also what a monitoring fault looks like; ADR-143's remnant) |
    /// | [`Self::forget_deleted_nodes`] | node gone | any node subject |
    /// | `events::engine` | the event rule's own lifecycle | `event:*` |
    /// | `pool_coverage` | the pool recovered | [`Subject::Pool`] |
    ///
    /// The split here is **derived vs collected**, not node vs port: a rule lookup answers a
    /// per-port question as readily as a node-wide one, so there is no reason to leave a collected
    /// `if_oper_status@7` stranded. The freshness sweep is the one that must stay node-only, and
    /// for a different reason — the store answers about nodes.
    ///
    /// 🚨 **Safe only because a failed config load no longer degrades to "no rules" (ADR-080)**,
    /// exactly as for its two derived siblings, and over a strictly larger set: if a database blip
    /// could still empty the ruleset, this sweep would resolve every threshold alert in the fleet
    /// and send a recovery for each.
    ///
    /// 🚨 **And because an engine with no config installed sweeps nothing.** `AlertConfig::default`
    /// is what this type holds before the first refresh, and [`Self::restore`] has already seeded
    /// the active set by then — so without the guard below, the first tick after every start would
    /// resolve every threshold alert in the fleet and page a recovery for each. That is ADR-097
    /// decision 7's lesson, and the guard lives **here rather than at the call site** for the same
    /// reason: it has to hold for any caller, and most of this module's own tests configure the
    /// manager with an empty map.
    pub fn resolve_orphaned_collected_alerts(&self) -> Vec<NotifyAction> {
        let orphans: Vec<CheckId> = {
            let config = self.config.read().expect("config rwlock poisoned");
            if config.node_meta.is_empty() {
                return Vec::new();
            }
            let active = self.active.lock().expect("alerts mutex poisoned");
            active
                .values()
                .filter_map(|a| {
                    let node = a.node()?;
                    if !Self::is_collected_threshold_alert(a) {
                        return None;
                    }
                    config
                        .resolve(node, a.ifindex, a.row_name.as_deref(), &a.metric)
                        .is_none()
                        .then_some(a.check)
                })
                .collect()
        };
        self.resolve_orphans(orphans)
    }

    /// Whether this alert is one a **threshold rule** raised, rather than one some other part of
    /// the system owns end to end.
    ///
    /// The two rejections both sweeps of ADR-097 Increment 6 share, in one function so they cannot
    /// come to disagree. Each names the closer that owns what it drops; the full table is in
    /// [`Self::resolve_orphaned_collected_alerts`]. The caller has already established that the
    /// subject is a node.
    fn is_threshold_alert(a: &Alert) -> bool {
        // Liveness is `process_check`'s, and it is the one check that actually reaches the
        // `!alerting` branch.
        if a.metric == LIVENESS {
            return false;
        }
        // A passive-event alert has no series and no threshold rule; `events::engine` owns its
        // whole lifecycle, and closing one from outside permanently suppresses the rule's re-fire.
        if a.metric.starts_with(crate::events::EVENT_METRIC_PREFIX) {
            return false;
        }
        true
    }

    /// Whether this alert is about a metric a **poller collects**, as opposed to one Yagra computes.
    ///
    /// The extra rejection the *rule-gone* sweep needs and the freshness sweep must not copy: the
    /// two derived dimensions already have a rule-gone closer each, while for **freshness** a
    /// derived metric is very much in scope — it is simply asked about by its inputs, because it is
    /// never stored (see [`Self::freshness_candidates`]).
    fn is_collected_threshold_alert(a: &Alert) -> bool {
        Self::is_threshold_alert(a)
            && crate::derived::derived_node_metric(&a.metric).is_none()
            && crate::interface_util::derived_metric_name(&a.metric).is_none()
    }

    /// Every open alert whose metric a store could still be asked about, and the series that would
    /// answer (ADR-097 Increment 6).
    ///
    /// The engine cannot decide this on its own — "is anything still measuring this?" is a fact
    /// about the data, and the engine's own memory of what it has observed is exactly what a
    /// restart destroys. So this half only says *which* alerts are worth asking about and *what to
    /// ask*; `alerts::stale` asks, and the two windows and the safety canary live there.
    ///
    /// # What is dropped, and who owns it instead
    ///
    /// | dropped | closer that owns it |
    /// |---|---|
    /// | [`Subject::Pool`] | `pool_coverage` — a pool has no series and no inventory row |
    /// | a node absent from `node_meta` | [`Self::forget_deleted_nodes`] |
    /// | `__liveness__` | `process_check` |
    /// | `event:*` | `events::engine`, whose runtime set must not diverge |
    /// | anything carrying an `ifindex` | nobody, deliberately — ADR-097 decision 16 |
    ///
    /// The port dimension is out of scope because the store answers about **nodes**: a node-level
    /// freshness query cannot tell "port 7's `if_oper_status` stopped" from "port 8's did not". A
    /// per-port answer needs a new `MetricStore` method and a new query shape, which is its own
    /// increment. Both boxes were measured before this shipped and every stranded alert on them was
    /// node-dimension.
    ///
    /// # 🚨 A derived metric is asked about by its **inputs**
    ///
    /// A derived metric is computed and never stored (ADR-105), so asking the store for
    /// `cisco_cemp_mem_used_pct` returns nothing on a **perfectly healthy** node. Keying freshness
    /// on the alert's own name would therefore close the entire derived dimension on the first
    /// tick. `Formula::inputs` names the two series that really do arrive, and either one going
    /// missing is enough to say the evaluator has nothing left to work from.
    ///
    /// ⚠️ The same trap caught the *investigation*: "no samples in 30 days" was written down as
    /// evidence that a derived alert was stranded, and it is equally true of one that is fine.
    ///
    /// 🚨 **An engine with no config installed answers nothing**, for ADR-097 decision 7's reason,
    /// and the guard is inside this method so no caller can skip it.
    pub fn freshness_candidates(&self) -> Vec<FreshnessCandidate> {
        let config = self.config.read().expect("config rwlock poisoned");
        if config.node_meta.is_empty() {
            return Vec::new();
        }
        let active = self.active.lock().expect("alerts mutex poisoned");
        active
            .values()
            .filter_map(|a| {
                let node = a.node()?;
                if !config.node_meta.contains_key(&node) {
                    return None;
                }
                if !Self::is_threshold_alert(a) || a.ifindex.is_some() {
                    return None;
                }
                let inputs = match crate::derived::derived_node_metric(&a.metric) {
                    Some(d) => {
                        let [x, y] = d.formula.inputs();
                        // `Formula::Complement` repeats its one input; the array is fixed-width so
                        // no caller has to branch on which shape it got.
                        match x == y {
                            true => vec![x.to_owned()],
                            false => vec![x.to_owned(), y.to_owned()],
                        }
                    }
                    None => vec![a.metric.clone()],
                };
                Some(FreshnessCandidate {
                    check: a.check,
                    node,
                    metric: a.metric.clone(),
                    inputs,
                })
            })
            .collect()
    }

    /// Close the checks a freshness sweep found nothing measuring.
    ///
    /// Separate from [`Self::freshness_candidates`] because the decision is made outside, against a
    /// store, with `.await`s in between — which is why [`Self::resolve_orphans`] resolves before it
    /// drops a dwell window, and why `resolve_event_alert`'s `remove(&check)?` returning `None`
    /// makes a check that recovered over that gap a no-op rather than a wrong close.
    pub fn resolve_stale_alerts(&self, checks: Vec<CheckId>) -> Vec<NotifyAction> {
        self.resolve_orphans(checks)
    }

    /// Close every alert about a node the inventory no longer holds, and forget what the engine
    /// still believed about that node (ADR-097 Increment 4).
    ///
    /// The third sweep of this shape, and the one whose subject is the **node** rather than the
    /// rule. Its motivating failure is different from its two siblings': deleting a node produces no
    /// poll result, so [`Self::observe_with_no_reading`]'s `!alerting` branch is never reached for any of its checks
    /// and there is no path by which a deleted node's alert can ever resolve. ADR-097 decision 4
    /// made that invisible rather than harmless — `open_alerts` drops a row whose node is gone, so a
    /// restart makes the alert **disappear without ever having been resolved**, and whatever
    /// external tool its dedup key reached never hears that it closed.
    ///
    /// # Two halves, and the second is not optional
    ///
    /// The first resolves the alerts, which is what closes the incident. The second drops the node
    /// from `live`/`down`, which is what makes the fleet tally add up again:
    /// `api/fleet.rs::state_tally` takes its total from PostgreSQL and its per-state breakdown from
    /// this engine, so a node present in one and not the other makes the breakdown sum to more than
    /// the total. Measured on the lab core 2026-08-30 — 15,000 synthetic nodes deleted, and the live
    /// suite's "the fleet summary does not add up to its own total" went red. A deleted node that
    /// never had an alert is still in `live`, so the first half alone fixes nothing there.
    ///
    /// # Why an empty map is a no-op
    ///
    /// 🚨 **An empty `node_meta` means "no config has been installed yet", never "the fleet is
    /// empty".** [`AlertConfig::default`] is what this type holds before the first refresh, and
    /// [`Self::restore`] has already seeded the active set by then — so without this guard the first
    /// cycle after every start would resolve every alert in the fleet and page a recovery for each.
    /// That is the accident ADR-080 paid for once. The guard lives **here rather than at the call
    /// site** on purpose: it has to hold for any caller, and most of this module's own tests
    /// configure the manager with an empty map.
    ///
    /// ⚠️ **The map is never *partially* loaded.** `alerts::config::load_alert_config_base`
    /// propagates every read with `?`, so the snapshot is either a complete `list_nodes` scan or the
    /// previous one unchanged — the same property the two sweeps above depend on.
    ///
    /// ⚠️ **A node deleted outside the API stays invisible until something bumps the config
    /// generation**, because that is what rebuilds `node_meta`. Every operator-facing delete goes
    /// through the API; a direct `DELETE FROM nodes` (the load-test teardown) needs a core restart.
    ///
    /// ⚠️ **What this deliberately does not reclaim**: the [`CheckState`] of a check that had no
    /// active alert. `CheckId` is a one-way UUIDv5 of `(node, metric)`, so a deleted node's checks
    /// cannot be enumerated. It is memory only — nothing will ever observe that check again, and a
    /// node re-created with the same id has its committed state dropped by [`Self::resolve_orphans`]
    /// along with its alert.
    pub fn forget_deleted_nodes(&self) -> Vec<NotifyAction> {
        let (orphans, gone) = {
            let config = self.config.read().expect("config rwlock poisoned");
            if config.node_meta.is_empty() {
                return Vec::new();
            }
            // `active` and `live` are collected one at a time. The two are never held together
            // anywhere in this type — `restore` and `process_check` each release one before taking
            // the other — and this is not the place to start.
            let orphans: Vec<CheckId> = {
                let active = self.active.lock().expect("alerts mutex poisoned");
                active
                    .values()
                    .filter_map(|a| {
                        // A non-node subject (a poller pool) has no inventory row that could be
                        // missing, so it is not this sweep's to close.
                        let node = a.node()?;
                        (!config.node_meta.contains_key(&node)).then_some(a.check)
                    })
                    .collect()
            };
            let gone: Vec<NodeId> = {
                let live = self.live.lock().expect("live mutex poisoned");
                live.keys()
                    .filter(|n| !config.node_meta.contains_key(n))
                    .copied()
                    .collect()
            };
            (orphans, gone)
        };
        if !gone.is_empty() {
            {
                let mut live = self.live.lock().expect("live mutex poisoned");
                for node in &gone {
                    live.remove(node);
                }
            }
            {
                let mut down = self.down.lock().expect("down mutex poisoned");
                for node in &gone {
                    down.remove(node);
                }
            }
            // What a deleted node's rows were called and which of them held a state (ADR-143) —
            // forgotten with the node, each lock on its own.
            {
                let mut names = self.row_names.write().expect("row names rwlock poisoned");
                for node in &gone {
                    names.remove(node);
                }
            }
            let mut rows = self.row_states.lock().expect("row states mutex poisoned");
            for node in &gone {
                rows.remove(node);
            }
        }
        let actions = self.resolve_orphans(orphans);
        if !actions.is_empty() || !gone.is_empty() {
            tracing::info!(
                resolved = actions.len(),
                forgotten = gone.len(),
                "closed the alerts of nodes the inventory no longer holds"
            );
        }
        actions
    }

    /// Close each orphaned check and forget its dwell window.
    ///
    /// 🚨 Drop the dwell/flap bookkeeping along with the alert, or the check goes permanently
    /// silent. Resolving only the alert leaves the state machine committed at `Warning` while
    /// nothing is active, so a rule recreated on the same target observes `Warning → Warning`,
    /// sees no transition, and **never fires again** for the life of the process.
    ///
    /// Found on the test server, not by the unit test that shipped with the first sweep: the alert
    /// closed on deletion, the rule was recreated at 1%, the port sat at 6.7% for eight minutes and
    /// nothing happened. The test only asserted the sweep was idempotent — which it was, on a check
    /// that could no longer do anything.
    ///
    /// Removing the entry rather than resetting it is the honest form: the rule that set this
    /// check's dwell is gone, so its window carries no meaning to preserve.
    ///
    /// ⚠️ **The alert is resolved first, and the dwell window is dropped only if it was still
    /// open.** The three sweeps that predate ADR-097 Increment 6 collect their orphans and call
    /// this with no `.await` in between, so for them the two orders are the same. The freshness
    /// sweep is the first to ask a store between reading `active` and acting — and over that gap a
    /// check can recover through the poll path. Dropping `states` unconditionally would then throw
    /// away the dwell window of a **live** check that has nothing wrong with it, which is the same
    /// permanent silence this function's whole doc is about, arrived at from the other side.
    fn resolve_orphans(&self, orphans: Vec<CheckId>) -> Vec<NotifyAction> {
        orphans
            .into_iter()
            .filter_map(|check| {
                let action = self.resolve_event_alert(check)?;
                self.states
                    .lock()
                    .expect("states mutex poisoned")
                    .remove(&check);
                Some(action)
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

#[cfg(test)]
mod tests {
    use super::super::testkit::*;
    use super::super::NodeMeta;
    use super::*;
    use yagra_common::{ScopeLevel, ThresholdBounds};
    use yagra_topology::Topology;
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

    /// ADR-097, the **accepting** side — and it is written first on purpose. A ban that also
    /// rejected the healthy case would pass a suite in which the engine simply never reports
    /// anything (`rejection-only-tests-pass-when-everything-rejects`).
    ///
    /// A device that answers its first poll agrees with the seed, so it is confirmed at once and
    /// reads exactly as it did before this ADR.
    #[test]
    fn a_node_that_answers_its_first_poll_is_ok_immediately() {
        let mgr = manager();
        let node = NodeId::new();
        mgr.observe(&result(node, CheckOutcome::Reachable, 0));
        assert_eq!(mgr.node_liveness(node), Some(NodeState::Ok));
        assert_eq!(mgr.node_state(node), Some(NodeState::Ok));
    }

    /// The defect ADR-097 exists for: a check has to be *seeded* with `Ok` so that a transition
    /// away from it can fire, and that seed used to be published as the node's state. After a core
    /// restart every check is rebuilt from the seed, so the whole fleet read `ok` — measured on the
    /// test server, five minutes after a restart 15 of 22 stopped devices were reported healthy.
    ///
    /// A node whose first poll *fails* has told the engine nothing. It must have no state at all,
    /// which is what `nodes::state_or_fallback` already answers for.
    #[test]
    fn a_node_whose_first_poll_fails_has_no_state_rather_than_ok() {
        let mgr = manager();
        let node = NodeId::new();
        mgr.observe(&result(node, CheckOutcome::Unreachable, 0));
        assert_eq!(
            mgr.node_liveness(node),
            None,
            "the seed is held, never published"
        );
        assert_eq!(mgr.node_state(node), None, "and the roll-up says so too");
        assert!(
            !mgr.down_set().contains(&node),
            "an unconfirmed check cannot move the down-set either"
        );

        // Two more failures reach the dwell, and only then does a state exist.
        for i in 1..DEFAULT_LIVENESS_DWELL {
            mgr.observe(&result(node, CheckOutcome::Unreachable, i64::from(i)));
        }
        assert_eq!(mgr.node_liveness(node), Some(NodeState::Unreachable));
        assert!(mgr.down_set().contains(&node));
    }

    /// 🚨 The half that must **not** move: this ADR changes what is displayed, never what is paged.
    /// The same run that leaves the node stateless above still fires exactly once, at the dwell.
    #[test]
    fn withholding_the_seed_does_not_change_when_a_node_pages() {
        let mgr = manager();
        let node = NodeId::new();
        for i in 0..(DEFAULT_LIVENESS_DWELL - 1) {
            assert!(
                mgr.observe(&result(node, CheckOutcome::Unreachable, i64::from(i)))
                    .is_empty(),
                "no notification before the dwell is satisfied"
            );
        }
        assert!(matches!(
            mgr.observe(&result(node, CheckOutcome::Unreachable, 100))
                .as_slice(),
            [NotifyAction::Fire(_)]
        ));
        assert_eq!(mgr.active_alerts().len(), 1);
    }

    /// 🚨 **The point of ADR-097 decision 2.** A device that recovers while core is down used to be
    /// unrecoverable: the engine had forgotten the alert, so the recovery was not a transition, so
    /// no `Resolve` was ever sent and the incident stayed open in the external tool forever.
    /// Measured on the test server — 1,356 stored transitions carrying 18 clears.
    #[test]
    fn a_restored_outage_resolves_when_the_device_comes_back() {
        let mgr = manager();
        let node = NodeId::new();
        assert_eq!(
            mgr.restore(vec![open_alert(node, LIVENESS, NodeState::Unreachable)]),
            1
        );
        assert_eq!(
            mgr.node_state(node),
            Some(NodeState::Unreachable),
            "the fleet reads correctly from the first second, with no poll yet"
        );
        assert!(mgr.down_set().contains(&node), "and suppression knows too");

        for i in 0..(DEFAULT_LIVENESS_DWELL - 1) {
            assert!(
                mgr.observe(&result(node, CheckOutcome::Reachable, i64::from(i)))
                    .is_empty(),
                "recovery still costs a full dwell"
            );
        }
        assert!(matches!(
            mgr.observe(&result(node, CheckOutcome::Reachable, 100))
                .as_slice(),
            [NotifyAction::Resolve(_)]
        ));
        assert!(mgr.active_alerts().is_empty());
        assert!(!mgr.down_set().contains(&node));
    }

    /// The other half, and the one the operator feels every deploy: a device that is *still* broken
    /// must not open a second incident. Before this, each restart wrote another fire — one
    /// continuously-down device had eight `__liveness__` fires and no clear inside 24 hours.
    #[test]
    fn a_restored_outage_does_not_fire_again_while_it_is_still_broken() {
        let mgr = manager();
        let node = NodeId::new();
        mgr.restore(vec![open_alert(node, LIVENESS, NodeState::Unreachable)]);
        for i in 0..=i64::from(DEFAULT_LIVENESS_DWELL) {
            assert!(
                mgr.observe(&result(node, CheckOutcome::Unreachable, i))
                    .is_empty(),
                "poll {i} re-fired an outage that never stopped"
            );
        }
        assert_eq!(mgr.active_alerts().len(), 1, "still one incident, not two");
    }

    /// ADR-087's attribution is not stored — `alert_history` has no `root_cause` column — so it has
    /// to be re-derived on the way back in. Without this, every restart erased the "part of this
    /// node's outage" marker until the node next transitioned, and a node that stays down never
    /// transitions again.
    #[test]
    fn a_restored_alert_on_a_down_node_is_still_part_of_that_nodes_outage() {
        let mgr = manager();
        let node = NodeId::new();
        let other = NodeId::new();
        mgr.restore(vec![
            open_alert(node, "snmp_up", NodeState::Critical),
            open_alert(node, LIVENESS, NodeState::Unreachable),
            // A reachable node's own threshold alert: nothing owns it, so it must stay unattributed.
            open_alert(other, "cpu_util", NodeState::Critical),
        ]);
        let owned = mgr.alerts_for(node);
        let snmp = owned
            .iter()
            .find(|a| a.metric == "snmp_up")
            .expect("the snmp alert was restored");
        assert_eq!(
            snmp.root_cause,
            Some(node),
            "rolled into this node's outage"
        );
        let liveness = owned
            .iter()
            .find(|a| a.metric == LIVENESS)
            .expect("the outage itself was restored");
        assert_eq!(
            liveness.root_cause, None,
            "the outage is the incident; it is not part of another one"
        );
        assert_eq!(
            mgr.alerts_for(other)[0].root_cause,
            None,
            "a node that is not down owns nothing"
        );
    }

    /// Restoring is `or_insert` on every map, which is what makes the call safe to move. A restore
    /// that arrived after a poll had already spoken would otherwise overwrite a real observation
    /// with a stale row — the one way this feature could make things worse rather than better.
    #[test]
    fn restoring_cannot_overwrite_something_the_engine_has_already_observed() {
        let mgr = manager();
        let node = NodeId::new();
        for i in 0..DEFAULT_LIVENESS_DWELL {
            mgr.observe(&result(node, CheckOutcome::Reachable, i64::from(i)));
        }
        mgr.restore(vec![open_alert(node, LIVENESS, NodeState::Unreachable)]);
        assert_eq!(
            mgr.node_liveness(node),
            Some(NodeState::Ok),
            "the live observation wins over the stored one"
        );
        assert!(!mgr.down_set().contains(&node));
    }

    /// The live view has to stay silent too. The node-state SSE stream exists so the WebUI can
    /// patch one row without re-fetching (S14), and before ADR-097 a device whose first poll after a
    /// restart *failed* pushed `"ok"` down it — the engine announcing a state it had never observed.
    #[test]
    fn a_failed_first_poll_broadcasts_nothing() {
        let mgr = manager();
        let node = NodeId::new();
        let mut rx = mgr.subscribe_node_states();

        mgr.observe(&result(node, CheckOutcome::Unreachable, 0));
        assert!(
            rx.try_recv().is_err(),
            "nothing observed ⇒ nothing to announce"
        );

        for i in 1..DEFAULT_LIVENESS_DWELL {
            mgr.observe(&result(node, CheckOutcome::Unreachable, i64::from(i)));
        }
        let (_, ev) = rx.try_recv().expect("the dwell-crossing observe emits");
        assert!(ev.contains("\"unreachable\""), "state unreachable: {ev}");
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

    /// One band rule, walked through every state it can reach (ADR-081).
    ///
    /// The seven unit tests in `yagra-common` pin `ThresholdBounds::evaluate` and the resolution
    /// fold; none of them runs a band through the **engine**, which is where the four bounds have
    /// to survive `AlertConfig` construction, `resolve`, the dwell window and the check id. This
    /// walks the three states an operator actually sees, on one rule and therefore **one check**:
    /// the invariant ADR-081 chose ranges to protect (one rule = one check = one dwell window) is
    /// only worth anything if a single check can change which side it breaches without changing
    /// identity.
    ///
    /// Verified against the live deployment 2026-08-21 on `jpmyj01fw01`'s `icmp_rtt_ms`, by moving
    /// the band rather than the metric — snmpsim replays a fixed recording, so the lab cannot move
    /// an optical level (the ADR-077 decision 1 constraint). Same three transitions, same rule.
    #[test]
    fn a_band_rule_fires_on_each_side_and_clears_between_them() {
        use yagra_bus::Sample;
        use yagra_common::{Direction, ThresholdBounds, ThresholdRule};

        let node = NodeId::new();
        let mgr = manager();
        let mut meta = HashMap::new();
        meta.insert(node, NodeMeta::default());
        // An optical receive level: dark at/below -20 dBm, overdriven at/above -3 dBm.
        mgr.set_config(cfg(
            vec![StoredThreshold::new(
                Uuid::nil(),
                ScopeLevel::Node,
                vec![node.to_string()],
                ThresholdRule::new(
                    "if_rx_power_dbm",
                    ThresholdBounds {
                        warning_below: Some(-18.0),
                        critical_below: Some(-20.0),
                        warning_above: Some(-5.0),
                        critical_above: Some(-3.0),
                    },
                    1,
                ),
            )],
            meta,
        ));

        let observe = |value: f64, at: i64| {
            let mut r = result(node, CheckOutcome::Reachable, at);
            r.samples = vec![Sample::gauge("if_rx_power_dbm", value)];
            mgr.observe(&r)
        };

        // Dark: -25 <= -20 => critical on the LOW side.
        assert!(matches!(
            observe(-25.0, 0).as_slice(),
            [NotifyAction::Fire(_)]
        ));
        let low = mgr.active_alerts();
        assert_eq!(low.len(), 1);
        assert_eq!(low[0].state, NodeState::Critical);
        let check = low[0].check;
        // 🚨 What the operator is TOLD, which is a separate claim from what the engine decided.
        // This rule's primary side is `above` (a band is filed under `above`, for the legacy
        // column), so publishing the primary side would say "0.909 exceeded 5000" — the shape that
        // reached the test deployment on 2026-08-21 and read as nonsense. It must name the side the
        // value crossed.
        assert_eq!(
            low[0].breach.as_ref().map(|b| (b.direction, b.threshold)),
            Some((Direction::Below, Some(-20.0))),
            "a breach on the low side must not be published as the primary side's bound"
        );

        // Healthy light: inside the band => resolves. A one-directional rule could not express
        // "-10 is fine but both -25 and -2 are not" at all, which is why this row exists.
        assert!(matches!(
            observe(-10.0, 1_000).as_slice(),
            [NotifyAction::Resolve(_)]
        ));
        assert!(mgr.active_alerts().is_empty());

        // Still the low side, but only warning: -19 <= -18 and > -20. Both severities on one side.
        assert!(matches!(
            observe(-19.0, 2_000).as_slice(),
            [NotifyAction::Fire(_)]
        ));
        let warn = mgr.active_alerts();
        assert_eq!(warn[0].state, NodeState::Warning);
        assert_eq!(
            warn[0].breach.as_ref().map(|b| (b.direction, b.threshold)),
            Some((Direction::Below, Some(-18.0))),
            "the warning bound reported must be the one on the side that was crossed"
        );
        assert!(matches!(
            observe(-10.0, 3_000).as_slice(),
            [NotifyAction::Resolve(_)]
        ));

        // Overdriven: -2 >= -3 => critical on the HIGH side, out of the SAME rule.
        assert!(matches!(
            observe(-2.0, 4_000).as_slice(),
            [NotifyAction::Fire(_)]
        ));
        let high = mgr.active_alerts();
        assert_eq!(high.len(), 1);
        assert_eq!(high[0].state, NodeState::Critical);
        // The mirror: the same rule, the other side, and the bound reported moves with it. A fix
        // that simply hard-coded the low side would pass every assertion above and fail this one.
        assert_eq!(
            high[0].breach.as_ref().map(|b| (b.direction, b.threshold)),
            Some((Direction::Above, Some(-3.0))),
        );
        // The load-bearing assertion. `check_id` is the external dedup key (ADR-015/075): if the
        // side a band happens to be breaching were part of a check's identity, an incident in
        // PagerDuty would be orphaned every time the value crossed the band instead of updated.
        assert_eq!(
            high[0].check, check,
            "both sides of one rule must be one check"
        );
    }

    /// The node-wide fold must keep the sample that is **breaching**, not the largest (ADR-081).
    ///
    /// A metric with several table rows per node (chassis temperature sensors, stack power
    /// supplies) is collapsed to one sample per poll before the state machine sees it, because the
    /// rows' identities were lost at collection time. Before ranges that fold asked one question —
    /// "which value is furthest in the rule's single direction" — and a band has no single
    /// direction to be furthest in.
    ///
    /// The numbers are measured, not invented: `jpmyj01fw01` reports 15 `huawei_temp` rows,
    /// **one at 59 and fourteen at 0** (2026-08-21). A rule reading "at/below 40 is critical" is
    /// therefore decided entirely by whether the fold can look past the 59 — and a fold that kept
    /// the maximum would report `Ok` and never fire, silently, with the rule visible on the screen.
    #[test]
    fn the_node_wide_fold_keeps_a_breaching_sample_over_a_higher_one_inside_the_band() {
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
                ThresholdRule::new(
                    "huawei_temp",
                    // The upper bound is out of reach: only the low side can decide this.
                    ThresholdBounds {
                        warning_below: Some(45.0),
                        critical_below: Some(40.0),
                        warning_above: Some(90_000.0),
                        critical_above: Some(100_000.0),
                    },
                    1,
                ),
            )],
            meta,
        ));

        let mut r = result(node, CheckOutcome::Reachable, 0);
        // The order is deliberate: the in-band 59 arrives FIRST and so becomes the incumbent. A
        // fold that only ever replaces the incumbent with a larger value keeps it and reports Ok.
        r.samples = vec![
            Sample::gauge("huawei_temp", 59.0),
            Sample::gauge("huawei_temp", 0.0),
            Sample::gauge("huawei_temp", 0.0),
        ];
        assert!(
            matches!(mgr.observe(&r).as_slice(), [NotifyAction::Fire(_)]),
            "a sensor at 0 breaches `critical_below: 40` and must not be hidden by one at 59"
        );
        let alerts = mgr.active_alerts();
        assert_eq!(alerts.len(), 1, "many rows, one check, one alert (ADR-076)");
        assert_eq!(alerts[0].state, NodeState::Critical);
        assert_eq!(
            alerts[0].breach.as_ref().map(|b| b.value),
            Some(0.0),
            "the operator must be shown the row that actually breached"
        );
    }

    /// The mirror image: a low sample sitting inside the band must not hide a high breach.
    ///
    /// Written because the fix for the case above is a severity comparison, and a severity
    /// comparison written the other way round — "keep the smallest" — would pass that test and
    /// fail this one. Neither direction may win by default.
    #[test]
    fn the_node_wide_fold_keeps_a_high_breach_over_a_lower_sample_inside_the_band() {
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
                ThresholdRule::new(
                    "huawei_temp",
                    // Now the LOW bound is out of reach and only the high side can decide.
                    ThresholdBounds {
                        warning_below: Some(-100.0),
                        critical_below: Some(-200.0),
                        warning_above: Some(45.0),
                        critical_above: Some(50.0),
                    },
                    1,
                ),
            )],
            meta,
        ));

        let mut r = result(node, CheckOutcome::Reachable, 0);
        // In-band incumbent first again, this time below the breach rather than above it.
        r.samples = vec![
            Sample::gauge("huawei_temp", 0.0),
            Sample::gauge("huawei_temp", 0.0),
            Sample::gauge("huawei_temp", 59.0),
        ];
        assert!(matches!(
            mgr.observe(&r).as_slice(),
            [NotifyAction::Fire(_)]
        ));
        let alerts = mgr.active_alerts();
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].state, NodeState::Critical);
        assert_eq!(
            alerts[0].breach.as_ref().map(|b| b.value),
            Some(59.0),
            "the operator must be shown the row that actually breached"
        );
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

    /// The ADR-077 regression, and the mirror of
    /// `one_breaching_port_among_many_fires_exactly_one_alert`.
    ///
    /// Before ADR-077, one hot CPU among fourteen idle ones had its dwell candidate reset by the
    /// very next sample in the same poll, so a 3-sample rule **never fired at all** —
    /// `huawei_cpu_usage` arrives 15 times per poll and `juniper_cpu_1min` 53 times. ADR-077 folded
    /// the rows into one check; ADR-143 gives each row its own, so the hot row's dwell is its own and
    /// the idle rows cannot reach it either way.
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

        // Exactly one alert — one for the hot row, none for the fourteen idle ones.
        assert_eq!(fired.len(), 1, "one hot row must raise exactly one alert");
        let NotifyAction::Fire(alert) = &fired[0] else {
            panic!("expected a fire, got {:?}", fired[0]);
        };
        assert_eq!(alert.metric, "huawei_cpu_usage");
        assert_eq!(alert.severity, Severity::Critical);
        // The check is the row's own (ADR-143), so the incident says which CPU is hot. A row is not
        // a port, so `ifindex` stays empty and the row travels in `row`.
        assert_eq!(alert.check, row_check_id(node, 4, "huawei_cpu_usage"));
        assert_eq!(alert.row, Some(4));
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
    /// Before ADR-077 a 3-sample rule on a metric with three or more rows fired on the first poll,
    /// which is the dwell silently becoming "three rows" instead of "three polls" — the opposite
    /// failure to the inert one, and just as wrong. Since ADR-143 every row has its own window, so
    /// twelve breaching rows are twelve incidents — each one only after three polls.
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
            "twelve breaching rows in one poll are one step of twelve windows, not a whole dwell"
        );
        assert!(mgr.observe(&poll(1_000)).is_empty());
        let fired = mgr.observe(&poll(2_000));
        assert_eq!(
            fired.len(),
            12,
            "the third poll completes every row's dwell"
        );
        let checks: std::collections::HashSet<_> = fired
            .iter()
            .map(|a| match a {
                NotifyAction::Fire(alert) => alert.check,
                other => panic!("expected a fire, got {other:?}"),
            })
            .collect();
        assert_eq!(checks.len(), 12, "one check per row, never a shared one");
    }

    /// A `below` rule must alert on the **low** row, and this is the test that catches reading the
    /// table through `max` — which is what the node-level chart does.
    ///
    /// `query_metrics` collapses an entity metric to its maximum and its own response says the
    /// consequence out loud: where low is the fault, the maximum is the *healthiest* series. A UPS
    /// with one string at 15% and two at 80/90% is in trouble; read with `max` it reports 90 and
    /// alerts on nothing. ADR-077 folded to the worst row; since ADR-143 the depleted row is judged
    /// on its own check, which answers the same way.
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

    /// A metric the catalogue does not call per-interface is never read as a port, even when its
    /// samples carry an `ifindex` — the label is a row key, not a port number (ADR-011). Since
    /// ADR-143 each row is its own check, but it is a **row** check: `ifindex` stays empty.
    #[test]
    fn a_row_key_that_is_not_a_port_is_a_row_not_a_port() {
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

        // Two rows breaching in one poll are one step of each row's own window, so a two-sample
        // dwell still means two polls (ADR-077's property, kept by ADR-143 without the fold).
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
            "one poll is one dwell step, however many rows breach in it"
        );
        let actions = mgr.observe(&poll(1_000));
        assert_eq!(actions.len(), 2, "each chassis row is its own check");
        for (action, row) in actions.iter().zip([17_u32, 18]) {
            let NotifyAction::Fire(alert) = action else {
                panic!("expected a fire, got {action:?}");
            };
            assert_eq!(alert.ifindex, None, "a chassis reading names no port");
            assert_eq!(alert.row, Some(row));
            assert_eq!(alert.check, row_check_id(node, row, "cisco_env_temp"));
        }
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

    /// 🚨 **The paged reader must answer exactly what the fleet-wide one does** (ADR-125).
    /// `node_states_for` is a second implementation of `node_states`' rollup, written to avoid
    /// cloning the whole fleet on the hottest read in the product — and a second implementation of
    /// a rule is a mirror, so it needs the test that fails when the two drift
    /// (`extensibility.md` §2). Nothing else would notice: both return a plausible map.
    #[test]
    fn the_paged_states_agree_with_the_fleet_wide_ones() {
        use yagra_bus::Sample;
        use yagra_common::{ThresholdBounds, ThresholdRule};

        let mgr = manager();
        let up = NodeId::new();
        let down = NodeId::new();
        let breaching = NodeId::new();
        let never_observed = NodeId::new();

        for i in 0..DEFAULT_LIVENESS_DWELL {
            mgr.observe(&result(up, CheckOutcome::Reachable, i64::from(i)));
            mgr.observe(&result(down, CheckOutcome::Unreachable, i64::from(i)));
            mgr.observe(&result(breaching, CheckOutcome::Reachable, i64::from(i)));
        }
        // One node reachable but breaching a threshold, so the alert rollup — the half that reads
        // `active` rather than `live` — is exercised on both sides rather than only liveness.
        let mut meta = HashMap::new();
        meta.insert(breaching, NodeMeta::default());
        mgr.set_config(cfg(
            vec![StoredThreshold::new(
                Uuid::nil(),
                ScopeLevel::Node,
                vec![breaching.to_string()],
                ThresholdRule::new(
                    "icmp_rtt_ms",
                    ThresholdBounds::above(Some(50.0), Some(100.0)),
                    1,
                ),
            )],
            meta,
        ));
        let mut high = result(breaching, CheckOutcome::Reachable, 100);
        high.samples = vec![Sample::gauge("icmp_rtt_ms", 150.0)];
        let _ = mgr.observe(&high);

        let fleet = mgr.node_states();
        let page = [up, down, breaching, never_observed];
        let paged = mgr.node_states_for(&page);

        for n in page {
            assert_eq!(
                paged.get(&n).copied(),
                fleet.get(&n).copied(),
                "paged and fleet-wide answers differ for {n}"
            );
        }
        // The rollup actually did something — without this the loop above is satisfied by two
        // implementations that both return nothing.
        assert_eq!(paged.get(&up).copied(), Some(NodeState::Ok));
        assert_eq!(paged.get(&down).copied(), Some(NodeState::Unreachable));
        assert_eq!(paged.get(&breaching).copied(), Some(NodeState::Critical));
        // A node the engine has never observed stays absent, so the caller's fallback still runs.
        assert!(!paged.contains_key(&never_observed));
        // And the page is a page: nothing outside it comes back, however much the fleet holds.
        assert_eq!(paged.len(), 3);
        assert!(fleet.len() >= paged.len());
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

    // ── ADR-087: a node's outage owns that node's other alerts ──────────────────────────────────
    //
    // The defect these cover, measured on the running deployment (2026-08-22): a device falling
    // over raised **two** critical alerts — `__liveness__` and `snmp_up` — and `dedup_string`
    // carries the check id, so PagerDuty/JSM opened two incidents for one outage. Thirteen nodes
    // were in that state. `repo.rs`'s built-in rule table has always called two criticals for one
    // outage a notification flood and said this project treats that as a bug.

    /// A `snmp_up` rule shaped like the seeded one (`below 0.5`), with the dwell the caller wants.
    fn snmp_up_rule(dwell: u32) -> StoredThreshold {
        StoredThreshold::new(
            Uuid::new_v4(),
            ScopeLevel::Global,
            vec!["global".to_string()],
            yagra_common::ThresholdRule::new(
                yagra_common::METRIC_SNMP_UP,
                ThresholdBounds::from_legacy(yagra_common::Direction::Below, None, Some(0.5)),
                dwell,
            ),
        )
    }

    /// One poll result carrying `snmp_up = 0` — what the poller sends on the SNMP error path, which
    /// is the case that matters: `worker/snmp.rs` emits the sample even when the GET could not be issued.
    fn snmp_down(node: NodeId, outcome: CheckOutcome, at: i64) -> PollResult {
        let mut r = result(node, outcome, at);
        r.samples = vec![yagra_bus::Sample::gauge(yagra_common::METRIC_SNMP_UP, 0.0)];
        r
    }

    /// **The receiving side first**: a node already down rolls its other alerts into its own
    /// outage, so only one incident is opened.
    ///
    /// Written before the ordering test below on purpose — a suite that only demonstrates
    /// suppression would pass against an engine that suppressed everything
    /// (`rejection-only-tests-pass-when-everything-rejects`).
    #[test]
    fn an_alert_on_a_node_that_is_already_down_rolls_into_that_nodes_outage() {
        let node = NodeId::new();
        let mgr = AlertManager::new();
        mgr.set_config(cfg(vec![liveness_rule(), snmp_up_rule(1)], meta_for(node)));

        // Commit the outage first: liveness needs its full dwell.
        for i in 0..i64::from(DEFAULT_LIVENESS_DWELL) {
            mgr.observe(&result(node, CheckOutcome::Unreachable, i));
        }
        assert!(mgr.down_set().contains(&node), "the outage is committed");
        let liveness = mgr.active_alerts();
        assert_eq!(liveness.len(), 1);
        assert_eq!(
            liveness[0].root_cause, None,
            "the outage itself pages — it is the incident, not part of one"
        );

        // Now SNMP reports the agent gone. It is part of that outage, not a second one.
        let actions = mgr.observe(&snmp_down(node, CheckOutcome::Unreachable, 100));
        let fired = actions
            .iter()
            .find_map(|a| match a {
                NotifyAction::Fire(al) if al.metric == yagra_common::METRIC_SNMP_UP => Some(al),
                _ => None,
            })
            .expect("snmp_up breaches its rule");
        assert_eq!(
            fired.root_cause,
            Some(node),
            "attributed to the node whose outage it is — `Notifier` keys its skip on this being \
             Some, so the second incident is never opened"
        );
        // Both are still visible: only the page is rolled up, never the signal.
        assert_eq!(mgr.active_alerts().len(), 2);
    }

    /// The ordering case, which is the common one: `snmp_up` commits **before** liveness does, so
    /// it pages standalone and must then be closed when the outage commits.
    ///
    /// Measured across the 13 doubled nodes: `snmp_up` won 7 times, liveness 4, and they tied
    /// twice — the two checks run on different intervals, so neither order is the rule. This is
    /// the same "page once, then close" a child alert that beats its parent's dwell already gets
    /// (`child_down_before_parent_rolls_up_when_parent_falls`), and it is why attributing at fire
    /// time alone is not enough.
    #[test]
    fn an_alert_that_beat_the_outage_is_closed_when_the_outage_commits() {
        let node = NodeId::new();
        let mgr = AlertManager::new();
        mgr.set_config(cfg(vec![liveness_rule(), snmp_up_rule(1)], meta_for(node)));

        // Sample 1: SNMP is gone but the outage has not committed (dwell 3), so this pages on its
        // own — correctly, at that moment nothing says the device is down.
        let first = mgr.observe(&snmp_down(node, CheckOutcome::Unreachable, 0));
        let paged = first
            .iter()
            .find_map(|a| match a {
                NotifyAction::Fire(al) if al.metric == yagra_common::METRIC_SNMP_UP => Some(al),
                _ => None,
            })
            .expect("snmp_up fires at dwell 1");
        assert_eq!(paged.root_cause, None, "nothing to roll it up into yet");

        // Samples 2-3: the outage commits, and the re-sweep must reconsider the alert that beat it.
        let mut suppressed = None;
        let mut liveness_fired = false;
        for i in 1..i64::from(DEFAULT_LIVENESS_DWELL) {
            for action in mgr.observe(&snmp_down(node, CheckOutcome::Unreachable, i)) {
                match action {
                    NotifyAction::Suppress(al) if al.metric == yagra_common::METRIC_SNMP_UP => {
                        suppressed = Some(al);
                    }
                    NotifyAction::Fire(al) if al.metric == LIVENESS => liveness_fired = true,
                    _ => {}
                }
            }
        }
        assert!(liveness_fired, "the outage itself pages");
        let suppressed = suppressed.expect(
            "the snmp_up alert that had been paging standalone must be closed, or on-call is left \
             with a second open incident for one outage",
        );
        assert_eq!(suppressed.root_cause, Some(node));
    }

    /// Recovery is the direction that must not go quiet: a device that pings again while its SNMP
    /// agent is still dead is exactly the case the seeded `snmp_up` rule was written for — its own
    /// comment says "the SNMP agent stopped answering **while the device itself is fine**".
    #[test]
    fn when_the_node_comes_back_a_still_broken_check_pages_on_its_own() {
        let node = NodeId::new();
        let mgr = AlertManager::new();
        mgr.set_config(cfg(vec![liveness_rule(), snmp_up_rule(1)], meta_for(node)));

        for i in 0..i64::from(DEFAULT_LIVENESS_DWELL) {
            mgr.observe(&snmp_down(node, CheckOutcome::Unreachable, i));
        }
        assert!(mgr.down_set().contains(&node));
        assert!(mgr
            .active_alerts()
            .iter()
            .any(|a| a.metric == yagra_common::METRIC_SNMP_UP && a.root_cause == Some(node)));

        // ICMP answers again; SNMP still does not.
        let mut fired = None;
        for i in 0..i64::from(DEFAULT_LIVENESS_DWELL) {
            for action in mgr.observe(&snmp_down(node, CheckOutcome::Reachable, 100 + i)) {
                if let NotifyAction::Fire(al) = action {
                    if al.metric == yagra_common::METRIC_SNMP_UP {
                        fired = Some(al);
                    }
                }
            }
        }
        assert!(!mgr.down_set().contains(&node), "the outage is over");
        let fired = fired.expect(
            "with the outage gone the SNMP failure is its own incident again — staying silent here \
             would mean an agent-only outage never pages",
        );
        assert_eq!(fired.root_cause, None);
    }

    /// A neighbour's alert is not touched when this node flips, and a pool-coverage alert is not
    /// touched at all.
    ///
    /// The scoping half: the re-sweep reconsiders `changed`'s **own** non-liveness alerts, not
    /// every open alert in the fleet. Without this the check would be "does anything change",
    /// which a sweep over the whole active set would also satisfy — while costing O(active) on
    /// every flip, the exact regression S3 removed.
    #[test]
    fn a_flip_does_not_reattribute_another_nodes_alert_or_a_pool_alert() {
        use yagra_bus::Sample;

        let down_node = NodeId::new();
        let other = NodeId::new();
        let mgr = AlertManager::new();
        let mut meta = meta_for(down_node);
        meta.extend(meta_for(other));
        mgr.set_config(cfg(
            vec![
                liveness_rule(),
                snmp_up_rule(1),
                StoredThreshold::new(
                    Uuid::new_v4(),
                    ScopeLevel::Global,
                    vec!["global".to_string()],
                    yagra_common::ThresholdRule::new(
                        "icmp_rtt_ms",
                        ThresholdBounds::above(None, Some(100.0)),
                        1,
                    ),
                ),
            ],
            meta,
        ));

        // `other` is reachable but slow: its own alert, nothing to do with `down_node`.
        let mut slow = result(other, CheckOutcome::Reachable, 0);
        slow.samples = vec![Sample::gauge("icmp_rtt_ms", 150.0)];
        mgr.observe(&slow);
        // …and a pool alert, which has no node at all.
        assert!(mgr.raise_pool_coverage_alert("tokyo", 1_000).is_some());

        for i in 0..i64::from(DEFAULT_LIVENESS_DWELL) {
            mgr.observe(&snmp_down(down_node, CheckOutcome::Unreachable, i));
        }
        assert!(mgr.down_set().contains(&down_node));

        for alert in mgr.active_alerts() {
            match alert.node() {
                Some(n) if n == down_node => {
                    if alert.metric != LIVENESS {
                        assert_eq!(alert.root_cause, Some(down_node));
                    }
                }
                Some(_) => assert_eq!(
                    alert.root_cause, None,
                    "another node's alert must be untouched by this node's outage"
                ),
                None => assert_eq!(
                    alert.root_cause, None,
                    "a pool alert has no node, so it can belong to no node's outage"
                ),
            }
        }
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

    /// A manager whose every node polls every `secs` seconds — or one nothing was published into.
    fn manager_polling_every(
        secs: Option<u32>,
    ) -> (AlertManager, crate::poll_interval::PollIntervals) {
        let intervals = crate::poll_interval::PollIntervals::unknown();
        if let Some(secs) = secs {
            intervals.publish(crate::poll_interval::IntervalSnapshot::build(secs, []));
        }
        (
            AlertManager::with_poll_intervals(intervals.clone()),
            intervals,
        )
    }

    /// A three-breach `if_in_util_pct` rule on port 7 of `node`.
    fn three_breach_port_rule(node: NodeId) -> StoredThreshold {
        StoredThreshold::new(
            Uuid::new_v4(),
            ScopeLevel::Interface,
            vec![format!("{}:7", node.as_uuid())],
            yagra_common::ThresholdRule::new(
                "if_in_util_pct",
                ThresholdBounds::above(None, Some(90.0)),
                3,
            ),
        )
    }

    /// ADR-144 decision 5. The utilisation evaluator ticks every minute whatever the node's poll
    /// interval, so on a node polled every five minutes one reading arrives five times. A
    /// three-breach rule has to see three polls — fifteen ticks — not one poll read three times.
    #[test]
    fn a_tick_counted_dwell_spans_the_polls_on_a_slow_node() {
        let fires_on_tick = |interval: Option<u32>| {
            let node = NodeId::from(Uuid::new_v4());
            let (mgr, _) = manager_polling_every(interval);
            mgr.set_config(cfg(vec![three_breach_port_rule(node)], HashMap::new()));
            (1..=30_i64).find(|tick| {
                mgr.observe_interface_metric(
                    node,
                    IfIndex(7),
                    "if_in_util_pct",
                    95.0,
                    tick * 60_000,
                )
                .expect("a rule is in force")
                .iter()
                .any(|a| matches!(a, NotifyAction::Fire(_)))
            })
        };
        assert_eq!(
            fires_on_tick(None),
            Some(3),
            "nothing published: three ticks, as before"
        );
        assert_eq!(
            fires_on_tick(Some(30)),
            Some(3),
            "faster than the tick: unchanged"
        );
        assert_eq!(
            fires_on_tick(Some(300)),
            Some(15),
            "three five-minute polls"
        );
    }

    /// The derived node metrics tick the same way and convert the same way — through the per-row
    /// path when the metric is a table.
    #[test]
    fn a_derived_metrics_dwell_spans_the_polls_on_a_slow_node() {
        use yagra_common::ThresholdRule;
        let node = NodeId::new();
        let (mgr, _) = manager_polling_every(Some(300));
        mgr.set_config(cfg(
            vec![StoredThreshold::new(
                Uuid::nil(),
                ScopeLevel::Node,
                vec![node.to_string()],
                ThresholdRule::new(
                    "huawei_mem_used_pct",
                    ThresholdBounds::above(Some(1.0), Some(99.0)),
                    2,
                ),
            )],
            meta_for(node),
        ));
        let _ = mgr.observe(&result(node, CheckOutcome::Reachable, 0));
        let fired_on = (1..=20_i64).find(|tick| {
            mgr.observe_derived_metric(node, "huawei_mem_used_pct", &[(0, 80.0)], tick * 60_000)
                .expect("a rule is in force")
                .iter()
                .any(|a| matches!(a, NotifyAction::Fire(_)))
        });
        assert_eq!(fired_on, Some(10), "two five-minute polls are ten ticks");
    }

    /// The poll path already sees each poll once, so a published interval must not stretch it.
    #[test]
    fn a_poll_result_counts_polls_whatever_the_interval() {
        let node = NodeId::new();
        let (mgr, _) = manager_polling_every(Some(300));
        mgr.set_config(cfg(Vec::new(), HashMap::new()));
        let dwell = i64::from(liveness_rule().rule.dwell_samples);
        let fired_on = (1..=20_i64).find(|poll| {
            mgr.observe(&result(node, CheckOutcome::Unreachable, poll * 300_000))
                .iter()
                .any(|a| matches!(a, NotifyAction::Fire(_)))
        });
        assert_eq!(fired_on, Some(dwell));
    }

    /// Publishing a slower interval while a port is already breaching keeps the run it has made:
    /// the count grows, it does not start over (the `set_dwell` contract, ADR-075).
    #[test]
    fn raising_the_interval_mid_breach_extends_the_count_without_restarting_it() {
        let node = NodeId::from(Uuid::new_v4());
        let (mgr, intervals) = manager_polling_every(Some(30));
        mgr.set_config(cfg(vec![three_breach_port_rule(node)], HashMap::new()));
        let breach = |tick: i64| {
            mgr.observe_interface_metric(node, IfIndex(7), "if_in_util_pct", 95.0, tick * 60_000)
                .expect("a rule is in force")
                .iter()
                .any(|a| matches!(a, NotifyAction::Fire(_)))
        };
        assert!(!breach(1));
        assert!(!breach(2));
        intervals.publish(crate::poll_interval::IntervalSnapshot::build(300, []));
        assert_eq!((3..=30_i64).find(|tick| breach(*tick)), Some(15));
    }

    /// ADR-144 decision 6. A port that changes state once every five-minute poll is flapping, and a
    /// fixed ten-minute window can never say so: five transitions take twenty minutes. The same
    /// readings with nothing published keep today's answer, which is "not flapping".
    #[test]
    fn flapping_is_detected_on_a_slowly_polled_node() {
        let flapping_on_each_fire = |interval: Option<u32>| {
            let node = NodeId::from(Uuid::new_v4());
            let (mgr, _) = manager_polling_every(interval);
            mgr.set_config(cfg(vec![port_rule(node, IfIndex(7), 50.0)], HashMap::new()));
            let mut fires = Vec::new();
            for tick in 1..=25_i64 {
                // Five minutes over the bound, five minutes under it: one change per poll.
                let value = if (tick - 1) / 5 % 2 == 0 { 95.0 } else { 10.0 };
                for action in mgr
                    .observe_interface_metric(
                        node,
                        IfIndex(7),
                        "if_in_util_pct",
                        value,
                        tick * 60_000,
                    )
                    .expect("a rule is in force")
                {
                    if let NotifyAction::Fire(alert) = action {
                        fires.push(alert.flapping);
                    }
                }
            }
            fires
        };
        assert_eq!(
            flapping_on_each_fire(Some(300)),
            vec![false, false, true],
            "the fifth transition lands inside a twenty-poll window"
        );
        assert_eq!(
            flapping_on_each_fire(None),
            vec![false, false, false],
            "nothing published: the ten-minute window, as before"
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

    /// Deleting a **node-level derived** rule must close its alert, for the same reason its port
    /// sibling below does — nothing polls `huawei_mem_used_pct` either.
    ///
    /// Written from the deployment. The verification rule was deleted and its warning was still
    /// open three minutes later, because `observe`'s `!alerting` branch only visits checks a poll
    /// result carries and a derived metric is in no poll result. The port dimension had this sweep
    /// since ADR-076 増分 7; the node dimension shipped without it.
    #[test]
    fn the_orphan_sweep_closes_a_node_derived_alert_whose_rule_was_deleted() {
        use yagra_common::{ThresholdBounds, ThresholdRule};
        let node = NodeId::new();
        let rule = |warning: f64| {
            StoredThreshold::new(
                Uuid::nil(),
                ScopeLevel::Node,
                vec![node.to_string()],
                ThresholdRule::new(
                    "huawei_mem_used_pct",
                    ThresholdBounds::above(Some(warning), Some(99.0)),
                    1,
                ),
            )
        };
        let mgr = manager();
        mgr.set_config(cfg(vec![rule(1.0)], meta_for(node)));
        let _ = mgr.observe(&result(node, CheckOutcome::Reachable, 0));
        let acts = mgr
            .observe_derived_metric(node, "huawei_mem_used_pct", &[(0, 80.44)], 1)
            .expect("a rule is in force");
        assert!(acts.iter().any(|a| matches!(a, NotifyAction::Fire(_))));

        // The accepting half, and it is load-bearing: a sweep that resolved everything would pass
        // the rest of this test.
        assert!(
            mgr.resolve_orphaned_node_derived_alerts().is_empty(),
            "a rule that still exists is not an orphan"
        );
        // …and it must leave the other dimension alone, or one alert gets two closers racing.
        assert!(
            mgr.resolve_orphaned_interface_alerts().is_empty(),
            "a node-wide check is not the port sweep's to close"
        );

        mgr.set_config(cfg(Vec::new(), meta_for(node)));
        let swept = mgr.resolve_orphaned_node_derived_alerts();
        assert_eq!(swept.len(), 1);
        assert!(matches!(swept[0], NotifyAction::Resolve(_)));
        assert!(mgr.active_alerts().is_empty());
        assert!(
            mgr.resolve_orphaned_node_derived_alerts().is_empty(),
            "the sweep runs every 60s for the life of the process; it must be idempotent"
        );

        // 🚨 And the metric must be able to alert again — the trap the port version documents.
        // Idempotence alone is also satisfied by a check that can no longer do anything.
        mgr.set_config(cfg(vec![rule(1.0)], meta_for(node)));
        let acts = mgr
            .observe_derived_metric(node, "huawei_mem_used_pct", &[(0, 80.44)], 3)
            .expect("the recreated rule is in force");
        assert!(
            acts.iter().any(|a| matches!(a, NotifyAction::Fire(_))),
            "a metric whose rule was deleted and recreated must alert again, got {acts:?}"
        );
    }

    /// A *collected* node metric belongs to [`AlertManager::resolve_orphaned_collected_alerts`],
    /// not to the derived sweep — and somebody has to take it.
    ///
    /// 🚨 **This test used to assert that the poll path would close it, and the poll path never
    /// could** (ADR-097 Increment 6). Every assertion in it was a refusal, and it never re-polled,
    /// so it could not see that no other closer existed: `observe` `continue`s a sample whose
    /// threshold does not resolve before `process_check` is reached. The positive half below is
    /// what makes the refusal above mean something.
    #[test]
    fn the_node_derived_sweep_leaves_collected_metrics_to_the_collected_sweep() {
        use yagra_bus::Sample;
        use yagra_common::{ThresholdBounds, ThresholdRule};
        let node = NodeId::new();
        let mgr = manager();
        mgr.set_config(cfg(
            vec![StoredThreshold::new(
                Uuid::nil(),
                ScopeLevel::Node,
                vec![node.to_string()],
                // Collected, not computed — the vendor's own percentage.
                ThresholdRule::new(
                    "huawei_mem_usage",
                    ThresholdBounds::above(Some(1.0), Some(99.0)),
                    1,
                ),
            )],
            meta_for(node),
        ));
        let mut r = result(node, CheckOutcome::Reachable, 0);
        r.samples = vec![Sample::gauge("huawei_mem_usage", 80.0)];
        let acts = mgr.observe(&r);
        assert!(acts.iter().any(|a| matches!(a, NotifyAction::Fire(_))));

        mgr.set_config(cfg(Vec::new(), meta_for(node)));
        assert!(
            mgr.resolve_orphaned_node_derived_alerts().is_empty(),
            "a collected metric is not the derived sweep's; two closers would race"
        );
        assert_eq!(
            mgr.active_alerts().len(),
            1,
            "…and it is still open until its own sweep runs"
        );

        let acts = mgr.resolve_orphaned_collected_alerts();
        assert_eq!(
            acts.len(),
            1,
            "the collected sweep is the one that closes it, got {acts:?}"
        );
        assert!(matches!(acts[0], NotifyAction::Resolve(_)));
        assert!(mgr.active_alerts().is_empty());
        assert!(
            mgr.resolve_orphaned_collected_alerts().is_empty(),
            "the sweep runs every tick for the life of the process; it must be idempotent"
        );

        // 🚨 And the check must still be able to fire. Closing an alert without dropping its dwell
        // window leaves the state machine committed, so a recreated rule sees no transition.
        mgr.set_config(cfg(
            vec![StoredThreshold::new(
                Uuid::nil(),
                ScopeLevel::Node,
                vec![node.to_string()],
                ThresholdRule::new(
                    "huawei_mem_usage",
                    ThresholdBounds::above(Some(1.0), Some(99.0)),
                    1,
                ),
            )],
            meta_for(node),
        ));
        let acts = mgr.observe(&r);
        assert!(
            acts.iter().any(|a| matches!(a, NotifyAction::Fire(_))),
            "a collected metric whose rule was deleted and recreated must alert again, got {acts:?}"
        );
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

    /// A *collected* per-port metric belongs to
    /// [`AlertManager::resolve_orphaned_collected_alerts`] too — the split between the sweeps is
    /// derived-vs-collected, not node-vs-port, because a rule lookup answers a per-port question as
    /// readily as a node-wide one.
    ///
    /// 🚨 Same correction as its node-wide sibling: this used to assert the poll path would close
    /// it, and nothing did.
    #[test]
    fn the_interface_sweep_leaves_collected_port_alerts_to_the_collected_sweep() {
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
            "a collected port metric is not the derived sweep's; two closers would race"
        );
        assert_eq!(mgr.active_alerts().len(), 1);

        let acts = mgr.resolve_orphaned_collected_alerts();
        assert_eq!(
            acts.len(),
            1,
            "the collected sweep closes the port dimension too, got {acts:?}"
        );
        assert!(mgr.active_alerts().is_empty());
    }

    // ---- ADR-097 Increment 6: the collected sweep's refusals ----

    /// 🚨 **The accepting half, and it is load-bearing.** A sweep that closed everything would
    /// satisfy every other assertion about this function; only this one can tell them apart.
    #[test]
    fn the_collected_sweep_leaves_an_alert_whose_rule_still_resolves() {
        let node = NodeId::new();
        let mgr = manager();
        mgr.set_config(cfg(vec![snmp_up_rule(1)], meta_for(node)));
        let _ = mgr.observe(&snmp_down(node, CheckOutcome::Reachable, 0));
        assert_eq!(
            mgr.active_alerts().len(),
            1,
            "the alert is open to begin with"
        );

        assert!(
            mgr.resolve_orphaned_collected_alerts().is_empty(),
            "the rule is still there; nothing is orphaned"
        );
        assert_eq!(mgr.active_alerts().len(), 1);
    }

    /// Liveness is `process_check`'s — the one check that actually reaches the `!alerting` branch.
    #[test]
    fn the_collected_sweep_never_closes_a_liveness_alert() {
        let node = NodeId::new();
        let mgr = manager();
        // No rules at all, so `resolve` answers `None` for everything. The liveness alert must
        // still survive this sweep.
        mgr.set_config(cfg(Vec::new(), meta_for(node)));
        mgr.restore(vec![open_alert(node, LIVENESS, NodeState::Unreachable)]);

        assert!(
            mgr.resolve_orphaned_collected_alerts().is_empty(),
            "the liveness alert is not this sweep's to close"
        );
        assert_eq!(mgr.active_alerts().len(), 1);
    }

    /// 🚨 A passive-event alert has no threshold rule and no series, so both of ADR-097 Increment 6's
    /// sweeps would take it. Closing one from outside `events::engine` leaves its `runtime.active`
    /// entry behind, which suppresses the rule's re-fire **permanently** — worse than a wrong close.
    #[test]
    fn the_collected_sweep_never_closes_an_event_alert() {
        let node = NodeId::new();
        let mgr = manager();
        mgr.set_config(cfg(Vec::new(), meta_for(node)));
        let metric = format!("{}link flap", crate::events::EVENT_METRIC_PREFIX);
        mgr.restore(vec![open_alert(node, &metric, NodeState::Critical)]);

        assert!(
            mgr.resolve_orphaned_collected_alerts().is_empty(),
            "an event alert belongs to events::engine, whole lifecycle"
        );
        assert_eq!(mgr.active_alerts().len(), 1);
    }

    /// The prefix exclusion is only sound if no metric a poller can emit collides with it.
    #[test]
    fn no_catalogue_metric_name_starts_with_the_event_prefix() {
        let prefix = crate::events::EVENT_METRIC_PREFIX;
        let mut checked = 0usize;
        for name in crate::metric_meaning::CHECK_METRICS {
            assert!(!name.starts_with(prefix), "{name} collides with `{prefix}`");
            checked += 1;
        }
        for d in crate::derived::DERIVED_NODE_METRICS {
            assert!(
                !d.name.starts_with(prefix),
                "{} collides with `{prefix}`",
                d.name
            );
            checked += 1;
        }
        for name in crate::interface_util::DERIVED_INTERFACE_METRICS {
            assert!(!name.starts_with(prefix), "{name} collides with `{prefix}`");
            checked += 1;
        }
        for (item, _) in crate::mib::builtin_mib_rows() {
            assert!(
                !item.metric_name.starts_with(prefix),
                "{} collides with `{prefix}`",
                item.metric_name
            );
            checked += 1;
        }
        // A floor, because everything above asks whether something is *absent*: over an empty
        // iteration that is a claim about nothing.
        assert!(
            checked >= 100,
            "only {checked} metric names were inspected; the catalogues did not load"
        );
    }

    /// A pool-coverage alert is `pool_coverage`'s, and it has no node to resolve a rule against.
    #[test]
    fn the_collected_sweep_never_closes_a_pool_subject_alert() {
        let mgr = manager();
        mgr.set_config(cfg(Vec::new(), meta_for(NodeId::new())));
        mgr.raise_pool_coverage_alert("default", 1_000);
        assert_eq!(mgr.active_alerts().len(), 1);

        assert!(
            mgr.resolve_orphaned_collected_alerts().is_empty(),
            "a pool subject has no inventory row and no threshold rule"
        );
        assert_eq!(mgr.active_alerts().len(), 1);
    }

    /// 🚨 The disaster case, the same one `an_empty_node_map_resolves_nothing` guards for the
    /// deleted-node sweep: `AlertConfig::default` is what the manager holds before the first
    /// refresh, and `restore` has already seeded the active set by then.
    ///
    /// ⚠️ Written with `snmp_up` rather than `__liveness__` on purpose — the liveness alert is
    /// refused by this sweep for a second, unrelated reason, so it would pass whether the guard
    /// existed or not.
    #[test]
    fn an_engine_with_no_config_installed_sweeps_no_collected_alert() {
        let node = NodeId::new();
        let mgr = AlertManager::new();
        mgr.restore(vec![open_alert(node, "snmp_up", NodeState::Critical)]);

        assert!(
            mgr.resolve_orphaned_collected_alerts().is_empty(),
            "no config installed is not the same as no rules configured"
        );
        assert_eq!(mgr.active_alerts().len(), 1);

        // The other side: once a config really is installed and really has no rule, it closes.
        mgr.set_config(cfg(Vec::new(), meta_for(node)));
        assert_eq!(mgr.resolve_orphaned_collected_alerts().len(), 1);
    }

    // ---- ADR-097 Increment 4: a deleted node's alerts are closed, not abandoned ----

    /// A manager holding one open liveness alert per node named, over `meta` as the inventory.
    ///
    /// ⚠️ Several tests below keep a second node in `meta` purely so the map is not *empty* after
    /// the deletion — an empty map means "no config installed" and is a deliberate no-op, so a test
    /// that deleted the only node would pass for the wrong reason.
    fn with_open_liveness_alerts(
        nodes: &[NodeId],
        meta: HashMap<NodeId, NodeMeta>,
    ) -> AlertManager {
        let mgr = AlertManager::new();
        mgr.set_config(cfg(Vec::new(), meta));
        for node in nodes {
            for i in 0..i64::from(DEFAULT_LIVENESS_DWELL) {
                mgr.observe(&result(*node, CheckOutcome::Unreachable, i));
            }
        }
        mgr
    }

    /// 🚨 The accepting side, and it is written first on purpose: a sweep that resolved everything
    /// would satisfy every other test here.
    #[test]
    fn an_alert_about_a_node_the_inventory_still_holds_is_left_alone() {
        let node = NodeId::new();
        let mgr = with_open_liveness_alerts(&[node], meta_for(node));
        assert_eq!(mgr.active_alerts().len(), 1);

        assert!(
            mgr.forget_deleted_nodes().is_empty(),
            "the node is in the inventory; nothing about it is orphaned"
        );
        assert_eq!(mgr.active_alerts().len(), 1);
        assert_eq!(mgr.node_state(node), Some(NodeState::Unreachable));
    }

    /// The whole point: nothing else can ever close this alert, because nothing polls a node that
    /// no longer exists.
    #[test]
    fn an_alert_about_a_deleted_node_is_resolved_and_forgotten() {
        let kept = NodeId::new();
        let gone = NodeId::new();
        let mut meta = meta_for(kept);
        meta.insert(gone, NodeMeta::default());
        let mgr = with_open_liveness_alerts(&[kept, gone], meta);
        assert_eq!(mgr.active_alerts().len(), 2);

        // The node is deleted, so the next config load no longer carries it.
        mgr.set_config(cfg(Vec::new(), meta_for(kept)));
        let actions = mgr.forget_deleted_nodes();

        assert_eq!(actions.len(), 1, "exactly the deleted node");
        let NotifyAction::Resolve(closed) = &actions[0] else {
            panic!("it must close as a resolution, which is what shuts the external incident");
        };
        assert_eq!(closed.node(), Some(gone));
        assert_eq!(mgr.active_alerts().len(), 1);
        assert_eq!(mgr.active_alerts()[0].node(), Some(kept));
        assert_eq!(
            mgr.node_state(gone),
            None,
            "and it leaves the display tally"
        );
        assert_eq!(mgr.node_state(kept), Some(NodeState::Unreachable));
    }

    /// It runs on every config-refresh cycle for the life of the process.
    #[test]
    fn the_deleted_node_sweep_is_idempotent() {
        let kept = NodeId::new();
        let gone = NodeId::new();
        let mut meta = meta_for(kept);
        meta.insert(gone, NodeMeta::default());
        let mgr = with_open_liveness_alerts(&[gone], meta);

        mgr.set_config(cfg(Vec::new(), meta_for(kept)));
        assert_eq!(mgr.forget_deleted_nodes().len(), 1);
        assert!(mgr.forget_deleted_nodes().is_empty());
        assert!(mgr.forget_deleted_nodes().is_empty());
    }

    /// 🚨 The disaster case. `AlertConfig::default()` is what the engine holds between start-up and
    /// the first config refresh — and `restore` has already seeded the active set by then. Reading
    /// an empty map as "every node was deleted" would resolve the whole fleet and page a recovery
    /// for each, which is exactly the accident ADR-080 paid for once.
    #[test]
    fn an_empty_node_map_resolves_nothing() {
        let node = NodeId::new();
        let mgr = with_open_liveness_alerts(&[node], meta_for(node));
        assert_eq!(mgr.active_alerts().len(), 1);

        // Direction 1: a real inventory that still holds the node.
        assert!(mgr.forget_deleted_nodes().is_empty());

        // Direction 2: no config installed at all.
        mgr.set_config(AlertConfig::default());
        assert!(
            mgr.forget_deleted_nodes().is_empty(),
            "an empty node map means no config has been installed, never that the fleet is empty"
        );
        assert_eq!(mgr.active_alerts().len(), 1);
        assert_eq!(mgr.node_state(node), Some(NodeState::Unreachable));
    }

    /// The half the alerts alone do not cover: a deleted node that never had an alert still sits in
    /// `live` and in the suppression down-set. `api/fleet.rs::state_tally` takes its total from
    /// PostgreSQL and its breakdown from here, so leaving it makes the breakdown sum to more than
    /// the total — which is how this was found on the lab core.
    #[test]
    fn a_deleted_node_leaves_the_display_tally_and_the_down_set() {
        let kept = NodeId::new();
        let gone = NodeId::new();
        let mut meta = meta_for(kept);
        meta.insert(gone, NodeMeta::default());

        // No liveness rule, so the state commits and the down-set moves while nobody is paged
        // (ADR-075). That is what makes this an alert-free case.
        let mgr = AlertManager::new();
        mgr.set_config(AlertConfig::new(Vec::new(), meta));
        for i in 0..=i64::from(DEFAULT_LIVENESS_DWELL) {
            mgr.observe(&result(gone, CheckOutcome::Unreachable, i));
            mgr.observe(&result(kept, CheckOutcome::Reachable, i));
        }
        assert!(mgr.active_alerts().is_empty(), "no rule means no alert");
        assert!(mgr.down_set().contains(&gone));
        assert_eq!(mgr.node_states().len(), 2);

        mgr.set_config(AlertConfig::new(Vec::new(), meta_for(kept)));
        assert!(
            mgr.forget_deleted_nodes().is_empty(),
            "there was never an alert to close — and the sweep must still do its second half"
        );
        assert_eq!(
            mgr.node_states().len(),
            1,
            "the tally must stop counting a node the inventory no longer holds"
        );
        assert!(
            !mgr.down_set().contains(&gone),
            "a node that no longer exists must not keep suppressing anything"
        );
    }

    /// A pool-coverage alert has no inventory row that could be missing, so this sweep must not be
    /// the thing that closes it — `resolve_pool_coverage_alert` owns that.
    #[test]
    fn a_pool_subject_is_never_swept_as_a_deleted_node() {
        let node = NodeId::new();
        let mgr = AlertManager::new();
        mgr.set_config(cfg(Vec::new(), meta_for(node)));
        assert!(mgr.raise_pool_coverage_alert("site-a", 1).is_some());
        assert_eq!(mgr.active_alerts().len(), 1);

        assert!(mgr.forget_deleted_nodes().is_empty());
        assert_eq!(mgr.active_alerts().len(), 1);
    }

    /// 🚨 The trap `resolve_orphans` documents, met from this direction: a config-bundle import
    /// preserves node ids, so a node can come back with the same id. If the sweep resolved the alert
    /// without dropping the dwell state, the re-created node would observe `Unreachable →
    /// Unreachable`, see no transition, and never fire again for the life of the process.
    #[test]
    fn a_node_recreated_with_the_same_id_can_alert_again() {
        let node = NodeId::new();
        let keeper = NodeId::new();
        let mut meta = meta_for(keeper);
        meta.insert(node, NodeMeta::default());
        let mgr = with_open_liveness_alerts(&[node], meta);
        assert_eq!(mgr.active_alerts().len(), 1);

        mgr.set_config(cfg(Vec::new(), meta_for(keeper)));
        assert_eq!(mgr.forget_deleted_nodes().len(), 1);
        assert!(mgr.active_alerts().is_empty());

        let mut back = meta_for(keeper);
        back.insert(node, NodeMeta::default());
        mgr.set_config(cfg(Vec::new(), back));
        for i in 200..(200 + i64::from(DEFAULT_LIVENESS_DWELL)) {
            mgr.observe(&result(node, CheckOutcome::Unreachable, i));
        }
        assert_eq!(
            mgr.active_alerts().len(),
            1,
            "the check must be able to fire again"
        );
    }

    // ---- ADR-097 Increment 5: a node deleted while core was stopped is closed too ----

    /// 🚨 **The accepting side, written first for the same reason as Increment 4's.** A
    /// `restore_deleted` that seeded the fleet exactly like `restore` would satisfy the test below
    /// it, and the damage would be dependency suppression: a node that no longer exists sitting in
    /// the down set, silencing whatever topology says is behind it, until the first sweep.
    #[test]
    fn restore_seeds_the_fleet_but_restore_deleted_does_not() {
        let live_node = NodeId::new();
        let gone_node = NodeId::new();

        let mgr = AlertManager::new();
        mgr.restore(vec![open_alert(
            live_node,
            LIVENESS,
            NodeState::Unreachable,
        )]);
        assert_eq!(
            mgr.node_state(live_node),
            Some(NodeState::Unreachable),
            "a node that still exists reads correctly from the first second (decision 2)"
        );
        assert!(
            mgr.down_set().contains(&live_node),
            "and suppression knows about it"
        );

        let mgr = AlertManager::new();
        mgr.restore_deleted(vec![open_alert(
            gone_node,
            LIVENESS,
            NodeState::Unreachable,
        )]);
        assert_eq!(
            mgr.active_alerts().len(),
            1,
            "the alert itself is restored — that is what lets the sweep close it"
        );
        assert!(
            mgr.down_set().is_empty(),
            "but a deleted node must never suppress anything"
        );
        // ⚠️ Asserted positively because it surprises: `node_states` unions `live` with every
        // active alert's node, so the deleted node *is* in the rolled-up display state despite not
        // being seeded into `live`. That is the transient `restore_deleted`'s doc names — the sweep
        // removes it — and pinning it here stops a future reader "fixing" the seeding and believing
        // they fixed the tally.
        assert_eq!(mgr.node_state(gone_node), Some(NodeState::Unreachable));
    }

    /// The defect itself, end to end within the engine: an alert read back from the log about a
    /// node the inventory no longer holds is closed **as a resolution**, which is what shuts the
    /// incident in the external tool.
    ///
    /// Before Increment 5 the restore dropped this row, so `active` never held it, so the sweep —
    /// which reads `active` — could not see it. Measured on hardware: 43,227 rows left open.
    #[test]
    fn an_alert_restored_about_a_node_deleted_while_core_was_down_is_closed() {
        let kept = NodeId::new();
        let gone = NodeId::new();
        let mgr = AlertManager::new();
        // The config the restart loads: the deleted node is simply not in it.
        mgr.set_config(cfg(Vec::new(), meta_for(kept)));
        mgr.restore_deleted(vec![open_alert(gone, LIVENESS, NodeState::Unreachable)]);

        let actions = mgr.forget_deleted_nodes();
        assert_eq!(actions.len(), 1, "the orphan, and only it");
        let NotifyAction::Resolve(closed) = &actions[0] else {
            panic!("it must close as a resolution, not merely vanish from memory");
        };
        assert_eq!(closed.node(), Some(gone));
        assert!(mgr.active_alerts().is_empty());
        assert!(
            mgr.forget_deleted_nodes().is_empty(),
            "and it closes once, not on every tick"
        );
    }

    /// 🚨 The other accepting side: restoring an alert about a node that **is** in the inventory
    /// must not make it closeable. Without this, "orphan" could be spelled backwards — or dropped
    /// entirely — and the whole live fleet would be resolved on the first tick after every restart.
    #[test]
    fn a_restored_alert_about_a_live_node_is_not_closed() {
        let node = NodeId::new();
        let mgr = AlertManager::new();
        mgr.set_config(cfg(Vec::new(), meta_for(node)));
        mgr.restore(vec![open_alert(node, LIVENESS, NodeState::Unreachable)]);

        assert!(
            mgr.forget_deleted_nodes().is_empty(),
            "the node is in the inventory; its outage is still real"
        );
        assert_eq!(mgr.active_alerts().len(), 1);
        assert_eq!(mgr.node_state(node), Some(NodeState::Unreachable));
    }
}

/// ADR-143: a vendor table's rows are checks of their own.
///
/// A sibling of `tests` rather than more of it: these are about one decision, and the fixtures they
/// share (a percentage table metric, a named row) are not what the rest of the engine's tests use.
#[cfg(test)]
mod row_tests {
    use super::super::testkit::*;
    use super::*;
    use yagra_common::{ScopeLevel, ThresholdBounds, ThresholdRule};

    const MEM: &str = "huawei_mem_usage";

    fn rule(warning: f64, critical: f64, dwell: u32, row_match: Option<&str>) -> StoredThreshold {
        StoredThreshold::new(
            Uuid::new_v4(),
            ScopeLevel::Global,
            Vec::new(),
            ThresholdRule::new(
                MEM,
                ThresholdBounds::above(Some(warning), Some(critical)),
                dwell,
            ),
        )
        .with_row_match(row_match.map(str::to_owned))
    }

    fn rows(node: NodeId, values: &[(u32, f64)], at: i64) -> PollResult {
        let mut r = result(node, CheckOutcome::Reachable, at);
        r.samples = values
            .iter()
            .map(|(row, v)| Sample::interface(MEM, IfIndex(*row), *v, MetricKind::Gauge))
            .collect();
        r
    }

    fn name(metric: &str, row: u32, name: &str) -> RowName {
        RowName {
            metric: metric.to_owned(),
            row,
            name: name.to_owned(),
        }
    }

    fn fires(actions: &[NotifyAction]) -> Vec<&Alert> {
        actions
            .iter()
            .filter_map(|a| match a {
                NotifyAction::Fire(alert) => Some(alert),
                _ => None,
            })
            .collect()
    }

    fn resolves(actions: &[NotifyAction]) -> Vec<&Alert> {
        actions
            .iter()
            .filter_map(|a| match a {
                NotifyAction::Resolve(alert) => Some(alert),
                _ => None,
            })
            .collect()
    }

    fn setup(rules: Vec<StoredThreshold>) -> (AlertManager, NodeId) {
        let node = NodeId::new();
        let mgr = manager();
        mgr.set_config(cfg(rules, meta_for(node)));
        (mgr, node)
    }

    /// ADR-143 decision 3: a stored name stands in only until a poll delivers one, so the seed may
    /// fill a row no poll has named but never replaces a name a poll did — whichever came first.
    #[test]
    fn a_seeded_row_name_never_replaces_one_a_poll_delivered() {
        let (mgr, node) = setup(Vec::new());
        let named = |row: u32| {
            mgr.row_names
                .read()
                .unwrap()
                .get(&node)
                .and_then(|metrics| metrics.get(MEM))
                .and_then(|rows| rows.get(&row))
                .cloned()
        };

        // The poll first, then a seed carrying a stale name for the same row: the poll's name
        // stands, and only the row the poll did not name is taken.
        mgr.record_row_names(node, &[name(MEM, 7, "MPU Board 0")]);
        let taken = mgr.seed_row_names([
            (node, MEM.to_owned(), 7, "stale".to_owned()),
            (node, MEM.to_owned(), 8, "MPU Board 1".to_owned()),
        ]);
        assert_eq!(taken, 1);
        assert_eq!(named(7).as_deref(), Some("MPU Board 0"));
        assert_eq!(named(8).as_deref(), Some("MPU Board 1"));

        // The seed first, then a poll: the poll's name replaces the seeded one.
        mgr.record_row_names(node, &[name(MEM, 8, "MPU Board 1 (renamed)")]);
        assert_eq!(named(8).as_deref(), Some("MPU Board 1 (renamed)"));
    }

    /// ADR-143 Inc.2: the engine cleans and caps what it keeps rather than trusting the poller to
    /// have done it — a name held here becomes an alert's `row_name` and reaches every notification.
    #[test]
    fn the_engine_keeps_a_cleaned_name_and_no_more_rows_than_the_cap() {
        let (mgr, node) = setup(Vec::new());
        let held_rows = || {
            mgr.row_names
                .read()
                .unwrap()
                .get(&node)
                .and_then(|metrics| metrics.get(MEM))
                .map_or(0, |rows| rows.len())
        };

        mgr.record_row_names(node, &[name(MEM, 7, "MPU\nBoard 0")]);
        let named = mgr
            .row_names
            .read()
            .unwrap()
            .get(&node)
            .and_then(|metrics| metrics.get(MEM))
            .and_then(|rows| rows.get(&7))
            .cloned();
        assert_eq!(named.as_deref(), Some("MPU Board 0"));

        let many: Vec<RowName> = (100..700).map(|row| name(MEM, row, "pool")).collect();
        mgr.record_row_names(node, &many);
        assert_eq!(
            held_rows(),
            1 + yagra_common::row_names::ROW_NAMES_MAX,
            "row 7 from before, plus exactly the cap from a result that carried 600"
        );
    }

    /// The accepting side first: a breaching row fires on its own check, named, and a healthy row
    /// and a zero row leave nothing behind — the property that keeps a 306-row table affordable.
    #[test]
    fn a_breaching_row_fires_on_its_own_check_and_a_healthy_row_holds_no_state() {
        let (mgr, node) = setup(vec![rule(80.0, 90.0, 1, None)]);
        mgr.record_row_names(node, &[name(MEM, 7, "MPU Board 0")]);
        let actions = mgr.observe(&rows(node, &[(7, 85.0), (8, 33.0), (9, 0.0)], 1));
        let fired = fires(&actions);
        assert_eq!(fired.len(), 1, "{actions:?}");
        assert_eq!(fired[0].check, row_check_id(node, 7, MEM));
        assert_eq!(fired[0].row, Some(7));
        assert_eq!(fired[0].row_name.as_deref(), Some("MPU Board 0"));
        assert_eq!(fired[0].ifindex, None, "a row is not a port");

        let states = mgr.states.lock().unwrap();
        assert!(states.contains_key(&row_check_id(node, 7, MEM)));
        assert!(!states.contains_key(&row_check_id(node, 8, MEM)));
        assert!(!states.contains_key(&row_check_id(node, 9, MEM)));
        assert!(
            !states.contains_key(&check_id(node, MEM)),
            "no node-wide check for a table metric"
        );
    }

    /// The ADR-077 failure, both directions, now structurally impossible: a bad row among good ones
    /// reaches its dwell, and two bad rows do not reach it in one poll.
    #[test]
    fn each_row_keeps_its_own_dwell_window() {
        let (mgr, node) = setup(vec![rule(80.0, 90.0, 3, None)]);
        for at in 1..=2 {
            let actions = mgr.observe(&rows(node, &[(7, 85.0), (8, 10.0), (9, 86.0)], at));
            assert!(fires(&actions).is_empty(), "poll {at}: {actions:?}");
        }
        let actions = mgr.observe(&rows(node, &[(7, 85.0), (8, 10.0), (9, 86.0)], 3));
        let mut fired: Vec<Option<u32>> = fires(&actions).iter().map(|a| a.row).collect();
        fired.sort();
        assert_eq!(fired, vec![Some(7), Some(9)]);
    }

    /// Recovery and a new breach are two incidents, not one flip of a shared check.
    #[test]
    fn a_row_recovers_and_another_breaches_as_separate_incidents() {
        let (mgr, node) = setup(vec![rule(80.0, 90.0, 1, None)]);
        let _ = mgr.observe(&rows(node, &[(7, 85.0), (8, 10.0)], 1));
        let actions = mgr.observe(&rows(node, &[(7, 50.0), (8, 95.0)], 2));
        assert_eq!(
            resolves(&actions).iter().map(|a| a.row).collect::<Vec<_>>(),
            vec![Some(7)]
        );
        assert_eq!(
            fires(&actions).iter().map(|a| a.row).collect::<Vec<_>>(),
            vec![Some(8)]
        );
        assert_eq!(mgr.active_alerts().len(), 1);
    }

    /// Decision 7 end to end: at the same scope, the rule naming the row wins for that row alone.
    #[test]
    fn a_rule_naming_a_row_governs_that_row_and_only_that_row() {
        let (mgr, node) = setup(vec![
            rule(80.0, 90.0, 1, None),
            rule(90.0, 95.0, 1, Some("MPU Board 0")),
        ]);
        mgr.record_row_names(
            node,
            &[name(MEM, 7, "MPU Board 0"), name(MEM, 8, "MPU Board 1")],
        );
        let actions = mgr.observe(&rows(node, &[(7, 85.0), (8, 85.0)], 1));
        let fired = fires(&actions);
        assert_eq!(fired.len(), 1, "{actions:?}");
        assert_eq!(fired[0].row_name.as_deref(), Some("MPU Board 1"));
    }

    /// A row with no name yet is judged by the rules without a pattern — never by none at all.
    #[test]
    fn an_unnamed_row_falls_back_to_the_unpatterned_rule() {
        let (mgr, node) = setup(vec![
            rule(80.0, 90.0, 1, None),
            rule(90.0, 95.0, 1, Some("MPU Board 0")),
        ]);
        let actions = mgr.observe(&rows(node, &[(7, 85.0)], 1));
        assert_eq!(fires(&actions).len(), 1, "{actions:?}");
    }

    /// Decision 6: the node-wide alert a table metric had before this version is closed by the first
    /// per-row observation, with a real resolve — so the incident it opened externally closes too.
    #[test]
    fn a_restored_node_wide_alert_is_closed_by_the_first_per_row_observation() {
        let (mgr, node) = setup(vec![rule(80.0, 90.0, 1, None)]);
        let legacy = Alert {
            subject: Subject::Node(node),
            check: check_id(node, MEM),
            severity: Severity::Warning,
            state: NodeState::Warning,
            at_unix_ms: 0,
            root_cause: None,
            flapping: false,
            metric: MEM.to_owned(),
            breach: None,
            ifindex: None,
            row: None,
            row_name: None,
        };
        assert_eq!(mgr.restore(vec![legacy]), 1);
        let actions = mgr.observe(&rows(node, &[(7, 50.0)], 1));
        let resolved = resolves(&actions);
        assert_eq!(resolved.len(), 1, "{actions:?}");
        assert_eq!(resolved[0].check, check_id(node, MEM));
        assert!(mgr.active_alerts().is_empty());
        // …and only once: the next poll has nothing left to retire.
        assert!(resolves(&mgr.observe(&rows(node, &[(7, 50.0)], 2))).is_empty());
    }

    /// 🚨 Decision 5's one way to be wrong: a restored row alert must be in the index of rows holding
    /// a state, or its healthy sample is skipped and it never resolves.
    #[test]
    fn a_restored_row_alert_resolves_when_its_row_recovers() {
        let (mgr, node) = setup(vec![rule(80.0, 90.0, 1, None)]);
        let restored = Alert {
            subject: Subject::Node(node),
            check: row_check_id(node, 7, MEM),
            severity: Severity::Warning,
            state: NodeState::Warning,
            at_unix_ms: 0,
            root_cause: None,
            flapping: false,
            metric: MEM.to_owned(),
            breach: None,
            ifindex: None,
            row: Some(7),
            row_name: Some("MPU Board 0".to_owned()),
        };
        assert_eq!(mgr.restore(vec![restored]), 1);
        let actions = mgr.observe(&rows(node, &[(7, 10.0)], 1));
        assert_eq!(resolves(&actions).len(), 1, "{actions:?}");
        assert!(mgr.active_alerts().is_empty());
    }

    /// A derived table metric alerts per row, named from its first input's rows — the C2960S this ADR
    /// started from, where the Processor pool is healthy and the I/O pool is not.
    #[test]
    fn a_derived_metric_alerts_per_row_under_its_input_rows_name() {
        let pct = crate::derived::METRIC_CISCO_MEM_USED_PCT;
        let node = NodeId::new();
        let mgr = manager();
        let base = StoredThreshold::new(
            Uuid::new_v4(),
            ScopeLevel::Global,
            Vec::new(),
            ThresholdRule::new(pct, ThresholdBounds::above(Some(80.0), Some(90.0)), 1),
        );
        mgr.set_config(cfg(vec![base.clone()], meta_for(node)));
        mgr.record_row_names(
            node,
            &[
                name("cisco_mem_used", 1, "Processor"),
                name("cisco_mem_used", 2, "I/O"),
                name("cisco_mem_used", 20, "Driver text"),
            ],
        );
        let _ = mgr.observe(&result(node, CheckOutcome::Reachable, 0));
        let pools = [(1, 56.4), (2, 83.9), (20, 0.004)];
        let actions = mgr
            .observe_derived_metric(node, pct, &pools, 1)
            .expect("a rule is in force");
        let fired = fires(&actions);
        assert_eq!(fired.len(), 1, "{actions:?}");
        assert_eq!(fired[0].row, Some(2));
        assert_eq!(fired[0].row_name.as_deref(), Some("I/O"));

        // The shipped default for the I/O pool (90/95) resolves it without touching the others.
        let io = StoredThreshold::new(
            Uuid::new_v4(),
            ScopeLevel::Global,
            Vec::new(),
            ThresholdRule::new(pct, ThresholdBounds::above(Some(90.0), Some(95.0)), 1),
        )
        .with_row_match(Some("I/O".to_owned()));
        mgr.set_config(cfg(vec![base, io], meta_for(node)));
        let actions = mgr
            .observe_derived_metric(node, pct, &pools, 2)
            .expect("rules are in force");
        assert_eq!(resolves(&actions).len(), 1, "{actions:?}");
        assert!(fires(&actions).is_empty());
        assert!(mgr.active_alerts().is_empty());
    }

    /// A derived metric computed from scalars keeps its one node-wide check — no existing check id
    /// moves for a Net-SNMP host.
    #[test]
    fn a_scalar_derived_metric_keeps_its_node_wide_check() {
        let pct = crate::derived::METRIC_UCD_MEM_USED_PCT;
        let node = NodeId::new();
        let mgr = manager();
        mgr.set_config(cfg(
            vec![StoredThreshold::new(
                Uuid::new_v4(),
                ScopeLevel::Global,
                Vec::new(),
                ThresholdRule::new(pct, ThresholdBounds::above(Some(80.0), None), 1),
            )],
            meta_for(node),
        ));
        let _ = mgr.observe(&result(node, CheckOutcome::Reachable, 0));
        let actions = mgr
            .observe_derived_metric(node, pct, &[(0, 91.0)], 1)
            .expect("a rule is in force");
        let fired = fires(&actions);
        assert_eq!(fired.len(), 1, "{actions:?}");
        assert_eq!(fired[0].check, check_id(node, pct));
        assert_eq!(fired[0].row, None);
    }
}

/// ADR-156: a vendor's "no reading" placeholder is evidence about a table row, and nothing else is.
///
/// Built on the AC6508 measurement the ADR started from — fifteen `hwEntityTemperature` rows, twelve
/// of them the placeholder — and on the one decision the user made explicitly: a row that merely
/// stops arriving must never close its alert, because that is also what a monitoring fault looks
/// like.
#[cfg(test)]
mod no_reading_tests {
    use super::super::testkit::*;
    use super::*;
    use crate::no_reading_filter::{
        ac6508_temperature_rows, NoReadingHandle, NoReadingMarkers, AC6508_MARKER,
    };
    use yagra_common::{
        CollectionItem, CollectionKind, ScopeLevel, ThresholdBounds, ThresholdRule,
    };

    const TEMP: &str = "huawei_temp";
    const PLACEHOLDER_ROWS: [u32; 12] = [3, 5, 14, 78, 142, 206, 270, 334, 398, 462, 526, 590];

    /// The seeded Huawei default's bounds and dwell (`repo/defaults.rs`: above 70/80, dwell 2), at
    /// global scope so no profile has to be wired up to reach it.
    fn default_temp_rule() -> StoredThreshold {
        StoredThreshold::new(
            Uuid::new_v4(),
            ScopeLevel::Global,
            Vec::new(),
            ThresholdRule::new(TEMP, ThresholdBounds::above(Some(70.0), Some(80.0)), 2),
        )
    }

    fn setup() -> (AlertManager, NodeId) {
        let node = NodeId::new();
        let mgr = manager();
        mgr.set_config(cfg(vec![default_temp_rule()], meta_for(node)));
        (mgr, node)
    }

    /// The ingest boundary as production builds it: the table from the built-in column, then
    /// `admit`, so every test here observes exactly what `ingest_result` would hand the engine.
    fn handle() -> NoReadingHandle {
        let handle = NoReadingHandle::default();
        handle.publish(NoReadingMarkers::from_items(&[CollectionItem {
            metric_name: TEMP.to_owned(),
            oid: "1.3.6.1.4.1.2011.5.25.31.1.1.1.1.11".to_owned(),
            kind: CollectionKind::Table,
            metric_kind: MetricKind::Gauge,
        }]));
        handle
    }

    fn observe(
        mgr: &AlertManager,
        node: NodeId,
        samples: Vec<Sample>,
        at: i64,
    ) -> Vec<NotifyAction> {
        let mut r = result(node, CheckOutcome::Reachable, at);
        r.samples = samples;
        let admitted = handle().admit(r);
        mgr.observe_with_no_reading(admitted.result(), admitted.no_reading())
    }

    fn row(r: u32, value: f64) -> Sample {
        Sample::interface(TEMP, IfIndex(r), value, MetricKind::Gauge)
    }

    fn fires(actions: &[NotifyAction]) -> usize {
        actions
            .iter()
            .filter(|a| matches!(a, NotifyAction::Fire(_)))
            .count()
    }

    fn resolved_rows(actions: &[NotifyAction]) -> Vec<u32> {
        let mut rows: Vec<u32> = actions
            .iter()
            .filter_map(|a| match a {
                NotifyAction::Resolve(alert) => alert.row,
                _ => None,
            })
            .collect();
        rows.sort_unstable();
        rows
    }

    /// One of the twelve alerts the PoC had open, as `alert_history` gives it back after a restart.
    fn open_row_alert(node: NodeId, row: u32) -> Alert {
        Alert {
            subject: Subject::Node(node),
            check: row_check_id(node, row, TEMP),
            severity: Severity::Critical,
            state: NodeState::Critical,
            at_unix_ms: 0,
            root_cause: None,
            flapping: false,
            metric: TEMP.to_owned(),
            breach: None,
            ifindex: None,
            row: Some(row),
            row_name: None,
        }
    }

    /// The upgrade, end to end: twelve alerts restored from history, then the device keeps answering
    /// its placeholder. Nothing on the first poll — the rule's dwell is two — and all twelve on the
    /// second, as ordinary resolves.
    #[test]
    fn the_restored_ac6508_alerts_close_after_the_rules_dwell_of_placeholders() {
        let (mgr, node) = setup();
        let restored: Vec<Alert> = PLACEHOLDER_ROWS
            .iter()
            .map(|r| open_row_alert(node, *r))
            .collect();
        assert_eq!(mgr.restore(restored), 12);

        let first = observe(&mgr, node, ac6508_temperature_rows(), 1);
        assert!(resolved_rows(&first).is_empty(), "{first:?}");
        assert_eq!(mgr.active_alerts().len(), 12);

        let second = observe(&mgr, node, ac6508_temperature_rows(), 2);
        assert_eq!(
            resolved_rows(&second),
            PLACEHOLDER_ROWS.to_vec(),
            "{second:?}"
        );
        assert_eq!(fires(&second), 0);
        assert!(mgr.active_alerts().is_empty());

        // …and it stays closed: the placeholder never becomes a value to breach with.
        for at in 3..8 {
            assert!(observe(&mgr, node, ac6508_temperature_rows(), at).is_empty());
        }
        assert!(mgr.active_alerts().is_empty());
    }

    /// 🚨 **ADR-156 決定 3, the user's decision.** A row that stops arriving is what a poller defect, a
    /// truncated walk and a changed SNMP view all look like, so it must keep its alert open however
    /// long it is gone. Only the device answering the placeholder closes it.
    #[test]
    fn a_row_that_stops_arriving_keeps_its_alert_until_the_device_answers_its_placeholder() {
        let (mgr, node) = setup();
        // Row 3 genuinely overheats and fires after the dwell.
        let _ = observe(&mgr, node, vec![row(3, 90.0), row(9, 58.0)], 1);
        assert_eq!(
            fires(&observe(&mgr, node, vec![row(3, 90.0), row(9, 58.0)], 2)),
            1
        );

        // Then row 3 is simply absent while row 9 keeps arriving — for longer than any dwell.
        for at in 3..20 {
            let actions = observe(&mgr, node, vec![row(9, 58.0)], at);
            assert!(
                resolved_rows(&actions).is_empty(),
                "absence closed row 3: {actions:?}"
            );
        }
        assert_eq!(
            mgr.active_alerts().len(),
            1,
            "a missing row is not a recovered row"
        );

        // The device then answers row 3 with its placeholder: evidence, through the dwell.
        let placeholder = vec![row(3, AC6508_MARKER), row(9, 58.0)];
        assert!(resolved_rows(&observe(&mgr, node, placeholder.clone(), 20)).is_empty());
        assert_eq!(
            resolved_rows(&observe(&mgr, node, placeholder, 21)),
            vec![3]
        );
        assert!(mgr.active_alerts().is_empty());
    }

    /// A placeholder on a row with no state creates none — the same fifteen rows that raised twelve
    /// criticals as values raise nothing once they are recognised. The contrast half runs the very
    /// same samples as values, so this cannot pass because nothing ever fires.
    #[test]
    fn a_placeholder_is_never_judged_as_a_value() {
        let (mgr, node) = setup();
        for at in 1..6 {
            assert!(observe(&mgr, node, ac6508_temperature_rows(), at).is_empty());
        }
        assert!(mgr.active_alerts().is_empty());
        // No state was created either: a real 85 on a placeholder row still needs the full dwell.
        assert_eq!(fires(&observe(&mgr, node, vec![row(3, 85.0)], 6)), 0);
        assert_eq!(fires(&observe(&mgr, node, vec![row(3, 85.0)], 7)), 1);

        // Contrast: the same rows handed over as values are today's bug, twelve criticals.
        let (mgr, node) = setup();
        let mut r = result(node, CheckOutcome::Reachable, 1);
        r.samples = ac6508_temperature_rows();
        let _ = mgr.observe(&r);
        r.at_unix_ms = 2;
        assert_eq!(fires(&mgr.observe(&r)), 12);
    }

    /// A sensor that answers a value and its placeholder on alternate polls is not closed by the
    /// placeholders and not re-fired by the values: each one resets the other's dwell, exactly as an
    /// in-band reading would. Closing on the spot would instead hide the real 85 forever.
    #[test]
    fn a_sensor_alternating_with_its_placeholder_neither_fires_nor_resolves() {
        let (mgr, node) = setup();
        // Ends on a placeholder (at = 10), so the two values below start a fresh dwell.
        for at in 1..=10 {
            let value = if at % 2 == 1 { 85.0 } else { AC6508_MARKER };
            assert_eq!(fires(&observe(&mgr, node, vec![row(3, value)], at)), 0);
        }
        assert!(mgr.active_alerts().is_empty());

        // Once it has fired, alternating does not resolve it.
        let _ = observe(&mgr, node, vec![row(3, 85.0)], 11);
        assert_eq!(fires(&observe(&mgr, node, vec![row(3, 85.0)], 12)), 1);
        for at in 13..21 {
            let value = if at % 2 == 1 { AC6508_MARKER } else { 85.0 };
            let actions = observe(&mgr, node, vec![row(3, value)], at);
            assert!(resolved_rows(&actions).is_empty(), "{actions:?}");
            assert_eq!(fires(&actions), 0, "{actions:?}");
        }
        assert_eq!(mgr.active_alerts().len(), 1);
    }

    /// A placeholder with no row key says nothing about a node-wide check, which folds several rows
    /// into one: it is not stored, and it closes nothing.
    #[test]
    fn a_placeholder_without_a_row_closes_nothing() {
        let (mgr, node) = setup();
        let _ = observe(&mgr, node, vec![Sample::gauge(TEMP, 90.0)], 1);
        assert_eq!(
            fires(&observe(&mgr, node, vec![Sample::gauge(TEMP, 90.0)], 2)),
            1
        );
        for at in 3..10 {
            let actions = observe(&mgr, node, vec![Sample::gauge(TEMP, AC6508_MARKER)], at);
            assert!(actions.is_empty(), "{actions:?}");
        }
        assert_eq!(mgr.active_alerts().len(), 1);
    }

    /// ADR-143 決定 6 counts a placeholder row as a per-row observation: the node-wide alert a
    /// pre-ADR-143 core raised on the 2147483647 maximum is retired by it, as by any row.
    #[test]
    fn the_first_placeholder_row_retires_a_restored_node_wide_alert() {
        let (mgr, node) = setup();
        let legacy = Alert {
            row: None,
            check: check_id(node, TEMP),
            ..open_row_alert(node, 0)
        };
        assert_eq!(mgr.restore(vec![legacy]), 1);
        let actions = observe(&mgr, node, vec![row(3, AC6508_MARKER)], 1);
        let resolved: Vec<CheckId> = actions
            .iter()
            .filter_map(|a| match a {
                NotifyAction::Resolve(alert) => Some(alert.check),
                _ => None,
            })
            .collect();
        assert_eq!(resolved, vec![check_id(node, TEMP)], "{actions:?}");
        assert!(mgr.active_alerts().is_empty());
    }
}
