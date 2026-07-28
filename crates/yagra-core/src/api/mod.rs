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
mod error;
pub(crate) mod eventlog;
pub(crate) mod extract;
pub(crate) mod fleet;
mod flow;
pub(crate) mod maintenance;
pub(crate) mod metrics;
pub(crate) mod nodes;
#[cfg(test)]
mod route_table;
#[cfg(test)]
mod tests_support;
pub(crate) mod topology;
mod users;
mod util;

pub(crate) use error::{error_response, internal, not_found, unavailable};
pub use error::{ApiError, ApiResult};
pub(crate) use extract::{authorize, bearer, current_username, require_leader, require_view};
pub(crate) use util::{now_unix_s, parse_rfc3339};

use crate::ack::AckRepo;
use crate::alerts::AlertManager;
use crate::analysis::AnalysisRunner;
use crate::audit::AuditRepo;
use crate::auth::{AuthError, LoginThrottle, SessionStore, UserStore};
use crate::classification::{ClassificationRepo, Classifier};
use crate::collection::{CollectionRepo, CreateTemplateOutcome};
use crate::coordinator::{Coordinator, PollerView};
use crate::dashboard::{DashboardRepo, SharedDashboardRepo};
use crate::discovery::DiscoveryRunner;
use crate::groups::{placement_order, would_create_cycle, GroupRepo, GroupType};
use crate::history::AlertHistoryStore;
use crate::logstore::LogStore;
use crate::maintenance::MaintenanceRepo;
use crate::mib::MibRepo;
use crate::notifications::{ChannelConfig, NotificationRepo};
use crate::pollers::{PollerRepo, PollerRow};
use crate::poolres::{PoolResolver, PoolSource};
use crate::repo::{NodeListing, NodeRepo};
use crate::reports::{self, ReportRunner, ScheduleInput};
use crate::scheduler::PollDispatcher;
use crate::secrets::CredentialStore;
use crate::store::{MetricPoint, MetricStore};
use crate::thresholds::ThresholdStore;
use axum::{
    extract::{DefaultBodyLimit, Path, Query, Request, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
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
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use uuid::Uuid;
use yagra_common::{
    is_ssrf_blocked, resolve_collection_set, DiskUsage, DnsCheckConfig, HostSample, Node, NodeId,
    NodeState, Permission, ProfileCategory, Role, SeriesKey, Severity, UrlCheckConfig,
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
    pub coordinator: Arc<Coordinator<yagra_bus::NatsBus>>,
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
        .route("/api/v1/config", get(get_config).put(update_config))
        .route("/api/v1/nodes", get(list_nodes).post(create_node))
        // Static `/nodes/search` + `/nodes/by-group` before the `/nodes/:node_id` param route
        // (matchit prioritizes the static segment, but keep them adjacent for clarity).
        .route("/api/v1/nodes/search", get(search_nodes))
        .route("/api/v1/nodes/by-group", get(list_group_nodes))
        .route("/api/v1/node-names", post(node_names_batch))
        .route("/api/v1/nodes/:node_id", get(get_node).delete(delete_node))
        .route("/api/v1/nodes/:node_id/status", get(get_node_status))
        // The manual "poll now" action, in `api/nodes.rs` (the rest of the node domain follows).
        .merge(nodes::routes())
        .route("/api/v1/nodes/:node_id/bindings", put(set_node_bindings))
        // Which pool the node effectively belongs to, and which poller currently holds it.
        .route("/api/v1/nodes/:node_id/assignment", get(node_assignment))
        .route(
            "/api/v1/nodes/:node_id/url-check",
            get(get_url_check)
                .put(set_url_check)
                .delete(delete_url_check),
        )
        .route("/api/v1/url-monitors", post(create_url_monitor))
        // DNS name-resolution monitoring (ADR-033).
        .route(
            "/api/v1/nodes/:node_id/dns-check",
            get(get_dns_check)
                .put(set_dns_check)
                .delete(delete_dns_check),
        )
        .route("/api/v1/nodes/:node_id/dns-chain", get(get_dns_chain))
        .route(
            "/api/v1/nodes/:node_id/dns-chain/history",
            get(list_dns_chain_history),
        )
        .route("/api/v1/dns-monitors", post(create_dns_monitor))
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
        .route("/api/v1/nodes/:node_id/group", put(set_node_group))
        // Poll-pool assignment from the inventory tree (ADR-009/020). Single-field writes, so a
        // pool move can't clobber the node's profile/credential/vendor/model.
        .route("/api/v1/nodes/:node_id/pool", put(set_node_pool))
        .route("/api/v1/pools", get(list_pools))
        .route("/api/v1/nodes/:node_id/parent", put(set_node_parent))
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
        .route("/api/v1/node-groups/:id/pool", put(set_node_group_pool))
        .route("/api/v1/profiles", get(list_profiles).post(create_profile))
        .route(
            "/api/v1/profiles/:id",
            put(update_profile).delete(delete_profile),
        )
        // Metric reads + fleet Top-N rankings, in `api/metrics.rs`.
        .merge(metrics::routes())
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
        // Flow analysis (ADR-031) — the twelve `/flow/*` endpoints, in `api/flow.rs`.
        .merge(flow::routes())
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
        // Local user accounts + roles (admin-only), in `api/users.rs`.
        .merge(users::routes())
        // API tokens (PATs) for non-browser clients / the MCP tool surface (ADR-028). Admin-only;
        // issuance/revocation are audited by `audit_mw`. The raw token is returned once on create.
        .route(
            "/api/v1/api-tokens",
            get(list_api_tokens).post(create_api_token),
        )
        .route("/api/v1/api-tokens/:id", delete(revoke_api_token))
        // Forwarding ("tee", ADR-034): relay received syslog/traps to external collectors.
        // `ManageConfig`-gated and audited by `audit_mw` — a destination sends log bodies, which
        // routinely carry credentials, off-box.
        .route(
            "/api/v1/forwarding/destinations",
            get(list_forward_destinations).post(create_forward_destination),
        )
        .route(
            "/api/v1/forwarding/destinations/:id",
            put(update_forward_destination).delete(delete_forward_destination),
        )
        .route(
            "/api/v1/forwarding/destinations/:id/test",
            post(test_forward_destination),
        )
        .route("/api/v1/forwarding/status", get(forwarding_status))
        .route("/api/v1/auth/login", post(login))
        .route("/api/v1/auth/logout", post(logout))
        .route("/api/v1/auth/me", get(auth_me))
        // OIDC login (external IdP, ADR-010 Phase 3). Both are unauthenticated (pre-session), guarded
        // by the CSRF `state` + PKCE + nonce in the flow itself.
        .route("/api/v1/auth/oidc/authorize", get(oidc_authorize))
        .route("/api/v1/auth/oidc/callback", post(oidc_callback))
        // Provider config (admin-only). client_secret is write-only (never returned).
        .route(
            "/api/v1/settings/oidc",
            get(list_oidc_providers).post(create_oidc_provider),
        )
        .route(
            "/api/v1/settings/oidc/:id",
            put(update_oidc_provider).delete(delete_oidc_provider),
        )
        // AI-assisted RCA (ADR-029). Provider config is admin-only and its credential is
        // write-only; generation is `AckAlerts` (the people who actually work incidents) plus the
        // orchestrator's own rate + concurrency caps, and reading a stored report is `View`.
        .route(
            "/api/v1/llm/config",
            get(get_llm_config).put(put_llm_config),
        )
        .route("/api/v1/llm/test", post(test_llm_provider))
        .route("/api/v1/rca", post(create_rca))
        .route("/api/v1/rca/:id", get(get_rca))
        .route("/api/v1/roles", get(list_roles))
        // Alerts: active list, history + aggregations, inbound ack, and the two SSE streams,
        // in `api/alerts.rs`.
        .merge(alerts::routes())
        // The dependency graph, in `api/topology.rs`.
        .merge(topology::routes())
        // Fleet-wide rollups (summary / per-group / coverage / timeline), in `api/fleet.rs`.
        .merge(fleet::routes())
        .route("/api/v1/poller-health", get(poller_health))
        // Distributed poller pool (ADR-009/020): the fleet of registered pollers + per-pool summary.
        .route("/api/v1/pollers", get(list_pollers))
        // Static `/pollers/:id/nodes` drill-down alongside the `:id` param route.
        .route("/api/v1/pollers/:id/nodes", get(poller_nodes))
        .route("/api/v1/pollers/:id", delete(delete_poller))
        // Store-and-forward (Phase 3): recent core↔poller visibility outages (monitoring gaps).
        .route("/api/v1/monitoring-gaps", get(list_monitoring_gaps))
        .route("/api/v1/system-health", get(system_health))
        // Host self-observability: current CPU/load/mem/disk of core + each poller, and the trend
        // series behind the System Health "Host resources" charts.
        .route("/api/v1/system/hosts", get(system_hosts))
        .route(
            "/api/v1/system/hosts/:instance/metrics/range",
            get(host_metric_range),
        )
        .route("/api/v1/version", get(version))
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
        .route(
            "/api/v1/classification-rules",
            get(list_classification_rules).post(create_classification_rule),
        )
        .route(
            "/api/v1/classification-rules/:id",
            put(update_classification_rule).delete(delete_classification_rule),
        )
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
        .route("/api/v1/events/alerts/close", post(close_event_alert))
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

/// Product/build version for the WebUI's Settings ▸ About page. Public (no secrets) — just the
/// running `yagra-core` crate version, which inherits the workspace version (the canonical source
/// of truth). The WebUI shows its own build version alongside this, so a core/web skew during a
/// rolling upgrade is visible at a glance.
async fn version() -> Response {
    Json(serde_json::json!({
        "core": env!("CARGO_PKG_VERSION"),
    }))
    .into_response()
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
    let sso_enabled = match st.oidc.as_ref() {
        Some(oidc) => oidc.sso_enabled().await.unwrap_or(false),
        None => false,
    };
    // Whether an LLM provider is configured *and* enabled. The WebUI hides the "Explain this
    // incident" action when it is false, so an operator is never offered a button that can only
    // answer 503 — the default state of every installation.
    let rca_enabled = match st.rca.as_ref() {
        Some(rca) => rca.available().await,
        None => false,
    };
    Json(serde_json::json!({
        "public_dashboard": st.public_dashboard,
        "auth_available": st.admin.is_some(),
        "sso_enabled": sso_enabled,
        "rca_enabled": rca_enabled,
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

// ── Response shapes ──────────────────────────────────────────────────────────

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
    /// The node's **own** poll-pool; `null` ⇒ inherited from its folder, else the default pool.
    /// The tree's pool picker edits exactly this value, so it is what marks the active choice —
    /// the *effective* pool (and the poller holding the node) comes from `/nodes/:id/assignment`.
    pool: Option<String>,
    /// How this node is monitored, for the tree badge: `"meraki"` for a Cisco Meraki device,
    /// otherwise `"device"`. (URL monitors can reuse this later.)
    source: &'static str,
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

// ── Handlers ─────────────────────────────────────────────────────────────────

/// Keyset pagination query for the node list. An optional `search` substring switches the endpoint
/// into server-side name/address search mode (capped, single page, no cursor) — so the Nodes tree's
/// filter never full-loads the fleet into the browser (ui-conventions: search is server-side at
/// scale). Both modes return full `NodeSummary` rows so the tree can nest + color them.
#[derive(Deserialize)]
struct NodePageQuery {
    cursor: Option<Uuid>,
    limit: Option<i64>,
    search: Option<String>,
}

/// Cap on one batch name-resolution request so a client can't force an unbounded `IN (…)` query.
const NODE_NAMES_BATCH_MAX: usize = 1000;

/// Request body for `POST /api/v1/node-names`: the node ids whose display names to resolve.
#[derive(Deserialize)]
struct NodeNamesReq {
    ids: Vec<Uuid>,
}

/// One resolved node id → display name (unresolved ids are omitted; the caller keeps the raw id).
#[derive(Serialize)]
struct NodeNameEntry {
    id: Uuid,
    name: String,
}

/// Resolve a batch of node ids to their display names (S12). The shared `useEntityNames` resolver
/// and any table that renders a node reference by id use this so names resolve across the **whole**
/// fleet: the old path resolved against the first page of `list_nodes` (default 100), so a
/// reference to the 101st+ node silently degraded to a raw UUID. View-gated, read-only, bounded.
async fn node_names_batch(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Json(req): Json<NodeNamesReq>,
) -> Response {
    if let Some(resp) = require_view(&st, &headers) {
        return resp;
    }
    let mut ids = req.ids;
    ids.truncate(NODE_NAMES_BATCH_MAX);
    let names = match st.admin.as_ref() {
        Some(admin) => admin.repo.node_names(&ids).await.unwrap_or_default(),
        None => std::collections::HashMap::new(),
    };
    let out: Vec<NodeNameEntry> = ids
        .iter()
        .filter_map(|id| {
            names.get(id).map(|name| NodeNameEntry {
                id: *id,
                name: name.clone(),
            })
        })
        .collect();
    Json(out).into_response()
}

/// Query for the node-picker typeahead: `?q=<substr>&limit=<n>`. Empty/absent `q` returns the
/// first page ordered by name.
#[derive(Deserialize)]
struct NodeSearchQuery {
    q: Option<String>,
    limit: Option<i64>,
}

/// One node-picker result: id + display name + address. Deliberately excludes credentials/bindings
/// (security.md — the picker only needs to show and select a node).
#[derive(Serialize)]
struct NodeSearchResult {
    id: Uuid,
    name: String,
    address: String,
}

/// Server-side node search for the node-picker typeahead (A-2): match the name or address by
/// case-insensitive substring, capped, so a picker never loads the whole inventory into the browser
/// (ui-conventions: search is server-side at fleet scale). Now also backs the Nodes tree name filter
/// and the Troubleshoot scope picker, which previously full-loaded the fleet client-side.
/// View-gated, read-only; routes through the shared `NodeListing` so it also works in skeleton mode.
/// Only id/name/address leave the server.
async fn search_nodes(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Query(q): Query<NodeSearchQuery>,
) -> Response {
    if let Some(resp) = require_view(&st, &headers) {
        return resp;
    }
    let term = q.q.unwrap_or_default();
    let limit = q.limit.unwrap_or(50);
    match st.nodes.search(term.trim(), limit).await {
        Ok(nodes) => {
            let out: Vec<NodeSearchResult> = nodes
                .into_iter()
                .map(|n| NodeSearchResult {
                    id: n.id.as_uuid(),
                    name: n.name,
                    address: n.address.to_string(),
                })
                .collect();
            Json(out).into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "node search failed");
            internal("failed to search nodes")
        }
    }
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
    // Search mode: a non-empty `search` filters by name/address server-side and returns a single
    // capped page (no keyset cursor) — the tree's filter searches the fleet without loading it.
    if let Some(term) = q.search.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        return match st.nodes.search(term, limit).await {
            Ok(nodes) => {
                let out = build_node_summaries(&st, nodes).await;
                Json(serde_json::json!({ "nodes": out, "next_cursor": serde_json::Value::Null }))
                    .into_response()
            }
            Err(e) => {
                tracing::error!(error = %e, "failed to search nodes for list");
                internal("failed to list nodes")
            }
        };
    }
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
            let out = build_node_summaries(&st, nodes).await;
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

/// Enrich a batch of raw `Node` rows into UI `NodeSummary` rows: attach the live rolled-up display
/// state (the alert engine's opinion, else a coarse fresh-RTT fallback for a not-yet-observed node),
/// the per-node tree sort order, and the Meraki-source badge. Shared by the paged fleet list and the
/// per-group lazy tree load (A-3) so both paths produce identical rows. One batched TSDB probe for
/// the unobserved subset (no per-node round-trip); steady state adds no query.
async fn build_node_summaries(st: &ApiState, nodes: Vec<Node>) -> Vec<NodeSummary> {
    let states = st.alerts.node_states();
    // Per-node tree order + Meraki badge (admin/live only; skeleton mode → 0 order, no badge).
    let (orders, meraki_ids) = match st.admin.as_ref() {
        Some(admin) => {
            let ids: Vec<Uuid> = nodes.iter().map(|n| n.id.as_uuid()).collect();
            (
                admin.repo.node_sort_orders(&ids).await.unwrap_or_default(),
                admin
                    .meraki_devices
                    .filter_meraki(&ids)
                    .await
                    .unwrap_or_default(),
            )
        }
        None => (
            std::collections::HashMap::new(),
            std::collections::HashSet::new(),
        ),
    };
    let unobserved: Vec<NodeId> = nodes
        .iter()
        .filter(|n| !states.contains_key(&n.id))
        .map(|n| n.id)
        .collect();
    let fresh_fallback = fresh_fallback_ids(st, &unobserved).await;
    nodes
        .into_iter()
        .map(|n| {
            let state = match states.get(&n.id) {
                Some(s) => *s,
                None if fresh_fallback.contains(&n.id.as_uuid()) => NodeState::Ok,
                None => NodeState::Unknown,
            };
            let sort_order = orders.get(&n.id.as_uuid()).copied().unwrap_or(0.0);
            let source = if meraki_ids.contains(&n.id.as_uuid()) {
                "meraki"
            } else {
                "device"
            };
            NodeSummary {
                id: n.id,
                name: n.name,
                address: n.address.to_string(),
                state,
                vendor: n.vendor,
                model: n.model,
                group_id: n.group.map(|g| g.as_uuid()),
                sort_order,
                pool: n.pool,
                source,
            }
        })
        .collect()
}

/// Query for the per-group lazy tree load (A-3): `?group=<uuid>` returns that group's direct
/// members; an absent/empty `group` returns the ungrouped nodes (`group_id IS NULL`).
#[derive(Deserialize)]
struct GroupNodesQuery {
    group: Option<Uuid>,
}

/// Backstop cap on one group's direct-member load. The inventory tree lazy-loads a group's members
/// only when it is expanded (A-3), so this bounds a single pathologically-large group; the client
/// flags a truncated group. Normal groups are far below this.
const GROUP_NODES_CAP: i64 = 2000;

/// A group's direct members for the lazy inventory tree (A-3): the nodes whose `group_id` is exactly
/// `group` (or the ungrouped bucket), ordered by the tree's sort order, capped. Loaded on demand
/// when a group is expanded, so the initial page never pulls the whole fleet — it fetches the group
/// skeleton + per-group counts (`/fleet/group-summary`) and streams members per open group.
async fn list_group_nodes(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Query(q): Query<GroupNodesQuery>,
) -> Response {
    if let Some(resp) = require_view(&st, &headers) {
        return resp;
    }
    let Some(admin) = st.admin.as_ref() else {
        // Skeleton mode has no group membership; the demo node is ungrouped. Return it for the
        // ungrouped bucket, nothing for a specific group.
        let nodes = if q.group.is_none() {
            st.nodes
                .search("", GROUP_NODES_CAP)
                .await
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let out = build_node_summaries(&st, nodes).await;
        return Json(serde_json::json!({ "nodes": out, "truncated": false })).into_response();
    };
    match admin
        .repo
        .list_nodes_in_group(q.group, GROUP_NODES_CAP + 1)
        .await
    {
        Ok(mut nodes) => {
            let truncated = nodes.len() as i64 > GROUP_NODES_CAP;
            if truncated {
                nodes.truncate(GROUP_NODES_CAP as usize);
            }
            let out = build_node_summaries(&st, nodes).await;
            Json(serde_json::json!({ "nodes": out, "truncated": truncated })).into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to list group nodes");
            internal("failed to load group nodes")
        }
    }
}

/// Coarse fallback state for a node the alert engine has not observed yet: a recent ICMP
/// RTT reading ⇒ `ok`, otherwise `unknown`. Used only when the engine has no opinion (e.g.
/// just-added node, or skeleton mode where nothing polls). Single-node path (`get_node`); list
/// and topology use the batched [`fresh_fallback_ids`] to avoid a per-node TSDB round-trip.
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

/// Freshness window for the coarse fallback probe: a node with an ICMP RTT sample within this
/// window is treated as `ok`, else `unknown` (matches the fleet-coverage staleness horizon).
const FALLBACK_FRESH_SECS: u64 = 600;

/// Batched fallback probe for a page/graph of nodes. The alert engine already holds a state for
/// most; the `unobserved` remainder (just-added, or skeleton mode) would otherwise each cost a
/// `latest()` round-trip to the TSDB — N sequential HTTP queries, and right after a core restart
/// `states` is empty so *every* node hits this. Instead do a single `fresh_node_ids` query and
/// membership-test, exactly as `fleet_coverage` does. Skips the query entirely when nothing is
/// unobserved, so the steady-state path (engine knows every node) adds no TSDB call. Returns the
/// set of node ids with a recent RTT sample (⇒ `ok`; absent ⇒ `unknown`).
async fn fresh_fallback_ids(
    st: &ApiState,
    unobserved: &[NodeId],
) -> std::collections::HashSet<Uuid> {
    if unobserved.is_empty() {
        return std::collections::HashSet::new();
    }
    // Scope the freshness query to just this page's unobserved nodes (S20) — don't pull the whole
    // fleet's series back from the TSDB to answer about ≤ a page of nodes.
    let scope: Vec<Uuid> = unobserved.iter().map(NodeId::as_uuid).collect();
    st.store
        .fresh_node_ids_scoped("icmp_rtt_ms", FALLBACK_FRESH_SECS, &scope)
        .await
        .into_iter()
        .collect()
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
    /// The node's **own** poll-pool (ADR-009/020); `null` ⇒ it inherits from its folder, else the
    /// default pool. Deliberately the raw stored value, not the effective one, so the edit form can
    /// tell an explicit assignment from an inherited one — the *effective* pool (and which poller
    /// currently holds the node) comes from `GET /nodes/:id/assignment`.
    pool: Option<String>,
    /// URL-monitor config when this node is a URL monitor; `null` otherwise.
    url_check: Option<UrlCheckConfig>,
    /// DNS-monitor config when this node is a DNS monitor; `null` otherwise.
    dns_check: Option<DnsCheckConfig>,
    /// Cisco Meraki binding when this node is a Meraki device; `null` otherwise.
    meraki_device: Option<yagra_common::MerakiDeviceConfig>,
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
            // Best-effort: a URL/DNS-check or Meraki load failure shouldn't fail the node detail.
            let url_check = admin.url_checks.get(node_id).await.unwrap_or(None);
            let dns_check = admin.dns_checks.get(node_id).await.unwrap_or(None);
            let meraki_device = admin.meraki_devices.get(node_id).await.unwrap_or(None);
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
                pool: node.pool,
                url_check,
                dns_check,
                meraki_device,
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

// ── Distributed poller pool (ADR-009/020): the Pollers view ───────────────────

/// One poller in the `GET /api/v1/pollers` response — a merge of the live registry (current
/// status/telemetry) and the durable inventory (so an offline poller still lists). No secrets.
#[derive(Debug, Serialize, PartialEq)]
struct PollerInfo {
    /// Sanitized poller id (stable across restarts).
    id: String,
    /// Pool it serves (live view wins; else the durable row).
    pool: String,
    /// `"online"` when it is beating within the offline window, else `"offline"`.
    status: &'static str,
    /// Last durably-recorded contact (RFC 3339); `null` for a live poller not yet persisted (it
    /// registers within the 60s inventory-upsert throttle window).
    last_seen: Option<String>,
    /// First durably-recorded contact (RFC 3339); `null` if not yet persisted.
    first_seen: Option<String>,
    /// Build version from its latest heartbeat (or the durable row when it is offline).
    version: Option<String>,
    /// Working-set node count it last reported (0 when offline / never reported).
    working_set_nodes: u32,
    /// Working-set spec count it last reported.
    working_set_specs: u32,
    /// Poll results core has consumed from it since core started.
    results_total: u64,
    /// Current host CPU utilization % (0–100) from its latest heartbeat; `null` when the poller is
    /// offline or on an N-1 build without host telemetry.
    cpu_pct: Option<f64>,
    /// Current host memory-used % (0–100); `null` when unavailable.
    mem_used_pct: Option<f64>,
    /// Highest watched-filesystem used % (0–100); `null` when unavailable.
    disk_used_pct: Option<f64>,
}

/// One pool in the `GET /api/v1/pollers` response — node count vs. live pollers, its dispatch mode,
/// and a warning when it has nodes but no live poller (they would go unmonitored).
#[derive(Debug, Serialize, PartialEq)]
struct PoolSummary {
    /// Pool name (`default` for unassigned nodes).
    pool: String,
    /// Non-Meraki nodes assigned to this pool.
    nodes: usize,
    /// Live (online) pollers serving this pool.
    live_pollers: usize,
    /// `"working_set"` when a live poller serves it, else `"legacy"` (per-job fallback).
    mode: &'static str,
    /// `"nodes_without_live_poller"` when the pool has nodes but no live poller, else `null`.
    warning: Option<&'static str>,
}

/// The `GET /api/v1/pollers` body: the fleet of pollers + the per-pool summary.
#[derive(Debug, Serialize, PartialEq)]
struct PollersResponse {
    pollers: Vec<PollerInfo>,
    pools: Vec<PoolSummary>,
}

/// Merge the durable poller inventory (`PollerRepo::list`) with the live registry view
/// (`Coordinator::poller_views`) and the per-pool node counts into the Pollers response. Pure (no
/// I/O, no clock) so the merge precedence and the pool-summary arithmetic are unit-testable.
///
/// Merge rules: a poller may appear in the live view (online or recently-offline), in the durable
/// inventory, or both. The live view is authoritative for status and telemetry; the durable row
/// supplies first/last-seen timestamps (and version/pool when the poller is gone from the live
/// registry entirely). A live poller not yet in the inventory (first 60s) still lists. A pool is
/// reported when it has nodes or a live poller; `mode`/`warning` follow the scheduler's per-pool
/// working-set-vs-legacy decision.
fn build_pollers_response(
    inventory: Vec<PollerRow>,
    live: Vec<PollerView>,
    node_pools: std::collections::HashMap<String, usize>,
) -> PollersResponse {
    use std::collections::HashMap;

    let live_by_id: HashMap<&str, &PollerView> = live.iter().map(|v| (v.id.as_str(), v)).collect();
    let inv_by_id: HashMap<&str, &PollerRow> =
        inventory.iter().map(|r| (r.id.as_str(), r)).collect();

    // Union of every id we know about (live registry ∪ durable inventory), sorted + deduped so the
    // list is stable regardless of hash-map iteration order.
    let mut ids: Vec<String> = live
        .iter()
        .map(|v| v.id.clone())
        .chain(inventory.iter().map(|r| r.id.clone()))
        .collect();
    ids.sort_unstable();
    ids.dedup();

    let pollers = ids
        .iter()
        .map(|id| {
            let lv = live_by_id.get(id.as_str()).copied();
            let inv = inv_by_id.get(id.as_str()).copied();
            let online = lv.is_some_and(|v| v.online);
            PollerInfo {
                id: id.clone(),
                pool: lv
                    .map(|v| v.pool.clone())
                    .or_else(|| inv.map(|r| r.pool.clone()))
                    .unwrap_or_default(),
                status: if online { "online" } else { "offline" },
                last_seen: inv.map(|r| r.last_seen.clone()),
                first_seen: inv.map(|r| r.first_seen.clone()),
                // Prefer a non-empty live version (a sync-request-only entry reports none); else the
                // durable row's last version.
                version: lv
                    .map(|v| v.version.clone())
                    .filter(|s| !s.is_empty())
                    .or_else(|| inv.and_then(|r| r.last_version.clone())),
                working_set_nodes: lv.map_or(0, |v| v.working_set_nodes),
                working_set_specs: lv.map_or(0, |v| v.working_set_specs),
                results_total: lv.map_or(0, |v| v.results_total),
                // Host telemetry from the live registry only (the durable row carries none).
                cpu_pct: lv.and_then(|v| v.host.as_ref()).map(|h| h.cpu_pct),
                mem_used_pct: lv
                    .and_then(|v| v.host.as_ref())
                    .and_then(HostSample::mem_used_pct),
                disk_used_pct: lv
                    .and_then(|v| v.host.as_ref())
                    .and_then(HostSample::primary_disk_used_pct),
            }
        })
        .collect();

    // Online pollers per pool (an offline poller can't serve work, so it doesn't count).
    let mut live_per_pool: HashMap<String, usize> = HashMap::new();
    for v in live.iter().filter(|v| v.online) {
        *live_per_pool.entry(v.pool.clone()).or_insert(0) += 1;
    }

    // Report pools that have nodes ∪ pools that have a live poller (so a poller registered but not
    // yet assigned any work still shows up — with no warning).
    let mut pool_names: Vec<String> = node_pools
        .keys()
        .cloned()
        .chain(live_per_pool.keys().cloned())
        .collect();
    pool_names.sort_unstable();
    pool_names.dedup();

    let pools = pool_names
        .iter()
        .map(|name| {
            let nodes = node_pools.get(name).copied().unwrap_or(0);
            let live_pollers = live_per_pool.get(name).copied().unwrap_or(0);
            PoolSummary {
                pool: name.clone(),
                nodes,
                live_pollers,
                mode: if live_pollers > 0 {
                    "working_set"
                } else {
                    "legacy"
                },
                warning: if nodes > 0 && live_pollers == 0 {
                    Some("nodes_without_live_poller")
                } else {
                    None
                },
            }
        })
        .collect();

    PollersResponse { pollers, pools }
}

/// `GET /api/v1/pollers` — the registered poller fleet + per-pool summary (ADR-009/020). Read-only
/// and secret-free (working-set *counts* only, never spec contents), so it is `View`-gated like the
/// other fleet views; skeleton mode (no coordinator/DB) returns the standard 503.
async fn list_pollers(State(st): State<ApiState>, headers: HeaderMap) -> Response {
    if let Some(resp) = require_view(&st, &headers) {
        return resp;
    }
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    let now = Instant::now();
    // In-memory registry is the source of truth for liveness (ADR-009).
    let live = admin.coordinator.poller_views(now);
    // Durable inventory is best-effort context (offline pollers + timestamps); degrade to just the
    // live view on a read error rather than failing the page (ADR-017).
    let inventory = match admin.pollers.list().await {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!(error = %e, "poller inventory list failed; showing live view only");
            Vec::new()
        }
    };
    // Node counts per pool. Meraki-managed nodes are excluded — the org collector owns them, not a
    // pool poller (mirrors the scheduler). Degrade to an empty summary on a read error.
    let meraki_ids = admin.meraki_devices.node_ids().await.unwrap_or_default();
    // Effective pool, so a node inheriting from its folder is counted under the pool that actually
    // polls it. A folder-read error degrades to "no inheritance" for the *summary only* — it never
    // reaches the scheduler, so a wrong count here can't misroute polling.
    let resolver = pool_resolver(admin).await;
    let mut node_pools: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    match admin.repo.list_nodes().await {
        Ok(nodes) => {
            for n in nodes {
                if meraki_ids.contains(&n.id.as_uuid()) {
                    continue;
                }
                *node_pools
                    .entry(resolver.resolve_pool(&n).to_owned())
                    .or_insert(0) += 1;
            }
        }
        Err(e) => tracing::error!(error = %e, "list nodes for pool summary failed"),
    }
    Json(build_pollers_response(inventory, live, node_pools)).into_response()
}

/// Build a [`PoolResolver`] from the folder tree. A read error degrades to "no folder inheritance"
/// with a warning: every caller is a read-only view, where missing inheritance is a display
/// inaccuracy and never a polling decision (the scheduler builds its own resolver and holds the
/// last-known one instead of degrading).
pub(crate) async fn pool_resolver(admin: &AdminState) -> PoolResolver {
    match admin.groups.pool_rows().await {
        Ok(rows) => PoolResolver::build(rows),
        Err(e) => {
            tracing::warn!(error = %e, "loading folder pools failed; resolving without inheritance");
            PoolResolver::empty()
        }
    }
}

/// Which poller currently polls a node — the node detail's "Polled by" fact.
#[derive(Debug, Serialize, PartialEq)]
struct PolledBy {
    /// One of `assigned`, `legacy_fanout`, `pending`, `meraki`, `unknown`.
    state: &'static str,
    /// The owning poller; set only in the `assigned` state.
    poller_id: Option<String>,
}

/// `GET /api/v1/nodes/:id/assignment` — the node's effective pool, where that pool came from, and
/// which poller currently holds it.
#[derive(Debug, Serialize, PartialEq)]
struct NodeAssignment {
    /// Effective pool: the node's own, else the nearest ancestor folder's, else the default.
    pool: String,
    pool_source: PoolSource,
    /// The folder that supplied the pool, when `pool_source` is `group`.
    pool_source_group_id: Option<Uuid>,
    polled_by: PolledBy,
}

/// The five distinct answers to "who polls this node". Pure, so every branch is testable without a
/// coordinator or a database.
///
/// The order matters. Leadership is checked first because an HA standby runs no coordinator
/// (`run_heartbeat_consumer` is leader-only), so its empty registry would otherwise report the
/// plausible-looking but wrong `legacy_fanout` for the entire fleet. Meraki devices come next:
/// core's own org collector polls them, so no pool poller ever will. Only then does the published
/// owner — and failing that, the pool's dispatch mode — decide.
fn resolve_polled_by(
    is_leader: bool,
    is_meraki: bool,
    owner: Option<String>,
    pool_has_live_poller: bool,
) -> PolledBy {
    if !is_leader {
        return PolledBy {
            state: "unknown",
            poller_id: None,
        };
    }
    if is_meraki {
        return PolledBy {
            state: "meraki",
            poller_id: None,
        };
    }
    if let Some(id) = owner {
        return PolledBy {
            state: "assigned",
            poller_id: Some(id),
        };
    }
    if pool_has_live_poller {
        // Working-set mode, but this node is in nobody's set: added since the last sweep, or it
        // resolves to no specs at all (so it is never published to anyone).
        PolledBy {
            state: "pending",
            poller_id: None,
        }
    } else {
        // No live poller in the pool ⇒ the scheduler falls back to legacy per-job publish on
        // `yagra.jobs.{pool}`, which has no single owner. With nothing subscribed those jobs are
        // silently discarded, so this means "possibly unmonitored" — not a healthy alternative mode.
        PolledBy {
            state: "legacy_fanout",
            poller_id: None,
        }
    }
}

/// `GET /api/v1/nodes/:node_id/assignment` — which pool a node effectively belongs to and which
/// poller is currently polling it. `View`-gated like the rest of the node detail; carries no
/// credentials.
async fn node_assignment(
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
    let node = match admin.repo.get_node(node_id).await {
        Ok(Some(n)) => n,
        Ok(None) => return not_found("node_not_found", format!("no node {node_id}")),
        Err(e) => {
            tracing::error!(error = %e, "node assignment: load node failed");
            return internal("failed to load node");
        }
    };
    let resolved = pool_resolver(admin).await.resolve(&node);
    let now = Instant::now();
    // Best-effort, like the node detail's own Meraki lookup: a read failure just means we don't
    // special-case it below.
    let is_meraki = admin
        .meraki_devices
        .get(node_id)
        .await
        .unwrap_or(None)
        .is_some();
    let polled_by = resolve_polled_by(
        st.is_leader.load(std::sync::atomic::Ordering::Acquire),
        is_meraki,
        admin.coordinator.owner_of(node.id, now),
        admin.coordinator.live_pools(now).contains(&resolved.pool),
    );
    Json(NodeAssignment {
        pool: resolved.pool,
        pool_source: resolved.source,
        pool_source_group_id: resolved.group,
        polled_by,
    })
    .into_response()
}

/// Largest node page the poller drill-down returns. A poller in a big pool can hold tens of
/// thousands of nodes, so the page is capped — and the response says it was, rather than looking
/// like a complete list.
const POLLER_NODES_MAX: usize = 500;

#[derive(Deserialize)]
struct PollerNodesQuery {
    limit: Option<usize>,
}

/// One node in the poller drill-down.
#[derive(Debug, Serialize, PartialEq)]
struct PollerNodeRef {
    id: Uuid,
    name: String,
}

/// `GET /api/v1/pollers/:id/nodes` body.
#[derive(Debug, Serialize, PartialEq)]
struct PollerNodesResponse {
    poller_id: String,
    /// The pool it serves; `null` unless it is live.
    pool: Option<String>,
    /// `assigned` (live, working set known), `offline` (unknown or not beating), or `unknown`
    /// (this core is an HA standby and runs no coordinator).
    state: &'static str,
    /// Nodes in its working set, before the page cap.
    total: usize,
    /// Whether `nodes` is a capped page of `total`.
    truncated: bool,
    nodes: Vec<PollerNodeRef>,
}

/// `GET /api/v1/pollers/:id/nodes` — the nodes a poller currently holds, for the Pollers-page
/// drill-down ("if this poller dies, what stops being monitored?").
///
/// Served from the coordinator's published working set rather than from a database query, so it is
/// the same data the node detail's "Polled by" reads and the two can never disagree.
async fn poller_nodes(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<PollerNodesQuery>,
) -> Response {
    if let Some(resp) = require_view(&st, &headers) {
        return resp;
    }
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    let empty = |poller_id: String, state: &'static str| {
        Json(PollerNodesResponse {
            poller_id,
            pool: None,
            state,
            total: 0,
            truncated: false,
            nodes: Vec::new(),
        })
        .into_response()
    };
    if !st.is_leader.load(std::sync::atomic::Ordering::Acquire) {
        return empty(id, "unknown");
    }
    let now = Instant::now();
    let Some(owned) = admin.coordinator.published_nodes(&id, now) else {
        return empty(id, "offline");
    };
    let limit = q
        .limit
        .unwrap_or(POLLER_NODES_MAX)
        .clamp(1, POLLER_NODES_MAX);
    let total = owned.len();
    let truncated = total > limit;
    if truncated {
        tracing::info!(
            poller = %id,
            total,
            limit,
            "poller node drill-down capped to the page limit"
        );
    }
    let page: Vec<Uuid> = owned.iter().take(limit).map(NodeId::as_uuid).collect();
    // Names are context, not the answer — an id with no name still tells the operator which node
    // moved, so a lookup failure degrades to bare ids rather than failing the drill-down.
    let names = admin.repo.node_names(&page).await.unwrap_or_else(|e| {
        tracing::warn!(error = %e, "poller node drill-down: name lookup failed");
        std::collections::HashMap::new()
    });
    let mut nodes: Vec<PollerNodeRef> = page
        .into_iter()
        .map(|id| PollerNodeRef {
            name: names.get(&id).cloned().unwrap_or_else(|| id.to_string()),
            id,
        })
        .collect();
    // Paged by uuid (stable), presented by name (useful).
    nodes.sort_by(|a, b| a.name.cmp(&b.name).then(a.id.cmp(&b.id)));
    let pool = admin.coordinator.pool_of(&id, now);
    Json(PollerNodesResponse {
        poller_id: id,
        pool,
        state: "assigned",
        total,
        truncated,
        nodes,
    })
    .into_response()
}

/// `GET /api/v1/monitoring-gaps` — recent core↔poller visibility outages (Phase 3, store-and-forward).
/// Read-only self-monitoring data, secret-free, so it is `View`-gated like the other fleet views;
/// skeleton mode (no coordinator/DB) returns the standard 503. Newest first, capped. A DB read error
/// degrades to an empty list rather than failing the Pollers page.
async fn list_monitoring_gaps(State(st): State<ApiState>, headers: HeaderMap) -> Response {
    if let Some(resp) = require_view(&st, &headers) {
        return resp;
    }
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    let gaps = admin
        .pollers
        .list_monitoring_gaps(200)
        .await
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "list monitoring gaps failed; returning empty");
            Vec::new()
        });
    Json(gaps).into_response()
}

/// `DELETE /api/v1/pollers/:id` — remove a decommissioned poller from the durable inventory. A
/// currently-online poller is refused (409 `poller_online`): it would just re-register on its next
/// heartbeat. `ManageConfig`-gated (audited by the mutation middleware). Authorize-first ordering
/// (like the shared-dashboard write) so a forbidden caller can't probe whether the DB is wired and
/// the RBAC gate is testable without a database.
async fn delete_poller(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if let Some(resp) = authorize(&st, &headers, Permission::ManageConfig) {
        return resp;
    }
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    if admin
        .coordinator
        .poller_views(Instant::now())
        .iter()
        .any(|v| v.id == id && v.online)
    {
        return error_response(
            StatusCode::CONFLICT,
            "poller_online",
            "poller is currently online; stop it before removing it".to_owned(),
        );
    }
    match admin.pollers.delete(&id).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => not_found("poller_not_found", format!("no poller {id}")),
        Err(e) => {
            tracing::error!(error = %e, "delete poller failed");
            internal("failed to delete poller")
        }
    }
}

/// Reachability of one backing dependency, for the system-health endpoint. Carries only a
/// boolean and a human label — no connection strings or secrets.
#[derive(Serialize)]
struct DependencyHealth {
    /// Whether the dependency answered a cheap liveness probe.
    reachable: bool,
    /// Short human description of what was checked (no secrets).
    detail: String,
}

/// Yagra's own health: the reachability of its backing services. `bus` is an *indirect* signal
/// (NATS isn't held in `ApiState`, so it's inferred from poll-loop liveness — see `system_health`).
#[derive(Serialize)]
struct SystemHealth {
    /// `"ok"` when every dependency is reachable, else `"degraded"`.
    overall: String,
    /// PostgreSQL (metadata store) — `SELECT 1`.
    postgres: DependencyHealth,
    /// VictoriaMetrics (TSDB) — `/-/healthy`.
    tsdb: DependencyHealth,
    /// VictoriaLogs (event log, ADR-024) — `/health`. Reported reachable when not configured
    /// (events then live in PostgreSQL), so an unconfigured log store never degrades `overall`.
    logs: DependencyHealth,
    /// ClickHouse (flow store, ADR-031) — `/ping`. Reported reachable when not configured
    /// (flow monitoring off), so an unconfigured flow store never degrades `overall`.
    flow: DependencyHealth,
    /// NATS bus — inferred from a recent scheduler sweep (publish+consume working), not a direct ping.
    bus: DependencyHealth,
}

/// Whether the last scheduler sweep is recent enough to treat the bus as healthy. The poll loop
/// records a sweep every round (cadence = the smallest poll interval in play, ≤ the default), so a
/// sweep within `2 × default_interval + 60s` means jobs are publishing and results consuming over
/// NATS. A stale (or never-recorded) sweep means the poll loop — and thus the bus path — is stalled.
fn bus_sweep_is_fresh(
    last_sweep_unix_ms: Option<i64>,
    default_interval_secs: u32,
    now_ms: i64,
) -> bool {
    match last_sweep_unix_ms {
        Some(ms) => {
            let window_ms = (i64::from(default_interval_secs) * 2 + 60) * 1000;
            now_ms.saturating_sub(ms) <= window_ms
        }
        None => false,
    }
}

/// Yagra self-health: reachability of the backing services (PostgreSQL, TSDB) plus an indirect bus
/// signal derived from poll-loop liveness. Read-only and secret-free, so it is `View`-gated like
/// poller-health. Degrades to a `"degraded"` JSON body in skeleton mode (no admin/DB) rather than
/// 503, so the UI can always render the page.
async fn system_health(State(st): State<ApiState>, headers: HeaderMap) -> Response {
    if let Some(resp) = require_view(&st, &headers) {
        return resp;
    }

    // TSDB: ping the metric store (VictoriaMetrics `/-/healthy`; the in-memory sink is always up).
    let tsdb = DependencyHealth {
        reachable: st.store.healthy().await,
        detail: "VictoriaMetrics (TSDB)".to_owned(),
    };

    // Event log store (ADR-024): ping VictoriaLogs when configured. When unset, events live in
    // PostgreSQL, so report reachable with a clarifying detail (never degrades `overall`).
    let logs = match st.logs.as_ref() {
        Some(log_store) => DependencyHealth {
            reachable: log_store.healthy().await,
            detail: "VictoriaLogs (event log)".to_owned(),
        },
        None => DependencyHealth {
            reachable: true,
            detail: "VictoriaLogs not configured (events in PostgreSQL)".to_owned(),
        },
    };

    // Flow store (ADR-031): ping ClickHouse when configured. When unset, flow monitoring is off, so
    // report reachable with a clarifying detail (never degrades `overall`).
    let flow = match st.flows.as_ref() {
        Some(flow_store) => DependencyHealth {
            reachable: flow_store.healthy().await,
            detail: "ClickHouse (flow store)".to_owned(),
        },
        None => DependencyHealth {
            reachable: true,
            detail: "ClickHouse not configured (flow monitoring off)".to_owned(),
        },
    };

    // PostgreSQL and the bus signal both depend on the live write side. In skeleton mode there is
    // no DB and no poll loop, so both are reported unreachable (not an error).
    let (postgres, bus) = match st.admin.as_ref() {
        Some(admin) => {
            let postgres = DependencyHealth {
                reachable: admin.repo.healthy().await,
                detail: "PostgreSQL (metadata)".to_owned(),
            };
            let default_secs = admin
                .repo
                .get_default_poll_interval()
                .await
                .unwrap_or(crate::config::DEFAULT_POLL_INTERVAL_SECS);
            let now_ms = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
                .unwrap_or(0);
            let fresh = bus_sweep_is_fresh(
                admin.scheduler_stats.snapshot().last_sweep_unix_ms,
                default_secs,
                now_ms,
            );
            let bus = DependencyHealth {
                reachable: fresh,
                detail: "NATS bus (inferred from poll-loop activity)".to_owned(),
            };
            (postgres, bus)
        }
        None => (
            DependencyHealth {
                reachable: false,
                detail: "PostgreSQL not configured (skeleton mode)".to_owned(),
            },
            DependencyHealth {
                reachable: false,
                detail: "poll loop not running (skeleton mode)".to_owned(),
            },
        ),
    };

    let overall = if postgres.reachable
        && tsdb.reachable
        && logs.reachable
        && flow.reachable
        && bus.reachable
    {
        "ok"
    } else {
        "degraded"
    };
    Json(SystemHealth {
        overall: overall.to_owned(),
        postgres,
        tsdb,
        logs,
        flow,
        bus,
    })
    .into_response()
}

// ── Host self-observability: CPU/load/mem/disk of core + each poller ──────────

/// One self-monitored host in `GET /api/v1/system/hosts`: current resource values for the core
/// process's host (`role="core"`) or a poller's host (`role="poller"`). Secret-free — an
/// `instance` is `core` or an already-sanitized poller id, and `mount` labels are configured
/// aliases, never raw device paths.
#[derive(Serialize)]
struct HostInfo {
    /// `core` or the poller id.
    instance: String,
    /// `"core"` or `"poller"`.
    role: &'static str,
    /// Pool the poller serves; `null` for core.
    pool: Option<String>,
    /// Whether the source is currently reporting (core is always online; a poller within its
    /// heartbeat window).
    online: bool,
    /// Current CPU utilization % (0–100); `null` when no sample yet.
    cpu_pct: Option<f64>,
    /// Current 1-minute load average; `null` when no sample / unavailable on the platform.
    load1: Option<f64>,
    /// Memory in use, bytes; `null` when no sample.
    mem_used_bytes: Option<u64>,
    /// Total physical memory, bytes; `null` when no sample.
    mem_total_bytes: Option<u64>,
    /// Memory-used %, derived; `null` when total is unknown.
    mem_used_pct: Option<f64>,
    /// Highest watched-filesystem used %, for at-a-glance columns; `null` when none has a capacity.
    disk_used_pct: Option<f64>,
    /// Current per-watched-filesystem usage.
    disks: Vec<DiskUsage>,
}

/// The `GET /api/v1/system/hosts` body: core plus every poller that reports host telemetry.
#[derive(Serialize)]
struct SystemHostsResponse {
    hosts: Vec<HostInfo>,
}

/// Build a [`HostInfo`] from an optional sample (absent ⇒ all-`null` metrics but the row still
/// lists, so the instance selector shows the host even before its first sample).
fn host_info(
    instance: &str,
    role: &'static str,
    pool: Option<String>,
    online: bool,
    s: Option<&HostSample>,
) -> HostInfo {
    HostInfo {
        instance: instance.to_owned(),
        role,
        pool,
        online,
        cpu_pct: s.map(|s| s.cpu_pct),
        load1: s.map(|s| s.load1),
        mem_used_bytes: s.map(|s| s.mem_used_bytes),
        mem_total_bytes: s.map(|s| s.mem_total_bytes),
        mem_used_pct: s.and_then(HostSample::mem_used_pct),
        disk_used_pct: s.and_then(HostSample::primary_disk_used_pct),
        disks: s.map(|s| s.disks.clone()).unwrap_or_default(),
    }
}

/// `GET /api/v1/system/hosts` — current host resources for core and every poller reporting
/// telemetry. `View`-gated and secret-free (like the other fleet views). Always returns core (it is
/// answering the request); pollers come from the live registry, so skeleton mode returns just core.
async fn system_hosts(State(st): State<ApiState>, headers: HeaderMap) -> Response {
    if let Some(resp) = require_view(&st, &headers) {
        return resp;
    }
    let mut hosts = Vec::new();
    let core = st.host_sample.lock().ok().and_then(|g| g.clone());
    hosts.push(host_info("core", "core", None, true, core.as_ref()));
    if let Some(admin) = st.admin.as_ref() {
        for v in admin.coordinator.poller_views(Instant::now()) {
            if let Some(h) = v.host.as_ref() {
                hosts.push(host_info(
                    &v.id,
                    "poller",
                    Some(v.pool.clone()),
                    v.online,
                    Some(h),
                ));
            }
        }
    }
    Json(SystemHostsResponse { hosts }).into_response()
}

/// The watched-filesystem mounts for one instance, or `None` if the instance is unknown (⇒ 404).
/// `core` is always valid (empty mounts until its first sample); a poller is valid iff it is in the
/// live registry. Bounds the `instance`/`mount` values that reach a PromQL selector to known ones.
fn host_disk_mounts(st: &ApiState, instance: &str) -> Option<Vec<String>> {
    if instance == "core" {
        let mounts = st
            .host_sample
            .lock()
            .ok()
            .and_then(|g| g.clone())
            .map(|s| s.disks.iter().map(|d| d.mount.clone()).collect())
            .unwrap_or_default();
        return Some(mounts);
    }
    let admin = st.admin.as_ref()?;
    let view = admin
        .coordinator
        .poller_views(Instant::now())
        .into_iter()
        .find(|v| v.id == instance)?;
    Some(
        view.host
            .map(|h| h.disks.into_iter().map(|d| d.mount).collect())
            .unwrap_or_default(),
    )
}

/// Query params for the host-metrics range endpoint (all optional; same defaults as node ranges).
#[derive(Deserialize)]
struct HostRangeQuery {
    from: Option<i64>,
    to: Option<i64>,
    step: Option<u64>,
}

/// A per-mount filesystem trend: used vs. size over the window (the frontend derives % or shows a
/// bare-bytes trend when `size` is all zero, e.g. the `database` size proxy).
#[derive(Serialize)]
struct HostDiskRange {
    mount: String,
    used_bytes: Vec<MetricPoint>,
    size_bytes: Vec<MetricPoint>,
}

/// The `GET /api/v1/system/hosts/:instance/metrics/range` body — the scalar host trends plus a
/// per-mount filesystem trend, all over one window. One round trip per instance/range change.
#[derive(Serialize)]
struct HostMetricRange {
    instance: String,
    cpu_pct: Vec<MetricPoint>,
    load1: Vec<MetricPoint>,
    load5: Vec<MetricPoint>,
    load15: Vec<MetricPoint>,
    mem_used_bytes: Vec<MetricPoint>,
    mem_total_bytes: Vec<MetricPoint>,
    disks: Vec<HostDiskRange>,
}

/// `GET /api/v1/system/hosts/:instance/metrics/range` — host CPU/load/mem/disk trends for one
/// instance (`core` or a poller id) over `[from,to]` at `step`. `View`-gated; the `instance` is
/// validated against the known set before any selector is built.
async fn host_metric_range(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Path(instance): Path<String>,
    Query(q): Query<HostRangeQuery>,
) -> Response {
    if let Some(resp) = require_view(&st, &headers) {
        return resp;
    }
    let Some(mounts) = host_disk_mounts(&st, &instance) else {
        return not_found("host_not_found", format!("unknown instance {instance:?}"));
    };
    let to = q.to.unwrap_or_else(now_unix_s);
    let from = q.from.unwrap_or(to - DEFAULT_RANGE_SECS);
    let step = clamp_range_step(from, to, q.step.unwrap_or(DEFAULT_STEP_SECS), 1);
    let store = &st.store;
    // The six scalar host series and each mount's two disk series are all independent range
    // queries — fan them all out concurrently (this backs the 15s System Health refresh).
    let (cpu_pct, load1, load5, load15, mem_used_bytes, mem_total_bytes) = tokio::join!(
        store.host_metric_range(&instance, "cpu_pct", from, to, step),
        store.host_metric_range(&instance, "load1", from, to, step),
        store.host_metric_range(&instance, "load5", from, to, step),
        store.host_metric_range(&instance, "load15", from, to, step),
        store.host_metric_range(&instance, "mem_used_bytes", from, to, step),
        store.host_metric_range(&instance, "mem_total_bytes", from, to, step),
    );
    let disks = futures::future::join_all(mounts.into_iter().map(|mount| {
        let store = &st.store;
        let instance = &instance;
        async move {
            let (used_bytes, size_bytes) = tokio::join!(
                store.host_disk_range(instance, "fs_used_bytes", &mount, from, to, step),
                store.host_disk_range(instance, "fs_size_bytes", &mount, from, to, step),
            );
            HostDiskRange {
                mount,
                used_bytes,
                size_bytes,
            }
        }
    }))
    .await;
    Json(HostMetricRange {
        instance,
        cpu_pct,
        load1,
        load5,
        load15,
        mem_used_bytes,
        mem_total_bytes,
        disks,
    })
    .into_response()
}

// ── Reports (Dashboard → Reports) ────────────────────────────────────────────
//
// Shared resource: everyone reads, only admins (ManageConfig) write — same model as the Shared
// Dashboard. Definitions are reusable templates (opaque `spec`); schedules fire them on a preset
// cadence; runs are saved generated reports. Generation runs in core as a background task.

/// Max bytes for a report definition `spec` (shares the dashboard cap).
const MAX_REPORT_SPEC_BYTES: usize = MAX_DASHBOARD_BYTES;

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

// ── Admin (write) handlers — live mode only ──────────────────────────────────

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
    // Brute-force guard: refuse (without touching Argon2) if this account is locked out or the
    // global attempt-rate cap is spent. Audited so a guessing run is visible.
    if let Err(reject) = st.login_throttle.check(&body.username) {
        audit_record(&admin.audit, &body.username, "auth.login", 429).await;
        let mut resp = error_response(
            StatusCode::TOO_MANY_REQUESTS,
            "too_many_attempts",
            format!(
                "too many login attempts; retry in {} seconds",
                reject.retry_after_secs
            ),
        );
        if let Ok(v) = HeaderValue::from_str(&reject.retry_after_secs.to_string()) {
            resp.headers_mut().insert(header::RETRY_AFTER, v);
        }
        return resp;
    }
    match admin.users.verify(&body.username, &body.password).await {
        Ok(Some((user_id, principal))) => {
            st.login_throttle.record_success(&body.username);
            let role = principal.role;
            let token = st.sessions.issue(user_id, principal, &body.username);
            // Auth events are audited with the username only — never the credential.
            audit_record(&admin.audit, &body.username, "auth.login", 200).await;
            Json(serde_json::json!({ "token": token, "role": role })).into_response()
        }
        Ok(None) => {
            st.login_throttle.record_failure(&body.username);
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

/// Server-side logout: revoke the caller's bearer token so it can't be reused. Idempotent — an
/// absent or already-revoked token still returns 204.
async fn logout(State(st): State<ApiState>, headers: HeaderMap) -> Response {
    if let Some(token) = bearer(&headers) {
        st.sessions.revoke_token(token);
    }
    StatusCode::NO_CONTENT.into_response()
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

// ── API tokens (PATs) — the durable credential for the MCP tool surface (ADR-028) ─────────────────
// Admin-only (issuing a role-bearing credential is a user-management action). Mutations are audited
// generically by `audit_mw`; the raw token is returned once on create and never stored.

/// Request body for `POST /api/v1/api-tokens`.
#[derive(Deserialize)]
struct CreateApiTokenBody {
    /// Human label (unique, ≤128 chars).
    name: String,
    /// The role the token grants (`viewer` recommended for a read-only MCP client).
    role: Role,
    /// Visibility scope (`"All"` or `{"Groups":[…]}`); defaults to `All` when omitted.
    scope: Option<yagra_common::Scope>,
}

/// Resolve the caller's session, requiring `ManageUsers` (admin-only). On failure returns the
/// short-circuit `Err(response)`; on success `Ok(session)` (used for the `created_by` actor). The
/// error is boxed so the `Ok`/`Err` size gap stays small (`clippy::result_large_err`).
fn require_admin_session(
    st: &ApiState,
    headers: &HeaderMap,
) -> Result<crate::auth::Session, Box<Response>> {
    match st
        .sessions
        .authorize(bearer(headers), Permission::ManageUsers)
    {
        Ok(session) => Ok(session),
        Err(AuthError::Forbidden) => Err(Box::new(error_response(
            StatusCode::FORBIDDEN,
            "forbidden",
            "managing API tokens requires the admin role".to_owned(),
        ))),
        Err(_) => Err(Box::new(error_response(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "a valid bearer token is required".to_owned(),
        ))),
    }
}

async fn create_api_token(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Json(body): Json<CreateApiTokenBody>,
) -> Response {
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    let session = match require_admin_session(&st, &headers) {
        Ok(s) => s,
        Err(resp) => return *resp,
    };
    let name = body.name.trim();
    if name.is_empty() || name.chars().count() > 128 {
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_name",
            "token name must be 1–128 characters".to_owned(),
        );
    }
    let scope = body.scope.unwrap_or(yagra_common::Scope::All);
    match admin
        .api_tokens
        .create(name, body.role, &scope, &session.username)
        .await
    {
        Ok((id, raw)) => (
            StatusCode::CREATED,
            // The raw token is returned exactly once — the client must store it now (it's unrecoverable).
            Json(serde_json::json!({
                "id": id,
                "name": name,
                "role": body.role,
                "token": raw,
            })),
        )
            .into_response(),
        Err(e) => {
            if e.downcast_ref::<sqlx::Error>()
                .and_then(sqlx::Error::as_database_error)
                .is_some_and(|db| db.is_unique_violation())
            {
                return error_response(
                    StatusCode::CONFLICT,
                    "duplicate_name",
                    "an API token with that name already exists".to_owned(),
                );
            }
            tracing::error!(error = %e, "API token create failed");
            internal("failed to create API token")
        }
    }
}

async fn list_api_tokens(State(st): State<ApiState>, headers: HeaderMap) -> Response {
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    if let Err(resp) = require_admin_session(&st, &headers) {
        return *resp;
    }
    match admin.api_tokens.list().await {
        Ok(tokens) => Json(tokens).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "API token list failed");
            internal("failed to list API tokens")
        }
    }
}

async fn revoke_api_token(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Response {
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    if let Err(resp) = require_admin_session(&st, &headers) {
        return *resp;
    }
    match admin.api_tokens.revoke(id).await {
        // Idempotent: a missing/already-revoked id is a no-op 204 (nothing left to do).
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => {
            tracing::error!(error = %e, "API token revoke failed");
            internal("failed to revoke API token")
        }
    }
}

// ── Forwarding destinations (the passive-data tee, ADR-034) ─────────────────────────────────────

/// Request body for creating/updating a forwarding destination.
#[derive(Deserialize)]
struct ForwardDestinationBody {
    /// Human label (unique, 1–120 chars).
    name: String,
    /// Whether the forwarder should send to it. Defaults to enabled.
    #[serde(default = "default_true")]
    enabled: bool,
    /// Which received stream to tee.
    source_kind: yagra_forward::SourceKind,
    /// How to speak to the collector.
    dest_kind: yagra_forward::DestKind,
    /// `host:port` of the collector.
    target: String,
    /// Restrict to one poller pool; omitted/null = every pool.
    #[serde(default)]
    pool: Option<String>,
    /// Relay the original bytes (default) rather than re-rendering from the parsed fields.
    #[serde(default = "default_true")]
    verbatim: bool,
    /// The filter; omitted = forward the whole stream.
    #[serde(default)]
    filter: yagra_forward::FilterExpr,
    /// Optional messages/second ceiling.
    #[serde(default)]
    rate_limit_per_sec: Option<u32>,
    /// SNMP community for re-encoded traps (`snmp_trap_udp` only). Sealed at rest; on update,
    /// omitting it keeps the stored value.
    #[serde(default)]
    community: Option<String>,
    /// PEM certificate(s) to trust for a TLS destination, in addition to the system roots. Not a
    /// secret, so unlike `community` it round-trips and an empty value clears it.
    #[serde(default)]
    ca_cert: Option<String>,
    /// Google service-account key JSON for a `bigquery` destination. Sealed at rest; on update,
    /// omitting it keeps the stored key. Omitting it on create selects Workload Identity (the
    /// GCE/GKE metadata server), which stores no secret at all.
    #[serde(default)]
    service_account_json: Option<String>,
}

const fn default_true() -> bool {
    true
}

/// Validate a destination body and turn it into store input. Everything an operator can get wrong
/// is rejected here, so the forwarder only ever sees configurations it can honour. The error is
/// boxed so the `Ok`/`Err` size gap stays small (`clippy::result_large_err`).
fn validate_forward_body(
    st: &ApiState,
    body: ForwardDestinationBody,
) -> Result<crate::forward_store::ForwardDestinationInput, Box<Response>> {
    let bad = |code: &str, msg: &str| {
        Box::new(error_response(
            StatusCode::BAD_REQUEST,
            code,
            msg.to_owned(),
        ))
    };

    let name = body.name.trim().to_owned();
    if name.is_empty() || name.chars().count() > 120 {
        return Err(bad("invalid_name", "name must be 1–120 characters"));
    }
    // Two target shapes: `host:port` for the socket kinds, `project.dataset.table` for BigQuery.
    // Validated per kind here so a typo is a 400 with a useful message, rather than a destination
    // that only reports "404" from Google after an admin enables it.
    let target = body.target.trim().to_owned();
    let mut socket_port = None;
    if body.dest_kind.is_host_port() {
        let Some((_, port)) = split_host_port(&target) else {
            return Err(bad(
                "invalid_target",
                "target must be host:port (use [addr]:port for a literal IPv6 address)",
            ));
        };
        socket_port = Some(port);
    } else if let Err(e) = crate::bigquery::BigQueryTarget::parse(&target) {
        return Err(Box::new(error_response(
            StatusCode::BAD_REQUEST,
            "invalid_target",
            format!("target must be project.dataset.table — {e}"),
        )));
    }
    if !body.dest_kind.accepts(body.source_kind) {
        return Err(bad(
            "incompatible_kinds",
            "that destination kind cannot carry that source stream",
        ));
    }
    // Refusing verbatim here (rather than quietly ignoring it) keeps the UI honest: a trap PDU sent
    // to a syslog collector would be undecodable, so that pairing is always re-rendered.
    if body.verbatim && !body.dest_kind.supports_verbatim(body.source_kind) {
        return Err(bad(
            "verbatim_unsupported",
            "byte-exact relay is not possible for this source/destination pairing",
        ));
    }
    // ...and the converse for flow: a template-bound binary export has no rendered form, so
    // "rebuild from the parsed fields" is not a choice that exists.
    if !body.verbatim && !body.dest_kind.supports_rendered(body.source_kind) {
        return Err(bad(
            "rendered_unsupported",
            "a flow export can only be relayed byte-for-byte — there is no rendered form",
        ));
    }
    if body.rate_limit_per_sec == Some(0) {
        return Err(bad(
            "invalid_rate_limit",
            "rate limit must be greater than zero, or omitted for no limit",
        ));
    }
    let pool = body
        .pool
        .map(|p| p.trim().to_owned())
        .filter(|p| !p.is_empty());
    if pool.as_ref().is_some_and(|p| p.chars().count() > 64) {
        return Err(bad("invalid_pool", "pool must be at most 64 characters"));
    }

    // Compile the filter now so a broken expression is a 400, never a destination that silently
    // drops everything (or, worse, forwards everything) once the dispatcher picks it up.
    yagra_forward::compile(&body.filter).map_err(|e| {
        Box::new(error_response(
            StatusCode::BAD_REQUEST,
            "invalid_filter",
            e.to_string(),
        ))
    })?;
    // A condition on a field the stream never carries can only ever be a mistake.
    if let Some(c) = body
        .filter
        .conditions
        .iter()
        .find(|c| !c.field.applies_to(body.source_kind))
    {
        return Err(Box::new(error_response(
            StatusCode::BAD_REQUEST,
            "invalid_filter",
            format!(
                "field `{}` never appears on {} events",
                c.field.as_str(),
                body.source_kind.as_str()
            ),
        )));
    }

    // Loop guard: forwarding Yagra's own output back into one of its own listeners amplifies
    // without bound. Core can only see the *ports* pollers bind (a heartbeat advertises the bind
    // address, not the poller's routable address), so this catches the realistic mistake — aiming a
    // destination at localhost on a listener port — and cannot catch every possible cycle.
    // (BigQuery has no port and cannot address a Yagra listener at all, so it skips this.)
    if let (Some(admin), Some(port)) = (st.admin.as_ref(), socket_port) {
        let loopback_target = target_is_local(&target);
        if loopback_target
            && admin
                .coordinator
                .listener_ports(std::time::Instant::now())
                .contains(&port)
        {
            return Err(bad(
                "self_loop",
                "that target is one of Yagra's own listeners — forwarding to it would loop",
            ));
        }
    }

    let community = body
        .community
        .map(|c| c.trim().to_owned())
        .filter(|c| !c.is_empty());
    if community.is_some() && body.dest_kind != yagra_forward::DestKind::SnmpTrapUdp {
        return Err(bad(
            "invalid_community",
            "a community only applies to an SNMP trap destination",
        ));
    }

    let ca_cert = body
        .ca_cert
        .map(|c| c.trim().to_owned())
        .filter(|c| !c.is_empty());
    if ca_cert.is_some() && !body.dest_kind.is_tls() {
        return Err(bad(
            "invalid_ca_cert",
            "a CA certificate only applies to a TLS destination",
        ));
    }
    // Parse now so a mistyped certificate is a 400 rather than a destination that fails every
    // handshake with an error only visible on the status page.
    if let Some(pem) = ca_cert.as_deref() {
        if pem.len() > MAX_CA_CERT_BYTES {
            return Err(bad(
                "invalid_ca_cert",
                "the CA certificate is too large (max 64 KiB)",
            ));
        }
        use rustls::pki_types::{pem::PemObject, CertificateDer};
        let parsed = CertificateDer::pem_slice_iter(pem.as_bytes()).collect::<Result<Vec<_>, _>>();
        if parsed.map(|c| c.is_empty()).unwrap_or(true) {
            return Err(bad(
                "invalid_ca_cert",
                "that is not a PEM certificate (expected a -----BEGIN CERTIFICATE----- block)",
            ));
        }
    }

    let service_account_json = body
        .service_account_json
        .map(|j| j.trim().to_owned())
        .filter(|j| !j.is_empty());
    if service_account_json.is_some() && body.dest_kind != yagra_forward::DestKind::BigQuery {
        return Err(bad(
            "invalid_service_account",
            "a service-account key only applies to a BigQuery destination",
        ));
    }
    if let Some(json) = service_account_json.as_deref() {
        if json.len() > MAX_SERVICE_ACCOUNT_BYTES {
            return Err(bad(
                "invalid_service_account",
                "the service-account key is too large (max 16 KiB)",
            ));
        }
        // Parse and reject here so a mistyped key is a 400 rather than a destination whose every
        // batch fails with an error only visible on the status page. The error text is Google-shaped
        // and never quotes key material.
        crate::gcp::validate_service_account(json).map_err(|e| {
            Box::new(error_response(
                StatusCode::BAD_REQUEST,
                "invalid_service_account",
                e,
            ))
        })?;
    }

    // Exactly one secret per destination, and which one is decided by the destination kind — the
    // two are mutually exclusive above, so this cannot silently discard the other.
    let secret = match (community, service_account_json) {
        (Some(community), _) => Some(crate::forward_store::DestSecret::SnmpCommunity { community }),
        (None, Some(json)) => Some(crate::forward_store::DestSecret::GcpServiceAccount { json }),
        (None, None) => None,
    };

    Ok(crate::forward_store::ForwardDestinationInput {
        name,
        enabled: body.enabled,
        source_kind: body.source_kind,
        dest_kind: body.dest_kind,
        target,
        pool,
        verbatim: body.verbatim,
        filter: body.filter,
        rate_limit_per_sec: body.rate_limit_per_sec,
        ca_cert,
        secret,
    })
}

/// Ceiling on a stored service-account key. A real Google key file is ~2.3 KiB.
const MAX_SERVICE_ACCOUNT_BYTES: usize = 16 * 1024;

/// Ceiling on a destination's PEM trust bundle. Generous for a chain, small enough that the column
/// cannot be used as arbitrary storage.
const MAX_CA_CERT_BYTES: usize = 64 * 1024;

/// Split `host:port`, accepting `[::1]:514` for a literal IPv6 address. Returns `None` unless both
/// halves are present and the port is a valid non-zero number.
fn split_host_port(target: &str) -> Option<(String, u16)> {
    let (host, port) = if let Some(rest) = target.strip_prefix('[') {
        let (host, rest) = rest.split_once(']')?;
        (host.to_owned(), rest.strip_prefix(':')?)
    } else {
        let (host, port) = target.rsplit_once(':')?;
        // A bare IPv6 literal has several colons and no brackets — reject it rather than treating
        // its last group as a port.
        if host.contains(':') {
            return None;
        }
        (host.to_owned(), port)
    };
    let port: u16 = port.parse().ok()?;
    if host.is_empty() || host.len() > 255 || port == 0 {
        return None;
    }
    Some((host, port))
}

/// Whether a target names this machine. Only literal forms are checked — resolving a hostname here
/// would block the handler on DNS, and the guard this backs is best-effort by design.
fn target_is_local(target: &str) -> bool {
    let Some((host, _)) = split_host_port(target) else {
        return false;
    };
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    host.parse::<std::net::IpAddr>()
        .is_ok_and(|ip| ip.is_loopback() || ip.is_unspecified())
}

async fn list_forward_destinations(State(st): State<ApiState>, headers: HeaderMap) -> Response {
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    if let Some(resp) = authorize(&st, &headers, Permission::ManageConfig) {
        return resp;
    }
    match admin.forward.list().await {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "forwarding destination list failed");
            internal("failed to list forwarding destinations")
        }
    }
}

async fn create_forward_destination(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Json(body): Json<ForwardDestinationBody>,
) -> Response {
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    if let Some(resp) = authorize(&st, &headers, Permission::ManageConfig) {
        return resp;
    }
    let input = match validate_forward_body(&st, body) {
        Ok(input) => input,
        Err(resp) => return *resp,
    };
    match admin.forward.count().await {
        Ok(n) if n >= crate::forward_store::MAX_DESTINATIONS => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "too_many_destinations",
                format!(
                    "at most {} forwarding destinations are supported",
                    crate::forward_store::MAX_DESTINATIONS
                ),
            );
        }
        Ok(_) => {}
        Err(e) => {
            tracing::error!(error = %e, "forwarding destination count failed");
            return internal("failed to create the forwarding destination");
        }
    }
    match admin.forward.create(&input).await {
        Ok(id) => {
            admin.forward_handle.poke();
            (StatusCode::CREATED, Json(serde_json::json!({ "id": id }))).into_response()
        }
        Err(e) => {
            if e.downcast_ref::<sqlx::Error>()
                .and_then(sqlx::Error::as_database_error)
                .is_some_and(|db| db.is_unique_violation())
            {
                return error_response(
                    StatusCode::CONFLICT,
                    "duplicate_name",
                    "a forwarding destination with that name already exists".to_owned(),
                );
            }
            tracing::error!(error = %e, "forwarding destination create failed");
            internal("failed to create the forwarding destination")
        }
    }
}

