// SPDX-License-Identifier: AGPL-3.0-only
//! Job scheduling: turn inventory into [`PollJob`]s for the bus.
//!
//! The scheduler is the core-side producer of work: per-metric intervals, jitter, and
//! pool-aware dispatch (ADR-009). Jobs carry everything the poller needs (ADR-003).
//!
//! **Split by what each part needs in order to run** (ADR-096):
//!
//! - [`checks`] — the arguments it is handed, and nothing else. Builds **one** check.
//! - [`assemble`] — an already-resolved node and its config. Decides **which** checks it gets.
//! - [`dispatch`] — the stores (PostgreSQL, Redis, the bus). Resolves what the two above need.
//! - [`sweep`] — the clock and the pool map. Decides **when**.
//! - [`stats`] — nothing (lock-free counters).
//!
//! ⚠️ The first two are **pure: they never `.await`**, and `guards.rs` fails the build if they
//! start to. That is not tidiness — a store round trip added there is one *per node per round*,
//! which at fleet scale is the cost [`MonitorHints`] exists to avoid. This module used to claim
//! the whole file was "pure given a node", which stopped being true when the dispatcher arrived.
//!
//! Adding a check kind: the builder goes in [`checks`], the decision to emit it in [`assemble`].

use crate::secrets;
use std::collections::{HashMap, HashSet};
use std::time::Duration;
use uuid::Uuid;
use yagra_common::ProfileId;

mod assemble;
mod checks;
mod dispatch;
#[cfg(test)]
mod guards;
mod stats;
mod sweep;
#[cfg(test)]
mod testkit;

// Everything a caller outside this module could name before the split, still named the
// same way. ⚠️ rustc calls most of them unused and `cargo fix` offers to delete them: nothing
// *inside* the crate spells them, because each sibling imports from the file that defines it.
// Deleting them would narrow what `crate::scheduler` can name — ADR-094 and ADR-095 met this
// twice and answered it the same way, so the allow is what refuses the offer.
#[allow(unused_imports)]
pub use assemble::{assemble_node_jobs, AdjacencyPolicy, MonitorHints, SpecialMonitor};
pub use dispatch::{PollDispatcher, PollDispatcherSeams};
pub use stats::{SchedulerStats, SchedulerStatsSnapshot};
pub(crate) use sweep::run_scheduler;

/// The effective polling interval (seconds) for a node: its profile's override if one is set, else
/// the global default. Pure (no I/O) so the scheduler's resolution is unit-testable.
#[must_use]
pub fn resolve_interval(
    profile: Option<ProfileId>,
    overrides: &HashMap<Uuid, u32>,
    default_secs: u32,
) -> u32 {
    profile
        .and_then(|p| overrides.get(&p.0).copied())
        .unwrap_or(default_secs)
}

/// Whether a node is due to poll, given the time elapsed since its last dispatch. `None` ⇒ never
/// dispatched (due immediately). Pure so the due-check is unit-testable without a clock.
#[must_use]
pub fn due(elapsed_since_last: Option<Duration>, interval: Duration) -> bool {
    match elapsed_since_last {
        Some(elapsed) => elapsed >= interval,
        None => true,
    }
}

/// Whether a pool has ≥1 live poller (`live_pools`, from the coordinator), i.e. whether it can be
/// served in **working-set** mode. Pure so the question is unit-testable.
///
/// ⚠️ This is no longer the whole mode decision: since ADR-009 Increment 1 the complement splits
/// into [`PoolMode::Legacy`] and [`PoolMode::Wait`], and [`pool_mode`] is what a sweep asks. What
/// survives here is the narrower question the fast-path cache needs — "is this cached pool still
/// working-set" — which has no clock and no inventory in it.
#[must_use]
pub fn pool_uses_working_set(pool: &str, live_pools: &HashSet<String>) -> bool {
    live_pools.contains(pool)
}

/// How long a pool with no live poller is waited on before the legacy fallback engages
/// (ADR-009 Increment 1).
///
/// Deliberately [`yagra_bus::OFFLINE_AFTER_SECS`] rather than a number of its own: that is already
/// the window core uses to decide a poller it *had* heard from is gone, so reusing it applies one
/// standard to the first beat and to the next one alike. A second constant here would be a copy of
/// the same fact, free to drift from it.
pub const POOL_GRACE: Duration = Duration::from_secs(yagra_bus::OFFLINE_AFTER_SECS);

