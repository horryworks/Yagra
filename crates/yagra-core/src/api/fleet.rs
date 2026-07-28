// SPDX-License-Identifier: AGPL-3.0-only
//! Fleet-wide rollups: the status summary, the per-group tally, data coverage, and the state
//! timeline.
//!
//! Every endpoint here exists because the client used to compute the same thing from a **paged**
//! node slice, which silently under-counted everything past the first page (S12) — a correctness
//! bug, not just a scale one. So the shape of this module is: aggregate server-side, over the whole
//! inventory, and return all the keys every time.
//!
//! "All the keys every time" is load-bearing. A tally that omits a zero forces every client to
//! special-case an absent state, and the two surfaces here had already drifted on exactly that:
//! REST pre-seeded all six states, MCP returned only the observed ones. [`state_tally`] is now the
//! single answer, and it is keyed off [`NodeState::ALL`] rather than a hand-written list.

use super::extract::{Admin, RequireView};
use super::{ApiError, ApiResult, ApiState};
use axum::{
    extract::{Query, State},
    routing::get,
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use uuid::Uuid;
use yagra_common::{NodeId, NodeState};

/// The fleet routes, merged into `/api/v1` by [`super::router`].
pub(crate) fn routes() -> Router<ApiState> {
    Router::new()
        .route("/api/v1/fleet/summary", get(fleet_summary))
        .route("/api/v1/fleet/group-summary", get(fleet_group_summary))
        .route("/api/v1/fleet/coverage", get(fleet_coverage))
        .route("/api/v1/fleet/state-history", get(fleet_state_history))
}

// ── Fleet status summary ─────────────────────────────────────────────────────

/// Total node count plus a per-state tally.
///
/// The `states` keys are **always all six** and always sum to `total`, so a client can index them
/// blind. Shared with the MCP `get_fleet_summary` tool, which used to build its own version that
/// omitted zeroes.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct FleetSummary {
    pub total: i64,
    pub states: BTreeMap<&'static str, i64>,
}

/// Tally the fleet by rolled-up display state.
///
/// Nodes the alert engine has never observed — brand new, or in a pool with no live poller — are
/// counted as `unknown`. That matches the per-node list's fallback, and it is what makes the tally
/// reconcile with the inventory `total` instead of quietly summing to less.
pub(crate) async fn state_tally(st: &ApiState) -> FleetSummary {
    let total = st.nodes.count().await.unwrap_or_else(|e| {
        tracing::warn!(error = %e, "fleet summary node count failed");
        0
    });
    let mut states: BTreeMap<&'static str, i64> =
        NodeState::ALL.iter().map(|s| (s.as_str(), 0)).collect();
    let mut observed_total: i64 = 0;
    for (state, n) in st.alerts.node_state_counts() {
        let n = n as i64;
        *states.entry(state.as_str()).or_insert(0) += n;
        observed_total += n;
    }
    let unobserved = (total - observed_total).max(0);
    *states.entry(NodeState::Unknown.as_str()).or_insert(0) += unobserved;
    FleetSummary { total, states }
}

/// Fleet-wide status summary, computed server-side from the live alert engine (S12).
///
/// View-gated and works without the admin store, so a public dashboard can render its
/// status-summary / health-ring / nodes-down widgets.
async fn fleet_summary(_perm: RequireView, State(st): State<ApiState>) -> Json<FleetSummary> {
    Json(state_tally(&st).await)
}

// ── Per-group rollup ─────────────────────────────────────────────────────────

/// Per-group direct-member state tally. All six keys are always present (a missing state is `0`)
/// and they sum to the group's direct-member count.
#[derive(Serialize, Default, Debug, PartialEq)]
pub(crate) struct GroupStateCounts {
    pub ok: i64,
    pub warning: i64,
    pub critical: i64,
    pub unknown: i64,
    pub unreachable: i64,
    pub maintenance: i64,
}

/// The per-group rollup response: `group_id → direct-member state counts`.
#[derive(Serialize)]
pub(crate) struct FleetGroupSummary {
    pub groups: HashMap<Uuid, GroupStateCounts>,
}

