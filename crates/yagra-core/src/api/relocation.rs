// SPDX-License-Identifier: AGPL-3.0-only
//! Moving this whole deployment to another server — Settings ▸ Move to another server (ADR-121).
//!
//! Two reads and three writes over one thing: a **relocation archive** holding the KEK, the full
//! database, the metrics, optionally the event and flow stores, and this deployment's own `.env`
//! and composition. Either it is pushed over SSH to a bare Linux host and restored there, or it is
//! downloaded and carried by hand.
//!
//! ## Why this is the most heavily gated surface in the API
//!
//! 🚨 **The archive is every secret this deployment holds, in one file.** `security.md`'s first
//! rule is that secrets are never returned in an API response; this is its **one deliberate
//! exception**, and the exception is paid for here: `ManageSystem` + `ManageCredentials` +
//! `ViewAudit` + `Admin` + `Leader` on every write, and both the request and the download are
//! `POST` so `audit_mw` records them. A `GET` download would have been the obvious spelling and
//! would have left no audit row at all — the same reason the poller site bundle is a `POST`.
//!
//! The reads are `ManageSystem` only. They answer what the mechanism is doing, not what it holds:
//! a stage name, a message, a fingerprint, a size. Nothing sealed passes through them.
//!
//! ## What it refuses, and why the refusals are typed
//!
//! * 503 `relocation_unsupported` — the sidecar predates the feature and does not declare it. It
//!   must be a refusal rather than an attempt: an old sidecar answers an unknown command by
//!   writing a rejection into `status.json`, which is the *upgrade* page's file.
//! * 409 `relocation_in_progress` / `upgrade_in_progress` — both directions. The two mechanisms
//!   drive `docker compose` on the same deployment.
//! * 507 `insufficient_disk` — before anything is written. The sidecar checks again against the
//!   real volumes; this one exists so the operator is told before they wait.

use async_trait::async_trait;
use axum::{
    body::Body,
    extract::{FromRequestParts, Query},
    http::{
        header::{CONTENT_DISPOSITION, CONTENT_LENGTH, CONTENT_TYPE},
        request::Parts,
        StatusCode,
    },
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use utoipa::OpenApi;

use super::error::{ApiError, ApiResult};
use super::extract::{
    Admin, Caller, Leader, RequireManageCredentials, RequireManageSystem, RequireViewAudit, Upgrade,
};
use super::upgrade::UpdaterInfo;
use super::ApiState;
use crate::relocation::{
    RelocationMode, RelocationOptions, RelocationRun, SshAuthKind, SshSecrets, SshTarget,
};

/// This domain's slice of the OpenAPI document (ADR-035), merged by [`super::openapi::document`].
#[derive(OpenApi)]
#[openapi(paths(
    get_relocation,
    start_relocation,
    delete_relocation,
    get_relocation_log,
    download_archive
))]
pub(super) struct Doc;

/// The relocation routes, merged into `/api/v1` by [`super::router`].
pub(super) fn routes() -> Router<ApiState> {
    Router::new()
        .route(
            "/api/v1/system/relocation",
            get(get_relocation)
                .post(start_relocation)
                .delete(delete_relocation),
        )
        .route("/api/v1/system/relocation/log", get(get_relocation_log))
        .route("/api/v1/system/relocation/archive", post(download_archive))
}

/// The authorization every relocation write requires, as one extractor.
///
/// Three permissions, and each one is doing work rather than padding the list: `ManageSystem`
/// because this replaces a deployment, `ManageCredentials` because the artefact carries every
/// stored credential in openable form, and `ViewAudit` because the same conjunction already gates
/// the support bundle — the diagnostic archive that carries *less* than this one does.
///
/// `Leader`, for the reason [`super::upgrade::UpgradeWrite`] takes it: two cores sharing a hand-off
/// volume must not both write a request file.
///
/// The **order** is the contract — permissions first, availability after — so an unauthenticated
/// caller cannot learn from the status code whether this core is the leader, or whether the
/// deployment even has an updater.
pub(crate) struct RelocationWrite;

