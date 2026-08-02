// SPDX-License-Identifier: AGPL-3.0-only
//! Troubleshoot analysis jobs (ADR-022).
//!
//! An analysis is a background read over the metric / passive-event / flow stores that produces
//! findings. It changes nothing about the fleet, so the only side effect it can have is the
//! optional notification — which is why `notify` is an explicit field of [`AnalysisRequest`]
//! rather than a default: the MCP surface must always pass `false`, and a default would let that
//! invariant be lost by omission.
//!
//! **Launching is `AckAlerts` (Operator and up) on both surfaces.** It used to be `ManageConfig`
//! here and `View` over MCP, which meant an on-call operator was refused in the WebUI while the
//! same person could run the identical analysis through an AI client — the sort of split that
//! teaches people to route around the product. `ManageConfig` was never about configuration: it
//! stood in for a rate limit, throttling an expensive TSDB read by restricting it to admins. Real
//! admission control (`YAGRA_ANALYSIS_MAX_CONCURRENT`, `YAGRA_ANALYSIS_RATE_PER_MIN`) does that job
//! now, so the permission is free to say what the endpoint *is*: incident-response work, the same
//! bracket as acknowledging an alert. A Viewer gets neither surface — running an analysis is not
//! reading, even though what it reads is already visible to them.
//!
//! [`job_params`] is the seam. Launching a run means validating a tool token, validating a scope,
//! requiring a scope id for group/node, and clamping four numeric knobs — and all of that existed
//! twice, once per surface, with the clamps written out separately in each. Two copies of a
//! *validation* rule is the worst kind to keep, because when they drift the looser one is the
//! security boundary.

use super::extract::{Admin, RequireAckAlerts, RequireView, Scoped};
use super::scope::ScopeTarget;
use super::util::ListQuery;
use super::{ApiError, ApiResult, ApiState};
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

/// This domain's slice of the OpenAPI document (ADR-035), merged by [`super::openapi::document`].
#[derive(utoipa::OpenApi)]
#[openapi(paths(
    list_analysis_jobs,
    create_analysis_job,
    get_analysis_job,
    analysis_findings,
    saved_findings,
    list_analysis_schedules,
    create_analysis_schedule,
    update_analysis_schedule,
    delete_analysis_schedule,
    cancel_analysis_job,
    stream_analysis
))]
pub(super) struct Doc;

/// The analysis routes, merged into `/api/v1` by [`super::router`].
pub(crate) fn routes() -> Router<ApiState> {
    Router::new()
        .route("/api/v1/analysis/findings", get(saved_findings))
        .route(
            "/api/v1/analysis/schedules",
            get(list_analysis_schedules).post(create_analysis_schedule),
        )
        .route(
            "/api/v1/analysis/schedules/:id",
            axum::routing::put(update_analysis_schedule).delete(delete_analysis_schedule),
        )
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

// ── Group scope over run rows (ADR-014) ──────────────────────────────────────

/// A run's target, as [`ScopeTarget`] — exhaustive over [`ScopeKind`].
///
/// A run is an artefact that *names its own scope*, exactly like a mute or a maintenance window, so
/// the visibility decision is the shared one in `api/scope.rs`. Only this reading is local, because
/// only this module knows what its own columns mean (see the warning on [`ScopeTarget`]).
///
/// A fleet-wide (`all`) run is [`ScopeTarget::Unbounded`]: its findings span the whole inventory, so
/// there is no group scope that honestly contains it. `create_analysis_job` already refuses to let a
/// scoped caller launch one; this is the read side of the same rule.
fn job_target(kind: Option<ScopeKind>, id: Option<Uuid>) -> ScopeTarget {
    match kind {
        Some(ScopeKind::Node) => id.map_or(ScopeTarget::Unbounded, |n| {
            ScopeTarget::Node(yagra_common::NodeId::from(n))
        }),
        Some(ScopeKind::Group) => id.map_or(ScopeTarget::Unbounded, ScopeTarget::Group),
        // `all`, or a persisted kind this build does not recognise — neither is bounded.
        Some(ScopeKind::All) | None => ScopeTarget::Unbounded,
    }
}

/// [`job_target`] for a stored row, whose `scope_kind` is still in its persisted text form.
pub(crate) fn row_target(job: &AnalysisJob) -> ScopeTarget {
    job_target(ScopeKind::from_str(&job.scope_kind), job.scope_id)
}

/// Refuse a run the caller may not see, as `404` rather than `403`.
///
/// The run row is the unit of visibility. Answering `403` here would confirm that a job with that
/// id exists, which is the same enumeration oracle `scope::require_visible_node` avoids — and the
/// two endpoints that read a run must agree, or `GET /jobs/{id}` answering 404 while
/// `GET /jobs/{id}/findings` answers `200 []` is itself the oracle.
pub(crate) fn require_visible_job(
    st: &ApiState,
    scope: &super::scope::NodeScope,
    job: &AnalysisJob,
) -> Result<(), ApiError> {
    if scope.allows_target(st, row_target(job)) {
        Ok(())
    } else {
        Err(ApiError::not_found(
            "job_not_found",
            format!("no analysis job {}", job.id),
        ))
    }
}

/// Drop the findings `scope` may not see.
///
/// Not redundant with [`require_visible_job`]: a run's scope is resolved to a node set when it
/// *starts*, and a node can be moved to another group before anyone opens the results — so the
/// group the run named is not proof about every node its findings name.
pub(crate) fn visible_findings(
    st: &ApiState,
    scope: &super::scope::NodeScope,
    findings: Vec<crate::analysis::AnalysisFinding>,
) -> Vec<crate::analysis::AnalysisFinding> {
    if scope.is_all() {
        return findings;
    }
    findings
        .into_iter()
        .filter(|f| match f.node_id {
            Some(n) => scope.allows_node(st, yagra_common::NodeId::from(n)),
            // A finding attributed to no node — the flow-tier-off notice, a fleet-level summary
            // row. Always shown to an unrestricted caller (handled above); hidden from a scoped one
            // for the same reason an ungrouped node is, since nothing places it inside their scope.
            None => false,
        })
        .collect()
}

// ── Handlers ─────────────────────────────────────────────────────────────────

/// Recent analysis jobs (the runs list). `?limit=` (default 50).
///
/// Skeleton mode has no runner, so this answers an empty list rather than a 503: the runs list is
/// a panel on a page that otherwise works, and an error there would break the page.
#[utoipa::path(
    get, path = "/api/v1/analysis/jobs", tag = "analysis",
    params(ListQuery),
    responses(
        (status = 200, description = "Recent runs, newest first; empty when this deployment has no runner", body = Vec<AnalysisJob>),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks the read permission", body = super::error::ErrorBody),
    ),
)]
async fn list_analysis_jobs(
    _perm: RequireView,
    Scoped(scope): Scoped,
    State(st): State<ApiState>,
    Query(q): Query<ListQuery>,
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
    // Post-filtered on the run's own scope. A short page is correct here rather than merely
    // tolerable: the runs list is "recent activity", not a cursor-paged collection, so there is no
    // paging invariant to preserve — and over-fetching would only widen how much of somebody else's
    // activity this handler touches.
    Ok(Json(
        jobs.into_iter()
            .filter(|j| scope.allows_target(&st, row_target(j)))
            .collect(),
    ))
}

