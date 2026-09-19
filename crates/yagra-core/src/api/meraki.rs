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
//!
//! **Nothing here asks the Dashboard API what an organization holds.** That is the inventory sync's
//! (`meraki_sync.rs`); the device list and an import both read what it recorded. The import
//! wizard's own `POST …/enumerate` did ask, leniently, and was removed with the wizard (ADR-164
//! Inc.5) — two readers of one listing, one of which accepted a partial answer.
//!
//! **Every write also refuses a folder-scoped caller** ([`meraki_is_deployment_wide`], ADR-164).
//! `ManageConfig` is held by Operators, and an Operator can be restricted to folders (ADR-014). An
//! organization is not inside anybody's folders: its key sees every device in it, importing files
//! nodes wherever they belong, and deleting it purges all of them. Until ADR-164 these routes were
//! ledgered "admin-only, an Admin is unscoped by construction" while taking a guard an Operator
//! passes — so a scoped Operator could create nodes outside their folders and delete nodes they
//! could not see. The same hole `cfeda70b` closed for wireless controllers.

use super::error::{ApiError, ApiResult};
use super::extract::{Admin, Leader, RequireManageConfig, RequireView, Scoped};
use super::ApiState;
use axum::{
    extract::Path,
    http::StatusCode,
    routing::{get, post, put},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::meraki_filing::{Filing, FilingReason, MerakiFiled};
use crate::meraki_import::ImportCandidate;
use crate::meraki_inventory::{
    usable_address, DeviceRecord, MerakiDeviceCounts, MerakiDeviceState,
};
use crate::meraki_sync::{MerakiSyncFailure, MerakiSyncReport, SyncError};

/// Timeout for a control-plane Meraki API call (discover, and validating a key) from core.
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
    set_meraki_import_settings,
    sync_meraki_org,
    list_meraki_devices,
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
            "/api/v1/meraki/orgs/:id/import-settings",
            put(set_meraki_import_settings),
        )
        .route("/api/v1/meraki/orgs/:id/sync", post(sync_meraki_org))
        .route("/api/v1/meraki/orgs/:id/devices", get(list_meraki_devices))
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

/// Refuse a folder-scoped caller a Meraki write. **Call it first**, before the body is looked at and
/// before anything is sent to the Dashboard API.
///
/// Refused rather than narrowed because there is nothing to narrow by: a device that has not been
/// imported yet belongs to no folder, the organization's folder sits at the top of the tree, and
/// pausing or deleting an organization reaches every node in it. Letting a caller through when they
/// can see the organization's folder is a possible later loosening and would be purely additive.
///
/// The reason string is repeated by the route ledger's `MERAKI_WRITE` line.
fn meraki_is_deployment_wide(scope: &super::scope::NodeScope) -> Result<(), ApiError> {
    super::scope::require_fleet_wide(
        scope,
        "a Meraki organization is monitored as a whole, across every folder, so an account \
         restricted to folders cannot change it",
    )
}

