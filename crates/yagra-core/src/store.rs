//! The metric-store seam: where poll results are written and read back.
//!
//! [`MetricStore`] abstracts the TSDB so the API and the result consumer don't care
//! whether they talk to VictoriaMetrics ([`VmStore`], live) or the in-memory skeleton
//! ([`InMemorySink`]). Writes use VictoriaMetrics' Prometheus import endpoint with the
//! thin-label exposition line (ADR-011); reads use an instant `query`. Raw counters are
//! stored as-is; rates are derived at query time (ADR-012).

use async_trait::async_trait;
use serde::Serialize;
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;
use yagra_bus::PollResult;
use yagra_common::SeriesKey;

use crate::sink::InMemorySink;

/// How a fleet Top-N collapses each node's series to a single rankable value over the lookback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TopAgg {
    /// The most recent value within the instant lookback window (the value "now").
    Now,
    /// The peak value over the trailing hour (sustained hotspots, robust to a momentary dip).
    Max1h,
}

/// A fleet interface Top-N dimension. Each maps to a fixed, safe PromQL rate expression over the
/// thin-label interface counters (never user-interpolated) — so the API edge only validates the
/// enum, not a raw metric string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InterfaceTopMetric {
    /// Total throughput in+out (bits/sec).
    Throughput,
    /// Inbound throughput (bits/sec).
    InBps,
    /// Outbound throughput (bits/sec).
    OutBps,
    /// Errors in+out (per second).
    Errors,
    /// Discards in+out (per second).
    Discards,
}

/// Direction for the interface rate-delta (traffic spikes vs drops).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeltaDirection {
    /// Largest positive change (spikes).
    Up,
    /// Largest negative change (drops).
    Down,
}

/// One point of a time series: Unix-seconds timestamp and value.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MetricPoint {
    /// Unix timestamp in seconds.
    pub t: i64,
    /// Sample value.
    pub v: f64,
}

/// Somewhere poll results are written and read back.
#[async_trait]
pub trait MetricStore: Send + Sync {
    /// Persist every sample in a completed poll.
    async fn write(&self, result: &PollResult);
    /// The latest value for a series, if any.
    async fn latest(&self, key: &SeriesKey) -> Option<f64>;
    /// Sampled values for a series over `[from_s, to_s]` at `step_s` resolution
    /// (oldest first). Empty if the store has no history (e.g. the in-memory sink).
    async fn range(&self, key: &SeriesKey, from_s: i64, to_s: i64, step_s: u64)
        -> Vec<MetricPoint>;
    /// Per-second rate of a counter series over the trailing `lookback_s` window, derived
    /// at query time (ADR-012) — the TSDB's `rate()` handles counter wrap/reset, so the
    /// poller never does counter arithmetic. `None` if the store has no history.
    async fn rate(&self, key: &SeriesKey, lookback_s: u64) -> Option<f64>;
    /// Per-second rate of a counter series sampled across `[from_s, to_s]` at `step_s`
    /// resolution (oldest first), each point a `rate(...[lookback_s])`. For charting a
    /// counter as a rate over time. Empty if the store has no history.
    async fn rate_range(
        &self,
        key: &SeriesKey,
        from_s: i64,
        to_s: i64,
        step_s: u64,
        lookback_s: u64,
    ) -> Vec<MetricPoint>;
    /// Latest **node-level aggregate** (`max` across the metric's table/entity series) for the
    /// key's metric, robust to poll jitter. Collapses a per-entity table gauge (e.g. CPU% per
    /// `entPhysicalIndex`, one series per core) into a single node value — the idle entities
    /// reporting 0 drop out. `None` if the store has no such series.
    async fn aggregate_latest(&self, key: &SeriesKey) -> Option<f64>;
    /// Sampled node-level aggregate (`max`) over `[from_s, to_s]` at `step_s` resolution
    /// (oldest first). For charting a per-entity gauge as one node series. Empty if no history.
    async fn aggregate_range(
        &self,
        key: &SeriesKey,
        from_s: i64,
        to_s: i64,
        step_s: u64,
    ) -> Vec<MetricPoint>;
    /// The `limit` nodes with the highest value for `metric`, fleet-wide, as `(node_id, value)`
    /// pairs sorted descending. Any per-entity/per-interface series is collapsed to a node max,
    /// so the ranking is by node and the result carries only the node id (names are joined from
    /// PostgreSQL at the API edge, ADR-011). `metric` is a validated identifier OR a
    /// caller-built `{__name__=~"…"}` selector (logical CPU/memory alias). Empty if the store has
    /// no such data (e.g. the in-memory sink).
    async fn top_nodes(&self, metric: &str, agg: TopAgg, limit: usize) -> Vec<(Uuid, f64)>;
    /// The `limit` interfaces with the highest value for `metric`, fleet-wide, as
    /// `(node_id, ifindex, value)` sorted descending. Ranks by query-time `rate()` of the thin
    /// interface counters; names (node + if_name/if_alias) are joined from PostgreSQL at the edge.
    /// Empty if the store has no such data.
    async fn top_interfaces(
        &self,
        metric: InterfaceTopMetric,
        agg: TopAgg,
        limit: usize,
    ) -> Vec<(Uuid, i32, f64)>;
    /// The node ids that have reported `metric` within the trailing `within_secs` window — the
    /// "fresh" set for fleet data-coverage. The API edge diffs this against the inventory to find
    /// stale (silent) nodes. Empty if the store has no such data.
    async fn fresh_node_ids(&self, metric: &str, within_secs: u64) -> Vec<Uuid>;
    /// The `limit` interfaces whose total throughput changed the most vs `window_secs` ago, as
    /// `(node_id, ifindex, signed_delta_bps)` ordered by magnitude. `Up` ⇒ biggest increases
    /// (spikes), `Down` ⇒ biggest decreases (drops). Empty if the store has no such data.
    async fn interface_delta(
        &self,
        direction: DeltaDirection,
        window_secs: u64,
        limit: usize,
    ) -> Vec<(Uuid, i32, f64)>;
    /// Fleet aggregate throughput: the sum of all interface in/out rates (×8 = bits/sec) sampled
    /// across `[from_s, to_s]` at `step_s`, as `(in_bps_points, out_bps_points)` oldest-first.
    /// Empty if the store has no history.
    async fn throughput_range(
        &self,
        from_s: i64,
        to_s: i64,
        step_s: u64,
    ) -> (Vec<MetricPoint>, Vec<MetricPoint>);
    /// Total throughput (in+out rate ×8 = bits/sec) for one interface over `[from_s, to_s]` at
    /// `step_s`, oldest first. For the per-link utilization heatmap. Empty if no history.
    async fn interface_throughput_range(
        &self,
        node: Uuid,
        ifindex: i32,
        from_s: i64,
        to_s: i64,
        step_s: u64,
    ) -> Vec<MetricPoint>;
    /// The distinct metric names that have at least one sample for `node` within the trailing
    /// `within_secs` window — i.e. which series actually have data for the node (used by the
    /// Troubleshoot analyses to discover what to scan). Empty if the store can't enumerate
    /// (the in-memory sink). Names are deduplicated; order is unspecified.
    async fn node_metric_names(&self, node: Uuid, within_secs: u64) -> Vec<String>;
}

