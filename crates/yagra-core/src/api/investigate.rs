// SPDX-License-Identifier: AGPL-3.0-only
//! Investigate (ADR-251): confirm a hunch about one node, answered in the request.
//!
//! This file only gathers what the metric store cannot answer — the threshold rules in force for
//! the node, its ports and their speeds, its pool's monitoring gaps — and
//! [`crate::analysis::AnalysisRunner::investigate_running_out`] decides everything from them. REST
//! and the MCP `investigate_node` tool share [`running_out_of`].

use std::collections::HashMap;

use axum::extract::{Path, Query, State};
use axum::{routing::get, Json, Router};
use chrono::Utc;
use serde::Deserialize;
use uuid::Uuid;

use super::error::{ApiError, ApiResult};
use super::extract::{Admin, RequireView, VisibleNode};
use super::util::pool_resolver;
use super::{AdminState, ApiState};
use crate::analysis::{InvestigateInput, InvestigateReport, PortInfo};

/// The shortest and longest window a question may read, in days.
const DAYS_MIN: u32 = 7;
const DAYS_MAX: u32 = 90;
/// The window read when none is asked for.
const DAYS_DEFAULT: u32 = 30;

/// This domain's slice of the OpenAPI document (ADR-035), merged by [`super::openapi::document`].
#[derive(utoipa::OpenApi)]
#[openapi(paths(running_out))]
pub(super) struct Doc;

/// The Investigate routes, merged into `/api/v1` by [`super::router`].
pub(super) fn routes() -> Router<ApiState> {
    Router::new().route(
        "/api/v1/nodes/:node_id/investigate/running-out",
        get(running_out),
    )
}

/// `?days=` for an Investigate question.
#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub(crate) struct InvestigateQuery {
    /// How many days back to read, 7 to 90. Defaults to 30.
    pub days: Option<u32>,
}

/// Parse `?days=`. A value outside the range is refused rather than clamped, so the answer is
/// always about the window that was asked for.
pub(crate) fn parse_days(raw: Option<u32>) -> Result<u32, ApiError> {
    match raw {
        None => Ok(DAYS_DEFAULT),
        Some(d) if (DAYS_MIN..=DAYS_MAX).contains(&d) => Ok(d),
        Some(_) => Err(ApiError::bad_request(
            "invalid_days",
            format!("days must be between {DAYS_MIN} and {DAYS_MAX}"),
        )),
    }
}

#[utoipa::path(
    get, path = "/api/v1/nodes/{node_id}/investigate/running-out", tag = "analysis",
    params(("node_id" = Uuid, Path, description = "Node id"), InvestigateQuery),
    responses(
        (status = 200, description = "Whether the node's memory, disk, CPU or busiest links are running out: per hypothesis a verdict of hit, miss or unknown, and the series behind it. Each series is judged against the warning bound in force for the node and metric, else the critical one, else 100%. Unknown means too little data, never fine.", body = InvestigateReport),
        (status = 400, description = "`days` outside 7 to 90", body = super::error::ErrorBody),
        (status = 401, description = "Not signed in", body = super::error::ErrorBody),
        (status = 403, description = "Missing View", body = super::error::ErrorBody),
        (status = 404, description = "No such node, or the node is outside the caller's scope", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode: no database", body = super::error::ErrorBody),
    ),
)]
async fn running_out(
    _guard: RequireView,
    _visible: VisibleNode,
    State(st): State<ApiState>,
    admin: Admin,
    Path(node_id): Path<Uuid>,
    Query(q): Query<InvestigateQuery>,
) -> ApiResult<Json<InvestigateReport>> {
    let days = parse_days(q.days)?;
    Ok(Json(running_out_of(&st, &admin, node_id, days).await?))
}

