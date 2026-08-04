// SPDX-License-Identifier: AGPL-3.0-only
//! Derive the connectivity graph from what devices reported (ADR-043).
//!
//! **Pure**: no database, no clock, no bus. That is not tidiness — this is the one place in the
//! feature where a mistake is silent and expensive. A wrong edge here becomes a wrong parent in
//! Increment 2, and a wrong parent suppresses a real outage. Everything it decides is therefore
//! decided from arguments and testable without a device.
//!
//! Two rules carry the design:
//!
//! * **Two nodes with an address in the same prefix are adjacent.** That is a fact, not an
//!   inference: having a foot in a network is what the address means. It is also why the routing
//!   table is not walked — `ipAddrTable` answers the same question in tens of rows.
//!
//! * **A segment with many members never becomes a clique.** Fifty servers on a `/24` are not
//!   fifty-times-forty-nine links; they are fifty links to whatever routes for them. Which member
//!   routes is read off the observation ([`L3Snapshot::subnets`] — a box with a foot in two
//!   networks forwards between them), never guessed, and when no member qualifies the segment
//!   produces **no** edges and is counted instead. See [`derive_links`] for why the answer is a
//!   cross product rather than a star.

use std::collections::{BTreeMap, BTreeSet};
use std::net::IpAddr;
use yagra_common::{
    DerivedLink, L3Snapshot, LinkOverride, LinkOverrideAction, LinkSource, NeighborProto,
    NeighborSet, NodeId, SubnetKey, TopologyLinkSummary, MAX_LINKS_PER_NODE,
};

/// Cap on how many members of one subnet are considered. A `/16` with every host answering SNMP
/// would otherwise make the containment rule quadratic in the router count.
pub const MAX_MEMBERS_PER_SUBNET: usize = 1024;

/// Cap on how many members of one segment are treated as transit nodes. Beyond a handful, the
/// "which of these routes" question has stopped having a useful answer.
pub const MAX_ROUTERS_PER_SEGMENT: usize = 8;

/// Everything the derivation reads.
#[derive(Debug, Clone, Copy)]
pub struct DeriveInput<'a> {
    /// Inventory: every node and its primary management address. This is what lets a CDP/LLDP row
    /// naming an address be matched to a node even when that node's own L3 walk has not run.
    pub nodes: &'a [(NodeId, IpAddr)],
    /// Each node's most recent interface-address observation.
    pub l3: &'a [(NodeId, L3Snapshot)],
    /// Each node's most recent CDP/LLDP observation.
    pub neighbors: &'a [(NodeId, NeighborSet)],
    /// Operator decisions. Empty in Increment 1.
    pub overrides: &'a [LinkOverride],
}

/// The derived graph plus what the run declined to turn into an edge.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeriveOutput {
    /// The links, ordered by their canonical key so the result is stable across runs.
    pub links: Vec<DerivedLink>,
    /// Counters for everything that did not become a link.
    pub summary: TopologyLinkSummary,
}

