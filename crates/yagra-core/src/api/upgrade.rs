// SPDX-License-Identifier: AGPL-3.0-only
//! What this deployment is running, and how far back it can be taken — Settings ▸ Upgrade (ADR-050).
//!
//! One read and two writes. The read answers three questions a closed-network operator otherwise
//! cannot ask at all: which binary is *actually* running, how much schema this database carries,
//! and whether returning to an earlier version is still possible. The writes hand a release to the
//! privileged updater — by tag from a registry, or as an uploaded archive for a site that can reach
//! no registry at all.
//!
//! **`ManageConfig`, not `View`, on the read.** The obvious reading is that a version number is a
//! health counter, and ADR-050 first said so. It is not: this answers with build provenance, the
//! registry the updater pulls from, resolved digests and the store images the target compose pins.
//! That is deployment configuration, which is exactly the argument `mcp/folded.rs` already records
//! for putting the `forwarding` section behind `ManageConfig` rather than `View`, and the same bar
//! `/settings/tls` sits at.
//!
//! **Provenance stops being bundle-only here.** `source_ref` and `build_profile` distinguish a
//! release build from a `/flashdeploy` build of the same commit, and until now the only way to read
//! them was a support bundle behind three permissions. A green pipeline has shipped a stale binary
//! in this repository before (ADR-036); the commit alone does not settle the question, so the
//! answer needs to be reachable without taking an archive. `GET /api/v1/version` keeps its
//! contract — public, one field — deliberately untouched.

use async_trait::async_trait;
use axum::{
    body::Body,
    extract::{DefaultBodyLimit, FromRequestParts, Query, State},
    http::{request::Parts, StatusCode},
    routing::{get, post},
    Json, Router,
};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use utoipa::OpenApi;

use super::error::{ApiError, ApiResult};
use super::extract::{
    Admin, Caller, Leader, RequireManageConfig, RequireManageCredentials, Upgrade,
};
use super::ApiState;
use crate::upgrade::{AvailableVersions, BundleError, Command, RunStatus, SchemaState};

use crate::upgrade::MAINTENANCE_WINDOW_SECS;

/// This domain's slice of the OpenAPI document (ADR-035), merged by [`super::openapi::document`].
#[derive(OpenApi)]
#[openapi(paths(
    get_upgrade,
    apply_upgrade,
    check_upgrades,
    upload_bundle,
    set_upgrade_enabled
))]
pub(super) struct Doc;

/// The upgrade routes, merged into `/api/v1` by [`super::router`].
pub(super) fn routes() -> Router<ApiState> {
    Router::new()
        .route(
            "/api/v1/system/upgrade",
            get(get_upgrade).post(apply_upgrade),
        )
        .route("/api/v1/system/upgrade/check", post(check_upgrades))
        .route(
            "/api/v1/system/upgrade/enabled",
            axum::routing::put(set_upgrade_enabled),
        )
        .merge(
            Router::new()
                .route("/api/v1/system/upgrade/bundle", post(upload_bundle))
                // Its own sub-router so the disabled limit reaches this route and nothing else.
                // The body is a multi-gigabyte image archive streamed straight to disk, so axum's
                // 2 MiB default would refuse it before the handler ran. The real bound is
                // `YAGRA_UPGRADE_BUNDLE_MAX_BYTES`, enforced chunk by chunk as the bytes land,
                // ahead of a free-space check made before any of them do.
                .layer(DefaultBodyLimit::disable()),
        )
}

/// The authorization both write paths require, as one extractor.
///
/// `ManageConfig` alone is not enough: an upgrade replaces the process that holds the KEK, so it is
/// restricted to someone already trusted with the credentials that process can decrypt — the same
/// argument ADR-045 used for the support bundle. `Leader` because two cores must not both rewrite
/// the compose project.
///
/// One type rather than three parameters repeated on each handler, so the rule and its **order**
/// are written once: permissions first, availability after, which is what keeps an unauthenticated
/// caller from learning whether this core is the leader.
pub(crate) struct UpgradeWrite;

#[async_trait]
impl FromRequestParts<ApiState> for UpgradeWrite {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, st: &ApiState) -> Result<Self, Self::Rejection> {
        RequireManageConfig::from_request_parts(parts, st).await?;
        RequireManageCredentials::from_request_parts(parts, st).await?;
        Leader::from_request_parts(parts, st).await?;
        Ok(Self)
    }
}

/// The authorization for asking the updater to re-read the registry.
///
/// Deliberately **weaker than [`UpgradeWrite`]**: no `ManageCredentials`. That permission is on the
/// write paths because an upgrade replaces the process holding the KEK, and none of that applies
/// here — this installs nothing, restarts nothing, and carries no argument. Picking the marker by
/// what the endpoint *is* (api-conventions.md), it is the read's own `ManageConfig`: it refreshes
/// the cache that `GET` serves, so anyone who can see the page can update what they are seeing.
///
/// `Leader` stays. Two cores sharing a hand-off volume must not both write a request file, and a
/// standby has no business talking to the registry on the deployment's behalf.
///
/// Today `ManageConfig` and `ManageCredentials` resolve to the same accounts, so this buys nothing
/// yet — it is what lets the two come apart later without silently taking the button with them.
pub(crate) struct UpgradeCheck;

