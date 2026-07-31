// SPDX-License-Identifier: AGPL-3.0-only
//! Metric reads: one node's series, and the fleet-wide Top-N rankings behind the dashboard
//! widgets.
//!
//! Two rules run through everything here.
//!
//! **The metric name reaches a PromQL selector**, so it is parsed at the edge with
//! [`super::is_valid_metric_name`] before it is interpolated, never sanitized afterwards
//! (security.md). The logical aliases (`cpu`, `memory`) expand to selectors built from constants in
//! this file only — no request input reaches those.
//!
//! **The TSDB carries only node ids** (ADR-011), so every ranking comes back to PostgreSQL for
//! display names via [`super::nodes::resolve_node_names`]. That join is best-effort: a node deleted
//! since the sample was written keeps its id as its name rather than failing the whole ranking.
//!
//! Rates are not stored. Pollers write raw counters and rate/utilisation is derived at query time
//! (ADR-012), which is why the interface endpoints rank by a query-time rate rather than by a
//! stored gauge.

use super::extract::RequireView;
use super::{
    clamp_range_step, is_valid_metric_name, ApiError, ApiResult, ApiState, DEFAULT_RANGE_SECS,
    DEFAULT_RATE_LOOKBACK_SECS, DEFAULT_STEP_SECS,
};
use crate::store::{DeltaDirection, InterfaceTopMetric, MetricPoint, TopAgg};
use axum::{
    extract::{Path, Query, State},
    routing::get,
    Json, Router,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use yagra_common::{IfIndex, NodeId, SeriesKey};

/// This domain's slice of the OpenAPI document (ADR-035), merged by [`super::openapi::document`].
#[derive(utoipa::OpenApi)]
#[openapi(paths(
    get_node_metric,
    get_node_metric_range,
    get_interface_series,
    top_metrics,
    interface_top,
    interface_delta,
    interface_heatmap,
    throughput_range
))]
pub(super) struct Doc;

/// The metric routes, merged into `/api/v1` by [`super::router`].
pub(crate) fn routes() -> Router<ApiState> {
    Router::new()
        .route(
            "/api/v1/nodes/:node_id/metrics/:metric",
            get(get_node_metric),
        )
        .route(
            "/api/v1/nodes/:node_id/metrics/:metric/range",
            get(get_node_metric_range),
        )
        .route(
            "/api/v1/nodes/:node_id/interfaces/:ifindex/series",
            get(get_interface_series),
        )
        .route("/api/v1/metrics/top", get(top_metrics))
        .route("/api/v1/metrics/interface-top", get(interface_top))
        .route("/api/v1/metrics/interface-delta", get(interface_delta))
        .route("/api/v1/metrics/interface-heatmap", get(interface_heatmap))
        .route("/api/v1/metrics/throughput-range", get(throughput_range))
}

// ── One node's series ────────────────────────────────────────────────────────

/// Latest reading for one node metric.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub(crate) struct MetricReading {
    pub node_id: NodeId,
    pub metric: String,
    pub value: f64,
}

/// A time-series window for one node metric.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub(crate) struct MetricRange {
    pub node_id: NodeId,
    pub metric: String,
    pub points: Vec<MetricPoint>,
}

/// Optional aggregation for the metric reads. `agg=max` collapses a per-entity table gauge
/// (e.g. CPU% per `entPhysicalIndex`) into one node-level value; absent ⇒ scalar node series.
#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub(super) struct MetricQuery {
    agg: Option<String>,
}

/// Reject an `agg` value we don't support (validate at the edge — security.md).
fn invalid_agg(other: &str) -> ApiError {
    ApiError::bad_request(
        "invalid_agg",
        format!("unsupported agg {other:?}; expected 'max'"),
    )
}

/// Reject a metric name that is not a Prometheus identifier, **before** it reaches the selector.
fn check_metric_name(metric: &str) -> Result<(), ApiError> {
    if is_valid_metric_name(metric) {
        Ok(())
    } else {
        Err(ApiError::bad_request(
            "invalid_metric_name",
            format!("metric name {metric:?} is not a valid identifier"),
        ))
    }
}

