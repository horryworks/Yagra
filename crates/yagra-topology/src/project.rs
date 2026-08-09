// SPDX-License-Identifier: AGPL-3.0-only
//! Project the undirected connectivity graph onto a directed dependency graph (ADR-043 I2).
//!
//! **Pure**: no database, no clock, no bus — for the same reason [`crate::derive`] is. A wrong edge
//! on the map is visible; a wrong *parent* silently suppresses a real outage.
//!
//! The whole increment rests on one sentence: **a node's parents are the nodes immediately before it
//! on the path from a poller.** That is why direction is never guessed from what a device looks
//! like, and it is why the projection needs to know where the pollers are before it can produce
//! anything at all.
//!
//! Two consequences fall out of that sentence and both are deliberate:
//!
//! * **Redundant paths produce multiple parents.** Two equal-length routes to a node mean two nodes
//!   at distance − 1, so the node gets both. `Topology::is_suppressed` has always required *every*
//!   parent to be down, and this is the first thing in the system that gives it more than one — the
//!   HSRP pair where killing one router must not silence every server behind it.
//!
//! * **A node no poller can reach has no parents.** That is the fail-**safe** direction: an
//!   unmodelled node's alert stands rather than being attributed to something that may not be its
//!   cause. The alternative — attaching orphans to the nearest anything — is how a derivation starts
//!   swallowing outages.

use crate::Topology;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::net::IpAddr;
use yagra_common::{subnet_key, DerivedLink, L3Snapshot, NodeId};

/// Cap on parents attributed to one node.
///
/// A pathological set is safe rather than dangerous — suppression needs *all* parents down, so two
/// hundred parents means the node is effectively never suppressed — but an unbounded fan-in makes
/// the Dependencies view unreadable and the root-cause climb pointlessly wide. Applied after
/// sorting, so which parents survive is deterministic.
pub(crate) const MAX_DERIVED_PARENTS: usize = 16;

/// Cap on nodes one poller may anchor.
///
/// A poller sharing a `/16` with five hundred monitored nodes would make all five hundred roots,
/// which is fail-safe (nothing gets suppressed) but is also indistinguishable from the feature not
/// working. The cap keeps that case bounded and countable.
pub(crate) const MAX_ANCHORS_PER_POLLER: usize = 64;

/// Where one poller sits, as core knows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PollerLocation {
    /// The poller's id, used to report which pollers could not be placed.
    pub poller_id: String,
    /// The pool it serves — the granularity at which an unresolved anchor blocks derived mode,
    /// because a pool is the set of nodes one group of pollers is responsible for.
    pub pool: String,
    /// Addresses the poller reported for itself.
    pub mgmt_addrs: Vec<IpAddr>,
    /// The node an operator named as the poller's attachment point. Wins outright.
    pub anchor_node_id: Option<NodeId>,
}

/// Which nodes root the graph, and which pollers could not be placed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AnchorResolution {
    /// Nodes with nothing upstream of them.
    pub anchors: BTreeSet<NodeId>,
    /// Pool → the ids of its pollers that could not be placed, sorted. **Non-empty is a blocking
    /// condition for derived mode**, not a warning: a pool whose poller has no anchor contributes no
    /// roots, so its nodes are unreachable, so none of them is ever suppressed — while every screen
    /// looks like the feature is on.
    pub unresolved: BTreeMap<String, Vec<String>>,
    /// Pollers whose anchor set hit [`MAX_ANCHORS_PER_POLLER`].
    pub truncated_pollers: u32,
}

