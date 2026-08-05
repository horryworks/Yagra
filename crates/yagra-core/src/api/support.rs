// SPDX-License-Identifier: AGPL-3.0-only
//! The support-bundle download (ADR-045) — the one endpoint whose job is to be useful to somebody
//! who cannot reach this deployment at all.
//!
//! The collection logic, the redaction scan and the archive format live in
//! [`crate::support_bundle`]; this module is the seam that reads the running system. It exists as
//! its own domain rather than inside `api/system.rs` because the guard is different, and that
//! difference is the interesting part:
//!
//! # Three permissions, not one
//!
//! The bundle is the union of things three separate surfaces answer — configuration
//! (`ManageConfig`), whether stored credentials still decrypt (`ManageCredentials`), and who did
//! what (`ViewAudit`). So the handler demands **all three**, in its signature, rather than picking
//! the loosest and calling it an admin endpoint.
//!
//! Picking one would have made this a privilege-escalation path: a `ManageConfig` operator could
//! download the audit log and the credential report through a route whose name says neither. The
//! union costs nothing today — only Admin holds all three — and is what keeps that true if a role
//! ever holds one without the others, which is the whole reason those permissions are separate
//! (`api-conventions.md`).
//!
//! # Degrade, never fail
//!
//! Every section is collected independently and a failure records an omission instead of aborting.
//! A bundle is requested *because* something is broken; refusing to produce one because the
//! forwarding subsystem is unreachable would withhold the evidence at the exact moment it is
//! wanted. The one thing that does abort is a redaction hit — see [`crate::support_bundle`].

use super::error::{ApiError, ApiResult};
use super::extract::{Admin, RequireManageConfig, RequireManageCredentials, RequireViewAudit};
use super::ApiState;
use crate::support_bundle::{
    self, BundleBuilder, BundleError, SecretScan, DEFAULT_SINCE_HOURS, MAX_LOG_BYTES,
    MAX_SINCE_HOURS,
};
use axum::{
    extract::{Query, State},
    http::header::{CONTENT_DISPOSITION, CONTENT_TYPE},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use serde::Deserialize;

/// This domain's slice of the OpenAPI document (ADR-035), merged by [`super::openapi::document`].
#[derive(utoipa::OpenApi)]
#[openapi(paths(support_bundle))]
pub(super) struct Doc;

/// The support-bundle route, merged into `/api/v1` by [`super::router`].
pub(super) fn routes() -> Router<ApiState> {
    Router::new().route("/api/v1/system/support-bundle", get(support_bundle))
}

/// How far back to reach for log files.
#[derive(Debug, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub(crate) struct BundleQuery {
    /// Hours of log history to carry. Clamped to `[1, 168]`; the appender's own retention is
    /// usually the tighter bound.
    since_hours: Option<u32>,
}

