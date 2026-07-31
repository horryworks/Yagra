// SPDX-License-Identifier: AGPL-3.0-only
//! Notification channels and the routing rules that pick which alerts reach them.
//!
//! `ManageConfig` throughout, reads included: a channel holds a sealed credential and a rule
//! decides who gets woken up.
//!
//! **The URL validation here is a security boundary, not a typo check.** Core holds the database
//! and the KEK, and it is core that makes the outbound request on every alert. So a `ManageConfig`
//! user must not be able to aim a channel at `http://169.254.169.254/…` and have core fetch cloud
//! metadata for them ([`validate_webhook_url`]), nor point a *sealed vendor credential* at a server
//! of their choosing ([`validate_vendor_url`], exact-host allowlist). The delivery path re-checks
//! resolved addresses as well — defence in depth, since DNS can change between the two.

use super::error::{ApiError, ApiResult};
use super::extract::{Admin, RequireManageConfig};
use super::util::{CreatedId, EnabledBody};
use super::ApiState;
use crate::notifications::ChannelConfig;
use axum::{
    extract::Path,
    http::StatusCode,
    routing::{get, put},
    Json, Router,
};
use serde::Deserialize;
use uuid::Uuid;
use yagra_common::{is_ssrf_blocked, Severity};

/// This domain's slice of the OpenAPI document (ADR-035), merged by [`super::openapi::document`].
#[derive(utoipa::OpenApi)]
#[openapi(paths(
    list_notification_channels,
    create_notification_channel,
    set_notification_channel_enabled,
    delete_notification_channel,
    list_routing_rules,
    create_routing_rule,
    set_routing_rule_enabled,
    delete_routing_rule
))]
pub(super) struct Doc;

/// The notification routes, merged into `/api/v1` by [`super::router`].
pub(super) fn routes() -> Router<ApiState> {
    Router::new()
        .route(
            "/api/v1/notification-channels",
            get(list_notification_channels).post(create_notification_channel),
        )
        .route(
            "/api/v1/notification-channels/:id",
            put(set_notification_channel_enabled).delete(delete_notification_channel),
        )
        .route(
            "/api/v1/routing-rules",
            get(list_routing_rules).post(create_routing_rule),
        )
        .route(
            "/api/v1/routing-rules/:id",
            put(set_routing_rule_enabled).delete(delete_routing_rule),
        )
}

/// Exact-host allowlists for the fixed-vendor channels.
///
/// Exact match, never suffix match: suffix matching is how allowlist bypasses happen, because
/// `events.pagerduty.com.attacker.io` ends with nothing the naive check looks for but resolves
/// wherever the attacker likes.
const PAGERDUTY_HOSTS: &[&str] = &["events.pagerduty.com", "events.eu.pagerduty.com"];
const JSM_HOSTS: &[&str] = &[
    "api.atlassian.com",
    "api.opsgenie.com",
    "api.eu.opsgenie.com",
];

/// Validate a fixed-vendor API URL: https only, host exactly in that vendor's allowlist.
///
/// Stricter than the generic webhook check because the credential is sealed and sent on every
/// alert: without the allowlist, a `ManageConfig` user could point a PagerDuty routing key at a
/// server they control and harvest it.
fn validate_vendor_url(url: &str, allowed_hosts: &[&str]) -> Result<(), &'static str> {
    let url = url.trim();
    if url.is_empty() {
        return Err("API URL required");
    }
    let parsed = reqwest::Url::parse(url).map_err(|_| "API URL is not a valid URL")?;
    if parsed.scheme() != "https" {
        return Err("API URL must be https");
    }
    let Some(host) = parsed.host_str() else {
        return Err("API URL must have a host");
    };
    if !allowed_hosts.contains(&host) {
        return Err("API URL host is not an allowed vendor endpoint");
    }
    Ok(())
}

/// Validate a notification-webhook URL at the API edge (SSRF).
///
/// Private ranges are deliberately *allowed* — an internal collector is a legitimate webhook target.
/// What is refused is the escalation surface: loopback, link-local (which is where cloud metadata
/// lives), multicast and unspecified. `host_ip` unwraps the IPv6 bracket form, so `[::ffff:169.254.
/// 169.254]` is caught too.
fn validate_webhook_url(url: &str) -> Result<(), &'static str> {
    let url = url.trim();
    if url.is_empty() {
        return Err("webhook url required");
    }
    let parsed = reqwest::Url::parse(url).map_err(|_| "webhook url is not a valid URL")?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err("webhook url scheme must be http or https");
    }
    let Some(host) = parsed.host_str() else {
        return Err("webhook url must have a host");
    };
    if let Some(ip) = yagra_common::host_ip(host) {
        if is_ssrf_blocked(ip) {
            return Err("webhook url target is not allowed (loopback / link-local / metadata)");
        }
    }
    Ok(())
}

