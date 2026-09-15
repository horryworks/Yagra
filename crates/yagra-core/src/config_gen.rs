// SPDX-License-Identifier: AGPL-3.0-only
//! Process-wide **config generation** counter — the S2/S6 dirty signal.
//!
//! Bumped by the API audit middleware ([`crate::api`]) on every successful config-changing mutation,
//! so background rebuilders (the alert-config reloader, later the scheduler's spec resolution) can
//! skip their expensive full-fleet rebuild when nothing has changed since they last ran.
//!
//! Deliberately **coarse**: *any* config mutation bumps it (safe over-invalidation — a needed rebuild
//! is never skipped), which keeps the mechanism correct without wiring a per-node dirty set into the
//! ~22 individual write handlers. A rebuilder records the generation it built at and compares; an
//! unchanged generation means its inputs are byte-for-byte identical, so it can reuse its last build.
//!
//! Non-API config-adjacent writes (e.g. the ingest path's `vendor`/`model` classification) do **not**
//! bump this — they don't affect the alert-config inputs (profile/tags/parent/thresholds), and for
//! the poll-spec cache they cause only benign staleness (a redundant sysDescr re-probe) that the
//! periodic rebuild backstop clears.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

use tokio::sync::Notify;

static CONFIG_GENERATION: AtomicU64 = AtomicU64::new(0);

/// Woken by [`bump`], so a rebuilder can act on a change instead of discovering it on its next round.
static CHANGED: OnceLock<Notify> = OnceLock::new();

fn changed_signal() -> &'static Notify {
    CHANGED.get_or_init(Notify::new)
}

/// Signal that some monitoring config changed (nodes / thresholds / maintenance / groups /
/// profiles / collection / credentials / URL checks). Called from the API audit middleware.
pub fn bump() {
    CONFIG_GENERATION.fetch_add(1, Ordering::Relaxed);
    changed_signal().notify_one();
}

/// Resolves after the next [`bump`] — or at once, if one happened since the last wait.
///
/// The scheduler's sweep selects on this against its own sleep (ADR-144). The sleep is one poll
/// interval, and at a 300-second default that is how long a node added through the API waited for
/// its first poll, and how long an interval edit waited to apply.
///
/// ⚠️ **One waiter: the sweep.** `notify_one` wakes a single waiter and, when nobody is waiting,
/// keeps one permit for the next — which is what folds a burst of edits into one extra sweep and
/// keeps an edit made mid-rebuild from being missed. A second waiter would take turns with the
/// sweep for the same permit.
pub async fn changed() {
    changed_signal().notified().await;
}

/// The current config generation. Compare against a previously-observed value to detect a change.
#[must_use]
pub fn current() -> u64 {
    CONFIG_GENERATION.load(Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bump_advances_the_generation() {
        // The counter is process-global and monotonic, so even with other tests bumping in
        // parallel a post-bump read is strictly greater than a pre-bump snapshot.
        let before = current();
        bump();
        assert!(current() > before);
    }

    #[tokio::test]
    async fn a_bump_wakes_the_waiter_even_when_it_came_first() {
        // The permit is what matters: the sweep is usually busy rebuilding, not waiting, when an
        // edit lands. Other tests bump in parallel, so this can only prove "resolves", never "not".
        bump();
        tokio::time::timeout(std::time::Duration::from_secs(5), changed())
            .await
            .expect("a bump made before the wait still wakes it");
    }
}
