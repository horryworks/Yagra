// SPDX-License-Identifier: AGPL-3.0-only
//! **How a report looks** — a section's value and its two outputs, the inline chart, the CSV, and
//! the document that wraps it all.
//!
//! Everything here runs on numbers already in hand: `super::guards` refuses an `.await` in this
//! file, which is what keeps the arithmetic of a section (`availability_from_snapshots`, the
//! metric selector, the SI formatting) reachable from a unit test. Seven of this module's eleven
//! tests are here for that reason.
//!
//! 🚨 **Device-supplied text is escaped on the way in** (`security.md`): node names and device
//! strings reach both the WebUI viewer and the PDF renderer, so [`esc`] is not optional decoration.
//!
//! Not split further into "a section" and "the document": [`esc`] is called from both halves and
//! [`Section`] straddles them, so the line would be arbitrary — and at 323 lines this is not even
//! the largest file here.

use super::*;

use std::collections::HashMap;
use std::fmt::Write as _;

use yagra_common::NodeState;

// ── Rendering: a section's structured output (data + HTML stay in sync) ───────────────────

/// A simple labelled table for a section.
#[derive(Debug, Clone, Default)]
pub(super) struct Table {
    pub(super) columns: Vec<String>,
    pub(super) rows: Vec<Vec<String>>,
}

/// A rendered section: a title, optional KPIs/table/chart. Produces both the structured JSON
/// (for CSV / re-render) and the HTML fragment (the in-app + PDF artifact) from one source.
#[derive(Debug, Clone, Default)]
pub(super) struct Section {
    pub(super) id: String,
    pub(super) kind: String,
    pub(super) title: String,
    pub(super) summary: Option<String>,
    pub(super) kpis: Vec<(String, String)>,
    pub(super) table: Option<Table>,
    pub(super) chart_svg: Option<String>,
    pub(super) note: Option<String>,
}

impl Section {
    pub(super) fn to_data(&self) -> serde_json::Value {
        let kpis: Vec<serde_json::Value> = self
            .kpis
            .iter()
            .map(|(l, v)| serde_json::json!({ "label": l, "value": v }))
            .collect();
        let table = self
            .table
            .as_ref()
            .map(|t| serde_json::json!({ "columns": t.columns, "rows": t.rows }));
        serde_json::json!({
            "id": self.id,
            "kind": self.kind,
            "title": self.title,
            "summary": self.summary,
            "kpis": kpis,
            "table": table,
        })
    }

    pub(super) fn to_html(&self) -> String {
        let mut h = String::new();
        let _ = write!(h, "<section class=\"rs\"><h2>{}</h2>", esc(&self.title));
        if let Some(s) = &self.summary {
            let _ = write!(h, "<p class=\"rs-sum\">{}</p>", esc(s));
        }
        if !self.kpis.is_empty() {
            h.push_str("<div class=\"rs-kpis\">");
            for (label, value) in &self.kpis {
                let _ = write!(
                    h,
                    "<div class=\"rs-kpi\"><div class=\"rs-kpi-v\">{}</div><div class=\"rs-kpi-l\">{}</div></div>",
                    esc(value),
                    esc(label)
                );
            }
            h.push_str("</div>");
        }
        if let Some(svg) = &self.chart_svg {
            let _ = write!(h, "<div class=\"rs-chart\">{svg}</div>");
        }
        if let Some(t) = &self.table {
            h.push_str("<table class=\"rs-table\"><thead><tr>");
            for c in &t.columns {
                let _ = write!(h, "<th>{}</th>", esc(c));
            }
            h.push_str("</tr></thead><tbody>");
            if t.rows.is_empty() {
                let _ = write!(
                    h,
                    "<tr><td class=\"rs-empty\" colspan=\"{}\">No data in this window.</td></tr>",
                    t.columns.len().max(1)
                );
            }
            for row in &t.rows {
                h.push_str("<tr>");
                for cell in row {
                    let _ = write!(h, "<td>{}</td>", esc(cell));
                }
                h.push_str("</tr>");
            }
            h.push_str("</tbody></table>");
        }
        if let Some(n) = &self.note {
            let _ = write!(h, "<p class=\"rs-note\">{}</p>", esc(n));
        }
        h.push_str("</section>");
        h
    }
}

