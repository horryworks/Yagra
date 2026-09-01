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

/// This domain's slice of the OpenAPI document (ADR-035), merged by [`super::openapi::document`].
#[derive(utoipa::OpenApi)]
#[openapi(paths(
    oidc_authorize,
    oidc_callback,
    list_oidc_providers,
    create_oidc_provider,
    update_oidc_provider,
    delete_oidc_provider
))]
pub(super) struct Doc;

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
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct AuthorizeUrl {
    authorize_url: String,
}

/// A session minted by a completed SSO login — the same shape local login returns, deliberately, so
/// the WebUI stores a token the same way whichever path produced it.
#[derive(Debug, Serialize, utoipa::ToSchema)]
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
#[utoipa::path(
    get, path = "/api/v1/auth/oidc/authorize", tag = "oidc",
    security(()),
    responses(
        (status = 200, description = "The IdP authorization URL the browser should be sent to", body = AuthorizeUrl),
        (status = 502, description = "Provider discovery or configuration failed", body = super::error::ErrorBody),
        (status = 503, description = "No OIDC provider store, or no provider enabled", body = super::error::ErrorBody),
    ),
)]
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

/// Audit action for a completed or refused SSO login.
const AUDIT_OIDC: &str = "auth.oidc";
/// Audit action for the one refusal that carries a diagnosis: the IdP delivered the groups claim
/// out-of-band. A **suffix** of [`AUDIT_OIDC`] on purpose — the LDAP path set the precedent
/// (`auth.login.ldap_unavailable` beside `auth.login`), and it keeps a prefix query over the
/// existing action finding the new rows.
const AUDIT_OIDC_OVERAGE: &str = "auth.oidc.group_overage";

/// OIDC callback body: the `code` + `state` the WebUI forwards from the IdP redirect.
#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct OidcCallbackBody {
    code: String,
    state: String,
}

/// Complete an OIDC login: exchange the code, validate the ID token, map IdP groups to a role,
/// JIT-provision the account, and issue a Yagra session.
#[utoipa::path(
    post, path = "/api/v1/auth/oidc/callback", tag = "oidc",
    security(()),
    request_body = OidcCallbackBody,
    responses(
        (status = 200, description = "A Yagra session for the SSO account", body = OidcSession),
        (status = 401, description = "The login could not be completed; which step failed stays server-side", body = super::error::ErrorBody),
        (status = 503, description = "No OIDC provider store, no provider enabled, or skeleton mode", body = super::error::ErrorBody),
    ),
)]
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
            // Bad or expired state, code-exchange failure, ID-token validation failure, and "no
            // group maps to a role" all land here and all answer the same thing. Which one it
            // was stays server-side: the difference is exactly what an attacker probing the
            // callback would want to learn.
            Err(crate::oidc::OidcError::Other(e)) => {
                tracing::warn!(error = %e, "OIDC login rejected");
                audit_record(&admin.audit, "(oidc)", AUDIT_OIDC, 401).await;
                return Err(ApiError::unauthorized_with(
                    "oidc_denied",
                    "SSO login could not be completed",
                ));
            }
            // Group overage is the one refusal an admin cannot diagnose from the outside: before
            // this branch existed it was not a refusal at all — the groups read as empty and the
            // login silently took `default_role`. The client still gets the identical 401, but the
            // audit log gets its own action *and the real username*, because "an admin quietly
            // became a viewer" is unactionable without knowing which admin.
            //
            // Naming the user here does not help a prober: reaching this point requires a code
            // exchange plus an ID token whose signature, issuer, audience, expiry and nonce all
            // verified against the configured IdP. Nobody arrives here by guessing.
            Err(crate::oidc::OidcError::GroupOverage { username, claim }) => {
                // The remediation belongs in the log, where an admin is actually looking. Note what
                // is *not* logged: `_claim_sources` carries a Graph URL embedding the user's
                // directory object id, so the claim name is as far as this goes.
                tracing::warn!(
                    user = %username,
                    claim = %claim,
                    "SSO login refused: the IdP delivered the groups claim out-of-band (group \
                     overage). Yagra reads groups from the ID token only — configure the app \
                     registration to emit only the groups assigned to it, or use a group-filtering \
                     claim."
                );
                metrics::counter!("yagra_oidc_login_total", "result" => "group_overage")
                    .increment(1);
                audit_record(&admin.audit, &username, AUDIT_OIDC_OVERAGE, 401).await;
                return Err(ApiError::unauthorized_with(
                    "oidc_denied",
                    "SSO login could not be completed",
                ));
            }
        };
    let upsert = admin
        .users
        .upsert_external_user(
            yagra_common::UserKind::Oidc,
            config.id,
            &login.subject,
            &login.username,
            login.role,
        )
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "OIDC user provisioning",
                "failed to provision the SSO account",
            )
        })?;
    let (user_id, principal) = match upsert {
        crate::auth::ExternalUpsert::Ok(id, principal) => (id, principal),
        // Both of these used to reach the client as a 500 through `from_internal`. They are
        // ordinary refusals: an account an admin switched off, and a name somebody else owns.
        // Which one it was stays server-side, like every other branch of this handler.
        outcome => {
            match &outcome {
                crate::auth::ExternalUpsert::Disabled => {
                    tracing::warn!(user = %login.username, "SSO login refused: account is disabled");
                }
                crate::auth::ExternalUpsert::UsernameTaken(existing) => {
                    tracing::warn!(
                        subject = %login.subject,
                        existing = %existing,
                        "SSO login refused: the username is already held by another account"
                    );
                }
                crate::auth::ExternalUpsert::Ok(..) => unreachable!("matched above"),
            }
            audit_record(&admin.audit, &login.username, AUDIT_OIDC, 401).await;
            return Err(ApiError::unauthorized_with(
                "oidc_denied",
                "SSO login could not be completed",
            ));
        }
    };
    let role = principal.role;
    let token = st.sessions.issue(user_id, principal, &login.username);
    audit_record(&admin.audit, &login.username, AUDIT_OIDC, 200).await;
    Ok(Json(OidcSession { token, role }))
}