/// Roll a node→group map + the engine's per-node states into per-group **direct-member** tallies.
///
/// Pure (no I/O) so the grouping and fallback rules are unit-testable. Ungrouped nodes (null
/// `group_id`) are skipped — no widget rolls them up. A node the engine has never observed counts
/// as `unknown`, matching the per-node list's fallback so a group's tally reconciles with its size.
pub(crate) fn aggregate_group_counts(
    node_groups: &[(Uuid, Option<Uuid>)],
    states: &HashMap<NodeId, NodeState>,
) -> HashMap<Uuid, GroupStateCounts> {
    let mut groups: HashMap<Uuid, GroupStateCounts> = HashMap::new();
    for (id, group_id) in node_groups {
        let Some(gid) = group_id else { continue };
        let state = states
            .get(&NodeId::from(*id))
            .copied()
            .unwrap_or(NodeState::Unknown);
        let c = groups.entry(*gid).or_default();
        match state {
            NodeState::Ok => c.ok += 1,
            NodeState::Warning => c.warning += 1,
            NodeState::Critical => c.critical += 1,
            NodeState::Unknown => c.unknown += 1,
            NodeState::Unreachable => c.unreachable += 1,
            NodeState::Maintenance => c.maintenance += 1,
        }
    }
    groups
}

/// Per-group health rollup for the site-matrix / region-rollup / geo-map widgets.
///
/// View-gated and works without the admin store — the node→group map comes from the shared
/// `NodeListing`. The client joins these counts to the bounded group tree for names and geo, and
/// sums descendants for the region rollup.
async fn fleet_group_summary(
    _perm: RequireView,
    State(st): State<ApiState>,
) -> ApiResult<Json<FleetGroupSummary>> {
    let node_groups = st.nodes.node_group_map().await.map_err(|e| {
        ApiError::from_internal(
            e.as_ref(),
            "fleet group summary node map",
            "failed to load group summary",
        )
    })?;
    let states = st.alerts.node_states();
    Ok(Json(FleetGroupSummary {
        groups: aggregate_group_counts(&node_groups, &states),
    }))
}

// ── Data coverage (the blind-spot detector) ──────────────────────────────────

/// How recent a node's last ICMP sample must be to count as "fresh" (silent beyond this ⇒ stale).
const COVERAGE_FRESH_SECS: u64 = 600;
/// Cap on the returned watchlist. A fleet that is 90% stale has a systemic problem, not 40,000
/// individual ones, so the first 50 names are as much as a human can act on.
const STALE_WATCHLIST_MAX: usize = 50;

/// A node returning no fresh data (silent failure / blind spot).
#[derive(Debug, Clone, Serialize)]
pub(crate) struct StaleNode {
    pub node_id: Uuid,
    pub name: String,
}

/// Fleet data-coverage summary: fresh vs total nodes + the stale watchlist.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct FleetCoverage {
    pub total: usize,
    pub fresh: usize,
    /// Percent of nodes reporting fresh data (100 when the inventory is empty).
    pub coverage_pct: i64,
    /// The stale nodes by name, capped at [`STALE_WATCHLIST_MAX`]; `total - fresh` is the real
    /// count, so a truncated list is still legible against the totals above it.
    pub stale: Vec<StaleNode>,
}

/// Split an inventory against the set of nodes with fresh data. Pure, so the percentage rule and
/// the empty-fleet case are testable without a store.
fn coverage_of(nodes: Vec<yagra_common::Node>, fresh_ids: &HashSet<Uuid>) -> FleetCoverage {
    let total = nodes.len();
    let mut fresh = 0usize;
    let mut stale: Vec<StaleNode> = Vec::new();
    for n in nodes {
        if fresh_ids.contains(&n.id.as_uuid()) {
            fresh += 1;
        } else {
            stale.push(StaleNode {
                node_id: n.id.as_uuid(),
                name: n.name,
            });
        }
    }
    // An empty fleet is 100% covered, not 0% — there is nothing being missed. Reporting 0 would
    // light up the blind-spot widget on every fresh installation.
    let coverage_pct = if total > 0 {
        ((fresh as f64 / total as f64) * 100.0).round() as i64
    } else {
        100
    };
    stale.sort_by(|a, b| a.name.cmp(&b.name));
    stale.truncate(STALE_WATCHLIST_MAX);
    FleetCoverage {
        total,
        fresh,
        coverage_pct,
        stale,
    }
}

/// Which nodes have (not) reported ICMP within the freshness window — low coverage means the
/// monitoring itself is missing data. Admin-only data source (full inventory).
async fn fleet_coverage(
    _perm: RequireView,
    admin: Admin,
    State(st): State<ApiState>,
) -> ApiResult<Json<FleetCoverage>> {
    let nodes = admin.repo.list_nodes().await.map_err(|e| {
        ApiError::from_internal(
            e.as_ref(),
            "fleet coverage list nodes",
            "failed to load fleet coverage",
        )
    })?;
    let fresh_ids: HashSet<Uuid> = st
        .store
        .fresh_node_ids("icmp_rtt_ms", COVERAGE_FRESH_SECS)
        .await
        .into_iter()
        .collect();
    Ok(Json(coverage_of(nodes, &fresh_ids)))
}

// ── State timeline ───────────────────────────────────────────────────────────

