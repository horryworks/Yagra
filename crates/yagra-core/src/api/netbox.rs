// SPDX-License-Identifier: AGPL-3.0-only
//! NetBox — read-only site-hierarchy import (ADR-100 Inc.1).
//!
//! Six routes: list / create / update / delete a configured server, test a connection before it is
//! saved, and run a sync now. Reads are `View`, writes are `ManageConfig` — the same split as
//! `api/meraki.rs`, because this is the same kind of thing: an external system Yagra reads to
//! decide what it monitors.
//!
//! # Three guards protect the operator, and each one has a different failure it prevents
//!
//! - **[`crate::netbox::validate_base_url`]** — the base URL is operator-entered, which no other
//!   integration's is (Meraki pins an allow-list of vendor hosts; ADR-034 made BigQuery's a
//!   constant). Without it, anyone who can reach this endpoint could point core at a server they
//!   control and have it deliver the API token there. The rule lives in `netbox.rs` and is called
//!   from here rather than restated, because a second copy of a URL check is exactly the shape
//!   this workspace has already shipped a hole in (`extensibility.md` §3).
//! - **[`crate::netbox::validate_ca_pem`]** — a pasted CA lands in a plaintext, API-readable
//!   column, so a private key pasted into that box would be published. Refused, never stripped.
//! - **[`upstream_error`]** — a NetBox error body can quote the request, and the request carries
//!   the `Authorization: Token …` header. So upstream detail is logged and never returned.
//!
//! # What is not here
//!
//! No write ever reaches NetBox. There is no handler that could: the only client is
//! `NetboxClient`, whose every method is a `GET`.

use super::error::{ApiError, ApiResult};
use super::extract::{Admin, RequireManageConfig, RequireView};
use super::ApiState;
use crate::netbox::{self, NetboxClient, NetboxRepo, NetboxServer};
use crate::secrets::KIND_NETBOX_TOKEN;
use axum::{
    extract::Path,
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Bounds on the operator-set sync cadence. The floor is not the same as
/// `netbox::MIN_SYNC_INTERVAL`'s: that one is a safety net inside the loop, this one is the form's
/// contract, and a value refused here can still arrive from a config import.
const MIN_INTERVAL_SECS: i32 = 60;
const MAX_INTERVAL_SECS: i32 = 86_400;

/// This domain's slice of the OpenAPI document (ADR-035), merged by [`super::openapi::document`].
#[derive(utoipa::OpenApi)]
#[openapi(paths(
    list_netbox_servers,
    create_netbox_server,
    update_netbox_server,
    delete_netbox_server,
    test_netbox_connection,
    sync_netbox_server
))]
pub(super) struct Doc;

/// The NetBox routes, merged into `/api/v1` by [`super::router`].
pub(super) fn routes() -> Router<ApiState> {
    Router::new()
        .route(
            "/api/v1/netbox/servers",
            get(list_netbox_servers).post(create_netbox_server),
        )
        .route(
            "/api/v1/netbox/servers/:id",
            axum::routing::put(update_netbox_server).delete(delete_netbox_server),
        )
        .route("/api/v1/netbox/servers/:id/sync", post(sync_netbox_server))
        .route("/api/v1/netbox/test", post(test_netbox_connection))
}

/// A NetBox call failed. **The upstream detail is logged, never returned** — a NetBox error body
/// can echo the request, and the request carries the API token.
fn upstream_error(what: &str, e: &anyhow::Error) -> ApiError {
    tracing::warn!(error = %e, "netbox {what} failed");
    ApiError::bad_gateway(
        "netbox_upstream",
        format!("the NetBox {what} call failed; see the core log for detail"),
    )
}

fn no_server(id: Uuid) -> ApiError {
    ApiError::not_found("netbox_server_not_found", format!("no NetBox server {id}"))
}

/// The token as it will actually be sent, or a 400 if there is nothing to send.
///
/// 🚨 **Trim once, here, and use the result** — the three call sites disagreed before this existed:
/// `update` trimmed, `create` and `test` validated `trim()` and then used the **untrimmed** string.
/// A token pasted with a trailing newline was therefore sealed with the newline, and every later
/// sync failed with *"NetBox refused the API token"* — a message that sends the operator to check a
/// token that looks, and is, correct. The WebUI trims before sending, so this was reachable only
/// from a REST or MCP client; the asymmetry between the three handlers is the tell that it was
/// nobody's decision.
fn validated_token(raw: &str) -> Result<&str, ApiError> {
    let token = raw.trim();
    if token.is_empty() {
        return Err(ApiError::bad_request(
            "invalid_token",
            "token must not be empty",
        ));
    }
    Ok(token)
}

