// SPDX-License-Identifier: AGPL-3.0-only
//! Pins on the inventory tree: the nodes and folders one account keeps within reach (ADR-146).
//!
//! The tree's "Pinned only" switch shows what is pinned plus the folders above it. The pins live per
//! account in `user_pins` ([`crate::pins`]), so they follow a person between machines.
//!
//! [`Caller`] on every route, for the reason `preferences.rs` gives: the rows are keyed by the
//! signed-in account, `RequireView` would open them to every anonymous visitor of a public
//! deployment, and an API token names no person.
//!
//! **Visibility is checked on the way in and filtered on the way out** (ADR-014). A pin on a node or
//! folder the caller cannot see is refused with the same 404 every per-node route gives. A pin that
//! stops being visible later — the account's scope was narrowed — stays stored and is simply not
//! returned. **Removing a pin checks nothing**: it can only ever reach the caller's own row, and
//! refusing it would leave an invisible pin counting against the cap with no way to clear it.
//!
//! ⚠️ Every change here writes an audit row (`audit_mw` has no per-route opt-out). That is one row
//! per click, which is the grain an audit log is for — unlike the preferences document, nothing here
//! is saved per pointer event.

use super::error::{ApiError, ApiResult};
use super::extract::{Admin, Caller, Scoped, VisibleNode};
use super::ApiState;
use crate::pins::{PinOutcome, PinTarget, PINS_MAX};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::{get, put},
    Json, Router,
};
use serde::Serialize;
use std::collections::HashMap;
use uuid::Uuid;

/// This domain's slice of the OpenAPI document (ADR-035), merged by [`super::openapi::document`].
#[derive(utoipa::OpenApi)]
#[openapi(paths(list_pins, pin_node, unpin_node, pin_group, unpin_group))]
pub(super) struct Doc;

/// The caller's pins, as the inventory tree draws them.
#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct Pins {
    /// Pinned folders the caller may see. The tree shows each one with everything beneath it.
    group_ids: Vec<Uuid>,
    /// Pinned nodes the caller may see, as full inventory rows. A pinned node usually sits in a
    /// folder the tree has not loaded, so its row cannot come from anywhere else.
    nodes: Vec<super::nodes::NodeSummary>,
}

/// The pin routes, merged into `/api/v1` by [`super::router`].
pub(super) fn routes() -> Router<ApiState> {
    Router::new()
        .route("/api/v1/pins", get(list_pins))
        .route(
            "/api/v1/pins/nodes/:node_id",
            put(pin_node).delete(unpin_node),
        )
        .route(
            "/api/v1/pins/groups/:group_id",
            put(pin_group).delete(unpin_group),
        )
}

/// The caller's pinned folders and nodes, narrowed to what the caller may see.
#[utoipa::path(
    get, path = "/api/v1/pins", tag = "pins",
    responses(
        (status = 200, description = "The caller's pinned folders and nodes, narrowed to what the caller may see", body = Pins),
        (status = 401, description = "No valid bearer token — pins are keyed by account, so this stays closed in public-dashboard mode", body = super::error::ErrorBody),
        (status = 403, description = "An API token names no person, so it has no pins", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode has no write side", body = super::error::ErrorBody),
    ),
)]
async fn list_pins(
    caller: Caller,
    Scoped(scope): Scoped,
    admin: Admin,
    State(st): State<ApiState>,
) -> ApiResult<Json<Pins>> {
    let pins = admin
        .pins
        .list_for_user(&caller.0.username)
        .await
        .map_err(|e| ApiError::from_internal(e.as_ref(), "list pins", "failed to load pins"))?;
    let rows = admin
        .repo
        .list_nodes_by_ids(scope.group_filter(), &pins.nodes)
        .await
        .map_err(|e| {
            ApiError::from_internal(e.as_ref(), "load pinned nodes", "failed to load pins")
        })?;
    // The order arrives with the rows, so `build_node_summaries` asks nothing more about it.
    let orders: HashMap<Uuid, f64> = rows
        .iter()
        .map(|o| (o.node.id.as_uuid(), o.sort_order))
        .collect();
    let nodes = rows.into_iter().map(|o| o.node).collect();
    // `allows_group`, not `allows_group_row`: a breadcrumb ancestor is named so the tree has a
    // spine, but showing it as pinned would show everything beneath it (ADR-014).
    let group_ids = pins
        .groups
        .into_iter()
        .filter(|g| scope.allows_group(Some(*g)))
        .collect();
    Ok(Json(Pins {
        group_ids,
        nodes: super::nodes::build_node_summaries(&st, nodes, orders).await,
    }))
}

