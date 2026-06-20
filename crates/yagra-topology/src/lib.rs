//! Yagra-topology — dependency graph and suppression.
//!
//! Models upstream→downstream dependencies so the alert engine can suppress a child's
//! alert when its upstream is down and roll it up to the real root cause (ADR-015,
//! monitoring-conventions). The nested inventory exists precisely to enable this.
//!
//! Multi-parent rule (ADR-015): a node is only suppressed if **every** parent is down —
//! if any parent is still up, a path remains and the node's own alert stands. Cycles are
//! tolerated via a visited set so a misconfigured graph can never loop forever.

use std::collections::{BTreeMap, BTreeSet};
use yagra_common::NodeId;

/// A dependency graph of nodes. Edges point child → parent(s) (downstream → upstream).
#[derive(Debug, Default, Clone)]
pub struct Topology {
    /// child → its direct parents (upstreams).
    parents: BTreeMap<NodeId, BTreeSet<NodeId>>,
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
