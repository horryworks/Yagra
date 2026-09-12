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
use super::extract::{Admin, RequireManageConfig, RequireView, Scoped};
use super::nodes::{validate_pool_create, validate_pool_update, PoolAssignment};
use super::util::CreatedId;
use super::ApiState;
use crate::groups::{placement_order, would_create_cycle, GroupType, SortDirection};
use axum::{
    extract::Path,
    http::StatusCode,
    routing::{get, post, put},
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
    sort_group_children,
    set_node_group_pool,
    set_node_group_geo,
    set_node_group_prefixes,
    set_node_group_tags
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
        .route("/api/v1/node-groups/:id/sort", post(sort_group_children))
        .route("/api/v1/node-groups/:id/pool", put(set_node_group_pool))
        .route("/api/v1/node-groups/:id/geo", put(set_node_group_geo))
        .route(
            "/api/v1/node-groups/:id/prefixes",
            put(set_node_group_prefixes),
        )
        .route("/api/v1/node-groups/:id/tags", put(set_node_group_tags))
}

/// How many hand-made ranges one folder may carry.
///
/// Bounded because every limit at this edge is (`api-conventions.md`), and because the list is
/// rendered as a row per range in a dialog. A site with more than this many distinct subnets is
/// one NetBox should be the source of truth for.
const MAX_GROUP_PREFIXES: usize = 64;

/// How long a range's description may be. The column is unbounded `TEXT`; the dialog is not.
const MAX_PREFIX_DESCRIPTION: usize = 200;

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
    Scoped(scope): Scoped,
    admin: Admin,
) -> ApiResult<Json<Vec<crate::groups::GroupSummary>>> {
    Ok(Json(visible_groups(&admin, &scope).await?))
}

/// The folder tree the caller may see: their subtree **and the ancestors above it**.
///
/// Dropping the ancestors is the tempting simplification and it breaks the tree: it is built from
/// `parent_id`, so every visible root would point at a parent that is not in the response and
/// render as an orphan. The ancestors are names only — `allows_group_row` admits them, while
/// membership questions go through `allows_group`, which does not.
///
/// Extracted so that choice is made once. ADR-042's `list_node_groups` tool returns `parent_id`
/// too, so a second filter written with `allows_group` would be subtly wrong in exactly this way.
///
/// 🚨 **`prefixes` uses the other predicate, and that asymmetry is the point** (ADR-100 decision
/// 10). A breadcrumb ancestor is admitted here as a *name*, so the tree has a spine. Its IP
/// prefixes are not a name — they are the subnet layout of a site whose membership this caller was
/// refused — so they are cleared on exactly the rows `allows_group` rejects. That is the same
/// split `scope::require_visible_group` makes, for the same reason: a folder may be named without
/// being reachable.
pub(crate) async fn visible_groups(
    admin: &super::AdminState,
    scope: &super::scope::NodeScope,
) -> ApiResult<Vec<crate::groups::GroupSummary>> {
    let list = admin.groups.list().await.map_err(|e| {
        ApiError::from_internal(e.as_ref(), "list node groups", "failed to list node groups")
    })?;
    Ok(list
        .into_iter()
        .filter(|g| scope.allows_group_row(g.id))
        .map(|mut g| {
            if !g.prefixes.is_empty() && !scope.allows_group(Some(g.id)) {
                g.prefixes.clear();
            }
            g
        })
        .collect())
}

