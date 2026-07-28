// SPDX-License-Identifier: AGPL-3.0-only
//! Troubleshoot analysis jobs (ADR-022).
//!
//! An analysis is a background read over the metric / passive-event / flow stores that produces
//! findings. It changes nothing about the fleet, so the only side effect it can have is the
//! optional notification — which is why `notify` is an explicit field of [`AnalysisRequest`]
//! rather than a default: the MCP surface must always pass `false`, and a default would let that
//! invariant be lost by omission.
//!
//! [`job_params`] is the seam. Launching a run means validating a tool token, validating a scope,
//! requiring a scope id for group/node, and clamping four numeric knobs — and all of that existed
//! twice, once per surface, with the clamps written out separately in each. Two copies of a
//! *validation* rule is the worst kind to keep, because when they drift the looser one is the
//! security boundary.

use super::extract::{Admin, RequireManageConfig, RequireView};
use super::{ApiError, ApiResult, ApiState, HistoryQuery};
use crate::analysis::{AnalysisJob, AnalysisTool, CreateError, JobParams, ScopeKind};
use axum::{
    extract::{Path, Query, State},
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Response,
    },
    routing::{get, post},
    Json, Router,
};
use futures::stream::StreamExt;
use serde::{Deserialize, Serialize};
use std::convert::Infallible;
use uuid::Uuid;

/// The analysis routes, merged into `/api/v1` by [`super::router`].
pub(crate) fn routes() -> Router<ApiState> {
    Router::new()
        .route(
            "/api/v1/analysis/jobs",
            get(list_analysis_jobs).post(create_analysis_job),
        )
        .route("/api/v1/analysis/jobs/:id", get(get_analysis_job))
        .route("/api/v1/analysis/jobs/:id/findings", get(analysis_findings))
        .route(
            "/api/v1/analysis/jobs/:id/cancel",
            post(cancel_analysis_job),
        )
        .route("/api/v1/stream/analysis", get(stream_analysis))
}

// ── The shared launch path ───────────────────────────────────────────────────

/// Default baseline window: two weeks of history to compare the run window against.
const DEFAULT_BASELINE_SECS: i64 = 14 * 86_400;
/// Analysis windows below five minutes have too few samples to say anything; above a year the
/// query cost stops being bounded.
const WINDOW_BOUNDS: (i64, i64) = (300, 365 * 86_400);
const BASELINE_BOUNDS: (i64, i64) = (3600, 365 * 86_400);
/// Sensitivity is a sigma multiplier: below 0.5 everything is an anomaly, above 6 nothing is.
const SENSITIVITY_BOUNDS: (f64, f64) = (0.5, 6.0);

/// An unvalidated request to launch an analysis, as either surface receives it.
///
/// `window_secs` is already resolved because the two surfaces disagree on it deliberately — the
/// REST body requires it, the MCP tool defaults it — while every other default and every bound is
/// shared.
pub(crate) struct AnalysisRequest {
    pub tool: String,
    pub scope_kind: String,
    pub scope_id: Option<Uuid>,
    pub scope_label: String,
    pub window_secs: i64,
    pub baseline_secs: Option<i64>,
    pub sensitivity: Option<f64>,
    pub depth: Option<String>,
    pub family: Option<String>,
    /// Whether a completed run may notify. **Always `false` from a read-only surface.**
    pub notify: bool,
}

/// Validate and clamp a launch request into runner parameters.
///
/// Every numeric knob is clamped rather than rejected (defence in depth at the edge — security.md):
/// an out-of-range window is an over-eager client, not an attack, and silently bounding it beats
/// both trusting it and failing the run.
pub(crate) fn job_params(req: AnalysisRequest) -> Result<JobParams, ApiError> {
    let tool = AnalysisTool::from_str(&req.tool).ok_or_else(|| {
        ApiError::bad_request(
            "invalid_tool",
            format!(
                "unknown analysis tool {:?}; must be one of: {}",
                req.tool,
                AnalysisTool::token_list()
            ),
        )
    })?;
    let scope_kind = ScopeKind::from_str(&req.scope_kind).ok_or_else(|| {
        ApiError::bad_request(
            "invalid_scope",
            format!(
                "scope_kind must be all|group|node, got {:?}",
                req.scope_kind
            ),
        )
    })?;
    if scope_kind != ScopeKind::All && req.scope_id.is_none() {
        return Err(ApiError::bad_request(
            "missing_scope_id",
            "scope_id is required for group/node scope",
        ));
    }
    Ok(JobParams {
        tool,
        scope_kind,
        scope_id: req.scope_id,
        scope_label: req.scope_label,
        window_secs: req.window_secs.clamp(WINDOW_BOUNDS.0, WINDOW_BOUNDS.1),
        baseline_secs: req
            .baseline_secs
            .unwrap_or(DEFAULT_BASELINE_SECS)
            .clamp(BASELINE_BOUNDS.0, BASELINE_BOUNDS.1),
        sensitivity: req
            .sensitivity
            .unwrap_or(3.0)
            .clamp(SENSITIVITY_BOUNDS.0, SENSITIVITY_BOUNDS.1),
        depth: req.depth.unwrap_or_else(|| "standard".to_owned()),
        family: req.family.unwrap_or_else(|| "all".to_owned()),
        notify: req.notify,
    })
}

