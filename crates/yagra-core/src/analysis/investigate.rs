// SPDX-License-Identifier: AGPL-3.0-only
//! Investigate: confirm a hunch about one node (ADR-251).
//!
//! The analyses beside this file *search* — a job, a scope, a list of scored findings. This file
//! *confirms*: one node, one question, and per hypothesis a verdict of hit, miss or unknown with
//! the numbers behind it. It shares the analyses' reads ([`Engine`]) and their arithmetic
//! (`stats.rs`), never a copy of either, and it is not an [`AnalysisTool`]: it has no job row, no
//! progress and no queue, because it answers in one request.
//!
//! What it does not read itself — the node's threshold rules, its ports and their speeds, its
//! pool's monitoring gaps — the caller gathers and hands in as [`InvestigateInput`], so every
//! decision here runs in a test with no database.

use super::*;
use crate::tsdb::store::PortDirection;
use yagra_common::derived_metric::{derived_node_metric, DerivedMetric};
use yagra_common::IfIndex;

/// What is being asked. One question in Inc.1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Question {
    /// Is anything on this node running out — memory, disk, CPU or a link's bandwidth?
    RunningOut,
}

/// One thing a question checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Hypothesis {
    Memory,
    Disk,
    Cpu,
    LinkBandwidth,
}

impl Hypothesis {
    /// Every hypothesis of [`Question::RunningOut`], in the order the report lists them.
    pub const ALL: [Self; 4] = [Self::Memory, Self::Disk, Self::Cpu, Self::LinkBandwidth];
}

/// The answer for one series, and for one hypothesis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// The hunch is right.
    Hit,
    /// Checked, and it is not.
    Miss,
    /// Not enough data to say. Never read as "fine".
    Unknown,
}

/// Why a series got its verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    /// The latest value is already at or above the target line.
    AlreadyOver,
    /// The daily peaks reach the target line within 30 days.
    Reaches,
    /// It climbs, falls at a restart, and climbs again.
    Leak,
    /// The daily peaks do not reach the target line within 30 days.
    NotReaching,
    /// Fewer samples than any trend can be drawn from.
    TooFewPoints,
    /// Fewer than seven days of samples.
    TooFewDays,
}

/// One series behind a hypothesis.
#[derive(Debug, Clone, PartialEq, Serialize, utoipa::ToSchema)]
pub struct Evidence {
    /// The metric read (a derived one is computed from its two inputs).
    pub metric: String,
    /// The table row (filesystem, memory pool) the series is for, when the metric has rows.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub row: Option<i64>,
    /// The port, for a link.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ifindex: Option<i32>,
    /// The port's name, for a link.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub verdict: Verdict,
    pub reason: Reason,
    /// The latest value, in percent.
    pub current: Option<f64>,
    /// The line the series is judged against, in percent: the warning bound in force for this
    /// node and metric, else the critical one, else 100.
    pub target: f64,
    /// How fast the series climbs, in percentage points per day.
    pub slope_per_day: Option<f64>,
    /// Days until the series reaches the target, when it is climbing.
    pub days_to_target: Option<f64>,
}

/// One hypothesis and the series it was judged from. No evidence means the node has no metric for
/// it, and the verdict is then unknown.
#[derive(Debug, Clone, PartialEq, Serialize, utoipa::ToSchema)]
pub struct HypothesisResult {
    pub hypothesis: Hypothesis,
    pub verdict: Verdict,
    pub evidence: Vec<Evidence>,
}

/// A span in which the node's pool had no live poller, so missing samples are missing collection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct GapSpan {
    /// Unix milliseconds.
    pub from_ms: i64,
    /// Unix milliseconds.
    pub to_ms: i64,
}

/// The answer to one question about one node.
#[derive(Debug, Clone, PartialEq, Serialize, utoipa::ToSchema)]
pub struct InvestigateReport {
    pub question: Question,
    pub node_id: Uuid,
    /// The window read, Unix seconds.
    pub from: i64,
    /// Unix seconds.
    pub to: i64,
    pub hypotheses: Vec<HypothesisResult>,
    /// Where collection itself stopped inside the window.
    pub monitoring_gaps: Vec<GapSpan>,
}