#[async_trait]
impl MetricStore for InMemorySink {
    async fn write(&self, result: &PollResult) {
        self.ingest(result);
    }

    async fn latest(&self, key: &SeriesKey) -> Option<f64> {
        // Inherent (sync) method — disambiguated from this trait method.
        InMemorySink::latest(self, key)
    }

    async fn range(
        &self,
        _key: &SeriesKey,
        _from_s: i64,
        _to_s: i64,
        _step_s: u64,
    ) -> Vec<MetricPoint> {
        // The skeleton sink keeps only the latest value, so it has no history to serve.
        Vec::new()
    }

    async fn rate(&self, _key: &SeriesKey, _lookback_s: u64) -> Option<f64> {
        // No history ⇒ no rate.
        None
    }

    async fn rate_range(
        &self,
        _key: &SeriesKey,
        _from_s: i64,
        _to_s: i64,
        _step_s: u64,
        _lookback_s: u64,
    ) -> Vec<MetricPoint> {
        // No history ⇒ no rate series.
        Vec::new()
    }

    async fn aggregate_latest(&self, _key: &SeriesKey) -> Option<f64> {
        // No table/entity series ⇒ no aggregate.
        None
    }

    async fn aggregate_range(
        &self,
        _key: &SeriesKey,
        _from_s: i64,
        _to_s: i64,
        _step_s: u64,
    ) -> Vec<MetricPoint> {
        // No history ⇒ no aggregate series.
        Vec::new()
    }

    async fn top_nodes(&self, _metric: &str, _agg: TopAgg, _limit: usize) -> Vec<(Uuid, f64)> {
        // The skeleton sink holds a single demo series, not a fleet — nothing to rank.
        Vec::new()
    }

    async fn top_interfaces(
        &self,
        _metric: InterfaceTopMetric,
        _agg: TopAgg,
        _limit: usize,
    ) -> Vec<(Uuid, i32, f64)> {
        Vec::new()
    }

    async fn fresh_node_ids(&self, _metric: &str, _within_secs: u64) -> Vec<Uuid> {
        Vec::new()
    }

    async fn interface_delta(
        &self,
        _direction: DeltaDirection,
        _window_secs: u64,
        _limit: usize,
    ) -> Vec<(Uuid, i32, f64)> {
        Vec::new()
    }

    async fn throughput_range(
        &self,
        _from_s: i64,
        _to_s: i64,
        _step_s: u64,
    ) -> (Vec<MetricPoint>, Vec<MetricPoint>) {
        (Vec::new(), Vec::new())
    }

    async fn interface_throughput_range(
        &self,
        _node: Uuid,
        _ifindex: i32,
        _from_s: i64,
        _to_s: i64,
        _step_s: u64,
    ) -> Vec<MetricPoint> {
        Vec::new()
    }

    async fn node_metric_names(&self, _node: Uuid, _within_secs: u64) -> Vec<String> {
        // The skeleton sink holds a single demo series and can't enumerate per node.
        Vec::new()
    }
}