/// One analysis job by id.
#[utoipa::path(
    get, path = "/api/v1/analysis/jobs/{id}", tag = "analysis",
    params(("id" = Uuid, Path, description = "Analysis job id")),
    responses(
        (status = 200, description = "The job row, including its state and progress", body = AnalysisJob),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks the read permission", body = super::error::ErrorBody),
        (status = 404, description = "No such job — also the answer when this deployment has no runner", body = super::error::ErrorBody),
    ),
)]
async fn get_analysis_job(
    _perm: RequireView,
    Scoped(scope): Scoped,
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
    // Out of scope reads as absent — the same 404 as an unknown id, so the job-id space is not an
    // enumeration oracle. The row carries the run's `scope_label`, which is a node or group *name*,
    // so leaking the row leaks inventory even before anyone reads its findings.
    if !scope.allows_target(&st, row_target(&job)) {
        return Err(ApiError::not_found(
            "job_not_found",
            format!("no analysis job {id}"),
        ));
    }
    Ok(Json(job))
}

/// A job's findings (the report list). Empty in skeleton mode.
#[utoipa::path(
    get, path = "/api/v1/analysis/jobs/{id}/findings", tag = "analysis",
    params(("id" = Uuid, Path, description = "Analysis job id")),
    responses(
        (status = 200, description = "The run's findings; empty for a job that has not succeeded", body = Vec<crate::analysis::AnalysisFinding>),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks the read permission", body = super::error::ErrorBody),
    ),
)]
async fn analysis_findings(
    _perm: RequireView,
    Scoped(scope): Scoped,
    State(st): State<ApiState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Vec<crate::analysis::AnalysisFinding>>> {
    let Some(admin) = st.admin.as_ref() else {
        return Ok(Json(Vec::new()));
    };
    // The run row is the unit of visibility, so gate on it first — otherwise this endpoint answers
    // `200 []` where `GET /analysis/jobs/{id}` answers 404 for the same id, and the pair of them is
    // an existence oracle. Unrestricted callers skip the lookup entirely.
    if !scope.is_all() {
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
        require_visible_job(&st, &scope, &job)?;
    }
    let findings = admin.analysis.findings(id).await.map_err(|e| {
        ApiError::from_internal(
            e.as_ref(),
            "list analysis findings",
            "failed to load findings",
        )
    })?;
    Ok(Json(visible_findings(&st, &scope, findings)))
}

/// Cap on one page of the cross-run findings search.
///
/// Lower than the runs list's 200 and deliberately so: a findings page is a row per *finding*, and
/// a busy fleet produces them faster than it produces runs. The screen pages, so the cap is a
/// bound on one round trip rather than on what an operator can see.
const FINDINGS_PAGE_MAX: i64 = 200;

/// Query params for the cross-run findings search.
#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub(super) struct SavedFindingsQuery {
    /// Page cursor: the `at` of the previous page's last row (RFC 3339).
    before: Option<String>,
    /// Page cursor tiebreak: that same row's `id`. Findings written by one run share a millisecond
    /// routinely, so a cursor without it would repeat or skip the rows sharing the boundary
    /// instant. Omitting it reads as "strictly before this instant".
    before_id: Option<Uuid>,
    /// Inclusive lower bound on finding time, RFC 3339 — the range filter, not the cursor.
    since: Option<String>,
    /// Restrict to one diagnostic (an `AnalysisTool` token, e.g. `anomaly`).
    tool: Option<String>,
    /// Restrict to `crit`, `warn` or `info`.
    severity: Option<String>,
    /// Restrict to findings about one node.
    node_id: Option<Uuid>,
    /// Restrict to findings about nodes in one folder group **and everything beneath it**.
    group_id: Option<Uuid>,
    /// Page size, clamped to 200 (default 100).
    limit: Option<i64>,
}

