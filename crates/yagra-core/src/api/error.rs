// SPDX-License-Identifier: AGPL-3.0-only
//! The typed API error, and the one place a failure becomes an HTTP response.
//!
//! Every northbound failure is a value of [`ApiError`] that renders itself into the fixed ADR-019
//! envelope `{"error": {"code", "message"}}`. Handlers return `Result<T, ApiError>` and propagate
//! with `?`, so the status/code mapping for a given kind of failure exists exactly once.
//!
//! Before this, handlers returned a bare `Response` and built errors by hand at ~350 sites — the
//! same six-line `Err(e) => { tracing::error!(…); internal("failed to …") }` arm copied per
//! handler, plus four different return shapes (`Response`, `Result<_, Response>`,
//! `Result<_, Box<Response>>` — the box forced by `clippy::result_large_err` — and
//! `Option<Response>` for guards). Nothing propagated with `?`, so every new handler re-derived
//! the mapping and could quietly pick a different status for the same condition.
//!
//! **Security contract (security.md):** an internal error's own text never reaches the client.
//! [`ApiError::internal`] carries a fixed operator-facing sentence; the underlying error is logged
//! at the point it is converted, so the detail lands in the logs and only there.

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;

/// The ADR-019 envelope every failure renders as. `pub(crate)` and schema-bearing so the OpenAPI
/// document can name one error shape for every endpoint (ADR-035) instead of leaving 4xx/5xx bodies
/// undescribed — a client that has to guess the failure shape ends up parsing the success shape and
/// reading `undefined`.
#[derive(Serialize, utoipa::ToSchema)]
#[schema(as = ApiErrorBody)]
pub(crate) struct ErrorBody {
    error: ErrorDetail,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(crate) struct ErrorDetail {
    /// Stable machine-readable code. Clients branch on this, never on the message.
    code: String,
    /// Operator-facing sentence. Safe to display; never carries an internal error's own text.
    message: String,
}

/// Build the ADR-019 error envelope directly — the one place it is rendered.
///
/// It has two callers, and the second is the reason this is not private: [`ApiError`]'s
/// `IntoResponse` below, and the `/mcp` edge (`mcp/mod.rs`), which refuses an unauthenticated or
/// unpermitted caller in its own tower layer *before* any handler runs and so has no `ApiError` to
/// return. An API handler must not call this — it returns `ApiResult` and lets `ApiError` render
/// (`api-conventions.md`); there is no hand-built-response path left to match.
pub(crate) fn error_response(status: StatusCode, code: &str, message: String) -> Response {
    (
        status,
        Json(ErrorBody {
            error: ErrorDetail {
                code: code.to_owned(),
                message,
            },
        }),
    )
        .into_response()
}

/// A northbound API failure: a status, a stable machine-readable code, and an operator-facing
/// message. Construct with the helpers below rather than by hand so the status↔code pairing stays
/// consistent across domains.
#[derive(Debug, Clone)]
pub struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
    /// Seconds for a `Retry-After` header, when the refusal is a timed one.
    retry_after_secs: Option<u64>,
}

