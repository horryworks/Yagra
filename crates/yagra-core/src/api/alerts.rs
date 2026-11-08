// SPDX-License-Identifier: AGPL-3.0-only
//! Alerts: the active list, history and its aggregations, and the two live streams.
//!
//! ## No acknowledgement
//!
//! Yagra holds no escalation scheduler and no ack (ADR-015, ADR-233): it neither records nor shows
//! whether someone has acknowledged an alert. That belongs to the external incident tool.

use super::extract::{RequireView, Scoped};
use super::util::Ranked;
use super::{ApiError, ApiResult, ApiState};
use crate::alerts::history::AlertHistoryRow;
use axum::{
    extract::{Query, State},
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Response,
    },
    routing::get,
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
        .route("/api/v1/alerts/top-nodes", get(alert_top_nodes))
        .route("/api/v1/alerts/calendar", get(alert_calendar))
        .route("/api/v1/alerts/transitions", get(alert_transitions))
        .route("/api/v1/stream/alerts", get(stream_alerts))
        .route("/api/v1/stream/node-states", get(stream_node_states))
}

// ── Decoration ───────────────────────────────────────────────────────────

/// An active alert plus what the list adds to it: its subject decomposed, its title, its port.
///
/// `subject_kind` and `subject_name` decompose the alert's `node` field, which carries either a
/// node's UUID, `pool:<name>` or `meraki_org:<uuid>`. The live alert stream emits the same three keys.
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
    /// What the alert is called, in English — `SNMP not responding` rather than `snmp_up`
    /// (ADR-196). The metric's own name when Yagra has none for it; absent for an alert with no
    /// metric at all.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// The name of the port a per-port alert is about (`ifName`), read from the interface inventory
    /// when the alert is read rather than stored with it (ADR-196 decision 6). Absent for an alert
    /// about no port, and for a port whose name is not known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub if_name: Option<String>,
}

/// An alert-history row plus its title and its port's current name.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub(crate) struct AlertHistoryView {
    #[serde(flatten)]
    pub row: AlertHistoryRow,
    /// What the alert is called, in English (ADR-196). See [`ActiveAlertView::title`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// The port's name, as it is called **now** (ADR-196 decision 6) — not necessarily what it was
    /// called when this row was written. See [`ActiveAlertView::if_name`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub if_name: Option<String>,
}

/// The names of the ports a page of alerts (or a node's status) is about, keyed by
/// `(node, ifindex)` (ADR-196 decision 6).
///
/// One primary-key read per port, and none when no alert is about a port — the ordinary case. A
/// port name is decorative, so a failed read (or skeleton mode, with no inventory to read) answers
/// an empty map and the alerts are served with their ifIndex alone.
pub(crate) async fn port_names(
    st: &ApiState,
    ports: impl IntoIterator<Item = (Uuid, u32)>,
) -> HashMap<(Uuid, u32), String> {
    match st.admin.as_ref() {
        Some(admin) => port_names_of(admin, ports).await,
        None => HashMap::new(),
    }
}

/// [`port_names`] for a caller already holding the live write side — the threshold list, which
/// names a port rule's target the way an alert names its port, and the neighbour views, which
/// name a CDP `ifindex <n>` port by its interface name.
pub(crate) async fn port_names_of(
    admin: &super::AdminState,
    ports: impl IntoIterator<Item = (Uuid, u32)>,
) -> HashMap<(Uuid, u32), String> {
    let wanted: Vec<(Uuid, u32)> = ports
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    if wanted.is_empty() {
        return HashMap::new();
    }
    admin
        .repo
        .port_names_for(&wanted)
        .await
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "failed to read port names; serving ifIndex only");
            HashMap::new()
        })
}

