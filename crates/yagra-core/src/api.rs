//! Northbound REST API (`/api/v1`).
//!
//! Path-versioned (ADR-019). Responses are JSON; errors use the fixed envelope
//! `{"error": {"code", "message"}}` so clients never see a raw internal error. Readings
//! come from the [`MetricStore`] (VictoriaMetrics live, in-memory for the skeleton) and
//! the inventory from a [`NodeListing`]. A node's display state and the alert endpoints are
//! served from the live [`AlertManager`] (committed liveness + threshold roll-up + active
//! alerts). Cursor pagination is in; RBAC scoping lands as the API grows.

use crate::alerts::AlertManager;
use crate::analysis::{AnalysisRunner, AnalysisTool, JobParams, ScopeKind};
use crate::audit::AuditRepo;
use crate::auth::{AuthError, SessionStore, UserCreateOutcome, UserMutation, UserStore};
use crate::classification::{ClassificationRepo, Classifier};
use crate::collection::{CollectionRepo, CreateTemplateOutcome};
use crate::dashboard::{DashboardRepo, SharedDashboardRepo};
use crate::discovery::DiscoveryRunner;
use crate::groups::{placement_order, would_create_cycle, GroupRepo, GroupType};
use crate::history::AlertHistoryStore;
use crate::maintenance::MaintenanceRepo;
use crate::mib::MibRepo;
use crate::notifications::{ChannelConfig, NotificationRepo};
use crate::repo::{NodeListing, NodeRepo};
use crate::scheduler::PollDispatcher;
use crate::secrets::CredentialStore;
use crate::store::{DeltaDirection, InterfaceTopMetric, MetricPoint, MetricStore, TopAgg};
use crate::thresholds::ThresholdStore;
use axum::{
    extract::{Path, Query, Request, State},
    http::{header::AUTHORIZATION, HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Response,
    },
    routing::{delete, get, post, put},
    Json, Router,
};
use futures::stream::StreamExt;
use serde::{Deserialize, Serialize};
use std::convert::Infallible;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;
use yagra_common::{
    is_ssrf_blocked, resolve_collection_set, IfIndex, NodeId, NodeState, Permission,
    ProfileCategory, Role, SeriesKey, Severity, UrlCheckConfig,
};

/// Live-only write side: inventory, credentials, and user accounts. Absent in skeleton
/// mode, where the management/auth endpoints return 503.
pub struct AdminState {
    pub repo: Arc<NodeRepo>,
    pub creds: Arc<CredentialStore>,
    pub users: Arc<UserStore>,
    pub thresholds: Arc<ThresholdStore>,
    pub collection: Arc<CollectionRepo>,
    pub notifications: Arc<NotificationRepo>,
    pub mib: Arc<MibRepo>,
    pub discovery: Arc<DiscoveryRunner>,
    pub maintenance: Arc<MaintenanceRepo>,
    /// Operator-editable device-classification rules (CRUD) + the in-memory classifier the
    /// discovery runner consults; handlers reload the classifier after a rule edit.
    pub classification: Arc<ClassificationRepo>,
    pub classifier: Arc<Classifier>,
    pub groups: Arc<GroupRepo>,
    pub audit: Arc<AuditRepo>,
    /// Per-user "My Dashboard" widget layouts (server-side persistence).
    pub dashboards: Arc<DashboardRepo>,
    /// The single global "Shared Dashboard" layout (admin-edited, shown to all users).
    pub shared_dashboard: Arc<SharedDashboardRepo>,
    /// Live poll-loop self-monitoring counters (the poller-health endpoint).
    pub scheduler_stats: Arc<crate::scheduler::SchedulerStats>,
    /// On-demand poll dispatch (the "poll now" action) — shares the scheduler's job-building so a
    /// manual poll matches a periodic one. Bus-only (core⇄poller never call directly, ADR-003).
    pub poll: Arc<PollDispatcher>,
    /// Troubleshoot deep-diagnostic jobs (anomaly/correlation/capacity/flap). A TSDB-read
    /// background computation in core (ADR-022) — not a poller/bus job.
    pub analysis: Arc<AnalysisRunner>,
    /// Per-node URL-monitor configs (HTTP/HTTPS endpoint checks).
    pub url_checks: Arc<crate::url_check::UrlCheckRepo>,
}

/// Default range window when `from`/`to` are omitted (seconds).
const DEFAULT_RANGE_SECS: i64 = 3600;
/// Default range step when `step` is omitted (seconds).
const DEFAULT_STEP_SECS: u64 = 60;

/// Shared API state: the metric store, the node inventory source, and the alert engine.
#[derive(Clone)]
pub struct ApiState {
    /// TSDB read/write seam.
    pub store: Arc<dyn MetricStore>,
    /// Inventory read seam.
    pub nodes: Arc<dyn NodeListing>,
    /// Alert engine (active alerts + live event stream).
    pub alerts: Arc<AlertManager>,
    /// Write side (inventory + credentials + users); `None` in skeleton mode.
    pub admin: Option<Arc<AdminState>>,
    /// Bearer-token sessions for local auth.
    pub sessions: Arc<SessionStore>,
    /// Alert history (read); `None` in skeleton mode.
    pub history: Option<Arc<AlertHistoryStore>>,
    /// When true, read-only endpoints skip authentication (public read-only dashboard).
    /// When false (default), they require a valid session with `View` (every role has it).
    pub public_dashboard: bool,
}

/// Build the `/api/v1` router backed by the given state.
pub fn router(state: ApiState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/api/v1/config", get(get_config).put(update_config))
        .route("/api/v1/nodes", get(list_nodes).post(create_node))
        .route("/api/v1/nodes/:node_id", get(get_node).delete(delete_node))
        .route("/api/v1/nodes/:node_id/status", get(get_node_status))
        .route("/api/v1/nodes/:node_id/poll", post(poll_node_now))
        .route("/api/v1/nodes/:node_id/bindings", put(set_node_bindings))
        .route(
            "/api/v1/nodes/:node_id/url-check",
            get(get_url_check)
                .put(set_url_check)
                .delete(delete_url_check),
        )
        .route("/api/v1/url-monitors", post(create_url_monitor))
        .route("/api/v1/nodes/:node_id/group", put(set_node_group))
        .route("/api/v1/nodes/:node_id/placement", put(place_node))
        .route(
            "/api/v1/node-groups",
            get(list_node_groups).post(create_node_group),
        )
        .route(
            "/api/v1/node-groups/:id",
            put(update_node_group).delete(delete_node_group),
        )
        .route("/api/v1/node-groups/:id/placement", put(place_group))
        .route("/api/v1/node-groups/:id/geo", put(set_node_group_geo))
        .route("/api/v1/profiles", get(list_profiles).post(create_profile))
        .route(
            "/api/v1/profiles/:id",
            put(update_profile).delete(delete_profile),
        )
        .route(
            "/api/v1/nodes/:node_id/metrics/:metric",
            get(get_node_metric),
        )
        .route(
            "/api/v1/nodes/:node_id/metrics/:metric/range",
            get(get_node_metric_range),
        )
        .route("/api/v1/metrics/top", get(top_metrics))
        .route("/api/v1/metrics/interface-top", get(interface_top))
        .route("/api/v1/metrics/interface-delta", get(interface_delta))
        .route(
            "/api/v1/credentials",
            get(list_credentials).post(create_credential),
        )
        .route(
            "/api/v1/credentials/:id",
            put(update_credential).delete(delete_credential),
        )
        .route(
            "/api/v1/thresholds",
            get(list_thresholds).post(create_threshold),
        )
        .route("/api/v1/thresholds/:id", delete(delete_threshold))
        .route(
            "/api/v1/nodes/:node_id/collection",
            get(list_node_collection).post(create_node_collection),
        )
        .route(
            "/api/v1/collection/:item_id",
            delete(delete_collection_item),
        )
        .route(
            "/api/v1/nodes/:node_id/interfaces",
            get(list_node_interfaces),
        )
        .route(
            "/api/v1/nodes/:node_id/interfaces/:ifindex/series",
            get(get_interface_series),
        )
        .route(
            "/api/v1/collection-templates",
            get(list_collection_templates).post(create_collection_template),
        )
        .route(
            "/api/v1/collection-templates/:id",
            delete(delete_collection_template),
        )
        .route(
            "/api/v1/collection-templates/:id/items",
            get(list_template_items).post(create_template_item),
        )
        .route(
            "/api/v1/collection-templates/:id/items/:item_id",
            delete(delete_template_item),
        )
        .route(
            "/api/v1/profiles/:id/templates",
            get(list_profile_templates).put(set_profile_templates),
        )
        .route("/api/v1/users", get(list_users).post(create_user))
        .route("/api/v1/users/:id", delete(delete_user))
        .route("/api/v1/users/:id/role", put(set_user_role))
        .route("/api/v1/users/:id/enabled", put(set_user_status))
        .route("/api/v1/users/:id/password", put(set_user_password))
        .route("/api/v1/auth/login", post(login))
        .route("/api/v1/auth/me", get(auth_me))
        .route("/api/v1/roles", get(list_roles))
        .route("/api/v1/alerts", get(list_alerts))
        .route("/api/v1/alerts/history", get(list_alert_history))
        .route("/api/v1/alerts/top-nodes", get(alert_top_nodes))
        .route("/api/v1/alerts/calendar", get(alert_calendar))
        .route("/api/v1/alerts/transitions", get(alert_transitions))
        .route("/api/v1/topology", get(get_topology))
        .route("/api/v1/fleet/coverage", get(fleet_coverage))
        .route("/api/v1/fleet/state-history", get(fleet_state_history))
        .route("/api/v1/metrics/throughput-range", get(throughput_range))
        .route("/api/v1/metrics/interface-heatmap", get(interface_heatmap))
        .route("/api/v1/poller-health", get(poller_health))
        .route("/api/v1/stream/alerts", get(stream_alerts))
        .route(
            "/api/v1/notification-channels",
            get(list_notification_channels).post(create_notification_channel),
        )
        .route(
            "/api/v1/notification-channels/:id",
            put(set_notification_channel_enabled).delete(delete_notification_channel),
        )
        .route(
            "/api/v1/routing-rules",
            get(list_routing_rules).post(create_routing_rule),
        )
        .route(
            "/api/v1/routing-rules/:id",
            put(set_routing_rule_enabled).delete(delete_routing_rule),
        )
        .route(
            "/api/v1/mib-catalog",
            get(list_mib_catalog).post(create_mib_entry),
        )
        .route("/api/v1/mib-catalog/:id", delete(delete_mib_entry))
        .route("/api/v1/discovery/scan", post(start_discovery_scan))
        .route("/api/v1/discovery/scan/:id", get(get_discovery_scan))
        .route("/api/v1/discovery/import", post(import_discovered))
        .route("/api/v1/discovery/candidates", get(discovery_candidates))
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
        .route(
            "/api/v1/classification-rules",
            get(list_classification_rules).post(create_classification_rule),
        )
        .route(
            "/api/v1/classification-rules/:id",
            put(update_classification_rule).delete(delete_classification_rule),
        )
        .route(
            "/api/v1/maintenance-windows",
            get(list_maintenance_windows).post(create_maintenance_window),
        )
        .route(
            "/api/v1/maintenance-windows/:id",
            put(set_maintenance_window_enabled).delete(delete_maintenance_window),
        )
        .route("/api/v1/mutes", get(list_mutes).post(create_mute))
        .route("/api/v1/mutes/:id", delete(delete_mute))
        .route("/api/v1/audit", get(list_audit))
        .route("/api/v1/dashboard", get(get_dashboard).put(put_dashboard))
        .route(
            "/api/v1/shared-dashboard",
            get(get_shared_dashboard).put(put_shared_dashboard),
        )
        // Audit middleware: records every mutating /api/v1 request (who + method/path +
        // status) so new write endpoints are covered automatically (security.md).
        .layer(middleware::from_fn_with_state(state.clone(), audit_mw))
        .with_state(state)
}

/// Username recorded when a mutating request carries no valid session.
const AUDIT_ANONYMOUS: &str = "(unauthenticated)";

/// Record one audit entry, best-effort: auditing must never take the API down, so
/// failures are logged and swallowed.
async fn audit_record(audit: &AuditRepo, username: &str, action: &str, status: u16) {
    if let Err(e) = audit.record(username, action, status).await {
        tracing::warn!(error = %e, %action, "audit record failed");
    }
}

/// Middleware: append an audit row for every mutating `/api/v1` request. Auth endpoints
/// are excluded here — login is recorded by its handler (with the attempted username,
/// never the credential). Reads are not audited.
async fn audit_mw(State(st): State<ApiState>, req: Request, next: Next) -> Response {
    let method = req.method().clone();
    let path = req.uri().path().to_owned();
    let mutating = matches!(method.as_str(), "POST" | "PUT" | "DELETE" | "PATCH");
    let audited = mutating && path.starts_with("/api/v1/") && !path.starts_with("/api/v1/auth/");
    // Resolve the actor before the handler runs (the request is consumed by it).
    let username = if audited {
        bearer(req.headers())
            .and_then(|t| st.sessions.lookup(t))
            .map(|s| s.username)
    } else {
        None
    };
    let resp = next.run(req).await;
    if audited {
        if let Some(admin) = st.admin.as_ref() {
            let user = username.as_deref().unwrap_or(AUDIT_ANONYMOUS);
            let action = format!("{method} {path}");
            audit_record(&admin.audit, user, &action, resp.status().as_u16()).await;
        }
    }
    resp
}

/// Liveness probe for the deploy/orchestrator — no auth, no store access.
async fn healthz() -> &'static str {
    "ok"
}

/// Public client bootstrap config (no secrets): tells the WebUI whether read endpoints
/// are open and whether interactive login is available, so it can decide up front whether
/// to gate behind a login screen. Also exposes the current global default polling interval so
/// the System-settings page can render it. Intentionally unauthenticated (no secrets here).
async fn get_config(State(st): State<ApiState>) -> Response {
    let default_poll_interval_secs = match st.admin.as_ref() {
        Some(admin) => admin
            .repo
            .get_default_poll_interval()
            .await
            .unwrap_or(crate::config::DEFAULT_POLL_INTERVAL_SECS),
        None => crate::config::DEFAULT_POLL_INTERVAL_SECS,
    };
    Json(serde_json::json!({
        "public_dashboard": st.public_dashboard,
        "auth_available": st.admin.is_some(),
        "default_poll_interval_secs": default_poll_interval_secs,
    }))
    .into_response()
}

/// Update the global default polling interval (seconds). `ManageConfig`-gated and audited (by the
/// mutating-request middleware). The scheduler re-reads this each round, so a change applies on the
/// next poll round without a restart. 503 in skeleton mode (no metadata store).
#[derive(Deserialize)]
struct ConfigBody {
    default_poll_interval_secs: u32,
}

async fn update_config(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Json(body): Json<ConfigBody>,
) -> Response {
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    if let Some(resp) = authorize(&st, &headers, Permission::ManageConfig) {
        return resp;
    }
    let secs = body.default_poll_interval_secs;
    if !crate::config::interval_in_bounds(secs) {
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_poll_interval",
            format!(
                "default poll interval must be {}-{} seconds",
                crate::config::MIN_POLL_INTERVAL_SECS,
                crate::config::MAX_POLL_INTERVAL_SECS
            ),
        );
    }
    match admin.repo.set_default_poll_interval(secs).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => {
            tracing::error!(error = %e, "update config failed");
            internal("failed to update config")
        }
    }
}

fn now_unix_s() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

// ── Response shapes ──────────────────────────────────────────────────────────

/// Latest reading for one node metric.
#[derive(Serialize)]
struct MetricReading {
    node_id: NodeId,
    metric: String,
    value: f64,
}

/// A time-series window for one node metric.
#[derive(Serialize)]
struct MetricRange {
    node_id: NodeId,
    metric: String,
    points: Vec<MetricPoint>,
}

/// One inventory row (mirrors the WebUI `NodeSummary`).
#[derive(Serialize)]
struct NodeSummary {
    id: NodeId,
    name: String,
    address: String,
    state: NodeState,
    /// Descriptive maker/model for the "name (addr) (vendor) (model)" display.
    vendor: Option<String>,
    model: Option<String>,
    /// The group this node belongs to (for the inventory tree); `null` ⇒ ungrouped.
    group_id: Option<Uuid>,
    /// Manual order within the group (the tree sorts members by this, then by name).
    sort_order: f64,
}

/// The fixed error envelope (ADR-019).
#[derive(Serialize)]
struct ErrorBody {
    error: ErrorDetail,
}

#[derive(Serialize)]
struct ErrorDetail {
    code: String,
    message: String,
}

fn error_response(status: StatusCode, code: &str, message: String) -> Response {
    (
        status,
        Json(ErrorBody {
            error: ErrorDetail {
                code: code.to_owned(),
                message,
            },
        }),
    )
        .into_response()
}

fn not_found(code: &str, message: String) -> Response {
    error_response(StatusCode::NOT_FOUND, code, message)
}

/// A Prometheus-style metric name: `[a-zA-Z_:][a-zA-Z0-9_:]*`. Validating at the edge
/// keeps the (untrusted) path segment from being interpolated into the PromQL selector
/// sent to the TSDB (security.md: parse into strong, bounded types at the API edge).
fn is_valid_metric_name(metric: &str) -> bool {
    let mut chars = metric.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' || c == ':' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == ':')
}

// ── Handlers ─────────────────────────────────────────────────────────────────

/// Keyset pagination query for the node list.
#[derive(Deserialize)]
struct NodePageQuery {
    cursor: Option<Uuid>,
    limit: Option<i64>,
}

async fn list_nodes(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Query(q): Query<NodePageQuery>,
) -> Response {
    if let Some(resp) = require_view(&st, &headers) {
        return resp;
    }
    let limit = q.limit.unwrap_or(100).clamp(1, 500);
    // Fetch one extra row to tell "exactly a full page" from "a full page with more after it",
    // so the client never makes a trailing request that returns an empty page at the boundary.
    match st.nodes.list_page(q.cursor, limit + 1).await {
        Ok(mut nodes) => {
            let has_more = nodes.len() as i64 > limit;
            if has_more {
                nodes.truncate(limit as usize);
            }
            let next_cursor = if has_more {
                nodes.last().map(|n| n.id.to_string())
            } else {
                None
            };
            // Display state comes from the live alert engine (committed liveness rolled up
            // with any active threshold alert). Nodes the engine hasn't observed yet fall
            // back to a coarse store probe (a recent RTT ⇒ ok, else unknown).
            let states = st.alerts.node_states();
            // Per-node tree order (admin/live only; skeleton mode has no order → 0 = name order).
            let orders = match st.admin.as_ref() {
                Some(admin) => {
                    let ids: Vec<Uuid> = nodes.iter().map(|n| n.id.as_uuid()).collect();
                    admin.repo.node_sort_orders(&ids).await.unwrap_or_default()
                }
                None => std::collections::HashMap::new(),
            };
            let mut out = Vec::with_capacity(nodes.len());
            for n in nodes {
                let state = match states.get(&n.id) {
                    Some(s) => *s,
                    None => derive_fallback_state(&st, n.id).await,
                };
                let sort_order = orders.get(&n.id.as_uuid()).copied().unwrap_or(0.0);
                out.push(NodeSummary {
                    id: n.id,
                    name: n.name,
                    address: n.address.to_string(),
                    state,
                    vendor: n.vendor,
                    model: n.model,
                    group_id: n.group.map(|g| g.as_uuid()),
                    sort_order,
                });
            }
            Json(serde_json::json!({ "nodes": out, "next_cursor": next_cursor })).into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to list nodes");
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                "failed to list nodes".to_owned(),
            )
        }
    }
}

/// Coarse fallback state for a node the alert engine has not observed yet: a recent ICMP
/// RTT reading ⇒ `ok`, otherwise `unknown`. Used only when the engine has no opinion (e.g.
/// just-added node, or skeleton mode where nothing polls).
async fn derive_fallback_state(st: &ApiState, node: NodeId) -> NodeState {
    if st
        .store
        .latest(&SeriesKey::node(node, "icmp_rtt_ms"))
        .await
        .is_some()
    {
        NodeState::Ok
    } else {
        NodeState::Unknown
    }
}

/// One node's configuration detail, including its bindings (profile/credential/parent) so
/// the node-detail page can show and edit them. Live mode only (PostgreSQL inventory).
#[derive(Serialize)]
struct NodeDetail {
    id: NodeId,
    name: String,
    address: String,
    profile_id: Option<Uuid>,
    credential_id: Option<Uuid>,
    parent_id: Option<Uuid>,
    /// Descriptive maker/model, editable from the node detail.
    vendor: Option<String>,
    model: Option<String>,
    /// The group this node belongs to; `null` ⇒ ungrouped.
    group_id: Option<Uuid>,
    /// URL-monitor config when this node is a URL monitor; `null` otherwise.
    url_check: Option<UrlCheckConfig>,
}

