//! Alert engine wiring (Workstream B).
//!
//! Drives the tested [`yagra_alert`] state machine from live poll results: each node has
//! one ICMP-liveness check whose raw state (reachable→ok, unreachable→unreachable,
//! error→unknown) is fed through dwell-time **hysteresis** + **flapping** detection
//! ([`CheckState`]). A committed transition into a problem state fires an [`Alert`];
//! recovery resolves it. Active alerts are held in memory and broadcast to SSE
//! subscribers; transitions are forwarded to a [`Notifier`] (Webhook) with the engine's
//! dedup + retry. Threshold-based alerting and persistence land in a later pass — this
//! MVP covers reachability, which needs no threshold table.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Mutex, RwLock};

use async_trait::async_trait;
use tokio::sync::broadcast;
use uuid::Uuid;
use yagra_alert::CheckState;
use yagra_alert::{
    Alert, DedupKey, Dispatcher, Notification, NotifyChannel, NotifyError, RetryPolicy,
};
use yagra_bus::{CheckOutcome, PollResult};
use yagra_common::{
    resolve_effective, CheckId, EffectiveThreshold, NodeId, NodeState, ScopeLevel, ScopedThreshold,
};

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
    /// An alert recovered; stop deduping it so the next occurrence notifies again.
    Resolve(DedupKey),
}

/// Per-node metadata used to resolve threshold scope (profile + groups).
#[derive(Debug, Clone, Default)]
pub struct NodeMeta {
    /// Profile id (as text) the node belongs to, if any.
    pub profile: Option<String>,
    /// Group identifiers (node tag values) for group-scoped thresholds.
    pub groups: BTreeSet<String>,
}

/// A snapshot of thresholds + node metadata the engine evaluates against. Rebuilt
/// periodically from the database so threshold edits take effect without a restart.
#[derive(Debug, Clone, Default)]
pub struct AlertConfig {
    thresholds: Vec<StoredThreshold>,
    node_meta: HashMap<NodeId, NodeMeta>,
}

impl AlertConfig {
    /// Build a config from the stored thresholds and node metadata.
    #[must_use]
    pub fn new(thresholds: Vec<StoredThreshold>, node_meta: HashMap<NodeId, NodeMeta>) -> Self {
        Self {
            thresholds,
            node_meta,
        }
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

/// In-memory alert engine: per-check state, active alerts, an SSE broadcast, and the
/// threshold/metadata config snapshot.
pub struct AlertManager {
    states: Mutex<HashMap<CheckId, CheckState>>,
    active: Mutex<HashMap<CheckId, Alert>>,
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
                ));
            }
        }
        actions
    }

    /// Run one raw state through a check's hysteresis and emit a fire/resolve action on a
    /// committed transition.
    fn process_check(
        &self,
        check: CheckId,
        node: NodeId,
        raw: NodeState,
        dwell: u32,
        at_unix_ms: i64,
    ) -> Vec<NotifyAction> {
        let transition = {
            let mut states = self.states.lock().expect("states mutex poisoned");
            let cs = states.entry(check).or_insert_with(|| {
                CheckState::new(NodeState::Ok, dwell.max(1), FLAP_WINDOW_MS, FLAP_THRESHOLD)
            });
            cs.observe(raw, at_unix_ms)
        };
        let Some(t) = transition else {
            return Vec::new();
        };

        match t.to_alert(node, check, at_unix_ms, None) {
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
                        vec![NotifyAction::Resolve(alert.dedup_key())]
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

/// Forwards alert lifecycle to a channel with the engine's dedup + retry (ADR-015).
pub struct Notifier {
    dispatcher: tokio::sync::Mutex<Dispatcher<WebhookChannel>>,
}

impl Notifier {
    /// A notifier delivering to `url` over a Webhook channel.
    #[must_use]
    pub fn webhook(url: String) -> Self {
        Self {
            dispatcher: tokio::sync::Mutex::new(Dispatcher::new(
                WebhookChannel::new(url),
                RetryPolicy::default(),
            )),
        }
    }

    /// Apply one notify action (deliver a fire, or clear a resolved alert's dedup state).
    pub async fn handle(&self, action: NotifyAction) {
        let mut d = self.dispatcher.lock().await;
        match action {
            NotifyAction::Fire(alert) => {
                let summary = format!("node {} is {}", alert.node, alert.state);
                let payload = serde_json::to_string(&alert).unwrap_or_else(|_| "{}".to_owned());
                let outcome = d
                    .dispatch(Notification::for_alert(&alert, summary, payload))
                    .await;
                tracing::info!(?outcome, node = %alert.node, "alert notification dispatched");
            }
            NotifyAction::Resolve(key) => d.mark_resolved(&key),
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
}
