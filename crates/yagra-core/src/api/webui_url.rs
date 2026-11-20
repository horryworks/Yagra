// SPDX-License-Identifier: AGPL-3.0-only
//! The WebUI address: where operators open this deployment, so a notification can link back to it
//! (ADR-260).
//!
//! Yagra cannot learn this by itself. Core listens behind nginx, often behind a reverse proxy or a
//! NAT as well, and the address a person types is a fact about their network, not about the
//! process. So an operator states it once, and the notification path builds `node_url` from it.
//!
//! Not part of `GET /api/v1/config`: that endpoint is the unauthenticated bootstrap blob, and an
//! internal host name is not something to hand to a stranger.

use super::error::{ApiError, ApiResult};
use super::extract::{Admin, RequireManageConfig, RequireView, Scoped};
use super::ApiState;
use axum::{http::StatusCode, routing::get, Json, Router};
use serde::{Deserialize, Serialize};

/// This domain's slice of the OpenAPI document (ADR-035), merged by [`super::openapi::document`].
#[derive(utoipa::OpenApi)]
#[openapi(paths(get_webui_url, set_webui_url))]
pub(super) struct Doc;

/// The WebUI-address routes, merged into `/api/v1` by [`super::router`].
pub(super) fn routes() -> Router<ApiState> {
    Router::new().route(
        "/api/v1/settings/webui-url",
        get(get_webui_url).put(set_webui_url),
    )
}

/// The longest address accepted. A URL a person types into a browser is far shorter. The cap is
/// set so a whole node link (this, `/nodes/` and a 36-character id) stays under the length a JSM
/// detail is cut at (`notify_text::DETAIL_VALUE_MAX_CHARS`): a link cut short is a broken one.
pub(crate) const MAX_WEBUI_URL_LEN: usize = 400;

/// The address operators open the WebUI at.
#[derive(Debug, Serialize, Deserialize, utoipa::ToSchema)]
pub(crate) struct WebuiUrl {
    /// An absolute `http`/`https` URL with no query and no fragment, without a trailing `/`
    /// (for example `https://yagra.example.com`). `null` when not set; notifications then carry no
    /// link to Yagra.
    pub url: Option<String>,
}

#[utoipa::path(
    get, path = "/api/v1/settings/webui-url", tag = "settings",
    responses(
        (status = 200, description = "The WebUI address notifications link to, or null when not set", body = WebuiUrl),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks View", body = super::error::ErrorBody),
        (status = 503, description = "Inventory storage is unavailable (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn get_webui_url(_guard: RequireView, admin: Admin) -> ApiResult<Json<WebuiUrl>> {
    Ok(Json(current(&admin).await))
}

/// The stored address — the seam both edges call (REST here, MCP's `get_config`).
pub(crate) async fn current(admin: &super::AdminState) -> WebuiUrl {
    WebuiUrl {
        url: admin.repo.get_webui_url().await,
    }
}

#[utoipa::path(
    put, path = "/api/v1/settings/webui-url", tag = "settings",
    request_body = WebuiUrl,
    responses(
        (status = 204, description = "Saved; notifications sent from the next routing refresh (within 30 seconds) carry links built from it. An empty or null `url` clears it"),
        (status = 400, description = "Not an absolute http/https URL, or it carries a query, a fragment or credentials, or it is too long (`invalid_webui_url`)", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig, or the account is restricted to folders (`scope_unsupported`)", body = super::error::ErrorBody),
        (status = 503, description = "Inventory storage is unavailable (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn set_webui_url(
    _guard: RequireManageConfig,
    Scoped(scope): Scoped,
    admin: Admin,
    Json(body): Json<WebuiUrl>,
) -> ApiResult<StatusCode> {
    super::scope::require_fleet_wide(
        &scope,
        "the WebUI address is one setting for the whole deployment, so an account restricted to \
         folders cannot change it",
    )?;
    let url = normalize(body.url.as_deref().unwrap_or_default())
        .map_err(|why| ApiError::bad_request("invalid_webui_url", why))?;
    admin
        .repo
        .set_webui_url(url.as_deref())
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "set webui url",
                "failed to update the WebUI address",
            )
        })?;
    Ok(StatusCode::NO_CONTENT)
}

