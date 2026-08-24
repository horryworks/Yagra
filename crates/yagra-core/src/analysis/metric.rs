// SPDX-License-Identifier: AGPL-3.0-only
//! The analyses that read the TSDB: anomaly, capacity, flap, correlation (ADR-022, ADR-089).
//!
//! First of the four groups [`super::AnalysisTool::ALL`] is ordered by — metric, passive-event,
//! flow, cross-store. Every one is a method on [`super::AnalysisRunner`], so the dispatch stays a
//! single exhaustive `match` in `mod.rs` and a sixteenth analysis cannot ship without an arm.

use super::*;

impl Engine {
    // ── Engine: Anomaly Detection ─────────────────────────────────────────────────
    //
    // For each node × usable gauge: learn a baseline (mean/σ over the baseline window) and flag
    // the recent window's largest deviation past the sensitivity σ threshold. Score scales with
    // how far past the threshold it went; the shape (spike/level/drift/flat/season) is classified
    // from the recent segment. The full series is stored for the report chart.
    pub(super) async fn run_anomaly(
        &self,
        id: Uuid,
        params: &JobParams,
        node_ids: &[Uuid],
        names: &HashMap<Uuid, String>,
        cancel: &AtomicBool,
    ) -> anyhow::Result<Option<(Vec<NewFinding>, String)>> {
        let to = now_s();
        let from = to - params.baseline_secs.max(3600);
        let step = read_step(from, to);
        let recent_cutoff = to - params.window_secs.max(300);
        let sigma = params.sensitivity.max(0.5);

        self.progress(id, 15, "Fetching baseline…").await;
        let mut findings: Vec<NewFinding> = Vec::new();
        let mut nodes_hit: BTreeSet<Uuid> = BTreeSet::new();
        let mut series_scanned = 0usize;
        let total = node_ids.len().max(1);

        for (i, node) in node_ids.iter().enumerate() {
            if cancel.load(Ordering::Relaxed) {
                return Ok(None);
            }
            let pct = 15 + (i * 70 / total) as i32;
            self.progress(id, pct, "Fitting per-metric models…").await;

            let metrics = self
                .store
                .node_metric_names(*node, params.baseline_secs as u64)
                .await;
            for metric in metrics {
                if !anomaly_usable(&metric) || !family_matches(params, &metric) {
                    continue;
                }
                let pts = self.gauge_range(*node, &metric, from, to, step).await;
                if pts.len() < MIN_POINTS {
                    continue;
                }
                series_scanned += 1;
                let Some(found) = score_anomaly(&pts, recent_cutoff, sigma) else {
                    continue;
                };
                nodes_hit.insert(*node);
                let node_name = name_lookup(names, node);
                findings.push(NewFinding {
                    score: found.score,
                    severity: severity_for(found.score).to_owned(),
                    node_id: Some(*node),
                    node_name,
                    metric: metric.clone(),
                    kind: found.kind.to_owned(),
                    when_label: rel_label(found.when_s, to),
                    duration: found.duration,
                    detail: found.detail,
                });
            }
        }

        self.progress(id, 90, "Ranking & classifying findings…")
            .await;
        findings.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        findings.truncate(MAX_FINDINGS);
        let summary = format!(
            "{} anomalies · {} nodes · {} series scanned",
            findings.len(),
            nodes_hit.len(),
            series_scanned
        );
        Ok(Some((findings, summary)))
    }