async fn get_node(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Path(node_id): Path<Uuid>,
) -> Response {
    if let Some(resp) = require_view(&st, &headers) {
        return resp;
    }
    let Some(admin) = st.admin.as_ref() else {
        return not_found("node_not_found", format!("no node {node_id}"));
    };
    match admin.repo.get_node(node_id).await {
        Ok(Some(node)) => {
            // Best-effort: a URL-check load failure shouldn't fail the whole node detail.
            let url_check = admin.url_checks.get(node_id).await.unwrap_or(None);
            Json(NodeDetail {
                id: node.id,
                name: node.name,
                address: node.address.to_string(),
                profile_id: node.profile.map(|p| p.0),
                credential_id: node.credential.map(|c| c.as_uuid()),
                parent_id: node.parent.map(|p| p.as_uuid()),
                vendor: node.vendor,
                model: node.model,
                group_id: node.group.map(|g| g.as_uuid()),
                url_check,
            })
            .into_response()
        }
        Ok(None) => not_found("node_not_found", format!("no node {node_id}")),
        Err(e) => {
            tracing::error!(error = %e, "get node failed");
            internal("failed to load node")
        }
    }
}

/// One node's live status: its rolled-up display state plus the alerts currently attributed
/// to it (so node detail can show *why* it's down without re-deriving from the list).
async fn get_node_status(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Path(node_id): Path<Uuid>,
) -> Response {
    if let Some(resp) = require_view(&st, &headers) {
        return resp;
    }
    let node = NodeId::from(node_id);
    let state = match st.alerts.node_state(node) {
        Some(s) => s,
        None => derive_fallback_state(&st, node).await,
    };
    let alerts = st.alerts.alerts_for(node);
    Json(serde_json::json!({ "node_id": node, "state": state, "alerts": alerts })).into_response()
}

/// Optional aggregation for the metric reads. `agg=max` collapses a per-entity table gauge
/// (e.g. CPU% per `entPhysicalIndex`) into one node-level value; absent ⇒ scalar node series.
#[derive(Deserialize)]
struct MetricQuery {
    agg: Option<String>,
}

/// Reject an `agg` value we don't support (validate at the edge — security.md).
fn invalid_agg(other: &str) -> Response {
    error_response(
        StatusCode::BAD_REQUEST,
        "invalid_agg",
        format!("unsupported agg {other:?}; expected 'max'"),
    )
}

async fn get_node_metric(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Path((node_id, metric)): Path<(Uuid, String)>,
    Query(q): Query<MetricQuery>,
) -> Response {
    if let Some(resp) = require_view(&st, &headers) {
        return resp;
    }
    if !is_valid_metric_name(&metric) {
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_metric_name",
            format!("metric name {metric:?} is not a valid identifier"),
        );
    }
    let node = NodeId::from(node_id);
    let key = SeriesKey::node(node, metric.as_str());
    let value = match q.agg.as_deref() {
        Some("max") => st.store.aggregate_latest(&key).await,
        Some(other) => return invalid_agg(other),
        None => st.store.latest(&key).await,
    };
    match value {
        Some(value) => Json(MetricReading {
            node_id: node,
            metric,
            value,
        })
        .into_response(),
        None => not_found(
            "metric_not_found",
            format!("no reading for metric '{metric}' on node {node_id}"),
        ),
    }
}

/// Query params for the range endpoint (all optional; sensible defaults applied).
#[derive(Deserialize)]
struct RangeQuery {
    from: Option<i64>,
    to: Option<i64>,
    step: Option<u64>,
    /// `max` ⇒ node-level aggregate of a per-entity table gauge; absent ⇒ scalar node series.
    agg: Option<String>,
}

async fn get_node_metric_range(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Path((node_id, metric)): Path<(Uuid, String)>,
    Query(q): Query<RangeQuery>,
) -> Response {
    if let Some(resp) = require_view(&st, &headers) {
        return resp;
    }
    if !is_valid_metric_name(&metric) {
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_metric_name",
            format!("metric name {metric:?} is not a valid identifier"),
        );
    }
    let node = NodeId::from(node_id);
    let to = q.to.unwrap_or_else(now_unix_s);
    let from = q.from.unwrap_or(to - DEFAULT_RANGE_SECS);
    let step = q.step.unwrap_or(DEFAULT_STEP_SECS).max(1);
    let key = SeriesKey::node(node, metric.as_str());
    let points = match q.agg.as_deref() {
        Some("max") => st.store.aggregate_range(&key, from, to, step).await,
        Some(other) => return invalid_agg(other),
        None => st.store.range(&key, from, to, step).await,
    };
    Json(MetricRange {
        node_id: node,
        metric,
        points,
    })
    .into_response()
}

/// Query for the fleet Top-N endpoint (`GET /api/v1/metrics/top`).
#[derive(Deserialize)]
struct TopQuery {
    /// Metric to rank by (validated identifier).
    metric: String,
    /// `now` (default) ⇒ most recent value; `max_1h` ⇒ trailing-hour peak.
    agg: Option<String>,
    /// How many nodes to return (default 5, clamped 1..=50).
    limit: Option<usize>,
}

/// One ranked node in a Top-N result.
#[derive(Serialize)]
struct TopEntry {
    node_id: Uuid,
    /// Display name, joined from PostgreSQL (TSDB carries only the id, ADR-011); falls back to
    /// the id string if the node has since been deleted.
    name: String,
    value: f64,
}

/// Logical node-metric aliases for the fleet Top-N: a friendly name → the set of per-vendor
/// metric names ranked together via a `__name__` regex (one query collapses them with
/// `max by (node)`). Only "busy-style" gauges where higher = worse are included — idle/temperature
/// metrics are excluded. Memory uses the vendors that expose a direct % (bytes-derived % for
/// Cisco/UCD is a later recording-rule job). The selector is built from these constants only, so
/// it is safe to interpolate (no user input reaches the PromQL).
fn logical_metric_selector(alias: &str) -> Option<String> {
    let names: &[&str] = match alias {
        "cpu" => &[
            "huawei_cpu_usage",
            "cisco_cpu_5min",
            "nxos_cpu_util",
            "fortinet_cpu_usage",
            "juniper_cpu_1min",
            "hr_processor_load",
        ],
        "memory" => &["huawei_mem_usage", "nxos_mem_util", "fortinet_mem_usage"],
        _ => return None,
    };
    Some(format!("{{__name__=~\"{}\"}}", names.join("|")))
}

/// Parse the shared `agg` query param (`now` default | `max_1h`) into a [`TopAgg`]. The error is
/// boxed so the `Ok` path stays cheap (`clippy::result_large_err`).
fn parse_top_agg(agg: Option<&str>) -> Result<TopAgg, Box<Response>> {
    match agg {
        None | Some("now") => Ok(TopAgg::Now),
        Some("max_1h") => Ok(TopAgg::Max1h),
        Some(other) => Err(Box::new(error_response(
            StatusCode::BAD_REQUEST,
            "invalid_agg",
            format!("agg must be 'now' or 'max_1h', got {other:?}"),
        ))),
    }
}

/// Fleet-wide Top-N for a metric: the highest-value nodes right now (or by hourly peak).
/// Powers the dashboard "Top RTT / CPU / memory / …" widgets from one endpoint. `metric` is
/// either a raw collected metric name (e.g. `icmp_rtt_ms`) or a logical alias (`cpu`, `memory`).
async fn top_metrics(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Query(q): Query<TopQuery>,
) -> Response {
    if let Some(resp) = require_view(&st, &headers) {
        return resp;
    }
    // A logical alias expands to a constant `{__name__=~…}` selector; otherwise the metric must be
    // a valid identifier (it's interpolated into the PromQL selector).
    let selector = match logical_metric_selector(&q.metric) {
        Some(sel) => sel,
        None => {
            if !is_valid_metric_name(&q.metric) {
                return error_response(
                    StatusCode::BAD_REQUEST,
                    "invalid_metric_name",
                    format!("metric name {:?} is not a valid identifier", q.metric),
                );
            }
            q.metric.clone()
        }
    };
    let agg = match parse_top_agg(q.agg.as_deref()) {
        Ok(a) => a,
        Err(resp) => return *resp,
    };
    let limit = q.limit.unwrap_or(5).clamp(1, 50);
    let ranked = st.store.top_nodes(&selector, agg, limit).await;
    // Join node id → name (TSDB labels carry only the id, ADR-011). Best-effort: in skeleton
    // mode (no repo) or for a since-deleted node the row keeps the id string as its name.
    let ids: Vec<Uuid> = ranked.iter().map(|(id, _)| *id).collect();
    let names = match st.admin.as_ref() {
        Some(admin) => admin.repo.node_names(&ids).await.unwrap_or_default(),
        None => std::collections::HashMap::new(),
    };
    let out: Vec<TopEntry> = ranked
        .into_iter()
        .map(|(id, value)| TopEntry {
            node_id: id,
            name: names.get(&id).cloned().unwrap_or_else(|| id.to_string()),
            value,
        })
        .collect();
    Json(out).into_response()
}

/// Query for the fleet interface Top-N endpoint.
#[derive(Deserialize)]
struct InterfaceTopQuery {
    /// `throughput` | `in_bps` | `out_bps` | `errors` | `discards`.
    metric: String,
    agg: Option<String>,
    limit: Option<usize>,
}

/// One ranked interface in a fleet interface Top-N.
#[derive(Serialize)]
struct InterfaceTopEntry {
    node_id: Uuid,
    node_name: String,
    ifindex: i32,
    if_name: Option<String>,
    if_alias: Option<String>,
    /// Configured speed (bits/sec) for util%; `null` if unknown.
    if_speed_bps: Option<i64>,
    /// bits/sec for throughput metrics, errors|discards per second otherwise.
    value: f64,
}

/// Fleet-wide busiest/erroring interfaces. Ranks `(node,ifindex)` by a query-time rate, then
/// joins node + interface names (and speed) from PostgreSQL (TSDB carries only ids, ADR-011).
async fn interface_top(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Query(q): Query<InterfaceTopQuery>,
) -> Response {
    if let Some(resp) = require_view(&st, &headers) {
        return resp;
    }
    let metric = match q.metric.as_str() {
        "throughput" => InterfaceTopMetric::Throughput,
        "in_bps" => InterfaceTopMetric::InBps,
        "out_bps" => InterfaceTopMetric::OutBps,
        "errors" => InterfaceTopMetric::Errors,
        "discards" => InterfaceTopMetric::Discards,
        other => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "invalid_metric",
                format!("metric must be throughput|in_bps|out_bps|errors|discards, got {other:?}"),
            )
        }
    };
    let agg = match parse_top_agg(q.agg.as_deref()) {
        Ok(a) => a,
        Err(resp) => return *resp,
    };
    let limit = q.limit.unwrap_or(6).clamp(1, 50);
    let ranked = st.store.top_interfaces(metric, agg, limit).await;
    Json(build_interface_entries(&st, ranked).await).into_response()
}

/// Join a fleet interface ranking `(node, ifindex, value)` to node + interface names (and speed)
/// from PostgreSQL — one repo query over the distinct nodes in the result. Shared by the
/// interface Top-N and interface-delta endpoints.
async fn build_interface_entries(
    st: &ApiState,
    ranked: Vec<(Uuid, i32, f64)>,
) -> Vec<InterfaceTopEntry> {
    let node_ids: Vec<Uuid> = {
        let mut ids: Vec<Uuid> = ranked.iter().map(|(n, _, _)| *n).collect();
        ids.sort_unstable();
        ids.dedup();
        ids
    };
    let (names, idents) = match st.admin.as_ref() {
        Some(admin) => (
            admin.repo.node_names(&node_ids).await.unwrap_or_default(),
            admin
                .repo
                .interface_idents_for(&node_ids)
                .await
                .unwrap_or_default(),
        ),
        None => (
            std::collections::HashMap::new(),
            std::collections::HashMap::new(),
        ),
    };
    ranked
        .into_iter()
        .map(|(node_id, ifindex, value)| {
            let ident = idents.get(&(node_id, ifindex));
            InterfaceTopEntry {
                node_id,
                node_name: names
                    .get(&node_id)
                    .cloned()
                    .unwrap_or_else(|| node_id.to_string()),
                ifindex,
                if_name: ident.and_then(|i| i.if_name.clone()),
                if_alias: ident.and_then(|i| i.if_alias.clone()),
                if_speed_bps: ident.and_then(|i| i.if_speed),
                value,
            }
        })
        .collect()
}

/// Query for the interface rate-delta endpoint (traffic spikes/drops).
#[derive(Deserialize)]
struct InterfaceDeltaQuery {
    /// `up` (spikes) | `down` (drops).
    direction: String,
    /// Comparison window in seconds (default 300 = now vs 5m ago).
    window: Option<u64>,
    limit: Option<usize>,
}

/// Interfaces whose total throughput moved the most vs `window` ago — spikes (`up`) or drops
/// (`down`). `value` is the signed delta in bits/sec.
async fn interface_delta(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Query(q): Query<InterfaceDeltaQuery>,
) -> Response {
    if let Some(resp) = require_view(&st, &headers) {
        return resp;
    }
    let direction = match q.direction.as_str() {
        "up" => DeltaDirection::Up,
        "down" => DeltaDirection::Down,
        other => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "invalid_direction",
                format!("direction must be 'up' or 'down', got {other:?}"),
            )
        }
    };
    let window = q.window.unwrap_or(300).clamp(60, 3600);
    let limit = q.limit.unwrap_or(6).clamp(1, 50);
    let ranked = st.store.interface_delta(direction, window, limit).await;
    Json(build_interface_entries(&st, ranked).await).into_response()
}

/// One node in the dependency/topology graph.
#[derive(Serialize)]
struct TopologyNode {
    id: Uuid,
    name: String,
    /// Upstream parent in the dependency graph (`null` ⇒ a root).
    parent_id: Option<Uuid>,
    state: NodeState,
    /// Upstream node currently identified as the root cause of this node's alert (dependency
    /// suppression), if any — lets the UI collapse downstream alerts under the cause.
    root_cause: Option<Uuid>,
}

/// The dependency graph: every node with its parent edge, current state, and any active
/// root-cause attribution. Assembled from the inventory (parent links) + the live alert engine
/// (state + root_cause) — no new model. Admin-only data source (full node list).
async fn get_topology(State(st): State<ApiState>, headers: HeaderMap) -> Response {
    if let Some(resp) = require_view(&st, &headers) {
        return resp;
    }
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    let nodes = match admin.repo.list_nodes().await {
        Ok(n) => n,
        Err(e) => {
            tracing::error!(error = %e, "topology list nodes failed");
            return internal("failed to load topology");
        }
    };
    let states = st.alerts.node_states();
    // node → upstream root cause (from active, suppressed alerts).
    let mut root_causes: std::collections::HashMap<NodeId, Uuid> = std::collections::HashMap::new();
    for a in st.alerts.active_alerts() {
        if let Some(cause) = a.root_cause {
            root_causes.entry(a.node).or_insert_with(|| cause.as_uuid());
        }
    }
    let mut out = Vec::with_capacity(nodes.len());
    for n in nodes {
        let state = match states.get(&n.id) {
            Some(s) => *s,
            None => derive_fallback_state(&st, n.id).await,
        };
        out.push(TopologyNode {
            id: n.id.as_uuid(),
            name: n.name,
            parent_id: n.parent.map(|p| p.as_uuid()),
            state,
            root_cause: root_causes.get(&n.id).copied(),
        });
    }
    Json(serde_json::json!({ "nodes": out })).into_response()
}

/// A node returning no fresh data (silent failure / blind spot).
#[derive(Serialize)]
struct StaleNode {
    node_id: Uuid,
    name: String,
}

/// Fleet data-coverage summary: fresh vs total nodes + the stale watchlist.
#[derive(Serialize)]
struct FleetCoverage {
    total: usize,
    fresh: usize,
    /// Percent of nodes reporting fresh data (100 when the inventory is empty).
    coverage_pct: i64,
    stale: Vec<StaleNode>,
}

/// How recent a node's last ICMP sample must be to count as "fresh" (silent beyond this ⇒ stale).
const COVERAGE_FRESH_SECS: u64 = 600;

/// Fleet data coverage + the stale-data watchlist: which nodes have (not) reported ICMP within
/// the freshness window. A blind-spot detector — low coverage means the monitoring itself is
/// missing data. Admin-only data source (full inventory).
async fn fleet_coverage(State(st): State<ApiState>, headers: HeaderMap) -> Response {
    if let Some(resp) = require_view(&st, &headers) {
        return resp;
    }
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    let nodes = match admin.repo.list_nodes().await {
        Ok(n) => n,
        Err(e) => {
            tracing::error!(error = %e, "fleet coverage list nodes failed");
            return internal("failed to load fleet coverage");
        }
    };
    let fresh_ids: std::collections::HashSet<Uuid> = st
        .store
        .fresh_node_ids("icmp_rtt_ms", COVERAGE_FRESH_SECS)
        .await
        .into_iter()
        .collect();
    let total = nodes.len();
    let mut fresh = 0usize;
    let mut stale: Vec<StaleNode> = Vec::new();
    for n in nodes {
        if fresh_ids.contains(&n.id.as_uuid()) {
            fresh += 1;
        } else {
            stale.push(StaleNode {
                node_id: n.id.as_uuid(),
                name: n.name,
            });
        }
    }
    let coverage_pct = if total > 0 {
        ((fresh as f64 / total as f64) * 100.0).round() as i64
    } else {
        100
    };
    stale.sort_by(|a, b| a.name.cmp(&b.name));
    stale.truncate(50);
    Json(FleetCoverage {
        total,
        fresh,
        coverage_pct,
        stale,
    })
    .into_response()
}

/// Query for the fleet state-history timeline: `?from=&to=` Unix seconds (default last 24h).
#[derive(Deserialize)]
struct StateHistoryQuery {
    from: Option<i64>,
    to: Option<i64>,
}

/// The node-state counts over time, pivoted into per-state series aligned to a shared timestamp
/// axis (for the "fleet health timeline" stacked/line chart). Admin-only data source.
async fn fleet_state_history(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Query(q): Query<StateHistoryQuery>,
) -> Response {
    if let Some(resp) = require_view(&st, &headers) {
        return resp;
    }
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    let to = q.to.unwrap_or_else(now_unix_s);
    // Default window: the last 24h of snapshots.
    let from = q.from.unwrap_or(to - 24 * 3600);
    // Bound the requested window so a client can't ask for an unboundedly large scan (defense in
    // depth on top of snapshot retention): cap at 90 days and require from <= to.
    const MAX_HISTORY_SECS: i64 = 90 * 24 * 3600;
    if to < from || to - from > MAX_HISTORY_SECS {
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_range",
            "from must be <= to and the window must not exceed 90 days".to_owned(),
        );
    }
    let rows = match admin.repo.state_history(from, to).await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(error = %e, "fleet state history failed");
            return internal("failed to load state history");
        }
    };
    // Pivot (ts, state, count) rows into { timestamps, series: { state: [aligned counts] } }.
    let mut timestamps: Vec<i64> = Vec::new();
    let mut ts_index: std::collections::HashMap<i64, usize> = std::collections::HashMap::new();
    for (t, _, _) in &rows {
        if !ts_index.contains_key(t) {
            ts_index.insert(*t, timestamps.len());
            timestamps.push(*t);
        }
    }
    // Fixed state set so the series keys are stable for the client.
    const STATES: [&str; 6] = [
        "ok",
        "warning",
        "critical",
        "unreachable",
        "unknown",
        "maintenance",
    ];
    let mut series: std::collections::BTreeMap<String, Vec<i64>> = STATES
        .iter()
        .map(|s| ((*s).to_owned(), vec![0i64; timestamps.len()]))
        .collect();
    for (t, state, count) in rows {
        if let (Some(&i), Some(arr)) = (ts_index.get(&t), series.get_mut(&state)) {
            arr[i] = count;
        }
    }
    Json(serde_json::json!({ "timestamps": timestamps, "series": series })).into_response()
}

/// Query for the aggregate-throughput range: `?from=&to=&step=` (default last 24h, 300s step).
#[derive(Deserialize)]
struct ThroughputRangeQuery {
    from: Option<i64>,
    to: Option<i64>,
    step: Option<u64>,
}