/// Refuse a folder id that does not exist, with a 400 that names it.
///
/// `nodes.group_id` is a foreign key, so an unknown id otherwise aborts the statement and reaches
/// the client as a 500 that names nothing. Every write path that accepts a destination folder owes
/// this check, and it lives here because there are three of them (ADR-124 決定 1) — the import,
/// the single move and the bulk move. `None` is always fine: it means "ungrouped".
pub(super) async fn require_group_exists(
    admin: &super::AdminState,
    group: Option<Uuid>,
) -> Result<(), ApiError> {
    let Some(group) = group else { return Ok(()) };
    let known = admin.groups.exists(group).await.map_err(|e| {
        ApiError::from_internal(e.as_ref(), "check node group", "failed to read node groups")
    })?;
    if !known {
        return Err(ApiError::bad_request(
            "invalid_group",
            format!("no node group {group}"),
        ));
    }
    Ok(())
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
        (status = 201, description = "Group created", body = CreatedId),
        (status = 400, description = "Empty name, unknown group type, or an invalid pool name", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
        (status = 503, description = "This core has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn create_node_group(
    _guard: RequireManageConfig,
    admin: Admin,
    Json(body): Json<GroupBody>,
) -> ApiResult<(StatusCode, Json<CreatedId>)> {
    let group_type = parse_group_body(&body)?;
    let pool = validate_pool_create(body.pool)?;
    let id = admin
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
    Ok((StatusCode::CREATED, Json(CreatedId { id })))
}

#[utoipa::path(
    put, path = "/api/v1/node-groups/{id}", tag = "groups",
    params(("id" = Uuid, Path, description = "Group id")),
    request_body = GroupBody,
    responses(
        (status = 204, description = "Group updated"),
        (status = 400, description = "Empty name, unknown group type, an invalid pool name, or a move that would nest the group inside its own subtree", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
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
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
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

/// Which way to order this folder's children (ADR-130).
#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct SortChildren {
    /// `asc` = A → Z, `desc` = Z → A. Required: there is no sensible default for a command whose
    /// whole content is the direction, and a missing field would silently pick one.
    direction: SortDirection,
}

/// Arrange one folder's **direct** children in name order, writing the tree's stored `sort_order`.
///
/// Subfolders and member nodes are renumbered within their own sibling scopes, so the two never
/// interleave — the tree draws every folder above every node whatever the values are. Folders
/// deeper down are untouched: the operator right-clicked one folder.
///
/// 🚨 **This replaces an order somebody arranged by hand, and nothing keeps the old one.** That is
/// the decision (ADR-130 決定 5) rather than an oversight — the command is reached by right-clicking
/// the folder it acts on, which is the same consent a file manager asks for.
///
/// ⚠️ **Not a bulk `placement`.** Doing this by calling `PUT /node-groups/{id}/placement` once per
/// child would be a partial write with nothing to read back when it fails halfway, which is the
/// same reason a multi-node drag appends rather than inserting (`nodeTreeDnd.ts`). One request,
/// one transaction.
#[utoipa::path(
    post, path = "/api/v1/node-groups/{id}/sort", tag = "groups",
    params(("id" = Uuid, Path, description = "Group id")),
    request_body = SortChildren,
    responses(
        (status = 204, description = "The folder's direct children were renumbered in name order"),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
        (status = 404, description = "No such group", body = super::error::ErrorBody),
        (status = 503, description = "This core has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn sort_group_children(
    _guard: RequireManageConfig,
    admin: Admin,
    Path(id): Path<Uuid>,
    Json(body): Json<SortChildren>,
) -> ApiResult<StatusCode> {
    let sorted = admin
        .groups
        .sort_children(id, body.direction)
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "sort group children",
                "failed to sort the folder",
            )
        })?;
    if sorted {
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
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
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

/// A folder's map pin. Both fields or neither — see [`set_node_group_geo`].
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
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
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

#[utoipa::path(
    delete, path = "/api/v1/node-groups/{id}", tag = "groups",
    params(("id" = Uuid, Path, description = "Group id")),
    responses(
        (status = 204, description = "Group deleted"),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
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

/// The folder's labels, in full (ADR-135 inc. 2).
///
/// ⚠️ **Whole value, not a diff**, and **a sub-resource rather than a field on `GroupBody`** — the
/// same shape `geo` and `prefixes` chose, for three reasons and the first settles it:
///
/// 1. **Scoping.** This is `GroupFiltered` + `require_visible_group`, because a folder's labels
///    reach every node under it and `manage_config` is held by Operator, who can be group-scoped
///    (ADR-131 決定 8). `PUT /node-groups/{id}` claims `ADMIN_CFG` and takes no `Scoped`; putting
///    labels on its body would mean either widening that route's claim — changing rename, move and
///    re-pool for everyone — or shipping a scope-blind label write.
/// 2. **Three-state cost.** `GroupBody.pool` is already an `Option<String>` whose doc has to
///    explain that absent means unchanged, and `GroupModal` always sends it for exactly that
///    reason. A second such field doubles the trap ADR-135 決定 4 exists for.
/// 3. **The dialog.** A per-label `DELETE` would act the moment ✕ is clicked — before Save, and
///    with no way back. Clearing every label is `{"tags": []}`.
#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct GroupTags {
    /// Labels this folder supplies to its whole subtree. Same rules as a node's: free-form, at
    /// most 64 characters each, at most 32 of them.
    #[serde(default)]
    tags: Vec<String>,
    /// Labels this folder refuses to inherit from its own ancestors — and therefore takes away
    /// from everything beneath it too. Not length- or character-checked, for the reason
    /// `api::nodes::normalized_removals` records.
    #[serde(default)]
    tags_excluded: Vec<String>,
}

#[utoipa::path(
    put, path = "/api/v1/node-groups/{id}/tags", tag = "groups",
    params(("id" = String, Path, description = "Folder id")),
    request_body = GroupTags,
    responses(
        (status = 204, description = "The folder's labels were replaced"),
        (status = 400, description = "An empty label, one over 64 characters, one carrying a control character, or more than 32", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
        (status = 404, description = "No such folder, or not one this caller may act on", body = super::error::ErrorBody),
        (status = 503, description = "This deployment has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn set_node_group_tags(
    _guard: RequireManageConfig,
    Scoped(scope): Scoped,
    admin: Admin,
    Path(id): Path<Uuid>,
    Json(body): Json<GroupTags>,
) -> ApiResult<StatusCode> {
    // Before anything is read or written, exactly as `set_node_group_prefixes` does: a folder this
    // caller may not act on is a 404, not a silent no-op.
    super::scope::require_visible_group(&scope, id)?;
    let tags = super::nodes::validated_labels(body.tags)?;
    let excluded = super::nodes::normalized_removals(body.tags_excluded);
    let found = admin
        .groups
        .set_tags(id, &tags, &excluded)
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "set node group tags",
                "failed to update node group",
            )
        })?;
    if found {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found("group_not_found", "no such node group"))
    }
}

/// One hand-made IP range on a folder.
#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct GroupPrefixEntry {
    /// A CIDR. Host bits are allowed and canonicalised — `192.168.1.5/24` is stored as
    /// `192.168.1.0/24`, because that is what a person reading a device's config types.
    prefix: String,
    /// What the range is called ("Matsuyama LAN"), or empty.
    #[serde(default)]
    description: String,
}