/// A [`MetricStore`] backed by VictoriaMetrics over HTTP.
pub struct VmStore {
    http: reqwest::Client,
    base: String,
}

impl VmStore {
    /// Point at a VictoriaMetrics base URL (e.g. `http://victoriametrics:8428`).
    #[must_use]
    pub fn new(base: impl Into<String>) -> Self {
        Self {
            http: reqwest::Client::new(),
            base: base.into(),
        }
    }

    /// Run a PromQL `query_range` and parse the first series' points (oldest first). Shared
    /// by the raw `range` and the derived `rate_range`. Empty on any request/parse failure.
    async fn query_range_points(
        &self,
        query: String,
        from_s: i64,
        to_s: i64,
        step_s: u64,
    ) -> Vec<MetricPoint> {
        let url = format!("{}/api/v1/query_range", self.base);
        let resp = match self
            .http
            .get(&url)
            .query(&[
                ("query", query),
                ("start", from_s.to_string()),
                ("end", to_s.to_string()),
                ("step", format!("{step_s}s")),
            ])
            .send()
            .await
        {
            Ok(resp) => resp,
            Err(e) => {
                tracing::warn!(error = %e, "VictoriaMetrics query_range request failed");
                return Vec::new();
            }
        };
        let Ok(json) = resp.json::<serde_json::Value>().await else {
            return Vec::new();
        };
        // data.result[0].values is [[<ts_seconds>, "<value>"], …].
        let Some(values) = json
            .get("data")
            .and_then(|d| d.get("result"))
            .and_then(|r| r.get(0))
            .and_then(|s| s.get("values"))
            .and_then(|v| v.as_array())
        else {
            return Vec::new();
        };
        values
            .iter()
            .filter_map(|pair| {
                let arr = pair.as_array()?;
                // VM returns the timestamp as a float second; round rather than truncate so a
                // sub-second fraction doesn't shift the point back by up to a second.
                let t = arr.first()?.as_f64()?.round() as i64;
                let v = arr.get(1)?.as_str()?.parse::<f64>().ok()?;
                Some(MetricPoint { t, v })
            })
            .collect()
    }
}

/// PromQL instant-vector selector for a thin-label series, e.g.
/// `icmp_rtt_ms{node="…"}` (plus `ifindex` for per-interface series).
fn selector(key: &SeriesKey) -> String {
    match key.ifindex {
        Some(idx) => format!(
            "{}{{node=\"{}\",ifindex=\"{}\"}}",
            key.metric, key.node, idx
        ),
        None => format!("{}{{node=\"{}\"}}", key.metric, key.node),
    }
}

/// PromQL `rate()` query over the selector for a trailing window, e.g.
/// `rate(if_hc_in_octets{node="…",ifindex="3"}[300s])`. Used for both the instant `rate`
/// and the `rate_range` series (the range form samples this expression across the window).
fn rate_query(key: &SeriesKey, lookback_s: u64) -> String {
    format!("rate({}[{}s])", selector(key), lookback_s.max(1))
}

/// How far back an *instant* read (`latest`/`rate`) looks for the most recent value. SNMP
/// polls are jittered and some devices answer slowly, so a bare instant query at `now` often
/// falls outside VictoriaMetrics' default staleness window and returns nothing — blanking the
/// UI even though recent data exists. Wrapping in `last_over_time` over this window returns
/// the most recent value within it (the `stale` flag still dims genuinely-old rows).
const INSTANT_LOOKBACK_SECS: u64 = 1800;

/// Instant query for the latest value within [`INSTANT_LOOKBACK_SECS`], robust to poll gaps:
/// `last_over_time(icmp_rtt_ms{node="…"}[1800s])`.
fn latest_query(key: &SeriesKey) -> String {
    format!(
        "last_over_time({}[{}s])",
        selector(key),
        INSTANT_LOOKBACK_SECS
    )
}

/// Instant query for the most recent rate within [`INSTANT_LOOKBACK_SECS`] — a subquery that
/// samples `rate(…[lookback])` across the window and takes the last point, so a slightly
/// stale series still yields its last computed rate rather than nothing.
fn instant_rate_query(key: &SeriesKey, lookback_s: u64) -> String {
    format!(
        "last_over_time(({})[{}s:{}s])",
        rate_query(key, lookback_s),
        INSTANT_LOOKBACK_SECS,
        lookback_s.max(1)
    )
}

/// Node-scoped selector that ignores any `ifindex` dimension — `metric{node="…"}` regardless
/// of the key's ifindex. Used by the aggregate reads so a table gauge (one series per
/// entity/core) collapses to a single node-level value via `max()`.
fn node_scope_selector(key: &SeriesKey) -> String {
    format!("{}{{node=\"{}\"}}", key.metric, key.node)
}

/// Instant node-level MAX across entity series within the lookback:
/// `max(last_over_time(metric{node="…"}[1800s]))`. Inner `last_over_time` keeps the
/// poll-jitter robustness of [`latest_query`]; outer `max` collapses the entities.
fn aggregate_latest_query(key: &SeriesKey) -> String {
    format!(
        "max(last_over_time({}[{}s]))",
        node_scope_selector(key),
        INSTANT_LOOKBACK_SECS
    )
}

