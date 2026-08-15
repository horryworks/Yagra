// SPDX-License-Identifier: AGPL-3.0-only
//! Passive events: webhook ingest, the sources and rules that shape it, and the manual alert close.
//!
//! Reading and searching the event log lives next door in [`super::eventlog`]; this module owns the
//! configuration side plus the one machine-facing endpoint.
//!
//! **Webhook ingest authenticates differently from everything else in this API.** An external sender
//! holds a per-source bearer token, not a session — it is a script, not a person — so
//! [`ingest_webhook`] verifies that token against the source rather than taking a `Require*` guard.
//! It is also exempt from the audit middleware, because the events table *is* the record; auditing
//! every ingested event would duplicate the whole stream into the audit log.
//!
//! **Ingest must run on the HA leader** ([`Leader`]). The engine's persist and action channels are
//! drained by leader-only writers, so ingesting on a standby enqueues to a channel nobody drains.
//!
//! Rule patterns are compiled at the edge. A rule that does not compile would otherwise be stored,
//! loaded into the engine snapshot, and silently match nothing forever.

use super::error::{ApiError, ApiResult};
use super::extract::{bearer, Admin, Events, Leader, RequireManageConfig};
use super::util::CreatedId;
use super::{AdminState, ApiState};
use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

/// Max accepted webhook ingest body (axum answers 413 beyond it).
pub(super) const WEBHOOK_BODY_LIMIT: usize = 64 * 1024;
/// Normalized event-text cap. Matches the `events.message` DB CHECK, so a message that would be
/// rejected by the database is truncated here instead of failing the ingest.
const EVENT_TEXT_MAX_CHARS: usize = 4096;

/// This domain's slice of the OpenAPI document (ADR-035), merged by [`super::openapi::document`].
#[derive(utoipa::OpenApi)]
#[openapi(paths(
    ingest_webhook,
    list_event_sources,
    create_event_source,
    update_event_source,
    delete_event_source,
    rotate_event_source_token,
    list_event_rules,
    create_event_rule,
    update_event_rule,
    delete_event_rule,
    test_event_rule
))]
pub(super) struct Doc;

/// The passive-event routes, merged into `/api/v1` by [`super::router`].
///
/// The ingest route's body limit is applied by [`super::router`], which owns the layering.
pub(super) fn routes() -> Router<ApiState> {
    Router::new()
        .route(
            "/api/v1/event-sources",
            get(list_event_sources).post(create_event_source),
        )
        .route(
            "/api/v1/event-sources/:id",
            axum::routing::put(update_event_source).delete(delete_event_source),
        )
        .route(
            "/api/v1/event-sources/:id/rotate-token",
            post(rotate_event_source_token),
        )
        .route(
            "/api/v1/event-rules",
            get(list_event_rules).post(create_event_rule),
        )
        .route("/api/v1/event-rules/test", post(test_event_rule))
        .route(
            "/api/v1/event-rules/:id",
            axum::routing::put(update_event_rule).delete(delete_event_rule),
        )
}

/// The ingest route on its own, so [`super::router`] can attach the body limit to just this path.
pub(super) fn ingest_route() -> Router<ApiState> {
    Router::new().route("/api/v1/ingest/webhook/:source_id", post(ingest_webhook))
}

/// Pull the matchable text out of a webhook body.
///
/// A zero-config heuristic covering the common alertmanager/grafana/custom-script shapes: a JSON
/// object's first present `message` | `text` | `summary` string, else the compact JSON, else the raw
/// body as lossy UTF-8. Truncation is by **character**, not byte, so a multi-byte boundary is never
/// split. Returns whether it truncated, so the stored event can say so.
fn extract_webhook_text(body: &[u8]) -> (String, bool) {
    let text = match serde_json::from_slice::<Value>(body) {
        Ok(v) => {
            let field = v.as_object().and_then(|obj| {
                ["message", "text", "summary"]
                    .iter()
                    .find_map(|k| obj.get(*k).and_then(|x| x.as_str()).map(str::to_owned))
            });
            field.unwrap_or_else(|| v.to_string())
        }
        Err(_) => String::from_utf8_lossy(body).into_owned(),
    };
    match text.char_indices().nth(EVENT_TEXT_MAX_CHARS) {
        Some((idx, _)) => (text[..idx].to_owned(), true),
        None => (text, false),
    }
}

