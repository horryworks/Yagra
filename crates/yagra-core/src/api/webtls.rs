// SPDX-License-Identifier: AGPL-3.0-only
//! The WebUI's TLS certificate (ADR-044) — Settings ▸ TLS.
//!
//! Three endpoints: read what is being served, import a certificate, regenerate the self-signed one.
//!
//! All three take [`RequireManageSystem`] — this is deployment infrastructure, the same class as
//! retention and the polling defaults, and an Admin is unscoped by construction so there is nothing
//! for group scoping to narrow. Authorization runs **before** availability (`Require*` first, then
//! the [`WebTls`] extractor) so an unauthenticated prober cannot learn anything about how this
//! deployment is configured from the difference between 401 and 503.
//!
//! **The response carries the certificate and never the key.** That asymmetry is the feature: the
//! chain has to come back so the UI can show what is being served *and* offer it for download —
//! which is how an operator running on the self-signed bootstrap gets it into a Prometheus
//! `ca_file` or an OS trust store. The private key has no such use and never leaves the server.

use axum::{
    extract::State,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use utoipa::OpenApi;

use super::error::{ApiError, ApiResult};
use super::extract::{Caller, RequireManageSystem, WebTls};
use super::ApiState;
use crate::server_cert::CertError;
use crate::webtls::WebTlsView;

#[derive(OpenApi)]
#[openapi(paths(get_web_tls, put_web_tls, regenerate_web_tls))]
pub(super) struct Doc;

pub(super) fn routes() -> Router<ApiState> {
    Router::new()
        .route("/api/v1/settings/tls", get(get_web_tls).put(put_web_tls))
        .route("/api/v1/settings/tls/regenerate", post(regenerate_web_tls))
}

/// The WebUI's TLS certificate, plus one fact about how this deployment is exposed.
//
// Every `///` in this file is published verbatim to API clients and into the generated site
// reference, so rationale goes in `//` notes like this one.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct WebTlsResponse {
    /// The certificate being served, or `null` if none has been established yet.
    // `null` rather than a 404: a core still finishing its first startup has none, and the form
    // should render its waiting state rather than an error.
    config: Option<WebTlsView>,
    /// Whether Yagra's API port is reachable beyond the server itself. When `true`, the API is also
    /// available over plain HTTP on that port, so the encrypted WebUI is not the only way in.
    // The one sentence the page must be able to say. An operator cannot tell from the UI otherwise,
    // and ADR-044 decision 9 makes closing it the documented step after importing a certificate.
    api_port_is_public: bool,
}

/// A certificate chain and its private key, in PEM.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub(crate) struct WebTlsImport {
    /// The full certificate chain in PEM, leaf first. A private key in this field is rejected —
    /// send it in `private_key`.
    certificate: String,
    /// The matching private key in PEM: PKCS#8, PKCS#1 or SEC1, and not passphrase-protected.
    /// Never returned by any endpoint.
    private_key: String,
}

/// Which names a regenerated self-signed certificate should cover.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub(crate) struct WebTlsRegenerate {
    /// Hostnames and IP addresses to put in the subject alternative name. If empty, the deployment's
    /// defaults are used — loopback plus the server's own hostname, which is rarely the address
    /// browsers actually use.
    #[serde(default)]
    names: Vec<String>,
}

/// The certificate the WebUI is serving. Never includes the private key.
#[utoipa::path(
    get, path = "/api/v1/settings/tls", tag = "tls",
    responses(
        (status = 200, description = "The current certificate, or null if none has been established yet", body = WebTlsResponse),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageSystem", body = super::error::ErrorBody),
        (status = 503, description = "This deployment has no certificate store", body = super::error::ErrorBody),
    ),
)]
async fn get_web_tls(_guard: RequireManageSystem, tls: WebTls) -> ApiResult<Json<WebTlsResponse>> {
    let config = tls.view().await.map_err(|e| {
        ApiError::from_internal(
            e.as_ref(),
            "read the TLS certificate",
            "failed to read the TLS certificate",
        )
    })?;
    Ok(Json(WebTlsResponse {
        config,
        api_port_is_public: crate::webtls::api_port_is_public(),
    }))
}

/// Import a certificate and its private key.
#[utoipa::path(
    put, path = "/api/v1/settings/tls", tag = "tls",
    request_body = WebTlsImport,
    responses(
        (status = 200, description = "Imported and live; the body is the new certificate's details", body = WebTlsResponse),
        (status = 400, description = "The pair is not usable; the message says which of the checks it failed", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageSystem", body = super::error::ErrorBody),
        (status = 503, description = "This deployment has no certificate store", body = super::error::ErrorBody),
    ),
)]
async fn put_web_tls(
    _guard: RequireManageSystem,
    State(_st): State<ApiState>,
    tls: WebTls,
    caller: Option<Caller>,
    Json(body): Json<WebTlsImport>,
) -> ApiResult<Json<WebTlsResponse>> {
    // `Option<Caller>` rather than `Caller`: an API token is a legitimate way to automate this, and
    // requiring a browser session would rule out a renewal script. It only costs the "imported by"
    // attribution, which is then honestly blank rather than wrong.
    let by = caller.map(|c| c.0.user_id);
    let config = tls
        .import(&body.certificate, &body.private_key, by)
        .await
        .map_err(to_api_error)?;
    Ok(Json(WebTlsResponse {
        config: Some(config),
        api_port_is_public: crate::webtls::api_port_is_public(),
    }))
}

