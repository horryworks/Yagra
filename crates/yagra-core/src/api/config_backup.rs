// SPDX-License-Identifier: AGPL-3.0-only
//! Config backup through the bundled oxidized (ADR-210 stage 2).
//!
//! Per node, addressed by path (`NodeScoped`):
//!
//! - `GET …/config-backup` — what is configured, whether the node is fetched and why not, and what
//!   oxidized last reported. `View`.
//! - `PUT …/config-backup` — switch it on or off, pick the CLI login, SSH or telnet, the model.
//!   `ManageConfig`. oxidized is asked to re-read its node list straight after.
//! - `GET …/config-backup/versions` — the stored versions. `View`.
//! - `GET …/config-backup/versions/{oid}` and `GET …/config-backup/diff` — the text, and two
//!   versions side by side. **`ManageConfig`** (decision 10): oxidized's `remove_secret` is model by
//!   model and can miss one, so a Viewer does not read device configuration. Neither has an MCP
//!   tool, for the same reason (decision 16).
//! - `POST …/config-backup/fetch` — fetch now. `ManageConfig`.
//!
//! And one route for oxidized itself, outside `/api/v1`: `GET /internal/oxidized/nodes`, the HTTP
//! source oxidized reads its node list from. **It returns device logins in plaintext** — the second
//! reader of plaintext after `POST /credentials/{id}/reveal` (security.md). nginx forwards only
//! `/api/` and `/mcp`, so the TLS edge never serves it; core's own port is on the LAN, so it demands
//! the source token the one-shot `oxidized-config` wrote, compared in constant time. It is not
//! audited per call (oxidized reads it every few minutes); `yagra_oxidized_source_reads_total`
//! counts it instead.

use std::sync::Arc;

use axum::{
    extract::{Path, Query},
    http::{HeaderMap, StatusCode},
    routing::{get, post},
    Json, Router,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use yagra_base::config_backup::{BackupInput, NodeConfigBackup};

use super::error::{ApiError, ApiResult};
use super::extract::{
    Admin, RequireManageConfig, RequireRevealCredentials, RequireView, Scoped, VisibleNode,
};
use super::ApiState;
use crate::oxidized::{self, client::OxidizedClient, source::Readiness};

/// This domain's slice of the OpenAPI document (ADR-035), merged by [`super::openapi::document`].
#[derive(utoipa::OpenApi)]
#[openapi(paths(
    get_config_backup,
    put_config_backup,
    list_versions,
    get_version,
    get_diff,
    fetch_now,
    oxidized_source
))]
pub(super) struct Doc;

/// The config-backup routes, merged by [`super::router`].
pub(super) fn routes() -> Router<ApiState> {
    Router::new()
        .route(
            "/api/v1/nodes/:node_id/config-backup",
            get(get_config_backup).put(put_config_backup),
        )
        .route(
            "/api/v1/nodes/:node_id/config-backup/versions",
            get(list_versions),
        )
        .route(
            "/api/v1/nodes/:node_id/config-backup/versions/:oid",
            get(get_version),
        )
        .route("/api/v1/nodes/:node_id/config-backup/diff", get(get_diff))
        .route(
            "/api/v1/nodes/:node_id/config-backup/fetch",
            post(fetch_now),
        )
        .route("/internal/oxidized/nodes", get(oxidized_source))
}

/// What this core holds to reach the bundled oxidized.
pub(crate) struct OxidizedHandle {
    pub settings: oxidized::Settings,
    pub client: Arc<OxidizedClient>,
}

fn no_oxidized() -> ApiError {
    ApiError::unavailable(
        "config_backup_unavailable",
        "this deployment has no config backup (YAGRA_OXIDIZED_URL is unset)",
    )
}

fn oxidized_of(admin: &super::AdminState) -> Result<&OxidizedHandle, ApiError> {
    admin.oxidized.as_deref().ok_or_else(no_oxidized)
}