/// The accepted event's id.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct IngestedEvent {
    event_id: Uuid,
}

/// Machine-scoped webhook ingest.
///
/// Authenticated by the per-source bearer token rather than a session — see the module doc. The
/// three token outcomes are deliberately distinct: a wrong token is `401 bad_token`, an unknown or
/// disabled source is `404`, and a source over its rate is `429`. A sender needs to tell "fix my
/// token" from "that source is gone" from "slow down".
#[utoipa::path(
    post, path = "/api/v1/ingest/webhook/{source_id}", tag = "events",
    params(("source_id" = Uuid, Path, description = "Webhook source id")),
    request_body(
        content = String,
        description = "Arbitrary sender payload; a JSON object's message/text/summary becomes the event text",
        content_type = "application/json"
    ),
    responses(
        (status = 202, description = "Event accepted", body = IngestedEvent),
        (status = 401, description = "Missing or wrong per-source ingest token", body = super::error::ErrorBody),
        (status = 404, description = "No enabled webhook source with that id", body = super::error::ErrorBody),
        (status = 413, description = "Body over the per-request ingest cap"),
        (status = 429, description = "This source is over its ingest rate limit", body = super::error::ErrorBody),
        (status = 503, description = "Event ingestion is not configured, or this core is not the HA leader", body = super::error::ErrorBody),
    ),
)]
async fn ingest_webhook(
    engine: Events,
    _leader: Leader,
    headers: HeaderMap,
    Path(source_id): Path<Uuid>,
    body: axum::body::Bytes,
) -> ApiResult<(StatusCode, Json<IngestedEvent>)> {
    let token = bearer(&headers).ok_or_else(|| {
        ApiError::unauthorized_with("missing_token", "Authorization: Bearer <token> required")
    })?;
    let node_id = match engine.repo().verify_token(source_id, token).await {
        Ok(crate::events::TokenVerify::Ok { node_id }) => node_id,
        Ok(crate::events::TokenVerify::BadToken) => {
            return Err(ApiError::unauthorized_with(
                "bad_token",
                "ingest token does not match",
            ))
        }
        Ok(crate::events::TokenVerify::UnknownOrDisabled) => {
            return Err(ApiError::not_found(
                "source_not_found",
                format!("no enabled webhook source {source_id}"),
            ))
        }
        Err(e) => {
            return Err(ApiError::from_internal(
                e.as_ref(),
                "verify ingest token",
                "failed to verify ingest token",
            ))
        }
    };
    if !engine.ingest_allowed(source_id) {
        return Err(ApiError::too_many_requests(
            "rate_limited",
            "ingest rate limit exceeded for this source",
        ));
    }

    let (message, truncated) = extract_webhook_text(&body);
    let event_id = Uuid::new_v4();
    let at_unix_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
    let msg = yagra_bus::EventMsg {
        event_id,
        kind: yagra_bus::EventKind::Webhook,
        at_unix_ms,
        source_ip: None,
        pool: None,
        message,
        facility: None,
        syslog_severity: None,
        hostname: None,
        app_name: None,
        trap_oid: None,
        varbinds: Vec::new(),
        truncated,
        // A webhook body is JSON, not a datagram: there is no wire form to forward verbatim, and the
        // body can hold the shared secret. Forwarding renders from the parsed fields instead.
        raw: None,
        src_port: None,
    };
    engine
        .handle_event(
            msg,
            Some(crate::events::SourceBinding { source_id, node_id }),
        )
        .await;
    Ok((StatusCode::ACCEPTED, Json(IngestedEvent { event_id })))
}

/// Reload the engine's rule/address snapshot after an edit so the change applies immediately.
///
/// Best-effort by design — the 30s refresh loop is the backstop, so a reload failure delays a rule
/// rather than losing the write that already succeeded.
async fn reload_event_engine(st: &ApiState, admin: &AdminState) {
    if let Some(engine) = st.events.as_ref() {
        engine.reload(&admin.repo).await;
    }
}