/// Fleet aggregate ingress/egress (bits/sec) over time, aligned to one timestamp axis. For the
/// "aggregate throughput" 2-series chart.
async fn throughput_range(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Query(q): Query<ThroughputRangeQuery>,
) -> Response {
    if let Some(resp) = require_view(&st, &headers) {
        return resp;
    }
    let to = q.to.unwrap_or_else(now_unix_s);
    let from = q.from.unwrap_or(to - 24 * 3600);
    let step = q.step.unwrap_or(300).max(60);
    let (in_pts, out_pts) = st.store.throughput_range(from, to, step).await;
    // Align in/out onto one sorted timestamp axis (null where a side has no point).
    let mut grid: std::collections::BTreeMap<i64, (Option<f64>, Option<f64>)> =
        std::collections::BTreeMap::new();
    for p in in_pts {
        grid.entry(p.t).or_default().0 = Some(p.v);
    }
    for p in out_pts {
        grid.entry(p.t).or_default().1 = Some(p.v);
    }
    let timestamps: Vec<i64> = grid.keys().copied().collect();
    let in_bps: Vec<Option<f64>> = grid.values().map(|(i, _)| *i).collect();
    let out_bps: Vec<Option<f64>> = grid.values().map(|(_, o)| *o).collect();
    Json(serde_json::json!({ "timestamps": timestamps, "in_bps": in_bps, "out_bps": out_bps }))
        .into_response()
}

/// Query for the interface throughput heatmap: `?limit=&from=&to=&step=`.
#[derive(Deserialize)]
struct HeatmapQuery {
    limit: Option<usize>,
    from: Option<i64>,
    to: Option<i64>,
    step: Option<u64>,
}

/// Busiest-links × time heatmap: picks the top interfaces by current throughput, then returns
/// each link's throughput (bits/sec) over time on a shared timestamp axis. Cells are intensity-
/// shaded client-side.
async fn interface_heatmap(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Query(q): Query<HeatmapQuery>,
) -> Response {
    if let Some(resp) = require_view(&st, &headers) {
        return resp;
    }
    let limit = q.limit.unwrap_or(8).clamp(1, 20);
    let to = q.to.unwrap_or_else(now_unix_s);
    let from = q.from.unwrap_or(to - 6 * 3600);
    let step = q.step.unwrap_or(600).max(60);
    // Pick the busiest links now, then fetch each one's throughput series.
    let top = st
        .store
        .top_interfaces(InterfaceTopMetric::Throughput, TopAgg::Now, limit)
        .await;
    let entries = build_interface_entries(&st, top).await;
    let mut union: std::collections::BTreeSet<i64> = std::collections::BTreeSet::new();
    let mut per_link: Vec<(String, std::collections::HashMap<i64, f64>)> = Vec::new();
    for e in &entries {
        let pts = st
            .store
            .interface_throughput_range(e.node_id, e.ifindex, from, to, step)
            .await;
        let mut m = std::collections::HashMap::new();
        for p in pts {
            union.insert(p.t);
            m.insert(p.t, p.v);
        }
        let iface = e
            .if_name
            .clone()
            .or_else(|| e.if_alias.clone())
            .unwrap_or_else(|| format!("if{}", e.ifindex));
        per_link.push((format!("{} · {}", e.node_name, iface), m));
    }
    let timestamps: Vec<i64> = union.into_iter().collect();
    let links: Vec<String> = per_link.iter().map(|(l, _)| l.clone()).collect();
    let values: Vec<Vec<f64>> = per_link
        .iter()
        .map(|(_, m)| {
            timestamps
                .iter()
                .map(|t| m.get(t).copied().unwrap_or(0.0))
                .collect()
        })
        .collect();
    Json(serde_json::json!({ "links": links, "timestamps": timestamps, "values": values }))
        .into_response()
}

/// Query for the standing discovery-candidates view.
#[derive(Deserialize)]
struct CandidatesQuery {
    limit: Option<usize>,
}

/// Recent discovered (unclassified) devices across in-memory scans — the dashboard "discovery
/// queue". Read-only; empty in skeleton mode (no discovery runner).
async fn discovery_candidates(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Query(q): Query<CandidatesQuery>,
) -> Response {
    if let Some(resp) = require_view(&st, &headers) {
        return resp;
    }
    let Some(admin) = st.admin.as_ref() else {
        return Json(Vec::<serde_json::Value>::new()).into_response();
    };
    let limit = q.limit.unwrap_or(10).clamp(1, 50);
    Json(admin.discovery.recent_candidates(limit)).into_response()
}

/// Poll-loop self-monitoring: last sweep time, jobs dispatched last round, total results consumed.
/// The "stat strip" of the poller & collection-health widget. Per-poller rows need poller identity
/// on the bus and are a later addition. Admin-only (stats live with the live write side).
async fn poller_health(State(st): State<ApiState>, headers: HeaderMap) -> Response {
    if let Some(resp) = require_view(&st, &headers) {
        return resp;
    }
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    Json(admin.scheduler_stats.snapshot()).into_response()
}

/// Currently active alerts (from the in-memory alert engine).
async fn list_alerts(State(st): State<ApiState>, headers: HeaderMap) -> Response {
    if let Some(resp) = require_view(&st, &headers) {
        return resp;
    }
    Json(st.alerts.active_alerts()).into_response()
}

/// Recent alert-history rows. Query: `?limit=` (default 100). Empty in skeleton mode.
#[derive(Deserialize)]
struct HistoryQuery {
    limit: Option<i64>,
}

async fn list_alert_history(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Query(q): Query<HistoryQuery>,
) -> Response {
    if let Some(resp) = require_view(&st, &headers) {
        return resp;
    }
    let Some(history) = st.history.as_ref() else {
        return Json(Vec::<serde_json::Value>::new()).into_response();
    };
    match history.recent(q.limit.unwrap_or(100)).await {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "list alert history failed");
            internal("failed to list alert history")
        }
    }
}

/// Query for the alert top-nodes aggregation: `?window=<secs>` (default 24h) + `?limit=`.
#[derive(Deserialize)]
struct AlertTopQuery {
    window: Option<i64>,
    limit: Option<i64>,
}

/// One chronic-offender row.
#[derive(Serialize)]
struct AlertNodeCount {
    node_id: Uuid,
    name: String,
    count: i64,
}

/// Nodes generating the most alert fires over a trailing window (chronic offenders). Empty in
/// skeleton mode (no history store).
async fn alert_top_nodes(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Query(q): Query<AlertTopQuery>,
) -> Response {
    if let Some(resp) = require_view(&st, &headers) {
        return resp;
    }
    let Some(history) = st.history.as_ref() else {
        return Json(Vec::<AlertNodeCount>::new()).into_response();
    };
    let window = q.window.unwrap_or(86_400).clamp(60, 30 * 86_400);
    let since_ms = (now_unix_s() - window) * 1000;
    let limit = q.limit.unwrap_or(6).clamp(1, 50);
    let counts = match history.top_nodes_by_fires(since_ms, limit).await {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(error = %e, "alert top-nodes failed");
            return internal("failed to aggregate alerting nodes");
        }
    };
    let ids: Vec<Uuid> = counts.iter().map(|(n, _)| *n).collect();
    let names = match st.admin.as_ref() {
        Some(admin) => admin.repo.node_names(&ids).await.unwrap_or_default(),
        None => std::collections::HashMap::new(),
    };
    let out: Vec<AlertNodeCount> = counts
        .into_iter()
        .map(|(node_id, count)| AlertNodeCount {
            node_id,
            name: names
                .get(&node_id)
                .cloned()
                .unwrap_or_else(|| node_id.to_string()),
            count,
        })
        .collect();
    Json(out).into_response()
}

/// Query for the alert calendar heatmap: `?days=<n>` (default 7) of history.
#[derive(Deserialize)]
struct AlertCalendarQuery {
    days: Option<i64>,
}

/// One weekday×hour heatmap cell.
#[derive(Serialize)]
struct CalendarBucket {
    /// 0 = Sunday … 6 = Saturday (UTC).
    dow: i32,
    /// Hour of day 0–23 (UTC).
    hour: i32,
    count: i64,
}

/// Alert fires bucketed weekday×hour over the last `days` (for the calendar heatmap). Empty in
/// skeleton mode.
async fn alert_calendar(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Query(q): Query<AlertCalendarQuery>,
) -> Response {
    if let Some(resp) = require_view(&st, &headers) {
        return resp;
    }
    let Some(history) = st.history.as_ref() else {
        return Json(Vec::<CalendarBucket>::new()).into_response();
    };
    let days = q.days.unwrap_or(7).clamp(1, 90);
    let since_ms = (now_unix_s() - days * 86_400) * 1000;
    match history.fires_by_weekday_hour(since_ms).await {
        Ok(buckets) => {
            let out: Vec<CalendarBucket> = buckets
                .into_iter()
                .map(|(dow, hour, count)| CalendarBucket { dow, hour, count })
                .collect();
            Json(out).into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "alert calendar failed");
            internal("failed to build the alert calendar")
        }
    }
}

/// One recent state-change row (a fire = into an alert state; a resolve = recovery to ok).
#[derive(Serialize)]
struct AlertTransition {
    node_id: Uuid,
    name: String,
    state: String,
    severity: String,
    /// true = recovery (→ ok); false = went into the alert state.
    resolved: bool,
    at_unix_ms: i64,
}

/// Recent up/down transitions (latest fires and resolutions), node names joined. Empty in
/// skeleton mode.
async fn alert_transitions(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Query(q): Query<HistoryQuery>,
) -> Response {
    if let Some(resp) = require_view(&st, &headers) {
        return resp;
    }
    let Some(history) = st.history.as_ref() else {
        return Json(Vec::<AlertTransition>::new()).into_response();
    };
    let rows = match history.recent(q.limit.unwrap_or(12)).await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(error = %e, "alert transitions failed");
            return internal("failed to list alert transitions");
        }
    };
    let ids: Vec<Uuid> = rows.iter().map(|r| r.node).collect();
    let names = match st.admin.as_ref() {
        Some(admin) => admin.repo.node_names(&ids).await.unwrap_or_default(),
        None => std::collections::HashMap::new(),
    };
    let out: Vec<AlertTransition> = rows
        .into_iter()
        .map(|r| AlertTransition {
            node_id: r.node,
            name: names
                .get(&r.node)
                .cloned()
                .unwrap_or_else(|| r.node.to_string()),
            state: r.state,
            severity: r.severity,
            resolved: r.resolved,
            at_unix_ms: r.at_unix_ms,
        })
        .collect();
    Json(out).into_response()
}

/// Live alert stream (SSE, ADR-019): fires and resolutions as they happen. Each event's
/// `data` is the alert JSON with a `resolved` flag. Keep-alive holds the connection open
/// when idle.
async fn stream_alerts(State(st): State<ApiState>, headers: HeaderMap) -> Response {
    if let Some(resp) = require_view(&st, &headers) {
        return resp;
    }
    let stream = tokio_stream::wrappers::BroadcastStream::new(st.alerts.subscribe()).filter_map(
        |r| async move {
            match r {
                Ok(json) => Some(Ok::<_, Infallible>(Event::default().data(json))),
                // The subscriber fell behind the buffer and missed `n` events (only this slow
                // receiver is affected — other clients keep their own cursor). Log it and emit a
                // named `resync` event so the client can re-fetch the active-alert list and close
                // the gap. Browsers ignore named events they don't listen for, so this is
                // backward-compatible with clients that only read the default message stream.
                Err(tokio_stream::wrappers::errors::BroadcastStreamRecvError::Lagged(n)) => {
                    tracing::warn!(
                        missed = n,
                        "SSE alert subscriber lagged; emitting resync hint"
                    );
                    Some(Ok(Event::default().event("resync").data(n.to_string())))
                }
            }
        },
    );
    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}

// ── Troubleshoot analysis jobs (ADR-022) ─────────────────────────────────────

/// Recent analysis jobs (the runs list). `?limit=` (default 50). Empty in skeleton mode.
async fn list_analysis_jobs(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Query(q): Query<HistoryQuery>,
) -> Response {
    if let Some(resp) = require_view(&st, &headers) {
        return resp;
    }
    let Some(admin) = st.admin.as_ref() else {
        return Json(Vec::<serde_json::Value>::new()).into_response();
    };
    let limit = q.limit.unwrap_or(50).clamp(1, 200);
    match admin.analysis.list(limit).await {
        Ok(jobs) => Json(jobs).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "list analysis jobs failed");
            internal("failed to list analysis jobs")
        }
    }
}

/// One analysis job by id.
async fn get_analysis_job(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Response {
    if let Some(resp) = require_view(&st, &headers) {
        return resp;
    }
    let Some(admin) = st.admin.as_ref() else {
        return not_found("job_not_found", format!("no analysis job {id}"));
    };
    match admin.analysis.get(id).await {
        Ok(Some(job)) => Json(job).into_response(),
        Ok(None) => not_found("job_not_found", format!("no analysis job {id}")),
        Err(e) => {
            tracing::error!(error = %e, "get analysis job failed");
            internal("failed to load analysis job")
        }
    }
}

/// A job's findings (the report list). Empty in skeleton mode.
async fn analysis_findings(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Response {
    if let Some(resp) = require_view(&st, &headers) {
        return resp;
    }
    let Some(admin) = st.admin.as_ref() else {
        return Json(Vec::<serde_json::Value>::new()).into_response();
    };
    match admin.analysis.findings(id).await {
        Ok(findings) => Json(findings).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "list analysis findings failed");
            internal("failed to load findings")
        }
    }
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

/// Launch a background analysis job (operator+). Validates the tool/scope at the edge, then
/// hands off to the runner; the row is returned immediately and progresses over SSE.
async fn create_analysis_job(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Json(body): Json<CreateAnalysisJob>,
) -> Response {
    if let Some(resp) = authorize(&st, &headers, Permission::ManageConfig) {
        return resp;
    }
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    let Some(tool) = AnalysisTool::from_str(&body.tool) else {
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_tool",
            format!("unknown analysis tool {:?}", body.tool),
        );
    };
    let Some(scope_kind) = ScopeKind::from_str(&body.scope_kind) else {
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_scope",
            format!(
                "scope_kind must be all|group|node, got {:?}",
                body.scope_kind
            ),
        );
    };
    if scope_kind != ScopeKind::All && body.scope_id.is_none() {
        return error_response(
            StatusCode::BAD_REQUEST,
            "missing_scope_id",
            "scope_id is required for group/node scope".to_owned(),
        );
    }
    // Clamp every numeric param to a sane bound (defence in depth at the edge — security.md).
    let params = JobParams {
        tool,
        scope_kind,
        scope_id: body.scope_id,
        scope_label: body.scope_label,
        window_secs: body.window_secs.clamp(300, 365 * 86_400),
        baseline_secs: body
            .baseline_secs
            .unwrap_or(14 * 86_400)
            .clamp(3600, 365 * 86_400),
        sensitivity: body.sensitivity.unwrap_or(3.0).clamp(0.5, 6.0),
        depth: body.depth.unwrap_or_else(|| "standard".to_owned()),
        family: body.family.unwrap_or_else(|| "all".to_owned()),
        notify: body.notify.unwrap_or(true),
    };
    let user = bearer(&headers)
        .and_then(|t| st.sessions.lookup(t))
        .map(|s| s.username);
    match admin.analysis.create(params, user).await {
        Ok(job) => Json(job).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "create analysis job failed");
            internal("failed to create analysis job")
        }
    }
}

/// Cancel a running analysis job (operator+). The task observes the flag between phases.
async fn cancel_analysis_job(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Response {
    if let Some(resp) = authorize(&st, &headers, Permission::ManageConfig) {
        return resp;
    }
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    if admin.analysis.cancel(id) {
        Json(serde_json::json!({ "cancelled": true })).into_response()
    } else {
        not_found("job_not_running", format!("no running analysis job {id}"))
    }
}

/// Live analysis-job status stream (SSE): each event is the job JSON with its current state and
/// progress. Mirrors the alert stream (lagged subscribers get a `resync` hint).
async fn stream_analysis(State(st): State<ApiState>, headers: HeaderMap) -> Response {
    if let Some(resp) = require_view(&st, &headers) {
        return resp;
    }
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
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

// ── Admin (write) handlers — live mode only ──────────────────────────────────

fn unavailable() -> Response {
    error_response(
        StatusCode::SERVICE_UNAVAILABLE,
        "admin_unavailable",
        "inventory/credential management is not available in skeleton mode".to_owned(),
    )
}

fn internal(what: &str) -> Response {
    error_response(
        StatusCode::INTERNAL_SERVER_ERROR,
        "internal_error",
        what.to_owned(),
    )
}

/// Extract the `Authorization: Bearer <token>` value, if present.
fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
}

/// Require a valid token with `perm`. Returns `Some(error response)` to short-circuit the
/// handler on failure (401/403), or `None` when authorized.
fn authorize(st: &ApiState, headers: &HeaderMap, perm: Permission) -> Option<Response> {
    match st.sessions.authorize(bearer(headers), perm) {
        Ok(_) => None,
        Err(AuthError::Forbidden) => Some(error_response(
            StatusCode::FORBIDDEN,
            "forbidden",
            "your role does not permit this action".to_owned(),
        )),
        Err(_) => Some(error_response(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "a valid bearer token is required".to_owned(),
        )),
    }
}

/// Gate a read-only endpoint. In public-dashboard mode reads are open (returns `None`);
/// otherwise a valid session with `View` (granted to every role) is required. Returns
/// `Some(error response)` to short-circuit on failure.
fn require_view(st: &ApiState, headers: &HeaderMap) -> Option<Response> {
    if st.public_dashboard {
        return None;
    }
    authorize(st, headers, Permission::View)
}

/// Login request body.
#[derive(Deserialize)]
struct LoginBody {
    username: String,
    password: String,
}

async fn login(State(st): State<ApiState>, Json(body): Json<LoginBody>) -> Response {
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    match admin.users.verify(&body.username, &body.password).await {
        Ok(Some(principal)) => {
            let role = principal.role;
            let token = st.sessions.issue(principal, &body.username);
            // Auth events are audited with the username only — never the credential.
            audit_record(&admin.audit, &body.username, "auth.login", 200).await;
            Json(serde_json::json!({ "token": token, "role": role })).into_response()
        }
        Ok(None) => {
            audit_record(&admin.audit, &body.username, "auth.login", 401).await;
            error_response(
                StatusCode::UNAUTHORIZED,
                "invalid_credentials",
                "incorrect username or password".to_owned(),
            )
        }
        Err(e) => {
            tracing::error!(error = %e, "login failed");
            internal("login failed")
        }
    }
}

async fn auth_me(State(st): State<ApiState>, headers: HeaderMap) -> Response {
    // `View` is granted to every role, so this just checks for a valid token.
    match st.sessions.authorize(bearer(&headers), Permission::View) {
        Ok(session) => Json(serde_json::json!({
            "role": session.principal.role,
            "username": session.username,
        }))
        .into_response(),
        Err(_) => error_response(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "not authenticated".to_owned(),
        ),
    }
}

/// One permission in the role/privilege matrix (`GET /api/v1/roles`).
#[derive(Serialize)]
struct PermissionInfo {
    key: &'static str,
    label: &'static str,
    description: &'static str,
}

/// One role in the matrix: its metadata and the permission keys it grants.
#[derive(Serialize)]
struct RoleInfo {
    key: &'static str,
    label: &'static str,
    description: &'static str,
    /// Built-in roles are fixed (custom roles are not configurable yet).
    builtin: bool,
    /// The keys of the permissions this role grants.
    permissions: Vec<&'static str>,
}

/// The role-vs-privilege matrix: the permission catalogue plus, for each role, the permissions
/// it grants. Read-only and informational (no secrets), so it only needs `View`. Roles are the
/// fixed built-ins today; the shape is forward-compatible with future custom roles.
async fn list_roles(State(st): State<ApiState>, headers: HeaderMap) -> Response {
    if let Some(resp) = require_view(&st, &headers) {
        return resp;
    }
    let permissions: Vec<PermissionInfo> = Permission::ALL
        .into_iter()
        .map(|p| PermissionInfo {
            key: p.key(),
            label: p.label(),
            description: p.description(),
        })
        .collect();
    let roles: Vec<RoleInfo> = Role::ALL
        .into_iter()
        .map(|r| RoleInfo {
            key: r.key(),
            label: r.label(),
            description: r.description(),
            builtin: true,
            permissions: Permission::ALL
                .into_iter()
                .filter(|p| r.grants(*p))
                .map(Permission::key)
                .collect(),
        })
        .collect();
    Json(serde_json::json!({ "permissions": permissions, "roles": roles })).into_response()
}

/// Create-node request body. `profile_id`/`credential_id`/`parent_id` are optional.
#[derive(Deserialize)]
struct CreateNode {
    name: String,
    address: String,
    pool: Option<String>,
    profile_id: Option<Uuid>,
    credential_id: Option<Uuid>,
    parent_id: Option<Uuid>,
    #[serde(default)]
    vendor: Option<String>,
    #[serde(default)]
    model: Option<String>,
}

async fn create_node(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Json(body): Json<CreateNode>,
) -> Response {
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    if let Some(resp) = authorize(&st, &headers, Permission::ManageConfig) {
        return resp;
    }
    if body.name.trim().is_empty() {
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_name",
            "node name must not be empty".to_owned(),
        );
    }
    let Ok(address) = body.address.parse::<IpAddr>() else {
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_address",
            format!("address {:?} is not a valid IP address", body.address),
        );
    };
    match admin
        .repo
        .create_node(
            body.name.trim(),
            address,
            body.pool.as_deref(),
            body.profile_id,
            body.credential_id,
            body.parent_id,
            body.vendor
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty()),
            body.model
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty()),
        )
        .await
    {
        Ok(id) => (StatusCode::CREATED, Json(serde_json::json!({ "id": id }))).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "create node failed");
            internal("failed to create node")
        }
    }
}

