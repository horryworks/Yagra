// SPDX-License-Identifier: AGPL-3.0-only
//! Device profiles — the reusable "what kind of device is this, and how often do we poll it"
//! binding that collection templates attach to.
//!
//! `ManageConfig` throughout, reads included: a profile decides what every node bound to it is
//! polled for.
//!
//! Both editable fields are parsed rather than trusted. The category must be a real
//! [`ProfileCategory`] token — an unknown one would bind nodes to a class the scheduler does not
//! know how to poll — and the interval override must fall inside the configured bounds, because a
//! profile is a multiplier: setting it to one second applies that to every node bound to it.

use super::error::{ApiError, ApiResult};
use super::extract::{Admin, RequireManageConfig};
use super::util::CreatedId;
use super::ApiState;
use axum::{
    extract::Path,
    http::StatusCode,
    routing::{get, put},
    Json, Router,
};
use serde::Deserialize;
use uuid::Uuid;
use yagra_common::ProfileCategory;

/// This domain's slice of the OpenAPI document (ADR-035), merged by [`super::openapi::document`].
#[derive(utoipa::OpenApi)]
#[openapi(paths(list_profiles, create_profile, update_profile, delete_profile))]
pub(super) struct Doc;

/// The profile routes, merged into `/api/v1` by [`super::router`].
pub(super) fn routes() -> Router<ApiState> {
    Router::new()
        .route("/api/v1/profiles", get(list_profiles).post(create_profile))
        .route(
            "/api/v1/profiles/:id",
            put(update_profile).delete(delete_profile),
        )
}

/// Create/update body. `category` is optional on create (defaults to generic SNMP).
#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct ProfileBody {
    name: String,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    vendor: Option<String>,
    /// Per-profile polling-interval override, in seconds. Omitted/`null` ⇒ inherit the global
    /// default; when present it must fall within `[MIN, MAX]`.
    #[serde(default)]
    poll_interval_secs: Option<u32>,
}

/// Validated profile fields ready for the repo. The interval is `i32` for the INTEGER column;
/// `None` ⇒ inherit the global default.
#[derive(Debug)]
struct ParsedProfile {
    name: String,
    category: &'static str,
    vendor: Option<String>,
    poll_interval_secs: Option<i32>,
}

fn parse_profile_body(body: &ProfileBody) -> Result<ParsedProfile, ApiError> {
    let name = body.name.trim();
    if name.is_empty() {
        return Err(ApiError::bad_request(
            "invalid_name",
            "profile name must not be empty",
        ));
    }
    let category = match body.category.as_deref().map(str::trim) {
        None | Some("") => ProfileCategory::default(),
        Some(tok) => ProfileCategory::from_token(tok).ok_or_else(|| {
            ApiError::bad_request(
                "invalid_category",
                format!("unknown profile category {tok:?}"),
            )
        })?,
    };
    let vendor = body
        .vendor
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    let poll_interval_secs = match body.poll_interval_secs {
        None => None,
        Some(n) => {
            // Bounded because a profile is a multiplier: a one-second interval here becomes a
            // one-second interval for every node bound to it.
            if !crate::config::interval_in_bounds(n) {
                return Err(ApiError::bad_request(
                    "invalid_poll_interval",
                    format!(
                        "poll interval must be {}-{} seconds",
                        crate::config::MIN_POLL_INTERVAL_SECS,
                        crate::config::MAX_POLL_INTERVAL_SECS
                    ),
                ));
            }
            Some(n as i32)
        }
    };
    Ok(ParsedProfile {
        name: name.to_owned(),
        category: category.as_str(),
        vendor,
        poll_interval_secs,
    })
}

#[utoipa::path(
    get, path = "/api/v1/profiles", tag = "profiles",
    responses(
        (status = 200, description = "Every device profile", body = Vec<crate::repo::ProfileSummary>),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode has no write side", body = super::error::ErrorBody),
    ),
)]
async fn list_profiles(
    _guard: RequireManageConfig,
    admin: Admin,
) -> ApiResult<Json<Vec<crate::repo::ProfileSummary>>> {
    let list = admin.repo.list_profiles().await.map_err(|e| {
        ApiError::from_internal(e.as_ref(), "list profiles", "failed to list profiles")
    })?;
    Ok(Json(list))
}