#[utoipa::path(
    get, path = "/api/v1/nodes/{node_id}/metrics/{metric}", tag = "metrics",
    params(
        ("node_id" = Uuid, Path, description = "Node id"),
        ("metric" = String, Path, description = "Metric name — a Prometheus identifier"),
        MetricQuery,
    ),
    responses(
        (status = 200, description = "The latest sample for that series", body = MetricReading),
        (status = 400, description = "The metric name is not an identifier, or `agg` is unsupported", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks the read permission", body = super::error::ErrorBody),
        (status = 404, description = "The node is not collecting that series", body = super::error::ErrorBody),
    ),
)]
async fn get_node_metric(
    _perm: RequireView,
    State(st): State<ApiState>,
    Path((node_id, metric)): Path<(Uuid, String)>,
    Query(q): Query<MetricQuery>,
) -> ApiResult<Json<MetricReading>> {
    check_metric_name(&metric)?;
    let node = NodeId::from(node_id);
    let key = SeriesKey::node(node, metric.as_str());
    let value = match q.agg.as_deref() {
        Some("max") => st.store.aggregate_latest(&key).await,
        Some(other) => return Err(invalid_agg(other)),
        None => st.store.latest(&key).await,
    };
    // "No reading" is a 404 rather than a null value: the caller asked for a specific series, and
    // an absent one means the node is not collecting it — a different fact from "it reads zero".
    let value = value.ok_or_else(|| {
        ApiError::not_found(
            "metric_not_found",
            format!("no reading for metric '{metric}' on node {node_id}"),
        )
    })?;
    Ok(Json(MetricReading {
        node_id: node,
        metric,
        value,
    }))
}

/// Query params for the range endpoint (all optional; sensible defaults applied).
#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub(super) struct RangeQuery {
    from: Option<i64>,
    to: Option<i64>,
    step: Option<u64>,
    /// `max` ⇒ node-level aggregate of a per-entity table gauge; absent ⇒ scalar node series.
    agg: Option<String>,
}

#[utoipa::path(
    get, path = "/api/v1/nodes/{node_id}/metrics/{metric}/range", tag = "metrics",
    params(
        ("node_id" = Uuid, Path, description = "Node id"),
        ("metric" = String, Path, description = "Metric name — a Prometheus identifier"),
        RangeQuery,
    ),
    responses(
        (status = 200, description = "The window's points; empty when the slice has no samples", body = MetricRange),
        (status = 400, description = "The metric name is not an identifier, or `agg` is unsupported", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks the read permission", body = super::error::ErrorBody),
    ),
)]
async fn get_node_metric_range(
    _perm: RequireView,
    State(st): State<ApiState>,
    Path((node_id, metric)): Path<(Uuid, String)>,
    Query(q): Query<RangeQuery>,
) -> ApiResult<Json<MetricRange>> {
    check_metric_name(&metric)?;
    let node = NodeId::from(node_id);
    let to = q.to.unwrap_or_else(super::now_unix_s);
    let from = q.from.unwrap_or(to - DEFAULT_RANGE_SECS);
    // Clamping the step (rather than trusting it) bounds the point count, so a wide window cannot
    // be turned into an unbounded TSDB response by asking for a one-second step.
    let step = clamp_range_step(from, to, q.step.unwrap_or(DEFAULT_STEP_SECS), 1);
    let key = SeriesKey::node(node, metric.as_str());
    let points = match q.agg.as_deref() {
        Some("max") => st.store.aggregate_range(&key, from, to, step).await,
        Some(other) => return Err(invalid_agg(other)),
        None => st.store.range(&key, from, to, step).await,
    };
    // An empty window is a 200 with no points, not a 404: the series exists, this slice of it is
    // simply empty, and a chart renders that as a gap rather than an error.
    Ok(Json(MetricRange {
        node_id: node,
        metric,
        points,
    }))
}

// ── One interface's series ───────────────────────────────────────────────────

/// Per-interface time-series for the node-detail Interfaces pane: In/Out throughput (bits/sec,
/// from `rate()` of the octet counters) and In/Out errors (per second).
///
/// All four share one `timestamps` axis — the union of returned points, with `null` in the gaps —
/// so the chart gets aligned series rather than four independently-indexed ones. Derived at query
/// time (ADR-012); empty when there is no history.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub(crate) struct InterfaceSeries {
    pub timestamps: Vec<i64>,
    pub in_bps: Vec<Option<f64>>,
    pub out_bps: Vec<Option<f64>>,
    pub in_errors: Vec<Option<f64>>,
    pub out_errors: Vec<Option<f64>>,
}

