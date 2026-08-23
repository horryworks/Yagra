// SPDX-License-Identifier: AGPL-3.0-only
//! Where an alert transition goes: the History row **and** the notification, as one step (ADR-092).
//!
//! ## Why this is a type rather than a convention
//!
//! Recording and notifying are two calls that must both happen, and for a year they were two calls
//! written next to each other in three places. That is a shape, not a rule — and it broke exactly
//! the way a shape breaks. ADR-009 Increment 1 wired the pool-coverage watch to the notifier alone:
//! the alert raised, the operator was paged, `alert_history` stayed empty, and every gauge and log
//! line looked correct. It was found on real hardware, not by a test.
//!
//! What stood in for the missing type until now was a **structural test that read the loop's own
//! source text** and asserted both call sites appeared in its body — the technique this repository
//! reaches for when there is no seam to test through, and `pool_coverage.rs` said so in as many
//! words: *"structural because the loop's body is a 30-second tick around a real database"*.
//!
//! So the fix is not a better test. A caller that holds an [`AlertSink`] **cannot** notify without
//! recording, because it does not hold a [`Notifier`] to notify with. The two watch loops and
//! [`crate::events::EventEngine`] each gave up their `Notifier` and `AlertHistoryStore` handles to
//! take one of these, and the four assertions that used to grep for the pair are gone.
//!
//! ## The seam is *under* the sink, not beside it
//!
//! 🚨 The obvious shape — one `AlertSink` trait, a real impl and a fake impl — is wrong here, and
//! wrong in a way this repository has paid for before: a fake that reimplements the rule is tested
//! instead of the rule, and the two drift while every test stays green. So [`RecordingSink`] is the
//! **only** implementation of [`AlertSink`], and the seams are the two effects underneath it,
//! [`HistoryWriter`] and [`AlertNotifier`]. A test builds a real `RecordingSink` over two fakes and
//! watches what it does, which means the logic under test is the logic that ships.
//!
//! ## What deliberately does not come through here
//!
//! Two other alert sources record and notify, and both keep their own I/O because their I/O is the
//! point. They share the *rule* — [`crate::alerts::history_row`] — and nothing else:
//!
//! * **`events::flush_actions`** batches one multi-row INSERT per drained buffer (S10). Folding it
//!   into a per-action `dispatch` would put a database round-trip back on every action of an event
//!   storm, which is the cost that motivated the writer.
//! * **`result_ingest::enqueue_history`** hands the row to a channel and the notification to a
//!   different one (ADR-025), so the poll path never blocks on either. A synchronous `dispatch`
//!   would undo that.
//!
//! ⚠️ Neither of those is protected by the type, so neither gets the property this module buys.
//! That is the honest shape of it: three of five sources cannot notify without recording, and two
//! are still a pair of calls with a comment. Do not read "one sink" as "one path".
//!
//! ## Failure is one-sided on purpose
//!
//! A History write that fails is logged and swallowed; the notification still goes out. An operator
//! being paged matters more than the row, and the row is what History reads afterwards — the
//! reverse order (drop the page because the insert failed) would turn a database hiccup into a
//! missed outage.

use std::sync::Arc;

use async_trait::async_trait;
use yagra_alert::Alert;

use super::{history_row, notify::Notifier, NotifyAction};
use crate::history::AlertHistoryStore;

/// Where a component sends an alert transition.
#[async_trait]
pub(crate) trait AlertSink: Send + Sync {
    /// Record the transition if it produces a row, then deliver it. Both, or neither — a caller
    /// holding this has no way to ask for only one.
    async fn dispatch(&self, action: NotifyAction);
}

/// The `alert_history` half, as a seam. Implemented by [`AlertHistoryStore`] over PostgreSQL.
#[async_trait]
pub(crate) trait HistoryWriter: Send + Sync {
    async fn record(&self, alert: &Alert, resolved: bool) -> anyhow::Result<()>;
}

#[async_trait]
impl HistoryWriter for AlertHistoryStore {
    async fn record(&self, alert: &Alert, resolved: bool) -> anyhow::Result<()> {
        AlertHistoryStore::record(self, alert, resolved).await
    }
}

/// The delivery half, as a seam. Implemented by [`Notifier`] over the four channels.
#[async_trait]
pub(crate) trait AlertNotifier: Send + Sync {
    async fn handle(&self, action: NotifyAction);
}

#[async_trait]
impl AlertNotifier for Notifier {
    async fn handle(&self, action: NotifyAction) {
        Notifier::handle(self, action).await;
    }
}

/// The live sink: `alert_history` first, then delivery.
pub(crate) struct RecordingSink {
    history: Arc<dyn HistoryWriter>,
    notifier: Arc<dyn AlertNotifier>,
    /// What failed, for the log line — "a pool-coverage transition", "an event alert". A parameter
    /// rather than one shared message across three sources, which would tell an operator that
    /// *something* failed to record and not which loop.
    subject: &'static str,
}

impl RecordingSink {
    pub(crate) fn new(
        history: Arc<dyn HistoryWriter>,
        notifier: Arc<dyn AlertNotifier>,
        subject: &'static str,
    ) -> Self {
        Self {
            history,
            notifier,
            subject,
        }
    }
}

