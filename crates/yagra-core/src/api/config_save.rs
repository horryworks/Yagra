// SPDX-License-Identifier: AGPL-3.0-only
//! Saving a device's running configuration from Yagra (ADR-222).
//!
//! - `POST /api/v1/nodes/{id}/config-backup/save` — the button. `SaveDeviceConfig` (Admin only,
//!   never an API token) and a fleet-wide caller, because it hands the node's login to the device
//!   as the backup does. It records a pending save and asks for a fetch; the save itself runs after
//!   that fetch succeeds, so what is saved is already backed up (decision 7). Audited by
//!   `audit_mw` like every write.
//! - `GET …/config-backup/saves/{save_id}/startup` — the startup configuration a save replaced.
//!   `ManageConfig`, and off MCP: ADR-210 stage 10 opened a version's text to `get_node_config`
//!   and left this one off (decision 82).
//! - `GET …/config-backup/unsaved` — what the device printed the last time the save script found
//!   running and startup different (Inc.9). `ManageConfig`; on MCP through `get_node_config`
//!   (`view=unsaved`) since ADR-210 stage 10, behind its switch, blanking and audit.
//! - `POST /api/v1/config-backups/auto-save` — automatic save on or off for many nodes at once
//!   (Inc.11 decision 14): a node switched on is saved right after a fetch finds it unsaved
//!   (decision 13). `SaveDeviceConfig` and a fleet-wide caller, as the button: it lets Yagra write
//!   to those devices on its own. Only a node backed up now, whose model has a save command, is
//!   switched on. Read back per node in the config backup list and the node's view.
//! - `PUT /api/v1/config-mcp-settings` - whether `/mcp`'s `get_node_config` may hand a
//!   configuration's text to an AI client (ADR-210 stage 10 decision 85). `ManageSystem`, a
//!   signed-in person (`Caller`, never an API token: a token is what would read the text) and a
//!   fleet-wide caller. Read back in `GET /api/v1/config-backup-schedule`.
//!
//! And two routes for the save script inside the bundled oxidized, outside `/api/v1`, behind the
//! source token, on the internal listener only (ADR-217):
//!
//! - `GET /internal/oxidized/save?node=` — what to do after a fetch: nothing (204), a check, or a
//!   save, with the node's login and its model's commands from `config_save::SAVE_COMMANDS`. **It
//!   returns a device login in plaintext**, like `GET /internal/oxidized/nodes`, for the same reader.
//! - `POST /internal/oxidized/save` — how it went: a failure kind, whether running and startup
//!   differed, the startup configuration kept before a save, and the comparison's output. Never
//!   the device's output beyond those two.

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post},
    Form, Json, Router,
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use yagra_base::config_save::{McpConfigText, SaveFailure, SaveOrigin};
use yagra_common::config_save::{commands_for, SaveCommands};

use super::config_backup::{check_source_token, oxidized_of, start_fetch};
use super::error::{ApiError, ApiResult};
use super::extract::{
    Actor, Admin, Caller, RequireManageConfig, RequireManageSystem, RequireSaveDeviceConfig,
    Scoped, VisibleNode,
};
use super::ApiState;
use crate::oxidized::{self, save::Action};

/// This domain's slice of the OpenAPI document (ADR-035), merged by [`super::openapi::document`].
#[derive(utoipa::OpenApi)]
#[openapi(paths(
    save_now,
    get_startup,
    get_unsaved,
    switch_auto_save,
    put_mcp_config_text,
    save_job,
    save_report
))]
pub(super) struct Doc;

/// The save routes, merged by [`super::router`].
pub(super) fn routes() -> Router<ApiState> {
    Router::new()
        .route("/api/v1/nodes/:node_id/config-backup/save", post(save_now))
        .route(
            "/api/v1/nodes/:node_id/config-backup/saves/:save_id/startup",
            get(get_startup),
        )
        .route(
            "/api/v1/nodes/:node_id/config-backup/unsaved",
            get(get_unsaved),
        )
        .route("/api/v1/config-backups/auto-save", post(switch_auto_save))
        .route(
            "/api/v1/config-mcp-settings",
            axum::routing::put(put_mcp_config_text),
        )
        .route("/internal/oxidized/save", get(save_job).post(save_report))
}

fn failed(e: &anyhow::Error, what: &'static str) -> ApiError {
    ApiError::from_internal(e.as_ref(), what, "failed to read config saves")
}

/// The save the button asked for.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct SaveAccepted {
    /// The save's id; it appears in the node's config backup with how it ended.
    id: Uuid,
}

