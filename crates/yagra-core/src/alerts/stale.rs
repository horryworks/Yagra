// SPDX-License-Identifier: AGPL-3.0-only
//! The loop that notices an alert nothing is evaluating any more (ADR-097 Increment 6).
//!
//! Two failures of one family, both of which left an alert open for the life of the process:
//!
//! * **The rule was deleted.** `observe` `continue`s a sample whose threshold does not resolve
//!   *before* `process_check` is reached, and `observe_threshold_sample` hard-codes
//!   `alerting: true` — so the `!alerting` close branch is reachable only by the liveness check.
//!   Nothing closed a collected metric's alert. Two doc comments and two tests said otherwise.
//! * **The data stopped.** A node whose SNMP credential is detached stops producing `snmp_up`
//!   entirely, so `observe` never visits that check again and no rule lookup can tell: the rule is
//!   still there, and still resolves. Measured on `.211` 2026-09-02 — four nodes red on a metric
//!   whose last sample was 4.33 days old, across three restarts.
//!
//! The judgement for the first lives in [`AlertManager::resolve_orphaned_collected_alerts`]; this
//! file is the tick and the delivery, in the shape `deleted`, `interface_util` and `derived`
//! already use.

use std::sync::Arc;
use std::time::Duration;

use super::sink::AlertSink;
use super::AlertManager;

/// How often the two questions are asked.
///
/// Deliberately not the siblings' 60 seconds. The rule half's answer can only change when the alert
/// config snapshot is rebuilt (gated on the config generation behind a 30-second refresh), and the
/// freshness half's answer cannot change faster than its own window, which is hours. A minute tick
/// would ask the same question five times over.
///
/// Bounds "the rule was deleted" to "closed" at roughly `TICK`, and "the data stopped" to
/// `STALE_WINDOW_SECS + TICK`.
const TICK: Duration = Duration::from_secs(300);

/// Leader-only: close the alerts nothing is evaluating any more.
///
/// # Why its own task rather than a step of the config-refresh loop
///
/// The same reason `deleted` is: [`AlertSink::dispatch`] awaits the History insert **and** the
/// notification, and ADR-104 measured one notification taking up to 31.5 seconds against a slow
/// vendor. Folding this into `alerts::config::run_alert_config_refresh` would park maintenance-
/// window resolution, mute expiry and the classifier reload behind a notification storm.
///
/// # Why leader-only
///
/// Poll-result ingest is leader-only, so only the leader's engine holds any of this state, and two
/// instances dispatching the same resolution would double every notification.
///
/// # Why there is no startup cadence, unlike `deleted`
///
/// That loop runs fast for its first two minutes because ADR-097 Increment 5 gave it a transient
/// that exists only between startup and the first sweep — a restored deleted-node alert that makes
/// the fleet breakdown sum past its own total. This loop has no such transient. The one thing it
/// waits for is a loaded config, and that is handled inside the engine method rather than by a
/// cadence here.
///
/// ⚠️ The safety property this loop depends on lives in the engine method, not here — an engine
/// with no config installed sweeps nothing. A guard at this call site would have to be remembered
/// by the next caller; one inside the method cannot be skipped.
pub(crate) async fn run_stale_check_watch(alerts: Arc<AlertManager>, sink: Arc<dyn AlertSink>) {
    loop {
        tokio::time::sleep(TICK).await;
        // Resolutions are dispatched one at a time and in order, the same as every other alert
        // source: `Dispatcher` serialises per channel anyway, so concurrency here would only move
        // the queue.
        for action in alerts.resolve_orphaned_collected_alerts() {
            sink.dispatch(action).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tick is slower than its siblings on purpose, and the reason is written in `TICK`'s doc.
    /// Pinned so that "make it match the others" is a deliberate edit rather than a tidy-up.
    #[test]
    fn the_tick_is_slower_than_the_config_refresh_it_reads_behind() {
        assert!(
            TICK >= Duration::from_secs(60),
            "a faster tick asks a question whose answer cannot have changed"
        );
    }

    /// The loop must actually run both halves and drain them. Reads production source rather than
    /// `include_str!` so the file this points at is the one the crate compiles.
    ///
    /// 🚨 The floor comes first: everything below asks whether something is *present*, and over an
    /// empty slice that is a claim about nothing.
    #[test]
    fn the_rule_sweep_runs_inside_the_watch_loop() {
        let production = crate::module_source::code("src/alerts", "stale");
        let watch = production
            .split("async fn run_stale_check_watch")
            .nth(1)
            .expect("the watch loop exists");
        let body = &watch[..watch.find("\nfn ").unwrap_or(watch.len())];
        assert!(
            body.contains("sink.dispatch("),
            "the slice is not the watch loop's body — it does not even drain its actions"
        );
        assert!(
            body.contains("resolve_orphaned_collected_alerts()"),
            "without it, deleting a collected metric's rule strands its alert for the life of the \
             process — the defect ADR-097 Increment 6 exists to close"
        );
    }
}
