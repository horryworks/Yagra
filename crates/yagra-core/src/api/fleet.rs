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

use super::extract::{Admin, RequireView, Scoped};
use super::scope::NodeScope;
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

/// This domain's slice of the OpenAPI document (ADR-035), merged by [`super::openapi::document`].
#[derive(utoipa::OpenApi)]
#[openapi(paths(
    fleet_summary,
    fleet_group_summary,
    fleet_coverage,
    fleet_state_history
))]
pub(super) struct Doc;

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
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub(crate) struct FleetSummary {
    pub total: i64,
    pub states: BTreeMap<&'static str, i64>,
}

/// Tally the fleet by rolled-up display state.
///
/// Nodes the alert engine has never observed — brand new, or right after a core restart before the
/// first sweep — take the same fallback the per-node list takes: a recent ICMP sample means `ok`,
/// silence means `unknown` ([`super::nodes::state_or_fallback`]). This tally used to count *all* of
/// them as `unknown` while claiming in a comment that it matched the list, so for the minutes after
/// every restart the dashboard summary reported `unknown` for nodes the Nodes page beside it was
/// showing as `ok`.
///
/// The fallback costs a TSDB query, so it is asked **only when something is actually unobserved** —
/// in the steady state the engine holds an opinion about every node and this function does no I/O
/// beyond the inventory count, exactly as before.
///
/// Scoping costs the unrestricted caller nothing: `NodeScope::All` keeps the precomputed
/// `node_state_counts()` fast path exactly as it was. A scoped caller instead walks `node_states()`
/// once, intersecting with the visible set — O(fleet) over an in-memory map, not a database scan,
/// and only on a dashboard poll. That asymmetry is deliberate; do not "unify" the two branches by
/// making everyone take the walk.
pub(crate) async fn state_tally(st: &ApiState, scope: &NodeScope) -> FleetSummary {
    let total = st
        .nodes
        .count(scope.group_filter())
        .await
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "fleet summary node count failed");
            0
        });
    let mut states: BTreeMap<&'static str, i64> =
        NodeState::ALL.iter().map(|s| (s.as_str(), 0)).collect();
    let mut observed_total: i64 = 0;
    if scope.is_all() {
        for (state, n) in st.alerts.node_state_counts() {
            let n = n as i64;
            *states.entry(state.as_str()).or_insert(0) += n;
            observed_total += n;
        }
    } else {
        for (node, state) in st.alerts.node_states() {
            if !scope.allows_node(st, node) {
                continue;
            }
            *states.entry(state.as_str()).or_insert(0) += 1;
            observed_total += 1;
        }
    }
    let unobserved = (total - observed_total).max(0);
    let fresh = fresh_unobserved(st, scope, unobserved).await;
    *states.entry(NodeState::Ok.as_str()).or_insert(0) += fresh;
    *states.entry(NodeState::Unknown.as_str()).or_insert(0) += unobserved - fresh;
    FleetSummary { total, states }
}

/// How many of the `unobserved` visible nodes a recent ICMP sample makes `ok`.
///
/// This tally holds *counts*, not the ids of the unobserved nodes, so it cannot use the paged
/// probe: it asks for the fleet's fresh set once and subtracts everything the engine already has an
/// opinion about. Clamped to `unobserved` because the TSDB outlives the inventory — a deleted
/// node's series stays fresh for the rest of the retention window, and an unclamped count would
/// make the tally sum to more than `total`.
async fn fresh_unobserved(st: &ApiState, scope: &NodeScope, unobserved: i64) -> i64 {
    if unobserved <= 0 {
        return 0;
    }
    let known = st.alerts.node_states();
    let fresh = super::nodes::fresh_fleet_ids(st.store.as_ref()).await;
    let n = fresh
        .into_iter()
        .map(NodeId::from)
        .filter(|id| !known.contains_key(id) && scope.allows_node(st, *id))
        .count() as i64;
    n.min(unobserved)
}