fn oxidized_failed(e: &anyhow::Error) -> ApiError {
    tracing::warn!(error = %e, "config backup: oxidized did not answer");
    ApiError::bad_gateway(
        "oxidized_unreachable",
        "the config backup service did not answer",
    )
}

/// What oxidized last reported.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct BackupStatusView {
    /// oxidized's word: `success`, `no_connection`, `fail`, `timelimit` or `never`.
    status: String,
    last_end: Option<DateTime<Utc>>,
    /// When Yagra last read it. Older than ten minutes means oxidized has not been answering.
    observed_at: DateTime<Utc>,
}

/// The part of a node's config backup MCP's `get_node_status` carries (ADR-210 decision 16).
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub(crate) struct ConfigBackupSummary {
    readiness: Readiness,
    /// oxidized's word for the last fetch; `null` ⇒ never reported.
    last_status: Option<String>,
    last_end: Option<DateTime<Utc>>,
}

impl ConfigBackupSummary {
    /// A populated instance, for the MCP result canary.
    #[cfg(test)]
    pub(crate) fn sample() -> Self {
        Self {
            readiness: Readiness::Ready,
            last_status: Some("success".to_owned()),
            last_end: Some(DateTime::<Utc>::UNIX_EPOCH),
        }
    }
}

/// The summary for one node, from the same reads the REST view makes. `None` when a read failed.
pub(crate) async fn summary_of(
    admin: &super::AdminState,
    node: Uuid,
) -> Option<ConfigBackupSummary> {
    let cfg = admin.config_backup.get(node).await.ok()?;
    let profile = admin.config_backup.profile_name(node).await.ok()?;
    let kind = credential_kind(admin, cfg.as_ref().and_then(|c| c.credential_id))
        .await
        .ok()?;
    let status = admin.config_backup.status(node).await.ok()?;
    let moved = admin.config_backup.address_changed(node).await.ok()?;
    Some(ConfigBackupSummary {
        readiness: oxidized::source::readiness(
            cfg.as_ref(),
            profile.as_deref(),
            kind.as_deref(),
            moved,
        ),
        last_status: status.as_ref().map(|s| s.status.clone()),
        last_end: status.and_then(|s| s.last_end),
    })
}

/// One node's config backup.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct ConfigBackupView {
    enabled: bool,
    credential_id: Option<Uuid>,
    input: BackupInput,
    /// The model a person picked; `null` means "from the profile".
    model_override: Option<String>,
    /// The model the node's profile maps to, if any.
    profile_model: Option<String>,
    /// Whether it is fetched, or the one thing to fix.
    readiness: Readiness,
    status: Option<BackupStatusView>,
    /// The models a person may pick.
    models: Vec<String>,
    /// Whether this deployment has an oxidized at all.
    available: bool,
}

/// The kind of a stored credential, or `None` when it no longer exists.
async fn credential_kind(
    admin: &super::AdminState,
    id: Option<Uuid>,
) -> Result<Option<String>, ApiError> {
    let Some(id) = id else { return Ok(None) };
    let list = admin.creds.list().await.map_err(|e| {
        ApiError::from_internal(e.as_ref(), "list credentials", "failed to read credentials")
    })?;
    Ok(list.into_iter().find(|c| c.id == id).map(|c| c.kind))
}

