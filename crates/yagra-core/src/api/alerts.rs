// SPDX-License-Identifier: AGPL-3.0-only
//! Alerts: the active list, history and its aggregations, inbound ack reflection, and the two live
//! streams.
//!
//! ## Ack is inbound-only
//!
//! Yagra holds no escalation scheduler and no ack *action* (ADR-015). An external incident tool
//! (PagerDuty / JSM) mirrors ack state in, keyed by the alert dedup identity
//! `(node, check, severity)`, and Yagra never writes it back out. So `acked` is decorative: if
//! loading it fails, the alert list is still served without it, because a blank alert list during
//! an incident is far worse than a missing pill.
//!
//! [`apply_ack`] exists because recording an ack has **two** halves — persist it, then broadcast so
//! the pill appears without a refetch — and both the REST endpoint and the MCP `ack_alert` tool did
//! them separately. Omitting the broadcast is a silent failure: the write succeeds, the caller sees
//! success, and every open dashboard keeps showing an unacknowledged alert until someone reloads.
//! Making them one call means you cannot do one without the other.

use super::extract::{RequireAckAlerts, RequireView, Scoped};
use super::util::Ranked;
use super::{ApiError, ApiResult, ApiState};
use crate::ack::{AckKey, AckView};
use crate::history::AlertHistoryRow;
use axum::{
    extract::{Query, State},
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Response,
    },
    routing::{get, post},
    Json, Router,
};
use futures::stream::StreamExt;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::convert::Infallible;
use uuid::Uuid;
use yagra_alert::{Alert, SubjectKind};
use yagra_common::{NodeState, Severity};

/// This domain's slice of the OpenAPI document (ADR-035), merged by [`super::openapi::document`].
#[derive(utoipa::OpenApi)]
#[openapi(paths(
    list_alerts,
    list_alert_history,
    ack_alert,
    alert_top_nodes,
    alert_calendar,
    alert_transitions,
    stream_alerts,
    stream_node_states
))]
pub(super) struct Doc;

/// The alert routes, merged into `/api/v1` by [`super::router`].
pub(crate) fn routes() -> Router<ApiState> {
    Router::new()
        .route("/api/v1/alerts", get(list_alerts))
        .route("/api/v1/alerts/history", get(list_alert_history))
        .route("/api/v1/alerts/ack", post(ack_alert))
        .route("/api/v1/alerts/top-nodes", get(alert_top_nodes))
        .route("/api/v1/alerts/calendar", get(alert_calendar))
        .route("/api/v1/alerts/transitions", get(alert_transitions))
        .route("/api/v1/stream/alerts", get(stream_alerts))
        .route("/api/v1/stream/node-states", get(stream_node_states))
}

// ── Ack decoration ───────────────────────────────────────────────────────────

/// An active alert plus its inbound (read-only) ack state.
///
/// `subject_kind` and `subject_name` decompose the alert's `node` field, which carries either a
/// node's UUID or `pool:<name>`. The live alert stream emits the same three keys.
//
// They are here rather than on `Alert` itself so the alert's own serialized form stays
// byte-identical for a node subject — the same reason `Subject` serializes flat.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub(crate) struct ActiveAlertView {
    #[serde(flatten)]
    pub alert: Alert,
    /// What this alert is about: a monitored node, or Yagra's own polling coverage for a pool.
    pub subject_kind: SubjectKind,
    /// The subject's name, for a subject identified by name rather than by id (a poller pool).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub acked: Option<AckView>,
}

/// An alert-history row plus its current inbound ack state (keyed by the dedup identity, so all
/// transitions of one incident share it).
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub(crate) struct AlertHistoryView {
    #[serde(flatten)]
    pub row: AlertHistoryRow,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub acked: Option<AckView>,
}

/// The ack map key for one alert identity.
///
/// The first component is the subject's **storage id** — a node's own id for a node alert, so
/// every acknowledgement recorded before the subject split still joins. Written once so the two
/// decorators and the ack writer cannot key on three different things.
fn ack_key(subject: &yagra_alert::Subject, check: Uuid, severity: Severity) -> AckKey {
    (subject.storage_id(), check, severity.as_str().to_owned())
}

/// Join the ack map into the active-alert list (pure; unit-tested without a DB).
fn decorate_alerts(alerts: Vec<Alert>, acks: &HashMap<AckKey, AckView>) -> Vec<ActiveAlertView> {
    alerts
        .into_iter()
        .map(|alert| {
            let acked = acks
                .get(&ack_key(
                    &alert.subject,
                    alert.check.as_uuid(),
                    alert.severity,
                ))
                .cloned();
            ActiveAlertView {
                subject_kind: alert.subject.kind(),
                subject_name: alert.subject.name().map(str::to_owned),
                alert,
                acked,
            }
        })
        .collect()
}

/// Join the ack map into a history page (pure; unit-tested without a DB).
fn decorate_history(
    rows: Vec<AlertHistoryRow>,
    acks: &HashMap<AckKey, AckView>,
) -> Vec<AlertHistoryView> {
    rows.into_iter()
        .map(|row| {
            let acked = row
                .subject()
                .and_then(|s| acks.get(&ack_key(&s, row.check, row.severity)).cloned());
            AlertHistoryView { row, acked }
        })
        .collect()
}

/// Load the current ack map, or an empty map when ack is unavailable (skeleton) or the query
/// errors — ack state is decorative, so a failure here must not blank the alert list.
async fn ack_map(st: &ApiState) -> HashMap<AckKey, AckView> {
    match st.ack.as_ref() {
        Some(repo) => repo.all().await.unwrap_or_else(|e| {
            tracing::error!(error = %e, "load ack map failed; serving alerts without ack state");
            HashMap::new()
        }),
        None => HashMap::new(),
    }
}

/// Record or clear an inbound ack, **and** broadcast the change.
///
/// `view` is `Some` to acknowledge, `None` to clear. The two steps are one function on purpose: the
/// persist is what survives a restart and the broadcast is what makes the pill appear without a
/// refetch, and a caller that does only the first gets a success response while every open
/// dashboard keeps showing the alert unacknowledged.
pub(crate) async fn apply_ack(
    st: &ApiState,
    subject: &yagra_alert::Subject,
    check: Uuid,
    severity: Severity,
    view: Option<AckView>,
) -> Result<(), ApiError> {
    let Some(repo) = st.ack.as_ref() else {
        return Err(ApiError::admin_unavailable());
    };
    match &view {
        Some(v) => repo
            .set(subject, check, severity.as_str(), v)
            .await
            .map_err(|e| {
                ApiError::from_internal(
                    e.as_ref(),
                    "record inbound ack",
                    "failed to record acknowledgement",
                )
            })?,
        None => repo
            .clear(subject, check, severity.as_str())
            .await
            .map_err(|e| {
                ApiError::from_internal(
                    e.as_ref(),
                    "clear inbound ack",
                    "failed to clear acknowledgement",
                )
            })?,
    }
    // The event carries the alert shape plus `acked` and no `resolved`, so a client treats it as
    // an upsert rather than a removal.
    st.alerts.broadcast_acked(
        subject,
        check,
        severity,
        view.as_ref().and_then(|v| serde_json::to_value(v).ok()),
    );
    Ok(())
}

