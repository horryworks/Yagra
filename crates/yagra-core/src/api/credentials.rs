// SPDX-License-Identifier: AGPL-3.0-only
//! Stored monitoring credentials — SNMP communities, SNMPv3 USM documents, device logins.
//!
//! `ManageCredentials`, its own permission rather than `ManageConfig`: these are what the whole
//! fleet is polled with, so holding them is a strictly larger power than editing what gets polled.
//!
//! **The secret leaves this process in the clear through one door only**: `POST
//! /credentials/{id}/reveal` (ADR-210), which takes its own permission (`RevealCredentials`), is
//! refused to API tokens and to folder-scoped callers, is a POST so `audit_mw` records every call,
//! and answers only the kinds a person signs in to a device with. Everywhere else the secret is
//! write-only: sealed with envelope encryption before it reaches the database (ADR-018), the
//! listing carries metadata only, and nothing here logs or echoes a secret — including the
//! validation errors, which name a static reason and never any field content (security.md).
//!
//! An update with no `secret` is a rename: the stored secret is left intact rather than cleared.
//! With one, `kind` must accompany it, because the secret's *format* is kind-specific and re-sealing
//! a v3 document as a community string would produce a credential that silently fails every poll.

use super::error::{ApiError, ApiResult};
use super::extract::{Admin, RequireManageCredentials, RequireRevealCredentials, Scoped};
use super::util::CreatedId;
use super::ApiState;
use axum::{
    extract::Path,
    http::StatusCode,
    routing::{get, post, put},
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
    delete_credential,
    set_credential_order,
    reveal_credential
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
        .route("/api/v1/credentials/order", put(set_credential_order))
        .route(
            "/api/v1/credentials/:id",
            put(update_credential).delete(delete_credential),
        )
        .route("/api/v1/credentials/:id/reveal", post(reveal_credential))
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
    Ok(Json(credential_decrypt_health(&admin).await?))
}

/// Whether every stored credential still decrypts, shared by `GET /api/v1/credentials/health` and
/// the MCP `get_system_health(section="credentials")` tool (ADR-042 I3a).
///
/// **`ManageCredentials`, and the result carries identity only** — id, name and kind of anything
/// that failed to open, never a length, a key id, or anything derived from the ciphertext
/// (ADR-018). That is what makes this exemptible from the "secret material" rule: health and names
/// are not the secret.
pub(crate) async fn credential_decrypt_health(
    admin: &super::AdminState,
) -> Result<CredentialHealth, ApiError> {
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
    Ok(CredentialHealth {
        decryptable: total - u32::try_from(failures.len()).unwrap_or(0),
        total,
        failures,
    })
}

/// Reject a structurally invalid secret for its kind, at the edge.
///
/// Four kinds have a parseable shape: SNMPv3, HTTP auth, and — since ADR-164 Inc.11 — the two an
/// integration owns. Checking here means a malformed document is a 400 the operator can act on,
/// rather than a stored credential that fails every poll with an error only a log holds. For a
/// Meraki key that was not hypothetical: `PUT /credentials/{id}` is the only way a rotated key gets
/// in, the WebUI sent it as the bare key, and every collect and sync of the organization then
/// failed with `credential`. **The reason is the parser's static text and never any field
/// content.**
fn check_secret_shape(kind: &str, secret: &[u8]) -> Result<(), ApiError> {
    if kind == yagra_base::secrets::KIND_MERAKI_API {
        if let Err(reason) = yagra_base::secrets::MerakiApiSecret::parse(secret) {
            return Err(ApiError::bad_request(
                "invalid_credential",
                format!("invalid Meraki API credential: {reason}"),
            ));
        }
    }
    if kind == yagra_base::secrets::KIND_NETBOX_TOKEN {
        if let Err(reason) = yagra_base::secrets::NetboxTokenSecret::parse(secret) {
            return Err(ApiError::bad_request(
                "invalid_credential",
                format!("invalid NetBox token credential: {reason}"),
            ));
        }
    }
    if kind == yagra_base::secrets::KIND_SNMP_V3 {
        if let Err(reason) = yagra_base::secrets::SnmpV3Secret::parse(secret) {
            return Err(ApiError::bad_request(
                "invalid_credential",
                format!("invalid SNMPv3 credential: {reason}"),
            ));
        }
    }
    if kind == yagra_base::secrets::KIND_HTTP_AUTH {
        if let Err(reason) = yagra_base::secrets::parse_http_auth(kind, secret) {
            return Err(ApiError::bad_request(
                "invalid_credential",
                format!("invalid HTTP auth credential: {reason}"),
            ));
        }
    }
    if kind == yagra_base::secrets::KIND_CLI_LOGIN {
        if let Err(reason) = yagra_base::secrets::CliLoginSecret::parse(secret) {
            return Err(ApiError::bad_request(
                "invalid_credential",
                format!("invalid CLI login credential: {reason}"),
            ));
        }
    }
    Ok(())
}