#[utoipa::path(
    get, path = "/api/v1/nodes/{node_id}/interfaces/{ifindex}/series", tag = "metrics",
    params(
        ("node_id" = Uuid, Path, description = "Node id"),
        ("ifindex" = u32, Path, description = "SNMP ifIndex of the interface"),
        RangeQuery,
    ),
    responses(
        (status = 200, description = "In/out throughput and error rates on one shared timestamp axis", body = InterfaceSeries),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks the read permission", body = super::error::ErrorBody),
    ),
)]
async fn get_interface_series(
    _perm: RequireView,
    State(st): State<ApiState>,
    Path((node_id, ifindex)): Path<(Uuid, u32)>,
    Query(q): Query<RangeQuery>,
) -> Json<InterfaceSeries> {
    let node = NodeId::from(node_id);
    let key = |metric: &str| SeriesKey::interface(node, IfIndex(ifindex), metric);
    let to = q.to.unwrap_or_else(super::now_unix_s);
    let from = q.from.unwrap_or(to - DEFAULT_RANGE_SECS);
    // Default to ~120 points across the window; the rate lookback spans a few steps so a
    // single missed poll doesn't punch a hole in the line.
    let span = u64::try_from((to - from).max(1)).unwrap_or(DEFAULT_RANGE_SECS as u64);
    let step = clamp_range_step(from, to, q.step.unwrap_or((span / 120).max(60)), 1);
    let lookback = (step * 4).max(DEFAULT_RATE_LOOKBACK_SECS);

    // The four series are independent range queries — fan them out concurrently (this endpoint
    // fires per lazy row-sparkline and on the 15s interface-dock refresh). Bind the keys first so
    // they outlive the joined futures.
    let (k_in, k_out, k_ierr, k_oerr) = (
        key("if_hc_in_octets"),
        key("if_hc_out_octets"),
        key("if_in_errors"),
        key("if_out_errors"),
    );
    let (in_oct, out_oct, in_err, out_err) = tokio::join!(
        st.store.rate_range(&k_in, from, to, step, lookback),
        st.store.rate_range(&k_out, from, to, step, lookback),
        st.store.rate_range(&k_ierr, from, to, step, lookback),
        st.store.rate_range(&k_oerr, from, to, step, lookback),
    );
    Json(align_interface_series(&in_oct, &out_oct, &in_err, &out_err))
}

/// Align the four counter-derived series onto one shared axis.
///
/// Octet rates are scaled ×8 here — the counters are bytes and the chart is bits/sec. Doing it at
/// the edge rather than in the store keeps the stored series raw (ADR-012), and doing it in one
/// place keeps the two octet series from drifting apart from the two error series, which are not
/// scaled.
fn align_interface_series(
    in_oct: &[MetricPoint],
    out_oct: &[MetricPoint],
    in_err: &[MetricPoint],
    out_err: &[MetricPoint],
) -> InterfaceSeries {
    let mut grid_set: std::collections::BTreeSet<i64> = std::collections::BTreeSet::new();
    for s in [in_oct, out_oct, in_err, out_err] {
        for p in s {
            grid_set.insert(p.t);
        }
    }
    let grid: Vec<i64> = grid_set.into_iter().collect();
    let align = |pts: &[MetricPoint], scale: f64| -> Vec<Option<f64>> {
        let m: std::collections::HashMap<i64, f64> = pts.iter().map(|p| (p.t, p.v)).collect();
        grid.iter().map(|t| m.get(t).map(|v| v * scale)).collect()
    };
    InterfaceSeries {
        in_bps: align(in_oct, 8.0),
        out_bps: align(out_oct, 8.0),
        in_errors: align(in_err, 1.0),
        out_errors: align(out_err, 1.0),
        timestamps: grid,
    }
}

// ── Fleet Top-N ──────────────────────────────────────────────────────────────

/// Query for the fleet Top-N endpoint (`GET /api/v1/metrics/top`).
#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub(super) struct TopQuery {
    /// Metric to rank by (validated identifier, or a logical alias).
    metric: String,
    /// `now` (default) ⇒ most recent value; `max_1h` ⇒ trailing-hour peak.
    agg: Option<String>,
    /// How many nodes to return (default 5, clamped 1..=50).
    limit: Option<usize>,
}

/// One ranked node in a Top-N result.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub(crate) struct TopEntry {
    pub node_id: Uuid,
    /// Display name, joined from PostgreSQL (ADR-011); falls back to the id string if the node has
    /// since been deleted.
    pub name: String,
    pub value: f64,
}