/// A downloadable archive of this deployment's logs and status.
///
/// Written for a deployment behind an air gap: every entry is text, `MANIFEST.json` lists what is
/// carried **and what is deliberately not**, and a redaction scan over the assembled bytes aborts
/// the export rather than shipping a secret.
#[utoipa::path(
    get, path = "/api/v1/system/support-bundle", tag = "system",
    params(BundleQuery),
    responses(
        (status = 200, description = "A gzipped tar of JSON and text files: build provenance, every system-health section, the environment allow-list, applied migrations, table sizes, active alerts, the audit tail, the Prometheus scrape, and core's own rotated log files. Carries no secrets — see MANIFEST.json's `omitted` and `redaction` sections", content_type = "application/gzip"),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks any of ManageConfig, ManageCredentials or ViewAudit", body = super::error::ErrorBody),
        (status = 500, description = "The redaction scan matched, so nothing was released. The rule and the file are named in the log, never the value", body = super::error::ErrorBody),
        (status = 503, description = "Inventory storage is unavailable (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn support_bundle(
    _cfg: RequireManageConfig,
    _creds: RequireManageCredentials,
    _audit: RequireViewAudit,
    admin: Admin,
    State(st): State<ApiState>,
    Query(q): Query<BundleQuery>,
) -> ApiResult<Response> {
    let hours = q
        .since_hours
        .unwrap_or(DEFAULT_SINCE_HOURS)
        .clamp(1, MAX_SINCE_HOURS);
    let now = chrono::Utc::now();
    let archive = build(&st, &admin, hours, now).await.map_err(to_api_error)?;

    let stamp = now.format("%Y%m%dT%H%M%SZ");
    tracing::info!(
        bytes = archive.len(),
        window_hours = hours,
        "support bundle exported"
    );
    Ok((
        [
            (CONTENT_TYPE, "application/gzip".to_owned()),
            (
                CONTENT_DISPOSITION,
                format!("attachment; filename=\"yagra-support-{stamp}.tar.gz\""),
            ),
        ],
        archive,
    )
        .into_response())
}

/// Collect every section and assemble the archive.
///
/// Split from the handler so the assembly is reachable from a test without a router, and so the
/// "collect, then scan, then write" order is one readable sequence.
async fn build(
    st: &ApiState,
    admin: &super::AdminState,
    hours: u32,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<Vec<u8>, BundleError> {
    let mut b = BundleBuilder::new(hours);

    // ── Which binary is this, really ──────────────────────────────────────────────────────────
    // First, and first in the archive, because every other answer is conditional on it.
    b.add_json(
        "provenance.json",
        "Which binary is running: crate version, image source ref, build profile, uptime",
        &support_bundle::provenance(st.started),
    )?;
    b.add_json(
        "env.json",
        "The allow-listed environment of the core process, with URL credentials stripped",
        &support_bundle::env_snapshot(),
    )?;

    // ── System health, one file per section ───────────────────────────────────────────────────
    // The same seams `GET /api/v1/system-health` and MCP `get_system_health` read, so a bundle and
    // a live query answer the same question the same way.
    b.add_json(
        "health/version.json",
        "The running core's build version",
        &super::health::running_version(),
    )?;
    b.add_json(
        "health/deployment.json",
        "Which optional tiers are enabled (SSO, RCA, flow, public dashboard)",
        &super::health::client_config(st).await,
    )?;
    b.add_json(
        "health/dependencies.json",
        "Per-store reachability: PostgreSQL, VictoriaMetrics, VictoriaLogs, ClickHouse, bus, WebUI TLS",
        &super::health::system_health_snapshot(st).await,
    )?;
    b.add_json(
        "health/pollers.json",
        "The poller fleet: liveness, pools, assignment counts",
        &super::pollers::poller_inventory(admin).await,
    )?;
    b.add_json(
        "health/poller_health.json",
        "Poll-loop counters (sweeps, dispatches, results, drops)",
        &admin.scheduler_stats.snapshot(),
    )?;
    b.add_json(
        "health/pools.json",
        "The pools nodes are assigned to",
        &super::nodes::pool_options(admin).await,
    )?;
    b.add_json(
        "health/monitoring_gaps.json",
        "Recent core-poller outages: data missing from these windows is missing, not flat",
        &super::pollers::monitoring_gaps(admin).await,
    )?;
    b.add_json(
        "health/hosts.json",
        "CPU, load, memory and disk for core and every reporting poller",
        &super::system::host_inventory(st),
    )?;
    b.add_json(
        "health/forwarding.json",
        "Relay delivery status per forwarding destination",
        &super::forwarding::forwarding_delivery_status(st, admin),
    )?;
    match super::credentials::credential_decrypt_health(admin).await {
        Ok(h) => b.add_json(
            "health/credentials.json",
            "Whether stored credentials still decrypt. Counts and ids only — never a secret",
            &h,
        )?,
        Err(e) => {
            tracing::warn!(error = %e.message(), "support bundle: credential health unavailable");
            b.omit(
                "health/credentials.json",
                "the credential store could not be read while this bundle was taken; the cause is \
                 in the bundled core log",
            );
        }
    }

    // ── Configuration ─────────────────────────────────────────────────────────────────────────
    // Reused wholesale rather than re-derived: ADR-040 already decided what a portable, secret-free
    // description of a deployment's configuration looks like, and a second answer here would be a
    // second thing to keep secret-free.
    match admin.config_bundle.export().await {
        Ok(bundle) => b.add_json(
            "config/bundle.json",
            "The deployment's monitoring configuration, in the ADR-040 portable form (no secrets)",
            &bundle,
        )?,
        Err(e) => {
            tracing::warn!(error = %e, "support bundle: configuration export unavailable");
            b.omit(
                "config/bundle.json",
                "the configuration export failed or exceeded its per-table row cap; take it \
                 separately from Settings if it is needed",
            );
        }
    }

    // ── PostgreSQL ────────────────────────────────────────────────────────────────────────────
    match admin.support.migrations().await {
        Ok(rows) => b.add_json(
            "db/migrations.json",
            "Applied migrations with their checksums — the first thing to compare when core will \
             not start",
            &rows,
        )?,
        Err(e) => {
            tracing::warn!(error = %e, "support bundle: migration state unavailable");
            b.omit("db/migrations.json", "PostgreSQL was not readable");
        }
    }
    match admin.support.table_stats().await {
        Ok(rows) => b.add_json(
            "db/tables.json",
            "Per-table estimated row counts and on-disk size (planner estimates, not COUNT(*))",
            &rows,
        )?,
        Err(e) => {
            tracing::warn!(error = %e, "support bundle: table statistics unavailable");
            b.omit("db/tables.json", "PostgreSQL was not readable");
        }
    }
    match admin.support.connection_stats().await {
        Ok(rows) => b.add_json(
            "db/connections.json",
            "Connections to this database grouped by state (no SQL text)",
            &rows,
        )?,
        Err(e) => {
            tracing::warn!(error = %e, "support bundle: connection statistics unavailable");
            b.omit("db/connections.json", "PostgreSQL was not readable");
        }
    }

    // ── What is happening right now ───────────────────────────────────────────────────────────
    b.add_json(
        "alerts/active.json",
        "Every currently-active alert",
        &st.alerts.active_alerts(),
    )?;
    match admin.audit.list(AUDIT_ROWS, None).await {
        Ok(rows) => b.add_json(
            "audit/recent.json",
            "The newest audit entries: who changed or acknowledged what",
            &rows,
        )?,
        Err(e) => {
            tracing::warn!(error = %e, "support bundle: audit tail unavailable");
            b.omit("audit/recent.json", "the audit log was not readable");
        }
    }

    // ── Counters ──────────────────────────────────────────────────────────────────────────────
    // Denser than the logs and cheaper to read: one render of everything `metrics::counter!` has
    // touched since startup.
    match st.metrics.as_ref() {
        Some(handle) => b.add_bytes(
            "metrics/core.prom",
            "The Prometheus scrape of this core process, as /metrics serves it",
            handle.render().into_bytes(),
        ),
        None => b.omit(
            "metrics/core.prom",
            "this core is running without a Prometheus recorder installed",
        ),
    }

    // ── Logs ──────────────────────────────────────────────────────────────────────────────────
    collect_logs(&mut b, hours);

    b.finish(&SecretScan::from_env(), now)
}

/// How many audit rows to carry. The store's own page maximum — enough to cover the change that
/// preceded an incident without turning the bundle into an audit export.
const AUDIT_ROWS: i64 = crate::audit::MAX_LIMIT;

/// Add core's rotated log files, or record why there are none.
///
/// The "not configured" branch is not an error and says so in words an operator can act on: file
/// logging is opt-in, and a bundle from a deployment without it is still worth having — it just
/// cannot answer what happened before the current process started.
fn collect_logs(b: &mut BundleBuilder, hours: u32) {
    let Some(dir) = yagra_telemetry::log_dir() else {
        b.omit(
            "logs/",
            "on-disk logging is not enabled: set YAGRA_LOG_DIR (and mount a volume there) so a \
             future bundle can carry core's own log, including a panic from a run that has already \
             ended. Without it the logs exist only in `docker logs` on the host.",
        );
        return;
    };
    let since = std::time::SystemTime::now()
        .checked_sub(std::time::Duration::from_secs(u64::from(hours) * 3600))
        .unwrap_or(std::time::UNIX_EPOCH);
    let collected = support_bundle::collect_logs(&dir, "yagra-core", since, MAX_LOG_BYTES);
    for (name, bytes) in collected.files {
        b.add_bytes(
            &format!("logs/{name}"),
            "One rotated hour of core's structured log (JSON lines)",
            bytes,
        );
    }
    // Both caps are reported. A log truncated silently reads as "nothing was logged", which is a
    // wrong answer rather than a missing one.
    if collected.dropped_for_size > 0 {
        b.omit(
            format!("{} older log file(s)", collected.dropped_for_size),
            format!(
                "the bundle's {MAX_LOG_BYTES}-byte log cap was reached; the newest hours were kept. \
                 Request a shorter window, or take the older hours in a second bundle."
            ),
        );
    }
    if collected.outside_window > 0 {
        b.omit(
            format!(
                "{} log file(s) older than the window",
                collected.outside_window
            ),
            format!(
                "outside the requested {hours}-hour window; re-request with a larger since_hours"
            ),
        );
    }
}

/// Map assembly failures onto the API envelope.
///
/// The redaction hit is the one worth its own code: it is not a server fault the operator should
/// retry past, it is the export refusing to release something. The message names the file and the
/// rule and **never the matched value** — this text reaches a log and a screen.
fn to_api_error(e: BundleError) -> ApiError {
    match e {
        BundleError::SecretDetected { file, rule } => {
            tracing::error!(
                file = %file,
                rule = %rule,
                "support bundle refused: the redaction scan matched"
            );
            ApiError::internal_with_code(
                "support_bundle_redaction_failed",
                format!(
                    "the redaction scan matched {rule} in {file}, so no bundle was produced. \
                     This is a leak to fix, not a check to bypass."
                ),
            )
        }
        BundleError::Archive(e) => ApiError::from_internal(
            &e,
            "support bundle",
            "failed to assemble the support bundle",
        ),
        BundleError::Serialize(e) => ApiError::from_internal(
            &e,
            "support bundle",
            "failed to assemble the support bundle",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::router;
    use crate::api::tests_support::{private_state, public_state};
    use axum::body::Body;
    use axum::http::{header::AUTHORIZATION, Request, StatusCode};
    use tower::ServiceExt;
    use uuid::Uuid;
    use yagra_common::{Principal, Role, Scope};

    const PATH: &str = "/api/v1/system/support-bundle";

    async fn send(st: ApiState, token: Option<&str>) -> Response {
        let mut b = Request::builder().uri(PATH);
        if let Some(t) = token {
            b = b.header(AUTHORIZATION, format!("Bearer {t}"));
        }
        router(st)
            .oneshot(b.body(Body::empty()).unwrap())
            .await
            .unwrap()
    }

    /// Gated before the store is consulted: an anonymous caller learns only that it is
    /// unauthenticated, never whether this deployment has an inventory (api-conventions).
    #[tokio::test]
    async fn anonymous_is_refused_and_told_only_that() {
        assert_eq!(
            send(private_state(), None).await.status(),
            StatusCode::UNAUTHORIZED
        );
    }

    /// `public_dashboard` opens reads. This is a read, and it must stay shut anyway — a bundle is
    /// the whole configuration plus the audit log, which is not dashboard material.
    #[tokio::test]
    async fn a_public_dashboard_does_not_open_the_bundle() {
        assert_eq!(
            send(public_state(), None).await.status(),
            StatusCode::UNAUTHORIZED
        );
    }

    /// The union guard, which is the design decision this module exists for. Viewer and Operator
    /// are refused with **403** rather than 401 — the two must stay distinguishable — and refused
    /// even though an Operator holds `ManageConfig`, because the bundle also carries the audit log
    /// and the credential report.
    #[tokio::test]
    async fn a_role_short_of_any_one_permission_gets_403() {
        let st = private_state();
        for role in [Role::Viewer, Role::Operator] {
            let token = st
                .sessions
                .issue(Uuid::new_v4(), Principal::new(role, Scope::All), "u");
            assert_eq!(
                send(st.clone(), Some(&token)).await.status(),
                StatusCode::FORBIDDEN,
                "{role:?} must not be able to download a support bundle"
            );
        }
    }

    /// Authorization is proved before availability is reported: an Admin on a skeleton deployment
    /// gets 503 (no write side), not 200 and not 403. Ordering is the security property — an
    /// unauthenticated caller must never learn which subsystems exist.
    #[tokio::test]
    async fn an_admin_on_a_skeleton_deployment_gets_503_not_403() {
        let st = private_state();
        let token = st
            .sessions
            .issue(Uuid::new_v4(), Principal::new(Role::Admin, Scope::All), "a");
        assert_eq!(
            send(st, Some(&token)).await.status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
    }

    /// The redaction refusal carries a code the UI can branch on, and its message names the rule
    /// and the file without ever naming the value — this string reaches a log and a screen.
    #[test]
    fn a_redaction_hit_is_a_typed_refusal_that_discloses_nothing() {
        let err = to_api_error(BundleError::SecretDetected {
            file: "logs/yagra-core.2026-08-06-14.log".to_owned(),
            rule: "a URL carrying user:password@ userinfo",
        });
        assert_eq!(err.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(err.code(), "support_bundle_redaction_failed");
        assert!(err.message().contains("logs/yagra-core.2026-08-06-14.log"));
        assert!(err.message().contains("userinfo"));
    }

    /// An I/O or serialization failure reveals nothing about itself (security.md), unlike the
    /// redaction refusal above — which is deliberately specific because the operator has to act on
    /// it.
    #[test]
    fn an_internal_failure_reveals_nothing_about_itself() {
        let err = to_api_error(BundleError::Archive(std::io::Error::other(
            "/var/lib/yagra/secret-path exploded",
        )));
        assert_eq!(err.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert!(!err.message().contains("secret-path"));
    }
}
