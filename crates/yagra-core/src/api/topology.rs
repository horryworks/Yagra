// SPDX-License-Identifier: AGPL-3.0-only
//! The dependency graph (`GET /api/v1/topology`).
//!
//! Assembled from two live sources and stored as neither: the inventory supplies the parent edges,
//! the alert engine supplies each node's current state and any root-cause attribution from
//! dependency suppression. There is no topology table.
//!
//! Keyset-paginated (S7): this used to return one unbounded full-fleet blob every 15 seconds. It
//! now returns bounded pages ordered by id with a `next_cursor`, and the client keeps them fresh
//! over the node-state SSE stream rather than re-fetching.
//!
//! [`topology_page`] is the seam the REST handler and the MCP `get_topology` tool both call, so the
//! graph is assembled once in the codebase. The two surfaces keep their own limits — the UI wants
//! large pages, an AI client wants small ones — because a clamp is surface policy, not assembly.

use super::extract::{Actor, RequireManageConfig, RequireView, Scoped};
use super::nodes::{fresh_fallback_ids, state_or_fallback, NodePageQuery};
use super::{ApiError, ApiResult, ApiState};
use crate::api::extract::Admin;
use axum::{
    extract::{Path, Query},
    routing::{delete, get},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap, HashSet};
use uuid::Uuid;
use yagra_common::{
    LinkDirection, LinkOverrideAction, LinkSource, NodeId, NodeState, TopologyLinkSummary,
};

/// This domain's slice of the OpenAPI document (ADR-035), merged by [`super::openapi::document`].
#[derive(utoipa::OpenApi)]
#[openapi(paths(
    get_topology,
    get_topology_links,
    get_topology_map,
    get_link_overrides,
    create_link_override,
    delete_link_override,
    get_topology_shadow,
    set_topology_mode,
))]
pub(super) struct Doc;

/// The topology routes, merged into `/api/v1` by [`super::router`].
pub(crate) fn routes() -> Router<ApiState> {
    Router::new()
        .route("/api/v1/topology", get(get_topology))
        .route("/api/v1/topology/links", get(get_topology_links))
        .route("/api/v1/topology/map", get(get_topology_map))
        .route(
            "/api/v1/topology/link-overrides",
            get(get_link_overrides).post(create_link_override),
        )
        .route(
            "/api/v1/topology/link-overrides/:id",
            delete(delete_link_override),
        )
        .route("/api/v1/topology/shadow", get(get_topology_shadow))
        // ⚠️ Write-only, deliberately. A `GET /api/v1/settings/topology` would be a configuration
        // read with no MCP tool, which raises `MCP_PENDING` — a ratchet that only moves down. The
        // mode is already returned by `/topology/shadow`, which does have one, so the read exists
        // without the gap. This is the ledger doing its job; do not route around it.
        .route(
            "/api/v1/settings/topology",
            axum::routing::put(set_topology_mode),
        )
}

/// One node in the dependency/topology graph.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub(crate) struct TopologyNode {
    pub id: Uuid,
    pub name: String,
    /// Upstream parent in the dependency graph (`null` ⇒ a root).
    pub parent_id: Option<Uuid>,
    pub state: NodeState,
    /// Upstream node currently identified as the root cause of this node's alert (dependency
    /// suppression), if any — lets a client collapse downstream alerts under the cause.
    pub root_cause: Option<Uuid>,
}

/// One keyset page of the dependency graph.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub(crate) struct TopologyPage {
    pub nodes: Vec<TopologyNode>,
    /// Pass back as `cursor` for the next page; `null` ⇒ this was the last one.
    pub next_cursor: Option<String>,
}

/// Assemble one page of the dependency graph, enriched with live state and root cause.
///
/// `limit` is the page size the caller has already clamped — this fetches `limit + 1` rows to
/// detect a further page and truncates before returning, so `next_cursor` is `Some` if and only if
/// there really is more.
///
/// Scoped at the row source, so an out-of-scope node never enters the page — including as somebody
/// else's `parent_id`. A visible node whose parent is not visible therefore arrives with a
/// `parent_id` pointing at a node absent from the response, and the graph draws it as a root. That
/// is the honest rendering: the caller is not shown the parent, so they are not told about it.
pub(crate) async fn topology_page(
    st: &ApiState,
    admin: &super::AdminState,
    scope: &super::scope::NodeScope,
    cursor: Option<Uuid>,
    limit: i64,
) -> Result<TopologyPage, ApiError> {
    let mut rows = admin
        .repo
        .list_topology_page(scope.group_filter(), cursor, limit + 1)
        .await
        .map_err(|e| {
            ApiError::from_internal(e.as_ref(), "topology list nodes", "failed to load topology")
        })?;
    let has_more = rows.len() as i64 > limit;
    if has_more {
        rows.truncate(limit as usize);
    }
    let next_cursor = if has_more {
        rows.last().map(|r| r.id.to_string())
    } else {
        None
    };

    let states = st.alerts.node_states();
    let root_causes = root_cause_map(st);
    // Batch the coarse fallback probe for this page's unobserved nodes: a single TSDB query rather
    // than one `latest()` round-trip per node (see `fresh_fallback_ids`). After a core restart
    // (empty `states`) the per-node version fired one VM query for every node.
    let unobserved: Vec<NodeId> = rows
        .iter()
        .map(|r| NodeId::from(r.id))
        .filter(|id| !states.contains_key(id))
        .collect();
    let fresh_fallback: HashSet<Uuid> = fresh_fallback_ids(st, &unobserved).await;

    let nodes = rows
        .into_iter()
        .map(|r| {
            let nid = NodeId::from(r.id);
            TopologyNode {
                id: r.id,
                name: r.name,
                parent_id: r.parent_id,
                // The rule lives in `nodes::state_or_fallback`, not here: this map was a fourth
                // hand-written copy of it, and the copies had already drifted.
                state: state_or_fallback(states.get(&nid).copied(), fresh_fallback.contains(&r.id)),
                root_cause: root_causes.get(&r.id).copied(),
            }
        })
        .collect();
    Ok(TopologyPage { nodes, next_cursor })
}

/// node → the upstream node its alert is suppressed under (from active, suppressed alerts).
///
/// Shared by the dependency graph and the network map, so "which node is blamed" is read once.
fn root_cause_map(st: &ApiState) -> HashMap<Uuid, Uuid> {
    let mut root_causes: HashMap<Uuid, Uuid> = HashMap::new();
    for a in st.alerts.active_alerts() {
        // Node subjects only — both graphs key on nodes and have no pool vertices.
        if let (Some(node), Some(cause)) = (a.node(), a.root_cause) {
            root_causes
                .entry(node.as_uuid())
                .or_insert_with(|| cause.as_uuid());
        }
    }
    root_causes
}

