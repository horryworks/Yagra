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

/// Where a component sends an alert transition: the History row **and** the notification, as one
/// step (ADR-092).
///
/// The trait lives here, beside what it carries, so a crate above the engine (the Meraki
/// integration, ADR-202 Inc.6) can be handed one without naming core. Its one implementation —
/// `RecordingSink`, over a history writer and a notifier — stays in core's `alerts/sink.rs`, which
/// says why there is exactly one.
#[async_trait::async_trait]
pub trait AlertSink: Send + Sync {
    /// Record the transition if it produces a row, then deliver it. Both, or neither — a caller
    /// holding this has no way to ask for only one.
    async fn dispatch(&self, action: NotifyAction);
}

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
    /// More down nodes were rolled up into a border whose fire was already sent (ADR-270 decision
    /// 3). Carries the border's open alert with `grouped` set to every node rolled up into it now,
    /// not only the new ones. Nothing recovered and nothing new fired, so it records no History
    /// row; the notifier adds it to the incident the fire opened.
    Update(Alert),
    /// A down node's alert, already rolled up into one outage, now sits inside another: carries
    /// the alert with its new `root_cause` (ADR-270 Inc.4). It never paged, so there is nothing
    /// to close and nothing to send; it is History's record of the move, so the outage it joined
    /// lists it, and the new border's incident is told through an [`Self::Update`].
    Regroup(Alert),
}

/// Which kind of `alert_history` row a transition writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryEvent {
    /// The alert fired.
    Fire,
    /// The alert recovered.
    Resolve,
    /// An alert was grouped into an outage: one that had paged on its own (ADR-270 decision 4),
    /// or one that moved there from another outage (Inc.4). It is still open, so the row is not a
    /// resolve, and it is no new fire either: every reader that counts fires skips it.
    RolledUp,
}

impl HistoryEvent {
    /// The row's `resolved` column.
    #[must_use]
    pub const fn resolved(self) -> bool {
        matches!(self, Self::Resolve)
    }

    /// The row's `rolled_up` column.
    #[must_use]
    pub const fn rolled_up(self) -> bool {
        matches!(self, Self::RolledUp)
    }

    /// The kind of an ordinary row: a fire, or with `resolved` a recovery.
    #[must_use]
    pub const fn of_resolved(resolved: bool) -> Self {
        if resolved {
            Self::Resolve
        } else {
            Self::Fire
        }
    }
}

/// `resolved` as the kind it always meant before a roll-up had a row (ADR-270).
impl From<bool> for HistoryEvent {
    fn from(resolved: bool) -> Self {
        Self::of_resolved(resolved)
    }
}

/// The `alert_history` row a notify action produces — `(alert, kind)`, or `None` for nothing.
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
/// **[`NotifyAction::Suppress`] writes a [`HistoryEvent::RolledUp`] row since ADR-270** (it wrote
/// none before, so the moment an alert that had paged was grouped into an outage left no trace once
/// the alerts closed). It is not a resolve — the node is still down, and the eventual real recovery
/// is what records one. Only the poll path produces it: pool coverage, interface utilisation and
/// events never reach `resweep_suppression`.
///
/// **[`NotifyAction::Update`] returns `None`**: the members of an open outage changed, its
/// lifecycle did not, and History reads the members from the members' own rows.
///
/// **[`NotifyAction::Regroup`] writes a [`HistoryEvent::RolledUp`] row** (ADR-270 Inc.4): it is
/// the member's own row naming the outage it moved into, which is what that outage's member list
/// reads. Without it a node moved from one border to the next was listed under neither.
///
/// It is spelled out rather than caught by a wildcard so a sixth action variant has to decide what
/// History should do with it.
pub fn history_row(action: &NotifyAction) -> Option<(&Alert, HistoryEvent)> {
    match action {
        NotifyAction::Fire(a) => Some((a, HistoryEvent::Fire)),
        NotifyAction::Resolve(a) => Some((a, HistoryEvent::Resolve)),
        NotifyAction::Suppress(a) => Some((a, HistoryEvent::RolledUp)),
        NotifyAction::Update(_) => None,
        NotifyAction::Regroup(a) => Some((a, HistoryEvent::RolledUp)),
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
            notify_held_until: None,
            snmp_failure: None,
            grouped: Vec::new(),
        };
        // A fire and a resolve are both rows, and `resolved` is decided here rather than by each
        // caller — the half that used to be written five ways (ADR-092).
        assert_eq!(
            history_row(&NotifyAction::Fire(alert.clone())).map(|(_, r)| r),
            Some(HistoryEvent::Fire)
        );
        assert_eq!(
            history_row(&NotifyAction::Resolve(alert.clone())).map(|(_, r)| r),
            Some(HistoryEvent::Resolve)
        );
        // ADR-270: a roll-up is a row of its own, neither a fire nor a resolve.
        let rolled = history_row(&NotifyAction::Suppress(alert.clone())).map(|(_, r)| r);
        assert_eq!(rolled, Some(HistoryEvent::RolledUp));
        assert!(rolled.is_some_and(|r| !r.resolved() && r.rolled_up()));
        assert!(history_row(&NotifyAction::Update(alert.clone())).is_none());
        // ADR-270 Inc.4: a move between outages is a roll-up row of its own.
        assert_eq!(
            history_row(&NotifyAction::Regroup(alert)).map(|(_, r)| r),
            Some(HistoryEvent::RolledUp)
        );
    }
}
