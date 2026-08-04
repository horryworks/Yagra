// SPDX-License-Identifier: AGPL-3.0-only
//! Routing-protocol adjacency: the links that share no subnet (ADR-043 Increment 4).
//!
//! Increment 1 derives a link from two nodes having an address in the same prefix. Three real link
//! classes never satisfy that, and this module collects the evidence for them:
//!
//! * **Point-to-point host routes.** A PPPoE `Dialer` or an `ip unnumbered` interface carries a
//!   `/32`, which shares a subnet with nothing — including the peer's `/32`. Increment 1 stores
//!   those addresses without joining them ([`crate::L3Address::is_host_route`]) precisely so this
//!   increment can resolve them here.
//! * **OSPF neighbours.** `ospfNbrTable` is indexed by the neighbour's own IP, and RFC 1850 requires
//!   that address to be present *even on addressless links* — which is why OSPF, not the routing
//!   table, is the clean answer to the unnumbered case Increment 1 declared out of scope.
//! * **BGP peers.** `bgpPeerTable` is indexed by the peer address.
//!
//! Three rules carry the design, and each is here rather than at a call site:
//!
//! 1. **The routing table is never walked as a table.** A core router's `inetCidrRouteTable` runs to
//!    hundreds of thousands of rows, and a bounded walk of it returns the numerically-first routes,
//!    which is worse than useless. Instead the table's index is exploited: it begins
//!    `(destType, dest, …)`, so a subtree walk rooted at `<column>.<destType>.<len>.<octets>`
//!    returns the routes to **exactly that destination** and stops. See [`route_probe_oid`]. The
//!    scale problem is designed out the same way [`crate::l3`] designed it out, which is the
//!    argument for this shape over a filtered walk.
//!
//! 2. **A session's state never conditions its link.** A down BGP session is still a link — it is
//!    usually a link with a *fault*, which is the thing the operator wants explained. Conditioning
//!    the edge on `established(6)` would make the topology flap in step with the outage it exists to
//!    describe, and the graph would lose the edge exactly when suppression needed it. The state is
//!    recorded ([`RoutingAdjacency::state`]) and deliberately excluded from
//!    [`RoutingSnapshot::content_key`].
//!
//! 3. **`bgpPeerState` is walked a second time, on purpose.** [`crate::CollectionKind`]'s `T_BGP`
//!    template already reads the same OID — through the *numeric* walker, which folds the multi-part
//!    index down to a synthetic ifIndex and so destroys `bgpPeerRemoteAddr`, which **is** the index.
//!    One consumer wants a TSDB gauge per peer and the other wants the peer's identity; they cannot
//!    share a walker because one of them needs the very thing the other discards. This duplication
//!    is load-bearing, not a refactoring target.
//!
//! BGP4-MIB is IPv4-only. `bgp4V2PeerTable` (RFC 9086) would cover IPv6 peers, but vendor
//! implementations of it disagree, so **IPv6 BGP peers are out of scope and declared so** rather
//! than being silently answered with the v4 half. OSPF and the route probe are both family-agnostic
//! on the wire; `ospfNbrTable` is OSPFv2 and therefore v4, while the route probe handles both.

use crate::topology::LinkSource;
use serde::{Deserialize, Serialize};
use std::net::{IpAddr, Ipv4Addr};

/// Stable TSDB metric: how many routing adjacencies the node currently reports. Node-level and
/// bounded by [`MAX_ROUTING_ADJACENCIES_PER_NODE`], so it is a safe series. The peer addresses
/// themselves never become labels — an IP in a label is the cardinality explosion CLAUDE.md §7.1
/// warns about.
pub const METRIC_SNMP_ROUTING_ADJACENCY_COUNT: &str = "snmp_routing_adjacency_count";

/// Cap on adjacencies recorded per node, applied after sorting so truncation is deterministic. A
/// route reflector can hold hundreds of iBGP sessions; without a cap the stored document and the
/// content key grow with the peering mesh.
pub const MAX_ROUTING_ADJACENCIES_PER_NODE: usize = 256;