/// The name of each node's site — the nearest Site folder at or above its folder (ADR-248) —
/// keyed by node id. A node with no Site folder above it is absent, and so is one outside the
/// caller's scope. Decorative like [`port_names`]: a failed read (or skeleton mode) answers an
/// empty map and the alerts are served without it.
pub(crate) async fn site_names(
    st: &ApiState,
    scope: &super::scope::NodeScope,
    nodes: impl IntoIterator<Item = Uuid>,
) -> HashMap<Uuid, String> {
    let Some(admin) = st.admin.as_ref() else {
        return HashMap::new();
    };
    let ids: Vec<Uuid> = nodes
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    if ids.is_empty() {
        return HashMap::new();
    }
    let (found, groups) = tokio::join!(
        admin.repo.node_names_and_groups(scope.group_filter(), &ids),
        admin.groups.list(),
    );
    let (Ok(found), Ok(groups)) = (found, groups) else {
        tracing::warn!("failed to read node folders; serving alerts without their site");
        return HashMap::new();
    };
    let names: HashMap<Uuid, &str> = groups.iter().map(|g| (g.id, g.name.as_str())).collect();
    crate::inventory::sites::nearest_site_by_node(
        &groups,
        found.iter().map(|(id, (_, group_id))| (*id, *group_id)),
    )
    .into_iter()
    .filter_map(|(node, site)| Some((node, (*names.get(&site?)?).to_owned())))
    .collect()
}

/// The `(node, ifindex)` a live alert is about, when it is about one port of a node — the key
/// [`port_names`] answers by.
fn port_key(alert: &Alert) -> Option<(Uuid, u32)> {
    Some((alert.node()?.as_uuid(), alert.ifindex?.0))
}

/// A live alert with the name of the port it is about (ADR-196 decision 6) — the same `if_name`
/// [`ActiveAlertView`] carries, for a surface that serves an alert without the rest of that view
/// (a node's own status).
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub(crate) struct PortNamedAlert {
    #[serde(flatten)]
    pub alert: Alert,
    /// The name of the port this alert is about (`ifName`), read from the interface inventory when
    /// the alert is read. Absent for an alert about no port, and for a port whose name is not
    /// known. See [`ActiveAlertView::if_name`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub if_name: Option<String>,
}

/// Name the ports a list of live alerts is about, through the one lookup every alert surface uses.
pub(crate) async fn with_port_names(st: &ApiState, alerts: Vec<Alert>) -> Vec<PortNamedAlert> {
    let ports = port_names(st, alerts.iter().filter_map(port_key)).await;
    alerts
        .into_iter()
        .map(|alert| PortNamedAlert {
            if_name: port_key(&alert).and_then(|k| ports.get(&k).cloned()),
            alert,
        })
        .collect()
}

/// Decorate the active-alert list (pure; unit-tested without a DB).
fn decorate_alerts(alerts: Vec<Alert>) -> Vec<ActiveAlertView> {
    alerts
        .into_iter()
        .map(|alert| ActiveAlertView {
            subject_kind: alert.subject.kind(),
            subject_name: alert.subject.name().map(str::to_owned),
            title: crate::tsdb::metric_meaning::alert_title_of(&alert.metric),
            if_name: None,
            alert,
        })
        .collect()
}

/// Decorate a history page (pure; unit-tested without a DB).
fn decorate_history(rows: Vec<AlertHistoryRow>) -> Vec<AlertHistoryView> {
    rows.into_iter()
        .map(|row| AlertHistoryView {
            title: row
                .metric
                .as_deref()
                .and_then(crate::tsdb::metric_meaning::alert_title_of),
            if_name: None,
            row,
        })
        .collect()
}

// ── Reads ────────────────────────────────────────────────────────────────────

#[utoipa::path(
    get, path = "/api/v1/alerts", tag = "alerts",
    responses(
        (status = 200, description = "Every active alert", body = Vec<ActiveAlertView>),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks the read permission", body = super::error::ErrorBody),
    ),
)]
async fn list_alerts(
    _perm: RequireView,
    Scoped(scope): Scoped,
    State(st): State<ApiState>,
) -> Json<Vec<ActiveAlertView>> {
    // Filter before decorating, so no port lookup is spent on an out-of-scope alert.
    let mut views = decorate_alerts(visible_active_alerts(&st, &scope));
    let ports = port_names(&st, views.iter().filter_map(|v| port_key(&v.alert))).await;
    for view in &mut views {
        fill_subject_name(&st, &mut view.subject_name, &view.alert.subject);
        view.if_name = port_key(&view.alert).and_then(|k| ports.get(&k).cloned());
    }
    Json(views)
}

