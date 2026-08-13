// SPDX-License-Identifier: AGPL-3.0-only
//! The audit log (Settings ▸ Audit) — who changed or acknowledged what.
//!
//! Read-only here: entries are written by `audit_mw`, which covers every mutating `/api/v1` request
//! via a deny-list, so there is no endpoint that appends one. Its own permission
//! ([`Permission::ViewAudit`](yagra_common::Permission)) rather than `ManageConfig`, because the log
//! names actions taken across domains the caller may not otherwise be allowed to see.

use super::error::{ApiError, ApiResult};
use super::extract::RequireViewAudit;
use super::util::{normalize_search, parse_rfc3339};
use super::ApiState;
use crate::audit::{AuditAction, AuditFilter, AuditStatusClass};
use axum::response::IntoResponse;
use axum::{
    extract::{Query, State},
    routing::get,
    Json, Router,
};
use serde::Deserialize;

/// This domain's slice of the OpenAPI document (ADR-035), merged by [`super::openapi::document`].
///
/// The two filter enums are registered as components, and since ADR-053 Inc.4b **nothing in this
/// document references them** — the query parameters are comma-separated strings now, so the
/// `$ref`s that used to require this registration are gone.
///
/// They stay registered on purpose. `web/src/types/api.ts` asserts its `AUDIT_ACTIONS` /
/// `AUDIT_STATUS_CLASSES` runtime arrays equal `components['schemas']['AuditAction']` /
/// `['AuditStatusClass']`, which is what stops the WebUI's option list from drifting from the
/// vocabulary the backend accepts. Dropping these two lines because "nothing points at them" would
/// delete that check silently — the arrays would still compile, and a seventh action would appear
/// in Rust and never in the filter row.
#[derive(utoipa::OpenApi)]
#[openapi(
    paths(list_audit, export_audit),
    components(schemas(AuditAction, AuditStatusClass))
)]
pub(super) struct Doc;

/// The audit routes, merged into `/api/v1` by [`super::router`].
pub(super) fn routes() -> Router<ApiState> {
    Router::new()
        .route("/api/v1/audit", get(list_audit))
        .route("/api/v1/audit/export.csv", get(export_audit))
}

/// Ceiling on one export.
///
/// Far above the page limit and still a limit: an unbounded `SELECT` over a table that grows with
/// every request the deployment serves is a memory and a wall-clock risk, and this endpoint renders
/// the whole answer into a `String` before sending it. An export that reaches this says so **in the
/// file** — see [`export_audit`].
const EXPORT_MAX: i64 = 50_000;

/// A page of the audit log, with the filters the Settings ▸ Audit toolbar offers.
///
/// `before` pages; `since`/`until` bound the window being searched. They are separate parameters on
/// purpose — one is where this page starts, the other is what the operator asked to look at.
#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub(super) struct AuditQuery {
    /// Max rows (1–500, default 100).
    limit: Option<i64>,
    /// Keyset cursor: return rows strictly older than this RFC 3339 timestamp.
    before: Option<String>,
    /// Only entries at or after this RFC 3339 timestamp.
    since: Option<String>,
    /// Only entries at or before this RFC 3339 timestamp.
    until: Option<String>,
    /// Free text matched against the username and the action (case-insensitive substring).
    q: Option<String>,
    /// Comma-separated action kinds (`post`, `put`, `patch`, `delete`, `login`, `mcp`); empty or
    /// absent means every kind. An unknown token is rejected rather than ignored.
    action: Option<String>,
    /// Comma-separated status classes (`ok`, `client`, `server`); empty or absent means every
    /// class.
    status: Option<String>,
}

