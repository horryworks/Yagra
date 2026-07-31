// SPDX-License-Identifier: AGPL-3.0-only
//! The endpoints that describe Yagra itself: build version, client bootstrap config, the global
//! polling default, and self-health.
//!
//! **Three of these are deliberately unauthenticated**, which is unusual enough to state:
//! `/version` and `/config` carry no secrets and the WebUI needs them *before* it can decide
//! whether to show a login screen at all. Gating them would mean the login page could not render.
//! The one write here — the global poll interval — is `ManageConfig` like any other config change.
//!
//! `/system-health` degrades to a `"degraded"` body rather than answering 503 in skeleton mode. It
//! is the page an operator opens when something is wrong, so it must always render; "PostgreSQL not
//! configured" is an answer, and a 503 is not.

use super::error::{ApiError, ApiResult};
use super::extract::{Admin, RequireManageConfig, RequireView};
use super::ApiState;
use axum::{extract::State, http::StatusCode, routing::get, Json, Router};
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

/// The self-description routes, merged into `/api/v1` by [`super::router`].
pub(super) fn routes() -> Router<ApiState> {
    Router::new()
        .route("/api/v1/version", get(version))
        .route("/api/v1/config", get(get_config).put(update_config))
        .route("/api/v1/system-health", get(system_health))
}

/// Build version for Settings ▸ About.
#[derive(Debug, Serialize)]
pub(crate) struct VersionInfo {
    core: &'static str,
}

/// The running `yagra-core` crate version, which inherits the workspace version (the canonical
/// source of truth). The WebUI shows its own build version alongside this, so a core/web skew
/// during a rolling upgrade is visible at a glance.
async fn version() -> Json<VersionInfo> {
    Json(VersionInfo {
        core: env!("CARGO_PKG_VERSION"),
    })
}

/// Client bootstrap config — no secrets.
#[derive(Debug, Serialize)]
pub(crate) struct ClientConfig {
    /// Whether read endpoints are open to anonymous callers.
    public_dashboard: bool,
    /// Whether interactive login exists at all (false in skeleton mode).
    auth_available: bool,
    sso_enabled: bool,
    /// Whether an LLM provider is configured *and* enabled.
    rca_enabled: bool,
    default_poll_interval_secs: u32,
}

/// Tells the WebUI whether reads are open and whether login is available, so it can decide up front
/// whether to gate behind a login screen. Intentionally unauthenticated — see the module doc.
async fn get_config(State(st): State<ApiState>) -> Json<ClientConfig> {
    let default_poll_interval_secs = match st.admin.as_ref() {
        Some(admin) => admin
            .repo
            .get_default_poll_interval()
            .await
            .unwrap_or(crate::config::DEFAULT_POLL_INTERVAL_SECS),
        None => crate::config::DEFAULT_POLL_INTERVAL_SECS,
    };
    let sso_enabled = match st.oidc.as_ref() {
        Some(oidc) => oidc.sso_enabled().await.unwrap_or(false),
        None => false,
    };
    // The WebUI hides the "Explain this incident" action when this is false, so an operator is
    // never offered a button that can only answer 503 — the default state of every installation.
    let rca_enabled = match st.rca.as_ref() {
        Some(rca) => rca.available().await,
        None => false,
    };
    Json(ClientConfig {
        public_dashboard: st.public_dashboard,
        auth_available: st.admin.is_some(),
        sso_enabled,
        rca_enabled,
        default_poll_interval_secs,
    })
}

#[derive(Deserialize)]
struct ConfigBody {
    default_poll_interval_secs: u32,
}

/// Update the global default polling interval. The scheduler re-reads it each round, so a change
/// applies on the next poll round without a restart.
async fn update_config(
    _guard: RequireManageConfig,
    admin: Admin,
    Json(body): Json<ConfigBody>,
) -> ApiResult<StatusCode> {
    let secs = body.default_poll_interval_secs;
    // The floor is what stops one edit from turning the whole fleet into a load generator.
    if !crate::config::interval_in_bounds(secs) {
        return Err(ApiError::bad_request(
            "invalid_poll_interval",
            format!(
                "default poll interval must be {}-{} seconds",
                crate::config::MIN_POLL_INTERVAL_SECS,
                crate::config::MAX_POLL_INTERVAL_SECS
            ),
        ));
    }
    admin
        .repo
        .set_default_poll_interval(secs)
        .await
        .map_err(|e| {
            ApiError::from_internal(e.as_ref(), "update config", "failed to update config")
        })?;
    Ok(StatusCode::NO_CONTENT)
}

