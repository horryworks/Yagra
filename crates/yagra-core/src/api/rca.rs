// SPDX-License-Identifier: AGPL-3.0-only
//! AI-assisted root-cause analysis (ADR-029) — five endpoints across two audiences.
//!
//! **Provider configuration is `ManageConfig`.** It picks which cloud the operator's incident data
//! may cross into and holds a billable credential, so it is an admin concern and its secret is
//! write-only.
//!
//! **Generation is `AckAlerts`, not `ManageConfig`** — deliberately. The people carrying the pager
//! are exactly who needs the explanation; gating it behind admin would put the feature out of reach
//! of its users. What bounds abuse is the orchestrator's rate and concurrency caps, not the role
//! (the conclusion ADR-028 WS-A reached about expensive reads).
//!
//! **Reading a stored report is `View`**, so a viewer can be shown an explanation an operator
//! generated without being able to spend money themselves.
//!
//! Authorization runs before availability on every one of them, including the two that would
//! otherwise 503. Whether an operator has wired up an LLM — and which vendor they chose — is not
//! something an unauthenticated prober should be able to read off a 503-vs-403.

use super::error::{ApiError, ApiResult};
use super::extract::{current_username, Admin, RequireAckAlerts, RequireManageConfig, RequireView};
use super::ApiState;
use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

/// Longest model reply echoed back by the provider test. It is rendered in a toast, and a model
/// that ignores "reply with ok" must not be able to paste an essay into the settings page.
const TEST_REPLY_MAX_CHARS: usize = 200;

/// The RCA routes, merged into `/api/v1` by [`super::router`].
pub(super) fn routes() -> Router<ApiState> {
    Router::new()
        .route(
            "/api/v1/llm/config",
            get(get_llm_config).put(put_llm_config),
        )
        .route("/api/v1/llm/test", post(test_llm_provider))
        .route("/api/v1/rca", post(create_rca))
        .route("/api/v1/rca/:id", get(get_rca))
}

/// The `POST /api/v1/rca` body.
#[derive(Deserialize)]
struct RcaBody {
    /// The alerting node. May be a symptom of an upstream failure — the context builder follows
    /// `root_cause` to the incident before assembling anything.
    node: Uuid,
    check: Uuid,
    /// Timeline window in seconds; clamped by the orchestrator. Defaults to an hour.
    #[serde(default)]
    window_secs: Option<i64>,
    /// UI language tag (`ja`, `en-GB`, …). The instructions stay English; the answer follows the
    /// reader. Unknown tags fall back to English rather than failing.
    #[serde(default)]
    language: Option<String>,
    /// Regenerate instead of serving a cached report. Still rate-limited.
    #[serde(default)]
    force: bool,
}