/// Generate a new self-signed certificate, replacing whatever is being served.
#[utoipa::path(
    post, path = "/api/v1/settings/tls/regenerate", tag = "tls",
    request_body = WebTlsRegenerate,
    responses(
        (status = 200, description = "Generated and live; the body is the new certificate's details", body = WebTlsResponse),
        (status = 400, description = "A supplied name is not a usable hostname or IP address", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageSystem", body = super::error::ErrorBody),
        (status = 503, description = "This deployment has no certificate store", body = super::error::ErrorBody),
    ),
)]
async fn regenerate_web_tls(
    _guard: RequireManageSystem,
    tls: WebTls,
    caller: Option<Caller>,
    Json(body): Json<WebTlsRegenerate>,
) -> ApiResult<Json<WebTlsResponse>> {
    let names: Vec<String> = body
        .names
        .iter()
        .map(|n| n.trim().to_owned())
        .filter(|n| !n.is_empty())
        .collect();
    let names = if names.is_empty() {
        crate::webtls::configured_names()
    } else {
        names
    };
    let by = caller.map(|c| c.0.user_id);
    let config = tls.regenerate(&names, by).await.map_err(to_api_error)?;
    Ok(Json(WebTlsResponse {
        config: Some(config),
        api_port_is_public: crate::webtls::api_port_is_public(),
    }))
}

/// Map a store failure to a status.
///
/// Everything [`CertError`] describes is the operator's own paste, so it is a 400 carrying the
/// module's own text — the whole value of validating at import time is that the message says *which*
/// check failed instead of the browser saying the site is down half an hour later. Anything else is
/// the database, and says nothing about itself (security.md).
fn to_api_error(e: anyhow::Error) -> ApiError {
    match e.downcast_ref::<CertError>() {
        Some(cert) => ApiError::bad_request("invalid_certificate", cert.to_string()),
        None => ApiError::from_internal(
            e.as_ref(),
            "store the TLS certificate",
            "failed to store the TLS certificate",
        ),
    }
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
    // opens reads of monitoring data, never the deployment's own TLS material.
    #[tokio::test]
    async fn tls_settings_are_gated_before_availability() {
        for st in [private_state(), public_state()] {
            let app = super::super::router(st);
            let res = app
                .oneshot(get("/api/v1/settings/tls", None))
                .await
                .expect("response");
            assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        }
    }

    #[tokio::test]
    async fn only_manage_config_may_read_or_change_the_certificate() {
        for role in [Role::Viewer, Role::Operator] {
            let st = private_state();
            let token =
                st.sessions
                    .issue(uuid::Uuid::new_v4(), Principal::new(role, Scope::All), "u");
            let app = super::super::router(st);
            let res = app
                .oneshot(get("/api/v1/settings/tls", Some(&token)))
                .await
                .expect("response");
            assert_eq!(
                res.status(),
                StatusCode::FORBIDDEN,
                "{role:?} must be 403, not 401 — the two have to stay distinguishable"
            );
        }
    }

    #[tokio::test]
    async fn the_import_endpoint_is_gated_the_same_as_the_read() {
        let st = private_state();
        let token = st.sessions.issue(
            uuid::Uuid::new_v4(),
            Principal::new(Role::Operator, Scope::All),
            "op",
        );
        let app = super::super::router(st);
        let req = Request::builder()
            .uri("/api/v1/settings/tls")
            .method("PUT")
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .body(Body::from(r#"{"certificate":"x","private_key":"y"}"#))
            .expect("request");
        let res = app.oneshot(req).await.expect("response");
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
    }

    // Skeleton mode has no certificate store, so an *authorized* admin gets the typed 503 — which is
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
            .oneshot(get("/api/v1/settings/tls", Some(&token)))
            .await
            .expect("response");
        assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    /// The structural form of "the private key never leaves the server".
    ///
    /// Reads this module's own source rather than a response body, because the response that would
    /// prove it needs a database. What it can prove without one is that the *shape* returned here
    /// has no key-bearing field — the mistake would be adding one to `WebTlsView` for the UI's
    /// convenience, which is exactly the kind of change that looks harmless in review.
    #[test]
    fn no_response_shape_in_this_feature_carries_key_material() {
        let src = include_str!("../webtls.rs");
        let view = src
            .split("pub struct WebTlsView {")
            .nth(1)
            .and_then(|s| s.split('}').next())
            .expect("WebTlsView is declared in webtls.rs");
        for banned in [
            "private_key",
            "key_pem",
            "secret",
            "wrapped_dek",
            "ciphertext",
        ] {
            assert!(
                !view.contains(banned),
                "WebTlsView has a `{banned}` field — the private key must never be serialized"
            );
        }
        // `key_algorithm` and `key_unreadable` are about the key without being it; if the needles
        // above ever start matching those, this test has stopped meaning anything.
        assert!(view.contains("key_algorithm"), "the view lost its metadata");
    }
}