#[utoipa::path(
    post, path = "/api/v1/nodes/{node_id}/config-backup/save", tag = "nodes",
    params(("node_id" = Uuid, Path, description = "Node id")),
    responses(
        (status = 202, description = "Asked: the node is fetched now, and right after that fetch succeeds its running configuration is saved as its startup configuration. The startup configuration it replaces is kept first. A save already waiting or running for the node is answered with its id rather than a second one", body = SaveAccepted),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks SaveDeviceConfig, the caller is an API token (`token_not_permitted`), or the caller is restricted to folders (`scope_unsupported`)", body = super::error::ErrorBody),
        (status = 404, description = "No such node the caller can see", body = super::error::ErrorBody),
        (status = 409, description = "Yagra cannot save this node: config backup is off or not ready, or its model has no save command (`config_save_unsupported`); or the fetch in front of it could not start (`config_backup_not_ready`, `config_backup_busy`)", body = super::error::ErrorBody),
        (status = 502, description = "The bundled oxidized did not answer (`oxidized_unreachable`)", body = super::error::ErrorBody),
        (status = 503, description = "This deployment has no config backup (`config_backup_unavailable`)", body = super::error::ErrorBody),
    ),
)]
async fn save_now(
    _perm: RequireSaveDeviceConfig,
    _visible: VisibleNode,
    Scoped(scope): Scoped,
    Caller(caller): Caller,
    admin: Admin,
    Path(node_id): Path<Uuid>,
) -> ApiResult<(StatusCode, Json<SaveAccepted>)> {
    super::scope::require_fleet_wide(
        &scope,
        "a save goes through the CLI login every folder shares, so an account restricted to \
         folders cannot ask for one",
    )?;
    oxidized_of(&admin)?;
    let unsupported = || {
        ApiError::conflict(
            "config_save_unsupported",
            "this node is not backed up, or Yagra has no save command for its model",
        )
    };
    let target = admin
        .config_backup
        .target(node_id)
        .await
        .map_err(|e| failed(&e, "config save target"))?
        .ok_or_else(unsupported)?;
    let model =
        oxidized::source::model_of(target.config.model_override.as_deref(), &target.platform)
            .ok_or_else(unsupported)?;
    if commands_for(&model).is_none() || !target.rotation.login_confirmed {
        return Err(unsupported());
    }
    let (id, created) = admin
        .config_saves
        .request(node_id, SaveOrigin::Person, Some(caller.user_id))
        .await
        .map_err(|e| failed(&e, "config save request"))?;
    match start_fetch(&admin, node_id).await {
        Ok(()) => {}
        // A fetch already out serves: the save runs after it succeeds.
        Err(e) if e.code() == "config_backup_pending" => {}
        Err(e) => {
            // Only a save this press created is given up; one already waiting is someone else's.
            if created {
                admin
                    .config_saves
                    .abandon(id)
                    .await
                    .map_err(|e| failed(&e, "config save abandon"))?;
            }
            return Err(e);
        }
    }
    Ok((StatusCode::ACCEPTED, Json(SaveAccepted { id })))
}

/// The startup configuration a save replaced.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct StartupText {
    text: String,
}

#[utoipa::path(
    get, path = "/api/v1/nodes/{node_id}/config-backup/saves/{save_id}/startup", tag = "nodes",
    params(
        ("node_id" = Uuid, Path, description = "Node id"),
        ("save_id" = Uuid, Path, description = "Save id"),
    ),
    responses(
        (status = 200, description = "The startup configuration the device held just before this save replaced it, with oxidized's secret filter applied", body = StartupText),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
        (status = 404, description = "No such node the caller can see, no such save, or none was kept (`config_save_not_found`)", body = super::error::ErrorBody),
    ),
)]
async fn get_startup(
    _perm: RequireManageConfig,
    _visible: VisibleNode,
    admin: Admin,
    Path((node_id, save_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<StartupText>> {
    let text = admin
        .config_saves
        .startup_before(node_id, save_id)
        .await
        .map_err(|e| failed(&e, "config save startup"))?
        .ok_or_else(|| {
            ApiError::not_found("config_save_not_found", "no startup configuration was kept")
        })?;
    Ok(Json(StartupText { text }))
}

/// How running differed from startup when the save script last compared them (Inc.9).
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct UnsavedText {
    /// The first time the two were seen to differ.
    pub(crate) since: chrono::DateTime<Utc>,
    /// When the script last compared them.
    pub(crate) checked_at: chrono::DateTime<Utc>,
    /// What the device printed for that comparison.
    pub(crate) text: String,
}

#[utoipa::path(
    get, path = "/api/v1/nodes/{node_id}/config-backup/unsaved", tag = "nodes",
    params(("node_id" = Uuid, Path, description = "Node id")),
    responses(
        (status = 200, description = "What the device printed when the save script last compared its running configuration with its startup configuration, while the two differ, with oxidized's secret filter and Yagra's own applied. Only a model Yagra compares over the command line has one", body = UnsavedText),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
        (status = 404, description = "No such node the caller can see, or it is not read as unsaved with a comparison kept (`config_unsaved_not_found`)", body = super::error::ErrorBody),
    ),
)]
async fn get_unsaved(
    _perm: RequireManageConfig,
    _visible: VisibleNode,
    admin: Admin,
    Path(node_id): Path<Uuid>,
) -> ApiResult<Json<UnsavedText>> {
    Ok(Json(unsaved_of(&admin, node_id).await?))
}

/// The comparison `GET …/unsaved` answers, shared with `/mcp`'s `get_node_config` (ADR-210 stage
/// 10) so the two surfaces refuse the same node for the same reason. The caller has already
/// checked the permission and the node's visibility.
pub(crate) async fn unsaved_of(admin: &super::AdminState, node_id: Uuid) -> ApiResult<UnsavedText> {
    let state = admin
        .config_saves
        .unsaved_of(node_id)
        .await
        .map_err(|e| failed(&e, "config unsaved read"))?;
    let (since, text) = state
        .as_ref()
        .and_then(|s| Some((s.since?, s.compare_output.clone()?)))
        .ok_or_else(|| {
            ApiError::not_found(
                "config_unsaved_not_found",
                "no unsaved comparison is kept for this node",
            )
        })?;
    let checked_at = state.map_or(since, |s| s.checked_at);
    Ok(UnsavedText {
        since,
        checked_at,
        text,
    })
}

/// The most nodes one automatic-save switch may name, as for the backup's own switch.
const MAX_AUTO_SAVE_SWITCH: usize = 5_000;

/// Automatic save switched on or off for many nodes at once (Inc.11 decision 14).
#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub(super) struct AutoSaveSwitchBody {
    node_ids: Vec<Uuid>,
    enabled: bool,
}

/// What an automatic-save switch did.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct AutoSaveSwitchResult {
    /// Switched by this call.
    switched: Vec<Uuid>,
    /// Not switched on: the node is not backed up now, or its model has no save command.
    unsupported: Vec<Uuid>,
    /// Named but already in that state.
    unchanged: usize,
}