/// Derive every link from one snapshot of the fleet's observations.
///
/// Deterministic: the output depends only on the *content* of the input, never on its order. Two
/// cores with the same observations produce byte-identical graphs, and a map redrawn after a poll
/// that changed nothing does not move.
#[must_use]
pub fn derive_links(input: DeriveInput<'_>) -> DeriveOutput {
    let mut summary = TopologyLinkSummary::default();

    // ── Who owns which address ───────────────────────────────────────────────
    // Both the inventory address and every observed interface address, because a CDP row may name
    // either. An address claimed by more than one node identifies neither: it is a VRRP/HSRP
    // virtual IP or a duplicate-address misconfiguration, and picking a claimant would be the guess
    // 決定 2 forbids.
    let mut claimants: BTreeMap<IpAddr, BTreeSet<NodeId>> = BTreeMap::new();
    // Addresses that could place a node on a segment if they were not contested. Used only to keep
    // the `duplicate_addresses` counter meaningful — see below.
    let mut edge_eligible: BTreeSet<IpAddr> = BTreeSet::new();
    for (id, addr) in input.nodes {
        claimants.entry(*addr).or_default().insert(*id);
    }
    for (id, snap) in input.l3 {
        if snap.truncated {
            summary.truncated_nodes += 1;
        }
        for a in &snap.addresses {
            claimants.entry(a.ip).or_default().insert(*id);
            if a.can_form_subnet_edge() {
                edge_eligible.insert(a.ip);
            }
        }
    }
    let contested: BTreeSet<IpAddr> = claimants
        .iter()
        .filter(|(_, owners)| owners.len() > 1)
        .map(|(ip, _)| *ip)
        .collect();
    // Counted only when the contested address could otherwise have formed an edge.
    //
    // Counting every contested address makes the number useless in practice, which real data showed
    // immediately: every URL and DNS monitor carries `0.0.0.0` as a placeholder address, and every
    // device reports `127.0.0.1`. Both are contested on any real fleet and neither can ever form an
    // edge, so the raw count reads ≥1 on every deployment forever — and a diagnostic that always
    // fires is one an operator learns to ignore. What this counter is *for* is the VRRP/HSRP virtual
    // IP and the genuine duplicate-address misconfiguration, and those are contested addresses that
    // would otherwise have placed a node on a segment.
    summary.duplicate_addresses =
        u32::try_from(contested.intersection(&edge_eligible).count()).unwrap_or(u32::MAX);
    let owner: BTreeMap<IpAddr, NodeId> = claimants
        .iter()
        .filter(|(_, owners)| owners.len() == 1)
        .filter_map(|(ip, owners)| owners.iter().next().map(|n| (*ip, *n)))
        .collect();

    // ── Subnet membership, and how many networks each node has a foot in ─────
    let mut members: BTreeMap<SubnetKey, BTreeSet<NodeId>> = BTreeMap::new();
    let mut network_count: BTreeMap<NodeId, usize> = BTreeMap::new();
    for (id, snap) in input.l3 {
        let mut own: BTreeSet<SubnetKey> = BTreeSet::new();
        for a in &snap.addresses {
            // A contested address still belongs to the device, but it cannot place it on a segment.
            if contested.contains(&a.ip) {
                continue;
            }
            if let Some(key) = a.subnet() {
                own.insert(key);
                members.entry(key).or_default().insert(*id);
            }
        }
        network_count.insert(*id, own.len());
    }

    // Same network address under two prefix lengths is a misconfiguration, not an adjacency (D9).
    // Counted rather than reconciled, so it is visible instead of quietly halving a segment.
    let mut by_network: BTreeMap<IpAddr, BTreeSet<u8>> = BTreeMap::new();
    for key in members.keys() {
        by_network
            .entry(key.network)
            .or_default()
            .insert(key.prefix_len);
    }
    summary.subnet_prefix_mismatch =
        u32::try_from(by_network.values().filter(|lens| lens.len() > 1).count())
            .unwrap_or(u32::MAX);

    // ── Edges ────────────────────────────────────────────────────────────────
    let mut links: BTreeMap<String, DerivedLink> = BTreeMap::new();
    let add = |link: DerivedLink, links: &mut BTreeMap<String, DerivedLink>| {
        links
            .entry(link.link_key())
            .and_modify(|existing| existing.merge(&link))
            .or_insert(link);
    };

    for (key, m) in &members {
        let mut m: Vec<NodeId> = m.iter().copied().collect(); // BTreeSet ⇒ already sorted
        if m.len() > MAX_MEMBERS_PER_SUBNET {
            m.truncate(MAX_MEMBERS_PER_SUBNET);
            summary.oversized_segments += 1;
        }
        match m.len() {
            0 | 1 => {}
            2 => {
                // A point-to-point: /30, /31, /64. The highest-confidence edge in the system —
                // there is nothing to decide, the two ends are the only members.
                let mut link = DerivedLink::new(m[0], m[1], LinkSource::L3Subnet);
                link.subnet = Some(key.to_string());
                add(link, &mut links);
            }
            _ => {
                // A shared segment. The transit candidates are the members with a foot in the most
                // networks — a firewall sees five or ten, a dual-homed server sees exactly two, a
                // plain server sees one — and a segment where the maximum is one has no observable
                // router in it at all.
                let max_networks = m
                    .iter()
                    .map(|n| network_count.get(n).copied().unwrap_or(0))
                    .max()
                    .unwrap_or(0);
                if max_networks < 2 {
                    summary.oversized_segments += 1;
                    continue;
                }
                let mut routers: Vec<NodeId> = m
                    .iter()
                    .copied()
                    .filter(|n| network_count.get(n).copied().unwrap_or(0) == max_networks)
                    .collect();
                routers.truncate(MAX_ROUTERS_PER_SEGMENT);

                // Every non-router attaches to **every** router, not to one elected anchor.
                //
                // A star to a single router would mean that when that router dies, each server's
                // only modelled path is gone and all of them suppress — even though the second HSRP
                // router is alive and they are perfectly reachable. That is a false suppression: the
                // exact failure class ADR-038 named and 決定 2 accepted risk on. The cross product
                // gives every server *both* routers as parents, which is what makes
                // `Topology::is_suppressed` ("all parents down") correct rather than merely
                // reachable.
                for n in &m {
                    if routers.contains(n) {
                        continue;
                    }
                    for r in &routers {
                        let mut link = DerivedLink::new(*n, *r, LinkSource::L3Subnet);
                        link.subnet = Some(key.to_string());
                        add(link, &mut links);
                    }
                }
                // The routers are on the segment together too.
                for (i, a) in routers.iter().enumerate() {
                    for b in &routers[i + 1..] {
                        let mut link = DerivedLink::new(*a, *b, LinkSource::L3Subnet);
                        link.subnet = Some(key.to_string());
                        add(link, &mut links);
                    }
                }
            }
        }
    }

    // ── L2 adjacency, matched to inventory by management address ─────────────
    //
    // The matching key is the IP, never the chassis id. A chassis id is device-supplied text with no
    // relationship to anything in the inventory, so joining on it would mean string-matching a MAC
    // or a hostname against a node name and hoping.
    for (id, set) in input.neighbors {
        for nb in &set.neighbors {
            let source = match nb.proto {
                NeighborProto::Lldp => LinkSource::Lldp,
                NeighborProto::Cdp => LinkSource::Cdp,
            };
            let unmatched = |s: &mut TopologyLinkSummary| match source {
                LinkSource::Lldp => s.unmatched_lldp_rows += 1,
                _ => s.unmatched_cdp_rows += 1,
            };
            let Some(ip) = nb
                .remote_mgmt_addr
                .as_deref()
                .and_then(|a| a.parse::<IpAddr>().ok())
            else {
                unmatched(&mut summary);
                continue;
            };
            if contested.contains(&ip) {
                summary.ambiguous_mgmt_addrs += 1;
                continue;
            }
            let Some(peer) = owner.get(&ip).copied() else {
                unmatched(&mut summary);
                continue;
            };
            // A device that advertises its own address as a peer's is describing a loop, not a link.
            if peer == *id {
                continue;
            }
            let mut link = DerivedLink::new(*id, peer, source);
            // `local_port` describes *this* node's side, so it has to follow the endpoint through
            // canonicalization rather than always landing on `a`.
            if link.is_a(*id) {
                link.a_if_name = Some(nb.local_port.clone());
                link.a_ifindex = nb.local_ifindex;
            } else {
                link.b_if_name = Some(nb.local_port.clone());
                link.b_ifindex = nb.local_ifindex;
            }
            add(link, &mut links);
        }
    }

    // ── Manual decisions win ─────────────────────────────────────────────────
    apply_overrides(&mut links, input.overrides);

    // ── Caps ─────────────────────────────────────────────────────────────────
    // Applied over the canonically ordered set, so which links survive is deterministic rather than
    // a function of which subnet happened to be visited first.
    let mut per_node: BTreeMap<NodeId, usize> = BTreeMap::new();
    let mut out: Vec<DerivedLink> = Vec::with_capacity(links.len());
    for link in links.into_values() {
        let a = per_node.get(&link.a_node).copied().unwrap_or(0);
        let b = per_node.get(&link.b_node).copied().unwrap_or(0);
        if a >= MAX_LINKS_PER_NODE || b >= MAX_LINKS_PER_NODE {
            summary.oversized_segments += 1;
            continue;
        }
        *per_node.entry(link.a_node).or_insert(0) += 1;
        *per_node.entry(link.b_node).or_insert(0) += 1;
        match LinkSource::best(&link.sources) {
            Some(LinkSource::Lldp) => summary.lldp_links += 1,
            Some(LinkSource::Cdp) => summary.cdp_links += 1,
            Some(LinkSource::L3Subnet) => summary.l3_links += 1,
            _ => {}
        }
        out.push(link);
    }

    DeriveOutput {
        links: out,
        summary,
    }
}

