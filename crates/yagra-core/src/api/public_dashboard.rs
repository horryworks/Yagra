// SPDX-License-Identifier: AGPL-3.0-only
//! The public dashboard: the switch that opens a deployment to anonymous visitors, and the one
//! board they see (ADR-123).
//!
//! **Why this is not part of `api/dashboard.rs`.** That module holds two boards that differ only in
//! who they answer for; both are presentation state and both write with `ManageConfig`. This board
//! is a third thing: what it carries **decides which API routes an unauthenticated request may
//! reach** ([`crate::public_access`]). Saving it is an access-control act, so it writes with
//! `ManageSystem` (Admin) — the permission ADR-057 gives to "the deployment itself, and anything
//! that leaves it" — and it lives beside the switch that turns the whole thing on rather than
//! beside the boards it merely resembles.
//!
//! **Four endpoints, and the guard on each is the design:**
//!
//! | | Guard | Why |
//! |---|---|---|
//! | `GET /settings/public-dashboard` | `RequireView` | any signed-in user may know whether this deployment is public |
//! | `PUT /settings/public-dashboard` | `RequireManageSystem` | removing authentication is an Admin act |
//! | `GET /public-dashboard` | `RequireView` | 🚨 **and open to anonymous callers**, via `ALWAYS_OPEN` — without the layout there is no page to draw |
//! | `PUT /public-dashboard` | `RequireManageSystem` | composing this board widens the anonymous surface |
//!
//! 🚨 **`PUT /api/v1/config` was the wrong place for the switch and the reason is worth keeping.**
//! That handler takes `ManageConfig`, which ADR-057 gives to **Operator**. Putting "serve this
//! deployment without authentication" behind it would let an operator open the fleet to the
//! internet. The two permissions are one word apart and answer completely different questions.
//!
//! Both `PUT`s re-derive the anonymous surface **synchronously** before returning, so the admin who
//! just clicked sees the effect on their next request rather than waiting out
//! [`crate::public_access::REFRESH_SECS`]. A standby core in an HA pair still catches up on its own
//! refresh; that window is stated in `public_access`'s own doc.

use super::error::{ApiError, ApiResult};
use super::extract::{Admin, Caller, RequireManageSystem, RequireView};
use super::util::{validate_opaque_doc, MAX_JSON_DOC_BYTES};
use super::ApiState;
use crate::public_access::{self, PublicAccess};
use axum::extract::State;
use axum::{routing::get, Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// This domain's slice of the OpenAPI document (ADR-035), merged by [`super::openapi::document`].
#[derive(utoipa::OpenApi)]
#[openapi(paths(
    get_public_dashboard_switch,
    put_public_dashboard_switch,
    get_public_dashboard,
    put_public_dashboard
))]
pub(super) struct Doc;

/// The state of the switch, and what it currently opens.
#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct PublicDashboardSwitch {
    /// Whether anonymous visitors are served the public board.
    enabled: bool,
    /// How many API routes the current board opens to them.
    ///
    /// Shown in the confirmation dialog before the switch is turned on — the cost stated before the
    /// click rather than discovered after it. Zero with an empty board, which is the honest answer:
    /// turning the switch on with nothing composed serves a page and no data.
    route_count: usize,
}

/// Request body for the switch.
#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct SetPublicDashboard {
    enabled: bool,
}

/// A save's acknowledgement, carrying what the board now opens.
#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct PublicDashboardSaved {
    ok: bool,
    /// Recomputed from the board just saved — so the editor can say "this board opens N routes"
    /// without a second round trip, and without the WebUI reimplementing the derivation.
    route_count: usize,
}

/// The public-dashboard routes, merged into `/api/v1` by [`super::router`].
pub(super) fn routes() -> Router<ApiState> {
    Router::new()
        .route(
            "/api/v1/settings/public-dashboard",
            get(get_public_dashboard_switch).put(put_public_dashboard_switch),
        )
        .route(
            "/api/v1/public-dashboard",
            get(get_public_dashboard).put(put_public_dashboard),
        )
}

/// Reject a layout that is not a JSON object, or is too big to be a real board.
fn validate_layout(body: &Value) -> Result<(), ApiError> {
    validate_opaque_doc(
        body,
        "invalid_layout",
        "layout_too_large",
        "public dashboard layout",
        MAX_JSON_DOC_BYTES,
    )
}

/// Is this deployment public, and how much does the current board open?
#[utoipa::path(
    get, path = "/api/v1/settings/public-dashboard", tag = "settings",
    responses(
        (status = 200, description = "The switch state and the number of routes the current board opens", body = PublicDashboardSwitch),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks read permission", body = super::error::ErrorBody),
    ),
)]
async fn get_public_dashboard_switch(
    _guard: RequireView,
    State(st): State<ApiState>,
) -> ApiResult<Json<PublicDashboardSwitch>> {
    let access = public_access::current(&st.public_access);
    Ok(Json(PublicDashboardSwitch {
        enabled: access.enabled(),
        route_count: access.route_count(),
    }))
}

