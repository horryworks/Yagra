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
use yagra_common::{is_ssrf_blocked, NotifyEvent, PreviewSample, Severity};

/// This domain's slice of the OpenAPI document (ADR-035), merged by [`super::openapi::document`].
#[derive(utoipa::OpenApi)]
#[openapi(paths(
    list_notification_channels,
    create_notification_channel,
    set_notification_channel_enabled,
    delete_notification_channel,
    set_notification_template,
    test_notification_channel,
    preview_notification_template,
    list_template_variables,
    get_builtin_template,
    list_routing_rules,
    create_routing_rule,
    set_routing_rule_enabled,
    update_routing_rule,
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
            "/api/v1/notification-channels/builtin-template",
            get(get_builtin_template),
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
            "/api/v1/notification-channels/:id/test",
            post(test_notification_channel),
        )
        .route(
            "/api/v1/routing-rules",
            get(list_routing_rules).post(create_routing_rule),
        )
        .route(
            "/api/v1/routing-rules/:id",
            put(set_routing_rule_enabled).delete(delete_routing_rule),
        )
        // Beside `/:id` the way a channel's `/template` is: `PUT /:id` already means "switch it on
        // or off" to every API client, so the edit gets its own path rather than a second meaning.
        .route(
            "/api/v1/routing-rules/:id/definition",
            put(update_routing_rule),
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

/// What a test send did (ADR-192). A failure is reported **in the 200 response**, as the template
/// preview does: the request worked, and what it found out is that the channel does not.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(super) struct ChannelTestResult {
    /// Whether the channel accepted the test notification.
    delivered: bool,
    /// Whether the incident the test opened was closed again. `null` for a channel kind with no
    /// incident (webhook, email), and when the delivery itself failed.
    closed: Option<bool>,
    /// The first failure, as the channel reported it. Never contains the channel's URL.
    error: Option<String>,
}

/// Send one test notification through a channel, now (ADR-192).
///
/// The template preview's sample alert, rendered with this channel's template, with `[TEST] ` at
/// the start of the subject and `"test": true` in the built-in JSON body. Sent once, with no retry.
/// A PagerDuty or JSM channel then closes the incident it opened, so the on-call is notified once
/// and nothing is left open. Works on a disabled channel, so one can be checked before it is
/// switched on.
#[utoipa::path(
    post, path = "/api/v1/notification-channels/{id}/test", tag = "notifications",
    params(("id" = Uuid, Path, description = "Channel id")),
    responses(
        (status = 200, description = "The test was attempted; whether the channel accepted it is in the body", body = ChannelTestResult),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageSystem", body = super::error::ErrorBody),
        (status = 404, description = "No such channel", body = super::error::ErrorBody),
        (status = 503, description = "This core has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn test_notification_channel(
    _guard: RequireManageSystem,
    admin: Admin,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<ChannelTestResult>> {
    let (_, open) = admin
        .notifications
        .open_channel(id)
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "open notification channel",
                "failed to read notification channel",
            )
        })?
        .ok_or_else(|| ApiError::not_found("channel_not_found", format!("no channel {id}")))?;
    let kind = open.config.kind();
    let Some(channel) = crate::alerts::notify::build_test_channel(&open.config) else {
        // Only an email channel can fail to build: its stored addresses did not parse. The same
        // channel is silently skipped by the routing snapshot, which is worth saying here.
        return Ok(Json(ChannelTestResult {
            delivered: false,
            closed: None,
            error: Some("the stored SMTP host or addresses could not be parsed".to_owned()),
        }));
    };
    let notification = crate::alerts::notify::test_notification(kind, &open.template);
    let outcome = crate::alerts::notify::send_test(
        channel.as_ref(),
        &notification,
        crate::alerts::notify::test_close_delay(kind),
    )
    .await;
    tracing::info!(
        channel = %id,
        delivered = outcome.delivered,
        closed = ?outcome.closed,
        "notification channel test sent"
    );
    Ok(Json(ChannelTestResult {
        delivered: outcome.delivered,
        closed: outcome.closed,
        error: outcome.error,
    }))
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
    /// Which representative alert to render against: `threshold` (a port over its threshold, every
    /// optional variable present — the default) or `liveness` (a node that stopped answering, with
    /// no metric, value, threshold, direction or port).
    #[serde(default)]
    sample: PreviewSample,
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
    let (alert, resolved) = crate::notify_facts::preview_sample(req.sample);
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

