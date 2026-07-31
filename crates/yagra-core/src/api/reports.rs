// SPDX-License-Identifier: AGPL-3.0-only
//! Scheduled and on-demand reports (Dashboard ▸ Reports).
//!
//! A **shared resource**, like the Shared Dashboard: everyone reads, only `ManageConfig` writes.
//! Three nouns — a *definition* is a reusable template carrying an opaque `spec`, a *schedule* fires
//! one on a preset cadence, and a *run* is a saved generated report. Generation itself happens in
//! core as a background task; `run_now` returns the row immediately and the run progresses over SSE.
//!
//! **The reads degrade differently on purpose**, and the difference is the contract:
//! - the section catalog is static, so it answers fully even in skeleton mode;
//! - list endpoints answer an empty array — "no reports" is true when there is no database;
//! - single-item reads answer 404 — "no report run 123" is also true, and is what the viewer needs
//!   in order to show its not-found state rather than an error.
//!
//! The `spec` is stored without interpretation, so it is validated at the edge for the two things
//! the server *can* be responsible for: it is a JSON object within the shared document cap, and
//! every section kind it names is one this build knows how to render. A spec naming an unknown
//! section would otherwise be saved happily and fail at generation time, long after the operator
//! left the builder.

use super::error::{ApiError, ApiResult};
use super::extract::{current_username, Admin, RequireManageConfig, RequireView};
use super::util::{CreatedId, ListQuery, MAX_JSON_DOC_BYTES};
use super::ApiState;
use crate::reports::{self, ScheduleInput};
use axum::{
    extract::{Path, Query, State},
    http::HeaderMap,
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Response,
    },
    routing::{get, post},
    Json, Router,
};
use futures::stream::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::convert::Infallible;
use uuid::Uuid;

/// Max bytes for a report definition `spec` — the shared cap on an opaque operator-authored JSON
/// document. This read `= MAX_DASHBOARD_BYTES` while the dashboard block owned that constant, which
/// is why it is now expressed against the shared one.
const MAX_REPORT_SPEC_BYTES: usize = MAX_JSON_DOC_BYTES;

/// This domain's slice of the OpenAPI document (ADR-035), merged by [`super::openapi::document`].
#[derive(utoipa::OpenApi)]
#[openapi(paths(
    list_report_sections,
    list_report_definitions,
    create_report_definition,
    get_report_definition,
    update_report_definition,
    delete_report_definition,
    run_report_definition,
    list_report_runs,
    get_report_run,
    delete_report_run,
    export_report_run,
    list_report_schedules,
    create_report_schedule,
    update_report_schedule,
    delete_report_schedule,
    stream_report_runs
))]
pub(super) struct Doc;

/// The report routes, merged into `/api/v1` by [`super::router`].
pub(super) fn routes() -> Router<ApiState> {
    Router::new()
        .route("/api/v1/reports/sections", get(list_report_sections))
        .route(
            "/api/v1/reports/definitions",
            get(list_report_definitions).post(create_report_definition),
        )
        .route(
            "/api/v1/reports/definitions/:id",
            get(get_report_definition)
                .put(update_report_definition)
                .delete(delete_report_definition),
        )
        .route(
            "/api/v1/reports/definitions/:id/run",
            post(run_report_definition),
        )
        .route("/api/v1/reports/runs", get(list_report_runs))
        .route(
            "/api/v1/reports/runs/:id",
            get(get_report_run).delete(delete_report_run),
        )
        .route("/api/v1/reports/runs/:id/export", get(export_report_run))
        .route(
            "/api/v1/reports/schedules",
            get(list_report_schedules).post(create_report_schedule),
        )
        .route(
            "/api/v1/reports/schedules/:id",
            axum::routing::put(update_report_schedule).delete(delete_report_schedule),
        )
        .route("/api/v1/stream/report-runs", get(stream_report_runs))
}

/// `{"ok": true}` — the body of a successful mutation that has nothing else to say.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct Ok_ {
    ok: bool,
}

impl Ok_ {
    fn yes() -> Json<Self> {
        Json(Self { ok: true })
    }
}