/// Turn anonymous viewing on or off.
///
/// `ManageSystem`, not `ManageConfig` — see the module doc. Mutating, so `audit_mw` records it
/// automatically; the row is the durable answer to "who opened this deployment and when".
#[utoipa::path(
    put, path = "/api/v1/settings/public-dashboard", tag = "settings",
    request_body = SetPublicDashboard,
    responses(
        (status = 200, description = "Switch applied; the anonymous surface has already been re-derived", body = PublicDashboardSwitch),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageSystem", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode has no write side", body = super::error::ErrorBody),
    ),
)]
async fn put_public_dashboard_switch(
    _guard: RequireManageSystem,
    admin: Admin,
    State(st): State<ApiState>,
    Json(body): Json<SetPublicDashboard>,
) -> ApiResult<Json<PublicDashboardSwitch>> {
    admin
        .repo
        .set_public_dashboard_enabled(body.enabled)
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "set public dashboard switch",
                "failed to save the public dashboard switch",
            )
        })?;
    let access = apply(&st, &admin, body.enabled).await?;
    Ok(Json(PublicDashboardSwitch {
        enabled: access.enabled(),
        route_count: access.route_count(),
    }))
}

/// The public board's layout, or JSON `null` when no admin has composed one.
///
/// 🚨 **The one route anonymous callers reach whatever the board says** (`ALWAYS_OPEN` in
/// [`crate::public_access`]): the page cannot draw itself without knowing which widgets to place.
/// It carries the board's *shape* and no monitoring data — every widget fetches its own content
/// through a route the board had to open.
#[utoipa::path(
    get, path = "/api/v1/public-dashboard", tag = "dashboard",
    responses(
        (status = 200, description = "The public board's opaque layout document, or JSON null when none has been composed", body = serde_json::Value),
        (status = 401, description = "No valid bearer token, and this deployment is not public", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks read permission", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode has no write side", body = super::error::ErrorBody),
    ),
)]
async fn get_public_dashboard(_guard: RequireView, admin: Admin) -> ApiResult<Json<Value>> {
    let layout = admin.public_dashboard.get_public().await.map_err(|e| {
        ApiError::from_internal(
            e.as_ref(),
            "get public dashboard",
            "failed to load the public dashboard layout",
        )
    })?;
    Ok(Json(layout.unwrap_or(Value::Null)))
}

/// Compose the public board — **Admin only**, because this is what decides the anonymous surface.
///
/// Takes `RequireManageSystem` *and* [`Caller`]: the first decides whether the write is allowed,
/// the second names who made it. Attribution matters more here than on the shared board — the row
/// records who last widened what strangers can read.
#[utoipa::path(
    put, path = "/api/v1/public-dashboard", tag = "dashboard",
    request_body = serde_json::Value,
    responses(
        (status = 200, description = "Board saved; the anonymous route set has already been re-derived from it", body = PublicDashboardSaved),
        (status = 400, description = "The layout is not a JSON object", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageSystem", body = super::error::ErrorBody),
        (status = 413, description = "The layout exceeds the document size cap", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode has no write side", body = super::error::ErrorBody),
    ),
)]
async fn put_public_dashboard(
    _guard: RequireManageSystem,
    caller: Caller,
    admin: Admin,
    State(st): State<ApiState>,
    Json(body): Json<Value>,
) -> ApiResult<Json<PublicDashboardSaved>> {
    validate_layout(&body)?;
    admin
        .public_dashboard
        .upsert_public(&body, &caller.0.username)
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "save public dashboard",
                "failed to save the public dashboard layout",
            )
        })?;
    let enabled = public_access::current(&st.public_access).enabled();
    let access = apply(&st, &admin, enabled).await?;
    Ok(Json(PublicDashboardSaved {
        ok: true,
        route_count: access.route_count(),
    }))
}

/// Re-derive and publish the anonymous surface after a write.
///
/// Reads the board back out of the database rather than trusting what was just sent, so the surface
/// always describes what a *reader* will get — the two differ if the write partly failed, and the
/// safe reading of that is the stored row.
async fn apply(
    st: &ApiState,
    admin: &Admin,
    enabled: bool,
) -> Result<std::sync::Arc<PublicAccess>, ApiError> {
    let next = if enabled {
        let layout = admin.public_dashboard.get_public().await.map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "re-read public dashboard",
                "saved, but failed to re-read the public dashboard layout",
            )
        })?;
        PublicAccess::derive(true, layout.as_ref())
    } else {
        PublicAccess::closed()
    };
    public_access::store(&st.public_access, next);
    Ok(public_access::current(&st.public_access))
}