#[utoipa::path(
    get, path = "/api/v1/credentials", tag = "credentials",
    responses(
        (status = 200, description = "Credential metadata only — the secret is never returned", body = Vec<yagra_base::secrets::CredentialSummary>),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role does not hold ManageCredentials", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode has no write side", body = super::error::ErrorBody),
    ),
)]
async fn list_credentials(
    _guard: RequireManageCredentials,
    admin: Admin,
) -> ApiResult<Json<Vec<yagra_base::secrets::CredentialSummary>>> {
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

/// Refuse a folder-scoped caller a credential **write** (ADR-158 A8, the three it missed).
///
/// `ManageCredentials` is held by Operators, and an Operator can be restricted to folders. The
/// shelf of credentials is not in any folder: one community string or one Meraki key is what the
/// devices of every site are polled with. So an account limited to Tokyo could re-seal or delete
/// the credential Osaka runs on — and for a Meraki key that makes a whole organization go quiet
/// rather than alert, because those nodes are outside the freshness sweep. The ledger called these
/// routes "admin-only" while the guard let an Operator through: the hole ADR-158 closed on eleven
/// node writes and ADR-164 on the Meraki ones.
///
/// 🚨 **Only the writes.** `GET /credentials` stays open to a scoped caller on purpose: the add-node
/// and edit-node dialogs, the collection tab, the check fields and Discovery all read it to offer
/// the picker, and it answers names and kinds, never a secret. Refusing it would stop a scoped
/// Operator from binding a credential to their own node. Letting one through for a credential that
/// only their own nodes use is a possible later loosening, and would be purely additive.
///
/// The reason string is repeated by the route ledger's `CREDENTIAL_WRITE` line.
fn credentials_are_deployment_wide(scope: &super::scope::NodeScope) -> Result<(), ApiError> {
    super::scope::require_fleet_wide(
        scope,
        "monitoring credentials are shared by every folder, so an account restricted to folders \
         cannot create, change or delete one",
    )
}

#[utoipa::path(
    post, path = "/api/v1/credentials", tag = "credentials",
    request_body = CreateCredential,
    responses(
        (status = 201, description = "Credential sealed and stored", body = CreatedId),
        (status = 400, description = "A missing field, or a secret that does not parse for its kind", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role does not hold ManageCredentials, or the account is restricted to folders (`scope_unsupported`)", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode has no write side", body = super::error::ErrorBody),
    ),
)]
async fn create_credential(
    _guard: RequireManageCredentials,
    Scoped(scope): Scoped,
    admin: Admin,
    Json(body): Json<CreateCredential>,
) -> ApiResult<(StatusCode, Json<CreatedId>)> {
    credentials_are_deployment_wide(&scope)?;
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
        (status = 403, description = "Role does not hold ManageCredentials, or the account is restricted to folders (`scope_unsupported`)", body = super::error::ErrorBody),
        (status = 404, description = "No such credential", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode has no write side", body = super::error::ErrorBody),
    ),
)]
async fn update_credential(
    _guard: RequireManageCredentials,
    Scoped(scope): Scoped,
    admin: Admin,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateCredential>,
) -> ApiResult<StatusCode> {
    credentials_are_deployment_wide(&scope)?;
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
    let new_cli_secret =
        reseal.is_some_and(|(kind, _)| kind == yagra_base::secrets::KIND_CLI_LOGIN);
    let updated = admin.creds.update(id, name, reseal).await.map_err(|e| {
        ApiError::from_internal(
            e.as_ref(),
            "update credential",
            "failed to update credential",
        )
    })?;
    if updated {
        // A CLI login with a new password may now work where it failed: every node still looking
        // for one starts its pass again (ADR-210 stage 3 decision 20). Best effort — the change
        // is saved, and a pass not restarted only waits for a person to ask.
        if new_cli_secret {
            if let Err(e) = admin.config_backup.restart_unconfirmed_rounds().await {
                tracing::warn!(error = %e, "config backup: passes were not restarted after a login changed");
            }
        }
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
        (status = 403, description = "Role does not hold ManageCredentials, or the account is restricted to folders (`scope_unsupported`)", body = super::error::ErrorBody),
        (status = 404, description = "No such credential", body = super::error::ErrorBody),
        (status = 409, description = "A Meraki organization or a NetBox server still uses it (`credential_in_use`); nothing was deleted", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode has no write side", body = super::error::ErrorBody),
    ),
)]
async fn delete_credential(
    _guard: RequireManageCredentials,
    Scoped(scope): Scoped,
    admin: Admin,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    use yagra_base::secrets::CredentialDelete;
    credentials_are_deployment_wide(&scope)?;
    match admin.creds.delete(id).await {
        Ok(CredentialDelete::Deleted) => Ok(StatusCode::NO_CONTENT),
        Ok(CredentialDelete::NotFound) => Err(ApiError::not_found(
            "credential_not_found",
            format!("no credential {id}"),
        )),
        // A node's binding is cleared by the delete; an integration's cannot be, because a Meraki
        // organization or a NetBox server with no key is not a row either table can hold. This
        // used to reach the client as a 500 that named nothing (ADR-164 Inc.6).
        Ok(CredentialDelete::InUse) => Err(ApiError::conflict(
            "credential_in_use",
            "a Meraki organization or a NetBox server still uses this credential; remove it \
             there first",
        )),
        Err(e) => Err(ApiError::from_internal(
            e.as_ref(),
            "delete credential",
            "failed to delete credential",
        )),
    }
}

/// The most ids one order may name. Credentials are typed in by an operator, so a real list is a
/// handful; the cap only bounds what one request can make the server sort and compare.
const ORDER_IDS_MAX: usize = 10_000;

/// The try order, first to last (ADR-205 Inc.2).
#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub(crate) struct CredentialOrder {
    /// Every stored credential's id, each exactly once, in the order Discovery should start from.
    ids: Vec<Uuid>,
}

/// Replace the order stored credentials are listed and tried in (ADR-205 Inc.2).
///
/// It is the order `GET /api/v1/credentials` returns, so it is the order Discovery starts a sweep
/// from and the order every credential picker lists. The body names **every** credential: one that
/// names a different set — because a credential was added or deleted after the caller read the
/// list — is refused with 409 and changes nothing, rather than placing the newcomer arbitrarily.
#[utoipa::path(
    put, path = "/api/v1/credentials/order", tag = "credentials",
    request_body = CredentialOrder,
    responses(
        (status = 204, description = "Order replaced"),
        (status = 400, description = "An id appears twice, or more than 10,000 are named (`invalid_order`)", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role does not hold ManageCredentials, or the account is restricted to folders (`scope_unsupported`)", body = super::error::ErrorBody),
        (status = 409, description = "The ids are not exactly the stored credentials — one was added or deleted since the list was read (`credential_order_stale`); nothing was changed", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode has no write side", body = super::error::ErrorBody),
    ),
)]
async fn set_credential_order(
    _guard: RequireManageCredentials,
    Scoped(scope): Scoped,
    admin: Admin,
    Json(body): Json<CredentialOrder>,
) -> ApiResult<StatusCode> {
    use yagra_base::secrets::SetOrder;
    credentials_are_deployment_wide(&scope)?;
    if body.ids.len() > ORDER_IDS_MAX {
        return Err(ApiError::bad_request(
            "invalid_order",
            format!("an order names at most {ORDER_IDS_MAX} credentials"),
        ));
    }
    let mut seen = std::collections::HashSet::with_capacity(body.ids.len());
    if !body.ids.iter().all(|id| seen.insert(*id)) {
        return Err(ApiError::bad_request(
            "invalid_order",
            "each credential may appear in the order once",
        ));
    }
    match admin.creds.set_order(&body.ids).await {
        Ok(SetOrder::Done) => Ok(StatusCode::NO_CONTENT),
        Ok(SetOrder::Mismatch) => Err(ApiError::conflict(
            "credential_order_stale",
            "the order does not name exactly the stored credentials; reload the list and try again",
        )),
        Err(e) => Err(ApiError::from_internal(
            e.as_ref(),
            "set credential order",
            "failed to save the credential order",
        )),
    }
}

