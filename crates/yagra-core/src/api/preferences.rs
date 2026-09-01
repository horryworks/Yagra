// SPDX-License-Identifier: AGPL-3.0-only
//! Per-account WebUI preferences: one opaque JSON document per signed-in account (ADR-058).
//!
//! These are the settings that follow a person between machines rather than living in one browser's
//! `localStorage`. The document is opaque to the backend — the WebUI owns its shape and migrates it
//! client-side — so this endpoint validates only what the server can be responsible for: that the
//! body is a JSON object, and that it is not large enough to be an attack.
//!
//! **The opacity is the point, not a shortcut.** A column per preference would put every future
//! checkbox on both sides of the wire: a migration, a DTO, an OpenAPI regeneration, generated
//! TypeScript and an N/N-1 argument, each time. This way the *second* preference costs nothing here.
//! ⚠️ The corollary: anything the **backend** must read does not belong in this document, because
//! nothing on this side can validate, query or migrate it.
//!
//! [`Caller`] rather than `RequireView`, for the same reason My Dashboard uses it: the row is keyed
//! by the caller's username, and `RequireView` short-circuits `Ok` in public-dashboard mode — so
//! opening this the way other reads open would give every anonymous visitor of a public deployment
//! the same shared, nameless preferences row. It also refuses an API token, which names no person.

use super::error::{ApiError, ApiResult};
use super::extract::{Admin, Caller};
use super::util::validate_opaque_doc;
use super::ApiState;
use axum::{routing::get, Json, Router};
use serde::Serialize;
use serde_json::Value;

/// This domain's slice of the OpenAPI document (ADR-035), merged by [`super::openapi::document`].
#[derive(utoipa::OpenApi)]
#[openapi(paths(get_preferences, put_preferences))]
pub(super) struct Doc;

/// Edge cap on a preferences document.
///
/// Deliberately far below [`super::util::MAX_JSON_DOC_BYTES`] (256 KiB). That constant is the cap
/// for an **operator-authored** document — a dashboard layout, a report spec — something a person
/// composes and looks at. This is **machine-written UI chrome**: a flat map of scalars the browser
/// writes without anyone reading it. 16 KiB holds several hundred such keys, and the difference
/// matters because this table has one row *per account*, so the cap multiplies by the user count
/// rather than standing alone.
///
/// ⚠️ One-way: raising it later is backward-compatible, lowering it silently 413s documents that
/// already exist in the database.
const MAX_USER_PREFS_BYTES: usize = 16_384;

/// A save's acknowledgement. The document is not echoed back — the client already has it.
#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct PreferencesSaved {
    ok: bool,
}

/// The preferences routes, merged into `/api/v1` by [`super::router`].
pub(super) fn routes() -> Router<ApiState> {
    Router::new().route(
        "/api/v1/preferences",
        get(get_preferences).put(put_preferences),
    )
}

/// Reject a document that is not a JSON object, or is too big to be real preferences.
fn validate_prefs(body: &Value) -> Result<(), ApiError> {
    validate_opaque_doc(
        body,
        "invalid_preferences",
        "preferences_too_large",
        "preferences document",
        MAX_USER_PREFS_BYTES,
    )
}

/// The caller's saved preferences, or JSON `null` when they have never saved any — an explicit null
/// rather than 404, so the client keeps its browser-local values instead of showing an error.
///
/// That choice is load-bearing for N-1: a core that predates this endpoint answers a bodyless 404,
/// and `null` means the same thing to the client, so the WebUI has **one** fallback path rather
/// than two.
#[utoipa::path(
    get, path = "/api/v1/preferences", tag = "preferences",
    responses(
        (status = 200, description = "The caller's opaque preferences document, or JSON null when they have never saved one", body = serde_json::Value),
        (status = 401, description = "No valid bearer token — preferences are keyed by account, so this stays closed in public-dashboard mode", body = super::error::ErrorBody),
        (status = 403, description = "An API token names no person, so it cannot read someone's preferences", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode has no write side", body = super::error::ErrorBody),
    ),
)]
async fn get_preferences(caller: Caller, admin: Admin) -> ApiResult<Json<Value>> {
    let prefs = admin
        .prefs
        .get_for_user(&caller.0.username)
        .await
        .map_err(|e| {
            ApiError::from_internal(e.as_ref(), "get preferences", "failed to load preferences")
        })?;
    Ok(Json(prefs.unwrap_or(Value::Null)))
}