/// The dependency graph: every node with its parent edge, current state, and any active root-cause
/// attribution. Admin-only data source.
///
/// The default page is large — the graph views assemble the whole fleet, so fewer round-trips is
/// better — but bounded, so no single response is a multi-MB blob.
#[utoipa::path(
    get, path = "/api/v1/topology", tag = "topology",
    params(NodePageQuery),
    responses(
        (status = 200, description = "One keyset page of the dependency graph; `next_cursor` is null on the last page", body = TopologyPage),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks the view permission", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode has no inventory to build the graph from", body = super::error::ErrorBody),
    ),
)]
async fn get_topology(
    _perm: RequireView,
    Scoped(scope): Scoped,
    admin: Admin,
    axum::extract::State(st): axum::extract::State<ApiState>,
    Query(q): Query<NodePageQuery>,
) -> ApiResult<Json<TopologyPage>> {
    let limit = q.limit.unwrap_or(2000).clamp(1, 5000);
    Ok(Json(
        topology_page(&st, &admin, &scope, q.cursor, limit).await?,
    ))
}

// ── The derived connectivity graph (ADR-043) ─────────────────────────────────

/// One undirected link in the derived connectivity graph.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub(crate) struct TopologyLink {
    /// Stable id, and the keyset cursor.
    pub id: i64,
    /// One endpoint. `null` is reserved for an endpoint that is not a monitored node.
    pub a_node: Option<Uuid>,
    /// The other endpoint. `null` is reserved for an endpoint that is not a monitored node.
    pub b_node: Option<Uuid>,
    /// `ifIndex` on the `a` side, when a source reported one.
    pub a_ifindex: Option<i32>,
    /// `ifIndex` on the `b` side, when a source reported one.
    pub b_ifindex: Option<i32>,
    /// Port name on the `a` side, when a source reported one.
    pub a_if_name: Option<String>,
    /// Port name on the `b` side, when a source reported one.
    pub b_if_name: Option<String>,
    /// Every kind of evidence that produced this link.
    pub sources: Vec<LinkSource>,
    /// The strongest of `sources` — what to label the link with.
    pub source: LinkSource,
    /// The subnet behind a shared-subnet link, e.g. `192.168.1.0/24`.
    pub subnet: Option<String>,
    /// The endpoint an operator declared upstream, when one was declared. `null` means the direction
    /// is worked out from how far each end is from a poller.
    pub forced_parent: Option<Uuid>,
    /// When this link was first derived (RFC 3339).
    pub first_seen: String,
    /// When it was last confirmed (RFC 3339).
    pub last_seen: String,
}

/// One keyset page of the derived connectivity graph, with what the last derivation run saw.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub(crate) struct TopologyLinkPage {
    pub links: Vec<TopologyLink>,
    /// Pass back as `cursor` for the next page; `null` ⇒ this was the last one.
    pub next_cursor: Option<i64>,
    /// Counters for everything the last derivation declined to turn into a link.
    pub summary: TopologyLinkSummary,
    /// How many links the whole graph holds, not just this page.
    pub total_links: i64,
    /// When the graph was last derived (RFC 3339), or `null` before the first run.
    pub derived_at: Option<String>,
}

/// Query parameters for one page of the connectivity graph.
#[derive(Debug, Clone, serde::Deserialize, utoipa::IntoParams)]
pub(crate) struct LinkPageQuery {
    /// Return links with an id greater than this (the previous page's `next_cursor`).
    pub cursor: Option<i64>,
    /// Maximum links to return.
    pub limit: Option<i64>,
}

/// Assemble one page of the derived connectivity graph.
///
/// The seam the REST handler and the MCP `get_topology` tool both call, so the page is assembled
/// once in the codebase — the same arrangement [`topology_page`] has.
///
/// Fetches `limit + 1` rows to detect a further page and truncates before returning, so
/// `next_cursor` is `Some` if and only if there really is more.
pub(crate) async fn topology_link_page(
    admin: &super::AdminState,
    scope: &super::scope::NodeScope,
    cursor: Option<i64>,
    limit: i64,
) -> Result<TopologyLinkPage, ApiError> {
    let mut rows = admin
        .topology_links
        .list_page(scope.group_filter(), cursor, limit + 1)
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "topology list links",
                "failed to load topology links",
            )
        })?;
    let next_cursor = (rows.len() as i64 > limit).then(|| {
        rows.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
        rows.last().map(|r| r.id).unwrap_or_default()
    });

    let last = admin.topology_links.last_run().await.unwrap_or(None);
    let links = rows
        .into_iter()
        .map(|r| TopologyLink {
            id: r.id,
            a_node: r.a_node.map(|n| n.as_uuid()),
            b_node: r.b_node.map(|n| n.as_uuid()),
            a_ifindex: r.a_ifindex,
            b_ifindex: r.b_ifindex,
            a_if_name: r.a_if_name,
            b_if_name: r.b_if_name,
            // Derived rather than stored, so "which sources saw this" and "what do we call it"
            // cannot drift apart. A row whose tokens were all unknown falls back to the weakest
            // source rather than failing the page.
            source: LinkSource::best(&r.sources).unwrap_or(LinkSource::L3Subnet),
            sources: r.sources,
            subnet: r.subnet,
            forced_parent: r.forced_parent.map(|n| n.as_uuid()),
            first_seen: r.first_seen.to_rfc3339(),
            last_seen: r.last_seen.to_rfc3339(),
        })
        .collect();

    Ok(TopologyLinkPage {
        links,
        next_cursor,
        summary: last.as_ref().map(|l| l.summary.clone()).unwrap_or_default(),
        total_links: last.as_ref().map_or(0, |l| l.link_count),
        derived_at: last.map(|l| l.derived_at.to_rfc3339()),
    })
}

/// The derived connectivity graph: every link between monitored nodes, with the evidence that
/// produced it.
///
/// Links are derived from CDP/LLDP adjacency and from nodes sharing an IP subnet; they are
/// recomputed periodically rather than stored by hand. A group-scoped caller sees only links whose
/// **both** endpoints are visible to them.
#[utoipa::path(
    get, path = "/api/v1/topology/links", tag = "topology",
    params(LinkPageQuery),
    responses(
        (status = 200, description = "One keyset page of the connectivity graph; `next_cursor` is null on the last page", body = TopologyLinkPage),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks the view permission", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode has no inventory to build the graph from", body = super::error::ErrorBody),
    ),
)]
async fn get_topology_links(
    _perm: RequireView,
    Scoped(scope): Scoped,
    admin: Admin,
    Query(q): Query<LinkPageQuery>,
) -> ApiResult<Json<TopologyLinkPage>> {
    let limit = q.limit.unwrap_or(2000).clamp(1, 2000);
    Ok(Json(
        topology_link_page(&admin, &scope, q.cursor, limit).await?,
    ))
}

