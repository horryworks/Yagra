// SPDX-License-Identifier: AGPL-3.0-only
//! Maintenance windows and mutes — the two ways an operator says "do not alert me about this".
//!
//! They carry different permissions: a **window** is planned configuration
//! (`ManageMaintenance`), a **mute** is an operational reaction to something happening right now
//! (`AckAlerts`, the same permission as acknowledging an alert). Both currently resolve to
//! `Role::Operator` and up, so the distinction buys nothing *today* — it exists so a future role
//! can be given one without the other, which is only possible if the endpoints ask for the right
//! one now.
//!
//! Both suppress alerting, so both are load-bearing for the "no notification floods" goal — and
//! both are therefore validated hard at the edge. The shared piece is [`window_bounds`]: a window
//! that ends before it starts, or a mute already in the past, silences nothing while looking to the
//! operator like it worked. That is a worse failure than a rejection.

use super::extract::{Admin, RequireAckAlerts, RequireManageMaintenance, RequireView, Scoped};
use super::scope::ScopeTarget;
use super::util::CreatedId;
use super::{is_valid_metric_name, parse_rfc3339, ApiError, ApiResult, ApiState};
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{delete, get, put},
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
    clear_ended_maintenance_windows,
    set_maintenance_window_enabled,
    delete_maintenance_window,
    list_mutes,
    create_mute,
    delete_mute
))]
pub(super) struct Doc;

/// The maintenance + mute routes, merged into `/api/v1` by [`super::router`].
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
        .route("/api/v1/mutes", get(list_mutes).post(create_mute))
        .route("/api/v1/mutes/:id", delete(delete_mute))
}

/// The scope levels a maintenance window may target. `group_id` is a folder-group UUID (resolved
/// recursively — the All Nodes right-click scope); the other three mirror threshold scoping
/// (ADR-013).
///
/// ⚠️ **`system` is deliberately absent.** A fleet-wide window exists (ADR-050) but is opened only
/// by the upgrade path, which knows its own end time. Leaving it out of this list means no caller —
/// scoped or not — can silence the entire fleet through the ordinary CRUD surface, and the list
/// stays the answer to "what may an operator create", which is what the error message promises.
const WINDOW_SCOPE_LEVELS: [&str; 4] = ["profile", "group", "node", "group_id"];
/// The scope kinds a mute may target.
const MUTE_SCOPE_KINDS: [&str; 2] = ["node", "group"];

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
/// ⚠️ **Exhaustive over [`WindowScope`], and deliberately not shared with [`mute_target`].** A
/// window's `Group` scope is a *tag value* and its folder group is `FolderGroup`; a mute's `Group`
/// **is** the folder group. Both serialize as `"group"`. A converter keyed on that string would read
/// a window's tag as a folder id — an operator may perfectly well tag nodes with a UUID — and hand a
/// scoped caller somebody else's window. Only the decision is shared; the reading is per row type.
fn window_target(w: &crate::maintenance::StoredWindow) -> ScopeTarget {
    use crate::maintenance::WindowScope;
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
        // A device class, and a free-form tag value, respectively — each an open-ended node set.
        // `System` joins them for the same reason and the strongest version of it: it names the
        // whole fleet, so no group scope narrows it.
        WindowScope::Profile | WindowScope::Group | WindowScope::System => ScopeTarget::Unbounded,
    }
}

/// A mute's target. `StoredMute` keeps the id in a column per kind, so there is nothing to parse.
fn mute_target(m: &crate::maintenance::StoredMute) -> ScopeTarget {
    use crate::maintenance::MuteScope;
    match m.scope_kind {
        MuteScope::Node => m.node_id.map_or(ScopeTarget::Unbounded, |n| {
            ScopeTarget::Node(yagra_common::NodeId::from(n))
        }),
        MuteScope::Group => m
            .group_id
            .map_or(ScopeTarget::Unbounded, ScopeTarget::Group),
    }
}

/// For a scoped caller, confirm `id` names a window they can see; otherwise `404` — the same answer
/// they get for a window that does not exist, so the endpoint is not an id-enumeration oracle.
///
/// Reads the list rather than one row because [`crate::maintenance::MaintenanceRepo`] has no by-id
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

