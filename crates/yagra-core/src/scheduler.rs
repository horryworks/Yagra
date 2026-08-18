// SPDX-License-Identifier: AGPL-3.0-only
//! Job scheduling: turn inventory into [`PollJob`]s for the bus.
//!
//! The scheduler is the core-side producer of work. For the walking skeleton it builds
//! one ICMP job per node; the full scheduler adds per-metric intervals, jitter, and
//! pool-aware dispatch (ADR-009). Jobs carry everything the poller needs (ADR-003), so
//! this is pure given a node.

use crate::collection::CollectionRepo;
use crate::dns_check::DnsCheckRepo;
use crate::neighbors::AdjacencySettings;
use crate::routing::RoutingPlan;
use crate::secrets::{self, CredentialStore, SnmpV3Secret};
use crate::url_check::UrlCheckRepo;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;
use yagra_bus::{
    DnsCheck, HttpCheck, IcmpCheck, JobSpec, NatsBus, OpticalProbe, PollJob, SnmpArpCheck,
    SnmpArpColumn, SnmpCheck, SnmpColumn, SnmpL3Check, SnmpL3Column, SnmpMauCheck, SnmpMetaColumn,
    SnmpNeighborCheck, SnmpNeighborColumn, SnmpOpticalCheck, SnmpRouteProbe, SnmpRoutingCheck,
    SnmpRoutingColumn, SnmpTableCheck, SnmpV3ArpCheck, SnmpV3Check, SnmpV3L3Check, SnmpV3MauCheck,
    SnmpV3NeighborCheck, SnmpV3OpticalCheck, SnmpV3RoutingCheck, SnmpV3TableCheck, SyncBus,
};
use yagra_common::{
    builtin_arp_columns, builtin_interface_meta_columns, builtin_l3_columns,
    builtin_neighbor_columns, builtin_routing_columns, route_probe_columns, route_probe_oid,
    CollectionItem, CollectionKind, DnsCheckConfig, HttpAuth, Node, NodeId, NodeKind, NodeRows,
    OpticalFlavor, ProfileId, UrlCheckConfig, METRIC_CISCO_TEMP_C, METRIC_IF_RX_POWER_DBM,
    METRIC_IF_TX_POWER_DBM,
};

/// The effective polling interval (seconds) for a node: its profile's override if one is set, else
/// the global default. Pure (no I/O) so the scheduler's resolution is unit-testable.
#[must_use]
pub fn resolve_interval(
    profile: Option<ProfileId>,
    overrides: &HashMap<Uuid, u32>,
    default_secs: u32,
) -> u32 {
    profile
        .and_then(|p| overrides.get(&p.0).copied())
        .unwrap_or(default_secs)
}

/// Whether a node is due to poll, given the time elapsed since its last dispatch. `None` ⇒ never
/// dispatched (due immediately). Pure so the due-check is unit-testable without a clock.
#[must_use]
pub fn due(elapsed_since_last: Option<Duration>, interval: Duration) -> bool {
    match elapsed_since_last {
        Some(elapsed) => elapsed >= interval,
        None => true,
    }
}

/// Whether a pool is served in **working-set** mode on a sweep: exactly when it has ≥1 live poller
/// (`live_pools`, from the coordinator). Legacy per-job mode is the strict complement, so the
/// scheduler runs each pool in one mode or the other — never both — and a node is never
/// double-polled (ADR-009). Pure so the per-pool mode decision is unit-testable.
#[must_use]
pub fn pool_uses_working_set(pool: &str, live_pools: &std::collections::HashSet<String>) -> bool {
    live_pools.contains(pool)
}

/// Per-poll SNMP timeout pushed to the poller (ms). Matches the periodic and on-demand paths.
const SNMP_TIMEOUT_MS: u32 = 2000;

/// Build an ICMP poll job targeting a node's management address.
#[must_use]
pub fn build_icmp_job(node: &Node, check: IcmpCheck, interval_secs: u32, job_id: Uuid) -> PollJob {
    PollJob::icmp(job_id, node.id, node.address, check, interval_secs)
}

/// Build an SNMP v2c scalar poll job targeting a node's management address.
#[must_use]
pub fn build_snmp_job(node: &Node, check: SnmpCheck, interval_secs: u32, job_id: Uuid) -> PollJob {
    PollJob::snmp(job_id, node.id, node.address, check, interval_secs)
}

/// Build an SNMP v2c table-walk poll job targeting a node's management address.
#[must_use]
pub fn build_snmp_table_job(
    node: &Node,
    check: SnmpTableCheck,
    interval_secs: u32,
    job_id: Uuid,
) -> PollJob {
    PollJob::snmp_table(job_id, node.id, node.address, check, interval_secs)
}

/// Build an SNMP v3 (USM) scalar poll job targeting a node's management address.
#[must_use]
pub fn build_snmp_v3_job(
    node: &Node,
    check: SnmpV3Check,
    interval_secs: u32,
    job_id: Uuid,
) -> PollJob {
    PollJob::snmp_v3(job_id, node.id, node.address, check, interval_secs)
}

/// Build an SNMP v3 (USM) table-walk poll job targeting a node's management address.
#[must_use]
pub fn build_snmp_v3_table_job(
    node: &Node,
    check: SnmpV3TableCheck,
    interval_secs: u32,
    job_id: Uuid,
) -> PollJob {
    PollJob::snmp_v3_table(job_id, node.id, node.address, check, interval_secs)
}

/// Map a stored [`UrlCheckConfig`] into the bus [`HttpCheck`]. Auth is not inlined yet (MVP probe
/// is unauthenticated); when it lands, core resolves/decrypts `cfg.credential` here (ADR-018/020).
#[must_use]
pub fn build_http_check(cfg: &UrlCheckConfig, auth: Option<HttpAuth>) -> HttpCheck {
    HttpCheck {
        url: cfg.url.clone(),
        method: cfg.method,
        expected_status: cfg.expected_status.clone(),
        verify_tls: cfg.verify_tls,
        follow_redirects: cfg.follow_redirects,
        timeout_ms: cfg.timeout_ms,
        auth,
        // Carried through unchanged — they hold no secret, so unlike `auth` there is nothing for
        // core to resolve. `body_match`'s presence is what makes the coordinator demand
        // `CAP_HTTP_BODY` of whichever poller the spec lands on; `json_extract`'s deliberately
        // does not (see `spec_required_caps`).
        body_match: cfg.body_match.clone(),
        json_extract: cfg.json_extract.clone(),
        body_max_bytes: cfg.body_max_bytes,
    }
}

/// Build an HTTP/HTTPS URL-monitor poll job. `target` is the node's management address (display /
/// optional ICMP); the real request target is `check.url`.
#[must_use]
pub fn build_http_job(node: &Node, check: HttpCheck, interval_secs: u32, job_id: Uuid) -> PollJob {
    PollJob::http(job_id, node.id, node.address, check, interval_secs)
}

/// Map a stored [`DnsCheckConfig`] into the bus [`DnsCheck`] (ADR-033).
#[must_use]
pub fn build_dns_check(cfg: &DnsCheckConfig) -> DnsCheck {
    DnsCheck {
        name: cfg.name.clone(),
        record_type: cfg.record_type,
        resolver: cfg.resolver,
        resolver_port: cfg.resolver_port,
        max_depth: cfg.max_depth,
        timeout_ms: cfg.timeout_ms,
    }
}

/// Build a DNS-monitor poll job. `target` is the node's display address, which a DNS monitor never
/// pings; the real target is the resolver inside `check`.
#[must_use]
pub fn build_dns_job(node: &Node, check: DnsCheck, interval_secs: u32, job_id: Uuid) -> PollJob {
    PollJob::dns(job_id, node.id, node.address, check, interval_secs)
}

/// Build the SNMP v3 scalar check for a node from its resolved collection set and v3
/// credential. `None` when the set has no scalar items. (Table items become a separate
/// [`build_snmp_v3_table_check`] job — the v3 analogue of the v2c scalar/table split.)
#[must_use]
pub fn build_snmp_v3_check(
    secret: &SnmpV3Secret,
    items: &[CollectionItem],
    timeout_ms: u32,
) -> Option<SnmpV3Check> {
    let scalar: Vec<SnmpColumn> = items
        .iter()
        .filter(|i| i.kind == CollectionKind::Scalar)
        .map(|i| SnmpColumn {
            metric_name: i.metric_name.clone(),
            oid: i.oid.clone(),
            kind: i.metric_kind,
        })
        .collect();
    (!scalar.is_empty()).then(|| SnmpV3Check {
        user: secret.user.clone(),
        security_level: secret.security_level.clone(),
        auth_protocol: secret.auth_protocol.clone(),
        auth_key: secret.auth_key.clone(),
        priv_protocol: secret.priv_protocol.clone(),
        priv_key: secret.priv_key.clone(),
        oids: Vec::new(),
        columns: scalar,
        timeout_ms,
    })
}

/// Build the SNMP v3 (USM) table-walk check for a node from its resolved collection set and v3
/// credential — the v3 analogue of the table half of [`build_snmp_checks`]. `None` when the set
/// has no table items. Table columns become walk columns, plus the standard interface-metadata
/// columns (ifName/ifAlias/ifSpeed) so discovered interfaces get names (PostgreSQL metadata, never
/// TSDB labels — ADR-011).
#[must_use]
pub fn build_snmp_v3_table_check(
    secret: &SnmpV3Secret,
    items: &[CollectionItem],
    timeout_ms: u32,
) -> Option<SnmpV3TableCheck> {
    let table: Vec<SnmpColumn> = items
        .iter()
        .filter(|i| i.kind == CollectionKind::Table)
        .map(|i| SnmpColumn {
            metric_name: i.metric_name.clone(),
            oid: i.oid.clone(),
            kind: i.metric_kind,
        })
        .collect();
    (!table.is_empty()).then(|| SnmpV3TableCheck {
        user: secret.user.clone(),
        security_level: secret.security_level.clone(),
        auth_protocol: secret.auth_protocol.clone(),
        auth_key: secret.auth_key.clone(),
        priv_protocol: secret.priv_protocol.clone(),
        priv_key: secret.priv_key.clone(),
        columns: table,
        meta_columns: builtin_interface_meta_columns()
            .into_iter()
            .map(|(field, oid)| SnmpMetaColumn {
                field,
                oid: oid.to_owned(),
            })
            .collect(),
        timeout_ms,
    })
}

/// Split a resolved collection set into the scalar GET check and the table-walk check for a
/// node, given the resolved `community`. Each is `None` when that shape has no items.
///
/// Scalar items become named [`SnmpColumn`]s so their configured metric names are honoured
/// (not just the poller's built-in OID map). Table items become walk columns, plus the
/// standard interface-metadata columns (ifName/ifAlias/ifSpeed) so discovered interfaces get
/// names — those are PostgreSQL metadata, never TSDB labels (ADR-011).
#[must_use]
pub fn build_snmp_checks(
    community: &str,
    items: &[CollectionItem],
    timeout_ms: u32,
) -> (Option<SnmpCheck>, Option<SnmpTableCheck>) {
    let to_column = |i: &CollectionItem| SnmpColumn {
        metric_name: i.metric_name.clone(),
        oid: i.oid.clone(),
        kind: i.metric_kind,
    };
    let scalar: Vec<SnmpColumn> = items
        .iter()
        .filter(|i| i.kind == CollectionKind::Scalar)
        .map(to_column)
        .collect();
    let table: Vec<SnmpColumn> = items
        .iter()
        .filter(|i| i.kind == CollectionKind::Table)
        .map(to_column)
        .collect();

    let scalar_check = (!scalar.is_empty()).then(|| SnmpCheck {
        community: community.to_owned(),
        oids: Vec::new(),
        columns: scalar,
        timeout_ms,
    });
    let table_check = (!table.is_empty()).then(|| SnmpTableCheck {
        community: community.to_owned(),
        columns: table,
        meta_columns: builtin_interface_meta_columns()
            .into_iter()
            .map(|(field, oid)| SnmpMetaColumn {
                field,
                oid: oid.to_owned(),
            })
            .collect(),
        timeout_ms,
    });
    (scalar_check, table_check)
}

/// Group a node's optical collection items into one probe per dialect (ADR-062).
///
/// An item's `oid` names the dialect and its `metric_name` says which of the two readings it is,
/// so an operator can disable either half at node scope and get exactly that. Items whose OID no
/// dialect claims are dropped here rather than sent: core is where a metric name is decided, and
/// a probe the poller cannot interpret is a job it would spend a session on for nothing.
fn optical_probes(items: &[CollectionItem]) -> Vec<OpticalProbe> {
    let mut probes: Vec<OpticalProbe> = Vec::new();
    for item in items.iter().filter(|i| i.kind == CollectionKind::Optical) {
        let Some(flavor) = OpticalFlavor::from_root(&item.oid) else {
            continue;
        };
        let probe = match probes.iter_mut().find(|p| p.flavor == flavor) {
            Some(p) => p,
            None => {
                probes.push(OpticalProbe {
                    flavor,
                    rx_metric: None,
                    tx_metric: None,
                    temp_metric: None,
                });
                probes.last_mut().expect("just pushed")
            }
        };
        match item.metric_name.as_str() {
            METRIC_IF_RX_POWER_DBM => probe.rx_metric = Some(item.metric_name.clone()),
            METRIC_IF_TX_POWER_DBM => probe.tx_metric = Some(item.metric_name.clone()),
            // Not an optical reading — the chassis sensors in the same table (ADR-070). It shares
            // the probe so the table is walked once; `optical_probes` groups by `flavor`, so a
            // node carrying both the Cisco optical template and the temperature template lands
            // both metric names on one probe.
            METRIC_CISCO_TEMP_C => probe.temp_metric = Some(item.metric_name.clone()),
            // The API edge refuses any other name for an optical item; a row that predates that
            // rule (or arrived by config bundle) is skipped rather than published under a name
            // the poller has no reading for.
            _ => {}
        }
    }
    probes.retain(|p| p.rx_metric.is_some() || p.tx_metric.is_some() || p.temp_metric.is_some());
    probes
}

