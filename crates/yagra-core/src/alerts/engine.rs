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

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex, RwLock};

use tokio::sync::broadcast;
use uuid::Uuid;
use yagra_alert::CheckState;
use yagra_alert::{Alert, Breach, Subject};
use yagra_bus::{CheckOutcome, PollResult, Sample};
use yagra_common::{
    CheckId, Direction, EffectiveThreshold, IfIndex, MetricKind, NodeId, NodeState, Severity,
};

use crate::thresholds::StoredThreshold;

use super::rules::*;
use super::{NotifyAction, StreamFrame};

/// Flapping detection window and threshold.
const FLAP_WINDOW_MS: i64 = 600_000;
const FLAP_THRESHOLD: usize = 5;

/// SSE broadcast buffer. Sized generously so a briefly-slow subscriber doesn't lag past the
/// window and miss events; if one does lag, the stream handler logs it and emits a `resync`
/// hint so the client can re-fetch the active-alert list (see `stream_alerts` in api.rs).
const EVENT_BUFFER: usize = 1024;
/// Node-state SSE buffer (S14). Larger than the alert buffer because a full poll sweep can emit
/// one state event per node (first observation plus genuine transitions). A subscriber that
/// overflows this gets a `resync` hint and re-seeds from REST, so the bound is a soft backstop.
const NODE_EVENT_BUFFER: usize = 4096;

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
        // One lock at a time, in the order `process_check` takes them — this runs before the ingest
        // starts, but a restore that could deadlock against a poll result would be a trap laid for
        // whoever moves the call.
        {
            let mut states = self.states.lock().expect("states mutex poisoned");
            for a in &alerts {
                states.entry(a.check).or_insert_with(|| {
                    CheckState::restored(
                        a.state,
                        DEFAULT_LIVENESS_DWELL,
                        FLAP_WINDOW_MS,
                        FLAP_THRESHOLD,
                    )
                });
            }
        }
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
        let (transition, observed) = {
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

    /// Feed a **derived node metric** through the state machine (ADR-105).
    ///
    /// The node-dimension twin of [`Self::observe_interface_metric`]: same maintenance handling,
    /// same "`None` means nobody is watching", but a node-wide [`check_id`] and no port.
    ///
    /// `values` is every row the evaluator computed for this node — a filesystem each for
    /// `hr_storage_used_pct`, a memory pool each for `cisco_mem_used_pct`, one entry for a scalar.
    /// They are folded to **one** observation here, under this rule's own direction, exactly as
    /// [`Self::observe`] folds a table walk's samples (ADR-077 decision 1). 🚨 The caller must not
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
        values: &[f64],
        at_unix_ms: i64,
    ) -> Option<Vec<NotifyAction>> {
        if !crate::interface_util::may_observe_ports(self.node_liveness(node)) {
            // Frozen, not observed. Feeding a value would either resolve a real alert the moment
            // the device went unreachable, or page about memory on a box that is already down.
            return None;
        }
        let (eff, in_maintenance) = {
            let config = self.config.read().expect("config rwlock poisoned");
            (
                config.resolve(node, None, metric),
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
        Some(self.process_check(
            node,
            raw,
            at_unix_ms,
            CheckSpec {
                check: check_id(node, metric),
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
            },
        ))
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

    /// One alert as `alerts::restore` hands it over: no `root_cause` (not stored) and not flapping.
    fn open_alert(node: NodeId, metric: &str, state: NodeState) -> Alert {
        Alert {
            subject: Subject::Node(node),
            check: check_id(node, metric),
            severity: yagra_common::Severity::Critical,
            state,
            at_unix_ms: 1_000,
            root_cause: None,
            flapping: false,
            metric: metric.to_owned(),
            breach: None,
            ifindex: None,
        }
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
