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

use super::extract::{Admin, RequireView, Scoped, VisibleNode};
use super::util::Ranked;
use super::{
    clamp_range_step, is_valid_metric_name, ApiError, ApiResult, ApiState, DEFAULT_RANGE_SECS,
    DEFAULT_RATE_LOOKBACK_SECS, DEFAULT_STEP_SECS,
};
use crate::store::{DeltaDirection, InterfaceTopMetric, MetricPoint, NodeSeries, TopAgg};
use axum::{
    extract::{Path, Query, State},
    routing::get,
    Json, Router,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use yagra_common::{CollectionItem, IfIndex, MetricKind, NodeId, SeriesKey};

/// This domain's slice of the OpenAPI document (ADR-035), merged by [`super::openapi::document`].
#[derive(utoipa::OpenApi)]
#[openapi(paths(
    list_node_metrics,
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
        .route("/api/v1/nodes/:node_id/metrics", get(list_node_metrics))
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

// ── One node's metric inventory (ADR-046) ────────────────────────────────────

// The inventory is a join of two sources that each lie on their own (ADR-046 decision 2): the
// collection set knows what a node is *told* to collect but not whether anything arrived, and the
// TSDB knows what arrived but not what was asked for. Collapsing the join into "here are some
// metrics" would make an empty answer mean three different things at once — the misreading ADR-045
// paid for once already, where a `0` that does not say *why* gets read as "safe".
//
// ⚠️ Everything below is a `///` on a `ToSchema` type, which utoipa publishes **verbatim** to API
// clients and to the public API reference. Design rationale belongs in `//` comments like this one.

/// Whether a metric is configured for collection, has data, or both.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MetricStatus {
    /// Configured for collection, and the TSDB has samples in the window.
    Ok,
    /// Configured for collection, but nothing has arrived in the window.
    NoData,
    /// Not in the node's collection set, but the TSDB has samples. Normal for metrics that do not
    /// come from a collection set at all — reachability, the URL and DNS monitors, the neighbour
    /// count, and values extracted from a monitored JSON response — and otherwise means an item was
    /// removed while its history remains.
    Unconfigured,
}

/// The dimension a metric's series carry, which decides how it can be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MetricDimension {
    /// One series per node — read it directly.
    None,
    /// One series per interface, each naming an interface this node has. Read those through the
    /// per-interface series endpoint.
    Interface,
    /// One series per table row. Row identity is lost when the values are collected, so these rows
    /// cannot be named — only aggregated node-wide with `agg=max`.
    Entity,
}

/// One metric on one node: what it is, whether it has data, and how it must be read.
#[derive(Debug, Clone, PartialEq, Serialize, utoipa::ToSchema)]
pub(crate) struct NodeMetricEntry {
    /// The TSDB metric name.
    pub metric: String,
    /// Gauge vs raw counter. A counter's stored value is an odometer reading, so chart it with
    /// `rate=true` rather than plotting it directly.
    pub metric_kind: MetricKind,
    pub dimension: MetricDimension,
    pub status: MetricStatus,
    /// How many series share this name on this node — the fan-out behind one entry.
    pub series_count: u32,
}

/// Fallback window for the inventory: how far back a metric may have last been seen and still count
/// as "has data". Wide enough that a slow (hourly) collection set is not reported as missing.
const DEFAULT_INVENTORY_WINDOW_SECS: u64 = 6 * 3600;

/// Ceiling on the inventory window. `/api/v1/series` widens with the range, so an unbounded window
/// is an unbounded TSDB scan — the same clamping discipline as [`clamp_range_step`].
const MAX_INVENTORY_WINDOW_SECS: u64 = 30 * 86_400;

/// How far back the inventory looks when deciding whether a metric has data.
#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub(super) struct InventoryQuery {
    within_secs: Option<u64>,
}

/// Resolve a metric's kind without re-spelling any catalog.
///
/// Order: the node's own resolved collection set, then the built-in catalog (which
/// [`yagra_common::builtin_metric_kind`] exists to answer without naming its metrics twice), then
/// gauge.
///
/// The gauge fallback is not a guess. `Sample::counter` has **no production call site** — counters
/// reach the TSDB only by way of a collection item's `metric_kind` — so a metric with no collection
/// item is a gauge. [`tests::counters_only_ever_arrive_through_a_collection_item`] fails if that
/// stops being true, because the fallback would then silently start plotting raw counters.
fn resolve_metric_kind(metric: &str, configured: &[CollectionItem]) -> MetricKind {
    configured
        .iter()
        .find(|i| i.metric_name == metric)
        .map(|i| i.metric_kind)
        .or_else(|| yagra_common::builtin_metric_kind(metric))
        .unwrap_or(MetricKind::Gauge)
}