async fn update_forward_destination(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<ForwardDestinationBody>,
) -> Response {
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    if let Some(resp) = authorize(&st, &headers, Permission::ManageConfig) {
        return resp;
    }
    let input = match validate_forward_body(&st, body) {
        Ok(input) => input,
        Err(resp) => return *resp,
    };
    match admin.forward.update(id, &input).await {
        Ok(true) => {
            admin.forward_handle.poke();
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(false) => not_found("not_found", "no such forwarding destination".to_owned()),
        Err(e) => {
            if e.downcast_ref::<sqlx::Error>()
                .and_then(sqlx::Error::as_database_error)
                .is_some_and(|db| db.is_unique_violation())
            {
                return error_response(
                    StatusCode::CONFLICT,
                    "duplicate_name",
                    "a forwarding destination with that name already exists".to_owned(),
                );
            }
            tracing::error!(error = %e, "forwarding destination update failed");
            internal("failed to update the forwarding destination")
        }
    }
}

async fn delete_forward_destination(
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
    match admin.forward.delete(id).await {
        // Idempotent: a missing id is a no-op 204.
        Ok(_) => {
            admin.forward_handle.poke();
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "forwarding destination delete failed");
            internal("failed to delete the forwarding destination")
        }
    }
}

/// Send one synthetic message to a destination and report what happened. Deliberately independent
/// of the running senders so a still-disabled destination can be validated, and so the caller gets
/// the transport error itself rather than "check the logs".
async fn test_forward_destination(
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
    // `get_open` ignores `enabled` and decrypts the secret: the point of the button is validating a
    // destination *before* turning it on, and testing with the wrong community would be worthless.
    let open = match admin.forward.get_open(id).await {
        Ok(Some(open)) => open,
        Ok(None) => return not_found("not_found", "no such forwarding destination".to_owned()),
        Err(e) => {
            tracing::error!(error = %e, "forwarding destination load failed");
            return internal("failed to load the forwarding destination");
        }
    };
    match crate::forward::send_test(&open).await {
        Ok(()) => Json(serde_json::json!({ "delivered": true })).into_response(),
        // Not a 5xx: the destination's configuration is the caller's, and the text is the
        // transport error (never a payload — syslog bodies carry credentials).
        Err(e) => Json(serde_json::json!({ "delivered": false, "error": e })).into_response(),
    }
}

