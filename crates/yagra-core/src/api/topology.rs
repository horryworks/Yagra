// SPDX-License-Identifier: AGPL-3.0-only
//! The connectivity graph: its links, one folder level of the network map, and the operator's
//! decisions about links (ADR-043, ADR-191).
//!
//! There is no dependency graph any more (ADR-236): nothing names a parent in advance, and an
//! alert's `root_cause` is decided when an outage happens. The map reads that attribution from the
//! open alerts ([`root_cause_map`]).

use super::extract::{Actor, RequireManageConfig, RequireView, Scoped};
use super::{ApiError, ApiResult, ApiState};
use crate::api::extract::Admin;
use crate::runtime::node_display::state_or_fallback;
use axum::{
    extract::{Path, Query},
    routing::{delete, get},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;
use yagra_common::{
    LinkDirection, LinkOverrideAction, LinkSource, NodeId, NodeState, TopologyLinkSummary,
};

/// This domain's slice of the OpenAPI document (ADR-035), merged by [`super::openapi::document`].
#[derive(utoipa::OpenApi)]
#[openapi(paths(
    get_topology_links,
    get_topology_map,
    get_link_overrides,
    create_link_override,
    delete_link_override,
))]
pub(super) struct Doc;

/// The topology routes, merged into `/api/v1` by [`super::router`].
pub(crate) fn routes() -> Router<ApiState> {
    Router::new()
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
}

/// node → the node its alert is rolled up into (from the open alerts, ADR-236).
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
    /// The endpoint an operator declared upstream before ADR-236, when one was. Read by nothing
    /// since then: no direction is worked out or recorded any more.
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
    let limit = super::util::page_limit(q.limit, 2000, 2000);
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

/// What a node does in the network, which decides the row the map draws it in (ADR-191 Inc.6):
/// the way out of the site on top, then the switches that route, the switches that do not, and
/// access points and everything else at the bottom.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MapRole {
    /// A router or firewall (a Meraki MX among them): the way out of the site.
    Edge,
    /// A switch that routes between the site's networks.
    L3Switch,
    /// A switch that does not route.
    L2Switch,
    /// A Wi-Fi access point.
    AccessPoint,
    /// Anything else: a wireless controller, a server, a printer, a URL or DNS monitor.
    Other,
}

/// Why a node was given its [`MapRole`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MapRoleReason {
    /// It was imported from its wireless controller as an access point.
    WirelessAp,
    /// The Meraki product type says what it is.
    MerakiProduct,
    /// Its device profile's category says what it is.
    ProfileCategory,
    /// It routes, and its default route leaves its site: the next hop is an address no node of the
    /// site claims and not on a subnet another node of the site holds, or the route points out of an
    /// interface with no gateway. It is the site's way out (ADR-191 Inc.10).
    DefaultRoute,
    /// It holds an OSPF, BGP or routing-table adjacency.
    RoutingAdjacency,
    /// It has an address in two or more subnets, so it routes between them.
    Subnets,
    /// Nothing points anywhere else.
    Default,
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
    /// The node whose outage this alert is rolled up into: the border of the outage it sits behind (ADR-236), or its own node for a down node's other alerts (ADR-087).
    pub root_cause: Option<Uuid>,
    /// On a `flattened` level, the folders between the level and the node, outermost first, ending
    /// with the one the node is filed in; empty when it sits directly in the level's own folder
    /// (always empty on an ordinary level).
    pub folder_path: Vec<MapBreadcrumb>,
    /// The node's role is `access_point` — one imported from its wireless controller, a Meraki
    /// MR, or a device whose profile is classified Wireless AP. The map draws it as an
    /// access-point symbol under the device it hangs off.
    pub access_point: bool,
    /// What the node does in the network, which decides its row on the map (ADR-191 Inc.6).
    pub role: MapRole,
    /// Why it was given that role.
    pub role_reason: MapRoleReason,
    /// How many subnets the node has an address in, from its last address walk; `null` when no
    /// walk has been recorded.
    pub subnet_count: Option<u32>,
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
    /// What that run observed but did not turn into a link — the same counts `/topology/links`
    /// carries, for the whole network rather than this level. All zero before the first run.
    pub summary: TopologyLinkSummary,
}

/// Query parameters for one map level.
#[derive(Debug, Clone, Deserialize, utoipa::IntoParams)]
pub(crate) struct MapLevelQuery {
    /// The folder to draw. Omit for the whole network.
    pub group: Option<Uuid>,
}