// ── One folder level of the network map (ADR-191) ───────────────────────────────

/// What one end of a map edge is: a node on this level, a subfolder's box, or a stub for
/// something outside the level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MapEndpointKind {
    Node,
    Folder,
    External,
}

/// What a stub stands for: one node, or a folder holding the far ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MapStubKind {
    Node,
    Folder,
}

/// One end of a map edge. `id` is a node id for `node`, a folder id for `folder`, and the stub's
/// own `id` for `external`.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub(crate) struct MapEndpoint {
    pub kind: MapEndpointKind,
    pub id: Uuid,
}

/// A folder named on the map: the level itself or one of its ancestors.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub(crate) struct MapBreadcrumb {
    pub id: Uuid,
    pub name: String,
}

/// A subfolder drawn as one box, with its whole subtree's tally (visible nodes only).
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub(crate) struct MapFolder {
    pub id: Uuid,
    pub name: String,
    /// The folder's type key (`site`, `region`, …).
    pub group_type: String,
    /// How many nodes the subtree holds.
    pub node_count: i64,
    /// Those nodes, by state.
    pub counts: super::fleet::GroupStateCounts,
}

/// A node drawn on the level that has at least one link there. On an ordinary level it sits
/// directly in the level's folder; on a `flattened` level it may sit anywhere in the subtree.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub(crate) struct MapNode {
    pub id: Uuid,
    pub name: String,
    pub state: NodeState,
    /// Upstream node blamed for this node's alert (dependency suppression), if any.
    pub root_cause: Option<Uuid>,
    /// On a `flattened` level, the folders between the level and the node, outermost first, ending
    /// with the one the node is filed in; empty when it sits directly in the level's own folder
    /// (always empty on an ordinary level).
    pub folder_path: Vec<MapBreadcrumb>,
}

/// Where links leave the level: a node or a folder outside it.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub(crate) struct MapStub {
    pub kind: MapStubKind,
    /// The node or folder the stub stands for.
    pub id: Uuid,
    pub name: String,
    /// The first level on which both ends are visible; open it to see where the links go.
    /// `null` is the whole network.
    pub level_group: Option<Uuid>,
}

/// One link inside a bundled edge, oriented so `a_node` sits at the edge's `a` end.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub(crate) struct MapEdgeMember {
    pub link_id: i64,
    pub a_node: Uuid,
    pub b_node: Uuid,
    pub a_if_name: Option<String>,
    pub b_if_name: Option<String>,
    /// The strongest evidence behind this link.
    pub source: LinkSource,
    /// The subnet behind a shared-subnet link.
    pub subnet: Option<String>,
}

/// Every link between the same two things on this level, drawn as one line.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub(crate) struct MapEdge {
    /// Stable id, built from the two ends.
    pub id: String,
    pub a: MapEndpoint,
    pub b: MapEndpoint,
    /// How many links the bundle holds (`members` lists at most 50).
    pub count: i64,
    /// The strongest of `sources`.
    pub source: LinkSource,
    /// Every kind of evidence behind any member.
    pub sources: Vec<LinkSource>,
    /// The links, strongest evidence first.
    pub members: Vec<MapEdgeMember>,
}

/// One folder level of the network map.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub(crate) struct MapLevel {
    /// The folder drawn; `null` is the whole network.
    pub group: Option<MapBreadcrumb>,
    /// Its ancestors, outermost first (the level itself is not included).
    pub breadcrumbs: Vec<MapBreadcrumb>,
    /// Its subfolders, each drawn as a box (empty on a `flattened` level).
    pub folders: Vec<MapFolder>,
    /// How many subfolders sit directly in it — on a `flattened` level too, where none is a box.
    pub subfolder_count: i64,
    /// Its own nodes that have a link on this level — on a `flattened` level, every such node in
    /// the subtree.
    pub nodes: Vec<MapNode>,
    /// The places links leave the level for.
    pub stubs: Vec<MapStub>,
    pub edges: Vec<MapEdge>,
    /// Every node directly in this folder, linked or not — on a `flattened` level, every node in
    /// the subtree.
    pub direct_node_count: i64,
    /// Of those, how many have a link on this level.
    pub linked_node_count: i64,
    /// How many bundled edges the level has (also when `overflow` emptied `edges`).
    pub edge_count: i64,
    /// Direct nodes with no link on this level: counted, not drawn.
    pub isolated_count: i64,
    /// The level is too large to draw: `nodes`, `stubs` and `edges` are empty, the boxes remain.
    pub overflow: bool,
    /// The level is a Site folder or lies beneath one, so its subfolders are not boxes: every node
    /// in the subtree is drawn, tagged with its `folder_path`. `false` when the flat drawing would
    /// exceed the bounds, in which case the level falls back to boxes.
    pub flattened: bool,
    /// The most linked nodes a level draws.
    pub node_limit: i64,
    /// The most edges a level draws.
    pub edge_limit: i64,
    /// When the connectivity graph was last derived (RFC 3339), or `null` before the first run.
    pub derived_at: Option<String>,
}

/// Query parameters for one map level.
#[derive(Debug, Clone, Deserialize, utoipa::IntoParams)]
pub(crate) struct MapLevelQuery {
    /// The folder to draw. Omit for the whole network.
    pub group: Option<Uuid>,
}