/// Set/clear a node's profile + bound credential and its descriptive maker/model. The node-edit
/// UI loads the current values and resends them, so an unchanged field is preserved.
#[derive(Deserialize)]
struct NodeBindings {
    profile_id: Option<Uuid>,
    credential_id: Option<Uuid>,
    #[serde(default)]
    vendor: Option<String>,
    #[serde(default)]
    model: Option<String>,
}

async fn set_node_bindings(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<NodeBindings>,
) -> Response {
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    if let Some(resp) = authorize(&st, &headers, Permission::ManageConfig) {
        return resp;
    }
    match admin
        .repo
        .set_node_bindings(
            id,
            body.profile_id,
            body.credential_id,
            body.vendor
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty()),
            body.model
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty()),
        )
        .await
    {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => not_found("node_not_found", format!("no node {id}")),
        Err(e) => {
            tracing::error!(error = %e, "set node bindings failed");
            internal("failed to update node")
        }
    }
}

/// Validate an operator-supplied monitor URL at the API edge (security.md: validate input; ADR
/// §229: SSRF). Scheme must be http/https; an IP-literal host that is loopback/link-local
/// (incl. cloud metadata)/multicast/unspecified is refused. Hostnames are allowed here (the
/// transport does a deeper resolved-address check before the request). `Ok` ⇒ the parsed URL.
// The `Err` is the standard axum `Response` used by every handler in this module; boxing it just
// to satisfy the lint would make call sites noisier than the win.
#[allow(clippy::result_large_err)]
fn validate_monitor_url(url: &str) -> Result<reqwest::Url, Response> {
    let parsed = reqwest::Url::parse(url).map_err(|_| {
        error_response(
            StatusCode::BAD_REQUEST,
            "invalid_url",
            format!("{url:?} is not a valid URL"),
        )
    })?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            "invalid_url_scheme",
            "url scheme must be http or https".to_owned(),
        ));
    }
    if let Some(host) = parsed.host_str() {
        if let Ok(ip) = host.parse::<IpAddr>() {
            if is_ssrf_blocked(ip) {
                return Err(error_response(
                    StatusCode::BAD_REQUEST,
                    "blocked_target",
                    "target address is not allowed (loopback / link-local / metadata)".to_owned(),
                ));
            }
        }
    } else {
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            "invalid_url",
            "url must have a host".to_owned(),
        ));
    }
    Ok(parsed)
}

/// Best-effort resolve a URL's host to a single management IP for the node's `address` column
/// (used for display only — URL monitors skip ICMP and the probe re-resolves the URL each poll).
/// Falls back to the unspecified address if the host can't be resolved.
async fn resolve_monitor_address(url: &reqwest::Url) -> IpAddr {
    let unspecified = IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED);
    let Some(host) = url.host_str() else {
        return unspecified;
    };
    if let Ok(ip) = host.parse::<IpAddr>() {
        return ip;
    }
    let port = url.port_or_known_default().unwrap_or(443);
    match tokio::net::lookup_host((host, port)).await {
        Ok(mut addrs) => addrs.next().map_or(unspecified, |a| a.ip()),
        Err(_) => unspecified,
    }
}

/// The URL-monitor config for a node, or 404 if the node isn't a URL monitor.
async fn get_url_check(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Path(node_id): Path<Uuid>,
) -> Response {
    if let Some(resp) = require_view(&st, &headers) {
        return resp;
    }
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    match admin.url_checks.get(node_id).await {
        Ok(Some(cfg)) => Json(cfg).into_response(),
        Ok(None) => not_found(
            "url_check_not_found",
            format!("no url check for node {node_id}"),
        ),
        Err(e) => {
            tracing::error!(error = %e, "get url check failed");
            internal("failed to load url check")
        }
    }
}

/// Create or replace a node's URL-monitor config. The node must already exist.
async fn set_url_check(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Path(node_id): Path<Uuid>,
    Json(cfg): Json<UrlCheckConfig>,
) -> Response {
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    if let Some(resp) = authorize(&st, &headers, Permission::ManageConfig) {
        return resp;
    }
    if let Err(resp) = validate_monitor_url(&cfg.url) {
        return resp;
    }
    match admin.repo.get_node(node_id).await {
        Ok(Some(_)) => {}
        Ok(None) => return not_found("node_not_found", format!("no node {node_id}")),
        Err(e) => {
            tracing::error!(error = %e, "set url check: load node failed");
            return internal("failed to load node");
        }
    }
    match admin.url_checks.upsert(node_id, &cfg).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => {
            tracing::error!(error = %e, "set url check failed");
            internal("failed to save url check")
        }
    }
}

/// Remove a node's URL-monitor config (the node itself is untouched).
async fn delete_url_check(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Path(node_id): Path<Uuid>,
) -> Response {
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    if let Some(resp) = authorize(&st, &headers, Permission::ManageConfig) {
        return resp;
    }
    match admin.url_checks.delete(node_id).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => not_found(
            "url_check_not_found",
            format!("no url check for node {node_id}"),
        ),
        Err(e) => {
            tracing::error!(error = %e, "delete url check failed");
            internal("failed to delete url check")
        }
    }
}

/// Create a URL monitor in one call: a node bound to the built-in URL/HTTP profile (so it inherits
/// the default thresholds) plus its URL-check config. `name`/`parent_id`/`pool` plus a flattened
/// [`UrlCheckConfig`] (only `url` is required; everything else defaults).
#[derive(Deserialize)]
struct CreateUrlMonitor {
    name: String,
    #[serde(default)]
    parent_id: Option<Uuid>,
    #[serde(default)]
    pool: Option<String>,
    #[serde(flatten)]
    config: UrlCheckConfig,
}

async fn create_url_monitor(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Json(body): Json<CreateUrlMonitor>,
) -> Response {
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    if let Some(resp) = authorize(&st, &headers, Permission::ManageConfig) {
        return resp;
    }
    if body.name.trim().is_empty() {
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_name",
            "monitor name must not be empty".to_owned(),
        );
    }
    let parsed = match validate_monitor_url(&body.config.url) {
        Ok(u) => u,
        Err(resp) => return resp,
    };
    // Bind to the built-in URL/HTTP profile (if present) so default thresholds are inherited.
    let profile = admin
        .repo
        .profile_id_for_category(ProfileCategory::UrlCheck.as_str())
        .await
        .unwrap_or(None);
    let address = resolve_monitor_address(&parsed).await;
    let node_id = match admin
        .repo
        .create_node(
            body.name.trim(),
            address,
            body.pool.as_deref(),
            profile,
            None,
            body.parent_id,
            None,
            None,
        )
        .await
    {
        Ok(id) => id,
        Err(e) => {
            tracing::error!(error = %e, "create url monitor: create node failed");
            return internal("failed to create url monitor");
        }
    };
    if let Err(e) = admin.url_checks.upsert(node_id, &body.config).await {
        // The node exists but the check didn't save — roll back the node so we don't leave a
        // half-created monitor (a URL-category node with no check would just get no jobs).
        let _ = admin.repo.delete_node(node_id).await;
        tracing::error!(error = %e, "create url monitor: save check failed");
        return internal("failed to create url monitor");
    }
    (
        StatusCode::CREATED,
        Json(serde_json::json!({ "id": node_id })),
    )
        .into_response()
}

/// Trigger an immediate poll of one node (the "poll now" action): dispatches its full configured
/// poll set (ICMP liveness + SNMP scalar/table, per its bindings) to the bus right away, bypassing
/// the scheduler's interval/jitter. Results arrive asynchronously on the normal result path, so
/// this just confirms how many jobs were dispatched — the UI refreshes its readings shortly after.
/// `ManageConfig` (an operator action, like a discovery scan); audited by the mutation middleware.
async fn poll_node_now(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Path(node_id): Path<Uuid>,
) -> Response {
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    if let Some(resp) = authorize(&st, &headers, Permission::ManageConfig) {
        return resp;
    }
    let node = match admin.repo.get_node(node_id).await {
        Ok(Some(node)) => node,
        Ok(None) => return not_found("node_not_found", format!("no node {node_id}")),
        Err(e) => {
            tracing::error!(error = %e, "poll-now: load node failed");
            return internal("failed to load node");
        }
    };
    let dispatched = admin.poll.poll_now(&node).await;
    tracing::info!(node = %node_id, dispatched, "manual poll dispatched");
    (
        StatusCode::ACCEPTED,
        Json(serde_json::json!({ "dispatched": dispatched })),
    )
        .into_response()
}

/// Move a node into a group (or `null` to ungroup). Used by the inventory tree (drag/move).
#[derive(Deserialize)]
struct NodeGroupAssignment {
    group_id: Option<Uuid>,
}

async fn set_node_group(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<NodeGroupAssignment>,
) -> Response {
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    if let Some(resp) = authorize(&st, &headers, Permission::ManageConfig) {
        return resp;
    }
    match admin.repo.set_node_group(id, body.group_id).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => not_found("node_not_found", format!("no node {id}")),
        Err(e) => {
            tracing::error!(error = %e, "set node group failed");
            internal("failed to move node")
        }
    }
}

/// Drag-reorder a node within (or into) a group, positioning it relative to a sibling node.
/// `group_id` is the destination group (`null` ⇒ ungrouped); `before`/`after` name the sibling
/// to land next to (both omitted ⇒ append to the end). At most one of before/after may be set.
#[derive(Deserialize)]
struct NodePlacement {
    #[serde(default)]
    group_id: Option<Uuid>,
    #[serde(default)]
    before: Option<Uuid>,
    #[serde(default)]
    after: Option<Uuid>,
}

async fn place_node(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<NodePlacement>,
) -> Response {
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    if let Some(resp) = authorize(&st, &headers, Permission::ManageConfig) {
        return resp;
    }
    if body.before.is_some() && body.after.is_some() {
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_placement",
            "specify at most one of before/after".to_owned(),
        );
    }
    // Order among the destination group's current members, excluding the moving node so it
    // doesn't anchor against itself, then interpolate a fractional order next to the target.
    let siblings = match admin.repo.ordered_nodes_in_group(body.group_id).await {
        Ok(s) => s
            .into_iter()
            .filter(|(sid, _)| *sid != id)
            .collect::<Vec<_>>(),
        Err(e) => {
            tracing::error!(error = %e, "load node siblings failed");
            return internal("failed to move node");
        }
    };
    let order = placement_order(&siblings, body.before, body.after);
    match admin.repo.place_node(id, body.group_id, order).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => not_found("node_not_found", format!("no node {id}")),
        Err(e) => {
            tracing::error!(error = %e, "place node failed");
            internal("failed to move node")
        }
    }
}

// ── Node groups (the inventory folder tree) — ManageConfig writes, View reads ─

async fn list_node_groups(State(st): State<ApiState>, headers: HeaderMap) -> Response {
    if let Some(resp) = require_view(&st, &headers) {
        return resp;
    }
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    match admin.groups.list().await {
        Ok(list) => Json(list).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "list node groups failed");
            internal("failed to list node groups")
        }
    }
}

/// Create/update body for a group. `group_type` is a validated [`GroupType`] key.
#[derive(Deserialize)]
struct GroupBody {
    name: String,
    group_type: String,
    #[serde(default)]
    parent_id: Option<Uuid>,
}

/// Validate the request: non-empty name + a known group type. Returns the parsed type, or a
/// client-safe message for a 400 (kept as a `String` so the `Err` stays small).
fn parse_group_body(body: &GroupBody) -> Result<GroupType, String> {
    if body.name.trim().is_empty() {
        return Err("group name must not be empty".to_owned());
    }
    GroupType::from_key(body.group_type.trim())
        .ok_or_else(|| format!("unknown group type {:?}", body.group_type))
}

async fn create_node_group(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Json(body): Json<GroupBody>,
) -> Response {
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    if let Some(resp) = authorize(&st, &headers, Permission::ManageConfig) {
        return resp;
    }
    let group_type = match parse_group_body(&body) {
        Ok(t) => t,
        Err(msg) => return error_response(StatusCode::BAD_REQUEST, "invalid_group", msg),
    };
    match admin
        .groups
        .create(body.name.trim(), group_type, body.parent_id)
        .await
    {
        Ok(id) => (StatusCode::CREATED, Json(serde_json::json!({ "id": id }))).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "create node group failed");
            internal("failed to create group")
        }
    }
}

async fn update_node_group(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<GroupBody>,
) -> Response {
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    if let Some(resp) = authorize(&st, &headers, Permission::ManageConfig) {
        return resp;
    }
    let group_type = match parse_group_body(&body) {
        Ok(t) => t,
        Err(msg) => return error_response(StatusCode::BAD_REQUEST, "invalid_group", msg),
    };
    // Reject a re-parent that would create a cycle (a group can't be its own ancestor).
    if body.parent_id.is_some() {
        match admin.groups.edges().await {
            Ok(edges) => {
                if would_create_cycle(&edges, id, body.parent_id) {
                    return error_response(
                        StatusCode::BAD_REQUEST,
                        "invalid_group",
                        "that move would nest the group inside itself".to_owned(),
                    );
                }
            }
            Err(e) => {
                tracing::error!(error = %e, "load group edges failed");
                return internal("failed to update group");
            }
        }
    }
    match admin
        .groups
        .update(id, body.name.trim(), group_type, body.parent_id)
        .await
    {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => not_found("group_not_found", format!("no group {id}")),
        Err(e) => {
            tracing::error!(error = %e, "update node group failed");
            internal("failed to update group")
        }
    }
}

/// Drag-reorder a group, re-parenting it under `parent_id` (`null` ⇒ top level) and positioning
/// it relative to a sibling group. `before`/`after` name the sibling (both omitted ⇒ append).
/// Refuses a move that would nest the group inside its own subtree (cycle guard).
#[derive(Deserialize)]
struct GroupPlacement {
    #[serde(default)]
    parent_id: Option<Uuid>,
    #[serde(default)]
    before: Option<Uuid>,
    #[serde(default)]
    after: Option<Uuid>,
}

async fn place_group(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<GroupPlacement>,
) -> Response {
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    if let Some(resp) = authorize(&st, &headers, Permission::ManageConfig) {
        return resp;
    }
    if body.before.is_some() && body.after.is_some() {
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_placement",
            "specify at most one of before/after".to_owned(),
        );
    }
    let edges = match admin.groups.edges().await {
        Ok(e) => e,
        Err(e) => {
            tracing::error!(error = %e, "load group edges failed");
            return internal("failed to move group");
        }
    };
    if would_create_cycle(&edges, id, body.parent_id) {
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_group",
            "that move would nest the group inside itself".to_owned(),
        );
    }
    let siblings = match admin.groups.ordered_siblings(body.parent_id).await {
        Ok(s) => s
            .into_iter()
            .filter(|(sid, _)| *sid != id)
            .collect::<Vec<_>>(),
        Err(e) => {
            tracing::error!(error = %e, "load group siblings failed");
            return internal("failed to move group");
        }
    };
    let order = placement_order(&siblings, body.before, body.after);
    match admin.groups.place(id, body.parent_id, order).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => not_found("group_not_found", format!("no group {id}")),
        Err(e) => {
            tracing::error!(error = %e, "place group failed");
            internal("failed to move group")
        }
    }
}

/// Set/clear a group's geo coordinates (both or neither). Body: `{ latitude, longitude }` —
/// `null` for both clears the pin.
#[derive(Deserialize)]
struct GroupGeo {
    latitude: Option<f64>,
    longitude: Option<f64>,
}

async fn set_node_group_geo(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<GroupGeo>,
) -> Response {
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    if let Some(resp) = authorize(&st, &headers, Permission::ManageConfig) {
        return resp;
    }
    // Both or neither, and within valid coordinate ranges.
    match (body.latitude, body.longitude) {
        (Some(lat), Some(lon)) => {
            if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
                return error_response(
                    StatusCode::BAD_REQUEST,
                    "invalid_coordinates",
                    "latitude must be -90..90 and longitude -180..180".to_owned(),
                );
            }
        }
        (None, None) => {}
        _ => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "invalid_coordinates",
                "provide both latitude and longitude, or neither (to clear)".to_owned(),
            )
        }
    }
    match admin
        .groups
        .set_geo(id, body.latitude, body.longitude)
        .await
    {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => not_found("group_not_found", format!("no group {id}")),
        Err(e) => {
            tracing::error!(error = %e, "set group geo failed");
            internal("failed to set group coordinates")
        }
    }
}

async fn delete_node_group(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Response {
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    if let Some(resp) = authorize(&st, &headers, Permission::ManageConfig) {
        return resp;
    }
    match admin.groups.delete(id).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => not_found("group_not_found", format!("no group {id}")),
        Err(e) => {
            tracing::error!(error = %e, "delete node group failed");
            internal("failed to delete group")
        }
    }
}

async fn list_profiles(State(st): State<ApiState>, headers: HeaderMap) -> Response {
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    if let Some(resp) = authorize(&st, &headers, Permission::ManageConfig) {
        return resp;
    }
    match admin.repo.list_profiles().await {
        Ok(list) => Json(list).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "list profiles failed");
            internal("failed to list profiles")
        }
    }
}

/// Create/update-profile request body. `category` is optional on create (defaults to
/// generic-snmp); when present it must be a valid `ProfileCategory` token.
#[derive(Deserialize)]
struct ProfileBody {
    name: String,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    vendor: Option<String>,
    /// Optional per-profile polling-interval override (seconds). Omitted/`null` ⇒ inherit the
    /// global default; when present it must fall within `[MIN, MAX]`.
    #[serde(default)]
    poll_interval_secs: Option<u32>,
}

/// Validated profile fields ready for the repo: name, category token, vendor, and the optional
/// interval override (as `i32` for the INTEGER column; `None` ⇒ inherit the global default).
struct ParsedProfile {
    name: String,
    category: &'static str,
    vendor: Option<String>,
    poll_interval_secs: Option<i32>,
}

/// Validate the body into [`ParsedProfile`] or `(error_code, message)` for a 400. Returns the
/// small error tuple (not a `Response`) so the helper stays cheap to return.
fn parse_profile_body(body: &ProfileBody) -> Result<ParsedProfile, (&'static str, String)> {
    let name = body.name.trim();
    if name.is_empty() {
        return Err(("invalid_name", "profile name must not be empty".to_owned()));
    }
    let category = match body.category.as_deref().map(str::trim) {
        None | Some("") => ProfileCategory::default(),
        Some(tok) => ProfileCategory::from_token(tok).ok_or_else(|| {
            (
                "invalid_category",
                format!("unknown profile category {tok:?}"),
            )
        })?,
    };
    let vendor = body
        .vendor
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    let poll_interval_secs = match body.poll_interval_secs {
        None => None,
        Some(n) => {
            if !crate::config::interval_in_bounds(n) {
                return Err((
                    "invalid_poll_interval",
                    format!(
                        "poll interval must be {}-{} seconds",
                        crate::config::MIN_POLL_INTERVAL_SECS,
                        crate::config::MAX_POLL_INTERVAL_SECS
                    ),
                ));
            }
            Some(n as i32)
        }
    };
    Ok(ParsedProfile {
        name: name.to_owned(),
        category: category.as_str(),
        vendor,
        poll_interval_secs,
    })
}

