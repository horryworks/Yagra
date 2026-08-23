// SPDX-License-Identifier: AGPL-3.0-only
//! **The live matcher** (ADR-095): burst dedup → node correlation → rule matching → planning →
//! dispatch, plus the in-memory lifecycle state event alerts have instead of dwell.
//!
//! [`EventEngine::plan`] is deliberately pure given a snapshot: it takes a message and returns the
//! actions it implies, so the ten planner tests can assert on outcomes without a database, a bus or
//! a clock. Everything that touches the outside world is behind `dispatch_action`, which is the
//! seam ADR-092 put on the alert sink.
//!
//! ⚠️ No SQL belongs in this file — see [`super::repo`]'s doc for what enforces that.

use std::borrow::Cow;
use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, VecDeque};
use std::hash::{Hash, Hasher};
use std::net::IpAddr;
use std::sync::{Arc, Mutex, RwLock};

use uuid::Uuid;
use yagra_alert::Alert;
use yagra_bus::{EventKind, EventMsg};
use yagra_common::{trap_oid_name, CheckId, NodeId, NodeState, Severity};

use crate::alerts::{check_id, AlertManager, NotifyAction};
use crate::repo::NodeRepo;

// The vocabulary lives in the parent, which a child can see without any widening — see
// `super`'s doc for why that is what decides where a thing goes here.
use crate::alerts::sink::AlertSink;

use super::rules::compile_rule;
use super::*;

// ─── Engine ────────────────────────────────────────────────────────────────────────

/// The compiled-rules + address-map snapshot (refreshed by the 30s loop and inline
/// after rule/source edits).
#[derive(Default)]
struct Snapshot {
    rules: Vec<CompiledRule>,
    addresses: HashMap<IpAddr, Uuid>,
}

/// An active (raised, not yet resolved) event alert — tracks the TTL deadline so the
/// sweeper can expire it. Keyed by the alert's check id in [`Runtime::active`].
struct ActiveEvent {
    expires_at_ms: i64,
}

/// Mutable matching state. Never held across an await.
struct Runtime {
    /// Rolling match timestamps per (rule, node) for the min-count/window gate.
    counters: HashMap<(Uuid, Uuid), VecDeque<i64>>,
    /// Active event alerts by check id, with their TTL deadline.
    active: HashMap<CheckId, ActiveEvent>,
    /// Burst-dedup window: content-hash → last-seen ms.
    dedup_seen: HashMap<u64, i64>,
    dedup_order: VecDeque<u64>,
}

impl Runtime {
    fn new() -> Self {
        Self {
            counters: HashMap::new(),
            active: HashMap::new(),
            dedup_seen: HashMap::new(),
            dedup_order: VecDeque::new(),
        }
    }

    /// Whether this event is an identical repeat inside the dedup window.
    fn is_duplicate(&mut self, key: u64, now_ms: i64) -> bool {
        while self.dedup_order.len() >= DEDUP_CAP {
            if let Some(old) = self.dedup_order.pop_front() {
                self.dedup_seen.remove(&old);
            }
        }
        match self.dedup_seen.get(&key) {
            Some(&seen) if now_ms - seen < DEDUP_WINDOW_MS => true,
            _ => {
                if self.dedup_seen.insert(key, now_ms).is_none() {
                    self.dedup_order.push_back(key);
                }
                false
            }
        }
    }

    /// min-count/window gate: record a match and report whether the rule may fire.
    fn gate_passes(&mut self, rule: &CompiledRule, node: Uuid, now_ms: i64) -> bool {
        if rule.min_count <= 1 {
            return true;
        }
        let dq = self.counters.entry((rule.id, node)).or_default();
        let cutoff = now_ms - i64::from(rule.window_secs) * 1000;
        while dq.front().is_some_and(|&t| t < cutoff) {
            dq.pop_front();
        }
        dq.push_back(now_ms);
        dq.len() >= rule.min_count as usize
    }
}

