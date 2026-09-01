// SPDX-License-Identifier: AGPL-3.0-only
//! Forwarding destinations — the passive-data tee (ADR-034).
//!
//! `ManageSystem` throughout (ADR-057): forwarding is the clearest case of "data leaves the box",
//! so it stayed with the administrator when monitoring configuration moved down to the operator.
//! A destination says "everything matching this filter also goes there",
//! so defining one is an egress decision: it sends received syslog, traps and flow exports to a
//! system outside Yagra, and those payloads routinely contain credentials.
//!
//! **Validation happens at the edge, deliberately and exhaustively** ([`validate_forward_body`]).
//! Everything an operator can get wrong — a filter that does not compile, a certificate that does
//! not parse, a service-account key Google will refuse, a source/destination pairing that cannot
//! carry byte-exact bytes — is a `400` here rather than a destination that fails on every message
//! and reports it only on a status page nobody is watching.
//!
//! **Secrets are write-only.** A destination holds at most one (an SNMP community or a GCP
//! service-account key), chosen by its kind; omitting it on update keeps the stored value. The
//! `ca_cert` is *not* a secret and so round-trips normally.
//!
//! The self-loop guard is best-effort by design: core can only see the ports pollers bind, so it
//! catches the realistic mistake (aiming a destination at localhost on a listener port) and cannot
//! catch every possible cycle.

use super::error::{ApiError, ApiResult};
use super::extract::{Admin, RequireManageSystem};
use super::util::CreatedId;
use super::ApiState;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// This domain's slice of the OpenAPI document (ADR-035), merged by [`super::openapi::document`].
#[derive(utoipa::OpenApi)]
#[openapi(paths(
    list_forward_destinations,
    create_forward_destination,
    update_forward_destination,
    delete_forward_destination,
    test_forward_destination,
    forwarding_status
))]
pub(super) struct Doc;

/// The forwarding routes, merged into `/api/v1` by [`super::router`].
pub(super) fn routes() -> Router<ApiState> {
    Router::new()
        .route(
            "/api/v1/forwarding/destinations",
            get(list_forward_destinations).post(create_forward_destination),
        )
        .route(
            "/api/v1/forwarding/destinations/:id",
            axum::routing::put(update_forward_destination).delete(delete_forward_destination),
        )
        .route(
            "/api/v1/forwarding/destinations/:id/test",
            post(test_forward_destination),
        )
        .route("/api/v1/forwarding/status", get(forwarding_status))
}

/// Request body for creating/updating a forwarding destination.
#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct ForwardDestinationBody {
    /// Human label (unique, 1–120 chars).
    name: String,
    /// Whether the forwarder should send to it. Defaults to enabled.
    #[serde(default = "default_true")]
    enabled: bool,
    /// Which received stream to tee.
    source_kind: yagra_forward::SourceKind,
    /// How to speak to the collector.
    dest_kind: yagra_forward::DestKind,
    /// `host:port` of the collector.
    target: String,
    /// Restrict to one poller pool; omitted/null = every pool.
    #[serde(default)]
    pool: Option<String>,
    /// Relay the original bytes (default) rather than re-rendering from the parsed fields.
    #[serde(default = "default_true")]
    verbatim: bool,
    /// The filter; omitted = forward the whole stream.
    #[serde(default)]
    filter: yagra_forward::FilterExpr,
    /// Optional messages/second ceiling.
    #[serde(default)]
    rate_limit_per_sec: Option<u32>,
    /// SNMP community for re-encoded traps (`snmp_trap_udp` only). Sealed at rest; on update,
    /// omitting it keeps the stored value.
    #[serde(default)]
    community: Option<String>,
    /// PEM certificate(s) to trust for a TLS destination, in addition to the system roots. Not a
    /// secret, so unlike `community` it round-trips and an empty value clears it.
    #[serde(default)]
    ca_cert: Option<String>,
    /// Google service-account key JSON for a `bigquery` destination. Sealed at rest; on update,
    /// omitting it keeps the stored key. Omitting it on create selects Workload Identity (the
    /// GCE/GKE metadata server), which stores no secret at all.
    #[serde(default)]
    service_account_json: Option<String>,
}

const fn default_true() -> bool {
    true
}