/// Assemble one map level: the seam the REST handler and the MCP `get_topology` tool both call.
pub(crate) async fn topology_map_level(
    st: &ApiState,
    admin: &super::AdminState,
    scope: &super::scope::NodeScope,
    group: Option<Uuid>,
) -> Result<MapLevel, ApiError> {
    fn load(what: &'static str) -> impl Fn(anyhow::Error) -> ApiError {
        move |e| ApiError::from_internal(e.as_ref(), what, "failed to load the network map")
    }
    let edges = admin
        .groups
        .cached_edges()
        .await
        .map_err(load("map group edges"))?;
    if let Some(g) = group {
        // Scope first, existence second: a scoped caller cannot tell a hidden folder from none.
        super::scope::require_visible_group(scope, g)?;
        if !edges.iter().any(|(id, _)| *id == g) {
            return Err(ApiError::not_found(
                "group_not_found",
                format!("no group {g}"),
            ));
        }
    }
    let folders: Vec<crate::topology_level::FolderRow> =
        super::groups::visible_groups(admin, scope)
            .await?
            .iter()
            .map(Into::into)
            .collect();
    let visible: Option<HashSet<Uuid>> = scope.group_filter().map(|v| v.iter().copied().collect());
    let node_groups = st
        .nodes
        .node_group_map(scope.group_filter())
        .await
        .map_err(load("map node groups"))?;
    let raw = st.alerts.node_states();
    let any_unobserved = node_groups
        .iter()
        .any(|(id, _)| !raw.contains_key(&NodeId::from(*id)));
    let fresh = if any_unobserved {
        super::nodes::fresh_fleet_ids(st.store.as_ref()).await
    } else {
        HashSet::new()
    };
    let states: HashMap<Uuid, NodeState> = node_groups
        .iter()
        .map(|(id, _)| {
            let s = state_or_fallback(raw.get(&NodeId::from(*id)).copied(), fresh.contains(id));
            (*id, s)
        })
        .collect();
    let root_causes = root_cause_map(st);
    let links = admin
        .topology_links
        .all_stored_links()
        .await
        .map_err(load("map links"))?;

    let mut level = crate::topology_level::compute(&crate::topology_level::LevelInput {
        level: group,
        edges: &edges,
        groups: &folders,
        visible: visible.as_ref(),
        node_groups: &node_groups,
        states: &states,
        root_causes: &root_causes,
        links: &links,
    });
    let names =
        super::nodes::resolve_node_names(st, scope, crate::topology_level::names_needed(&level))
            .await;
    crate::topology_level::apply_names(&mut level, &names);
    level.derived_at = admin
        .topology_links
        .last_run()
        .await
        .unwrap_or(None)
        .map(|l| l.derived_at.to_rfc3339());
    Ok(level)
}

/// One folder level of the network map: the folder's own linked nodes, each subfolder as one box
/// with its subtree's counts, links between the same two things bundled into one edge, and a stub
/// for every place links leave the level.
///
/// Omit `group` for the whole network. A group-scoped caller sees their visible folders, with the
/// roots of their scope directly under the whole network, and only links whose **both** ends are
/// visible to them. A level with more than `node_limit` linked nodes or `edge_limit` edges answers
/// `overflow: true` with its boxes and no nodes or edges.
///
/// A Site folder, and every folder beneath one, is drawn flat (`flattened: true`): its subfolders
/// are not boxes, every node in its subtree is drawn and carries the folders down to the one it is
/// filed in (`folder_path`). A link to another folder of the same site leaves as a stub for the far
/// node itself, since the site draws it. A flat drawing over the bounds falls back to boxes.
#[utoipa::path(
    get, path = "/api/v1/topology/map", tag = "topology",
    params(MapLevelQuery),
    responses(
        (status = 200, description = "One level of the network map", body = MapLevel),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks the view permission", body = super::error::ErrorBody),
        (status = 404, description = "No folder with that id that the caller can see", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode has no inventory to build the map from", body = super::error::ErrorBody),
    ),
)]
async fn get_topology_map(
    _perm: RequireView,
    Scoped(scope): Scoped,
    admin: Admin,
    axum::extract::State(st): axum::extract::State<ApiState>,
    Query(q): Query<MapLevelQuery>,
) -> ApiResult<Json<MapLevel>> {
    Ok(Json(
        topology_map_level(&st, &admin, &scope, q.group).await?,
    ))
}

// ── Operator decisions about links (ADR-043 decision 4) ───────────────────────────

/// One operator decision about one link.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub(crate) struct LinkOverrideRow {
    pub id: Uuid,
    /// The lower-ordered endpoint. Endpoints are stored in a canonical order, so this is not
    /// necessarily the one that was submitted first.
    pub a_node: Uuid,
    /// The higher-ordered endpoint.
    pub b_node: Uuid,
    pub action: LinkOverrideAction,
    /// Which endpoint is upstream. Present only when `action` is `direction`.
    pub direction: Option<LinkDirection>,
    pub note: Option<String>,
    /// Who recorded the decision.
    pub created_by: Option<String>,
    /// When it was recorded (RFC 3339).
    pub created_at: String,
}

/// Every operator decision visible to the caller.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub(crate) struct LinkOverrideList {
    pub overrides: Vec<LinkOverrideRow>,
}

/// A decision to record about one link.
#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
pub(crate) struct LinkOverrideRequest {
    /// One endpoint. Order is not significant — the pair is canonicalized before storing, and a
    /// `direction` is re-expressed to match.
    pub a_node: Uuid,
    /// The other endpoint.
    pub b_node: Uuid,
    /// `pin`, `hide` or `direction`.
    pub action: LinkOverrideAction,
    /// Which endpoint is upstream. Required when `action` is `direction`, rejected otherwise.
    pub direction: Option<LinkDirection>,
    /// Free-text note for whoever reads this decision later.
    pub note: Option<String>,
}

/// Operator decisions that override the derived connectivity graph.
///
/// A group-scoped caller sees only decisions whose **both** endpoints are visible to them, matching
/// how the links themselves are filtered.
#[utoipa::path(
    get, path = "/api/v1/topology/link-overrides", tag = "topology",
    responses(
        (status = 200, description = "Every visible link override", body = LinkOverrideList),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks the view permission", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode has no write side", body = super::error::ErrorBody),
    ),
)]
async fn get_link_overrides(
    _perm: RequireView,
    Scoped(scope): Scoped,
    admin: Admin,
    axum::extract::State(st): axum::extract::State<ApiState>,
) -> ApiResult<Json<LinkOverrideList>> {
    Ok(Json(link_override_list(&st, &admin, &scope).await?))
}

/// Assemble the visible override list — the seam the REST handler and the MCP `get_topology` tool
/// both call, so the scope filter is written once.
pub(crate) async fn link_override_list(
    st: &ApiState,
    admin: &super::AdminState,
    scope: &super::scope::NodeScope,
) -> Result<LinkOverrideList, ApiError> {
    let rows = admin.link_overrides.list().await.map_err(|e| {
        ApiError::from_internal(
            e.as_ref(),
            "list link overrides",
            "failed to load link overrides",
        )
    })?;
    let overrides = rows
        .into_iter()
        // Both endpoints, for the same reason the links themselves need both: returning a decision
        // with one visible end would tell a scoped operator that the other node exists.
        .filter(|o| scope.allows_node(st, o.a_node) && scope.allows_node(st, o.b_node))
        .map(|o| LinkOverrideRow {
            id: o.id,
            a_node: o.a_node.as_uuid(),
            b_node: o.b_node.as_uuid(),
            action: o.action,
            direction: o.direction,
            note: o.note,
            created_by: o.created_by,
            created_at: o.created_at.to_rfc3339(),
        })
        .collect();
    Ok(LinkOverrideList { overrides })
}

