// SPDX-License-Identifier: AGPL-3.0-only
//! The core⇄poller bus, as an operator sees it (ADR-065) — Settings ▸ Pollers.
//!
//! Three endpoints: read the certificate remote sites must pin, mint a replacement covering a new
//! site's address, and turn acceptance of remote-site pollers on or off.
//!
//! ## Why this is a screen at all
//!
//! Job messages carry **plaintext device credentials** (ADR-020), so a bus that leaves the host has
//! to be TLS-encrypted and authenticated before it does. The shipped procedure for that was an
//! `openssl` invocation, a hand edit of two blocks in `docker-compose.deploy.yml`, and one shared
//! password copied to every site — and the hand edits are **erased by the next upgrade** (ADR-050
//! decision 5 reinstalls the composition from the target image), after which the central stack keeps
//! working and only the remote sites go silent. Moving the whole thing behind these endpoints is
//! what removes the class of failure, not just the typing.
//!
//! ## All three take `RequireManageSystem`
//!
//! This is deployment infrastructure, the same class as TLS and upgrades — and the switch reaches
//! the updater sidecar, which holds the Docker socket. An Admin is unscoped by construction, so
//! there is nothing for group scoping to narrow. Authorization runs **before** availability
//! (`Require*` first, then the extractors) so an unauthenticated prober cannot learn from the
//! difference between 401 and 503 whether this deployment has a bus certificate store.
//!
//! ## The response carries the certificate and never the key
//!
//! Same asymmetry as `api/webtls.rs`, and here it is the point rather than a nicety: the certificate
//! **is** what a remote site needs as its `YAGRA_BUS_CA_FILE`, so it has to come back and be
//! copyable. The private key has no such use and never leaves the server.

use axum::{
    routing::{get, post, put},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use utoipa::OpenApi;

use super::error::{ApiError, ApiResult};
use super::extract::{Admin, BusTls, Caller, RequireManageSystem, Upgrade};
use super::ApiState;
use crate::bus_cert::BusTlsView;
use crate::server_cert::CertError;
use crate::upgrade::{Command, MAINTENANCE_WINDOW_SECS};

#[derive(OpenApi)]
#[openapi(paths(get_bus, regenerate_bus_cert, set_bus_remote))]
pub(super) struct Doc;

pub(super) fn routes() -> Router<ApiState> {
    Router::new()
        .route("/api/v1/settings/bus", get(get_bus))
        .route(
            "/api/v1/settings/bus/certificate",
            post(regenerate_bus_cert),
        )
        .route("/api/v1/settings/bus/remote", put(set_bus_remote))
}

/// How many characters the generated bus passwords carry.
///
/// 32 characters of `[A-Za-z0-9]` is ~190 bits. They are typed by nobody — the poller's arrives in a
/// generated `.env` — so there is no length worth trading entropy for, and the ceiling is the
/// updater request file's own 128-byte field cap.
const BUS_SECRET_LEN: usize = 32;

/// The bus, as Settings ▸ Pollers shows it.
//
// Every `///` in this file is published verbatim to API clients and into the generated site
// reference, so rationale goes in `//` notes like this one.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct BusResponse {
    /// The certificate the bus serves, or `null` if none has been established yet.
    // `null` rather than a 404: a deployment whose bus-cert one-shot has not run has none, and the
    // card should render its waiting state rather than an error.
    certificate: Option<BusTlsView>,
    /// Whether this core is talking to the bus over TLS, which is what accepting remote-site
    /// pollers requires.
    // Derived from core's own connection rather than from a stored setting, deliberately: it reports
    // what is *running*. A stored flag would keep saying "on" after an upgrade reverted the
    // composition, which is precisely the failure this feature exists to remove.
    remote_enabled: bool,
    /// Whether the switch below can be operated. `false` means the updater sidecar that performs it
    /// is not deployed, is switched off, or has stopped reporting — the certificate is still
    /// readable, but turning remote acceptance on or off needs a shell on the host.
    can_switch: bool,
}