    // ── Engine: Capacity Forecast ─────────────────────────────────────────────────
    //
    // For each node × utilization-percent gauge: least-squares regress over the window, and if the
    // trend is rising, project the seconds until it reaches 100%. Nearer exhaustion ⇒ higher score.
    pub(super) async fn run_capacity(
        &self,
        id: Uuid,
        params: &JobParams,
        node_ids: &[Uuid],
        names: &HashMap<Uuid, String>,
        cancel: &AtomicBool,
    ) -> anyhow::Result<Option<(Vec<NewFinding>, String)>> {
        let to = now_s();
        let from = to - params.window_secs.max(7 * 86_400);
        let step = read_step(from, to);
        self.progress(id, 15, "Reading utilization history…").await;

        let mut findings: Vec<NewFinding> = Vec::new();
        let total = node_ids.len().max(1);
        for (i, node) in node_ids.iter().enumerate() {
            if cancel.load(Ordering::Relaxed) {
                return Ok(None);
            }
            self.progress(id, 15 + (i * 70 / total) as i32, "Projecting growth…")
                .await;
            let metrics = self
                .store
                .node_metric_names(*node, params.window_secs as u64)
                .await;
            for metric in metrics {
                if !is_utilization(&metric) {
                    continue;
                }
                let pts = self.gauge_range(*node, &metric, from, to, step).await;
                if pts.len() < MIN_POINTS {
                    continue;
                }
                let Some(proj) = project_exhaustion(&pts) else {
                    continue;
                };
                let days = proj.tte_secs as f64 / 86_400.0;
                let score = capacity_score(days);
                findings.push(NewFinding {
                    score,
                    severity: severity_for(score).to_owned(),
                    node_id: Some(*node),
                    node_name: name_lookup(names, node),
                    metric: metric.clone(),
                    kind: "capacity".to_owned(),
                    when_label: format!("{:.0}% now", proj.current),
                    duration: format!("~{} to 100%", human_days(days)),
                    detail: serde_json::json!({
                        "current": proj.current,
                        "slope_per_day": proj.slope_per_s * 86_400.0,
                        "tte_days": days,
                    }),
                });
            }
        }

        self.progress(id, 90, "Ranking by urgency…").await;
        findings.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        findings.truncate(MAX_FINDINGS);
        let near = findings.iter().filter(|f| f.severity != "info").count();
        let summary = format!(
            "{} resources approaching exhaustion ({} within 30d)",
            findings.len(),
            near
        );
        Ok(Some((findings, summary)))
    }

    // ── Engine: Flap Analysis ─────────────────────────────────────────────────────
    //
    // Reachability flap: read each node's ICMP RTT and count gaps (down→up cycles) — a node that
    // bounces leaves gaps in its otherwise-regular series. High churn ⇒ a flapping node.
    pub(super) async fn run_flap(
        &self,
        id: Uuid,
        params: &JobParams,
        node_ids: &[Uuid],
        names: &HashMap<Uuid, String>,
        cancel: &AtomicBool,
    ) -> anyhow::Result<Option<(Vec<NewFinding>, String)>> {
        let to = now_s();
        let from = to - params.window_secs.max(86_400);
        let step = read_step(from, to);
        self.progress(id, 15, "Scanning reachability history…")
            .await;

        let mut findings: Vec<NewFinding> = Vec::new();
        let total = node_ids.len().max(1);
        let window_hours = ((to - from) as f64 / 3600.0).max(1.0);
        for (i, node) in node_ids.iter().enumerate() {
            if cancel.load(Ordering::Relaxed) {
                return Ok(None);
            }
            self.progress(id, 15 + (i * 70 / total) as i32, "Counting state churn…")
                .await;
            // Raw RTT series (not aggregated) — gaps are down periods.
            let key = SeriesKey::node(NodeId::from(*node), "icmp_rtt_ms");
            let pts = self.store.range(&key, from, to, step).await;
            if pts.len() < MIN_POINTS {
                continue;
            }
            let flaps = count_flaps(&pts, step as i64);
            if flaps < 2 {
                continue;
            }
            let rate = flaps as f64 / window_hours;
            let score = flap_score(flaps);
            findings.push(NewFinding {
                score,
                severity: severity_for(score).to_owned(),
                node_id: Some(*node),
                node_name: name_lookup(names, node),
                metric: "icmp_rtt_ms".to_owned(),
                kind: "flap".to_owned(),
                when_label: format!("{flaps} flaps"),
                duration: format!("{rate:.1}/h"),
                detail: serde_json::json!({ "flaps": flaps, "per_hour": rate }),
            });
        }

        self.progress(id, 90, "Ranking flapping nodes…").await;
        findings.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        findings.truncate(MAX_FINDINGS);
        let chronic = findings.iter().filter(|f| f.severity == "crit").count();
        let summary = format!("{} nodes flapping · {} chronic", findings.len(), chronic);
        Ok(Some((findings, summary)))
    }