/// Fleet-wide status summary, computed server-side from the live alert engine (S12).
///
/// View-gated and works without the admin store, so a public dashboard can render its
/// status-summary / health-ring / nodes-down widgets.
#[utoipa::path(
    get, path = "/api/v1/fleet/summary", tag = "fleet",
    responses(
        (status = 200, description = "Total node count plus a tally carrying all six states", body = FleetSummary),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks the view permission", body = super::error::ErrorBody),
    ),
)]
async fn fleet_summary(
    _perm: RequireView,
    Scoped(scope): Scoped,
    State(st): State<ApiState>,
) -> Json<FleetSummary> {
    Json(state_tally(&st, &scope).await)
}

// ── Per-group rollup ─────────────────────────────────────────────────────────

/// Per-group direct-member state tally. All six keys are always present (a missing state is `0`)
/// and they sum to the group's direct-member count.
#[derive(Serialize, Default, Debug, Clone, PartialEq, utoipa::ToSchema)]
pub(crate) struct GroupStateCounts {
    pub ok: i64,
    pub warning: i64,
    pub critical: i64,
    pub unknown: i64,
    pub unreachable: i64,
    pub maintenance: i64,
}

/// The per-group rollup response: `group_id → direct-member state counts`.
#[derive(Serialize, utoipa::ToSchema)]
pub(crate) struct FleetGroupSummary {
    pub groups: HashMap<Uuid, GroupStateCounts>,
}

/// Roll a node→group map + the engine's per-node states into per-group **direct-member** tallies.
///
/// Pure (no I/O) so the grouping and fallback rules are unit-testable. Ungrouped nodes (null
/// `group_id`) are skipped — no widget rolls them up. A node the engine has never observed takes
/// the same fallback as the per-node list ([`super::nodes::state_or_fallback`]): `fresh` holds the
/// unobserved nodes with a recent ICMP sample, and the rest are `unknown` — so a group's tally
/// reconciles with its size *and* agrees with what the Nodes tree shows for the same members.
pub(crate) fn aggregate_group_counts(
    node_groups: &[(Uuid, Option<Uuid>)],
    states: &HashMap<NodeId, NodeState>,
    fresh: &HashSet<Uuid>,
) -> HashMap<Uuid, GroupStateCounts> {
    let mut groups: HashMap<Uuid, GroupStateCounts> = HashMap::new();
    for (id, group_id) in node_groups {
        let Some(gid) = group_id else { continue };
        let state = super::nodes::state_or_fallback(
            states.get(&NodeId::from(*id)).copied(),
            fresh.contains(id),
        );
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
#[utoipa::path(
    get, path = "/api/v1/fleet/group-summary", tag = "fleet",
    responses(
        (status = 200, description = "Per-group direct-member state tallies, keyed by group id", body = FleetGroupSummary),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks the view permission", body = super::error::ErrorBody),
    ),
)]
async fn fleet_group_summary(
    _perm: RequireView,
    Scoped(scope): Scoped,
    State(st): State<ApiState>,
) -> ApiResult<Json<FleetGroupSummary>> {
    Ok(Json(group_summary(&st, &scope).await?))
}

/// Per-group direct-member state tallies, keyed by group id, for what `scope` may see.
///
/// Filtering the node→group map is enough: the rollup is keyed by group, so a group with no visible
/// members simply never appears rather than appearing with a zeroed tally. Note this scans only the
/// rows the caller may see — it does not add a second pass.
pub(crate) async fn group_summary(
    st: &ApiState,
    scope: &super::scope::NodeScope,
) -> Result<FleetGroupSummary, ApiError> {
    let node_groups = st
        .nodes
        .node_group_map(scope.group_filter())
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "fleet group summary node map",
                "failed to load group summary",
            )
        })?;
    let states = st.alerts.node_states();
    // Same fallback as the tally above and the per-node list, and skipped on the same condition:
    // one fleet-wide freshness query, only when the engine is missing an opinion about someone.
    let any_unobserved = node_groups
        .iter()
        .any(|(id, group)| group.is_some() && !states.contains_key(&NodeId::from(*id)));
    let fresh = if any_unobserved {
        super::nodes::fresh_fleet_ids(st.store.as_ref()).await
    } else {
        HashSet::new()
    };
    Ok(FleetGroupSummary {
        groups: aggregate_group_counts(&node_groups, &states, &fresh),
    })
}

// ── Data coverage (the blind-spot detector) ──────────────────────────────────

