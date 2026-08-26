// SPDX-License-Identifier: AGPL-3.0-only
//! The distributed poller pool (ADR-009/020) — the Pollers view, per-node assignment, and the
//! drill-down that answers "if this poller dies, what stops being monitored?".
//!
//! Reads are `View` and **secret-free by construction**: working-set *counts*, never spec contents.
//! Removing a decommissioned poller is `ManageSystem` — the poller fleet is deployment topology,
//! not monitoring configuration (ADR-057).
//!
//! **Almost every read here degrades rather than fails.** The durable inventory, the pool summary's
//! node counts, the folder-pool resolver and the drill-down's name lookup all fall back to a partial
//! answer on a read error, because this is the page an operator opens *when something is already
//! wrong* — a 500 here hides the live registry, which is the part that still works. The one
//! exception is the live registry itself: without it there is nothing to show.
//!
//! Liveness comes from the in-memory registry, not the database (ADR-009), and the drill-down is
//! served from the coordinator's **published** working set rather than a fresh query — so it and the
//! node detail's "Polled by" read the same data and cannot disagree.

use super::error::{ApiError, ApiResult};
use super::extract::{Admin, RequireManageSystem, RequireView, Scoped, VisibleNode};
use super::util::pool_resolver;
use super::{AdminState, ApiState};
use crate::coordinator::PollerView;
use crate::pollers::PollerRow;
use crate::poolres::PoolSource;
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{delete, get},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::time::Instant;
use uuid::Uuid;
use yagra_common::{HostSample, NodeId};

/// This domain's slice of the OpenAPI document (ADR-035), merged by [`super::openapi::document`].
#[derive(utoipa::OpenApi)]
#[openapi(paths(
    poller_health,
    list_pollers,
    poller_nodes,
    delete_poller,
    node_assignment,
    list_monitoring_gaps,
    set_poller_anchor,
    issue_poller_token,
    revoke_poller_token
))]
pub(super) struct Doc;

/// The poller-pool routes, merged into `/api/v1` by [`super::router`].
pub(super) fn routes() -> Router<ApiState> {
    Router::new()
        .route("/api/v1/poller-health", get(poller_health))
        .route("/api/v1/pollers", get(list_pollers))
        .route("/api/v1/pollers/:id/nodes", get(poller_nodes))
        .route("/api/v1/pollers/:id", delete(delete_poller))
        .route(
            "/api/v1/pollers/:id/anchor",
            axum::routing::put(set_poller_anchor),
        )
        // Issue returns the whole site bundle rather than a token (ADR-065 Inc.4): the token exists
        // only at this instant, and every other thing the site needs is derivable here and nowhere
        // else. Two calls would mean a token in a JSON body that somebody has to keep until the
        // second call.
        .route(
            "/api/v1/pollers/:id/token",
            axum::routing::post(issue_poller_token).delete(revoke_poller_token),
        )
        .route("/api/v1/nodes/:node_id/assignment", get(node_assignment))
        .route("/api/v1/monitoring-gaps", get(list_monitoring_gaps))
}

/// Poll-loop self-monitoring: last sweep time, jobs dispatched last round, total results consumed.
/// The "stat strip" of the poller & collection-health widget.
#[utoipa::path(
    get, path = "/api/v1/poller-health", tag = "pollers",
    responses(
        (status = 200, description = "Poll-loop counters since core started", body = crate::scheduler::SchedulerStatsSnapshot),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks the View permission", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode: no write side to read the scheduler from", body = super::error::ErrorBody),
    ),
)]
async fn poller_health(
    _guard: RequireView,
    admin: Admin,
) -> ApiResult<Json<crate::scheduler::SchedulerStatsSnapshot>> {
    Ok(Json(admin.scheduler_stats.snapshot()))
}

/// One poller in the `GET /api/v1/pollers` response — a merge of the live registry (current
/// status/telemetry) and the durable inventory (so an offline poller still lists). No secrets.
#[derive(Debug, Serialize, PartialEq, utoipa::ToSchema)]
pub(crate) struct PollerInfo {
    /// Sanitized poller id (stable across restarts).
    id: String,
    /// Pool it serves (live view wins; else the durable row).
    pool: String,
    /// `"online"` when it is beating within the offline window, else `"offline"`.
    status: &'static str,
    /// Last durably-recorded contact (RFC 3339); `null` for a live poller not yet persisted (it
    /// registers within the 60s inventory-upsert throttle window).
    last_seen: Option<String>,
    /// First durably-recorded contact (RFC 3339); `null` if not yet persisted.
    first_seen: Option<String>,
    /// Build version from its latest heartbeat (or the durable row when it is offline).
    version: Option<String>,
    /// Working-set node count it last reported (0 when offline / never reported).
    working_set_nodes: u32,
    /// Working-set spec count it last reported.
    working_set_specs: u32,
    /// Poll results core has consumed from it since core started.
    results_total: u64,
    /// Current host CPU utilization % (0–100) from its latest heartbeat; `null` when the poller is
    /// offline or on an N-1 build without host telemetry.
    cpu_pct: Option<f64>,
    /// Current host memory-used % (0–100); `null` when unavailable.
    mem_used_pct: Option<f64>,
    /// Highest watched-filesystem used % (0–100); `null` when unavailable.
    disk_used_pct: Option<f64>,
    /// Interface addresses the poller reported for itself. Empty for an older poller build, and
    /// empty for a containerized poller whose only address is a container-network one.
    mgmt_addrs: Vec<String>,
    /// The node this poller attaches to, naming where it sits in the derived dependency graph.
    /// `null` ⇒ core places it from `mgmt_addrs` instead.
    anchor_node_id: Option<Uuid>,
    /// Optional capabilities this poller's build advertises (`raw-capture`, `flow-relay`,
    /// `http-auth`, `http-body`, `self-upgrade`). Empty when the poller is offline, and empty from
    /// an N-1 build — **absence means "cannot", never "unknown"**, which is the same reading core
    /// applies when it decides whether to send a poller work that depends on one.
    caps: Vec<String>,
    /// Passive-event listeners it has bound (e.g. `syslog:514`, `trap:162`). Empty when offline.
    ///
    /// Worth reading before restarting a poller: unlike active polling, nothing can take these over
    /// and nothing backfills them, so whatever they would have received while it was down is gone
    /// (the same set `monitoring_gaps` stamps onto a healed gap).
    listeners: Vec<String>,
    /// Whether this poller has a bus token of its own (ADR-065). `false` means it is admitted by
    /// the deployment-wide bootstrap secret, which every poller was before tokens existed and which
    /// a co-located poller on an unencrypted internal bus still is.
    // Never the token, and never its digest — this is the one fact the page needs, and it is what
    // tells an operator which sites still share one credential.
    has_token: bool,
    /// When its token was issued, RFC 3339. `null` when it has none.
    token_issued_at: Option<String>,
}

/// One pool in the `GET /api/v1/pollers` response — node count vs. live pollers, its dispatch mode,
/// and a warning when it has nodes but no live poller (they would go unmonitored).
#[derive(Debug, Serialize, PartialEq, utoipa::ToSchema)]
pub(crate) struct PoolSummary {
    /// Pool name (`default` for unassigned nodes).
    pool: String,
    /// Non-Meraki nodes assigned to this pool.
    nodes: usize,
    /// Live (online) pollers serving this pool.
    live_pollers: usize,
    /// `"working_set"` when a live poller serves it, else `"legacy"` (per-job fallback).
    mode: &'static str,
    /// `"nodes_without_live_poller"` when the pool has nodes but no live poller, else `null`.
    warning: Option<&'static str>,
}