// ── Reads ────────────────────────────────────────────────────────────────────

#[utoipa::path(
    get, path = "/api/v1/alerts", tag = "alerts",
    responses(
        (status = 200, description = "Every active alert, each decorated with its inbound ack state", body = Vec<ActiveAlertView>),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks the read permission", body = super::error::ErrorBody),
    ),
)]
async fn list_alerts(
    _perm: RequireView,
    Scoped(scope): Scoped,
    State(st): State<ApiState>,
) -> Json<Vec<ActiveAlertView>> {
    let acks = ack_map(&st).await;
    // Filter before decorating: an out-of-scope alert must not reach the ack join either, or the
    // response would be shorter but the work — and the ack lookups — would still name those nodes.
    let alerts = st
        .alerts
        .active_alerts()
        .into_iter()
        .filter(|a| scope.allows_subject(&st, &a.subject))
        .collect();
    Json(decorate_alerts(alerts, &acks))
}

/// A page of alert history: `?limit=`, the keyset cursor, and the filters the History toolbar
/// offers.
///
/// `before`/`before_id` page; `since`/`until` bound the window being searched. They are separate
/// parameters on purpose — one is where this page starts, the other is what the operator asked to
/// look at — so a filtered scroll keeps its window while the cursor advances.
#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub(crate) struct HistoryQuery {
    /// Max rows (1–1000, default 100).
    pub limit: Option<i64>,
    /// Keyset cursor, first half: the last row's `recorded_at`, as an RFC 3339 timestamp.
    pub before: Option<String>,
    /// Keyset cursor, second half: **the same row's `id`**. Send both. A whole flush of alerts is
    /// written in one transaction and therefore shares one `recorded_at`, so a timestamp-only
    /// cursor lands inside that group and skips its remaining rows. Omitting it is still accepted
    /// and means "strictly before that instant", which is what an older client sends.
    pub before_id: Option<Uuid>,
    /// Only transitions recorded at or after this RFC 3339 timestamp.
    ///
    /// Bounds `recorded_at` (when the row was written), not `at_unix_ms` (when the alert fired), so
    /// one index serves the ordering, the cursor and the range. The two differ by the ingest
    /// writer's flush latency — under a second — so a row may sit marginally outside the window its
    /// displayed time suggests.
    pub since: Option<String>,
    /// Only transitions recorded at or before this RFC 3339 timestamp. Bounds `recorded_at`; see
    /// `since`.
    pub until: Option<String>,
    /// Only transitions at this severity.
    pub severity: Option<Severity>,
    /// Only transitions into this state.
    pub state: Option<NodeState>,
    /// `false` for fires only, `true` for clears only. Omit for both.
    pub resolved: Option<bool>,
    /// Only transitions about this node. Rows about something other than a node (a poller pool)
    /// are excluded by construction.
    pub node_id: Option<Uuid>,
    /// Only transitions about nodes in this folder group **or any group beneath it**.
    pub group_id: Option<Uuid>,
}

#[utoipa::path(
    get, path = "/api/v1/alerts/history", tag = "alerts",
    params(HistoryQuery),
    responses(
        (status = 200, description = "A page of history rows, newest first; empty when this deployment keeps no history", body = Vec<AlertHistoryView>),
        (status = 400, description = "A cursor or range bound is not RFC 3339, or `severity`/`state` is not one of the listed values", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks the read permission", body = super::error::ErrorBody),
        (status = 404, description = "The requested `node_id` or `group_id` is outside the caller's scope", body = super::error::ErrorBody),
    ),
)]
async fn list_alert_history(
    _perm: RequireView,
    Scoped(scope): Scoped,
    State(st): State<ApiState>,
    Query(q): Query<HistoryQuery>,
) -> ApiResult<Json<Vec<AlertHistoryView>>> {
    let rows = history_page(
        &st,
        &scope,
        HistoryFilterInput {
            limit: q.limit,
            before: q.before.as_deref(),
            before_id: q.before_id,
            since: q.since.as_deref(),
            until: q.until.as_deref(),
            severity: q.severity,
            state: q.state,
            resolved: q.resolved,
            node_id: q.node_id,
            group_id: q.group_id,
        },
    )
    .await?;
    let acks = ack_map(&st).await;
    Ok(Json(decorate_history(rows, &acks)))
}

/// The raw filter fields, as either surface receives them.
pub(crate) struct HistoryFilterInput<'a> {
    pub limit: Option<i64>,
    pub before: Option<&'a str>,
    pub before_id: Option<Uuid>,
    pub since: Option<&'a str>,
    pub until: Option<&'a str>,
    pub severity: Option<Severity>,
    pub state: Option<NodeState>,
    pub resolved: Option<bool>,
    pub node_id: Option<Uuid>,
    pub group_id: Option<Uuid>,
}

/// Parse a severity or state token from a surface that receives it as text (MCP).
///
/// Here rather than in `mcp/tools.rs` so both surfaces accept exactly the same vocabulary. REST
/// takes the enum directly — serde does this parse — and this is the one extra step MCP needs
/// because its parameter schemas are `schemars`, which `yagra-common` deliberately does not derive.
pub(crate) fn parse_severity(s: Option<&str>) -> Result<Option<Severity>, ApiError> {
    s.map(|v| {
        Severity::from_token(v).ok_or_else(|| {
            ApiError::bad_request("invalid_severity", "severity must be info|warning|critical")
        })
    })
    .transpose()
}

/// See [`parse_severity`].
pub(crate) fn parse_state(s: Option<&str>) -> Result<Option<NodeState>, ApiError> {
    s.map(|v| {
        NodeState::from_token(v).ok_or_else(|| {
            ApiError::bad_request("invalid_state", "state is not a known node state")
        })
    })
    .transpose()
}

