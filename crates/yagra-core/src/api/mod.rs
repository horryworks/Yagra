// SPDX-License-Identifier: AGPL-3.0-only
//! Northbound REST API (`/api/v1`).
//!
//! Path-versioned (ADR-019). Responses are JSON; errors use the fixed envelope
//! `{"error": {"code", "message"}}` so clients never see a raw internal error. Readings
//! come from the [`MetricStore`] (VictoriaMetrics live, in-memory for the skeleton) and
//! the inventory from a [`NodeListing`]. A node's display state and the alert endpoints are
//! served from the live [`AlertManager`] (committed liveness + threshold roll-up + active
//! alerts). Cursor pagination is in; RBAC scoping lands as the API grows.
//!
//! ## Layout
//!
//! This module was a single 13.7k-line file holding 28 unrelated domains separated only by comment
//! banners, with one 393-line `router()` expression registering all ~200 method/path pairs — so
//! every feature branch edited the same two places and conflicted. It is being split per domain:
//! each `api/<domain>.rs` owns its handlers, its DTOs and its own [`axum::Router`], and
//! [`router`] merges them.
//!
//! Cross-cutting pieces live beside it:
//!  - [`error`] — the typed [`ApiError`] and the ADR-019 envelope. New handlers return
//!    `ApiResult<T>` and propagate with `?`.
//!  - [`extract`] — auth/availability guards as extractors, so a missing guard is a compile error
//!    rather than a silent hole.
//!  - [`route_table`] — the full method/path inventory, pinned by a test, so moving a domain out
//!    cannot quietly drop an endpoint.
//!
//! Handlers not yet moved still return a bare `Response` and use the `error_response`/`internal`
//! helpers; both styles are supported during the migration. New endpoints should use the new one.

pub(crate) mod alerts;
pub(crate) mod analysis;
mod api_tokens;
mod audit;
pub(crate) mod checks;
mod classification;
mod collection;
mod credentials;
mod dashboard;
mod discovery;
mod error;
pub(crate) mod eventlog;
pub(crate) mod extract;
pub(crate) mod fleet;
mod flow;
mod forwarding;
mod groups;
mod health;
pub(crate) mod maintenance;
pub(crate) mod metrics;
mod mib;
pub(crate) mod nodes;
mod notifications;
mod oidc;
mod pollers;
mod profiles;
mod rca;
#[cfg(test)]
mod route_table;
mod session;
mod system;
#[cfg(test)]
mod tests_support;
pub(crate) mod thresholds;
pub(crate) mod topology;
mod users;
mod util;

pub(crate) use error::{error_response, internal, not_found, unavailable};
pub use error::{ApiError, ApiResult};
pub(crate) use extract::{authorize, bearer, current_username, require_leader, require_view};
// Pool names are validated in `nodes` — every writer of one, including the folder-group and Meraki
// import paths still here, must go through these (a name becomes a NATS subject verbatim).
pub(crate) use util::{
    audit_record, is_valid_oid, is_valid_oid_prefix, now_unix_s, parse_rfc3339, pool_resolver,
    DEFAULT_RATE_LOOKBACK_SECS,
};

use crate::ack::AckRepo;
use crate::alerts::AlertManager;
use crate::analysis::AnalysisRunner;
use crate::audit::AuditRepo;
use crate::auth::{LoginThrottle, SessionStore, UserStore};
use crate::classification::{ClassificationRepo, Classifier};
use crate::collection::CollectionRepo;
use crate::coordinator::Coordinator;
use crate::dashboard::{DashboardRepo, SharedDashboardRepo};
use crate::discovery::DiscoveryRunner;
use crate::groups::GroupRepo;
use crate::history::AlertHistoryStore;
use crate::logstore::LogStore;
use crate::maintenance::MaintenanceRepo;
use crate::mib::MibRepo;
use crate::notifications::NotificationRepo;
use crate::pollers::PollerRepo;

use crate::repo::{NodeListing, NodeRepo};
use crate::reports::{self, ReportRunner, ScheduleInput};
use crate::scheduler::PollDispatcher;
use crate::secrets::CredentialStore;
use crate::store::MetricStore;
use crate::thresholds::ThresholdStore;
use axum::{
    extract::{DefaultBodyLimit, Path, Query, Request, State},
    http::{HeaderMap, StatusCode},
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
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;
use yagra_common::{HostSample, Permission};

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
    /// Reports (Dashboard → Reports): definitions/schedules/runs + the background generator. A
    /// TSDB+PostgreSQL-read computation in core (shared resource — admin-edited, ADR-017).
    pub reports: Arc<ReportRunner>,
    /// Per-node URL-monitor configs (HTTP/HTTPS endpoint checks).
    pub url_checks: Arc<crate::url_check::UrlCheckRepo>,
    /// Per-node DNS-monitor configs plus the observed resolution chains and their history.
    pub dns_checks: Arc<crate::dns_check::DnsCheckRepo>,
    /// Cisco Meraki organizations + network scope + device import (read-only Dashboard API).
    pub meraki_orgs: Arc<crate::meraki::MerakiOrgRepo>,
    /// Per-node Cisco Meraki device bindings.
    pub meraki_devices: Arc<crate::meraki::MerakiDeviceRepo>,
    /// Passive-event sources / rules / event log (syslog/trap/webhook pipeline).
    pub events: Arc<crate::events::EventRepo>,
    /// Distributed poller pool control plane (ADR-009/020): the live poller registry + working-set
    /// publisher. Backs the Pollers view's live stats and the pool-routing decisions (discovery
    /// scans, node pool moves). Live mode only — skeleton mode has no coordinator.
    pub coordinator: Arc<Coordinator>,
    /// Durable poller inventory (ADR-009): lets the Pollers view surface a poller that is currently
    /// offline (its live liveness lives only in the coordinator/Redis, which forget it on TTL).
    pub pollers: Arc<PollerRepo>,
    /// Long-lived API tokens (PATs) for non-browser clients (ADR-028): backs Settings ▸ API tokens
    /// and the MCP auth gate. Admin-managed; the raw token is shown once at creation, never stored.
    pub api_tokens: Arc<crate::apitokens::ApiTokenStore>,
    /// Forwarding destinations (ADR-034): backs Settings ▸ Forwarding. Present on every core so
    /// destination CRUD works from either; only the leader's dispatcher actually sends.
    pub forward: Arc<crate::forward_store::ForwardStore>,
    /// Live forwarding status + the poke that makes a destination edit take effect immediately.
    pub forward_handle: crate::forward::ForwardHandle,
    /// AI-assisted RCA provider configuration (ADR-029): backs Settings ▸ AI. The credential is
    /// envelope-encrypted and write-only, so this store never hands one back out.
    pub llm: Arc<crate::rca::store::RcaRepo>,
}

/// Default range window when `from`/`to` are omitted (seconds).
const DEFAULT_RANGE_SECS: i64 = 3600;
/// Default range step when `step` is omitted (seconds).
const DEFAULT_STEP_SECS: u64 = 60;

/// Hard cap on the number of samples one range query may materialize, regardless of the requested
/// window and step (S20). A client asking for a huge span at a tiny step would otherwise make
/// VictoriaMetrics emit — and core parse — millions of points (a cheap resource-exhaustion vector).
/// ~5000 comfortably exceeds any chart's horizontal pixel resolution.
const MAX_RANGE_POINTS: i64 = 5000;

/// Clamp a range query's `step` so `[from, to]` yields at most [`MAX_RANGE_POINTS`] samples. Floors
/// at the caller's own minimum (`min_step`) and at 1s, then raises it further if the requested span
/// would otherwise exceed the point cap. `from`/`to` are Unix seconds.
pub(crate) fn clamp_range_step(from: i64, to: i64, step: u64, min_step: u64) -> u64 {
    let span = (to - from).max(0);
    let needed = u64::try_from(span / MAX_RANGE_POINTS.max(1)).unwrap_or(u64::MAX);
    step.max(min_step).max(needed).max(1)
}

/// Core's own latest host-resource sample (self-observability), refreshed by the collector task in
/// `main`. Read by `GET /api/v1/system/hosts`; `None` until the first sample (or in skeleton mode).
pub type CoreHostSample = Arc<std::sync::Mutex<Option<HostSample>>>;

/// Shared API state: the metric store, the node inventory source, and the alert engine.
#[derive(Clone)]
pub struct ApiState {
    /// TSDB read/write seam.
    pub store: Arc<dyn MetricStore>,
    /// Event log store (ADR-024). `Some` when VictoriaLogs is configured — then event search
    /// reads from it and it holds the full firehose; `None` keeps events entirely in PostgreSQL.
    pub logs: Option<Arc<dyn LogStore>>,
    /// Flow store (ADR-031, the traffic-flow tier). `Some` when ClickHouse is configured — then the
    /// flow-query endpoints serve top talkers/conversations/ports/protocols/trend; `None` (default-OFF)
    /// makes those endpoints return `503 service_unavailable`.
    pub flows: Option<Arc<dyn crate::flowstore::FlowStore>>,
    /// Offline IP→ASN table (ADR-031 Increment 3), behind a hot-swappable handle so a periodic
    /// reloader can refresh it without a restart. Holds `Some` when `YAGRA_IPASN_DB` is configured —
    /// then the flow top-AS endpoint resolves AS numbers to organization names; `None` leaves names
    /// unset.
    pub ipasn: crate::ipasn::IpAsnHandle,
    /// Core's own latest host-resource sample (CPU/load/mem/disk), for the System Health page.
    pub host_sample: CoreHostSample,
    /// Inventory read seam.
    pub nodes: Arc<dyn NodeListing>,
    /// Alert engine (active alerts + live event stream).
    pub alerts: Arc<AlertManager>,
    /// Write side (inventory + credentials + users); `None` in skeleton mode.
    pub admin: Option<Arc<AdminState>>,
    /// Bearer-token sessions for local auth.
    pub sessions: Arc<SessionStore>,
    /// Brute-force guard for `POST /auth/login` (per-account lockout + global rate cap).
    pub login_throttle: Arc<LoginThrottle>,
    /// Alert history (read); `None` in skeleton mode.
    pub history: Option<Arc<AlertHistoryStore>>,
    /// Inbound ack reflection from external tools (read-only display, ADR-015); `None` in
    /// skeleton mode.
    pub ack: Option<Arc<AckRepo>>,
    /// Passive-event engine (webhook ingest + manual close + inline rule reload);
    /// `None` in skeleton mode.
    pub events: Option<Arc<crate::events::EventEngine>>,
    /// When true, read-only endpoints skip authentication (public read-only dashboard).
    /// When false (default), they require a valid session with `View` (every role has it).
    pub public_dashboard: bool,
    /// HA leadership (ADR-016): `true` when this core holds the advisory lock and runs the
    /// coordinator + ingest + alert/notify singletons. Drives `/readyz` (so a load balancer routes
    /// only to the leader) and gates the event-ingest handlers that would otherwise enqueue to an
    /// undrained channel on a standby. Always `true` when HA is off or in skeleton mode.
    pub is_leader: Arc<std::sync::atomic::AtomicBool>,
    /// External-IdP (OIDC) provider store (ADR-010 Phase 3); `None` in skeleton mode. Drives the
    /// SSO login endpoints and Settings ▸ Auth CRUD.
    pub oidc: Option<Arc<crate::oidc::OidcRepo>>,
    /// In-flight OIDC authorizations (CSRF state → nonce/PKCE), one per pending SSO login.
    pub oidc_flight: Arc<crate::oidc::OidcFlight>,
    /// MCP server enabled (ADR-028, `YAGRA_ENABLE_MCP`). When `true`, `serve()` mounts the read-only
    /// MCP tool surface at `/mcp`; when `false` (default) the route is absent (a request 404s). Held
    /// here so `serve()` reads it off the same state it already threads.
    pub enable_mcp: bool,
    /// AI-assisted root-cause analysis (ADR-029); `None` in skeleton mode. Present on every core —
    /// generation is an on-demand read plus one outbound call, so there is nothing for a standby to
    /// double-do. Whether it actually *works* depends on an operator having configured a provider;
    /// with no config row every RCA endpoint answers 503 and no request leaves the building.
    pub rca: Option<Arc<crate::rca::orchestrator::RcaOrchestrator>>,
}

