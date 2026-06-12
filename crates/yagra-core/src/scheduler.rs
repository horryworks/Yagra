//! Job scheduling: turn inventory into [`PollJob`]s for the bus.
//!
//! The scheduler is the core-side producer of work. For the walking skeleton it builds
//! one ICMP job per node; the full scheduler adds per-metric intervals, jitter, and
//! pool-aware dispatch (ADR-009). Jobs carry everything the poller needs (ADR-003), so
//! this is pure given a node.

use crate::secrets::SnmpV3Secret;
use uuid::Uuid;
use yagra_bus::{
    IcmpCheck, PollJob, SnmpCheck, SnmpColumn, SnmpMetaColumn, SnmpTableCheck, SnmpV3Check,
};
use yagra_common::{builtin_interface_meta_columns, CollectionItem, CollectionKind, Node};

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

/// Build the SNMP v3 scalar check for a node from its resolved collection set and v3
/// credential. `None` when the set has no scalar items. Table items are **not** polled
/// over v3 yet (the v3 GETBULK walk is a follow-up) — the caller logs what was skipped.
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

#[cfg(test)]
mod tests {
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
        // Only the scalar item travels; tables are a v3 follow-up.
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
}