/// The mute counterpart of [`require_visible_window`].
async fn require_visible_mute(
    st: &ApiState,
    scope: &super::scope::NodeScope,
    admin: &super::AdminState,
    id: Uuid,
) -> Result<(), ApiError> {
    if scope.is_all() {
        return Ok(());
    }
    let mutes = admin.maintenance.list_mutes().await.map_err(|e| {
        ApiError::from_internal(e.as_ref(), "resolve mute scope", "failed to load mutes")
    })?;
    let visible = mutes
        .iter()
        .any(|m| m.id == id && scope.allows_target(st, mute_target(m)));
    if visible {
        Ok(())
    } else {
        Err(ApiError::not_found(
            "mute_not_found",
            format!("no mute {id}"),
        ))
    }
}

/// Confirm `scope_id` names an existing folder group.
///
/// A window or mute pointing at a group that does not exist silences nothing — same failure mode as
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
        (status = 200, description = "Every maintenance window", body = Vec<crate::maintenance::StoredWindow>),
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
) -> ApiResult<Json<Vec<crate::maintenance::StoredWindow>>> {
    Ok(Json(visible_windows(&st, &scope, &admin).await?))
}

/// The maintenance windows `scope` may see.
///
/// A window names its own target, so this filters on the row rather than on a query predicate — and
/// a profile/tag-scoped window is hidden from a scoped caller entirely, matching the rule
/// [`create_maintenance_window`] enforces on the way in.
pub(crate) async fn visible_windows(
    st: &ApiState,
    scope: &super::scope::NodeScope,
    admin: &super::AdminState,
) -> Result<Vec<crate::maintenance::StoredWindow>, ApiError> {
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

/// Create-window body. Times are RFC 3339; the scope mirrors thresholds (ADR-013) plus
/// `group_id` (a folder-group UUID, resolved recursively — the All Nodes right-click scope).
#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct CreateWindow {
    name: String,
    scope_level: String,
    scope_id: String,
    starts_at: String,
    ends_at: String,
}

