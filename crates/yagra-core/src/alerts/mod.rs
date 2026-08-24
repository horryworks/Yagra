// SPDX-License-Identifier: AGPL-3.0-only
//! Alert engine wiring (Workstream B).
//!
//! Drives the tested [`yagra_alert`] state machine from live poll results: each node has
//! one ICMP-liveness check whose raw state (reachable→ok, unreachable→unreachable,
//! error→unknown) is fed through dwell-time **hysteresis** + **flapping** detection
//! ([`CheckState`]). A committed transition into a problem state fires an [`Alert`];
//! recovery resolves it. Active alerts are held in memory and broadcast to SSE
//! subscribers; transitions are forwarded to a [`Notifier`] (Webhook) with the engine's
//! dedup + retry.
//!
//! More quality features are wired here on top of liveness:
//! - **Threshold alerting** — each poll sample with a resolved [`EffectiveThreshold`]
//!   (scope inheritance via [`AlertConfig`]) is evaluated and fed through the same
//!   hysteresis/flapping machinery as liveness.
//! - **Dependency suppression** — a node's committed liveness is tracked per node; when a
//!   node goes down and *every* upstream is also down (per the [`Topology`]), its alert is
//!   attributed to the highest down ancestor (`root_cause`) and the downstream
//!   notification is suppressed (rolled up into the parent incident, ADR-015). The alert
//!   still fires for the UI/history — only the duplicate page is suppressed.
//! - **Maintenance windows** — nodes covered by an active window (snapshot in
//!   [`AlertConfig`]) observe `Maintenance` instead of their real state, so no alert can
//!   fire during the window and existing alerts resolve (after the usual dwell). When the
//!   window ends the real state flows again and surviving problems re-commit.
//! - **Mutes** — the [`Notifier`] skips delivery for alerts matching an unexpired mute
//!   (one node, optionally one check). The alert still fires for the UI/history — a mute
//!   only silences the page.
//!
//! # Layout (ADR-083)
//!
//! This was one 6,376-line file until 2026-08-21. It held two programs that shared no type —
//! the engine that decides, and the delivery that pages — plus the rule index they both sit on.
//! The split is by what a reader has to hold in their head at the time:
//!
//! | file | question it answers |
//! |---|---|
//! | [`rules`] | *which* threshold applies to this (node, port, metric), and what a check is called |
//! | [`engine`] | *has anything changed*: dwell, flapping, suppression, maintenance, SSE |
//! | [`notify`] | *who gets told*: mutes, routing, the four channels, the vendor wire formats |
//!
//! Everything the rest of the crate names is re-exported here, so `crate::alerts::X` still
//! resolves for all 26 callers — moving an item between the three files is not a change to them.
//!
//! 🚨 **The engine must not learn to call delivery, and delivery must not learn engine types.**
//! That the two shared zero types is what made the split provably behaviour-free; it is also what
//! stops [`Notifier`]'s serialized dispatch (its own doc admits it) from being able to stall
//! evaluation. If you find yourself importing across that line, the thing you want belongs here.

use std::collections::BTreeSet;
use std::sync::Arc;

use uuid::Uuid;

use yagra_alert::{Alert, Subject};

pub(crate) mod config;
pub(crate) mod engine;
pub(crate) mod notify;
pub(crate) mod restore;
pub(crate) mod rules;
pub(crate) mod sink;
#[cfg(test)]
pub(crate) mod testkit;

