// SPDX-License-Identifier: AGPL-3.0-only
//! Maintenance windows — the one way an operator says "do not alert me about this".
//!
//! A window is planned configuration (`ManageMaintenance`). Yagra used to have a second way, the
//! mute, which silenced only the notification; ADR-234 removed it, so a window is the whole answer.
//!
//! A window suppresses alerting, so it is load-bearing for the "no notification floods" goal — and
//! therefore validated hard at the edge. A window that ends before it starts silences nothing
//! while looking to the operator like it worked, which is a worse failure than a rejection
//! ([`window_bounds`]).

use super::extract::{Admin, RequireManageMaintenance, RequireView, Scoped, VisibleNode};
use super::scope::ScopeTarget;
use super::util::CreatedId;
use super::{parse_rfc3339, ApiError, ApiResult, ApiState};
use crate::alerts::maintenance::{inherited_maintenance_end, CoverageFacts};
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{get, post, put},
    Json, Router,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// This domain's slice of the OpenAPI document (ADR-035), merged by [`super::openapi::document`].
#[derive(utoipa::OpenApi)]
#[openapi(paths(
    list_maintenance_windows,
    create_maintenance_window,
    create_maintenance_windows_bulk,
    clear_ended_maintenance_windows,
    set_maintenance_window_enabled,
    end_maintenance_window,
    delete_maintenance_window,
    list_suppression_exemptions,
    set_node_maintenance_exemption,
    release_nodes_from_maintenance
))]
pub(super) struct Doc;

/// The maintenance routes, merged into `/api/v1` by [`super::router`].
pub(crate) fn routes() -> Router<ApiState> {
    Router::new()
        .route(
            "/api/v1/maintenance-windows",
            get(list_maintenance_windows)
                .post(create_maintenance_window)
                .delete(clear_ended_maintenance_windows),
        )
        .route(
            "/api/v1/maintenance-windows/:id",
            put(set_maintenance_window_enabled).delete(delete_maintenance_window),
        )
        .route(
            "/api/v1/maintenance-windows/:id/end",
            post(end_maintenance_window),
        )
        .route(
            "/api/v1/maintenance-windows/bulk",
            post(create_maintenance_windows_bulk),
        )
        // Suppression exemptions. The write hangs off `/nodes/:node_id/…` because that is what it
        // addresses — and it is what earns it the `VisibleNode` extractor — but it belongs to this
        // domain, not `api/nodes.rs`: what it means is defined by windows.
        .route(
            "/api/v1/nodes/:node_id/maintenance-exemption",
            put(set_node_maintenance_exemption),
        )
        .route(
            "/api/v1/suppression-exemptions",
            get(list_suppression_exemptions),
        )
        .route(
            "/api/v1/maintenance/release",
            post(release_nodes_from_maintenance),
        )
}

/// The scope levels a maintenance window may target. `group_id` is a folder-group UUID (resolved
/// recursively — the All Nodes right-click scope); the other two mirror threshold scoping
/// (ADR-013). Every stored [`WindowScope`] but `System`, which a test pins.
///
/// ⚠️ **`system` is deliberately absent.** A fleet-wide window exists (ADR-050) but is opened only
/// by the upgrade path, which knows its own end time. Leaving it out of this list means no caller —
/// scoped or not — can silence the entire fleet through the ordinary CRUD surface, and the list
/// stays the answer to "what may an operator create", which is what the error message promises.
const WINDOW_SCOPE_LEVELS: [&str; 3] = ["profile", "node", "group_id"];

/// Ceiling on one bulk suppression request (ADR-124 Inc.11).
///
/// 🚨 **Over the ceiling is a refusal, not a truncation**, for the reason `NODE_MOVE_BATCH_MAX`
/// gives: a truncated write would answer "suppressed" while leaving everything past the cut paging
/// someone, with no way to see which half. Deliberately the same number as the node batches — a
/// second ceiling would be a second answer to "how many is too many".
const SUPPRESSION_BATCH_MAX: usize = 1000;

/// Parse and sanity-check a window's bounds.
///
/// Shared by every creation path, including the MCP `open_maintenance` tool. A window that ends
/// before it begins is accepted by the database and suppresses nothing, so the operator believes
/// they are covered and is paged through the change anyway — which is exactly the failure the
/// maintenance feature exists to prevent.
pub(crate) fn window_bounds(
    starts_at: &str,
    ends_at: &str,
) -> Result<(DateTime<Utc>, DateTime<Utc>), ApiError> {
    let (Some(starts), Some(ends)) = (parse_rfc3339(starts_at), parse_rfc3339(ends_at)) else {
        return Err(ApiError::bad_request(
            "invalid_window",
            "starts_at/ends_at must be RFC 3339 timestamps",
        ));
    };
    check_order(starts, ends)?;
    Ok((starts, ends))
}

/// The ordering half of [`window_bounds`], for callers that already hold timestamps.
pub(crate) fn check_order(starts: DateTime<Utc>, ends: DateTime<Utc>) -> Result<(), ApiError> {
    if ends <= starts {
        return Err(ApiError::bad_request(
            "invalid_window",
            "ends_at must be after starts_at",
        ));
    }
    Ok(())
}

// ── Group scope over stored suppression rows (ADR-014) ───────────────────────

/// A window's target, as [`ScopeTarget`].
///
/// ⚠️ **Exhaustive over [`WindowScope`].** A window's folder group is `FolderGroup` (`group_id`). A
/// stored window row with the old `group` token (the label scope ADR-229 removed) reads back as
/// `Profile` and so stays unbounded here — a converter keyed on the string would read it as a
/// folder id.
fn window_target(w: &crate::alerts::maintenance::StoredWindow) -> ScopeTarget {
    use crate::alerts::maintenance::WindowScope;
    match w.level {
        WindowScope::Node => w
            .scope_id
            .parse::<Uuid>()
            .map_or(ScopeTarget::Unbounded, |n| {
                ScopeTarget::Node(yagra_common::NodeId::from(n))
            }),
        WindowScope::FolderGroup => w
            .scope_id
            .parse::<Uuid>()
            .map_or(ScopeTarget::Unbounded, ScopeTarget::Group),
        // A device class is an open-ended node set. `System` joins it for the same reason and the
        // strongest version of it: it names the whole fleet, so no group scope narrows it.
        WindowScope::Profile | WindowScope::System => ScopeTarget::Unbounded,
    }
}

/// For a scoped caller, confirm `id` names a window they can see; otherwise `404` — the same answer
/// they get for a window that does not exist, so the endpoint is not an id-enumeration oracle.
///
/// Reads the list rather than one row because [`crate::alerts::maintenance::MaintenanceRepo`] has no by-id
/// read, and windows are a table an operator populates by hand (tens of rows). An unrestricted
/// caller returns before the query, so the common path is unchanged.
async fn require_visible_window(
    st: &ApiState,
    scope: &super::scope::NodeScope,
    admin: &super::AdminState,
    id: Uuid,
) -> Result<(), ApiError> {
    if scope.is_all() {
        return Ok(());
    }
    let windows = admin.maintenance.list_windows().await.map_err(|e| {
        ApiError::from_internal(
            e.as_ref(),
            "resolve maintenance window scope",
            "failed to load maintenance windows",
        )
    })?;
    let visible = windows
        .iter()
        .any(|w| w.id == id && scope.allows_target(st, window_target(w)));
    if visible {
        Ok(())
    } else {
        Err(ApiError::not_found(
            "window_not_found",
            format!("no maintenance window {id}"),
        ))
    }
}