#[utoipa::path(
    get, path = "/api/v1/nodes/{node_id}/config-backup", tag = "nodes",
    params(("node_id" = Uuid, Path, description = "Node id")),
    responses(
        (status = 200, description = "The node's config backup: configuration, whether it is fetched, the last result", body = ConfigBackupView),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 404, description = "No such node the caller can see", body = super::error::ErrorBody),
        (status = 503, description = "This deployment has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn get_config_backup(
    _perm: RequireView,
    _visible: VisibleNode,
    admin: Admin,
    Path(node_id): Path<Uuid>,
) -> ApiResult<Json<ConfigBackupView>> {
    let read = |e: anyhow::Error| {
        ApiError::from_internal(e.as_ref(), "config backup", "failed to read config backup")
    };
    let cfg = admin.config_backup.get(node_id).await.map_err(read)?;
    let profile = admin
        .config_backup
        .profile_name(node_id)
        .await
        .map_err(read)?;
    let kind = credential_kind(&admin, cfg.as_ref().and_then(|c| c.credential_id)).await?;
    let status = admin.config_backup.status(node_id).await.map_err(read)?;
    let moved = admin
        .config_backup
        .address_changed(node_id)
        .await
        .map_err(read)?;
    let readiness =
        oxidized::source::readiness(cfg.as_ref(), profile.as_deref(), kind.as_deref(), moved);
    let cfg = cfg.unwrap_or(NodeConfigBackup {
        enabled: false,
        credential_id: None,
        input: BackupInput::Ssh,
        model_override: None,
    });
    Ok(Json(ConfigBackupView {
        enabled: cfg.enabled,
        credential_id: cfg.credential_id,
        input: cfg.input,
        model_override: cfg.model_override,
        profile_model: oxidized::source::model_of(None, profile.as_deref()),
        readiness,
        status: status.map(|s| BackupStatusView {
            status: s.status,
            last_end: s.last_end,
            observed_at: s.observed_at,
        }),
        models: yagra_common::oxidized_model::MODELS
            .iter()
            .map(|m| (*m).to_owned())
            .collect(),
        available: admin.oxidized.is_some(),
    }))
}

/// A node's config-backup configuration.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub(super) struct ConfigBackupBody {
    enabled: bool,
    #[serde(default)]
    credential_id: Option<Uuid>,
    input: BackupInput,
    #[serde(default)]
    model_override: Option<String>,
}

