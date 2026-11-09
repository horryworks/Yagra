// SPDX-License-Identifier: AGPL-3.0-only
//! Investigate over a whole folder (ADR-251 decision 12): the wide, shallow first pass.
//!
//! Judging one node reads about forty ranges, so a folder of sixty would be thousands — too many
//! to answer in one request. This file reads two instants per metric instead (now, and the same
//! time a week ago), one query per ~200 nodes, and ranks every node by where that pair points in
//! 30 days. The caller then judges only the top [`DEEP_MAX`] with the single-node question, so
//! every verdict a folder answer carries is the same verdict that node's own page would show.
//!
//! ⚠️ What two instants cannot see, and the answer therefore counts rather than hides: something
//! that rose and fell back inside the week, a node whose target line is below [`SCREEN_FLOOR_PCT`]
//! and that is not among the busiest, and a link that is only busy at another time of day (the link
//! half reads the last five minutes).

use super::investigate::{InvestigateReport, Question, RUNNING_OUT_METRICS};
use super::*;
use crate::tsdb::store::InterfaceTopMetric;
use yagra_common::derived_metric::{derived_node_metric, Formula};

/// How far back the second instant is: a week, so the two land at the same hour of the same
/// weekday and a daily or weekly cycle does not read as growth.
pub(super) const SCREEN_OFFSET_SECS: u64 = 7 * 86_400;
/// The projected level, in percent, below which a node is not judged further.
pub const SCREEN_FLOOR_PCT: f64 = 50.0;
/// The most nodes judged in full for one folder answer.
pub const DEEP_MAX: usize = 20;
/// A port is a link candidate when its traffic reaches this share of the folder's slowest port.
pub const LINK_FLOOR_SHARE: f64 = SCREEN_FLOOR_PCT / 100.0;

/// What the first pass found.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Screen {
    /// Nodes that answered at least one of the metrics read.
    pub nodes_with_data: HashSet<Uuid>,
    /// Per node, the highest level any of its series is projected to in 30 days, in percent.
    pub pressure: HashMap<Uuid, f64>,
    /// Ports whose current traffic crossed the floor, as `(node, ifindex, bits/sec)`, the busier
    /// direction of each.
    pub busy_ports: Vec<(Uuid, i32, f64)>,
    /// Whether ports were read at all: false when no port in the folder has a usable speed, or
    /// the store did not answer.
    pub links_screened: bool,
}

/// Where a pair of readings a week apart points in 30 days. A falling or missing week-ago value
/// projects the current one.
#[must_use]
pub fn projected_pct(now: f64, week_ago: Option<f64>) -> f64 {
    let per_day = week_ago.map_or(0.0, |b| ((now - b) / 7.0).max(0.0));
    now + per_day * RUNNING_OUT_HORIZON_DAYS
}

/// The nodes worth judging in full, busiest first: each node's projected level, raised by its
/// busiest port's utilisation, kept when it reaches [`SCREEN_FLOOR_PCT`]. A port with no usable
/// speed is skipped.
#[must_use]
pub fn rank_candidates(
    screen: &Screen,
    speeds: &HashMap<(Uuid, i32), Option<i64>>,
) -> Vec<(Uuid, f64)> {
    let mut level = screen.pressure.clone();
    for &(node, ifindex, bps) in &screen.busy_ports {
        let Some(pct) = speeds
            .get(&(node, ifindex))
            .and_then(|s| crate::tsdb::interface_util::utilisation_pct(bps, *s))
        else {
            continue;
        };
        let e = level.entry(node).or_insert(pct);
        *e = e.max(pct);
    }
    let mut out: Vec<(Uuid, f64)> = level
        .into_iter()
        .filter(|(_, v)| *v >= SCREEN_FLOOR_PCT)
        .collect();
    out.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    out
}