#[utoipa::path(
    post, path = "/api/v1/maintenance-windows", tag = "maintenance",
    request_body = CreateWindow,
    responses(
        (status = 201, description = "Window created", body = CreatedId),
        (status = 400, description = "Empty name/scope_id, an unknown scope_level, or bounds that are not RFC 3339 or end at or before they start", body = super::error::ErrorBody),
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
    // Same reasoning as `create_mute`: `ManageMaintenance` is an operator permission, so a scoped
    // caller must not open a window over something outside their scope. The `profile`/`group` (tag)
    // levels name a *class* of node rather than one — they are refused outright for a scoped
    // caller, because there is no way to open one that is honestly bounded by their scope.
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
            _ => {
                return Err(ApiError::forbidden_code(
                    "scope_unsupported",
                    "a group-scoped account can open a maintenance window over a node or a folder \
                     group, not over a profile or a tag",
                ))
            }
        }
    }
    let (starts, ends) = window_bounds(&body.starts_at, &body.ends_at)?;
    let id = admin
        .maintenance
        .create_window(
            body.name.trim(),
            &body.scope_level,
            body.scope_id.trim(),
            starts,
            ends,
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
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found(
            "window_not_found",
            format!("no maintenance window {id}"),
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

// ── Mutes (reactive; AckAlerts) ──────────────────────────────────────────────

#[utoipa::path(
    get, path = "/api/v1/mutes", tag = "maintenance",
    responses(
        (status = 200, description = "Every mute", body = Vec<crate::maintenance::StoredMute>),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks the View permission", body = super::error::ErrorBody),
        (status = 503, description = "This core has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn list_mutes(
    _perm: RequireView,
    Scoped(scope): Scoped,
    State(st): State<ApiState>,
    admin: Admin,
) -> ApiResult<Json<Vec<crate::maintenance::StoredMute>>> {
    Ok(Json(visible_mutes(&st, &scope, &admin).await?))
}

/// The mutes `scope` may see — the [`visible_windows`] rule over [`mute_target`].
pub(crate) async fn visible_mutes(
    st: &ApiState,
    scope: &super::scope::NodeScope,
    admin: &super::AdminState,
) -> Result<Vec<crate::maintenance::StoredMute>, ApiError> {
    let mutes =
        admin.maintenance.list_mutes().await.map_err(|e| {
            ApiError::from_internal(e.as_ref(), "list mutes", "failed to list mutes")
        })?;
    Ok(mutes
        .into_iter()
        .filter(|m| scope.allows_target(st, mute_target(m)))
        .collect())
}

/// Create-mute body. `scope_kind` is `node` (silence one node, optionally one `metric_name`) or
/// `group` (silence every node under a folder group, recursive — `metric_name` is ignored);
/// `scope_id` is the node/group UUID. `until` is RFC 3339.
#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct CreateMute {
    scope_kind: String,
    scope_id: Uuid,
    metric_name: Option<String>,
    until: String,
    reason: Option<String>,
}

#[utoipa::path(
    post, path = "/api/v1/mutes", tag = "maintenance",
    request_body = CreateMute,
    responses(
        (status = 201, description = "Mute created", body = CreatedId),
        (status = 400, description = "An unknown scope_kind, an invalid metric_name, or an `until` that is not RFC 3339 or is already in the past", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks the AckAlerts permission", body = super::error::ErrorBody),
        (status = 404, description = "A group scope naming a group that does not exist", body = super::error::ErrorBody),
        (status = 503, description = "This core has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn create_mute(
    _perm: RequireAckAlerts,
    Scoped(scope): Scoped,
    State(st): State<ApiState>,
    admin: Admin,
    Json(body): Json<CreateMute>,
) -> ApiResult<(StatusCode, Json<CreatedId>)> {
    if !MUTE_SCOPE_KINDS.contains(&body.scope_kind.as_str()) {
        return Err(ApiError::bad_request(
            "invalid_mute",
            format!("scope_kind must be {}", MUTE_SCOPE_KINDS.join("|")),
        ));
    }
    let group = body.scope_kind == "group";
    // A mute names its target in the body, and `AckAlerts` is a permission a *scoped* operator
    // holds — so the target has to be checked here or a scoped operator could silence the fleet.
    // A group mute reaches every node beneath the group, hence the stricter group check.
    if group {
        super::scope::require_visible_group(&scope, body.scope_id)?;
    } else {
        super::scope::require_visible_node(&st, &scope, yagra_common::NodeId::from(body.scope_id))?;
    }
    // A group mute silences the whole node-set, so a per-metric mute only applies to a node scope.
    let check = if group {
        None
    } else {
        body.metric_name
            .as_deref()
            .map(str::trim)
            .filter(|c| !c.is_empty())
    };
    if let Some(check) = check {
        // The metric name reaches a PromQL selector, so it is parsed at the edge rather than
        // interpolated (security.md).
        if !is_valid_metric_name(check) {
            return Err(ApiError::bad_request(
                "invalid_mute",
                "metric_name must be a valid metric name (or omitted for the whole node)",
            ));
        }
    }
    if group {
        validate_group_scope(&admin, &body.scope_id.to_string()).await?;
    }
    let Some(until) = parse_rfc3339(&body.until) else {
        return Err(ApiError::bad_request(
            "invalid_mute",
            "until must be an RFC 3339 timestamp",
        ));
    };
    // A mute that has already expired silences nothing while reading as success — the same trap as
    // a backwards window.
    if until <= Utc::now() {
        return Err(ApiError::bad_request(
            "invalid_mute",
            "until must be in the future",
        ));
    }
    let id = admin
        .maintenance
        .create_mute(
            &body.scope_kind,
            body.scope_id,
            check,
            until,
            body.reason.as_deref(),
        )
        .await
        .map_err(|e| ApiError::from_internal(e.as_ref(), "create mute", "failed to create mute"))?;
    Ok((StatusCode::CREATED, Json(CreatedId { id })))
}

#[utoipa::path(
    delete, path = "/api/v1/mutes/{id}", tag = "maintenance",
    params(("id" = Uuid, Path, description = "Mute id")),
    responses(
        (status = 204, description = "Mute deleted"),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks the AckAlerts permission", body = super::error::ErrorBody),
        (status = 404, description = "No such mute", body = super::error::ErrorBody),
        (status = 503, description = "This core has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn delete_mute(
    _perm: RequireAckAlerts,
    Scoped(scope): Scoped,
    State(st): State<ApiState>,
    admin: Admin,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    // Lifting a mute makes the fleet page again — the inverse of creating one, and checked the same
    // way. `create_mute` already validates its target; this closes the other half.
    require_visible_mute(&st, &scope, &admin, id).await?;
    let found =
        admin.maintenance.delete_mute(id).await.map_err(|e| {
            ApiError::from_internal(e.as_ref(), "delete mute", "failed to delete mute")
        })?;
    if found {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found(
            "mute_not_found",
            format!("no mute {id}"),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::router;
    use crate::api::tests_support::{private_state, public_state};
    use crate::maintenance::{StoredMute, StoredWindow};
    use axum::body::Body;
    use axum::http::{header::AUTHORIZATION, Request};
    use tower::ServiceExt;
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

    fn window(level: crate::maintenance::WindowScope, scope_id: &str) -> StoredWindow {
        StoredWindow {
            id: Uuid::new_v4(),
            name: "w".to_owned(),
            level,
            scope_id: scope_id.to_owned(),
            starts_at: "2026-08-01T00:00:00Z".to_owned(),
            ends_at: "2026-08-02T00:00:00Z".to_owned(),
            enabled: true,
            active: true,
        }
    }

    fn mute(
        kind: crate::maintenance::MuteScope,
        node: Option<Uuid>,
        group: Option<Uuid>,
    ) -> StoredMute {
        StoredMute {
            id: Uuid::new_v4(),
            scope_kind: kind,
            node_id: node,
            group_id: group,
            check_name: None,
            until_at: "2026-08-02T00:00:00Z".to_owned(),
            reason: None,
        }
    }

    #[test]
    fn a_windows_tag_group_is_never_read_as_a_folder_group() {
        // ⚠️ The trap this whole `Target` type exists for. A window's `group` scope holds a *tag
        // value*, a mute's `group` holds a folder-group id, and both serialize as "group". An
        // operator is perfectly entitled to tag nodes with a UUID-shaped string, so a helper keyed
        // on the scope text would parse a window's tag as a folder id, compare it against the
        // caller's visible set, and hand them a window belonging to a group they cannot see.
        use crate::maintenance::{MuteScope, WindowScope};
        let g = Uuid::from_u128(42);

        // Same text, same shape, two different meanings — and they must not resolve alike.
        assert_eq!(
            window_target(&window(WindowScope::Group, &g.to_string())),
            ScopeTarget::Unbounded,
            "a window's `group` is a tag value, not a folder group"
        );
        assert_eq!(
            mute_target(&mute(MuteScope::Group, None, Some(g))),
            ScopeTarget::Group(g),
            "a mute's `group` IS the folder group"
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
        use crate::maintenance::{MuteScope, WindowScope};
        assert_eq!(
            window_target(&window(WindowScope::Node, "not-a-uuid")),
            ScopeTarget::Unbounded
        );
        assert_eq!(
            mute_target(&mute(MuteScope::Node, None, None)),
            ScopeTarget::Unbounded
        );
        assert_eq!(
            mute_target(&mute(MuteScope::Group, Some(Uuid::new_v4()), None)),
            ScopeTarget::Unbounded,
            "a group mute reads group_id; a stray node_id must not stand in for it"
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
        let mut b = Request::builder().method(method).uri(path);
        if let Some(t) = token {
            b = b.header(AUTHORIZATION, format!("Bearer {t}"));
        }
        let body = if method == "POST" {
            b = b.header("content-type", "application/json");
            Body::from("{}")
        } else {
            Body::empty()
        };
        router(st)
            .oneshot(b.body(body).unwrap())
            .await
            .unwrap()
            .status()
    }

    #[tokio::test]
    async fn suppression_writes_stay_closed_on_a_public_dashboard() {
        // Silencing alerts is exactly what an anonymous visitor must not be able to do, however
        // open the dashboard's reads are.
        for path in ["/api/v1/maintenance-windows", "/api/v1/mutes"] {
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
    }

    #[tokio::test]
    async fn a_viewer_can_neither_open_a_suppression_nor_clear_one() {
        let st = private_state();
        let token = st.sessions.issue(
            Uuid::new_v4(),
            Principal::new(Role::Viewer, Scope::All),
            "viewer1",
        );
        for path in ["/api/v1/maintenance-windows", "/api/v1/mutes"] {
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
                st,
                "DELETE",
                "/api/v1/maintenance-windows?status=ended",
                Some(&token)
            )
            .await,
            StatusCode::FORBIDDEN
        );
    }

    #[tokio::test]
    async fn an_operator_passes_both_guards_and_stops_at_availability() {
        // `AckAlerts` and `ManageMaintenance` both resolve to Operator-and-up today, so this pins
        // that both endpoints ask for a permission an operator actually holds — and that having
        // passed the guard, the caller learns availability (503), which the anonymous caller above
        // deliberately does not.
        let st = private_state();
        let token = st.sessions.issue(
            Uuid::new_v4(),
            Principal::new(Role::Operator, Scope::All),
            "op1",
        );
        for path in ["/api/v1/mutes", "/api/v1/maintenance-windows"] {
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
                st,
                "DELETE",
                "/api/v1/maintenance-windows?status=ended",
                Some(&token)
            )
            .await,
            StatusCode::SERVICE_UNAVAILABLE
        );
    }
}