/// How a pool is served on one sweep (ADR-009, Increment 1 added the third).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PoolMode {
    /// A live poller owns it: hand the coordinator the pool's whole desired set and let it diff.
    WorkingSet,
    /// No live poller, and no grounds to expect one: per-node `due()` + jittered per-job publish.
    /// The zero-poller fallback and the N/N-1 safety net — this arm is what ADR-009 promised and
    /// nothing here narrows it.
    Legacy,
    /// No live poller **yet**, with grounds to expect one inside [`POOL_GRACE`]: serve the pool
    /// neither way this round and look again shortly.
    Wait,
}

/// Which of the three ways a pool is served on this sweep (ADR-009 Increment 1).
///
/// `since_last_live` is how long ago this scheduler loop last saw the pool in `live_pools`
/// (`None` = never, this process); `since_start` is how long the loop has been running;
/// `registered_pools` is [`Coordinator::registered_pools`](crate::coordinator::Coordinator).
/// Pure so the decision is unit-testable without a clock, a bus or a database.
///
/// **Why a third answer.** "No live poller" used to be read as "nobody is coming", and the two are
/// very different. Both of these were measured on 2026-08-31, on 32-node deployments:
///
/// - **core restarted** — the live registry is in-memory, beats are 10s apart, and the first sweep
///   reads `live_pools` before any of them arrive. Every pool looked empty ⇒ 187 legacy jobs, the
///   whole fleet. At 15,000 nodes it is 120,187, and because legacy publishes are jittered across
///   the poll interval rather than ranked, draining them took ~50 minutes.
/// - **the poller restarted, core untouched** — SIGTERM sends a `leaving` beat, which drops the
///   entry *and* wakes the sweep, so the pool is empty within milliseconds of a graceful restart.
///   `docker restart` of one poller: 187 → 374 published jobs.
///
/// 🚨 In both cases the jobs are **duplicates**, not cover: a poller keeps polling its working set
/// while core is away. So waiting costs nothing when a poller does return, and at most
/// [`POOL_GRACE`] of delayed fallback when it does not.
///
/// ⚠️ The registered-pools arm is the one that must stay narrow. A poller too old to heartbeat has
/// no row in the durable inventory, so its pool is **never** waited on and the legacy fallback it
/// depends on is byte-identical — `an_unregistered_pool_never_waits` is that promise.
#[must_use]
pub fn pool_mode(
    pool: &str,
    live_pools: &HashSet<String>,
    since_last_live: Option<Duration>,
    since_start: Duration,
    registered_pools: &HashSet<String>,
) -> PoolMode {
    if live_pools.contains(pool) {
        return PoolMode::WorkingSet;
    }
    // Heard from, and recently: almost certainly a restart in progress.
    if since_last_live.is_some_and(|d| d < POOL_GRACE) {
        return PoolMode::Wait;
    }
    // Never heard from this process. Only the durable inventory can tell "too early" from
    // "nobody", and it is read as grounds to wait — never as a claim that anyone is alive.
    if since_start < POOL_GRACE && registered_pools.contains(pool) {
        return PoolMode::Wait;
    }
    PoolMode::Legacy
}

/// Per-poll SNMP timeout pushed to the poller (ms). Matches the periodic and on-demand paths.
const SNMP_TIMEOUT_MS: u32 = 2000;

/// A node's resolved SNMP authentication: a v2c community string or a v3 USM document.
pub enum SnmpAuth {
    V2c(String),
    V3(secrets::SnmpV3Secret),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interval_resolves_profile_override_then_default() {
        let p1 = ProfileId::new();
        let p2 = ProfileId::new();
        let mut overrides = HashMap::new();
        overrides.insert(p1.0, 15u32);
        // Profile with an override → its value.
        assert_eq!(resolve_interval(Some(p1), &overrides, 30), 15);
        // Profile without an override → the global default.
        assert_eq!(resolve_interval(Some(p2), &overrides, 30), 30);
        // No profile at all → the global default.
        assert_eq!(resolve_interval(None, &overrides, 30), 30);
    }

    fn pools(names: &[&str]) -> HashSet<String> {
        names.iter().map(|p| (*p).to_owned()).collect()
    }