/// Cap on how many host addresses one node is asked to probe for per cycle.
///
/// Each target costs two subtree walks (route type and route ifIndex), so this is 128 round trips
/// at the ceiling — acceptable at the hour-plus cadence this check runs on, and the reason the
/// probe list is *targeted* rather than a walk in the first place.
pub const MAX_ROUTE_PROBES_PER_NODE: usize = 64;

/// Row budget for the adjacency walk (`bgpPeerState` + `ospfNbrState`), enforced while paging.
pub const MAX_ROUTING_WALK_ROWS: usize = 2048;

/// Row budget for the whole set of route probes on one node.
///
/// A single destination has a handful of routes at most (one per policy/next-hop pair), so this is
/// [`MAX_ROUTE_PROBES_PER_NODE`] × 2 columns × 4 routes. Kept separate from
/// [`MAX_ROUTING_WALK_ROWS`] rather than sharing one budget: a shared budget means a router with a
/// large peering mesh silently starves its own route probes, and which half loses would depend on
/// the order the bases happen to be listed in.
pub const MAX_ROUTE_PROBE_ROWS: usize = 512;

/// `bgpPeerState`, BGP4-MIB. Index: `bgpPeerRemoteAddr` (IPv4, four sub-identifiers).
const OID_BGP_PEER_STATE: &str = "1.3.6.1.2.1.15.3.1.2";
/// `ospfNbrState`, OSPF-MIB. Index: `(ospfNbrIpAddr, ospfNbrAddressLessIndex)`.
const OID_OSPF_NBR_STATE: &str = "1.3.6.1.2.1.14.10.1.6";
/// `inetCidrRouteIfIndex`, IP-FORWARD-MIB (RFC 4292).
const OID_INET_CIDR_ROUTE_IF_INDEX: &str = "1.3.6.1.2.1.4.24.7.1.7";
/// `inetCidrRouteType`, IP-FORWARD-MIB (RFC 4292).
const OID_INET_CIDR_ROUTE_TYPE: &str = "1.3.6.1.2.1.4.24.7.1.8";

/// `inetCidrRouteType = local(3)` — the destination is reached over a local interface.
///
/// ⚠️ Not `direct(3)`: that is RFC 1213's `ipRouteType` spelling. RFC 4292 renamed the enumeration
/// to `other(1) / reject(2) / local(3) / remote(4) / blackhole(5)`, and `remote(4)` is the one that
/// means "forwarded to somebody else" — a routing decision, not an adjacency.
pub const INET_CIDR_ROUTE_TYPE_LOCAL: i64 = 3;

/// Which routing column an OID base carries. Typed rather than matched on the OID string, so the
/// poller's assembly is a `match` the compiler checks — the same contract [`crate::L3Column`] has.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoutingColumn {
    /// `bgpPeerState` — walked; the peer address is the row index.
    BgpPeerState,
    /// `ospfNbrState` — walked; the neighbour address is the first half of the row index.
    OspfNbrState,
    /// `inetCidrRouteType` — probed per destination, never walked as a table.
    InetCidrRouteType,
    /// `inetCidrRouteIfIndex` — probed per destination, never walked as a table.
    InetCidrRouteIfIndex,
}

impl RoutingColumn {
    /// Every column, for iteration and coverage tests.
    pub const ALL: [RoutingColumn; 4] = [
        RoutingColumn::BgpPeerState,
        RoutingColumn::OspfNbrState,
        RoutingColumn::InetCidrRouteType,
        RoutingColumn::InetCidrRouteIfIndex,
    ];

    /// The stable token, for logs and tests.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            RoutingColumn::BgpPeerState => "bgp_peer_state",
            RoutingColumn::OspfNbrState => "ospf_nbr_state",
            RoutingColumn::InetCidrRouteType => "inet_cidr_route_type",
            RoutingColumn::InetCidrRouteIfIndex => "inet_cidr_route_if_index",
        }
    }

    /// Parse a stored token back into a column.
    #[must_use]
    pub fn from_token(s: &str) -> Option<Self> {
        match s {
            "bgp_peer_state" => Some(RoutingColumn::BgpPeerState),
            "ospf_nbr_state" => Some(RoutingColumn::OspfNbrState),
            "inet_cidr_route_type" => Some(RoutingColumn::InetCidrRouteType),
            "inet_cidr_route_if_index" => Some(RoutingColumn::InetCidrRouteIfIndex),
            _ => None,
        }
    }
}