#[utoipa::path(
    get, path = "/api/v1/audit", tag = "audit",
    params(AuditQuery),
    responses(
        (status = 200, description = "One page of audit rows, newest first", body = Vec<crate::audit::AuditRow>),
        (status = 400, description = "A cursor or range bound is not RFC 3339, or `action`/`status` is not one of the listed values", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks the view-audit permission", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode keeps no audit log", body = super::error::ErrorBody),
    ),
)]
// Extractor order is `RequireViewAudit` → `Query`, and there is deliberately **no `Admin`**.
// The house rule is `Require*` first — authenticate before anything else, so an anonymous caller
// learns only that it is anonymous and never which subsystems this deployment has. What follows is
// the reason `list_alert_history` states about its cursor: a malformed filter is the client's bug
// whether or not this deployment keeps a log, and answering 503 to it would tell a client walking
// the log "there is nothing here" when the truth is "your request was wrong". It leaks nothing —
// whether the parse fails depends only on the request.
//
// **`Admin` was dropped from the signature and the availability check moved into [`audit_page`],
// after the parse.** Until ADR-053 Inc.4b that ordering held for `action`/`status` only, because
// they were typed and serde rejected them during extraction, while the RFC 3339 bounds were parsed
// in the handler and therefore lost to `Admin`'s 503 on a skeleton deployment — a wart this comment
// used to describe as accepted. Making the two enums comma-separated moved their parse into the
// handler too, which would have made the wart the *rule*. So the wart was fixed instead: every
// filter is now validated before availability is consulted, on both this endpoint and the export.
//
// ⚠️ The thing `Admin` existed to stop being forgettable is still owed — [`audit_page`] must
// resolve `st.admin` itself and return `ApiError::admin_unavailable()`, and
// `a_well_formed_filter_still_reports_the_missing_write_side` is what fails if it stops doing so.
// Do not "tidy" that check away, and do not re-add the extractor: with both, the 503 wins again.
async fn list_audit(
    _guard: RequireViewAudit,
    Query(q): Query<AuditQuery>,
    State(st): State<ApiState>,
) -> ApiResult<Json<Vec<crate::audit::AuditRow>>> {
    Ok(Json(
        audit_page(
            &st,
            AuditFilterInput {
                limit: q.limit,
                before: q.before.as_deref(),
                since: q.since.as_deref(),
                until: q.until.as_deref(),
                q: q.q.as_deref(),
                action: q.action.as_deref(),
                status: q.status.as_deref(),
            },
        )
        .await?,
    ))
}

/// The audit log as CSV, narrowed by the same filters as the list.
///
/// **This is the endpoint that makes Export mean what it says.** The button used to write out the
/// rows the browser had scrolled to, which is a different set from "everything matching" — in a log
/// whose purpose is completeness, that is a correctness problem rather than a missing feature. The
/// list endpoint's filters moved into SQL first; this closes the other half.
///
/// `before` is deliberately **not** accepted: a cursor is where a page starts, and an export is not
/// paged. Accepting it would let a caller export "the second page" and believe it was the answer.
///
/// The response is a download rather than JSON, so a failure has nowhere useful to render — which
/// is why the filter is validated the same way the list validates it, before anything is fetched.
#[utoipa::path(
    get, path = "/api/v1/audit/export.csv", tag = "audit",
    params(AuditExportQuery),
    responses(
        (status = 200, description = "Every matching entry as CSV, newest first, capped", content_type = "text/csv"),
        (status = 400, description = "A range bound is not RFC 3339, or `action`/`status` is not one of the listed values", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks the view-audit permission", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode keeps no audit log", body = super::error::ErrorBody),
    ),
)]
async fn export_audit(
    _guard: RequireViewAudit,
    Query(q): Query<AuditExportQuery>,
    State(st): State<ApiState>,
) -> Result<axum::response::Response, ApiError> {
    let rows = audit_page(
        &st,
        AuditFilterInput {
            limit: Some(EXPORT_MAX),
            before: None,
            since: q.since.as_deref(),
            until: q.until.as_deref(),
            q: q.q.as_deref(),
            action: q.action.as_deref(),
            status: q.status.as_deref(),
        },
    )
    .await?;
    Ok((
        [
            (
                axum::http::header::CONTENT_TYPE,
                "text/csv; charset=utf-8".to_owned(),
            ),
            (
                axum::http::header::CONTENT_DISPOSITION,
                "attachment; filename=\"audit-log.csv\"".to_owned(),
            ),
        ],
        render_audit_csv(&rows, EXPORT_MAX),
    )
        .into_response())
}

/// Render audit rows as CSV, appending a notice row when the export hit its ceiling.
///
/// The notice is a **row in the file**, not a header. A browser download discards response headers,
/// so anything said there is said to nobody; a truncated CSV that does not admit it is the same lie
/// the client-side export used to tell, moved server-side. A spreadsheet shows this as a final row.
///
/// Every field — the notice included — goes through [`crate::csv::field`], which neutralizes
/// spreadsheet formulas. That matters most here: `username` is whatever a **failed** sign-in
/// submitted, so an unauthenticated stranger chooses it.
fn render_audit_csv(rows: &[crate::audit::AuditRow], cap: i64) -> String {
    let mut out = String::new();
    out.push_str(&crate::csv::row(&["time", "user", "action", "status"]));
    for r in rows {
        out.push_str("\r\n");
        let status = r.status.to_string();
        out.push_str(&crate::csv::row(&[&r.at, &r.username, &r.action, &status]));
    }
    if i64::try_from(rows.len()).unwrap_or(i64::MAX) >= cap {
        out.push_str("\r\n");
        out.push_str(&crate::csv::row(&[&format!(
            "truncated: only the newest {cap} matching entries were exported — narrow the time range"
        )]));
    }
    out
}

