// SPDX-License-Identifier: AGPL-3.0-only
//! What the engine already knew, before this process started (ADR-097 decision 2).
//!
//! The alert engine is in-memory by design, so every restart used to begin believing the network
//! was fine. Two things went wrong with that, and the second is the worse one:
//!
//! - **A still-broken device re-fired.** `dedup_string` carries the check id and is stable, so an
//!   external tool merges the duplicate — but `alert_history` does not, and one outage therefore
//!   reads as N separate never-closed incidents. Measured on the test server: 1,356 transitions
//!   carrying **18** clears, and one continuously-down device with eight `__liveness__` fires and
//!   no clear inside 24 hours.
//! - **A device that recovered *during* the restart never resolved.** The engine had forgotten the
//!   alert, so the recovery was not a transition, so nothing was ever sent. The incident stays open
//!   in PagerDuty/JSM until a human closes it by hand.
//!
//! `alert_history` is an append-only transition log, which is exactly the shape that can answer
//! "what was open": the newest row per check, kept when it is a fire. That is
//! [`AlertHistoryStore::open_alerts`]; this module turns those rows back into the [`Alert`]s the
//! engine holds and hands them over.
//!
//! 🚨 **This must finish before poll results start arriving.** It is idempotent and never
//! overwrites (see [`AlertManager::restore`]), so a late call is harmless rather than destructive —
//! but a late call is also useless, because the first result of a still-broken check would already
//! have re-fired it. `run_live` awaits it while it is still building handles, before `LeaderTasks`
//! spawns anything.

use std::time::Instant;

use yagra_alert::{Alert, Breach};
use yagra_common::IfIndex;

use crate::history::{AlertHistoryRow, AlertHistoryStore};

use super::{check_id, AlertManager, LIVENESS};

/// Backstop on how many open alerts one restore will take, **per side**.
///
/// Not a policy — the answer is bounded by the number of *checks* a deployment has, not by the size
/// of the log, and a fleet with more open alerts than this has bigger problems than a restart. It
/// exists so that a pathological table cannot make core startup unbounded, and it warns when hit
/// rather than silently restoring a prefix.
///
/// 🚨 **Per side, not shared.** The deleted-node rows (ADR-097 Increment 5) are counted separately
/// on purpose: they are taken newest-first, a mass deletion's residue is the newest thing in the
/// table, and one shared budget would let 43,227 orphans push the live fleet's genuinely open
/// alerts out of the restore — which re-fires them, the exact cost this module exists to avoid.
const RESTORE_MAX: i64 = 50_000;

/// Read the open alerts back and seed the engine with them.
///
/// Two reads, not one, and they go to two different entry points:
///
/// - **Alerts about something that still exists** seed the engine proper, including the fleet's
///   `live`/`down` view — [`AlertManager::restore`].
/// - **Alerts about a node the inventory no longer holds** seed the alert set only, so that the
///   deleted-node sweep can close them properly instead of them staying open forever —
///   [`AlertManager::restore_deleted`], where the reason for the asymmetry is written down.
///
/// A failed read is logged and skipped rather than fatal, and that is safe in a way it would not be
/// for `alerts::config` (ADR-080): restoring nothing lands on exactly the behaviour that shipped
/// before this ADR, and no path here can *resolve* anything, so a degraded read cannot close an
/// incident. It can only cost a duplicate fire. The two reads fail independently — a failure on the
/// orphan side leaves those rows for the next restart and does not cost the live fleet its restore.
pub(crate) async fn restore(mgr: &AlertManager, history: &AlertHistoryStore) {
    let started = Instant::now();
    let Some(rows) = read(history.open_alerts(RESTORE_MAX).await, "open alerts") else {
        return;
    };
    let taken = mgr.restore(rows.into_iter().filter_map(alert_from_row).collect());
    tracing::info!(
        restored = taken,
        elapsed_ms = started.elapsed().as_millis() as u64,
        "restored the alerts that were open before this process started (ADR-097)"
    );

    // The orphan side is deliberately *after* the fleet's own restore: it is the slower query (a
    // `NOT IN` over the inventory) and nothing waits on it, whereas a poll result arriving before
    // the live fleet is seeded is a duplicate incident.
    let Some(rows) = read(
        history.open_alerts_of_deleted_nodes(RESTORE_MAX).await,
        "open alerts of deleted nodes",
    ) else {
        return;
    };
    if rows.is_empty() {
        return;
    }
    let orphans = rows.len();
    mgr.restore_deleted(rows.into_iter().filter_map(alert_from_row).collect());
    tracing::info!(
        orphans,
        "alerts about nodes the inventory no longer holds were read back so the deleted-node \
         sweep can close them (ADR-097 Inc.5); they are not counted in any fleet total"
    );
}

