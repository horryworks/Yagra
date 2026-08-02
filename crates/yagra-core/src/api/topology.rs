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
use yagra_common::{NodeId, NodeState};

/// This domain's slice of the OpenAPI document (ADR-035), merged by [`super::openapi::document`].
#[derive(utoipa::OpenApi)]
#[openapi(paths(get_topology))]
pub(super) struct Doc;

/// The topology routes, merged into `/api/v1` by [`super::router`].
pub(crate) fn routes() -> Router<ApiState> {
    Router::new().route("/api/v1/topology", get(get_topology))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::router;
    use crate::api::tests_support::{private_state, public_state};
    use axum::body::{to_bytes, Body};
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    async fn status_of(st: ApiState) -> StatusCode {
        router(st)
            .oneshot(
                Request::builder()
                    .uri("/api/v1/topology")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
            .status()
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
