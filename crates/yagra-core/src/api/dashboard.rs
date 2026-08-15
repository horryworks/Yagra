// SPDX-License-Identifier: AGPL-3.0-only
//! Saved dashboard layouts: **My Dashboard** (per account) and the **Shared Dashboard** (one global
//! board an admin edits for everyone).
//!
//! The layout document is opaque to the backend — the WebUI owns its shape and migrates it (doc
//! schema v2, multi-board). So these endpoints validate only what the server can be responsible
//! for: that the body is a JSON object, and that it is not large enough to be an attack. Storing an
//! arbitrary object is deliberate; parsing widget types here would put every new widget on both
//! sides of the wire.
//!
//! The two boards differ in *who* they answer for, which is the whole reason they are separate
//! endpoints rather than a scope parameter:
//!
//! - My Dashboard is keyed by the caller's username, so it demands a real session even in
//!   public-dashboard mode ([`Caller`]) — an anonymous visitor has no account to key on, and
//!   without that guard every public visitor would share one nameless layout.
//! - The Shared Dashboard reads like any other dashboard data (open on a public deployment) but
//!   writes are `ManageConfig`: the change lands on every user's screen.
//!
//! Both `PUT`s are in `changes_monitoring_config`'s exception list. A layout is presentation state
//! no rebuild input reads, and it is saved on every widget add, move, resize and setting change —
//! so counting it made laying out a dashboard force a full-fleet re-resolution on the next tick.
//! That went unnoticed for releases because the recomputed answer was always correct.

use super::error::{ApiError, ApiResult};
use super::extract::{Admin, Caller, RequireManageConfig, RequireView};
use super::util::{validate_opaque_doc, MAX_JSON_DOC_BYTES};
use super::ApiState;
use axum::{routing::get, Json, Router};
use serde::Serialize;
use serde_json::Value;

/// This domain's slice of the OpenAPI document (ADR-035), merged by [`super::openapi::document`].
#[derive(utoipa::OpenApi)]
#[openapi(paths(
    get_dashboard,
    put_dashboard,
    get_shared_dashboard,
    put_shared_dashboard
))]
pub(super) struct Doc;

/// A save's acknowledgement. The layout is not echoed back — the client already has it.
#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct DashboardSaved {
    ok: bool,
}

/// The dashboard routes, merged into `/api/v1` by [`super::router`].
pub(super) fn routes() -> Router<ApiState> {
    Router::new()
        .route("/api/v1/dashboard", get(get_dashboard).put(put_dashboard))
        .route(
            "/api/v1/shared-dashboard",
            get(get_shared_dashboard).put(put_shared_dashboard),
        )
}

/// Reject a layout that is not a JSON object, or is too big to be a real board.
///
/// The two checks live in [`validate_opaque_doc`] because `api/preferences.rs` needs the same pair
/// over a different document with different codes and a different cap. What stays here is this
/// domain's half — the codes the WebUI branches on and the noun its messages use.
fn validate_layout(body: &Value) -> Result<(), ApiError> {
    validate_opaque_doc(
        body,
        "invalid_layout",
        "layout_too_large",
        "dashboard layout",
        MAX_JSON_DOC_BYTES,
    )
}

/// The caller's saved layout, or JSON `null` when they have never saved one — an explicit null
/// rather than 404, so the client falls back to its default layout instead of showing an error.
#[utoipa::path(
    get, path = "/api/v1/dashboard", tag = "dashboard",
    responses(
        (status = 200, description = "The caller's opaque layout document, or JSON null when they have never saved one", body = serde_json::Value),
        (status = 401, description = "No valid bearer token — the layout is keyed by account, so this stays closed in public-dashboard mode", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks read permission", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode has no write side", body = super::error::ErrorBody),
    ),
)]
async fn get_dashboard(caller: Caller, admin: Admin) -> ApiResult<Json<Value>> {
    let layout = admin
        .dashboards
        .get_for_user(&caller.0.username)
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "get dashboard",
                "failed to load the dashboard layout",
            )
        })?;
    Ok(Json(layout.unwrap_or(Value::Null)))
}