/// Record a decision about a link, replacing any previous decision of the same kind for that pair.
///
/// The decision takes effect on the next derivation cycle. A pinned link is re-emitted by every run,
/// so it never expires the way an unobserved derived link does.
#[utoipa::path(
    post, path = "/api/v1/topology/link-overrides", tag = "topology",
    request_body = LinkOverrideRequest,
    responses(
        (status = 200, description = "The decision was recorded", body = super::util::CreatedId),
        (status = 400, description = "The two endpoints are the same node, or `direction` disagrees with `action`", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks the manage-config permission", body = super::error::ErrorBody),
        (status = 404, description = "One of the endpoints is not a node the caller can see", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode has no write side", body = super::error::ErrorBody),
    ),
)]
async fn create_link_override(
    _perm: RequireManageConfig,
    Scoped(scope): Scoped,
    Actor(actor): Actor,
    admin: Admin,
    axum::extract::State(st): axum::extract::State<ApiState>,
    Json(req): Json<LinkOverrideRequest>,
) -> ApiResult<Json<super::util::CreatedId>> {
    let (a, b) = (NodeId::from(req.a_node), NodeId::from(req.b_node));
    if a == b {
        return Err(ApiError::bad_request(
            "invalid_link",
            "a link needs two different nodes",
        ));
    }
    // `direction` and `action` must agree. Rejecting the two inconsistent combinations here is what
    // lets every reader downstream trust that a stored `direction` row carries a direction — the
    // alternative is a row that parses fine and silently forces nothing.
    let wants_direction = req.action == LinkOverrideAction::Direction;
    if wants_direction != req.direction.is_some() {
        return Err(ApiError::bad_request(
            "invalid_direction",
            "`direction` is required for action `direction` and not allowed for any other",
        ));
    }
    // Both endpoints, checked against the caller's scope before anything is written. A scoped
    // operator must not be able to attach a decision to a node outside their scope, and must not
    // learn that it exists either — hence 404 rather than 403.
    super::scope::require_visible_node(&st, &scope, a)?;
    super::scope::require_visible_node(&st, &scope, b)?;

    let id = admin
        .link_overrides
        .upsert(
            a,
            b,
            req.action,
            req.direction,
            req.note.as_deref(),
            actor.as_deref(),
        )
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "upsert link override",
                "failed to record the link override",
            )
        })?;
    Ok(Json(super::util::CreatedId { id }))
}

/// Remove an operator decision, letting the derivation's own answer stand again.
#[utoipa::path(
    delete, path = "/api/v1/topology/link-overrides/{id}", tag = "topology",
    params(("id" = Uuid, Path, description = "The override's id")),
    responses(
        (status = 204, description = "The decision was removed"),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks the manage-config permission", body = super::error::ErrorBody),
        (status = 404, description = "No such override, or its endpoints are not visible to the caller", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode has no write side", body = super::error::ErrorBody),
    ),
)]
async fn delete_link_override(
    _perm: RequireManageConfig,
    Scoped(scope): Scoped,
    admin: Admin,
    axum::extract::State(st): axum::extract::State<ApiState>,
    Path(id): Path<Uuid>,
) -> ApiResult<axum::http::StatusCode> {
    let ends = admin.link_overrides.endpoints(id).await.map_err(|e| {
        ApiError::from_internal(
            e.as_ref(),
            "read link override",
            "failed to read the link override",
        )
    })?;
    let Some((a, b)) = ends else {
        return Err(ApiError::not_found(
            "override_not_found",
            format!("no link override {id}"),
        ));
    };
    // Scope-check before deleting, and answer 404 either way, so an out-of-scope id is
    // indistinguishable from one that does not exist.
    super::scope::require_visible_node(&st, &scope, a)?;
    super::scope::require_visible_node(&st, &scope, b)?;
    admin.link_overrides.delete(id).await.map_err(|e| {
        ApiError::from_internal(
            e.as_ref(),
            "delete link override",
            "failed to delete the link override",
        )
    })?;
    Ok(axum::http::StatusCode::NO_CONTENT)
}

// ── Shadow mode: what the derived graph would do (ADR-043 decision 5) ─────────────

/// One node whose suppression would change if the deployment moved to the derived graph.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub(crate) struct ShadowAlert {
    /// The node whose active alert is affected.
    pub node_id: Uuid,
    /// The node the derived graph blames, when it has one.
    pub root_cause: Option<Uuid>,
}

/// One node whose parent set differs between the two graphs.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub(crate) struct ShadowEdge {
    /// The downstream node.
    pub child: Uuid,
    /// The upstream node.
    pub parent: Uuid,
}

/// What the derived dependency graph would do, against what the manual one does.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub(crate) struct TopologyShadow {
    /// `manual`, `shadow` or `derived`.
    pub mode: crate::topology_mode::TopologyMode,
    /// Edges in the hand-authored graph.
    pub manual_edges: usize,
    /// Edges in the derived graph.
    pub derived_edges: usize,
    /// Parent edges only the hand-authored graph has.
    pub only_in_manual: Vec<ShadowEdge>,
    /// Parent edges only the derived graph has.
    pub only_in_derived: Vec<ShadowEdge>,
    /// Active alerts the derived graph **would suppress** and the manual one does not.
    ///
    /// The number to review before enabling `derived`: each of these is an alert that would stop
    /// being raised.
    pub would_suppress: Vec<ShadowAlert>,
    /// Active alerts the manual graph suppresses and the derived one would not — the noise
    /// direction.
    pub would_unsuppress: Vec<ShadowAlert>,
    /// Nodes the derived graph treats as roots, because a poller sits on their segment.
    pub anchors: Vec<Uuid>,
    /// Pools with at least one poller whose location could not be resolved.
    ///
    /// **Non-empty blocks `derived`.** A pool with no anchor contributes no roots, so none of its
    /// nodes would ever be suppressed while every screen showed the feature as on.
    pub unresolved_pools: Vec<String>,
    /// Poller ids that could not be placed, so an operator knows which to give an anchor.
    pub unresolved_pollers: Vec<String>,
    /// When the mode was last changed (RFC 3339), or `null` if it never has been.
    pub mode_since: Option<String>,
    /// How many nodes the derived graph gives at least one upstream.
    ///
    /// Read with `total_nodes`: a low ratio means most of the fleet would keep alerting exactly as
    /// it does now, which is usually the point rather than a problem.
    pub covered_nodes: usize,
    /// Nodes in the inventory.
    pub total_nodes: usize,
    /// Nodes an operator has excluded from derived suppression entirely.
    ///
    /// A list rather than a count: the screen that shows the comparison is also where the exclusion
    /// is toggled, and a count cannot tell a row whether it is the excluded one. Bounded by what an
    /// operator typed in, not by the fleet.
    pub opted_out: Vec<Uuid>,
}