/// Live forwarding status: per-destination counters plus any online poller that cannot supply the
/// original bytes, so a byte-exact destination silently degrading is visible in the UI.
async fn forwarding_status(State(st): State<ApiState>, headers: HeaderMap) -> Response {
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    if let Some(resp) = authorize(&st, &headers, Permission::ManageConfig) {
        return resp;
    }
    let destinations = admin.forward_handle.status();
    let now = std::time::Instant::now();
    let pollers_without_raw_capture = admin
        .coordinator
        .pollers_missing_cap(yagra_bus::CAP_RAW_CAPTURE, now);
    // Separate from raw capture on purpose: an Increment-1 poller attaches raw bytes to events but
    // relays no flow datagrams at all, so a flow destination fed only by such pollers receives
    // *nothing* — a different failure from "byte-exact silently degraded to rendered".
    let pollers_without_flow_relay = admin
        .coordinator
        .pollers_missing_cap(yagra_bus::CAP_FLOW_RELAY, now);
    Json(serde_json::json!({
        "destinations": destinations,
        "pollers_without_raw_capture": pollers_without_raw_capture,
        "pollers_without_flow_relay": pollers_without_flow_relay,
        // False on an HA standby, whose dispatcher is not running (destination CRUD still works).
        "sending": st.is_leader.load(std::sync::atomic::Ordering::Acquire),
    }))
    .into_response()
}

