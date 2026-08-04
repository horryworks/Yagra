// SPDX-License-Identifier: AGPL-3.0-only
//! The derived connectivity graph's shared vocabulary: what a link is, and what evidence produced
//! it (ADR-043).
//!
//! ADR-043 inverts how topology is authored. Until now the only edge source was `nodes.parent_id` —
//! single-parent, directed, and typed in by hand, which meant in practice it was never typed in at
//! all and dependency suppression never fired. The **Network map** becomes an undirected
//! connectivity graph derived from what devices report, and Dependencies becomes a projection of it.
//! This module holds the pieces both the deriving crate (`yagra-topology`) and the serving crate
//! (`yagra-core`) need to agree on.
//!
//! Two decisions are encoded here rather than at each call site:
//!
//! 1. **A link's identity is the unordered node pair.** Not `(node, port, node, port)` — per-port
//!    adjacency already lives in [`crate::NeighborSet`] (ADR-038), and storing it twice would make
//!    the same fact maintainable in two places. Interfaces ride along as payload. A side effect
//!    worth having: the whole class of "did CDP and L3 agree on the ifIndex" bugs cannot occur.
//!
//! 2. **Evidence accumulates; it does not compete.** One physical link seen by LLDP, by CDP and by
//!    shared-subnet membership is *one* link with three [`LinkSource`]s, not three rows. The single
//!    representative source shown in a UI is derived by [`LinkSource::best`], so "which sources saw
//!    this" and "what do we call it" cannot drift apart.

use crate::NodeId;
use serde::{Deserialize, Serialize};

/// Cap on links attributed to any one node, applied after sorting so truncation is deterministic.
/// A flat segment with a misdetected transit node could otherwise attach a single box to thousands
/// of peers.
pub const MAX_LINKS_PER_NODE: usize = 512;

/// What kind of evidence produced a link, ordered strongest first.
//
// ⚠️ The `///` above and on each variant is published verbatim to API clients through the generated
// OpenAPI document — keep it outward-facing and put the reasoning in `//` like this.
//
// The ordering is a total order by strength and it is load-bearing: ADR-043 決定 4 says a manually
// pinned link always wins a recomputation, and expressing that as a rank on this enum makes it one
// function's property instead of a rule repeated wherever links are merged. Increments 2–4 append
// variants (`Route`, `Bgp`, `Ospf`); they slot in by rank rather than by declaration order, and the
// stored column is a plain `TEXT[]` with no `CHECK`, so adding one needs no migration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum LinkSource {
    /// An operator confirmed this link by hand.
    Manual,
    /// LLDP adjacency (IEEE 802.1AB), matched to a node by the peer's management address.
    Lldp,
    /// Cisco Discovery Protocol adjacency, matched by the peer's cache address.
    Cdp,
    /// Both nodes have an interface address in the same IP subnet.
    L3Subnet,
}

impl LinkSource {
    /// Every source, for iteration and coverage tests.
    pub const ALL: [LinkSource; 4] = [
        LinkSource::Manual,
        LinkSource::Lldp,
        LinkSource::Cdp,
        LinkSource::L3Subnet,
    ];

    /// The stable token stored in `node_links.sources` and rendered by the UI.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            LinkSource::Manual => "manual",
            LinkSource::Lldp => "lldp",
            LinkSource::Cdp => "cdp",
            LinkSource::L3Subnet => "l3_subnet",
        }
    }

    /// Parse a stored token back into a source. Unknown tokens yield `None` so a row written by a
    /// newer core is skipped rather than mis-read by an older one.
    #[must_use]
    pub fn from_token(s: &str) -> Option<Self> {
        match s {
            "manual" => Some(LinkSource::Manual),
            "lldp" => Some(LinkSource::Lldp),
            "cdp" => Some(LinkSource::Cdp),
            "l3_subnet" => Some(LinkSource::L3Subnet),
            _ => None,
        }
    }

    /// Strength rank, lower is stronger.
    ///
    /// LLDP outranks CDP because it is the standard, and because [`crate::NeighborProto`] already
    /// documents that a device speaking both reports the same link twice under different
    /// identities. Both outrank shared-subnet membership: L2 names a physical port, whereas L3
    /// co-membership is an inference from two nodes being on one network.
    #[must_use]
    pub const fn rank(self) -> u8 {
        match self {
            LinkSource::Manual => 0,
            LinkSource::Lldp => 1,
            LinkSource::Cdp => 2,
            LinkSource::L3Subnet => 3,
        }
    }

    /// The strongest source in a set — the one a UI should label the link with.
    ///
    /// `None` only for an empty set, which a stored link never has.
    #[must_use]
    pub fn best(sources: &[LinkSource]) -> Option<LinkSource> {
        sources.iter().copied().min_by_key(|s| s.rank())
    }
}