/// One part of a revealed secret. Which parts a credential has depends on its kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RevealedFieldKey {
    /// An SNMP v1/v2c community string.
    Community,
    /// The SNMPv3 USM user name.
    User,
    /// The SNMPv3 security level: `noauth`, `auth` or `authpriv`.
    SecurityLevel,
    /// The SNMPv3 authentication protocol.
    AuthProtocol,
    /// The SNMPv3 authentication passphrase.
    AuthKey,
    /// The SNMPv3 privacy protocol.
    PrivProtocol,
    /// The SNMPv3 privacy passphrase.
    PrivKey,
    /// The HTTP authentication scheme: `basic`, `bearer` or `header`.
    Scheme,
    /// The HTTP Basic user name.
    Username,
    /// The HTTP Basic password.
    Password,
    /// A bearer token.
    Token,
    /// The name of a custom HTTP header.
    HeaderName,
    /// The value of a custom HTTP header.
    HeaderValue,
    /// A device's privileged-mode ("enable") password, sent after a CLI login.
    Enable,
}

/// One part of a revealed secret and its value.
#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
pub(crate) struct RevealedField {
    field: RevealedFieldKey,
    value: String,
    /// Whether the value is itself secret (a passphrase, a password, a token) rather than a
    /// setting beside it (a protocol, a user name).
    secret: bool,
}

/// A credential's secret, in plaintext.
#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
pub(crate) struct RevealedCredential {
    id: Uuid,
    kind: String,
    /// The parts of the secret, in the order a person types them into a device.
    fields: Vec<RevealedField>,
}

/// The parts of a stored secret, or `None` for a kind that is not revealed.
///
/// Only the kinds a person signs in to a device with (ADR-210 decision 5). An integration key —
/// Meraki, NetBox — is Yagra's own key to someone else's API rather than a device login, and a
/// kind this core does not know might be anything, so neither is opened for a person. `Err`
/// carries a static reason only, never field content.
fn revealed_fields(kind: &str, secret: &[u8]) -> Option<Result<Vec<RevealedField>, &'static str>> {
    use yagra_base::secrets::{KIND_API_TOKEN, KIND_CLI_LOGIN, KIND_HTTP_AUTH, KIND_SNMP_V3};
    use yagra_common::url_check::HttpAuth;
    use RevealedFieldKey as K;
    let field = |field, value: &str, secret| RevealedField {
        field,
        value: value.to_owned(),
        secret,
    };
    let fields = match kind {
        "snmp_v2c" => std::str::from_utf8(secret)
            .map(|c| vec![field(K::Community, c, true)])
            .map_err(|_| "the community is not valid UTF-8"),
        KIND_SNMP_V3 => yagra_base::secrets::SnmpV3Secret::parse(secret).map(|v3| {
            let mut out = vec![
                field(K::User, &v3.user, false),
                field(K::SecurityLevel, &v3.security_level, false),
            ];
            let optional = [
                (K::AuthProtocol, &v3.auth_protocol, false),
                (K::AuthKey, &v3.auth_key, true),
                (K::PrivProtocol, &v3.priv_protocol, false),
                (K::PrivKey, &v3.priv_key, true),
            ];
            for (key, value, is_secret) in optional {
                if let Some(value) = value.as_deref().filter(|v| !v.is_empty()) {
                    out.push(field(key, value, is_secret));
                }
            }
            out
        }),
        KIND_HTTP_AUTH | KIND_API_TOKEN => {
            yagra_base::secrets::parse_http_auth(kind, secret).map(|auth| {
                let scheme = field(K::Scheme, auth.scheme(), false);
                match &auth {
                    HttpAuth::Basic { username, password } => vec![
                        scheme,
                        field(K::Username, username, false),
                        field(K::Password, password, true),
                    ],
                    HttpAuth::Bearer { token } => vec![scheme, field(K::Token, token, true)],
                    HttpAuth::Header { name, value } => vec![
                        scheme,
                        field(K::HeaderName, name, false),
                        field(K::HeaderValue, value, true),
                    ],
                }
            })
        }
        KIND_CLI_LOGIN => yagra_base::secrets::CliLoginSecret::parse(secret).map(|cli| {
            let mut out = vec![
                field(K::Username, &cli.username, false),
                field(K::Password, &cli.password, true),
            ];
            if let Some(enable) = cli.enable.as_deref() {
                out.push(field(K::Enable, enable, true));
            }
            out
        }),
        _ => return None,
    };
    Some(fields)
}

