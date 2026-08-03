// SPDX-License-Identifier: AGPL-3.0-only
//! Northbound REST API (`/api/v1`).
//!
//! Path-versioned (ADR-019). Responses are JSON; errors use the fixed envelope
//! `{"error": {"code", "message"}}` so clients never see a raw internal error. Readings
//! come from the [`MetricStore`] (VictoriaMetrics live, in-memory for the skeleton) and
//! the inventory from a [`NodeListing`]. A node's display state and the alert endpoints are
//! served from the live [`AlertManager`] (committed liveness + threshold roll-up + active
//! alerts). Cursor pagination is in, and so is **RBAC group scoping** (ADR-014): every route
//! declares how it treats the caller's scope in [`route_table`], and [`scope`] is the one place
//! that resolves it — see that module for what each rule means and where it is applied.
//!
//! ## Layout
//!
//! This module was a single 13.7k-line file holding 28 unrelated domains separated only by comment
//! banners, with one 393-line `router()` expression registering all ~200 method/path pairs — so
//! every feature branch edited the same two places and conflicted. **That split is finished**
//! (2026-07-31): every handler lives in an `api/<domain>.rs` that owns its handlers, its DTOs, its
//! `#[utoipa::path]` fragment and its own [`axum::Router`], and [`router`] merges them. What is
//! left here is what belongs to no domain — the state structs, [`router`], [`audit_mw`], and the
//! two unauthenticated probes.
//!
//! Cross-cutting pieces live beside it:
//!  - [`error`] — the typed [`ApiError`] and the ADR-019 envelope. Handlers return `ApiResult<T>`
//!    and propagate with `?`; there is no hand-built-`Response` path left.
//!  - [`extract`] — auth/availability guards as extractors, so a missing guard is a compile error
//!    rather than a silent hole.
//!  - [`route_table`] — the full method/path inventory, pinned in both directions against the
//!    router *and* the OpenAPI document, so moving a domain out cannot quietly drop an endpoint
//!    (which would still compile) nor leave the published contract describing it.
//!  - [`openapi`] — the generated document (ADR-035); the WebUI's types come from it.

pub(crate) mod alerts;
pub(crate) mod analysis;
mod api_tokens;
mod audit;
pub(crate) mod checks;
mod classification;
mod collection;
mod config_bundle;
mod credentials;
mod dashboard;
mod discovery;
mod error;
pub(crate) mod eventlog;
mod events;
pub(crate) mod extract;
pub(crate) mod fleet;
pub(crate) mod flow;
mod forwarding;
pub(crate) mod groups;
mod health;
pub(crate) mod maintenance;
mod meraki;
pub(crate) mod metrics;
mod mib;
pub(crate) mod neighbors;
pub(crate) mod nodes;
mod notifications;
mod oidc;
pub mod openapi;
mod pollers;
mod profiles;
mod rca;
mod reports;
mod retention;
#[cfg(test)]
mod route_table;
pub(crate) mod scope;
mod session;
mod system;
#[cfg(test)]
mod tests_support;
pub(crate) mod thresholds;
pub(crate) mod topology;
mod users;
pub(crate) mod util;

