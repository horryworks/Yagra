// SPDX-License-Identifier: AGPL-3.0-only
//! **Where each section's numbers come from** — the six \`render_*\`, one per section kind.
//!
//! Each one fetches from the seams [\`super::runner\`] holds and hands the result to
//! [\`super::render\`]; the arithmetic they need is over there, where a test can reach it. That
//! division is the whole reason the split line is "does it \`.await\`" — see [\`super\`].
//!
//! Adding a section: an entry in [\`super::catalog\`], an arm in
//! [\`super::runner::ReportRunner::render_section\`], and a \`render_*\` here. Two of the three are
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
        let from_ms = from_s * 1000;
        // Fires in the window, by severity (resolved=false records since `from`).
        let recent = self
            .history
            .recent(1000, None, None)
            .await
            .unwrap_or_default();
        let mut fires: HashMap<String, i64> = HashMap::new();
        for r in &recent {
            if !r.resolved && r.at_unix_ms >= from_ms {
                *fires.entry(r.severity.as_str().to_owned()).or_insert(0) += 1;
            }
        }
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
            .history
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