/// Resolve the anchor set from poller locations and the inventory.
///
/// Two rules, in order:
///
/// 1. **A named anchor wins.** `anchor_node_id` is the operator saying where the poller attaches,
///    and it is the *normal* answer rather than a repair: a containerized poller reports its bridge
///    address and matches nothing, and that is the majority deployment.
///
/// 2. **Otherwise, every inventory node sharing a subnet with a poller address is an anchor.** Not a
///    guess about which box is the first hop — the claim is weaker and exactly true: a node on the
///    poller's own segment has nothing between it and the poller, so it has no upstream. Containment
///    is tested against each *node's* prefix length, which is why the heartbeat need not carry one.
///
/// A poller matching nothing is reported, never silently dropped.
#[must_use]
pub fn resolve_anchors(
    pollers: &[PollerLocation],
    l3: &[(NodeId, L3Snapshot)],
    known_nodes: &BTreeSet<NodeId>,
) -> AnchorResolution {
    let mut out = AnchorResolution::default();
    for p in pollers {
        // A named anchor pointing at a node that has since been deleted is *not* a fallback to
        // address matching: the operator's statement is stale, and quietly substituting a different
        // answer would hide that. It reads as unresolved, which is visible and blocking.
        if let Some(id) = p.anchor_node_id {
            if known_nodes.contains(&id) {
                out.anchors.insert(id);
            } else {
                out.unresolved
                    .entry(p.pool.clone())
                    .or_default()
                    .push(p.poller_id.clone());
            }
            continue;
        }

        let mut found: BTreeSet<NodeId> = BTreeSet::new();
        for (node, snap) in l3 {
            for a in &snap.addresses {
                if !a.can_form_subnet_edge() {
                    continue;
                }
                let Some(net) = a.subnet() else { continue };
                if p.mgmt_addrs
                    .iter()
                    .any(|ip| subnet_key(*ip, a.prefix_len) == Some(net))
                {
                    found.insert(*node);
                    break;
                }
            }
        }
        if found.is_empty() {
            out.unresolved
                .entry(p.pool.clone())
                .or_default()
                .push(p.poller_id.clone());
            continue;
        }
        if found.len() > MAX_ANCHORS_PER_POLLER {
            out.truncated_pollers += 1;
        }
        out.anchors
            .extend(found.into_iter().take(MAX_ANCHORS_PER_POLLER));
    }
    for ids in out.unresolved.values_mut() {
        ids.sort();
        ids.dedup();
    }
    out
}

/// Project links onto a dependency graph rooted at `anchors`.
///
/// `parents(v) = { u : (u,v) ∈ E ∧ dist(u) = dist(v) − 1 }` where `dist` is the hop count from the
/// nearest anchor over the undirected graph. Predecessors on **every** shortest path, not one of
/// them: that is what makes a redundant pair of upstreams show up as two parents rather than as an
/// arbitrary choice between them.
///
/// An operator's `forced_parent` on a link overrides both directions of that edge — it names the
/// upstream end, and the other end is then never treated as upstream through that link no matter
/// what the distances say.
///
/// `opt_out` names nodes an operator has excluded from derived suppression entirely. They keep their
/// place in the graph — other nodes still reach the anchors *through* them — but are given no
/// parents of their own, so their alerts always stand. The flag can only ever remove suppression,
/// never add it, which is why it is safe to hand to an operator in a way per-edge approval was not.
#[must_use]
pub fn predecessors(
    links: &[DerivedLink],
    anchors: &BTreeSet<NodeId>,
    opt_out: &BTreeSet<NodeId>,
) -> Topology {
    // BFS runs over the undirected graph including forced edges, so a hand-declared link still
    // shortens paths; only the *direction* assignment below consults `forced_parent`.
    let mut adj: BTreeMap<NodeId, BTreeSet<NodeId>> = BTreeMap::new();
    for l in links {
        adj.entry(l.a_node).or_default().insert(l.b_node);
        adj.entry(l.b_node).or_default().insert(l.a_node);
    }

    let mut dist: BTreeMap<NodeId, u32> = BTreeMap::new();
    let mut queue: VecDeque<NodeId> = VecDeque::new();
    for a in anchors {
        dist.insert(*a, 0);
        queue.push_back(*a);
    }
    while let Some(n) = queue.pop_front() {
        let d = dist[&n];
        let Some(peers) = adj.get(&n) else { continue };
        for &p in peers {
            // `or_insert` would overwrite nothing but also tell us nothing; the vacant arm is the
            // "first time we have reached this node" signal that decides whether to enqueue it.
            if let std::collections::btree_map::Entry::Vacant(slot) = dist.entry(p) {
                slot.insert(d + 1);
                queue.push_back(p);
            }
        }
    }

    // Collect candidate parents per child first, so the per-node cap is applied to a sorted set
    // rather than to whatever order the links arrived in.
    let mut candidates: BTreeMap<NodeId, BTreeSet<NodeId>> = BTreeMap::new();
    for l in links {
        match l.forced_parent {
            Some(parent) => {
                let child = if parent == l.a_node {
                    l.b_node
                } else if parent == l.b_node {
                    l.a_node
                } else {
                    // The stored direction names an endpoint this link no longer has — the pair was
                    // re-derived, or the row was written by something inconsistent. Fall through to
                    // the derived direction rather than inventing a child.
                    natural_parents(l, &dist, &mut candidates);
                    continue;
                };
                candidates.entry(child).or_default().insert(parent);
            }
            None => natural_parents(l, &dist, &mut candidates),
        }
    }

    let mut topo = Topology::new();
    for (child, parents) in candidates {
        // Anchors are roots by definition and must never be suppressed: they are the reference point
        // the whole projection is measured from, and a root with a parent is a contradiction that
        // would let one segment silence the poller's own view of it.
        if anchors.contains(&child) || opt_out.contains(&child) {
            continue;
        }
        for parent in parents.into_iter().take(MAX_DERIVED_PARENTS) {
            topo.add_dependency(child, parent);
        }
    }
    topo
}