// ── OIDC login (external IdP, ADR-010 Phase 3) ──────────────────────────────────────────────────

/// Begin an OIDC login: return the IdP authorization URL the browser should be sent to. The CSRF
/// `state`, `nonce`, and PKCE verifier are stashed server-side (in-memory flight). Unauthenticated
/// (pre-session). `503` when no provider is configured/enabled.
async fn oidc_authorize(State(st): State<ApiState>) -> Response {
    let Some(oidc) = st.oidc.as_ref() else {
        return unavailable();
    };
    let config = match oidc.enabled_config().await {
        Ok(Some(c)) => c,
        Ok(None) => {
            return error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "sso_disabled",
                "no OIDC provider is enabled".to_owned(),
            );
        }
        Err(e) => {
            tracing::error!(error = %e, "load OIDC provider failed");
            return internal("failed to load the OIDC provider");
        }
    };
    match crate::oidc::begin_authorize(&config, &st.oidc_flight).await {
        Ok(url) => Json(serde_json::json!({ "authorize_url": url })).into_response(),
        Err(e) => {
            // Discovery / config problems: don't leak provider internals to the browser.
            tracing::error!(error = %e, "OIDC authorize failed");
            error_response(
                StatusCode::BAD_GATEWAY,
                "oidc_error",
                "could not start the SSO login (check the provider configuration)".to_owned(),
            )
        }
    }
}