/// Ordering *is* strength ordering, so `min` means "strongest" everywhere without a comparator.
impl Ord for LinkSource {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.rank().cmp(&other.rank())
    }
}

impl PartialOrd for LinkSource {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// One undirected link between two inventory nodes, as the derivation produces it.
///
/// Endpoints are strict [`NodeId`]s here on purpose. The *stored columns* and the *API DTO* are
/// nullable from the first migration, because Increment 3 attaches ARP-discovered endpoints that
/// are not in `nodes` and widening a published DTO later would be a breaking API change. Widening
/// this internal struct, by contrast, is a compiler-checked edit — so the cost is paid where it is
/// expensive to change, not where it is cheap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DerivedLink {
    /// The lower-ordered endpoint. Always `< b_node`; see [`DerivedLink::new`].
    pub a_node: NodeId,
    /// The higher-ordered endpoint.
    pub b_node: NodeId,
    /// `ifIndex` on the `a` side, when a source named one.
    pub a_ifindex: Option<u32>,
    /// `ifIndex` on the `b` side.
    pub b_ifindex: Option<u32>,
    /// Human-readable port name on the `a` side (`Gi0/1`), when a source named one.
    pub a_if_name: Option<String>,
    /// Port name on the `b` side.
    pub b_if_name: Option<String>,
    /// Every source that produced this link, canonically ordered strongest-first.
    pub sources: Vec<LinkSource>,
    /// The subnet that produced an [`LinkSource::L3Subnet`] edge, e.g. `192.168.1.0/24`. Carried so
    /// the operator can answer "why is this edge here" without re-deriving anything.
    pub subnet: Option<String>,
}

impl DerivedLink {
    /// A bare link between two nodes, with the endpoints put in canonical order.
    ///
    /// Canonical ordering is what makes the undirected edge have exactly one representation. Skip
    /// it and the same link stores twice, under two keys, and the `UNIQUE` index means nothing.
    #[must_use]
    pub fn new(x: NodeId, y: NodeId, source: LinkSource) -> Self {
        let (a_node, b_node) = if x <= y { (x, y) } else { (y, x) };
        Self {
            a_node,
            b_node,
            a_ifindex: None,
            b_ifindex: None,
            a_if_name: None,
            b_if_name: None,
            sources: vec![source],
            subnet: None,
        }
    }

    /// Whether the two endpoints were supplied in the order they are stored in.
    ///
    /// Callers that know something about one *side* (which port the local node saw the peer on)
    /// need this to attach that fact to the right endpoint after canonicalization.
    #[must_use]
    pub fn is_a(&self, node: NodeId) -> bool {
        self.a_node == node
    }

    /// The stable dedup key: `v1|<lower>|<upper>`.
    ///
    /// Endpoints are prefixed with `n:` so Increment 3 can introduce `x:` for an endpoint that is
    /// not an inventory node without colliding or re-keying anything already stored.
    ///
    /// ⚠️ Versioned because changing the grammar re-keys every stored link. Unlike a neighbour set,
    /// where a re-key costs one spurious history row, a re-keyed link silently loses its
    /// `first_seen` — the operator's answer to "how long has this been here".
    #[must_use]
    pub fn link_key(&self) -> String {
        format!("v1|n:{}|n:{}", self.a_node, self.b_node)
    }

