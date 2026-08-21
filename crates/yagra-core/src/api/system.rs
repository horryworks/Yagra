// SPDX-License-Identifier: AGPL-3.0-only
//! Host self-observability — CPU, load, memory and filesystem usage of the core process's host and
//! of every poller reporting telemetry. Yagra monitoring Yagra.
//!
//! `View`-gated like the other fleet views, and **secret-free by construction**: an `instance` is
//! either the literal `core` or an already-sanitized poller id, and a `mount` is a configured alias
//! rather than a raw device path. That matters because both reach a PromQL selector — so
//! [`host_disk_mounts`] resolves the instance against the *known* set first and 404s otherwise,
//! rather than interpolating whatever arrived in the path.

use super::error::{ApiError, ApiResult};
use super::extract::RequireView;
use super::util::now_unix_s;
use super::{clamp_range_step, ApiState, DEFAULT_RANGE_SECS, DEFAULT_STEP_SECS};
use crate::store::MetricPoint;
use axum::{
    extract::{Path, Query, State},
    routing::get,
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::time::Instant;
use yagra_common::{DiskUsage, HostSample};

/// This domain's slice of the OpenAPI document (ADR-035), merged by [`super::openapi::document`].
#[derive(utoipa::OpenApi)]
#[openapi(paths(system_hosts, host_metric_range))]
pub(super) struct Doc;

/// The self-observability routes, merged into `/api/v1` by [`super::router`].
pub(super) fn routes() -> Router<ApiState> {
    Router::new()
        .route("/api/v1/system/hosts", get(system_hosts))
        .route(
            "/api/v1/system/hosts/:instance/metrics/range",
            get(host_metric_range),
        )
}

/// One self-monitored host: current resource values for the core process's host (`role="core"`) or
/// a poller's (`role="poller"`).
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct HostInfo {
    /// `core` or the poller id.
    instance: String,
    /// `"core"` or `"poller"`.
    role: &'static str,
    /// Pool the poller serves; `null` for core.
    pool: Option<String>,
    /// Whether the source is currently reporting (core is always online; a poller within its
    /// heartbeat window).
    online: bool,
    /// Current CPU utilization % (0–100); `null` when no sample yet.
    cpu_pct: Option<f64>,
    /// Current 1-minute load average; `null` when no sample / unavailable on the platform.
    load1: Option<f64>,
    /// Memory in use, bytes; `null` when no sample.
    mem_used_bytes: Option<u64>,
    /// Total physical memory, bytes; `null` when no sample.
    mem_total_bytes: Option<u64>,
    /// Memory-used %, derived; `null` when total is unknown.
    mem_used_pct: Option<f64>,
    /// Highest watched-filesystem used %, for at-a-glance columns; `null` when none has a capacity.
    disk_used_pct: Option<f64>,
    /// Current per-watched-filesystem usage.
    disks: Vec<DiskUsage>,
}

/// Core plus every poller that reports host telemetry.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct SystemHostsResponse {
    hosts: Vec<HostInfo>,
}

/// Build a [`HostInfo`] from an optional sample.
///
/// An absent sample yields all-`null` metrics but still lists the row, so the instance selector
/// shows a host that has just come up rather than hiding it until its first sample lands.
fn host_info(
    instance: &str,
    role: &'static str,
    pool: Option<String>,
    online: bool,
    s: Option<&HostSample>,
) -> HostInfo {
    HostInfo {
        instance: instance.to_owned(),
        role,
        pool,
        online,
        cpu_pct: s.map(|s| s.cpu_pct),
        load1: s.map(|s| s.load1),
        mem_used_bytes: s.map(|s| s.mem_used_bytes),
        mem_total_bytes: s.map(|s| s.mem_total_bytes),
        mem_used_pct: s.and_then(HostSample::mem_used_pct),
        disk_used_pct: s.and_then(HostSample::primary_disk_used_pct),
        disks: s.map(|s| s.disks.clone()).unwrap_or_default(),
    }
}