/// Show a stored credential's secret, for a person signing in to the device (ADR-210).
///
/// **A POST although it changes nothing**, so `audit_mw` writes a row naming who read which
/// credential — the same reason the relocation download is one. The answer is never cached
/// (`Cache-Control: no-store`). Refused to an API token (`token_not_permitted`) and to an account
/// restricted to folders (`scope_unsupported`): the shelf belongs to no folder, so a scoped caller
/// could otherwise read the login of a site it cannot see. An integration key and a kind this
/// server does not know are refused (`not_revealable`).
#[utoipa::path(
    post, path = "/api/v1/credentials/{id}/reveal", tag = "credentials",
    params(("id" = Uuid, Path, description = "Credential id")),
    responses(
        (status = 200, description = "The secret in plaintext. Every call is recorded in the audit log", body = RevealedCredential),
        (status = 400, description = "An integration key or an unknown kind, which is not revealed (`not_revealable`)", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role does not hold RevealCredentials, the caller is an API token (`token_not_permitted`), or the account is restricted to folders (`scope_unsupported`)", body = super::error::ErrorBody),
        (status = 404, description = "No such credential", body = super::error::ErrorBody),
        (status = 500, description = "The stored secret cannot be decrypted or does not parse for its kind", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode has no credential store", body = super::error::ErrorBody),
    ),
)]
async fn reveal_credential(
    _guard: RequireRevealCredentials,
    Scoped(scope): Scoped,
    admin: Admin,
    Path(id): Path<Uuid>,
) -> ApiResult<(
    [(axum::http::header::HeaderName, &'static str); 1],
    Json<RevealedCredential>,
)> {
    super::scope::require_fleet_wide(
        &scope,
        "monitoring credentials are shared by every folder, so an account restricted to folders \
         cannot read one back",
    )?;
    let (kind, secret) = admin
        .creds
        .open(id)
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "reveal credential",
                "failed to decrypt credential",
            )
        })?
        .ok_or_else(|| {
            ApiError::not_found("credential_not_found", format!("no credential {id}"))
        })?;
    let fields = match revealed_fields(&kind, &secret) {
        None => {
            return Err(ApiError::bad_request(
                "not_revealable",
                "only device credentials (SNMP, HTTP and CLI) can be shown; an integration key \
                 cannot",
            ))
        }
        Some(Err(reason)) => {
            // The reason is static text; the stored bytes never reach the log.
            tracing::error!(credential = %id, %kind, reason, "a stored credential does not parse");
            return Err(ApiError::internal(
                "the stored secret does not parse for its kind",
            ));
        }
        Some(Ok(fields)) => fields,
    };
    Ok((
        [(axum::http::header::CACHE_CONTROL, "no-store")],
        Json(RevealedCredential { id, kind, fields }),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::tests_support::{private_state, public_state, status_of};
    use yagra_common::{Principal, Role, Scope};

    const ID: &str = "00000000-0000-0000-0000-000000000001";

    fn all_routes() -> Vec<(&'static str, String)> {
        vec![
            ("GET", "/api/v1/credentials".to_owned()),
            ("POST", "/api/v1/credentials".to_owned()),
            ("PUT", format!("/api/v1/credentials/{ID}")),
            ("DELETE", format!("/api/v1/credentials/{ID}")),
            ("POST", format!("/api/v1/credentials/{ID}/reveal")),
        ]
    }

    #[tokio::test]
    async fn the_credential_store_is_closed_to_a_viewer_and_open_to_an_operator() {
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
        // ADR-057 moved `ManageCredentials` down to Operator: these are the keys Yagra reaches
        // *monitored devices* with, not the ones people sign in with, so they belong to whoever
        // runs the monitoring. A viewer is still refused every one of them, listing included.
        let st = private_state();
        for (role, want) in [
            (Role::Viewer, StatusCode::FORBIDDEN),
            (Role::Operator, StatusCode::SERVICE_UNAVAILABLE),
        ] {
            let token = st
                .sessions
                .issue(Uuid::new_v4(), Principal::new(role, Scope::All), "u");
            for (method, path) in all_routes() {
                assert_eq!(
                    status_of(st.clone(), method, &path, Some(&token)).await,
                    want,
                    "{role:?} {method} {path}"
                );
            }
        }
    }

    #[test]
    fn a_malformed_v3_document_is_rejected_before_it_is_sealed() {
        // Otherwise it is stored successfully and then fails every poll, with the reason visible
        // only in the poller's log.
        let err = check_secret_shape(yagra_base::secrets::KIND_SNMP_V3, b"not a usm document")
            .unwrap_err();
        assert_eq!(err.code(), "invalid_credential");
        // The reason is the parser's static text. Nothing from the submitted secret may appear.
        assert!(!err.message().contains("not a usm document"));

        // A community string has no parseable shape, so anything non-empty is accepted here.
        assert!(check_secret_shape("snmp_v2c", b"public").is_ok());
    }

    /// ADR-164 Inc.11. An integration's key is a JSON document, and the bare key is what an
    /// operator pastes — stored as typed, it made every collect and sync of the organization fail
    /// with `credential`, with nothing on the dialog that had taken it.
    #[test]
    fn an_integration_key_that_is_not_its_document_is_rejected_before_it_is_sealed() {
        use yagra_base::secrets::{KIND_MERAKI_API, KIND_NETBOX_TOKEN};
        for (kind, bare, document) in [
            (
                KIND_MERAKI_API,
                "0123deadbeef",
                r#"{"api_key":"0123deadbeef"}"#,
            ),
            (KIND_NETBOX_TOKEN, "nbt-secret", r#"{"token":"nbt-secret"}"#),
        ] {
            let err = check_secret_shape(kind, bare.as_bytes()).unwrap_err();
            assert_eq!(err.code(), "invalid_credential", "{kind}");
            assert!(
                !err.message().contains(bare),
                "{kind}: the refusal quoted the key"
            );
            assert!(
                check_secret_shape(kind, document.as_bytes()).is_ok(),
                "{kind}"
            );
        }
        // Each kind's document is its own: the other's is refused.
        assert!(check_secret_shape(KIND_MERAKI_API, br#"{"token":"x"}"#).is_err());
        assert!(check_secret_shape(KIND_NETBOX_TOKEN, br#"{"api_key":"x"}"#).is_err());
    }

    /// The edit dialog builds those two documents in the browser (`integrationSecret`), so their
    /// field names exist twice. A drifted copy is a 400 rather than a broken organization — the
    /// check above sees to that — but a 400 on every key rotation is still a dialog that does not
    /// work, and nothing in the WebUI's own tests knows what this side parses.
    ///
    /// ⚠️ It finds each document as a `JSON.stringify({ <field>: value })` line.
    #[test]
    fn the_webui_seals_an_integration_key_in_the_shape_this_api_parses() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../web/src/lib/credentialKinds.ts"
        );
        let text = std::fs::read_to_string(path).expect("the WebUI's credential kinds");
        let fields: Vec<&str> = text
            .split("JSON.stringify({ ")
            .skip(1)
            .filter_map(|rest| rest.split_once(": value })").map(|(field, _)| field))
            .collect();
        assert_eq!(
            fields,
            ["api_key", "token"],
            "the documents {path} builds are not the ones this API parses — or its shape changed \
             and this reader stopped finding them"
        );
        for (kind, field) in [
            (yagra_base::secrets::KIND_MERAKI_API, fields[0]),
            (yagra_base::secrets::KIND_NETBOX_TOKEN, fields[1]),
        ] {
            let document = serde_json::json!({ field: "k" }).to_string();
            assert!(
                check_secret_shape(kind, document.as_bytes()).is_ok(),
                "{kind} refuses {document}"
            );
        }
    }

    /// The rotation itself, through the router: the document is accepted and resealed, and the
    /// bare key is refused with the stored secret left as it was.
    #[sqlx::test(migrator = "yagra_base::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_meraki_key_is_replaced_by_its_document_and_never_by_the_bare_key(
        pool: sqlx::PgPool,
    ) {
        use crate::api::tests_support::{live_state, send, token};
        let st = live_state(pool.clone()).await;
        let tok = token(&st, yagra_common::Role::Admin);
        let admin = st.admin.as_ref().expect("live state has an admin side");
        let id = admin
            .creds
            .create(
                "Meraki API — Acme",
                yagra_base::secrets::KIND_MERAKI_API,
                br#"{"api_key":"old-key"}"#,
            )
            .await
            .expect("seal the first key");
        let path = format!("/api/v1/credentials/{id}");
        let stored_key = || async {
            let (kind, secret) = admin
                .creds
                .open(id)
                .await
                .expect("open")
                .expect("the credential");
            (
                kind,
                yagra_base::secrets::MerakiApiSecret::parse(&secret).map(|s| s.api_key),
            )
        };

        let (status, body) = send(
            &st,
            "PUT",
            &path,
            &tok,
            Some(serde_json::json!({
                "name": "Meraki API — Acme",
                "kind": "meraki_api",
                "secret": "new-key",
            })),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(body["error"]["code"], "invalid_credential");
        assert_eq!(
            stored_key().await,
            ("meraki_api".to_owned(), Ok("old-key".to_owned())),
            "a refused replacement changed the stored key"
        );

        let (status, body) = send(
            &st,
            "PUT",
            &path,
            &tok,
            Some(serde_json::json!({
                "name": "Meraki API — Acme",
                "kind": "meraki_api",
                "secret": r#"{"api_key":"new-key"}"#,
            })),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::NO_CONTENT, "{body}");
        assert_eq!(
            stored_key().await,
            ("meraki_api".to_owned(), Ok("new-key".to_owned()))
        );
    }
    // ── An accepted write (ADR-115) ──────────────────────────────────────────────────

    /// A credential is stored sealed, and the list never hands the secret back (ADR-018).
    #[sqlx::test(migrator = "yagra_base::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn creating_a_credential_seals_it_and_the_list_never_returns_it(pool: sqlx::PgPool) {
        use crate::api::tests_support::{live_state, send, token};
        let st = live_state(pool.clone()).await;
        let tok = token(&st, yagra_common::Role::Admin);
        let (status, body) = send(
            &st,
            "POST",
            "/api/v1/credentials",
            &tok,
            Some(serde_json::json!({
                "name": "lab community",
                "kind": "snmp_v2c",
                "secret": "s3cr3t-community",
            })),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::CREATED, "{body}");
        assert_eq!(yagra_base::pgtest::rows(&pool, "credentials").await, 1);

        let (status, list) = send(&st, "GET", "/api/v1/credentials", &tok, None).await;
        assert_eq!(status, axum::http::StatusCode::OK, "{list}");
        assert!(
            !list.to_string().contains("s3cr3t-community"),
            "the list returned the secret"
        );
    }

    /// **An account restricted to folders may read the shelf and change nothing on it** (ADR-158 A8,
    /// the three writes it missed).
    ///
    /// The credentials belong to no folder — one community string is what every site is polled
    /// with — so an Operator limited to one site could re-seal or delete what another site runs
    /// on. The list stays open on purpose: the add-node and edit-node dialogs fill their picker
    /// from it, and it answers names, never a secret.
    #[sqlx::test(migrator = "yagra_base::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_folder_scoped_caller_reads_the_credentials_and_changes_none(pool: sqlx::PgPool) {
        use crate::api::tests_support::{live_state, scoped_token, send, token};
        let st = live_state(pool.clone()).await;
        let admin = st.admin.clone().expect("live state");
        let existing = yagra_base::pgtest::credential(&pool, "osaka core v3", "snmp_v2c").await;
        let folder = yagra_base::pgtest::group(&pool, "Tokyo").await;
        let scoped = scoped_token(&st, &[folder]);
        // Assembled rather than written out: `scope.rs::no_handler_spells_the_scope_refusal_by_hand`
        // counts the quoted code across this directory, tests included.
        let refusal = format!("{}_unsupported", "scope");

        let writes = [
            (
                "POST",
                "/api/v1/credentials".to_owned(),
                Some(serde_json::json!({
                    "name": "tokyo community", "kind": "snmp_v2c", "secret": "s3cr3t",
                })),
            ),
            (
                "PUT",
                format!("/api/v1/credentials/{existing}"),
                Some(serde_json::json!({
                    "name": "renamed", "kind": "snmp_v2c", "secret": "replaced",
                })),
            ),
            ("DELETE", format!("/api/v1/credentials/{existing}"), None),
        ];
        for (method, path, body) in writes.clone() {
            let (status, answer) = send(&st, method, &path, &scoped, body).await;
            assert_eq!(
                status,
                axum::http::StatusCode::FORBIDDEN,
                "{method} {path}: {answer}"
            );
            assert_eq!(
                answer["error"]["code"].as_str(),
                Some(refusal.as_str()),
                "{method} {path}"
            );
        }
        // Refused means refused: nothing was added, renamed or removed.
        let stored = admin.creds.list().await.expect("list");
        assert_eq!(stored.len(), 1, "a refused write changed the shelf");
        assert_eq!(stored[0].name, "osaka core v3");

        // The read the pickers depend on still answers — with the name, which is all it holds.
        let (status, list) = send(&st, "GET", "/api/v1/credentials", &scoped, None).await;
        assert_eq!(status, axum::http::StatusCode::OK, "{list}");
        assert_eq!(list[0]["name"], "osaka core v3", "{list}");

        // And it is the *scope* that was refused, not the role: the same three from an Operator
        // with no folder restriction are accepted.
        let unscoped = token(&st, yagra_common::Role::Operator);
        let expected = [
            axum::http::StatusCode::CREATED,
            axum::http::StatusCode::NO_CONTENT,
            axum::http::StatusCode::NO_CONTENT,
        ];
        for ((method, path, body), want) in writes.into_iter().zip(expected) {
            let (status, answer) = send(&st, method, &path, &unscoped, body).await;
            assert_eq!(status, want, "{method} {path}: {answer}");
        }
    }

    /// A credential a Meraki organization is polled with says so in the list, and deleting it is
    /// refused with a reason — it used to read "unused" and answer 500 (ADR-164 Inc.6). Once the
    /// organization is gone the same delete is accepted.
    #[sqlx::test(migrator = "yagra_base::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_credential_an_integration_uses_is_counted_and_cannot_be_deleted(pool: sqlx::PgPool) {
        use crate::api::tests_support::{live_state, send, token};
        let st = live_state(pool.clone()).await;
        let admin = st.admin.clone().expect("live state");
        let tok = token(&st, yagra_common::Role::Operator);

        let key = yagra_base::pgtest::credential(&pool, "meraki key", "meraki_api").await;
        let mut orgs = Vec::new();
        for (org_id, name) in [("100", "Acme"), ("200", "Globex")] {
            orgs.push(
                admin
                    .meraki_orgs
                    .create(org_id, name, "https://api.meraki.com", key)
                    .await
                    .expect("org"),
            );
        }
        // Three nodes bound to the same credential as well. Nothing would do this to a Meraki key;
        // it is here because the three counts come from three tables, and joined under one GROUP BY
        // they would multiply (3 × 2 = 6 of each).
        for n in 1..=3u8 {
            let node = yagra_base::pgtest::node(&pool, &format!("n{n}"), n, None).await;
            sqlx::query("UPDATE nodes SET credential_id = $1 WHERE id = $2")
                .bind(key)
                .bind(node)
                .execute(&pool)
                .await
                .expect("bind");
        }
        // The NetBox fixture seals a token of its own.
        yagra_base::pgtest::netbox_server(&pool, "lab").await;

        let (status, list) = send(&st, "GET", "/api/v1/credentials", &tok, None).await;
        assert_eq!(status, StatusCode::OK, "{list}");
        let row = |name: &str| -> serde_json::Value {
            list.as_array()
                .expect("an array")
                .iter()
                .find(|c| c["name"] == name)
                .unwrap_or_else(|| panic!("{name} is listed: {list}"))
                .clone()
        };
        let meraki = row("meraki key");
        assert_eq!(
            (
                meraki["used_by"].as_i64(),
                meraki["used_by_meraki_orgs"].as_i64(),
                meraki["used_by_netbox_servers"].as_i64(),
            ),
            (Some(3), Some(2), Some(0)),
            "{meraki}"
        );
        let netbox = row("lab-token");
        assert_eq!(netbox["used_by_netbox_servers"], 1, "{netbox}");
        assert_eq!(netbox["used_by_meraki_orgs"], 0, "{netbox}");

        let path = format!("/api/v1/credentials/{key}");
        let (status, body) = send(&st, "DELETE", &path, &tok, None).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body["error"]["code"], "credential_in_use", "{body}");
        assert_eq!(
            yagra_base::pgtest::rows(&pool, "credentials").await,
            2,
            "a refused delete removed nothing"
        );

        for org in orgs {
            assert!(admin.meraki_orgs.purge(org).await.expect("purge"));
        }
        let (status, body) = send(&st, "DELETE", &path, &tok, None).await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
        assert_eq!(yagra_base::pgtest::rows(&pool, "credentials").await, 1);

        // And one that was never there is still a 404, not a conflict.
        let (status, body) = send(&st, "DELETE", &path, &tok, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    }

    // ── The try order (ADR-205 Inc.2) ─────────────────────────────────────────

    /// The names `GET /api/v1/credentials` lists, in the order it lists them.
    async fn listed_names(st: &crate::api::ApiState, tok: &str) -> Vec<String> {
        use crate::api::tests_support::send;
        let (status, list) = send(st, "GET", "/api/v1/credentials", tok, None).await;
        assert_eq!(status, axum::http::StatusCode::OK, "{list}");
        list.as_array()
            .expect("an array")
            .iter()
            .map(|c| c["name"].as_str().expect("a name").to_owned())
            .collect()
    }

    async fn create(st: &crate::api::ApiState, tok: &str, name: &str) -> Uuid {
        use crate::api::tests_support::send;
        let (status, body) = send(
            st,
            "POST",
            "/api/v1/credentials",
            tok,
            Some(serde_json::json!({ "name": name, "kind": "snmp_v2c", "secret": "c" })),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::CREATED, "{body}");
        body["id"].as_str().expect("an id").parse().expect("a uuid")
    }

    /// A new credential goes to the end, the order can be replaced, and the list follows it — the
    /// list being what Discovery starts a sweep from.
    #[sqlx::test(migrator = "yagra_base::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn the_try_order_is_replaced_and_the_list_follows_it(pool: sqlx::PgPool) {
        use crate::api::tests_support::{live_state, send, token};
        let st = live_state(pool.clone()).await;
        let tok = token(&st, yagra_common::Role::Operator);
        let a = create(&st, &tok, "a").await;
        let b = create(&st, &tok, "b").await;
        let c = create(&st, &tok, "c").await;
        assert_eq!(
            listed_names(&st, &tok).await,
            ["a", "b", "c"],
            "created last, listed last"
        );

        let (status, body) = send(
            &st,
            "PUT",
            "/api/v1/credentials/order",
            &tok,
            Some(serde_json::json!({ "ids": [c, a, b] })),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::NO_CONTENT, "{body}");
        assert_eq!(listed_names(&st, &tok).await, ["c", "a", "b"]);

        // And one created afterwards still goes to the end of the order as it now stands.
        create(&st, &tok, "d").await;
        assert_eq!(listed_names(&st, &tok).await, ["c", "a", "b", "d"]);
    }

    /// An order that does not name exactly the stored credentials changes nothing: one missing,
    /// one unknown (409 — the caller's list is out of date) or one twice (400 — malformed).
    #[sqlx::test(migrator = "yagra_base::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn an_order_naming_a_different_set_changes_nothing(pool: sqlx::PgPool) {
        use crate::api::tests_support::{live_state, send, token};
        use axum::http::StatusCode;
        let st = live_state(pool.clone()).await;
        let tok = token(&st, yagra_common::Role::Operator);
        let a = create(&st, &tok, "a").await;
        let b = create(&st, &tok, "b").await;
        let stranger = Uuid::new_v4();
        let cases = [
            (
                serde_json::json!({ "ids": [b] }),
                StatusCode::CONFLICT,
                "credential_order_stale",
            ),
            (
                serde_json::json!({ "ids": [b, a, stranger] }),
                StatusCode::CONFLICT,
                "credential_order_stale",
            ),
            (
                serde_json::json!({ "ids": [b, b, a] }),
                StatusCode::BAD_REQUEST,
                "invalid_order",
            ),
        ];
        for (body, want, code) in cases {
            let (status, answer) = send(
                &st,
                "PUT",
                "/api/v1/credentials/order",
                &tok,
                Some(body.clone()),
            )
            .await;
            assert_eq!(status, want, "{body}: {answer}");
            assert_eq!(answer["error"]["code"].as_str(), Some(code), "{body}");
            assert_eq!(
                listed_names(&st, &tok).await,
                ["a", "b"],
                "{body} moved something"
            );
        }
    }

    /// The order is a credential write: a Viewer may not set it, and neither may an account
    /// restricted to folders — the order is shared by every site.
    #[sqlx::test(migrator = "yagra_base::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_viewer_or_a_scoped_caller_cannot_reorder(pool: sqlx::PgPool) {
        use crate::api::tests_support::{live_state, scoped_token, send, token};
        use axum::http::StatusCode;
        let st = live_state(pool.clone()).await;
        let operator = token(&st, yagra_common::Role::Operator);
        let a = create(&st, &operator, "a").await;
        let b = create(&st, &operator, "b").await;
        let folder = yagra_base::pgtest::group(&pool, "Tokyo").await;
        let refused = [
            token(&st, yagra_common::Role::Viewer),
            scoped_token(&st, &[folder]),
        ];
        for tok in &refused {
            let (status, answer) = send(
                &st,
                "PUT",
                "/api/v1/credentials/order",
                tok,
                Some(serde_json::json!({ "ids": [b, a] })),
            )
            .await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{answer}");
        }
        assert_eq!(listed_names(&st, &operator).await, ["a", "b"]);
    }

    // ── Reading a credential back (ADR-210) ─────────────────────────────────────────

    /// The `(field, value, secret)` triples `revealed_fields` answers, for compact assertions.
    fn triples(kind: &str, secret: &[u8]) -> Vec<(RevealedFieldKey, String, bool)> {
        revealed_fields(kind, secret)
            .expect("a revealed kind")
            .expect("a well-formed secret")
            .into_iter()
            .map(|f| (f.field, f.value, f.secret))
            .collect()
    }

    #[test]
    fn every_device_kind_is_revealed_as_the_parts_a_person_types() {
        use RevealedFieldKey as K;
        assert_eq!(
            triples("snmp_v2c", b"s3cr3t"),
            [(K::Community, "s3cr3t".to_owned(), true)]
        );
        assert_eq!(
            triples(
                "snmp_v3",
                br#"{"user":"ops","security_level":"authpriv","auth_protocol":"sha","auth_key":"a-pass","priv_protocol":"aes","priv_key":"p-pass"}"#
            ),
            [
                (K::User, "ops".to_owned(), false),
                (K::SecurityLevel, "authpriv".to_owned(), false),
                (K::AuthProtocol, "sha".to_owned(), false),
                (K::AuthKey, "a-pass".to_owned(), true),
                (K::PrivProtocol, "aes".to_owned(), false),
                (K::PrivKey, "p-pass".to_owned(), true),
            ]
        );
        // A noauth user has no keys to show, and shows none rather than empty rows.
        assert_eq!(
            triples("snmp_v3", br#"{"user":"ro","security_level":"noauth"}"#),
            [
                (K::User, "ro".to_owned(), false),
                (K::SecurityLevel, "noauth".to_owned(), false),
            ]
        );
        assert_eq!(
            triples(
                "http_auth",
                br#"{"scheme":"basic","username":"admin","password":"pw"}"#
            ),
            [
                (K::Scheme, "basic".to_owned(), false),
                (K::Username, "admin".to_owned(), false),
                (K::Password, "pw".to_owned(), true),
            ]
        );
        assert_eq!(
            triples(
                "http_auth",
                br#"{"scheme":"header","name":"X-API-Key","value":"k"}"#
            ),
            [
                (K::Scheme, "header".to_owned(), false),
                (K::HeaderName, "X-API-Key".to_owned(), false),
                (K::HeaderValue, "k".to_owned(), true),
            ]
        );
        // The legacy kind is a bare bearer token.
        assert_eq!(
            triples("api_token", b"tok"),
            [
                (K::Scheme, "bearer".to_owned(), false),
                (K::Token, "tok".to_owned(), true),
            ]
        );
    }

    #[test]
    fn an_integration_key_or_an_unknown_kind_is_not_revealed() {
        for kind in [
            yagra_base::secrets::KIND_MERAKI_API,
            yagra_base::secrets::KIND_NETBOX_TOKEN,
            "ssh_login_from_a_newer_core",
        ] {
            assert!(
                revealed_fields(kind, br#"{"api_key":"k","token":"t"}"#).is_none(),
                "{kind} must not be revealed"
            );
        }
        // A device kind whose stored bytes do not parse is an error, never a guess.
        assert!(matches!(
            revealed_fields("snmp_v3", b"not json"),
            Some(Err(_))
        ));
    }

    /// The WebUI offers "Show secret" for `CREDENTIAL_KINDS` (`lib/credentialKinds.ts`,
    /// `isRevealableCredentialKind`), and this server reveals the kinds `revealed_fields` answers.
    /// Two lists in two languages: a kind the screen offers and the server refuses is a menu item
    /// that only ever errors, and one the server reveals with no item is a door nobody sees.
    #[test]
    fn the_kinds_the_webui_offers_to_reveal_are_the_kinds_revealed_here() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../web/src/lib/credentialKinds.ts");
        let src = std::fs::read_to_string(&path).expect("read credentialKinds.ts");
        let line = src
            .lines()
            .find(|l| l.starts_with("export const CREDENTIAL_KINDS = ["))
            .expect("CREDENTIAL_KINDS is declared on one line");
        let offered: Vec<&str> = line.split('\'').skip(1).step_by(2).collect();
        assert!(offered.len() >= 4, "parsed too few kinds: {offered:?}");
        // A well-formed secret for each, so the answer is about the kind rather than the bytes.
        let sample = |kind: &str| -> &'static [u8] {
            match kind {
                "snmp_v3" => br#"{"user":"u","security_level":"noauth"}"#,
                "http_auth" => br#"{"scheme":"bearer","token":"t"}"#,
                "cli_login" => br#"{"username":"u","password":"p"}"#,
                _ => b"x",
            }
        };
        for kind in &offered {
            assert!(
                matches!(revealed_fields(kind, sample(kind)), Some(Ok(_))),
                "the WebUI offers to reveal {kind}, which this server refuses"
            );
        }
        for kind in [
            yagra_base::secrets::KIND_MERAKI_API,
            yagra_base::secrets::KIND_NETBOX_TOKEN,
        ] {
            assert!(!offered.contains(&kind), "{kind} must not be offered");
        }
    }

    /// An Operator reads a credential back, and the read leaves an audit row naming it (ADR-210).
    #[sqlx::test(migrator = "yagra_base::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn an_operator_reveals_a_credential_and_the_audit_log_records_it(pool: sqlx::PgPool) {
        use crate::api::tests_support::{live_state, send, token};
        let st = live_state(pool.clone()).await;
        let tok = token(&st, yagra_common::Role::Operator);
        let (status, body) = send(
            &st,
            "POST",
            "/api/v1/credentials",
            &tok,
            Some(serde_json::json!({
                "name": "lab community", "kind": "snmp_v2c", "secret": "s3cr3t-community",
            })),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::CREATED, "{body}");
        let id = body["id"].as_str().expect("an id").to_owned();
        let audit_before = yagra_base::pgtest::rows(&pool, "audit_log").await;

        let path = format!("/api/v1/credentials/{id}/reveal");
        let (status, body) = send(&st, "POST", &path, &tok, None).await;
        assert_eq!(status, axum::http::StatusCode::OK, "{body}");
        assert_eq!(body["kind"], "snmp_v2c");
        assert_eq!(
            body["fields"],
            serde_json::json!([{ "field": "community", "value": "s3cr3t-community", "secret": true }])
        );

        assert_eq!(
            yagra_base::pgtest::rows(&pool, "audit_log").await,
            audit_before + 1,
            "the read left no audit row"
        );
        let action: String =
            sqlx::query_scalar("SELECT action FROM audit_log ORDER BY at DESC, id DESC LIMIT 1")
                .fetch_one(&pool)
                .await
                .expect("the audit row");
        assert_eq!(action, format!("POST {path}"));
    }

    /// Who may not read a credential back: a Viewer, an account restricted to folders, and anyone
    /// asking for an integration key or a credential that does not exist.
    #[sqlx::test(migrator = "yagra_base::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_reveal_is_refused_to_a_viewer_a_scoped_caller_and_an_integration_key(
        pool: sqlx::PgPool,
    ) {
        use crate::api::tests_support::{live_state, scoped_token, send, token};
        use axum::http::StatusCode;
        let st = live_state(pool.clone()).await;
        let community = yagra_base::pgtest::credential(&pool, "lab", "snmp_v2c").await;
        let key = yagra_base::pgtest::credential(&pool, "meraki key", "meraki_api").await;
        let folder = yagra_base::pgtest::group(&pool, "Tokyo").await;
        let reveal = |id: Uuid| format!("/api/v1/credentials/{id}/reveal");
        // Assembled rather than written out, for the reason given in the scoped-write test above.
        let refusal = format!("{}_unsupported", "scope");

        let viewer = token(&st, yagra_common::Role::Viewer);
        let (status, answer) = send(&st, "POST", &reveal(community), &viewer, None).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{answer}");

        let scoped = scoped_token(&st, &[folder]);
        let (status, answer) = send(&st, "POST", &reveal(community), &scoped, None).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{answer}");
        assert_eq!(answer["error"]["code"].as_str(), Some(refusal.as_str()));

        let operator = token(&st, yagra_common::Role::Operator);
        let (status, answer) = send(&st, "POST", &reveal(key), &operator, None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{answer}");
        assert_eq!(answer["error"]["code"], "not_revealable");
        assert!(
            !answer.to_string().contains("a-test-secret"),
            "a refusal carried the secret"
        );

        let (status, answer) = send(&st, "POST", &reveal(Uuid::new_v4()), &operator, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{answer}");
        assert_eq!(answer["error"]["code"], "credential_not_found");
    }
}
