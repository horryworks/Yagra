// SPDX-License-Identifier: AGPL-3.0-only
//! **The four background tasks** of the passive-event pipeline (ADR-095): the bus consumer, the TTL
//! sweeper, and the two batch writers.
//!
//! Named for the same thing [`crate::result_ingest`] and [`crate::flow_ingest`] are (ADR-090) —
//! what happens to a record after it arrives. The writers are here rather than beside the repo they
//! call because they are *loops with their own buffers and shutdown paths*, and because both of them
//! route rows between two stores: PostgreSQL keeps alert-linked rows, the log store keeps the rest
//! (ADR-024).
//!
//! ⚠️ No SQL belongs in this file — see [`super::repo`]'s doc for what enforces that.

use std::sync::Arc;

use futures::stream::{Stream, StreamExt};
use yagra_alert::Alert;
use yagra_bus::EventMsg;
use yagra_telemetry::CancellationToken;

use crate::alerts::{Notifier, NotifyAction};
use crate::history::AlertHistoryStore;

use crate::logstore::LogStore;

// The vocabulary lives in the parent, which a child can see without any widening — see
// `super`'s doc for why that is what decides where a thing goes here.
use super::*;

/// Drain events off the bus into the engine. Returns when the stream ends.
///
/// When forwarding is configured (ADR-034), each message is also offered to the forwarder **before**
/// rule matching — so a destination receives the full firehose, unaffected by the burst dedup that
/// exists to keep alerts sane. `offer` never blocks: a full inlet drops the copy and counts it, so
/// forwarding can never slow intake or alerting.
pub async fn consume_events<S>(
    mut events: S,
    engine: Arc<EventEngine>,
    forward: Option<crate::forward::ForwardHandle>,
) where
    S: Stream<Item = EventMsg> + Unpin,
{
    while let Some(msg) = events.next().await {
        if let Some(forward) = forward.as_ref() {
            forward.offer(&msg);
        }
        engine.handle_event(msg, None).await;
    }
    tracing::warn!("event stream ended");
}

/// TTL sweeper loop (spawned in `run_live`).
pub async fn run_ttl_sweeper(engine: Arc<EventEngine>) {
    loop {
        tokio::time::sleep(SWEEP_INTERVAL).await;
        engine.sweep(now_unix_ms()).await;
    }
}

/// Flush a batch of queued events to the durable stores (ADR-024). PostgreSQL gets the firehose
/// when the log store is disabled, or only the alert-linked rows when it is enabled (Contract —
/// the log store then holds the full firehose for search). Best-effort: a store error is logged,
/// never propagated (alerts already fired synchronously in `handle_event`).
async fn flush_persist(
    repo: &EventRepo,
    logs: &Option<Arc<dyn LogStore>>,
    buf: &mut Vec<PersistRecord>,
) {
    if buf.is_empty() {
        return;
    }
    let pg: Vec<&PersistRecord> = if logs.is_some() {
        buf.iter().filter(|r| r.is_alert_linked()).collect()
    } else {
        buf.iter().collect()
    };
    if !pg.is_empty() {
        match repo.insert_events_batch(&pg).await {
            Ok(n) => {
                metrics::counter!("yagra_events_persisted_total", "store" => "postgres")
                    .increment(n);
            }
            Err(e) => tracing::warn!(error = %e, "batch-insert events to PostgreSQL failed"),
        }
    }
    if let Some(store) = logs {
        store.ingest_batch(buf).await;
        metrics::counter!("yagra_events_persisted_total", "store" => "victorialogs")
            .increment(buf.len() as u64);
    }
    buf.clear();
}

/// Async batch persist writer (ADR-024): drains the bounded persist queue and fans each batch out
/// to PostgreSQL and/or the log store off the matcher's hot path. Batches opportunistically (one
/// blocking `recv`, then a non-blocking drain up to [`PERSIST_BATCH_MAX`]). On shutdown it drains
/// and flushes what's queued (best-effort final flush) before returning.
pub async fn run_persist_writer(
    mut rx: tokio::sync::mpsc::Receiver<PersistRecord>,
    repo: Arc<EventRepo>,
    logs: Option<Arc<dyn LogStore>>,
    shutdown: CancellationToken,
) {
    let mut buf: Vec<PersistRecord> = Vec::with_capacity(PERSIST_BATCH_MAX);
    loop {
        tokio::select! {
            biased;
            () = shutdown.cancelled() => {
                while let Ok(rec) = rx.try_recv() {
                    buf.push(rec);
                    if buf.len() >= PERSIST_BATCH_MAX {
                        flush_persist(&repo, &logs, &mut buf).await;
                    }
                }
                flush_persist(&repo, &logs, &mut buf).await;
                break;
            }
            first = rx.recv() => {
                match first {
                    None => {
                        flush_persist(&repo, &logs, &mut buf).await;
                        break;
                    }
                    Some(rec) => {
                        buf.push(rec);
                        while buf.len() < PERSIST_BATCH_MAX {
                            match rx.try_recv() {
                                Ok(rec) => buf.push(rec),
                                Err(_) => break,
                            }
                        }
                        flush_persist(&repo, &logs, &mut buf).await;
                        metrics::gauge!("yagra_persist_queue_depth", "stream" => "events")
                            .set(rx.len() as f64);
                    }
                }
            }
        }
    }
}

