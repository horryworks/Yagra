// SPDX-License-Identifier: AGPL-3.0-only
//! Where config backups are pushed (ADR-210 stage 4): one Git repository for the whole deployment.
//!
//! For a person, `ManageSystem` (decision 35 — it decides what leaves the box, ADR-057):
//!
//! - `GET /api/v1/config-backup-remote` — the destination as set, the public half of its SSH key,
//!   the fingerprints of the host keys learned, and how the last push went. Never the token or the
//!   private key: the credential is an integration key, sealed and not revealed (decision 30).
//! - `PUT /api/v1/config-backup-remote` — set or replace it. An HTTPS token is sealed on arrival and
//!   kept when a later change omits it; an SSH key is generated here the first time and kept.
//! - `DELETE /api/v1/config-backup-remote` — stop pushing, and delete the sealed credential.
//! - `POST /api/v1/config-backup-remote/ssh-key` — replace the SSH key with a new one.
//! - `DELETE /api/v1/config-backup-remote/host-keys` — forget the destination's host keys, so the
//!   next push learns them again (decision 31).
//!
//! `GET /api/v1/config-backup-remote/status` is the same "how did the last push go", for anyone with
//! `View`: the host the backups go to and when they last arrived, without the path or the key.
//!
//! And one route for the push script inside the bundled oxidized, outside `/api/v1`, behind the
//! source token like the other two (`api/config_backup.rs`):
//!
//! - `GET /internal/oxidized/push?head=` — whether that head needs pushing, and if so where and with
//!   what. **It returns the destination's token or private key in plaintext**, for the one push the
//!   script is about to make: the third reader of plaintext (security.md), argued in decision 30.
//! - `POST /internal/oxidized/push` — how the push went: the failure kind core's patterns named,
//!   never git's output.

use axum::{
    extract::Query,
    http::{HeaderMap, StatusCode},
    routing::{delete, get, post},
    Form, Json, Router,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use yagra_base::config_remote::{self, ConfigRemote, GitAuth, PushFailure, RemoteSettings};
use yagra_base::secrets::{GitRemoteSecret, KIND_GIT_REMOTE};

use super::config_backup::{check_oid, check_source_token, oxidized_of};
use super::error::{ApiError, ApiResult};
use super::extract::{Admin, RequireManageSystem, RequireView};
use super::ApiState;

/// This domain's slice of the OpenAPI document (ADR-035), merged by [`super::openapi::document`].
#[derive(utoipa::OpenApi)]
#[openapi(paths(
    get_remote,
    put_remote,
    delete_remote,
    regenerate_key,
    forget_host_keys,
    push_status,
    push_ask,
    push_report
))]
pub(super) struct Doc;

/// The config-backup destination's routes, merged by [`super::router`].
pub(super) fn routes() -> Router<ApiState> {
    Router::new()
        .route(
            "/api/v1/config-backup-remote",
            get(get_remote).put(put_remote).delete(delete_remote),
        )
        .route("/api/v1/config-backup-remote/ssh-key", post(regenerate_key))
        .route(
            "/api/v1/config-backup-remote/host-keys",
            delete(forget_host_keys),
        )
        .route("/api/v1/config-backup-remote/status", get(push_status))
        .route("/internal/oxidized/push", get(push_ask).post(push_report))
}

/// The name the sealed credential is listed under in Settings ▸ Credentials.
const CREDENTIAL_NAME: &str = "Git destination (config backup)";

/// The longest HTTPS user name and token accepted.
const LOGIN_MAX: usize = 4096;

fn read_failed(e: &anyhow::Error) -> ApiError {
    ApiError::from_internal(
        e.as_ref(),
        "config backup destination",
        "failed to read the config backup destination",
    )
}

fn write_failed(e: &anyhow::Error) -> ApiError {
    ApiError::from_internal(
        e.as_ref(),
        "config backup destination",
        "failed to store the config backup destination",
    )
}

fn bad(message: &'static str) -> ApiError {
    ApiError::bad_request("invalid_config_backup_remote", message)
}

fn no_destination() -> ApiError {
    ApiError::not_found(
        "no_config_backup_remote",
        "no config backup destination is set",
    )
}

/// How the last push went.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct PushStatusView {
    /// A destination is set.
    configured: bool,
    /// The host config backups go to — the URL's host, not its path.
    host: Option<String>,
    /// When a push last succeeded, and the commit the destination then held: the one a person
    /// finds there.
    pushed_at: Option<DateTime<Utc>>,
    pushed_commit: Option<String>,
    /// Why the last push failed, when the last one did.
    failure: Option<PushFailure>,
    failed_at: Option<DateTime<Utc>>,
    /// Failed pushes in a row.
    failures: u32,
}

