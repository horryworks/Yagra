// SPDX-License-Identifier: AGPL-3.0-only
//! Local user accounts and roles (Settings ▸ Users & roles).
//!
//! Admin-only: every endpoint here takes [`RequireManageUsers`], so the guard is part of the
//! handler signature rather than a prologue someone can forget. Passwords are write-only — hashed
//! before storage, never logged, never returned.
//!
//! **Session invalidation is the load-bearing part.** Deleting, demoting, disabling or resetting
//! the password of an account must also cut that account's live sessions; otherwise a stolen
//! bearer token outlives the very action taken to stop it. Each handler below revokes explicitly,
//! and the tests pin which mutations revoke and which do not (enabling an account has nothing to
//! revoke).
//!
//! The store also refuses to remove, demote or disable the **last admin** — a lock-out guard that
//! surfaces as `409 last_admin`.

use super::extract::{Admin, RequireManageUsers, RequireView};
use super::util::CreatedId;
use super::{ApiError, ApiResult, ApiState};
use crate::auth::{UserCreateOutcome, UserMutation, UserSummary};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::{delete, get, put},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use yagra_common::{Permission, Role};

/// Minimum password length accepted for a new account or a reset.
const MIN_PASSWORD_LEN: usize = 8;

/// This domain's slice of the OpenAPI document (ADR-035), merged by [`super::openapi::document`].
#[derive(utoipa::OpenApi)]
#[openapi(paths(
    list_users,
    create_user,
    delete_user,
    set_user_role,
    set_user_status,
    set_user_password,
    list_roles
))]
pub(super) struct Doc;

/// The user-administration routes, merged into `/api/v1` by [`super::router`].
pub(super) fn routes() -> Router<ApiState> {
    Router::new()
        .route("/api/v1/users", get(list_users).post(create_user))
        .route("/api/v1/users/:id", delete(delete_user))
        .route("/api/v1/users/:id/role", put(set_user_role))
        .route("/api/v1/users/:id/enabled", put(set_user_status))
        .route("/api/v1/users/:id/password", put(set_user_password))
        .route("/api/v1/roles", get(list_roles))
}

/// Validate a role string against `yagra_common::Role` (snake_case), returning the `400` a bad one
/// deserves. Kept as a parse rather than a bare predicate so no unchecked string reaches the store.
fn checked_role(role: &str) -> ApiResult<&str> {
    if matches!(role, "viewer" | "operator" | "admin") {
        Ok(role)
    } else {
        Err(ApiError::bad_request(
            "invalid_role",
            "role must be viewer, operator, or admin",
        ))
    }
}

/// Reject a password below the minimum length. The value itself is never echoed back.
fn check_password(password: &str) -> ApiResult<()> {
    if password.len() < MIN_PASSWORD_LEN {
        return Err(ApiError::bad_request(
            "weak_password",
            format!("password must be at least {MIN_PASSWORD_LEN} characters"),
        ));
    }
    Ok(())
}

/// Map a store mutation outcome to its response. The three outcomes are identical for every
/// mutating endpoint here, so they resolve in one place: `409 last_admin` is the lock-out guard,
/// and `404` names the missing account.
fn mutation_result(outcome: UserMutation, id: Uuid) -> ApiResult<StatusCode> {
    match outcome {
        UserMutation::Done => Ok(StatusCode::NO_CONTENT),
        UserMutation::NotFound => Err(ApiError::not_found(
            "user_not_found",
            format!("no user {id}"),
        )),
        UserMutation::LastAdmin => Err(ApiError::conflict(
            "last_admin",
            "cannot remove, demote, or disable the last admin account",
        )),
    }
}

/// `GET /api/v1/users` — every local account (never a password hash).
#[utoipa::path(
    get, path = "/api/v1/users", tag = "users",
    responses(
        (status = 200, description = "Every local account, without its password hash", body = Vec<UserSummary>),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role below Admin", body = super::error::ErrorBody),
        (status = 503, description = "This core has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn list_users(_perm: RequireManageUsers, admin: Admin) -> ApiResult<Json<Vec<UserSummary>>> {
    let list =
        admin.users.list().await.map_err(|e| {
            ApiError::from_internal(e.as_ref(), "list users", "failed to list users")
        })?;
    Ok(Json(list))
}

/// Create-user request body. The password is hashed before storage and never logged.
#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct CreateUser {
    username: String,
    password: String,
    role: String,
}