/// Build the SNMP v2c optical-transceiver check, or `None` when the node collects no optics.
#[must_use]
pub fn build_snmp_optical_check(
    community: &str,
    items: &[CollectionItem],
    timeout_ms: u32,
) -> Option<SnmpOpticalCheck> {
    let probes = optical_probes(items);
    (!probes.is_empty()).then(|| SnmpOpticalCheck {
        community: community.to_owned(),
        probes,
        timeout_ms,
    })
}

/// Build the SNMP v3 (USM) optical-transceiver check — the v3 analogue of
/// [`build_snmp_optical_check`].
#[must_use]
pub fn build_snmp_v3_optical_check(
    secret: &SnmpV3Secret,
    items: &[CollectionItem],
    timeout_ms: u32,
) -> Option<SnmpV3OpticalCheck> {
    let probes = optical_probes(items);
    (!probes.is_empty()).then(|| SnmpV3OpticalCheck {
        user: secret.user.clone(),
        security_level: secret.security_level.clone(),
        auth_protocol: secret.auth_protocol.clone(),
        auth_key: secret.auth_key.clone(),
        priv_protocol: secret.priv_protocol.clone(),
        priv_key: secret.priv_key.clone(),
        probes,
        timeout_ms,
    })
}

/// Build the SNMP v2c media-type check for a node (ADR-063 Inc.2).
///
/// Takes no items and no columns, for the same reason [`build_snmp_neighbor_check`] does not: the
/// OID set is a fixed standard, so there is nothing per-device to configure — only whether to
/// collect and how often, which is a deployment-wide setting. And it *could* not be a collection
/// template, because a `CollectionItem` declares a TSDB series and a media type is a string.
#[must_use]
pub fn build_snmp_mau_check(community: &str, timeout_ms: u32) -> SnmpMauCheck {
    SnmpMauCheck {
        community: community.to_owned(),
        entity_fallback: true,
        timeout_ms,
    }
}

/// Build the SNMP v3 (USM) media-type check — the v3 analogue of [`build_snmp_mau_check`].
#[must_use]
pub fn build_snmp_v3_mau_check(secret: &SnmpV3Secret, timeout_ms: u32) -> SnmpV3MauCheck {
    SnmpV3MauCheck {
        user: secret.user.clone(),
        security_level: secret.security_level.clone(),
        auth_protocol: secret.auth_protocol.clone(),
        auth_key: secret.auth_key.clone(),
        priv_protocol: secret.priv_protocol.clone(),
        priv_key: secret.priv_key.clone(),
        entity_fallback: true,
        timeout_ms,
    }
}

/// Build the SNMP v2c CDP/LLDP neighbour check for a node (ADR-038).
///
/// The column list is the fixed LLDP-MIB / CISCO-CDP-MIB set — there is nothing for an operator to
/// configure, so it is not a collection template (and could not be: a `CollectionItem` declares a
/// TSDB series, and the numeric walker drops every string row). Sent on the wire rather than
/// assumed poller-side for the same reason `meta_columns` is: core stays the one place the OID set
/// is decided.
#[must_use]
pub fn build_snmp_neighbor_check(community: &str, timeout_ms: u32) -> SnmpNeighborCheck {
    SnmpNeighborCheck {
        community: community.to_owned(),
        columns: neighbor_columns(),
        timeout_ms,
    }
}

/// Build the SNMP v3 (USM) neighbour check — the v3 analogue of [`build_snmp_neighbor_check`].
#[must_use]
pub fn build_snmp_v3_neighbor_check(secret: &SnmpV3Secret, timeout_ms: u32) -> SnmpV3NeighborCheck {
    SnmpV3NeighborCheck {
        user: secret.user.clone(),
        security_level: secret.security_level.clone(),
        auth_protocol: secret.auth_protocol.clone(),
        auth_key: secret.auth_key.clone(),
        priv_protocol: secret.priv_protocol.clone(),
        priv_key: secret.priv_key.clone(),
        columns: neighbor_columns(),
        timeout_ms,
    }
}

/// Build the SNMP v2c interface-address check for a node (ADR-043).
///
/// The column list is the fixed RFC 1213 / RFC 4293 set — like the neighbour columns there is
/// nothing here for an operator to configure, and for the same reason it could not be a collection
/// template: a `CollectionItem` declares a TSDB series, and an interface address must never become
/// one. Sent on the wire rather than assumed poller-side so core stays the one place the OID set is
/// decided.
#[must_use]
pub fn build_snmp_l3_check(community: &str, timeout_ms: u32) -> SnmpL3Check {
    SnmpL3Check {
        community: community.to_owned(),
        columns: l3_columns(),
        timeout_ms,
    }
}

/// Build the SNMP v3 (USM) interface-address check — the v3 analogue of [`build_snmp_l3_check`].
#[must_use]
pub fn build_snmp_v3_l3_check(secret: &SnmpV3Secret, timeout_ms: u32) -> SnmpV3L3Check {
    SnmpV3L3Check {
        user: secret.user.clone(),
        security_level: secret.security_level.clone(),
        auth_protocol: secret.auth_protocol.clone(),
        auth_key: secret.auth_key.clone(),
        priv_protocol: secret.priv_protocol.clone(),
        priv_key: secret.priv_key.clone(),
        columns: l3_columns(),
        timeout_ms,
    }
}

/// Build the SNMP v2c ARP / IPv6-neighbour check for a node (ADR-043 Increment 3).
///
/// Unlike its two siblings this carries a row budget on the wire. The other walks read tables whose
/// size is a property of the device; this one reads a table whose size is a property of the network,
/// and a fleet-wide bound is core's decision to make rather than a constant compiled into whichever
/// poller build happens to be running.
#[must_use]
pub fn build_snmp_arp_check(community: &str, timeout_ms: u32) -> SnmpArpCheck {
    SnmpArpCheck {
        community: community.to_owned(),
        columns: arp_columns(),
        max_rows: arp_max_rows(),
        timeout_ms,
    }
}

/// Build the SNMP v3 (USM) ARP check — the v3 analogue of [`build_snmp_arp_check`].
#[must_use]
pub fn build_snmp_v3_arp_check(secret: &SnmpV3Secret, timeout_ms: u32) -> SnmpV3ArpCheck {
    SnmpV3ArpCheck {
        user: secret.user.clone(),
        security_level: secret.security_level.clone(),
        auth_protocol: secret.auth_protocol.clone(),
        auth_key: secret.auth_key.clone(),
        priv_protocol: secret.priv_protocol.clone(),
        priv_key: secret.priv_key.clone(),
        columns: arp_columns(),
        max_rows: arp_max_rows(),
        timeout_ms,
    }
}

/// Build the SNMP v2c routing-adjacency check (ADR-043 Increment 4).
///
/// `targets` is the node's slice of the fleet's host-route addresses, decided by [`RoutingPlan`].
/// The probe OIDs are built **here** rather than poller-side, so `inetCidrRouteTable`'s index
/// grammar stays a fact core owns — the same reason every other check is sent its column OIDs.
#[must_use]
pub fn build_snmp_routing_check(
    community: &str,
    targets: &[std::net::IpAddr],
    timeout_ms: u32,
) -> SnmpRoutingCheck {
    SnmpRoutingCheck {
        community: community.to_owned(),
        columns: routing_columns(),
        route_probes: route_probes(targets),
        timeout_ms,
    }
}

/// Build the SNMP v3 (USM) routing-adjacency check — the v3 analogue of
/// [`build_snmp_routing_check`].
#[must_use]
pub fn build_snmp_v3_routing_check(
    secret: &SnmpV3Secret,
    targets: &[std::net::IpAddr],
    timeout_ms: u32,
) -> SnmpV3RoutingCheck {
    SnmpV3RoutingCheck {
        user: secret.user.clone(),
        security_level: secret.security_level.clone(),
        auth_protocol: secret.auth_protocol.clone(),
        auth_key: secret.auth_key.clone(),
        priv_protocol: secret.priv_protocol.clone(),
        priv_key: secret.priv_key.clone(),
        columns: routing_columns(),
        route_probes: route_probes(targets),
        timeout_ms,
    }
}

fn routing_columns() -> Vec<SnmpRoutingColumn> {
    builtin_routing_columns()
        .into_iter()
        .map(|(field, oid)| SnmpRoutingColumn {
            field,
            oid: oid.to_owned(),
        })
        .collect()
}

/// One probe per (column, destination): the route type says whether the destination is on a local
/// interface and the ifIndex says which one, so both are needed for every target.
fn route_probes(targets: &[std::net::IpAddr]) -> Vec<SnmpRouteProbe> {
    let columns = route_probe_columns();
    let mut probes = Vec::with_capacity(targets.len() * columns.len());
    for target in targets {
        for (field, base) in &columns {
            probes.push(SnmpRouteProbe {
                field: *field,
                oid: route_probe_oid(base, *target),
                target: *target,
            });
        }
    }
    probes
}

fn arp_columns() -> Vec<SnmpArpColumn> {
    builtin_arp_columns()
        .into_iter()
        .map(|(field, oid)| SnmpArpColumn {
            field,
            oid: oid.to_owned(),
        })
        .collect()
}

/// The row budget sent on the wire, derived from the one constant that declares it.
fn arp_max_rows() -> u32 {
    u32::try_from(yagra_common::MAX_ARP_WALK_ROWS).unwrap_or(u32::MAX)
}

fn l3_columns() -> Vec<SnmpL3Column> {
    builtin_l3_columns()
        .into_iter()
        .map(|(field, oid)| SnmpL3Column {
            field,
            oid: oid.to_owned(),
        })
        .collect()
}

fn neighbor_columns() -> Vec<SnmpNeighborColumn> {
    builtin_neighbor_columns()
        .into_iter()
        .map(|(field, oid)| SnmpNeighborColumn {
            field,
            oid: oid.to_owned(),
        })
        .collect()
}

/// Whether this deployment collects connectivity data, and how often — resolved once per sweep and
/// passed into [`assemble_node_jobs`] so that function stays pure (no store, no clock).
///
/// One struct for both walks (L2 adjacency, ADR-038; L3 interface addresses, ADR-043) so the sweep
/// resolves settings once. Increment 3's ARP cadence and Increment 4's routing cadence become
/// fields here rather than another settings query in the scheduling loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdjacencyPolicy {
    /// Whether CDP/LLDP neighbour jobs are issued.
    pub neighbors_enabled: bool,
    /// Neighbour cadence in seconds. Deliberately slower than the node's metric interval; the
    /// poller's local scheduler honours it per spec, and the legacy publish path gates on it
    /// separately.
    pub neighbors_interval_secs: u32,
    /// Whether interface-address jobs are issued.
    pub l3_enabled: bool,
    /// Interface-address cadence in seconds. Same reasoning as the neighbour cadence: addressing
    /// changes on the order of months.
    pub l3_interval_secs: u32,
    /// Whether ARP / IPv6-neighbour jobs are issued. Off unless an operator turned it on — the one
    /// walk here that costs the device measurable work.
    pub arp_enabled: bool,
    /// ARP cadence in seconds. Slower again than the two above: what it discovers is "which hosts
    /// exist on my segments", which is an inventory question, not a state one.
    pub arp_interval_secs: u32,
    /// Whether routing-adjacency jobs are issued (ADR-043 Increment 4).
    pub routing_enabled: bool,
    /// Routing-adjacency cadence in seconds.
    pub routing_interval_secs: u32,
    /// Whether media-type jobs are issued (ADR-063 Inc.2).
    pub media_enabled: bool,
    /// Media cadence in seconds. Same reasoning as the neighbour cadence: a port's medium changes
    /// when someone swaps a module.
    pub media_interval_secs: u32,
    /// Which host addresses each node is asked to probe for, resolved once per sweep alongside the
    /// settings. Shared rather than cloned per node — it is the same fleet-wide fact for all of
    /// them, and nearly every node's entry is empty.
    pub routing_plan: Arc<RoutingPlan>,
}

