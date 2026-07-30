// SPDX-License-Identifier: AGPL-3.0-only
//! Single sign-on against an external OpenID Connect provider (ADR-010 Phase 3).
//!
//! Two unrelated surfaces share this file because they share a store:
//!
//! - **The login flow** (`/auth/oidc/authorize`, `/auth/oidc/callback`) is unauthenticated, like
//!   password login — completing it is how a caller gets a session. It is protected instead by the
//!   `state`/PKCE/nonce triple held server-side for the duration of the flight.
//! - **Provider administration** (`/settings/oidc`) is `ManageUsers`, the same permission as
//!   issuing an API token: whoever defines the provider and its group→role map decides who becomes
//!   an admin.
//!
//! **The client secret is write-only over this API.** It is sealed with envelope encryption before
//! it reaches the database and the listing never carries it back; an update that omits it keeps the
//! stored value rather than clearing it.

use super::error::{ApiError, ApiResult};
use super::extract::{Admin, Oidc, RequireManageUsers};
use super::util::{audit_record, CreatedId};
use super::ApiState;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use yagra_common::Role;

/// The OIDC routes, merged into `/api/v1` by [`super::router`].
pub(super) fn routes() -> Router<ApiState> {
    Router::new()
        .route("/api/v1/auth/oidc/authorize", get(oidc_authorize))
        .route("/api/v1/auth/oidc/callback", post(oidc_callback))
        .route(
            "/api/v1/settings/oidc",
            get(list_oidc_providers).post(create_oidc_provider),
        )
        .route(
            "/api/v1/settings/oidc/:id",
            axum::routing::put(update_oidc_provider).delete(delete_oidc_provider),
        )
}

/// Where to send the browser to start the SSO handshake.
#[derive(Debug, Serialize)]
pub(crate) struct AuthorizeUrl {
    authorize_url: String,
}

/// A session minted by a completed SSO login — the same shape local login returns, deliberately, so
/// the WebUI stores a token the same way whichever path produced it.
#[derive(Debug, Serialize)]
pub(crate) struct OidcSession {
    token: String,
    role: Role,
}

/// The enabled provider, or the typed 503 the UI distinguishes from "not configured at all".
async fn enabled_config(oidc: &Oidc) -> Result<crate::oidc::OidcProviderConfig, ApiError> {
    oidc.enabled_config()
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "load OIDC provider",
                "failed to load the OIDC provider",
            )
        })?
        .ok_or_else(|| ApiError::unavailable("sso_disabled", "no OIDC provider is enabled"))
}

/// Begin an OIDC login: the IdP authorization URL the browser should be sent to. The CSRF `state`,
/// `nonce` and PKCE verifier are stashed server-side for the flight. Unauthenticated (pre-session).
async fn oidc_authorize(State(st): State<ApiState>, oidc: Oidc) -> ApiResult<Json<AuthorizeUrl>> {
    let config = enabled_config(&oidc).await?;
    let url = crate::oidc::begin_authorize(&config, &st.oidc_flight)
        .await
        // A discovery or configuration problem is the *provider's* failure, not the browser's, so it
        // is a 502 — and its detail stays server-side rather than describing the IdP to a stranger.
        .map_err(|e| {
            tracing::error!(error = %e, "OIDC authorize failed");
            ApiError::bad_gateway(
                "oidc_error",
                "could not start the SSO login (check the provider configuration)",
            )
        })?;
    Ok(Json(AuthorizeUrl { authorize_url: url }))
}

/// OIDC callback body: the `code` + `state` the WebUI forwards from the IdP redirect.
#[derive(Deserialize)]
struct OidcCallbackBody {
    code: String,
    state: String,
}

/// Complete an OIDC login: exchange the code, validate the ID token, map IdP groups to a role,
/// JIT-provision the account, and issue a Yagra session.
async fn oidc_callback(
    State(st): State<ApiState>,
    oidc: Oidc,
    admin: Admin,
    Json(body): Json<OidcCallbackBody>,
) -> ApiResult<Json<OidcSession>> {
    let config = enabled_config(&oidc).await?;
    let login =
        match crate::oidc::complete_callback(&config, &st.oidc_flight, &body.code, &body.state)
            .await
        {
            Ok(login) => login,
            Err(e) => {
                // Bad or expired state, code-exchange failure, ID-token validation failure, and "no
                // group maps to a role" all land here and all answer the same thing. Which one it
                // was stays server-side: the difference is exactly what an attacker probing the
                // callback would want to learn.
                tracing::warn!(error = %e, "OIDC login rejected");
                audit_record(&admin.audit, "(oidc)", "auth.oidc", 401).await;
                return Err(ApiError::unauthorized_with(
                    "oidc_denied",
                    "SSO login could not be completed",
                ));
            }
        };
    let (user_id, principal) = admin
        .users
        .upsert_oidc_user(config.id, &login.subject, &login.username, login.role)
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "OIDC user provisioning",
                "failed to provision the SSO account",
            )
        })?;
    let role = principal.role;
    let token = st.sessions.issue(user_id, principal, &login.username);
    audit_record(&admin.audit, &login.username, "auth.oidc", 200).await;
    Ok(Json(OidcSession { token, role }))
}

/// Configured providers — **metadata only**, never the client secret.
async fn list_oidc_providers(
    _guard: RequireManageUsers,
    oidc: Oidc,
) -> ApiResult<Json<Vec<crate::oidc::OidcProviderSummary>>> {
    let list = oidc.list().await.map_err(|e| {
        ApiError::from_internal(
            e.as_ref(),
            "list OIDC providers",
            "failed to list OIDC providers",
        )
    })?;
    Ok(Json(list))
}