/// The answer to one question about a folder.
#[derive(Debug, Clone, PartialEq, Serialize, utoipa::ToSchema)]
pub struct FolderInvestigateReport {
    pub question: Question,
    pub group_id: Uuid,
    /// The window each judged node was read over, Unix seconds.
    pub from: i64,
    /// Unix seconds.
    pub to: i64,
    /// Nodes in the folder and every folder beneath it.
    pub nodes_total: usize,
    /// Nodes that reported any memory, disk or CPU percentage in the last hour.
    pub nodes_with_data: usize,
    /// Nodes the first pass put at or above 50% in 30 days.
    pub candidates: usize,
    /// Candidates judged in full — the busiest, at most 20.
    pub judged: usize,
    /// Candidates not judged because 20 were. Never read these as fine.
    pub not_judged: usize,
    /// Whether ports were part of the first pass. False when no port in the folder has a usable
    /// speed, or the store did not answer.
    pub links_screened: bool,
    /// The judged nodes, those with a hit first.
    pub nodes: Vec<InvestigateReport>,
}

impl FolderInvestigateReport {
    /// Put the judged nodes in the order the answer lists them: a hit first, then an unknown,
    /// then the rest, keeping the first pass's order inside each.
    pub fn sort_nodes(&mut self) {
        let rank = |r: &InvestigateReport| {
            let verdicts = || r.hypotheses.iter().map(|h| h.verdict);
            if verdicts().any(|v| v == Verdict::Hit) {
                0
            } else if verdicts().any(|v| v == Verdict::Unknown) {
                1
            } else {
                2
            }
        };
        self.nodes.sort_by_key(rank);
    }
}

impl Engine {
    /// The first pass over `nodes` (ADR-251 decision 12). `link_floor_bps` is the traffic above
    /// which a port is worth a look; `None` skips ports.
    pub(super) async fn screen_running_out(
        &self,
        nodes: &[Uuid],
        link_floor_bps: Option<f64>,
    ) -> Screen {
        let mut screen = Screen::default();
        if nodes.is_empty() {
            return screen;
        }
        // Every name read, once: the collected percentages and both inputs of each derived one.
        let mut names: BTreeSet<&'static str> = BTreeSet::new();
        for (metric, _) in RUNNING_OUT_METRICS {
            match derived_node_metric(metric) {
                Some(d) => names.extend(d.formula.inputs()),
                None => {
                    names.insert(metric);
                }
            }
        }
        let mut now: HashMap<&str, HashMap<(Uuid, i64), f64>> = HashMap::new();
        let mut before: HashMap<&str, HashMap<(Uuid, i64), f64>> = HashMap::new();
        for name in names {
            let n = self.store.rows_at_offset(name, nodes, 0).await;
            if n.is_empty() {
                continue;
            }
            before.insert(
                name,
                self.store
                    .rows_at_offset(name, nodes, SCREEN_OFFSET_SECS)
                    .await,
            );
            now.insert(name, n);
        }

        let empty = HashMap::new();
        for (metric, _hypothesis) in RUNNING_OUT_METRICS {
            let levels = match derived_node_metric(metric) {
                None => pair_levels(
                    now.get(metric).unwrap_or(&empty),
                    before.get(metric).unwrap_or(&empty),
                ),
                Some(d) => {
                    let [a, b] = d.formula.inputs();
                    derived_levels(
                        &d.formula,
                        [now.get(a).unwrap_or(&empty), now.get(b).unwrap_or(&empty)],
                        [
                            before.get(a).unwrap_or(&empty),
                            before.get(b).unwrap_or(&empty),
                        ],
                    )
                }
            };
            for ((node, _row), (n, b)) in levels {
                screen.nodes_with_data.insert(node);
                let p = projected_pct(n, b);
                let e = screen.pressure.entry(node).or_insert(p);
                *e = e.max(p);
            }
        }

        if let Some(floor) = link_floor_bps {
            let mut busiest: HashMap<(Uuid, i32), f64> = HashMap::new();
            let mut answered = true;
            for dir in [InterfaceTopMetric::InBps, InterfaceTopMetric::OutBps] {
                match self
                    .store
                    .interface_candidates(dir, floor, Some(nodes))
                    .await
                {
                    Some(ports) => {
                        for (node, ifindex, bps) in ports {
                            let e = busiest.entry((node, ifindex)).or_insert(bps);
                            *e = e.max(bps);
                        }
                    }
                    None => answered = false,
                }
            }
            screen.links_screened = answered;
            screen.busy_ports = busiest
                .into_iter()
                .map(|((node, ifindex), bps)| (node, ifindex, bps))
                .collect();
            screen
                .busy_ports
                .sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
        }
        screen
    }
}