/// The export's filters: the list's, minus the cursor.
#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub(super) struct AuditExportQuery {
    /// Only entries at or after this RFC 3339 timestamp.
    since: Option<String>,
    /// Only entries at or before this RFC 3339 timestamp.
    until: Option<String>,
    /// Free text matched against the username and the action (case-insensitive substring).
    q: Option<String>,
    /// Comma-separated action kinds (`post`, `put`, `patch`, `delete`, `login`, `mcp`); empty or
    /// absent means every kind. An unknown token is rejected rather than ignored.
    action: Option<String>,
    /// Comma-separated status classes (`ok`, `client`, `server`); empty or absent means every
    /// class.
    status: Option<String>,
}

/// The raw, unvalidated filter fields, as either surface receives them.
#[derive(Default)]
pub(crate) struct AuditFilterInput<'a> {
    pub limit: Option<i64>,
    pub before: Option<&'a str>,
    pub since: Option<&'a str>,
    pub until: Option<&'a str>,
    pub q: Option<&'a str>,
    /// Comma-separated action kinds; parsed by [`audit_page`], not by serde. See the long note in
    /// `crate::audit` for why the typed field was given up.
    pub action: Option<&'a str>,
    /// Comma-separated status classes.
    pub status: Option<&'a str>,
}

