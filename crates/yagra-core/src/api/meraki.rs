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
//! could not see. The same hole `9b1c295d` closed for wireless controllers.

use super::error::{ApiError, ApiResult};
use super::extract::{Admin, RequireManageConfig, RequireView, Scoped};
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
use crate::meraki_inventory::{DeviceRecord, MerakiDeviceCounts, MerakiDeviceState};
use crate::meraki_sync::MerakiSyncFailure;

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

/// Where an onboarding request's API key comes from (ADR-164 Inc.6).
#[derive(Debug, PartialEq, Eq)]
enum KeySource {
    /// Typed into the dialog. Sealed as a new credential if an organization is added with it.
    Typed(String),
    /// A `meraki_api` credential already in the store, named by id. Nothing new is sealed.
    Saved(Uuid),
}

/// Read the key's source out of a request body: **exactly one** of the two fields.
///
/// Both is refused rather than resolved by a precedence rule. A client that sends both has a bug,
/// and whichever one quietly won would be the wrong one half the time — with the loser being a key
/// somebody typed and believes is in use.
fn key_source(api_key: Option<&str>, credential_id: Option<Uuid>) -> Result<KeySource, ApiError> {
    let typed = api_key.map(str::trim).filter(|k| !k.is_empty());
    match (typed, credential_id) {
        (Some(_), Some(_)) => Err(ApiError::bad_request(
            "invalid_request",
            "send api_key or credential_id, not both",
        )),
        (Some(key), None) => Ok(KeySource::Typed(key.to_owned())),
        (None, Some(id)) => Ok(KeySource::Saved(id)),
        (None, None) => Err(ApiError::bad_request(
            "invalid_api_key",
            "api_key or credential_id is required",
        )),
    }
}

/// The key itself, from wherever the request said it is.
///
/// 🚨 For a saved key, [`crate::meraki::open_saved_meraki_key`] refuses any credential that is not
/// a `meraki_api` one **before** its secret is read, so naming an SNMP community's id here cannot
/// get that community sent to the Dashboard API. Both refusals are 400 `invalid_credential` — the
/// id came in the body, so it is the request that is wrong (the shape `invalid_group` has).
async fn resolve_key(admin: &super::AdminState, source: &KeySource) -> Result<String, ApiError> {
    use crate::meraki::SavedKeyError;
    let id = match source {
        KeySource::Typed(key) => return Ok(key.clone()),
        KeySource::Saved(id) => *id,
    };
    match crate::meraki::open_saved_meraki_key(&admin.creds, id).await {
        Ok(key) => Ok(key),
        Err(SavedKeyError::NotFound) => Err(ApiError::bad_request(
            "invalid_credential",
            format!("no credential {id}"),
        )),
        Err(SavedKeyError::WrongKind) => Err(ApiError::bad_request(
            "invalid_credential",
            format!("credential {id} is not a Meraki API key"),
        )),
        Err(SavedKeyError::Unreadable(e)) => Err(ApiError::from_internal(
            e.as_ref(),
            "open saved meraki key",
            "failed to read the saved Meraki API key",
        )),
    }
}

/// The `org_id` of every organization already onboarded. `meraki_orgs.org_id` is `UNIQUE` across
/// the deployment, whichever key an organization was added under.
async fn onboarded_org_ids(
    admin: &super::AdminState,
) -> Result<std::collections::HashSet<String>, ApiError> {
    let orgs = admin.meraki_orgs.list().await.map_err(|e| {
        ApiError::from_internal(
            e.as_ref(),
            "list meraki orgs",
            "failed to list meraki organizations",
        )
    })?;
    Ok(orgs.into_iter().map(|o| o.org_id).collect())
}

#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct MerakiDiscoverReq {
    /// A key typed by the operator. Send this **or** `credential_id`, never both.
    #[serde(default)]
    api_key: Option<String>,
    /// A `meraki_api` credential already stored here, used instead of typing the key again.
    #[serde(default)]
    credential_id: Option<Uuid>,
    #[serde(default)]
    base_url: Option<String>,
}

#[derive(Debug, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub(crate) struct MerakiOrgOption {
    id: String,
    name: String,
    /// This organization is already monitored here, under this key or another one. It cannot be
    /// added a second time: `POST /meraki/orgs` skips it.
    already_added: bool,
}

/// What the key can see, each marked with whether it is already here.
fn org_options(
    seen: Vec<yagra_transport::MerakiOrgInfo>,
    onboarded: &std::collections::HashSet<String>,
) -> Vec<MerakiOrgOption> {
    seen.into_iter()
        .map(|o| MerakiOrgOption {
            already_added: onboarded.contains(&o.id),
            id: o.id,
            name: o.name,
        })
        .collect()
}

/// List the organizations an API key can access, so the operator can multi-select which to monitor.
/// Read-only upstream (`GET /organizations`) and persists nothing.
#[utoipa::path(
    post, path = "/api/v1/meraki/orgs/discover", tag = "meraki",
    request_body = MerakiDiscoverReq,
    responses(
        (status = 200, description = "The organizations the key can access, each marked `already_added` when it is monitored here already", body = Vec<MerakiOrgOption>),
        (status = 400, description = "No key was named (`invalid_api_key`), both `api_key` and `credential_id` were sent (`invalid_request`), `credential_id` is not a stored Meraki API key (`invalid_credential`), or base_url is not an https allow-listed Meraki host", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig, or the account is restricted to folders (`scope_unsupported`)", body = super::error::ErrorBody),
        (status = 502, description = "The Dashboard API call failed; the detail is logged, never returned", body = super::error::ErrorBody),
        (status = 503, description = "Inventory storage is unavailable (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn meraki_discover(
    _guard: RequireManageConfig,
    Scoped(scope): Scoped,
    admin: Admin,
    Json(body): Json<MerakiDiscoverReq>,
) -> ApiResult<Json<Vec<MerakiOrgOption>>> {
    meraki_is_deployment_wide(&scope)?;
    let source = key_source(body.api_key.as_deref(), body.credential_id)?;
    // The allow-list runs before the key is resolved, and long before it is sent anywhere.
    let base = meraki_base_url(body.base_url)?;
    let key = resolve_key(&admin, &source).await?;
    let seen = admin
        .meraki_sync
        .directory()
        .organizations(&base, &key)
        .await
        .map_err(|e| meraki_upstream_error("discover organizations", &e))?;
    Ok(Json(org_options(seen, &onboarded_org_ids(&admin).await?)))
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
    /// The switch-port tier's interval (seconds) — every switch port's status, speed and traffic.
    switch_ports_secs: u32,
    /// The wireless tier's interval (seconds) — every access point's clients and each radio's
    /// channel utilization (ADR-168). The SSIDs and radio settings are read every twenty minutes
    /// whatever this says.
    wireless_secs: u32,
    enabled_tiers: Vec<String>,
    /// Requests per second this organization may be sent, **in total** (ADR-169). Its collects run
    /// in two lanes that can be asking at once — a fast one for availability, uplink, traffic and a
    /// wireless round, a slow one for the switch ports, the SSID read and the inventory sync
    /// (periodic, or asked for by "Sync now") — and each paces at half of this. The one exception is
    /// the LAN reads of an organization with no node yet, which take all of it: nothing is
    /// collected for it, so the fast lane is idle. Below 0.2
    /// each lane stops at 0.1, the slowest a session paces, so the two together can then send up
    /// to 0.2 whatever this says.
    target_rps: f64,
    group_id: Option<Uuid>,
    /// Which stored credential holds this organization's API key. An id, not a secret — the key
    /// stays sealed in `credentials`, and this type has no field that could carry it. Its *name* is
    /// `GET /credentials`'s to give, to a caller holding `ManageCredentials`.
    credential_id: Uuid,
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
    /// The most nodes automatic import lets this organization hold. A new organization starts at
    /// 10,000; one added by an earlier version keeps the 1,000 it started with.
    max_devices: u32,
    /// How many devices that cap left out on the last sync; zero while automatic import is off.
    devices_over_cap: u32,
    /// The collect tiers the Dashboard API is **not answering** right now, each with why and since
    /// when. Empty when none is known to be failing.
    ///
    /// Distinct from `last_sync_error`, which is the inventory sync — this server asking what the
    /// organization holds. A collect is a poller asking how the devices are, and it is what a
    /// device's state depends on: while `availability` is listed here the organization's nodes keep
    /// the last state they had, and after three failures in a row one alert is raised about the
    /// organization (`subject_kind: meraki_org`) — never one per device. `uplink`, `wireless`,
    /// `switch_ports` or `traffic` listed alone raises nothing: readings are missing, liveness is not.
    collect_failures: Vec<MerakiCollectFailureView>,
    /// A whole-organization read asked for or running (ADR-164 決定 30〜32); `null` when there is
    /// none. "Sync now" asks for one; an organization's first sync is one nobody asked for.
    full_sync: Option<MerakiFullSyncView>,
}

/// A whole-organization read: every MX network's LAN side, then the import (ADR-164 決定 30〜32).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub(crate) struct MerakiFullSyncView {
    /// When "Sync now" asked for it; `null` for a read nobody asked for — an organization's first,
    /// or the one a sync makes when it finds networks it has never read.
    requested_at: Option<chrono::DateTime<chrono::Utc>>,
    /// When it began; `null` while it waits for the organization's slow collect lane.
    started_at: Option<chrono::DateTime<chrono::Utc>>,
    /// How many networks' LAN sides it reads; `null` until it has begun.
    networks: Option<u32>,
    /// How many of those it has asked so far — a network whose read failed counts; `null` until
    /// it has begun.
    read: Option<u32>,
}

impl MerakiFullSyncView {
    /// The organization's read, if one is asked for or running.
    fn of(o: &crate::meraki::MerakiOrg) -> Option<Self> {
        if o.full_sync_requested_at.is_none() && o.full_sync.is_none() {
            return None;
        }
        Some(Self {
            requested_at: o.full_sync_requested_at,
            started_at: o.full_sync.map(|p| p.started_at),
            networks: o.full_sync.map(|p| p.networks),
            read: o.full_sync.map(|p| p.read),
        })
    }
}

/// One collect tier the Dashboard API is not answering (see `MerakiOrgView.collect_failures`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub(crate) struct MerakiCollectFailureView {
    /// `availability`, `uplink`, `wireless`, `switch_ports` or `traffic`.
    tier: String,
    /// Why the most recent collect of this tier failed. The vocabulary of `last_sync_error`, plus
    /// `no_answer`: the collect was sent and nothing came back.
    reason: MerakiSyncFailure,
    /// When this run of failures began.
    since: chrono::DateTime<chrono::Utc>,
    /// How many collects in a row have failed.
    failures: u32,
    /// Which of the tier's reads failed, when one did while the others answered (ADR-164 決定 25):
    /// `uplinks_loss_and_latency`, `appliance_uplink_statuses`, `appliance_vpn_statuses`, … — the
    /// uplink tier reads three, the switch-port tier up to four (`switch_port_statuses`,
    /// `switch_port_usage`, `switch_port_topology`, `switch_port_config`), the wireless tier up to three
    /// (`wireless_clients`, `wireless_channel_utilization`, `wireless_ssid_statuses`). Absent when
    /// the whole collect failed, or a poller from before this reported it.
    #[serde(skip_serializing_if = "Option::is_none")]
    listing: Option<String>,
}

