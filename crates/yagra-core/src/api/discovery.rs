// SPDX-License-Identifier: AGPL-3.0-only
//! Device discovery — sweep a set of addresses, review what answered, import the ones you want.
//!
//! `ManageConfig`, except the candidates view: a sweep sends SNMP at addresses on the operator's
//! network with credentials they nominate, which is a configuration act, not a read.
//!
//! **Credentials are resolved server-side, by id** (ADR-018/020). The request names stored
//! credentials; this module opens them, converts them into the sweep job's inline form, and hands
//! them to the bus. So the only place plaintext exists is in memory here and in the job — and every
//! error this module can return about a credential names an **id and a static reason, never any
//! secret content** (security.md).
//!
//! The sweep is bounded at [`MAX_SCAN_TARGETS`] because it is the one endpoint that turns a single
//! request into a large amount of outbound traffic.
//!
//! ## Two ways a device gets found, and why they meet only at import
//!
//! A **scan** is something an operator starts: it holds its state in memory, ends, and produces
//! candidates. **Endpoint discovery** (ADR-043 Increment 3) is passive and continuous: routers are
//! walked for their ARP/ND caches, and anything answering that Yagra does not monitor accumulates in
//! `l3_discovered`. They answer the same operator question from opposite directions — "what is out
//! there" — so they share the *import* path and nothing else. A discovered endpoint imports by
//! building the same node-creation request a scan candidate does, which is what keeps the
//! classification pipeline (`yagra_discovery::identify` → `classification.rs`) unchanged and stops
//! there being two ways to create a node.

use super::error::{ApiError, ApiResult};
use super::extract::{Admin, Leader, RequireManageConfig, RequireView, Scoped};
use super::ApiState;
use crate::secrets::CredentialStore;
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::net::IpAddr;
use std::time::Instant;
use uuid::Uuid;

/// Most targets a single scan may sweep. The cap is what keeps one request from becoming an
/// unbounded outbound scan of someone else's network.
const MAX_SCAN_TARGETS: usize = 1024;

/// Default page size for the discovered-endpoint list.
const ENDPOINT_DEFAULT_LIMIT: i64 = 100;
/// Hard cap on the page size — an unbounded page is a DoS vector (api-conventions).
const ENDPOINT_MAX_LIMIT: i64 = 500;

/// This domain's slice of the OpenAPI document (ADR-035), merged by [`super::openapi::document`].
#[derive(utoipa::OpenApi)]
#[openapi(paths(
    start_discovery_scan,
    get_discovery_scan,
    list_discovery_scans,
    cancel_discovery_scan,
    import_discovered,
    discovery_candidates,
    list_discovered_endpoints,
    import_discovered_endpoint
))]
pub(super) struct Doc;

/// Default number of scans the list returns.
const SCANS_DEFAULT_LIMIT: usize = 20;
/// Hard cap on the scan list. Above `DiscoveryRunner`'s own retention cap it would be meaningless,
/// but a clamp is still the rule for every list (api-conventions).
const SCANS_MAX_LIMIT: usize = 50;

/// The discovery routes, merged into `/api/v1` by [`super::router`].
pub(super) fn routes() -> Router<ApiState> {
    Router::new()
        .route("/api/v1/discovery/scan", post(start_discovery_scan))
        .route("/api/v1/discovery/scan/:id", get(get_discovery_scan))
        .route("/api/v1/discovery/scans", get(list_discovery_scans))
        .route(
            "/api/v1/discovery/scan/:id/cancel",
            post(cancel_discovery_scan),
        )
        .route("/api/v1/discovery/import", post(import_discovered))
        .route("/api/v1/discovery/candidates", get(discovery_candidates))
        .route(
            "/api/v1/discovered-endpoints",
            get(list_discovered_endpoints),
        )
        .route(
            "/api/v1/discovered-endpoints/:id/import",
            post(import_discovered_endpoint),
        )
}

/// Start-scan body: explicit target IPs (the WebUI expands a CIDR), candidate stored credentials by
/// id, and ad-hoc communities.
#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct StartScan {
    targets: Vec<String>,
    #[serde(default)]
    communities: Vec<String>,
    #[serde(default)]
    credential_ids: Vec<String>,
    /// Poll-pool to run the sweep in (ADR-009/020). Absent/empty = legacy global discovery.
    #[serde(default)]
    pool: Option<String>,
    /// Try SNMP on addresses that do not answer ICMP.
    ///
    /// Absent means **no**. Earlier releases had no such option and tried every address in the
    /// range with every candidate credential, which is why sweeping a /24 took minutes; set this to
    /// get that behaviour back, and with it a device that filters ICMP but answers SNMP.
    // ADR-068 Increment 3. ⚠️ The `///` above is published verbatim to API clients through the
    // OpenAPI document, so it names neither the ADR nor a version that does not exist yet.
    #[serde(default)]
    snmp_when_unreachable: bool,
}

/// The accepted scan's id, for polling its status.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct StartedScan {
    scan_id: Uuid,
}

/// How many nodes an import created.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct ImportResult {
    created: u32,
}

