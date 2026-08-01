// SPDX-License-Identifier: AGPL-3.0-only
//! Password login, logout, and "who am I" — the `/api/v1/auth/*` endpoints.
//!
//! These are the one part of the API that cannot take an authorization guard, because passing them
//! *is* how a caller becomes authorized. What stands in for a guard here is the throttle: login is
//! the only endpoint where an anonymous caller can make the server do expensive work (Argon2) and
//! learn something from the answer, so it is rate-limited per account and globally before the hash
//! is ever computed, and every outcome is audited.
//!
//! **Audit rows carry the attempted username and never the credential** (security.md). A failed
//! login records the name that was tried — that is what makes a guessing run visible — but the
//! password never reaches a log, a response, or the audit table.

use super::error::{ApiError, ApiResult};
use super::extract::{bearer, Admin, Caller};
use super::util::audit_record;
use super::ApiState;
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use yagra_common::Role;

/// This domain's slice of the OpenAPI document (ADR-035), merged by [`super::openapi::document`].
#[derive(utoipa::OpenApi)]
#[openapi(paths(login, logout, auth_me))]
pub(super) struct Doc;

/// The auth routes, merged into `/api/v1` by [`super::router`].
pub(super) fn routes() -> Router<ApiState> {
    Router::new()
        .route("/api/v1/auth/login", post(login))
        .route("/api/v1/auth/logout", post(logout))
        .route("/api/v1/auth/me", get(auth_me))
}

/// Login request body. Never logged, never echoed.
#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct LoginBody {
    username: String,
    password: String,
}

/// A freshly issued session.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct LoginOk {
    /// The bearer token for subsequent requests.
    token: String,
    /// The role it carries, so the UI can render the right navigation immediately.
    role: Role,
}

/// The caller's own identity.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct AuthMe {
    role: Role,
    username: String,
}

/// Exchange a username and password for a bearer token.
///
/// Takes `Admin` with no permission guard in front of it — unlike every other handler — because
/// there is nothing to authenticate yet. A skeleton deployment answering 503 here is correct and
/// not a disclosure: "there is no user store" is exactly what a would-be logger-in needs to know.
#[utoipa::path(
    post, path = "/api/v1/auth/login", tag = "session",
    security(()),
    request_body = LoginBody,
    responses(
        (status = 200, description = "A bearer token and the role it carries", body = LoginOk),
        (status = 401, description = "Incorrect username or password — one code for both, so the endpoint is not an account-enumeration oracle", body = super::error::ErrorBody),
        (status = 429, description = "Too many attempts; `Retry-After` carries the wait in seconds", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode has no user store, so there is nothing to log in to", body = super::error::ErrorBody),
    ),
)]
async fn login(
    State(st): State<ApiState>,
    admin: Admin,
    Json(body): Json<LoginBody>,
) -> ApiResult<Json<LoginOk>> {
    // Brute-force guard, checked *before* Argon2 runs: verifying a password is deliberately
    // expensive, so an unthrottled login endpoint is a CPU amplifier as well as a guessing oracle.
    if let Err(reject) = st.login_throttle.check(&body.username) {
        audit_record(&admin.audit, &body.username, "auth.login", 429).await;
        return Err(ApiError::too_many_requests(
            "too_many_attempts",
            format!(
                "too many login attempts; retry in {} seconds",
                reject.retry_after_secs
            ),
        )
        .retry_after(reject.retry_after_secs));
    }
    let verified = admin
        .users
        .verify(&body.username, &body.password)
        .await
        .map_err(|e| ApiError::from_internal(e.as_ref(), "login", "login failed"))?;
    let Some((user_id, principal)) = verified else {
        st.login_throttle.record_failure(&body.username);
        audit_record(&admin.audit, &body.username, "auth.login", 401).await;
        // One message for both "no such user" and "wrong password" — telling them apart is an
        // account-enumeration oracle.
        return Err(ApiError::unauthorized_with(
            "invalid_credentials",
            "incorrect username or password",
        ));
    };
    st.login_throttle.record_success(&body.username);
    let role = principal.role;
    let token = st.sessions.issue(user_id, principal, &body.username);
    audit_record(&admin.audit, &body.username, "auth.login", 200).await;
    Ok(Json(LoginOk { token, role }))
}

