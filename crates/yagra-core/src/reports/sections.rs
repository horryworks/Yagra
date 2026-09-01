// SPDX-License-Identifier: AGPL-3.0-only
//! **Where each section's numbers come from** — the six `render_*`, one per section kind.
//!
//! Each one fetches from the seams [`super::runner`] holds and hands the result to
//! [`super::render`]; the arithmetic they need is over there, where a test can reach it. That
//! division is the whole reason the split line is "does it `.await`" — see [`super`].
//!
//! Adding a section: an entry in [`super::catalog`], an arm in
//! [`super::runner::ReportRunner::render_section`], and a `render_*` here. Two of the three are
//! in this directory and the compiler finds neither, so the catalog test is what notices a kind
//! that has no renderer.

use super::*;

use std::collections::HashMap;

use uuid::Uuid;

use crate::store::TopAgg;

impl ReportRunner {
    pub(super) async fn render_availability(&self, id: String, from_s: i64, to_s: i64) -> Section {
        let rows = self
            .nodes
            .state_history(from_s, to_s)
            .await
            .unwrap_or_default();
        let (uptime, per_state) = availability_from_snapshots(&rows);
        let kpis = vec![
            (
                "Uptime".to_owned(),
                uptime.map_or_else(|| "—".to_owned(), |u| format!("{u:.2}%")),
            ),
            ("Snapshots".to_owned(), rows.len().to_string()),
        ];
        let total: i64 = per_state.iter().map(|(_, c)| c).sum();
        let table = Table {
            columns: vec!["State".to_owned(), "Share".to_owned(), "Samples".to_owned()],
            rows: per_state
                .iter()
                .map(|(s, c)| {
                    let share = if total > 0 {
                        format!("{:.1}%", *c as f64 / total as f64 * 100.0)
                    } else {
                        "—".to_owned()
                    };
                    vec![s.clone(), share, c.to_string()]
                })
                .collect(),
        };
        Section {
            id,
            kind: "availability-summary".to_owned(),
            title: "Availability summary (SLA)".to_owned(),
            summary: Some(
                "Uptime = reachable (ok + warning) ÷ (reachable + down); unknown and maintenance \
                 samples are excluded. Coverage depends on state-snapshot retention."
                    .to_owned(),
            ),
            kpis,
            table: Some(table),
            ..Default::default()
        }
    }

    pub(super) async fn render_alert_summary(
        &self,
        id: String,
        from_s: i64,
        _to_s: i64,
    ) -> Section {
        // Fires in the window, by severity. The store counts them: this used to fold the newest
        // 1000 history rows here and was silently low on a busy fleet (ADR-112 Inc.2). The lower
        // bound is the same expression `render_top_alerting` passes, which is what stops the two
        // numbers in one report from disagreeing.
        let from_ms = from_s * 1000;
        let fires: HashMap<String, i64> = self
            .alerts
            .fires_by_severity(from_ms)
            .await
            .unwrap_or_default()
            .into_iter()
            .collect();
        let total_fires: i64 = fires.values().sum();
        // Active alerts now, by severity.
        let active = self.alerts.active_alerts();
        let mut active_by_sev: HashMap<String, i64> = HashMap::new();
        for a in &active {
            *active_by_sev
                .entry(a.severity.as_str().to_owned())
                .or_insert(0) += 1;
        }
        let kpis = vec![
            ("Active alerts".to_owned(), active.len().to_string()),
            ("Fires in window".to_owned(), total_fires.to_string()),
            (
                "Critical fires".to_owned(),
                fires.get("critical").copied().unwrap_or(0).to_string(),
            ),
        ];
        let sev_order = ["critical", "warning", "info"];
        let rows: Vec<Vec<String>> = sev_order
            .iter()
            .map(|s| {
                vec![
                    (*s).to_owned(),
                    active_by_sev.get(*s).copied().unwrap_or(0).to_string(),
                    fires.get(*s).copied().unwrap_or(0).to_string(),
                ]
            })
            .collect();
        Section {
            id,
            kind: "alert-summary".to_owned(),
            title: "Alert summary".to_owned(),
            summary: Some(format!("{total_fires} alert fires in the window.")),
            kpis,
            table: Some(Table {
                columns: vec![
                    "Severity".to_owned(),
                    "Active now".to_owned(),
                    "Fires in window".to_owned(),
                ],
                rows,
            }),
            ..Default::default()
        }
    }

