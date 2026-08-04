// SPDX-License-Identifier: AGPL-3.0-only
//! Assemble walked adjacency rows and targeted route probes into a [`RoutingSnapshot`]
//! (ADR-043 Increment 4).
//!
//! Kept **pure** — already-walked [`SnmpInstanceRow`]s in, a snapshot out — for the same reason
//! `l3::assemble` and `arp::assemble` are. Three things it must get right:
//!
//! * **The peer is the row index, in all three sources.** `bgpPeerState` is indexed by
//!   `bgpPeerRemoteAddr` and `ospfNbrState` by `(ospfNbrIpAddr, ospfNbrAddressLessIndex)`, so the
//!   instance is not a join key to be discarded — it is the answer. This is the whole reason the
//!   instance walker is used here rather than the numeric one the `T_BGP` metric template uses.
//!
//! * **A route probe needs both of its columns to agree, on the same route.** `inetCidrRouteType`
//!   says whether the destination is on a local interface; `inetCidrRouteIfIndex` says which one.
//!   They are matched on the *full* remaining index, not just the destination, so a `local` route
//!   and a `remote` route to the same address cannot be spliced into a link that neither describes.
//!
//! * **A row that means nothing is dropped, not defaulted.** An `ospfNbrState` row for `0.0.0.0` is
//!   a neighbour the agent has not resolved; a probe that came back `remote(4)` is a routing
//!   decision, not an adjacency. Neither becomes an adjacency with a placeholder in it.

use std::collections::BTreeMap;
use yagra_bus::{SnmpRouteProbe, SnmpRoutingColumn};
use yagra_common::{
    bgp_peer_from_instance, host_prefix_len, ospf_neighbor_from_instance,
    route_prefix_len_from_instance, RoutingAdjacency, RoutingColumn, RoutingProto, RoutingSnapshot,
    INET_CIDR_ROUTE_TYPE_LOCAL,
};
use yagra_transport::{SnmpInstanceRow, SnmpValue};

/// Build the node's routing-adjacency snapshot from the adjacency walk and the probe results.
///
/// `columns` and `probes` are the job's declared lists; rows whose base is in neither are ignored,
/// so a poller that receives a column it has no handling for degrades to "that source is absent"
/// rather than mis-attributing values — the contract every assembler here offers.
///
/// `walk_truncated` comes from the worker, the only layer that knows what budget it asked for.
#[must_use]
pub fn assemble(
    columns: &[SnmpRoutingColumn],
    probes: &[SnmpRouteProbe],
    rows: &[SnmpInstanceRow],
    walk_truncated: bool,
) -> RoutingSnapshot {
    let walked: BTreeMap<&str, RoutingColumn> =
        columns.iter().map(|c| (c.oid.as_str(), c.field)).collect();
    // A probe's OID is its identity: core built one per (column, destination), so two probes never
    // share a root. Keyed by that string, the row's own `oid_base` names its probe directly.
    let probed: BTreeMap<&str, &SnmpRouteProbe> =
        probes.iter().map(|p| (p.oid.as_str(), p)).collect();

    let mut adjacencies = Vec::new();
    // Probe rows, bucketed by (destination, full route index) so the type and the ifIndex that get
    // combined are guaranteed to describe the same route.
    let mut routes: BTreeMap<(std::net::IpAddr, Vec<u32>), RouteRow> = BTreeMap::new();

    for row in rows {
        if let Some(field) = walked.get(row.oid_base.as_str()).copied() {
            if let Some(a) = walked_adjacency(field, row) {
                adjacencies.push(a);
            }
            continue;
        }
        let Some(probe) = probed.get(row.oid_base.as_str()).copied() else {
            continue;
        };
        let entry = routes
            .entry((probe.target, row.instance.clone()))
            .or_default();
        match (probe.field, &row.value) {
            (RoutingColumn::InetCidrRouteType, SnmpValue::Int(v)) => entry.route_type = Some(*v),
            (RoutingColumn::InetCidrRouteIfIndex, SnmpValue::Int(v)) => {
                entry.ifindex = u32::try_from(*v).ok();
            }
            // A wrongly typed value is skipped rather than coerced, and a walked column arriving on
            // a probe OID (or vice versa) is a core that built the job wrongly — neither is guessed
            // at here.
            _ => {}
        }
    }

    for ((target, instance), row) in routes {
        // Only a host route answers the question this probe was sent to ask. A `/24` covering the
        // address says the destination is somewhere on that segment, which is the answer Increment
        // 1 already gives — and a shorter prefix would attach the node to everything behind a
        // summary route.
        if route_prefix_len_from_instance(&instance) != Some(host_prefix_len(target)) {
            continue;
        }
        if row.route_type != Some(INET_CIDR_ROUTE_TYPE_LOCAL) {
            continue;
        }
        let mut a = RoutingAdjacency::new(RoutingProto::Route, target);
        a.local_ifindex = row.ifindex;
        adjacencies.push(a);
    }

    RoutingSnapshot::new(adjacencies, walk_truncated)
}

