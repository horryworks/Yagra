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
use std::sync::{Arc, Mutex, RwLock};

use async_trait::async_trait;
use tokio::sync::broadcast;
use uuid::Uuid;
use yagra_alert::CheckState;
use yagra_alert::{
    Alert, Breach, Dispatcher, Notification, NotifyChannel, NotifyError, RetryPolicy,
};
use yagra_bus::{CheckOutcome, PollResult};
use yagra_common::{
    is_ssrf_blocked, resolve_effective, CheckId, Direction, EffectiveThreshold, MetricKind, NodeId,
    NodeState, ScopeLevel, ScopedThreshold, Severity,
};
use yagra_topology::Topology;

use crate::notifications::{ChannelConfig, OpenChannel, RoutingRule};
use crate::thresholds::StoredThreshold;

/// Liveness check name (distinct from any metric name). `pub(crate)` so anything that has to
/// recognise the sentinel — the RCA prompt renders it as "liveness" rather than showing an operator
/// an internal token — tests against this rather than re-spelling the literal.
pub(crate) const LIVENESS: &str = "__liveness__";
/// Consecutive samples a state must hold before it commits (anti-flap) for liveness.
const DWELL_SAMPLES: u32 = 3;
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

/// Per-node metadata used to resolve threshold scope (profile + groups).
#[derive(Debug, Clone, Default)]
pub struct NodeMeta {
    /// Profile id (as text) the node belongs to, if any.
    pub profile: Option<String>,
    /// Group identifiers (node tag values) for group-scoped thresholds.
    pub groups: BTreeSet<String>,
}

/// A snapshot of thresholds + node metadata + dependency topology the engine evaluates
/// against. Rebuilt periodically from the database so threshold/topology edits take effect
/// without a restart.
#[derive(Debug, Clone, Default)]
pub struct AlertConfig {
    /// Thresholds bucketed by metric name so per-sample resolution scans only the rules for that
    /// one metric, not the fleet's entire threshold set (S19). Built once at construction; a poll
    /// with M samples then costs O(rules-for-those-M-metrics), not O(all thresholds) × M.
    by_metric: HashMap<String, Vec<StoredThreshold>>,
    node_meta: HashMap<NodeId, NodeMeta>,
    topology: Topology,
    /// Nodes currently inside an active maintenance window (resolved at refresh time).
    maintenance: BTreeSet<NodeId>,
}