/// The section catalog that drives the builder. Static, so it answers fully in skeleton mode.
#[utoipa::path(
    get, path = "/api/v1/reports/sections", tag = "reports",
    responses(
        (status = 200, description = "Every section kind this build can render", body = Vec<reports::SectionDef>),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks View", body = super::error::ErrorBody),
    ),
)]
async fn list_report_sections(_guard: RequireView) -> Json<Vec<reports::SectionDef>> {
    Json(reports::section_catalog())
}

/// Validate a report definition `spec`.
///
/// Two checks the server can actually be responsible for: the document is an object within the
/// shared size cap, and every section kind it names is one this build can render. The second is the
/// one that matters — an unknown kind would otherwise be stored happily and fail at generation
/// time, long after whoever wrote it has left the builder.
fn validate_report_spec(spec: &Value) -> Result<(), ApiError> {
    if !spec.is_object() {
        return Err(ApiError::bad_request(
            "invalid_spec",
            "report spec must be a JSON object",
        ));
    }
    if serde_json::to_vec(spec).map_or(usize::MAX, |v| v.len()) > MAX_REPORT_SPEC_BYTES {
        return Err(ApiError::payload_too_large(
            "spec_too_large",
            format!("report spec exceeds {MAX_REPORT_SPEC_BYTES} bytes"),
        ));
    }
    if let Some(sections) = spec.get("sections").and_then(|s| s.as_array()) {
        for sec in sections {
            let kind = sec.get("kind").and_then(|k| k.as_str()).unwrap_or("");
            if !reports::is_known_section(kind) {
                return Err(ApiError::bad_request(
                    "unknown_section",
                    format!("unknown report section kind {kind:?}"),
                ));
            }
        }
    }
    Ok(())
}

/// All report definitions. Empty in skeleton mode — "no templates" is the truth when there is no
/// database, and the builder renders its empty state rather than an error.
#[utoipa::path(
    get, path = "/api/v1/reports/definitions", tag = "reports",
    responses(
        (status = 200, description = "Every report definition; empty in skeleton mode", body = Vec<reports::ReportDefinition>),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks View", body = super::error::ErrorBody),
    ),
)]
async fn list_report_definitions(
    _guard: RequireView,
    State(st): State<ApiState>,
) -> ApiResult<Json<Vec<reports::ReportDefinition>>> {
    let Some(admin) = st.admin.as_ref() else {
        return Ok(Json(Vec::new()));
    };
    let defs = admin.reports.repo().list_definitions().await.map_err(|e| {
        ApiError::from_internal(
            e.as_ref(),
            "list report definitions",
            "failed to list report definitions",
        )
    })?;
    Ok(Json(defs))
}

/// The not-found error both single-item definition reads answer with.
fn no_definition(id: Uuid) -> ApiError {
    ApiError::not_found("definition_not_found", format!("no report definition {id}"))
}

/// The not-found error the run reads answer with.
fn no_run(id: Uuid) -> ApiError {
    ApiError::not_found("run_not_found", format!("no report run {id}"))
}

/// One report definition. 404 in skeleton mode rather than 503: with no store the definition really
/// does not exist, and the viewer wants its not-found state.
#[utoipa::path(
    get, path = "/api/v1/reports/definitions/{id}", tag = "reports",
    params(("id" = Uuid, Path, description = "Report definition id")),
    responses(
        (status = 200, description = "The definition", body = reports::ReportDefinition),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks View", body = super::error::ErrorBody),
        (status = 404, description = "No such definition, or skeleton mode", body = super::error::ErrorBody),
    ),
)]
async fn get_report_definition(
    _guard: RequireView,
    State(st): State<ApiState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<reports::ReportDefinition>> {
    let admin = st.admin.as_ref().ok_or_else(|| no_definition(id))?;
    admin
        .reports
        .repo()
        .get_definition(id)
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "get report definition",
                "failed to load report definition",
            )
        })?
        .map(Json)
        .ok_or_else(|| no_definition(id))
}

/// Create/update body for a report definition.
#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct ReportDefinitionBody {
    name: String,
    description: Option<String>,
    spec: Value,
}

