// SPDX-License-Identifier: AGPL-3.0-only
//! Yagra-topology — dependency graph and suppression.
//!
//! Models upstream→downstream dependencies so the alert engine can suppress a child's
//! alert when its upstream is down and roll it up to the real root cause (ADR-015,
//! monitoring-conventions). The nested inventory exists precisely to enable this.
//!
//! Multi-parent rule (ADR-015): a node is only suppressed if **every** parent is down —
//! if any parent is still up, a path remains and the node's own alert stands. Cycles are
//! tolerated via a visited set so a misconfigured graph can never loop forever.

pub mod derive;
pub mod project;

use std::collections::{BTreeMap, BTreeSet};
use yagra_common::NodeId;

/// A dependency graph of nodes. Edges point child → parent(s) (downstream → upstream).
#[derive(Debug, Default, Clone)]
pub struct Topology {
    /// child → its direct parents (upstreams).
    parents: BTreeMap<NodeId, BTreeSet<NodeId>>,
    /// parent → its direct children (downstreams) — the forward index of `parents`, so a node's
    /// whole subtree can be found without scanning the graph. Used to scope a suppression re-sweep
    /// to only the nodes a single down-state flip can actually affect (its descendants).
    children: BTreeMap<NodeId, BTreeSet<NodeId>>,
}

impl Topology {
    /// An empty topology.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Declare that `child` depends on `parent` (parent is upstream of child).
    pub fn add_dependency(&mut self, child: NodeId, parent: NodeId) {
        self.parents.entry(child).or_default().insert(parent);
        self.children.entry(parent).or_default().insert(child);
    }

    /// This node's direct parents. Empty for a root, and for a node the graph has never heard of.
    ///
    /// Exists so ADR-043's shadow preview can compare two graphs edge by edge without either of them
    /// exposing its internals — the comparison is the operator's evidence for enabling derived
    /// suppression, so it has to read the same structure `is_suppressed` does.
    #[must_use]
    pub fn parents_of(&self, node: NodeId) -> BTreeSet<NodeId> {
        self.parents.get(&node).cloned().unwrap_or_default()
    }

    /// This node's direct children. Empty for a leaf, and for a node the graph has never heard of.
    ///
    /// The mirror of [`Self::parents_of`], and deliberately **not** [`Self::descendants`]: a
    /// one-hop neighbourhood is what a diagnostic wants (ADR-022's incident correlation), whereas
    /// the transitive subtree of a core switch is most of the fleet.
    #[must_use]
    pub fn children_of(&self, node: NodeId) -> BTreeSet<NodeId> {
        self.children.get(&node).cloned().unwrap_or_default()
    }

    /// How many child→parent edges the graph holds.
    #[must_use]
    pub fn edge_count(&self) -> usize {
        self.parents.values().map(BTreeSet::len).sum()
    }

    /// All transitive descendants (downstreams) of `node` — every node that has `node` on an
    /// ancestor path. Excludes `node` itself; cycle-safe (each node visited once). A flip of
    /// `node`'s down-state can only change the root-cause attribution of these nodes, so a
    /// suppression re-sweep need only reconsider them (ADR-015) instead of the whole active set.
    #[must_use]
    pub fn descendants(&self, node: NodeId) -> BTreeSet<NodeId> {
        let mut out = BTreeSet::new();
        let mut visited = BTreeSet::new();
        visited.insert(node); // never emit the start node, even inside a cycle
        let mut stack = vec![node];
        while let Some(n) = stack.pop() {
            if let Some(kids) = self.children.get(&n) {
                for &c in kids {
                    if visited.insert(c) {
                        out.insert(c);
                        stack.push(c);
                    }
                }
            }
        }
        out
    }

    /// Whether `node`'s alert should be suppressed given the set of currently-down nodes.
    ///
    /// Suppressed iff the node has at least one parent and **all** parents are down (no
    /// surviving upstream path). A node with no parents is never suppressed.
    #[must_use]
    pub fn is_suppressed(&self, node: NodeId, down: &BTreeSet<NodeId>) -> bool {
        match self.parents.get(&node) {
            Some(parents) if !parents.is_empty() => parents.iter().all(|p| down.contains(p)),
            _ => false,
        }
    }

    /// The root-cause ancestor for a suppressed node: the highest upstream that is down
    /// while no longer being suppressed by something further up (i.e. the top of the
    /// down chain). Returns `None` if the node is not suppressed.
    ///
    /// Cycle-safe: each node is visited at most once.
    #[must_use]
    pub fn root_cause(&self, node: NodeId, down: &BTreeSet<NodeId>) -> Option<NodeId> {
        if !self.is_suppressed(node, down) {
            return None;
        }
        let mut visited = BTreeSet::new();
        Some(self.climb(node, down, &mut visited))
    }