/// Turn what an operator typed into the stored form, or say why it cannot be one.
///
/// Blank clears the setting. Otherwise: an absolute `http`/`https` URL with a host, no user name or
/// password, no query and no fragment; a path is allowed (a deployment behind a reverse proxy may
/// live under one) and every trailing `/` is dropped, so `{url}/nodes/…` never doubles the slash.
pub(crate) fn normalize(raw: &str) -> Result<Option<String>, &'static str> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    if trimmed.len() > MAX_WEBUI_URL_LEN {
        return Err("the WebUI address is too long");
    }
    let parsed = reqwest::Url::parse(trimmed).map_err(|_| {
        "the WebUI address must be an absolute URL such as https://yagra.example.com"
    })?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err("the WebUI address must start with http:// or https://");
    }
    if parsed.host_str().is_none_or(str::is_empty) {
        return Err("the WebUI address must name a host");
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err("the WebUI address must not carry a user name or password");
    }
    if parsed.query().is_some() || parsed.fragment().is_some() {
        return Err("the WebUI address must not carry a query (?) or a fragment (#)");
    }
    // The parser's spelling, not the operator's: it lower-cases the scheme and host and writes a
    // bare origin as `https://host/`, which the trim below turns back into `https://host`.
    Ok(Some(parsed.as_str().trim_end_matches('/').to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_origin_is_stored_without_its_trailing_slash() {
        assert_eq!(
            normalize(" https://Yagra.Example.com/ "),
            Ok(Some("https://yagra.example.com".to_owned()))
        );
        assert_eq!(
            normalize("http://192.0.2.10:8443"),
            Ok(Some("http://192.0.2.10:8443".to_owned()))
        );
        assert_eq!(
            normalize("https://[2001:db8::1]/"),
            Ok(Some("https://[2001:db8::1]".to_owned()))
        );
    }

    #[test]
    fn a_path_is_kept_for_a_deployment_behind_a_proxy() {
        assert_eq!(
            normalize("https://example.com/yagra//"),
            Ok(Some("https://example.com/yagra".to_owned()))
        );
    }

    #[test]
    fn the_longest_address_still_fits_a_whole_node_link_in_a_jsm_detail() {
        let link = MAX_WEBUI_URL_LEN + "/nodes/".len() + 36;
        assert!(link <= crate::alerts::notify_text::DETAIL_VALUE_MAX_CHARS);
    }

    #[test]
    fn blank_clears_the_setting() {
        assert_eq!(normalize(""), Ok(None));
        assert_eq!(normalize("   "), Ok(None));
    }

    #[test]
    fn anything_but_a_plain_http_url_is_refused() {
        for bad in [
            "yagra.example.com",
            "/nodes",
            "ftp://example.com",
            "javascript:alert(1)",
            "https://user:pw@example.com",
            "https://example.com/?a=1",
            "https://example.com/#top",
            "file:///etc/passwd",
        ] {
            assert!(normalize(bad).is_err(), "{bad} should be refused");
        }
        let long = format!("https://example.com/{}", "a".repeat(MAX_WEBUI_URL_LEN));
        assert!(normalize(&long).is_err());
    }

    // ── An accepted write (ADR-115) ──────────────────────────────────────────────────

    /// The address is stored in its normalized form, read back by GET, and cleared by a blank.
    #[sqlx::test(migrator = "yagra_base::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn the_webui_url_round_trips_and_clears(pool: sqlx::PgPool) {
        use crate::api::tests_support::{live_state, send, token};
        let st = live_state(pool.clone()).await;
        let tok = token(&st, yagra_common::Role::Admin);
        let (status, body) = send(
            &st,
            "PUT",
            "/api/v1/settings/webui-url",
            &tok,
            Some(serde_json::json!({ "url": "https://Yagra.Example.com/" })),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
        let (status, body) = send(&st, "GET", "/api/v1/settings/webui-url", &tok, None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["url"], "https://yagra.example.com");

        let (status, body) = send(
            &st,
            "PUT",
            "/api/v1/settings/webui-url",
            &tok,
            Some(serde_json::json!({ "url": "" })),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
        let admin = st.admin.clone().expect("live state");
        assert_eq!(admin.repo.get_webui_url().await, None);

        let (status, body) = send(
            &st,
            "PUT",
            "/api/v1/settings/webui-url",
            &tok,
            Some(serde_json::json!({ "url": "ftp://example.com" })),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(body["error"]["code"], "invalid_webui_url");
    }
}