/// Resolve stored credential ids into the inline candidates the sweep job carries.
///
/// Every error names an id and a static reason. None of them can carry secret content — that is a
/// deliberate property of this function, not an accident of the current messages.
async fn resolve_scan_credentials(
    creds: &CredentialStore,
    ids: &[String],
) -> Result<Vec<yagra_bus::DiscoveryCredential>, ApiError> {
    let mut out = Vec::with_capacity(ids.len());
    for raw in ids {
        let Ok(id) = raw.parse::<Uuid>() else {
            return Err(ApiError::bad_request(
                "invalid_credential",
                format!("'{raw}' is not a valid credential id"),
            ));
        };
        let opened = creds.open(id).await.map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "open scan credential",
                "failed to resolve a scan credential",
            )
        })?;
        let Some((kind, secret)) = opened else {
            return Err(ApiError::bad_request(
                "credential_not_found",
                format!("no credential {id}"),
            ));
        };
        if kind == crate::secrets::KIND_SNMP_V3 {
            match crate::secrets::SnmpV3Secret::parse(&secret) {
                // `DiscoveryV3` is `SnmpV3Auth` since ADR-084, so the six-field copy that used to
                // sit here is `SnmpV3Secret::auth()` — the one crossing from the stored shape to
                // the wire shape, shared with the eight scheduler builders.
                Ok(v3) => out.push(yagra_bus::DiscoveryCredential {
                    cred_ref: id,
                    community: None,
                    v3: Some(v3.auth()),
                }),
                Err(reason) => {
                    return Err(ApiError::bad_request(
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
                    return Err(ApiError::bad_request(
                        "invalid_credential",
                        format!("credential {id} is not usable as an SNMP community"),
                    ))
                }
            }
        }
    }
    Ok(out)
}

#[utoipa::path(
    post, path = "/api/v1/discovery/scan", tag = "discovery",
    request_body = StartScan,
    responses(
        (status = 202, description = "Sweep accepted; poll its status by id", body = StartedScan),
        (status = 400, description = "No targets or more than the cap, an unparseable address, or a named credential that is missing or unusable", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode has no write side, or this core is not the HA leader", body = super::error::ErrorBody),
    ),
)]
async fn start_discovery_scan(
    _guard: RequireManageConfig,
    admin: Admin,
    // ⚠️ Leader-gated, and the reason is not efficiency. Only the leader runs the discovery result
    // consumer (`main.rs`'s `LeaderWork`), so a standby accepting this would publish a job — real
    // ICMP and SNMP at the operator's network — whose every result lands on a core that has no
    // record of the scan and drops it. The sweep would happen and be invisible.
    _leader: Leader,
    Json(body): Json<StartScan>,
) -> ApiResult<(StatusCode, Json<StartedScan>)> {
    if body.targets.is_empty() || body.targets.len() > MAX_SCAN_TARGETS {
        return Err(ApiError::bad_request(
            "invalid_scan",
            format!("targets must be 1..={MAX_SCAN_TARGETS} addresses"),
        ));
    }
    // Parsed into `IpAddr` here, so nothing that is not an address reaches the sweep.
    let mut targets = Vec::with_capacity(body.targets.len());
    for t in &body.targets {
        let ip = t.parse::<IpAddr>().map_err(|_| {
            ApiError::bad_request(
                "invalid_address",
                format!("'{t}' is not a valid IP address"),
            )
        })?;
        targets.push(ip);
    }
    let credentials = resolve_scan_credentials(&admin.creds, &body.credential_ids).await?;
    // Route to a pool's own discovery subject only when that pool actually has a live poller;
    // otherwise fall back to the legacy global subject. That fallback is both the N/N-1 compat path
    // (an old wildcard poller still absorbs the sweep) and the guard against a typo'd pool name
    // black-holing the scan.
    let requested_pool = body
        .pool
        .as_deref()
        .map(str::trim)
        .filter(|p| !p.is_empty());
    let pool_route = match requested_pool {
        Some(p) if admin.coordinator.live_pools(Instant::now()).contains(p) => Some(p),
        _ => None,
    };
    // Note the two defaults point opposite ways on purpose, and each is right for its own question.
    // Here, "the caller did not say" is a fresh request that should take the fast path. On the bus
    // (`DiscoveryJob::snmp_when_unreachable`) an absent field means the job came from a core that
    // predates this option, and that core meant "probe everything".
    let silent = if body.snmp_when_unreachable {
        crate::discovery::SilentTargets::ProbeSnmp
    } else {
        crate::discovery::SilentTargets::Skip
    };
    let scan_id = admin
        .discovery
        .start(targets, body.communities, credentials, pool_route, silent)
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "start discovery scan",
                "failed to start discovery scan",
            )
        })?;
    Ok((StatusCode::ACCEPTED, Json(StartedScan { scan_id })))
}

#[utoipa::path(
    get, path = "/api/v1/discovery/scan/{id}", tag = "discovery",
    params(("id" = Uuid, Path, description = "Scan id returned when the sweep was accepted")),
    responses(
        (status = 200, description = "Progress and the candidates found so far", body = crate::discovery::ScanStatus),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
        (status = 404, description = "No such scan", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode has no write side", body = super::error::ErrorBody),
    ),
)]
async fn get_discovery_scan(
    _guard: RequireManageConfig,
    admin: Admin,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<crate::discovery::ScanStatus>> {
    admin
        .discovery
        .get(id)
        .map(Json)
        .ok_or_else(|| ApiError::not_found("scan_not_found", format!("no scan {id}")))
}

/// Query for the scan list.
#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub(super) struct ScansQuery {
    limit: Option<usize>,
}