/// Reachability of one backing dependency. Carries only a boolean and a human label — no connection
/// strings, no secrets.
#[derive(Debug, Serialize)]
pub(crate) struct DependencyHealth {
    reachable: bool,
    detail: String,
}

/// Yagra's own health: the reachability of its backing services.
#[derive(Debug, Serialize)]
pub(crate) struct SystemHealth {
    /// `"ok"` when every dependency is reachable, else `"degraded"`.
    overall: String,
    /// PostgreSQL (metadata store) — `SELECT 1`.
    postgres: DependencyHealth,
    /// VictoriaMetrics (TSDB) — `/-/healthy`.
    tsdb: DependencyHealth,
    /// VictoriaLogs (event log, ADR-024) — reported reachable when not configured (events then live
    /// in PostgreSQL), so an unconfigured log store never degrades `overall`.
    logs: DependencyHealth,
    /// ClickHouse (flow store, ADR-031) — likewise reachable when not configured.
    flow: DependencyHealth,
    /// NATS — **inferred** from a recent scheduler sweep, not a direct ping.
    bus: DependencyHealth,
}

/// Whether the last scheduler sweep is recent enough to treat the bus as healthy.
///
/// The bus is the one dependency with no handle in `ApiState`, so it is inferred rather than
/// pinged: the poll loop records a sweep every round (cadence = the smallest poll interval in play,
/// ≤ the default), so a sweep within `2 × default + 60s` means jobs are publishing and results
/// consuming over NATS. A stale — or never-recorded — sweep means that path is stalled.
fn bus_sweep_is_fresh(
    last_sweep_unix_ms: Option<i64>,
    default_interval_secs: u32,
    now_ms: i64,
) -> bool {
    match last_sweep_unix_ms {
        Some(ms) => {
            let window_ms = (i64::from(default_interval_secs) * 2 + 60) * 1000;
            now_ms.saturating_sub(ms) <= window_ms
        }
        None => false,
    }
}