/// Validate the fields a create and an update share, so the two cannot drift apart.
fn validated_common(
    name: &str,
    base_url: &str,
    ca_cert_pem: Option<&str>,
    sync_interval_secs: i32,
) -> Result<String, ApiError> {
    if name.trim().is_empty() {
        return Err(ApiError::bad_request(
            "invalid_name",
            "name must not be empty",
        ));
    }
    let base = netbox::validate_base_url(base_url)
        .map_err(|e| ApiError::bad_request(e.code(), e.message()))?;
    if let Some(pem) = ca_cert_pem {
        netbox::validate_ca_pem(pem).map_err(|m| ApiError::bad_request("invalid_ca_cert", m))?;
    }
    if !(MIN_INTERVAL_SECS..=MAX_INTERVAL_SECS).contains(&sync_interval_secs) {
        return Err(ApiError::bad_request(
            "invalid_sync_interval",
            format!(
                "sync_interval_secs must be between {MIN_INTERVAL_SECS} and {MAX_INTERVAL_SECS}"
            ),
        ));
    }
    Ok(base)
}

/// A configured server as the API exposes it.
///
/// 🚨 **`credential_id` is here but the token is not, and cannot be** — the token lives sealed in
/// `credentials` and this type has no field that could carry it. `ca_cert_pem` *is* returned, on
/// purpose: a CA certificate is public-key material, and an operator who cannot read back what
/// they pasted cannot tell a stored certificate from a lost one.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct NetboxServerView {
    id: Uuid,
    name: String,
    base_url: String,
    credential_id: Uuid,
    ca_cert_pem: Option<String>,
    enabled: bool,
    sync_interval_secs: i32,
    /// `netbox-version` learned from the last successful connection, so the screen can state the
    /// supported range rather than guessing at it.
    api_version: Option<String>,
    last_sync_at: Option<String>,
    last_sync_ok: Option<bool>,
    last_sync_error: Option<String>,
    /// Folders this server owns that NetBox no longer lists. **Never auto-deleted** (ADR-100
    /// decision 5) — surfaced so the operator can decide.
    missing_folders: usize,
}

fn view(s: &NetboxServer, missing: usize) -> NetboxServerView {
    NetboxServerView {
        id: s.id,
        name: s.name.clone(),
        base_url: s.base_url.clone(),
        credential_id: s.credential_id,
        ca_cert_pem: s.ca_cert_pem.clone(),
        enabled: s.enabled,
        sync_interval_secs: s.sync_interval_secs,
        api_version: s.api_version.clone(),
        last_sync_at: s.last_sync_at.map(|t| t.to_rfc3339()),
        last_sync_ok: s.last_sync_ok,
        last_sync_error: s.last_sync_error.clone(),
        missing_folders: missing,
    }
}

#[utoipa::path(
    get, path = "/api/v1/netbox/servers", tag = "netbox",
    responses(
        (status = 200, description = "Every configured NetBox server. The API token is never included; the CA certificate is, because it is not a secret", body = Vec<NetboxServerView>),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks View", body = super::error::ErrorBody),
        (status = 503, description = "Inventory storage is unavailable (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn list_netbox_servers(
    _guard: RequireView,
    admin: Admin,
) -> ApiResult<Json<Vec<NetboxServerView>>> {
    Ok(Json(server_views(&admin.netbox).await?))
}

/// The configured servers as the API exposes them — the seam both edges call, so `get_config`'s
/// `netbox` section and this route cannot answer differently (ADR-042 read parity).
pub(crate) async fn server_views(repo: &NetboxRepo) -> Result<Vec<NetboxServerView>, ApiError> {
    let servers = repo.list().await.map_err(|e| {
        ApiError::from_internal(
            e.as_ref(),
            "list netbox servers",
            "failed to list NetBox servers",
        )
    })?;
    let mut out = Vec::with_capacity(servers.len());
    for s in &servers {
        let missing = repo.count_missing(s.id).await.map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "count netbox missing folders",
                "failed to read NetBox folder state",
            )
        })?;
        out.push(view(s, missing));
    }
    Ok(out)
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub(crate) struct CreateNetboxServerReq {
    name: String,
    /// The NetBox base URL, e.g. `https://netbox.example.com`. Validated against the SSRF policy
    /// before the token is ever sent to it.
    base_url: String,
    /// The API token. **Sealed on arrival and never returned by any endpoint.**
    token: String,
    /// PEM for a private CA, when NetBox is not signed by a publicly trusted one.
    #[serde(default)]
    ca_cert_pem: Option<String>,
    #[serde(default = "default_interval")]
    sync_interval_secs: i32,
}