/// Configured providers — **metadata only**, never the client secret.
#[utoipa::path(
    get, path = "/api/v1/settings/oidc", tag = "oidc",
    responses(
        (status = 200, description = "Every configured provider, without its client secret", body = Vec<crate::oidc::OidcProviderSummary>),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks the ManageUsers permission", body = super::error::ErrorBody),
        (status = 503, description = "This deployment does not persist SSO configuration", body = super::error::ErrorBody),
    ),
)]
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

#[utoipa::path(
    post, path = "/api/v1/settings/oidc", tag = "oidc",
    request_body = crate::oidc::OidcProviderInput,
    responses(
        (status = 201, description = "Provider created", body = CreatedId),
        (status = 400, description = "The provider definition is invalid, or the client secret is missing", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks the ManageUsers permission", body = super::error::ErrorBody),
        (status = 503, description = "This deployment does not persist SSO configuration", body = super::error::ErrorBody),
    ),
)]
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

#[utoipa::path(
    put, path = "/api/v1/settings/oidc/{id}", tag = "oidc",
    params(("id" = Uuid, Path, description = "Provider id")),
    request_body = crate::oidc::OidcProviderInput,
    responses(
        (status = 204, description = "Provider updated; an omitted client secret keeps the stored one"),
        (status = 400, description = "The provider definition is invalid", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks the ManageUsers permission", body = super::error::ErrorBody),
        (status = 404, description = "No such provider", body = super::error::ErrorBody),
        (status = 503, description = "This deployment does not persist SSO configuration", body = super::error::ErrorBody),
    ),
)]
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

#[utoipa::path(
    delete, path = "/api/v1/settings/oidc/{id}", tag = "oidc",
    params(("id" = Uuid, Path, description = "Provider id")),
    responses(
        (status = 204, description = "Provider deleted"),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks the ManageUsers permission", body = super::error::ErrorBody),
        (status = 404, description = "No such provider", body = super::error::ErrorBody),
        (status = 503, description = "This deployment does not persist SSO configuration", body = super::error::ErrorBody),
    ),
)]
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

    // The overage refusal gets its own audit action so an admin can find it, but an existing
    // prefix query over the SSO action must keep matching it — that is why it is a suffix and not a
    // sibling like `auth.oidc_group_overage`.
    #[test]
    fn the_overage_audit_action_extends_the_sso_action() {
        assert!(AUDIT_OIDC_OVERAGE.starts_with(AUDIT_OIDC));
        assert_ne!(AUDIT_OIDC_OVERAGE, AUDIT_OIDC);
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
            kind: crate::oidc::OidcProviderKind::Generic,
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
    // ── An accepted write (ADR-115) ──────────────────────────────────────────────────

    /// An SSO provider is created, and its client secret never comes back out.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn creating_a_provider_seals_the_client_secret(pool: sqlx::PgPool) {
        use crate::api::tests_support::{live_state, send, token};
        let st = live_state(pool.clone()).await;
        let tok = token(&st, yagra_common::Role::Admin);
        let (status, body) = send(
            &st,
            "POST",
            "/api/v1/settings/oidc",
            &tok,
            Some(serde_json::json!({
                "name": "corp idp",
                "issuer": "https://idp.corp.invalid",
                "client_id": "yagra",
                "client_secret": "s3cr3t-client",
                "redirect_uri": "https://yagra.corp.invalid/auth/oidc/callback",
            })),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::CREATED, "{body}");
        assert_eq!(crate::pgtest::rows(&pool, "oidc_providers").await, 1);

        let (status, list) = send(&st, "GET", "/api/v1/settings/oidc", &tok, None).await;
        assert_eq!(status, axum::http::StatusCode::OK, "{list}");
        assert!(
            !list.to_string().contains("s3cr3t-client"),
            "the list returned the client secret"
        );
    }
}
