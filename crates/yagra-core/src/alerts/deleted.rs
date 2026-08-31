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
use std::time::{Duration, Instant};

use super::sink::AlertSink;
use super::AlertManager;

/// How often the engine is asked whether it still believes something about a node that is gone.
///
/// The answer can only change when the alert config snapshot is rebuilt, and that is gated on the
/// config generation behind a 30-second refresh — so a faster tick would ask the same question
/// twice. This matches the cadence of the two sibling watches and bounds "deleted" to "closed" at
/// roughly 90 seconds.
const TICK: Duration = Duration::from_secs(60);

/// The cadence for the first [`STARTUP_WINDOW`] of the process, and why it is not [`TICK`].
///
/// ADR-097 Increment 5 gave this loop a second kind of work: a node deleted while core was
/// *stopped* has its alerts read back out of `alert_history` at startup, and until this sweep runs
/// they sit in the engine's active set — where `node_states` unions them into the fleet's per-state
/// breakdown, which can then sum to more than the total `api::fleet::state_tally` reads from
/// PostgreSQL. That transient is bounded by whenever this loop next runs, so at startup it runs
/// often. Steady state is untouched.
///
/// ⚠️ It cannot start before the config does: `forget_deleted_nodes` is a deliberate no-op while
/// `node_meta` is empty, which it is until the first config load. These ticks are cheap for exactly
/// that reason — a sweep with no config takes three locks and returns.
const STARTUP_TICK: Duration = Duration::from_secs(5);

/// How long the fast cadence lasts. Long enough to cover the config's own 30-second refresh with
/// room for a slow first load, short enough that it is over before anything else happens.
const STARTUP_WINDOW: Duration = Duration::from_secs(120);

/// How long to wait before the next sweep, given how long this process has been running.
///
/// Extracted from the loop because the boundary is the only thing here that can be got wrong, and a
/// loop around a real database is not somewhere a test can reach. `<` rather than `<=`: at
/// exactly [`STARTUP_WINDOW`] the startup period is over.
fn tick_at(elapsed: Duration) -> Duration {
    match elapsed < STARTUP_WINDOW {
        true => STARTUP_TICK,
        false => TICK,
    }
}

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
    let started = Instant::now();
    loop {
        tokio::time::sleep(tick_at(started.elapsed())).await;
        // Resolutions are dispatched one at a time and in order, the same as every other alert
        // source: `Dispatcher` serialises per channel anyway, so concurrency here would only move
        // the queue.
        for action in alerts.forget_deleted_nodes() {
            sink.dispatch(action).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The startup cadence has to outlast the alert config's own 30-second refresh, because a sweep
    /// before the first config load is a deliberate no-op — set the window shorter and the fast
    /// ticks would all land while `node_meta` is still empty, leaving the orphan close on the
    /// steady-state minute after all (ADR-097 Increment 5).
    #[test]
    fn the_fast_cadence_outlasts_the_config_refresh_it_is_waiting_for() {
        assert!(
            STARTUP_TICK < TICK,
            "a startup tick no faster than the steady one buys nothing"
        );
        assert!(
            STARTUP_WINDOW >= Duration::from_secs(60),
            "the window must cover two config refreshes, not one"
        );
    }

    /// The boundary itself, which is the only thing in this file a compiler cannot check. Asserted
    /// at the instant before, at, and after — an off-by-one here either drops the fast cadence
    /// entirely or never leaves it.
    #[test]
    fn the_cadence_switches_exactly_at_the_window() {
        assert_eq!(tick_at(Duration::ZERO), STARTUP_TICK);
        assert_eq!(
            tick_at(STARTUP_WINDOW - Duration::from_millis(1)),
            STARTUP_TICK
        );
        assert_eq!(
            tick_at(STARTUP_WINDOW),
            TICK,
            "at the window it is over, not still running"
        );
        assert_eq!(tick_at(STARTUP_WINDOW + Duration::from_secs(3600)), TICK);
    }
}