/// Validate the name and spec shared by create and update, so the two cannot diverge on what they
/// accept.
fn checked_definition(body: &ReportDefinitionBody) -> Result<&str, ApiError> {
    let name = body.name.trim();
    if name.is_empty() {
        return Err(ApiError::bad_request(
            "invalid_name",
            "report name is required",
        ));
    }
    validate_report_spec(&body.spec)?;
    Ok(name)
}

#[utoipa::path(
    post, path = "/api/v1/reports/definitions", tag = "reports",
    request_body = ReportDefinitionBody,
    responses(
        (status = 200, description = "The created definition", body = reports::ReportDefinition),
        (status = 400, description = "The name is blank, the spec is not an object, or it names a section kind this build cannot render", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
        (status = 413, description = "The spec exceeds the shared JSON-document cap", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode: no write side", body = super::error::ErrorBody),
    ),
)]
async fn create_report_definition(
    _guard: RequireManageConfig,
    State(st): State<ApiState>,
    admin: Admin,
    headers: HeaderMap,
    Json(body): Json<ReportDefinitionBody>,
) -> ApiResult<Json<reports::ReportDefinition>> {
    let name = checked_definition(&body)?;
    let user = current_username(&st, &headers);
    let def = admin
        .reports
        .repo()
        .create_definition(
            name,
            body.description.as_deref(),
            &body.spec,
            user.as_deref(),
        )
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "create report definition",
                "failed to create report definition",
            )
        })?;
    Ok(Json(def))
}

#[utoipa::path(
    put, path = "/api/v1/reports/definitions/{id}", tag = "reports",
    params(("id" = Uuid, Path, description = "Report definition id")),
    request_body = ReportDefinitionBody,
    responses(
        (status = 200, description = "Definition updated", body = Ok_),
        (status = 400, description = "The name is blank, the spec is not an object, or it names a section kind this build cannot render", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
        (status = 404, description = "No such definition", body = super::error::ErrorBody),
        (status = 413, description = "The spec exceeds the shared JSON-document cap", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode: no write side", body = super::error::ErrorBody),
    ),
)]
async fn update_report_definition(
    _guard: RequireManageConfig,
    State(st): State<ApiState>,
    admin: Admin,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<ReportDefinitionBody>,
) -> ApiResult<Json<Ok_>> {
    let name = checked_definition(&body)?;
    let user = current_username(&st, &headers);
    let updated = admin
        .reports
        .repo()
        .update_definition(
            id,
            name,
            body.description.as_deref(),
            &body.spec,
            user.as_deref(),
        )
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "update report definition",
                "failed to update report definition",
            )
        })?;
    if updated {
        Ok(Ok_::yes())
    } else {
        Err(no_definition(id))
    }
}

/// Delete a definition. Schedules cascade; **saved runs are kept** with a null `definition_id` — a
/// generated report is evidence of what the fleet looked like, and deleting the template it came
/// from should not destroy that.
#[utoipa::path(
    delete, path = "/api/v1/reports/definitions/{id}", tag = "reports",
    params(("id" = Uuid, Path, description = "Report definition id")),
    responses(
        (status = 200, description = "Definition deleted; its schedules cascade, its saved runs are kept", body = Ok_),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
        (status = 404, description = "No such definition", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode: no write side", body = super::error::ErrorBody),
    ),
)]
async fn delete_report_definition(
    _guard: RequireManageConfig,
    admin: Admin,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Ok_>> {
    let deleted = admin
        .reports
        .repo()
        .delete_definition(id)
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "delete report definition",
                "failed to delete report definition",
            )
        })?;
    if deleted {
        Ok(Ok_::yes())
    } else {
        Err(no_definition(id))
    }
}

