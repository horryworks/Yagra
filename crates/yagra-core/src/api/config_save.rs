// SPDX-License-Identifier: AGPL-3.0-only
//! Saving a device's running configuration from Yagra (ADR-222).
//!
//! - `POST /api/v1/nodes/{id}/config-backup/save` — the button. `SaveDeviceConfig` (Admin only,
//!   never an API token) and a fleet-wide caller, because it hands the node's login to the device
//!   as the backup does. It records a pending save and asks for a fetch; the save itself runs after
//!   that fetch succeeds, so what is saved is already backed up (decision 7). Audited by
//!   `audit_mw` like every write.
//! - `GET …/config-backup/saves/{save_id}/startup` — the startup configuration a save replaced.
//!   `ManageConfig`, and off MCP, for the reason a version's text is (ADR-210 decision 16).
//! - `PUT /api/v1/config-save-settings` — automatic save on or off (decision 8). When is the
//!   Unsaved configuration alert's to say (decision 11). `SaveDeviceConfig` and a fleet-wide
//!   caller: it lets Yagra save every
//!   device on its own. Read back in `GET /api/v1/config-backup-schedule`.
//!
//! And two routes for the save script inside the bundled oxidized, outside `/api/v1`, behind the
//! source token, on the internal listener only (ADR-217):
//!
//! - `GET /internal/oxidized/save?node=` — what to do after a fetch: nothing (204), a check, or a
//!   save, with the node's login and its model's commands from `config_save::SAVE_COMMANDS`. **It
//!   returns a device login in plaintext**, like `GET /internal/oxidized/nodes`, for the same reader.
//! - `POST /internal/oxidized/save` — how it went: a failure kind, whether running and startup
//!   differed, and the startup configuration kept before a save. Never the device's output beyond
//!   that one copy.

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post},
    Form, Json, Router,
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use yagra_base::config_save::{AutoSave, SaveFailure, SaveOrigin};
use yagra_common::config_save::{commands_for, SaveCommands};

use super::config_backup::{check_source_token, oxidized_of, start_fetch};
use super::error::{ApiError, ApiResult};
use super::extract::{
    Admin, Caller, RequireManageConfig, RequireSaveDeviceConfig, Scoped, VisibleNode,
};
use super::ApiState;
use crate::oxidized::{self, save::Action};

/// This domain's slice of the OpenAPI document (ADR-035), merged by [`super::openapi::document`].
#[derive(utoipa::OpenApi)]
#[openapi(paths(save_now, get_startup, put_auto_save, save_job, save_report))]
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
            "/api/v1/config-save-settings",
            axum::routing::put(put_auto_save),
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
    let model = oxidized::source::model_of(
        target.config.model_override.as_deref(),
        target.profile_name.as_deref(),
    )
    .ok_or_else(unsupported)?;
    if commands_for(&model).is_none() || !target.rotation.login_confirmed {
        return Err(unsupported());
    }
    let id = admin
        .config_saves
        .request(node_id, SaveOrigin::Person, Some(caller.user_id))
        .await
        .map_err(|e| failed(&e, "config save request"))?;
    match start_fetch(&admin, node_id).await {
        Ok(()) => {}
        // A fetch already out serves: the save runs after it succeeds.
        Err(e) if e.code() == "config_backup_pending" => {}
        Err(e) => {
            admin
                .config_saves
                .abandon(id)
                .await
                .map_err(|e| failed(&e, "config save abandon"))?;
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

#[utoipa::path(
    put, path = "/api/v1/config-save-settings", tag = "nodes",
    request_body = AutoSave,
    responses(
        (status = 200, description = "Saved. When on, a device with an open Unsaved configuration alert is saved right after its next successful fetch, unless its last save failed. How long a device may stay unsaved is that alert rule's threshold", body = AutoSave),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks SaveDeviceConfig, the caller is an API token (`token_not_permitted`), or the caller is restricted to folders (`scope_unsupported`)", body = super::error::ErrorBody),
    ),
)]
async fn put_auto_save(
    _perm: RequireSaveDeviceConfig,
    Scoped(scope): Scoped,
    admin: Admin,
    Json(body): Json<AutoSave>,
) -> ApiResult<Json<AutoSave>> {
    super::scope::require_fleet_wide(
        &scope,
        "automatic save acts on every device, so an account restricted to folders cannot set it",
    )?;
    let saved = admin
        .config_saves
        .set_auto_save(body)
        .await
        .map_err(|e| failed(&e, "config auto-save setting"))?;
    Ok(Json(saved))
}