/// The `GET /api/v1/pollers` body: the fleet of pollers + the per-pool summary.
#[derive(Debug, Serialize, PartialEq, utoipa::ToSchema)]
pub(crate) struct PollersResponse {
    pollers: Vec<PollerInfo>,
    pools: Vec<PoolSummary>,
}

/// Merge the durable poller inventory (`PollerRepo::list`) with the live registry view
/// (`Coordinator::poller_views`) and the per-pool node counts into the Pollers response. Pure (no
/// I/O, no clock) so the merge precedence and the pool-summary arithmetic are unit-testable.
///
/// Merge rules: a poller may appear in the live view (online or recently-offline), in the durable
/// inventory, or both. The live view is authoritative for status and telemetry; the durable row
/// supplies first/last-seen timestamps (and version/pool when the poller is gone from the live
/// registry entirely). A live poller not yet in the inventory (first 60s) still lists. A pool is
/// reported when it has nodes or a live poller; `mode`/`warning` follow the scheduler's per-pool
/// working-set-vs-legacy decision.
fn build_pollers_response(
    inventory: Vec<PollerRow>,
    live: Vec<PollerView>,
    node_pools: std::collections::HashMap<String, usize>,
) -> PollersResponse {
    use std::collections::HashMap;

    let live_by_id: HashMap<&str, &PollerView> = live.iter().map(|v| (v.id.as_str(), v)).collect();
    let inv_by_id: HashMap<&str, &PollerRow> =
        inventory.iter().map(|r| (r.id.as_str(), r)).collect();

    // Union of every id we know about (live registry ∪ durable inventory), sorted + deduped so the
    // list is stable regardless of hash-map iteration order.
    let mut ids: Vec<String> = live
        .iter()
        .map(|v| v.id.clone())
        .chain(inventory.iter().map(|r| r.id.clone()))
        .collect();
    ids.sort_unstable();
    ids.dedup();

    let pollers = ids
        .iter()
        .map(|id| {
            let lv = live_by_id.get(id.as_str()).copied();
            let inv = inv_by_id.get(id.as_str()).copied();
            let online = lv.is_some_and(|v| v.online);
            PollerInfo {
                id: id.clone(),
                pool: lv
                    .map(|v| v.pool.clone())
                    .or_else(|| inv.map(|r| r.pool.clone()))
                    .unwrap_or_default(),
                status: if online { "online" } else { "offline" },
                last_seen: inv.map(|r| r.last_seen.clone()),
                first_seen: inv.map(|r| r.first_seen.clone()),
                // Prefer a non-empty live version (a sync-request-only entry reports none); else the
                // durable row's last version.
                version: lv
                    .map(|v| v.version.clone())
                    .filter(|s| !s.is_empty())
                    .or_else(|| inv.and_then(|r| r.last_version.clone())),
                working_set_nodes: lv.map_or(0, |v| v.working_set_nodes),
                working_set_specs: lv.map_or(0, |v| v.working_set_specs),
                results_total: lv.map_or(0, |v| v.results_total),
                // Host telemetry from the live registry only (the durable row carries none).
                cpu_pct: lv.and_then(|v| v.host.as_ref()).map(|h| h.cpu_pct),
                mem_used_pct: lv
                    .and_then(|v| v.host.as_ref())
                    .and_then(HostSample::mem_used_pct),
                disk_used_pct: lv
                    .and_then(|v| v.host.as_ref())
                    .and_then(HostSample::primary_disk_used_pct),
                // From the durable row only: the live registry does not carry the poller's
                // addresses, and the anchor is not the poller's to report at all.
                mgmt_addrs: inv.map(|r| r.mgmt_addrs.clone()).unwrap_or_default(),
                anchor_node_id: inv.and_then(|r| r.anchor_node_id),
                // Live-only, and deliberately empty rather than stale when the poller is offline: a
                // capability describes the build that is *running*, so reporting the last one seen
                // would answer for a process that no longer exists.
                caps: lv
                    .filter(|_| online)
                    .map_or_else(Vec::new, |v| v.caps.clone()),
                listeners: lv
                    .filter(|_| online)
                    .map_or_else(Vec::new, |v| v.listeners.clone()),
                // Durable-only: a token is a property of the inventory row, not of the connection.
                has_token: inv.is_some_and(|r| r.has_token),
                token_issued_at: inv.and_then(|r| r.token_issued_at.clone()),
            }
        })
        .collect();

    // The pool arithmetic lives in `pool_coverage`, which is also what the leader-side watch loop
    // notifies from — so the pill this endpoint renders and the page an operator receives are the
    // same judgement rather than two spellings of it.
    let pools = crate::pool_coverage::coverage(&live, &node_pools)
        .into_iter()
        .map(|c| PoolSummary {
            mode: if c.live_pollers > 0 {
                "working_set"
            } else {
                "legacy"
            },
            warning: c.is_uncovered().then_some("nodes_without_live_poller"),
            pool: c.pool,
            nodes: c.nodes,
            live_pollers: c.live_pollers,
        })
        .collect();

    PollersResponse { pollers, pools }
}

/// The registered poller fleet plus the per-pool summary.
#[utoipa::path(
    get, path = "/api/v1/pollers", tag = "pollers",
    responses(
        (status = 200, description = "Every known poller (live ∪ durable inventory) and the per-pool summary", body = PollersResponse),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks the View permission", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode: no poller inventory", body = super::error::ErrorBody),
    ),
)]
async fn list_pollers(_guard: RequireView, admin: Admin) -> ApiResult<Json<PollersResponse>> {
    Ok(Json(poller_inventory(&admin).await))
}

/// The poller fleet and per-pool summary, shared by `GET /api/v1/pollers` and the MCP
/// `get_system_health(section="pollers")` tool (ADR-042 I3a).
///
/// Infallible by construction: every read here degrades to a partial answer rather than failing
/// (ADR-017), because "which pollers exist" is the question an operator asks *while* something is
/// broken. Keeping that in the seam is the point — a second surface reimplementing it would be one
/// `?` away from failing the page the moment the inventory read hiccups.
pub(crate) async fn poller_inventory(admin: &AdminState) -> PollersResponse {
    let now = Instant::now();
    // The in-memory registry is the source of truth for liveness (ADR-009).
    let live = admin.coordinator.poller_views(now);
    // The durable inventory is best-effort context (offline pollers + timestamps); degrade to the
    // live view alone on a read error rather than failing the page (ADR-017).
    let inventory = admin.pollers.list().await.unwrap_or_else(|e| {
        tracing::warn!(error = %e, "poller inventory list failed; showing live view only");
        Vec::new()
    });
    // Non-Meraki nodes by effective pool. Shared with the coverage watch loop so the exclusion and
    // the folder inheritance are decided once (see `pool_coverage::node_counts_by_pool`).
    let node_pools = crate::pool_coverage::node_counts_by_pool(
        &admin.repo,
        &admin.meraki_devices,
        &admin.groups,
    )
    .await;
    build_pollers_response(inventory, live, node_pools)
}