impl PushStatusView {
    fn of(remote: Option<&ConfigRemote>) -> Self {
        Self {
            configured: remote.is_some(),
            host: remote
                .and_then(|r| config_remote::host_of(&r.url))
                .map(str::to_owned),
            pushed_at: remote.and_then(|r| r.pushed_at),
            pushed_commit: remote.and_then(|r| r.pushed_commit.clone()),
            failure: remote.and_then(|r| r.failure),
            failed_at: remote.and_then(|r| r.failed_at),
            failures: remote.map_or(0, |r| r.failures),
        }
    }
}

/// The destination, for the person who sets it.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct RemoteView {
    configured: bool,
    url: Option<String>,
    branch: Option<String>,
    auth: Option<GitAuth>,
    /// The HTTPS user name. The token is never returned.
    username: Option<String>,
    /// The public half of the SSH key, to register as a deploy key with write access.
    ssh_public_key: Option<String>,
    /// The destination's host keys learned on the first SSH push, as `type SHA256:…`.
    host_keys: Vec<String>,
    status: PushStatusView,
}

async fn view_of(admin: &super::AdminState) -> ApiResult<RemoteView> {
    let remote = admin
        .config_remote
        .get()
        .await
        .map_err(|e| read_failed(&e))?;
    let username = match &remote {
        Some(r) if r.auth == GitAuth::HttpsToken => match sealed_secret(admin, r).await? {
            Some(GitRemoteSecret::HttpsToken { username, .. }) => Some(username),
            Some(GitRemoteSecret::SshKey { .. }) | None => None,
        },
        Some(_) | None => None,
    };
    Ok(RemoteView {
        configured: remote.is_some(),
        url: remote.as_ref().map(|r| r.url.clone()),
        branch: remote.as_ref().map(|r| r.branch.clone()),
        auth: remote.as_ref().map(|r| r.auth),
        username,
        ssh_public_key: remote.as_ref().and_then(|r| r.ssh_public_key.clone()),
        host_keys: remote
            .as_ref()
            .and_then(|r| r.known_hosts.as_deref())
            .map(config_remote::fingerprints)
            .unwrap_or_default(),
        status: PushStatusView::of(remote.as_ref()),
    })
}

/// The destination's sealed secret, opened. `None` when the credential is gone or holds something
/// else — a destination nothing can push to, which the next push reports.
async fn sealed_secret(
    admin: &super::AdminState,
    remote: &ConfigRemote,
) -> ApiResult<Option<GitRemoteSecret>> {
    let opened = admin
        .creds
        .open(remote.credential_id)
        .await
        .map_err(|e| read_failed(&e))?;
    Ok(opened
        .filter(|(kind, _)| kind == KIND_GIT_REMOTE)
        .and_then(|(_, bytes)| GitRemoteSecret::parse(&bytes).ok()))
}

/// Seal `secret` into the destination's credential: the existing one when there is one, a new one
/// otherwise.
async fn seal(
    admin: &super::AdminState,
    existing: Option<Uuid>,
    secret: &GitRemoteSecret,
) -> ApiResult<Uuid> {
    let bytes = serde_json::to_vec(secret).map_err(|e| write_failed(&e.into()))?;
    match existing {
        Some(id) => {
            let updated = admin
                .creds
                .update(id, CREDENTIAL_NAME, Some((KIND_GIT_REMOTE, &bytes)))
                .await
                .map_err(|e| write_failed(&e))?;
            if updated {
                return Ok(id);
            }
            // Deleted underneath the destination: seal a new one.
            admin
                .creds
                .create(CREDENTIAL_NAME, KIND_GIT_REMOTE, &bytes)
                .await
                .map_err(|e| write_failed(&e))
        }
        None => admin
            .creds
            .create(CREDENTIAL_NAME, KIND_GIT_REMOTE, &bytes)
            .await
            .map_err(|e| write_failed(&e)),
    }
}

/// Store `settings` and delete a credential it replaced, if one was replaced.
async fn store(admin: &super::AdminState, settings: &RemoteSettings) -> ApiResult<()> {
    let replaced = admin
        .config_remote
        .put(settings)
        .await
        .map_err(|e| write_failed(&e))?;
    if let Some(old) = replaced {
        if let Err(e) = admin.creds.delete(old).await {
            tracing::warn!(error = %e, "config backup: a replaced Git credential was not deleted");
        }
    }
    Ok(())
}

