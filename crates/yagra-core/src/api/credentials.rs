// SPDX-License-Identifier: AGPL-3.0-only
//! Stored monitoring credentials — SNMP communities, SNMPv3 USM documents, device logins.
//!
//! `ManageCredentials`, its own permission rather than `ManageConfig`: these are what the whole
//! fleet is polled with, so holding them is a strictly larger power than editing what gets polled.
//!
//! **The secret is write-only and never leaves this process in the clear.** It is sealed with
//! envelope encryption before it reaches the database (ADR-018), the listing carries metadata only,
//! and nothing here logs or echoes a secret — including the validation errors, which name a static
//! reason and never any field content (security.md).
//!
//! An update with no `secret` is a rename: the stored secret is left intact rather than cleared.
//! With one, `kind` must accompany it, because the secret's *format* is kind-specific and re-sealing
//! a v3 document as a community string would produce a credential that silently fails every poll.

use super::error::{ApiError, ApiResult};
use super::extract::{Admin, RequireManageCredentials};
use super::util::CreatedId;
use super::ApiState;
use axum::{
    extract::Path,
    http::StatusCode,
    routing::{get, put},
    Json, Router,
};
use serde::Deserialize;
use uuid::Uuid;

/// The credential routes, merged into `/api/v1` by [`super::router`].
pub(super) fn routes() -> Router<ApiState> {
    Router::new()
        .route(
            "/api/v1/credentials",
            get(list_credentials).post(create_credential),
        )
        .route(
            "/api/v1/credentials/:id",
            put(update_credential).delete(delete_credential),
        )
}

/// Reject a structurally invalid secret for its kind, at the edge.
///
/// Only SNMPv3 has a parseable shape today. Checking it here means a malformed USM document is a
/// 400 the operator can act on, rather than a stored credential that fails every poll with an error
/// only the poller logs. **The reason is the parser's static text and never any field content.**
fn check_secret_shape(kind: &str, secret: &[u8]) -> Result<(), ApiError> {
    if kind == crate::secrets::KIND_SNMP_V3 {
        if let Err(reason) = crate::secrets::SnmpV3Secret::parse(secret) {
            return Err(ApiError::bad_request(
                "invalid_credential",
                format!("invalid SNMPv3 credential: {reason}"),
            ));
        }
    }
    Ok(())
}

async fn list_credentials(
    _guard: RequireManageCredentials,
    admin: Admin,
) -> ApiResult<Json<Vec<crate::secrets::CredentialSummary>>> {
    // Metadata only — the summary carries no secret, which is what makes this listable at all.
    let list = admin.creds.list().await.map_err(|e| {
        ApiError::from_internal(e.as_ref(), "list credentials", "failed to list credentials")
    })?;
    Ok(Json(list))
}

/// Create body. `secret` is sealed before storage and never logged.
#[derive(Deserialize)]
struct CreateCredential {
    name: String,
    kind: String,
    secret: String,
}

async fn create_credential(
    _guard: RequireManageCredentials,
    admin: Admin,
    Json(body): Json<CreateCredential>,
) -> ApiResult<(StatusCode, Json<CreatedId>)> {
    let (name, kind) = (body.name.trim(), body.kind.trim());
    if name.is_empty() || kind.is_empty() || body.secret.is_empty() {
        return Err(ApiError::bad_request(
            "invalid_credential",
            "name, kind, and secret are required",
        ));
    }
    check_secret_shape(kind, body.secret.as_bytes())?;
    let id = admin
        .creds
        .create(name, kind, body.secret.as_bytes())
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "create credential",
                "failed to store credential",
            )
        })?;
    Ok((StatusCode::CREATED, Json(CreatedId { id })))
}

/// Update body. `name` is required; `secret` is optional — see the module doc for why omitting it
/// is a rename and supplying it requires `kind`.
#[derive(Deserialize)]
struct UpdateCredential {
    name: String,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    secret: Option<String>,
}

async fn update_credential(
    _guard: RequireManageCredentials,
    admin: Admin,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateCredential>,
) -> ApiResult<StatusCode> {
    let name = body.name.trim();
    if name.is_empty() {
        return Err(ApiError::bad_request(
            "invalid_credential",
            "name is required",
        ));
    }
    // A blank secret is "no change", not "clear it": clearing would leave a credential that
    // authenticates as nothing, which is worse than the old value.
    let secret = body.secret.as_deref().filter(|s| !s.is_empty());
    let reseal = match secret {
        Some(secret) => {
            let Some(kind) = body
                .kind
                .as_deref()
                .map(str::trim)
                .filter(|k| !k.is_empty())
            else {
                return Err(ApiError::bad_request(
                    "invalid_credential",
                    "kind is required when changing the secret",
                ));
            };
            check_secret_shape(kind, secret.as_bytes())?;
            Some((kind, secret.as_bytes()))
        }
        None => None,
    };
    let updated = admin.creds.update(id, name, reseal).await.map_err(|e| {
        ApiError::from_internal(
            e.as_ref(),
            "update credential",
            "failed to update credential",
        )
    })?;
    if updated {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found(
            "credential_not_found",
            format!("no credential {id}"),
        ))
    }
}

async fn delete_credential(
    _guard: RequireManageCredentials,
    admin: Admin,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    match admin.creds.delete(id).await {
        Ok(true) => Ok(StatusCode::NO_CONTENT),
        Ok(false) => Err(ApiError::not_found(
            "credential_not_found",
            format!("no credential {id}"),
        )),
        Err(e) => Err(ApiError::from_internal(
            e.as_ref(),
            "delete credential",
            "failed to delete credential",
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
            ("GET", "/api/v1/credentials".to_owned()),
            ("POST", "/api/v1/credentials".to_owned()),
            ("PUT", format!("/api/v1/credentials/{ID}")),
            ("DELETE", format!("/api/v1/credentials/{ID}")),
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
    async fn the_credential_store_is_closed_to_everyone_below_admin() {
        // Including the listing: it names what the fleet is polled with, which is a map of what to
        // go after even without the secrets themselves.
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
    fn a_malformed_v3_document_is_rejected_before_it_is_sealed() {
        // Otherwise it is stored successfully and then fails every poll, with the reason visible
        // only in the poller's log.
        let err =
            check_secret_shape(crate::secrets::KIND_SNMP_V3, b"not a usm document").unwrap_err();
        assert_eq!(err.code(), "invalid_credential");
        // The reason is the parser's static text. Nothing from the submitted secret may appear.
        assert!(!err.message().contains("not a usm document"));

        // A community string has no parseable shape, so anything non-empty is accepted here.
        assert!(check_secret_shape("snmp_v2c", b"public").is_ok());
    }
}