/// Validate a destination body and turn it into store input.
///
/// Everything an operator can get wrong is rejected here, so the forwarder only ever sees
/// configurations it can honour. That is the point of doing it at the edge: a filter that does
/// not compile, a certificate that does not parse, or a service-account key Google will refuse
/// would otherwise become a destination that fails on every message and reports it only on a
/// status page nobody is watching.
fn validate_forward_body(
    st: &ApiState,
    body: ForwardDestinationBody,
) -> Result<crate::forward_store::ForwardDestinationInput, ApiError> {
    let bad = ApiError::bad_request;

    let name = body.name.trim().to_owned();
    if name.is_empty() || name.chars().count() > 120 {
        return Err(bad("invalid_name", "name must be 1–120 characters"));
    }
    // Two target shapes: `host:port` for the socket kinds, `project.dataset.table` for BigQuery.
    // Validated per kind here so a typo is a 400 with a useful message, rather than a destination
    // that only reports "404" from Google after an admin enables it.
    let target = body.target.trim().to_owned();
    let mut socket_port = None;
    if body.dest_kind.is_host_port() {
        let Some((_, port)) = split_host_port(&target) else {
            return Err(bad(
                "invalid_target",
                "target must be host:port (use [addr]:port for a literal IPv6 address)",
            ));
        };
        socket_port = Some(port);
    } else if let Err(e) = crate::bigquery::BigQueryTarget::parse(&target) {
        return Err(ApiError::bad_request(
            "invalid_target",
            format!("target must be project.dataset.table — {e}"),
        ));
    }
    if !body.dest_kind.accepts(body.source_kind) {
        return Err(bad(
            "incompatible_kinds",
            "that destination kind cannot carry that source stream",
        ));
    }
    // Refusing verbatim here (rather than quietly ignoring it) keeps the UI honest: a trap PDU sent
    // to a syslog collector would be undecodable, so that pairing is always re-rendered.
    if body.verbatim && !body.dest_kind.supports_verbatim(body.source_kind) {
        return Err(bad(
            "verbatim_unsupported",
            "byte-exact relay is not possible for this source/destination pairing",
        ));
    }
    // ...and the converse for flow: a template-bound binary export has no rendered form, so
    // "rebuild from the parsed fields" is not a choice that exists.
    if !body.verbatim && !body.dest_kind.supports_rendered(body.source_kind) {
        return Err(bad(
            "rendered_unsupported",
            "a flow export can only be relayed byte-for-byte — there is no rendered form",
        ));
    }
    if body.rate_limit_per_sec == Some(0) {
        return Err(bad(
            "invalid_rate_limit",
            "rate limit must be greater than zero, or omitted for no limit",
        ));
    }
    let pool = body
        .pool
        .map(|p| p.trim().to_owned())
        .filter(|p| !p.is_empty());
    if pool.as_ref().is_some_and(|p| p.chars().count() > 64) {
        return Err(bad("invalid_pool", "pool must be at most 64 characters"));
    }

    // Compile the filter now so a broken expression is a 400, never a destination that silently
    // drops everything (or, worse, forwards everything) once the dispatcher picks it up.
    yagra_forward::compile(&body.filter)
        .map_err(|e| ApiError::bad_request("invalid_filter", e.to_string()))?;
    // A condition on a field the stream never carries can only ever be a mistake.
    if let Some(c) = body
        .filter
        .conditions
        .iter()
        .find(|c| !c.field.applies_to(body.source_kind))
    {
        return Err(ApiError::bad_request(
            "invalid_filter",
            format!(
                "field `{}` never appears on {} events",
                c.field.as_str(),
                body.source_kind.as_str()
            ),
        ));
    }

    // Loop guard: forwarding Yagra's own output back into one of its own listeners amplifies
    // without bound. Core can only see the *ports* pollers bind (a heartbeat advertises the bind
    // address, not the poller's routable address), so this catches the realistic mistake — aiming a
    // destination at localhost on a listener port — and cannot catch every possible cycle.
    // (BigQuery has no port and cannot address a Yagra listener at all, so it skips this.)
    if let (Some(admin), Some(port)) = (st.admin.as_ref(), socket_port) {
        let loopback_target = target_is_local(&target);
        if loopback_target
            && admin
                .coordinator
                .listener_ports(std::time::Instant::now())
                .contains(&port)
        {
            return Err(bad(
                "self_loop",
                "that target is one of Yagra's own listeners — forwarding to it would loop",
            ));
        }
    }

    let community = body
        .community
        .map(|c| c.trim().to_owned())
        .filter(|c| !c.is_empty());
    if community.is_some() && body.dest_kind != yagra_forward::DestKind::SnmpTrapUdp {
        return Err(bad(
            "invalid_community",
            "a community only applies to an SNMP trap destination",
        ));
    }

    let ca_cert = body
        .ca_cert
        .map(|c| c.trim().to_owned())
        .filter(|c| !c.is_empty());
    if ca_cert.is_some() && !body.dest_kind.is_tls() {
        return Err(bad(
            "invalid_ca_cert",
            "a CA certificate only applies to a TLS destination",
        ));
    }
    // Parse now so a mistyped certificate is a 400 rather than a destination that fails every
    // handshake with an error only visible on the status page.
    if let Some(pem) = ca_cert.as_deref() {
        if pem.len() > MAX_CA_CERT_BYTES {
            return Err(bad(
                "invalid_ca_cert",
                "the CA certificate is too large (max 64 KiB)",
            ));
        }
        use rustls::pki_types::{pem::PemObject, CertificateDer};
        let parsed = CertificateDer::pem_slice_iter(pem.as_bytes()).collect::<Result<Vec<_>, _>>();
        if parsed.map(|c| c.is_empty()).unwrap_or(true) {
            return Err(bad(
                "invalid_ca_cert",
                "that is not a PEM certificate (expected a -----BEGIN CERTIFICATE----- block)",
            ));
        }
    }

    let service_account_json = body
        .service_account_json
        .map(|j| j.trim().to_owned())
        .filter(|j| !j.is_empty());
    if service_account_json.is_some() && body.dest_kind != yagra_forward::DestKind::BigQuery {
        return Err(bad(
            "invalid_service_account",
            "a service-account key only applies to a BigQuery destination",
        ));
    }
    if let Some(json) = service_account_json.as_deref() {
        if json.len() > MAX_SERVICE_ACCOUNT_BYTES {
            return Err(bad(
                "invalid_service_account",
                "the service-account key is too large (max 16 KiB)",
            ));
        }
        // Parse and reject here so a mistyped key is a 400 rather than a destination whose every
        // batch fails with an error only visible on the status page. The error text is Google-shaped
        // and never quotes key material.
        crate::gcp::validate_service_account(json)
            .map_err(|e| ApiError::bad_request("invalid_service_account", e))?;
    }

    // Exactly one secret per destination, and which one is decided by the destination kind — the
    // two are mutually exclusive above, so this cannot silently discard the other.
    let secret = match (community, service_account_json) {
        (Some(community), _) => Some(crate::forward_store::DestSecret::SnmpCommunity { community }),
        (None, Some(json)) => Some(crate::forward_store::DestSecret::GcpServiceAccount { json }),
        (None, None) => None,
    };

    Ok(crate::forward_store::ForwardDestinationInput {
        name,
        enabled: body.enabled,
        source_kind: body.source_kind,
        dest_kind: body.dest_kind,
        target,
        pool,
        verbatim: body.verbatim,
        filter: body.filter,
        rate_limit_per_sec: body.rate_limit_per_sec,
        ca_cert,
        secret,
    })
}