/// Join the node's collection set against the series the TSDB actually holds.
///
/// Pure, and deliberately so: the handler around it needs a database and cannot be unit-tested,
/// while every decision worth getting right — the three statuses, the interface/entity split, the
/// metric-kind fallback — is in here.
///
/// `interfaces` is the node's known ifindexes. A metric whose ifindexes all resolve to real
/// interfaces is [`MetricDimension::Interface`]; one that carries ifindexes which do not is
/// [`MetricDimension::Entity`], the folded multi-index case whose rows cannot be named.
fn join_inventory(
    configured: &[CollectionItem],
    series: &[NodeSeries],
    interfaces: &[i32],
) -> Vec<NodeMetricEntry> {
    let by_name: std::collections::BTreeMap<&str, &NodeSeries> =
        series.iter().map(|s| (s.metric.as_str(), s)).collect();
    let known: std::collections::BTreeSet<i32> = interfaces.iter().copied().collect();

    // BTreeMap so the two sources merge without a second pass and the result is name-ordered.
    let mut out: std::collections::BTreeMap<&str, NodeMetricEntry> =
        std::collections::BTreeMap::new();
    for item in configured {
        let name = item.metric_name.as_str();
        let found = by_name.get(name);
        out.insert(
            name,
            NodeMetricEntry {
                metric: item.metric_name.clone(),
                metric_kind: item.metric_kind,
                dimension: found.map_or(
                    // No data yet, so the TSDB cannot say. The collection kind is the only
                    // evidence there is: a table walk yields one series per row.
                    match item.kind {
                        yagra_common::CollectionKind::Scalar => MetricDimension::None,
                        yagra_common::CollectionKind::Table => MetricDimension::Entity,
                    },
                    |s| dimension_of(s, &known),
                ),
                status: if found.is_some() {
                    MetricStatus::Ok
                } else {
                    MetricStatus::NoData
                },
                series_count: found.map_or(0, |s| s.series_count),
            },
        );
    }
    for s in series {
        out.entry(s.metric.as_str())
            .or_insert_with(|| NodeMetricEntry {
                metric: s.metric.clone(),
                metric_kind: resolve_metric_kind(&s.metric, configured),
                dimension: dimension_of(s, &known),
                status: MetricStatus::Unconfigured,
                series_count: s.series_count,
            });
    }
    out.into_values().collect()
}

/// A series' dimension, given the node's known interfaces.
fn dimension_of(s: &NodeSeries, known: &std::collections::BTreeSet<i32>) -> MetricDimension {
    if !s.has_ifindex {
        return MetricDimension::None;
    }
    // Every ifindex naming a real interface ⇒ per-interface. A metric with an unparseable ifindex
    // has `has_ifindex` set and an empty list, which lands on Entity — the safe side, since Entity
    // promises less (node aggregate only, rows unnameable).
    if !s.ifindexes.is_empty() && s.ifindexes.iter().all(|i| known.contains(i)) {
        MetricDimension::Interface
    } else {
        MetricDimension::Entity
    }
}

// Read permission, not config permission, and the split is deliberate: this response carries names,
// kinds and status, never an OID, an item id or a scope — and withholding the names would protect
// nothing anyway, since `GET /nodes/:node_id/metrics/:metric` already serves any name a viewer asks
// for. What `collection.rs`'s ManageConfig guard protects is the OIDs and the ability to edit them.
/// Every metric this node is configured to collect or has data for, with the status of each.
///
/// Answers what there is to look at for a node, including metrics that come from its checks rather
/// than from a collection set and so appear in no collection listing.
#[utoipa::path(
    get, path = "/api/v1/nodes/{node_id}/metrics", tag = "metrics",
    params(("node_id" = Uuid, Path, description = "Node id"), InventoryQuery),
    responses(
        (status = 200, description = "The node's metrics, name-ordered", body = Vec<NodeMetricEntry>),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks the read permission", body = super::error::ErrorBody),
        (status = 404, description = "No such node", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode: no write side", body = super::error::ErrorBody),
    ),
)]
async fn list_node_metrics(
    _perm: RequireView,
    _visible: VisibleNode,
    admin: Admin,
    State(st): State<ApiState>,
    Path(node_id): Path<Uuid>,
    Query(q): Query<InventoryQuery>,
) -> ApiResult<Json<Vec<NodeMetricEntry>>> {
    Ok(Json(
        node_metric_inventory(&admin, st.store.as_ref(), node_id, q.within_secs).await?,
    ))
}