/// OIDC callback body: the `code` + `state` the WebUI forwards from the IdP redirect.
#[derive(Deserialize)]
struct OidcCallbackBody {
    code: String,
    state: String,
}

/// Complete an OIDC login: exchange the code, validate the ID token, map the IdP groups to a role,
/// JIT-provision the account, and issue a Yagra session — returning `{ token, role }` exactly like
/// local login. Unauthenticated (this mints the session); protected by the `state`/PKCE/nonce.
async fn oidc_callback(State(st): State<ApiState>, Json(body): Json<OidcCallbackBody>) -> Response {
    let Some(oidc) = st.oidc.as_ref() else {
        return unavailable();
    };
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    let config = match oidc.enabled_config().await {
        Ok(Some(c)) => c,
        Ok(None) => {
            return error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "sso_disabled",
                "no OIDC provider is enabled".to_owned(),
            )
        }
        Err(e) => {
            tracing::error!(error = %e, "load OIDC provider failed");
            return internal("failed to load the OIDC provider");
        }
    };
    let login =
        match crate::oidc::complete_callback(&config, &st.oidc_flight, &body.code, &body.state)
            .await
        {
            Ok(login) => login,
            Err(e) => {
                // Covers bad/expired state, code exchange failure, ID-token validation failure, and
                // "no group maps to a role". Audited as a failed OIDC login; the reason stays server-side.
                tracing::warn!(error = %e, "OIDC login rejected");
                audit_record(&admin.audit, "(oidc)", "auth.oidc", 401).await;
                return error_response(
                    StatusCode::UNAUTHORIZED,
                    "oidc_denied",
                    "SSO login could not be completed".to_owned(),
                );
            }
        };
    match admin
        .users
        .upsert_oidc_user(config.id, &login.subject, &login.username, login.role)
        .await
    {
        Ok((user_id, principal)) => {
            let role = principal.role;
            let token = st.sessions.issue(user_id, principal, &login.username);
            audit_record(&admin.audit, &login.username, "auth.oidc", 200).await;
            Json(serde_json::json!({ "token": token, "role": role })).into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "OIDC user provisioning failed");
            internal("failed to provision the SSO account")
        }
    }
}

/// List configured OIDC providers (metadata only — never the client_secret). `ManageUsers`.
async fn list_oidc_providers(State(st): State<ApiState>, headers: HeaderMap) -> Response {
    let Some(oidc) = st.oidc.as_ref() else {
        return unavailable();
    };
    if let Some(resp) = authorize(&st, &headers, Permission::ManageUsers) {
        return resp;
    }
    match oidc.list().await {
        Ok(list) => Json(list).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "list OIDC providers failed");
            internal("failed to list OIDC providers")
        }
    }
}

/// Create an OIDC provider. `ManageUsers`. Audited by the mutating-request middleware.
async fn create_oidc_provider(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Json(input): Json<crate::oidc::OidcProviderInput>,
) -> Response {
    let Some(oidc) = st.oidc.as_ref() else {
        return unavailable();
    };
    if let Some(resp) = authorize(&st, &headers, Permission::ManageUsers) {
        return resp;
    }
    match oidc.create(&input).await {
        Ok(id) => (StatusCode::CREATED, Json(serde_json::json!({ "id": id }))).into_response(),
        Err(e) => error_response(StatusCode::BAD_REQUEST, "invalid_provider", e.to_string()),
    }
}

/// Update an OIDC provider (client_secret omitted keeps the stored value). `ManageUsers`.
async fn update_oidc_provider(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(input): Json<crate::oidc::OidcProviderInput>,
) -> Response {
    let Some(oidc) = st.oidc.as_ref() else {
        return unavailable();
    };
    if let Some(resp) = authorize(&st, &headers, Permission::ManageUsers) {
        return resp;
    }
    match oidc.update(id, &input).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => not_found("provider_not_found", format!("no OIDC provider {id}")),
        Err(e) => error_response(StatusCode::BAD_REQUEST, "invalid_provider", e.to_string()),
    }
}

/// Delete an OIDC provider. `ManageUsers`.
async fn delete_oidc_provider(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Response {
    let Some(oidc) = st.oidc.as_ref() else {
        return unavailable();
    };
    if let Some(resp) = authorize(&st, &headers, Permission::ManageUsers) {
        return resp;
    }
    match oidc.delete(id).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => not_found("provider_not_found", format!("no OIDC provider {id}")),
        Err(e) => {
            tracing::error!(error = %e, "delete OIDC provider failed");
            internal("failed to delete the OIDC provider")
        }
    }
}

// ── AI-assisted RCA (ADR-029) ────────────────────────────────────────────────
//
// Five endpoints across two audiences. Provider configuration is an admin concern — it picks which
// cloud the operator's incident data may cross into, and it holds a billable credential — so it is
// `ManageConfig` and its secret is write-only. Generation is an *operator* concern: the people
// carrying the pager are exactly who needs the explanation, and gating it behind `ManageConfig`
// would put the feature out of reach of its users. What bounds abuse there is the orchestrator's
// rate and concurrency caps, not the role (the conclusion ADR-028 WS-A reached about expensive
// reads). Reading a report back is `View`, so a viewer can be shown an explanation an operator
// generated without being able to spend money themselves.

/// The `POST /api/v1/rca` body.
#[derive(Deserialize)]
struct RcaBody {
    /// The alerting node. May be a symptom of an upstream failure — the context builder follows
    /// `root_cause` to the incident before assembling anything.
    node: Uuid,
    check: Uuid,
    /// Timeline window in seconds; clamped by the orchestrator. Defaults to an hour.
    #[serde(default)]
    window_secs: Option<i64>,
    /// UI language tag (`ja`, `en-GB`, …). The instructions stay English; the answer follows the
    /// reader. Unknown tags fall back to English rather than failing.
    #[serde(default)]
    language: Option<String>,
    /// Regenerate instead of serving a cached report. Still rate-limited.
    #[serde(default)]
    force: bool,
}

/// Map an orchestrator refusal onto a status. Each is a distinct operator action: fix the
/// configuration, wait, look at a different alert, or check the vendor.
fn rca_error_response(e: &crate::rca::orchestrator::RcaError) -> Response {
    use crate::rca::orchestrator::RcaError;
    match e {
        RcaError::NotConfigured => error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "rca_not_configured",
            e.to_string(),
        ),
        // Also 503, but a distinct code: the form has been filled in and something in it is wrong,
        // which is a different next step from "nobody has set this up".
        RcaError::Misconfigured(msg) => error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "rca_misconfigured",
            msg.clone(),
        ),
        // `Retry-After: 60` matches the rate window; a client that honors it comes back exactly
        // when a slot is guaranteed to have freed.
        RcaError::TooManyConcurrent(_) | RcaError::RateLimited(_) => {
            let mut resp =
                error_response(StatusCode::TOO_MANY_REQUESTS, "rate_limited", e.to_string());
            resp.headers_mut().insert(
                axum::http::header::RETRY_AFTER,
                HeaderValue::from_static("60"),
            );
            resp
        }
        RcaError::NoIncident(msg) => not_found("no_incident", msg.clone()),
        // 502, not 500: the failure is the upstream vendor's, and the text is the typed error —
        // never a raw body, which can echo the prompt (and therefore device output) back.
        RcaError::Provider(inner) => error_response(
            StatusCode::BAD_GATEWAY,
            match inner {
                crate::rca::provider::LlmError::Refused(_) => "model_refused",
                _ => "provider_error",
            },
            inner.to_string(),
        ),
        RcaError::Internal(err) => {
            tracing::error!(error = %err, "RCA generation failed");
            internal("failed to generate the analysis")
        }
    }
}

/// Generate (or serve from cache) an explanation of one incident. `AckAlerts`; audited by the
/// mutating-request middleware.
async fn create_rca(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Json(body): Json<RcaBody>,
) -> Response {
    // Authorization first, availability second — the reverse of the older admin handlers, and
    // deliberately so here. Whether an operator has wired up an LLM, and which vendor they chose,
    // is not something an unauthenticated prober should be able to infer from a 503-vs-403; and
    // checking the permission first means the AckAlerts/ManageConfig split holds regardless of
    // deployment state rather than only once a provider exists.
    if let Some(resp) = authorize(&st, &headers, Permission::AckAlerts) {
        return resp;
    }
    let Some(rca) = st.rca.as_ref() else {
        return unavailable();
    };
    let req = crate::rca::orchestrator::RcaRequest {
        node: yagra_common::NodeId::from(body.node),
        check: yagra_common::CheckId::from(body.check),
        window_secs: body.window_secs,
        language: crate::rca::prompt::Language::from_tag(body.language.as_deref().unwrap_or("en")),
        force: body.force,
        username: current_username(&st, &headers).unwrap_or_else(|| "(unknown)".to_owned()),
    };
    match rca.explain(&req).await {
        Ok(report) => Json(report).into_response(),
        Err(e) => rca_error_response(&e),
    }
}

/// Read a stored report. `View` — the explanation is display-only text, so showing it to everyone
/// who can see the alert costs nothing and keeps the incident channel on one shared account.
async fn get_rca(State(st): State<ApiState>, headers: HeaderMap, Path(id): Path<Uuid>) -> Response {
    if let Some(resp) = require_view(&st, &headers) {
        return resp;
    }
    let Some(rca) = st.rca.as_ref() else {
        return unavailable();
    };
    match rca.get(id).await {
        Ok(Some(report)) => Json(report).into_response(),
        Ok(None) => not_found("not_found", format!("no analysis {id}")),
        Err(e) => rca_error_response(&e),
    }
}

/// The active LLM provider's configuration. `ManageConfig`. Never includes the credential.
async fn get_llm_config(State(st): State<ApiState>, headers: HeaderMap) -> Response {
    if let Some(resp) = authorize(&st, &headers, Permission::ManageConfig) {
        return resp;
    }
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    match admin.llm.view().await {
        // `config: null` rather than 404: "not configured yet" is the normal state, and the form
        // wants to render its empty self, not an error. `providers` ships the choices with their
        // placeholders and — importantly — their egress warning, so the UI reads the boundary
        // classification off the same code the adapters obey instead of restating it.
        Ok(view) => Json(serde_json::json!({
            "config": view,
            "providers": crate::rca::provider_choices(),
        }))
        .into_response(),
        Err(e) => {
            tracing::error!(error = %e, "read LLM config failed");
            internal("failed to read the AI provider configuration")
        }
    }
}