/// What decides each drawn node's role on the map (ADR-191 Inc.6). A read that fails leaves its
/// facts unknown rather than failing the map: the drawing is useful without the rows.
async fn role_facts(
    admin: &super::AdminState,
    drawn: &[Uuid],
    links: &[crate::topology::topology_links::StoredLink],
) -> HashMap<Uuid, super::topology_level::RoleFacts> {
    let (kinds, categories, l3, hops) = tokio::join!(
        super::nodes::node_kinds_with_products(admin, drawn),
        async {
            admin
                .repo
                .profile_categories(drawn)
                .await
                .unwrap_or_default()
        },
        async { admin.l3.current_for(drawn).await.unwrap_or_default() },
        async {
            admin
                .routing
                .default_next_hops_for(drawn)
                .await
                .unwrap_or_default()
        },
    );
    let networks: HashMap<Uuid, std::collections::BTreeSet<yagra_common::SubnetKey>> = l3
        .into_iter()
        .map(|(id, snap)| (id, snap.subnets()))
        .collect();
    let subnets: HashMap<Uuid, usize> = networks.iter().map(|(id, n)| (*id, n.len())).collect();
    let mut routing: HashSet<Uuid> = HashSet::new();
    for l in links {
        if l.sources
            .iter()
            .any(|s| super::topology_level::is_routing_evidence(*s))
        {
            routing.extend(l.a_node.iter().chain(l.b_node.iter()).map(|n| n.as_uuid()));
        }
    }
    let exits = site_exits_for(admin, drawn, &hops, &networks).await;
    drawn
        .iter()
        .filter_map(|id| {
            let kind = *kinds.kinds.get(id)?;
            Some((
                *id,
                super::topology_level::RoleFacts {
                    kind,
                    meraki_product: kinds.meraki_product_types.get(id).cloned(),
                    category: categories.get(id).copied(),
                    subnet_count: subnets.get(id).copied(),
                    routing_adjacency: routing.contains(id),
                    exits_site: exits.get(id).copied(),
                },
            ))
        })
        .collect()
}

