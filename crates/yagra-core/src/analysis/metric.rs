// SPDX-License-Identifier: AGPL-3.0-only
//! The analyses that read the TSDB: anomaly, capacity, flap, correlation (ADR-022, ADR-089).
//!
//! First of the four groups [`super::AnalysisTool::ALL`] is ordered by — metric, passive-event,
//! flow, cross-store. Every one is a method on [`super::AnalysisRunner`], so the dispatch stays a
//! single exhaustive `match` in `mod.rs` and a sixteenth analysis cannot ship without an arm.

use super::*;

impl AnalysisRunner {
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