#[async_trait]
impl FromRequestParts<ApiState> for UpgradeCheck {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, st: &ApiState) -> Result<Self, Self::Rejection> {
        RequireManageConfig::from_request_parts(parts, st).await?;
        Leader::from_request_parts(parts, st).await?;
        Ok(Self)
    }
}

/// Which binary is running.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct RunningBuild {
    /// The crate version this core was compiled from.
    core_version: String,
    /// The commit the image was built from (`/etc/yagra-source-ref`); `null` outside a container.
    source_ref: Option<String>,
    /// `release` or `ci-fast` (`/etc/yagra-build-profile`); `null` outside a container.
    // The pair matters together: a release and a flash build of the same commit are different
    // binaries sharing a source ref (ADR-036).
    build_profile: Option<String>,
    /// The container's hostname, which is how a replica identifies itself in the logs.
    hostname: Option<String>,
    /// Seconds since this process started.
    uptime_seconds: u64,
}

/// Whether the privileged updater container is deployed, and whether it is alive.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct UpdaterInfo {
    /// Whether this deployment has an upgrade mechanism at all — that is, whether the host wired
    /// the hand-off directory in. `false` describes how the deployment was installed rather than
    /// anything that went wrong: upgrading from the WebUI needs a container composition that
    /// carries the privileged updater alongside core, so a native installation, or a composition
    /// that does not ship it, cannot offer it. The command-line upgrade path is unaffected.
    installed: bool,
    /// Whether the updater has ever reported. Meaningful only where `installed` is true, and there
    /// it means the container is missing or has not finished starting — a fault, not a property of
    /// the deployment.
    present: bool,
    /// Whether its last report is recent enough for it to be considered alive.
    // `installed` → `present` → `fresh` narrow in that order, and each boundary is one the operator
    // acts on differently: nothing to run here / it should be running and is not / it ran and
    // stopped. Collapsing any pair would make the UI guess which of two situations it is in.
    fresh: bool,
    /// The image repository it is pinned to. Fixed by the host environment and not settable over
    /// this API.
    repo: Option<String>,
    /// Unix seconds of its last report.
    last_seen: Option<i64>,
    /// How often it re-checks the registry, in seconds.
    check_interval_secs: Option<u64>,
    /// Whether it will install an uploaded image archive. Off unless the host's compose file turns
    /// it on — it is the only path that can bring an image this deployment never named onto the
    /// host, so it is gated separately from the mechanism as a whole (ADR-050 Increment 3).
    allow_bundle: bool,
    /// The largest archive core will accept, in bytes. Present whenever `allow_bundle` is, so the
    /// UI can refuse an oversized file before spending an hour uploading it.
    bundle_max_bytes: Option<u64>,
    /// Whether the sidecar has seen the switch turned off. Distinct from `upgrade_enabled`, which
    /// is what this deployment *stored*: while the two disagree the change has not reached the
    /// sidecar yet, which takes at most one of its beats.
    paused: bool,
}

/// The state of the upgrade mechanism and of this deployment's schema.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct UpgradeStatusResponse {
    /// Whether an upgrade can be requested from here — the updater is deployed, alive, **and** the
    /// operator's switch is on. When `false`, this response describes only what is installed.
    enabled: bool,
    /// The operator's switch, as stored by this deployment. Separate from `enabled`, which also
    /// depends on the updater being alive: this one says what was *chosen*, and it is what the
    /// toggle in Settings ▸ Upgrade reflects.
    upgrade_enabled: bool,
    /// The updater container's own state.
    updater: UpdaterInfo,
    /// The running build.
    current: RunningBuild,
    /// Applied migrations and the compatibility floor they imply.
    schema: SchemaState,
    /// Releases the updater last saw in the registry; `null` when it has never looked. Carries the
    /// raw list and why it may be empty — `offers` is the part a page should render.
    available: Option<AvailableVersions>,
    /// The moves this deployment could make, each with its direction and whether it is allowed.
    /// Excludes the running version. Empty when the updater has found nothing.
    offers: Vec<crate::upgrade::ReleaseOffer>,
    /// The most recent run, finished or in flight; `null` when none has ever been requested.
    last_run: Option<RunStatus>,
    /// Which pollers this operation would replace and which it would leave on their current build
    /// (ADR-051). `null` when the updater has not reported the pollers in its own compose project —
    /// core cannot tell a co-located poller from a remote one on its own, and an invented answer
    /// would read as fact.
    pollers: Option<crate::upgrade::PollerUpgradePlan>,
}

/// A request to move this deployment to a particular release.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub(crate) struct ApplyUpgrade {
    /// The release tag to move to, e.g. `v0.2.2`. Must be a published release tag; the repository
    /// it is fetched from is fixed by the deployment and cannot be set here.
    target_tag: String,
}

/// The accepted run.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct RunAccepted {
    /// Correlation id for this run; it appears on the status the updater writes.
    id: String,
    /// The release the run targets.
    target_tag: String,
    /// The fleet-wide maintenance window opened for the duration, or `null` if one could not be
    /// opened (the run still proceeds — silencing is a courtesy, not a precondition).
    maintenance_window_id: Option<String>,
}