/// The scans this core is holding, newest first.
///
/// Exists so a sweep survives leaving the page: the scan id used to live only in the browser tab
/// that started it, so navigating away lost a sweep the poller was still running. What bounds this
/// list is the runner's retention (finished scans age out, running ones are never capped away),
/// not the caller's `limit`.
///
/// **A restarted core answers an empty list even while a poller is still sweeping** — scan state is
/// in memory by decision (ADR-068). That is why the WebUI must render "this core does not know that
/// scan" for a 404 on a remembered id, rather than an empty page.
///
/// `ManageConfig` and `503` in skeleton mode, matching `GET /discovery/scan/{id}` rather than the
/// candidates queue: this list is the Discovery screen's own state, and a screen that cannot scan
/// has no scans to list. (The candidates queue answers `200 []` instead because it backs a
/// dashboard widget, where an error would break a page that otherwise works.)
#[utoipa::path(
    get, path = "/api/v1/discovery/scans", tag = "discovery",
    params(ScansQuery),
    responses(
        (status = 200, description = "Retained scans, newest first", body = Vec<crate::discovery::ScanSummary>),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode has no discovery runner", body = super::error::ErrorBody),
    ),
)]
async fn list_discovery_scans(
    _guard: RequireManageConfig,
    admin: Admin,
    Query(q): Query<ScansQuery>,
) -> ApiResult<Json<Vec<crate::discovery::ScanSummary>>> {
    Ok(Json(
        admin.discovery.list(
            q.limit
                .unwrap_or(SCANS_DEFAULT_LIMIT)
                .clamp(1, SCANS_MAX_LIMIT),
        ),
    ))
}

/// The outcome of asking a sweep to stop.
///
/// ⚠️ **Deliberately does not claim the sweep stopped**, and the field names carry that. Core
/// broadcasts the stop and cannot know who — or whether anyone — acted on it, so the honest report
/// is "requested, and here is whether the pollers that might be running it understand the command".
/// The confirmation arrives later, as the scan's own state going to `cancelled`.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct CancelRequested {
    /// The stop was published. Always true on a 200 — a publish failure is a 500.
    requested: bool,
    /// Whether every live poller that could be running this sweep advertises cancellation support.
    ///
    /// ⚠️ An **approximation**, which is why it is not called `will_stop`. A sweep on the global
    /// route could be held by any live poller, so this is the answer across all of them; even for a
    /// pool-scoped sweep it says the pool can stop sweeps, not that the poller holding this one
    /// will. `false` means at least one poller predates the feature and the sweep may run to
    /// completion.
    poller_supports_cancel: bool,
    /// The pool the stop was published to; `null` for the global subject.
    pool: Option<String>,
}

/// Ask the poller running a sweep to stop (ADR-068 Increment 2).
///
/// `ManageConfig`, the same as starting one: stopping a sweep is the same authority as causing it.
///
/// **Answers 200 for a scan this core has no record of, unlike `analysis`'s cancel.** That
/// asymmetry is deliberate. Scan state is in memory, so a restarted core forgets sweeps its pollers
/// are still running, and requiring a local record would make exactly those sweeps — the ones an
/// operator most wants to stop — unstoppable. The `analysis` endpoint 404s because there the
/// 200/404 split answers "is that job running" for any id a caller cares to try; here the id is an
/// unguessable UUID and the caller already holds `ManageConfig`, so the split would only reveal
/// whether this process remembers something.
#[utoipa::path(
    post, path = "/api/v1/discovery/scan/{id}/cancel", tag = "discovery",
    params(("id" = Uuid, Path, description = "Scan id returned when the sweep was accepted")),
    responses(
        (status = 200, description = "The stop was published. Not a promise that the sweep stopped — watch the scan's state for that", body = CancelRequested),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode has no write side, or this core is not the HA leader", body = super::error::ErrorBody),
    ),
)]
async fn cancel_discovery_scan(
    _guard: RequireManageConfig,
    admin: Admin,
    // Leader-gated for the same reason the start is: only the leader consumes discovery results, so
    // only it holds the scan record whose route the stop must follow.
    _leader: Leader,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<CancelRequested>> {
    let pool = admin.discovery.cancel(id).await.map_err(|e| {
        ApiError::from_internal(
            e.as_ref(),
            "cancel discovery scan",
            "failed to request the sweep stop",
        )
    })?;
    Ok(Json(CancelRequested {
        requested: true,
        poller_supports_cancel: admin.coordinator.pollers_support(
            pool.as_deref(),
            yagra_bus::CAP_DISCOVERY_CANCEL,
            Instant::now(),
        ),
        pool,
    }))
}

/// One discovered device the operator chose to add.
#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct ImportNode {
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
#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct ImportDiscovered {
    nodes: Vec<ImportNode>,
}

