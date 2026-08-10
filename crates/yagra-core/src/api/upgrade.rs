// SPDX-License-Identifier: AGPL-3.0-only
//! What this deployment is running, and how far back it can be taken — Settings ▸ Upgrade (ADR-050).
//!
//! One endpoint so far. It answers three questions a closed-network operator currently cannot ask
//! at all: which binary is *actually* running, how much schema this database carries, and whether
//! returning to an earlier version is still possible.
//!
//! **`ManageConfig`, not `View`.** The obvious reading is that a version number is a health
//! counter, and ADR-050 first said so. It is not: this answers with build provenance and — once the
//! updater sidecar lands in Increment 1b — the registry it pulls from, resolved digests and the
//! store images the target compose pins. That is deployment configuration, which is exactly the
//! argument `mcp/folded.rs` already records for putting the `forwarding` section behind
//! `ManageConfig` rather than `View`, and the same bar `/settings/tls` sits at.
//!
//! **Provenance stops being bundle-only here.** `source_ref` and `build_profile` distinguish a
//! release build from a `/flashdeploy` build of the same commit, and until now the only way to read
//! them was a support bundle behind three permissions. A green pipeline has shipped a stale binary
//! in this repository before (ADR-036); the commit alone does not settle the question, so the
//! answer needs to be reachable without taking an archive. `GET /api/v1/version` keeps its
//! contract — public, one field — deliberately untouched.

use axum::{extract::State, http::StatusCode, routing::get, Json, Router};
use serde::{Deserialize, Serialize};
use utoipa::OpenApi;

use super::error::{ApiError, ApiResult};
use super::extract::{
    Admin, Caller, Leader, RequireManageConfig, RequireManageCredentials, Upgrade,
};
use super::ApiState;
use crate::upgrade::{AvailableVersions, RunStatus, SchemaState};

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
#[openapi(paths(get_upgrade, apply_upgrade))]
pub(super) struct Doc;

/// The upgrade routes, merged into `/api/v1` by [`super::router`].
pub(super) fn routes() -> Router<ApiState> {
    Router::new().route(
        "/api/v1/system/upgrade",
        get(get_upgrade).post(apply_upgrade),
    )
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
}

/// The state of the upgrade mechanism and of this deployment's schema.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct UpgradeStatusResponse {
    /// Whether an upgrade can be requested from here — the updater is deployed **and** alive.
    /// When `false`, this response describes only what is already installed.
    enabled: bool,
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
    let p = crate::support_bundle::provenance(started);
    let beat = upgrade.heartbeat();
    let now = super::util::now_unix_s();
    let fresh = beat.as_ref().is_some_and(|h| {
        crate::upgrade::heartbeat_is_fresh(h.written_at, h.check_interval_secs, now)
    });
    Ok(UpgradeStatusResponse {
        enabled: beat.is_some() && fresh,
        updater: UpdaterInfo {
            present: beat.is_some(),
            fresh,
            repo: beat.as_ref().map(|h| h.repo.clone()),
            last_seen: beat.as_ref().map(|h| h.written_at),
            check_interval_secs: beat.as_ref().map(|h| h.check_interval_secs),
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
    // ManageConfig alone is not enough: an upgrade replaces the process that holds the KEK, so it
    // is restricted to someone already trusted with the credentials it can decrypt — the same
    // argument ADR-045 used for the support bundle. `Leader` because two cores must not both
    // rewrite the compose project.
    _config: RequireManageConfig,
    _creds: RequireManageCredentials,
    _leader: Leader,
    upgrade: Upgrade,
    admin: Admin,
    caller: Option<Caller>,
    Json(body): Json<ApplyUpgrade>,
) -> ApiResult<(StatusCode, Json<RunAccepted>)> {
    let tag = body.target_tag.trim();
    if !crate::upgrade::is_valid_tag(tag) {
        return Err(ApiError::bad_request(
            "invalid_tag",
            "target_tag must be a published release tag such as v0.2.2",
        ));
    }
    let now = super::util::now_unix_s();
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

    let by = caller.map_or_else(|| "unknown".to_owned(), |c| c.0.username.clone());

    // Silence self-monitoring for a BOUNDED period, before the request is written — core is about
    // to stop, so this is the last moment anything can open it. Failure is not fatal: an upgrade
    // that runs noisily is better than one that does not run.
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

    let id = upgrade.request_apply(tag, &by, now).map_err(|e| {
        ApiError::from_internal(
            e.as_ref(),
            "hand the upgrade request to the updater",
            "failed to hand the upgrade request to the updater",
        )
    })?;
    tracing::warn!(run = %id, target = tag, by = %by, "upgrade requested; core will restart");

    Ok((
        StatusCode::ACCEPTED,
        Json(RunAccepted {
            id,
            target_tag: tag.to_owned(),
            maintenance_window_id: window.map(|w| w.to_string()),
        }),
    ))
}
