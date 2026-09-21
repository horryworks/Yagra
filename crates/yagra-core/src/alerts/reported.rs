// SPDX-License-Identifier: AGPL-3.0-only
//! Is anyone still reporting this node? — for a node Yagra never polls itself (ADR-064 増分 G).
//!
//! A wireless access point is answered for by its controller: core turns each inventory the
//! controller's AP walk publishes into one result per imported AP (`wireless_fanout.rs`). When the
//! controller stops answering, those results stop — and, deliberately, nothing replaces them
//! (決定 9b: one controller must not raise an alert per AP, and ADR-156 決定 3: absence closes
//! nothing). So the AP's committed liveness simply stayed where it was. On the PoC an AC pair went
//! dark for five hours and its five APs read `ok` the whole time, then `unknown` after a core
//! restart for no reason anyone could see.
//!
//! This module is the ledger that says how long ago each such node was last reported, and the rule
//! for when that is too long:
//!
//! - **What counts as a report** is a result `wireless_fanout` produced for the node, noted by the
//!   live consumer as it hands the result on ([`crate::result_ingest`]). A replayed result is hours
//!   old and is never noted.
//! - **When it is stale**: older than `max(600 s, 3 × the reporter's poll interval)` —
//!   [`fresh_for_ms`]. 600 s is [`FRESH_FLOOR_SECS`], the window the display fallback already asks
//!   the TSDB about, so the answer before and after a core restart is the same answer.
//! - **What a stale report changes is display only.** The engine shows a committed `ok` as
//!   `unknown` (`AlertManager::node_state` and its two siblings); `unreachable` and `maintenance`
//!   stay, and open alerts still roll up on top. The state machine is never fed anything.
//!
//! ⚠️ Only nodes some controller has reported are in the ledger — an ordinary device, a URL or DNS
//! monitor and a Meraki device never are, so their display is exactly what it was (G4).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use yagra_common::NodeId;

use super::AlertManager;

/// The shortest time a report stays current, in seconds.
///
/// 🚨 **The same number as the display fallback's freshness window** (`api::nodes`, which reads
/// this constant). After a core restart the engine holds no opinion about an AP, and the fallback
/// decides `ok`/`unknown` from whether a liveness sample is this recent; a different floor here
/// would make one outage read differently depending on whether core had restarted — the defect
/// this module exists to remove.
pub(crate) const FRESH_FLOOR_SECS: u64 = 600;

/// How many of the reporter's polls a report may miss before it is stale. Three, so one slow or
/// incomplete walk (決定 9b drops an incomplete inventory whole) does not grey a controller's APs.
const FRESH_POLLS: u64 = 3;

/// How often [`run_report_watch`] looks for reports that went stale, or came back.
pub(crate) const WATCH_TICK: Duration = Duration::from_secs(15);

/// How long a report stays current, for a reporter polled every `interval` seconds:
/// `max(FRESH_FLOOR_SECS, 3 × interval)`. `None` (no interval published yet) is the floor.
#[must_use]
pub(crate) fn fresh_for_ms(interval: Option<u32>) -> i64 {
    let secs = interval.map_or(FRESH_FLOOR_SECS, |s| {
        FRESH_FLOOR_SECS.max(u64::from(s).saturating_mul(FRESH_POLLS))
    });
    i64::try_from(secs.saturating_mul(1000)).unwrap_or(i64::MAX)
}

/// Whether a report made at `at_unix_ms` by a reporter polled every `interval` seconds is still
/// current at `now_ms`. The boundary itself is current.
#[must_use]
pub(crate) fn is_current(at_unix_ms: i64, interval: Option<u32>, now_ms: i64) -> bool {
    now_ms.saturating_sub(at_unix_ms) <= fresh_for_ms(interval)
}

/// The last report about one node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Report {
    /// When it was made — the controller's poll time, not when core read it.
    pub(crate) at_unix_ms: i64,
    /// Who made it: the controller node, whose poll interval sizes the window.
    pub(crate) by: NodeId,
    /// Whether the node-state stream was last told this report is stale. What stops
    /// [`ReportLedger::flips`] announcing the same node on every tick.
    announced_stale: bool,
}

/// Every node some controller reports, and its last report. Memory only: rebuilt from the live
/// results, and seeded at startup from what PostgreSQL recorded ([`Self::seed`]).
#[derive(Debug, Default)]
pub(crate) struct ReportLedger {
    reports: HashMap<NodeId, Report>,
}

impl ReportLedger {
    /// `by` reported `node` at `at_unix_ms`. A report older than the one held is ignored: results
    /// from two members of an HA pair can arrive out of order, and the newer one is what counts.
    pub(crate) fn note(&mut self, node: NodeId, by: NodeId, at_unix_ms: i64) {
        let entry = self.reports.entry(node).or_insert(Report {
            at_unix_ms,
            by,
            announced_stale: false,
        });
        if at_unix_ms >= entry.at_unix_ms {
            entry.at_unix_ms = at_unix_ms;
            entry.by = by;
        }
    }

