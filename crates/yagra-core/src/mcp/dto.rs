// SPDX-License-Identifier: AGPL-3.0-only
//! Sanitized output types for the MCP tool surface — the ADR-018 enforcement boundary.
//!
//! **Every MCP tool output type is defined in this file**, built field-by-field from the internal
//! model. No tool serializes a raw `sqlx` row, a `yagra_common`/`yagra_alert` model, or anything via
//! `#[serde(flatten)]` of one — that is how a credential reference would leak. The one
//! credential-bearing field on [`yagra_common::Node`] (`credential`, a `CredentialId` reference) and
//! the internal poller-assignment `pool` are deliberately **excluded** from [`NodeSummaryDto`].
//!
//! The canary in this module's tests enforces that in **two tiers**, because the two bans have
//! different reasons and different reaches:
//!
//! - `SECRET_KEYS` (`credential`, `community`, `password`, `token`, `auth_key`, `priv_key`,
//!   `secret`) — never, in any tool result, at any depth (ADR-018 / security.md).
//! - `INVENTORY_NOISE_KEYS` (`pool`, `profile`) — dropped from DTOs describing monitored
//!   equipment. Not secret: `pool` is the *answer* to "which poller owns this node", which
//!   ADR-042 I3 exposes deliberately.
//!
//! A future field addition that reintroduces one fails the build. Which types the canary covers is
//! itself pinned: `the_canary_covers_every_dto_in_this_module` for the types declared here, and
//! `every_typed_tool_result_is_canaried` for the REST types tools serialize straight through.

use serde::Serialize;
use std::collections::BTreeMap;
use uuid::Uuid;
use yagra_alert::Alert;
use yagra_common::{Node, NodeKind, NodeState};

use crate::analysis::{AnalysisFinding, AnalysisJob};
use crate::events::EventRow;
use crate::history::AlertHistoryRow;
use crate::repo::InterfaceMeta;

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
    /// What this node is, and therefore what it can be asked about:
    /// `device` (ICMP, plus SNMP when configured) / `url` (an HTTP endpoint monitor) /
    /// `dns` (a name-resolution monitor) / `meraki` (polled through the Meraki Dashboard API).
    ///
    /// Worth reading before `list_node_metrics`: a URL monitor has `http_*` and no interfaces, a
    /// DNS monitor has `dns_*`, and neither is ever pinged — so an absent `icmp_rtt_ms` on one of
    /// them is the design, not an outage.
    pub kind: String,
    pub parent: Option<Uuid>,
    pub group: Option<Uuid>,
    pub vendor: Option<String>,
    pub model: Option<String>,
    pub tags: BTreeMap<String, String>,
}

