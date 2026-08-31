// SPDX-License-Identifier: AGPL-3.0-only
//! Live self-monitoring counters for the poll loop.
//!
//! Needs nothing: lock-free atomics written on the hot path by the scheduler and the result
//! consumer, read by the poller-health endpoint.

/// Live self-monitoring counters for the poll loop, shared between the scheduler (producer) and
/// the result consumer. Lock-free atomics — updated on the hot path, read by the poller-health
/// endpoint. Per-poller breakdown needs poller identity on the bus and is a later addition.
#[derive(Default)]
pub struct SchedulerStats {
    last_sweep_ms: std::sync::atomic::AtomicI64,
    jobs_last_round: std::sync::atomic::AtomicU64,
    results_total: std::sync::atomic::AtomicU64,
    // Distributed poller pool (ADR-009/020). Counters bumped by the coordinator as it distributes
    // working sets; the two `pools_*` are gauge-style (overwritten each sweep by the scheduler).
    snapshots_published_total: std::sync::atomic::AtomicU64,
    deltas_published_total: std::sync::atomic::AtomicU64,
    // Redis assignment-mirror rewrites the coordinator actually issued (S18): in steady state this
    // stays flat sweep-over-sweep because an unchanged working set skips the O(fleet) DEL+HSET.
    assignment_mirror_writes_total: std::sync::atomic::AtomicU64,
    pools_working_set: std::sync::atomic::AtomicU64,
    pools_legacy: std::sync::atomic::AtomicU64,
}

/// A point-in-time view of [`SchedulerStats`] for the API.
#[derive(serde::Serialize, utoipa::ToSchema)]
pub struct SchedulerStatsSnapshot {
    /// When the last poll round was dispatched (Unix ms), or `None` if none yet.
    pub last_sweep_unix_ms: Option<i64>,
    /// Jobs published in the most recent round (legacy per-job dispatch only).
    pub jobs_last_round: u64,
    /// Total poll results consumed since start.
    pub results_total: u64,
    /// Working-set snapshots core has published to pollers since start (ADR-020).
    pub snapshots_published_total: u64,
    /// Working-set deltas core has published to pollers since start (ADR-020).
    pub deltas_published_total: u64,
    /// Redis assignment-mirror rewrites issued since start (S18). Flat across steady-state sweeps —
    /// an unchanged working set skips the rewrite — so growth tracks real assignment churn.
    pub assignment_mirror_writes_total: u64,
    /// Pools served in working-set mode in the most recent sweep (a live poller owns them).
    pub pools_working_set: u64,
    /// Pools served in legacy per-job mode in the most recent sweep (no live poller).
    pub pools_legacy: u64,
}

impl SchedulerStats {
    /// Record a completed dispatch round (`jobs` published, stamped now).
    pub fn record_sweep(&self, jobs: u64) {
        use std::sync::atomic::Ordering;
        let ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
            .unwrap_or(0);
        self.last_sweep_ms.store(ms, Ordering::Relaxed);
        self.jobs_last_round.store(jobs, Ordering::Relaxed);
    }

    /// Count one consumed poll result.
    pub fn record_result(&self) {
        self.results_total
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    /// Count one working-set snapshot published (a snapshot, not each of its chunks).
    pub fn record_snapshot(&self) {
        self.snapshots_published_total
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    /// Count one working-set delta published.
    pub fn record_delta(&self) {
        self.deltas_published_total
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    /// Count one Redis assignment-mirror rewrite (S18); skipped sweeps don't call this.
    pub fn record_assignment_write(&self) {
        self.assignment_mirror_writes_total
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    /// Record how many pools ran in each mode this sweep (gauge-style: overwrites).
    pub fn set_pool_modes(&self, working_set: u64, legacy: u64) {
        use std::sync::atomic::Ordering;
        self.pools_working_set.store(working_set, Ordering::Relaxed);
        self.pools_legacy.store(legacy, Ordering::Relaxed);
    }

    /// Snapshot for the API.
    #[must_use]
    pub fn snapshot(&self) -> SchedulerStatsSnapshot {
        use std::sync::atomic::Ordering;
        let ms = self.last_sweep_ms.load(Ordering::Relaxed);
        SchedulerStatsSnapshot {
            last_sweep_unix_ms: (ms > 0).then_some(ms),
            jobs_last_round: self.jobs_last_round.load(Ordering::Relaxed),
            results_total: self.results_total.load(Ordering::Relaxed),
            snapshots_published_total: self.snapshots_published_total.load(Ordering::Relaxed),
            deltas_published_total: self.deltas_published_total.load(Ordering::Relaxed),
            assignment_mirror_writes_total: self
                .assignment_mirror_writes_total
                .load(Ordering::Relaxed),
            pools_working_set: self.pools_working_set.load(Ordering::Relaxed),
            pools_legacy: self.pools_legacy.load(Ordering::Relaxed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A process that has not swept yet reports **no** timestamp, not the epoch.
    ///
    /// The distinction reaches an operator: `last_sweep_unix_ms` feeds "when did this core last
    /// dispatch", and a zero would render as 1970 — which reads as a badly broken clock rather
    /// than as "this core has not started sweeping".
    #[test]
    fn a_core_that_has_not_swept_reports_no_timestamp() {
        let stats = SchedulerStats::default();
        let snap = stats.snapshot();
        assert_eq!(snap.last_sweep_unix_ms, None);
        assert_eq!(snap.jobs_last_round, 0);

        stats.record_sweep(7);
        let snap = stats.snapshot();
        assert!(snap.last_sweep_unix_ms.is_some_and(|ms| ms > 0));
        assert_eq!(snap.jobs_last_round, 7);
    }

    /// 🚨 **Two of these are gauges and the rest are counters, and the difference is not visible
    /// from the field names.** `jobs_last_round` and the two `pools_*` describe the *most recent*
    /// sweep and are overwritten; everything else accumulates for the life of the process. Folding
    /// one into the other would look like a tidy-up and would silently change what the
    /// poller-health endpoint means.
    #[test]
    fn the_per_sweep_gauges_overwrite_while_the_totals_accumulate() {
        let stats = SchedulerStats::default();

        stats.record_sweep(10);
        stats.set_pool_modes(3, 1);
        for _ in 0..4 {
            stats.record_result();
        }
        stats.record_snapshot();
        stats.record_delta();
        stats.record_assignment_write();

        stats.record_sweep(2);
        stats.set_pool_modes(4, 0);
        stats.record_result();
        stats.record_snapshot();

        let snap = stats.snapshot();
        // Gauges: the second sweep replaced the first.
        assert_eq!(snap.jobs_last_round, 2);
        assert_eq!(snap.pools_working_set, 4);
        assert_eq!(snap.pools_legacy, 0);
        // Counters: both sweeps are still in the total.
        assert_eq!(snap.results_total, 5);
        assert_eq!(snap.snapshots_published_total, 2);
        assert_eq!(snap.deltas_published_total, 1);
        assert_eq!(snap.assignment_mirror_writes_total, 1);
    }

    /// Every counter moves independently — a snapshot must not be counted as a delta, and an
    /// assignment-mirror rewrite is its own thing (S18: it stays flat while the fleet is steady,
    /// which is the property that makes it worth reading at all).
    #[test]
    fn each_counter_moves_only_when_its_own_event_happens() {
        let stats = SchedulerStats::default();
        stats.record_delta();
        let snap = stats.snapshot();
        assert_eq!(snap.deltas_published_total, 1);
        assert_eq!(snap.snapshots_published_total, 0);
        assert_eq!(snap.assignment_mirror_writes_total, 0);
        assert_eq!(snap.results_total, 0);
    }
}
