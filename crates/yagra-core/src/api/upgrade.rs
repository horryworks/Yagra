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

/// How long the fleet-wide maintenance window opened by an apply may last.
///
/// **Bounded before the run starts, not closed when it ends** (ADR-050 decision 12). core restarts
/// in the middle, so there is no process left to close it — and a run that dies partway must not
/// leave the whole fleet silent for good. The window therefore expires on its own.
///
/// ⚠️ Fifteen minutes is a placeholder with a real deadline attached: ADR-050 requires measuring
/// pull + backup + `up -d` on hardware before this number means anything, and in particular whether
/// it fits inside `YAGRA_POOL_COVERAGE_ALERT_AFTER_SECS` (300s). Too short and the upgrade alerts
/// on itself; too long and a genuine outage during it is invisible.
const MAINTENANCE_WINDOW_SECS: i64 = 900;

/// This domain's slice of the OpenAPI document (ADR-035), merged by [`super::openapi::document`].
#[derive(OpenApi)]
#[openapi(paths(get_upgrade, apply_upgrade, upload_bundle, set_upgrade_enabled))]
pub(super) struct Doc;

/// The upgrade routes, merged into `/api/v1` by [`super::router`].
pub(super) fn routes() -> Router<ApiState> {
    Router::new()
        .route(
            "/api/v1/system/upgrade",
            get(get_upgrade).post(apply_upgrade),
        )
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
    /// Whether the updater has ever reported. `false` means it is not deployed at all — the
    /// default, since the mechanism is opt-in.
    present: bool,
    /// Whether its last report is recent enough for it to be considered alive.
    // Reported separately from `present` because "never deployed" and "deployed but stopped" call
    // for different actions, and a single flag would force the UI to guess which it is looking at.
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
    /// Releases the updater last saw in the registry; `null` when it has never looked.
    available: Option<AvailableVersions>,
    /// The most recent run, finished or in flight; `null` when none has ever been requested.
    last_run: Option<RunStatus>,
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
    State(st): State<ApiState>,
) -> ApiResult<Json<UpgradeStatusResponse>> {
    upgrade_status(&upgrade, st.started)
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

/// The body of [`get_upgrade`], shared with `get_system_health(section="upgrade")`.
///
/// Split out rather than duplicated so the MCP surface answers with the REST route's own type —
/// which is what lets `mcp/folded.rs` check this branch's schema against the published contract
/// instead of against a hand-built canary (ADR-042).
pub(crate) async fn upgrade_status(
    upgrade: &crate::upgrade::UpgradeRepo,
    started: std::time::SystemTime,
) -> anyhow::Result<UpgradeStatusResponse> {
    let schema = upgrade.schema_state().await?;
    let switched_on = upgrade.enabled().await;
    let p = crate::support_bundle::provenance(started);
    let beat = upgrade.heartbeat();
    let now = super::util::now_unix_s();
    let fresh = beat.as_ref().is_some_and(|h| {
        crate::upgrade::heartbeat_is_fresh(h.written_at, h.check_interval_secs, now)
    });
    Ok(UpgradeStatusResponse {
        enabled: beat.is_some() && fresh && switched_on,
        upgrade_enabled: switched_on,
        updater: UpdaterInfo {
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
        schema,
        available: upgrade.available(),
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
        (status = 503, description = "The updater is not deployed, not running, or this core is not the leader", body = super::error::ErrorBody),
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
        (status = 503, description = "The updater is not deployed, not running, not the leader, or does not accept archives", body = super::error::ErrorBody),
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

/// Map a staging failure onto the status the UI branches on, keeping the I/O cause in the log.
fn bundle_error(e: BundleError) -> ApiError {
    match e {
        BundleError::TooLarge { cap } => too_large(cap),
        BundleError::Disabled => ApiError::unavailable(
            "upgrade_unavailable",
            "the upgrade mechanism is not enabled on this deployment",
        ),
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
            "the upgrade mechanism is not enabled on this deployment, or its updater is not running",
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
            "upgrade",
            chrono::Utc::now(),
            ends,
        )
        .await
        .map_err(|e| tracing::warn!(error = %e, "could not open the upgrade maintenance window"))
        .ok();

    upgrade.request(command, id, tag, by, now).map_err(|e| {
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