#[utoipa::path(
    get, path = "/api/v1/config-backup-remote", tag = "nodes",
    responses(
        (status = 200, description = "The config backup destination, `configured: false` when none is set. Never the token or the private key", body = RemoteView),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "The caller lacks manage-system", body = super::error::ErrorBody),
        (status = 503, description = "This deployment has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn get_remote(_perm: RequireManageSystem, admin: Admin) -> ApiResult<Json<RemoteView>> {
    Ok(Json(view_of(&admin).await?))
}

/// The destination a person sets.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub(super) struct RemoteBody {
    /// `https://host/owner/repo.git` for a token, `ssh://git@host/owner/repo.git` or
    /// `git@host:owner/repo.git` for a key.
    url: String,
    /// The branch pushed to. Omitted: `main`.
    #[serde(default)]
    branch: Option<String>,
    auth: GitAuth,
    /// The HTTPS user name (`x-token-auth` for a Bitbucket repository token).
    #[serde(default)]
    username: Option<String>,
    /// The HTTPS access token. Omitted: keep the stored one, which is only kept for the same host
    /// and port.
    #[serde(default)]
    token: Option<String>,
}

#[utoipa::path(
    put, path = "/api/v1/config-backup-remote", tag = "nodes",
    request_body = RemoteBody,
    responses(
        (status = 200, description = "Set. With an SSH key, the response carries the public half to register as a deploy key", body = RemoteView),
        (status = 400, description = "A URL, branch, user name or token that is refused (`invalid_config_backup_remote`), or a destination moved to another host with no token of its own (`token_required_for_new_address`)", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "The caller lacks manage-system", body = super::error::ErrorBody),
        (status = 503, description = "This deployment has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn put_remote(
    _perm: RequireManageSystem,
    admin: Admin,
    Json(body): Json<RemoteBody>,
) -> ApiResult<Json<RemoteView>> {
    let url = body.url.trim().to_owned();
    config_remote::check_url(&url).map_err(bad)?;
    let branch = body
        .branch
        .as_deref()
        .map(str::trim)
        .filter(|b| !b.is_empty())
        .unwrap_or("main")
        .to_owned();
    config_remote::check_branch(&branch).map_err(bad)?;
    let https = url.starts_with("https://");
    match (body.auth, https) {
        (GitAuth::HttpsToken, true) | (GitAuth::SshKey, false) => {}
        (GitAuth::HttpsToken, false) => return Err(bad("a token is used with an https:// URL")),
        (GitAuth::SshKey, true) => {
            return Err(bad("an SSH key is used with an ssh:// or git@host: URL"))
        }
        (GitAuth::Unknown, _) => return Err(bad("auth must be https_token or ssh_key")),
    }

    let existing = admin
        .config_remote
        .get()
        .await
        .map_err(|e| read_failed(&e))?;
    let stored = match &existing {
        Some(r) => sealed_secret(&admin, r).await?,
        None => None,
    };
    let existing_id = existing.as_ref().map(|r| r.credential_id);

    let (credential_id, ssh_public_key) = match body.auth {
        GitAuth::HttpsToken => {
            let username = body.username.as_deref().map(str::trim).unwrap_or("");
            if username.is_empty() || username.len() > LOGIN_MAX {
                return Err(bad("a user name is needed with a token"));
            }
            let typed = body
                .token
                .as_deref()
                .map(str::trim)
                .filter(|t| !t.is_empty());
            // The stored token goes only to the host it was given for: an edit that moves the URL
            // to another host or port must bring a token, or the sealed one would be sent to a
            // server it was never meant for (the rule NetBox's edit follows, ADR-178 decision 3).
            let same_origin = existing.as_ref().is_some_and(|r| {
                config_remote::https_origin(&r.url).is_some()
                    && config_remote::https_origin(&r.url) == config_remote::https_origin(&url)
            });
            let token = match (typed, &stored) {
                (Some(t), _) => t.to_owned(),
                (None, Some(GitRemoteSecret::HttpsToken { token, .. })) if same_origin => {
                    token.clone()
                }
                (None, Some(GitRemoteSecret::HttpsToken { .. })) => {
                    return Err(ApiError::bad_request(
                        "token_required_for_new_address",
                        "a destination on another host needs its own token",
                    ))
                }
                (None, Some(GitRemoteSecret::SshKey { .. }) | None) => {
                    return Err(bad("a token is needed"))
                }
            };
            if token.len() > LOGIN_MAX {
                return Err(bad("the token is too long"));
            }
            let secret = GitRemoteSecret::HttpsToken {
                username: username.to_owned(),
                token,
            };
            (seal(&admin, existing_id, &secret).await?, None)
        }
        GitAuth::SshKey => match (&existing, &stored) {
            // The key already registered stays: replacing it would break the deploy key.
            (Some(r), Some(GitRemoteSecret::SshKey { .. })) if r.ssh_public_key.is_some() => {
                (r.credential_id, r.ssh_public_key.clone())
            }
            _ => {
                let (private_key, public) = config_remote::generate_ssh_key();
                let secret = GitRemoteSecret::SshKey { private_key };
                (seal(&admin, existing_id, &secret).await?, Some(public))
            }
        },
        GitAuth::Unknown => return Err(bad("auth must be https_token or ssh_key")),
    };
    store(
        &admin,
        &RemoteSettings {
            url,
            branch,
            auth: body.auth,
            credential_id,
            ssh_public_key,
        },
    )
    .await?;
    Ok(Json(view_of(&admin).await?))
}

#[utoipa::path(
    delete, path = "/api/v1/config-backup-remote", tag = "nodes",
    responses(
        (status = 204, description = "Removed, with its sealed credential. Config backups are kept on the box only"),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "The caller lacks manage-system", body = super::error::ErrorBody),
        (status = 404, description = "No destination is set (`no_config_backup_remote`)", body = super::error::ErrorBody),
        (status = 503, description = "This deployment has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn delete_remote(_perm: RequireManageSystem, admin: Admin) -> ApiResult<StatusCode> {
    let credential = admin
        .config_remote
        .delete()
        .await
        .map_err(|e| write_failed(&e))?
        .ok_or_else(no_destination)?;
    if let Err(e) = admin.creds.delete(credential).await {
        tracing::warn!(error = %e, "config backup: the Git credential was not deleted");
    }
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    post, path = "/api/v1/config-backup-remote/ssh-key", tag = "nodes",
    responses(
        (status = 200, description = "A new key; the old one no longer works and the new public half must be registered", body = RemoteView),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "The caller lacks manage-system", body = super::error::ErrorBody),
        (status = 404, description = "No destination is set (`no_config_backup_remote`)", body = super::error::ErrorBody),
        (status = 409, description = "The destination uses a token, not a key (`not_ssh_key`)", body = super::error::ErrorBody),
        (status = 503, description = "This deployment has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn regenerate_key(_perm: RequireManageSystem, admin: Admin) -> ApiResult<Json<RemoteView>> {
    let remote = admin
        .config_remote
        .get()
        .await
        .map_err(|e| read_failed(&e))?
        .ok_or_else(no_destination)?;
    if remote.auth != GitAuth::SshKey {
        return Err(ApiError::conflict(
            "not_ssh_key",
            "the destination uses a token, not an SSH key",
        ));
    }
    let (private_key, public) = config_remote::generate_ssh_key();
    let credential_id = seal(
        &admin,
        Some(remote.credential_id),
        &GitRemoteSecret::SshKey { private_key },
    )
    .await?;
    store(
        &admin,
        &RemoteSettings {
            url: remote.url,
            branch: remote.branch,
            auth: GitAuth::SshKey,
            credential_id,
            ssh_public_key: Some(public),
        },
    )
    .await?;
    Ok(Json(view_of(&admin).await?))
}

#[utoipa::path(
    delete, path = "/api/v1/config-backup-remote/host-keys", tag = "nodes",
    responses(
        (status = 204, description = "Forgotten; the next SSH push learns the destination's host keys again"),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "The caller lacks manage-system", body = super::error::ErrorBody),
        (status = 404, description = "No destination is set (`no_config_backup_remote`)", body = super::error::ErrorBody),
        (status = 503, description = "This deployment has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn forget_host_keys(_perm: RequireManageSystem, admin: Admin) -> ApiResult<StatusCode> {
    if admin
        .config_remote
        .forget_host_keys()
        .await
        .map_err(|e| write_failed(&e))?
    {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(no_destination())
    }
}

/// How the last push went. Shared by REST and MCP.
pub(crate) async fn push_status_of(admin: &super::AdminState) -> ApiResult<PushStatusView> {
    let remote = admin
        .config_remote
        .get()
        .await
        .map_err(|e| read_failed(&e))?;
    Ok(PushStatusView::of(remote.as_ref()))
}

#[utoipa::path(
    get, path = "/api/v1/config-backup-remote/status", tag = "nodes",
    responses(
        (status = 200, description = "Whether config backups are reaching their Git destination: its host, the last push that arrived, the last failure", body = PushStatusView),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 503, description = "This deployment has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn push_status(_perm: RequireView, admin: Admin) -> ApiResult<Json<PushStatusView>> {
    Ok(Json(push_status_of(&admin).await?))
}

/// What the push script asks with.
#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub(super) struct PushQuery {
    /// The head of oxidized's repository.
    head: String,
}

/// One push, for the push script: where, as what, and the patterns its output is read by.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct PushJob {
    /// Echoed in the report, so a push made for a destination that has since changed is ignored.
    generation: i64,
    url: String,
    branch: String,
    auth: GitAuth,
    username: Option<String>,
    /// The HTTPS token, in plaintext, for this push alone.
    token: Option<String>,
    /// The SSH private key, in plaintext, for this push alone.
    private_key: Option<String>,
    /// The pinned host keys in known_hosts form; empty before the first SSH push.
    known_hosts: String,
    /// `[failure, lower-case text]` pairs, matched in order against git's lower-cased output.
    patterns: Vec<Vec<String>>,
    /// `[node id, path]` pairs: the file each node's configuration is pushed as (stage 4 decision
    /// 38). A node oxidized has a file for and this does not name is pushed as
    /// `deleted/<id>.txt`.
    files: Vec<Vec<String>>,
    /// The digest of `files`, echoed in the report.
    layout: String,
}

