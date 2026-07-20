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

use crate::analysis::{AnalysisFinding, AnalysisJob};
use crate::events::EventRow;
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
    /// The check that fired — the third part of an alert's `(node, check, severity)` identity, needed
    /// to acknowledge it via the `ack_alert` tool.
    pub check_id: Uuid,
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
            check_id: alert.check.0,
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

/// A Troubleshoot analysis job (ADR-022), sanitized for AI consumption — identity, tool, scope, and
/// lifecycle state. Timestamps are RFC 3339 UTC. Carries no credential-bearing field (a job is a
/// record of a **read** over the TSDB; ADR-028 Increment 2 treats analyses as read-only).
#[derive(Debug, Clone, Serialize)]
pub struct AnalysisJobDto {
    pub id: Uuid,
    /// `anomaly` / `correlation` / `capacity` / `flap`.
    pub tool: String,
    /// `all` / `group` / `node`.
    pub scope_kind: String,
    /// Group/node id the analysis ran over (`None` for fleet-wide `all` scope).
    pub scope_id: Option<Uuid>,
    /// Human label for the scope (e.g. a group or node name).
    pub scope_label: String,
    /// Lifecycle state: `running` / `done` / `failed` / `cancelled`.
    pub state: String,
    /// Progress percent (0–100).
    pub pct: i32,
    /// Current progress caption while running (`None` once terminal).
    pub phase: Option<String>,
    /// Number of findings produced (valid once `done`).
    pub finding_count: i32,
    /// One-line result summary (once `done`).
    pub summary: Option<String>,
    /// Failure reason (once `failed`).
    pub error: Option<String>,
    pub created_at: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
}

impl AnalysisJobDto {
    /// Build from an analysis-job row.
    #[must_use]
    pub fn from_job(job: &AnalysisJob) -> Self {
        Self {
            id: job.id,
            tool: job.tool.clone(),
            scope_kind: job.scope_kind.clone(),
            scope_id: job.scope_id,
            scope_label: job.scope_label.clone(),
            state: job.state.clone(),
            pct: job.pct,
            phase: job.phase.clone(),
            finding_count: job.finding_count,
            summary: job.summary.clone(),
            error: job.error.clone(),
            created_at: unix_ms_to_rfc3339(job.created_ms),
            started_at: job.started_ms.map(unix_ms_to_rfc3339),
            finished_at: job.finished_ms.map(unix_ms_to_rfc3339),
        }
    }
}

/// One analysis finding (anomaly card / correlation pair / capacity projection / flap row), sanitized.
/// The bulky chart `points` array is stripped from `detail` (see [`compact_detail`]) — the LLM needs
/// the scalar characterization, not per-sample data. No secret can appear: findings are derived from
/// metric values keyed by node id, never from device credentials.
#[derive(Debug, Clone, Serialize)]
pub struct AnalysisFindingDto {
    /// 0–100 significance score (higher = more urgent).
    pub score: f64,
    /// `info` / `warn` / `crit`.
    pub severity: String,
    /// The node this finding is about (`None` for cross-node correlations).
    pub node_id: Option<Uuid>,
    pub node_name: String,
    /// The metric involved (or, for correlation, the two series being related).
    pub metric: String,
    /// Finding kind/shape: `spike`/`level`/`drift`/`flat`/`season` (anomaly), `capacity`, `flap`,
    /// or `correlation`.
    pub kind: String,
    /// Relative "when" label (e.g. `2h ago`, `co-rising`, `N flaps`).
    pub when_label: String,
    /// Duration/magnitude label (e.g. `ongoing`, `~30d to 100%`, `r=0.93`).
    pub duration: String,
    /// Scalar detail for the finding kind (chart `points` removed). Shapes: capacity →
    /// `{current, slope_per_day, tte_days}`; correlation → `{r, samples}`; flap → `{flaps, per_hour}`;
    /// anomaly → `{mean, sigma, recent_from}`.
    pub detail: serde_json::Value,
}

impl AnalysisFindingDto {
    /// Build from a finding row, compacting its `detail`.
    #[must_use]
    pub fn from_finding(f: &AnalysisFinding) -> Self {
        Self {
            score: f.score,
            severity: f.severity.clone(),
            node_id: f.node_id,
            node_name: f.node_name.clone(),
            metric: f.metric.clone(),
            kind: f.kind.clone(),
            when_label: f.when_label.clone(),
            duration: f.duration.clone(),
            detail: compact_detail(f.detail.clone()),
        }
    }
}

/// Strip the bulky per-sample `points` array from a finding's `detail` (kept for the WebUI chart, of
/// no use to an LLM), leaving the scalar characterization. A non-object detail passes through.
fn compact_detail(mut detail: serde_json::Value) -> serde_json::Value {
    if let Some(obj) = detail.as_object_mut() {
        obj.remove("points");
    }
    detail
}

