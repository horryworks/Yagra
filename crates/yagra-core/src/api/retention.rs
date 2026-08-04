// SPDX-License-Identifier: AGPL-3.0-only
//! Data-retention policy: how long each store keeps what, and the windows an operator can change
//! (ADR-040).
//!
//! The read returns the whole table rather than just the editable numbers, because the useful
//! question is "how long is anything kept here", and answering half of it — the half Yagra happens
//! to control — is how the retention story ended up spread across eight files with no statement of
//! policy anywhere.
//!
//! Two rows are read-only, and the read fetches their value from the store itself rather than
//! mirroring a number Yagra was told at boot. VictoriaMetrics and VictoriaLogs keep retention in a
//! process start flag with no runtime API, so the value Yagra could report from its own config
//! would be a claim about a file it does not own; the value the store reports is the value the
//! store is enforcing. When it cannot be read, the row says so — it never falls back to a guess.
//!
//! This lives on `/api/v1/settings/retention` rather than joining `/api/v1/config` deliberately:
//! that endpoint is the **unauthenticated** pre-login bootstrap blob, and a deployment's retention
//! policy is not something to hand to anonymous callers.

use super::error::{ApiError, ApiResult};
use super::extract::{Admin, RequireManageConfig, RequireView};
use super::ApiState;
use crate::retention::{self, RetentionSettings, Subject};
use axum::{extract::State, http::StatusCode, routing::get, Json, Router};
use serde::{Deserialize, Serialize};

/// This domain's slice of the OpenAPI document (ADR-035), merged by [`super::openapi::document`].
#[derive(utoipa::OpenApi)]
#[openapi(paths(get_retention, update_retention))]
pub(super) struct Doc;

/// The retention routes, merged into `/api/v1` by [`super::router`].
pub(super) fn routes() -> Router<ApiState> {
    Router::new().route(
        "/api/v1/settings/retention",
        get(get_retention).put(update_retention),
    )
}

/// The operator-editable retention windows.
#[derive(Debug, Serialize, Deserialize, utoipa::ToSchema)]
pub(crate) struct RetentionValues {
    /// Days to keep alert history, node-state snapshots, DNS chain changes and matched events.
    pub alert_linked_days: u32,
    /// Hours to keep passive events that matched no rule.
    pub unmatched_event_hours: u32,
    /// Days to keep generated report runs.
    pub report_run_days: u32,
    /// Days to keep traffic-flow records, applied as a ClickHouse table TTL.
    pub flow_days: u32,
}

/// One line of the retention table.
// Field rationale that clients have no use for stays in `//`: every `///` below is published
// verbatim to API consumers (and onto the public API reference), so it is written for them.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct RetentionRow {
    /// Stable identifier for the retained data, e.g. `alert_history`.
    subject: String,
    /// The store that holds it.
    store: String,
    /// How the window is applied: `pg_prune`, `store_ttl`, `store_flag` or `unlimited`.
    enforcement: String,
    /// Where it can be changed: `settings` (here), `store_flag_read_only` (the store's own
    /// command-line flag), or `by_decision` (not retained on a schedule at all).
    tunable: String,
    /// Which field of `settings` this row binds to, or `store_owned` / `unlimited`.
    field: String,
    /// Unit of `value`: `days`, `hours`, or empty when the row has no configurable number.
    unit: String,
    /// The configured window, for rows this deployment controls.
    value: Option<u32>,
    /// What the store itself reports for a `store_flag` row, verbatim (e.g. `12`, `30d`). Absent
    /// when the store is unreachable or is running its own default — in which case the value is
    /// genuinely unknown and is not guessed.
    store_reported: Option<String>,
    /// Whether the backing store is configured in this deployment. A row for an unconfigured
    /// optional store retains nothing.
    store_configured: bool,
    /// Operator-facing explanation. For a read-only row it names the flag that does change it.
    note: String,
}

