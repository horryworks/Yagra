// SPDX-License-Identifier: AGPL-3.0-only
//! Device discovery — sweep a set of addresses, review what answered, import the ones you want.
//!
//! `ManageConfig`, except the candidates view: a sweep sends SNMP at addresses on the operator's
//! network with credentials they nominate, which is a configuration act, not a read.
//!
//! **Credentials are resolved server-side, by id** (ADR-018/020). The request names stored
//! credentials; this module opens them, converts them into the sweep job's inline form, and hands
//! them to the bus. So the only place plaintext exists is in memory here and in the job — and every
//! error this module can return about a credential names an **id and a static reason, never any
//! secret content** (security.md).
//!
//! The sweep is bounded at [`MAX_SCAN_TARGETS`] because it is the one endpoint that turns a single
//! request into a large amount of outbound traffic.

use super::error::{ApiError, ApiResult};
use super::extract::{Admin, RequireManageConfig, RequireView};
use super::ApiState;
use crate::secrets::CredentialStore;
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::net::IpAddr;
use std::time::Instant;
use uuid::Uuid;

/// Most targets a single scan may sweep. The cap is what keeps one request from becoming an
/// unbounded outbound scan of someone else's network.
const MAX_SCAN_TARGETS: usize = 1024;

/// This domain's slice of the OpenAPI document (ADR-035), merged by [`super::openapi::document`].
#[derive(utoipa::OpenApi)]
#[openapi(paths(
    start_discovery_scan,
    get_discovery_scan,
    import_discovered,
    discovery_candidates
))]
pub(super) struct Doc;

/// The discovery routes, merged into `/api/v1` by [`super::router`].
pub(super) fn routes() -> Router<ApiState> {
    Router::new()
        .route("/api/v1/discovery/scan", post(start_discovery_scan))
        .route("/api/v1/discovery/scan/:id", get(get_discovery_scan))
        .route("/api/v1/discovery/import", post(import_discovered))
        .route("/api/v1/discovery/candidates", get(discovery_candidates))
}

/// Start-scan body: explicit target IPs (the WebUI expands a CIDR), candidate stored credentials by
/// id, and ad-hoc communities.
#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct StartScan {
    targets: Vec<String>,
    #[serde(default)]
    communities: Vec<String>,
    #[serde(default)]
    credential_ids: Vec<String>,
    /// Poll-pool to run the sweep in (ADR-009/020). Absent/empty = legacy global discovery.
    #[serde(default)]
    pool: Option<String>,
}

/// The accepted scan's id, for polling its status.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct StartedScan {
    scan_id: Uuid,
}

/// How many nodes an import created.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct ImportResult {
    created: u32,
}

/// Resolve stored credential ids into the inline candidates the sweep job carries.
///
/// Every error names an id and a static reason. None of them can carry secret content — that is a
/// deliberate property of this function, not an accident of the current messages.
async fn resolve_scan_credentials(
    creds: &CredentialStore,
    ids: &[String],
) -> Result<Vec<yagra_bus::DiscoveryCredential>, ApiError> {
    let mut out = Vec::with_capacity(ids.len());
    for raw in ids {
        let Ok(id) = raw.parse::<Uuid>() else {
            return Err(ApiError::bad_request(
                "invalid_credential",
                format!("'{raw}' is not a valid credential id"),
            ));
        };
        let opened = creds.open(id).await.map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "open scan credential",
                "failed to resolve a scan credential",
            )
        })?;
        let Some((kind, secret)) = opened else {
            return Err(ApiError::bad_request(
                "credential_not_found",
                format!("no credential {id}"),
            ));
        };
        if kind == crate::secrets::KIND_SNMP_V3 {
            match crate::secrets::SnmpV3Secret::parse(&secret) {
                Ok(v3) => out.push(yagra_bus::DiscoveryCredential {
                    cred_ref: id,
                    community: None,
                    v3: Some(yagra_bus::DiscoveryV3 {
                        user: v3.user,
                        security_level: v3.security_level,
                        auth_protocol: v3.auth_protocol,
                        auth_key: v3.auth_key,
                        priv_protocol: v3.priv_protocol,
                        priv_key: v3.priv_key,
                    }),
                }),
                Err(reason) => {
                    return Err(ApiError::bad_request(
                        "invalid_credential",
                        format!("credential {id} is not usable: {reason}"),
                    ))
                }
            }
        } else {
            match String::from_utf8(secret) {
                Ok(community) => out.push(yagra_bus::DiscoveryCredential {
                    cred_ref: id,
                    community: Some(community),
                    v3: None,
                }),
                Err(_) => {
                    return Err(ApiError::bad_request(
                        "invalid_credential",
                        format!("credential {id} is not usable as an SNMP community"),
                    ))
                }
            }
        }
    }
    Ok(out)
}