/// Store a pin and turn its outcome into the route's answer.
async fn pin(admin: &Admin, username: &str, target: PinTarget) -> ApiResult<StatusCode> {
    let outcome = admin
        .pins
        .pin(username, target, PINS_MAX)
        .await
        .map_err(|e| ApiError::from_internal(e.as_ref(), "pin", "failed to save the pin"))?;
    match outcome {
        PinOutcome::Pinned => Ok(StatusCode::NO_CONTENT),
        PinOutcome::AtLimit => Err(ApiError::conflict(
            "pin_limit",
            format!("an account may hold at most {PINS_MAX} pins"),
        )),
        PinOutcome::NoTarget => Err(match target {
            PinTarget::Node(id) => ApiError::not_found("node_not_found", format!("no node {id}")),
            PinTarget::Group(id) => {
                ApiError::not_found("group_not_found", format!("no group {id}"))
            }
        }),
        PinOutcome::NoUser => Err(ApiError::not_found(
            "user_not_found",
            "no such user account",
        )),
    }
}

/// Remove a pin. Answers 204 whether or not it was there.
async fn unpin(admin: &Admin, username: &str, target: PinTarget) -> ApiResult<StatusCode> {
    admin
        .pins
        .unpin(username, target)
        .await
        .map_err(|e| ApiError::from_internal(e.as_ref(), "unpin", "failed to remove the pin"))?;
    Ok(StatusCode::NO_CONTENT)
}