#[async_trait]
impl FromRequestParts<ApiState> for RelocationWrite {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, st: &ApiState) -> Result<Self, Self::Rejection> {
        RequireManageSystem::from_request_parts(parts, st).await?;
        RequireManageCredentials::from_request_parts(parts, st).await?;
        RequireViewAudit::from_request_parts(parts, st).await?;
        Leader::from_request_parts(parts, st).await?;
        Ok(Self)
    }
}

/// Where to send this deployment.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub(crate) struct TargetSpec {
    /// Host name or IP of the new server.
    host: String,
    /// SSH port. 22 unless the site moved it.
    #[serde(default = "default_port")]
    port: u16,
    /// The account to log in as. It needs `sudo` only when Docker has to be installed.
    user: String,
    /// A directory name — not a path — created under that account's home.
    #[serde(default = "default_dir")]
    dir: String,
}

fn default_port() -> u16 {
    22
}

fn default_dir() -> String {
    "yagra".to_owned()
}

/// How to authenticate to it. **Write-only**: nothing here is ever returned.
#[derive(Deserialize, utoipa::ToSchema)]
pub(crate) struct AuthSpec {
    /// `password` or `key`.
    kind: SshAuthKind,
    /// The password, or the OpenSSH private key, depending on `kind`.
    #[schema(write_only)]
    secret: String,
    /// The sudo password, when it differs from the login password. Only ever needed to install
    /// Docker on the target.
    #[serde(default)]
    #[schema(write_only)]
    sudo_password: Option<String>,
}

/// What the operator asked for.
#[derive(Deserialize, utoipa::ToSchema)]
pub(crate) struct RelocationRequest {
    /// `archive` builds one and stops, `preflight` only checks the target, `push` does everything.
    #[serde(default)]
    mode: RelocationMode,
    /// Carry the metrics (VictoriaMetrics). On by default — a monitoring system that arrives with
    /// no history is a new deployment, not a moved one.
    #[serde(default = "yes")]
    include_metrics: bool,
    /// Carry the events and flows (VictoriaLogs, ClickHouse). On by default, and the one option
    /// with a running cost: both stores are stopped for the minutes the copy takes.
    #[serde(default = "yes")]
    include_tier2: bool,
    /// Carry the three Yagra images. Off by default — the new host usually pulls them. Needed when
    /// it cannot reach the registry, or when this deployment runs images from a private one.
    #[serde(default)]
    include_images: bool,
    /// Install Docker on the target if it has none. On by default; needs `sudo` and internet
    /// there, and runs the official `get.docker.com` script as root.
    #[serde(default = "yes")]
    install_docker: bool,
    /// Required for `preflight` and `push`.
    #[serde(default)]
    target: Option<TargetSpec>,
    /// Required for `preflight` and `push`. Never stored, never logged, never returned.
    #[serde(default)]
    auth: Option<AuthSpec>,
}

fn yes() -> bool {
    true
}

impl RelocationRequest {
    fn options(&self) -> RelocationOptions {
        RelocationOptions {
            mode: self.mode,
            include_metrics: self.include_metrics,
            include_tier2: self.include_tier2,
            include_images: self.include_images,
            install_docker: self.install_docker,
            target: self.target.as_ref().map(|t| SshTarget {
                host: t.host.trim().to_owned(),
                port: t.port,
                user: t.user.trim().to_owned(),
                dir: t.dir.trim().to_owned(),
                auth: self.auth.as_ref().map_or(SshAuthKind::Password, |a| a.kind),
            }),
        }
    }

    /// The secrets to stage, in the shape the sidecar reads them back.
    ///
    /// A `key` login puts the secret in `ssh_key` and a `password` login in `ssh_password`, so the
    /// procedure never has to decide which one a single field meant.
    fn secrets(&self) -> SshSecrets {
        let Some(auth) = self.auth.as_ref() else {
            return SshSecrets {
                ssh_password: None,
                ssh_key: None,
                sudo_password: None,
            };
        };
        let (password, key) = match auth.kind {
            SshAuthKind::Password => (Some(auth.secret.clone()), None),
            SshAuthKind::Key => (None, Some(auth.secret.clone())),
        };
        SshSecrets {
            ssh_password: password,
            ssh_key: key,
            sudo_password: auth
                .sudo_password
                .as_ref()
                .filter(|s| !s.is_empty())
                .cloned(),
        }
    }
}