/// The active alerts `scope` may see — the set both `GET /alerts` and `/mcp`'s `get_active_alerts`
/// start from (ADR-184). Filtered first, so nothing downstream (a port lookup, a limit) is spent on
/// a row that is about to be dropped.
pub(crate) fn visible_active_alerts(st: &ApiState, scope: &super::scope::NodeScope) -> Vec<Alert> {
    st.alerts
        .active_alerts()
        .into_iter()
        .filter(|a| scope.allows_subject(st, &a.subject))
        .collect()
}

/// Give a row the engine's name for its subject when it has none of its own. A subject identified by
/// id that is not a node (a Meraki organization) carries no name, so neither surface's decorator has
/// one to give; the engine's snapshot does.
pub(crate) fn fill_subject_name(
    st: &ApiState,
    name: &mut Option<String>,
    subject: &yagra_alert::Subject,
) {
    if name.is_none() {
        *name = st.alerts.subject_display_name(subject);
    }
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
    /// Comma-separated severities (`info`, `warning`, `critical`); empty or absent means every
    /// severity. An unknown token is rejected rather than ignored — a dropped token would silently
    /// widen the answer to everything.
    pub severity: Option<String>,
    /// Comma-separated node states; empty or absent means every state.
    pub state: Option<String>,
    /// `false` for fires only, `true` for clears only. Omit for both.
    pub resolved: Option<bool>,
    /// Only transitions whose metric name contains this text (case-insensitive). Liveness rows
    /// store no metric and therefore never match.
    pub metric: Option<String>,
    /// Only transitions about this node. Rows about something other than a node (a poller pool)
    /// are excluded by construction.
    pub node_id: Option<Uuid>,
    /// Only transitions about nodes whose **current name** contains this text (case-insensitive).
    /// Distinct from `node_id`, which names exactly one node.
    pub node_q: Option<String>,
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
            severity: q.severity.as_deref(),
            state: q.state.as_deref(),
            resolved: q.resolved,
            metric: q.metric.as_deref(),
            node_id: q.node_id,
            node_q: q.node_q.as_deref(),
            group_id: q.group_id,
        },
    )
    .await?;
    let mut views = decorate_history(rows);
    let ports = port_names(
        &st,
        views
            .iter()
            .filter_map(|v| Some((v.row.subject()?.node()?.as_uuid(), v.row.ifindex?))),
    )
    .await;
    // As above: a history row of a Meraki organization stores an id and no name.
    for view in &mut views {
        view.if_name = view
            .row
            .subject()
            .and_then(|s| s.node())
            .zip(view.row.ifindex)
            .and_then(|(n, i)| ports.get(&(n.as_uuid(), i)).cloned());
        if view.row.subject_name.is_none() {
            view.row.subject_name = view
                .row
                .subject()
                .and_then(|s| st.alerts.subject_display_name(&s));
        }
    }
    Ok(Json(views))
}