#[utoipa::path(
    post, path = "/api/v1/discovery/import", tag = "discovery",
    request_body = ImportDiscovered,
    responses(
        (status = 201, description = "Nodes created, in one transaction", body = ImportResult),
        (status = 400, description = "An unparseable address, an empty name, or a binding id that is not a UUID", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode has no write side", body = super::error::ErrorBody),
    ),
)]
async fn import_discovered(
    _guard: RequireManageConfig,
    admin: Admin,
    Json(body): Json<ImportDiscovered>,
) -> ApiResult<(StatusCode, Json<ImportResult>)> {
    let parse_uuid = |s: &Option<String>| -> Result<Option<Uuid>, ()> {
        match s {
            None => Ok(None),
            Some(v) => v.parse::<Uuid>().map(Some).map_err(|_| ()),
        }
    };
    // Every node is validated up front and the batch is then inserted in one transaction, so a
    // failure partway cannot leave half an import behind (NodeRepo::import_nodes).
    let mut prepared: Vec<crate::repo::NewNode<'_>> = Vec::with_capacity(body.nodes.len());
    for n in &body.nodes {
        let Ok(addr) = n.address.parse::<IpAddr>() else {
            return Err(ApiError::bad_request(
                "invalid_address",
                format!("'{}' is not a valid IP address", n.address),
            ));
        };
        let name = n.name.trim();
        if name.is_empty() {
            return Err(ApiError::bad_request(
                "invalid_node",
                "name must not be empty",
            ));
        }
        let (Ok(profile), Ok(credential)) =
            (parse_uuid(&n.profile_id), parse_uuid(&n.credential_id))
        else {
            return Err(ApiError::bad_request(
                "invalid_binding",
                "profile_id/credential_id must be UUIDs",
            ));
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
    let created = admin.repo.import_nodes(&prepared).await.map_err(|e| {
        ApiError::from_internal(
            e.as_ref(),
            "import discovered nodes",
            "failed to import discovered nodes",
        )
    })?;
    Ok((StatusCode::CREATED, Json(ImportResult { created })))
}

/// Query for the standing discovery-candidates view.
#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub(super) struct CandidatesQuery {
    limit: Option<usize>,
}

/// Recent discovered (unclassified) devices across in-memory scans — the dashboard "discovery
/// queue".
///
/// `View`, unlike the rest of this module, because it reports what has been seen rather than
/// causing anything to happen. Empty in skeleton mode: with no discovery runner there are genuinely
/// no candidates, which is an answer rather than an outage.
#[utoipa::path(
    get, path = "/api/v1/discovery/candidates", tag = "discovery",
    params(CandidatesQuery),
    responses(
        (status = 200, description = "Recent unclassified devices; empty in skeleton mode", body = Vec<crate::discovery::Candidate>),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks read permission", body = super::error::ErrorBody),
    ),
)]
async fn discovery_candidates(
    _guard: RequireView,
    State(st): State<ApiState>,
    Query(q): Query<CandidatesQuery>,
) -> ApiResult<Json<Vec<crate::discovery::Candidate>>> {
    Ok(Json(recent_candidates(&st, q.limit)))
}

/// Recent unclassified devices, capped — the seam both edges call.
///
/// Skeleton mode returns empty rather than unavailable: with no discovery runner there are
/// genuinely no candidates, which is an answer.
pub(crate) fn recent_candidates(
    st: &ApiState,
    limit: Option<usize>,
) -> Vec<crate::discovery::Candidate> {
    let Some(admin) = st.admin.as_ref() else {
        return Vec::new();
    };
    admin
        .discovery
        .recent_candidates(limit.unwrap_or(10).clamp(1, 50))
}

// ── Endpoints seen on the network (ADR-043 Increment 3) ─────────────────────

/// One address the fleet has resolved on the wire but does not monitor.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct DiscoveredEndpointRow {
    pub id: Uuid,
    /// The endpoint's address.
    pub ip: String,
    /// Its hardware address, lowercase colon-separated hex; `null` for an incomplete ARP entry.
    pub mac: Option<String>,
    /// Which monitored node resolved it; `null` once that node has been deleted.
    pub via_node: Option<Uuid>,
    /// The SNMP ifIndex it was resolved on — the port it is behind.
    pub via_ifindex: Option<u32>,
    /// When it was first seen anywhere in the fleet (RFC 3339).
    pub first_seen: String,
    /// When it was last confirmed still present (RFC 3339).
    pub last_seen: String,
    /// The node this address became, once it is monitored; `null` while it is still unmonitored.
    pub promoted_node_id: Option<Uuid>,
}

/// Keyset cursor for the next page.
//
// Keyset, never OFFSET (ADR-019): the sweep updates `last_seen` under the reader, so an offset page
// would skip and repeat rows. The reason lives in `//` — the `///` above is published verbatim to
// API clients, and an internal ADR number means nothing to them.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct DiscoveredEndpointCursor {
    pub last_seen: String,
    pub id: Uuid,
}