/// Range node-level MAX across entity series at each step: `max(metric{node="…"})`, sampled
/// by `query_range` (consistent with the bare-expression `range`/`rate_range` path).
fn aggregate_range_query(key: &SeriesKey) -> String {
    format!("max({})", node_scope_selector(key))
}

/// Trailing window for the `Max1h` Top-N aggregate.
const MAX_WINDOW_SECS: u64 = 3600;

/// Fleet Top-N PromQL: rank nodes by their value for `metric`. `max by (node)` collapses any
/// per-entity/per-interface series to one value per node (and drops every label but `node`, so
/// the result is keyed cleanly by node id); `topk` keeps the highest `limit`. `Now` reads the
/// most recent value within the instant lookback (jitter-robust); `Max1h` takes the hourly peak.
/// `metric` is a caller-validated identifier (`is_valid_metric_name` at the API edge), so it is
/// safe to interpolate into the selector.
fn topk_query(metric: &str, agg: TopAgg, limit: usize) -> String {
    let n = limit.clamp(1, 100);
    match agg {
        TopAgg::Now => {
            format!("topk({n}, max by (node) (last_over_time({metric}[{INSTANT_LOOKBACK_SECS}s])))")
        }
        TopAgg::Max1h => {
            format!("topk({n}, max by (node) (max_over_time({metric}[{MAX_WINDOW_SECS}s])))")
        }
    }
}

/// Rate lookback for interface Top-N (matches the interface-list/series default).
const INTERFACE_RATE_LOOKBACK_SECS: u64 = 300;

/// The fixed rate expression for an interface Top-N dimension over the thin counters. `in`+`out`
/// rates share `(node,ifindex)` labels, so vector addition aligns per interface. Octet rates are
/// scaled ×8 to bits/sec.
fn interface_expr(metric: InterfaceTopMetric) -> String {
    let w = INTERFACE_RATE_LOOKBACK_SECS;
    match metric {
        InterfaceTopMetric::Throughput => {
            format!("(rate(if_hc_in_octets[{w}s]) + rate(if_hc_out_octets[{w}s])) * 8")
        }
        InterfaceTopMetric::InBps => format!("rate(if_hc_in_octets[{w}s]) * 8"),
        InterfaceTopMetric::OutBps => format!("rate(if_hc_out_octets[{w}s]) * 8"),
        InterfaceTopMetric::Errors => {
            format!("(rate(if_in_errors[{w}s]) + rate(if_out_errors[{w}s]))")
        }
        InterfaceTopMetric::Discards => {
            format!("(rate(if_in_discards[{w}s]) + rate(if_out_discards[{w}s]))")
        }
    }
}

/// Fleet interface Top-N PromQL: rank `(node,ifindex)` by the rate expression. `Now` takes the
/// most recent value within the instant lookback (jitter-robust, like `instant_rate_query`);
/// `Max1h` the trailing-hour peak. The result carries only `node`+`ifindex` labels.
fn topk_interface_query(metric: InterfaceTopMetric, agg: TopAgg, limit: usize) -> String {
    let n = limit.clamp(1, 100);
    let expr = interface_expr(metric);
    let w = INTERFACE_RATE_LOOKBACK_SECS;
    let instant = match agg {
        TopAgg::Now => format!("last_over_time(({expr})[{INSTANT_LOOKBACK_SECS}s:{w}s])"),
        TopAgg::Max1h => format!("max_over_time(({expr})[{MAX_WINDOW_SECS}s:{w}s])"),
    };
    format!("topk({n}, max by (node,ifindex) ({instant}))")
}

/// Fleet interface rate-delta PromQL: per `(node,ifindex)`, total throughput now minus the rate
/// `window_secs` ago (×8 = bits/sec). `Up` keeps the biggest increases (`topk`), `Down` the
/// biggest decreases (`bottomk`). The two `rate()`s share `(node,ifindex)` labels so the
/// subtraction aligns per interface.
fn interface_delta_query(direction: DeltaDirection, window_secs: u64, limit: usize) -> String {
    let n = limit.clamp(1, 100);
    let w = window_secs.max(1);
    let delta = format!(
        "((rate(if_hc_in_octets[{w}s]) + rate(if_hc_out_octets[{w}s])) - \
          (rate(if_hc_in_octets[{w}s] offset {w}s) + rate(if_hc_out_octets[{w}s] offset {w}s))) * 8"
    );
    match direction {
        DeltaDirection::Up => format!("topk({n}, {delta})"),
        DeltaDirection::Down => format!("bottomk({n}, {delta})"),
    }
}