    #[test]
    fn a_live_pool_is_always_working_set() {
        // The mainline must not move. A live poller beats both waiting arms, so the third answer
        // can only ever divert traffic that was already headed for the legacy fallback.
        let live = pools(&["tokyo"]);
        let registered = pools(&["tokyo"]);
        for since_start in [Duration::ZERO, Duration::from_secs(3600)] {
            for last in [None, Some(Duration::ZERO), Some(Duration::from_secs(3600))] {
                assert_eq!(
                    pool_mode("tokyo", &live, last, since_start, &registered),
                    PoolMode::WorkingSet
                );
            }
        }
    }

    #[test]
    fn a_registered_pool_stops_waiting_once_the_grace_expires() {
        // 🚨 The accepting side, and the reason it is written first: an implementation that simply
        // never publishes passes every other test in this file, and the zero-poller fallback would
        // be dead with nothing anywhere to say so.
        let none = pools(&[]);
        let registered = pools(&["tokyo"]);
        // Inside the window with a row in the inventory: hold the fallback back.
        assert_eq!(
            pool_mode("tokyo", &none, None, Duration::from_secs(5), &registered),
            PoolMode::Wait
        );
        // Past it: publish, exactly as before ADR-009 Increment 1.
        assert_eq!(
            pool_mode(
                "tokyo",
                &none,
                None,
                POOL_GRACE + Duration::from_secs(1),
                &registered
            ),
            PoolMode::Legacy
        );
        // The boundary itself is not a wait — the comparison is `<`, matching `live_pools`.
        assert_eq!(
            pool_mode("tokyo", &none, None, POOL_GRACE, &registered),
            PoolMode::Legacy
        );
    }

    #[test]
    fn an_unregistered_pool_never_waits() {
        // 🚨 The N/N-1 promise. A poller too old to heartbeat has no row in `pollers`, so nothing
        // here may hold its jobs back — not even for the first moment of a core's life, which is
        // precisely when it is waiting for work after a rollout.
        let none = pools(&[]);
        for since_start in [
            Duration::ZERO,
            Duration::from_secs(1),
            POOL_GRACE,
            Duration::from_secs(3600),
        ] {
            assert_eq!(
                pool_mode("ancient", &none, None, since_start, &none),
                PoolMode::Legacy,
                "an unregistered pool must fall straight through to the legacy fallback"
            );
        }
    }

    #[test]
    fn a_pool_whose_poller_just_left_waits_then_falls_back() {
        // The poller-restart arm: measured at +187 published jobs for one `docker restart` of the
        // poller on a 32-node deployment whose core never stopped. It must not depend on the
        // inventory or on startup — this happens hours into a process, to a pool that may have no
        // durable row at all.
        let none = pools(&[]);
        let long_run = Duration::from_secs(86_400);
        assert_eq!(
            pool_mode(
                "tokyo",
                &none,
                Some(Duration::from_secs(5)),
                long_run,
                &none
            ),
            PoolMode::Wait
        );
        assert_eq!(
            pool_mode("tokyo", &none, Some(POOL_GRACE), long_run, &none),
            PoolMode::Legacy
        );
    }

    #[test]
    fn the_third_answer_only_ever_subdivides_the_legacy_side() {
        // What the old working-set-XOR-legacy test was really defending, restated for three
        // answers: waiting may only take traffic from the legacy fallback, never from working-set
        // mode. Break that and a pool with a live poller is served by nobody.
        let live = pools(&["tokyo"]);
        let registries = [pools(&[]), pools(&["tokyo", "osaka"])];
        for pool in ["tokyo", "osaka"] {
            for last in [None, Some(Duration::ZERO), Some(POOL_GRACE)] {
                for since_start in [Duration::ZERO, POOL_GRACE] {
                    for registered in &registries {
                        let mode = pool_mode(pool, &live, last, since_start, registered);
                        assert_eq!(
                            mode == PoolMode::WorkingSet,
                            pool_uses_working_set(pool, &live),
                            "{pool}: working-set mode must be decided by liveness and nothing else"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn due_when_never_dispatched_or_interval_elapsed() {
        let interval = Duration::from_secs(30);
        // Never dispatched → due immediately.
        assert!(due(None, interval));
        // Less than the interval has passed → not due.
        assert!(!due(Some(Duration::from_secs(29)), interval));
        // Exactly the interval → due (>=).
        assert!(due(Some(Duration::from_secs(30)), interval));
        // Well past the interval → due.
        assert!(due(Some(Duration::from_secs(120)), interval));
    }
}