/// The run id, so the page can tell its own request from one another admin started.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct RelocationAccepted {
    id: String,
}

/// The archive waiting on this host, when there is one.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct ArchiveInfo {
    filename: String,
    size_bytes: u64,
    /// Unix seconds.
    modified_at: i64,
}

/// Everything the relocation page needs in one read.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct RelocationStatusResponse {
    /// Whether this deployment's updater declares the command at all. `false` on an updater that
    /// predates ADR-121 — the next upgrade recreates it.
    supported: bool,
    /// Whether a relocation can be started from here: the updater is deployed, alive, the
    /// operator's switch is on, **and** it supports the command.
    enabled: bool,
    /// The operator's switch, as stored. Shared with the upgrade mechanism: one sidecar, one
    /// switch — turning upgrades off turns this off too, which is deliberate.
    upgrade_enabled: bool,
    /// The updater container's own state, from the same reading the Upgrade page uses.
    updater: UpdaterInfo,
    /// The current or most recent run.
    run: Option<RelocationRun>,
    /// The archive on this host, if one is waiting to be downloaded.
    archive: Option<ArchiveInfo>,
    /// Free bytes on the filesystem holding the hand-off volume.
    free_bytes: Option<u64>,
    /// Roughly how large the archive will be. ⚠️ **Partial** — the database plus the metrics.
    /// Core cannot see the event, flow or image volumes; only the sidecar can, and its check is
    /// the one that can stop a run.
    estimate_bytes: Option<u64>,
    /// [`estimate_bytes`](Self::estimate_bytes) doubled: the copy, and the tar of the copy.
    needed_bytes: Option<u64>,
}

/// The tail of the run's log.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct RelocationLog {
    lines: Vec<String>,
}

/// How much of the log to return.
#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub(crate) struct LogQuery {
    /// Lines from the end. Clamped to 1..=2000.
    tail: Option<usize>,
}

const DEFAULT_LOG_TAIL: usize = 200;

