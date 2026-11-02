// SPDX-License-Identifier: AGPL-3.0-only
//! Delivery hours (ADR-226): the named schedules that keep notifications back outside a site's
//! hours, the deployment's default zone, and what is being held right now.
//!
//! The schedules and the default zone are `ManageSystem`, reads included, for the reason routing
//! rules are (ADR-057): a schedule decides when somebody gets woken up. What is being held is
//! `View` and scoped, because it is a fact about alerts the caller can already see.

use super::error::{ApiError, ApiResult};
use super::extract::{Admin, RequireManageSystem, RequireView, Scoped};
use super::util::CreatedId;
use super::ApiState;
use crate::alerts::delivery_hours::{DeliverySchedule, HeldEvent, HeldStore, ScheduleFields};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::{get, put},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// This domain's slice of the OpenAPI document (ADR-035), merged by [`super::openapi::document`].
#[derive(utoipa::OpenApi)]
#[openapi(paths(
    list_delivery_hours,
    create_delivery_hours,
    update_delivery_hours,
    delete_delivery_hours,
    set_default_time_zone,
    list_held_notifications
))]
pub(super) struct Doc;

/// The delivery-hours routes, merged into `/api/v1` by [`super::router`].
pub(super) fn routes() -> Router<ApiState> {
    Router::new()
        .route(
            "/api/v1/delivery-hours",
            get(list_delivery_hours).post(create_delivery_hours),
        )
        // A literal sibling of `/:id`; the router prefers the literal segment.
        .route(
            "/api/v1/delivery-hours/default-time-zone",
            put(set_default_time_zone),
        )
        .route(
            "/api/v1/delivery-hours/:id",
            put(update_delivery_hours).delete(delete_delivery_hours),
        )
        .route("/api/v1/held-notifications", get(list_held_notifications))
}

/// Every schedule, and the zone a node with no zoned folder above it keeps time in.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct DeliveryHoursConfig {
    /// IANA name, or null for UTC.
    pub(crate) default_time_zone: Option<String>,
    pub(crate) schedules: Vec<DeliverySchedule>,
}

/// The read both this route and `get_config(kind=delivery_hours)` answer with, so they cannot
/// disagree.
pub(crate) async fn config(admin: &super::AdminState) -> ApiResult<DeliveryHoursConfig> {
    let schedules = admin.delivery_hours.list().await.map_err(|e| {
        ApiError::from_internal(
            e.as_ref(),
            "list delivery hours",
            "failed to list delivery hours",
        )
    })?;
    let default_time_zone = admin.delivery_hours.default_zone().await.map_err(|e| {
        ApiError::from_internal(
            e.as_ref(),
            "read default time zone",
            "failed to read the default time zone",
        )
    })?;
    Ok(DeliveryHoursConfig {
        default_time_zone,
        schedules,
    })
}

#[utoipa::path(
    get, path = "/api/v1/delivery-hours", tag = "notifications",
    responses(
        (status = 200, description = "Every delivery-hours schedule and the default time zone", body = DeliveryHoursConfig),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageSystem", body = super::error::ErrorBody),
        (status = 503, description = "This core has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn list_delivery_hours(
    _guard: RequireManageSystem,
    admin: Admin,
) -> ApiResult<Json<DeliveryHoursConfig>> {
    Ok(Json(config(&admin).await?))
}

/// Create or replace body. Times are minutes after local midnight; a start later than the end
/// spans midnight, and an equal pair means the whole of each listed day.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub(super) struct ScheduleBody {
    name: String,
    #[serde(default = "enabled_by_default")]
    enabled: bool,
    /// ISO weekdays, 1 = Monday .. 7 = Sunday. At least one.
    days: Vec<u8>,
    start_minute: u16,
    end_minute: u16,
    #[serde(default)]
    group_ids: Vec<Uuid>,
    #[serde(default)]
    labels: Vec<String>,
}

const fn enabled_by_default() -> bool {
    true
}

/// The most labels or folders one schedule may name — far beyond any real use, and a bound on
/// what one request can make every refresh iterate.
const MAX_TARGETS: usize = 200;