/// Build the `/api/v1` router backed by the given state.
pub fn router(state: ApiState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        // The inventory itself: listing, detail, and the folder/dependency-tree writes.
        .merge(nodes::routes())
        // Which pool the node effectively belongs to, and which poller currently holds it. Stays
        // with the Pollers view below, whose resolution helpers it shares.
        // URL/HTTP and DNS monitoring (ADR-033) — one node is one kind, see `api/checks.rs`.
        .merge(checks::routes())
        // Cisco Meraki (read-only Dashboard API monitoring).
        .route(
            "/api/v1/meraki/orgs",
            get(list_meraki_orgs).post(create_meraki_orgs),
        )
        .route("/api/v1/meraki/orgs/discover", post(meraki_discover))
        .route("/api/v1/meraki/orgs/:id", delete(delete_meraki_org))
        .route(
            "/api/v1/meraki/orgs/:id/enabled",
            put(set_meraki_org_enabled),
        )
        .route(
            "/api/v1/meraki/orgs/:id/cadence",
            put(set_meraki_org_cadence),
        )
        .route(
            "/api/v1/meraki/orgs/:id/networks",
            get(list_meraki_networks).put(set_meraki_networks_monitored),
        )
        .route(
            "/api/v1/meraki/orgs/:id/enumerate",
            post(enumerate_meraki_org),
        )
        .route("/api/v1/meraki/import", post(import_meraki_devices))
        .route(
            "/api/v1/meraki/polling",
            get(get_meraki_polling).put(set_meraki_polling),
        )
        // Metric reads + fleet Top-N rankings, in `api/metrics.rs`.
        .merge(metrics::routes())
        // Threshold rules — the one config table that grows with the fleet, so its list is capped.
        .merge(thresholds::routes())
        // Flow analysis (ADR-031) — the twelve `/flow/*` endpoints, in `api/flow.rs`.
        .merge(flow::routes())
        // Local user accounts + roles (admin-only), in `api/users.rs`.
        .merge(users::routes())
        // API tokens (PATs) for non-browser clients / the MCP tool surface (ADR-028). Admin-only;
        // issuance/revocation are audited by `audit_mw`. The raw token is returned once on create.
        // Forwarding ("tee", ADR-034): relay received syslog/traps to external collectors.
        // `ManageConfig`-gated and audited by `audit_mw` — a destination sends log bodies, which
        // routinely carry credentials, off-box.
        // OIDC login (external IdP, ADR-010 Phase 3). Both are unauthenticated (pre-session), guarded
        // by the CSRF `state` + PKCE + nonce in the flow itself.
        // AI-assisted RCA (ADR-029). Provider config is admin-only and its credential is
        // write-only; generation is `AckAlerts` (the people who actually work incidents) plus the
        // orchestrator's own rate + concurrency caps, and reading a stored report is `View`.
        // Alerts: active list, history + aggregations, inbound ack, and the two SSE streams,
        // in `api/alerts.rs`.
        .merge(alerts::routes())
        // The dependency graph, in `api/topology.rs`.
        .merge(topology::routes())
        // Fleet-wide rollups (summary / per-group / coverage / timeline), in `api/fleet.rs`.
        .merge(fleet::routes())
        // Distributed poller pool (ADR-009/020): the fleet of registered pollers + per-pool summary.
        // Static `/pollers/:id/nodes` drill-down alongside the `:id` param route.
        // Store-and-forward (Phase 3): recent core↔poller visibility outages (monitoring gaps).
        // Host self-observability: current CPU/load/mem/disk of core + each poller, and the trend
        // series behind the System Health "Host resources" charts.
        // Troubleshoot analysis jobs (ADR-022), in `api/analysis.rs`.
        .merge(analysis::routes())
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
            put(update_report_schedule).delete(delete_report_schedule),
        )
        .route("/api/v1/stream/report-runs", get(stream_report_runs))
        // Maintenance windows + mutes (alert suppression), in `api/maintenance.rs`.
        .merge(maintenance::routes())
        // Passive events: machine-scoped webhook ingest (per-source bearer token, small
        // body cap) + sources/rules CRUD + the event log + manual alert close.
        .route(
            "/api/v1/ingest/webhook/:source_id",
            post(ingest_webhook).layer(DefaultBodyLimit::max(WEBHOOK_BODY_LIMIT)),
        )
        .route(
            "/api/v1/event-sources",
            get(list_event_sources).post(create_event_source),
        )
        .route(
            "/api/v1/event-sources/:id",
            put(update_event_source).delete(delete_event_source),
        )
        .route(
            "/api/v1/event-sources/:id/rotate-token",
            post(rotate_event_source_token),
        )
        .route(
            "/api/v1/event-rules",
            get(list_event_rules).post(create_event_rule),
        )
        .route("/api/v1/event-rules/test", post(test_event_rule))
        .route(
            "/api/v1/event-rules/:id",
            put(update_event_rule).delete(delete_event_rule),
        )
        // The event-log read surface (list + stats), in `api/eventlog.rs`.
        .merge(eventlog::routes())
        .merge(audit::routes())
        .merge(dashboard::routes())
        .merge(mib::routes())
        .merge(api_tokens::routes())
        .merge(session::routes())
        .merge(oidc::routes())
        .merge(system::routes())
        .merge(collection::routes())
        .merge(classification::routes())
        .merge(discovery::routes())
        .merge(notifications::routes())
        .merge(rca::routes())
        .merge(forwarding::routes())
        .merge(groups::routes())
        .merge(profiles::routes())
        .merge(credentials::routes())
        .merge(pollers::routes())
        .merge(health::routes())
        .route("/api/v1/events/alerts/close", post(close_event_alert))
        // Audit middleware: records every mutating /api/v1 request (who + method/path +
        // status) so new write endpoints are covered automatically (security.md).
        .layer(middleware::from_fn_with_state(state.clone(), audit_mw))
        .with_state(state)
}

/// Username recorded when a mutating request carries no valid session.
const AUDIT_ANONYMOUS: &str = "(unauthenticated)";

/// Record one audit entry, best-effort: auditing must never take the API down, so
/// failures are logged and swallowed.
/// Middleware: append an audit row for every mutating `/api/v1` request. Auth endpoints
/// are excluded here — login is recorded by its handler (with the attempted username,
/// never the credential). Reads are not audited.
async fn audit_mw(State(st): State<ApiState>, req: Request, next: Next) -> Response {
    let method = req.method().clone();
    let path = req.uri().path().to_owned();
    let mutating = matches!(method.as_str(), "POST" | "PUT" | "DELETE" | "PATCH");
    // Ingest is exempt: a per-event audit row would flood the audit log (the events table
    // is itself the record); config changes to sources/rules stay audited as usual.
    let audited = mutating
        && path.starts_with("/api/v1/")
        && !path.starts_with("/api/v1/auth/")
        && !path.starts_with("/api/v1/ingest/");
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
        // A successful config mutation bumps the process-wide config generation so background
        // rebuilders (alert-config reloader, scheduler spec resolution) skip their full-fleet
        // rebuild when nothing changed (S2/S6). Coarse but safe: any config write invalidates.
        if resp.status().is_success() && changes_monitoring_config(&path) {
            crate::config_gen::bump();
        }
        if let Some(admin) = st.admin.as_ref() {
            let user = username.as_deref().unwrap_or(AUDIT_ANONYMOUS);
            let action = format!("{method} {path}");
            audit_record(&admin.audit, user, &action, resp.status().as_u16()).await;
        }
    }
    resp
}

/// Whether a mutating request can have changed the inputs to the alert-config / poll-spec rebuild.
///
/// Almost everything can, so this is a deny-list of the exceptions rather than an allow-list of the
/// ~22 write handlers — keeping the S6 signal's safe-over-invalidation property (a needed rebuild is
/// never skipped because someone forgot to add a path).
///
/// The exceptions are the endpoints that are **reads wearing POST**: they create a job or a report,
/// touch no node, profile, threshold, group or credential, and are admission-controlled precisely
/// because they are expensive to *run*, not because they change anything. Counting them as config
/// writes would defeat S6 exactly when it matters most — during an incident, when an operator is
/// launching analyses and asking for explanations, every one of them would force the next 30s tick
/// to redo a full-fleet rebuild across tens of thousands of nodes.
///
/// They stay **audited**; only the dirty signal is suppressed.
fn changes_monitoring_config(path: &str) -> bool {
    !(path.starts_with("/api/v1/analysis/") || path == "/api/v1/rca")
}

/// Liveness probe for the deploy/orchestrator — no auth, no store access. Both the leader and HA
/// standbys answer this so their containers stay healthy while a standby waits for leadership.
async fn healthz() -> &'static str {
    "ok"
}

/// Readiness probe (ADR-016): `200` only when this core holds HA leadership — i.e. it is running the
/// coordinator + ingest and can serve live status. A standby returns `503` so a load balancer /
/// orchestrator routes traffic only to the active core. With HA off this core is always the leader,
/// so it always returns `200`. No auth, no store access (mirrors `/healthz`).
async fn readyz(State(st): State<ApiState>) -> StatusCode {
    if st.is_leader.load(std::sync::atomic::Ordering::Acquire) {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    }
}

/// A Prometheus-style metric name: `[a-zA-Z_:][a-zA-Z0-9_:]*`. Validating at the edge
/// keeps the (untrusted) path segment from being interpolated into the PromQL selector
/// sent to the TSDB (security.md: parse into strong, bounded types at the API edge).
pub(crate) fn is_valid_metric_name(metric: &str) -> bool {
    let mut chars = metric.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' || c == ':' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == ':')
}

// ── Reports (Dashboard → Reports) ────────────────────────────────────────────
//
// Shared resource: everyone reads, only admins (ManageConfig) write — same model as the Shared
// Dashboard. Definitions are reusable templates (opaque `spec`); schedules fire them on a preset
// cadence; runs are saved generated reports. Generation runs in core as a background task.

/// Max bytes for a report definition `spec` — the shared cap on an opaque operator-authored JSON
/// document (see [`util::MAX_JSON_DOC_BYTES`]). This used to read `= MAX_DASHBOARD_BYTES`, reaching
/// into the dashboard block for it, which became a compile error the moment that block moved out.
const MAX_REPORT_SPEC_BYTES: usize = util::MAX_JSON_DOC_BYTES;

/// The section catalog (drives the builder). Open-read; static, so it works in skeleton mode too.
async fn list_report_sections(State(st): State<ApiState>, headers: HeaderMap) -> Response {
    if let Some(resp) = require_view(&st, &headers) {
        return resp;
    }
    Json(reports::section_catalog()).into_response()
}

/// Validate a report definition `spec`: a JSON object within the size cap whose section kinds are all
/// known. Returns `Some(error response)` to short-circuit.
fn validate_report_spec(spec: &serde_json::Value) -> Option<Response> {
    if !spec.is_object() {
        return Some(error_response(
            StatusCode::BAD_REQUEST,
            "invalid_spec",
            "report spec must be a JSON object".to_owned(),
        ));
    }
    if serde_json::to_vec(spec).map_or(usize::MAX, |v| v.len()) > MAX_REPORT_SPEC_BYTES {
        return Some(error_response(
            StatusCode::PAYLOAD_TOO_LARGE,
            "spec_too_large",
            format!("report spec exceeds {MAX_REPORT_SPEC_BYTES} bytes"),
        ));
    }
    if let Some(sections) = spec.get("sections").and_then(|s| s.as_array()) {
        for sec in sections {
            let kind = sec.get("kind").and_then(|k| k.as_str()).unwrap_or("");
            if !reports::is_known_section(kind) {
                return Some(error_response(
                    StatusCode::BAD_REQUEST,
                    "unknown_section",
                    format!("unknown report section kind {kind:?}"),
                ));
            }
        }
    }
    None
}