/// A page of alert history, newest first — shared by `GET /api/v1/alerts/history` and the MCP
/// `get_alert_history` tool.
///
/// **The seam is the whole page function, not just a validator.** `parse_event_filter` shares only
/// the validation with its MCP twin, and the halves around it drifted anyway — the term cap existed
/// on the REST side and not on the surface with no human in the loop. With parsing, the scope
/// checks, the store call and the post-filter all inside, there is nothing left to re-derive.
pub(crate) async fn history_page(
    st: &ApiState,
    scope: &super::scope::NodeScope,
    input: HistoryFilterInput<'_>,
) -> Result<Vec<AlertHistoryRow>, ApiError> {
    // The cursor and the range bounds get different codes: a bad cursor is a client paging bug, a
    // bad bound is operator input, and the UI surfaces them differently.
    fn ts(
        value: Option<&str>,
        field: &str,
        code: &'static str,
    ) -> Result<Option<chrono::DateTime<chrono::Utc>>, ApiError> {
        match value {
            None => Ok(None),
            Some(s) => super::parse_rfc3339(s).map(Some).ok_or_else(|| {
                ApiError::bad_request(code, format!("{field} must be an RFC 3339 timestamp"))
            }),
        }
    }
    // Parse before checking for a store: a malformed cursor is the client's bug whether or not this
    // deployment has history, and answering `200 []` to it would let a paging bug look like "you
    // have reached the end".
    let before = ts(input.before, "before", "invalid_cursor")?;
    let since = ts(input.since, "since", "invalid_filter")?;
    let until = ts(input.until, "until", "invalid_filter")?;

    // Filtering *by* a node or a group is still naming one, so both go through the same visibility
    // check the rest of the surface uses — the shape `search_saved_findings` established. Answering
    // `200 []` instead would be a slower way of saying the same thing for a group that exists, and a
    // way of confirming one that does not.
    if let Some(n) = input.node_id {
        super::scope::require_visible_node(st, scope, yagra_common::NodeId::from(n))?;
    }
    let in_group = match input.group_id {
        None => None,
        Some(g) => {
            super::scope::require_visible_group(scope, g)?;
            Some(super::scope::subtree_of(st, g).await?)
        }
    };

    // Skeleton mode has no history store. An empty page rather than a 503: the alerts page renders
    // with an empty history panel instead of an error, matching the runs list in `api/analysis.rs`.
    let Some(history) = st.history.as_ref() else {
        return Ok(Vec::new());
    };
    let filter = crate::history::HistoryFilter {
        before,
        before_id: input.before_id,
        since,
        until,
        severity: input.severity,
        state: input.state,
        resolved: input.resolved,
        node_id: input.node_id,
        in_group: in_group.as_deref(),
        limit: input.limit.unwrap_or(100),
    };
    let rows = history.search(&filter).await.map_err(|e| {
        ApiError::from_internal(
            e.as_ref(),
            "list alert history",
            "failed to list alert history",
        )
    })?;
    // The caller's **scope** stays post-filtered rather than pushed into the query, and that is a
    // decision rather than an omission: `AlertHistoryStore::SCOPE_PREDICATE` fails closed for a
    // non-node subject, while `allows_subject` resolves pools properly — so pushing it in would hide
    // pool-coverage alerts from exactly the scoped operator whose site had gone dark. A page may
    // therefore come back short, which is correct: the cursor is the last row returned, so paging
    // still terminates. The *requested* `group_id` above is a different thing and does go into SQL.
    //
    // A row whose subject could not be read at all (a newer core's `subject_kind`) is dropped for
    // everyone: `search` already drops those, so `subject()` is `Some` here — but spelling the
    // `None` case as "not visible" keeps the fail-closed direction if that ever changes.
    Ok(rows
        .into_iter()
        .filter(|r| r.subject().is_some_and(|s| scope.allows_subject(st, &s)))
        .collect())
}

// ── Inbound ack (ADR-015, A1) ────────────────────────────────────────────────

/// Default for `AckRequest::acked` when the field is omitted (a bare body means "ack").
fn default_acked() -> bool {
    true
}

/// Inbound ack reflection. An external incident tool mirrors ack state in by the dedup identity
/// `(subject, check, severity)`; `acked:false` clears it. `AckAlerts`-gated; the mutating-request
/// middleware records the audit entry.
#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct AckRequest {
    /// The node the alert is about. Omit it and send `subject` instead for an alert about
    /// something other than a node (a poller pool). Exactly one of the two is required.
    #[serde(default)]
    node: Option<Uuid>,
    /// The alert's subject in its flat form — a node's UUID, or `pool:<name>`. This is the value
    /// the alert's own `node` field carries, so an integration can echo back what it received.
    #[serde(default)]
    subject: Option<String>,
    check: Uuid,
    severity: Severity,
    /// `true` = acked (upsert), `false` = cleared (delete). Defaults to `true`.
    #[serde(default = "default_acked")]
    acked: bool,
    /// External actor reference (id / handle) — never a secret.
    #[serde(default)]
    by: Option<String>,
    /// Originating tool: `pagerduty` | `jsm` | `manual` | …
    #[serde(default)]
    source: Option<String>,
    /// Optional free-text note from the external tool.
    #[serde(default)]
    note: Option<String>,
    /// When the external tool recorded the ack (Unix ms, UTC); defaults to now.
    #[serde(default)]
    at_unix_ms: Option<i64>,
}

/// What an ack call reports back: the new state, and the stored view when acknowledging.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub(super) struct AckResult {
    acked: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    ack: Option<AckView>,
}

#[utoipa::path(
    post, path = "/api/v1/alerts/ack", tag = "alerts",
    request_body = AckRequest,
    responses(
        (status = 200, description = "The ack was recorded (or cleared) and broadcast", body = AckResult),
        (status = 400, description = "Neither `node` nor a readable `subject` was supplied", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks the alert-acknowledgement permission", body = super::error::ErrorBody),
        (status = 404, description = "The subject is outside the caller's group scope", body = super::error::ErrorBody),
        (status = 503, description = "This deployment has no ack store", body = super::error::ErrorBody),
    ),
)]
async fn ack_alert(
    _perm: RequireAckAlerts,
    Scoped(scope): Scoped,
    State(st): State<ApiState>,
    Json(body): Json<AckRequest>,
) -> ApiResult<Json<AckResult>> {
    let subject = ack_subject(body.node, body.subject.as_deref())?;
    super::scope::require_visible_subject(&st, &scope, &subject)?;
    let view = body.acked.then(|| AckView {
        // The external tool's own timestamp wins when it sends one — its clock is the record of
        // when the human actually acknowledged, which may be well before this request arrives.
        at_unix_ms: body
            .at_unix_ms
            .unwrap_or_else(|| super::now_unix_s() * 1000),
        by: body.by.unwrap_or_else(|| "external".to_owned()),
        source: body.source.unwrap_or_else(|| "external".to_owned()),
        note: body.note,
    });
    apply_ack(&st, &subject, body.check, body.severity, view.clone()).await?;
    Ok(Json(AckResult {
        acked: body.acked,
        ack: view,
    }))
}