    /// Fold another observation of the same link into this one.
    ///
    /// Sources accumulate. Interface names and the subnet fill in from whichever observation has
    /// them, but a **stronger** source may overwrite what a weaker one wrote: L2 names the physical
    /// port and L3 names the SVI, so when both are present the L2 answer is the one an operator
    /// should see.
    pub fn merge(&mut self, other: &DerivedLink) {
        debug_assert_eq!(
            (self.a_node, self.b_node),
            (other.a_node, other.b_node),
            "merge is only defined for two observations of the same node pair"
        );
        let incoming_is_stronger = match (
            LinkSource::best(&other.sources),
            LinkSource::best(&self.sources),
        ) {
            (Some(new), Some(cur)) => new.rank() < cur.rank(),
            (Some(_), None) => true,
            _ => false,
        };
        for s in &other.sources {
            if !self.sources.contains(s) {
                self.sources.push(*s);
            }
        }
        self.sources.sort_unstable();
        let take = |cur: &mut Option<u32>, new: Option<u32>| {
            if new.is_some() && (cur.is_none() || incoming_is_stronger) {
                *cur = new;
            }
        };
        take(&mut self.a_ifindex, other.a_ifindex);
        take(&mut self.b_ifindex, other.b_ifindex);
        if other.a_if_name.is_some() && (self.a_if_name.is_none() || incoming_is_stronger) {
            self.a_if_name.clone_from(&other.a_if_name);
        }
        if other.b_if_name.is_some() && (self.b_if_name.is_none() || incoming_is_stronger) {
            self.b_if_name.clone_from(&other.b_if_name);
        }
        if self.subnet.is_none() {
            self.subnet.clone_from(&other.subnet);
        }
    }
}