/// HTML-escape untrusted text (node names, device strings) before embedding (security.md: device
/// data is untrusted; this HTML is rendered in the WebUI and fed to the PDF renderer).
fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// Human bits/sec with SI suffix (e.g. "1.2 Gbps").
pub(super) fn human_bps(v: f64) -> String {
    let units = ["bps", "Kbps", "Mbps", "Gbps", "Tbps"];
    let mut val = v;
    let mut i = 0;
    while val.abs() >= 1000.0 && i < units.len() - 1 {
        val /= 1000.0;
        i += 1;
    }
    format!("{val:.1} {}", units[i])
}

/// Sample step that keeps a window under [`MAX_POINTS`] samples (min 60s).
pub(super) fn read_step(from_s: i64, to_s: i64) -> u64 {
    let span = (to_s - from_s).max(1);
    ((span / MAX_POINTS).max(60)) as u64
}

/// The PromQL metric/selector + display label + unit for a top-metric section. CPU/memory expand to
/// a `{__name__=~"…"}` selector (the logical alias across vendors, mirrors api.rs); RTT is a plain
/// validated metric. Returns `None` for an unknown kind.
pub(super) fn top_metric_selector(kind: &str) -> Option<(String, &'static str, &'static str)> {
    match kind {
        "top-cpu" => Some((
            "{__name__=~\"huawei_cpu_usage|cisco_cpu_5min|nxos_cpu_util|fortinet_cpu_usage|juniper_cpu_1min|hr_processor_load\"}".to_owned(),
            "CPU",
            "%",
        )),
        "top-memory" => Some((
            "{__name__=~\"huawei_mem_usage|nxos_mem_util|fortinet_mem_usage\"}".to_owned(),
            "Memory",
            "%",
        )),
        "top-rtt" => Some(("icmp_rtt_ms".to_owned(), "RTT", "ms")),
        _ => None,
    }
}

/// Compute fleet uptime/availability from `node_state_snapshots` rows `(ts, state, count)`.
/// "up" = ok + warning (reachable, possibly degraded); "down" = critical + unreachable;
/// unknown and maintenance are excluded from the ratio. Returns `(uptime_pct, per_state_counts)`.
pub(super) fn availability_from_snapshots(
    rows: &[(i64, String, i64)],
) -> (Option<f64>, Vec<(String, i64)>) {
    let mut by_state: HashMap<String, i64> = HashMap::new();
    for (_, state, count) in rows {
        *by_state.entry(state.clone()).or_insert(0) += *count;
    }
    let get = |s: &str| -> i64 { by_state.get(s).copied().unwrap_or(0) };
    let sum = |states: &[NodeState]| -> i64 { states.iter().map(|s| get(s.as_str())).sum() };
    let up = sum(&[NodeState::Ok, NodeState::Warning]);
    let down = sum(&[NodeState::Critical, NodeState::Unreachable]);
    let denom = up + down;
    let uptime = if denom > 0 {
        Some(up as f64 / denom as f64 * 100.0)
    } else {
        None
    };
    // Stable display order, from the one enumeration (`NodeState::ALL`) rather than a third
    // hand-written copy of the six states.
    let order: Vec<&'static str> = NodeState::ALL.iter().map(NodeState::as_str).collect();
    let mut out: Vec<(String, i64)> = order
        .iter()
        .filter_map(|s| by_state.get(*s).map(|c| ((*s).to_owned(), *c)))
        .collect();
    // Any state not in the known order (forward-compatible) appended.
    for (s, c) in &by_state {
        if !order.contains(&s.as_str()) {
            out.push((s.clone(), *c));
        }
    }
    (uptime, out)
}

/// A line-chart series for the inline SVG renderer.
pub(super) struct LineSeries {
    pub(super) label: String,
    pub(super) color: &'static str,
    pub(super) points: Vec<(i64, f64)>,
}