/// Findings across every run — the Saved-findings screen.
///
/// Complements `GET /analysis/jobs/{id}/findings`, which answers "what did this run find". This one
/// answers the question an operator actually starts from — "has anything been found about this node
/// / this site / this week" — which no single run can answer because the runs are what get
/// enumerated otherwise.
///
/// Skeleton mode has no job store, so it answers an empty list rather than a 503: the screen is a
/// search, and an error where "nothing yet" is the truthful answer reads as a broken page.
#[utoipa::path(
    get, path = "/api/v1/analysis/findings", tag = "analysis",
    params(SavedFindingsQuery),
    responses(
        (status = 200, description = "Matching findings, newest first; empty when this deployment has no runner", body = Vec<crate::analysis::SavedFinding>),
        (status = 400, description = "A cursor or range bound is not RFC 3339, or the tool/severity is unknown", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks the read permission", body = super::error::ErrorBody),
        (status = 404, description = "The requested node or group filter is outside the caller's scope", body = super::error::ErrorBody),
    ),
)]
async fn saved_findings(
    _perm: RequireView,
    Scoped(scope): Scoped,
    State(st): State<ApiState>,
    Query(q): Query<SavedFindingsQuery>,
) -> ApiResult<Json<Vec<crate::analysis::SavedFinding>>> {
    // Validate before consulting availability, unlike the plain reads. A malformed filter is the
    // caller's own request being wrong, and answering `200 []` for it would mean a client with a
    // typo in its cursor pages forever through an empty result and never learns why.
    let tool = q
        .tool
        .as_deref()
        .map(|t| {
            AnalysisTool::from_str(t).ok_or_else(|| {
                ApiError::bad_request(
                    "invalid_tool",
                    format!(
                        "unknown analysis tool {t:?}; must be one of: {}",
                        AnalysisTool::token_list()
                    ),
                )
            })
        })
        .transpose()?;
    // Validated against the list the engine writes from, not against a copy of it.
    if let Some(sev) = q.severity.as_deref() {
        if !crate::analysis::FINDING_SEVERITIES.contains(&sev) {
            return Err(ApiError::bad_request(
                "invalid_severity",
                format!(
                    "unknown severity {sev:?}; must be one of: {}",
                    crate::analysis::FINDING_SEVERITIES.join(", ")
                ),
            ));
        }
    }
    // Filtering *by* a node or a group is still naming one, so both go through the same check the
    // rest of the surface uses. Answering `200 []` instead would be a slower way of saying the same
    // thing for a group that exists, and a way of confirming one that does not.
    if let Some(n) = q.node_id {
        super::scope::require_visible_node(&st, &scope, yagra_common::NodeId::from(n))?;
    }
    let in_group = match q.group_id {
        None => None,
        Some(g) => {
            super::scope::require_visible_group(&scope, g)?;
            Some(super::scope::subtree_of(&st, g).await?)
        }
    };
    let before = bound(q.before.as_deref(), "before")?;
    let since = bound(q.since.as_deref(), "since")?;
    // Skeleton mode has no job store, so there is nothing to search — and "nothing found" is a
    // truthful answer to a search, unlike the 503 an unconfigured subsystem owes a reader.
    let Some(admin) = st.admin.as_ref() else {
        return Ok(Json(Vec::new()));
    };
    let filter = crate::analysis::FindingSearch {
        before,
        before_id: q.before_id,
        since,
        tool,
        severity: q.severity.as_deref(),
        node_id: q.node_id,
        groups: scope.group_filter(),
        in_group: in_group.as_deref(),
        limit: q.limit.unwrap_or(100).clamp(1, FINDINGS_PAGE_MAX),
    };
    let found = admin.analysis.search_findings(&filter).await.map_err(|e| {
        ApiError::from_internal(e.as_ref(), "search findings", "failed to search findings")
    })?;
    Ok(Json(found))
}

/// Parse an optional RFC 3339 bound, rejecting rather than dropping an unparseable one.
///
/// Dropping it would widen the query — a malformed `since` would silently search all of history —
/// which is the edge-parsing rule in `security.md`: an unparseable filter is an error, never a
/// default.
fn bound(
    raw: Option<&str>,
    field: &'static str,
) -> Result<Option<chrono::DateTime<chrono::Utc>>, ApiError> {
    raw.map(|s| {
        super::util::parse_rfc3339(s).ok_or_else(|| {
            ApiError::bad_request("invalid_time", format!("`{field}` is not RFC 3339"))
        })
    })
    .transpose()
}