/// Pin a node. Repeating it is not an error.
#[utoipa::path(
    put, path = "/api/v1/pins/nodes/{node_id}", tag = "pins",
    params(("node_id" = Uuid, Path, description = "Node id")),
    responses(
        (status = 204, description = "The node is pinned (or already was)"),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "An API token names no person, so it cannot pin", body = super::error::ErrorBody),
        (status = 404, description = "No such node, not one this caller may see, or the session's account no longer exists", body = super::error::ErrorBody),
        (status = 409, description = "The account already holds the maximum number of pins (`pin_limit`)", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode has no write side", body = super::error::ErrorBody),
    ),
)]
async fn pin_node(
    caller: Caller,
    _visible: VisibleNode,
    admin: Admin,
    Path(node_id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    pin(&admin, &caller.0.username, PinTarget::Node(node_id)).await
}

/// Remove the caller's pin on a node.
#[utoipa::path(
    delete, path = "/api/v1/pins/nodes/{node_id}", tag = "pins",
    params(("node_id" = Uuid, Path, description = "Node id")),
    responses(
        (status = 204, description = "The node is not pinned (whether or not it was)"),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "An API token names no person, so it has no pins", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode has no write side", body = super::error::ErrorBody),
    ),
)]
async fn unpin_node(
    caller: Caller,
    admin: Admin,
    Path(node_id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    unpin(&admin, &caller.0.username, PinTarget::Node(node_id)).await
}

/// Pin a folder. The tree shows it with everything beneath it. Repeating it is not an error.
#[utoipa::path(
    put, path = "/api/v1/pins/groups/{group_id}", tag = "pins",
    params(("group_id" = Uuid, Path, description = "Folder id")),
    responses(
        (status = 204, description = "The folder is pinned (or already was)"),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "An API token names no person, so it cannot pin", body = super::error::ErrorBody),
        (status = 404, description = "No such folder, not one this caller may see, or the session's account no longer exists", body = super::error::ErrorBody),
        (status = 409, description = "The account already holds the maximum number of pins (`pin_limit`)", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode has no write side", body = super::error::ErrorBody),
    ),
)]
async fn pin_group(
    caller: Caller,
    Scoped(scope): Scoped,
    admin: Admin,
    Path(group_id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    super::scope::require_visible_group(&scope, group_id)?;
    pin(&admin, &caller.0.username, PinTarget::Group(group_id)).await
}

/// Remove the caller's pin on a folder.
#[utoipa::path(
    delete, path = "/api/v1/pins/groups/{group_id}", tag = "pins",
    params(("group_id" = Uuid, Path, description = "Folder id")),
    responses(
        (status = 204, description = "The folder is not pinned (whether or not it was)"),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "An API token names no person, so it has no pins", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode has no write side", body = super::error::ErrorBody),
    ),
)]
async fn unpin_group(
    caller: Caller,
    admin: Admin,
    Path(group_id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    unpin(&admin, &caller.0.username, PinTarget::Group(group_id)).await
}

#[cfg(test)]
mod tests {
    use crate::api::router;
    use crate::api::tests_support::{private_state, public_state};
    use crate::api::ApiState;
    use axum::body::Body;
    use axum::http::{header::AUTHORIZATION, Request, StatusCode};
    use tower::ServiceExt;
    use uuid::Uuid;
    use yagra_common::{Principal, Role, Scope};

    /// Every pin route, with a concrete id in the path.
    fn routes() -> Vec<(&'static str, String)> {
        let id = Uuid::new_v4();
        vec![
            ("GET", "/api/v1/pins".to_owned()),
            ("PUT", format!("/api/v1/pins/nodes/{id}")),
            ("DELETE", format!("/api/v1/pins/nodes/{id}")),
            ("PUT", format!("/api/v1/pins/groups/{id}")),
            ("DELETE", format!("/api/v1/pins/groups/{id}")),
        ]
    }

    async fn status_of(st: ApiState, method: &str, path: &str, token: Option<&str>) -> StatusCode {
        let mut b = Request::builder().method(method).uri(path);
        if let Some(t) = token {
            b = b.header(AUTHORIZATION, format!("Bearer {t}"));
        }
        router(st)
            .oneshot(b.body(Body::empty()).unwrap())
            .await
            .unwrap()
            .status()
    }

    #[tokio::test]
    async fn every_pin_route_answers_an_anonymous_caller_with_401() {
        // `Caller` first: never the 503 that would reveal whether this deployment has a write side,
        // and never open on a public dashboard, where every visitor would share one set of pins.
        for (method, path) in routes() {
            for st in [private_state(), public_state()] {
                assert_eq!(
                    status_of(st, method, &path, None).await,
                    StatusCode::UNAUTHORIZED,
                    "{method} {path}"
                );
            }
        }
    }

    #[tokio::test]
    async fn a_signed_in_viewer_clears_every_gate_and_reaches_availability() {
        // The positive control: without it a guard refusing everyone would look correct. A viewer
        // may pin — pins are the caller's own navigation, not configuration.
        let st = private_state();
        let token = st.sessions.issue(
            Uuid::new_v4(),
            Principal::new(Role::Viewer, Scope::All),
            "v",
        );
        for (method, path) in routes() {
            assert_eq!(
                status_of(st.clone(), method, &path, Some(&token)).await,
                StatusCode::SERVICE_UNAVAILABLE,
                "{method} {path}"
            );
        }
    }

    // ── Accepted writes (ADR-115) ────────────────────────────────────────────────────

    fn id_of(v: &serde_json::Value) -> Vec<String> {
        v.as_array()
            .expect("an array")
            .iter()
            .map(|x| {
                x.as_str()
                    .map_or_else(|| x["id"].to_string(), str::to_owned)
            })
            .map(|s| s.trim_matches('"').to_owned())
            .collect()
    }

    /// A viewer pins a node and a folder, reads them back, and removes one.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn pinning_a_node_and_a_folder_lists_them_and_unpinning_removes_them(pool: sqlx::PgPool) {
        use crate::api::tests_support::{account_token, live_state, send};
        let st = live_state(pool.clone()).await;
        let (tok, _) = account_token(&st, "fixture-pins", Role::Viewer).await;
        let site = crate::pgtest::group(&pool, "site").await;
        let node = crate::pgtest::node(&pool, "core-1", 1, Some(site)).await;

        for path in [
            format!("/api/v1/pins/nodes/{node}"),
            format!("/api/v1/pins/groups/{site}"),
            // Repeated: still 204, still one row.
            format!("/api/v1/pins/nodes/{node}"),
        ] {
            let (status, body) = send(&st, "PUT", &path, &tok, None).await;
            assert_eq!(status, StatusCode::NO_CONTENT, "{path}: {body}");
        }
        assert_eq!(crate::pgtest::rows(&pool, "user_pins").await, 2);

        let (status, pins) = send(&st, "GET", "/api/v1/pins", &tok, None).await;
        assert_eq!(status, StatusCode::OK, "{pins}");
        assert_eq!(id_of(&pins["group_ids"]), vec![site.to_string()]);
        assert_eq!(id_of(&pins["nodes"]), vec![node.to_string()]);
        // A full inventory row, so the tree can place it under its folder.
        assert_eq!(pins["nodes"][0]["group_id"], serde_json::json!(site));

        let path = format!("/api/v1/pins/nodes/{node}");
        for _ in 0..2 {
            let (status, body) = send(&st, "DELETE", &path, &tok, None).await;
            assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
        }
        assert_eq!(crate::pgtest::rows(&pool, "user_pins").await, 1);
    }

    /// Deleting a pinned node or folder takes the pin with it — no id is left behind to point at
    /// nothing.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_pin_leaves_with_the_node_or_folder_it_names(pool: sqlx::PgPool) {
        use crate::api::tests_support::{account_token, live_state, send};
        let st = live_state(pool.clone()).await;
        let (tok, _) = account_token(&st, "fixture-pins-admin", Role::Admin).await;
        let site = crate::pgtest::group(&pool, "site").await;
        let node = crate::pgtest::node(&pool, "core-1", 1, None).await;
        for path in [
            format!("/api/v1/pins/nodes/{node}"),
            format!("/api/v1/pins/groups/{site}"),
        ] {
            let (status, body) = send(&st, "PUT", &path, &tok, None).await;
            assert_eq!(status, StatusCode::NO_CONTENT, "{path}: {body}");
        }

        let (status, body) =
            send(&st, "DELETE", &format!("/api/v1/nodes/{node}"), &tok, None).await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
        assert_eq!(crate::pgtest::rows(&pool, "user_pins").await, 1);
        let path = format!("/api/v1/node-groups/{site}");
        let (status, body) = send(&st, "DELETE", &path, &tok, None).await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
        assert_eq!(crate::pgtest::rows(&pool, "user_pins").await, 0);
    }

    /// A scoped caller cannot pin what they cannot see, and a pin that falls outside a narrowed
    /// scope is not returned. Both directions, so a handler refusing everyone cannot pass.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_scope_decides_what_may_be_pinned_and_what_is_returned(pool: sqlx::PgPool) {
        use crate::api::tests_support::{account_token, live_state, send};
        let st = live_state(pool.clone()).await;
        let (all, user_id) = account_token(&st, "fixture-pins-scoped", Role::Viewer).await;
        let mine = crate::pgtest::group(&pool, "mine").await;
        let theirs = crate::pgtest::group(&pool, "theirs").await;
        let far = crate::pgtest::node(&pool, "far-1", 2, Some(theirs)).await;
        let scoped = st.sessions.issue(
            user_id,
            Principal::new(Role::Viewer, Scope::groups([mine.to_string()])),
            "fixture-pins-scoped",
        );

        for path in [
            format!("/api/v1/pins/nodes/{far}"),
            format!("/api/v1/pins/groups/{theirs}"),
        ] {
            let (status, body) = send(&st, "PUT", &path, &scoped, None).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{path}: {body}");
        }
        let (status, body) = send(
            &st,
            "PUT",
            &format!("/api/v1/pins/groups/{mine}"),
            &scoped,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

        // Pinned while the account could see everything, then read through the narrow session.
        for path in [
            format!("/api/v1/pins/nodes/{far}"),
            format!("/api/v1/pins/groups/{theirs}"),
        ] {
            let (status, body) = send(&st, "PUT", &path, &all, None).await;
            assert_eq!(status, StatusCode::NO_CONTENT, "{path}: {body}");
        }
        assert_eq!(crate::pgtest::rows(&pool, "user_pins").await, 3);
        let (status, pins) = send(&st, "GET", "/api/v1/pins", &scoped, None).await;
        assert_eq!(status, StatusCode::OK, "{pins}");
        assert_eq!(id_of(&pins["group_ids"]), vec![mine.to_string()]);
        assert!(id_of(&pins["nodes"]).is_empty(), "{pins}");
        let (_, everything) = send(&st, "GET", "/api/v1/pins", &all, None).await;
        assert_eq!(id_of(&everything["group_ids"]).len(), 2, "{everything}");
        assert_eq!(id_of(&everything["nodes"]), vec![far.to_string()]);
    }
}