/// A port as the caller found it in the inventory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortInfo {
    pub ifindex: i32,
    pub name: Option<String>,
    pub speed_bps: Option<i64>,
}

/// Everything [`Engine::investigate_running_out`] does not read from the metric store itself.
#[derive(Debug, Clone, Default)]
pub struct InvestigateInput {
    pub node_id: Uuid,
    pub days: u32,
    pub now_s: i64,
    /// The warning (else critical) upper bound in force for each node-level metric.
    pub targets: HashMap<String, f64>,
    pub ports: Vec<PortInfo>,
    /// Unix milliseconds.
    pub monitoring_gaps: Vec<(i64, i64)>,
}

/// The internal judgement of one series, before it is labelled.
pub(super) struct Judgement {
    pub(super) verdict: Verdict,
    pub(super) reason: Reason,
    pub(super) current: Option<f64>,
    pub(super) slope_per_day: Option<f64>,
    pub(super) days_to_target: Option<f64>,
}

/// The percentage metrics each hypothesis reads, collected or derived. A name here that the
/// built-in templates do not collect fails `every_running_out_metric_is_a_catalog_or_derived_metric`.
pub(super) const RUNNING_OUT_METRICS: &[(&str, Hypothesis)] = &[
    ("cisco_cpu_5min", Hypothesis::Cpu),
    ("huawei_cpu_usage", Hypothesis::Cpu),
    ("nxos_cpu_util", Hypothesis::Cpu),
    ("fortinet_cpu_usage", Hypothesis::Cpu),
    ("juniper_cpu_1min", Hypothesis::Cpu),
    ("hr_processor_load", Hypothesis::Cpu),
    (yagra_common::wlan::METRIC_WLAN_AP_CPU_PCT, Hypothesis::Cpu),
    ("ucd_cpu_used_pct", Hypothesis::Cpu),
    ("huawei_mem_usage", Hypothesis::Memory),
    ("nxos_mem_util", Hypothesis::Memory),
    ("fortinet_mem_usage", Hypothesis::Memory),
    (
        yagra_common::wlan::METRIC_WLAN_AP_MEM_PCT,
        Hypothesis::Memory,
    ),
    ("cisco_mem_used_pct", Hypothesis::Memory),
    ("cisco_cemp_mem_used_pct", Hypothesis::Memory),
    ("cisco_cpu_mem_used_pct", Hypothesis::Memory),
    ("huawei_mem_used_pct", Hypothesis::Memory),
    ("ucd_mem_used_pct", Hypothesis::Memory),
    ("ucd_disk_used_pct", Hypothesis::Disk),
    ("hr_storage_used_pct", Hypothesis::Disk),
];

/// The busiest ports a link hypothesis reads, so a 48-port switch costs 16 range reads, not 96.
pub(super) const LINK_PORTS_MAX: usize = 8;
/// The most rows of one table metric read.
pub(super) const ROWS_MAX: usize = 16;
/// The most evidence lines one hypothesis reports.
pub(super) const EVIDENCE_MAX: usize = 12;
/// The metric whose bound, when a rule sets one, is a link's target line.
pub(super) const LINK_TARGET_METRIC: &str = "if_in_util_pct";