/// Request body to launch an analysis (launch drawer / report config bar).
#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct CreateAnalysisJob {
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
#[utoipa::path(
    post, path = "/api/v1/analysis/jobs", tag = "analysis",
    request_body = CreateAnalysisJob,
    responses(
        (status = 200, description = "The queued job row; it progresses over `/api/v1/stream/analysis`", body = AnalysisJob),
        (status = 400, description = "Unknown tool, unknown scope kind, or a group/node scope with no `scope_id`", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role below Operator", body = super::error::ErrorBody),
        (status = 429, description = "The runner is at its concurrency or rate limit — retryable", body = super::error::ErrorBody),
        (status = 503, description = "This deployment has no runner", body = super::error::ErrorBody),
    ),
)]
async fn create_analysis_job(
    _perm: RequireAckAlerts,
    Scoped(scope): Scoped,
    admin: Admin,
    State(st): State<ApiState>,
    actor: super::extract::Actor,
    Json(body): Json<CreateAnalysisJob>,
) -> ApiResult<Json<AnalysisJob>> {
    require_launchable_scope(&st, &scope, &body.scope_kind, body.scope_id)?;
    let user = actor.0;
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

// ── Schedules ────────────────────────────────────────────────────────────────

/// Create/update body for an analysis schedule: the launch spec plus the cadence.
#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct AnalysisScheduleBody {
    /// Which diagnostic to run (an `AnalysisTool` token, e.g. `anomaly`).
    tool: String,
    /// `all` | `group` | `node`.
    scope_kind: String,
    scope_id: Option<Uuid>,
    scope_label: String,
    window_secs: i64,
    baseline_secs: Option<i64>,
    sensitivity: Option<f64>,
    depth: Option<String>,
    family: Option<String>,
    /// Whether a completed run may notify. Defaults to `false` — a schedule fires unattended, so
    /// silence is the safer default; the manual launch path defaults it on because someone is
    /// waiting for that run.
    notify: Option<bool>,
    /// `daily` | `weekly` | `monthly`.
    frequency: String,
    /// 0=Sun … 6=Sat. Read only for `weekly`.
    day_of_week: Option<i16>,
    /// 1 … 28. Read only for `monthly`.
    day_of_month: Option<i16>,
    at_hour: i16,
    at_minute: i16,
    /// Defaults to enabled.
    enabled: Option<bool>,
}

/// Validate a schedule body into a [`crate::analysis::ScheduleInput`] and its first `next_run_at`.
///
/// The launch half goes through the same [`job_params`] the immediate launch uses, so a schedule
/// cannot be saved with a window a direct run would refuse — and so the clamps are applied on the
/// way in *and* re-applied at fire time (see [`scheduled_params`]).
fn parse_schedule_body(
    st: &ApiState,
    scope: &super::scope::NodeScope,
    admin: &super::AdminState,
    body: AnalysisScheduleBody,
) -> Result<
    (
        crate::analysis::ScheduleInput,
        chrono::DateTime<chrono::Utc>,
    ),
    ApiError,
> {
    require_launchable_scope(st, scope, &body.scope_kind, body.scope_id)?;
    let cadence = super::util::parse_cadence(super::util::CadenceBody {
        frequency: body.frequency,
        day_of_week: body.day_of_week,
        day_of_month: body.day_of_month,
        at_hour: body.at_hour,
        at_minute: body.at_minute,
    })?;
    let params = job_params(AnalysisRequest {
        tool: body.tool,
        scope_kind: body.scope_kind,
        scope_id: body.scope_id,
        scope_label: body.scope_label,
        window_secs: body.window_secs,
        baseline_secs: body.baseline_secs,
        sensitivity: body.sensitivity,
        depth: body.depth,
        family: body.family,
        notify: body.notify.unwrap_or(false),
    })?;
    // Refused rather than accepted-and-useless: a flow analysis with no flow store short-circuits
    // to a single "flow tier not enabled" info finding. Once a day, forever, that is an unbroken
    // column of successful empty runs — a schedule that looks like it is working.
    if params.tool.needs_flow_tier() && !admin.analysis.flow_enabled() {
        return Err(ApiError::bad_request(
            "flow_tier_off",
            "this analysis reads the traffic-flow store, which this deployment does not have \
             configured — scheduling it would produce an empty run on every fire",
        ));
    }
    let next = crate::cadence::compute_next_run(cadence, chrono::Utc::now());
    Ok((
        crate::analysis::ScheduleInput {
            params,
            cadence,
            enabled: body.enabled.unwrap_or(true),
        },
        next,
    ))
}