/// Save (replace) the caller's layout. Mutating, so `audit_mw` records it automatically.
#[utoipa::path(
    put, path = "/api/v1/dashboard", tag = "dashboard",
    request_body = serde_json::Value,
    responses(
        (status = 200, description = "Layout saved", body = DashboardSaved),
        (status = 400, description = "The layout is not a JSON object", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token — the layout is keyed by account, so this stays closed in public-dashboard mode", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks read permission", body = super::error::ErrorBody),
        (status = 404, description = "The session's account no longer exists", body = super::error::ErrorBody),
        (status = 413, description = "The layout exceeds the document size cap", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode has no write side", body = super::error::ErrorBody),
    ),
)]
async fn put_dashboard(
    caller: Caller,
    admin: Admin,
    Json(body): Json<Value>,
) -> ApiResult<Json<DashboardSaved>> {
    validate_layout(&body)?;
    let saved = admin
        .dashboards
        .upsert_for_user(&caller.0.username, &body)
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "save dashboard",
                "failed to save the dashboard layout",
            )
        })?;
    if !saved {
        // A valid session whose account vanished mid-request.
        return Err(ApiError::not_found(
            "user_not_found",
            "no such user account",
        ));
    }
    Ok(Json(DashboardSaved { ok: true }))
}

/// The global Shared Dashboard layout, or JSON `null` when no admin has saved one. Open-read like
/// the other dashboard data, so it works in public-dashboard mode; the write side is not.
#[utoipa::path(
    get, path = "/api/v1/shared-dashboard", tag = "dashboard",
    responses(
        (status = 200, description = "The global opaque layout document, or JSON null when no admin has saved one", body = serde_json::Value),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks read permission", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode has no write side", body = super::error::ErrorBody),
    ),
)]
async fn get_shared_dashboard(_guard: RequireView, admin: Admin) -> ApiResult<Json<Value>> {
    let layout = admin.shared_dashboard.get_shared().await.map_err(|e| {
        ApiError::from_internal(
            e.as_ref(),
            "get shared dashboard",
            "failed to load the shared dashboard layout",
        )
    })?;
    Ok(Json(layout.unwrap_or(Value::Null)))
}