impl Engine {
    /// Is anything on this node running out? (ADR-251 Inc.1)
    pub(super) async fn investigate_running_out(
        &self,
        input: &InvestigateInput,
    ) -> InvestigateReport {
        let node = input.node_id;
        let to = input.now_s;
        let from = to - i64::from(input.days.max(1)) * 86_400;
        let step = read_step(from, to);
        let step_s = i64::try_from(step).unwrap_or(i64::MAX);
        let within = u64::try_from(to - from).unwrap_or(0);
        let names: HashSet<String> = self
            .store
            .node_metric_names(node, within)
            .await
            .into_iter()
            .collect();
        let restarts = restart_times(
            &self
                .gauge_range(node, yagra_common::config_unsaved::UPTIME, from, to, step)
                .await,
        );

        let mut hypotheses = Vec::with_capacity(Hypothesis::ALL.len());
        for hypothesis in Hypothesis::ALL {
            let mut evidence = match hypothesis {
                Hypothesis::Memory | Hypothesis::Disk | Hypothesis::Cpu => {
                    let mut out = Vec::new();
                    for (metric, _) in RUNNING_OUT_METRICS.iter().filter(|(_, h)| *h == hypothesis)
                    {
                        let target = input.targets.get(*metric).copied().unwrap_or(100.0);
                        for (row, pts) in self
                            .percent_series(node, metric, &names, from, to, step, within)
                            .await
                        {
                            let j = judge_running_out(&pts, target, &restarts, step_s);
                            out.push(evidence_of(metric, row, None, None, target, j));
                        }
                    }
                    out
                }
                Hypothesis::LinkBandwidth => {
                    self.link_evidence(input, from, to, step, &restarts).await
                }
            };
            evidence.sort_by(|a, b| {
                verdict_rank(a.verdict).cmp(&verdict_rank(b.verdict)).then(
                    a.days_to_target
                        .unwrap_or(f64::MAX)
                        .total_cmp(&b.days_to_target.unwrap_or(f64::MAX)),
                )
            });
            evidence.truncate(EVIDENCE_MAX);
            hypotheses.push(HypothesisResult {
                hypothesis,
                verdict: combine_verdicts(evidence.iter().map(|e| e.verdict)),
                evidence,
            });
        }

        InvestigateReport {
            question: Question::RunningOut,
            node_id: node,
            from,
            to,
            hypotheses,
            monitoring_gaps: input
                .monitoring_gaps
                .iter()
                .map(|&(from_ms, to_ms)| GapSpan { from_ms, to_ms })
                .collect(),
        }
    }

    /// One percentage metric as series: a collected one read as the node aggregate, a derived one
    /// computed from its inputs — row by row when it has rows, never `max / max` (which divides a
    /// small filesystem's usage by a large one's size, `tsdb/derived.rs`). Empty when the node has
    /// none of it.
    #[allow(clippy::too_many_arguments)] // Every argument is one fact about one read; a struct would only rename them.
    async fn percent_series(
        &self,
        node: Uuid,
        metric: &str,
        names: &HashSet<String>,
        from: i64,
        to: i64,
        step: u64,
        within: u64,
    ) -> Vec<(Option<i64>, Vec<MetricPoint>)> {
        let Some(derived) = derived_node_metric(metric) else {
            if !names.contains(metric) {
                return Vec::new();
            }
            return vec![(None, self.gauge_range(node, metric, from, to, step).await)];
        };
        let [a, b] = derived.formula.inputs();
        if !names.contains(a) || !names.contains(b) {
            return Vec::new();
        }
        if !derived.per_row {
            let pa = self.gauge_range(node, a, from, to, step).await;
            let pb = if a == b {
                pa.clone()
            } else {
                self.gauge_range(node, b, from, to, step).await
            };
            return vec![(None, derive_series(&derived.formula, &pa, &pb))];
        }
        self.derived_rows(node, derived, from, to, step, within)
            .await
    }

    async fn derived_rows(
        &self,
        node: Uuid,
        derived: &DerivedMetric,
        from: i64,
        to: i64,
        step: u64,
        within: u64,
    ) -> Vec<(Option<i64>, Vec<MetricPoint>)> {
        let [a, b] = derived.formula.inputs();
        let rows: BTreeSet<i64> = self
            .store
            .series_rows(a, Some(&[node]), within)
            .await
            .into_keys()
            .filter(|(n, _)| *n == node)
            .map(|(_, row)| row)
            .collect();
        let mut out = Vec::new();
        for row in rows.into_iter().take(ROWS_MAX) {
            let Ok(index) = u32::try_from(row) else {
                continue;
            };
            let key = |m: &str| SeriesKey::interface(NodeId::from(node), IfIndex(index), m);
            let pa = self.store.range(&key(a), from, to, step).await;
            let pb = if a == b {
                pa.clone()
            } else {
                self.store.range(&key(b), from, to, step).await
            };
            out.push((Some(row), derive_series(&derived.formula, &pa, &pb)));
        }
        out
    }

