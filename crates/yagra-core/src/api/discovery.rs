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
use std::collections::{HashMap, HashSet};
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
    preview_discovery_import,
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
        .route(
            "/api/v1/discovery/import-preview",
            post(preview_discovery_import),
        )
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

/// How the batch was filed, when the request asked for filing by IP range (ADR-131 決定 2).
///
/// 🚨 **Three numbers, not two.** A device two folders claim equally well and one no range covers
/// both end up in the request's fallback folder — but they are different facts, and folding them
/// loses the actionable one: an ambiguous address means two folders have overlapping ranges
/// configured, which is a thing to go and fix. Reported separately so the operator reads
/// "3 fell back, 1 of them because two folders disagree" rather than "3 addresses are outside
/// every range", which would be untrue.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct PrefixFiling {
    /// Filed into the one folder whose range contains the address.
    matched: u32,
    /// Two or more folders claimed it at the same prefix length; filed into the fallback.
    ambiguous: u32,
    /// No folder's range contained it; filed into the fallback.
    unmatched: u32,
    /// The operator named this row's folder themselves, so no rule was applied to it
    /// (ADR-131 決定 11). Counted apart from the three above because it is not an outcome of the
    /// match — reporting it as `matched` would credit the rule with a choice a person made.
    chosen: u32,
}

/// How many nodes an import created, and — when filing by IP range was asked for — how.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct ImportResult {
    created: u32,
    /// Present only when the request set `file_by_prefix`. `null` otherwise, which is every
    /// endpoint promotion and every scan import with the option off.
    ///
    /// ⚠️ `skip_serializing_if` rather than a zero-filled struct, for two reasons. This type is
    /// shared with `import_discovered_endpoint`, where filing by range never happens and zeros
    /// would be a lie; and with the field absent the wire shape is byte-identical to what every
    /// existing client already parses. `created == matched + ambiguous + unmatched`.
    #[serde(skip_serializing_if = "Option::is_none")]
    filed: Option<PrefixFiling>,
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
    // consumer (`main.rs`'s `LeaderTasks`), so a standby accepting this would publish a job — real
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
    /// The folder this one device goes into, overriding both the IP-range rule and the request's
    /// `group_id` (ADR-131 決定 11).
    ///
    /// 🚨 **Three states, not two, and `Option<Uuid>` cannot carry them.** Absent means "follow the
    /// rule"; an id means that folder; **`null` means the operator chose the tree root**, which is
    /// a destination like any other. With a plain `Option<Uuid>` serde maps absent and `null` to
    /// the same `None`, so a device deliberately sent to the root would silently be filed by range
    /// instead — a control that lies about what it does. `deserialize_some` keeps them apart.
    ///
    /// ⚠️ **This is the per-row field ADR-100 決定 10 refused, and it is admitted under a
    /// condition.** That decision's objection was a UI in which fifty rows each carry an
    /// independent choice and the screen has to explain the result. Here a row's destination still
    /// comes from one rule by default, and this is an *override* of it — so the screen explains
    /// itself by saying which rows the operator changed, and a row nobody touched is still the
    /// rule's answer. Remove the default and the original objection applies again in full.
    #[serde(default, deserialize_with = "deserialize_some")]
    #[schema(value_type = Option<String>, nullable)]
    group_id: Option<Option<Uuid>>,
}

/// Deserialize into `Some`, so an explicit `null` survives as `Some(None)` while an absent field
/// stays `None`. The standard three-state idiom for a JSON field that can be unset, set, or
/// cleared; [`ImportNode::group_id`] says why this one needs it.
fn deserialize_some<'de, T, D>(d: D) -> Result<Option<T>, D::Error>
where
    T: serde::Deserialize<'de>,
    D: serde::Deserializer<'de>,
{
    T::deserialize(d).map(Some)
}