/// Which poller currently polls a node — the node detail's "Polled by" fact.
#[derive(Debug, Serialize, PartialEq, utoipa::ToSchema)]
pub(crate) struct PolledBy {
    /// One of `assigned`, `legacy_fanout`, `pending`, `meraki`, `unknown`.
    state: &'static str,
    /// The owning poller; set only in the `assigned` state.
    poller_id: Option<String>,
}

/// `GET /api/v1/nodes/:id/assignment` — the node's effective pool, where that pool came from, and
/// which poller currently holds it.
#[derive(Debug, Serialize, PartialEq, utoipa::ToSchema)]
pub(crate) struct NodeAssignment {
    /// Effective pool: the node's own, else the nearest ancestor folder's, else the default.
    pool: String,
    pool_source: PoolSource,
    /// The folder that supplied the pool, when `pool_source` is `group`.
    pool_source_group_id: Option<Uuid>,
    polled_by: PolledBy,
}

/// The five distinct answers to "who polls this node". Pure, so every branch is testable without a
/// coordinator or a database.
///
/// The order matters. Leadership is checked first because an HA standby runs no coordinator
/// (`run_heartbeat_consumer` is leader-only), so its empty registry would otherwise report the
/// plausible-looking but wrong `legacy_fanout` for the entire fleet. Meraki devices come next:
/// core's own org collector polls them, so no pool poller ever will. Only then does the published
/// owner — and failing that, the pool's dispatch mode — decide.
fn resolve_polled_by(
    is_leader: bool,
    is_meraki: bool,
    owner: Option<String>,
    pool_has_live_poller: bool,
) -> PolledBy {
    if !is_leader {
        return PolledBy {
            state: "unknown",
            poller_id: None,
        };
    }
    if is_meraki {
        return PolledBy {
            state: "meraki",
            poller_id: None,
        };
    }
    if let Some(id) = owner {
        return PolledBy {
            state: "assigned",
            poller_id: Some(id),
        };
    }
    if pool_has_live_poller {
        // Working-set mode, but this node is in nobody's set: added since the last sweep, or it
        // resolves to no specs at all (so it is never published to anyone).
        PolledBy {
            state: "pending",
            poller_id: None,
        }
    } else {
        // No live poller in the pool ⇒ the scheduler falls back to legacy per-job publish on
        // `yagra.jobs.{pool}`, which has no single owner. With nothing subscribed those jobs are
        // silently discarded, so this means "possibly unmonitored" — not a healthy alternative mode.
        PolledBy {
            state: "legacy_fanout",
            poller_id: None,
        }
    }
}

/// Which pool a node effectively belongs to and which poller is currently polling it.
#[utoipa::path(
    get, path = "/api/v1/nodes/{node_id}/assignment", tag = "pollers",
    params(("node_id" = Uuid, Path, description = "Node id")),
    responses(
        (status = 200, description = "The node's effective pool, where it came from, and its owning poller", body = NodeAssignment),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks the View permission", body = super::error::ErrorBody),
        (status = 404, description = "No such node", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode: no inventory to resolve the node against", body = super::error::ErrorBody),
    ),
)]
async fn node_assignment(
    _guard: RequireView,
    _visible: VisibleNode,
    State(st): State<ApiState>,
    admin: Admin,
    Path(node_id): Path<Uuid>,
) -> ApiResult<Json<NodeAssignment>> {
    Ok(Json(node_assignment_of(&st, &admin, node_id).await?))
}

/// One node's effective pool and owning poller, shared by `GET /api/v1/nodes/:id/assignment` and
/// the MCP `get_node_status(include_assignment=true)` tool (ADR-042 I3a).
///
/// **The caller checks visibility first.** This takes an already-authorized node id — REST via the
/// `VisibleNode` extractor, MCP via `deny_invisible_node` — because the answer names a poller and a
/// pool, which is inventory an out-of-scope caller must not learn even in the negative.
pub(crate) async fn node_assignment_of(
    st: &ApiState,
    admin: &AdminState,
    node_id: Uuid,
) -> Result<NodeAssignment, ApiError> {
    let node = admin
        .repo
        .get_node(node_id)
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "node assignment: load node",
                "failed to load node",
            )
        })?
        .ok_or_else(|| ApiError::not_found("node_not_found", format!("no node {node_id}")))?;
    let resolved = pool_resolver(admin).await.resolve(&node);
    let now = Instant::now();
    // Best-effort, like the node detail's own Meraki lookup: a read failure just means the Meraki
    // case below is not special-cased.
    let is_meraki = admin
        .meraki_devices
        .get(node_id)
        .await
        .unwrap_or(None)
        .is_some();
    let polled_by = resolve_polled_by(
        st.is_leader.load(std::sync::atomic::Ordering::Acquire),
        is_meraki,
        admin.coordinator.owner_of(node.id, now),
        admin.coordinator.live_pools(now).contains(&resolved.pool),
    );
    Ok(NodeAssignment {
        pool: resolved.pool,
        pool_source: resolved.source,
        pool_source_group_id: resolved.group,
        polled_by,
    })
}

/// Largest node page the poller drill-down returns. A poller in a big pool can hold tens of
/// thousands of nodes, so the page is capped — and the response says it was, rather than looking
/// like a complete list.
const POLLER_NODES_MAX: usize = 500;

#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub(super) struct PollerNodesQuery {
    limit: Option<usize>,
}

/// One node in the poller drill-down.
#[derive(Debug, Serialize, PartialEq, utoipa::ToSchema)]
pub(crate) struct PollerNodeRef {
    id: Uuid,
    name: String,
}

/// `GET /api/v1/pollers/:id/nodes` body.
#[derive(Debug, Serialize, PartialEq, utoipa::ToSchema)]
pub(crate) struct PollerNodesResponse {
    poller_id: String,
    /// The pool it serves; `null` unless it is live.
    pool: Option<String>,
    /// `assigned` (live, working set known), `offline` (unknown or not beating), or `unknown`
    /// (this core is an HA standby and runs no coordinator).
    state: &'static str,
    /// Nodes in its working set, before the page cap.
    total: usize,
    /// Whether `nodes` is a capped page of `total`.
    truncated: bool,
    nodes: Vec<PollerNodeRef>,
}

/// The nodes a poller currently holds, for the Pollers-page drill-down.
///
/// Served from the coordinator's published working set rather than a database query, so this and
/// the node detail's "Polled by" read the same data and can never disagree.
#[utoipa::path(
    get, path = "/api/v1/pollers/{id}/nodes", tag = "pollers",
    params(("id" = String, Path, description = "Poller id"), PollerNodesQuery),
    responses(
        (status = 200, description = "The poller's published working set, capped to one page", body = PollerNodesResponse),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks the View permission", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode: no coordinator to read the working set from", body = super::error::ErrorBody),
    ),
)]
async fn poller_nodes(
    _guard: RequireView,
    Scoped(scope): Scoped,
    State(st): State<ApiState>,
    admin: Admin,
    Path(id): Path<String>,
    Query(q): Query<PollerNodesQuery>,
) -> ApiResult<Json<PollerNodesResponse>> {
    Ok(Json(
        poller_nodes_page(&st, &admin, id, q.limit, &scope).await,
    ))
}