/// Which protocol reported an adjacency.
///
/// Separate from [`LinkSource`], which also covers evidence no routing protocol produces (manual,
/// LLDP, CDP, shared subnet). [`RoutingProto::link_source`] is the one place the two are related,
/// and a test pins the mapping onto exactly the three routing members of `LinkSource`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoutingProto {
    /// An OSPF neighbour relationship (`ospfNbrTable`).
    Ospf,
    /// A connected host route to the peer's address (`inetCidrRouteTable`, `local(3)`).
    Route,
    /// A BGP peering session (`bgpPeerTable`).
    Bgp,
}

impl RoutingProto {
    /// Every protocol, for iteration and coverage tests.
    pub const ALL: [RoutingProto; 3] = [RoutingProto::Ospf, RoutingProto::Route, RoutingProto::Bgp];

    /// The stable token stored in the content key.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            RoutingProto::Ospf => "ospf",
            RoutingProto::Route => "route",
            RoutingProto::Bgp => "bgp",
        }
    }

    /// Parse a stored token back into a protocol.
    #[must_use]
    pub fn from_token(s: &str) -> Option<Self> {
        match s {
            "ospf" => Some(RoutingProto::Ospf),
            "route" => Some(RoutingProto::Route),
            "bgp" => Some(RoutingProto::Bgp),
            _ => None,
        }
    }

    /// The link source this protocol's evidence produces.
    #[must_use]
    pub const fn link_source(self) -> LinkSource {
        match self {
            RoutingProto::Ospf => LinkSource::Ospf,
            RoutingProto::Route => LinkSource::Route,
            RoutingProto::Bgp => LinkSource::Bgp,
        }
    }

    /// Whether an adjacency from this protocol is, on its own, evidence of a *physical* link.
    ///
    /// OSPF neighbours are link-local by construction (virtual links live in `ospfVirtNbrTable`,
    /// which is not walked) and a `local(3)` route says the destination is on a local interface. A
    /// BGP session says nothing of the kind: iBGP is routinely established between loopbacks many
    /// hops apart, and in a route-reflector design *every* router peers with the reflector. Taking
    /// those at face value would draw a star to the reflector that does not exist, and in `derived`
    /// suppression mode that star becomes a parent that silences real outages.
    ///
    /// So a BGP peer is admitted only when the local node also terminates a subnet containing the
    /// peer address — a fact read off [`crate::L3Snapshot`], not a guess. That is exactly the
    /// eBGP-over-a-shared-segment case, and exactly not the iBGP-to-loopback one.
    #[must_use]
    pub const fn implies_direct_link(self) -> bool {
        match self {
            RoutingProto::Ospf | RoutingProto::Route => true,
            RoutingProto::Bgp => false,
        }
    }
}

/// One routing adjacency a node reported.
///
/// The identity is `(proto, peer)`: a node has at most one BGP session and one OSPF neighbour
/// relationship per peer address, and the same peer legitimately appears under two protocols.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoutingAdjacency {
    /// Which protocol reported it.
    pub proto: RoutingProto,
    /// The peer's address, as the agent indexed it. Never narrowed to v4 in this type even though
    /// BGP4-MIB and OSPFv2 are both IPv4-only — the route probe is not.
    pub peer: IpAddr,
    /// The local interface the adjacency is on, when the source names one. OSPF supplies it only on
    /// an addressless (unnumbered) link, which is the case it exists to resolve; the route probe
    /// always supplies it.
    #[serde(default)]
    pub local_ifindex: Option<u32>,
    /// The protocol's own state value, verbatim (`bgpPeerState` 1–6, `ospfNbrState` 1–8).
    ///
    /// Recorded for the operator, and **deliberately not part of the content key** — see the module
    /// header. `None` for a route probe, which has no session to be in a state.
    #[serde(default)]
    pub state: Option<i64>,
}

impl RoutingAdjacency {
    /// An adjacency with the two things every one of them has.
    #[must_use]
    pub fn new(proto: RoutingProto, peer: IpAddr) -> Self {
        Self {
            proto,
            peer,
            local_ifindex: None,
            state: None,
        }
    }