/// `POST /api/v1/users` — create a local account.
#[utoipa::path(
    post, path = "/api/v1/users", tag = "users",
    request_body = CreateUser,
    responses(
        (status = 201, description = "Account created", body = CreatedId),
        (status = 400, description = "Empty username, a password below the minimum length, or an unknown role", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role below Admin", body = super::error::ErrorBody),
        (status = 409, description = "The username is already taken", body = super::error::ErrorBody),
        (status = 503, description = "This core has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn create_user(
    _perm: RequireManageUsers,
    admin: Admin,
    Json(body): Json<CreateUser>,
) -> ApiResult<(StatusCode, Json<CreatedId>)> {
    let username = body.username.trim();
    if username.is_empty() {
        return Err(ApiError::bad_request(
            "invalid_user",
            "username must not be empty",
        ));
    }
    check_password(&body.password)?;
    let role = checked_role(&body.role)?;
    match admin.users.create(username, &body.password, role).await {
        Ok(UserCreateOutcome::Created(id)) => Ok((StatusCode::CREATED, Json(CreatedId { id }))),
        Ok(UserCreateOutcome::UsernameTaken) => Err(ApiError::conflict(
            "username_taken",
            format!("username {username:?} is already taken"),
        )),
        Err(e) => Err(ApiError::from_internal(
            e.as_ref(),
            "create user",
            "failed to create user",
        )),
    }
}

/// `DELETE /api/v1/users/:id` — remove an account and cut any sessions it still holds.
#[utoipa::path(
    delete, path = "/api/v1/users/{id}", tag = "users",
    params(("id" = Uuid, Path, description = "User account id")),
    responses(
        (status = 204, description = "Account removed and its sessions revoked"),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role below Admin", body = super::error::ErrorBody),
        (status = 404, description = "No such account", body = super::error::ErrorBody),
        (status = 409, description = "Refused: this is the last admin account", body = super::error::ErrorBody),
        (status = 503, description = "This core has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn delete_user(
    _perm: RequireManageUsers,
    admin: Admin,
    State(st): State<ApiState>,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    let outcome =
        admin.users.delete(id).await.map_err(|e| {
            ApiError::from_internal(e.as_ref(), "delete user", "failed to delete user")
        })?;
    let status = mutation_result(outcome, id)?;
    // Drop any live sessions the deleted account still holds.
    st.sessions.revoke_user(id);
    Ok(status)
}

/// Change-role request body.
#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct SetRole {
    role: String,
}

/// `PUT /api/v1/users/:id/role` — change an account's role.
#[utoipa::path(
    put, path = "/api/v1/users/{id}/role", tag = "users",
    params(("id" = Uuid, Path, description = "User account id")),
    request_body = SetRole,
    responses(
        (status = 204, description = "Role changed and the account's sessions revoked"),
        (status = 400, description = "Role is not viewer, operator, or admin", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role below Admin", body = super::error::ErrorBody),
        (status = 404, description = "No such account", body = super::error::ErrorBody),
        (status = 409, description = "Refused: this would demote the last admin account", body = super::error::ErrorBody),
        (status = 503, description = "This core has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn set_user_role(
    _perm: RequireManageUsers,
    admin: Admin,
    State(st): State<ApiState>,
    Path(id): Path<Uuid>,
    Json(body): Json<SetRole>,
) -> ApiResult<StatusCode> {
    let role = checked_role(&body.role)?;
    let outcome = admin.users.set_role(id, role).await.map_err(|e| {
        ApiError::from_internal(e.as_ref(), "set user role", "failed to update user role")
    })?;
    let status = mutation_result(outcome, id)?;
    // Force re-login so the new role's permissions take effect immediately (the old principal is
    // cached in the session).
    st.sessions.revoke_user(id);
    Ok(status)
}

/// Enable/disable-account request body.
#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct SetStatus {
    enabled: bool,
}

/// `PUT /api/v1/users/:id/enabled` — enable or disable an account.
#[utoipa::path(
    put, path = "/api/v1/users/{id}/enabled", tag = "users",
    params(("id" = Uuid, Path, description = "User account id")),
    request_body = SetStatus,
    responses(
        (status = 204, description = "Status changed; disabling also revokes the account's sessions"),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role below Admin", body = super::error::ErrorBody),
        (status = 404, description = "No such account", body = super::error::ErrorBody),
        (status = 409, description = "Refused: this would disable the last admin account", body = super::error::ErrorBody),
        (status = 503, description = "This core has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn set_user_status(
    _perm: RequireManageUsers,
    admin: Admin,
    State(st): State<ApiState>,
    Path(id): Path<Uuid>,
    Json(body): Json<SetStatus>,
) -> ApiResult<StatusCode> {
    let outcome = admin
        .users
        .set_enabled(id, body.enabled)
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "set user status",
                "failed to update user status",
            )
        })?;
    let status = mutation_result(outcome, id)?;
    // Disabling an account must also cut its live sessions (the whole point of disabling a
    // compromised account); enabling has nothing to revoke.
    if !body.enabled {
        st.sessions.revoke_user(id);
    }
    Ok(status)
}

/// Reset-password request body. The password is hashed before storage and never logged.
#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct SetPassword {
    password: String,
}