/// The nodes a poller currently holds, shared by `GET /api/v1/pollers/:id/nodes` and the MCP
/// `get_system_health(section="poller_nodes")` tool (ADR-042 I3a).
///
/// ⚠️ **The scope filter runs before the count, and that ordering is the security property.**
/// `total` and `truncated` are part of the answer, so deriving them from the poller's full working
/// set would tell a scoped caller how many nodes it holds outside their scope — the count leaks as
/// much as the ids would. Keeping this in one function is what stops the second surface from
/// filtering the page but counting the whole set.
///
/// `limit` is clamped here rather than at either edge, so neither surface can ask for more.
pub(crate) async fn poller_nodes_page(
    st: &ApiState,
    admin: &AdminState,
    id: String,
    limit: Option<usize>,
    scope: &super::scope::NodeScope,
) -> PollerNodesResponse {
    let empty = |poller_id: String, state: &'static str| PollerNodesResponse {
        poller_id,
        pool: None,
        state,
        total: 0,
        truncated: false,
        nodes: Vec::new(),
    };
    // An HA standby runs no coordinator, so its empty registry would otherwise read as "this poller
    // holds nothing" rather than "this core cannot know".
    if !st.is_leader.load(std::sync::atomic::Ordering::Acquire) {
        return empty(id, "unknown");
    }
    let now = Instant::now();
    let Some(owned) = admin.coordinator.published_nodes(&id, now) else {
        return empty(id, "offline");
    };
    let limit = limit.unwrap_or(POLLER_NODES_MAX).clamp(1, POLLER_NODES_MAX);
    // Filter before counting, not after: `total` and `truncated` are part of the answer, so
    // deriving them from the poller's full working set would tell a scoped caller how many nodes it
    // holds outside their scope — the count is as much of a leak as the ids would be.
    let owned: Vec<NodeId> = owned
        .into_iter()
        .filter(|n| scope.allows_node(st, *n))
        .collect();
    let total = owned.len();
    let truncated = total > limit;
    if truncated {
        tracing::info!(
            poller = %id,
            total,
            limit,
            "poller node drill-down capped to the page limit"
        );
    }
    let page: Vec<Uuid> = owned.iter().take(limit).map(NodeId::as_uuid).collect();
    // Names are context, not the answer — an id with no name still tells the operator which node
    // moved, so a lookup failure degrades to bare ids rather than failing the drill-down. The ids
    // are already scope-filtered above, so the name lookup adds nothing to filter.
    let names = admin
        .repo
        .node_names(scope.group_filter(), &page)
        .await
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "poller node drill-down: name lookup failed");
            std::collections::HashMap::new()
        });
    let mut nodes: Vec<PollerNodeRef> = page
        .into_iter()
        .map(|id| PollerNodeRef {
            name: names.get(&id).cloned().unwrap_or_else(|| id.to_string()),
            id,
        })
        .collect();
    // Paged by uuid (stable), presented by name (useful).
    nodes.sort_by(|a, b| a.name.cmp(&b.name).then(a.id.cmp(&b.id)));
    let pool = admin.coordinator.pool_of(&id, now);
    PollerNodesResponse {
        poller_id: id,
        pool,
        state: "assigned",
        total,
        truncated,
        nodes,
    }
}

/// Recent core↔poller visibility outages (Phase 3, store-and-forward). Newest first, capped. A read
/// error degrades to an empty list rather than failing the Pollers page.
#[utoipa::path(
    get, path = "/api/v1/monitoring-gaps", tag = "pollers",
    responses(
        (status = 200, description = "Recent core↔poller visibility outages, newest first", body = Vec<crate::pollers::MonitoringGapRow>),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks the View permission", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode: no durable poller store", body = super::error::ErrorBody),
    ),
)]
async fn list_monitoring_gaps(
    _guard: RequireView,
    admin: Admin,
) -> ApiResult<Json<Vec<crate::pollers::MonitoringGapRow>>> {
    Ok(Json(monitoring_gaps(&admin).await))
}

/// Recent core↔poller visibility outages, shared by `GET /api/v1/monitoring-gaps` and the MCP
/// `get_system_health(section="monitoring_gaps")` tool (ADR-042 I3a). The 200-row cap lives here so
/// neither surface can ask for the whole history.
pub(crate) async fn monitoring_gaps(admin: &AdminState) -> Vec<crate::pollers::MonitoringGapRow> {
    admin
        .pollers
        .list_monitoring_gaps(200)
        .await
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "list monitoring gaps failed; returning empty");
            Vec::new()
        })
}

/// Remove a decommissioned poller from the durable inventory.
///
/// A currently-online poller is refused: deleting it would achieve nothing, because it re-registers
/// on its next heartbeat. Better to say so than to appear to work.
#[utoipa::path(
    delete, path = "/api/v1/pollers/{id}", tag = "pollers",
    params(("id" = String, Path, description = "Poller id")),
    responses(
        (status = 204, description = "Poller removed from the durable inventory"),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageSystem", body = super::error::ErrorBody),
        (status = 404, description = "No such poller", body = super::error::ErrorBody),
        (status = 409, description = "The poller is online and would re-register on its next heartbeat", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode: no durable poller store", body = super::error::ErrorBody),
    ),
)]
async fn delete_poller(
    _guard: RequireManageSystem,
    admin: Admin,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    if admin
        .coordinator
        .poller_views(Instant::now())
        .iter()
        .any(|v| v.id == id && v.online)
    {
        return Err(ApiError::conflict(
            "poller_online",
            "poller is currently online; stop it before removing it",
        ));
    }
    match admin.pollers.delete(&id).await {
        Ok(true) => Ok(StatusCode::NO_CONTENT),
        Ok(false) => Err(ApiError::not_found(
            "poller_not_found",
            format!("no poller {id}"),
        )),
        Err(e) => Err(ApiError::from_internal(
            e.as_ref(),
            "delete poller",
            "failed to delete poller",
        )),
    }
}

/// What a new site needs, beyond its own name.
#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
pub(super) struct PollerTokenRequest {
    /// The pool this poller serves. Only used when the poller is not in the inventory yet — an
    /// existing poller keeps the pool it reported.
    #[serde(default)]
    pool: Option<String>,
    /// The hostname or IP address the site will dial. Must be one the bus certificate covers, or
    /// the site's connection fails with nothing visible centrally. Defaults to the first name on
    /// the certificate that is not an internal one.
    #[serde(default)]
    host: Option<String>,
    /// The bus port at that address. Defaults to 4222.
    #[serde(default)]
    port: Option<u16>,
}

/// How long a generated poller token is, in characters of `[A-Za-z0-9]`.
///
/// 40 characters is ~238 bits. It is typed by nobody — it arrives inside a generated `.env` — so
/// there is no length worth trading entropy for.
const POLLER_TOKEN_LEN: usize = 40;

/// Names a bus certificate carries for the deployment's own containers, which no remote site dials.
///
/// Used only to pick a *default* host for the bundle. Getting the default wrong costs an operator
/// one form field; leaving it out entirely would cost every operator one, on the field most likely
/// to be filled in wrongly.
const INTERNAL_NAMES: &[&str] = &["nats", "localhost", "127.0.0.1", "::1"];