/// Which names a regenerated bus certificate should cover.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub(crate) struct BusCertRegenerate {
    /// Hostnames and IP addresses remote pollers will dial, added to the deployment's internal
    /// defaults. A poller's connection fails unless the exact address it dials is present, so this
    /// is the field that decides whether a new site can connect.
    #[serde(default)]
    names: Vec<String>,
}

/// Turn acceptance of remote-site pollers on or off.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub(crate) struct BusRemoteRequest {
    /// `true` encrypts and authenticates the bus and publishes its port; `false` returns it to the
    /// internal-only plaintext bus and stops every remote poller from connecting.
    enabled: bool,
    /// Hostnames and IP addresses remote pollers will dial. Used to reissue the certificate before
    /// the bus restarts, so the names are already right when it comes back. Ignored when disabling.
    #[serde(default)]
    names: Vec<String>,
}

/// What the switch returns. The poller secret appears **once**.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct BusRemoteAccepted {
    /// Identifier for this change, matching the run reported by the upgrade status endpoint.
    id: String,
    /// The fleet-wide maintenance window opened for the duration, or `null` if one could not be
    /// opened. Monitoring stops for as long as the bus is being recreated.
    maintenance_window_id: Option<String>,
    /// The bootstrap secret a remote poller presents when it connects, shown **once**. Store it
    /// now — it is written into the deployment's environment and is never returned again.
    // Deliberately the same shape as a personal access token: generated here, displayed once, never
    // readable afterwards. The alternative — an endpoint that returns it on demand — would be a
    // secret any Admin session could re-read at any time, which is worse than the copy the operator
    // makes deliberately. `null` when disabling, where there is nothing to hand out.
    poller_secret: Option<String>,
    /// The certificate a remote poller must pin as its `YAGRA_BUS_CA_FILE`, in PEM. `null` when
    /// disabling.
    ca_certificate: Option<String>,
}

/// The bus certificate and whether this deployment accepts remote-site pollers.
#[utoipa::path(
    get, path = "/api/v1/settings/bus", tag = "pollers",
    responses(
        (status = 200, description = "The bus certificate and the remote-acceptance state", body = BusResponse),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageSystem", body = super::error::ErrorBody),
        (status = 503, description = "This deployment has no bus certificate store", body = super::error::ErrorBody),
    ),
)]
async fn get_bus(
    _guard: RequireManageSystem,
    bus: BusTls,
    upgrade: Option<Upgrade>,
) -> ApiResult<Json<BusResponse>> {
    let certificate = bus.view().await.map_err(|e| {
        ApiError::from_internal(
            e.as_ref(),
            "read the bus certificate",
            "failed to read the bus certificate",
        )
    })?;
    Ok(Json(BusResponse {
        certificate,
        remote_enabled: remote_enabled(),
        can_switch: switch_ready(upgrade.as_deref()).await,
    }))
}

/// Mint a new bus certificate covering the given names.
#[utoipa::path(
    post, path = "/api/v1/settings/bus/certificate", tag = "pollers",
    request_body = BusCertRegenerate,
    responses(
        (status = 200, description = "Generated and written; the body is the new certificate's details", body = BusResponse),
        (status = 400, description = "A supplied name is not a usable hostname or IP address", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageSystem", body = super::error::ErrorBody),
        (status = 503, description = "This deployment has no bus certificate store", body = super::error::ErrorBody),
    ),
)]
async fn regenerate_bus_cert(
    _guard: RequireManageSystem,
    bus: BusTls,
    upgrade: Option<Upgrade>,
    caller: Option<Caller>,
    Json(body): Json<BusCertRegenerate>,
) -> ApiResult<Json<BusResponse>> {
    let names = merge_names(&body.names);
    let by = caller.map(|c| c.0.user_id);
    let certificate = bus.regenerate(&names, by).await.map_err(to_api_error)?;
    // No restart here, and that is honest rather than lazy: the file on the volume is current
    // immediately, but `nats-server` reads its certificate at startup, so the *bus* keeps serving
    // the previous one until it is recreated. The card says so; the switch is what recreates it.
    tracing::warn!(
        fingerprint = %certificate.fingerprint_sha256,
        sans = ?certificate.sans,
        "the bus certificate was reissued — every remote poller must be given the new one, and the \
         bus serves it only after it is restarted"
    );
    Ok(Json(BusResponse {
        certificate: Some(certificate),
        remote_enabled: remote_enabled(),
        can_switch: switch_ready(upgrade.as_deref()).await,
    }))
}