async fn create_profile(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Json(body): Json<ProfileBody>,
) -> Response {
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    if let Some(resp) = authorize(&st, &headers, Permission::ManageConfig) {
        return resp;
    }
    let p = match parse_profile_body(&body) {
        Ok(v) => v,
        Err((code, msg)) => return error_response(StatusCode::BAD_REQUEST, code, msg),
    };
    match admin
        .repo
        .create_profile(
            &p.name,
            p.category,
            p.vendor.as_deref(),
            p.poll_interval_secs,
        )
        .await
    {
        Ok(id) => (StatusCode::CREATED, Json(serde_json::json!({ "id": id }))).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "create profile failed");
            internal("failed to create profile")
        }
    }
}

async fn update_profile(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<ProfileBody>,
) -> Response {
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    if let Some(resp) = authorize(&st, &headers, Permission::ManageConfig) {
        return resp;
    }
    let p = match parse_profile_body(&body) {
        Ok(v) => v,
        Err((code, msg)) => return error_response(StatusCode::BAD_REQUEST, code, msg),
    };
    match admin
        .repo
        .update_profile(
            id,
            &p.name,
            p.category,
            p.vendor.as_deref(),
            p.poll_interval_secs,
        )
        .await
    {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => not_found("profile_not_found", format!("no profile {id}")),
        Err(e) => {
            tracing::error!(error = %e, "update profile failed");
            internal("failed to update profile")
        }
    }
}

async fn delete_profile(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Response {
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    if let Some(resp) = authorize(&st, &headers, Permission::ManageConfig) {
        return resp;
    }
    match admin.repo.delete_profile(id).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => not_found("profile_not_found", format!("no profile {id}")),
        Err(e) => {
            tracing::error!(error = %e, "delete profile failed");
            internal("failed to delete profile")
        }
    }
}

async fn delete_node(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Response {
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    if let Some(resp) = authorize(&st, &headers, Permission::ManageConfig) {
        return resp;
    }
    match admin.repo.delete_node(id).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => not_found("node_not_found", format!("no node {id}")),
        Err(e) => {
            tracing::error!(error = %e, "delete node failed");
            internal("failed to delete node")
        }
    }
}

/// Create-credential request body. `secret` is encrypted before storage and never logged.
#[derive(Deserialize)]
struct CreateCredential {
    name: String,
    kind: String,
    secret: String,
}

async fn create_credential(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Json(body): Json<CreateCredential>,
) -> Response {
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    if let Some(resp) = authorize(&st, &headers, Permission::ManageCredentials) {
        return resp;
    }
    if body.name.trim().is_empty() || body.kind.trim().is_empty() || body.secret.is_empty() {
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_credential",
            "name, kind, and secret are required".to_owned(),
        );
    }
    // An snmp_v3 secret must be a structurally valid USM document — reject at the edge so
    // a malformed one can't silently break polling later. The reason is static text only
    // (never any field content).
    if body.kind.trim() == crate::secrets::KIND_SNMP_V3 {
        if let Err(reason) = crate::secrets::SnmpV3Secret::parse(body.secret.as_bytes()) {
            return error_response(
                StatusCode::BAD_REQUEST,
                "invalid_credential",
                format!("invalid SNMPv3 credential: {reason}"),
            );
        }
    }
    match admin
        .creds
        .create(body.name.trim(), body.kind.trim(), body.secret.as_bytes())
        .await
    {
        Ok(id) => (StatusCode::CREATED, Json(serde_json::json!({ "id": id }))).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "create credential failed");
            internal("failed to store credential")
        }
    }
}

async fn list_credentials(State(st): State<ApiState>, headers: HeaderMap) -> Response {
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    if let Some(resp) = authorize(&st, &headers, Permission::ManageCredentials) {
        return resp;
    }
    match admin.creds.list().await {
        Ok(list) => Json(list).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "list credentials failed");
            internal("failed to list credentials")
        }
    }
}

/// Update-credential request body. `name` is required; `secret` is optional — when present the
/// secret is re-sealed and `kind` must accompany it (the secret format is kind-specific). With no
/// `secret` only the name changes (rename) and the stored secret is left intact. `secret` is
/// encrypted before storage and never logged.
#[derive(Deserialize)]
struct UpdateCredential {
    name: String,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    secret: Option<String>,
}

async fn update_credential(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateCredential>,
) -> Response {
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    if let Some(resp) = authorize(&st, &headers, Permission::ManageCredentials) {
        return resp;
    }
    let name = body.name.trim();
    if name.is_empty() {
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_credential",
            "name is required".to_owned(),
        );
    }
    // Resolve the optional re-seal. A non-empty secret replaces the stored one and must carry a
    // kind (the secret format is kind-specific); a missing/blank secret is a rename only.
    let secret = body.secret.as_deref().filter(|s| !s.is_empty());
    let reseal = match secret {
        Some(secret) => {
            let Some(kind) = body
                .kind
                .as_deref()
                .map(str::trim)
                .filter(|k| !k.is_empty())
            else {
                return error_response(
                    StatusCode::BAD_REQUEST,
                    "invalid_credential",
                    "kind is required when changing the secret".to_owned(),
                );
            };
            if kind == crate::secrets::KIND_SNMP_V3 {
                if let Err(reason) = crate::secrets::SnmpV3Secret::parse(secret.as_bytes()) {
                    return error_response(
                        StatusCode::BAD_REQUEST,
                        "invalid_credential",
                        format!("invalid SNMPv3 credential: {reason}"),
                    );
                }
            }
            Some((kind, secret.as_bytes()))
        }
        None => None,
    };
    match admin.creds.update(id, name, reseal).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => not_found("credential_not_found", format!("no credential {id}")),
        Err(e) => {
            tracing::error!(error = %e, "update credential failed");
            internal("failed to update credential")
        }
    }
}

async fn delete_credential(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Response {
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    if let Some(resp) = authorize(&st, &headers, Permission::ManageCredentials) {
        return resp;
    }
    match admin.creds.delete(id).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => not_found("credential_not_found", format!("no credential {id}")),
        Err(e) => {
            tracing::error!(error = %e, "delete credential failed");
            internal("failed to delete credential")
        }
    }
}

async fn list_thresholds(State(st): State<ApiState>, headers: HeaderMap) -> Response {
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    if let Some(resp) = authorize(&st, &headers, Permission::ManageConfig) {
        return resp;
    }
    match admin.thresholds.list_all().await {
        Ok(list) => Json(list).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "list thresholds failed");
            internal("failed to list thresholds")
        }
    }
}

/// Create-threshold request body.
#[derive(Deserialize)]
struct CreateThreshold {
    scope_level: String,
    scope_id: String,
    metric: String,
    direction: String,
    warning: Option<f64>,
    critical: Option<f64>,
    dwell_samples: Option<i32>,
}

async fn create_threshold(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Json(body): Json<CreateThreshold>,
) -> Response {
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    if let Some(resp) = authorize(&st, &headers, Permission::ManageConfig) {
        return resp;
    }
    if !is_valid_metric_name(&body.metric) {
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_metric_name",
            "metric must be a valid identifier".to_owned(),
        );
    }
    if !matches!(body.scope_level.as_str(), "profile" | "group" | "node")
        || !matches!(body.direction.as_str(), "above" | "below")
    {
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_threshold",
            "scope_level must be profile|group|node and direction above|below".to_owned(),
        );
    }
    match admin
        .thresholds
        .create(
            &body.scope_level,
            &body.scope_id,
            &body.metric,
            &body.direction,
            body.warning,
            body.critical,
            body.dwell_samples.unwrap_or(3),
        )
        .await
    {
        Ok(id) => (StatusCode::CREATED, Json(serde_json::json!({ "id": id }))).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "create threshold failed");
            internal("failed to create threshold")
        }
    }
}

async fn delete_threshold(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Response {
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    if let Some(resp) = authorize(&st, &headers, Permission::ManageConfig) {
        return resp;
    }
    match admin.thresholds.delete(id).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => not_found("threshold_not_found", format!("no threshold {id}")),
        Err(e) => {
            tracing::error!(error = %e, "delete threshold failed");
            internal("failed to delete threshold")
        }
    }
}

// ── Notification channels + routing rules — ManageConfig only ────────────────

/// Toggle body shared by channel/rule enable-disable PUTs.
#[derive(Deserialize)]
struct EnabledBody {
    enabled: bool,
}

async fn list_notification_channels(State(st): State<ApiState>, headers: HeaderMap) -> Response {
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    if let Some(resp) = authorize(&st, &headers, Permission::ManageConfig) {
        return resp;
    }
    match admin.notifications.list_channels().await {
        Ok(list) => Json(list).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "list notification channels failed");
            internal("failed to list notification channels")
        }
    }
}

/// Create-channel body: a name + the (secret) connection config (tagged by `kind`).
#[derive(Deserialize)]
struct CreateChannel {
    name: String,
    config: ChannelConfig,
}

async fn create_notification_channel(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Json(body): Json<CreateChannel>,
) -> Response {
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    if let Some(resp) = authorize(&st, &headers, Permission::ManageConfig) {
        return resp;
    }
    if body.name.trim().is_empty() {
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_channel",
            "name must not be empty".to_owned(),
        );
    }
    if let Err(msg) = validate_channel_config(&body.config) {
        return error_response(StatusCode::BAD_REQUEST, "invalid_channel", msg.to_owned());
    }
    match admin
        .notifications
        .create_channel(body.name.trim(), &body.config)
        .await
    {
        Ok(id) => (StatusCode::CREATED, Json(serde_json::json!({ "id": id }))).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "create notification channel failed");
            internal("failed to create notification channel")
        }
    }
}

async fn set_notification_channel_enabled(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<EnabledBody>,
) -> Response {
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    if let Some(resp) = authorize(&st, &headers, Permission::ManageConfig) {
        return resp;
    }
    match admin
        .notifications
        .set_channel_enabled(id, body.enabled)
        .await
    {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => not_found("channel_not_found", format!("no channel {id}")),
        Err(e) => {
            tracing::error!(error = %e, "update notification channel failed");
            internal("failed to update notification channel")
        }
    }
}

async fn delete_notification_channel(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Response {
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    if let Some(resp) = authorize(&st, &headers, Permission::ManageConfig) {
        return resp;
    }
    match admin.notifications.delete_channel(id).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => not_found("channel_not_found", format!("no channel {id}")),
        Err(e) => {
            tracing::error!(error = %e, "delete notification channel failed");
            internal("failed to delete notification channel")
        }
    }
}

async fn list_routing_rules(State(st): State<ApiState>, headers: HeaderMap) -> Response {
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    if let Some(resp) = authorize(&st, &headers, Permission::ManageConfig) {
        return resp;
    }
    match admin.notifications.list_rules().await {
        Ok(list) => Json(list).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "list routing rules failed");
            internal("failed to list routing rules")
        }
    }
}

/// Create-rule body: a name, optional severity filter (null = any), and target channels.
#[derive(Deserialize)]
struct CreateRule {
    name: String,
    severity: Option<String>,
    channel_ids: Vec<Uuid>,
}

async fn create_routing_rule(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Json(body): Json<CreateRule>,
) -> Response {
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    if let Some(resp) = authorize(&st, &headers, Permission::ManageConfig) {
        return resp;
    }
    if body.name.trim().is_empty() {
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_rule",
            "name must not be empty".to_owned(),
        );
    }
    let severity = match parse_severity_opt(body.severity.as_deref()) {
        Ok(s) => s,
        Err(()) => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "invalid_rule",
                "severity must be critical|warning|info or null".to_owned(),
            )
        }
    };
    match admin
        .notifications
        .create_rule(body.name.trim(), severity, &body.channel_ids)
        .await
    {
        Ok(id) => (StatusCode::CREATED, Json(serde_json::json!({ "id": id }))).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "create routing rule failed");
            internal("failed to create routing rule")
        }
    }
}

async fn set_routing_rule_enabled(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<EnabledBody>,
) -> Response {
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    if let Some(resp) = authorize(&st, &headers, Permission::ManageConfig) {
        return resp;
    }
    match admin.notifications.set_rule_enabled(id, body.enabled).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => not_found("rule_not_found", format!("no rule {id}")),
        Err(e) => {
            tracing::error!(error = %e, "update routing rule failed");
            internal("failed to update routing rule")
        }
    }
}

async fn delete_routing_rule(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Response {
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    if let Some(resp) = authorize(&st, &headers, Permission::ManageConfig) {
        return resp;
    }
    match admin.notifications.delete_rule(id).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => not_found("rule_not_found", format!("no rule {id}")),
        Err(e) => {
            tracing::error!(error = %e, "delete routing rule failed");
            internal("failed to delete routing rule")
        }
    }
}

/// Parse an optional severity token from the API (None = any). `Err` ⇒ unknown token.
fn parse_severity_opt(s: Option<&str>) -> Result<Option<Severity>, ()> {
    match s {
        None => Ok(None),
        Some("critical") => Ok(Some(Severity::Critical)),
        Some("warning") => Ok(Some(Severity::Warning)),
        Some("info") => Ok(Some(Severity::Info)),
        Some(_) => Err(()),
    }
}

/// Light validation of a channel's connection config at the API edge.
fn validate_channel_config(c: &ChannelConfig) -> Result<(), &'static str> {
    match c {
        ChannelConfig::Webhook { url } if url.trim().is_empty() => Err("webhook url required"),
        ChannelConfig::Email { host, from, to, .. }
            if host.trim().is_empty() || from.trim().is_empty() || to.trim().is_empty() =>
        {
            Err("email host/from/to required")
        }
        _ => Ok(()),
    }
}

// ── MIB repository (curated OID catalog) — browse (View) / edit (ManageConfig) ──

/// Search query for the catalog list.
#[derive(Deserialize)]
struct MibQuery {
    q: Option<String>,
}

async fn list_mib_catalog(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Query(q): Query<MibQuery>,
) -> Response {
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    // Browsable by any viewer (the collection editor picks from it).
    if let Some(resp) = require_view(&st, &headers) {
        return resp;
    }
    let needle = q.q.as_deref().map(str::trim).filter(|s| !s.is_empty());
    match admin.mib.list(needle).await {
        Ok(list) => Json(list).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "list mib catalog failed");
            internal("failed to list MIB catalog")
        }
    }
}

/// Create-entry body for the catalog.
#[derive(Deserialize)]
struct CreateMibEntry {
    metric_name: String,
    oid: String,
    collection: String,
    metric_kind: String,
    vendor: Option<String>,
    description: Option<String>,
}

async fn create_mib_entry(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Json(body): Json<CreateMibEntry>,
) -> Response {
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    if let Some(resp) = authorize(&st, &headers, Permission::ManageConfig) {
        return resp;
    }
    if !is_valid_metric_name(&body.metric_name) {
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_metric_name",
            "metric_name must be a valid identifier".to_owned(),
        );
    }
    if !is_valid_oid(&body.oid) {
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_oid",
            "oid must be a dotted numeric OID".to_owned(),
        );
    }
    if !matches!(body.collection.as_str(), "scalar" | "table")
        || !matches!(body.metric_kind.as_str(), "gauge" | "counter")
    {
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_mib_entry",
            "collection must be scalar|table and metric_kind gauge|counter".to_owned(),
        );
    }
    match admin
        .mib
        .create(
            &body.metric_name,
            &body.oid,
            &body.collection,
            &body.metric_kind,
            body.vendor.as_deref(),
            body.description.as_deref(),
        )
        .await
    {
        Ok(Some(id)) => {
            (StatusCode::CREATED, Json(serde_json::json!({ "id": id }))).into_response()
        }
        Ok(None) => error_response(
            StatusCode::CONFLICT,
            "metric_name_taken",
            format!(
                "a catalog entry named '{}' already exists",
                body.metric_name
            ),
        ),
        Err(e) => {
            tracing::error!(error = %e, "create mib entry failed");
            internal("failed to create MIB entry")
        }
    }
}

async fn delete_mib_entry(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Response {
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    if let Some(resp) = authorize(&st, &headers, Permission::ManageConfig) {
        return resp;
    }
    match admin.mib.delete(id).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => not_found("mib_entry_not_found", format!("no entry {id}")),
        Err(e) => {
            tracing::error!(error = %e, "delete mib entry failed");
            internal("failed to delete MIB entry")
        }
    }
}

// ── Discovery (subnet sweep → review → import) — ManageConfig only ───────────

/// Most targets a single scan may sweep (keeps the sweep bounded).
const MAX_SCAN_TARGETS: usize = 1024;

/// Start-scan body: explicit target IPs (the WebUI expands a CIDR), candidate stored
/// credentials (by id — resolved server-side, ADR-018/020), and ad-hoc communities.
#[derive(Deserialize)]
struct StartScan {
    targets: Vec<String>,
    #[serde(default)]
    communities: Vec<String>,
    #[serde(default)]
    credential_ids: Vec<String>,
}

/// Resolve a scan's stored credential ids into inline candidates for the sweep job.
/// `Err` carries a client-safe message (ids and static reasons only — never any secret
/// content, security.md).
async fn resolve_scan_credentials(
    creds: &CredentialStore,
    ids: &[String],
) -> Result<Vec<yagra_bus::DiscoveryCredential>, Response> {
    let mut out = Vec::with_capacity(ids.len());
    for raw in ids {
        let Ok(id) = raw.parse::<Uuid>() else {
            return Err(error_response(
                StatusCode::BAD_REQUEST,
                "invalid_credential",
                format!("'{raw}' is not a valid credential id"),
            ));
        };
        let opened = match creds.open(id).await {
            Ok(o) => o,
            Err(e) => {
                tracing::error!(error = %e, credential = %id, "open scan credential failed");
                return Err(internal("failed to resolve a scan credential"));
            }
        };
        let Some((kind, secret)) = opened else {
            return Err(error_response(
                StatusCode::BAD_REQUEST,
                "credential_not_found",
                format!("no credential {id}"),
            ));
        };
        if kind == crate::secrets::KIND_SNMP_V3 {
            match crate::secrets::SnmpV3Secret::parse(&secret) {
                Ok(v3) => out.push(yagra_bus::DiscoveryCredential {
                    cred_ref: id,
                    community: None,
                    v3: Some(yagra_bus::DiscoveryV3 {
                        user: v3.user,
                        security_level: v3.security_level,
                        auth_protocol: v3.auth_protocol,
                        auth_key: v3.auth_key,
                        priv_protocol: v3.priv_protocol,
                        priv_key: v3.priv_key,
                    }),
                }),
                Err(reason) => {
                    return Err(error_response(
                        StatusCode::BAD_REQUEST,
                        "invalid_credential",
                        format!("credential {id} is not usable: {reason}"),
                    ))
                }
            }
        } else {
            match String::from_utf8(secret) {
                Ok(community) => out.push(yagra_bus::DiscoveryCredential {
                    cred_ref: id,
                    community: Some(community),
                    v3: None,
                }),
                Err(_) => {
                    return Err(error_response(
                        StatusCode::BAD_REQUEST,
                        "invalid_credential",
                        format!("credential {id} is not usable as an SNMP community"),
                    ))
                }
            }
        }
    }
    Ok(out)
}

async fn start_discovery_scan(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Json(body): Json<StartScan>,
) -> Response {
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    if let Some(resp) = authorize(&st, &headers, Permission::ManageConfig) {
        return resp;
    }
    if body.targets.is_empty() || body.targets.len() > MAX_SCAN_TARGETS {
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_scan",
            format!("targets must be 1..={MAX_SCAN_TARGETS} addresses"),
        );
    }
    let mut targets = Vec::with_capacity(body.targets.len());
    for t in &body.targets {
        match t.parse::<IpAddr>() {
            Ok(ip) => targets.push(ip),
            Err(_) => {
                return error_response(
                    StatusCode::BAD_REQUEST,
                    "invalid_address",
                    format!("'{t}' is not a valid IP address"),
                )
            }
        }
    }
    let credentials = match resolve_scan_credentials(&admin.creds, &body.credential_ids).await {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    match admin
        .discovery
        .start(targets, body.communities, credentials)
        .await
    {
        Ok(scan_id) => (
            StatusCode::ACCEPTED,
            Json(serde_json::json!({ "scan_id": scan_id })),
        )
            .into_response(),
        Err(e) => {
            tracing::error!(error = %e, "start discovery scan failed");
            internal("failed to start discovery scan")
        }
    }
}

