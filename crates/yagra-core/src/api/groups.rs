// SPDX-License-Identifier: AGPL-3.0-only
//! Node groups — the inventory folder tree.
//!
//! Reads are `View` (the tree is how an operator navigates the fleet); every write is
//! `ManageConfig`.
//!
//! **Two guards here are structural, not cosmetic.** A group's parent can be edited by three
//! different endpoints, and every one of them re-checks [`would_create_cycle`] against the current
//! edges: a group nested inside its own subtree makes the tree walk non-terminating, which takes
//! out the inventory page rather than showing a bad row. The pool field carries the same
//! three-state contract as a node's — **absent** leaves it unchanged, `""` clears it to inherited,
//! a value moves the folder's nodes — so it goes through the shared
//! [`validate_pool_update`]/[`validate_pool_create`] rather than being re-derived here.

use super::error::{ApiError, ApiResult};
use super::extract::{Admin, RequireManageConfig, RequireView};
use super::nodes::{validate_pool_create, validate_pool_update, PoolAssignment};
use super::ApiState;
use crate::groups::{placement_order, would_create_cycle, GroupType};
use axum::{
    extract::Path,
    http::StatusCode,
    routing::{get, put},
    Json, Router,
};
use serde::Deserialize;
use uuid::Uuid;

/// This domain's slice of the OpenAPI document (ADR-035), merged by [`super::openapi::document`].
#[derive(utoipa::OpenApi)]
#[openapi(paths(
    list_node_groups,
    create_node_group,
    update_node_group,
    delete_node_group,
    place_group,
    set_node_group_geo,
    set_node_group_pool
))]
pub(super) struct Doc;

/// The node-group routes, merged into `/api/v1` by [`super::router`].
pub(super) fn routes() -> Router<ApiState> {
    Router::new()
        .route(
            "/api/v1/node-groups",
            get(list_node_groups).post(create_node_group),
        )
        .route(
            "/api/v1/node-groups/:id",
            put(update_node_group).delete(delete_node_group),
        )
        .route("/api/v1/node-groups/:id/placement", put(place_group))
        .route("/api/v1/node-groups/:id/geo", put(set_node_group_geo))
        .route("/api/v1/node-groups/:id/pool", put(set_node_group_pool))
}

#[utoipa::path(
    get, path = "/api/v1/node-groups", tag = "groups",
    responses(
        (status = 200, description = "Every folder group in the inventory tree", body = Vec<crate::groups::GroupSummary>),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks the View permission", body = super::error::ErrorBody),
        (status = 503, description = "This core has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn list_node_groups(
    _guard: RequireView,
    admin: Admin,
) -> ApiResult<Json<Vec<crate::groups::GroupSummary>>> {
    let list = admin.groups.list().await.map_err(|e| {
        ApiError::from_internal(e.as_ref(), "list node groups", "failed to list node groups")
    })?;
    Ok(Json(list))
}

/// Create/update body for a group. `group_type` is a validated [`GroupType`] key.
#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct GroupBody {
    name: String,
    group_type: String,
    #[serde(default)]
    parent_id: Option<Uuid>,
    /// Poll-pool this folder assigns to its nodes (ADR-009/020). Same three-state contract as a
    /// node's: **absent** leaves it unchanged, `""` clears it (inherit from the nearest ancestor,
    /// else the default pool), otherwise it moves the folder's nodes to that pool.
    #[serde(default)]
    pool: Option<String>,
}

/// Validate the request: non-empty name plus a known group type.
fn parse_group_body(body: &GroupBody) -> Result<GroupType, ApiError> {
    if body.name.trim().is_empty() {
        return Err(ApiError::bad_request(
            "invalid_group",
            "group name must not be empty",
        ));
    }
    GroupType::from_key(body.group_type.trim()).ok_or_else(|| {
        ApiError::bad_request(
            "invalid_group",
            format!("unknown group type {:?}", body.group_type),
        )
    })
}

