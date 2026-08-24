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