/// How recent a node's last ICMP sample must be to count as "fresh" (silent beyond this ⇒ stale).
const COVERAGE_FRESH_SECS: u64 = 600;
/// Cap on the returned watchlist. A fleet that is 90% stale has a systemic problem, not 40,000
/// individual ones, so the first 50 names are as much as a human can act on.
const STALE_WATCHLIST_MAX: usize = 50;

/// A node returning no fresh data (silent failure / blind spot).
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub(crate) struct StaleNode {
    pub node_id: Uuid,
    pub name: String,
}

/// Fleet data-coverage summary: fresh vs total nodes + the stale watchlist.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
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
#[utoipa::path(
    get, path = "/api/v1/fleet/coverage", tag = "fleet",
    responses(
        (status = 200, description = "Fresh vs total node counts plus the capped stale watchlist", body = FleetCoverage),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks the view permission", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode has no inventory to measure coverage against", body = super::error::ErrorBody),
    ),
)]
async fn fleet_coverage(
    _perm: RequireView,
    Scoped(scope): Scoped,
    admin: Admin,
    State(st): State<ApiState>,
) -> ApiResult<Json<FleetCoverage>> {
    Ok(Json(coverage(&st, &admin, &scope).await?))
}

/// Which nodes are actually being monitored, shared by `GET /api/v1/fleet/coverage` and the MCP
/// `get_fleet_summary(kind="coverage")` tool (ADR-042 I3a).
///
/// Group-filtered rather than refused: unlike the state timeline below, coverage still holds node
/// ids, so a scoped caller gets their own slice.
pub(crate) async fn coverage(
    st: &ApiState,
    admin: &super::AdminState,
    scope: &super::scope::NodeScope,
) -> Result<FleetCoverage, ApiError> {
    // `list_nodes` is the unscoped internal scan, so the filter is applied to its result here
    // rather than in SQL. Coverage is an admin's periodic blind-spot check over a response that is
    // bounded either way (the watchlist caps at 50), so the extra pass is not on a hot path.
    let nodes: Vec<_> = admin
        .repo
        .list_nodes()
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "fleet coverage list nodes",
                "failed to load fleet coverage",
            )
        })?
        .into_iter()
        .filter(|n| scope.allows_group(n.group.map(|g| g.as_uuid())))
        .collect();
    let fresh_ids: HashSet<Uuid> = st
        .store
        .fresh_node_ids("icmp_rtt_ms", COVERAGE_FRESH_SECS)
        .await
        .into_iter()
        .collect();
    Ok(coverage_of(nodes, &fresh_ids))
}

// ── State timeline ───────────────────────────────────────────────────────────

/// Query for the fleet state-history timeline: `?from=&to=` Unix seconds (default last 24h).
#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub(super) struct StateHistoryQuery {
    from: Option<i64>,
    to: Option<i64>,
}

/// Cap on the requested history window — defence in depth on top of snapshot retention, so a
/// client cannot ask for an unboundedly large scan.
const MAX_HISTORY_SECS: i64 = 90 * 24 * 3600;

/// Node-state counts over time, pivoted into per-state series on a shared timestamp axis.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
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
#[utoipa::path(
    get, path = "/api/v1/fleet/state-history", tag = "fleet",
    params(StateHistoryQuery),
    responses(
        (status = 200, description = "One aligned series per state on a shared timestamp axis", body = FleetStateHistory),
        (status = 400, description = "`to` precedes `from`, or the window exceeds 90 days", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks the view permission", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode keeps no state-history snapshots", body = super::error::ErrorBody),
    ),
)]
async fn fleet_state_history(
    _perm: RequireView,
    Scoped(scope): Scoped,
    admin: Admin,
    Query(q): Query<StateHistoryQuery>,
) -> ApiResult<Json<FleetStateHistory>> {
    Ok(Json(state_history(&admin, &scope, q.from, q.to).await?))
}

