// SPDX-License-Identifier: AGPL-3.0-only
//! Cisco Meraki — read-only Dashboard API monitoring.
//!
//! Unlike every other node kind, Meraki devices are polled by **core's own org collector**, not by a
//! pool poller: the Dashboard API is a single cloud endpoint with a per-org rate budget, so one
//! collector with one budget is the only shape that works. That is why a Meraki-bound node reports
//! `polled_by: meraki` and is excluded from every pool count.
//!
//! **Two guards protect the API key, and both matter:**
//! - [`meraki_base_url`] refuses anything that is not an https allow-listed Meraki host, checked
//!   *before* the key is ever sent to it. Without it, an operator who can reach this endpoint could
//!   have core hand the key to a server of their choosing.
//! - [`meraki_upstream_error`] answers a generic 502. A Meraki error body can quote the request,
//!   and the request carries the key.
//!
//! The key is sealed **once** per onboarding batch as a shared `meraki_api` credential; the org
//! rows hold only a reference plus their own org id.
//!
//! Reads are `View`, writes are `ManageConfig`. `set_meraki_polling` is the global kill switch —
//! the one control that instantly halts all Meraki collection without losing configuration.

use super::error::{ApiError, ApiResult};
use super::extract::{Admin, RequireManageConfig, RequireView};
use super::ApiState;
use axum::{
    extract::Path,
    http::StatusCode,
    routing::{get, post, put},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Timeout for a control-plane Meraki API call (discover/enumerate) from core.
const MERAKI_API_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
/// Default Dashboard API base URL (the global shard).
const DEFAULT_MERAKI_BASE_URL: &str = "https://api.meraki.com";

/// This domain's slice of the OpenAPI document (ADR-035), merged by [`super::openapi::document`].
#[derive(utoipa::OpenApi)]
#[openapi(paths(
    list_meraki_orgs,
    create_meraki_orgs,
    meraki_discover,
    delete_meraki_org,
    set_meraki_org_enabled,
    set_meraki_org_cadence,
    list_meraki_networks,
    set_meraki_networks_monitored,
    enumerate_meraki_org,
    import_meraki_devices,
    get_meraki_polling,
    set_meraki_polling
))]
pub(super) struct Doc;

/// The Meraki routes, merged into `/api/v1` by [`super::router`].
pub(super) fn routes() -> Router<ApiState> {
    Router::new()
        .route(
            "/api/v1/meraki/orgs",
            get(list_meraki_orgs).post(create_meraki_orgs),
        )
        .route("/api/v1/meraki/orgs/discover", post(meraki_discover))
        .route(
            "/api/v1/meraki/orgs/:id",
            axum::routing::delete(delete_meraki_org),
        )
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
}

/// Validate and normalize an operator-supplied Meraki base URL.
///
/// It must be an `https` URL whose host is an allow-listed Meraki API host, and this runs **before
/// the key is sent anywhere**. Without it, anyone who can reach this endpoint could point core at a
/// server they control and have it deliver the API key there.
fn meraki_base_url(base: Option<String>) -> Result<String, ApiError> {
    let url = base
        .filter(|b| !b.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_MERAKI_BASE_URL.to_owned());
    let parsed = reqwest::Url::parse(&url)
        .map_err(|_| ApiError::bad_request("invalid_base_url", "base_url is not a valid URL"))?;
    let host_ok = parsed
        .host_str()
        .is_some_and(yagra_common::is_meraki_api_host);
    if parsed.scheme() != "https" || !host_ok {
        return Err(ApiError::bad_request(
            "invalid_base_url",
            "base_url must be an https Meraki API host (api.meraki.com / regional shard)",
        ));
    }
    Ok(url)
}

/// Map a Meraki upstream failure to a generic 502.
///
/// Generic on purpose: a Dashboard API error body can quote the request that produced it, and that
/// request carries the API key. The detail goes to the log, never to the client (security.md).
fn meraki_upstream_error(context: &str, e: &yagra_transport::TransportError) -> ApiError {
    tracing::warn!(error = %e, "meraki upstream call failed: {context}");
    ApiError::bad_gateway(
        "meraki_upstream_error",
        format!("Meraki Dashboard API call failed ({context})"),
    )
}

#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct MerakiDiscoverReq {
    api_key: String,
    #[serde(default)]
    base_url: Option<String>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct MerakiOrgOption {
    id: String,
    name: String,
}

