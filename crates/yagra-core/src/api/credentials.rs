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

/// This domain's slice of the OpenAPI document (ADR-035), merged by [`super::openapi::document`].
#[derive(utoipa::OpenApi)]
#[openapi(paths(
    list_credentials,
    credential_health,
    create_credential,
    update_credential,
    delete_credential
))]
pub(super) struct Doc;

/// The credential routes, merged into `/api/v1` by [`super::router`].
pub(super) fn routes() -> Router<ApiState> {
    Router::new()
        .route(
            "/api/v1/credentials",
            get(list_credentials).post(create_credential),
        )
        .route("/api/v1/credentials/health", get(credential_health))
        .route(
            "/api/v1/credentials/:id",
            put(update_credential).delete(delete_credential),
        )
}

/// A credential the current KEK cannot open. Carries identity only — never a length, a `key_id`,
/// or anything derived from the ciphertext.
#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
pub(crate) struct UndecryptableCredential {
    id: Uuid,
    name: String,
    kind: String,
}

/// Whether the stored credentials can actually be decrypted with the KEK this process loaded.
#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
pub(crate) struct CredentialHealth {
    /// How many credentials are stored.
    total: u32,
    /// How many of them the current key opened successfully.
    decryptable: u32,
    /// The ones that failed, if any. A non-empty list means polling with those credentials will
    /// fail until the correct key file is restored.
    failures: Vec<UndecryptableCredential>,
}

/// Report whether every stored credential can still be decrypted.
///
/// This is the check a restore cannot skip. A database can come back whole — right row counts,
/// healthy API — while the key-encryption key is a different one, in which case every credential is
/// permanently unreadable and nothing says so until the next poll fails. `scripts/yagra-restore-verify.sh`
/// asserts on this endpoint for exactly that reason, and it is worth looking at after any KEK
/// rotation or restore.
///
/// It decrypts in memory and reports booleans; no secret value crosses this boundary.
#[utoipa::path(
    get, path = "/api/v1/credentials/health", tag = "credentials",
    responses(
        (status = 200, description = "Per-credential decryptability. `failures` is empty on a healthy deployment", body = CredentialHealth),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageCredentials", body = super::error::ErrorBody),
        (status = 503, description = "Credential storage is unavailable (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn credential_health(
    _perm: RequireManageCredentials,
    admin: Admin,
) -> ApiResult<Json<CredentialHealth>> {
    let report = admin.creds.decrypt_report().await.map_err(|e| {
        ApiError::from_internal(
            e.as_ref(),
            "credential health",
            "failed to check credential health",
        )
    })?;
    let total = u32::try_from(report.len()).unwrap_or(u32::MAX);
    let failures: Vec<_> = report
        .iter()
        .filter(|(_, _, _, ok)| !ok)
        .map(|(id, name, kind, _)| UndecryptableCredential {
            id: *id,
            name: name.clone(),
            kind: kind.clone(),
        })
        .collect();
    if !failures.is_empty() {
        // Loud: this is a data-loss condition in progress, not a routine 200.
        tracing::error!(
            failed = failures.len(),
            total,
            "stored credentials cannot be decrypted with the loaded KEK"
        );
    }
    Ok(Json(CredentialHealth {
        decryptable: total - u32::try_from(failures.len()).unwrap_or(0),
        total,
        failures,
    }))
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
    if kind == crate::secrets::KIND_HTTP_AUTH {
        if let Err(reason) = crate::secrets::parse_http_auth(kind, secret) {
            return Err(ApiError::bad_request(
                "invalid_credential",
                format!("invalid HTTP auth credential: {reason}"),
            ));
        }
    }
    Ok(())
}

#[utoipa::path(
    get, path = "/api/v1/credentials", tag = "credentials",
    responses(
        (status = 200, description = "Credential metadata only — the secret is never returned", body = Vec<crate::secrets::CredentialSummary>),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role does not hold ManageCredentials", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode has no write side", body = super::error::ErrorBody),
    ),
)]
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
#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct CreateCredential {
    name: String,
    kind: String,
    secret: String,
}

#[utoipa::path(
    post, path = "/api/v1/credentials", tag = "credentials",
    request_body = CreateCredential,
    responses(
        (status = 201, description = "Credential sealed and stored", body = CreatedId),
        (status = 400, description = "A missing field, or a secret that does not parse for its kind", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role does not hold ManageCredentials", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode has no write side", body = super::error::ErrorBody),
    ),
)]
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
#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct UpdateCredential {
    name: String,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    secret: Option<String>,
}

#[utoipa::path(
    put, path = "/api/v1/credentials/{id}", tag = "credentials",
    params(("id" = Uuid, Path, description = "Credential id")),
    request_body = UpdateCredential,
    responses(
        (status = 204, description = "Credential updated; an omitted secret is a rename, not a clear"),
        (status = 400, description = "An empty name, a secret without its kind, or a secret that does not parse for its kind", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role does not hold ManageCredentials", body = super::error::ErrorBody),
        (status = 404, description = "No such credential", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode has no write side", body = super::error::ErrorBody),
    ),
)]
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

#[utoipa::path(
    delete, path = "/api/v1/credentials/{id}", tag = "credentials",
    params(("id" = Uuid, Path, description = "Credential id")),
    responses(
        (status = 204, description = "Credential deleted"),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role does not hold ManageCredentials", body = super::error::ErrorBody),
        (status = 404, description = "No such credential", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode has no write side", body = super::error::ErrorBody),
    ),
)]
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
