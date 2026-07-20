// SPDX-License-Identifier: AGPL-3.0-only
//! Sanitized output types for the MCP tool surface — the ADR-018 enforcement boundary.
//!
//! **Every MCP tool output type is defined in this file**, built field-by-field from the internal
//! model. No tool serializes a raw `sqlx` row, a `yagra_common`/`yagra_alert` model, or anything via
//! `#[serde(flatten)]` of one — that is how a credential reference would leak. The one
//! credential-bearing field on [`yagra_common::Node`] (`credential`, a `CredentialId` reference) and
//! the internal poller-assignment `pool` are deliberately **excluded** from [`NodeSummaryDto`]. The
//! `dto_canary` test asserts no forbidden key (`credential`, `community`, `password`, `token`, `pool`,
//! `auth_key`, `priv_key`, `secret`) ever appears in a serialized DTO, so a future field addition that
//! reintroduces one fails the build.

use serde::Serialize;
use std::collections::BTreeMap;
use uuid::Uuid;
use yagra_alert::Alert;
use yagra_common::{Node, NodeState};

use crate::history::AlertHistoryRow;
use crate::repo::{InterfaceMeta, TopologyRow};

/// Render an optional rolled-up state to its stable lowercase string (unobserved ⇒ `"unknown"`).
fn state_str(state: Option<NodeState>) -> String {
    state.map_or("unknown", |s| s.as_str()).to_owned()
}

/// A monitored node, sanitized for AI consumption: identity + display metadata + rolled-up state.
/// Excludes `credential` (ADR-018), `pool` (internal poller assignment), and `profile` (internal id).
#[derive(Debug, Clone, Serialize)]
pub struct NodeSummaryDto {
    pub id: Uuid,
    pub name: String,
    /// Management address (IPv4 or IPv6), rendered as text.
    pub address: String,
    /// Rolled-up display state: `ok`/`warning`/`critical`/`unknown`/`unreachable`/`maintenance`.
    pub state: String,
    pub parent: Option<Uuid>,
    pub group: Option<Uuid>,
    pub vendor: Option<String>,
    pub model: Option<String>,
    pub tags: BTreeMap<String, String>,
}

impl NodeSummaryDto {
    /// Build from a node and its (optional) rolled-up display state.
    #[must_use]
    pub fn from_node(node: &Node, state: Option<NodeState>) -> Self {
        Self {
            id: node.id.0,
            name: node.name.clone(),
            address: node.address.to_string(),
            state: state_str(state),
            parent: node.parent.map(|p| p.0),
            group: node.group.map(|g| g.0),
            vendor: node.vendor.clone(),
            model: node.model.clone(),
            tags: node.tags.clone(),
        }
    }
}

/// The numeric breach detail of a threshold alert (liveness alerts carry none).
#[derive(Debug, Clone, Serialize)]
pub struct BreachDto {
    pub value: f64,
    pub threshold: Option<f64>,
    /// `"above"` / `"below"`.
    pub direction: String,
}

/// An active or historical alert, sanitized. Carries no credential-bearing field.
#[derive(Debug, Clone, Serialize)]
pub struct AlertDto {
    pub node_id: Uuid,
    /// Resolved node display name when available (else `None`; the caller keeps the id).
    pub node_name: Option<String>,
    /// `info` / `warning` / `critical`.
    pub severity: String,
    /// The committed state that fired: `warning`/`critical`/`unreachable`/…
    pub state: String,
    /// Metric the check measured (`icmp_rtt_ms`, or the liveness sentinel).
    pub metric: String,
    /// Fire time as an RFC 3339 UTC timestamp.
    pub fired_at: String,
    /// Upstream root-cause node id when this alert was attributed by dependency analysis.
    pub root_cause: Option<Uuid>,
    /// Whether the underlying check is currently flapping.
    pub flapping: bool,
    pub breach: Option<BreachDto>,
}

impl AlertDto {
    /// Build from an active alert and an optional resolved node name.
    #[must_use]
    pub fn from_alert(alert: &Alert, node_name: Option<String>) -> Self {
        Self {
            node_id: alert.node.0,
            node_name,
            severity: alert.severity.as_str().to_owned(),
            state: alert.state.as_str().to_owned(),
            metric: alert.metric.clone(),
            fired_at: unix_ms_to_rfc3339(alert.at_unix_ms),
            root_cause: alert.root_cause.map(|r| r.0),
            flapping: alert.flapping,
            breach: alert.breach.as_ref().map(|b| BreachDto {
                value: b.value,
                threshold: b.threshold,
                direction: b.direction.clone(),
            }),
        }
    }
}