/// Ceiling on a stored service-account key. A real Google key file is ~2.3 KiB.
const MAX_SERVICE_ACCOUNT_BYTES: usize = 16 * 1024;

/// Ceiling on a destination's PEM trust bundle. Generous for a chain, small enough that the column
/// cannot be used as arbitrary storage.
const MAX_CA_CERT_BYTES: usize = 64 * 1024;

/// Split `host:port`, accepting `[::1]:514` for a literal IPv6 address. Returns `None` unless both
/// halves are present and the port is a valid non-zero number.
fn split_host_port(target: &str) -> Option<(String, u16)> {
    let (host, port) = if let Some(rest) = target.strip_prefix('[') {
        let (host, rest) = rest.split_once(']')?;
        (host.to_owned(), rest.strip_prefix(':')?)
    } else {
        let (host, port) = target.rsplit_once(':')?;
        // A bare IPv6 literal has several colons and no brackets — reject it rather than treating
        // its last group as a port.
        if host.contains(':') {
            return None;
        }
        (host.to_owned(), port)
    };
    let port: u16 = port.parse().ok()?;
    if host.is_empty() || host.len() > 255 || port == 0 {
        return None;
    }
    Some((host, port))
}

/// Whether a target names this machine. Only literal forms are checked — resolving a hostname here
/// would block the handler on DNS, and the guard this backs is best-effort by design.
fn target_is_local(target: &str) -> bool {
    let Some((host, _)) = split_host_port(target) else {
        return false;
    };
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    host.parse::<std::net::IpAddr>()
        .is_ok_and(|ip| ip.is_loopback() || ip.is_unspecified())
}