/// What this deployment is running, and how far back it can be taken.
#[utoipa::path(
    get, path = "/api/v1/system/upgrade", tag = "system",
    responses(
        (status = 200, description = "The running build, the applied schema, and the compatibility floor", body = UpgradeStatusResponse),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks the manage-configuration permission", body = super::error::ErrorBody),
        (status = 503, description = "This deployment has no metadata store", body = super::error::ErrorBody),
    ),
)]
async fn get_upgrade(
    _guard: RequireManageConfig,
    upgrade: Upgrade,
    admin: Admin,
    State(st): State<ApiState>,
) -> ApiResult<Json<UpgradeStatusResponse>> {
    upgrade_status(&upgrade, st.started, &poller_builds(&admin))
        .await
        .map(Json)
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "read the migration history",
                "failed to read the migration history",
            )
        })
}

/// The live poller fleet, reduced to what the upgrade view asks of it.
///
/// Live registry only: an upgrade acts on pollers that are running now, and a poller core has not
/// heard from cannot be handed a release whatever the durable inventory last recorded about it.
pub(crate) fn poller_builds(admin: &super::AdminState) -> Vec<crate::upgrade::PollerBuild> {
    let now = std::time::Instant::now();
    admin
        .coordinator
        .poller_views(now)
        .into_iter()
        .filter(|v| v.online)
        .map(|v| crate::upgrade::PollerBuild {
            self_upgrades: v.caps.iter().any(|c| c == yagra_bus::CAP_SELF_UPGRADE),
            version: (!v.version.is_empty()).then_some(v.version),
            id: v.id,
        })
        .collect()
}

/// The body of [`get_upgrade`], shared with `get_system_health(section="upgrade")`.
///
/// Split out rather than duplicated so the MCP surface answers with the REST route's own type —
/// which is what lets `mcp/folded.rs` check this branch's schema against the published contract
/// instead of against a hand-built canary (ADR-042).
pub(crate) async fn upgrade_status(
    upgrade: &crate::upgrade::UpgradeRepo,
    started: std::time::SystemTime,
    fleet: &[crate::upgrade::PollerBuild],
) -> anyhow::Result<UpgradeStatusResponse> {
    let schema = upgrade.schema_state().await?;
    let switched_on = upgrade.enabled().await;
    let available = upgrade.available();
    let p = crate::support_bundle::provenance(started);
    let beat = upgrade.heartbeat();
    let now = super::util::now_unix_s();
    let fresh = beat.as_ref().is_some_and(|h| {
        crate::upgrade::heartbeat_is_fresh(h.written_at, h.check_interval_secs, now)
    });
    // Who this operation would carry and who it would strand. `None` — not an empty plan — while the
    // updater has not named the pollers in its own project, because core has no other way to tell a
    // co-located poller from a remote one and a wrong answer here is worse than no answer.
    let plan = beat
        .as_ref()
        .and_then(|h| h.local_pollers.as_ref())
        .map(|local| crate::upgrade::poller_upgrade_plan(fleet, local));
    Ok(UpgradeStatusResponse {
        enabled: beat.is_some() && fresh && switched_on,
        upgrade_enabled: switched_on,
        updater: UpdaterInfo {
            installed: upgrade.installed(),
            present: beat.is_some(),
            fresh,
            repo: beat.as_ref().map(|h| h.repo.clone()),
            last_seen: beat.as_ref().map(|h| h.written_at),
            check_interval_secs: beat.as_ref().map(|h| h.check_interval_secs),
            allow_bundle: beat.as_ref().is_some_and(|h| h.allow_bundle),
            bundle_max_bytes: beat
                .as_ref()
                .filter(|h| h.allow_bundle)
                .map(|_| upgrade.bundle_max_bytes()),
            paused: beat.as_ref().is_some_and(|h| h.paused),
        },
        current: RunningBuild {
            core_version: p.core_version.to_owned(),
            source_ref: p.source_ref,
            build_profile: p.build_profile,
            hostname: p.hostname,
            uptime_seconds: p.uptime_seconds,
        },
        available: available.clone(),
        offers: crate::upgrade::release_offers(
            available.as_ref().map_or(&[], |a| a.releases.as_slice()),
            p.core_version,
            schema.compat.as_ref(),
            // Only pollers this operation leaves behind can be stranded on an unsupported version,
            // and an unreported project means an empty slice — the gate stays open rather than
            // guessing (see `PollerUpgradePlan`).
            &plan.as_ref().map_or_else(Vec::new, |p| {
                p.manual.iter().filter_map(|m| m.version.clone()).collect()
            }),
        ),
        pollers: plan,
        schema,
        last_run: upgrade.last_run(),
    })
}