/// Refuse a re-parent that would nest a group inside its own subtree.
///
/// Checked against the *current* edges on every path that can set a parent, because the failure is
/// not a bad row: a cycle makes the tree walk non-terminating, so the inventory page stops
/// rendering entirely.
async fn reject_cycle(admin: &Admin, id: Uuid, parent_id: Option<Uuid>) -> Result<(), ApiError> {
    let edges = admin.groups.edges().await.map_err(|e| {
        ApiError::from_internal(e.as_ref(), "load group edges", "failed to update group")
    })?;
    if would_create_cycle(&edges, id, parent_id) {
        return Err(ApiError::bad_request(
            "invalid_group",
            "that move would nest the group inside itself",
        ));
    }
    Ok(())
}

#[utoipa::path(
    post, path = "/api/v1/node-groups", tag = "groups",
    request_body = GroupBody,
    responses(
        (status = 204, description = "Group created"),
        (status = 400, description = "Empty name, unknown group type, or an invalid pool name", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role below Admin", body = super::error::ErrorBody),
        (status = 503, description = "This core has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn create_node_group(
    _guard: RequireManageConfig,
    admin: Admin,
    Json(body): Json<GroupBody>,
) -> ApiResult<StatusCode> {
    let group_type = parse_group_body(&body)?;
    let pool = validate_pool_create(body.pool)?;
    admin
        .groups
        .create(
            body.name.trim(),
            group_type,
            body.parent_id,
            pool.as_deref(),
        )
        .await
        .map_err(|e| {
            ApiError::from_internal(e.as_ref(), "create node group", "failed to create group")
        })?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    put, path = "/api/v1/node-groups/{id}", tag = "groups",
    params(("id" = Uuid, Path, description = "Group id")),
    request_body = GroupBody,
    responses(
        (status = 204, description = "Group updated"),
        (status = 400, description = "Empty name, unknown group type, an invalid pool name, or a move that would nest the group inside its own subtree", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role below Admin", body = super::error::ErrorBody),
        (status = 404, description = "No such group", body = super::error::ErrorBody),
        (status = 503, description = "This core has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn update_node_group(
    _guard: RequireManageConfig,
    admin: Admin,
    Path(id): Path<Uuid>,
    Json(body): Json<GroupBody>,
) -> ApiResult<StatusCode> {
    let group_type = parse_group_body(&body)?;
    let pool_update = validate_pool_update(body.pool)?;
    if body.parent_id.is_some() {
        reject_cycle(&admin, id, body.parent_id).await?;
    }
    let updated = admin
        .groups
        .update(
            id,
            body.name.trim(),
            group_type,
            body.parent_id,
            pool_update.as_ref().map(|inner| inner.as_deref()),
        )
        .await
        .map_err(|e| {
            ApiError::from_internal(e.as_ref(), "update node group", "failed to update group")
        })?;
    if updated {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found(
            "group_not_found",
            format!("no group {id}"),
        ))
    }
}

/// Drag-reorder a group: re-parent it under `parent_id` (`null` ⇒ top level) and position it
/// relative to a sibling. `before`/`after` name the sibling; both omitted ⇒ append.
#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct GroupPlacement {
    #[serde(default)]
    parent_id: Option<Uuid>,
    #[serde(default)]
    before: Option<Uuid>,
    #[serde(default)]
    after: Option<Uuid>,
}

#[utoipa::path(
    put, path = "/api/v1/node-groups/{id}/placement", tag = "groups",
    params(("id" = Uuid, Path, description = "Group id")),
    request_body = GroupPlacement,
    responses(
        (status = 204, description = "Group re-parented and re-ordered"),
        (status = 400, description = "Both before and after given, or a move that would nest the group inside its own subtree", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role below Admin", body = super::error::ErrorBody),
        (status = 404, description = "No such group", body = super::error::ErrorBody),
        (status = 503, description = "This core has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn place_group(
    _guard: RequireManageConfig,
    admin: Admin,
    Path(id): Path<Uuid>,
    Json(body): Json<GroupPlacement>,
) -> ApiResult<StatusCode> {
    if body.before.is_some() && body.after.is_some() {
        return Err(ApiError::bad_request(
            "invalid_placement",
            "specify at most one of before/after",
        ));
    }
    reject_cycle(&admin, id, body.parent_id).await?;
    let siblings = admin
        .groups
        .ordered_siblings(body.parent_id)
        .await
        .map_err(|e| {
            ApiError::from_internal(e.as_ref(), "load group siblings", "failed to move group")
        })?
        .into_iter()
        // The group being moved is not its own neighbour; leaving it in would let it be placed
        // "before itself".
        .filter(|(sid, _)| *sid != id)
        .collect::<Vec<_>>();
    let order = placement_order(&siblings, body.before, body.after);
    let placed = admin
        .groups
        .place(id, body.parent_id, order)
        .await
        .map_err(|e| ApiError::from_internal(e.as_ref(), "place group", "failed to move group"))?;
    if placed {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found(
            "group_not_found",
            format!("no group {id}"),
        ))
    }
}

/// Set or clear a group's map pin. `null` for both clears it.
#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct GroupGeo {
    latitude: Option<f64>,
    longitude: Option<f64>,
}

#[utoipa::path(
    put, path = "/api/v1/node-groups/{id}/geo", tag = "groups",
    params(("id" = Uuid, Path, description = "Group id")),
    request_body = GroupGeo,
    responses(
        (status = 204, description = "Map pin set or cleared"),
        (status = 400, description = "Only one of latitude/longitude given, or a coordinate out of range", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role below Admin", body = super::error::ErrorBody),
        (status = 404, description = "No such group", body = super::error::ErrorBody),
        (status = 503, description = "This core has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn set_node_group_geo(
    _guard: RequireManageConfig,
    admin: Admin,
    Path(id): Path<Uuid>,
    Json(body): Json<GroupGeo>,
) -> ApiResult<StatusCode> {
    // Both or neither, and in range. Half a coordinate pair is not a location, and an out-of-range
    // one puts the pin somewhere the map cannot show.
    match (body.latitude, body.longitude) {
        (Some(lat), Some(lon)) => {
            if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
                return Err(ApiError::bad_request(
                    "invalid_coordinates",
                    "latitude must be -90..90 and longitude -180..180",
                ));
            }
        }
        (None, None) => {}
        _ => {
            return Err(ApiError::bad_request(
                "invalid_coordinates",
                "provide both latitude and longitude, or neither (to clear)",
            ))
        }
    }
    let updated = admin
        .groups
        .set_geo(id, body.latitude, body.longitude)
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "set group geo",
                "failed to set group coordinates",
            )
        })?;
    if updated {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found(
            "group_not_found",
            format!("no group {id}"),
        ))
    }
}