/// One received passive event (syslog / SNMP trap / webhook), sanitized for AI consumption. Excludes
/// the internal `pool` (poller-assignment) and `source_id` (event-source id) — the LLM sees the
/// human-facing shape (source IP, hostname, trap name, message), never internal routing ids.
#[derive(Debug, Clone, Serialize)]
pub struct EventDto {
    pub id: Uuid,
    /// `syslog` / `trap` / `webhook`.
    pub kind: String,
    /// Event time as an RFC 3339 UTC timestamp.
    pub at: String,
    pub node_id: Option<Uuid>,
    /// Resolved node display name when available.
    pub node_name: Option<String>,
    /// The datagram source address (device that emitted the event), if known.
    pub source_ip: Option<String>,
    /// Syslog hostname field, if present.
    pub hostname: Option<String>,
    /// Syslog app-name field, if present.
    pub app_name: Option<String>,
    /// SNMP trap OID (trap events only).
    pub trap_oid: Option<String>,
    /// Well-known MIB name for `trap_oid` (e.g. `linkDown`), if in the curated set.
    pub trap_name: Option<String>,
    pub message: String,
    /// The event rule that matched (raised/cleared an alert), if any.
    pub matched_rule_id: Option<Uuid>,
    /// What the pipeline did with the event: `raised` / `cleared` / `logged` / …
    pub action: String,
}

impl EventDto {
    /// Build from an event row and an optional resolved node name.
    #[must_use]
    pub fn from_row(row: &EventRow, node_name: Option<String>) -> Self {
        Self {
            id: row.id,
            kind: row.kind.clone(),
            at: unix_ms_to_rfc3339(row.at_unix_ms),
            node_id: row.node_id,
            node_name,
            source_ip: row.source_ip.clone(),
            hostname: row.hostname.clone(),
            app_name: row.app_name.clone(),
            trap_oid: row.trap_oid.clone(),
            trap_name: row.trap_name.clone(),
            message: row.message.clone(),
            matched_rule_id: row.matched_rule_id,
            action: row.action.clone(),
        }
    }
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
            check_id: uuid::Uuid::new_v4(),
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

        let job = AnalysisJob {
            id: node.id.0,
            tool: "anomaly".to_owned(),
            scope_kind: "all".to_owned(),
            scope_id: None,
            scope_label: "All nodes".to_owned(),
            params: serde_json::json!({ "sensitivity": 3.0 }),
            state: "done".to_owned(),
            pct: 100,
            phase: None,
            finding_count: 1,
            summary: Some("1 anomaly".to_owned()),
            error: None,
            created_ms: 0,
            started_ms: Some(0),
            finished_ms: Some(1000),
        };
        assert_no_forbidden_keys(
            &serde_json::to_value(AnalysisJobDto::from_job(&job)).unwrap(),
            "AnalysisJob",
        );

        let finding = AnalysisFinding {
            id: node.id.0,
            score: 92.0,
            severity: "crit".to_owned(),
            node_id: Some(node.id.0),
            node_name: "edge-router-1".to_owned(),
            metric: "cpu_percent".to_owned(),
            kind: "spike".to_owned(),
            when_label: "2h ago".to_owned(),
            duration: "ongoing".to_owned(),
            detail: serde_json::json!({
                "mean": 20.0, "sigma": 3.0,
                "points": [{ "t": 0, "v": 20.0 }, { "t": 60, "v": 90.0 }],
            }),
        };
        assert_no_forbidden_keys(
            &serde_json::to_value(AnalysisFindingDto::from_finding(&finding)).unwrap(),
            "AnalysisFinding",
        );

        // EventRow carries `pool` (a forbidden key) — the DTO must drop it.
        let event = EventRow {
            id: node.id.0,
            kind: "trap".to_owned(),
            at_unix_ms: 0,
            recorded_at: chrono::DateTime::from_timestamp(0, 0).unwrap(),
            source_ip: Some("10.0.0.1".to_owned()),
            node_id: Some(node.id.0),
            source_id: None,
            pool: Some("tokyo".to_owned()),
            facility: None,
            syslog_severity: None,
            hostname: Some("edge-router-1".to_owned()),
            app_name: None,
            trap_oid: Some("1.3.6.1.6.3.1.1.5.3".to_owned()),
            trap_name: Some("linkDown".to_owned()),
            varbinds: None,
            message: "link down on Gi0/1".to_owned(),
            matched_rule_id: None,
            action: "raised".to_owned(),
        };
        let event_json =
            serde_json::to_value(EventDto::from_row(&event, Some("edge-router-1".to_owned())))
                .unwrap();
        assert!(
            event_json.get("pool").is_none(),
            "pool dropped from EventDto"
        );
        assert_no_forbidden_keys(&event_json, "Event");
    }

    #[test]
    fn finding_dto_strips_chart_points() {
        // The bulky per-sample `points` array is dropped; the scalar characterization is kept.
        let finding = AnalysisFinding {
            id: uuid::Uuid::new_v4(),
            score: 80.0,
            severity: "warn".to_owned(),
            node_id: None,
            node_name: "a ↔ b".to_owned(),
            metric: "a ↔ b".to_owned(),
            kind: "correlation".to_owned(),
            when_label: "co-rising".to_owned(),
            duration: "r=0.93".to_owned(),
            detail: serde_json::json!({
                "r": 0.93, "samples": 42,
                "points": [{ "t": 0, "v": 1.0 }],
            }),
        };
        let json = serde_json::to_value(AnalysisFindingDto::from_finding(&finding)).unwrap();
        assert!(json["detail"].get("points").is_none(), "points stripped");
        assert_eq!(json["detail"]["r"], 0.93, "scalars kept");
        assert_eq!(json["detail"]["samples"], 42);
    }
}
