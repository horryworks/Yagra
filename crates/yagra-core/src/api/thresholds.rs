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
    routing::{get, put},
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
#[openapi(paths(list_thresholds, create_threshold, update_threshold, delete_threshold))]
pub(super) struct Doc;

/// The threshold routes, merged into `/api/v1` by [`super::router`].
pub(super) fn routes() -> Router<ApiState> {
    Router::new()
        .route(
            "/api/v1/thresholds",
            get(list_thresholds).post(create_threshold),
        )
        .route(
            "/api/v1/thresholds/:id",
            put(update_threshold).delete(delete_threshold),
        )
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
/// The two enums are parsed at the edge, and an unknown token is a `400` rather than a filter
/// silently dropped. Dropping one would *widen* the answer — the operator asked for node-level
/// rules and would be shown every rule in the fleet, believing the narrower thing.
///
/// Since ADR-053 Inc.4b they are comma-separated strings rather than typed serde fields, so several
/// values can be named at once; the parse moved from serde to [`super::util::parse_set`]. The
/// rejection property is unchanged, which is the half that matters.
#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub(super) struct ThresholdQuery {
    limit: Option<i64>,
    /// Case-insensitive substring of the metric name.
    q: Option<String>,
    /// Comma-separated scope levels (`global` | `profile` | `group` | `node`); empty or absent
    /// means every level.
    scope_level: Option<String>,
    /// Comma-separated directions (`above` | `below`); empty or absent means both.
    direction: Option<String>,
}