#[utoipa::path(
    post, path = "/api/v1/config-backups/auto-save", tag = "nodes",
    request_body = AutoSaveSwitchBody,
    responses(
        (status = 200, description = "Switched. A node switched on is saved in the same login right after a fetch finds its running configuration differs from its startup one, outside a maintenance window; a failed save is tried again after the next fetch. Only a node backed up now whose model has a save command is switched on; switching the backup off switches this off too", body = AutoSaveSwitchResult),
        (status = 400, description = "No node, or more than 5,000 (`invalid_auto_save`)", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks SaveDeviceConfig, the caller is an API token (`token_not_permitted`), or the caller is restricted to folders (`scope_unsupported`)", body = super::error::ErrorBody),
        (status = 503, description = "This deployment has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn switch_auto_save(
    _perm: RequireSaveDeviceConfig,
    Scoped(scope): Scoped,
    Actor(actor): Actor,
    admin: Admin,
    Json(body): Json<AutoSaveSwitchBody>,
) -> ApiResult<Json<AutoSaveSwitchResult>> {
    super::scope::require_fleet_wide(
        &scope,
        "automatic save writes to a device through the CLI login every folder shares, so an \
         account restricted to folders cannot switch it",
    )?;
    let bad = |m: &str| ApiError::bad_request("invalid_auto_save", m.to_owned());
    if body.node_ids.is_empty() {
        return Err(bad("name at least one node"));
    }
    if body.node_ids.len() > MAX_AUTO_SAVE_SWITCH {
        return Err(bad("too many nodes in one switch"));
    }
    let mut ids = body.node_ids;
    ids.sort_unstable();
    ids.dedup();
    let (eligible, unsupported) = if body.enabled {
        let targets = admin
            .config_backup
            .targets()
            .await
            .map_err(|e| failed(&e, "config auto-save targets"))?;
        let saveable: std::collections::HashSet<Uuid> = targets
            .iter()
            .filter(|t| oxidized::save::can_save(t))
            .map(|t| t.node_id)
            .collect();
        ids.iter().partition(|id| saveable.contains(id))
    } else {
        (ids.clone(), Vec::new())
    };
    let switched = admin
        .config_backup
        .set_auto_save(&eligible, body.enabled)
        .await
        .map_err(|e| failed(&e, "config auto-save switch"))?;
    // One row per node, beside `audit_mw`'s row for the request, as the backup's switch writes
    // (ADR-224 decision 9): the path names no node, and switching one on lets Yagra write to it.
    let who = actor.as_deref().unwrap_or("(unknown)");
    for id in &switched {
        super::util::audit_record(
            &admin.audit,
            who,
            &format!("config_backup.auto_save node={id} enabled={}", body.enabled),
            200,
        )
        .await;
    }
    Ok(Json(AutoSaveSwitchResult {
        unchanged: ids.len() - switched.len() - unsupported.len(),
        switched,
        unsupported,
    }))
}

#[utoipa::path(
    put, path = "/api/v1/config-mcp-settings", tag = "nodes",
    request_body = McpConfigText,
    responses(
        (status = 200, description = "Saved. When on, an MCP client holding manage-config can read a backed-up configuration's text, its diffs and its unsaved comparison through get_node_config, with secrets blanked by oxidized and again by Yagra. Neither pass is complete, so a secret both miss reaches the client. Every read is audited", body = McpConfigText),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageSystem, the caller is an API token (`session_required`), or the caller is restricted to folders (`scope_unsupported`)", body = super::error::ErrorBody),
    ),
)]
async fn put_mcp_config_text(
    _perm: RequireManageSystem,
    _person: Caller,
    Scoped(scope): Scoped,
    admin: Admin,
    Json(body): Json<McpConfigText>,
) -> ApiResult<Json<McpConfigText>> {
    super::scope::require_fleet_wide(
        &scope,
        "this setting decides what AI clients may read about every device, so an account \
         restricted to folders cannot set it",
    )?;
    let saved = admin
        .config_saves
        .set_mcp_config_text(body)
        .await
        .map_err(|e| failed(&e, "config text for MCP setting"))?;
    Ok(Json(saved))
}

/// Whether Yagra may save a device on its own at this fetch (decision 13): the node is switched
/// on (Inc.11 decision 14) and is not in maintenance or an upgrade pause. A failed save does not
/// stop the next one (decision 21, which replaces decision 9): it is tried again after the next
/// fetch, in the login that fetch just used.
///
/// It saves only what Yagra itself reads as unsaved, never on the Unsaved configuration alert:
/// threshold rules are `ManageConfig`, an Operator's, and a write to a device is Admin's. A model
/// that can compare does so in the same login before saving; one read over SNMP needs
/// `unsaved_secs > 0` here.
fn auto_save_allowed(switched_on: bool, in_maintenance: bool) -> bool {
    switched_on && !in_maintenance
}

/// Whether a model read over SNMP is due an automatic save: allowed, and read as unsaved.
fn snmp_save_due(allowed: bool, unsaved_secs: Option<f64>) -> bool {
    allowed && unsaved_secs.is_some_and(|s| s > 0.0)
}

/// Whether Yagra may save `node` on its own at this fetch. For a model that cannot compare in the
/// CLI, an allowed save is asked for here, against the SNMP reading, and recorded in the audit log
/// under Yagra's own name, because no request carried it.
async fn auto_save(
    st: &ApiState,
    admin: &Admin,
    node: Uuid,
    switched_on: bool,
    can_compare: bool,
) -> anyhow::Result<bool> {
    let saves = &admin.config_saves;
    if !switched_on {
        return Ok(false);
    }
    let in_maintenance = st
        .alerts
        .in_maintenance(node.into(), Utc::now().timestamp_millis());
    let allowed = auto_save_allowed(switched_on, in_maintenance);
    if !allowed || can_compare {
        return Ok(allowed);
    }
    let unsaved = st
        .store
        .latest(&yagra_common::SeriesKey::node(
            node.into(),
            yagra_common::config_unsaved::METRIC,
        ))
        .await;
    if snmp_save_due(allowed, unsaved) {
        // A save already waiting (a person's press) is run as it is and stays theirs in the log.
        let (id, created) = saves.request(node, SaveOrigin::Auto, None).await?;
        if created {
            audit_auto_save(admin, node, id, 202).await;
        }
    }
    Ok(allowed)
}