/// Rebuild a launch request from a stored schedule and re-validate it through [`job_params`].
///
/// Re-validated, not trusted: the clamps are *edge* validation, so a schedule saved before a bound
/// moved must not keep firing outside the new one. It also means a row hand-edited in the database
/// cannot drive the runner past its limits.
pub(crate) fn scheduled_params(
    s: &crate::analysis::AnalysisSchedule,
) -> Result<crate::analysis::JobParams, ApiError> {
    let p = &s.params;
    let num = |k: &str| p.get(k).and_then(serde_json::Value::as_i64);
    let text = |k: &str| {
        p.get(k)
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
    };
    job_params(AnalysisRequest {
        tool: s.tool.clone(),
        scope_kind: s.scope_kind.clone(),
        scope_id: s.scope_id,
        scope_label: s.scope_label.clone(),
        // A stored row with no window is a row this build cannot honour as written; the default is
        // the same one the launch drawer offers, not a silently unbounded scan.
        window_secs: num("window_secs").unwrap_or(7 * 86_400),
        baseline_secs: num("baseline_secs"),
        sensitivity: p.get("sensitivity").and_then(serde_json::Value::as_f64),
        depth: text("depth"),
        family: text("family"),
        notify: p
            .get("notify")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
    })
}

/// The schedule's own target, as a [`ScopeTarget`] — exhaustive over [`ScopeKind`], and the same
/// reading `job_target` gives a run row.
fn schedule_target(s: &crate::analysis::AnalysisSchedule) -> ScopeTarget {
    job_target(ScopeKind::from_str(&s.scope_kind), s.scope_id)
}

/// Every analysis schedule, soonest first. Empty in skeleton mode.
#[utoipa::path(
    get, path = "/api/v1/analysis/schedules", tag = "analysis",
    responses(
        (status = 200, description = "Every schedule the caller may see; empty when this deployment has no runner", body = Vec<crate::analysis::AnalysisSchedule>),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks the read permission", body = super::error::ErrorBody),
    ),
)]
async fn list_analysis_schedules(
    _perm: RequireView,
    Scoped(scope): Scoped,
    State(st): State<ApiState>,
) -> ApiResult<Json<Vec<crate::analysis::AnalysisSchedule>>> {
    let Some(admin) = st.admin.as_ref() else {
        return Ok(Json(Vec::new()));
    };
    let list = admin.analysis.repo().list_schedules().await.map_err(|e| {
        ApiError::from_internal(
            e.as_ref(),
            "list analysis schedules",
            "failed to list analysis schedules",
        )
    })?;
    // Post-filtered on each schedule's own target, exactly as the runs list is: a schedule names a
    // node or a folder group, and a fleet-wide one is unbounded.
    Ok(Json(
        list.into_iter()
            .filter(|s| scope.allows_target(&st, schedule_target(s)))
            .collect(),
    ))
}

/// Create an analysis schedule (operator+).
#[utoipa::path(
    post, path = "/api/v1/analysis/schedules", tag = "analysis",
    request_body = AnalysisScheduleBody,
    responses(
        (status = 201, description = "Schedule created", body = super::util::CreatedId),
        (status = 400, description = "Unknown tool or cadence, a group/node scope with no `scope_id`, or a flow analysis on a deployment with no flow store", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role below Operator, or a fleet-wide schedule from a group-scoped account", body = super::error::ErrorBody),
        (status = 404, description = "The named node or group is outside the caller's scope", body = super::error::ErrorBody),
        (status = 503, description = "This deployment has no runner", body = super::error::ErrorBody),
    ),
)]
async fn create_analysis_schedule(
    _perm: RequireAckAlerts,
    Scoped(scope): Scoped,
    admin: Admin,
    State(st): State<ApiState>,
    actor: super::extract::Actor,
    Json(body): Json<AnalysisScheduleBody>,
) -> ApiResult<(axum::http::StatusCode, Json<super::util::CreatedId>)> {
    let (input, next) = parse_schedule_body(&st, &scope, &admin, body)?;
    let id = admin
        .analysis
        .repo()
        .create_schedule(&input, next, actor.0.as_deref())
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "create analysis schedule",
                "failed to create analysis schedule",
            )
        })?;
    Ok((
        axum::http::StatusCode::CREATED,
        Json(super::util::CreatedId { id }),
    ))
}

/// Update a schedule. Recomputes `next_run_at` from the new cadence, so an edit takes effect at the
/// next matching instant rather than whenever the old one happened to be.
#[utoipa::path(
    put, path = "/api/v1/analysis/schedules/{id}", tag = "analysis",
    params(("id" = Uuid, Path, description = "Analysis schedule id")),
    request_body = AnalysisScheduleBody,
    responses(
        (status = 204, description = "Schedule updated"),
        (status = 400, description = "Unknown tool or cadence, a group/node scope with no `scope_id`, or a flow analysis on a deployment with no flow store", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role below Operator, or a fleet-wide schedule from a group-scoped account", body = super::error::ErrorBody),
        (status = 404, description = "No such schedule, or it is outside the caller's scope", body = super::error::ErrorBody),
        (status = 503, description = "This deployment has no runner", body = super::error::ErrorBody),
    ),
)]
async fn update_analysis_schedule(
    _perm: RequireAckAlerts,
    Scoped(scope): Scoped,
    admin: Admin,
    State(st): State<ApiState>,
    Path(id): Path<Uuid>,
    actor: super::extract::Actor,
    Json(body): Json<AnalysisScheduleBody>,
) -> ApiResult<axum::http::StatusCode> {
    // Both ends are checked: the schedule as it stands must be visible, and so must the target the
    // edit moves it to. Checking only the new one would let a scoped caller retarget somebody
    // else's schedule onto their own group and thereby take it over.
    require_visible_schedule(&st, &scope, &admin, id).await?;
    let (input, next) = parse_schedule_body(&st, &scope, &admin, body)?;
    let found = admin
        .analysis
        .repo()
        .update_schedule(id, &input, next, actor.0.as_deref())
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "update analysis schedule",
                "failed to update analysis schedule",
            )
        })?;
    if found {
        Ok(axum::http::StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found(
            "schedule_not_found",
            format!("no analysis schedule {id}"),
        ))
    }
}

