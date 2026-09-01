// SPDX-License-Identifier: AGPL-3.0-only
//! LDAP/AD directory configuration (ADR-041) — Settings ▸ Auth.
//!
//! Three endpoints: read the configuration, save it, and test it. All three take
//! [`RequireManageUsers`] rather than `ManageConfig`, matching the OIDC provider CRUD next door —
//! this decides *who can sign in and as what*, which is user administration, not monitoring
//! configuration. It matters more here than it does for OIDC: the test endpoint's optional
//! `username` turns the directory into a readable oracle over other people's DNs and group
//! memberships, and that is not something an operator-level account should be able to ask.
//!
//! Authorization runs **before** availability on every one of them (`Require*` first, then the
//! `Ldap` extractor), so an unauthenticated prober cannot tell a deployment that has a directory
//! from one that does not by the difference between 401 and 503.

use axum::{
    extract::State,
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use utoipa::OpenApi;

use super::error::{ApiError, ApiResult};
use super::extract::{Ldap, RequireManageUsers};
use super::ApiState;
use crate::ldap::{LdapConfigInput, LdapConfigView, LdapTestResult};

#[derive(OpenApi)]
#[openapi(paths(get_ldap_config, put_ldap_config, test_ldap_config))]
pub(super) struct Doc;

pub(super) fn routes() -> Router<ApiState> {
    Router::new()
        .route(
            "/api/v1/settings/ldap",
            get(get_ldap_config).put(put_ldap_config),
        )
        .route("/api/v1/settings/ldap/test", post(test_ldap_config))
}

/// The directory configuration, or `null` when none has been saved.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct LdapConfigResponse {
    /// `null` rather than a 404: "not configured" is the normal state of a fresh installation, and
    /// the settings form wants to render its empty self rather than an error.
    config: Option<LdapConfigView>,
}

/// Which account, if any, the test should look up.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub(crate) struct LdapTestBody {
    /// A username to resolve. Optional: with none, the check still proves the connection, the TLS
    /// trust and the service account's bind. With one, it also reports the DN, the groups and the
    /// role that user would receive — **without** binding as them, so it can neither be used as a
    /// credential-testing proxy into the directory nor push anybody towards their domain's lockout
    /// threshold.
    #[serde(default)]
    username: Option<String>,
}

/// The stored directory configuration — never the bind password.
#[utoipa::path(
    get, path = "/api/v1/settings/ldap", tag = "ldap",
    responses(
        (status = 200, description = "The directory configuration, or null when none is saved", body = LdapConfigResponse),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks the user-administration permission", body = super::error::ErrorBody),
        (status = 503, description = "This deployment has no directory store", body = super::error::ErrorBody),
    ),
)]
async fn get_ldap_config(
    _guard: RequireManageUsers,
    ldap: Ldap,
) -> ApiResult<Json<LdapConfigResponse>> {
    let config = ldap.view().await.map_err(|e| {
        ApiError::from_internal(
            e.as_ref(),
            "read the directory configuration",
            "failed to read the directory configuration",
        )
    })?;
    Ok(Json(LdapConfigResponse { config }))
}

/// Save the directory configuration.
#[utoipa::path(
    put, path = "/api/v1/settings/ldap", tag = "ldap",
    request_body = LdapConfigInput,
    responses(
        (status = 204, description = "Saved"),
        (status = 400, description = "The configuration is not usable; the message names the field", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks the user-administration permission", body = super::error::ErrorBody),
        (status = 503, description = "This deployment has no directory store", body = super::error::ErrorBody),
    ),
)]
async fn put_ldap_config(
    _guard: RequireManageUsers,
    State(st): State<ApiState>,
    ldap: Ldap,
    Json(body): Json<LdapConfigInput>,
) -> ApiResult<StatusCode> {
    // Read the previous state before writing, so a true→false transition can be detected.
    let was_enabled = ldap.is_enabled().await.unwrap_or(false);
    let enabling = body.enabled;
    ldap.save(&body)
        .await
        // Every failure `save` can return before it touches the database is the caller's own
        // configuration, phrased for them. A constraint violation would arrive here too, which is
        // why the edge validates first: a CHECK surfacing as a 500 is a worse version of this 400.
        .map_err(|e| ApiError::bad_request("invalid_ldap_config", e.to_string()))?;

    if was_enabled && !enabling {
        // Switching the directory off is disabling every account that came from it, all at once.
        // Without this their sessions keep working until they expire, and api-conventions is
        // explicit that a disabling action which does not revoke leaves a stolen token outliving
        // the action taken to stop it.
        match ldap.account_ids().await {
            Ok(ids) => {
                for id in ids {
                    st.sessions.revoke_user(id);
                }
            }
            Err(e) => {
                tracing::error!(
                    error = %e,
                    "directory login was disabled but its accounts' sessions could not be revoked"
                );
            }
        }
    }
    Ok(StatusCode::NO_CONTENT)
}