/// Resolve an ack request's target from its two spellings.
///
/// `node` is the original field and stays the common case; `subject` is what an integration echoes
/// back for an alert about something else. Pure, so the precedence and the refusal are unit-tested
/// without a router.
fn ack_subject(
    node: Option<Uuid>,
    subject: Option<&str>,
) -> Result<yagra_alert::Subject, ApiError> {
    // `node` wins when both are present: it is the narrower statement, and a caller that sends a
    // node id cannot have meant a pool.
    if let Some(node) = node {
        return Ok(yagra_alert::Subject::Node(yagra_common::NodeId::from(node)));
    }
    subject.and_then(|s| s.parse().ok()).ok_or_else(|| {
        ApiError::bad_request(
            "invalid_subject",
            "send `node`, or a `subject` of `<uuid>` or `pool:<name>`",
        )
    })
}

// ── History aggregations ─────────────────────────────────────────────────────

/// Query for the alert top-nodes aggregation: `?window=<secs>` (default 24h) + `?limit=`.
#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub(super) struct AlertTopQuery {
    window: Option<i64>,
    limit: Option<i64>,
}

/// One chronic-offender row.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub(crate) struct AlertNodeCount {
    pub node_id: Uuid,
    pub name: String,
    pub count: i64,
}

/// Nodes generating the most alert fires over a trailing window (chronic offenders).
#[utoipa::path(
    get, path = "/api/v1/alerts/top-nodes", tag = "alerts",
    params(AlertTopQuery),
    responses(
        (status = 200, description = "Nodes ranked by alert fires; empty when this deployment keeps no history, and `partial` says the scope filter may have shortened the list", body = super::util::Ranked<AlertNodeCount>),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks the read permission", body = super::error::ErrorBody),
    ),
)]
async fn alert_top_nodes(
    _perm: RequireView,
    Scoped(scope): Scoped,
    State(st): State<ApiState>,
    Query(q): Query<AlertTopQuery>,
) -> ApiResult<Json<Ranked<AlertNodeCount>>> {
    Ok(Json(
        top_alerting_nodes(&st, &scope, q.window, q.limit).await?,
    ))
}

/// Nodes ranked by alert fires over a trailing window, filtered to what `scope` may see.
///
/// The two clamps (`window` 60s–30d, `limit` 1–50) and the over-fetch live here rather than at the
/// edge, so a second surface cannot ask for a ranking a thousand rows deep — the flow-limit bug
/// ADR-042 I1 fixed was exactly one clamp that existed on one edge only.
pub(crate) async fn top_alerting_nodes(
    st: &ApiState,
    scope: &super::scope::NodeScope,
    window: Option<i64>,
    limit: Option<i64>,
) -> Result<Ranked<AlertNodeCount>, ApiError> {
    let Some(history) = st.history.as_ref() else {
        // No history store is "there is nothing to rank", not "the ranking was cut short".
        return Ok(Ranked {
            entries: Vec::new(),
            partial: false,
        });
    };
    let window = window.unwrap_or(86_400).clamp(60, 30 * 86_400);
    let since_ms = (super::now_unix_s() - window) * 1000;
    let limit = limit.unwrap_or(6).clamp(1, 50) as usize;
    // Ranked by fire count in SQL, filtered by scope afterwards — same shape and same trade-off as
    // the TSDB rankings in `api/metrics.rs`. Over-fetch so a scoped list is usually still full.
    let fetched = super::scope::ranking_fetch_limit(scope, limit);
    let counts = history
        .top_nodes_by_fires(since_ms, fetched as i64)
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "alert top-nodes",
                "failed to aggregate alerting nodes",
            )
        })?;
    let counts: Vec<_> = counts
        .into_iter()
        .filter(|(id, _)| scope.allows_node(st, yagra_common::NodeId::from(*id)))
        .collect();
    let counts = Ranked::new(counts, limit, fetched);
    let names =
        super::nodes::resolve_node_names(st, scope, counts.entries.iter().map(|(n, _)| *n)).await;
    Ok(Ranked {
        entries: counts
            .entries
            .into_iter()
            .map(|(node_id, count)| AlertNodeCount {
                node_id,
                name: names
                    .get(&node_id)
                    .cloned()
                    .unwrap_or_else(|| node_id.to_string()),
                count,
            })
            .collect(),
        partial: counts.partial,
    })
}

/// Query for the alert calendar heatmap: `?days=<n>` (default 7) of history.
#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub(super) struct AlertCalendarQuery {
    days: Option<i64>,
}

/// One weekday×hour heatmap cell.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub(crate) struct CalendarBucket {
    /// 0 = Sunday … 6 = Saturday (UTC).
    pub dow: i32,
    /// Hour of day 0–23 (UTC).
    pub hour: i32,
    pub count: i64,
}

/// Alert fires bucketed weekday×hour over the last `days` (for the calendar heatmap).
#[utoipa::path(
    get, path = "/api/v1/alerts/calendar", tag = "alerts",
    params(AlertCalendarQuery),
    responses(
        (status = 200, description = "Weekday×hour fire counts; empty when this deployment keeps no history", body = Vec<CalendarBucket>),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks the read permission", body = super::error::ErrorBody),
    ),
)]
async fn alert_calendar(
    _perm: RequireView,
    Scoped(scope): Scoped,
    State(st): State<ApiState>,
    Query(q): Query<AlertCalendarQuery>,
) -> ApiResult<Json<Vec<CalendarBucket>>> {
    Ok(Json(alert_calendar_buckets(&st, &scope, q.days).await?))
}