/// Current host resources for core and every poller reporting telemetry.
///
/// Reads `st.admin` opportunistically rather than taking the `Admin` extractor: core is answering
/// the request, so it can always report *itself*. Skeleton mode returns just core rather than 503.
#[utoipa::path(
    get, path = "/api/v1/system/hosts", tag = "system",
    responses(
        (status = 200, description = "Core plus every poller reporting host telemetry", body = SystemHostsResponse),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks the view permission", body = super::error::ErrorBody),
    ),
)]
async fn system_hosts(
    _guard: RequireView,
    State(st): State<ApiState>,
) -> ApiResult<Json<SystemHostsResponse>> {
    Ok(Json(host_inventory(&st)))
}

/// Core plus every poller reporting host telemetry, shared by `GET /api/v1/system/hosts` and the
/// MCP `get_system_health(section="hosts")` tool (ADR-042 I3a).
///
/// Infallible: core is answering the request, so it can always report itself. Skeleton mode returns
/// core alone rather than an error.
pub(crate) fn host_inventory(st: &ApiState) -> SystemHostsResponse {
    let mut hosts = Vec::new();
    let core = st.host_sample.lock().ok().and_then(|g| g.clone());
    hosts.push(host_info("core", "core", None, true, core.as_ref()));
    if let Some(admin) = st.admin.as_ref() {
        for v in admin.coordinator.poller_views(Instant::now()) {
            if let Some(h) = v.host.as_ref() {
                hosts.push(host_info(
                    &v.id,
                    "poller",
                    Some(v.pool.clone()),
                    v.online,
                    Some(h),
                ));
            }
        }
    }
    SystemHostsResponse { hosts }
}

/// The watched-filesystem mounts for one instance, or `None` when the instance is unknown (⇒ 404).
///
/// This is the input validation for both path parameters at once: `core` is always valid (empty
/// mounts until its first sample), a poller is valid only if the live registry knows it, and the
/// mounts come back from that same lookup. Nothing the caller typed reaches a selector.
fn host_disk_mounts(st: &ApiState, instance: &str) -> Option<Vec<String>> {
    if instance == "core" {
        let mounts = st
            .host_sample
            .lock()
            .ok()
            .and_then(|g| g.clone())
            .map(|s| s.disks.iter().map(|d| d.mount.clone()).collect())
            .unwrap_or_default();
        return Some(mounts);
    }
    let admin = st.admin.as_ref()?;
    let view = admin
        .coordinator
        .poller_views(Instant::now())
        .into_iter()
        .find(|v| v.id == instance)?;
    Some(
        view.host
            .map(|h| h.disks.into_iter().map(|d| d.mount).collect())
            .unwrap_or_default(),
    )
}

/// Query params for the host-metrics range endpoint (all optional; same defaults as node ranges).
#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub(super) struct HostRangeQuery {
    from: Option<i64>,
    to: Option<i64>,
    step: Option<u64>,
}

/// A per-mount filesystem trend. The frontend derives % from the pair, or shows a bare-bytes trend
/// when `size_bytes` is all zero (the `database` size proxy has no capacity).
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct HostDiskRange {
    mount: String,
    used_bytes: Vec<MetricPoint>,
    size_bytes: Vec<MetricPoint>,
}

/// The scalar host trends plus a per-mount filesystem trend, all over one window — one round trip
/// per instance/range change.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct HostMetricRange {
    instance: String,
    cpu_pct: Vec<MetricPoint>,
    load1: Vec<MetricPoint>,
    load5: Vec<MetricPoint>,
    load15: Vec<MetricPoint>,
    mem_used_bytes: Vec<MetricPoint>,
    mem_total_bytes: Vec<MetricPoint>,
    disks: Vec<HostDiskRange>,
}