/// List the organizations an API key can access, so the operator can multi-select which to monitor.
/// Read-only upstream (`GET /organizations`) and persists nothing.
#[utoipa::path(
    post, path = "/api/v1/meraki/orgs/discover", tag = "meraki",
    request_body = MerakiDiscoverReq,
    responses(
        (status = 200, description = "The organizations the key can access", body = Vec<MerakiOrgOption>),
        (status = 400, description = "The key is empty, or base_url is not an https allow-listed Meraki host", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
        (status = 502, description = "The Dashboard API call failed; the detail is logged, never returned", body = super::error::ErrorBody),
        (status = 503, description = "Inventory storage is unavailable (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn meraki_discover(
    _guard: RequireManageConfig,
    _admin: Admin,
    Json(body): Json<MerakiDiscoverReq>,
) -> ApiResult<Json<Vec<MerakiOrgOption>>> {
    if body.api_key.trim().is_empty() {
        return Err(ApiError::bad_request(
            "invalid_api_key",
            "api_key must not be empty",
        ));
    }
    let base = meraki_base_url(body.base_url)?;
    let orgs = yagra_transport::list_organizations(&base, &body.api_key, MERAKI_API_TIMEOUT)
        .await
        .map_err(|e| meraki_upstream_error("discover organizations", &e))?;
    Ok(Json(
        orgs.into_iter()
            .map(|o| MerakiOrgOption {
                id: o.id,
                name: o.name,
            })
            .collect(),
    ))
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct MerakiOrgView {
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

/// Project a stored org into its API view. **The credential reference is not in it** — the view
/// exists partly so an org row cannot accidentally serialize the field that points at the key.
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

#[utoipa::path(
    get, path = "/api/v1/meraki/orgs", tag = "meraki",
    responses(
        (status = 200, description = "Every onboarded organization; the credential reference is not included", body = Vec<MerakiOrgView>),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks View", body = super::error::ErrorBody),
        (status = 503, description = "Inventory storage is unavailable (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn list_meraki_orgs(
    _guard: RequireView,
    admin: Admin,
) -> ApiResult<Json<Vec<MerakiOrgView>>> {
    let orgs = admin.meraki_orgs.list().await.map_err(|e| {
        ApiError::from_internal(
            e.as_ref(),
            "list meraki orgs",
            "failed to list meraki organizations",
        )
    })?;
    Ok(Json(orgs.iter().map(meraki_org_view).collect()))
}

#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct CreateMerakiOrgsReq {
    api_key: String,
    #[serde(default)]
    base_url: Option<String>,
    org_ids: Vec<String>,
}

/// How many organizations an onboarding batch created.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct MerakiCreated {
    created: u32,
}

/// Onboard one or more organizations under a single read-only API key.
///
/// The key is validated by listing orgs, then sealed **once** as a shared credential — each org row
/// holds a reference plus its own org id. Onboarding twenty orgs therefore stores one secret, not
/// twenty copies of the same one.
///
/// A per-org create failure is logged and skipped rather than failing the batch: the count says how
/// many landed, and retrying is harmless.
#[utoipa::path(
    post, path = "/api/v1/meraki/orgs", tag = "meraki",
    request_body = CreateMerakiOrgsReq,
    responses(
        (status = 201, description = "How many organizations the batch created; a per-org failure is skipped, not fatal", body = MerakiCreated),
        (status = 400, description = "The key or org list is empty, or base_url is not an https allow-listed Meraki host", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
        (status = 502, description = "The Dashboard API rejected the key or was unreachable", body = super::error::ErrorBody),
        (status = 503, description = "Inventory storage is unavailable (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn create_meraki_orgs(
    _guard: RequireManageConfig,
    admin: Admin,
    Json(body): Json<CreateMerakiOrgsReq>,
) -> ApiResult<(StatusCode, Json<MerakiCreated>)> {
    if body.api_key.trim().is_empty() || body.org_ids.is_empty() {
        return Err(ApiError::bad_request(
            "invalid_request",
            "api_key and at least one org_id are required",
        ));
    }
    let base = meraki_base_url(body.base_url)?;
    // Proves the key works before anything is stored, and supplies the org names.
    let orgs = yagra_transport::list_organizations(&base, &body.api_key, MERAKI_API_TIMEOUT)
        .await
        .map_err(|e| meraki_upstream_error("validate key / list organizations", &e))?;
    let secret = serde_json::json!({ "api_key": body.api_key }).to_string();
    let cred_name = format!("Meraki API ({} org)", body.org_ids.len());
    let cred_id = admin
        .creds
        .create(
            &cred_name,
            crate::secrets::KIND_MERAKI_API,
            secret.as_bytes(),
        )
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "seal meraki credential",
                "failed to store meraki credential",
            )
        })?;
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
    Ok((StatusCode::CREATED, Json(MerakiCreated { created })))
}

/// The not-found error every org-scoped endpoint answers with.
fn no_org(id: Uuid) -> ApiError {
    ApiError::not_found("meraki_org_not_found", format!("no meraki org {id}"))
}

/// Delete an organization: removes its device nodes, config, and folder tree.
#[utoipa::path(
    delete, path = "/api/v1/meraki/orgs/{id}", tag = "meraki",
    params(("id" = Uuid, Path, description = "Organization row id")),
    responses(
        (status = 204, description = "Organization, its device nodes and its folder tree removed"),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
        (status = 404, description = "No such organization", body = super::error::ErrorBody),
        (status = 503, description = "Inventory storage is unavailable (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn delete_meraki_org(
    _guard: RequireManageConfig,
    admin: Admin,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    match admin.meraki_orgs.purge(id).await {
        Ok(true) => Ok(StatusCode::NO_CONTENT),
        Ok(false) => Err(no_org(id)),
        Err(e) => Err(ApiError::from_internal(
            e.as_ref(),
            "delete meraki org",
            "failed to delete meraki organization",
        )),
    }
}

#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct MerakiEnabledReq {
    enabled: bool,
}

/// Enable/disable an org — pauses collection without losing its config or history.
#[utoipa::path(
    put, path = "/api/v1/meraki/orgs/{id}/enabled", tag = "meraki",
    params(("id" = Uuid, Path, description = "Organization row id")),
    request_body = MerakiEnabledReq,
    responses(
        (status = 204, description = "Collection paused or resumed; config and history are kept"),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
        (status = 404, description = "No such organization", body = super::error::ErrorBody),
        (status = 503, description = "Inventory storage is unavailable (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn set_meraki_org_enabled(
    _guard: RequireManageConfig,
    admin: Admin,
    Path(id): Path<Uuid>,
    Json(body): Json<MerakiEnabledReq>,
) -> ApiResult<StatusCode> {
    match admin.meraki_orgs.set_enabled(id, body.enabled).await {
        Ok(true) => Ok(StatusCode::NO_CONTENT),
        Ok(false) => Err(no_org(id)),
        Err(e) => Err(ApiError::from_internal(
            e.as_ref(),
            "set meraki org enabled",
            "failed to update meraki organization",
        )),
    }
}

#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct MerakiCadenceReq {
    availability_secs: i32,
    uplink_secs: i32,
    traffic_secs: i32,
    inventory_secs: i32,
    enabled_tiers: Vec<String>,
    target_rps: f64,
}

/// Check a cadence request against the per-tier bands and the hard rps cap.
///
/// All three are safeguards against the *Dashboard API's* rate limit rather than against Yagra:
/// exceeding it gets the whole organization throttled, which stops collection for every device in
/// it. So these are rejected rather than clamped — an operator who typed 1 second meant something,
/// and silently getting 60 would hide that the request was impossible.
fn check_cadence(body: &MerakiCadenceReq) -> Result<(), ApiError> {
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
        return Err(ApiError::bad_request(
            "invalid_cadence",
            "a cadence value is outside its allowed range",
        ));
    }
    if !(body.target_rps > 0.0 && body.target_rps <= MERAKI_TARGET_RPS_MAX) {
        return Err(ApiError::bad_request(
            "invalid_target_rps",
            format!("target_rps must be in (0, {MERAKI_TARGET_RPS_MAX}]"),
        ));
    }
    if body
        .enabled_tiers
        .iter()
        .any(|t| yagra_common::MerakiTier::from_token(t).is_none())
    {
        return Err(ApiError::bad_request(
            "invalid_tier",
            "enabled_tiers contains an unknown tier",
        ));
    }
    Ok(())
}

/// Update an org's per-tier cadence, enabled tiers, and rate budget.
#[utoipa::path(
    put, path = "/api/v1/meraki/orgs/{id}/cadence", tag = "meraki",
    params(("id" = Uuid, Path, description = "Organization row id")),
    request_body = MerakiCadenceReq,
    responses(
        (status = 204, description = "Cadence, enabled tiers and rate budget updated"),
        (status = 400, description = "A cadence value is outside its band, target_rps is outside the cap, or a tier is unknown", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
        (status = 404, description = "No such organization", body = super::error::ErrorBody),
        (status = 503, description = "Inventory storage is unavailable (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn set_meraki_org_cadence(
    _guard: RequireManageConfig,
    admin: Admin,
    Path(id): Path<Uuid>,
    Json(body): Json<MerakiCadenceReq>,
) -> ApiResult<StatusCode> {
    check_cadence(&body)?;
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
        Ok(true) => Ok(StatusCode::NO_CONTENT),
        Ok(false) => Err(no_org(id)),
        Err(e) => Err(ApiError::from_internal(
            e.as_ref(),
            "set meraki org cadence",
            "failed to update meraki organization",
        )),
    }
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct MerakiNetworkView {
    network_id: String,
    name: String,
    monitored: bool,
}

/// The org's networks with their monitored (in-scope) flag.
#[utoipa::path(
    get, path = "/api/v1/meraki/orgs/{id}/networks", tag = "meraki",
    params(("id" = Uuid, Path, description = "Organization row id")),
    responses(
        (status = 200, description = "The org's known networks and whether each is in scope", body = Vec<MerakiNetworkView>),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks View", body = super::error::ErrorBody),
        (status = 503, description = "Inventory storage is unavailable (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn list_meraki_networks(
    _guard: RequireView,
    admin: Admin,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Vec<MerakiNetworkView>>> {
    let nets = admin.meraki_orgs.list_networks(id).await.map_err(|e| {
        ApiError::from_internal(
            e.as_ref(),
            "list meraki networks",
            "failed to list meraki networks",
        )
    })?;
    Ok(Json(
        nets.into_iter()
            .map(|(network_id, name, monitored)| MerakiNetworkView {
                network_id,
                name,
                monitored,
            })
            .collect(),
    ))
}

#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct MerakiMonitoredReq {
    network_ids: Vec<String>,
    monitored: bool,
}

/// Set the monitored (watch/skip) flag for a set of the org's networks.
#[utoipa::path(
    put, path = "/api/v1/meraki/orgs/{id}/networks", tag = "meraki",
    params(("id" = Uuid, Path, description = "Organization row id")),
    request_body = MerakiMonitoredReq,
    responses(
        (status = 204, description = "Network scope updated"),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
        (status = 503, description = "Inventory storage is unavailable (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn set_meraki_networks_monitored(
    _guard: RequireManageConfig,
    admin: Admin,
    Path(id): Path<Uuid>,
    Json(body): Json<MerakiMonitoredReq>,
) -> ApiResult<StatusCode> {
    admin
        .meraki_orgs
        .set_networks_monitored(id, &body.network_ids, body.monitored)
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "set meraki networks monitored",
                "failed to update network scope",
            )
        })?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct MerakiCandidate {
    serial: String,
    name: String,
    model: Option<String>,
    product_type: String,
    network_id: String,
    network_name: String,
    lan_ip: Option<String>,
}

/// What the import wizard reads in one call: the org's network scope, and the devices not yet
/// imported.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct MerakiEnumeration {
    networks: Vec<MerakiNetworkView>,
    devices: Vec<MerakiCandidate>,
}

/// Enumerate an org's networks and devices from the Dashboard API, for the import wizard.
///
/// Read-only upstream. It upserts the network scope (**preserving monitored flags** — re-enumerating
/// must not silently un-watch networks an operator chose) and returns the devices not already
/// imported.
#[utoipa::path(
    post, path = "/api/v1/meraki/orgs/{id}/enumerate", tag = "meraki",
    params(("id" = Uuid, Path, description = "Organization row id")),
    responses(
        (status = 200, description = "The org's networks and the devices not already imported", body = MerakiEnumeration),
        (status = 400, description = "The org's stored API key could not be resolved", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
        (status = 404, description = "No such organization", body = super::error::ErrorBody),
        (status = 502, description = "The Dashboard API call failed; the detail is logged, never returned", body = super::error::ErrorBody),
        (status = 503, description = "Inventory storage is unavailable (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn enumerate_meraki_org(
    _guard: RequireManageConfig,
    admin: Admin,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<MerakiEnumeration>> {
    let org = admin
        .meraki_orgs
        .get(id)
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "enumerate: load org",
                "failed to load meraki organization",
            )
        })?
        .ok_or_else(|| no_org(id))?;
    let api_key = crate::meraki::resolve_meraki_key(&admin.creds, org.credential_id)
        .await
        .ok_or_else(|| {
            ApiError::bad_request(
                "meraki_credential_unavailable",
                "the org's API key could not be resolved",
            )
        })?;
    let networks =
        yagra_transport::list_networks(&org.base_url, &api_key, &org.org_id, MERAKI_API_TIMEOUT)
            .await
            .map_err(|e| meraki_upstream_error("list networks", &e))?;
    let devices =
        yagra_transport::list_devices(&org.base_url, &api_key, &org.org_id, MERAKI_API_TIMEOUT)
            .await
            .map_err(|e| meraki_upstream_error("list devices", &e))?;
    // Persist the network list + stamp the sync. Both are best-effort: failing the wizard because
    // a cache write missed would be worse than showing the freshly-fetched data.
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
    Ok(Json(MerakiEnumeration {
        networks: networks_view,
        devices: candidates,
    }))
}

#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct MerakiImportReq {
    org_uuid: Uuid,
    #[serde(default)]
    monitored_network_ids: Vec<String>,
    devices: Vec<MerakiImportDeviceReq>,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct MerakiImportDeviceReq {
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

/// How many devices an import created.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct MerakiImported {
    imported: u32,
}

/// Import selected devices as nodes, atomically.
///
/// Already-imported serials are skipped rather than rejected, so re-running the wizard after a
/// partial selection does the obvious thing instead of erroring on the ones already there.
#[utoipa::path(
    post, path = "/api/v1/meraki/import", tag = "meraki",
    request_body = MerakiImportReq,
    responses(
        (status = 201, description = "How many devices became nodes; already-imported serials are skipped", body = MerakiImported),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
        (status = 404, description = "No such organization", body = super::error::ErrorBody),
        (status = 503, description = "Inventory storage is unavailable (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn import_meraki_devices(
    _guard: RequireManageConfig,
    admin: Admin,
    Json(body): Json<MerakiImportReq>,
) -> ApiResult<(StatusCode, Json<MerakiImported>)> {
    let org = admin
        .meraki_orgs
        .get(body.org_uuid)
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "import: load org",
                "failed to load meraki organization",
            )
        })?
        .ok_or_else(|| no_org(body.org_uuid))?;
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
        // Prefer the built-in Meraki-API profile for the product type; fall back to the category's
        // profile so an unrecognized product type still gets monitored rather than nothing.
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
        // A device with no name is identified by its serial, which is always present.
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
    let count = admin
        .meraki_orgs
        .import_devices(&org, &to_import)
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "meraki import",
                "failed to import meraki devices",
            )
        })?;
    Ok((
        StatusCode::CREATED,
        Json(MerakiImported { imported: count }),
    ))
}