/// The name an onboarding batch's shared credential is stored under.
///
/// Names the organizations rather than counting them. It used to be `Meraki API (N org)`, so two
/// batches of the same size produced two credentials nobody could tell apart on the Credentials
/// page — which is exactly the case once a deployment holds more than one API key (ADR-164).
fn meraki_credential_name(org_names: &[&str]) -> String {
    match org_names {
        [] => "Meraki API".to_owned(),
        [only] => format!("Meraki API — {only}"),
        [first, rest @ ..] => format!("Meraki API — {first} +{}", rest.len()),
    }
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
        (status = 403, description = "Role lacks ManageConfig, or the account is restricted to folders (`scope_unsupported`)", body = super::error::ErrorBody),
        (status = 502, description = "The Dashboard API call failed; the detail is logged, never returned", body = super::error::ErrorBody),
        (status = 503, description = "Inventory storage is unavailable (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn meraki_discover(
    _guard: RequireManageConfig,
    Scoped(scope): Scoped,
    _admin: Admin,
    Json(body): Json<MerakiDiscoverReq>,
) -> ApiResult<Json<Vec<MerakiOrgOption>>> {
    meraki_is_deployment_wide(&scope)?;
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
    /// When the last **successful** inventory sync ran. A failed sync does not move it.
    last_sync_at: Option<chrono::DateTime<chrono::Utc>>,
    /// `null` until a sync has run — "has not synced yet" is not "failed".
    last_sync_ok: Option<bool>,
    /// Why the last sync failed; `null` after a success. A closed vocabulary, never upstream text.
    last_sync_error: Option<MerakiSyncFailure>,
    /// What the last successful sync found, read against which devices are nodes here.
    devices: MerakiDeviceCounts,
    /// Whether the sync turns newly listed devices into nodes, and watches newly found networks.
    import_devices: bool,
    /// Whether an imported device is filed by its address into the folder whose IP range holds it.
    file_by_prefix: bool,
    /// The most nodes automatic import lets this organization hold.
    max_devices: u32,
    /// How many devices that cap left out on the last sync; zero while automatic import is off.
    devices_over_cap: u32,
}

/// Project a stored org into its API view. **The credential reference is not in it** — the view
/// exists partly so an org row cannot accidentally serialize the field that points at the key.
fn meraki_org_view(o: &crate::meraki::MerakiOrg, devices: MerakiDeviceCounts) -> MerakiOrgView {
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
        last_sync_at: o.last_sync_at,
        last_sync_ok: o.last_sync_ok,
        // Only a failed sync has a reason. A row written by a newer core may carry a token this
        // build does not know; `from_token` reads that as `internal`, never as "no failure".
        last_sync_error: (o.last_sync_ok == Some(false)).then(|| {
            MerakiSyncFailure::from_token(o.last_sync_error.as_deref().unwrap_or_default())
        }),
        devices,
        import_devices: o.import_devices,
        file_by_prefix: o.file_by_prefix,
        max_devices: o.max_devices,
        devices_over_cap: o.devices_over_cap,
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
    Ok(Json(org_views(&admin).await?))
}

/// The onboarded organizations as the API exposes them — the seam both edges call.
///
/// `meraki_org_view` is what keeps the credential reference off the wire; going through it rather
/// than the stored row is the whole point of having a seam here.
pub(crate) async fn org_views(admin: &super::AdminState) -> ApiResult<Vec<MerakiOrgView>> {
    let orgs = admin.meraki_orgs.list().await.map_err(|e| {
        ApiError::from_internal(
            e.as_ref(),
            "list meraki orgs",
            "failed to list meraki organizations",
        )
    })?;
    let counts = admin.meraki_inventory.counts().await.map_err(|e| {
        ApiError::from_internal(
            e.as_ref(),
            "count meraki inventory",
            "failed to list meraki organizations",
        )
    })?;
    Ok(orgs
        .iter()
        // An organization that has not synced yet has no rows, and so no entry: all zero.
        .map(|o| meraki_org_view(o, counts.get(&o.id).copied().unwrap_or_default()))
        .collect())
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
        (status = 403, description = "Role lacks ManageConfig, or the account is restricted to folders (`scope_unsupported`)", body = super::error::ErrorBody),
        (status = 502, description = "The Dashboard API rejected the key or was unreachable", body = super::error::ErrorBody),
        (status = 503, description = "Inventory storage is unavailable (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn create_meraki_orgs(
    _guard: RequireManageConfig,
    Scoped(scope): Scoped,
    admin: Admin,
    Json(body): Json<CreateMerakiOrgsReq>,
) -> ApiResult<(StatusCode, Json<MerakiCreated>)> {
    meraki_is_deployment_wide(&scope)?;
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
    // The name each requested org goes by, falling back to its id exactly as the row below does.
    let org_name = |oid: &String| -> String {
        orgs.iter()
            .find(|o| &o.id == oid)
            .map_or_else(|| oid.clone(), |o| o.name.clone())
    };
    let names: Vec<String> = body.org_ids.iter().map(org_name).collect();
    let cred_name = meraki_credential_name(&names.iter().map(String::as_str).collect::<Vec<_>>());
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
    for (oid, name) in body.org_ids.iter().zip(&names) {
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
        (status = 403, description = "Role lacks ManageConfig, or the account is restricted to folders (`scope_unsupported`)", body = super::error::ErrorBody),
        (status = 404, description = "No such organization", body = super::error::ErrorBody),
        (status = 503, description = "Inventory storage is unavailable (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn delete_meraki_org(
    _guard: RequireManageConfig,
    Scoped(scope): Scoped,
    admin: Admin,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    meraki_is_deployment_wide(&scope)?;
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
        (status = 403, description = "Role lacks ManageConfig, or the account is restricted to folders (`scope_unsupported`)", body = super::error::ErrorBody),
        (status = 404, description = "No such organization", body = super::error::ErrorBody),
        (status = 503, description = "Inventory storage is unavailable (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn set_meraki_org_enabled(
    _guard: RequireManageConfig,
    Scoped(scope): Scoped,
    admin: Admin,
    Path(id): Path<Uuid>,
    Json(body): Json<MerakiEnabledReq>,
) -> ApiResult<StatusCode> {
    meraki_is_deployment_wide(&scope)?;
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
        (status = 403, description = "Role lacks ManageConfig, or the account is restricted to folders (`scope_unsupported`)", body = super::error::ErrorBody),
        (status = 404, description = "No such organization", body = super::error::ErrorBody),
        (status = 503, description = "Inventory storage is unavailable (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn set_meraki_org_cadence(
    _guard: RequireManageConfig,
    Scoped(scope): Scoped,
    admin: Admin,
    Path(id): Path<Uuid>,
    Json(body): Json<MerakiCadenceReq>,
) -> ApiResult<StatusCode> {
    meraki_is_deployment_wide(&scope)?;
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
    Ok(Json(network_views(&admin, id).await?))
}

/// One org's networks with their monitored flag — the seam both edges call.
pub(crate) async fn network_views(
    admin: &super::AdminState,
    org: Uuid,
) -> ApiResult<Vec<MerakiNetworkView>> {
    let nets = admin.meraki_orgs.list_networks(org).await.map_err(|e| {
        ApiError::from_internal(
            e.as_ref(),
            "list meraki networks",
            "failed to list meraki networks",
        )
    })?;
    Ok(nets
        .into_iter()
        .map(|(network_id, name, monitored)| MerakiNetworkView {
            network_id,
            name,
            monitored,
        })
        .collect())
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
        (status = 403, description = "Role lacks ManageConfig, or the account is restricted to folders (`scope_unsupported`)", body = super::error::ErrorBody),
        (status = 503, description = "Inventory storage is unavailable (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn set_meraki_networks_monitored(
    _guard: RequireManageConfig,
    Scoped(scope): Scoped,
    admin: Admin,
    Path(id): Path<Uuid>,
    Json(body): Json<MerakiMonitoredReq>,
) -> ApiResult<StatusCode> {
    meraki_is_deployment_wide(&scope)?;
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

#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct MerakiImportSettingsReq {
    /// Turn newly listed devices into nodes on every sync, and watch newly found networks.
    import_devices: bool,
    /// File an imported device by its address into the folder whose IP range holds it.
    file_by_prefix: bool,
    /// The most nodes automatic import lets the organization hold. Absent keeps the current cap.
    #[serde(default)]
    max_devices: Option<i32>,
}

/// Set how the inventory sync imports an organization's devices.
///
/// With `import_devices` on, each sync turns a device into a node when it is in a watched network,
/// Meraki has reported it online at least once, and it has never been a node here — so a device an
/// operator deleted stays deleted. A network the sync finds for the first time is watched from then
/// on; a network already known keeps its flag. Takes effect at the next sync.
#[utoipa::path(
    put, path = "/api/v1/meraki/orgs/{id}/import-settings", tag = "meraki",
    params(("id" = Uuid, Path, description = "Organization row id")),
    request_body = MerakiImportSettingsReq,
    responses(
        (status = 204, description = "Import settings stored; they take effect at the next sync"),
        (status = 400, description = "max_devices outside 1–50000 (`invalid_max_devices`)", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig, or the account is restricted to folders (`scope_unsupported`)", body = super::error::ErrorBody),
        (status = 404, description = "No such organization", body = super::error::ErrorBody),
        (status = 503, description = "Inventory storage is unavailable (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn set_meraki_import_settings(
    _guard: RequireManageConfig,
    Scoped(scope): Scoped,
    admin: Admin,
    Path(id): Path<Uuid>,
    Json(body): Json<MerakiImportSettingsReq>,
) -> ApiResult<StatusCode> {
    meraki_is_deployment_wide(&scope)?;
    let org = admin
        .meraki_orgs
        .get(id)
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "import settings: load org",
                "failed to load meraki organization",
            )
        })?
        .ok_or_else(|| no_org(id))?;
    let max_devices = body
        .max_devices
        .unwrap_or_else(|| i32::try_from(org.max_devices).unwrap_or(i32::MAX));
    // Refused, not clamped: a cap silently moved is a cap the operator does not know they have.
    if !(1..=crate::config::MERAKI_MAX_DEVICES_HARD).contains(&max_devices) {
        return Err(ApiError::bad_request(
            "invalid_max_devices",
            format!(
                "max_devices must be between 1 and {}",
                crate::config::MERAKI_MAX_DEVICES_HARD
            ),
        ));
    }
    match admin
        .meraki_orgs
        .set_import_settings(id, body.import_devices, body.file_by_prefix, max_devices)
        .await
    {
        Ok(true) => Ok(StatusCode::NO_CONTENT),
        Ok(false) => Err(no_org(id)),
        Err(e) => Err(ApiError::from_internal(
            e.as_ref(),
            "set meraki import settings",
            "failed to update import settings",
        )),
    }
}

/// Refuse a folder-scoped caller the device list (ADR-164 決定 11).
///
/// A read, and refused all the same. The list names every device in the organization with its
/// address, and most of those are not in anybody's folders yet — an unimported device has no folder
/// to be inside or outside of — so there is no honest way to narrow it. Serving it whole would hand
/// a scoped account the inventory of sites it was restricted away from.
fn meraki_devices_are_deployment_wide(scope: &super::scope::NodeScope) -> Result<(), ApiError> {
    super::scope::require_fleet_wide(
        scope,
        "the device list covers a whole Meraki organization, most of it not yet filed in any \
         folder, so it cannot be narrowed to an account restricted to folders",
    )
}

/// Sync one organization's inventory now, rather than waiting for the periodic sync.
///
/// Read-only upstream (three paged GETs). It goes through the same single flight as the periodic
/// sync and the collector, so pressing it while a collect is running answers 409 rather than
/// spending the organization's rate budget twice.
#[utoipa::path(
    post, path = "/api/v1/meraki/orgs/{id}/sync", tag = "meraki",
    params(("id" = Uuid, Path, description = "Organization row id")),
    responses(
        (status = 200, description = "The sync completed; what it found and how many rows it wrote", body = MerakiSyncReport),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig, or the account is restricted to folders (`scope_unsupported`)", body = super::error::ErrorBody),
        (status = 404, description = "No such organization", body = super::error::ErrorBody),
        (status = 409, description = "Meraki polling is paused globally (`meraki_polling_paused`), this organization is paused (`meraki_org_paused`), or a collect or another sync is running for it (`meraki_sync_busy`)", body = super::error::ErrorBody),
        (status = 502, description = "The sync ran and failed (`meraki_sync_failed`); the reason is recorded on the organization as `last_sync_error`", body = super::error::ErrorBody),
        (status = 503, description = "Inventory storage is unavailable (skeleton mode), or this core is a standby (`not_leader`)", body = super::error::ErrorBody),
    ),
)]
async fn sync_meraki_org(
    _guard: RequireManageConfig,
    Scoped(scope): Scoped,
    admin: Admin,
    // Leader-gated because the organization's single flight lives in the leader's process: a
    // standby syncing would run beside the leader's collector with neither knowing about the other.
    _leader: Leader,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<MerakiSyncReport>> {
    meraki_is_deployment_wide(&scope)?;
    let org = admin
        .meraki_orgs
        .get(id)
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "sync: load org",
                "failed to load meraki organization",
            )
        })?
        .ok_or_else(|| no_org(id))?;
    // Both switches mean "send nothing to the Dashboard API", and a button must not be a way round
    // them. Said as a conflict rather than done silently, so the operator learns which switch.
    if !admin.repo.get_meraki_polling_enabled().await {
        return Err(ApiError::conflict(
            "meraki_polling_paused",
            "Meraki polling is paused for the whole deployment; resume it before syncing",
        ));
    }
    if !org.enabled {
        return Err(ApiError::conflict(
            "meraki_org_paused",
            "this organization is paused; resume it before syncing",
        ));
    }
    match admin.meraki_sync.sync_org(&org).await {
        Ok(report) => Ok(Json(report)),
        Err(SyncError::Busy) => Err(ApiError::conflict(
            "meraki_sync_busy",
            "a collect or another sync is running for this organization; try again in a moment",
        )),
        // The reason is a closed vocabulary (`MerakiSyncFailure`), so naming it is safe — unlike
        // `meraki_upstream_error`, which has only an upstream string and must stay generic.
        Err(SyncError::Failed(reason)) => Err(ApiError::bad_gateway(
            "meraki_sync_failed",
            format!("the sync failed ({})", reason.as_str()),
        )),
    }
}

/// One device of an organization, as the last successful sync recorded it.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct MerakiDeviceView {
    serial: String,
    name: String,
    model: Option<String>,
    product_type: String,
    network_id: String,
    /// The network's name; `null` for a network the sync has not recorded.
    network_name: Option<String>,
    /// Whether the device's network is one this organization watches.
    network_monitored: bool,
    /// The address Meraki reports, when it reports a usable one.
    #[schema(value_type = Option<String>)]
    lan_ip: Option<std::net::IpAddr>,
    state: MerakiDeviceState,
    /// The node this device is monitored as; `null` unless `state` is `monitored` or `missing`.
    node_id: Option<Uuid>,
    first_seen_at: chrono::DateTime<chrono::Utc>,
    /// When a complete listing first failed to contain the device; `null` while Meraki lists it.
    missing_since: Option<chrono::DateTime<chrono::Utc>>,
    /// The folder the device is filed in, when it is a node — or the folder an import would file
    /// it in, when `filing.reason` is `matched`. `null` for a node at the top of the tree, and for
    /// a device an import would put under the organization's own folder, in one named after its
    /// network (that folder is created by the import that first needs it).
    folder_id: Option<Uuid>,
    /// Where an import would file the device, and why. `null` for a device that is already a
    /// node: it is where it is, and no import moves it.
    filing: Option<MerakiFilingView>,
}

/// Where an import would file a device that is not a node yet, under the organization's current
/// `file_by_prefix` setting and the IP ranges folders carry right now.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct MerakiFilingView {
    reason: FilingReason,
    /// The IP range that claimed the address; only with `matched`.
    prefix: Option<String>,
    /// How many folders claim the address equally; only with `ambiguous`.
    folders: Option<u32>,
}