/// "Is anything on this node running out?", shared by REST and the MCP `investigate_node` tool.
///
/// **The caller checks visibility first** (REST through `VisibleNode`, MCP through the scope it
/// resolved).
pub(crate) async fn running_out_of(
    st: &ApiState,
    admin: &AdminState,
    node_id: Uuid,
    days: u32,
) -> Result<InvestigateReport, ApiError> {
    let fail = |what: &'static str| {
        move |e: anyhow::Error| {
            ApiError::from_internal(e.as_ref(), what, "failed to investigate this node")
        }
    };
    let now = Utc::now();
    let node = admin
        .repo
        .get_node(node_id)
        .await
        .map_err(fail("investigate: load node"))?
        .ok_or_else(|| ApiError::not_found("node_not_found", format!("no node {node_id}")))?;

    let targets = target_lines(
        super::thresholds::node_thresholds(st, admin, node_id)
            .await?
            .iter()
            .filter(|m| m.in_force)
            .map(|m| &m.rule.rule),
    );
    let ports = admin
        .repo
        .list_interfaces(node_id)
        .await
        .map_err(fail("investigate: list interfaces"))?
        .into_iter()
        .map(|i| PortInfo {
            ifindex: i.ifindex,
            name: i.if_name.or(i.if_alias),
            speed_bps: i.if_speed,
        })
        .collect();
    let pool = pool_resolver(admin).await.resolve(&node).pool;
    let now_ms = now.timestamp_millis();
    let from_ms = now_ms - i64::from(days) * 86_400_000;
    let monitoring_gaps = admin
        .pollers
        .gaps_for_pool(&pool, from_ms, now_ms)
        .await
        .map_err(fail("investigate: monitoring gaps"))?;

    Ok(admin
        .analysis
        .investigate_running_out(&InvestigateInput {
            node_id,
            days,
            now_s: now.timestamp(),
            targets,
            ports,
            monitoring_gaps,
        })
        .await)
}

/// The line each metric is judged against: the lowest warning upper bound among the rules in
/// force, else the lowest critical one. Several rules can be in force for one metric (the engine
/// keeps the more restrictive bound of each), so the lowest is the one that trips first.
fn target_lines<'a>(
    rules: impl IntoIterator<Item = &'a yagra_common::ThresholdRule>,
) -> HashMap<String, f64> {
    let mut warning: HashMap<String, f64> = HashMap::new();
    let mut critical: HashMap<String, f64> = HashMap::new();
    for r in rules {
        if let Some(w) = r.warning_above {
            let e = warning.entry(r.metric.clone()).or_insert(w);
            *e = e.min(w);
        }
        if let Some(c) = r.critical_above {
            let e = critical.entry(r.metric.clone()).or_insert(c);
            *e = e.min(c);
        }
    }
    for (metric, c) in critical {
        warning.entry(metric).or_insert(c);
    }
    warning
}

#[cfg(test)]
mod tests {
    use super::*;
    use yagra_common::{ThresholdBounds, ThresholdRule};

    fn rule(metric: &str, warning: Option<f64>, critical: Option<f64>) -> ThresholdRule {
        ThresholdRule::new(
            metric,
            ThresholdBounds {
                warning_above: warning,
                critical_above: critical,
                ..ThresholdBounds::default()
            },
            1,
        )
    }

    #[test]
    fn the_target_is_the_lowest_warning_else_the_lowest_critical() {
        let rules = [
            rule("huawei_mem_usage", Some(85.0), Some(95.0)),
            rule("huawei_mem_usage", Some(80.0), None),
            rule("ucd_disk_used_pct", None, Some(90.0)),
            rule("ucd_disk_used_pct", None, Some(97.0)),
            rule("snmp_up", None, None),
        ];
        let t = target_lines(rules.iter());
        assert_eq!(t.get("huawei_mem_usage"), Some(&80.0));
        assert_eq!(t.get("ucd_disk_used_pct"), Some(&90.0));
        assert_eq!(
            t.get("snmp_up"),
            None,
            "a rule with no upper bound sets no line"
        );
    }

    #[test]
    fn days_outside_the_range_are_refused_not_clamped() {
        assert_eq!(parse_days(None).ok(), Some(30));
        assert_eq!(parse_days(Some(7)).ok(), Some(7));
        assert_eq!(parse_days(Some(90)).ok(), Some(90));
        assert!(parse_days(Some(6)).is_err());
        assert!(parse_days(Some(91)).is_err());
    }
}