/// Turn acceptance of remote-site pollers on or off.
///
/// The bus, this core and the co-located poller are all recreated, so monitoring stops for as long
/// as that takes. A fleet-wide maintenance window is opened first.
#[utoipa::path(
    put, path = "/api/v1/settings/bus/remote", tag = "pollers",
    request_body = BusRemoteRequest,
    responses(
        (status = 202, description = "Accepted; the bus is being recreated and this core will restart", body = BusRemoteAccepted),
        (status = 400, description = "Enabling without an address remote pollers can dial", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageSystem", body = super::error::ErrorBody),
        (status = 409, description = "An upgrade or another bus change is already running", body = super::error::ErrorBody),
        (status = 503, description = "No updater is deployed, or it is switched off", body = super::error::ErrorBody),
    ),
)]
async fn set_bus_remote(
    _guard: RequireManageSystem,
    bus: BusTls,
    upgrade: Upgrade,
    admin: Admin,
    caller: Option<Caller>,
    Json(body): Json<BusRemoteRequest>,
) -> ApiResult<(axum::http::StatusCode, Json<BusRemoteAccepted>)> {
    // The updater's own four refusals, verbatim from the upgrade path — including the 409 that
    // stops a second click from recreating the stack underneath the first. Shared rather than
    // re-derived: a second copy is how one path ends up missing one of them.
    super::upgrade::reachable(&upgrade, chrono::Utc::now().timestamp()).await?;

    let by = caller
        .as_ref()
        .map_or_else(|| "unknown".to_owned(), |c| c.0.username.clone());
    let id = uuid::Uuid::new_v4().to_string();

    // Reissue BEFORE the bus restarts, so the certificate it comes back serving already covers the
    // site. Doing it afterwards would need a second restart, and the operator would have watched
    // monitoring stop twice for one change.
    let (poller_secret, ca_certificate, extra) = if body.enabled {
        let names = merge_names(&body.names);
        if body.names.iter().all(|n| n.trim().is_empty()) {
            return Err(ApiError::bad_request(
                "missing_address",
                "give the hostname or IP address remote pollers will dial — their connection fails \
                 unless it is in the certificate",
            ));
        }
        let by_id = caller.as_ref().map(|c| c.0.user_id);
        let cert = bus.regenerate(&names, by_id).await.map_err(to_api_error)?;
        let core_secret = random_secret();
        let poller_secret = random_secret();
        (
            Some(poller_secret.clone()),
            Some(cert.certificate),
            vec![
                ("bus_mode".to_owned(), "on".to_owned()),
                ("bus_core_password".to_owned(), core_secret),
                ("bus_poller_password".to_owned(), poller_secret),
            ],
        )
    } else {
        (None, None, vec![("bus_mode".to_owned(), "off".to_owned())])
    };

    // Failure is not fatal: a change that runs noisily is better than one that does not run.
    let ends = chrono::Utc::now() + chrono::Duration::seconds(MAINTENANCE_WINDOW_SECS);
    let window = admin
        .maintenance
        .create_window(
            if body.enabled {
                "Enabling remote pollers"
            } else {
                "Disabling remote pollers"
            },
            crate::maintenance::WindowScope::System.as_str(),
            crate::maintenance::UPGRADE_SCOPE_ID,
            chrono::Utc::now(),
            ends,
        )
        .await
        .map_err(|e| tracing::warn!(error = %e, "could not open the bus-change maintenance window"))
        .ok();

    let borrowed: Vec<(&str, &str)> = extra
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    upgrade
        .request_with(
            Command::Bus,
            &id,
            None,
            &by,
            chrono::Utc::now().timestamp(),
            &borrowed,
        )
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "hand the bus change to the updater",
                "failed to hand the bus change to the updater",
            )
        })?;
    tracing::warn!(
        run = %id, enabled = body.enabled, by = %by,
        "remote-poller acceptance change requested; the bus and core will restart"
    );

    Ok((
        axum::http::StatusCode::ACCEPTED,
        Json(BusRemoteAccepted {
            id,
            maintenance_window_id: window.map(|w| w.to_string()),
            poller_secret,
            ca_certificate,
        }),
    ))
}