/// Cap on how many differing edges or affected alerts are listed.
///
/// The counts above are exact; the lists are evidence. An unbounded diff on a fleet whose manual
/// graph was never filled in is the whole fleet, which is a multi-megabyte response answering a
/// question the count already answered.
const SHADOW_LIST_CAP: usize = 500;

/// What the derived dependency graph would do to alerting, compared with the hand-authored one.
///
/// This is the review surface for enabling derived suppression: `would_suppress` lists the active
/// alerts that would stop being raised, and `unresolved_pools` lists the pollers that have no place
/// in the graph yet. Both are computed on demand and neither affects alerting.
#[utoipa::path(
    get, path = "/api/v1/topology/shadow", tag = "topology",
    responses(
        (status = 200, description = "The comparison between the manual and derived dependency graphs", body = TopologyShadow),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks the view permission", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode has no inventory to compare", body = super::error::ErrorBody),
    ),
)]
async fn get_topology_shadow(
    _perm: RequireView,
    Scoped(scope): Scoped,
    admin: Admin,
    axum::extract::State(st): axum::extract::State<ApiState>,
) -> ApiResult<Json<TopologyShadow>> {
    Ok(Json(topology_shadow(&st, &admin, &scope).await?))
}

/// Compare the two dependency graphs — the seam the REST handler and the MCP `get_topology` tool
/// both call, so an operator reviewing in the UI and a model reviewing over MCP are told the same
/// thing.
pub(crate) async fn topology_shadow(
    st: &ApiState,
    admin: &super::AdminState,
    scope: &super::scope::NodeScope,
) -> Result<TopologyShadow, ApiError> {
    let nodes = admin.repo.list_nodes().await.map_err(|e| {
        ApiError::from_internal(
            e.as_ref(),
            "shadow list nodes",
            "failed to load the topology",
        )
    })?;
    let sources = crate::topology_projection::TopologySources {
        links: admin.topology_links.clone(),
        pollers: admin.pollers.clone(),
        l3: admin.l3.clone(),
        nodes: admin.repo.clone(),
    };
    let manual = crate::topology_projection::manual_topology(&nodes);
    let (derived, resolution) =
        crate::topology_projection::derived_topology(&sources, &nodes).await;

    // Both graphs are built over the whole fleet — a graph filtered to one operator's groups would
    // attribute a root cause to the nearest *visible* node rather than the actual one. The scope is
    // applied to what is *listed*, after the comparison.
    let visible = |id: NodeId| scope.allows_node(st, id);
    let mut only_in_manual = Vec::new();
    let mut only_in_derived = Vec::new();
    for node in &nodes {
        let m = manual.parents_of(node.id);
        let d = derived.parents_of(node.id);
        if !visible(node.id) {
            continue;
        }
        for p in m.difference(&d).copied().filter(|p| visible(*p)) {
            only_in_manual.push(ShadowEdge {
                child: node.id.as_uuid(),
                parent: p.as_uuid(),
            });
        }
        for p in d.difference(&m).copied().filter(|p| visible(*p)) {
            only_in_derived.push(ShadowEdge {
                child: node.id.as_uuid(),
                parent: p.as_uuid(),
            });
        }
    }

    // The engine's own view of what is down, reused rather than recomputed: a second derivation of
    // "which nodes are down" would let the preview and the engine disagree about the very thing the
    // preview exists to predict.
    let down: BTreeSet<NodeId> = st.alerts.down_set();
    let mut would_suppress = Vec::new();
    let mut would_unsuppress = Vec::new();
    for alert in st.alerts.active_alerts() {
        // Node subjects only: suppression is a statement about the dependency graph, and a
        // pool-coverage alert is not a vertex in it (nor is it ever suppressed — it is raised
        // with `root_cause: None`).
        let Some(alert_node) = alert.node() else {
            continue;
        };
        if !visible(alert_node) {
            continue;
        }
        let m = manual.is_suppressed(alert_node, &down);
        let d = derived.is_suppressed(alert_node, &down);
        if d && !m {
            would_suppress.push(ShadowAlert {
                node_id: alert_node.as_uuid(),
                root_cause: derived
                    .root_cause(alert_node, &down)
                    .filter(|c| visible(*c))
                    .map(|c| c.as_uuid()),
            });
        } else if m && !d {
            would_unsuppress.push(ShadowAlert {
                node_id: alert_node.as_uuid(),
                root_cause: manual
                    .root_cause(alert_node, &down)
                    .filter(|c| visible(*c))
                    .map(|c| c.as_uuid()),
            });
        }
    }

    let manual_edges = manual.edge_count();
    let derived_edges = derived.edge_count();
    for v in [&mut only_in_manual, &mut only_in_derived] {
        v.truncate(SHADOW_LIST_CAP);
    }
    for v in [&mut would_suppress, &mut would_unsuppress] {
        v.truncate(SHADOW_LIST_CAP);
    }
    Ok(TopologyShadow {
        mode: admin.repo.get_topology_mode().await,
        manual_edges,
        derived_edges,
        only_in_manual,
        only_in_derived,
        would_suppress,
        would_unsuppress,
        anchors: resolution
            .anchors
            .iter()
            .filter(|id| visible(**id))
            .map(|id| id.as_uuid())
            .collect(),
        unresolved_pools: resolution.unresolved.keys().cloned().collect(),
        unresolved_pollers: resolution.unresolved.values().flatten().cloned().collect(),
        // The checklist below is **advisory and stays advisory**. Enforcing "shadow for 24 hours"
        // would put approval back on a schedule, which is the input cost ADR-043 exists to remove;
        // the one genuinely blocking condition is `unresolved_pools`, above.
        mode_since: admin
            .repo
            .topology_mode_since()
            .await
            .map(|t| t.to_rfc3339()),
        covered_nodes: nodes
            .iter()
            .filter(|n| visible(n.id) && !derived.parents_of(n.id).is_empty())
            .count(),
        total_nodes: nodes.iter().filter(|n| visible(n.id)).count(),
        opted_out: admin
            .repo
            .suppression_opt_outs()
            .await
            .iter()
            .filter(|id| visible(**id))
            .map(|id| id.as_uuid())
            .collect(),
    })
}

/// The topology mode to move the deployment to.
#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
pub(crate) struct TopologyModeRequest {
    /// `manual`, `shadow` or `derived`.
    pub mode: crate::topology_mode::TopologyMode,
}

