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
use super::util::{normalize_search, CreatedId};
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

/// This domain's slice of the OpenAPI document (ADR-035), merged by [`super::openapi::document`].
#[derive(utoipa::OpenApi)]
#[openapi(paths(list_thresholds, create_threshold, delete_threshold))]
pub(super) struct Doc;

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
/// `total` is the count of rules **matching the filter**, not `items.len()`, so the UI can say *how
/// many* it is not showing. `truncated` is derived rather than left to the client comparing the
/// two — a client that forgets the comparison shows a complete-looking list.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct ThresholdPage {
    items: Vec<crate::thresholds::StoredThreshold>,
    /// Rules matching the filter, ignoring the cap.
    total: i64,
    /// Whether `items` is a prefix of the matching rules rather than all of them.
    truncated: bool,
}

/// `?limit=` plus the filters, all optional.
///
/// The two enums are parsed at the edge rather than taken as strings: an unknown token is a `400`,
/// never a filter silently dropped. Dropping it here would *widen* the answer — the operator asked
/// for node-level rules and would be shown every rule in the fleet, believing the narrower thing.
#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub(super) struct ThresholdQuery {
    limit: Option<i64>,
    /// Case-insensitive substring of the metric name.
    q: Option<String>,
    /// Only rules defined at this scope level (`profile` | `group` | `node`).
    scope_level: Option<yagra_common::ScopeLevel>,
    /// Only rules breaching in this direction (`above` | `below`).
    direction: Option<yagra_common::Direction>,
}

#[utoipa::path(
    get, path = "/api/v1/thresholds", tag = "thresholds",
    params(ThresholdQuery),
    responses(
        (status = 200, description = "A capped page of matching rules, with the matching total", body = ThresholdPage),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role below Admin — the ruleset decides when the fleet pages someone", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode has no write side", body = super::error::ErrorBody),
    ),
)]
async fn list_thresholds(
    _guard: RequireManageConfig,
    Query(q): Query<ThresholdQuery>,
    admin: Admin,
) -> ApiResult<Json<ThresholdPage>> {
    let metric = normalize_search(q.q.as_deref());
    Ok(Json(
        threshold_page(
            &admin,
            q.limit,
            &crate::thresholds::ThresholdFilter {
                metric: metric.as_deref(),
                level: q.scope_level,
                direction: q.direction,
            },
        )
        .await?,
    ))
}

/// A capped page of rules — the seam both edges call.
///
/// The cap and the `truncated` derivation live here rather than in the handler so the MCP tool
/// cannot serve an uncapped list, and cannot report a complete-looking one either. `truncated` is
/// the load-bearing half: a silently short ruleset reads as "these are all the rules", which is
/// exactly the wrong belief to hold about alerting configuration.
///
/// **The filter is deliberately not exposed on the MCP side.** `get_config(kind=thresholds)` is a
/// configuration dump — its callers ask "what is the ruleset", not "show me the CPU ones" — so the
/// filters are a UI narrowing rather than a question MCP cannot otherwise answer (ADR-042 asks for
/// parity on *questions*, and records the reason when a read has no tool of its own). It still
/// comes through this seam, so the cap and `truncated` can never differ between the two edges.
pub(crate) async fn threshold_page(
    admin: &super::AdminState,
    limit: Option<i64>,
    filter: &crate::thresholds::ThresholdFilter<'_>,
) -> ApiResult<ThresholdPage> {
    let limit = limit.unwrap_or(THRESHOLDS_MAX).clamp(1, THRESHOLDS_MAX);
    let (items, total) = admin
        .thresholds
        .list_page(limit, filter)
        .await
        .map_err(|e| {
            ApiError::from_internal(e.as_ref(), "list thresholds", "failed to list thresholds")
        })?;
    let truncated = total > i64::try_from(items.len()).unwrap_or(i64::MAX);
    if truncated {
        tracing::info!(total, limit, "threshold list capped to the page limit");
    }
    Ok(ThresholdPage {
        items,
        total,
        truncated,
    })
}

/// Whether the built-in catalog declares `metric` a raw counter. Pure (no store) so the
/// rejection below is testable without a database; custom collection items are the async
/// half, via [`crate::collection::CollectionRepo::metric_declared_counter`].
fn is_builtin_counter(metric: &str) -> bool {
    match yagra_common::builtin_metric_kind(metric) {
        Some(yagra_common::MetricKind::Counter) => true,
        Some(yagra_common::MetricKind::Gauge) | None => false,
    }
}

/// Create-threshold request body.
#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct CreateThreshold {
    scope_level: String,
    scope_id: String,
    metric: String,
    direction: String,
    warning: Option<f64>,
    critical: Option<f64>,
    dwell_samples: Option<i32>,
}

#[utoipa::path(
    post, path = "/api/v1/thresholds", tag = "thresholds",
    request_body = CreateThreshold,
    responses(
        (status = 201, description = "Rule created", body = CreatedId),
        (status = 400, description = "The metric is not an identifier, scope_level/direction is outside its vocabulary, or the metric is a raw counter (a monotonic value has no meaningful fixed bound)", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role below Admin", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode has no write side", body = super::error::ErrorBody),
    ),
)]
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
    // A raw counter's sampled value only ever increases (until it wraps or the device reboots),
    // so a fixed bound cannot be evaluated against it: `above` latches permanently and `below`
    // fires on every reset. Rates come from the TSDB at query time (ADR-012). The engine also
    // refuses to evaluate counter samples, so this is the operator-facing half of one rule.
    let is_counter = is_builtin_counter(&body.metric)
        || admin
            .collection
            .metric_declared_counter(&body.metric)
            .await
            .map_err(|e| {
                ApiError::from_internal(
                    e.as_ref(),
                    "create threshold",
                    "failed to create threshold",
                )
            })?;
    if is_counter {
        return Err(ApiError::bad_request(
            "counter_metric",
            "the metric is a raw counter; its sampled value only ever increases, so a fixed threshold cannot be evaluated against it",
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

#[utoipa::path(
    delete, path = "/api/v1/thresholds/{id}", tag = "thresholds",
    params(("id" = Uuid, Path, description = "Threshold rule id")),
    responses(
        (status = 204, description = "Rule deleted"),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role below Admin", body = super::error::ErrorBody),
        (status = 404, description = "No such rule", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode has no write side", body = super::error::ErrorBody),
    ),
)]
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
    fn builtin_counters_are_rejected_gauges_and_unknowns_pass() {
        // The catalog's counters can never take a threshold — the sampled value is monotonic.
        assert!(is_builtin_counter("if_hc_in_octets"));
        assert!(is_builtin_counter("if_out_errors"));
        // sysUpTime is deliberately a gauge: reboot detection via `below` must keep working.
        assert!(!is_builtin_counter("snmp_sys_uptime_ticks"));
        // A name outside the catalog is not rejected here — the stored-item check owns it.
        assert!(!is_builtin_counter("icmp_rtt_ms"));
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
