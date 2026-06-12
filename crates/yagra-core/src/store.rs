//! The metric-store seam: where poll results are written and read back.
//!
//! [`MetricStore`] abstracts the TSDB so the API and the result consumer don't care
//! whether they talk to VictoriaMetrics ([`VmStore`], live) or the in-memory skeleton
//! ([`InMemorySink`]). Writes use VictoriaMetrics' Prometheus import endpoint with the
//! thin-label exposition line (ADR-011); reads use an instant `query`. Raw counters are
//! stored as-is; rates are derived at query time (ADR-012).

use async_trait::async_trait;
use serde::Serialize;
use yagra_bus::PollResult;
use yagra_common::SeriesKey;

use crate::sink::InMemorySink;

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
                let t = arr.first()?.as_f64()? as i64;
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
        };
        MetricStore::write(&store, &result).await;
        assert_eq!(
            MetricStore::latest(&store, &SeriesKey::node(node, "icmp_rtt_ms")).await,
            Some(7.0)
        );
    }
}