#[cfg(test)]
mod tests {
    use crate::api::router;
    use crate::api::tests_support::{private_state, public_board_state, public_state};
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    async fn get(st: crate::api::ApiState, path: &str) -> StatusCode {
        router(st)
            .oneshot(
                Request::builder()
                    .uri(path)
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response")
            .status()
    }

    #[tokio::test]
    async fn the_switch_is_not_readable_without_a_session() {
        // `RequireView`, and the route is not on any board's allow-list, so a private deployment
        // refuses it — and so does a public one, which is the half worth pinning.
        assert_eq!(
            get(private_state(), "/api/v1/settings/public-dashboard").await,
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn the_layout_route_is_reachable_anonymously_on_a_public_deployment() {
        // The bootstrap route (`ALWAYS_OPEN`): without it an anonymous visitor gets a blank page
        // and no way to tell why. 503 here rather than 200 because the skeleton fixture has no
        // write side — what matters is that it got past the auth guard, which a 401 would not.
        assert_eq!(
            get(public_state(), "/api/v1/public-dashboard").await,
            StatusCode::SERVICE_UNAVAILABLE
        );
    }

    #[tokio::test]
    async fn the_layout_route_still_needs_a_session_on_a_private_deployment() {
        assert_eq!(
            get(private_state(), "/api/v1/public-dashboard").await,
            StatusCode::UNAUTHORIZED
        );
    }

    /// A node id that exists nowhere. `VisibleNode` only asks whether the caller’s scope admits
    /// the id, and an anonymous caller on an allowed route is unscoped, so any well-formed uuid
    /// reaches the handler — which is what these two are about.
    const SOME_NODE: &str = "11111111-2222-3333-4444-555555555555";

    #[tokio::test]
    async fn a_board_with_a_parameterized_widget_opens_the_route_that_widget_reads() {
        // 🚨 The test this feature shipped without, and the defect it would have caught on day
        // one: the allow-list was built from the OpenAPI spelling (`{node_id}`) and compared
        // against `MatchedPath`, which is the router’s (`:node_id`). Every parameterized route
        // was refused for every anonymous visitor, whatever the board carried — and the WebUI
        // drops the reason, so the widget rendered "no traffic yet" rather than an error.
        //
        // ⚠️ It has to go through the real router. `MatchedPath` has no public constructor, so a
        // unit test asserting "the guard accepts `/api/v1/nodes/:node_id/interfaces`" would be
        // asserting our own belief about what axum hands over — the belief that was wrong.
        //
        // 200, not 503: this handler answers an empty list in skeleton mode instead of taking
        // `Admin`. What is being measured is only that it got past the guard.
        assert_eq!(
            get(
                public_board_state(&["interface-traffic"]),
                &format!("/api/v1/nodes/{SOME_NODE}/interfaces")
            )
            .await,
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn a_board_without_that_widget_still_refuses_the_same_parameterized_route() {
        // 🚨 The other half, and neither half means anything alone. With only the accepting test,
        // an implementation that opened every route would pass; with only this one, the shipped
        // defect — which refused everything parameterized — passes. Same shape as
        // `api/guards.rs::every_write_domain_has_an_accepted_write_test`.
        assert_eq!(
            get(
                public_board_state(&["status-summary"]),
                &format!("/api/v1/nodes/{SOME_NODE}/interfaces")
            )
            .await,
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn taking_the_widget_off_the_board_closes_the_route_again() {
        // The property the whole design rests on — the board *is* the access-control list — is
        // only visible across two boards. Asserted here on a parameterized route because that is
        // the family that had never been exercised at all.
        let open = public_board_state(&["metric-chart"]);
        let closed = public_board_state(&["status-summary"]);
        let path = format!("/api/v1/nodes/{SOME_NODE}/metrics");
        // 503 rather than 200: unlike the interfaces list, this handler takes `Admin`. Past the
        // guard is the claim; what the handler then does about a missing write side is not.
        assert_eq!(get(open, &path).await, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(get(closed, &path).await, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn composing_the_board_stays_closed_on_a_public_deployment() {
        // 🚨 The one that must never regress: the board decides what strangers can read, so an
        // anonymous caller must not be able to edit it — on a public deployment least of all.
        let st = public_state();
        let res = router(st)
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/v1/public-dashboard")
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }

    // ── Accepted writes (ADR-115) ────────────────────────────────────────────────────

    /// Composing the board is accepted, stored, and **re-derives the anonymous surface**.
    ///
    /// 🚨 The last third is the point. A test that only checked the row would pass on an
    /// implementation that saved the board and never told `public_access` about it — the operator
    /// would compose a board, see it saved, and find that visitors could reach nothing for the next
    /// thirty seconds, or forever if the refresh task were not running.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn composing_the_board_is_stored_and_reopens_its_routes(pool: sqlx::PgPool) {
        use crate::api::tests_support::{account_token, live_state, send};
        let st = live_state(pool.clone()).await;
        let (tok, _) = account_token(&st, "fixture-public", yagra_common::Role::Admin).await;

        // Switch on first: with it off, a saved board opens nothing (the closed default).
        let (status, body) = send(
            &st,
            "PUT",
            "/api/v1/settings/public-dashboard",
            &tok,
            Some(serde_json::json!({ "enabled": true })),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::OK, "{body}");
        assert_eq!(
            crate::public_access::current(&st.public_access).route_count(),
            0
        );

        let board = serde_json::json!({
            "version": 2,
            "boards": [{
                "id": "b1",
                "name": "Public",
                "widgets": [{ "instanceId": "w1", "type": "status-summary" }],
            }],
        });
        let (status, body) = send(&st, "PUT", "/api/v1/public-dashboard", &tok, Some(board)).await;
        assert_eq!(status, axum::http::StatusCode::OK, "{body}");
        assert_eq!(crate::pgtest::rows(&pool, "public_dashboard").await, 1);

        let access = crate::public_access::current(&st.public_access);
        assert!(access.allows("GET", "/api/v1/fleet/summary"));
        assert!(!access.allows("GET", "/api/v1/events"));

        // And the read side hands the board back.
        let (status, read) = send(&st, "GET", "/api/v1/public-dashboard", &tok, None).await;
        assert_eq!(status, axum::http::StatusCode::OK, "{read}");
        assert!(read.to_string().contains("status-summary"), "{read}");
    }

    /// Turning the switch off closes the surface even with a board still saved.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn switching_off_closes_a_board_that_is_still_stored(pool: sqlx::PgPool) {
        use crate::api::tests_support::{account_token, live_state, send};
        let st = live_state(pool.clone()).await;
        let (tok, _) = account_token(&st, "fixture-public-off", yagra_common::Role::Admin).await;
        send(
            &st,
            "PUT",
            "/api/v1/settings/public-dashboard",
            &tok,
            Some(serde_json::json!({ "enabled": true })),
        )
        .await;
        let board = serde_json::json!({
            "version": 2,
            "boards": [{ "id": "b1", "name": "P", "widgets": [{ "instanceId": "w", "type": "status-summary" }] }],
        });
        send(&st, "PUT", "/api/v1/public-dashboard", &tok, Some(board)).await;
        assert!(
            crate::public_access::current(&st.public_access).allows("GET", "/api/v1/fleet/summary")
        );

        let (status, body) = send(
            &st,
            "PUT",
            "/api/v1/settings/public-dashboard",
            &tok,
            Some(serde_json::json!({ "enabled": false })),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::OK, "{body}");
        // The row survives — turning the deployment private must not destroy the composed board,
        // or turning it public again would silently serve an empty page.
        assert_eq!(crate::pgtest::rows(&pool, "public_dashboard").await, 1);
        let access = crate::public_access::current(&st.public_access);
        assert!(!access.enabled());
        assert!(!access.allows("GET", "/api/v1/fleet/summary"));
    }

    /// An Operator may read the switch and may not move it.
    ///
    /// 🚨 This is the specific mistake the module doc warns about: `PUT /api/v1/config` takes
    /// `ManageConfig`, which Operator holds, so putting the switch there would have let an operator
    /// open the deployment. Pinned as a behaviour rather than as a comment.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn an_operator_can_read_the_switch_but_not_move_it(pool: sqlx::PgPool) {
        use crate::api::tests_support::{account_token, live_state, send};
        let st = live_state(pool.clone()).await;
        let (tok, _) = account_token(&st, "fixture-op", yagra_common::Role::Operator).await;
        let (status, _) = send(&st, "GET", "/api/v1/settings/public-dashboard", &tok, None).await;
        assert_eq!(status, axum::http::StatusCode::OK);
        let (status, _) = send(
            &st,
            "PUT",
            "/api/v1/settings/public-dashboard",
            &tok,
            Some(serde_json::json!({ "enabled": true })),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::FORBIDDEN);
        assert!(!crate::public_access::current(&st.public_access).enabled());
    }

    #[tokio::test]
    async fn flipping_the_switch_stays_closed_on_a_public_deployment() {
        let st = public_state();
        let res = router(st)
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/v1/settings/public-dashboard")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"enabled":false}"#))
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }
}
