// SPDX-License-Identifier: AGPL-3.0-only
//! How strongly an outage's alerts are grouped (ADR-236): the deployment's level, its exceptions
//! (by folder and tag, the weakest reaching a node wins - ADR-262), and what each level would do
//! to the outage open now.
//!
//! `ManageSystem`, reads included, for the reason routing rules and delivery hours are (ADR-057):
//! a level decides whose phone rings when a site goes dark.

use super::error::{ApiError, ApiResult};
use super::extract::{Admin, RequireManageSystem};
use super::ApiState;
use crate::alerts::grouping::GroupingRepo;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post, put},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use yagra_topology::grouping::Level;

/// This domain's slice of the OpenAPI document (ADR-035), merged by [`super::openapi::document`].
#[derive(utoipa::OpenApi)]
#[openapi(paths(
    get_alert_grouping,
    set_alert_grouping_level,
    set_alert_grouping_wait,
    create_grouping_exception,
    update_grouping_exception,
    delete_grouping_exception
))]
pub(super) struct Doc;

/// The grouping routes, merged into `/api/v1` by [`super::router`].
pub(super) fn routes() -> Router<ApiState> {
    Router::new()
        .route("/api/v1/alert-grouping", get(get_alert_grouping))
        .route(
            "/api/v1/alert-grouping/level",
            put(set_alert_grouping_level),
        )
        .route(
            "/api/v1/alert-grouping/exceptions",
            post(create_grouping_exception),
        )
        .route(
            "/api/v1/alert-grouping/exceptions/:id",
            put(update_grouping_exception).delete(delete_grouping_exception),
        )
        .route("/api/v1/alert-grouping/wait", put(set_alert_grouping_wait))
}

/// How strongly alerts are grouped, weakest first (ADR-236 decision 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GroupingLevel {
    /// Never grouped.
    Off,
    /// Only on CDP/LLDP links, and only for a node every one of whose neighbour rows was matched.
    Certain,
    /// CDP/LLDP links where a node has any, otherwise shared-subnet and routing links.
    Links,
    /// As `links`, and every node down in the same Site folder joins the outage.
    Site,
}

impl From<GroupingLevel> for Level {
    fn from(l: GroupingLevel) -> Self {
        match l {
            GroupingLevel::Off => Level::Off,
            GroupingLevel::Certain => Level::Certain,
            GroupingLevel::Links => Level::Links,
            GroupingLevel::Site => Level::Site,
        }
    }
}

impl From<Level> for GroupingLevel {
    fn from(l: Level) -> Self {
        match l {
            Level::Off => GroupingLevel::Off,
            Level::Certain => GroupingLevel::Certain,
            Level::Links => GroupingLevel::Links,
            Level::Site => GroupingLevel::Site,
        }
    }
}

/// One exception to the deployment's level (ADR-262 decision 2): every node inside one of
/// `group_ids` (at any depth) or carrying one of `tags` groups at `level`. When several reach one
/// node, the weakest level wins.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct GroupingException {
    pub(crate) id: Uuid,
    pub(crate) group_ids: Vec<Uuid>,
    pub(crate) tags: Vec<String>,
    pub(crate) level: GroupingLevel,
}

/// What an exception applies to and its level. Names at least one folder or tag.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub(crate) struct ExceptionBody {
    #[serde(default)]
    pub(crate) group_ids: Vec<Uuid>,
    #[serde(default)]
    pub(crate) tags: Vec<String>,
    pub(crate) level: GroupingLevel,
}

/// The id of a created exception.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct CreatedException {
    pub(crate) id: Uuid,
}

/// What would happen to the outage open now.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct GroupingPreview {
    /// Nodes down now.
    pub(crate) down: usize,
    /// How many of them would page on their own with the deployment at each level, weakest first
    /// (`off`, `certain`, `links`, `site`), every exception keeping its own level.
    pub(crate) notified: Vec<usize>,
}

/// What one level would have done to the last seven days' fires.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct GroupingReplayLevel {
    pub(crate) level: GroupingLevel,
    /// Fires that would have paged, each sent the moment it committed.
    pub(crate) notified: usize,
    /// Fires that would have paged waiting for a failing neighbour to commit first.
    pub(crate) notified_waiting: usize,
    /// Of the fires that waited and then paged, the median wait in seconds.
    pub(crate) delay_median_secs: Option<i64>,
    /// Of the fires that waited and then paged, the longest wait in seconds.
    pub(crate) delay_max_secs: Option<i64>,
}