    /// Whether this adjacency can name a peer at all.
    ///
    /// An unspecified, loopback, link-local or multicast peer address identifies nothing: `0.0.0.0`
    /// is what an agent reports for a neighbour it has not resolved, and every device's `127.0.0.1`
    /// would match every other device's.
    #[must_use]
    pub fn identifies_a_peer(&self) -> bool {
        match self.peer {
            IpAddr::V4(v4) => {
                !v4.is_unspecified()
                    && !v4.is_loopback()
                    && !v4.is_link_local()
                    && !v4.is_multicast()
                    && !v4.is_broadcast()
            }
            IpAddr::V6(v6) => {
                !v6.is_unspecified()
                    && !v6.is_loopback()
                    && !v6.is_multicast()
                    && (v6.segments()[0] & 0xffc0) != 0xfe80
            }
        }
    }
}

/// Every routing adjacency a node reports on one observation, as a set.
///
/// The unit of storage is the whole set per node, for the same reason [`crate::NeighborSet`] and
/// [`crate::L3Snapshot`] are: a partial walk must never read as "every peer disappeared". A failed
/// walk sends no set at all (`PollResult.routing = None`) so nothing is written; an **empty** set is
/// a real observation and does replace the stored one.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoutingSnapshot {
    /// The adjacencies, canonically ordered.
    #[serde(default)]
    pub adjacencies: Vec<RoutingAdjacency>,
    /// Whether a cap was hit and rows were dropped.
    #[serde(default)]
    pub truncated: bool,
}

impl RoutingSnapshot {
    /// Build a snapshot from observed adjacencies, canonicalizing immediately.
    #[must_use]
    pub fn new(adjacencies: Vec<RoutingAdjacency>, walk_truncated: bool) -> Self {
        let mut set = Self {
            adjacencies,
            truncated: walk_truncated,
        };
        set.canonicalize();
        set
    }

    /// How many adjacencies the snapshot holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.adjacencies.len()
    }

    /// Whether the node reported no adjacency at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.adjacencies.is_empty()
    }

    /// Put the snapshot in canonical form so two observations of an unchanged device compare equal.
    ///
    /// Adjacencies that name no usable peer are dropped first — they can never produce a link, and
    /// keeping them would let a router full of unresolved `0.0.0.0` OSPF neighbours consume the cap
    /// that the real ones need. Truncation happens **after** sorting, so which adjacencies survive
    /// does not depend on walk order.
    pub fn canonicalize(&mut self) {
        self.adjacencies.retain(RoutingAdjacency::identifies_a_peer);
        self.adjacencies.sort_by_key(|a| (a.proto, a.peer));
        self.adjacencies
            .dedup_by(|a, b| (a.proto, a.peer) == (b.proto, b.peer));
        if self.adjacencies.len() > MAX_ROUTING_ADJACENCIES_PER_NODE {
            self.adjacencies.truncate(MAX_ROUTING_ADJACENCIES_PER_NODE);
            self.truncated = true;
        }
    }

    /// The stable content key used for change comparison.
    ///
    /// ⚠️ Effectively a wire format; versioned (`v1` first line) for the same reason
    /// [`crate::L3Snapshot::content_key`] is. Built by hand rather than through `serde_json` so
    /// serde's output cannot drift underneath it.
    ///
    /// **`state` is excluded.** A BGP session that flaps between `active(3)` and `established(6)`
    /// has not changed the topology; including it would make the key move on every flap, which is
    /// the fastest way to make a change signal meaningless. What the key answers is "is this node
    /// adjacent to the same set of peers, over the same interfaces".
    #[must_use]
    pub fn content_key(&self) -> String {
        let mut out = String::from("v1\n");
        for a in &self.adjacencies {
            // Every field here is a number, an address or a fixed token — no device-supplied free
            // text — so no value can forge a line boundary.
            out.push_str("k=");
            out.push_str(a.proto.as_str());
            out.push('\n');
            out.push_str("p=");
            out.push_str(&a.peer.to_string());
            out.push('\n');
            out.push_str("i=");
            match a.local_ifindex {
                Some(i) => out.push_str(&i.to_string()),
                None => out.push('-'),
            }
            out.push('\n');
        }
        out.push_str(if self.truncated { "x=1\n" } else { "x=0\n" });
        out
    }
}

// ── The fixed column lists (a bus contract) ──────────────────────────────────────────