/// Add whichever endpoint is one hop closer to an anchor as the other's parent.
///
/// Neither endpoint qualifies when they are equidistant (a link *across* a tier rather than between
/// two of them) or when either is unreachable — in both cases the honest answer is that this link
/// says nothing about direction.
fn natural_parents(
    l: &DerivedLink,
    dist: &BTreeMap<NodeId, u32>,
    candidates: &mut BTreeMap<NodeId, BTreeSet<NodeId>>,
) {
    let (Some(&da), Some(&db)) = (dist.get(&l.a_node), dist.get(&l.b_node)) else {
        return;
    };
    if da + 1 == db {
        candidates.entry(l.b_node).or_default().insert(l.a_node);
    } else if db + 1 == da {
        candidates.entry(l.a_node).or_default().insert(l.b_node);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use yagra_common::{L3Address, LinkSource};

    /// `n` sequential node ids, so expectations do not depend on random UUID ordering.
    fn nodes(n: usize) -> Vec<NodeId> {
        (0..n)
            .map(|i| NodeId(uuid::Uuid::from_u128(0x1000 + i as u128)))
            .collect()
    }

    fn link(a: NodeId, b: NodeId) -> DerivedLink {
        DerivedLink::new(a, b, LinkSource::L3Subnet)
    }

    fn directed(a: NodeId, b: NodeId, parent: NodeId) -> DerivedLink {
        let mut l = link(a, b);
        l.forced_parent = Some(parent);
        l
    }

    fn anchors(ids: &[NodeId]) -> BTreeSet<NodeId> {
        ids.iter().copied().collect()
    }

    /// Projection with no opt-outs — the ordinary case, so the tests below read as being about the
    /// graph rather than about the escape hatch. Shadows the real one inside this module; the
    /// opt-out tests call `super::predecessors` explicitly.
    fn predecessors(links: &[DerivedLink], anchors: &BTreeSet<NodeId>) -> Topology {
        super::predecessors(links, anchors, &BTreeSet::new())
    }

    fn snapshot(addrs: &[(&str, u8)]) -> L3Snapshot {
        L3Snapshot {
            addresses: addrs
                .iter()
                .map(|(ip, len)| L3Address {
                    ifindex: 1,
                    ip: ip.parse().unwrap(),
                    prefix_len: *len,
                    addr_type: yagra_common::L3AddrType::Unicast,
                    source_table: yagra_common::L3SourceTable::IpAddressTable,
                })
                .collect(),
            truncated: false,
        }
    }

    // ── Anchor resolution ────────────────────────────────────────────────────

    #[test]
    fn a_poller_on_a_segment_anchors_every_node_on_it() {
        let n = nodes(3);
        let l3 = vec![
            (n[0], snapshot(&[("192.168.1.1", 24)])),
            (n[1], snapshot(&[("192.168.1.5", 24)])),
            (n[2], snapshot(&[("10.0.0.1", 24)])),
        ];
        let got = resolve_anchors(
            &[PollerLocation {
                poller_id: "edge-1".into(),
                pool: "default".into(),
                mgmt_addrs: vec!["192.168.1.9".parse().unwrap()],
                anchor_node_id: None,
            }],
            &l3,
            &nodes(3).into_iter().collect(),
        );
        // Both segment members, and only them. This is not a guess about which is the first hop:
        // neither has anything between it and the poller.
        assert_eq!(got.anchors, anchors(&[n[0], n[1]]));
        assert!(got.unresolved.is_empty());
    }

    #[test]
    fn a_containerized_poller_resolves_to_nothing_and_says_so() {
        // The majority deployment: the poller reports its docker bridge address, which shares a
        // subnet with no monitored node. Producing a rootless graph here is the silent failure the
        // whole blocking mechanism exists to prevent.
        let n = nodes(1);
        let got = resolve_anchors(
            &[PollerLocation {
                poller_id: "edge-1".into(),
                pool: "default".into(),
                mgmt_addrs: vec!["172.18.0.4".parse().unwrap()],
                anchor_node_id: None,
            }],
            &[(n[0], snapshot(&[("192.168.1.1", 24)]))],
            &anchors(&n),
        );
        assert!(got.anchors.is_empty());
        assert_eq!(got.unresolved["default"], vec!["edge-1".to_owned()]);
    }

    #[test]
    fn a_named_anchor_wins_and_does_not_consult_addresses() {
        let n = nodes(2);
        let got = resolve_anchors(
            &[PollerLocation {
                poller_id: "edge-1".into(),
                pool: "default".into(),
                // Would have matched n[0] on its own; the operator named n[1].
                mgmt_addrs: vec!["192.168.1.9".parse().unwrap()],
                anchor_node_id: Some(n[1]),
            }],
            &[(n[0], snapshot(&[("192.168.1.1", 24)]))],
            &anchors(&n),
        );
        assert_eq!(got.anchors, anchors(&[n[1]]));
    }

    #[test]
    fn a_named_anchor_pointing_at_a_deleted_node_is_unresolved_not_a_fallback() {
        // Silently falling back to address matching would hide that the operator's statement is
        // stale, and would do it at exactly the moment the graph changed shape.
        let n = nodes(2);
        let got = resolve_anchors(
            &[PollerLocation {
                poller_id: "edge-1".into(),
                pool: "default".into(),
                mgmt_addrs: vec!["192.168.1.9".parse().unwrap()],
                anchor_node_id: Some(n[1]),
            }],
            &[(n[0], snapshot(&[("192.168.1.1", 24)]))],
            &anchors(&[n[0]]), // n[1] no longer exists
        );
        assert!(got.anchors.is_empty());
        assert_eq!(got.unresolved["default"], vec!["edge-1".to_owned()]);
    }

    #[test]
    fn a_loopback_or_host_route_never_anchors_anything() {
        // Every device has 127.0.0.1 and many have a /32; either would anchor the whole fleet.
        let n = nodes(2);
        let got = resolve_anchors(
            &[PollerLocation {
                poller_id: "edge-1".into(),
                pool: "default".into(),
                mgmt_addrs: vec![
                    "127.0.0.1".parse().unwrap(),
                    "133.123.189.1".parse().unwrap(),
                ],
                anchor_node_id: None,
            }],
            &[
                (n[0], snapshot(&[("127.0.0.1", 8)])),
                (n[1], snapshot(&[("133.123.189.109", 32)])),
            ],
            &anchors(&n),
        );
        assert!(got.anchors.is_empty(), "{:?}", got.anchors);
    }

    #[test]
    fn a_v6_poller_anchors_through_a_v6_prefix() {
        let n = nodes(1);
        let got = resolve_anchors(
            &[PollerLocation {
                poller_id: "edge-1".into(),
                pool: "default".into(),
                mgmt_addrs: vec!["2001:db8::9".parse().unwrap()],
                anchor_node_id: None,
            }],
            &[(n[0], snapshot(&[("2001:db8::1", 64)]))],
            &anchors(&n),
        );
        assert_eq!(got.anchors, anchors(&n));
    }

    #[test]
    fn unresolved_pollers_are_reported_per_pool() {
        let got = resolve_anchors(
            &[
                PollerLocation {
                    poller_id: "b".into(),
                    pool: "site-a".into(),
                    mgmt_addrs: vec![],
                    anchor_node_id: None,
                },
                PollerLocation {
                    poller_id: "a".into(),
                    pool: "site-a".into(),
                    mgmt_addrs: vec![],
                    anchor_node_id: None,
                },
            ],
            &[],
            &BTreeSet::new(),
        );
        assert_eq!(
            got.unresolved["site-a"],
            vec!["a".to_owned(), "b".to_owned()]
        );
    }

    // ── Projection ───────────────────────────────────────────────────────────

    #[test]
    fn a_chain_gives_each_node_exactly_one_parent() {
        let n = nodes(3);
        let topo = predecessors(&[link(n[0], n[1]), link(n[1], n[2])], &anchors(&[n[0]]));
        let down: BTreeSet<NodeId> = anchors(&[n[0], n[1], n[2]]);
        assert!(topo.is_suppressed(n[1], &down));
        assert_eq!(topo.root_cause(n[2], &down), Some(n[0]));
    }

    #[test]
    fn a_redundant_pair_gives_two_parents_and_one_survivor_keeps_the_alert() {
        // The HSRP case, and the reason `is_suppressed` has always required *all* parents down.
        // A star to a single elected router would silence this server the moment either router
        // failed — the false-suppression class ADR-038 names and 決定 2 accepted the risk of.
        let n = nodes(4); // 0 = anchor, 1 & 2 = routers, 3 = server
        let topo = predecessors(
            &[
                link(n[0], n[1]),
                link(n[0], n[2]),
                link(n[1], n[3]),
                link(n[2], n[3]),
            ],
            &anchors(&[n[0]]),
        );
        let both_down = anchors(&[n[1], n[2], n[3]]);
        assert!(topo.is_suppressed(n[3], &both_down));

        let one_down = anchors(&[n[1], n[3]]);
        assert!(
            !topo.is_suppressed(n[3], &one_down),
            "a surviving upstream must keep the server's own alert standing"
        );
    }

    #[test]
    fn an_anchor_never_has_a_parent() {
        let n = nodes(2);
        let topo = predecessors(&[link(n[0], n[1])], &anchors(&[n[0], n[1]]));
        let down = anchors(&[n[0], n[1]]);
        assert!(!topo.is_suppressed(n[0], &down));
        assert!(!topo.is_suppressed(n[1], &down));
    }

    #[test]
    fn a_node_no_poller_can_reach_has_no_parents() {
        // Fail-safe: an unmodelled node's alert stands. Attaching orphans to the nearest anything
        // is how a derivation starts swallowing outages.
        let n = nodes(4);
        let topo = predecessors(&[link(n[0], n[1]), link(n[2], n[3])], &anchors(&[n[0]]));
        let down = anchors(&[n[2], n[3]]);
        assert!(!topo.is_suppressed(n[3], &down));
        assert!(!topo.is_suppressed(n[2], &down));
    }

    #[test]
    fn a_link_between_equidistant_nodes_states_no_direction() {
        // Two access switches cross-connected under one core: neither is upstream of the other.
        let n = nodes(3);
        let topo = predecessors(
            &[link(n[0], n[1]), link(n[0], n[2]), link(n[1], n[2])],
            &anchors(&[n[0]]),
        );
        // n[1] is suppressed only by n[0], not by its equidistant peer.
        assert!(topo.is_suppressed(n[1], &anchors(&[n[0], n[1]])));
        assert!(!topo.is_suppressed(n[1], &anchors(&[n[2], n[1]])));
    }

    #[test]
    fn a_forced_direction_beats_the_derived_one() {
        let n = nodes(3);
        // Distance would make n[1] the parent of n[2] (n[1] is one hop from the anchor, n[2] two).
        // The operator says the reverse — a dual-homed box the poller happens to reach the long way
        // round.
        let topo = predecessors(
            &[link(n[0], n[1]), directed(n[1], n[2], n[2])],
            &anchors(&[n[0]]),
        );
        // The derived direction is gone: n[1] being down no longer explains n[2].
        assert!(!topo.is_suppressed(n[2], &anchors(&[n[1], n[2]])));
        // And the override *adds* an upstream rather than replacing n[1]'s real one, so n[1] stands
        // until both of its parents are down. Suppressing on the operator's edge alone would
        // silence n[1] while the poller still had a working path to it through n[0].
        assert!(!topo.is_suppressed(n[1], &anchors(&[n[2], n[1]])));
        assert!(topo.is_suppressed(n[1], &anchors(&[n[0], n[2], n[1]])));
    }

    #[test]
    fn a_forced_direction_applies_even_where_distance_says_nothing() {
        // Equidistant peers, so the derived rule abstains — an operator's statement is the only
        // thing that can produce a direction here, which is what the override is for.
        let n = nodes(3);
        let topo = predecessors(
            &[
                link(n[0], n[1]),
                link(n[0], n[2]),
                directed(n[1], n[2], n[1]),
            ],
            &anchors(&[n[0]]),
        );
        assert!(topo.is_suppressed(n[2], &anchors(&[n[0], n[1], n[2]])));
        assert!(
            !topo.is_suppressed(n[2], &anchors(&[n[1], n[2]])),
            "n[0] is up, so a path remains"
        );
    }

    #[test]
    fn an_opted_out_node_gets_no_parents_but_still_carries_the_path() {
        // The escape hatch, and the two halves that make it safe. The opted-out node's own alert
        // always stands — that is the whole point. And it stays *in* the graph, so the node behind
        // it is still reachable and still gets a parent; removing it from the topology instead
        // would silently unsuppress everything downstream of it too.
        let n = nodes(3); // anchor → mid (opted out) → leaf
        let opt_out = anchors(&[n[1]]);
        let topo = super::predecessors(
            &[link(n[0], n[1]), link(n[1], n[2])],
            &anchors(&[n[0]]),
            &opt_out,
        );
        let all_down = anchors(&[n[0], n[1], n[2]]);
        assert!(
            !topo.is_suppressed(n[1], &all_down),
            "an opted-out node must always alert on its own"
        );
        assert!(
            topo.is_suppressed(n[2], &all_down),
            "the node behind it is still explained by it"
        );
    }

    #[test]
    fn opting_out_can_only_ever_remove_suppression() {
        // The property that makes this flag safe to hand to an operator where per-edge approval was
        // not: it cannot create the silent-failure class. For every node, suppression with the flag
        // set implies suppression without it.
        let n = nodes(5);
        let links = [
            link(n[0], n[1]),
            link(n[0], n[2]),
            link(n[1], n[3]),
            link(n[2], n[3]),
            link(n[3], n[4]),
        ];
        let down: BTreeSet<NodeId> = n[1..].iter().copied().collect();
        let base = super::predecessors(&links, &anchors(&[n[0]]), &BTreeSet::new());
        for opted in &n {
            let with = super::predecessors(&links, &anchors(&[n[0]]), &anchors(&[*opted]));
            for node in &n {
                assert!(
                    !with.is_suppressed(*node, &down) || base.is_suppressed(*node, &down),
                    "opting {opted} out made {node} suppressed when it was not"
                );
            }
        }
    }

    #[test]
    fn a_cycle_terminates() {
        let n = nodes(3);
        let topo = predecessors(
            &[link(n[0], n[1]), link(n[1], n[2]), link(n[2], n[0])],
            &anchors(&[n[0]]),
        );
        let down = anchors(&[n[0], n[1], n[2]]);
        assert!(topo.root_cause(n[1], &down).is_some());
    }

    #[test]
    fn parents_are_capped_deterministically() {
        // 1 anchor, N routers all one hop from it, one server one hop from every router.
        let n = nodes(MAX_DERIVED_PARENTS + 12);
        let anchor = n[0];
        let server = *n.last().unwrap();
        let mut links = Vec::new();
        for r in &n[1..n.len() - 1] {
            links.push(link(anchor, *r));
            links.push(link(*r, server));
        }
        let first = predecessors(&links, &anchors(&[anchor]));
        links.reverse();
        let second = predecessors(&links, &anchors(&[anchor]));

        // All-down must still suppress, which proves the cap kept a coherent parent set rather
        // than an arbitrary slice that happens to include an up node.
        let all_down: BTreeSet<NodeId> = n[1..].iter().copied().collect();
        assert!(first.is_suppressed(server, &all_down));
        assert_eq!(
            first.root_cause(server, &all_down),
            second.root_cause(server, &all_down),
            "the surviving parents must not depend on link order"
        );
    }

    #[test]
    fn the_projection_is_invariant_under_link_order() {
        let n = nodes(6);
        let mut links = vec![
            link(n[0], n[1]),
            link(n[1], n[2]),
            link(n[1], n[3]),
            link(n[2], n[4]),
            link(n[3], n[4]),
            directed(n[4], n[5], n[4]),
        ];
        let down: BTreeSet<NodeId> = n[1..].iter().copied().collect();
        let a = predecessors(&links, &anchors(&[n[0]]));
        links.reverse();
        let b = predecessors(&links, &anchors(&[n[0]]));
        for node in &n {
            assert_eq!(
                a.is_suppressed(*node, &down),
                b.is_suppressed(*node, &down),
                "suppression of {node} depends on input order"
            );
            assert_eq!(a.root_cause(*node, &down), b.root_cause(*node, &down));
        }
    }
}