    /// The busiest ports with a known speed, each judged on the higher of its two directions.
    async fn link_evidence(
        &self,
        input: &InvestigateInput,
        from: i64,
        to: i64,
        step: u64,
        restarts: &[i64],
    ) -> Vec<Evidence> {
        let node = input.node_id;
        let live = self.store.node_interface_live(node, 300).await;
        let mut busiest: Vec<(f64, &PortInfo)> = input
            .ports
            .iter()
            .filter(|p| p.speed_bps.is_some_and(|s| s > 0))
            .filter_map(|p| {
                let l = live.get(&p.ifindex)?;
                let bytes = l.in_bps.unwrap_or(0.0).max(l.out_bps.unwrap_or(0.0));
                let now = crate::tsdb::interface_util::utilisation_pct(bytes * 8.0, p.speed_bps)?;
                Some((now, p))
            })
            .collect();
        busiest.sort_by(|a, b| b.0.total_cmp(&a.0));
        let target = input
            .targets
            .get(LINK_TARGET_METRIC)
            .copied()
            .unwrap_or(100.0);
        let step_s = i64::try_from(step).unwrap_or(i64::MAX);
        let lookback = step.max(300);
        let mut out = Vec::new();
        for (_, port) in busiest.into_iter().take(LINK_PORTS_MAX) {
            let key = (node, port.ifindex);
            let pin = self
                .store
                .interface_bps_range(key, PortDirection::In, from, to, step, lookback)
                .await;
            let pout = self
                .store
                .interface_bps_range(key, PortDirection::Out, from, to, step, lookback)
                .await;
            let pts = port_utilisation(&pin, &pout, port.speed_bps);
            let j = judge_running_out(&pts, target, restarts, step_s);
            out.push(evidence_of(
                LINK_TARGET_METRIC,
                None,
                Some(port.ifindex),
                port.name.clone(),
                target,
                j,
            ));
        }
        out
    }
}

fn evidence_of(
    metric: &str,
    row: Option<i64>,
    ifindex: Option<i32>,
    label: Option<String>,
    target: f64,
    j: Judgement,
) -> Evidence {
    Evidence {
        metric: metric.to_owned(),
        row,
        ifindex,
        label,
        verdict: j.verdict,
        reason: j.reason,
        current: j.current,
        target,
        slope_per_day: j.slope_per_day,
        days_to_target: j.days_to_target,
    }
}