/// Create or replace the provider configuration. `ManageConfig`; audited by the middleware.
async fn put_llm_config(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Json(body): Json<crate::rca::store::LlmConfigInput>,
) -> Response {
    if let Some(resp) = authorize(&st, &headers, Permission::ManageConfig) {
        return resp;
    }
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    match admin.llm.save(&body).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        // `save` validates first, so a bad field is the caller's — a 400 naming it beats a 500.
        Err(e) => error_response(StatusCode::BAD_REQUEST, "invalid_llm_config", e.to_string()),
    }
}

/// Send one minimal prompt to the configured provider. `ManageConfig`; audited.
///
/// Deliberately ignores `enabled` — validating a provider before switching it on is the whole point
/// of the button (the same reasoning as the forwarding-destination test).
async fn test_llm_provider(State(st): State<ApiState>, headers: HeaderMap) -> Response {
    if let Some(resp) = authorize(&st, &headers, Permission::ManageConfig) {
        return resp;
    }
    let Some(rca) = st.rca.as_ref() else {
        return unavailable();
    };
    match rca.test().await {
        Ok((latency_ms, reply)) => Json(serde_json::json!({
            "ok": true,
            "latency_ms": latency_ms,
            // Bounded: this is model output rendered in a toast, and a model that ignores "reply
            // with ok" should not be able to paste an essay into the settings page.
            "reply": reply.chars().take(200).collect::<String>(),
        }))
        .into_response(),
        // Not a 5xx: the provider configuration is the caller's, and the typed error is what tells
        // them which part of it is wrong.
        Err(e) => Json(serde_json::json!({ "ok": false, "error": e.to_string() })).into_response(),
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
    let pool = match validate_pool_create(body.pool) {
        Ok(p) => p,
        Err(resp) => return resp,
    };
    match admin
        .repo
        .create_node(
            body.name.trim(),
            address,
            pool.as_deref(),
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

/// Set/clear a node's profile + bound credential and its descriptive maker/model, and optionally
/// move it to a different poll-pool. The node-edit UI loads the current values and resends them, so
/// an unchanged field is preserved.
#[derive(Deserialize)]
struct NodeBindings {
    profile_id: Option<Uuid>,
    credential_id: Option<Uuid>,
    #[serde(default)]
    vendor: Option<String>,
    #[serde(default)]
    model: Option<String>,
    /// Poll-pool assignment (ADR-009). **Absent** = leave the pool unchanged; `""` (or whitespace)
    /// = clear it to the `default` pool; otherwise move the node to that pool (validated as a
    /// NATS-subject-safe token). See [`validate_pool_update`].
    #[serde(default)]
    pool: Option<String>,
}

/// Longest accepted pool name (a single NATS subject token — keep it short and human-manageable).
const MAX_POOL_LEN: usize = 63;

/// Validate an operator-supplied pool name for [`set_node_bindings`], returning the DB update
/// instruction: outer `None` = the field was absent, leave the node's pool unchanged; inner `None`
/// = clear it to NULL (the node falls back to the `default` pool); inner `Some` = set it.
///
/// A pool name becomes the `yagra.jobs.<pool>` / assignment subject, so it must already be a legal
/// single NATS token (`[A-Za-z0-9_-]`). We **reject** anything that would sanitize to a different
/// string (dots, spaces, slashes, …) rather than silently rewriting it, so operator intent stays
/// explicit. Surrounding whitespace is trimmed first (matching the sibling vendor/model fields), and
/// a value that trims to empty clears the pool.
#[allow(clippy::result_large_err)]
fn validate_pool_update(pool: Option<String>) -> Result<Option<Option<String>>, Response> {
    let Some(raw) = pool else {
        return Ok(None); // field absent → leave the pool as-is
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(Some(None)); // explicit clear → NULL (default pool)
    }
    if trimmed.chars().count() > MAX_POOL_LEN {
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            "invalid_pool",
            format!("pool name must be at most {MAX_POOL_LEN} characters"),
        ));
    }
    if yagra_bus::subjects::sanitize_token(trimmed) != trimmed {
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            "invalid_pool",
            "pool name may contain only letters, digits, '_' or '-'".to_owned(),
        ));
    }
    Ok(Some(Some(trimmed.to_owned())))
}

/// [`validate_pool_update`] for a **create** path, where there is no prior value to leave alone:
/// absent and empty both mean "no pool" (inherit from the folder, else the default pool).
///
/// Every path that writes a pool must go through one of these two. The pool name becomes the
/// `yagra.jobs.<pool>` subject verbatim (`subjects::jobs_for_pool` does not sanitize), so an
/// unvalidated value like `tokyo.1` would publish to a subject no poller subscribes to and the
/// node's jobs would be silently discarded.
#[allow(clippy::result_large_err)]
fn validate_pool_create(pool: Option<String>) -> Result<Option<String>, Response> {
    Ok(validate_pool_update(pool)?.flatten())
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
    let pool_update = match validate_pool_update(body.pool) {
        Ok(u) => u,
        Err(resp) => return resp,
    };
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
            pool_update.as_ref().map(|inner| inner.as_deref()),
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

/// The single-purpose monitor kinds a node can be bound to. One node is one kind.
#[derive(Clone, Copy)]
enum MonitorKind {
    Url,
    Dns,
}

impl MonitorKind {
    /// The kind that would conflict with this one.
    fn other(self) -> &'static str {
        match self {
            Self::Url => "DNS",
            Self::Dns => "URL",
        }
    }
}

/// Refuse to bind `incoming` to a node that already carries the *other* monitor kind.
///
/// The scheduler resolves a node's kind in one place ([`crate::scheduler::SpecialMonitor::resolve`])
/// and URL wins, so a node holding both rows would have its DNS check silently never run. Guarding
/// only the DNS writer — which is what shipped — just moved the hole to the URL writer, so both
/// call this. Node *creation* needs no guard: those handlers make a fresh node.
// The `Err` is the standard axum `Response`, same reasoning as `validate_monitor_url`.
#[allow(clippy::result_large_err)]
async fn reject_conflicting_monitor(
    admin: &AdminState,
    node_id: Uuid,
    incoming: MonitorKind,
) -> Result<(), Response> {
    let existing = match incoming {
        MonitorKind::Url => admin.dns_checks.get(node_id).await.map(|c| c.is_some()),
        MonitorKind::Dns => admin.url_checks.get(node_id).await.map(|c| c.is_some()),
    };
    match existing {
        Ok(false) => Ok(()),
        Ok(true) => Err(error_response(
            StatusCode::CONFLICT,
            "conflicting_monitor_kind",
            format!("node is already a {} monitor", incoming.other()),
        )),
        Err(e) => {
            tracing::error!(error = %e, "monitor-kind conflict lookup failed");
            Err(internal("failed to check monitor kind"))
        }
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
    if let Err(resp) = reject_conflicting_monitor(admin, node_id, MonitorKind::Url).await {
        return resp;
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
    let pool = match validate_pool_create(body.pool) {
        Ok(p) => p,
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
            pool.as_deref(),
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

// ── DNS name-resolution monitoring (ADR-033) ───────────────────────────────────────────────

/// Largest history page a client may request.
const DNS_HISTORY_MAX_LIMIT: i64 = 200;

/// Validate an operator-supplied DNS check at the API edge, returning it with the name normalized.
///
/// Two things are enforced here (security.md — parse into checked values at the edge):
/// the name must be a syntactically valid DNS name, and a named resolver must not be an
/// SSRF-blocked address. The resolver policy is deliberately shared with URL monitors: an NMS
/// legitimately points at internal DNS servers (RFC1918/ULA allowed), while loopback, link-local
/// (including 169.254.169.254), multicast and the unspecified address are never real targets. The
/// transport re-checks before opening a socket (defense in depth).
// Same reasoning as `validate_monitor_url`: the `Err` is the standard axum `Response`.
#[allow(clippy::result_large_err)]
fn validate_dns_check(cfg: &DnsCheckConfig) -> Result<DnsCheckConfig, Response> {
    let Some(name) = yagra_common::validate_dns_name(&cfg.name) else {
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            "invalid_dns_name",
            format!("not a usable DNS name: {}", cfg.name),
        ));
    };
    if let Some(ip) = cfg.resolver {
        if yagra_common::is_resolver_blocked(ip) {
            return Err(error_response(
                StatusCode::BAD_REQUEST,
                "blocked_resolver",
                format!("resolver address is not allowed: {ip}"),
            ));
        }
    }
    if cfg.resolver_port == 0 {
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            "invalid_resolver_port",
            "resolver port must be 1-65535".to_owned(),
        ));
    }
    if !(1..=16).contains(&cfg.max_depth) {
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            "invalid_max_depth",
            "max_depth must be 1-16".to_owned(),
        ));
    }
    if !(100..=30_000).contains(&cfg.timeout_ms) {
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            "invalid_timeout",
            "timeout_ms must be 100-30000".to_owned(),
        ));
    }
    Ok(DnsCheckConfig {
        name,
        ..cfg.clone()
    })
}

async fn get_dns_check(
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
    match admin.dns_checks.get(node_id).await {
        Ok(Some(cfg)) => Json(cfg).into_response(),
        Ok(None) => not_found(
            "dns_check_not_found",
            format!("no dns check for node {node_id}"),
        ),
        Err(e) => {
            tracing::error!(error = %e, "get dns check failed");
            internal("failed to load dns check")
        }
    }
}

/// Create or replace a node's DNS-monitor config. The node must already exist.
async fn set_dns_check(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Path(node_id): Path<Uuid>,
    Json(cfg): Json<DnsCheckConfig>,
) -> Response {
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    if let Some(resp) = authorize(&st, &headers, Permission::ManageConfig) {
        return resp;
    }
    let cfg = match validate_dns_check(&cfg) {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    match admin.repo.get_node(node_id).await {
        Ok(Some(_)) => {}
        Ok(None) => return not_found("node_not_found", format!("no node {node_id}")),
        Err(e) => {
            tracing::error!(error = %e, "set dns check: load node failed");
            return internal("failed to load node");
        }
    }
    if let Err(resp) = reject_conflicting_monitor(admin, node_id, MonitorKind::Dns).await {
        return resp;
    }
    match admin.dns_checks.upsert(node_id, &cfg).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => {
            tracing::error!(error = %e, "set dns check failed");
            internal("failed to save dns check")
        }
    }
}

/// Remove a node's DNS-monitor config (the node and its recorded chains are untouched).
async fn delete_dns_check(
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
    match admin.dns_checks.delete(node_id).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => not_found(
            "dns_check_not_found",
            format!("no dns check for node {node_id}"),
        ),
        Err(e) => {
            tracing::error!(error = %e, "delete dns check failed");
            internal("failed to delete dns check")
        }
    }
}

/// The node's current resolution chain, plus how long it has held.
#[derive(Serialize)]
struct DnsChainCurrentDto {
    chain: yagra_common::DnsChain,
    resolved: bool,
    failure_kind: Option<String>,
    first_seen: String,
    last_seen: String,
}

async fn get_dns_chain(
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
    match admin.dns_checks.current_chain(node_id).await {
        Ok(Some(cur)) => Json(DnsChainCurrentDto {
            chain: cur.chain,
            resolved: cur.resolved,
            failure_kind: cur.failure_kind,
            first_seen: cur.first_seen.to_rfc3339(),
            last_seen: cur.last_seen.to_rfc3339(),
        })
        .into_response(),
        Ok(None) => not_found(
            "dns_chain_not_found",
            format!("no resolution recorded for node {node_id}"),
        ),
        Err(e) => {
            tracing::error!(error = %e, "get dns chain failed");
            internal("failed to load dns chain")
        }
    }
}

/// One append-on-change history row.
#[derive(Serialize)]
struct DnsChainChangeDto {
    id: i64,
    at: String,
    chain: yagra_common::DnsChain,
    resolved: bool,
    failure_kind: Option<String>,
    prev_chain_key: Option<String>,
}

/// Keyset cursor for the next page (ADR-019 — never OFFSET).
#[derive(Deserialize)]
struct DnsHistoryQuery {
    #[serde(default)]
    limit: Option<i64>,
    #[serde(default)]
    before_at: Option<String>,
    #[serde(default)]
    before_id: Option<i64>,
}

async fn list_dns_chain_history(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Path(node_id): Path<Uuid>,
    Query(q): Query<DnsHistoryQuery>,
) -> Response {
    if let Some(resp) = require_view(&st, &headers) {
        return resp;
    }
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    let limit = q.limit.unwrap_or(50).clamp(1, DNS_HISTORY_MAX_LIMIT);
    // Both cursor halves or neither — a half-specified cursor would silently page from the top.
    let before = match (q.before_at.as_deref(), q.before_id) {
        (Some(at), Some(id)) => match chrono::DateTime::parse_from_rfc3339(at) {
            Ok(ts) => Some((ts.with_timezone(&chrono::Utc), id)),
            Err(_) => {
                return error_response(
                    StatusCode::BAD_REQUEST,
                    "invalid_cursor",
                    "before_at must be RFC 3339".to_owned(),
                )
            }
        },
        (None, None) => None,
        _ => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "invalid_cursor",
                "before_at and before_id must be given together".to_owned(),
            )
        }
    };

    match admin.dns_checks.list_changes(node_id, before, limit).await {
        Ok(rows) => {
            let next = rows
                .last()
                .filter(|_| i64::try_from(rows.len()).unwrap_or(0) == limit)
                .map(|r| serde_json::json!({ "at": r.at.to_rfc3339(), "id": r.id }));
            let changes: Vec<DnsChainChangeDto> = rows
                .into_iter()
                .map(|r| DnsChainChangeDto {
                    id: r.id,
                    at: r.at.to_rfc3339(),
                    chain: r.chain,
                    resolved: r.resolved,
                    failure_kind: r.failure_kind,
                    prev_chain_key: r.prev_chain_key,
                })
                .collect();
            Json(serde_json::json!({ "changes": changes, "next": next })).into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "list dns chain history failed");
            internal("failed to load dns chain history")
        }
    }
}

/// Create a DNS monitor in one call: a node bound to the built-in DNS profile (so it inherits the
/// default `dns_up` threshold) plus its DNS-check config.
///
/// The check config is spelled out rather than `#[serde(flatten)]`ed (as `CreateUrlMonitor` does)
/// because both the node and the check have a `name`: flattening would bind them to the same JSON
/// key and silently force the display label to equal the resolved name.
#[derive(Deserialize)]
struct CreateDnsMonitor {
    /// Display name for the node.
    name: String,
    /// The DNS name to resolve.
    dns_name: String,
    #[serde(default)]
    parent_id: Option<Uuid>,
    #[serde(default)]
    pool: Option<String>,
    #[serde(default)]
    record_type: yagra_common::DnsRecordType,
    #[serde(default)]
    resolver: Option<std::net::IpAddr>,
    #[serde(default)]
    resolver_port: Option<u16>,
    #[serde(default)]
    max_depth: Option<u8>,
    #[serde(default)]
    timeout_ms: Option<u32>,
}

impl CreateDnsMonitor {
    /// The check config this request describes, with server-side defaults filled in.
    fn config(&self) -> DnsCheckConfig {
        let base = DnsCheckConfig::new(self.dns_name.clone());
        DnsCheckConfig {
            record_type: self.record_type,
            resolver: self.resolver,
            resolver_port: self.resolver_port.unwrap_or(base.resolver_port),
            max_depth: self.max_depth.unwrap_or(base.max_depth),
            timeout_ms: self.timeout_ms.unwrap_or(base.timeout_ms),
            ..base
        }
    }
}

async fn create_dns_monitor(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Json(body): Json<CreateDnsMonitor>,
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
    let config = match validate_dns_check(&body.config()) {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    let pool = match validate_pool_create(body.pool) {
        Ok(p) => p,
        Err(resp) => return resp,
    };
    // Bind to the built-in DNS profile (if present) so the default threshold is inherited.
    let profile = admin
        .repo
        .profile_id_for_category(ProfileCategory::DnsCheck.as_str())
        .await
        .unwrap_or(None);
    // Display address only: a DNS monitor is never pinged, so show the resolver we ask (or the
    // unspecified address when the poller's system resolver is used).
    let address = config
        .resolver
        .unwrap_or(std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED));
    let node_id = match admin
        .repo
        .create_node(
            body.name.trim(),
            address,
            pool.as_deref(),
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
            tracing::error!(error = %e, "create dns monitor: create node failed");
            return internal("failed to create dns monitor");
        }
    };
    if let Err(e) = admin.dns_checks.upsert(node_id, &config).await {
        // The node exists but the check didn't save — roll it back rather than leave a
        // DNS-category node that would never get a job.
        let _ = admin.repo.delete_node(node_id).await;
        tracing::error!(error = %e, "create dns monitor: save check failed");
        return internal("failed to create dns monitor");
    }
    (
        StatusCode::CREATED,
        Json(serde_json::json!({ "id": node_id })),
    )
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

/// Move a node to a poll-pool, or clear it back to inherited. Used by the inventory tree's
/// context menu. Absent or `""` ⇒ NULL (inherit from the folder, else the default pool).
#[derive(Deserialize)]
struct PoolAssignment {
    #[serde(default)]
    pool: Option<String>,
}