#[async_trait]
impl AlertSink for RecordingSink {
    async fn dispatch(&self, action: NotifyAction) {
        if let Some((alert, resolved)) = history_row(&action) {
            if let Err(e) = self.history.record(alert, resolved).await {
                let subject = self.subject;
                tracing::warn!(error = %e, "recording {subject} failed");
            }
        }
        // Unconditional, and outside the `if`: a suppression produces no row but is still a
        // delivery — it is what closes the incident that was opened before the roll-up.
        self.notifier.handle(action).await;
    }
}

#[cfg(test)]
pub(crate) mod testkit {
    use std::sync::Mutex;

    use super::*;

    /// A [`HistoryWriter`] that remembers, and optionally refuses.
    #[derive(Default)]
    pub(crate) struct FakeHistory {
        pub(crate) rows: Mutex<Vec<(Alert, bool)>>,
        /// When true, every write fails — the case where the notification must still go out.
        pub(crate) fail: bool,
    }

    #[async_trait]
    impl HistoryWriter for FakeHistory {
        async fn record(&self, alert: &Alert, resolved: bool) -> anyhow::Result<()> {
            if self.fail {
                anyhow::bail!("the database said no");
            }
            self.rows
                .lock()
                .expect("no panic holds this lock")
                .push((alert.clone(), resolved));
            Ok(())
        }
    }

    /// An [`AlertNotifier`] that remembers.
    #[derive(Default)]
    pub(crate) struct FakeNotifier {
        pub(crate) delivered: Mutex<Vec<NotifyAction>>,
    }

    #[async_trait]
    impl AlertNotifier for FakeNotifier {
        async fn handle(&self, action: NotifyAction) {
            self.delivered
                .lock()
                .expect("no panic holds this lock")
                .push(action);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testkit::{FakeHistory, FakeNotifier};
    use super::*;
    use uuid::Uuid;
    use yagra_alert::{Breach, Subject};

    fn alert() -> Alert {
        Alert {
            subject: Subject::Pool("tokyo".to_owned()),
            check: yagra_common::CheckId::from(Uuid::nil()),
            severity: yagra_common::Severity::Critical,
            state: yagra_common::NodeState::Unreachable,
            at_unix_ms: 0,
            root_cause: None,
            flapping: false,
            metric: "live_pollers".to_owned(),
            breach: None::<Breach>,
            ifindex: None,
        }
    }

    fn sink(fail: bool) -> (RecordingSink, Arc<FakeHistory>, Arc<FakeNotifier>) {
        let history = Arc::new(FakeHistory {
            fail,
            ..FakeHistory::default()
        });
        let notifier = Arc::new(FakeNotifier::default());
        let s = RecordingSink::new(history.clone(), notifier.clone(), "a test transition");
        (s, history, notifier)
    }

    /// **Acceptance first**: a fire is written *and* delivered.
    ///
    /// The order matters — a test that only proved "nothing was skipped" would pass against a sink
    /// that did nothing at all (`rejection-only-tests-pass-when-everything-rejects`).
    #[tokio::test]
    async fn a_fire_is_recorded_and_then_delivered() {
        let (sink, history, notifier) = sink(false);
        sink.dispatch(NotifyAction::Fire(alert())).await;
        let rows = history.rows.lock().unwrap();
        assert_eq!(rows.len(), 1, "the fire left no History row");
        assert!(!rows[0].1, "a fire is recorded with resolved = false");
        assert_eq!(
            notifier.delivered.lock().unwrap().len(),
            1,
            "…and it must still be delivered"
        );
    }

    /// A resolve is the same row with the other flag — the half each caller used to derive itself.
    #[tokio::test]
    async fn a_resolve_is_recorded_as_resolved() {
        let (sink, history, _) = sink(false);
        sink.dispatch(NotifyAction::Resolve(alert())).await;
        assert!(
            history.rows.lock().unwrap()[0].1,
            "a resolve is recorded with resolved = true"
        );
    }

    /// A roll-up is delivered but not recorded: it is not a lifecycle transition, and the incident
    /// it closes was opened by the fire that *was* recorded.
    #[tokio::test]
    async fn a_suppression_is_delivered_but_not_recorded() {
        let (sink, history, notifier) = sink(false);
        sink.dispatch(NotifyAction::Suppress(alert())).await;
        assert!(
            history.rows.lock().unwrap().is_empty(),
            "a roll-up must not be written as a lifecycle row"
        );
        assert_eq!(
            notifier.delivered.lock().unwrap().len(),
            1,
            "…but it must still reach the notifier, which is what closes the incident"
        );
    }

    /// 🚨 **A failed write must not swallow the page.** The reverse order would turn a database
    /// hiccup into a missed outage, which is worse than a missing row by a wide margin.
    #[tokio::test]
    async fn a_failed_history_write_still_delivers() {
        let (sink, history, notifier) = sink(true);
        sink.dispatch(NotifyAction::Fire(alert())).await;
        assert!(history.rows.lock().unwrap().is_empty(), "the write failed");
        assert_eq!(
            notifier.delivered.lock().unwrap().len(),
            1,
            "the operator must still be paged when History is unavailable"
        );
    }
}