#[utoipa::path(
    get, path = "/api/v1/event-sources", tag = "events",
    responses(
        (status = 200, description = "Every ingest source (never the token hash)", body = Vec<crate::events::EventSourceView>),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
        (status = 503, description = "This core has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn list_event_sources(
    _guard: RequireManageConfig,
    admin: Admin,
) -> ApiResult<Json<Vec<crate::events::EventSourceView>>> {
    let sources = admin.events.list_sources().await.map_err(|e| {
        ApiError::from_internal(
            e.as_ref(),
            "list event sources",
            "failed to list event sources",
        )
    })?;
    Ok(Json(sources))
}

/// A source's name, bounded so it stays renderable in the sources table.
fn checked_source_name(name: &str) -> Result<&str, ApiError> {
    let name = name.trim();
    if name.is_empty() || name.len() > 120 {
        return Err(ApiError::bad_request(
            "invalid_source",
            "name must be 1..=120 characters",
        ));
    }
    Ok(name)
}

#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct CreateEventSource {
    name: String,
    #[serde(default)]
    node_id: Option<Uuid>,
}

/// A newly created source, with its ingest token.
///
/// **The plaintext token appears here and nowhere else** — only its hash is stored, so this response
/// and `rotate-token` are the only two places it can ever be read.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct CreatedSource {
    id: Uuid,
    token: String,
}

#[utoipa::path(
    post, path = "/api/v1/event-sources", tag = "events",
    request_body = CreateEventSource,
    responses(
        (status = 201, description = "Source created; the plaintext ingest token appears here only", body = CreatedSource),
        (status = 400, description = "Name is empty or over 120 characters", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
        (status = 503, description = "This core has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn create_event_source(
    _guard: RequireManageConfig,
    admin: Admin,
    Json(body): Json<CreateEventSource>,
) -> ApiResult<(StatusCode, Json<CreatedSource>)> {
    let name = checked_source_name(&body.name)?;
    let (id, token) = admin
        .events
        .create_source(name, body.node_id)
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "create event source",
                "failed to create event source",
            )
        })?;
    Ok((StatusCode::CREATED, Json(CreatedSource { id, token })))
}

#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct UpdateEventSource {
    name: String,
    enabled: bool,
    #[serde(default)]
    node_id: Option<Uuid>,
}

/// The not-found error every source-scoped endpoint answers with.
fn no_source(id: Uuid) -> ApiError {
    ApiError::not_found("source_not_found", format!("no event source {id}"))
}

#[utoipa::path(
    put, path = "/api/v1/event-sources/{id}", tag = "events",
    params(("id" = Uuid, Path, description = "Event source id")),
    request_body = UpdateEventSource,
    responses(
        (status = 204, description = "Source updated"),
        (status = 400, description = "Name is empty or over 120 characters", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
        (status = 404, description = "No such event source", body = super::error::ErrorBody),
        (status = 503, description = "This core has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn update_event_source(
    _guard: RequireManageConfig,
    admin: Admin,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateEventSource>,
) -> ApiResult<StatusCode> {
    let name = checked_source_name(&body.name)?;
    match admin
        .events
        .update_source(id, name, body.enabled, body.node_id)
        .await
    {
        Ok(true) => Ok(StatusCode::NO_CONTENT),
        Ok(false) => Err(no_source(id)),
        Err(e) => Err(ApiError::from_internal(
            e.as_ref(),
            "update event source",
            "failed to update event source",
        )),
    }
}

/// A freshly rotated ingest token — the second and last place the plaintext appears.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct RotatedToken {
    token: String,
}

#[utoipa::path(
    post, path = "/api/v1/event-sources/{id}/rotate-token", tag = "events",
    params(("id" = Uuid, Path, description = "Event source id")),
    responses(
        (status = 200, description = "A new ingest token; the plaintext is readable here only", body = RotatedToken),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
        (status = 404, description = "No such event source", body = super::error::ErrorBody),
        (status = 503, description = "This core has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn rotate_event_source_token(
    _guard: RequireManageConfig,
    admin: Admin,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<RotatedToken>> {
    admin
        .events
        .rotate_token(id)
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "rotate event source token",
                "failed to rotate event source token",
            )
        })?
        .map(|token| Json(RotatedToken { token }))
        .ok_or_else(|| no_source(id))
}