impl MerakiCollectFailureView {
    fn of(f: &crate::meraki_health::TierFailure) -> Self {
        Self {
            tier: f.tier.as_str().to_owned(),
            reason: f.reason,
            // Out of range only for a corrupt row; the epoch then, rather than a failed page.
            since: chrono::DateTime::from_timestamp_millis(f.since_unix_ms).unwrap_or_default(),
            failures: f.failures,
            listing: f.listing.map(|l| l.as_str().to_owned()),
        }
    }
}

/// Project a stored org into its API view.
///
/// It carries the credential's **id** since ADR-164 Inc.6, as `NetboxServerView` always has: two
/// organizations under one key have to be tellable from two under two, and "use a saved key" has to
/// be able to say which organizations a key already serves. (This doc used to say the view existed
/// to keep that reference off the wire. The reference was never the secret.)
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
        switch_ports_secs: o.switch_ports_secs,
        wireless_secs: o.wireless_secs,
        enabled_tiers: o.enabled_tiers.clone(),
        target_rps: o.target_rps,
        group_id: o.group_id,
        credential_id: o.credential_id,
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
        collect_failures: o
            .collect_failures
            .iter()
            .map(MerakiCollectFailureView::of)
            .collect(),
        full_sync: MerakiFullSyncView::of(o),
    }
}

#[utoipa::path(
    get, path = "/api/v1/meraki/orgs", tag = "meraki",
    responses(
        (status = 200, description = "Every onboarded organization, each with the id of the credential holding its key — never the key", body = Vec<MerakiOrgView>),
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
/// Both edges go through `meraki_org_view` rather than the stored row, so what an organization
/// says about itself is decided once: REST and `get_config kind=meraki_orgs` cannot disagree.
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
    /// A key typed by the operator; sealed as a new credential. Send this **or** `credential_id`.
    #[serde(default)]
    api_key: Option<String>,
    /// A `meraki_api` credential already stored here. The new organizations share it, and no new
    /// credential is created.
    #[serde(default)]
    credential_id: Option<Uuid>,
    #[serde(default)]
    base_url: Option<String>,
    org_ids: Vec<String>,
}

/// What an onboarding batch did.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct MerakiCreated {
    /// Organizations this request added.
    created: u32,
    /// Organizations it named that were monitored here already, and were left as they are.
    already_added: u32,
}

/// The requested organization ids that are not here yet, in the order asked, each once.
///
/// `meraki_orgs.org_id` is `UNIQUE`, so an id that is already onboarded could only ever fail its
/// insert. Taking those out *first* is what lets a request made only of them finish without sealing
/// a credential for nothing — which is what used to happen: adding the same organization twice left
/// a second `Meraki API — <name>` on the Credentials page, used by nobody.
fn not_yet_onboarded<'a>(
    requested: &'a [String],
    onboarded: &std::collections::HashSet<String>,
) -> Vec<&'a String> {
    let mut seen = std::collections::HashSet::new();
    requested
        .iter()
        .filter(|id| !onboarded.contains(*id) && seen.insert(id.as_str()))
        .collect()
}

/// Onboard one or more organizations under a single read-only API key.
///
/// The key is validated by listing orgs. A **typed** key is then sealed **once** as a shared
/// credential — each org row holds a reference plus its own org id, so onboarding twenty orgs
/// stores one secret, not twenty copies of the same one. A **saved** key (`credential_id`,
/// ADR-164 Inc.6) seals nothing: the new rows point at the credential that is already there, which
/// is how an organization is added later under a key nobody has to find and paste again.
///
/// An organization that is already monitored here is skipped and counted. A per-org create failure
/// is logged and skipped rather than failing the batch: the counts say what landed, and retrying is
/// harmless.
#[utoipa::path(
    post, path = "/api/v1/meraki/orgs", tag = "meraki",
    request_body = CreateMerakiOrgsReq,
    responses(
        (status = 201, description = "How many organizations the batch created, and how many it named were here already; a per-org failure is skipped, not fatal", body = MerakiCreated),
        (status = 400, description = "The org list is empty or both `api_key` and `credential_id` were sent (`invalid_request`), no key was named (`invalid_api_key`), `credential_id` is not a stored Meraki API key (`invalid_credential`), base_url is not an https allow-listed Meraki host, or an org id is not one the key can see (`unknown_org`)", body = super::error::ErrorBody),
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
    if body.org_ids.is_empty() {
        return Err(ApiError::bad_request(
            "invalid_request",
            "at least one org_id is required",
        ));
    }
    let source = key_source(body.api_key.as_deref(), body.credential_id)?;
    let base = meraki_base_url(body.base_url)?;
    let key = resolve_key(&admin, &source).await?;
    // Proves the key works before anything is stored, and supplies the org names.
    let orgs = admin
        .meraki_sync
        .directory()
        .organizations(&base, &key)
        .await
        .map_err(|e| meraki_upstream_error("validate key / list organizations", &e))?;

    let onboarded = onboarded_org_ids(&admin).await?;
    let fresh = not_yet_onboarded(&body.org_ids, &onboarded);
    let already_added = u32::try_from(
        body.org_ids
            .iter()
            .filter(|id| onboarded.contains(*id))
            .count(),
    )
    .unwrap_or(u32::MAX);
    if fresh.is_empty() {
        return Ok((
            StatusCode::CREATED,
            Json(MerakiCreated {
                created: 0,
                already_added,
            }),
        ));
    }

    // Only organizations this key can see (ADR-164 増分 18). One it cannot see used to be stored
    // under its own id as a name, and then every sync of it failed.
    if let Some(unseen) = fresh
        .iter()
        .find(|oid| !orgs.iter().any(|o| o.id.as_str() == oid.as_str()))
    {
        tracing::debug!(org = %unseen, "meraki: an organization the key cannot see was asked for");
        return Err(ApiError::bad_request(
            "unknown_org",
            "an organization id is not one this API key can see",
        ));
    }
    // The name each requested org goes by, falling back to its id exactly as the row below does.
    let org_name = |oid: &String| -> String {
        orgs.iter()
            .find(|o| &o.id == oid)
            .map_or_else(|| oid.clone(), |o| o.name.clone())
    };
    let names: Vec<String> = fresh.iter().map(|oid| org_name(oid)).collect();
    let (cred_id, sealed_here) = match source {
        KeySource::Saved(id) => (id, false),
        KeySource::Typed(_) => {
            let secret = serde_json::json!({ "api_key": key }).to_string();
            let cred_name =
                meraki_credential_name(&names.iter().map(String::as_str).collect::<Vec<_>>());
            let id = admin
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
            (id, true)
        }
    };
    let mut created = 0u32;
    for (oid, name) in fresh.iter().zip(&names) {
        match admin.meraki_orgs.create(oid, name, &base, cred_id).await {
            Ok(_) => created += 1,
            Err(e) => tracing::warn!(org = %oid, error = %e, "create meraki org failed (skipped)"),
        }
    }
    // A credential sealed a moment ago that ended up backing nothing is taken back out, so a
    // batch that failed whole does not leave a key on the Credentials page named after
    // organizations that are not here. Never a saved one: that was there before this request.
    if created == 0 && sealed_here {
        if let Err(e) = admin.creds.delete(cred_id).await {
            tracing::warn!(error = %e, "removing an unused meraki credential failed");
        }
    }
    Ok((
        StatusCode::CREATED,
        Json(MerakiCreated {
            created,
            already_added,
        }),
    ))
}

/// The not-found error every org-scoped endpoint answers with.
fn no_org(id: Uuid) -> ApiError {
    ApiError::not_found("meraki_org_not_found", format!("no meraki org {id}"))
}

/// Delete an organization, its folder tree, and every node filed beneath that tree (ADR-174).
///
/// "Every node" includes nodes Meraki did not import: the tree goes the way any folder deletion
/// does, so a device an operator filed under the organization's folders goes with it.
#[utoipa::path(
    delete, path = "/api/v1/meraki/orgs/{id}", tag = "meraki",
    params(("id" = Uuid, Path, description = "Organization row id")),
    responses(
        (status = 204, description = "Organization and its folder tree removed, with every node filed beneath that tree — including nodes Meraki did not import and sub-folders an operator added"),
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
    /// The switch-port tier's interval, 300–600 seconds. Optional: left out, the stored interval
    /// stays as it is, so a client written before the tier existed does not reset it.
    #[serde(default)]
    switch_ports_secs: Option<i32>,
    /// The wireless tier's interval, 300–600 seconds. Optional, for the same reason.
    #[serde(default)]
    wireless_secs: Option<i32>,
    enabled_tiers: Vec<String>,
    /// Requests per second this organization may be sent, **in total** (ADR-169). Its collects run
    /// in two lanes that can be asking at once — a fast one for availability, uplink, traffic and a
    /// wireless round, a slow one for the switch ports, the SSID read and the inventory sync
    /// (periodic, or asked for by "Sync now") — and each paces at half of this. The one exception is
    /// the LAN reads of an organization with no node yet, which take all of it: nothing is
    /// collected for it, so the fast lane is idle. Below 0.2 each lane stops at 0.1, the slowest a
    /// session paces, so the two together can then send up to 0.2 whatever this says.
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
        || body.switch_ports_secs.is_some_and(|v| {
            !in_range(
                v,
                MERAKI_SWITCH_PORTS_MIN_SECS,
                MERAKI_SWITCH_PORTS_MAX_SECS,
            )
        })
        || body
            .wireless_secs
            .is_some_and(|v| !in_range(v, MERAKI_WIRELESS_MIN_SECS, MERAKI_WIRELESS_MAX_SECS))
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
    // The availability tier is the only one that speaks for whether a device is up (ADR-164
    // Inc.3): uplink and traffic are observational. Without it every node of the organization
    // sits in `unknown` and node-down can never fire, with nothing on any screen saying why — so
    // leaving it out is refused rather than stored (決定 17).
    let availability = yagra_common::MerakiTier::Availability;
    if !body
        .enabled_tiers
        .iter()
        .any(|t| yagra_common::MerakiTier::from_token(t) == Some(availability))
    {
        return Err(ApiError::bad_request(
            "availability_required",
            "enabled_tiers must include availability: it is the only tier that says whether a \
             device is up",
        ));
    }
    Ok(())
}