const fn verdict_rank(v: Verdict) -> u8 {
    match v {
        Verdict::Hit => 0,
        Verdict::Miss => 1,
        Verdict::Unknown => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::super::testkit::*;
    use super::*;
    use crate::tsdb::store::InterfaceLive;

    const DAY: i64 = 86_400;
    const NOW: i64 = 1_800_000_000 - (1_800_000_000 % DAY);

    /// Thirty days, six samples a day, of `f(day, sample)`.
    fn month(f: impl Fn(i64, i64) -> f64) -> Vec<MetricPoint> {
        (0..30)
            .flat_map(|d| (0..6).map(move |s| (d, s)))
            .map(|(d, s)| MetricPoint {
                t: NOW - 30 * DAY + d * DAY + s * 4 * 3600,
                v: f(d, s),
            })
            .collect()
    }

    fn input(node: Uuid) -> InvestigateInput {
        InvestigateInput {
            node_id: node,
            days: 30,
            now_s: NOW,
            ..InvestigateInput::default()
        }
    }

    fn result(r: &InvestigateReport, h: Hypothesis) -> &HypothesisResult {
        r.hypotheses
            .iter()
            .find(|x| x.hypothesis == h)
            .expect("every hypothesis is reported")
    }

    #[test]
    fn every_running_out_metric_is_a_catalog_or_derived_metric() {
        let collected: HashSet<String> = yagra_common::collection::builtin_templates()
            .into_iter()
            .flat_map(|t| t.items)
            .map(|i| i.metric_name)
            .chain([
                yagra_common::wlan::METRIC_WLAN_AP_CPU_PCT.to_owned(),
                yagra_common::wlan::METRIC_WLAN_AP_MEM_PCT.to_owned(),
            ])
            .collect();
        for (name, _) in RUNNING_OUT_METRICS {
            assert!(
                collected.contains(*name) || derived_node_metric(name).is_some(),
                "{name} is neither collected by a built-in template nor derived"
            );
        }
        for h in [Hypothesis::Memory, Hypothesis::Disk, Hypothesis::Cpu] {
            assert!(
                RUNNING_OUT_METRICS.iter().any(|(_, x)| *x == h),
                "{h:?} reads no metric"
            );
        }
    }

    #[tokio::test]
    async fn a_flat_memory_is_a_miss() {
        let h = Harness::new();
        let node = h.inventory.node(1, "sw-01");
        h.metrics
            .series(node, "huawei_mem_usage", month(|_, s| 40.0 + s as f64));
        let r = h.engine().investigate_running_out(&input(node)).await;
        let mem = result(&r, Hypothesis::Memory);
        assert_eq!(mem.verdict, Verdict::Miss);
        assert_eq!(mem.evidence[0].reason, Reason::NotReaching);
        assert_eq!(mem.evidence[0].target, 100.0);
    }

    #[tokio::test]
    async fn a_disk_reaching_its_warning_bound_in_about_twenty_days_is_a_hit() {
        let h = Harness::new();
        let node = h.inventory.node(1, "srv-01");
        // Peaks climb 1 point a day and end at 60; the warning bound is 80.
        h.metrics.series(
            node,
            "ucd_disk_used_pct",
            month(|d, s| 31.0 + d as f64 - s as f64 * 0.1),
        );
        let mut i = input(node);
        i.targets.insert("ucd_disk_used_pct".to_owned(), 80.0);
        let r = h.engine().investigate_running_out(&i).await;
        let disk = result(&r, Hypothesis::Disk);
        assert_eq!(disk.verdict, Verdict::Hit);
        let e = &disk.evidence[0];
        assert_eq!(e.reason, Reason::Reaches);
        assert_eq!(e.target, 80.0);
        let days = e.days_to_target.expect("a climbing series has a date");
        assert!((days - 20.0).abs() < 2.0, "days = {days}");
    }

    #[tokio::test]
    async fn a_memory_that_climbs_and_resets_at_restarts_is_a_leak() {
        let h = Harness::new();
        let node = h.inventory.node(1, "fw-01");
        // Ten-day rises from 30 to 70, reset at day 10 and day 20.
        let saw = month(|d, s| 30.0 + (d % 10) as f64 * 4.0 + s as f64 * 0.5);
        h.metrics.series(node, "fortinet_mem_usage", saw.clone());
        let uptime: Vec<MetricPoint> = saw
            .iter()
            .map(|p| MetricPoint {
                t: p.t,
                v: ((p.t - (NOW - 30 * DAY)) % (10 * DAY)) as f64,
            })
            .collect();
        h.metrics
            .aggregated(node, yagra_common::config_unsaved::UPTIME, uptime);
        let r = h.engine().investigate_running_out(&input(node)).await;
        let mem = result(&r, Hypothesis::Memory);
        assert_eq!(mem.verdict, Verdict::Hit);
        assert_eq!(mem.evidence[0].reason, Reason::Leak);
    }

    #[tokio::test]
    async fn one_unexplained_drop_is_not_a_leak() {
        let h = Harness::new();
        let node = h.inventory.node(1, "fw-01");
        // One reset at day 15 and no restart behind it.
        h.metrics.series(
            node,
            "fortinet_mem_usage",
            month(|d, s| 30.0 + (d % 15) as f64 * 2.0 + s as f64 * 0.1),
        );
        let r = h.engine().investigate_running_out(&input(node)).await;
        assert_ne!(
            result(&r, Hypothesis::Memory).evidence[0].reason,
            Reason::Leak
        );
    }

    #[tokio::test]
    async fn a_value_already_over_its_bound_is_a_hit() {
        let h = Harness::new();
        let node = h.inventory.node(1, "sw-01");
        h.metrics
            .series(node, "huawei_cpu_usage", month(|_, _| 92.0));
        let mut i = input(node);
        i.targets.insert("huawei_cpu_usage".to_owned(), 90.0);
        let r = h.engine().investigate_running_out(&i).await;
        let cpu = result(&r, Hypothesis::Cpu);
        assert_eq!(cpu.verdict, Verdict::Hit);
        assert_eq!(cpu.evidence[0].reason, Reason::AlreadyOver);
    }

    #[tokio::test]
    async fn too_few_samples_and_no_metric_are_unknown_not_fine() {
        let h = Harness::new();
        let node = h.inventory.node(1, "sw-01");
        h.metrics.series(
            node,
            "huawei_cpu_usage",
            month(|_, _| 10.0).into_iter().take(5).collect(),
        );
        let r = h.engine().investigate_running_out(&input(node)).await;
        let cpu = result(&r, Hypothesis::Cpu);
        assert_eq!(cpu.verdict, Verdict::Unknown);
        assert_eq!(cpu.evidence[0].reason, Reason::TooFewPoints);
        let disk = result(&r, Hypothesis::Disk);
        assert_eq!(disk.verdict, Verdict::Unknown);
        assert!(disk.evidence.is_empty());
    }

    #[tokio::test]
    async fn a_derived_table_metric_is_divided_row_by_row() {
        let h = Harness::new();
        let node = h.inventory.node(1, "srv-01");
        // Row 1: a small /boot, 90% full. Row 2: a large /, 10% full. max/max would read 900/10000.
        h.metrics
            .row_series(node, "hr_storage_used", 1, month(|_, _| 900.0));
        h.metrics
            .row_series(node, "hr_storage_size", 1, month(|_, _| 1_000.0));
        h.metrics
            .row_series(node, "hr_storage_used", 2, month(|_, _| 1_000.0));
        h.metrics
            .row_series(node, "hr_storage_size", 2, month(|_, _| 10_000.0));
        let mut i = input(node);
        i.targets.insert("hr_storage_used_pct".to_owned(), 85.0);
        let r = h.engine().investigate_running_out(&i).await;
        let disk = result(&r, Hypothesis::Disk);
        assert_eq!(disk.verdict, Verdict::Hit);
        let boot = disk
            .evidence
            .iter()
            .find(|e| e.row == Some(1))
            .expect("row 1 is judged");
        assert_eq!(boot.reason, Reason::AlreadyOver);
        assert!((boot.current.unwrap_or_default() - 90.0).abs() < 1e-9);
        let root = disk
            .evidence
            .iter()
            .find(|e| e.row == Some(2))
            .expect("row 2 is judged");
        assert!((root.current.unwrap_or_default() - 10.0).abs() < 1e-9);
    }

    #[tokio::test]
    async fn a_port_without_a_speed_is_not_judged_and_a_busy_one_is() {
        let h = Harness::new();
        let node = h.inventory.node(1, "sw-01");
        let live = |bytes: f64| InterfaceLive {
            in_bps: Some(bytes),
            out_bps: Some(0.0),
            oper_status: Some(1.0),
        };
        h.metrics
            .interfaces(node, HashMap::from([(1, live(1.0e6)), (2, live(9.0e6))]));
        // Port 2 is a 100 Mb/s port carrying 96 Mb/s.
        h.metrics
            .port_series(node, 2, PortDirection::In, month(|_, _| 96.0e6));
        h.metrics
            .port_series(node, 2, PortDirection::Out, month(|_, _| 1.0e6));
        let mut i = input(node);
        i.ports = vec![
            PortInfo {
                ifindex: 1,
                name: Some("ge-0/0/1".to_owned()),
                speed_bps: None,
            },
            PortInfo {
                ifindex: 2,
                name: Some("ge-0/0/2".to_owned()),
                speed_bps: Some(100_000_000),
            },
        ];
        i.targets.insert(LINK_TARGET_METRIC.to_owned(), 90.0);
        let r = h.engine().investigate_running_out(&i).await;
        let link = result(&r, Hypothesis::LinkBandwidth);
        assert_eq!(link.evidence.len(), 1, "the port with no speed is skipped");
        assert_eq!(link.evidence[0].ifindex, Some(2));
        assert_eq!(link.verdict, Verdict::Hit);
        assert_eq!(link.evidence[0].reason, Reason::AlreadyOver);
    }

    #[test]
    fn a_hit_outranks_a_miss_and_a_miss_outranks_unknown() {
        use Verdict::*;
        assert_eq!(combine_verdicts([Unknown, Miss, Hit]), Hit);
        assert_eq!(combine_verdicts([Unknown, Miss]), Miss);
        assert_eq!(combine_verdicts([Unknown]), Unknown);
        assert_eq!(combine_verdicts([]), Unknown);
    }
}