/// The raw filter fields, as either surface receives them.
///
/// ⚠️ **This struct is the REST/MCP parity seam for history**, the same role
/// `eventlog::EventFilterInput` plays for events: a dimension added here is a compile error in
/// `mcp/tools.rs` until the tool takes it too. What the compiler still cannot see is whether the
/// tool *offers* the dimension as a parameter or hard-codes it to `None` — that half is covered by
/// `mcp/tools/guards.rs::every_shared_filter_seam_is_passed_through_whole`, which reads this struct's own
/// field list rather than a copy of it.
///
/// `severity` and `state` are comma-separated text on both surfaces rather than enums. REST could
/// have kept `Option<Severity>` and let serde parse it, but then the two surfaces would accept
/// different vocabularies for the same field, and the multi-value spelling would live in two
/// places. Both go through [`super::util::parse_set`], which is also what events use.
pub(crate) struct HistoryFilterInput<'a> {
    pub limit: Option<i64>,
    pub before: Option<&'a str>,
    pub before_id: Option<Uuid>,
    pub since: Option<&'a str>,
    pub until: Option<&'a str>,
    pub severity: Option<&'a str>,
    pub state: Option<&'a str>,
    pub resolved: Option<bool>,
    pub metric: Option<&'a str>,
    pub node_id: Option<Uuid>,
    pub node_q: Option<&'a str>,
    pub group_id: Option<Uuid>,
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
    use super::util::ts_param as ts;
    // Parse before checking for a store: a malformed cursor is the client's bug whether or not this
    // deployment has history, and answering `200 []` to it would let a paging bug look like "you
    // have reached the end".
    let before = ts(input.before, "before", "invalid_cursor")?;
    let since = ts(input.since, "since", "invalid_filter")?;
    let until = ts(input.until, "until", "invalid_filter")?;

    // Same set spelling as events (ADR-053): comma-separated, order-preserving, empty means
    // unfiltered, and an unknown token is a 400. The last is the load-bearing half — dropping a
    // token widens the answer, and a widened list reads exactly like the answer to the narrower
    // question that was asked.
    let severity = super::util::parse_set(
        "severity",
        input.severity,
        "info, warning or critical",
        Severity::from_token,
    )?;
    let state = super::util::parse_set(
        "state",
        input.state,
        &super::util::token_list(NodeState::ALL.iter().map(NodeState::as_str)),
        NodeState::from_token,
    )?;
    let metric = super::util::normalize_search(input.metric);
    let node_q = super::util::normalize_search(input.node_q);

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
    let filter = crate::alerts::history::HistoryFilter {
        before,
        before_id: input.before_id,
        since,
        until,
        severity,
        state,
        resolved: input.resolved,
        node_id: input.node_id,
        in_group: in_group.as_deref(),
        metric: metric.as_deref(),
        node_q: node_q.as_deref(),
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
    let limit = super::util::page_limit(limit, 6, 50) as usize;
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
///
/// `ports` is the inventory to name a per-port alert's port from (ADR-196 Inc.4): `Some` on the
/// alert stream with an inventory, `None` on the node-state stream and in skeleton mode. Only a
/// visible frame is named, so a scoped subscriber never costs a read for an alert it is not sent.
fn sse_with_resync(
    rx: tokio::sync::broadcast::Receiver<crate::alerts::StreamFrame>,
    alerts: std::sync::Weak<crate::alerts::AlertManager>,
    scope: super::scope::NodeScope,
    what: &'static str,
    ports: Option<std::sync::Arc<yagra_base::repo::NodeRepo>>,
) -> impl futures::Stream<Item = Result<Event, Infallible>> {
    tokio_stream::wrappers::BroadcastStream::new(rx).filter_map(move |r| {
        // Cloned per frame so the closure stays `FnMut` rather than consuming its captures; all
        // are cheap (a `Weak` bump and two `Arc` clones).
        let (alerts, scope, ports) = (alerts.clone(), scope.clone(), ports.clone());
        async move {
            match r {
                Ok((subject, json)) => {
                    let visible = scope.is_all()
                        || alerts
                            .upgrade()
                            .is_some_and(|a| scope.allows_subject_in(&a, &subject));
                    if !visible {
                        return None;
                    }
                    let named = match ports {
                        Some(repo) => with_live_port_name(&repo, &subject, &json).await,
                        None => None,
                    };
                    Some(Ok::<_, Infallible>(
                        Event::default().data(named.as_deref().unwrap_or(&json)),
                    ))
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

/// How long a live frame's port name is reused before it is read again (ADR-196 Inc.4). A port is
/// renamed rarely, and the REST list re-reads it on every resync anyway.
const LIVE_PORT_NAME_TTL: std::time::Duration = std::time::Duration::from_secs(60);

/// How many ports the live-frame cache holds before it starts over. A flapping chassis sends many
/// frames for few ports; this bounds the cache without an eviction order to maintain.
const LIVE_PORT_NAME_CAP: usize = 10_000;

/// Port names already read for live frames, shared by every subscriber of this core, so an alert
/// storm seen by ten open tabs reads each port once rather than ten times. A missing name is
/// cached too, so a port with none is not re-read on every frame.
type LivePortNames = HashMap<(Uuid, u32), (std::time::Instant, Option<String>)>;
static LIVE_PORT_NAMES: std::sync::LazyLock<std::sync::Mutex<LivePortNames>> =
    std::sync::LazyLock::new(Default::default);

/// The `(node, ifindex)` a live alert frame is about, with the frame parsed, or `None` for a frame
/// about no port or no node. The substring test spares the parse for the ordinary frame.
fn frame_port(
    subject: &yagra_alert::Subject,
    json: &str,
) -> Option<((Uuid, u32), serde_json::Map<String, serde_json::Value>)> {
    let yagra_alert::Subject::Node(node) = subject else {
        return None;
    };
    if !json.contains("\"ifindex\"") {
        return None;
    }
    let serde_json::Value::Object(frame) = serde_json::from_str(json).ok()? else {
        return None;
    };
    let ifindex = u32::try_from(frame.get("ifindex")?.as_u64()?).ok()?;
    Some(((node.as_uuid(), ifindex), frame))
}

/// The frame with `if_name` added, the same field `ActiveAlertView` carries, or `None` to send it
/// as it is: no port, no name, or the inventory could not be read (a name is decorative, so the
/// frame goes out with its ifIndex rather than not at all).
async fn with_live_port_name(
    repo: &yagra_base::repo::NodeRepo,
    subject: &yagra_alert::Subject,
    json: &str,
) -> Option<String> {
    let (key, mut frame) = frame_port(subject, json)?;
    let cached = {
        let cache = LIVE_PORT_NAMES
            .lock()
            .expect("live port-name cache poisoned");
        cache
            .get(&key)
            .filter(|(at, _)| at.elapsed() < LIVE_PORT_NAME_TTL)
            .map(|(_, name)| name.clone())
    };
    let name = match cached {
        Some(name) => name,
        None => {
            let read = repo.port_names_for(&[key]).await.map_err(|e| {
                tracing::warn!(error = %e, "failed to read a port name for a live alert; sending ifIndex only");
            });
            // Not cached on a failed read: the next frame tries again.
            let name = read.ok()?.remove(&key);
            let mut cache = LIVE_PORT_NAMES
                .lock()
                .expect("live port-name cache poisoned");
            if cache.len() >= LIVE_PORT_NAME_CAP {
                cache.clear();
            }
            cache.insert(key, (std::time::Instant::now(), name.clone()));
            name
        }
    };
    frame.insert("if_name".to_owned(), serde_json::Value::String(name?));
    Some(serde_json::Value::Object(frame).to_string())
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
    let ports = st
        .admin
        .as_ref()
        .map(|admin| std::sync::Arc::clone(&admin.repo));
    Sse::new(sse_with_resync(rx, alerts, scope, "alert", ports))
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
    Sse::new(sse_with_resync(rx, alerts, scope, "node-state", None))
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
            ifindex: None,
            row: None,
            row_name: None,
            notify_held_until: None,
        };
        let out = decorate_alerts(vec![alert]);
        let json = serde_json::to_value(&out[0]).unwrap();
        assert_eq!(json["node"], "pool:tokyo");
        assert!(json["node"].is_string());
        assert_eq!(json["subject_kind"], "pool");
        assert_eq!(json["subject_name"], "tokyo");
    }

    #[tokio::test]
    async fn the_alert_list_serves_in_skeleton_mode() {
        // Skeleton mode has no store behind it, and the list must still serve — a blank alert list
        // during an incident is the worst answer it could give.
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

    /// Which live frames the SSE edge asks a port name for (ADR-196 Inc.4): a node's alert that
    /// names a port, and nothing else — a pool or Meraki alert has no inventory, and a node-level
    /// alert has no port.
    #[test]
    fn only_a_node_alert_about_a_port_asks_for_a_port_name() {
        let node = Uuid::from_u128(1);
        let subject = yagra_alert::Subject::Node(NodeId::from(node));
        let (key, frame) = frame_port(
            &subject,
            r#"{"node":"x","ifindex":7,"severity":"critical"}"#,
        )
        .expect("a port alert");
        assert_eq!(key, (node, 7));
        assert_eq!(
            frame["severity"], "critical",
            "the rest of the frame is kept"
        );

        assert!(frame_port(&subject, r#"{"node":"x","severity":"critical"}"#).is_none());
        assert!(frame_port(&subject, r#"{"node":"x","ifindex":null}"#).is_none());
        let pool = yagra_alert::Subject::Pool("tokyo".to_owned());
        assert!(frame_port(&pool, r#"{"node":"pool:tokyo","ifindex":7}"#).is_none());
    }

    /// **A live port alert reaches the screen with its port's name, and one with no known name goes
    /// out unchanged** — never dropped, since the name is decorative (ADR-196 Inc.4).
    #[sqlx::test(migrator = "yagra_base::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_live_port_alert_is_named_from_the_inventory(pool: sqlx::PgPool) {
        let node = yagra_base::pgtest::node(&pool, "sw1", 1, None).await;
        let repo = yagra_base::pgtest::repo(pool.clone());
        sqlx::query("INSERT INTO interfaces (node_id, ifindex, if_name) VALUES ($1, 7, 'Gi0/7')")
            .bind(node)
            .execute(&pool)
            .await
            .expect("seed a port");
        let subject = yagra_alert::Subject::Node(NodeId::from(node));

        let named = with_live_port_name(&repo, &subject, r#"{"node":"x","ifindex":7}"#)
            .await
            .expect("a known port is named");
        let v: serde_json::Value = serde_json::from_str(&named).unwrap();
        assert_eq!(v["if_name"], "Gi0/7");
        assert_eq!(v["ifindex"], 7);

        assert!(
            with_live_port_name(&repo, &subject, r#"{"node":"x","ifindex":8}"#)
                .await
                .is_none(),
            "a port with no name is sent as it was"
        );
    }

    /// **A node's own status names its port alerts the way the Active list does** — the Overview
    /// tab used to show `port 10106` beside an Active list reading `Gi1/0/6` for the same alert,
    /// because `GET /nodes/{id}/status` served the bare alert (ADR-196 decision 6).
    #[sqlx::test(migrator = "yagra_base::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_node_status_alert_is_named_from_the_inventory(pool: sqlx::PgPool) {
        let node = yagra_base::pgtest::node(&pool, "sw1", 1, None).await;
        sqlx::query("INSERT INTO interfaces (node_id, ifindex, if_name) VALUES ($1, 7, 'Gi0/7')")
            .bind(node)
            .execute(&pool)
            .await
            .expect("seed a port");
        let st = crate::api::tests_support::live_state(pool).await;
        let alert = |ifindex: Option<u32>| Alert {
            subject: yagra_alert::Subject::Node(NodeId::from(node)),
            check: yagra_common::CheckId::from(Uuid::from_u128(3)),
            severity: Severity::Critical,
            state: NodeState::Critical,
            at_unix_ms: 1,
            root_cause: None,
            flapping: false,
            metric: "if_oper_status".to_string(),
            breach: None,
            ifindex: ifindex.map(yagra_common::IfIndex),
            row: None,
            row_name: None,
            notify_held_until: None,
        };
        let out = with_port_names(&st, vec![alert(Some(7)), alert(Some(8)), alert(None)]).await;
        let json: Vec<serde_json::Value> = out
            .iter()
            .map(|a| serde_json::to_value(a).unwrap())
            .collect();
        assert_eq!(json[0]["if_name"], "Gi0/7");
        assert_eq!(json[0]["ifindex"], 7, "the alert's own fields stay flat");
        assert!(
            json[1].get("if_name").is_none(),
            "a port with no name keeps its ifIndex alone"
        );
        assert!(
            json[2].get("if_name").is_none(),
            "a node-level alert has no port"
        );
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
        let stream = sse_with_resync(
            rx,
            std::sync::Arc::downgrade(&st.alerts),
            scope,
            "alert",
            None,
        );

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
            None,
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
            None,
        );
        drop(st);
        // No `take_until` here — that is the whole point. If the stream held the engine strongly,
        // this would never return.
        assert!(
            std::pin::pin!(stream).collect::<Vec<_>>().await.is_empty(),
            "the stream must end once its sender is gone"
        );
    }
}