impl NodeSummaryDto {
    /// Build from a node, its (optional) rolled-up display state, and its resolved kind.
    ///
    /// The kind is passed in rather than derived here: it comes from `NodeKind::resolve` over the
    /// side-table rows, which is a database read the caller has already batched over the page.
    #[must_use]
    pub fn from_node(node: &Node, state: Option<NodeState>, kind: NodeKind) -> Self {
        Self {
            id: node.id.0,
            name: node.name.clone(),
            address: node.address.to_string(),
            state: state_str(state),
            // The serde token, so this string and the REST field are produced by one mechanism.
            kind: serde_json::to_value(kind)
                .ok()
                .and_then(|v| v.as_str().map(str::to_owned))
                .unwrap_or_else(|| "device".to_owned()),
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
    /// The node this alert is about; `null` when it is not about a node — read `subject_kind`
    /// first. A `pool` alert says Yagra itself has stopped polling that pool's nodes.
    pub node_id: Option<Uuid>,
    /// What the alert is about: `node` or `pool`.
    pub subject_kind: String,
    /// The subject's name, for a subject identified by name rather than by id (the poller pool).
    pub subject_name: Option<String>,
    /// The check that fired — the third part of an alert's `(subject, check, severity)` identity,
    /// needed to acknowledge it via the `ack_alert` tool.
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
    /// The port this is about, as its SNMP ifIndex, for a metric collected once per interface —
    /// interface utilisation, port status, optical level. `null` means the alert is about the node
    /// as a whole, which is the ordinary case and not a missing value.
    ///
    /// This is the index the device files the row under, **not** a position on the front panel and
    /// not a name: pair it with `get_node_status`, whose `interfaces` carry the same `ifindex`
    /// alongside the port's name, alias and speed. Two alerts on one node with different values
    /// here are about different ports and are separate incidents, each with its own `check_id`.
    pub ifindex: Option<u32>,
    /// Whether the underlying check is currently flapping.
    pub flapping: bool,
    pub breach: Option<BreachDto>,
}

impl AlertDto {
    /// Build from an active alert and an optional resolved node name.
    ///
    /// A non-node subject reports `node_id: null` rather than an invented UUID: a plausible wrong
    /// id is the worst answer to give a model, which will happily go and look it up.
    #[must_use]
    pub fn from_alert(alert: &Alert, node_name: Option<String>) -> Self {
        Self {
            node_id: alert.node().map(|n| n.0),
            subject_kind: alert.subject.kind().as_str().to_owned(),
            subject_name: alert.subject.name().map(str::to_owned),
            check_id: alert.check.0,
            node_name,
            severity: alert.severity.as_str().to_owned(),
            state: alert.state.as_str().to_owned(),
            metric: alert.metric.clone(),
            fired_at: unix_ms_to_rfc3339(alert.at_unix_ms),
            root_cause: alert.root_cause.map(|r| r.0),
            ifindex: alert.ifindex.map(|i| i.0),
            flapping: alert.flapping,
            breach: alert.breach.as_ref().map(|b| BreachDto {
                value: b.value,
                threshold: b.threshold,
                direction: b.direction.as_str().to_owned(),
            }),
        }
    }
}

/// One historical alert transition, sanitized (from the alert-history store).
#[derive(Debug, Clone, Serialize)]
pub struct AlertHistoryDto {
    /// The node this transition was about; `null` when it was not about a node — read
    /// `subject_kind` first.
    pub node_id: Option<Uuid>,
    /// What the transition was about: `node` or `pool`.
    pub subject_kind: String,
    /// The subject's name, for a subject identified by name rather than by id (the poller pool).
    pub subject_name: Option<String>,
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
    /// The port this is about, as its SNMP ifIndex, for a metric collected once per interface —
    /// interface utilisation, port status, optical level. `null` means the alert is about the node
    /// as a whole, which is the ordinary case and not a missing value.
    ///
    /// This is the index the device files the row under, **not** a position on the front panel and
    /// not a name: pair it with `get_node_status`, whose `interfaces` carry the same `ifindex`
    /// alongside the port's name, alias and speed. Two alerts on one node with different values
    /// here are about different ports and are separate incidents, each with its own `check_id`.
    ///
    /// Also `null` on every row recorded before Yagra could alert per port, so an old row is not
    /// evidence that the alert was node-wide.
    pub ifindex: Option<u32>,
    /// Keyset cursor for the next page, first half — pass the **oldest** returned row's value as
    /// `before`. This is insertion time, which is **not** `at`: `at` is when the alert fired, and
    /// paging on it returns the wrong rows.
    pub cursor_at: String,
    /// Keyset cursor, second half — pass the same row's value as `before_id`. Required: a whole
    /// flush of alerts is written in one transaction and shares one `cursor_at`, so a
    /// timestamp-only cursor lands inside that group and skips its remaining rows.
    pub cursor_id: Uuid,
}

impl AlertHistoryDto {
    /// Build from a history row and an optional resolved node name.
    #[must_use]
    pub fn from_row(row: &AlertHistoryRow, node_name: Option<String>) -> Self {
        Self {
            node_id: row.node,
            subject_kind: row.subject_kind.as_str().to_owned(),
            subject_name: row.subject_name.clone(),
            node_name,
            severity: row.severity.as_str().to_owned(),
            state: row.state.as_str().to_owned(),
            metric: row.metric.clone(),
            resolved: row.resolved,
            at: unix_ms_to_rfc3339(row.at_unix_ms),
            observed_value: row.observed_value,
            threshold_value: row.threshold_value,
            direction: row.direction.map(|d| d.as_str().to_owned()),
            ifindex: row.ifindex,
            // Without these the tool advertised `before` while returning nothing a caller could
            // build it from — and its description pointed at `at`, which is a different clock.
            cursor_at: row.recorded_at.clone(),
            cursor_id: row.id,
        }
    }
}

/// One interface's identity, link metadata and current load (no secrets).
///
/// Mirrors what `GET /api/v1/nodes/{id}/interfaces` gives the WebUI's Interfaces tab. It used to
/// carry identity only, which made the route ledger's claim that `get_node_status` folds that
/// endpoint half true: an MCP client could name a node's ports but could not tell which one was
/// **down** or which one was **busy** — the two questions the tab exists to answer (ADR-042 I4).
#[derive(Debug, Clone, Serialize)]
pub struct InterfaceDto {
    pub ifindex: i32,
    pub name: Option<String>,
    pub alias: Option<String>,
    /// Nominal speed in bits/sec, if known.
    pub speed: Option<i64>,
    /// Negotiated duplex — `half` or `full` — or `None` when it is not known (ADR-063 Inc.1).
    ///
    /// `None` covers three cases that are indistinguishable here: the device implements none of
    /// the three columns Yagra reads (EtherLike-MIB's `dot3StatsDuplexStatus`, Huawei's
    /// `hwEthernetDuplex`, MAU-MIB's `ifMauType`), the port is down and has negotiated nothing,
    /// and the device answered "unknown".
    ///
    /// ⚠️ **`None` on an optical port is normal, not a fault** — IEEE 802.3 defines no
    /// half duplex above 1 Gbit/s, so there is nothing to negotiate. The field is diagnostic on
    /// copper, where one end forced to full against an auto-negotiating peer is a real and common
    /// cause of a link that works but is slow.
    pub duplex: Option<String>,
    /// IANAifType as the raw integer (6 = ethernetCsmacd), if the device reported one.
    ///
    /// Use it to tell **"duplex does not apply to this interface"** — a loopback (24), a virtual
    /// interface (53), a dialer (23), a tunnel (131) — from "we could not read it". Reporting a
    /// missing duplex as a problem on a loopback would be a false finding.
    pub if_type: Option<i32>,
    /// Canonical IEEE media designation — `1000BASE-T`, `1000BASE-SX`, `10GBASE-SR` — or `None`
    /// when it is not known (ADR-063 Inc.2).
    ///
    /// ⚠️ `None` is the common case, and does **not** mean the port has no medium — it means no
    /// source Yagra reads named one. There are four, in precedence order: MAU-MIB's `ifMauType`
    /// (canonical, and rarely implemented); CISCO-STACK-MIB's `portType`, which names medium and
    /// reach together; Huawei's `hwEthernetPortType`, from which a **copper** port's designation
    /// follows from its speed (802.3 registers one twisted-pair standard per rate) while a fibre
    /// one deliberately does not, since "1000BASE-X" would fight the MAU spelling on the same
    /// cell; and an ENTITY-MIB pluggable part string that happens to contain a designation. A
    /// fixed copper port therefore fills on Huawei and on Cisco stack platforms, and stays empty
    /// elsewhere.
    pub media: Option<String>,
    /// The pluggable transceiver's vendor part string, verbatim — `SFP-1000BaseLX`.
    ///
    /// ⚠️ **A part number, not a media type.** It is reported separately from `media` precisely so
    /// nothing has to pretend one is the other; `media` is filled from it only when it demonstrably
    /// contains a canonical designation. `None` for a fixed copper port, which has no pluggable.
    pub transceiver_model: Option<String>,
    /// Last time this interface was seen, as an RFC 3339 UTC timestamp (if ever).
    pub last_seen: Option<String>,
    /// Latest `ifOperStatus` (1 = up), or `None` when the node has never reported one.
    pub oper_status: Option<f64>,
    /// Current inbound rate in **bits**/sec over the standard lookback.
    pub in_bps: Option<f64>,
    /// Current outbound rate in **bits**/sec.
    pub out_bps: Option<f64>,
    /// Inbound rate as a percentage of `speed`; `None` when the speed is unknown or zero.
    pub in_util_pct: Option<f64>,
    /// Outbound rate as a percentage of `speed`.
    pub out_util_pct: Option<f64>,
    /// The transceiver's own acceptable receive-power window, dBm — the module's published floor.
    ///
    /// `None` for any interface that is not optical, and for optical ones whose vendor MIB
    /// publishes no thresholds. Present so a reader can judge `rx_power_dbm` at all: −7 dBm is
    /// comfortable on one module and failing on another, and without the window a light level is a
    /// number with no scale. **Nothing alerts on these** — they are the module's figures, not a
    /// threshold configured in Yagra (ADR-062 Inc.4).
    pub rx_power_low_dbm: Option<f64>,
    /// Ceiling of the receive window. See [`InterfaceDto::rx_power_low_dbm`].
    pub rx_power_high_dbm: Option<f64>,
    /// Floor of the transmit window. See [`InterfaceDto::rx_power_low_dbm`].
    pub tx_power_low_dbm: Option<f64>,
    /// Ceiling of the transmit window. See [`InterfaceDto::rx_power_low_dbm`].
    pub tx_power_high_dbm: Option<f64>,
    /// The node has not reported this interface recently — treat its numbers as history.
    pub stale: bool,
}

impl InterfaceDto {
    /// Build from an interface-metadata row plus its query-time rates.
    ///
    /// ⚠️ **`InterfaceLive`'s octet rates are BYTES per second** — the name says `bps` and the
    /// contents do not, which is why the ×8 lives at every call site rather than in the store. Drop
    /// it and every interface reads one eighth of its real load, with nothing to show it is wrong.
    #[must_use]
    pub fn from_meta_and_live(
        meta: &InterfaceMeta,
        live: crate::store::InterfaceLive,
        now_s: i64,
        stale_after_s: i64,
    ) -> Self {
        let in_bps = live.in_bps.map(|r| r * 8.0);
        let out_bps = live.out_bps.map(|r| r * 8.0);
        // A zero or absent speed means "unknown", not "0 bps": dividing by it would report an
        // infinite utilization for an interface that never advertised its rate.
        let speed = meta.if_speed.filter(|s| *s > 0);
        let util = |bps: Option<f64>| match (bps, speed) {
            (Some(b), Some(s)) => Some(b / s as f64 * 100.0),
            _ => None,
        };
        Self {
            ifindex: meta.ifindex,
            name: meta.if_name.clone(),
            alias: meta.if_alias.clone(),
            speed: meta.if_speed,
            duplex: meta.if_duplex.clone(),
            if_type: meta.if_type,
            media: meta.if_media.clone(),
            transceiver_model: meta.transceiver_model.clone(),
            last_seen: meta.last_seen_s.map(unix_s_to_rfc3339),
            oper_status: live.oper_status,
            in_bps,
            out_bps,
            in_util_pct: util(in_bps),
            out_util_pct: util(out_bps),
            rx_power_low_dbm: meta.rx_power_low_dbm,
            rx_power_high_dbm: meta.rx_power_high_dbm,
            tx_power_low_dbm: meta.tx_power_low_dbm,
            tx_power_high_dbm: meta.tx_power_high_dbm,
            stale: meta.last_seen_s.is_none_or(|s| now_s - s > stale_after_s),
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

// The dependency-graph DTO is not here: `get_topology` serves `api::topology::TopologyPage`, the
// same type `GET /api/v1/topology` returns. A second, thinner `TopologyEdgeDto` lived here and
// carried only (id, name, parent) — so the AI surface, the one surface whose whole job is
// diagnosis, was the one that could not see node state or root-cause attribution. The canary in
// this module's tests covers the shared type; do not re-add a parallel one.

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
    /// How the answer was reached, when that is not simply "the series". Present when the node
    /// carries several series under this name and they were collapsed to their maximum — a
    /// collapsed number that reads like a plain one is a wrong answer wearing the right shape.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
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

/// One folder group in the inventory tree, sanitized for AI consumption.
///
/// **Not `crate::groups::GroupSummary` served directly**, unlike the `get_topology` move above: that
/// type carries `pool`, the poll-pool assignment, which is a forbidden key here — see this module's
/// canary. The precedent for reusing a REST type applies only when the REST type is already clean.
///
/// Everything else is kept, geo included: "which site is this, and where" is a question an operator
/// asks during an incident, and parity is about which questions can be answered.
#[derive(Debug, Clone, Serialize)]
pub struct NodeGroupDto {
    pub id: Uuid,
    pub name: String,
    /// The folder's kind (`site`, `rack`, …) as a stable key.
    pub group_type: String,
    /// Parent folder; `None` for a root. The tree is rebuilt from this.
    pub parent_id: Option<Uuid>,
    /// Manual order within the parent (siblings sort by this, then by name).
    pub sort_order: f64,
    /// The folder's own coordinates, as stored; `None` when it inherits.
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    /// Where it sits on the map after inheritance: its own, else the nearest placed ancestor's.
    pub effective_latitude: Option<f64>,
    pub effective_longitude: Option<f64>,
    /// `own` | `inherited` | `unset` — whether the effective position is this folder's own.
    ///
    /// The enum itself rather than a string: it has no `as_str()`, only `#[serde(rename_all)]`, so
    /// spelling the tokens out here would create a second spelling with nothing making the two
    /// agree (`testing.md`, "an enum's token and its serde tag").
    pub geo_source: crate::groups::GeoSource,
    /// The folder that supplied the effective position: the map pin this folder's nodes count at.
    pub geo_group: Option<Uuid>,
    /// Direct-member state tallies, when the caller asked for them (ADR-042 I2, the
    /// `/fleet/group-summary` rollup). `None` means "not requested", never "no members" — the
    /// counts cost a second query, so they are opt-in.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state_counts: Option<crate::api::fleet::GroupStateCounts>,
}

impl NodeGroupDto {
    /// Project a repo row, dropping the poll-pool assignment.
    #[must_use]
    pub fn from_summary(g: &crate::groups::GroupSummary) -> Self {
        Self {
            id: g.id,
            name: g.name.clone(),
            group_type: g.group_type.clone(),
            parent_id: g.parent_id,
            sort_order: g.sort_order,
            latitude: g.latitude,
            longitude: g.longitude,
            effective_latitude: g.effective_latitude,
            effective_longitude: g.effective_longitude,
            geo_source: g.geo_source,
            geo_group: g.geo_group,
            state_counts: None,
        }
    }

    /// The same projection, carrying this folder's direct-member state tally.
    #[must_use]
    pub fn with_state(mut self, counts: Option<&crate::api::fleet::GroupStateCounts>) -> Self {
        // A folder with no direct members has no row in the rollup; a zeroed tally is the truthful
        // answer there, and is what the site-matrix widget renders too.
        self.state_counts = Some(counts.cloned().unwrap_or_default());
        self
    }
}

/// What is currently suppressing alerts: planned maintenance windows and reactive mutes.
///
/// Every array in one result rather than a `kind` parameter, because "is the fleet quiet or is it
/// silenced" is one question — the same reasoning `get_neighbors` uses for current + history. The
/// rows are the REST types; none carries a secret, and the canary checks that by type.
///
/// `exemptions` is the negative half and belongs here for the same reason: a node released from
/// its group's window is *not* silenced, and a reader that saw only the window would conclude the
/// opposite.
#[derive(Debug, Clone, Serialize)]
pub struct SuppressionsDto {
    pub maintenance_windows: Vec<crate::maintenance::StoredWindow>,
    pub mutes: Vec<crate::maintenance::StoredMute>,
    pub exemptions: Vec<crate::maintenance::StoredExemption>,
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
    /// Lifecycle state: `queued` / `running` / `done` / `failed` / `cancelled`. Typed rather than
    /// copied as text, so this tool's vocabulary is the one the writers produce.
    pub state: crate::analysis::AnalysisJobState,
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
            state: job.state,
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
    /// `correlation`, or one of the event/flow kinds (`event_storm`, `event_flap`, `severity_shift`,
    /// `rule_gap`, `auth_probe`, `traffic_anomaly`, `talker_shift`, `new_destination`, `flow_scan`,
    /// `saturation`, `incident_correlate`).
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

/// A node's URL check as this surface serves it: the probe definition, with the bound credential
/// lowered to a yes/no (ADR-042 I3b).
///
/// The field it drops is a [`yagra_common::CredentialId`] *reference*, not a secret value. It is
/// dropped anyway, for the reason [`NodeSummaryDto`] drops `Node.credential`: ADR-018's guard is a
/// rule about key names, and an id a model cannot resolve, cannot use, and can only repeat into its
/// own output is not worth the exception. `has_credential` answers the question a model actually
/// has — is this probe authenticated — which the id does not. Same shape as `has_secret` /
/// `has_bind_password` / `has_api_key` elsewhere on this surface.
///
/// The REST body keeps `credential`, and must: the WebUI's edit form prefills the credential
/// selector from it and PUTs the whole config back, and that PUT is a replace, so a form that could
/// not prefill the binding would clear it every time an operator changed a timeout. `mcp/folded.rs`
/// records the divergence in `lowered_to`.
#[derive(Debug, Clone, Serialize)]
pub struct UrlCheckDto {
    /// The URL probed.
    pub url: String,
    /// Request method.
    pub method: yagra_common::HttpMethod,
    /// Which status codes count as healthy.
    pub expected_status: yagra_common::ExpectedStatus,
    /// Whether the TLS certificate chain is verified.
    pub verify_tls: bool,
    /// Whether 3xx redirects are followed.
    pub follow_redirects: bool,
    /// Per-request timeout, in milliseconds.
    pub timeout_ms: u32,
    /// Whether an auth credential is bound. Which one is not served (ADR-018).
    pub has_credential: bool,
    /// The response-body keyword rule, if the monitor has one (ADR-047 Inc.2).
    ///
    /// Served in full, unlike the credential: the keyword and the mode are what an AI client needs
    /// to explain why `http_body_match` is 0, and neither is secret material.
    pub body_match: Option<yagra_common::BodyMatch>,
    /// The JSON extraction rules, if any (ADR-047 Inc.3). These name metrics the monitor reports,
    /// so without them a client sees a series it has no way to explain.
    pub json_extract: Vec<yagra_common::JsonExtract>,
    /// How many bytes of the response body the monitor reads.
    pub body_max_bytes: u32,
}

impl UrlCheckDto {
    /// Project a stored URL check onto the sanitized shape.
    ///
    /// Field by field rather than `#[serde(flatten)]` on purpose: flatten would carry `credential`
    /// straight through, and a field added to [`yagra_common::UrlCheckConfig`] later should have to
    /// be looked at rather than inherited.
    #[must_use]
    pub fn from_config(cfg: &yagra_common::UrlCheckConfig) -> Self {
        Self {
            url: cfg.url.clone(),
            method: cfg.method,
            expected_status: cfg.expected_status.clone(),
            verify_tls: cfg.verify_tls,
            follow_redirects: cfg.follow_redirects,
            timeout_ms: cfg.timeout_ms,
            has_credential: cfg.credential.is_some(),
            body_match: cfg.body_match.clone(),
            json_extract: cfg.json_extract.clone(),
            body_max_bytes: cfg.body_max_bytes,
        }
    }
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
            kind: row.kind.as_str().to_owned(),
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
            action: row.action.as_str().to_owned(),
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
    const SECRET_KEYS: &[&str] = &[
        "credential",
        "community",
        "password",
        "token",
        "auth_key",
        "priv_key",
        "secret",
    ];

    /// Internal identifiers dropped from **inventory** DTOs — a node, a group, an event. These are
    /// not secret; they are noise on a row describing monitored equipment, and `pool` in particular
    /// is a scheduling detail an AI client reasoning about a device has no use for.
    ///
    /// ⚠️ **Kept separate from [`SECRET_KEYS`] deliberately.** They were one list, which read as
    /// "these eight things are equally forbidden" and was not true in either direction: `pool` is
    /// the *answer* when the question is which poller owns a node (ADR-042 I3 folds `/pollers`,
    /// `/pools` and `/nodes/:id/assignment` into a tool), while `credential` is forbidden
    /// everywhere and for a different reason. With one list, relaxing it for the poller tools would
    /// have relaxed it for the node DTOs too.
    const INVENTORY_NOISE_KEYS: &[&str] = &["pool", "profile"];

    /// Recursively assert no object key in `value` is in `banned`.
    fn assert_no_keys(value: &serde_json::Value, banned: &[&str], ctx: &str, why: &str) {
        match value {
            serde_json::Value::Object(map) => {
                for (k, v) in map {
                    assert!(
                        !banned.contains(&k.as_str()),
                        "{why} key {k:?} appeared in a {ctx} DTO"
                    );
                    assert_no_keys(v, banned, ctx, why);
                }
            }
            serde_json::Value::Array(items) => {
                for v in items {
                    assert_no_keys(v, banned, ctx, why);
                }
            }
            _ => {}
        }
    }

    /// The rule every tool result obeys: no secret material, anywhere, at any depth.
    fn assert_no_forbidden_keys(value: &serde_json::Value, ctx: &str) {
        assert_no_keys(value, SECRET_KEYS, ctx, "forbidden");
    }

    /// The stricter rule for a DTO describing monitored equipment: secrets *and* internal ids.
    fn assert_inventory_dto_is_clean(value: &serde_json::Value, ctx: &str) {
        assert_no_forbidden_keys(value, ctx);
        assert_no_keys(value, INVENTORY_NOISE_KEYS, ctx, "internal-inventory");
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
        let dto = NodeSummaryDto::from_node(&node, Some(NodeState::Warning), NodeKind::Url);
        let json = serde_json::to_value(&dto).expect("serialize");
        // Sanity: it carries the safe fields…
        assert_eq!(json["name"], "edge-router-1");
        assert_eq!(json["state"], "warning");
        // The kind is the serde token, not the Debug spelling — a model reads this string and the
        // REST `kind` field must be the same word.
        assert_eq!(json["kind"], "url");
        assert_eq!(json["tags"]["site"], "tokyo");
        // …and not the secret/internal ones.
        assert!(json.get("credential").is_none());
        assert!(json.get("pool").is_none());
        assert!(json.get("profile").is_none());
    }

    /// A URL check with the binding actually set — the only version of this test that proves
    /// anything. `has_credential: false` would pass with a projection that simply forgot the field.
    fn sample_url_check_with_credential() -> yagra_common::UrlCheckConfig {
        let mut cfg = yagra_common::UrlCheckConfig::new("https://api.example.com/health");
        cfg.credential = Some(CredentialId::from(uuid::Uuid::new_v4()));
        cfg
    }

    #[test]
    fn url_check_dto_omits_the_credential_binding() {
        let json =
            serde_json::to_value(UrlCheckDto::from_config(&sample_url_check_with_credential()))
                .expect("serialize");
        // The positive half: the binding is reported, as a yes/no…
        assert_eq!(json["has_credential"], true);
        assert_eq!(json["url"], "https://api.example.com/health");
        // …and the negative half: never as the reference itself.
        assert!(json.get("credential").is_none());
    }

    #[test]
    fn a_url_check_without_a_credential_says_so() {
        let cfg = yagra_common::UrlCheckConfig::new("https://api.example.com/health");
        let json = serde_json::to_value(UrlCheckDto::from_config(&cfg)).expect("serialize");
        assert_eq!(json["has_credential"], false);
    }

    #[test]
    fn every_dto_is_free_of_forbidden_keys() {
        // Build one instance of each DTO with representative data and run the canary over each.
        let node = sample_node_with_secret();

        // Built from a config that *does* carry a binding, so the canary sees the case that would
        // fail rather than the one that trivially passes.
        assert_no_forbidden_keys(
            &serde_json::to_value(UrlCheckDto::from_config(&sample_url_check_with_credential()))
                .unwrap(),
            "UrlCheck",
        );

        let summary = NodeSummaryDto::from_node(&node, Some(NodeState::Ok), NodeKind::Device);
        assert_inventory_dto_is_clean(&serde_json::to_value(&summary).unwrap(), "NodeSummary");

        let status = NodeStatusDto {
            node: summary.clone(),
            alerts: vec![],
            interfaces: vec![InterfaceDto {
                ifindex: 1,
                name: Some("Gig0/1".to_owned()),
                alias: Some("uplink".to_owned()),
                speed: Some(1_000_000_000),
                duplex: Some("full".to_owned()),
                if_type: Some(yagra_common::IF_TYPE_ETHERNET_CSMACD),
                media: Some("1000BASE-T".to_owned()),
                transceiver_model: None,
                last_seen: Some(unix_s_to_rfc3339(0)),
                oper_status: Some(1.0),
                in_bps: Some(4_000_000.0),
                out_bps: Some(500_000.0),
                in_util_pct: Some(0.4),
                out_util_pct: Some(0.05),
                // A populated optical window, not `None`: the canary only sees the fields an
                // instance actually fills, so leaving these empty would exempt them from the
                // forbidden-key scan they exist to be covered by.
                rx_power_low_dbm: Some(-24.0),
                rx_power_high_dbm: Some(-3.0),
                tx_power_low_dbm: Some(-9.0),
                tx_power_high_dbm: Some(-1.0),
                stale: false,
            }],
        };
        assert_inventory_dto_is_clean(&serde_json::to_value(&status).unwrap(), "NodeStatus");

        let alert = AlertDto {
            node_id: Some(node.id.0),
            subject_kind: "node".to_owned(),
            subject_name: None,
            check_id: uuid::Uuid::new_v4(),
            node_name: Some("edge-router-1".to_owned()),
            severity: "critical".to_owned(),
            state: "unreachable".to_owned(),
            metric: "icmp_rtt_ms".to_owned(),
            fired_at: unix_ms_to_rfc3339(0),
            root_cause: None,
            // Populated, not `None`: the canary only scans the fields an instance actually fills.
            ifindex: Some(7),
            flapping: false,
            breach: Some(BreachDto {
                value: 900.0,
                threshold: Some(500.0),
                direction: "above".to_owned(),
            }),
        };
        assert_no_forbidden_keys(&serde_json::to_value(&alert).unwrap(), "Alert");

        // `get_topology`'s four branches and `list_discovered_endpoints` stood here, one
        // hand-built instance each. ADR-085 Inc.3 moved them to `folded::FOLDED_READS`, where
        // `every_folded_result_is_free_of_forbidden_keys` walks their response schema instead —
        // every field of every nested type, rather than the fields these samples happened to set.
        // The discovered-endpoint shape is the one that most wanted the stronger check: it is the
        // only tool result built from what a *device* volunteered about hosts nobody registered,
        // and it names a router and a port beside each address.

        let series = MetricSeriesDto {
            node_id: node.id.0,
            metric: "cpu_percent".to_owned(),
            mode: "range".to_owned(),
            latest: None,
            points: vec![MetricPointDto { t: 0, v: 42.0 }],
            note: Some("collapsed the node's 15 table-row series to their maximum".to_owned()),
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
            state: crate::analysis::AnalysisJobState::Done,
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
            kind: yagra_bus::EventKind::Trap,
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
            // Was "raised", which the pipeline has never produced — the String field let the
            // fixture name an outcome that does not exist.
            action: crate::events::EventAction::Fired,
        };
        let event_json =
            serde_json::to_value(EventDto::from_row(&event, Some("edge-router-1".to_owned())))
                .unwrap();
        assert!(
            event_json.get("pool").is_none(),
            "pool dropped from EventDto"
        );
        assert_inventory_dto_is_clean(&event_json, "Event");

        // ADR-042 I1. A REST type served directly, clean as it stands, so a parallel DTO would
        // only add drift. Two siblings stood here — the topology page and the interface series —
        // and both moved to `folded::FOLDED_READS` in ADR-085 Inc.3, where the walk reads their
        // schema instead of this sample. `top_interfaces` did not: it answers two REST routes
        // (`interface-top` and `interface-delta`) with no argument naming which, so it has no
        // branch key to file rows under.
        let top_ifaces = crate::api::util::Ranked {
            entries: vec![crate::api::metrics::InterfaceTopEntry {
                node_id: node.id.0,
                node_name: "edge-router-1".to_owned(),
                ifindex: 1,
                if_name: Some("Gi0/1".to_owned()),
                if_alias: Some("uplink".to_owned()),
                if_speed_bps: Some(1_000_000_000),
                value: 42.0,
            }],
            partial: false,
        };
        assert_no_forbidden_keys(
            &serde_json::to_value(&top_ifaces).unwrap(),
            "RankedInterfaceTopEntry",
        );

        // `GroupSummary` is the one that is *not* clean — it carries `pool` — which is why this DTO
        // exists rather than the row being served directly.
        let group = crate::groups::GroupSummary {
            id: uuid::Uuid::new_v4(),
            name: "Tokyo".to_owned(),
            group_type: "site".to_owned(),
            parent_id: None,
            sort_order: 1.0,
            latitude: Some(35.6),
            longitude: Some(139.7),
            effective_latitude: Some(35.6),
            effective_longitude: Some(139.7),
            geo_source: crate::groups::GeoSource::Own,
            geo_group: None,
            pool: Some("tokyo".to_owned()),
        };
        let group_json = serde_json::to_value(NodeGroupDto::from_summary(&group)).unwrap();
        assert!(
            group_json.get("pool").is_none(),
            "the poll-pool assignment is dropped from NodeGroupDto"
        );
        assert_eq!(
            group_json["geo_source"], "own",
            "the enum keeps its serde tag"
        );
        assert_inventory_dto_is_clean(&group_json, "NodeGroup");

        let history = AlertHistoryDto::from_row(
            &AlertHistoryRow {
                id: Uuid::new_v4(),
                node: Some(node.id.0),
                subject_kind: yagra_alert::SubjectKind::Node,
                subject_name: None,
                check: uuid::Uuid::new_v4(),
                severity: yagra_common::Severity::Critical,
                state: NodeState::Unreachable,
                metric: Some("icmp_rtt_ms".to_owned()),
                resolved: false,
                at_unix_ms: 0,
                observed_value: Some(1.0),
                threshold_value: Some(2.0),
                direction: None,
                recorded_at: "1970-01-01T00:00:00Z".to_owned(),
                ifindex: None,
            },
            Some("edge-router-1".to_owned()),
        );
        assert_no_forbidden_keys(&serde_json::to_value(&history).unwrap(), "AlertHistory");

        // ── Types tools serve straight through, found by `every_typed_tool_result_is_canaried` ──

        // `FlowRows` is `#[serde(untagged)]`, so it serializes as the bare array the REST edge
        // returns; one variant is enough to prove the wrapper adds no key of its own.
        let flow_rows = crate::api::flow::FlowRows::Talkers(vec![crate::flowstore::FlowTalker {
            addr: "10.0.0.1".to_owned(),
            bytes: 1,
            packets: 1,
            flows: 1,
        }]);
        assert_no_forbidden_keys(&serde_json::to_value(&flow_rows).unwrap(), "FlowRows");

        let fanout = vec![crate::flowstore::FlowFanout {
            src: "10.0.0.1".to_owned(),
            distinct_dst: 2,
            distinct_ports: 3,
            flows: 4,
            bytes: 5,
        }];
        assert_no_forbidden_keys(&serde_json::to_value(&fanout).unwrap(), "FlowFanout");

        // ⚠️ **Not** `assert_inventory_dto_is_clean`: this one carries `pool` on purpose. A poll
        // receipt saying which pool the jobs went to is the answer to "why did nothing happen" —
        // the node's *effective* pool may be inherited, and the caller cannot derive it. That is
        // the distinction the two-tier split exists to express; under the old single list this
        // type was simply never checked at all.
        let poll = crate::api::nodes::PollNowResult {
            dispatched: 3,
            node_id: node.id.0,
            pool: "tokyo".to_owned(),
        };
        let poll_json = serde_json::to_value(&poll).unwrap();
        assert_no_forbidden_keys(&poll_json, "PollNowResult");
        assert_eq!(
            poll_json["pool"], "tokyo",
            "the dispatch pool is part of the receipt, not leakage"
        );

        // ── ADR-042 I2 tool results ─────────────────────────────────────────────────────────────

        let suppressions = SuppressionsDto {
            maintenance_windows: vec![crate::maintenance::StoredWindow {
                id: uuid::Uuid::new_v4(),
                name: "core upgrade".to_owned(),
                level: crate::maintenance::WindowScope::Node,
                scope_id: node.id.0.to_string(),
                starts_at: "1970-01-01T00:00:00Z".to_owned(),
                ends_at: "1970-01-01T01:00:00Z".to_owned(),
                enabled: true,
                active: false,
            }],
            mutes: vec![crate::maintenance::StoredMute {
                id: uuid::Uuid::new_v4(),
                scope_kind: crate::maintenance::MuteScope::Node,
                node_id: Some(node.id.0),
                group_id: None,
                check_name: Some("icmp_rtt_ms".to_owned()),
                until_at: "1970-01-01T02:00:00Z".to_owned(),
                reason: Some("known noisy link".to_owned()),
            }],
            exemptions: vec![crate::maintenance::StoredExemption {
                id: uuid::Uuid::new_v4(),
                kind: crate::maintenance::ExemptionKind::Maintenance,
                node_id: node.id.0,
                until_at: "1970-01-01T01:00:00Z".to_owned(),
            }],
        };
        assert_no_forbidden_keys(
            &serde_json::to_value(&suppressions).unwrap(),
            "Suppressions",
        );
    }

    /// **Every DTO in this module is covered by the canary above.**
    ///
    /// That canary is a hand-written body listing one instance per type, and a hand-maintained list
    /// that mirrors another list drifts — this one already had: `AlertHistoryDto` shipped uncovered,
    /// so the forbidden-key check had a hole in it for as long as that type existed, and nothing
    /// said so. `testing.md` names exactly this shape ("a hand-maintained list that mirrors a
    /// directory or another list: pin it to its source"), so the list is now pinned to the module's
    /// own declarations.
    ///
    /// This turns "someone forgets a DTO" from *will happen again* into *cannot*, at the cost of
    /// one line per new type — which is the line that was being forgotten anyway.
    #[test]
    fn the_canary_covers_every_dto_in_this_module() {
        let src = include_str!("dto.rs");
        // Needles assembled at runtime: this test reads its own file, so a literal would match
        // itself and the assertion would pass forever without checking anything.
        let decl = format!("pub {} ", "struct");
        let canary_fn = format!("fn {}_dto_is_free_of_forbidden_keys", "every");
        let after = src
            .split(&canary_fn)
            .nth(1)
            .expect("the canary test must exist for this guard to mean anything");
        // Bounded to the canary's own body. Without this the window runs to the end of the file and
        // a type merely *mentioned* by some later test would count as covered — the guard would
        // still pass while the canary skipped it.
        let body = after
            .split(&format!("#[{}]", "test"))
            .next()
            .unwrap_or(after);
        let mut checked = 0usize;
        let mut missing = Vec::new();
        for chunk in src.split(&decl).skip(1) {
            let name: String = chunk
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            if name.is_empty() {
                continue;
            }
            checked += 1;
            if !body.contains(&name) {
                missing.push(name);
            }
        }
        assert!(
            checked >= 15,
            "only found {checked} DTO declarations — the parser drifted"
        );
        assert!(
            missing.is_empty(),
            "these DTOs are declared here but never reach the forbidden-key canary — add an \
             instance of each to it: {missing:?}"
        );
    }

    /// Every tool that serializes a **shared type**, and the canary label covering that type.
    ///
    /// The sibling guard above only reaches types declared in this module. A tool may also serve a
    /// REST type straight through (`api::topology::TopologyPage`, `api::metrics::InterfaceSeries`,
    /// …), and for those there is nothing in this file to enumerate — so they were added to the
    /// canary by hand, and forgetting one cost nothing. It had already cost something: `poll_now`
    /// serialized `api::nodes::PollNowResult`, which carries `pool`, from the day ADR-028 WS-E
    /// shipped it. Nothing failed, because no instance of that type ever reached the canary.
    ///
    /// One row per `ok_json` call site. Adding a tool that returns a type means adding a line here
    /// and an instance to the canary; that is the whole cost, and it is the cost of the line that
    /// was being skipped.
    ///
    /// A tool may hold **several** rows: `alert_trends` and `list_analyses` fold endpoints whose
    /// row type differs by `kind`, so each shape it can return needs its own coverage.
    const TOOL_RESULT_TYPES: &[(&str, &str)] = &[
        ("list_nodes", "NodeSummary"),
        ("get_node_status", "NodeStatus"),
        ("get_active_alerts", "Alert"),
        ("get_alert_history", "AlertHistory"),
        ("query_metrics", "MetricSeries"),
        ("top_interfaces", "RankedInterfaceTopEntry"),
        ("list_node_groups", "NodeGroup"),
        ("top_flows", "FlowRows"),
        ("flow_fanout", "FlowFanout"),
        ("search_events", "Event"),
        ("poll_now", "PollNowResult"),
        ("list_suppressions", "Suppressions"),
    ];

    /// Tools whose result is a `serde_json::json!` object built in the tool body. There is no
    /// shared type to instantiate, and the keys are visible at the call site, so the canary has
    /// nothing to hold — but the tool still has to be accounted for, or "not in either list" would
    /// be the quiet way to skip the check.
    const TOOLS_WITH_INLINE_RESULTS: &[(&str, &str)] = &[
        (
            "get_neighbors",
            "a json! of `current` + `history`, both api::neighbors types with no secret-bearing field",
        ),
        (
            "run_analysis",
            "a json! wrapper around findings already covered as AnalysisFinding",
        ),
        (
            "get_analysis_findings",
            "a json! wrapper around findings already covered as AnalysisFinding",
        ),
        ("event_stats", "a json! of counts built from group-by rows"),
        ("ack_alert", "a json! acknowledgement receipt: ids and a count"),
        (
            "open_maintenance",
            "a json! receipt carrying the created window's id and bounds",
        ),
    ];

    /// The distinct tool names passed as the first argument to `fname(` in `src`.
    ///
    /// Tolerates the call being wrapped across lines, which a literal `fname("` needle does not —
    /// see the note in [`every_typed_tool_result_is_canaried`]. Anything that is neither
    /// `fname(<whitespace>"…"` nor `fname(<whitespace>TOOL` is skipped rather than guessed at.
    ///
    /// **It follows `const TOOL` because ADR-085 Inc.2 moved the name there** — a tool body used to
    /// spell its own name at every call site, and now declares it once at the top and passes the
    /// constant. Resolving it by taking the nearest preceding declaration is exact rather than a
    /// heuristic: a `const` is function-scoped, so a body using `TOOL` without declaring one does
    /// not compile, and every declaration is its function's first statement.
    ///
    /// 🚨 **That increment broke this guard and only its floor noticed.** After the conversion the
    /// literal needle matched nothing, so the loop below had no sites to check and the whole
    /// canary would have passed while verifying nothing — `assert!(sites.len() >= 22)` is what
    /// turned that into a failure. Keep that assertion, and keep it above the loop.
    fn call_sites(src: &str, fname: &str) -> Vec<String> {
        // `const TOOL: &str = "` — assembled rather than written, so this stays safe if the helper
        // is ever pointed at a file that contains it (this one does not; `dto.rs` reads `tools.rs`).
        let const_decl = format!("const {}: &str = {}", "TOOL", '"');
        let needle = format!("{fname}(");
        let mut out: Vec<String> = Vec::new();
        let mut at = 0usize;
        while let Some(rel) = src[at..].find(&needle) {
            let start = at + rel + needle.len();
            at = start;
            let rest = src[start..].trim_start();
            let name: String = if let Some(inner) = rest.strip_prefix('"') {
                inner.chars().take_while(|c| *c != '"').collect()
            } else if rest
                .strip_prefix("TOOL")
                .is_some_and(|t| !t.starts_with(|c: char| c.is_alphanumeric() || c == '_'))
            {
                match src[..start].rfind(&const_decl) {
                    Some(k) => src[k + const_decl.len()..]
                        .chars()
                        .take_while(|c| *c != '"')
                        .collect(),
                    None => continue,
                }
            } else {
                continue;
            };
            if !name.is_empty() && !out.contains(&name) {
                out.push(name);
            }
        }
        out
    }

    /// **No tool is covered by both tables** (ADR-085 Inc.3).
    ///
    /// [`every_typed_tool_result_is_canaried`] skips any tool with a row in `FOLDED_READS`, because
    /// the schema walk over there checks the same thing and checks it harder. So a tool listed in
    /// both is a `TOOL_RESULT_TYPES` row **nothing reads** — and, worse, an instance in the canary
    /// that exists only to satisfy a claim no longer being made. A row nobody reads is the kind
    /// that goes on asserting something after it stops being true.
    ///
    /// This is not hypothetical: `("get_fleet_summary", "FleetSummary")` had been dead since
    /// ADR-042 I3a folded that tool's `coverage` branch, and was removed by the increment that
    /// added this test. Nothing had noticed, because being covered twice looks exactly like being
    /// covered once from either side.
    #[test]
    fn no_tool_is_covered_by_both_the_folded_table_and_the_instance_canary() {
        let folded: std::collections::BTreeSet<&str> = crate::mcp::folded::FOLDED_READS
            .iter()
            .map(|f| f.tool)
            .collect();
        let both: Vec<&str> = TOOL_RESULT_TYPES
            .iter()
            .map(|(t, _)| *t)
            .filter(|t| folded.contains(t))
            .collect();
        assert!(
            both.is_empty(),
            "{both:?} appear in both TOOL_RESULT_TYPES and FOLDED_READS. The canary skips a folded \
             tool, so those rows are read by nothing — drop them, and drop the instances they \
             claim, or the canary keeps carrying values for a check that no longer runs"
        );
        // …and the same rule for the inline list, which the canary skips for a different reason.
        let inline: Vec<&str> = TOOLS_WITH_INLINE_RESULTS
            .iter()
            .map(|(t, _)| *t)
            .filter(|t| folded.contains(t))
            .collect();
        assert!(
            inline.is_empty(),
            "{inline:?} are folded, so saying they build their result inline explains nothing that \
             is still checked"
        );
        // The load-bearing half: an empty `folded` makes "no overlap" vacuously true, and it is
        // built by iterating at run time, so nothing else here would notice.
        //
        // `TOOL_RESULT_TYPES` needs no such assertion and must not be given one — it is a `const`,
        // so clippy folds `is_empty()` to a literal and refuses the line as an expression that
        // always evaluates the same way. What guards *that* table against being emptied is
        // [`every_typed_tool_result_is_canaried`]'s own floor.
        assert!(
            !folded.is_empty(),
            "the folded table came back empty; this test proves nothing"
        );
    }

    /// **Every `ok_json` call site names a type the canary instantiates.**
    ///
    /// Reads `tools.rs` for the call sites rather than the tool declarations, because the risk is
    /// specifically about *what a tool serializes* — a tool can exist without returning a shared
    /// type, and the write tools do.
    #[test]
    fn every_typed_tool_result_is_canaried() {
        let tools_src = crate::mcp::tool_source::tool_surface();
        let tools_src = tools_src.as_str();
        let dto_src = include_str!("dto.rs");
        // `tools.rs` is a different file, so a literal needle is safe here; the canary-body needles
        // below read this file and are assembled at runtime for the usual reason.
        //
        // The tool name is read *after skipping whitespace*, not straight after the paren. The
        // needle used to be `ok_json("`, which silently missed a call rustfmt had wrapped —
        // `list_suppressions` was already invisible to it, and since the floor sat exactly on the
        // count, the next tool written that way would have been skipped without moving it.
        let sites = call_sites(tools_src, "ok_json");
        assert!(
            sites.len() >= 22,
            "only matched {} ok_json call sites; the parser drifted",
            sites.len()
        );

        let canary_fn = format!("fn {}_dto_is_free_of_forbidden_keys", "every");
        let after = dto_src
            .split(&canary_fn)
            .nth(1)
            .expect("the canary test must exist for this guard to mean anything");
        let body = after
            .split(&format!("#[{}]", "test"))
            .next()
            .unwrap_or(after);

        for tool in &sites {
            // A tool whose branches are all in the folded table is already checked, and checked
            // harder: `folded::every_folded_result_is_free_of_forbidden_keys` walks the OpenAPI
            // schema of each branch's response, which sees every field of every nested type —
            // where an instance only shows the fields that instance happened to populate. Asking
            // for a hand-built instance *as well* would be ~20 of them for `get_system_health`
            // alone, to prove less. So the schema walk discharges the obligation for these.
            if crate::mcp::folded::FOLDED_READS
                .iter()
                .any(|f| f.tool == *tool)
            {
                continue;
            }
            let labels: Vec<_> = TOOL_RESULT_TYPES
                .iter()
                .filter(|(t, _)| t == tool)
                .map(|(_, l)| *l)
                .collect();
            assert!(
                !labels.is_empty(),
                "MCP tool `{tool}` serializes a shared type that nothing checks. Either add a row \
                 to TOOL_RESULT_TYPES plus an instance in the canary, or — if every branch of the \
                 tool mirrors a REST route — add its rows to `folded::FOLDED_READS`, which checks \
                 the response schema instead and needs no instance."
            );
            for label in labels {
                assert!(
                    body.contains(label),
                    "`{tool}` claims canary label {label:?}, but the forbidden-key canary never \
                     instantiates it — add an instance so the type is actually checked"
                );
            }
        }

        // The inline list is accounted for too: a reason, and no overlap with the typed list.
        for (tool, why) in TOOLS_WITH_INLINE_RESULTS {
            assert!(
                why.len() >= 20,
                "`{tool}` is listed as an inline result with no real explanation"
            );
            assert!(
                !TOOL_RESULT_TYPES.iter().any(|(t, _)| t == tool),
                "`{tool}` is in both result lists; it can only be one"
            );
        }

        // …and the other direction, which nothing checked before: a tool that builds its result
        // inline must *say* so. Without this, "in neither list" was the quiet way past both halves
        // of the canary — the exact shape of hole that let `poll_now` return `pool` uncovered.
        for tool in call_sites(tools_src, "ok_json_value") {
            assert!(
                TOOLS_WITH_INLINE_RESULTS.iter().any(|(t, _)| *t == tool),
                "MCP tool `{tool}` builds its result inline but is not listed in \
                 TOOLS_WITH_INLINE_RESULTS; add a row saying what it returns and why the canary \
                 has nothing to hold"
            );
        }
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