impl From<&Filing> for MerakiFilingView {
    fn from(f: &Filing) -> Self {
        let (prefix, folders) = match f {
            Filing::Matched { prefix, .. } => (Some(prefix.clone()), None),
            Filing::Ambiguous { folders } => {
                (None, Some(u32::try_from(*folders).unwrap_or(u32::MAX)))
            }
            Filing::Unmatched | Filing::NoAddress | Filing::NotAsked => (None, None),
        };
        Self {
            reason: f.reason(),
            prefix,
            folders,
        }
    }
}

impl MerakiDeviceView {
    /// `filing` is what an import would do with the device; `None` for one that is already a node.
    fn new(d: DeviceRecord, filing: Option<&Filing>) -> Self {
        Self {
            folder_id: match filing {
                Some(f) => f.folder(),
                None => d.node_group_id,
            },
            filing: filing.map(MerakiFilingView::from),
            serial: d.serial,
            name: d.name,
            model: d.model,
            product_type: d.product_type,
            network_id: d.network_id,
            network_name: d.network_name,
            network_monitored: d.network_monitored,
            lan_ip: d.lan_ip,
            state: d.state,
            node_id: d.node_id,
            first_seen_at: d.first_seen_at,
            missing_since: d.missing_since,
        }
    }
}

/// An organization's devices as the last successful sync recorded them — monitored or not — with
/// each one's state. Served from PostgreSQL: it never calls the Dashboard API.
#[utoipa::path(
    get, path = "/api/v1/meraki/orgs/{id}/devices", tag = "meraki",
    params(("id" = Uuid, Path, description = "Organization row id")),
    responses(
        (status = 200, description = "The organization's devices; empty until its first sync", body = Vec<MerakiDeviceView>),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks View, or the account is restricted to folders (`scope_unsupported`)", body = super::error::ErrorBody),
        (status = 404, description = "No such organization", body = super::error::ErrorBody),
        (status = 503, description = "Inventory storage is unavailable (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn list_meraki_devices(
    _guard: RequireView,
    Scoped(scope): Scoped,
    admin: Admin,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Vec<MerakiDeviceView>>> {
    Ok(Json(device_views(&admin, &scope, id).await?))
}

/// One organization's devices — the seam both edges call, so the scoped refusal is decided once.
pub(crate) async fn device_views(
    admin: &super::AdminState,
    scope: &super::scope::NodeScope,
    org: Uuid,
) -> ApiResult<Vec<MerakiDeviceView>> {
    meraki_devices_are_deployment_wide(scope)?;
    let stored = admin
        .meraki_orgs
        .get(org)
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "devices: load org",
                "failed to load meraki organization",
            )
        })?
        .ok_or_else(|| no_org(org))?;
    let devices = admin.meraki_inventory.devices(org).await.map_err(|e| {
        ApiError::from_internal(
            e.as_ref(),
            "list meraki devices",
            "failed to list meraki devices",
        )
    })?;
    // Where each device that is not a node would go — asked of the same resolver an import uses,
    // so the page cannot promise one folder and the import use another. A device that is a node
    // is not asked about: no import moves it.
    let addresses: Vec<Option<std::net::IpAddr>> = devices
        .iter()
        .filter(|d| d.node_id.is_none())
        .map(|d| d.lan_ip)
        .collect();
    let (filings, _) = admin
        .meraki_import
        .filings(&addresses, stored.file_by_prefix)
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "devices: match ip ranges",
                "failed to list meraki devices",
            )
        })?;
    let mut filings = filings.iter();
    Ok(devices
        .into_iter()
        .map(|d| {
            let filing = if d.node_id.is_none() {
                filings.next()
            } else {
                None
            };
            MerakiDeviceView::new(d, filing)
        })
        .collect())
}