/// What the planning step decided. The manager-side raise/resolve is done **inside**
/// `plan` under the runtime lock (atomically with `Runtime::active`, so the two active
/// sets can never diverge — see the sweep/plan race guard); `actions` carries the
/// resulting [`NotifyAction`]s so the async step only does the I/O (history + notify),
/// each tagged with the metric reason for a resolve ("clear"; ignored for a fire).
#[derive(Default)]
struct Planned {
    row_action: EventAction,
    matched_rule: Option<Uuid>,
    actions: Vec<(NotifyAction, &'static str)>,
}

/// Row-action precedence: the strongest outcome describes the event.
/// The webhook source binding the ingest endpoint resolved (token already verified).
pub struct SourceBinding {
    pub source_id: Uuid,
    pub node_id: Option<Uuid>,
}

/// Webhook ingest rate limit per source (requests/second; burst 2×).
const INGEST_RATE_PER_SOURCE: f64 = 10.0;

/// Orchestrates the event pipeline. Sync matching state under short-lived locks; all
/// I/O (DB, history, notifications) happens after the locks are released.
pub struct EventEngine {
    repo: Arc<EventRepo>,
    alerts: Arc<AlertManager>,
    /// Where a planned transition goes when it runs inline: the History row and the delivery, as
    /// one step. Holding this rather than a `Notifier` and an `AlertHistoryStore` is what makes
    /// "notified but not recorded" unwritable here (ADR-092).
    sink: Arc<dyn AlertSink>,
    snapshot: RwLock<Snapshot>,
    runtime: Mutex<Runtime>,
    /// Ingest token buckets per (already-verified) webhook source: (tokens, last-refill ms).
    ingest_rate: Mutex<HashMap<Uuid, (f64, i64)>>,
    /// Non-blocking handoff to the async batch persist writer (ADR-024). `None` in unit tests that
    /// exercise the pure planner only.
    persist_tx: Option<tokio::sync::mpsc::Sender<PersistRecord>>,
    /// Blocking handoff to the async action writer (S10): alert-history + notification I/O for
    /// planned fire/resolve actions runs off the matcher's hot path. `None` falls back to inline
    /// execution (unit tests / skeleton) so behavior is unchanged there.
    action_tx: Option<tokio::sync::mpsc::Sender<QueuedAction>>,
}

impl EventEngine {
    #[must_use]
    pub fn new(
        repo: Arc<EventRepo>,
        alerts: Arc<AlertManager>,
        sink: Arc<dyn AlertSink>,
        persist_tx: Option<tokio::sync::mpsc::Sender<PersistRecord>>,
        action_tx: Option<tokio::sync::mpsc::Sender<QueuedAction>>,
    ) -> Self {
        Self {
            repo,
            alerts,
            sink,
            snapshot: RwLock::new(Snapshot::default()),
            runtime: Mutex::new(Runtime::new()),
            ingest_rate: Mutex::new(HashMap::new()),
            persist_tx,
            action_tx,
        }
    }

    /// Webhook ingest rate gate (per verified source, so the key space is bounded by
    /// operator-created sources). Called by the ingest endpoint before any DB write.
    #[must_use]
    pub fn ingest_allowed(&self, source_id: Uuid) -> bool {
        let now_ms = now_unix_ms();
        let burst = INGEST_RATE_PER_SOURCE * 2.0;
        let mut buckets = self.ingest_rate.lock().expect("ingest mutex poisoned");
        let (tokens, last) = buckets.entry(source_id).or_insert((burst, now_ms));
        let elapsed_ms = (now_ms - *last).max(0) as f64;
        *tokens = (*tokens + INGEST_RATE_PER_SOURCE * elapsed_ms / 1000.0).min(burst);
        *last = now_ms;
        if *tokens >= 1.0 {
            *tokens -= 1.0;
            true
        } else {
            false
        }
    }

    /// Reload the rules + address-map snapshot. Keeps the previous snapshot parts on a
    /// load failure (never downgrades to empty because the DB blinked).
    pub async fn reload(&self, nodes: &NodeRepo) {
        let rules = match self.repo.list_rules().await {
            Ok(stored) => {
                let compiled: Vec<CompiledRule> = stored.iter().filter_map(compile_rule).collect();
                let skipped = stored.iter().filter(|r| r.enabled).count() - compiled.len();
                if skipped > 0 {
                    tracing::warn!(skipped, "event rules failed to compile and were skipped");
                }
                Some(compiled)
            }
            Err(e) => {
                tracing::warn!(error = %e, "failed to load event rules; keeping previous snapshot");
                None
            }
        };
        let addresses = match nodes.address_map().await {
            Ok(map) => Some(map),
            Err(e) => {
                tracing::warn!(error = %e, "failed to load node address map; keeping previous snapshot");
                None
            }
        };
        let mut snap = self.snapshot.write().expect("snapshot rwlock poisoned");
        if let Some(rules) = rules {
            snap.rules = rules;
        }
        if let Some(addresses) = addresses {
            snap.addresses = addresses;
        }
    }