/// All report definitions (templates). Open-read; empty in skeleton mode.
async fn list_report_definitions(State(st): State<ApiState>, headers: HeaderMap) -> Response {
    if let Some(resp) = require_view(&st, &headers) {
        return resp;
    }
    let Some(admin) = st.admin.as_ref() else {
        return Json(Vec::<serde_json::Value>::new()).into_response();
    };
    match admin.reports.repo().list_definitions().await {
        Ok(defs) => Json(defs).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "list report definitions failed");
            internal("failed to list report definitions")
        }
    }
}

/// One report definition by id.
async fn get_report_definition(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Response {
    if let Some(resp) = require_view(&st, &headers) {
        return resp;
    }
    let Some(admin) = st.admin.as_ref() else {
        return not_found("definition_not_found", format!("no report definition {id}"));
    };
    match admin.reports.repo().get_definition(id).await {
        Ok(Some(def)) => Json(def).into_response(),
        Ok(None) => not_found("definition_not_found", format!("no report definition {id}")),
        Err(e) => {
            tracing::error!(error = %e, "get report definition failed");
            internal("failed to load report definition")
        }
    }
}

/// Create/update body for a report definition.
#[derive(Deserialize)]
struct ReportDefinitionBody {
    name: String,
    description: Option<String>,
    spec: serde_json::Value,
}

/// Create a report definition (admin only — shared resource).
async fn create_report_definition(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Json(body): Json<ReportDefinitionBody>,
) -> Response {
    if let Some(resp) = authorize(&st, &headers, Permission::ManageConfig) {
        return resp;
    }
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    let name = body.name.trim();
    if name.is_empty() {
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_name",
            "report name is required".to_owned(),
        );
    }
    if let Some(resp) = validate_report_spec(&body.spec) {
        return resp;
    }
    let user = current_username(&st, &headers);
    match admin
        .reports
        .repo()
        .create_definition(
            name,
            body.description.as_deref(),
            &body.spec,
            user.as_deref(),
        )
        .await
    {
        Ok(def) => Json(def).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "create report definition failed");
            internal("failed to create report definition")
        }
    }
}

/// Update a report definition (admin only).
async fn update_report_definition(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<ReportDefinitionBody>,
) -> Response {
    if let Some(resp) = authorize(&st, &headers, Permission::ManageConfig) {
        return resp;
    }
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    let name = body.name.trim();
    if name.is_empty() {
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_name",
            "report name is required".to_owned(),
        );
    }
    if let Some(resp) = validate_report_spec(&body.spec) {
        return resp;
    }
    let user = current_username(&st, &headers);
    match admin
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
    {
        Ok(true) => Json(serde_json::json!({ "ok": true })).into_response(),
        Ok(false) => not_found("definition_not_found", format!("no report definition {id}")),
        Err(e) => {
            tracing::error!(error = %e, "update report definition failed");
            internal("failed to update report definition")
        }
    }
}

/// Delete a report definition (admin only). Schedules cascade; saved runs are kept (definition_id
/// set null).
async fn delete_report_definition(
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
    match admin.reports.repo().delete_definition(id).await {
        Ok(true) => Json(serde_json::json!({ "ok": true })).into_response(),
        Ok(false) => not_found("definition_not_found", format!("no report definition {id}")),
        Err(e) => {
            tracing::error!(error = %e, "delete report definition failed");
            internal("failed to delete report definition")
        }
    }
}

/// Generate a report from a definition now (admin only — writes a shared saved run). Returns the
/// run row immediately; it progresses over SSE.
async fn run_report_definition(
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
    let user = current_username(&st, &headers);
    match admin.reports.run_now(id, "manual", user).await {
        Ok(Some(run)) => Json(run).into_response(),
        Ok(None) => not_found("definition_not_found", format!("no report definition {id}")),
        Err(e) => {
            tracing::error!(error = %e, "run report failed");
            internal("failed to start report generation")
        }
    }
}

/// Saved report runs, newest first. `?limit=` (default 50). Open-read; empty in skeleton mode.
async fn list_report_runs(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Query(q): Query<util::ListQuery>,
) -> Response {
    if let Some(resp) = require_view(&st, &headers) {
        return resp;
    }
    let Some(admin) = st.admin.as_ref() else {
        return Json(Vec::<serde_json::Value>::new()).into_response();
    };
    let limit = q.limit.unwrap_or(50).clamp(1, 500);
    match admin.reports.repo().list_runs(limit).await {
        Ok(runs) => Json(runs).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "list report runs failed");
            internal("failed to list report runs")
        }
    }
}

/// One report run with its rendered result (the viewer).
async fn get_report_run(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Response {
    if let Some(resp) = require_view(&st, &headers) {
        return resp;
    }
    let Some(admin) = st.admin.as_ref() else {
        return not_found("run_not_found", format!("no report run {id}"));
    };
    match admin.reports.repo().get_run_detail(id).await {
        Ok(Some(detail)) => Json(detail).into_response(),
        Ok(None) => not_found("run_not_found", format!("no report run {id}")),
        Err(e) => {
            tracing::error!(error = %e, "get report run failed");
            internal("failed to load report run")
        }
    }
}

/// Delete a saved report run (admin only).
async fn delete_report_run(
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
    match admin.reports.repo().delete_run(id).await {
        Ok(true) => Json(serde_json::json!({ "ok": true })).into_response(),
        Ok(false) => not_found("run_not_found", format!("no report run {id}")),
        Err(e) => {
            tracing::error!(error = %e, "delete report run failed");
            internal("failed to delete report run")
        }
    }
}

/// Export-format query for a report run.
#[derive(Deserialize)]
struct ExportQuery {
    format: Option<String>,
}

/// Download a saved report run as HTML / CSV / PDF. Open-read. HTML and CSV are rendered from the
/// stored result; PDF is produced on demand by piping the stored HTML through `wkhtmltopdf` (a
/// 503 if the renderer is unavailable). The run must have succeeded (have a rendered result).
async fn export_report_run(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Query(q): Query<ExportQuery>,
) -> Response {
    use axum::http::header::{CONTENT_DISPOSITION, CONTENT_TYPE};
    if let Some(resp) = require_view(&st, &headers) {
        return resp;
    }
    let Some(admin) = st.admin.as_ref() else {
        return not_found("run_not_found", format!("no report run {id}"));
    };
    let detail = match admin.reports.repo().get_run_detail(id).await {
        Ok(Some(d)) => d,
        Ok(None) => return not_found("run_not_found", format!("no report run {id}")),
        Err(e) => {
            tracing::error!(error = %e, "export report run failed");
            return internal("failed to load report run");
        }
    };
    let Some(html) = detail.result_html.clone() else {
        return error_response(
            StatusCode::CONFLICT,
            "run_not_ready",
            "this report run has no rendered result (still running or failed)".to_owned(),
        );
    };
    let format = q.format.as_deref().unwrap_or("html");
    let stem = format!("report-{id}");
    match format {
        "html" => (
            [
                (CONTENT_TYPE, "text/html; charset=utf-8".to_owned()),
                (
                    CONTENT_DISPOSITION,
                    format!("attachment; filename=\"{stem}.html\""),
                ),
            ],
            html,
        )
            .into_response(),
        "csv" => {
            let json = detail.result_json.unwrap_or(serde_json::Value::Null);
            let csv = reports::result_json_to_csv(&json);
            (
                [
                    (CONTENT_TYPE, "text/csv; charset=utf-8".to_owned()),
                    (
                        CONTENT_DISPOSITION,
                        format!("attachment; filename=\"{stem}.csv\""),
                    ),
                ],
                csv,
            )
                .into_response()
        }
        "pdf" => match html_to_pdf(&html).await {
            Ok(pdf) => (
                [
                    (CONTENT_TYPE, "application/pdf".to_owned()),
                    (
                        CONTENT_DISPOSITION,
                        format!("attachment; filename=\"{stem}.pdf\""),
                    ),
                ],
                pdf,
            )
                .into_response(),
            Err(e) => {
                tracing::warn!(error = %e, "PDF render failed (wkhtmltopdf)");
                error_response(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "pdf_unavailable",
                    "PDF rendering is unavailable on this server".to_owned(),
                )
            }
        },
        other => error_response(
            StatusCode::BAD_REQUEST,
            "invalid_format",
            format!("format must be html|csv|pdf, got {other:?}"),
        ),
    }
}

/// Render HTML to PDF by piping it through `wkhtmltopdf` (reads HTML on stdin, writes PDF to stdout).
/// The HTML is passed via stdin (never the command line), and the args are fixed, so there is no
/// command injection. Returns an error if the binary is missing or exits non-zero.
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

/// All report schedules. Open-read; empty in skeleton mode.
async fn list_report_schedules(State(st): State<ApiState>, headers: HeaderMap) -> Response {
    if let Some(resp) = require_view(&st, &headers) {
        return resp;
    }
    let Some(admin) = st.admin.as_ref() else {
        return Json(Vec::<serde_json::Value>::new()).into_response();
    };
    match admin.reports.repo().list_schedules().await {
        Ok(list) => Json(list).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "list report schedules failed");
            internal("failed to list report schedules")
        }
    }
}

/// Create/update body for a report schedule (preset cadence).
#[derive(Deserialize)]
struct ReportScheduleBody {
    definition_id: Uuid,
    frequency: String,
    day_of_week: Option<i16>,
    day_of_month: Option<i16>,
    at_hour: i16,
    at_minute: i16,
    enabled: Option<bool>,
}