#[utoipa::path(
    delete, path = "/api/v1/event-sources/{id}", tag = "events",
    params(("id" = Uuid, Path, description = "Event source id")),
    responses(
        (status = 204, description = "Source deleted"),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
        (status = 404, description = "No such event source", body = super::error::ErrorBody),
        (status = 503, description = "This core has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn delete_event_source(
    _guard: RequireManageConfig,
    State(st): State<ApiState>,
    admin: Admin,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    let deleted = admin.events.delete_source(id).await.map_err(|e| {
        ApiError::from_internal(
            e.as_ref(),
            "delete event source",
            "failed to delete event source",
        )
    })?;
    if !deleted {
        return Err(no_source(id));
    }
    reload_event_engine(&st, &admin).await;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    get, path = "/api/v1/event-rules", tag = "events",
    responses(
        (status = 200, description = "Every stored event rule", body = Vec<crate::events::StoredEventRule>),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
        (status = 503, description = "This core has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn list_event_rules(
    _guard: RequireManageConfig,
    admin: Admin,
) -> ApiResult<Json<Vec<crate::events::StoredEventRule>>> {
    let rules = admin.events.list_rules().await.map_err(|e| {
        ApiError::from_internal(e.as_ref(), "list event rules", "failed to list event rules")
    })?;
    Ok(Json(rules))
}

#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct EventRuleBody {
    name: String,
    #[serde(default = "default_event_rule_enabled")]
    enabled: bool,
    #[serde(default)]
    source_kind: Option<String>,
    #[serde(default)]
    source_id: Option<Uuid>,
    #[serde(default)]
    node_id: Option<Uuid>,
    match_kind: String,
    pattern: String,
    #[serde(default)]
    clear_pattern: Option<String>,
    severity: String,
    #[serde(default)]
    ttl_secs: Option<i32>,
    #[serde(default)]
    min_count: Option<i32>,
    #[serde(default)]
    window_secs: Option<i32>,
}

const fn default_event_rule_enabled() -> bool {
    true
}

/// Validate an event-rule body at the edge.
///
/// Mirrors the DB CHECKs, and — the part that matters — **compiles both patterns**. A rule whose
/// regex does not compile would otherwise be stored, loaded into the engine's snapshot, and match
/// nothing for the rest of its life, with no error anywhere.
fn validate_event_rule(body: &EventRuleBody) -> Result<crate::events::RuleParams<'_>, ApiError> {
    let bad = |msg: String| ApiError::bad_request("invalid_rule", msg);
    let name = body.name.trim();
    if name.is_empty() || name.len() > 120 {
        return Err(bad("name must be 1..=120 characters".to_owned()));
    }
    // Token vocabularies come from the enums' own `from_token`/`from_stored` (one source, no
    // re-typed lists). `Unknown` is the reader's shrug for a token this build doesn't recognise —
    // an operator cannot author a rule with it, so garbage and the literal "unknown" both reject.
    match crate::events::EventMatchKind::from_stored(&body.match_kind) {
        crate::events::EventMatchKind::Unknown => {
            return Err(bad("match_kind must be substring or regex".to_owned()));
        }
        crate::events::EventMatchKind::Substring | crate::events::EventMatchKind::Regex => {}
    }
    crate::events::compile_matcher(&body.match_kind, &body.pattern)
        .map_err(|e| bad(format!("pattern: {e}")))?;
    if let Some(clear) = body.clear_pattern.as_deref() {
        crate::events::compile_matcher(&body.match_kind, clear)
            .map_err(|e| bad(format!("clear_pattern: {e}")))?;
    }
    if yagra_common::Severity::from_token(&body.severity).is_none() {
        let allowed = yagra_common::Severity::ALL.map(|s| s.as_str()).join(", ");
        return Err(bad(format!("severity must be one of: {allowed}")));
    }
    if let Some(kind) = body.source_kind.as_deref() {
        if yagra_bus::EventKind::from_token(kind).is_none() {
            let allowed = yagra_bus::EventKind::ALL.map(|k| k.as_str()).join(", ");
            return Err(bad(format!("source_kind must be one of: {allowed}")));
        }
    }
    let ttl_secs = body.ttl_secs.unwrap_or(1800);
    if !(60..=604_800).contains(&ttl_secs) {
        return Err(bad("ttl_secs must be 60..=604800".to_owned()));
    }
    let min_count = body.min_count.unwrap_or(1);
    if !(1..=100).contains(&min_count) {
        return Err(bad("min_count must be 1..=100".to_owned()));
    }
    let window_secs = body.window_secs.unwrap_or(60);
    if !(1..=3600).contains(&window_secs) {
        return Err(bad("window_secs must be 1..=3600".to_owned()));
    }
    Ok(crate::events::RuleParams {
        name,
        enabled: body.enabled,
        source_kind: body.source_kind.as_deref(),
        source_id: body.source_id,
        node_id: body.node_id,
        match_kind: &body.match_kind,
        pattern: &body.pattern,
        clear_pattern: body.clear_pattern.as_deref(),
        severity: &body.severity,
        ttl_secs,
        min_count,
        window_secs,
    })
}

/// The not-found error every rule-scoped endpoint answers with.
fn no_rule(id: Uuid) -> ApiError {
    ApiError::not_found("rule_not_found", format!("no event rule {id}"))
}

#[utoipa::path(
    post, path = "/api/v1/event-rules", tag = "events",
    request_body = EventRuleBody,
    responses(
        (status = 201, description = "Rule created", body = CreatedId),
        (status = 400, description = "A pattern does not compile, or a field is outside its accepted range", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
        (status = 503, description = "This core has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn create_event_rule(
    _guard: RequireManageConfig,
    State(st): State<ApiState>,
    admin: Admin,
    Json(body): Json<EventRuleBody>,
) -> ApiResult<(StatusCode, Json<CreatedId>)> {
    let params = validate_event_rule(&body)?;
    let id = admin.events.create_rule(&params).await.map_err(|e| {
        ApiError::from_internal(
            e.as_ref(),
            "create event rule",
            "failed to create event rule",
        )
    })?;
    reload_event_engine(&st, &admin).await;
    Ok((StatusCode::CREATED, Json(CreatedId { id })))
}

#[utoipa::path(
    put, path = "/api/v1/event-rules/{id}", tag = "events",
    params(("id" = Uuid, Path, description = "Event rule id")),
    request_body = EventRuleBody,
    responses(
        (status = 204, description = "Rule updated"),
        (status = 400, description = "A pattern does not compile, or a field is outside its accepted range", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
        (status = 404, description = "No such event rule", body = super::error::ErrorBody),
        (status = 503, description = "This core has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn update_event_rule(
    _guard: RequireManageConfig,
    State(st): State<ApiState>,
    admin: Admin,
    Path(id): Path<Uuid>,
    Json(body): Json<EventRuleBody>,
) -> ApiResult<StatusCode> {
    let params = validate_event_rule(&body)?;
    let updated = admin.events.update_rule(id, &params).await.map_err(|e| {
        ApiError::from_internal(
            e.as_ref(),
            "update event rule",
            "failed to update event rule",
        )
    })?;
    if !updated {
        return Err(no_rule(id));
    }
    reload_event_engine(&st, &admin).await;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    delete, path = "/api/v1/event-rules/{id}", tag = "events",
    params(("id" = Uuid, Path, description = "Event rule id")),
    responses(
        (status = 204, description = "Rule deleted"),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
        (status = 404, description = "No such event rule", body = super::error::ErrorBody),
        (status = 503, description = "This core has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn delete_event_rule(
    _guard: RequireManageConfig,
    State(st): State<ApiState>,
    admin: Admin,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    let deleted = admin.events.delete_rule(id).await.map_err(|e| {
        ApiError::from_internal(
            e.as_ref(),
            "delete event rule",
            "failed to delete event rule",
        )
    })?;
    if !deleted {
        return Err(no_rule(id));
    }
    reload_event_engine(&st, &admin).await;
    Ok(StatusCode::NO_CONTENT)
}

/// Body for the interactive rule tester.
#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct EventRuleTest {
    match_kind: String,
    pattern: String,
    #[serde(default)]
    clear_pattern: Option<String>,
    sample: String,
}

/// The tester's answer. A compile failure is reported **in-band** rather than as a 400: the whole
/// point of the tester is to show the operator what is wrong with the pattern they are typing, and
/// a 400 would be rendered as a request failure instead of inline next to the field.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct RuleTestResult {
    matched: bool,
    clear_matched: Option<bool>,
    error: Option<String>,
}

impl RuleTestResult {
    fn failed(error: String) -> Json<Self> {
        Json(Self {
            matched: false,
            clear_matched: None,
            error: Some(error),
        })
    }
}

/// Try a pattern against a sample message.
///
/// A read wearing POST — it compiles a regex and returns, changing nothing — so it is on
/// `changes_monitoring_config`'s exception list and does not dirty the config generation.
#[utoipa::path(
    post, path = "/api/v1/event-rules/test", tag = "events",
    request_body = EventRuleTest,
    responses(
        (status = 200, description = "Whether the sample matched; a pattern that does not compile is reported in-band", body = RuleTestResult),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
    ),
)]
async fn test_event_rule(
    _guard: RequireManageConfig,
    Json(body): Json<EventRuleTest>,
) -> Json<RuleTestResult> {
    let matcher = match crate::events::compile_matcher(&body.match_kind, &body.pattern) {
        Ok(m) => m,
        Err(e) => return RuleTestResult::failed(format!("pattern: {e}")),
    };
    let clear_matched = match body.clear_pattern.as_deref() {
        Some(clear) => match crate::events::compile_matcher(&body.match_kind, clear) {
            Ok(m) => Some(m.matches(&body.sample)),
            Err(e) => return RuleTestResult::failed(format!("clear_pattern: {e}")),
        },
        None => None,
    };
    Json(RuleTestResult {
        matched: matcher.matches(&body.sample),
        clear_matched,
        error: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::router;
    use crate::api::tests_support::{private_state, public_state};
    use axum::body::{to_bytes, Body};
    use axum::http::{header::AUTHORIZATION, Request};
    use tower::ServiceExt;
    use yagra_common::{Principal, Role, Scope};

    const ID: &str = "00000000-0000-0000-0000-000000000001";

    fn config_routes() -> Vec<(&'static str, String)> {
        vec![
            ("GET", "/api/v1/event-sources".to_owned()),
            ("POST", "/api/v1/event-sources".to_owned()),
            ("PUT", format!("/api/v1/event-sources/{ID}")),
            ("DELETE", format!("/api/v1/event-sources/{ID}")),
            ("POST", format!("/api/v1/event-sources/{ID}/rotate-token")),
            ("GET", "/api/v1/event-rules".to_owned()),
            ("POST", "/api/v1/event-rules".to_owned()),
            ("PUT", format!("/api/v1/event-rules/{ID}")),
            ("DELETE", format!("/api/v1/event-rules/{ID}")),
            ("POST", "/api/v1/event-rules/test".to_owned()),
        ]
    }

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
    async fn event_configuration_is_operator_and_up() {
        // Including the reads: a source row is an ingest endpoint, and a rule decides what raises
        // an alert. Since ADR-057 that is an operator's to hold — deciding what raises an alert is
        // running the monitoring — and a viewer's to be refused.
        for (method, path) in config_routes() {
            assert_eq!(
                send(private_state(), method, &path, None, "{}")
                    .await
                    .status(),
                StatusCode::UNAUTHORIZED,
                "anon {method} {path}"
            );
            assert_eq!(
                send(public_state(), method, &path, None, "{}")
                    .await
                    .status(),
                StatusCode::UNAUTHORIZED,
                "public {method} {path}"
            );
        }
        // What "past the guard" looks like is 503 (skeleton mode has no store) everywhere but the
        // rule tester, which reads no store at all: it gets as far as the body, which this loop
        // sends as `{}`, and answers 422. Naming the code rather than asserting "not 403" is what
        // keeps this two-directional — a guard that admitted nobody would fail here.
        let st = private_state();
        for (role, want) in [
            (Role::Viewer, StatusCode::FORBIDDEN),
            (Role::Operator, StatusCode::SERVICE_UNAVAILABLE),
        ] {
            let token = st
                .sessions
                .issue(Uuid::new_v4(), Principal::new(role, Scope::All), "u");
            for (method, path) in config_routes() {
                let want = if want == StatusCode::SERVICE_UNAVAILABLE
                    && path == "/api/v1/event-rules/test"
                {
                    StatusCode::UNPROCESSABLE_ENTITY
                } else {
                    want
                };
                assert_eq!(
                    send(st.clone(), method, &path, Some(&token), "{}")
                        .await
                        .status(),
                    want,
                    "{role:?} {method} {path}"
                );
            }
        }
    }

    #[tokio::test]
    async fn ingest_takes_a_source_token_not_a_session() {
        // No session guard runs, so an anonymous request reaches the engine check and gets the
        // engine's own 503 — not a 401. That is the contract: the sender is a script with a token.
        let resp = send(
            private_state(),
            "POST",
            &format!("/api/v1/ingest/webhook/{ID}"),
            None,
            "{}",
        )
        .await;
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn the_rule_tester_reports_a_bad_pattern_in_band() {
        // A 400 would render as "the request failed"; the operator needs the compile error next to
        // the field they are typing in.
        let st = private_state();
        let token = st.sessions.issue(
            Uuid::new_v4(),
            Principal::new(Role::Admin, Scope::All),
            "admin1",
        );
        let resp = send(
            st,
            "POST",
            "/api/v1/event-rules/test",
            Some(&token),
            r#"{"match_kind":"regex","pattern":"[unclosed","sample":"x"}"#,
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let json: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["matched"], false);
        assert!(json["error"].as_str().unwrap().starts_with("pattern:"));
    }

    #[test]
    fn webhook_text_extraction_prefers_a_known_field_and_truncates_by_character() {
        let (text, truncated) = extract_webhook_text(br#"{"message":"disk full","x":1}"#);
        assert_eq!(text, "disk full");
        assert!(!truncated);
        // Falls through the preference list, then to the compact JSON, then to raw bytes.
        assert_eq!(extract_webhook_text(br#"{"summary":"s"}"#).0, "s");
        assert_eq!(extract_webhook_text(br#"{"a":1}"#).0, r#"{"a":1}"#);
        assert_eq!(
            extract_webhook_text(b"not json at all").0,
            "not json at all"
        );

        // Truncation is by character, so a multi-byte boundary is never split — slicing mid-UTF-8
        // would panic rather than shorten.
        let long = format!(
            r#"{{"message":"{}"}}"#,
            "あ".repeat(EVENT_TEXT_MAX_CHARS + 10)
        );
        let (cut, truncated) = extract_webhook_text(long.as_bytes());
        assert!(truncated);
        assert_eq!(cut.chars().count(), EVENT_TEXT_MAX_CHARS);
    }

    #[test]
    fn a_rule_whose_pattern_does_not_compile_is_rejected_before_it_is_stored() {
        // Otherwise it loads into the engine snapshot and matches nothing for the rest of its life.
        let rule = |pattern: &str, clear: Option<&str>| EventRuleBody {
            name: "link down".to_owned(),
            enabled: true,
            source_kind: None,
            source_id: None,
            node_id: None,
            match_kind: "regex".to_owned(),
            pattern: pattern.to_owned(),
            clear_pattern: clear.map(str::to_owned),
            severity: "warning".to_owned(),
            ttl_secs: None,
            min_count: None,
            window_secs: None,
        };
        assert!(validate_event_rule(&rule("link.*down", None)).is_ok());
        assert!(validate_event_rule(&rule("[unclosed", None)).is_err());
        // The clear pattern is compiled too — it is what stops the alert, so a broken one leaves
        // the alert stuck open.
        assert!(validate_event_rule(&rule("ok", Some("[unclosed"))).is_err());

        // The numeric bounds mirror the DB CHECKs, so a value the database would reject is a 400
        // here rather than a 500 from the insert.
        let mut ttl = rule("x", None);
        ttl.ttl_secs = Some(1);
        assert_eq!(
            validate_event_rule(&ttl).unwrap_err().code(),
            "invalid_rule"
        );
        let mut sev = rule("x", None);
        sev.severity = "catastrophic".to_owned();
        assert_eq!(
            validate_event_rule(&sev).unwrap_err().code(),
            "invalid_rule"
        );
    }
}