/// Import body: the selected devices to create as nodes.
#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct ImportDiscovered {
    nodes: Vec<ImportNode>,
    /// Inventory folder to file every imported node under (ADR-100 decision 10), or absent for
    /// the tree root — which is what every import did before this existed.
    ///
    /// ⚠️ **One folder for the whole request, not one per node.** A sweep is aimed at a site, so
    /// the folder is a property of the sweep; per-row would invite a UI that lets fifty rows
    /// disagree and then have to explain itself.
    ///
    /// When `file_by_prefix` is set this is the **fallback** rather than the destination — still
    /// one folder, still a property of the request.
    #[serde(default)]
    group_id: Option<Uuid>,
    /// File each device into the folder whose IP range contains its address, falling back to
    /// `group_id` for one no range covers — or that two folders claim equally well (ADR-131).
    ///
    /// ⚠️ **This does not reverse ADR-100 decision 10.** That decision refuses a *per-row folder
    /// field*, because fifty rows could then disagree and the screen would have to explain it.
    /// This is a rule for the whole request: no row carries a choice, every destination is derived
    /// by one rule from data the operator did not type, and the request still names exactly one
    /// operator-chosen folder. One request, one intent, one thing to explain.
    ///
    /// `#[serde(default)]` so an N-1 client's body means exactly what it meant before.
    #[serde(default)]
    file_by_prefix: bool,
}

#[utoipa::path(
    post, path = "/api/v1/discovery/import", tag = "discovery",
    request_body = ImportDiscovered,
    responses(
        (status = 201, description = "Nodes created, in one transaction", body = ImportResult),
        (status = 400, description = "An unparseable address, an empty name, a binding id that is not a UUID, or a group_id no folder has", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode has no write side", body = super::error::ErrorBody),
    ),
)]
async fn import_discovered(
    _guard: RequireManageConfig,
    Scoped(scope): Scoped,
    admin: Admin,
    Json(body): Json<ImportDiscovered>,
) -> ApiResult<(StatusCode, Json<ImportResult>)> {
    let parse_uuid = |s: &Option<String>| -> Result<Option<Uuid>, ()> {
        match s {
            None => Ok(None),
            Some(v) => v.parse::<Uuid>().map(Some).map_err(|_| ()),
        }
    };
    // The destination is checked before the rows: filing nodes *into* a folder this caller may not
    // act on would put them where that caller can no longer reach them — the same order and the
    // same reason as `nodes::move_nodes` (ADR-131 決定 8).
    if let Some(group) = body.group_id {
        super::scope::require_visible_group(&scope, group)?;
    }
    // Checked before anything is prepared: `nodes.group_id` is a foreign key, so an id that is
    // not there would abort the transaction and surface as a 500 that names nothing.
    super::groups::require_group_exists(&admin, body.group_id).await?;
    // Every per-row folder gets the same two checks, deduplicated so fifty rows aimed at one
    // folder cost one round trip rather than fifty. Doing it here, before any row is prepared,
    // keeps the guarantee the whole handler rests on: the insert is one transaction, so a folder
    // refused halfway would otherwise roll back an import the operator was told had started.
    let mut seen_rows: HashSet<Uuid> = HashSet::new();
    for n in &body.nodes {
        // `Some(None)` is the tree root, which needs no folder check — only a named folder does.
        let Some(Some(group)) = n.group_id else {
            continue;
        };
        if !seen_rows.insert(group) {
            continue;
        }
        super::scope::require_visible_group(&scope, group)?;
        super::groups::require_group_exists(&admin, Some(group)).await?;
    }
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
            // Precedence, narrowest first (ADR-131 決定 11): the operator's own choice for this
            // row, else the IP-range rule applied below, else the request's folder.
            group: match n.group_id {
                Some(choice) => choice,
                None => body.group_id,
            },
        });
    }

    // Filing by IP range (ADR-131). `NodeRepo::import_nodes` already binds `group_id` per row and
    // computes `sort_order` per destination folder inside its transaction, so a batch that lands in
    // several folders needs nothing from the writer — only a different value in each row.
    // The addresses the rule still has to decide: a row the operator named a folder for is already
    // settled, and asking the matcher about it would only invite the answer to overwrite the
    // choice. Collected before the rule runs so the two cannot disagree.
    let chosen: Vec<IpAddr> = body
        .nodes
        .iter()
        .zip(prepared.iter())
        .filter(|(n, _)| n.group_id.is_some())
        .map(|(_, row)| row.address)
        .collect();

    let filed = if body.file_by_prefix {
        let addrs: Vec<IpAddr> = prepared
            .iter()
            .filter(|n| !chosen.contains(&n.address))
            .map(|n| n.address)
            .collect();
        let hits = admin
            .groups
            .match_address_prefixes(&addrs, scope.group_filter())
            .await
            .map_err(|e| {
                ApiError::from_internal(
                    e.as_ref(),
                    "match discovered addresses",
                    "failed to match prefixes",
                )
            })?;
        let fold = crate::groups::fold_prefix_matches(&addrs, hits);
        let by_address: HashMap<IpAddr, Uuid> = fold
            .matched
            .iter()
            .map(|(addr, group, _)| (*addr, *group))
            .collect();
        for row in prepared.iter_mut() {
            // `chosen` rows keep what the operator gave them; nothing here can reach them, because
            // their addresses were never handed to the matcher.
            if let Some(group) = by_address.get(&row.address) {
                row.group = Some(*group);
            }
        }
        Some(PrefixFiling {
            matched: fold.matched.len() as u32,
            ambiguous: fold.ambiguous.len() as u32,
            unmatched: fold.unmatched.len() as u32,
            chosen: chosen.len() as u32,
        })
    } else if !chosen.is_empty() {
        // The rule is off but the operator still directed some rows. Reporting that is the honest
        // answer; `None` here would say "nothing was decided per row", which is untrue.
        Some(PrefixFiling {
            matched: 0,
            ambiguous: 0,
            unmatched: 0,
            chosen: chosen.len() as u32,
        })
    } else {
        None
    };

    let created = admin.repo.import_nodes(&prepared).await.map_err(|e| {
        ApiError::from_internal(
            e.as_ref(),
            "import discovered nodes",
            "failed to import discovered nodes",
        )
    })?;
    Ok((StatusCode::CREATED, Json(ImportResult { created, filed })))
}