/// Generate a report from a definition now. Returns the run row immediately; it progresses over SSE.
#[utoipa::path(
    post, path = "/api/v1/reports/definitions/{id}/run", tag = "reports",
    params(("id" = Uuid, Path, description = "Report definition id")),
    responses(
        (status = 200, description = "The freshly created run, still generating", body = reports::ReportRun),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
        (status = 404, description = "No such definition", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode: no write side", body = super::error::ErrorBody),
    ),
)]
async fn run_report_definition(
    _guard: RequireManageConfig,
    State(st): State<ApiState>,
    admin: Admin,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<reports::ReportRun>> {
    let user = current_username(&st, &headers);
    admin
        .reports
        .run_now(id, "manual", user)
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "run report",
                "failed to start report generation",
            )
        })?
        .map(Json)
        .ok_or_else(|| no_definition(id))
}

/// Saved runs, newest first. Empty in skeleton mode.
#[utoipa::path(
    get, path = "/api/v1/reports/runs", tag = "reports",
    params(ListQuery),
    responses(
        (status = 200, description = "Saved runs, newest first; empty in skeleton mode", body = Vec<reports::ReportRun>),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks View", body = super::error::ErrorBody),
    ),
)]
async fn list_report_runs(
    _guard: RequireView,
    State(st): State<ApiState>,
    Query(q): Query<ListQuery>,
) -> ApiResult<Json<Vec<reports::ReportRun>>> {
    let Some(admin) = st.admin.as_ref() else {
        return Ok(Json(Vec::new()));
    };
    let limit = q.limit.unwrap_or(50).clamp(1, 500);
    let runs = admin.reports.repo().list_runs(limit).await.map_err(|e| {
        ApiError::from_internal(e.as_ref(), "list report runs", "failed to list report runs")
    })?;
    Ok(Json(runs))
}

/// One run with its rendered result (the viewer).
#[utoipa::path(
    get, path = "/api/v1/reports/runs/{id}", tag = "reports",
    params(("id" = Uuid, Path, description = "Report run id")),
    responses(
        (status = 200, description = "The run with its rendered result", body = reports::ReportRunDetail),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks View", body = super::error::ErrorBody),
        (status = 404, description = "No such run, or skeleton mode", body = super::error::ErrorBody),
    ),
)]
async fn get_report_run(
    _guard: RequireView,
    State(st): State<ApiState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<reports::ReportRunDetail>> {
    let admin = st.admin.as_ref().ok_or_else(|| no_run(id))?;
    admin
        .reports
        .repo()
        .get_run_detail(id)
        .await
        .map_err(|e| {
            ApiError::from_internal(e.as_ref(), "get report run", "failed to load report run")
        })?
        .map(Json)
        .ok_or_else(|| no_run(id))
}

#[utoipa::path(
    delete, path = "/api/v1/reports/runs/{id}", tag = "reports",
    params(("id" = Uuid, Path, description = "Report run id")),
    responses(
        (status = 200, description = "Run deleted", body = Ok_),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
        (status = 404, description = "No such run", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode: no write side", body = super::error::ErrorBody),
    ),
)]
async fn delete_report_run(
    _guard: RequireManageConfig,
    admin: Admin,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Ok_>> {
    let deleted = admin.reports.repo().delete_run(id).await.map_err(|e| {
        ApiError::from_internal(
            e.as_ref(),
            "delete report run",
            "failed to delete report run",
        )
    })?;
    if deleted {
        Ok(Ok_::yes())
    } else {
        Err(no_run(id))
    }
}

/// Export-format query for a report run.
#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub(super) struct ExportQuery {
    format: Option<String>,
}