#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct MerakiImportReq {
    org_uuid: Uuid,
    #[serde(default)]
    monitored_network_ids: Vec<String>,
    devices: Vec<MerakiImportDeviceReq>,
    /// File each device into the folder whose IP range holds its address, when exactly one does;
    /// false files every device under the organization's network folders. Absent means the
    /// organization's own `file_by_prefix` setting — what its page shows and the sync uses.
    #[serde(default)]
    file_by_prefix: Option<bool>,
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

/// What an import created, and where it put it.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct MerakiImported {
    /// Devices that became nodes. A serial that already was one is not counted.
    imported: u32,
    /// How those devices were filed. The four add up to `imported`, except that all four are zero
    /// when the request switched filing by IP range off.
    filed: MerakiFiled,
    /// Whether any folder carries an IP range at all. False means `filed.unmatched` says nothing
    /// about the devices: there was nothing for an address to match.
    ranges_configured: bool,
}

/// Import selected devices as nodes, atomically.
///
/// A device whose address falls inside exactly one folder's IP range is filed in that folder;
/// every other device goes under the organization's folder, in a folder named after its network.
/// Already-imported serials are skipped rather than rejected, so importing again after a partial
/// selection does the obvious thing instead of erroring on the ones already there.
#[utoipa::path(
    post, path = "/api/v1/meraki/import", tag = "meraki",
    request_body = MerakiImportReq,
    responses(
        (status = 201, description = "How many devices became nodes and how they were filed; already-imported serials are skipped", body = MerakiImported),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig, or the account is restricted to folders (`scope_unsupported`)", body = super::error::ErrorBody),
        (status = 404, description = "No such organization", body = super::error::ErrorBody),
        (status = 503, description = "Inventory storage is unavailable (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn import_meraki_devices(
    _guard: RequireManageConfig,
    Scoped(scope): Scoped,
    admin: Admin,
    Json(body): Json<MerakiImportReq>,
) -> ApiResult<(StatusCode, Json<MerakiImported>)> {
    meraki_is_deployment_wide(&scope)?;
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
    // Where each device goes is decided by the resolver the sync also uses, and written by
    // `import_devices`; which serials are already nodes is decided *there*, under its lock. Reading
    // them here first is what let two imports of one device both see it free.
    let candidates: Vec<ImportCandidate> = body
        .devices
        .into_iter()
        .map(|d| ImportCandidate {
            lan_ip: d.lan_ip.as_deref().and_then(usable_address),
            serial: d.serial,
            name: d.name,
            model: d.model,
            product_type: d.product_type,
            network_id: d.network_id,
            network_name: d.network_name,
        })
        .collect();
    let resolved = admin
        .meraki_import
        .resolve(
            candidates,
            body.file_by_prefix.unwrap_or(org.file_by_prefix),
        )
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "import: resolve filing",
                "failed to import meraki devices",
            )
        })?;
    let outcome = admin
        .meraki_orgs
        .import_devices(&org, &resolved.devices)
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
        Json(MerakiImported {
            imported: outcome.imported,
            filed: outcome.filed,
            ranges_configured: resolved.ranges_configured,
        }),
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
    Ok(Json(polling_switch(&admin).await))
}

/// The global Meraki kill switch — the seam both edges call. Exists because `MerakiPolling`'s field
/// is private to this module, and it should stay that way.
pub(crate) async fn polling_switch(admin: &super::AdminState) -> MerakiPolling {
    MerakiPolling {
        enabled: admin.repo.get_meraki_polling_enabled().await,
    }
}