/// `PUT /api/v1/users/:id/password` — reset an account's password.
#[utoipa::path(
    put, path = "/api/v1/users/{id}/password", tag = "users",
    params(("id" = Uuid, Path, description = "User account id")),
    request_body = SetPassword,
    responses(
        (status = 204, description = "Password reset and the account's sessions revoked"),
        (status = 400, description = "Password below the minimum length", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role below Admin", body = super::error::ErrorBody),
        (status = 404, description = "No such account", body = super::error::ErrorBody),
        (status = 503, description = "This core has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn set_user_password(
    _perm: RequireManageUsers,
    admin: Admin,
    State(st): State<ApiState>,
    Path(id): Path<Uuid>,
    Json(body): Json<SetPassword>,
) -> ApiResult<StatusCode> {
    check_password(&body.password)?;
    let changed = admin
        .users
        .set_password(id, &body.password)
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "reset user password",
                "failed to reset password",
            )
        })?;
    if !changed {
        return Err(ApiError::not_found(
            "user_not_found",
            format!("no user {id}"),
        ));
    }
    // A password reset (e.g. after a compromise) must invalidate existing sessions, or the
    // attacker's stolen token survives the reset.
    st.sessions.revoke_user(id);
    Ok(StatusCode::NO_CONTENT)
}

/// One permission in the role/privilege matrix.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct PermissionInfo {
    key: &'static str,
    label: &'static str,
    description: &'static str,
}

/// One role in the matrix: its metadata and the permission keys it grants.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct RoleInfo {
    key: &'static str,
    label: &'static str,
    description: &'static str,
    /// Built-in roles are fixed (custom roles are not configurable yet).
    builtin: bool,
    /// The keys of the permissions this role grants.
    permissions: Vec<&'static str>,
}

/// The permission catalogue plus, for each role, what it grants.
///
/// Only `View`, unlike the rest of this module: it is the *shape* of the permission model, not
/// anyone's account. Derived from `Permission::ALL` and `Role::ALL` rather than listed here, so a
/// new permission appears in the matrix without anyone remembering to add it.
#[utoipa::path(
    get, path = "/api/v1/roles", tag = "users",
    responses(
        (status = 200, description = "The permission catalogue and what each role grants", body = RolesMatrix),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks read access", body = super::error::ErrorBody),
    ),
)]
async fn list_roles(_guard: RequireView) -> ApiResult<Json<RolesMatrix>> {
    let permissions = Permission::ALL
        .into_iter()
        .map(|p| PermissionInfo {
            key: p.key(),
            label: p.label(),
            description: p.description(),
        })
        .collect();
    let roles = Role::ALL
        .into_iter()
        .map(|r| RoleInfo {
            key: r.key(),
            label: r.label(),
            description: r.description(),
            builtin: true,
            permissions: Permission::ALL
                .into_iter()
                .filter(|p| r.grants(*p))
                .map(Permission::key)
                .collect(),
        })
        .collect();
    Ok(Json(RolesMatrix { permissions, roles }))
}