/// Move this deployment to a different release.
///
/// Hands the request to the privileged updater container and returns immediately — the work
/// outlives this process, which restarts partway through. Poll `GET` for the outcome.
#[utoipa::path(
    post, path = "/api/v1/system/upgrade", tag = "system",
    request_body = ApplyUpgrade,
    responses(
        (status = 202, description = "Accepted; the updater will carry it out", body = RunAccepted),
        (status = 400, description = "Not a published release tag", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks manage-configuration or manage-credentials", body = super::error::ErrorBody),
        (status = 409, description = "A run is already in flight", body = super::error::ErrorBody),
        (status = 503, description = "This deployment has no updater (`upgrade_unsupported`), the mechanism is switched off (`upgrade_disabled`), the updater is not running (`upgrade_unavailable`), or this core is not the leader", body = super::error::ErrorBody),
    ),
)]
async fn apply_upgrade(
    _auth: UpgradeWrite,
    upgrade: Upgrade,
    admin: Admin,
    caller: Option<Caller>,
    Json(body): Json<ApplyUpgrade>,
) -> ApiResult<(StatusCode, Json<RunAccepted>)> {
    let tag = body.target_tag.trim();
    let now = super::util::now_unix_s();
    preflight(&upgrade, tag, now).await?;
    let by = caller.map_or_else(|| "unknown".to_owned(), |c| c.0.username.clone());
    let id = crate::upgrade::new_run_id();
    Ok((
        StatusCode::ACCEPTED,
        Json(dispatch(&upgrade, &admin, Command::Apply, &id, tag, &by, now).await?),
    ))
}

/// Ask the updater to re-read the registry's release list now.
///
/// The list is otherwise refreshed on the sidecar's own clock — every 24 hours by default — so the
/// hour a release is published is the hour the picker is guaranteed to be out of date. Accepted and
/// carried out asynchronously; the answer arrives as a newer `available.written_at` on `GET`.
///
/// Changes nothing about this deployment: no image is installed, no container restarts, and the
/// repository queried is fixed by the host, not by this request.
#[utoipa::path(
    post, path = "/api/v1/system/upgrade/check", tag = "system",
    responses(
        (status = 202, description = "Accepted; the updater will re-read the registry within a few seconds"),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks manage-configuration", body = super::error::ErrorBody),
        (status = 409, description = "An upgrade is already in flight", body = super::error::ErrorBody),
        (status = 503, description = "This deployment has no updater (`upgrade_unsupported`), the mechanism is switched off (`upgrade_disabled`), the updater is not running (`upgrade_unavailable`), or this core is not the leader", body = super::error::ErrorBody),
    ),
)]
async fn check_upgrades(_auth: UpgradeCheck, upgrade: Upgrade) -> ApiResult<StatusCode> {
    let now = super::util::now_unix_s();
    reachable(&upgrade, now).await?;
    // Not through `dispatch`: that opens the fleet-wide maintenance window first, because core is
    // about to stop. Nothing stops here — and a window opened for a refresh would never be closed,
    // since `settle_finished_run` waits on a `status.json` this command deliberately never writes.
    // The fleet would go silent for the 900-second bound over a button that reads a tag list.
    //
    // That is not left to discipline: `dispatch` needs `Admin` for the maintenance repo, and this
    // handler does not take one. Routing this through it means adding a parameter, which is a
    // decision rather than an oversight.
    upgrade
        .request(
            Command::Refresh,
            &crate::upgrade::new_run_id(),
            None,
            "",
            now,
        )
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "ask the updater to re-read the registry",
                "failed to ask the updater to re-read the registry",
            )
        })?;
    Ok(StatusCode::ACCEPTED)
}

/// Where an uploaded archive is going.
#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub(crate) struct BundleQuery {
    /// The release the archive contains, e.g. `v0.2.2`. The updater checks it against the images
    /// the archive actually carries, so a bundle cannot quietly install a different version.
    target_tag: String,
}

