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
    let links = match topo.links.all_links().await {
        Ok(l) => l,
        Err(e) => {
            tracing::warn!(error = %e, "dependency projection: reading derived links failed");
            return (Topology::new(), resolution);
        }
    };
    let topology = project::predecessors(&links, &resolution.anchors);
    (topology, resolution)
}

/// Resolve the anchor set alone, for callers that only need to know whether the pollers are placed
/// (the mode switch's precondition, and the banner the UI shows while they are not).
pub async fn resolve(topo: &TopologySources, nodes: &[yagra_common::Node]) -> AnchorResolution {
    let l3_rows = topo.l3.all_current().await.unwrap_or_else(|e| {
        tracing::warn!(error = %e, "anchor resolution: reading interface addresses failed");
        Vec::new()
    });
    let known: BTreeSet<NodeId> = nodes.iter().map(|n| n.id).collect();
    let resolution = project::resolve_anchors(&poller_locations(topo).await, &l3_rows, &known);
    for (pool, ids) in &resolution.unresolved {
        metrics::gauge!("yagra_topology_anchor_unresolved", "pool" => pool.clone())
            .set(ids.len() as f64);
    }
    metrics::gauge!("yagra_topology_anchors_total").set(resolution.anchors.len() as f64);
    resolution
}

/// Every poller's recorded location.
async fn poller_locations(topo: &TopologySources) -> Vec<PollerLocation> {
    topo.pollers
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