async fn get_discovery_scan(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Response {
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    if let Some(resp) = authorize(&st, &headers, Permission::ManageConfig) {
        return resp;
    }
    match admin.discovery.get(id) {
        Some(status) => Json(status).into_response(),
        None => not_found("scan_not_found", format!("no scan {id}")),
    }
}

/// One discovered device the operator chose to add.
#[derive(Deserialize)]
struct ImportNode {
    address: String,
    name: String,
    profile_id: Option<String>,
    credential_id: Option<String>,
    /// Maker/model pre-filled from discovery's sysDescr classification (editable before import).
    #[serde(default)]
    vendor: Option<String>,
    #[serde(default)]
    model: Option<String>,
}

/// Import body: the selected devices to create as nodes.
#[derive(Deserialize)]
struct ImportDiscovered {
    nodes: Vec<ImportNode>,
}

async fn import_discovered(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Json(body): Json<ImportDiscovered>,
) -> Response {
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    if let Some(resp) = authorize(&st, &headers, Permission::ManageConfig) {
        return resp;
    }
    let parse_uuid = |s: &Option<String>| -> Result<Option<Uuid>, ()> {
        match s {
            None => Ok(None),
            Some(v) => v.parse::<Uuid>().map(Some).map_err(|_| ()),
        }
    };
    // Validate every node up front, then insert the whole batch in one transaction so a failure
    // partway can't leave a partial import (atomicity — see NodeRepo::import_nodes).
    let mut prepared: Vec<crate::repo::NewNode<'_>> = Vec::with_capacity(body.nodes.len());
    for n in &body.nodes {
        let Ok(addr) = n.address.parse::<IpAddr>() else {
            return error_response(
                StatusCode::BAD_REQUEST,
                "invalid_address",
                format!("'{}' is not a valid IP address", n.address),
            );
        };
        let name = n.name.trim();
        if name.is_empty() {
            return error_response(
                StatusCode::BAD_REQUEST,
                "invalid_node",
                "name must not be empty".to_owned(),
            );
        }
        let (Ok(profile), Ok(credential)) =
            (parse_uuid(&n.profile_id), parse_uuid(&n.credential_id))
        else {
            return error_response(
                StatusCode::BAD_REQUEST,
                "invalid_binding",
                "profile_id/credential_id must be UUIDs".to_owned(),
            );
        };
        prepared.push(crate::repo::NewNode {
            name,
            address: addr,
            profile,
            credential,
            vendor: n.vendor.as_deref().map(str::trim).filter(|s| !s.is_empty()),
            model: n.model.as_deref().map(str::trim).filter(|s| !s.is_empty()),
        });
    }
    let created = match admin.repo.import_nodes(&prepared).await {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(error = %e, "import discovered nodes failed");
            return internal("failed to import discovered nodes");
        }
    };
    (
        StatusCode::CREATED,
        Json(serde_json::json!({ "created": created })),
    )
        .into_response()
}

// ── Classification rules (discovery → suggested profile) — ManageConfig only ─

/// Create/update body for a classification rule. `profile_id` is a UUID string; at least one
/// of `sysobjectid_prefix` / `sysdescr_regex` must be present (validated below).
#[derive(Deserialize)]
struct ClassificationRuleBody {
    priority: i32,
    #[serde(default)]
    sysobjectid_prefix: Option<String>,
    #[serde(default)]
    sysdescr_regex: Option<String>,
    profile_id: String,
    #[serde(default)]
    vendor: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default = "default_enabled")]
    enabled: bool,
}

const fn default_enabled() -> bool {
    true
}

/// A validated, normalized classification rule ready for the repo (empty strings dropped to
/// `None`, regex compiled-checked, prefix shape-checked, profile existence confirmed).
struct NormalizedRule {
    priority: i32,
    prefix: Option<String>,
    regex: Option<String>,
    profile_id: Uuid,
    vendor: Option<String>,
    model: Option<String>,
    enabled: bool,
}

/// A dotted-OID prefix: dot-separated non-empty numeric arcs, with an optional trailing dot
/// (a trailing dot is the safe form so `1.3.6.1.4.1.9.` can't also match `...91`).
fn is_valid_oid_prefix(s: &str) -> bool {
    let core = s.strip_suffix('.').unwrap_or(s);
    !core.is_empty()
        && core
            .split('.')
            .all(|arc| !arc.is_empty() && arc.bytes().all(|b| b.is_ascii_digit()))
}

/// Validate + normalize a rule body. Errors are client-safe 400s (security.md: parse into
/// strong, bounded types at the edge; the regex engine is linear-time so a pattern can't ReDoS).
async fn normalize_classification_rule(
    repo: &ClassificationRepo,
    body: ClassificationRuleBody,
) -> Result<NormalizedRule, Response> {
    let trim_opt = |v: Option<String>| -> Option<String> {
        v.map(|s| s.trim().to_owned()).filter(|s| !s.is_empty())
    };
    let prefix = trim_opt(body.sysobjectid_prefix);
    let regex = trim_opt(body.sysdescr_regex);
    let vendor = trim_opt(body.vendor);
    let model = trim_opt(body.model);

    if prefix.is_none() && regex.is_none() {
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            "invalid_rule",
            "a rule needs a sysObjectID prefix and/or a sysDescr regex".to_owned(),
        ));
    }
    if let Some(p) = &prefix {
        if !is_valid_oid_prefix(p) {
            return Err(error_response(
                StatusCode::BAD_REQUEST,
                "invalid_prefix",
                "sysObjectID prefix must be a dotted-numeric OID (e.g. 1.3.6.1.4.1.9.)".to_owned(),
            ));
        }
    }
    if let Some(re) = &regex {
        if let Err(e) = regex::Regex::new(re) {
            return Err(error_response(
                StatusCode::BAD_REQUEST,
                "invalid_regex",
                format!("sysDescr regex does not compile: {e}"),
            ));
        }
    }
    let Ok(profile_id) = body.profile_id.parse::<Uuid>() else {
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            "invalid_profile",
            format!("'{}' is not a valid profile id", body.profile_id),
        ));
    };
    match repo.profile_exists(profile_id).await {
        Ok(true) => {}
        Ok(false) => {
            return Err(error_response(
                StatusCode::BAD_REQUEST,
                "profile_not_found",
                format!("no profile {profile_id}"),
            ))
        }
        Err(e) => {
            tracing::error!(error = %e, "profile existence check failed");
            return Err(internal("failed to validate the profile"));
        }
    }
    Ok(NormalizedRule {
        priority: body.priority,
        prefix,
        regex,
        profile_id,
        vendor,
        model,
        enabled: body.enabled,
    })
}

async fn list_classification_rules(State(st): State<ApiState>, headers: HeaderMap) -> Response {
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    if let Some(resp) = authorize(&st, &headers, Permission::ManageConfig) {
        return resp;
    }
    match admin.classification.list_rules().await {
        Ok(rules) => Json(rules).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "list classification rules failed");
            internal("failed to list classification rules")
        }
    }
}

async fn create_classification_rule(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Json(body): Json<ClassificationRuleBody>,
) -> Response {
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    if let Some(resp) = authorize(&st, &headers, Permission::ManageConfig) {
        return resp;
    }
    let r = match normalize_classification_rule(&admin.classification, body).await {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    match admin
        .classification
        .create_rule(
            r.priority,
            r.prefix.as_deref(),
            r.regex.as_deref(),
            r.profile_id,
            r.vendor.as_deref(),
            r.model.as_deref(),
            r.enabled,
        )
        .await
    {
        Ok(id) => {
            reload_classifier(admin).await;
            (StatusCode::CREATED, Json(serde_json::json!({ "id": id }))).into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "create classification rule failed");
            internal("failed to create classification rule")
        }
    }
}

async fn update_classification_rule(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<ClassificationRuleBody>,
) -> Response {
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    if let Some(resp) = authorize(&st, &headers, Permission::ManageConfig) {
        return resp;
    }
    let r = match normalize_classification_rule(&admin.classification, body).await {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    match admin
        .classification
        .update_rule(
            id,
            r.priority,
            r.prefix.as_deref(),
            r.regex.as_deref(),
            r.profile_id,
            r.vendor.as_deref(),
            r.model.as_deref(),
            r.enabled,
        )
        .await
    {
        Ok(true) => {
            reload_classifier(admin).await;
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(false) => not_found("rule_not_found", format!("no classification rule {id}")),
        Err(e) => {
            tracing::error!(error = %e, "update classification rule failed");
            internal("failed to update classification rule")
        }
    }
}

async fn delete_classification_rule(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Response {
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    if let Some(resp) = authorize(&st, &headers, Permission::ManageConfig) {
        return resp;
    }
    match admin.classification.delete_rule(id).await {
        Ok(true) => {
            reload_classifier(admin).await;
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(false) => not_found("rule_not_found", format!("no classification rule {id}")),
        Err(e) => {
            tracing::error!(error = %e, "delete classification rule failed");
            internal("failed to delete classification rule")
        }
    }
}

/// Reload the in-memory classifier after a rule edit so the change takes effect immediately
/// (best-effort: the periodic refresh would pick it up anyway, so a failure only delays it).
async fn reload_classifier(admin: &AdminState) {
    if let Err(e) = admin.classifier.reload(&admin.classification).await {
        tracing::warn!(error = %e, "failed to reload classifier after rule edit");
    }
}

// ── Collection sets (what to poll, per profile/node) — ManageConfig only ─────

/// Rate-window for interface utilization (seconds). Matches the TSDB query-time rate()
/// derivation (ADR-012); 5 min covers a few poll intervals so a single missed poll doesn't
/// blank the rate.
const DEFAULT_RATE_LOOKBACK_SECS: u64 = 300;
/// An interface not refreshed within this window is flagged stale (its metadata is old).
const INTERFACE_STALE_SECS: i64 = 900;

/// A dotted numeric OID, e.g. `1.3.6.1.2.1.1.3.0`. Validated at the edge so an OID can't be
/// interpolated into an SNMP request as anything but digits and dots (security.md).
fn is_valid_oid(oid: &str) -> bool {
    !oid.is_empty()
        && oid
            .split('.')
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

/// Create/update body for a collection item.
#[derive(Deserialize)]
struct CreateCollectionItem {
    metric_name: String,
    oid: String,
    collection: String,
    metric_kind: String,
    enabled: Option<bool>,
}

/// Query for the node collection list: `?resolved=true` returns the effective set.
#[derive(Deserialize)]
struct CollectionQuery {
    resolved: Option<bool>,
}

/// One interface row for the node-detail Interfaces tab: stored metadata joined with
/// query-time rate()/latest() metrics. Utilization is derived here (never stored, ADR-012).
#[derive(Serialize)]
struct InterfaceRow {
    ifindex: u32,
    if_name: Option<String>,
    if_alias: Option<String>,
    if_speed_bps: Option<i64>,
    oper_status: Option<f64>,
    in_bps: Option<f64>,
    out_bps: Option<f64>,
    in_util_pct: Option<f64>,
    out_util_pct: Option<f64>,
    last_seen_unix: Option<i64>,
    stale: bool,
}

async fn list_node_collection(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Path(node_id): Path<Uuid>,
    Query(q): Query<CollectionQuery>,
) -> Response {
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    if let Some(resp) = authorize(&st, &headers, Permission::ManageConfig) {
        return resp;
    }
    if q.resolved.unwrap_or(false) {
        // Effective set: profile defaults overridden by node-level items.
        let profile = match admin.repo.get_node(node_id).await {
            Ok(Some(node)) => node.profile.map(|p| p.0),
            Ok(None) => return not_found("node_not_found", format!("no node {node_id}")),
            Err(e) => {
                tracing::error!(error = %e, "get node failed");
                return internal("failed to load node");
            }
        };
        match admin.collection.list_items_for_node(node_id, profile).await {
            Ok(scoped) => Json(resolve_collection_set(&scoped)).into_response(),
            Err(e) => {
                tracing::error!(error = %e, "resolve collection failed");
                internal("failed to resolve collection set")
            }
        }
    } else {
        match admin.collection.list_items("node", node_id).await {
            Ok(list) => Json(list).into_response(),
            Err(e) => {
                tracing::error!(error = %e, "list node collection failed");
                internal("failed to list collection set")
            }
        }
    }
}

async fn create_node_collection(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Path(node_id): Path<Uuid>,
    Json(body): Json<CreateCollectionItem>,
) -> Response {
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    if let Some(resp) = authorize(&st, &headers, Permission::ManageConfig) {
        return resp;
    }
    create_collection_item(admin, "node", node_id, body).await
}

/// Validate a collection-item body at the API edge; `Some(resp)` short-circuits with 400.
fn validate_collection_item(body: &CreateCollectionItem) -> Option<Response> {
    if !is_valid_metric_name(&body.metric_name) {
        return Some(error_response(
            StatusCode::BAD_REQUEST,
            "invalid_metric_name",
            "metric_name must be a valid identifier".to_owned(),
        ));
    }
    if !is_valid_oid(&body.oid) {
        return Some(error_response(
            StatusCode::BAD_REQUEST,
            "invalid_oid",
            "oid must be a dotted numeric OID".to_owned(),
        ));
    }
    if !matches!(body.collection.as_str(), "scalar" | "table")
        || !matches!(body.metric_kind.as_str(), "gauge" | "counter")
    {
        return Some(error_response(
            StatusCode::BAD_REQUEST,
            "invalid_collection_item",
            "collection must be scalar|table and metric_kind gauge|counter".to_owned(),
        ));
    }
    None
}

/// Validate + create a node-scope collection item (the only direct-scope create now).
async fn create_collection_item(
    admin: &AdminState,
    scope_level: &str,
    scope_id: Uuid,
    body: CreateCollectionItem,
) -> Response {
    if let Some(resp) = validate_collection_item(&body) {
        return resp;
    }
    match admin
        .collection
        .create_item(
            scope_level,
            scope_id,
            &body.metric_name,
            &body.oid,
            &body.collection,
            &body.metric_kind,
            body.enabled.unwrap_or(true),
        )
        .await
    {
        Ok(id) => (StatusCode::CREATED, Json(serde_json::json!({ "id": id }))).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "create collection item failed");
            internal("failed to create collection item")
        }
    }
}

// ── Collection templates (reusable metric bundles) — ManageConfig only ───────

/// Create-template body.
#[derive(Deserialize)]
struct CreateTemplate {
    name: String,
    description: Option<String>,
}

/// Replace-all body for a profile's attached templates.
#[derive(Deserialize)]
struct SetProfileTemplates {
    template_ids: Vec<Uuid>,
}

async fn list_collection_templates(State(st): State<ApiState>, headers: HeaderMap) -> Response {
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    if let Some(resp) = authorize(&st, &headers, Permission::ManageConfig) {
        return resp;
    }
    match admin.collection.list_templates().await {
        Ok(list) => Json(list).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "list templates failed");
            internal("failed to list collection templates")
        }
    }
}

async fn create_collection_template(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Json(body): Json<CreateTemplate>,
) -> Response {
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    if let Some(resp) = authorize(&st, &headers, Permission::ManageConfig) {
        return resp;
    }
    if body.name.trim().is_empty() {
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_name",
            "template name must not be empty".to_owned(),
        );
    }
    match admin
        .collection
        .create_template(body.name.trim(), body.description.as_deref())
        .await
    {
        Ok(CreateTemplateOutcome::Created(id)) => {
            (StatusCode::CREATED, Json(serde_json::json!({ "id": id }))).into_response()
        }
        Ok(CreateTemplateOutcome::NameTaken) => error_response(
            StatusCode::CONFLICT,
            "template_name_taken",
            "a template with that name already exists".to_owned(),
        ),
        Err(e) => {
            tracing::error!(error = %e, "create template failed");
            internal("failed to create collection template")
        }
    }
}

async fn delete_collection_template(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Response {
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    if let Some(resp) = authorize(&st, &headers, Permission::ManageConfig) {
        return resp;
    }
    match admin.collection.delete_template(id).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => not_found("template_not_found", format!("no template {id}")),
        Err(e) => {
            tracing::error!(error = %e, "delete template failed");
            internal("failed to delete collection template")
        }
    }
}

async fn list_template_items(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Response {
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    if let Some(resp) = authorize(&st, &headers, Permission::ManageConfig) {
        return resp;
    }
    match admin.collection.list_template_items(id).await {
        Ok(list) => Json(list).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "list template items failed");
            internal("failed to list template items")
        }
    }
}

async fn create_template_item(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<CreateCollectionItem>,
) -> Response {
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    if let Some(resp) = authorize(&st, &headers, Permission::ManageConfig) {
        return resp;
    }
    if let Some(resp) = validate_collection_item(&body) {
        return resp;
    }
    match admin
        .collection
        .create_template_item(
            id,
            &body.metric_name,
            &body.oid,
            &body.collection,
            &body.metric_kind,
            body.enabled.unwrap_or(true),
        )
        .await
    {
        Ok(item_id) => (
            StatusCode::CREATED,
            Json(serde_json::json!({ "id": item_id })),
        )
            .into_response(),
        Err(e) => {
            tracing::error!(error = %e, "create template item failed");
            internal("failed to create template item")
        }
    }
}

async fn delete_template_item(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Path((id, item_id)): Path<(Uuid, Uuid)>,
) -> Response {
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    if let Some(resp) = authorize(&st, &headers, Permission::ManageConfig) {
        return resp;
    }
    match admin.collection.delete_template_item(id, item_id).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => not_found(
            "template_item_not_found",
            format!("no item {item_id} in template {id}"),
        ),
        Err(e) => {
            tracing::error!(error = %e, "delete template item failed");
            internal("failed to delete template item")
        }
    }
}

async fn list_profile_templates(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Response {
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    if let Some(resp) = authorize(&st, &headers, Permission::ManageConfig) {
        return resp;
    }
    match admin.collection.list_profile_templates(id).await {
        Ok(list) => Json(list).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "list profile templates failed");
            internal("failed to list profile templates")
        }
    }
}

async fn set_profile_templates(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<SetProfileTemplates>,
) -> Response {
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    if let Some(resp) = authorize(&st, &headers, Permission::ManageConfig) {
        return resp;
    }
    match admin
        .collection
        .set_profile_templates(id, &body.template_ids)
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => {
            tracing::error!(error = %e, "set profile templates failed");
            internal("failed to set profile templates")
        }
    }
}

async fn delete_collection_item(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Path(item_id): Path<Uuid>,
) -> Response {
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    if let Some(resp) = authorize(&st, &headers, Permission::ManageConfig) {
        return resp;
    }
    match admin.collection.delete_item(item_id).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => not_found(
            "collection_item_not_found",
            format!("no collection item {item_id}"),
        ),
        Err(e) => {
            tracing::error!(error = %e, "delete collection item failed");
            internal("failed to delete collection item")
        }
    }
}

/// Interfaces discovered on a node, with query-time utilization. Read endpoint; empty in
/// skeleton mode (interface inventory is PostgreSQL-only).
async fn list_node_interfaces(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Path(node_id): Path<Uuid>,
) -> Response {
    if let Some(resp) = require_view(&st, &headers) {
        return resp;
    }
    let Some(admin) = st.admin.as_ref() else {
        return Json(Vec::<InterfaceRow>::new()).into_response();
    };
    let metas = match admin.repo.list_interfaces(node_id).await {
        Ok(m) => m,
        Err(e) => {
            tracing::error!(error = %e, "list interfaces failed");
            return internal("failed to list interfaces");
        }
    };
    let node = NodeId::from(node_id);
    let now = now_unix_s();
    let mut out = Vec::with_capacity(metas.len());
    for m in metas {
        let ifindex = u32::try_from(m.ifindex).unwrap_or(0);
        let key = |metric: &str| SeriesKey::interface(node, IfIndex(ifindex), metric);
        // Octet counters are bytes/sec via rate(); ×8 ⇒ bits/sec.
        let in_bps = st
            .store
            .rate(&key("if_hc_in_octets"), DEFAULT_RATE_LOOKBACK_SECS)
            .await
            .map(|r| r * 8.0);
        let out_bps = st
            .store
            .rate(&key("if_hc_out_octets"), DEFAULT_RATE_LOOKBACK_SECS)
            .await
            .map(|r| r * 8.0);
        let oper_status = st.store.latest(&key("if_oper_status")).await;
        let speed = m.if_speed.filter(|s| *s > 0);
        let util = |bps: Option<f64>| match (bps, speed) {
            (Some(b), Some(s)) => Some(b / s as f64 * 100.0),
            _ => None,
        };
        let stale = m.last_seen_s.is_none_or(|s| now - s > INTERFACE_STALE_SECS);
        out.push(InterfaceRow {
            ifindex,
            if_name: m.if_name,
            if_alias: m.if_alias,
            if_speed_bps: m.if_speed,
            oper_status,
            in_bps,
            out_bps,
            in_util_pct: util(in_bps),
            out_util_pct: util(out_bps),
            last_seen_unix: m.last_seen_s,
            stale,
        });
    }
    Json(out).into_response()
}