/// Validate a schedule body into a [`ScheduleInput`] + its first `next_run_at`. The error response is
/// boxed so the `Ok` path stays cheap (`clippy::result_large_err`).
#[allow(clippy::type_complexity)]
fn parse_schedule_body(
    body: ReportScheduleBody,
) -> Result<(ScheduleInput, chrono::DateTime<chrono::Utc>), Box<Response>> {
    if !matches!(body.frequency.as_str(), "daily" | "weekly" | "monthly") {
        return Err(Box::new(error_response(
            StatusCode::BAD_REQUEST,
            "invalid_frequency",
            "frequency must be daily|weekly|monthly".to_owned(),
        )));
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
    let at_hour = body.at_hour.clamp(0, 23);
    let at_minute = body.at_minute.clamp(0, 59);
    let input = ScheduleInput {
        definition_id: body.definition_id,
        frequency: body.frequency,
        day_of_week,
        day_of_month,
        at_hour,
        at_minute,
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

/// Create a report schedule (admin only).
async fn create_report_schedule(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Json(body): Json<ReportScheduleBody>,
) -> Response {
    if let Some(resp) = authorize(&st, &headers, Permission::ManageConfig) {
        return resp;
    }
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    let (input, next) = match parse_schedule_body(body) {
        Ok(v) => v,
        Err(resp) => return *resp,
    };
    let user = current_username(&st, &headers);
    match admin
        .reports
        .repo()
        .create_schedule(&input, next, user.as_deref())
        .await
    {
        Ok(id) => Json(serde_json::json!({ "id": id })).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "create report schedule failed");
            internal("failed to create report schedule")
        }
    }
}

/// Update a report schedule (admin only). Recomputes `next_run_at` from the new cadence.
async fn update_report_schedule(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<ReportScheduleBody>,
) -> Response {
    if let Some(resp) = authorize(&st, &headers, Permission::ManageConfig) {
        return resp;
    }
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    let (input, next) = match parse_schedule_body(body) {
        Ok(v) => v,
        Err(resp) => return *resp,
    };
    let user = current_username(&st, &headers);
    match admin
        .reports
        .repo()
        .update_schedule(id, &input, next, user.as_deref())
        .await
    {
        Ok(true) => Json(serde_json::json!({ "ok": true })).into_response(),
        Ok(false) => not_found("schedule_not_found", format!("no report schedule {id}")),
        Err(e) => {
            tracing::error!(error = %e, "update report schedule failed");
            internal("failed to update report schedule")
        }
    }
}

/// Delete a report schedule (admin only).
async fn delete_report_schedule(
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
    match admin.reports.repo().delete_schedule(id).await {
        Ok(true) => Json(serde_json::json!({ "ok": true })).into_response(),
        Ok(false) => not_found("schedule_not_found", format!("no report schedule {id}")),
        Err(e) => {
            tracing::error!(error = %e, "delete report schedule failed");
            internal("failed to delete report schedule")
        }
    }
}

/// Live report-run status stream (SSE): each event is the run JSON with its current state/progress.
async fn stream_report_runs(State(st): State<ApiState>, headers: HeaderMap) -> Response {
    if let Some(resp) = require_view(&st, &headers) {
        return resp;
    }
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
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

// ── Cisco Meraki (read-only Dashboard API monitoring) ──────────────────────────────────────

/// Timeout for a control-plane Meraki API call (discover/enumerate) from core.
const MERAKI_API_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
/// Default Dashboard API base URL (the global shard).
const DEFAULT_MERAKI_BASE_URL: &str = "https://api.meraki.com";

/// Validate/normalize an operator-supplied Meraki base URL: it must be an `https` URL whose host is
/// an allow-listed Meraki API host — enforced before the key is ever sent to it (never affect / leak
/// to a non-Meraki host). The `Err` carries the error response to short-circuit with.
#[allow(clippy::result_large_err)]
fn meraki_base_url(base: Option<String>) -> Result<String, Response> {
    let url = base
        .filter(|b| !b.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_MERAKI_BASE_URL.to_owned());
    let parsed = reqwest::Url::parse(&url).map_err(|_| {
        error_response(
            StatusCode::BAD_REQUEST,
            "invalid_base_url",
            "base_url is not a valid URL".to_owned(),
        )
    })?;
    let host_ok = parsed
        .host_str()
        .is_some_and(yagra_common::is_meraki_api_host);
    if parsed.scheme() != "https" || !host_ok {
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            "invalid_base_url",
            "base_url must be an https Meraki API host (api.meraki.com / regional shard)"
                .to_owned(),
        ));
    }
    Ok(url)
}

/// Map a Meraki upstream/transport failure to a 502 with a generic message (never leak the key or
/// raw error internals to the client).
fn meraki_upstream_error(context: &str, e: &yagra_transport::TransportError) -> Response {
    tracing::warn!(error = %e, "meraki upstream call failed: {context}");
    error_response(
        StatusCode::BAD_GATEWAY,
        "meraki_upstream_error",
        format!("Meraki Dashboard API call failed ({context})"),
    )
}

#[derive(Deserialize)]
struct MerakiDiscoverReq {
    api_key: String,
    #[serde(default)]
    base_url: Option<String>,
}

#[derive(Serialize)]
struct MerakiOrgOption {
    id: String,
    name: String,
}

/// List the organizations an API key can access (read-only `GET /organizations`) so the operator can
/// multi-select which to monitor. Does not persist anything.
async fn meraki_discover(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Json(body): Json<MerakiDiscoverReq>,
) -> Response {
    let Some(_admin) = st.admin.as_ref() else {
        return unavailable();
    };
    if let Some(resp) = authorize(&st, &headers, Permission::ManageConfig) {
        return resp;
    }
    if body.api_key.trim().is_empty() {
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_api_key",
            "api_key must not be empty".to_owned(),
        );
    }
    let base = match meraki_base_url(body.base_url) {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    match yagra_transport::list_organizations(&base, &body.api_key, MERAKI_API_TIMEOUT).await {
        Ok(orgs) => Json(
            orgs.into_iter()
                .map(|o| MerakiOrgOption {
                    id: o.id,
                    name: o.name,
                })
                .collect::<Vec<_>>(),
        )
        .into_response(),
        Err(e) => meraki_upstream_error("discover organizations", &e),
    }
}

#[derive(Serialize)]
struct MerakiOrgView {
    id: Uuid,
    org_id: String,
    name: String,
    base_url: String,
    enabled: bool,
    availability_secs: u32,
    uplink_secs: u32,
    traffic_secs: u32,
    inventory_secs: u32,
    enabled_tiers: Vec<String>,
    target_rps: f64,
    group_id: Option<Uuid>,
}

fn meraki_org_view(o: &crate::meraki::MerakiOrg) -> MerakiOrgView {
    MerakiOrgView {
        id: o.id,
        org_id: o.org_id.clone(),
        name: o.name.clone(),
        base_url: o.base_url.clone(),
        enabled: o.enabled,
        availability_secs: o.availability_secs,
        uplink_secs: o.uplink_secs,
        traffic_secs: o.traffic_secs,
        inventory_secs: o.inventory_secs,
        enabled_tiers: o.enabled_tiers.clone(),
        target_rps: o.target_rps,
        group_id: o.group_id,
    }
}

/// List the configured Meraki organizations (Integrations page).
async fn list_meraki_orgs(State(st): State<ApiState>, headers: HeaderMap) -> Response {
    if let Some(resp) = require_view(&st, &headers) {
        return resp;
    }
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    match admin.meraki_orgs.list().await {
        Ok(orgs) => Json(orgs.iter().map(meraki_org_view).collect::<Vec<_>>()).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "list meraki orgs failed");
            internal("failed to list meraki organizations")
        }
    }
}

#[derive(Deserialize)]
struct CreateMerakiOrgsReq {
    api_key: String,
    #[serde(default)]
    base_url: Option<String>,
    org_ids: Vec<String>,
}

/// Onboard one or more organizations under a single (read-only) API key: validate the key by
/// listing orgs, seal it once as a shared `meraki_api` credential, then create an org row per
/// selected id. Returns how many were created.
async fn create_meraki_orgs(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Json(body): Json<CreateMerakiOrgsReq>,
) -> Response {
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    if let Some(resp) = authorize(&st, &headers, Permission::ManageConfig) {
        return resp;
    }
    if body.api_key.trim().is_empty() || body.org_ids.is_empty() {
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "api_key and at least one org_id are required".to_owned(),
        );
    }
    let base = match meraki_base_url(body.base_url) {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    // Validate the key is read-only-usable and get org names.
    let orgs =
        match yagra_transport::list_organizations(&base, &body.api_key, MERAKI_API_TIMEOUT).await {
            Ok(o) => o,
            Err(e) => return meraki_upstream_error("validate key / list organizations", &e),
        };
    // Seal the key ONCE as a shared credential (holds only the key — org id lives on the row).
    let secret = serde_json::json!({ "api_key": body.api_key }).to_string();
    let cred_name = format!("Meraki API ({} org)", body.org_ids.len());
    let cred_id = match admin
        .creds
        .create(
            &cred_name,
            crate::secrets::KIND_MERAKI_API,
            secret.as_bytes(),
        )
        .await
    {
        Ok(id) => id,
        Err(e) => {
            tracing::error!(error = %e, "seal meraki credential failed");
            return internal("failed to store meraki credential");
        }
    };
    let mut created = 0u32;
    for oid in &body.org_ids {
        let name = orgs
            .iter()
            .find(|o| &o.id == oid)
            .map_or(oid.as_str(), |o| o.name.as_str());
        match admin.meraki_orgs.create(oid, name, &base, cred_id).await {
            Ok(_) => created += 1,
            Err(e) => tracing::warn!(org = %oid, error = %e, "create meraki org failed (skipped)"),
        }
    }
    (
        StatusCode::CREATED,
        Json(serde_json::json!({ "created": created })),
    )
        .into_response()
}

/// Delete an organization: removes its device nodes, config, and HostTree groups.
async fn delete_meraki_org(
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
    match admin.meraki_orgs.purge(id).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => not_found("meraki_org_not_found", format!("no meraki org {id}")),
        Err(e) => {
            tracing::error!(error = %e, "delete meraki org failed");
            internal("failed to delete meraki organization")
        }
    }
}

#[derive(Deserialize)]
struct MerakiEnabledReq {
    enabled: bool,
}

/// Enable/disable an org (pause collection without losing config/history).
async fn set_meraki_org_enabled(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<MerakiEnabledReq>,
) -> Response {
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    if let Some(resp) = authorize(&st, &headers, Permission::ManageConfig) {
        return resp;
    }
    match admin.meraki_orgs.set_enabled(id, body.enabled).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => not_found("meraki_org_not_found", format!("no meraki org {id}")),
        Err(e) => {
            tracing::error!(error = %e, "set meraki org enabled failed");
            internal("failed to update meraki organization")
        }
    }
}

#[derive(Deserialize)]
struct MerakiCadenceReq {
    availability_secs: i32,
    uplink_secs: i32,
    traffic_secs: i32,
    inventory_secs: i32,
    enabled_tiers: Vec<String>,
    target_rps: f64,
}

/// Update an org's per-tier cadence, enabled tiers, and rate budget (validated against the Meraki
/// cadence bands + the hard rps cap safeguard).
async fn set_meraki_org_cadence(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<MerakiCadenceReq>,
) -> Response {
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    if let Some(resp) = authorize(&st, &headers, Permission::ManageConfig) {
        return resp;
    }
    use crate::config::*;
    let in_range = |v: i32, lo: i32, hi: i32| v >= lo && v <= hi;
    if !in_range(
        body.availability_secs,
        MERAKI_FAST_MIN_SECS,
        MERAKI_FAST_MAX_SECS,
    ) || !in_range(body.uplink_secs, MERAKI_FAST_MIN_SECS, MERAKI_FAST_MAX_SECS)
        || !in_range(
            body.traffic_secs,
            MERAKI_TRAFFIC_MIN_SECS,
            MERAKI_TRAFFIC_MAX_SECS,
        )
        || !in_range(
            body.inventory_secs,
            MERAKI_INVENTORY_MIN_SECS,
            MERAKI_INVENTORY_MAX_SECS,
        )
    {
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_cadence",
            "a cadence value is outside its allowed range".to_owned(),
        );
    }
    if !(body.target_rps > 0.0 && body.target_rps <= MERAKI_TARGET_RPS_MAX) {
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_target_rps",
            format!("target_rps must be in (0, {MERAKI_TARGET_RPS_MAX}]"),
        );
    }
    if body
        .enabled_tiers
        .iter()
        .any(|t| yagra_common::MerakiTier::from_token(t).is_none())
    {
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_tier",
            "enabled_tiers contains an unknown tier".to_owned(),
        );
    }
    match admin
        .meraki_orgs
        .update_cadence(
            id,
            body.availability_secs,
            body.uplink_secs,
            body.traffic_secs,
            body.inventory_secs,
            &body.enabled_tiers,
            body.target_rps,
        )
        .await
    {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => not_found("meraki_org_not_found", format!("no meraki org {id}")),
        Err(e) => {
            tracing::error!(error = %e, "set meraki org cadence failed");
            internal("failed to update meraki organization")
        }
    }
}

#[derive(Serialize)]
struct MerakiNetworkView {
    network_id: String,
    name: String,
    monitored: bool,
}