/// A page of the audit log, newest first — shared by `GET /api/v1/audit` and the MCP `get_audit`
/// tool (ADR-042 I3a).
///
/// **The seam is the whole page function, not just a validator.** `parse_event_filter` shares only
/// the validation with its MCP twin, and the two halves around it drifted anyway — the term cap
/// existed on the REST side and not on the surface with no human in the loop. With parsing, the
/// store call and the error mapping all inside, there is nothing left for a second surface to
/// re-derive.
///
/// **`ViewAudit`, not `View`.** Also note this goes through `AuditRepo::list`, which is where the
/// `1..=MAX_LIMIT` clamp lives — a caller that rebuilt the query itself would inherit no bound.
///
/// ⚠️ **Takes `ApiState`, not `AdminState`, and resolves the write side itself — deliberately.**
/// The whole filter is validated first, so a malformed request answers `400` on a deployment that
/// keeps no log rather than the `503` an `Admin` extractor would have returned before the body ran.
/// See the long comment above `list_audit`.
pub(crate) async fn audit_page(
    st: &ApiState,
    input: AuditFilterInput<'_>,
) -> Result<Vec<crate::audit::AuditRow>, ApiError> {
    // The cursor and the range bounds get different codes: a bad cursor is a client paging bug, a
    // bad bound is operator input, and the UI surfaces them differently. Same split as the event log.
    fn ts(
        value: Option<&str>,
        field: &str,
        code: &'static str,
    ) -> Result<Option<chrono::DateTime<chrono::Utc>>, ApiError> {
        match value {
            None => Ok(None),
            Some(s) => parse_rfc3339(s).map(Some).ok_or_else(|| {
                ApiError::bad_request(code, format!("{field} must be an RFC 3339 timestamp"))
            }),
        }
    }
    // An unparseable cursor is rejected, not dropped: silently returning the newest page instead of
    // the requested one makes a paging bug look like "you have reached the end".
    let filter = AuditFilter {
        before: ts(input.before, "before", "invalid_cursor")?,
        since: ts(input.since, "since", "invalid_filter")?,
        until: ts(input.until, "until", "invalid_filter")?,
        q: normalize_search(input.q),
        // Same set spelling as events and alert history (ADR-053): comma-separated, empty means
        // unfiltered, unknown token is a 400. The allowed-values message is read off the enum so a
        // seventh action cannot be accepted here but missing from the error text.
        action: super::util::parse_set(
            "action",
            input.action,
            &AuditAction::ALL
                .iter()
                .map(|a| a.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            AuditAction::from_token,
        )?,
        status: super::util::parse_set(
            "status",
            input.status,
            &AuditStatusClass::ALL
                .iter()
                .map(|c| c.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            AuditStatusClass::from_token,
        )?,
        limit: input.limit.unwrap_or(crate::audit::DEFAULT_LIMIT),
    };
    // Availability **after** the parse — this is what the `Admin` extractor used to do before the
    // body ran, and moving it here is the whole point (see `list_audit`). Still a 503 rather than
    // an empty page: "nobody has done anything" and "this deployment keeps no audit log" must not
    // read alike.
    let Some(admin) = st.admin.as_ref() else {
        return Err(ApiError::admin_unavailable());
    };
    admin.audit.list(&filter).await.map_err(|e| {
        ApiError::from_internal(e.as_ref(), "list audit log", "failed to list the audit log")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::router;
    use crate::api::tests_support::{private_state, public_state};
    use axum::body::Body;
    use axum::http::{header::AUTHORIZATION, Request, StatusCode};
    use tower::ServiceExt;
    use uuid::Uuid;
    use yagra_common::{Principal, Role, Scope};

    async fn status_of(st: ApiState, path: &str, token: Option<&str>) -> StatusCode {
        let mut b = Request::builder().method("GET").uri(path);
        if let Some(t) = token {
            b = b.header(AUTHORIZATION, format!("Bearer {t}"));
        }
        router(st)
            .oneshot(b.body(Body::empty()).unwrap())
            .await
            .unwrap()
            .status()
    }

    #[tokio::test]
    async fn an_anonymous_caller_is_told_it_is_anonymous_and_nothing_else() {
        // `Require*` before `Admin`: 401, never the 503 that would say whether this deployment has
        // a write side. It answered 503 first before the migration.
        assert_eq!(
            status_of(private_state(), "/api/v1/audit", None).await,
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn the_audit_log_is_closed_on_a_public_dashboard() {
        // Reads are open in public-dashboard mode — this one is not. The log names operators and
        // the configuration changes they made.
        assert_eq!(
            status_of(public_state(), "/api/v1/audit", None).await,
            StatusCode::UNAUTHORIZED
        );
    }

    // The RFC 3339 bounds are **not** checked here, and the test says so rather than pretending.
    // They are validated inside `audit_page`, behind the live store, so on `private_state()` the
    // honest answer is the 503 asserted by `a_well_formed_filter_still_reports_the_missing_write_side`.
    // Naming that beats a test whose name claims more than it checks (the same note `mcp/tools.rs`
    // leaves on its own audit test).

    #[tokio::test]
    async fn an_unknown_action_or_status_class_is_rejected_rather_than_ignored() {
        // Rejecting beats ignoring: a typo that silently dropped the filter would answer with the
        // whole log while the operator believes they are looking at DELETEs only. The rejection is
        // serde's, from the closed enum — which is also why the same words cannot mean something
        // different over MCP.
        let st = private_state();
        let token = st
            .sessions
            .issue(Uuid::new_v4(), Principal::new(Role::Admin, Scope::All), "a");
        for bad in [
            "/api/v1/audit?action=GET",  // real method, not an audited one
            "/api/v1/audit?action=POST", // right idea, wrong case — tokens are lowercase
            "/api/v1/audit?action=sign-in",
            "/api/v1/audit?status=2xx",
            "/api/v1/audit?status=success",
        ] {
            assert_eq!(
                status_of(st.clone(), bad, Some(&token)).await,
                StatusCode::BAD_REQUEST,
                "{bad}"
            );
        }
    }

    #[tokio::test]
    async fn a_well_formed_filter_still_reports_the_missing_write_side() {
        // The other half of the ordering rule: once the request itself is fine, the answer is about
        // the deployment again — 503, not a misleading empty page.
        let st = private_state();
        let token = st
            .sessions
            .issue(Uuid::new_v4(), Principal::new(Role::Admin, Scope::All), "a");
        assert_eq!(
            status_of(
                st,
                "/api/v1/audit?action=delete&status=ok&q=admin&limit=10",
                Some(&token)
            )
            .await,
            StatusCode::SERVICE_UNAVAILABLE
        );
    }

    #[tokio::test]
    async fn a_role_without_view_audit_is_403_not_401() {
        let st = private_state();
        for role in [Role::Viewer, Role::Operator] {
            let token = st
                .sessions
                .issue(Uuid::new_v4(), Principal::new(role, Scope::All), "u");
            assert_eq!(
                status_of(st.clone(), "/api/v1/audit", Some(&token)).await,
                StatusCode::FORBIDDEN,
                "{role:?}"
            );
        }
    }

    // ── Export ──────────────────────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn the_export_is_gated_exactly_as_the_list_is() {
        // A download is the easiest thing to leave open by accident: it is reached by navigation
        // rather than by the API client, so a missing guard would not show up as a failing fetch.
        let st = private_state();
        assert_eq!(
            status_of(st.clone(), "/api/v1/audit/export.csv", None).await,
            StatusCode::UNAUTHORIZED
        );
        for role in [Role::Viewer, Role::Operator] {
            let token = st
                .sessions
                .issue(Uuid::new_v4(), Principal::new(role, Scope::All), "u");
            assert_eq!(
                status_of(st.clone(), "/api/v1/audit/export.csv", Some(&token)).await,
                StatusCode::FORBIDDEN,
                "{role:?}"
            );
        }
    }

    #[tokio::test]
    async fn a_malformed_export_filter_is_rejected_before_anything_is_fetched() {
        // The response is a file download, so there is nowhere useful to render an error once the
        // browser has started saving it. Same ordering as the list.
        let st = private_state();
        let token = st
            .sessions
            .issue(Uuid::new_v4(), Principal::new(Role::Admin, Scope::All), "a");
        assert_eq!(
            status_of(
                st.clone(),
                "/api/v1/audit/export.csv?action=GET",
                Some(&token)
            )
            .await,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            status_of(st, "/api/v1/audit/export.csv?status=2xx", Some(&token)).await,
            StatusCode::BAD_REQUEST
        );
    }

    #[tokio::test]
    async fn the_export_takes_no_cursor() {
        // `before` is where a *page* starts. An export is not paged, and accepting the parameter
        // would let a caller export the second page and believe it was the whole answer. Serde
        // ignores unknown query keys, so the proof is that it changes nothing — not that it 400s.
        let st = private_state();
        let token = st
            .sessions
            .issue(Uuid::new_v4(), Principal::new(Role::Admin, Scope::All), "a");
        // Reaches the store (503 in skeleton mode) rather than being rejected: the key is simply
        // not part of this query's shape.
        assert_eq!(
            status_of(
                st,
                "/api/v1/audit/export.csv?before=2026-01-01T00:00:00Z",
                Some(&token)
            )
            .await,
            StatusCode::SERVICE_UNAVAILABLE
        );
    }

    fn row(at: &str, username: &str, action: &str, status: i32) -> crate::audit::AuditRow {
        crate::audit::AuditRow {
            id: Uuid::new_v4(),
            at: at.to_owned(),
            username: username.to_owned(),
            action: action.to_owned(),
            status,
        }
    }

    #[test]
    fn the_export_neutralizes_a_username_a_stranger_chose() {
        // The audit log records the username submitted to a **failed** sign-in, so anyone who can
        // reach the sign-in page can plant this. It must arrive as text.
        let csv = render_audit_csv(
            &[row(
                "2026-08-13T00:00:00Z",
                "=HYPERLINK(\"http://evil\")",
                "auth.login",
                401,
            )],
            EXPORT_MAX,
        );
        assert!(
            csv.contains("\"'=HYPERLINK"),
            "the formula was not neutralized: {csv}"
        );
    }

    #[test]
    fn the_export_says_so_when_it_was_cut_short() {
        // A truncated file that does not admit it is the same lie the client-side export told,
        // moved server-side. The notice is a ROW because a browser download discards headers.
        let rows: Vec<_> = (0..3)
            .map(|i| row("2026-08-13T00:00:00Z", "admin", "DELETE /x", i))
            .collect();
        let complete = render_audit_csv(&rows, 10);
        assert!(!complete.contains("truncated"), "{complete}");
        let cut = render_audit_csv(&rows, 3);
        assert!(cut.lines().last().unwrap().contains("truncated"), "{cut}");
    }

    #[test]
    fn the_export_header_names_the_columns_the_rows_carry() {
        // Four columns, one header. A header that drifts from the row builder silently mislabels
        // every column after the one that moved.
        let csv = render_audit_csv(&[row("t", "u", "a", 200)], EXPORT_MAX);
        let mut lines = csv.lines();
        assert_eq!(
            lines.next().unwrap(),
            "\"time\",\"user\",\"action\",\"status\""
        );
        assert_eq!(lines.next().unwrap(), "\"t\",\"u\",\"a\",\"200\"");
        assert!(lines.next().is_none());
    }
}