#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct MerakiPollingReq {
    enabled: bool,
}

/// The global Meraki polling kill switch.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct MerakiPolling {
    enabled: bool,
}

#[utoipa::path(
    get, path = "/api/v1/meraki/polling", tag = "meraki",
    responses(
        (status = 200, description = "Whether Meraki collection is running at all", body = MerakiPolling),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks View", body = super::error::ErrorBody),
        (status = 503, description = "Inventory storage is unavailable (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn get_meraki_polling(_guard: RequireView, admin: Admin) -> ApiResult<Json<MerakiPolling>> {
    let enabled = admin.repo.get_meraki_polling_enabled().await;
    Ok(Json(MerakiPolling { enabled }))
}

/// Set the global kill switch — the one control that instantly halts all Meraki collection without
/// losing any configuration, for when the Dashboard API budget needs to be given back at once.
#[utoipa::path(
    put, path = "/api/v1/meraki/polling", tag = "meraki",
    request_body = MerakiPollingReq,
    responses(
        (status = 204, description = "Collection halted or resumed globally; no configuration is lost"),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
        (status = 503, description = "Inventory storage is unavailable (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn set_meraki_polling(
    _guard: RequireManageConfig,
    admin: Admin,
    Json(body): Json<MerakiPollingReq>,
) -> ApiResult<StatusCode> {
    admin
        .repo
        .set_meraki_polling_enabled(body.enabled)
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "set meraki polling switch",
                "failed to update meraki polling switch",
            )
        })?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::router;
    use crate::api::tests_support::{private_state, public_state};
    use axum::body::Body;
    use axum::http::{header::AUTHORIZATION, Request};
    use tower::ServiceExt;
    use yagra_common::{Principal, Role, Scope};

    const ID: &str = "00000000-0000-0000-0000-000000000001";

    fn write_routes() -> Vec<(&'static str, String)> {
        vec![
            ("POST", "/api/v1/meraki/orgs".to_owned()),
            ("POST", "/api/v1/meraki/orgs/discover".to_owned()),
            ("DELETE", format!("/api/v1/meraki/orgs/{ID}")),
            ("PUT", format!("/api/v1/meraki/orgs/{ID}/enabled")),
            ("PUT", format!("/api/v1/meraki/orgs/{ID}/cadence")),
            ("PUT", format!("/api/v1/meraki/orgs/{ID}/networks")),
            ("POST", format!("/api/v1/meraki/orgs/{ID}/enumerate")),
            ("POST", "/api/v1/meraki/import".to_owned()),
            ("PUT", "/api/v1/meraki/polling".to_owned()),
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
    async fn anything_that_touches_the_api_key_is_admin_only() {
        // `discover` takes no stored state at all, but it *accepts* a key and calls out with it, so
        // it is gated exactly like the writes.
        for (method, path) in write_routes() {
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
        for role in [Role::Viewer, Role::Operator] {
            let token = st
                .sessions
                .issue(Uuid::new_v4(), Principal::new(role, Scope::All), "u");
            for (method, path) in write_routes() {
                assert_eq!(
                    status_of(st.clone(), method, &path, Some(&token)).await,
                    StatusCode::FORBIDDEN,
                    "{role:?} {method} {path}"
                );
            }
        }
    }

    #[test]
    fn the_base_url_allowlist_is_what_keeps_the_key_from_leaving_meraki() {
        // Absent/blank ⇒ the global shard.
        assert_eq!(meraki_base_url(None).unwrap(), DEFAULT_MERAKI_BASE_URL);
        assert_eq!(
            meraki_base_url(Some("   ".to_owned())).unwrap(),
            DEFAULT_MERAKI_BASE_URL
        );
        assert!(meraki_base_url(Some("https://api.meraki.com".to_owned())).is_ok());

        // Anything else would receive the API key on the very next call.
        for bad in [
            "http://api.meraki.com",              // plaintext
            "https://api.meraki.com.attacker.io", // suffix trick
            "https://evil.example",
            "not a url",
        ] {
            assert_eq!(
                meraki_base_url(Some(bad.to_owned())).unwrap_err().code(),
                "invalid_base_url",
                "{bad}"
            );
        }
    }

    #[test]
    fn an_out_of_band_cadence_is_rejected_rather_than_clamped() {
        // The bands protect the *Dashboard API's* rate limit: exceeding it throttles the whole
        // organization, stopping collection for every device in it. Silently substituting a legal
        // value would hide that the operator asked for something impossible.
        let ok = || MerakiCadenceReq {
            availability_secs: crate::config::MERAKI_FAST_MIN_SECS,
            uplink_secs: crate::config::MERAKI_FAST_MIN_SECS,
            traffic_secs: crate::config::MERAKI_TRAFFIC_MIN_SECS,
            inventory_secs: crate::config::MERAKI_INVENTORY_MIN_SECS,
            enabled_tiers: Vec::new(),
            target_rps: 1.0,
        };
        assert!(check_cadence(&ok()).is_ok());

        let mut fast = ok();
        fast.availability_secs = 1;
        assert_eq!(check_cadence(&fast).unwrap_err().code(), "invalid_cadence");

        let mut rps = ok();
        rps.target_rps = 0.0;
        assert_eq!(
            check_cadence(&rps).unwrap_err().code(),
            "invalid_target_rps"
        );
        rps.target_rps = crate::config::MERAKI_TARGET_RPS_MAX + 1.0;
        assert_eq!(
            check_cadence(&rps).unwrap_err().code(),
            "invalid_target_rps"
        );

        let mut tier = ok();
        tier.enabled_tiers = vec!["not-a-tier".to_owned()];
        assert_eq!(check_cadence(&tier).unwrap_err().code(), "invalid_tier");
    }
}
