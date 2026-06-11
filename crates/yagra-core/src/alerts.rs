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
//! Two more quality features are wired here on top of liveness:
//! - **Threshold alerting** — each poll sample with a resolved [`EffectiveThreshold`]
//!   (scope inheritance via [`AlertConfig`]) is evaluated and fed through the same
//!   hysteresis/flapping machinery as liveness.
//! - **Dependency suppression** — a node's committed liveness is tracked per node; when a
//!   node goes down and *every* upstream is also down (per the [`Topology`]), its alert is
//!   attributed to the highest down ancestor (`root_cause`) and the downstream
//!   notification is suppressed (rolled up into the parent incident, ADR-015). The alert
//!   still fires for the UI/history — only the duplicate page is suppressed.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Mutex, RwLock};

use async_trait::async_trait;
use tokio::sync::broadcast;
use uuid::Uuid;
use yagra_alert::CheckState;
use yagra_alert::{Alert, Dispatcher, Notification, NotifyChannel, NotifyError, RetryPolicy};
use yagra_bus::{CheckOutcome, PollResult};
use yagra_common::{
    resolve_effective, CheckId, EffectiveThreshold, NodeId, NodeState, ScopeLevel, ScopedThreshold,
};
use yagra_topology::Topology;

use crate::thresholds::StoredThreshold;

/// Liveness check name (distinct from any metric name).
const LIVENESS: &str = "__liveness__";
/// Consecutive samples a state must hold before it commits (anti-flap) for liveness.
const DWELL_SAMPLES: u32 = 3;
/// Flapping detection window and threshold.
const FLAP_WINDOW_MS: i64 = 600_000;
const FLAP_THRESHOLD: usize = 5;
/// SSE broadcast buffer.
const EVENT_BUFFER: usize = 256;

/// What the manager wants done about a committed transition.
#[derive(Debug, Clone)]
pub enum NotifyAction {
    /// A new problem alert fired.
    Fire(Alert),
    /// An alert recovered (carries the previously-active alert so it can be logged and its
    /// dedup state cleared).
    Resolve(Alert),
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
    thresholds: Vec<StoredThreshold>,
    node_meta: HashMap<NodeId, NodeMeta>,
    topology: Topology,
}

impl AlertConfig {
    /// Build a config from the stored thresholds and node metadata (no dependency edges;
    /// add them with [`Self::with_topology`]).
    #[must_use]
    pub fn new(thresholds: Vec<StoredThreshold>, node_meta: HashMap<NodeId, NodeMeta>) -> Self {
        Self {
            thresholds,
            node_meta,
            topology: Topology::new(),
        }
    }

    /// Attach the dependency topology used for parent-down suppression / root-cause roll-up.
    #[must_use]
    pub fn with_topology(mut self, topology: Topology) -> Self {
        self.topology = topology;
        self
    }