/// The fleet state timeline, shared by `GET /api/v1/fleet/state-history` and the MCP
/// `fleet_state_history` tool (ADR-042 I3a).
///
/// ⚠️ **The scope refusal and the 90-day bound are both inside this function, deliberately.** A
/// second surface that called the repo directly would serve a group-scoped caller the whole fleet's
/// numbers as if they were their own — the exact failure ADR-014 exists to prevent — and would have
/// no window bound at all. The refusal goes through `scope::require_fleet_wide` rather than being
/// spelled out here; `no_handler_spells_the_scope_refusal_by_hand` enforces that.
pub(crate) async fn state_history(
    admin: &super::AdminState,
    scope: &super::scope::NodeScope,
    from: Option<i64>,
    to: Option<i64>,
) -> Result<FleetStateHistory, ApiError> {
    // `node_state_snapshots` stores `(ts, state, count)` — the tally was already computed when the
    // snapshot was written, and no node id survives into the row. So there is nothing to filter and
    // nothing to join: a scoped caller cannot be served a narrower timeline from this table at all.
    // Serving them the fleet's numbers would be a leak, and serving zeroes would be a lie, so it
    // refuses. Making this scopable means snapshotting per group — a schema and writer change, i.e.
    // a feature rather than a filter.
    super::scope::require_fleet_wide(
        scope,
        "the fleet state timeline is stored pre-aggregated with no per-node attribution, so it \
         cannot be narrowed to a group-scoped account",
    )?;
    let to = to.unwrap_or_else(super::now_unix_s);
    let from = from.unwrap_or(to - 24 * 3600);
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
    Ok(pivot_state_history(rows))
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
        // n_never intentionally absent from `states`, and absent from the fresh set too — so it
        // takes the `unknown` half of the fallback.
        let out = aggregate_group_counts(&node_groups, &states, &HashSet::new());

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

    /// A store holding one RTT reading for `node`, and nothing else.
    fn store_with_rtt(node: NodeId) -> std::sync::Arc<dyn crate::store::MetricStore> {
        let sink = crate::sink::InMemorySink::default();
        sink.ingest(&yagra_bus::PollResult {
            job_id: Uuid::nil(),
            node_id: node,
            at_unix_ms: 0,
            outcome: yagra_bus::CheckOutcome::Reachable,
            samples: vec![yagra_bus::Sample::gauge("icmp_rtt_ms", 1.5)],
            interfaces: Vec::new(),
            sys_descr: None,
            dns_chain: None,
            neighbors: None,
            l3: None,
            arp: None,
            routing: None,
            observational: false,
            poller_id: None,
            trace_context: Default::default(),
        });
        std::sync::Arc::new(sink)
    }

    #[tokio::test]
    async fn the_tally_and_the_per_node_list_agree_about_an_unobserved_but_fresh_node() {
        // The bug this pins: right after a core restart the engine has observed nobody, so the
        // summary counted every node `unknown` while the Nodes page beside it — which applies the
        // freshness fallback — showed the same nodes `ok`. Two surfaces, one node, two answers.
        let mut st = public_state();
        let ids: Vec<NodeId> = st
            .nodes
            .list_page(None, None, 10)
            .await
            .expect("skeleton inventory")
            .iter()
            .map(|n| n.id)
            .collect();
        assert_eq!(ids.len(), 1, "the skeleton inventory is one demo node");
        st.store = store_with_rtt(ids[0]);

        let per_node = crate::api::nodes::display_states(&st, &ids).await;
        assert_eq!(per_node.get(&ids[0]).copied(), Some(NodeState::Ok));

        let tally = state_tally(&st, &NodeScope::All).await;
        assert_eq!(tally.total, 1);
        assert_eq!(
            tally.states["ok"], 1,
            "the tally must apply the same fallback the per-node list does"
        );
        assert_eq!(tally.states["unknown"], 0);
        // Still all six keys, and they still sum to `total`.
        assert_eq!(tally.states.len(), NodeState::ALL.len());
        assert_eq!(tally.states.values().sum::<i64>(), tally.total);
    }

    #[tokio::test]
    async fn a_silent_unobserved_node_is_still_unknown() {
        // The other half of the fallback, and the reason it is a fallback rather than an
        // assumption: no recent sample means we genuinely do not know, and saying `ok` there would
        // report a dead fleet as healthy.
        let st = public_state();
        let tally = state_tally(&st, &NodeScope::All).await;
        assert_eq!((tally.total, tally.states["unknown"]), (1, 1));
        assert_eq!(tally.states["ok"], 0);
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