/// Map a runner admission decision onto the API error vocabulary.
///
/// Capacity and rate rejections are **retryable**, so they are `429` and not `500`; a client that
/// cannot tell them apart either hammers a full queue or gives up on a transient refusal.
pub(crate) fn create_error(err: CreateError) -> ApiError {
    match err {
        e @ (CreateError::TooManyConcurrent(_) | CreateError::RateLimited(_)) => {
            ApiError::too_many_requests("analysis_busy", e.to_string())
        }
        CreateError::Internal(e) => ApiError::from_internal(
            e.as_ref(),
            "create analysis job",
            "failed to create analysis job",
        ),
    }
}

/// Launch a run: validate, clamp, then hand off to the runner. The row comes back immediately and
/// progresses over SSE.
pub(crate) async fn launch(
    admin: &super::AdminState,
    req: AnalysisRequest,
    user: Option<String>,
) -> Result<AnalysisJob, ApiError> {
    let params = job_params(req)?;
    admin
        .analysis
        .create(params, user)
        .await
        .map_err(create_error)
}

/// One job plus its findings — what a "poll a run" caller actually wants.
///
/// Findings exist only on a successful run; a failed or cancelled job carries its state and error
/// instead, and returning an empty list for it is correct rather than an omission.
///
/// Deliberately **not** `Serialize`: it holds internal models, and the MCP surface must project it
/// through `mcp::dto` (the ADR-018 enforcement boundary) rather than serialize it directly. Making
/// that a compile error rather than a convention is cheaper than trusting it.
#[derive(Debug)]
pub(crate) struct AnalysisReport {
    pub job: AnalysisJob,
    pub findings: Vec<crate::analysis::AnalysisFinding>,
}

/// Load a job and, if it succeeded, its findings.
pub(crate) async fn report(
    admin: &super::AdminState,
    job_id: Uuid,
) -> Result<AnalysisReport, ApiError> {
    let job = admin
        .analysis
        .get(job_id)
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "get analysis job",
                "failed to load analysis job",
            )
        })?
        .ok_or_else(|| ApiError::not_found("job_not_found", format!("no analysis job {job_id}")))?;
    let findings = if job.state == "done" {
        admin.analysis.findings(job_id).await.map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "list analysis findings",
                "failed to load findings",
            )
        })?
    } else {
        Vec::new()
    };
    Ok(AnalysisReport { job, findings })
}

// ── Handlers ─────────────────────────────────────────────────────────────────

/// Recent analysis jobs (the runs list). `?limit=` (default 50).
///
/// Skeleton mode has no runner, so this answers an empty list rather than a 503: the runs list is
/// a panel on a page that otherwise works, and an error there would break the page.
async fn list_analysis_jobs(
    _perm: RequireView,
    State(st): State<ApiState>,
    Query(q): Query<HistoryQuery>,
) -> ApiResult<Json<Vec<AnalysisJob>>> {
    let Some(admin) = st.admin.as_ref() else {
        return Ok(Json(Vec::new()));
    };
    let limit = q.limit.unwrap_or(50).clamp(1, 200);
    let jobs = admin.analysis.list(limit).await.map_err(|e| {
        ApiError::from_internal(
            e.as_ref(),
            "list analysis jobs",
            "failed to list analysis jobs",
        )
    })?;
    Ok(Json(jobs))
}

/// One analysis job by id.
async fn get_analysis_job(
    _perm: RequireView,
    State(st): State<ApiState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<AnalysisJob>> {
    // Skeleton mode has no job store, so no id exists — the same answer as an unknown id, and the
    // one that tells an anonymous-but-permitted caller least about the deployment.
    let admin = st
        .admin
        .as_ref()
        .ok_or_else(|| ApiError::not_found("job_not_found", format!("no analysis job {id}")))?;
    let job = admin
        .analysis
        .get(id)
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "get analysis job",
                "failed to load analysis job",
            )
        })?
        .ok_or_else(|| ApiError::not_found("job_not_found", format!("no analysis job {id}")))?;
    Ok(Json(job))
}