/// Exercise the **saved** configuration against the directory.
#[utoipa::path(
    post, path = "/api/v1/settings/ldap/test", tag = "ldap",
    request_body = LdapTestBody,
    responses(
        (status = 200, description = "How far the check got; a failed stage is reported here, not as an error status", body = LdapTestResult),
        (status = 400, description = "No directory configuration has been saved yet", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks the user-administration permission", body = super::error::ErrorBody),
        (status = 503, description = "This deployment has no directory store", body = super::error::ErrorBody),
    ),
)]
async fn test_ldap_config(
    _guard: RequireManageUsers,
    ldap: Ldap,
    Json(body): Json<LdapTestBody>,
) -> ApiResult<Json<LdapTestResult>> {
    // The **saved** row, ignoring `enabled` — validating a directory before switching it on is the
    // whole point of the button (the same reasoning as the LLM and forwarding-destination tests).
    // Testing a submitted body instead would need the bind password in the request and would turn
    // this into a general-purpose outbound LDAP client aimed at any host the caller names.
    let cfg = ldap
        .configured()
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "load the directory configuration",
                "failed to load the directory configuration",
            )
        })?
        .ok_or_else(|| {
            ApiError::bad_request(
                "ldap_not_configured",
                "save the directory configuration before testing it",
            )
        })?;
    let username = body
        .username
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    // A reachable-but-wrong directory answers 200 with a failed stage, never a 5xx: the
    // configuration is the caller's, and the staged result is what tells them which part of it is
    // wrong.
    Ok(Json(crate::ldap::probe(&cfg, username).await))
}

#[cfg(test)]
mod tests {
    use super::super::tests_support::{private_state, public_state};
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;
    use yagra_common::{Principal, Role, Scope};

    fn get(path: &str, token: Option<&str>) -> Request<Body> {
        let mut b = Request::builder().uri(path).method("GET");
        if let Some(t) = token {
            b = b.header("authorization", format!("Bearer {t}"));
        }
        b.body(Body::empty()).expect("request")
    }

    // Authorization before availability, and closed even on a public dashboard: `public_dashboard`
    // opens reads of monitoring data, never the configuration that decides who may sign in.
    #[tokio::test]
    async fn directory_settings_are_gated_before_availability() {
        for st in [private_state(), public_state()] {
            let app = super::super::router(st);
            let res = app
                .oneshot(get("/api/v1/settings/ldap", None))
                .await
                .expect("response");
            assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        }
    }

    #[tokio::test]
    async fn only_manage_users_may_configure_the_directory() {
        for role in [Role::Viewer, Role::Operator] {
            let st = private_state();
            let token =
                st.sessions
                    .issue(uuid::Uuid::new_v4(), Principal::new(role, Scope::All), "u");
            let app = super::super::router(st);
            let res = app
                .oneshot(get("/api/v1/settings/ldap", Some(&token)))
                .await
                .expect("response");
            assert_eq!(
                res.status(),
                StatusCode::FORBIDDEN,
                "{role:?} must be 403, not 401 — the two have to stay distinguishable"
            );
        }
    }

    // The test button reads other people's DNs and group memberships out of the directory, so it is
    // gated exactly like the configuration itself rather than as a lesser "diagnostic".
    #[tokio::test]
    async fn the_test_button_is_gated_the_same_as_the_configuration() {
        let st = private_state();
        let token = st.sessions.issue(
            uuid::Uuid::new_v4(),
            Principal::new(Role::Operator, Scope::All),
            "op",
        );
        let app = super::super::router(st);
        let req = Request::builder()
            .uri("/api/v1/settings/ldap/test")
            .method("POST")
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .body(Body::from("{}"))
            .expect("request");
        let res = app.oneshot(req).await.expect("response");
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
    }

    // Skeleton mode has no directory store, so an *authorized* admin gets the typed 503 — which is
    // the answer the 401s above must not have been.
    #[tokio::test]
    async fn an_admin_gets_the_typed_unavailable_when_there_is_no_store() {
        let st = private_state();
        let token = st.sessions.issue(
            uuid::Uuid::new_v4(),
            Principal::new(Role::Admin, Scope::All),
            "admin",
        );
        let app = super::super::router(st);
        let res = app
            .oneshot(get("/api/v1/settings/ldap", Some(&token)))
            .await
            .expect("response");
        assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
    // ── An accepted write (ADR-115) ──────────────────────────────────────────────────

    /// The directory configuration is stored, and its bind password never comes back out.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn storing_the_directory_config_seals_the_bind_password(pool: sqlx::PgPool) {
        use crate::api::tests_support::{live_state, send, token};
        let st = live_state(pool.clone()).await;
        let tok = token(&st, yagra_common::Role::Admin);
        let (status, body) = send(
            &st,
            "PUT",
            "/api/v1/settings/ldap",
            &tok,
            Some(serde_json::json!({
                "host": "dc01.corp.invalid",
                "bind_dn": "cn=yagra,ou=svc,dc=corp",
                "bind_password": "s3cr3t-bind",
                "user_base_dn": "ou=people,dc=corp",
            })),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::NO_CONTENT, "{body}");
        assert_eq!(crate::pgtest::rows(&pool, "ldap_config").await, 1);

        let (status, read) = send(&st, "GET", "/api/v1/settings/ldap", &tok, None).await;
        assert_eq!(status, axum::http::StatusCode::OK, "{read}");
        assert!(
            !read.to_string().contains("s3cr3t-bind"),
            "the bind password came back out"
        );
    }
}