impl ApiError {
    /// `400` — the request itself is malformed or fails edge validation.
    pub fn bad_request(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, code, message)
    }

    /// `404` — the addressed resource does not exist.
    pub fn not_found(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, code, message)
    }

    /// `409` — the request conflicts with current state (duplicate name, last admin, …).
    pub fn conflict(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, code, message)
    }

    /// `413` — the body is too large for an edge cap. Its own status rather than a 400 so the UI
    /// can tell "this document is too big" (shrink it) from "this document is malformed" (fix it).
    pub fn payload_too_large(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::PAYLOAD_TOO_LARGE, code, message)
    }

    /// `507` — the request is fine but this host has nowhere to put the result. Distinct from
    /// `413`: the payload is within the accepted size, so the operator's move is to free space or
    /// upgrade from the command line, not to send something smaller.
    pub fn insufficient_storage(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::INSUFFICIENT_STORAGE, code, message)
    }

    /// `502` — an upstream this endpoint depends on (an IdP, a cloud API) misbehaved. Distinct from
    /// `500`: the fault is outside Yagra, so the operator's next move is to check that system.
    pub fn bad_gateway(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_GATEWAY, code, message)
    }

    /// `429` — refused for capacity or rate, and **retryable**. Distinct from `503` (a subsystem
    /// this deployment does not have) and from `500` (a fault): a client that cannot tell the
    /// three apart either hammers a full queue or gives up on a transient refusal.
    pub fn too_many_requests(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::TOO_MANY_REQUESTS, code, message)
    }

    /// `503` with a caller-chosen code — a subsystem this endpoint needs is not configured
    /// (e.g. `flow_unavailable` when no ClickHouse flow store is set up).
    pub fn unavailable(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::SERVICE_UNAVAILABLE, code, message)
    }

    /// `401` — no valid bearer token.
    pub fn unauthorized() -> Self {
        Self::new(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "a valid bearer token is required",
        )
    }

    /// `401` with a caller-chosen code. For the login path, which must distinguish "you sent no
    /// token" from "those credentials are wrong" — the UI shows different things for each.
    pub fn unauthorized_with(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, code, message)
    }

    /// `403` — authenticated, but the role does not carry the required permission.
    pub fn forbidden() -> Self {
        Self::new(
            StatusCode::FORBIDDEN,
            "forbidden",
            "your role does not permit this action",
        )
    }

    /// `403` with a specific code — for a refusal the client should be able to tell apart from a
    /// plain role failure (e.g. an endpoint that cannot yet honour a group scope, which is a
    /// property of the endpoint rather than of the caller's role).
    pub fn forbidden_code(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, code, message)
    }

    /// `503` — the write side is absent (skeleton mode has no admin state).
    pub fn admin_unavailable() -> Self {
        Self::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "admin_unavailable",
            "inventory/credential management is not available in skeleton mode",
        )
    }

    /// `503` — this core is an HA standby (ADR-016).
    ///
    /// Distinct from [`Self::admin_unavailable`]: the deployment is fine, this *instance* is not the
    /// one that can serve the request. A client should retry against the leader, which `/readyz`
    /// identifies, rather than concluding the subsystem is missing.
    pub fn not_leader() -> Self {
        Self::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "not_leader",
            "this core is a standby; the request must be routed to the leader",
        )
    }

    /// `500` — an internal failure. `what` is the operator-facing sentence ("failed to list
    /// thresholds"); the underlying error is **not** included, and must be logged by the caller
    /// (usually via [`ApiError::from_internal`], which logs it for you).
    pub fn internal(what: impl Into<String>) -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            what.into(),
        )
    }

    /// `500` with a caller-chosen code, for the rare internal failure a client must be able to
    /// **distinguish** rather than merely retry.
    ///
    /// Deliberately narrow, and not the general-purpose 500: [`ApiError::internal`] exists so that
    /// a fault reveals nothing, and a code that varies per call site is a channel for revealing
    /// something. Use it only where the refusal itself is the information — the support bundle's
    /// redaction stop (ADR-045) is one, because "we found a secret and released nothing" is an
    /// instruction to the operator, not a transient fault to retry past.
    pub fn internal_with_code(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, code, message)
    }

    /// Log an internal error and turn it into a `500` that reveals nothing about it. This is the
    /// idiom that replaces the hand-copied `Err(e) => { tracing::error!(…); internal(…) }` arm:
    ///
    /// ```ignore
    /// let rows = admin.thresholds.list().await
    ///     .map_err(|e| ApiError::from_internal(&e, "list thresholds", "failed to list thresholds"))?;
    /// ```
    pub fn from_internal(
        err: &(dyn std::error::Error + 'static),
        context: &str,
        public: impl Into<String>,
    ) -> Self {
        tracing::error!(error = %err, "{context} failed");
        Self::internal(public)
    }

    fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
            retry_after_secs: None,
        }
    }

    /// Attach a `Retry-After` header to a timed refusal.
    ///
    /// The machine-readable half of a 429: without it a client that is being throttled has to guess
    /// how long to wait, and guessing wrong is what turns a rate limit into a hot loop against it.
    #[must_use]
    pub fn retry_after(mut self, secs: u64) -> Self {
        self.retry_after_secs = Some(secs);
        self
    }

    /// The status this error renders as.
    ///
    /// HTTP handlers never need this — they return the error and let [`IntoResponse`] render it.
    /// It exists for the **non-HTTP** surface: the MCP server calls the same service functions and
    /// must translate their failures into JSON-RPC, which it does by matching on this rather than
    /// by re-deriving the mapping per tool.
    #[must_use]
    pub fn status(&self) -> StatusCode {
        self.status
    }

    /// The stable machine-readable code clients match on.
    #[must_use]
    pub fn code(&self) -> &'static str {
        self.code
    }

    /// The operator-facing message.
    ///
    /// Safe to forward to any client: every constructor takes it as a fixed sentence, and
    /// [`ApiError::from_internal`] deliberately keeps the underlying error out of it (security.md).
    /// `an_internal_error_never_leaks_the_underlying_message` is what holds that true.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut resp = error_response(self.status, self.code, self.message);
        if let Some(secs) = self.retry_after_secs {
            if let Ok(v) = axum::http::HeaderValue::from_str(&secs.to_string()) {
                resp.headers_mut()
                    .insert(axum::http::header::RETRY_AFTER, v);
            }
        }
        resp
    }
}