#[utoipa::path(
    put, path = "/api/v1/nodes/{node_id}/config-backup", tag = "nodes",
    params(("node_id" = Uuid, Path, description = "Node id")),
    request_body = ConfigBackupBody,
    responses(
        (status = 204, description = "Saved; the bundled oxidized is asked to re-read its node list"),
        (status = 400, description = "An unknown input or model, or a credential that is not a CLI login (`invalid_config_backup`)", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig; or a login is named by an account restricted to folders (`scope_unsupported`) or by a caller without RevealCredentials, such as an API token (`login_not_permitted`)", body = super::error::ErrorBody),
        (status = 404, description = "No such node the caller can see", body = super::error::ErrorBody),
        (status = 503, description = "This deployment has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn put_config_backup(
    _perm: RequireManageConfig,
    // Optional: needed only to choose a login (below). An API token never holds it.
    reveal: Option<RequireRevealCredentials>,
    _visible: VisibleNode,
    Scoped(scope): Scoped,
    admin: Admin,
    Path(node_id): Path<Uuid>,
    Json(body): Json<ConfigBackupBody>,
) -> ApiResult<StatusCode> {
    let bad = |m: &str| ApiError::bad_request("invalid_config_backup", m.to_owned());
    if body.input == BackupInput::Unknown {
        return Err(bad("input must be ssh, telnet or ssh_then_telnet"));
    }
    let model_override = body
        .model_override
        .map(|m| m.trim().to_owned())
        .filter(|m| !m.is_empty());
    if let Some(m) = model_override.as_deref() {
        if !yagra_common::oxidized_model::is_model(m) {
            return Err(bad("model is not one Yagra offers"));
        }
    }
    if body.credential_id.is_some() {
        refuse_scoped_login(&scope)?;
        // Choosing a login hands it to the node's address, so it takes what reading one back takes:
        // a caller that may not see the password must not be able to send it somewhere it chose.
        if reveal.is_none() {
            return Err(ApiError::forbidden_code(
                "login_not_permitted",
                "choosing a CLI login needs the Reveal credentials permission, which an API token \
                 never holds",
            ));
        }
        match credential_kind(&admin, body.credential_id)
            .await?
            .as_deref()
        {
            Some(yagra_base::secrets::KIND_CLI_LOGIN) => {}
            Some(_) => return Err(bad("the credential must be a CLI login")),
            None => return Err(bad("no such credential")),
        }
    }
    let cfg = NodeConfigBackup {
        enabled: body.enabled,
        credential_id: body.credential_id,
        input: body.input,
        model_override,
    };
    let saved = admin.config_backup.put(node_id, &cfg).await.map_err(|e| {
        ApiError::from_internal(
            e.as_ref(),
            "save config backup",
            "failed to save config backup",
        )
    })?;
    if !saved {
        return Err(ApiError::not_found(
            "node_not_found",
            format!("no node {node_id}"),
        ));
    }
    // Best effort, off the request: the status loop reloads within five minutes anyway.
    if let Some(ox) = admin.oxidized.clone() {
        tokio::spawn(async move {
            if let Err(e) = ox.client.reload().await {
                tracing::warn!(error = %e, "config backup: oxidized did not reload after a change");
            }
        });
    }
    Ok(StatusCode::NO_CONTENT)
}

/// A CLI login is shared by every folder, like every credential (`credentials_are_deployment_wide`).
/// A folder-scoped caller could otherwise bind one to a node of theirs whose address they control,
/// and oxidized would hand that node's address the password. Switching off, or saving with no
/// login, stays theirs.
fn refuse_scoped_login(scope: &super::scope::NodeScope) -> Result<(), ApiError> {
    super::scope::require_fleet_wide(
        scope,
        "a CLI login is shared by every folder, so an account restricted to folders cannot \
         choose one for config backup",
    )
}

/// One stored version.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct ConfigVersion {
    oid: String,
    /// When it was stored. `null` when oxidized printed a time this could not read.
    stored_at: Option<DateTime<Utc>>,
}

#[utoipa::path(
    get, path = "/api/v1/nodes/{node_id}/config-backup/versions", tag = "nodes",
    params(("node_id" = Uuid, Path, description = "Node id")),
    responses(
        (status = 200, description = "Stored versions, newest first", body = Vec<ConfigVersion>),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 404, description = "No such node the caller can see", body = super::error::ErrorBody),
        (status = 502, description = "The bundled oxidized did not answer (`oxidized_unreachable`)", body = super::error::ErrorBody),
        (status = 503, description = "This deployment has no config backup (`config_backup_unavailable`)", body = super::error::ErrorBody),
    ),
)]
async fn list_versions(
    _perm: RequireView,
    _visible: VisibleNode,
    admin: Admin,
    Path(node_id): Path<Uuid>,
) -> ApiResult<Json<Vec<ConfigVersion>>> {
    let ox = oxidized_of(&admin)?;
    // 🚨 A node oxidized is not fetching is not in its list, and oxidized answers its history with a
    // 500 (`NodeNotFound`), which read as "the config backup service did not answer" under the table
    // of every node not switched on yet — measured on a lab deployment. Such a node has no history
    // oxidized can serve, so the answer is none, without asking.
    let ready = summary_of(&admin, node_id)
        .await
        .is_some_and(|s| s.readiness == Readiness::Ready);
    if !ready {
        return Ok(Json(Vec::new()));
    }
    let versions = ox
        .client
        .versions(node_id)
        .await
        .map_err(|e| oxidized_failed(&e))?;
    Ok(Json(
        versions
            .into_iter()
            .map(|v| ConfigVersion {
                stored_at: v.date.as_deref().and_then(oxidized::client::parse_time),
                oid: v.oid,
            })
            .collect(),
    ))
}

/// A Git object id: 40 lowercase hexadecimal characters. Checked before it reaches oxidized's
/// query string.
fn check_oid(oid: &str) -> Result<(), ApiError> {
    if oid.len() == 40
        && oid
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        Ok(())
    } else {
        Err(ApiError::bad_request("invalid_oid", "not a version id"))
    }
}

/// One version's text.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct ConfigText {
    oid: String,
    text: String,
}