    /// Walk up through down ancestors to the topmost down node that is itself not
    /// suppressed (a root cause). `node` is assumed suppressed on entry.
    fn climb(
        &self,
        node: NodeId,
        down: &BTreeSet<NodeId>,
        visited: &mut BTreeSet<NodeId>,
    ) -> NodeId {
        visited.insert(node);
        // Among this node's down parents, follow one that is itself suppressed (has its
        // own all-down parents). If a down parent is *not* suppressed, it is the root.
        if let Some(parents) = self.parents.get(&node) {
            for &parent in parents {
                if !down.contains(&parent) || visited.contains(&parent) {
                    continue;
                }
                if self.is_suppressed(parent, down) {
                    return self.climb(parent, down, visited);
                }
                // Parent is down but not suppressed → it is the top of the down chain.
                return parent;
            }
        }
        // No further down/unsuppressed ancestor: the node itself is the top.
        node
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn down_set(nodes: &[NodeId]) -> BTreeSet<NodeId> {
        nodes.iter().copied().collect()
    }

    #[test]
    fn root_node_is_never_suppressed() {
        let topo = Topology::new();
        let n = NodeId::new();
        assert!(!topo.is_suppressed(n, &down_set(&[n])));
        assert_eq!(topo.root_cause(n, &down_set(&[n])), None);
    }

    #[test]
    fn child_suppressed_when_sole_parent_down() {
        let (parent, child) = (NodeId::new(), NodeId::new());
        let mut topo = Topology::new();
        topo.add_dependency(child, parent);

        let down = down_set(&[parent, child]);
        assert!(topo.is_suppressed(child, &down));
        assert_eq!(topo.root_cause(child, &down), Some(parent));
    }

    #[test]
    fn child_not_suppressed_if_any_parent_alive() {
        let (p_down, p_up, child) = (NodeId::new(), NodeId::new(), NodeId::new());
        let mut topo = Topology::new();
        topo.add_dependency(child, p_down);
        topo.add_dependency(child, p_up); // redundant upstream still alive

        let down = down_set(&[p_down, child]);
        assert!(!topo.is_suppressed(child, &down)); // p_up survives → stands
        assert_eq!(topo.root_cause(child, &down), None);
    }

    #[test]
    fn root_cause_climbs_to_topmost_down_ancestor() {
        // grandparent → parent → child, all down: root cause is grandparent.
        let (gp, p, c) = (NodeId::new(), NodeId::new(), NodeId::new());
        let mut topo = Topology::new();
        topo.add_dependency(p, gp);
        topo.add_dependency(c, p);

        let down = down_set(&[gp, p, c]);
        assert_eq!(topo.root_cause(c, &down), Some(gp));
    }

    #[test]
    fn root_cause_stops_at_first_up_ancestor() {
        // grandparent UP, parent down, child down → root cause is the parent.
        let (gp, p, c) = (NodeId::new(), NodeId::new(), NodeId::new());
        let mut topo = Topology::new();
        topo.add_dependency(p, gp);
        topo.add_dependency(c, p);

        let down = down_set(&[p, c]); // gp is up
        assert_eq!(topo.root_cause(c, &down), Some(p));
    }

    #[test]
    fn descendants_are_transitive_and_exclude_self() {
        // gp → p → {c1, c2}; descendants(gp) is the whole subtree below gp.
        let (gp, p, c1, c2) = (NodeId::new(), NodeId::new(), NodeId::new(), NodeId::new());
        let mut topo = Topology::new();
        topo.add_dependency(p, gp);
        topo.add_dependency(c1, p);
        topo.add_dependency(c2, p);

        assert_eq!(topo.descendants(gp), down_set(&[p, c1, c2]));
        assert_eq!(topo.descendants(p), down_set(&[c1, c2]));
        assert!(topo.descendants(c1).is_empty()); // a leaf has no descendants
        assert_eq!(topo.descendants(NodeId::new()), down_set(&[])); // unknown node
    }

    #[test]
    fn descendants_are_cycle_safe() {
        // a → b → a (misconfiguration). Must terminate; neither includes itself as the start.
        let (a, b) = (NodeId::new(), NodeId::new());
        let mut topo = Topology::new();
        topo.add_dependency(a, b);
        topo.add_dependency(b, a);
        assert_eq!(topo.descendants(a), down_set(&[b]));
        assert_eq!(topo.descendants(b), down_set(&[a]));
    }

    /// `children_of` is the mirror of `parents_of` and must stay **one hop**, unlike `descendants`.
    /// The distinction is the whole reason it exists: ADR-022's incident correlation wants a node's
    /// immediate neighbourhood, and the transitive subtree of a core switch is most of the fleet.
    #[test]
    fn children_of_mirrors_parents_of_and_stays_one_hop() {
        let (gp, p, c1, c2) = (NodeId::new(), NodeId::new(), NodeId::new(), NodeId::new());
        let mut topo = Topology::new();
        topo.add_dependency(p, gp);
        topo.add_dependency(c1, p);
        topo.add_dependency(c2, p);

        assert_eq!(topo.children_of(p), down_set(&[c1, c2]));
        assert_eq!(topo.parents_of(p), down_set(&[gp]));
        // One hop: gp's children are {p}, not {p, c1, c2} — that is what `descendants` is for.
        assert_eq!(topo.children_of(gp), down_set(&[p]));
        assert_eq!(topo.descendants(gp), down_set(&[p, c1, c2]));
        // A leaf and an unknown node both answer empty rather than panicking.
        assert!(topo.children_of(c1).is_empty());
        assert!(topo.children_of(NodeId::new()).is_empty());
    }

    #[test]
    fn children_of_is_cycle_safe() {
        // a → b → a: one hop each way, and no traversal to loop on.
        let (a, b) = (NodeId::new(), NodeId::new());
        let mut topo = Topology::new();
        topo.add_dependency(a, b);
        topo.add_dependency(b, a);
        assert_eq!(topo.children_of(a), down_set(&[b]));
        assert_eq!(topo.children_of(b), down_set(&[a]));
    }

    #[test]
    fn cycle_is_tolerated() {
        // a → b → a (misconfiguration). Both down. Must terminate.
        let (a, b) = (NodeId::new(), NodeId::new());
        let mut topo = Topology::new();
        topo.add_dependency(a, b);
        topo.add_dependency(b, a);

        let down = down_set(&[a, b]);
        // Both are "suppressed" by each other; root_cause must return *something* and not loop.
        let rc = topo.root_cause(a, &down);
        assert!(rc == Some(a) || rc == Some(b));
    }
}
