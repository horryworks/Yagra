// SPDX-License-Identifier: AGPL-3.0-only
//! Notification channels and the routing rules that pick which alerts reach them.
//!
//! `ManageSystem` throughout, reads included (ADR-057): a channel holds a sealed credential and a rule
//! decides who gets woken up.
//!
//! **The URL validation here is a security boundary, not a typo check.** Core holds the database
//! and the KEK, and it is core that makes the outbound request on every alert. So a `ManageConfig`
//! user must not be able to aim a channel at `http://169.254.169.254/…` and have core fetch cloud
//! metadata for them ([`validate_webhook_url`]), nor point a *sealed vendor credential* at a server
//! of their choosing ([`validate_vendor_url`], exact-host allowlist). The delivery path re-checks
//! resolved addresses as well — defence in depth, since DNS can change between the two.

use super::error::{ApiError, ApiResult};
use super::extract::{Admin, RequireManageSystem};
use super::util::{CreatedId, EnabledBody};
use super::ApiState;
use crate::notifications::{ChannelConfig, ChannelKind};
use crate::notify_render::ChannelTemplate;
use axum::{
    extract::Path,
    http::StatusCode,
    routing::{get, post, put},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use yagra_common::{is_ssrf_blocked, NotifyEvent, Severity};

/// This domain's slice of the OpenAPI document (ADR-035), merged by [`super::openapi::document`].
#[derive(utoipa::OpenApi)]
#[openapi(paths(
    list_notification_channels,
    create_notification_channel,
    set_notification_channel_enabled,
    delete_notification_channel,
    set_notification_template,
    preview_notification_template,
    list_template_variables,
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
        // Both static siblings of `:id` below; the router prefers a literal segment, the same way
        // `/meraki/orgs/discover` sits beside `/meraki/orgs/:id`.
        .route(
            "/api/v1/notification-channels/preview",
            post(preview_notification_template),
        )
        .route(
            "/api/v1/notification-channels/template-variables",
            get(list_template_variables),
        )
        .route(
            "/api/v1/notification-channels/:id",
            put(set_notification_channel_enabled).delete(delete_notification_channel),
        )
        // No matching GET: a channel's template comes back on the list, so the editor opens with
        // no extra round trip and the ledger gains no read that MCP would then have to answer for.
        .route(
            "/api/v1/notification-channels/:id/template",
            put(set_notification_template),
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
///
/// Exhaustive on purpose. A `_ =>` arm here fails **open**: a fifth channel kind would be accepted
/// unvalidated, and the first delivery attempt would be the first check the operator ever gets.
/// Every arm has to say what it accepts, even when the answer is "anything".
fn validate_channel_config(c: &ChannelConfig) -> Result<(), &'static str> {
    match c {
        ChannelConfig::Webhook { url } => validate_webhook_url(url),
        ChannelConfig::Email { host, from, to, .. } => {
            if host.trim().is_empty() || from.trim().is_empty() || to.trim().is_empty() {
                return Err("email host/from/to required");
            }
            // No host allow-list, unlike PagerDuty and JSM below: an SMTP relay is site-local by
            // nature, so there is no vendor endpoint to pin it to.
            Ok(())
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
    }
}

/// Parse an optional severity token (absent = any). `Err` ⇒ unknown token.
fn parse_severity_opt(s: Option<&str>) -> Result<Option<Severity>, ()> {
    match s {
        None => Ok(None),
        // Operator input: an unrecognised value is a rejection, never a silent default — a routing
        // rule that quietly matched a different severity than the one typed is a missed page.
        Some(raw) => Severity::from_token(raw).map(Some).ok_or(()),
    }
}

#[utoipa::path(
    get, path = "/api/v1/notification-channels", tag = "notifications",
    responses(
        (status = 200, description = "Every channel, without its sealed connection config", body = Vec<crate::notifications::ChannelSummary>),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageSystem", body = super::error::ErrorBody),
        (status = 503, description = "This core has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn list_notification_channels(
    _guard: RequireManageSystem,
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
        (status = 403, description = "Role lacks ManageSystem", body = super::error::ErrorBody),
        (status = 503, description = "This core has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn create_notification_channel(
    _guard: RequireManageSystem,
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
        (status = 403, description = "Role lacks ManageSystem", body = super::error::ErrorBody),
        (status = 404, description = "No such channel", body = super::error::ErrorBody),
        (status = 503, description = "This core has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn set_notification_channel_enabled(
    _guard: RequireManageSystem,
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
        (status = 403, description = "Role lacks ManageSystem", body = super::error::ErrorBody),
        (status = 404, description = "No such channel", body = super::error::ErrorBody),
        (status = 503, description = "This core has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn delete_notification_channel(
    _guard: RequireManageSystem,
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

/// A channel's notification-template override. Both fields are replaced together; `null` or blank
/// on a field restores Yagra's built-in wording for it.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub(super) struct TemplateBody {
    #[serde(default)]
    subject: Option<String>,
    #[serde(default)]
    body: Option<String>,
}

impl TemplateBody {
    // Blank collapses to "built-in". An empty string is a template that renders to nothing, which
    // is a subject line an operator could set by clearing the field and then wonder where their
    // notifications went; the database column keeps NULL as the only "no override" value.
    fn into_template(self) -> ChannelTemplate {
        fn meaningful(s: Option<String>) -> Option<String> {
            s.filter(|s| !s.trim().is_empty())
        }
        ChannelTemplate {
            subject: meaningful(self.subject),
            body: meaningful(self.body),
        }
    }
}

/// Replace a channel's notification template.
///
/// A template that does not compile is rejected here rather than at delivery time — the operator is
/// still looking at the field. The renderer additionally falls back to the built-in format if a
/// stored template fails while an alert is being sent, so a broken template can never swallow a
/// notification.
#[utoipa::path(
    put, path = "/api/v1/notification-channels/{id}/template", tag = "notifications",
    params(("id" = Uuid, Path, description = "Channel id")),
    request_body = TemplateBody,
    responses(
        (status = 204, description = "Template saved (or cleared, restoring the built-in wording)"),
        (status = 400, description = "The template does not compile, or is longer than the accepted maximum", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageSystem", body = super::error::ErrorBody),
        (status = 404, description = "No such channel", body = super::error::ErrorBody),
        (status = 503, description = "This core has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn set_notification_template(
    _guard: RequireManageSystem,
    admin: Admin,
    Path(id): Path<Uuid>,
    Json(body): Json<TemplateBody>,
) -> ApiResult<StatusCode> {
    let template = body.into_template();
    check_template_size(&template)?;
    crate::notify_render::validate(&template).map_err(|e| {
        ApiError::bad_request(
            "invalid_template",
            format!("{} template: {}", e.field.as_str(), e.message),
        )
    })?;
    match admin
        .notifications
        .set_channel_template(id, &template)
        .await
    {
        Ok(true) => Ok(StatusCode::NO_CONTENT),
        Ok(false) => Err(ApiError::not_found(
            "channel_not_found",
            format!("no channel {id}"),
        )),
        Err(e) => Err(ApiError::from_internal(
            e.as_ref(),
            "update notification template",
            "failed to update notification template",
        )),
    }
}

/// Longest template *source* accepted, per field. Matches the table CHECKs in migration 0063, and
/// is deliberately looser than the cap on rendered output — a template can reasonably be longer
/// than what it produces.
const MAX_SUBJECT_SOURCE: usize = 4000;
const MAX_BODY_SOURCE: usize = 64_000;

/// Reject an over-long template at the edge, so the table CHECK is never what the operator sees.
fn check_template_size(template: &ChannelTemplate) -> Result<(), ApiError> {
    for (label, source, cap) in [
        ("subject", template.subject.as_deref(), MAX_SUBJECT_SOURCE),
        ("body", template.body.as_deref(), MAX_BODY_SOURCE),
    ] {
        if source.is_some_and(|s| s.chars().count() > cap) {
            return Err(ApiError::bad_request(
                "invalid_template",
                format!("{label} template is longer than the {cap}-character maximum"),
            ));
        }
    }
    Ok(())
}

/// A template to render against a representative alert.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub(super) struct PreviewRequest {
    /// The channel kind the template is for. Decides whether the body has to be valid JSON.
    kind: ChannelKind,
    /// Which point in an alert's life to render: `fire`, `resolve`, or `suppress`.
    #[serde(default = "default_event")]
    event: NotifyEvent,
    #[serde(default)]
    subject: Option<String>,
    #[serde(default)]
    body: Option<String>,
}

fn default_event() -> NotifyEvent {
    NotifyEvent::Fire
}

/// What the template produces, or what stopped it.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(super) struct PreviewResult {
    /// The rendered subject. Yagra's built-in wording when the subject is not overridden, or when
    /// rendering it failed — which is exactly what would be sent.
    subject: String,
    /// The rendered body, under the same rule.
    body: String,
    /// One entry per field that could not be rendered and fell back. Empty on success.
    problems: Vec<PreviewProblem>,
    /// Whether the rendered body parses as JSON. `null` when this channel kind sends the body as
    /// plain text, where the question does not apply.
    #[serde(skip_serializing_if = "Option::is_none")]
    json_valid: Option<bool>,
}

/// One field that could not be used.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(super) struct PreviewProblem {
    /// `subject` or `body`.
    field: String,
    /// `compile`, `render`, `too_large`, or `not_json`.
    reason: String,
    /// The engine's message, including the offending line where it knows it.
    message: String,
}

/// Render a template against a representative alert, without saving anything.
///
/// A template is code that first runs during an outage, so being able to see its output while
/// writing it is part of the feature rather than a convenience. Takes no channel id, so a template
/// can be checked before the channel it belongs to exists.
///
/// Problems come back **in the 200 response**, not as a 400: they are notes about the text being
/// typed, and a failed request would render as "the preview is broken" instead.
#[utoipa::path(
    post, path = "/api/v1/notification-channels/preview", tag = "notifications",
    request_body = PreviewRequest,
    responses(
        (status = 200, description = "What this template would send; a template that cannot be used is reported in-band alongside the built-in text that would go instead", body = PreviewResult),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageSystem", body = super::error::ErrorBody),
    ),
)]
async fn preview_notification_template(
    _guard: RequireManageSystem,
    Json(req): Json<PreviewRequest>,
) -> Json<PreviewResult> {
    let needs_json = crate::notify_render::body_must_be_json(req.kind);
    // The same sample alert, the same context builder and the same built-in wording the delivery
    // path uses — a preview that agreed only with a second copy of the rules would be worthless.
    let (alert, resolved) = crate::notify_facts::preview_sample();
    let facts = crate::notify_facts::context_for(&alert, req.event, &resolved);
    let builtin = crate::alerts::builtin_notification(&alert, req.event);
    let template = TemplateBody {
        subject: req.subject,
        body: req.body,
    }
    .into_template();
    let rendered = crate::notify_render::render_with_fallback(
        Some(&template),
        &facts,
        needs_json,
        &builtin.summary,
        &builtin.payload,
    );
    Json(PreviewResult {
        json_valid: needs_json
            .then(|| serde_json::from_str::<serde_json::Value>(&rendered.body).is_ok()),
        problems: rendered
            .failures
            .iter()
            .map(|f| PreviewProblem {
                field: f.field.as_str().to_owned(),
                reason: f.kind.as_str().to_owned(),
                message: f.message.clone(),
            })
            .collect(),
        subject: rendered.subject,
        body: rendered.body,
    })
}

/// Every variable a notification template can reference.
///
/// Served rather than documented so the editor's list and the renderer's context cannot disagree —
/// they are the same list.
#[utoipa::path(
    get, path = "/api/v1/notification-channels/template-variables", tag = "notifications",
    responses(
        (status = 200, description = "The template variables, with what each one means and whether every alert carries it", body = Vec<yagra_common::TemplateVariable>),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageSystem", body = super::error::ErrorBody),
    ),
)]
async fn list_template_variables(
    _guard: RequireManageSystem,
) -> Json<Vec<yagra_common::TemplateVariable>> {
    Json(yagra_common::TEMPLATE_VARIABLES.to_vec())
}

#[utoipa::path(
    get, path = "/api/v1/routing-rules", tag = "notifications",
    responses(
        (status = 200, description = "Every routing rule and the channels it fans out to", body = Vec<crate::notifications::RoutingRule>),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageSystem", body = super::error::ErrorBody),
        (status = 503, description = "This core has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn list_routing_rules(
    _guard: RequireManageSystem,
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
        (status = 403, description = "Role lacks ManageSystem", body = super::error::ErrorBody),
        (status = 503, description = "This core has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn create_routing_rule(
    _guard: RequireManageSystem,
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
        (status = 403, description = "Role lacks ManageSystem", body = super::error::ErrorBody),
        (status = 404, description = "No such rule", body = super::error::ErrorBody),
        (status = 503, description = "This core has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn set_routing_rule_enabled(
    _guard: RequireManageSystem,
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
        (status = 403, description = "Role lacks ManageSystem", body = super::error::ErrorBody),
        (status = 404, description = "No such rule", body = super::error::ErrorBody),
        (status = 503, description = "This core has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn delete_routing_rule(
    _guard: RequireManageSystem,
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
            (
                "PUT",
                format!("/api/v1/notification-channels/{ID}/template"),
            ),
            ("POST", "/api/v1/notification-channels/preview".to_owned()),
            (
                "GET",
                "/api/v1/notification-channels/template-variables".to_owned(),
            ),
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

    /// The contract the editor branches on: a template that does not compile is a **typed 400**
    /// with `invalid_template`, not a 500 out of migration 0063's CHECK and not a silent save that
    /// only fails months later when an alert fires.
    ///
    /// Asserted on the mapping rather than through the router because the handler takes `Admin`
    /// before its body, so a skeleton-mode request is answered 503 before validation is reached —
    /// which is the guard ordering `api-conventions.md` requires, not a bug.
    #[test]
    fn a_template_that_does_not_compile_maps_to_a_typed_400() {
        use axum::response::IntoResponse;
        let bad = ChannelTemplate {
            subject: None,
            body: Some("{% if severity %}unclosed".to_owned()),
        };
        let err = crate::notify_render::validate(&bad).expect_err("must not compile");
        assert_eq!(err.field.as_str(), "body");
        let resp = ApiError::bad_request(
            "invalid_template",
            format!("{} template: {}", err.field.as_str(), err.message),
        )
        .into_response();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        // …and a template that compiles is accepted, including the empty pair (= built-in).
        crate::notify_render::validate(&ChannelTemplate {
            subject: Some("{{ severity }} {{ node_name }}".to_owned()),
            body: Some("{% if event == 'resolve' %}ok{% endif %}".to_owned()),
        })
        .expect("valid template");
        crate::notify_render::validate(&ChannelTemplate::default()).expect("empty is valid");
    }

    /// Blank is how an operator clears an override, and it has to mean "built-in" rather than
    /// "send an empty subject" — the second is a silent way to lose every notification's headline.
    #[test]
    fn a_blank_field_clears_the_override_rather_than_emptying_it() {
        let cleared = TemplateBody {
            subject: Some("   ".to_owned()),
            body: Some(String::new()),
        }
        .into_template();
        assert!(cleared.is_builtin());

        let kept = TemplateBody {
            subject: Some("{{ node_name }}".to_owned()),
            body: None,
        }
        .into_template();
        assert_eq!(kept.subject.as_deref(), Some("{{ node_name }}"));
        assert!(kept.body.is_none());
    }

    /// The edge rejects an over-long template so the operator sees a 400 naming the limit, not a
    /// 500 from migration 0063's CHECK.
    #[test]
    fn an_over_long_template_is_rejected_before_the_database_sees_it() {
        let too_long = ChannelTemplate {
            subject: Some("x".repeat(MAX_SUBJECT_SOURCE + 1)),
            body: None,
        };
        assert!(check_template_size(&too_long).is_err());
        let ok = ChannelTemplate {
            subject: Some("x".repeat(MAX_SUBJECT_SOURCE)),
            body: Some("y".repeat(MAX_BODY_SOURCE)),
        };
        assert!(check_template_size(&ok).is_ok());
    }

    /// The preview is a read: it must not dirty the config generation, or an operator typing a
    /// template would trigger a full-fleet rebuild on every keystroke-batch (S6).
    #[test]
    fn previewing_a_template_is_not_a_config_change() {
        assert!(!crate::api::changes_monitoring_config(
            "/api/v1/notification-channels/preview"
        ));
        // …but actually saving one is.
        assert!(crate::api::changes_monitoring_config(&format!(
            "/api/v1/notification-channels/{ID}/template"
        )));
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

        // Email: the arm that used to reach the fail-open wildcard. Accept side first — a
        // rejection-only check here would pass even if every config were refused.
        let email = |host: &str, from: &str, to: &str| ChannelConfig::Email {
            host: host.into(),
            port: None,
            from: from.into(),
            to: to.into(),
            user: None,
            pass: None,
        };
        assert!(validate_channel_config(&email("smtp.example", "a@example", "b@example")).is_ok());
        assert!(validate_channel_config(&email(" ", "a@example", "b@example")).is_err());
        assert!(validate_channel_config(&email("smtp.example", "", "b@example")).is_err());
        assert!(validate_channel_config(&email("smtp.example", "a@example", "  ")).is_err());
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
    // ── An accepted write (ADR-115) ──────────────────────────────────────────────────

    /// A channel is created sealed: stored, listed, and its target never returned.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn creating_a_channel_stores_it_without_returning_its_config(pool: sqlx::PgPool) {
        use crate::api::tests_support::{live_state, send, token};
        let st = live_state(pool.clone()).await;
        let tok = token(&st, yagra_common::Role::Admin);
        let (status, body) = send(
            &st,
            "POST",
            "/api/v1/notification-channels",
            &tok,
            Some(serde_json::json!({
                "name": "ops webhook",
                "config": { "kind": "webhook", "url": "http://10.0.0.9/hook/s3cr3t-path" },
            })),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::CREATED, "{body}");
        assert_eq!(crate::pgtest::rows(&pool, "notification_channels").await, 1);

        let (status, list) = send(&st, "GET", "/api/v1/notification-channels", &tok, None).await;
        assert_eq!(status, axum::http::StatusCode::OK, "{list}");
        assert!(
            !list.to_string().contains("s3cr3t-path"),
            "the list returned the channel's target"
        );
    }
}