/// One lifecycle point's built-in subject, written as a template.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(super) struct BuiltinSubjectTemplate {
    /// `fire`, `resolve`, or `suppress`.
    event: NotifyEvent,
    /// The template that renders Yagra's built-in subject for a node alert at this point.
    subject: String,
}

/// Yagra's built-in subject for a node alert, written as a template, once per lifecycle point.
///
/// The template editor opens a channel that has no template on this text, so an operator starts
/// from what is sent today. Rendering it produces exactly the built-in subject. A poller pool's
/// and a Meraki organization's alerts have built-in wording of their own, which is not described
/// here. There is no built-in body template: the built-in body is the whole alert as JSON.
#[utoipa::path(
    get, path = "/api/v1/notification-channels/builtin-template", tag = "notifications",
    responses(
        (status = 200, description = "The built-in subject of a node alert as a template, for `fire`, `resolve` and `suppress`", body = Vec<BuiltinSubjectTemplate>),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageSystem", body = super::error::ErrorBody),
    ),
)]
async fn get_builtin_template(_guard: RequireManageSystem) -> Json<Vec<BuiltinSubjectTemplate>> {
    Json(
        NotifyEvent::ALL
            .into_iter()
            .map(|event| BuiltinSubjectTemplate {
                event,
                subject: crate::alerts::builtin_node_subject_template(event).to_owned(),
            })
            .collect(),
    )
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
/// Editing a rule takes the same body (ADR-193).
#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct CreateRule {
    name: String,
    severity: Option<String>,
    channel_ids: Vec<Uuid>,
}