/// Per-interface time-series for the node-detail Interfaces detail pane: In/Out throughput
/// (bits/sec, from `rate()` of the octet counters) and In/Out errors (per second). All four
/// share one `timestamps` x-axis (the union of returned points; gaps → null) so the chart
/// gets aligned series. Derived at query time (ADR-012); empty when there's no history.
#[derive(Serialize)]
struct InterfaceSeries {
    timestamps: Vec<i64>,
    in_bps: Vec<Option<f64>>,
    out_bps: Vec<Option<f64>>,
    in_errors: Vec<Option<f64>>,
    out_errors: Vec<Option<f64>>,
}

async fn get_interface_series(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Path((node_id, ifindex)): Path<(Uuid, u32)>,
    Query(q): Query<RangeQuery>,
) -> Response {
    if let Some(resp) = require_view(&st, &headers) {
        return resp;
    }
    let node = NodeId::from(node_id);
    let key = |metric: &str| SeriesKey::interface(node, IfIndex(ifindex), metric);
    let to = q.to.unwrap_or_else(now_unix_s);
    let from = q.from.unwrap_or(to - DEFAULT_RANGE_SECS);
    // Default to ~120 points across the window; the rate lookback spans a few steps so a
    // single missed poll doesn't punch a hole in the line.
    let span = u64::try_from((to - from).max(1)).unwrap_or(DEFAULT_RANGE_SECS as u64);
    let step = q.step.unwrap_or((span / 120).max(60)).max(1);
    let lookback = (step * 4).max(DEFAULT_RATE_LOOKBACK_SECS);

    let in_oct = st
        .store
        .rate_range(&key("if_hc_in_octets"), from, to, step, lookback)
        .await;
    let out_oct = st
        .store
        .rate_range(&key("if_hc_out_octets"), from, to, step, lookback)
        .await;
    let in_err = st
        .store
        .rate_range(&key("if_in_errors"), from, to, step, lookback)
        .await;
    let out_err = st
        .store
        .rate_range(&key("if_out_errors"), from, to, step, lookback)
        .await;

    // Shared x-axis = the union of all returned timestamps; align each series onto it.
    let mut grid_set: std::collections::BTreeSet<i64> = std::collections::BTreeSet::new();
    for s in [&in_oct, &out_oct, &in_err, &out_err] {
        for p in s {
            grid_set.insert(p.t);
        }
    }
    let grid: Vec<i64> = grid_set.into_iter().collect();
    let align = |pts: &[MetricPoint], scale: f64| -> Vec<Option<f64>> {
        let m: std::collections::HashMap<i64, f64> = pts.iter().map(|p| (p.t, p.v)).collect();
        grid.iter().map(|t| m.get(t).map(|v| v * scale)).collect()
    };

    Json(InterfaceSeries {
        in_bps: align(&in_oct, 8.0),
        out_bps: align(&out_oct, 8.0),
        in_errors: align(&in_err, 1.0),
        out_errors: align(&out_err, 1.0),
        timestamps: grid,
    })
    .into_response()
}

// ── Users & roles (Settings ▸ Users & roles) — ManageUsers (admin) only ──────

/// Minimum password length accepted for a new account or a reset.
const MIN_PASSWORD_LEN: usize = 8;

/// A valid role string (mirrors `yagra_common::Role`, snake_case).
fn is_valid_role(role: &str) -> bool {
    matches!(role, "viewer" | "operator" | "admin")
}

async fn list_users(State(st): State<ApiState>, headers: HeaderMap) -> Response {
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    if let Some(resp) = authorize(&st, &headers, Permission::ManageUsers) {
        return resp;
    }
    match admin.users.list().await {
        Ok(list) => Json(list).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "list users failed");
            internal("failed to list users")
        }
    }
}

/// Create-user request body. The password is hashed before storage and never logged.
#[derive(Deserialize)]
struct CreateUser {
    username: String,
    password: String,
    role: String,
}

async fn create_user(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Json(body): Json<CreateUser>,
) -> Response {
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    if let Some(resp) = authorize(&st, &headers, Permission::ManageUsers) {
        return resp;
    }
    let username = body.username.trim();
    if username.is_empty() {
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_user",
            "username must not be empty".to_owned(),
        );
    }
    if body.password.len() < MIN_PASSWORD_LEN {
        return error_response(
            StatusCode::BAD_REQUEST,
            "weak_password",
            format!("password must be at least {MIN_PASSWORD_LEN} characters"),
        );
    }
    if !is_valid_role(&body.role) {
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_role",
            "role must be viewer, operator, or admin".to_owned(),
        );
    }
    match admin
        .users
        .create(username, &body.password, &body.role)
        .await
    {
        Ok(UserCreateOutcome::Created(id)) => {
            (StatusCode::CREATED, Json(serde_json::json!({ "id": id }))).into_response()
        }
        Ok(UserCreateOutcome::UsernameTaken) => error_response(
            StatusCode::CONFLICT,
            "username_taken",
            format!("username {username:?} is already taken"),
        ),
        Err(e) => {
            tracing::error!(error = %e, "create user failed");
            internal("failed to create user")
        }
    }
}

async fn delete_user(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Response {
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    if let Some(resp) = authorize(&st, &headers, Permission::ManageUsers) {
        return resp;
    }
    match admin.users.delete(id).await {
        Ok(UserMutation::Done) => StatusCode::NO_CONTENT.into_response(),
        Ok(UserMutation::NotFound) => not_found("user_not_found", format!("no user {id}")),
        Ok(UserMutation::LastAdmin) => last_admin(),
        Err(e) => {
            tracing::error!(error = %e, "delete user failed");
            internal("failed to delete user")
        }
    }
}

/// Change-role request body.
#[derive(Deserialize)]
struct SetRole {
    role: String,
}

async fn set_user_role(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<SetRole>,
) -> Response {
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    if let Some(resp) = authorize(&st, &headers, Permission::ManageUsers) {
        return resp;
    }
    if !is_valid_role(&body.role) {
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_role",
            "role must be viewer, operator, or admin".to_owned(),
        );
    }
    match admin.users.set_role(id, &body.role).await {
        Ok(UserMutation::Done) => StatusCode::NO_CONTENT.into_response(),
        Ok(UserMutation::NotFound) => not_found("user_not_found", format!("no user {id}")),
        Ok(UserMutation::LastAdmin) => last_admin(),
        Err(e) => {
            tracing::error!(error = %e, "set user role failed");
            internal("failed to update user role")
        }
    }
}

/// Enable/disable-account request body.
#[derive(Deserialize)]
struct SetStatus {
    enabled: bool,
}

async fn set_user_status(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<SetStatus>,
) -> Response {
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    if let Some(resp) = authorize(&st, &headers, Permission::ManageUsers) {
        return resp;
    }
    match admin.users.set_enabled(id, body.enabled).await {
        Ok(UserMutation::Done) => StatusCode::NO_CONTENT.into_response(),
        Ok(UserMutation::NotFound) => not_found("user_not_found", format!("no user {id}")),
        Ok(UserMutation::LastAdmin) => last_admin(),
        Err(e) => {
            tracing::error!(error = %e, "set user status failed");
            internal("failed to update user status")
        }
    }
}

/// Reset-password request body. The password is hashed before storage and never logged.
#[derive(Deserialize)]
struct SetPassword {
    password: String,
}

async fn set_user_password(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<SetPassword>,
) -> Response {
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    if let Some(resp) = authorize(&st, &headers, Permission::ManageUsers) {
        return resp;
    }
    if body.password.len() < MIN_PASSWORD_LEN {
        return error_response(
            StatusCode::BAD_REQUEST,
            "weak_password",
            format!("password must be at least {MIN_PASSWORD_LEN} characters"),
        );
    }
    match admin.users.set_password(id, &body.password).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => not_found("user_not_found", format!("no user {id}")),
        Err(e) => {
            tracing::error!(error = %e, "reset user password failed");
            internal("failed to reset password")
        }
    }
}

/// 409 for the last-admin lock-out guard (delete or demote of the only admin).
fn last_admin() -> Response {
    error_response(
        StatusCode::CONFLICT,
        "last_admin",
        "cannot remove, demote, or disable the last admin account".to_owned(),
    )
}

// ── Audit log ────────────────────────────────────────────────────────────────

/// Audit listing query: page size + a keyset cursor (rows strictly older than `before`).
#[derive(Deserialize)]
struct AuditQuery {
    limit: Option<i64>,
    before: Option<String>,
}

async fn list_audit(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Query(q): Query<AuditQuery>,
) -> Response {
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    // Admin-only: the audit log exposes who did what across the whole system.
    if let Some(resp) = authorize(&st, &headers, Permission::ViewAudit) {
        return resp;
    }
    let before = match q.before.as_deref() {
        Some(s) => match parse_rfc3339(s) {
            Some(t) => Some(t),
            None => {
                return error_response(
                    StatusCode::BAD_REQUEST,
                    "invalid_cursor",
                    "before must be an RFC 3339 timestamp".to_owned(),
                )
            }
        },
        None => None,
    };
    match admin
        .audit
        .list(q.limit.unwrap_or(crate::audit::DEFAULT_LIMIT), before)
        .await
    {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "list audit log failed");
            internal("failed to list the audit log")
        }
    }
}

// ── My Dashboard (per-user widget layout) ────────────────────────────────────

/// Max accepted size of a saved dashboard layout (serialized JSON). A single board is a few KB;
/// the My Dashboard / Shared Dashboard documents now hold multiple boards, so this allows several
/// while still capping abuse. Enforced at the edge before the DB.
const MAX_DASHBOARD_BYTES: usize = 262_144;

/// Resolve the caller's session (a *real* authenticated user — public-dashboard mode does not
/// apply here, since "My Dashboard" is inherently per-account). Returns the session or a boxed
/// error response to short-circuit with (boxed so the `Ok` path stays cheap — `clippy::result_large_err`).
fn require_session(
    st: &ApiState,
    headers: &HeaderMap,
) -> Result<crate::auth::Session, Box<Response>> {
    st.sessions
        .authorize(bearer(headers), Permission::View)
        .map_err(|e| {
            Box::new(match e {
                AuthError::Forbidden => error_response(
                    StatusCode::FORBIDDEN,
                    "forbidden",
                    "your role does not permit this action".to_owned(),
                ),
                _ => error_response(
                    StatusCode::UNAUTHORIZED,
                    "unauthorized",
                    "a valid bearer token is required".to_owned(),
                ),
            })
        })
}

/// The caller's saved dashboard layout, or `null` when they have never saved one (the WebUI
/// then renders its default layout). Always scoped to the authenticated caller.
async fn get_dashboard(State(st): State<ApiState>, headers: HeaderMap) -> Response {
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    let session = match require_session(&st, &headers) {
        Ok(s) => s,
        Err(resp) => return *resp,
    };
    match admin.dashboards.get_for_user(&session.username).await {
        // No saved layout ⇒ explicit JSON null so the client falls back to its default.
        Ok(layout) => Json(layout.unwrap_or(serde_json::Value::Null)).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "get dashboard failed");
            internal("failed to load the dashboard layout")
        }
    }
}

/// Save (replace) the caller's dashboard layout. The body is an opaque JSON object — the
/// backend never interprets widget types (the WebUI owns and migrates the shape). Mutating, so
/// the audit middleware records it automatically.
async fn put_dashboard(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    let session = match require_session(&st, &headers) {
        Ok(s) => s,
        Err(resp) => return *resp,
    };
    // Must be a JSON object (the layout document), and within the size cap.
    if !body.is_object() {
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_layout",
            "dashboard layout must be a JSON object".to_owned(),
        );
    }
    if serde_json::to_vec(&body).map_or(usize::MAX, |v| v.len()) > MAX_DASHBOARD_BYTES {
        return error_response(
            StatusCode::PAYLOAD_TOO_LARGE,
            "layout_too_large",
            format!("dashboard layout exceeds {MAX_DASHBOARD_BYTES} bytes"),
        );
    }
    match admin
        .dashboards
        .upsert_for_user(&session.username, &body)
        .await
    {
        Ok(true) => Json(serde_json::json!({ "ok": true })).into_response(),
        // A valid session whose account vanished mid-request — treat as gone.
        Ok(false) => not_found("user_not_found", "no such user account".to_owned()),
        Err(e) => {
            tracing::error!(error = %e, "save dashboard failed");
            internal("failed to save the dashboard layout")
        }
    }
}

// ── Shared Dashboard (one global widget layout, admin-edited) ─────────────────

/// The saved global Shared Dashboard layout, or `null` when no admin has saved one (the WebUI then
/// renders its default). Open-read (like the other dashboard reads) so it works in public-dashboard
/// mode; the *write* side is admin-only.
async fn get_shared_dashboard(State(st): State<ApiState>, headers: HeaderMap) -> Response {
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    if let Some(resp) = require_view(&st, &headers) {
        return resp;
    }
    match admin.shared_dashboard.get_shared().await {
        // No saved layout ⇒ explicit JSON null so the client falls back to its default.
        Ok(layout) => Json(layout.unwrap_or(serde_json::Value::Null)).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "get shared dashboard failed");
            internal("failed to load the shared dashboard layout")
        }
    }
}

/// Save (replace) the global Shared Dashboard layout — **admin only**: the change applies to every
/// user. The body is an opaque JSON object (the WebUI owns and migrates the shape). Mutating, so the
/// audit middleware records it automatically.
///
/// Authorization is checked **first** (before the `admin`/503 fallback) so the RBAC decision is
/// testable without a DB and a forbidden caller never learns whether the admin backend is
/// configured — a deliberate divergence from `put_dashboard`'s order.
async fn put_shared_dashboard(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> Response {
    // Admin-only: ManageConfig is granted to the Admin role alone (rbac.rs).
    if let Some(resp) = authorize(&st, &headers, Permission::ManageConfig) {
        return resp;
    }
    // Recover the caller's username for attribution (it just passed `authorize`, so this succeeds).
    let session = match require_session(&st, &headers) {
        Ok(s) => s,
        Err(resp) => return *resp,
    };
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    // Must be a JSON object (the layout document), and within the size cap.
    if !body.is_object() {
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_layout",
            "dashboard layout must be a JSON object".to_owned(),
        );
    }
    if serde_json::to_vec(&body).map_or(usize::MAX, |v| v.len()) > MAX_DASHBOARD_BYTES {
        return error_response(
            StatusCode::PAYLOAD_TOO_LARGE,
            "layout_too_large",
            format!("dashboard layout exceeds {MAX_DASHBOARD_BYTES} bytes"),
        );
    }
    match admin
        .shared_dashboard
        .upsert_shared(&body, &session.username)
        .await
    {
        Ok(()) => Json(serde_json::json!({ "ok": true })).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "save shared dashboard failed");
            internal("failed to save the shared dashboard layout")
        }
    }
}

// ── Maintenance windows + mutes ──────────────────────────────────────────────

/// Parse an RFC 3339 timestamp from the API edge into UTC.
fn parse_rfc3339(s: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|t| t.with_timezone(&chrono::Utc))
}

async fn list_maintenance_windows(State(st): State<ApiState>, headers: HeaderMap) -> Response {
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    if let Some(resp) = require_view(&st, &headers) {
        return resp;
    }
    match admin.maintenance.list_windows().await {
        Ok(list) => Json(list).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "list maintenance windows failed");
            internal("failed to list maintenance windows")
        }
    }
}

/// Create-window body. Times are RFC 3339; the scope mirrors thresholds (ADR-013) plus
/// `group_id` (a folder-group UUID, resolved recursively — the All Nodes right-click scope).
#[derive(Deserialize)]
struct CreateWindow {
    name: String,
    scope_level: String,
    scope_id: String,
    starts_at: String,
    ends_at: String,
}

/// Whether `group_id` names an existing folder group. `None` = exists; `Some(resp)` = the error
/// response to return (bad UUID, missing group, or DB failure). Shared by window + mute creation.
async fn validate_group_scope(admin: &AdminState, scope_id: &str) -> Option<Response> {
    let Ok(gid) = Uuid::parse_str(scope_id.trim()) else {
        return Some(error_response(
            StatusCode::BAD_REQUEST,
            "invalid_scope",
            "scope_id must be a group UUID for a folder-group scope".to_owned(),
        ));
    };
    match admin.groups.edges().await {
        Ok(edges) if edges.iter().any(|(id, _)| *id == gid) => None,
        Ok(_) => Some(not_found("group_not_found", format!("no group {gid}"))),
        Err(e) => {
            tracing::error!(error = %e, "validate group scope failed");
            Some(internal("failed to validate group scope"))
        }
    }
}

async fn create_maintenance_window(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Json(body): Json<CreateWindow>,
) -> Response {
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    if let Some(resp) = authorize(&st, &headers, Permission::ManageMaintenance) {
        return resp;
    }
    if body.name.trim().is_empty()
        || body.scope_id.trim().is_empty()
        || !matches!(
            body.scope_level.as_str(),
            "profile" | "group" | "node" | "group_id"
        )
    {
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_window",
            "name/scope_id must not be empty; scope_level must be profile|group|node|group_id"
                .to_owned(),
        );
    }
    // A folder-group scope (`group_id`) must reference an existing group.
    if body.scope_level == "group_id" {
        if let Some(resp) = validate_group_scope(admin, &body.scope_id).await {
            return resp;
        }
    }
    let (Some(starts), Some(ends)) = (parse_rfc3339(&body.starts_at), parse_rfc3339(&body.ends_at))
    else {
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_window",
            "starts_at/ends_at must be RFC 3339 timestamps".to_owned(),
        );
    };
    if ends <= starts {
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_window",
            "ends_at must be after starts_at".to_owned(),
        );
    }
    match admin
        .maintenance
        .create_window(
            body.name.trim(),
            &body.scope_level,
            body.scope_id.trim(),
            starts,
            ends,
        )
        .await
    {
        Ok(id) => (StatusCode::CREATED, Json(serde_json::json!({ "id": id }))).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "create maintenance window failed");
            internal("failed to create maintenance window")
        }
    }
}

async fn set_maintenance_window_enabled(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<EnabledBody>,
) -> Response {
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    if let Some(resp) = authorize(&st, &headers, Permission::ManageMaintenance) {
        return resp;
    }
    match admin.maintenance.set_window_enabled(id, body.enabled).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => not_found("window_not_found", format!("no maintenance window {id}")),
        Err(e) => {
            tracing::error!(error = %e, "update maintenance window failed");
            internal("failed to update maintenance window")
        }
    }
}

async fn delete_maintenance_window(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Response {
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    if let Some(resp) = authorize(&st, &headers, Permission::ManageMaintenance) {
        return resp;
    }
    match admin.maintenance.delete_window(id).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => not_found("window_not_found", format!("no maintenance window {id}")),
        Err(e) => {
            tracing::error!(error = %e, "delete maintenance window failed");
            internal("failed to delete maintenance window")
        }
    }
}

async fn list_mutes(State(st): State<ApiState>, headers: HeaderMap) -> Response {
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    if let Some(resp) = require_view(&st, &headers) {
        return resp;
    }
    match admin.maintenance.list_mutes().await {
        Ok(list) => Json(list).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "list mutes failed");
            internal("failed to list mutes")
        }
    }
}

/// Create-mute body. `scope_kind` is `node` (silence one node, optionally one `metric_name`) or
/// `group` (silence every node under a folder group, recursive — `metric_name` is ignored);
/// `scope_id` is the node/group UUID. `until` is RFC 3339.
#[derive(Deserialize)]
struct CreateMute {
    scope_kind: String,
    scope_id: Uuid,
    metric_name: Option<String>,
    until: String,
    reason: Option<String>,
}

async fn create_mute(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Json(body): Json<CreateMute>,
) -> Response {
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    // Mutes are an operational action (Operator and up), not a config change.
    if let Some(resp) = authorize(&st, &headers, Permission::AckAlerts) {
        return resp;
    }
    if !matches!(body.scope_kind.as_str(), "node" | "group") {
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_mute",
            "scope_kind must be node|group".to_owned(),
        );
    }
    let group = body.scope_kind == "group";
    // A group mute silences the whole node-set, so a per-metric mute only applies to a node scope.
    let check = if group {
        None
    } else {
        body.metric_name
            .as_deref()
            .map(str::trim)
            .filter(|c| !c.is_empty())
    };
    if let Some(check) = check {
        if !is_valid_metric_name(check) {
            return error_response(
                StatusCode::BAD_REQUEST,
                "invalid_mute",
                "metric_name must be a valid metric name (or omitted for the whole node)"
                    .to_owned(),
            );
        }
    }
    // A group mute must reference an existing folder group.
    if group {
        if let Some(resp) = validate_group_scope(admin, &body.scope_id.to_string()).await {
            return resp;
        }
    }
    let Some(until) = parse_rfc3339(&body.until) else {
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_mute",
            "until must be an RFC 3339 timestamp".to_owned(),
        );
    };
    if until <= chrono::Utc::now() {
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_mute",
            "until must be in the future".to_owned(),
        );
    }
    match admin
        .maintenance
        .create_mute(
            &body.scope_kind,
            body.scope_id,
            check,
            until,
            body.reason.as_deref(),
        )
        .await
    {
        Ok(id) => (StatusCode::CREATED, Json(serde_json::json!({ "id": id }))).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "create mute failed");
            internal("failed to create mute")
        }
    }
}