/// The outcome of a one-shot delivery test.
///
/// `delivered: false` is a *successful* request reporting a failed probe, not a 5xx: the
/// destination's configuration is the caller's, and the transport error is what tells them which
/// part of it is wrong. The error text is the transport's, never a payload — syslog bodies carry
/// credentials.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct TestResult {
    delivered: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

/// Translate a store write failure, mapping the unique-name violation to a `409`.
fn write_error(e: &anyhow::Error, what: &'static str, msg: &'static str) -> ApiError {
    if e.downcast_ref::<sqlx::Error>()
        .and_then(sqlx::Error::as_database_error)
        .is_some_and(|db| db.is_unique_violation())
    {
        return ApiError::conflict(
            "duplicate_name",
            "a forwarding destination with that name already exists",
        );
    }
    ApiError::from_internal(e.as_ref(), what, msg)
}

#[utoipa::path(
    get, path = "/api/v1/forwarding/destinations", tag = "forwarding",
    responses(
        (status = 200, description = "Every destination, without its stored secret", body = Vec<crate::forward_store::ForwardDestination>),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageSystem", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode: no destination store", body = super::error::ErrorBody),
    ),
)]
async fn list_forward_destinations(
    _guard: RequireManageSystem,
    admin: Admin,
) -> ApiResult<Json<Vec<crate::forward_store::ForwardDestination>>> {
    let rows = admin.forward.list().await.map_err(|e| {
        ApiError::from_internal(
            e.as_ref(),
            "forwarding destination list",
            "failed to list forwarding destinations",
        )
    })?;
    Ok(Json(rows))
}

#[utoipa::path(
    post, path = "/api/v1/forwarding/destinations", tag = "forwarding",
    request_body = ForwardDestinationBody,
    responses(
        (status = 201, description = "Destination created", body = CreatedId),
        (status = 400, description = "Edge validation rejected the destination (target, kind pairing, filter, certificate, credential, rate limit, or the destination cap)", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageSystem", body = super::error::ErrorBody),
        (status = 409, description = "A destination with that name already exists", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode: no destination store", body = super::error::ErrorBody),
    ),
)]
async fn create_forward_destination(
    _guard: RequireManageSystem,
    State(st): State<ApiState>,
    admin: Admin,
    Json(body): Json<ForwardDestinationBody>,
) -> ApiResult<(StatusCode, Json<CreatedId>)> {
    let input = validate_forward_body(&st, body)?;
    let n = admin.forward.count().await.map_err(|e| {
        ApiError::from_internal(
            e.as_ref(),
            "forwarding destination count",
            "failed to create the forwarding destination",
        )
    })?;
    if n >= crate::forward_store::MAX_DESTINATIONS {
        return Err(ApiError::bad_request(
            "too_many_destinations",
            format!(
                "at most {} forwarding destinations are supported",
                crate::forward_store::MAX_DESTINATIONS
            ),
        ));
    }
    let id = admin.forward.create(&input).await.map_err(|e| {
        write_error(
            &e,
            "forwarding destination create",
            "failed to create the forwarding destination",
        )
    })?;
    // Wake the dispatcher so the destination starts sending now rather than at the next refresh.
    admin.forward_handle.poke();
    Ok((StatusCode::CREATED, Json(CreatedId { id })))
}