#[utoipa::path(
    post, path = "/api/v1/discovery/scan", tag = "discovery",
    request_body = StartScan,
    responses(
        (status = 202, description = "Sweep accepted; poll its status by id", body = StartedScan),
        (status = 400, description = "No targets or more than the cap, an unparseable address, or a named credential that is missing or unusable", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role below Admin", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode has no write side", body = super::error::ErrorBody),
    ),
)]
async fn start_discovery_scan(
    _guard: RequireManageConfig,
    admin: Admin,
    Json(body): Json<StartScan>,
) -> ApiResult<(StatusCode, Json<StartedScan>)> {
    if body.targets.is_empty() || body.targets.len() > MAX_SCAN_TARGETS {
        return Err(ApiError::bad_request(
            "invalid_scan",
            format!("targets must be 1..={MAX_SCAN_TARGETS} addresses"),
        ));
    }
    // Parsed into `IpAddr` here, so nothing that is not an address reaches the sweep.
    let mut targets = Vec::with_capacity(body.targets.len());
    for t in &body.targets {
        let ip = t.parse::<IpAddr>().map_err(|_| {
            ApiError::bad_request(
                "invalid_address",
                format!("'{t}' is not a valid IP address"),
            )
        })?;
        targets.push(ip);
    }
    let credentials = resolve_scan_credentials(&admin.creds, &body.credential_ids).await?;
    // Route to a pool's own discovery subject only when that pool actually has a live poller;
    // otherwise fall back to the legacy global subject. That fallback is both the N/N-1 compat path
    // (an old wildcard poller still absorbs the sweep) and the guard against a typo'd pool name
    // black-holing the scan.
    let requested_pool = body
        .pool
        .as_deref()
        .map(str::trim)
        .filter(|p| !p.is_empty());
    let pool_route = match requested_pool {
        Some(p) if admin.coordinator.live_pools(Instant::now()).contains(p) => Some(p),
        _ => None,
    };
    let scan_id = admin
        .discovery
        .start(targets, body.communities, credentials, pool_route)
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "start discovery scan",
                "failed to start discovery scan",
            )
        })?;
    Ok((StatusCode::ACCEPTED, Json(StartedScan { scan_id })))
}

#[utoipa::path(
    get, path = "/api/v1/discovery/scan/{id}", tag = "discovery",
    params(("id" = Uuid, Path, description = "Scan id returned when the sweep was accepted")),
    responses(
        (status = 200, description = "Progress and the candidates found so far", body = crate::discovery::ScanStatus),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role below Admin", body = super::error::ErrorBody),
        (status = 404, description = "No such scan", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode has no write side", body = super::error::ErrorBody),
    ),
)]
async fn get_discovery_scan(
    _guard: RequireManageConfig,
    admin: Admin,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<crate::discovery::ScanStatus>> {
    admin
        .discovery
        .get(id)
        .map(Json)
        .ok_or_else(|| ApiError::not_found("scan_not_found", format!("no scan {id}")))
}

/// One discovered device the operator chose to add.
#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct ImportNode {
    address: String,
    name: String,
    profile_id: Option<String>,
    credential_id: Option<String>,
    /// Maker/model pre-filled from discovery's sysDescr classification (editable before import).
    #[serde(default)]
    vendor: Option<String>,
    #[serde(default)]
    model: Option<String>,
}

/// Import body: the selected devices to create as nodes.
#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct ImportDiscovered {
    nodes: Vec<ImportNode>,
}