    /// Resolve the effective threshold for one (node, metric), honouring scope inheritance.
    fn resolve(&self, node: NodeId, metric: &str) -> Option<EffectiveThreshold> {
        let meta = self.node_meta.get(&node);
        let scoped: Vec<ScopedThreshold> = self
            .thresholds
            .iter()
            .filter(|t| t.rule.metric == metric && self.applies(t, node, meta))
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

/// Deterministic check id for a (node, check-name) pair, so the same logical check keeps a
/// stable dedup identity across restarts.
fn check_id(node: NodeId, name: &str) -> CheckId {
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
    tx: broadcast::Sender<String>,
    config: RwLock<AlertConfig>,
}

impl AlertManager {
    /// New manager with an empty config (no thresholds until [`Self::set_config`]).
    #[must_use]
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(EVENT_BUFFER);
        Self {
            states: Mutex::new(HashMap::new()),
            active: Mutex::new(HashMap::new()),
            live: Mutex::new(HashMap::new()),
            tx,
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

    /// The rolled-up display state for one node, if the engine has observed it.
    #[must_use]
    pub fn node_state(&self, node: NodeId) -> Option<NodeState> {
        self.node_states().get(&node).copied()
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
        self.live
            .lock()
            .expect("live mutex poisoned")
            .iter()
            .filter(|(_, s)| matches!(**s, NodeState::Unreachable))
            .map(|(n, _)| *n)
            .collect()
    }

    /// Feed one poll result through the engine: a liveness check from the outcome plus a
    /// threshold check per sample that has a resolved threshold. Returns notify actions for
    /// every committed transition (also broadcast to SSE subscribers here).
    pub fn observe(&self, result: &PollResult) -> Vec<NotifyAction> {
        let node = result.node_id;
        let mut actions = Vec::new();

        // Liveness from the reachability outcome.
        let raw = match result.outcome {
            CheckOutcome::Reachable => NodeState::Ok,
            CheckOutcome::Unreachable => NodeState::Unreachable,
            CheckOutcome::Error => NodeState::Unknown,
        };
        actions.extend(self.process_check(
            check_id(node, LIVENESS),
            node,
            raw,
            DWELL_SAMPLES,
            result.at_unix_ms,
            true,
        ));

        // Threshold checks per sample (only metrics with a resolved threshold alert).
        for sample in &result.samples {
            let eff = {
                let config = self.config.read().expect("config rwlock poisoned");
                config.resolve(node, &sample.metric)
            };
            if let Some(eff) = eff {
                let raw = eff.evaluate(sample.value);
                actions.extend(self.process_check(
                    check_id(node, &sample.metric),
                    node,
                    raw,
                    eff.dwell_samples,
                    result.at_unix_ms,
                    false,
                ));
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
        check: CheckId,
        node: NodeId,
        raw: NodeState,
        dwell: u32,
        at_unix_ms: i64,
        is_liveness: bool,
    ) -> Vec<NotifyAction> {
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
        // still needs to read it as `ok`).
        if is_liveness {
            self.live
                .lock()
                .expect("live mutex poisoned")
                .insert(node, committed);
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

        match t.to_alert(node, check, at_unix_ms, root_cause) {
            Some(alert) => {
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
        }
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
            "resolved": resolved,
        });
        // Fire-and-forget: no subscribers is not an error.
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
        Self {
            http: reqwest::Client::new(),
            url,
        }
    }
}

#[async_trait]
impl NotifyChannel for WebhookChannel {
    async fn deliver(&self, notification: &Notification) -> Result<(), NotifyError> {
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

/// An email [`NotifyChannel`] over SMTP (`lettre`, async + rustls).
pub struct EmailChannel {
    mailer: lettre::AsyncSmtpTransport<lettre::Tokio1Executor>,
    from: lettre::message::Mailbox,
    to: lettre::message::Mailbox,
}

impl EmailChannel {
    /// Build from env (`YAGRA_SMTP_HOST`, `_FROM`, `_TO`, optional `_PORT`/`_USER`/`_PASS`).
    /// Returns `None` if the required vars are missing or malformed.
    pub fn from_env() -> Option<Self> {
        use lettre::transport::smtp::authentication::Credentials;
        let host = std::env::var("YAGRA_SMTP_HOST")
            .ok()
            .filter(|s| !s.is_empty())?;
        let from = std::env::var("YAGRA_SMTP_FROM").ok()?.parse().ok()?;
        let to = std::env::var("YAGRA_SMTP_TO").ok()?.parse().ok()?;
        let mut builder =
            lettre::AsyncSmtpTransport::<lettre::Tokio1Executor>::relay(&host).ok()?;
        if let Ok(port) = std::env::var("YAGRA_SMTP_PORT") {
            if let Ok(port) = port.parse::<u16>() {
                builder = builder.port(port);
            }
        }
        if let (Ok(user), Ok(pass)) = (
            std::env::var("YAGRA_SMTP_USER"),
            std::env::var("YAGRA_SMTP_PASS"),
        ) {
            builder = builder.credentials(Credentials::new(user, pass));
        }
        Some(Self {
            mailer: builder.build(),
            from,
            to,
        })
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
}

/// Forwards alert lifecycle to the configured channels with the engine's dedup + retry
/// (ADR-015).
pub struct Notifier {
    dispatcher: tokio::sync::Mutex<Dispatcher<MultiChannel>>,
}

impl Notifier {
    /// Build a notifier from env: a Webhook (`YAGRA_WEBHOOK_URL`) and/or email
    /// (`YAGRA_SMTP_*`). Returns `None` if no channel is configured.
    #[must_use]
    pub fn from_env() -> Option<Self> {
        let mut channels: Vec<Box<dyn NotifyChannel>> = Vec::new();
        if let Ok(url) = std::env::var("YAGRA_WEBHOOK_URL") {
            if !url.is_empty() {
                channels.push(Box::new(WebhookChannel::new(url)));
            }
        }
        if let Some(email) = EmailChannel::from_env() {
            channels.push(Box::new(email));
        }
        if channels.is_empty() {
            return None;
        }
        tracing::info!(channels = channels.len(), "alert notifier enabled");
        Some(Self {
            dispatcher: tokio::sync::Mutex::new(Dispatcher::new(
                MultiChannel { channels },
                RetryPolicy::default(),
            )),
        })
    }

    /// Apply one notify action (deliver a fire, or clear a resolved alert's dedup state).
    pub async fn handle(&self, action: NotifyAction) {
        let mut d = self.dispatcher.lock().await;
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
                let summary = format!("node {} is {}", alert.node, alert.state);
                let payload = serde_json::to_string(&alert).unwrap_or_else(|_| "{}".to_owned());
                let outcome = d
                    .dispatch(Notification::for_alert(&alert, summary, payload))
                    .await;
                tracing::info!(?outcome, node = %alert.node, "alert notification dispatched");
            }
            NotifyAction::Resolve(alert) => d.mark_resolved(&alert.dedup_key()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;
    use yagra_common::NodeId;

    fn result(node: NodeId, outcome: CheckOutcome, at: i64) -> PollResult {
        PollResult {
            schema_version: 1,
            job_id: Uuid::nil(),
            node_id: node,
            at_unix_ms: at,
            outcome,
            samples: Vec::new(),
            interfaces: Vec::new(),
        }
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
}