/// `PUT /api/v1/nodes/:node_id/pool` — set just the node's own pool.
///
/// Deliberately **not** folded into `set_node_bindings`: that handler overwrites
/// profile/credential/vendor/model unconditionally (only its `pool` is three-state-gated), so a
/// pool-only caller going through it would silently blank all four.
async fn set_node_pool(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<PoolAssignment>,
) -> Response {
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    if let Some(resp) = authorize(&st, &headers, Permission::ManageConfig) {
        return resp;
    }
    // A single-field endpoint has no "leave unchanged" case, so absent and empty both mean clear.
    let pool = match validate_pool_create(body.pool) {
        Ok(p) => p,
        Err(resp) => return resp,
    };
    match admin.repo.set_node_pool(id, pool.as_deref()).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => not_found("node_not_found", format!("no node {id}")),
        Err(e) => {
            tracing::error!(error = %e, "set node pool failed");
            internal("failed to set node pool")
        }
    }
}

/// `PUT /api/v1/node-groups/:id/pool` — set just the folder's pool. Every node beneath it that
/// has no pool of its own follows on the next sweep (see `poolres`).
async fn set_node_group_pool(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<PoolAssignment>,
) -> Response {
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    if let Some(resp) = authorize(&st, &headers, Permission::ManageConfig) {
        return resp;
    }
    let pool = match validate_pool_create(body.pool) {
        Ok(p) => p,
        Err(resp) => return resp,
    };
    match admin.groups.set_pool(id, pool.as_deref()).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => not_found("group_not_found", format!("no group {id}")),
        Err(e) => {
            tracing::error!(error = %e, "set group pool failed");
            internal("failed to set group pool")
        }
    }
}

/// One pool offered by the pool picker.
#[derive(Debug, Serialize, PartialEq, Eq, PartialOrd, Ord)]
struct PoolOption {
    /// Pool name.
    name: String,
    /// Whether a live poller currently serves it. A pool with none takes the legacy per-job path
    /// onto a subject nothing subscribes to, so its jobs are silently discarded — the picker has to
    /// say so rather than present it as an equivalent choice.
    live: bool,
}

/// Merge the pools nodes use, the pools folders assign, and the pools with a live poller into the
/// picker's option list. Pure so the union, the default-first ordering, and the `live` flag are
/// unit-testable without a database or a coordinator.
///
/// [`yagra_bus::DEFAULT_POOL`] is always offered (it is where an unassigned node lands, whether or
/// not anything references it explicitly); the rest follow alphabetically.
fn build_pool_options(
    node_pools: Vec<String>,
    group_pools: Vec<String>,
    live: &std::collections::HashSet<String>,
) -> Vec<PoolOption> {
    let mut names: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for p in node_pools.into_iter().chain(group_pools) {
        let trimmed = p.trim();
        if !trimmed.is_empty() {
            names.insert(trimmed.to_owned());
        }
    }
    names.extend(live.iter().cloned());
    names.remove(yagra_bus::DEFAULT_POOL);

    let option = |name: String| PoolOption {
        live: live.contains(&name),
        name,
    };
    std::iter::once(option(yagra_bus::DEFAULT_POOL.to_owned()))
        .chain(names.into_iter().map(option))
        .collect()
}

/// `GET /api/v1/pools` — the pools that exist, for the assignment picker. `View`-gated; names
/// only, no telemetry.
///
/// Deliberately separate from `GET /pollers`, which scans the whole node table to build its
/// per-pool counts; this is two indexed `DISTINCT`s and is loaded by an ordinary page.
async fn list_pools(State(st): State<ApiState>, headers: HeaderMap) -> Response {
    if let Some(resp) = require_view(&st, &headers) {
        return resp;
    }
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    // A read error degrades to "fewer suggestions", never to a failed picker — the operator can
    // still type any pool via Custom.
    let node_pools = admin.repo.distinct_pools().await.unwrap_or_else(|e| {
        tracing::warn!(error = %e, "listing node pools failed");
        Vec::new()
    });
    let group_pools = admin.groups.distinct_pools().await.unwrap_or_else(|e| {
        tracing::warn!(error = %e, "listing folder pools failed");
        Vec::new()
    });
    let live = admin.coordinator.live_pools(Instant::now());
    Json(serde_json::json!({
        "pools": build_pool_options(node_pools, group_pools, &live)
    }))
    .into_response()
}

/// Set (or clear) a node's **dependency parent** (upstream). `parent_id: null` removes the
/// dependency. This is the alert-suppression edge (parent down ⇒ suppress children, ADR-015) —
/// distinct from `PUT /nodes/:id/group`, which moves the node in the inventory folder tree.
#[derive(Deserialize)]
struct NodeParentAssignment {
    parent_id: Option<Uuid>,
}

async fn set_node_parent(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<NodeParentAssignment>,
) -> Response {
    let Some(admin) = st.admin.as_ref() else {
        return unavailable();
    };
    if let Some(resp) = authorize(&st, &headers, Permission::ManageConfig) {
        return resp;
    }
    // Validate the requested edge before persisting: no self-dependency, the parent must exist,
    // and the new edge must not close a cycle (the dependency graph is a single-parent forest).
    if let Some(parent) = body.parent_id {
        if parent == id {
            return error_response(
                StatusCode::BAD_REQUEST,
                "invalid_dependency",
                "a node cannot depend on itself".to_owned(),
            );
        }
        let nodes = match admin.repo.list_nodes().await {
            Ok(n) => n,
            Err(e) => {
                tracing::error!(error = %e, "load nodes for dependency check failed");
                return internal("failed to set dependency");
            }
        };
        if !nodes.iter().any(|n| n.id.as_uuid() == parent) {
            return error_response(
                StatusCode::BAD_REQUEST,
                "parent_not_found",
                format!("no node {parent}"),
            );
        }
        // Reuse the folder-tree cycle guard: the dependency edges have the same (id, parent) shape.
        let edges: Vec<(Uuid, Option<Uuid>)> = nodes
            .iter()
            .map(|n| (n.id.as_uuid(), n.parent.map(|p| p.as_uuid())))
            .collect();
        if would_create_cycle(&edges, id, Some(parent)) {
            return error_response(
                StatusCode::BAD_REQUEST,
                "invalid_dependency",
                "that dependency would create a cycle".to_owned(),
            );
        }
    }
    match admin.repo.set_node_parent(id, body.parent_id).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => not_found("node_not_found", format!("no node {id}")),
        Err(e) => {
            tracing::error!(error = %e, "set node parent failed");
            internal("failed to set dependency")
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
    /// Poll-pool this folder assigns to its nodes (ADR-009/020). Same three-state contract as
    /// [`NodeBindings::pool`]: **absent** leaves it unchanged, `""` clears it (inherit from the
    /// nearest ancestor, else the default pool), otherwise it moves the folder's nodes to that
    /// pool. See [`validate_pool_update`].
    #[serde(default)]
    pool: Option<String>,
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
    let pool = match validate_pool_create(body.pool) {
        Ok(p) => p,
        Err(resp) => return resp,
    };
    match admin
        .groups
        .create(
            body.name.trim(),
            group_type,
            body.parent_id,
            pool.as_deref(),
        )
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
    let pool_update = match validate_pool_update(body.pool) {
        Ok(u) => u,
        Err(resp) => return resp,
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
        .update(
            id,
            body.name.trim(),
            group_type,
            body.parent_id,
            pool_update.as_ref().map(|inner| inner.as_deref()),
        )
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
        ChannelConfig::Webhook { url } => validate_webhook_url(url),
        ChannelConfig::Email { host, from, to, .. }
            if host.trim().is_empty() || from.trim().is_empty() || to.trim().is_empty() =>
        {
            Err("email host/from/to required")
        }
        ChannelConfig::PagerDuty {
            routing_key,
            api_url,
        } => {
            if routing_key.trim().is_empty() {
                return Err("PagerDuty routing key required");
            }
            match api_url.as_deref() {
                None => Ok(()),
                Some(url) => validate_vendor_url(url, PAGERDUTY_HOSTS),
            }
        }
        ChannelConfig::Jsm { api_url, api_key } => {
            if api_key.trim().is_empty() {
                return Err("JSM API key required");
            }
            validate_vendor_url(api_url, JSM_HOSTS)
        }
        _ => Ok(()),
    }
}

/// Exact-host allowlists for the fixed-vendor channels. Exact match, not suffix match —
/// suffix matching is how allowlist bypasses happen (`evil-events.pagerduty.com.attacker.io`).
const PAGERDUTY_HOSTS: &[&str] = &["events.pagerduty.com", "events.eu.pagerduty.com"];
const JSM_HOSTS: &[&str] = &[
    "api.atlassian.com",
    "api.opsgenie.com",
    "api.eu.opsgenie.com",
];

/// Validate a fixed-vendor API URL: https only, host exactly in the vendor's allowlist.
/// These are SaaS endpoints, so this is stricter than the generic webhook check — a
/// `ManageConfig` user can't point the sealed credential at an arbitrary server.
fn validate_vendor_url(url: &str, allowed_hosts: &[&str]) -> Result<(), &'static str> {
    let url = url.trim();
    if url.is_empty() {
        return Err("API URL required");
    }
    let parsed = reqwest::Url::parse(url).map_err(|_| "API URL is not a valid URL")?;
    if parsed.scheme() != "https" {
        return Err("API URL must be https");
    }
    let Some(host) = parsed.host_str() else {
        return Err("API URL must have a host");
    };
    if !allowed_hosts.contains(&host) {
        return Err("API URL host is not an allowed vendor endpoint");
    }
    Ok(())
}