/// Revoke the caller's bearer token so it cannot be reused.
///
/// Idempotent and unguarded: an absent, expired or already-revoked token still answers 204. Making
/// logout require a valid session would mean the one action that fixes a suspect token is refused
/// precisely when the token has gone bad.
#[utoipa::path(
    post, path = "/api/v1/auth/logout", tag = "session",
    security(()),
    responses(
        (status = 204, description = "Token revoked; an absent, expired or already-revoked token answers 204 too"),
    ),
)]
async fn logout(State(st): State<ApiState>, headers: HeaderMap) -> StatusCode {
    if let Some(token) = bearer(&headers) {
        st.sessions.revoke_token(token);
    }
    StatusCode::NO_CONTENT
}

/// Who the bearer token belongs to. [`Caller`] does the work: it demands a real session (not open
/// in public-dashboard mode, since an anonymous visitor has no identity to report).
#[utoipa::path(
    get, path = "/api/v1/auth/me", tag = "session",
    responses(
        (status = 200, description = "The bearer holder's role and username", body = AuthMe),
        (status = 401, description = "No valid bearer token — closed even on a public dashboard", body = super::error::ErrorBody),
    ),
)]
async fn auth_me(caller: Caller) -> ApiResult<Json<AuthMe>> {
    Ok(Json(AuthMe {
        role: caller.0.principal.role,
        username: caller.0.username,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::router;
    use crate::api::tests_support::{private_state, public_state};
    use axum::body::{to_bytes, Body};
    use axum::http::{header::AUTHORIZATION, Request};
    use axum::response::IntoResponse;
    use tower::ServiceExt;
    use uuid::Uuid;
    use yagra_common::{Principal, Scope};

    async fn send(
        st: ApiState,
        method: &str,
        path: &str,
        token: Option<&str>,
        body: &str,
    ) -> axum::response::Response {
        let mut b = Request::builder()
            .method(method)
            .uri(path)
            .header("content-type", "application/json");
        if let Some(t) = token {
            b = b.header(AUTHORIZATION, format!("Bearer {t}"));
        }
        router(st)
            .oneshot(b.body(Body::from(body.to_owned())).unwrap())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn logout_succeeds_with_no_token_at_all() {
        // The action that fixes a suspect token must not require that token to be good.
        let resp = send(private_state(), "POST", "/api/v1/auth/logout", None, "").await;
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn who_am_i_needs_a_session_even_on_a_public_dashboard() {
        for st in [private_state(), public_state()] {
            let resp = send(st, "GET", "/api/v1/auth/me", None, "").await;
            assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        }
    }

    #[tokio::test]
    async fn who_am_i_reports_the_bearer_holders_identity() {
        let st = private_state();
        let token = st.sessions.issue(
            Uuid::new_v4(),
            Principal::new(Role::Operator, Scope::All),
            "op1",
        );
        let resp = send(st, "GET", "/api/v1/auth/me", Some(&token), "").await;
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["username"], "op1");
        assert_eq!(json["role"], "operator");
    }

    #[tokio::test]
    async fn login_needs_a_user_store() {
        // No guard runs first here, so skeleton mode answers 503 — which is the honest answer to
        // "let me log in" on a deployment that has no accounts.
        let resp = send(
            public_state(),
            "POST",
            "/api/v1/auth/login",
            None,
            r#"{"username":"a","password":"b"}"#,
        )
        .await;
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[test]
    fn a_throttled_login_tells_the_client_how_long_to_wait() {
        // Without Retry-After a throttled client has to guess, and guessing wrong turns a rate
        // limit into a hot loop against it.
        let resp = ApiError::too_many_requests("too_many_attempts", "slow down")
            .retry_after(42)
            .into_response();
        assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(resp.headers()[axum::http::header::RETRY_AFTER], "42");
    }
}