const fn default_interval() -> i32 {
    3600
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct CreatedNetboxServer {
    id: Uuid,
}

#[utoipa::path(
    post, path = "/api/v1/netbox/servers", tag = "netbox",
    request_body = CreateNetboxServerReq,
    responses(
        (status = 201, description = "The server was registered and its token sealed", body = CreatedNetboxServer),
        (status = 400, description = "Empty name or token, a base_url that is malformed or refused by the SSRF policy, an invalid CA certificate, or an out-of-range interval", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
        (status = 503, description = "Inventory storage is unavailable (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn create_netbox_server(
    _guard: RequireManageConfig,
    admin: Admin,
    Json(body): Json<CreateNetboxServerReq>,
) -> ApiResult<(StatusCode, Json<CreatedNetboxServer>)> {
    let token = validated_token(&body.token)?;
    let base = validated_common(
        &body.name,
        &body.base_url,
        body.ca_cert_pem.as_deref(),
        body.sync_interval_secs,
    )?;

    // Sealed through the production writer, so the envelope-encryption columns have one author
    // (ADR-018). The plaintext exists only in this frame.
    let secret = serde_json::json!({ "token": token }).to_string();
    let credential_id = admin
        .creds
        .create(
            &format!("NetBox: {}", body.name.trim()),
            KIND_NETBOX_TOKEN,
            secret.as_bytes(),
        )
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "seal netbox token",
                "failed to store the NetBox token",
            )
        })?;

    let id = admin
        .netbox
        .create(
            body.name.trim(),
            &base,
            credential_id,
            body.ca_cert_pem.as_deref(),
            body.sync_interval_secs,
        )
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "create netbox server",
                "failed to register the NetBox server",
            )
        })?;
    Ok((StatusCode::CREATED, Json(CreatedNetboxServer { id })))
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub(crate) struct UpdateNetboxServerReq {
    name: String,
    base_url: String,
    /// A replacement token. Omitted (or null) leaves the sealed one alone — which is what lets the
    /// settings form round-trip without the browser ever holding the token.
    #[serde(default)]
    token: Option<String>,
    /// Three-state, and it has to be: absent leaves the stored CA alone, `null` clears it, a string
    /// replaces it. Without the middle state there is no way to remove a certificate short of
    /// deleting the server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ca_cert_pem: Option<Option<String>>,
    enabled: bool,
    #[serde(default = "default_interval")]
    sync_interval_secs: i32,
}

#[utoipa::path(
    put, path = "/api/v1/netbox/servers/{id}", tag = "netbox",
    params(("id" = Uuid, Path, description = "The server id")),
    request_body = UpdateNetboxServerReq,
    responses(
        (status = 204, description = "Updated"),
        (status = 400, description = "A field failed validation (see the create route)", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
        (status = 404, description = "No such server", body = super::error::ErrorBody),
        (status = 503, description = "Inventory storage is unavailable (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn update_netbox_server(
    _guard: RequireManageConfig,
    admin: Admin,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateNetboxServerReq>,
) -> ApiResult<StatusCode> {
    let ca = body.ca_cert_pem.as_ref().and_then(|o| o.as_deref());
    let base = validated_common(&body.name, &body.base_url, ca, body.sync_interval_secs)?;

    let existing = admin
        .netbox
        .get(id)
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "read netbox server",
                "failed to read the server",
            )
        })?
        .ok_or_else(|| no_server(id))?;

    // A replacement token becomes a new sealed credential rather than an in-place rewrite: the
    // store's `create` is the only writer of the envelope columns, and the old row stays until an
    // operator removes it, so a mistyped replacement is recoverable.
    let credential_id = match body
        .token
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
    {
        Some(token) => {
            let secret = serde_json::json!({ "token": token }).to_string();
            admin
                .creds
                .create(
                    &format!("NetBox: {}", body.name.trim()),
                    KIND_NETBOX_TOKEN,
                    secret.as_bytes(),
                )
                .await
                .map_err(|e| {
                    ApiError::from_internal(
                        e.as_ref(),
                        "seal netbox token",
                        "failed to store the NetBox token",
                    )
                })?
        }
        None => existing.credential_id,
    };

    let ca_arg = body.ca_cert_pem.as_ref().map(|o| o.as_deref());
    match admin
        .netbox
        .update(
            id,
            crate::netbox::ServerUpdate {
                name: body.name.trim(),
                base_url: &base,
                credential_id,
                ca_cert_pem: ca_arg,
                enabled: body.enabled,
                sync_interval_secs: body.sync_interval_secs,
            },
        )
        .await
    {
        Ok(true) => Ok(StatusCode::NO_CONTENT),
        Ok(false) => Err(no_server(id)),
        Err(e) => Err(ApiError::from_internal(
            e.as_ref(),
            "update netbox server",
            "failed to update the NetBox server",
        )),
    }
}