/// Install a release from an uploaded image archive, for a site with no reachable registry.
///
/// The body is the output of `docker save` for the release's three images. Nothing else about the
/// upgrade changes: the same backup is taken, the same composition is installed from the target
/// image, and the same provenance check decides whether it worked. Returns as soon as the archive
/// is stored; poll `GET /api/v1/system/upgrade` for the outcome.
///
/// Accepting archives is opt-in on the deployment and off by default, so this answers `503` until
/// an operator has turned it on at the host.
// Why it is separately gated: every other route here names a tag, and the repository it is fetched
// from is fixed by the host, so the set of images that can reach this machine is bounded. `docker
// load` installs whatever the archive carries. That is the only widening in the feature, hence its
// own environment variable rather than a ride on the sidecar being enabled at all.
#[utoipa::path(
    post, path = "/api/v1/system/upgrade/bundle", tag = "system",
    params(BundleQuery),
    request_body(
        content = Vec<u8>,
        description = "A `docker save` archive holding the core, poller and web images for the target release",
        content_type = "application/octet-stream",
    ),
    responses(
        (status = 202, description = "Archive stored; the updater will install it", body = RunAccepted),
        (status = 400, description = "Not a published release tag", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks manage-configuration or manage-credentials", body = super::error::ErrorBody),
        (status = 409, description = "A run is already in flight", body = super::error::ErrorBody),
        (status = 413, description = "The archive is larger than this deployment accepts", body = super::error::ErrorBody),
        (status = 503, description = "This deployment has no updater (`upgrade_unsupported`), the mechanism is switched off (`upgrade_disabled`), the updater is not running (`upgrade_unavailable`) or does not accept archives (`bundle_not_allowed`), or this core is not the leader", body = super::error::ErrorBody),
        (status = 507, description = "Not enough free space to store the archive and unpack it", body = super::error::ErrorBody),
    ),
)]
async fn upload_bundle(
    _auth: UpgradeWrite,
    upgrade: Upgrade,
    admin: Admin,
    caller: Option<Caller>,
    Query(q): Query<BundleQuery>,
    headers: axum::http::HeaderMap,
    // Last: it consumes the request, so nothing can be extracted after it.
    body: Body,
) -> ApiResult<(StatusCode, Json<RunAccepted>)> {
    let tag = q.target_tag.trim().to_owned();
    let now = super::util::now_unix_s();
    preflight(&upgrade, &tag, now).await?;
    if !upgrade.heartbeat().is_some_and(|h| h.allow_bundle) {
        return Err(ApiError::unavailable(
            "bundle_not_allowed",
            "this deployment does not accept uploaded image archives; set \
             YAGRA_UPGRADE_ALLOW_BUNDLE on the updater to enable it",
        ));
    }

    // Refuse on the declared length before a byte is written. Advisory — a chunked upload declares
    // nothing and the per-chunk cap is what actually holds — but when it is present it turns an
    // hour-long upload that was always going to be refused into an immediate answer.
    let cap = upgrade.bundle_max_bytes();
    let declared = headers
        .get(axum::http::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    if let Some(len) = declared {
        if len > cap {
            return Err(too_large(cap));
        }
        // A full Docker filesystem stops PostgreSQL, so this refusal protects the deployment rather
        // than the upload. Unmeasurable free space is not treated as no space.
        if let Some(free) = upgrade.free_bytes() {
            let need = crate::upgrade::bundle_space_needed(len);
            if free < need {
                tracing::warn!(
                    free,
                    need,
                    "refused an image archive for lack of disk space"
                );
                return Err(ApiError::insufficient_storage(
                    "insufficient_disk",
                    format!(
                        "this host has {} MiB free; storing the archive and unpacking it needs \
                         about {} MiB",
                        free / (1024 * 1024),
                        need / (1024 * 1024)
                    ),
                ));
            }
        }
    }

    // Whatever the last run left behind goes now: there is no run in flight (preflight said so),
    // so every archive still on disk is abandoned, and this is about to write another gigabyte.
    upgrade.prune_stale_bundles();

    let id = crate::upgrade::new_run_id();
    let mut sink = upgrade.begin_bundle(&id).await.map_err(bundle_error)?;
    let mut stream = body.into_data_stream();
    while let Some(chunk) = stream.next().await {
        // A dropped connection lands here. The partial file goes with `sink` (its `Drop`), so an
        // abandoned upload costs nothing beyond the transfer.
        let chunk = chunk.map_err(|e| {
            tracing::warn!(run = %id, error = %e, "image archive upload broke off");
            ApiError::bad_request("bundle_incomplete", "the upload did not complete")
        })?;
        sink.write(&chunk).await.map_err(bundle_error)?;
    }
    let bytes = sink.finish().await.map_err(bundle_error)?;

    let by = caller.map_or_else(|| "unknown".to_owned(), |c| c.0.username.clone());
    tracing::warn!(run = %id, target = %tag, bytes, by = %by, "image archive received");
    Ok((
        StatusCode::ACCEPTED,
        Json(dispatch(&upgrade, &admin, Command::Bundle, &id, &tag, &by, now).await?),
    ))
}

/// The operator's switch.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub(crate) struct UpgradeSwitch {
    /// Whether upgrading from the WebUI is permitted on this deployment.
    enabled: bool,
}

/// Turn upgrading from the WebUI on or off for this deployment.
///
/// The updater container ships with the deployment and holds the Docker socket; this decides
/// whether it may be asked to do anything. Off means no upgrade can be requested and the updater
/// stops contacting the registry — it does **not** remove the container or its socket, which is a
/// change to the composition.
///
/// Stored in the database, so it survives the upgrades it governs, and it is the only switch
/// reachable without a shell on the host.
// Deliberately narrower than it looks: this can withdraw a capability the host granted and restore
// it, never widen it. Whether an *uploaded* archive may be installed stays a host-side environment
// variable for that reason — see ADR-050 decision 1.
#[utoipa::path(
    put, path = "/api/v1/system/upgrade/enabled", tag = "system",
    request_body = UpgradeSwitch,
    responses(
        (status = 204, description = "Stored; the updater picks it up within one of its beats"),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks manage-configuration or manage-credentials", body = super::error::ErrorBody),
        (status = 503, description = "This deployment has no metadata store, or this core is not the leader", body = super::error::ErrorBody),
    ),
)]
async fn set_upgrade_enabled(
    _auth: UpgradeWrite,
    upgrade: Upgrade,
    Json(body): Json<UpgradeSwitch>,
) -> ApiResult<StatusCode> {
    upgrade.set_enabled(body.enabled).await.map_err(|e| {
        ApiError::from_internal(
            e.as_ref(),
            "store the upgrade switch",
            "failed to store the upgrade switch",
        )
    })?;
    // Worth a line of its own: `audit_mw` records that the endpoint was called, and this records
    // which way it went — the two together are what an operator needs after the fact.
    tracing::warn!(
        enabled = body.enabled,
        "upgrading from the WebUI was switched"
    );
    Ok(StatusCode::NO_CONTENT)
}