/// Render an inline SVG line chart (no external deps, prints cleanly to PDF). Empty series ⇒ a
/// small "no data" placeholder.
pub(super) fn svg_line_chart(series: &[LineSeries]) -> String {
    const W: f64 = 720.0;
    const H: f64 = 220.0;
    const PAD: f64 = 28.0;
    let all: Vec<&(i64, f64)> = series.iter().flat_map(|s| s.points.iter()).collect();
    if all.is_empty() {
        return format!(
            "<svg viewBox=\"0 0 {W} {H}\" class=\"rs-svg\"><text x=\"{}\" y=\"{}\" \
             text-anchor=\"middle\" class=\"rs-svg-empty\">No data in this window.</text></svg>",
            W / 2.0,
            H / 2.0
        );
    }
    let (mut tmin, mut tmax) = (i64::MAX, i64::MIN);
    let (mut vmin, mut vmax) = (f64::INFINITY, f64::NEG_INFINITY);
    for (t, v) in &all {
        tmin = tmin.min(*t);
        tmax = tmax.max(*t);
        vmin = vmin.min(*v);
        vmax = vmax.max(*v);
    }
    if vmin > 0.0 {
        vmin = 0.0; // anchor charts at zero so magnitudes read true
    }
    let tspan = (tmax - tmin).max(1) as f64;
    let vspan = (vmax - vmin).max(1e-9);
    let x = |t: i64| PAD + (t - tmin) as f64 / tspan * (W - 2.0 * PAD);
    let y = |v: f64| H - PAD - (v - vmin) / vspan * (H - 2.0 * PAD);

    let mut svg = format!("<svg viewBox=\"0 0 {W} {H}\" class=\"rs-svg\">");
    // Axes (light).
    let _ = write!(
        svg,
        "<line x1=\"{PAD}\" y1=\"{0}\" x2=\"{1}\" y2=\"{0}\" class=\"rs-axis\"/>\
         <line x1=\"{PAD}\" y1=\"{PAD}\" x2=\"{PAD}\" y2=\"{0}\" class=\"rs-axis\"/>",
        H - PAD,
        W - PAD
    );
    for s in series {
        if s.points.is_empty() {
            continue;
        }
        let pts: String = s
            .points
            .iter()
            .map(|(t, v)| format!("{:.1},{:.1}", x(*t), y(*v)))
            .collect::<Vec<_>>()
            .join(" ");
        let _ = write!(
            svg,
            "<polyline fill=\"none\" stroke=\"{}\" stroke-width=\"2\" points=\"{}\"/>",
            s.color, pts
        );
    }
    // Legend.
    let mut lx = PAD;
    for s in series {
        let _ = write!(
            svg,
            "<rect x=\"{lx}\" y=\"6\" width=\"10\" height=\"10\" fill=\"{}\"/>\
             <text x=\"{}\" y=\"15\" class=\"rs-legend\">{}</text>",
            s.color,
            lx + 14.0,
            esc(&s.label)
        );
        lx += 110.0;
    }
    svg.push_str("</svg>");
    svg
}

/// Convert a stored result document into a CSV (one labelled block per section). Pure (testable).
#[must_use]
pub fn result_json_to_csv(result: &serde_json::Value) -> String {
    let mut out = String::new();
    let title = result
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or("Report");
    let _ = writeln!(out, "{}", csv_row(&[title]));
    let sections = result
        .get("sections")
        .and_then(|s| s.as_array())
        .cloned()
        .unwrap_or_default();
    for sec in &sections {
        out.push('\n');
        let stitle = sec
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("Section");
        let _ = writeln!(out, "{}", csv_row(&[stitle]));
        if let Some(summary) = sec.get("summary").and_then(|v| v.as_str()) {
            let _ = writeln!(out, "{}", csv_row(&[summary]));
        }
        if let Some(kpis) = sec.get("kpis").and_then(|v| v.as_array()) {
            for k in kpis {
                let label = k.get("label").and_then(|v| v.as_str()).unwrap_or("");
                let value = k.get("value").and_then(|v| v.as_str()).unwrap_or("");
                let _ = writeln!(out, "{}", csv_row(&[label, value]));
            }
        }
        if let Some(table) = sec.get("table") {
            if let Some(cols) = table.get("columns").and_then(|v| v.as_array()) {
                let cols: Vec<String> = cols
                    .iter()
                    .map(|c| c.as_str().unwrap_or("").to_owned())
                    .collect();
                let refs: Vec<&str> = cols.iter().map(String::as_str).collect();
                let _ = writeln!(out, "{}", csv_row(&refs));
            }
            if let Some(rows) = table.get("rows").and_then(|v| v.as_array()) {
                for row in rows {
                    if let Some(cells) = row.as_array() {
                        let cells: Vec<String> = cells
                            .iter()
                            .map(|c| c.as_str().unwrap_or("").to_owned())
                            .collect();
                        let refs: Vec<&str> = cells.iter().map(String::as_str).collect();
                        let _ = writeln!(out, "{}", csv_row(&refs));
                    }
                }
            }
        }
    }
    out
}