/// A series' row, as the store keys it.
type RowKey = (Uuid, i64);
/// A row's value now, and a week ago when it had one.
type Level = (f64, Option<f64>);

/// A collected percentage's rows, each with its week-ago value when it had one.
fn pair_levels(
    now: &HashMap<(Uuid, i64), f64>,
    before: &HashMap<(Uuid, i64), f64>,
) -> Vec<(RowKey, Level)> {
    now.iter()
        .map(|(k, v)| (*k, (*v, before.get(k).copied())))
        .collect()
}

/// A derived percentage computed row by row from its two inputs, at both instants — never one
/// row's numerator over another row's denominator (`tsdb/derived.rs`). A shape whose denominator is
/// a count of rows rather than a row's value is not paired here and contributes nothing.
fn derived_levels(
    formula: &Formula,
    now: [&HashMap<(Uuid, i64), f64>; 2],
    before: [&HashMap<(Uuid, i64), f64>; 2],
) -> Vec<(RowKey, Level)> {
    match formula {
        Formula::PerSeriesCount { .. } => return Vec::new(),
        Formula::PercentOf { .. }
        | Formula::PercentOfSum { .. }
        | Formula::PercentUsedOfTotal { .. }
        | Formula::Complement { .. } => {}
    }
    let at = |side: [&HashMap<(Uuid, i64), f64>; 2], k: &(Uuid, i64)| {
        formula.apply(*side[0].get(k)?, *side[1].get(k)?)
    };
    now[0]
        .keys()
        .filter_map(|k| Some((*k, (at(now, k)?, at(before, k)))))
        .collect()
}

/// Every hypothesis the first pass reads, so a test can see none is left out of it.
#[cfg(test)]
pub(super) fn screened_hypotheses() -> HashSet<super::investigate::Hypothesis> {
    RUNNING_OUT_METRICS.iter().map(|(_, h)| *h).collect()
}

#[cfg(test)]
mod tests {
    use super::super::investigate::Hypothesis;
    use super::super::testkit::Harness;
    use super::*;
    use crate::tsdb::store::PortDirection;

    fn pt(t: i64, v: f64) -> MetricPoint {
        MetricPoint { t, v }
    }

    const DAY: i64 = 86_400;

    #[test]
    fn a_week_of_growth_is_carried_thirty_days_forward_and_a_fall_is_not() {
        assert!((projected_pct(60.0, Some(53.0)) - 90.0).abs() < 1e-9);
        assert!((projected_pct(60.0, Some(70.0)) - 60.0).abs() < 1e-9);
        assert!((projected_pct(60.0, None) - 60.0).abs() < 1e-9);
    }

    #[test]
    fn candidates_are_ranked_busiest_first_and_a_quiet_node_is_left_out() {
        let (a, b, c) = (Uuid::from_u128(1), Uuid::from_u128(2), Uuid::from_u128(3));
        let screen = Screen {
            nodes_with_data: [a, b, c].into(),
            pressure: [(a, 70.0), (b, 40.0), (c, 95.0)].into(),
            busy_ports: vec![(b, 5, 800e6), (b, 6, 800e6)],
            links_screened: true,
        };
        let speeds = [((b, 5), Some(1_000_000_000_i64)), ((b, 6), None)].into();
        let ranked = rank_candidates(&screen, &speeds);
        assert_eq!(
            ranked.iter().map(|(n, _)| *n).collect::<Vec<_>>(),
            vec![c, b, a],
            "b rises to 80% through its port; the port with no speed is skipped"
        );
        let quiet = Screen {
            pressure: [(a, 49.9)].into(),
            ..Screen::default()
        };
        assert!(rank_candidates(&quiet, &HashMap::new()).is_empty());
    }

