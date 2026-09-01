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
use std::collections::HashSet;
use uuid::Uuid;
use yagra_common::{Permission, Role, Scope, UserKind};

/// Minimum password length accepted for a new account or a reset.
const MIN_PASSWORD_LEN: usize = 8;

/// This domain's slice of the OpenAPI document (ADR-035), merged by [`super::openapi::document`].
#[derive(utoipa::OpenApi)]
#[openapi(paths(
    list_users,
    create_user,
    delete_user,
    set_user_role,
    set_user_scope,
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
        .route("/api/v1/users/:id/scope", put(set_user_scope))
        .route("/api/v1/users/:id/enabled", put(set_user_status))
        .route("/api/v1/users/:id/password", put(set_user_password))
        .route("/api/v1/roles", get(list_roles))
}

/// Validate a role string against `yagra_common::Role` (snake_case), returning the `400` a bad one
/// deserves. Kept as a parse rather than a bare predicate so no unchecked string reaches the store.
fn checked_role(role: &str) -> ApiResult<&str> {
    if Role::parse(role).is_some() {
        Ok(role)
    } else {
        let allowed = Role::ALL.map(Role::key).join(", ");
        Err(ApiError::bad_request(
            "invalid_role",
            format!("role must be one of: {allowed}"),
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
        UserMutation::AdminIsUnscoped => Err(ApiError::conflict(
            "admin_is_unscoped",
            "an admin account cannot be limited to groups — its permissions are fleet-wide; \
             change the role first",
        )),
        UserMutation::NotLocal => Err(ApiError::bad_request(
            "not_a_local_account",
            "this account signs in through an identity provider or a directory, so it has no \
             password here — an administrator has to change it at the source",
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
    /// Required for a `local` account, and rejected for a `service` one — a machine account has no
    /// password by design, so accepting a discarded one would advertise a login that does not exist.
    #[serde(default)]
    password: Option<String>,
    role: String,
    /// What kind of account this is. Defaults to `local`, so a client written before service
    /// accounts existed keeps creating exactly what it did before.
    ///
    /// `oidc` is not accepted: those accounts are provisioned by signing in through the IdP, never
    /// by hand — creating one here would produce an account whose subject matches nobody.
    #[serde(default)]
    kind: Option<UserKind>,
}

/// `POST /api/v1/users` — create a local or service account.
#[utoipa::path(
    post, path = "/api/v1/users", tag = "users",
    request_body = CreateUser,
    responses(
        (status = 201, description = "Account created", body = CreatedId),
        (status = 400, description = "Empty username, an unknown role, a password that is missing/too short for a local account or supplied for a service one, or `kind: oidc`", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role below Admin", body = super::error::ErrorBody),
        (status = 409, description = "The username is already taken", body = super::error::ErrorBody),
        (status = 503, description = "This core has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
/// Create an account.
///
/// A **service account** (`kind: "service"`) is a machine identity: no password, and no way to sign
/// in through either the local form or SSO. It exists to own API tokens, so an unattended
/// integration keeps working when the person who set it up changes teams — and so that disabling it
/// stops every credential it owns at once.
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
    let role = checked_role(&body.role)?;
    let kind = body.kind.unwrap_or(UserKind::Local);
    let created = match kind {
        UserKind::Local => {
            let Some(password) = body.password.as_deref() else {
                return Err(ApiError::bad_request(
                    "password_required",
                    "a local account needs a password",
                ));
            };
            check_password(password)?;
            admin.users.create(username, password, role).await
        }
        UserKind::Service => {
            // Refuse rather than ignore a supplied password. Silently dropping it would leave the
            // admin believing they had set one, and later believing the account could sign in.
            if body.password.is_some() {
                return Err(ApiError::bad_request(
                    "password_not_allowed",
                    "a service account cannot sign in, so it takes no password",
                ));
            }
            admin.users.create_service(username, role).await
        }
        // Every externally-backed kind is provisioned by signing in, never by hand: the account has
        // to carry the directory's own identifier for it, and only a successful login reveals that.
        // Creating a shell here would produce a row that can never be matched to the person.
        UserKind::Oidc | UserKind::Ldap => {
            return Err(ApiError::bad_request(
                "kind_not_creatable",
                "an SSO or directory account is provisioned by signing in through it",
            ))
        }
    };
    match created {
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

/// `DELETE /api/v1/users/:id` — remove an account and cut every credential it still holds.
#[utoipa::path(
    delete, path = "/api/v1/users/{id}", tag = "users",
    params(("id" = Uuid, Path, description = "User account id")),
    responses(
        (status = 204, description = "Account removed, its sessions revoked and its API tokens revoked"),
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
    // Revoke the account's API tokens *before* deleting it, while the owner column still points at
    // it. The FK is `ON DELETE SET NULL` so the rows survive as an audit record, and verification
    // refuses an owner-less token anyway — but leaving them un-revoked would show a departed
    // account's credentials as "active" in the listing, which is the state this whole change exists
    // to make impossible to overlook.
    revoke_tokens_of(&admin, id).await;
    let outcome =
        admin.users.delete(id).await.map_err(|e| {
            ApiError::from_internal(e.as_ref(), "delete user", "failed to delete user")
        })?;
    let status = mutation_result(outcome, id)?;
    // Drop any live sessions the deleted account still holds.
    st.sessions.revoke_user(id);
    Ok(status)
}

/// Revoke every API token owned by `id`, logging rather than failing on error.
///
/// Best-effort by design: this runs alongside the session revocation that is already best-effort,
/// and the account change itself must not be rolled back because a credential cleanup failed. The
/// token would still be refused at verification time — an owner that is disabled or gone does not
/// authenticate — so a failure here costs visibility in the listing, not safety.
async fn revoke_tokens_of(admin: &Admin, id: Uuid) {
    match admin.api_tokens.revoke_owned_by(id).await {
        Ok(0) => {}
        Ok(n) => tracing::info!(user_id = %id, revoked = n, "revoked API tokens owned by account"),
        Err(e) => {
            tracing::error!(error = %e, user_id = %id, "failed to revoke the account's API tokens");
        }
    }
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

/// Change-scope request body.
#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct SetScope {
    scope: Scope,
}

/// Check a requested scope against the folder groups that exist, returning the `400` a bad one
/// deserves. `known` is every `node_groups.id`. Shared with API-token minting, which accepts the
/// same value and would otherwise grow a second, drifting copy of these two rules.
///
/// Both rejections exist because the failure they prevent is **silent**. A scope naming a group id
/// that does not exist resolves to an empty visible set, and a scope naming no groups at all is
/// already empty — either way the account signs in successfully and sees an inventory of nothing,
/// with no error anywhere to explain it. `Scope::group_uuids` is deliberately built to fail closed
/// on exactly these inputs; this is where an admin gets told instead.
pub(super) fn checked_scope(scope: &Scope, known: &HashSet<Uuid>) -> ApiResult<()> {
    let Scope::Groups(raw) = scope else {
        return Ok(());
    };
    if raw.is_empty() {
        return Err(ApiError::bad_request(
            "empty_scope",
            "a group scope must name at least one group; send \"All\" for the whole fleet",
        ));
    }
    for entry in raw {
        // A group *name* is the mistake this catches: names are editable and not unique, so one
        // would silently widen or void the scope the day somebody renames a folder.
        let Ok(id) = Uuid::parse_str(entry) else {
            return Err(ApiError::bad_request(
                "invalid_scope",
                format!("scope entry {entry:?} is not a group id"),
            ));
        };
        if !known.contains(&id) {
            return Err(ApiError::bad_request(
                "unknown_group",
                format!("no node group {id}"),
            ));
        }
    }
    Ok(())
}

/// `PUT /api/v1/users/:id/scope` — set which node groups an account may see.
#[utoipa::path(
    put, path = "/api/v1/users/{id}/scope", tag = "users",
    params(("id" = Uuid, Path, description = "User account id")),
    request_body = SetScope,
    responses(
        (status = 204, description = "Scope changed and the account's sessions revoked"),
        (status = 400, description = "The scope names no groups, or names something that is not an existing group id", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role below Admin", body = super::error::ErrorBody),
        (status = 404, description = "No such account", body = super::error::ErrorBody),
        (status = 409, description = "Refused: the account is an Admin, whose permissions are fleet-wide", body = super::error::ErrorBody),
        (status = 503, description = "This core has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
/// Limit an account to a set of node groups, or restore fleet-wide visibility with `"All"`.
///
/// A scope narrows what the account can **see**: node lists, aggregates and rankings are filtered to
/// the allowed groups and everything beneath them, and a node outside it answers `404` — the same
/// answer an unknown id gets, so the scope cannot be used to probe for what exists. Endpoints whose
/// answer retains no per-node attribution (a rendered report, a pre-summed fleet timeline) refuse a
/// scoped caller rather than quietly serving fleet-wide numbers.
///
/// It is not a substitute for a role: a scoped Operator can still acknowledge alerts and open
/// maintenance windows, within their groups. An **Admin cannot be scoped** — administration is
/// fleet-wide, so promoting an account to Admin also clears whatever scope it held.
async fn set_user_scope(
    _perm: RequireManageUsers,
    admin: Admin,
    State(st): State<ApiState>,
    Path(id): Path<Uuid>,
    Json(body): Json<SetScope>,
) -> ApiResult<StatusCode> {
    let known: HashSet<Uuid> = admin
        .groups
        .edges()
        .await
        .map_err(|e| {
            ApiError::from_internal(e.as_ref(), "read group ids", "failed to read node groups")
        })?
        .into_iter()
        .map(|(id, _parent)| id)
        .collect();
    checked_scope(&body.scope, &known)?;
    let outcome = admin.users.set_scope(id, &body.scope).await.map_err(|e| {
        ApiError::from_internal(e.as_ref(), "set user scope", "failed to update user scope")
    })?;
    let status = mutation_result(outcome, id)?;
    // Narrowing a scope is a demotion, and the principal — scope included — is captured in the
    // session token at issue time. Without this the account keeps its old, wider view of the fleet
    // until the token expires, which is exactly the window the change was made to close.
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
        (status = 204, description = "Status changed; disabling also revokes the account's sessions and API tokens"),
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
    // Disabling an account must cut every credential it holds — live sessions *and* API tokens.
    // Verification already refuses a token whose owner is disabled, so this is belt-and-braces, but
    // it is the visible half: re-enabling the account must not silently resurrect credentials that
    // were taken away with it. Enabling has nothing to revoke.
    if !body.enabled {
        st.sessions.revoke_user(id);
        revoke_tokens_of(&admin, id).await;
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
    let outcome = admin
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
    mutation_result(outcome, id)?;
    // A password reset (e.g. after a compromise) must invalidate existing sessions, or the
    // attacker's stolen token survives the reset.
    st.sessions.revoke_user(id);
    Ok(StatusCode::NO_CONTENT)
}

/// One permission in the role/privilege matrix.
// `key` is the enum, not a string: the WebUI names a permission wherever it decides whether to draw
// a write control, so the contract has to publish the seven that exist (ADR-056 Inc.2). The wire
// bytes are unchanged — `Permission::key()` has always been the serde tag, and
// `rbac.rs::matrix_catalog_keys_match_serde` is what keeps that true.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct PermissionInfo {
    key: Permission,
    label: &'static str,
    description: &'static str,
}

/// One role in the matrix: its metadata and the permission keys it grants.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct RoleInfo {
    key: Role,
    label: &'static str,
    description: &'static str,
    /// Built-in roles are fixed (custom roles are not configurable yet).
    builtin: bool,
    /// The keys of the permissions this role grants.
    permissions: Vec<Permission>,
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
    Ok(Json(roles_matrix()))
}

/// The permission catalogue and what each role grants — the seam both edges call.
///
/// Pure: it reads no store, because the matrix is the type system's, not the deployment's. Derived
/// from `Permission::ALL` × `Role::ALL` so a new permission appears without anyone remembering.
pub(crate) fn roles_matrix() -> RolesMatrix {
    let permissions = Permission::ALL
        .into_iter()
        .map(|p| PermissionInfo {
            key: p,
            label: p.label(),
            description: p.description(),
        })
        .collect();
    let roles = Role::ALL
        .into_iter()
        .map(|r| RoleInfo {
            key: r,
            label: r.label(),
            description: r.description(),
            builtin: true,
            permissions: Permission::ALL
                .into_iter()
                .filter(|p| r.grants(*p))
                .collect(),
        })
        .collect();
    RolesMatrix { permissions, roles }
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
    fn a_scope_naming_no_groups_is_refused_rather_than_stored() {
        // `Groups([])` is a *valid* scope that sees nothing, so nothing downstream would fail — the
        // account would simply sign in to an empty inventory with no error to explain it. "All" is
        // how you say "the whole fleet"; an empty set is always a mistake at the edge.
        let err = checked_scope(&Scope::Groups(Default::default()), &HashSet::new()).unwrap_err();
        assert_eq!(err.status(), StatusCode::BAD_REQUEST);
        assert_eq!(err.code(), "empty_scope");
        // "All" needs no groups to exist at all.
        assert!(checked_scope(&Scope::All, &HashSet::new()).is_ok());
    }

    #[test]
    fn a_scope_must_name_group_ids_that_exist() {
        let known: HashSet<Uuid> = [Uuid::from_u128(1)].into_iter().collect();
        assert!(checked_scope(&Scope::groups([Uuid::from_u128(1).to_string()]), &known).is_ok());

        // A group *name* is the mistake worth catching: it parses as neither a uuid nor anything
        // the resolver can match, so it would silently restrict to nothing.
        let err = checked_scope(&Scope::groups(["tokyo"]), &known).unwrap_err();
        assert_eq!(err.code(), "invalid_scope");

        // A well-formed id for a group that was deleted (or never existed) is the same silence.
        let err =
            checked_scope(&Scope::groups([Uuid::from_u128(9).to_string()]), &known).unwrap_err();
        assert_eq!(err.code(), "unknown_group");

        // One bad entry among good ones still fails — a partially-applied scope is not a thing.
        let mixed = Scope::groups([
            Uuid::from_u128(1).to_string(),
            Uuid::from_u128(9).to_string(),
        ]);
        assert_eq!(
            checked_scope(&mixed, &known).unwrap_err().code(),
            "unknown_group"
        );
    }

    #[test]
    fn refusing_to_scope_an_admin_is_a_conflict_the_ui_can_explain() {
        // Not a 400: the request is well-formed, and what makes it impossible is the *target's*
        // role, which the caller can change. The code is what the UI branches on to say so.
        let err = mutation_result(UserMutation::AdminIsUnscoped, Uuid::nil()).unwrap_err();
        assert_eq!(err.status(), StatusCode::CONFLICT);
        assert_eq!(err.code(), "admin_is_unscoped");
    }

    #[tokio::test]
    async fn setting_a_scope_is_admin_only_and_authenticates_first() {
        let st = private_state();
        let token = st.sessions.issue(
            Uuid::new_v4(),
            Principal::new(Role::Operator, Scope::All),
            "op1",
        );
        let uri = "/api/v1/users/00000000-0000-0000-0000-000000000000/scope";
        // Anonymous: 401, and it learns nothing about whether this deployment has a user store.
        let (status, json) = send(st.clone(), "PUT", uri, None, r#"{"scope":"All"}"#).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(json["error"]["code"], "unauthorized");
        // Authenticated but not an admin: 403, distinct from the 401 above.
        let (status, json) = send(st, "PUT", uri, Some(&token), r#"{"scope":"All"}"#).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(json["error"]["code"], "forbidden");
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
    // ── An accepted write (ADR-115) ──────────────────────────────────────────────────

    /// An account is created and can be listed; its password is never returned.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn creating_an_account_stores_it_without_returning_the_password(pool: sqlx::PgPool) {
        use crate::api::tests_support::{account_token, live_state, send};
        let st = live_state(pool.clone()).await;
        let (tok, _) = account_token(&st, "fixture-admin", yagra_common::Role::Admin).await;
        let before = crate::pgtest::rows(&pool, "users").await;
        let (status, body) = send(
            &st,
            "POST",
            "/api/v1/users",
            &tok,
            Some(serde_json::json!({
                "username": "viewer-sam",
                "password": "a-long-enough-password",
                "role": "viewer",
            })),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::CREATED, "{body}");
        assert_eq!(crate::pgtest::rows(&pool, "users").await, before + 1);

        let (status, list) = send(&st, "GET", "/api/v1/users", &tok, None).await;
        assert_eq!(status, axum::http::StatusCode::OK, "{list}");
        assert!(list.to_string().contains("viewer-sam"), "{list}");
        assert!(
            !list.to_string().contains("a-long-enough-password"),
            "the list returned the password"
        );
    }
}