    /// What PostgreSQL last recorded, for a node this process has not heard about yet. Never
    /// overrides a live [`Self::note`] — the seed is older by construction.
    pub(crate) fn seed(&mut self, node: NodeId, by: NodeId, at_unix_ms: i64) {
        self.reports.entry(node).or_insert(Report {
            at_unix_ms,
            by,
            announced_stale: false,
        });
    }

    #[must_use]
    pub(crate) fn get(&self, node: &NodeId) -> Option<&Report> {
        self.reports.get(node)
    }

    #[must_use]
    pub(crate) fn len(&self) -> usize {
        self.reports.len()
    }

    #[must_use]
    pub(crate) fn is_empty(&self) -> bool {
        self.reports.is_empty()
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (&NodeId, &Report)> {
        self.reports.iter()
    }

    /// Drop every node `keep` says no longer exists.
    pub(crate) fn retain(&mut self, mut keep: impl FnMut(&NodeId) -> bool) {
        self.reports.retain(|node, _| keep(node));
    }

    /// The nodes whose report went stale, or came back, since the stream was last told — each
    /// named once per change, never once per tick. `interval_of` is the reporter's poll interval.
    pub(crate) fn flips(
        &mut self,
        now_ms: i64,
        interval_of: impl Fn(NodeId) -> Option<u32>,
    ) -> Vec<NodeId> {
        let mut out = Vec::new();
        for (node, report) in &mut self.reports {
            let stale = !is_current(report.at_unix_ms, interval_of(report.by), now_ms);
            if stale != report.announced_stale {
                report.announced_stale = stale;
                out.push(*node);
            }
        }
        out
    }
}

/// Leader-only loop: every [`WATCH_TICK`], tell the node-state stream about every reported node
/// whose report went stale or came back (G6).
///
/// 🚨 **Not optional polish.** A tree left open overlays what the stream last said on top of what
/// it fetched (`web/src/dashboard/useNodeStates.ts`: the overlay wins), and a report going stale
/// is not a transition the state machine ever makes — nothing is observed. Without this, a page
/// that watched an AP go `ok` keeps drawing it green for as long as it stays open, however stale
/// the report underneath has become.
///
/// # Why leader-only
///
/// The same reason as the deleted-node and stale-check watches: poll-result ingest is leader-only,
/// so only the leader's engine holds a ledger. A standby's is empty and this would do nothing there.
pub(crate) async fn run_report_watch(alerts: Arc<AlertManager>) {
    let mut tick = tokio::time::interval(WATCH_TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        alerts.announce_report_staleness(crate::pool_coverage::now_unix_ms());
    }
}

#[cfg(test)]
mod tests {
    use super::super::testkit::{cfg, meta_for, open_alert, result};
    use super::*;
    use crate::poll_interval::{IntervalSnapshot, PollIntervals};
    use std::collections::HashMap;
    use yagra_bus::CheckOutcome;
    use yagra_common::NodeState;

    const MIN: i64 = 60_000;

    fn now() -> i64 {
        crate::pool_coverage::now_unix_ms()
    }

    /// A manager whose AP node has committed `outcome`, reported by `controller` at `at`: the
    /// liveness dwell satisfied (three results), and the report noted the way the live consumer
    /// notes one.
    fn reported(
        mgr: &AlertManager,
        ap: NodeId,
        controller: NodeId,
        outcome: CheckOutcome,
        at: i64,
    ) {
        for i in 0..3 {
            mgr.note_report(ap, controller, at + i);
            mgr.observe(&result(ap, outcome, at + i));
        }
    }

    fn manager_with(intervals: PollIntervals) -> AlertManager {
        let mgr = AlertManager::with_poll_intervals(intervals);
        mgr.set_config(cfg(Vec::new(), HashMap::new()));
        mgr
    }

    #[test]
    fn the_window_is_ten_minutes_or_three_polls_whichever_is_longer() {
        assert_eq!(fresh_for_ms(None), 600_000, "no interval yet: the floor");
        assert_eq!(
            fresh_for_ms(Some(60)),
            600_000,
            "three fast polls: the floor"
        );
        assert_eq!(fresh_for_ms(Some(200)), 600_000, "exactly the floor");
        assert_eq!(
            fresh_for_ms(Some(900)),
            2_700_000,
            "a slow controller stretches it"
        );
        assert!(is_current(0, None, 600_000), "the boundary is current");
        assert!(!is_current(0, None, 600_001));
        assert!(is_current(0, Some(900), 2_700_000));
        assert!(!is_current(0, Some(900), 2_700_001));
    }