/// Query for the fleet state-history timeline: `?from=&to=` Unix seconds (default last 24h).
#[derive(Deserialize)]
struct StateHistoryQuery {
    from: Option<i64>,
    to: Option<i64>,
}

/// Cap on the requested history window — defence in depth on top of snapshot retention, so a
/// client cannot ask for an unboundedly large scan.
const MAX_HISTORY_SECS: i64 = 90 * 24 * 3600;

/// Node-state counts over time, pivoted into per-state series on a shared timestamp axis.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct FleetStateHistory {
    pub timestamps: Vec<i64>,
    /// One aligned series per state. Keyed off [`NodeState::ALL`], so every state is present with
    /// zeroes even if it never occurred in the window — the chart's series set must not change
    /// shape depending on what happened to the fleet.
    pub series: BTreeMap<&'static str, Vec<i64>>,
}

/// Pivot `(ts, state, count)` rows into aligned per-state series. Pure, and the reason it is worth
/// separating: the alignment is the part that can be silently wrong.
fn pivot_state_history(rows: Vec<(i64, String, i64)>) -> FleetStateHistory {
    let mut timestamps: Vec<i64> = Vec::new();
    let mut ts_index: HashMap<i64, usize> = HashMap::new();
    for (t, _, _) in &rows {
        if !ts_index.contains_key(t) {
            ts_index.insert(*t, timestamps.len());
            timestamps.push(*t);
        }
    }
    let mut series: BTreeMap<&'static str, Vec<i64>> = NodeState::ALL
        .iter()
        .map(|s| (s.as_str(), vec![0i64; timestamps.len()]))
        .collect();
    for (t, state, count) in rows {
        // An unrecognised state is dropped rather than added as a new series: the client's chart
        // legend is built from a fixed set, and a stray key would render unlabelled.
        if let (Some(&i), Some(arr)) = (ts_index.get(&t), series.get_mut(state.as_str())) {
            arr[i] = count;
        }
    }
    FleetStateHistory { timestamps, series }
}