/// Choose which dependency graph drives alert suppression.
///
/// `manual` uses each node's hand-authored parent. `shadow` changes nothing about alerting and only
/// makes the comparison at `GET /api/v1/topology/shadow` meaningful. `derived` hands suppression to
/// the graph derived from CDP/LLDP adjacency and shared subnets.
///
/// Moving to `derived` is **refused** while any pool that has nodes has a poller whose location
/// could not be resolved: such a pool contributes no roots, so none of its nodes would ever be
/// suppressed — the change would look like it worked and quietly do nothing.
#[utoipa::path(
    put, path = "/api/v1/settings/topology", tag = "settings",
    request_body = TopologyModeRequest,
    responses(
        (status = 204, description = "The mode was changed"),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks the manage-config permission", body = super::error::ErrorBody),
        (status = 409, description = "`derived` was requested while a pool with nodes has an unplaced poller", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode has no write side", body = super::error::ErrorBody),
    ),
)]
async fn set_topology_mode(
    _perm: RequireManageConfig,
    admin: Admin,
    Json(req): Json<TopologyModeRequest>,
) -> ApiResult<axum::http::StatusCode> {
    if req.mode.uses_derived() {
        let nodes = admin.repo.list_nodes().await.map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "topology mode nodes",
                "failed to load the fleet",
            )
        })?;
        let sources = crate::topology_projection::TopologySources {
            links: admin.topology_links.clone(),
            pollers: admin.pollers.clone(),
            l3: admin.l3.clone(),
            nodes: admin.repo.clone(),
        };
        let resolution = crate::topology_projection::resolve(&sources, &nodes).await;
        // Only pools that actually have nodes block. An idle pool with a misconfigured poller is a
        // problem for that pool, not a reason to refuse a fleet the operator has already reviewed.
        // A node with no explicit pool belongs to `default` — the same fallback the coordinator
        // applies. Reading `None` as "no pool" here would let the fleet's unassigned majority slip
        // past the block entirely.
        let populated: BTreeSet<&str> = nodes
            .iter()
            .map(|n| n.pool.as_deref().unwrap_or("default"))
            .collect();
        let blocking: Vec<&str> = resolution
            .unresolved
            .keys()
            .map(String::as_str)
            .filter(|pool| populated.contains(pool))
            .collect();
        if !blocking.is_empty() {
            return Err(ApiError::conflict(
                "anchor_unresolved",
                format!(
                    "these pools have nodes but no placed poller, so nothing in them would ever \
                     be suppressed: {}. Set an anchor node for their pollers first.",
                    blocking.join(", ")
                ),
            ));
        }
    }
    admin.repo.set_topology_mode(req.mode).await.map_err(|e| {
        ApiError::from_internal(
            e.as_ref(),
            "set topology mode",
            "failed to change the topology mode",
        )
    })?;
    Ok(axum::http::StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::router;
    use crate::api::tests_support::{private_state, public_state};
    use axum::body::{to_bytes, Body};
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    async fn status_of(st: ApiState) -> StatusCode {
        status_of_path(st, "/api/v1/topology").await
    }

    async fn status_of_path(st: ApiState, path: &str) -> StatusCode {
        router(st)
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap()
            .status()
    }

    #[tokio::test]
    async fn the_links_endpoint_is_gated_before_its_subsystem_is_consulted() {
        // Same ordering property as the dependency graph: an anonymous caller learns only that it
        // is unauthenticated, never whether this deployment has a write side.
        assert_eq!(
            status_of_path(private_state(), "/api/v1/topology/links").await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            status_of_path(public_state(), "/api/v1/topology/links").await,
            StatusCode::SERVICE_UNAVAILABLE
        );
    }

    #[tokio::test]
    async fn the_link_page_shape_is_an_object_with_a_cursor_and_a_summary() {
        // The DTO is the contract for the WebUI, the MCP tool and the map. `summary` in particular
        // has to survive: it is the only thing that distinguishes "no links" from "links exist but
        // nothing could be matched", which is the acceptance instrument for the LLDP half.
        let page = TopologyLinkPage {
            links: vec![TopologyLink {
                id: 7,
                a_node: Some(Uuid::nil()),
                b_node: Some(Uuid::nil()),
                a_ifindex: Some(8),
                b_ifindex: None,
                a_if_name: Some("GigabitEthernet0/1".to_owned()),
                b_if_name: None,
                sources: vec![LinkSource::Lldp, LinkSource::L3Subnet],
                source: LinkSource::Lldp,
                subnet: Some("192.168.1.0/24".to_owned()),
                forced_parent: None,
                first_seen: "2026-08-04T00:00:00Z".to_owned(),
                last_seen: "2026-08-04T01:00:00Z".to_owned(),
            }],
            next_cursor: Some(7),
            total_links: 1,

            summary: TopologyLinkSummary {
                unmatched_lldp_rows: 3,
                ..TopologyLinkSummary::default()
            },
            derived_at: Some("2026-08-04T01:00:00Z".to_owned()),
        };
        let json = serde_json::to_value(&page).unwrap();
        assert!(json["links"].is_array());
        assert_eq!(json["links"][0]["source"], "lldp");
        assert_eq!(json["links"][0]["sources"][1], "l3_subnet");
        assert_eq!(json["links"][0]["subnet"], "192.168.1.0/24");
        assert!(json["links"][0]["b_if_name"].is_null());
        assert_eq!(json["next_cursor"], 7);
        assert_eq!(json["summary"]["unmatched_lldp_rows"], 3);
        assert!(json["derived_at"].is_string());
    }

    #[test]
    fn the_representative_source_is_the_strongest_of_the_sources_array() {
        // Derived rather than stored, so "which sources saw this" and "what do we call it" cannot
        // drift apart.
        assert_eq!(
            LinkSource::best(&[LinkSource::L3Subnet, LinkSource::Cdp, LinkSource::Lldp]),
            Some(LinkSource::Lldp)
        );
    }

    #[tokio::test]
    async fn an_anonymous_caller_is_told_only_that_it_is_unauthorized() {
        // `RequireView` runs before `Admin`, so a private skeleton deployment answers 401 rather
        // than 503 — an unauthenticated caller must not learn whether the write side exists.
        assert_eq!(status_of(private_state()).await, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn a_public_dashboard_reaches_the_availability_check() {
        // Reads are open on a public dashboard, so the request gets past the permission guard and
        // stops at `Admin` instead — proving the two guards are ordered, not merged.
        assert_eq!(
            status_of(public_state()).await,
            StatusCode::SERVICE_UNAVAILABLE
        );
    }

    #[tokio::test]
    async fn the_page_shape_is_an_object_with_a_cursor_not_a_bare_array() {
        // The DTO is the contract for both the WebUI and the MCP tool: a bare array would leave a
        // client unable to tell a full page from the end of the graph.
        let page = TopologyPage {
            nodes: vec![TopologyNode {
                id: Uuid::nil(),
                name: "edge-router-1".to_owned(),
                parent_id: None,
                state: NodeState::Ok,
                root_cause: None,
            }],
            next_cursor: Some(Uuid::nil().to_string()),
        };
        let json = serde_json::to_value(&page).unwrap();
        assert!(json["nodes"].is_array());
        assert_eq!(json["nodes"][0]["name"], "edge-router-1");
        assert_eq!(json["nodes"][0]["state"], "ok");
        assert!(json["nodes"][0]["parent_id"].is_null());
        assert_eq!(json["next_cursor"], Uuid::nil().to_string());
    }

    #[tokio::test]
    async fn body_is_serialized_by_the_dto_not_a_json_macro() {
        // Guards the migration itself: the handler must return `Json<TopologyPage>`, so a field
        // added to the DTO reaches the wire without anyone editing a `json!` literal.
        let resp = router(public_state())
            .oneshot(
                Request::builder()
                    .uri("/api/v1/topology")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        // Skeleton mode has no write side, so this is the typed 503 — not an empty 200.
        assert_eq!(json["error"]["code"], "admin_unavailable");
    }
    // ── An accepted write (ADR-115) ──────────────────────────────────────────────────

    /// An operator's decision about a link is stored — the one topology table that is a source of
    /// truth rather than a recomputable cache.
    ///
    /// `200`, not `201`: recording a decision is idempotent, so the endpoint documents an OK
    /// carrying the id rather than a creation.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn pinning_a_link_stores_the_override(pool: sqlx::PgPool) {
        use crate::api::tests_support::{live_state, send, token};
        let st = live_state(pool.clone()).await;
        let tok = token(&st, yagra_common::Role::Admin);
        let a = crate::pgtest::node(&pool, "left", 4, None).await;
        let b = crate::pgtest::node(&pool, "right", 5, None).await;
        let (status, body) = send(
            &st,
            "POST",
            "/api/v1/topology/link-overrides",
            &tok,
            Some(serde_json::json!({ "a_node": a, "b_node": b, "action": "pin" })),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::OK, "{body}");
        assert_eq!(crate::pgtest::rows(&pool, "link_overrides").await, 1);
    }

    // ── One level of the network map (ADR-191), through the whole router ────────────────────

    /// The whole network, a folder level, and a scoped caller, over a real database.
    ///
    /// ```text
    /// (whole)            lone (ungrouped)
    /// ├─ east            ├ sw-a ─┐ (LLDP)
    /// │  └─ site-a       │       └ sw-b
    /// │     └─ floor-1   └ sw-d   (LLDP to sw-b)
    /// └─ west            └ sw-c   (LLDP to sw-a)
    /// ```
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_map_level_bundles_links_and_honours_the_scope(pool: sqlx::PgPool) {
        use crate::api::tests_support::{live_state, scoped_token, send, token};
        use yagra_common::{DerivedLink, NodeId};
        let groups = crate::groups::GroupRepo::new(pool.clone());
        let east = crate::pgtest::group(&pool, "east").await;
        let west = crate::pgtest::group(&pool, "west").await;
        let site = groups
            .create("site-a", crate::groups::GroupType::Site, Some(east), None)
            .await
            .expect("site-a");
        let lone = crate::pgtest::node(&pool, "lone", 1, None).await;
        let a = crate::pgtest::node(&pool, "sw-a", 2, Some(site)).await;
        let b = crate::pgtest::node(&pool, "sw-b", 3, Some(site)).await;
        let c = crate::pgtest::node(&pool, "sw-c", 4, Some(west)).await;
        let floor = groups
            .create(
                "floor-1",
                crate::groups::GroupType::Generic,
                Some(site),
                None,
            )
            .await
            .expect("floor-1");
        let d = crate::pgtest::node(&pool, "sw-d", 5, Some(floor)).await;
        crate::topology_links::TopoLinkRepo::new(pool.clone())
            .upsert_batch(&[
                DerivedLink::new(NodeId(a), NodeId(b), LinkSource::Lldp),
                DerivedLink::new(NodeId(a), NodeId(c), LinkSource::Lldp),
                DerivedLink::new(NodeId(lone), NodeId(c), LinkSource::Cdp),
                DerivedLink::new(NodeId(b), NodeId(d), LinkSource::Lldp),
            ])
            .await
            .expect("links");
        let st = live_state(pool.clone()).await;
        let admin = token(&st, yagra_common::Role::Viewer);

        // The whole network: two boxes, the ungrouped node, east↔west and lone↔west.
        let (status, whole) = send(&st, "GET", "/api/v1/topology/map", &admin, None).await;
        assert_eq!(status, StatusCode::OK, "{whole}");
        assert!(whole["group"].is_null());
        assert_eq!(whole["folders"].as_array().map(Vec::len), Some(2));
        assert_eq!(whole["edges"].as_array().map(Vec::len), Some(2));
        assert_eq!(whole["nodes"][0]["name"], "lone");

        // site-a is a site, so it is flat: its own two nodes and floor-1's sw-d, no boxes, and one
        // stub towards west (on the whole level).
        let (status, level) = send(
            &st,
            "GET",
            &format!("/api/v1/topology/map?group={site}"),
            &admin,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{level}");
        assert_eq!(level["group"]["name"], "site-a");
        assert_eq!(level["breadcrumbs"][0]["name"], "east");
        assert_eq!(level["flattened"], true);
        assert_eq!(level["folders"].as_array().map(Vec::len), Some(0));
        let nodes = level["nodes"].as_array().expect("nodes");
        assert_eq!(nodes.len(), 3);
        let sw_d = nodes
            .iter()
            .find(|n| n["name"] == "sw-d")
            .expect("sw-d is drawn on site-a");
        assert_eq!(sw_d["folder_path"][0]["name"], "floor-1");
        assert_eq!(
            level["subfolder_count"], 1,
            "floor-1, though it is not a box"
        );
        assert!(nodes
            .iter()
            .find(|n| n["name"] == "sw-a")
            .is_some_and(|n| n["folder_path"].as_array().is_some_and(Vec::is_empty)));
        assert_eq!(level["stubs"][0]["name"], "west");
        assert!(level["stubs"][0]["level_group"].is_null());

        // A caller scoped to east sees east as a root, and no line into west.
        let scoped = scoped_token(&st, &[east]);
        let (status, mine) = send(&st, "GET", "/api/v1/topology/map", &scoped, None).await;
        assert_eq!(status, StatusCode::OK, "{mine}");
        assert_eq!(mine["folders"].as_array().map(Vec::len), Some(1));
        assert_eq!(mine["folders"][0]["name"], "east");
        assert_eq!(mine["edges"].as_array().map(Vec::len), Some(0));
        let (status, _) = send(
            &st,
            "GET",
            &format!("/api/v1/topology/map?group={west}"),
            &scoped,
            None,
        )
        .await;
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "a folder outside the scope is not found"
        );
    }
}