async fn delete_mute(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Response {
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    if let Some(resp) = authorize(&st, &headers, Permission::AckAlerts) {
        return resp;
    }
    match admin.maintenance.delete_mute(id).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => not_found("mute_not_found", format!("no mute {id}")),
        Err(e) => {
            tracing::error!(error = %e, "delete mute failed");
            internal("failed to delete mute")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::StaticNodeList;
    use crate::sink::InMemorySink;
    use axum::body::{to_bytes, Body};
    use axum::http::Request;
    use tower::ServiceExt; // for `oneshot`
    use yagra_bus::{CheckOutcome, PollResult, Sample};

    fn state_with(store: Arc<dyn MetricStore>) -> ApiState {
        // Public-dashboard mode: read endpoints are open (no token required).
        ApiState {
            store,
            nodes: Arc::new(StaticNodeList::demo()),
            alerts: Arc::new(AlertManager::new()),
            admin: None,
            sessions: Arc::new(SessionStore::new()),
            history: None,
            public_dashboard: true,
        }
    }

    /// A private (auth-required) state plus a freshly issued Viewer token for it.
    fn private_state_with(store: Arc<dyn MetricStore>) -> (ApiState, String) {
        use yagra_common::{Principal, Role, Scope};
        let sessions = Arc::new(SessionStore::new());
        let token = sessions.issue(Principal::new(Role::Viewer, Scope::All), "viewer1");
        let state = ApiState {
            store,
            nodes: Arc::new(StaticNodeList::demo()),
            alerts: Arc::new(AlertManager::new()),
            admin: None,
            sessions,
            history: None,
            public_dashboard: false,
        };
        (state, token)
    }

    /// A private (auth-required) state plus a freshly issued token for `role` — for RBAC tests on
    /// admin-only writes (e.g. the Shared Dashboard PUT).
    fn state_with_role_token(role: yagra_common::Role) -> (ApiState, String) {
        use yagra_common::{Principal, Scope};
        let sessions = Arc::new(SessionStore::new());
        let token = sessions.issue(Principal::new(role, Scope::All), "u1");
        let state = ApiState {
            store: Arc::new(InMemorySink::default()),
            nodes: Arc::new(StaticNodeList::demo()),
            alerts: Arc::new(AlertManager::new()),
            admin: None,
            sessions,
            history: None,
            public_dashboard: false,
        };
        (state, token)
    }

    fn store_with_reading(node: NodeId, metric: &str, value: f64) -> Arc<dyn MetricStore> {
        let sink = InMemorySink::default();
        sink.ingest(&PollResult {
            schema_version: 1,
            job_id: Uuid::nil(),
            node_id: node,
            at_unix_ms: 0,
            outcome: CheckOutcome::Reachable,
            samples: vec![Sample::gauge(metric, value)],
            interfaces: Vec::new(),
            sys_descr: None,
        });
        Arc::new(sink)
    }

    async fn body_json(resp: Response) -> serde_json::Value {
        let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    #[tokio::test]
    async fn returns_latest_reading() {
        let node = NodeId::from(Uuid::nil());
        let app = router(state_with(store_with_reading(node, "icmp_rtt_ms", 8.0)));

        let resp = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/v1/nodes/{node}/metrics/icmp_rtt_ms"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert_eq!(json["metric"], "icmp_rtt_ms");
        assert_eq!(json["value"], 8.0);
    }

    #[tokio::test]
    async fn missing_metric_returns_error_envelope() {
        let node = NodeId::from(Uuid::nil());
        let app = router(state_with(Arc::new(InMemorySink::default())));

        let resp = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/v1/nodes/{node}/metrics/icmp_rtt_ms"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let json = body_json(resp).await;
        assert_eq!(json["error"]["code"], "metric_not_found");
    }

    #[test]
    fn metric_name_validation_rejects_injection() {
        assert!(is_valid_metric_name("icmp_rtt_ms"));
        assert!(is_valid_metric_name("_internal:ratio"));
        // PromQL-injection attempts and stray characters are rejected.
        assert!(!is_valid_metric_name("up} or vector(1) #"));
        assert!(!is_valid_metric_name("a b"));
        assert!(!is_valid_metric_name("9starts_with_digit"));
        assert!(!is_valid_metric_name(""));
    }

    #[test]
    fn oid_validation_accepts_dotted_digits_only() {
        assert!(is_valid_oid("1.3.6.1.2.1.31.1.1.1.6"));
        assert!(is_valid_oid("0"));
        // Injection / non-numeric / malformed are rejected.
        assert!(!is_valid_oid("1.3.6.x"));
        assert!(!is_valid_oid("1..3"));
        assert!(!is_valid_oid(".1.3"));
        assert!(!is_valid_oid(""));
        assert!(!is_valid_oid("1.3; DROP"));
    }

    #[tokio::test]
    async fn collection_create_unavailable_without_admin() {
        let node = Uuid::nil();
        let app = router(state_with(Arc::new(InMemorySink::default())));
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/v1/nodes/{node}/collection"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"metric_name":"if_hc_in_octets","oid":"1.3.6.1.2.1.31.1.1.1.6","collection":"table","metric_kind":"counter"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body_json(resp).await["error"]["code"], "admin_unavailable");
    }

    #[tokio::test]
    async fn collection_templates_list_unavailable_without_admin() {
        let app = router(state_with(Arc::new(InMemorySink::default())));
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/collection-templates")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body_json(resp).await["error"]["code"], "admin_unavailable");
    }

    #[tokio::test]
    async fn create_template_unavailable_without_admin() {
        let app = router(state_with(Arc::new(InMemorySink::default())));
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/collection-templates")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"name":"My template"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body_json(resp).await["error"]["code"], "admin_unavailable");
    }

    #[tokio::test]
    async fn set_profile_templates_unavailable_without_admin() {
        let id = Uuid::nil();
        let app = router(state_with(Arc::new(InMemorySink::default())));
        let resp = app
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri(format!("/api/v1/profiles/{id}/templates"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"template_ids":[]}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body_json(resp).await["error"]["code"], "admin_unavailable");
    }

    #[tokio::test]
    async fn node_detail_not_found_without_admin() {
        // Skeleton has no metadata store, so node-detail (bindings) reads 404.
        let node = Uuid::nil();
        let app = router(state_with(Arc::new(InMemorySink::default())));
        let resp = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/v1/nodes/{node}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        assert_eq!(body_json(resp).await["error"]["code"], "node_not_found");
    }

    #[tokio::test]
    async fn poll_now_unavailable_without_admin() {
        // "Poll now" dispatches bus jobs via the admin-only dispatcher; skeleton has none.
        let node = Uuid::nil();
        let app = router(state_with(Arc::new(InMemorySink::default())));
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/v1/nodes/{node}/poll"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body_json(resp).await["error"]["code"], "admin_unavailable");
    }

    #[tokio::test]
    async fn interfaces_empty_array_without_admin() {
        // Interface inventory is PostgreSQL-only; skeleton returns an empty array, not 503.
        let node = Uuid::nil();
        let app = router(state_with(Arc::new(InMemorySink::default())));
        let resp = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/v1/nodes/{node}/interfaces"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert!(json.is_array());
        assert_eq!(json.as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn interface_series_returns_aligned_arrays() {
        // No history (in-memory store) ⇒ 200 with empty, aligned series arrays.
        let node = Uuid::nil();
        let app = router(state_with(Arc::new(InMemorySink::default())));
        let resp = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/v1/nodes/{node}/interfaces/3/series"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert!(json["timestamps"].is_array());
        assert_eq!(json["timestamps"].as_array().unwrap().len(), 0);
        assert!(json["in_bps"].is_array());
        assert!(json["out_errors"].is_array());
    }

    #[tokio::test]
    async fn invalid_metric_name_returns_bad_request() {
        let node = NodeId::from(Uuid::nil());
        let app = router(state_with(Arc::new(InMemorySink::default())));
        let resp = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/v1/nodes/{node}/metrics/bad%20name"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let json = body_json(resp).await;
        assert_eq!(json["error"]["code"], "invalid_metric_name");
    }

    #[tokio::test]
    async fn lists_nodes_with_derived_state() {
        let node = NodeId::from(Uuid::nil());
        // Demo node has a live RTT ⇒ state "ok".
        let app = router(state_with(store_with_reading(node, "icmp_rtt_ms", 8.0)));
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/nodes")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert_eq!(json["nodes"][0]["id"], node.to_string());
        assert_eq!(json["nodes"][0]["state"], "ok");
    }

    #[tokio::test]
    async fn node_without_reading_is_unknown() {
        let app = router(state_with(Arc::new(InMemorySink::default())));
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/nodes")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let json = body_json(resp).await;
        assert_eq!(json["nodes"][0]["state"], "unknown");
    }

    #[tokio::test]
    async fn range_returns_points_array() {
        let node = NodeId::from(Uuid::nil());
        // In-memory store has no history, so points is an empty array (not an error).
        let app = router(state_with(Arc::new(InMemorySink::default())));
        let resp = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/v1/nodes/{node}/metrics/icmp_rtt_ms/range"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert_eq!(json["metric"], "icmp_rtt_ms");
        assert!(json["points"].is_array());
    }

    #[tokio::test]
    async fn create_node_unavailable_without_admin() {
        // Skeleton mode (admin: None) rejects writes with 503 rather than 404/500.
        let app = router(state_with(Arc::new(InMemorySink::default())));
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/nodes")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"name":"r1","address":"10.0.0.1"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        let json = body_json(resp).await;
        assert_eq!(json["error"]["code"], "admin_unavailable");
    }

    #[tokio::test]
    async fn node_status_reports_state_and_alerts() {
        let node = NodeId::from(Uuid::nil());
        // Demo node has a live RTT but no alert-engine activity ⇒ fallback state "ok",
        // and no active alerts attributed to it.
        let app = router(state_with(store_with_reading(node, "icmp_rtt_ms", 8.0)));
        let resp = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/v1/nodes/{node}/status"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert_eq!(json["node_id"], node.to_string());
        assert_eq!(json["state"], "ok");
        assert_eq!(json["alerts"].as_array().map(Vec::len), Some(0));
    }

    #[tokio::test]
    async fn alerts_empty_by_default() {
        let app = router(state_with(Arc::new(InMemorySink::default())));
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/alerts")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert_eq!(json.as_array().map(Vec::len), Some(0));
    }

    #[tokio::test]
    async fn private_mode_rejects_unauthenticated_reads() {
        // Default (non-public) mode: listing nodes without a token is 401.
        let (state, _token) = private_state_with(Arc::new(InMemorySink::default()));
        let app = router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/nodes")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        let json = body_json(resp).await;
        assert_eq!(json["error"]["code"], "unauthorized");
    }

    #[tokio::test]
    async fn private_mode_allows_reads_with_viewer_token() {
        let node = NodeId::from(Uuid::nil());
        let (state, token) = private_state_with(store_with_reading(node, "icmp_rtt_ms", 8.0));
        let app = router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/nodes")
                    .header(AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert_eq!(json["nodes"][0]["id"], node.to_string());
    }

    #[tokio::test]
    async fn roles_matrix_lists_roles_and_permissions() {
        // The matrix is a read endpoint: gated like other reads (View), and it reflects grants().
        let (state, token) = private_state_with(Arc::new(InMemorySink::default()));
        let app = router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/roles")
                    .header(AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert_eq!(json["permissions"].as_array().unwrap().len(), 7);
        let roles = json["roles"].as_array().unwrap();
        assert_eq!(roles.len(), 3);
        let admin = roles.iter().find(|r| r["key"] == "admin").unwrap();
        let admin_perms = admin["permissions"].as_array().unwrap();
        assert!(admin_perms.iter().any(|p| p == "manage_users"));
        let viewer = roles.iter().find(|r| r["key"] == "viewer").unwrap();
        assert_eq!(viewer["permissions"].as_array().unwrap(), &vec!["view"]);
    }

    #[tokio::test]
    async fn roles_matrix_requires_auth_in_private_mode() {
        let (state, _token) = private_state_with(Arc::new(InMemorySink::default()));
        let app = router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/roles")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn private_mode_healthz_stays_open() {
        // The liveness probe is never gated, even in private mode.
        let (state, _token) = private_state_with(Arc::new(InMemorySink::default()));
        let app = router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/healthz")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[test]
    fn role_validation_accepts_only_known_roles() {
        assert!(is_valid_role("viewer"));
        assert!(is_valid_role("operator"));
        assert!(is_valid_role("admin"));
        assert!(!is_valid_role("Admin")); // case-sensitive (matches serde snake_case)
        assert!(!is_valid_role("superuser"));
        assert!(!is_valid_role(""));
    }

    #[tokio::test]
    async fn list_users_unavailable_without_admin() {
        // Skeleton mode (admin: None) has no user store, so user management is 503.
        let app = router(state_with(Arc::new(InMemorySink::default())));
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/users")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        let json = body_json(resp).await;
        assert_eq!(json["error"]["code"], "admin_unavailable");
    }

    #[tokio::test]
    async fn set_user_status_unavailable_without_admin() {
        // The enable/disable route is wired; in skeleton mode (admin: None) it is 503.
        let app = router(state_with(Arc::new(InMemorySink::default())));
        let resp = app
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/v1/users/00000000-0000-0000-0000-000000000000/enabled")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"enabled":false}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        let json = body_json(resp).await;
        assert_eq!(json["error"]["code"], "admin_unavailable");
    }

    #[tokio::test]
    async fn create_user_unavailable_without_admin() {
        let app = router(state_with(Arc::new(InMemorySink::default())));
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/users")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"username":"alice","password":"hunter2hunter2","role":"viewer"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        let json = body_json(resp).await;
        assert_eq!(json["error"]["code"], "admin_unavailable");
    }

    #[tokio::test]
    async fn create_node_group_unavailable_without_admin() {
        // The route is wired; in skeleton mode (admin: None) group management is 503.
        let app = router(state_with(Arc::new(InMemorySink::default())));
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/node-groups")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"name":"Tokyo","group_type":"site"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        let json = body_json(resp).await;
        assert_eq!(json["error"]["code"], "admin_unavailable");
    }

    #[tokio::test]
    async fn placement_routes_unavailable_without_admin() {
        // Both drag-reorder routes are wired; in skeleton mode (admin: None) they are 503.
        let app = router(state_with(Arc::new(InMemorySink::default())));
        let node_resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/v1/nodes/00000000-0000-0000-0000-000000000000/placement")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"group_id":null}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(node_resp.status(), StatusCode::SERVICE_UNAVAILABLE);

        let group_resp = app
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/v1/node-groups/00000000-0000-0000-0000-000000000000/placement")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"parent_id":null}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(group_resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn healthz_returns_ok() {
        let app = router(state_with(Arc::new(InMemorySink::default())));
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/healthz")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn top_metrics_empty_on_in_memory_store() {
        // Public-dashboard read; the in-memory sink can't rank a fleet, so the result is [].
        let app = router(state_with(Arc::new(InMemorySink::default())));
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/metrics/top?metric=icmp_rtt_ms")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(body_json(resp).await, serde_json::json!([]));
    }

    #[tokio::test]
    async fn interface_top_empty_on_in_memory_and_rejects_bad_metric() {
        let app = router(state_with(Arc::new(InMemorySink::default())));
        // In-memory store can't rank a fleet ⇒ [].
        let ok = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/metrics/interface-top?metric=throughput")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(ok.status(), StatusCode::OK);
        assert_eq!(body_json(ok).await, serde_json::json!([]));
        // Unknown metric ⇒ 400.
        let bad = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/metrics/interface-top?metric=bogus")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(bad.status(), StatusCode::BAD_REQUEST);
        assert_eq!(body_json(bad).await["error"]["code"], "invalid_metric");
    }

    #[test]
    fn logical_metric_selector_expands_cpu_and_memory() {
        let cpu = logical_metric_selector("cpu").unwrap();
        assert!(cpu.starts_with("{__name__=~\""));
        assert!(cpu.contains("huawei_cpu_usage") && cpu.contains("hr_processor_load"));
        // idle/temperature are intentionally excluded from "busy" CPU ranking
        assert!(!cpu.contains("idle") && !cpu.contains("temp"));
        assert!(logical_metric_selector("memory")
            .unwrap()
            .contains("huawei_mem_usage"));
        assert!(logical_metric_selector("icmp_rtt_ms").is_none());
    }

    #[tokio::test]
    async fn top_metrics_rejects_invalid_metric_and_agg() {
        let app = router(state_with(Arc::new(InMemorySink::default())));
        let bad_metric = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/metrics/top?metric=up}+or+vector(1)")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(bad_metric.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            body_json(bad_metric).await["error"]["code"],
            "invalid_metric_name"
        );

        let bad_agg = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/metrics/top?metric=icmp_rtt_ms&agg=bogus")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(bad_agg.status(), StatusCode::BAD_REQUEST);
        assert_eq!(body_json(bad_agg).await["error"]["code"], "invalid_agg");
    }

    #[tokio::test]
    async fn dashboard_unavailable_without_admin() {
        // Skeleton has no user store; "My Dashboard" persistence needs the admin/DB side.
        let app = router(state_with(Arc::new(InMemorySink::default())));
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/dashboard")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body_json(resp).await["error"]["code"], "admin_unavailable");
    }

    #[test]
    fn require_session_gates_on_a_real_token() {
        // "My Dashboard" is per-account, so its handlers demand a real session even in
        // public-dashboard mode — unlike `require_view`, which is open then.
        let (state, token) = private_state_with(Arc::new(InMemorySink::default()));

        // No bearer ⇒ 401.
        let empty = HeaderMap::new();
        let denied = require_session(&state, &empty).expect_err("no token must be rejected");
        assert_eq!(denied.status(), StatusCode::UNAUTHORIZED); // Box<Response> derefs for .status()

        // Valid bearer ⇒ the caller's session (scopes the layout to this username).
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, format!("Bearer {token}").parse().unwrap());
        let session = require_session(&state, &headers).expect("valid token authorizes");
        assert_eq!(session.username, "viewer1");
    }

    #[tokio::test]
    async fn shared_dashboard_unavailable_without_admin() {
        // Skeleton has no DB; the global Shared Dashboard persistence needs the admin side.
        let app = router(state_with(Arc::new(InMemorySink::default())));
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/shared-dashboard")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body_json(resp).await["error"]["code"], "admin_unavailable");
    }

    #[tokio::test]
    async fn put_shared_dashboard_forbidden_for_non_admin() {
        use yagra_common::Role;
        // Viewer and Operator both lack ManageConfig ⇒ 403, decided before any DB/admin work
        // (authorize-first ordering), so a forbidden caller can't even probe whether admin exists.
        for role in [Role::Viewer, Role::Operator] {
            let (state, token) = state_with_role_token(role);
            let resp = router(state)
                .oneshot(
                    Request::builder()
                        .method("PUT")
                        .uri("/api/v1/shared-dashboard")
                        .header(AUTHORIZATION, format!("Bearer {token}"))
                        .header("content-type", "application/json")
                        .body(Body::from(r#"{"version":2,"boards":[]}"#))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                resp.status(),
                StatusCode::FORBIDDEN,
                "role {role:?} must be forbidden"
            );
            assert_eq!(body_json(resp).await["error"]["code"], "forbidden");
        }
    }

    #[tokio::test]
    async fn put_shared_dashboard_admin_passes_authorization() {
        // Admin clears the RBAC gate; with no DB wired it then hits the admin/503 fallback — so an
        // admin sees 503 (auth passed), not 403. Proves ManageConfig admits the Admin role.
        let (state, token) = state_with_role_token(yagra_common::Role::Admin);
        let resp = router(state)
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/v1/shared-dashboard")
                    .header(AUTHORIZATION, format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"version":2,"boards":[]}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body_json(resp).await["error"]["code"], "admin_unavailable");
    }
}