/// The last seven days' node-down fires replayed against today's links and levels (ADR-236
/// decision 13). An approximation: the links are today's, and the history keeps commit times only.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct GroupingReplay {
    /// Node-down fires recorded in the last seven days.
    pub(crate) fired: usize,
    /// More history was recorded than is read, so the newest fires are missing.
    pub(crate) truncated: bool,
    /// One per level, weakest first (`off`, `certain`, `links`, `site`).
    pub(crate) levels: Vec<GroupingReplayLevel>,
}

/// The deployment's level, its exceptions, and the preview.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct AlertGroupingConfig {
    pub(crate) level: GroupingLevel,
    /// Whether a border's notification waits for a failing neighbour to commit (ADR-236 Inc.2).
    pub(crate) wait: bool,
    /// Oldest first.
    pub(crate) exceptions: Vec<GroupingException>,
    pub(crate) now: GroupingPreview,
    /// `null` when the history could not be read; the rest of the answer stands.
    pub(crate) last_7_days: Option<GroupingReplay>,
}

fn replay_dto(r: &crate::alerts::grouping_replay::Replayed) -> GroupingReplay {
    let secs = |ms: Option<i64>| ms.map(|m| m / 1_000);
    GroupingReplay {
        fired: r.summary.fired,
        truncated: r.truncated,
        levels: r
            .summary
            .levels
            .iter()
            .map(|l| GroupingReplayLevel {
                level: l.level.into(),
                notified: l.notified,
                notified_waiting: l.notified_waiting,
                delay_median_secs: secs(l.delay_median_ms),
                delay_max_secs: secs(l.delay_max_ms),
            })
            .collect(),
    }
}

fn repo(admin: &super::AdminState) -> GroupingRepo {
    GroupingRepo::new(admin.repo.pool())
}

fn internal(what: &'static str) -> impl FnOnce(anyhow::Error) -> ApiError {
    move |e| ApiError::from_internal(e.as_ref(), what, "failed to read or write alert grouping")
}

/// The read both this route and `get_config(kind=alert_grouping)` answer with, so they cannot
/// disagree.
pub(crate) async fn config(
    alerts: &crate::alerts::AlertManager,
    admin: &super::AdminState,
) -> ApiResult<AlertGroupingConfig> {
    let repo = repo(admin);
    let level = repo
        .level()
        .await
        .map_err(internal("read grouping level"))?;
    let wait = repo.wait().await.map_err(internal("read grouping wait"))?;
    let exceptions: Vec<GroupingException> = repo
        .exceptions()
        .await
        .map_err(internal("read grouping exceptions"))?
        .into_iter()
        .map(|e| GroupingException {
            id: e.id,
            group_ids: e.group_ids,
            tags: e.tags,
            level: e.level.into(),
        })
        .collect();
    let (down, notified) = alerts.grouping_preview();
    let history = crate::alerts::history::AlertHistoryStore::new(admin.repo.pool());
    let last_7_days = match crate::alerts::grouping_replay::last_7_days(alerts, &history).await {
        Ok(r) => Some(replay_dto(&r)),
        Err(e) => {
            tracing::warn!(error = %e, "replaying the last seven days of node-down alerts failed");
            None
        }
    };
    Ok(AlertGroupingConfig {
        level: level.into(),
        wait,
        last_7_days,
        exceptions,
        now: GroupingPreview {
            down,
            notified: notified.to_vec(),
        },
    })
}

#[utoipa::path(
    get, path = "/api/v1/alert-grouping", tag = "notifications",
    responses(
        (status = 200, description = "The deployment's grouping level, its exceptions, and what each level would do to the outage open now", body = AlertGroupingConfig),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageSystem", body = super::error::ErrorBody),
        (status = 503, description = "This core has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn get_alert_grouping(
    _guard: RequireManageSystem,
    State(st): State<ApiState>,
    admin: Admin,
) -> ApiResult<Json<AlertGroupingConfig>> {
    Ok(Json(config(&st.alerts, &admin).await?))
}

/// A level to set.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub(super) struct LevelBody {
    level: GroupingLevel,
}

