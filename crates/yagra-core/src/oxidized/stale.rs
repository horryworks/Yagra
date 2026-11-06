// SPDX-License-Identifier: AGPL-3.0-only
//! The "config backup not fetched" alert (ADR-240 decision 6): once a minute the leader reads every
//! node switched on for config backup, and opens a node's alert when it has had no fetch that
//! worked for [`STALE_AFTER`], or closes it once one has.
//!
//! A node never fetched counts from when its settings were last saved, which is when it was
//! switched on unless someone edited the settings or searched its logins again since. A fetch that
//! worked closes the alert; so does switching the backup off. Nothing else does - a read that
//! fails changes nothing, because an absent answer is not evidence that the node was fetched.
//!
//! A node in a maintenance window gets no new alert (it is being worked on, and a fetch failing
//! then is expected); one already open stays open through the window.
//!
//! Leader-only: two cores raising the same alert would both page. Run only where the bundled
//! oxidized is configured - without it nothing fetches, and every node switched on would page.

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use uuid::Uuid;
use yagra_alert::action::AlertSink;
use yagra_alert::engine::CONFIG_STALE_METRIC;
use yagra_alert::Subject;
use yagra_base::config_backup::{ConfigBackupRepo, StaleWatchNode};
use yagra_common::NodeId;

use crate::alerts::AlertManager;

/// How long a node switched on may go without a fetch that worked. Fetches run daily, so one
/// failed day does not page on its own. ⚠️ The Config tab holds the same number
/// (`web/src/components/NodeDetail/configBackupForm.ts::STALE_AFTER_MS`); a test pins the two.
pub(crate) const STALE_AFTER: Duration = Duration::from_secs(48 * 3600);

/// How often the leader looks.
const WATCH_TICK: Duration = Duration::from_secs(60);

/// What one look decides for one node.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Step {
    /// Open the node's alert; it has gone this many hours without a fetch that worked.
    Raise(Uuid, f64),
    /// Close it.
    Resolve(Uuid),
}

/// How many hours `node` has gone unfetched, when that is at least [`STALE_AFTER`].
#[must_use]
pub(crate) fn hours_unfetched(node: &StaleWatchNode, now: DateTime<Utc>) -> Option<f64> {
    let since = node.last_success_at.unwrap_or(node.settings_saved_at);
    let age = (now - since).to_std().ok()?;
    #[allow(clippy::cast_precision_loss)] // hours of a duration that fits in an i64 of seconds
    (age >= STALE_AFTER).then(|| age.as_secs() as f64 / 3600.0)
}

/// What to do for every node switched on (`nodes`) and every node whose alert is open (`open`).
/// A node whose alert is open and that is no longer switched on - or no longer exists - is closed.
#[must_use]
pub(crate) fn steps(
    nodes: &[StaleWatchNode],
    open: &[Uuid],
    now: DateTime<Utc>,
    in_maintenance: impl Fn(Uuid) -> bool,
) -> Vec<Step> {
    let mut out = Vec::new();
    for n in nodes {
        match hours_unfetched(n, now) {
            Some(hours) if !in_maintenance(n.node_id) => out.push(Step::Raise(n.node_id, hours)),
            Some(_) => {}
            None if open.contains(&n.node_id) => out.push(Step::Resolve(n.node_id)),
            None => {}
        }
    }
    for id in open {
        if !nodes.iter().any(|n| n.node_id == *id) {
            out.push(Step::Resolve(*id));
        }
    }
    out
}

/// The nodes whose alert is open now.
fn open_alerts(alerts: &AlertManager) -> Vec<Uuid> {
    alerts
        .active_alerts()
        .into_iter()
        .filter(|a| a.metric == CONFIG_STALE_METRIC)
        .filter_map(|a| match a.subject {
            Subject::Node(id) => Some(id.as_uuid()),
            Subject::Pool(_) | Subject::MerakiOrg(_) | Subject::ConfigRemote => None,
        })
        .collect()
}

/// One look: read, decide, act.
pub(crate) async fn tick(repo: &ConfigBackupRepo, alerts: &AlertManager, sink: &dyn AlertSink) {
    let nodes = match repo.stale_watch_nodes().await {
        Ok(nodes) => nodes,
        Err(e) => {
            tracing::warn!(error = %e, "config backup: could not read the nodes switched on");
            return;
        }
    };
    let now = Utc::now();
    let at = now.timestamp_millis();
    let open = open_alerts(alerts);
    for step in steps(&nodes, &open, now, |id| {
        alerts.in_maintenance(NodeId::from(id), at)
    }) {
        let action = match step {
            Step::Raise(id, hours) => alerts.raise_config_stale_alert(NodeId::from(id), hours, at),
            Step::Resolve(id) => alerts.resolve_config_stale_alert(NodeId::from(id)),
        };
        if let Some(action) = action {
            sink.dispatch(action).await;
        }
    }
}