/// A node's metric inventory — the seam both edges call.
///
/// Shared rather than reimplemented for MCP: the inventory *is* a join, and a second copy of it
/// would be a second set of rules for the three statuses and the interface/entity split. Those
/// rules are the whole feature; two surfaces answering the same question differently is the bug
/// ADR-042's parity rule exists to prevent.
pub(crate) async fn node_metric_inventory(
    admin: &super::AdminState,
    store: &dyn crate::store::MetricStore,
    node_id: Uuid,
    within_secs: Option<u64>,
) -> ApiResult<Vec<NodeMetricEntry>> {
    let within = within_secs
        .unwrap_or(DEFAULT_INVENTORY_WINDOW_SECS)
        .clamp(60, MAX_INVENTORY_WINDOW_SECS);
    // The resolved view, not the node's own overrides: what the poller actually collects is what
    // the operator is asking about. This is also what 404s an unknown node id.
    let configured = match super::collection::node_collection(admin, node_id, true).await? {
        super::collection::NodeCollection::Resolved(items) => items,
        // Unreachable — `node_collection(_, _, true)` always takes the resolved arm — but the union
        // is a type, so the impossible branch is stated rather than unwrapped.
        super::collection::NodeCollection::Stored(_) => Vec::new(),
    };
    let interfaces = admin
        .repo
        .list_interfaces(node_id)
        .await
        .map_err(|e| {
            ApiError::from_internal(e.as_ref(), "list interfaces", "failed to load interfaces")
        })?
        .into_iter()
        .map(|i| i.ifindex)
        .collect::<Vec<i32>>();
    let series = store.node_series(node_id, within).await;
    Ok(join_inventory(&configured, &series, &interfaces))
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

/// Reject `rate` combined with `agg` — there is no node-max-rate to serve.
///
/// A per-entity counter would need its rows differentiated and then collapsed, and the rows of a
/// folded multi-index table cannot be named in the first place (ADR-046 decision 5). Interface
/// counters, the case anyone actually wants, are already served per interface and as fleet
/// throughput. So this is refused at the edge rather than answered with something plausible.
fn rate_and_agg_together() -> ApiError {
    ApiError::bad_request(
        "rate_with_agg",
        "`rate` and `agg` cannot be combined; a per-entity counter has no node-level rate — read \
         its interfaces individually instead",
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
    _visible: VisibleNode,
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

/// Query params for the range endpoints that take a window and nothing else (interface series,
/// fleet throughput).
///
/// It used to carry `agg` as well, because the node-metric range shared it — and the other two
/// never read that field. The published contract therefore advertised an aggregation parameter on
/// two routes that discard it. Splitting [`NodeRangeQuery`] out is what made that visible.
#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub(super) struct RangeQuery {
    from: Option<i64>,
    to: Option<i64>,
    step: Option<u64>,
}

/// Query params for **the node-metric range only**, which additionally takes `rate`.
///
/// Split from [`RangeQuery`] rather than adding a field to it: the interface-series and
/// throughput-range routes share that type and would ignore `rate`, and a parameter the published
/// contract advertises on a route that discards it is worse than no parameter at all.
#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub(super) struct NodeRangeQuery {
    from: Option<i64>,
    to: Option<i64>,
    step: Option<u64>,
    /// `max` ⇒ node-level aggregate of a per-entity table gauge; absent ⇒ scalar node series.
    agg: Option<String>,
    // Counters are stored raw and differentiated at query time (ADR-012); plotting the stored
    // values gives an odometer, and taking `agg=max` of them gives a rising line that looks like
    // traffic and is not. Without this the WebUI had no way to chart a counter at all.
    /// `true` ⇒ per-second rate of a counter series instead of its stored values. Cannot be
    /// combined with `agg`.
    rate: Option<bool>,
}

#[utoipa::path(
    get, path = "/api/v1/nodes/{node_id}/metrics/{metric}/range", tag = "metrics",
    params(
        ("node_id" = Uuid, Path, description = "Node id"),
        ("metric" = String, Path, description = "Metric name — a Prometheus identifier"),
        NodeRangeQuery,
    ),
    responses(
        (status = 200, description = "The window's points; empty when the slice has no samples", body = MetricRange),
        (status = 400, description = "The metric name is not an identifier, `agg` is unsupported, or `rate` and `agg` were combined", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks the read permission", body = super::error::ErrorBody),
    ),
)]
async fn get_node_metric_range(
    _perm: RequireView,
    _visible: VisibleNode,
    State(st): State<ApiState>,
    Path((node_id, metric)): Path<(Uuid, String)>,
    Query(q): Query<NodeRangeQuery>,
) -> ApiResult<Json<MetricRange>> {
    check_metric_name(&metric)?;
    let node = NodeId::from(node_id);
    let to = q.to.unwrap_or_else(super::now_unix_s);
    let from = q.from.unwrap_or(to - DEFAULT_RANGE_SECS);
    // Clamping the step (rather than trusting it) bounds the point count, so a wide window cannot
    // be turned into an unbounded TSDB response by asking for a one-second step.
    let step = clamp_range_step(from, to, q.step.unwrap_or(DEFAULT_STEP_SECS), 1);
    let key = SeriesKey::node(node, metric.as_str());
    let rate = q.rate.unwrap_or(false);
    if rate && q.agg.is_some() {
        return Err(rate_and_agg_together());
    }
    let points = if rate {
        // Same lookback rule as the MCP `query_metrics(mode=rate)` branch, so the two surfaces
        // answer the same question with the same window.
        st.store
            .rate_range(&key, from, to, step, step.max(60))
            .await
    } else {
        match q.agg.as_deref() {
            Some("max") => st.store.aggregate_range(&key, from, to, step).await,
            Some(other) => return Err(invalid_agg(other)),
            None => st.store.range(&key, from, to, step).await,
        }
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
    _visible: VisibleNode,
    State(st): State<ApiState>,
    Path((node_id, ifindex)): Path<(Uuid, u32)>,
    Query(q): Query<RangeQuery>,
) -> Json<InterfaceSeries> {
    let to = q.to.unwrap_or_else(super::now_unix_s);
    let from = q.from.unwrap_or(to - DEFAULT_RANGE_SECS);
    Json(
        interface_series(
            &st,
            NodeId::from(node_id),
            IfIndex(ifindex),
            from,
            to,
            q.step,
        )
        .await,
    )
}

/// Sampling step and rate lookback for one interface's series.
///
/// Pure, and separated because it is the part that can be silently wrong: too coarse a step hides a
/// spike, and too short a lookback turns one missed poll into a hole in the line. ~120 points across
/// the window, with the lookback spanning a few steps.
pub(crate) fn interface_series_step(from: i64, to: i64, requested: Option<u64>) -> (u64, u64) {
    let span = u64::try_from((to - from).max(1)).unwrap_or(DEFAULT_RANGE_SECS as u64);
    let step = clamp_range_step(from, to, requested.unwrap_or((span / 120).max(60)), 1);
    (step, (step * 4).max(DEFAULT_RATE_LOOKBACK_SECS))
}

/// One interface's four aligned series (ADR-042 I1's `get_interface_series` reads this too).
///
/// The four metric names, the step/lookback rule and the ×8 bytes→bits scaling live here rather
/// than in the handler because a second surface needs the same answer, and reproducing it there
/// would mean a model (or a maintainer) having to know all three.
pub(crate) async fn interface_series(
    st: &ApiState,
    node: NodeId,
    ifindex: IfIndex,
    from: i64,
    to: i64,
    step: Option<u64>,
) -> InterfaceSeries {
    let key = |metric: &str| SeriesKey::interface(node, ifindex, metric);
    let (step, lookback) = interface_series_step(from, to, step);
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
    align_interface_series(&in_oct, &out_oct, &in_err, &out_err)
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
pub(crate) fn parse_top_agg(agg: Option<&str>) -> Result<TopAgg, ApiError> {
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
        (status = 200, description = "The highest-value nodes, ranked; `partial` says the scope filter may have shortened the list", body = super::util::Ranked<TopEntry>),
        (status = 400, description = "`metric` is neither a logical alias nor an identifier, or `agg` is unsupported", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks the read permission", body = super::error::ErrorBody),
    ),
)]
async fn top_metrics(
    _perm: RequireView,
    Scoped(scope): Scoped,
    State(st): State<ApiState>,
    Query(q): Query<TopQuery>,
) -> ApiResult<Json<Ranked<TopEntry>>> {
    Ok(Json(
        ranked_nodes(&st, &scope, &q.metric, q.agg.as_deref(), q.limit).await?,
    ))
}

/// The fleet node ranking: validate, over-fetch, scope-filter, join names.
///
/// Every bound lives here — the `metric` validation that keeps request input out of the PromQL, the
/// `1..=50` clamp, and the over-fetch — because ADR-042's `top_metrics` tool needs the same answer
/// and a clamp that lives at one edge is a clamp the other edge does not have.
pub(crate) async fn ranked_nodes(
    st: &ApiState,
    scope: &super::scope::NodeScope,
    metric: &str,
    agg: Option<&str>,
    limit: Option<usize>,
) -> ApiResult<Ranked<TopEntry>> {
    let selector = top_selector(metric)?;
    let agg = parse_top_agg(agg)?;
    let limit = limit.unwrap_or(5).clamp(1, 50);
    // The TSDB ranks by value and knows nothing about groups, so scoping happens after the ranking
    // — over-fetch first so a scoped caller usually still gets a full list. See RANKING_OVERFETCH.
    let fetched = super::scope::ranking_fetch_limit(scope, limit);
    let ranked = st.store.top_nodes(&selector, agg, fetched).await;
    let ranked: Vec<_> = ranked
        .into_iter()
        .filter(|(id, _)| scope.allows_node(st, yagra_common::NodeId::from(*id)))
        .collect();
    let ranked = Ranked::new(ranked, limit, fetched);
    let names =
        super::nodes::resolve_node_names(st, scope, ranked.entries.iter().map(|(id, _)| *id)).await;
    Ok(Ranked {
        entries: ranked
            .entries
            .into_iter()
            .map(|(id, value)| TopEntry {
                node_id: id,
                name: names.get(&id).cloned().unwrap_or_else(|| id.to_string()),
                value,
            })
            .collect(),
        partial: ranked.partial,
    })
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
pub(crate) fn parse_interface_metric(metric: &str) -> Result<InterfaceTopMetric, ApiError> {
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
        (status = 200, description = "The busiest or most-erroring interfaces, ranked; `partial` says the scope filter may have shortened the list", body = super::util::Ranked<InterfaceTopEntry>),
        (status = 400, description = "`metric` is not one of throughput|in_bps|out_bps|errors|discards, or `agg` is unsupported", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks the read permission", body = super::error::ErrorBody),
    ),
)]
async fn interface_top(
    _perm: RequireView,
    Scoped(scope): Scoped,
    State(st): State<ApiState>,
    Query(q): Query<InterfaceTopQuery>,
) -> ApiResult<Json<Ranked<InterfaceTopEntry>>> {
    let rank = InterfaceRanking::Metric(
        parse_interface_metric(&q.metric)?,
        parse_top_agg(q.agg.as_deref())?,
    );
    Ok(Json(ranked_interfaces(&st, &scope, rank, q.limit).await))
}

/// How an interface ranking is ordered.
///
/// Each variant carries exactly its own parameters, so there is no "ignored for this kind" field —
/// `agg` genuinely does not apply to a delta, and a window genuinely does not apply to a rate.
/// ADR-042's `top_interfaces` tool folds both endpoints behind one `rank_by` vocabulary, and this
/// is the type that vocabulary parses into.
#[derive(Debug, Clone, Copy)]
pub(crate) enum InterfaceRanking {
    /// Rank by a query-time rate expression.
    Metric(InterfaceTopMetric, TopAgg),
    /// Rank by signed throughput change over a window in seconds. Build it with
    /// [`InterfaceRanking::delta`] so the window is clamped.
    Delta(DeltaDirection, u64),
}

impl InterfaceRanking {
    /// A delta ranking with its comparison window clamped to `60..=3600`.
    ///
    /// A constructor rather than a bare variant because the clamp is the bound that keeps an
    /// unbounded window out of the TSDB, and both edges must get it.
    pub(crate) fn delta(direction: DeltaDirection, window_secs: Option<u64>) -> Self {
        Self::Delta(direction, window_secs.unwrap_or(300).clamp(60, 3600))
    }
}

/// Parse the interface-delta `direction` param.
///
/// Named for the same reason `parse_interface_metric` is: it selects which store query runs, so an
/// unrecognised value must be a rejection and not a default. It was an inline `match` in the handler
/// until a second surface needed the same vocabulary.
pub(crate) fn parse_delta_direction(direction: &str) -> Result<DeltaDirection, ApiError> {
    match direction {
        "up" => Ok(DeltaDirection::Up),
        "down" => Ok(DeltaDirection::Down),
        other => Err(ApiError::bad_request(
            "invalid_direction",
            format!("direction must be 'up' or 'down', got {other:?}"),
        )),
    }
}

/// The fleet interface ranking, over-fetched and scope-filtered, with names joined.
///
/// Holds the `1..=50` limit clamp and the over-fetch so neither edge can forget them.
pub(crate) async fn ranked_interfaces(
    st: &ApiState,
    scope: &super::scope::NodeScope,
    rank: InterfaceRanking,
    limit: Option<usize>,
) -> Ranked<InterfaceTopEntry> {
    let limit = limit.unwrap_or(6).clamp(1, 50);
    let fetched = super::scope::ranking_fetch_limit(scope, limit);
    let ranked = match rank {
        InterfaceRanking::Metric(metric, agg) => {
            st.store.top_interfaces(metric, agg, fetched).await
        }
        InterfaceRanking::Delta(direction, window) => {
            st.store.interface_delta(direction, window, fetched).await
        }
    };
    Ranked::new(
        build_interface_entries(st, scope, ranked).await,
        limit,
        fetched,
    )
}

/// Join a fleet interface ranking `(node, ifindex, value)` to node + interface names (and speed)
/// from PostgreSQL — one repo query over the distinct nodes in the result. Shared by the interface
/// Top-N and interface-delta endpoints.
///
/// **Scope-filters the ranking**, so every caller of this shared join gets it — an interface
/// ranking is node data, and there are three call sites that would each otherwise have to remember.
pub(crate) async fn build_interface_entries(
    st: &ApiState,
    scope: &super::scope::NodeScope,
    ranked: Vec<(Uuid, i32, f64)>,
) -> Vec<InterfaceTopEntry> {
    let ranked: Vec<(Uuid, i32, f64)> = ranked
        .into_iter()
        .filter(|(n, _, _)| scope.allows_node(st, yagra_common::NodeId::from(*n)))
        .collect();
    let node_ids: Vec<Uuid> = {
        let mut ids: Vec<Uuid> = ranked.iter().map(|(n, _, _)| *n).collect();
        ids.sort_unstable();
        ids.dedup();
        ids
    };
    let names = super::nodes::resolve_node_names(st, scope, node_ids.iter().copied()).await;
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
        (status = 200, description = "Interfaces ranked by signed throughput delta (bits/sec); `partial` says the scope filter may have shortened the list", body = super::util::Ranked<InterfaceTopEntry>),
        (status = 400, description = "`direction` is not 'up' or 'down'", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks the read permission", body = super::error::ErrorBody),
    ),
)]
async fn interface_delta(
    _perm: RequireView,
    Scoped(scope): Scoped,
    State(st): State<ApiState>,
    Query(q): Query<InterfaceDeltaQuery>,
) -> ApiResult<Json<Ranked<InterfaceTopEntry>>> {
    let rank = InterfaceRanking::delta(parse_delta_direction(&q.direction)?, q.window);
    Ok(Json(ranked_interfaces(&st, &scope, rank, q.limit).await))
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
    // Same flag as `util::Ranked::partial`, an added field rather than an envelope only because
    // this response was already an object. Doc comment kept outward-facing — it is published.
    /// `true` when the link set covers only the groups the calling account may see and links it is
    /// entitled to may be missing. Always `false` for an account with unrestricted visibility.
    pub partial: bool,
}