/// Logical node-metric aliases for the fleet Top-N: a friendly name → the set of per-vendor
/// metric names ranked together via a `__name__` regex (one query collapses them with
/// `max by (node)`).
///
/// Only "busy-style" gauges where higher = worse are included — idle and temperature metrics are
/// excluded, because ranking them descending would put the *healthiest* nodes at the top. Memory
/// uses the vendors that expose a direct percentage; the bytes-derived percentage for Cisco/UCD is
/// a later recording-rule job.
///
/// The selector is built from these constants only, so it is safe to interpolate — no request
/// input reaches the PromQL.
fn logical_metric_selector(alias: &str) -> Option<String> {
    let names: &[&str] = match alias {
        "cpu" => &[
            "huawei_cpu_usage",
            "cisco_cpu_5min",
            "nxos_cpu_util",
            "fortinet_cpu_usage",
            "juniper_cpu_1min",
            "hr_processor_load",
        ],
        "memory" => &["huawei_mem_usage", "nxos_mem_util", "fortinet_mem_usage"],
        _ => return None,
    };
    Some(format!("{{__name__=~\"{}\"}}", names.join("|")))
}

/// Parse the shared `agg` query param (`now` default | `max_1h`) into a [`TopAgg`].
fn parse_top_agg(agg: Option<&str>) -> Result<TopAgg, ApiError> {
    match agg {
        None | Some("now") => Ok(TopAgg::Now),
        Some("max_1h") => Ok(TopAgg::Max1h),
        Some(other) => Err(ApiError::bad_request(
            "invalid_agg",
            format!("agg must be 'now' or 'max_1h', got {other:?}"),
        )),
    }
}

/// Resolve a Top-N `metric` param into a PromQL selector: a logical alias expands to a constant
/// multi-name selector, anything else must be a valid identifier.
fn top_selector(metric: &str) -> Result<String, ApiError> {
    match logical_metric_selector(metric) {
        Some(sel) => Ok(sel),
        None => {
            check_metric_name(metric)?;
            Ok(metric.to_owned())
        }
    }
}

/// Fleet-wide Top-N for a metric: the highest-value nodes right now (or by hourly peak). Powers
/// the dashboard "Top RTT / CPU / memory / …" widgets from one endpoint.
#[utoipa::path(
    get, path = "/api/v1/metrics/top", tag = "metrics",
    params(TopQuery),
    responses(
        (status = 200, description = "The highest-value nodes, ranked; empty when the store cannot rank", body = Vec<TopEntry>),
        (status = 400, description = "`metric` is neither a logical alias nor an identifier, or `agg` is unsupported", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks the read permission", body = super::error::ErrorBody),
    ),
)]
async fn top_metrics(
    _perm: RequireView,
    State(st): State<ApiState>,
    Query(q): Query<TopQuery>,
) -> ApiResult<Json<Vec<TopEntry>>> {
    let selector = top_selector(&q.metric)?;
    let agg = parse_top_agg(q.agg.as_deref())?;
    let limit = q.limit.unwrap_or(5).clamp(1, 50);
    let ranked = st.store.top_nodes(&selector, agg, limit).await;
    let names = super::nodes::resolve_node_names(&st, ranked.iter().map(|(id, _)| *id)).await;
    Ok(Json(
        ranked
            .into_iter()
            .map(|(id, value)| TopEntry {
                node_id: id,
                name: names.get(&id).cloned().unwrap_or_else(|| id.to_string()),
                value,
            })
            .collect(),
    ))
}

// ── Interface rankings ───────────────────────────────────────────────────────

/// Query for the fleet interface Top-N endpoint.
#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub(super) struct InterfaceTopQuery {
    /// `throughput` | `in_bps` | `out_bps` | `errors` | `discards`.
    metric: String,
    agg: Option<String>,
    limit: Option<usize>,
}

/// One ranked interface in a fleet interface Top-N.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub(crate) struct InterfaceTopEntry {
    pub node_id: Uuid,
    pub node_name: String,
    pub ifindex: i32,
    pub if_name: Option<String>,
    pub if_alias: Option<String>,
    /// Configured speed (bits/sec) for util%; `null` if unknown.
    pub if_speed_bps: Option<i64>,
    /// bits/sec for throughput metrics, errors|discards per second otherwise.
    pub value: f64,
}

/// Parse the interface Top-N `metric` param. Listed exhaustively rather than matched loosely: this
/// selects which query-time rate expression runs, so an unrecognised value must be a rejection and
/// not a default.
fn parse_interface_metric(metric: &str) -> Result<InterfaceTopMetric, ApiError> {
    match metric {
        "throughput" => Ok(InterfaceTopMetric::Throughput),
        "in_bps" => Ok(InterfaceTopMetric::InBps),
        "out_bps" => Ok(InterfaceTopMetric::OutBps),
        "errors" => Ok(InterfaceTopMetric::Errors),
        "discards" => Ok(InterfaceTopMetric::Discards),
        other => Err(ApiError::bad_request(
            "invalid_metric",
            format!("metric must be throughput|in_bps|out_bps|errors|discards, got {other:?}"),
        )),
    }
}

