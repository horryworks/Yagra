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
use std::collections::HashMap;
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

/// Whether a pool is served in **working-set** mode on a sweep: exactly when it has ≥1 live poller
/// (`live_pools`, from the coordinator). Legacy per-job mode is the strict complement, so the
/// scheduler runs each pool in one mode or the other — never both — and a node is never
/// double-polled (ADR-009). Pure so the per-pool mode decision is unit-testable.
#[must_use]
pub fn pool_uses_working_set(pool: &str, live_pools: &std::collections::HashSet<String>) -> bool {
    live_pools.contains(pool)
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

    #[test]
    fn pool_mode_is_working_set_xor_legacy() {
        use std::collections::HashSet;
        let live: HashSet<String> = ["tokyo".to_string()].into_iter().collect();
        // A pool with a live poller runs working-set; one without runs legacy.
        assert!(pool_uses_working_set("tokyo", &live));
        assert!(!pool_uses_working_set("osaka", &live));
        // For every pool the two modes are exclusive (working-set == !legacy) — no double-polling.
        for pool in ["tokyo", "osaka", "default"] {
            let working_set = pool_uses_working_set(pool, &live);
            let legacy = !pool_uses_working_set(pool, &live);
            assert_ne!(working_set, legacy, "a pool is working-set XOR legacy");
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