/// Validate a channel's connection config at the API edge.
fn validate_channel_config(c: &ChannelConfig) -> Result<(), &'static str> {
    match c {
        ChannelConfig::Webhook { url } => validate_webhook_url(url),
        ChannelConfig::Email { host, from, to, .. }
            if host.trim().is_empty() || from.trim().is_empty() || to.trim().is_empty() =>
        {
            Err("email host/from/to required")
        }
        ChannelConfig::PagerDuty {
            routing_key,
            api_url,
        } => {
            if routing_key.trim().is_empty() {
                return Err("PagerDuty routing key required");
            }
            match api_url.as_deref() {
                None => Ok(()),
                Some(url) => validate_vendor_url(url, PAGERDUTY_HOSTS),
            }
        }
        ChannelConfig::Jsm { api_url, api_key } => {
            if api_key.trim().is_empty() {
                return Err("JSM API key required");
            }
            validate_vendor_url(api_url, JSM_HOSTS)
        }
        _ => Ok(()),
    }
}

/// Parse an optional severity token (absent = any). `Err` ⇒ unknown token.
fn parse_severity_opt(s: Option<&str>) -> Result<Option<Severity>, ()> {
    match s {
        None => Ok(None),
        Some("critical") => Ok(Some(Severity::Critical)),
        Some("warning") => Ok(Some(Severity::Warning)),
        Some("info") => Ok(Some(Severity::Info)),
        Some(_) => Err(()),
    }
}

#[utoipa::path(
    get, path = "/api/v1/notification-channels", tag = "notifications",
    responses(
        (status = 200, description = "Every channel, without its sealed connection config", body = Vec<crate::notifications::ChannelSummary>),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role below Admin", body = super::error::ErrorBody),
        (status = 503, description = "This core has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn list_notification_channels(
    _guard: RequireManageConfig,
    admin: Admin,
) -> ApiResult<Json<Vec<crate::notifications::ChannelSummary>>> {
    let list = admin.notifications.list_channels().await.map_err(|e| {
        ApiError::from_internal(
            e.as_ref(),
            "list notification channels",
            "failed to list notification channels",
        )
    })?;
    Ok(Json(list))
}

/// Create-channel body: a name plus the (secret-bearing) connection config, tagged by `kind`.
#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct CreateChannel {
    name: String,
    config: ChannelConfig,
}

#[utoipa::path(
    post, path = "/api/v1/notification-channels", tag = "notifications",
    request_body = CreateChannel,
    responses(
        (status = 201, description = "Channel created", body = CreatedId),
        (status = 400, description = "Empty name, or a connection config whose URL fails the SSRF / vendor-allowlist check", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role below Admin", body = super::error::ErrorBody),
        (status = 503, description = "This core has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn create_notification_channel(
    _guard: RequireManageConfig,
    admin: Admin,
    Json(body): Json<CreateChannel>,
) -> ApiResult<(StatusCode, Json<CreatedId>)> {
    let name = body.name.trim();
    if name.is_empty() {
        return Err(ApiError::bad_request(
            "invalid_channel",
            "name must not be empty",
        ));
    }
    validate_channel_config(&body.config)
        .map_err(|msg| ApiError::bad_request("invalid_channel", msg))?;
    let id = admin
        .notifications
        .create_channel(name, &body.config)
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "create notification channel",
                "failed to create notification channel",
            )
        })?;
    Ok((StatusCode::CREATED, Json(CreatedId { id })))
}