/// Download a saved run as HTML / CSV / PDF.
///
/// Returns a raw `Response` rather than `Json<T>` — the body is a file with its own content type
/// and `Content-Disposition`, which is exactly the case `Json` cannot express.
///
/// HTML and CSV are rendered from the stored result; PDF is produced on demand. The run must have
/// succeeded: an unfinished or failed run has no rendered result, and that is a `409` (come back
/// later) rather than a 404 (it does not exist).
#[utoipa::path(
    get, path = "/api/v1/reports/runs/{id}/export", tag = "reports",
    params(("id" = Uuid, Path, description = "Report run id"), ExportQuery),
    responses(
        (status = 200, description = "The rendered report as an attachment; `text/html` (default), `text/csv`, or `application/pdf` per `format`", content_type = "text/html"),
        (status = 400, description = "`format` is not html|csv|pdf", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks View", body = super::error::ErrorBody),
        (status = 404, description = "No such run, or skeleton mode", body = super::error::ErrorBody),
        (status = 409, description = "The run has no rendered result yet (still running or failed)", body = super::error::ErrorBody),
        (status = 503, description = "PDF rendering is not available on this server", body = super::error::ErrorBody),
    ),
)]
async fn export_report_run(
    _guard: RequireView,
    State(st): State<ApiState>,
    Path(id): Path<Uuid>,
    Query(q): Query<ExportQuery>,
) -> ApiResult<Response> {
    use axum::http::header::{CONTENT_DISPOSITION, CONTENT_TYPE};
    let admin = st.admin.as_ref().ok_or_else(|| no_run(id))?;
    let detail = admin
        .reports
        .repo()
        .get_run_detail(id)
        .await
        .map_err(|e| {
            ApiError::from_internal(e.as_ref(), "export report run", "failed to load report run")
        })?
        .ok_or_else(|| no_run(id))?;
    let html = detail.result_html.clone().ok_or_else(|| {
        ApiError::conflict(
            "run_not_ready",
            "this report run has no rendered result (still running or failed)",
        )
    })?;
    let format = q.format.as_deref().unwrap_or("html");
    let stem = format!("report-{id}");
    let attachment = |ext: &str| {
        (
            CONTENT_DISPOSITION,
            format!("attachment; filename=\"{stem}.{ext}\""),
        )
    };
    Ok(match format {
        "html" => (
            [
                (CONTENT_TYPE, "text/html; charset=utf-8".to_owned()),
                attachment("html"),
            ],
            html,
        )
            .into_response(),
        "csv" => {
            let json = detail.result_json.unwrap_or(Value::Null);
            let csv = reports::result_json_to_csv(&json);
            (
                [
                    (CONTENT_TYPE, "text/csv; charset=utf-8".to_owned()),
                    attachment("csv"),
                ],
                csv,
            )
                .into_response()
        }
        "pdf" => match html_to_pdf(&html).await {
            Ok(pdf) => (
                [
                    (CONTENT_TYPE, "application/pdf".to_owned()),
                    attachment("pdf"),
                ],
                pdf,
            )
                .into_response(),
            Err(e) => {
                // The renderer is an optional external binary, so its absence is a 503 about this
                // server — not a 500 about the request, which is fine as HTML or CSV.
                tracing::warn!(error = %e, "PDF render failed (wkhtmltopdf)");
                return Err(ApiError::unavailable(
                    "pdf_unavailable",
                    "PDF rendering is unavailable on this server",
                ));
            }
        },
        other => {
            return Err(ApiError::bad_request(
                "invalid_format",
                format!("format must be html|csv|pdf, got {other:?}"),
            ))
        }
    })
}

/// Render HTML to PDF by piping it through `wkhtmltopdf`.
///
/// **The HTML goes in on stdin and the arguments are fixed**, so there is no command injection —
/// which matters because the HTML contains device-supplied strings. Errors if the binary is missing
/// or exits non-zero.
async fn html_to_pdf(html: &str) -> anyhow::Result<Vec<u8>> {
    use tokio::io::AsyncWriteExt;
    use tokio::process::Command;
    let mut child = Command::new("wkhtmltopdf")
        .args([
            "--quiet",
            "--enable-local-file-access",
            "--encoding",
            "utf-8",
            "-", // read HTML from stdin
            "-", // write PDF to stdout
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(html.as_bytes()).await?;
        stdin.shutdown().await?;
    }
    let output = child.wait_with_output().await?;
    if !output.status.success() {
        anyhow::bail!("wkhtmltopdf exited with status {}", output.status);
    }
    Ok(output.stdout)
}

/// All schedules. Empty in skeleton mode.
#[utoipa::path(
    get, path = "/api/v1/reports/schedules", tag = "reports",
    responses(
        (status = 200, description = "Every schedule; empty in skeleton mode", body = Vec<reports::ReportSchedule>),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks View", body = super::error::ErrorBody),
    ),
)]
async fn list_report_schedules(
    _guard: RequireView,
    State(st): State<ApiState>,
) -> ApiResult<Json<Vec<reports::ReportSchedule>>> {
    let Some(admin) = st.admin.as_ref() else {
        return Ok(Json(Vec::new()));
    };
    let list = admin.reports.repo().list_schedules().await.map_err(|e| {
        ApiError::from_internal(
            e.as_ref(),
            "list report schedules",
            "failed to list report schedules",
        )
    })?;
    Ok(Json(list))
}

