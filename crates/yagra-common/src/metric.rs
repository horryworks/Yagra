// SPDX-License-Identifier: AGPL-3.0-only
//! Metric identity and kind.
//!
//! Series identity follows the **thin-label model** (ADR-011): a series is keyed by
//! the stable `(NodeId, Option<IfIndex>, metric name)` only — descriptive, mutable
//! attributes (ifDescr, site, role, …) stay in PostgreSQL and are joined at query
//! time. This is what keeps series cardinality bounded.
//!
//! [`MetricKind`] distinguishes counters from gauges because pollers store **raw**
//! counters and rates/utilization are derived at query time via the TSDB's
//! `rate()`/`increase()` (ADR-012) — the poller holds no previous-value state.

use crate::ids::{IfIndex, NodeId};
use serde::{Deserialize, Serialize};
use std::fmt;

/// What a metric's values represent over time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum MetricKind {
    /// An instantaneous value (CPU %, temperature, memory). Used as-is.
    Gauge,
    /// A monotonically increasing counter (octets, packets, errors). Stored **raw**;
    /// rates are derived at query/eval time, never by the poller (ADR-012).
    Counter,
}

/// Whether `name` is a legal Prometheus/VictoriaMetrics metric name — `[a-zA-Z_:][a-zA-Z0-9_:]*`,
/// non-empty. Series identity is the single biggest cardinality risk (ADR-011), so this is the
/// shape check applied at every edge where a metric name enters series identity: the API (operator
/// input) *and* the TSDB write path (poller-supplied sample names). A name that fails this can't be
/// interpolated into an exposition line to inject stray labels or forge a series.
#[must_use]
pub fn is_valid_metric_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' || c == ':' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == ':')
}

/// The stable identity of one time series.
///
/// Construct via [`SeriesKey::node`] for node-level metrics or
/// [`SeriesKey::interface`] for per-interface metrics. The `metric` name is a stable
/// identifier (e.g. `if_in_octets`), not a human-readable description.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SeriesKey {
    /// Owning node.
    pub node: NodeId,
    /// Interface, for per-interface metrics; `None` for node-level metrics.
    pub ifindex: Option<IfIndex>,
    /// Stable metric name (the TSDB metric label).
    pub metric: String,
}

impl SeriesKey {
    /// A node-level series (no interface dimension).
    #[must_use]
    pub fn node(node: NodeId, metric: impl Into<String>) -> Self {
        Self {
            node,
            ifindex: None,
            metric: metric.into(),
        }
    }

    /// A per-interface series.
    #[must_use]
    pub fn interface(node: NodeId, ifindex: IfIndex, metric: impl Into<String>) -> Self {
        Self {
            node,
            ifindex: Some(ifindex),
            metric: metric.into(),
        }
    }
}

impl SeriesKey {
    /// Render this sample as a VictoriaMetrics/Prometheus exposition line:
    /// `metric{node="…",ifindex="3"} value timestamp_ms`. Label values are quoted; the thin
    /// label set (node + optional ifindex) is all that goes to the TSDB (ADR-011). This is the
    /// exact wire format the VictoriaMetrics import endpoint consumes.
    #[must_use]
    pub fn prometheus_line(&self, value: f64, ts_unix_ms: i64) -> String {
        match self.ifindex {
            Some(idx) => format!(
                "{}{{node=\"{}\",ifindex=\"{}\"}} {} {}",
                self.metric, self.node, idx, value, ts_unix_ms
            ),
            None => format!(
                "{}{{node=\"{}\"}} {} {}",
                self.metric, self.node, value, ts_unix_ms
            ),
        }
    }
}

impl fmt::Display for SeriesKey {
    /// Renders the thin label set, e.g. `if_in_octets{node=…,ifindex=3}`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.ifindex {
            Some(idx) => write!(f, "{}{{node={},ifindex={}}}", self.metric, self.node, idx),
            None => write!(f, "{}{{node={}}}", self.metric, self.node),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metric_name_validation_accepts_real_names_and_rejects_injection() {
        // Every name the poller actually emits is valid.
        for ok in [
            "icmp_rtt_ms",
            "if_hc_in_octets",
            "cpu_util",
            "snmp_sys_uptime_ticks",
            "snmp_oid_1_3_6_1_2_1_2_2_1_10",
            "http:up",
            "_leading_underscore",
        ] {
            assert!(is_valid_metric_name(ok), "{ok} should be valid");
        }
        // Malformed / hostile names are rejected before they reach series identity.
        for bad in [
            "",
            "1_leading_digit",
            "has space",
            "has-dash",
            "with.dot",
            "inject{evil=\"x\"} 1\nother",
            "trailing\n",
        ] {
            assert!(!is_valid_metric_name(bad), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn node_series_has_no_interface() {
        let k = SeriesKey::node(NodeId::new(), "cpu_util");
        assert_eq!(k.ifindex, None);
    }

    #[test]
    fn interface_series_carries_ifindex() {
        let node = NodeId::new();
        let k = SeriesKey::interface(node, IfIndex(3), "if_in_octets");
        assert_eq!(k.ifindex, Some(IfIndex(3)));
        assert_eq!(k.node, node);
    }

    #[test]
    fn display_renders_thin_labels() {
        use uuid::Uuid;
        let node = NodeId::from(Uuid::nil());
        let k = SeriesKey::interface(node, IfIndex(3), "if_in_octets");
        assert_eq!(
            k.to_string(),
            "if_in_octets{node=00000000-0000-0000-0000-000000000000,ifindex=3}"
        );
    }

    #[test]
    fn prometheus_line_quotes_labels_and_appends_value_ts() {
        use uuid::Uuid;
        let node = NodeId::from(Uuid::nil());
        assert_eq!(
            SeriesKey::node(node, "cpu_util").prometheus_line(42.5, 1000),
            "cpu_util{node=\"00000000-0000-0000-0000-000000000000\"} 42.5 1000"
        );
        assert_eq!(
            SeriesKey::interface(node, IfIndex(3), "if_in_octets").prometheus_line(99.0, 2000),
            "if_in_octets{node=\"00000000-0000-0000-0000-000000000000\",ifindex=\"3\"} 99 2000"
        );
    }
}
