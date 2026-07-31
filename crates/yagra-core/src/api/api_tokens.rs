// SPDX-License-Identifier: AGPL-3.0-only
//! Personal access tokens (Settings ▸ API tokens) — the durable credential for the MCP tool
//! surface and for external automation (ADR-028).
//!
//! Admin-only throughout, under `ManageUsers` rather than `ManageConfig`: a token carries a role and
//! a scope, so minting one hands out an identity. That is a user-management act however it is
//! spelled, and an operator who may reconfigure monitoring still must not be able to issue
//! themselves an admin credential.
//!
//! **The raw token exists in one response and nowhere else.** Only its hash is stored, so create
//! returns it once and no later read can. It must never be logged or echoed back — the listing
//! carries metadata only.

use super::error::{ApiError, ApiResult};
use super::extract::{Admin, Caller, RequireManageUsers};
use super::ApiState;
use axum::{
    extract::Path,
    http::StatusCode,
    routing::{delete, get},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use yagra_common::Role;

/// Longest accepted token label, in characters (not bytes — the name is operator-facing text).
const MAX_TOKEN_NAME_CHARS: usize = 128;

/// This domain's slice of the OpenAPI document (ADR-035), merged by [`super::openapi::document`].
#[derive(utoipa::OpenApi)]
#[openapi(paths(list_api_tokens, create_api_token, revoke_api_token))]
pub(super) struct Doc;

/// The API-token routes, merged into `/api/v1` by [`super::router`].
pub(super) fn routes() -> Router<ApiState> {
    Router::new()
        .route(
            "/api/v1/api-tokens",
            get(list_api_tokens).post(create_api_token),
        )
        .route("/api/v1/api-tokens/:id", delete(revoke_api_token))
}

/// Request body for `POST /api/v1/api-tokens`.
#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct CreateApiTokenBody {
    /// Human label (unique, ≤128 chars).
    name: String,
    /// The role the token grants (`viewer` is the right default for a read-only MCP client).
    role: Role,
    /// Visibility scope; defaults to `All` when omitted. Only `"All"` is accepted today — a
    /// group scope is rejected with `400 unsupported_scope` rather than stored, because nothing
    /// enforces it yet.
    scope: Option<yagra_common::Scope>,
}

/// The one and only response carrying a usable token.
///
/// A named type rather than an inline `json!` because of what the `token` field is: the client has
/// to store it now, since only its hash is kept and no later call can produce it again. Giving it a
/// type makes that field visible in one place if this response ever grows a second consumer.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct CreatedApiToken {
    id: Uuid,
    name: String,
    role: Role,
    /// The raw bearer token, returned **once**. Never stored, never logged.
    token: String,
}

/// Check the request fields that need no store, returning the trimmed name and resolved scope.
///
/// Split out of the handler because the handler's body is only reachable past the `Admin`
/// extractor, so a skeleton-mode test answers `503` and can never see these rules. Keeping the
/// judgement in a plain function is what makes them testable at all.
fn validate_create(body: &CreateApiTokenBody) -> Result<(&str, yagra_common::Scope), ApiError> {
    let name = body.name.trim();
    if name.is_empty() || name.chars().count() > MAX_TOKEN_NAME_CHARS {
        return Err(ApiError::bad_request(
            "invalid_name",
            "token name must be 1–128 characters",
        ));
    }
    // Absent scope means the whole fleet, matching what the UI offers by default.
    let scope = body.scope.clone().unwrap_or(yagra_common::Scope::All);
    // A group-scoped token would be a credential that works nowhere. `/mcp` refuses a non-`All`
    // principal outright (fail-closed, because its read tools return unfiltered data), and the REST
    // handlers never consult `Scope` at all — `Principal::can_see` has no caller under `api/`. So
    // storing one hands an admin a credential they believe is least-privileged while it is in fact
    // either rejected or unrestricted, depending on which surface they point it at. Refuse it until
    // scope filtering lands (ADR-014 / ADR-028 WS-F), applying the rule the MCP gate already does.
    if scope != yagra_common::Scope::All {
        return Err(ApiError::bad_request(
            "unsupported_scope",
            "group-scoped tokens are not supported yet — omit `scope` or send \"All\"",
        ));
    }
    Ok((name, scope))
}

#[utoipa::path(
    post, path = "/api/v1/api-tokens", tag = "api-tokens",
    request_body = CreateApiTokenBody,
    responses(
        (status = 201, description = "Token minted; `token` is the raw bearer and is returned only here", body = CreatedApiToken),
        (status = 400, description = "The token name is empty or over 128 characters, or the scope is not `All`", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role below Admin", body = super::error::ErrorBody),
        (status = 409, description = "An API token with that name already exists", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode has no token store", body = super::error::ErrorBody),
    ),
)]
async fn create_api_token(
    _guard: RequireManageUsers,
    caller: Caller,
    admin: Admin,
    Json(body): Json<CreateApiTokenBody>,
) -> ApiResult<(StatusCode, Json<CreatedApiToken>)> {
    let (name, scope) = validate_create(&body)?;
    let (id, raw) = admin
        .api_tokens
        .create(name, body.role, &scope, &caller.0.username)
        .await
        .map_err(|e| {
            // The unique index on the label is the only expected failure; everything else is a
            // fault the caller cannot act on.
            if e.downcast_ref::<sqlx::Error>()
                .and_then(sqlx::Error::as_database_error)
                .is_some_and(|db| db.is_unique_violation())
            {
                return ApiError::conflict(
                    "duplicate_name",
                    "an API token with that name already exists",
                );
            }
            ApiError::from_internal(e.as_ref(), "create API token", "failed to create API token")
        })?;
    Ok((
        StatusCode::CREATED,
        Json(CreatedApiToken {
            id,
            name: name.to_owned(),
            role: body.role,
            token: raw,
        }),
    ))
}