/// The columns that are **walked**: one row per BGP peer and per OSPF neighbour.
///
/// Both tables are indexed by the peer's address, which is why they go through the instance walker
/// rather than the numeric one — see the module header on `T_BGP`.
///
/// **This list is the bus contract**; keep it stable for N/N-1 compatibility.
#[must_use]
pub fn builtin_routing_columns() -> Vec<(RoutingColumn, &'static str)> {
    vec![
        (RoutingColumn::BgpPeerState, OID_BGP_PEER_STATE),
        (RoutingColumn::OspfNbrState, OID_OSPF_NBR_STATE),
    ]
}

/// The columns that are **probed** — one subtree walk per destination, never a table walk.
///
/// Both are needed and neither is redundant: the type says whether the destination is on a local
/// interface (`local(3)`) or merely routed onward, and the ifIndex says which port. Reading only
/// the ifIndex would turn every reachable address in the fleet into an adjacency.
#[must_use]
pub fn route_probe_columns() -> Vec<(RoutingColumn, &'static str)> {
    vec![
        (RoutingColumn::InetCidrRouteType, OID_INET_CIDR_ROUTE_TYPE),
        (
            RoutingColumn::InetCidrRouteIfIndex,
            OID_INET_CIDR_ROUTE_IF_INDEX,
        ),
    ]
}

/// Build the OID a targeted route probe walks: the column, then the leading part of the row index
/// that pins the destination.
///
/// `inetCidrRouteEntry`'s INDEX is
/// `(destType, dest, pfxLen, policy, nextHopType, nextHop)`, and `dest` is a variable-length
/// `InetAddress`, so it is encoded length-prefixed. Rooting a subtree walk at
///
/// ```text
/// <column> . <destType> . <addrLen> . <address octets>
/// ```
///
/// therefore covers **every route to that destination and nothing else** — the sub-identifiers that
/// follow are the prefix length, policy and next hop. That is what makes this a bounded probe on a
/// table nobody may walk: the answer is a handful of rows however large the routing table is.
///
/// The remaining index is returned by the agent as the row's instance, whose first sub-identifier is
/// the prefix length — see [`route_prefix_len_from_instance`].
#[must_use]
pub fn route_probe_oid(column_base: &str, target: IpAddr) -> String {
    let mut oid = String::with_capacity(column_base.len() + 24);
    oid.push_str(column_base);
    match target {
        IpAddr::V4(v4) => {
            oid.push_str(".1.4");
            for o in v4.octets() {
                oid.push('.');
                oid.push_str(&o.to_string());
            }
        }
        IpAddr::V6(v6) => {
            oid.push_str(".2.16");
            for o in v6.octets() {
                oid.push('.');
                oid.push_str(&o.to_string());
            }
        }
    }
    oid
}

/// The prefix length a probe row's instance begins with, or `None` for an instance that is empty.
///
/// The instance is everything after the destination: `pfxLen . policy… . nextHopType . nextHop…`.
/// Only the first sub-identifier is read, because a host route is the only kind this probe accepts
/// and the rest of the index describes *how* the route forwards, which an adjacency does not depend
/// on.
#[must_use]
pub fn route_prefix_len_from_instance(instance: &[u32]) -> Option<u8> {
    u8::try_from(*instance.first()?).ok()
}

/// The prefix length that means "this address alone" for its family.
#[must_use]
pub const fn host_prefix_len(ip: IpAddr) -> u8 {
    match ip {
        IpAddr::V4(_) => 32,
        IpAddr::V6(_) => 128,
    }
}

/// Whether an address is one BGP4-MIB can express. Used to declare the IPv6 gap rather than answer
/// it with silence.
#[must_use]
pub const fn bgp_mib_covers(ip: IpAddr) -> bool {
    matches!(ip, IpAddr::V4(_))
}

/// Decode a `bgpPeerState` row instance (`bgpPeerRemoteAddr`, four sub-identifiers).
#[must_use]
pub fn bgp_peer_from_instance(instance: &[u32]) -> Option<IpAddr> {
    let &[a, b, c, d] = instance else { return None };
    let octets: Vec<u8> = [a, b, c, d]
        .iter()
        .map(|v| u8::try_from(*v).ok())
        .collect::<Option<_>>()?;
    Some(IpAddr::V4(Ipv4Addr::new(
        octets[0], octets[1], octets[2], octets[3],
    )))
}

