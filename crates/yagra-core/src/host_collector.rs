// SPDX-License-Identifier: AGPL-3.0-only
//! Core's own host-resource sampling (self-observability, monitoring-conventions).
//!
//! Yagra monitors itself, and this is the half that watches the machine core runs on: CPU, load,
//! memory and disk, sampled every [`HOST_SAMPLE_SECS`], cached for the System Health page and
//! written to the TSDB as the `yagra_host_*` series. **Core is the single writer for its own host
//! and for every poller's** — a poller sends its sample over the bus and core persists it, so there
//! is exactly one process holding the TSDB write path for this series family.
//!
//! Lifted out of `run_live` by ADR-090, which is also why the sampling interval lives here rather
//! than at the crate root: `api/system.rs` derives its trend step from it, and a constant a screen
//! reads should sit with the loop that produces the data, not with the wiring that starts it.

use std::sync::Arc;
use std::time::Duration;

use crate::store::MetricStore;

/// How often core samples its own host resources (self-observability). Matches the WebUI refresh.
pub(crate) const HOST_SAMPLE_SECS: u64 = 15;

/// Start the sampler for the process lifetime.
///
/// **Runs on every core, deliberately not leader-gated.** The series is labelled with the host, so
/// two cores in an HA pair write two distinct series rather than racing one; and a standby whose
/// CPU is pinned is exactly the thing an operator needs to see before promoting it.
pub(crate) fn start(
    store: Arc<dyn MetricStore>,
    cache: crate::api::CoreHostSample,
    pool: sqlx::PgPool,
    shutdown: &yagra_telemetry::CancellationToken,
) {
    yagra_telemetry::spawn_cancellable(shutdown, run_host_collector(store, cache, pool));
}

/// Sample core's own host every [`HOST_SAMPLE_SECS`]: refresh the shared latest-sample cache (read
/// by `GET /api/v1/system/hosts`) and persist the `yagra_host_*` series to the TSDB. Also records
/// PostgreSQL growth as a `mount="database"` used-only proxy — core can't `statvfs` the 0700 PG data
/// dir, so its size comes from `pg_database_size`. Runs for the process lifetime.
async fn run_host_collector(
    store: Arc<dyn MetricStore>,
    cache: crate::api::CoreHostSample,
    pool: sqlx::PgPool,
) {
    let collector = yagra_hoststats::HostCollector::from_env();
    let mut tick = tokio::time::interval(Duration::from_secs(HOST_SAMPLE_SECS));
    loop {
        tick.tick().await;
        let mut sample = collector.sample();
        // Database growth trend: used-only proxy (capacity unknown ⇒ size_bytes = 0).
        match sqlx::query_scalar::<_, i64>("SELECT pg_database_size(current_database())")
            .fetch_one(&pool)
            .await
        {
            Ok(bytes) => sample.disks.push(yagra_common::DiskUsage {
                mount: "database".to_owned(),
                used_bytes: u64::try_from(bytes).unwrap_or(0),
                size_bytes: 0,
            }),
            Err(e) => tracing::debug!(error = %e, "pg_database_size query failed"),
        }
        let at_unix_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
            .unwrap_or(0);
        store
            .write_host_sample("core", "core", None, &sample, at_unix_ms)
            .await;
        if let Ok(mut g) = cache.lock() {
            *g = Some(sample);
        }
    }
}