async fn audit_auto_save(admin: &Admin, node: Uuid, id: Uuid, status: u16) {
    super::util::audit_record(
        &admin.audit,
        "(yagra)",
        &format!("config.auto_save node={node} save={id}"),
        status,
    )
    .await;
}

/// The node the save script asks about.
#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub(super) struct SaveJobQuery {
    /// oxidized's name for the node: its id.
    node: Uuid,
}

/// What the save script does for one node, with what it needs to do it.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct SaveJob {
    /// `save`, `auto` (compare, and save when unsaved) or `check`.
    action: &'static str,
    /// The save this run reports against; absent for a check.
    #[serde(skip_serializing_if = "Option::is_none")]
    save: Option<Uuid>,
    ip: String,
    model: String,
    input: &'static str,
    group: &'static str,
    username: String,
    password: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    enable: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ssh_port: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    telnet_port: Option<u16>,
    /// The model's row of `config_save::SAVE_COMMANDS`, the only commands the script may send.
    #[schema(value_type = Object)]
    commands: SaveCommands,
}

#[utoipa::path(
    get, path = "/internal/oxidized/save", tag = "nodes",
    params(SaveJobQuery),
    responses(
        (status = 200, description = "What to do for the node just fetched, with its login in plaintext and its model's commands. For the save script inside the bundled oxidized alone: it demands the source token in `x-yagra-oxidized-token`", body = SaveJob),
        (status = 204, description = "Nothing to do for this node"),
        (status = 401, description = "No token, or the wrong one (`oxidized_token`)", body = super::error::ErrorBody),
        (status = 503, description = "This deployment has no config backup (`config_backup_unavailable`)", body = super::error::ErrorBody),
    ),
)]
async fn save_job(
    State(st): State<ApiState>,
    admin: Admin,
    headers: HeaderMap,
    Query(q): Query<SaveJobQuery>,
) -> ApiResult<axum::response::Response> {
    use axum::response::IntoResponse;
    let ox = oxidized_of(&admin)?;
    check_source_token(ox, &headers)?;
    let nothing = || Ok(StatusCode::NO_CONTENT.into_response());
    let Some(target) = admin
        .config_backup
        .target(q.node)
        .await
        .map_err(|e| failed(&e, "config save target"))?
    else {
        return nothing();
    };
    let Some(model) =
        oxidized::source::model_of(target.config.model_override.as_deref(), &target.platform)
    else {
        return nothing();
    };
    let switched_on = target.auto_save;
    // The login exactly as the source hands it to oxidized: through the node's relay port, and
    // only while it may be handed over at all. Read before an automatic save is asked for, so a
    // login that cannot be handed over never leaves Yagra a failed save of its own for a device it
    // never touched.
    let ports = admin
        .config_backup
        .relay_ports(&[q.node])
        .await
        .map_err(|e| failed(&e, "config save relay port"))?;
    let relay_ip = match oxidized::relay::host_of(&ox.settings.url) {
        Some(host) => oxidized::relay::relay_address(&host).await,
        None => None,
    };
    let login =
        oxidized::source::source_nodes(vec![target], &admin.creds, Utc::now(), &ports, relay_ip)
            .await
            .into_iter()
            .next()
            .filter(|n| n.ip != oxidized::source::PARKED_IP);
    let auto = match (&login, commands_for(&model)) {
        (Some(_), Some(commands)) => auto_save(
            &st,
            &admin,
            q.node,
            switched_on,
            commands.compare.is_some(),
        )
        .await
        .unwrap_or_else(|e| {
            tracing::warn!(node = %q.node, error = %e, "config save: automatic save not asked");
            false
        }),
        _ => false,
    };
    let pending = admin
        .config_saves
        .claim(q.node)
        .await
        .map_err(|e| failed(&e, "config save claim"))?
        .map(|(id, _)| id);
    let saves = admin.config_saves.clone();
    let give_up = |failure: SaveFailure| {
        let saves = saves.clone();
        async move {
            if let Some(id) = pending {
                saves.finish(id, Some(failure), None).await.ok();
            }
        }
    };
    let Some(login) = login else {
        give_up(SaveFailure::NotFetched).await;
        return nothing();
    };
    let Some(action) = oxidized::save::decide(&model, pending, auto) else {
        give_up(SaveFailure::Unsupported).await;
        return nothing();
    };
    let Some(commands) = commands_for(&model) else {
        give_up(SaveFailure::Unsupported).await;
        return nothing();
    };
    metrics::counter!("yagra_config_save_jobs_total", "action" => match action {
        Action::Save(_) => "save",
        Action::SaveIfUnsaved => "auto",
        Action::Check => "check",
    })
    .increment(1);
    let (action, save) = match action {
        Action::Save(id) => ("save", Some(id)),
        Action::SaveIfUnsaved => ("auto", None),
        Action::Check => ("check", None),
    };
    let job = SaveJob {
        action,
        save,
        ip: login.ip,
        model,
        input: login.input,
        group: login.group,
        username: login.username,
        password: login.password,
        enable: login.enable,
        ssh_port: login.ssh_port,
        telnet_port: login.telnet_port,
        commands: *commands,
    };
    let mut response = Json(job).into_response();
    response.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"),
    );
    Ok(response)
}

