// SPDX-License-Identifier: AGPL-3.0-only
//! Threshold rules (Alerting ▸ Thresholds) — scope-based warning/critical bounds.
//!
//! Every endpoint is `ManageConfig`: a threshold decides when the fleet pages someone, so reading
//! the ruleset is as sensitive as writing it. Resolution (most-specific-wins, ADR-013) lives in
//! `yagra_common`; this module is only the CRUD surface over [`crate::thresholds::ThresholdStore`].
//!
//! **The list is capped.** Thresholds are the one configuration table that grows with the fleet —
//! a node-level override is per (node × metric), so tens of thousands of nodes means a response
//! nobody can render. The cap is explicit in the payload (`total` / `truncated`) rather than a
//! silent prefix, because a silently short list reads as "these are all the rules", which is
//! exactly the wrong belief to hold about alerting configuration.

use super::error::{ApiError, ApiResult};
use super::extract::{Admin, RequireManageConfig};
use super::util::{CreatedId, ListQuery};
use super::{is_valid_metric_name, ApiState};
use axum::{
    extract::{Path, Query},
    http::StatusCode,
    routing::{delete, get},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Maximum rules returned by one list call. Matches the poller drill-down's cap: enough that no
/// real hand-authored ruleset is ever truncated, small enough that a fleet-scale accident is a
/// slow page rather than an unusable one.
const THRESHOLDS_MAX: i64 = 500;

/// The threshold routes, merged into `/api/v1` by [`super::router`].
pub(super) fn routes() -> Router<ApiState> {
    Router::new()
        .route(
            "/api/v1/thresholds",
            get(list_thresholds).post(create_threshold),
        )
        .route("/api/v1/thresholds/:id", delete(delete_threshold))
}

/// A capped page of threshold rules.
///
/// `total` is the unfiltered row count, not `items.len()`, so the UI can say *how many* it is not
/// showing. `truncated` is derived rather than left to the client comparing the two — a client that
/// forgets the comparison shows a complete-looking list.
#[derive(Debug, Serialize)]
pub(crate) struct ThresholdPage {
    items: Vec<crate::thresholds::StoredThreshold>,
    /// Rules stored in total, ignoring the cap.
    total: i64,
    /// Whether `items` is a prefix of the ruleset rather than all of it.
    truncated: bool,
}

async fn list_thresholds(
    _guard: RequireManageConfig,
    admin: Admin,
    Query(q): Query<ListQuery>,
) -> ApiResult<Json<ThresholdPage>> {
    let limit = q.limit.unwrap_or(THRESHOLDS_MAX).clamp(1, THRESHOLDS_MAX);
    let (items, total) = admin.thresholds.list_page(limit).await.map_err(|e| {
        ApiError::from_internal(e.as_ref(), "list thresholds", "failed to list thresholds")
    })?;
    let truncated = total > i64::try_from(items.len()).unwrap_or(i64::MAX);
    if truncated {
        tracing::info!(total, limit, "threshold list capped to the page limit");
    }
    Ok(Json(ThresholdPage {
        items,
        total,
        truncated,
    }))
}

/// Create-threshold request body.
#[derive(Deserialize)]
struct CreateThreshold {
    scope_level: String,
    scope_id: String,
    metric: String,
    direction: String,
    warning: Option<f64>,
    critical: Option<f64>,
    dwell_samples: Option<i32>,
}

async fn create_threshold(
    _guard: RequireManageConfig,
    admin: Admin,
    Json(body): Json<CreateThreshold>,
) -> ApiResult<(StatusCode, Json<CreatedId>)> {
    if !is_valid_metric_name(&body.metric) {
        return Err(ApiError::bad_request(
            "invalid_metric_name",
            "metric must be a valid identifier",
        ));
    }
    if !matches!(body.scope_level.as_str(), "profile" | "group" | "node")
        || !matches!(body.direction.as_str(), "above" | "below")
    {
        return Err(ApiError::bad_request(
            "invalid_threshold",
            "scope_level must be profile|group|node and direction above|below",
        ));
    }
    let id = admin
        .thresholds
        .create(
            &body.scope_level,
            &body.scope_id,
            &body.metric,
            &body.direction,
            body.warning,
            body.critical,
            body.dwell_samples.unwrap_or(3),
        )
        .await
        .map_err(|e| {
            ApiError::from_internal(e.as_ref(), "create threshold", "failed to create threshold")
        })?;
    Ok((StatusCode::CREATED, Json(CreatedId { id })))
}

async fn delete_threshold(
    _guard: RequireManageConfig,
    admin: Admin,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    match admin.thresholds.delete(id).await {
        Ok(true) => Ok(StatusCode::NO_CONTENT),
        Ok(false) => Err(ApiError::not_found(
            "threshold_not_found",
            format!("no threshold {id}"),
        )),
        Err(e) => Err(ApiError::from_internal(
            e.as_ref(),
            "delete threshold",
            "failed to delete threshold",
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

    /// Every method/path this module serves, so the guard tests below cannot silently skip one.
    fn routes_under_test() -> Vec<(&'static str, String)> {
        vec![
            ("GET", "/api/v1/thresholds".to_owned()),
            ("POST", "/api/v1/thresholds".to_owned()),
            ("DELETE", format!("/api/v1/thresholds/{ID}")),
        ]
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
    async fn an_anonymous_caller_is_told_it_is_anonymous_and_nothing_else() {
        // `Require*` before `Admin`: 401, never the 503 that would reveal whether this deployment
        // has a write side. All three answered 503 first before the migration.
        for (method, path) in routes_under_test() {
            assert_eq!(
                status_of(private_state(), method, &path, None).await,
                StatusCode::UNAUTHORIZED,
                "{method} {path}"
            );
        }
    }

    #[tokio::test]
    async fn reading_thresholds_is_admin_only_even_on_a_public_dashboard() {
        // Unlike most reads, the list is `ManageConfig`, not `View`: the ruleset is what decides
        // when the fleet pages someone, and a public dashboard must not expose it.
        assert_eq!(
            status_of(public_state(), "GET", "/api/v1/thresholds", None).await,
            StatusCode::UNAUTHORIZED,
        );
        let st = private_state();
        for role in [Role::Viewer, Role::Operator] {
            let token = st
                .sessions
                .issue(Uuid::new_v4(), Principal::new(role, Scope::All), "u");
            for (method, path) in routes_under_test() {
                // 403, not 401 — "not allowed" and "not logged in" are different fixes.
                assert_eq!(
                    status_of(st.clone(), method, &path, Some(&token)).await,
                    StatusCode::FORBIDDEN,
                    "{role:?} {method} {path}"
                );
            }
        }
    }

    #[test]
    fn the_page_limit_is_clamped_to_the_cap_in_both_directions() {
        // The clamp is the DoS guard: `?limit=` is operator-supplied, so an unbounded or zero/
        // negative value must never reach the query.
        let clamp = |n: Option<i64>| n.unwrap_or(THRESHOLDS_MAX).clamp(1, THRESHOLDS_MAX);
        assert_eq!(clamp(None), THRESHOLDS_MAX);
        assert_eq!(clamp(Some(10)), 10);
        assert_eq!(clamp(Some(0)), 1);
        assert_eq!(clamp(Some(-5)), 1);
        assert_eq!(clamp(Some(i64::MAX)), THRESHOLDS_MAX);
    }

    #[test]
    fn truncation_is_derived_from_the_total_not_from_a_full_page() {
        // A page that happens to be exactly `limit` long is not truncated; the server says so
        // rather than leaving the client to infer it from `items.len() == limit`, which is wrong
        // whenever the ruleset is exactly the cap size.
        let truncated = |total: i64, got: usize| total > i64::try_from(got).unwrap_or(i64::MAX);
        assert!(!truncated(500, 500));
        assert!(truncated(501, 500));
        assert!(!truncated(0, 0));
    }
}