/// One read, with the cap warning and the degraded-read decision in one place so the two sides
/// cannot answer them differently.
fn read(
    result: anyhow::Result<Vec<AlertHistoryRow>>,
    what: &'static str,
) -> Option<Vec<AlertHistoryRow>> {
    match result {
        Ok(rows) => {
            if rows.len() as i64 >= RESTORE_MAX {
                tracing::warn!(
                    limit = RESTORE_MAX,
                    what,
                    "open-alert restore hit its cap; the oldest were not restored — a live alert \
                     will re-fire on its next poll, a deleted node's will be closed by a later \
                     restart"
                );
            }
            Some(rows)
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                what,
                "reading open alerts failed; starting with an empty engine, so anything still \
                 broken will re-fire on its next poll"
            );
            None
        }
    }
}

/// One stored transition, read back as the alert the engine was holding.
///
/// Two fields are not stored and are re-derived rather than guessed:
///
/// - **`root_cause`** — [`AlertManager::restore`] re-derives it from the restored down set, because
///   it needs every liveness row before it can answer for any one alert.
/// - **`flapping`** — starts `false`. The flap detector's window is a property of the running
///   process, and a restart genuinely has no flap history to carry.
fn alert_from_row(row: AlertHistoryRow) -> Option<Alert> {
    let subject = row.subject()?;
    // The stored metric is the in-memory one verbatim, including the liveness sentinel (checked
    // against the running deployment: 328 `__liveness__` rows and no NULL). The fallback is for
    // rows written before migration 0036 added the column, where the check id is the only thing
    // left that can say what this was — and `metric == LIVENESS` is the predicate the rest of the
    // engine branches on, so getting it back is what keeps `resweep_suppression` correct.
    let metric = row.metric.unwrap_or_else(|| {
        match subject
            .node()
            .is_some_and(|n| check_id(n, LIVENESS) == row.check.into())
        {
            true => LIVENESS.to_owned(),
            false => String::new(),
        }
    });
    let breach = row.observed_value.map(|value| Breach {
        value,
        threshold: row.threshold_value,
        // A stored `observed_value` with no `direction` can only be a row this code did not write;
        // `Above` is the side an operator reads as "too much", and the field is descriptive only.
        direction: row.direction.unwrap_or(yagra_common::Direction::Above),
    });
    Some(Alert {
        subject,
        check: row.check.into(),
        severity: row.severity,
        state: row.state,
        at_unix_ms: row.at_unix_ms,
        root_cause: None,
        flapping: false,
        metric,
        breach,
        ifindex: row.ifindex.map(IfIndex),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;
    use yagra_alert::SubjectKind;
    use yagra_common::{Direction, NodeId, NodeState, Severity};

    fn row(node: NodeId, metric: Option<&str>, state: NodeState) -> AlertHistoryRow {
        AlertHistoryRow {
            id: Uuid::new_v4(),
            node: Some(node.as_uuid()),
            subject_kind: SubjectKind::Node,
            subject_name: None,
            check: check_id(node, metric.unwrap_or(LIVENESS)).0,
            severity: Severity::Critical,
            state,
            at_unix_ms: 1_000,
            resolved: false,
            metric: metric.map(str::to_owned),
            observed_value: None,
            threshold_value: None,
            direction: None,
            ifindex: None,
            recorded_at: "2026-08-24T00:00:00Z".to_owned(),
        }
    }

    /// The accepting side: a stored row round-trips into the alert the engine was holding, metric
    /// included. Written first, because a converter that returned `None` for everything would pass
    /// any test that only checked what it rejects.
    #[test]
    fn a_stored_row_reads_back_as_the_alert_it_came_from() {
        let node = NodeId::new();
        let a = alert_from_row(row(node, Some("snmp_up"), NodeState::Critical))
            .expect("a node row is convertible");
        assert_eq!(a.node(), Some(node));
        assert_eq!(a.metric, "snmp_up");
        assert_eq!(a.state, NodeState::Critical);
        assert_eq!(a.root_cause, None, "re-derived by the manager, not stored");
        assert!(!a.flapping, "a restart has no flap history to carry");
    }

    /// A pre-0036 row has no metric column, and `metric == LIVENESS` is what the engine branches on
    /// — so losing it would make a restored outage stop behaving like an outage.
    #[test]
    fn a_row_with_no_metric_recovers_the_liveness_sentinel_from_its_check_id() {
        let node = NodeId::new();
        let a = alert_from_row(row(node, None, NodeState::Unreachable)).expect("convertible");
        assert_eq!(a.metric, LIVENESS);

        // ...and only for the check id that really is the liveness one.
        let mut other = row(node, None, NodeState::Critical);
        other.check = check_id(node, "cpu_util").0;
        let a = alert_from_row(other).expect("convertible");
        assert_eq!(a.metric, "");
    }

    #[test]
    fn a_breach_reads_back_with_its_bound_and_direction() {
        let node = NodeId::new();
        let mut r = row(node, Some("cpu_util"), NodeState::Warning);
        r.observed_value = Some(91.5);
        r.threshold_value = Some(80.0);
        r.direction = Some(Direction::Above);
        let b = alert_from_row(r)
            .expect("convertible")
            .breach
            .expect("breach");
        assert_eq!(b.value, 91.5);
        assert_eq!(b.threshold, Some(80.0));
        assert_eq!(b.direction, Direction::Above);
    }

    /// This module's own production text — read through [`crate::module_source`] rather than
    /// `include_str!`, so the needles below cannot match the test that writes them.
    fn production_source() -> String {
        crate::module_source::code_no_comments("src/alerts", "restore")
    }

    /// 🚨 **Structural, because the wiring has no seam and the hole is silent.**
    ///
    /// [`restore`] takes a concrete `AlertHistoryStore` over PostgreSQL, so nothing here can run
    /// it. That was measured rather than assumed: deleting the whole orphan branch from this file
    /// left the **entire 2,826-test workspace green**, and the only symptom on a real deployment is
    /// an alert about a deleted node that never closes — which is invisible until someone counts
    /// rows in `alert_history`. It is the defect ADR-097 Increment 5 exists to remove, so losing
    /// the call silently would be losing the increment silently.
    ///
    /// ⚠️ **What this cannot see**: whether the two reads go to the *right* entry points. Swap them
    /// — live alerts into `restore_deleted`, orphans into `restore` — and both needles still match
    /// while the fleet's `down` set fills with nodes that no longer exist. Only the hardware check
    /// covers that.
    #[test]
    fn the_startup_restore_reads_and_seeds_both_sides() {
        let src = production_source();
        for needle in [
            "history.open_alerts(",
            "history.open_alerts_of_deleted_nodes(",
            "mgr.restore(",
            "mgr.restore_deleted(",
        ] {
            assert!(
                src.contains(needle),
                "the startup restore no longer calls `{needle}` — an alert about a node deleted \
                 while core was stopped would stay open forever (ADR-097 Inc.5)"
            );
        }
    }
}