/// Set just the folder's pool. Every node beneath it that has no pool of its own follows on the
/// next sweep (see `poolres`).
#[utoipa::path(
    put, path = "/api/v1/node-groups/{id}/pool", tag = "groups",
    params(("id" = Uuid, Path, description = "Group id")),
    request_body = PoolAssignment,
    responses(
        (status = 204, description = "Folder pool set or cleared"),
        (status = 400, description = "Pool name too long or containing characters outside letters, digits, '_' and '-'", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role below Admin", body = super::error::ErrorBody),
        (status = 404, description = "No such group", body = super::error::ErrorBody),
        (status = 503, description = "This core has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn set_node_group_pool(
    _guard: RequireManageConfig,
    admin: Admin,
    Path(id): Path<Uuid>,
    Json(body): Json<PoolAssignment>,
) -> ApiResult<StatusCode> {
    let pool = validate_pool_create(body.pool)?;
    let updated = admin
        .groups
        .set_pool(id, pool.as_deref())
        .await
        .map_err(|e| {
            ApiError::from_internal(e.as_ref(), "set group pool", "failed to set group pool")
        })?;
    if updated {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found(
            "group_not_found",
            format!("no group {id}"),
        ))
    }
}

#[utoipa::path(
    delete, path = "/api/v1/node-groups/{id}", tag = "groups",
    params(("id" = Uuid, Path, description = "Group id")),
    responses(
        (status = 204, description = "Group deleted"),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role below Admin", body = super::error::ErrorBody),
        (status = 404, description = "No such group", body = super::error::ErrorBody),
        (status = 503, description = "This core has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn delete_node_group(
    _guard: RequireManageConfig,
    admin: Admin,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    match admin.groups.delete(id).await {
        Ok(true) => Ok(StatusCode::NO_CONTENT),
        Ok(false) => Err(ApiError::not_found(
            "group_not_found",
            format!("no group {id}"),
        )),
        Err(e) => Err(ApiError::from_internal(
            e.as_ref(),
            "delete node group",
            "failed to delete group",
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
    use yagra_common::{Principal, Role, Scope};

    const ID: &str = "00000000-0000-0000-0000-000000000001";

    fn write_routes() -> Vec<(&'static str, String)> {
        vec![
            ("POST", "/api/v1/node-groups".to_owned()),
            ("PUT", format!("/api/v1/node-groups/{ID}")),
            ("DELETE", format!("/api/v1/node-groups/{ID}")),
            ("PUT", format!("/api/v1/node-groups/{ID}/placement")),
            ("PUT", format!("/api/v1/node-groups/{ID}/geo")),
            ("PUT", format!("/api/v1/node-groups/{ID}/pool")),
        ]
    }

    async fn status_of(st: ApiState, method: &str, path: &str, token: Option<&str>) -> StatusCode {
        let mut b = Request::builder()
            .method(method)
            .uri(path)
            .header("content-type", "application/json");
        if let Some(t) = token {
            b = b.header(AUTHORIZATION, format!("Bearer {t}"));
        }
        router(st)
            .oneshot(b.body(Body::from("{}")).unwrap())
            .await
            .unwrap()
            .status()
    }

    #[tokio::test]
    async fn the_tree_reads_openly_but_only_admins_reshape_it() {
        // The folder tree is how an operator navigates the fleet, so reading it is `View` and open
        // on a public dashboard (503 = past the guard, into skeleton mode).
        assert_eq!(
            status_of(public_state(), "GET", "/api/v1/node-groups", None).await,
            StatusCode::SERVICE_UNAVAILABLE,
        );
        for (method, path) in write_routes() {
            assert_eq!(
                status_of(private_state(), method, &path, None).await,
                StatusCode::UNAUTHORIZED,
                "anon {method} {path}"
            );
            assert_eq!(
                status_of(public_state(), method, &path, None).await,
                StatusCode::UNAUTHORIZED,
                "public {method} {path}"
            );
        }
        let st = private_state();
        for role in [Role::Viewer, Role::Operator] {
            let token = st
                .sessions
                .issue(Uuid::new_v4(), Principal::new(role, Scope::All), "u");
            for (method, path) in write_routes() {
                assert_eq!(
                    status_of(st.clone(), method, &path, Some(&token)).await,
                    StatusCode::FORBIDDEN,
                    "{role:?} {method} {path}"
                );
            }
        }
    }

    #[test]
    fn a_group_needs_a_name_and_a_known_type() {
        let body = |name: &str, kind: &str| GroupBody {
            name: name.to_owned(),
            group_type: kind.to_owned(),
            parent_id: None,
            pool: None,
        };
        assert!(parse_group_body(&body("Tokyo", "site")).is_ok());
        // Whitespace is not a name — it renders as an unclickable blank row in the tree.
        assert_eq!(
            parse_group_body(&body("   ", "site")).unwrap_err().code(),
            "invalid_group"
        );
        assert_eq!(
            parse_group_body(&body("Tokyo", "not-a-type"))
                .unwrap_err()
                .code(),
            "invalid_group"
        );
    }
}