/// Parse a VM instant-query response into `(node_id, ifindex, value)` triples (node + ifindex
/// labels). Skips series with an unparseable node/ifindex/value. Sorted highest-first.
fn parse_top_interfaces(json: &serde_json::Value) -> Vec<(Uuid, i32, f64)> {
    let Some(results) = json
        .get("data")
        .and_then(|d| d.get("result"))
        .and_then(|r| r.as_array())
    else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(results.len());
    for series in results {
        let m = series.get("metric");
        let node = m
            .and_then(|m| m.get("node"))
            .and_then(|n| n.as_str())
            .and_then(|s| Uuid::parse_str(s).ok());
        let ifindex = m
            .and_then(|m| m.get("ifindex"))
            .and_then(|i| i.as_str())
            .and_then(|s| s.parse::<i32>().ok());
        let value = series
            .get("value")
            .and_then(|v| v.get(1))
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<f64>().ok());
        if let (Some(node), Some(ifindex), Some(value)) = (node, ifindex, value) {
            out.push((node, ifindex, value));
        }
    }
    out.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
    out
}

/// Parse a VictoriaMetrics instant-query response into `(node_id, value)` pairs: each
/// `data.result[]` series contributes its `metric.node` label (parsed as a UUID) and
/// `value[1]` (the string-encoded sample). Series without a parseable node label or value are
/// skipped. Split out for unit-testing the parse without a live TSDB.
fn parse_top_nodes(json: &serde_json::Value) -> Vec<(Uuid, f64)> {
    let Some(results) = json
        .get("data")
        .and_then(|d| d.get("result"))
        .and_then(|r| r.as_array())
    else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(results.len());
    for series in results {
        let node = series
            .get("metric")
            .and_then(|m| m.get("node"))
            .and_then(|n| n.as_str())
            .and_then(|s| Uuid::parse_str(s).ok());
        let value = series
            .get("value")
            .and_then(|v| v.get(1))
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<f64>().ok());
        if let (Some(node), Some(value)) = (node, value) {
            out.push((node, value));
        }
    }
    // VM returns topk highest-first, but sort defensively so callers can rely on the order.
    out.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    out
}

#[async_trait]
impl MetricStore for VmStore {
    async fn write(&self, result: &PollResult) {
        if result.samples.is_empty() {
            return;
        }
        // VictoriaMetrics ingests Prometheus exposition lines with a trailing ms timestamp.
        let mut body = String::new();
        for sample in &result.samples {
            let key = sample.series_key(result.node_id);
            body.push_str(&key.prometheus_line(sample.value, result.at_unix_ms));
            body.push('\n');
        }
        let url = format!("{}/api/v1/import/prometheus", self.base);
        match self.http.post(&url).body(body).send().await {
            Ok(resp) if !resp.status().is_success() => {
                tracing::warn!(status = %resp.status(), "VictoriaMetrics import non-2xx");
            }
            Err(e) => tracing::warn!(error = %e, "VictoriaMetrics import request failed"),
            Ok(_) => {}
        }
    }

    async fn latest(&self, key: &SeriesKey) -> Option<f64> {
        let url = format!("{}/api/v1/query", self.base);
        let resp = self
            .http
            .get(&url)
            .query(&[("query", latest_query(key))])
            .send()
            .await
            .ok()?;
        let json: serde_json::Value = resp.json().await.ok()?;
        // data.result[0].value[1] is the sample value as a string.
        let raw = json
            .get("data")?
            .get("result")?
            .get(0)?
            .get("value")?
            .get(1)?
            .as_str()?;
        raw.parse().ok()
    }

    async fn range(
        &self,
        key: &SeriesKey,
        from_s: i64,
        to_s: i64,
        step_s: u64,
    ) -> Vec<MetricPoint> {
        self.query_range_points(selector(key), from_s, to_s, step_s)
            .await
    }

    async fn rate(&self, key: &SeriesKey, lookback_s: u64) -> Option<f64> {
        let url = format!("{}/api/v1/query", self.base);
        let resp = self
            .http
            .get(&url)
            .query(&[("query", instant_rate_query(key, lookback_s))])
            .send()
            .await
            .ok()?;
        let json: serde_json::Value = resp.json().await.ok()?;
        // data.result[0].value[1] is the rate value as a string (empty result ⇒ None).
        let raw = json
            .get("data")?
            .get("result")?
            .get(0)?
            .get("value")?
            .get(1)?
            .as_str()?;
        raw.parse().ok()
    }

    async fn rate_range(
        &self,
        key: &SeriesKey,
        from_s: i64,
        to_s: i64,
        step_s: u64,
        lookback_s: u64,
    ) -> Vec<MetricPoint> {
        self.query_range_points(rate_query(key, lookback_s), from_s, to_s, step_s)
            .await
    }

    async fn aggregate_latest(&self, key: &SeriesKey) -> Option<f64> {
        let url = format!("{}/api/v1/query", self.base);
        let resp = self
            .http
            .get(&url)
            .query(&[("query", aggregate_latest_query(key))])
            .send()
            .await
            .ok()?;
        let json: serde_json::Value = resp.json().await.ok()?;
        // data.result[0].value[1] is the aggregated value as a string (empty result ⇒ None).
        let raw = json
            .get("data")?
            .get("result")?
            .get(0)?
            .get("value")?
            .get(1)?
            .as_str()?;
        raw.parse().ok()
    }

    async fn aggregate_range(
        &self,
        key: &SeriesKey,
        from_s: i64,
        to_s: i64,
        step_s: u64,
    ) -> Vec<MetricPoint> {
        self.query_range_points(aggregate_range_query(key), from_s, to_s, step_s)
            .await
    }