/// Update an org's per-tier cadence, enabled tiers, and rate budget.
///
/// `enabled_tiers` must include `availability`. It is the one tier that decides whether a device
/// is up; the others only record readings, so an organization without it could raise no node-down
/// alert at all. `switch_ports_secs` and `wireless_secs` may be left out, and the stored interval
/// then stays.
#[utoipa::path(
    put, path = "/api/v1/meraki/orgs/{id}/cadence", tag = "meraki",
    params(("id" = Uuid, Path, description = "Organization row id")),
    request_body = MerakiCadenceReq,
    responses(
        (status = 204, description = "Cadence, enabled tiers and rate budget updated"),
        (status = 400, description = "A cadence value is outside its band (`switch_ports_secs` and `wireless_secs` 300–600), target_rps is outside the cap, a tier is unknown, or `enabled_tiers` leaves out `availability` (`availability_required`)", body = super::error::ErrorBody),
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
    let cadence = crate::meraki::MerakiCadence {
        availability_secs: body.availability_secs,
        uplink_secs: body.uplink_secs,
        traffic_secs: body.traffic_secs,
        inventory_secs: body.inventory_secs,
        switch_ports_secs: body.switch_ports_secs,
        wireless_secs: body.wireless_secs,
        enabled_tiers: body.enabled_tiers,
        target_rps: body.target_rps,
    };
    match admin.meraki_orgs.update_cadence(id, &cadence).await {
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
        (status = 404, description = "No such organization", body = super::error::ErrorBody),
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
    require_org(admin, org, "networks: load org").await?;
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

/// 404 unless organization `id` exists — what every `/meraki/orgs/{id}/…` route answers for one
/// that does not. The two networks routes used to answer `200 []` and `204` instead.
async fn require_org(admin: &super::AdminState, id: Uuid, what: &'static str) -> ApiResult<()> {
    admin
        .meraki_orgs
        .get(id)
        .await
        .map_err(|e| {
            ApiError::from_internal(e.as_ref(), what, "failed to load meraki organization")
        })?
        .map(|_| ())
        .ok_or_else(|| no_org(id))
}

#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct MerakiMonitoredReq {
    network_ids: Vec<String>,
    monitored: bool,
}

/// Set the monitored (watch/skip) flag for a set of the org's networks.
///
/// ⚠️ With the organization's automatic import on, watching a network makes the next sync import
/// every device in it that is not a node yet. Un-watching one stops collecting for the nodes in it:
/// they keep their last state until it is watched again.
#[utoipa::path(
    put, path = "/api/v1/meraki/orgs/{id}/networks", tag = "meraki",
    params(("id" = Uuid, Path, description = "Organization row id")),
    request_body = MerakiMonitoredReq,
    responses(
        (status = 204, description = "Network scope updated"),
        (status = 400, description = "`network_ids` is empty (`invalid_request`)", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig, or the account is restricted to folders (`scope_unsupported`)", body = super::error::ErrorBody),
        (status = 404, description = "No such organization", body = super::error::ErrorBody),
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
    if body.network_ids.is_empty() {
        return Err(ApiError::bad_request(
            "invalid_request",
            "at least one network id is required",
        ));
    }
    require_org(&admin, id, "networks: load org").await?;
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

/// Refuse a folder-scoped caller the device list (ADR-164 決定 9).
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

/// Ask for the whole organization to be read again now, rather than waiting for the periodic sync
/// (ADR-164 決定 32).
///
/// **Accepted, not run.** The read re-reads every MX network's VLANs one network at a time, which
/// takes minutes (about six for 350 networks at the default rate), so this records the request and
/// answers 202. The leader runs it once the organization's slow collect lane is free — never in the
/// fast lane, which is availability's — and the organization shows `full_sync` while it waits and
/// while it runs; how it ended is `last_sync_at` / `last_sync_ok` / `last_sync_error`, as for any
/// sync. Asking while one is already asked for, or while the one asked for is running, changes
/// nothing: it is the same request. Asked while a read nobody asked for is running — an
/// organization's first, or a sync that found networks never read — the request stands and runs
/// once that read has ended.
///
/// Read-only upstream: the three paged inventory listings, the VLANs of every MX network
/// (`appliance/vlans`, falling back to `appliance/singleLan`), and the MX uplink statuses (for each
/// warm-spare pair's configured roles). Then the devices the organization imports are imported.
/// While it runs, the organization's switch-port and SSID reads wait for the lane.
#[utoipa::path(
    post, path = "/api/v1/meraki/orgs/{id}/sync", tag = "meraki",
    params(("id" = Uuid, Path, description = "Organization row id")),
    responses(
        (status = 202, description = "The read is asked for — or already was. It runs in the background; watch `full_sync` on the organization", body = MerakiFullSyncView),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig, or the account is restricted to folders (`scope_unsupported`)", body = super::error::ErrorBody),
        (status = 404, description = "No such organization", body = super::error::ErrorBody),
        (status = 409, description = "Meraki polling is paused globally (`meraki_polling_paused`), or this organization is paused (`meraki_org_paused`)", body = super::error::ErrorBody),
        (status = 503, description = "Inventory storage is unavailable (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn sync_meraki_org(
    _guard: RequireManageConfig,
    Scoped(scope): Scoped,
    admin: Admin,
    Path(id): Path<Uuid>,
) -> ApiResult<(StatusCode, Json<MerakiFullSyncView>)> {
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
    let internal = |e: anyhow::Error| {
        ApiError::from_internal(
            e.as_ref(),
            "sync: request a full read",
            "failed to ask for the organization to be read",
        )
    };
    // Written rather than run: the read takes minutes and the leader's loop runs it, so any core may
    // take the request — which is why this is not leader-gated, unlike the sync it replaced.
    if !admin
        .meraki_orgs
        .request_full_sync(id)
        .await
        .map_err(internal)?
    {
        return Err(no_org(id));
    }
    let org = admin
        .meraki_orgs
        .get(id)
        .await
        .map_err(internal)?
        .ok_or_else(|| no_org(id))?;
    Ok((
        StatusCode::ACCEPTED,
        Json(MerakiFullSyncView::of(&org).unwrap_or_default()),
    ))
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
    /// The device's LAN address, when a usable one is known: the `lanIp` Meraki reports. An MX
    /// (`appliance`) reports none, so its address is one of its own VLAN IPs — never its WAN
    /// address. An IP another network of the organization also uses is skipped; of the rest, the
    /// lowest-numbered VLAN inside a folder's IP range, else the lowest-numbered — and when every IP
    /// is shared, the lowest-numbered of them all. `null` for an MX until its network's VLANs have
    /// been read (such an MX is not imported automatically until then), and for good for an MX whose
    /// network has no LAN side at all.
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
    /// An MX's configured warm-spare role (ADR-164 決定 26). Absent from the object — not `null` —
    /// for a single MX, a device that is not an MX, and one whose role the sync has not read yet.
    #[serde(skip_serializing_if = "Option::is_none")]
    ha_role: Option<yagra_common::MerakiHaRole>,
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

impl MerakiFilingView {
    /// An MX no import takes yet: its network's LAN side has not been read (ADR-164 決定 39).
    fn lan_pending() -> Self {
        Self {
            reason: FilingReason::LanPending,
            prefix: None,
            folders: None,
        }
    }
}

impl MerakiDeviceView {
    /// `filing` is what an import would do with the device; `None` for one that is already a node.
    /// An MX still waiting for its network's LAN side says so instead (決定 39), whatever the match
    /// made of the address it does not have yet.
    fn new(d: DeviceRecord, filing: Option<&Filing>) -> Self {
        let waiting = filing.is_some() && d.lan_pending;
        Self {
            folder_id: match filing {
                Some(_) if waiting => None,
                Some(f) => f.folder(),
                None => d.node_group_id,
            },
            filing: if waiting {
                Some(MerakiFilingView::lan_pending())
            } else {
                filing.map(MerakiFilingView::from)
            },
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
            ha_role: d.ha_role,
        }
    }
}

/// What a warm-spare pair is doing, seen from one of its MX (ADR-164 決定 26).
///
/// Worked out from the two devices' **liveness**, never from their roles: the role Meraki reports
/// is the configured one, and on a real organization a primary that was down still said `primary`
/// while its spare carried the traffic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MerakiPairState {
    /// Both are up.
    Normal,
    /// The primary is down and the spare is up — the site runs on its spare.
    RunningOnSpare,
    /// The spare is down while the primary is up — the site has lost its redundancy.
    SpareDown,
    /// Both are down.
    BothDown,
    /// Not known: the partner is not a node, the caller cannot see it, a state is neither up nor
    /// down (unknown, maintenance), or the two roles do not make a pair.
    Unknown,
}

/// The other MX of a pair, as the caller may see it.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub(crate) struct MerakiPartnerView {
    /// The name Meraki lists it under.
    pub(crate) name: String,
    /// Its configured role; `null` when none has been read.
    pub(crate) role: Option<yagra_common::MerakiHaRole>,
    /// Its node, when it has been imported.
    pub(crate) node_id: Option<Uuid>,
    /// Its node's displayed state; `null` without a node.
    pub(crate) node_state: Option<yagra_common::NodeState>,
}

/// One MX's warm-spare pair (ADR-164 決定 26) — `GET /api/v1/nodes/{node_id}`'s `meraki_pair`.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub(crate) struct MerakiPairView {
    /// This MX's configured role.
    pub(crate) role: yagra_common::MerakiHaRole,
    /// What the pair is doing.
    pub(crate) state: MerakiPairState,
    /// The other MX; `null` when there is none, it cannot be told apart, or it sits in a folder the
    /// caller cannot see — in which case `state` is `unknown` too.
    pub(crate) partner: Option<MerakiPartnerView>,
}

/// Up, down, or neither, as the pair verdict reads a node's displayed state.
fn up_of(state: yagra_common::NodeState) -> Option<bool> {
    use yagra_common::NodeState;
    match state {
        NodeState::Ok | NodeState::Warning | NodeState::Critical => Some(true),
        NodeState::Unreachable => Some(false),
        NodeState::Unknown | NodeState::Maintenance => None,
    }
}

/// What the pair is doing. Pure, so the table is tested without a database.
pub(crate) fn pair_state(
    own_role: yagra_common::MerakiHaRole,
    own: yagra_common::NodeState,
    partner: Option<(
        Option<yagra_common::MerakiHaRole>,
        Option<yagra_common::NodeState>,
    )>,
) -> MerakiPairState {
    use yagra_common::MerakiHaRole::{Primary, Spare};
    let Some((partner_role, Some(partner_state))) = partner else {
        return MerakiPairState::Unknown;
    };
    let (primary, spare) = match (own_role, partner_role) {
        (Primary, Some(Spare)) => (up_of(own), up_of(partner_state)),
        (Spare, Some(Primary)) => (up_of(partner_state), up_of(own)),
        (Primary | Spare, _) => return MerakiPairState::Unknown,
    };
    match (primary, spare) {
        (Some(true), Some(true)) => MerakiPairState::Normal,
        (Some(false), Some(true)) => MerakiPairState::RunningOnSpare,
        (Some(true), Some(false)) => MerakiPairState::SpareDown,
        (Some(false), Some(false)) => MerakiPairState::BothDown,
        _ => MerakiPairState::Unknown,
    }
}

/// `node`'s warm-spare pair, for the node detail and the MCP tool that folds it (ADR-164 決定 26).
/// `None` for a node that is not an MX in a pair. Best effort: a failed read reads as "no pair".
///
/// 🚨 The partner is narrowed to the caller's scope, as `wireless::node_wireless` narrows an AP's
/// controller: reading this node proves only that **this** node is visible, and the partner may sit
/// in a folder the caller cannot see. Then no partner is named and the state is `unknown`.
pub(crate) async fn node_meraki_pair(
    st: &ApiState,
    admin: &super::AdminState,
    scope: &super::scope::NodeScope,
    node: Uuid,
    binding: Option<&yagra_common::MerakiDeviceConfig>,
) -> Option<MerakiPairView> {
    let binding = binding.filter(|b| b.product_type == "appliance")?;
    let pair = match admin
        .meraki_inventory
        .ha_pair(binding.org_uuid, &binding.serial)
        .await
    {
        Ok(p) => p?,
        Err(e) => {
            tracing::warn!(node = %node, error = %e, "meraki pair read failed");
            return None;
        }
    };
    let own = super::nodes::display_state(st, yagra_common::NodeId::from(node)).await;
    let partner = match pair.partner {
        Some(p) => {
            let visible = p
                .node_id
                .is_none_or(|n| scope.allows_node(st, yagra_common::NodeId::from(n)));
            if visible {
                let node_state = match p.node_id {
                    Some(n) => {
                        Some(super::nodes::display_state(st, yagra_common::NodeId::from(n)).await)
                    }
                    None => None,
                };
                Some(MerakiPartnerView {
                    name: p.name,
                    role: p.role,
                    node_id: p.node_id,
                    node_state,
                })
            } else {
                None
            }
        }
        None => None,
    };
    let state = pair_state(
        pair.role,
        own,
        partner.as_ref().map(|p| (p.role, p.node_state)),
    );
    Some(MerakiPairView {
        role: pair.role,
        state,
        partner,
    })
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
    /// Networks to start watching along with the import — normally the ones `devices` are in.
    /// Collection asks the Dashboard about watched networks only, so a device imported from a
    /// network that stays unwatched becomes a node nothing is collected for. Absent or empty
    /// changes no network. ⚠️ With automatic import on, watching a network also makes the next
    /// sync import every other device in it.
    #[serde(default)]
    monitored_network_ids: Vec<String>,
    devices: Vec<MerakiImportDeviceReq>,
    /// File each device into the folder whose IP range holds its address, when exactly one does;
    /// false files every device under the organization's network folders. Absent means the
    /// organization's own `file_by_prefix` setting — what its page shows and the sync uses.
    #[serde(default)]
    file_by_prefix: Option<bool>,
}

/// One device to import. **Only `serial` is read** (ADR-164 決定 39): everything else about the
/// device — its name, model, network and address — is taken from what this organization's last
/// sync recorded, never from the request. A page opened before Meraki renamed a device used to
/// create the node under the old name, and the node then never followed a rename again. The other
/// fields are accepted and ignored, so a client that still sends them keeps working.
#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct MerakiImportDeviceReq {
    serial: String,
    /// Ignored; the inventory's name is used.
    #[serde(default)]
    #[allow(dead_code)]
    name: String,
    /// Ignored.
    #[serde(default)]
    #[allow(dead_code)]
    model: Option<String>,
    /// Ignored.
    #[serde(default)]
    #[allow(dead_code)]
    product_type: String,
    /// Ignored.
    #[serde(default)]
    #[allow(dead_code)]
    network_id: String,
    /// Ignored.
    #[serde(default)]
    #[allow(dead_code)]
    network_name: Option<String>,
    /// Ignored; the inventory's address is used.
    #[serde(default)]
    #[allow(dead_code)]
    lan_ip: Option<String>,
}

/// What an import created, and where it put it.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct MerakiImported {
    /// Devices that became nodes. A serial that already was one is not counted.
    imported: u32,
    /// How those devices were filed. The four add up to `imported`, except that all four are zero
    /// when filing by IP range was off for this import: the request's `file_by_prefix`, or the
    /// organization's own setting when the request leaves it out.
    filed: MerakiFiled,
    /// Whether any folder carries an IP range at all. False means `filed.unmatched` says nothing
    /// about the devices: there was nothing for an address to match.
    ranges_configured: bool,
    /// MX that were asked for and not imported, because their network's LAN side has not been read
    /// yet (ADR-164 決定 39) — their address, and so their folder, is not known. The next sync reads
    /// it; import them after that.
    waiting_lan: u32,
    /// Devices asked for that are already a node of **another** organization (the device was moved
    /// between organizations in Meraki). A serial is one node deployment-wide, so they were skipped.
    bound_elsewhere: u32,
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
        (status = 201, description = "How many devices became nodes and how they were filed; already-imported serials are skipped, and an MX whose network's LAN side has not been read yet is not imported (`waiting_lan`)", body = MerakiImported),
        (status = 400, description = "A serial this organization's inventory does not hold (`unknown_serial`)", body = super::error::ErrorBody),
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
    // What each device IS comes from the inventory this organization's sync keeps, by serial —
    // never from the request (ADR-164 決定 39). A serial it does not hold is refused: it is not a
    // device of this organization, or not one the sync has seen.
    let inventory: std::collections::HashMap<String, DeviceRecord> = admin
        .meraki_inventory
        .devices(org.id)
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "import: read inventory",
                "failed to import meraki devices",
            )
        })?
        .into_iter()
        .map(|d| (d.serial.clone(), d))
        .collect();
    let mut candidates: Vec<ImportCandidate> = Vec::with_capacity(body.devices.len());
    let mut waiting_lan = 0u32;
    for d in &body.devices {
        let Some(record) = inventory.get(&d.serial) else {
            return Err(ApiError::bad_request(
                "unknown_serial",
                "a serial is not a device of this organization's inventory",
            ));
        };
        // Automatic import waits for an MX's LAN side (決定 28); by hand it waits too (決定 39). An
        // import files a node once and never moves it, and without its LAN address an MX would be
        // filed under its network's folder for good.
        if record.node_id.is_none() && record.lan_pending {
            waiting_lan += 1;
            continue;
        }
        candidates.push(ImportCandidate::from(record));
    }
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
    // After the import, not before it (決定 39): with automatic import on, a watched network has its
    // every device imported by the next sync, and an import that then failed left the networks
    // watched with nothing the operator asked for done.
    if !body.monitored_network_ids.is_empty() {
        // 🚨 Not fatal, and not silent either. Collection asks the Dashboard about watched networks
        // only, so a network that fails to be watched here is a node that is collected nothing for
        // — the state `MerakiDeviceCounts.monitored_unwatched` and the page's own "N monitored
        // devices are in networks that are not watched" notice both exist to name (決定 15). That
        // notice is the recovery, which is why this does not fail the import: the devices really
        // were imported, and unwinding them would be worse than a request the operator repeats.
        // What it must not do is say nothing — `let _ =` left a whole organization uncollected
        // with not one line anywhere saying a write had failed.
        if let Err(e) = admin
            .meraki_orgs
            .set_networks_monitored(org.id, &body.monitored_network_ids, true)
            .await
        {
            tracing::warn!(
                org = %org.org_id,
                networks = body.monitored_network_ids.len(),
                error = %e,
                "meraki import: the networks to watch could not be stored; the imported devices are \
                 nodes nothing is collected for until they are watched"
            );
        }
    }
    Ok((
        StatusCode::CREATED,
        Json(MerakiImported {
            imported: outcome.imported,
            filed: outcome.filed,
            ranges_configured: resolved.ranges_configured,
            waiting_lan,
            bound_elsewhere: outcome.bound_elsewhere,
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

    /// One device as this organization's sync would have recorded it, in a network whose LAN side
    /// has been read (so an MX is not waiting for it). An import takes every fact about a device
    /// from here, never from the request (ADR-164 決定 39).
    async fn listed(
        pool: &sqlx::PgPool,
        org: Uuid,
        (serial, name, model, product_type, network, ip): (
            &str,
            &str,
            &str,
            &str,
            &str,
            Option<&str>,
        ),
    ) {
        sqlx::query(
            "INSERT INTO meraki_org_networks (org_id, network_id, name, lan_ips, lan_read_at) \
             VALUES ($1, $2, $2 || '-name', '{}', now()) ON CONFLICT (org_id, network_id) DO NOTHING",
        )
        .bind(org)
        .bind(network)
        .execute(pool)
        .await
        .expect("network row");
        sqlx::query(
            "INSERT INTO meraki_inventory \
                 (org_id, serial, name, model, product_type, network_id, lan_ip, first_online_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, now())",
        )
        .bind(org)
        .bind(serial)
        .bind(name)
        .bind(model)
        .bind(product_type)
        .bind(network)
        .bind(ip)
        .execute(pool)
        .await
        .expect("inventory row");
    }

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
            switch_ports_secs: None,
            wireless_secs: None,
            enabled_tiers: vec!["availability".to_owned()],
            target_rps: 1.0,
        };
        assert!(check_cadence(&ok()).is_ok());

        let mut fast = ok();
        fast.availability_secs = 1;
        assert_eq!(check_cadence(&fast).unwrap_err().code(), "invalid_cadence");

        // ADR-168 決定 9: the wireless band, likewise.
        for (secs, accepted) in [
            (crate::config::MERAKI_WIRELESS_MIN_SECS - 1, false),
            (crate::config::MERAKI_WIRELESS_MIN_SECS, true),
            (crate::config::MERAKI_WIRELESS_MAX_SECS, true),
            (crate::config::MERAKI_WIRELESS_MAX_SECS + 1, false),
        ] {
            let mut wireless = ok();
            wireless.wireless_secs = Some(secs);
            wireless.enabled_tiers = vec!["availability".to_owned(), "wireless".to_owned()];
            assert_eq!(check_cadence(&wireless).is_ok(), accepted, "{secs}");
        }

        // ADR-167 決定 11: the switch-port band, when the field is sent at all.
        for (secs, accepted) in [
            (crate::config::MERAKI_SWITCH_PORTS_MIN_SECS - 1, false),
            (crate::config::MERAKI_SWITCH_PORTS_MIN_SECS, true),
            (crate::config::MERAKI_SWITCH_PORTS_MAX_SECS, true),
            (crate::config::MERAKI_SWITCH_PORTS_MAX_SECS + 1, false),
        ] {
            let mut ports = ok();
            ports.switch_ports_secs = Some(secs);
            ports.enabled_tiers = vec!["availability".to_owned(), "switch_ports".to_owned()];
            assert_eq!(check_cadence(&ports).is_ok(), accepted, "{secs}");
        }

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
        tier.enabled_tiers = vec!["availability".to_owned(), "not-a-tier".to_owned()];
        assert_eq!(check_cadence(&tier).unwrap_err().code(), "invalid_tier");
    }

    /// 決定 17. Availability is the only tier that decides whether a device is up, so a request
    /// that leaves it out would store an organization whose nodes can never be reported down.
    #[test]
    fn a_cadence_that_leaves_out_the_availability_tier_is_refused() {
        let with = |tiers: &[&str]| MerakiCadenceReq {
            availability_secs: crate::config::MERAKI_FAST_MIN_SECS,
            uplink_secs: crate::config::MERAKI_FAST_MIN_SECS,
            traffic_secs: crate::config::MERAKI_TRAFFIC_MIN_SECS,
            inventory_secs: crate::config::MERAKI_INVENTORY_MIN_SECS,
            switch_ports_secs: None,
            wireless_secs: None,
            enabled_tiers: tiers.iter().map(|t| (*t).to_owned()).collect(),
            target_rps: 1.0,
        };
        for without in [&[][..], &["uplink"][..], &["uplink", "traffic"][..]] {
            assert_eq!(
                check_cadence(&with(without)).unwrap_err().code(),
                "availability_required",
                "{without:?} was accepted, and nothing in it says whether a device is up"
            );
        }
        for kept in [&["availability"][..], &["traffic", "availability"][..]] {
            assert!(
                check_cadence(&with(kept)).is_ok(),
                "{kept:?} carries availability and was refused"
            );
        }
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
        assert_eq!(declared_number(&text, path, "MAX_DEVICES_MIN"), 1);
        assert_eq!(
            declared_number(&text, path, "MAX_DEVICES_MAX"),
            crate::config::MERAKI_MAX_DEVICES_HARD
        );
    }

    /// The number a TypeScript module declares as `export const NAME = <digits>;`. Panics naming the
    /// constant when it is not there or not a plain number: a reader that stopped finding what it
    /// reads must not pass for one that found nothing wrong.
    fn declared_number(text: &str, path: &str, name: &str) -> i32 {
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
    }

    /// The quoted words of one `export const NAME = [ … ] as const;` array in a WebUI file, in
    /// order. Panics when the array is not there, for the reason [`declared_number`] does.
    fn declared_words(text: &str, path: &str, name: &str) -> Vec<String> {
        let declared = format!("export const {name} = [");
        let rest = text
            .split_once(declared.as_str())
            .unwrap_or_else(|| panic!("{name} is not declared in {path}"))
            .1;
        let body = rest
            .split_once(']')
            .unwrap_or_else(|| panic!("{name} in {path} does not close"))
            .0;
        body.split('\'')
            .skip(1)
            .step_by(2)
            .map(str::to_owned)
            .collect()
    }

    /// The Meraki tiers are a bare string in the API, so nothing generated carries the set to the
    /// WebUI: `merakiTiers.ts` labels every stored token and `merakiCadence.ts` draws one interval
    /// per tier, each hand-written. A seventh tier added here would otherwise be a raw key in the
    /// organization list and an interval the dialog cannot edit — so both lists are pinned to
    /// `MerakiTier::ALL`, in order.
    #[test]
    fn the_webuis_two_tier_lists_are_the_backends_tiers_in_order() {
        let backend: Vec<String> = yagra_common::MerakiTier::ALL
            .iter()
            .map(|t| t.as_str().to_owned())
            .collect();
        for (file, name) in [
            ("pages/merakiTiers.ts", "MERAKI_TIERS"),
            (
                "pages/integrations/merakiCadence.ts",
                "MERAKI_CADENCE_FIELDS",
            ),
        ] {
            let path = format!("{}/../../web/src/{file}", env!("CARGO_MANIFEST_DIR"));
            let text = std::fs::read_to_string(&path).expect("a WebUI tier list");
            assert_eq!(
                declared_words(&text, &path, name),
                backend,
                "{name} in {path} is not MerakiTier::ALL"
            );
        }
    }

    /// `merakiCard.ts::merakiUplinkState` reads a stored `meraki_uplink_status` back into a word —
    /// a second copy of `MerakiUplinkStatus::gauge`. Change one number here and the card would call
    /// a failed uplink "not connected" with nothing failing, so each word the card knows is pinned
    /// to the value the collector writes for it.
    #[test]
    fn the_cards_uplink_words_read_the_values_the_collector_writes() {
        use yagra_common::MerakiUplinkStatus as S;
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../web/src/components/NodeDetail/merakiCard.ts"
        );
        let text = std::fs::read_to_string(path).expect("the Meraki card's judgement module");
        let body = text
            .split_once("export function merakiUplinkState(")
            .expect("merakiUplinkState is declared")
            .1;
        let body = &body[..body.find("\n}\n").expect("merakiUplinkState closes")];
        // `case <number>:` then, on the next line, `return '<word>';`.
        let mut read: Vec<(f64, String)> = Vec::new();
        let mut lines = body.lines().map(str::trim);
        while let Some(line) = lines.next() {
            if let Some(n) = line.strip_prefix("case ").and_then(|l| l.strip_suffix(':')) {
                let word = lines
                    .next()
                    .and_then(|l| l.strip_prefix("return '"))
                    .and_then(|l| l.strip_suffix("';"))
                    .unwrap_or_else(|| panic!("case {n} in {path} returns no plain word"));
                read.push((n.parse().expect("a numeric case"), word.to_owned()));
            }
        }
        let expected = [
            (S::Active.gauge(), "active"),
            (S::Ready.gauge(), "ready"),
            (S::NotConnected.gauge(), "notConnected"),
            (S::Failed.gauge(), "failed"),
        ];
        assert_eq!(read.len(), expected.len(), "{read:?}");
        for (value, word) in expected {
            assert!(
                read.iter().any(|(v, w)| *v == value && w == word),
                "the card does not read {value} as {word}: {read:?}"
            );
        }
        // Connecting, and a word this build does not know, are written as not connected.
        assert_eq!(S::Connecting.gauge(), S::NotConnected.gauge());
        assert_eq!(S::Other.gauge(), S::NotConnected.gauge());
    }

    /// ADR-164 決定 26, as a table: what a warm-spare pair is doing, from either MX. The roles say
    /// which is which; only the two devices' liveness says what is happening — a down primary
    /// still reports `primary` (measured), so reading the role for this would say "normal".
    #[test]
    fn a_pairs_state_comes_from_both_devices_liveness_whichever_side_asks() {
        use yagra_common::MerakiHaRole::{Primary, Spare};
        use yagra_common::NodeState::{Critical, Maintenance, Ok, Unknown, Unreachable, Warning};
        use MerakiPairState as S;
        // (own role, own state, partner role, partner state) → verdict
        let table = [
            (Primary, Ok, Some(Spare), Some(Ok), S::Normal),
            (Primary, Warning, Some(Spare), Some(Critical), S::Normal),
            (
                Primary,
                Unreachable,
                Some(Spare),
                Some(Ok),
                S::RunningOnSpare,
            ),
            (
                Spare,
                Ok,
                Some(Primary),
                Some(Unreachable),
                S::RunningOnSpare,
            ),
            (Primary, Ok, Some(Spare), Some(Unreachable), S::SpareDown),
            (Spare, Unreachable, Some(Primary), Some(Ok), S::SpareDown),
            (
                Primary,
                Unreachable,
                Some(Spare),
                Some(Unreachable),
                S::BothDown,
            ),
            // Neither up nor down: not known.
            (Primary, Maintenance, Some(Spare), Some(Ok), S::Unknown),
            (Spare, Ok, Some(Primary), Some(Unknown), S::Unknown),
            // Not a pair: two primaries, or a partner with no role read yet.
            (Primary, Ok, Some(Primary), Some(Ok), S::Unknown),
            (Primary, Ok, None, Some(Ok), S::Unknown),
        ];
        for (own_role, own, partner_role, partner_state, want) in table {
            assert_eq!(
                pair_state(own_role, own, Some((partner_role, partner_state))),
                want,
                "{own_role:?} {own:?} / {partner_role:?} {partner_state:?}"
            );
        }
        // A partner with no node, or one the caller cannot see, is not known either.
        assert_eq!(
            pair_state(Primary, Ok, Some((Some(Spare), None))),
            S::Unknown
        );
        assert_eq!(pair_state(Primary, Ok, None), S::Unknown);
    }

    /// `MerakiCollectFailureView.listing` is a plain string on the wire, so the WebUI's list of the
    /// tokens it labels (`MERAKI_LISTINGS` in `web/src/types/api.ts`) is a hand-kept copy of
    /// `yagra_common::MerakiListing` that no generated type checks (ADR-164 決定 25). A token missing
    /// there is a failure line with no label; read from the file so the two cannot drift silently.
    #[test]
    fn every_listing_token_is_one_the_webui_lists() {
        let ts = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web/src/types/api.ts"),
        )
        .expect("web/src/types/api.ts");
        let start = ts
            .find("export const MERAKI_LISTINGS = [")
            .expect("MERAKI_LISTINGS is declared in types/api.ts");
        let block = &ts[start..];
        let block = &block[..block.find("] as const;").expect("the list is closed")];
        let listed: Vec<&str> = block.split('\'').skip(1).step_by(2).collect();
        let ours: Vec<&str> = yagra_common::MerakiListing::ALL
            .iter()
            .map(|l| l.as_str())
            .collect();
        assert_eq!(ours.len(), 12, "the listings a collect reads today");
        assert_eq!(
            listed, ours,
            "types/api.ts lists exactly these, in this order"
        );
    }

    /// The cadence dialog's ranges are the third copy of these bounds (after `config.rs` and the
    /// CHECKs in migrations 0038 and 0124), and they were four string literals in a `.tsx` until
    /// ADR-164 Inc.11 — one of them already moved by hand when the inventory floor went from 900
    /// to 60. A hint that disagrees sends the operator into `400 invalid_cadence`.
    #[test]
    fn the_cadence_bounds_the_webui_shows_are_the_ones_this_api_accepts() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../web/src/pages/integrations/merakiCadence.ts"
        );
        let text = std::fs::read_to_string(path).expect("the cadence dialog's bounds");
        for (name, accepted) in [
            ("CADENCE_FAST_MIN_SECS", crate::config::MERAKI_FAST_MIN_SECS),
            ("CADENCE_FAST_MAX_SECS", crate::config::MERAKI_FAST_MAX_SECS),
            (
                "CADENCE_TRAFFIC_MIN_SECS",
                crate::config::MERAKI_TRAFFIC_MIN_SECS,
            ),
            (
                "CADENCE_TRAFFIC_MAX_SECS",
                crate::config::MERAKI_TRAFFIC_MAX_SECS,
            ),
            (
                "CADENCE_INVENTORY_MIN_SECS",
                crate::config::MERAKI_INVENTORY_MIN_SECS,
            ),
            (
                "CADENCE_INVENTORY_MAX_SECS",
                crate::config::MERAKI_INVENTORY_MAX_SECS,
            ),
            (
                "CADENCE_SWITCH_PORTS_MIN_SECS",
                crate::config::MERAKI_SWITCH_PORTS_MIN_SECS,
            ),
            (
                "CADENCE_SWITCH_PORTS_MAX_SECS",
                crate::config::MERAKI_SWITCH_PORTS_MAX_SECS,
            ),
            (
                "CADENCE_WIRELESS_MIN_SECS",
                crate::config::MERAKI_WIRELESS_MIN_SECS,
            ),
            (
                "CADENCE_WIRELESS_MAX_SECS",
                crate::config::MERAKI_WIRELESS_MAX_SECS,
            ),
        ] {
            assert_eq!(
                declared_number(&text, path, name),
                accepted,
                "{name} in {path} is not what `check_cadence` accepts"
            );
        }
        // The rate box's ceiling too (ADR-164 増分 18): the dialog refuses a save past it.
        assert_eq!(
            f64::from(declared_number(&text, path, "CADENCE_TARGET_RPS_MAX")),
            crate::config::MERAKI_TARGET_RPS_MAX,
            "CADENCE_TARGET_RPS_MAX in {path} is not what `check_cadence` accepts"
        );
        // The edge really is held to those constants: one second outside each band is refused.
        let at = |availability: i32, traffic: i32, inventory: i32, ports: i32, wireless: i32| {
            MerakiCadenceReq {
                availability_secs: availability,
                uplink_secs: crate::config::MERAKI_FAST_MIN_SECS,
                traffic_secs: traffic,
                inventory_secs: inventory,
                switch_ports_secs: Some(ports),
                wireless_secs: Some(wireless),
                enabled_tiers: vec!["availability".to_owned()],
                target_rps: 1.0,
            }
        };
        let (fast, traffic, inventory, ports, wireless) = (
            crate::config::MERAKI_FAST_MAX_SECS,
            crate::config::MERAKI_TRAFFIC_MAX_SECS,
            crate::config::MERAKI_INVENTORY_MAX_SECS,
            crate::config::MERAKI_SWITCH_PORTS_MAX_SECS,
            crate::config::MERAKI_WIRELESS_MAX_SECS,
        );
        assert!(check_cadence(&at(fast, traffic, inventory, ports, wireless)).is_ok());
        for outside in [
            at(fast + 1, traffic, inventory, ports, wireless),
            at(fast, traffic + 1, inventory, ports, wireless),
            at(fast, traffic, inventory + 1, ports, wireless),
            at(fast, traffic, inventory, ports + 1, wireless),
            at(fast, traffic, inventory, ports, wireless + 1),
        ] {
            assert_eq!(
                check_cadence(&outside).unwrap_err().code(),
                "invalid_cadence"
            );
        }
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
                    "inventory_secs": 21600, "enabled_tiers": ["availability"], "target_rps": 1.0,
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

    /// "Sync now" is accepted — 202, the request on the organization's row, the same request however
    /// often it is pressed (ADR-164 決定 32) — and refused by each switch that means "send nothing to
    /// Meraki". The read the loop then runs answers the request and stamps the row. The device list
    /// answers from the database.
    ///
    /// ⚠️ The fixture's Dashboard is [`crate::api::tests_support::EmptyDashboard`], so this proves
    /// the endpoint — the request, the view, the two 409s, the stamp — and nothing about what a sync
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

        // Before any sync: not failed, not synced, no counts to show, nothing asked for.
        let (status, orgs) = send(&st, "GET", "/api/v1/meraki/orgs", &operator, None).await;
        assert_eq!(status, StatusCode::OK, "{orgs}");
        assert!(orgs[0]["last_sync_ok"].is_null(), "{orgs}");
        assert!(orgs[0]["last_sync_at"].is_null(), "{orgs}");
        assert!(orgs[0]["full_sync"].is_null(), "{orgs}");

        let (status, asked) = send(&st, "POST", &sync, &operator, None).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{asked}");
        assert!(asked["requested_at"].is_string(), "{asked}");
        assert!(asked["started_at"].is_null(), "{asked}");
        let (status, again) = send(&st, "POST", &sync, &operator, None).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{again}");
        assert_eq!(
            again["requested_at"], asked["requested_at"],
            "a second press made a second request"
        );
        let (_, orgs) = send(&st, "GET", "/api/v1/meraki/orgs", &operator, None).await;
        assert_eq!(orgs[0]["full_sync"], asked, "{orgs}");
        assert!(
            orgs[0]["last_sync_at"].is_null(),
            "the endpoint ran the sync itself: {orgs}"
        );

        // What the leader's loop then does with it.
        let row = admin.meraki_orgs.get(org).await.expect("get").expect("org");
        admin
            .meraki_sync
            .sync_org_requested(&row)
            .await
            .expect("the requested read");
        let (_, orgs) = send(&st, "GET", "/api/v1/meraki/orgs", &operator, None).await;
        assert!(
            orgs[0]["full_sync"].is_null(),
            "the request outlived its read: {orgs}"
        );
        assert_eq!(orgs[0]["last_sync_ok"], true, "{orgs}");
        assert!(orgs[0]["last_sync_at"].is_string(), "{orgs}");
        assert!(orgs[0]["last_sync_error"].is_null(), "{orgs}");
        // The whole object, so a count added later has to be decided about here too.
        assert_eq!(
            orgs[0]["devices"],
            serde_json::json!({
                "seen": 0, "monitored": 0, "new": 0, "missing": 0, "monitored_unwatched": 0
            })
        );
        // The id of the credential since Inc.6 — and nothing of what that credential seals.
        assert_eq!(
            orgs[0]["credential_id"],
            serde_json::json!(credential),
            "{orgs}"
        );
        assert!(
            !orgs.to_string().contains("not-a-real-key"),
            "the view carried the key itself: {orgs}"
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

    /// A warm-spare pair on the node detail and on the device list (ADR-164 決定 26): the partner is
    /// named to a caller who can see it, and withheld — with the state `unknown` — from one whose
    /// folders hold only this MX. Reading a node proves only that **this** node is visible.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_pairs_partner_is_named_only_to_a_caller_who_can_see_it(pool: sqlx::PgPool) {
        use crate::api::tests_support::{live_state, scoped_token, send, token};
        let st = live_state(pool.clone()).await;
        let admin = st.admin.clone().expect("live state");
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
        let site = crate::pgtest::group(&pool, "Site").await;
        let elsewhere = crate::pgtest::group(&pool, "Elsewhere").await;
        let primary = crate::pgtest::node(&pool, "mx-a", 1, Some(site)).await;
        let spare = crate::pgtest::node(&pool, "mx-b", 2, Some(elsewhere)).await;
        let single = crate::pgtest::node(&pool, "mx-c", 3, Some(site)).await;
        for (node, serial, name, network, role) in [
            (primary, "Q2-A", "mx-a-dashboard", "N_1", Some("primary")),
            (spare, "Q2-B", "mx-b-dashboard", "N_1", Some("spare")),
            (single, "Q2-C", "mx-c-dashboard", "N_2", None),
        ] {
            sqlx::query(
                "INSERT INTO meraki_inventory \
                     (org_id, serial, name, model, product_type, network_id, ha_role) \
                 VALUES ($1, $2, $3, 'MX85', 'appliance', $4, $5)",
            )
            .bind(org)
            .bind(serial)
            .bind(name)
            .bind(network)
            .bind(role)
            .execute(&pool)
            .await
            .expect("inventory row");
            sqlx::query(
                "INSERT INTO meraki_devices (node_id, org_id, serial, network_id, product_type, model) \
                 VALUES ($1, $2, $3, $4, 'appliance', 'MX85')",
            )
            .bind(node)
            .bind(org)
            .bind(serial)
            .bind(network)
            .execute(&pool)
            .await
            .expect("binding");
        }
        let detail = |node: Uuid| format!("/api/v1/nodes/{node}");
        let admin_token = token(&st, yagra_common::Role::Admin);

        let (status, body) = send(&st, "GET", &detail(primary), &admin_token, None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let pair = &body["meraki_pair"];
        assert_eq!(pair["role"], "primary", "{body}");
        assert_eq!(pair["partner"]["name"], "mx-b-dashboard", "{body}");
        assert_eq!(pair["partner"]["role"], "spare", "{body}");
        assert_eq!(pair["partner"]["node_id"], spare.to_string(), "{body}");
        // No poll has been judged here, so neither side is up or down yet.
        assert_eq!(pair["state"], "unknown", "{body}");
        // Either side can ask.
        let (_, body) = send(&st, "GET", &detail(spare), &admin_token, None).await;
        assert_eq!(body["meraki_pair"]["role"], "spare", "{body}");
        assert_eq!(
            body["meraki_pair"]["partner"]["name"], "mx-a-dashboard",
            "{body}"
        );

        // A caller who can see only the primary's folder: no partner, not even its name. Scope is
        // read from the alert engine's snapshot, which the config loader would have built.
        st.alerts.set_config(crate::alerts::AlertConfig::new(
            Vec::new(),
            [(primary, site), (spare, elsewhere), (single, site)]
                .into_iter()
                .map(|(node, group)| {
                    (
                        yagra_common::NodeId::from(node),
                        crate::alerts::NodeMeta {
                            folder_group: Some(group),
                            folder_chain: vec![group],
                            ..crate::alerts::NodeMeta::default()
                        },
                    )
                })
                .collect(),
        ));
        let scoped = scoped_token(&st, &[site]);
        let (status, body) = send(&st, "GET", &detail(primary), &scoped, None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["meraki_pair"]["role"], "primary", "{body}");
        assert_eq!(body["meraki_pair"]["state"], "unknown", "{body}");
        assert!(body["meraki_pair"]["partner"].is_null(), "{body}");
        assert!(
            !body.to_string().contains("mx-b-dashboard"),
            "the partner leaked to a caller who cannot see it: {body}"
        );

        // A single MX holds no role, so it has no pair at all.
        let (_, body) = send(&st, "GET", &detail(single), &admin_token, None).await;
        assert!(body["meraki_pair"].is_null(), "{body}");

        // The device list carries each MX's role, and nothing for the single one.
        let (status, list) = send(
            &st,
            "GET",
            &format!("/api/v1/meraki/orgs/{org}/devices"),
            &admin_token,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{list}");
        let role_of = |serial: &str| {
            list.as_array()
                .expect("a list")
                .iter()
                .find(|d| d["serial"] == serial)
                .map(|d| d.get("ha_role").cloned())
                .expect("listed")
        };
        assert_eq!(role_of("Q2-A"), Some(serde_json::json!("primary")));
        assert_eq!(role_of("Q2-B"), Some(serde_json::json!("spare")));
        assert_eq!(role_of("Q2-C"), None, "{list}");
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
        for (serial, ip) in [
            ("Q3-0", Some("10.1.0.4")),
            ("Q3-1", Some("10.1.0.5")),
            ("Q3-2", None),
            ("Q3-3", Some("0.0.0.0")),
            ("Q3-4", Some("10.1.0.6")),
        ] {
            listed(&pool, org, (serial, serial, "MR46", "wireless", "N_1", ip)).await;
        }
        // The request's own fields are ignored (決定 39): it sends a wrong address on purpose, and
        // each device is still filed by the address the inventory holds.
        let device = |serial: &str, _ip: Option<&str>| {
            serde_json::json!({
                "serial": serial, "name": "stale", "model": "MR46", "product_type": "wireless",
                "network_id": "N_1", "network_name": "One", "lan_ip": "192.0.2.200",
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

    /// 🚨 ADR-164 決定 39: a manual import takes everything about a device from the inventory, by
    /// serial. A page opened before Meraki renamed a device used to create the node under the old
    /// name — which then never followed a rename again (決定 14 follows only while the node still
    /// carries Meraki's name). A serial the organization does not hold is refused, and an MX whose
    /// network's LAN side has not been read waits, as automatic import does (決定 28).
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_manual_import_reads_the_inventory_and_waits_for_an_unread_mx(pool: sqlx::PgPool) {
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
        listed(
            &pool,
            org,
            (
                "Q4-AP",
                "ap-renamed",
                "MR46",
                "wireless",
                "N_1",
                Some("10.1.0.9"),
            ),
        )
        .await;
        // An MX in a network the sync has recorded but not read the LAN side of.
        sqlx::query(
            "INSERT INTO meraki_org_networks (org_id, network_id, name) VALUES ($1, 'N_2', 'Two')",
        )
        .bind(org)
        .execute(&pool)
        .await
        .expect("unread network");
        sqlx::query(
            "INSERT INTO meraki_inventory \
                 (org_id, serial, name, model, product_type, network_id, first_online_at) \
             VALUES ($1, 'Q4-MX', 'edge', 'MX85', 'appliance', 'N_2', now())",
        )
        .bind(org)
        .execute(&pool)
        .await
        .expect("unread mx");

        // The page's list says so before anything is pressed.
        let (_, list) = send(
            &st,
            "GET",
            &format!("/api/v1/meraki/orgs/{org}/devices"),
            &caller,
            None,
        )
        .await;
        let mx = list
            .as_array()
            .expect("a list")
            .iter()
            .find(|d| d["serial"] == "Q4-MX")
            .expect("listed")
            .clone();
        assert_eq!(mx["filing"]["reason"], "lan_pending", "{mx}");
        assert!(mx["folder_id"].is_null(), "{mx}");

        let (status, answer) = send(
            &st,
            "POST",
            "/api/v1/meraki/import",
            &caller,
            Some(serde_json::json!({
                "org_uuid": org,
                "devices": [
                    { "serial": "Q4-AP", "name": "ap-old-name", "lan_ip": "192.0.2.1",
                      "product_type": "wireless", "network_id": "N_1" },
                    { "serial": "Q4-MX", "product_type": "appliance", "network_id": "N_2" },
                ],
            })),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{answer}");
        assert_eq!(answer["imported"], 1, "{answer}");
        assert_eq!(answer["waiting_lan"], 1, "{answer}");
        let (name, address): (String, String) =
            sqlx::query_as("SELECT name, host(address) FROM nodes WHERE id = $1")
                .bind(crate::meraki::device_node_id("Q4-AP"))
                .fetch_one(&pool)
                .await
                .expect("the node");
        assert_eq!(
            (name.as_str(), address.as_str()),
            ("ap-renamed", "10.1.0.9"),
            "the request's stale name or address was used"
        );
        let mx_node: Option<Uuid> = sqlx::query_scalar("SELECT id FROM nodes WHERE id = $1")
            .bind(crate::meraki::device_node_id("Q4-MX"))
            .fetch_optional(&pool)
            .await
            .expect("query");
        assert_eq!(mx_node, None, "an MX with its LAN side unread was imported");

        let (status, answer) = send(
            &st,
            "POST",
            "/api/v1/meraki/import",
            &caller,
            Some(serde_json::json!({
                "org_uuid": org,
                "devices": [{ "serial": "Q4-NOT-HERE" }],
            })),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{answer}");
        assert_eq!(answer["error"]["code"], "unknown_serial", "{answer}");
    }

    /// 🚨 ADR-164 決定 40: a configuration bundle does not carry a node a Meraki organization owns.
    /// Its binding does not travel, so on the target it was an ordinary device, pinged and polled at
    /// its LAN address. The ordinary node beside it still travels.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_configuration_bundle_leaves_out_a_node_an_integration_owns(pool: sqlx::PgPool) {
        use crate::api::tests_support::live_state;
        let st = live_state(pool.clone()).await;
        let admin = st.admin.clone().expect("live state");
        let credential = crate::pgtest::credential(&pool, "meraki-key", "meraki_api").await;
        let org = admin
            .meraki_orgs
            .create("123456", "Acme", "https://api.meraki.com", credential)
            .await
            .expect("create org");
        let plain = crate::pgtest::node(&pool, "switch-1", 1, None).await;
        let owned = crate::pgtest::node(&pool, "mr-1", 2, None).await;
        sqlx::query(
            "INSERT INTO meraki_devices (node_id, org_id, serial, network_id, product_type) \
             VALUES ($1, $2, 'Q5-MR', 'N_1', 'wireless')",
        )
        .bind(owned)
        .bind(org)
        .execute(&pool)
        .await
        .expect("binding");

        let bundle = crate::config_bundle::ConfigBundleRepo::new(pool.clone())
            .export()
            .await
            .expect("export");
        let carried: Vec<Uuid> = bundle.nodes.iter().map(|n| n.id).collect();
        assert!(carried.contains(&plain), "the ordinary node was left out");
        assert!(
            !carried.contains(&owned),
            "the Meraki node travelled as an ordinary device"
        );
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

    /// 決定 18, through the router. While the Dashboard API is not answering an organization there is
    /// **one** alert, about the organization — and its nodes, whose state is now the last one
    /// collected, say so. Every surface a person reads has to agree: the alert list names the
    /// organization rather than an id, the node's status carries the fault and its reason, and the
    /// organization's row says which collect is failing. A caller restricted to the folder that
    /// holds one of its nodes sees the alert; one restricted elsewhere does not.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn an_unanswered_organization_is_one_alert_and_its_nodes_say_their_state_is_stale(
        pool: sqlx::PgPool,
    ) {
        use crate::api::tests_support::{live_state, scoped_token, send, token};
        use crate::meraki_health::TierFailure;
        use std::collections::HashMap;
        let st = live_state(pool.clone()).await;
        let admin = st.admin.clone().expect("live state");
        let credential = crate::pgtest::credential(&pool, "meraki-key", "meraki_api").await;
        let org = admin
            .meraki_orgs
            .create("123456", "Acme", "https://api.meraki.com", credential)
            .await
            .expect("create org");
        let operator = token(&st, yagra_common::Role::Operator);
        listed(
            &pool,
            org,
            (
                "Q2XX-0001",
                "edge-tokyo",
                "MX67",
                "appliance",
                "N_1",
                Some("10.1.0.1"),
            ),
        )
        .await;
        let (status, body) = send(
            &st,
            "POST",
            "/api/v1/meraki/import",
            &operator,
            Some(serde_json::json!({
                "org_uuid": org,
                "devices": [{ "serial": "Q2XX-0001" }],
            })),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        let node = crate::meraki::device_node_id("Q2XX-0001");
        let folder = crate::meraki::network_group_id(org, "N_1");

        // What the config loader builds from `alert_bindings` and the node list.
        let bindings = admin.meraki_orgs.alert_bindings().await.expect("bindings");
        assert_eq!(bindings, vec![(org, "Acme".to_owned(), vec![node])]);
        st.alerts.set_config(
            crate::alerts::AlertConfig::new(Vec::new(), HashMap::new()).with_meraki_orgs(
                HashMap::from([(
                    org,
                    crate::alerts::MerakiOrgScope {
                        name: "Acme".to_owned(),
                        groups: std::collections::BTreeSet::from([folder]),
                        nodes: std::collections::BTreeSet::from([yagra_common::NodeId::from(node)]),
                    },
                )]),
            ),
        );

        // Healthy: no fault, nothing failing.
        let (_, healthy) = send(
            &st,
            "GET",
            &format!("/api/v1/nodes/{node}/status"),
            &operator,
            None,
        )
        .await;
        assert!(healthy.get("collection_fault").is_none(), "{healthy}");

        // Three failed availability collects later: the loop raises the alert and writes the row.
        assert!(st
            .alerts
            .raise_meraki_collect_alert(org, 3, 1_790_000_000_000)
            .is_some());
        admin
            .meraki_orgs
            .record_collect_failures(
                org,
                &[TierFailure {
                    tier: yagra_common::MerakiTier::Availability,
                    reason: MerakiSyncFailure::Auth,
                    since_unix_ms: 1_789_999_100_000,
                    failures: 3,
                    listing: None,
                }],
            )
            .await
            .expect("row");

        let (status, alerts) = send(&st, "GET", "/api/v1/alerts", &operator, None).await;
        assert_eq!(status, StatusCode::OK, "{alerts}");
        assert_eq!(
            alerts.as_array().map(Vec::len),
            Some(1),
            "one alert, not one per node"
        );
        assert_eq!(alerts[0]["subject_kind"], "meraki_org", "{alerts}");
        assert_eq!(alerts[0]["subject_name"], "Acme", "{alerts}");
        assert_eq!(alerts[0]["node"], format!("meraki_org:{org}"), "{alerts}");
        assert_eq!(alerts[0]["severity"], "critical", "{alerts}");

        let (_, stale) = send(
            &st,
            "GET",
            &format!("/api/v1/nodes/{node}/status"),
            &operator,
            None,
        )
        .await;
        let fault = &stale["collection_fault"];
        assert_eq!(fault["cause"], "meraki_api", "{stale}");
        assert_eq!(fault["meraki_org"], org.to_string(), "{stale}");
        assert_eq!(fault["meraki_org_name"], "Acme", "{stale}");
        assert_eq!(fault["reason"], "auth", "{stale}");
        assert_eq!(fault["since_unix_ms"], 1_790_000_000_000_i64, "{stale}");
        assert_eq!(
            stale["alerts"].as_array().map(Vec::len),
            Some(0),
            "the organization's alert was attributed to the node: {stale}"
        );

        let (_, orgs) = send(&st, "GET", "/api/v1/meraki/orgs", &operator, None).await;
        let failing = &orgs[0]["collect_failures"];
        assert_eq!(failing.as_array().map(Vec::len), Some(1), "{orgs}");
        assert_eq!(failing[0]["tier"], "availability", "{orgs}");
        assert_eq!(failing[0]["reason"], "auth", "{orgs}");
        assert_eq!(failing[0]["failures"], 3, "{orgs}");

        // Who sees it: the folder that holds one of its nodes, and nobody else's.
        let mine = scoped_token(&st, &[folder]);
        let (_, seen) = send(&st, "GET", "/api/v1/alerts", &mine, None).await;
        assert_eq!(seen.as_array().map(Vec::len), Some(1), "{seen}");
        let elsewhere = crate::pgtest::group(&pool, "Osaka").await;
        let theirs = scoped_token(&st, &[elsewhere]);
        let (_, hidden) = send(&st, "GET", "/api/v1/alerts", &theirs, None).await;
        assert_eq!(hidden.as_array().map(Vec::len), Some(0), "{hidden}");

        // Answered again: the alert goes, and so does the label.
        assert!(st.alerts.resolve_meraki_collect_alert(org).is_some());
        let (_, again) = send(
            &st,
            "GET",
            &format!("/api/v1/nodes/{node}/status"),
            &operator,
            None,
        )
        .await;
        assert!(again.get("collection_fault").is_none(), "{again}");
    }

    /// 決定 17, through the router: a cadence that keeps availability is stored, one that drops it
    /// is answered `400 availability_required` — and the refusal leaves the row as it was, so the
    /// organization does not end up with nothing that says whether its devices are up.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_cadence_is_stored_and_one_without_availability_is_refused(pool: sqlx::PgPool) {
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
        let path = format!("/api/v1/meraki/orgs/{org}/cadence");
        let cadence = |tiers: &[&str]| {
            serde_json::json!({
                "availability_secs": 120, "uplink_secs": 300, "traffic_secs": 1800,
                "inventory_secs": 600, "enabled_tiers": tiers, "target_rps": 1.0,
            })
        };

        let (status, body) = send(
            &st,
            "PUT",
            &path,
            &operator,
            Some(cadence(&["availability", "traffic"])),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
        let stored = admin.meraki_orgs.get(org).await.expect("get").expect("org");
        assert_eq!(stored.availability_secs, 120);
        assert_eq!(stored.enabled_tiers, vec!["availability", "traffic"]);
        assert_eq!(
            stored.switch_ports_secs, 300,
            "a request without the switch-port interval kept the stored one (ADR-167 決定 12)"
        );

        // ADR-167: the switch-port tier and its interval, accepted and stored.
        let mut ports = cadence(&["availability", "switch_ports"]);
        ports["switch_ports_secs"] = serde_json::json!(600);
        let (status, body) = send(&st, "PUT", &path, &operator, Some(ports)).await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
        let stored = admin.meraki_orgs.get(org).await.expect("get").expect("org");
        assert_eq!(stored.switch_ports_secs, 600);
        assert_eq!(stored.enabled_tiers, vec!["availability", "switch_ports"]);
        let view = send(&st, "GET", "/api/v1/meraki/orgs", &operator, None)
            .await
            .1;
        assert_eq!(view[0]["switch_ports_secs"], 600, "{view}");

        let mut fast = cadence(&["availability", "switch_ports"]);
        fast["switch_ports_secs"] = serde_json::json!(60);
        let (status, body) = send(&st, "PUT", &path, &operator, Some(fast)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(body["error"]["code"], "invalid_cadence", "{body}");

        // ADR-168: the wireless tier and its interval, accepted and stored — and an interval left
        // out keeps the stored one, as the switch ports' does.
        assert_eq!(stored.wireless_secs, 300, "the column default");
        let mut wireless = cadence(&["availability", "wireless"]);
        wireless["wireless_secs"] = serde_json::json!(600);
        let (status, body) = send(&st, "PUT", &path, &operator, Some(wireless)).await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
        let stored = admin.meraki_orgs.get(org).await.expect("get").expect("org");
        assert_eq!(stored.wireless_secs, 600);
        assert_eq!(
            stored.switch_ports_secs, 600,
            "an omitted interval was reset"
        );
        assert_eq!(stored.enabled_tiers, vec!["availability", "wireless"]);
        let view = send(&st, "GET", "/api/v1/meraki/orgs", &operator, None)
            .await
            .1;
        assert_eq!(view[0]["wireless_secs"], 600, "{view}");
        let mut slow = cadence(&["availability", "wireless"]);
        slow["wireless_secs"] = serde_json::json!(601);
        let (status, body) = send(&st, "PUT", &path, &operator, Some(slow)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(body["error"]["code"], "invalid_cadence", "{body}");

        let (status, body) = send(
            &st,
            "PUT",
            &path,
            &operator,
            Some(cadence(&["availability", "traffic"])),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

        let (status, body) = send(
            &st,
            "PUT",
            &path,
            &operator,
            Some(cadence(&["uplink", "traffic"])),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(body["error"]["code"], "availability_required", "{body}");
        let stored = admin.meraki_orgs.get(org).await.expect("get").expect("org");
        assert_eq!(
            stored.enabled_tiers,
            vec!["availability", "traffic"],
            "a refused cadence was stored anyway"
        );
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
                    follows: Vec::new(),
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

    // ── adding an organization under a saved key (ADR-164 Inc.6) ───────────────────────────────

    #[test]
    fn an_onboarding_request_names_exactly_one_source_for_its_key() {
        let id = Uuid::from_u128(7);
        assert_eq!(
            key_source(Some(" typed "), None).expect("typed"),
            KeySource::Typed("typed".to_owned())
        );
        assert_eq!(
            key_source(None, Some(id)).expect("saved"),
            KeySource::Saved(id)
        );
        // A blank box beside a chosen credential is the dialog's resting state, not a second key.
        assert_eq!(
            key_source(Some("   "), Some(id)).expect("blank is absent"),
            KeySource::Saved(id)
        );
        assert_eq!(
            key_source(Some("typed"), Some(id)).unwrap_err().code(),
            "invalid_request",
            "both is refused, not resolved by precedence"
        );
        assert_eq!(
            key_source(None, None).unwrap_err().code(),
            "invalid_api_key"
        );
        assert_eq!(
            key_source(Some(""), None).unwrap_err().code(),
            "invalid_api_key"
        );
    }

    fn org_info(id: &str, name: &str) -> yagra_transport::MerakiOrgInfo {
        yagra_transport::MerakiOrgInfo {
            id: id.to_owned(),
            name: name.to_owned(),
            url: None,
        }
    }

    #[test]
    fn what_a_key_can_see_is_marked_with_what_is_already_here() {
        let onboarded = ["100".to_owned()].into_iter().collect();
        let options = org_options(
            vec![org_info("100", "Acme"), org_info("200", "Globex")],
            &onboarded,
        );
        assert_eq!(
            options,
            vec![
                MerakiOrgOption {
                    id: "100".to_owned(),
                    name: "Acme".to_owned(),
                    already_added: true,
                },
                MerakiOrgOption {
                    id: "200".to_owned(),
                    name: "Globex".to_owned(),
                    already_added: false,
                },
            ]
        );
    }

    #[test]
    fn only_an_organization_that_is_not_here_yet_is_created_and_only_once() {
        let onboarded = ["100".to_owned()].into_iter().collect();
        let requested: Vec<String> = ["300", "100", "200", "300"]
            .into_iter()
            .map(str::to_owned)
            .collect();
        let fresh: Vec<&str> = not_yet_onboarded(&requested, &onboarded)
            .into_iter()
            .map(String::as_str)
            .collect();
        assert_eq!(
            fresh,
            vec!["300", "200"],
            "asked order, each once, 100 left out"
        );
    }

    /// A Dashboard with two organizations that remembers every key it was handed.
    struct TwoOrgDashboard {
        keys: std::sync::Mutex<Vec<String>>,
    }

    impl TwoOrgDashboard {
        fn keys(&self) -> Vec<String> {
            self.keys.lock().expect("keys").clone()
        }
    }

    #[async_trait::async_trait]
    impl crate::meraki_sync::MerakiDirectory for TwoOrgDashboard {
        async fn organizations(
            &self,
            _base_url: &str,
            api_key: &str,
        ) -> Result<Vec<yagra_transport::MerakiOrgInfo>, yagra_transport::TransportError> {
            self.keys.lock().expect("keys").push(api_key.to_owned());
            Ok(vec![org_info("100", "Acme"), org_info("200", "Globex")])
        }

        async fn inventory(
            &self,
            _org: &crate::meraki::MerakiOrg,
            _api_key: &str,
        ) -> Result<yagra_transport::MerakiInventory, yagra_transport::MerakiFetchError> {
            Ok(yagra_transport::MerakiInventory::default())
        }

        async fn ha_roles(
            &self,
            _org: &crate::meraki::MerakiOrg,
            _api_key: &str,
        ) -> Result<
            Vec<(String, Option<yagra_common::MerakiHaRole>)>,
            yagra_transport::MerakiFetchError,
        > {
            Ok(Vec::new())
        }

        /// Never asked: the listing holds no MX.
        async fn network_lans(
            &self,
            _org: &crate::meraki::MerakiOrg,
            _api_key: &str,
            _network_ids: &[String],
            _budget: std::time::Duration,
            _rps: f64,
        ) -> Result<
            Vec<(String, yagra_transport::MerakiNetworkLan)>,
            yagra_transport::MerakiFetchError,
        > {
            Ok(Vec::new())
        }
    }

    /// The whole saved-key path, accepted (ADR-115): a typed key is sealed once, the organization
    /// says which credential holds it, and a second organization is added under that credential —
    /// with the saved key being what the Dashboard is handed, and no second credential sealed.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn an_organization_is_added_under_a_saved_key_without_sealing_another(
        pool: sqlx::PgPool,
    ) {
        use crate::api::tests_support::{live_state_with_dashboard, send, token};
        use serde_json::json;
        let dashboard = std::sync::Arc::new(TwoOrgDashboard {
            keys: std::sync::Mutex::new(Vec::new()),
        });
        let st = live_state_with_dashboard(pool.clone(), dashboard.clone()).await;
        let operator = token(&st, yagra_common::Role::Operator);
        let orgs_path = "/api/v1/meraki/orgs";
        let discover = "/api/v1/meraki/orgs/discover";

        // A typed key: sealed once.
        let (status, body) = send(
            &st,
            "POST",
            orgs_path,
            &operator,
            Some(json!({ "api_key": "typed-key", "org_ids": ["100"] })),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        assert_eq!(body, json!({ "created": 1, "already_added": 0 }));
        assert_eq!(crate::pgtest::rows(&pool, "credentials").await, 1);

        // The organization says which credential holds its key — the id, and nothing of the key.
        let (_, list) = send(&st, "GET", orgs_path, &operator, None).await;
        let credential = list[0]["credential_id"]
            .as_str()
            .unwrap_or_else(|| panic!("credential_id on the view: {list}"))
            .to_owned();
        assert!(!list.to_string().contains("typed-key"), "{list}");

        // Discover by that id: the saved key is what the Dashboard is handed.
        let (status, options) = send(
            &st,
            "POST",
            discover,
            &operator,
            Some(json!({ "credential_id": credential })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{options}");
        assert_eq!(
            options,
            json!([
                { "id": "100", "name": "Acme", "already_added": true },
                { "id": "200", "name": "Globex", "already_added": false },
            ])
        );
        assert_eq!(
            dashboard.keys(),
            vec!["typed-key", "typed-key"],
            "the second call carried the key that was unsealed, not one from the request"
        );

        // Add under the saved key: one new organization, one skipped, no second credential.
        let (status, body) = send(
            &st,
            "POST",
            orgs_path,
            &operator,
            Some(json!({ "credential_id": credential, "org_ids": ["100", "200"] })),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        assert_eq!(body, json!({ "created": 1, "already_added": 1 }));
        assert_eq!(crate::pgtest::rows(&pool, "meraki_orgs").await, 2);
        assert_eq!(
            crate::pgtest::rows(&pool, "credentials").await,
            1,
            "a saved key seals nothing"
        );
        let (_, list) = send(&st, "GET", orgs_path, &operator, None).await;
        assert_eq!(list[0]["credential_id"], list[1]["credential_id"], "{list}");
        assert_eq!(
            list[1]["name"], "Globex",
            "named from the Dashboard's answer"
        );

        // A request made only of organizations that are here: nothing is sealed for nothing.
        let (status, body) = send(
            &st,
            "POST",
            orgs_path,
            &operator,
            Some(json!({ "api_key": "another-key", "org_ids": ["100"] })),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        assert_eq!(body, json!({ "created": 0, "already_added": 1 }));
        assert_eq!(crate::pgtest::rows(&pool, "credentials").await, 1);

        // An organization the key cannot see is refused, not stored under its own id to fail every
        // sync after (ADR-164 増分 18).
        let (status, body) = send(
            &st,
            "POST",
            orgs_path,
            &operator,
            Some(json!({ "credential_id": credential, "org_ids": ["999"] })),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(body["error"]["code"], "unknown_org", "{body}");
        assert_eq!(crate::pgtest::rows(&pool, "meraki_orgs").await, 2);

        // The two networks routes answer an unknown organization the way their siblings do, and an
        // empty list of networks is a request that says nothing.
        let unknown = format!("/api/v1/meraki/orgs/{}/networks", Uuid::from_u128(0xDEAD));
        let (status, body) = send(&st, "GET", &unknown, &operator, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
        let (status, body) = send(
            &st,
            "PUT",
            &unknown,
            &operator,
            Some(json!({ "network_ids": ["N_1"], "monitored": true })),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
        let known = format!(
            "/api/v1/meraki/orgs/{}/networks",
            list[0]["id"].as_str().expect("id")
        );
        let (status, body) = send(
            &st,
            "PUT",
            &known,
            &operator,
            Some(json!({ "network_ids": [], "monitored": true })),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    }

    /// 🚨 The security half: a credential that is not a Meraki key is refused **before the Dashboard
    /// is asked anything**. Otherwise naming an SNMP community's id here would send that community
    /// to a server as a bearer key.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_credential_that_is_not_a_meraki_key_never_reaches_the_dashboard(pool: sqlx::PgPool) {
        use crate::api::tests_support::{live_state_with_dashboard, send, token};
        use serde_json::json;
        let dashboard = std::sync::Arc::new(TwoOrgDashboard {
            keys: std::sync::Mutex::new(Vec::new()),
        });
        let st = live_state_with_dashboard(pool.clone(), dashboard.clone()).await;
        let operator = token(&st, yagra_common::Role::Operator);
        // A community that happens to be spelled like a key document. An ordinary one would stop at
        // the key parser even with the kind check gone, and answer 500 — red for the wrong reason,
        // with the dangerous case never run. This one parses, so the kind check is the only thing
        // between it and the Dashboard.
        let community = st
            .admin
            .clone()
            .expect("live state")
            .creds
            .create(
                "lab community",
                "snmp_v2c",
                br#"{"api_key":"the-snmp-community"}"#,
            )
            .await
            .expect("seal");

        for (path, extra) in [
            ("/api/v1/meraki/orgs/discover", json!({})),
            ("/api/v1/meraki/orgs", json!({ "org_ids": ["100"] })),
        ] {
            for (what, id) in [("another kind", community), ("no such id", Uuid::new_v4())] {
                let mut body = extra.clone();
                body["credential_id"] = json!(id);
                let (status, answer) = send(&st, "POST", path, &operator, Some(body)).await;
                // First, because it is the point: whatever the status says, nothing left.
                assert_eq!(
                    dashboard.keys(),
                    Vec::<String>::new(),
                    "{what} {path}: a secret that is not a Meraki key was sent to the Dashboard"
                );
                assert_eq!(status, StatusCode::BAD_REQUEST, "{what} {path}: {answer}");
                assert_eq!(
                    answer["error"]["code"], "invalid_credential",
                    "{what} {path}: {answer}"
                );
            }
            // Both sources at once is the request's own contradiction.
            let mut both = extra.clone();
            both["credential_id"] = json!(community);
            both["api_key"] = json!("typed");
            let (status, answer) = send(&st, "POST", path, &operator, Some(both)).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{path}: {answer}");
            assert_eq!(
                answer["error"]["code"], "invalid_request",
                "{path}: {answer}"
            );
        }
        assert_eq!(
            dashboard.keys(),
            Vec::<String>::new(),
            "nothing was sent to the Dashboard on any of the six refusals"
        );
        assert_eq!(crate::pgtest::rows(&pool, "meraki_orgs").await, 0);
    }
}