/// The folder's hand-made ranges, in full.
///
/// ⚠️ **Whole list, not a diff.** The three sibling sub-resources here (`pool`, `geo`,
/// `placement`) are whole-value PUTs for the same reason: the editor is a dialog with a Save
/// button, so a per-row `DELETE` would act the moment ✕ is clicked — before Save, and with no way
/// back. Clearing every hand-made range is `{"prefixes": []}`, which is why there is no companion
/// DELETE endpoint. (A per-row path could not carry a CIDR anyway: `/` and `:` are in the value.)
#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct GroupPrefixes {
    prefixes: Vec<GroupPrefixEntry>,
}

#[utoipa::path(
    put, path = "/api/v1/node-groups/{id}/prefixes", tag = "groups",
    params(("id" = String, Path, description = "Folder id")),
    request_body = GroupPrefixes,
    responses(
        (status = 204, description = "The folder's hand-made ranges were replaced"),
        (status = 400, description = "A value that is not an IP range, a duplicate, one a sync already owns, too many, or a description that is too long", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
        (status = 404, description = "No such folder, or not one this caller may act on", body = super::error::ErrorBody),
        (status = 503, description = "This deployment has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn set_node_group_prefixes(
    _guard: RequireManageConfig,
    Scoped(scope): Scoped,
    admin: Admin,
    Path(id): Path<Uuid>,
    Json(body): Json<GroupPrefixes>,
) -> ApiResult<StatusCode> {
    // Before anything is read or written: a folder this caller may not act on is a 404, not a
    // silent no-op. `GroupFiltered` on the ledger line is what makes this obligatory.
    super::scope::require_visible_group(&scope, id)?;
    if body.prefixes.len() > MAX_GROUP_PREFIXES {
        return Err(ApiError::bad_request(
            "too_many_prefixes",
            format!(
                "a folder may carry at most {MAX_GROUP_PREFIXES} ranges, got {}",
                body.prefixes.len()
            ),
        ));
    }
    if !admin.groups.exists(id).await.map_err(|e| {
        ApiError::from_internal(e.as_ref(), "check node group", "failed to read node groups")
    })? {
        return Err(ApiError::not_found("group_not_found", "no such node group"));
    }

    // 🚨 Canonicalise one row at a time, on the pool, **before** the transaction opens.
    //
    // Two reasons, and neither is style. A failed statement poisons its transaction, so probing
    // inside the write would abort the very thing being guarded. And one `unnest` over the batch
    // cannot say *which* value was wrong — PostgreSQL does not promise row-evaluation order — so
    // the only honest message would be "one of these is not an IP range". At form-submit pace the
    // round trips are free; naming the offending row is not.
    let mut rows: Vec<(String, String)> = Vec::with_capacity(body.prefixes.len());
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for entry in &body.prefixes {
        let raw = entry.prefix.trim();
        if raw.is_empty() {
            return Err(ApiError::bad_request(
                "invalid_prefix",
                "a range must not be empty",
            ));
        }
        let description = entry.description.trim();
        if description.chars().count() > MAX_PREFIX_DESCRIPTION {
            return Err(ApiError::bad_request(
                "prefix_description_too_long",
                format!("a description may be at most {MAX_PREFIX_DESCRIPTION} characters"),
            ));
        }
        let Some(canonical) = admin.groups.canonical_prefix(raw).await.map_err(|e| {
            ApiError::from_internal(e.as_ref(), "canonicalise prefix", "failed to read prefixes")
        })?
        else {
            // Echoing the value is safe and is the point: it is the caller's own input, not an
            // internal error's text (`security.md` forbids the latter, never the former).
            return Err(ApiError::bad_request(
                "invalid_prefix",
                format!("'{raw}' is not an IP range"),
            ));
        };
        if !seen.insert(canonical.clone()) {
            return Err(ApiError::bad_request(
                "duplicate_prefix",
                format!("'{canonical}' is listed twice"),
            ));
        }
        rows.push((canonical, description.to_owned()));
    }

    // A range a sync owns is refused by name rather than silently dropped: the operator typed it,
    // and nothing on screen would otherwise say it did not land (ADR-131 決定 5).
    let sync_owned = admin.groups.sync_owned_prefixes(id).await.map_err(|e| {
        ApiError::from_internal(e.as_ref(), "read sync prefixes", "failed to read prefixes")
    })?;
    if let Some((clash, _)) = rows.iter().find(|(p, _)| sync_owned.contains(p)) {
        return Err(ApiError::bad_request(
            "prefix_owned_by_sync",
            format!("'{clash}' is maintained by a sync and cannot be edited here"),
        ));
    }

    admin
        .groups
        .set_manual_prefixes(id, &rows)
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "set node group prefixes",
                "failed to set node group prefixes",
            )
        })?;
    Ok(StatusCode::NO_CONTENT)
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
            ("PUT", format!("/api/v1/node-groups/{ID}/pool")),
            ("PUT", format!("/api/v1/node-groups/{ID}/geo")),
            ("POST", format!("/api/v1/node-groups/{ID}/sort")),
            ("PUT", format!("/api/v1/node-groups/{ID}/prefixes")),
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
    async fn the_tree_reads_openly_and_operators_reshape_it() {
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
        // The folders an operator files nodes into are theirs to reshape (ADR-057); a viewer
        // reads the tree and changes none of it. 503 = past the guard, into skeleton mode.
        let st = private_state();
        for (role, want) in [
            (Role::Viewer, StatusCode::FORBIDDEN),
            (Role::Operator, StatusCode::SERVICE_UNAVAILABLE),
        ] {
            let token = st
                .sessions
                .issue(Uuid::new_v4(), Principal::new(role, Scope::All), "u");
            for (method, path) in write_routes() {
                assert_eq!(
                    status_of(st.clone(), method, &path, Some(&token)).await,
                    want,
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
    // ── An accepted write (ADR-115) ──────────────────────────────────────────────────

    /// A folder group is created and appears in the tree.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn creating_a_group_stores_it_and_lists_it(pool: sqlx::PgPool) {
        use crate::api::tests_support::{live_state, send, token};
        let st = live_state(pool.clone()).await;
        let tok = token(&st, yagra_common::Role::Admin);
        let (status, body) = send(
            &st,
            "POST",
            "/api/v1/node-groups",
            &tok,
            Some(serde_json::json!({ "name": "tokyo", "group_type": "site" })),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::CREATED, "{body}");
        assert_eq!(crate::pgtest::rows(&pool, "node_groups").await, 1);

        let (status, list) = send(&st, "GET", "/api/v1/node-groups", &tok, None).await;
        assert_eq!(status, axum::http::StatusCode::OK, "{list}");
        assert!(list.to_string().contains("tokyo"), "{list}");
    }

    /// Sorting a folder renumbers its subfolders and its member nodes, in name order (ADR-130).
    ///
    /// The two scopes are asserted **separately and both**, because they are two statements over
    /// two tables, and a transaction that renumbered only the folders would look entirely correct
    /// from a screenshot of the folder list.
    ///
    /// Names are deliberately mixed-case and deliberately not in creation order: `create` appends
    /// at `MAX(sort_order)+1`, so the starting state is creation order. Spelled ASCII-betically,
    /// `Alpha` and `Mike` would come before *every* lowercase name and the test would pass just as
    /// well on a case-sensitive `ORDER BY` — so the two capitals sit where only `lower(name)` puts
    /// them, and the descending pass puts them last.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn sorting_a_folder_renumbers_its_subfolders_and_its_nodes(pool: sqlx::PgPool) {
        use crate::api::tests_support::{live_state, send, token};
        use crate::groups::{GroupRepo, GroupType};
        let st = live_state(pool.clone()).await;
        let tok = token(&st, yagra_common::Role::Admin);
        let groups = GroupRepo::new(pool.clone());
        let repo = crate::pgtest::repo(pool.clone());

        let parent = groups
            .create("parent", GroupType::Site, None, None)
            .await
            .expect("parent");
        for name in ["charlie", "Alpha", "bravo"] {
            groups
                .create(name, GroupType::Generic, Some(parent), None)
                .await
                .expect("subfolder");
        }
        for (i, name) in ["zulu", "Mike", "november"].into_iter().enumerate() {
            crate::pgtest::node(
                &pool,
                name,
                10 + u8::try_from(i).expect("small"),
                Some(parent),
            )
            .await;
        }

        let read_folders = || async {
            let all = groups.list().await.expect("list");
            groups
                .ordered_siblings(Some(parent))
                .await
                .expect("siblings")
                .into_iter()
                .map(|(id, _)| {
                    all.iter()
                        .find(|g| g.id == id)
                        .expect("listed")
                        .name
                        .clone()
                })
                .collect::<Vec<_>>()
        };
        let read_nodes = || async {
            let ordered = repo
                .ordered_nodes_in_group(Some(parent))
                .await
                .expect("members");
            let mut out = Vec::new();
            for (id, _) in ordered {
                out.push(repo.get_node(id).await.expect("get").expect("node").name);
            }
            out
        };

        // What the tree would draw right now: creation order, in both scopes.
        assert_eq!(read_folders().await, ["charlie", "Alpha", "bravo"]);
        assert_eq!(read_nodes().await, ["zulu", "Mike", "november"]);

        // 204, not `is_success()` — the documented status is what the WebUI branches on.
        let (status, body) = send(
            &st,
            "POST",
            &format!("/api/v1/node-groups/{parent}/sort"),
            &tok,
            Some(serde_json::json!({ "direction": "asc" })),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::NO_CONTENT, "{body}");
        assert_eq!(read_folders().await, ["Alpha", "bravo", "charlie"]);
        assert_eq!(read_nodes().await, ["Mike", "november", "zulu"]);

        let (status, body) = send(
            &st,
            "POST",
            &format!("/api/v1/node-groups/{parent}/sort"),
            &tok,
            Some(serde_json::json!({ "direction": "desc" })),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::NO_CONTENT, "{body}");
        assert_eq!(read_folders().await, ["charlie", "bravo", "Alpha"]);
        assert_eq!(read_nodes().await, ["zulu", "november", "Mike"]);

        // A folder that is not there is a 404, never a silent no-op — otherwise the tree reports
        // success for a folder somebody else has just deleted.
        let (status, _) = send(
            &st,
            "POST",
            &format!("/api/v1/node-groups/{}/sort", uuid::Uuid::new_v4()),
            &tok,
            Some(serde_json::json!({ "direction": "asc" })),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::NOT_FOUND);
    }

    /// Sorting one folder leaves every **other** scope alone (ADR-130 決定 2).
    ///
    /// The two `UPDATE`s carry the whole of the scoping in their `WHERE`. Drop either predicate and
    /// the entire table is renumbered — while the folder the operator clicked still looks perfectly
    /// sorted. So the assertion that matters here is about the rows the request did *not* name.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn sorting_one_folder_does_not_touch_another(pool: sqlx::PgPool) {
        use crate::api::tests_support::{live_state, send, token};
        use crate::groups::{GroupRepo, GroupType};
        let st = live_state(pool.clone()).await;
        let tok = token(&st, yagra_common::Role::Admin);
        let groups = GroupRepo::new(pool.clone());
        let repo = crate::pgtest::repo(pool.clone());

        let target = groups
            .create("target", GroupType::Site, None, None)
            .await
            .expect("target");
        let other = groups
            .create("other", GroupType::Site, None, None)
            .await
            .expect("other");
        // Deliberately NOT in name order, so "was not touched" is distinguishable from "was sorted
        // and happened to already be right".
        for name in ["b", "a"] {
            groups
                .create(name, GroupType::Generic, Some(other), None)
                .await
                .expect("sub");
        }
        crate::pgtest::node(&pool, "zz", 1, Some(other)).await;
        crate::pgtest::node(&pool, "aa", 2, Some(other)).await;
        crate::pgtest::node(&pool, "yy", 3, Some(target)).await;

        let before_folders = groups.ordered_siblings(Some(other)).await.expect("sibs");
        let before_nodes = repo
            .ordered_nodes_in_group(Some(other))
            .await
            .expect("members");

        let (status, body) = send(
            &st,
            "POST",
            &format!("/api/v1/node-groups/{target}/sort"),
            &tok,
            Some(serde_json::json!({ "direction": "asc" })),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::NO_CONTENT, "{body}");

        assert_eq!(
            groups.ordered_siblings(Some(other)).await.expect("sibs"),
            before_folders,
            "another folder's subfolders were renumbered"
        );
        assert_eq!(
            repo.ordered_nodes_in_group(Some(other))
                .await
                .expect("members"),
            before_nodes,
            "another folder's nodes were renumbered"
        );
    }

    /// 🚨 A breadcrumb ancestor is listed by name and **without its prefixes** (ADR-100 decision
    /// 10).
    ///
    /// The tree needs the ancestor row or every visible root renders as an orphan, so the row
    /// itself cannot be dropped. But the ancestor here is a site the caller was refused, and its
    /// subnet layout is not part of "a name" — a scoped operator who can see one rack must not
    /// learn the addressing of the building it sits in. `allows_group_row` admits the row;
    /// `allows_group` is what decides the prefixes, and this test is the only thing that would
    /// notice if the two were unified.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_breadcrumb_ancestor_is_named_without_its_prefixes(pool: sqlx::PgPool) {
        use crate::api::tests_support::{live_state, scoped_token, send, token};
        let st = live_state(pool.clone()).await;
        let admin = token(&st, yagra_common::Role::Admin);

        let (_, parent) = send(
            &st,
            "POST",
            "/api/v1/node-groups",
            &admin,
            Some(serde_json::json!({ "name": "Matsuyama Home", "group_type": "site" })),
        )
        .await;
        let parent_id: uuid::Uuid = parent["id"]
            .as_str()
            .expect("parent id")
            .parse()
            .expect("uuid");
        let (_, child) = send(
            &st,
            "POST",
            "/api/v1/node-groups",
            &admin,
            Some(serde_json::json!({
                "name": "Rack 1", "group_type": "generic", "parent_id": parent_id,
            })),
        )
        .await;
        let child_id: uuid::Uuid = child["id"]
            .as_str()
            .expect("child id")
            .parse()
            .expect("uuid");

        // Both folders carry a prefix, so "the ancestor's is missing" cannot be confused with
        // "nothing has any".
        for (group, prefix) in [(parent_id, "192.168.1.0/24"), (child_id, "192.168.9.0/24")] {
            sqlx::query(
                "INSERT INTO node_group_prefixes (group_id, prefix, description) \
                 VALUES ($1, $2::cidr, 'lab')",
            )
            .bind(group)
            .bind(prefix)
            .execute(&pool)
            .await
            .expect("seed prefix");
        }

        // Scoped to the child only: the parent arrives as a breadcrumb.
        let scoped = scoped_token(&st, &[child_id]);
        let (status, list) = send(&st, "GET", "/api/v1/node-groups", &scoped, None).await;
        assert_eq!(status, axum::http::StatusCode::OK, "{list}");
        let rows = list.as_array().expect("a list");
        let of = |id: uuid::Uuid| {
            rows.iter()
                .find(|g| g["id"] == id.to_string())
                .unwrap_or_else(|| panic!("row {id} present"))
        };
        assert_eq!(
            of(child_id)["prefixes"],
            serde_json::json!([{ "prefix": "192.168.9.0/24", "description": "lab", "source": "manual" }]),
            "the folder in scope keeps its prefixes"
        );
        assert_eq!(
            of(parent_id)["prefixes"],
            serde_json::json!([]),
            "the breadcrumb ancestor is named, and says nothing about its addressing"
        );

        // The same read as an unscoped Admin still shows both, so the test above is measuring the
        // scope filter and not a write that never happened.
        let (_, all) = send(&st, "GET", "/api/v1/node-groups", &admin, None).await;
        let all_rows = all.as_array().expect("a list");
        let parent_row = all_rows
            .iter()
            .find(|g| g["id"] == parent_id.to_string())
            .expect("parent row");
        assert_eq!(
            parent_row["prefixes"][0]["prefix"], "192.168.1.0/24",
            "unscoped, the ancestor's prefixes are there"
        );
    }

    // ── ADR-135 inc. 2: a folder's labels, and what inherits them ───────────────────────

    /// 🚨 An accepted write (ADR-115), asserted on the **row** and on what the row implies.
    ///
    /// A 204 says the handler returned. What matters is that the labels reached the folder *and*
    /// that every folder under it now reports them as effective — that second half is the whole
    /// feature, and it is resolved on read, so a write that stored correctly and resolved wrongly
    /// would pass a status check and fail the operator.
    ///
    /// ⚠️ The documented status is named, not `is_success()`: 200 and 204 are both successes and
    /// only one of them is this route's contract.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_folders_labels_are_stored_and_reach_its_whole_subtree(pool: sqlx::PgPool) {
        use crate::api::tests_support::{live_state, send, token};
        let st = live_state(pool.clone()).await;
        let admin = token(&st, yagra_common::Role::Admin);

        let mk = |name: &'static str, parent: Option<uuid::Uuid>| {
            let admin = admin.clone();
            let st = &st;
            async move {
                let mut body = serde_json::json!({ "name": name, "group_type": "site" });
                if let Some(p) = parent {
                    body["parent_id"] = serde_json::json!(p);
                }
                let (_, row) = send(st, "POST", "/api/v1/node-groups", &admin, Some(body)).await;
                row["id"]
                    .as_str()
                    .expect("id")
                    .parse::<uuid::Uuid>()
                    .expect("uuid")
            }
        };
        let region = mk("Japan", None).await;
        let site = mk("Matsuyama", Some(region)).await;
        let rack = mk("Rack 1", Some(site)).await;

        let (status, body) = send(
            &st,
            "PUT",
            &format!("/api/v1/node-groups/{region}/tags"),
            &admin,
            Some(serde_json::json!({ "tags": ["  JAPAN  ", "core"] })),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::NO_CONTENT, "{body}");

        let (_, list) = send(&st, "GET", "/api/v1/node-groups", &admin, None).await;
        let rows = list.as_array().expect("a list");
        let of = |id: uuid::Uuid| {
            rows.iter()
                .find(|g| g["id"] == id.to_string())
                .unwrap_or_else(|| panic!("row {id} present"))
                .clone()
        };
        // Trimmed and sorted by the validator, and stored on the folder that was named.
        assert_eq!(of(region)["tags"], serde_json::json!(["JAPAN", "core"]));
        assert_eq!(of(site)["tags"], serde_json::json!([]));
        // …and effective everywhere beneath it, two levels down.
        assert_eq!(
            of(site)["effective_tags"],
            serde_json::json!(["JAPAN", "core"])
        );
        assert_eq!(
            of(rack)["effective_tags"],
            serde_json::json!(["JAPAN", "core"]),
            "a grandchild inherits through the folder between it and the label"
        );

        // A refusal partway down takes the label away from that folder and everything under it.
        let (status, body) = send(
            &st,
            "PUT",
            &format!("/api/v1/node-groups/{site}/tags"),
            &admin,
            Some(serde_json::json!({ "tags": ["matsuyama"], "tags_excluded": ["core"] })),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::NO_CONTENT, "{body}");
        let (_, list) = send(&st, "GET", "/api/v1/node-groups", &admin, None).await;
        let rows = list.as_array().expect("a list");
        let of = |id: uuid::Uuid| {
            rows.iter()
                .find(|g| g["id"] == id.to_string())
                .unwrap_or_else(|| panic!("row {id} present"))
                .clone()
        };
        assert_eq!(
            of(site)["effective_tags"],
            serde_json::json!(["JAPAN", "matsuyama"])
        );
        assert_eq!(
            of(rack)["effective_tags"],
            serde_json::json!(["JAPAN", "matsuyama"]),
            "an exclusion applies to the whole subtree below it, not only to the folder that set it"
        );
        assert_eq!(
            of(region)["effective_tags"],
            serde_json::json!(["JAPAN", "core"]),
            "and never upwards"
        );

        // The validator is reached: a label over the limit is a 400, not a truncated row.
        let (status, _) = send(
            &st,
            "PUT",
            &format!("/api/v1/node-groups/{region}/tags"),
            &admin,
            Some(serde_json::json!({ "tags": ["x".repeat(65)] })),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::BAD_REQUEST);
    }

    /// 🚨 A group-scoped caller cannot label a folder outside its scope.
    ///
    /// This route claims `GroupFiltered` in the ledger rather than the `ADMIN_CFG` its `geo` and
    /// `pool` siblings claim, and the claim is only worth anything if the handler acts on it. Both
    /// directions on purpose: a test that only sees the refusal would pass on a handler that
    /// refuses everyone.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn labelling_a_folder_outside_the_callers_scope_is_refused(pool: sqlx::PgPool) {
        use crate::api::tests_support::{live_state, scoped_token, send, token};
        let st = live_state(pool.clone()).await;
        let admin = token(&st, yagra_common::Role::Admin);

        let mut ids = Vec::new();
        for name in ["Mine", "Theirs"] {
            let (_, row) = send(
                &st,
                "POST",
                "/api/v1/node-groups",
                &admin,
                Some(serde_json::json!({ "name": name, "group_type": "site" })),
            )
            .await;
            ids.push(
                row["id"]
                    .as_str()
                    .expect("id")
                    .parse::<uuid::Uuid>()
                    .expect("uuid"),
            );
        }
        let (mine, theirs) = (ids[0], ids[1]);
        let scoped = scoped_token(&st, &[mine]);

        let (status, body) = send(
            &st,
            "PUT",
            &format!("/api/v1/node-groups/{mine}/tags"),
            &scoped,
            Some(serde_json::json!({ "tags": ["JAPAN"] })),
        )
        .await;
        assert_eq!(
            status,
            axum::http::StatusCode::NO_CONTENT,
            "the folder in scope is writable: {body}"
        );

        let (status, _) = send(
            &st,
            "PUT",
            &format!("/api/v1/node-groups/{theirs}/tags"),
            &scoped,
            Some(serde_json::json!({ "tags": ["JAPAN"] })),
        )
        .await;
        assert_eq!(
            status,
            axum::http::StatusCode::NOT_FOUND,
            "a folder this caller cannot see is a 404, not a silent no-op"
        );
        // And nothing was written to it.
        let (_, list) = send(&st, "GET", "/api/v1/node-groups", &admin, None).await;
        let their_row = list
            .as_array()
            .expect("a list")
            .iter()
            .find(|g| g["id"] == theirs.to_string())
            .expect("row")
            .clone();
        assert_eq!(their_row["tags"], serde_json::json!([]));
    }

    // ── ADR-131: the hand-made IP ranges ────────────────────────────────────────────────

    /// 🚨 An accepted write, asserted on the **row** rather than the status.
    ///
    /// A 204 says the handler returned; it does not say a range was stored, canonicalised, or
    /// marked as the operator's. This checks all three, and then reads them back through
    /// `GET /node-groups` — the surface the editor actually renders from.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn setting_a_folders_ranges_stores_them_as_the_operators(pool: sqlx::PgPool) {
        use crate::api::tests_support::{live_state, send, token};
        let st = live_state(pool.clone()).await;
        let tok = token(&st, yagra_common::Role::Admin);
        let (_, created) = send(
            &st,
            "POST",
            "/api/v1/node-groups",
            &tok,
            Some(serde_json::json!({ "name": "matsuyama", "group_type": "site" })),
        )
        .await;
        let id = created["id"].as_str().expect("id").to_owned();

        let (status, body) = send(
            &st,
            "PUT",
            &format!("/api/v1/node-groups/{id}/prefixes"),
            &tok,
            Some(serde_json::json!({ "prefixes": [
                // Host bits set on purpose: a plain `::cidr` cast would refuse this, and it is
                // what a person reading a device's config types.
                { "prefix": "192.168.1.5/24", "description": "office" },
                { "prefix": "2001:db8::1/64" },
            ] })),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::NO_CONTENT, "{body}");
        assert_eq!(crate::pgtest::rows(&pool, "node_group_prefixes").await, 2);

        let (status, list) = send(&st, "GET", "/api/v1/node-groups", &tok, None).await;
        assert_eq!(status, axum::http::StatusCode::OK, "{list}");
        let row = list
            .as_array()
            .expect("a list")
            .iter()
            .find(|g| g["id"] == id)
            .expect("the folder")
            .clone();
        let mut seen: Vec<(String, String)> = row["prefixes"]
            .as_array()
            .expect("prefixes")
            .iter()
            .map(|p| {
                (
                    p["prefix"].as_str().expect("prefix").to_owned(),
                    p["source"].as_str().expect("source").to_owned(),
                )
            })
            .collect();
        seen.sort();
        assert_eq!(
            seen,
            vec![
                ("192.168.1.0/24".to_string(), "manual".to_string()),
                ("2001:db8::/64".to_string(), "manual".to_string()),
            ],
            "both were canonicalised and both are the operator's"
        );

        // The empty list is the clear: there is deliberately no DELETE endpoint.
        let (status, body) = send(
            &st,
            "PUT",
            &format!("/api/v1/node-groups/{id}/prefixes"),
            &tok,
            Some(serde_json::json!({ "prefixes": [] })),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::NO_CONTENT, "{body}");
        assert_eq!(crate::pgtest::rows(&pool, "node_group_prefixes").await, 0);
    }

    /// A value that is not an IP range is a 400 that **names it**, and nothing is written.
    ///
    /// The second half is the point: the canonicalisation probe runs on the pool before the
    /// transaction opens, so a bad row cannot abort a write that had already begun.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_range_that_is_not_an_address_is_refused_by_name(pool: sqlx::PgPool) {
        use crate::api::tests_support::{live_state, send, token};
        let st = live_state(pool.clone()).await;
        let tok = token(&st, yagra_common::Role::Admin);
        let (_, created) = send(
            &st,
            "POST",
            "/api/v1/node-groups",
            &tok,
            Some(serde_json::json!({ "name": "site", "group_type": "site" })),
        )
        .await;
        let id = created["id"].as_str().expect("id").to_owned();

        let (status, body) = send(
            &st,
            "PUT",
            &format!("/api/v1/node-groups/{id}/prefixes"),
            &tok,
            Some(serde_json::json!({ "prefixes": [
                { "prefix": "10.0.0.0/8" },
                { "prefix": "not-an-address" },
            ] })),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(body["error"]["code"], "invalid_prefix", "{body}");
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap_or_default()
                .contains("not-an-address"),
            "the offending value is named: {body}"
        );
        assert_eq!(
            crate::pgtest::rows(&pool, "node_group_prefixes").await,
            0,
            "the good row must not have landed either"
        );
    }

    /// A range a sync owns is refused by name rather than silently dropped.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_range_a_sync_owns_cannot_be_taken_over_here(pool: sqlx::PgPool) {
        use crate::api::tests_support::{live_state, send, token};
        let st = live_state(pool.clone()).await;
        let tok = token(&st, yagra_common::Role::Admin);
        let (_, created) = send(
            &st,
            "POST",
            "/api/v1/node-groups",
            &tok,
            Some(serde_json::json!({ "name": "site", "group_type": "site" })),
        )
        .await;
        let id = created["id"].as_str().expect("id").to_owned();
        let group: Uuid = id.parse().expect("uuid");
        let server = crate::pgtest::netbox_server(&pool, "nb").await;
        sqlx::query(
            "INSERT INTO node_group_prefixes (group_id, prefix, description, netbox_server_id) \
             VALUES ($1, network($2::inet)::cidr, 'from netbox', $3)",
        )
        .bind(group)
        .bind("172.16.0.0/12")
        .bind(server)
        .execute(&pool)
        .await
        .expect("seed");

        let (status, body) = send(
            &st,
            "PUT",
            &format!("/api/v1/node-groups/{id}/prefixes"),
            &tok,
            Some(serde_json::json!({ "prefixes": [{ "prefix": "172.16.0.0/12" }] })),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(body["error"]["code"], "prefix_owned_by_sync", "{body}");
        assert_eq!(crate::pgtest::rows(&pool, "node_group_prefixes").await, 1);
    }

    /// A folder outside a scoped caller's reach is a 404, not a silent write.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_scoped_caller_cannot_set_ranges_on_a_folder_it_cannot_see(pool: sqlx::PgPool) {
        use crate::api::tests_support::{live_state, scoped_token, send, token};
        let st = live_state(pool.clone()).await;
        let admin = token(&st, yagra_common::Role::Admin);
        let (_, mine) = send(
            &st,
            "POST",
            "/api/v1/node-groups",
            &admin,
            Some(serde_json::json!({ "name": "mine", "group_type": "site" })),
        )
        .await;
        let (_, theirs) = send(
            &st,
            "POST",
            "/api/v1/node-groups",
            &admin,
            Some(serde_json::json!({ "name": "theirs", "group_type": "site" })),
        )
        .await;
        let mine_id: Uuid = mine["id"].as_str().expect("id").parse().expect("uuid");
        let theirs_id = theirs["id"].as_str().expect("id").to_owned();

        let scoped = scoped_token(&st, &[mine_id]);
        let (status, body) = send(
            &st,
            "PUT",
            &format!("/api/v1/node-groups/{theirs_id}/prefixes"),
            &scoped,
            Some(serde_json::json!({ "prefixes": [{ "prefix": "10.0.0.0/8" }] })),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::NOT_FOUND, "{body}");
        assert_eq!(crate::pgtest::rows(&pool, "node_group_prefixes").await, 0);
    }
}