/// The role-vs-privilege matrix.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct RolesMatrix {
    permissions: Vec<PermissionInfo>,
    roles: Vec<RoleInfo>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::router;
    use crate::api::tests_support::{private_state, public_state};
    use axum::body::{to_bytes, Body};
    use axum::http::Request;
    use tower::ServiceExt as _;
    use yagra_common::{Principal, Role, Scope};

    async fn send(
        st: ApiState,
        method: &str,
        uri: &str,
        token: Option<&str>,
        body: &str,
    ) -> (StatusCode, serde_json::Value) {
        let mut req = Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/json");
        if let Some(t) = token {
            req = req.header("authorization", format!("Bearer {t}"));
        }
        let resp = router(st)
            .oneshot(req.body(Body::from(body.to_owned())).unwrap())
            .await
            .unwrap();
        let status = resp.status();
        let bytes = to_bytes(resp.into_body(), 1 << 20).await.unwrap();
        let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        (status, json)
    }

    #[tokio::test]
    async fn user_administration_is_never_open_even_on_a_public_dashboard() {
        // public_dashboard opens *reads*. Listing accounts is not one of them — it is admin data,
        // and a deployment that opens its dashboard must not thereby expose its user list.
        let (status, json) = send(public_state(), "GET", "/api/v1/users", None, "").await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(json["error"]["code"], "unauthorized");
    }

    #[tokio::test]
    async fn a_non_admin_role_is_refused_before_anything_is_read() {
        let st = private_state();
        let token = st.sessions.issue(
            Uuid::new_v4(),
            Principal::new(Role::Operator, Scope::All),
            "op1",
        );
        for (method, uri, body) in [
            ("GET", "/api/v1/users", ""),
            (
                "POST",
                "/api/v1/users",
                r#"{"username":"x","password":"12345678","role":"admin"}"#,
            ),
            (
                "PUT",
                "/api/v1/users/00000000-0000-0000-0000-000000000000/role",
                r#"{"role":"admin"}"#,
            ),
        ] {
            let (status, json) = send(st.clone(), method, uri, Some(&token), body).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{method} {uri}");
            assert_eq!(json["error"]["code"], "forbidden", "{method} {uri}");
        }
    }

    #[tokio::test]
    async fn an_authorized_admin_still_gets_503_in_skeleton_mode() {
        // Guard order: authenticate first, then report availability. An admin learns the write
        // side is absent; an anonymous caller (above) learns only that it is unauthenticated.
        let st = private_state();
        let token = st.sessions.issue(
            Uuid::new_v4(),
            Principal::new(Role::Admin, Scope::All),
            "admin1",
        );
        let (status, json) = send(st, "GET", "/api/v1/users", Some(&token), "").await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(json["error"]["code"], "admin_unavailable");
    }

    #[test]
    fn only_the_three_known_roles_parse() {
        for ok in ["viewer", "operator", "admin"] {
            assert_eq!(checked_role(ok).unwrap(), ok);
        }
        for bad in ["Admin", "root", "", "superuser"] {
            let err = checked_role(bad).unwrap_err();
            assert_eq!(err.status(), StatusCode::BAD_REQUEST);
            assert_eq!(err.code(), "invalid_role");
        }
    }

    #[test]
    fn a_short_password_is_refused_without_echoing_it() {
        assert!(check_password("longenough123").is_ok());
        let err = check_password("short").unwrap_err();
        assert_eq!(err.code(), "weak_password");
        let rendered = format!("{err:?}");
        assert!(!rendered.contains("short"), "{rendered}");
    }

    #[test]
    fn the_last_admin_guard_is_a_conflict_not_a_500() {
        let id = Uuid::new_v4();
        assert_eq!(
            mutation_result(UserMutation::Done, id).unwrap(),
            StatusCode::NO_CONTENT
        );
        let err = mutation_result(UserMutation::LastAdmin, id).unwrap_err();
        assert_eq!(err.status(), StatusCode::CONFLICT);
        assert_eq!(err.code(), "last_admin");
        let err = mutation_result(UserMutation::NotFound, id).unwrap_err();
        assert_eq!(err.status(), StatusCode::NOT_FOUND);
    }
}