/// Issue a poller a bus token of its own and return the archive its site needs.
///
/// The response is the archive, not the token: this is the only moment the token exists in the
/// clear — only its digest is stored — and everything else the site needs is derivable here. See
/// `poller_bundle.rs`.
///
/// Creates the inventory row when the poller has not connected yet, which is what lets a site be
/// prepared before anything is running there. That is also what makes the callout able to refuse an
/// unregistered id: something has to be able to register one first.
#[utoipa::path(
    post, path = "/api/v1/pollers/{id}/token", tag = "pollers",
    params(("id" = String, Path, description = "Poller id")),
    request_body = PollerTokenRequest,
    responses(
        (status = 200, description = "A gzipped tar archive holding the site's .env, the bus certificate, the composition and a README", content_type = "application/gzip"),
        (status = 400, description = "The poller id is not usable as a bus identity, or no address was given and none could be derived", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageSystem", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode, or this deployment has no bus certificate yet", body = super::error::ErrorBody),
    ),
)]
async fn issue_poller_token(
    _guard: RequireManageSystem,
    admin: Admin,
    bus: super::extract::BusTls,
    caller: Option<super::extract::Caller>,
    Path(id): Path<String>,
    Json(req): Json<PollerTokenRequest>,
) -> ApiResult<axum::response::Response> {
    // The id becomes a NATS connection username and a subject component, so it is validated at the
    // edge rather than after it has been written into a certificate-signed scope.
    if id.is_empty() || yagra_bus::subjects::sanitize_token(&id) != id {
        return Err(ApiError::bad_request(
            "invalid_poller_id",
            "a poller id may contain only letters, digits, dash and underscore",
        ));
    }
    let cert = bus
        .view()
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "read the bus certificate",
                "failed to read the bus certificate",
            )
        })?
        .ok_or_else(|| {
            ApiError::unavailable(
                "bus_certificate_missing",
                "this deployment has no bus certificate yet, so a remote poller has nothing to pin",
            )
        })?;

    let host = match req.host.as_deref().map(str::trim).filter(|h| !h.is_empty()) {
        Some(h) => h.to_owned(),
        // Falling back to the first non-internal SAN rather than refusing: on a deployment where
        // remote acceptance is already on, that name is exactly the one the operator typed when
        // they turned it on, and asking again would be asking them to repeat themselves.
        None => cert
            .sans
            .iter()
            .find(|s| !INTERNAL_NAMES.contains(&s.as_str()))
            .cloned()
            .ok_or_else(|| {
                ApiError::bad_request(
                    "no_external_address",
                    "give the address this site will dial — the bus certificate covers only this \
                     deployment's own names, so there is nothing to guess from",
                )
            })?,
    };
    // Refused rather than issued-and-broken. A bundle naming an address the certificate does not
    // carry produces a poller that starts, fails its handshake at a site nobody is watching, and
    // never appears here — the exact failure this whole increment exists to remove.
    if !cert.sans.iter().any(|s| s.eq_ignore_ascii_case(&host)) {
        return Err(ApiError::bad_request(
            "address_not_in_certificate",
            format!(
                "the bus certificate does not cover {host}. Reissue it with that address first, at \
                 Settings ▸ Pollers ▸ Remote pollers"
            ),
        ));
    }

    let pool = req
        .pool
        .as_deref()
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .unwrap_or(yagra_bus::DEFAULT_POOL);
    if yagra_bus::subjects::sanitize_token(pool) != pool {
        return Err(ApiError::bad_request(
            "invalid_pool",
            "a pool name may contain only letters, digits, dash and underscore",
        ));
    }

    let token = random_token();
    admin
        .pollers
        .issue_token(
            &id,
            pool,
            &crate::token::token_hash(&token),
            caller.map(|c| c.0.user_id),
        )
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "issue a poller token",
                "failed to issue the poller token",
            )
        })?;

    let compose = std::fs::read_to_string(COMPOSE_IN_IMAGE).map_err(|e| {
        ApiError::from_internal(
            &e,
            "read the poller composition out of this image",
            "this core image ships no poller composition",
        )
    })?;
    let bytes = crate::poller_bundle::build(&crate::poller_bundle::SiteBundleInput {
        poller_id: &id,
        pool,
        host: &host,
        port: req.port.unwrap_or(4222),
        token: &token,
        ca_certificate: &cert.certificate,
        compose: &compose,
        core_version: env!("CARGO_PKG_VERSION"),
        mtime: u64::try_from(chrono::Utc::now().timestamp()).unwrap_or(0),
    })
    .map_err(|e| {
        ApiError::from_internal(
            &e,
            "build the poller site bundle",
            "failed to build the poller site bundle",
        )
    })?;

    tracing::warn!(poller = %id, %pool, %host, "issued a bus token for a poller");
    use axum::response::IntoResponse;
    Ok((
        [
            (
                axum::http::header::CONTENT_TYPE,
                "application/gzip".to_owned(),
            ),
            (
                axum::http::header::CONTENT_DISPOSITION,
                format!(
                    "attachment; filename=\"{}\"",
                    crate::poller_bundle::file_name(&id)
                ),
            ),
        ],
        bytes,
    )
        .into_response())
}

/// Where the core image keeps the composition a remote site runs (`docker/yagra-rust.Dockerfile`).
const COMPOSE_IN_IMAGE: &str = "/usr/share/yagra/docker-compose.poller.yml";

/// A poller token: `[A-Za-z0-9]` only.
///
/// The charset is not aesthetic — the value is pasted into a URL inside a `.env` a shell sources,
/// and anything with meaning to either is a place for the two to disagree.
fn random_token() -> String {
    use rand::Rng;
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let mut rng = rand::thread_rng();
    (0..POLLER_TOKEN_LEN)
        .map(|_| ALPHABET[rng.gen_range(0..ALPHABET.len())] as char)
        .collect()
}

/// Revoke a poller's token, returning it to the deployment-wide bootstrap secret.
///
/// Not the same thing as deleting the poller: an operator revoking a leaked token wants the site
/// back on a new one, not its inventory row, anchor and history gone.
#[utoipa::path(
    delete, path = "/api/v1/pollers/{id}/token", tag = "pollers",
    params(("id" = String, Path, description = "Poller id")),
    responses(
        (status = 204, description = "The token was revoked"),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageSystem", body = super::error::ErrorBody),
        (status = 404, description = "No such poller", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode: no durable poller store", body = super::error::ErrorBody),
    ),
)]
async fn revoke_poller_token(
    _guard: RequireManageSystem,
    admin: Admin,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    match admin.pollers.revoke_token(&id).await {
        Ok(true) => {
            tracing::warn!(poller = %id, "revoked a poller's bus token");
            Ok(StatusCode::NO_CONTENT)
        }
        Ok(false) => Err(ApiError::not_found(
            "poller_not_found",
            format!("no poller {id}"),
        )),
        Err(e) => Err(ApiError::from_internal(
            e.as_ref(),
            "revoke poller token",
            "failed to revoke the poller token",
        )),
    }
}

/// Where a poller attaches to the monitored network.
#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
pub(super) struct PollerAnchorRequest {
    /// The node the poller sits behind. `null` clears the anchor, returning the poller to being
    /// placed by its own reported addresses.
    node_id: Option<Uuid>,
}