impl From<AdjacencySettings> for AdjacencyPolicy {
    /// Settings alone, with an **empty** probe plan.
    ///
    /// The plan needs a database read, so a policy built from settings only issues the two walked
    /// columns and no route probes. Every caller that can afford the read uses
    /// [`PollDispatcher::adjacency_policy`] instead, which fills it in.
    fn from(s: AdjacencySettings) -> Self {
        Self {
            neighbors_enabled: s.neighbors_enabled,
            neighbors_interval_secs: s.neighbors_interval_secs,
            l3_enabled: s.l3_enabled,
            l3_interval_secs: s.l3_interval_secs,
            arp_enabled: s.arp_enabled,
            arp_interval_secs: s.arp_interval_secs,
            routing_enabled: s.routing_enabled,
            routing_interval_secs: s.routing_interval_secs,
            media_enabled: s.media_enabled,
            media_interval_secs: s.media_interval_secs,
            routing_plan: Arc::new(RoutingPlan::default()),
        }
    }
}

impl Default for AdjacencyPolicy {
    fn default() -> Self {
        AdjacencySettings::default().into()
    }
}

/// How long a [`RoutingPlan`] is reused before it is rebuilt.
///
/// The plan is derived from the fleet's host-route addressing, which changes when an interface is
/// re-addressed — on the order of weeks. Rebuilding it per sweep would mean a `jsonb_array_elements`
/// scan of `node_l3` every thirty seconds to find tens of rows; five minutes matches the derivation
/// task's own cadence, so the probe list and the graph it feeds move together.
const ROUTING_PLAN_TTL: Duration = Duration::from_secs(300);

/// The cached plan and when it was built.
struct CachedRoutingPlan {
    plan: Arc<RoutingPlan>,
    built_at: Option<std::time::Instant>,
}

/// A node's resolved SNMP authentication: a v2c community string or a v3 USM document.
pub enum SnmpAuth {
    V2c(String),
    V3(secrets::SnmpV3Secret),
}

/// A node bound to a single-purpose monitor, which replaces the ordinary ICMP/SNMP jobs entirely.
///
/// This is [`NodeKind`] carrying the config the job builder needs. The precedence itself lives in
/// [`NodeKind::resolve`] and nowhere else — it used to be expressed twice here (the load order of
/// [`PollDispatcher::build_scheduled_jobs_hinted`] and again in `assemble_node_jobs`), and a third
/// time as the Meraki short-circuit below, with nothing checking that the three agreed.
// Not `Copy`: the resolved credential is owned (`String`s), and a credential that copies
// implicitly is one that is easy to leave lying around in a second place.
#[derive(Debug, Clone)]
pub enum SpecialMonitor<'a> {
    /// The node has a `url_checks` row: one HTTP job. `auth` is the *resolved* credential, filled
    /// in by [`SpecialMonitor::with_http_auth`] after the store lookup — [`Self::resolve`] is sync
    /// and has no store, so it always produces `None` and the caller upgrades it.
    Url {
        cfg: &'a UrlCheckConfig,
        auth: Option<HttpAuth>,
    },
    /// The node has a `dns_checks` row: one DNS job.
    Dns(&'a DnsCheckConfig),
}

impl<'a> SpecialMonitor<'a> {
    /// The single-purpose monitor a node's rows resolve to, if any.
    ///
    /// Meraki is passed as `false` because **both callers have already excluded Meraki nodes** —
    /// the sweep by its preloaded id set, the on-demand path by its short-circuit — which is what
    /// lets this skip a `meraki_devices` round trip per node per round. It is an invariant of the
    /// call sites, not an assumption about the data.
    #[must_use]
    pub fn resolve(
        url: Option<&'a UrlCheckConfig>,
        dns: Option<&'a DnsCheckConfig>,
    ) -> Option<Self> {
        let rows = NodeRows {
            meraki: false,
            url: url.is_some(),
            dns: dns.is_some(),
        };
        match NodeKind::resolve(rows) {
            NodeKind::Url => url.map(|cfg| Self::Url { cfg, auth: None }),
            NodeKind::Dns => dns.map(Self::Dns),
            NodeKind::Meraki | NodeKind::Device => None,
        }
    }

    /// Attach resolved HTTP credentials. A no-op for a DNS monitor, which has none.
    #[must_use]
    pub fn with_http_auth(self, auth: Option<HttpAuth>) -> Self {
        match self {
            Self::Url { cfg, .. } => Self::Url { cfg, auth },
            Self::Dns(cfg) => Self::Dns(cfg),
        }
    }
}

/// The sweep's once-per-round preloaded id sets, one per single-purpose monitor kind.
///
/// Each kind lives in its own 1:1 side table, so resolving a node's kind means a query against
/// every one of them — at 50k nodes that is 50k round trips *per table* per round for the 99.9% of
/// the fleet that are ordinary devices. The sweep loads the ids once and hands them down; a node
/// absent from the set is known not to be that kind, and its lookup is skipped.
///
/// ⚠️ `None` and an **empty set** mean different things and must not be conflated.
/// `None` = "no hint was preloaded" ⇒ query directly (the on-demand single-node path).
/// `Some(∅)` = "there are no monitors of this kind at all" ⇒ skip every lookup.
/// [`Default`] is both-`None`, which is exactly the on-demand behaviour.
///
/// This is one struct rather than one `Option<&HashSet<Uuid>>` parameter per kind because the
/// second kind already made the parameter list ambiguous at the call sites, and a third would make
/// it worse — see `extensibility.md` §3.
#[derive(Debug, Clone, Copy, Default)]
pub struct MonitorHints<'a> {
    /// Nodes carrying a `url_checks` row, or `None` for "not preloaded".
    pub url: Option<&'a HashSet<Uuid>>,
    /// Nodes carrying a `dns_checks` row, or `None` for "not preloaded".
    pub dns: Option<&'a HashSet<Uuid>>,
}

/// Whether a per-node side-table lookup is worth paying for, given the sweep's preloaded id set.
///
/// One function rather than the `is_none_or` idiom written at each call site: the whole failure
/// mode is silent (get it backwards and a monitor is never polled, or every node queries every
/// table again), and inline it is untestable — `build_scheduled_jobs_hinted` needs a live database.
#[must_use]
fn hint_admits(hint: Option<&HashSet<Uuid>>, node: Uuid) -> bool {
    hint.is_none_or(|ids| ids.contains(&node))
}

/// Assemble every poll job for one node from its already-resolved SNMP auth + collection set:
/// an ICMP liveness job always, plus the SNMP scalar/table (v2c) or scalar (v3) jobs the set
/// calls for. Each job is tagged with a short kind label for logging. Pure (no I/O) — the async
/// credential/collection resolution lives in [`PollDispatcher::build_node_jobs`] — so the
/// job-shape logic is unit-testable without a database or bus.
///
/// `probe_identity` is set on the SNMP job only while the node's maker is unknown, so once a node
/// is classified we stop re-fetching sysDescr every poll (same rule as the periodic scheduler).
///
/// The neighbour job (ADR-038) is the one job here that does **not** run at `interval_secs`: it
/// carries `neighbors.interval_secs` instead, which the poller's per-spec scheduler honours
/// directly and the legacy publish path gates on separately.
#[must_use]
pub fn assemble_node_jobs(
    node: &Node,
    auth: Option<&SnmpAuth>,
    items: &[CollectionItem],
    monitor: Option<SpecialMonitor<'_>>,
    interval_secs: u32,
    neighbors: &AdjacencyPolicy,
) -> Vec<(PollJob, &'static str)> {
    // A URL or DNS monitor is its own node kind: dispatch that one job and nothing else. ICMP is
    // *not* added — a URL target may be non-pingable (e.g. behind a CDN), and a name has no
    // address of its own — and SNMP doesn't apply to either.
    match monitor {
        Some(SpecialMonitor::Url { cfg, auth }) => {
            let job = build_http_job(
                node,
                build_http_check(cfg, auth),
                interval_secs,
                Uuid::new_v4(),
            );
            return vec![(job, "http")];
        }
        Some(SpecialMonitor::Dns(cfg)) => {
            let job = build_dns_job(node, build_dns_check(cfg), interval_secs, Uuid::new_v4());
            return vec![(job, "dns")];
        }
        None => {}
    }
    let mut jobs = Vec::new();
    let probe_identity = node.vendor.is_none();
    match auth {
        Some(SnmpAuth::V2c(community)) => {
            let (scalar, table) = build_snmp_checks(community, items, SNMP_TIMEOUT_MS);
            if let Some(check) = scalar {
                let mut job = build_snmp_job(node, check, interval_secs, Uuid::new_v4());
                job.probe_identity = probe_identity;
                jobs.push((job, "snmp"));
            }
            if let Some(check) = table {
                let job = build_snmp_table_job(node, check, interval_secs, Uuid::new_v4());
                jobs.push((job, "snmp_table"));
            }
            // Optical rides `interval_secs`, not a slow adjacency cadence: optical power drifts
            // continuously with temperature and age, and it shares a time axis with throughput in
            // the interface dock (ADR-062).
            if let Some(check) = build_snmp_optical_check(community, items, SNMP_TIMEOUT_MS) {
                let job = PollJob::snmp_optical(
                    Uuid::new_v4(),
                    node.id,
                    node.address,
                    check,
                    interval_secs,
                );
                jobs.push((job, "snmp_optical"));
            }
            if neighbors.neighbors_enabled {
                let job = PollJob::snmp_neighbors(
                    Uuid::new_v4(),
                    node.id,
                    node.address,
                    build_snmp_neighbor_check(community, SNMP_TIMEOUT_MS),
                    neighbors.neighbors_interval_secs,
                );
                jobs.push((job, "snmp_neighbors"));
            }
            // Media rides the slow cadence, unlike optical above: a port's medium changes when
            // someone swaps a module, not continuously (ADR-063 Inc.2).
            if neighbors.media_enabled {
                let job = PollJob::snmp_mau(
                    Uuid::new_v4(),
                    node.id,
                    node.address,
                    build_snmp_mau_check(community, SNMP_TIMEOUT_MS),
                    neighbors.media_interval_secs,
                );
                jobs.push((job, "snmp_mau"));
            }
            if neighbors.l3_enabled {
                let job = PollJob::snmp_l3(
                    Uuid::new_v4(),
                    node.id,
                    node.address,
                    build_snmp_l3_check(community, SNMP_TIMEOUT_MS),
                    neighbors.l3_interval_secs,
                );
                jobs.push((job, "snmp_l3"));
            }
            if neighbors.arp_enabled {
                let job = PollJob::snmp_arp(
                    Uuid::new_v4(),
                    node.id,
                    node.address,
                    build_snmp_arp_check(community, SNMP_TIMEOUT_MS),
                    neighbors.arp_interval_secs,
                );
                jobs.push((job, "snmp_arp"));
            }
            if neighbors.routing_enabled {
                let job = PollJob::snmp_routing(
                    Uuid::new_v4(),
                    node.id,
                    node.address,
                    build_snmp_routing_check(
                        community,
                        neighbors.routing_plan.targets_for(node.id),
                        SNMP_TIMEOUT_MS,
                    ),
                    neighbors.routing_interval_secs,
                );
                jobs.push((job, "snmp_routing"));
            }
        }
        Some(SnmpAuth::V3(secret)) => {
            if let Some(check) = build_snmp_v3_check(secret, items, SNMP_TIMEOUT_MS) {
                let mut job = build_snmp_v3_job(node, check, interval_secs, Uuid::new_v4());
                job.probe_identity = probe_identity;
                jobs.push((job, "snmp_v3"));
            }
            if let Some(check) = build_snmp_v3_table_check(secret, items, SNMP_TIMEOUT_MS) {
                let job = build_snmp_v3_table_job(node, check, interval_secs, Uuid::new_v4());
                jobs.push((job, "snmp_v3_table"));
            }
            // See the v2c arm: optical rides the metric interval, not the adjacency cadence.
            if let Some(check) = build_snmp_v3_optical_check(secret, items, SNMP_TIMEOUT_MS) {
                let job = PollJob::snmp_v3_optical(
                    Uuid::new_v4(),
                    node.id,
                    node.address,
                    check,
                    interval_secs,
                );
                jobs.push((job, "snmp_v3_optical"));
            }
            if neighbors.neighbors_enabled {
                let job = PollJob::snmp_v3_neighbors(
                    Uuid::new_v4(),
                    node.id,
                    node.address,
                    build_snmp_v3_neighbor_check(secret, SNMP_TIMEOUT_MS),
                    neighbors.neighbors_interval_secs,
                );
                jobs.push((job, "snmp_v3_neighbors"));
            }
            // See the v2c arm: media rides the slow cadence, not the metric interval.
            if neighbors.media_enabled {
                let job = PollJob::snmp_v3_mau(
                    Uuid::new_v4(),
                    node.id,
                    node.address,
                    build_snmp_v3_mau_check(secret, SNMP_TIMEOUT_MS),
                    neighbors.media_interval_secs,
                );
                jobs.push((job, "snmp_v3_mau"));
            }
            if neighbors.l3_enabled {
                let job = PollJob::snmp_v3_l3(
                    Uuid::new_v4(),
                    node.id,
                    node.address,
                    build_snmp_v3_l3_check(secret, SNMP_TIMEOUT_MS),
                    neighbors.l3_interval_secs,
                );
                jobs.push((job, "snmp_v3_l3"));
            }
            if neighbors.arp_enabled {
                let job = PollJob::snmp_v3_arp(
                    Uuid::new_v4(),
                    node.id,
                    node.address,
                    build_snmp_v3_arp_check(secret, SNMP_TIMEOUT_MS),
                    neighbors.arp_interval_secs,
                );
                jobs.push((job, "snmp_v3_arp"));
            }
            if neighbors.routing_enabled {
                let job = PollJob::snmp_v3_routing(
                    Uuid::new_v4(),
                    node.id,
                    node.address,
                    build_snmp_v3_routing_check(
                        secret,
                        neighbors.routing_plan.targets_for(node.id),
                        SNMP_TIMEOUT_MS,
                    ),
                    neighbors.routing_interval_secs,
                );
                jobs.push((job, "snmp_v3_routing"));
            }
        }
        None => {}
    }
    // ICMP liveness is always polled, regardless of SNMP configuration.
    let icmp = build_icmp_job(node, IcmpCheck::default(), interval_secs, Uuid::new_v4());
    jobs.push((icmp, "icmp"));
    jobs
}

