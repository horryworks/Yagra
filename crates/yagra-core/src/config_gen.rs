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

static CONFIG_GENERATION: AtomicU64 = AtomicU64::new(0);

/// Signal that some monitoring config changed (nodes / thresholds / maintenance / groups /
/// profiles / collection / credentials / URL checks). Called from the API audit middleware.
pub fn bump() {
    CONFIG_GENERATION.fetch_add(1, Ordering::Relaxed);
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
}
