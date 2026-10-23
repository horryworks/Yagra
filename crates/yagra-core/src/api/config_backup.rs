// SPDX-License-Identifier: AGPL-3.0-only
//! Config backup through the bundled oxidized (ADR-210 stages 2 and 3).
//!
//! Per node, addressed by path (`NodeScoped`):
//!
//! - `GET …/config-backup` — what is configured, whether the node is fetched and why not, which
//!   login is being tried or has worked, when the next one may be, and the latest attempts. `View`.
//!   Logins are named by id: a credential's name is `ManageCredentials` reading, so the WebUI
//!   resolves names only for a caller who may list them.
//! - `PUT …/config-backup` — switch it on or off, the node's own order of CLI logins (or the try
//!   order of Credentials & secrets), SSH or telnet, the model. `ManageConfig`, and switching it on
//!   also takes `RevealCredentials` and a fleet-wide caller, because it hands logins to the node's
//!   address. oxidized is asked to re-read its node list straight after.
//! - `GET …/config-backup/versions` — the stored versions. `View`.
//! - `GET …/config-backup/versions/{oid}` and `GET …/config-backup/diff` — the text, and two
//!   versions side by side. **`ManageConfig`** (decision 10): oxidized's `remove_secret` is model by
//!   model and can miss one, so a Viewer does not read device configuration. Neither has an MCP
//!   tool, for the same reason (decision 16).
//! - `POST …/config-backup/fetch` — fetch now, with the node's next login, at once: a person's
//!   press does not wait for the lockout limits (stage 3 decision 25, the user's). `ManageConfig`.
//!
//! Fleet-wide, `GET /api/v1/config-backups`: every node switched on that the caller can see, with
//! its rotation and its last attempt — the Nodes ▸ Config backups list and the tree's marks.
//!
//! And two routes for oxidized itself, outside `/api/v1`, both behind the source token the
//! one-shot `oxidized-config` wrote (compared in constant time). nginx forwards only `/api/` and
//! `/mcp`, so the TLS edge never serves them; core's own port is on the LAN, so the token is what
//! keeps a neighbour out.
//!
//! - `GET /internal/oxidized/nodes`, the HTTP source oxidized reads its node list from. **It returns
//!   device logins in plaintext** — the second reader of plaintext after
//!   `POST /credentials/{id}/reveal` (security.md). It is not audited per call (oxidized reads it
//!   every few minutes); `yagra_oxidized_source_reads_total` counts it instead.
//! - `POST /internal/oxidized/events`, where oxidized's hook reports how each fetch ended (stage 3
//!   decision 21). It takes the node, the event and oxidized's error type, never the message.

use std::sync::Arc;

use axum::{
    extract::{Path, Query},
    http::{HeaderMap, StatusCode},
    routing::{get, post},
    Form, Json, Router,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use yagra_base::config_backup::{Attempt, AttemptOutcome, BackupInput, NodeConfigBackup, ScanMode};

use super::error::{ApiError, ApiResult};
use super::extract::{
    Admin, RequireManageConfig, RequireRevealCredentials, RequireView, Scoped, VisibleNode,
};
use super::ApiState;
use crate::oxidized::{
    self,
    client::OxidizedClient,
    rotation::{self, Step},
    source::Readiness,
};

/// This domain's slice of the OpenAPI document (ADR-035), merged by [`super::openapi::document`].
#[derive(utoipa::OpenApi)]
#[openapi(paths(
    get_config_backup,
    put_config_backup,
    list_versions,
    get_version,
    get_diff,
    fetch_now,
    list_config_backups,
    oxidized_source,
    oxidized_event
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
        .route("/api/v1/config-backups", get(list_config_backups))
        .route("/internal/oxidized/nodes", get(oxidized_source))
        .route("/internal/oxidized/events", post(oxidized_event))
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

pub(super) fn oxidized_of(admin: &super::AdminState) -> Result<&OxidizedHandle, ApiError> {
    admin.oxidized.as_deref().ok_or_else(no_oxidized)
}

fn oxidized_failed(e: &anyhow::Error) -> ApiError {
    tracing::warn!(error = %e, "config backup: oxidized did not answer");
    ApiError::bad_gateway(
        "oxidized_unreachable",
        "the config backup service did not answer",
    )
}

fn read_failed(e: &anyhow::Error) -> ApiError {
    ApiError::from_internal(e.as_ref(), "config backup", "failed to read config backup")
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
    /// A login has fetched successfully and is the one used.
    login_confirmed: bool,
    /// How the newest login attempt ended; `null` ⇒ none yet.
    last_outcome: Option<AttemptOutcome>,
}

impl ConfigBackupSummary {
    /// A populated instance, for the MCP result canary.
    #[cfg(test)]
    pub(crate) fn sample() -> Self {
        Self {
            readiness: Readiness::Ready,
            last_status: Some("success".to_owned()),
            last_end: Some(DateTime::<Utc>::UNIX_EPOCH),
            login_confirmed: true,
            last_outcome: Some(AttemptOutcome::Success),
        }
    }
}

/// Whether a node is fetched, read from the same stores the source reads.
async fn readiness_of(
    admin: &super::AdminState,
    node: Uuid,
    cfg: Option<&NodeConfigBackup>,
) -> anyhow::Result<Readiness> {
    let profile = admin.config_backup.profile_name(node).await?;
    let moved = admin.config_backup.address_changed(node).await?;
    let logins = rotation::cli_logins(&admin.creds).await?;
    let has = !rotation::candidates(cfg.and_then(|c| c.login_order.as_deref()), &logins).is_empty();
    Ok(oxidized::source::readiness(
        cfg,
        profile.as_deref(),
        has,
        moved,
        &admin.config_relay.route(node),
    ))
}

/// The summary for one node, from the same reads the REST view makes. `None` when a read failed.
pub(crate) async fn summary_of(
    admin: &super::AdminState,
    node: Uuid,
) -> Option<ConfigBackupSummary> {
    let row = admin.config_backup.get_with_rotation(node).await.ok()?;
    let cfg = row.as_ref().map(|(c, _)| c.clone());
    let readiness = readiness_of(admin, node, cfg.as_ref()).await.ok()?;
    let status = admin.config_backup.status(node).await.ok()?;
    let last = admin.config_backup.attempts_of(node, 1).await.ok()?;
    Some(ConfigBackupSummary {
        readiness,
        last_status: status.as_ref().map(|s| s.status.clone()),
        last_end: status.and_then(|s| s.last_end),
        login_confirmed: row.is_some_and(|(_, r)| r.login_confirmed),
        last_outcome: last.first().map(|a| a.outcome),
    })
}

/// One login attempt.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct AttemptView {
    at: DateTime<Utc>,
    /// The login tried; `null` when it has since been deleted.
    credential_id: Option<Uuid>,
    outcome: AttemptOutcome,
    /// For a success: whether a new version was stored. `null` otherwise.
    new_version: Option<bool>,
}