/// `413`, worded the same wherever it comes from.
fn too_large(cap: u64) -> ApiError {
    ApiError::payload_too_large(
        "bundle_too_large",
        format!(
            "the archive is larger than the {} MiB this deployment accepts; raise \
             YAGRA_UPGRADE_BUNDLE_MAX_BYTES if that is deliberate",
            cap / (1024 * 1024)
        ),
    )
}

/// `503` for a deployment that was never given an upgrade mechanism, worded the same wherever it
/// comes from.
///
/// Distinct from `upgrade_unavailable`, which means the mechanism exists and its updater is not
/// answering. The two call for opposite things — one is how the deployment was installed and the
/// operator should reach for the command line, the other is a container to go and look at — so a
/// client that branches on the code can say which.
fn unsupported() -> ApiError {
    ApiError::unavailable(
        "upgrade_unsupported",
        "this deployment cannot be upgraded from the WebUI: it was not installed with the \
         privileged updater alongside core. Upgrade it from the command line instead",
    )
}

/// Map a staging failure onto the status the UI branches on, keeping the I/O cause in the log.
fn bundle_error(e: BundleError) -> ApiError {
    match e {
        BundleError::TooLarge { cap } => too_large(cap),
        // Staging is refused for exactly one reason: there is no hand-off directory to stage into.
        // That is the deployment-shape case by construction, never a stopped sidecar.
        BundleError::Disabled => unsupported(),
        // Both of these are core's own doing — the id it minted, or a volume it cannot write.
        // Neither tells the client anything useful, and the I/O text must not reach it.
        BundleError::BadRunId | BundleError::Io(_) => ApiError::from_internal(
            &e,
            "stage the uploaded image archive",
            "failed to store the uploaded image archive",
        ),
    }
}

/// The checks both request paths share: a real tag, a live updater, nothing already running.
///
/// One function rather than two copies because the *order* is the security-relevant part — the tag
/// is validated before anything acts on it — and because a second copy is how one of them ends up
/// missing the 409 (extensibility.md §3).
async fn preflight(
    upgrade: &crate::upgrade::UpgradeRepo,
    tag: &str,
    now: i64,
) -> Result<(), ApiError> {
    if !crate::upgrade::is_valid_tag(tag) {
        return Err(ApiError::bad_request(
            "invalid_tag",
            "target_tag must be a published release tag such as v0.2.2",
        ));
    }
    reachable(upgrade, now).await
}

/// Is there a live, permitted updater with nothing already in flight?
///
/// The part of [`preflight`] that is not about a tag, so the check that names no release can share
/// it. Kept as one function for the reason `preflight` gives: a second copy is how one path ends up
/// missing the 409.
async fn reachable(upgrade: &crate::upgrade::UpgradeRepo, now: i64) -> Result<(), ApiError> {
    // First, because it outranks every check below it. The switch defaults to on, so a deployment
    // with no mechanism at all would otherwise fall through to `upgrade_unavailable` and be told
    // its updater is not running — naming a container it was never given, and sending the operator
    // to look for it.
    if !upgrade.installed() {
        return Err(unsupported());
    }
    // The operator's switch, checked here rather than only in the sidecar. The sidecar refuses too,
    // but that is defence in depth: this is the enforcement, because it is the only point that
    // cannot be reached by writing to the shared volume.
    if !upgrade.enabled().await {
        return Err(ApiError::unavailable(
            "upgrade_disabled",
            "upgrading from the WebUI is switched off for this deployment; turn it on in \
             Settings ▸ Upgrade",
        ));
    }
    let ready = upgrade.heartbeat().is_some_and(|h| {
        crate::upgrade::heartbeat_is_fresh(h.written_at, h.check_interval_secs, now)
    });
    if !ready {
        return Err(ApiError::unavailable(
            "upgrade_unavailable",
            "this deployment's updater is not running; check the container alongside core",
        ));
    }
    // One run at a time. The updater would serialize them anyway, but a 409 tells the operator why
    // their second click did nothing instead of silently queueing a second restart.
    if upgrade.last_run().is_some_and(|r| r.is_running()) {
        return Err(ApiError::conflict(
            "upgrade_in_progress",
            "an upgrade is already running",
        ));
    }
    Ok(())
}

/// Open the bounded maintenance window, then hand the request over.
///
/// Shared so the two commands cannot drift on the order. It matters: core stops moments after the
/// request file appears, so the window has to exist first or it never will.
///
/// Nothing here closes it — this process will not be alive to. The core that comes back does that
/// once the run reports an outcome ([`crate::upgrade::UpgradeRepo::settle_finished_run`]); the
/// `ends_at` written here is the backstop for the run that never reports at all.
async fn dispatch(
    upgrade: &crate::upgrade::UpgradeRepo,
    admin: &Admin,
    command: Command,
    id: &str,
    tag: &str,
    by: &str,
    now: i64,
) -> Result<RunAccepted, ApiError> {
    // Failure is not fatal: an upgrade that runs noisily is better than one that does not run.
    let ends = chrono::Utc::now() + chrono::Duration::seconds(MAINTENANCE_WINDOW_SECS);
    let window = admin
        .maintenance
        .create_window(
            &format!("Upgrade to {tag}"),
            crate::maintenance::WindowScope::System.as_str(),
            crate::maintenance::UPGRADE_SCOPE_ID,
            chrono::Utc::now(),
            ends,
        )
        .await
        .map_err(|e| tracing::warn!(error = %e, "could not open the upgrade maintenance window"))
        .ok();

    upgrade
        .request(command, id, Some(tag), by, now)
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "hand the upgrade request to the updater",
                "failed to hand the upgrade request to the updater",
            )
        })?;
    tracing::warn!(
        run = %id, target = tag, command = command.as_str(), by = %by,
        "upgrade requested; core will restart"
    );

    Ok(RunAccepted {
        id: id.to_owned(),
        target_tag: tag.to_owned(),
        maintenance_window_id: window.map(|w| w.to_string()),
    })
}