/// Host CPU/load/mem/disk trends for one instance over `[from,to]` at `step`.
#[utoipa::path(
    get, path = "/api/v1/system/hosts/{instance}/metrics/range", tag = "system",
    params(
        ("instance" = String, Path, description = "`core`, or the id of a poller the live registry knows"),
        HostRangeQuery,
    ),
    responses(
        (status = 200, description = "Scalar host trends plus a per-mount filesystem trend", body = HostMetricRange),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks the view permission", body = super::error::ErrorBody),
        (status = 404, description = "No such instance — resolved against the known set before any selector is built", body = super::error::ErrorBody),
    ),
)]
async fn host_metric_range(
    _guard: RequireView,
    State(st): State<ApiState>,
    Path(instance): Path<String>,
    Query(q): Query<HostRangeQuery>,
) -> ApiResult<Json<HostMetricRange>> {
    Ok(Json(
        host_trends(&st, instance, q.from, q.to, q.step).await?,
    ))
}

/// The step a host trend is folded and drawn at.
///
/// 🚨 **The floor is the sample interval, not 1.** [`crate::store`]'s `host_range_query` folds each
/// step with `avg_over_time` over a window of exactly that step, so a step finer than
/// [`crate::HOST_SAMPLE_SECS`] leaves buckets with no sample in them — gaps, where the old
/// raw-selector read drew a carried-forward value instead. Drawing finer than you sample was never
/// honest; before ADR-082 it was merely invisible, because the raw read had VictoriaMetrics'
/// staleness window to hide behind.
fn host_trend_step(from: i64, to: i64, requested: Option<u64>) -> u64 {
    clamp_range_step(
        from,
        to,
        requested.unwrap_or(DEFAULT_STEP_SECS),
        crate::HOST_SAMPLE_SECS,
    )
}