#[utoipa::path(
    get, path = "/api/v1/thresholds", tag = "thresholds",
    params(ThresholdQuery),
    responses(
        (status = 200, description = "A capped page of matching rules, with the matching total", body = ThresholdPage),
        (status = 400, description = "`scope_level` or `direction` names a value outside its vocabulary", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig — the ruleset decides when the fleet pages someone", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode has no write side", body = super::error::ErrorBody),
    ),
)]
async fn list_thresholds(
    _guard: RequireManageConfig,
    Query(q): Query<ThresholdQuery>,
    admin: Admin,
) -> ApiResult<Json<ThresholdPage>> {
    let metric = normalize_search(q.q.as_deref());
    let levels = super::util::parse_set(
        "scope_level",
        q.scope_level.as_deref(),
        "global, profile, group or node",
        yagra_common::ScopeLevel::from_token,
    )?;
    let directions = super::util::parse_set(
        "direction",
        q.direction.as_deref(),
        "above or below",
        yagra_common::Direction::from_token,
    )?;
    Ok(Json(
        threshold_page(
            &admin,
            q.limit,
            &crate::thresholds::ThresholdFilter {
                metric: metric.as_deref(),
                level: &levels,
                direction: &directions,
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

/// A threshold rule as the caller states it — the body of both `POST` and `PUT`.
///
/// One type for both, the shape `ProfileBody` already uses. Two would be two validators, and this
/// repo has paid for that: URL-check and DNS-check CRUD were two copies of one writer, so the "a
/// node is exactly one kind" rule shipped enforced on only one of them.
#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct ThresholdBody {
    scope_level: String,
    scope_id: String,
    metric: String,
    direction: String,
    warning: Option<f64>,
    critical: Option<f64>,
    dwell_samples: Option<i32>,
}

/// A body that passed the checks that need no I/O, with `scope_id` already normalized.
#[derive(Debug)]
struct ParsedThreshold<'a> {
    scope_level: &'a str,
    scope_id: &'a str,
    direction: &'a str,
}

/// The synchronous half of validating a rule — shared by create and update.
///
/// The counter check is **not** here: it reads the stored collection items, so it is async and
/// stays in the handlers, which call it in the same order on both paths.
fn parse_threshold_body(body: &ThresholdBody) -> ApiResult<ParsedThreshold<'_>> {
    if !is_valid_metric_name(&body.metric) {
        return Err(ApiError::bad_request(
            "invalid_metric_name",
            "metric must be a valid identifier",
        ));
    }
    if !matches!(
        body.scope_level.as_str(),
        "global" | "profile" | "group" | "node"
    ) || !matches!(body.direction.as_str(), "above" | "below")
    {
        return Err(ApiError::bad_request(
            "invalid_threshold",
            "scope_level must be global|profile|group|node and direction above|below",
        ));
    }
    // A `global` rule targets the whole fleet, so it has nothing to point at (ADR-075). Pinning
    // the id here rather than trusting the caller is what keeps `AlertConfig::applies` able to
    // ignore the column for this level: two global rules that differed only in a stray scope id
    // would both apply, look identical in the list, and be impossible to tell apart.
    let scope_id = if body.scope_level == "global" {
        ""
    } else {
        body.scope_id.as_str()
    };
    Ok(ParsedThreshold {
        scope_level: &body.scope_level,
        scope_id,
        direction: &body.direction,
    })
}

/// Whether `metric` may not carry a threshold because its samples only ever increase.
///
/// A raw counter's sampled value rises until it wraps or the device reboots, so a fixed bound
/// cannot be evaluated against it: `above` latches permanently and `below` fires on every reset.
/// Rates come from the TSDB at query time (ADR-012). The engine also refuses to evaluate counter
/// samples, so this is the operator-facing half of one rule — and it is a `SELECT`, which is why
/// it is not part of [`parse_threshold_body`].
async fn reject_counter_metric(admin: &super::AdminState, metric: &str, op: &str) -> ApiResult<()> {
    let is_counter = is_builtin_counter(metric)
        || admin
            .collection
            .metric_declared_counter(metric)
            .await
            .map_err(|e| ApiError::from_internal(e.as_ref(), op, "failed to save threshold"))?;
    if is_counter {
        return Err(ApiError::bad_request(
            "counter_metric",
            "the metric is a raw counter; its sampled value only ever increases, so a fixed threshold cannot be evaluated against it",
        ));
    }
    Ok(())
}

#[utoipa::path(
    post, path = "/api/v1/thresholds", tag = "thresholds",
    request_body = ThresholdBody,
    responses(
        (status = 201, description = "Rule created", body = CreatedId),
        (status = 400, description = "The metric is not an identifier, scope_level/direction is outside its vocabulary, or the metric is a raw counter (a monotonic value has no meaningful fixed bound). A `global` rule ignores `scope_id` — it targets every node", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode has no write side", body = super::error::ErrorBody),
    ),
)]
async fn create_threshold(
    _guard: RequireManageConfig,
    admin: Admin,
    Json(body): Json<ThresholdBody>,
) -> ApiResult<(StatusCode, Json<CreatedId>)> {
    let p = parse_threshold_body(&body)?;
    reject_counter_metric(&admin, &body.metric, "create threshold").await?;
    let id = admin
        .thresholds
        .create(
            p.scope_level,
            p.scope_id,
            &body.metric,
            p.direction,
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
    put, path = "/api/v1/thresholds/{id}", tag = "thresholds",
    params(("id" = Uuid, Path, description = "Threshold rule id")),
    request_body = ThresholdBody,
    responses(
        (status = 204, description = "Rule updated"),
        (status = 400, description = "The metric is not an identifier, scope_level/direction is outside its vocabulary, or the metric is a raw counter. A `global` rule ignores `scope_id` — it targets every node", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
        (status = 404, description = "No such rule", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode has no write side", body = super::error::ErrorBody),
    ),
)]
async fn update_threshold(
    _guard: RequireManageConfig,
    admin: Admin,
    Path(id): Path<Uuid>,
    Json(body): Json<ThresholdBody>,
) -> ApiResult<StatusCode> {
    let p = parse_threshold_body(&body)?;
    reject_counter_metric(&admin, &body.metric, "update threshold").await?;
    let updated = admin
        .thresholds
        .update(
            id,
            p.scope_level,
            p.scope_id,
            &body.metric,
            p.direction,
            body.warning,
            body.critical,
            body.dwell_samples.unwrap_or(3),
        )
        .await
        .map_err(|e| {
            ApiError::from_internal(e.as_ref(), "update threshold", "failed to update threshold")
        })?;
    if updated {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found(
            "threshold_not_found",
            format!("no threshold {id}"),
        ))
    }
}

#[utoipa::path(
    delete, path = "/api/v1/thresholds/{id}", tag = "thresholds",
    params(("id" = Uuid, Path, description = "Threshold rule id")),
    responses(
        (status = 204, description = "Rule deleted"),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
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
            ("PUT", format!("/api/v1/thresholds/{ID}")),
            ("DELETE", format!("/api/v1/thresholds/{ID}")),
        ]
    }

    async fn status_of(st: ApiState, method: &str, path: &str, token: Option<&str>) -> StatusCode {
        let mut b = Request::builder().method(method).uri(path);
        if let Some(t) = token {
            b = b.header(AUTHORIZATION, format!("Bearer {t}"));
        }
        let body = if method == "POST" || method == "PUT" {
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
    async fn reading_thresholds_needs_manage_config_even_on_a_public_dashboard() {
        // Unlike most reads, the list is `ManageConfig`, not `View`: the ruleset is what decides
        // when the fleet pages someone, and a public dashboard must not expose it.
        assert_eq!(
            status_of(public_state(), "GET", "/api/v1/thresholds", None).await,
            StatusCode::UNAUTHORIZED,
        );
        // A viewer is refused the ruleset entirely; an operator owns it (ADR-057).
        let st = private_state();
        for (role, want) in [
            (Role::Viewer, StatusCode::FORBIDDEN),
            (Role::Operator, StatusCode::SERVICE_UNAVAILABLE),
        ] {
            let token = st
                .sessions
                .issue(Uuid::new_v4(), Principal::new(role, Scope::All), "u");
            for (method, path) in routes_under_test() {
                // 403, not 401 — "not allowed" and "not logged in" are different fixes.
                assert_eq!(
                    status_of(st.clone(), method, &path, Some(&token)).await,
                    want,
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

    fn body(scope_level: &str, scope_id: &str, metric: &str, direction: &str) -> ThresholdBody {
        ThresholdBody {
            scope_level: scope_level.to_owned(),
            scope_id: scope_id.to_owned(),
            metric: metric.to_owned(),
            direction: direction.to_owned(),
            warning: None,
            critical: None,
            dwell_samples: None,
        }
    }

    #[test]
    fn a_global_rule_has_its_scope_id_pinned_empty_and_every_other_level_keeps_what_was_sent() {
        // The pinning half: a stray id on a global rule would produce two rules that both apply to
        // every node, look identical in the list, and cannot be told apart.
        let b = body("global", "not-a-real-id", "icmp_rtt_ms", "above");
        assert_eq!(parse_threshold_body(&b).unwrap().scope_id, "");
        // The receiving half, and it is the one that makes the test mean something: a normalizer
        // that emptied every scope id would pass the assertion above while silently making every
        // profile/group/node rule fleet-wide.
        for level in ["profile", "group", "node"] {
            let b = body(level, "keep-me", "icmp_rtt_ms", "above");
            let p = parse_threshold_body(&b).unwrap();
            assert_eq!(p.scope_id, "keep-me", "{level}");
            assert_eq!(p.scope_level, level);
        }
    }

    #[test]
    fn a_token_outside_either_vocabulary_is_refused_rather_than_quietly_defaulted() {
        // Dropping an unknown token would *widen* the rule: `parse_level` on the read side falls
        // back to `Profile`, so a rule stored with a level nobody admits would come back as a
        // profile rule with an empty scope id — inert, and indistinguishable from a typo.
        for (level, dir) in [
            ("galaxy", "above"),
            ("global", "sideways"),
            ("", "above"),
            ("node", ""),
        ] {
            let b = body(level, "x", "icmp_rtt_ms", dir);
            let err = parse_threshold_body(&b).unwrap_err();
            assert!(
                format!("{err:?}").contains("invalid_threshold"),
                "{level}/{dir}"
            );
        }
        // A metric name that is not an identifier is refused by its own code, so the operator is
        // told which field is wrong.
        let b = body("node", "x", "not a metric", "above");
        let err = parse_threshold_body(&b).unwrap_err();
        assert!(format!("{err:?}").contains("invalid_metric_name"));
    }

    #[test]
    fn create_and_update_run_the_same_two_checks_in_the_same_order() {
        // Two writers of one rule is how the DNS/URL CRUD pair shipped with the "a node is exactly
        // one kind" guard on only one of them (extensibility.md §3). Both handlers must call the
        // shared validator *and* the counter check, and the counter check must come second — it is
        // a database round trip, and a malformed body should not cost one.
        const SRC: &str = include_str!("thresholds.rs");
        for handler in ["async fn create_threshold", "async fn update_threshold"] {
            let after = SRC.split_once(handler).expect("handler exists").1;
            let body = after.split_once("\n}").map_or(after, |(b, _)| b);
            let parse = body.find("parse_threshold_body(&body)").expect(handler);
            let counter = body.find("reject_counter_metric(").expect(handler);
            assert!(parse < counter, "{handler} must validate before it queries");
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
