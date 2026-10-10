// SPDX-License-Identifier: AGPL-3.0-only
//! What the engine hands back about a committed transition, and what it streams.
//!
//! These lived in `yagra-core`'s `alerts/mod.rs` until the engine moved here (ADR-202 Inc.4). They
//! are the engine's output types: core's delivery, history and SSE edge read them, and the engine
//! names no type of theirs.

use std::sync::Arc;

use crate::{Alert, Subject};

/// One SSE frame: the subject it concerns, beside the already-serialized JSON body.
///
/// The subject travels *alongside* the payload rather than being parsed back out of it, because the
/// only consumer that needs it is the group-scope filter on the stream handler (ADR-014) and
/// deserializing every frame per subscriber to recover a field the sender already had would be pure
/// waste. The body stays shared rather than owned: `broadcast` clones the value once **per
/// receiver**, and a full sweep can emit one node-state frame per node, so with many dashboards open
/// this is the difference between cloning a pointer and cloning the JSON N times.
///
/// It is a [`Subject`] rather than a `NodeId` so a pool-coverage alert can be streamed at all. The
/// node-state channel only ever carries `Subject::Node` — a rolled-up display state belongs to a
/// node by definition — and shares the type so both streams go through one scope filter rather
/// than two copies of it.
pub type StreamFrame = (Subject, Arc<str>);

/// What the manager wants done about a committed transition.
#[derive(Debug, Clone)]
pub enum NotifyAction {
    /// A new problem alert fired.
    Fire(Alert),
    /// An alert recovered (carries the previously-active alert so it can be logged and its
    /// dedup state cleared).
    Resolve(Alert),
    /// A still-active downstream alert was rolled up under a newly-down upstream (event-driven
    /// dependency suppression). It had been paging standalone, so its remote incident must be
    /// **closed** — but unlike [`Self::Resolve`] the node has *not* recovered; it stays live in
    /// the UI, now grouped under its root cause. Carries the alert with its new `root_cause` set.
    Suppress(Alert),
}

/// The `alert_history` row a notify action produces — `(alert, resolved)`, or `None` for nothing.
///
/// **Every alert source in core goes through this**, and that is the point of ADR-092: the
/// rule was written five times, in five shapes, and each copy derived `resolved` for itself —
/// `matches!(action, Resolve(_))` in the two watch loops, a `match` in `events::run_action`, a
/// `filter_map` in `events::history_rows`, and two literal `false`/`true` arguments in
/// `result_ingest`. Nothing made them agree, and the copies are why the *effect* could drift: a
/// watch loop shipped notifying without recording, and the alert paged with no row behind it.
///
/// It was called `coverage_alert_of` and lived in core's `main.rs` until ADR-083, then
/// `recordable_alert` until ADR-092 folded `resolved` into it, and moved here with the engine
/// (ADR-202 Inc.4). The first name said "pool coverage" while the interface
/// watch had been calling it too, which is the kind of name that stops a reader from finding the
/// second caller.
///
/// **[`NotifyAction::Suppress`] returns `None`**, and the reason differs by caller, so no single one
/// of them justifies the arm:
/// * pool coverage — dependency suppression is a property of the node graph, and a pool is not in it;
/// * interface utilisation — `Suppress` is only ever produced by
///   [`crate::engine::AlertManager::resweep_suppression`], which runs off a **liveness**
///   transition; the interface watch reaches the engine through
///   [`crate::engine::AlertManager::observe_interface_metric`], which cannot get there;
/// * the poll path — a roll-up means the node is still down, so it is not a lifecycle resolve and
///   the eventual real recovery is what records;
/// * events — an event alert is never dependency-suppressed (a device emitting an event is
///   demonstrably reachable), so the pipeline never produces one.
///
/// It is spelled out rather than caught by a wildcard so a fourth action variant has to decide what
/// History should do with it.
pub fn history_row(action: &NotifyAction) -> Option<(&Alert, bool)> {
    match action {
        NotifyAction::Fire(a) => Some((a, false)),
        NotifyAction::Resolve(a) => Some((a, true)),
        NotifyAction::Suppress(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;
    #[test]
    fn every_notify_action_decides_what_history_does_with_it() {
        use crate::{Alert, Breach, Subject};
        let alert = Alert {
            subject: Subject::Pool("tokyo".to_owned()),
            check: yagra_common::CheckId::from(Uuid::nil()),
            severity: yagra_common::Severity::Critical,
            state: yagra_common::NodeState::Unreachable,
            at_unix_ms: 0,
            root_cause: None,
            flapping: false,
            metric: "live_pollers".to_owned(),
            breach: None::<Breach>,
            ifindex: None,
            row: None,
            row_name: None,
        };
        // A fire and a resolve are both rows, and `resolved` is decided here rather than by each
        // caller — the half that used to be written five ways (ADR-092).
        assert_eq!(
            history_row(&NotifyAction::Fire(alert.clone())).map(|(_, r)| r),
            Some(false)
        );
        assert_eq!(
            history_row(&NotifyAction::Resolve(alert.clone())).map(|(_, r)| r),
            Some(true)
        );
        // Suppression is a property of the node dependency graph, which a pool is not in.
        assert!(history_row(&NotifyAction::Suppress(alert)).is_none());
    }
}