// `csv_row` / `csv_escape` lived here and are gone. They quoted per RFC 4180 and stopped there —
// no spreadsheet-formula neutralization — while a report table's cells are device-supplied
// (`sysDescr`, `ifAlias`, a node name an operator typed). The WebUI had already paid for that
// exact omission once; this was the same hole, quieter, on the surface nobody re-read. Both now
// go through `crate::csv`, which is one encoder and therefore one security boundary
// (`extensibility.md` §3).
use crate::csv::row as csv_row;

/// Wrap section HTML fragments in a self-contained, print-friendly HTML document. The CSS is inline
/// so the same document renders in the WebUI viewer and the PDF renderer (WYSIWYG).
pub(super) fn render_document(
    title: &str,
    window_label: &str,
    generated_label: &str,
    body: &str,
) -> String {
    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <title>{title}</title><style>{css}</style></head><body>\
         <header class=\"rs-head\"><h1>{title}</h1>\
         <div class=\"rs-meta\">{window} · {generated}</div></header>\
         <main>{body}</main></body></html>",
        title = esc(title),
        window = esc(window_label),
        generated = esc(generated_label),
        css = REPORT_CSS,
    )
}

/// Inline stylesheet for the rendered report (light, print-oriented).
const REPORT_CSS: &str = "\
*{box-sizing:border-box}\
body{font-family:-apple-system,Segoe UI,Roboto,Helvetica,Arial,sans-serif;color:#1a2230;margin:0;padding:24px;background:#fff;font-size:13px;line-height:1.45}\
.rs-head{border-bottom:2px solid #1a2230;padding-bottom:10px;margin-bottom:20px}\
.rs-head h1{margin:0 0 4px;font-size:22px}\
.rs-meta{color:#5a6678;font-size:12px}\
section.rs{margin:0 0 26px;page-break-inside:avoid}\
section.rs h2{font-size:15px;margin:0 0 8px;border-left:3px solid #2f6df6;padding-left:8px}\
.rs-sum{color:#5a6678;margin:0 0 10px}\
.rs-kpis{display:flex;flex-wrap:wrap;gap:14px;margin:0 0 12px}\
.rs-kpi{border:1px solid #e2e6ee;border-radius:8px;padding:10px 14px;min-width:120px}\
.rs-kpi-v{font-size:20px;font-weight:600}\
.rs-kpi-l{font-size:11px;color:#5a6678;text-transform:uppercase;letter-spacing:.03em}\
table.rs-table{border-collapse:collapse;width:100%;margin-top:4px}\
table.rs-table th{text-align:left;font-size:11px;color:#5a6678;text-transform:uppercase;letter-spacing:.03em;border-bottom:1px solid #d6dbe6;padding:6px 8px}\
table.rs-table td{padding:6px 8px;border-bottom:1px solid #eef1f6}\
.rs-empty{color:#8a93a6;font-style:italic}\
.rs-note{color:#8a93a6;font-size:11px;margin-top:6px}\
.rs-chart{margin:6px 0}\
.rs-svg{width:100%;height:auto}\
.rs-axis{stroke:#d6dbe6;stroke-width:1}\
.rs-legend{font-size:11px;fill:#5a6678}\
.rs-svg-empty{font-size:13px;fill:#8a93a6;font-style:italic}\
";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_html_neutralizes_markup() {
        assert_eq!(esc("<b>&\"'"), "&lt;b&gt;&amp;&quot;&#39;");
        assert_eq!(esc("plain text"), "plain text");
    }

    #[test]
    fn human_bps_scales_si() {
        assert_eq!(human_bps(500.0), "500.0 bps");
        assert_eq!(human_bps(1_500.0), "1.5 Kbps");
        assert_eq!(human_bps(2_500_000.0), "2.5 Mbps");
        assert_eq!(human_bps(3_000_000_000.0), "3.0 Gbps");
    }

    #[test]
    fn top_metric_selector_expands_cpu_and_memory_and_rtt() {
        let (cpu, label, unit) = top_metric_selector("top-cpu").unwrap();
        assert!(cpu.starts_with("{__name__=~\"") && cpu.contains("hr_processor_load"));
        assert_eq!((label, unit), ("CPU", "%"));
        assert_eq!(top_metric_selector("top-rtt").unwrap().0, "icmp_rtt_ms");
        assert!(top_metric_selector("top-memory")
            .unwrap()
            .0
            .contains("huawei_mem_usage"));
        assert!(top_metric_selector("nope").is_none());
    }

    #[test]
    fn availability_excludes_unknown_and_maintenance() {
        // ok+warning = 80 up; critical+unreachable = 20 down ⇒ 80% uptime. unknown/maintenance ignored.
        let rows = vec![
            (0, "ok".to_owned(), 70),
            (0, "warning".to_owned(), 10),
            (0, "critical".to_owned(), 5),
            (0, "unreachable".to_owned(), 15),
            (0, "unknown".to_owned(), 100),
            (0, "maintenance".to_owned(), 50),
        ];
        let (uptime, per_state) = availability_from_snapshots(&rows);
        assert!((uptime.unwrap() - 80.0).abs() < 1e-9);
        // Per-state preserves all states in display order.
        assert_eq!(per_state[0], ("ok".to_owned(), 70));
        assert_eq!(per_state.len(), 6);
    }

    #[test]
    fn availability_none_when_no_reachability_samples() {
        let rows = vec![(0, "unknown".to_owned(), 10)];
        assert!(availability_from_snapshots(&rows).0.is_none());
    }

    #[test]
    fn csv_escapes_and_blocks() {
        let result = serde_json::json!({
            "title": "My, Report",
            "sections": [{
                "title": "Top CPU",
                "summary": "2 nodes",
                "kpis": [{"label": "Peak", "value": "88%"}],
                "table": { "columns": ["Node", "CPU"], "rows": [["r1", "88.0 %"], ["r\"2", "10"]] }
            }]
        });
        let csv = result_json_to_csv(&result);
        // Every field is quoted, not only the ones containing a comma. Deciding per value is a
        // rule that can be got wrong once and corrupt every row after it — which is why the shared
        // encoder does not offer the choice.
        assert!(csv.contains("\"My, Report\""));
        assert!(csv.contains("\"Node\",\"CPU\""));
        assert!(csv.contains("\"r\"\"2\",\"10\"")); // embedded quote doubled
        assert!(csv.contains("\"Peak\",\"88%\""));
    }

    #[test]
    fn a_report_cell_cannot_carry_a_spreadsheet_formula_out_of_the_product() {
        // Report tables are built from device-supplied strings — a node name, `ifAlias`,
        // `sysDescr`. This export went out for its whole life with RFC 4180 quoting and nothing
        // else, which a spreadsheet strips before evaluating the text underneath.
        let result = serde_json::json!({
            "title": "T",
            "sections": [{
                "title": "S",
                "table": {
                    "columns": ["Node"],
                    "rows": [["=HYPERLINK(\"http://evil\",\"click\")"], ["-0.5"]]
                }
            }]
        });
        let csv = result_json_to_csv(&result);
        assert!(csv.contains("\"'=HYPERLINK"), "not neutralized: {csv}");
        // …and a negative number is still a number, or every numeric column becomes text.
        assert!(csv.contains("\"-0.5\""), "{csv}");
    }
}