    async fn top_nodes(&self, metric: &str, agg: TopAgg, limit: usize) -> Vec<(Uuid, f64)> {
        let url = format!("{}/api/v1/query", self.base);
        let resp = match self
            .http
            .get(&url)
            .query(&[("query", topk_query(metric, agg, limit))])
            .send()
            .await
        {
            Ok(resp) => resp,
            Err(e) => {
                tracing::warn!(error = %e, "VictoriaMetrics topk query failed");
                return Vec::new();
            }
        };
        let Ok(json) = resp.json::<serde_json::Value>().await else {
            return Vec::new();
        };
        parse_top_nodes(&json)
    }

    async fn top_interfaces(
        &self,
        metric: InterfaceTopMetric,
        agg: TopAgg,
        limit: usize,
    ) -> Vec<(Uuid, i32, f64)> {
        let url = format!("{}/api/v1/query", self.base);
        let resp = match self
            .http
            .get(&url)
            .query(&[("query", topk_interface_query(metric, agg, limit))])
            .send()
            .await
        {
            Ok(resp) => resp,
            Err(e) => {
                tracing::warn!(error = %e, "VictoriaMetrics interface topk query failed");
                return Vec::new();
            }
        };
        let Ok(json) = resp.json::<serde_json::Value>().await else {
            return Vec::new();
        };
        parse_top_interfaces(&json)
    }

    async fn fresh_node_ids(&self, metric: &str, within_secs: u64) -> Vec<Uuid> {
        let url = format!("{}/api/v1/query", self.base);
        // Every node series that has any sample in the window contributes its node label.
        let query = format!("last_over_time({metric}[{}s])", within_secs.max(1));
        let resp = match self.http.get(&url).query(&[("query", query)]).send().await {
            Ok(resp) => resp,
            Err(e) => {
                tracing::warn!(error = %e, "VictoriaMetrics freshness query failed");
                return Vec::new();
            }
        };
        let Ok(json) = resp.json::<serde_json::Value>().await else {
            return Vec::new();
        };
        // Reuse the node-label parser; we only need the ids, not the values.
        parse_top_nodes(&json)
            .into_iter()
            .map(|(id, _)| id)
            .collect()
    }