    pub(super) async fn render_top_alerting(
        &self,
        id: String,
        settings: &serde_json::Value,
        from_s: i64,
    ) -> Section {
        let limit = setting_i64(settings, "limit", 10).clamp(1, 100);
        let pairs = self
            .alerts
            .top_nodes_by_fires(from_s * 1000, limit)
            .await
            .unwrap_or_default();
        let ids: Vec<Uuid> = pairs.iter().map(|(id, _)| *id).collect();
        // Unrestricted: a report is an admin-defined, fleet-wide artefact generated by a background
        // task with no requesting principal. Reports are `ManageConfig`-gated (ADR-014 non-goal).
        let names = self.nodes.node_names(None, &ids).await.unwrap_or_default();
        let rows: Vec<Vec<String>> = pairs
            .iter()
            .map(|(node, n)| {
                let name = names.get(node).cloned().unwrap_or_else(|| node.to_string());
                vec![name, n.to_string()]
            })
            .collect();
        Section {
            id,
            kind: "top-alerting-nodes".to_owned(),
            title: "Top alerting nodes".to_owned(),
            summary: Some(format!("{} nodes with the most fires.", rows.len())),
            table: Some(Table {
                columns: vec!["Node".to_owned(), "Fires".to_owned()],
                rows,
            }),
            ..Default::default()
        }
    }

    pub(super) async fn render_top_metric(
        &self,
        id: String,
        kind: &str,
        settings: &serde_json::Value,
    ) -> Section {
        let limit = setting_i64(settings, "limit", 10).clamp(1, 100) as usize;
        let agg = match setting_str(settings, "agg", "max_1h").as_str() {
            "now" => TopAgg::Now,
            _ => TopAgg::Max1h,
        };
        let Some((selector, label, unit)) = top_metric_selector(kind) else {
            return Section {
                id,
                kind: kind.to_owned(),
                title: format!("Unknown metric section: {kind}"),
                ..Default::default()
            };
        };
        let pairs = self.store.top_nodes(&selector, agg, limit).await;
        let ids: Vec<Uuid> = pairs.iter().map(|(id, _)| *id).collect();
        // Unrestricted: a report is an admin-defined, fleet-wide artefact generated by a background
        // task with no requesting principal. Reports are `ManageConfig`-gated (ADR-014 non-goal).
        let names = self.nodes.node_names(None, &ids).await.unwrap_or_default();
        let rows: Vec<Vec<String>> = pairs
            .iter()
            .map(|(node, v)| {
                let name = names.get(node).cloned().unwrap_or_else(|| node.to_string());
                vec![name, format!("{v:.1} {unit}")]
            })
            .collect();
        Section {
            id,
            kind: kind.to_owned(),
            title: format!("Top {label}"),
            summary: Some(format!("Top {} nodes by {label}.", rows.len())),
            table: Some(Table {
                columns: vec!["Node".to_owned(), format!("{label} ({unit})")],
                rows,
            }),
            ..Default::default()
        }
    }