#[utoipa::path(
    put, path = "/api/v1/alert-grouping/level", tag = "notifications",
    request_body = LevelBody,
    responses(
        (status = 204, description = "The deployment's grouping level is set"),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageSystem", body = super::error::ErrorBody),
        (status = 422, description = "Not a grouping level (the body framework's own rejection, plain text)"),
        (status = 503, description = "This core has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn set_alert_grouping_level(
    _guard: RequireManageSystem,
    admin: Admin,
    Json(body): Json<LevelBody>,
) -> ApiResult<StatusCode> {
    repo(&admin)
        .set_level(body.level.into())
        .await
        .map_err(internal("set grouping level"))?;
    Ok(StatusCode::NO_CONTENT)
}

/// Whether to wait.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub(super) struct WaitBody {
    wait: bool,
}

#[utoipa::path(
    put, path = "/api/v1/alert-grouping/wait", tag = "notifications",
    request_body = WaitBody,
    responses(
        (status = 204, description = "Whether a border's notification waits for a failing neighbour to commit is set"),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageSystem", body = super::error::ErrorBody),
        (status = 422, description = "Not a boolean (the body framework's own rejection, plain text)"),
        (status = 503, description = "This core has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn set_alert_grouping_wait(
    _guard: RequireManageSystem,
    admin: Admin,
    Json(body): Json<WaitBody>,
) -> ApiResult<StatusCode> {
    repo(&admin)
        .set_wait(body.wait)
        .await
        .map_err(internal("set grouping wait"))?;
    Ok(StatusCode::NO_CONTENT)
}

/// Check an exception's body: at least one folder or tag, each tag a valid label (the rule a
/// routing rule's tags pass, ADR-259 decision 3).
fn checked(body: ExceptionBody) -> ApiResult<(Vec<Uuid>, Vec<String>, Level)> {
    let tags = super::nodes::validated_labels(body.tags)?;
    if body.group_ids.is_empty() && tags.is_empty() {
        return Err(ApiError::bad_request(
            "empty_target",
            "an exception names at least one folder or tag",
        ));
    }
    Ok((body.group_ids, tags, body.level.into()))
}

#[utoipa::path(
    post, path = "/api/v1/alert-grouping/exceptions", tag = "notifications",
    request_body = ExceptionBody,
    responses(
        (status = 201, description = "The exception is added", body = CreatedException),
        (status = 400, description = "Names no folder or tag, or a tag is not a valid label", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageSystem", body = super::error::ErrorBody),
        (status = 422, description = "Not a grouping level (the body framework's own rejection, plain text)"),
        (status = 503, description = "This core has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn create_grouping_exception(
    _guard: RequireManageSystem,
    admin: Admin,
    Json(body): Json<ExceptionBody>,
) -> ApiResult<(StatusCode, Json<CreatedException>)> {
    let (group_ids, tags, level) = checked(body)?;
    let id = repo(&admin)
        .create_exception(&group_ids, &tags, level)
        .await
        .map_err(internal("create grouping exception"))?;
    Ok((StatusCode::CREATED, Json(CreatedException { id })))
}

#[utoipa::path(
    put, path = "/api/v1/alert-grouping/exceptions/{id}", tag = "notifications",
    params(("id" = Uuid, Path, description = "The exception")),
    request_body = ExceptionBody,
    responses(
        (status = 204, description = "The exception is replaced"),
        (status = 400, description = "Names no folder or tag, or a tag is not a valid label", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageSystem", body = super::error::ErrorBody),
        (status = 404, description = "No such exception", body = super::error::ErrorBody),
        (status = 422, description = "Not a grouping level (the body framework's own rejection, plain text)"),
        (status = 503, description = "This core has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn update_grouping_exception(
    _guard: RequireManageSystem,
    admin: Admin,
    Path(id): Path<Uuid>,
    Json(body): Json<ExceptionBody>,
) -> ApiResult<StatusCode> {
    let (group_ids, tags, level) = checked(body)?;
    let found = repo(&admin)
        .update_exception(id, &group_ids, &tags, level)
        .await
        .map_err(internal("update grouping exception"))?;
    if !found {
        return Err(ApiError::not_found("not_found", "exception not found"));
    }
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    delete, path = "/api/v1/alert-grouping/exceptions/{id}", tag = "notifications",
    params(("id" = Uuid, Path, description = "The exception")),
    responses(
        (status = 204, description = "The exception is removed; the nodes it reached follow the deployment's level or another exception"),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageSystem", body = super::error::ErrorBody),
        (status = 404, description = "No such exception", body = super::error::ErrorBody),
        (status = 503, description = "This core has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn delete_grouping_exception(
    _guard: RequireManageSystem,
    admin: Admin,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    let found = repo(&admin)
        .delete_exception(id)
        .await
        .map_err(internal("delete grouping exception"))?;
    if !found {
        return Err(ApiError::not_found("not_found", "exception not found"));
    }
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::super::tests_support::*;
    use super::*;
    use yagra_common::Role;

    #[test]
    fn the_api_levels_are_the_engines_levels_in_order() {
        let api = [
            GroupingLevel::Off,
            GroupingLevel::Certain,
            GroupingLevel::Links,
            GroupingLevel::Site,
        ];
        for (api, engine) in api.iter().zip(Level::ALL) {
            assert_eq!(Level::from(*api), engine);
            assert_eq!(GroupingLevel::from(engine), *api);
        }
    }

    #[tokio::test]
    async fn grouping_is_closed_to_anyone_below_admin() {
        let st = private_state();
        assert_eq!(
            status_of(st.clone(), "GET", "/api/v1/alert-grouping", None).await,
            StatusCode::UNAUTHORIZED
        );
        let viewer = token(&st, Role::Viewer);
        assert_eq!(
            status_of(st.clone(), "GET", "/api/v1/alert-grouping", Some(&viewer)).await,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            status_of(st, "PUT", "/api/v1/alert-grouping/wait", Some(&viewer)).await,
            StatusCode::FORBIDDEN
        );
    }

    #[sqlx::test(migrator = "yagra_base::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn an_admin_sets_the_level_and_an_exception_and_takes_it_away(pool: sqlx::PgPool) {
        let st = live_state(pool.clone()).await;
        let admin = token(&st, Role::Admin);
        let (status, read) = send(&st, "GET", "/api/v1/alert-grouping", &admin, None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(read["level"], "off");
        assert_eq!(read["wait"], false);
        assert_eq!(read["now"]["notified"].as_array().map(Vec::len), Some(4));
        assert_eq!(read["last_7_days"]["fired"], 0);
        assert_eq!(read["last_7_days"]["truncated"], false);
        assert_eq!(
            read["last_7_days"]["levels"].as_array().map(Vec::len),
            Some(4)
        );

        let (status, _) = send(
            &st,
            "PUT",
            "/api/v1/alert-grouping/wait",
            &admin,
            Some(serde_json::json!({ "wait": true })),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (_, read) = send(&st, "GET", "/api/v1/alert-grouping", &admin, None).await;
        assert_eq!(read["wait"], true);

        let (status, _) = send(
            &st,
            "PUT",
            "/api/v1/alert-grouping/level",
            &admin,
            Some(serde_json::json!({ "level": "certain" })),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        let site = yagra_base::pgtest::group(&pool, "Site A").await;
        let (status, created) = send(
            &st,
            "POST",
            "/api/v1/alert-grouping/exceptions",
            &admin,
            Some(serde_json::json!({ "group_ids": [site], "tags": ["no-lldp"], "level": "site" })),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let id = created["id"].as_str().expect("id").to_owned();

        let (_, read) = send(&st, "GET", "/api/v1/alert-grouping", &admin, None).await;
        assert_eq!(read["level"], "certain");
        assert_eq!(read["exceptions"][0]["group_ids"][0], site.to_string());
        assert_eq!(read["exceptions"][0]["tags"][0], "no-lldp");
        assert_eq!(read["exceptions"][0]["level"], "site");

        let path = format!("/api/v1/alert-grouping/exceptions/{id}");
        let (status, _) = send(
            &st,
            "PUT",
            &path,
            &admin,
            Some(serde_json::json!({ "tags": ["no-lldp"], "level": "off" })),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (_, read) = send(&st, "GET", "/api/v1/alert-grouping", &admin, None).await;
        assert_eq!(read["exceptions"][0]["level"], "off");
        assert_eq!(
            read["exceptions"][0]["group_ids"].as_array().map(Vec::len),
            Some(0)
        );

        // An exception naming nothing would be the deployment's level under another name.
        let (status, body) = send(
            &st,
            "PUT",
            &path,
            &admin,
            Some(serde_json::json!({ "level": "off" })),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["code"], "empty_target", "{body}");

        let (status, _) = send(&st, "DELETE", &path, &admin, None).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (status, _) = send(&st, "DELETE", &path, &admin, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }
}