pub(crate) use engine::AlertManager;
// Only what the rest of the crate actually imports. The four channel types, `RuleCoverage` and
// the scope-resolution helpers are reached through the module that owns them
// (`alerts::notify::…`, `alerts::rules::…`) — re-exporting an item nobody imports would put a
// second name on it, which is the drift this split exists to remove.
pub(crate) use notify::{builtin_notification, dedup_string, ActiveMute, Notifier};
pub(crate) use rules::{check_id, AlertConfig, DEFAULT_LIVENESS_DWELL, LIVENESS};
// Gated because the item is: the seeded liveness rule exists so a test can install the rule the
// database seeds, and a re-export cannot be wider than what it re-exports.
#[cfg(test)]
pub(crate) use rules::seeded_liveness_rule;

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
/// **Every alert source in the crate goes through this**, and that is the point of ADR-092: the
/// rule was written five times, in five shapes, and each copy derived `resolved` for itself —
/// `matches!(action, Resolve(_))` in the two watch loops, a `match` in `events::run_action`, a
/// `filter_map` in `events::history_rows`, and two literal `false`/`true` arguments in
/// `result_ingest`. Nothing made them agree, and the copies are why the *effect* could drift: a
/// watch loop shipped notifying without recording, and the alert paged with no row behind it.
///
/// It was called `coverage_alert_of` and lived in `main.rs` until ADR-083, then `recordable_alert`
/// until ADR-092 folded `resolved` into it. The first name said "pool coverage" while the interface
/// watch had been calling it too, which is the kind of name that stops a reader from finding the
/// second caller.
///
/// **[`NotifyAction::Suppress`] returns `None`**, and the reason differs by caller, so no single one
/// of them justifies the arm:
/// * pool coverage — dependency suppression is a property of the node graph, and a pool is not in it;
/// * interface utilisation — `Suppress` is only ever produced by [`AlertManager::resweep_suppression`],
///   which runs off a **liveness** transition; the interface watch reaches the engine through
///   [`AlertManager::observe_interface_metric`], which cannot get there;
/// * the poll path — a roll-up means the node is still down, so it is not a lifecycle resolve and
///   the eventual real recovery is what records;
/// * events — an event alert is never dependency-suppressed (a device emitting an event is
///   demonstrably reachable), so the pipeline never produces one.
///
/// It is spelled out rather than caught by a wildcard so a fourth action variant has to decide what
/// History should do with it.
pub(crate) fn history_row(action: &NotifyAction) -> Option<(&Alert, bool)> {
    match action {
        NotifyAction::Fire(a) => Some((a, false)),
        NotifyAction::Resolve(a) => Some((a, true)),
        NotifyAction::Suppress(_) => None,
    }
}

/// Per-node metadata used to resolve threshold scope, and to answer "may this caller see this
/// node" without a database round-trip (`api/scope.rs`).
///
/// ⚠️ **The two group fields are different concepts and must not be confused.** [`Self::tag_groups`]
/// holds *tag values* — free-form labels an operator puts on a node, which `ScopeLevel::Group`
/// thresholds match against. [`Self::folder_group`] is the node's row in the inventory folder tree,
/// which is what RBAC visibility is defined over (ADR-014).
///
/// The field was called `groups` until group scoping landed, and the collision was a live hazard:
/// `Scope::allows` takes a `BTreeSet<String>`, so `principal.can_see(&meta.groups)` compiled, ran,
/// and would have scoped visibility by *threshold tags* — failing **open** for any node whose tags
/// happened to match, with nothing to catch it. Hence the rename and
/// `node_meta_group_is_the_folder_group_not_a_tag_value`.
#[derive(Debug, Clone, Default)]
pub struct NodeMeta {
    /// Profile id (as text) the node belongs to, if any.
    pub profile: Option<String>,
    /// Node **tag values**, for group-scoped thresholds. Not the folder tree — see the type docs.
    pub tag_groups: BTreeSet<String>,
    /// The node's folder group (`nodes.group_id`), for RBAC visibility. `None` = ungrouped, which
    /// a scoped principal may **not** see.
    pub folder_group: Option<Uuid>,
    /// The node's folder group and every group above it, **nearest first** — the chain a
    /// `ScopeLevel::FolderGroup` threshold is matched against (ADR-075 増分 3).
    ///
    /// Ordered, not a set, because the position *is* the specificity: a rule on the node's own
    /// group must beat one on its grandparent. `folder_group` stays separate and stays first here;
    /// RBAC is defined over the node's own group only, and widening it to the chain would let a
    /// principal scoped to a parent see a child it was not granted.
    pub folder_chain: Vec<Uuid>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;
    #[test]
    fn every_notify_action_decides_what_history_does_with_it() {
        use yagra_alert::{Alert, Breach, Subject};
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