/// Create/update body for a schedule (preset cadence).
#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct ReportScheduleBody {
    definition_id: Uuid,
    frequency: String,
    day_of_week: Option<i16>,
    day_of_month: Option<i16>,
    at_hour: i16,
    at_minute: i16,
    enabled: Option<bool>,
}

/// Validate a schedule body into a [`ScheduleInput`] plus its first `next_run_at`.
///
/// The frequency is rejected outright, but the day/hour/minute fields are **clamped** rather than
/// rejected — a `day_of_month` of 31 is a reasonable thing for a form to send and there is a
/// defensible answer (28, so it fires every month including February), whereas an unknown frequency
/// has no defensible answer at all.
fn parse_schedule_body(
    body: ReportScheduleBody,
) -> Result<(ScheduleInput, chrono::DateTime<chrono::Utc>), ApiError> {
    if !matches!(body.frequency.as_str(), "daily" | "weekly" | "monthly") {
        return Err(ApiError::bad_request(
            "invalid_frequency",
            "frequency must be daily|weekly|monthly",
        ));
    }
    let day_of_week = if body.frequency == "weekly" {
        Some(body.day_of_week.unwrap_or(0).clamp(0, 6))
    } else {
        None
    };
    let day_of_month = if body.frequency == "monthly" {
        Some(body.day_of_month.unwrap_or(1).clamp(1, 28))
    } else {
        None
    };
    let input = ScheduleInput {
        definition_id: body.definition_id,
        frequency: body.frequency,
        day_of_week,
        day_of_month,
        at_hour: body.at_hour.clamp(0, 23),
        at_minute: body.at_minute.clamp(0, 59),
        enabled: body.enabled.unwrap_or(true),
    };
    let next = reports::compute_next_run(
        &input.frequency,
        input.day_of_week,
        input.day_of_month,
        input.at_hour,
        input.at_minute,
        chrono::Utc::now(),
    );
    Ok((input, next))
}

#[utoipa::path(
    post, path = "/api/v1/reports/schedules", tag = "reports",
    request_body = ReportScheduleBody,
    responses(
        (status = 200, description = "Schedule created", body = CreatedId),
        (status = 400, description = "`frequency` is not daily|weekly|monthly", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode: no write side", body = super::error::ErrorBody),
    ),
)]
async fn create_report_schedule(
    _guard: RequireManageConfig,
    State(st): State<ApiState>,
    admin: Admin,
    headers: HeaderMap,
    Json(body): Json<ReportScheduleBody>,
) -> ApiResult<Json<CreatedId>> {
    let (input, next) = parse_schedule_body(body)?;
    let user = current_username(&st, &headers);
    let id = admin
        .reports
        .repo()
        .create_schedule(&input, next, user.as_deref())
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "create report schedule",
                "failed to create report schedule",
            )
        })?;
    Ok(Json(CreatedId { id }))
}