/// The org's networks with their monitored (in-scope) flag.
async fn list_meraki_networks(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Response {
    if let Some(resp) = require_view(&st, &headers) {
        return resp;
    }
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    match admin.meraki_orgs.list_networks(id).await {
        Ok(nets) => Json(
            nets.into_iter()
                .map(|(network_id, name, monitored)| MerakiNetworkView {
                    network_id,
                    name,
                    monitored,
                })
                .collect::<Vec<_>>(),
        )
        .into_response(),
        Err(e) => {
            tracing::error!(error = %e, "list meraki networks failed");
            internal("failed to list meraki networks")
        }
    }
}

#[derive(Deserialize)]
struct MerakiMonitoredReq {
    network_ids: Vec<String>,
    monitored: bool,
}

/// Set the monitored (watch/skip) flag for a set of the org's networks.
async fn set_meraki_networks_monitored(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<MerakiMonitoredReq>,
) -> Response {
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    if let Some(resp) = authorize(&st, &headers, Permission::ManageConfig) {
        return resp;
    }
    match admin
        .meraki_orgs
        .set_networks_monitored(id, &body.network_ids, body.monitored)
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => {
            tracing::error!(error = %e, "set meraki networks monitored failed");
            internal("failed to update network scope")
        }
    }
}

#[derive(Serialize)]
struct MerakiCandidate {
    serial: String,
    name: String,
    model: Option<String>,
    product_type: String,
    network_id: String,
    network_name: String,
    lan_ip: Option<String>,
}

/// Enumerate an org's networks + devices from the Dashboard API (read-only): upsert the network
/// scope (preserving monitored flags) and return import candidates (already-imported serials
/// filtered out). Powers the import wizard.
async fn enumerate_meraki_org(
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
    let org = match admin.meraki_orgs.get(id).await {
        Ok(Some(o)) => o,
        Ok(None) => return not_found("meraki_org_not_found", format!("no meraki org {id}")),
        Err(e) => {
            tracing::error!(error = %e, "enumerate: load org failed");
            return internal("failed to load meraki organization");
        }
    };
    let Some(api_key) = crate::meraki::resolve_meraki_key(&admin.creds, org.credential_id).await
    else {
        return error_response(
            StatusCode::BAD_REQUEST,
            "meraki_credential_unavailable",
            "the org's API key could not be resolved".to_owned(),
        );
    };
    let networks = match yagra_transport::list_networks(
        &org.base_url,
        &api_key,
        &org.org_id,
        MERAKI_API_TIMEOUT,
    )
    .await
    {
        Ok(n) => n,
        Err(e) => return meraki_upstream_error("list networks", &e),
    };
    let devices = match yagra_transport::list_devices(
        &org.base_url,
        &api_key,
        &org.org_id,
        MERAKI_API_TIMEOUT,
    )
    .await
    {
        Ok(d) => d,
        Err(e) => return meraki_upstream_error("list devices", &e),
    };
    // Persist the network list (preserving monitored flags) + stamp last sync.
    let net_pairs: Vec<(String, String)> = networks
        .iter()
        .map(|n| (n.id.clone(), n.name.clone()))
        .collect();
    if let Err(e) = admin.meraki_orgs.upsert_networks(org.id, &net_pairs).await {
        tracing::warn!(error = %e, "enumerate: upsert networks failed");
    }
    let _ = admin.meraki_orgs.touch_sync(org.id).await;

    let imported = admin
        .meraki_devices
        .serials(org.id)
        .await
        .unwrap_or_default();
    let net_name = |nid: &str| {
        networks
            .iter()
            .find(|n| n.id == nid)
            .map_or_else(|| nid.to_owned(), |n| n.name.clone())
    };
    let candidates: Vec<MerakiCandidate> = devices
        .into_iter()
        .filter(|d| !imported.contains(&d.serial))
        .map(|d| MerakiCandidate {
            network_name: net_name(&d.network_id),
            serial: d.serial,
            name: d.name,
            model: d.model,
            product_type: d.product_type,
            network_id: d.network_id,
            lan_ip: d.lan_ip,
        })
        .collect();
    let networks_view: Vec<MerakiNetworkView> = admin
        .meraki_orgs
        .list_networks(org.id)
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|(network_id, name, monitored)| MerakiNetworkView {
            network_id,
            name,
            monitored,
        })
        .collect();
    Json(serde_json::json!({ "networks": networks_view, "devices": candidates })).into_response()
}

#[derive(Deserialize)]
struct MerakiImportReq {
    org_uuid: Uuid,
    #[serde(default)]
    monitored_network_ids: Vec<String>,
    devices: Vec<MerakiImportDeviceReq>,
}

#[derive(Deserialize)]
struct MerakiImportDeviceReq {
    serial: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    model: Option<String>,
    product_type: String,
    network_id: String,
    #[serde(default)]
    network_name: Option<String>,
    #[serde(default)]
    lan_ip: Option<String>,
}

/// Import selected devices as nodes (atomic): set the chosen networks in scope, then create each
/// node + its Meraki binding under the org→network group tree. Already-imported serials are skipped.
async fn import_meraki_devices(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Json(body): Json<MerakiImportReq>,
) -> Response {
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    if let Some(resp) = authorize(&st, &headers, Permission::ManageConfig) {
        return resp;
    }
    let org = match admin.meraki_orgs.get(body.org_uuid).await {
        Ok(Some(o)) => o,
        Ok(None) => {
            return not_found(
                "meraki_org_not_found",
                format!("no meraki org {}", body.org_uuid),
            )
        }
        Err(e) => {
            tracing::error!(error = %e, "import: load org failed");
            return internal("failed to load meraki organization");
        }
    };
    // Mark the chosen networks in scope.
    if !body.monitored_network_ids.is_empty() {
        let _ = admin
            .meraki_orgs
            .set_networks_monitored(org.id, &body.monitored_network_ids, true)
            .await;
    }
    let imported = admin
        .meraki_devices
        .serials(org.id)
        .await
        .unwrap_or_default();

    let mut to_import = Vec::new();
    for d in &body.devices {
        if imported.contains(&d.serial) {
            continue;
        }
        // Resolve the built-in Meraki-API profile for the product type (else a role fallback).
        let profile_id = match yagra_common::api_profile_name_for_product_type(&d.product_type) {
            Some(name) => admin.repo.profile_id_for_name(name).await.unwrap_or(None),
            None => None,
        };
        let profile_id = match profile_id {
            Some(p) => Some(p),
            None => admin
                .repo
                .profile_id_for_category(
                    yagra_common::category_for_product_type(&d.product_type).as_str(),
                )
                .await
                .unwrap_or(None),
        };
        let lan_ip = d
            .lan_ip
            .as_deref()
            .and_then(|s| s.parse::<std::net::IpAddr>().ok());
        let name = if d.name.trim().is_empty() {
            d.serial.clone()
        } else {
            d.name.clone()
        };
        let network_name = d
            .network_name
            .clone()
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| d.network_id.clone());
        to_import.push(crate::meraki::MerakiImportDevice {
            serial: d.serial.clone(),
            name,
            model: d.model.clone(),
            product_type: d.product_type.clone(),
            network_id: d.network_id.clone(),
            network_name,
            lan_ip,
            profile_id,
        });
    }
    match admin.meraki_orgs.import_devices(&org, &to_import).await {
        Ok(count) => (
            StatusCode::CREATED,
            Json(serde_json::json!({ "imported": count })),
        )
            .into_response(),
        Err(e) => {
            tracing::error!(error = %e, "meraki import failed");
            internal("failed to import meraki devices")
        }
    }
}

#[derive(Deserialize)]
struct MerakiPollingReq {
    enabled: bool,
}

/// Read the global Meraki polling kill switch.
async fn get_meraki_polling(State(st): State<ApiState>, headers: HeaderMap) -> Response {
    if let Some(resp) = require_view(&st, &headers) {
        return resp;
    }
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    let enabled = admin.repo.get_meraki_polling_enabled().await;
    Json(serde_json::json!({ "enabled": enabled })).into_response()
}

/// Set the global Meraki polling kill switch (safeguard: instantly halt all Meraki polling).
async fn set_meraki_polling(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Json(body): Json<MerakiPollingReq>,
) -> Response {
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    if let Some(resp) = authorize(&st, &headers, Permission::ManageConfig) {
        return resp;
    }
    match admin.repo.set_meraki_polling_enabled(body.enabled).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => {
            tracing::error!(error = %e, "set meraki polling switch failed");
            internal("failed to update meraki polling switch")
        }
    }
}

// ── Passive events: webhook ingest + sources/rules CRUD + event log + close ─────────

/// Max accepted webhook ingest body (axum returns 413 beyond it).
const WEBHOOK_BODY_LIMIT: usize = 64 * 1024;
/// Normalized event text cap (matches the `events.message` DB CHECK).
const EVENT_TEXT_MAX_CHARS: usize = 4096;

/// Pull the matchable text out of a webhook body. Zero-config heuristic: a JSON object's
/// first present `message` | `text` | `summary` string field, else the compact JSON,
/// else the raw body as lossy UTF-8. (Per-source JSON-path extraction is a follow-up —
/// this covers the common alertmanager/grafana/custom-script shapes without config.)
fn extract_webhook_text(body: &[u8]) -> (String, bool) {
    let text = match serde_json::from_slice::<serde_json::Value>(body) {
        Ok(v) => {
            let field = v.as_object().and_then(|obj| {
                ["message", "text", "summary"]
                    .iter()
                    .find_map(|k| obj.get(*k).and_then(|x| x.as_str()).map(str::to_owned))
            });
            field.unwrap_or_else(|| v.to_string())
        }
        Err(_) => String::from_utf8_lossy(body).into_owned(),
    };
    match text.char_indices().nth(EVENT_TEXT_MAX_CHARS) {
        Some((idx, _)) => (text[..idx].to_owned(), true),
        None => (text, false),
    }
}

/// `POST /api/v1/ingest/webhook/:source_id` — machine-scoped ingest. Auth is the
/// per-source bearer token (constant-time hash compare), NOT a session: external
/// senders hold only their token. Exempt from the audit middleware (the events table
/// is the record).
async fn ingest_webhook(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Path(source_id): Path<Uuid>,
    body: axum::body::Bytes,
) -> Response {
    let Some(engine) = st.events.as_ref() else {
        return unavailable();
    };
    // The event engine's persist/action channels are drained only by the leader's writers (ADR-016);
    // ingesting on a standby would enqueue to an undrained channel and eventually block. Route
    // ingestion to the leader (503 here, `/readyz` tells the LB which core that is).
    if let Some(resp) = require_leader(&st) {
        return resp;
    }
    let Some(token) = bearer(&headers) else {
        return error_response(
            StatusCode::UNAUTHORIZED,
            "missing_token",
            "Authorization: Bearer <token> required".to_owned(),
        );
    };
    let node_id = match engine.repo().verify_token(source_id, token).await {
        Ok(crate::events::TokenVerify::Ok { node_id }) => node_id,
        Ok(crate::events::TokenVerify::BadToken) => {
            return error_response(
                StatusCode::UNAUTHORIZED,
                "bad_token",
                "ingest token does not match".to_owned(),
            );
        }
        Ok(crate::events::TokenVerify::UnknownOrDisabled) => {
            return not_found(
                "source_not_found",
                format!("no enabled webhook source {source_id}"),
            );
        }
        Err(e) => {
            tracing::error!(error = %e, "verify ingest token failed");
            return internal("failed to verify ingest token");
        }
    };
    if !engine.ingest_allowed(source_id) {
        return error_response(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            "ingest rate limit exceeded for this source".to_owned(),
        );
    }

    let (message, truncated) = extract_webhook_text(&body);
    let event_id = Uuid::new_v4();
    let at_unix_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
    let msg = yagra_bus::EventMsg {
        event_id,
        kind: yagra_bus::EventKind::Webhook,
        at_unix_ms,
        source_ip: None,
        pool: None,
        message,
        facility: None,
        syslog_severity: None,
        hostname: None,
        app_name: None,
        trap_oid: None,
        varbinds: Vec::new(),
        truncated,
        // Webhook bodies are JSON, not a datagram — there is no wire form to forward verbatim,
        // and the body can hold the shared secret. Forwarding renders from the parsed fields.
        raw: None,
        src_port: None,
    };
    engine
        .handle_event(
            msg,
            Some(crate::events::SourceBinding { source_id, node_id }),
        )
        .await;
    (
        StatusCode::ACCEPTED,
        Json(serde_json::json!({ "event_id": event_id })),
    )
        .into_response()
}