#[utoipa::path(
    get, path = "/internal/oxidized/push", tag = "nodes",
    params(PushQuery),
    responses(
        (status = 200, description = "Push this head: the destination with its token or private key in plaintext. For the bundled oxidized alone: it demands the source token in `x-yagra-oxidized-token`", body = PushJob),
        (status = 204, description = "Nothing to push: no destination, the head is already there, or a recent push failed"),
        (status = 400, description = "Not a version id (`invalid_oid`)", body = super::error::ErrorBody),
        (status = 401, description = "No token, or the wrong one (`oxidized_token`)", body = super::error::ErrorBody),
        (status = 503, description = "This deployment has no config backup (`config_backup_unavailable`)", body = super::error::ErrorBody),
    ),
)]
async fn push_ask(
    admin: Admin,
    headers: HeaderMap,
    Query(q): Query<PushQuery>,
) -> ApiResult<axum::response::Response> {
    use axum::response::IntoResponse;
    let ox = oxidized_of(&admin)?;
    check_source_token(ox, &headers)?;
    check_oid(&q.head)?;
    let remote = admin
        .config_remote
        .get()
        .await
        .map_err(|e| read_failed(&e))?;
    let Some(remote) = remote else {
        return Ok(StatusCode::NO_CONTENT.into_response());
    };
    // The names the files are pushed under, as of now: a node renamed or deleted since the last
    // push changes them without a new version (decision 40).
    if let Err(e) = admin.config_backup.remember_names().await {
        tracing::warn!(error = %e, "config backup: node names could not be remembered");
    }
    let names = admin
        .config_backup
        .export_names()
        .await
        .map_err(|e| read_failed(&e))?;
    let files = config_remote::export_paths(&names);
    let layout = config_remote::layout_digest(&files);
    if !config_remote::should_push(&remote, &q.head, &layout, Utc::now()) {
        return Ok(StatusCode::NO_CONTENT.into_response());
    }
    let Some(secret) = sealed_secret(&admin, &remote).await? else {
        tracing::warn!("config backup: the Git destination's credential cannot be opened");
        return Ok(StatusCode::NO_CONTENT.into_response());
    };
    let (username, token, private_key) = match secret {
        GitRemoteSecret::HttpsToken { username, token } => (Some(username), Some(token), None),
        GitRemoteSecret::SshKey { private_key } => (None, None, Some(private_key)),
    };
    metrics::counter!("yagra_config_push_jobs_total").increment(1);
    let job = PushJob {
        generation: remote.generation,
        url: remote.url,
        branch: remote.branch,
        auth: remote.auth,
        username,
        token,
        private_key,
        known_hosts: remote.known_hosts.unwrap_or_default(),
        patterns: config_remote::PUSH_PATTERNS
            .iter()
            .map(|(kind, needle)| vec![kind.as_str().to_owned(), (*needle).to_owned()])
            .collect(),
        files: files
            .into_iter()
            .map(|(id, path)| vec![id.to_string(), path])
            .collect(),
        layout,
    };
    Ok(([(axum::http::header::CACHE_CONTROL, "no-store")], Json(job)).into_response())
}