/// Name the node a poller attaches to, rooting the derived dependency graph.
///
/// Direction in the derived graph comes from distance to a poller, so core has to know where each
/// poller sits. It works that out from the addresses the poller reports — but a poller running in a
/// container reports a container-network address that matches no monitored node, which is the
/// common case rather than the unusual one. This is how an operator says where it really is.
///
/// Until every pool that has nodes has a placed poller, derived suppression cannot be enabled.
#[utoipa::path(
    put, path = "/api/v1/pollers/{id}/anchor", tag = "pollers",
    params(("id" = String, Path, description = "Poller id")),
    request_body = PollerAnchorRequest,
    responses(
        (status = 204, description = "The anchor was set or cleared"),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageSystem", body = super::error::ErrorBody),
        (status = 404, description = "No such poller, or no such node", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode: no durable poller store", body = super::error::ErrorBody),
    ),
)]
async fn set_poller_anchor(
    _guard: RequireManageSystem,
    admin: Admin,
    Path(id): Path<String>,
    Json(req): Json<PollerAnchorRequest>,
) -> ApiResult<StatusCode> {
    // Check the node exists before storing the reference. The column has a foreign key, so a bad id
    // would fail anyway — as a 500 naming a constraint, which tells the operator nothing about what
    // they got wrong.
    if let Some(node_id) = req.node_id {
        if admin
            .repo
            .get_node(node_id)
            .await
            .map_err(|e| {
                ApiError::from_internal(e.as_ref(), "anchor node lookup", "failed to read the node")
            })?
            .is_none()
        {
            return Err(ApiError::not_found(
                "node_not_found",
                format!("no node {node_id}"),
            ));
        }
    }
    match admin.pollers.set_anchor(&id, req.node_id).await {
        Ok(true) => Ok(StatusCode::NO_CONTENT),
        Ok(false) => Err(ApiError::not_found(
            "poller_not_found",
            format!("no poller {id}"),
        )),
        Err(e) => Err(ApiError::from_internal(
            e.as_ref(),
            "set poller anchor",
            "failed to set the poller anchor",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::router;
    use crate::api::tests_support::{private_state, public_state};
    use axum::body::Body;
    use axum::http::{header::AUTHORIZATION, Request};
    use tower::ServiceExt;
    use yagra_common::{DiskUsage, Principal, Role, Scope};

    #[test]
    fn polled_by_states_cover_every_answer() {
        let assigned = || Some("edge-1".to_owned());

        // A standby core runs no coordinator, so its empty registry must read as "unknown" rather
        // than as the plausible-but-wrong "legacy_fanout" it would otherwise produce fleet-wide.
        for (meraki, owner, live) in [
            (false, None, false),
            (true, assigned(), true),
            (false, assigned(), true),
        ] {
            assert_eq!(
                resolve_polled_by(false, meraki, owner, live).state,
                "unknown"
            );
        }
        // Meraki devices are polled by core's org collector — outranks any ring answer.
        assert_eq!(
            resolve_polled_by(true, true, assigned(), true).state,
            "meraki"
        );
        // The normal case: the node is in a live poller's published working set.
        let owned = resolve_polled_by(true, false, assigned(), true);
        assert_eq!(owned.state, "assigned");
        assert_eq!(owned.poller_id.as_deref(), Some("edge-1"));
        // In a working-set pool but not (yet) in anyone's set: added since the last sweep, or it
        // builds no specs at all.
        let pending = resolve_polled_by(true, false, None, true);
        assert_eq!(pending.state, "pending");
        assert_eq!(pending.poller_id, None);
        // No live poller in the pool: the scheduler falls back to legacy per-job publish, whose
        // subject has no subscriber — i.e. possibly unmonitored, and definitely no single owner.
        let legacy = resolve_polled_by(true, false, None, false);
        assert_eq!(legacy.state, "legacy_fanout");
        assert_eq!(legacy.poller_id, None);
    }

    #[test]
    fn build_pollers_response_merges_live_and_inventory() {
        // p1: online live + durable row → online, live stats/version, PG timestamps.
        // p2: inventory only (coordinator forgot it on TTL) → offline, durable version, 0 stats.
        // p3: live only, not yet persisted (inside the 60s upsert throttle) → still listed, null ts.
        let live = vec![
            live_view("p1", "default", true),
            live_view("p3", "lab", true),
        ];
        let inventory = vec![inv_row("p1", "default"), inv_row("p2", "default")];
        let mut node_pools = std::collections::HashMap::new();
        node_pools.insert("default".to_owned(), 10usize);
        let resp = build_pollers_response(inventory, live, node_pools);

        assert_eq!(
            resp.pollers
                .iter()
                .map(|p| p.id.as_str())
                .collect::<Vec<_>>(),
            ["p1", "p2", "p3"],
            "pollers are sorted by id"
        );
        let by_id = |id: &str| resp.pollers.iter().find(|p| p.id == id).unwrap();

        let p1 = by_id("p1");
        assert_eq!(p1.status, "online");
        assert_eq!(p1.version.as_deref(), Some("0.1.2"), "live version wins");
        assert_eq!(p1.last_seen.as_deref(), Some("2026-07-06T01:00:00+00:00"));
        assert_eq!(p1.working_set_nodes, 5);
        assert_eq!(p1.results_total, 42);

        let p2 = by_id("p2");
        assert_eq!(p2.status, "offline");
        assert_eq!(
            p2.version.as_deref(),
            Some("0.1.1"),
            "offline poller falls back to the durable version"
        );
        assert_eq!(p2.working_set_nodes, 0);
        assert_eq!(p2.results_total, 0);

        let p3 = by_id("p3");
        assert_eq!(p3.status, "online");
        assert_eq!(p3.pool, "lab");
        assert_eq!(
            p3.last_seen, None,
            "a not-yet-persisted poller has no timestamp"
        );
        assert_eq!(p3.first_seen, None);
    }

    #[test]
    fn build_pollers_response_maps_host_columns_from_live_sample() {
        let mut online = live_view("p1", "default", true);
        online.host = Some(HostSample {
            cpu_pct: 40.0,
            mem_used_bytes: 3,
            mem_total_bytes: 4,
            disks: vec![DiskUsage {
                mount: "root".into(),
                used_bytes: 60,
                size_bytes: 100,
            }],
            ..Default::default()
        });
        let resp =
            build_pollers_response(Vec::new(), vec![online], std::collections::HashMap::new());
        let p1 = &resp.pollers[0];
        assert_eq!(p1.cpu_pct, Some(40.0));
        assert_eq!(p1.mem_used_pct, Some(75.0));
        assert_eq!(p1.disk_used_pct, Some(60.0));

        // A poller with no host sample (offline / N-1) exposes null host columns.
        let resp = build_pollers_response(
            Vec::new(),
            vec![live_view("p2", "default", true)],
            std::collections::HashMap::new(),
        );
        assert_eq!(resp.pollers[0].cpu_pct, None);
        assert_eq!(resp.pollers[0].disk_used_pct, None);
    }

    #[test]
    fn build_pollers_response_pool_summary_modes_and_warnings() {
        // default: nodes + a live poller → working_set, no warning.
        // legacy-pool: nodes but only an OFFLINE poller → legacy + warning (offline doesn't count).
        // waiting: a live poller but no nodes → working_set, no warning (poller idle).
        let live = vec![
            live_view("p1", "default", true),
            live_view("p-off", "legacy-pool", false),
            live_view("p-wait", "waiting", true),
        ];
        let mut node_pools = std::collections::HashMap::new();
        node_pools.insert("default".to_owned(), 10usize);
        node_pools.insert("legacy-pool".to_owned(), 4usize);
        let resp = build_pollers_response(Vec::new(), live, node_pools);

        assert_eq!(
            resp.pools
                .iter()
                .map(|p| p.pool.as_str())
                .collect::<Vec<_>>(),
            ["default", "legacy-pool", "waiting"],
            "pools are sorted by name"
        );
        let pool = |name: &str| resp.pools.iter().find(|p| p.pool == name).unwrap();

        let d = pool("default");
        assert_eq!(
            (d.nodes, d.live_pollers, d.mode, d.warning),
            (10, 1, "working_set", None)
        );
        let l = pool("legacy-pool");
        assert_eq!(
            (l.nodes, l.live_pollers, l.mode, l.warning),
            (4, 0, "legacy", Some("nodes_without_live_poller"))
        );
        let w = pool("waiting");
        assert_eq!(
            (w.nodes, w.live_pollers, w.mode, w.warning),
            (0, 1, "working_set", None)
        );
    }

    /// The pill and the page must be the same judgement.
    ///
    /// This endpoint's `warning` and the leader-side notification loop both answer "does this pool
    /// have nodes and no live poller". Two spellings of that would eventually disagree, and the
    /// failure mode is the worst kind: Settings ▸ Pollers showing a warning nobody was paged for,
    /// or a page for a pool the UI calls healthy. So `PoolCoverage::is_uncovered` is the only
    /// definition, and this pins the derivation to it.
    #[test]
    fn the_pool_warning_is_exactly_the_coverage_conditions_answer() {
        let live = vec![
            live_view("p1", "default", true),
            live_view("p-off", "legacy-pool", false),
            live_view("p-wait", "waiting", true),
        ];
        let mut node_pools = std::collections::HashMap::new();
        node_pools.insert("default".to_owned(), 10usize);
        node_pools.insert("legacy-pool".to_owned(), 4usize);
        node_pools.insert("empty-and-unserved".to_owned(), 0usize);

        let coverage = crate::pool_coverage::coverage(&live, &node_pools);
        let resp = build_pollers_response(Vec::new(), live.clone(), node_pools);

        assert_eq!(
            resp.pools.len(),
            coverage.len(),
            "both sides report the same pools"
        );
        for (summary, cov) in resp.pools.iter().zip(&coverage) {
            assert_eq!(summary.pool, cov.pool);
            assert_eq!(summary.nodes, cov.nodes);
            assert_eq!(summary.live_pollers, cov.live_pollers);
            assert_eq!(
                summary.warning.is_some(),
                cov.is_uncovered(),
                "pool {} disagrees between the pill and the coverage condition",
                summary.pool
            );
        }
        assert!(
            coverage.iter().any(PoolCoverageExt::uncovered),
            "the fixture must actually exercise the uncovered branch"
        );
    }

    /// Local alias so the assertion above reads as a predicate over the fixture.
    trait PoolCoverageExt {
        fn uncovered(&self) -> bool;
    }
    impl PoolCoverageExt for crate::pool_coverage::PoolCoverage {
        fn uncovered(&self) -> bool {
            self.is_uncovered()
        }
    }

    async fn status_of(st: ApiState, method: &str, path: &str, token: Option<&str>) -> StatusCode {
        let mut b = Request::builder().method(method).uri(path);
        if let Some(t) = token {
            b = b.header(AUTHORIZATION, format!("Bearer {t}"));
        }
        router(st)
            .oneshot(b.body(Body::empty()).unwrap())
            .await
            .unwrap()
            .status()
    }

    #[tokio::test]
    async fn the_pollers_view_is_read_gated_and_needs_the_live_side() {
        // View-gated like the other fleet reads, so a public dashboard reaches availability…
        assert_eq!(
            status_of(public_state(), "GET", "/api/v1/pollers", None).await,
            StatusCode::SERVICE_UNAVAILABLE,
        );
        // …and a private deployment does not serve it anonymously.
        for path in [
            "/api/v1/pollers",
            "/api/v1/poller-health",
            "/api/v1/monitoring-gaps",
            "/api/v1/pollers/edge-1/nodes",
        ] {
            assert_eq!(
                status_of(private_state(), "GET", path, None).await,
                StatusCode::UNAUTHORIZED,
                "{path}"
            );
        }
    }

    #[tokio::test]
    async fn removing_a_poller_needs_manage_config() {
        // A Viewer/Operator is rejected before any DB work, so the RBAC gate is testable without a
        // database — and an Admin reaches 503 rather than 403, which is the positive control that
        // the guard admits somebody.
        let st = private_state();
        for role in [Role::Viewer, Role::Operator] {
            let token = st
                .sessions
                .issue(Uuid::new_v4(), Principal::new(role, Scope::All), "u");
            assert_eq!(
                status_of(st.clone(), "DELETE", "/api/v1/pollers/edge-1", Some(&token)).await,
                StatusCode::FORBIDDEN,
                "{role:?}"
            );
        }
        let admin = st.sessions.issue(
            Uuid::new_v4(),
            Principal::new(Role::Admin, Scope::All),
            "admin1",
        );
        assert_eq!(
            status_of(st, "DELETE", "/api/v1/pollers/edge-1", Some(&admin)).await,
            StatusCode::SERVICE_UNAVAILABLE,
        );
    }

    /// A live registry view for the merge tests (online → recent, offline → stale).
    fn live_view(id: &str, pool: &str, online: bool) -> PollerView {
        PollerView {
            id: id.to_owned(),
            pool: pool.to_owned(),
            online,
            seconds_since_seen: if online { 3 } else { 120 },
            version: "0.1.2".to_owned(),
            incarnation: Uuid::nil(),
            working_set_nodes: 5,
            working_set_specs: 9,
            inflight: 0,
            results_total: 42,
            host: None,
            listeners: Vec::new(),
            caps: Vec::new(),
        }
    }

    /// A durable inventory row for the merge tests.
    fn inv_row(id: &str, pool: &str) -> PollerRow {
        PollerRow {
            id: id.to_owned(),
            pool: pool.to_owned(),
            first_seen: "2026-07-06T00:00:00+00:00".to_owned(),
            last_seen: "2026-07-06T01:00:00+00:00".to_owned(),
            last_version: Some("0.1.1".to_owned()),
            last_incarnation: None,
            mgmt_addrs: Vec::new(),
            anchor_node_id: None,
            has_token: false,
            token_issued_at: None,
        }
    }

    /// Every image a shipped composition pulls goes through `${YAGRA_IMAGE_REPO:-…}`.
    ///
    /// `docker-compose.poller.yml` is the one this module hands to a site (`COMPOSE_IN_IMAGE`,
    /// travelling inside the bundle), and it named `ghcr.io/horryworks/yagra-poller` outright for
    /// its whole life while the published reference documentation listed `YAGRA_IMAGE_REPO` as
    /// applying to "deploy, poller". A site pulling Yagra from an internal mirror could therefore
    /// redirect the central stack and not its own pollers — and nothing said so: compose resolves a
    /// literal perfectly happily, and the only symptom is a registry reached that the operator
    /// thought they had switched away from.
    ///
    /// The deploy composition is read here too, and not for symmetry: the two must spell the
    /// reference the same way or the variable means different things on the two ends of one
    /// deployment. This is the only test that reads the poller one at all.
    ///
    /// 🚨 It asserts how many lines it *checked*. A rename, a moved file or a pattern that stops
    /// matching would otherwise leave a scan of zero lines reporting success.
    #[test]
    fn every_shipped_composition_pulls_its_images_through_the_repo_variable() {
        const WANT: &str = "${YAGRA_IMAGE_REPO:-ghcr.io/horryworks}";
        let mut checked = 0;
        for file in ["docker-compose.deploy.yml", "docker-compose.poller.yml"] {
            let text = std::fs::read_to_string(format!("../../{file}"))
                .unwrap_or_else(|e| panic!("{file} ships with the product: {e}"));
            let mut in_file = 0;
            for (n, line) in text.lines().enumerate() {
                let trimmed = line.trim_start();
                // A commented-out service is not something compose pulls.
                if trimmed.starts_with('#') || !trimmed.starts_with("image:") {
                    continue;
                }
                if !trimmed.contains("yagra-") {
                    continue; // docker:28-cli and the like are not ours to redirect.
                }
                assert!(
                    trimmed.contains(WANT),
                    "{file}:{} pulls a Yagra image without {WANT}, so a deployment on a private \
                     mirror cannot redirect it: {trimmed}",
                    n + 1,
                );
                in_file += 1;
            }
            assert!(
                in_file > 0,
                "{file} named no Yagra image at all — this check scanned nothing and would have \
                 passed on an empty file",
            );
            checked += in_file;
        }
        assert_eq!(
            checked, 5,
            "expected 5 Yagra image references: four in the deploy composition (core, core again \
             for bus-init, the co-located poller, web) and one in the remote-site poller's. A \
             changed count means a service was added or removed, so decide this number again \
             rather than raising it to whatever was measured",
        );
    }

    /// A shipped composition never lets a poller name itself after its container (ADR-065 Inc.8).
    ///
    /// Unset, `YAGRA_POLLER_ID` falls back to the container hostname, which Docker invents afresh on
    /// every recreate — so an upgrade or a plain `up -d` hands the poller a new identity. On a
    /// deployment accepting remote pollers that is invisible and load-bearing at once: acceptance
    /// turns auto-registration off because the callout refuses an id nobody registered, but a
    /// co-located poller connects on the static `poller` account the callout deliberately bypasses,
    /// so it is neither refused nor registered. It keeps polling off its Redis liveness and vanishes
    /// from Settings ▸ Pollers, while every id it used before stays behind as a row.
    ///
    /// The two files answer this differently on purpose, and both answers are explicit:
    /// the co-located poller gets a default (`:-`), a remote site is *required* to name itself
    /// (`:?`) because two sites sharing an id would share an assignment.
    #[test]
    fn no_shipped_composition_lets_a_poller_name_itself_after_its_container() {
        let mut checked = 0;
        for (file, want) in [
            ("docker-compose.deploy.yml", "${YAGRA_POLLER_ID:-"),
            ("docker-compose.yml", "${YAGRA_POLLER_ID:-"),
            ("docker-compose.poller.yml", "${YAGRA_POLLER_ID:?"),
        ] {
            let text = std::fs::read_to_string(format!("../../{file}"))
                .unwrap_or_else(|e| panic!("{file} ships with the product: {e}"));
            assert!(
                text.lines().any(|l| l.trim_start().starts_with("poller:")),
                "{file} no longer defines a poller service, so this check is reading the wrong file",
            );
            let live: Vec<&str> = text
                .lines()
                .map(str::trim_start)
                .filter(|l| !l.starts_with('#'))
                .filter(|l| l.starts_with("YAGRA_POLLER_ID:"))
                .collect();
            assert_eq!(
                live.len(),
                1,
                "{file} must set YAGRA_POLLER_ID exactly once, live; found {live:?}",
            );
            assert!(
                live[0].contains(want),
                "{file} sets YAGRA_POLLER_ID without {want}, so the id is not pinned: {}",
                live[0],
            );
            checked += 1;
        }
        assert_eq!(
            checked, 3,
            "expected all three shipped compositions to be inspected; a lower number means one \
             stopped matching and this check passed over it",
        );
    }

    /// The updater resolves a poller id the way the poller itself does: env var first (ADR-065
    /// Inc.8).
    ///
    /// `local_pollers` is the list core pre-registers, and `YAGRA_POLLER_ID` is what a poller
    /// actually calls itself. If the updater read the container hostname first, the id core adopted
    /// and the id the poller claimed would be different strings, and pre-registering would create a
    /// row for a poller that does not exist while the real one stayed missing — the same end state
    /// this increment exists to remove, reached by the opposite route.
    #[test]
    fn the_updater_resolves_a_poller_id_env_first_like_the_poller_does() {
        let text = std::fs::read_to_string("../../docker-compose.deploy.yml")
            .expect("the deploy composition ships with the product");
        // Cut at the closing line, NOT at the first brace: the body is full of docker-inspect go
        // templates ({{range .Config.Env}}), and stopping at one of those ends the slice before
        // the line under test — which then reads as "the updater never mentions YAGRA_POLLER_ID"
        // rather than as a bad cut. That is the failure this check first produced.
        let body: String = text
            .split_once("local_pollers() {")
            .expect("the updater still derives local_pollers")
            .1
            .lines()
            .take_while(|l| *l != "        }")
            .collect::<Vec<_>>()
            .join(" ");
        let env_at = body
            .find("YAGRA_POLLER_ID")
            .expect("local_pollers must read YAGRA_POLLER_ID at all");
        let host_at = body
            .find(".Config.Hostname")
            .expect("local_pollers must still fall back to the hostname for an older poller");
        assert!(
            env_at < host_at,
            "local_pollers reads the container hostname before YAGRA_POLLER_ID, so core would \
             adopt an id no poller claims",
        );
    }

    /// Core and the poller beside it are handed the *same* id expression (ADR-065 Inc.8).
    ///
    /// Core adopts `YAGRA_LOCAL_POLLER_ID` at startup and the poller claims `YAGRA_POLLER_ID`. If
    /// the two defaults ever drift apart, core registers one id and the poller uses another: the
    /// row exists, the poller polls, and Settings ▸ Pollers shows a poller that is not the one
    /// doing the work — which is the original bug wearing a different hat, and nothing at runtime
    /// would say so.
    ///
    /// Comparing the expressions rather than the values is the point: `${YAGRA_POLLER_ID:-local}`
    /// resolves per deployment, and what has to hold is that both resolve the *same way*.
    #[test]
    fn core_and_its_poller_are_given_the_same_id_expression() {
        let text = std::fs::read_to_string("../../docker-compose.deploy.yml")
            .expect("the deploy composition ships with the product");
        let value_of = |key: &str| -> String {
            let live: Vec<&str> = text
                .lines()
                .map(str::trim_start)
                .filter(|l| !l.starts_with('#'))
                .filter(|l| l.starts_with(key))
                .collect();
            assert_eq!(
                live.len(),
                1,
                "{key} must be set exactly once, live; got {live:?}"
            );
            live[0]
                .split_once(':')
                .expect("a compose environment entry is key: value")
                .1
                .trim()
                .to_owned()
        };
        let poller = value_of("YAGRA_POLLER_ID:");
        let core = value_of("YAGRA_LOCAL_POLLER_ID:");
        assert_eq!(
            core, poller,
            "core is told to adopt {core} while the poller calls itself {poller}; core would \
             register a poller that does not exist and leave the real one invisible",
        );
        assert!(
            poller.contains("${"),
            "the id must stay overridable per deployment, not be hardcoded: {poller}",
        );
    }
}