/// The two probe columns for one route, before they are combined.
#[derive(Default)]
struct RouteRow {
    route_type: Option<i64>,
    ifindex: Option<u32>,
}

/// One walked adjacency row → an adjacency, or `None` for an index this column cannot carry.
fn walked_adjacency(field: RoutingColumn, row: &SnmpInstanceRow) -> Option<RoutingAdjacency> {
    let state = match row.value {
        SnmpValue::Int(v) => Some(v),
        // The state is a plain INTEGER in both MIBs; a different type means the agent answered
        // something other than the column that was asked for, so the row is not trusted at all.
        SnmpValue::Bytes(_) | SnmpValue::Oid(_) => return None,
    };
    match field {
        RoutingColumn::BgpPeerState => {
            let peer = bgp_peer_from_instance(&row.instance)?;
            let mut a = RoutingAdjacency::new(RoutingProto::Bgp, peer);
            a.state = state;
            Some(a)
        }
        RoutingColumn::OspfNbrState => {
            let (peer, ifindex) = ospf_neighbor_from_instance(&row.instance)?;
            let mut a = RoutingAdjacency::new(RoutingProto::Ospf, peer);
            a.local_ifindex = ifindex;
            a.state = state;
            Some(a)
        }
        // The route columns never arrive as walked rows — core sends them only as probes, and the
        // caller routes them by OID before this function is reached. Listed rather than wildcarded
        // so a fifth column cannot land here silently.
        RoutingColumn::InetCidrRouteType | RoutingColumn::InetCidrRouteIfIndex => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::IpAddr;
    use yagra_common::{builtin_routing_columns, route_probe_columns, route_probe_oid};

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    /// The job's walked-column list, exactly as core sends it.
    fn columns() -> Vec<SnmpRoutingColumn> {
        builtin_routing_columns()
            .into_iter()
            .map(|(field, oid)| SnmpRoutingColumn {
                field,
                oid: oid.to_owned(),
            })
            .collect()
    }

    /// The pair of probes core sends for one destination.
    fn probes_for(target: &str) -> Vec<SnmpRouteProbe> {
        let target = ip(target);
        route_probe_columns()
            .into_iter()
            .map(|(field, base)| SnmpRouteProbe {
                field,
                oid: route_probe_oid(base, target),
                target,
            })
            .collect()
    }

    fn base_of(field: RoutingColumn) -> String {
        builtin_routing_columns()
            .into_iter()
            .find(|(f, _)| *f == field)
            .map(|(_, o)| o.to_owned())
            .unwrap()
    }

    fn walked(field: RoutingColumn, instance: &[u32], value: i64) -> SnmpInstanceRow {
        SnmpInstanceRow {
            oid_base: base_of(field),
            instance: instance.to_vec(),
            value: SnmpValue::Int(value),
        }
    }

    fn probe_row(
        probes: &[SnmpRouteProbe],
        field: RoutingColumn,
        instance: &[u32],
        value: i64,
    ) -> SnmpInstanceRow {
        let p = probes.iter().find(|p| p.field == field).unwrap();
        SnmpInstanceRow {
            oid_base: p.oid.clone(),
            instance: instance.to_vec(),
            value: SnmpValue::Int(value),
        }
    }

    /// The index tail a `/32` route to a v4 destination returns: prefix length, then policy and
    /// next hop, neither of which this assembler reads.
    const HOST_ROUTE_INSTANCE: [u32; 6] = [32, 1, 0, 1, 4, 0];

    #[test]
    fn a_bgp_peer_becomes_an_adjacency_carrying_its_state() {
        let rows = vec![walked(RoutingColumn::BgpPeerState, &[192, 0, 2, 1], 6)];
        let snap = assemble(&columns(), &[], &rows, false);
        assert_eq!(snap.len(), 1);
        assert_eq!(snap.adjacencies[0].proto, RoutingProto::Bgp);
        assert_eq!(snap.adjacencies[0].peer, ip("192.0.2.1"));
        assert_eq!(snap.adjacencies[0].state, Some(6));
        assert_eq!(snap.adjacencies[0].local_ifindex, None);
    }

    #[test]
    fn a_down_bgp_session_still_produces_an_adjacency() {
        // The property the module and ADR both argue for: conditioning the edge on
        // `established(6)` would make the topology flap in step with the outage it exists to
        // explain, and the graph would lose the link exactly when suppression needed it.
        for state in 1..=6 {
            let rows = vec![walked(RoutingColumn::BgpPeerState, &[192, 0, 2, 1], state)];
            let snap = assemble(&columns(), &[], &rows, false);
            assert_eq!(snap.len(), 1, "state {state} must still yield an adjacency");
            assert_eq!(snap.adjacencies[0].state, Some(state));
        }
    }

    #[test]
    fn an_ospf_neighbour_on_an_addressless_link_names_the_local_interface() {
        // The unnumbered case Increment 1 declared out of scope. RFC 1850 requires `ospfNbrIpAddr`
        // to be populated even here, which is why OSPF is the clean answer to it.
        let rows = vec![
            walked(RoutingColumn::OspfNbrState, &[10, 0, 0, 2, 7], 8),
            walked(RoutingColumn::OspfNbrState, &[10, 0, 0, 3, 0], 8),
        ];
        let snap = assemble(&columns(), &[], &rows, false);
        assert_eq!(snap.len(), 2);
        let addressless = snap
            .adjacencies
            .iter()
            .find(|a| a.peer == ip("10.0.0.2"))
            .unwrap();
        assert_eq!(addressless.local_ifindex, Some(7));
        let numbered = snap
            .adjacencies
            .iter()
            .find(|a| a.peer == ip("10.0.0.3"))
            .unwrap();
        assert_eq!(
            numbered.local_ifindex, None,
            "a numbered link reports index 0, which is not an interface"
        );
    }

    #[test]
    fn a_down_ospf_neighbour_still_produces_an_adjacency() {
        for state in 1..=8 {
            let rows = vec![walked(
                RoutingColumn::OspfNbrState,
                &[10, 0, 0, 2, 0],
                state,
            )];
            assert_eq!(assemble(&columns(), &[], &rows, false).len(), 1);
        }
    }

    #[test]
    fn an_unresolved_neighbour_address_is_dropped() {
        // `0.0.0.0` is what an agent reports for a neighbour it has not resolved. Keeping it would
        // let every such router "peer" with every other one.
        let rows = vec![
            walked(RoutingColumn::OspfNbrState, &[0, 0, 0, 0, 0], 1),
            walked(RoutingColumn::BgpPeerState, &[0, 0, 0, 0], 1),
        ];
        assert!(assemble(&columns(), &[], &rows, false).is_empty());
    }

    #[test]
    fn a_local_host_route_becomes_a_route_adjacency() {
        let probes = probes_for("133.123.189.110");
        let rows = vec![
            probe_row(
                &probes,
                RoutingColumn::InetCidrRouteType,
                &HOST_ROUTE_INSTANCE,
                INET_CIDR_ROUTE_TYPE_LOCAL,
            ),
            probe_row(
                &probes,
                RoutingColumn::InetCidrRouteIfIndex,
                &HOST_ROUTE_INSTANCE,
                16,
            ),
        ];
        let snap = assemble(&columns(), &probes, &rows, false);
        assert_eq!(snap.len(), 1);
        assert_eq!(snap.adjacencies[0].proto, RoutingProto::Route);
        assert_eq!(snap.adjacencies[0].peer, ip("133.123.189.110"));
        assert_eq!(snap.adjacencies[0].local_ifindex, Some(16));
        assert_eq!(
            snap.adjacencies[0].state, None,
            "a route has no session to be in a state"
        );
    }

    #[test]
    fn a_remote_route_is_not_an_adjacency() {
        // `remote(4)` means the destination is forwarded to somebody else. Reading the ifIndex
        // without the type would turn every reachable address in the fleet into a link.
        let probes = probes_for("10.9.9.9");
        let rows = vec![
            probe_row(
                &probes,
                RoutingColumn::InetCidrRouteType,
                &HOST_ROUTE_INSTANCE,
                4,
            ),
            probe_row(
                &probes,
                RoutingColumn::InetCidrRouteIfIndex,
                &HOST_ROUTE_INSTANCE,
                3,
            ),
        ];
        assert!(assemble(&columns(), &probes, &rows, false).is_empty());
    }

    #[test]
    fn a_covering_prefix_is_not_a_point_to_point_answer() {
        // The agent answered with the /24 the address falls inside, because there is no host route.
        // That is the shared-subnet fact Increment 1 already derives, not the point-to-point one
        // this probe asked about — and accepting it would attach the node to everything behind a
        // summary route.
        let probes = probes_for("10.0.0.5");
        let covering = [24, 1, 0, 1, 4, 0];
        let rows = vec![
            probe_row(
                &probes,
                RoutingColumn::InetCidrRouteType,
                &covering,
                INET_CIDR_ROUTE_TYPE_LOCAL,
            ),
            probe_row(&probes, RoutingColumn::InetCidrRouteIfIndex, &covering, 3),
        ];
        assert!(assemble(&columns(), &probes, &rows, false).is_empty());
    }

    #[test]
    fn the_type_and_the_ifindex_must_describe_the_same_route() {
        // Two routes to one destination: a local one out ifIndex 16 and a remote one out ifIndex 3.
        // Matching on the destination alone would splice `local` onto ifIndex 3, or admit the
        // remote route because *some* route to this address was local.
        let probes = probes_for("198.51.100.1");
        let local = [32, 1, 0, 1, 4, 0];
        let remote = [32, 1, 0, 1, 4, 1];
        let rows = vec![
            probe_row(
                &probes,
                RoutingColumn::InetCidrRouteType,
                &local,
                INET_CIDR_ROUTE_TYPE_LOCAL,
            ),
            probe_row(&probes, RoutingColumn::InetCidrRouteIfIndex, &local, 16),
            probe_row(&probes, RoutingColumn::InetCidrRouteType, &remote, 4),
            probe_row(&probes, RoutingColumn::InetCidrRouteIfIndex, &remote, 3),
        ];
        let snap = assemble(&columns(), &probes, &rows, false);
        assert_eq!(snap.len(), 1, "one adjacency, from the local route only");
        assert_eq!(snap.adjacencies[0].local_ifindex, Some(16));
    }

    #[test]
    fn a_v6_host_route_probe_works_the_same_way() {
        let probes = probes_for("2001:db8::2");
        let instance = [128, 1, 0, 2, 16, 0];
        let rows = vec![
            probe_row(
                &probes,
                RoutingColumn::InetCidrRouteType,
                &instance,
                INET_CIDR_ROUTE_TYPE_LOCAL,
            ),
            probe_row(&probes, RoutingColumn::InetCidrRouteIfIndex, &instance, 5),
        ];
        let snap = assemble(&columns(), &probes, &rows, false);
        assert_eq!(snap.len(), 1);
        assert_eq!(snap.adjacencies[0].peer, ip("2001:db8::2"));
    }

    #[test]
    fn a_probe_that_returned_nothing_yields_nothing() {
        // The destination has no route on this device at all — the ordinary answer for 63 of 64
        // probes, and it must be silent rather than an adjacency with a placeholder in it.
        let probes = probes_for("203.0.113.7");
        assert!(assemble(&columns(), &probes, &[], false).is_empty());
    }

    #[test]
    fn a_column_the_job_did_not_declare_is_ignored_rather_than_misattributed() {
        let mut rows = vec![walked(RoutingColumn::BgpPeerState, &[192, 0, 2, 1], 6)];
        rows.push(SnmpInstanceRow {
            oid_base: "1.3.6.1.2.1.15.3.1.99".into(),
            instance: vec![192, 0, 2, 2],
            value: SnmpValue::Int(1),
        });
        let snap = assemble(&columns(), &[], &rows, false);
        assert_eq!(snap.len(), 1);
        assert_eq!(snap.adjacencies[0].peer, ip("192.0.2.1"));
    }

    #[test]
    fn a_wrongly_typed_state_is_skipped_not_coerced() {
        let rows = vec![SnmpInstanceRow {
            oid_base: base_of(RoutingColumn::BgpPeerState),
            instance: vec![192, 0, 2, 1],
            value: SnmpValue::Bytes(vec![6]),
        }];
        assert!(assemble(&columns(), &[], &rows, false).is_empty());
    }

    #[test]
    fn assembly_is_order_independent() {
        let probes = probes_for("133.123.189.110");
        let mut rows = vec![
            walked(RoutingColumn::BgpPeerState, &[192, 0, 2, 1], 6),
            walked(RoutingColumn::OspfNbrState, &[10, 0, 0, 2, 7], 8),
            probe_row(
                &probes,
                RoutingColumn::InetCidrRouteType,
                &HOST_ROUTE_INSTANCE,
                INET_CIDR_ROUTE_TYPE_LOCAL,
            ),
            probe_row(
                &probes,
                RoutingColumn::InetCidrRouteIfIndex,
                &HOST_ROUTE_INSTANCE,
                16,
            ),
        ];
        let forward = assemble(&columns(), &probes, &rows, false);
        rows.reverse();
        let reversed = assemble(&columns(), &probes, &rows, false);
        assert_eq!(forward.len(), 3);
        assert_eq!(forward, reversed);
        assert_eq!(forward.content_key(), reversed.content_key());
    }

    #[test]
    fn the_walk_truncation_flag_is_carried_through() {
        let snap = assemble(&columns(), &[], &[], true);
        assert!(snap.is_empty());
        assert!(
            snap.truncated,
            "an empty answer from a walk that ran out of budget is not a complete answer"
        );
    }
}