/// Update a schedule. Recomputes `next_run_at` from the new cadence, so an edit takes effect at the
/// next fire rather than at the old one.
#[utoipa::path(
    put, path = "/api/v1/reports/schedules/{id}", tag = "reports",
    params(("id" = Uuid, Path, description = "Report schedule id")),
    request_body = ReportScheduleBody,
    responses(
        (status = 200, description = "Schedule updated and `next_run_at` recomputed", body = Ok_),
        (status = 400, description = "`frequency` is not daily|weekly|monthly", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
        (status = 404, description = "No such schedule", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode: no write side", body = super::error::ErrorBody),
    ),
)]
async fn update_report_schedule(
    _guard: RequireManageConfig,
    State(st): State<ApiState>,
    admin: Admin,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<ReportScheduleBody>,
) -> ApiResult<Json<Ok_>> {
    let (input, next) = parse_schedule_body(body)?;
    let user = current_username(&st, &headers);
    let updated = admin
        .reports
        .repo()
        .update_schedule(id, &input, next, user.as_deref())
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "update report schedule",
                "failed to update report schedule",
            )
        })?;
    if updated {
        Ok(Ok_::yes())
    } else {
        Err(ApiError::not_found(
            "schedule_not_found",
            format!("no report schedule {id}"),
        ))
    }
}

#[utoipa::path(
    delete, path = "/api/v1/reports/schedules/{id}", tag = "reports",
    params(("id" = Uuid, Path, description = "Report schedule id")),
    responses(
        (status = 200, description = "Schedule deleted", body = Ok_),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
        (status = 404, description = "No such schedule", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode: no write side", body = super::error::ErrorBody),
    ),
)]
async fn delete_report_schedule(
    _guard: RequireManageConfig,
    admin: Admin,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Ok_>> {
    let deleted = admin
        .reports
        .repo()
        .delete_schedule(id)
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "delete report schedule",
                "failed to delete report schedule",
            )
        })?;
    if deleted {
        Ok(Ok_::yes())
    } else {
        Err(ApiError::not_found(
            "schedule_not_found",
            format!("no report schedule {id}"),
        ))
    }
}