/// Map a drained action batch to the `alert_history` rows to insert: a fire records `resolved=false`,
/// a resolve `resolved=true`, and a suppress records nothing (event alerts are never
/// dependency-suppressed, but the variant is handled for exhaustiveness). Pure — unit-tested.
fn history_rows(actions: &[QueuedAction]) -> Vec<(Alert, bool)> {
    actions
        .iter()
        // The rule is `alerts::history_row`; only the batching is this path's own (ADR-092).
        .filter_map(|ea| crate::alerts::history_row(&ea.action))
        .map(|(alert, resolved)| (alert.clone(), resolved))
        .collect()
}

/// Flush a drained batch of alert actions (S10): one multi-row `alert_history` INSERT for all
/// fire/resolve rows, then per-action notification delivery in FIFO order (this loop is what makes
/// it FIFO — since ADR-104 the notifier serializes per channel rather than globally, so a fire and
/// its later resolve stay ordered because they reach the same dispatcher). Best-effort on history: a DB error is logged, never propagated (the
/// in-memory alert state already advanced in the matcher). Fire/resolve counters mirror the inline
/// `run_action` path so metrics are identical whichever path executes.
async fn flush_actions(
    history: &AlertHistoryStore,
    notifier: &Notifier,
    buf: &mut Vec<QueuedAction>,
) {
    if buf.is_empty() {
        return;
    }
    let rows = history_rows(buf);
    if let Err(e) = history.record_batch(&rows).await {
        tracing::warn!(error = %e, count = rows.len(), "batch-record event alert history failed");
    }
    for ea in buf.drain(..) {
        match &ea.action {
            NotifyAction::Fire(_) => {
                metrics::counter!("yagra_event_alerts_fired_total").increment(1);
            }
            NotifyAction::Resolve(_) => {
                metrics::counter!("yagra_event_alerts_resolved_total", "reason" => ea.reason)
                    .increment(1);
            }
            NotifyAction::Suppress(_) => {}
        }
        notifier.handle(ea.action).await;
    }
}

