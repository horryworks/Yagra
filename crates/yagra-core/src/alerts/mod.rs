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
//! # Layout (ADR-083, ADR-202 Inc.4)
//!
//! This was one 6,376-line file until 2026-08-21. It held two programs that shared no type —
//! the engine that decides, and the delivery that pages — plus the rule index they both sit on.
//! The split is by what a reader has to hold in their head at the time:
//!
//! | where | question it answers |
//! |---|---|
//! | [`yagra_alert::rules`] | *which* threshold applies to this (node, port, metric), and what a check is called |
//! | [`yagra_alert::engine`] | *has anything changed*: dwell, flapping, suppression, maintenance, SSE |
//! | [`yagra_alert::reported`] | *is anyone still reporting* a node Yagra never polls itself — a wireless AP — and when an `ok` nobody confirms stops being shown as one (ADR-064 Inc.G) |
//! | [`notify`] | *who gets told*: mutes, routing, the four channels, the vendor wire formats |
//!
//! Since ADR-202 Inc.4 the first three are a crate of their own, built and tested without core:
//! the engine is in-memory and reaches no store. What stays here is everything that does — the
//! config load, delivery, history, the watch loops. The engine's names that the rest of this crate
//! uses are re-exported below, so `crate::alerts::AlertManager` still resolves; a path *into* the
//! engine's modules is spelled `yagra_alert::…`, so the crate boundary shows where it is crossed.
//!
//! 🚨 **The engine must not learn to call delivery, and delivery must not learn engine types.**
//! That the two shared zero types is what made the split provably behaviour-free. The first half
//! is now the compiler's: `yagra-alert` cannot name a type of this crate. The second half is still
//! a convention — if you find yourself importing an engine internal into [`notify`], the thing you
//! want belongs in `yagra_alert::action`, the engine's output types.
//!
//! ⚠️ **What the type split does *not* buy is that delivery cannot stall evaluation** — this doc
//! used to claim it did. The two are decoupled by a *bounded* channel (1024 actions, filled by
//! [`crate::result_ingest`] with a blocking send), not by the absence of a shared type, so a
//! delivery slow enough for long enough still reaches the matcher. ADR-104 narrowed what "slow"
//! means — a wedged vendor endpoint no longer holds up other channels or the 30-second config
//! refresh — but it did not remove that path, and its decision 6 says why not.

pub(crate) mod ack;
pub(crate) mod config;
pub(crate) mod deleted;
pub(crate) mod history;
pub(crate) mod maintenance;
pub(crate) mod notification_log;
pub(crate) mod notifications;
pub(crate) mod notify;
pub(crate) mod notify_facts;
pub(crate) mod notify_render;
pub(crate) mod notify_text;
pub(crate) mod restore;
pub(crate) mod sink;
pub(crate) mod stale;
pub(crate) mod thresholds;

pub(crate) use yagra_alert::action::{history_row, NotifyAction, StreamFrame};
pub(crate) use yagra_alert::engine::AlertManager;
// Only what the rest of the crate actually imports. The four channel types, `RuleCoverage` and
// the scope-resolution helpers are reached through the module that owns them
// (`alerts::notify::…`, `yagra_alert::rules::…`) — re-exporting an item nobody imports would put a
// second name on it, which is the drift this split exists to remove.
pub(crate) use notify::{
    builtin_body_template_for, builtin_for_kind, builtin_subject_template_for, dedup_string,
    ActiveMute, Notifier,
};
pub(crate) use yagra_alert::rules::{check_id, AlertConfig, MerakiOrgScope, NodeMeta, LIVENESS};

/// The engine as this crate runs it: its stream frames titled by [`crate::metric_meaning`], and each
/// node's poll interval read from `intervals`, the handle the scheduler publishes into (ADR-144).
///
/// The one place core hands the engine its titles (ADR-202 Inc.4), so a test of this function is a
/// test of what the live stream says.
pub(crate) fn manager_with(intervals: yagra_common::poll_interval::PollIntervals) -> AlertManager {
    AlertManager::with_poll_intervals(intervals, crate::metric_meaning::alert_title_of)
}

/// [`manager_with`] before any interval is published — what every test that does not care builds.
pub(crate) fn new_manager() -> AlertManager {
    manager_with(yagra_common::poll_interval::PollIntervals::unknown())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The engine titles a stream frame with whatever it was handed (`yagra-alert` tests that with
    /// a stand-in), so the one thing left to prove here is what core hands it.
    #[test]
    fn the_engine_core_builds_titles_its_frames_from_metric_meaning() {
        let mgr = new_manager();
        let mut rx = mgr.subscribe();
        assert!(
            mgr.raise_pool_coverage_alert("tokyo", 0).is_some(),
            "a pool with no live poller raises its alert"
        );
        let (_, body) = rx.try_recv().expect("the fire is streamed");
        let frame: serde_json::Value = serde_json::from_str(&body).expect("a frame is JSON");
        let metric = yagra_alert::engine::POOL_COVERAGE_METRIC;
        assert_eq!(
            frame["title"],
            serde_json::json!(crate::metric_meaning::alert_title(metric))
        );
        assert_ne!(
            frame["title"],
            serde_json::json!(metric),
            "the title is a name, not the metric echoed back"
        );
    }
}