/// How much of the fleet's ARP data this list was built from.
//
// The point of this block is `truncated_nodes`. A router whose ARP walk hit its row budget
// contributes a *sample*, so the list below is a sample too — and a sample presented as a complete
// answer is exactly the quiet wrongness the caps elsewhere in this codebase are written to declare.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct DiscoveredEndpointSummary {
    /// Total endpoints observed across the fleet, before dedup and before the unmonitored filter.
    pub observed_total: i64,
    /// How many nodes have reported an ARP/ND cache at all.
    pub nodes_reporting: i64,
    /// How many of those hit a cap, making their contribution a sample rather than a total.
    pub truncated_nodes: i64,
}

/// One page of discovered endpoints, most recently seen first.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct DiscoveredEndpointPage {
    pub endpoints: Vec<DiscoveredEndpointRow>,
    /// Pass back as `before_last_seen`+`before_id`; `null` ⇒ this was the last page.
    pub next: Option<DiscoveredEndpointCursor>,
    pub summary: DiscoveredEndpointSummary,
}

#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub(super) struct EndpointsQuery {
    #[serde(default)]
    limit: Option<i64>,
    /// Only endpoints seen by this node.
    #[serde(default)]
    via_node: Option<Uuid>,
    /// Include endpoints that have since become monitored nodes. Default `false`.
    #[serde(default)]
    include_promoted: Option<bool>,
    #[serde(default)]
    before_last_seen: Option<String>,
    #[serde(default)]
    before_id: Option<Uuid>,
}

/// Addresses seen on the network that Yagra does not monitor.
///
/// Built from the ARP / IPv6-neighbour caches of the nodes that *are* monitored, so an endpoint
/// appears here only if some monitored router has spoken to it. Requires the ARP walk to be enabled
/// (Settings ▸ System settings ▸ Discovery walks); with it off the list is empty, which is an answer
/// rather than an outage.
///
/// `summary.truncated_nodes > 0` means at least one router's cache exceeded its row budget and this
/// list is a **sample**, not a complete inventory of the segment.
#[utoipa::path(
    get, path = "/api/v1/discovered-endpoints", tag = "discovery",
    params(EndpointsQuery),
    responses(
        (status = 200, description = "One page of unmonitored endpoints, most recently seen first", body = DiscoveredEndpointPage),
        (status = 400, description = "before_last_seen and before_id must be given together, and before_last_seen must be RFC 3339", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks View", body = super::error::ErrorBody),
        (status = 503, description = "Inventory storage is unavailable (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn list_discovered_endpoints(
    _guard: RequireView,
    Scoped(scope): Scoped,
    admin: Admin,
    Query(q): Query<EndpointsQuery>,
) -> ApiResult<Json<DiscoveredEndpointPage>> {
    Ok(Json(
        discovered_endpoint_page(
            &admin,
            &scope,
            q.via_node,
            q.include_promoted.unwrap_or(false),
            endpoint_cursor(q.before_last_seen.as_deref(), q.before_id)?,
            q.limit,
        )
        .await?,
    ))
}

/// Parse a keyset cursor: both halves or neither.
///
/// A half-specified cursor is **rejected rather than ignored**, for the reason
/// `neighbors::parse_history_cursor` spells out: silently dropping it restarts paging from the top,
/// so a client walking the list loops over page one forever while looking like it is progressing.
pub(crate) fn endpoint_cursor(
    before_last_seen: Option<&str>,
    before_id: Option<Uuid>,
) -> Result<Option<(chrono::DateTime<chrono::Utc>, Uuid)>, ApiError> {
    match (before_last_seen, before_id) {
        (Some(at), Some(id)) => {
            let ts = super::parse_rfc3339(at).ok_or_else(|| {
                ApiError::bad_request("invalid_cursor", "before_last_seen must be RFC 3339")
            })?;
            Ok(Some((ts, id)))
        }
        (None, None) => Ok(None),
        _ => Err(ApiError::bad_request(
            "invalid_cursor",
            "before_last_seen and before_id must be given together",
        )),
    }
}

/// One page of discovered endpoints — the seam REST and MCP share (api-conventions).
///
/// ⚠️ **Scoping is security-critical here and it is not the usual node filter.** The rows carry an
/// address, not a node id, so what bounds them is the *observing* node's group. An endpoint seen
/// only by a node outside the caller's scope must not be listed: the address itself would disclose a
/// segment they cannot see. The predicate lives in the SQL (`DiscoveredRepo::list_page`) so both
/// surfaces get it from the same statement rather than each remembering to apply it.
pub(crate) async fn discovered_endpoint_page(
    admin: &super::AdminState,
    scope: &super::scope::NodeScope,
    via_node: Option<Uuid>,
    include_promoted: bool,
    before: Option<(chrono::DateTime<chrono::Utc>, Uuid)>,
    limit: Option<i64>,
) -> ApiResult<DiscoveredEndpointPage> {
    let limit = limit
        .unwrap_or(ENDPOINT_DEFAULT_LIMIT)
        .clamp(1, ENDPOINT_MAX_LIMIT);
    let rows = admin
        .discovered
        .list_page(
            scope.group_filter(),
            via_node,
            include_promoted,
            before,
            limit,
        )
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "list discovered endpoints",
                "failed to list discovered endpoints",
            )
        })?;
    // A cursor only when the page came back full — a short page is the end of the list, and handing
    // one back there makes a client fetch an empty page to discover that.
    let next = rows
        .last()
        .filter(|_| i64::try_from(rows.len()).unwrap_or(0) == limit)
        .map(|r| DiscoveredEndpointCursor {
            last_seen: r.last_seen.to_rfc3339(),
            id: r.id,
        });
    let (observed_total, nodes_reporting, truncated_nodes) =
        admin.arp.totals().await.unwrap_or((0, 0, 0));
    Ok(DiscoveredEndpointPage {
        endpoints: rows
            .into_iter()
            .map(|r| DiscoveredEndpointRow {
                id: r.id,
                ip: r.ip.to_string(),
                mac: r.mac,
                via_node: r.via_node.map(|n| n.as_uuid()),
                via_ifindex: r.via_ifindex,
                first_seen: r.first_seen.to_rfc3339(),
                last_seen: r.last_seen.to_rfc3339(),
                promoted_node_id: r.promoted_node_id.map(|n| n.as_uuid()),
            })
            .collect(),
        next,
        summary: DiscoveredEndpointSummary {
            observed_total,
            nodes_reporting,
            truncated_nodes,
        },
    })
}