    /// Feed one event through the pipeline (bus consumer passes `source: None`;
    /// the webhook ingest endpoint passes its verified source binding).
    pub async fn handle_event(&self, msg: EventMsg, source: Option<SourceBinding>) {
        metrics::counter!("yagra_events_ingested_total", "kind" => msg.kind.as_str()).increment(1);
        let now_ms = now_unix_ms();

        // Burst dedup: identical (kind, origin, message) within the window.
        let dedup_key = {
            let mut h = DefaultHasher::new();
            msg.kind.as_str().hash(&mut h);
            msg.source_ip.hash(&mut h);
            source.as_ref().map(|s| s.source_id).hash(&mut h);
            msg.message.hash(&mut h);
            h.finish()
        };
        {
            let mut runtime = self.runtime.lock().expect("runtime mutex poisoned");
            if runtime.is_duplicate(dedup_key, now_ms) {
                metrics::counter!("yagra_events_deduped_total").increment(1);
                return;
            }
        }

        // Correlate: webhook source binding wins, else source-IP → inventory.
        let node_id: Option<Uuid> = source.as_ref().and_then(|s| s.node_id).or_else(|| {
            let snap = self.snapshot.read().expect("snapshot rwlock poisoned");
            msg.source_ip
                .and_then(|ip| snap.addresses.get(&ip).copied())
        });
        if node_id.is_none() {
            metrics::counter!("yagra_events_unmatched_node_total").increment(1);
        }

        // Plan under short locks, then execute the I/O.
        let planned = match node_id {
            Some(node) => self.plan(&msg, source.as_ref().map(|s| s.source_id), node, now_ms),
            None => Planned::default(),
        };

        // Hand the raise/resolve side effects (history write + notification) to the async action
        // writer so the matcher isn't blocked on DB round-trips / vendor delivery under an event
        // storm (S10). The in-memory alert state already advanced under the plan lock; only the I/O
        // is deferred, in FIFO order, never dropped.
        for (action, reason) in planned.actions {
            self.dispatch_action(action, reason).await;
        }
        self.update_active_gauge();

        // Hand the event to the async batch writer for best-effort persistence (search/forensics,
        // ADR-024). Non-blocking: under sustained overload we shed the newest event rather than
        // block the matcher — alerts already fired above, so a dropped persist never loses an alert.
        if let Some(tx) = &self.persist_tx {
            let signature = signature_of(&msg);
            let record = PersistRecord {
                msg,
                node_id,
                source_id: source.as_ref().map(|s| s.source_id),
                matched_rule_id: planned.matched_rule,
                action: planned.row_action,
                signature,
            };
            match tx.try_send(record) {
                Ok(()) => metrics::counter!("yagra_events_persist_enqueued_total").increment(1),
                Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                    metrics::counter!("yagra_events_persist_dropped_total", "reason" => "channel_full")
                        .increment(1);
                }
                Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                    metrics::counter!("yagra_events_persist_dropped_total", "reason" => "closed")
                        .increment(1);
                }
            }
        }
    }

    /// The pure matching/planning step: clear pass before fire pass (a message hitting
    /// both resolves rather than flaps), all rules evaluated (no first-match-wins).
    fn plan(&self, msg: &EventMsg, source: Option<Uuid>, node: Uuid, now_ms: i64) -> Planned {
        let node_id = NodeId::from(node);
        let in_maintenance = self.alerts.in_maintenance(node_id);
        let snap = self.snapshot.read().expect("snapshot rwlock poisoned");
        let mut runtime = self.runtime.lock().expect("runtime mutex poisoned");
        let mut planned = Planned::default();
        // The strongest outcome wins when several rules match one event — a plain  now that
        // EventAction derives Ord from its declaration order.
        let bump = |planned: &mut Planned, action: EventAction, rule: Uuid| {
            if action > planned.row_action {
                planned.row_action = action;
                planned.matched_rule = Some(rule);
            }
        };

        // For SNMP traps, prepend the resolved MIB name to the text rules match against, so a
        // rule (built-in or user-authored) can match by name (`linkDown`) as well as by raw OID.
        // The stored/displayed message is untouched; matching only sees the enriched copy, and
        // resolution is centralized in core so it works regardless of poller version (N-1 safe).
        // Spelled out rather than `_ =>`: a fourth `EventKind` must decide whether it carries
        // something to resolve, instead of silently landing in the "no enrichment" arm.
        let haystack: Cow<str> = match (msg.kind, msg.trap_oid.as_deref().and_then(trap_oid_name)) {
            (EventKind::Trap, Some(name)) => Cow::Owned(format!("{name} {}", msg.message)),
            (EventKind::Trap, None) | (EventKind::Syslog | EventKind::Webhook, _) => {
                Cow::Borrowed(msg.message.as_str())
            }
        };

        for rule in snap
            .rules
            .iter()
            .filter(|r| r.applies(msg.kind, source, node))
        {
            let fire_hit = rule.matcher.matches(&haystack);
            let clear_hit = rule
                .clear_matcher
                .as_ref()
                .is_some_and(|m| m.matches(&haystack));
            if !fire_hit && !clear_hit {
                continue;
            }
            metrics::counter!("yagra_events_matched_total").increment(1);

            // Maintenance window: record the match, raise nothing (ADR-015 quality gate).
            if in_maintenance {
                bump(&mut planned, EventAction::Suppressed, rule.id);
                continue;
            }

            let check = check_id(node_id, &format!("event:{}", rule.id));

            // Clear takes precedence over fire for the same message (anti-flap). Remove
            // from BOTH active sets under the same lock so a concurrent sweep/plan can't
            // observe them diverged.
            if clear_hit {
                if runtime.active.remove(&check).is_some() {
                    if let Some(action) = self.alerts.resolve_event_alert(check) {
                        planned.actions.push((action, "clear"));
                    }
                }
                bump(&mut planned, EventAction::Cleared, rule.id);
                continue;
            }

            // Info severity = record only; the alert engine has no Info state.
            if rule.severity == Severity::Info {
                bump(&mut planned, EventAction::Info, rule.id);
                continue;
            }

            if !runtime.gate_passes(rule, node, now_ms) {
                // Counted toward the gate but below min-count — matched, not fired.
                bump(&mut planned, EventAction::Info, rule.id);
                continue;
            }

            let deadline = now_ms + i64::from(rule.ttl_secs) * 1000;
            if let Some(active) = runtime.active.get_mut(&check) {
                // Already alerting: extend the TTL, no re-notification.
                active.expires_at_ms = deadline;
                bump(&mut planned, EventAction::Refreshed, rule.id);
                continue;
            }
            // Only Warning and Critical reach here — Info is recorded and `continue`d above,
            // because the alert engine has no Info state. Listed rather than `_ => Warning` so a
            // fourth severity has to be decided here (and, if it is another non-alerting one, sent
            // down the same short-circuit) instead of silently inheriting Warning and paging
            // someone (coding-conventions: no wildcard over a domain enum).
            let state = match rule.severity {
                Severity::Critical => NodeState::Critical,
                Severity::Warning | Severity::Info => NodeState::Warning,
            };
            let alert = Alert {
                subject: yagra_alert::Subject::Node(node_id),
                check,
                severity: rule.severity,
                state,
                at_unix_ms: msg.at_unix_ms,
                root_cause: None,
                flapping: false,
                metric: format!("event:{}", rule.name),
                breach: None,
                // A passive event names its node, never a port: syslog and traps carry an
                // interface in their *text*, not as a series key we could resolve to an ifIndex.
                ifindex: None,
            };
            // Raise in the manager while holding the runtime lock, then mirror into
            // `runtime.active`. Because both sets are mutated together, the sweeper can
            // never resolve the manager alert while a fresh runtime entry lingers (which
            // would permanently suppress re-fires).
            match self.alerts.raise_event_alert(alert) {
                Some(action) => {
                    runtime.active.insert(
                        check,
                        ActiveEvent {
                            expires_at_ms: deadline,
                        },
                    );
                    planned.actions.push((action, "fire"));
                    bump(&mut planned, EventAction::Fired, rule.id);
                }
                None => {
                    // Manager already had this alert active at the same severity (its dedup
                    // fired). Keep the TTL entry consistent and treat as a refresh.
                    runtime.active.insert(
                        check,
                        ActiveEvent {
                            expires_at_ms: deadline,
                        },
                    );
                    bump(&mut planned, EventAction::Refreshed, rule.id);
                }
            }
        }
        planned
    }

    /// Execute the I/O for one planned/expired action (the manager-side active-set mutation
    /// already happened under the lock).
    ///
    /// The counters stay here and the row does not: which `alert_history` row an action produces
    /// is one rule for the whole crate (ADR-092), while `fired`/`resolved{reason}` are this
    /// pipeline's own instrumentation and mean nothing to a watch loop.
    async fn run_action(&self, action: NotifyAction, resolve_reason: &'static str) {
        match &action {
            NotifyAction::Fire(_) => {
                metrics::counter!("yagra_event_alerts_fired_total").increment(1);
            }
            NotifyAction::Resolve(_) => {
                metrics::counter!("yagra_event_alerts_resolved_total", "reason" => resolve_reason)
                    .increment(1);
            }
            // Event alerts are never dependency-suppressed (a device emitting an event is
            // demonstrably reachable), so the event pipeline never produces a roll-up. Handled
            // defensively for exhaustiveness; it is still delivered below.
            NotifyAction::Suppress(_) => {}
        }
        self.sink.dispatch(action).await;
    }

    /// Hand a planned action to the async action writer (S10). Blocking send — never drops (the
    /// alert-history audit trail must survive and the notifier needs FIFO fire→resolve order); a
    /// full queue backpressures the matcher, which is still strictly better than the old inline I/O.
    /// Falls back to inline execution when no writer is wired (unit tests / skeleton) or if the
    /// writer has already shut down, so the record/notify always lands.
    async fn dispatch_action(&self, action: NotifyAction, reason: &'static str) {
        match &self.action_tx {
            Some(tx) => {
                if let Err(err) = tx.send(QueuedAction { action, reason }).await {
                    let QueuedAction { action, reason } = err.0;
                    self.run_action(action, reason).await;
                } else {
                    metrics::counter!("yagra_event_actions_enqueued_total").increment(1);
                }
            }
            None => self.run_action(action, reason).await,
        }
    }

    /// One sweeper pass: resolve TTL-expired alerts, prune stale gate counters. The
    /// runtime removal and the manager resolve happen together under the runtime lock
    /// (so a matching event arriving mid-sweep can't leave the two active sets diverged
    /// and permanently suppress re-fires); the I/O runs after the lock is released.
    pub async fn sweep(&self, now_ms: i64) {
        let actions: Vec<(NotifyAction, &'static str)> = {
            let mut runtime = self.runtime.lock().expect("runtime mutex poisoned");
            let expired: Vec<CheckId> = runtime
                .active
                .iter()
                .filter(|(_, a)| a.expires_at_ms <= now_ms)
                .map(|(c, _)| *c)
                .collect();
            let mut actions = Vec::new();
            for check in &expired {
                runtime.active.remove(check);
                if let Some(action) = self.alerts.resolve_event_alert(*check) {
                    actions.push((action, "ttl"));
                }
            }
            // Gate counters go stale once their newest entry ages past any window.
            let counter_cutoff = now_ms - 2 * 3600 * 1000;
            runtime
                .counters
                .retain(|_, dq| dq.back().is_some_and(|&t| t >= counter_cutoff));
            actions
        };
        for (action, reason) in actions {
            self.dispatch_action(action, reason).await;
        }
        self.update_active_gauge();
    }

    /// The persistence handle (shared with the API's CRUD handlers).
    #[must_use]
    pub fn repo(&self) -> &Arc<EventRepo> {
        &self.repo
    }

    fn update_active_gauge(&self) {
        let n = {
            let runtime = self.runtime.lock().expect("runtime mutex poisoned");
            runtime.active.len()
        };
        #[allow(clippy::cast_precision_loss)]
        metrics::gauge!("yagra_event_alerts_active").set(n as f64);
    }
}