/// Turns a single node into the poll jobs the bus needs and publishes them. Shared by the
/// periodic [`run_scheduler`](crate) loop (which jitters each publish to avoid a stampede) and
/// the on-demand "poll now" API action (which publishes immediately). Holds the bus plus the
/// seams needed to resolve a node's SNMP auth and collection set — never a direct poller call
/// (core⇄poller flows only through the bus, ADR-003).
pub struct PollDispatcher {
    bus: Arc<NatsBus>,
    creds: Arc<CredentialStore>,
    collection: Arc<CollectionRepo>,
    /// Per-node URL-monitor configs (a node with one is a URL monitor → HTTP job, no ICMP/SNMP).
    url_checks: Arc<UrlCheckRepo>,
    /// Per-node DNS-monitor configs (a node with one is a DNS monitor → DNS job, no ICMP/SNMP).
    dns_checks: Arc<DnsCheckRepo>,
    /// Per-node Meraki bindings (a node with one is polled by the org collector → no ICMP/SNMP).
    meraki_devices: Arc<crate::meraki::MerakiDeviceRepo>,
    /// Deployment-wide settings, for the on-demand path only. The periodic sweep resolves the
    /// neighbour policy once per round and passes it in explicitly (as it already does with the
    /// poll interval), so this is never read in the hot loop.
    settings: Arc<crate::repo::NodeRepo>,
    /// Interface addresses, read only to rebuild the route-probe plan (ADR-043 Increment 4).
    l3: Arc<crate::l3::L3Repo>,
    /// The route-probe plan, rebuilt at most every [`ROUTING_PLAN_TTL`]. Behind a lock rather than
    /// recomputed per sweep because it is a `jsonb_array_elements` scan of `node_l3` that returns
    /// tens of rows, and the addressing it reads changes on the order of weeks.
    routing_plan: tokio::sync::RwLock<CachedRoutingPlan>,
    /// v2c community fallback for nodes without a bound credential.
    env_community: Option<String>,
    /// Fallback poll interval (seconds) stamped on on-demand "poll now" jobs. The periodic
    /// scheduler resolves the effective interval per node (profile override → DB default) and
    /// passes it explicitly; this is only the manual-poll default (the poller ignores the value).
    interval_secs: u32,
}

/// The seams a [`PollDispatcher`] needs, as one named value.
///
/// There is one repository per [`NodeKind`] that has a side table, so the argument list grew with
/// every kind added — eight positional `Arc`s, three of them the same shape, behind an
/// `#[allow(clippy::too_many_arguments)]`. Positional arguments of the same type are exactly where
/// two get swapped without a type error, so adding a kind should add a *field*, not a position.
pub struct PollDispatcherSeams {
    pub bus: Arc<NatsBus>,
    pub creds: Arc<CredentialStore>,
    pub collection: Arc<CollectionRepo>,
    pub url_checks: Arc<UrlCheckRepo>,
    pub dns_checks: Arc<DnsCheckRepo>,
    pub meraki_devices: Arc<crate::meraki::MerakiDeviceRepo>,
    /// Deployment-wide settings — see the field of the same name on [`PollDispatcher`].
    pub settings: Arc<crate::repo::NodeRepo>,
    /// Interface addresses — see the field of the same name on [`PollDispatcher`].
    pub l3: Arc<crate::l3::L3Repo>,
    /// v2c community fallback for nodes without a bound credential.
    pub env_community: Option<String>,
    /// Fallback poll interval (seconds) for on-demand "poll now" jobs — see the field of the
    /// same name on [`PollDispatcher`].
    pub interval_secs: u32,
}

impl PollDispatcher {
    #[must_use]
    pub fn new(seams: PollDispatcherSeams) -> Self {
        let PollDispatcherSeams {
            bus,
            creds,
            collection,
            url_checks,
            dns_checks,
            meraki_devices,
            settings,
            l3,
            env_community,
            interval_secs,
        } = seams;
        Self {
            bus,
            creds,
            collection,
            url_checks,
            dns_checks,
            meraki_devices,
            settings,
            l3,
            routing_plan: tokio::sync::RwLock::new(CachedRoutingPlan {
                plan: Arc::new(RoutingPlan::default()),
                built_at: None,
            }),
            env_community,
            interval_secs,
        }
    }

    /// The deployment's adjacency policy, degrading to the compiled default on a read failure —
    /// resolved once per sweep, and once per action on the on-demand path.
    pub async fn adjacency_policy(&self) -> AdjacencyPolicy {
        let mut policy: AdjacencyPolicy = self.settings.get_adjacency_settings().await.into();
        if policy.routing_enabled {
            policy.routing_plan = self.routing_plan().await;
        }
        policy
    }

    /// The route-probe plan, rebuilt when the cached one has expired (ADR-043 Increment 4).
    ///
    /// A failed read keeps the previous plan rather than falling back to an empty one: an empty
    /// plan issues no probes at all, so a transient database error would silently stop collecting
    /// point-to-point links — and nothing downstream could tell that apart from "there are none".
    /// The TTL is not advanced on failure, so the next sweep retries.
    async fn routing_plan(&self) -> Arc<RoutingPlan> {
        {
            let cached = self.routing_plan.read().await;
            if cached
                .built_at
                .is_some_and(|at| at.elapsed() < ROUTING_PLAN_TTL)
            {
                return cached.plan.clone();
            }
        }
        let mut cached = self.routing_plan.write().await;
        // Re-checked under the write lock: two sweeps racing here would otherwise both rebuild.
        if cached
            .built_at
            .is_some_and(|at| at.elapsed() < ROUTING_PLAN_TTL)
        {
            return cached.plan.clone();
        }
        match self.l3.host_addresses().await {
            Ok(hosts) => {
                let plan = Arc::new(RoutingPlan::build(&hosts));
                metrics::gauge!("yagra_route_probe_nodes")
                    .set(u32::try_from(plan.prober_count()).unwrap_or(u32::MAX));
                cached.plan = plan;
                cached.built_at = Some(std::time::Instant::now());
            }
            Err(e) => {
                tracing::warn!(error = %e, "route-probe plan refresh failed; keeping the previous plan");
            }
        }
        cached.plan.clone()
    }

    /// Every node that is a DNS monitor, for the periodic scheduler to preload once per sweep.
    ///
    /// Without this the sweep would need a `dns_checks` lookup per node per round — the exact
    /// per-node round trip the Meraki path already avoids. DNS monitors are few, so the set is tiny.
    ///
    /// ⚠️ On failure this yields an **empty** set, which [`MonitorHints`] reads as "there are no DNS
    /// monitors", not as "no hint" — so a failed preload costs one sweep's DNS jobs rather than
    /// silently reverting to 50k per-node queries. The warning says so.
    pub async fn dns_node_ids(&self) -> HashSet<Uuid> {
        self.dns_checks.node_ids().await.unwrap_or_else(|e| {
            tracing::warn!(error = %e, "dns-check id preload failed; DNS monitors are skipped for this sweep");
            HashSet::new()
        })
    }

    /// Every node that is a URL monitor, for the periodic scheduler to preload once per sweep.
    /// The [`Self::dns_node_ids`] twin — see it for the empty-set-on-failure semantics.
    pub async fn url_node_ids(&self) -> HashSet<Uuid> {
        self.url_checks.node_ids().await.unwrap_or_else(|e| {
            tracing::warn!(error = %e, "url-check id preload failed; URL monitors are skipped for this sweep");
            HashSet::new()
        })
    }

    /// Resolve a node's poll jobs. A URL monitor (it has a `url_checks` row) gets a single HTTP
    /// job; otherwise SNMP auth (decrypted in core, never the poller — ADR-018/020) + collection
    /// set + the always-on ICMP. The caller resolves the effective interval and decides whether to
    /// jitter (periodic) or publish at once (poll now).
    pub async fn build_node_jobs(
        &self,
        node: &Node,
        interval_secs: u32,
    ) -> Vec<(PollJob, &'static str)> {
        // Meraki short-circuit: a node with a Meraki binding is polled by the org collector, so it
        // emits no per-node ICMP/SNMP/HTTP job. This guards the on-demand "poll now" path; the
        // periodic scheduler already excludes these nodes (it preloads their ids) and calls
        // `build_scheduled_jobs`, which skips this per-node lookup.
        let bound = match self.meraki_devices.get(node.id.as_uuid()).await {
            Ok(bound) => bound.is_some(),
            Err(e) => {
                tracing::warn!(node = %node.id, error = %e, "meraki-device load failed; treating as non-Meraki node");
                false
            }
        };
        // Asked as "is this kind scheduled per node" rather than "is it Meraki", so a future kind
        // that is also collected elsewhere does not have to remember to add itself here.
        let kind = NodeKind::resolve(NodeRows {
            meraki: bound,
            ..NodeRows::default()
        });
        if !kind.is_polled_per_node() {
            return Vec::new();
        }
        // One node, one settings read — this path is an operator action, not the sweep.
        let neighbors = self.adjacency_policy().await;
        // Default = every hint `None` = look every side table up directly. Correct here: one node,
        // and preloading a fleet-wide id set to serve it would cost far more than it saves.
        self.build_scheduled_jobs_hinted(node, interval_secs, MonitorHints::default(), &neighbors)
            .await
    }

    /// A node's poll jobs **without** the Meraki-binding lookup — used by the periodic scheduler,
    /// which already excludes Meraki nodes by preloading their ids, so re-querying `meraki_devices`
    /// per node every sweep is wasted work (one JOIN per node per round at scale). The on-demand
    /// "poll now" path keeps the guard via [`Self::build_node_jobs`]. A URL monitor gets a single
    /// HTTP job; otherwise SNMP auth (decrypted in core) + collection set + the always-on ICMP.
    ///
    /// `neighbors` is resolved by the caller — once per sweep, or once per action on the on-demand
    /// path — so this never reads settings per node.
    ///
    /// `hints` is what keeps this from adding a per-node round trip *per monitor kind* to every
    /// sweep: when a kind's set is present, only nodes in it are looked up, so the 99.9% of the
    /// fleet that are ordinary devices cost nothing. [`MonitorHints::default`] (the on-demand
    /// "poll now" path, a single node) falls back to querying directly.
    pub async fn build_scheduled_jobs_hinted(
        &self,
        node: &Node,
        interval_secs: u32,
        hints: MonitorHints<'_>,
        neighbors: &AdjacencyPolicy,
    ) -> Vec<(PollJob, &'static str)> {
        let node_uuid = node.id.as_uuid();
        // Both side-table lookups are hint-gated. The URL one was not, for a long time: it ran
        // unconditionally for every node on every sweep while the DNS one beside it was already
        // short-circuited, so a 50k fleet paid 50k `url_checks` queries a round to find the handful
        // of URL monitors. Note the gate has to wrap the lookup itself — the ordering here is the
        // fix, and passing a hint without moving the query would have changed nothing.
        let url = if hint_admits(hints.url, node_uuid) {
            self.url_checks.get(node_uuid).await.unwrap_or_else(|e| {
                tracing::warn!(node = %node.id, error = %e, "url-check load failed; treating as non-URL node");
                None
            })
        } else {
            None
        };
        // Skipped entirely once URL has already won — `resolve` would discard it.
        let dns = if url.is_none() && hint_admits(hints.dns, node_uuid) {
            self.dns_checks.get(node_uuid).await.unwrap_or_else(|e| {
                tracing::warn!(node = %node.id, error = %e, "dns-check load failed; treating as non-DNS node");
                None
            })
        } else {
            None
        };
        // Single-purpose monitors replace ICMP/SNMP entirely, so neither the credential nor the
        // collection lookup is worth paying for them.
        if let Some(monitor) = SpecialMonitor::resolve(url.as_ref(), dns.as_ref()) {
            // A URL monitor may be bound to a credential; that lookup is the same `creds.open` +
            // parse + static-reason-warn path SNMP takes, and costs one round trip per bound URL
            // monitor per sweep. Unbound monitors and DNS monitors pay nothing.
            let monitor = if let SpecialMonitor::Url { cfg, .. } = &monitor {
                let auth = resolve_http_auth(&self.creds, node, cfg.credential).await;
                monitor.with_http_auth(auth)
            } else {
                monitor
            };
            return assemble_node_jobs(node, None, &[], Some(monitor), interval_secs, neighbors);
        }
        let auth = resolve_snmp_auth(&self.creds, node, self.env_community.as_deref()).await;
        // Only resolve the collection set when SNMP is configured (ICMP needs none).
        let items = if auth.is_some() {
            resolve_node_collection(&self.collection, node).await
        } else {
            Vec::new()
        };
        assemble_node_jobs(node, auth.as_ref(), &items, None, interval_secs, neighbors)
    }