/// Confirm `scope_id` names an existing folder group.
///
/// A window pointing at a group that does not exist silences nothing — same failure mode as
/// a backwards window, so it is rejected rather than stored.
async fn validate_group_scope(admin: &super::AdminState, scope_id: &str) -> Result<(), ApiError> {
    let gid = Uuid::parse_str(scope_id.trim()).map_err(|_| {
        ApiError::bad_request(
            "invalid_scope",
            "scope_id must be a group UUID for a folder-group scope",
        )
    })?;
    let edges = admin.groups.edges().await.map_err(|e| {
        ApiError::from_internal(
            e.as_ref(),
            "validate group scope",
            "failed to validate group scope",
        )
    })?;
    if edges.iter().any(|(id, _)| *id == gid) {
        Ok(())
    } else {
        Err(ApiError::not_found(
            "group_not_found",
            format!("no group {gid}"),
        ))
    }
}

// ── Maintenance windows (planned; ManageMaintenance) ─────────────────────────

#[utoipa::path(
    get, path = "/api/v1/maintenance-windows", tag = "maintenance",
    responses(
        (status = 200, description = "Every maintenance window", body = Vec<crate::alerts::maintenance::StoredWindow>),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks the View permission", body = super::error::ErrorBody),
        (status = 503, description = "This core has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn list_maintenance_windows(
    _perm: RequireView,
    Scoped(scope): Scoped,
    State(st): State<ApiState>,
    admin: Admin,
) -> ApiResult<Json<Vec<crate::alerts::maintenance::StoredWindow>>> {
    Ok(Json(visible_windows(&st, &scope, &admin).await?))
}

/// The maintenance windows `scope` may see.
///
/// A window names its own target, so this filters on the row rather than on a query predicate — and
/// a profile-scoped window is hidden from a scoped caller entirely, matching the rule
/// [`create_maintenance_window`] enforces on the way in.
pub(crate) async fn visible_windows(
    st: &ApiState,
    scope: &super::scope::NodeScope,
    admin: &super::AdminState,
) -> Result<Vec<crate::alerts::maintenance::StoredWindow>, ApiError> {
    let windows = admin.maintenance.list_windows().await.map_err(|e| {
        ApiError::from_internal(
            e.as_ref(),
            "list maintenance windows",
            "failed to list maintenance windows",
        )
    })?;
    Ok(windows
        .into_iter()
        .filter(|w| scope.allows_target(st, window_target(w)))
        .collect())
}

/// Create-window body. Times are RFC 3339; `scope_level` is `node`, `profile` or `group_id` (a
/// folder-group UUID, resolved recursively — the All Nodes right-click scope).
#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct CreateWindow {
    name: String,
    scope_level: String,
    scope_id: String,
    starts_at: String,
    ends_at: String,
    /// The IANA zone the operator typed the times in (ADR-261), recorded so the list can show the
    /// requester's clock. It does not change when the window runs — `starts_at` / `ends_at` are
    /// the instants. Omit (or send empty) to record none.
    #[serde(default)]
    time_zone: Option<String>,
}

#[utoipa::path(
    post, path = "/api/v1/maintenance-windows", tag = "maintenance",
    request_body = CreateWindow,
    responses(
        (status = 201, description = "Window created", body = CreatedId),
        (status = 400, description = "Empty name/scope_id, an unknown scope_level, bounds that are not RFC 3339 or end at or before they start, or a time_zone that is not an IANA zone name", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks the ManageMaintenance permission", body = super::error::ErrorBody),
        (status = 404, description = "A group_id scope naming a group that does not exist", body = super::error::ErrorBody),
        (status = 503, description = "This core has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn create_maintenance_window(
    _perm: RequireManageMaintenance,
    Scoped(scope): Scoped,
    State(st): State<ApiState>,
    admin: Admin,
    Json(body): Json<CreateWindow>,
) -> ApiResult<(StatusCode, Json<CreatedId>)> {
    if body.name.trim().is_empty()
        || body.scope_id.trim().is_empty()
        || !WINDOW_SCOPE_LEVELS.contains(&body.scope_level.as_str())
    {
        return Err(ApiError::bad_request(
            "invalid_window",
            format!(
                "name/scope_id must not be empty; scope_level must be {}",
                WINDOW_SCOPE_LEVELS.join("|")
            ),
        ));
    }
    if body.scope_level == "group_id" {
        validate_group_scope(&admin, &body.scope_id).await?;
    }
    // A window names its target in the body, and `ManageMaintenance` is an operator permission, so
    // a scoped caller must not open a window over something outside their scope. The `profile` level names a
    // *class* of node rather than one — it is refused outright for a scoped caller, because there
    // is no way to open one that is honestly bounded by their scope.
    if !scope.is_all() {
        match body.scope_level.as_str() {
            "node" => {
                let node = body.scope_id.parse::<Uuid>().map_err(|_| {
                    ApiError::bad_request("invalid_window", "scope_id must be a node uuid")
                })?;
                super::scope::require_visible_node(&st, &scope, yagra_common::NodeId::from(node))?;
            }
            "group_id" => {
                let gid = body.scope_id.parse::<Uuid>().map_err(|_| {
                    ApiError::bad_request("invalid_window", "scope_id must be a group uuid")
                })?;
                super::scope::require_visible_group(&scope, gid)?;
            }
            // Reached by `profile`, which names a *class* of node with no honestly-bounded version
            // for a scoped caller.
            //
            // The wildcard stays because `scope_level` is a `String`, but it is not the guard: a
            // fourth `WINDOW_SCOPE_LEVELS` entry landing here would not leak anything — refusing is
            // the safe direction — it would silently become un-openable for every scoped account
            // while this message claimed a reason that was not the real one.
            // `the_scope_check_decides_every_window_scope_level` fails the build instead.
            _ => {
                return Err(ApiError::forbidden_code(
                    "scope_unsupported",
                    "a group-scoped account can open a maintenance window over a node or a folder \
                     group, not over a profile",
                ))
            }
        }
    }
    let (starts, ends) = window_bounds(&body.starts_at, &body.ends_at)?;
    let zone = window_zone(body.time_zone.as_deref())?;
    let id = admin
        .maintenance
        .create_window_in(
            body.name.trim(),
            &body.scope_level,
            body.scope_id.trim(),
            starts,
            ends,
            zone.as_deref(),
        )
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "create maintenance window",
                "failed to create maintenance window",
            )
        })?;
    Ok((StatusCode::CREATED, Json(CreatedId { id })))
}

/// Open one window over each of many nodes.
#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct BulkWindow {
    node_ids: Vec<Uuid>,
    /// The name every window gets. `{node}` in it is replaced by each window's own node name, so
    /// one request can name each window after its node (`Maintenance — {node}`) instead of after
    /// the batch, which read as nonsense on any one window afterwards (ADR-261 decision 26).
    name: String,
    starts_at: String,
    ends_at: String,
    /// The IANA zone the times were typed in (ADR-261); recorded on every window opened.
    #[serde(default)]
    time_zone: Option<String>,
}

/// The zone a creation body names, canonicalised — or `None` when it names none. A name this
/// build does not know is refused rather than stored, because the list would then show a clock
/// nobody can read the times against.
fn window_zone(name: Option<&str>) -> Result<Option<String>, ApiError> {
    Ok(super::util::parse_time_zone(name)?
        .map(|tz| yagra_base::wall_clock::zone_name(&tz).to_owned()))
}

/// What a bulk maintenance write actually did.
#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct BulkSuppressionResult {
    /// Distinct node ids the request named, after de-duplication.
    requested: usize,
    /// Rows actually written. **Lower than `requested` is normal**: an id can name a node that has
    /// since been deleted, or one outside the caller's scope. The two are not distinguished.
    created: u64,
}