/// Async writer for event-alert side effects (S10): drains the bounded action queue and runs
/// alert-history + notification I/O off the matcher's hot path. Batches history INSERTs (one
/// blocking `recv`, then a non-blocking drain up to [`ACTION_BATCH_MAX`]) so an event storm doesn't
/// serialize a PG round-trip per action on the matcher. Delivers notifications in FIFO order so a
/// fire always precedes its later resolve. On shutdown it drains and flushes what's queued.
pub async fn run_event_action_writer(
    mut rx: tokio::sync::mpsc::Receiver<QueuedAction>,
    history: Arc<AlertHistoryStore>,
    notifier: Arc<Notifier>,
    shutdown: CancellationToken,
) {
    let mut buf: Vec<QueuedAction> = Vec::with_capacity(ACTION_BATCH_MAX);
    loop {
        tokio::select! {
            biased;
            () = shutdown.cancelled() => {
                while let Ok(a) = rx.try_recv() {
                    buf.push(a);
                    if buf.len() >= ACTION_BATCH_MAX {
                        flush_actions(&history, &notifier, &mut buf).await;
                    }
                }
                flush_actions(&history, &notifier, &mut buf).await;
                break;
            }
            first = rx.recv() => {
                match first {
                    None => {
                        flush_actions(&history, &notifier, &mut buf).await;
                        break;
                    }
                    Some(a) => {
                        buf.push(a);
                        while buf.len() < ACTION_BATCH_MAX {
                            match rx.try_recv() {
                                Ok(a) => buf.push(a),
                                Err(_) => break,
                            }
                        }
                        flush_actions(&history, &notifier, &mut buf).await;
                        metrics::gauge!("yagra_persist_queue_depth", "stream" => "event_actions")
                            .set(rx.len() as f64);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::testkit::persist_record;
    use super::*;
    use crate::alerts::check_id;
    use yagra_common::{NodeId, NodeState};

    fn lazy_repo() -> Arc<EventRepo> {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://localhost/unused")
            .expect("lazy pool");
        Arc::new(EventRepo::new(pool))
    }

    #[tokio::test]
    async fn persist_writer_routes_non_alert_rows_to_log_store_only() {
        // With the log store enabled, non-alert-linked rows go to the log store and never touch
        // Postgres — so this exercises the writer end-to-end against a never-connected lazy pool.
        let fake = Arc::new(crate::logstore::InMemoryLogStore::default());
        let logs: Option<Arc<dyn LogStore>> = Some(fake.clone());
        let (tx, rx) = tokio::sync::mpsc::channel::<PersistRecord>(16);
        let token = CancellationToken::new();
        let handle = tokio::spawn(run_persist_writer(rx, lazy_repo(), logs, token));

        tx.send(persist_record(EventAction::None)).await.unwrap();
        tx.send(persist_record(EventAction::Info)).await.unwrap();
        drop(tx); // close the channel → writer drains, flushes, returns
        handle.await.unwrap();

        assert_eq!(fake.len(), 2);
    }

    #[tokio::test]
    async fn persist_writer_final_flush_on_shutdown() {
        let fake = Arc::new(crate::logstore::InMemoryLogStore::default());
        let logs: Option<Arc<dyn LogStore>> = Some(fake.clone());
        let (tx, rx) = tokio::sync::mpsc::channel::<PersistRecord>(16);
        let token = CancellationToken::new();
        let handle = tokio::spawn(run_persist_writer(rx, lazy_repo(), logs, token.clone()));

        tx.send(persist_record(EventAction::None)).await.unwrap();
        // Give the writer a moment to drain the one message, then cancel; the buffer is already
        // flushed, and the cancel arm's final flush is a no-op.
        token.cancel();
        handle.await.unwrap();
        assert_eq!(fake.len(), 1);
    }

    // ── S10: event-action writer (history + notify offloaded off the matcher) ──

    fn test_alert(node: Uuid, severity: Severity) -> Alert {
        let node_id = NodeId::from(node);
        Alert {
            subject: yagra_alert::Subject::Node(node_id),
            check: check_id(node_id, "event:test"),
            severity,
            state: NodeState::Warning,
            at_unix_ms: 1_000,
            root_cause: None,
            flapping: false,
            metric: "event:test".into(),
            breach: None,
            ifindex: None,
        }
    }

    fn lazy_history() -> Arc<AlertHistoryStore> {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://localhost/unused")
            .expect("lazy pool");
        Arc::new(AlertHistoryStore::new(pool))
    }

    #[test]
    fn history_rows_map_fire_and_resolve_and_skip_suppress() {
        let node = Uuid::new_v4();
        let batch = vec![
            QueuedAction {
                action: NotifyAction::Fire(test_alert(node, Severity::Critical)),
                reason: "fire",
            },
            QueuedAction {
                action: NotifyAction::Resolve(test_alert(node, Severity::Critical)),
                reason: "clear",
            },
            QueuedAction {
                action: NotifyAction::Suppress(test_alert(node, Severity::Warning)),
                reason: "fire",
            },
        ];
        let rows = history_rows(&batch);
        // Fire → resolved=false, Resolve → resolved=true, Suppress → no row. Order preserved.
        assert_eq!(rows.len(), 2);
        assert!(!rows[0].1, "fire should record resolved=false");
        assert!(rows[1].1, "resolve should record resolved=true");
    }

    #[tokio::test]
    async fn action_writer_drains_and_returns_on_channel_close() {
        // Suppress actions record no history (no DB touched) and the env notifier has no channels,
        // so this exercises the writer's batch-drain + FIFO delivery + clean shutdown without a
        // live database or notifier. History mapping is covered purely above.
        let (tx, rx) = tokio::sync::mpsc::channel::<QueuedAction>(16);
        let token = CancellationToken::new();
        let handle = tokio::spawn(run_event_action_writer(
            rx,
            lazy_history(),
            Arc::new(Notifier::from_env()),
            token,
        ));
        for _ in 0..3 {
            tx.send(QueuedAction {
                action: NotifyAction::Suppress(test_alert(Uuid::new_v4(), Severity::Warning)),
                reason: "fire",
            })
            .await
            .unwrap();
        }
        drop(tx); // close the channel → writer drains, flushes, returns
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn action_writer_final_flush_on_shutdown() {
        let (tx, rx) = tokio::sync::mpsc::channel::<QueuedAction>(16);
        let token = CancellationToken::new();
        let handle = tokio::spawn(run_event_action_writer(
            rx,
            lazy_history(),
            Arc::new(Notifier::from_env()),
            token.clone(),
        ));
        tx.send(QueuedAction {
            action: NotifyAction::Suppress(test_alert(Uuid::new_v4(), Severity::Warning)),
            reason: "fire",
        })
        .await
        .unwrap();
        token.cancel();
        handle.await.unwrap();
    }
}
