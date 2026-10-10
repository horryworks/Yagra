// SPDX-License-Identifier: AGPL-3.0-only
//! The one answer to "what state do we show for this node?" when the alert engine has none yet.
//!
//! The engine's opinion wins whenever it has one; otherwise a recent liveness sample means `ok` and
//! silence means `unknown`. The REST handlers (`api/nodes.rs`, `api/fleet.rs`, `api/topology.rs`)
//! and the inventory report (`reports/sections.rs`) all apply that rule, so it lives below both of
//! them rather than inside the API layer (ADR-202 Inc.3) — a report must not reach up into `api/`
//! to say how a node is doing.

use std::collections::HashSet;

use uuid::Uuid;
use yagra_common::{NodeKind, NodeState};

/// Freshness window for the coarse fallback probe: a node with a liveness sample within this
/// window is treated as `ok`, else `unknown` (matches the fleet-coverage staleness horizon).
///
/// ⚠️ **One number with the floor of an AP's report window** (ADR-064 Inc.G): the engine shows an
/// AP whose controller has not reported it within that window as `unknown`, and after a restart
/// this fallback is what decides the same AP. Two numbers would make one outage read differently
/// depending on whether core had restarted.
pub(crate) const FALLBACK_FRESH_SECS: u64 = crate::alerts::reported::FRESH_FLOOR_SECS;

/// The metrics the fallback probe asks about: **every node kind's liveness series**, because a URL
/// monitor, a DNS monitor and a Meraki device are never pinged and so have no `icmp_rtt_ms` at all.
/// Asking only about ICMP made those three kinds fall to `unknown` whenever the engine had no
/// opinion yet — the same defect that made fleet coverage report them as silent (ADR-059).
///
/// The union answers without resolving each node's kind, which would put three database reads on
/// the node-list path for an answer that is identical either way.
pub(crate) const FALLBACK_METRICS: [&str; NodeKind::ALL.len()] = NodeKind::LIVENESS_METRICS;

/// **The display rule itself**: the engine's opinion when it has one, otherwise a recent liveness
/// sample means `ok` and silence means `unknown`.
///
/// "The engine's opinion" already accounts for a wireless AP its controller has stopped reporting:
/// the engine hands back `unknown` for its stale `ok` (ADR-064 Inc.G), so no caller here needs to
/// know which nodes are APs.
///
/// Pure — every caller brings its own already-batched inputs, and nothing here does I/O. It is a
/// function rather than three lines because it *was* three lines, four times over: the topology
/// graph, the fleet tally, the per-group rollup and the inventory report each restated it, and two
/// of them had dropped the fallback entirely. The visible symptom was a core restart making the
/// dashboard summary report `unknown` for nodes the Nodes page was simultaneously showing as `ok`.
pub(crate) fn state_or_fallback(known: Option<NodeState>, fresh: bool) -> NodeState {
    match known {
        Some(s) => s,
        None if fresh => NodeState::Ok,
        None => NodeState::Unknown,
    }
}

/// The **whole fleet's** fresh set, for the rollups that hold counts rather than a page of ids.
///
/// `api::nodes::fresh_fallback_ids` pushes its id set into the query selector, which is right for a
/// page and wrong for a rollup: the fleet tally, the per-group summary and the inventory report each
/// cover every visible node, and a selector carrying 50,000 UUIDs is not a query. So they share one
/// unscoped freshness query instead — and every caller skips it entirely unless something is
/// actually unobserved, which is the steady state (the engine holds an opinion about every node it
/// has swept). Takes the store rather than `ApiState` because the report renderer has no `ApiState`.
pub(crate) async fn fresh_fleet_ids(store: &dyn crate::store::MetricStore) -> HashSet<Uuid> {
    store
        .fresh_node_ids(&FALLBACK_METRICS, FALLBACK_FRESH_SECS)
        .await
        .into_iter()
        .collect()
}