pub(crate) use error::error_response;
pub use error::{ApiError, ApiResult};
pub(crate) use extract::bearer;
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
use crate::reports::ReportRunner;
use crate::scheduler::PollDispatcher;
use crate::secrets::CredentialStore;
use crate::store::MetricStore;
use crate::thresholds::ThresholdStore;
use axum::{
    extract::{DefaultBodyLimit, Request, State},
    http::StatusCode,
    middleware::{self, Next},
    response::Response,
    routing::get,
    Router,
};
use std::sync::Arc;
use yagra_common::HostSample;

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
    /// Observed CDP/LLDP adjacency per node, and its append-on-change history (ADR-038).
    pub neighbors: Arc<crate::neighbors::NeighborRepo>,
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
    /// Configuration bundle export/import (ADR-040 decision 3): move a monitoring configuration
    /// between deployments. Reads and writes many tables in one transaction and carries no secrets.
    pub config_bundle: Arc<crate::config_bundle::ConfigBundleRepo>,
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
        // Metric reads + fleet Top-N rankings, in `api/metrics.rs`.
        .merge(metrics::routes())
        // Threshold rules — the one config table that grows with the fleet, so its list is capped.
        .merge(thresholds::routes())
        // Flow analysis (ADR-031) — the twelve `/flow/*` endpoints, in `api/flow.rs`.
        .merge(flow::routes())
        // Local user accounts + roles (admin-only), in `api/users.rs`.
        .merge(users::routes())
        // API tokens (PATs) for the MCP tool surface at `/mcp` (ADR-028) — this REST API rejects
        // them, so they are not a second way to call these endpoints. Admin-only;
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
        // Maintenance windows + mutes (alert suppression), in `api/maintenance.rs`.
        .merge(maintenance::routes())
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
        // The generated OpenAPI document itself (ADR-035) — unauthenticated, see `api/openapi.rs`.
        .merge(openapi::routes())
        .merge(discovery::routes())
        .merge(notifications::routes())
        .merge(rca::routes())
        .merge(forwarding::routes())
        .merge(groups::routes())
        .merge(profiles::routes())
        .merge(credentials::routes())
        .merge(pollers::routes())
        .merge(health::routes())
        .merge(reports::routes())
        .merge(neighbors::routes())
        .merge(retention::routes())
        // Configuration bundle export/import (ADR-040 decision 3): move a configuration between
        // deployments. Admin-only in both directions, and it carries no secrets.
        .merge(config_bundle::routes())
        .merge(meraki::routes())
        // Passive events: sources/rules CRUD + manual alert close, in `api/events.rs`.
        .merge(events::routes())
        // Machine-scoped webhook ingest, layered with its own small body cap: it is the one
        // endpoint an untrusted sender posts to, so the limit belongs on this path alone.
        .merge(events::ingest_route().layer(DefaultBodyLimit::max(events::WEBHOOK_BODY_LIMIT)))
        // Audit middleware: records every mutating /api/v1 request (who + method/path +
        // status) so new write endpoints are covered automatically (security.md).
        .layer(middleware::from_fn_with_state(state.clone(), audit_mw))
        // Bearer resolution, *outside* the audit layer so it runs first and both the audit record
        // and every guard read the same answer. Layers added later wrap earlier ones, so this is
        // the outermost of the two. It authenticates nothing by itself — see `extract::resolve_auth`.
        .layer(middleware::from_fn_with_state(
            state.clone(),
            resolve_auth_mw,
        ))
        .with_state(state)
}

/// Username recorded when a mutating request carries no valid session.
const AUDIT_ANONYMOUS: &str = "(unauthenticated)";

/// Record one audit entry, best-effort: auditing must never take the API down, so
/// failures are logged and swallowed.
/// Middleware: append an audit row for every mutating `/api/v1` request. Auth endpoints
/// are excluded here — login is recorded by its handler (with the attempted username,
/// never the credential). Reads are not audited.
/// Resolve the request's bearer token once and stash the answer for the guards and the audit log.
///
/// A plain middleware rather than work inside each extractor because an API token costs a database
/// round-trip: a handler taking two guards would otherwise verify the same credential twice, and
/// `audit_mw` — which runs outside the extractors entirely — would have to verify it a third time.
async fn resolve_auth_mw(State(st): State<ApiState>, mut req: Request, next: Next) -> Response {
    if let Some(auth) = extract::resolve_auth(&st, req.headers()).await {
        req.extensions_mut().insert(auth);
    }
    next.run(req).await
}

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
    // Resolve the actor before the handler runs (the request is consumed by it). Read from the
    // extension `resolve_auth_mw` already filled rather than looking the bearer up again — that
    // second lookup consulted the session store alone, so a mutating call authenticated by an API
    // token was recorded as anonymous.
    let username = if audited {
        req.extensions()
            .get::<extract::Authenticated>()
            .map(extract::Authenticated::audit_actor)
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
#[utoipa::path(
    get, path = "/healthz", tag = "meta", security(()),
    responses((status = 200, description = "This process is alive", content_type = "text/plain")),
)]
pub(super) async fn healthz() -> &'static str {
    "ok"
}

/// Readiness probe (ADR-016): `200` only when this core holds HA leadership — i.e. it is running the
/// coordinator + ingest and can serve live status. A standby returns `503` so a load balancer /
/// orchestrator routes traffic only to the active core. With HA off this core is always the leader,
/// so it always returns `200`. No auth, no store access (mirrors `/healthz`).
#[utoipa::path(
    get, path = "/readyz", tag = "meta", security(()),
    responses(
        (status = 200, description = "This core holds HA leadership and can serve live status"),
        (status = 503, description = "This core is a standby — route traffic elsewhere"),
    ),
)]
pub(super) async fn readyz(State(st): State<ApiState>) -> StatusCode {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::StaticNodeList;
    use crate::sink::InMemorySink;
    use axum::body::{to_bytes, Body};
    use axum::http::{header::AUTHORIZATION, Request};
    use tower::ServiceExt; // for `oneshot`
    use uuid::Uuid;
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
            neighbors: None,
            observational: false,
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
    async fn a_stored_report_is_no_longer_readable_by_id() {
        // `GET /api/v1/rca/{id}` was removed: nothing ever called it — the modal renders the report
        // returned by the `POST` that generated it, and there is no history view to open one from.
        // It now 404s like any other unrouted path, which is what this pins; the report itself is
        // still stored and still comes back from `POST /api/v1/rca` (its cache hit re-serves it).
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
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
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