/// A job's findings (the report list). Empty in skeleton mode.
async fn analysis_findings(
    _perm: RequireView,
    State(st): State<ApiState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Vec<crate::analysis::AnalysisFinding>>> {
    let Some(admin) = st.admin.as_ref() else {
        return Ok(Json(Vec::new()));
    };
    let findings = admin.analysis.findings(id).await.map_err(|e| {
        ApiError::from_internal(
            e.as_ref(),
            "list analysis findings",
            "failed to load findings",
        )
    })?;
    Ok(Json(findings))
}

/// Request body to launch an analysis (launch drawer / report config bar).
#[derive(Deserialize)]
struct CreateAnalysisJob {
    tool: String,
    scope_kind: String,
    scope_id: Option<Uuid>,
    scope_label: String,
    window_secs: i64,
    baseline_secs: Option<i64>,
    sensitivity: Option<f64>,
    depth: Option<String>,
    family: Option<String>,
    notify: Option<bool>,
}

/// Launch a background analysis job (operator+).
async fn create_analysis_job(
    _perm: RequireManageConfig,
    admin: Admin,
    State(st): State<ApiState>,
    headers: axum::http::HeaderMap,
    Json(body): Json<CreateAnalysisJob>,
) -> ApiResult<Json<AnalysisJob>> {
    let user = super::current_username(&st, &headers);
    let req = AnalysisRequest {
        tool: body.tool,
        scope_kind: body.scope_kind,
        scope_id: body.scope_id,
        scope_label: body.scope_label,
        window_secs: body.window_secs,
        baseline_secs: body.baseline_secs,
        sensitivity: body.sensitivity,
        depth: body.depth,
        family: body.family,
        // The operator surface may notify; it defaults on because a run launched from the UI is
        // one someone is waiting for.
        notify: body.notify.unwrap_or(true),
    };
    Ok(Json(launch(&admin, req, user).await?))
}

/// What cancelling a run reports.
#[derive(Debug, Serialize)]
struct Cancelled {
    cancelled: bool,
}

/// Cancel a running analysis job (operator+). The task observes the flag between phases.
async fn cancel_analysis_job(
    _perm: RequireManageConfig,
    admin: Admin,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Cancelled>> {
    if admin.analysis.cancel(id) {
        Ok(Json(Cancelled { cancelled: true }))
    } else {
        Err(ApiError::not_found(
            "job_not_running",
            format!("no running analysis job {id}"),
        ))
    }
}

/// Live analysis-job status stream (SSE): each event is the job JSON with its current state and
/// progress. Mirrors the alert stream (lagged subscribers get a `resync` hint).
///
/// Returns a bare `Response` rather than `ApiResult`: an SSE body is not a `Json<T>`, and the
/// guards still run as extractors, which is where the safety was.
async fn stream_analysis(_perm: RequireView, admin: Admin) -> Response {
    let stream = tokio_stream::wrappers::BroadcastStream::new(admin.analysis.subscribe())
        .filter_map(|r| async move {
            match r {
                Ok(json) => Some(Ok::<_, Infallible>(Event::default().data(json))),
                Err(tokio_stream::wrappers::errors::BroadcastStreamRecvError::Lagged(n)) => {
                    Some(Ok(Event::default().event("resync").data(n.to_string())))
                }
            }
        });
    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::router;
    use crate::api::tests_support::{private_state, public_state};
    use axum::body::Body;
    use axum::http::{header::AUTHORIZATION, Request, StatusCode};
    use tower::ServiceExt;
    use yagra_common::{Principal, Role, Scope};

    fn req(tool: &str, scope_kind: &str, scope_id: Option<Uuid>) -> AnalysisRequest {
        AnalysisRequest {
            tool: tool.to_owned(),
            scope_kind: scope_kind.to_owned(),
            scope_id,
            scope_label: "All nodes".to_owned(),
            window_secs: 3600,
            baseline_secs: None,
            sensitivity: None,
            depth: None,
            family: None,
            notify: false,
        }
    }

    #[test]
    fn an_unknown_tool_is_rejected_with_the_real_list_of_tools() {
        // The message enumerates `AnalysisTool::ALL` rather than a hand-copied list, so a tool
        // added later cannot be missing from the very message that tells a client what exists.
        let err = job_params(req("teleport", "all", None)).expect_err("unknown tool must reject");
        assert_eq!(err.code(), "invalid_tool");
        assert!(err.message().contains("anomaly"), "{}", err.message());
        assert!(
            err.message().contains("incident_correlate"),
            "{}",
            err.message()
        );
    }

    #[test]
    fn a_scoped_run_without_a_scope_id_is_rejected() {
        // Group/node scope with no id would silently widen to the whole fleet in the runner, so
        // this is a rejection rather than a clamp — unlike the numeric knobs below, there is no
        // safe value to substitute.
        for kind in ["group", "node"] {
            let err = job_params(req("anomaly", kind, None))
                .err()
                .unwrap_or_else(|| panic!("{kind} scope without an id must reject"));
            assert_eq!(err.code(), "missing_scope_id");
        }
        assert!(job_params(req("anomaly", "node", Some(Uuid::nil()))).is_ok());
        // "all" needs no id — that is the whole fleet by definition.
        assert!(job_params(req("anomaly", "all", None)).is_ok());
        assert_eq!(
            job_params(req("anomaly", "everything", None))
                .expect_err("an unknown scope must reject")
                .code(),
            "invalid_scope"
        );
    }

    #[test]
    fn every_numeric_knob_is_clamped_rather_than_trusted() {
        // Clamping (not rejecting) is the deliberate choice: an over-eager client gets a bounded
        // run, not a failure. What must never happen is the value reaching the runner unbounded.
        let mut r = req("anomaly", "all", None);
        r.window_secs = i64::MAX;
        r.baseline_secs = Some(-1);
        r.sensitivity = Some(1e9);
        let p = job_params(r).expect("clamping never rejects");
        assert_eq!(p.window_secs, WINDOW_BOUNDS.1);
        assert_eq!(p.baseline_secs, BASELINE_BOUNDS.0);
        assert!((p.sensitivity - SENSITIVITY_BOUNDS.1).abs() < f64::EPSILON);

        let mut low = req("anomaly", "all", None);
        low.window_secs = 0;
        low.sensitivity = Some(-5.0);
        let p = job_params(low).expect("clamping never rejects");
        assert_eq!(p.window_secs, WINDOW_BOUNDS.0);
        assert!((p.sensitivity - SENSITIVITY_BOUNDS.0).abs() < f64::EPSILON);
    }

    #[test]
    fn notify_is_carried_through_and_never_defaulted() {
        // The one external side effect an analysis can have. `AnalysisRequest` has no default for
        // it precisely so a read-only surface cannot leave it on by forgetting to say.
        assert!(!job_params(req("anomaly", "all", None)).unwrap().notify);
        let mut r = req("anomaly", "all", None);
        r.notify = true;
        assert!(job_params(r).unwrap().notify);
    }

    #[test]
    fn shared_defaults_match_what_both_surfaces_used_to_write_separately() {
        let p = job_params(req("anomaly", "all", None)).expect("defaults are valid");
        assert_eq!(p.baseline_secs, DEFAULT_BASELINE_SECS);
        assert!((p.sensitivity - 3.0).abs() < f64::EPSILON);
        assert_eq!(p.depth, "standard");
        assert_eq!(p.family, "all");
    }

    #[tokio::test]
    async fn launching_a_run_is_gated_before_the_runner_is_consulted() {
        // A launch is a write: closed on a public dashboard, and an anonymous caller learns only
        // that it is unauthenticated — never whether this deployment has a runner.
        let resp = router(public_state())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/analysis/jobs")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"tool":"anomaly","scope_kind":"all","scope_label":"All nodes","window_secs":3600}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn a_viewer_may_read_the_runs_list_but_not_launch_one() {
        let st = private_state();
        let token = st.sessions.issue(
            Uuid::new_v4(),
            Principal::new(Role::Viewer, Scope::All),
            "viewer1",
        );
        let app = router(st);
        let list = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/analysis/jobs")
                    .header(AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(list.status(), StatusCode::OK);

        let launch = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/analysis/jobs")
                    .header(AUTHORIZATION, format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"tool":"anomaly","scope_kind":"all","scope_label":"All nodes","window_secs":3600}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(launch.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn the_runs_list_is_empty_not_broken_without_a_runner() {
        // Skeleton mode: the Troubleshoot page renders with an empty runs list rather than an
        // error panel, which is why this endpoint answers 200 where a launch answers 503.
        let resp = router(public_state())
            .oneshot(
                Request::builder()
                    .uri("/api/v1/analysis/jobs")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap();
        assert_eq!(&bytes[..], b"[]");
    }
}