#[cfg(test)]
mod tests {
    use super::super::testkit::{stored_rule, syslog_msg, trap_msg};
    use super::*;
    use crate::alerts::Notifier;
    use crate::history::AlertHistoryStore;

    #[test]
    fn min_count_window_gate() {
        let mut runtime = Runtime::new();
        let mut stored = stored_rule("x", "critical");
        stored.min_count = 3;
        stored.window_secs = 10;
        let rule = compile_rule(&stored).unwrap();
        let node = Uuid::new_v4();

        assert!(!runtime.gate_passes(&rule, node, 0));
        assert!(!runtime.gate_passes(&rule, node, 1_000));
        assert!(runtime.gate_passes(&rule, node, 2_000)); // 3rd inside 10s
                                                          // Outside the window the count restarts.
        assert!(!runtime.gate_passes(&rule, node, 60_000));
        // Another node's count is independent.
        assert!(!runtime.gate_passes(&rule, Uuid::new_v4(), 2_000));
    }

    #[test]
    fn burst_dedup_window() {
        let mut runtime = Runtime::new();
        assert!(!runtime.is_duplicate(42, 0));
        assert!(runtime.is_duplicate(42, 1_000)); // same key inside 5s
        assert!(!runtime.is_duplicate(43, 1_000)); // different key
        assert!(!runtime.is_duplicate(42, 10_000)); // window elapsed
    }