#[utoipa::path(
    delete, path = "/api/v1/netbox/servers/{id}", tag = "netbox",
    params(("id" = Uuid, Path, description = "The server id")),
    responses(
        (status = 204, description = "Removed. The folders it created are kept — disconnecting an integration never restructures the monitoring tree"),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
        (status = 404, description = "No such server", body = super::error::ErrorBody),
        (status = 503, description = "Inventory storage is unavailable (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn delete_netbox_server(
    _guard: RequireManageConfig,
    admin: Admin,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    match admin.netbox.delete(id).await {
        Ok(true) => Ok(StatusCode::NO_CONTENT),
        Ok(false) => Err(no_server(id)),
        Err(e) => Err(ApiError::from_internal(
            e.as_ref(),
            "delete netbox server",
            "failed to remove the NetBox server",
        )),
    }
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub(crate) struct TestNetboxReq {
    base_url: String,
    token: String,
    #[serde(default)]
    ca_cert_pem: Option<String>,
}

/// What a connection test found.
///
/// 🚨 The two booleans are separate **because a single 403 cannot tell them apart**, and that is
/// the whole value of this endpoint. NetBox requires authentication on `/api/status/`, but it sends
/// its `API-Version` header on every response *including* the unauthenticated refusal — so
/// `reachable && !authenticated` means "right address, wrong token", while `!reachable` means "not
/// a NetBox, or not that address". Collapsing them sends the operator to check the wrong field.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct TestNetboxResult {
    /// Something at that URL answered as a NetBox API.
    reachable: bool,
    /// …and it accepted the token.
    authenticated: bool,
    /// The `API-Version` header (e.g. `4.6`), present even when the token was refused.
    api_version: Option<String>,
    /// The full `netbox-version` (e.g. `4.6.9`), available only once authenticated.
    netbox_version: Option<String>,
}

#[utoipa::path(
    post, path = "/api/v1/netbox/test", tag = "netbox",
    request_body = TestNetboxReq,
    responses(
        (status = 200, description = "The probe ran. Check `reachable` and `authenticated` — a refused token is a 200 here, not an error, because the operator needs to see which of the two fields is wrong", body = TestNetboxResult),
        (status = 400, description = "base_url is malformed or refused by the SSRF policy, or the CA certificate is invalid", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
        (status = 502, description = "The NetBox call failed; the detail is logged, never returned", body = super::error::ErrorBody),
    ),
)]
async fn test_netbox_connection(
    _guard: RequireManageConfig,
    _admin: Admin,
    Json(body): Json<TestNetboxReq>,
) -> ApiResult<Json<TestNetboxResult>> {
    // The same trim `create` seals with, so a token that tests clean cannot then be stored dirty.
    let token = validated_token(&body.token)?;
    let base = netbox::validate_base_url(&body.base_url)
        .map_err(|e| ApiError::bad_request(e.code(), e.message()))?;
    if let Some(pem) = body.ca_cert_pem.as_deref() {
        netbox::validate_ca_pem(pem).map_err(|m| ApiError::bad_request("invalid_ca_cert", m))?;
    }
    let client = NetboxClient::new(&base, token, body.ca_cert_pem.as_deref())
        .map_err(|e| upstream_error("client setup", &e))?;
    let probe = client
        .probe()
        .await
        .map_err(|e| upstream_error("status", &e))?;
    Ok(Json(TestNetboxResult {
        reachable: probe.api_version.is_some(),
        authenticated: probe.authenticated,
        api_version: probe.api_version,
        netbox_version: probe.netbox_version,
    }))
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct SyncNetboxResult {
    regions: usize,
    sites: usize,
    /// Folders this server owns that NetBox no longer lists (ADR-100 decision 5 — marked, not
    /// deleted).
    missing_folders: usize,
}

#[utoipa::path(
    post, path = "/api/v1/netbox/servers/{id}/sync", tag = "netbox",
    params(("id" = Uuid, Path, description = "The server id")),
    responses(
        (status = 200, description = "The sync ran; the counts say what was mirrored", body = SyncNetboxResult),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
        (status = 404, description = "No such server", body = super::error::ErrorBody),
        (status = 502, description = "The NetBox call failed; the reason is stored on the server row and shown on the integration screen", body = super::error::ErrorBody),
        (status = 503, description = "Inventory storage is unavailable (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn sync_netbox_server(
    _guard: RequireManageConfig,
    admin: Admin,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<SyncNetboxResult>> {
    let server = admin
        .netbox
        .get(id)
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "read netbox server",
                "failed to read the server",
            )
        })?
        .ok_or_else(|| no_server(id))?;

    // The same function the leader task calls, so a manual sync and a scheduled one cannot behave
    // differently — including recording the failure on the row, which is what puts the reason on
    // the screen rather than only in the container log.
    let report = netbox::sync_server(&admin.netbox, &admin.creds, &server)
        .await
        .map_err(|e| upstream_error("sync", &e))?;
    Ok(Json(SyncNetboxResult {
        regions: report.regions,
        sites: report.sites,
        missing_folders: report.missing,
    }))
}