#[cfg(test)]
mod tests {
    use super::super::tests_support::{private_state, public_state};
    use super::ApiState;
    use crate::upgrade::BundleError;
    use axum::body::{to_bytes, Body};
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;
    use yagra_common::{Principal, Role, Scope};

    /// Every route this module serves, with a body that would be valid if the caller got that far.
    ///
    /// Listed rather than derived so a route added without a decision about who may reach it shows
    /// up as a missing row here — the whole surface is one permission pair, and the cost of getting
    /// that wrong on *this* feature is host root (ADR-050).
    const ROUTES: &[(&str, &str, &str)] = &[
        ("GET", "/api/v1/system/upgrade", ""),
        (
            "POST",
            "/api/v1/system/upgrade",
            r#"{"target_tag":"v0.2.2"}"#,
        ),
        ("POST", "/api/v1/system/upgrade/check", ""),
        (
            "POST",
            "/api/v1/system/upgrade/bundle?target_tag=v0.2.2",
            "not-really-a-tar",
        ),
        (
            "PUT",
            "/api/v1/system/upgrade/enabled",
            r#"{"enabled":false}"#,
        ),
    ];

    fn request(method: &str, uri: &str, body: &str, token: Option<&str>) -> Request<Body> {
        let mut b = Request::builder().uri(uri).method(method);
        if let Some(t) = token {
            b = b.header("authorization", format!("Bearer {t}"));
        }
        if !body.is_empty() {
            b = b.header("content-type", "application/json");
        }
        b.body(Body::from(body.to_owned())).expect("request")
    }