/// How one push went, as the push script reports it.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub(super) struct PushReport {
    generation: i64,
    head: String,
    /// Empty when the push succeeded; otherwise the failure core's patterns named.
    #[serde(default)]
    failure: String,
    /// The host keys the first SSH push met, in known_hosts form.
    #[serde(default)]
    known_hosts: Option<String>,
    /// The job's `layout`, echoed.
    #[serde(default)]
    layout: String,
    /// The commit pushed; required when the push succeeded.
    #[serde(default)]
    commit: String,
}

#[utoipa::path(
    post, path = "/internal/oxidized/push", tag = "nodes",
    request_body(content = PushReport, content_type = "application/x-www-form-urlencoded"),
    responses(
        (status = 204, description = "Recorded, or ignored when the destination changed since the push was asked for. For the bundled oxidized alone: it demands the source token in `x-yagra-oxidized-token`"),
        (status = 400, description = "Not a version id (`invalid_oid`), or a success with no layout digest (`invalid_layout`)", body = super::error::ErrorBody),
        (status = 401, description = "No token, or the wrong one (`oxidized_token`)", body = super::error::ErrorBody),
        (status = 503, description = "This deployment has no config backup (`config_backup_unavailable`)", body = super::error::ErrorBody),
    ),
)]
async fn push_report(
    admin: Admin,
    headers: HeaderMap,
    Form(report): Form<PushReport>,
) -> ApiResult<StatusCode> {
    let ox = oxidized_of(&admin)?;
    check_source_token(ox, &headers)?;
    check_oid(&report.head)?;
    let failure = match report.failure.trim() {
        "" => None,
        token => Some(PushFailure::from_token(token).unwrap_or(PushFailure::Other)),
    };
    let learned = report
        .known_hosts
        .as_deref()
        .and_then(config_remote::check_known_hosts);
    let pushed = if failure.is_none() {
        check_oid(&report.commit)?;
        if report.layout.len() != 64 || !report.layout.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(ApiError::bad_request(
                "invalid_layout",
                "not a layout digest",
            ));
        }
        Some(config_remote::Pushed {
            head: &report.head,
            layout: &report.layout,
            commit: &report.commit,
        })
    } else {
        None
    };
    let recorded = admin
        .config_remote
        .record_push(
            report.generation,
            pushed.as_ref(),
            failure,
            learned.as_deref(),
        )
        .await
        .map_err(|e| write_failed(&e))?;
    let outcome = failure.map_or("success", PushFailure::as_str);
    metrics::counter!("yagra_config_pushes_total", "outcome" => outcome).increment(1);
    if recorded {
        match failure {
            Some(f) => tracing::warn!(failure = f.as_str(), "config backup: a push failed"),
            None => tracing::info!("config backup: pushed to the Git destination"),
        }
    }
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::api::tests_support::{live_state, send, token};
    use crate::oxidized;

    const PATH: &str = "/api/v1/config-backup-remote";

    /// An administrator sets an HTTPS destination; the token is sealed and never comes back, a
    /// later edit without it keeps it, and an Operator can neither read nor change the settings.
    #[sqlx::test(migrator = "yagra_base::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn an_admin_sets_a_destination_and_the_token_never_comes_back(pool: sqlx::PgPool) {
        let st = live_state(pool.clone()).await;
        let admin = token(&st, yagra_common::Role::Admin);
        let operator = token(&st, yagra_common::Role::Operator);

        let (status, body) = send(&st, "GET", PATH, &admin, None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["configured"], false);

        let set = serde_json::json!({
            "url": "https://example.com/acme/configs.git", "auth": "https_token",
            "username": "x-access-token", "token": "tok-SECRET-1",
        });
        let (status, body) = send(&st, "PUT", PATH, &admin, Some(set)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["configured"], true);
        assert_eq!(body["branch"], "main");
        assert_eq!(body["username"], "x-access-token");
        assert!(!body.to_string().contains("tok-SECRET-1"), "{body}");

        // The token is sealed as an integration key, which Show secret refuses.
        let remote = st
            .admin
            .as_ref()
            .unwrap()
            .config_remote
            .get()
            .await
            .unwrap()
            .unwrap();
        let reveal = format!("/api/v1/credentials/{}/reveal", remote.credential_id);
        let (status, _) = send(&st, "POST", &reveal, &admin, None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        // An edit without a token keeps the sealed one, in the same credential.
        let edit = serde_json::json!({
            "url": "https://example.com/acme/configs.git", "branch": "backups",
            "auth": "https_token", "username": "bot",
        });
        let (status, body) = send(&st, "PUT", PATH, &admin, Some(edit)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["branch"], "backups");
        let again = st
            .admin
            .as_ref()
            .unwrap()
            .config_remote
            .get()
            .await
            .unwrap()
            .unwrap();
        assert_eq!(again.credential_id, remote.credential_id);
        assert_eq!(again.generation, 2);

        // The sealed token is never sent to another host: moving the URL needs a new one.
        let moved = serde_json::json!({
            "url": "https://example.org/acme/configs.git", "auth": "https_token",
            "username": "bot",
        });
        let (status, body) = send(&st, "PUT", PATH, &admin, Some(moved)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(body["error"]["code"], "token_required_for_new_address");

        for bad in [
            serde_json::json!({"url": "http://example.com/a.git", "auth": "https_token",
                               "username": "u", "token": "t"}),
            serde_json::json!({"url": "git@example.com:a.git", "auth": "https_token",
                               "username": "u", "token": "t"}),
            serde_json::json!({"url": "https://example.com/a.git", "auth": "https_token",
                               "username": "", "token": "t"}),
            serde_json::json!({"url": "https://example.com/a.git", "branch": "-f",
                               "auth": "https_token", "username": "u", "token": "t"}),
        ] {
            let (status, body) = send(&st, "PUT", PATH, &admin, Some(bad)).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
            assert_eq!(body["error"]["code"], "invalid_config_backup_remote");
        }

        let (status, _) = send(&st, "GET", PATH, &operator, None).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (status, _) = send(&st, "DELETE", PATH, &operator, None).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        // Whether pushes arrive is anyone's to read, without the path.
        let viewer = token(&st, yagra_common::Role::Viewer);
        let (status, body) = send(&st, "GET", &format!("{PATH}/status"), &viewer, None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["host"], "example.com");
        assert!(!body.to_string().contains("acme"), "{body}");

        let (status, _) = send(&st, "DELETE", PATH, &admin, None).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        assert_eq!(
            yagra_base::pgtest::rows(&pool, "config_backup_remote").await,
            0
        );
        let (_, body) = send(&st, "GET", PATH, &admin, None).await;
        assert_eq!(body["configured"], false);
        let (status, _) = send(&st, "DELETE", PATH, &admin, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    /// An SSH destination gets a key Yagra made, shows only its public half, keeps it across an
    /// edit, and replaces it on request.
    #[sqlx::test(migrator = "yagra_base::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn an_ssh_destination_gets_a_generated_key_and_keeps_it(pool: sqlx::PgPool) {
        let st = live_state(pool.clone()).await;
        let admin = token(&st, yagra_common::Role::Admin);
        let set = serde_json::json!({"url": "git@example.com:acme/configs.git", "auth": "ssh_key"});
        let (status, body) = send(&st, "PUT", PATH, &admin, Some(set.clone())).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let public = body["ssh_public_key"]
            .as_str()
            .expect("a public key")
            .to_owned();
        assert!(public.starts_with("ssh-ed25519 "), "{public}");
        assert!(!body.to_string().contains("PRIVATE KEY"), "{body}");

        let (_, body) = send(&st, "PUT", PATH, &admin, Some(set)).await;
        assert_eq!(
            body["ssh_public_key"], public,
            "an edit keeps the registered key"
        );

        let (status, body) = send(&st, "POST", &format!("{PATH}/ssh-key"), &admin, None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_ne!(body["ssh_public_key"], public);
        let (status, _) = send(&st, "DELETE", &format!("{PATH}/host-keys"), &admin, None).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
    }

    /// The push script's door: closed without the token; with it, a job carrying the secret for a
    /// head not yet pushed, a report recorded, and nothing to do for the head that arrived.
    #[sqlx::test(migrator = "yagra_base::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn the_push_script_is_handed_a_job_and_reports_it(pool: sqlx::PgPool) {
        use axum::body::Body;
        use tower::ServiceExt;
        let home = std::env::temp_dir().join(format!("yagra-ox-push-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&home).expect("dir");
        let ox_token = oxidized::config::ensure_token(&home).expect("token");
        let st = crate::api::tests_support::live_state_with_oxidized(
            pool.clone(),
            Arc::new(crate::api::config_backup::OxidizedHandle {
                settings: oxidized::Settings {
                    url: "http://127.0.0.1:9".to_owned(),
                    home: home.clone(),
                },
                client: Arc::new(oxidized::client::OxidizedClient::new("http://127.0.0.1:9")),
            }),
        )
        .await;
        let admin = token(&st, yagra_common::Role::Admin);
        let set = serde_json::json!({
            "url": "https://example.com/acme/configs.git", "auth": "https_token",
            "username": "bot", "token": "tok-SECRET-2",
        });
        let (status, _) = send(&st, "PUT", PATH, &admin, Some(set)).await;
        assert_eq!(status, StatusCode::OK);
        // A backed-up node, so the job carries a file name for it.
        let node = yagra_base::pgtest::node(&pool, "sw-01", 1, None).await;
        let backups = yagra_base::config_backup::ConfigBackupRepo::new(pool.clone());
        backups
            .put(
                node,
                &yagra_base::config_backup::NodeConfigBackup {
                    enabled: true,
                    login_order: None,
                    input: yagra_base::config_backup::BackupInput::Ssh,
                    model_override: Some("ios".to_owned()),
                    scan_mode: yagra_base::config_backup::ScanMode::Normal,
                },
                true,
            )
            .await
            .expect("put");

        let head = "a".repeat(40);
        let router = crate::api::router(st.clone());
        let ask = |presented: &str| {
            axum::http::Request::builder()
                .method("GET")
                .uri(format!("/internal/oxidized/push?head={head}"))
                .header(oxidized::config::TOKEN_HEADER, presented)
                .body(Body::empty())
                .expect("request")
        };
        let res = router.clone().oneshot(ask("wrong")).await.expect("send");
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        let res = router.clone().oneshot(ask(&ox_token)).await.expect("send");
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(res.headers()[axum::http::header::CACHE_CONTROL], "no-store");
        let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
            .await
            .expect("body");
        let job: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
        assert_eq!(job["token"], "tok-SECRET-2");
        assert_eq!(job["generation"], 1);
        assert_eq!(job["patterns"][0][0], "host_key");
        assert_eq!(
            job["files"],
            serde_json::json!([[node.to_string(), "sw-01.txt"]])
        );
        let layout = job["layout"].as_str().expect("layout").to_owned();

        let report = |form: String| {
            axum::http::Request::builder()
                .method("POST")
                .uri("/internal/oxidized/push")
                .header("content-type", "application/x-www-form-urlencoded")
                .header(oxidized::config::TOKEN_HEADER, ox_token.as_str())
                .body(Body::from(form))
                .expect("request")
        };
        let res = router
            .clone()
            .oneshot(report(format!("generation=1&head={head}&failure=auth")))
            .await
            .expect("send");
        assert_eq!(res.status(), StatusCode::NO_CONTENT);
        let remote = st
            .admin
            .as_ref()
            .unwrap()
            .config_remote
            .get()
            .await
            .unwrap()
            .unwrap();
        assert_eq!(remote.failure, Some(PushFailure::Auth));
        // Within ten minutes of a failure there is nothing to do.
        let res = router.clone().oneshot(ask(&ox_token)).await.expect("send");
        assert_eq!(res.status(), StatusCode::NO_CONTENT);

        // A success names the commit the destination now holds and the layout it was made with.
        let res = router
            .clone()
            .oneshot(report(format!("generation=1&head={head}&failure=")))
            .await
            .expect("send");
        assert_eq!(res.status(), StatusCode::BAD_REQUEST, "no commit");
        let commit = "c".repeat(40);
        let res = router
            .clone()
            .oneshot(report(format!(
                "generation=1&head={head}&failure=&layout={layout}&commit={commit}"
            )))
            .await
            .expect("send");
        assert_eq!(res.status(), StatusCode::NO_CONTENT);
        let remote = st
            .admin
            .as_ref()
            .unwrap()
            .config_remote
            .get()
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            (remote.pushed_head.as_deref(), remote.failure),
            (Some(head.as_str()), None)
        );
        assert_eq!(remote.pushed_commit.as_deref(), Some(commit.as_str()));
        let res = router.clone().oneshot(ask(&ox_token)).await.expect("send");
        assert_eq!(res.status(), StatusCode::NO_CONTENT, "already there");

        // A rename changes where the file goes, so the same head is pushed again under the new
        // name; once the node is deleted it moves to `deleted/`.
        sqlx::query("UPDATE nodes SET name = 'sw-01a' WHERE id = $1")
            .bind(node)
            .execute(&pool)
            .await
            .expect("rename");
        let res = router.clone().oneshot(ask(&ox_token)).await.expect("send");
        assert_eq!(res.status(), StatusCode::OK, "a rename is pushed");
        let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
            .await
            .expect("body");
        let job: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
        assert_eq!(job["files"][0][1], "sw-01a.txt");
        sqlx::query("DELETE FROM nodes WHERE id = $1")
            .bind(node)
            .execute(&pool)
            .await
            .expect("delete");
        let res = router.oneshot(ask(&ox_token)).await.expect("send");
        let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
            .await
            .expect("body");
        let job: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
        assert_eq!(job["files"][0][1], "deleted/sw-01a.txt");
        std::fs::remove_dir_all(&home).ok();
    }
}