#[cfg(test)]
mod tests {
    use super::super::tests_support::*;
    use axum::http::StatusCode;

    /// The accepted-write test `api/guards.rs` demands of every domain that registers one.
    ///
    /// 🚨 It asserts **201**, not `is_success()`. Nine of the thirty-six endpoints measured in
    /// ADR-115 answer 204, and `is_success()` cannot tell the two apart — a suite that only knows
    /// "not an error" is how a documented status drifts from the served one.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_netbox_server_registration_is_accepted_and_lands_in_the_database(
        pool: sqlx::PgPool,
    ) {
        let st = live_state(pool.clone()).await;
        let token = token(&st, yagra_common::Role::Admin);

        let res = send(
            &st,
            "POST",
            "/api/v1/netbox/servers",
            &token,
            Some(serde_json::json!({
                "name": "lab",
                "base_url": "http://192.168.1.214:8000/",
                "token": "0123456789abcdef",
                "sync_interval_secs": 3600
            })),
        )
        .await;
        assert_eq!(res.0, StatusCode::CREATED, "body: {}", res.1);

        assert_eq!(crate::pgtest::rows(&pool, "netbox_servers").await, 1);
        // The token must have been sealed, not stored beside the server row.
        assert_eq!(crate::pgtest::rows(&pool, "credentials").await, 1);
        let kind: String = sqlx::query_scalar("SELECT kind FROM credentials")
            .fetch_one(&pool)
            .await
            .expect("kind");
        assert_eq!(kind, crate::secrets::KIND_NETBOX_TOKEN);
        // …and no column anywhere holds it in the clear.
        let leaked: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM netbox_servers \
             WHERE base_url LIKE '%0123456789abcdef%' OR name LIKE '%0123456789abcdef%'",
        )
        .fetch_one(&pool)
        .await
        .expect("leak check");
        assert_eq!(
            leaked, 0,
            "the token must never appear in a plaintext column"
        );
    }

    /// 🚨 The defect this pins was found while diagnosing a live "NetBox refused the API token"
    /// (2026-09-03). That one turned out to be a genuinely wrong token — but the diagnosis showed
    /// `create` and `test` validating `token.trim()` and then using the **untrimmed** string, while
    /// `update` trimmed. A token pasted with a trailing newline would have been sealed with it and
    /// then refused forever, with a message pointing at a token that is correct.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_token_is_sealed_trimmed_so_a_pasted_newline_cannot_break_every_later_sync(
        pool: sqlx::PgPool,
    ) {
        let st = live_state(pool.clone()).await;
        let hdr = token(&st, yagra_common::Role::Admin);
        let res = send(
            &st,
            "POST",
            "/api/v1/netbox/servers",
            &hdr,
            Some(serde_json::json!({
                "name": "lab",
                "base_url": "http://10.0.0.1:8000/",
                "token": "  0123456789abcdef\n",
                "sync_interval_secs": 3600
            })),
        )
        .await;
        assert_eq!(res.0, StatusCode::CREATED, "body: {}", res.1);

        // Read the sealed document back through the production reader — asserting on the column
        // would only prove something was written, not that the right bytes were.
        let cred_id: uuid::Uuid = sqlx::query_scalar("SELECT credential_id FROM netbox_servers")
            .fetch_one(&pool)
            .await
            .expect("credential_id");
        let store = crate::secrets::CredentialStore::new(pool.clone(), crate::pgtest::kek());
        let (kind, bytes) = store.open(cred_id).await.expect("open").expect("row");
        assert_eq!(kind, crate::secrets::KIND_NETBOX_TOKEN);
        let secret = crate::secrets::NetboxTokenSecret::parse(&bytes).expect("parses");
        assert_eq!(
            secret.token, "0123456789abcdef",
            "the sealed token must be what will actually be sent, not what was pasted"
        );

        // …and the emptiness check still fires on something that is only whitespace.
        let res = send(
            &st,
            "POST",
            "/api/v1/netbox/servers",
            &hdr,
            Some(serde_json::json!({
                "name": "blank", "base_url": "http://10.0.0.2:8000/", "token": "   \n ",
                "sync_interval_secs": 3600
            })),
        )
        .await;
        assert_eq!(res.0, StatusCode::BAD_REQUEST, "body: {}", res.1);
    }

    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn the_ssrf_policy_is_enforced_at_the_edge_and_private_addresses_still_work(
        pool: sqlx::PgPool,
    ) {
        let st = live_state(pool.clone()).await;
        let token = token(&st, yagra_common::Role::Admin);
        let body = |url: &str| {
            Some(serde_json::json!({
                "name": "x", "base_url": url, "token": "t", "sync_interval_secs": 3600
            }))
        };

        for blocked in [
            "http://127.0.0.1:8000/",
            "http://[::1]:8000/",
            "http://169.254.169.254/",
            "ftp://netbox.example.com/",
        ] {
            let res = send(&st, "POST", "/api/v1/netbox/servers", &token, body(blocked)).await;
            assert_eq!(
                res.0,
                StatusCode::BAD_REQUEST,
                "{blocked} must be refused before the token is sent anywhere: {}",
                res.1
            );
        }
        // The accept side. An NMS reaches inside the perimeter, so refusing RFC1918 would make the
        // feature useless — and "refuse everything" satisfies every assertion above on its own.
        let res = send(
            &st,
            "POST",
            "/api/v1/netbox/servers",
            &token,
            body("http://10.20.30.40:8000/"),
        )
        .await;
        assert_eq!(res.0, StatusCode::CREATED, "body: {}", res.1);
    }

    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_private_key_pasted_into_the_certificate_box_is_refused(pool: sqlx::PgPool) {
        // It would land in a plaintext column this API returns. Refusing is the only safe answer:
        // silently stripping it leaves the operator believing a key they exposed is still private.
        let st = live_state(pool.clone()).await;
        let token = token(&st, yagra_common::Role::Admin);
        let kp = rcgen::KeyPair::generate().expect("keypair");
        let res = send(
            &st,
            "POST",
            "/api/v1/netbox/servers",
            &token,
            Some(serde_json::json!({
                "name": "x",
                "base_url": "http://10.0.0.1:8000/",
                "token": "t",
                "ca_cert_pem": kp.serialize_pem(),
                "sync_interval_secs": 3600
            })),
        )
        .await;
        assert_eq!(res.0, StatusCode::BAD_REQUEST, "body: {}", res.1);
        assert_eq!(crate::pgtest::rows(&pool, "netbox_servers").await, 0);
        assert_eq!(
            crate::pgtest::rows(&pool, "credentials").await,
            0,
            "and nothing is sealed on the way to the refusal"
        );
    }

    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn the_listing_never_carries_the_token(pool: sqlx::PgPool) {
        let st = live_state(pool.clone()).await;
        let token_hdr = token(&st, yagra_common::Role::Admin);
        send(
            &st,
            "POST",
            "/api/v1/netbox/servers",
            &token_hdr,
            Some(serde_json::json!({
                "name": "lab",
                "base_url": "http://10.0.0.1:8000/",
                "token": "s3cr3t-token-value",
                "sync_interval_secs": 3600
            })),
        )
        .await;

        let res = send(&st, "GET", "/api/v1/netbox/servers", &token_hdr, None).await;
        assert_eq!(res.0, StatusCode::OK);
        assert!(
            !res.1.to_string().contains("s3cr3t-token-value"),
            "the API token must never be returned: {}",
            res.1
        );
        assert!(
            res.1.to_string().contains("credential_id"),
            "…only its reference"
        );
    }
}