/// Fleet-wide busiest/erroring interfaces. Ranks `(node,ifindex)` by a query-time rate, then joins
/// node + interface names (and speed) from PostgreSQL.
#[utoipa::path(
    get, path = "/api/v1/metrics/interface-top", tag = "metrics",
    params(InterfaceTopQuery),
    responses(
        (status = 200, description = "The busiest or most-erroring interfaces, ranked", body = Vec<InterfaceTopEntry>),
        (status = 400, description = "`metric` is not one of throughput|in_bps|out_bps|errors|discards, or `agg` is unsupported", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks the read permission", body = super::error::ErrorBody),
    ),
)]
async fn interface_top(
    _perm: RequireView,
    State(st): State<ApiState>,
    Query(q): Query<InterfaceTopQuery>,
) -> ApiResult<Json<Vec<InterfaceTopEntry>>> {
    let metric = parse_interface_metric(&q.metric)?;
    let agg = parse_top_agg(q.agg.as_deref())?;
    let limit = q.limit.unwrap_or(6).clamp(1, 50);
    let ranked = st.store.top_interfaces(metric, agg, limit).await;
    Ok(Json(build_interface_entries(&st, ranked).await))
}

/// Join a fleet interface ranking `(node, ifindex, value)` to node + interface names (and speed)
/// from PostgreSQL — one repo query over the distinct nodes in the result. Shared by the interface
/// Top-N and interface-delta endpoints.
pub(crate) async fn build_interface_entries(
    st: &ApiState,
    ranked: Vec<(Uuid, i32, f64)>,
) -> Vec<InterfaceTopEntry> {
    let node_ids: Vec<Uuid> = {
        let mut ids: Vec<Uuid> = ranked.iter().map(|(n, _, _)| *n).collect();
        ids.sort_unstable();
        ids.dedup();
        ids
    };
    let names = super::nodes::resolve_node_names(st, node_ids.iter().copied()).await;
    let idents = match st.admin.as_ref() {
        Some(admin) => admin
            .repo
            .interface_idents_for(&node_ids)
            .await
            .unwrap_or_default(),
        None => std::collections::HashMap::new(),
    };
    ranked
        .into_iter()
        .map(|(node_id, ifindex, value)| {
            let ident = idents.get(&(node_id, ifindex));
            InterfaceTopEntry {
                node_id,
                node_name: names
                    .get(&node_id)
                    .cloned()
                    .unwrap_or_else(|| node_id.to_string()),
                ifindex,
                if_name: ident.and_then(|i| i.if_name.clone()),
                if_alias: ident.and_then(|i| i.if_alias.clone()),
                if_speed_bps: ident.and_then(|i| i.if_speed),
                value,
            }
        })
        .collect()
}

/// Query for the interface rate-delta endpoint (traffic spikes/drops).
#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub(super) struct InterfaceDeltaQuery {
    /// `up` (spikes) | `down` (drops).
    direction: String,
    /// Comparison window in seconds (default 300 = now vs 5m ago).
    window: Option<u64>,
    limit: Option<usize>,
}

/// Interfaces whose total throughput moved the most vs `window` ago — spikes (`up`) or drops
/// (`down`). `value` is the signed delta in bits/sec.
#[utoipa::path(
    get, path = "/api/v1/metrics/interface-delta", tag = "metrics",
    params(InterfaceDeltaQuery),
    responses(
        (status = 200, description = "Interfaces ranked by signed throughput delta (bits/sec)", body = Vec<InterfaceTopEntry>),
        (status = 400, description = "`direction` is not 'up' or 'down'", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks the read permission", body = super::error::ErrorBody),
    ),
)]
async fn interface_delta(
    _perm: RequireView,
    State(st): State<ApiState>,
    Query(q): Query<InterfaceDeltaQuery>,
) -> ApiResult<Json<Vec<InterfaceTopEntry>>> {
    let direction = match q.direction.as_str() {
        "up" => DeltaDirection::Up,
        "down" => DeltaDirection::Down,
        other => {
            return Err(ApiError::bad_request(
                "invalid_direction",
                format!("direction must be 'up' or 'down', got {other:?}"),
            ))
        }
    };
    let window = q.window.unwrap_or(300).clamp(60, 3600);
    let limit = q.limit.unwrap_or(6).clamp(1, 50);
    let ranked = st.store.interface_delta(direction, window, limit).await;
    Ok(Json(build_interface_entries(&st, ranked).await))
}