#[utoipa::path(
    get, path = "/api/v1/nodes/{node_id}/config-backup/versions/{oid}", tag = "nodes",
    params(
        ("node_id" = Uuid, Path, description = "Node id"),
        ("oid" = String, Path, description = "Version id from `…/versions`"),
    ),
    responses(
        (status = 200, description = "The stored configuration text", body = ConfigText),
        (status = 400, description = "Not a version id (`invalid_oid`)", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
        (status = 404, description = "No such node the caller can see", body = super::error::ErrorBody),
        (status = 502, description = "The bundled oxidized did not answer (`oxidized_unreachable`)", body = super::error::ErrorBody),
        (status = 503, description = "This deployment has no config backup (`config_backup_unavailable`)", body = super::error::ErrorBody),
    ),
)]
async fn get_version(
    _perm: RequireManageConfig,
    _visible: VisibleNode,
    admin: Admin,
    Path((node_id, oid)): Path<(Uuid, String)>,
) -> ApiResult<Json<ConfigText>> {
    check_oid(&oid)?;
    let ox = oxidized_of(&admin)?;
    let text = ox
        .client
        .version_text(node_id, &oid)
        .await
        .map_err(|e| oxidized_failed(&e))?;
    Ok(Json(ConfigText { oid, text }))
}

/// Which two versions to compare.
#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub(super) struct DiffQuery {
    /// The older version.
    from: String,
    /// The newer version.
    to: String,
}

/// How a line of a diff relates the two versions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DiffLineKind {
    /// In both.
    Same,
    /// Only in the newer.
    Added,
    /// Only in the older.
    Removed,
}

/// One line of a diff.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct DiffLine {
    kind: DiffLineKind,
    text: String,
}

/// Two versions compared line by line.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct ConfigDiff {
    from: String,
    to: String,
    lines: Vec<DiffLine>,
}

/// The line diff of two texts. Pure, so the arithmetic is tested without oxidized.
fn diff_lines(old: &str, new: &str) -> Vec<DiffLine> {
    similar::TextDiff::from_lines(old, new)
        .iter_all_changes()
        .map(|c| DiffLine {
            kind: match c.tag() {
                similar::ChangeTag::Equal => DiffLineKind::Same,
                similar::ChangeTag::Insert => DiffLineKind::Added,
                similar::ChangeTag::Delete => DiffLineKind::Removed,
            },
            text: c.value().trim_end_matches(['\r', '\n']).to_owned(),
        })
        .collect()
}

#[utoipa::path(
    get, path = "/api/v1/nodes/{node_id}/config-backup/diff", tag = "nodes",
    params(("node_id" = Uuid, Path, description = "Node id"), DiffQuery),
    responses(
        (status = 200, description = "The two versions compared line by line", body = ConfigDiff),
        (status = 400, description = "Not a version id (`invalid_oid`)", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
        (status = 404, description = "No such node the caller can see", body = super::error::ErrorBody),
        (status = 502, description = "The bundled oxidized did not answer (`oxidized_unreachable`)", body = super::error::ErrorBody),
        (status = 503, description = "This deployment has no config backup (`config_backup_unavailable`)", body = super::error::ErrorBody),
    ),
)]
async fn get_diff(
    _perm: RequireManageConfig,
    _visible: VisibleNode,
    admin: Admin,
    Path(node_id): Path<Uuid>,
    Query(q): Query<DiffQuery>,
) -> ApiResult<Json<ConfigDiff>> {
    check_oid(&q.from)?;
    check_oid(&q.to)?;
    let ox = oxidized_of(&admin)?;
    let old = ox
        .client
        .version_text(node_id, &q.from)
        .await
        .map_err(|e| oxidized_failed(&e))?;
    let new = ox
        .client
        .version_text(node_id, &q.to)
        .await
        .map_err(|e| oxidized_failed(&e))?;
    Ok(Json(ConfigDiff {
        lines: diff_lines(&old, &new),
        from: q.from,
        to: q.to,
    }))
}