/// What the relocation mechanism can do, and what it is doing.
#[utoipa::path(
    get, path = "/api/v1/system/relocation", tag = "system",
    responses(
        (status = 200, description = "The mechanism's state and the current run", body = RelocationStatusResponse),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageSystem", body = super::error::ErrorBody),
        (status = 503, description = "No upgrade mechanism is installed (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn get_relocation(
    _cfg: RequireManageSystem,
    upgrade: Upgrade,
    admin: Admin,
) -> ApiResult<Json<RelocationStatusResponse>> {
    let now = super::util::now_unix_s();
    let beat = upgrade.heartbeat();
    let fresh = beat.as_ref().is_some_and(|h| {
        crate::upgrade::heartbeat_is_fresh(h.written_at, h.check_interval_secs, now)
    });
    let supported = beat.as_ref().is_some_and(|h| h.relocate);
    let switched_on = upgrade.enabled().await;
    let run = crate::relocation::status(&upgrade);
    let archive = crate::relocation::archive(&upgrade).map(|(path, size, modified)| ArchiveInfo {
        filename: path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_owned(),
        size_bytes: size,
        modified_at: modified,
    });
    // The estimate is best effort on purpose: a database this cannot measure must not stop the
    // page from rendering, it must render without a number.
    let estimate = crate::relocation::estimate_bytes(
        &admin.0.repo.pool(),
        crate::relocation::metrics_dir().as_deref(),
        &RelocationOptions {
            mode: RelocationMode::Archive,
            include_metrics: true,
            include_tier2: false,
            include_images: false,
            install_docker: false,
            target: None,
        },
    )
    .await;
    Ok(Json(RelocationStatusResponse {
        supported,
        enabled: beat.is_some() && fresh && switched_on && supported,
        upgrade_enabled: switched_on,
        updater: super::upgrade::updater_info(&upgrade, beat.as_ref(), fresh),
        run,
        archive,
        free_bytes: upgrade.free_bytes(),
        estimate_bytes: estimate,
        needed_bytes: estimate.map(crate::relocation::space_needed),
    }))
}

/// Build a relocation archive, and — unless the mode says otherwise — send it and restore it.
///
/// Hands the request to the privileged updater and returns immediately; the work outlives this
/// request by minutes. Poll `GET` for the outcome.
#[utoipa::path(
    post, path = "/api/v1/system/relocation", tag = "system",
    request_body = RelocationRequest,
    responses(
        (status = 202, description = "Accepted; the updater will carry it out", body = RelocationAccepted),
        (status = 400, description = "A malformed target, or a push with no credentials", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageSystem, ManageCredentials or ViewAudit", body = super::error::ErrorBody),
        (status = 409, description = "A relocation or an upgrade is already running", body = super::error::ErrorBody),
        (status = 503, description = "The mechanism is absent, switched off, or too old for this command", body = super::error::ErrorBody),
        (status = 507, description = "Not enough free disk space to build the archive", body = super::error::ErrorBody),
    ),
)]
async fn start_relocation(
    _guard: RelocationWrite,
    upgrade: Upgrade,
    admin: Admin,
    caller: Option<Caller>,
    Json(req): Json<RelocationRequest>,
) -> ApiResult<(StatusCode, Json<RelocationAccepted>)> {
    let now = super::util::now_unix_s();
    // Everything the upgrade edge refuses, refused here too and in the same order — including the
    // 409 that stops the two mechanisms overlapping in the other direction.
    super::upgrade::reachable(&upgrade, now).await?;
    if !upgrade.heartbeat().is_some_and(|h| h.relocate) {
        return Err(ApiError::unavailable(
            "relocation_unsupported",
            "this deployment's updater predates the relocation feature; it is replaced by the \
             next upgrade, after which this page works",
        ));
    }
    if crate::relocation::is_running(&upgrade) {
        return Err(ApiError::conflict(
            "relocation_in_progress",
            "a relocation is already running",
        ));
    }
    let opts = req.options();
    opts.validate()
        .map_err(|m| ApiError::bad_request("invalid_relocation_request", m))?;
    let secrets = req.secrets();
    if opts.mode.needs_target() && secrets.is_empty() {
        return Err(ApiError::bad_request(
            "invalid_relocation_request",
            "connecting to another server needs a password or a private key",
        ));
    }
    // The estimate core can make, doubled. The sidecar measures the real volumes and refuses
    // again; this one exists so an operator is told before they wait for a backup that cannot fit.
    if let (Some(free), Some(estimate)) = (
        upgrade.free_bytes(),
        crate::relocation::estimate_bytes(&admin.0.repo.pool(), None, &opts).await,
    ) {
        let needed = crate::relocation::space_needed(estimate);
        if free < needed {
            return Err(ApiError::insufficient_storage(
                "insufficient_disk",
                format!(
                    "the archive needs about {} MB and this host has {} MB free; nothing has \
                     been written",
                    needed / 1_048_576,
                    free / 1_048_576
                ),
            ));
        }
    }
    let by = caller
        .as_ref()
        .map_or("unknown", |c| c.0.username.as_str())
        .to_owned();
    // 🚨 The secrets land BEFORE the request does. The sidecar starts within five seconds of the
    // request file appearing, and a run that finds no password fails at the first ssh — with the
    // operator's credentials sitting on the volume for the next one to pick up.
    if !secrets.is_empty() {
        crate::relocation::write_secrets(&upgrade, &secrets)
            .map_err(|e| ApiError::internal_with_code("relocation_failed", e.to_string()))?;
    }
    let id = crate::upgrade::new_run_id();
    if let Err(e) = crate::relocation::request(&upgrade, &id, &by, now, &opts) {
        // The request never landed, so nothing will ever remove what was staged for it.
        crate::relocation::clear_secrets(&upgrade);
        return Err(ApiError::internal_with_code(
            "relocation_failed",
            e.to_string(),
        ));
    }
    // The host, the mode and the account — never the credential. `audit_mw` records the call
    // itself; this is the operational line an engineer greps for at three in the morning.
    tracing::warn!(
        run = %id,
        mode = opts.mode.as_str(),
        host = opts.target.as_ref().map(|t| t.host.as_str()).unwrap_or("-"),
        by = %by,
        "relocation requested"
    );
    Ok((StatusCode::ACCEPTED, Json(RelocationAccepted { id })))
}

