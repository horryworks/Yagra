// SPDX-License-Identifier: AGPL-3.0-only
//! The last seven days replayed against today's grouping (ADR-236 decision 13): what the settings
//! screen shows beside each level, so a level is chosen on what it would have done rather than on
//! the outage that happens to be open now.
//!
//! The judgement is `yagra_topology::grouping::LinkGraph::replay`, which is pure. This file turns
//! history rows into outages and keeps the answer: it is computed on the first read and kept until
//! the graph is replaced or ten minutes pass, so opening the screen twice reads the history once.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use yagra_common::NodeId;
use yagra_topology::grouping::{LinkGraph, Outage, ReplaySummary};

use super::history::AlertHistoryStore;
use super::AlertManager;

/// How far back the replay reads.
pub(crate) const WINDOW_MS: i64 = 7 * 24 * 60 * 60 * 1_000;
/// The most history rows it reads. More than this and the answer says it was cut short.
pub(crate) const ROW_CAP: i64 = 200_000;
/// How long an answer is kept while nothing it depends on changes.
const MAX_AGE_MS: i64 = 10 * 60 * 1_000;

/// One replay's answer.
#[derive(Debug, Clone)]
pub(crate) struct Replayed {
    pub(crate) summary: ReplaySummary,
    /// More than [`ROW_CAP`] rows were recorded, so the newest were not read.
    pub(crate) truncated: bool,
}

/// The kept answer, with the graph it was computed against (held, so its address cannot be reused
/// by a later graph and mistaken for it) and when.
type Kept = (Arc<LinkGraph>, i64, Arc<Replayed>);
static KEPT: Mutex<Option<Kept>> = Mutex::new(None);

/// Each node's outages from its liveness rows (oldest first). A node whose first row in the window
/// is a resolve was already down when the window opened; one that has no row at all but is down
/// now has been down all along. Neither of those counts as a fire inside the window, so both start
/// just before it.
pub(crate) fn outages_from_rows(
    rows: &[(NodeId, i64, bool)],
    since_ms: i64,
    down_now: &BTreeSet<NodeId>,
) -> Vec<Outage> {
    let before = since_ms - 1;
    let mut open: BTreeMap<NodeId, Option<i64>> = BTreeMap::new();
    let mut out = Vec::new();
    for &(node, at, resolved) in rows {
        let seen = open.contains_key(&node);
        let slot = open.entry(node).or_insert(None);
        match (resolved, *slot) {
            // A second fire while open (a re-fire) adds nothing.
            (false, Some(_)) => {}
            (false, None) => *slot = Some(at),
            (true, Some(from)) => {
                out.push(Outage {
                    node,
                    from_ms: from,
                    to_ms: Some(at),
                });
                *slot = None;
            }
            (true, None) if !seen => out.push(Outage {
                node,
                from_ms: before,
                to_ms: Some(at),
            }),
            // A resolve with nothing open after the node's first row: nothing to close.
            (true, None) => {}
        }
    }
    for (&node, from) in &open {
        if let Some(from) = from {
            out.push(Outage {
                node,
                from_ms: *from,
                to_ms: None,
            });
        }
    }
    for &node in down_now {
        if !open.contains_key(&node) {
            out.push(Outage {
                node,
                from_ms: before,
                to_ms: None,
            });
        }
    }
    out
}

/// The last seven days, replayed — from the kept answer when nothing it depends on has changed.
pub(crate) async fn last_7_days(
    alerts: &AlertManager,
    history: &AlertHistoryStore,
) -> anyhow::Result<Arc<Replayed>> {
    let graph = alerts.grouping_graph();
    let now = yagra_common::clock::now_unix_ms();
    {
        let kept = KEPT
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((g, at, r)) = kept.as_ref() {
            if Arc::ptr_eq(g, &graph) && now - at < MAX_AGE_MS {
                return Ok(r.clone());
            }
        }
    }
    let since = now - WINDOW_MS;
    let mut rows = history.liveness_since(since, ROW_CAP + 1).await?;
    let truncated = rows.len() > usize::try_from(ROW_CAP).unwrap_or(usize::MAX);
    rows.truncate(usize::try_from(ROW_CAP).unwrap_or(usize::MAX));
    let rows: Vec<(NodeId, i64, bool)> = rows
        .into_iter()
        .map(|(n, at, r)| (NodeId::from(n), at, r))
        .collect();
    let outages = outages_from_rows(&rows, since, &alerts.down_set());
    let replayed = Arc::new(Replayed {
        summary: alerts.replay_grouping(&outages, since),
        truncated,
    });
    *KEPT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some((graph, now, replayed.clone()));
    Ok(replayed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn id(n: u128) -> NodeId {
        NodeId::from(Uuid::from_u128(n))
    }

    #[test]
    fn rows_become_outages_including_the_ones_open_at_either_edge() {
        let (a, b, c, d) = (id(1), id(2), id(3), id(4));
        let rows = [
            (b, 150, true),  // b was down when the window opened
            (a, 200, false), // a fires
            (a, 200, false), // and re-fires: nothing new
            (a, 300, true),
            (a, 400, false), // a again, still open
            (b, 500, true),  // a stray resolve after b's first row closes nothing
        ];
        let down_now: BTreeSet<NodeId> = [a, c].into_iter().collect();
        let mut got = outages_from_rows(&rows, 100, &down_now);
        got.sort_by_key(|o| (o.node, o.from_ms));
        assert_eq!(
            got,
            vec![
                Outage {
                    node: a,
                    from_ms: 200,
                    to_ms: Some(300)
                },
                Outage {
                    node: a,
                    from_ms: 400,
                    to_ms: None
                },
                Outage {
                    node: b,
                    from_ms: 99,
                    to_ms: Some(150)
                },
                // c has no row and is down: down all week.
                Outage {
                    node: c,
                    from_ms: 99,
                    to_ms: None
                },
            ]
        );
        assert!(!got.iter().any(|o| o.node == d));
    }
    #[sqlx::test(migrator = "yagra_base::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn the_week_is_read_from_the_liveness_rows_alone(pool: sqlx::PgPool) {
        use yagra_alert::rules::LIVENESS;
        use yagra_alert::testkit::open_alert;
        use yagra_common::NodeState;
        let store = AlertHistoryStore::new(pool);
        let node = NodeId::new();
        let down = open_alert(node, LIVENESS, NodeState::Unreachable);
        store.record(&down, true).await.expect("resolve");
        store.record(&down, false).await.expect("fire");
        // A threshold alert on the same node is not an outage.
        let cpu = open_alert(node, "cpu_util", NodeState::Warning);
        store.record(&cpu, false).await.expect("cpu");
        let rows = store.liveness_since(0, 10).await.expect("read");
        let at = down.at_unix_ms;
        assert_eq!(
            rows,
            vec![(node.as_uuid(), at, false), (node.as_uuid(), at, true)],
            "liveness only, a fire before a resolve at the same instant"
        );
        assert!(store
            .liveness_since(at + 1, 10)
            .await
            .expect("read")
            .is_empty());
        assert_eq!(store.liveness_since(0, 1).await.expect("read").len(), 1);
    }
}