/// What the last derivation run observed but did not turn into a link.
//
// ⚠️ Published verbatim to API clients (see `LinkSource`). Reasoning goes in `//`.
//
// Every field is a place the derivation chose to stay silent rather than guess. They exist because
// the alternative — dropping the row and moving on — makes an incomplete graph indistinguishable
// from a complete one. `unmatched_lldp_rows` is the acceptance instrument for the half of ADR-043
// that cannot be tested in this lab: the day a switch is racked, the number moves with no code
// change.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct TopologyLinkSummary {
    /// Links whose strongest evidence is shared-subnet membership.
    #[serde(default)]
    pub l3_links: u32,
    /// Links produced from a CDP adjacency.
    #[serde(default)]
    pub cdp_links: u32,
    /// Links produced from an LLDP adjacency.
    #[serde(default)]
    pub lldp_links: u32,
    /// LLDP rows whose management address matched no monitored node.
    #[serde(default)]
    pub unmatched_lldp_rows: u32,
    /// CDP rows whose management address matched no monitored node.
    #[serde(default)]
    pub unmatched_cdp_rows: u32,
    /// Adjacency rows whose management address matched more than one node, so no link could be
    /// attributed without guessing which.
    #[serde(default)]
    pub ambiguous_mgmt_addrs: u32,
    /// Addresses claimed by two or more nodes — a shared virtual IP, or a duplicate-address
    /// misconfiguration.
    #[serde(default)]
    pub duplicate_addresses: u32,
    /// Segments with more than two members where no member could be identified as routing for the
    /// others, so no link was drawn.
    #[serde(default)]
    pub oversized_segments: u32,
    /// Addresses sharing network bits but disagreeing on prefix length.
    #[serde(default)]
    pub subnet_prefix_mismatch: u32,
    /// Nodes whose observation hit a per-node cap, so what is recorded for them is incomplete.
    #[serde(default)]
    pub truncated_nodes: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn link_source_ranks_are_a_total_order_and_manual_is_strongest() {
        let mut ranks: Vec<u8> = LinkSource::ALL.iter().map(|s| s.rank()).collect();
        ranks.sort_unstable();
        ranks.dedup();
        assert_eq!(
            ranks.len(),
            LinkSource::ALL.len(),
            "two sources sharing a rank makes `best` non-deterministic"
        );
        assert_eq!(LinkSource::Manual.rank(), 0, "決定 4: manual always wins");
        assert_eq!(
            LinkSource::ALL.iter().copied().min(),
            Some(LinkSource::Manual),
            "Ord must agree with rank, or `min` silently means something else"
        );
    }

    #[test]
    fn best_picks_the_strongest_present() {
        assert_eq!(
            LinkSource::best(&[LinkSource::L3Subnet, LinkSource::Lldp]),
            Some(LinkSource::Lldp)
        );
        assert_eq!(
            LinkSource::best(&[LinkSource::Cdp, LinkSource::Manual, LinkSource::L3Subnet]),
            Some(LinkSource::Manual)
        );
        assert_eq!(LinkSource::best(&[]), None);
    }

    #[test]
    fn every_source_round_trips_through_its_token_and_through_serde() {
        for s in LinkSource::ALL {
            assert_eq!(LinkSource::from_token(s.as_str()), Some(s));
            assert_eq!(
                serde_json::to_string(&s).unwrap(),
                format!("\"{}\"", s.as_str()),
                "the DB token and the JSON tag are produced by different mechanisms"
            );
        }
        assert_eq!(LinkSource::from_token("bgp"), None);
    }

    #[test]
    fn endpoints_are_canonically_ordered_so_one_link_has_one_key() {
        let x = NodeId::new();
        let y = NodeId::new();
        let forward = DerivedLink::new(x, y, LinkSource::L3Subnet);
        let backward = DerivedLink::new(y, x, LinkSource::L3Subnet);
        assert_eq!(forward.a_node, backward.a_node);
        assert_eq!(forward.b_node, backward.b_node);
        assert!(forward.a_node <= forward.b_node);
        assert_eq!(forward.link_key(), backward.link_key());
    }

    #[test]
    fn the_link_key_is_versioned_and_names_both_endpoints() {
        let x = NodeId::new();
        let y = NodeId::new();
        let key = DerivedLink::new(x, y, LinkSource::Lldp).link_key();
        assert!(key.starts_with("v1|n:"), "{key}");
        assert!(key.contains(&x.to_string()) && key.contains(&y.to_string()));
    }

    #[test]
    fn merging_accumulates_sources_and_lets_l2_name_the_port() {
        let x = NodeId::new();
        let y = NodeId::new();

        let mut l3 = DerivedLink::new(x, y, LinkSource::L3Subnet);
        l3.subnet = Some("192.168.1.0/24".into());
        l3.a_if_name = Some("Vlan10".into());
        l3.a_ifindex = Some(10);

        let mut lldp = DerivedLink::new(x, y, LinkSource::Lldp);
        lldp.a_if_name = Some("Gi0/1".into());
        lldp.a_ifindex = Some(1);

        l3.merge(&lldp);
        assert_eq!(l3.sources, vec![LinkSource::Lldp, LinkSource::L3Subnet]);
        assert_eq!(
            l3.a_if_name.as_deref(),
            Some("Gi0/1"),
            "L2 names the physical port; L3 only knows the SVI"
        );
        assert_eq!(l3.a_ifindex, Some(1));
        assert_eq!(
            l3.subnet.as_deref(),
            Some("192.168.1.0/24"),
            "the L3 evidence is still why the link is partly there"
        );
    }

    #[test]
    fn a_weaker_observation_does_not_overwrite_a_stronger_one() {
        let x = NodeId::new();
        let y = NodeId::new();
        let mut lldp = DerivedLink::new(x, y, LinkSource::Lldp);
        lldp.a_if_name = Some("Gi0/1".into());

        let mut l3 = DerivedLink::new(x, y, LinkSource::L3Subnet);
        l3.a_if_name = Some("Vlan10".into());

        lldp.merge(&l3);
        assert_eq!(lldp.a_if_name.as_deref(), Some("Gi0/1"));
        assert_eq!(lldp.sources, vec![LinkSource::Lldp, LinkSource::L3Subnet]);
    }

    #[test]
    fn merging_is_idempotent() {
        let x = NodeId::new();
        let y = NodeId::new();
        let mut a = DerivedLink::new(x, y, LinkSource::Cdp);
        let b = a.clone();
        a.merge(&b);
        a.merge(&b);
        assert_eq!(a.sources, vec![LinkSource::Cdp]);
    }
}