/// Open a maintenance window over each of many nodes at once (ADR-124 Inc.11).
///
/// "This dozen, tonight" rarely follows a folder boundary, so the folder-scoped window cannot
/// express it and the single-node form meant one request each.
///
/// **One window per node rather than one window naming many**: a window's scope is a single id
/// (migration 0011), and widening that is a schema change the alert engine, the suppression index
/// and `list_suppressions` all read.
///
/// ⚠️ **Only the `node` level.** The `profile` level already reaches many nodes by naming a class, and the folder level reaches a subtree — a batch of ids is the case none of them
/// covers. The scope check is therefore the node one, applied by the store's `JOIN nodes` rather
/// than a pre-check, so a node deleted between the check and the write cannot get a window.
#[utoipa::path(
    post, path = "/api/v1/maintenance-windows/bulk", tag = "maintenance",
    request_body = BulkWindow,
    responses(
        (status = 200, description = "How many windows were opened", body = BulkSuppressionResult),
        (status = 400, description = "Empty name, unparseable or backwards bounds, an unknown time_zone, or more ids than one request may carry", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageMaintenance", body = super::error::ErrorBody),
        (status = 503, description = "This core has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn create_maintenance_windows_bulk(
    _perm: RequireManageMaintenance,
    Scoped(scope): Scoped,
    admin: Admin,
    Json(body): Json<BulkWindow>,
) -> ApiResult<Json<BulkSuppressionResult>> {
    if body.node_ids.len() > SUPPRESSION_BATCH_MAX {
        return Err(ApiError::bad_request(
            "too_many_nodes",
            format!(
                "at most {SUPPRESSION_BATCH_MAX} nodes may be suppressed in one request, got {}",
                body.node_ids.len()
            ),
        ));
    }
    if body.name.trim().is_empty() {
        return Err(ApiError::bad_request(
            "invalid_window",
            "name must not be empty",
        ));
    }
    // The same bounds check every creation path makes, including the MCP tool: a window that ends
    // before it begins suppresses nothing while reading as success.
    let (starts, ends) = window_bounds(&body.starts_at, &body.ends_at)?;
    let zone = window_zone(body.time_zone.as_deref())?;
    let (requested, created) = admin
        .maintenance
        .create_windows_for_nodes(
            &body.node_ids,
            body.name.trim(),
            starts,
            ends,
            zone.as_deref(),
            scope.group_filter(),
        )
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "create maintenance windows",
                "failed to create maintenance windows",
            )
        })?;
    // The audit middleware records method and path only, so without this the log says that someone
    // suppressed something and never how much (the bulk move and bulk clear do the same).
    tracing::info!(requested, created, "bulk maintenance window");
    Ok(Json(BulkSuppressionResult { requested, created }))
}

#[utoipa::path(
    put, path = "/api/v1/maintenance-windows/{id}", tag = "maintenance",
    params(("id" = Uuid, Path, description = "Maintenance window id")),
    request_body = super::util::EnabledBody,
    responses(
        (status = 204, description = "Window enabled or disabled"),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks the ManageMaintenance permission", body = super::error::ErrorBody),
        (status = 404, description = "No such maintenance window", body = super::error::ErrorBody),
        (status = 503, description = "This core has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn set_maintenance_window_enabled(
    _perm: RequireManageMaintenance,
    Scoped(scope): Scoped,
    State(st): State<ApiState>,
    admin: Admin,
    Path(id): Path<Uuid>,
    Json(body): Json<super::util::EnabledBody>,
) -> ApiResult<StatusCode> {
    // Disabling someone else's window is not a small thing: it un-suppresses their change and pages
    // them. Same 404 as an absent window, so the id space stays opaque.
    require_visible_window(&st, &scope, &admin, id).await?;
    let found = admin
        .maintenance
        .set_window_enabled(id, body.enabled)
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "update maintenance window",
                "failed to update maintenance window",
            )
        })?;
    if found {
        // Disabling is the third way coverage stops early (after ending and deleting), so a
        // release carved out of this window must be re-derived too. Enabling one cannot need it:
        // the reconcile never extends a release.
        if !body.enabled {
            reconcile_after_coverage_change(&admin).await;
        }
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found(
            "window_not_found",
            format!("no maintenance window {id}"),
        ))
    }
}

#[utoipa::path(
    delete, path = "/api/v1/maintenance-windows/{id}", tag = "maintenance",
    params(("id" = Uuid, Path, description = "Maintenance window id")),
    responses(
        (status = 204, description = "Window deleted"),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks the ManageMaintenance permission", body = super::error::ErrorBody),
        (status = 404, description = "No such maintenance window", body = super::error::ErrorBody),
        (status = 503, description = "This core has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn delete_maintenance_window(
    _perm: RequireManageMaintenance,
    Scoped(scope): Scoped,
    State(st): State<ApiState>,
    admin: Admin,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    require_visible_window(&st, &scope, &admin, id).await?;
    let found = admin.maintenance.delete_window(id).await.map_err(|e| {
        ApiError::from_internal(
            e.as_ref(),
            "delete maintenance window",
            "failed to delete maintenance window",
        )
    })?;
    if found {
        reconcile_after_coverage_change(&admin).await;
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found(
            "window_not_found",
            format!("no maintenance window {id}"),
        ))
    }
}

#[utoipa::path(
    post, path = "/api/v1/maintenance-windows/{id}/end", tag = "maintenance",
    params(("id" = Uuid, Path, description = "Maintenance window id")),
    responses(
        (status = 204, description = "Window ended; the row stays as a record of the maintenance that happened"),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks the ManageMaintenance permission", body = super::error::ErrorBody),
        (status = 404, description = "No such maintenance window, or it is not currently active (disabled, already over, or not yet started)", body = super::error::ErrorBody),
        (status = 503, description = "This core has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn end_maintenance_window(
    _perm: RequireManageMaintenance,
    Scoped(scope): Scoped,
    State(st): State<ApiState>,
    admin: Admin,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    // Ending a window makes the covered nodes page again, exactly as deleting it would, so it is
    // gated the same way. The 404 is the same answer a nonexistent id gets, so this is not an
    // id-enumeration oracle for a scoped caller.
    require_visible_window(&st, &scope, &admin, id).await?;
    let ended = admin.maintenance.end_window_now(id).await.map_err(|e| {
        ApiError::from_internal(
            e.as_ref(),
            "end maintenance window",
            "failed to end maintenance window",
        )
    })?;
    if ended {
        // The window stops here rather than at its stated end, so any release carved out of it is
        // now sized to a time that will never arrive. This is the path the bug was found on.
        reconcile_after_coverage_change(&admin).await;
        Ok(StatusCode::NO_CONTENT)
    } else {
        // Deliberately the same code the delete path uses for a missing id. "Visible but not
        // active" is a race — someone else ended it, or it expired between the page load and the
        // click — and the honest answer to "release this suppression" in that case is that there
        // is no longer one to release, not that the window is gone.
        Err(ApiError::not_found(
            "window_not_found",
            format!("no active maintenance window {id}"),
        ))
    }
}

/// The one value `status` may carry on a bulk clear.
const CLEARABLE_STATUS: &str = "ended";

/// Reject a bulk clear that does not say what it is clearing.
///
/// ⚠️ A bare `DELETE` on a collection reads, to any client, as "delete the collection" — and this
/// collection is what stops an operator being paged through planned work. Requiring an explicit
/// discriminator means such a caller gets a 400 naming what this endpoint actually does, rather
/// than quietly getting something narrower and believing they got the whole thing. It also leaves
/// room for a second status later without any existing caller's meaning changing under it.
///
/// No case folding and no synonyms: parse at the edge, do not guess (`api-conventions.md`).
fn require_clearable_status(status: Option<&str>) -> Result<(), ApiError> {
    if status.map(str::trim) == Some(CLEARABLE_STATUS) {
        return Ok(());
    }
    Err(ApiError::bad_request(
        "invalid_status",
        format!("status must be `{CLEARABLE_STATUS}`"),
    ))
}

/// Which windows a bulk clear removes.
#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub(super) struct ClearQuery {
    /// Must be `ended`. Required: a request without it is refused rather than read as "all".
    status: Option<String>,
}