async fn list_event_sources(State(st): State<ApiState>, headers: HeaderMap) -> Response {
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    if let Some(resp) = authorize(&st, &headers, Permission::ManageConfig) {
        return resp;
    }
    match admin.events.list_sources().await {
        Ok(sources) => Json(sources).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "list event sources failed");
            internal("failed to list event sources")
        }
    }
}

#[derive(Deserialize)]
struct CreateEventSource {
    name: String,
    #[serde(default)]
    node_id: Option<Uuid>,
}

async fn create_event_source(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Json(body): Json<CreateEventSource>,
) -> Response {
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    if let Some(resp) = authorize(&st, &headers, Permission::ManageConfig) {
        return resp;
    }
    let name = body.name.trim();
    if name.is_empty() || name.len() > 120 {
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_source",
            "name must be 1..=120 characters".to_owned(),
        );
    }
    match admin.events.create_source(name, body.node_id).await {
        // The plaintext token is disclosed exactly once, here — only its hash is stored.
        Ok((id, token)) => (
            StatusCode::CREATED,
            Json(serde_json::json!({ "id": id, "token": token })),
        )
            .into_response(),
        Err(e) => {
            tracing::error!(error = %e, "create event source failed");
            internal("failed to create event source")
        }
    }
}

#[derive(Deserialize)]
struct UpdateEventSource {
    name: String,
    enabled: bool,
    #[serde(default)]
    node_id: Option<Uuid>,
}

async fn update_event_source(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateEventSource>,
) -> Response {
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    if let Some(resp) = authorize(&st, &headers, Permission::ManageConfig) {
        return resp;
    }
    let name = body.name.trim();
    if name.is_empty() || name.len() > 120 {
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_source",
            "name must be 1..=120 characters".to_owned(),
        );
    }
    match admin
        .events
        .update_source(id, name, body.enabled, body.node_id)
        .await
    {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => not_found("source_not_found", format!("no event source {id}")),
        Err(e) => {
            tracing::error!(error = %e, "update event source failed");
            internal("failed to update event source")
        }
    }
}

async fn rotate_event_source_token(
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
    match admin.events.rotate_token(id).await {
        Ok(Some(token)) => Json(serde_json::json!({ "token": token })).into_response(),
        Ok(None) => not_found("source_not_found", format!("no event source {id}")),
        Err(e) => {
            tracing::error!(error = %e, "rotate event source token failed");
            internal("failed to rotate event source token")
        }
    }
}

async fn delete_event_source(
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
    match admin.events.delete_source(id).await {
        Ok(true) => {
            reload_event_engine(&st, admin).await;
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(false) => not_found("source_not_found", format!("no event source {id}")),
        Err(e) => {
            tracing::error!(error = %e, "delete event source failed");
            internal("failed to delete event source")
        }
    }
}

async fn list_event_rules(State(st): State<ApiState>, headers: HeaderMap) -> Response {
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    if let Some(resp) = authorize(&st, &headers, Permission::ManageConfig) {
        return resp;
    }
    match admin.events.list_rules().await {
        Ok(rules) => Json(rules).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "list event rules failed");
            internal("failed to list event rules")
        }
    }
}

#[derive(Deserialize)]
struct EventRuleBody {
    name: String,
    #[serde(default = "default_event_rule_enabled")]
    enabled: bool,
    #[serde(default)]
    source_kind: Option<String>,
    #[serde(default)]
    source_id: Option<Uuid>,
    #[serde(default)]
    node_id: Option<Uuid>,
    match_kind: String,
    pattern: String,
    #[serde(default)]
    clear_pattern: Option<String>,
    severity: String,
    #[serde(default)]
    ttl_secs: Option<i32>,
    #[serde(default)]
    min_count: Option<i32>,
    #[serde(default)]
    window_secs: Option<i32>,
}

const fn default_event_rule_enabled() -> bool {
    true
}

/// Validate an event-rule body at the API edge (mirrors the DB CHECKs; regexes must
/// compile so a broken rule never reaches the engine snapshot).
fn validate_event_rule(body: &EventRuleBody) -> Result<crate::events::RuleParams<'_>, String> {
    let name = body.name.trim();
    if name.is_empty() || name.len() > 120 {
        return Err("name must be 1..=120 characters".to_owned());
    }
    if !matches!(body.match_kind.as_str(), "substring" | "regex") {
        return Err("match_kind must be substring or regex".to_owned());
    }
    crate::events::compile_matcher(&body.match_kind, &body.pattern)
        .map_err(|e| format!("pattern: {e}"))?;
    if let Some(clear) = body.clear_pattern.as_deref() {
        crate::events::compile_matcher(&body.match_kind, clear)
            .map_err(|e| format!("clear_pattern: {e}"))?;
    }
    if !matches!(body.severity.as_str(), "info" | "warning" | "critical") {
        return Err("severity must be info, warning, or critical".to_owned());
    }
    if let Some(kind) = body.source_kind.as_deref() {
        if !matches!(kind, "syslog" | "trap" | "webhook") {
            return Err("source_kind must be syslog, trap, or webhook".to_owned());
        }
    }
    let ttl_secs = body.ttl_secs.unwrap_or(1800);
    if !(60..=604_800).contains(&ttl_secs) {
        return Err("ttl_secs must be 60..=604800".to_owned());
    }
    let min_count = body.min_count.unwrap_or(1);
    if !(1..=100).contains(&min_count) {
        return Err("min_count must be 1..=100".to_owned());
    }
    let window_secs = body.window_secs.unwrap_or(60);
    if !(1..=3600).contains(&window_secs) {
        return Err("window_secs must be 1..=3600".to_owned());
    }
    Ok(crate::events::RuleParams {
        name,
        enabled: body.enabled,
        source_kind: body.source_kind.as_deref(),
        source_id: body.source_id,
        node_id: body.node_id,
        match_kind: &body.match_kind,
        pattern: &body.pattern,
        clear_pattern: body.clear_pattern.as_deref(),
        severity: &body.severity,
        ttl_secs,
        min_count,
        window_secs,
    })
}

async fn create_event_rule(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Json(body): Json<EventRuleBody>,
) -> Response {
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    if let Some(resp) = authorize(&st, &headers, Permission::ManageConfig) {
        return resp;
    }
    let params = match validate_event_rule(&body) {
        Ok(p) => p,
        Err(msg) => return error_response(StatusCode::BAD_REQUEST, "invalid_rule", msg),
    };
    match admin.events.create_rule(&params).await {
        Ok(id) => {
            reload_event_engine(&st, admin).await;
            (StatusCode::CREATED, Json(serde_json::json!({ "id": id }))).into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "create event rule failed");
            internal("failed to create event rule")
        }
    }
}