/// Set the global kill switch — the one control that instantly halts all Meraki collection without
/// losing any configuration, for when the Dashboard API budget needs to be given back at once.
#[utoipa::path(
    put, path = "/api/v1/meraki/polling", tag = "meraki",
    request_body = MerakiPollingReq,
    responses(
        (status = 204, description = "Collection halted or resumed globally; no configuration is lost"),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig, or the account is restricted to folders (`scope_unsupported`)", body = super::error::ErrorBody),
        (status = 503, description = "Inventory storage is unavailable (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn set_meraki_polling(
    _guard: RequireManageConfig,
    Scoped(scope): Scoped,
    admin: Admin,
    Json(body): Json<MerakiPollingReq>,
) -> ApiResult<StatusCode> {
    meraki_is_deployment_wide(&scope)?;
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
            ("PUT", format!("/api/v1/meraki/orgs/{ID}/import-settings")),
            ("POST", format!("/api/v1/meraki/orgs/{ID}/sync")),
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
    async fn anything_that_touches_the_api_key_is_operator_and_up() {
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
        // A cloud-managed fleet is inventory, so ADR-057 puts it with the rest of the monitoring
        // setup: an operator imports and enables, a viewer does neither. 503 = past the guard.
        let st = private_state();
        for (role, want) in [
            (Role::Viewer, StatusCode::FORBIDDEN),
            (Role::Operator, StatusCode::SERVICE_UNAVAILABLE),
        ] {
            let token = st
                .sessions
                .issue(Uuid::new_v4(), Principal::new(role, Scope::All), "u");
            for (method, path) in write_routes() {
                assert_eq!(
                    status_of(st.clone(), method, &path, Some(&token)).await,
                    want,
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

    #[test]
    fn a_batchs_credential_is_named_after_its_organizations() {
        // Two batches of one organization each used to be two rows both called
        // "Meraki API (1 org)" — indistinguishable on the Credentials page (ADR-164).
        assert_eq!(meraki_credential_name(&["Acme"]), "Meraki API — Acme");
        assert_eq!(
            meraki_credential_name(&["Acme", "Beta", "Gamma"]),
            "Meraki API — Acme +2"
        );
        assert_ne!(
            meraki_credential_name(&["Acme"]),
            meraki_credential_name(&["Beta"])
        );
        // The handler refuses an empty batch before it gets here; this only must not panic.
        assert_eq!(meraki_credential_name(&[]), "Meraki API");
    }

    /// Every region the WebUI's picker offers is one this edge accepts.
    ///
    /// The picker and the allow-list are two copies of one fact in two languages, and nothing
    /// compared them: the picker offered Canada (`api.meraki.ca`) while the allow-list refused it,
    /// so choosing that region answered `400 invalid_base_url` every time (ADR-164). This reads the
    /// picker's own file and runs each URL through the function the endpoint runs.
    #[test]
    fn every_region_the_webui_offers_passes_the_base_url_allowlist() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../web/src/pages/integrations/merakiRegions.ts"
        );
        let text = std::fs::read_to_string(path).expect("the WebUI's Meraki region list");
        let urls: Vec<&str> = text
            .split(['\'', '"'])
            .filter(|s| s.starts_with("https://"))
            .collect();
        // A floor on what was INSPECTED: a reader that stopped finding URLs would otherwise pass.
        assert!(
            urls.len() >= 4,
            "found only {} region URLs in {path} — did the file's shape change?",
            urls.len()
        );
        for url in urls {
            assert!(
                meraki_base_url(Some(url.to_owned())).is_ok(),
                "the WebUI offers {url}, which this API refuses as invalid_base_url"
            );
        }
    }

    /// The import cap's bounds exist twice — `config::MERAKI_MAX_DEVICES_HARD` (with migration
    /// 0125's CHECK) and the organization page's own constants, which refuse a value before the
    /// request is sent. A form stricter than the server hides a legal setting; a looser one lets the
    /// operator press Save into a 400. Same shape as the region check above.
    ///
    /// ⚠️ It finds each bound as `NAME = <digits>;`, so one computed from another goes unchecked.
    #[test]
    fn the_import_cap_the_webui_accepts_is_the_one_this_api_accepts() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../web/src/pages/integrations/merakiDevices.ts"
        );
        let text = std::fs::read_to_string(path).expect("the organization page's judgement module");
        let bound = |name: &str| -> i32 {
            let declared = format!("export const {name} = ");
            let rest = text
                .split_once(declared.as_str())
                .unwrap_or_else(|| panic!("{name} is not declared in {path}"))
                .1;
            let digits: String = rest
                .chars()
                .take_while(|c| *c != ';')
                .filter(char::is_ascii_digit)
                .collect();
            digits
                .parse()
                .unwrap_or_else(|_| panic!("{name} in {path} is not a plain number"))
        };
        assert_eq!(bound("MAX_DEVICES_MIN"), 1);
        assert_eq!(
            bound("MAX_DEVICES_MAX"),
            crate::config::MERAKI_MAX_DEVICES_HARD
        );
    }

    // ── A folder-scoped caller (ADR-164) ─────────────────────────────────────────────

    /// A body each write route deserializes, so the request reaches the handler rather than being
    /// turned away at the JSON extractor — the refusal under test lives in the handler body.
    fn write_requests() -> Vec<(&'static str, String, Option<serde_json::Value>)> {
        use serde_json::json;
        vec![
            (
                "POST",
                "/api/v1/meraki/orgs".to_owned(),
                Some(json!({ "api_key": "k", "org_ids": ["1"] })),
            ),
            (
                "POST",
                "/api/v1/meraki/orgs/discover".to_owned(),
                Some(json!({ "api_key": "k" })),
            ),
            ("DELETE", format!("/api/v1/meraki/orgs/{ID}"), None),
            (
                "PUT",
                format!("/api/v1/meraki/orgs/{ID}/enabled"),
                Some(json!({ "enabled": false })),
            ),
            (
                "PUT",
                format!("/api/v1/meraki/orgs/{ID}/cadence"),
                Some(json!({
                    "availability_secs": 300, "uplink_secs": 300, "traffic_secs": 1800,
                    "inventory_secs": 21600, "enabled_tiers": [], "target_rps": 1.0,
                })),
            ),
            (
                "PUT",
                format!("/api/v1/meraki/orgs/{ID}/networks"),
                Some(json!({ "network_ids": [], "monitored": true })),
            ),
            (
                "PUT",
                format!("/api/v1/meraki/orgs/{ID}/import-settings"),
                Some(json!({ "import_devices": true, "file_by_prefix": true })),
            ),
            ("POST", format!("/api/v1/meraki/orgs/{ID}/sync"), None),
            (
                "POST",
                "/api/v1/meraki/import".to_owned(),
                Some(json!({ "org_uuid": ID, "devices": [] })),
            ),
            (
                "PUT",
                "/api/v1/meraki/polling".to_owned(),
                Some(json!({ "enabled": false })),
            ),
        ]
    }

    #[test]
    fn the_scoped_refusal_is_tried_against_every_write_route() {
        // `write_requests` is a second list of the write routes; it must not quietly fall behind
        // the first, or a new write route would ship without its refusal being exercised.
        let mut routes: Vec<(&str, String)> = write_routes();
        let mut requests: Vec<(&str, String)> = write_requests()
            .into_iter()
            .map(|(m, p, _)| (m, p))
            .collect();
        routes.sort();
        requests.sort();
        assert_eq!(routes, requests);
    }

    /// A caller restricted to folders can change nothing about a Meraki organization.
    ///
    /// `ManageConfig` is an Operator's permission and an Operator can be scoped (ADR-014), while an
    /// organization spans every folder: importing files nodes wherever they belong, and deleting one
    /// purges every node it holds. Before ADR-164 none of these routes asked for the scope at all.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_folder_scoped_caller_is_refused_every_meraki_write(pool: sqlx::PgPool) {
        use crate::api::tests_support::{live_state, scoped_token, send, token};
        let st = live_state(pool.clone()).await;
        let folder = crate::pgtest::group(&pool, "Branch").await;
        let scoped = scoped_token(&st, &[folder]);
        // Assembled rather than written out: `scope.rs::no_handler_spells_the_scope_refusal_by_hand`
        // counts the quoted code across this directory, tests included, so that a handler cannot
        // hand-roll the refusal. The handlers here call `require_fleet_wide`; this is the only
        // place the file needs the word, and the same spelling that guard's own test uses.
        let refusal = format!("{}_unsupported", "scope");
        for (method, path, body) in write_requests() {
            let (status, answer) = send(&st, method, &path, &scoped, body).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{method} {path}: {answer}");
            assert_eq!(
                answer["error"]["code"].as_str(),
                Some(refusal.as_str()),
                "{method} {path}"
            );
        }
        // Refused means refused: the kill switch the last request tried to flip is untouched.
        let admin = st.admin.clone().expect("live state");
        assert!(admin.repo.get_meraki_polling_enabled().await);

        // And it is the *scope* that was refused, not the route: the same request from an unscoped
        // caller gets as far as looking the organization up.
        let unscoped = token(&st, yagra_common::Role::Operator);
        let (status, answer) = send(
            &st,
            "DELETE",
            &format!("/api/v1/meraki/orgs/{ID}"),
            &unscoped,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{answer}");
        assert_eq!(answer["error"]["code"], "meraki_org_not_found");
    }

    // ── An accepted write (ADR-115) ──────────────────────────────────────────────────

    /// The Meraki polling switch is written to the deployment's settings.
    ///
    /// This endpoint and not `POST /meraki/orgs`: creating an organization validates the API key
    /// against the Dashboard API before storing anything, so it cannot be accepted without a
    /// network. What is provable here is the half that lives in PostgreSQL.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn setting_the_meraki_polling_switch_reaches_the_settings_row(pool: sqlx::PgPool) {
        use crate::api::tests_support::{live_state, send, token};
        let st = live_state(pool.clone()).await;
        let tok = token(&st, yagra_common::Role::Admin);
        let admin = st.admin.clone().expect("live state");
        assert!(admin.repo.get_meraki_polling_enabled().await);
        let (status, body) = send(
            &st,
            "PUT",
            "/api/v1/meraki/polling",
            &tok,
            Some(serde_json::json!({ "enabled": false })),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::NO_CONTENT, "{body}");
        assert!(!admin.repo.get_meraki_polling_enabled().await);
    }

    /// "Sync now" is accepted, recorded on the organization's row, and refused by each switch that
    /// means "send nothing to Meraki" (ADR-164). The device list answers from the database.
    ///
    /// ⚠️ The fixture's Dashboard is [`crate::api::tests_support::EmptyDashboard`], so this proves
    /// the endpoint — the flight, the key, the stamp, the two 409s — and nothing about what a sync
    /// does with devices. That is `meraki_sync.rs`'s, against its own fake.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn syncing_an_organization_is_accepted_and_recorded_on_its_row(pool: sqlx::PgPool) {
        use crate::api::tests_support::{live_state, scoped_token, send, token};
        let st = live_state(pool.clone()).await;
        let admin = st.admin.clone().expect("live state");
        // A real sealed key: the sync opens it before it asks the Dashboard anything.
        let credential = admin
            .creds
            .create(
                "Meraki API — Acme",
                crate::secrets::KIND_MERAKI_API,
                br#"{"api_key":"not-a-real-key"}"#,
            )
            .await
            .expect("seal key");
        let org = admin
            .meraki_orgs
            .create("123456", "Acme", "https://api.meraki.com", credential)
            .await
            .expect("create org");
        let operator = token(&st, yagra_common::Role::Operator);
        let sync = format!("/api/v1/meraki/orgs/{org}/sync");
        let devices = format!("/api/v1/meraki/orgs/{org}/devices");

        // Before any sync: not failed, not synced, and no counts to show.
        let (status, orgs) = send(&st, "GET", "/api/v1/meraki/orgs", &operator, None).await;
        assert_eq!(status, StatusCode::OK, "{orgs}");
        assert!(orgs[0]["last_sync_ok"].is_null(), "{orgs}");
        assert!(orgs[0]["last_sync_at"].is_null(), "{orgs}");

        let (status, report) = send(&st, "POST", &sync, &operator, None).await;
        assert_eq!(status, StatusCode::OK, "{report}");
        assert_eq!(report["devices"], 0, "{report}");

        let (_, orgs) = send(&st, "GET", "/api/v1/meraki/orgs", &operator, None).await;
        assert_eq!(orgs[0]["last_sync_ok"], true, "{orgs}");
        assert!(orgs[0]["last_sync_at"].is_string(), "{orgs}");
        assert!(orgs[0]["last_sync_error"].is_null(), "{orgs}");
        assert_eq!(
            orgs[0]["devices"],
            serde_json::json!({ "seen": 0, "monitored": 0, "new": 0, "missing": 0 })
        );
        assert!(
            orgs[0].get("credential_id").is_none(),
            "the view must not carry the reference to the key"
        );

        let (status, list) = send(&st, "GET", &devices, &operator, None).await;
        assert_eq!(status, StatusCode::OK, "{list}");
        assert_eq!(list, serde_json::json!([]));

        // The device list is refused to a folder-restricted account, read though it is.
        let folder = crate::pgtest::group(&pool, "Branch").await;
        let scoped = scoped_token(&st, &[folder]);
        let (status, answer) = send(&st, "GET", &devices, &scoped, None).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{answer}");

        // A paused organization is not synced…
        admin
            .meraki_orgs
            .set_enabled(org, false)
            .await
            .expect("pause");
        let (status, answer) = send(&st, "POST", &sync, &operator, None).await;
        assert_eq!(status, StatusCode::CONFLICT, "{answer}");
        assert_eq!(answer["error"]["code"], "meraki_org_paused");
        admin
            .meraki_orgs
            .set_enabled(org, true)
            .await
            .expect("resume");

        // …and neither is any organization while the kill switch is engaged.
        admin
            .repo
            .set_meraki_polling_enabled(false)
            .await
            .expect("kill switch");
        let (status, answer) = send(&st, "POST", &sync, &operator, None).await;
        assert_eq!(status, StatusCode::CONFLICT, "{answer}");
        assert_eq!(answer["error"]["code"], "meraki_polling_paused");

        let unknown = format!("/api/v1/meraki/orgs/{ID}/sync");
        let (status, answer) = send(&st, "POST", &unknown, &operator, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{answer}");
        let unknown = format!("/api/v1/meraki/orgs/{ID}/devices");
        let (status, answer) = send(&st, "GET", &unknown, &operator, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{answer}");
    }

    /// An import is accepted, files each device by its address, and says how (ADR-164): the one
    /// folder whose IP range holds the address, otherwise the organization's network folder — and
    /// `0.0.0.0` is not an address, whatever range would hold it.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn an_import_files_each_device_by_its_address_and_says_how(pool: sqlx::PgPool) {
        use crate::api::tests_support::{live_state, send, token};
        let st = live_state(pool.clone()).await;
        let admin = st.admin.clone().expect("live state");
        let credential = crate::pgtest::credential(&pool, "meraki-key", "meraki_api").await;
        let org = admin
            .meraki_orgs
            .create("123456", "Acme", "https://api.meraki.com", credential)
            .await
            .expect("create org");
        let caller = token(&st, yagra_common::Role::Admin);
        let device = |serial: &str, ip: Option<&str>| {
            serde_json::json!({
                "serial": serial, "name": serial, "model": "MR46", "product_type": "wireless",
                "network_id": "N_1", "network_name": "One", "lan_ip": ip,
            })
        };
        let folder_of = |pool: sqlx::PgPool, serial: &'static str| async move {
            sqlx::query_scalar::<_, Option<Uuid>>(
                "SELECT n.group_id FROM nodes n JOIN meraki_devices d ON d.node_id = n.id \
                 WHERE d.serial = $1",
            )
            .bind(serial)
            .fetch_one(&pool)
            .await
            .expect("the node's folder")
        };
        let network = crate::meraki::network_group_id(org, "N_1");

        // No folder carries a range yet: everything goes under the network, and the answer says
        // that "unmatched" is not a statement about the devices.
        let (status, answer) = send(
            &st,
            "POST",
            "/api/v1/meraki/import",
            &caller,
            Some(serde_json::json!({ "org_uuid": org, "devices": [device("Q3-0", Some("10.1.0.4"))] })),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{answer}");
        assert_eq!(answer["imported"], 1, "{answer}");
        assert_eq!(answer["ranges_configured"], false, "{answer}");
        assert_eq!(folder_of(pool.clone(), "Q3-0").await, Some(network));

        // A site with a range, and a catch-all that would swallow an address-less device if
        // `0.0.0.0` were treated as an address.
        let site = crate::pgtest::group(&pool, "Matsuyama").await;
        crate::pgtest::prefix(&pool, site, "10.1.0.0/24").await;
        let everything = crate::pgtest::group(&pool, "Everything").await;
        crate::pgtest::prefix(&pool, everything, "0.0.0.0/0").await;

        let (status, answer) = send(
            &st,
            "POST",
            "/api/v1/meraki/import",
            &caller,
            Some(serde_json::json!({
                "org_uuid": org,
                "devices": [
                    device("Q3-1", Some("10.1.0.5")),
                    device("Q3-2", None),
                    device("Q3-3", Some("0.0.0.0")),
                    device("Q3-0", Some("10.1.0.4")),
                ],
            })),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{answer}");
        assert_eq!(
            answer["imported"], 3,
            "the already-imported serial was counted: {answer}"
        );
        assert_eq!(answer["ranges_configured"], true, "{answer}");
        assert_eq!(
            answer["filed"],
            serde_json::json!({ "matched": 1, "ambiguous": 0, "unmatched": 0, "no_address": 2 }),
            "{answer}"
        );
        assert_eq!(
            folder_of(pool.clone(), "Q3-1").await,
            Some(site),
            "the longest range did not win over the catch-all"
        );
        assert_eq!(folder_of(pool.clone(), "Q3-2").await, Some(network));
        assert_eq!(
            folder_of(pool.clone(), "Q3-3").await,
            Some(network),
            "0.0.0.0 was matched against the catch-all range"
        );

        // Switched off, the same address goes under the network and nothing is claimed about it.
        let (status, answer) = send(
            &st,
            "POST",
            "/api/v1/meraki/import",
            &caller,
            Some(serde_json::json!({
                "org_uuid": org, "file_by_prefix": false,
                "devices": [device("Q3-4", Some("10.1.0.6"))],
            })),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{answer}");
        assert_eq!(
            answer["filed"],
            serde_json::json!({ "matched": 0, "ambiguous": 0, "unmatched": 0, "no_address": 0 }),
            "{answer}"
        );
        assert_eq!(folder_of(pool.clone(), "Q3-4").await, Some(network));
    }

    /// The import settings are accepted and reach the row, and an absurd cap is refused rather than
    /// clamped (ADR-164 Inc.4). An absent `max_devices` keeps the cap the organization has.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn the_import_settings_are_stored_and_an_absurd_cap_is_refused(pool: sqlx::PgPool) {
        use crate::api::tests_support::{live_state, send, token};
        let st = live_state(pool.clone()).await;
        let admin = st.admin.clone().expect("live state");
        let credential = crate::pgtest::credential(&pool, "meraki-key", "meraki_api").await;
        let org = admin
            .meraki_orgs
            .create("123456", "Acme", "https://api.meraki.com", credential)
            .await
            .expect("create org");
        let operator = token(&st, yagra_common::Role::Operator);
        let path = format!("/api/v1/meraki/orgs/{org}/import-settings");

        let (status, body) = send(
            &st,
            "PUT",
            &path,
            &operator,
            Some(serde_json::json!({
                "import_devices": false, "file_by_prefix": false, "max_devices": 50,
            })),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
        let stored = admin.meraki_orgs.get(org).await.expect("get").expect("org");
        assert_eq!(
            (
                stored.import_devices,
                stored.file_by_prefix,
                stored.max_devices
            ),
            (false, false, 50)
        );
        let (_, orgs) = send(&st, "GET", "/api/v1/meraki/orgs", &operator, None).await;
        assert_eq!(orgs[0]["import_devices"], false, "{orgs}");
        assert_eq!(orgs[0]["file_by_prefix"], false, "{orgs}");
        assert_eq!(orgs[0]["max_devices"], 50, "{orgs}");
        assert_eq!(orgs[0]["devices_over_cap"], 0, "{orgs}");

        // No cap named: the one it has stays.
        let (status, body) = send(
            &st,
            "PUT",
            &path,
            &operator,
            Some(serde_json::json!({ "import_devices": true, "file_by_prefix": true })),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
        let stored = admin.meraki_orgs.get(org).await.expect("get").expect("org");
        assert_eq!((stored.import_devices, stored.max_devices), (true, 50));

        for absurd in [0, -1, 50_001] {
            let (status, answer) = send(
                &st,
                "PUT",
                &path,
                &operator,
                Some(serde_json::json!({
                    "import_devices": true, "file_by_prefix": true, "max_devices": absurd,
                })),
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{absurd}: {answer}");
            assert_eq!(answer["error"]["code"], "invalid_max_devices", "{absurd}");
        }
        let stored = admin.meraki_orgs.get(org).await.expect("get").expect("org");
        assert_eq!(stored.max_devices, 50, "a refused cap was stored");

        let unknown = format!("/api/v1/meraki/orgs/{ID}/import-settings");
        let (status, answer) = send(
            &st,
            "PUT",
            &unknown,
            &operator,
            Some(serde_json::json!({ "import_devices": true, "file_by_prefix": true })),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{answer}");
    }

    /// The device list says where each device is, or would go — from the same resolver an import
    /// uses, so the page cannot promise one folder and the import use another (ADR-164 Inc.5).
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn the_device_list_says_where_each_device_is_or_would_go(pool: sqlx::PgPool) {
        use crate::api::tests_support::{live_state, send, token};
        use crate::meraki_inventory::{DeviceWrite, SeenDevice, SyncPlan};
        let st = live_state(pool.clone()).await;
        let admin = st.admin.clone().expect("live state");
        let credential = crate::pgtest::credential(&pool, "meraki-key", "meraki_api").await;
        let org = admin
            .meraki_orgs
            .create("123456", "Acme", "https://api.meraki.com", credential)
            .await
            .expect("create org");
        let site = crate::pgtest::group(&pool, "Matsuyama").await;
        crate::pgtest::prefix(&pool, site, "10.1.0.0/24").await;

        // What a sync would have recorded: three devices, all seen online.
        let seen = |serial: &str, ip: Option<&str>| DeviceWrite {
            device: SeenDevice {
                serial: serial.to_owned(),
                name: serial.to_owned(),
                model: Some("MR46".to_owned()),
                product_type: "wireless".to_owned(),
                network_id: "N_1".to_owned(),
                lan_ip: ip.map(|a| a.parse().expect("ip")),
                online: true,
            },
            first_online: true,
            imported_at: None,
        };
        admin
            .meraki_inventory
            .apply(
                org,
                &SyncPlan {
                    writes: vec![
                        seen("Q3-A", Some("10.1.0.5")),
                        seen("Q3-B", None),
                        seen("Q3-C", Some("192.168.9.9")),
                    ],
                    newly_missing: Vec::new(),
                },
            )
            .await
            .expect("record the inventory");

        let caller = token(&st, yagra_common::Role::Admin);
        let path = format!("/api/v1/meraki/orgs/{org}/devices");
        let row = |list: &serde_json::Value, serial: &str| -> serde_json::Value {
            list.as_array()
                .expect("a list")
                .iter()
                .find(|d| d["serial"] == serial)
                .unwrap_or_else(|| panic!("{serial} is not listed: {list}"))
                .clone()
        };

        let (status, list) = send(&st, "GET", &path, &caller, None).await;
        assert_eq!(status, StatusCode::OK, "{list}");
        let a = row(&list, "Q3-A");
        assert_eq!(a["state"], "new", "{a}");
        assert_eq!(a["folder_id"], serde_json::json!(site), "{a}");
        assert_eq!(
            a["filing"],
            serde_json::json!({ "reason": "matched", "prefix": "10.1.0.0/24", "folders": null }),
            "{a}"
        );
        let b = row(&list, "Q3-B");
        assert!(b["folder_id"].is_null(), "{b}");
        assert_eq!(b["filing"]["reason"], "no_address", "{b}");
        assert_eq!(row(&list, "Q3-C")["filing"]["reason"], "unmatched");

        // Imported with no `file_by_prefix` in the request: the organization's own setting decides,
        // and the device lands where the list said it would.
        let (status, answer) = send(
            &st,
            "POST",
            "/api/v1/meraki/import",
            &caller,
            Some(serde_json::json!({
                "org_uuid": org,
                "devices": [{
                    "serial": "Q3-A", "name": "Q3-A", "product_type": "wireless",
                    "network_id": "N_1", "lan_ip": "10.1.0.5",
                }],
            })),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{answer}");
        assert_eq!(answer["filed"]["matched"], 1, "{answer}");

        let (_, list) = send(&st, "GET", &path, &caller, None).await;
        let a = row(&list, "Q3-A");
        assert_eq!(a["state"], "monitored", "{a}");
        assert_eq!(a["folder_id"], serde_json::json!(site), "{a}");
        assert!(
            a["filing"].is_null(),
            "a node is where it is; no import moves it: {a}"
        );

        // With filing by range switched off for the organization, nothing is claimed about ranges.
        admin
            .meraki_orgs
            .set_import_settings(org, true, false, 1000)
            .await
            .expect("stop filing by range");
        let (_, list) = send(&st, "GET", &path, &caller, None).await;
        assert_eq!(row(&list, "Q3-C")["filing"]["reason"], "not_asked");
        assert_eq!(row(&list, "Q3-B")["filing"]["reason"], "not_asked");
    }
}