/// Whether Yagra may save a device on its own at this fetch (decision 13): switched on, the device
/// is not in maintenance or an upgrade pause, and its last save did not fail - after a failure a
/// person looks first (decision 9).
///
/// It saves only what Yagra itself reads as unsaved, never on the Unsaved configuration alert:
/// threshold rules are `ManageConfig`, an Operator's, and a write to a device is Admin's. A model
/// that can compare does so in the same login before saving; one read over SNMP needs
/// `unsaved_secs > 0` here.
fn auto_save_allowed(setting: AutoSave, in_maintenance: bool, last_failed: bool) -> bool {
    setting.enabled && !in_maintenance && !last_failed
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
    can_compare: bool,
) -> anyhow::Result<bool> {
    let saves = &admin.config_saves;
    let setting = saves.auto_save().await?;
    if !setting.enabled {
        return Ok(false);
    }
    let in_maintenance = st
        .alerts
        .in_maintenance(node.into(), Utc::now().timestamp_millis());
    let last_failed = saves.last_failed(node).await?;
    let allowed = auto_save_allowed(setting, in_maintenance, last_failed);
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
        let id = saves.request(node, SaveOrigin::Auto, None).await?;
        audit_auto_save(admin, node, id, 202).await;
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
    let Some(model) = oxidized::source::model_of(
        target.config.model_override.as_deref(),
        target.profile_name.as_deref(),
    ) else {
        return nothing();
    };
    let auto = match commands_for(&model) {
        Some(commands) => auto_save(&st, &admin, q.node, commands.compare.is_some())
            .await
            .unwrap_or_else(|e| {
                tracing::warn!(node = %q.node, error = %e, "config save: automatic save not asked");
                false
            }),
        None => false,
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
    let Some(action) = oxidized::save::decide(&model, pending, auto) else {
        give_up(SaveFailure::Unsupported).await;
        return nothing();
    };
    let Some(commands) = commands_for(&model) else {
        give_up(SaveFailure::Unsupported).await;
        return nothing();
    };
    // The login exactly as the source hands it to oxidized: through the node's relay port, and
    // only while it may be handed over at all.
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
    let Some(login) = login else {
        give_up(SaveFailure::NotFetched).await;
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
        None
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
        saves
            .record_unsaved(r.node, unsaved)
            .await
            .map_err(|e| failed(&e, "config unsaved record"))?;
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
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::tests_support::{send, token};
    use std::sync::Arc;

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
        let id = saves
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

        let check = format!("node={node}&action=check&unsaved_before=true");
        assert_eq!(
            post_report(&st, &ox_token, &check).await,
            StatusCode::NO_CONTENT
        );
        assert!(saves.unsaved_all().await.expect("unsaved")[&node].is_some());

        // An automatic run that found the device saved records no save; one that found it unsaved
        // and saved it records a finished automatic save with the startup it replaced.
        let quiet = format!("node={node}&action=auto&unsaved_before=false");
        assert_eq!(
            post_report(&st, &ox_token, &quiet).await,
            StatusCode::NO_CONTENT
        );
        assert_eq!(saves.recent(node).await.expect("recent").len(), 1);
        assert!(saves.unsaved_all().await.expect("unsaved")[&node].is_none());
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

    /// Automatic save is switched on by an Admin and read back with the schedule.
    #[sqlx::test(migrator = "yagra_base::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn automatic_save_is_set_and_read_back(pool: sqlx::PgPool) {
        let home = ox_state_home();
        let st = with_oxidized(pool.clone(), &home).await;
        let admin = token(&st, yagra_common::Role::Admin);
        let operator = token(&st, yagra_common::Role::Operator);
        // A client from before decision 11 still sends `after_hours`; it is ignored, not refused.
        let body = serde_json::json!({ "enabled": true, "after_hours": 12 });
        let (status, _) = send(
            &st,
            "PUT",
            "/api/v1/config-save-settings",
            &operator,
            Some(body.clone()),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (status, saved) = send(
            &st,
            "PUT",
            "/api/v1/config-save-settings",
            &admin,
            Some(body),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{saved}");
        assert_eq!(saved, serde_json::json!({ "enabled": true }));
        let (status, schedule) =
            send(&st, "GET", "/api/v1/config-backup-schedule", &admin, None).await;
        assert_eq!(status, StatusCode::OK, "{schedule}");
        assert_eq!(schedule["auto_save"]["enabled"], true);
        std::fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn an_automatic_save_waits_for_the_switch_a_quiet_window_and_a_clean_record() {
        let on = AutoSave { enabled: true };
        assert!(auto_save_allowed(on, false, false));
        assert!(!auto_save_allowed(AutoSave::default(), false, false));
        // A maintenance window or an upgrade pause (`in_maintenance` answers both).
        assert!(!auto_save_allowed(on, true, false));
        // After a failed save a person looks first.
        assert!(!auto_save_allowed(on, false, true));
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
