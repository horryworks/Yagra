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

use super::extract::{RequireView, Scoped};
use super::nodes::{fresh_fallback_ids, NodePageQuery};
use super::{ApiError, ApiResult, ApiState};
use crate::api::extract::Admin;
use axum::{extract::Query, routing::get, Json, Router};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use uuid::Uuid;
use yagra_common::{LinkSource, NodeId, NodeState, TopologyLinkSummary};

/// This domain's slice of the OpenAPI document (ADR-035), merged by [`super::openapi::document`].
#[derive(utoipa::OpenApi)]
#[openapi(paths(get_topology, get_topology_links))]
pub(super) struct Doc;

/// The topology routes, merged into `/api/v1` by [`super::router`].
pub(crate) fn routes() -> Router<ApiState> {
    Router::new()
        .route("/api/v1/topology", get(get_topology))
        .route("/api/v1/topology/links", get(get_topology_links))
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
    // node → upstream root cause (from active, suppressed alerts).
    let mut root_causes: HashMap<NodeId, Uuid> = HashMap::new();
    for a in st.alerts.active_alerts() {
        if let Some(cause) = a.root_cause {
            root_causes.entry(a.node).or_insert_with(|| cause.as_uuid());
        }
    }
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
            let state = match states.get(&nid) {
                Some(s) => *s,
                None if fresh_fallback.contains(&r.id) => NodeState::Ok,
                None => NodeState::Unknown,
            };
            TopologyNode {
                id: r.id,
                name: r.name,
                parent_id: r.parent_id,
                state,
                root_cause: root_causes.get(&nid).copied(),
            }
        })
        .collect();
    Ok(TopologyPage { nodes, next_cursor })
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
}