/// Save (replace) the global layout — **admin only**: it applies to every user.
///
/// Takes `RequireManageConfig` *and* [`Caller`]: the first decides whether the write is allowed, the
/// second names who made it for the row's attribution. A `ManageConfig` holder always has `View`, so
/// the second guard never rejects a caller the first admitted.
#[utoipa::path(
    put, path = "/api/v1/shared-dashboard", tag = "dashboard",
    request_body = serde_json::Value,
    responses(
        (status = 200, description = "Layout saved for every user", body = DashboardSaved),
        (status = 400, description = "The layout is not a JSON object", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
        (status = 413, description = "The layout exceeds the document size cap", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode has no write side", body = super::error::ErrorBody),
    ),
)]
async fn put_shared_dashboard(
    _guard: RequireManageConfig,
    caller: Caller,
    admin: Admin,
    Json(body): Json<Value>,
) -> ApiResult<Json<DashboardSaved>> {
    validate_layout(&body)?;
    admin
        .shared_dashboard
        .upsert_shared(&body, &caller.0.username)
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "save shared dashboard",
                "failed to save the shared dashboard layout",
            )
        })?;
    Ok(Json(DashboardSaved { ok: true }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::router;
    use crate::api::tests_support::{private_state, public_state};
    use axum::body::Body;
    use axum::http::{header::AUTHORIZATION, Request, StatusCode};
    use tower::ServiceExt;
    use uuid::Uuid;
    use yagra_common::{Principal, Role, Scope};

    async fn status_of(
        st: ApiState,
        method: &str,
        path: &str,
        token: Option<&str>,
        body: &str,
    ) -> StatusCode {
        let mut b = Request::builder()
            .method(method)
            .uri(path)
            .header("content-type", "application/json");
        if let Some(t) = token {
            b = b.header(AUTHORIZATION, format!("Bearer {t}"));
        }
        router(st)
            .oneshot(b.body(Body::from(body.to_owned())).unwrap())
            .await
            .unwrap()
            .status()
    }

    #[tokio::test]
    async fn every_dashboard_route_answers_an_anonymous_caller_with_401() {
        // `Require*`/`Caller` before `Admin`: never the 503 that would reveal whether this
        // deployment has a write side.
        for (method, path) in [
            ("GET", "/api/v1/dashboard"),
            ("PUT", "/api/v1/dashboard"),
            ("PUT", "/api/v1/shared-dashboard"),
        ] {
            assert_eq!(
                status_of(private_state(), method, path, None, "{}").await,
                StatusCode::UNAUTHORIZED,
                "{method} {path}"
            );
        }
    }

    #[tokio::test]
    async fn my_dashboard_needs_an_account_even_on_a_public_dashboard() {
        // The layout is keyed by username. Opening it the way other reads open would give every
        // anonymous visitor of a public deployment the same shared, nameless board.
        for method in ["GET", "PUT"] {
            assert_eq!(
                status_of(public_state(), method, "/api/v1/dashboard", None, "{}").await,
                StatusCode::UNAUTHORIZED,
                "{method}"
            );
        }
    }

    #[tokio::test]
    async fn the_shared_dashboard_reads_openly_and_is_written_by_operators_and_up() {
        // Read is open on a public deployment (503 = past the guard, into skeleton mode)…
        assert_eq!(
            status_of(public_state(), "GET", "/api/v1/shared-dashboard", None, "").await,
            StatusCode::SERVICE_UNAVAILABLE,
        );
        // …the write is not. A viewer is 403 rather than 401; an operator is through the guard
        // (`ManageConfig`, Operator-held since ADR-057) and stopped by skeleton mode.
        let st = private_state();
        for (role, want) in [
            (Role::Viewer, StatusCode::FORBIDDEN),
            (Role::Operator, StatusCode::SERVICE_UNAVAILABLE),
        ] {
            let token = st
                .sessions
                .issue(Uuid::new_v4(), Principal::new(role, Scope::All), "u");
            assert_eq!(
                status_of(
                    st.clone(),
                    "PUT",
                    "/api/v1/shared-dashboard",
                    Some(&token),
                    "{}"
                )
                .await,
                want,
                "{role:?}"
            );
        }
    }

    #[tokio::test]
    async fn an_admin_clears_the_gate_and_reaches_availability() {
        // The positive control for the test above: an Admin gets 503 (past RBAC, into skeleton
        // mode), not 403. Without it, a guard that rejected *everyone* would look correct.
        let st = private_state();
        let token = st.sessions.issue(
            Uuid::new_v4(),
            Principal::new(Role::Admin, Scope::All),
            "admin1",
        );
        assert_eq!(
            status_of(
                st,
                "PUT",
                "/api/v1/shared-dashboard",
                Some(&token),
                r#"{"version":2,"boards":[]}"#
            )
            .await,
            StatusCode::SERVICE_UNAVAILABLE,
        );
    }

    #[test]
    fn a_layout_must_be_an_object_and_within_the_size_cap() {
        assert!(validate_layout(&serde_json::json!({})).is_ok());
        // An array or a scalar is not a layout document; accepting one would store something the
        // WebUI cannot read back.
        for bad in [
            serde_json::json!([]),
            serde_json::json!("x"),
            Value::Null,
            serde_json::json!(1),
        ] {
            assert_eq!(
                validate_layout(&bad).unwrap_err().code(),
                "invalid_layout",
                "{bad}"
            );
        }
        let huge = serde_json::json!({ "big": "x".repeat(MAX_JSON_DOC_BYTES) });
        assert_eq!(
            validate_layout(&huge).unwrap_err().code(),
            "layout_too_large"
        );
    }
}