// ── Busiest-links heatmap ────────────────────────────────────────────────────

/// Query for the interface throughput heatmap: `?limit=&from=&to=&step=`.
#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub(super) struct HeatmapQuery {
    limit: Option<usize>,
    from: Option<i64>,
    to: Option<i64>,
    step: Option<u64>,
}

/// A links × time grid. `values[i][j]` is link `i`'s throughput at `timestamps[j]`, so every row
/// is the same length and the client can shade cells without bounds checks.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub(crate) struct InterfaceHeatmap {
    pub links: Vec<String>,
    pub timestamps: Vec<i64>,
    pub values: Vec<Vec<f64>>,
}

/// Build the grid from each link's independently-sampled series.
///
/// A gap becomes `0.0` rather than a hole: this is a *heatmap*, so every cell must have a value to
/// shade, and "no traffic recorded" is the honest reading of a missing throughput sample.
fn build_heatmap(entries: &[InterfaceTopEntry], ranges: Vec<Vec<MetricPoint>>) -> InterfaceHeatmap {
    let mut union: std::collections::BTreeSet<i64> = std::collections::BTreeSet::new();
    let mut per_link: Vec<(String, std::collections::HashMap<i64, f64>)> = Vec::new();
    for (e, pts) in entries.iter().zip(ranges) {
        let mut m = std::collections::HashMap::new();
        for p in pts {
            union.insert(p.t);
            m.insert(p.t, p.v);
        }
        // Prefer the interface name, fall back to its alias, then to the raw ifindex — a row with
        // no label at all would be unreadable in the chart.
        let iface = e
            .if_name
            .clone()
            .or_else(|| e.if_alias.clone())
            .unwrap_or_else(|| format!("if{}", e.ifindex));
        per_link.push((format!("{} · {}", e.node_name, iface), m));
    }
    let timestamps: Vec<i64> = union.into_iter().collect();
    InterfaceHeatmap {
        links: per_link.iter().map(|(l, _)| l.clone()).collect(),
        values: per_link
            .iter()
            .map(|(_, m)| {
                timestamps
                    .iter()
                    .map(|t| m.get(t).copied().unwrap_or(0.0))
                    .collect()
            })
            .collect(),
        timestamps,
    }
}

/// Busiest-links × time heatmap: picks the top interfaces by current throughput, then returns each
/// link's throughput (bits/sec) over time on a shared timestamp axis.
#[utoipa::path(
    get, path = "/api/v1/metrics/interface-heatmap", tag = "metrics",
    params(HeatmapQuery),
    responses(
        (status = 200, description = "A links × time throughput grid on one shared timestamp axis", body = InterfaceHeatmap),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks the read permission", body = super::error::ErrorBody),
    ),
)]
async fn interface_heatmap(
    _perm: RequireView,
    State(st): State<ApiState>,
    Query(q): Query<HeatmapQuery>,
) -> Json<InterfaceHeatmap> {
    let limit = q.limit.unwrap_or(8).clamp(1, 20);
    let to = q.to.unwrap_or_else(super::now_unix_s);
    let from = q.from.unwrap_or(to - 6 * 3600);
    let step = clamp_range_step(from, to, q.step.unwrap_or(600), 60);
    let top = st
        .store
        .top_interfaces(InterfaceTopMetric::Throughput, TopAgg::Now, limit)
        .await;
    let entries = build_interface_entries(&st, top).await;
    // The per-link throughput queries are independent (bounded ≤ 20), so fan them out concurrently
    // rather than awaiting one link at a time — one round-trip of latency instead of N.
    let ranges = futures::future::join_all(entries.iter().map(|e| {
        st.store
            .interface_throughput_range(e.node_id, e.ifindex, from, to, step)
    }))
    .await;
    Json(build_heatmap(&entries, ranges))
}

// ── Aggregate throughput ─────────────────────────────────────────────────────

/// Query for the aggregate-throughput range: `?from=&to=&step=` (default last 24h, 300s step).
#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub(super) struct ThroughputRangeQuery {
    from: Option<i64>,
    to: Option<i64>,
    step: Option<u64>,
}

/// Fleet aggregate ingress and egress, aligned onto one shared timestamp axis.
///
/// Both arrays are the same length as `timestamps`, with `null` where that side has no sample.
/// The chart draws two series against one x-axis, so aligning here rather than client-side is what
/// keeps a missing ingress point from silently shifting egress by one slot.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub(crate) struct ThroughputRange {
    pub timestamps: Vec<i64>,
    pub in_bps: Vec<Option<f64>>,
    pub out_bps: Vec<Option<f64>>,
}

