//! Job scheduling: turn inventory into [`PollJob`]s for the bus.
//!
//! The scheduler is the core-side producer of work. For the walking skeleton it builds
//! one ICMP job per node; the full scheduler adds per-metric intervals, jitter, and
//! pool-aware dispatch (ADR-009). Jobs carry everything the poller needs (ADR-003), so
//! this is pure given a node.

use uuid::Uuid;
use yagra_bus::{IcmpCheck, PollJob, SnmpCheck};
use yagra_common::Node;

/// Build an ICMP poll job targeting a node's management address.
#[must_use]
pub fn build_icmp_job(node: &Node, check: IcmpCheck, interval_secs: u32, job_id: Uuid) -> PollJob {
    PollJob::icmp(job_id, node.id, node.address, check, interval_secs)
}

/// Build an SNMP v2c poll job targeting a node's management address.
#[must_use]
pub fn build_snmp_job(node: &Node, check: SnmpCheck, interval_secs: u32, job_id: Uuid) -> PollJob {
    PollJob::snmp(job_id, node.id, node.address, check, interval_secs)
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
            timeout_ms: 2000,
        };
        let job = build_snmp_job(&node, check, 60, Uuid::nil());
        assert_eq!(job.target, addr);
        assert!(matches!(job.check, CheckSpec::Snmp(_)));
    }
}
