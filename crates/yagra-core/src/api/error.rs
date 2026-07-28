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

#[derive(Serialize)]
struct ErrorBody {
    error: ErrorDetail,
}

#[derive(Serialize)]
struct ErrorDetail {
    code: String,
    message: String,
}

/// Build the ADR-019 error envelope directly. Retained for the handlers that have not yet moved to
/// [`ApiError`]; new code should return an `ApiError` instead.
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

    /// `403` — authenticated, but the role does not carry the required permission.
    pub fn forbidden() -> Self {
        Self::new(
            StatusCode::FORBIDDEN,
            "forbidden",
            "your role does not permit this action",
        )
    }

    /// `503` — the write side is absent (skeleton mode has no admin state).
    pub fn admin_unavailable() -> Self {
        Self::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "admin_unavailable",
            "inventory/credential management is not available in skeleton mode",
        )
    }

    // A `not_leader()` constructor lands with the events domain: the two handlers that answer
    // `503 not_leader` today still build it through `require_leader(&st)` in `mod.rs`.

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
        }
    }

    /// The status this error renders as. Test-only: production code renders the error rather than
    /// inspecting it, and assertions read better against the status than against a serialized body.
    #[cfg(test)]
    pub fn status(&self) -> StatusCode {
        self.status
    }

    /// The stable machine-readable code clients match on. Test-only, for the same reason.
    #[cfg(test)]
    pub fn code(&self) -> &'static str {
        self.code
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        error_response(self.status, self.code, self.message)
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