    #[test]
    fn a_newer_report_wins_and_an_older_one_is_ignored() {
        let (ap, a, b) = (NodeId::new(), NodeId::new(), NodeId::new());
        let mut ledger = ReportLedger::default();
        ledger.note(ap, a, 2_000);
        ledger.note(ap, b, 1_000);
        assert_eq!(
            ledger.get(&ap).map(|r| (r.at_unix_ms, r.by)),
            Some((2_000, a))
        );
        ledger.note(ap, b, 3_000);
        assert_eq!(
            ledger.get(&ap).map(|r| (r.at_unix_ms, r.by)),
            Some((3_000, b))
        );
        // A seed never replaces what was noted.
        ledger.seed(ap, a, 9_999);
        assert_eq!(
            ledger.get(&ap).map(|r| (r.at_unix_ms, r.by)),
            Some((3_000, b))
        );
    }

    #[test]
    fn a_stale_ok_reads_unknown_on_every_display_surface() {
        let mgr = manager_with(PollIntervals::unknown());
        let (ap, ctl) = (NodeId::new(), NodeId::new());
        reported(&mgr, ap, ctl, CheckOutcome::Reachable, now() - 20 * MIN);
        // The committed liveness is untouched — only the colour changes.
        assert_eq!(mgr.node_liveness(ap), Some(NodeState::Ok));
        assert_eq!(mgr.node_state(ap), Some(NodeState::Unknown));
        assert_eq!(
            mgr.node_states_for(&[ap]).get(&ap).copied(),
            Some(NodeState::Unknown)
        );
        assert_eq!(
            mgr.node_states().get(&ap).copied(),
            Some(NodeState::Unknown)
        );
        assert_eq!(mgr.node_state_counts().get(&NodeState::Unknown), Some(&1));
        assert_eq!(mgr.node_state_counts().get(&NodeState::Ok), None);
        assert!(mgr.unreported_since(ap).is_some());
        // Nothing was raised about it — G5.
        assert!(mgr.active_alerts().is_empty());
    }

    #[test]
    fn a_current_report_leaves_ok_alone() {
        let mgr = manager_with(PollIntervals::unknown());
        let (ap, ctl) = (NodeId::new(), NodeId::new());
        reported(&mgr, ap, ctl, CheckOutcome::Reachable, now() - MIN);
        assert_eq!(mgr.node_state(ap), Some(NodeState::Ok));
        assert_eq!(
            mgr.node_states_for(&[ap]).get(&ap).copied(),
            Some(NodeState::Ok)
        );
        assert_eq!(mgr.node_states().get(&ap).copied(), Some(NodeState::Ok));
        assert_eq!(mgr.unreported_since(ap), None);
    }

    #[test]
    fn a_report_coming_back_turns_it_ok_again_without_touching_alerts() {
        let mgr = manager_with(PollIntervals::unknown());
        let (ap, ctl) = (NodeId::new(), NodeId::new());
        reported(&mgr, ap, ctl, CheckOutcome::Reachable, now() - 20 * MIN);
        assert_eq!(mgr.node_state(ap), Some(NodeState::Unknown));
        let back = now();
        mgr.note_report(ap, ctl, back);
        let actions = mgr.observe(&result(ap, CheckOutcome::Reachable, back));
        assert!(actions.is_empty(), "no alert opened or closed: {actions:?}");
        assert_eq!(mgr.node_state(ap), Some(NodeState::Ok));
        assert_eq!(mgr.unreported_since(ap), None);
    }

    #[test]
    fn down_stays_down_and_an_open_alert_still_rolls_up() {
        let mgr = manager_with(PollIntervals::unknown());
        let (ap, ctl) = (NodeId::new(), NodeId::new());
        // Down when last reported: that is the last thing anyone knew, and its alert is still open.
        reported(&mgr, ap, ctl, CheckOutcome::Unreachable, now() - 20 * MIN);
        assert_eq!(mgr.node_liveness(ap), Some(NodeState::Unreachable));
        assert_ne!(mgr.node_state(ap), Some(NodeState::Unknown));
        assert_ne!(
            mgr.node_states().get(&ap).copied(),
            Some(NodeState::Unknown)
        );
        // Up when last reported, with a threshold alert still open: the alert's colour wins over
        // `unknown`, exactly as it wins over `ok`.
        let ap2 = NodeId::new();
        reported(&mgr, ap2, ctl, CheckOutcome::Reachable, now() - 20 * MIN);
        mgr.restore(vec![open_alert(
            ap2,
            "wlan_ap_client_count",
            NodeState::Warning,
        )]);
        assert_eq!(mgr.node_state(ap2), Some(NodeState::Warning));
        assert_eq!(
            mgr.node_states_for(&[ap2]).get(&ap2).copied(),
            Some(NodeState::Warning)
        );
    }