#[utoipa::path(
    put, path = "/api/v1/forwarding/destinations/{id}", tag = "forwarding",
    params(("id" = Uuid, Path, description = "Destination id")),
    request_body = ForwardDestinationBody,
    responses(
        (status = 204, description = "Destination updated; an omitted secret keeps the stored one"),
        (status = 400, description = "Edge validation rejected the destination (target, kind pairing, filter, certificate, credential, or rate limit)", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageSystem", body = super::error::ErrorBody),
        (status = 404, description = "No such destination", body = super::error::ErrorBody),
        (status = 409, description = "A destination with that name already exists", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode: no destination store", body = super::error::ErrorBody),
    ),
)]
async fn update_forward_destination(
    _guard: RequireManageSystem,
    State(st): State<ApiState>,
    admin: Admin,
    Path(id): Path<Uuid>,
    Json(body): Json<ForwardDestinationBody>,
) -> ApiResult<StatusCode> {
    let input = validate_forward_body(&st, body)?;
    let updated = admin.forward.update(id, &input).await.map_err(|e| {
        write_error(
            &e,
            "forwarding destination update",
            "failed to update the forwarding destination",
        )
    })?;
    if !updated {
        return Err(ApiError::not_found(
            "not_found",
            "no such forwarding destination",
        ));
    }
    admin.forward_handle.poke();
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    delete, path = "/api/v1/forwarding/destinations/{id}", tag = "forwarding",
    params(("id" = Uuid, Path, description = "Destination id")),
    responses(
        (status = 204, description = "Destination deleted, or was already gone"),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageSystem", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode: no destination store", body = super::error::ErrorBody),
    ),
)]
async fn delete_forward_destination(
    _guard: RequireManageSystem,
    admin: Admin,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    // Idempotent: a missing id is a no-op 204. Removing a destination is what an operator reaches
    // for when it is misbehaving, so "it was already gone" is success.
    admin.forward.delete(id).await.map_err(|e| {
        ApiError::from_internal(
            e.as_ref(),
            "forwarding destination delete",
            "failed to delete the forwarding destination",
        )
    })?;
    admin.forward_handle.poke();
    Ok(StatusCode::NO_CONTENT)
}

/// Send one synthetic message to a destination and report what happened.
///
/// Deliberately independent of the running senders, so a still-disabled destination can be
/// validated before it is switched on — and so the caller gets the transport error itself rather
/// than "check the logs".
#[utoipa::path(
    post, path = "/api/v1/forwarding/destinations/{id}/test", tag = "forwarding",
    params(("id" = Uuid, Path, description = "Destination id")),
    responses(
        (status = 200, description = "The probe ran; `delivered` says whether it arrived", body = TestResult),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageSystem", body = super::error::ErrorBody),
        (status = 404, description = "No such destination", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode: no destination store", body = super::error::ErrorBody),
    ),
)]
async fn test_forward_destination(
    _guard: RequireManageSystem,
    admin: Admin,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<TestResult>> {
    // `get_open` ignores `enabled` and decrypts the secret: validating a destination *before*
    // turning it on is the point of the button, and testing with the wrong community is worthless.
    let open = admin
        .forward
        .get_open(id)
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "forwarding destination load",
                "failed to load the forwarding destination",
            )
        })?
        .ok_or_else(|| ApiError::not_found("not_found", "no such forwarding destination"))?;
    Ok(Json(match crate::forward::send_test(&open).await {
        Ok(()) => TestResult {
            delivered: true,
            error: None,
        },
        Err(e) => TestResult {
            delivered: false,
            error: Some(e),
        },
    }))
}

/// The `GET /api/v1/forwarding/status` body.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct ForwardingStatus {
    /// Runtime counters, one entry per running destination.
    destinations: Vec<crate::forward::DestStatus>,
    /// Online pollers that cannot attach the original bytes, so their traffic degrades to rendered.
    pollers_without_raw_capture: Vec<String>,
    /// Online pollers that relay no flow datagrams at all, so a flow destination fed only by these
    /// receives nothing.
    pollers_without_flow_relay: Vec<String>,
    /// False on an HA standby, whose dispatcher is not running (destination CRUD still works).
    sending: bool,
}

/// Live forwarding status: per-destination counters plus any online poller that cannot supply the
/// original bytes, so a byte-exact destination silently degrading is visible in the UI.
#[utoipa::path(
    get, path = "/api/v1/forwarding/status", tag = "forwarding",
    responses(
        (status = 200, description = "Per-destination counters and the pollers that cannot feed them", body = ForwardingStatus),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageSystem", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode: no forwarder", body = super::error::ErrorBody),
    ),
)]
async fn forwarding_status(
    _guard: RequireManageSystem,
    State(st): State<ApiState>,
    admin: Admin,
) -> ApiResult<Json<ForwardingStatus>> {
    Ok(Json(forwarding_delivery_status(&st, &admin)))
}