/// Build the grid from each link's independently-sampled series.
///
/// A gap becomes `0.0` rather than a hole: this is a *heatmap*, so every cell must have a value to
/// shade, and "no traffic recorded" is the honest reading of a missing throughput sample.
fn build_heatmap(
    entries: &[InterfaceTopEntry],
    ranges: Vec<Vec<MetricPoint>>,
    partial: bool,
) -> InterfaceHeatmap {
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
        partial,
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
    Scoped(scope): Scoped,
    State(st): State<ApiState>,
    Query(q): Query<HeatmapQuery>,
) -> Json<InterfaceHeatmap> {
    let limit = q.limit.unwrap_or(8).clamp(1, 20);
    let to = q.to.unwrap_or_else(super::now_unix_s);
    let from = q.from.unwrap_or(to - 6 * 3600);
    let step = clamp_range_step(from, to, q.step.unwrap_or(600), 60);
    let rank = InterfaceRanking::Metric(InterfaceTopMetric::Throughput, TopAgg::Now);
    let Ranked { entries, partial } = ranked_interfaces(&st, &scope, rank, Some(limit)).await;
    // The per-link throughput queries are independent (bounded ≤ 20), so fan them out concurrently
    // rather than awaiting one link at a time — one round-trip of latency instead of N.
    let ranges = futures::future::join_all(entries.iter().map(|e| {
        st.store
            .interface_throughput_range(e.node_id, e.ifindex, from, to, step)
    }))
    .await;
    Json(build_heatmap(&entries, ranges, partial))
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
    Scoped(scope): Scoped,
    State(st): State<ApiState>,
    Query(q): Query<ThroughputRangeQuery>,
) -> ApiResult<Json<ThroughputRange>> {
    Ok(Json(
        fleet_throughput(&st, &scope, q.from, q.to, q.step).await?,
    ))
}