/// Alert fires bucketed weekday×hour over the last `days` (1–90, default 7).
pub(crate) async fn alert_calendar_buckets(
    st: &ApiState,
    scope: &super::scope::NodeScope,
    days: Option<i64>,
) -> Result<Vec<CalendarBucket>, ApiError> {
    let Some(history) = st.history.as_ref() else {
        return Ok(Vec::new());
    };
    let days = days.unwrap_or(7).clamp(1, 90);
    let since_ms = (super::now_unix_s() - days * 86_400) * 1000;
    // The counts come out of a `GROUP BY`, so there are no per-node rows left to drop afterwards —
    // the scope has to go into the query. This is the one of the four aggregate endpoints whose
    // source table still carries a node id, which is why it can be filtered at all.
    let buckets = history
        .fires_by_weekday_hour(since_ms, scope.group_filter())
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "alert calendar",
                "failed to build the alert calendar",
            )
        })?;
    Ok(buckets
        .into_iter()
        .map(|(dow, hour, count)| CalendarBucket { dow, hour, count })
        .collect())
}

/// One recent state-change row (a fire = into an alert state; a resolve = recovery to ok).
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub(crate) struct AlertTransition {
    pub node_id: Uuid,
    pub name: String,
    pub state: NodeState,
    pub severity: Severity,
    /// true = recovery (→ ok); false = went into the alert state.
    pub resolved: bool,
    pub at_unix_ms: i64,
}

/// Recent up/down transitions (latest fires and resolutions), node names joined.
///
/// Only `limit` is documented, though the handler borrows the history query type: this endpoint
/// reads no cursor, and advertising one would promise paging it does not do.
#[utoipa::path(
    get, path = "/api/v1/alerts/transitions", tag = "alerts",
    params(("limit" = Option<i64>, Query, description = "How many transitions to return (default 12)")),
    responses(
        (status = 200, description = "The latest fires and resolutions; empty when this deployment keeps no history", body = Vec<AlertTransition>),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks the read permission", body = super::error::ErrorBody),
    ),
)]
async fn alert_transitions(
    _perm: RequireView,
    Scoped(scope): Scoped,
    State(st): State<ApiState>,
    Query(q): Query<HistoryQuery>,
) -> ApiResult<Json<Vec<AlertTransition>>> {
    Ok(Json(recent_transitions(&st, &scope, q.limit).await?))
}

/// The latest fires and resolutions the caller may see, node names joined.
///
/// The rows carry a node id, so the scope filter runs after the query; the names come from the
/// shared batch resolver rather than a second inventory read.
///
/// **Node transitions only.** Every row here is rendered as a node with a resolved name, and a
/// pool-coverage alert has no node to name — its rows would come back with an unresolvable id.
/// Those transitions are read on the History screen and the Alerts list instead, both of which
/// carry the subject.
///
/// No clamp here on purpose — `AlertHistory::recent` bounds the limit to 1..=1000 in the store, so
/// adding a second one would tighten REST's behaviour rather than protect anything.
pub(crate) async fn recent_transitions(
    st: &ApiState,
    scope: &super::scope::NodeScope,
    limit: Option<i64>,
) -> Result<Vec<AlertTransition>, ApiError> {
    let Some(history) = st.history.as_ref() else {
        return Ok(Vec::new());
    };
    let rows = history
        .recent(limit.unwrap_or(12), None, None)
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "alert transitions",
                "failed to list alert transitions",
            )
        })?;
    let rows: Vec<_> = rows
        .into_iter()
        .filter_map(|r| Some((r.node?, r)))
        .filter(|(node, _)| scope.allows_node(st, yagra_common::NodeId::from(*node)))
        .collect();
    let names = super::nodes::resolve_node_names(st, scope, rows.iter().map(|(n, _)| *n)).await;
    Ok(rows
        .into_iter()
        .map(|(node_id, r)| AlertTransition {
            node_id,
            name: names
                .get(&node_id)
                .cloned()
                .unwrap_or_else(|| node_id.to_string()),
            state: r.state,
            severity: r.severity,
            resolved: r.resolved,
            at_unix_ms: r.at_unix_ms,
        })
        .collect())
}

// ── Live streams ─────────────────────────────────────────────────────────────

/// Turn a broadcast receiver into an SSE stream for one subscriber: drop the frames their scope
/// does not cover, and emit a named `resync` event when they fall behind.
///
/// The resync hint is the whole point of the second half: a slow client that silently missed `n`
/// events would show a stale alert list forever. Browsers ignore named events they do not listen
/// for, so a client that only reads the default message stream is unaffected.
///
/// **The scope filter is per subscriber, and it must be**, which is why it lives here rather than at
/// the sender. One broadcast fans out to every open dashboard, and those callers do not share a
/// scope — the sender has no single right answer. Filtering here costs a set membership per frame
/// per subscriber and leaves the unrestricted case (`NodeScope::All`) taking the `is_all` short
/// circuit, so the common deployment pays nothing.
///
/// A dropped frame is **not** a lag: `resync` means "you missed something, re-fetch", and firing it
/// for an out-of-scope alert would send a scoped client back to REST — which correctly returns a
/// list without that alert — on every event in the rest of the fleet.
///
/// ⚠️ **The captured handle is `Weak` on purpose.** Deciding whether a node is in scope needs the
/// alert engine's fleet-wide node metadata — but the alert engine is also what owns the broadcast
/// sender feeding this stream. A strong `Arc` here would keep that sender alive for as long as the
/// stream lives and the stream alive for as long as the sender lives, so the body would never end.
/// If the upgrade ever fails the whole state is gone, and the frame is dropped: fail-closed.
fn sse_with_resync(
    rx: tokio::sync::broadcast::Receiver<crate::alerts::StreamFrame>,
    alerts: std::sync::Weak<crate::alerts::AlertManager>,
    scope: super::scope::NodeScope,
    what: &'static str,
) -> impl futures::Stream<Item = Result<Event, Infallible>> {
    tokio_stream::wrappers::BroadcastStream::new(rx).filter_map(move |r| {
        // Cloned per frame so the closure stays `FnMut` rather than consuming its captures; both
        // are cheap (a `Weak` bump and an `Arc<ScopeSet>` clone).
        let (alerts, scope) = (alerts.clone(), scope.clone());
        async move {
            match r {
                Ok((subject, json)) => {
                    let visible = scope.is_all()
                        || alerts
                            .upgrade()
                            .is_some_and(|a| scope.allows_subject_in(&a, &subject));
                    visible.then(|| Ok::<_, Infallible>(Event::default().data(&*json)))
                }
                Err(tokio_stream::wrappers::errors::BroadcastStreamRecvError::Lagged(n)) => {
                    tracing::warn!(
                        missed = n,
                        "SSE {what} subscriber lagged; emitting resync hint"
                    );
                    Some(Ok(Event::default().event("resync").data(n.to_string())))
                }
            }
        }
    })
}