/// Save (replace) the caller's preferences. Mutating, so `audit_mw` records it automatically.
///
/// ⚠️ There is no per-route audit opt-out, so **every** save writes one row. Debouncing on the
/// client is therefore a precondition of this endpoint, not a nicety — a control that saved per
/// pointer event would flood the audit log and the backend has no defence against it.
#[utoipa::path(
    put, path = "/api/v1/preferences", tag = "preferences",
    request_body = serde_json::Value,
    responses(
        (status = 200, description = "Preferences saved", body = PreferencesSaved),
        (status = 400, description = "The preferences document is not a JSON object", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token — preferences are keyed by account, so this stays closed in public-dashboard mode", body = super::error::ErrorBody),
        (status = 403, description = "An API token names no person, so it cannot write someone's preferences", body = super::error::ErrorBody),
        (status = 404, description = "The session's account no longer exists", body = super::error::ErrorBody),
        (status = 413, description = "The preferences document exceeds the size cap", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode has no write side", body = super::error::ErrorBody),
    ),
)]
async fn put_preferences(
    caller: Caller,
    admin: Admin,
    Json(body): Json<Value>,
) -> ApiResult<Json<PreferencesSaved>> {
    validate_prefs(&body)?;
    let saved = admin
        .prefs
        .upsert_for_user(&caller.0.username, &body)
        .await
        .map_err(|e| {
            ApiError::from_internal(e.as_ref(), "save preferences", "failed to save preferences")
        })?;
    if !saved {
        // A valid session whose account vanished mid-request.
        return Err(ApiError::not_found(
            "user_not_found",
            "no such user account",
        ));
    }
    Ok(Json(PreferencesSaved { ok: true }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::router;
    use crate::api::tests_support::{private_state, public_state};
    use crate::api::util::MAX_JSON_DOC_BYTES;
    use axum::body::Body;
    use axum::http::{header::AUTHORIZATION, Request, StatusCode};
    use tower::ServiceExt;
    use uuid::Uuid;
    use yagra_common::{Principal, Role, Scope};

    async fn status_of(
        st: ApiState,
        method: &str,
        path: &str,
        token: Option<&str>,
        body: &str,
    ) -> StatusCode {
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
            .status()
    }

    #[tokio::test]
    async fn every_preferences_route_answers_an_anonymous_caller_with_401() {
        // `Caller` before `Admin`: never the 503 that would reveal whether this deployment has a
        // write side.
        for method in ["GET", "PUT"] {
            assert_eq!(
                status_of(private_state(), method, "/api/v1/preferences", None, "{}").await,
                StatusCode::UNAUTHORIZED,
                "{method}"
            );
        }
    }

    #[tokio::test]
    async fn preferences_need_an_account_even_on_a_public_dashboard() {
        // Keyed by username. Opening it the way other reads open would give every anonymous visitor
        // of a public deployment the same shared, nameless preferences row.
        for method in ["GET", "PUT"] {
            assert_eq!(
                status_of(public_state(), method, "/api/v1/preferences", None, "{}").await,
                StatusCode::UNAUTHORIZED,
                "{method}"
            );
        }
    }

    #[tokio::test]
    async fn a_signed_in_viewer_clears_the_gate_and_reaches_availability() {
        // The positive control. Without it, a guard that rejected *everyone* would look correct —
        // every other test here asserts a refusal.
        //
        // ⚠️ There is deliberately no role-based 403 test, because none can exist: `rbac.rs` grants
        // `Permission::View` to every role and `Caller` demands only `View`. The reachable 403 on
        // these routes is the API-token path, covered below. Do not "fix" this gap by adding a role
        // test — it could not fail.
        let st = private_state();
        let token = st.sessions.issue(
            Uuid::new_v4(),
            Principal::new(Role::Viewer, Scope::All),
            "v",
        );
        for method in ["GET", "PUT"] {
            assert_eq!(
                status_of(
                    st.clone(),
                    method,
                    "/api/v1/preferences",
                    Some(&token),
                    "{}"
                )
                .await,
                StatusCode::SERVICE_UNAVAILABLE,
                "{method}"
            );
        }
    }

    #[test]
    fn a_preferences_document_must_be_an_object_and_within_the_size_cap() {
        assert!(validate_prefs(&serde_json::json!({})).is_ok());
        // An array or a scalar is not a preferences document; accepting one would store something
        // the WebUI cannot read back.
        for bad in [
            serde_json::json!([]),
            serde_json::json!("x"),
            Value::Null,
            serde_json::json!(1),
        ] {
            assert_eq!(
                validate_prefs(&bad).unwrap_err().code(),
                "invalid_preferences",
                "{bad}"
            );
        }
        let huge = serde_json::json!({ "big": "x".repeat(MAX_USER_PREFS_BYTES) });
        assert_eq!(
            validate_prefs(&huge).unwrap_err().code(),
            "preferences_too_large"
        );
    }

    #[test]
    fn the_preferences_cap_stays_below_the_operator_authored_document_cap() {
        // The divergence is deliberate (see the const's doc): this document is machine-written and
        // stored one row per account, so its cap multiplies by the user count. Asserted rather than
        // only commented, so nobody tidies it into an alias of the shared constant.
        // A `const` block so it fails at compile time rather than at test time — both operands are
        // constants, so there is nothing to learn from running it (clippy asks for this form).
        const { assert!(MAX_USER_PREFS_BYTES < MAX_JSON_DOC_BYTES) };
    }
    // ── An accepted write (ADR-115) ──────────────────────────────────────────────────

    /// Preferences are stored against the calling account and read back.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn preferences_round_trip_for_the_calling_account(pool: sqlx::PgPool) {
        use crate::api::tests_support::{account_token, live_state, send};
        let st = live_state(pool.clone()).await;
        let (tok, _) = account_token(&st, "fixture-prefs", yagra_common::Role::Viewer).await;
        let (status, body) = send(
            &st,
            "PUT",
            "/api/v1/preferences",
            &tok,
            Some(serde_json::json!({ "density": "compact" })),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::OK, "{body}");
        assert_eq!(crate::pgtest::rows(&pool, "user_preferences").await, 1);

        let (status, read) = send(&st, "GET", "/api/v1/preferences", &tok, None).await;
        assert_eq!(status, axum::http::StatusCode::OK, "{read}");
        assert!(read.to_string().contains("compact"), "{read}");
    }
}