    /// The node's poll jobs as reusable working-set [`JobSpec`]s (ADR-020) — the distributed-pool
    /// analogue of [`Self::build_node_jobs`], built via [`Self::build_scheduled_jobs_hinted`]
    /// (Meraki nodes are already excluded on the scheduler path, so the per-node Meraki lookup is
    /// skipped). The per-dispatch `job_id` is dropped here; the poller stamps a fresh one each time
    /// it schedules the spec locally.
    ///
    /// `hints` carries the sweep's preloaded per-kind monitor id sets — see
    /// [`Self::build_scheduled_jobs_hinted`] for why they matter at fleet scale.
    pub async fn build_node_specs(
        &self,
        node: &Node,
        interval_secs: u32,
        hints: MonitorHints<'_>,
        neighbors: &AdjacencyPolicy,
    ) -> Vec<JobSpec> {
        self.build_scheduled_jobs_hinted(node, interval_secs, hints, neighbors)
            .await
            .iter()
            .map(|(job, _kind)| JobSpec::from_job(job))
            .collect()
    }

    /// Publish one already-built job to `pool`'s subject, bumping the published-jobs counter (used
    /// by the periodic scheduler after its per-job jitter delay). Routing per pool keeps a job local
    /// to the pollers serving that pool (ADR-009). Returns whether the publish succeeded.
    pub async fn publish_job(&self, job: PollJob, kind: &str, node: NodeId, pool: &str) -> bool {
        publish(&self.bus, pool, job, kind, node).await
    }

    /// Build and immediately publish every poll job for a node (no jitter) — the operator's
    /// "poll now" action. Returns how many jobs were published. Stamps the dispatcher's default
    /// interval (the poller ignores the value; cadence is owned by the periodic scheduler).
    ///
    /// `pool` is the node's **effective** pool, resolved by the caller (which has the folder tree
    /// for inheritance) — the dispatcher deliberately doesn't own a group repo just for this.
    pub async fn poll_now(&self, node: &Node, pool: &str) -> usize {
        let jobs = self.build_node_jobs(node, self.interval_secs).await;
        let mut published = 0u64;
        for (job, kind) in jobs {
            if publish(&self.bus, pool, job, kind, node.id).await {
                published += 1;
            }
        }
        metrics::counter!("yagra_manual_poll_jobs_total").increment(published);
        usize::try_from(published).unwrap_or(usize::MAX)
    }
}

/// Resolve a node's effective collection set, defaulting to the built-in catalog when nothing is
/// configured (or the lookup fails) so polling always has a sensible default.
async fn resolve_node_collection(collection: &CollectionRepo, node: &Node) -> Vec<CollectionItem> {
    match collection
        .list_items_for_node(node.id.as_uuid(), node.profile.map(|p| p.0))
        .await
    {
        Ok(scoped) => {
            let resolved = yagra_common::resolve_collection_set(&scoped);
            if resolved.is_empty() {
                yagra_common::builtin_catalog()
            } else {
                resolved
            }
        }
        Err(e) => {
            tracing::warn!(node = %node.id, error = %e, "collection load failed; using built-in catalog");
            yagra_common::builtin_catalog()
        }
    }
}

/// Resolve a node's SNMP auth from its bound credential (decrypted in core, never the poller —
/// ADR-018/020). The credential `kind` picks the protocol: `snmp_v3` secrets are USM JSON docs;
/// anything else is treated as a v2c community (back-compat with credentials created before kinds
/// were meaningful). The env community is a v2c fallback for nodes without a bound credential.
/// Resolve a URL monitor's bound credential into the [`HttpAuth`] the poll job carries.
///
/// Mirrors [`resolve_snmp_auth`]: open the sealed secret, parse it, and on any failure warn with a
/// **static** reason and fall back to an unauthenticated probe. The alternative — skipping the poll
/// — would read as an outage; an unauthenticated probe against an endpoint that needs credentials
/// reads as a 401, which is closer to the truth and visible in the status code metric.
async fn resolve_http_auth(
    creds: &CredentialStore,
    node: &Node,
    credential: Option<yagra_common::CredentialId>,
) -> Option<HttpAuth> {
    let cred = credential?;
    match creds.open(cred.as_uuid()).await {
        Ok(Some((kind, bytes))) => match secrets::parse_http_auth(&kind, &bytes) {
            Ok(auth) => Some(auth),
            // Static reason only — never echo any part of the secret.
            Err(reason) => {
                tracing::warn!(node = %node.id, %reason, "invalid http auth credential");
                None
            }
        },
        Ok(None) => {
            tracing::warn!(node = %node.id, "bound credential not found");
            None
        }
        Err(e) => {
            tracing::warn!(node = %node.id, error = %e, "credential decrypt failed");
            None
        }
    }
}

async fn resolve_snmp_auth(
    creds: &CredentialStore,
    node: &Node,
    env_community: Option<&str>,
) -> Option<SnmpAuth> {
    if let Some(cred) = node.credential {
        match creds.open(cred.as_uuid()).await {
            Ok(Some((kind, bytes))) => {
                if kind == secrets::KIND_SNMP_V3 {
                    match secrets::SnmpV3Secret::parse(&bytes) {
                        Ok(secret) => return Some(SnmpAuth::V3(secret)),
                        // Static reason only — never echo any part of the secret.
                        Err(reason) => {
                            tracing::warn!(node = %node.id, %reason, "invalid snmp_v3 credential");
                        }
                    }
                } else if let Ok(community) = String::from_utf8(bytes) {
                    return Some(SnmpAuth::V2c(community));
                }
            }
            Ok(None) => tracing::warn!(node = %node.id, "bound credential not found"),
            Err(e) => tracing::warn!(node = %node.id, error = %e, "credential decrypt failed"),
        }
    }
    env_community.map(|c| SnmpAuth::V2c(c.to_owned()))
}

/// Publish a job to `pool`'s subject and bump the published-jobs counter, logging failures.
/// Returns success. Uses [`SyncBus::publish_job_for_pool`] so legacy per-job dispatch, "poll now",
/// and Meraki all stay local to the target pool (ADR-009) while an old wildcard poller still
/// absorbs them (N/N-1 compatible).
async fn publish(bus: &NatsBus, pool: &str, mut job: PollJob, kind: &str, node: NodeId) -> bool {
    // Seed the job with this dispatch's trace context so the poller's poll span (and core's later
    // result-ingest span) join one distributed trace (yagra-telemetry). A short-lived dispatch span
    // is the trace root for legacy/poll-now jobs; injection is a no-op when tracing export is off,
    // so `trace_context` stays empty and off the wire (N/N-1 safe, ADR-017). Enter/inject/drop the
    // guard before the await — never hold a span guard across `.await`.
    {
        let dispatch = tracing::info_span!("dispatch.poll_job", %kind, node = %node, pool);
        let _enter = dispatch.enter();
        job.trace_context = yagra_telemetry::current_trace_context();
    }
    match bus.publish_job_for_pool(pool, job).await {
        Ok(()) => {
            metrics::counter!("yagra_jobs_published_total").increment(1);
            true
        }
        Err(e) => {
            tracing::warn!(error = %e, %kind, node = %node, pool, "failed to publish job");
            false
        }
    }
}

/// Live self-monitoring counters for the poll loop, shared between the scheduler (producer) and
/// the result consumer. Lock-free atomics — updated on the hot path, read by the poller-health
/// endpoint. Per-poller breakdown needs poller identity on the bus and is a later addition.
#[derive(Default)]
pub struct SchedulerStats {
    last_sweep_ms: std::sync::atomic::AtomicI64,
    jobs_last_round: std::sync::atomic::AtomicU64,
    results_total: std::sync::atomic::AtomicU64,
    // Distributed poller pool (ADR-009/020). Counters bumped by the coordinator as it distributes
    // working sets; the two `pools_*` are gauge-style (overwritten each sweep by the scheduler).
    snapshots_published_total: std::sync::atomic::AtomicU64,
    deltas_published_total: std::sync::atomic::AtomicU64,
    // Redis assignment-mirror rewrites the coordinator actually issued (S18): in steady state this
    // stays flat sweep-over-sweep because an unchanged working set skips the O(fleet) DEL+HSET.
    assignment_mirror_writes_total: std::sync::atomic::AtomicU64,
    pools_working_set: std::sync::atomic::AtomicU64,
    pools_legacy: std::sync::atomic::AtomicU64,
}

/// A point-in-time view of [`SchedulerStats`] for the API.
#[derive(serde::Serialize, utoipa::ToSchema)]
pub struct SchedulerStatsSnapshot {
    /// When the last poll round was dispatched (Unix ms), or `None` if none yet.
    pub last_sweep_unix_ms: Option<i64>,
    /// Jobs published in the most recent round (legacy per-job dispatch only).
    pub jobs_last_round: u64,
    /// Total poll results consumed since start.
    pub results_total: u64,
    /// Working-set snapshots core has published to pollers since start (ADR-020).
    pub snapshots_published_total: u64,
    /// Working-set deltas core has published to pollers since start (ADR-020).
    pub deltas_published_total: u64,
    /// Redis assignment-mirror rewrites issued since start (S18). Flat across steady-state sweeps —
    /// an unchanged working set skips the rewrite — so growth tracks real assignment churn.
    pub assignment_mirror_writes_total: u64,
    /// Pools served in working-set mode in the most recent sweep (a live poller owns them).
    pub pools_working_set: u64,
    /// Pools served in legacy per-job mode in the most recent sweep (no live poller).
    pub pools_legacy: u64,
}

impl SchedulerStats {
    /// Record a completed dispatch round (`jobs` published, stamped now).
    pub fn record_sweep(&self, jobs: u64) {
        use std::sync::atomic::Ordering;
        let ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
            .unwrap_or(0);
        self.last_sweep_ms.store(ms, Ordering::Relaxed);
        self.jobs_last_round.store(jobs, Ordering::Relaxed);
    }

    /// Count one consumed poll result.
    pub fn record_result(&self) {
        self.results_total
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    /// Count one working-set snapshot published (a snapshot, not each of its chunks).
    pub fn record_snapshot(&self) {
        self.snapshots_published_total
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    /// Count one working-set delta published.
    pub fn record_delta(&self) {
        self.deltas_published_total
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    /// Count one Redis assignment-mirror rewrite (S18); skipped sweeps don't call this.
    pub fn record_assignment_write(&self) {
        self.assignment_mirror_writes_total
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    /// Record how many pools ran in each mode this sweep (gauge-style: overwrites).
    pub fn set_pool_modes(&self, working_set: u64, legacy: u64) {
        use std::sync::atomic::Ordering;
        self.pools_working_set.store(working_set, Ordering::Relaxed);
        self.pools_legacy.store(legacy, Ordering::Relaxed);
    }

    /// Snapshot for the API.
    #[must_use]
    pub fn snapshot(&self) -> SchedulerStatsSnapshot {
        use std::sync::atomic::Ordering;
        let ms = self.last_sweep_ms.load(Ordering::Relaxed);
        SchedulerStatsSnapshot {
            last_sweep_unix_ms: (ms > 0).then_some(ms),
            jobs_last_round: self.jobs_last_round.load(Ordering::Relaxed),
            results_total: self.results_total.load(Ordering::Relaxed),
            snapshots_published_total: self.snapshots_published_total.load(Ordering::Relaxed),
            deltas_published_total: self.deltas_published_total.load(Ordering::Relaxed),
            assignment_mirror_writes_total: self
                .assignment_mirror_writes_total
                .load(Ordering::Relaxed),
            pools_working_set: self.pools_working_set.load(Ordering::Relaxed),
            pools_legacy: self.pools_legacy.load(Ordering::Relaxed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};
    use yagra_bus::CheckSpec;
    use yagra_common::NodeId;

    #[test]
    fn interval_resolves_profile_override_then_default() {
        let p1 = ProfileId::new();
        let p2 = ProfileId::new();
        let mut overrides = HashMap::new();
        overrides.insert(p1.0, 15u32);
        // Profile with an override → its value.
        assert_eq!(resolve_interval(Some(p1), &overrides, 30), 15);
        // Profile without an override → the global default.
        assert_eq!(resolve_interval(Some(p2), &overrides, 30), 30);
        // No profile at all → the global default.
        assert_eq!(resolve_interval(None, &overrides, 30), 30);
    }

    #[test]
    fn pool_mode_is_working_set_xor_legacy() {
        use std::collections::HashSet;
        let live: HashSet<String> = ["tokyo".to_string()].into_iter().collect();
        // A pool with a live poller runs working-set; one without runs legacy.
        assert!(pool_uses_working_set("tokyo", &live));
        assert!(!pool_uses_working_set("osaka", &live));
        // For every pool the two modes are exclusive (working-set == !legacy) — no double-polling.
        for pool in ["tokyo", "osaka", "default"] {
            let working_set = pool_uses_working_set(pool, &live);
            let legacy = !pool_uses_working_set(pool, &live);
            assert_ne!(working_set, legacy, "a pool is working-set XOR legacy");
        }
    }

    #[test]
    fn due_when_never_dispatched_or_interval_elapsed() {
        let interval = Duration::from_secs(30);
        // Never dispatched → due immediately.
        assert!(due(None, interval));
        // Less than the interval has passed → not due.
        assert!(!due(Some(Duration::from_secs(29)), interval));
        // Exactly the interval → due (>=).
        assert!(due(Some(Duration::from_secs(30)), interval));
        // Well past the interval → due.
        assert!(due(Some(Duration::from_secs(120)), interval));
    }

    #[test]
    fn icmp_job_targets_node_address_and_id() {
        let addr = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 5));
        let node = Node::new(NodeId::new(), "edge-1", addr);

        let job = build_icmp_job(&node, IcmpCheck::default(), 30, Uuid::nil());

        assert_eq!(job.node_id, node.id);
        assert_eq!(job.target, addr);
        assert_eq!(job.interval_secs, 30);
        assert!(matches!(job.check, CheckSpec::Icmp(_)));
    }

    #[test]
    fn snmp_job_carries_check_and_target() {
        let addr = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 6));
        let node = Node::new(NodeId::new(), "sw-1", addr);
        let check = SnmpCheck {
            community: "public".to_owned(),
            oids: vec!["1.3.6.1.2.1.1.3.0".to_owned()],
            columns: Vec::new(),
            timeout_ms: 2000,
        };
        let job = build_snmp_job(&node, check, 60, Uuid::nil());
        assert_eq!(job.target, addr);
        assert!(matches!(job.check, CheckSpec::Snmp(_)));
    }

    #[test]
    fn snmp_table_job_carries_table_check() {
        let addr = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 7));
        let node = Node::new(NodeId::new(), "sw-2", addr);
        let check = SnmpTableCheck {
            community: "public".to_owned(),
            columns: vec![SnmpColumn {
                metric_name: "if_hc_in_octets".to_owned(),
                oid: "1.3.6.1.2.1.31.1.1.1.6".to_owned(),
                kind: yagra_common::MetricKind::Counter,
            }],
            meta_columns: Vec::new(),
            timeout_ms: 2000,
        };
        let job = build_snmp_table_job(&node, check, 60, Uuid::nil());
        assert_eq!(job.target, addr);
        assert!(matches!(job.check, CheckSpec::SnmpTable(_)));
    }