#[utoipa::path(
    post, path = "/api/v1/profiles", tag = "profiles",
    request_body = ProfileBody,
    responses(
        (status = 201, description = "Profile created", body = CreatedId),
        (status = 400, description = "Empty name, unknown category, or an out-of-bounds poll interval", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode has no write side", body = super::error::ErrorBody),
    ),
)]
async fn create_profile(
    _guard: RequireManageConfig,
    admin: Admin,
    Json(body): Json<ProfileBody>,
) -> ApiResult<(StatusCode, Json<CreatedId>)> {
    let p = parse_profile_body(&body)?;
    let id = admin
        .repo
        .create_profile(
            &p.name,
            p.category,
            p.vendor.as_deref(),
            p.poll_interval_secs,
        )
        .await
        .map_err(|e| {
            ApiError::from_internal(e.as_ref(), "create profile", "failed to create profile")
        })?;
    Ok((StatusCode::CREATED, Json(CreatedId { id })))
}

#[utoipa::path(
    put, path = "/api/v1/profiles/{id}", tag = "profiles",
    params(("id" = Uuid, Path, description = "Profile id")),
    request_body = ProfileBody,
    responses(
        (status = 204, description = "Profile updated"),
        (status = 400, description = "Empty name, unknown category, or an out-of-bounds poll interval", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
        (status = 404, description = "No such profile", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode has no write side", body = super::error::ErrorBody),
    ),
)]
async fn update_profile(
    _guard: RequireManageConfig,
    admin: Admin,
    Path(id): Path<Uuid>,
    Json(body): Json<ProfileBody>,
) -> ApiResult<StatusCode> {
    let p = parse_profile_body(&body)?;
    let updated = admin
        .repo
        .update_profile(
            id,
            &p.name,
            p.category,
            p.vendor.as_deref(),
            p.poll_interval_secs,
        )
        .await
        .map_err(|e| {
            ApiError::from_internal(e.as_ref(), "update profile", "failed to update profile")
        })?;
    if updated {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found(
            "profile_not_found",
            format!("no profile {id}"),
        ))
    }
}

