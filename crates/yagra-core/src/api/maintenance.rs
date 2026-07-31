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

use super::extract::{Admin, RequireAckAlerts, RequireManageMaintenance, RequireView};
use super::util::CreatedId;
use super::{is_valid_metric_name, parse_rfc3339, ApiError, ApiResult, ApiState};
use axum::{
    extract::Path,
    http::StatusCode,
    routing::{delete, get, put},
    Json, Router,
};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use uuid::Uuid;

/// This domain's slice of the OpenAPI document (ADR-035), merged by [`super::openapi::document`].
#[derive(utoipa::OpenApi)]
#[openapi(paths(
    list_maintenance_windows,
    create_maintenance_window,
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
            get(list_maintenance_windows).post(create_maintenance_window),
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
    admin: Admin,
) -> ApiResult<Json<Vec<crate::maintenance::StoredWindow>>> {
    admin
        .maintenance
        .list_windows()
        .await
        .map(Json)
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "list maintenance windows",
                "failed to list maintenance windows",
            )
        })
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
    admin: Admin,
    Path(id): Path<Uuid>,
    Json(body): Json<super::util::EnabledBody>,
) -> ApiResult<StatusCode> {
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
    admin: Admin,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
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
    admin: Admin,
) -> ApiResult<Json<Vec<crate::maintenance::StoredMute>>> {
    admin
        .maintenance
        .list_mutes()
        .await
        .map(Json)
        .map_err(|e| ApiError::from_internal(e.as_ref(), "list mutes", "failed to list mutes"))
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
    admin: Admin,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
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
    }

    #[tokio::test]
    async fn a_viewer_can_open_neither_a_window_nor_a_mute() {
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
    }
}