/// Reachability of the backing services plus the indirect bus signal.
///
/// Takes `State` rather than the `Admin` extractor on purpose: in skeleton mode this answers a
/// `"degraded"` body naming what is missing, not a 503. This is the page you open when something is
/// already broken.
async fn system_health(
    _guard: RequireView,
    State(st): State<ApiState>,
) -> ApiResult<Json<SystemHealth>> {
    let tsdb = DependencyHealth {
        reachable: st.store.healthy().await,
        detail: "VictoriaMetrics (TSDB)".to_owned(),
    };
    let logs = match st.logs.as_ref() {
        Some(log_store) => DependencyHealth {
            reachable: log_store.healthy().await,
            detail: "VictoriaLogs (event log)".to_owned(),
        },
        None => DependencyHealth {
            reachable: true,
            detail: "VictoriaLogs not configured (events in PostgreSQL)".to_owned(),
        },
    };
    let flow = match st.flows.as_ref() {
        Some(flow_store) => DependencyHealth {
            reachable: flow_store.healthy().await,
            detail: "ClickHouse (flow store)".to_owned(),
        },
        None => DependencyHealth {
            reachable: true,
            detail: "ClickHouse not configured (flow monitoring off)".to_owned(),
        },
    };
    // PostgreSQL and the bus signal both depend on the live write side. In skeleton mode there is
    // no DB and no poll loop, so both are unreachable — a fact, not an error.
    let (postgres, bus) = match st.admin.as_ref() {
        Some(admin) => {
            let postgres = DependencyHealth {
                reachable: admin.repo.healthy().await,
                detail: "PostgreSQL (metadata)".to_owned(),
            };
            let default_secs = admin
                .repo
                .get_default_poll_interval()
                .await
                .unwrap_or(crate::config::DEFAULT_POLL_INTERVAL_SECS);
            let now_ms = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
                .unwrap_or(0);
            let fresh = bus_sweep_is_fresh(
                admin.scheduler_stats.snapshot().last_sweep_unix_ms,
                default_secs,
                now_ms,
            );
            let bus = DependencyHealth {
                reachable: fresh,
                detail: "NATS bus (inferred from poll-loop activity)".to_owned(),
            };
            (postgres, bus)
        }
        None => (
            DependencyHealth {
                reachable: false,
                detail: "PostgreSQL not configured (skeleton mode)".to_owned(),
            },
            DependencyHealth {
                reachable: false,
                detail: "poll loop not running (skeleton mode)".to_owned(),
            },
        ),
    };

    let overall = if postgres.reachable
        && tsdb.reachable
        && logs.reachable
        && flow.reachable
        && bus.reachable
    {
        "ok"
    } else {
        "degraded"
    };
    Ok(Json(SystemHealth {
        overall: overall.to_owned(),
        postgres,
        tsdb,
        logs,
        flow,
        bus,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::router;
    use crate::api::tests_support::{private_state, public_state};
    use axum::body::{to_bytes, Body};
    use axum::http::{header::AUTHORIZATION, Request};
    use tower::ServiceExt;
    use uuid::Uuid;
    use yagra_common::{Principal, Role, Scope};

    async fn send(
        st: ApiState,
        method: &str,
        path: &str,
        token: Option<&str>,
    ) -> axum::response::Response {
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
    }

    #[tokio::test]
    async fn version_and_config_answer_an_anonymous_caller_on_a_private_deployment() {
        // Deliberate: the WebUI reads these *before* it knows whether to show a login screen, so
        // gating them would mean the login page could not render.
        for path in ["/api/v1/version", "/api/v1/config"] {
            assert_eq!(
                send(private_state(), "GET", path, None).await.status(),
                StatusCode::OK,
                "{path}"
            );
        }
    }

    #[tokio::test]
    async fn changing_the_global_interval_is_admin_only() {
        assert_eq!(
            send(private_state(), "PUT", "/api/v1/config", None)
                .await
                .status(),
            StatusCode::UNAUTHORIZED,
        );
        // Read is open, write is not — even on a public dashboard.
        assert_eq!(
            send(public_state(), "PUT", "/api/v1/config", None)
                .await
                .status(),
            StatusCode::UNAUTHORIZED,
        );
        let st = private_state();
        for role in [Role::Viewer, Role::Operator] {
            let token = st
                .sessions
                .issue(Uuid::new_v4(), Principal::new(role, Scope::All), "u");
            assert_eq!(
                send(st.clone(), "PUT", "/api/v1/config", Some(&token))
                    .await
                    .status(),
                StatusCode::FORBIDDEN,
                "{role:?}"
            );
        }
    }

    #[tokio::test]
    async fn system_health_renders_in_skeleton_mode_rather_than_503() {
        // This is the page you open when something is already wrong. "PostgreSQL not configured"
        // is an answer; a 503 is not.
        let resp = send(public_state(), "GET", "/api/v1/system-health", None).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["overall"], "degraded");
        assert_eq!(json["postgres"]["reachable"], false);
        // An unconfigured log/flow store is reported reachable, so it never degrades `overall` on
        // its own — otherwise every default installation would read as unhealthy.
        assert_eq!(json["logs"]["reachable"], true);
        assert_eq!(json["flow"]["reachable"], true);
    }

    #[test]
    fn bus_freshness_window_tracks_the_default_interval() {
        // Default 30s ⇒ window = 30*2 + 60 = 120s. A sweep 100s ago is fresh; 200s ago is stale.
        let now = 1_000_000i64;
        assert!(bus_sweep_is_fresh(Some(now - 100_000), 30, now));
        assert!(!bus_sweep_is_fresh(Some(now - 200_000), 30, now));
        // Never-swept ⇒ never fresh, regardless of interval. A poll loop that has not run once is
        // not "recently healthy".
        assert!(!bus_sweep_is_fresh(None, 30, now));
    }
}