#[utoipa::path(
    get, path = "/api/v1/api-tokens", tag = "api-tokens",
    responses(
        (status = 200, description = "Token metadata only — never the raw token or its hash", body = Vec<crate::apitokens::ApiTokenInfo>),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role below Admin", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode has no token store", body = super::error::ErrorBody),
    ),
)]
async fn list_api_tokens(
    _guard: RequireManageUsers,
    admin: Admin,
) -> ApiResult<Json<Vec<crate::apitokens::ApiTokenInfo>>> {
    // Metadata only — the row type carries no secret, which is what makes this listable at all.
    let tokens = admin.api_tokens.list().await.map_err(|e| {
        ApiError::from_internal(e.as_ref(), "list API tokens", "failed to list API tokens")
    })?;
    Ok(Json(tokens))
}

#[utoipa::path(
    delete, path = "/api/v1/api-tokens/{id}", tag = "api-tokens",
    params(("id" = Uuid, Path, description = "API token id")),
    responses(
        (status = 204, description = "Token revoked; idempotent, so an already-revoked id also answers 204"),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role below Admin", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode has no token store", body = super::error::ErrorBody),
    ),
)]
async fn revoke_api_token(
    _guard: RequireManageUsers,
    admin: Admin,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    // Idempotent: a missing or already-revoked id is a no-op 204. Revocation is what an operator
    // reaches for when a token may be compromised, so "it was already gone" is success, not 404.
    admin.api_tokens.revoke(id).await.map_err(|e| {
        ApiError::from_internal(e.as_ref(), "revoke API token", "failed to revoke API token")
    })?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::router;
    use crate::api::tests_support::{private_state, public_state};
    use axum::body::Body;
    use axum::http::{header::AUTHORIZATION, Request};
    use tower::ServiceExt;
    use yagra_common::{Principal, Scope};

    const ID: &str = "00000000-0000-0000-0000-000000000001";

    fn all_routes() -> Vec<(&'static str, String)> {
        vec![
            ("GET", "/api/v1/api-tokens".to_owned()),
            ("POST", "/api/v1/api-tokens".to_owned()),
            ("DELETE", format!("/api/v1/api-tokens/{ID}")),
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
    async fn an_anonymous_caller_is_told_it_is_anonymous_and_nothing_else() {
        for (method, path) in all_routes() {
            assert_eq!(
                status_of(private_state(), method, &path, None).await,
                StatusCode::UNAUTHORIZED,
                "{method} {path}"
            );
        }
    }

    #[tokio::test]
    async fn token_management_is_closed_on_a_public_dashboard() {
        // Including the *read*: the listing names the roles and scopes credentials carry, which is
        // a map of what an attacker would want to steal.
        for (method, path) in all_routes() {
            assert_eq!(
                status_of(public_state(), method, &path, None).await,
                StatusCode::UNAUTHORIZED,
                "{method} {path}"
            );
        }
    }

    #[tokio::test]
    async fn an_operator_cannot_mint_itself_a_credential() {
        // ManageUsers, not ManageConfig: an operator who may reconfigure monitoring must not be
        // able to issue a token — the token carries a role.
        let st = private_state();
        for role in [Role::Viewer, Role::Operator] {
            let token = st
                .sessions
                .issue(Uuid::new_v4(), Principal::new(role, Scope::All), "u");
            for (method, path) in all_routes() {
                assert_eq!(
                    status_of(st.clone(), method, &path, Some(&token)).await,
                    StatusCode::FORBIDDEN,
                    "{role:?} {method} {path}"
                );
            }
        }
    }

    fn body(name: &str, scope: Option<Scope>) -> CreateApiTokenBody {
        CreateApiTokenBody {
            name: name.to_owned(),
            role: Role::Viewer,
            scope,
        }
    }

    #[test]
    fn a_group_scoped_token_is_refused_rather_than_minted() {
        // It would authenticate nowhere: `/mcp` rejects a non-`All` principal (fail-closed) and the
        // REST edge never reads `Scope`. Minting one looks like least privilege and is not.
        let err = validate_create(&body("ro", Some(Scope::groups(["tokyo"])))).unwrap_err();
        assert_eq!(err.code(), "unsupported_scope");
    }

    #[test]
    fn an_absent_or_explicit_all_scope_is_accepted() {
        // Both spellings must survive: the WebUI omits the field, an API client may send "All".
        for scope in [None, Some(Scope::All)] {
            let req = body("  ro  ", scope);
            let (name, resolved) = validate_create(&req).unwrap();
            assert_eq!(name, "ro", "the name is trimmed");
            assert_eq!(resolved, Scope::All);
        }
    }

    #[test]
    fn a_blank_or_oversized_name_is_refused() {
        for name in ["", "   ", &"x".repeat(MAX_TOKEN_NAME_CHARS + 1)] {
            assert_eq!(
                validate_create(&body(name, None)).unwrap_err().code(),
                "invalid_name"
            );
        }
        // The boundary itself is allowed, and counts characters rather than bytes.
        assert!(validate_create(&body(&"あ".repeat(MAX_TOKEN_NAME_CHARS), None)).is_ok());
    }

    #[tokio::test]
    async fn an_admin_clears_the_gate_and_reaches_availability() {
        // Positive control: 503 (past RBAC, into skeleton mode), not 403.
        let st = private_state();
        let token = st.sessions.issue(
            Uuid::new_v4(),
            Principal::new(Role::Admin, Scope::All),
            "admin1",
        );
        for (method, path) in all_routes() {
            assert_eq!(
                status_of(st.clone(), method, &path, Some(&token)).await,
                StatusCode::SERVICE_UNAVAILABLE,
                "{method} {path}"
            );
        }
    }
}