impl From<Attempt> for AttemptView {
    fn from(a: Attempt) -> Self {
        Self {
            at: a.at,
            credential_id: a.credential_id,
            outcome: a.outcome,
            new_version: a.new_version,
        }
    }
}

/// How many attempts the node view carries.
const VIEW_ATTEMPTS: i64 = 10;

/// One node's config backup.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct ConfigBackupView {
    enabled: bool,
    /// The node's own order of CLI logins; `null` means every CLI login in the try order.
    login_order: Option<Vec<Uuid>>,
    /// The logins this node tries, in order (`login_order` with deleted ones dropped, or every CLI
    /// login).
    candidates: Vec<Uuid>,
    /// The login handed to oxidized: the one being tried, or the one that works.
    credential_id: Option<Uuid>,
    /// `credential_id` has fetched successfully.
    login_confirmed: bool,
    /// Yagra asked oxidized for a fetch and has not heard how it ended.
    pending: bool,
    /// When the lockout limits allow the next try; `null` when one may be tried now, or none is due.
    try_after: Option<DateTime<Utc>>,
    /// Every candidate failed in this pass; nothing is tried until a fetch is asked for, the order
    /// changes, or a CLI login is added or changed.
    exhausted: bool,
    /// The newest attempts, newest first.
    attempts: Vec<AttemptView>,
    input: BackupInput,
    /// How fast it tries its logins: one set of lockout limits each (stage 3 decision 27).
    scan_mode: ScanMode,
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