impl ScheduleBody {
    /// The checks a schedule passes before it is stored, shared by create and replace.
    fn fields(&self) -> ApiResult<(String, Vec<String>)> {
        let bad = |msg: &str| ApiError::bad_request("invalid_delivery_hours", msg.to_owned());
        let name = self.name.trim();
        if name.is_empty() {
            return Err(bad("name must not be empty"));
        }
        if self.days.is_empty() || self.days.iter().any(|d| !(1..=7).contains(d)) {
            return Err(bad(
                "days must name at least one weekday, 1 (Monday) to 7 (Sunday)",
            ));
        }
        if self.start_minute > 1439 || self.end_minute > 1439 {
            return Err(bad("start_minute and end_minute must be 0..1439"));
        }
        let labels: Vec<String> = self
            .labels
            .iter()
            .map(|l| l.trim().to_owned())
            .filter(|l| !l.is_empty())
            .collect();
        if self.group_ids.is_empty() && labels.is_empty() {
            return Err(bad(
                "name at least one folder or label to apply the hours to",
            ));
        }
        if self.group_ids.len() > MAX_TARGETS || labels.len() > MAX_TARGETS {
            return Err(bad("too many folders or labels"));
        }
        Ok((name.to_owned(), labels))
    }
}

#[utoipa::path(
    post, path = "/api/v1/delivery-hours", tag = "notifications",
    request_body = ScheduleBody,
    responses(
        (status = 201, description = "Schedule created", body = CreatedId),
        (status = 400, description = "Empty name, no weekday, a time outside the day, or no folder or label", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageSystem", body = super::error::ErrorBody),
        (status = 503, description = "This core has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn create_delivery_hours(
    _guard: RequireManageSystem,
    admin: Admin,
    Json(body): Json<ScheduleBody>,
) -> ApiResult<(StatusCode, Json<CreatedId>)> {
    let (name, labels) = body.fields()?;
    let id = admin
        .delivery_hours
        .create(&ScheduleFields {
            name: &name,
            enabled: body.enabled,
            days: &body.days,
            start_minute: body.start_minute,
            end_minute: body.end_minute,
            group_ids: &body.group_ids,
            labels: &labels,
        })
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "create delivery hours",
                "failed to create the schedule",
            )
        })?;
    Ok((StatusCode::CREATED, Json(CreatedId { id })))
}

#[utoipa::path(
    put, path = "/api/v1/delivery-hours/{id}", tag = "notifications",
    params(("id" = Uuid, Path, description = "Schedule id")),
    request_body = ScheduleBody,
    responses(
        (status = 204, description = "Schedule replaced"),
        (status = 400, description = "Empty name, no weekday, a time outside the day, or no folder or label", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageSystem", body = super::error::ErrorBody),
        (status = 404, description = "No such schedule", body = super::error::ErrorBody),
        (status = 503, description = "This core has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn update_delivery_hours(
    _guard: RequireManageSystem,
    admin: Admin,
    Path(id): Path<Uuid>,
    Json(body): Json<ScheduleBody>,
) -> ApiResult<StatusCode> {
    let (name, labels) = body.fields()?;
    let fields = ScheduleFields {
        name: &name,
        enabled: body.enabled,
        days: &body.days,
        start_minute: body.start_minute,
        end_minute: body.end_minute,
        group_ids: &body.group_ids,
        labels: &labels,
    };
    match admin.delivery_hours.update(id, &fields).await {
        Ok(true) => Ok(StatusCode::NO_CONTENT),
        Ok(false) => Err(ApiError::not_found(
            "delivery_hours_not_found",
            format!("no schedule {id}"),
        )),
        Err(e) => Err(ApiError::from_internal(
            e.as_ref(),
            "update delivery hours",
            "failed to update the schedule",
        )),
    }
}

#[utoipa::path(
    delete, path = "/api/v1/delivery-hours/{id}", tag = "notifications",
    params(("id" = Uuid, Path, description = "Schedule id")),
    responses(
        (status = 204, description = "Schedule deleted; what it was holding goes out on the next pass"),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageSystem", body = super::error::ErrorBody),
        (status = 404, description = "No such schedule", body = super::error::ErrorBody),
        (status = 503, description = "This core has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn delete_delivery_hours(
    _guard: RequireManageSystem,
    admin: Admin,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    match admin.delivery_hours.delete(id).await {
        Ok(true) => Ok(StatusCode::NO_CONTENT),
        Ok(false) => Err(ApiError::not_found(
            "delivery_hours_not_found",
            format!("no schedule {id}"),
        )),
        Err(e) => Err(ApiError::from_internal(
            e.as_ref(),
            "delete delivery hours",
            "failed to delete the schedule",
        )),
    }
}

/// Set-zone body: an IANA name, or null (or empty) for UTC.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub(super) struct TimeZoneBody {
    time_zone: Option<String>,
}

#[utoipa::path(
    put, path = "/api/v1/delivery-hours/default-time-zone", tag = "notifications",
    request_body = TimeZoneBody,
    responses(
        (status = 204, description = "Default time zone set"),
        (status = 400, description = "Not an IANA zone name", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageSystem", body = super::error::ErrorBody),
        (status = 503, description = "This core has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn set_default_time_zone(
    _guard: RequireManageSystem,
    admin: Admin,
    Json(body): Json<TimeZoneBody>,
) -> ApiResult<StatusCode> {
    super::util::parse_time_zone(body.time_zone.as_deref())?;
    let zone = super::util::stored_time_zone(body.time_zone.as_deref());
    admin
        .delivery_hours
        .set_default_zone(zone.as_deref())
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "set default time zone",
                "failed to set the default time zone",
            )
        })?;
    Ok(StatusCode::NO_CONTENT)
}

/// One notification held because its node is outside its delivery hours.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub(crate) struct HeldNotification {
    /// The node the alert is about.
    pub(crate) node_id: Option<Uuid>,
    /// The alert's check id - with `node_id`, what an active-alert row is matched by.
    pub(crate) check: Uuid,
    pub(crate) metric: String,
    pub(crate) severity: yagra_common::Severity,
    pub(crate) event: HeldEvent,
    pub(crate) held_at: chrono::DateTime<chrono::Utc>,
    /// When the node's hours were next due to open, as computed when it was held. A schedule
    /// edited since moves the real release without moving this.
    pub(crate) release_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// What is being held that `scope` may see, oldest first. Shared with `list_suppressions`.
pub(crate) async fn visible_held(
    st: &ApiState,
    scope: &super::scope::NodeScope,
    admin: &super::AdminState,
) -> ApiResult<Vec<HeldNotification>> {
    let rows = HeldStore::list(admin.delivery_hours.as_ref())
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "list held notifications",
                "failed to list held notifications",
            )
        })?;
    Ok(rows
        .into_iter()
        .filter(|r| scope.allows_subject(st, &r.alert.subject))
        .map(|r| HeldNotification {
            node_id: r.alert.node().map(|n| n.as_uuid()),
            check: r.alert.check.as_uuid(),
            metric: r.alert.metric.clone(),
            severity: r.alert.severity,
            event: r.event,
            held_at: r.held_at,
            release_at: r.release_at,
        })
        .collect())
}

