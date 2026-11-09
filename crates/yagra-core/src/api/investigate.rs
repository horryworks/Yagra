// SPDX-License-Identifier: AGPL-3.0-only
//! Investigate (ADR-251): confirm a hunch about one node, answered in the request.
//!
//! This file only gathers what the metric store cannot answer — the threshold rules in force for
//! the node, its ports and their speeds, its pool's monitoring gaps — and
//! [`crate::analysis::AnalysisRunner::investigate_running_out`] decides everything from them. REST
//! and the MCP `investigate_node` tool share [`running_out_of`].
//!
//! A folder (ADR-251 decision 12) is answered in two passes: a wide, shallow one over every node
//! ([`crate::analysis::AnalysisRunner::screen_running_out`]), then the single-node question for
//! the busiest [`DEEP_MAX`]. REST and the MCP `investigate_folder` tool share
//! [`folder_running_out_of`].

use std::collections::HashMap;

use axum::extract::{Path, Query, State};
use axum::{routing::get, Json, Router};
use chrono::Utc;
use serde::Deserialize;
use uuid::Uuid;

use super::error::{ApiError, ApiResult};
use super::extract::{Admin, RequireView, Scoped, VisibleNode};
use super::scope::NodeScope;
use super::util::pool_resolver_or_error;
use super::{AdminState, ApiState};
use crate::analysis::{
    rank_candidates, FolderInvestigateReport, InvestigateInput, InvestigateReport, PortInfo,
    DEEP_MAX, LINK_FLOOR_SHARE,
};

/// The shortest and longest window a question may read, in days.
const DAYS_MIN: u32 = 7;
const DAYS_MAX: u32 = 90;
/// The window read when none is asked for.
const DAYS_DEFAULT: u32 = 30;

/// This domain's slice of the OpenAPI document (ADR-035), merged by [`super::openapi::document`].
#[derive(utoipa::OpenApi)]
#[openapi(paths(running_out, folder_running_out))]
pub(super) struct Doc;

