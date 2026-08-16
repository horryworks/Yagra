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
//! (`ManageSystem`), whether stored credentials still decrypt (`ManageCredentials`), and who did
//! what (`ViewAudit`). So the handler demands **all three**, in its signature, rather than picking
//! the loosest and calling it an admin endpoint.
//!
//! Picking one would have made this a privilege-escalation path: a `ManageSystem` administrator could
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
use super::extract::{Admin, RequireManageCredentials, RequireManageSystem, RequireViewAudit};
use super::ApiState;
use crate::poller_logs::RemoteLogs;
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
use yagra_common::NodeId;

/// This domain's slice of the OpenAPI document (ADR-035), merged by [`super::openapi::document`].
#[derive(utoipa::OpenApi)]
#[openapi(paths(support_bundle))]
pub(super) struct Doc;

/// The support-bundle route, merged into `/api/v1` by [`super::router`].
pub(super) fn routes() -> Router<ApiState> {
    Router::new().route("/api/v1/system/support-bundle", get(support_bundle))
}

/// How far back to reach for log files, and which node the bundle is being taken *about*.
#[derive(Debug, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub(crate) struct BundleQuery {
    /// Hours of log history to carry. Clamped to `[1, 168]`; the appender's own retention is
    /// usually the tighter bound.
    since_hours: Option<u32>,
    /// The node this bundle is about. When set, the archive gains a `node/` section describing
    /// exactly that node: what it is and which poller holds it, its stored interface rows, the
    /// metrics it is configured to collect, which of those are actually arriving, and its alerts.
    ///
    /// Omitting it is not an error — the rest of the bundle is unchanged, and the manifest names
    /// this parameter so a reader who needed the section learns it exists.
    node_id: Option<uuid::Uuid>,
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
        (status = 200, description = "A gzipped tar of JSON and text files: build provenance, every system-health section, the environment allow-list, applied migrations, table sizes, active alerts, the audit tail, the Prometheus scrape, and rotated log files — core's, any co-located poller's, and any remote-site poller's that answered a request on the bus within the collection deadline. With `node_id`, also a `node/` section describing that one node — its inventory row and owning poller, its stored interface rows, the metrics it is configured to collect, which of those are arriving, and its alerts. Carries no secrets — see MANIFEST.json's `omitted` and `redaction` sections", content_type = "application/gzip"),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks any of ManageSystem, ManageCredentials or ViewAudit", body = super::error::ErrorBody),
        (status = 500, description = "The redaction scan matched, so nothing was released. The rule and the file are named in the log, never the value", body = super::error::ErrorBody),
        (status = 503, description = "Inventory storage is unavailable (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn support_bundle(
    _cfg: RequireManageSystem,
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
    let archive = build(&st, &admin, hours, q.node_id, now)
        .await
        .map_err(to_api_error)?;

    let stamp = now.format("%Y%m%dT%H%M%SZ");
    tracing::info!(
        bytes = archive.len(),
        window_hours = hours,
        node_id = ?q.node_id,
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
    node_id: Option<uuid::Uuid>,
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
    match admin
        .audit
        .list(&crate::audit::AuditFilter::newest(AUDIT_ROWS))
        .await
    {
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

    // ── The node this bundle is about (ADR-045 Inc.1) ─────────────────────────────────────────
    match node_id {
        Some(id) => add_node_section(&mut b, st, admin, id).await?,
        None => b.omit(
            "node/",
            "no node was named. Add `?node_id=<uuid>` to carry one node's interface rows, its \
             configured collection set, which of those metrics are actually arriving, and its \
             alerts — the section to ask for when the question is about one device rather than \
             about the deployment.",
        ),
    }

    // ── Remote-site poller logs, over the bus (ADR-045 Inc.4) ─────────────────────────────────
    // Asked for here, before the blocking section, because it is the last thing in `build` that
    // awaits: a fan-out to every site plus a bounded wait for the answers. The result is moved into
    // the blocking closure below, which decides which of them to keep — that decision needs the
    // on-disk file list, and reading it is itself blocking.
    let remote = collect_remote_poller_logs(st, admin, hours).await;

    // ── Logs, then assembly ───────────────────────────────────────────────────────────────────
    // Everything from here is synchronous and unbounded-ish: `collect_logs` reads up to
    // MAX_LOG_BYTES (24 MB) of rotated log files off disk, and `finish` then runs the redaction
    // scan and the gzip over those same bytes. There are no awaits left in `build`, so on a Tokio
    // worker that stretch is a straight stall of one runtime thread — on a support bundle, i.e.
    // exactly when the deployment is already unwell and someone is watching. Onto the blocking
    // pool, the same discipline the IP→ASN reloader (`main.rs`) and the store-and-forward disk
    // read (`yagra-poller`) already follow.
    let scan = SecretScan::from_env();
    tokio::task::spawn_blocking(move || {
        collect_logs(&mut b, hours, remote);
        b.finish(&scan, now)
    })
    .await
    // A `JoinError` here means the assembly panicked or was cancelled — the bundle genuinely was
    // not produced, which is what `Archive` already means to the caller. Reusing it keeps
    // `BundleError` (and so the 500 it maps to) unchanged.
    .map_err(|e| BundleError::Archive(std::io::Error::other(e)))?
}

/// How many audit rows to carry. The store's own page maximum — enough to cover the change that
/// preceded an incident without turning the bundle into an audit export.
const AUDIT_ROWS: i64 = crate::audit::MAX_LIMIT;

/// How many alert-history transitions to carry for the named node. Bounded per node rather than
/// per bundle: this is "how has this one device behaved lately", not an incident export.
const NODE_HISTORY_ROWS: i64 = 200;

/// Add the `node/` section: everything this deployment knows about one node (ADR-045 Inc.1).
///
/// Five files, each collected independently. The module's "degrade, never fail" rule applies here
/// too, and it is what makes an unknown node id produce a bundle carrying five explained omissions
/// rather than an error in place of the whole archive — a bundle is requested *because* something
/// is wrong, and "that id does not resolve" is itself an answer worth shipping.
///
/// **The pair that earns the section is `collection.json` and `metrics.json`.** One says what the
/// node is *configured* to collect; the other says which of those are actually arriving. Either
/// alone is ambiguous in exactly the way ADR-046 named — "no data" reads as "not configured" when
/// you cannot see the configuration beside it.
async fn add_node_section(
    b: &mut BundleBuilder,
    st: &ApiState,
    admin: &super::AdminState,
    node_id: uuid::Uuid,
) -> Result<(), BundleError> {
    let node = NodeId::from(node_id);

    // Identity — assembled from the same seams the WebUI and MCP read (`node_kinds`,
    // `node_assignment_of`, `display_state`) rather than re-derived here. A bundle that described
    // the node differently from the screens it was taken to explain would be worse than no
    // section: the reader would have to work out which of the two to believe.
    match admin.repo.get_node(node_id).await {
        Ok(Some(row)) => {
            let kind = super::nodes::node_kinds(admin, &[node_id])
                .await
                .remove(&node_id);
            let assignment = super::pollers::node_assignment_of(st, admin, node_id)
                .await
                .ok();
            let state = super::nodes::display_state(st, node).await;
            b.add_json(
                "node/identity.json",
                "What this node is, where it sits, and which poller currently holds it",
                &serde_json::json!({
                    "node": row,
                    "kind": kind,
                    "display_state": state,
                    "assignment": assignment,
                }),
            )?;
        }
        Ok(None) => b.omit(
            "node/identity.json",
            "the requested node_id does not exist in this deployment's inventory. The rest of the \
             node/ section is absent for the same reason.",
        ),
        Err(e) => {
            tracing::warn!(error = %e, "support bundle: node identity unavailable");
            b.omit("node/identity.json", "PostgreSQL was not readable");
        }
    }

    // The interface rows exactly as stored. This is the file the section was added for: every
    // column of `interfaces` is written by a *different* walk under COALESCE, so "what is stored"
    // and "what the device answers" are separate questions and only the first one is answerable
    // from core. ⚠️ An empty array means the walk has never stored a row for this node — it does
    // not mean the device has no ports.
    match admin.repo.list_interfaces(node_id).await {
        Ok(rows) => b.add_json(
            "node/interfaces.json",
            "The stored `interfaces` rows verbatim — names, speed, duplex, media, transceiver and \
             optical bounds. `ifAlias` is operator-authored text and is carried as written",
            &rows,
        )?,
        Err(e) => {
            tracing::warn!(error = %e, "support bundle: interface rows unavailable");
            b.omit("node/interfaces.json", "PostgreSQL was not readable");
        }
    }

    // What the poller is *told* to collect (the resolved set, not the node's own overrides —
    // resolved is what actually goes on the wire).
    match super::collection::node_collection(admin, node_id, true).await {
        Ok(set) => b.add_json(
            "node/collection.json",
            "The effective collection set: the OIDs this node is configured to poll, after profile \
             and group defaults are applied. Read together with node/metrics.json",
            &set,
        )?,
        Err(e) => {
            tracing::warn!(error = %e.message(), "support bundle: node collection unavailable");
            b.omit(
                "node/collection.json",
                "the collection set could not be resolved for this node; the cause is in the \
                 bundled core log",
            );
        }
    }

    // …and which of those are actually arriving. The counterpart to the file above.
    match super::metrics::node_metric_inventory(admin, st.store.as_ref(), node_id, None).await {
        Ok(entries) => b.add_json(
            "node/metrics.json",
            "The metric inventory: per metric, whether data is arriving and how it must be read. A \
             metric present in node/collection.json but flagged no_data here is the difference \
             between 'not configured' and 'configured but not arriving'",
            &entries,
        )?,
        Err(e) => {
            tracing::warn!(error = %e.message(), "support bundle: metric inventory unavailable");
            b.omit(
                "node/metrics.json",
                "the metric inventory could not be built; the TSDB may be unreachable — see \
                 health/dependencies.json",
            );
        }
    }

    // Current alerts plus recent transitions for this node alone.
    let active = st.alerts.alerts_for(node);
    match st.history.as_ref() {
        Some(store) => {
            match store
                .search(&crate::history::HistoryFilter {
                    node_id: Some(node_id),
                    limit: NODE_HISTORY_ROWS,
                    ..crate::history::HistoryFilter::default()
                })
                .await
            {
                Ok(rows) => b.add_json(
                    "node/alerts.json",
                    "This node's currently-active alerts and its most recent alert transitions",
                    &serde_json::json!({ "active": active, "history": rows }),
                )?,
                Err(e) => {
                    tracing::warn!(error = %e, "support bundle: node alert history unavailable");
                    b.add_json(
                        "node/alerts.json",
                        "This node's currently-active alerts. The transition history was not \
                         readable — see the bundled core log",
                        &serde_json::json!({ "active": active, "history": null }),
                    )?;
                }
            }
        }
        None => b.add_json(
            "node/alerts.json",
            "This node's currently-active alerts. This core keeps no alert history store, so there \
             are no transitions to carry",
            &serde_json::json!({ "active": active, "history": null }),
        )?,
    }

    Ok(())
}

/// Add core's rotated log files, or record why there are none.
///
/// The "not configured" branch is not an error and says so in words an operator can act on: file
/// logging is opt-in, and a bundle from a deployment without it is still worth having — it just
/// cannot answer what happened before the current process started.
fn collect_logs(b: &mut BundleBuilder, hours: u32, remote: RemoteLogs) {
    let Some(dir) = yagra_telemetry::log_dir() else {
        b.omit(
            "logs/",
            "on-disk logging is not enabled: set YAGRA_LOG_DIR (and mount a volume there) so a \
             future bundle can carry core's own log, including a panic from a run that has already \
             ended. Without it the logs exist only in `docker logs` on the host.",
        );
        // A remote site's log arrives over the bus and never touches this directory, so it is still
        // worth carrying — the two paths share only the archive they land in.
        add_remote_poller_logs(b, remote, &std::collections::BTreeSet::new());
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

    let colocated = collect_poller_logs(b, &dir, since);
    add_remote_poller_logs(b, remote, &colocated);
}

/// Where a co-located poller writes its own rolling log files: a subdirectory of core's log
/// directory, so one mounted volume serves both (ADR-045 Inc.3).
///
/// A subdirectory rather than the same directory, even though the filename prefixes already
/// differ. `collect_logs` selects by prefix, and a prefix match is a weaker guarantee than a path:
/// separating them physically means neither collector can ever pick up the other's files, whatever
/// a future service ends up being called.
const POLLER_LOG_SUBDIR: &str = "pollers";

/// The poller share of the bundle's log budget, across **all** pollers.
///
/// Deliberately a **separate** budget rather than a larger shared one: core's [`MAX_LOG_BYTES`] is
/// untouched, so adding this section cannot make a bundle carry less of core's log than the same
/// bundle carried before. A chatty poller must not be able to push out the file that answers "did
/// core even start".
const POLLER_LOG_MAX_BYTES: usize = 8 * 1024 * 1024;

/// Add any co-located poller's rotated log files, and return the poller ids they covered.
///
/// **This reaches only pollers that share a filesystem with core** — the single-node composition,
/// where they are two containers on one host. A remote-site poller writes its logs at its own site
/// and nothing here can see them; since ADR-045 Inc.4 those arrive over the bus instead
/// ([`add_remote_poller_logs`]), and whichever path fails still says so in the manifest rather than
/// leaving an absence, because "no poller logs" and "no poller" look identical in an archive.
///
/// What it *does* reach, and the bus never will, is a poller that is **no longer running**. That
/// is the whole reason this exists alongside the bus path: a poller killed by the OOM killer
/// cannot answer a request, but its last hour is on disk. Same argument ADR-045 決定 2 made for
/// core, one component over.
///
/// The returned id set is what stops the two paths carrying the same file twice for a co-located
/// poller that also answered on the bus.
fn collect_poller_logs(
    b: &mut BundleBuilder,
    core_dir: &std::path::Path,
    since: std::time::SystemTime,
) -> std::collections::BTreeSet<String> {
    let mut covered = std::collections::BTreeSet::new();
    let dir = core_dir.join(POLLER_LOG_SUBDIR);
    if !dir.is_dir() {
        b.omit(
            "logs/ (poller)",
            "no co-located poller log directory. A poller writes here only when it shares core's \
             log volume and sets YAGRA_LOG_DIR to the pollers/ subdirectory; a remote-site poller \
             never can, because its disk is at its own site — it is asked over the bus instead, and \
             any site that did not answer is listed separately below.",
        );
        return covered;
    }
    let collected = support_bundle::collect_logs(&dir, "yagra-poller", since, POLLER_LOG_MAX_BYTES);
    if collected.files.is_empty() && collected.dropped_for_size == 0 {
        b.omit(
            "logs/ (poller)",
            "the poller log directory exists but held nothing inside the window. A poller that has \
             not run in this window writes nothing — check health/pollers.json for whether one was \
             expected to be alive.",
        );
    }
    for (name, bytes) in collected.files {
        if let Some(id) = poller_id_from_log_name(&name) {
            covered.insert(id);
        }
        b.add_bytes(
            &format!("logs/{name}"),
            "One rotated hour of a co-located poller's structured log (JSON lines). The filename \
             carries the poller id",
            bytes,
        );
    }
    if collected.dropped_for_size > 0 {
        b.omit(
            format!("{} older poller log file(s)", collected.dropped_for_size),
            format!(
                "the {POLLER_LOG_MAX_BYTES}-byte poller log cap was reached; the newest hours were \
                 kept. This budget is separate from core's, so nothing of core's log was displaced."
            ),
        );
    }
    covered
}

/// Recover the poller id from a log filename written by `yagra_telemetry::file_prefix`:
/// `yagra-poller-<id>.<YYYY-MM-DD-HH>.log`.
///
/// Used only to avoid carrying a co-located poller's log twice when it answers on the bus as well.
/// A name that does not parse yields `None`, which costs a duplicate rather than a missing file —
/// the safe direction for a diagnostic artefact.
fn poller_id_from_log_name(name: &str) -> Option<String> {
    let rest = name.strip_prefix("yagra-poller-")?;
    let (id, _) = rest.split_once('.')?;
    (!id.is_empty()).then(|| id.to_owned())
}

/// Ask every live poller that can answer for its own log, over the bus (ADR-045 Inc.4).
///
/// Which pollers: those the coordinator currently considers **online** and that advertise
/// [`yagra_bus::CAP_LOG_SHIP`]. The capability is the difference between "this site sent nothing"
/// and "there was nothing to send" — a poller with no file layer would answer every request with an
/// empty reply, indistinguishable from one that is not subscribed at all.
///
/// Returns an empty result — never an error — when there is no collector (skeleton mode) or no
/// write side. A bundle is requested because something is broken; a bus that is down is a section
/// this bundle lacks, not a reason to refuse it.
async fn collect_remote_poller_logs(
    st: &ApiState,
    admin: &super::AdminState,
    hours: u32,
) -> RemoteLogs {
    let Some(collector) = st.poller_logs.as_ref() else {
        return RemoteLogs::default();
    };
    let targets: Vec<String> = admin
        .coordinator
        .poller_views(std::time::Instant::now())
        .into_iter()
        .filter(|v| v.online && v.caps.iter().any(|c| c == yagra_bus::CAP_LOG_SHIP))
        .map(|v| v.id)
        .collect();
    let since = chrono::Utc::now() - chrono::Duration::hours(i64::from(hours));
    collector.collect(&targets, since.timestamp()).await
}

/// Add what the remote pollers sent, and name every one that sent nothing.
///
/// `already_on_disk` holds the ids the disk path already covered, so a co-located poller that also
/// answered on the bus does not appear twice. Its disk copy wins, deliberately: it is the one that
/// survives the poller dying, and it was collected under the same cap as core's own log.
fn add_remote_poller_logs(
    b: &mut BundleBuilder,
    remote: RemoteLogs,
    already_on_disk: &std::collections::BTreeSet<String>,
) {
    for f in remote.files {
        if already_on_disk.contains(&f.poller_id) {
            continue;
        }
        b.add_bytes(
            &format!("logs/remote/{}", f.name),
            "One rotated hour of a remote-site poller's structured log (JSON lines), sent over the \
             bus. The poller scanned it for its own device credentials before sending — core's scan \
             cannot see those",
            f.bytes,
        );
    }
    for gap in remote.gaps {
        if already_on_disk.contains(&gap.poller_id) {
            continue;
        }
        b.omit(format!("logs/remote/ ({})", gap.poller_id), gap.why);
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

    /// A private directory for one test, named after the test so two cannot collide.
    fn scratch(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("yagra-poller-logs-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// No subdirectory ⇒ an omission, **not** silence. "No poller logs" and "no poller" look
    /// identical in an archive, and the whole point of the manifest is that they must not.
    #[test]
    fn a_deployment_with_no_colocated_poller_says_so() {
        let dir = scratch("absent");
        let mut b = BundleBuilder::new(6);
        collect_poller_logs(&mut b, &dir, std::time::UNIX_EPOCH);

        assert!(b.paths().is_empty());
        let why = b
            .omissions()
            .into_iter()
            .find(|(what, _)| *what == "logs/ (poller)")
            .expect("the absence is declared")
            .1
            .to_owned();
        assert!(why.contains("remote-site"), "{why}");
    }

    /// The file is carried under `logs/` with its name intact — the name is what identifies which
    /// poller wrote it, so a collector that renamed it would throw away the answer to "which one".
    #[test]
    fn a_colocated_pollers_log_is_carried_with_its_id_in_the_name() {
        let dir = scratch("present");
        let pollers = dir.join(super::POLLER_LOG_SUBDIR);
        std::fs::create_dir_all(&pollers).unwrap();
        std::fs::write(
            pollers.join("yagra-poller-edge-1.2026-08-16-10.log"),
            b"{\"msg\":\"hi\"}\n",
        )
        .unwrap();

        let mut b = BundleBuilder::new(6);
        collect_poller_logs(&mut b, &dir, std::time::UNIX_EPOCH);

        assert_eq!(
            b.paths(),
            vec!["logs/yagra-poller-edge-1.2026-08-16-10.log"]
        );
    }

    /// A directory that exists but is empty within the window is its own answer, and a different
    /// one from "no poller here" — it means a poller was set up and has not written lately.
    #[test]
    fn an_empty_poller_directory_is_reported_separately_from_an_absent_one() {
        let dir = scratch("empty");
        std::fs::create_dir_all(dir.join(super::POLLER_LOG_SUBDIR)).unwrap();

        let mut b = BundleBuilder::new(6);
        collect_poller_logs(&mut b, &dir, std::time::UNIX_EPOCH);

        let why = b
            .omissions()
            .into_iter()
            .find(|(what, _)| *what == "logs/ (poller)")
            .expect("the empty directory is declared")
            .1
            .to_owned();
        assert!(why.contains("health/pollers.json"), "{why}");
    }

    /// ⚠️ The property that makes this section safe to add at all: **core's budget is untouched.**
    /// If the two shared one, a chatty poller could displace the file that answers "did core even
    /// start" — which is the first question a bundle is taken to answer.
    #[test]
    fn the_poller_budget_is_separate_from_cores() {
        assert_ne!(
            super::POLLER_LOG_MAX_BYTES,
            MAX_LOG_BYTES,
            "a shared budget lets poller logs displace core's"
        );
        // A const block: both operands are constants, so this is checked when the crate compiles
        // rather than when the test runs. Stronger than the assertion above, and the reason the
        // weaker one stays is its message — a `const` failure names no reason.
        const { assert!(super::POLLER_LOG_MAX_BYTES < MAX_LOG_BYTES) };
    }

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
    /// are refused with **403** rather than 401 — the two must stay distinguishable — and since
    /// ADR-057 the Operator case is the interesting one: that role now holds `ManageCredentials`,
    /// one of the three this endpoint demands, and is still refused because the bundle also carries
    /// the deployment's own state and the audit log. A union guard is only worth writing when its
    /// members can come apart, and they now have.
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

    /// Assembly runs on the blocking pool, so a panic in it arrives as a `JoinError` rather than
    /// unwinding the handler. It must land on the same silent 500 as any other I/O failure — a
    /// panic message is the one internal string most likely to carry a path or a fragment of the
    /// very log the redaction scan exists to keep out of the response.
    #[tokio::test]
    async fn a_panic_while_assembling_is_a_silent_500_too() {
        let join_err = tokio::task::spawn_blocking(|| panic!("/var/lib/yagra/kek.bin exploded"))
            .await
            .expect_err("the task panicked, so the join must fail");
        let err = to_api_error(BundleError::Archive(std::io::Error::other(join_err)));
        assert_eq!(err.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert!(!err.message().contains("kek.bin"));
    }
}