#[utoipa::path(
    get, path = "/api/v1/held-notifications", tag = "notifications",
    responses(
        (status = 200, description = "Notifications held until their node's delivery hours open, oldest first", body = Vec<HeldNotification>),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks the View permission", body = super::error::ErrorBody),
        (status = 503, description = "This core has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn list_held_notifications(
    _perm: RequireView,
    Scoped(scope): Scoped,
    State(st): State<ApiState>,
    admin: Admin,
) -> ApiResult<Json<Vec<HeldNotification>>> {
    Ok(Json(visible_held(&st, &scope, &admin).await?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::tests_support::{live_state, private_state, send, status_of, token};
    use axum::http::StatusCode;
    use yagra_common::Role;

    fn body(days: &[u8], labels: &[&str]) -> ScheduleBody {
        ScheduleBody {
            name: "office".to_owned(),
            enabled: true,
            days: days.to_vec(),
            start_minute: 510,
            end_minute: 1040,
            group_ids: Vec::new(),
            labels: labels.iter().map(|l| (*l).to_owned()).collect(),
        }
    }

    #[test]
    fn a_schedule_must_name_a_day_a_time_in_the_day_and_a_target() {
        assert!(body(&[1, 2, 3, 4, 5], &["office"]).fields().is_ok());
        assert!(
            body(&[], &["office"]).fields().is_err(),
            "no weekday holds forever"
        );
        assert!(body(&[8], &["office"]).fields().is_err());
        assert!(
            body(&[1], &[]).fields().is_err(),
            "a schedule naming nothing does nothing"
        );
        assert!(
            body(&[1], &["  "]).fields().is_err(),
            "a blank label names nothing"
        );
        let mut late = body(&[1], &["office"]);
        late.end_minute = 1440;
        assert!(late.fields().is_err());
    }

    #[tokio::test]
    async fn the_schedules_are_closed_to_anyone_below_admin() {
        let st = private_state();
        assert_eq!(
            status_of(st.clone(), "GET", "/api/v1/delivery-hours", None).await,
            StatusCode::UNAUTHORIZED
        );
        let viewer = token(&st, Role::Viewer);
        assert_eq!(
            status_of(st, "GET", "/api/v1/delivery-hours", Some(&viewer)).await,
            StatusCode::FORBIDDEN
        );
    }

    #[sqlx::test(migrator = "yagra_base::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn an_admin_creates_edits_and_deletes_a_schedule(pool: sqlx::PgPool) {
        let st = live_state(pool).await;
        let admin = token(&st, Role::Admin);
        let (status, created) = send(
            &st,
            "POST",
            "/api/v1/delivery-hours",
            &admin,
            Some(serde_json::json!({
                "name": "office", "days": [1, 2, 3, 4, 5],
                "start_minute": 510, "end_minute": 1040, "labels": ["office"]
            })),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let id = created["id"].as_str().unwrap().to_owned();

        let (status, _) = send(
            &st,
            "PUT",
            &format!("/api/v1/delivery-hours/{id}"),
            &admin,
            Some(serde_json::json!({
                "name": "office", "enabled": false, "days": [1],
                "start_minute": 0, "end_minute": 0, "labels": ["office"]
            })),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        let (status, _) = send(
            &st,
            "PUT",
            "/api/v1/delivery-hours/default-time-zone",
            &admin,
            Some(serde_json::json!({ "time_zone": "Asia/Tokyo" })),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        let (status, read) = send(&st, "GET", "/api/v1/delivery-hours", &admin, None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(read["default_time_zone"], "Asia/Tokyo");
        assert_eq!(read["schedules"][0]["enabled"], false);
        assert_eq!(read["schedules"][0]["days"], serde_json::json!([1]));

        let (status, _) = send(
            &st,
            "DELETE",
            &format!("/api/v1/delivery-hours/{id}"),
            &admin,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (_, read) = send(&st, "GET", "/api/v1/delivery-hours", &admin, None).await;
        assert_eq!(read["schedules"], serde_json::json!([]));
    }

    /// A folder's zone is accepted from a person, refused while NetBox sets it, and out of reach
    /// of a caller scoped to other folders (ADR-226 decisions 5 and 6).
    #[sqlx::test(migrator = "yagra_base::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_folder_zone_is_set_by_a_person_unless_netbox_sets_it(pool: sqlx::PgPool) {
        let st = live_state(pool.clone()).await;
        let admin = token(&st, Role::Admin);
        let mut ids = Vec::new();
        for name in ["mine", "theirs"] {
            let (status, created) = send(
                &st,
                "POST",
                "/api/v1/node-groups",
                &admin,
                Some(serde_json::json!({ "name": name, "group_type": "site" })),
            )
            .await;
            assert_eq!(status, StatusCode::CREATED, "{created}");
            ids.push(created["id"].as_str().unwrap().to_owned());
        }
        let zone = |z: &str| Some(serde_json::json!({ "time_zone": z }));
        let path = |id: &str| format!("/api/v1/node-groups/{id}/time-zone");

        let (status, _) = send(&st, "PUT", &path(&ids[0]), &admin, zone("Asia/Tokyo")).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (_, groups) = send(&st, "GET", "/api/v1/node-groups", &admin, None).await;
        let mine = groups
            .as_array()
            .unwrap()
            .iter()
            .find(|g| g["id"] == ids[0].as_str())
            .unwrap();
        assert_eq!(mine["time_zone"], "Asia/Tokyo");
        assert_eq!(mine["effective_time_zone"], "Asia/Tokyo");

        let scoped = crate::api::tests_support::scoped_token(
            &st,
            &[uuid::Uuid::parse_str(&ids[0]).unwrap()],
        );
        let (status, _) = send(&st, "PUT", &path(&ids[1]), &scoped, zone("Asia/Tokyo")).await;
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "a folder outside the caller's scope"
        );

        sqlx::query("UPDATE node_groups SET netbox_time_zone = 'Europe/London' WHERE id = $1")
            .bind(uuid::Uuid::parse_str(&ids[1]).unwrap())
            .execute(&pool)
            .await
            .unwrap();
        let (status, body) = send(&st, "PUT", &path(&ids[1]), &admin, zone("Asia/Tokyo")).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["error"]["code"], "time_zone_from_netbox", "{body}");
    }

    #[sqlx::test(migrator = "yagra_base::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn an_unknown_zone_is_refused(pool: sqlx::PgPool) {
        let st = live_state(pool).await;
        let admin = token(&st, Role::Admin);
        let (status, _) = send(
            &st,
            "PUT",
            "/api/v1/delivery-hours/default-time-zone",
            &admin,
            Some(serde_json::json!({ "time_zone": "Mars/Olympus" })),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
}