/// The provider-test outcome. `ok: false` is a *successful* request reporting a failed probe — the
/// provider configuration is the caller's, and the typed error is what tells them which part of it
/// is wrong, so this is not a 5xx.
#[derive(Debug, Serialize)]
pub(crate) struct LlmTestResult {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    latency_ms: Option<u128>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reply: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

/// Map an orchestrator refusal onto a status.
///
/// Each arm is a distinct operator action: fix the configuration, wait, look at a different alert,
/// or check the vendor. Collapsing any two of them would tell the operator to do the wrong thing.
fn rca_error(e: &crate::rca::orchestrator::RcaError) -> ApiError {
    use crate::rca::orchestrator::RcaError;
    match e {
        RcaError::NotConfigured => ApiError::unavailable("rca_not_configured", e.to_string()),
        // Also 503, but a distinct code: the form has been filled in and something in it is wrong,
        // which is a different next step from "nobody has set this up".
        RcaError::Misconfigured(msg) => ApiError::unavailable("rca_misconfigured", msg.clone()),
        // `Retry-After: 60` matches the rate window, so a client that honors it comes back exactly
        // when a slot is guaranteed to have freed.
        RcaError::TooManyConcurrent(_) | RcaError::RateLimited(_) => {
            ApiError::too_many_requests("rate_limited", e.to_string()).retry_after(60)
        }
        RcaError::NoIncident(msg) => ApiError::not_found("no_incident", msg.clone()),
        // 502, not 500: the failure is the upstream vendor's. The text is the *typed* error and
        // never a raw body, which could echo the prompt — and therefore device output — back.
        RcaError::Provider(inner) => ApiError::bad_gateway(
            match inner {
                crate::rca::provider::LlmError::Refused(_) => "model_refused",
                _ => "provider_error",
            },
            inner.to_string(),
        ),
        RcaError::Internal(err) => {
            tracing::error!(error = %err, "RCA generation failed");
            ApiError::internal("failed to generate the analysis")
        }
    }
}

/// Generate (or serve from cache) an explanation of one incident.
async fn create_rca(
    _guard: RequireAckAlerts,
    State(st): State<ApiState>,
    headers: HeaderMap,
    Json(body): Json<RcaBody>,
) -> ApiResult<Json<crate::rca::store::RcaReport>> {
    let rca = st.rca.as_ref().ok_or_else(ApiError::admin_unavailable)?;
    let req = crate::rca::orchestrator::RcaRequest {
        node: yagra_common::NodeId::from(body.node),
        check: yagra_common::CheckId::from(body.check),
        window_secs: body.window_secs,
        language: crate::rca::prompt::Language::from_tag(body.language.as_deref().unwrap_or("en")),
        force: body.force,
        username: current_username(&st, &headers).unwrap_or_else(|| "(unknown)".to_owned()),
    };
    let report = rca.explain(&req).await.map_err(|e| rca_error(&e))?;
    Ok(Json(report))
}

/// Read a stored report. The explanation is display-only text, so showing it to everyone who can
/// see the alert costs nothing and keeps the incident channel on one shared account.
async fn get_rca(
    _guard: RequireView,
    State(st): State<ApiState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<crate::rca::store::RcaReport>> {
    let rca = st.rca.as_ref().ok_or_else(ApiError::admin_unavailable)?;
    match rca.get(id).await {
        Ok(Some(report)) => Ok(Json(report)),
        Ok(None) => Err(ApiError::not_found(
            "not_found",
            format!("no analysis {id}"),
        )),
        Err(e) => Err(rca_error(&e)),
    }
}

/// The active provider's configuration — never the credential.
async fn get_llm_config(_guard: RequireManageConfig, admin: Admin) -> ApiResult<Json<Value>> {
    let view = admin.llm.view().await.map_err(|e| {
        ApiError::from_internal(
            e.as_ref(),
            "read LLM config",
            "failed to read the AI provider configuration",
        )
    })?;
    // `config: null` rather than 404 — "not configured yet" is the normal state, and the form wants
    // to render its empty self, not an error. `providers` ships the choices with their placeholders
    // and their egress warning, so the UI reads the data-boundary classification off the same code
    // the adapters obey instead of restating it.
    Ok(Json(serde_json::json!({
        "config": view,
        "providers": crate::rca::provider_choices(),
    })))
}

/// Create or replace the provider configuration.
async fn put_llm_config(
    _guard: RequireManageConfig,
    admin: Admin,
    Json(body): Json<crate::rca::store::LlmConfigInput>,
) -> ApiResult<StatusCode> {
    // `save` validates before it writes, so a failure here is the caller's field — a 400 naming it
    // beats a 500 that says nothing.
    admin
        .llm
        .save(&body)
        .await
        .map_err(|e| ApiError::bad_request("invalid_llm_config", e.to_string()))?;
    Ok(StatusCode::NO_CONTENT)
}

/// Send one minimal prompt to the configured provider.
///
/// Deliberately ignores `enabled`: validating a provider *before* switching it on is the whole point
/// of the button (the same reasoning as the forwarding-destination test).
async fn test_llm_provider(
    _guard: RequireManageConfig,
    State(st): State<ApiState>,
) -> ApiResult<Json<LlmTestResult>> {
    let rca = st.rca.as_ref().ok_or_else(ApiError::admin_unavailable)?;
    Ok(Json(match rca.test().await {
        Ok((latency_ms, reply)) => LlmTestResult {
            ok: true,
            latency_ms: Some(latency_ms),
            reply: Some(reply.chars().take(TEST_REPLY_MAX_CHARS).collect()),
            error: None,
        },
        Err(e) => LlmTestResult {
            ok: false,
            latency_ms: None,
            reply: None,
            error: Some(e.to_string()),
        },
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
    use yagra_common::{Principal, Role, Scope};

    const ID: &str = "00000000-0000-0000-0000-000000000001";

    async fn body_json(resp: axum::response::Response) -> serde_json::Value {
        let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    fn all_routes() -> Vec<(&'static str, String)> {
        vec![
            ("GET", "/api/v1/llm/config".to_owned()),
            ("PUT", "/api/v1/llm/config".to_owned()),
            ("POST", "/api/v1/llm/test".to_owned()),
            ("POST", "/api/v1/rca".to_owned()),
            ("GET", format!("/api/v1/rca/{ID}")),
        ]
    }

    /// A well-formed RCA request. Needed wherever the assertion is about what happens *after* the
    /// guard: an empty object is rejected by the body extractor with 422 before the handler runs.
    fn rca_body() -> String {
        format!(r#"{{"node":"{ID}","check":"{ID}"}}"#)
    }

    async fn status_of(st: ApiState, method: &str, path: &str, token: Option<&str>) -> StatusCode {
        let body = if path == "/api/v1/rca" && method == "POST" {
            rca_body()
        } else {
            "{}".to_owned()
        };
        let mut b = Request::builder()
            .method(method)
            .uri(path)
            .header("content-type", "application/json");
        if let Some(t) = token {
            b = b.header(AUTHORIZATION, format!("Bearer {t}"));
        }
        router(st)
            .oneshot(b.body(Body::from(body)).unwrap())
            .await
            .unwrap()
            .status()
    }

    #[tokio::test]
    async fn no_rca_route_reveals_whether_an_llm_is_configured_to_a_stranger() {
        // Every one of them authenticates before consulting `st.rca`. A 503 here would tell an
        // anonymous prober that this deployment has (or has not) wired up a vendor.
        for (method, path) in all_routes() {
            assert_eq!(
                status_of(private_state(), method, &path, None).await,
                StatusCode::UNAUTHORIZED,
                "anon {method} {path}"
            );
        }
        // Reading a report is `View`, so a public dashboard reaches availability (503) — but the
        // four that spend money or expose configuration stay closed even there.
        assert_eq!(
            status_of(public_state(), "GET", &format!("/api/v1/rca/{ID}"), None).await,
            StatusCode::SERVICE_UNAVAILABLE,
        );
        for (method, path) in [
            ("GET", "/api/v1/llm/config"),
            ("PUT", "/api/v1/llm/config"),
            ("POST", "/api/v1/llm/test"),
            ("POST", "/api/v1/rca"),
        ] {
            assert_eq!(
                status_of(public_state(), method, path, None).await,
                StatusCode::UNAUTHORIZED,
                "public {method} {path}"
            );
        }
    }

    #[tokio::test]
    async fn generating_is_for_operators_and_configuring_is_for_admins() {
        // The split is the point: a viewer can read an explanation but not spend money, and an
        // operator can generate one but not choose which cloud the incident data crosses into.
        let st = private_state();
        let viewer = st.sessions.issue(
            Uuid::new_v4(),
            Principal::new(Role::Viewer, Scope::All),
            "v",
        );
        let operator = st.sessions.issue(
            Uuid::new_v4(),
            Principal::new(Role::Operator, Scope::All),
            "o",
        );

        // Viewer: may read a report (past the guard ⇒ 503), may not generate.
        assert_eq!(
            status_of(
                st.clone(),
                "GET",
                &format!("/api/v1/rca/{ID}"),
                Some(&viewer)
            )
            .await,
            StatusCode::SERVICE_UNAVAILABLE,
        );
        assert_eq!(
            status_of(st.clone(), "POST", "/api/v1/rca", Some(&viewer)).await,
            StatusCode::FORBIDDEN,
        );

        // Operator: may generate (past the guard ⇒ 503), may not touch provider configuration.
        assert_eq!(
            status_of(st.clone(), "POST", "/api/v1/rca", Some(&operator)).await,
            StatusCode::SERVICE_UNAVAILABLE,
        );
        for (method, path) in [
            ("GET", "/api/v1/llm/config"),
            ("PUT", "/api/v1/llm/config"),
            ("POST", "/api/v1/llm/test"),
        ] {
            assert_eq!(
                status_of(st.clone(), method, path, Some(&operator)).await,
                StatusCode::FORBIDDEN,
                "operator {method} {path}"
            );
        }
    }

    #[test]
    fn every_refusal_maps_to_a_distinct_actionable_status() {
        // Each of these asks the operator for a different thing — fix the config, wait, look
        // elsewhere, chase the vendor — so collapsing any two into one status would lose that.
        use crate::rca::orchestrator::RcaError;
        use crate::rca::provider::LlmError;
        let cases = [
            (RcaError::NotConfigured, StatusCode::SERVICE_UNAVAILABLE),
            (
                RcaError::Misconfigured("an API key is required".to_owned()),
                StatusCode::SERVICE_UNAVAILABLE,
            ),
            (RcaError::RateLimited(10), StatusCode::TOO_MANY_REQUESTS),
            (
                RcaError::TooManyConcurrent(2),
                StatusCode::TOO_MANY_REQUESTS,
            ),
            (
                RcaError::NoIncident("gone".to_owned()),
                StatusCode::NOT_FOUND,
            ),
            (
                RcaError::Provider(LlmError::Timeout),
                StatusCode::BAD_GATEWAY,
            ),
            (
                RcaError::Provider(LlmError::Refused(None)),
                StatusCode::BAD_GATEWAY,
            ),
        ];
        for (err, want) in cases {
            let resp = rca_error(&err).into_response();
            assert_eq!(resp.status(), want, "{err:?}");
            // A rate refusal must say when to come back, or a UI retry loop just hammers the
            // window it is already saturating.
            if want == StatusCode::TOO_MANY_REQUESTS {
                assert!(resp.headers().contains_key(axum::http::header::RETRY_AFTER));
            }
        }
    }

    #[tokio::test]
    async fn a_model_refusal_never_reads_as_an_outage() {
        // The model declining is a different outcome from the vendor being broken: nobody should
        // go page a cloud provider over a safety filter.
        use crate::rca::orchestrator::RcaError;
        use crate::rca::provider::LlmError;
        let resp = rca_error(&RcaError::Provider(LlmError::Refused(None))).into_response();
        assert_eq!(body_json(resp).await["error"]["code"], "model_refused");
    }

    #[tokio::test]
    async fn an_incomplete_configuration_says_what_is_missing() {
        // "Not set up" and "set up wrong" are both 503, but they are different jobs for the
        // operator, so they carry different codes and the second carries the reason.
        use crate::rca::orchestrator::RcaError;
        let resp = rca_error(&RcaError::Misconfigured(
            "an Anthropic API key is required".to_owned(),
        ))
        .into_response();
        let json = body_json(resp).await;
        assert_eq!(json["error"]["code"], "rca_misconfigured");
        assert!(json["error"]["message"]
            .as_str()
            .unwrap()
            .contains("API key"));
    }
}