/// Live forwarding delivery status, shared by `GET /api/v1/forwarding/status` and the MCP
/// `get_system_health(section="forwarding")` tool (ADR-042 I3a).
///
/// `ManageSystem`, not `View` — unlike the rest of the self-health family. It names every
/// destination a deployment tees to, which is closer to the forwarding *configuration* than to a
/// health counter, and the ledger records that asymmetry rather than smoothing it over.
pub(crate) fn forwarding_delivery_status(
    st: &ApiState,
    admin: &super::AdminState,
) -> ForwardingStatus {
    let destinations = admin.forward_handle.status();
    let now = std::time::Instant::now();
    let pollers_without_raw_capture = admin
        .coordinator
        .pollers_missing_cap(yagra_bus::CAP_RAW_CAPTURE, now);
    // Separate from raw capture on purpose: an Increment-1 poller attaches raw bytes to events but
    // relays no flow datagrams at all, so a flow destination fed only by such pollers receives
    // *nothing* — a different failure from "byte-exact silently degraded to rendered".
    let pollers_without_flow_relay = admin
        .coordinator
        .pollers_missing_cap(yagra_bus::CAP_FLOW_RELAY, now);
    ForwardingStatus {
        destinations,
        pollers_without_raw_capture,
        pollers_without_flow_relay,
        sending: st.is_leader.load(std::sync::atomic::Ordering::Acquire),
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
            ("GET", "/api/v1/forwarding/destinations".to_owned()),
            ("POST", "/api/v1/forwarding/destinations".to_owned()),
            ("PUT", format!("/api/v1/forwarding/destinations/{ID}")),
            ("DELETE", format!("/api/v1/forwarding/destinations/{ID}")),
            ("POST", format!("/api/v1/forwarding/destinations/{ID}/test")),
            ("GET", "/api/v1/forwarding/status".to_owned()),
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
    async fn defining_an_egress_path_is_admin_only() {
        // Every route, reads included: a destination row names where received data — which routinely
        // contains credentials — is being sent.
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
    fn host_port_split_handles_ipv6_literals_and_rejects_ambiguity() {
        assert_eq!(
            split_host_port("siem.example.com:514"),
            Some(("siem.example.com".to_owned(), 514))
        );
        assert_eq!(
            split_host_port("10.0.0.5:1514"),
            Some(("10.0.0.5".to_owned(), 1514))
        );
        // A literal v6 address needs brackets; without them its colons are ambiguous, so it is
        // rejected rather than silently parsed as `2001:db8::1` port `1`.
        assert_eq!(
            split_host_port("[2001:db8::1]:514"),
            Some(("2001:db8::1".to_owned(), 514))
        );
        for bad in [
            "2001:db8::1:514",
            "siem.example.com",
            ":514",
            "host:0",
            "host:70000",
            "host:syslog",
            "",
        ] {
            assert_eq!(split_host_port(bad), None, "{bad} should not parse");
        }
    }

    #[test]
    fn local_target_detection_covers_the_loop_guard_cases() {
        for local in [
            "localhost:514",
            "LOCALHOST:514",
            "127.0.0.1:514",
            "[::1]:162",
        ] {
            assert!(target_is_local(local), "{local}");
        }
        // 0.0.0.0 is "this host" too — a destination pointing there loops just as readily.
        assert!(target_is_local("0.0.0.0:514"));
        for remote in ["10.0.0.5:514", "siem.example.com:514", "not-a-target"] {
            assert!(!target_is_local(remote), "{remote}");
        }
    }
    // ── An accepted write (ADR-115) ──────────────────────────────────────────────────

    /// A forwarding destination is created and stored.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn creating_a_forward_destination_stores_it(pool: sqlx::PgPool) {
        use crate::api::tests_support::{live_state, send, token};
        let st = live_state(pool.clone()).await;
        let tok = token(&st, yagra_common::Role::Admin);
        let (status, body) = send(
            &st,
            "POST",
            "/api/v1/forwarding/destinations",
            &tok,
            Some(serde_json::json!({
                "name": "the siem",
                "source_kind": "syslog",
                "dest_kind": "syslog_udp",
                "target": "10.0.0.9:514",
            })),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::CREATED, "{body}");
        assert_eq!(crate::pgtest::rows(&pool, "forward_destinations").await, 1);
    }
}