    fn item(metric: &str, oid: &str, kind: CollectionKind) -> CollectionItem {
        CollectionItem {
            metric_name: metric.to_owned(),
            oid: oid.to_owned(),
            kind,
            metric_kind: yagra_common::MetricKind::Gauge,
        }
    }

    /// An optical item for `flavor`, publishing under `metric`.
    fn optical_item(metric: &str, flavor: OpticalFlavor) -> CollectionItem {
        item(metric, flavor.root_oid(), CollectionKind::Optical)
    }

    #[test]
    fn optical_items_group_into_one_probe_per_dialect() {
        let items = vec![
            optical_item(METRIC_IF_RX_POWER_DBM, OpticalFlavor::Huawei),
            optical_item(METRIC_IF_TX_POWER_DBM, OpticalFlavor::Huawei),
        ];
        let check = build_snmp_optical_check("public", &items, 1000).expect("a probe");
        assert_eq!(
            check.probes.len(),
            1,
            "one dialect must not become two sessions"
        );
        assert_eq!(check.probes[0].flavor, OpticalFlavor::Huawei);
        assert_eq!(
            check.probes[0].rx_metric.as_deref(),
            Some(METRIC_IF_RX_POWER_DBM)
        );
        assert_eq!(
            check.probes[0].tx_metric.as_deref(),
            Some(METRIC_IF_TX_POWER_DBM)
        );
    }

    /// Disabling one half at node scope must collect the other half, not both and not neither.
    /// The resolver filters disabled rows before we see them, so a missing item is the whole signal.
    #[test]
    fn one_disabled_half_leaves_the_other_collected() {
        let items = vec![optical_item(METRIC_IF_RX_POWER_DBM, OpticalFlavor::Juniper)];
        let check = build_snmp_optical_check("public", &items, 1000).expect("a probe");
        assert_eq!(
            check.probes[0].rx_metric.as_deref(),
            Some(METRIC_IF_RX_POWER_DBM)
        );
        assert!(check.probes[0].tx_metric.is_none());
    }

    /// A node bound to two vendor profiles gets both dialects, each once.
    #[test]
    fn two_dialects_on_one_node_stay_separate_probes() {
        let items = vec![
            optical_item(METRIC_IF_RX_POWER_DBM, OpticalFlavor::Huawei),
            optical_item(METRIC_IF_TX_POWER_DBM, OpticalFlavor::Huawei),
            optical_item(METRIC_IF_RX_POWER_DBM, OpticalFlavor::EntitySensor),
        ];
        let check = build_snmp_optical_check("public", &items, 1000).expect("probes");
        assert_eq!(check.probes.len(), 2);
    }

    /// An OID no dialect claims produces no job at all — core is where a metric name is decided,
    /// and a probe the poller cannot interpret would spend an SNMP session per poll for nothing.
    #[test]
    fn an_unrecognised_optical_oid_yields_no_check() {
        let items = vec![item(
            METRIC_IF_RX_POWER_DBM,
            "1.3.6.1.4.1.99999.1",
            CollectionKind::Optical,
        )];
        assert!(build_snmp_optical_check("public", &items, 1000).is_none());
    }

    /// A metric name outside the pair has no reading to publish, so it contributes nothing — and,
    /// being the only item, leaves no probe behind either.
    #[test]
    fn an_optical_item_with_an_unknown_metric_name_is_dropped() {
        let items = vec![optical_item("if_optical_something", OpticalFlavor::H3c)];
        assert!(build_snmp_optical_check("public", &items, 1000).is_none());
    }

    /// A node with no optical items gets no optical job — the overwhelmingly common case, and the
    /// one that decides whether this feature costs every SNMP node an extra session per poll.
    #[test]
    fn a_node_with_no_optical_items_gets_no_optical_check() {
        let items = vec![
            item(
                "snmp_sys_uptime_ticks",
                "1.3.6.1.2.1.1.3.0",
                CollectionKind::Scalar,
            ),
            item(
                "if_hc_in_octets",
                "1.3.6.1.2.1.31.1.1.1.6",
                CollectionKind::Table,
            ),
        ];
        assert!(build_snmp_optical_check("public", &items, 1000).is_none());
        assert!(build_snmp_v3_optical_check(&v3_secret(), &items, 1000).is_none());
    }

    /// The v3 twin carries the same probes and the caller's USM identity.
    #[test]
    fn the_v3_optical_check_mirrors_the_v2c_one() {
        let items = vec![
            optical_item(METRIC_IF_RX_POWER_DBM, OpticalFlavor::Juniper),
            optical_item(METRIC_IF_TX_POWER_DBM, OpticalFlavor::Juniper),
        ];
        let v2c = build_snmp_optical_check("public", &items, 1000).expect("v2c");
        let v3 = build_snmp_v3_optical_check(&v3_secret(), &items, 1000).expect("v3");
        assert_eq!(v2c.probes, v3.probes);
        assert_eq!(v3.user, "monitor");
    }

    #[test]
    fn build_snmp_checks_splits_scalar_and_table_and_adds_meta() {
        let items = [
            item(
                "snmp_sys_uptime_ticks",
                "1.3.6.1.2.1.1.3.0",
                CollectionKind::Scalar,
            ),
            item(
                "if_oper_status",
                "1.3.6.1.2.1.2.2.1.8",
                CollectionKind::Table,
            ),
        ];
        let (scalar, table) = build_snmp_checks("public", &items, 2000);
        let scalar = scalar.expect("scalar check present");
        assert_eq!(scalar.columns.len(), 1);
        assert_eq!(scalar.columns[0].metric_name, "snmp_sys_uptime_ticks");
        assert!(scalar.oids.is_empty()); // configured scalars travel as named columns
        let table = table.expect("table check present");
        assert_eq!(table.columns.len(), 1);
        // Standard interface-metadata columns are attached so interfaces get names.
        assert!(!table.meta_columns.is_empty());
    }

    #[test]
    fn build_snmp_checks_empty_set_yields_no_checks() {
        let (scalar, table) = build_snmp_checks("public", &[], 2000);
        assert!(scalar.is_none());
        assert!(table.is_none());
    }