/// Can the switch be operated right now?
///
/// The same question [`set_bus_remote`] refuses on, asked without a reason string — the card draws
/// the control from this, and refusing with a message is the `PUT`'s job. `Option` because a
/// deployment with no updater can still read its certificate and hand it to a site that was
/// configured from the command line; only the switch needs the sidecar.
async fn switch_ready(upgrade: Option<&crate::upgrade::UpgradeRepo>) -> bool {
    match upgrade {
        Some(u) => super::upgrade::reachable(u, chrono::Utc::now().timestamp())
            .await
            .is_ok(),
        None => false,
    }
}

/// Is this core's own bus connection encrypted?
///
/// Reads `YAGRA_BUS_URL`, which the composition sets per service. Reporting the running state rather
/// than a stored intent is the whole point — see [`BusResponse::remote_enabled`].
fn remote_enabled() -> bool {
    std::env::var("YAGRA_BUS_URL")
        .unwrap_or_default()
        .trim()
        .starts_with("tls://")
}

/// Operator-supplied names plus the internal defaults, deduplicated.
///
/// The defaults are never dropped: without `nats` in the SAN list the co-located core and poller
/// stop being able to reach their own bus the moment TLS comes on, which reads as "the switch broke
/// everything" rather than "one name is missing".
fn merge_names(supplied: &[String]) -> Vec<String> {
    let mut names = crate::bus_cert::default_names();
    for n in supplied.iter().map(|n| n.trim()).filter(|n| !n.is_empty()) {
        if !names.iter().any(|existing| existing == n) {
            names.push(n.to_owned());
        }
    }
    names
}

/// A bus password: `[A-Za-z0-9]` only.
///
/// The charset is not aesthetic. The value travels through the updater's `key=value` request file
/// and is then written into a `.env` the shell reads, so anything a shell or a parser could treat
/// specially is one place for the two to disagree. Alphanumerics cannot.
fn random_secret() -> String {
    use rand::Rng;
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let mut rng = rand::thread_rng();
    (0..BUS_SECRET_LEN)
        .map(|_| ALPHABET[rng.gen_range(0..ALPHABET.len())] as char)
        .collect()
}