/// One host's resource trends, shared by `GET /api/v1/system/hosts/:instance/metrics/range` and the
/// MCP `get_system_health(section="host_trends")` tool (ADR-042 I3a).
///
/// ⚠️ **`instance` is resolved against the known set before any selector is built** — `core`, or a
/// poller the live registry knows; anything else is a 404. Nothing the caller typed reaches a
/// PromQL selector, and keeping that in the seam is what stops a second surface from interpolating
/// a raw string. The window defaults and the step clamp live here for the same reason.
pub(crate) async fn host_trends(
    st: &ApiState,
    instance: String,
    from: Option<i64>,
    to: Option<i64>,
    step: Option<u64>,
) -> Result<HostMetricRange, ApiError> {
    let mounts = host_disk_mounts(st, &instance).ok_or_else(|| {
        ApiError::not_found("host_not_found", format!("unknown instance {instance:?}"))
    })?;
    let to = to.unwrap_or_else(now_unix_s);
    let from = from.unwrap_or(to - DEFAULT_RANGE_SECS);
    let step = host_trend_step(from, to, step);
    let store = &st.store;
    // The six scalar series and each mount's two disk series are independent queries — fan them out
    // concurrently, since this backs a 15s System Health refresh.
    let (cpu_pct, load1, load5, load15, mem_used_bytes, mem_total_bytes) = tokio::join!(
        store.host_metric_range(&instance, "cpu_pct", from, to, step),
        store.host_metric_range(&instance, "load1", from, to, step),
        store.host_metric_range(&instance, "load5", from, to, step),
        store.host_metric_range(&instance, "load15", from, to, step),
        store.host_metric_range(&instance, "mem_used_bytes", from, to, step),
        store.host_metric_range(&instance, "mem_total_bytes", from, to, step),
    );
    let disks = futures::future::join_all(mounts.into_iter().map(|mount| {
        let store = &st.store;
        let instance = &instance;
        async move {
            let (used_bytes, size_bytes) = tokio::join!(
                store.host_disk_range(instance, "fs_used_bytes", &mount, from, to, step),
                store.host_disk_range(instance, "fs_size_bytes", &mount, from, to, step),
            );
            HostDiskRange {
                mount,
                used_bytes,
                size_bytes,
            }
        }
    }))
    .await;
    Ok(HostMetricRange {
        instance,
        cpu_pct,
        load1,
        load5,
        load15,
        mem_used_bytes,
        mem_total_bytes,
        disks,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::router;
    use crate::api::tests_support::{private_state, public_state};
    use axum::body::{to_bytes, Body};
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    async fn send(st: ApiState, path: &str) -> axum::response::Response {
        router(st)
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn host_views_are_read_gated() {
        for path in [
            "/api/v1/system/hosts",
            "/api/v1/system/hosts/core/metrics/range",
        ] {
            assert_eq!(
                send(private_state(), path).await.status(),
                StatusCode::UNAUTHORIZED,
                "{path}"
            );
        }
    }

    #[tokio::test]
    async fn core_reports_itself_even_with_no_write_side() {
        // Core is answering the request, so it can always describe its own host. Skeleton mode
        // returns a one-row list rather than 503 — hence `State`, not the `Admin` extractor.
        let resp = send(public_state(), "/api/v1/system/hosts").await;
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let hosts = json["hosts"].as_array().expect("a hosts array");
        assert_eq!(hosts.len(), 1);
        assert_eq!(hosts[0]["instance"], "core");
        assert_eq!(hosts[0]["role"], "core");
        assert_eq!(hosts[0]["online"], true);
    }

    #[tokio::test]
    async fn an_unknown_instance_is_rejected_before_a_selector_is_built() {
        // The instance reaches a PromQL selector, so it is resolved against the known set first.
        // Anything else 404s rather than being interpolated.
        let resp = send(
            public_state(),
            "/api/v1/system/hosts/not-a-host/metrics/range",
        )
        .await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["error"]["code"], "host_not_found");
    }

    /// The step is floored at the sampling interval, so `avg_over_time`'s window always contains a
    /// sample.
    ///
    /// 🚨 The old floor was `1`. That was harmless only while the read was a bare selector, which
    /// VictoriaMetrics answers from its staleness window — folding at `[1s]` instead returns
    /// nothing for 14 of every 15 seconds, i.e. a chart that is mostly gaps. The two changes are
    /// one change: ADR-082 cannot land without this floor.
    #[test]
    fn the_host_trend_step_is_never_finer_than_the_sampling() {
        // A caller asking for per-second detail gets the sampling interval, not per-second gaps.
        assert_eq!(host_trend_step(0, 3600, Some(1)), crate::HOST_SAMPLE_SECS);
        assert_eq!(host_trend_step(0, 3600, Some(0)), crate::HOST_SAMPLE_SECS);
        // Anything at or above the floor is honoured as asked.
        assert_eq!(
            host_trend_step(0, 3600, Some(crate::HOST_SAMPLE_SECS)),
            crate::HOST_SAMPLE_SECS
        );
        assert_eq!(host_trend_step(0, 3600, Some(300)), 300);
        // The default is above the floor — otherwise the default view would be silently re-stepped.
        assert!(host_trend_step(0, 3600, None) >= crate::HOST_SAMPLE_SECS);
        assert_eq!(host_trend_step(0, 3600, None), DEFAULT_STEP_SECS);
        // A long window is still bounded by the point cap, which outranks the floor.
        assert!(host_trend_step(0, 90 * 86_400, Some(15)) > crate::HOST_SAMPLE_SECS);
    }

    #[test]
    fn a_host_with_no_sample_yet_still_lists() {
        // Otherwise a poller that has just come up is invisible in the selector until its first
        // sample, which reads as "that host is gone".
        let info = host_info("p1", "poller", Some("default".to_owned()), true, None);
        assert_eq!(info.instance, "p1");
        assert!(info.online);
        assert!(info.cpu_pct.is_none());
        assert!(info.disks.is_empty());
    }
}