/// An API handler's result. `Ok` is whatever the handler returns (usually `Json<T>`); `Err` renders
/// itself, so a handler body can `?` its way through fallible work.
pub type ApiResult<T> = Result<T, ApiError>;

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;

    async fn body_json(resp: Response) -> serde_json::Value {
        let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn renders_the_adr019_envelope() {
        let resp = ApiError::not_found("node_not_found", "no node abc").into_response();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let json = body_json(resp).await;
        assert_eq!(json["error"]["code"], "node_not_found");
        assert_eq!(json["error"]["message"], "no node abc");
    }

    #[test]
    fn each_constructor_pairs_its_status_with_a_stable_code() {
        // The pairing is the contract clients match on, so pin it rather than trust the helpers.
        for (err, status, code) in [
            (
                ApiError::bad_request("invalid_name", "x"),
                StatusCode::BAD_REQUEST,
                "invalid_name",
            ),
            (
                ApiError::conflict("last_admin", "x"),
                StatusCode::CONFLICT,
                "last_admin",
            ),
            (
                ApiError::unavailable("flow_unavailable", "x"),
                StatusCode::SERVICE_UNAVAILABLE,
                "flow_unavailable",
            ),
            (
                ApiError::unauthorized(),
                StatusCode::UNAUTHORIZED,
                "unauthorized",
            ),
            (ApiError::forbidden(), StatusCode::FORBIDDEN, "forbidden"),
            (
                ApiError::admin_unavailable(),
                StatusCode::SERVICE_UNAVAILABLE,
                "admin_unavailable",
            ),
            (
                ApiError::internal("failed to x"),
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
            ),
        ] {
            assert_eq!(err.status(), status, "status for {code}");
            assert_eq!(err.code(), code);
        }
    }

    #[tokio::test]
    async fn an_internal_error_never_leaks_the_underlying_message() {
        // security.md: the client learns that it failed, never why. The cause goes to the log.
        let cause = std::io::Error::other("connection to postgres://user:hunter2@db failed");
        let resp = ApiError::from_internal(&cause, "list thresholds", "failed to list thresholds")
            .into_response();
        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let json = body_json(resp).await;
        assert_eq!(json["error"]["message"], "failed to list thresholds");
        let rendered = json.to_string();
        assert!(!rendered.contains("hunter2"), "{rendered}");
        assert!(!rendered.contains("postgres"), "{rendered}");
    }
}