/// Live report-run status stream: each event is the run JSON with its current state and progress.
///
/// A lagged subscriber gets a `resync` event rather than a dropped connection, so the client knows
/// to refetch instead of silently showing a stale run forever.
#[utoipa::path(
    get, path = "/api/v1/stream/report-runs", tag = "reports",
    responses(
        (status = 200, description = "Server-sent event stream; each `data` is a run JSON, and a `resync` event means the subscriber lagged", content_type = "text/event-stream"),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks View", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode: no write side", body = super::error::ErrorBody),
    ),
)]
async fn stream_report_runs(_guard: RequireView, admin: Admin) -> Response {
    let stream = tokio_stream::wrappers::BroadcastStream::new(admin.reports.subscribe())
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
    use axum::body::{to_bytes, Body};
    use axum::http::{header::AUTHORIZATION, Request, StatusCode};
    use tower::ServiceExt;
    use yagra_common::{Principal, Role, Scope};

    const ID: &str = "00000000-0000-0000-0000-000000000001";

    fn write_routes() -> Vec<(&'static str, String)> {
        vec![
            ("POST", "/api/v1/reports/definitions".to_owned()),
            ("PUT", format!("/api/v1/reports/definitions/{ID}")),
            ("DELETE", format!("/api/v1/reports/definitions/{ID}")),
            ("POST", format!("/api/v1/reports/definitions/{ID}/run")),
            ("DELETE", format!("/api/v1/reports/runs/{ID}")),
            ("POST", "/api/v1/reports/schedules".to_owned()),
            ("PUT", format!("/api/v1/reports/schedules/{ID}")),
            ("DELETE", format!("/api/v1/reports/schedules/{ID}")),
        ]
    }

    async fn send(
        st: ApiState,
        method: &str,
        path: &str,
        token: Option<&str>,
    ) -> axum::response::Response {
        let mut b = Request::builder()
            .method(method)
            .uri(path)
            .header("content-type", "application/json");
        if let Some(t) = token {
            b = b.header(AUTHORIZATION, format!("Bearer {t}"));
        }
        router(st)
            .oneshot(b.body(Body::from("{}")).unwrap())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn everyone_reads_and_only_admins_write() {
        for (method, path) in write_routes() {
            assert_eq!(
                send(private_state(), method, &path, None).await.status(),
                StatusCode::UNAUTHORIZED,
                "anon {method} {path}"
            );
            // Writes stay closed even on a public dashboard — a report is a shared resource.
            assert_eq!(
                send(public_state(), method, &path, None).await.status(),
                StatusCode::UNAUTHORIZED,
                "public {method} {path}"
            );
        }
        let st = private_state();
        for role in [Role::Viewer, Role::Operator] {
            let token = st
                .sessions
                .issue(Uuid::new_v4(), Principal::new(role, Scope::All), "u");
            for (method, path) in write_routes() {
                assert_eq!(
                    send(st.clone(), method, &path, Some(&token)).await.status(),
                    StatusCode::FORBIDDEN,
                    "{role:?} {method} {path}"
                );
            }
        }
    }

    #[tokio::test]
    async fn the_section_catalog_is_static_and_answers_without_a_database() {
        // It drives the builder, so it has to work before anything else does.
        let resp = send(public_state(), "GET", "/api/v1/reports/sections", None).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = to_bytes(resp.into_body(), 256 * 1024).await.unwrap();
        let json: Value = serde_json::from_slice(&bytes).unwrap();
        assert!(!json.as_array().expect("an array").is_empty());
    }

    #[tokio::test]
    async fn lists_are_empty_without_a_database_and_single_reads_are_404() {
        // The distinction is deliberate: "no reports exist" is true for a list, and "no report run
        // 123" is true for an item — the viewer needs the latter to show its not-found state.
        for path in [
            "/api/v1/reports/definitions",
            "/api/v1/reports/runs",
            "/api/v1/reports/schedules",
        ] {
            let resp = send(public_state(), "GET", path, None).await;
            assert_eq!(resp.status(), StatusCode::OK, "{path}");
            let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
            assert_eq!(
                serde_json::from_slice::<Value>(&bytes).unwrap(),
                serde_json::json!([]),
                "{path}"
            );
        }
        for path in [
            format!("/api/v1/reports/definitions/{ID}"),
            format!("/api/v1/reports/runs/{ID}"),
            format!("/api/v1/reports/runs/{ID}/export"),
        ] {
            assert_eq!(
                send(public_state(), "GET", &path, None).await.status(),
                StatusCode::NOT_FOUND,
                "{path}"
            );
        }
    }

    #[test]
    fn a_spec_naming_an_unknown_section_is_rejected_at_the_edge() {
        // Otherwise it stores fine and fails at generation time, long after the operator left.
        assert!(validate_report_spec(&serde_json::json!({ "sections": [] })).is_ok());
        let bad = serde_json::json!({ "sections": [{ "kind": "not-a-section" }] });
        assert_eq!(
            validate_report_spec(&bad).unwrap_err().code(),
            "unknown_section"
        );
        // Shape and size are the other two the server can be responsible for.
        assert_eq!(
            validate_report_spec(&serde_json::json!([]))
                .unwrap_err()
                .code(),
            "invalid_spec"
        );
        let huge = serde_json::json!({ "x": "y".repeat(MAX_REPORT_SPEC_BYTES) });
        assert_eq!(
            validate_report_spec(&huge).unwrap_err().code(),
            "spec_too_large"
        );
    }

    #[test]
    fn an_unknown_cadence_is_rejected_but_an_out_of_range_day_is_clamped() {
        let body = |freq: &str, dom: Option<i16>| ReportScheduleBody {
            definition_id: Uuid::nil(),
            frequency: freq.to_owned(),
            day_of_week: None,
            day_of_month: dom,
            at_hour: 99,
            at_minute: 99,
            enabled: None,
        };
        assert_eq!(
            parse_schedule_body(body("yearly", None))
                .unwrap_err()
                .code(),
            "invalid_frequency"
        );
        // 31 clamps to 28 so a monthly report fires every month, February included, rather than
        // being rejected or silently skipping.
        let (input, _) = parse_schedule_body(body("monthly", Some(31))).unwrap();
        assert_eq!(input.day_of_month, Some(28));
        assert_eq!(input.at_hour, 23);
        assert_eq!(input.at_minute, 59);
        // A daily schedule carries neither day field.
        let (daily, _) = parse_schedule_body(body("daily", Some(5))).unwrap();
        assert_eq!(daily.day_of_month, None);
        assert_eq!(daily.day_of_week, None);
    }
}