    #[test]
    fn maintenance_stays_maintenance() {
        let mgr = AlertManager::new();
        let (ap, ctl) = (NodeId::new(), NodeId::new());
        mgr.set_config(cfg(Vec::new(), meta_for(ap)).with_maintenance([ap].into_iter().collect()));
        reported(&mgr, ap, ctl, CheckOutcome::Reachable, now() - 20 * MIN);
        assert_eq!(mgr.node_liveness(ap), Some(NodeState::Maintenance));
        assert_eq!(mgr.node_state(ap), Some(NodeState::Maintenance));
    }

    #[test]
    fn the_window_follows_the_reporting_controllers_interval() {
        let (ap, ctl) = (NodeId::new(), NodeId::new());
        let intervals = PollIntervals::unknown();
        intervals.publish(IntervalSnapshot::build(60, [(ctl.as_uuid(), 900)]));
        let mgr = manager_with(intervals);
        // Twenty minutes is stale at the floor, and current for a controller polled every 15.
        reported(&mgr, ap, ctl, CheckOutcome::Reachable, now() - 20 * MIN);
        assert_eq!(mgr.node_state(ap), Some(NodeState::Ok));
        // Past three of its polls it is stale after all.
        let ap_late = NodeId::new();
        reported(
            &mgr,
            ap_late,
            ctl,
            CheckOutcome::Reachable,
            now() - 50 * MIN,
        );
        assert_eq!(mgr.node_state(ap_late), Some(NodeState::Unknown));
    }

    #[test]
    fn a_node_nobody_reports_is_never_touched() {
        // An ordinary device polled long ago keeps its committed state: it is not in the ledger.
        let mgr = manager_with(PollIntervals::unknown());
        let device = NodeId::new();
        for i in 0..3 {
            mgr.observe(&result(
                device,
                CheckOutcome::Reachable,
                now() - 60 * MIN + i,
            ));
        }
        assert_eq!(mgr.node_state(device), Some(NodeState::Ok));
        assert_eq!(mgr.node_states().get(&device).copied(), Some(NodeState::Ok));
        assert_eq!(mgr.unreported_since(device), None);
    }

    #[test]
    fn the_stream_is_told_once_when_a_report_goes_stale_and_once_when_it_returns() {
        let mgr = manager_with(PollIntervals::unknown());
        let (ap, ctl) = (NodeId::new(), NodeId::new());
        let t0 = now();
        reported(&mgr, ap, ctl, CheckOutcome::Reachable, t0);
        let mut rx = mgr.subscribe_node_states();
        assert_eq!(mgr.announce_report_staleness(t0 + MIN), 0, "still current");
        assert_eq!(
            mgr.announce_report_staleness(t0 + 11 * MIN),
            1,
            "went stale"
        );
        let (_, body) = rx.try_recv().expect("one frame");
        let frame: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(frame["node_id"], ap.as_uuid().to_string());
        assert_eq!(frame["state"], "unknown");
        assert_eq!(
            mgr.announce_report_staleness(t0 + 12 * MIN),
            0,
            "not again on the next tick"
        );
        assert!(rx.try_recv().is_err());
        // The report returns; the next tick says so, once.
        mgr.note_report(ap, ctl, now());
        assert_eq!(mgr.announce_report_staleness(now()), 1, "came back");
        let (_, body) = rx.try_recv().expect("the recovery frame");
        let frame: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(frame["state"], "ok");
        assert_eq!(mgr.announce_report_staleness(now()), 0);
    }

    #[test]
    fn a_seed_explains_an_ap_the_engine_has_no_opinion_about_yet() {
        // Right after a restart: nothing observed, the seed from PostgreSQL is all there is. The
        // display comes from the fallback; the explanation comes from here.
        let mgr = manager_with(PollIntervals::unknown());
        let (ap, ctl) = (NodeId::new(), NodeId::new());
        let at = now() - 5 * 60 * MIN;
        mgr.seed_reports([(ap, ctl, at)]);
        assert_eq!(mgr.node_state(ap), None, "a seed is not an opinion");
        assert_eq!(mgr.unreported_since(ap), Some(at));
        // A fresh seed explains nothing.
        let fresh = NodeId::new();
        mgr.seed_reports([(fresh, ctl, now())]);
        assert_eq!(mgr.unreported_since(fresh), None);
    }

    #[test]
    fn a_deleted_node_is_forgotten() {
        let (ap, gone, ctl) = (NodeId::new(), NodeId::new(), NodeId::new());
        let mgr = AlertManager::new();
        mgr.set_config(cfg(Vec::new(), meta_for(ap)));
        let old = now() - 20 * MIN;
        mgr.seed_reports([(ap, ctl, old), (gone, ctl, old)]);
        mgr.forget_deleted_nodes();
        assert_eq!(mgr.unreported_since(ap), Some(old));
        assert_eq!(mgr.unreported_since(gone), None);
    }
}