/// Which of the drawn nodes route out of their site (ADR-191 Inc.10), from each one's stored default
/// next hops (`hops`, read alongside the other role facts) and the subnets the drawn nodes hold
/// (`networks`). Only the hop addresses are looked up, so a level whose nodes were never asked costs
/// nothing more. Every read here is unscoped: whether a hop is inside the site is a fact about the
/// network, not about what the caller may see, and only the yes/no leaves this function. A read that
/// fails leaves every node unknown, which keeps the role it would have had.
async fn site_exits_for(
    admin: &super::AdminState,
    drawn: &[Uuid],
    hops: &HashMap<Uuid, Vec<std::net::IpAddr>>,
    networks: &HashMap<Uuid, std::collections::BTreeSet<yagra_common::SubnetKey>>,
) -> HashMap<Uuid, bool> {
    if hops.is_empty() {
        return HashMap::new();
    }
    let mut addresses: Vec<std::net::IpAddr> = hops.values().flatten().copied().collect();
    addresses.sort_unstable();
    addresses.dedup();
    let (claims, groups) = tokio::join!(
        admin.repo.address_claims(&addresses, None),
        admin.groups.list()
    );
    let (Ok(claims), Ok(groups)) = (claims, groups) else {
        return HashMap::new();
    };
    let mut owners: HashMap<std::net::IpAddr, Vec<Uuid>> = HashMap::new();
    for c in &claims {
        owners.entry(c.address).or_default().push(c.id);
    }
    let mut members: Vec<Uuid> = drawn.to_vec();
    members.extend(claims.iter().map(|c| c.id));
    members.sort_unstable();
    members.dedup();
    let Ok(filed) = admin.repo.group_ids_of(&members).await else {
        return HashMap::new();
    };
    let parent: HashMap<Uuid, Option<Uuid>> = groups.iter().map(|g| (g.id, g.parent_id)).collect();
    let sites: HashSet<Uuid> = groups
        .iter()
        .filter(|g| {
            yagra_base::groups::GroupType::from_key(&g.group_type)
                == Some(yagra_base::groups::GroupType::Site)
        })
        .map(|g| g.id)
        .collect();
    let is_site = |g: Uuid| sites.contains(&g);
    let site = |node: Uuid| {
        super::topology_level::site_of_group(filed.get(&node).copied().flatten(), &parent, &is_site)
    };
    super::topology_level::site_exits(hops, &owners, networks, &site)
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
    let folders: Vec<super::topology_level::FolderRow> =
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
        crate::runtime::node_display::fresh_fleet_ids(st.store.as_ref()).await
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

    let mut level = super::topology_level::compute(&super::topology_level::LevelInput {
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
        super::nodes::resolve_node_names(st, scope, super::topology_level::names_needed(&level))
            .await;
    super::topology_level::apply_names(&mut level, &names);
    let drawn: Vec<Uuid> = level.nodes.iter().map(|n| n.id).collect();
    let roles = role_facts(admin, &drawn, &links).await;
    super::topology_level::apply_roles(&mut level, &roles);
    // The drawing is useful without the run's record, so a failed read leaves both unset.
    if let Some(run) = admin.topology_links.last_run().await.unwrap_or(None) {
        level.derived_at = Some(run.derived_at.to_rfc3339());
        level.summary = run.summary;
    }
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
    /// `pin` or `hide`. `direction` is refused (ADR-236).
    pub action: LinkOverrideAction,
    /// Refused when present (ADR-236): a link's direction is no longer recorded.
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
        (status = 400, description = "The two endpoints are the same node, or the action is `direction`, which is no longer recorded", body = super::error::ErrorBody),
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
    // ADR-236 decision 7: which end of a link is upstream no longer means anything — an outage is
    // grouped by who is alive around it, not by a direction decided in advance — so a new
    // `direction` decision is refused rather than stored and silently ignored. One already stored
    // stays readable until the column is dropped.
    if req.action == LinkOverrideAction::Direction || req.direction.is_some() {
        return Err(ApiError::bad_request(
            "direction_retired",
            "a link's direction is no longer recorded; pin or hide it instead",
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::tests_support::{private_state, public_state, status_with};
    use axum::http::StatusCode;

    async fn status_of_path(st: ApiState, path: &str) -> StatusCode {
        status_with(st, "GET", path, None, None).await
    }

    #[tokio::test]
    async fn the_links_endpoint_is_gated_before_its_subsystem_is_consulted() {
        // An anonymous caller learns only that it is unauthenticated, never whether this
        // deployment has a write side.
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

    // ── An accepted write (ADR-115) ──────────────────────────────────────────────────

    /// An operator's decision about a link is stored — the one topology table that is a source of
    /// truth rather than a recomputable cache.
    ///
    /// `200`, not `201`: recording a decision is idempotent, so the endpoint documents an OK
    /// carrying the id rather than a creation.
    #[sqlx::test(migrator = "yagra_base::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn pinning_a_link_stores_the_override(pool: sqlx::PgPool) {
        use crate::api::tests_support::{live_state, send, token};
        let st = live_state(pool.clone()).await;
        let tok = token(&st, yagra_common::Role::Admin);
        let a = yagra_base::pgtest::node(&pool, "left", 4, None).await;
        let b = yagra_base::pgtest::node(&pool, "right", 5, None).await;
        let (status, body) = send(
            &st,
            "POST",
            "/api/v1/topology/link-overrides",
            &tok,
            Some(serde_json::json!({ "a_node": a, "b_node": b, "action": "pin" })),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::OK, "{body}");
        assert_eq!(yagra_base::pgtest::rows(&pool, "link_overrides").await, 1);
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
    #[sqlx::test(migrator = "yagra_base::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_map_level_bundles_links_and_honours_the_scope(pool: sqlx::PgPool) {
        use crate::api::tests_support::{live_state, scoped_token, send, token};
        use yagra_common::{DerivedLink, NodeId};
        let groups = yagra_base::groups::GroupRepo::new(pool.clone());
        let east = yagra_base::pgtest::group(&pool, "east").await;
        let west = yagra_base::pgtest::group(&pool, "west").await;
        let site = groups
            .create(
                "site-a",
                yagra_base::groups::GroupType::Site,
                Some(east),
                None,
            )
            .await
            .expect("site-a");
        let lone = yagra_base::pgtest::node(&pool, "lone", 1, None).await;
        let a = yagra_base::pgtest::node(&pool, "sw-a", 2, Some(site)).await;
        let b = yagra_base::pgtest::node(&pool, "sw-b", 3, Some(site)).await;
        let c = yagra_base::pgtest::node(&pool, "sw-c", 4, Some(west)).await;
        let floor = groups
            .create(
                "floor-1",
                yagra_base::groups::GroupType::Generic,
                Some(site),
                None,
            )
            .await
            .expect("floor-1");
        let d = yagra_base::pgtest::node(&pool, "sw-d", 5, Some(floor)).await;
        crate::topology::topology_links::TopoLinkRepo::new(pool.clone())
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

        // What the derivation could not turn into a link rides on every level, so the map can say
        // it is incomplete (ADR-191 decision 15): zero before any run, the run's counts after one.
        assert!(whole["derived_at"].is_null());
        assert_eq!(whole["summary"]["unmatched_lldp_rows"], 0);
        crate::topology::topology_links::TopoLinkRepo::new(pool.clone())
            .record_run(
                &TopologyLinkSummary {
                    unmatched_lldp_rows: 2,
                    ambiguous_mgmt_addrs: 1,
                    ..TopologyLinkSummary::default()
                },
                4,
                &std::collections::BTreeSet::new(),
            )
            .await
            .expect("record the run");
        let (status, after) = send(
            &st,
            "GET",
            &format!("/api/v1/topology/map?group={site}"),
            &admin,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{after}");
        assert!(after["derived_at"].is_string());
        assert_eq!(after["summary"]["unmatched_lldp_rows"], 2);
        assert_eq!(after["summary"]["ambiguous_mgmt_addrs"], 1);
    }
}