    #[tokio::test]
    async fn the_first_pass_reads_two_instants_and_divides_row_by_row() {
        let h = Harness::new();
        let m = &h.metrics;
        let (a, b) = (Uuid::from_u128(1), Uuid::from_u128(2));
        let now = 100 * DAY;
        // Node a: memory climbing 1 point a day, from 50 to 57 over the week.
        m.series(
            a,
            "huawei_mem_usage",
            vec![pt(now - 7 * DAY, 50.0), pt(now, 57.0)],
        );
        // Node b: two filesystems. `/` is 90% full and flat, `/boot` small and empty — a
        // max-over-max division would mix them.
        m.row_series(
            b,
            "hr_storage_used",
            1,
            vec![pt(now - 7 * DAY, 90.0), pt(now, 90.0)],
        );
        m.row_series(
            b,
            "hr_storage_size",
            1,
            vec![pt(now - 7 * DAY, 100.0), pt(now, 100.0)],
        );
        m.row_series(
            b,
            "hr_storage_used",
            2,
            vec![pt(now - 7 * DAY, 1.0), pt(now, 1.0)],
        );
        m.row_series(
            b,
            "hr_storage_size",
            2,
            vec![pt(now - 7 * DAY, 1000.0), pt(now, 1000.0)],
        );
        let e = h.engine();
        let s = e.screen_running_out(&[a, b], None).await;
        assert_eq!(s.nodes_with_data, [a, b].into());
        assert!((s.pressure[&a] - 87.0).abs() < 1e-9, "57 + 1/day x 30");
        assert!((s.pressure[&b] - 90.0).abs() < 1e-9, "/ at 90%, flat");
        assert!(!s.links_screened, "no floor, no port read");
    }

    #[tokio::test]
    async fn ports_over_the_floor_are_kept_with_their_busier_direction() {
        let h = Harness::new();
        let m = &h.metrics;
        let a = Uuid::from_u128(1);
        m.candidate(a, 3, PortDirection::In, 600e6);
        m.candidate(a, 3, PortDirection::Out, 700e6);
        m.candidate(a, 4, PortDirection::In, 10e6);
        let e = h.engine();
        let s = e.screen_running_out(&[a], Some(500e6)).await;
        assert!(s.links_screened);
        assert_eq!(s.busy_ports, vec![(a, 3, 700e6)]);
    }

    #[test]
    fn every_hypothesis_but_links_is_read_from_a_metric() {
        let mut want: HashSet<Hypothesis> = Hypothesis::ALL.into_iter().collect();
        want.remove(&Hypothesis::LinkBandwidth);
        assert_eq!(screened_hypotheses(), want);
    }

    #[test]
    fn hits_are_listed_first() {
        let node = |n: u128, v: Verdict| InvestigateReport {
            question: Question::RunningOut,
            node_id: Uuid::from_u128(n),
            from: 0,
            to: 1,
            hypotheses: vec![super::super::investigate::HypothesisResult {
                hypothesis: Hypothesis::Memory,
                verdict: v,
                evidence: Vec::new(),
            }],
            monitoring_gaps: Vec::new(),
        };
        let mut r = FolderInvestigateReport {
            question: Question::RunningOut,
            group_id: Uuid::nil(),
            from: 0,
            to: 1,
            nodes_total: 3,
            nodes_with_data: 3,
            candidates: 3,
            judged: 3,
            not_judged: 0,
            links_screened: true,
            nodes: vec![
                node(1, Verdict::Miss),
                node(2, Verdict::Unknown),
                node(3, Verdict::Hit),
            ],
        };
        r.sort_nodes();
        assert_eq!(
            r.nodes
                .iter()
                .map(|n| n.node_id.as_u128())
                .collect::<Vec<_>>(),
            vec![3, 2, 1]
        );
    }
}