/// Total fleet throughput over a window, refusal included.
///
/// The refusal is **inside** rather than at the call site on purpose: this is the shape a second
/// surface gets wrong by omission, and a fleet total handed to an account that sees seven nodes
/// reads as a fact about those seven.
pub(crate) async fn fleet_throughput(
    st: &ApiState,
    scope: &super::scope::NodeScope,
    from: Option<i64>,
    to: Option<i64>,
    step: Option<u64>,
) -> ApiResult<ThroughputRange> {
    // One summed series, aggregated inside VictoriaMetrics. Unlike the Top-N endpoints there is no
    // per-node breakdown to over-fetch and filter — the sum arrives already collapsed. Asking for
    // `sum by (node)` instead would return one series per node, which at the 50k-node target is the
    // cardinality blow-up CLAUDE.md names as the single biggest design risk. So it refuses.
    super::scope::require_fleet_wide(
        scope,
        "fleet throughput is summed inside the TSDB with no per-node breakdown to filter, so it \
         cannot be narrowed to a group-scoped account",
    )?;
    let to = to.unwrap_or_else(super::now_unix_s);
    let from = from.unwrap_or(to - 24 * 3600);
    let step = clamp_range_step(from, to, step.unwrap_or(300), 60);
    let (in_pts, out_pts) = st.store.throughput_range(from, to, step).await;
    Ok(align_throughput(in_pts, out_pts))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::router;
    use crate::api::tests_support::{private_state, public_state};
    use axum::body::{to_bytes, Body};
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    // ── The metric inventory (ADR-046) ───────────────────────────────────────

    fn item(metric: &str, kind: yagra_common::CollectionKind, mk: MetricKind) -> CollectionItem {
        CollectionItem {
            metric_name: metric.to_owned(),
            oid: "1.3.6.1".to_owned(),
            kind,
            metric_kind: mk,
        }
    }

    fn scalar_item(metric: &str) -> CollectionItem {
        item(
            metric,
            yagra_common::CollectionKind::Scalar,
            MetricKind::Gauge,
        )
    }

    fn have(metric: &str, ifindexes: &[i32], series_count: u32) -> NodeSeries {
        NodeSeries {
            metric: metric.to_owned(),
            has_ifindex: !ifindexes.is_empty(),
            series_count,
            ifindexes: ifindexes.to_vec(),
        }
    }

    fn by_name<'a>(rows: &'a [NodeMetricEntry], metric: &str) -> &'a NodeMetricEntry {
        rows.iter()
            .find(|r| r.metric == metric)
            .unwrap_or_else(|| panic!("{metric} missing from {rows:?}"))
    }

    #[test]
    fn the_inventory_separates_configured_no_data_from_data_with_no_config() {
        // The whole point of the join. Before it, "no metrics" meant three different things and the
        // surface could not tell an operator which one they were looking at.
        let configured = [scalar_item("configured_and_flowing"), scalar_item("silent")];
        let series = [
            have("configured_and_flowing", &[], 1),
            have("snmp_neighbor_count", &[], 1),
        ];
        let rows = join_inventory(&configured, &series, &[]);
        assert_eq!(rows.len(), 3);
        assert_eq!(
            by_name(&rows, "configured_and_flowing").status,
            MetricStatus::Ok
        );
        assert_eq!(by_name(&rows, "silent").status, MetricStatus::NoData);
        // The ADR-038 leftover: a metric emitted by a check spec has no collection item and would
        // be invisible to any surface driven by the collection set alone.
        assert_eq!(
            by_name(&rows, "snmp_neighbor_count").status,
            MetricStatus::Unconfigured
        );
    }

    #[test]
    fn an_ifindex_that_names_a_real_interface_is_per_interface_and_one_that_does_not_is_an_entity()
    {
        // The distinction the TSDB cannot make: the SNMP walker folds a multi-index table's
        // instance OID into a synthetic ifindex, so `ifindex` alone does not mean "interface".
        let series = [
            have("if_hc_in_octets", &[1, 2, 3], 3),
            have("huawei_cpu_usage", &[1_913_284_991], 1),
        ];
        let rows = join_inventory(&[], &series, &[1, 2, 3]);
        assert_eq!(
            by_name(&rows, "if_hc_in_octets").dimension,
            MetricDimension::Interface
        );
        assert_eq!(
            by_name(&rows, "huawei_cpu_usage").dimension,
            MetricDimension::Entity
        );
    }

    #[test]
    fn a_partly_unknown_ifindex_set_is_an_entity_not_an_interface() {
        // Erring the other way would promise a per-interface breakdown for rows that have no
        // interface — the surface would render names for entities that do not have any.
        let rows = join_inventory(&[], &[have("mixed", &[1, 999], 2)], &[1]);
        assert_eq!(rows[0].dimension, MetricDimension::Entity);
        // And an ifindex label that failed to parse leaves the list empty while the flag stands.
        let unparseable = NodeSeries {
            metric: "odd".to_owned(),
            has_ifindex: true,
            series_count: 1,
            ifindexes: Vec::new(),
        };
        assert_eq!(
            join_inventory(&[], &[unparseable], &[1])[0].dimension,
            MetricDimension::Entity
        );
    }

    #[test]
    fn a_configured_metric_with_no_data_still_states_the_shape_its_collection_kind_implies() {
        // No series to inspect, so the collection kind is the only evidence: a table walk yields one
        // series per row. Reporting `none` here would tell the caller to read it as a node scalar.
        let configured = [
            scalar_item("scalar_pending"),
            item(
                "table_pending",
                yagra_common::CollectionKind::Table,
                MetricKind::Gauge,
            ),
        ];
        let rows = join_inventory(&configured, &[], &[]);
        assert_eq!(
            by_name(&rows, "scalar_pending").dimension,
            MetricDimension::None
        );
        assert_eq!(
            by_name(&rows, "table_pending").dimension,
            MetricDimension::Entity
        );
        assert_eq!(by_name(&rows, "table_pending").series_count, 0);
    }

    #[test]
    fn an_unconfigured_metric_takes_its_kind_from_the_builtin_catalog_then_falls_back_to_gauge() {
        let series = [have("if_hc_in_octets", &[], 1), have("http_up", &[], 1)];
        let rows = join_inventory(&[], &series, &[]);
        // Removed from a template but still in the TSDB: the catalog still knows it is a counter,
        // so it is not charted as if the stored odometer reading were the value.
        assert_eq!(
            by_name(&rows, "if_hc_in_octets").metric_kind,
            MetricKind::Counter
        );
        assert_eq!(by_name(&rows, "http_up").metric_kind, MetricKind::Gauge);
    }

    #[test]
    fn the_node_s_own_item_outranks_the_builtin_catalog_for_metric_kind() {
        // A node-level override that redefines a catalog name must win, or the inventory would
        // describe the metric as the catalog does while the poller collects something else.
        let configured = [item(
            "if_hc_in_octets",
            yagra_common::CollectionKind::Scalar,
            MetricKind::Gauge,
        )];
        let rows = join_inventory(&configured, &[have("if_hc_in_octets", &[], 1)], &[]);
        assert_eq!(rows[0].metric_kind, MetricKind::Gauge);
    }

    /// The gauge fallback in [`resolve_metric_kind`] is only sound while counters can arrive
    /// **exclusively** through a collection item's `metric_kind`.
    ///
    /// If a poller ever emits a counter some other way, that metric has no collection item, falls
    /// back to gauge here, and the WebUI plots a raw counter as a value — the ADR-012 accident,
    /// arrived at through a default rather than a decision. Nothing else would notice.
    #[test]
    fn counters_only_ever_arrive_through_a_collection_item() {
        // `worker.rs` is the only file in the poller that builds a `Sample` at all, and the only
        // counter it can produce is `kind: col.kind` — copied from the collection column. Every
        // other sample it emits goes through `Sample::gauge`.
        let src = include_str!("../../../yagra-poller/src/worker.rs");
        // Production code only: fixtures below `#[cfg(test)]` may build counters freely. Needles
        // assembled at runtime so this test's own text cannot satisfy the search.
        let production = src.split("#[cfg(test)]").next().unwrap_or(src);
        let hardcoded = format!("MetricKind::{}", "Counter");
        assert!(
            !production.contains(&hardcoded),
            "the poller now hardcodes a counter kind outside the collection path — a metric with \
             no collection item would fall back to gauge in `resolve_metric_kind`, and the \
             inventory would chart a raw counter as if it were a value (the ADR-012 accident, \
             arrived at through a default rather than a decision)"
        );
        // The collection path itself is still there, so the test is not passing because the file
        // stopped producing counters altogether.
        assert!(production.contains(&format!("kind: col.{}", "kind")));
    }

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
        assert!(parse_delta_direction("up").is_ok());
        assert!(parse_delta_direction("down").is_ok());
        assert_eq!(
            parse_delta_direction("sideways")
                .expect_err("unknown direction")
                .code(),
            "invalid_direction"
        );
        assert_eq!(invalid_agg("mean").code(), "invalid_agg");
    }

    #[test]
    fn the_interface_series_step_stays_sampleable_and_the_lookback_spans_it() {
        // ~120 points across the window is the target, and the lookback must span several steps or
        // one missed poll punches a hole in the line. Both are cheap to get subtly wrong and
        // invisible in the response, which is why they are pulled out as a pure function.
        let hour = (0, 3600);
        let (step, lookback) = interface_series_step(hour.0, hour.1, None);
        assert_eq!(step, 60, "an hour at ~120 points floors at the 60s minimum");
        assert!(lookback >= step * 4, "the lookback spans several steps");

        let day = (0, 86_400);
        let (step, lookback) = interface_series_step(day.0, day.1, None);
        assert_eq!(step, 720, "86400/120");
        assert_eq!(lookback, step * 4);

        // A caller asking for a one-second step over a wide window would otherwise turn one request
        // into an unbounded TSDB response; `clamp_range_step` is what stops that, and it must stay
        // inside this function rather than at the edge — ADR-042's tool has no edge to put it at.
        let (clamped, _) = interface_series_step(0, 86_400, Some(1));
        assert!(clamped > 1, "a one-second step over a day is clamped");
    }

    #[test]
    fn a_delta_ranking_clamps_its_window_however_it_is_built() {
        // The clamp lives in the constructor because both the REST edge and the MCP tool build one,
        // and a bound that lives at one edge is a bound the other edge does not have.
        let InterfaceRanking::Delta(_, w) = InterfaceRanking::delta(DeltaDirection::Up, Some(1))
        else {
            panic!("delta")
        };
        assert_eq!(w, 60);
        let InterfaceRanking::Delta(_, w) =
            InterfaceRanking::delta(DeltaDirection::Down, Some(999_999))
        else {
            panic!("delta")
        };
        assert_eq!(w, 3600);
        let InterfaceRanking::Delta(_, w) = InterfaceRanking::delta(DeltaDirection::Up, None)
        else {
            panic!("delta")
        };
        assert_eq!(w, 300);
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
            // The `Ranked` envelope, and `partial:false`: nothing was filtered away, the store just
            // had nothing to rank. Reporting `true` here would put a "may be incomplete" warning on
            // every widget of a deployment that simply has no data yet.
            assert_eq!(
                body,
                serde_json::json!({ "entries": [], "partial": false }),
                "{path}"
            );
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
    async fn combining_rate_and_agg_is_refused_with_its_own_code() {
        // Not a generic 400: the UI has to be able to tell "you asked for something that does not
        // exist" apart from "your metric name was rejected".
        let node = Uuid::nil();
        let (status, body) = get_json(&format!(
            "/api/v1/nodes/{node}/metrics/if_hc_in_octets/range?rate=true&agg=max"
        ))
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["code"], "rate_with_agg");
        // Each on its own is still fine — the refusal is about the pair, not about `rate`.
        for q in ["rate=true", "agg=max", ""] {
            let (status, _) = get_json(&format!(
                "/api/v1/nodes/{node}/metrics/if_hc_in_octets/range?{q}"
            ))
            .await;
            assert_eq!(status, StatusCode::OK, "{q}");
        }
    }

    #[tokio::test]
    async fn the_inventory_authenticates_before_it_reports_that_there_is_no_write_side() {
        // Guard order (`Require*` then `Admin`): an anonymous caller must learn only that it is
        // unauthenticated, never which subsystems this deployment has configured.
        let node = Uuid::nil();
        let resp = router(private_state())
            .oneshot(
                Request::builder()
                    .uri(format!("/api/v1/nodes/{node}/metrics"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        // On a skeleton deployment the read permission is open, so the 503 is what is left — and it
        // is the *typed* one, so the UI can say "no write side" rather than "something broke".
        let (status, body) = get_json(&format!("/api/v1/nodes/{node}/metrics")).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body["error"]["code"], "admin_unavailable");
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