    pub(super) async fn render_throughput(&self, id: String, from_s: i64, to_s: i64) -> Section {
        let step = read_step(from_s, to_s);
        let (in_pts, out_pts) = self.store.throughput_range(from_s, to_s, step).await;
        let peak_in = in_pts.iter().map(|p| p.v).fold(0.0_f64, f64::max);
        let peak_out = out_pts.iter().map(|p| p.v).fold(0.0_f64, f64::max);
        let series = vec![
            LineSeries {
                label: "In".to_owned(),
                color: "#2f6df6",
                points: in_pts.iter().map(|p| (p.t, p.v)).collect(),
            },
            LineSeries {
                label: "Out".to_owned(),
                color: "#16a34a",
                points: out_pts.iter().map(|p| (p.t, p.v)).collect(),
            },
        ];
        let chart = svg_line_chart(&series);
        // Downsample for the CSV table (≤24 rows).
        let stride = (in_pts.len() / 24).max(1);
        let table_rows: Vec<Vec<String>> = in_pts
            .iter()
            .step_by(stride)
            .map(|p| {
                let out = out_pts.iter().find(|o| o.t == p.t).map_or(0.0, |o| o.v);
                vec![fmt_minute(p.t), human_bps(p.v), human_bps(out)]
            })
            .collect();
        Section {
            id,
            kind: "throughput-trend".to_owned(),
            title: "Throughput trend".to_owned(),
            summary: Some("Fleet aggregate in/out throughput over the window.".to_owned()),
            kpis: vec![
                ("Peak in".to_owned(), human_bps(peak_in)),
                ("Peak out".to_owned(), human_bps(peak_out)),
            ],
            chart_svg: Some(chart),
            table: Some(Table {
                columns: vec!["Time".to_owned(), "In".to_owned(), "Out".to_owned()],
                rows: table_rows,
            }),
            ..Default::default()
        }
    }