async fn update_event_rule(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<EventRuleBody>,
) -> Response {
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    if let Some(resp) = authorize(&st, &headers, Permission::ManageConfig) {
        return resp;
    }
    let params = match validate_event_rule(&body) {
        Ok(p) => p,
        Err(msg) => return error_response(StatusCode::BAD_REQUEST, "invalid_rule", msg),
    };
    match admin.events.update_rule(id, &params).await {
        Ok(true) => {
            reload_event_engine(&st, admin).await;
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(false) => not_found("rule_not_found", format!("no event rule {id}")),
        Err(e) => {
            tracing::error!(error = %e, "update event rule failed");
            internal("failed to update event rule")
        }
    }
}

async fn delete_event_rule(
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
    match admin.events.delete_rule(id).await {
        Ok(true) => {
            reload_event_engine(&st, admin).await;
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(false) => not_found("rule_not_found", format!("no event rule {id}")),
        Err(e) => {
            tracing::error!(error = %e, "delete event rule failed");
            internal("failed to delete event rule")
        }
    }
}

/// Body for the interactive rule tester.
#[derive(Deserialize)]
struct EventRuleTest {
    match_kind: String,
    pattern: String,
    #[serde(default)]
    clear_pattern: Option<String>,
    sample: String,
}

/// `POST /api/v1/event-rules/test` — try a pattern against a sample message. Compile
/// errors come back in-band (`error` field) so the UI tester can show them inline.
async fn test_event_rule(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Json(body): Json<EventRuleTest>,
) -> Response {
    if let Some(resp) = authorize(&st, &headers, Permission::ManageConfig) {
        return resp;
    }
    let matcher = match crate::events::compile_matcher(&body.match_kind, &body.pattern) {
        Ok(m) => m,
        Err(e) => {
            return Json(serde_json::json!({
                "matched": false, "clear_matched": null, "error": format!("pattern: {e}"),
            }))
            .into_response();
        }
    };
    let clear_matched = match body.clear_pattern.as_deref() {
        Some(clear) => match crate::events::compile_matcher(&body.match_kind, clear) {
            Ok(m) => Some(m.matches(&body.sample)),
            Err(e) => {
                return Json(serde_json::json!({
                    "matched": false, "clear_matched": null,
                    "error": format!("clear_pattern: {e}"),
                }))
                .into_response();
            }
        },
        None => None,
    };
    Json(serde_json::json!({
        "matched": matcher.matches(&body.sample),
        "clear_matched": clear_matched,
        "error": null,
    }))
    .into_response()
}

/// Body for the manual event-alert close (identity mirrors the alert wire shape).
#[derive(Deserialize)]
struct CloseEventAlert {
    #[allow(dead_code)] // carried for parity with the alert identity; close keys on `check`
    node: Uuid,
    check: Uuid,
}

async fn close_event_alert(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Json(body): Json<CloseEventAlert>,
) -> Response {
    let Some(engine) = st.events.as_ref() else {
        return unavailable();
    };
    if let Some(resp) = authorize(&st, &headers, Permission::AckAlerts) {
        return resp;
    }
    // The event engine's active-alert map lives only in the leader's process (fed by the leader-only
    // event pipeline, ADR-016); closing on a standby would act on an empty map. Route to the leader.
    if let Some(resp) = require_leader(&st) {
        return resp;
    }
    if engine
        .close_alert(yagra_common::CheckId::from(body.check))
        .await
    {
        StatusCode::NO_CONTENT.into_response()
    } else {
        not_found(
            "alert_not_found",
            format!("no active event alert for check {}", body.check),
        )
    }
}

/// Reload the event engine's rules/address snapshot after a source/rule edit so the
/// change applies immediately (the 30s refresh loop is the backstop).
async fn reload_event_engine(st: &ApiState, admin: &AdminState) {
    if let Some(engine) = st.events.as_ref() {
        engine.reload(&admin.repo).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::StaticNodeList;
    use crate::sink::InMemorySink;
    use axum::body::{to_bytes, Body};
    use axum::http::{header::AUTHORIZATION, Request};
    use tower::ServiceExt; // for `oneshot`
    use yagra_bus::{CheckOutcome, PollResult, Sample};
    use yagra_common::NodeId;

    fn state_with(store: Arc<dyn MetricStore>) -> ApiState {
        // Public-dashboard mode: read endpoints are open (no token required).
        ApiState {
            store,
            logs: None,
            flows: None,
            ipasn: crate::ipasn::empty_handle(),
            nodes: Arc::new(StaticNodeList::demo()),
            alerts: Arc::new(AlertManager::new()),
            host_sample: Arc::new(std::sync::Mutex::new(None)),
            admin: None,
            sessions: Arc::new(SessionStore::new()),
            login_throttle: Arc::new(LoginThrottle::new()),
            history: None,
            ack: None,
            events: None,
            public_dashboard: true,
            is_leader: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            oidc: None,
            oidc_flight: Arc::new(crate::oidc::OidcFlight::new()),
            enable_mcp: false,
            rca: None,
        }
    }

    /// A private (auth-required) state plus a freshly issued Viewer token for it.
    fn private_state_with(store: Arc<dyn MetricStore>) -> (ApiState, String) {
        use yagra_common::{Principal, Role, Scope};
        let sessions = Arc::new(SessionStore::new());
        let token = sessions.issue(
            Uuid::new_v4(),
            Principal::new(Role::Viewer, Scope::All),
            "viewer1",
        );
        let state = ApiState {
            store,
            logs: None,
            flows: None,
            ipasn: crate::ipasn::empty_handle(),
            nodes: Arc::new(StaticNodeList::demo()),
            alerts: Arc::new(AlertManager::new()),
            host_sample: Arc::new(std::sync::Mutex::new(None)),
            admin: None,
            sessions,
            login_throttle: Arc::new(LoginThrottle::new()),
            history: None,
            ack: None,
            events: None,
            public_dashboard: false,
            is_leader: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            oidc: None,
            oidc_flight: Arc::new(crate::oidc::OidcFlight::new()),
            enable_mcp: false,
            rca: None,
        };
        (state, token)
    }

    /// A private (auth-required) state plus a freshly issued token for `role` — for RBAC tests on
    /// admin-only writes (e.g. the Shared Dashboard PUT).
    fn state_with_role_token(role: yagra_common::Role) -> (ApiState, String) {
        use yagra_common::{Principal, Scope};
        let sessions = Arc::new(SessionStore::new());
        let token = sessions.issue(Uuid::new_v4(), Principal::new(role, Scope::All), "u1");
        let state = ApiState {
            store: Arc::new(InMemorySink::default()),
            logs: None,
            flows: None,
            ipasn: crate::ipasn::empty_handle(),
            nodes: Arc::new(StaticNodeList::demo()),
            alerts: Arc::new(AlertManager::new()),
            host_sample: Arc::new(std::sync::Mutex::new(None)),
            admin: None,
            sessions,
            login_throttle: Arc::new(LoginThrottle::new()),
            history: None,
            ack: None,
            events: None,
            public_dashboard: false,
            is_leader: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            oidc: None,
            oidc_flight: Arc::new(crate::oidc::OidcFlight::new()),
            enable_mcp: false,
            rca: None,
        };
        (state, token)
    }

    fn store_with_reading(node: NodeId, metric: &str, value: f64) -> Arc<dyn MetricStore> {
        let sink = InMemorySink::default();
        sink.ingest(&PollResult {
            job_id: Uuid::nil(),
            node_id: node,
            at_unix_ms: 0,
            outcome: CheckOutcome::Reachable,
            samples: vec![Sample::gauge(metric, value)],
            interfaces: Vec::new(),
            sys_descr: None,
            dns_chain: None,
            poller_id: None,
            trace_context: Default::default(),
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

    #[tokio::test]
    async fn flow_endpoint_returns_503_when_flow_store_disabled() {
        // Default state has `flows: None` (default-OFF) — the flow API must 503, not 500/panic.
        let node = Uuid::nil();
        let app = router(state_with(Arc::new(InMemorySink::default())));
        let resp = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/v1/nodes/{node}/flow/top-talkers"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        let json = body_json(resp).await;
        assert_eq!(json["error"]["code"], "flow_unavailable");
    }

    #[tokio::test]
    async fn flow_top_talkers_returns_data_when_enabled() {
        use crate::flowstore::{FlowRow, FlowStore, InMemoryFlowStore};
        let node = Uuid::from_u128(7);
        let store = InMemoryFlowStore::default();
        let mk = |src: &str, bytes: u64| FlowRow {
            node_id: node,
            ts_unix_ms: 1_000_000,
            exporter_ip: "192.168.1.1".parse().unwrap(),
            if_index: 2,
            src_ip: src.parse().unwrap(),
            dst_ip: "8.8.8.8".parse().unwrap(),
            src_port: 40000,
            dst_port: 443,
            proto: 6,
            tos: 0,
            src_as: 0,
            dst_as: 0,
            bytes,
            packets: bytes / 100,
            flows: 1,
        };
        store
            .insert_batch(&[mk("10.0.0.2", 5000), mk("10.0.0.3", 100)])
            .await
            .unwrap();

        let mut st = state_with(Arc::new(InMemorySink::default()));
        st.flows = Some(Arc::new(store));
        let app = router(st);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/api/v1/nodes/{node}/flow/top-talkers?from=0&to=100000"
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert_eq!(json[0]["addr"], "10.0.0.2");
        assert_eq!(json[0]["bytes"], 5000);
    }

    #[tokio::test]
    async fn flow_top_as_ranks_and_filters() {
        // Endpoint shape + ordering + protocol filter (AS-name resolution is covered by the
        // `ipasn` unit tests; here `st.ipasn` is None so names stay unset).
        use crate::flowstore::{FlowRow, FlowStore, InMemoryFlowStore};
        let node = Uuid::from_u128(9);
        let store = InMemoryFlowStore::default();
        let mk = |dst: &str, dst_as: u32, proto: u8, bytes: u64| FlowRow {
            node_id: node,
            ts_unix_ms: 1_000_000,
            exporter_ip: "192.168.1.1".parse().unwrap(),
            if_index: 2,
            src_ip: "10.0.0.2".parse().unwrap(),
            dst_ip: dst.parse().unwrap(),
            src_port: 40000,
            dst_port: 443,
            proto,
            tos: 0,
            src_as: 0,
            dst_as,
            bytes,
            packets: bytes / 100,
            flows: 1,
        };
        store
            .insert_batch(&[mk("8.8.8.8", 15169, 6, 5000), mk("1.1.1.1", 13335, 17, 100)])
            .await
            .unwrap();

        let mut st = state_with(Arc::new(InMemorySink::default()));
        st.flows = Some(Arc::new(store));
        let app = router(st);
        // Default dir = dst: AS 15169 (5000) ranks above AS 13335 (100).
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/api/v1/nodes/{node}/flow/top-as?from=0&to=100000"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert_eq!(json[0]["asn"], 15169);
        assert_eq!(json[0]["bytes"], 5000);

        // Protocol filter (UDP) narrows to AS 13335 only.
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/api/v1/nodes/{node}/flow/top-as?from=0&to=100000&proto=17"
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert_eq!(json.as_array().unwrap().len(), 1);
        assert_eq!(json[0]["asn"], 13335);

        // AS filter narrows to just that ASN (drill-down from the Top-AS card).
        let resp = app
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/api/v1/nodes/{node}/flow/top-as?from=0&to=100000&asn=13335"
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert_eq!(json.as_array().unwrap().len(), 1);
        assert_eq!(json[0]["asn"], 13335);
    }

    #[tokio::test]
    async fn flow_conversations_carry_as() {
        // The conversations endpoint surfaces per-flow src/dst ASN (name-resolution is the
        // `ipasn` layer's job; `st.ipasn` is None here so the `*_as_name` stay null).
        use crate::flowstore::{FlowRow, FlowStore, InMemoryFlowStore};
        let node = Uuid::from_u128(11);
        let store = InMemoryFlowStore::default();
        store
            .insert_batch(&[FlowRow {
                node_id: node,
                ts_unix_ms: 1_000_000,
                exporter_ip: "192.168.1.1".parse().unwrap(),
                if_index: 2,
                src_ip: "10.0.0.2".parse().unwrap(),
                dst_ip: "17.248.221.6".parse().unwrap(),
                src_port: 40000,
                dst_port: 443,
                proto: 6,
                tos: 0,
                src_as: 0,     // internal host, unknown AS
                dst_as: 15169, // Google
                bytes: 5000,
                packets: 50,
                flows: 1,
            }])
            .await
            .unwrap();

        let mut st = state_with(Arc::new(InMemorySink::default()));
        st.flows = Some(Arc::new(store));
        let app = router(st);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/api/v1/nodes/{node}/flow/conversations?from=0&to=100000"
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert_eq!(json[0]["src"], "10.0.0.2");
        assert_eq!(json[0]["dst"], "17.248.221.6");
        assert_eq!(json[0]["src_asn"], 0);
        assert_eq!(json[0]["dst_asn"], 15169);
        // No IP→ASN table wired ⇒ names stay null.
        assert!(json[0]["src_as_name"].is_null());
        assert!(json[0]["dst_as_name"].is_null());
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
    fn clamp_range_step_bounds_point_count_but_honors_floors() {
        // A normal request keeps its step (well under the point cap).
        assert_eq!(clamp_range_step(0, 3600, 60, 1), 60);
        // The caller's own minimum floor is respected when the requested step is smaller.
        assert_eq!(clamp_range_step(0, 3600, 5, 60), 60);
        // A huge span at a tiny step is forced up so it yields at most MAX_RANGE_POINTS samples.
        let from = 0;
        let to = 100 * 24 * 3600; // 100 days
        let step = clamp_range_step(from, to, 1, 1);
        assert!(step > 1, "tiny step over a huge span is raised");
        assert!(
            (to - from) / (step as i64) <= MAX_RANGE_POINTS,
            "point count stays within the cap"
        );
        // Degenerate/backwards ranges never divide by zero or underflow; step floors at 1.
        assert_eq!(clamp_range_step(500, 500, 0, 0), 1);
        assert_eq!(clamp_range_step(500, 100, 0, 0), 1);
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
    async fn node_names_batch_resolves_and_is_bounded() {
        // Route registers (no collision with /nodes/:node_id), the JSON body parses, and the
        // read gate is open in public-dashboard mode. Without an admin repo no names resolve, so
        // the response is a well-formed empty array — the client then keeps the raw id (S12).
        let app = router(state_with(Arc::new(InMemorySink::default())));
        let id = Uuid::new_v4();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/node-names")
                    .header("content-type", "application/json")
                    .body(Body::from(format!(r#"{{"ids":["{id}"]}}"#)))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(body_json(resp).await.as_array().unwrap().is_empty());
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

    // `poll_now_unavailable_without_admin` moved to `api/nodes.rs` when that handler did, and
    // split in two: it asserted only that an *anonymous* caller sees 503, which is the ordering
    // this migration deliberately reverses. The replacements pin both halves — anonymous ⇒ 401
    // (availability is not disclosed before authentication), authorized ⇒ 503.

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

    // `create_node` moved to `api/nodes.rs` and is guard-first now, so its skeleton-mode answer to
    // an *anonymous* caller is 401 rather than 503 — pinned from both sides by
    // `nodes::tests::every_inventory_write_is_authenticated_before_anything_else` and
    // `nodes::tests::editing_the_inventory_takes_manage_config`.

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
    async fn report_sections_catalog_is_open_read() {
        // The section catalog is static — served even in skeleton mode — and drives the builder.
        let app = router(state_with(Arc::new(InMemorySink::default())));
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/reports/sections")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        let kinds: Vec<&str> = json
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["kind"].as_str().unwrap())
            .collect();
        assert!(kinds.contains(&"availability-summary"));
        assert!(kinds.contains(&"top-cpu"));
    }

    #[tokio::test]
    async fn report_runs_empty_without_admin() {
        // No DB wired ⇒ the saved-reports list is an empty array (not a 503), like analysis jobs.
        let app = router(state_with(Arc::new(InMemorySink::default())));
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/reports/runs")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(body_json(resp).await, serde_json::json!([]));
    }

    #[tokio::test]
    async fn create_report_definition_forbidden_for_non_admin() {
        // Reports are a shared resource: writing a definition needs ManageConfig (admin only). A
        // Viewer is rejected before any DB/admin work (decision is testable without a DB).
        let (state, token) = state_with_role_token(yagra_common::Role::Viewer);
        let resp = router(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/reports/definitions")
                    .header(AUTHORIZATION, format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"name":"Weekly","spec":{"version":1,"sections":[]}}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
        assert_eq!(body_json(resp).await["error"]["code"], "forbidden");
    }

    #[tokio::test]
    async fn create_report_schedule_admin_passes_authorization() {
        // Admin clears the RBAC gate; with no DB wired it hits the admin/503 fallback — proving
        // ManageConfig admits Admin (auth passed → 503, not 403).
        let (state, token) = state_with_role_token(yagra_common::Role::Admin);
        let resp = router(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/reports/schedules")
                    .header(AUTHORIZATION, format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"definition_id":"00000000-0000-0000-0000-000000000000","frequency":"daily","at_hour":9,"at_minute":0}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body_json(resp).await["error"]["code"], "admin_unavailable");
    }

    #[tokio::test]
    async fn readyz_reflects_leadership() {
        // Leader (the default in test state) ⇒ ready.
        let app = router(state_with(Arc::new(InMemorySink::default())));
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/readyz")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        // Standby (advisory lock not held) ⇒ 503 so a load balancer routes elsewhere.
        let mut state = state_with(Arc::new(InMemorySink::default()));
        state.is_leader = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let app = router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/readyz")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    // ── Passive events API ──

    #[tokio::test]
    async fn ingest_webhook_unavailable_without_engine() {
        // Skeleton mode has no event engine ⇒ ingest is 503 (even with a token).
        let app = router(state_with(Arc::new(InMemorySink::default())));
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/ingest/webhook/00000000-0000-0000-0000-000000000001")
                    .header(AUTHORIZATION, "Bearer some-token")
                    .body(Body::from(r#"{"message":"disk full"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn event_rule_test_endpoint_matches_and_reports_errors_in_band() {
        // The tester needs only ManageConfig (no DB): substring hit, regex miss, and a
        // regex compile error reported in-band for the UI.
        let (state, token) = state_with_role_token(yagra_common::Role::Admin);
        let app = router(state);

        let post = |body: &str| {
            Request::builder()
                .method("POST")
                .uri("/api/v1/event-rules/test")
                .header(AUTHORIZATION, format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(body.to_owned()))
                .unwrap()
        };

        let resp = app
            .clone()
            .oneshot(post(
                r#"{"match_kind":"substring","pattern":"link down","clear_pattern":"link up","sample":"chassisd: link down on ge-0/0/1"}"#,
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert_eq!(json["matched"], true);
        assert_eq!(json["clear_matched"], false);
        assert!(json["error"].is_null());

        let resp = app
            .clone()
            .oneshot(post(
                r#"{"match_kind":"regex","pattern":"(unclosed","sample":"x"}"#,
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert_eq!(json["matched"], false);
        assert!(json["error"].as_str().unwrap().starts_with("pattern:"));
    }

    #[tokio::test]
    async fn event_rule_test_forbidden_for_viewer() {
        let (state, token) = state_with_role_token(yagra_common::Role::Viewer);
        let resp = router(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/event-rules/test")
                    .header(AUTHORIZATION, format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"match_kind":"substring","pattern":"x","sample":"x"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[test]
    fn extract_webhook_text_heuristics() {
        // JSON object: first present message|text|summary string field wins.
        let (t, trunc) = extract_webhook_text(br#"{"summary":"s","message":"disk full"}"#);
        assert_eq!(t, "disk full");
        assert!(!trunc);
        let (t, _) = extract_webhook_text(br#"{"text":"from text"}"#);
        assert_eq!(t, "from text");
        // JSON object without those fields → compact JSON (still matchable).
        let (t, _) = extract_webhook_text(br#"{"status":"firing"}"#);
        assert!(t.contains("\"firing\""));
        // Non-JSON → raw body.
        let (t, _) = extract_webhook_text(b"plain text alert");
        assert_eq!(t, "plain text alert");
        // Oversized → clipped + flagged.
        let big = format!("{{\"message\":\"{}\"}}", "a".repeat(5000));
        let (t, trunc) = extract_webhook_text(big.as_bytes());
        assert_eq!(t.chars().count(), EVENT_TEXT_MAX_CHARS);
        assert!(trunc);
    }

    #[test]
    fn validate_event_rule_bounds_and_patterns() {
        let body = |json: &str| -> EventRuleBody { serde_json::from_str(json).unwrap() };
        // Minimal valid rule with defaults.
        let ok =
            body(r#"{"name":"r","match_kind":"substring","pattern":"x","severity":"warning"}"#);
        let p = validate_event_rule(&ok).unwrap();
        assert!(p.enabled);
        assert_eq!(p.ttl_secs, 1800);
        assert_eq!(p.min_count, 1);
        assert_eq!(p.window_secs, 60);

        for bad in [
            r#"{"name":"","match_kind":"substring","pattern":"x","severity":"warning"}"#,
            r#"{"name":"r","match_kind":"glob","pattern":"x","severity":"warning"}"#,
            r#"{"name":"r","match_kind":"regex","pattern":"(bad","severity":"warning"}"#,
            r#"{"name":"r","match_kind":"regex","pattern":"ok","clear_pattern":"(bad","severity":"warning"}"#,
            r#"{"name":"r","match_kind":"substring","pattern":"x","severity":"fatal"}"#,
            r#"{"name":"r","match_kind":"substring","pattern":"x","severity":"warning","source_kind":"smoke"}"#,
            r#"{"name":"r","match_kind":"substring","pattern":"x","severity":"warning","ttl_secs":10}"#,
            r#"{"name":"r","match_kind":"substring","pattern":"x","severity":"warning","min_count":0}"#,
            r#"{"name":"r","match_kind":"substring","pattern":"x","severity":"warning","window_secs":9999}"#,
        ] {
            assert!(
                validate_event_rule(&body(bad)).is_err(),
                "must reject: {bad}"
            );
        }
    }

    #[tokio::test]
    async fn close_event_alert_unavailable_without_engine() {
        let (state, token) = state_with_role_token(yagra_common::Role::Operator);
        let resp = router(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/events/alerts/close")
                    .header(AUTHORIZATION, format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"node":"00000000-0000-0000-0000-000000000001","check":"00000000-0000-0000-0000-000000000002"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    // ── Distributed poller pool (ADR-009/020) — Pollers API ───────────────────

    #[test]
    fn expensive_reads_wearing_post_do_not_dirty_the_config_generation() {
        // S6 caches a full-fleet alert-config rebuild keyed on this generation. Launching an
        // analysis or asking for an explanation changes no monitoring config, so counting either
        // as a config write would force a fleet-wide rebuild on the next 30s tick — during an
        // incident, which is when the fleet is largest and busiest.
        assert!(!changes_monitoring_config("/api/v1/rca"));
        assert!(!changes_monitoring_config("/api/v1/analysis/jobs"));
        assert!(!changes_monitoring_config(
            "/api/v1/analysis/jobs/abc/cancel"
        ));
        // Everything else still invalidates — a deny-list, so a new write handler is dirty by
        // default rather than silently skipping a rebuild it needed.
        for path in [
            "/api/v1/nodes",
            "/api/v1/nodes/abc/collection",
            "/api/v1/thresholds",
            "/api/v1/groups",
            "/api/v1/llm/config",
            "/api/v1/some-endpoint-invented-tomorrow",
        ] {
            assert!(changes_monitoring_config(path), "{path} must invalidate");
        }
    }

    // ── AI-assisted RCA (ADR-029) ────────────────────────────────────────────

    #[tokio::test]
    async fn rca_endpoints_are_absent_by_default_rather_than_half_working() {
        // The most important property of this feature: an installation that has configured no
        // provider makes no outbound call and exposes no half-enabled surface. With no
        // orchestrator every RCA route answers 503 — and it *answers*, so the routes are
        // registered (a 404 here would mean a path typo, which this also catches).
        let (state, token) = state_with_role_token(yagra_common::Role::Admin);
        let app = router(state);
        let node = Uuid::nil();
        let cases: Vec<(&str, String, Option<String>)> = vec![
            ("GET", "/api/v1/llm/config".to_owned(), None),
            (
                "PUT",
                "/api/v1/llm/config".to_owned(),
                Some(r#"{"provider":"claude","model":"m","api_key":"k"}"#.to_owned()),
            ),
            ("POST", "/api/v1/llm/test".to_owned(), Some("{}".to_owned())),
            (
                "POST",
                "/api/v1/rca".to_owned(),
                Some(format!(r#"{{"node":"{node}","check":"{node}"}}"#)),
            ),
            ("GET", format!("/api/v1/rca/{node}"), None),
        ];
        for (method, uri, body) in cases {
            let mut req = Request::builder()
                .method(method)
                .uri(&uri)
                .header(AUTHORIZATION, format!("Bearer {token}"));
            if body.is_some() {
                req = req.header("content-type", "application/json");
            }
            let resp = app
                .clone()
                .oneshot(req.body(body.map_or(Body::empty(), Body::from)).unwrap())
                .await
                .unwrap();
            assert_eq!(
                resp.status(),
                StatusCode::SERVICE_UNAVAILABLE,
                "{method} {uri} must be unavailable — not enabled, not missing"
            );
        }
    }

    #[tokio::test]
    async fn an_anonymous_caller_cannot_learn_whether_ai_is_configured() {
        // Authorization is checked before availability in these handlers, so an unauthenticated
        // prober gets a flat 401 rather than a 503-vs-200 that would tell them whether this
        // installation has an LLM wired up and which vendor it is.
        let (state, _token) = state_with_role_token(yagra_common::Role::Admin);
        let resp = router(state)
            .oneshot(
                Request::builder()
                    .uri("/api/v1/llm/config")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn generating_an_explanation_needs_more_than_view() {
        // Generation spends money and sends the incident to a third party, so a Viewer cannot
        // trigger it. The 403 lands before any store or provider work.
        let (state, token) = state_with_role_token(yagra_common::Role::Viewer);
        let node = Uuid::nil();
        let resp = router(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/rca")
                    .header(AUTHORIZATION, format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(format!(
                        r#"{{"node":"{node}","check":"{node}"}}"#
                    )))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
        assert_eq!(body_json(resp).await["error"]["code"], "forbidden");
    }

    #[tokio::test]
    async fn an_operator_may_generate_but_not_configure() {
        // The permission split. The people carrying the pager can ask for an explanation
        // (AckAlerts ⇒ 503: authorized, merely unconfigured), while choosing the vendor and
        // holding its credential stays with an admin (ManageConfig ⇒ 403 for an operator).
        let (state, token) = state_with_role_token(yagra_common::Role::Operator);
        let app = router(state);
        let node = Uuid::nil();
        let generate = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/rca")
                    .header(AUTHORIZATION, format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(format!(
                        r#"{{"node":"{node}","check":"{node}"}}"#
                    )))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(generate.status(), StatusCode::SERVICE_UNAVAILABLE);

        let configure = app
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/v1/llm/config")
                    .header(AUTHORIZATION, format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"provider":"claude","model":"m"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(configure.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn reading_a_stored_report_is_open_to_viewers() {
        // Deliberate: the explanation is display-only text, so anyone who can see the alert can
        // see the analysis of it. Only *producing* one costs anything.
        let (state, token) = state_with_role_token(yagra_common::Role::Viewer);
        let resp = router(state)
            .oneshot(
                Request::builder()
                    .uri(format!("/api/v1/rca/{}", Uuid::nil()))
                    .header(AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        // Authorized (not 403) — unavailable only because nothing is configured.
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn the_bootstrap_config_reports_rca_as_off() {
        // The WebUI hides the "Explain this incident" action on this flag, so an operator is never
        // offered a button that can only 503.
        let app = router(state_with(Arc::new(InMemorySink::default())));
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/config")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(body_json(resp).await["rca_enabled"], false);
    }
}