/// Live alert stream (SSE, ADR-019): fires and resolutions as they happen. Each event's `data` is
/// the alert JSON with a `resolved` flag.
#[utoipa::path(
    get, path = "/api/v1/stream/alerts", tag = "alerts",
    responses(
        (status = 200, description = "Server-sent event stream of fires and resolutions; a lagged subscriber gets a named `resync` event", content_type = "text/event-stream"),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks the read permission", body = super::error::ErrorBody),
    ),
)]
async fn stream_alerts(
    _perm: RequireView,
    Scoped(scope): Scoped,
    State(st): State<ApiState>,
) -> Response {
    let rx = st.alerts.subscribe();
    let alerts = std::sync::Arc::downgrade(&st.alerts);
    Sse::new(sse_with_resync(rx, alerts, scope, "alert"))
        .keep_alive(KeepAlive::default())
        .into_response()
}

/// Live node-state stream (SSE, S14): rolled-up display-state changes as they happen. The
/// inventory and topology views seed from REST once, then patch individual nodes off this stream
/// instead of re-fetching the whole fleet every 15s. Works in skeleton mode — the alert engine is
/// always present.
#[utoipa::path(
    get, path = "/api/v1/stream/node-states", tag = "alerts",
    responses(
        (status = 200, description = "Server-sent event stream of rolled-up node display-state changes", content_type = "text/event-stream"),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks the read permission", body = super::error::ErrorBody),
    ),
)]
async fn stream_node_states(
    _perm: RequireView,
    Scoped(scope): Scoped,
    State(st): State<ApiState>,
) -> Response {
    let rx = st.alerts.subscribe_node_states();
    let alerts = std::sync::Arc::downgrade(&st.alerts);
    Sse::new(sse_with_resync(rx, alerts, scope, "node-state"))
        .keep_alive(KeepAlive::default())
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::router;
    use crate::api::tests_support::public_state;
    use axum::body::{to_bytes, Body};
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;
    use yagra_common::{NodeId, NodeState};

    // ── Inbound ack reflection (ADR-015, A1) ────────────────────────────────

    #[test]
    fn decorate_alerts_attaches_inbound_ack_by_dedup_key() {
        use std::collections::HashMap;
        let node = NodeId::from(Uuid::from_u128(1));
        let check = yagra_common::CheckId::from(Uuid::from_u128(2));
        let acked_alert = Alert {
            subject: yagra_alert::Subject::Node(node),
            check,
            severity: Severity::Critical,
            state: NodeState::Critical,
            at_unix_ms: 100,
            root_cause: None,
            flapping: false,
            metric: "__liveness__".to_string(),
            breach: None,
        };
        let other = Alert {
            subject: yagra_alert::Subject::Node(NodeId::from(Uuid::from_u128(9))),
            check: yagra_common::CheckId::from(Uuid::from_u128(8)),
            severity: Severity::Warning,
            state: NodeState::Warning,
            at_unix_ms: 50,
            root_cause: None,
            flapping: false,
            metric: "__liveness__".to_string(),
            breach: None,
        };
        let mut acks: HashMap<AckKey, AckView> = HashMap::new();
        acks.insert(
            (node.as_uuid(), check.as_uuid(), "critical".to_owned()),
            AckView {
                at_unix_ms: 123,
                by: "pd-user".to_owned(),
                source: "pagerduty".to_owned(),
                note: Some("acked".to_owned()),
            },
        );

        let out = decorate_alerts(vec![acked_alert, other], &acks);
        assert_eq!(
            out[0].acked.as_ref().map(|a| a.source.as_str()),
            Some("pagerduty")
        );
        assert!(out[1].acked.is_none(), "unrelated alert must not be acked");

        // Serialized shape flattens the alert fields and includes the ack view.
        let json = serde_json::to_value(&out[0]).unwrap();
        assert_eq!(json["severity"], "critical");
        assert_eq!(json["acked"]["by"], "pd-user");
        // A non-acked alert omits the field entirely (skip_serializing_if).
        let json2 = serde_json::to_value(&out[1]).unwrap();
        assert!(json2.get("acked").is_none());
    }

    #[test]
    fn a_pool_alert_carries_its_subject_and_still_serializes_node_as_a_string() {
        // Two properties in one shape, because they fail together. `web/src/services/sse.ts` and
        // the list reader both gate on `node` being a string, so it must stay one for a pool;
        // `subject_kind`/`subject_name` are what stop the UI resolving that string through the
        // inventory and showing an operator an unresolvable id instead of their pool.
        let alert = Alert {
            subject: yagra_alert::Subject::Pool("tokyo".to_owned()),
            check: yagra_common::CheckId::from(Uuid::from_u128(3)),
            severity: Severity::Critical,
            state: NodeState::Unreachable,
            at_unix_ms: 1,
            root_cause: None,
            flapping: false,
            metric: "live_pollers".to_string(),
            breach: None,
        };
        let out = decorate_alerts(vec![alert], &HashMap::new());
        let json = serde_json::to_value(&out[0]).unwrap();
        assert_eq!(json["node"], "pool:tokyo");
        assert!(json["node"].is_string());
        assert_eq!(json["subject_kind"], "pool");
        assert_eq!(json["subject_name"], "tokyo");
    }

    #[test]
    fn a_pool_alerts_ack_does_not_land_on_the_node_whose_name_the_pool_carries() {
        // A pool name is operator-authored free text, so it can be spelled as a node's UUID. The
        // ack join keys on the subject's storage id, which is derived for a pool — if it keyed on
        // the raw name, that pool's alert would wear that node's acknowledgement.
        let node = Uuid::from_u128(42);
        let check = Uuid::from_u128(2);
        let mut acks: HashMap<AckKey, AckView> = HashMap::new();
        acks.insert(
            (node, check, "critical".to_owned()),
            AckView {
                at_unix_ms: 1,
                by: "x".to_owned(),
                source: "pagerduty".to_owned(),
                note: None,
            },
        );
        let impostor = Alert {
            subject: yagra_alert::Subject::Pool(node.to_string()),
            check: yagra_common::CheckId::from(check),
            severity: Severity::Critical,
            state: NodeState::Unreachable,
            at_unix_ms: 1,
            root_cause: None,
            flapping: false,
            metric: "live_pollers".to_string(),
            breach: None,
        };
        assert!(decorate_alerts(vec![impostor], &acks)[0].acked.is_none());
    }

    #[test]
    fn an_ack_request_names_its_subject_by_node_or_by_flat_form() {
        let node = Uuid::from_u128(5);
        assert_eq!(
            ack_subject(Some(node), None).unwrap(),
            yagra_alert::Subject::Node(NodeId::from(node))
        );
        assert_eq!(
            ack_subject(None, Some("pool:tokyo")).unwrap(),
            yagra_alert::Subject::Pool("tokyo".to_owned())
        );
        // A bare UUID in `subject` is the same node — an integration echoing back the alert's own
        // `node` field must not need to know which spelling to use.
        assert_eq!(
            ack_subject(None, Some(&node.to_string())).unwrap(),
            yagra_alert::Subject::Node(NodeId::from(node))
        );
        // `node` wins when both are sent: it is the narrower statement.
        assert_eq!(
            ack_subject(Some(node), Some("pool:tokyo")).unwrap(),
            yagra_alert::Subject::Node(NodeId::from(node))
        );
        // Neither, or an unreadable subject, is the caller's bug and gets a typed 400 rather than
        // a silent ack on some default target.
        for bad in [None, Some(""), Some("pool:"), Some("nonsense")] {
            let err = ack_subject(None, bad).expect_err("no target");
            assert_eq!(err.code(), "invalid_subject", "{bad:?}");
        }
    }

    #[test]
    fn decorate_history_shares_ack_across_an_incidents_transitions() {
        use std::collections::HashMap;
        let node = Uuid::from_u128(1);
        let check = Uuid::from_u128(2);
        let fire = AlertHistoryRow {
            id: Uuid::new_v4(),
            node: Some(node),
            subject_kind: SubjectKind::Node,
            subject_name: None,
            check,
            severity: Severity::Critical,
            state: NodeState::Critical,
            at_unix_ms: 10,
            resolved: false,
            metric: Some("icmp_rtt_ms".to_owned()),
            observed_value: Some(150.0),
            threshold_value: Some(100.0),
            direction: Some(yagra_common::Direction::Above),
            recorded_at: "1970-01-01T00:00:10Z".to_owned(),
        };
        let clear = AlertHistoryRow {
            id: Uuid::new_v4(),
            node: Some(node),
            subject_kind: SubjectKind::Node,
            subject_name: None,
            check,
            severity: Severity::Critical,
            state: NodeState::Ok,
            at_unix_ms: 20,
            resolved: true,
            metric: Some("icmp_rtt_ms".to_owned()),
            observed_value: None,
            threshold_value: None,
            direction: None,
            recorded_at: "1970-01-01T00:00:20Z".to_owned(),
        };
        let unrelated = AlertHistoryRow {
            id: Uuid::new_v4(),
            node: Some(Uuid::from_u128(7)),
            subject_kind: SubjectKind::Node,
            subject_name: None,
            check,
            severity: Severity::Warning,
            state: NodeState::Warning,
            at_unix_ms: 5,
            resolved: false,
            metric: None,
            observed_value: None,
            threshold_value: None,
            direction: None,
            recorded_at: "1970-01-01T00:00:05Z".to_owned(),
        };
        let mut acks: HashMap<AckKey, AckView> = HashMap::new();
        acks.insert(
            (node, check, "critical".to_owned()),
            AckView {
                at_unix_ms: 11,
                by: "x".to_owned(),
                source: "jsm".to_owned(),
                note: None,
            },
        );

        let out = decorate_history(vec![fire, clear, unrelated], &acks);
        assert!(out[0].acked.is_some(), "fire of acked incident is acked");
        assert!(
            out[1].acked.is_some(),
            "clear of same incident shares the ack"
        );
        assert!(out[2].acked.is_none(), "unrelated transition is not acked");
    }
    #[tokio::test]
    async fn the_alert_list_survives_an_unavailable_ack_store() {
        // Ack state is decorative (ADR-015: Yagra holds no ack action). Skeleton mode has no ack
        // repo, and the list must still serve — a blank alert list during an incident would be far
        // worse than a missing pill.
        let resp = router(public_state())
            .oneshot(
                Request::builder()
                    .uri("/api/v1/alerts")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        assert_eq!(&bytes[..], b"[]");
    }

    #[tokio::test]
    async fn history_and_its_aggregations_are_empty_not_broken_without_a_store() {
        // `top-nodes` answers the `Ranked` envelope rather than a bare array; the others are still
        // plain lists. Both must be *empty*, not an error — an alerts page whose history panel 500s
        // is worse than one with an empty panel.
        for (path, want) in [
            ("/api/v1/alerts/history", "[]"),
            (
                "/api/v1/alerts/top-nodes",
                r#"{"entries":[],"partial":false}"#,
            ),
            ("/api/v1/alerts/calendar", "[]"),
            ("/api/v1/alerts/transitions", "[]"),
        ] {
            let resp = router(public_state())
                .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::OK, "{path}");
            let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
            assert_eq!(
                String::from_utf8_lossy(&bytes),
                want,
                "{path}: an unrestricted caller must never be told the ranking was cut short"
            );
        }
    }

    #[tokio::test]
    async fn a_bad_history_cursor_is_a_typed_400() {
        // Distinguishable from a filter error so the UI can tell its own paging bug from bad
        // operator input — the same split `api/eventlog.rs` makes.
        let resp = router(public_state())
            .oneshot(
                Request::builder()
                    .uri("/api/v1/alerts/history?before=yesterday")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["error"]["code"], "invalid_cursor");
    }

    /// The status of a history request, and its typed error code when the body is the ADR-019
    /// envelope.
    ///
    /// The code is `None` for a rejection axum produced itself — a typed query field that failed to
    /// deserialize answers with plain text, not the envelope. That is the accepted cost of declaring
    /// `severity`/`state` as the real enums: the generated OpenAPI union is worth more than the
    /// envelope on a request the WebUI's own `<select>` cannot produce, and `?limit=abc` already
    /// behaved this way.
    async fn history_error(query: &str) -> (StatusCode, Option<String>) {
        let resp = router(public_state())
            .oneshot(
                Request::builder()
                    .uri(format!("/api/v1/alerts/history?{query}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = resp.status();
        let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let code = serde_json::from_slice::<serde_json::Value>(&bytes)
            .ok()
            .and_then(|j| j["error"]["code"].as_str().map(str::to_owned));
        (status, code)
    }

    #[tokio::test]
    async fn a_bad_range_bound_is_a_filter_error_not_a_cursor_error() {
        // The cursor is the client's paging state; the range is what the operator typed. The UI
        // surfaces them differently, so they must not collapse into one code.
        for bad in ["since=yesterday", "until=2026-07-28"] {
            assert_eq!(
                history_error(bad).await,
                (StatusCode::BAD_REQUEST, Some("invalid_filter".to_owned())),
                "{bad}"
            );
        }
    }

    #[tokio::test]
    async fn an_unknown_severity_or_state_is_rejected_rather_than_ignored() {
        // Rejecting beats ignoring: a typo that silently dropped the filter would answer with the
        // whole log while the operator believes they are looking at criticals only. serde does this
        // rejection because the parameter is the real enum — which is also what stops the same word
        // meaning something different over MCP. (Hence no typed code: see `history_error`.)
        for bad in ["severity=fatal", "severity=CRITICAL", "state=broken"] {
            assert_eq!(history_error(bad).await.0, StatusCode::BAD_REQUEST, "{bad}");
        }
    }

    #[tokio::test]
    async fn a_malformed_filter_is_rejected_even_with_no_history_store() {
        // `public_state()` keeps no history, and this endpoint answers an empty page rather than a
        // 503 for that. So the parse has to happen first, or a client bug reads as "you have
        // reached the end of the log" — the one wrong answer a paging API can give.
        assert_eq!(
            history_error("before=yesterday").await.0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(history_error("since=nope").await.0, StatusCode::BAD_REQUEST);
        // …while a well-formed filter still gets the empty page.
        assert_eq!(
            history_error("severity=critical&resolved=false").await.0,
            StatusCode::OK
        );
    }

    // ── Live streams ────────────────────────────────────────────────────────

    /// State whose alert engine knows one node in `group` and one node in no group at all.
    fn state_with_one_grouped_node(node: NodeId, group: Uuid) -> ApiState {
        use crate::alerts::{AlertConfig, NodeMeta};
        let st = public_state();
        let mut meta = std::collections::HashMap::new();
        meta.insert(
            node,
            NodeMeta {
                folder_group: Some(group),
                ..NodeMeta::default()
            },
        );
        st.alerts.set_config(AlertConfig::new(Vec::new(), meta));
        st
    }

    /// Everything the stream puts on the wire, as the browser would receive it.
    ///
    /// `Event` has no public accessor, so the assertion is made against the rendered SSE text —
    /// which is the better thing to assert on anyway. `take_until` bounds it: the broadcast channel
    /// never closes, and `Sse` without a keep-alive ends its body exactly when the inner stream does.
    async fn wire(
        stream: impl futures::Stream<Item = Result<Event, Infallible>> + Send + 'static,
    ) -> String {
        use axum::response::IntoResponse as _;
        use futures::StreamExt as _;
        let bounded = stream.take_until(tokio::time::sleep(std::time::Duration::from_millis(100)));
        let body = Sse::new(bounded).into_response().into_body();
        let bytes = axum::body::to_bytes(body, 1 << 20).await.expect("sse body");
        String::from_utf8(bytes.to_vec()).expect("sse text")
    }

    #[tokio::test]
    async fn the_alert_stream_drops_out_of_scope_frames_without_calling_them_a_lag() {
        // A scoped subscriber must not see another group's alert — and must not be told to resync
        // either. `resync` means "you missed something, re-fetch"; firing it for a frame the caller
        // was never entitled to would send them back to REST on every event in the rest of the
        // fleet, and REST correctly answers without that alert, so the round trip is pure noise.
        let mine = NodeId::new();
        let group = Uuid::from_u128(7);
        let st = state_with_one_grouped_node(mine, group);
        let scope = crate::api::scope::NodeScope::Groups(std::sync::Arc::new(
            crate::api::scope::ScopeSet {
                visible: vec![group],
                breadcrumb: Vec::new(),
            },
        ));

        let rx = st.alerts.subscribe();
        let stream = sse_with_resync(rx, std::sync::Arc::downgrade(&st.alerts), scope, "alert");

        // One frame for the visible node, one for a node the engine has never heard of.
        st.alerts
            .broadcast_test_frame(yagra_alert::Subject::Node(mine), "{\"node\":\"mine\"}");
        st.alerts.broadcast_test_frame(
            yagra_alert::Subject::Node(NodeId::new()),
            "{\"node\":\"theirs\"}",
        );

        let text = wire(stream).await;
        assert!(
            text.contains("mine"),
            "the in-scope frame is served: {text}"
        );
        assert!(
            !text.contains("theirs"),
            "another group's alert must not reach a scoped subscriber: {text}"
        );
        assert!(
            !text.contains("resync"),
            "a filtered frame is not a lag: {text}"
        );
    }

    #[tokio::test]
    async fn an_unrestricted_subscriber_still_sees_every_frame() {
        // The overwhelmingly common case — every admin, and every deployment with no scopes
        // assigned. It must be unchanged by the filter, including for nodes the alert engine's
        // metadata snapshot has never seen (which a *scoped* caller is denied, fail-closed).
        let st = public_state();
        let rx = st.alerts.subscribe();
        let stream = sse_with_resync(
            rx,
            std::sync::Arc::downgrade(&st.alerts),
            crate::api::scope::NodeScope::All,
            "alert",
        );
        st.alerts
            .broadcast_test_frame(yagra_alert::Subject::Node(NodeId::new()), "{\"a\":1}");
        st.alerts
            .broadcast_test_frame(yagra_alert::Subject::Node(NodeId::new()), "{\"b\":2}");
        let text = wire(stream).await;
        assert!(
            text.contains("\"a\":1") && text.contains("\"b\":2"),
            "{text}"
        );
    }

    #[tokio::test]
    async fn a_stream_does_not_keep_the_engine_that_feeds_it_alive() {
        // ⚠️ Regression guard. The filter needs the alert engine's node→group map to decide
        // visibility — and the alert engine is also what owns this stream's broadcast sender. Hold
        // it strongly and the two pin each other alive: the sender is never dropped, so the body
        // never ends, so `route_table::every_listed_route_is_served` (which reads every route's
        // body to completion) hangs forever rather than failing. It did.
        use futures::StreamExt as _;
        let st = public_state();
        let rx = st.alerts.subscribe();
        let stream = sse_with_resync(
            rx,
            std::sync::Arc::downgrade(&st.alerts),
            crate::api::scope::NodeScope::All,
            "alert",
        );
        drop(st);
        // No `take_until` here — that is the whole point. If the stream held the engine strongly,
        // this would never return.
        assert!(
            std::pin::pin!(stream).collect::<Vec<_>>().await.is_empty(),
            "the stream must end once its sender is gone"
        );
    }

    #[tokio::test]
    async fn acking_stays_closed_on_a_public_dashboard() {
        // Reads are open there; writing ack state is not.
        let resp = router(public_state())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/alerts/ack")
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }
}