/// Validate a notification-webhook URL at the API edge (SSRF: a `ManageConfig` user could
/// otherwise point a channel at `http://169.254.169.254/…`, and core — which holds the DB and
/// the KEK — would POST there on every alert). Scheme must be http/https; an IP-literal host
/// that is loopback/link-local (incl. cloud metadata)/multicast/unspecified is refused. The
/// runtime delivery path re-checks resolved addresses (defense in depth — see `WebhookChannel`).
fn validate_webhook_url(url: &str) -> Result<(), &'static str> {
    let url = url.trim();
    if url.is_empty() {
        return Err("webhook url required");
    }
    let parsed = reqwest::Url::parse(url).map_err(|_| "webhook url is not a valid URL")?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err("webhook url scheme must be http or https");
    }
    let Some(host) = parsed.host_str() else {
        return Err("webhook url must have a host");
    };
    let literal = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    if let Ok(ip) = literal.parse::<IpAddr>() {
        if is_ssrf_blocked(ip) {
            return Err("webhook url target is not allowed (loopback / link-local / metadata)");
        }
    }
    Ok(())
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
    /// Poll-pool to run the sweep in (ADR-009/020). Absent/empty = legacy global discovery
    /// (compat). When it names a pool with a live poller, the sweep is routed to that pool's own
    /// discovery subject so a remote-site poller scans its own network; otherwise it falls back to
    /// the global subject (so an operator can't accidentally aim a scan at a pool no poller serves).
    #[serde(default)]
    pool: Option<String>,
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
    // Route the sweep to a pool's own discovery subject only when that pool has a live poller (one
    // subscribed to it); otherwise fall back to the legacy global subject for N/N-1 compatibility
    // (an old wildcard poller still absorbs it, and a typo'd pool name never black-holes the scan).
    let requested_pool = body
        .pool
        .as_deref()
        .map(str::trim)
        .filter(|p| !p.is_empty());
    let pool_route = match requested_pool {
        Some(p) if admin.coordinator.live_pools(Instant::now()).contains(p) => Some(p),
        _ => None,
    };
    match admin
        .discovery
        .start(targets, body.communities, credentials, pool_route)
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
    let now = now_unix_s();
    // One batched fetch for the whole node (3 TSDB round-trips), not 3 queries per interface — a
    // 48-port switch refreshing for every open client would otherwise be ~150 sequential queries.
    let live = st
        .store
        .node_interface_live(node_id, DEFAULT_RATE_LOOKBACK_SECS)
        .await;
    let mut out = Vec::with_capacity(metas.len());
    for m in metas {
        let ifindex = u32::try_from(m.ifindex).unwrap_or(0);
        let l = live.get(&m.ifindex).copied().unwrap_or_default();
        // Octet counters are bytes/sec via rate(); ×8 ⇒ bits/sec.
        let in_bps = l.in_bps.map(|r| r * 8.0);
        let out_bps = l.out_bps.map(|r| r * 8.0);
        let oper_status = l.oper_status;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::StaticNodeList;
    use crate::sink::InMemorySink;
    use axum::body::{to_bytes, Body};
    use axum::http::{header::AUTHORIZATION, Request};
    use tower::ServiceExt; // for `oneshot`
    use yagra_bus::{CheckOutcome, PollResult, Sample};

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
    fn webhook_url_validation_blocks_ssrf_targets() {
        // Allowed: public and legitimate internal (private-range) webhook endpoints.
        assert!(validate_webhook_url("https://hooks.example.com/abc").is_ok());
        assert!(validate_webhook_url("http://10.0.0.5:8080/notify").is_ok());
        // Rejected: SSRF-escalation surface (loopback / cloud metadata / mapped metadata).
        assert!(validate_webhook_url("http://169.254.169.254/latest/meta-data/").is_err());
        assert!(validate_webhook_url("http://127.0.0.1/hook").is_err());
        assert!(validate_webhook_url("http://[::ffff:169.254.169.254]/").is_err());
        // Rejected: bad scheme / empty / hostless.
        assert!(validate_webhook_url("ftp://example.com/x").is_err());
        assert!(validate_webhook_url("   ").is_err());
        assert!(validate_webhook_url("not a url").is_err());
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
    async fn version_returns_core_version() {
        // Public (no auth), returns the running core crate version = the workspace version.
        let app = router(state_with(Arc::new(InMemorySink::default())));
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/version")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["core"], env!("CARGO_PKG_VERSION"));
    }

    #[tokio::test]
    async fn system_health_returns_degraded_json_in_skeleton_mode() {
        // No admin/DB and no poll loop ⇒ 200 with a degraded body (never 503), so the UI renders.
        // The in-memory sink's `healthy()` defaults to true, so only tsdb is reachable.
        let app = router(state_with(Arc::new(InMemorySink::default())));
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/system-health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["overall"], "degraded");
        assert_eq!(body["tsdb"]["reachable"], true);
        assert_eq!(body["postgres"]["reachable"], false);
        assert_eq!(body["bus"]["reachable"], false);
    }

    #[test]
    fn bus_sweep_freshness_window_tracks_default_interval() {
        // Default 30s ⇒ window = 30*2 + 60 = 120s. A sweep 100s ago is fresh; 200s ago is stale.
        let now = 1_000_000i64;
        assert!(bus_sweep_is_fresh(Some(now - 100_000), 30, now));
        assert!(!bus_sweep_is_fresh(Some(now - 200_000), 30, now));
        // Never-swept ⇒ never fresh, regardless of interval.
        assert!(!bus_sweep_is_fresh(None, 30, now));
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

    #[test]
    fn vendor_url_allowlist_is_exact_host_https_only() {
        // PagerDuty: both regions pass; http and lookalike hosts fail.
        assert!(
            validate_vendor_url("https://events.pagerduty.com/v2/enqueue", PAGERDUTY_HOSTS).is_ok()
        );
        assert!(validate_vendor_url(
            "https://events.eu.pagerduty.com/v2/enqueue",
            PAGERDUTY_HOSTS
        )
        .is_ok());
        assert!(
            validate_vendor_url("http://events.pagerduty.com/v2/enqueue", PAGERDUTY_HOSTS).is_err()
        );
        // Suffix tricks must fail (exact host match, not ends_with).
        assert!(validate_vendor_url(
            "https://events.pagerduty.com.attacker.io/v2/enqueue",
            PAGERDUTY_HOSTS
        )
        .is_err());
        assert!(validate_vendor_url("https://evil.example/v2/enqueue", PAGERDUTY_HOSTS).is_err());

        // JSM: Atlassian + Opsgenie hosts pass.
        assert!(validate_vendor_url(
            "https://api.atlassian.com/jsm/ops/integration/v2",
            JSM_HOSTS
        )
        .is_ok());
        assert!(validate_vendor_url("https://api.opsgenie.com/v2", JSM_HOSTS).is_ok());
        assert!(validate_vendor_url("https://api.eu.opsgenie.com/v2", JSM_HOSTS).is_ok());
        assert!(validate_vendor_url("https://api.atlassian.com.evil.io/v2", JSM_HOSTS).is_err());

        // PD/JSM channel configs route through validate_channel_config.
        assert!(validate_channel_config(&ChannelConfig::PagerDuty {
            routing_key: "rk".into(),
            api_url: None,
        })
        .is_ok());
        assert!(validate_channel_config(&ChannelConfig::PagerDuty {
            routing_key: "  ".into(),
            api_url: None,
        })
        .is_err());
        assert!(validate_channel_config(&ChannelConfig::Jsm {
            api_url: "https://api.atlassian.com/jsm/ops/integration/v2".into(),
            api_key: "k".into(),
        })
        .is_ok());
        assert!(validate_channel_config(&ChannelConfig::Jsm {
            api_url: "https://example.com/".into(),
            api_key: "k".into(),
        })
        .is_err());
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
    fn pool_name_validation() {
        // Absent → leave the node's pool unchanged.
        assert_eq!(validate_pool_update(None).ok(), Some(None));
        // Empty / whitespace-only → clear to NULL (the default pool).
        assert_eq!(
            validate_pool_update(Some(String::new())).ok(),
            Some(Some(None))
        );
        assert_eq!(
            validate_pool_update(Some("   ".to_owned())).ok(),
            Some(Some(None))
        );
        // A legal NATS-subject token → set (surrounding whitespace is trimmed).
        assert_eq!(
            validate_pool_update(Some("tokyo".to_owned())).ok(),
            Some(Some(Some("tokyo".to_owned())))
        );
        assert_eq!(
            validate_pool_update(Some("  edge-1_lab  ".to_owned())).ok(),
            Some(Some(Some("edge-1_lab".to_owned())))
        );
        // Rejected: anything that would sanitize to a different subject token (dot / space / slash).
        assert!(validate_pool_update(Some("tokyo.1".to_owned())).is_err());
        assert!(validate_pool_update(Some("east dc".to_owned())).is_err());
        assert!(validate_pool_update(Some("a/b".to_owned())).is_err());
        // Rejected: over the length bound; the bound itself is accepted.
        assert!(validate_pool_update(Some("p".repeat(MAX_POOL_LEN + 1))).is_err());
        assert!(validate_pool_update(Some("p".repeat(MAX_POOL_LEN))).is_ok());
    }

    #[test]
    fn pool_name_validation_on_create_collapses_absent_and_empty() {
        // A create has no prior value to leave alone, so "absent" and "cleared" are the same answer.
        assert_eq!(validate_pool_create(None).ok(), Some(None));
        assert_eq!(validate_pool_create(Some(String::new())).ok(), Some(None));
        assert_eq!(
            validate_pool_create(Some(" tokyo ".to_owned())).ok(),
            Some(Some("tokyo".to_owned()))
        );
        // The create paths used to skip validation entirely: `tokyo.1` reached the DB and its jobs
        // were published to `yagra.jobs.tokyo.1`, a subject no poller subscribes to, and silently
        // discarded (plain NATS, not JetStream).
        assert!(validate_pool_create(Some("tokyo.1".to_owned())).is_err());
        assert!(validate_pool_create(Some("east dc".to_owned())).is_err());
    }

    #[test]
    fn pool_options_union_default_first_and_flag_liveness() {
        let live: std::collections::HashSet<String> = ["tokyo".to_owned(), "spare".to_owned()]
            .into_iter()
            .collect();
        let opts = build_pool_options(
            // Duplicates within and across the two sources, plus blank/whitespace junk from rows
            // written before the API validated pool names.
            vec![
                "tokyo".to_owned(),
                "osaka".to_owned(),
                "tokyo".to_owned(),
                "  ".to_owned(),
            ],
            vec!["osaka".to_owned(), "  edge  ".to_owned()],
            &live,
        );
        let names: Vec<&str> = opts.iter().map(|o| o.name.as_str()).collect();
        // Default always offered and always first (it is where an unassigned node lands, whether
        // or not anything references it); the rest alphabetical, deduped, trimmed.
        assert_eq!(names, vec!["default", "edge", "osaka", "spare", "tokyo"]);

        let live_of = |n: &str| opts.iter().find(|o| o.name == n).map(|o| o.live);
        // A pool only referenced by a live poller still appears (nothing is assigned to it yet).
        assert_eq!(live_of("spare"), Some(true));
        assert_eq!(live_of("tokyo"), Some(true));
        // Assigned but with no live poller — the picker must be able to warn about these.
        assert_eq!(live_of("osaka"), Some(false));
        assert_eq!(live_of("edge"), Some(false));
        assert_eq!(live_of("default"), Some(false));
    }

    #[test]
    fn pool_options_are_offered_even_with_nothing_configured() {
        let opts = build_pool_options(Vec::new(), Vec::new(), &std::collections::HashSet::new());
        assert_eq!(opts.len(), 1);
        assert_eq!(opts[0].name, yagra_bus::DEFAULT_POOL);
        assert!(!opts[0].live);
    }

    // NB: the 400-on-a-bad-pool path is covered directly by
    // `pool_name_validation_on_create_collapses_absent_and_empty` — both handlers run
    // `validate_pool_create`, but only after the admin check, so an HTTP-level test in skeleton
    // mode would assert 503 and prove nothing.

    #[tokio::test]
    async fn pool_writes_are_unavailable_without_admin() {
        // Skeleton mode has no metadata store; both writes report the standard 503 rather than
        // pretending to have moved anything.
        for uri in [
            format!("/api/v1/nodes/{}/pool", Uuid::nil()),
            format!("/api/v1/node-groups/{}/pool", Uuid::nil()),
        ] {
            let app = router(state_with(Arc::new(InMemorySink::default())));
            let resp = app
                .oneshot(
                    Request::builder()
                        .method("PUT")
                        .uri(&uri)
                        .header("content-type", "application/json")
                        .body(Body::from(r#"{"pool":"tokyo"}"#))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE, "{uri}");
        }
    }

    #[test]
    fn polled_by_states_cover_every_answer() {
        let assigned = || Some("edge-1".to_owned());

        // A standby core runs no coordinator, so its empty registry must read as "unknown" rather
        // than as the plausible-but-wrong "legacy_fanout" it would otherwise produce fleet-wide.
        for (meraki, owner, live) in [
            (false, None, false),
            (true, assigned(), true),
            (false, assigned(), true),
        ] {
            assert_eq!(
                resolve_polled_by(false, meraki, owner, live).state,
                "unknown"
            );
        }
        // Meraki devices are polled by core's org collector — outranks any ring answer.
        assert_eq!(
            resolve_polled_by(true, true, assigned(), true).state,
            "meraki"
        );
        // The normal case: the node is in a live poller's published working set.
        let owned = resolve_polled_by(true, false, assigned(), true);
        assert_eq!(owned.state, "assigned");
        assert_eq!(owned.poller_id.as_deref(), Some("edge-1"));
        // In a working-set pool but not (yet) in anyone's set: added since the last sweep, or it
        // builds no specs at all.
        let pending = resolve_polled_by(true, false, None, true);
        assert_eq!(pending.state, "pending");
        assert_eq!(pending.poller_id, None);
        // No live poller in the pool: the scheduler falls back to legacy per-job publish, whose
        // subject has no subscriber — i.e. possibly unmonitored, and definitely no single owner.
        let legacy = resolve_polled_by(true, false, None, false);
        assert_eq!(legacy.state, "legacy_fanout");
        assert_eq!(legacy.poller_id, None);
    }

    #[test]
    fn host_port_split_handles_ipv6_literals_and_rejects_ambiguity() {
        assert_eq!(
            split_host_port("siem.example.com:514"),
            Some(("siem.example.com".to_owned(), 514))
        );
        assert_eq!(
            split_host_port("10.0.0.5:1514"),
            Some(("10.0.0.5".to_owned(), 1514))
        );
        // A literal v6 address needs brackets; without them its colons are ambiguous, so it is
        // rejected rather than silently parsed as `2001:db8::1` port `1`.
        assert_eq!(
            split_host_port("[2001:db8::1]:514"),
            Some(("2001:db8::1".to_owned(), 514))
        );
        for bad in [
            "2001:db8::1:514",
            "siem.example.com",
            ":514",
            "host:0",
            "host:70000",
            "host:syslog",
            "",
        ] {
            assert_eq!(split_host_port(bad), None, "{bad} should not parse");
        }
    }

    #[test]
    fn local_target_detection_covers_the_loop_guard_cases() {
        for local in [
            "localhost:514",
            "LOCALHOST:514",
            "127.0.0.1:514",
            "[::1]:162",
        ] {
            assert!(target_is_local(local), "{local}");
        }
        // 0.0.0.0 is "this host" too — a destination pointing there loops just as readily.
        assert!(target_is_local("0.0.0.0:514"));
        for remote in ["10.0.0.5:514", "siem.example.com:514", "not-a-target"] {
            assert!(!target_is_local(remote), "{remote}");
        }
    }

    /// A live registry view for the merge tests (online → recent, offline → stale).
    fn live_view(id: &str, pool: &str, online: bool) -> PollerView {
        PollerView {
            id: id.to_owned(),
            pool: pool.to_owned(),
            online,
            seconds_since_seen: if online { 3 } else { 120 },
            version: "0.1.2".to_owned(),
            incarnation: Uuid::nil(),
            working_set_nodes: 5,
            working_set_specs: 9,
            inflight: 0,
            results_total: 42,
            host: None,
            listeners: Vec::new(),
            caps: Vec::new(),
        }
    }

    /// A durable inventory row for the merge tests.
    fn inv_row(id: &str, pool: &str) -> PollerRow {
        PollerRow {
            id: id.to_owned(),
            pool: pool.to_owned(),
            first_seen: "2026-07-06T00:00:00+00:00".to_owned(),
            last_seen: "2026-07-06T01:00:00+00:00".to_owned(),
            last_version: Some("0.1.1".to_owned()),
            last_incarnation: None,
        }
    }

    #[test]
    fn build_pollers_response_merges_live_and_inventory() {
        // p1: online live + durable row → online, live stats/version, PG timestamps.
        // p2: inventory only (coordinator forgot it on TTL) → offline, durable version, 0 stats.
        // p3: live only, not yet persisted (inside the 60s upsert throttle) → still listed, null ts.
        let live = vec![
            live_view("p1", "default", true),
            live_view("p3", "lab", true),
        ];
        let inventory = vec![inv_row("p1", "default"), inv_row("p2", "default")];
        let mut node_pools = std::collections::HashMap::new();
        node_pools.insert("default".to_owned(), 10usize);
        let resp = build_pollers_response(inventory, live, node_pools);

        assert_eq!(
            resp.pollers
                .iter()
                .map(|p| p.id.as_str())
                .collect::<Vec<_>>(),
            ["p1", "p2", "p3"],
            "pollers are sorted by id"
        );
        let by_id = |id: &str| resp.pollers.iter().find(|p| p.id == id).unwrap();

        let p1 = by_id("p1");
        assert_eq!(p1.status, "online");
        assert_eq!(p1.version.as_deref(), Some("0.1.2"), "live version wins");
        assert_eq!(p1.last_seen.as_deref(), Some("2026-07-06T01:00:00+00:00"));
        assert_eq!(p1.working_set_nodes, 5);
        assert_eq!(p1.results_total, 42);

        let p2 = by_id("p2");
        assert_eq!(p2.status, "offline");
        assert_eq!(
            p2.version.as_deref(),
            Some("0.1.1"),
            "offline poller falls back to the durable version"
        );
        assert_eq!(p2.working_set_nodes, 0);
        assert_eq!(p2.results_total, 0);

        let p3 = by_id("p3");
        assert_eq!(p3.status, "online");
        assert_eq!(p3.pool, "lab");
        assert_eq!(
            p3.last_seen, None,
            "a not-yet-persisted poller has no timestamp"
        );
        assert_eq!(p3.first_seen, None);
    }

    #[test]
    fn build_pollers_response_maps_host_columns_from_live_sample() {
        let mut online = live_view("p1", "default", true);
        online.host = Some(HostSample {
            cpu_pct: 40.0,
            mem_used_bytes: 3,
            mem_total_bytes: 4,
            disks: vec![DiskUsage {
                mount: "root".into(),
                used_bytes: 60,
                size_bytes: 100,
            }],
            ..Default::default()
        });
        let resp =
            build_pollers_response(Vec::new(), vec![online], std::collections::HashMap::new());
        let p1 = &resp.pollers[0];
        assert_eq!(p1.cpu_pct, Some(40.0));
        assert_eq!(p1.mem_used_pct, Some(75.0));
        assert_eq!(p1.disk_used_pct, Some(60.0));

        // A poller with no host sample (offline / N-1) exposes null host columns.
        let resp = build_pollers_response(
            Vec::new(),
            vec![live_view("p2", "default", true)],
            std::collections::HashMap::new(),
        );
        assert_eq!(resp.pollers[0].cpu_pct, None);
        assert_eq!(resp.pollers[0].disk_used_pct, None);
    }

    #[test]
    fn build_pollers_response_pool_summary_modes_and_warnings() {
        // default: nodes + a live poller → working_set, no warning.
        // legacy-pool: nodes but only an OFFLINE poller → legacy + warning (offline doesn't count).
        // waiting: a live poller but no nodes → working_set, no warning (poller idle).
        let live = vec![
            live_view("p1", "default", true),
            live_view("p-off", "legacy-pool", false),
            live_view("p-wait", "waiting", true),
        ];
        let mut node_pools = std::collections::HashMap::new();
        node_pools.insert("default".to_owned(), 10usize);
        node_pools.insert("legacy-pool".to_owned(), 4usize);
        let resp = build_pollers_response(Vec::new(), live, node_pools);

        assert_eq!(
            resp.pools
                .iter()
                .map(|p| p.pool.as_str())
                .collect::<Vec<_>>(),
            ["default", "legacy-pool", "waiting"],
            "pools are sorted by name"
        );
        let pool = |name: &str| resp.pools.iter().find(|p| p.pool == name).unwrap();

        let d = pool("default");
        assert_eq!(
            (d.nodes, d.live_pollers, d.mode, d.warning),
            (10, 1, "working_set", None)
        );
        let l = pool("legacy-pool");
        assert_eq!(
            (l.nodes, l.live_pollers, l.mode, l.warning),
            (4, 0, "legacy", Some("nodes_without_live_poller"))
        );
        let w = pool("waiting");
        assert_eq!(
            (w.nodes, w.live_pollers, w.mode, w.warning),
            (0, 1, "working_set", None)
        );
    }

    #[tokio::test]
    async fn pollers_list_unavailable_without_admin() {
        // Public skeleton mode (admin: None): no coordinator/DB → the standard 503.
        let app = router(state_with(Arc::new(InMemorySink::default())));
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/pollers")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body_json(resp).await["error"]["code"], "admin_unavailable");
    }

    #[tokio::test]
    async fn pollers_list_requires_auth_in_private_mode() {
        // Private mode: the Pollers view is View-gated like the other fleet reads.
        let (state, _token) = private_state_with(Arc::new(InMemorySink::default()));
        let resp = router(state)
            .oneshot(
                Request::builder()
                    .uri("/api/v1/pollers")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn delete_poller_forbidden_for_non_admin() {
        use yagra_common::Role;
        // Deleting a poller needs ManageConfig; a Viewer/Operator is rejected before any DB/admin
        // work (authorize-first ordering), so the RBAC gate is testable without a database.
        for role in [Role::Viewer, Role::Operator] {
            let (state, token) = state_with_role_token(role);
            let resp = router(state)
                .oneshot(
                    Request::builder()
                        .method("DELETE")
                        .uri("/api/v1/pollers/edge-1")
                        .header(AUTHORIZATION, format!("Bearer {token}"))
                        .body(Body::empty())
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
    async fn delete_poller_admin_passes_authorization() {
        // Admin clears the RBAC gate; with no DB wired it then hits the admin/503 fallback — so an
        // admin sees 503 (auth passed), not 403. Proves ManageConfig admits the Admin role.
        let (state, token) = state_with_role_token(yagra_common::Role::Admin);
        let resp = router(state)
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/api/v1/pollers/edge-1")
                    .header(AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body_json(resp).await["error"]["code"], "admin_unavailable");
    }

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

    #[test]
    fn every_rca_refusal_maps_to_a_distinct_actionable_status() {
        // Each of these asks the operator for a different thing — fix the config, wait, look
        // elsewhere, chase the vendor — so collapsing any two into one status would lose that.
        use crate::rca::orchestrator::RcaError;
        use crate::rca::provider::LlmError;
        let cases = [
            (RcaError::NotConfigured, StatusCode::SERVICE_UNAVAILABLE),
            (
                RcaError::Misconfigured("an API key is required".to_owned()),
                StatusCode::SERVICE_UNAVAILABLE,
            ),
            (RcaError::RateLimited(10), StatusCode::TOO_MANY_REQUESTS),
            (
                RcaError::TooManyConcurrent(2),
                StatusCode::TOO_MANY_REQUESTS,
            ),
            (
                RcaError::NoIncident("gone".to_owned()),
                StatusCode::NOT_FOUND,
            ),
            (
                RcaError::Provider(LlmError::Timeout),
                StatusCode::BAD_GATEWAY,
            ),
            (
                RcaError::Provider(LlmError::Refused(None)),
                StatusCode::BAD_GATEWAY,
            ),
        ];
        for (err, want) in cases {
            let resp = rca_error_response(&err);
            assert_eq!(resp.status(), want, "{err:?}");
            // A rate refusal must say when to come back, or a UI retry loop just hammers the
            // window it is already saturating.
            if want == StatusCode::TOO_MANY_REQUESTS {
                assert!(resp.headers().contains_key(axum::http::header::RETRY_AFTER));
            }
        }
    }

    #[test]
    fn a_monitor_kind_conflict_names_the_kind_the_node_already_has() {
        // The guard is symmetric now (it used to exist on the DNS writer only), so the message
        // must name the *existing* kind, not the incoming one — otherwise the URL direction would
        // tell an operator their node "is already a URL monitor" while they were setting a URL.
        assert_eq!(MonitorKind::Url.other(), "DNS");
        assert_eq!(MonitorKind::Dns.other(), "URL");
    }

    #[tokio::test]
    async fn a_model_refusal_never_reads_as_an_outage() {
        // The model declining is a different outcome from the vendor being broken: nobody should
        // go page a cloud provider over a safety filter.
        use crate::rca::orchestrator::RcaError;
        use crate::rca::provider::LlmError;
        let resp = rca_error_response(&RcaError::Provider(LlmError::Refused(None)));
        assert_eq!(body_json(resp).await["error"]["code"], "model_refused");
    }

    #[tokio::test]
    async fn an_incomplete_configuration_says_what_is_missing() {
        // "Not set up" and "set up wrong" are both 503, but they are different jobs for the
        // operator, so they carry different codes and the second carries the reason.
        use crate::rca::orchestrator::RcaError;
        let resp = rca_error_response(&RcaError::Misconfigured(
            "an Anthropic API key is required".to_owned(),
        ));
        let json = body_json(resp).await;
        assert_eq!(json["error"]["code"], "rca_misconfigured");
        assert!(json["error"]["message"]
            .as_str()
            .unwrap()
            .contains("API key"));
    }
}