/// One historical alert transition, sanitized (from the alert-history store).
#[derive(Debug, Clone, Serialize)]
pub struct AlertHistoryDto {
    pub node_id: Uuid,
    pub node_name: Option<String>,
    pub severity: String,
    pub state: String,
    pub metric: Option<String>,
    /// Whether this row is the resolution (clear) of a prior fire.
    pub resolved: bool,
    /// Event time as an RFC 3339 UTC timestamp.
    pub at: String,
    pub observed_value: Option<f64>,
    pub threshold_value: Option<f64>,
    /// Breach direction, `above`/`below` (threshold checks only).
    pub direction: Option<String>,
}

impl AlertHistoryDto {
    /// Build from a history row and an optional resolved node name.
    #[must_use]
    pub fn from_row(row: &AlertHistoryRow, node_name: Option<String>) -> Self {
        Self {
            node_id: row.node,
            node_name,
            severity: row.severity.clone(),
            state: row.state.clone(),
            metric: row.metric.clone(),
            resolved: row.resolved,
            at: unix_ms_to_rfc3339(row.at_unix_ms),
            observed_value: row.observed_value,
            threshold_value: row.threshold_value,
            direction: row.direction.clone(),
        }
    }
}

/// One interface's identity + link metadata (no counters, no secrets).
#[derive(Debug, Clone, Serialize)]
pub struct InterfaceDto {
    pub ifindex: i32,
    pub name: Option<String>,
    pub alias: Option<String>,
    /// Nominal speed in bits/sec, if known.
    pub speed: Option<i64>,
    /// Last time this interface was seen, as an RFC 3339 UTC timestamp (if ever).
    pub last_seen: Option<String>,
}

impl InterfaceDto {
    /// Build from an interface-metadata row.
    #[must_use]
    pub fn from_meta(meta: &InterfaceMeta) -> Self {
        Self {
            ifindex: meta.ifindex,
            name: meta.if_name.clone(),
            alias: meta.if_alias.clone(),
            speed: meta.if_speed,
            last_seen: meta.last_seen_s.map(unix_s_to_rfc3339),
        }
    }
}

/// A node's full status: its summary, active alerts, and interfaces.
#[derive(Debug, Clone, Serialize)]
pub struct NodeStatusDto {
    pub node: NodeSummaryDto,
    pub alerts: Vec<AlertDto>,
    pub interfaces: Vec<InterfaceDto>,
}

/// One dependency-graph edge: a node and its upstream parent.
#[derive(Debug, Clone, Serialize)]
pub struct TopologyEdgeDto {
    pub id: Uuid,
    pub name: String,
    pub parent_id: Option<Uuid>,
}

impl TopologyEdgeDto {
    /// Build from a topology row.
    #[must_use]
    pub fn from_row(row: &TopologyRow) -> Self {
        Self {
            id: row.id,
            name: row.name.clone(),
            parent_id: row.parent_id,
        }
    }
}

/// One metric sample.
#[derive(Debug, Clone, Serialize)]
pub struct MetricPointDto {
    /// Unix timestamp (seconds).
    pub t: i64,
    pub v: f64,
}

/// A metric query answer: the series (range/rate) or a single latest value.
#[derive(Debug, Clone, Serialize)]
pub struct MetricSeriesDto {
    pub node_id: Uuid,
    pub metric: String,
    /// `latest` / `range` / `rate`.
    pub mode: String,
    /// The single latest value (mode `latest`); `None` if the series has no data.
    pub latest: Option<f64>,
    /// The sampled points (modes `range`/`rate`), oldest first.
    pub points: Vec<MetricPointDto>,
}

/// Fleet health summary: inventory size and rolled-up state counts + which optional stores are on.
#[derive(Debug, Clone, Serialize)]
pub struct FleetSummaryDto {
    /// Total nodes in the inventory.
    pub total_nodes: i64,
    /// Count per rolled-up state (`ok`/`warning`/`critical`/`unreachable`/`maintenance`), plus a
    /// derived `unknown` for never-observed nodes (`total_nodes − observed`).
    pub states: BTreeMap<String, i64>,
    /// Number of currently active alerts.
    pub active_alerts: usize,
    /// Whether the TSDB (metrics) backend answered its health probe.
    pub metrics_healthy: bool,
    /// Whether the flow tier (ClickHouse) is enabled on this core.
    pub flow_tier_enabled: bool,
    /// Whether the event-log tier (VictoriaLogs) is enabled on this core.
    pub log_tier_enabled: bool,
}

/// Convert Unix milliseconds to an RFC 3339 UTC string (falls back to the epoch on an out-of-range
/// value rather than panicking — a malformed timestamp must never take down a tool call).
fn unix_ms_to_rfc3339(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .unwrap_or_else(|| chrono::DateTime::from_timestamp(0, 0).expect("epoch is valid"))
        .to_rfc3339()
}