/// The Investigate routes, merged into `/api/v1` by [`super::router`].
pub(super) fn routes() -> Router<ApiState> {
    Router::new()
        .route(
            "/api/v1/nodes/:node_id/investigate/running-out",
            get(running_out),
        )
        .route(
            "/api/v1/node-groups/:id/investigate/running-out",
            get(folder_running_out),
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
    // Not the degraded `pool_resolver`: on a failed read it forgets folder inheritance, and the
    // gaps of the wrong pool would be presented as this node's (ADR-158 B4's reason).
    let pool = pool_resolver_or_error(admin).await?.resolve(&node).pool;
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

/// The most nodes one folder question reads, counting every folder beneath it — the same bound as
/// a folder's prefix-gap report, for the same reason: above it the answer is refused by name rather
/// than computed over a slice, because a partial answer would call the rest fine.
pub(crate) const MAX_FOLDER_NODES: usize = super::groups::MAX_PREFIX_GAP_NODES;
/// How many nodes are judged in full at once.
const JUDGE_CONCURRENCY: usize = 4;

#[utoipa::path(
    get, path = "/api/v1/node-groups/{id}/investigate/running-out", tag = "analysis",
    params(("id" = Uuid, Path, description = "Folder id"), InvestigateQuery),
    responses(
        (status = 200, description = "Whether anything is running out on the nodes of this folder and every folder beneath it. A first pass reads each node's memory, disk and CPU percentages now and a week ago, and its ports' current traffic; the nodes it puts at 50% or more in 30 days are judged exactly as the single-node question judges them, busiest first, at most 20. The counts say how many nodes were read, how many were candidates and how many candidates were not judged.", body = FolderInvestigateReport),
        (status = 400, description = "`days` outside 7 to 90, or `too_many_nodes`: the folder holds more than 2,000 nodes", body = super::error::ErrorBody),
        (status = 401, description = "Not signed in", body = super::error::ErrorBody),
        (status = 403, description = "Missing View", body = super::error::ErrorBody),
        (status = 404, description = "No such folder, or not one this caller may see", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode: no database", body = super::error::ErrorBody),
    ),
)]
async fn folder_running_out(
    _guard: RequireView,
    Scoped(scope): Scoped,
    State(st): State<ApiState>,
    admin: Admin,
    Path(id): Path<Uuid>,
    Query(q): Query<InvestigateQuery>,
) -> ApiResult<Json<FolderInvestigateReport>> {
    let days = parse_days(q.days)?;
    Ok(Json(
        folder_running_out_of(&st, &admin, &scope, id, days).await?,
    ))
}

/// "Is anything in this folder running out?", shared by REST and the MCP `investigate_folder`
/// tool. Checks the folder is visible to `scope` itself.
pub(crate) async fn folder_running_out_of(
    st: &ApiState,
    admin: &AdminState,
    scope: &NodeScope,
    id: Uuid,
    days: u32,
) -> Result<FolderInvestigateReport, ApiError> {
    use futures::stream::{self, StreamExt, TryStreamExt};
    use yagra_base::groups::group_subtree;

    let fail = |what: &'static str| {
        move |e: anyhow::Error| {
            ApiError::from_internal(e.as_ref(), what, "failed to investigate this folder")
        }
    };
    super::scope::require_visible_group(scope, id)?;
    let groups = admin
        .groups
        .list()
        .await
        .map_err(fail("investigate: list node groups"))?;
    if !groups.iter().any(|g| g.id == id) {
        return Err(ApiError::not_found("group_not_found", "no such node group"));
    }
    let edges: Vec<(Uuid, Option<Uuid>)> = groups.iter().map(|g| (g.id, g.parent_id)).collect();
    let nodes = admin
        .repo
        .nodes_in_groups(&group_subtree(&edges, id))
        .await
        .map_err(fail("investigate: list folder nodes"))?;
    if nodes.len() > MAX_FOLDER_NODES {
        return Err(ApiError::bad_request(
            "too_many_nodes",
            format!(
                "this folder holds {} nodes; one question reads at most {MAX_FOLDER_NODES}",
                nodes.len()
            ),
        ));
    }

    // Ports are worth a look from half the folder's slowest usable speed: below that, no port in
    // the folder can be half full. No usable speed anywhere means ports are not read at all.
    #[allow(clippy::cast_precision_loss)] // A port speed is far below 2^52 bits/sec.
    let link_floor = admin
        .repo
        .slowest_interface_speed_bps(&nodes)
        .await
        .map_err(fail("investigate: slowest port"))?
        .map(|s| s as f64 * LINK_FLOOR_SHARE);
    let screen = admin.analysis.screen_running_out(&nodes, link_floor).await;
    let busy_nodes: Vec<Uuid> = screen
        .busy_ports
        .iter()
        .map(|(n, _, _)| *n)
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    let speeds: HashMap<(Uuid, i32), Option<i64>> = admin
        .repo
        .interface_idents_for(&busy_nodes)
        .await
        .map_err(fail("investigate: port speeds"))?
        .into_iter()
        .map(|(k, v)| (k, v.if_speed))
        .collect();
    let ranked = rank_candidates(&screen, &speeds);

    // A node deleted between the two passes is dropped rather than failing the whole answer.
    let to_judge: Vec<Uuid> = ranked.iter().take(DEEP_MAX).map(|(n, _)| *n).collect();
    let judged: Vec<InvestigateReport> = stream::iter(to_judge)
        .map(|node| async move {
            match running_out_of(st, admin, node, days).await {
                Ok(r) => Ok(Some(r)),
                Err(e) if e.status() == axum::http::StatusCode::NOT_FOUND => Ok(None),
                Err(e) => Err(e),
            }
        })
        .buffered(JUDGE_CONCURRENCY)
        .try_filter_map(|r| async move { Ok(r) })
        .try_collect()
        .await?;

    let now = Utc::now().timestamp();
    let mut report = FolderInvestigateReport {
        question: crate::analysis::Question::RunningOut,
        group_id: id,
        from: now - i64::from(days) * 86_400,
        to: now,
        nodes_total: nodes.len(),
        nodes_with_data: screen.nodes_with_data.len(),
        candidates: ranked.len(),
        judged: judged.len(),
        not_judged: ranked.len().saturating_sub(DEEP_MAX),
        links_screened: screen.links_screened,
        nodes: judged,
    };
    report.sort_nodes();
    Ok(report)
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

    /// The folder question is answered — counted over the folder and its subfolders — and a
    /// folder the caller cannot name is a 404, not an empty answer. The live store is a stand-in
    /// with no history, so nothing is a candidate and nothing is judged.
    #[sqlx::test(migrator = "yagra_base::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_folder_is_answered_over_its_whole_subtree(pool: sqlx::PgPool) {
        use crate::api::tests_support::{live_state, send, token};
        let st = live_state(pool.clone()).await;
        let tok = token(&st, yagra_common::Role::Viewer);
        let site = yagra_base::pgtest::group(&pool, "site-a").await;
        let floor = yagra_base::groups::GroupRepo::new(pool.clone())
            .create(
                "floor-1",
                yagra_base::groups::GroupType::Site,
                Some(site),
                None,
            )
            .await
            .expect("create the floor");
        yagra_base::pgtest::node(&pool, "sw-a", 1, Some(site)).await;
        yagra_base::pgtest::node(&pool, "sw-b", 2, Some(floor)).await;
        yagra_base::pgtest::node(&pool, "elsewhere", 3, None).await;

        let (status, body) = send(
            &st,
            "GET",
            &format!("/api/v1/node-groups/{site}/investigate/running-out"),
            &tok,
            None,
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::OK, "{body}");
        assert_eq!(
            body["nodes_total"], 2,
            "the floor's node counts, the other does not: {body}"
        );
        assert_eq!(body["question"], "running_out");
        assert_eq!(body["judged"], 0, "{body}");
        assert_eq!(body["nodes"], serde_json::json!([]));

        let (status, _) = send(
            &st,
            "GET",
            &format!(
                "/api/v1/node-groups/{}/investigate/running-out",
                Uuid::new_v4()
            ),
            &tok,
            None,
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::NOT_FOUND);
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