/// Apply operator decisions to a derived set. **The only place "manual always wins" is expressed.**
///
/// `Hide` removes the link whatever produced it; `Pin` inserts one that the observations did not;
/// `Direction` marks which endpoint is upstream so the projection stops deriving that from
/// distance. A pinned link is re-emitted by every run, which is why the ordinary staleness prune
/// never has to know about it.
///
/// **Order matters, and it is fixed here rather than left to the caller.** `Hide` is applied last,
/// so hiding a link an operator also pinned removes it — the two decisions contradict, and the
/// removing one is the safe reading: an edge that should not exist can suppress a real outage,
/// whereas a missing edge only fails to suppress a redundant one.
fn apply_overrides(links: &mut BTreeMap<String, DerivedLink>, overrides: &[LinkOverride]) {
    for ov in overrides
        .iter()
        .filter(|o| o.action != LinkOverrideAction::Hide)
    {
        let mut manual = DerivedLink::new(ov.a_node, ov.b_node, LinkSource::Manual);
        manual.forced_parent = ov.forced_parent();
        let key = manual.link_key();
        match ov.action {
            LinkOverrideAction::Pin => {
                links
                    .entry(key)
                    .and_modify(|existing| existing.merge(&manual))
                    .or_insert(manual);
            }
            // A direction annotates a link; it does not assert one. An operator who wants both says
            // so with both rows, which is why the table lets them coexist. Applied to a link that
            // was not derived, it does nothing — and that is visible, because the link is not on
            // the map either.
            LinkOverrideAction::Direction => {
                if let Some(existing) = links.get_mut(&key) {
                    existing.forced_parent = manual.forced_parent;
                }
            }
            LinkOverrideAction::Hide => unreachable!("filtered out above"),
        }
    }
    for ov in overrides
        .iter()
        .filter(|o| o.action == LinkOverrideAction::Hide)
    {
        links.remove(&DerivedLink::new(ov.a_node, ov.b_node, LinkSource::Manual).link_key());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use yagra_common::{L3Address, Neighbor};

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    /// `n` sequential node ids, so a test's expectations do not depend on random UUID ordering.
    fn ids(n: usize) -> Vec<NodeId> {
        (1..=n)
            .map(|i| NodeId(uuid::Uuid::from_u128(i as u128)))
            .collect()
    }

    fn snap(addrs: &[(&str, u8)]) -> L3Snapshot {
        L3Snapshot::new(
            addrs
                .iter()
                .enumerate()
                .map(|(i, (a, p))| {
                    #[allow(clippy::cast_possible_truncation)]
                    L3Address::new(i as u32 + 1, ip(a), *p)
                })
                .collect(),
        )
    }

    fn derive(l3: &[(NodeId, L3Snapshot)]) -> DeriveOutput {
        derive_links(DeriveInput {
            nodes: &[],
            l3,
            neighbors: &[],
            overrides: &[],
        })
    }

    fn pair(link: &DerivedLink) -> (NodeId, NodeId) {
        (link.a_node, link.b_node)
    }

    #[test]
    fn a_point_to_point_30_yields_exactly_one_link() {
        let n = ids(2);
        let out = derive(&[
            (n[0], snap(&[("10.0.0.1", 30)])),
            (n[1], snap(&[("10.0.0.2", 30)])),
        ]);
        assert_eq!(out.links.len(), 1);
        assert_eq!(pair(&out.links[0]), (n[0], n[1]));
        assert_eq!(out.links[0].sources, vec![LinkSource::L3Subnet]);
        assert_eq!(out.links[0].subnet.as_deref(), Some("10.0.0.0/30"));
        assert_eq!(out.summary.l3_links, 1);
    }

    #[test]
    fn a_31_is_a_real_point_to_point() {
        // RFC 3021. Treating /31 as a host route would delete most modern router-to-router links.
        let n = ids(2);
        let out = derive(&[
            (n[0], snap(&[("10.0.0.0", 31)])),
            (n[1], snap(&[("10.0.0.1", 31)])),
        ]);
        assert_eq!(out.links.len(), 1);
        assert_eq!(out.links[0].subnet.as_deref(), Some("10.0.0.0/31"));
    }

    #[test]
    fn a_flat_segment_with_one_router_yields_a_star_not_a_clique() {
        // 50 servers + 1 router on a /24. The clique would be 1275 links; the answer is 50.
        let n = ids(51);
        let mut l3: Vec<(NodeId, L3Snapshot)> = (0..50)
            .map(|i| (n[i], snap(&[(&format!("10.0.0.{}", i + 10), 24)])))
            .collect();
        // The router has a foot in a second network, which is what identifies it.
        l3.push((n[50], snap(&[("10.0.0.1", 24), ("10.0.1.1", 24)])));

        let out = derive(&l3);
        assert_eq!(
            out.links.len(),
            50,
            "a clique here would be 1275 links and an unusable map"
        );
        assert!(
            out.links
                .iter()
                .all(|l| l.a_node == n[50] || l.b_node == n[50]),
            "every link must touch the router"
        );
    }

    #[test]
    fn a_segment_with_two_hsrp_routers_gives_every_server_two_parents() {
        // **The test that makes multi-parent suppression live.** 50 servers + 2 routers ⇒ each
        // server attaches to both routers (100 links) plus the router-to-router link = 101. A star
        // to one elected router would be 51 links and would suppress every server when that one
        // router died, while the other was still carrying their traffic.
        let n = ids(52);
        let mut l3: Vec<(NodeId, L3Snapshot)> = (0..50)
            .map(|i| (n[i], snap(&[(&format!("10.0.0.{}", i + 10), 24)])))
            .collect();
        l3.push((n[50], snap(&[("10.0.0.1", 24), ("10.0.1.1", 24)])));
        l3.push((n[51], snap(&[("10.0.0.2", 24), ("10.0.1.2", 24)])));

        let out = derive(&l3);
        assert_eq!(out.links.len(), 101);
        for (i, server) in n.iter().take(50).enumerate() {
            let parents = out
                .links
                .iter()
                .filter(|l| l.a_node == *server || l.b_node == *server)
                .count();
            assert_eq!(parents, 2, "server {i} must see both routers");
        }
    }

    #[test]
    fn a_segment_with_no_observable_router_produces_nothing_and_is_counted() {
        // Three servers on a /24 whose gateway is not monitored. Guessing which of them routes
        // would invent a topology; saying nothing and counting it is the honest answer, and it is
        // what Increment 3's ARP discovery goes on to fill in.
        let n = ids(3);
        let out = derive(&[
            (n[0], snap(&[("10.0.0.10", 24)])),
            (n[1], snap(&[("10.0.0.11", 24)])),
            (n[2], snap(&[("10.0.0.12", 24)])),
        ]);
        assert!(out.links.is_empty());
        assert_eq!(out.summary.oversized_segments, 1);
    }

    #[test]
    fn host_routes_never_join_a_subnet() {
        // The lab's real PPPoE address, plus another unrelated /32. They share no subnet with
        // anything — including each other.
        let n = ids(2);
        let out = derive(&[
            (n[0], snap(&[("133.123.189.109", 32)])),
            (n[1], snap(&[("203.0.113.7", 32)])),
        ]);
        assert!(out.links.is_empty());
    }

    /// Found on the real fleet, not in a fixture: every URL and DNS monitor carries `0.0.0.0` as a
    /// placeholder address and every device reports `127.0.0.1`, so both are contested on any real
    /// deployment. Counting them would pin `duplicate_addresses` at ≥1 forever and teach the
    /// operator to ignore the one number that is supposed to surface a VIP or a duplicate-IP
    /// misconfiguration.
    #[test]
    fn an_address_that_could_never_form_an_edge_is_not_counted_as_a_duplicate() {
        let n = ids(4);
        let out = derive_links(DeriveInput {
            // Three URL/DNS monitors sharing the placeholder address, as the real inventory has.
            nodes: &[
                (n[0], ip("0.0.0.0")),
                (n[1], ip("0.0.0.0")),
                (n[2], ip("0.0.0.0")),
            ],
            // And a device reporting the loopback every device reports, contested with a node whose
            // inventory address is also 127.0.0.1.
            l3: &[(n[3], snap(&[("127.0.0.1", 8), ("10.0.0.1", 24)]))],
            neighbors: &[],
            overrides: &[],
        });
        assert_eq!(
            out.summary.duplicate_addresses, 0,
            "neither 0.0.0.0 nor 127.0.0.1 could have formed an edge, so neither is a finding"
        );

        // A real VIP still counts: two routers sharing a routable address on a real segment.
        let m = ids(2);
        let out = derive_links(DeriveInput {
            nodes: &[],
            l3: &[
                (m[0], snap(&[("10.0.0.1", 24), ("10.0.1.1", 24)])),
                (m[1], snap(&[("10.0.0.1", 24), ("10.0.2.1", 24)])),
            ],
            neighbors: &[],
            overrides: &[],
        });
        assert_eq!(out.summary.duplicate_addresses, 1);
    }

    #[test]
    fn node_local_addresses_do_not_wire_the_whole_fleet_together() {
        // Every device has 127.0.0.1 and an fe80:: address. If either could form an edge, the
        // entire inventory would collapse into one segment.
        let n = ids(3);
        let out = derive(&[
            (n[0], snap(&[("127.0.0.1", 8), ("169.254.1.1", 16)])),
            (n[1], snap(&[("127.0.0.1", 8), ("169.254.1.2", 16)])),
            (n[2], snap(&[("fe80::1", 64)])),
        ]);
        assert!(out.links.is_empty());
    }

    #[test]
    fn mismatched_prefix_lengths_do_not_join_but_are_counted() {
        let n = ids(2);
        let out = derive(&[
            (n[0], snap(&[("10.0.0.1", 24)])),
            (n[1], snap(&[("10.0.0.2", 16)])),
        ]);
        assert!(out.links.is_empty());
        assert_eq!(out.summary.subnet_prefix_mismatch, 1);
    }

    #[test]
    fn a_duplicate_address_anchors_nothing_but_does_not_exile_its_node() {
        // Two routers sharing a VRRP VIP, each with its own real address on the same segment plus a
        // second network. The VIP must not place either of them, but their real addresses must.
        let n = ids(3);
        let out = derive(&[
            (
                n[0],
                snap(&[("10.0.0.1", 24), ("10.0.0.254", 24), ("10.0.1.1", 24)]),
            ),
            (
                n[1],
                snap(&[("10.0.0.2", 24), ("10.0.0.254", 24), ("10.0.2.1", 24)]),
            ),
            (n[2], snap(&[("10.0.0.50", 24)])),
        ]);
        assert_eq!(out.summary.duplicate_addresses, 1, "the VIP is contested");
        // The server still reaches both routers through their real addresses.
        let server_links = out
            .links
            .iter()
            .filter(|l| l.a_node == n[2] || l.b_node == n[2])
            .count();
        assert_eq!(server_links, 2);
    }

    #[test]
    fn an_lldp_row_that_matches_no_node_is_counted_not_dropped() {
        // The instrument for the half of ADR-043 the lab cannot verify: when a switch arrives, this
        // number is what says whether the management-address decode is working.
        let n = ids(1);
        let mut nb = Neighbor::new(
            NeighborProto::Lldp,
            "Gi0/1",
            "aa:bb:cc:dd:ee:01",
            "Gi1/0/24",
        );
        nb.remote_mgmt_addr = Some("198.51.100.7".into());
        let out = derive_links(DeriveInput {
            nodes: &[],
            l3: &[],
            neighbors: &[(n[0], NeighborSet::new(vec![nb]))],
            overrides: &[],
        });
        assert!(out.links.is_empty());
        assert_eq!(out.summary.unmatched_lldp_rows, 1);
    }

    #[test]
    fn an_adjacency_matched_by_management_address_becomes_a_link() {
        let n = ids(2);
        let mut nb = Neighbor::new(
            NeighborProto::Lldp,
            "Gi0/1",
            "aa:bb:cc:dd:ee:01",
            "Gi1/0/24",
        );
        nb.remote_mgmt_addr = Some("10.0.0.2".into());
        let out = derive_links(DeriveInput {
            nodes: &[(n[0], ip("10.0.0.1")), (n[1], ip("10.0.0.2"))],
            l3: &[],
            neighbors: &[(n[0], NeighborSet::new(vec![nb]))],
            overrides: &[],
        });
        assert_eq!(out.links.len(), 1);
        assert_eq!(out.links[0].sources, vec![LinkSource::Lldp]);
        assert_eq!(out.summary.lldp_links, 1);
        // The reporting node's own port name lands on the reporting node's endpoint.
        let l = &out.links[0];
        let side = if l.a_node == n[0] {
            &l.a_if_name
        } else {
            &l.b_if_name
        };
        assert_eq!(side.as_deref(), Some("Gi0/1"));
    }

    #[test]
    fn an_ambiguous_management_address_produces_no_link() {
        // Two nodes claim the address the peer advertised. Choosing one would be a guess.
        let n = ids(3);
        let mut nb = Neighbor::new(NeighborProto::Cdp, "Gi0/1", "peer", "Fa0/1");
        nb.remote_mgmt_addr = Some("10.0.0.9".into());
        let out = derive_links(DeriveInput {
            nodes: &[
                (n[0], ip("10.0.0.1")),
                (n[1], ip("10.0.0.9")),
                (n[2], ip("10.0.0.9")),
            ],
            l3: &[],
            neighbors: &[(n[0], NeighborSet::new(vec![nb]))],
            overrides: &[],
        });
        assert!(out.links.is_empty());
        assert_eq!(out.summary.ambiguous_mgmt_addrs, 1);
    }

    #[test]
    fn cdp_and_l3_seeing_the_same_pair_produce_one_link_with_two_sources() {
        let n = ids(2);
        let mut nb = Neighbor::new(NeighborProto::Cdp, "Gi0/1", "peer", "Fa0/1");
        nb.remote_mgmt_addr = Some("10.0.0.2".into());
        nb.local_ifindex = Some(1);
        let out = derive_links(DeriveInput {
            nodes: &[(n[0], ip("10.0.0.1")), (n[1], ip("10.0.0.2"))],
            l3: &[
                (n[0], snap(&[("10.0.0.1", 24)])),
                (n[1], snap(&[("10.0.0.2", 24)])),
            ],
            neighbors: &[(n[0], NeighborSet::new(vec![nb]))],
            overrides: &[],
        });
        assert_eq!(out.links.len(), 1, "one physical link, not two rows");
        assert_eq!(
            out.links[0].sources,
            vec![LinkSource::Cdp, LinkSource::L3Subnet]
        );
        // The representative source is the stronger one, and the subnet evidence is still carried.
        assert_eq!(
            LinkSource::best(&out.links[0].sources),
            Some(LinkSource::Cdp)
        );
        assert_eq!(out.links[0].subnet.as_deref(), Some("10.0.0.0/24"));
        assert_eq!(out.summary.cdp_links, 1);
        assert_eq!(
            out.summary.l3_links, 0,
            "it is counted once, by its best source"
        );
    }

    #[test]
    fn derivation_is_deterministic_under_input_permutation() {
        let n = ids(5);
        let mut l3 = vec![
            (n[0], snap(&[("10.0.0.1", 24), ("10.0.1.1", 24)])),
            (n[1], snap(&[("10.0.0.2", 24)])),
            (n[2], snap(&[("10.0.0.3", 24)])),
            (n[3], snap(&[("10.0.1.2", 24)])),
            (n[4], snap(&[("10.0.1.3", 24)])),
        ];
        let forward = derive(&l3);
        l3.reverse();
        let reversed = derive(&l3);
        assert_eq!(forward, reversed);
        // And running it twice on the same input changes nothing.
        assert_eq!(forward, derive(&l3));
    }

    #[test]
    fn the_lab_capture_produces_exactly_one_link() {
        // The only real edge this lab can produce: the Huawei USG's LAN interface and the test
        // server share 192.168.1.0/24. Its `Dialer1` /32 must contribute nothing.
        let n = ids(2);
        let usg = snap(&[("192.168.1.1", 24), ("133.123.189.109", 32)]);
        let server = snap(&[("192.168.1.2", 24)]);
        let out = derive(&[(n[0], usg), (n[1], server)]);
        assert_eq!(out.links.len(), 1);
        assert_eq!(out.links[0].subnet.as_deref(), Some("192.168.1.0/24"));
        assert_eq!(out.links[0].sources, vec![LinkSource::L3Subnet]);
    }

    #[test]
    fn a_hidden_link_is_removed_and_a_pinned_one_survives_an_empty_observation() {
        let n = ids(3);
        let l3 = [
            (n[0], snap(&[("10.0.0.1", 30)])),
            (n[1], snap(&[("10.0.0.2", 30)])),
        ];
        let hidden = derive_links(DeriveInput {
            nodes: &[],
            l3: &l3,
            neighbors: &[],
            overrides: &[LinkOverride {
                a_node: n[1],
                b_node: n[0], // reversed on purpose: the override is canonicalized like any link
                action: LinkOverrideAction::Hide,
                direction: None,
            }],
        });
        assert!(hidden.links.is_empty(), "manual always wins");

        let pinned = derive_links(DeriveInput {
            nodes: &[],
            l3: &[],
            neighbors: &[],
            overrides: &[LinkOverride {
                a_node: n[0],
                b_node: n[2],
                action: LinkOverrideAction::Pin,
                direction: None,
            }],
        });
        assert_eq!(pinned.links.len(), 1);
        assert_eq!(pinned.links[0].sources, vec![LinkSource::Manual]);
    }

    #[test]
    fn a_node_advertising_its_own_address_does_not_link_to_itself() {
        let n = ids(1);
        let mut nb = Neighbor::new(NeighborProto::Cdp, "Gi0/1", "self", "Gi0/1");
        nb.remote_mgmt_addr = Some("10.0.0.1".into());
        let out = derive_links(DeriveInput {
            nodes: &[(n[0], ip("10.0.0.1"))],
            l3: &[],
            neighbors: &[(n[0], NeighborSet::new(vec![nb]))],
            overrides: &[],
        });
        assert!(out.links.is_empty());
    }

    #[test]
    fn a_truncated_observation_is_reported() {
        let n = ids(1);
        let mut s = snap(&[("10.0.0.1", 24)]);
        s.truncated = true;
        let out = derive(&[(n[0], s)]);
        assert_eq!(out.summary.truncated_nodes, 1);
    }

    #[test]
    fn a_v6_segment_derives_the_same_way_a_v4_one_does() {
        let n = ids(3);
        let out = derive(&[
            (n[0], snap(&[("2001:db8::1", 64), ("2001:db8:1::1", 64)])),
            (n[1], snap(&[("2001:db8::2", 64)])),
            (n[2], snap(&[("2001:db8::3", 64)])),
        ]);
        assert_eq!(out.links.len(), 2);
        assert!(out
            .links
            .iter()
            .all(|l| l.subnet.as_deref() == Some("2001:db8::/64")));
    }
}