/// Map a store failure to a status.
///
/// Everything [`CertError`] describes is a name the operator typed, so it is a 400 carrying the
/// module's own text. Anything else is the database, and says nothing about itself (security.md).
fn to_api_error(e: anyhow::Error) -> ApiError {
    match e.downcast_ref::<CertError>() {
        Some(cert) => ApiError::bad_request("invalid_certificate", cert.to_string()),
        None => ApiError::from_internal(
            e.as_ref(),
            "store the bus certificate",
            "failed to store the bus certificate",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests_support::{private_state, public_state};
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;
    use yagra_common::{Principal, Role, Scope};

    /// Every route this module serves, with a body that would be valid if the caller got that far.
    ///
    /// Listed rather than derived, for the same reason `api/upgrade.rs` lists its own: the whole
    /// surface is one permission pair, and one of these routes reaches a container holding the
    /// Docker socket.
    const ROUTES: &[(&str, &str, &str)] = &[
        ("GET", "/api/v1/settings/bus", ""),
        (
            "POST",
            "/api/v1/settings/bus/certificate",
            r#"{"names":["yagra.example.net"]}"#,
        ),
        (
            "PUT",
            "/api/v1/settings/bus/remote",
            r#"{"enabled":false,"names":[]}"#,
        ),
    ];

    fn req(method: &str, path: &str, body: &str, token: Option<&str>) -> Request<Body> {
        let mut b = Request::builder()
            .uri(path)
            .method(method)
            .header("content-type", "application/json");
        if let Some(t) = token {
            b = b.header("authorization", format!("Bearer {t}"));
        }
        b.body(Body::from(body.to_owned())).expect("request")
    }

    // Authorization before availability, and closed even on a public dashboard: `public_dashboard`
    // opens reads of monitoring data, never the deployment's own key material or its stack.
    #[tokio::test]
    async fn every_bus_route_is_gated_before_availability() {
        for (method, path, body) in ROUTES {
            for st in [private_state(), public_state()] {
                let app = super::super::router(st);
                let res = app
                    .oneshot(req(method, path, body, None))
                    .await
                    .expect("response");
                assert_eq!(
                    res.status(),
                    StatusCode::UNAUTHORIZED,
                    "{method} {path} answered before authenticating"
                );
            }
        }
    }

    #[tokio::test]
    async fn only_manage_system_may_read_or_change_the_bus() {
        for (method, path, body) in ROUTES {
            for role in [Role::Viewer, Role::Operator] {
                let st = private_state();
                let token =
                    st.sessions
                        .issue(uuid::Uuid::new_v4(), Principal::new(role, Scope::All), "u");
                let app = super::super::router(st);
                let res = app
                    .oneshot(req(method, path, body, Some(&token)))
                    .await
                    .expect("response");
                assert_eq!(
                    res.status(),
                    StatusCode::FORBIDDEN,
                    "{method} {path} for {role:?} must be 403, not 401 — the two have to stay \
                     distinguishable, and an Operator manages monitoring, not the deployment"
                );
            }
        }
    }

    /// The defaults are what the co-located core and poller dial. Losing them to an operator's
    /// entry would take the deployment's own bus down while looking like a successful change.
    #[test]
    fn operator_names_are_added_to_the_internal_defaults_never_substituted() {
        let merged = super::merge_names(&["yagra.example.net".to_owned(), "  ".to_owned()]);
        assert!(merged.iter().any(|n| n == "nats"), "{merged:?}");
        assert!(
            merged.iter().any(|n| n == "yagra.example.net"),
            "{merged:?}"
        );
        assert!(
            !merged.iter().any(|n| n.trim().is_empty()),
            "a blank entry became a SAN: {merged:?}"
        );
        // Re-supplying a default must not duplicate it.
        let again = super::merge_names(&["nats".to_owned()]);
        assert_eq!(again.iter().filter(|n| *n == "nats").count(), 1);
    }

    /// The secret is written into a `key=value` file read as root and then into a `.env` a shell
    /// sources. A character either of them treats specially is one place for the two to disagree.
    #[test]
    fn a_generated_bus_secret_is_alphanumeric_and_long_enough() {
        for _ in 0..64 {
            let s = super::random_secret();
            assert_eq!(s.len(), super::BUS_SECRET_LEN);
            assert!(
                s.chars().all(|c| c.is_ascii_alphanumeric()),
                "generated `{s}`, which the request-file charset would refuse"
            );
        }
        assert_ne!(
            super::random_secret(),
            super::random_secret(),
            "two draws were identical"
        );
    }
}
