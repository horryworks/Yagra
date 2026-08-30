// SPDX-License-Identifier: AGPL-3.0-only
//! The loop that notices a node is gone (ADR-097 Increment 4).
//!
//! Deleting a node produces no poll result, so no check of that node is ever visited again and
//! nothing on the poll path can resolve its alert. This is the periodic ask that closes it — the
//! third of the engine's "the subject of this alert no longer exists" sweeps, and the only one whose
//! subject is the node itself rather than a rule.
//!
//! The whole judgement lives in [`AlertManager::forget_deleted_nodes`]; this file is the tick and
//! the delivery, in the shape `interface_util` and `derived` already use.

use std::sync::Arc;
use std::time::Duration;

use super::sink::AlertSink;
use super::AlertManager;

/// How often the engine is asked whether it still believes something about a node that is gone.
///
/// The answer can only change when the alert config snapshot is rebuilt, and that is gated on the
/// config generation behind a 30-second refresh — so a faster tick would ask the same question
/// twice. This matches the cadence of the two sibling watches and bounds "deleted" to "closed" at
/// roughly 90 seconds.
const TICK: Duration = Duration::from_secs(60);

/// Leader-only: close the alerts of deleted nodes, and forget their display state.
///
/// # Why its own task rather than a step of the config-refresh loop
///
/// [`AlertSink::dispatch`] awaits the History insert **and** the notification, and ADR-104 measured
/// one notification taking up to 31.5 seconds against a slow vendor (61.5 with a 429). Deleting a
/// site's worth of nodes yields one resolution per open alert, so folding this into
/// `alerts::config::run_alert_config_refresh` would park maintenance-window resolution, mute expiry
/// and the classifier reload behind a notification storm — the exact coupling ADR-104 removed.
///
/// # Why leader-only
///
/// The same reason the interface-utilisation and derived-metric watches are: poll-result ingest is
/// leader-only, so only the leader's engine holds any of this state, and two instances dispatching
/// the same resolution would double every notification.
///
/// ⚠️ The safety property this loop depends on lives in [`AlertManager::forget_deleted_nodes`], not
/// here — an engine with no config installed sweeps nothing. A guard at this call site would have to
/// be remembered by the next caller; one inside the method cannot be skipped.
pub(crate) async fn run_deleted_node_watch(alerts: Arc<AlertManager>, sink: Arc<dyn AlertSink>) {
    loop {
        tokio::time::sleep(TICK).await;
        // Resolutions are dispatched one at a time and in order, the same as every other alert
        // source: `Dispatcher` serialises per channel anyway, so concurrency here would only move
        // the queue.
        for action in alerts.forget_deleted_nodes() {
            sink.dispatch(action).await;
        }
    }
}