    fn engine_for_plan() -> EventEngine {
        // The repo/history are never touched by plan(); connect_lazy gives us handles
        // without a live database.
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://localhost/unused")
            .expect("lazy pool");
        EventEngine::new(
            Arc::new(EventRepo::new(pool.clone())),
            Arc::new(AlertManager::new()),
            Arc::new(crate::alerts::sink::RecordingSink::new(
                Arc::new(AlertHistoryStore::new(pool)),
                Arc::new(Notifier::from_env()),
                "an event alert",
            )),
            None,
            None,
        )
    }

    fn set_rules(engine: &EventEngine, rules: Vec<CompiledRule>) {
        engine.snapshot.write().unwrap().rules = rules;
    }

    fn fires(p: &Planned) -> Vec<&Alert> {
        p.actions
            .iter()
            .filter_map(|(a, _)| match a {
                NotifyAction::Fire(alert) => Some(alert),
                NotifyAction::Resolve(_) | NotifyAction::Suppress(_) => None,
            })
            .collect()
    }

    fn resolves(p: &Planned) -> usize {
        p.actions
            .iter()
            .filter(|(a, _)| matches!(a, NotifyAction::Resolve(_)))
            .count()
    }

    #[tokio::test]
    async fn plan_fires_then_refreshes_then_clears() {
        let engine = engine_for_plan();
        let node = Uuid::new_v4();
        let mut stored = stored_rule("link down", "critical");
        stored.clear_pattern = Some("link up".into());
        let rule_id = stored.id;
        set_rules(&engine, vec![compile_rule(&stored).unwrap()]);

        // First match fires (and the manager now holds the active alert).
        let p = engine.plan(&syslog_msg("link down on ge-0/0/1"), None, node, 1_000);
        assert_eq!(p.row_action, EventAction::Fired);
        assert_eq!(p.matched_rule, Some(rule_id));
        let f = fires(&p);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].severity, Severity::Critical);
        assert_eq!(f[0].state, NodeState::Critical);
        assert!(f[0].root_cause.is_none());
        assert_eq!(f[0].metric, "event:test rule");
        assert_eq!(engine.alerts.active_alerts().len(), 1);

        // Repeat match refreshes (extends TTL), no second fire.
        let p = engine.plan(&syslog_msg("link down on ge-0/0/1"), None, node, 2_000);
        assert_eq!(p.row_action, EventAction::Refreshed);
        assert!(fires(&p).is_empty());

        // Clear pattern resolves in both active sets.
        let p = engine.plan(&syslog_msg("link up on ge-0/0/1"), None, node, 3_000);
        assert_eq!(p.row_action, EventAction::Cleared);
        assert_eq!(resolves(&p), 1);
        assert!(engine.alerts.active_alerts().is_empty());

        // After the clear, a new match fires again.
        let p = engine.plan(&syslog_msg("link down on ge-0/0/1"), None, node, 4_000);
        assert_eq!(p.row_action, EventAction::Fired);
        assert_eq!(fires(&p).len(), 1);
    }

    #[tokio::test]
    async fn plan_matches_trap_by_resolved_name() {
        let engine = engine_for_plan();
        let node = Uuid::new_v4();
        // A built-in-style rule matches the MIB *name*, not the raw OID (source-scoped to traps).
        let mut stored = stored_rule("linkDown", "warning");
        stored.source_kind = Some("trap".into());
        stored.clear_pattern = Some("linkUp".into());
        set_rules(&engine, vec![compile_rule(&stored).unwrap()]);

        // The wire message is the raw OID — "linkDown" is never in it — yet the rule fires,
        // because core enriches the match text with the resolved name (yagra_common::trap_oid_name).
        let down = trap_msg("1.3.6.1.6.3.1.1.5.3");
        assert!(!down.message.contains("linkDown"));
        let p = engine.plan(&down, None, node, 1_000);
        assert_eq!(p.row_action, EventAction::Fired);
        assert_eq!(fires(&p).len(), 1);
        assert_eq!(engine.alerts.active_alerts().len(), 1);

        // The linkUp trap (a different OID) resolves via the clear pattern's resolved name.
        let p = engine.plan(&trap_msg("1.3.6.1.6.3.1.1.5.4"), None, node, 2_000);
        assert_eq!(p.row_action, EventAction::Cleared);
        assert_eq!(resolves(&p), 1);
        assert!(engine.alerts.active_alerts().is_empty());
    }

    #[tokio::test]
    async fn plan_unknown_trap_oid_still_matches_by_raw_oid() {
        let engine = engine_for_plan();
        let node = Uuid::new_v4();
        // A vendor trap outside the curated name set: a rule can still match its numeric OID,
        // which is always present in the message (enrichment is additive, never a replacement).
        let stored = stored_rule("1.3.6.1.4.1.9.9.43.2.0.1", "warning");
        set_rules(&engine, vec![compile_rule(&stored).unwrap()]);
        let p = engine.plan(&trap_msg("1.3.6.1.4.1.9.9.43.2.0.1"), None, node, 1_000);
        assert_eq!(p.row_action, EventAction::Fired);
    }

    #[tokio::test]
    async fn plan_clear_wins_over_fire_for_ambiguous_message() {
        let engine = engine_for_plan();
        let node = Uuid::new_v4();
        let mut stored = stored_rule("link", "warning");
        stored.clear_pattern = Some("link recovered".into());
        set_rules(&engine, vec![compile_rule(&stored).unwrap()]);

        // Fire first so there's something active.
        let p = engine.plan(&syslog_msg("link failed"), None, node, 1_000);
        assert_eq!(p.row_action, EventAction::Fired);
        // "link recovered" matches BOTH the fire pattern ("link") and the clear pattern —
        // clear must win or the alert would flap.
        let p = engine.plan(&syslog_msg("link recovered"), None, node, 2_000);
        assert_eq!(p.row_action, EventAction::Cleared);
        assert!(fires(&p).is_empty());
    }

    #[tokio::test]
    async fn plan_info_severity_records_without_alert() {
        let engine = engine_for_plan();
        let node = Uuid::new_v4();
        set_rules(
            &engine,
            vec![compile_rule(&stored_rule("config changed", "info")).unwrap()],
        );
        let p = engine.plan(&syslog_msg("config changed by admin"), None, node, 1_000);
        assert_eq!(p.row_action, EventAction::Info);
        assert!(p.actions.is_empty());
        assert!(p.matched_rule.is_some());
    }

    #[tokio::test]
    async fn plan_gates_below_min_count() {
        let engine = engine_for_plan();
        let node = Uuid::new_v4();
        let mut stored = stored_rule("auth failure", "warning");
        stored.min_count = 3;
        stored.window_secs = 60;
        set_rules(&engine, vec![compile_rule(&stored).unwrap()]);

        let p = engine.plan(&syslog_msg("auth failure for admin"), None, node, 1_000);
        assert_eq!(p.row_action, EventAction::Info); // matched, gated
        let p = engine.plan(&syslog_msg("auth failure for admin"), None, node, 2_000);
        assert_eq!(p.row_action, EventAction::Info);
        let p = engine.plan(&syslog_msg("auth failure for admin"), None, node, 3_000);
        assert_eq!(p.row_action, EventAction::Fired); // 3rd match inside the window
        assert_eq!(fires(&p).len(), 1);
    }

    #[tokio::test]
    async fn plan_suppresses_during_maintenance() {
        let engine = engine_for_plan();
        let node = Uuid::new_v4();
        set_rules(
            &engine,
            vec![compile_rule(&stored_rule("link down", "critical")).unwrap()],
        );
        // Put the node into an active maintenance window via the alert config.
        let mut maint = std::collections::BTreeSet::new();
        maint.insert(NodeId::from(node));
        engine.alerts.set_config(
            crate::alerts::AlertConfig::new(Vec::new(), HashMap::new()).with_maintenance(maint),
        );

        let p = engine.plan(&syslog_msg("link down on ge-0/0/1"), None, node, 1_000);
        assert_eq!(p.row_action, EventAction::Suppressed);
        assert!(p.actions.is_empty());
    }

    #[tokio::test]
    async fn plan_evaluates_all_rules_not_first_match() {
        let engine = engine_for_plan();
        let node = Uuid::new_v4();
        let r1 = compile_rule(&stored_rule("link down", "warning")).unwrap();
        let r2 = compile_rule(&stored_rule("ge-0/0/1", "critical")).unwrap();
        set_rules(&engine, vec![r1, r2]);

        let p = engine.plan(&syslog_msg("link down on ge-0/0/1"), None, node, 1_000);
        // Both rules matched and both fired independently.
        assert_eq!(fires(&p).len(), 2);
        assert_eq!(p.row_action, EventAction::Fired);
    }

    #[tokio::test]
    async fn re_fire_after_sweep_is_not_permanently_suppressed() {
        // Regression for the sweep/plan race: because plan() and sweep() mutate the
        // runtime and manager active sets together under the runtime lock, an event that
        // arrives right after a TTL sweep re-fires cleanly rather than getting stuck in a
        // "refreshed" loop with the manager alert already closed.
        let engine = engine_for_plan();
        let node = Uuid::new_v4();
        let mut stored = stored_rule("link down", "critical");
        stored.ttl_secs = 60;
        set_rules(&engine, vec![compile_rule(&stored).unwrap()]);

        // Fire, then expire it via the sweeper (both sets cleared together).
        let p = engine.plan(&syslog_msg("link down"), None, node, 0);
        assert_eq!(p.row_action, EventAction::Fired);
        assert_eq!(engine.alerts.active_alerts().len(), 1);
        engine.sweep(1_000_000).await; // well past the 60s TTL
        assert!(engine.alerts.active_alerts().is_empty());

        // A new matching event must fire again (not silently refresh a closed alert).
        let p = engine.plan(&syslog_msg("link down"), None, node, 1_001_000);
        assert_eq!(p.row_action, EventAction::Fired);
        assert_eq!(fires(&p).len(), 1);
        assert_eq!(engine.alerts.active_alerts().len(), 1);
    }

    #[test]
    fn raise_and_resolve_event_alert_in_manager() {
        let manager = AlertManager::new();
        let node = NodeId::from(Uuid::new_v4());
        let check = check_id(node, "event:test");
        let alert = Alert {
            subject: yagra_alert::Subject::Node(node),
            check,
            severity: Severity::Warning,
            state: NodeState::Warning,
            at_unix_ms: 1,
            root_cause: None,
            flapping: false,
            metric: "event:test".into(),
            breach: None,
            ifindex: None,
        };

        // First raise fires; a same-severity duplicate is deduped at the manager.
        assert!(manager.raise_event_alert(alert.clone()).is_some());
        assert!(manager.raise_event_alert(alert.clone()).is_none());
        assert_eq!(manager.active_alerts().len(), 1);
        // The node display state rolls up the event alert.
        assert_eq!(manager.node_state(node), Some(NodeState::Warning));

        // A severity escalation replaces and re-fires.
        let mut worse = alert.clone();
        worse.severity = Severity::Critical;
        worse.state = NodeState::Critical;
        assert!(manager.raise_event_alert(worse).is_some());

        // Resolve returns the previously-active alert; second resolve is a no-op.
        let resolved = manager.resolve_event_alert(check);
        assert!(
            matches!(resolved, Some(NotifyAction::Resolve(a)) if a.severity == Severity::Critical)
        );
        assert!(manager.resolve_event_alert(check).is_none());
        assert!(manager.active_alerts().is_empty());
    }
}