/// The fleet health timeline (stacked/line chart). Admin-only data source.
async fn fleet_state_history(
    _perm: RequireView,
    admin: Admin,
    Query(q): Query<StateHistoryQuery>,
) -> ApiResult<Json<FleetStateHistory>> {
    let to = q.to.unwrap_or_else(super::now_unix_s);
    let from = q.from.unwrap_or(to - 24 * 3600);
    if to < from || to - from > MAX_HISTORY_SECS {
        return Err(ApiError::bad_request(
            "invalid_range",
            "from must be <= to and the window must not exceed 90 days",
        ));
    }
    let rows = admin.repo.state_history(from, to).await.map_err(|e| {
        ApiError::from_internal(
            e.as_ref(),
            "fleet state history",
            "failed to load state history",
        )
    })?;
    Ok(Json(pivot_state_history(rows)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::router;
    use crate::api::tests_support::{private_state, public_state};
    use axum::body::{to_bytes, Body};
    use axum::http::{Request, StatusCode};
    use std::net::Ipv4Addr;
    use tower::ServiceExt;
    use yagra_common::Node;

    fn node(name: &str, last_octet: u8) -> Node {
        Node::new(
            NodeId::from(Uuid::new_v4()),
            name,
            Ipv4Addr::new(10, 0, 0, last_octet).into(),
        )
    }

    #[test]
    fn a_tally_carries_every_state_even_at_zero() {
        // The whole point of aggregating server-side: a client indexes `states["maintenance"]`
        // without checking. The MCP surface used to omit zeroes, which is the drift this closes.
        let summary = FleetSummary {
            total: 0,
            states: NodeState::ALL.iter().map(|s| (s.as_str(), 0)).collect(),
        };
        let json = serde_json::to_value(&summary).unwrap();
        for s in NodeState::ALL {
            assert!(
                json["states"][s.as_str()].is_i64(),
                "{} missing from the tally",
                s.as_str()
            );
        }
    }

    #[test]
    fn aggregate_group_counts_tallies_direct_members_with_unknown_fallback() {
        let g1 = Uuid::from_u128(1);
        let g2 = Uuid::from_u128(2);
        let n_ok = Uuid::from_u128(10);
        let n_warn = Uuid::from_u128(11);
        let n_crit = Uuid::from_u128(12);
        let n_never = Uuid::from_u128(13); // engine never observed it → unknown fallback
        let n_ungrouped = Uuid::from_u128(14); // null group_id → contributes to no group

        let node_groups = vec![
            (n_ok, Some(g1)),
            (n_warn, Some(g1)),
            (n_crit, Some(g2)),
            (n_never, Some(g2)),
            (n_ungrouped, None),
        ];
        let mut states = HashMap::new();
        states.insert(NodeId::from(n_ok), NodeState::Ok);
        states.insert(NodeId::from(n_warn), NodeState::Warning);
        states.insert(NodeId::from(n_crit), NodeState::Critical);
        // n_never intentionally absent from `states`.

        let out = aggregate_group_counts(&node_groups, &states);

        // The ungrouped node rolls up into no group.
        assert_eq!(out.len(), 2);
        let c1 = &out[&g1];
        assert_eq!((c1.ok, c1.warning, c1.critical), (1, 1, 0));
        let c2 = &out[&g2];
        assert_eq!(c2.critical, 1);
        assert_eq!(c2.unknown, 1); // never-observed member counts as unknown
                                   // Each group's tally reconciles with its direct-member count.
        let total = |c: &GroupStateCounts| {
            c.ok + c.warning + c.critical + c.unknown + c.unreachable + c.maintenance
        };
        assert_eq!(total(c1), 2);
        assert_eq!(total(c2), 2);
    }

    #[test]
    fn an_empty_fleet_is_fully_covered_not_zero_covered() {
        // 0/0 is 100%, not 0%. Reporting 0 would light the blind-spot widget on every fresh
        // installation, training operators to ignore it.
        let cov = coverage_of(Vec::new(), &HashSet::new());
        assert_eq!(cov.coverage_pct, 100);
        assert_eq!((cov.total, cov.fresh), (0, 0));
        assert!(cov.stale.is_empty());
    }

    #[test]
    fn coverage_splits_fresh_from_stale_and_bounds_the_watchlist() {
        let nodes: Vec<Node> = (0..60u8).map(|i| node(&format!("n{i:02}"), i)).collect();
        // Only the first ten have reported.
        let fresh_ids: HashSet<Uuid> = nodes.iter().take(10).map(|n| n.id.as_uuid()).collect();
        let cov = coverage_of(nodes, &fresh_ids);
        assert_eq!((cov.total, cov.fresh), (60, 10));
        assert_eq!(cov.coverage_pct, 17); // 16.67 rounds to 17
                                          // The list is capped but the totals above it are not, so the truncation stays legible.
        assert_eq!(cov.stale.len(), STALE_WATCHLIST_MAX);
        assert_eq!(cov.total - cov.fresh, 50);
        // Sorted by name so the watchlist is stable between polls rather than reshuffling.
        let names: Vec<&str> = cov.stale.iter().map(|s| s.name.as_str()).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(names, sorted);
    }

    #[test]
    fn the_timeline_has_one_series_per_state_regardless_of_what_happened() {
        // A chart whose series set depends on the data cannot keep a stable legend or colour
        // mapping — so every state gets an aligned, zero-filled array even if it never occurred.
        let hist = pivot_state_history(vec![
            (100, "ok".to_owned(), 5),
            (100, "critical".to_owned(), 1),
            (200, "ok".to_owned(), 6),
        ]);
        assert_eq!(hist.timestamps, vec![100, 200]);
        assert_eq!(hist.series.len(), NodeState::ALL.len());
        for arr in hist.series.values() {
            assert_eq!(arr.len(), hist.timestamps.len(), "series must stay aligned");
        }
        assert_eq!(hist.series["ok"], vec![5, 6]);
        assert_eq!(hist.series["critical"], vec![1, 0]);
        assert_eq!(hist.series["maintenance"], vec![0, 0]);
    }

    #[test]
    fn an_unknown_state_from_the_store_is_dropped_not_added_as_a_series() {
        // Forward compatibility in the safe direction: a row this build does not know about must
        // not appear as an unlabelled series in the client's legend.
        let hist = pivot_state_history(vec![
            (100, "ok".to_owned(), 5),
            (100, "quarantined".to_owned(), 3),
        ]);
        assert_eq!(hist.series.len(), NodeState::ALL.len());
        assert!(!hist.series.contains_key("quarantined"));
    }

    #[tokio::test]
    async fn the_summary_serves_a_public_dashboard_but_coverage_needs_the_inventory() {
        // The split that matters for a public deployment: the rollup widgets work without the
        // admin store, the blind-spot detector does not (it reads the full inventory).
        let app = router(public_state());
        let summary = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/fleet/summary")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(summary.status(), StatusCode::OK);
        let bytes = to_bytes(summary.into_body(), 64 * 1024).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["states"]["maintenance"], 0);

        let coverage = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/fleet/coverage")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(coverage.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn a_private_deployment_gates_every_fleet_rollup() {
        let app = router(private_state());
        for path in [
            "/api/v1/fleet/summary",
            "/api/v1/fleet/group-summary",
            "/api/v1/fleet/coverage",
            "/api/v1/fleet/state-history",
        ] {
            let resp = app
                .clone()
                .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::UNAUTHORIZED, "{path}");
        }
    }
}