/// What to call the endpoint, and what to bind it to, when promoting it to a node.
#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct ImportEndpoint {
    /// Node name. Defaults to the address when omitted or blank.
    #[serde(default)]
    name: Option<String>,
    profile_id: Option<String>,
    credential_id: Option<String>,
}

/// Promote a discovered endpoint to a monitored node.
///
/// Builds the same `NewNode` a scan import does and goes through the same writer, so classification
/// (`sysDescr` → maker/model) happens on the node's first identity probe exactly as it does for any
/// other node — there is no second creation path to keep in step.
///
/// `409` means the address is already an inventory node. That is not a failure of the import so much
/// as an answer: the endpoint stopped being unmonitored between the list being read and the button
/// being pressed, and the row is reconciled on the way out so the list stops showing it.
#[utoipa::path(
    post, path = "/api/v1/discovered-endpoints/{id}/import", tag = "discovery",
    params(("id" = Uuid, Path, description = "Discovered-endpoint id")),
    request_body = ImportEndpoint,
    responses(
        (status = 201, description = "The endpoint is now a monitored node", body = ImportResult),
        (status = 400, description = "A binding id that is not a UUID", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
        (status = 404, description = "No such discovered endpoint", body = super::error::ErrorBody),
        (status = 409, description = "That address is already a monitored node", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode has no write side", body = super::error::ErrorBody),
    ),
)]
async fn import_discovered_endpoint(
    _guard: RequireManageConfig,
    admin: Admin,
    Path(id): Path<Uuid>,
    Json(body): Json<ImportEndpoint>,
) -> ApiResult<(StatusCode, Json<ImportResult>)> {
    let endpoint = admin
        .discovered
        .get(id)
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "read discovered endpoint",
                "failed to read the discovered endpoint",
            )
        })?
        .ok_or_else(|| {
            ApiError::not_found("endpoint_not_found", format!("no discovered endpoint {id}"))
        })?;
    if endpoint.promoted_node_id.is_some() {
        return Err(ApiError::conflict(
            "already_monitored",
            format!("{} is already a monitored node", endpoint.ip),
        ));
    }
    let parse_uuid = |s: &Option<String>| -> Result<Option<Uuid>, ()> {
        match s {
            None => Ok(None),
            Some(v) => v.parse::<Uuid>().map(Some).map_err(|_| ()),
        }
    };
    let (Ok(profile), Ok(credential)) = (
        parse_uuid(&body.profile_id),
        parse_uuid(&body.credential_id),
    ) else {
        return Err(ApiError::bad_request(
            "invalid_binding",
            "profile_id/credential_id must be UUIDs",
        ));
    };
    let address = endpoint.ip.to_string();
    let name = body
        .name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(address.as_str());
    let created = admin
        .repo
        .import_nodes(&[crate::repo::NewNode {
            name,
            address: endpoint.ip,
            profile,
            credential,
            // Deliberately blank. A MAC's OUI names the *chassis* vendor, which for a monitored
            // device is routinely not the vendor whose MIBs it answers — a whitebox switch, a VM's
            // virtual NIC. `sysDescr` classification fills these correctly on the first poll, and a
            // wrong pre-fill would outlive the guess by being an operator-set value.
            vendor: None,
            model: None,
        }])
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "import discovered endpoint",
                "failed to import the discovered endpoint",
            )
        })?;
    // Runs the same reconcile the sweep does rather than stamping the column here — one rule, one
    // place. Best-effort: the sweep repeats it, so a failure costs a stale row for one cycle.
    if let Err(e) = admin.discovered.reconcile_promotions().await {
        tracing::warn!(error = %e, "reconciling the promoted endpoint failed");
    }
    Ok((StatusCode::CREATED, Json(ImportResult { created })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::router;
    use crate::api::tests_support::{private_state, public_state};
    use axum::body::{to_bytes, Body};
    use axum::http::{header::AUTHORIZATION, Request};
    use tower::ServiceExt;
    use yagra_common::{Principal, Role, Scope};

    const ID: &str = "00000000-0000-0000-0000-000000000001";

    fn config_routes() -> Vec<(&'static str, String)> {
        vec![
            ("POST", "/api/v1/discovery/scan".to_owned()),
            ("GET", format!("/api/v1/discovery/scan/{ID}")),
            // The scan list is the Discovery screen's own state, so it is gated exactly as the
            // single-scan read is — and answers 503 in skeleton mode rather than an empty 200,
            // which would read as "no sweeps have run" on a deployment that cannot sweep at all.
            ("GET", "/api/v1/discovery/scans".to_owned()),
            // Stopping a sweep is the same authority as causing one (ADR-068 Inc.2).
            ("POST", format!("/api/v1/discovery/scan/{ID}/cancel")),
            ("POST", "/api/v1/discovery/import".to_owned()),
        ]
    }

    async fn status_of(st: ApiState, method: &str, path: &str, token: Option<&str>) -> StatusCode {
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
            .status()
    }

    #[tokio::test]
    async fn sweeping_and_importing_are_an_operators_to_do_and_closed_to_a_viewer() {
        // A sweep sends traffic at the operator's network using credentials it names — which is
        // exactly an operator's job, so ADR-057 left it on `ManageConfig` and that moved down to
        // them. A viewer triggers nothing.
        for (method, path) in config_routes() {
            assert_eq!(
                status_of(private_state(), method, &path, None).await,
                StatusCode::UNAUTHORIZED,
                "anon {method} {path}"
            );
            assert_eq!(
                status_of(public_state(), method, &path, None).await,
                StatusCode::UNAUTHORIZED,
                "public {method} {path}"
            );
        }
        let st = private_state();
        for (role, want) in [
            (Role::Viewer, StatusCode::FORBIDDEN),
            (Role::Operator, StatusCode::SERVICE_UNAVAILABLE),
        ] {
            let token = st
                .sessions
                .issue(Uuid::new_v4(), Principal::new(role, Scope::All), "u");
            for (method, path) in config_routes() {
                assert_eq!(
                    status_of(st.clone(), method, &path, Some(&token)).await,
                    want,
                    "{role:?} {method} {path}"
                );
            }
        }
    }

    #[tokio::test]
    async fn the_candidates_queue_reads_openly_and_is_empty_without_a_runner() {
        let resp = router(public_state())
            .oneshot(
                Request::builder()
                    .uri("/api/v1/discovery/candidates")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&bytes).unwrap(),
            serde_json::json!([])
        );
    }

    // ── Endpoints seen on the network (ADR-043 Increment 3) ─────────────────

    #[tokio::test]
    async fn the_endpoint_list_authenticates_before_reporting_anything_about_the_deployment() {
        // 401, never a 503 that reveals whether this deployment has inventory storage at all.
        assert_eq!(
            status_of(private_state(), "GET", "/api/v1/discovered-endpoints", None).await,
            StatusCode::UNAUTHORIZED
        );
        // Skeleton mode answers 503 to an authenticated caller — the guard ran first.
        let st = private_state();
        let token = st.sessions.issue(
            Uuid::new_v4(),
            Principal::new(Role::Viewer, Scope::All),
            "u",
        );
        assert_eq!(
            status_of(st, "GET", "/api/v1/discovered-endpoints", Some(&token)).await,
            StatusCode::SERVICE_UNAVAILABLE
        );
    }

    #[tokio::test]
    async fn promoting_an_endpoint_is_closed_to_a_viewer() {
        // The import creates a node, so it is `ManageConfig` — the same gate the scan import has,
        // reached from the other of the two discovery paths.
        let path = format!("/api/v1/discovered-endpoints/{ID}/import");
        assert_eq!(
            status_of(private_state(), "POST", &path, None).await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            status_of(public_state(), "POST", &path, None).await,
            StatusCode::UNAUTHORIZED,
            "a write guard stays closed on a public-dashboard deployment"
        );
        let st = private_state();
        for (role, want) in [
            (Role::Viewer, StatusCode::FORBIDDEN),
            (Role::Operator, StatusCode::SERVICE_UNAVAILABLE),
        ] {
            let token = st
                .sessions
                .issue(Uuid::new_v4(), Principal::new(role, Scope::All), "u");
            assert_eq!(
                status_of(st.clone(), "POST", &path, Some(&token)).await,
                want,
                "{role:?} promoting an endpoint to a monitored node"
            );
        }
    }

    #[test]
    fn a_half_specified_cursor_is_refused_rather_than_ignored() {
        // Ignoring it restarts paging from the top, so a client walking the list loops over page
        // one forever while looking like it is making progress.
        assert!(endpoint_cursor(Some("2026-08-04T00:00:00Z"), None).is_err());
        assert!(endpoint_cursor(None, Some(Uuid::nil())).is_err());
        assert!(endpoint_cursor(None, None).unwrap().is_none());
        assert!(endpoint_cursor(Some("not-a-time"), Some(Uuid::nil())).is_err());
        assert!(
            endpoint_cursor(Some("2026-08-04T00:00:00Z"), Some(Uuid::nil()))
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn the_page_size_is_clamped_at_both_ends() {
        // An unbounded page is a DoS vector; a zero-length one is an infinite paging loop.
        for (asked, want) in [
            (None, ENDPOINT_DEFAULT_LIMIT),
            (Some(0), 1),
            (Some(-5), 1),
            (Some(10), 10),
            (Some(100_000), ENDPOINT_MAX_LIMIT),
        ] {
            let got = asked
                .unwrap_or(ENDPOINT_DEFAULT_LIMIT)
                .clamp(1, ENDPOINT_MAX_LIMIT);
            assert_eq!(got, want, "limit={asked:?}");
        }
    }

    /// A full sweep's cumulative result has to fit in one bus message, and nothing enforced that.
    ///
    /// The poller republishes the **whole** candidate list on every chunk — `DiscoveryResult::found`
    /// is cumulative by design, so that an older core reading one message as final still converges
    /// (ADR-017). The largest such message a single request can provoke is therefore bounded by
    /// [`MAX_SCAN_TARGETS`], here, and the ceiling it must clear is NATS's `max_payload`, which is
    /// 1 MiB unless the server config says otherwise (`docker/nats/nats-server.conf` now says so
    /// out loud rather than inheriting it).
    ///
    /// Exceeding it fails the way this feature has already failed twice: the publish is rejected,
    /// the poller logs one warning, no terminal result is ever sent, and the scan sits at
    /// `running` forever. So the budget is pinned rather than reasoned about — this fails the day
    /// someone raises the cap or adds a field to `DiscoveredDevice`.
    ///
    /// **The device below is measured, not imagined.** Sixteen real SNMP walks off the lab devices
    /// (2026-08-18) put the longest `sysDescr` at 384 bytes — a Huawei VRP NE8000-M8 — with a
    /// median near 150. The address is IPv6 because its text form is the longer one, and every
    /// optional field is populated because a real device fills them all in.
    ///
    /// ⚠️ **`sysDescr` is device-supplied and unbounded**, so "it fits today" is only half an
    /// answer. The second assertion is the other half: it bisects the real encoder for the sysDescr
    /// length at which the message stops fitting, which turns the remaining risk into a number
    /// rather than a shrug — and, because it bounds that number from *both* sides, it is also what
    /// stops the first assertion passing vacuously if serialization ever returned something tiny.
    #[test]
    fn a_full_sweep_of_the_widest_devices_still_fits_in_one_bus_message() {
        // NATS's default, restated here as well as in the server config. The config is loaded on
        // one deployment shape (the remote-poller one); this bound applies to all of them.
        const NATS_MAX_PAYLOAD: usize = 1024 * 1024;
        const OBSERVED_WORST_SYSDESCR: usize = 384;

        fn message_bytes(sysdescr_len: usize) -> usize {
            let found: Vec<yagra_bus::DiscoveredDevice> = (0..MAX_SCAN_TARGETS)
                .map(|i| yagra_bus::DiscoveredDevice {
                    address: std::net::IpAddr::V6(std::net::Ipv6Addr::new(
                        0x2001,
                        0x0db8,
                        0xdead,
                        0xbeef,
                        0xffff,
                        0xffff,
                        0xffff,
                        u16::try_from(i).unwrap_or(u16::MAX),
                    )),
                    reachable: true,
                    sysdescr: Some("W".repeat(sysdescr_len)),
                    sysname: Some("edge-router-with-a-long-hostname.example.net".to_owned()),
                    sysobjectid: Some("1.3.6.1.4.1.2011.2.240.121".to_owned()),
                    matched_credential: Some(Uuid::from_u128(1)),
                })
                .collect();
            let targets = u32::try_from(MAX_SCAN_TARGETS).unwrap_or(u32::MAX);
            serde_json::to_vec(&yagra_bus::DiscoveryResult {
                scan_id: Uuid::from_u128(7),
                found,
                probed: targets,
                total: targets,
                done: true,
                cancelled: false,
            })
            .expect("a discovery result serializes")
            .len()
        }

        let realistic = message_bytes(OBSERVED_WORST_SYSDESCR);
        assert!(
            realistic < NATS_MAX_PAYLOAD,
            "a full sweep of {MAX_SCAN_TARGETS} devices at the longest sysDescr seen on real              hardware serializes to {realistic} bytes, past the {NATS_MAX_PAYLOAD}-byte bus limit              -- such a sweep would end with no terminal message and sit at `running` forever"
        );

        // The margin as a number rather than an adjective: how long a sysDescr this budget can
        // absorb on *every* one of the targets before the message stops fitting. Bisected over the
        // real encoder (~13 calls), not interpolated from the one sample above.
        const CEILING: usize = 8 * 1024;
        let lengths: Vec<usize> = (0..=CEILING).collect();
        let break_even = lengths.partition_point(|&n| message_bytes(n) < NATS_MAX_PAYLOAD);
        assert!(
            (750..CEILING).contains(&break_even),
            "the budget absorbs a sysDescr of {break_even} bytes on every one of              {MAX_SCAN_TARGETS} devices before the message stops fitting. Below 750 the margin              over the {OBSERVED_WORST_SYSDESCR} bytes measured on real hardware is too thin to              call a margin -- if a field was just added to DiscoveredDevice, this is the number              that has to be re-decided rather than the floor that has to be lowered. At {CEILING}              nothing was found to fail at all, which would mean this test measures nothing."
        );
    }
}