/// Decode an `ospfNbrState` row instance: `(ospfNbrIpAddr, ospfNbrAddressLessIndex)`.
///
/// Returns the neighbour's address and, when the link is addressless, the **local** ifIndex the
/// adjacency runs over. `ospfNbrAddressLessIndex` is zero on a numbered link, and RFC 1850 requires
/// `ospfNbrIpAddr` to be populated in both cases — on an addressless link with the address of
/// another of the neighbour's interfaces. That is precisely why OSPF answers the unnumbered case
/// Increment 1 could not.
#[must_use]
pub fn ospf_neighbor_from_instance(instance: &[u32]) -> Option<(IpAddr, Option<u32>)> {
    let &[a, b, c, d, addressless] = instance else {
        return None;
    };
    let octets: Vec<u8> = [a, b, c, d]
        .iter()
        .map(|v| u8::try_from(*v).ok())
        .collect::<Option<_>>()?;
    let ip = IpAddr::V4(Ipv4Addr::new(octets[0], octets[1], octets[2], octets[3]));
    let ifindex = if addressless == 0 {
        None
    } else {
        Some(addressless)
    };
    Some((ip, ifindex))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn every_token_round_trips_through_serde_and_from_token() {
        // The stored token and the JSON tag come from two different mechanisms (`as_str` and
        // `#[serde(rename_all)]`); a disagreement would mean documents the writer produces are
        // documents the reader silently drops.
        for c in RoutingColumn::ALL {
            assert_eq!(RoutingColumn::from_token(c.as_str()), Some(c));
            assert_eq!(
                serde_json::to_string(&c).unwrap(),
                format!("\"{}\"", c.as_str())
            );
        }
        for p in RoutingProto::ALL {
            assert_eq!(RoutingProto::from_token(p.as_str()), Some(p));
            assert_eq!(
                serde_json::to_string(&p).unwrap(),
                format!("\"{}\"", p.as_str())
            );
        }
        assert_eq!(RoutingProto::from_token("isis"), None);
    }

    #[test]
    fn every_protocol_maps_to_a_distinct_routing_link_source() {
        // The mapping is the one place `RoutingProto` and `LinkSource` are related. If two protocols
        // ever collapsed onto one source, the map would stop being able to say *why* an edge exists.
        let mut sources: Vec<LinkSource> =
            RoutingProto::ALL.iter().map(|p| p.link_source()).collect();
        sources.sort_unstable();
        sources.dedup();
        assert_eq!(sources.len(), RoutingProto::ALL.len());
        assert_eq!(
            sources,
            vec![LinkSource::Ospf, LinkSource::Route, LinkSource::Bgp],
            "these three sources exist only to carry routing evidence"
        );
    }

    #[test]
    fn only_bgp_needs_a_reachability_qualifier() {
        // The asymmetry is the whole iBGP defence: OSPF and a connected route are link-local facts,
        // a BGP session is not.
        assert!(RoutingProto::Ospf.implies_direct_link());
        assert!(RoutingProto::Route.implies_direct_link());
        assert!(!RoutingProto::Bgp.implies_direct_link());
    }

    #[test]
    fn the_route_probe_oid_pins_the_destination_and_nothing_more() {
        // The lab's real PPPoE address. A subtree walk from this OID returns only routes to it.
        assert_eq!(
            route_probe_oid("1.3.6.1.2.1.4.24.7.1.8", ip("133.123.189.109")),
            "1.3.6.1.2.1.4.24.7.1.8.1.4.133.123.189.109"
        );
        // v6 uses the InetAddressType/length pair its family requires; nothing here is v4-only.
        assert_eq!(
            route_probe_oid("1.3.6.1.2.1.4.24.7.1.7", ip("2001:db8::1")),
            "1.3.6.1.2.1.4.24.7.1.7.2.16.32.1.13.184.0.0.0.0.0.0.0.0.0.0.0.1"
        );
    }

    #[test]
    fn a_probe_row_reports_its_prefix_length_first() {
        // instance = pfxLen . policyLen . policy… . nextHopType . nextHopLen . nextHop…
        assert_eq!(
            route_prefix_len_from_instance(&[32, 1, 0, 1, 4, 10, 0, 0, 1]),
            Some(32)
        );
        assert_eq!(route_prefix_len_from_instance(&[128]), Some(128));
        assert_eq!(route_prefix_len_from_instance(&[]), None);
        // A sub-identifier that cannot be a prefix length is rejected rather than wrapped.
        assert_eq!(route_prefix_len_from_instance(&[999]), None);
    }

    #[test]
    fn host_prefix_len_is_family_aware() {
        assert_eq!(host_prefix_len(ip("10.0.0.1")), 32);
        assert_eq!(host_prefix_len(ip("2001:db8::1")), 128);
    }

    #[test]
    fn bgp4_mib_is_declared_ipv4_only_rather_than_answered_with_half_the_truth() {
        assert!(bgp_mib_covers(ip("10.0.0.1")));
        assert!(!bgp_mib_covers(ip("2001:db8::1")));
    }

    #[test]
    fn a_bgp_peer_is_the_row_index() {
        assert_eq!(
            bgp_peer_from_instance(&[192, 0, 2, 1]),
            Some(ip("192.0.2.1"))
        );
        // Wrong arity, and an octet that cannot be one.
        assert_eq!(bgp_peer_from_instance(&[192, 0, 2]), None);
        assert_eq!(bgp_peer_from_instance(&[192, 0, 2, 1, 0]), None);
        assert_eq!(bgp_peer_from_instance(&[192, 0, 2, 300]), None);
    }

    #[test]
    fn an_ospf_neighbour_carries_the_local_ifindex_only_when_the_link_is_addressless() {
        // Numbered link: the address is enough, and there is no local index to report.
        assert_eq!(
            ospf_neighbor_from_instance(&[10, 0, 0, 2, 0]),
            Some((ip("10.0.0.2"), None))
        );
        // Addressless (unnumbered) link — the case Increment 1 declared out of scope. The neighbour
        // still names an address (RFC 1850), and the index names our own interface.
        assert_eq!(
            ospf_neighbor_from_instance(&[10, 0, 0, 2, 7]),
            Some((ip("10.0.0.2"), Some(7)))
        );
        assert_eq!(ospf_neighbor_from_instance(&[10, 0, 0, 2]), None);
    }

    #[test]
    fn an_adjacency_that_names_no_usable_peer_is_dropped() {
        // `0.0.0.0` is what an agent reports for a neighbour it has not resolved; every device has
        // 127.0.0.1 and an fe80:: address, so any of them would match every other device.
        for bad in [
            "0.0.0.0",
            "127.0.0.1",
            "169.254.1.1",
            "224.0.0.5",
            "255.255.255.255",
            "::",
            "::1",
            "fe80::1",
            "ff02::5",
        ] {
            let a = RoutingAdjacency::new(RoutingProto::Ospf, ip(bad));
            assert!(!a.identifies_a_peer(), "{bad} must not identify a peer");
        }
        assert!(RoutingAdjacency::new(RoutingProto::Bgp, ip("192.0.2.1")).identifies_a_peer());
        assert!(RoutingAdjacency::new(RoutingProto::Route, ip("2001:db8::1")).identifies_a_peer());
    }

    #[test]
    fn canonicalization_is_order_independent_and_drops_unusable_peers() {
        let good = RoutingAdjacency::new(RoutingProto::Bgp, ip("192.0.2.1"));
        let other = RoutingAdjacency::new(RoutingProto::Ospf, ip("10.0.0.2"));
        let junk = RoutingAdjacency::new(RoutingProto::Ospf, ip("0.0.0.0"));

        let one = RoutingSnapshot::new(vec![good.clone(), junk.clone(), other.clone()], false);
        let two = RoutingSnapshot::new(vec![other, junk, good], false);
        assert_eq!(one.len(), 2);
        assert_eq!(one, two);
        assert_eq!(one.content_key(), two.content_key());
    }

    #[test]
    fn the_same_peer_under_two_protocols_is_two_adjacencies() {
        // A pair of routers routinely runs OSPF and iBGP over the same address. Collapsing them
        // would lose the evidence that says which one produced an edge.
        let snap = RoutingSnapshot::new(
            vec![
                RoutingAdjacency::new(RoutingProto::Ospf, ip("10.0.0.2")),
                RoutingAdjacency::new(RoutingProto::Bgp, ip("10.0.0.2")),
            ],
            false,
        );
        assert_eq!(snap.len(), 2);
    }

    #[test]
    fn a_flapping_session_does_not_move_the_content_key() {
        // The property the module header argues for: state is recorded but never part of the key.
        let mut down = RoutingAdjacency::new(RoutingProto::Bgp, ip("192.0.2.1"));
        down.state = Some(3); // active
        let mut up = down.clone();
        up.state = Some(6); // established

        let a = RoutingSnapshot::new(vec![down], false);
        let b = RoutingSnapshot::new(vec![up], false);
        assert_ne!(a, b, "the state is still recorded");
        assert_eq!(
            a.content_key(),
            b.content_key(),
            "a session flap is not a topology change"
        );
    }

    #[test]
    fn the_content_key_moves_on_a_real_change() {
        let base = RoutingSnapshot::new(
            vec![RoutingAdjacency::new(RoutingProto::Ospf, ip("10.0.0.2"))],
            false,
        );
        // A new peer.
        let grown = RoutingSnapshot::new(
            vec![
                RoutingAdjacency::new(RoutingProto::Ospf, ip("10.0.0.2")),
                RoutingAdjacency::new(RoutingProto::Ospf, ip("10.0.0.3")),
            ],
            false,
        );
        assert_ne!(base.content_key(), grown.content_key());
        // The same peer moved to another interface.
        let mut moved = RoutingAdjacency::new(RoutingProto::Ospf, ip("10.0.0.2"));
        moved.local_ifindex = Some(9);
        assert_ne!(
            base.content_key(),
            RoutingSnapshot::new(vec![moved], false).content_key()
        );
        // And losing every adjacency.
        assert_ne!(base.content_key(), RoutingSnapshot::default().content_key());
    }

    #[test]
    fn truncation_is_deterministic_and_declares_itself() {
        let many: Vec<RoutingAdjacency> = (0..MAX_ROUTING_ADJACENCIES_PER_NODE + 10)
            .map(|i| {
                #[allow(clippy::cast_possible_truncation)]
                let addr = IpAddr::V4(Ipv4Addr::from((10u32 << 24) + i as u32 + 1));
                RoutingAdjacency::new(RoutingProto::Bgp, addr)
            })
            .collect();
        let mut shuffled = many.clone();
        shuffled.reverse();

        let a = RoutingSnapshot::new(many, false);
        let b = RoutingSnapshot::new(shuffled, false);
        assert_eq!(a.len(), MAX_ROUTING_ADJACENCIES_PER_NODE);
        assert!(a.truncated, "the cap must be surfaced, not swallowed");
        assert_eq!(
            a, b,
            "which adjacencies survive must not depend on walk order"
        );
    }

    #[test]
    fn a_walk_that_hit_its_budget_stays_truncated_even_when_the_set_is_small() {
        // The walk-level flag comes from the worker, which is the only layer that knows the budget;
        // canonicalization must carry it through rather than recomputing it from the length.
        let snap = RoutingSnapshot::new(
            vec![RoutingAdjacency::new(RoutingProto::Bgp, ip("192.0.2.1"))],
            true,
        );
        assert!(snap.truncated);
        assert!(snap.content_key().contains("x=1"));
    }

    #[test]
    fn the_walked_and_probed_column_lists_are_disjoint_and_distinct() {
        let walked = builtin_routing_columns();
        let probed = route_probe_columns();
        assert_eq!(walked.len(), 2);
        assert_eq!(probed.len(), 2);
        let mut oids: Vec<&str> = walked
            .iter()
            .chain(probed.iter())
            .map(|(_, o)| *o)
            .collect();
        oids.sort_unstable();
        oids.dedup();
        assert_eq!(oids.len(), 4, "a shared OID base would merge two columns");
        // The route columns must never appear in the walked list: walking `inetCidrRouteTable` as a
        // table is the failure this whole design exists to avoid.
        assert!(walked
            .iter()
            .all(|(_, o)| !o.starts_with("1.3.6.1.2.1.4.24")));
    }
}