/// How the save script's run went, as it posts it.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub(super) struct SaveReport {
    node: Uuid,
    /// `save`, `auto` or `check`.
    action: String,
    /// The save reported on; empty for a check.
    #[serde(default)]
    save: String,
    /// A `SaveFailure` token; empty when it succeeded.
    #[serde(default)]
    failure: String,
    /// `true` or `false` when the model can compare running with startup; empty otherwise. For
    /// `auto`, `true` means a save was tried in the same login.
    #[serde(default)]
    unsaved_before: String,
    /// The same, read again after a save that succeeded.
    #[serde(default)]
    unsaved_after: String,
    /// The startup configuration read just before a save.
    #[serde(default)]
    startup: String,
    /// What the device printed for the comparison the reading above was taken from (the later one
    /// after a save that worked, the earlier one otherwise), through oxidized's secret filter.
    /// Kept only while the two differ (Inc.9).
    #[serde(default)]
    compare: String,
}

/// The comparison with Yagra's own secret pass applied before it is stored (ADR-222 Inc.12).
///
/// oxidized's filters are written for a configuration, and several anchor at the start of a line
/// (NX-OS's `^(snmp-server community)`). A comparison is a diff, whose lines start with `+ `, `- `
/// or `! `, so those filters miss exactly the lines that changed. The pass `/mcp` applies to
/// configuration text matches a secret word anywhere in a line, so it runs here for every model.
/// ⚠️ It is a second net, not a guarantee (see `mcp/config_text.rs`).
///
/// The diff marker is taken off each line before the pass and put back after it: some of its
/// rules read a token's position (`snmp-server host <address> … <community>` names no secret word,
/// and is recognised by `host` being the second token), and a leading `+` would shift it.
fn blank_compare(text: &str) -> String {
    let (markers, bodies): (Vec<&str>, Vec<&str>) = text.lines().map(split_diff_marker).unzip();
    crate::mcp::config_text::redact_lines(bodies.into_iter())
        .into_iter()
        .zip(markers)
        .map(|((line, _), marker)| format!("{marker}{line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// A diff line's marker (`+ `, `- `, `! `, or the bare character on a line of its own) and the
/// rest. `---`/`+++` headings and lines without one come back with an empty marker.
fn split_diff_marker(line: &str) -> (&str, &str) {
    let mut chars = line.chars();
    match (chars.next(), chars.next()) {
        (Some('+' | '-' | '!'), Some(' ')) => line.split_at(2),
        (Some('+' | '-' | '!'), None) => line.split_at(1),
        _ => ("", line),
    }
}

fn flag(text: &str) -> Option<bool> {
    match text {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

#[utoipa::path(
    post, path = "/internal/oxidized/save", tag = "nodes",
    request_body(content = SaveReport, content_type = "application/x-www-form-urlencoded"),
    responses(
        (status = 204, description = "Recorded. For the save script inside the bundled oxidized alone: it demands the source token in `x-yagra-oxidized-token`"),
        (status = 401, description = "No token, or the wrong one (`oxidized_token`)", body = super::error::ErrorBody),
        (status = 503, description = "This deployment has no config backup (`config_backup_unavailable`)", body = super::error::ErrorBody),
    ),
)]
async fn save_report(
    admin: Admin,
    headers: HeaderMap,
    Form(r): Form<SaveReport>,
) -> ApiResult<StatusCode> {
    let ox = oxidized_of(&admin)?;
    check_source_token(ox, &headers)?;
    let saves = &admin.config_saves;
    let failure = if r.failure.is_empty() {
        // A save the device answered as a success, after which running and startup still differ,
        // did not save: success counts only when the re-read shows no difference (decision 6).
        (flag(&r.unsaved_after) == Some(true)).then_some(SaveFailure::NoAnswer)
    } else {
        Some(SaveFailure::from_token(&r.failure).unwrap_or(SaveFailure::Other))
    };
    // What the device said about running and startup: after a save that worked, the later reading
    // is the one that holds; otherwise the earlier one.
    let unsaved = if failure.is_none() {
        flag(&r.unsaved_after).or_else(|| flag(&r.unsaved_before))
    } else {
        flag(&r.unsaved_before)
    };
    if let Some(unsaved) = unsaved {
        let compare = Some(blank_compare(&r.compare)).filter(|s| !s.is_empty());
        saves
            .record_unsaved(r.node, unsaved, compare.as_deref())
            .await
            .map_err(|e| failed(&e, "config unsaved record"))?;
        if !unsaved {
            admin.config_unsaved.mark_saved(r.node);
        }
    }
    if r.action == "auto" && flag(&r.unsaved_before) == Some(true) {
        let startup = Some(r.startup.as_str()).filter(|s| !s.is_empty());
        let id = saves
            .record_auto(r.node, failure, startup)
            .await
            .map_err(|e| failed(&e, "config auto save record"))?;
        let outcome = failure.map_or("saved", SaveFailure::as_str);
        metrics::counter!("yagra_config_saves_total", "outcome" => outcome).increment(1);
        audit_auto_save(
            &admin,
            r.node,
            id,
            if failure.is_none() { 204 } else { 502 },
        )
        .await;
        tracing::info!(node = %r.node, save = %id, outcome, "config auto save finished");
    }
    if r.action == "save" {
        if let Ok(id) = r.save.parse::<Uuid>() {
            let startup = Some(r.startup.as_str()).filter(|s| !s.is_empty());
            let finished = saves
                .finish(id, failure, startup)
                .await
                .map_err(|e| failed(&e, "config save finish"))?;
            let outcome = match failure {
                None => "saved",
                Some(f) => f.as_str(),
            };
            metrics::counter!("yagra_config_saves_total", "outcome" => outcome).increment(1);
            if finished {
                tracing::info!(node = %r.node, save = %id, outcome, "config save finished");
            }
        }
    }
    if saved_just_now(&r, failure) {
        recheck_after_save(admin.0.clone(), r.node);
    }
    Ok(StatusCode::NO_CONTENT)
}

/// Whether this report is of a save the device just took: one asked for, or an automatic one the
/// comparison called for, that did not fail.
fn saved_just_now(r: &SaveReport, failure: Option<SaveFailure>) -> bool {
    failure.is_none()
        && (r.action == "save" || (r.action == "auto" && flag(&r.unsaved_before) == Some(true)))
}

/// How far apart the two polls after a save are, so they arrive as two readings in order.
const RECHECK_GAP: std::time::Duration = std::time::Duration::from_secs(5);

/// Poll `node` twice, right after a save, rather than leave the Unsaved configuration alert open
/// until the schedule polls it twice more (its dwell is 2, so up to two intervals). Each poll is a
/// real reading: SNMP reads the device's own history again, and the CLI reading was written into
/// the cache above. A poll that cannot be sent costs only that wait, so it never fails the report.
fn recheck_after_save(admin: std::sync::Arc<super::AdminState>, node: Uuid) {
    tokio::spawn(async move {
        for i in 0..2 {
            if i > 0 {
                tokio::time::sleep(RECHECK_GAP).await;
            }
            if let Err(e) = super::nodes::poll_now(&admin, node).await {
                tracing::warn!(node = %node, error = ?e, "could not poll the node after its save");
                return;
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::tests_support::{send, token};
    use std::sync::Arc;

    fn report(action: &str, unsaved_before: &str) -> SaveReport {
        SaveReport {
            node: Uuid::nil(),
            action: action.to_owned(),
            save: String::new(),
            failure: String::new(),
            unsaved_before: unsaved_before.to_owned(),
            unsaved_after: String::new(),
            startup: String::new(),
            compare: String::new(),
        }
    }

    /// ADR-222 Inc.12: a diff line oxidized's line-anchored filter misses is blanked before the
    /// comparison is stored, and a line naming no secret is kept as it came.
    #[test]
    fn the_stored_comparison_loses_the_secrets_a_diff_hides_from_oxidized() {
        let diff = "+ snmp-server community S3cret group network-operator\n\
                    - tacacs-server key 7 \"0x1234\"\n \
                    === Current configuration line 9\n+ vlan 30";
        let stored = blank_compare(diff);
        assert!(!stored.contains("S3cret"), "{stored}");
        assert!(!stored.contains("0x1234"), "{stored}");
        assert!(
            stored.contains(" === Current configuration line 9"),
            "{stored}"
        );
        assert!(stored.contains("+ vlan 30"), "{stored}");
        assert_eq!(blank_compare(""), "");
        // The community trails `snmp-server host` with no secret word before it; the rule that
        // finds it reads token positions, which the diff marker would otherwise shift.
        for line in [
            "+ snmp-server host 192.0.2.10 traps version 2c c0mmun1ty",
            "- snmp-server host 192.0.2.10 traps version 2c c0mmun1ty",
            "! snmp-server host 192.0.2.10 traps version 2c c0mmun1ty",
        ] {
            let stored = blank_compare(line);
            assert!(!stored.contains("c0mmun1ty"), "{stored}");
            assert!(
                stored.starts_with(&line[..2]),
                "the marker is kept: {stored}"
            );
        }
        assert_eq!(
            blank_compare("--- 1,3 ----\n+++  Running-config"),
            "--- 1,3 ----\n+++  Running-config"
        );
    }

    /// Only a save the device took asks for the polls that close the alert at once: a check, an
    /// automatic run that found nothing to save, and a failed save leave the schedule to it.
    #[test]
    fn only_a_save_that_worked_rechecks_at_once() {
        assert!(saved_just_now(&report("save", ""), None));
        assert!(saved_just_now(&report("auto", "true"), None));
        assert!(!saved_just_now(&report("auto", "false"), None));
        assert!(!saved_just_now(&report("check", "true"), None));
        assert!(!saved_just_now(
            &report("save", ""),
            Some(SaveFailure::NoAnswer)
        ));
    }

    fn ox_state_home() -> std::path::PathBuf {
        let home = std::env::temp_dir().join(format!("yagra-ox-save-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&home).expect("dir");
        home
    }

    async fn with_oxidized(pool: sqlx::PgPool, home: &std::path::Path) -> ApiState {
        crate::api::tests_support::live_state_with_oxidized(
            pool,
            Arc::new(crate::api::config_backup::OxidizedHandle {
                settings: oxidized::Settings {
                    url: "http://127.0.0.1:9".to_owned(),
                    home: home.to_path_buf(),
                },
                client: Arc::new(oxidized::client::OxidizedClient::new("http://127.0.0.1:9")),
            }),
        )
        .await
    }

    async fn post_report(st: &ApiState, token: &str, form: &str) -> StatusCode {
        use axum::body::Body;
        use tower::ServiceExt;
        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/internal/oxidized/save")
            .header(oxidized::config::TOKEN_HEADER, token)
            .header(
                axum::http::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .body(Body::from(form.to_owned()))
            .expect("request");
        crate::api::router(st.clone())
            .oneshot(req)
            .await
            .expect("send")
            .status()
    }

    /// The script's report is accepted: a claimed save is recorded as saved with the startup it
    /// replaced, a check records what it saw, and the wrong token changes nothing.
    #[sqlx::test(migrator = "yagra_base::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn the_save_script_report_is_recorded(pool: sqlx::PgPool) {
        let home = ox_state_home();
        let ox_token = oxidized::config::ensure_token(&home).expect("token");
        let st = with_oxidized(pool.clone(), &home).await;
        let node = yagra_base::pgtest::node(&pool, "sw-01", 1, None).await;
        let saves = yagra_base::config_save::ConfigSaveRepo::new(pool.clone());
        let (id, _) = saves
            .request(node, SaveOrigin::Person, None)
            .await
            .expect("request");
        assert!(saves.claim(node).await.expect("claim").is_some());

        let form = format!(
            "node={node}&action=save&save={id}&failure=&unsaved_before=&unsaved_after=&startup=hostname+sw-01"
        );
        assert_eq!(
            post_report(&st, "wrong", &form).await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            post_report(&st, &ox_token, &form).await,
            StatusCode::NO_CONTENT
        );
        let recent = saves.recent(node).await.expect("recent");
        assert_eq!(recent[0].state, yagra_base::config_save::SaveState::Saved);
        assert_eq!(
            saves
                .startup_before(node, id)
                .await
                .expect("startup")
                .as_deref(),
            Some("hostname sw-01")
        );

        let check = format!(
            "node={node}&action=check&unsaved_before=true&compare=+%3D%3D%3D+Current+configuration+line+9%0Avcmp+role+client"
        );
        assert_eq!(
            post_report(&st, &ox_token, &check).await,
            StatusCode::NO_CONTENT
        );
        assert!(saves.unsaved_all().await.expect("unsaved")[&node].is_some());
        // What the device printed for the comparison is kept beside the reading (Inc.9), and is
        // configuration text: an Operator reads it, a Viewer sees only that the node is unsaved.
        let state = saves
            .unsaved_of(node)
            .await
            .expect("read")
            .expect("checked");
        assert_eq!(
            state.compare_output.as_deref(),
            Some(" === Current configuration line 9\nvcmp role client")
        );
        let unsaved_path = format!("/api/v1/nodes/{node}/config-backup/unsaved");
        let operator = token(&st, yagra_common::Role::Operator);
        let (status, body) = send(&st, "GET", &unsaved_path, &operator, None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            body["text"],
            " === Current configuration line 9\nvcmp role client"
        );
        let viewer = token(&st, yagra_common::Role::Viewer);
        let (status, _) = send(&st, "GET", &unsaved_path, &viewer, None).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let view_path = format!("/api/v1/nodes/{node}/config-backup");
        let (status, body) = send(&st, "GET", &view_path, &viewer, None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["unsaved"]["has_output"], true);

        // An automatic run that found the device saved records no save; one that found it unsaved
        // and saved it records a finished automatic save with the startup it replaced.
        let quiet = format!("node={node}&action=auto&unsaved_before=false");
        assert_eq!(
            post_report(&st, &ox_token, &quiet).await,
            StatusCode::NO_CONTENT
        );
        assert_eq!(saves.recent(node).await.expect("recent").len(), 1);
        assert!(saves.unsaved_all().await.expect("unsaved")[&node].is_none());
        let state = saves
            .unsaved_of(node)
            .await
            .expect("read")
            .expect("checked");
        assert_eq!(state.compare_output, None);
        let (status, body) = send(&st, "GET", &unsaved_path, &operator, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"]["code"], "config_unsaved_not_found");
        let auto = format!(
            "node={node}&action=auto&failure=&unsaved_before=true&unsaved_after=false&startup=hostname+sw-01"
        );
        assert_eq!(
            post_report(&st, &ox_token, &auto).await,
            StatusCode::NO_CONTENT
        );
        let recent = saves.recent(node).await.expect("recent");
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].origin, SaveOrigin::Auto);
        assert_eq!(recent[0].state, yagra_base::config_save::SaveState::Saved);
        assert!(recent[0].kept_startup);
        assert!(saves.unsaved_all().await.expect("unsaved")[&node].is_none());

        // The device answered the save as a success, but running and startup still differ when
        // read again: that is not a save (decision 6), and the device stays unsaved.
        let unconfirmed = format!(
            "node={node}&action=auto&failure=&unsaved_before=true&unsaved_after=true&startup=hostname+sw-01"
        );
        assert_eq!(
            post_report(&st, &ox_token, &unconfirmed).await,
            StatusCode::NO_CONTENT
        );
        let recent = saves.recent(node).await.expect("recent");
        assert_eq!(recent.len(), 3);
        assert_eq!(recent[0].state, yagra_base::config_save::SaveState::Failed);
        assert_eq!(recent[0].failure, Some(SaveFailure::NoAnswer));
        assert!(saves.unsaved_all().await.expect("unsaved")[&node].is_some());
        std::fs::remove_dir_all(&home).ok();
    }

    /// The button is Admin's alone, and an API token is refused even for an Admin.
    #[sqlx::test(migrator = "yagra_base::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn only_a_signed_in_admin_may_ask_for_a_save(pool: sqlx::PgPool) {
        let home = ox_state_home();
        let st = with_oxidized(pool.clone(), &home).await;
        let node = yagra_base::pgtest::node(&pool, "sw-01", 1, None).await;
        let path = format!("/api/v1/nodes/{node}/config-backup/save");
        let operator = token(&st, yagra_common::Role::Operator);
        let (status, _) = send(&st, "POST", &path, &operator, None).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        // An Admin passes the guard and meets the node, which has no config backup.
        let admin = token(&st, yagra_common::Role::Admin);
        let (status, body) = send(&st, "POST", &path, &admin, None).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body["error"]["code"], "config_save_unsupported");
        std::fs::remove_dir_all(&home).ok();
    }

    fn backed_up(model: &str) -> yagra_base::config_backup::NodeConfigBackup {
        yagra_base::config_backup::NodeConfigBackup {
            enabled: true,
            login_order: None,
            input: yagra_base::config_backup::BackupInput::Ssh,
            model_override: Some(model.to_owned()),
            scan_mode: yagra_base::config_backup::ScanMode::Normal,
        }
    }

    /// Automatic save is switched per node by an Admin (Inc.11): only a node backed up with a model
    /// Yagra can save is switched on, the list and the node's view read it back, the schedule's
    /// deployment-wide flag follows it, and an Operator is refused.
    #[sqlx::test(migrator = "yagra_base::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn automatic_save_is_switched_per_node(pool: sqlx::PgPool) {
        let home = ox_state_home();
        let st = with_oxidized(pool.clone(), &home).await;
        let repo = yagra_base::config_backup::ConfigBackupRepo::new(pool.clone());
        let vrp = yagra_base::pgtest::node(&pool, "sw-01", 1, None).await;
        let asa = yagra_base::pgtest::node(&pool, "fw-01", 2, None).await;
        let off = yagra_base::pgtest::node(&pool, "sw-02", 3, None).await;
        repo.put(vrp, &backed_up("vrp"), true).await.expect("vrp");
        repo.put(asa, &backed_up("asa"), true).await.expect("asa");
        let path = "/api/v1/config-backups/auto-save";
        let body = serde_json::json!({ "node_ids": [vrp, asa, off], "enabled": true });

        let operator = token(&st, yagra_common::Role::Operator);
        let (status, _) = send(&st, "POST", path, &operator, Some(body.clone())).await;
        assert_eq!(status, StatusCode::FORBIDDEN);

        let admin = token(&st, yagra_common::Role::Admin);
        let (status, out) = send(&st, "POST", path, &admin, Some(body.clone())).await;
        assert_eq!(status, StatusCode::OK, "{out}");
        assert_eq!(out["switched"], serde_json::json!([vrp]));
        let mut unsupported = vec![asa.to_string(), off.to_string()];
        unsupported.sort();
        assert_eq!(out["unsupported"], serde_json::json!(unsupported));
        assert_eq!(out["unchanged"], 0);
        assert!(repo.auto_save_of(vrp).await.expect("vrp"));
        assert!(!repo.auto_save_of(asa).await.expect("asa"));

        let (_, list) = send(&st, "GET", "/api/v1/config-backups?state=all", &admin, None).await;
        let row = |id: Uuid| {
            list.as_array()
                .expect("rows")
                .iter()
                .find(|r| r["node_id"] == id.to_string())
                .cloned()
                .expect("row")
        };
        assert_eq!(row(vrp)["auto_save"], true);
        assert_eq!(row(vrp)["save_supported"], true);
        assert_eq!(row(asa)["auto_save"], false);
        assert_eq!(row(asa)["save_supported"], false);
        let (_, view) = send(
            &st,
            "GET",
            &format!("/api/v1/nodes/{vrp}/config-backup"),
            &admin,
            None,
        )
        .await;
        assert_eq!(view["auto_save"], true);
        let (_, schedule) = send(&st, "GET", "/api/v1/config-backup-schedule", &admin, None).await;
        assert_eq!(schedule["auto_save"]["enabled"], true);

        // Again: nothing changes. Off is accepted and written.
        let (_, out) = send(&st, "POST", path, &admin, Some(body)).await;
        assert_eq!(out["unchanged"], 1);
        let off_body = serde_json::json!({ "node_ids": [vrp], "enabled": false });
        let (status, out) = send(&st, "POST", path, &admin, Some(off_body)).await;
        assert_eq!(status, StatusCode::OK, "{out}");
        assert_eq!(out["switched"], serde_json::json!([vrp]));
        assert!(!repo.auto_save_of(vrp).await.expect("vrp"));
        let (_, schedule) = send(&st, "GET", "/api/v1/config-backup-schedule", &admin, None).await;
        assert_eq!(schedule["auto_save"]["enabled"], false);

        let (status, out) = send(
            &st,
            "POST",
            path,
            &admin,
            Some(serde_json::json!({ "node_ids": [], "enabled": true })),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(out["error"]["code"], "invalid_auto_save");
        std::fs::remove_dir_all(&home).ok();
    }

    /// Configuration text for MCP is off by default, refused to an Operator, switched on by an
    /// Admin and read back with the schedule (ADR-210 stage 10 decision 85). That an API token is
    /// refused is `Caller`'s own rule, tested in `extract.rs`.
    #[sqlx::test(migrator = "yagra_base::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn config_text_for_mcp_is_set_by_an_admin(pool: sqlx::PgPool) {
        let home = ox_state_home();
        let st = with_oxidized(pool.clone(), &home).await;
        let path = "/api/v1/config-mcp-settings";
        let body = serde_json::json!({ "enabled": true });
        let admin = token(&st, yagra_common::Role::Admin);
        let (status, schedule) =
            send(&st, "GET", "/api/v1/config-backup-schedule", &admin, None).await;
        assert_eq!(status, StatusCode::OK, "{schedule}");
        assert_eq!(
            schedule["mcp_config_text"]["enabled"], false,
            "off by default"
        );
        let operator = token(&st, yagra_common::Role::Operator);
        let (status, _) = send(&st, "PUT", path, &operator, Some(body.clone())).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (status, saved) = send(&st, "PUT", path, &admin, Some(body)).await;
        assert_eq!(status, StatusCode::OK, "{saved}");
        assert_eq!(saved, serde_json::json!({ "enabled": true }));
        let (_, schedule) = send(&st, "GET", "/api/v1/config-backup-schedule", &admin, None).await;
        assert_eq!(schedule["mcp_config_text"]["enabled"], true);
        std::fs::remove_dir_all(&home).ok();
    }

    /// An automatic save waits for the node's switch and a quiet window, and for nothing else: a
    /// failed save does not stop the next one (Inc.11 decision 21), so the decision takes no record.
    #[test]
    fn an_automatic_save_waits_for_the_switch_and_a_quiet_window() {
        assert!(auto_save_allowed(true, false));
        assert!(!auto_save_allowed(false, false));
        // A maintenance window or an upgrade pause (`in_maintenance` answers both).
        assert!(!auto_save_allowed(true, true));
    }

    /// A model read over SNMP is saved only while Yagra reads it as unsaved: a device already
    /// saved is never written to, whatever any alert rule says.
    #[test]
    fn an_snmp_read_model_is_saved_only_while_read_as_unsaved() {
        assert!(snmp_save_due(true, Some(60.0)));
        assert!(!snmp_save_due(true, Some(0.0)));
        assert!(!snmp_save_due(true, None));
        assert!(!snmp_save_due(false, Some(60.0)));
    }

    #[test]
    fn a_flag_is_read_only_from_the_two_words_the_script_sends() {
        assert_eq!(flag("true"), Some(true));
        assert_eq!(flag("false"), Some(false));
        assert_eq!(flag(""), None);
        assert_eq!(flag("1"), None);
    }
}