/// The outcome of a bulk clear.
#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct ClearedWindows {
    /// How many windows were deleted. May be lower than a client expected: the server's clock
    /// decides what has ended, and a group-scoped caller clears only what it can see.
    deleted: u64,
}

#[utoipa::path(
    delete, path = "/api/v1/maintenance-windows", tag = "maintenance",
    params(ClearQuery),
    responses(
        (status = 200, description = "How many ended windows were deleted", body = ClearedWindows),
        (status = 400, description = "`status` missing or not `ended`", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks the ManageMaintenance permission", body = super::error::ErrorBody),
        (status = 503, description = "This core has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn clear_ended_maintenance_windows(
    _perm: RequireManageMaintenance,
    // ⚠️ `Scoped` must stay the second parameter: `route_table`'s
    // `every_filtered_route_takes_the_scope_extractor` reads the signature only as far as the first
    // `)`, so a `Query(…)` ahead of it would truncate what that test can see.
    Scoped(scope): Scoped,
    State(st): State<ApiState>,
    admin: Admin,
    Query(q): Query<ClearQuery>,
) -> ApiResult<Json<ClearedWindows>> {
    require_clearable_status(q.status.as_deref())?;
    // The same seam the GET and the MCP tool read, so this can never remove a window the caller
    // cannot see. The *ended* half is applied again by the database — see `delete_ended_windows`.
    let ids: Vec<Uuid> = visible_windows(&st, &scope, &admin)
        .await?
        .iter()
        .map(|w| w.id)
        .collect();
    let deleted = admin
        .maintenance
        .delete_ended_windows(&ids)
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "clear ended maintenance windows",
                "failed to clear ended maintenance windows",
            )
        })?;
    // `audit_mw` records the path only — no query string, no body — so the audit row says this was
    // called and not what it did. Same accepted trade as `config_bundle::import_bundle`; the count
    // lives here so it is at least recoverable from the logs.
    tracing::info!(deleted, "ended maintenance windows cleared");
    Ok(Json(ClearedWindows { deleted }))
}

// ── Exemptions: releasing one node from a suppression it inherited (migration 0081) ──────────
//
// A window that *names* a node can simply be ended or deleted, and the inventory tree does exactly
// that. Coverage a node merely inherits — from its folder group, its profile, or the fleet-wide
// upgrade window — cannot be, because the row belongs to everyone else
// under it too. An exemption is the carve-out, and it exists only for that case.
//
// Two rules make it safe to hand to an operator, and both are enforced here rather than in the
// browser:
//
//  1. **It never cancels coverage that names the node.** `crate::alerts::maintenance::scope_covers`'
//     `WindowScope::Node` arm is excluded below, so an operator can
//     still put a released node into maintenance deliberately.
//  2. **It expires with the coverage it was carved out of.** The expiry is computed *here* from
//     what is actually in force, never supplied by the client — an exemption that outlived its
//     window would silently exclude the node from the next one, which is the monitoring blind spot
//     this feature must not create. Computing it once is not enough, because coverage can stop
//     sooner than it said it would: ending, disabling or deleting a window
//     each re-derive every release through `crate::alerts::maintenance::reconcile_exemptions`, which is
//     also run on the refresh cycle so no future path can reopen that hole by forgetting to call
//     it.

/// Resolve one node's coverage-relevant facts. `404` if the node is gone.
///
/// The facts and the rules over them live in [`crate::alerts::maintenance`] — three callers need them
/// (this grant, the alert engine's refresh, and the reconcile that keeps a release from outliving
/// its coverage), so they must not live inside any one of the three (`api-conventions.md`).
async fn coverage_facts(
    admin: &super::AdminState,
    node_id: Uuid,
) -> Result<CoverageFacts, ApiError> {
    let node = admin
        .repo
        .get_node(node_id)
        .await
        .map_err(|e| {
            ApiError::from_internal(e.as_ref(), "load node for release", "failed to load node")
        })?
        .ok_or_else(|| ApiError::not_found("node_not_found", format!("no node {node_id}")))?;
    let edges = admin.groups.edges().await.map_err(|e| {
        ApiError::from_internal(
            e.as_ref(),
            "load group edges for release",
            "failed to load groups",
        )
    })?;
    Ok(CoverageFacts::of(&node, &edges))
}

/// Re-derive every release against the coverage that is left, after this request removed some.
///
/// Best-effort on purpose: the window is already gone, so failing the request would report
/// a change that happened as one that did not. The 30s refresh cycle runs the same reconcile, so
/// the worst case of a failure here is that the operator's markers lag by one cycle. Called inline
/// anyway because the screen that just ended the window is the screen looking at those markers.
async fn reconcile_after_coverage_change(admin: &super::AdminState) {
    if let Err(e) = crate::alerts::maintenance::reconcile_exemptions(
        &admin.maintenance,
        &admin.groups,
        &admin.repo,
    )
    .await
    {
        tracing::warn!(error = %e, "failed to reconcile suppression exemptions");
    }
}

/// The 400 for "there is nothing here to release".
///
/// Deliberately not a silent 204. A release that stored nothing would leave the operator believing
/// the node was out of the window; and the likeliest way to reach it is a node covered only by a
/// window that *names* it, where the honest instruction is to end that window instead.
fn nothing_inherited(kind: &str) -> ApiError {
    ApiError::bad_request(
        "not_suppressed",
        format!(
            "this node inherits no {kind} suppression right now; a window naming the node is \
             ended directly instead"
        ),
    )
}

/// Whether a node is released from the suppression it inherits.
#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct ExemptionBody {
    /// `true` releases the node until the coverage it was carved out of ends. `false` puts it
    /// back, and is accepted whether or not it was released — this is a state, not a toggle.
    exempt: bool,
}

#[utoipa::path(
    put, path = "/api/v1/nodes/{node_id}/maintenance-exemption", tag = "maintenance",
    params(("node_id" = Uuid, Path, description = "Node id")),
    request_body = ExemptionBody,
    responses(
        (status = 204, description = "Node released from, or returned to, inherited maintenance"),
        (status = 400, description = "The node inherits no maintenance right now (`not_suppressed`)", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks the ManageMaintenance permission", body = super::error::ErrorBody),
        (status = 404, description = "No such node", body = super::error::ErrorBody),
        (status = 503, description = "This core has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn set_node_maintenance_exemption(
    _perm: RequireManageMaintenance,
    _visible: VisibleNode,
    admin: Admin,
    Path(node_id): Path<Uuid>,
    Json(body): Json<ExemptionBody>,
) -> ApiResult<StatusCode> {
    use crate::alerts::maintenance::ExemptionKind;
    if !body.exempt {
        admin
            .maintenance
            .clear_exemption(ExemptionKind::Maintenance, node_id)
            .await
            .map_err(|e| {
                ApiError::from_internal(
                    e.as_ref(),
                    "clear maintenance exemption",
                    "failed to return the node to maintenance",
                )
            })?;
        return Ok(StatusCode::NO_CONTENT);
    }
    let facts = coverage_facts(&admin, node_id).await?;
    let windows = admin
        .maintenance
        .windows_a_release_answers_to()
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "load active windows for release",
                "failed to load maintenance windows",
            )
        })?;
    let until = inherited_maintenance_end(&windows, &facts)
        .ok_or_else(|| nothing_inherited("maintenance"))?;
    admin
        .maintenance
        .set_exemption(ExemptionKind::Maintenance, node_id, until)
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "set maintenance exemption",
                "failed to release the node from maintenance",
            )
        })?;
    Ok(StatusCode::NO_CONTENT)
}

/// Which nodes to take out of maintenance now.
#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct ReleaseBody {
    node_ids: Vec<Uuid>,
}

/// What taking nodes out of maintenance did.
#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct ReleaseResult {
    /// Distinct nodes the request named that the caller may see. A node deleted since the page
    /// loaded, or one outside the caller's scope, is not counted - the two are not distinguished.
    requested: usize,
    /// Windows naming one of those nodes that were ended (or, starting within the minute,
    /// cancelled).
    ended: u64,
    /// Nodes carved out of a folder, profile or fleet-wide window they inherit.
    released: usize,
}

