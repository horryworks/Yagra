// SPDX-License-Identifier: AGPL-3.0-only
//! Turning stored links into the dependency graph (ADR-043 I2).
//!
//! Two callers want exactly this and must not diverge:
//!
//! * the alert-config reloader, which uses the result to suppress with (`derived` mode);
//! * the shadow endpoint, which uses it to show an operator what *would* be suppressed.
//!
//! If those two built the graph differently, the review an operator does in shadow would not be a
//! review of what happens when they flip the switch — which is the entire mechanism ADR-043 決定 5
//! introduced to stop a wrong edge silencing a real outage. So there is one builder, here, and both
//! call it.
//!
//! The projection itself is pure and lives in `yagra_topology::project`. This module is only the
//! I/O: read the links, read where the pollers are, hand both over.

use crate::l3::L3Repo;
use crate::pollers::PollerRepo;
use crate::topology_links::TopoLinkRepo;
use std::collections::BTreeSet;
use std::sync::Arc;
use yagra_common::NodeId;
use yagra_topology::project::{self, AnchorResolution, PollerLocation};
use yagra_topology::Topology;

/// The stores the projection reads.
///
/// Grouped rather than passed as three arguments so the functions that take it keep a short
/// signature — `clippy::too_many_arguments` on the caller would be the parameters asking to become
/// this struct anyway.
#[derive(Clone)]
pub struct TopologySources {
    /// The derived connectivity graph (a cache).
    pub links: Arc<TopoLinkRepo>,
    /// The durable poller inventory, which is where the anchors come from.
    pub pollers: Arc<PollerRepo>,
    /// Interface addresses, used to place a poller on a segment.
    pub l3: Arc<L3Repo>,
    /// The inventory, for the per-node suppression opt-out (ADR-043 Increment 3).
    pub nodes: Arc<crate::repo::NodeRepo>,
}

/// Project the stored links into a dependency graph, and report how the anchors resolved.
///
/// Degrades to an **empty** graph on a failed link read. Empty means nothing is suppressed, which is
/// the noisy direction; carrying a partial graph forward would silence alerts on the strength of a
/// failed query. The empty case is also what an unresolved anchor produces — which is why moving to
/// `derived` is blocked while any pool has one, rather than left to look like it worked.
pub async fn derived_topology(
    topo: &TopologySources,
    nodes: &[yagra_common::Node],
) -> (Topology, AnchorResolution) {
    let resolution = resolve(topo, nodes).await;
    let Some(links) = read_links(&topo.links).await else {
        return (Topology::new(), resolution);
    };
    let opt_out = topo.nodes.suppression_opt_outs().await;
    let topology = project::predecessors(&links, &resolution.anchors, &opt_out);
    (topology, resolution)
}

/// Every derived link, or `None` when the read failed.
///
/// Split out so the degrade decision is reachable from a test: the caller above turns `None` into an
/// empty graph, and an empty graph suppresses nothing. Inlined, the only way to exercise the branch
/// that decides whether alerts stay noisy or go silent would be a live database that is down.
async fn read_links(links: &TopoLinkRepo) -> Option<Vec<yagra_common::DerivedLink>> {
    match links.all_links().await {
        Ok(l) => Some(l),
        Err(e) => {
            tracing::warn!(error = %e, "dependency projection: reading derived links failed");
            None
        }
    }
}

/// Resolve the anchor set alone, for callers that only need to know whether the pollers are placed
/// (the mode switch's precondition, and the banner the UI shows while they are not).
pub async fn resolve(topo: &TopologySources, nodes: &[yagra_common::Node]) -> AnchorResolution {
    resolve_from(&topo.l3, &topo.pollers, nodes).await
}

/// [`resolve`] against the two stores it actually reads.
///
/// Takes them individually rather than the whole [`TopologySources`] so a test can hand it two
/// repos pointed at a dead database and check what a failed read degrades to — the struct also
/// carries a [`crate::repo::NodeRepo`], which this path never touches.
async fn resolve_from(
    l3: &L3Repo,
    pollers: &PollerRepo,
    nodes: &[yagra_common::Node],
) -> AnchorResolution {
    let l3_rows = l3.all_current().await.unwrap_or_else(|e| {
        tracing::warn!(error = %e, "anchor resolution: reading interface addresses failed");
        Vec::new()
    });
    let known: BTreeSet<NodeId> = nodes.iter().map(|n| n.id).collect();
    let resolution = project::resolve_anchors(&poller_locations(pollers).await, &l3_rows, &known);
    for (pool, ids) in &resolution.unresolved {
        metrics::gauge!("yagra_topology_anchor_unresolved", "pool" => pool.clone())
            .set(ids.len() as f64);
    }
    metrics::gauge!("yagra_topology_anchors_total").set(resolution.anchors.len() as f64);
    resolution
}

/// Every poller's recorded location.
async fn poller_locations(pollers: &PollerRepo) -> Vec<PollerLocation> {
    pollers
        .list()
        .await
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "anchor resolution: reading poller inventory failed");
            Vec::new()
        })
        .into_iter()
        .map(|p| PollerLocation {
            poller_id: p.id,
            pool: p.pool,
            // Addresses come back through `host()`, so anything that fails to parse is a corrupt
            // row rather than an ordinary CIDR. Dropped rather than guessed at — a mis-parsed
            // address would place the poller on a segment it is not on, and every parent derived
            // from that anchor would be wrong.
            mgmt_addrs: p
                .mgmt_addrs
                .iter()
                .filter_map(|a| a.parse::<std::net::IpAddr>().ok())
                .collect(),
            anchor_node_id: p.anchor_node_id.map(NodeId),
        })
        .collect()
}

