// SPDX-License-Identifier: AGPL-3.0-only
//! One node's availability graph (ADR-223): when it was down, in maintenance or not recorded over
//! the last 24 hours, 7, 30 or 90 days.
//!
//! One read. This file only gathers the facts — the node's liveness rows, the maintenance windows
//! that cover it, its pool's monitoring gaps, when it was added and how far back the alert history
//! reaches — and [`crate::alerts::availability::compute`] decides everything from them. REST and
//! the MCP `get_node_availability` tool share [`availability_of`].

use axum::extract::{Path, Query, State};
use axum::{routing::get, Json, Router};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use uuid::Uuid;

use super::error::{ApiError, ApiResult};
use super::extract::{Admin, RequireView, VisibleNode};
use super::util::{pool_resolver, tag_resolver};
use super::{AdminState, ApiState};
use crate::alerts::availability::{compute, AvailabilityPeriod, Inputs, NodeAvailability};
use crate::alerts::maintenance::{window_is_inherited_by, CoverageFacts, WindowScope};
use yagra_common::NodeId;

/// The most liveness rows read inside one period. Far above what one answer can show
/// ([`crate::alerts::availability::SPANS_MAX`] spans), so a flapping node is cut by the span cap,
/// where the cut is reported, rather than here, where it is not.
const LIVENESS_ROWS_MAX: i64 = 20_000;

/// This domain's slice of the OpenAPI document (ADR-035), merged by [`super::openapi::document`].
#[derive(utoipa::OpenApi)]
#[openapi(paths(node_availability))]
pub(super) struct Doc;

/// The availability route, merged into `/api/v1` by [`super::router`].
pub(super) fn routes() -> Router<ApiState> {
    Router::new().route(
        "/api/v1/nodes/:node_id/availability",
        get(node_availability),
    )
}

/// `?period=` for the availability graph.
#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub(crate) struct AvailabilityQuery {
    /// `24h`, `7d`, `30d` or `90d`. Defaults to `30d`.
    pub period: Option<String>,
}

/// Parse `?period=`, defaulting to 30 days. An unknown value is refused rather than defaulted, so
/// a typo does not answer a different question than the one asked.
pub(crate) fn parse_period(raw: Option<&str>) -> Result<AvailabilityPeriod, ApiError> {
    match raw {
        None => Ok(AvailabilityPeriod::Month),
        Some(token) => AvailabilityPeriod::from_token(token).ok_or_else(|| {
            ApiError::bad_request("invalid_period", "period must be one of 24h, 7d, 30d, 90d")
        }),
    }
}

#[utoipa::path(
    get, path = "/api/v1/nodes/{node_id}/availability", tag = "nodes",
    params(("node_id" = Uuid, Path, description = "Node id"), AvailabilityQuery),
    responses(
        (status = 200, description = "When the node was down, in maintenance or not recorded over the period, and the share of the rest it was up. Down means its liveness alert was open.", body = NodeAvailability),
        (status = 400, description = "Unknown `period`", body = super::error::ErrorBody),
        (status = 401, description = "Not signed in", body = super::error::ErrorBody),
        (status = 403, description = "Missing View, or the node is outside the caller's scope", body = super::error::ErrorBody),
        (status = 404, description = "No such node", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode: no database", body = super::error::ErrorBody),
    ),
)]
async fn node_availability(
    _guard: RequireView,
    _visible: VisibleNode,
    State(st): State<ApiState>,
    admin: Admin,
    Path(node_id): Path<Uuid>,
    Query(q): Query<AvailabilityQuery>,
) -> ApiResult<Json<NodeAvailability>> {
    let period = parse_period(q.period.as_deref())?;
    Ok(Json(
        availability_of(&st, &admin, node_id, period, Utc::now()).await?,
    ))
}

