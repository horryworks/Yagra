// SPDX-License-Identifier: AGPL-3.0-only
//! The PostgreSQL retention sweep: nine append-only tables, pruned on the operator's policy
//! (ADR-040), lifted out of `main.rs::run_fleet_health_timeline` by ADR-083.
//!
//! [`crate::retention`] declares *how long* each table is kept and is pure; this is the half that
//! actually deletes. Keeping them apart is that module's own rule — "this module declares the
//! table, and every prune site implements it".
//!
//! 🚨 **This is nine of the ten PostgreSQL prunes, not all of them.** `report_runs` is pruned by
//! `run_report_scheduler` on its own cadence, and folding it in here would change when it happens.
//! So do not read this module as "the place retention is enforced" — read it as "the sweep that
//! rides the fleet-health tick". A tenth table added to `Subject` still has to find a prune site,
//! and this one is only the likeliest home, not the guaranteed one.
//!
//! **Why it is a function and not its own task.** It runs from the same 300-second tick as the
//! fleet-state snapshot, in the same order, on the same `retention` read. A second `tokio::spawn`
//! would have been tidier to look at and would have changed the timing — which ADR-083 forbids,
//! because the whole increment is meant to be provably behaviour-free.

use std::sync::Arc;

use crate::history::AlertHistoryStore;
use crate::pollers::PollerRepo;
use crate::repo::NodeRepo;
use crate::retention::RetentionSettings;
use crate::{analysis, dns_check, events, l3, neighbors, rca};

/// The nine stores the sweep deletes from.
///
/// Built once by the caller and reused every tick — the handles are all `Arc`, so this is a
/// borrow, not a per-tick clone of nine reference counts.
pub(crate) struct Targets {
    pub repo: Arc<NodeRepo>,
    pub history: Arc<AlertHistoryStore>,
    pub events_repo: Arc<events::EventRepo>,
    pub dns_checks: Arc<dns_check::DnsCheckRepo>,
    pub neighbors: Arc<neighbors::NeighborRepo>,
    pub l3: Arc<l3::L3Repo>,
    pub analyses: Arc<analysis::AnalysisRepo>,
    pub rca_reports: Arc<rca::store::RcaRepo>,
    pub pollers: Arc<PollerRepo>,
}

/// Delete everything past its retention window, warning and continuing on each failure.
///
/// **Every failure is a warning, never a return.** A prune that cannot run leaves rows that will
/// be picked up on the next tick five minutes later; a prune that aborts the sweep would let one
/// sick table stop the other eight from ever running, and the symptom would be a disk filling up
/// with no error naming the cause.
///
/// `retention` is passed in rather than read here so the caller's read stays where it was — one
/// read per tick, before the snapshot, exactly as it was written.
pub(crate) async fn sweep(t: &Targets, retention: &RetentionSettings) {
    let Targets {
        repo,
        history,
        events_repo,
        dns_checks,
        neighbors,
        l3,
        analyses,
        rca_reports,
        pollers,
    } = t;
    let alert_linked_secs = retention.alert_linked_secs();
    if let Err(e) = repo.prune_state_snapshots(alert_linked_secs).await {
        tracing::warn!(error = %e, "prune state snapshots failed");
    }
    if let Err(e) = history.prune_old(alert_linked_secs).await {
        tracing::warn!(error = %e, "prune alert history failed");
    }
    // Passive events in PostgreSQL: matched rows follow alert-history retention, unmatched
    // (rule-authoring material) get their own shorter window. When the log store is enabled
    // (ADR-024) unmatched rows never land in PostgreSQL, so this pruning naturally trims
    // PostgreSQL to the alert-linked subset; the log store keeps the full firehose.
    if let Err(e) = events_repo
        .prune_old(alert_linked_secs, retention.unmatched_event_secs())
        .await
    {
        tracing::warn!(error = %e, "prune events failed");
    }
    // DNS chain history is append-on-change, so a healthy fleet writes almost nothing here —
    // the canonicalization in `DnsChain::content_key` is exactly what keeps it that way. Prune
    // on the same window as alert history so the retention story stays consistent.
    if let Err(e) = dns_checks.prune_chain_changes(alert_linked_secs).await {
        tracing::warn!(error = %e, "prune dns chain history failed");
    }
    // Adjacency history is append-on-change for the same reason and on the same window
    // (`retention::Subject::NeighborChanges`): a rack nobody is repatching writes nothing.
    if let Err(e) = neighbors.prune_changes(alert_linked_secs).await {
        tracing::warn!(error = %e, "prune neighbour history failed");
    }
    // Interface-address history, same shape and same window (`retention::Subject::L3Changes`):
    // a network nobody is re-subnetting writes nothing.
    if let Err(e) = l3.prune_changes(alert_linked_secs).await {
        tracing::warn!(error = %e, "prune interface-address history failed");
    }
    // Monitoring gaps go on the alert-linked window too (`retention::Subject::MonitoringGaps`)
    // — a gap explains an absence of alerts, so outliving the alert history would leave a
    // window nothing can be read against.
    if let Err(e) = pollers.prune_monitoring_gaps(alert_linked_secs).await {
        tracing::warn!(error = %e, "prune monitoring gaps failed");
    }
    // Diagnostic artefacts get their own window (`retention::Subject::AnalysisRuns` /
    // `RcaReports`): both are reproducible by asking again, unlike everything above. Analysis
    // findings need no prune of their own — they cascade from the job.
    let diagnostic_secs = retention.diagnostic_secs();
    if let Err(e) = analyses.prune_jobs(diagnostic_secs).await {
        tracing::warn!(error = %e, "prune analysis runs failed");
    }
    if let Err(e) = rca_reports.prune_reports(diagnostic_secs).await {
        tracing::warn!(error = %e, "prune RCA reports failed");
    }
}