impl CreateRule {
    /// The checks a rule passes before it is stored, shared by create and edit so the two cannot
    /// accept different things.
    fn parse(&self) -> ApiResult<(&str, Option<Severity>)> {
        let name = self.name.trim();
        if name.is_empty() {
            return Err(ApiError::bad_request(
                "invalid_rule",
                "name must not be empty",
            ));
        }
        let severity = parse_severity_opt(self.severity.as_deref()).map_err(|()| {
            ApiError::bad_request(
                "invalid_rule",
                "severity must be critical|warning|info or null",
            )
        })?;
        Ok((name, severity))
    }
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
    let (name, severity) = body.parse()?;
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

/// Replace a rule's name, severity filter and channels. Whether it is switched on is left as it was.
#[utoipa::path(
    put, path = "/api/v1/routing-rules/{id}/definition", tag = "notifications",
    params(("id" = Uuid, Path, description = "Routing rule id")),
    request_body = CreateRule,
    responses(
        (status = 204, description = "Rule replaced; its enabled switch is unchanged"),
        (status = 400, description = "Empty name, or a severity outside critical|warning|info|null", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageSystem", body = super::error::ErrorBody),
        (status = 404, description = "No such rule", body = super::error::ErrorBody),
        (status = 503, description = "This core has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn update_routing_rule(
    _guard: RequireManageSystem,
    admin: Admin,
    Path(id): Path<Uuid>,
    Json(body): Json<CreateRule>,
) -> ApiResult<StatusCode> {
    let (name, severity) = body.parse()?;
    match admin
        .notifications
        .update_rule(id, name, severity, &body.channel_ids)
        .await
    {
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
            ("POST", format!("/api/v1/notification-channels/{ID}/test")),
            ("POST", "/api/v1/notification-channels/preview".to_owned()),
            (
                "GET",
                "/api/v1/notification-channels/template-variables".to_owned(),
            ),
            (
                "GET",
                "/api/v1/notification-channels/builtin-template".to_owned(),
            ),
            ("GET", "/api/v1/routing-rules".to_owned()),
            ("POST", "/api/v1/routing-rules".to_owned()),
            ("PUT", format!("/api/v1/routing-rules/{ID}")),
            ("PUT", format!("/api/v1/routing-rules/{ID}/definition")),
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

    /// Send one request as an administrator and read the JSON it answers. Neither route below takes
    /// `Admin`, so skeleton mode serves them and no database is needed.
    async fn admin_json(
        method: &str,
        path: &str,
        body: serde_json::Value,
    ) -> (StatusCode, serde_json::Value) {
        let st = private_state();
        let token = st
            .sessions
            .issue(Uuid::new_v4(), Principal::new(Role::Admin, Scope::All), "u");
        let resp = router(st)
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .header("content-type", "application/json")
                    .header(AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, serde_json::from_slice(&bytes).unwrap_or_default())
    }

    /// The editor's preview pane is this response, so it is checked through the router: the
    /// sample and the event it asked for, a blank field answered with the built-in text, and a
    /// line the template leaves out on the alert that lacks its value (ADR-039 Inc.2).
    #[tokio::test]
    async fn a_preview_renders_the_requested_sample_and_event() {
        let path = "/api/v1/notification-channels/preview";
        let body = "{{ subject_name }}
{% if metric is defined %}{{ metric }} = {{ value }}
{% endif %}at {{ at }}";
        let (status, out) = admin_json(
            "POST",
            path,
            serde_json::json!({ "kind": "jsm", "event": "resolve", "sample": "liveness",
                                "subject": null, "body": body }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            out["subject"], "resolved: node 6f1c9d2a-0b3e-4a71-9c8d-2e5f7a1b4c60 recovered",
            "a blank subject is the built-in text for the event asked for"
        );
        assert_eq!(
            out["body"],
            "core-sw-01
at 2026-08-04T09:41:07+00:00"
        );
        assert_eq!(out["problems"], serde_json::json!([]));
        assert!(out.get("json_valid").is_none(), "JSM's body is plain text");

        // No `sample`: the threshold breach, as every caller before the field existed received.
        let (_, out) = admin_json(
            "POST",
            path,
            serde_json::json!({ "kind": "jsm", "body": body }),
        )
        .await;
        assert_eq!(
            out["body"],
            "core-sw-01
if_in_util_pct = 94.2
at 2026-08-04T09:41:07+00:00"
        );
        assert_eq!(
            out["subject"],
            "node 6f1c9d2a-0b3e-4a71-9c8d-2e5f7a1b4c60 is critical"
        );
    }

    /// The editor's insert list groups and names the variables from its own copy of their names
    /// (ADR-039 Inc.2). A variable added here and not there would be one an operator cannot insert;
    /// one there and not here would be a tag that renders empty.
    #[test]
    fn every_template_variable_is_one_the_editor_groups() {
        let ts = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../web/src/pages/templateVariables.ts"),
        )
        .expect("web/src/pages/templateVariables.ts");
        let start = ts
            .find("export const TEMPLATE_VARIABLE_NAMES = [")
            .expect("TEMPLATE_VARIABLE_NAMES is declared in templateVariables.ts");
        let block = &ts[start..];
        let block = &block[..block.find("] as const").expect("the array closes")];
        let listed: std::collections::BTreeSet<&str> =
            block.split('\'').skip(1).step_by(2).collect();
        let ours: std::collections::BTreeSet<&str> = yagra_common::TEMPLATE_VARIABLES
            .iter()
            .map(|v| v.name)
            .collect();
        assert!(listed.len() >= 20, "read too few names: {listed:?}");
        assert_eq!(
            listed, ours,
            "templateVariables.ts lists exactly the template variables"
        );
    }

    /// The draft the editor opens on: one template per lifecycle point, in lifecycle order.
    #[tokio::test]
    async fn the_builtin_template_names_every_lifecycle_point() {
        let (status, out) = admin_json(
            "GET",
            "/api/v1/notification-channels/builtin-template",
            serde_json::Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let events: Vec<&str> = out
            .as_array()
            .expect("an array")
            .iter()
            .map(|e| e["event"].as_str().expect("event"))
            .collect();
        assert_eq!(events, ["fire", "resolve", "suppress"]);
        assert_eq!(out[0]["subject"], "node {{ node_id }} is {{ state }}");
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

    /// ADR-192: the test send is ACCEPTED (200) even when the channel cannot deliver — the failure
    /// is the answer, reported in the body — and the reason never carries the channel's URL, which
    /// is sealed at rest and never returned. `.invalid` is reserved (RFC 2606), so the lookup fails
    /// without anything leaving the machine. Works on a disabled channel, and an unknown id is 404.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn testing_a_channel_reports_the_failure_without_its_url(pool: sqlx::PgPool) {
        use crate::api::tests_support::{live_state, send, token};
        let st = live_state(pool).await;
        let tok = token(&st, yagra_common::Role::Admin);
        let (status, created) = send(
            &st,
            "POST",
            "/api/v1/notification-channels",
            &tok,
            Some(serde_json::json!({
                "name": "unreachable",
                "config": { "kind": "webhook", "url": "http://hooks.nonexistent.invalid/s3cr3t-path" },
            })),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::CREATED, "{created}");
        let id = created["id"].as_str().unwrap().to_owned();
        let (status, _) = send(
            &st,
            "PUT",
            &format!("/api/v1/notification-channels/{id}"),
            &tok,
            Some(serde_json::json!({ "enabled": false })),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::NO_CONTENT);

        let (status, body) = send(
            &st,
            "POST",
            &format!("/api/v1/notification-channels/{id}/test"),
            &tok,
            None,
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::OK, "{body}");
        assert_eq!(body["delivered"], false, "{body}");
        assert!(
            body["closed"].is_null(),
            "a webhook has no incident: {body}"
        );
        let error = body["error"].as_str().expect("a failure says why");
        assert!(!error.is_empty());
        for secret in ["s3cr3t-path", "nonexistent.invalid"] {
            assert!(
                !error.contains(secret),
                "the reason leaked {secret}: {error}"
            );
        }

        let (status, _) = send(
            &st,
            "POST",
            &format!("/api/v1/notification-channels/{}/test", Uuid::new_v4()),
            &tok,
            None,
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::NOT_FOUND);
    }

    /// ADR-193: a rule's definition is replaced in place — name, severity and channels — while
    /// its on/off switch stays where the operator left it. The checks are the create path's.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn editing_a_rule_replaces_its_definition_and_keeps_it_switched_off(pool: sqlx::PgPool) {
        use crate::api::tests_support::{live_state, send, token};
        let st = live_state(pool).await;
        let tok = token(&st, yagra_common::Role::Admin);
        let mut channels = Vec::new();
        for name in ["first", "second"] {
            let (status, created) = send(
                &st,
                "POST",
                "/api/v1/notification-channels",
                &tok,
                Some(serde_json::json!({
                    "name": name,
                    "config": { "kind": "webhook", "url": "http://10.0.0.9/hook" },
                })),
            )
            .await;
            assert_eq!(status, axum::http::StatusCode::CREATED, "{created}");
            channels.push(created["id"].as_str().unwrap().to_owned());
        }
        let (status, created) = send(
            &st,
            "POST",
            "/api/v1/routing-rules",
            &tok,
            Some(serde_json::json!({
                "name": "everything", "severity": null, "channel_ids": [channels[0]],
            })),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::CREATED, "{created}");
        let id = created["id"].as_str().unwrap().to_owned();
        let (status, _) = send(
            &st,
            "PUT",
            &format!("/api/v1/routing-rules/{id}"),
            &tok,
            Some(serde_json::json!({ "enabled": false })),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::NO_CONTENT);

        let path = format!("/api/v1/routing-rules/{id}/definition");
        let (status, body) = send(
            &st,
            "PUT",
            &path,
            &tok,
            Some(serde_json::json!({
                "name": "  criticals  ", "severity": "critical", "channel_ids": [channels[1]],
            })),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::NO_CONTENT, "{body}");
        let (status, list) = send(&st, "GET", "/api/v1/routing-rules", &tok, None).await;
        assert_eq!(status, axum::http::StatusCode::OK, "{list}");
        let rule = &list[0];
        assert_eq!(rule["name"], "criticals", "{list}");
        assert_eq!(rule["severity"], "critical", "{list}");
        assert_eq!(
            rule["channel_ids"],
            serde_json::json!([channels[1]]),
            "{list}"
        );
        assert_eq!(
            rule["enabled"], false,
            "an edit switched the rule back on: {list}"
        );

        // The create path's checks, not a looser copy of them.
        for bad in [
            serde_json::json!({ "name": " ", "severity": null, "channel_ids": [] }),
            serde_json::json!({ "name": "x", "severity": "CRITICAL", "channel_ids": [] }),
        ] {
            let (status, body) = send(&st, "PUT", &path, &tok, Some(bad)).await;
            assert_eq!(status, axum::http::StatusCode::BAD_REQUEST, "{body}");
            assert_eq!(body["error"]["code"], "invalid_rule", "{body}");
        }

        let (status, body) = send(
            &st,
            "PUT",
            &format!("/api/v1/routing-rules/{}/definition", Uuid::new_v4()),
            &tok,
            Some(serde_json::json!({ "name": "x", "severity": null, "channel_ids": [] })),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::NOT_FOUND, "{body}");
        assert_eq!(body["error"]["code"], "rule_not_found", "{body}");
    }
}