/// Download the archive.
///
/// 🚨 This is the one endpoint in the API that returns secrets. See the module doc.
#[utoipa::path(
    post, path = "/api/v1/system/relocation/archive", tag = "system",
    responses(
        (status = 200, description = "The relocation archive: a gzipped tar holding the KEK, a full PostgreSQL dump, the metrics, and this deployment's .env and composition. Treat it exactly as you would the KEK itself", content_type = "application/gzip"),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageSystem, ManageCredentials or ViewAudit", body = super::error::ErrorBody),
        (status = 404, description = "There is no archive on this host", body = super::error::ErrorBody),
        (status = 409, description = "A relocation is running; the archive is not final yet", body = super::error::ErrorBody),
        (status = 503, description = "No upgrade mechanism is installed", body = super::error::ErrorBody),
    ),
)]
async fn download_archive(_guard: RelocationWrite, upgrade: Upgrade) -> ApiResult<Response> {
    if crate::relocation::is_running(&upgrade) {
        return Err(ApiError::conflict(
            "relocation_in_progress",
            "a relocation is running; wait for it to finish before downloading the archive",
        ));
    }
    let Some((path, size, _)) = crate::relocation::archive(&upgrade) else {
        return Err(ApiError::not_found(
            "no_archive",
            "there is no relocation archive on this host; create one first",
        ));
    };
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("yagra-relocation.tar.gz")
        .to_owned();
    let file = tokio::fs::File::open(&path)
        .await
        .map_err(|e| ApiError::internal_with_code("relocation_failed", e.to_string()))?;
    // Streamed rather than read into memory: it is a full database dump plus the metrics, and on
    // a real deployment that is gigabytes.
    let body = Body::from_stream(tokio_util::io::ReaderStream::new(file));
    tracing::warn!(bytes = size, file = %name, "relocation archive downloaded");
    Ok((
        [
            (CONTENT_TYPE, "application/gzip".to_owned()),
            (CONTENT_LENGTH, size.to_string()),
            (
                CONTENT_DISPOSITION,
                format!("attachment; filename=\"{name}\""),
            ),
        ],
        body,
    )
        .into_response())
}

/// Delete the archive and any staged credentials.
#[utoipa::path(
    delete, path = "/api/v1/system/relocation", tag = "system",
    responses(
        (status = 204, description = "Deleted"),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageSystem, ManageCredentials or ViewAudit", body = super::error::ErrorBody),
        (status = 404, description = "There was nothing to delete", body = super::error::ErrorBody),
        (status = 409, description = "A relocation is running", body = super::error::ErrorBody),
        (status = 503, description = "No upgrade mechanism is installed", body = super::error::ErrorBody),
    ),
)]
async fn delete_relocation(_guard: RelocationWrite, upgrade: Upgrade) -> ApiResult<StatusCode> {
    if crate::relocation::is_running(&upgrade) {
        return Err(ApiError::conflict(
            "relocation_in_progress",
            "a relocation is running; wait for it to finish",
        ));
    }
    let removed = crate::relocation::delete_archive(&upgrade)
        .map_err(|e| ApiError::internal_with_code("relocation_failed", e.to_string()))?;
    // Always, even when there was no archive: this is also the button an operator presses after
    // changing their mind about a push that was refused.
    crate::relocation::clear_secrets(&upgrade);
    if removed {
        tracing::warn!("relocation archive deleted");
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found(
            "no_archive",
            "there is no relocation archive on this host",
        ))
    }
}