/// The leader loop.
pub(crate) async fn run_stale_alert_watch(
    repo: Arc<ConfigBackupRepo>,
    alerts: Arc<AlertManager>,
    sink: Arc<dyn AlertSink>,
) {
    loop {
        tokio::time::sleep(WATCH_TICK).await;
        tick(&repo, &alerts, sink.as_ref()).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: &str = "2026-11-06T12:00:00Z";

    fn now() -> DateTime<Utc> {
        NOW.parse().expect("a time")
    }

    fn hours_ago(h: i64) -> DateTime<Utc> {
        now() - chrono::Duration::hours(h)
    }

    fn node(id: u128, saved_h: i64, success_h: Option<i64>) -> StaleWatchNode {
        StaleWatchNode {
            node_id: Uuid::from_u128(id),
            settings_saved_at: hours_ago(saved_h),
            last_success_at: success_h.map(hours_ago),
        }
    }

    #[test]
    fn a_node_is_stale_from_48_hours_after_its_last_fetch_that_worked() {
        assert_eq!(hours_unfetched(&node(1, 500, Some(47)), now()), None);
        assert_eq!(hours_unfetched(&node(1, 500, Some(48)), now()), Some(48.0));
        assert_eq!(hours_unfetched(&node(1, 1, Some(72)), now()), Some(72.0));
    }

    #[test]
    fn a_node_never_fetched_counts_from_when_its_settings_were_saved() {
        assert_eq!(hours_unfetched(&node(1, 47, None), now()), None);
        assert_eq!(hours_unfetched(&node(1, 49, None), now()), Some(49.0));
    }

    #[test]
    fn a_clock_behind_the_stored_times_is_not_stale() {
        let ahead = StaleWatchNode {
            node_id: Uuid::from_u128(1),
            settings_saved_at: now() + chrono::Duration::hours(1),
            last_success_at: None,
        };
        assert_eq!(hours_unfetched(&ahead, now()), None);
    }

    #[test]
    fn opens_the_stale_closes_the_fetched_and_leaves_the_rest() {
        let stale = node(1, 100, Some(60));
        let fetched = node(2, 100, Some(1));
        let quiet = node(3, 100, Some(2));
        let open = [Uuid::from_u128(2)];
        let got = steps(&[stale, fetched, quiet], &open, now(), |_| false);
        assert_eq!(
            got,
            vec![
                Step::Raise(Uuid::from_u128(1), 60.0),
                Step::Resolve(Uuid::from_u128(2))
            ]
        );
    }

    #[test]
    fn closes_a_node_switched_off_or_deleted() {
        let got = steps(&[], &[Uuid::from_u128(9)], now(), |_| false);
        assert_eq!(got, vec![Step::Resolve(Uuid::from_u128(9))]);
    }

    #[test]
    fn a_maintenance_window_holds_back_a_new_alert_and_keeps_an_open_one() {
        let stale = node(1, 100, Some(60));
        let in_window = |id: Uuid| id == Uuid::from_u128(1);
        assert!(steps(&[stale], &[], now(), in_window).is_empty());
        assert!(steps(&[stale], &[Uuid::from_u128(1)], now(), in_window).is_empty());
    }

    /// The Config tab says "not fetched for over 2 days" from the same moment this raises.
    #[test]
    fn the_config_tab_uses_the_same_limit() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../web/src/components/NodeDetail/configBackupForm.ts");
        let ts = std::fs::read_to_string(&path).expect("read configBackupForm.ts");
        let line = ts
            .lines()
            .find(|l| l.starts_with("export const STALE_AFTER_MS = "))
            .expect("STALE_AFTER_MS is declared on one line");
        let factors: Vec<u64> = line
            .trim_start_matches("export const STALE_AFTER_MS = ")
            .trim_end_matches(';')
            .split('*')
            .map(|f| f.trim().replace('_', "").parse().expect("a number"))
            .collect();
        let ms: u64 = factors.iter().product();
        assert_eq!(u128::from(ms), STALE_AFTER.as_millis());
    }
}