#[utoipa::path(
    post, path = "/api/v1/discovery/import", tag = "discovery",
    request_body = ImportDiscovered,
    responses(
        (status = 201, description = "Nodes created, in one transaction", body = ImportResult),
        (status = 400, description = "An unparseable address, an empty name, or a binding id that is not a UUID", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role below Admin", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode has no write side", body = super::error::ErrorBody),
    ),
)]
async fn import_discovered(
    _guard: RequireManageConfig,
    admin: Admin,
    Json(body): Json<ImportDiscovered>,
) -> ApiResult<(StatusCode, Json<ImportResult>)> {
    let parse_uuid = |s: &Option<String>| -> Result<Option<Uuid>, ()> {
        match s {
            None => Ok(None),
            Some(v) => v.parse::<Uuid>().map(Some).map_err(|_| ()),
        }
    };
    // Every node is validated up front and the batch is then inserted in one transaction, so a
    // failure partway cannot leave half an import behind (NodeRepo::import_nodes).
    let mut prepared: Vec<crate::repo::NewNode<'_>> = Vec::with_capacity(body.nodes.len());
    for n in &body.nodes {
        let Ok(addr) = n.address.parse::<IpAddr>() else {
            return Err(ApiError::bad_request(
                "invalid_address",
                format!("'{}' is not a valid IP address", n.address),
            ));
        };
        let name = n.name.trim();
        if name.is_empty() {
            return Err(ApiError::bad_request(
                "invalid_node",
                "name must not be empty",
            ));
        }
        let (Ok(profile), Ok(credential)) =
            (parse_uuid(&n.profile_id), parse_uuid(&n.credential_id))
        else {
            return Err(ApiError::bad_request(
                "invalid_binding",
                "profile_id/credential_id must be UUIDs",
            ));
        };
        prepared.push(crate::repo::NewNode {
            name,
            address: addr,
            profile,
            credential,
            vendor: n.vendor.as_deref().map(str::trim).filter(|s| !s.is_empty()),
            model: n.model.as_deref().map(str::trim).filter(|s| !s.is_empty()),
        });
    }
    let created = admin.repo.import_nodes(&prepared).await.map_err(|e| {
        ApiError::from_internal(
            e.as_ref(),
            "import discovered nodes",
            "failed to import discovered nodes",
        )
    })?;
    Ok((StatusCode::CREATED, Json(ImportResult { created })))
}

/// Query for the standing discovery-candidates view.
#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub(super) struct CandidatesQuery {
    limit: Option<usize>,
}

/// Recent discovered (unclassified) devices across in-memory scans — the dashboard "discovery
/// queue".
///
/// `View`, unlike the rest of this module, because it reports what has been seen rather than
/// causing anything to happen. Empty in skeleton mode: with no discovery runner there are genuinely
/// no candidates, which is an answer rather than an outage.
#[utoipa::path(
    get, path = "/api/v1/discovery/candidates", tag = "discovery",
    params(CandidatesQuery),
    responses(
        (status = 200, description = "Recent unclassified devices; empty in skeleton mode", body = Vec<crate::discovery::Candidate>),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks read permission", body = super::error::ErrorBody),
    ),
)]
async fn discovery_candidates(
    _guard: RequireView,
    State(st): State<ApiState>,
    Query(q): Query<CandidatesQuery>,
) -> ApiResult<Json<Value>> {
    let Some(admin) = st.admin.as_ref() else {
        return Ok(Json(Value::Array(Vec::new())));
    };
    let limit = q.limit.unwrap_or(10).clamp(1, 50);
    let candidates = admin.discovery.recent_candidates(limit);
    Ok(Json(
        serde_json::to_value(candidates).unwrap_or(Value::Array(Vec::new())),
    ))
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
            ("POST", "/api/v1/discovery/scan".to_owned()),
            ("GET", format!("/api/v1/discovery/scan/{ID}")),
            ("POST", "/api/v1/discovery/import".to_owned()),
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
    async fn sweeping_and_importing_are_closed_to_everyone_below_admin() {
        // A sweep sends traffic at the operator's network using credentials it names. That is not
        // something a viewer or an operator gets to trigger.
        for (method, path) in config_routes() {
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
            for (method, path) in config_routes() {
                assert_eq!(
                    status_of(st.clone(), method, &path, Some(&token)).await,
                    StatusCode::FORBIDDEN,
                    "{role:?} {method} {path}"
                );
            }
        }
    }

    #[tokio::test]
    async fn the_candidates_queue_reads_openly_and_is_empty_without_a_runner() {
        let resp = router(public_state())
            .oneshot(
                Request::builder()
                    .uri("/api/v1/discovery/candidates")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&bytes).unwrap(),
            serde_json::json!([])
        );
    }
}