    fn v3_secret() -> SnmpV3Secret {
        SnmpV3Secret::parse(
            br#"{"user":"monitor","security_level":"authpriv","auth_protocol":"sha256",
                 "auth_key":"a-pass","priv_protocol":"aes128","priv_key":"p-pass"}"#,
        )
        .expect("valid v3 secret")
    }

    #[test]
    fn build_snmp_v3_check_carries_usm_params_and_scalar_columns_only() {
        let items = [
            item(
                "snmp_sys_uptime_ticks",
                "1.3.6.1.2.1.1.3.0",
                CollectionKind::Scalar,
            ),
            item(
                "if_oper_status",
                "1.3.6.1.2.1.2.2.1.8",
                CollectionKind::Table,
            ),
        ];
        let check = build_snmp_v3_check(&v3_secret(), &items, 2000).expect("scalar check");
        assert_eq!(check.user, "monitor");
        assert_eq!(check.security_level, "authpriv");
        assert_eq!(check.auth_protocol.as_deref(), Some("sha256"));
        // Only the scalar item lands in the scalar check; the table item goes to the table check.
        assert_eq!(check.columns.len(), 1);
        assert_eq!(check.columns[0].metric_name, "snmp_sys_uptime_ticks");
        assert!(check.oids.is_empty());
    }

    #[test]
    fn build_snmp_v3_check_without_scalars_yields_none() {
        let items = [item(
            "if_oper_status",
            "1.3.6.1.2.1.2.2.1.8",
            CollectionKind::Table,
        )];
        assert!(build_snmp_v3_check(&v3_secret(), &items, 2000).is_none());
    }

    #[test]
    fn build_snmp_v3_table_check_carries_usm_params_table_columns_and_meta() {
        let items = [
            item(
                "snmp_sys_uptime_ticks",
                "1.3.6.1.2.1.1.3.0",
                CollectionKind::Scalar,
            ),
            item(
                "if_hc_in_octets",
                "1.3.6.1.2.1.31.1.1.1.6",
                CollectionKind::Table,
            ),
        ];
        let check = build_snmp_v3_table_check(&v3_secret(), &items, 2000).expect("table check");
        assert_eq!(check.user, "monitor");
        assert_eq!(check.priv_protocol.as_deref(), Some("aes128"));
        // Only the table item becomes a walk column; the scalar stays in the scalar check.
        assert_eq!(check.columns.len(), 1);
        assert_eq!(check.columns[0].oid, "1.3.6.1.2.1.31.1.1.1.6");
        // Interface-metadata columns are attached so discovered interfaces get names (ADR-011).
        assert!(
            !check.meta_columns.is_empty(),
            "v3 table walk carries the ifName/ifAlias/ifSpeed metadata columns"
        );
    }

    #[test]
    fn build_snmp_v3_table_check_without_tables_yields_none() {
        let items = [item(
            "snmp_sys_uptime_ticks",
            "1.3.6.1.2.1.1.3.0",
            CollectionKind::Scalar,
        )];
        assert!(build_snmp_v3_table_check(&v3_secret(), &items, 2000).is_none());
    }

    #[test]
    fn snmp_v3_job_targets_node_and_tags_check() {
        let addr = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 8));
        let node = Node::new(NodeId::new(), "fw-1", addr);
        let items = [item(
            "snmp_sys_uptime_ticks",
            "1.3.6.1.2.1.1.3.0",
            CollectionKind::Scalar,
        )];
        let check = build_snmp_v3_check(&v3_secret(), &items, 2000).unwrap();
        let job = build_snmp_v3_job(&node, check, 60, Uuid::nil());
        assert_eq!(job.target, addr);
        assert!(matches!(job.check, CheckSpec::SnmpV3(_)));
    }

    fn node(name: &str) -> Node {
        Node::new(NodeId::new(), name, IpAddr::V4(Ipv4Addr::new(10, 0, 0, 20)))
    }

    /// The kinds present in an assembled job list (order-independent assertions).
    fn kinds(jobs: &[(PollJob, &'static str)]) -> Vec<&'static str> {
        jobs.iter().map(|(_, k)| *k).collect()
    }

    #[test]
    fn assemble_without_auth_yields_icmp_only() {
        // No SNMP credential resolved ⇒ liveness only, regardless of the collection set.
        let jobs = assemble_node_jobs(
            &node("ping-only"),
            None,
            &[],
            None,
            30,
            &AdjacencyPolicy::default(),
        );
        assert_eq!(kinds(&jobs), vec!["icmp"]);
        assert!(matches!(jobs[0].0.check, CheckSpec::Icmp(_)));
    }

    #[test]
    fn assemble_url_monitor_yields_http_only_no_icmp() {
        // A URL monitor is HTTP-only: no ICMP (target may be non-pingable) and no SNMP.
        let cfg = UrlCheckConfig::new("https://api.example.com/health");
        let monitor = SpecialMonitor::resolve(Some(&cfg), None);
        let jobs = assemble_node_jobs(
            &node("url-mon"),
            None,
            &[],
            monitor,
            30,
            &AdjacencyPolicy::default(),
        );
        assert_eq!(kinds(&jobs), vec!["http"]);
        assert!(matches!(jobs[0].0.check, CheckSpec::Http(_)));
    }

    #[test]
    fn assemble_dns_monitor_yields_dns_only_no_icmp() {
        // A DNS monitor is DNS-only: no ICMP (a name has no address of its own, and the resolver
        // need not be pingable) and no SNMP.
        let cfg = DnsCheckConfig::new("horryworks.net");
        let monitor = SpecialMonitor::resolve(None, Some(&cfg));
        let jobs = assemble_node_jobs(
            &node("dns-mon"),
            None,
            &[],
            monitor,
            30,
            &AdjacencyPolicy::default(),
        );
        assert_eq!(kinds(&jobs), vec!["dns"]);
        assert!(matches!(jobs[0].0.check, CheckSpec::Dns(_)));
    }

    #[test]
    fn resolve_prefers_url_when_a_node_somehow_has_both_kinds() {
        // The API edge refuses to create the second row (`api::reject_conflicting_monitor`, both
        // directions), but a row can predate that guard, so dispatch must still be deterministic
        // rather than depending on which side-table lookup ran first.
        let url = UrlCheckConfig::new("https://api.example.com/health");
        let dns = DnsCheckConfig::new("horryworks.net");
        let monitor = SpecialMonitor::resolve(Some(&url), Some(&dns));
        assert!(matches!(monitor, Some(SpecialMonitor::Url { .. })));
        let jobs = assemble_node_jobs(
            &node("both"),
            None,
            &[],
            monitor,
            30,
            &AdjacencyPolicy::default(),
        );
        assert_eq!(kinds(&jobs), vec!["http"]);
    }

    #[test]
    fn resolve_maps_each_single_row_and_no_rows() {
        // Pins the truth table for the job builder's view of it. The rule itself lives in
        // `NodeKind::resolve`; the dispatcher's load order defers to this, which defers to that.
        let url = UrlCheckConfig::new("https://api.example.com/health");
        let dns = DnsCheckConfig::new("horryworks.net");
        assert!(matches!(
            SpecialMonitor::resolve(Some(&url), None),
            Some(SpecialMonitor::Url { .. })
        ));
        assert!(matches!(
            SpecialMonitor::resolve(None, Some(&dns)),
            Some(SpecialMonitor::Dns(_))
        ));
        assert!(SpecialMonitor::resolve(None, None).is_none());
    }

    #[test]
    fn a_missing_hint_admits_every_node_but_an_empty_hint_admits_none() {
        // The only way to get `hint_admits` wrong, and both directions are silent in production:
        // read `None` as "nothing to poll" and the on-demand "poll now" path stops finding any
        // URL/DNS monitor; read an empty set as "no hint" and every node queries every side table
        // again, which is the 50k-round-trips-a-sweep debt this exists to close.
        let id = Uuid::new_v4();
        let other = Uuid::new_v4();

        assert!(hint_admits(None, id), "no hint must fall back to querying");

        let empty: HashSet<Uuid> = HashSet::new();
        assert!(
            !hint_admits(Some(&empty), id),
            "an empty preload means there are no monitors of this kind, not that we don't know"
        );

        let one: HashSet<Uuid> = [id].into_iter().collect();
        assert!(hint_admits(Some(&one), id));
        assert!(!hint_admits(Some(&one), other));
    }

    #[test]
    fn the_default_hints_query_every_side_table() {
        // `build_node_jobs` (the operator's "poll now", one node) passes the default. If a kind
        // ever defaulted to `Some(∅)` that path would silently stop polling monitors of that kind
        // — and it is the path an operator uses precisely when they suspect something is wrong.
        let hints = MonitorHints::default();
        assert!(hints.url.is_none());
        assert!(hints.dns.is_none());
        let id = Uuid::new_v4();
        assert!(hint_admits(hints.url, id));
        assert!(hint_admits(hints.dns, id));
    }

    #[test]
    fn the_job_builder_and_the_api_resolve_a_node_to_the_same_kind() {
        // `SpecialMonitor::resolve` is a view of `NodeKind::resolve`, not a second copy of the
        // precedence — the node-detail API answers the same question from the same function, and
        // the two disagreeing is exactly the bug this arrangement removes (a node polled as one
        // kind while the operator is shown another). Delegation is cheap to "optimize" back into a
        // local match, so pin it.
        let url = UrlCheckConfig::new("https://api.example.com/health");
        let dns = DnsCheckConfig::new("horryworks.net");
        for (u, d) in [
            (None, None),
            (Some(&url), None),
            (None, Some(&dns)),
            (Some(&url), Some(&dns)),
        ] {
            let rows = NodeRows {
                meraki: false,
                url: u.is_some(),
                dns: d.is_some(),
            };
            let expected = match NodeKind::resolve(rows) {
                NodeKind::Url => Some("url"),
                NodeKind::Dns => Some("dns"),
                NodeKind::Meraki | NodeKind::Device => None,
            };
            let actual = SpecialMonitor::resolve(u, d).map(|m| match m {
                SpecialMonitor::Url { .. } => "url",
                SpecialMonitor::Dns(_) => "dns",
            });
            assert_eq!(actual, expected, "rows {rows:?} resolved differently");
        }
    }

    #[test]
    fn build_dns_check_maps_every_config_field() {
        let cfg = DnsCheckConfig {
            name: "horryworks.net".into(),
            record_type: yagra_common::DnsRecordType::Aaaa,
            resolver: Some("10.0.0.53".parse().unwrap()),
            resolver_port: 5353,
            max_depth: 4,
            timeout_ms: 1500,
        };
        let check = build_dns_check(&cfg);
        assert_eq!(check.name, "horryworks.net");
        assert_eq!(check.record_type, yagra_common::DnsRecordType::Aaaa);
        assert_eq!(check.resolver, Some("10.0.0.53".parse().unwrap()));
        assert_eq!(check.resolver_port, 5353);
        assert_eq!(check.max_depth, 4);
        assert_eq!(check.timeout_ms, 1500);
    }

    #[test]
    fn dns_job_targets_the_node_address_not_the_name() {
        // `target` stays the node's display address; the real target is the resolver in the check.
        // A DNS monitor is never pinged, so this is display/provenance only.
        let n = node("dns-mon");
        let job = build_dns_job(
            &n,
            build_dns_check(&DnsCheckConfig::new("horryworks.net")),
            30,
            Uuid::nil(),
        );
        assert_eq!(job.target, n.address);
        assert_eq!(job.interval_secs, 30);
        assert!(!job.probe_identity, "DNS monitors never probe sysDescr");
    }

    #[test]
    fn assemble_v2c_builds_scalar_table_and_icmp_and_probes_identity() {
        let items = [
            item(
                "snmp_sys_uptime_ticks",
                "1.3.6.1.2.1.1.3.0",
                CollectionKind::Scalar,
            ),
            item(
                "if_oper_status",
                "1.3.6.1.2.1.2.2.1.8",
                CollectionKind::Table,
            ),
        ];
        let auth = SnmpAuth::V2c("public".to_owned());
        let jobs = assemble_node_jobs(
            &node("sw"),
            Some(&auth),
            &items,
            None,
            30,
            &AdjacencyPolicy::default(),
        );
        assert_eq!(
            kinds(&jobs),
            vec![
                "snmp",
                "snmp_table",
                "snmp_neighbors",
                "snmp_mau",
                "snmp_l3",
                "snmp_routing",
                "icmp"
            ]
        );
        // The maker is unknown (vendor None), so the scalar SNMP job carries the identity probe.
        let snmp = jobs.iter().find(|(_, k)| *k == "snmp").unwrap();
        assert!(snmp.0.probe_identity, "unknown-maker node probes sysDescr");
    }

    #[test]
    fn assemble_skips_identity_probe_once_maker_is_known() {
        let mut n = node("classified");
        n.vendor = Some("Cisco".to_owned());
        let items = [item(
            "snmp_sys_uptime_ticks",
            "1.3.6.1.2.1.1.3.0",
            CollectionKind::Scalar,
        )];
        let auth = SnmpAuth::V2c("public".to_owned());
        let jobs = assemble_node_jobs(
            &n,
            Some(&auth),
            &items,
            None,
            30,
            &AdjacencyPolicy::default(),
        );
        let snmp = jobs.iter().find(|(_, k)| *k == "snmp").unwrap();
        assert!(
            !snmp.0.probe_identity,
            "known-maker node stops probing sysDescr"
        );
    }

    #[test]
    fn assemble_v3_builds_scalar_table_and_icmp() {
        // v3 now walks the table set too (the GETBULK v3 walk): a v3 node with both a scalar and a
        // table item produces the scalar job, the table-walk job, and the always-on ICMP.
        let items = [
            item(
                "snmp_sys_uptime_ticks",
                "1.3.6.1.2.1.1.3.0",
                CollectionKind::Scalar,
            ),
            item(
                "if_hc_in_octets",
                "1.3.6.1.2.1.31.1.1.1.6",
                CollectionKind::Table,
            ),
        ];
        let auth = SnmpAuth::V3(v3_secret());
        let jobs = assemble_node_jobs(
            &node("fw"),
            Some(&auth),
            &items,
            None,
            30,
            &AdjacencyPolicy::default(),
        );
        assert_eq!(
            kinds(&jobs),
            vec![
                "snmp_v3",
                "snmp_v3_table",
                "snmp_v3_neighbors",
                "snmp_v3_mau",
                "snmp_v3_l3",
                "snmp_v3_routing",
                "icmp"
            ]
        );
        assert!(matches!(
            jobs.iter().find(|(_, k)| *k == "snmp_v3").unwrap().0.check,
            CheckSpec::SnmpV3(_)
        ));
        assert!(matches!(
            jobs.iter()
                .find(|(_, k)| *k == "snmp_v3_table")
                .unwrap()
                .0
                .check,
            CheckSpec::SnmpV3Table(_)
        ));
    }

    #[test]
    fn assemble_v3_scalar_only_omits_table_job() {
        // A v3 node with no table items produces just the scalar + ICMP jobs (no empty walk job).
        let items = [item(
            "snmp_sys_uptime_ticks",
            "1.3.6.1.2.1.1.3.0",
            CollectionKind::Scalar,
        )];
        let auth = SnmpAuth::V3(v3_secret());
        let jobs = assemble_node_jobs(
            &node("fw"),
            Some(&auth),
            &items,
            None,
            30,
            &AdjacencyPolicy::default(),
        );
        assert_eq!(
            kinds(&jobs),
            vec![
                "snmp_v3",
                "snmp_v3_neighbors",
                "snmp_v3_mau",
                "snmp_v3_l3",
                "snmp_v3_routing",
                "icmp"
            ]
        );
    }

    /// The neighbour job is the only one that does **not** run at the node's interval. Adjacency
    /// changes on the order of months, so riding the metric cadence would walk two extra tables on
    /// every SNMP node every minute — device load spent re-reading a constant.
    #[test]
    fn the_neighbor_job_carries_its_own_slow_cadence_not_the_nodes() {
        let items = [item(
            "if_hc_in_octets",
            "1.3.6.1.2.1.31.1.1.1.6",
            CollectionKind::Table,
        )];
        let auth = SnmpAuth::V2c("public".to_owned());
        let policy = AdjacencyPolicy::default();
        let jobs = assemble_node_jobs(&node("sw"), Some(&auth), &items, None, 30, &policy);
        for (job, kind) in &jobs {
            let expected = if kind.contains("neighbors")
                || kind.contains("l3")
                || kind.contains("routing")
                || kind.contains("mau")
            {
                3600
            } else {
                30
            };
            assert_eq!(job.interval_secs, expected, "{kind} cadence");
        }
        // The legacy publish path keys its extra due-check off exactly this inequality.
        let neighbor = jobs.iter().find(|(_, k)| *k == "snmp_neighbors").unwrap();
        assert!(neighbor.0.interval_secs > 30);
        assert!(matches!(neighbor.0.check, CheckSpec::SnmpNeighbors(_)));
        // The interface-address walk rides the same slow tier, for the same reason (ADR-043).
        let l3 = jobs.iter().find(|(_, k)| *k == "snmp_l3").unwrap();
        assert!(l3.0.interval_secs > 30);
        assert!(matches!(l3.0.check, CheckSpec::SnmpL3(_)));
    }

    /// The toggle is the whole safety valve: off must mean no job is built at all, on either
    /// protocol — not a job the poller then declines to run.
    #[test]
    fn disabling_collection_emits_no_neighbor_job_for_either_protocol() {
        let off = AdjacencyPolicy {
            neighbors_enabled: false,
            l3_enabled: false,
            media_enabled: false,
            ..AdjacencyPolicy::default()
        };
        let items = [item(
            "if_hc_in_octets",
            "1.3.6.1.2.1.31.1.1.1.6",
            CollectionKind::Table,
        )];
        let v2c = assemble_node_jobs(
            &node("sw"),
            Some(&SnmpAuth::V2c("public".to_owned())),
            &items,
            None,
            30,
            &off,
        );
        assert_eq!(kinds(&v2c), vec!["snmp_table", "snmp_routing", "icmp"]);
        let v3 = assemble_node_jobs(
            &node("fw"),
            Some(&SnmpAuth::V3(v3_secret())),
            &items,
            None,
            30,
            &off,
        );
        assert_eq!(kinds(&v3), vec!["snmp_v3_table", "snmp_v3_routing", "icmp"]);
    }

    /// The two walks share a settings struct but not a switch. A fleet may want L2 adjacency and
    /// not L3 addressing (or the reverse), and folding them onto one toggle would take that away —
    /// this pins that the sharing is about *when settings are resolved*, not about what they mean.
    #[test]
    fn the_two_discovery_walks_toggle_independently() {
        let items = [item(
            "if_hc_in_octets",
            "1.3.6.1.2.1.31.1.1.1.6",
            CollectionKind::Table,
        )];
        let auth = SnmpAuth::V2c("public".to_owned());

        let l3_only = AdjacencyPolicy {
            neighbors_enabled: false,
            media_enabled: false,
            ..AdjacencyPolicy::default()
        };
        let jobs = assemble_node_jobs(&node("sw"), Some(&auth), &items, None, 30, &l3_only);
        assert_eq!(
            kinds(&jobs),
            vec!["snmp_table", "snmp_l3", "snmp_routing", "icmp"]
        );

        let neighbors_only = AdjacencyPolicy {
            l3_enabled: false,
            media_enabled: false,
            ..AdjacencyPolicy::default()
        };
        let jobs = assemble_node_jobs(&node("sw"), Some(&auth), &items, None, 30, &neighbors_only);
        assert_eq!(
            kinds(&jobs),
            vec!["snmp_table", "snmp_neighbors", "snmp_routing", "icmp"]
        );
    }

    /// The media walk is on by default, has its own switch, and rides the slow tier (ADR-063 Inc.2).
    ///
    /// Three properties in one test because they are the three ways this increment could be wrong in
    /// production and in no local test: **shipped off** would leave the Media column empty on every
    /// fleet with nobody knowing there was a switch; **not independently switchable** would make an
    /// operator disable neighbour discovery to stop it; and **riding `interval_secs`** would walk
    /// `ifMauTable` on every 48-port switch every 30 seconds to learn a fact that changes when
    /// someone unplugs a cable.
    #[test]
    fn the_media_walk_is_on_by_default_switchable_and_slow() {
        let items = [item(
            "if_hc_in_octets",
            "1.3.6.1.2.1.31.1.1.1.6",
            CollectionKind::Table,
        )];
        assert!(
            AdjacencyPolicy::default().media_enabled,
            "shipped on by default (ADR-063 decision 6)"
        );

        for (auth, kind) in [
            (SnmpAuth::V2c("public".to_owned()), "snmp_mau"),
            (SnmpAuth::V3(v3_secret()), "snmp_v3_mau"),
        ] {
            let on = assemble_node_jobs(
                &node("sw"),
                Some(&auth),
                &items,
                None,
                30,
                &AdjacencyPolicy::default(),
            );
            let job = on
                .iter()
                .find(|(_, k)| *k == kind)
                .unwrap_or_else(|| panic!("{kind} must be built by default"));
            assert!(
                matches!(job.0.check, CheckSpec::SnmpMau(_) | CheckSpec::SnmpV3Mau(_)),
                "{kind} carries a media spec"
            );
            assert_eq!(job.0.interval_secs, 3600, "{kind} rides the slow tier");
            assert!(job.0.interval_secs > 30, "{kind} is slower than the node");

            // Off must mean no job at all, not a job the poller declines — the same safety valve
            // the neighbour toggle test pins.
            let off = AdjacencyPolicy {
                media_enabled: false,
                ..AdjacencyPolicy::default()
            };
            let jobs = assemble_node_jobs(&node("sw"), Some(&auth), &items, None, 30, &off);
            assert!(
                !kinds(&jobs).contains(&kind),
                "{kind} must not be built when the toggle is off"
            );
            // …and turning it off leaves the neighbour walk alone: separate switches, one struct.
            assert!(kinds(&jobs).iter().any(|k| k.contains("neighbors")));
        }
    }

    /// ARP discovery is off in the default policy, and that default is what every node gets until an
    /// operator says otherwise. A regression here would start walking `ipNetToPhysicalTable` on
    /// every SNMP node in a fleet on the strength of an upgrade.
    #[test]
    fn the_arp_walk_is_absent_from_the_default_policy_on_both_protocols() {
        let items = [item(
            "if_hc_in_octets",
            "1.3.6.1.2.1.31.1.1.1.6",
            CollectionKind::Table,
        )];
        assert!(!AdjacencyPolicy::default().arp_enabled);
        for (auth, kind) in [
            (SnmpAuth::V2c("public".to_owned()), "snmp_arp"),
            (SnmpAuth::V3(v3_secret()), "snmp_v3_arp"),
        ] {
            let jobs = assemble_node_jobs(
                &node("sw"),
                Some(&auth),
                &items,
                None,
                30,
                &AdjacencyPolicy::default(),
            );
            assert!(
                !kinds(&jobs).contains(&kind),
                "{kind} must not be issued unless an operator enabled it"
            );
        }
    }

    /// Turned on, it rides its own cadence and carries the columns and the row budget core decides.
    #[test]
    fn an_enabled_arp_walk_carries_its_own_cadence_columns_and_row_budget() {
        let items = [item(
            "if_hc_in_octets",
            "1.3.6.1.2.1.31.1.1.1.6",
            CollectionKind::Table,
        )];
        let policy = AdjacencyPolicy {
            arp_enabled: true,
            arp_interval_secs: 21_600,
            ..AdjacencyPolicy::default()
        };
        let jobs = assemble_node_jobs(
            &node("sw"),
            Some(&SnmpAuth::V2c("public".to_owned())),
            &items,
            None,
            30,
            &policy,
        );
        let arp = jobs.iter().find(|(_, k)| *k == "snmp_arp").expect("issued");
        assert_eq!(arp.0.interval_secs, 21_600);
        let CheckSpec::SnmpArp(check) = &arp.0.check else {
            panic!("wrong variant");
        };
        // The budget must reach the wire: a poller that received 0 would collect nothing, silently.
        assert_eq!(
            usize::try_from(check.max_rows).unwrap(),
            yagra_common::MAX_ARP_WALK_ROWS
        );
        let sent: Vec<&str> = check.columns.iter().map(|c| c.oid.as_str()).collect();
        let declared: Vec<&str> = builtin_arp_columns().iter().map(|(_, o)| *o).collect();
        assert_eq!(
            sent, declared,
            "core is the one place the OID set is decided"
        );
    }

    /// The routing walk is the third to ride the slow tier, and unlike ARP it ships on. Its cost
    /// argument is the tables it reads, so pin that it is actually issued by default.
    #[test]
    fn the_routing_walk_is_issued_by_default_on_both_protocols() {
        let items = [item(
            "if_hc_in_octets",
            "1.3.6.1.2.1.31.1.1.1.6",
            CollectionKind::Table,
        )];
        assert!(AdjacencyPolicy::default().routing_enabled);
        for (auth, kind) in [
            (SnmpAuth::V2c("public".to_owned()), "snmp_routing"),
            (SnmpAuth::V3(v3_secret()), "snmp_v3_routing"),
        ] {
            let jobs = assemble_node_jobs(
                &node("rtr"),
                Some(&auth),
                &items,
                None,
                30,
                &AdjacencyPolicy::default(),
            );
            let job = jobs.iter().find(|(_, k)| *k == kind).expect("issued");
            assert_eq!(job.0.interval_secs, 3600, "{kind} rides the slow tier");
        }
    }

    /// The toggle is the safety valve here too: off means no job is built at all, on either
    /// protocol — not a job the poller then declines to run.
    #[test]
    fn disabling_routing_collection_emits_no_job_for_either_protocol() {
        let off = AdjacencyPolicy {
            routing_enabled: false,
            ..AdjacencyPolicy::default()
        };
        for auth in [
            SnmpAuth::V2c("public".to_owned()),
            SnmpAuth::V3(v3_secret()),
        ] {
            let jobs = assemble_node_jobs(&node("rtr"), Some(&auth), &[], None, 30, &off);
            assert!(kinds(&jobs).iter().all(|k| !k.contains("routing")));
        }
    }

    /// A node with no host address of its own gets the adjacency walk and **no probes** — the rule
    /// that keeps `inetCidrRouteTable` off 50,000 nodes. Nearly every node is in this state.
    #[test]
    fn a_node_with_no_planned_targets_walks_adjacency_but_probes_nothing() {
        let jobs = assemble_node_jobs(
            &node("rtr"),
            Some(&SnmpAuth::V2c("public".to_owned())),
            &[],
            None,
            30,
            &AdjacencyPolicy::default(),
        );
        let job = jobs
            .iter()
            .find(|(_, k)| *k == "snmp_routing")
            .expect("issued");
        let CheckSpec::SnmpRouting(check) = &job.0.check else {
            panic!("wrong variant");
        };
        assert!(check.route_probes.is_empty());
        let sent: Vec<&str> = check.columns.iter().map(|c| c.oid.as_str()).collect();
        let declared: Vec<&str> = builtin_routing_columns().iter().map(|(_, o)| *o).collect();
        assert_eq!(
            sent, declared,
            "core is the one place the OID set is decided"
        );
    }

    /// A planned target becomes one probe per route column, each rooted at that destination — and
    /// the OID must end in the destination's index encoding, because that is the whole reason the
    /// routing table can be consulted without being walked.
    #[test]
    fn a_planned_target_becomes_one_probe_per_route_column() {
        let targets = ["198.51.100.2".parse().unwrap()];
        let check = build_snmp_routing_check("public", &targets, 2000);
        assert_eq!(check.route_probes.len(), route_probe_columns().len());
        for probe in &check.route_probes {
            assert_eq!(probe.target, targets[0]);
            assert!(
                probe.oid.ends_with(".1.4.198.51.100.2"),
                "the probe must pin the destination: {}",
                probe.oid
            );
        }
        // Both protocols must ask for the same thing, or a v3 fleet quietly derives fewer links.
        let v3 = build_snmp_v3_routing_check(&v3_secret(), &targets, 2000);
        let v2c_oids: Vec<&str> = check.route_probes.iter().map(|p| p.oid.as_str()).collect();
        let v3_oids: Vec<&str> = v3.route_probes.iter().map(|p| p.oid.as_str()).collect();
        assert_eq!(v2c_oids, v3_oids);
        assert_eq!(check.columns.len(), v3.columns.len());
    }

    /// The L3 job must carry exactly the columns `yagra-common` declares — core is the one place
    /// the OID set is decided, and a poller that receives a short list silently collects less.
    #[test]
    fn the_l3_job_carries_every_declared_column() {
        let check = build_snmp_l3_check("public", 2000);
        assert_eq!(check.columns.len(), builtin_l3_columns().len());
        let v3 = build_snmp_v3_l3_check(&v3_secret(), 2000);
        assert_eq!(v3.columns.len(), builtin_l3_columns().len());
        // Both protocols must ask for the same thing, or a v3 fleet quietly derives fewer links.
        let v2c_oids: Vec<&str> = check.columns.iter().map(|c| c.oid.as_str()).collect();
        let v3_oids: Vec<&str> = v3.columns.iter().map(|c| c.oid.as_str()).collect();
        assert_eq!(v2c_oids, v3_oids);
    }

    /// A node with no SNMP credential gets no neighbour job even with collection enabled: there is
    /// nothing to authenticate the walk with, and an ICMP-only node has no LLDP/CDP tables anyway.
    #[test]
    fn a_node_without_snmp_gets_no_neighbor_job() {
        let jobs = assemble_node_jobs(
            &node("ping-only"),
            None,
            &[],
            None,
            30,
            &AdjacencyPolicy::default(),
        );
        assert_eq!(kinds(&jobs), vec!["icmp"]);
    }

    /// Single-purpose monitors return before the SNMP arm, so they must not gain a neighbour job —
    /// a URL or DNS monitor has no device to walk.
    #[test]
    fn single_purpose_monitors_get_no_neighbor_job() {
        let url = UrlCheckConfig::new("https://api.example.com/health");
        let jobs = assemble_node_jobs(
            &node("url-mon"),
            None,
            &[],
            SpecialMonitor::resolve(Some(&url), None),
            30,
            &AdjacencyPolicy::default(),
        );
        assert_eq!(kinds(&jobs), vec!["http"]);
    }

    /// The wire carries the fixed LLDP/CDP column list, so a poller never has to know it — and the
    /// two protocol variants must agree on it.
    #[test]
    fn both_neighbor_checks_carry_the_same_fixed_column_list() {
        let expected = builtin_neighbor_columns().len();
        let v2c = build_snmp_neighbor_check("public", 2000);
        let v3 = build_snmp_v3_neighbor_check(&v3_secret(), 2000);
        assert_eq!(v2c.columns.len(), expected);
        assert_eq!(v3.columns.len(), expected);
        assert_eq!(v2c.columns, v3.columns);
        assert!(!v2c.columns.is_empty());
    }
}