/// Delete a schedule (operator+).
#[utoipa::path(
    delete, path = "/api/v1/analysis/schedules/{id}", tag = "analysis",
    params(("id" = Uuid, Path, description = "Analysis schedule id")),
    responses(
        (status = 204, description = "Schedule deleted"),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role below Operator", body = super::error::ErrorBody),
        (status = 404, description = "No such schedule, or it is outside the caller's scope", body = super::error::ErrorBody),
        (status = 503, description = "This deployment has no runner", body = super::error::ErrorBody),
    ),
)]
async fn delete_analysis_schedule(
    _perm: RequireAckAlerts,
    Scoped(scope): Scoped,
    admin: Admin,
    State(st): State<ApiState>,
    Path(id): Path<Uuid>,
) -> ApiResult<axum::http::StatusCode> {
    require_visible_schedule(&st, &scope, &admin, id).await?;
    let found = admin
        .analysis
        .repo()
        .delete_schedule(id)
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "delete analysis schedule",
                "failed to delete analysis schedule",
            )
        })?;
    if found {
        Ok(axum::http::StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found(
            "schedule_not_found",
            format!("no analysis schedule {id}"),
        ))
    }
}

/// Refuse a schedule the caller cannot see — with the same 404 an unknown id gets, so the id space
/// is not an enumeration oracle. Unrestricted callers skip the lookup entirely.
async fn require_visible_schedule(
    st: &ApiState,
    scope: &super::scope::NodeScope,
    admin: &super::AdminState,
    id: Uuid,
) -> Result<(), ApiError> {
    if scope.is_all() {
        return Ok(());
    }
    let existing = admin
        .analysis
        .repo()
        .get_schedule(id)
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "get analysis schedule",
                "failed to load analysis schedule",
            )
        })?
        .ok_or_else(|| {
            ApiError::not_found("schedule_not_found", format!("no analysis schedule {id}"))
        })?;
    if scope.allows_target(st, schedule_target(&existing)) {
        Ok(())
    } else {
        Err(ApiError::not_found(
            "schedule_not_found",
            format!("no analysis schedule {id}"),
        ))
    }
}

/// Refuse a launch target the caller may not run an analysis over.
///
/// A run's scope is resolved server-side and its findings are read back later, so an over-broad
/// launch would hand a scoped operator fleet-wide data through the results. A scoped caller must
/// name a target inside their scope and cannot ask for `all`.
///
/// Shared by the immediate launch and the schedule writers, because a schedule is a launch with a
/// delay — checking one and not the other would make the schedule the way around the check. The
/// MCP `run_analysis` tool takes it too, for the same reason: two surfaces launching the same run
/// must agree on who may launch it over what.
pub(crate) fn require_launchable_scope(
    st: &ApiState,
    scope: &super::scope::NodeScope,
    scope_kind: &str,
    scope_id: Option<Uuid>,
) -> Result<(), ApiError> {
    if scope.is_all() {
        return Ok(());
    }
    match scope_kind {
        "node" => {
            let node = scope_id.ok_or_else(|| {
                ApiError::bad_request("missing_scope_id", "scope_id is required for node scope")
            })?;
            super::scope::require_visible_node(st, scope, yagra_common::NodeId::from(node))
        }
        "group" => {
            let gid = scope_id.ok_or_else(|| {
                ApiError::bad_request("missing_scope_id", "scope_id is required for group scope")
            })?;
            super::scope::require_visible_group(scope, gid)
        }
        _ => Err(ApiError::forbidden_code(
            "scope_unsupported",
            "a group-scoped account must run an analysis against a node or a folder group, \
             not the whole fleet",
        )),
    }
}

/// What cancelling a run reports.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(super) struct Cancelled {
    cancelled: bool,
}