#[utoipa::path(
    post, path = "/api/v1/nodes/{node_id}/config-backup/fetch", tag = "nodes",
    params(("node_id" = Uuid, Path, description = "Node id")),
    responses(
        (status = 202, description = "Asked; the bundled oxidized fetches the node next"),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
        (status = 404, description = "No such node the caller can see", body = super::error::ErrorBody),
        (status = 502, description = "The bundled oxidized did not answer (`oxidized_unreachable`)", body = super::error::ErrorBody),
        (status = 503, description = "This deployment has no config backup (`config_backup_unavailable`)", body = super::error::ErrorBody),
    ),
)]
async fn fetch_now(
    _perm: RequireManageConfig,
    _visible: VisibleNode,
    admin: Admin,
    Path(node_id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    let ox = oxidized_of(&admin)?;
    // Reload first, so a node switched on a moment ago is in oxidized's list to be queued.
    ox.client.reload().await.map_err(|e| oxidized_failed(&e))?;
    ox.client
        .fetch_now(node_id)
        .await
        .map_err(|e| oxidized_failed(&e))?;
    Ok(StatusCode::ACCEPTED)
}

#[utoipa::path(
    get, path = "/internal/oxidized/nodes", tag = "nodes",
    responses(
        (status = 200, description = "The nodes the bundled oxidized fetches, with their logins in plaintext. For oxidized alone: it demands the source token in `x-yagra-oxidized-token`", body = Vec<oxidized::source::SourceNode>),
        (status = 401, description = "No token, or the wrong one (`oxidized_token`)", body = super::error::ErrorBody),
        (status = 503, description = "This deployment has no config backup (`config_backup_unavailable`)", body = super::error::ErrorBody),
    ),
)]
async fn oxidized_source(
    admin: Admin,
    headers: HeaderMap,
) -> ApiResult<Json<Vec<oxidized::source::SourceNode>>> {
    let ox = oxidized_of(&admin)?;
    let refused = || ApiError::unauthorized_with("oxidized_token", "no valid source token");
    // Read on every call rather than at start-up: the one-shot that writes it may run after core.
    let expected = oxidized::config::read_token(&ox.settings.home).map_err(|e| {
        tracing::warn!(error = %e, "config backup: the oxidized source token could not be read");
        refused()
    })?;
    let presented = headers
        .get(oxidized::config::TOKEN_HEADER)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(refused)?;
    if !crate::events::constant_time_eq(presented.as_bytes(), expected.as_bytes()) {
        metrics::counter!("yagra_oxidized_source_refusals_total").increment(1);
        return Err(refused());
    }
    let targets = admin.config_backup.targets().await.map_err(|e| {
        ApiError::from_internal(
            e.as_ref(),
            "config backup targets",
            "failed to read config backup",
        )
    })?;
    metrics::counter!("yagra_oxidized_source_reads_total").increment(1);
    let mut nodes = oxidized::source::source_nodes(targets, &admin.creds).await;
    if nodes.is_empty() {
        // oxidized will not start on an empty list (`source::placeholder`).
        nodes.push(oxidized::source::placeholder());
    }
    Ok(Json(nodes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_diff_marks_each_line_by_which_version_holds_it() {
        let lines = diff_lines("hostname a\nntp 1\n", "hostname a\nntp 2\n");
        let kinds: Vec<_> = lines.iter().map(|l| (l.kind, l.text.as_str())).collect();
        assert_eq!(
            kinds,
            [
                (DiffLineKind::Same, "hostname a"),
                (DiffLineKind::Removed, "ntp 1"),
                (DiffLineKind::Added, "ntp 2"),
            ]
        );
    }

    /// A folder-scoped caller may not hand the shared login to a node of theirs. Tested here rather
    /// than through the router: the route's `VisibleNode` reads the folder of a node from the alert
    /// engine's snapshot, which the live test state never loads.
    #[test]
    fn a_scoped_caller_cannot_choose_a_login() {
        use crate::api::scope::{NodeScope, ScopeSet};
        assert!(refuse_scoped_login(&NodeScope::All).is_ok());
        let scoped = NodeScope::Groups(Arc::new(ScopeSet {
            visible: vec![Uuid::from_u128(7)],
            breadcrumb: Vec::new(),
        }));
        let err = refuse_scoped_login(&scoped).expect_err("refused");
        assert_eq!(err.status(), StatusCode::FORBIDDEN);
        assert_eq!(err.code(), format!("{}_unsupported", "scope"));
    }

    #[test]
    fn only_a_lowercase_git_object_id_reaches_oxidized() {
        assert!(check_oid(&"a".repeat(40)).is_ok());
        assert!(check_oid(&"A".repeat(40)).is_err());
        assert!(check_oid("abc").is_err());
        assert!(check_oid(&format!("{}&group=x", "a".repeat(31))).is_err());
    }

    /// A configuration written by a person is accepted and read back; a Viewer cannot write one.
    #[sqlx::test(migrator = "yagra_base::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_node_is_switched_on_with_a_cli_login_and_a_viewer_cannot(pool: sqlx::PgPool) {
        use crate::api::tests_support::{live_state, send, token};
        let st = live_state(pool.clone()).await;
        let node = yagra_base::pgtest::node(&pool, "sw-01", 1, None).await;
        let login = yagra_base::pgtest::credential(&pool, "ops", "cli_login").await;
        let community = yagra_base::pgtest::credential(&pool, "lab", "snmp_v2c").await;
        let path = format!("/api/v1/nodes/{node}/config-backup");
        let operator = token(&st, yagra_common::Role::Operator);

        let (status, body) = send(&st, "GET", &path, &operator, None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["readiness"], "off");
        assert_eq!(body["available"], false);

        let on = serde_json::json!({
            "enabled": true, "credential_id": login, "input": "telnet", "model_override": "ios",
        });
        let (status, body) = send(&st, "PUT", &path, &operator, Some(on.clone())).await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
        let (_, body) = send(&st, "GET", &path, &operator, None).await;
        assert_eq!(body["readiness"], "ready", "{body}");
        assert_eq!(body["input"], "telnet");

        // A community is not a CLI login, and a model Yagra does not offer is refused.
        for bad in [
            serde_json::json!({"enabled": true, "credential_id": community, "input": "ssh"}),
            serde_json::json!({"enabled": true, "input": "ssh", "model_override": "ios; rm -rf"}),
        ] {
            let (status, body) = send(&st, "PUT", &path, &operator, Some(bad)).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
            assert_eq!(body["error"]["code"], "invalid_config_backup");
        }

        let viewer = token(&st, yagra_common::Role::Viewer);
        let (status, _) = send(&st, "PUT", &path, &viewer, Some(on)).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    /// A CLI login is never handed to an SNMP sweep as a community (ADR-210).
    #[sqlx::test(migrator = "yagra_base::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_discovery_sweep_refuses_a_cli_login(pool: sqlx::PgPool) {
        let st = crate::api::tests_support::live_state(pool.clone()).await;
        let admin = st.admin.clone().expect("live state");
        let login = yagra_base::pgtest::credential(&pool, "ops", "cli_login").await;
        let err =
            crate::api::discovery::resolve_scan_credentials(&admin.creds, &[login.to_string()])
                .await
                .expect_err("a CLI login is refused");
        assert_eq!(err.code(), "invalid_credential");
    }

    /// The source refuses a caller without the token, and with a deployment that has no oxidized
    /// it says so rather than serving logins.
    #[sqlx::test(migrator = "yagra_base::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn the_source_is_closed_without_oxidized(pool: sqlx::PgPool) {
        use crate::api::tests_support::{live_state, send};
        let st = live_state(pool.clone()).await;
        let (status, body) = send(&st, "GET", "/internal/oxidized/nodes", "nonsense", None).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    }
}