/// The full retention policy: the editable windows plus every row of the table.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct RetentionPolicy {
    /// The windows this endpoint can change.
    settings: RetentionValues,
    /// Every retained subject, including the ones no API can change.
    rows: Vec<RetentionRow>,
}

#[utoipa::path(
    get, path = "/api/v1/settings/retention", tag = "settings",
    responses(
        (status = 200, description = "The deployment's retention policy: editable windows plus the full table, including rows set by a store's own start flag", body = RetentionPolicy),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks View", body = super::error::ErrorBody),
        (status = 503, description = "Inventory storage is unavailable (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn get_retention(
    _guard: RequireView,
    admin: Admin,
    State(st): State<ApiState>,
) -> ApiResult<Json<RetentionPolicy>> {
    let settings = admin.repo.get_retention_settings().await;
    // Ask the two stores that own their own retention what they are actually enforcing. Both are
    // one cheap HTTP GET and both degrade to `None` (rendered as unknown) rather than failing the
    // whole page — this is the screen an operator opens to find out what is going on.
    let metrics_reported = st.store.retention_flag().await;
    let logs_reported = match st.logs.as_ref() {
        Some(logs) => logs.retention_flag().await,
        None => None,
    };

    let rows = Subject::ALL
        .iter()
        .map(|&subject| {
            let row = subject.row();
            let store_reported = match subject {
                Subject::Metrics => metrics_reported.clone(),
                Subject::EventLogStore => logs_reported.clone(),
                _ => None,
            };
            RetentionRow {
                subject: subject.as_str().to_owned(),
                store: row.store.to_owned(),
                enforcement: row.enforcement.as_str().to_owned(),
                tunable: row.tunable.as_str().to_owned(),
                field: row.field.as_str().to_owned(),
                unit: row.unit().to_owned(),
                value: subject.configured(&settings),
                store_reported,
                store_configured: store_configured(&st, subject),
                note: row.note.to_owned(),
            }
        })
        .collect();

    Ok(Json(RetentionPolicy {
        settings: to_values(&settings),
        rows,
    }))
}

/// Update the retention windows. Applies immediately: the PostgreSQL prune loops re-read the policy
/// on their next tick, and the flow store's table TTL is altered before this returns.
#[utoipa::path(
    put, path = "/api/v1/settings/retention", tag = "settings",
    request_body = RetentionValues,
    responses(
        (status = 204, description = "Retention updated. Lowering a window deletes data older than it on the next prune"),
        (status = 400, description = "A window is outside the allowed range", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
        (status = 502, description = "The flow store rejected the retention change; nothing was saved", body = super::error::ErrorBody),
        (status = 503, description = "Inventory storage is unavailable (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn update_retention(
    _guard: RequireManageConfig,
    admin: Admin,
    State(st): State<ApiState>,
    Json(body): Json<RetentionValues>,
) -> ApiResult<StatusCode> {
    let next = RetentionSettings {
        alert_linked_days: body.alert_linked_days,
        unmatched_event_hours: body.unmatched_event_hours,
        report_run_days: body.report_run_days,
        flow_days: body.flow_days,
    };
    if !next.in_bounds() {
        return Err(ApiError::bad_request(
            "invalid_retention",
            format!(
                "day windows must be {}-{} and the unmatched-event window {}-{} hours",
                retention::MIN_RETENTION_DAYS,
                retention::MAX_RETENTION_DAYS,
                retention::MIN_RETENTION_HOURS,
                retention::MAX_RETENTION_HOURS,
            ),
        ));
    }

    // The flow store first, then PostgreSQL. If the ALTER fails nothing is saved, so the two cannot
    // disagree; if the save failed after a successful ALTER, startup reconciles ClickHouse back to
    // the stored value (`ensure_schema` calls `sync_ttl`). The other ordering has no such recovery.
    if let Some(flows) = st.flows.as_ref() {
        flows
            .set_retention_days(next.flow_days)
            .await
            .map_err(|e| {
                // The store's own text stays in the log, never in the response (security.md).
                tracing::warn!(error = %e, days = next.flow_days, "flow retention change failed");
                ApiError::bad_gateway(
                    "flow_retention_failed",
                    "the flow store rejected the retention change; nothing was saved",
                )
            })?;
    }
    admin
        .repo
        .set_retention_settings(&next)
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "set retention settings",
                "failed to update retention settings",
            )
        })?;
    Ok(StatusCode::NO_CONTENT)
}