/// Reject a provider definition the caller got wrong, before the write path runs.
///
/// Splitting this out is what lets the handlers below tell a bad submission from a broken server.
/// Both used to render *every* failure of `create`/`update` as `400 invalid_provider` carrying
/// `e.to_string()` — so a failure to seal the secret (a KEK problem) or a database error was
/// reported to the operator as "your input is invalid", with the internal message attached. Wrong
/// status, and internal detail on the wire (security.md).
fn validate(input: &crate::oidc::OidcProviderInput) -> Result<(), ApiError> {
    crate::oidc::OidcRepo::validate(input)
        .map_err(|e| ApiError::bad_request("invalid_provider", e.to_string()))
}

async fn create_oidc_provider(
    _guard: RequireManageUsers,
    oidc: Oidc,
    Json(input): Json<crate::oidc::OidcProviderInput>,
) -> ApiResult<(StatusCode, Json<CreatedId>)> {
    validate(&input)?;
    // Required on create, optional on update (where omitting it keeps the stored secret), so it is
    // checked here rather than inside the shared validator.
    if input
        .client_secret
        .as_deref()
        .filter(|s| !s.is_empty())
        .is_none()
    {
        return Err(ApiError::bad_request(
            "invalid_provider",
            "client_secret is required",
        ));
    }
    let id = oidc.create(&input).await.map_err(|e| {
        ApiError::from_internal(
            e.as_ref(),
            "create OIDC provider",
            "failed to create the OIDC provider",
        )
    })?;
    Ok((StatusCode::CREATED, Json(CreatedId { id })))
}

async fn update_oidc_provider(
    _guard: RequireManageUsers,
    oidc: Oidc,
    Path(id): Path<Uuid>,
    Json(input): Json<crate::oidc::OidcProviderInput>,
) -> ApiResult<StatusCode> {
    validate(&input)?;
    let changed = oidc.update(id, &input).await.map_err(|e| {
        ApiError::from_internal(
            e.as_ref(),
            "update OIDC provider",
            "failed to update the OIDC provider",
        )
    })?;
    if changed {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found(
            "provider_not_found",
            format!("no OIDC provider {id}"),
        ))
    }
}

async fn delete_oidc_provider(
    _guard: RequireManageUsers,
    oidc: Oidc,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    match oidc.delete(id).await {
        Ok(true) => Ok(StatusCode::NO_CONTENT),
        Ok(false) => Err(ApiError::not_found(
            "provider_not_found",
            format!("no OIDC provider {id}"),
        )),
        Err(e) => Err(ApiError::from_internal(
            e.as_ref(),
            "delete OIDC provider",
            "failed to delete the OIDC provider",
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
    use yagra_common::{Principal, Scope};

    const ID: &str = "00000000-0000-0000-0000-000000000001";

    fn admin_routes() -> Vec<(&'static str, String)> {
        vec![
            ("GET", "/api/v1/settings/oidc".to_owned()),
            ("POST", "/api/v1/settings/oidc".to_owned()),
            ("PUT", format!("/api/v1/settings/oidc/{ID}")),
            ("DELETE", format!("/api/v1/settings/oidc/{ID}")),
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
    async fn provider_administration_is_gated_before_availability() {
        for (method, path) in admin_routes() {
            assert_eq!(
                status_of(private_state(), method, &path, None).await,
                StatusCode::UNAUTHORIZED,
                "{method} {path}"
            );
            // Closed on a public dashboard too — the role map decides who becomes an admin.
            assert_eq!(
                status_of(public_state(), method, &path, None).await,
                StatusCode::UNAUTHORIZED,
                "public {method} {path}"
            );
        }
    }

    #[tokio::test]
    async fn only_manage_users_may_define_a_provider() {
        // Same permission as minting an API token, and for the same reason: whoever writes the
        // group→role map decides who logs in as an admin.
        let st = private_state();
        for role in [Role::Viewer, Role::Operator] {
            let token = st
                .sessions
                .issue(Uuid::new_v4(), Principal::new(role, Scope::All), "u");
            for (method, path) in admin_routes() {
                assert_eq!(
                    status_of(st.clone(), method, &path, Some(&token)).await,
                    StatusCode::FORBIDDEN,
                    "{role:?} {method} {path}"
                );
            }
        }
    }

    #[tokio::test]
    async fn the_login_flow_is_reachable_without_a_session() {
        // It has to be: completing it is how a caller gets one. With no provider store configured
        // it answers 503 rather than 401.
        for (method, path) in [
            ("GET", "/api/v1/auth/oidc/authorize"),
            ("POST", "/api/v1/auth/oidc/callback"),
        ] {
            assert_eq!(
                status_of(private_state(), method, path, None).await,
                StatusCode::SERVICE_UNAVAILABLE,
                "{method} {path}"
            );
        }
    }

    #[test]
    fn a_malformed_provider_is_a_400_and_says_why() {
        // The client-safe half: this is about what the caller submitted.
        let bad = crate::oidc::OidcProviderInput {
            issuer: "not a url".to_owned(),
            ..sample_input()
        };
        let err = validate(&bad).unwrap_err();
        assert_eq!(err.status(), StatusCode::BAD_REQUEST);
        assert_eq!(err.code(), "invalid_provider");

        assert!(validate(&sample_input()).is_ok());
    }

    fn sample_input() -> crate::oidc::OidcProviderInput {
        crate::oidc::OidcProviderInput {
            name: "idp".to_owned(),
            issuer: "https://idp.example.com".to_owned(),
            client_id: "yagra".to_owned(),
            client_secret: Some("s".to_owned()),
            redirect_uri: "https://yagra.example.com/callback".to_owned(),
            scopes: "openid".to_owned(),
            groups_claim: "groups".to_owned(),
            role_map: Default::default(),
            default_role: None,
            enabled: true,
        }
    }
}