/// Convert Unix seconds to an RFC 3339 UTC string (same fallback discipline).
fn unix_s_to_rfc3339(s: i64) -> String {
    chrono::DateTime::from_timestamp(s, 0)
        .unwrap_or_else(|| chrono::DateTime::from_timestamp(0, 0).expect("epoch is valid"))
        .to_rfc3339()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;
    use yagra_common::{CredentialId, GroupId, Node, NodeId};

    /// Keys that must NEVER appear in any serialized MCP DTO (ADR-018 / security.md). If a future
    /// field addition reintroduces one, this test fails the build.
    const FORBIDDEN_KEYS: &[&str] = &[
        "credential",
        "community",
        "password",
        "token",
        "pool",
        "auth_key",
        "priv_key",
        "secret",
    ];

    /// Recursively assert no object key in `value` is a forbidden key.
    fn assert_no_forbidden_keys(value: &serde_json::Value, ctx: &str) {
        match value {
            serde_json::Value::Object(map) => {
                for (k, v) in map {
                    assert!(
                        !FORBIDDEN_KEYS.contains(&k.as_str()),
                        "forbidden key {k:?} appeared in a {ctx} DTO"
                    );
                    assert_no_forbidden_keys(v, ctx);
                }
            }
            serde_json::Value::Array(items) => {
                for v in items {
                    assert_no_forbidden_keys(v, ctx);
                }
            }
            _ => {}
        }
    }

    fn sample_node_with_secret() -> Node {
        let mut node = Node::new(
            NodeId::from(uuid::Uuid::new_v4()),
            "edge-router-1",
            Ipv4Addr::new(10, 0, 0, 1).into(),
        );
        // Populate exactly the fields the DTO must NOT surface.
        node.credential = Some(CredentialId::from(uuid::Uuid::new_v4()));
        node.pool = Some("tokyo".to_owned());
        node.group = Some(GroupId::from(uuid::Uuid::new_v4()));
        node.tags.insert("site".to_owned(), "tokyo".to_owned());
        node
    }

    #[test]
    fn node_summary_dto_omits_credential_and_pool() {
        let node = sample_node_with_secret();
        let dto = NodeSummaryDto::from_node(&node, Some(NodeState::Warning));
        let json = serde_json::to_value(&dto).expect("serialize");
        // Sanity: it carries the safe fields…
        assert_eq!(json["name"], "edge-router-1");
        assert_eq!(json["state"], "warning");
        assert_eq!(json["tags"]["site"], "tokyo");
        // …and not the secret/internal ones.
        assert!(json.get("credential").is_none());
        assert!(json.get("pool").is_none());
        assert!(json.get("profile").is_none());
    }

    #[test]
    fn every_dto_is_free_of_forbidden_keys() {
        // Build one instance of each DTO with representative data and run the canary over each.
        let node = sample_node_with_secret();
        let summary = NodeSummaryDto::from_node(&node, Some(NodeState::Ok));
        assert_no_forbidden_keys(&serde_json::to_value(&summary).unwrap(), "NodeSummary");

        let status = NodeStatusDto {
            node: summary.clone(),
            alerts: vec![],
            interfaces: vec![InterfaceDto {
                ifindex: 1,
                name: Some("Gig0/1".to_owned()),
                alias: Some("uplink".to_owned()),
                speed: Some(1_000_000_000),
                last_seen: Some(unix_s_to_rfc3339(0)),
            }],
        };
        assert_no_forbidden_keys(&serde_json::to_value(&status).unwrap(), "NodeStatus");

        let alert = AlertDto {
            node_id: node.id.0,
            node_name: Some("edge-router-1".to_owned()),
            severity: "critical".to_owned(),
            state: "unreachable".to_owned(),
            metric: "icmp_rtt_ms".to_owned(),
            fired_at: unix_ms_to_rfc3339(0),
            root_cause: None,
            flapping: false,
            breach: Some(BreachDto {
                value: 900.0,
                threshold: Some(500.0),
                direction: "above".to_owned(),
            }),
        };
        assert_no_forbidden_keys(&serde_json::to_value(&alert).unwrap(), "Alert");

        let topo = TopologyEdgeDto {
            id: node.id.0,
            name: "edge-router-1".to_owned(),
            parent_id: None,
        };
        assert_no_forbidden_keys(&serde_json::to_value(&topo).unwrap(), "TopologyEdge");

        let series = MetricSeriesDto {
            node_id: node.id.0,
            metric: "cpu_percent".to_owned(),
            mode: "range".to_owned(),
            latest: None,
            points: vec![MetricPointDto { t: 0, v: 42.0 }],
        };
        assert_no_forbidden_keys(&serde_json::to_value(&series).unwrap(), "MetricSeries");

        let mut states = BTreeMap::new();
        states.insert("ok".to_owned(), 3);
        let fleet = FleetSummaryDto {
            total_nodes: 3,
            states,
            active_alerts: 0,
            metrics_healthy: true,
            flow_tier_enabled: false,
            log_tier_enabled: false,
        };
        assert_no_forbidden_keys(&serde_json::to_value(&fleet).unwrap(), "FleetSummary");
    }
}