    async fn call(
        st: ApiState,
        method: &str,
        uri: &str,
        body: &str,
        token: Option<&str>,
    ) -> (StatusCode, serde_json::Value) {
        let res = super::super::router(st)
            .oneshot(request(method, uri, body, token))
            .await
            .expect("response");
        let status = res.status();
        let bytes = to_bytes(res.into_body(), 1 << 20).await.expect("body");
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
        )
    }

    fn token_for(st: &ApiState, role: Role) -> String {
        st.sessions
            .issue(uuid::Uuid::new_v4(), Principal::new(role, Scope::All), "u")
    }

    /// Authentication runs before the mechanism is looked at, on reads as well as writes.
    ///
    /// The read is included deliberately, and so is `public_state`: `public_dashboard` opens reads
    /// of *monitoring data*, and this read is not that — it answers with build provenance, the
    /// registry the updater pulls from, resolved digests and the images the target composition
    /// pins. An anonymous caller must learn only that it is unauthenticated, never whether this
    /// deployment has an updater installed.
    #[tokio::test]
    async fn every_upgrade_route_is_gated_before_the_mechanism_is_consulted() {
        for st in [private_state, public_state] {
            for (method, uri, body) in ROUTES {
                let (status, json) = call(st(), method, uri, body, None).await;
                assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {uri}");
                assert_eq!(json["error"]["code"], "unauthorized", "{method} {uri}");
            }
        }
    }

    /// 403, not 401 — the two have to stay distinguishable, or an operator cannot tell "sign in"
    /// from "your role cannot do this".
    #[tokio::test]
    async fn neither_viewer_nor_operator_may_reach_the_upgrade_surface() {
        for role in [Role::Viewer, Role::Operator] {
            for (method, uri, body) in ROUTES {
                let st = private_state();
                let token = token_for(&st, role);
                let (status, _) = call(st, method, uri, body, Some(&token)).await;
                assert_eq!(status, StatusCode::FORBIDDEN, "{role:?} on {method} {uri}");
            }
        }
    }

    /// The answer the 401s above must *not* have been.
    ///
    /// Skeleton mode carries no `UpgradeRepo`, so an authorized admin reaches the availability
    /// guard and gets the typed 503. Distinguishing this from the 401 is the whole point of the
    /// extractor ordering rule (`api-conventions.md`).
    #[tokio::test]
    async fn an_authorized_admin_gets_the_typed_unavailable_when_no_mechanism_is_installed() {
        for (method, uri, body) in ROUTES {
            let st = private_state();
            let token = token_for(&st, Role::Admin);
            let (status, json) = call(st, method, uri, body, Some(&token)).await;
            assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{method} {uri}");
            assert_eq!(json["error"]["code"], "admin_unavailable", "{method} {uri}");
        }
    }

    /// `Leader` sits inside `UpgradeWrite`, so it is checked *before* the mechanism is looked at.
    ///
    /// The two 503s carry different codes, which is what makes this observable: a standby answers
    /// `not_leader` where a leaderless-but-configured deployment would answer `admin_unavailable`.
    /// Reversing the order would make a standby report the deployment's configuration instead.
    #[tokio::test]
    async fn a_write_is_refused_for_leadership_before_the_mechanism_is_looked_at() {
        for (method, uri, body) in ROUTES.iter().filter(|(m, ..)| *m != "GET") {
            let st = private_state();
            st.is_leader
                .store(false, std::sync::atomic::Ordering::Release);
            let token = token_for(&st, Role::Admin);
            let (status, json) = call(st, method, uri, body, Some(&token)).await;
            assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{method} {uri}");
            assert_eq!(json["error"]["code"], "not_leader", "{method} {uri}");
        }
        // The read is not leader-gated: asking a standby what it is running is a fair question, and
        // refusing it would make the standby the harder of the two to diagnose.
        let st = private_state();
        st.is_leader
            .store(false, std::sync::atomic::Ordering::Release);
        let token = token_for(&st, Role::Admin);
        let (_, json) = call(st, "GET", "/api/v1/system/upgrade", "", Some(&token)).await;
        assert_ne!(json["error"]["code"], "not_leader");
    }

    /// Each staging failure maps to the status the page branches on.
    ///
    /// `Disabled` is the one worth pinning, and it now renders as `upgrade_unsupported` — not
    /// `upgrade_disabled`, and no longer `upgrade_unavailable` either. All three are 503 and all
    /// three mean something different to the operator: "this deployment has no updater at all" vs
    /// "there is one and you switched it off" vs "there is one and it stopped answering". The page
    /// offers a different next step for each, so collapsing any pair sends someone to look for a
    /// container that was never there.
    #[test]
    fn every_staging_failure_maps_to_the_status_the_page_branches_on() {
        let cases = [
            (
                BundleError::TooLarge { cap: 4 << 30 },
                StatusCode::PAYLOAD_TOO_LARGE,
                "bundle_too_large",
            ),
            (
                BundleError::Disabled,
                StatusCode::SERVICE_UNAVAILABLE,
                "upgrade_unsupported",
            ),
            (
                BundleError::BadRunId,
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
            ),
            (
                BundleError::Io(std::io::Error::other("no space left on device")),
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
            ),
        ];
        for (err, status, code) in cases {
            let mapped = super::bundle_error(err);
            assert_eq!(mapped.status(), status, "{code}");
            assert_eq!(mapped.code(), code);
        }
    }

    /// A refusal tells the operator the cap, and tells them nothing about the filesystem.
    ///
    /// The archive path is the one place in this feature where an OS error text is within reach of
    /// a response — `BundleError::Io` wraps a real `io::Error`, and its `Display` deliberately drops
    /// the cause. This is the assertion that keeps it dropped (security.md).
    #[test]
    fn a_refusal_names_the_cap_and_never_the_filesystem() {
        let cap = 4u64 << 30;
        let too_large = super::too_large(cap);
        assert!(
            too_large.message().contains("4096 MiB"),
            "the cap has to be in the message, or the operator cannot act on it: {}",
            too_large.message()
        );

        let leaked = "no space left on device";
        let io = super::bundle_error(BundleError::Io(std::io::Error::other(leaked)));
        assert!(
            !io.message().contains(leaked),
            "an OS error text reached the client: {}",
            io.message()
        );
    }

    /// A repo with no hand-off directory, for the two tests below.
    ///
    /// `connect_lazy` builds a handle without touching the network — but it does spawn the pool's
    /// reaper, so every caller has to be a `#[tokio::test]`. Nothing here reaches the database:
    /// `installed()` reads a field, and `reachable()` refuses before the one query it would
    /// otherwise run.
    fn repo(dir: Option<std::path::PathBuf>) -> crate::upgrade::UpgradeRepo {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://127.0.0.1:1/yagra-test-no-such-db")
            .expect("a lazy handle needs no server");
        crate::upgrade::UpgradeRepo::new(pool, dir, crate::upgrade::DEFAULT_BUNDLE_MAX_BYTES)
    }

    /// The hand-off directory is what says this deployment has a mechanism at all.
    #[tokio::test]
    async fn a_deployment_with_no_hand_off_directory_reports_no_mechanism() {
        assert!(!repo(None).installed());
        assert!(repo(Some(std::path::PathBuf::from("/data/upgrade"))).installed());
    }

    /// Not being installed outranks every other refusal, and says so in its own code.
    ///
    /// The ordering is the assertion, and this test can make it because of how the *other* checks
    /// fail here: with no database, `enabled()` fail-closes to `false`, so a `reachable()` that
    /// consulted the switch first would answer `upgrade_disabled`. Getting `upgrade_unsupported`
    /// is therefore proof the installed check ran ahead of it — which is what stops a deployment
    /// that was never given an updater being told to go and look at one.
    #[tokio::test]
    async fn a_deployment_with_no_mechanism_is_refused_as_unsupported_not_as_switched_off() {
        let err = super::reachable(&repo(None), 1_760_000_000)
            .await
            .expect_err("a deployment with no mechanism cannot be upgraded from here");
        assert_eq!(err.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(err.code(), "upgrade_unsupported");
        // The operator is pointed at the command line, not at a container that does not exist.
        assert!(
            !err.message().contains("not running"),
            "an uninstalled deployment must not be described as a stopped updater: {}",
            err.message()
        );
    }
}