/// One node's availability, shared by REST and the MCP `get_node_availability` tool.
///
/// **The caller checks visibility first** (REST through `VisibleNode`, MCP through the scope it
/// resolved): the answer says when a node was down, which an out-of-scope caller must not learn.
pub(crate) async fn availability_of(
    st: &ApiState,
    admin: &AdminState,
    node_id: Uuid,
    period: AvailabilityPeriod,
    now: DateTime<Utc>,
) -> Result<NodeAvailability, ApiError> {
    let fail = |what: &'static str| {
        move |e: anyhow::Error| {
            ApiError::from_internal(e.as_ref(), what, "failed to read availability")
        }
    };
    let now_ms = now.timestamp_millis();
    let from_ms = now_ms - period.millis();

    let node = admin
        .repo
        .get_node(node_id)
        .await
        .map_err(fail("availability: load node"))?
        .ok_or_else(|| ApiError::not_found("node_not_found", format!("no node {node_id}")))?;
    let created_at_ms = admin
        .repo
        .created_at_ms(node_id)
        .await
        .map_err(fail("availability: node created_at"))?
        .ok_or_else(|| ApiError::not_found("node_not_found", format!("no node {node_id}")))?;

    let history = st
        .history
        .as_ref()
        .ok_or_else(ApiError::admin_unavailable)?;
    let check = yagra_alert::rules::check_id(NodeId(node_id), yagra_alert::rules::LIVENESS);
    let liveness = history
        .liveness_rows(check.as_uuid(), from_ms, LIVENESS_ROWS_MAX)
        .await
        .map_err(fail("availability: liveness rows"))?;

    let retention = admin.repo.get_retention_settings().await;
    let history_start_ms = now_ms - retention.alert_linked_secs() * 1000;

    let edges = admin
        .groups
        .edges()
        .await
        .map_err(fail("availability: group edges"))?;
    let facts = CoverageFacts::of(&node, &edges, &tag_resolver(admin).await);
    let node_text = node_id.to_string();
    let maintenance = admin
        .maintenance
        .list_windows()
        .await
        .map_err(fail("availability: maintenance windows"))?
        .into_iter()
        .filter(|w| w.enabled)
        .filter(|w| match w.level {
            WindowScope::Node => w.scope_id == node_text,
            WindowScope::Profile
            | WindowScope::Group
            | WindowScope::FolderGroup
            | WindowScope::System => window_is_inherited_by(w.level, &w.scope_id, &facts),
        })
        .filter_map(|w| {
            let start = DateTime::parse_from_rfc3339(&w.starts_at).ok()?;
            let end = DateTime::parse_from_rfc3339(&w.ends_at).ok()?;
            Some((start.timestamp_millis(), end.timestamp_millis()))
        })
        .collect();

    let pool = pool_resolver(admin).await.resolve(&node).pool;
    let monitoring_gaps = admin
        .pollers
        .gaps_for_pool(&pool, from_ms, now_ms)
        .await
        .map_err(fail("availability: monitoring gaps"))?;

    Ok(compute(
        node_id,
        period,
        now_ms,
        &Inputs {
            liveness,
            maintenance,
            monitoring_gaps,
            created_at_ms,
            history_start_ms,
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::tests_support::{private_state, send, token};
    use axum::http::StatusCode;
    use yagra_common::Role;

    #[test]
    fn the_period_defaults_to_thirty_days_and_refuses_an_unknown_value() {
        assert_eq!(parse_period(None).unwrap(), AvailabilityPeriod::Month);
        assert_eq!(parse_period(Some("7d")).unwrap(), AvailabilityPeriod::Week);
        assert_eq!(
            parse_period(Some("1y")).unwrap_err().code(),
            "invalid_period"
        );
    }

    fn uri() -> String {
        format!("/api/v1/nodes/{}/availability", Uuid::nil())
    }

    #[tokio::test]
    async fn an_anonymous_caller_is_refused_before_anything_is_read() {
        let st = private_state();
        let (status, _) = send(&st, "GET", &uri(), "", None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn skeleton_mode_answers_unavailable_to_a_viewer() {
        let st = private_state();
        let bearer = token(&st, Role::Viewer);
        let (status, _) = send(&st, "GET", &uri(), &bearer, None).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    }
}