#[utoipa::path(
    delete, path = "/api/v1/profiles/{id}", tag = "profiles",
    params(("id" = Uuid, Path, description = "Profile id")),
    responses(
        (status = 204, description = "Profile deleted"),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
        (status = 404, description = "No such profile", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode has no write side", body = super::error::ErrorBody),
    ),
)]
async fn delete_profile(
    _guard: RequireManageConfig,
    admin: Admin,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    match admin.repo.delete_profile(id).await {
        Ok(true) => Ok(StatusCode::NO_CONTENT),
        Ok(false) => Err(ApiError::not_found(
            "profile_not_found",
            format!("no profile {id}"),
        )),
        Err(e) => Err(ApiError::from_internal(
            e.as_ref(),
            "delete profile",
            "failed to delete profile",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::router;
    use crate::api::tests_support::{private_state, public_state};
    use axum::body::Body;
    use axum::http::{header::AUTHORIZATION, Request};
    use tower::ServiceExt;
    use yagra_common::{Principal, Role, Scope};

    const ID: &str = "00000000-0000-0000-0000-000000000001";

    fn all_routes() -> Vec<(&'static str, String)> {
        vec![
            ("GET", "/api/v1/profiles".to_owned()),
            ("POST", "/api/v1/profiles".to_owned()),
            ("PUT", format!("/api/v1/profiles/{ID}")),
            ("DELETE", format!("/api/v1/profiles/{ID}")),
        ]
    }

    async fn status_of(st: ApiState, method: &str, path: &str, token: Option<&str>) -> StatusCode {
        let mut b = Request::builder()
            .method(method)
            .uri(path)
            .header("content-type", "application/json");
        if let Some(t) = token {
            b = b.header(AUTHORIZATION, format!("Bearer {t}"));
        }
        router(st)
            .oneshot(b.body(Body::from("{}")).unwrap())
            .await
            .unwrap()
            .status()
    }

    #[tokio::test]
    async fn profiles_are_monitoring_configuration_and_closed_to_a_viewer() {
        for (method, path) in all_routes() {
            assert_eq!(
                status_of(private_state(), method, &path, None).await,
                StatusCode::UNAUTHORIZED,
                "anon {method} {path}"
            );
            assert_eq!(
                status_of(public_state(), method, &path, None).await,
                StatusCode::UNAUTHORIZED,
                "public {method} {path}"
            );
        }
        // A profile is how a class of device is polled — monitoring, so an operator holds it and
        // a viewer does not (ADR-057). 503 = past the guard, into skeleton mode.
        let st = private_state();
        for (role, want) in [
            (Role::Viewer, StatusCode::FORBIDDEN),
            (Role::Operator, StatusCode::SERVICE_UNAVAILABLE),
        ] {
            let token = st
                .sessions
                .issue(Uuid::new_v4(), Principal::new(role, Scope::All), "u");
            for (method, path) in all_routes() {
                assert_eq!(
                    status_of(st.clone(), method, &path, Some(&token)).await,
                    want,
                    "{role:?} {method} {path}"
                );
            }
        }
    }

    #[test]
    fn the_interval_override_is_bounded_because_a_profile_is_a_multiplier() {
        let body = |secs: Option<u32>| ProfileBody {
            name: "Edge switch".to_owned(),
            category: None,
            vendor: None,
            poll_interval_secs: secs,
        };
        // Absent ⇒ inherit, not zero.
        assert_eq!(
            parse_profile_body(&body(None)).unwrap().poll_interval_secs,
            None
        );
        assert!(parse_profile_body(&body(Some(crate::config::MIN_POLL_INTERVAL_SECS))).is_ok());
        assert!(parse_profile_body(&body(Some(crate::config::MAX_POLL_INTERVAL_SECS))).is_ok());
        // One second here is one second for every node bound to the profile.
        assert_eq!(
            parse_profile_body(&body(Some(0))).unwrap_err().code(),
            "invalid_poll_interval"
        );
        assert_eq!(
            parse_profile_body(&body(Some(crate::config::MAX_POLL_INTERVAL_SECS + 1)))
                .unwrap_err()
                .code(),
            "invalid_poll_interval"
        );
    }

    #[test]
    fn an_unknown_category_is_rejected_rather_than_defaulted() {
        // Defaulting would bind nodes to a class the scheduler polls differently from what the
        // operator asked for, silently.
        let bad = ProfileBody {
            name: "x".to_owned(),
            category: Some("not-a-category".to_owned()),
            vendor: None,
            poll_interval_secs: None,
        };
        assert_eq!(
            parse_profile_body(&bad).unwrap_err().code(),
            "invalid_category"
        );

        // Absent or blank *is* a default, deliberately — the create form leaves it empty.
        for c in [None, Some(String::new()), Some("  ".to_owned())] {
            let p = ProfileBody {
                name: "x".to_owned(),
                category: c,
                vendor: None,
                poll_interval_secs: None,
            };
            assert_eq!(
                parse_profile_body(&p).unwrap().category,
                ProfileCategory::default().as_str()
            );
        }
    }
    // ── An accepted write (ADR-115) ──────────────────────────────────────────────────

    /// A profile is created on top of the built-in ones.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn creating_a_profile_adds_one_row(pool: sqlx::PgPool) {
        use crate::api::tests_support::{live_state, send, token};
        let st = live_state(pool.clone()).await;
        let tok = token(&st, yagra_common::Role::Admin);
        let before = crate::pgtest::rows(&pool, "profiles").await;
        assert!(before > 0, "the built-in profiles were not seeded");
        let (status, body) = send(
            &st,
            "POST",
            "/api/v1/profiles",
            &tok,
            Some(serde_json::json!({ "name": "my own switch", "poll_interval_secs": 120 })),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::CREATED, "{body}");
        assert_eq!(crate::pgtest::rows(&pool, "profiles").await, before + 1);
    }
}