/// What the relocation has printed so far, including the restore on the other host.
#[utoipa::path(
    get, path = "/api/v1/system/relocation/log", tag = "system",
    params(LogQuery),
    responses(
        (status = 200, description = "The tail of the run's log", body = RelocationLog),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageSystem", body = super::error::ErrorBody),
        (status = 503, description = "No upgrade mechanism is installed", body = super::error::ErrorBody),
    ),
)]
async fn get_relocation_log(
    _cfg: RequireManageSystem,
    upgrade: Upgrade,
    _admin: Admin,
    Query(q): Query<LogQuery>,
) -> ApiResult<Json<RelocationLog>> {
    Ok(Json(RelocationLog {
        lines: crate::relocation::log_tail(&upgrade, q.tail.unwrap_or(DEFAULT_LOG_TAIL)),
    }))
}

#[cfg(test)]
mod tests {
    use super::super::tests_support::{private_state, public_state};
    use super::ApiState;
    use axum::body::{to_bytes, Body};
    use axum::http::{header::AUTHORIZATION, Request, StatusCode};
    use tower::ServiceExt;
    use yagra_common::{Principal, Role, Scope};

    /// Every route this module serves, with a body that would be valid if the caller got that far.
    ///
    /// Listed rather than derived, for the reason the upgrade module's list is: this whole surface
    /// is one permission set, and the cost of getting it wrong here is every secret the deployment
    /// holds.
    const ROUTES: &[(&str, &str, &str)] = &[
        ("GET", "/api/v1/system/relocation", ""),
        ("GET", "/api/v1/system/relocation/log", ""),
        ("POST", "/api/v1/system/relocation", r#"{"mode":"archive"}"#),
        ("POST", "/api/v1/system/relocation/archive", ""),
        ("DELETE", "/api/v1/system/relocation", ""),
    ];

    async fn send(
        st: ApiState,
        method: &str,
        uri: &str,
        token: Option<&str>,
        body: &str,
    ) -> (StatusCode, serde_json::Value) {
        let mut b = Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/json");
        if let Some(t) = token {
            b = b.header(AUTHORIZATION, format!("Bearer {t}"));
        }
        let res = super::super::router(st)
            .oneshot(b.body(Body::from(body.to_owned())).expect("request"))
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

    /// Authentication runs before the mechanism is looked at, reads included.
    ///
    /// `public_state` too: `public_dashboard` opens reads of *monitoring data*, and none of these
    /// is that. An anonymous caller must learn only that it is unauthenticated — never whether
    /// this deployment has an archive of its own secrets sitting on disk.
    #[tokio::test]
    async fn every_relocation_route_is_gated_before_the_mechanism_is_consulted() {
        for st in [private_state, public_state] {
            for (method, uri, body) in ROUTES {
                let (status, json) = send(st(), method, uri, None, body).await;
                assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {uri}");
                assert_eq!(json["error"]["code"], "unauthorized", "{method} {uri}");
            }
        }
    }

    /// 403, not 401 — and the read is included on purpose. The page draws no `useCan`: a refused
    /// read renders `LoadBlockNotice` instead of the screen, so the buttons are never mounted for
    /// someone who could not press them (ADR-056 decision 2).
    #[tokio::test]
    async fn neither_viewer_nor_operator_may_reach_the_relocation_surface() {
        for role in [Role::Viewer, Role::Operator] {
            for (method, uri, body) in ROUTES {
                let st = private_state();
                let token = token_for(&st, role);
                let (status, _) = send(st, method, uri, Some(&token), body).await;
                assert_eq!(status, StatusCode::FORBIDDEN, "{role:?} on {method} {uri}");
            }
        }
    }

    /// The answer the 401s above must *not* have been: skeleton mode carries no `UpgradeRepo`, so
    /// an authorized admin reaches the availability guard and gets the typed 503.
    #[tokio::test]
    async fn an_admin_gets_the_typed_unavailable_when_no_mechanism_is_installed() {
        for (method, uri, body) in ROUTES {
            let st = private_state();
            let token = token_for(&st, Role::Admin);
            let (status, json) = send(st, method, uri, Some(&token), body).await;
            assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{method} {uri}");
            assert_eq!(json["error"]["code"], "admin_unavailable", "{method} {uri}");
        }
    }

    /// `Leader` sits inside `RelocationWrite`, so a standby is refused before the mechanism is
    /// looked at — and with a different code, which is what makes the ordering observable.
    #[tokio::test]
    async fn a_write_is_refused_for_leadership_before_the_mechanism_is_looked_at() {
        for (method, uri, body) in ROUTES.iter().filter(|(m, ..)| *m != "GET") {
            let st = private_state();
            st.is_leader
                .store(false, std::sync::atomic::Ordering::Release);
            let token = token_for(&st, Role::Admin);
            let (status, json) = send(st, method, uri, Some(&token), body).await;
            assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{method} {uri}");
            assert_eq!(json["error"]["code"], "not_leader", "{method} {uri}");
        }
    }

    // ── The accepted-write tests (ADR-115). They need a real database. ─────────────────────────

    use super::super::tests_support::live_state_with_upgrade_dir;
    use crate::api::tests_support::token as issue_token;

    fn heartbeat(dir: &std::path::Path, relocate: bool) {
        let now = super::super::util::now_unix_s();
        let extra = if relocate { r#","relocate":true"# } else { "" };
        std::fs::write(
            dir.join("current.json"),
            format!(
                r#"{{"written_at":{now},"repo":"ghcr.io/horryworks","check_interval_secs":86400,"allow_bundle":false,"paused":false,"local_pollers":[]{extra}}}"#
            ),
        )
        .expect("write the heartbeat");
    }

    fn tempdir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("yagra-reloc-api-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join(crate::relocation::SUBDIR))
            .expect("make the hand-off dir");
        dir
    }

    /// The whole write surface, **accepted** — which is the shape ADR-115 exists to demand. Every
    /// other test in this file asserts a refusal, and a suite made only of refusals passes just as
    /// well when everything is refused.
    ///
    /// It walks the states in the order an operator does: request, refuse a second one, refuse an
    /// upgrade on top of it, then delete, download, delete again.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_relocation_request_is_accepted_and_handed_to_the_sidecar(pool: sqlx::PgPool) {
        let dir = tempdir();
        heartbeat(&dir, true);
        let st = live_state_with_upgrade_dir(pool, dir.clone()).await;
        let token = issue_token(&st, Role::Admin);

        let (status, json) = send(
            st.clone(),
            "POST",
            "/api/v1/system/relocation",
            Some(&token),
            r#"{"mode":"push","target":{"host":"192.0.2.10","user":"ubuntu"},
                "auth":{"kind":"password","secret":"hunter2","sudo_password":"hunter3"}}"#,
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED, "{json}");
        let id = json["id"].as_str().expect("a run id").to_owned();

        // What the sidecar will read, and — the half that matters — what it will not.
        let request =
            std::fs::read_to_string(dir.join("request")).expect("the request was written");
        for line in [
            "command=relocate\n",
            "tag=\n",
            "mode=push\n",
            "install_docker=1\n",
            "include_tier2=1\n",
            "target_host=192.0.2.10\n",
            "target_user=ubuntu\n",
            "target_dir=yagra\n",
            "auth=password\n",
        ] {
            assert!(
                request.contains(line),
                "the request has no `{line:?}`:\n{request}"
            );
        }
        for secret in ["hunter2", "hunter3"] {
            assert!(
                !request.contains(secret),
                "a credential reached the request file, which is read as root by the sidecar and \
                 is not a place secrets may live:\n{request}"
            );
        }
        let sec = dir
            .join(crate::relocation::SUBDIR)
            .join(crate::relocation::SECRET_DIR);
        assert_eq!(
            std::fs::read_to_string(sec.join("ssh_password")).unwrap(),
            "hunter2"
        );
        assert_eq!(
            std::fs::read_to_string(sec.join("sudo_password")).unwrap(),
            "hunter3"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(sec.join("ssh_password"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }

        // The sidecar's first act is to write a run. From here the two mechanisms lock each other.
        let running = format!(
            r#"{{"id":"{id}","mode":"push","state":"running","stage":"backup","started_at":1}}"#
        );
        let status_path = dir
            .join(crate::relocation::SUBDIR)
            .join(crate::relocation::STATUS_FILE);
        std::fs::write(&status_path, &running).unwrap();

        let (status, json) = send(
            st.clone(),
            "POST",
            "/api/v1/system/relocation",
            Some(&token),
            r#"{"mode":"archive"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(json["error"]["code"], "relocation_in_progress");

        // The mirror case: an upgrade must not start on top of a relocation either.
        let (status, json) = send(
            st.clone(),
            "POST",
            "/api/v1/system/upgrade/check",
            Some(&token),
            "",
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(json["error"]["code"], "relocation_in_progress");

        let (status, _) = send(
            st.clone(),
            "DELETE",
            "/api/v1/system/relocation",
            Some(&token),
            "",
        )
        .await;
        assert_eq!(
            status,
            StatusCode::CONFLICT,
            "an archive in flight is not deletable"
        );

        // The run finishes. There is still no archive, so the download and the delete both 404 —
        // which is what makes the 200 and the 204 below mean something.
        std::fs::write(
            &status_path,
            format!(r#"{{"id":"{id}","mode":"archive","state":"done","stage":"archive","started_at":1}}"#),
        )
        .unwrap();
        let (status, json) = send(
            st.clone(),
            "POST",
            "/api/v1/system/relocation/archive",
            Some(&token),
            "",
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(json["error"]["code"], "no_archive");

        let archive = dir
            .join(crate::relocation::SUBDIR)
            .join("yagra-relocation-20260908T000000Z.tar.gz");
        std::fs::write(&archive, b"not really a tarball").unwrap();

        // The download, which is the one API response in this repository that carries secrets.
        let res = super::super::router(st.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/system/relocation/archive")
                    .header(AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(res.headers()["content-type"], "application/gzip");
        assert!(res.headers()["content-disposition"]
            .to_str()
            .unwrap()
            .contains("yagra-relocation-20260908T000000Z.tar.gz"));
        let bytes = to_bytes(res.into_body(), 1 << 20).await.unwrap();
        assert_eq!(&bytes[..], b"not really a tarball");

        // The read reports it, and reports the mechanism as usable.
        let (status, json) = send(
            st.clone(),
            "GET",
            "/api/v1/system/relocation",
            Some(&token),
            "",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["supported"], true);
        assert_eq!(json["enabled"], true);
        assert_eq!(
            json["archive"]["filename"],
            "yagra-relocation-20260908T000000Z.tar.gz"
        );

        let (status, _) = send(
            st.clone(),
            "DELETE",
            "/api/v1/system/relocation",
            Some(&token),
            "",
        )
        .await;
        assert_eq!(
            status,
            StatusCode::NO_CONTENT,
            "the documented status, not merely a success"
        );
        assert!(!archive.exists());
        assert!(
            !sec.exists(),
            "the delete clears the staged credentials too"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An updater that does not declare the command is refused rather than asked.
    ///
    /// Asking it anyway is not harmless: it would answer by writing a rejection into
    /// `status.json`, which is the *upgrade* page's file and is claimed by
    /// `settle_finished_run` (ADR-121 decision 5).
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_sidecar_without_the_capability_is_refused(pool: sqlx::PgPool) {
        let dir = tempdir();
        heartbeat(&dir, false);
        let st = live_state_with_upgrade_dir(pool, dir.clone()).await;
        let token = issue_token(&st, Role::Admin);
        let (status, json) = send(
            st,
            "POST",
            "/api/v1/system/relocation",
            Some(&token),
            r#"{"mode":"archive"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(json["error"]["code"], "relocation_unsupported");
        assert!(
            !dir.join("request").exists(),
            "a refused request must not reach the sidecar's request file"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