    pub(super) async fn render_inventory(
        &self,
        id: String,
        settings: &serde_json::Value,
    ) -> Section {
        let limit = setting_i64(settings, "limit", 200).clamp(1, 5000) as usize;
        let mut nodes = self.nodes.list_nodes().await.unwrap_or_default();
        nodes.sort_by(|a, b| a.name.cmp(&b.name));
        let states = self.alerts.node_states();
        let total = nodes.len();
        // A node the engine has never observed takes the same fallback every other surface takes
        // (`api::nodes::state_or_fallback`): a recent ICMP sample means `ok`. A report generated in
        // the minutes after a core restart used to print `unknown` down the whole column while the
        // WebUI showed the same fleet as up — and a report is the artifact someone forwards.
        // One fleet-wide freshness query, and only when there is something to fall back for.
        let fresh = if nodes
            .iter()
            .take(limit)
            .any(|n| !states.contains_key(&n.id))
        {
            crate::api::nodes::fresh_fleet_ids(self.store.as_ref()).await
        } else {
            std::collections::HashSet::new()
        };
        let rows: Vec<Vec<String>> = nodes
            .iter()
            .take(limit)
            .map(|n| {
                let state = crate::api::nodes::state_or_fallback(
                    states.get(&n.id).copied(),
                    fresh.contains(&n.id.as_uuid()),
                )
                .as_str()
                .to_owned();
                vec![
                    n.name.clone(),
                    n.address.to_string(),
                    n.vendor.clone().unwrap_or_default(),
                    n.model.clone().unwrap_or_default(),
                    state,
                ]
            })
            .collect();
        let note = if total > limit {
            Some(format!("Showing {limit} of {total} nodes (row limit)."))
        } else {
            None
        };
        Section {
            id,
            kind: "inventory-listing".to_owned(),
            title: "Inventory listing".to_owned(),
            summary: Some(format!("{total} monitored nodes.")),
            table: Some(Table {
                columns: vec![
                    "Name".to_owned(),
                    "Address".to_owned(),
                    "Vendor".to_owned(),
                    "Model".to_owned(),
                    "State".to_owned(),
                ],
                rows,
            }),
            note,
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testkit::{alert, harness, n, node, uid};
    use super::*;

    use crate::store::{MetricPoint, TopAgg};
    use yagra_common::{NodeState, Severity};

    /// The value of one KPI, by its label.
    fn kpi(s: &Section, label: &str) -> String {
        s.kpis
            .iter()
            .find(|(l, _)| l == label)
            .map(|(_, v)| v.clone())
            .unwrap_or_else(|| panic!("no KPI called {label}: {:?}", s.kpis))
    }

    /// The rows of a section's table.
    fn rows(s: &Section) -> Vec<Vec<String>> {
        s.table.as_ref().expect("a table").rows.clone()
    }

    fn settings(v: serde_json::Value) -> serde_json::Value {
        v
    }

    // ── Availability ─────────────────────────────────────────────────────────────────────────

    /// The "Snapshots" KPI counts snapshot **rows**, not monitored nodes — two rows describing one
    /// instant read as 2. Pinned because the label invites the other reading.
    #[tokio::test]
    async fn the_availability_section_reports_the_share_of_each_state() {
        let h = harness()
            .state_history(vec![(0, "ok".to_owned(), 3), (0, "critical".to_owned(), 1)])
            .build();
        let s = h.runner.render_availability("a".to_owned(), 0, 60).await;
        assert_eq!(n(&h.calls.state_history), 1);
        assert_eq!(kpi(&s, "Uptime"), "75.00%");
        assert_eq!(kpi(&s, "Snapshots"), "2");
        assert_eq!(
            rows(&s),
            vec![
                vec!["ok".to_owned(), "75.0%".to_owned(), "3".to_owned()],
                vec!["critical".to_owned(), "25.0%".to_owned(), "1".to_owned()],
            ]
        );
    }

    /// A store that cannot answer must degrade to an empty section, not a failed report — every
    /// renderer takes `unwrap_or_default`, and the run around it still has to finish.
    #[tokio::test]
    async fn a_failed_snapshot_read_renders_an_empty_availability_section() {
        let h = harness().inventory_fails().build();
        let s = h.runner.render_availability("a".to_owned(), 0, 60).await;
        assert_eq!(n(&h.calls.state_history), 1);
        assert_eq!(kpi(&s, "Uptime"), "—");
        assert_eq!(kpi(&s, "Snapshots"), "0");
        assert!(rows(&s).is_empty());
    }

    // ── Alert summary ────────────────────────────────────────────────────────────────────────

    /// 🎯 **The counts come from the store now** (ADR-112 Inc.2), so what this file is responsible
    /// for is putting them in the right places: three KPIs and a fixed three-row table.
    ///
    /// ⚠️ What moved out of reach in exchange: "a resolved row is not a fire" and "a row from
    /// before the window is not a fire *in the window*" used to be assertions here, over a fold. In
    /// SQL a fake cannot see them — the same limit `ReportsRepo` has. They are one `const` in
    /// `history.rs` now, shared with the ranking below, which is why the trade is worth taking.
    #[tokio::test]
    async fn the_store_counts_the_fires_and_this_file_places_them() {
        let h = harness()
            .fires(vec![("critical".to_owned(), 3), ("warning".to_owned(), 1)])
            .build();
        let s = h.runner.render_alert_summary("a".to_owned(), 0, 60).await;
        assert_eq!(n(&h.calls.fires_by_severity), 1);
        assert_eq!(kpi(&s, "Fires in window"), "4");
        assert_eq!(kpi(&s, "Critical fires"), "3");
        assert_eq!(
            rows(&s),
            vec![
                vec!["critical".to_owned(), "0".to_owned(), "3".to_owned()],
                vec!["warning".to_owned(), "0".to_owned(), "1".to_owned()],
                vec!["info".to_owned(), "0".to_owned(), "0".to_owned()],
            ]
        );
    }

    /// 🚨 **The defect ADR-112 recorded and did not fix, closed** (Inc.2).
    ///
    /// "Fires in window" and "Top alerting nodes" appear two sections apart in one report and a
    /// reader compares them. The first used to be folded from the newest 1000 history rows and the
    /// second has always been a SQL aggregate, so on a busy fleet the document disagreed with
    /// itself. Both are aggregates now; this pins the remaining way they could still diverge, which
    /// is being counted from different instants.
    #[tokio::test]
    async fn both_halves_of_the_alert_story_count_from_the_same_instant() {
        let h = harness().build();
        let _ = h.runner.render_alert_summary("a".to_owned(), 5, 60).await;
        let _ = h
            .runner
            .render_top_alerting("b".to_owned(), &settings(serde_json::Value::Null), 5)
            .await;
        let seen = h.alerts.since.lock().expect("poisoned").clone();
        assert_eq!(
            seen,
            vec![5_000, 5_000],
            "the summary and the ranking must be asked for the same window"
        );
    }

    /// The accept side of the two above, and the other half of the table: what is alerting *now*.
    #[tokio::test]
    async fn active_alerts_are_counted_by_severity_beside_the_fires() {
        let h = harness()
            .active(vec![
                alert(Severity::Critical),
                alert(Severity::Critical),
                alert(Severity::Warning),
            ])
            .fires(vec![("warning".to_owned(), 1)])
            .build();
        let s = h.runner.render_alert_summary("a".to_owned(), 0, 60).await;
        assert_eq!(n(&h.calls.active_alerts), 1);
        assert_eq!(n(&h.calls.fires_by_severity), 1);
        assert_eq!(kpi(&s, "Active alerts"), "3");
        assert_eq!(kpi(&s, "Fires in window"), "1");
        // severity, active now, fires in window — in the fixed order critical/warning/info.
        assert_eq!(
            rows(&s),
            vec![
                vec!["critical".to_owned(), "2".to_owned(), "0".to_owned()],
                vec!["warning".to_owned(), "1".to_owned(), "1".to_owned()],
                vec!["info".to_owned(), "0".to_owned(), "0".to_owned()],
            ]
        );
    }

    #[tokio::test]
    async fn a_failed_history_read_renders_an_alert_summary_with_no_fires() {
        let h = harness()
            .fires(vec![("critical".to_owned(), 9)])
            .alerts_fail()
            .build();
        let s = h.runner.render_alert_summary("a".to_owned(), 0, 60).await;
        assert_eq!(kpi(&s, "Fires in window"), "0");
        assert_eq!(rows(&s).len(), 3, "the severity rows are always present");
    }

    // ── Top alerting nodes ───────────────────────────────────────────────────────────────────

    /// A node the name lookup does not answer for still has to appear — the ranking came from the
    /// history store, and dropping the row would under-report the fleet's worst offenders.
    #[tokio::test]
    async fn a_node_with_no_resolvable_name_falls_back_to_its_uuid() {
        let h = harness()
            .top_fires(vec![(uid(1), 9), (uid(2), 4)])
            .names(&[(uid(1), "core-sw-1")])
            .build();
        let s = h
            .runner
            .render_top_alerting("a".to_owned(), &settings(serde_json::Value::Null), 0)
            .await;
        assert_eq!(
            rows(&s),
            vec![
                vec!["core-sw-1".to_owned(), "9".to_owned()],
                vec![uid(2).to_string(), "4".to_owned()],
            ]
        );
    }

    #[tokio::test]
    async fn the_top_alerting_row_limit_is_clamped_at_both_ends() {
        let many: Vec<(uuid::Uuid, i64)> = (1..=200_u32)
            .map(|i| (uid(u128::from(i)), i64::from(i)))
            .collect();
        for (asked, expected) in [(0_i64, 1_usize), (5, 5), (5000, 100)] {
            let h = harness().top_fires(many.clone()).build();
            let s = h
                .runner
                .render_top_alerting(
                    "a".to_owned(),
                    &settings(serde_json::json!({ "limit": asked })),
                    0,
                )
                .await;
            assert_eq!(rows(&s).len(), expected, "limit={asked}");
        }
    }

    #[tokio::test]
    async fn a_failed_history_read_renders_an_empty_top_alerting_table() {
        let h = harness().alerts_fail().build();
        let s = h
            .runner
            .render_top_alerting("a".to_owned(), &settings(serde_json::Value::Null), 0)
            .await;
        assert_eq!(n(&h.calls.top_nodes_by_fires), 1);
        assert!(rows(&s).is_empty());
    }

    // ── Top metric ───────────────────────────────────────────────────────────────────────────

    /// 🎯 The unknown-kind branch returns **before** the query. A store round trip for a section
    /// that is going to render a placeholder is pure waste, and nothing but a counter can see it.
    #[tokio::test]
    async fn an_unknown_metric_section_never_asks_the_store() {
        let h = harness().top_nodes(vec![(uid(1), 1.0)]).build();
        let s = h
            .runner
            .render_top_metric(
                "a".to_owned(),
                "top-bananas",
                &settings(serde_json::Value::Null),
            )
            .await;
        assert_eq!(
            n(&h.calls.top_nodes),
            0,
            "the TSDB was queried for a section that renders a placeholder"
        );
        assert_eq!(n(&h.calls.node_names), 0);
        assert!(s.title.starts_with("Unknown metric section"));
    }

    /// The accept side: a kind the selector *does* recognise queries exactly once.
    #[tokio::test]
    async fn a_known_metric_section_asks_the_store_once() {
        let h = harness()
            .top_nodes(vec![(uid(1), 42.5)])
            .names(&[(uid(1), "core-sw-1")])
            .build();
        let s = h
            .runner
            .render_top_metric(
                "a".to_owned(),
                "top-cpu",
                &settings(serde_json::Value::Null),
            )
            .await;
        assert_eq!(n(&h.calls.top_nodes), 1);
        assert_eq!(
            rows(&s),
            vec![vec!["core-sw-1".to_owned(), "42.5 %".to_owned()]]
        );
    }

    /// The aggregate is parsed with a `_ =>` fallback, so a typo becomes the hourly max rather than
    /// an error. That is the intended degradation; this pins that it is still what happens.
    #[tokio::test]
    async fn an_unrecognised_aggregate_setting_falls_back_to_the_hourly_max() {
        for (asked, expected) in [
            ("now", TopAgg::Now),
            ("max_1h", TopAgg::Max1h),
            ("weekly", TopAgg::Max1h),
        ] {
            let h = harness().build();
            let _ = h
                .runner
                .render_top_metric(
                    "a".to_owned(),
                    "top-rtt",
                    &settings(serde_json::json!({ "agg": asked })),
                )
                .await;
            assert_eq!(
                *h.metrics.aggs.lock().expect("poisoned"),
                vec![expected],
                "agg={asked}"
            );
        }
    }

    // ── Throughput ───────────────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn the_throughput_peaks_are_the_maximum_of_each_direction() {
        let h = harness()
            .throughput(
                vec![pt(0, 1_000.0), pt(60, 3_000.0)],
                vec![pt(0, 2_000.0), pt(60, 500.0)],
            )
            .build();
        let s = h.runner.render_throughput("a".to_owned(), 0, 120).await;
        assert_eq!(n(&h.calls.throughput_range), 1);
        assert_eq!(kpi(&s, "Peak in"), "3.0 Kbps");
        assert_eq!(kpi(&s, "Peak out"), "2.0 Kbps");
    }

    /// 🚨 The table joins the two directions by timestamp and maps a miss to **0**, so a gap in the
    /// out series prints as "0 bps" rather than as missing data. Recorded, not endorsed: an
    /// operator reading the CSV cannot tell a real zero from an absent sample.
    #[tokio::test]
    async fn a_timestamp_missing_from_the_out_series_is_printed_as_zero() {
        let h = harness()
            .throughput(vec![pt(0, 1_000.0)], vec![pt(999, 8_000.0)])
            .build();
        let s = h.runner.render_throughput("a".to_owned(), 0, 120).await;
        let r = rows(&s);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0][2], "0.0 bps");
    }

    /// The CSV table is downsampled so a long window does not produce thousands of rows; the chart
    /// keeps every point.
    #[tokio::test]
    async fn a_long_throughput_series_is_downsampled_for_the_table() {
        let pts: Vec<MetricPoint> = (0..100_i32)
            .map(|i| pt(i64::from(i), f64::from(i)))
            .collect();
        let h = harness().throughput(pts.clone(), pts).build();
        let s = h.runner.render_throughput("a".to_owned(), 0, 6_000).await;
        assert_eq!(rows(&s).len(), 25, "100 points at stride 4");
    }

    fn pt(t: i64, v: f64) -> MetricPoint {
        MetricPoint { t, v }
    }

    // ── Inventory ────────────────────────────────────────────────────────────────────────────

    /// 🎯 The freshness query is a fleet-wide TSDB scan. It runs **only** when some listed node has
    /// no state from the alert engine — which, on a settled deployment, is never.
    #[tokio::test]
    async fn the_freshness_query_is_skipped_when_every_listed_node_has_a_state() {
        let h = harness()
            .nodes(vec![node(1, "a"), node(2, "b")])
            .states(&[(1, NodeState::Ok), (2, NodeState::Critical)])
            .build();
        let s = h
            .runner
            .render_inventory("a".to_owned(), &settings(serde_json::Value::Null))
            .await;
        assert_eq!(n(&h.calls.list_nodes), 1);
        assert_eq!(
            n(&h.calls.fresh_node_ids),
            0,
            "a fleet-wide TSDB scan ran with nothing to fall back for"
        );
        assert_eq!(rows(&s)[0][4], "ok");
        assert_eq!(rows(&s)[1][4], "critical");
    }

    /// The accept side, and the reason the fallback exists: in the minutes after a restart the
    /// engine has no opinion yet, and a recent liveness sample means `ok` rather than `unknown`.
    #[tokio::test]
    async fn a_node_the_engine_has_not_observed_falls_back_to_a_recent_sample() {
        let h = harness()
            .nodes(vec![node(1, "fresh"), node(2, "silent")])
            .fresh(vec![uid(1)])
            .build();
        let s = h
            .runner
            .render_inventory("a".to_owned(), &settings(serde_json::Value::Null))
            .await;
        assert_eq!(n(&h.calls.fresh_node_ids), 1);
        assert_eq!(rows(&s)[0][4], "ok", "fresh but unobserved");
        assert_eq!(rows(&s)[1][4], "unknown", "neither observed nor fresh");
    }

    #[tokio::test]
    async fn the_inventory_note_says_how_many_nodes_were_left_out() {
        let h = harness()
            .nodes((1..=5).map(|i| node(i, "n")).collect())
            .build();
        let s = h
            .runner
            .render_inventory("a".to_owned(), &settings(serde_json::json!({ "limit": 2 })))
            .await;
        assert_eq!(rows(&s).len(), 2);
        assert_eq!(s.note.as_deref(), Some("Showing 2 of 5 nodes (row limit)."));
        assert_eq!(s.summary.as_deref(), Some("5 monitored nodes."));
    }

    #[tokio::test]
    async fn a_failed_inventory_read_renders_an_empty_listing() {
        let h = harness().inventory_fails().build();
        let s = h
            .runner
            .render_inventory("a".to_owned(), &settings(serde_json::Value::Null))
            .await;
        assert!(rows(&s).is_empty());
        assert_eq!(s.note, None);
    }

    // ── The scope a report reads with ────────────────────────────────────────────────────────

    /// 🎯 A report is generated by a background task with no requesting principal, so it resolves
    /// names across the whole fleet. Two lines of comment said so; nothing checked it, and the
    /// seam deliberately keeps the argument so that it can be (ADR-112 decision 4).
    #[tokio::test]
    async fn a_report_never_narrows_its_node_name_lookup() {
        let h = harness()
            .top_fires(vec![(uid(1), 3)])
            .top_nodes(vec![(uid(2), 1.0)])
            .build();
        let _ = h
            .runner
            .render_top_alerting("a".to_owned(), &settings(serde_json::Value::Null), 0)
            .await;
        let _ = h
            .runner
            .render_top_metric(
                "b".to_owned(),
                "top-cpu",
                &settings(serde_json::Value::Null),
            )
            .await;
        let scopes = h.inventory.scopes.lock().expect("poisoned").clone();
        assert_eq!(scopes.len(), 2, "both name lookups were observed");
        assert!(
            scopes.iter().all(Option::is_none),
            "a report narrowed a name lookup to a group scope: {scopes:?}"
        );
    }
}
