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

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use tokio::sync::broadcast;
use yagra_alert::CheckState;
use yagra_alert::{
    Alert, DedupKey, Dispatcher, Notification, NotifyChannel, NotifyError, RetryPolicy,
};
use yagra_bus::{CheckOutcome, PollResult};
use yagra_common::{CheckId, NodeState};

/// Consecutive samples a state must hold before it commits (anti-flap).
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

/// In-memory alert engine: per-node check state, active alerts, and an SSE broadcast.
pub struct AlertManager {
    states: Mutex<HashMap<CheckId, CheckState>>,
    active: Mutex<HashMap<CheckId, Alert>>,
    tx: broadcast::Sender<String>,
}

impl AlertManager {
    /// New manager.
    #[must_use]
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(EVENT_BUFFER);
        Self {
            states: Mutex::new(HashMap::new()),
            active: Mutex::new(HashMap::new()),
            tx,
        }
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

    /// Feed one poll result through the state machine. Returns the notify actions for any
    /// committed transition (also broadcast to SSE subscribers here).
    pub fn observe(&self, result: &PollResult) -> Vec<NotifyAction> {
        let raw = match result.outcome {
            CheckOutcome::Reachable => NodeState::Ok,
            CheckOutcome::Unreachable => NodeState::Unreachable,
            CheckOutcome::Error => NodeState::Unknown,
        };
        // One liveness check per node; its id is derived from the node id (stable dedup).
        let check = CheckId::from(result.node_id.as_uuid());

        let transition = {
            let mut states = self.states.lock().expect("states mutex poisoned");
            let cs = states.entry(check).or_insert_with(|| {
                CheckState::new(NodeState::Ok, DWELL_SAMPLES, FLAP_WINDOW_MS, FLAP_THRESHOLD)
            });
            cs.observe(raw, result.at_unix_ms)
        };
        let Some(t) = transition else {
            return Vec::new();
        };

        match t.to_alert(result.node_id, check, result.at_unix_ms, None) {
            Some(alert) => {
                self.active
                    .lock()
                    .expect("alerts mutex poisoned")
                    .insert(check, alert.clone());
                self.broadcast(&alert, false);
                vec![NotifyAction::Fire(alert)]
            }
            None => {
                // Recovered (Ok/Unknown) — resolve any active alert for this check.
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
}