#[utoipa::path(
    put, path = "/api/v1/notification-channels/{id}", tag = "notifications",
    params(("id" = Uuid, Path, description = "Channel id")),
    request_body = EnabledBody,
    responses(
        (status = 204, description = "Channel enabled or disabled"),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role below Admin", body = super::error::ErrorBody),
        (status = 404, description = "No such channel", body = super::error::ErrorBody),
        (status = 503, description = "This core has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn set_notification_channel_enabled(
    _guard: RequireManageConfig,
    admin: Admin,
    Path(id): Path<Uuid>,
    Json(body): Json<EnabledBody>,
) -> ApiResult<StatusCode> {
    match admin
        .notifications
        .set_channel_enabled(id, body.enabled)
        .await
    {
        Ok(true) => Ok(StatusCode::NO_CONTENT),
        Ok(false) => Err(ApiError::not_found(
            "channel_not_found",
            format!("no channel {id}"),
        )),
        Err(e) => Err(ApiError::from_internal(
            e.as_ref(),
            "update notification channel",
            "failed to update notification channel",
        )),
    }
}

#[utoipa::path(
    delete, path = "/api/v1/notification-channels/{id}", tag = "notifications",
    params(("id" = Uuid, Path, description = "Channel id")),
    responses(
        (status = 204, description = "Channel deleted"),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role below Admin", body = super::error::ErrorBody),
        (status = 404, description = "No such channel", body = super::error::ErrorBody),
        (status = 503, description = "This core has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn delete_notification_channel(
    _guard: RequireManageConfig,
    admin: Admin,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    match admin.notifications.delete_channel(id).await {
        Ok(true) => Ok(StatusCode::NO_CONTENT),
        Ok(false) => Err(ApiError::not_found(
            "channel_not_found",
            format!("no channel {id}"),
        )),
        Err(e) => Err(ApiError::from_internal(
            e.as_ref(),
            "delete notification channel",
            "failed to delete notification channel",
        )),
    }
}

#[utoipa::path(
    get, path = "/api/v1/routing-rules", tag = "notifications",
    responses(
        (status = 200, description = "Every routing rule and the channels it fans out to", body = Vec<crate::notifications::RoutingRule>),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role below Admin", body = super::error::ErrorBody),
        (status = 503, description = "This core has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn list_routing_rules(
    _guard: RequireManageConfig,
    admin: Admin,
) -> ApiResult<Json<Vec<crate::notifications::RoutingRule>>> {
    let list = admin.notifications.list_rules().await.map_err(|e| {
        ApiError::from_internal(
            e.as_ref(),
            "list routing rules",
            "failed to list routing rules",
        )
    })?;
    Ok(Json(list))
}

/// Create-rule body: a name, an optional severity filter (absent = any), and target channels.
#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct CreateRule {
    name: String,
    severity: Option<String>,
    channel_ids: Vec<Uuid>,
}

#[utoipa::path(
    post, path = "/api/v1/routing-rules", tag = "notifications",
    request_body = CreateRule,
    responses(
        (status = 201, description = "Rule created", body = CreatedId),
        (status = 400, description = "Empty name, or a severity outside critical|warning|info|null", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role below Admin", body = super::error::ErrorBody),
        (status = 503, description = "This core has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn create_routing_rule(
    _guard: RequireManageConfig,
    admin: Admin,
    Json(body): Json<CreateRule>,
) -> ApiResult<(StatusCode, Json<CreatedId>)> {
    let name = body.name.trim();
    if name.is_empty() {
        return Err(ApiError::bad_request(
            "invalid_rule",
            "name must not be empty",
        ));
    }
    let severity = parse_severity_opt(body.severity.as_deref()).map_err(|()| {
        ApiError::bad_request(
            "invalid_rule",
            "severity must be critical|warning|info or null",
        )
    })?;
    let id = admin
        .notifications
        .create_rule(name, severity, &body.channel_ids)
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "create routing rule",
                "failed to create routing rule",
            )
        })?;
    Ok((StatusCode::CREATED, Json(CreatedId { id })))
}

#[utoipa::path(
    put, path = "/api/v1/routing-rules/{id}", tag = "notifications",
    params(("id" = Uuid, Path, description = "Routing rule id")),
    request_body = EnabledBody,
    responses(
        (status = 204, description = "Rule enabled or disabled"),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role below Admin", body = super::error::ErrorBody),
        (status = 404, description = "No such rule", body = super::error::ErrorBody),
        (status = 503, description = "This core has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn set_routing_rule_enabled(
    _guard: RequireManageConfig,
    admin: Admin,
    Path(id): Path<Uuid>,
    Json(body): Json<EnabledBody>,
) -> ApiResult<StatusCode> {
    match admin.notifications.set_rule_enabled(id, body.enabled).await {
        Ok(true) => Ok(StatusCode::NO_CONTENT),
        Ok(false) => Err(ApiError::not_found(
            "rule_not_found",
            format!("no rule {id}"),
        )),
        Err(e) => Err(ApiError::from_internal(
            e.as_ref(),
            "update routing rule",
            "failed to update routing rule",
        )),
    }
}

#[utoipa::path(
    delete, path = "/api/v1/routing-rules/{id}", tag = "notifications",
    params(("id" = Uuid, Path, description = "Routing rule id")),
    responses(
        (status = 204, description = "Rule deleted"),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role below Admin", body = super::error::ErrorBody),
        (status = 404, description = "No such rule", body = super::error::ErrorBody),
        (status = 503, description = "This core has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn delete_routing_rule(
    _guard: RequireManageConfig,
    admin: Admin,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    match admin.notifications.delete_rule(id).await {
        Ok(true) => Ok(StatusCode::NO_CONTENT),
        Ok(false) => Err(ApiError::not_found(
            "rule_not_found",
            format!("no rule {id}"),
        )),
        Err(e) => Err(ApiError::from_internal(
            e.as_ref(),
            "delete routing rule",
            "failed to delete routing rule",
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
            ("GET", "/api/v1/notification-channels".to_owned()),
            ("POST", "/api/v1/notification-channels".to_owned()),
            ("PUT", format!("/api/v1/notification-channels/{ID}")),
            ("DELETE", format!("/api/v1/notification-channels/{ID}")),
            ("GET", "/api/v1/routing-rules".to_owned()),
            ("POST", "/api/v1/routing-rules".to_owned()),
            ("PUT", format!("/api/v1/routing-rules/{ID}")),
            ("DELETE", format!("/api/v1/routing-rules/{ID}")),
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
    async fn channels_and_rules_are_closed_to_everyone_below_admin() {
        // Reads included: a channel row describes where alerts go and a rule says who is woken.
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

    #[test]
    fn webhook_url_validation_blocks_ssrf_targets() {
        // Allowed: public endpoints and legitimate internal (private-range) collectors.
        assert!(validate_webhook_url("https://hooks.example.com/abc").is_ok());
        assert!(validate_webhook_url("http://10.0.0.5:8080/notify").is_ok());
        // Rejected: the escalation surface — loopback, cloud metadata, and the v4-mapped form of it.
        assert!(validate_webhook_url("http://169.254.169.254/latest/meta-data/").is_err());
        assert!(validate_webhook_url("http://127.0.0.1/hook").is_err());
        assert!(validate_webhook_url("http://[::ffff:169.254.169.254]/").is_err());
        // Rejected: bad scheme / empty / hostless.
        assert!(validate_webhook_url("ftp://example.com/x").is_err());
        assert!(validate_webhook_url("   ").is_err());
        assert!(validate_webhook_url("not a url").is_err());
    }

    #[test]
    fn vendor_url_allowlist_is_exact_host_https_only() {
        // PagerDuty: both regions pass; http and lookalike hosts fail.
        assert!(
            validate_vendor_url("https://events.pagerduty.com/v2/enqueue", PAGERDUTY_HOSTS).is_ok()
        );
        assert!(validate_vendor_url(
            "https://events.eu.pagerduty.com/v2/enqueue",
            PAGERDUTY_HOSTS
        )
        .is_ok());
        assert!(
            validate_vendor_url("http://events.pagerduty.com/v2/enqueue", PAGERDUTY_HOSTS).is_err()
        );
        // Suffix tricks must fail — this is why the check is exact-match, not `ends_with`.
        assert!(validate_vendor_url(
            "https://events.pagerduty.com.attacker.io/v2/enqueue",
            PAGERDUTY_HOSTS
        )
        .is_err());
        assert!(validate_vendor_url("https://evil.example/v2/enqueue", PAGERDUTY_HOSTS).is_err());

        // JSM: Atlassian + Opsgenie hosts pass.
        assert!(validate_vendor_url(
            "https://api.atlassian.com/jsm/ops/integration/v2",
            JSM_HOSTS
        )
        .is_ok());
        assert!(validate_vendor_url("https://api.opsgenie.com/v2", JSM_HOSTS).is_ok());
        assert!(validate_vendor_url("https://api.eu.opsgenie.com/v2", JSM_HOSTS).is_ok());
        assert!(validate_vendor_url("https://api.atlassian.com.evil.io/v2", JSM_HOSTS).is_err());

        // PD/JSM channel configs route through validate_channel_config.
        assert!(validate_channel_config(&ChannelConfig::PagerDuty {
            routing_key: "rk".into(),
            api_url: None,
        })
        .is_ok());
        assert!(validate_channel_config(&ChannelConfig::PagerDuty {
            routing_key: "  ".into(),
            api_url: None,
        })
        .is_err());
        assert!(validate_channel_config(&ChannelConfig::Jsm {
            api_url: "https://api.atlassian.com/jsm/ops/integration/v2".into(),
            api_key: "k".into(),
        })
        .is_ok());
        assert!(validate_channel_config(&ChannelConfig::Jsm {
            api_url: "https://example.com/".into(),
            api_key: "k".into(),
        })
        .is_err());
    }

    #[test]
    fn severity_filter_accepts_only_the_three_tokens_or_none() {
        assert_eq!(parse_severity_opt(None), Ok(None));
        assert_eq!(
            parse_severity_opt(Some("critical")),
            Ok(Some(Severity::Critical))
        );
        assert_eq!(
            parse_severity_opt(Some("warning")),
            Ok(Some(Severity::Warning))
        );
        assert_eq!(parse_severity_opt(Some("info")), Ok(Some(Severity::Info)));
        // An unknown token is rejected rather than silently meaning "any" — a typo'd filter that
        // widened to every severity would page people for everything.
        assert_eq!(parse_severity_opt(Some("CRITICAL")), Err(()));
        assert_eq!(parse_severity_opt(Some("")), Err(()));
    }
}