/// Align two independently-sampled series onto the union of their timestamps. Pure, because the
/// alignment is the part that can be silently wrong.
fn align_throughput(in_pts: Vec<MetricPoint>, out_pts: Vec<MetricPoint>) -> ThroughputRange {
    let mut grid: std::collections::BTreeMap<i64, (Option<f64>, Option<f64>)> =
        std::collections::BTreeMap::new();
    for p in in_pts {
        grid.entry(p.t).or_default().0 = Some(p.v);
    }
    for p in out_pts {
        grid.entry(p.t).or_default().1 = Some(p.v);
    }
    ThroughputRange {
        timestamps: grid.keys().copied().collect(),
        in_bps: grid.values().map(|(i, _)| *i).collect(),
        out_bps: grid.values().map(|(_, o)| *o).collect(),
    }
}

/// Fleet aggregate ingress/egress (bits/sec) over time.
#[utoipa::path(
    get, path = "/api/v1/metrics/throughput-range", tag = "metrics",
    params(ThroughputRangeQuery),
    responses(
        (status = 200, description = "Fleet ingress and egress aligned on one timestamp axis", body = ThroughputRange),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks the read permission", body = super::error::ErrorBody),
    ),
)]
async fn throughput_range(
    _perm: RequireView,
    State(st): State<ApiState>,
    Query(q): Query<ThroughputRangeQuery>,
) -> Json<ThroughputRange> {
    let to = q.to.unwrap_or_else(super::now_unix_s);
    let from = q.from.unwrap_or(to - 24 * 3600);
    let step = clamp_range_step(from, to, q.step.unwrap_or(300), 60);
    let (in_pts, out_pts) = st.store.throughput_range(from, to, step).await;
    Json(align_throughput(in_pts, out_pts))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::router;
    use crate::api::tests_support::{private_state, public_state};
    use axum::body::{to_bytes, Body};
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    #[test]
    fn a_metric_name_that_is_not_an_identifier_never_reaches_the_selector() {
        // The rejection is the security boundary: this string is interpolated into PromQL, so
        // anything that could close the selector and append a clause must not get through.
        for bad in [
            "cpu; drop",
            "a\"}",
            "{__name__=~\".*\"}",
            "9starts_with_digit",
            "has space",
            "",
        ] {
            assert_eq!(
                check_metric_name(bad).expect_err("must reject").code(),
                "invalid_metric_name",
                "{bad:?} must not be accepted"
            );
        }
        for ok in ["icmp_rtt_ms", "cpu_percent", "_leading", "ns:metric"] {
            assert!(check_metric_name(ok).is_ok(), "{ok:?} must be accepted");
        }
    }

    #[test]
    fn a_logical_alias_expands_to_a_constant_selector_and_nothing_else_does() {
        // The alias path is the only one that produces a `{__name__=~…}` selector, and it is built
        // from constants — so an attacker cannot reach that shape by naming a metric.
        let cpu = top_selector("cpu").expect("cpu is a known alias");
        assert!(cpu.starts_with("{__name__=~\""), "{cpu}");
        assert!(cpu.contains("huawei_cpu_usage") && cpu.contains("hr_processor_load"));
        // A raw metric passes through verbatim, having been validated first.
        assert_eq!(top_selector("icmp_rtt_ms").unwrap(), "icmp_rtt_ms");
        assert_eq!(
            top_selector("{__name__=~\".*\"}")
                .expect_err("a hand-written selector is not a metric name")
                .code(),
            "invalid_metric_name"
        );
    }

    #[test]
    fn the_cpu_alias_excludes_idle_style_gauges() {
        // Ranking descending on an idle metric would put the healthiest nodes at the top of a
        // "busiest nodes" widget — a silently wrong answer rather than an error.
        let cpu = top_selector("cpu").unwrap();
        for wrong in ["idle", "temperature", "temp"] {
            assert!(!cpu.contains(wrong), "{cpu} must not rank on {wrong}");
        }
    }

    #[test]
    fn every_enumerated_param_rejects_rather_than_defaulting() {
        // Each of these selects a different store query, so falling back to a default would answer
        // a question the caller did not ask.
        assert_eq!(parse_top_agg(None).unwrap(), TopAgg::Now);
        assert_eq!(parse_top_agg(Some("now")).unwrap(), TopAgg::Now);
        assert_eq!(parse_top_agg(Some("max_1h")).unwrap(), TopAgg::Max1h);
        assert_eq!(
            parse_top_agg(Some("max_1d"))
                .expect_err("unknown agg")
                .code(),
            "invalid_agg"
        );
        assert!(parse_interface_metric("throughput").is_ok());
        assert_eq!(
            parse_interface_metric("bytes")
                .expect_err("unknown interface metric")
                .code(),
            "invalid_metric"
        );
        assert_eq!(invalid_agg("mean").code(), "invalid_agg");
    }

    #[tokio::test]
    async fn a_missing_reading_is_404_but_an_empty_window_is_200() {
        // The distinction the WebUI branches on: "this node does not collect that" versus "it
        // collects it and this slice happens to be empty". A chart draws the second as a gap.
        let app = router(public_state());
        let node = Uuid::nil();
        let missing = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/api/v1/nodes/{node}/metrics/never_collected"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);
        let bytes = to_bytes(missing.into_body(), 64 * 1024).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["error"]["code"], "metric_not_found");

        let range = app
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/api/v1/nodes/{node}/metrics/never_collected/range"
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(range.status(), StatusCode::OK);
    }

    #[test]
    fn throughput_alignment_pads_the_side_that_is_missing_a_sample() {
        // If the two series were returned unaligned, a chart pairing them by index would draw
        // egress against the wrong timestamps the moment one side dropped a scrape.
        let r = align_throughput(
            vec![MetricPoint { t: 10, v: 1.0 }, MetricPoint { t: 30, v: 3.0 }],
            vec![MetricPoint { t: 20, v: 2.0 }, MetricPoint { t: 30, v: 4.0 }],
        );
        assert_eq!(r.timestamps, vec![10, 20, 30]);
        assert_eq!(r.in_bps, vec![Some(1.0), None, Some(3.0)]);
        assert_eq!(r.out_bps, vec![None, Some(2.0), Some(4.0)]);
        assert_eq!(r.in_bps.len(), r.timestamps.len());
        assert_eq!(r.out_bps.len(), r.timestamps.len());

        let empty = align_throughput(Vec::new(), Vec::new());
        assert!(empty.timestamps.is_empty() && empty.in_bps.is_empty());
    }

    /// GET `path` against a public-dashboard skeleton, returning (status, body).
    async fn get_json(path: &str) -> (StatusCode, serde_json::Value) {
        let resp = router(public_state())
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = resp.status();
        let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    #[tokio::test]
    async fn a_fleet_ranking_the_store_cannot_answer_is_an_empty_list_not_an_error() {
        // The in-memory sink cannot rank a fleet. An empty ranking is the honest answer and the
        // one the dashboard widget can render; a 500 here would break the whole page.
        for path in [
            "/api/v1/metrics/top?metric=icmp_rtt_ms",
            "/api/v1/metrics/interface-top?metric=throughput",
        ] {
            let (status, body) = get_json(path).await;
            assert_eq!(status, StatusCode::OK, "{path}");
            assert_eq!(body, serde_json::json!([]), "{path}");
        }
    }

    #[tokio::test]
    async fn a_promql_injection_attempt_is_rejected_at_the_edge() {
        // The regression this pins: `up} or vector(1)` would close the selector and append a
        // clause, turning a Top-N into an arbitrary query. It must never reach the store.
        let (status, body) = get_json("/api/v1/metrics/top?metric=up}+or+vector(1)").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["code"], "invalid_metric_name");
    }

    #[tokio::test]
    async fn each_bad_enumerated_param_answers_its_own_error_code() {
        // The UI branches on these codes, so they must stay distinguishable end-to-end and not
        // collapse into a generic 400.
        for (path, code) in [
            (
                "/api/v1/metrics/top?metric=icmp_rtt_ms&agg=bogus",
                "invalid_agg",
            ),
            (
                "/api/v1/metrics/interface-top?metric=bogus",
                "invalid_metric",
            ),
            (
                "/api/v1/metrics/interface-delta?direction=sideways",
                "invalid_direction",
            ),
        ] {
            let (status, body) = get_json(path).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{path}");
            assert_eq!(body["error"]["code"], code, "{path}");
        }
    }

    #[tokio::test]
    async fn a_private_deployment_gates_every_metric_read() {
        let app = router(private_state());
        for path in [
            "/api/v1/metrics/top?metric=icmp_rtt_ms",
            "/api/v1/metrics/interface-top?metric=throughput",
            "/api/v1/metrics/interface-delta?direction=up",
            "/api/v1/metrics/throughput-range",
        ] {
            let resp = app
                .clone()
                .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::UNAUTHORIZED, "{path}");
        }
    }
}