    async fn interface_delta(
        &self,
        direction: DeltaDirection,
        window_secs: u64,
        limit: usize,
    ) -> Vec<(Uuid, i32, f64)> {
        let url = format!("{}/api/v1/query", self.base);
        let query = interface_delta_query(direction, window_secs, limit);
        let resp = match self.http.get(&url).query(&[("query", query)]).send().await {
            Ok(resp) => resp,
            Err(e) => {
                tracing::warn!(error = %e, "VictoriaMetrics interface-delta query failed");
                return Vec::new();
            }
        };
        let Ok(json) = resp.json::<serde_json::Value>().await else {
            return Vec::new();
        };
        let mut out = parse_top_interfaces(&json);
        // Order by magnitude so the biggest movers lead, in both directions.
        out.sort_by(|a, b| {
            b.2.abs()
                .partial_cmp(&a.2.abs())
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        out
    }

    async fn throughput_range(
        &self,
        from_s: i64,
        to_s: i64,
        step_s: u64,
    ) -> (Vec<MetricPoint>, Vec<MetricPoint>) {
        // Fleet sum of per-interface octet rates ×8 = bits/sec. 300s rate window is robust to
        // poll jitter; sampled at the requested step across the range.
        let in_q = "sum(rate(if_hc_in_octets[300s])) * 8".to_string();
        let out_q = "sum(rate(if_hc_out_octets[300s])) * 8".to_string();
        let in_pts = self.query_range_points(in_q, from_s, to_s, step_s).await;
        let out_pts = self.query_range_points(out_q, from_s, to_s, step_s).await;
        (in_pts, out_pts)
    }

    async fn interface_throughput_range(
        &self,
        node: Uuid,
        ifindex: i32,
        from_s: i64,
        to_s: i64,
        step_s: u64,
    ) -> Vec<MetricPoint> {
        // node is a UUID and ifindex an i32 (both bounded types from the topk result), so they're
        // safe to interpolate into the thin-label selector.
        let q = format!(
            "(rate(if_hc_in_octets{{node=\"{node}\",ifindex=\"{ifindex}\"}}[300s]) + \
              rate(if_hc_out_octets{{node=\"{node}\",ifindex=\"{ifindex}\"}}[300s])) * 8"
        );
        self.query_range_points(q, from_s, to_s, step_s).await
    }

    async fn node_metric_names(&self, node: Uuid, within_secs: u64) -> Vec<String> {
        // VictoriaMetrics /api/v1/series lists the label sets of every series matching a selector.
        // `node` is a UUID (bounded type), so it's safe to interpolate into the matcher.
        let url = format!("{}/api/v1/series", self.base);
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
        let start = now - i64::try_from(within_secs.max(1)).unwrap_or(i64::MAX);
        let resp = match self
            .http
            .get(&url)
            .query(&[
                ("match[]", format!("{{node=\"{node}\"}}")),
                ("start", start.to_string()),
                ("end", now.to_string()),
            ])
            .send()
            .await
        {
            Ok(resp) => resp,
            Err(e) => {
                tracing::warn!(error = %e, "VictoriaMetrics series enumeration failed");
                return Vec::new();
            }
        };
        let Ok(json) = resp.json::<serde_json::Value>().await else {
            return Vec::new();
        };
        let Some(series) = json.get("data").and_then(|d| d.as_array()) else {
            return Vec::new();
        };
        let mut names: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for s in series {
            if let Some(name) = s.get("__name__").and_then(|n| n.as_str()) {
                names.insert(name.to_owned());
            }
        }
        names.into_iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;
    use yagra_common::{IfIndex, NodeId};

    #[test]
    fn node_selector_has_only_node_label() {
        let key = SeriesKey::node(NodeId::from(Uuid::nil()), "icmp_rtt_ms");
        assert_eq!(
            selector(&key),
            "icmp_rtt_ms{node=\"00000000-0000-0000-0000-000000000000\"}"
        );
    }

    #[test]
    fn interface_selector_adds_ifindex_label() {
        let key = SeriesKey::interface(NodeId::from(Uuid::nil()), IfIndex(3), "if_in_octets");
        assert_eq!(
            selector(&key),
            "if_in_octets{node=\"00000000-0000-0000-0000-000000000000\",ifindex=\"3\"}"
        );
    }

    #[test]
    fn rate_query_wraps_selector_in_rate_window() {
        let key = SeriesKey::interface(NodeId::from(Uuid::nil()), IfIndex(3), "if_hc_in_octets");
        assert_eq!(
            rate_query(&key, 300),
            "rate(if_hc_in_octets{node=\"00000000-0000-0000-0000-000000000000\",ifindex=\"3\"}[300s])"
        );
    }

    #[test]
    fn latest_query_uses_last_over_time_window() {
        let key = SeriesKey::node(NodeId::from(Uuid::nil()), "icmp_rtt_ms");
        assert_eq!(
            latest_query(&key),
            "last_over_time(icmp_rtt_ms{node=\"00000000-0000-0000-0000-000000000000\"}[1800s])"
        );
    }

    #[test]
    fn instant_rate_query_wraps_rate_in_last_over_time_subquery() {
        let key = SeriesKey::interface(NodeId::from(Uuid::nil()), IfIndex(3), "if_hc_in_octets");
        assert_eq!(
            instant_rate_query(&key, 300),
            "last_over_time((rate(if_hc_in_octets{node=\"00000000-0000-0000-0000-000000000000\",ifindex=\"3\"}[300s]))[1800s:300s])"
        );
    }

    #[test]
    fn aggregate_latest_query_wraps_node_selector_in_max_last_over_time() {
        let key = SeriesKey::node(NodeId::from(Uuid::nil()), "huawei_cpu_usage");
        assert_eq!(
            aggregate_latest_query(&key),
            "max(last_over_time(huawei_cpu_usage{node=\"00000000-0000-0000-0000-000000000000\"}[1800s]))"
        );
    }

    #[test]
    fn aggregate_range_query_wraps_node_selector_in_max() {
        let key = SeriesKey::node(NodeId::from(Uuid::nil()), "huawei_mem_usage");
        assert_eq!(
            aggregate_range_query(&key),
            "max(huawei_mem_usage{node=\"00000000-0000-0000-0000-000000000000\"})"
        );
    }

    #[test]
    fn aggregate_selector_drops_ifindex_dimension() {
        // Built from an interface key, but the aggregate collapses to a node-only selector.
        let key = SeriesKey::interface(NodeId::from(Uuid::nil()), IfIndex(7), "huawei_cpu_usage");
        assert_eq!(
            node_scope_selector(&key),
            "huawei_cpu_usage{node=\"00000000-0000-0000-0000-000000000000\"}"
        );
    }

    #[tokio::test]
    async fn in_memory_store_has_no_aggregate() {
        let store = InMemorySink::default();
        let key = SeriesKey::node(NodeId::new(), "huawei_cpu_usage");
        assert_eq!(MetricStore::aggregate_latest(&store, &key).await, None);
        assert!(MetricStore::aggregate_range(&store, &key, 0, 3600, 60)
            .await
            .is_empty());
    }

    #[tokio::test]
    async fn in_memory_store_has_no_rate() {
        let store = InMemorySink::default();
        let key = SeriesKey::interface(NodeId::new(), IfIndex(1), "if_hc_in_octets");
        assert_eq!(MetricStore::rate(&store, &key, 300).await, None);
    }

    #[tokio::test]
    async fn in_memory_store_has_no_rate_range() {
        let store = InMemorySink::default();
        let key = SeriesKey::interface(NodeId::new(), IfIndex(1), "if_hc_in_octets");
        assert!(MetricStore::rate_range(&store, &key, 0, 3600, 60, 300)
            .await
            .is_empty());
    }

    #[tokio::test]
    async fn in_memory_store_has_no_top_nodes() {
        let store = InMemorySink::default();
        assert!(
            MetricStore::top_nodes(&store, "icmp_rtt_ms", TopAgg::Now, 5)
                .await
                .is_empty()
        );
        assert!(MetricStore::top_interfaces(
            &store,
            InterfaceTopMetric::Throughput,
            TopAgg::Now,
            5
        )
        .await
        .is_empty());
    }

    #[test]
    fn topk_interface_throughput_sums_in_out_rate_scaled_to_bits() {
        assert_eq!(
            topk_interface_query(InterfaceTopMetric::Throughput, TopAgg::Now, 5),
            "topk(5, max by (node,ifindex) (last_over_time(((rate(if_hc_in_octets[300s]) + rate(if_hc_out_octets[300s])) * 8)[1800s:300s])))"
        );
    }

    #[test]
    fn interface_delta_up_uses_topk_and_offset() {
        assert_eq!(
            interface_delta_query(DeltaDirection::Up, 300, 5),
            "topk(5, ((rate(if_hc_in_octets[300s]) + rate(if_hc_out_octets[300s])) - (rate(if_hc_in_octets[300s] offset 300s) + rate(if_hc_out_octets[300s] offset 300s))) * 8)"
        );
    }

    #[test]
    fn interface_delta_down_uses_bottomk() {
        assert!(interface_delta_query(DeltaDirection::Down, 300, 5).starts_with("bottomk(5,"));
    }

    #[test]
    fn topk_interface_errors_uses_error_counters_and_hourly_peak() {
        assert_eq!(
            topk_interface_query(InterfaceTopMetric::Errors, TopAgg::Max1h, 3),
            "topk(3, max by (node,ifindex) (max_over_time(((rate(if_in_errors[300s]) + rate(if_out_errors[300s])))[3600s:300s])))"
        );
    }

    #[test]
    fn parse_top_interfaces_extracts_node_ifindex_value_sorted_desc() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let json = serde_json::json!({
            "data": { "result": [
                { "metric": { "node": a.to_string(), "ifindex": "3" }, "value": [1, "1000"] },
                { "metric": { "node": b.to_string(), "ifindex": "7" }, "value": [1, "5000"] },
                { "metric": { "node": a.to_string() }, "value": [1, "9"] },
            ]}
        });
        assert_eq!(
            parse_top_interfaces(&json),
            vec![(b, 7, 5000.0), (a, 3, 1000.0)]
        );
    }

    #[test]
    fn topk_now_collapses_to_node_max_within_lookback() {
        assert_eq!(
            topk_query("icmp_rtt_ms", TopAgg::Now, 5),
            "topk(5, max by (node) (last_over_time(icmp_rtt_ms[1800s])))"
        );
    }

    #[test]
    fn topk_max_1h_uses_hourly_peak() {
        assert_eq!(
            topk_query("cisco_cpu_5min", TopAgg::Max1h, 3),
            "topk(3, max by (node) (max_over_time(cisco_cpu_5min[3600s])))"
        );
    }

    #[test]
    fn topk_clamps_limit_into_range() {
        assert!(topk_query("m", TopAgg::Now, 0).starts_with("topk(1,"));
        assert!(topk_query("m", TopAgg::Now, 9999).starts_with("topk(100,"));
    }

    #[test]
    fn parse_top_nodes_extracts_node_and_value_sorted_desc() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let json = serde_json::json!({
            "data": { "result": [
                { "metric": { "node": a.to_string() }, "value": [1_000, "12.5"] },
                { "metric": { "node": b.to_string() }, "value": [1_000, "44.0"] },
            ]}
        });
        let out = parse_top_nodes(&json);
        // Highest-first regardless of input order.
        assert_eq!(out, vec![(b, 44.0), (a, 12.5)]);
    }

    #[test]
    fn parse_top_nodes_skips_unparseable_series_and_empty() {
        let good = Uuid::new_v4();
        let json = serde_json::json!({
            "data": { "result": [
                { "metric": {}, "value": [1, "1.0"] },                       // no node label
                { "metric": { "node": "not-a-uuid" }, "value": [1, "1.0"] }, // bad node
                { "metric": { "node": good.to_string() }, "value": [1, "x"] }, // bad value
                { "metric": { "node": good.to_string() }, "value": [1, "9.0"] },
            ]}
        });
        assert_eq!(parse_top_nodes(&json), vec![(good, 9.0)]);
        assert!(parse_top_nodes(&serde_json::json!({})).is_empty());
    }

    #[tokio::test]
    async fn in_memory_store_round_trips_through_trait() {
        use yagra_bus::{CheckOutcome, Sample};
        let node = NodeId::new();
        let store = InMemorySink::default();
        let result = PollResult {
            schema_version: 1,
            job_id: Uuid::nil(),
            node_id: node,
            at_unix_ms: 0,
            outcome: CheckOutcome::Reachable,
            samples: vec![Sample::gauge("icmp_rtt_ms", 7.0)],
            interfaces: Vec::new(),
            sys_descr: None,
        };
        MetricStore::write(&store, &result).await;
        assert_eq!(
            MetricStore::latest(&store, &SeriesKey::node(node, "icmp_rtt_ms")).await,
            Some(7.0)
        );
    }
}