    // ── Engine: Event Correlation ─────────────────────────────────────────────────
    //
    // Pull a capped set of the most-variable gauges across the scope over the window, then surface
    // pairs whose values co-move (|Pearson r| past a threshold) on their shared timestamps.
    pub(super) async fn run_correlation(
        &self,
        id: Uuid,
        params: &JobParams,
        node_ids: &[Uuid],
        names: &HashMap<Uuid, String>,
        cancel: &AtomicBool,
    ) -> anyhow::Result<Option<(Vec<NewFinding>, String)>> {
        let to = now_s();
        let from = to - params.window_secs.max(3600);
        let step = read_step(from, to);
        self.progress(id, 15, "Collecting series…").await;

        // Gather candidate series (node, metric, points). Cap per-node to keep it bounded.
        let mut series: Vec<CandidateSeries> = Vec::new();
        let total = node_ids.len().max(1);
        for (i, node) in node_ids.iter().enumerate() {
            if cancel.load(Ordering::Relaxed) {
                return Ok(None);
            }
            self.progress(id, 15 + (i * 45 / total) as i32, "Collecting series…")
                .await;
            let metrics = self
                .store
                .node_metric_names(*node, params.window_secs as u64)
                .await;
            let mut per_node = 0;
            for metric in metrics {
                if !anomaly_usable(&metric) {
                    continue;
                }
                let pts = self.gauge_range(*node, &metric, from, to, step).await;
                if pts.len() < MIN_POINTS {
                    continue;
                }
                let values: Vec<f64> = pts.iter().map(|p| p.v).collect();
                let m = mean(&values);
                let var = variance(&values, m);
                if var <= f64::EPSILON {
                    continue;
                }
                series.push(CandidateSeries {
                    label: format!("{} · {}", name_lookup(names, node), metric),
                    var,
                    points: pts,
                });
                per_node += 1;
                if per_node >= 6 {
                    break;
                }
            }
        }

        // Keep the most-variable series (the interesting movers), cap the pair count.
        self.progress(id, 65, "Cross-correlating…").await;
        series.sort_by(|a, b| {
            b.var
                .partial_cmp(&a.var)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        series.truncate(24);

        let mut findings: Vec<NewFinding> = Vec::new();
        for a in 0..series.len() {
            if cancel.load(Ordering::Relaxed) {
                return Ok(None);
            }
            for b in (a + 1)..series.len() {
                let Some((r, n)) = correlate(&series[a].points, &series[b].points) else {
                    continue;
                };
                if n < 10 || r.abs() < 0.85 {
                    continue;
                }
                let score = (r.abs() * 100.0).min(100.0);
                findings.push(NewFinding {
                    score,
                    severity: severity_for(score).to_owned(),
                    node_id: None,
                    node_name: series[a].label.clone(),
                    metric: format!("{} ↔ {}", series[a].label, series[b].label),
                    kind: "correlation".to_owned(),
                    when_label: if r >= 0.0 {
                        "co-rising".to_owned()
                    } else {
                        "inverse".to_owned()
                    },
                    duration: format!("r={r:.2}"),
                    detail: serde_json::json!({ "r": r, "samples": n }),
                });
            }
        }

        self.progress(id, 90, "Ranking correlations…").await;
        findings.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        findings.truncate(MAX_FINDINGS);
        let summary = format!("{} correlated pairs", findings.len());
        Ok(Some((findings, summary)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::testkit::{flat, params, Harness};

    /// A ramp of `count` points ending at `to_s`, `step_s` apart, rising from `start` by `delta`.
    fn ramp(to_s: i64, step_s: i64, count: usize, start: f64, delta: f64) -> Vec<MetricPoint> {
        (0..count)
            .map(|i| MetricPoint {
                t: to_s - step_s * (count as i64 - 1 - i as i64),
                v: start + delta * i as f64,
            })
            .collect()
    }

    /// A flat baseline over the whole window, then a step change inside the recent hour. The shape
    /// `run_anomaly` is looking for.
    fn baseline_then_spike(to: i64) -> Vec<MetricPoint> {
        let mut pts = flat(to - 4_000, 2_000, 40, 10.0);
        for offset in [3_000, 2_000, 1_000] {
            pts.push(MetricPoint {
                t: to - offset,
                v: 90.0,
            });
        }
        pts
    }

    /// Acceptance first: the spike is found, named, and attributed. Written before the negative
    /// cases below because a suite that only proves things are skipped is satisfied by an analysis
    /// that skips everything (`rejection-only-tests-pass-when-everything-rejects`).
    #[tokio::test]
    async fn a_step_change_in_the_recent_window_is_reported_against_its_node() {
        let h = Harness::new();
        let node = h.inventory.node(1, "core-sw-01");
        h.metrics
            .aggregated(node, "cpu_pct", baseline_then_spike(now_s()));

        let (findings, summary) = h
            .engine()
            .run_anomaly(
                Uuid::nil(),
                &params(AnalysisTool::Anomaly),
                &[node],
                &HashMap::from([(node, "core-sw-01".to_owned())]),
                &AtomicBool::new(false),
            )
            .await
            .expect("the run succeeds")
            .expect("the run was not cancelled");

        assert_eq!(findings.len(), 1, "one anomalous series ⇒ one finding");
        assert_eq!(findings[0].node_id, Some(node));
        assert_eq!(findings[0].node_name, "core-sw-01");
        assert_eq!(findings[0].metric, "cpu_pct");
        assert!(findings[0].score > 0.0);
        assert!(
            summary.contains("1 anomalies") && summary.contains("1 series scanned"),
            "the summary counts what was scanned, not only what was found: {summary}"
        );
    }

    /// Counters and status enums are excluded by name, and a series too short to model is skipped.
    /// The usable metric is seeded alongside them so a store that answered nothing at all would
    /// fail this test rather than pass it.
    #[tokio::test]
    async fn a_counter_a_status_enum_and_a_short_series_are_all_skipped() {
        let to = now_s();
        let h = Harness::new();
        let node = h.inventory.node(1, "n");
        h.metrics
            .aggregated(node, "cpu_pct", baseline_then_spike(to));
        h.metrics
            .aggregated(node, "if_in_octets", baseline_then_spike(to)); // counter
        h.metrics
            .aggregated(node, "if_oper_status", baseline_then_spike(to)); // discrete enum
        h.metrics
            .aggregated(node, "mem_pct", flat(to, 600, MIN_POINTS - 1, 10.0)); // too short

        let (findings, summary) = h
            .engine()
            .run_anomaly(
                Uuid::nil(),
                &params(AnalysisTool::Anomaly),
                &[node],
                &HashMap::new(),
                &AtomicBool::new(false),
            )
            .await
            .expect("the run succeeds")
            .expect("not cancelled");

        assert_eq!(
            findings
                .iter()
                .map(|f| f.metric.as_str())
                .collect::<Vec<_>>(),
            vec!["cpu_pct"],
            "only the usable gauge is modelled"
        );
        assert!(
            summary.contains("1 series scanned"),
            "a skipped series is not scanned either: {summary}"
        );
    }

    /// `family=system` narrows what is modelled. The same store answers both runs, so the
    /// difference is the filter and not the data.
    #[tokio::test]
    async fn the_family_filter_narrows_which_metrics_are_modelled() {
        let to = now_s();
        let h = Harness::new();
        let node = h.inventory.node(1, "n");
        h.metrics
            .aggregated(node, "cpu_pct", baseline_then_spike(to));
        h.metrics
            .aggregated(node, "bgp_peer_uptime", baseline_then_spike(to));

        let all = params(AnalysisTool::Anomaly);
        let mut system = params(AnalysisTool::Anomaly);
        system.family = "system".to_owned();

        let engine = h.engine();
        let both = engine
            .run_anomaly(
                Uuid::nil(),
                &all,
                &[node],
                &HashMap::new(),
                &AtomicBool::new(false),
            )
            .await
            .expect("ok")
            .expect("not cancelled")
            .0;
        let narrowed = engine
            .run_anomaly(
                Uuid::nil(),
                &system,
                &[node],
                &HashMap::new(),
                &AtomicBool::new(false),
            )
            .await
            .expect("ok")
            .expect("not cancelled")
            .0;

        assert_eq!(both.len(), 2, "family=all models both");
        assert_eq!(
            narrowed
                .iter()
                .map(|f| f.metric.as_str())
                .collect::<Vec<_>>(),
            vec!["cpu_pct"],
            "family=system drops the routing metric"
        );
    }

    /// Findings come back worst-first — the report shows the top of this list and nothing else.
    #[tokio::test]
    async fn findings_are_ranked_worst_first() {
        let to = now_s();
        let h = Harness::new();
        let mild = h.inventory.node(1, "mild");
        let severe = h.inventory.node(2, "severe");
        // The same baseline for both — mean 10, standard deviation exactly 2 — so the only thing
        // separating the two findings is how far the recent sample departs from it. A flat
        // baseline would not do: its deviation floor sends every departure to a score of 100.
        let wobble = |v: f64| {
            let mut pts: Vec<MetricPoint> = (0..40)
                .map(|i| MetricPoint {
                    t: to - 4_000 - 2_000 * (39 - i),
                    v: if i % 2 == 0 { 8.0 } else { 12.0 },
                })
                .collect();
            pts.push(MetricPoint { t: to - 1_000, v });
            pts
        };
        h.metrics.aggregated(mild, "cpu_pct", wobble(17.0)); // 3.5 sigma
        h.metrics.aggregated(severe, "cpu_pct", wobble(90.0)); // 40 sigma

        let (findings, _) = h
            .engine()
            .run_anomaly(
                Uuid::nil(),
                &params(AnalysisTool::Anomaly),
                &[mild, severe],
                &HashMap::new(),
                &AtomicBool::new(false),
            )
            .await
            .expect("ok")
            .expect("not cancelled");

        assert_eq!(findings.len(), 2);
        assert_eq!(
            findings[0].node_id,
            Some(severe),
            "the worse finding leads: {:?}",
            findings.iter().map(|f| f.score).collect::<Vec<_>>()
        );
        assert!(findings[0].score >= findings[1].score);
    }

    /// A cancelled job yields `Ok(None)` — no findings at all rather than the half it had reached,
    /// because `run_job` writes whatever comes back and a partial set would read as a complete one.
    #[tokio::test]
    async fn a_cancelled_run_returns_nothing_rather_than_what_it_had_so_far() {
        let h = Harness::new();
        let node = h.inventory.node(1, "n");
        h.metrics
            .aggregated(node, "cpu_pct", baseline_then_spike(now_s()));

        let out = h
            .engine()
            .run_anomaly(
                Uuid::nil(),
                &params(AnalysisTool::Anomaly),
                &[node],
                &HashMap::new(),
                &AtomicBool::new(true),
            )
            .await
            .expect("cancelling is not an error");
        assert!(out.is_none(), "cancelled ⇒ no partial findings");
    }

    /// The phases a run reports are monotonic and end where the API's progress bar expects.
    /// Nothing else checks this, and a job that never ticks looks identical to a fast one.
    #[tokio::test]
    async fn a_run_reports_progress_from_start_to_ranking() {
        let h = Harness::new();
        let node = h.inventory.node(1, "n");
        h.metrics
            .aggregated(node, "cpu_pct", baseline_then_spike(now_s()));

        h.engine()
            .run_anomaly(
                Uuid::nil(),
                &params(AnalysisTool::Anomaly),
                &[node],
                &HashMap::new(),
                &AtomicBool::new(false),
            )
            .await
            .expect("ok")
            .expect("not cancelled");

        let ticks = h.progress.ticks();
        assert!(ticks.len() >= 3, "a run reports its phases: {ticks:?}");
        assert_eq!(ticks.first().map(|t| t.0), Some(15));
        assert_eq!(ticks.last().map(|t| t.0), Some(90));
        assert!(
            ticks.windows(2).all(|w| w[0].0 <= w[1].0),
            "progress never goes backwards: {ticks:?}"
        );
    }

    // ── Capacity ─────────────────────────────────────────────────────────────────────────

    /// A rising utilization gauge is projected; a rising gauge that is not utilization is not,
    /// even though the two series are identical. The distinction is the metric's name.
    #[tokio::test]
    async fn capacity_projects_a_rising_utilization_gauge_and_only_that() {
        let to = now_s();
        let h = Harness::new();
        let node = h.inventory.node(1, "n");
        let rising = ramp(to, 30_000, 20, 10.0, 3.5);
        h.metrics.aggregated(node, "disk_usage_pct", rising.clone());
        h.metrics.aggregated(node, "temp_celsius", rising);

        let (findings, summary) = h
            .engine()
            .run_capacity(
                Uuid::nil(),
                &params(AnalysisTool::Capacity),
                &[node],
                &HashMap::new(),
                &AtomicBool::new(false),
            )
            .await
            .expect("ok")
            .expect("not cancelled");

        assert_eq!(
            findings
                .iter()
                .map(|f| f.metric.as_str())
                .collect::<Vec<_>>(),
            vec!["disk_usage_pct"],
            "only a percent-like gauge can be extrapolated toward 100%"
        );
        assert_eq!(findings[0].kind, "capacity");
        assert!(summary.contains("1 resources approaching exhaustion"));
    }

    /// A utilization gauge that is falling has no exhaustion date, so it produces no finding —
    /// the projection is directional, not merely a trend line.
    #[tokio::test]
    async fn capacity_says_nothing_about_a_falling_gauge() {
        let to = now_s();
        let h = Harness::new();
        let node = h.inventory.node(1, "n");
        h.metrics
            .aggregated(node, "disk_usage_pct", ramp(to, 30_000, 20, 80.0, -3.0));

        let (findings, _) = h
            .engine()
            .run_capacity(
                Uuid::nil(),
                &params(AnalysisTool::Capacity),
                &[node],
                &HashMap::new(),
                &AtomicBool::new(false),
            )
            .await
            .expect("ok")
            .expect("not cancelled");
        assert!(findings.is_empty(), "falling ⇒ nothing to warn about");
    }

    // ── Flap ─────────────────────────────────────────────────────────────────────────────

    /// 🎯 **`run_flap` must read the raw series, never the aggregate.** A flap leaves *gaps* in an
    /// otherwise regular series, and an aggregate read fills them in — so switching this one call
    /// to `gauge_range`, which is what its three neighbours use, would silently stop the analysis
    /// finding anything. Nothing else in the repository can tell the two calls apart.
    ///
    /// The two maps are seeded on purpose: the raw one has the gaps, the aggregated one is smooth.
    /// Reading the wrong one returns a healthy node.
    #[tokio::test]
    async fn flap_reads_the_raw_series_where_the_gaps_are_not_the_aggregate() {
        let to = now_s();
        let h = Harness::new();
        let node = h.inventory.node(1, "flapper");

        // Regular 300s samples, with three long gaps — three down/up cycles.
        let mut raw = Vec::new();
        let mut t = to - 80_000;
        for i in 0..16 {
            raw.push(MetricPoint { t, v: 4.0 });
            t += if i % 5 == 4 { 20_000 } else { 300 };
        }
        h.metrics.raw(node, "icmp_rtt_ms", raw);
        // What an aggregate read would return: the same window, evenly sampled, no gaps.
        h.metrics
            .aggregated(node, "icmp_rtt_ms", flat(to, 300, 40, 4.0));

        let (findings, summary) = h
            .engine()
            .run_flap(
                Uuid::nil(),
                &params(AnalysisTool::Flap),
                &[node],
                &HashMap::from([(node, "flapper".to_owned())]),
                &AtomicBool::new(false),
            )
            .await
            .expect("ok")
            .expect("not cancelled");

        assert_eq!(
            findings.len(),
            1,
            "the gaps in the raw series are the flaps; an aggregate read sees none"
        );
        assert_eq!(findings[0].kind, "flap");
        assert_eq!(findings[0].metric, "icmp_rtt_ms");
        assert_eq!(findings[0].node_name, "flapper");
        assert!(summary.contains("1 nodes flapping"));
    }

    /// One gap is a reboot, not a flap. The floor is two.
    #[tokio::test]
    async fn a_single_gap_is_not_a_flap() {
        let to = now_s();
        let h = Harness::new();
        let node = h.inventory.node(1, "n");
        let mut raw: Vec<MetricPoint> = (0..16)
            .map(|i| MetricPoint {
                t: to - 80_000 + i * 300,
                v: 4.0,
            })
            .collect();
        raw.push(MetricPoint { t: to, v: 4.0 }); // one long gap
        h.metrics.raw(node, "icmp_rtt_ms", raw);

        let (findings, _) = h
            .engine()
            .run_flap(
                Uuid::nil(),
                &params(AnalysisTool::Flap),
                &[node],
                &HashMap::new(),
                &AtomicBool::new(false),
            )
            .await
            .expect("ok")
            .expect("not cancelled");
        assert!(findings.is_empty(), "one gap is below the floor");
    }

    // ── Correlation ──────────────────────────────────────────────────────────────────────

    /// Two series that move together are reported as a pair; the pair is named from both labels so
    /// the report can say what co-moved with what.
    #[tokio::test]
    async fn correlation_pairs_two_series_that_move_together() {
        let to = now_s();
        let h = Harness::new();
        let node = h.inventory.node(1, "n");
        // `series` rather than `aggregated`: correlation reads through `gauge_range` like its
        // neighbours, and unlike `run_flap` it has no reason to care which map answers.
        h.metrics
            .series(node, "cpu_pct", ramp(to, 300, 20, 10.0, 2.0));
        h.metrics
            .series(node, "mem_pct", ramp(to, 300, 20, 30.0, 1.5));

        let (findings, summary) = h
            .engine()
            .run_correlation(
                Uuid::nil(),
                &params(AnalysisTool::Correlation),
                &[node],
                &HashMap::from([(node, "n".to_owned())]),
                &AtomicBool::new(false),
            )
            .await
            .expect("ok")
            .expect("not cancelled");

        assert_eq!(findings.len(), 1, "one pair: {summary}");
        assert_eq!(findings[0].kind, "correlation");
        assert!(findings[0].metric.contains("cpu_pct"));
        assert!(findings[0].metric.contains("mem_pct"));
        assert_eq!(
            findings[0].node_id, None,
            "a pair is not one node's finding"
        );
    }

    /// A series that never moves has no correlation to report, however many partners it has —
    /// a constant correlates with everything and means nothing.
    #[tokio::test]
    async fn a_flat_series_is_not_correlated_with_anything() {
        let to = now_s();
        let h = Harness::new();
        let node = h.inventory.node(1, "n");
        h.metrics
            .aggregated(node, "cpu_pct", ramp(to, 300, 20, 10.0, 2.0));
        h.metrics
            .aggregated(node, "mem_pct", flat(to, 300, 20, 5.0));

        let (findings, _) = h
            .engine()
            .run_correlation(
                Uuid::nil(),
                &params(AnalysisTool::Correlation),
                &[node],
                &HashMap::new(),
                &AtomicBool::new(false),
            )
            .await
            .expect("ok")
            .expect("not cancelled");
        assert!(findings.is_empty(), "a constant is not a mover");
    }
}