impl AlertConfig {
    /// Build a config from the stored thresholds and node metadata (no dependency edges;
    /// add them with [`Self::with_topology`]).
    #[must_use]
    pub fn new(thresholds: Vec<StoredThreshold>, node_meta: HashMap<NodeId, NodeMeta>) -> Self {
        let mut by_metric: HashMap<String, Vec<StoredThreshold>> = HashMap::new();
        for t in thresholds {
            by_metric.entry(t.rule.metric.clone()).or_default().push(t);
        }
        Self {
            by_metric,
            node_meta,
            topology: Topology::new(),
            maintenance: BTreeSet::new(),
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

    /// Resolve the effective threshold for one (node, metric), honouring scope inheritance.
    fn resolve(&self, node: NodeId, metric: &str) -> Option<EffectiveThreshold> {
        let candidates = self.by_metric.get(metric)?;
        let meta = self.node_meta.get(&node);
        let scoped: Vec<ScopedThreshold> = candidates
            .iter()
            .filter(|t| self.applies(t, node, meta))
            .map(|t| ScopedThreshold::new(t.level, t.rule.clone()))
            .collect();
        resolve_effective(&scoped)
    }

    fn applies(&self, t: &StoredThreshold, node: NodeId, meta: Option<&NodeMeta>) -> bool {
        match t.level {
            ScopeLevel::Node => t.scope_id == node.to_string(),
            ScopeLevel::Profile => {
                meta.and_then(|m| m.profile.as_deref()) == Some(t.scope_id.as_str())
            }
            ScopeLevel::Group => meta.is_some_and(|m| m.groups.contains(&t.scope_id)),
        }
    }
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
/// it measures, the dwell, whether it's the liveness check, and (for thresholds) the breach eval.
/// Bundled so [`AlertManager::process_check`] takes one descriptor instead of a long arg list.
struct CheckSpec<'a> {
    check: CheckId,
    metric: &'a str,
    dwell: u32,
    is_liveness: bool,
    eval: Option<ThresholdEval>,
}

/// Deterministic check id for a (node, check-name) pair, so the same logical check keeps a
/// stable dedup identity across restarts. Also used by the event pipeline (`events.rs`)
/// with `event:<rule-id>` names, keeping event alerts in the same identity space.
pub(crate) fn check_id(node: NodeId, name: &str) -> CheckId {
    CheckId::from(Uuid::new_v5(
        &Uuid::NAMESPACE_OID,
        format!("{node}:{name}").as_bytes(),
    ))
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
    tx: broadcast::Sender<String>,
    /// Incremental node-state (rolled-up display state) change stream for the WebUI (S14) — a
    /// dedicated channel so the inventory/topology views patch one node live instead of re-fetching
    /// the whole fleet every 15s. Kept separate from `tx` so the two event schemas don't mix.
    node_tx: broadcast::Sender<String>,
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

    /// Subscribe to the live alert event stream (JSON strings; `resolved` flag included).
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<String> {
        self.tx.subscribe()
    }

    /// Subscribe to the incremental node-state stream (S14): JSON `{node_id, state, at_unix_ms}`
    /// per rolled-up display-state change, for the inventory/topology live-patch views.
    #[must_use]
    pub fn subscribe_node_states(&self) -> broadcast::Receiver<String> {
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
        let _ = self.node_tx.send(event.to_string());
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
    #[must_use]
    pub fn node_states(&self) -> HashMap<NodeId, NodeState> {
        let mut out = self.live.lock().expect("live mutex poisoned").clone();
        for alert in self.active.lock().expect("alerts mutex poisoned").values() {
            out.entry(alert.node)
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
            .filter(|a| a.node == node)
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
            .filter(|a| a.node == node)
            .cloned()
            .collect()
    }

    /// The set of nodes currently committed `Unreachable` — the suppression down-set.
    fn down_set(&self) -> BTreeSet<NodeId> {
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

        // Inside an active maintenance window every check observes `Maintenance` instead of
        // its real state: no alert can fire (Maintenance carries no severity) and existing
        // alerts resolve after the usual dwell. The real state flows again when the window
        // ends, re-committing any surviving problem.
        let in_maintenance = self
            .config
            .read()
            .expect("config rwlock poisoned")
            .maintenance
            .contains(&node);

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
                dwell: DWELL_SAMPLES,
                is_liveness: true,
                eval: None,
            },
        ));

        // Threshold checks per sample (only metrics with a resolved threshold alert).
        for sample in &result.samples {
            let eff = {
                let config = self.config.read().expect("config rwlock poisoned");
                config.resolve(node, &sample.metric)
            };
            if let Some(eff) = eff {
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
                let eval = ThresholdEval {
                    value: sample.value,
                    direction: eff.direction,
                    warning: eff.warning,
                    critical: eff.critical,
                };
                actions.extend(self.process_check(
                    node,
                    raw,
                    result.at_unix_ms,
                    CheckSpec {
                        check: check_id(node, &sample.metric),
                        metric: &sample.metric,
                        dwell: eff.dwell_samples,
                        is_liveness: false,
                        eval: Some(eval),
                    },
                ));
            }
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
            eval,
        } = spec;
        let (transition, committed) = {
            let mut states = self.states.lock().expect("states mutex poisoned");
            let cs = states.entry(check).or_insert_with(|| {
                CheckState::new(NodeState::Ok, dwell.max(1), FLAP_WINDOW_MS, FLAP_THRESHOLD)
            });
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

        let mut actions = match t.to_alert(node, check, at_unix_ms, root_cause) {
            Some(mut alert) => {
                // Tag the alert with what it measured so the history log / notification is
                // human-readable. The crossed bound depends on the committed severity, now known.
                alert.metric = metric.to_string();
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
                .filter(|a| a.metric == LIVENESS && affected.contains(&a.node))
                .cloned()
                .collect()
        };
        let mut actions = Vec::new();
        for alert in candidates {
            let new_rc = self
                .config
                .read()
                .expect("config rwlock poisoned")
                .topology
                .root_cause(alert.node, &down);
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

    fn broadcast(&self, alert: &Alert, resolved: bool) {
        // Wire shape the WebUI consumes (Alert fields + a `resolved` flag).
        let event = serde_json::json!({
            "node": alert.node,
            "check": alert.check,
            "severity": alert.severity,
            "state": alert.state,
            "at_unix_ms": alert.at_unix_ms,
            "root_cause": alert.root_cause,
            "flapping": alert.flapping,
            "metric": alert.metric,
            "breach": alert.breach,
            "resolved": resolved,
        });
        // Fire-and-forget: no subscribers is not an error.
        let _ = self.tx.send(event.to_string());
    }

    /// Broadcast an inbound ack-state change for one alert so subscribers update the read-only
    /// acked indicator live (ADR-015). Finds the matching active alert by its dedup identity
    /// `(node, check, severity)` and re-sends its wire shape with `acked` attached (the external
    /// tool's view as a JSON value, or `null` when cleared). No `resolved` flag ⇒ the client
    /// treats it as an upsert, not a recovery. If the alert isn't currently active there's
    /// nothing on screen to update, so this is a no-op (History reflects it on next fetch).
    pub fn broadcast_acked(
        &self,
        node: Uuid,
        check: Uuid,
        severity: Severity,
        acked: Option<serde_json::Value>,
    ) {
        let active = self.active.lock().expect("alerts mutex poisoned");
        let Some(alert) = active.values().find(|a| {
            a.node.as_uuid() == node && a.check.as_uuid() == check && a.severity == severity
        }) else {
            return;
        };
        let event = serde_json::json!({
            "node": alert.node,
            "check": alert.check,
            "severity": alert.severity,
            "state": alert.state,
            "at_unix_ms": alert.at_unix_ms,
            "root_cause": alert.root_cause,
            "flapping": alert.flapping,
            "metric": alert.metric,
            "breach": alert.breach,
            "acked": acked,
        });
        let _ = self.tx.send(event.to_string());
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
fn dedup_string(key: &yagra_alert::DedupKey) -> String {
    format!("yagra:{}:{}:{}", key.node, key.check, key.severity.as_str())
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
            "source": notification.dedup_key.node.to_string(),
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

/// The JSM/Opsgenie close-by-alias URL (alias chars are UUID hex/dashes/colons — all
/// valid in a path segment, no encoding needed).
fn jsm_close_url(api_url: &str, notification: &Notification) -> String {
    format!(
        "{}/alerts/{}/close?identifierType=alias",
        api_url,
        dedup_string(&notification.dedup_key)
    )
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActiveMute {
    pub node: NodeId,
    pub check: Option<CheckId>,
}

impl ActiveMute {
    /// Build from a stored mute row (node uuid + optional check name).
    #[must_use]
    pub fn new(node: Uuid, check_name: Option<&str>) -> Self {
        let node = NodeId::from(node);
        Self {
            node,
            check: check_name.map(|name| check_id(node, name)),
        }
    }
}

/// Whether an alert is covered by any active mute (separate fn for unit testing).
#[must_use]
fn mute_matches(mutes: &[ActiveMute], alert: &Alert) -> bool {
    mutes
        .iter()
        .any(|m| m.node == alert.node && m.check.is_none_or(|c| c == alert.check))
}

/// The live routing snapshot: the always-on env default route, the DB-configured channels
/// (each with its own dedup+retry dispatcher), and the rules that select channels per alert.
struct Routes {
    /// Env-configured channels (`YAGRA_WEBHOOK_URL`/`YAGRA_SMTP_*`) — fire for *every* alert,
    /// preserving the pre-routing behaviour. `None` if no env channel is set.
    default: Option<Dispatcher<MultiChannel>>,
    /// DB channels by id, each with its own dedup state (preserved across config refresh).
    channels: HashMap<Uuid, Dispatcher<Arc<dyn NotifyChannel>>>,
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
                rules: Vec::new(),
                mutes: Vec::new(),
            }),
        }
    }

    /// Replace the DB routing snapshot. Channels that still exist keep their dispatcher (so the
    /// periodic refresh doesn't reset dedup and re-page active alerts); new channels get a
    /// fresh dispatcher; removed channels are dropped. Config for an existing channel is treated
    /// as immutable (changing it means delete + recreate).
    pub async fn set_routing(&self, channels: Vec<OpenChannel>, rules: Vec<RoutingRule>) {
        let mut routes = self.routes.lock().await;
        let mut old = std::mem::take(&mut routes.channels);
        let mut next = HashMap::new();
        for ch in channels {
            if let Some(disp) = old.remove(&ch.id) {
                next.insert(ch.id, disp); // preserve dedup
            } else if let Some(channel) = build_channel(&ch.config) {
                next.insert(ch.id, Dispatcher::new(channel, RetryPolicy::default()));
            }
        }
        routes.channels = next;
        routes.rules = rules;
    }

    /// Replace the unexpired-mute snapshot (refreshed alongside routing).
    pub async fn set_mutes(&self, mutes: Vec<ActiveMute>) {
        self.routes.lock().await.mutes = mutes;
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
        let mut routes = self.routes.lock().await;
        match action {
            NotifyAction::Fire(alert) => {
                // Suppressed downstream alert: it's attributed to an upstream root cause and
                // rolled into that incident, so we don't page for it separately (the root
                // cause's own alert — root_cause: None — is what notifies). It still fired
                // for the UI/history; only the duplicate notification is suppressed.
                if let Some(root) = alert.root_cause {
                    tracing::debug!(node = %alert.node, %root, "suppressing downstream alert notification (rolled up under root cause)");
                    return;
                }
                // Muted: the operator asked for silence on this node/check until the mute
                // expires. The alert itself stays live in the UI/history.
                if mute_matches(&routes.mutes, &alert) {
                    tracing::debug!(node = %alert.node, "suppressing muted alert notification");
                    return;
                }
                let summary = format!("node {} is {}", alert.node, alert.state);
                let payload = serde_json::to_string(&alert).unwrap_or_else(|_| "{}".to_owned());
                let notification = Notification::for_alert(&alert, summary, payload);

                // Channels selected by the routing rules (severity match; None = any).
                let matched: BTreeSet<Uuid> = routes
                    .rules
                    .iter()
                    .filter(|r| r.enabled && rule_matches_severity(r.severity, alert.severity))
                    .flat_map(|r| r.channel_ids.iter().copied())
                    .collect();

                if let Some(d) = routes.default.as_mut() {
                    let outcome = d.dispatch(notification.clone()).await;
                    tracing::info!(?outcome, node = %alert.node, route = "default", "alert notification dispatched");
                }
                for id in matched {
                    if let Some(d) = routes.channels.get_mut(&id) {
                        let outcome = d.dispatch(notification.clone()).await;
                        tracing::info!(?outcome, node = %alert.node, channel = %id, "alert notification dispatched");
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
                let summary = format!("resolved: node {} recovered", alert.node);
                let payload = serde_json::to_string(&alert).unwrap_or_else(|_| "{}".to_owned());
                let notification = Notification::for_alert(&alert, summary, payload);

                let matched: BTreeSet<Uuid> = routes
                    .rules
                    .iter()
                    .filter(|r| r.enabled && rule_matches_severity(r.severity, alert.severity))
                    .flat_map(|r| r.channel_ids.iter().copied())
                    .collect();

                if let Some(d) = routes.default.as_mut() {
                    let outcome = d.dispatch_resolve(notification.clone()).await;
                    tracing::info!(?outcome, node = %alert.node, route = "default", "alert resolve dispatched");
                }
                let ids: Vec<Uuid> = routes.channels.keys().copied().collect();
                for id in ids {
                    if let Some(d) = routes.channels.get_mut(&id) {
                        if matched.contains(&id) {
                            let outcome = d.dispatch_resolve(notification.clone()).await;
                            tracing::info!(?outcome, node = %alert.node, channel = %id, "alert resolve dispatched");
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
                let summary = format!("rolled up: node {} suppressed under upstream", alert.node);
                let payload = serde_json::to_string(&alert).unwrap_or_else(|_| "{}".to_owned());
                let notification = Notification::for_alert(&alert, summary, payload);

                let matched: BTreeSet<Uuid> = routes
                    .rules
                    .iter()
                    .filter(|r| r.enabled && rule_matches_severity(r.severity, alert.severity))
                    .flat_map(|r| r.channel_ids.iter().copied())
                    .collect();

                if let Some(d) = routes.default.as_mut() {
                    let outcome = d.dispatch_resolve(notification.clone()).await;
                    tracing::info!(?outcome, node = %alert.node, route = "default", "downstream alert rolled up (incident closed)");
                }
                let ids: Vec<Uuid> = routes.channels.keys().copied().collect();
                for id in ids {
                    if let Some(d) = routes.channels.get_mut(&id) {
                        if matched.contains(&id) {
                            let outcome = d.dispatch_resolve(notification.clone()).await;
                            tracing::info!(?outcome, node = %alert.node, channel = %id, "downstream alert rolled up (incident closed)");
                        } else {
                            d.mark_resolved(&key);
                        }
                    }
                }
            }
        }
    }
}

/// Match a routing rule's severity against an alert's (separate fn for unit testing).
#[must_use]
fn rule_matches_severity(rule_severity: Option<Severity>, alert_severity: Severity) -> bool {
    rule_severity.is_none_or(|s| s == alert_severity)
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;
    use yagra_common::NodeId;

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
            node: NodeId::from(Uuid::nil()),
            check: yagra_common::CheckId::from(Uuid::nil()),
            severity,
            state: NodeState::Critical,
            at_unix_ms: 1,
            root_cause: None,
            flapping: false,
            metric: "event:test".to_owned(),
            breach: None,
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

    #[test]
    fn fires_after_dwell_then_resolves_on_recovery() {
        let mgr = AlertManager::new();
        let node = NodeId::new();

        // Unreachable must persist DWELL_SAMPLES times before it commits/fires.
        for i in 0..(DWELL_SAMPLES - 1) {
            let actions = mgr.observe(&result(node, CheckOutcome::Unreachable, i64::from(i)));
            assert!(actions.is_empty(), "should not fire before dwell satisfied");
        }
        let actions = mgr.observe(&result(node, CheckOutcome::Unreachable, 100));
        assert!(matches!(actions.as_slice(), [NotifyAction::Fire(_)]));
        assert_eq!(mgr.active_alerts().len(), 1);

        // Recovery is symmetric: it also needs DWELL_SAMPLES consecutive reachable
        // samples before the alert resolves (anti-flap on the way back too).
        for i in 0..(DWELL_SAMPLES - 1) {
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
        let mgr = AlertManager::new();
        let node = NodeId::new();
        let mut rx = mgr.subscribe_node_states();

        // First observation commits Ok and emits the initial node-state event.
        mgr.observe(&result(node, CheckOutcome::Reachable, 0));
        let ev = rx.try_recv().expect("first observe emits state");
        assert!(ev.contains("\"ok\""), "state ok in payload: {ev}");
        assert!(
            ev.contains(&node.as_uuid().to_string()),
            "carries node id: {ev}"
        );

        // Steady Ok: no state change ⇒ no further events.
        mgr.observe(&result(node, CheckOutcome::Reachable, 1));
        assert!(rx.try_recv().is_err(), "steady Ok must not emit");

        // Drive Unreachable up to the dwell threshold; only the committing observe changes state.
        for i in 0..(DWELL_SAMPLES - 1) {
            mgr.observe(&result(node, CheckOutcome::Unreachable, 10 + i64::from(i)));
            assert!(rx.try_recv().is_err(), "pre-dwell must not emit");
        }
        mgr.observe(&result(node, CheckOutcome::Unreachable, 100));
        let ev = rx.try_recv().expect("dwell-crossing observe emits");
        assert!(ev.contains("\"unreachable\""), "state unreachable: {ev}");
        assert!(rx.try_recv().is_err(), "exactly one event per real change");
    }

    #[test]
    fn steady_reachable_never_fires() {
        let mgr = AlertManager::new();
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
        use yagra_common::{Direction, ThresholdRule};

        let node = NodeId::new();
        let mgr = AlertManager::new();
        // Node-scoped threshold: icmp_rtt_ms critical at/above 100ms, no dwell.
        let mut meta = HashMap::new();
        meta.insert(node, NodeMeta::default());
        mgr.set_config(AlertConfig::new(
            vec![StoredThreshold {
                id: Uuid::nil(),
                level: ScopeLevel::Node,
                scope_id: node.to_string(),
                rule: ThresholdRule {
                    metric: "icmp_rtt_ms".into(),
                    direction: Direction::Above,
                    warning: Some(50.0),
                    critical: Some(100.0),
                    dwell_samples: 1,
                },
            }],
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

    #[test]
    fn counter_sample_never_fires_and_drains_a_latched_alert() {
        use yagra_bus::Sample;
        use yagra_common::{Direction, ThresholdRule};

        let node = NodeId::new();
        let mgr = AlertManager::new();
        // A rule that predates the create-side counter rejection: octets "above 1000".
        let mut meta = HashMap::new();
        meta.insert(node, NodeMeta::default());
        mgr.set_config(AlertConfig::new(
            vec![StoredThreshold {
                id: Uuid::nil(),
                level: ScopeLevel::Node,
                scope_id: node.to_string(),
                rule: ThresholdRule {
                    metric: "if_hc_in_octets".into(),
                    direction: Direction::Above,
                    warning: None,
                    critical: Some(1000.0),
                    dwell_samples: 1,
                },
            }],
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
        let mgr = AlertManager::new();
        let mut meta = HashMap::new();
        meta.insert(node, NodeMeta::default());
        mgr.set_config(AlertConfig::new(
            vec![StoredThreshold {
                id: Uuid::nil(),
                level: ScopeLevel::Node,
                scope_id: node.to_string(),
                rule: ThresholdRule {
                    metric: "icmp_rtt_ms".into(),
                    direction: Direction::Above,
                    warning: Some(50.0),
                    critical: Some(100.0),
                    dwell_samples: 1,
                },
            }],
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
        let mgr = AlertManager::new();
        // Drive unreachable past the dwell so liveness commits and fires.
        let mut fired = None;
        for i in 0..=i64::from(DWELL_SAMPLES) {
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
        let mgr = AlertManager::new();
        let mut r = result(node, CheckOutcome::Reachable, 0);
        r.samples = vec![yagra_bus::Sample::gauge("icmp_rtt_ms", 9999.0)];
        // No thresholds configured ⇒ no metric alert (and reachable ⇒ no liveness alert).
        assert!(mgr.observe(&r).is_empty());
    }

    #[test]
    fn node_states_reflect_liveness_and_threshold_rollup() {
        use yagra_bus::Sample;
        use yagra_common::{Direction, ThresholdRule};

        let mgr = AlertManager::new();
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
        mgr.set_config(AlertConfig::new(
            vec![StoredThreshold {
                id: Uuid::nil(),
                level: ScopeLevel::Node,
                scope_id: node.to_string(),
                rule: ThresholdRule {
                    metric: "icmp_rtt_ms".into(),
                    direction: Direction::Above,
                    warning: Some(50.0),
                    critical: Some(100.0),
                    dwell_samples: 1,
                },
            }],
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
        let mgr = AlertManager::new();
        let up = NodeId::new();
        let down = NodeId::new();
        for i in 0..DWELL_SAMPLES {
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
        let mgr = AlertManager::new();
        let node = NodeId::new();

        // Drive the node down until its liveness alert commits.
        let mut fired = false;
        for i in 0..DWELL_SAMPLES {
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
        mgr.set_config(AlertConfig::default().with_maintenance(maintenance));
        let mut resolved = false;
        for i in 0..DWELL_SAMPLES {
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
        mgr.set_config(AlertConfig::default());
        let mut refired = false;
        for i in 0..DWELL_SAMPLES {
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
        use yagra_common::{Direction, ThresholdRule};

        let node = NodeId::new();
        let mut meta = HashMap::new();
        meta.insert(node, NodeMeta::default());
        let thresholds = vec![StoredThreshold {
            id: Uuid::nil(),
            level: ScopeLevel::Node,
            scope_id: node.to_string(),
            rule: ThresholdRule {
                metric: "icmp_rtt_ms".into(),
                direction: Direction::Above,
                warning: Some(50.0),
                critical: Some(100.0),
                dwell_samples: 1,
            },
        }];

        let mgr = AlertManager::new();
        let mut maintenance = BTreeSet::new();
        maintenance.insert(node);
        mgr.set_config(AlertConfig::new(thresholds, meta).with_maintenance(maintenance));

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
            node,
            check: check_id(node, "icmp_rtt_ms"),
            severity: Severity::Critical,
            state: NodeState::Critical,
            at_unix_ms: 0,
            root_cause: None,
            flapping: false,
            metric: "icmp_rtt_ms".to_string(),
            breach: None,
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

        let mgr = AlertManager::new();
        mgr.set_config(AlertConfig::default().with_topology(topo));

        // Helper: drive a node Unreachable until it commits and return the fired alert.
        let drive_down = |node: NodeId, base: i64| -> Alert {
            let mut fired = None;
            for i in 0..DWELL_SAMPLES {
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

        let mgr = AlertManager::new();
        mgr.set_config(AlertConfig::default().with_topology(topo));

        // Collect every action produced while driving `node` to `outcome` across the dwell window.
        let drive = |node: NodeId, outcome: CheckOutcome, base: i64| -> Vec<NotifyAction> {
            let mut out = Vec::new();
            for i in 0..DWELL_SAMPLES {
                out.extend(mgr.observe(&result(node, outcome, base + i64::from(i))));
            }
            out
        };

        // Child falls first, while its parent is still up ⇒ it pages standalone (no root cause).
        let child_actions = drive(child, CheckOutcome::Unreachable, 0);
        let child_fire = child_actions
            .iter()
            .find_map(|a| match a {
                NotifyAction::Fire(al) if al.node == child => Some(al.clone()),
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
                NotifyAction::Fire(al) if al.node == parent && al.root_cause.is_none()
            )),
            "parent fires as the root cause"
        );
        let suppressed = parent_actions
            .iter()
            .find_map(|a| match a {
                NotifyAction::Suppress(al) if al.node == child => Some(al.clone()),
                _ => None,
            })
            .expect("child rolled up under the parent");
        assert_eq!(suppressed.root_cause, Some(parent));

        // The child stays active (still down) but is now attributed to the parent.
        let child_active = mgr
            .active_alerts()
            .into_iter()
            .find(|a| a.node == child)
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

        let mgr = AlertManager::new();
        mgr.set_config(AlertConfig::default().with_topology(topo));

        let drive = |node: NodeId, outcome: CheckOutcome, base: i64| -> Vec<NotifyAction> {
            let mut out = Vec::new();
            for i in 0..DWELL_SAMPLES {
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
            .find(|a| a.node == child)
            .expect("child active");
        assert_eq!(child_active.root_cause, Some(parent));

        // Parent recovers while the child is still down ⇒ the child must now page standalone.
        let recovery = drive(parent, CheckOutcome::Reachable, 200);
        assert!(
            recovery.iter().any(|a| matches!(
                a,
                NotifyAction::Fire(al) if al.node == child && al.root_cause.is_none()
            )),
            "child re-pages standalone once its upstream is back"
        );
        let child_active = mgr
            .active_alerts()
            .into_iter()
            .find(|a| a.node == child)
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

        let mgr = AlertManager::new();
        mgr.set_config(AlertConfig::default().with_topology(topo));

        let drive = |node: NodeId, outcome: CheckOutcome, base: i64| {
            for i in 0..DWELL_SAMPLES {
                mgr.observe(&result(node, outcome, base + i64::from(i)));
            }
        };

        // c falls first (all upstream up) → pages standalone. Then p falls → c rolls under p.
        drive(child, CheckOutcome::Unreachable, 0);
        drive(parent, CheckOutcome::Unreachable, 100);
        let c_active = mgr
            .active_alerts()
            .into_iter()
            .find(|a| a.node == child)
            .expect("child active");
        assert_eq!(c_active.root_cause, Some(parent));

        // gp falls: the re-sweep from gp must reach the grandchild c (transitively) and re-attribute
        // it to gp — the new topmost down, unsuppressed ancestor. p rolls up too.
        drive(gp, CheckOutcome::Unreachable, 200);
        let c_active = mgr
            .active_alerts()
            .into_iter()
            .find(|a| a.node == child)
            .expect("child still active");
        assert_eq!(
            c_active.root_cause,
            Some(gp),
            "grandchild re-attributed to the grandparent via the transitive re-sweep"
        );
        let p_active = mgr
            .active_alerts()
            .into_iter()
            .find(|a| a.node == parent)
            .expect("parent still active");
        assert_eq!(p_active.root_cause, Some(gp));
    }

    #[tokio::test]
    async fn broadcast_acked_emits_upsert_for_active_alert() {
        // Fire a liveness alert, then mirror an inbound ack for it (ADR-015).
        let mgr = AlertManager::new();
        let node = NodeId::new();
        for i in 0..DWELL_SAMPLES {
            mgr.observe(&result(node, CheckOutcome::Unreachable, i64::from(i)));
        }
        let active = mgr.active_alerts();
        let alert = active.first().expect("one active alert after dwell");

        // Subscribe *after* the fire so only the ack event is observed.
        let mut rx = mgr.subscribe();
        mgr.broadcast_acked(
            alert.node.as_uuid(),
            alert.check.as_uuid(),
            alert.severity,
            Some(serde_json::json!({ "by": "pd-user", "source": "pagerduty" })),
        );

        let msg = rx.try_recv().expect("ack event broadcast");
        let v: serde_json::Value = serde_json::from_str(&msg).unwrap();
        assert_eq!(v["acked"]["by"], "pd-user");
        // No `resolved` flag ⇒ the client upserts (keeps the alert), it doesn't clear it.
        assert!(v.get("resolved").is_none());
        assert_eq!(v["node"], serde_json::to_value(node).unwrap());
    }

    #[tokio::test]
    async fn broadcast_acked_is_noop_when_alert_not_active() {
        let mgr = AlertManager::new();
        let mut rx = mgr.subscribe();
        // No matching active alert ⇒ nothing on screen to update, so no event is sent.
        mgr.broadcast_acked(
            Uuid::from_u128(1),
            Uuid::from_u128(2),
            Severity::Critical,
            None,
        );
        assert!(rx.try_recv().is_err());
    }
}
