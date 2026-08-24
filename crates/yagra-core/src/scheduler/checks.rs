// SPDX-License-Identifier: AGPL-3.0-only
//! One check at a time: given the arguments, build a [`CheckSpec`] or a [`PollJob`].
//!
//! Needs nothing but what it is handed — no store, no clock, no `self`. **Never `.await`s**
//! (`guards.rs` enforces it), and it is the only file allowed to name a [`CheckSpec`] variant,
//! so a check can never be constructed inline at the site that decides to emit it (ADR-096;
//! that inlining is what ADR-084 removed from `assemble_node_jobs`).

use crate::secrets::SnmpV3Secret;
use uuid::Uuid;
use yagra_bus::{
    DnsCheck, HttpCheck, IcmpCheck, OpticalProbe, PollJob, SnmpArpCheck, SnmpArpColumn, SnmpCheck,
    SnmpColumn, SnmpL3Check, SnmpL3Column, SnmpMauCheck, SnmpMetaColumn, SnmpNeighborCheck,
    SnmpNeighborColumn, SnmpOpticalCheck, SnmpRouteProbe, SnmpRoutingCheck, SnmpRoutingColumn,
    SnmpTableCheck, SnmpV3ArpCheck, SnmpV3Check, SnmpV3L3Check, SnmpV3MauCheck,
    SnmpV3NeighborCheck, SnmpV3OpticalCheck, SnmpV3RoutingCheck, SnmpV3TableCheck,
};
use yagra_common::{
    builtin_arp_columns, builtin_interface_meta_columns, builtin_l3_columns,
    builtin_neighbor_columns, builtin_routing_columns, route_probe_columns, route_probe_oid,
    CollectionItem, CollectionKind, DnsCheckConfig, HttpAuth, Node, OpticalFlavor, UrlCheckConfig,
    METRIC_CISCO_TEMP_C, METRIC_IF_RX_POWER_DBM, METRIC_IF_TX_POWER_DBM,
};

/// Build an ICMP poll job targeting a node's management address.
#[must_use]
pub fn build_icmp_job(node: &Node, check: IcmpCheck, interval_secs: u32, job_id: Uuid) -> PollJob {
    PollJob::icmp(job_id, node.id, node.address, check, interval_secs)
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
        auth: secret.auth(),
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
        auth: secret.auth(),
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
        auth: secret.auth(),
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
        auth: secret.auth(),
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
        auth: secret.auth(),
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
        auth: secret.auth(),
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
        auth: secret.auth(),
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
        auth: secret.auth(),
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

#[cfg(test)]
mod tests {
    use super::super::testkit::{item, node, optical_item, v3_secret};
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};
    use yagra_bus::CheckSpec;
    use yagra_common::NodeId;

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
        let job = PollJob::snmp(Uuid::nil(), node.id, node.address, check, 60);
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
        let job = PollJob::snmp_table(Uuid::nil(), node.id, node.address, check, 60);
        assert_eq!(job.target, addr);
        assert!(matches!(job.check, CheckSpec::SnmpTable(_)));
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
        assert_eq!(v3.auth.user, "monitor");
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
        assert_eq!(check.auth.user, "monitor");
        assert_eq!(check.auth.security_level, "authpriv");
        assert_eq!(check.auth.auth_protocol.as_deref(), Some("sha256"));
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
        assert_eq!(check.auth.user, "monitor");
        assert_eq!(check.auth.priv_protocol.as_deref(), Some("aes128"));
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
        let job = PollJob::snmp_v3(Uuid::nil(), node.id, node.address, check, 60);
        assert_eq!(job.target, addr);
        assert!(matches!(job.check, CheckSpec::SnmpV3(_)));
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