/// Whether the store behind a subject is configured here. The optional stores (ClickHouse for flow,
/// VictoriaLogs for the event log) retain nothing when absent, and a row that showed a window for a
/// store this deployment does not run would be describing something that never happens.
fn store_configured(st: &ApiState, subject: Subject) -> bool {
    match subject {
        Subject::FlowRecords => st.flows.is_some(),
        Subject::EventLogStore => st.logs.is_some(),
        Subject::AlertHistory
        | Subject::NodeStateSnapshots
        | Subject::DnsChainChanges
        | Subject::NeighborChanges
        | Subject::L3Changes
        | Subject::EventsMatched
        | Subject::EventsUnmatched
        | Subject::ReportRuns
        | Subject::AuditLog
        | Subject::Metrics => true,
    }
}

fn to_values(s: &RetentionSettings) -> RetentionValues {
    RetentionValues {
        alert_linked_days: s.alert_linked_days,
        unmatched_event_hours: s.unmatched_event_hours,
        report_run_days: s.report_run_days,
        flow_days: s.flow_days,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::tests_support::{private_state, public_state};
    use crate::retention::Tunable;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    fn app(state: ApiState) -> Router {
        crate::api::router(state)
    }

    fn put_body(v: &str) -> Request<Body> {
        Request::builder()
            .method("PUT")
            .uri("/api/v1/settings/retention")
            .header("content-type", "application/json")
            .body(Body::from(v.to_owned()))
            .unwrap()
    }

    /// Gated before the store is consulted: an anonymous caller must not learn from the status code
    /// whether this deployment even has an inventory (api-conventions).
    #[tokio::test]
    async fn anonymous_is_unauthorized_not_unavailable() {
        let resp = app(private_state())
            .oneshot(
                Request::builder()
                    .uri("/api/v1/settings/retention")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn anonymous_cannot_write_even_on_a_public_dashboard() {
        // public_dashboard opens reads only; a write guard stays shut.
        let resp = app(public_state())
            .oneshot(put_body(
                r#"{"alert_linked_days":90,"unmatched_event_hours":24,"report_run_days":90,"flow_days":30}"#,
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    /// The bounds live in `retention`, and this is the contract the UI branches on: an out-of-range
    /// window is a typed 400, never a 500 from the table CHECK.
    #[test]
    fn out_of_range_windows_are_rejected_before_the_database_sees_them() {
        for bad in [
            RetentionSettings {
                alert_linked_days: 0,
                ..Default::default()
            },
            RetentionSettings {
                flow_days: retention::MAX_RETENTION_DAYS + 1,
                ..Default::default()
            },
            RetentionSettings {
                unmatched_event_hours: 0,
                ..Default::default()
            },
        ] {
            assert!(!bad.in_bounds());
        }
        assert!(RetentionSettings::default().in_bounds());
    }

    /// Every row the API emits must carry a subject/field/tunable token, or the UI cannot decide
    /// which control (if any) owns it.
    #[test]
    fn every_row_is_fully_described() {
        let settings = RetentionSettings::default();
        for subject in Subject::ALL {
            let row = subject.row();
            assert!(!subject.as_str().is_empty());
            assert!(!row.enforcement.as_str().is_empty());
            assert!(!row.tunable.as_str().is_empty());
            assert!(!row.field.as_str().is_empty());
            // An editable row must expose a number to edit; a read-only one must not pretend to.
            assert_eq!(
                row.tunable == Tunable::Settings,
                subject.configured(&settings).is_some()
            );
        }
    }
}