/// Body for the import preview: the candidate addresses about to be imported.
#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct ImportPreviewQuery {
    addresses: Vec<String>,
}

/// One address, and the single folder whose IP range contains it.
#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct AddressProposal {
    address: String,
    group_id: Uuid,
    /// The range that matched — shown so the operator can see *why* this folder is proposed.
    prefix: String,
}

/// One address claimed equally well by two or more folders. Never resolved automatically.
#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct AddressAmbiguity {
    address: String,
    group_ids: Vec<Uuid>,
}

/// Where each candidate would be filed. **A proposal, not an action** — nothing is written by the
/// endpoint that returns this (ADR-131 決定 7, the same posture as ADR-124 決定 6).
#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct ImportPreviewResult {
    matched: Vec<AddressProposal>,
    ambiguous: Vec<AddressAmbiguity>,
    /// Addresses that fall inside no visible folder's range.
    unmatched: Vec<String>,
    /// Whether **any** folder this caller can see carries a range at all.
    ///
    /// Without this, a deployment with no ranges reports every address as unmatched and the
    /// operator cannot tell "these addresses are not covered" from "there was never anything to
    /// match against" — one message for two situations is how an inert feature looks like a
    /// working one.
    any_prefixes: bool,
}

#[utoipa::path(
    post, path = "/api/v1/discovery/import-preview", tag = "discovery",
    request_body = ImportPreviewQuery,
    responses(
        (status = 200, description = "Which folder's IP range would claim each address", body = ImportPreviewResult),
        (status = 400, description = "An unparseable address, or more than one request may carry", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode has no write side", body = super::error::ErrorBody),
    ),
)]
async fn preview_discovery_import(
    _guard: RequireManageConfig,
    Scoped(scope): Scoped,
    admin: Admin,
    Json(body): Json<ImportPreviewQuery>,
) -> ApiResult<Json<ImportPreviewResult>> {
    if body.addresses.len() > MAX_SCAN_TARGETS {
        return Err(ApiError::bad_request(
            "too_many_addresses",
            format!(
                "at most {MAX_SCAN_TARGETS} addresses may be examined in one request, got {}",
                body.addresses.len()
            ),
        ));
    }
    // Parsed at the edge, so the `::inet` cast downstream can only ever see a real address —
    // `match_address_prefixes`' doc says why that matters (a bad value would fail the statement
    // and become a 500 naming nothing, instead of the named 400 below).
    let mut addrs: Vec<IpAddr> = Vec::with_capacity(body.addresses.len());
    for raw in &body.addresses {
        let Ok(addr) = raw.parse::<IpAddr>() else {
            return Err(ApiError::bad_request(
                "invalid_address",
                format!("'{raw}' is not a valid IP address"),
            ));
        };
        addrs.push(addr);
    }
    let hits = admin
        .groups
        .match_address_prefixes(&addrs, scope.group_filter())
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "match discovered addresses",
                "failed to match prefixes",
            )
        })?;
    let any_prefixes = admin
        .groups
        .any_prefixes(scope.group_filter())
        .await
        .map_err(|e| {
            ApiError::from_internal(e.as_ref(), "read group prefixes", "failed to read prefixes")
        })?;
    let fold = crate::groups::fold_prefix_matches(&addrs, hits);
    Ok(Json(ImportPreviewResult {
        matched: fold
            .matched
            .into_iter()
            .map(|(address, group_id, prefix)| AddressProposal {
                address: address.to_string(),
                group_id,
                prefix,
            })
            .collect(),
        ambiguous: fold
            .ambiguous
            .into_iter()
            .map(|(address, group_ids)| AddressAmbiguity {
                address: address.to_string(),
                group_ids,
            })
            .collect(),
        unmatched: fold.unmatched.iter().map(ToString::to_string).collect(),
        any_prefixes,
    }))
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
            // No folder, on purpose. This is the *passive* path (ADR-043 Inc.3): the row is an
            // address a router mentioned, with no site behind it. The scan import files into a
            // folder because the operator aimed the sweep at one; here there is nothing to aim.
            group: None,
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
    Ok((
        StatusCode::CREATED,
        // Filing by IP range is a scan-import concept: this promotes one address a router
        // mentioned, with no sweep and no folder behind it (see `group: None` above).
        Json(ImportResult {
            created,
            filed: None,
        }),
    ))
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
            // The preview writes nothing, but it discloses which folder claims an address, so it
            // is gated exactly as the import it precedes (ADR-131 決定 7).
            ("POST", "/api/v1/discovery/import-preview".to_owned()),
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
    // ── An accepted write (ADR-115) ──────────────────────────────────────────────────

    /// A sweep is accepted and becomes a scan the caller can look up by id.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn starting_a_sweep_is_accepted_and_the_scan_is_listed(pool: sqlx::PgPool) {
        use crate::api::tests_support::{live_state, send, token};
        let st = live_state(pool.clone()).await;
        let tok = token(&st, yagra_common::Role::Admin);
        let (status, body) = send(
            &st,
            "POST",
            "/api/v1/discovery/scan",
            &tok,
            Some(serde_json::json!({ "targets": ["10.0.0.1"], "communities": ["public"] })),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::ACCEPTED, "{body}");
        let id = body["scan_id"].as_str().expect("scan id").to_owned();

        let (status, scan) = send(
            &st,
            "GET",
            &format!("/api/v1/discovery/scan/{id}"),
            &tok,
            None,
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::OK, "{scan}");
    }

    /// An import filed into a folder is accepted, and the node really lands there (ADR-100
    /// decision 10).
    ///
    /// 🚨 The assertion is on the **row**, not on the status. Before `group_id` existed this
    /// endpoint already answered 201 while writing `group_id = NULL` for every node, so a status
    /// check alone would pass against the code this test exists to prove changed.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn importing_into_a_folder_files_the_node_there(pool: sqlx::PgPool) {
        use crate::api::tests_support::{live_state, send, token};
        let st = live_state(pool.clone()).await;
        let tok = token(&st, yagra_common::Role::Admin);

        let (status, created) = send(
            &st,
            "POST",
            "/api/v1/node-groups",
            &tok,
            Some(serde_json::json!({ "name": "Matsuyama Home", "group_type": "site" })),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::CREATED, "{created}");
        let group = created["id"].as_str().expect("group id").to_owned();

        let (status, body) = send(
            &st,
            "POST",
            "/api/v1/discovery/import",
            &tok,
            Some(serde_json::json!({
                "group_id": group,
                "nodes": [{ "address": "192.168.1.50", "name": "found-1" }],
            })),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::CREATED, "{body}");
        assert_eq!(body["created"], 1);

        let filed: Option<uuid::Uuid> =
            sqlx::query_scalar("SELECT group_id FROM nodes WHERE name = 'found-1'")
                .fetch_one(&pool)
                .await
                .expect("the imported node");
        assert_eq!(
            filed.map(|g| g.to_string()),
            Some(group),
            "the node is in the folder the sweep was aimed at"
        );
    }

    /// A folder id nothing has is a 400 that names the problem, not a foreign-key 500.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn importing_into_a_folder_that_does_not_exist_is_refused(pool: sqlx::PgPool) {
        use crate::api::tests_support::{live_state, send, token};
        let st = live_state(pool.clone()).await;
        let tok = token(&st, yagra_common::Role::Admin);
        let (status, body) = send(
            &st,
            "POST",
            "/api/v1/discovery/import",
            &tok,
            Some(serde_json::json!({
                "group_id": uuid::Uuid::new_v4(),
                "nodes": [{ "address": "192.168.1.51", "name": "found-2" }],
            })),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(body["error"]["code"], "invalid_group");
        assert_eq!(
            crate::pgtest::rows(&pool, "nodes").await,
            0,
            "the batch is refused before anything is written"
        );
    }

    // ── ADR-131: filing an import by IP range ───────────────────────────────────────────

    /// A body with no `file_by_prefix` means exactly what it meant before (the N-1 client).
    #[test]
    fn an_import_body_without_the_option_reads_as_off() {
        let body: ImportDiscovered = serde_json::from_value(serde_json::json!({
            "nodes": [{ "address": "10.0.0.1", "name": "r1" }],
        }))
        .expect("parse");
        assert!(!body.file_by_prefix);
        assert!(body.group_id.is_none());
    }

    /// 🚨 The feature's own accepted write: four devices, four destinations, asserted on the rows.
    ///
    /// One inside folder A's range, one inside B's, one inside nothing, and one that A and B claim
    /// at the same length. The last two both land in the fallback — and the counts report them
    /// **separately**, which is the whole of ADR-131 決定 2: folding them would tell the operator
    /// that two addresses are outside every range, which is untrue of the ambiguous one.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn an_import_files_each_device_into_the_folder_whose_range_holds_it(pool: sqlx::PgPool) {
        use crate::api::tests_support::{live_state, send, token};
        let st = live_state(pool.clone()).await;
        let tok = token(&st, yagra_common::Role::Admin);

        let folder = |name: &'static str| {
            let st = st.clone();
            let tok = tok.clone();
            async move {
                let (_, g) = send(
                    &st,
                    "POST",
                    "/api/v1/node-groups",
                    &tok,
                    Some(serde_json::json!({ "name": name, "group_type": "site" })),
                )
                .await;
                g["id"].as_str().expect("id").to_owned()
            }
        };
        let a = folder("A").await;
        let b = folder("B").await;
        let fallback = folder("fallback").await;
        let (a_id, b_id, fb_id): (Uuid, Uuid, Uuid) = (
            a.parse().expect("uuid"),
            b.parse().expect("uuid"),
            fallback.parse().expect("uuid"),
        );

        crate::pgtest::prefix(&pool, a_id, "192.168.1.0/24").await;
        crate::pgtest::prefix(&pool, b_id, "192.168.2.0/24").await;
        // Both claim this one at the same length: the tie the feature refuses to break.
        crate::pgtest::prefix(&pool, a_id, "10.5.0.0/16").await;
        crate::pgtest::prefix(&pool, b_id, "10.5.0.0/16").await;

        let (status, body) = send(
            &st,
            "POST",
            "/api/v1/discovery/import",
            &tok,
            Some(serde_json::json!({
                "group_id": fallback,
                "file_by_prefix": true,
                "nodes": [
                    { "address": "192.168.1.10", "name": "in-a" },
                    { "address": "192.168.2.10", "name": "in-b" },
                    { "address": "172.31.0.1",   "name": "nowhere" },
                    { "address": "10.5.0.9",     "name": "contested" },
                ],
            })),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::CREATED, "{body}");
        assert_eq!(body["created"], 4, "{body}");
        assert_eq!(body["filed"]["matched"], 2, "{body}");
        assert_eq!(body["filed"]["ambiguous"], 1, "{body}");
        assert_eq!(body["filed"]["unmatched"], 1, "{body}");

        let placed = |name: &'static str| {
            let pool = pool.clone();
            async move {
                sqlx::query_scalar::<_, Option<Uuid>>("SELECT group_id FROM nodes WHERE name = $1")
                    .bind(name)
                    .fetch_one(&pool)
                    .await
                    .unwrap_or_else(|e| panic!("read {name}: {e}"))
            }
        };
        assert_eq!(placed("in-a").await, Some(a_id));
        assert_eq!(placed("in-b").await, Some(b_id));
        assert_eq!(
            placed("nowhere").await,
            Some(fb_id),
            "an address no range covers falls back"
        );
        assert_eq!(
            placed("contested").await,
            Some(fb_id),
            "a tie falls back rather than being broken"
        );
    }

    /// With the option off, the same data all lands in the one folder — the compatibility half.
    ///
    /// Without this the test above would pass just as well against an implementation that files by
    /// range unconditionally, which is the behaviour change nobody asked for.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn an_import_with_the_option_off_still_uses_one_folder(pool: sqlx::PgPool) {
        use crate::api::tests_support::{live_state, send, token};
        let st = live_state(pool.clone()).await;
        let tok = token(&st, yagra_common::Role::Admin);
        let (_, a) = send(
            &st,
            "POST",
            "/api/v1/node-groups",
            &tok,
            Some(serde_json::json!({ "name": "A", "group_type": "site" })),
        )
        .await;
        let (_, fb) = send(
            &st,
            "POST",
            "/api/v1/node-groups",
            &tok,
            Some(serde_json::json!({ "name": "fallback", "group_type": "site" })),
        )
        .await;
        let a_id: Uuid = a["id"].as_str().expect("id").parse().expect("uuid");
        let fb_id: Uuid = fb["id"].as_str().expect("id").parse().expect("uuid");
        crate::pgtest::prefix(&pool, a_id, "192.168.1.0/24").await;

        let (status, body) = send(
            &st,
            "POST",
            "/api/v1/discovery/import",
            &tok,
            Some(serde_json::json!({
                "group_id": fb["id"],
                "nodes": [{ "address": "192.168.1.10", "name": "would-match" }],
            })),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::CREATED, "{body}");
        assert!(
            body.get("filed").is_none_or(serde_json::Value::is_null),
            "the wire shape is unchanged when the option is off: {body}"
        );
        let group: Option<Uuid> =
            sqlx::query_scalar("SELECT group_id FROM nodes WHERE name = 'would-match'")
                .fetch_one(&pool)
                .await
                .expect("read");
        assert_eq!(group, Some(fb_id), "the range was not consulted");
    }

    /// The preview answers per address, and says whether there was anything to match against.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn the_import_preview_names_the_folder_that_would_claim_each_address(pool: sqlx::PgPool) {
        use crate::api::tests_support::{live_state, send, token};
        let st = live_state(pool.clone()).await;
        let tok = token(&st, yagra_common::Role::Admin);

        // Before any range exists, `any_prefixes` is what stops "nothing matched" from being read
        // as "these addresses are not covered".
        let (status, empty) = send(
            &st,
            "POST",
            "/api/v1/discovery/import-preview",
            &tok,
            Some(serde_json::json!({ "addresses": ["192.168.1.10"] })),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::OK, "{empty}");
        assert_eq!(empty["any_prefixes"], false, "{empty}");
        assert_eq!(empty["unmatched"][0], "192.168.1.10", "{empty}");

        let (_, g) = send(
            &st,
            "POST",
            "/api/v1/node-groups",
            &tok,
            Some(serde_json::json!({ "name": "A", "group_type": "site" })),
        )
        .await;
        let gid: Uuid = g["id"].as_str().expect("id").parse().expect("uuid");
        crate::pgtest::prefix(&pool, gid, "192.168.1.0/24").await;

        let (status, body) = send(
            &st,
            "POST",
            "/api/v1/discovery/import-preview",
            &tok,
            Some(serde_json::json!({ "addresses": ["192.168.1.10", "10.9.9.9"] })),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::OK, "{body}");
        assert_eq!(body["any_prefixes"], true, "{body}");
        assert_eq!(body["matched"][0]["address"], "192.168.1.10", "{body}");
        assert_eq!(body["matched"][0]["group_id"], g["id"], "{body}");
        assert_eq!(body["matched"][0]["prefix"], "192.168.1.0/24", "{body}");
        assert_eq!(body["unmatched"][0], "10.9.9.9", "{body}");
    }

    /// An address the preview cannot parse is a named 400, never a failed statement.
    ///
    /// This is what keeps raw request text away from `match_address_prefixes`' `::inet` cast,
    /// which that method's doc says must never see one.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn the_import_preview_refuses_a_value_that_is_not_an_address(pool: sqlx::PgPool) {
        use crate::api::tests_support::{live_state, send, token};
        let st = live_state(pool.clone()).await;
        let tok = token(&st, yagra_common::Role::Admin);
        let (status, body) = send(
            &st,
            "POST",
            "/api/v1/discovery/import-preview",
            &tok,
            Some(serde_json::json!({ "addresses": ["nonsense"] })),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(body["error"]["code"], "invalid_address", "{body}");
    }

    /// 🚨 A row's `group_id` has **three** states and they must stay apart.
    ///
    /// Absent ⇒ follow the rule. An id ⇒ that folder. `null` ⇒ the operator chose the tree root.
    /// A plain `Option<Uuid>` collapses the first and the third, which would make the picker's
    /// "Tree root" option quietly mean "file by range instead" — a control that lies.
    #[test]
    fn a_rows_folder_tells_absent_apart_from_an_explicit_root() {
        let body: ImportDiscovered = serde_json::from_value(serde_json::json!({
            "nodes": [
                { "address": "10.0.0.1", "name": "follows-the-rule" },
                { "address": "10.0.0.2", "name": "sent-to-root", "group_id": null },
                { "address": "10.0.0.3", "name": "sent-to-a-folder",
                  "group_id": "11111111-1111-4111-8111-111111111111" },
            ],
        }))
        .expect("parse");
        assert!(
            body.nodes[0].group_id.is_none(),
            "absent means follow the rule"
        );
        assert_eq!(
            body.nodes[1].group_id,
            Some(None),
            "an explicit null is a choice: the tree root"
        );
        assert_eq!(
            body.nodes[2].group_id,
            Some(Some(
                "11111111-1111-4111-8111-111111111111"
                    .parse()
                    .expect("uuid")
            ))
        );
    }

    /// A per-row folder wins over the IP-range rule, and over the request's fallback.
    ///
    /// The row that names a folder is also **kept out of the matcher**, so the two can never
    /// disagree — that is why `chosen` is counted separately from `matched`.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_row_the_operator_directed_ignores_the_range_that_claims_it(pool: sqlx::PgPool) {
        use crate::api::tests_support::{live_state, send, token};
        let st = live_state(pool.clone()).await;
        let tok = token(&st, yagra_common::Role::Admin);

        let mk = |name: &'static str| {
            let st = st.clone();
            let tok = tok.clone();
            async move {
                let (_, g) = send(
                    &st,
                    "POST",
                    "/api/v1/node-groups",
                    &tok,
                    Some(serde_json::json!({ "name": name, "group_type": "site" })),
                )
                .await;
                g["id"].as_str().expect("id").to_owned()
            }
        };
        let ranged = mk("ranged").await;
        let elsewhere = mk("elsewhere").await;
        let fallback = mk("fallback").await;
        let ranged_id: Uuid = ranged.parse().expect("uuid");
        let elsewhere_id: Uuid = elsewhere.parse().expect("uuid");
        let fallback_id: Uuid = fallback.parse().expect("uuid");
        crate::pgtest::prefix(&pool, ranged_id, "192.168.1.0/24").await;

        let (status, body) = send(
            &st,
            "POST",
            "/api/v1/discovery/import",
            &tok,
            Some(serde_json::json!({
                "group_id": fallback,
                "file_by_prefix": true,
                "nodes": [
                    // The range claims this one, and nobody overrode it.
                    { "address": "192.168.1.10", "name": "by-rule" },
                    // The range claims this one too, and the operator said otherwise.
                    { "address": "192.168.1.11", "name": "overridden", "group_id": elsewhere },
                    // …and this one was deliberately sent to the tree root.
                    { "address": "192.168.1.12", "name": "to-root", "group_id": null },
                ],
            })),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::CREATED, "{body}");
        assert_eq!(body["filed"]["matched"], 1, "{body}");
        assert_eq!(
            body["filed"]["chosen"], 2,
            "both directed rows are the operator's, not the rule's: {body}"
        );

        let placed = |name: &'static str| {
            let pool = pool.clone();
            async move {
                sqlx::query_scalar::<_, Option<Uuid>>("SELECT group_id FROM nodes WHERE name = $1")
                    .bind(name)
                    .fetch_one(&pool)
                    .await
                    .unwrap_or_else(|e| panic!("read {name}: {e}"))
            }
        };
        assert_eq!(placed("by-rule").await, Some(ranged_id));
        assert_eq!(
            placed("overridden").await,
            Some(elsewhere_id),
            "the range must not overwrite a folder the operator named"
        );
        assert_eq!(
            placed("to-root").await,
            None,
            "an explicit null is the tree root, not a fall-through to the rule"
        );
        assert_ne!(placed("to-root").await, Some(fallback_id));
    }

    /// A per-row folder outside a scoped caller's reach is refused, and **nothing is imported**.
    ///
    /// The insert is one transaction, so the check has to happen before any row is prepared —
    /// otherwise the operator would be told an import started and then have it rolled back.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_row_aimed_at_a_folder_out_of_scope_imports_nothing(pool: sqlx::PgPool) {
        use crate::api::tests_support::{live_state, scoped_token, send, token};
        let st = live_state(pool.clone()).await;
        let admin = token(&st, yagra_common::Role::Admin);
        let (_, mine) = send(
            &st,
            "POST",
            "/api/v1/node-groups",
            &admin,
            Some(serde_json::json!({ "name": "mine", "group_type": "site" })),
        )
        .await;
        let (_, theirs) = send(
            &st,
            "POST",
            "/api/v1/node-groups",
            &admin,
            Some(serde_json::json!({ "name": "theirs", "group_type": "site" })),
        )
        .await;
        let mine_id: Uuid = mine["id"].as_str().expect("id").parse().expect("uuid");

        let scoped = scoped_token(&st, &[mine_id]);
        let (status, body) = send(
            &st,
            "POST",
            "/api/v1/discovery/import",
            &scoped,
            Some(serde_json::json!({
                "group_id": mine["id"],
                "nodes": [
                    { "address": "10.0.0.1", "name": "ok" },
                    { "address": "10.0.0.2", "name": "out-of-scope", "group_id": theirs["id"] },
                ],
            })),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::NOT_FOUND, "{body}");
        assert_eq!(
            crate::pgtest::rows(&pool, "nodes").await,
            0,
            "the good row must not have landed either"
        );
    }
}