/// Cancel a running analysis job (operator+). The task observes the flag between phases.
#[utoipa::path(
    post, path = "/api/v1/analysis/jobs/{id}/cancel", tag = "analysis",
    params(("id" = Uuid, Path, description = "Analysis job id")),
    responses(
        (status = 200, description = "The run was flagged for cancellation", body = Cancelled),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role below Operator", body = super::error::ErrorBody),
        (status = 404, description = "No job by that id is currently running", body = super::error::ErrorBody),
        (status = 503, description = "This deployment has no runner", body = super::error::ErrorBody),
    ),
)]
async fn cancel_analysis_job(
    _perm: RequireAckAlerts,
    Scoped(scope): Scoped,
    State(st): State<ApiState>,
    admin: Admin,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Cancelled>> {
    // Killing someone else's run is a write, and the 200/404 split also answers "is this job
    // running right now" — a read oracle wearing a POST. Same 404 as a job that is not running.
    if !scope.is_all() {
        let visible = admin
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
            .is_some_and(|job| scope.allows_target(&st, row_target(&job)));
        if !visible {
            return Err(ApiError::not_found(
                "job_not_running",
                format!("no running analysis job {id}"),
            ));
        }
    }
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
#[utoipa::path(
    get, path = "/api/v1/stream/analysis", tag = "analysis",
    responses(
        (status = 200, description = "Server-sent event stream of job state and progress; a lagged subscriber gets a named `resync` event", content_type = "text/event-stream"),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks the read permission", body = super::error::ErrorBody),
        (status = 503, description = "This deployment has no runner", body = super::error::ErrorBody),
    ),
)]
async fn stream_analysis(
    _perm: RequireView,
    Scoped(scope): Scoped,
    State(st): State<ApiState>,
    admin: Admin,
) -> Response {
    // ⚠️ `Weak`, for the reason spelled out on `scope::NodeScope::allows_node_in`: the runner owns
    // this stream's broadcast sender, so a strong handle here would keep the sender alive as long
    // as the stream and the stream alive as long as the sender — a body that never ends.
    let alerts = std::sync::Arc::downgrade(&st.alerts);
    let stream = tokio_stream::wrappers::BroadcastStream::new(admin.analysis.subscribe())
        .filter_map(move |r| {
            let (alerts, scope) = (alerts.clone(), scope.clone());
            async move {
                match r {
                    Ok((kind, id, json)) => {
                        let target = job_target(kind, id);
                        let visible = scope.is_all()
                            || alerts
                                .upgrade()
                                .is_some_and(|a| scope.allows_target_in(&a, target));
                        // Another caller's run progressing is dropped, not resynced — see the note
                        // on `api/alerts.rs::sse_with_resync`.
                        visible.then(|| Ok::<_, Infallible>(Event::default().data(&*json)))
                    }
                    Err(tokio_stream::wrappers::errors::BroadcastStreamRecvError::Lagged(n)) => {
                        Some(Ok(Event::default().event("resync").data(n.to_string())))
                    }
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
    async fn an_operator_may_launch_a_run() {
        // The boundary is Operator, not Admin. This used to be `ManageConfig`, which refused the
        // on-call operator in the WebUI while the same person could run the identical analysis
        // through MCP — so this test is what stops the gate drifting back up to admin-only.
        // 503 is the positive control: past RBAC, into skeleton mode's "no runner here".
        for (role, who) in [(Role::Operator, "op1"), (Role::Admin, "admin1")] {
            let st = private_state();
            let token = st
                .sessions
                .issue(Uuid::new_v4(), Principal::new(role, Scope::All), who);
            let resp = router(st)
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
            assert_eq!(
                resp.status(),
                StatusCode::SERVICE_UNAVAILABLE,
                "{role:?} should clear the permission gate"
            );
        }
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

    // ── The cross-run findings search ────────────────────────────────────────

    /// GET `/api/v1/analysis/findings` with `query`, unauthenticated against public-dashboard
    /// state (reads are open there, so this exercises the handler rather than the gate).
    async fn search(query: &str) -> (StatusCode, String) {
        let resp = router(public_state())
            .oneshot(
                Request::builder()
                    .uri(format!("/api/v1/analysis/findings{query}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap();
        (status, String::from_utf8_lossy(&bytes).into_owned())
    }

    #[tokio::test]
    async fn a_malformed_filter_is_rejected_rather_than_widening_the_search() {
        // Each of these would otherwise be *dropped*, and a dropped filter searches more than the
        // caller asked for. `since` is the one that matters most: silently ignoring it turns "this
        // week" into all of history.
        for (q, code) in [
            ("?since=yesterday", "invalid_time"),
            ("?before=2026-13-45", "invalid_time"),
            ("?tool=teleport", "invalid_tool"),
            ("?severity=urgent", "invalid_severity"),
        ] {
            let (status, body) = search(q).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{q} → {body}");
            assert!(body.contains(code), "{q} → {body}");
        }
    }

    #[tokio::test]
    async fn the_severity_filter_names_the_values_the_engine_actually_writes() {
        // The message enumerates `FINDING_SEVERITIES`, so it cannot drift from what a finding can
        // hold — the same property `an_unknown_tool_is_rejected_with_the_real_list_of_tools` pins
        // for tools.
        let (_, body) = search("?severity=urgent").await;
        for sev in crate::analysis::FINDING_SEVERITIES {
            assert!(body.contains(sev), "{sev} missing from: {body}");
        }
        // …and every value it names is accepted.
        for sev in crate::analysis::FINDING_SEVERITIES {
            let (status, body) = search(&format!("?severity={sev}")).await;
            assert_eq!(status, StatusCode::OK, "{sev} → {body}");
        }
    }

    #[tokio::test]
    async fn the_findings_search_is_empty_not_broken_without_a_runner() {
        // Same reasoning as the runs list: a search over nothing found nothing.
        let (status, body) = search("").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, "[]");
    }

    #[tokio::test]
    async fn a_scoped_caller_cannot_filter_by_a_node_or_group_it_cannot_see() {
        // Filtering *by* an id is naming it, so both answer 404 — the same answer an unknown id
        // gets, because a 403 here would confirm the id exists (`extract::VisibleNode`).
        let st = private_state();
        let token = st.sessions.issue(
            Uuid::new_v4(),
            Principal::new(
                Role::Viewer,
                Scope::groups([Uuid::from_u128(1).to_string()]),
            ),
            "scoped1",
        );
        let app = router(st);
        for q in [
            format!("?node_id={}", Uuid::from_u128(7)),
            format!("?group_id={}", Uuid::from_u128(8)),
        ] {
            let resp = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(format!("/api/v1/analysis/findings{q}"))
                        .header(AUTHORIZATION, format!("Bearer {token}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::NOT_FOUND, "{q}");
        }
    }

    // ── Scheduled analyses ───────────────────────────────────────────────────

    fn schedule_row(params: serde_json::Value) -> crate::analysis::AnalysisSchedule {
        crate::analysis::AnalysisSchedule {
            id: Uuid::new_v4(),
            tool: "anomaly".to_owned(),
            scope_kind: "all".to_owned(),
            scope_id: None,
            scope_label: "All nodes".to_owned(),
            params,
            frequency: crate::cadence::Cadence::Daily,
            day_of_week: None,
            day_of_month: None,
            at_hour: 3,
            at_minute: 0,
            enabled: true,
            next_run_ms: 0,
            last_run_ms: None,
            last_status: None,
        }
    }

    #[test]
    fn a_stored_schedule_is_re_clamped_at_fire_time_not_trusted() {
        // The reason the fire path re-runs `job_params` instead of reading the blob straight: the
        // clamps are edge validation. A row saved before a bound moved — or edited in the database
        // — must not drive the runner past today's limits.
        let p = scheduled_params(&schedule_row(serde_json::json!({
            "window_secs": i64::MAX,
            "baseline_secs": -1,
            "sensitivity": 1e9,
            "depth": "exhaustive",
            "family": "all",
            "notify": true,
        })))
        .expect("a stored schedule with absurd knobs still fires, bounded");
        assert_eq!(p.window_secs, WINDOW_BOUNDS.1);
        assert_eq!(p.baseline_secs, BASELINE_BOUNDS.0);
        assert!((p.sensitivity - SENSITIVITY_BOUNDS.1).abs() < f64::EPSILON);
    }

    #[test]
    fn a_schedule_with_no_stored_window_gets_a_default_rather_than_an_unbounded_scan() {
        let p = scheduled_params(&schedule_row(serde_json::json!({}))).expect("defaults apply");
        assert_eq!(p.window_secs, 7 * 86_400);
        assert_eq!(p.baseline_secs, DEFAULT_BASELINE_SECS);
        // Notify defaults **off** for an unattended fire, unlike the manual launch path.
        assert!(!p.notify);
    }

    #[test]
    fn a_schedule_naming_a_tool_this_build_lacks_is_reported_not_run() {
        let mut row = schedule_row(serde_json::json!({}));
        row.tool = "teleport".to_owned();
        assert_eq!(
            scheduled_params(&row)
                .expect_err("unknown tool must reject")
                .code(),
            "invalid_tool"
        );
    }

    #[tokio::test]
    async fn scheduling_is_gated_before_the_runner_is_consulted() {
        // A schedule is a write: an anonymous caller learns only that it is unauthenticated, never
        // whether this deployment has a runner.
        let resp = router(public_state())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/analysis/schedules")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"tool":"anomaly","scope_kind":"all","scope_label":"All nodes","window_secs":3600,"frequency":"daily","at_hour":3,"at_minute":0}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn a_viewer_may_read_schedules_but_not_write_one() {
        let st = private_state();
        let token = st.sessions.issue(
            Uuid::new_v4(),
            Principal::new(Role::Viewer, Scope::All),
            "viewer2",
        );
        let app = router(st);
        let list = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/analysis/schedules")
                    .header(AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(list.status(), StatusCode::OK);

        let create = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/analysis/schedules")
                    .header(AUTHORIZATION, format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"tool":"anomaly","scope_kind":"all","scope_label":"All nodes","window_secs":3600,"frequency":"daily","at_hour":3,"at_minute":0}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(create.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn the_schedules_list_is_empty_not_broken_without_a_runner() {
        let resp = router(public_state())
            .oneshot(
                Request::builder()
                    .uri("/api/v1/analysis/schedules")
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

    #[tokio::test]
    async fn the_findings_search_is_gated_before_it_reports_anything() {
        // Authenticate first, then availability (api-conventions): an anonymous caller learns only
        // that it is unauthenticated, never whether this deployment has a runner.
        let resp = router(private_state())
            .oneshot(
                Request::builder()
                    .uri("/api/v1/analysis/findings")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }
}