#[utoipa::path(
    get, path = "/api/v1/nodes/{node_id}/config-backup", tag = "nodes",
    params(("node_id" = Uuid, Path, description = "Node id")),
    responses(
        (status = 200, description = "The node's config backup: configuration, whether it is fetched, the login being tried or that works, the latest attempts", body = ConfigBackupView),
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
    let repo = &admin.config_backup;
    let row = repo
        .get_with_rotation(node_id)
        .await
        .map_err(|e| read_failed(&e))?;
    let profile = repo
        .profile_name(node_id)
        .await
        .map_err(|e| read_failed(&e))?;
    let status = repo.status(node_id).await.map_err(|e| read_failed(&e))?;
    let attempts = repo
        .attempts_of(node_id, VIEW_ATTEMPTS)
        .await
        .map_err(|e| read_failed(&e))?;
    let logins = rotation::cli_logins(&admin.creds)
        .await
        .map_err(|e| read_failed(&e))?;
    let (cfg, rot) = row.unwrap_or((
        NodeConfigBackup {
            enabled: false,
            login_order: None,
            input: BackupInput::Ssh,
            model_override: None,
            scan_mode: ScanMode::Normal,
        },
        yagra_base::config_backup::Rotation::default(),
    ));
    let candidates = rotation::candidates(cfg.login_order.as_deref(), &logins);
    let readiness = readiness_of(&admin, node_id, Some(&cfg))
        .await
        .map_err(|e| read_failed(&e))?;
    let now = Utc::now();
    let pending = rotation::in_flight(rot.requested_at, now);
    let step = if readiness == Readiness::Ready && !pending {
        rotation::step_of(repo, &admin.creds, &admin.config_relay, node_id, now)
            .await
            .map_err(|e| read_failed(&e))?
    } else {
        None
    };
    Ok(Json(ConfigBackupView {
        enabled: cfg.enabled,
        login_order: cfg.login_order,
        candidates,
        credential_id: rot.credential_id,
        login_confirmed: rot.login_confirmed,
        pending,
        try_after: match step {
            Some(Step::Wait { until, .. }) => Some(until),
            _ => None,
        },
        exhausted: step == Some(Step::Exhausted),
        attempts: attempts.into_iter().map(AttemptView::from).collect(),
        input: cfg.input,
        scan_mode: cfg.scan_mode,
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
    /// The node's own order of CLI logins. Omitted or `null`: every CLI login, in the try order of
    /// Credentials & secrets.
    #[serde(default)]
    login_order: Option<Vec<Uuid>>,
    /// Deprecated: one login, read as `login_order: [credential_id]`. Refused together with
    /// `login_order`.
    #[serde(default)]
    credential_id: Option<Uuid>,
    input: BackupInput,
    #[serde(default)]
    model_override: Option<String>,
    /// How fast it tries its logins. Omitted: `normal`.
    #[serde(default)]
    scan_mode: ScanMode,
}

/// The most logins one node's order may name.
const MAX_ORDER: usize = 200;

#[utoipa::path(
    put, path = "/api/v1/nodes/{node_id}/config-backup", tag = "nodes",
    params(("node_id" = Uuid, Path, description = "Node id")),
    request_body = ConfigBackupBody,
    responses(
        (status = 204, description = "Saved; the bundled oxidized is asked to re-read its node list. Switching it on, or changing the order, starts the logins afresh"),
        (status = 400, description = "An unknown input or model, a login that is not a CLI login, a repeated one, or both `login_order` and `credential_id` (`invalid_config_backup`)", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig; or the backup is switched on by an account restricted to folders (`scope_unsupported`) or by a caller without RevealCredentials, such as an API token (`login_not_permitted`)", body = super::error::ErrorBody),
        (status = 404, description = "No such node the caller can see", body = super::error::ErrorBody),
        (status = 503, description = "This deployment has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn put_config_backup(
    _perm: RequireManageConfig,
    // Optional: needed only to switch the backup on (below). An API token never holds it.
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
    if body.scan_mode == ScanMode::Unknown {
        return Err(bad("scan_mode must be rapid, normal or slow"));
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
    let login_order = match (body.login_order, body.credential_id) {
        (Some(_), Some(_)) => return Err(bad("send login_order or credential_id, not both")),
        (Some(order), None) => Some(order),
        (None, Some(one)) => Some(vec![one]),
        (None, None) => None,
    };
    if let Some(order) = login_order.as_deref() {
        if order.len() > MAX_ORDER {
            return Err(bad("too many logins in the order"));
        }
        let mut seen = std::collections::HashSet::new();
        if !order.iter().all(|id| seen.insert(*id)) {
            return Err(bad("a login appears twice in the order"));
        }
        let logins = rotation::cli_logins(&admin.creds)
            .await
            .map_err(|e| read_failed(&e))?;
        if !order.iter().all(|id| logins.contains(id)) {
            return Err(bad("every login in the order must be a CLI login"));
        }
    }
    if body.enabled {
        refuse_scoped_login(&scope)?;
        // Switching it on hands every login in the order to the node's address, so it takes what
        // reading one back takes: a caller that may not see a password must not be able to send
        // it somewhere it chose.
        if reveal.is_none() {
            return Err(ApiError::forbidden_code(
                "login_not_permitted",
                "switching config backup on hands CLI logins to the device and needs the Reveal \
                 credentials permission, which an API token never holds",
            ));
        }
    }
    let before = admin
        .config_backup
        .get(node_id)
        .await
        .map_err(|e| read_failed(&e))?;
    let cfg = NodeConfigBackup {
        enabled: body.enabled,
        login_order,
        input: body.input,
        model_override,
        scan_mode: body.scan_mode,
    };
    // A node switched on, or one whose candidates changed, starts its logins afresh.
    let restart = before
        .as_ref()
        .is_none_or(|b| (!b.enabled && cfg.enabled) || b.login_order != cfg.login_order);
    let saved = admin
        .config_backup
        .put(node_id, &cfg, restart)
        .await
        .map_err(|e| {
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
    // Best effort, off the request: the status loop reloads within five minutes anyway. Never
    // while a fetch is out, which a reload would start a second time (`rotation::may_reload`).
    if let Some(ox) = admin.oxidized.clone() {
        if rotation::may_reload(&admin.config_backup, Utc::now())
            .await
            .unwrap_or(false)
        {
            tokio::spawn(async move {
                if let Err(e) = ox.client.reload().await {
                    tracing::warn!(error = %e, "config backup: oxidized did not reload after a change");
                }
            });
        }
    }
    Ok(StatusCode::NO_CONTENT)
}

/// A CLI login is shared by every folder, like every credential (`credentials_are_deployment_wide`).
/// A folder-scoped caller could otherwise switch the backup on for a node of theirs whose address
/// they control, and oxidized would hand that address the passwords. Switching off stays theirs.
fn refuse_scoped_login(scope: &super::scope::NodeScope) -> Result<(), ApiError> {
    super::scope::require_fleet_wide(
        scope,
        "a CLI login is shared by every folder, so an account restricted to folders cannot \
         switch config backup on",
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
    // 🚨 A node oxidized does not hold answers its history with a 500 (`NodeNotFound`), which read
    // as "the config backup service did not answer" under the table of every node not switched on
    // yet — measured on a lab deployment. So the answer is none, without asking, for a node not in
    // the list: one switched off, or one with no model or connection. A node still looking for a
    // login is in it, parked (`source::source_nodes`, stage 3 decision 26), so its versions show.
    let listed = admin
        .config_backup
        .get(node_id)
        .await
        .ok()
        .flatten()
        .filter(|c| c.enabled && c.input.oxidized().is_some());
    let Some(cfg) = listed else {
        return Ok(Json(Vec::new()));
    };
    let profile = admin
        .config_backup
        .profile_name(node_id)
        .await
        .map_err(|e| read_failed(&e))?;
    if oxidized::source::model_of(cfg.model_override.as_deref(), profile.as_deref()).is_none() {
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
pub(super) fn check_oid(oid: &str) -> Result<(), ApiError> {
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
        (status = 202, description = "Asked; the bundled oxidized fetches the node now with its next login, without waiting for the lockout limits — they hold back only the automatic rotation. A pass in which every login failed starts again"),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
        (status = 404, description = "No such node the caller can see", body = super::error::ErrorBody),
        (status = 409, description = "The node is not switched on or not ready, a fetch is already out for it, or the login it needs cannot be handed to oxidized while another node's fetch is out (`config_backup_not_ready`, `config_backup_pending`, `config_backup_busy`)", body = super::error::ErrorBody),
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
    let repo = &admin.config_backup;
    let now = Utc::now();
    let rot = repo
        .get_with_rotation(node_id)
        .await
        .map_err(|e| read_failed(&e))?
        .map(|(_, r)| r);
    if rot
        .as_ref()
        .is_some_and(|r| rotation::in_flight(r.requested_at, now))
    {
        return Err(ApiError::conflict(
            "config_backup_pending",
            "a fetch is already out for this node",
        ));
    }
    let not_ready = || {
        ApiError::conflict(
            "config_backup_not_ready",
            "this node is not switched on for config backup, or has nothing to fetch with",
        )
    };
    let mut step = rotation::step_of(repo, &admin.creds, &admin.config_relay, node_id, now)
        .await
        .map_err(|e| read_failed(&e))?
        .ok_or_else(not_ready)?;
    if step == Step::Exhausted {
        // Asked for by a person: every login gets one more pass.
        repo.restart_round(node_id)
            .await
            .map_err(|e| read_failed(&e))?;
        step = rotation::step_of(repo, &admin.creds, &admin.config_relay, node_id, now)
            .await
            .map_err(|e| read_failed(&e))?
            .ok_or_else(not_ready)?;
    }
    match rotation::login_to_force(step) {
        Some(credential) => {
            // A node fetched again with the login that already works is in oxidized's list as it
            // is, so it needs no reload. Any other login has to reach the list first, and the list
            // may not be read again while another fetch is out (`rotation::may_reload`).
            let reload = !rot
                .as_ref()
                .is_some_and(|r| r.login_confirmed && r.credential_id == Some(credential));
            if reload
                && !rotation::may_reload(repo, now)
                    .await
                    .map_err(|e| read_failed(&e))?
            {
                return Err(ApiError::conflict(
                    "config_backup_busy",
                    "another node's fetch is out; this one can start when it has answered",
                ));
            }
            let started = repo
                .dispatch(node_id, credential, now, now - rotation::REQUEST_TIMEOUT)
                .await
                .map_err(|e| read_failed(&e))?;
            if !started {
                return Err(ApiError::conflict(
                    "config_backup_pending",
                    "a fetch is already out for this node",
                ));
            }
            let asked = async {
                if reload {
                    ox.client.reload().await?;
                }
                ox.client.fetch_now(node_id).await
            }
            .await;
            if let Err(e) = asked {
                // oxidized never got the request, so nothing may later count it as a login.
                repo.clear_request(node_id).await.ok();
                return Err(oxidized_failed(&e));
            }
            Ok(StatusCode::ACCEPTED)
        }
        None => Err(not_ready()),
    }
}

/// One node in the fleet-wide list.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct ConfigBackupRow {
    node_id: Uuid,
    /// The login being tried or that works.
    credential_id: Option<Uuid>,
    login_confirmed: bool,
    /// A fetch is out and not yet answered.
    pending: bool,
    /// When the lockout limits next allow a try, as the leader last worked it out.
    try_after: Option<DateTime<Utc>>,
    /// The newest attempt; `null` when none has been made.
    last: Option<AttemptView>,
    /// When a fetch last succeeded.
    last_success_at: Option<DateTime<Utc>>,
    /// Whether a poller can carry the node's login session (ADR-210 stage 5 decision 48).
    poller: PollerReach,
}

/// Whether a node's config-backup login has a poller to go through (ADR-210 stage 5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PollerReach {
    /// A live poller polls it and carries the relay.
    Ready,
    /// No live poller polls it.
    NoPoller,
    /// The poller that polls it predates the relay.
    PollerTooOld,
}

impl From<&oxidized::relay::Route> for PollerReach {
    fn from(r: &oxidized::relay::Route) -> Self {
        match r {
            oxidized::relay::Route::Via(_) => Self::Ready,
            oxidized::relay::Route::NoPoller => Self::NoPoller,
            oxidized::relay::Route::PollerTooOld => Self::PollerTooOld,
        }
    }
}

/// Every node switched on that the caller can see. Shared by REST and MCP.
pub(crate) async fn config_backup_rows(
    admin: &super::AdminState,
    scope: &super::scope::NodeScope,
) -> Result<Vec<ConfigBackupRow>, ApiError> {
    let now = Utc::now();
    let rows = admin
        .config_backup
        .overview(scope.group_filter())
        .await
        .map_err(|e| read_failed(&e))?;
    Ok(rows
        .into_iter()
        .map(|r| ConfigBackupRow {
            node_id: r.node_id,
            credential_id: r.rotation.credential_id,
            login_confirmed: r.rotation.login_confirmed,
            pending: rotation::in_flight(r.rotation.requested_at, now),
            try_after: r.rotation.next_try_at.filter(|t| *t > now),
            last: r.last.map(AttemptView::from),
            last_success_at: r.last_success_at,
            poller: PollerReach::from(&admin.config_relay.route(r.node_id)),
        })
        .collect())
}

#[utoipa::path(
    get, path = "/api/v1/config-backups", tag = "nodes",
    responses(
        (status = 200, description = "Every node switched on for config backup that the caller can see, with its login and its last attempt", body = Vec<ConfigBackupRow>),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 503, description = "This deployment has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn list_config_backups(
    _perm: RequireView,
    Scoped(scope): Scoped,
    admin: Admin,
) -> ApiResult<Json<Vec<ConfigBackupRow>>> {
    Ok(Json(config_backup_rows(&admin, &scope).await?))
}

/// The source token, checked in constant time. Read on every call rather than at start-up: the
/// one-shot that writes it may run after core.
pub(super) fn check_source_token(ox: &OxidizedHandle, headers: &HeaderMap) -> Result<(), ApiError> {
    let refused = || ApiError::unauthorized_with("oxidized_token", "no valid source token");
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
    Ok(())
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
    check_source_token(ox, &headers)?;
    let targets = admin.config_backup.targets().await.map_err(|e| {
        ApiError::from_internal(
            e.as_ref(),
            "config backup targets",
            "failed to read config backup",
        )
    })?;
    metrics::counter!("yagra_oxidized_source_reads_total").increment(1);
    // The names configurations are pushed under outlive their nodes (ADR-210 stage 4 decision 39).
    // Best effort: a failure here must not stop oxidized from reading its list.
    if let Err(e) = admin.config_backup.remember_names().await {
        tracing::warn!(error = %e, "config backup: node names could not be remembered");
    }
    // Every node with a configuration gets its relay port here, so the ports oxidized is handed
    // and the ones core listens on come from one table (stage 5 decision 44).
    let ids: Vec<Uuid> = targets.iter().map(|t| t.node_id).collect();
    let ports = admin.config_backup.relay_ports(&ids).await.map_err(|e| {
        ApiError::from_internal(
            e.as_ref(),
            "config backup relay ports",
            "failed to read config backup",
        )
    })?;
    let relay = admin.config_relay.clone();
    let mut nodes =
        oxidized::source::source_nodes(targets, &admin.creds, Utc::now(), &ports, &|node| {
            relay.route(node)
        })
        .await;
    if nodes.is_empty() {
        // oxidized will not start on an empty list (`source::placeholder`).
        nodes.push(oxidized::source::placeholder());
    }
    Ok(Json(nodes))
}

/// How one fetch ended, as oxidized's hook posts it (`config::render`).
#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub(super) struct OxidizedEvent {
    /// `node_success` or `node_fail`.
    event: String,
    /// oxidized's name for the node: its id.
    node: String,
    #[serde(default)]
    group: Option<String>,
    /// The address oxidized dialled. A parked node's (`source::PARKED_IP`) result is not a login
    /// attempt and is ignored.
    #[serde(default)]
    ip: Option<String>,
    /// oxidized's error class, such as `Net::SSH::AuthenticationFailed`; empty on success.
    #[serde(default)]
    err_type: Option<String>,
}

#[utoipa::path(
    post, path = "/internal/oxidized/events", tag = "nodes",
    request_body(content = OxidizedEvent, content_type = "application/x-www-form-urlencoded"),
    responses(
        (status = 204, description = "Recorded, or ignored when it is not about one of Yagra's nodes. For oxidized alone: it demands the source token in `x-yagra-oxidized-token`"),
        (status = 401, description = "No token, or the wrong one (`oxidized_token`)", body = super::error::ErrorBody),
        (status = 503, description = "This deployment has no config backup (`config_backup_unavailable`)", body = super::error::ErrorBody),
    ),
)]
async fn oxidized_event(
    admin: Admin,
    headers: HeaderMap,
    Form(ev): Form<OxidizedEvent>,
) -> ApiResult<StatusCode> {
    let ox = oxidized_of(&admin)?;
    check_source_token(ox, &headers)?;
    let Some(node) = oxidized::source::node_of(&ev.node, ev.group.as_deref()) else {
        return Ok(StatusCode::NO_CONTENT);
    };
    // A parked node's fetch reached no device and sent no login (stage 3 decision 26).
    if ev.ip.as_deref() == Some(oxidized::source::PARKED_IP) {
        return Ok(StatusCode::NO_CONTENT);
    }
    let Some(mut outcome) = rotation::classify(&ev.event, ev.err_type.as_deref().unwrap_or(""))
    else {
        return Ok(StatusCode::NO_CONTENT);
    };
    // A fetch the relay could not carry reached no device, whatever oxidized made of the closed
    // connection: counting it as a refused login would move on to the next one (stage 5 decision
    // 47).
    if outcome != AttemptOutcome::Success && admin.config_relay.take_recent_failure(node) {
        outcome = AttemptOutcome::Unreachable;
    }
    // The newest version tells a new one from an unchanged one. Best effort: without it the success
    // is recorded and the screen says only that it succeeded.
    let oid = if outcome == AttemptOutcome::Success {
        ox.client
            .versions(node)
            .await
            .ok()
            .and_then(|v| v.into_iter().next())
            .map(|v| v.oid)
    } else {
        None
    };
    admin
        .config_backup
        .record_result(node, outcome, oid.as_deref())
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "record config backup attempt",
                "failed to record the attempt",
            )
        })?;
    metrics::counter!("yagra_config_backup_attempts_total", "outcome" => outcome.as_str())
        .increment(1);
    Ok(StatusCode::NO_CONTENT)
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

    /// A folder-scoped caller may not hand the shared logins to a node of theirs. Tested here
    /// rather than through the router: the route's `VisibleNode` reads the folder of a node from
    /// the alert engine's snapshot, which the live test state never loads.
    #[test]
    fn a_scoped_caller_cannot_switch_the_backup_on() {
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
    async fn a_node_is_switched_on_with_its_logins_and_a_viewer_cannot(pool: sqlx::PgPool) {
        use crate::api::tests_support::{live_state, send, token};
        let st = live_state(pool.clone()).await;
        let node = yagra_base::pgtest::node(&pool, "sw-01", 1, None).await;
        let first = yagra_base::pgtest::credential(&pool, "ops-a", "cli_login").await;
        let second = yagra_base::pgtest::credential(&pool, "ops-b", "cli_login").await;
        let community = yagra_base::pgtest::credential(&pool, "lab", "snmp_v2c").await;
        let path = format!("/api/v1/nodes/{node}/config-backup");
        let operator = token(&st, yagra_common::Role::Operator);

        let (status, body) = send(&st, "GET", &path, &operator, None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["readiness"], "off");
        assert_eq!(body["available"], false);
        // With no order of its own, a node tries every CLI login in the try order.
        assert_eq!(
            body["candidates"],
            serde_json::json!([first.to_string(), second.to_string()])
        );

        let on = serde_json::json!({
            "enabled": true, "login_order": [second, first], "input": "telnet",
            "model_override": "ios", "scan_mode": "slow",
        });
        let (status, body) = send(&st, "PUT", &path, &operator, Some(on.clone())).await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
        let (_, body) = send(&st, "GET", &path, &operator, None).await;
        assert_eq!(body["readiness"], "ready", "{body}");
        assert_eq!(body["input"], "telnet");
        assert_eq!(body["scan_mode"], "slow");
        assert_eq!(
            body["candidates"],
            serde_json::json!([second.to_string(), first.to_string()])
        );
        assert_eq!(body["login_confirmed"], false);
        assert_eq!(body["exhausted"], false);

        // The stage-2 body still works: one login becomes the whole order.
        let one = serde_json::json!({"enabled": true, "credential_id": first, "input": "ssh"});
        let (status, body) = send(&st, "PUT", &path, &operator, Some(one)).await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
        let (_, body) = send(&st, "GET", &path, &operator, None).await;
        assert_eq!(body["login_order"], serde_json::json!([first.to_string()]));

        // A community is not a CLI login; a login twice, both spellings and an unknown model are
        // refused.
        for bad in [
            serde_json::json!({"enabled": true, "login_order": [community], "input": "ssh"}),
            serde_json::json!({"enabled": true, "login_order": [first, first], "input": "ssh"}),
            serde_json::json!({"enabled": true, "login_order": [first], "credential_id": first,
                               "input": "ssh"}),
            serde_json::json!({"enabled": true, "input": "ssh", "model_override": "ios; rm -rf"}),
            serde_json::json!({"enabled": true, "input": "ssh", "scan_mode": "unknown"}),
        ] {
            let (status, body) = send(&st, "PUT", &path, &operator, Some(bad)).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
            assert_eq!(body["error"]["code"], "invalid_config_backup");
        }

        let viewer = token(&st, yagra_common::Role::Viewer);
        let (status, _) = send(&st, "PUT", &path, &viewer, Some(on)).await;
        assert_eq!(status, StatusCode::FORBIDDEN);

        // The fleet list carries the node for a Viewer too.
        let (status, body) = send(&st, "GET", "/api/v1/config-backups", &viewer, None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body.as_array().map(Vec::len), Some(1), "{body}");
        assert_eq!(body[0]["node_id"], node.to_string());
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

    /// The two oxidized routes refuse a caller without the token, and with a deployment that has no
    /// oxidized they say so rather than serving logins or recording anything.
    #[sqlx::test(migrator = "yagra_base::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn the_oxidized_routes_are_closed_without_oxidized(pool: sqlx::PgPool) {
        use crate::api::tests_support::{live_state, send};
        let st = live_state(pool.clone()).await;
        let (status, body) = send(&st, "GET", "/internal/oxidized/nodes", "nonsense", None).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
        let (status, body) = send(&st, "POST", "/internal/oxidized/events", "nonsense", None).await;
        assert_ne!(status, StatusCode::NO_CONTENT, "{body}");
        assert_eq!(
            yagra_base::pgtest::rows(&pool, "config_backup_attempts").await,
            0
        );
    }

    /// With the token, a reported failure is recorded against the login the node was handed, and
    /// a wrong token records nothing.
    #[sqlx::test(migrator = "yagra_base::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_reported_fetch_is_recorded_only_with_the_token(pool: sqlx::PgPool) {
        use axum::body::Body;
        use tower::ServiceExt;
        let home = std::env::temp_dir().join(format!("yagra-ox-ev-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&home).expect("dir");
        let token = oxidized::config::ensure_token(&home).expect("token");
        // A closed port: a failure is recorded without asking oxidized for anything.
        let st = crate::api::tests_support::live_state_with_oxidized(
            pool.clone(),
            Arc::new(OxidizedHandle {
                settings: oxidized::Settings {
                    url: "http://127.0.0.1:9".to_owned(),
                    home: home.clone(),
                },
                client: Arc::new(OxidizedClient::new("http://127.0.0.1:9")),
            }),
        )
        .await;
        let node = yagra_base::pgtest::node(&pool, "sw-09", 9, None).await;
        let login = yagra_base::pgtest::credential(&pool, "ops", "cli_login").await;
        let repo = yagra_base::config_backup::ConfigBackupRepo::new(pool.clone());
        repo.put(
            node,
            &NodeConfigBackup {
                enabled: true,
                login_order: None,
                input: BackupInput::Ssh,
                model_override: Some("ios".to_owned()),
                scan_mode: ScanMode::Normal,
            },
            true,
        )
        .await
        .expect("put");
        repo.dispatch(
            node,
            login,
            Utc::now(),
            Utc::now() - rotation::REQUEST_TIMEOUT,
        )
        .await
        .expect("dispatch");

        let post_from = |presented: &str, ip: &str| {
            axum::http::Request::builder()
                .method("POST")
                .uri("/internal/oxidized/events")
                .header("content-type", "application/x-www-form-urlencoded")
                .header(oxidized::config::TOKEN_HEADER, presented)
                .body(Body::from(format!(
                    "event=node_fail&node={node}&group=yagra&ip={ip}&err_type=Net%3A%3ASSH%3A%3AAuthenticationFailed"
                )))
                .expect("request")
        };
        let post = |presented: &str| post_from(presented, "192.0.2.9");
        let router = crate::api::router(st.clone());
        let res = router.clone().oneshot(post("wrong")).await.expect("send");
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            yagra_base::pgtest::rows(&pool, "config_backup_attempts").await,
            0
        );
        // A parked node's fetch reached no device: nothing is recorded (stage 3 decision 26).
        let res = router
            .clone()
            .oneshot(post_from(&token, oxidized::source::PARKED_IP))
            .await
            .expect("send");
        assert_eq!(res.status(), StatusCode::NO_CONTENT);
        assert_eq!(
            yagra_base::pgtest::rows(&pool, "config_backup_attempts").await,
            0
        );
        let res = router.oneshot(post(&token)).await.expect("send");
        assert_eq!(res.status(), StatusCode::NO_CONTENT);
        let attempts = repo.attempts_of(node, 5).await.expect("attempts");
        assert_eq!(attempts.len(), 1);
        assert_eq!(attempts[0].outcome, AttemptOutcome::AuthFailed);
        assert_eq!(attempts[0].credential_id, Some(login));
        std::fs::remove_dir_all(&home).ok();
    }

    /// A node handed a login is handed core's relay instead of its own address, and a fetch the
    /// relay could not carry is recorded as unreachable rather than as a refused login (ADR-210
    /// stage 5 decisions 43 and 47).
    #[sqlx::test(migrator = "yagra_base::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_handed_node_goes_through_the_relay_and_its_failure_is_not_a_refused_login(
        pool: sqlx::PgPool,
    ) {
        use axum::body::Body;
        use tower::ServiceExt;
        let home = std::env::temp_dir().join(format!("yagra-ox-relay-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&home).expect("dir");
        let token = oxidized::config::ensure_token(&home).expect("token");
        let st = crate::api::tests_support::live_state_with_oxidized(
            pool.clone(),
            Arc::new(OxidizedHandle {
                settings: oxidized::Settings {
                    url: "http://127.0.0.1:9".to_owned(),
                    home: home.clone(),
                },
                client: Arc::new(OxidizedClient::new("http://127.0.0.1:9")),
            }),
        )
        .await;
        let admin = st.admin.clone().expect("live state");
        let node = yagra_base::pgtest::node(&pool, "sw-10", 10, None).await;
        let login = admin
            .creds
            .create("ops", "cli_login", br#"{"username":"ops","password":"pw"}"#)
            .await
            .expect("login");
        let repo = yagra_base::config_backup::ConfigBackupRepo::new(pool.clone());
        repo.put(
            node,
            &NodeConfigBackup {
                enabled: true,
                login_order: None,
                input: BackupInput::SshThenTelnet,
                model_override: Some("ios".to_owned()),
                scan_mode: ScanMode::Normal,
            },
            true,
        )
        .await
        .expect("put");
        repo.dispatch(
            node,
            login,
            Utc::now(),
            Utc::now() - rotation::REQUEST_TIMEOUT,
        )
        .await
        .expect("dispatch");

        let router = crate::api::router(st.clone());
        let res = router
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .uri("/internal/oxidized/nodes")
                    .header(oxidized::config::TOKEN_HEADER, &token)
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("send");
        assert_eq!(res.status(), StatusCode::OK);
        let body = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .expect("body");
        let list: serde_json::Value = serde_json::from_slice(&body).expect("json");
        let entry = list
            .as_array()
            .expect("array")
            .iter()
            .find(|n| n["name"] == node.to_string())
            .expect("the node is listed");
        assert_eq!(entry["ip"], oxidized::relay::RELAY_HOST);
        assert_eq!(entry["username"], "ops");
        let ssh = entry["ssh_port"].as_u64().expect("an SSH relay port");
        assert_eq!(entry["telnet_port"].as_u64(), Some(ssh + 1));
        assert_eq!(ssh % 2, 0);
        // oxidized never learns the device's own address (the fixture's `10.0.0.10`).
        assert!(!body.windows(b"10.0.0.10".len()).any(|w| w == b"10.0.0.10"));

        admin.config_relay.record_failure(node);
        let res = router
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/internal/oxidized/events")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .header(oxidized::config::TOKEN_HEADER, &token)
                    .body(Body::from(format!(
                        "event=node_fail&node={node}&group=yagra&ip=core&err_type=Net%3A%3ASSH%3A%3ADisconnect"
                    )))
                    .expect("request"),
            )
            .await
            .expect("send");
        assert_eq!(res.status(), StatusCode::NO_CONTENT);
        let attempts = repo.attempts_of(node, 5).await.expect("attempts");
        assert_eq!(attempts.len(), 1);
        assert_eq!(attempts[0].outcome, AttemptOutcome::Unreachable);
        std::fs::remove_dir_all(&home).ok();
    }
}