/// Take nodes out of maintenance now, whatever covers them (ADR-261 Inc.4).
///
/// One request does what the release panel used to ask for window by window: every window that
/// names a node ends, and a node covered by a window it inherits is carved out of it until that
/// window ends - both in one transaction. A node with nothing covering it is accepted and counted
/// in `requested` only: the operator's intent ("not in maintenance") already holds.
///
/// It replaces nothing on the wire: ending one window and releasing one node keep their routes.
#[utoipa::path(
    post, path = "/api/v1/maintenance/release", tag = "maintenance",
    request_body = ReleaseBody,
    responses(
        (status = 200, description = "What was ended and released", body = ReleaseResult),
        (status = 400, description = "More ids than one request may carry", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageMaintenance", body = super::error::ErrorBody),
        (status = 503, description = "This core has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn release_nodes_from_maintenance(
    _perm: RequireManageMaintenance,
    Scoped(scope): Scoped,
    admin: Admin,
    Json(body): Json<ReleaseBody>,
) -> ApiResult<Json<ReleaseResult>> {
    if body.node_ids.len() > SUPPRESSION_BATCH_MAX {
        return Err(ApiError::bad_request(
            "too_many_nodes",
            format!(
                "at most {SUPPRESSION_BATCH_MAX} nodes may be released in one request, got {}",
                body.node_ids.len()
            ),
        ));
    }
    let internal = |what: &'static str| {
        move |e: anyhow::Error| {
            ApiError::from_internal(e.as_ref(), what, "failed to take nodes out of maintenance")
        }
    };
    let nodes = admin
        .maintenance
        .visible_node_ids(&body.node_ids, scope.group_filter())
        .await
        .map_err(internal("resolve nodes to release"))?;
    // Inherited coverage does not depend on the windows that name a node, so it can be read before
    // those are ended - and must be, for the release to be written in the same transaction.
    let windows = admin
        .maintenance
        .windows_a_release_answers_to()
        .await
        .map_err(internal("load active windows for release"))?;
    let edges = admin
        .groups
        .edges()
        .await
        .map_err(internal("load group edges for release"))?;
    // One read for the whole batch: a node at a time was up to `SUPPRESSION_BATCH_MAX` serial
    // round trips before the transaction. A node deleted since `visible_node_ids` is simply absent.
    let mut releases = Vec::new();
    for node in admin
        .repo
        .nodes_by_ids(&nodes, scope.group_filter())
        .await
        .map_err(internal("load nodes for release"))?
    {
        let facts = CoverageFacts::of(&node, &edges);
        if let Some(until) = inherited_maintenance_end(&windows, &facts) {
            releases.push((node.id.as_uuid(), until));
        }
    }
    let ended = admin
        .maintenance
        .release_nodes_now(&nodes, &releases)
        .await
        .map_err(internal("release nodes from maintenance"))?;
    if ended > 0 {
        // Ending a window can shorten a release carved out of it elsewhere.
        reconcile_after_coverage_change(&admin).await;
    }
    // The audit middleware records method and path only; say how much this did.
    tracing::info!(
        requested = nodes.len(),
        ended,
        released = releases.len(),
        "nodes taken out of maintenance"
    );
    Ok(Json(ReleaseResult {
        requested: nodes.len(),
        ended,
        released: releases.len(),
    }))
}

#[utoipa::path(
    get, path = "/api/v1/suppression-exemptions", tag = "maintenance",
    responses(
        (status = 200, description = "Every unexpired exemption", body = Vec<crate::alerts::maintenance::StoredExemption>),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks the View permission", body = super::error::ErrorBody),
        (status = 503, description = "This core has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn list_suppression_exemptions(
    _perm: RequireView,
    Scoped(scope): Scoped,
    State(st): State<ApiState>,
    admin: Admin,
) -> ApiResult<Json<Vec<crate::alerts::maintenance::StoredExemption>>> {
    Ok(Json(visible_exemptions(&st, &scope, &admin).await?))
}

/// The exemptions `scope` may see. Each row names one node, so this is the same post-filter the
/// window list uses — and the seam the MCP tool reads, so neither surface can show a row
/// the other would hide.
pub(crate) async fn visible_exemptions(
    st: &ApiState,
    scope: &super::scope::NodeScope,
    admin: &super::AdminState,
) -> Result<Vec<crate::alerts::maintenance::StoredExemption>, ApiError> {
    let rows = admin.maintenance.list_exemptions().await.map_err(|e| {
        ApiError::from_internal(
            e.as_ref(),
            "list suppression exemptions",
            "failed to list suppression exemptions",
        )
    })?;
    Ok(rows
        .into_iter()
        .filter(|e| scope.allows_node(st, yagra_common::NodeId::from(e.node_id)))
        .collect())
}

#[cfg(test)]
mod tests {
    /// Every scope level a window may be created at has a decision in the group-scope check.
    ///
    /// That check matches on a `String`, so it ends in a wildcard that cannot be removed — and a
    /// wildcard over a token list is the shape this repo keeps paying for. Pinning the list here
    /// is the substitute: adding a fifth level fails this test, which is the moment to decide
    /// whether a scoped caller may open one, rather than discovering months later that they
    /// silently cannot.
    ///
    /// ⚠️ Do not "fix" a failure by editing the array below. Add the arm first.
    #[test]
    fn the_scope_check_decides_every_window_scope_level() {
        // The two the check resolves to a concrete target, and the two it refuses outright.
        let resolved = ["node", "group_id"];
        let refused = ["profile"];
        let mut decided: Vec<&str> = resolved.iter().chain(refused.iter()).copied().collect();
        decided.sort_unstable();
        let mut levels = super::WINDOW_SCOPE_LEVELS.to_vec();
        levels.sort_unstable();
        assert_eq!(
            levels, decided,
            "a window scope level exists that the group-scope check has no arm for"
        );
    }

    /// The creatable levels are every stored window scope but `System`, spelt as the stored
    /// token. The two lists are written apart, so a scope added to the enum (or removed from it,
    /// as the label level was in ADR-229) must be answered here as well.
    #[test]
    fn the_creatable_levels_are_every_window_scope_but_system() {
        use crate::alerts::maintenance::WindowScope;
        let mut from_enum: Vec<&str> = WindowScope::ALL
            .iter()
            .filter(|s| !matches!(s, WindowScope::System))
            .map(|s| s.as_str())
            .collect();
        from_enum.sort_unstable();
        let mut listed = super::WINDOW_SCOPE_LEVELS.to_vec();
        listed.sort_unstable();
        assert_eq!(listed, from_enum);
    }

    use super::*;
    use crate::alerts::maintenance::StoredWindow;
    use crate::api::tests_support::{private_state, public_state, status_with};
    use yagra_common::{Principal, Role, Scope};

    #[test]
    fn a_backwards_window_is_rejected_rather_than_silently_suppressing_nothing() {
        // The important half: the DB would accept it, the window would match no time, and the
        // operator would be paged through the change believing they were covered.
        let err = window_bounds("2026-07-28T10:00:00Z", "2026-07-28T09:00:00Z")
            .expect_err("ends before starts must reject");
        assert_eq!(err.code(), "invalid_window");
        assert!(err.message().contains("after"), "{}", err.message());

        // Zero-length is the same failure, so it is rejected too.
        assert!(window_bounds("2026-07-28T10:00:00Z", "2026-07-28T10:00:00Z").is_err());
        assert!(window_bounds("2026-07-28T09:00:00Z", "2026-07-28T10:00:00Z").is_ok());
    }

    #[test]
    fn window_bounds_applies_the_offset_rather_than_dropping_it() {
        // A JST operator scheduling 22:00–23:00 local must not get a window at 22:00 UTC.
        let (starts, ends) =
            window_bounds("2026-07-28T22:00:00+09:00", "2026-07-28T23:00:00+09:00")
                .expect("offsets are valid RFC 3339");
        assert_eq!(starts.to_rfc3339(), "2026-07-28T13:00:00+00:00");
        assert_eq!(ends.to_rfc3339(), "2026-07-28T14:00:00+00:00");
    }

    #[test]
    fn a_bulk_clear_must_say_what_it_is_clearing() {
        // The failure this guards is not a typo — it is a client issuing `DELETE` on the collection
        // meaning "delete them all" and getting a silent partial. Tested as a pure function because
        // the router cannot reach it: extractors run in argument order and `Admin` answers 503 in
        // skeleton mode long before the query is parsed.
        for bad in [None, Some(""), Some("  "), Some("active"), Some("ENDED")] {
            let Err(err) = require_clearable_status(bad) else {
                panic!("{bad:?} must be refused");
            };
            assert_eq!(err.code(), "invalid_status", "{bad:?}");
        }
        assert!(require_clearable_status(Some("ended")).is_ok());
        assert!(require_clearable_status(Some(" ended ")).is_ok());
    }

    #[test]
    fn unparseable_bounds_are_rejected_not_defaulted() {
        for (s, e) in [
            ("not-a-time", "2026-07-28T10:00:00Z"),
            ("2026-07-28T09:00:00Z", "soon"),
            ("2026-07-28", "2026-07-29"), // dates carry no instant
        ] {
            assert_eq!(
                window_bounds(s, e).expect_err("must reject").code(),
                "invalid_window"
            );
        }
    }

    // ── Group scope over stored rows ────────────────────────────────────────

    fn window(level: crate::alerts::maintenance::WindowScope, scope_id: &str) -> StoredWindow {
        StoredWindow {
            id: Uuid::new_v4(),
            name: "w".to_owned(),
            level,
            scope_id: scope_id.to_owned(),
            starts_at: "2026-08-01T00:00:00Z".to_owned(),
            ends_at: "2026-08-02T00:00:00Z".to_owned(),
            enabled: true,
            active: true,
            time_zone: None,
        }
    }

    #[test]
    fn a_windows_old_label_row_is_never_read_as_a_folder_group() {
        // A window row stored with the `group` token (the label scope ADR-229 removed) holds a
        // label. A helper keyed on the text would parse the label as a folder id and hand a scoped
        // caller a window it cannot see.
        use crate::alerts::maintenance::WindowScope;
        let g = Uuid::from_u128(42);
        assert_eq!(
            window_target(&window(WindowScope::from_stored("group"), &g.to_string())),
            ScopeTarget::Unbounded,
            "an old label window is not a folder group"
        );

        // And the folder-group case a window does have is spelled differently.
        assert_eq!(
            window_target(&window(WindowScope::FolderGroup, &g.to_string())),
            ScopeTarget::Group(g)
        );
        assert_eq!(
            window_target(&window(WindowScope::Profile, &g.to_string())),
            ScopeTarget::Unbounded
        );
    }

    #[test]
    fn an_unparseable_or_missing_target_is_a_class_not_a_node() {
        // Fail-closed: a row whose target cannot be read is treated as unbounded, so only an
        // unrestricted caller sees it. The inversion — defaulting to some node id — would expose
        // the row to whoever happened to hold that node.
        use crate::alerts::maintenance::WindowScope;
        assert_eq!(
            window_target(&window(WindowScope::Node, "not-a-uuid")),
            ScopeTarget::Unbounded
        );
    }

    #[test]
    fn a_class_scoped_row_is_visible_only_to_an_unrestricted_caller() {
        use crate::api::scope::{NodeScope, ScopeSet};
        use std::sync::Arc;
        let st = public_state();
        let g = Uuid::from_u128(7);
        let scoped = NodeScope::Groups(Arc::new(ScopeSet {
            visible: vec![g],
            breadcrumb: Vec::new(),
        }));

        assert!(NodeScope::All.allows_target(&st, ScopeTarget::Unbounded));
        assert!(
            !scoped.allows_target(&st, ScopeTarget::Unbounded),
            "a profile/tag-scoped row covers an open-ended node set; no group scope contains it"
        );
        assert!(scoped.allows_target(&st, ScopeTarget::Group(g)));
        assert!(!scoped.allows_target(&st, ScopeTarget::Group(Uuid::from_u128(8))));
    }

    async fn status_of(st: ApiState, method: &str, path: &str, token: Option<&str>) -> StatusCode {
        let body = (method == "POST").then_some("{}");
        status_with(st, method, path, token, body).await
    }

    #[tokio::test]
    async fn suppression_writes_stay_closed_on_a_public_dashboard() {
        // Silencing alerts is exactly what an anonymous visitor must not be able to do, however
        // open the dashboard's reads are.
        for path in ["/api/v1/maintenance-windows"] {
            assert_eq!(
                status_of(public_state(), "POST", path, None).await,
                StatusCode::UNAUTHORIZED,
                "{path}"
            );
        }
        // The bulk clear is a write too, and it is gated *before* the query is looked at — an
        // anonymous caller learns it is anonymous, never whether `status=ended` was acceptable.
        assert_eq!(
            status_of(
                public_state(),
                "DELETE",
                "/api/v1/maintenance-windows?status=ended",
                None
            )
            .await,
            StatusCode::UNAUTHORIZED
        );
        // Ending a window early makes the fleet page again. That is a write however it is spelled,
        // and reachable from a single click in the inventory tree, so it is closed here too.
        assert_eq!(
            status_of(public_state(), "POST", &end_path(), None).await,
            StatusCode::UNAUTHORIZED
        );
    }

    /// A concrete `/end` path. The id never resolves in these tests — the guards answer first,
    /// which is the property under test.
    fn end_path() -> String {
        format!("/api/v1/maintenance-windows/{}/end", Uuid::from_u128(9))
    }

    // ── Exemptions ──────────────────────────────────────────────────────────
    //
    // What may be released from, and for how long, is `crate::alerts::maintenance`'s to answer and is
    // tested there — three callers share those rules now. What is tested here is the edge: the
    // refusal, the guards, and the paths.

    #[test]
    fn nothing_inherited_is_a_refusal_not_an_empty_release() {
        // A node covered only by a window that *names* it has nothing to be released from, and the
        // honest answer is the 400 that says to end that window instead — storing an exemption
        // would leave the operator believing the node was out of the window when it is not. The
        // half that decides there is nothing to release is
        // `maintenance::a_window_that_only_names_the_node_leaves_nothing_to_release`.
        assert_eq!(nothing_inherited("maintenance").code(), "not_suppressed");
    }

    /// The exemption path, and the node path it hangs off.
    fn exemption_paths() -> [String; 1] {
        let n = Uuid::from_u128(11);
        [format!("/api/v1/nodes/{n}/maintenance-exemption")]
    }

    #[tokio::test]
    async fn releasing_a_node_is_a_write_and_is_gated_like_one() {
        // Releasing a node makes it page again during planned work, so it is closed to an
        // anonymous caller on a public dashboard exactly as opening a suppression is.
        for path in exemption_paths() {
            assert_eq!(
                status_of(public_state(), "PUT", &path, None).await,
                StatusCode::UNAUTHORIZED,
                "{path}"
            );
        }

        let st = private_state();
        let viewer = st.sessions.issue(
            Uuid::new_v4(),
            Principal::new(Role::Viewer, Scope::All),
            "viewer2",
        );
        for path in exemption_paths() {
            assert_eq!(
                status_of(st.clone(), "PUT", &path, Some(&viewer)).await,
                StatusCode::FORBIDDEN,
                "{path}"
            );
        }

        // An operator holds `ManageMaintenance`, so the endpoint gets past the permission and stops
        // at availability — which also pins the extractor order.
        let op = st.sessions.issue(
            Uuid::new_v4(),
            Principal::new(Role::Operator, Scope::All),
            "op2",
        );
        for path in exemption_paths() {
            assert_eq!(
                status_of(st.clone(), "PUT", &path, Some(&op)).await,
                StatusCode::SERVICE_UNAVAILABLE,
                "{path}"
            );
        }
    }

    #[tokio::test]
    async fn the_exemption_list_is_a_read_and_is_gated_like_one() {
        let st = private_state();
        assert_eq!(
            status_of(st.clone(), "GET", "/api/v1/suppression-exemptions", None).await,
            StatusCode::UNAUTHORIZED
        );
        let viewer = st.sessions.issue(
            Uuid::new_v4(),
            Principal::new(Role::Viewer, Scope::All),
            "viewer3",
        );
        // A viewer may read it — knowing a node has been released is part of reading the fleet's
        // suppression state, and it stops at availability rather than at the permission.
        assert_eq!(
            status_of(st, "GET", "/api/v1/suppression-exemptions", Some(&viewer)).await,
            StatusCode::SERVICE_UNAVAILABLE
        );
    }

    #[tokio::test]
    async fn a_viewer_can_neither_open_a_suppression_nor_clear_one() {
        let st = private_state();
        let token = st.sessions.issue(
            Uuid::new_v4(),
            Principal::new(Role::Viewer, Scope::All),
            "viewer1",
        );
        for path in ["/api/v1/maintenance-windows"] {
            assert_eq!(
                status_of(st.clone(), "POST", path, Some(&token)).await,
                StatusCode::FORBIDDEN,
                "{path}"
            );
        }
        // 403, not 401 — the two must stay distinguishable, and a viewer clearing the fleet's
        // maintenance history would be a write dressed as a tidy-up.
        assert_eq!(
            status_of(
                st.clone(),
                "DELETE",
                "/api/v1/maintenance-windows?status=ended",
                Some(&token)
            )
            .await,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            status_of(st, "POST", &end_path(), Some(&token)).await,
            StatusCode::FORBIDDEN
        );
    }

    #[tokio::test]
    async fn an_operator_passes_both_guards_and_stops_at_availability() {
        // `ManageMaintenance` resolves to Operator-and-up, so this pins that the endpoint asks for a
        // permission an operator actually holds — and that having passed the guard, the caller learns availability (503), which the anonymous caller above
        // deliberately does not.
        let st = private_state();
        let token = st.sessions.issue(
            Uuid::new_v4(),
            Principal::new(Role::Operator, Scope::All),
            "op1",
        );
        for path in ["/api/v1/maintenance-windows"] {
            assert_eq!(
                status_of(st.clone(), "POST", path, Some(&token)).await,
                StatusCode::SERVICE_UNAVAILABLE,
                "{path}"
            );
        }
        // Also pins the extractor order on the bulk clear: availability is reported before the
        // query is parsed, so a valid `status` cannot make a skeleton core answer anything else.
        assert_eq!(
            status_of(
                st.clone(),
                "DELETE",
                "/api/v1/maintenance-windows?status=ended",
                Some(&token)
            )
            .await,
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            status_of(st, "POST", &end_path(), Some(&token)).await,
            StatusCode::SERVICE_UNAVAILABLE
        );
    }
    // ── An accepted write (ADR-115) ──────────────────────────────────────────────────

    /// A maintenance window is created over a node that exists.
    #[sqlx::test(migrator = "yagra_base::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn creating_a_maintenance_window_stores_it(pool: sqlx::PgPool) {
        use crate::api::tests_support::{live_state, send, token};
        let st = live_state(pool.clone()).await;
        let tok = token(&st, yagra_common::Role::Admin);
        let node = yagra_base::pgtest::node(&pool, "under-maintenance", 2, None).await;
        let (status, body) = send(
            &st,
            "POST",
            "/api/v1/maintenance-windows",
            &tok,
            Some(serde_json::json!({
                "name": "firmware",
                "scope_level": "node",
                "scope_id": node.to_string(),
                "starts_at": "2030-01-01T00:00:00Z",
                "ends_at": "2030-01-01T02:00:00Z",
            })),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::CREATED, "{body}");
        assert_eq!(
            yagra_base::pgtest::rows(&pool, "maintenance_windows").await,
            1
        );
    }

    /// The zone the times were typed in is stored canonical and read back on the list (ADR-261),
    /// a window that named none reads back without one, and a zone this build does not know is
    /// refused rather than stored.
    #[sqlx::test(migrator = "yagra_base::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_window_keeps_the_zone_it_was_asked_in(pool: sqlx::PgPool) {
        use crate::api::tests_support::{live_state, send, token};
        let st = live_state(pool.clone()).await;
        let tok = token(&st, yagra_common::Role::Admin);
        let node = yagra_base::pgtest::node(&pool, "remote-site", 2, None).await;
        let body = |name: &str, zone: serde_json::Value| {
            serde_json::json!({
                "name": name,
                "scope_level": "node",
                "scope_id": node.to_string(),
                "starts_at": "2030-01-02T03:00:00Z",
                "ends_at": "2030-01-02T05:00:00Z",
                "time_zone": zone,
            })
        };
        for (name, zone) in [
            (
                "asked in new york",
                serde_json::json!("  America/New_York "),
            ),
            ("no zone", serde_json::Value::Null),
            ("empty zone", serde_json::json!("")),
        ] {
            let (status, out) = send(
                &st,
                "POST",
                "/api/v1/maintenance-windows",
                &tok,
                Some(body(name, zone)),
            )
            .await;
            assert_eq!(status, axum::http::StatusCode::CREATED, "{name}: {out}");
        }
        let (status, out) = send(
            &st,
            "POST",
            "/api/v1/maintenance-windows",
            &tok,
            Some(body(
                "abbreviation",
                serde_json::json!("EST5EDT-not-a-zone"),
            )),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::BAD_REQUEST, "{out}");
        assert_eq!(out["error"]["code"], "invalid_time_zone", "{out}");

        let (status, list) = send(&st, "GET", "/api/v1/maintenance-windows", &tok, None).await;
        assert_eq!(status, axum::http::StatusCode::OK, "{list}");
        let zone_of = |name: &str| {
            list.as_array()
                .expect("a list")
                .iter()
                .find(|w| w["name"] == name)
                .unwrap_or_else(|| panic!("{name} missing: {list}"))
                .get("time_zone")
                .cloned()
        };
        assert_eq!(
            zone_of("asked in new york"),
            Some(serde_json::json!("America/New_York"))
        );
        assert_eq!(zone_of("no zone"), None);
        assert_eq!(zone_of("empty zone"), None);
    }

    /// One window per selected node, and a node the caller cannot see gets none.
    ///
    /// 🚨 The scope is carried by the store's `JOIN nodes`, not by a pre-check, so this is the
    /// test that would notice it being dropped — and it has to pass a real filter, because a
    /// `None` scope short-circuits that predicate entirely.
    #[sqlx::test(migrator = "yagra_base::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_bulk_maintenance_window_covers_each_node_it_may_see(pool: sqlx::PgPool) {
        use crate::api::tests_support::{live_state, scoped_token, send};
        let st = live_state(pool.clone()).await;
        let mine = yagra_base::pgtest::group(&pool, "mine").await;
        let theirs = yagra_base::pgtest::group(&pool, "theirs").await;
        let a = yagra_base::pgtest::node(&pool, "a", 1, Some(mine)).await;
        let b = yagra_base::pgtest::node(&pool, "b", 2, Some(mine)).await;
        let hidden = yagra_base::pgtest::node(&pool, "hidden", 3, Some(theirs)).await;
        let tok = scoped_token(&st, &[mine]);

        let (status, body) = send(
            &st,
            "POST",
            "/api/v1/maintenance-windows/bulk",
            &tok,
            Some(serde_json::json!({
                "node_ids": [a, b, hidden],
                "name": "tonight — {node}",
                "starts_at": "2030-01-01T00:00:00Z",
                "ends_at": "2030-01-01T02:00:00Z",
            })),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::OK, "{body}");
        assert_eq!(body["requested"], 3, "{body}");
        assert_eq!(
            body["created"], 2,
            "the scope did not hold, or a window was not opened: {body}"
        );
        assert_eq!(
            yagra_base::pgtest::rows(&pool, "maintenance_windows").await,
            2
        );
        // Each window is named after its own node, not after the batch (ADR-261 decision 26).
        let mut names: Vec<String> =
            sqlx::query_scalar("SELECT name FROM maintenance_windows ORDER BY name")
                .fetch_all(&pool)
                .await
                .expect("names");
        names.sort();
        assert_eq!(
            names,
            vec!["tonight — a".to_owned(), "tonight — b".to_owned()]
        );

        // A window that ends before it begins suppresses nothing while reading as success — the
        // same refusal every other creation path makes.
        let (status, body) = send(
            &st,
            "POST",
            "/api/v1/maintenance-windows/bulk",
            &tok,
            Some(serde_json::json!({
                "node_ids": [a],
                "name": "backwards",
                "starts_at": "2030-01-01T02:00:00Z",
                "ends_at": "2030-01-01T00:00:00Z",
            })),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(body["error"]["code"], "invalid_window", "{body}");
    }

    /// Taking nodes out of maintenance ends every window that names them - two at once included,
    /// and one whose start a browser clock put a few seconds ahead - carves them out of a folder
    /// window they inherit, and leaves planned maintenance and other nodes alone (ADR-261 Inc.4).
    #[sqlx::test(migrator = "yagra_base::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn releasing_nodes_takes_them_out_of_every_window(pool: sqlx::PgPool) {
        use crate::alerts::maintenance::{ExemptionKind, MaintenanceRepo};
        use crate::api::tests_support::{live_state, send, token};
        let st = live_state(pool.clone()).await;
        let tok = token(&st, yagra_common::Role::Admin);
        let site = yagra_base::pgtest::group(&pool, "site").await;
        let a = yagra_base::pgtest::node(&pool, "a", 1, Some(site)).await;
        let b = yagra_base::pgtest::node(&pool, "b", 2, None).await;
        let bystander = yagra_base::pgtest::node(&pool, "c", 3, None).await;
        let repo = MaintenanceRepo::new(pool.clone());
        let now = chrono::Utc::now();
        let hour = chrono::Duration::hours(1);
        let window = |name: &'static str, level: &'static str, id: Uuid, from, to| {
            let repo = &repo;
            async move {
                repo.create_window(name, level, &id.to_string(), from, to)
                    .await
                    .expect("window")
            }
        };
        // Two windows naming `a`, as a doubled bulk action leaves, and one whose start is a few
        // seconds ahead of the database's clock.
        window("twice 1", "node", a, now - hour, now + hour).await;
        window("twice 2", "node", a, now - hour, now + hour).await;
        let skewed = window(
            "browser ahead",
            "node",
            a,
            now + chrono::Duration::seconds(5),
            now + hour,
        )
        .await;
        // The folder `a` sits in, which it inherits.
        window("site work", "group_id", site, now - hour, now + hour * 2).await;
        // Planned for later on `b`, and running on a node nobody asked about.
        let planned = window("tomorrow", "node", b, now + hour * 20, now + hour * 21).await;
        let other = window("other", "node", bystander, now - hour, now + hour).await;

        let (status, body) = send(
            &st,
            "POST",
            "/api/v1/maintenance/release",
            &tok,
            Some(serde_json::json!({ "node_ids": [a, b, a, Uuid::new_v4()] })),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::OK, "{body}");
        assert_eq!(body["requested"], 2, "{body}");
        assert_eq!(body["ended"], 3, "{body}");
        assert_eq!(body["released"], 1, "only `a` inherits anything: {body}");

        let windows = repo.list_windows().await.expect("list");
        // Still covers some time from here on: ends after both its own start and now (a second of
        // slack for the two clocks in this test). A window made zero-length covers nothing.
        let ts = |s: &str| chrono::DateTime::parse_from_rfc3339(s).expect("ts");
        let covers = |w: &StoredWindow| {
            let end = ts(&w.ends_at);
            end > ts(&w.starts_at) && end > now + chrono::Duration::seconds(1)
        };
        let still_running = |id: Uuid| covers(windows.iter().find(|w| w.id == id).expect("kept"));
        assert!(
            !windows
                .iter()
                .filter(|w| w.scope_id == a.to_string())
                .any(covers),
            "a window naming `a` still covers time: {windows:?}"
        );
        assert!(
            !still_running(skewed),
            "the skewed window was left to start"
        );
        assert!(still_running(planned), "planned maintenance was cancelled");
        assert!(still_running(other), "another node's window was ended");
        let exempt = repo
            .exempt_nodes(ExemptionKind::Maintenance)
            .await
            .expect("exemptions");
        assert_eq!(
            exempt,
            vec![a],
            "`a` was not carved out of its folder's window"
        );
    }

    /// A folder window whose start a browser clock put a few seconds ahead is still released from
    /// (ADR-261 Inc.4): the release must not miss it, and the reconcile that runs right after must
    /// not drop the release before the window begins.
    #[sqlx::test(migrator = "yagra_base::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_folder_window_a_second_ahead_is_still_released(pool: sqlx::PgPool) {
        use crate::alerts::maintenance::{reconcile_exemptions, ExemptionKind, MaintenanceRepo};
        use crate::api::tests_support::{live_state, send, token};
        let st = live_state(pool.clone()).await;
        let tok = token(&st, yagra_common::Role::Admin);
        let site = yagra_base::pgtest::group(&pool, "site").await;
        let node = yagra_base::pgtest::node(&pool, "a", 1, Some(site)).await;
        let repo = MaintenanceRepo::new(pool.clone());
        let now = chrono::Utc::now();
        repo.create_window(
            "site work",
            "group_id",
            &site.to_string(),
            now + chrono::Duration::seconds(5),
            now + chrono::Duration::hours(2),
        )
        .await
        .expect("window");

        let (status, body) = send(
            &st,
            "POST",
            "/api/v1/maintenance/release",
            &tok,
            Some(serde_json::json!({ "node_ids": [node] })),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::OK, "{body}");
        assert_eq!(body["released"], 1, "the folder window was missed: {body}");

        let admin = st.admin.clone().expect("live state");
        reconcile_exemptions(&repo, &admin.groups, &admin.repo)
            .await
            .expect("reconcile");
        assert_eq!(
            repo.exempt_nodes(ExemptionKind::Maintenance)
                .await
                .expect("exemptions"),
            vec![node],
            "the reconcile dropped a release for a window about to begin"
        );
    }

    /// A Viewer does not hold `ManageMaintenance`, so the bulk route refuses before touching the store.
    #[sqlx::test(migrator = "yagra_base::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_viewer_cannot_suppress_a_batch(pool: sqlx::PgPool) {
        use crate::api::tests_support::{live_state, send, token};
        let st = live_state(pool.clone()).await;
        let viewer = token(&st, yagra_common::Role::Viewer);
        let node = yagra_base::pgtest::node(&pool, "a", 1, None).await;

        for (path, body) in [(
            "/api/v1/maintenance-windows/bulk",
            serde_json::json!({
                "node_ids": [node], "name": "n",
                "starts_at": "2030-01-01T00:00:00Z", "ends_at": "2030-01-01T02:00:00Z",
            }),
        )] {
            let (status, out) = send(&st, "POST", path, &viewer, Some(body)).await;
            assert_eq!(status, axum::http::StatusCode::FORBIDDEN, "{path}: {out}");
        }
        assert_eq!(
            yagra_base::pgtest::rows(&pool, "maintenance_windows").await,
            0
        );
    }
}
