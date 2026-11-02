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
        (status = 200, description = "When the node was down, in maintenance or not recorded over the period, and the share of the rest it was up. Down means its liveness alert was open. While no liveness rule reaches the node, or its pool has no live poller, the time it was not down is not recorded rather than up, and there is no share.", body = NodeAvailability),
        (status = 400, description = "Unknown `period`", body = super::error::ErrorBody),
        (status = 401, description = "Not signed in", body = super::error::ErrorBody),
        (status = 403, description = "Missing View", body = super::error::ErrorBody),
        (status = 404, description = "No such node, or the node is outside the caller's scope", body = super::error::ErrorBody),
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

    // Whether anything would have recorded an outage (ADR-223 decision 8): a liveness rule that
    // reaches the node, and a live poller in its pool. Without either, a quiet history is silence.
    let liveness_rules: Vec<_> = admin
        .thresholds
        .list_all()
        .await
        .map_err(fail("availability: threshold rules"))?
        .into_iter()
        .filter(|r| r.rule.metric == yagra_alert::rules::LIVENESS)
        .collect();
    let ruled = st
        .alerts
        .matching_rules(&liveness_rules, NodeId(node_id), None)
        .iter()
        .any(|(_, in_force)| *in_force);

    let pool = pool_resolver(admin).await.resolve(&node).pool;
    let polled = admin
        .coordinator
        .live_pools(std::time::Instant::now())
        .contains(&pool);
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
            unobserved: !ruled || !polled,
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

    /// The read end to end over a real database: the node's liveness rows become one outage, and
    /// the time before the node was added is not recorded (ADR-223).
    #[sqlx::test(migrator = "yagra_base::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_viewer_reads_one_outage_and_the_time_before_the_node_existed(pool: sqlx::PgPool) {
        use crate::api::tests_support::live_state;
        use yagra_alert::{Alert, Subject};
        use yagra_common::{NodeState, Severity};

        let st = live_state(pool.clone()).await;
        let node = yagra_base::pgtest::node(&pool, "sw-01", 1, None).await;
        const H: i64 = 3_600_000;
        let now = Utc::now().timestamp_millis();
        sqlx::query("UPDATE nodes SET created_at = to_timestamp($2 / 1000.0) WHERE id = $1")
            .bind(node)
            .bind((now - 12 * H) as f64)
            .execute(&pool)
            .await
            .expect("created_at");
        let fired = now - 5 * H;
        let liveness = Alert {
            subject: Subject::Node(NodeId::from(node)),
            check: yagra_alert::rules::check_id(NodeId::from(node), yagra_alert::rules::LIVENESS),
            severity: Severity::Critical,
            state: NodeState::Unreachable,
            at_unix_ms: fired,
            metric: yagra_alert::rules::LIVENESS.to_owned(),
            breach: None,
            flapping: false,
            root_cause: None,
            ifindex: None,
            row: None,
            row_name: None,
        };
        crate::alerts::history::AlertHistoryStore::new(pool.clone())
            .record_batch(&[(liveness.clone(), false), (liveness, true)])
            .await
            .expect("record");
        // The writer stamps `now()`; the clear is recorded an hour after the fire.
        sqlx::query(
            "UPDATE alert_history SET recorded_at = to_timestamp(\
             (at_unix_ms + CASE WHEN resolved THEN 3600000 ELSE 0 END) / 1000.0)",
        )
        .execute(&pool)
        .await
        .expect("stamp");

        let viewer = token(&st, Role::Viewer);
        let uri = format!("/api/v1/nodes/{node}/availability?period=24h");

        // Nothing watches it yet: the seeded liveness rule is not there and no poller is live,
        // so the quiet hours are not recorded rather than up (ADR-223 decision 8).
        let (status, body) = send(&st, "GET", &uri, &viewer, None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["up_secs"], 0, "{body}");
        assert!(body["availability_pct"].is_null(), "{body}");

        // The built-in rules, and a live poller in the node's pool.
        let admin = st.admin.as_ref().expect("live state");
        admin.repo.seed_builtin_profiles().await.expect("seed");
        sqlx::query("INSERT INTO pollers (id, pool) VALUES ('edge-1', 'default')")
            .execute(&pool)
            .await
            .expect("register the poller");
        admin
            .coordinator
            .observe_heartbeat(
                yagra_bus::HeartbeatMsg {
                    poller_id: "edge-1".to_owned(),
                    pool: "default".to_owned(),
                    incarnation: Uuid::new_v4(),
                    version: "0.3.66".to_owned(),
                    epoch: None,
                    last_seq: 0,
                    working_set_nodes: 0,
                    working_set_specs: 0,
                    inflight: 0,
                    results_total: 0,
                    listeners: Vec::new(),
                    caps: Vec::new(),
                    host: None,
                    leaving: false,
                    mgmt_addrs: Vec::new(),
                    upgrade: None,
                },
                std::time::Instant::now(),
            )
            .await;
        let (status, body) = send(&st, "GET", &uri, &viewer, None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["period"], "24h");
        assert_eq!(body["outages"], 1, "{body}");
        assert_eq!(body["down_secs"], 3_600, "{body}");
        // The handler reads its own clock a few milliseconds after this test read its.
        let no_record = body["no_record_secs"].as_i64().expect("no_record_secs");
        assert!((12 * 3_600 - 1..=12 * 3_600).contains(&no_record), "{body}");
        let kinds: Vec<&str> = body["spans"]
            .as_array()
            .expect("spans")
            .iter()
            .map(|s| s["kind"].as_str().unwrap_or(""))
            .collect();
        assert_eq!(kinds, ["no_record", "down"], "{body}");
    }

    #[tokio::test]
    async fn skeleton_mode_answers_unavailable_to_a_viewer() {
        let st = private_state();
        let bearer = token(&st, Role::Viewer);
        let (status, _) = send(&st, "GET", &uri(), &bearer, None).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    }
}