/// The manual dependency graph — each node's hand-authored parent (ADR-015).
///
/// Here rather than inline in the reloader because the shadow endpoint compares against it, and a
/// second transcription of "child → parent" is exactly the kind of duplicate that drifts into
/// showing an operator a diff against a graph the engine does not use.
#[must_use]
pub fn manual_topology(nodes: &[yagra_common::Node]) -> Topology {
    let mut topo = Topology::new();
    for node in nodes {
        if let Some(parent) = node.parent {
            topo.add_dependency(node.id, parent);
        }
    }
    topo
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use yagra_common::{DerivedLink, LinkSource, Node};

    /// A pool that will never connect, so every query through it fails.
    ///
    /// This is the whole point of the fixture: the branches below are the *degrade* paths, and the
    /// only other way to reach them is a live database that is down — which is exactly when nobody
    /// is running tests. `connect_lazy` builds the handle without touching the network, and the
    /// short acquire timeout keeps the failure to a fraction of a second.
    fn dead_pool() -> sqlx::PgPool {
        sqlx::postgres::PgPoolOptions::new()
            .acquire_timeout(Duration::from_millis(200))
            .connect_lazy("postgres://127.0.0.1:1/yagra-test-no-such-db")
            .expect("lazy pool")
    }

    fn node_id(n: u128) -> NodeId {
        NodeId(uuid::Uuid::from_u128(n))
    }

    fn node(n: u128) -> Node {
        Node::new(
            node_id(n),
            format!("n{n}"),
            std::net::IpAddr::V4(std::net::Ipv4Addr::new(10, 0, 0, n as u8)),
        )
    }

    #[tokio::test]
    async fn a_failed_link_read_degrades_to_a_graph_that_suppresses_nothing() {
        assert!(
            read_links(&TopoLinkRepo::new(dead_pool())).await.is_none(),
            "a failed link read must report failure, not an empty link list — the caller's \
             substitution is a deliberate decision, not a coincidence of shape"
        );
        // And what the caller substitutes for it suppresses nobody. This is the assertion that
        // stops the degraded value drifting into something that silences alerts on the strength of
        // a failed query: with no edges, a down neighbour is never anyone's root cause.
        let empty = Topology::new();
        let down: BTreeSet<NodeId> = [node_id(1)].into_iter().collect();
        assert_eq!(empty.edge_count(), 0);
        assert!(!empty.is_suppressed(node_id(2), &down));
        assert_eq!(empty.root_cause(node_id(2), &down), None);
    }

    #[tokio::test]
    async fn failed_store_reads_resolve_no_anchors_at_all() {
        // Both reads inside `resolve_from` fail, so both `unwrap_or_else` fallbacks run.
        let resolution = resolve_from(
            &L3Repo::new(dead_pool()),
            &PollerRepo::new(dead_pool()),
            &[node(1), node(2)],
        )
        .await;
        assert!(resolution.anchors.is_empty());
        // `unresolved` is empty too, and that is not a claim that everything resolved: no poller
        // was read, so no pool was even considered. It stays safe because zero anchors means zero
        // edges below — the graph suppresses nothing, which is the noisy direction.
        assert!(resolution.unresolved.is_empty());
        assert_eq!(resolution.truncated_pollers, 0);
        assert!(project::predecessors(
            &[DerivedLink::new(
                node_id(1),
                node_id(2),
                LinkSource::L3Subnet
            )],
            &resolution.anchors,
            &BTreeSet::new(),
        )
        .parents_of(node_id(2))
        .is_empty());
    }

    #[test]
    fn a_failed_address_read_leaves_pollers_unresolved_rather_than_anchored() {
        // The dangerous half of the L3 fallback: pollers *were* read, their addresses were not. An
        // empty address list must leave every un-named poller unplaced — reporting it as anchored
        // would hand `predecessors` a plausible root and suppress real outages under it. Non-empty
        // `unresolved` is what blocks the switch into derived mode.
        let pollers = vec![PollerLocation {
            poller_id: "poller-a".to_owned(),
            pool: "default".to_owned(),
            mgmt_addrs: vec![std::net::IpAddr::V4(std::net::Ipv4Addr::new(10, 0, 0, 9))],
            anchor_node_id: None,
        }];
        let known: BTreeSet<NodeId> = [node_id(1)].into_iter().collect();
        let resolution = project::resolve_anchors(&pollers, &[], &known);
        assert!(resolution.anchors.is_empty());
        assert_eq!(
            resolution.unresolved.get("default"),
            Some(&vec!["poller-a".to_owned()])
        );
    }

    #[test]
    fn the_manual_graph_carries_exactly_the_authored_parents() {
        let mut child = node(2);
        child.parent = Some(node_id(1));
        let topo = manual_topology(&[node(1), child]);
        assert_eq!(topo.edge_count(), 1);
        assert!(topo.parents_of(node_id(2)).contains(&node_id(1)));
        assert!(topo.parents_of(node_id(1)).is_empty());
    }
}
