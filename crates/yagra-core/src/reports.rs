//! Reports (Dashboard → Reports).
//!
//! A **report definition** is a reusable, customizable template — a name plus an opaque `spec`
//! (the selected sections + their settings + the time range). A **schedule** runs a definition on a
//! preset cadence. A **run** is one generated report, saved for later viewing/export. Reports are a
//! SHARED resource: everyone reads, only admins write (mirrors `shared_dashboard`; the write gate is
//! at the API edge).
//!
//! Generation is a TSDB + PostgreSQL read computation — it never touches a device, so (like the
//! analysis runner, ADR-022) it runs as a background `tokio` task inside core, not a poller/bus job.
//! [`ReportRunner::run_now`] inserts a run row, spawns the task, and returns immediately; the task
//! renders each section (querying the same store/inventory/alert/history seams the rest of core
//! uses), persists the result (structured JSON + rendered HTML), and broadcasts progress over SSE.
//! Definitions/schedules/runs are metadata, so they live in PostgreSQL ([`ReportsRepo`], ADR-004).

use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::{DateTime, Datelike, Duration as ChronoDuration, NaiveDate, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row};
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::alerts::AlertManager;
use crate::history::AlertHistoryStore;
use crate::repo::NodeRepo;
use crate::store::{MetricStore, TopAgg};

/// Broadcast buffer for the run-status SSE stream (matches the analysis runner's sizing).
const EVENT_BUFFER: usize = 256;
/// Default report window when a spec omits `range_secs` (7 days).
const DEFAULT_RANGE_SECS: i64 = 7 * 86_400;
/// Target sample count for a time-series section (bounds the step so a long window stays cheap).
const MAX_POINTS: i64 = 240;

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

fn now_s() -> i64 {
    now_ms() / 1000
}

// ── Persisted shapes ──────────────────────────────────────────────────────────────────

/// A report definition (reusable template), as served to the API.
#[derive(Debug, Clone, Serialize)]
pub struct ReportDefinition {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub spec: serde_json::Value,
    pub updated_by: Option<String>,
    pub created_ms: i64,
    pub updated_ms: i64,
}

/// A schedule row (joined with its definition's name for display).
#[derive(Debug, Clone, Serialize)]
pub struct ReportSchedule {
    pub id: Uuid,
    pub definition_id: Uuid,
    pub definition_name: String,
    pub frequency: String,
    pub day_of_week: Option<i16>,
    pub day_of_month: Option<i16>,
    pub at_hour: i16,
    pub at_minute: i16,
    pub enabled: bool,
    pub next_run_ms: i64,
    pub last_run_ms: Option<i64>,
    pub last_status: Option<String>,
}

/// A run row for the saved-reports list (without the heavy `result_*` payloads).
#[derive(Debug, Clone, Serialize)]
pub struct ReportRun {
    pub id: Uuid,
    pub definition_id: Option<Uuid>,
    pub name: String,
    pub trigger: String,
    pub state: String,
    pub pct: i32,
    pub error: Option<String>,
    pub range_from_ms: Option<i64>,
    pub range_to_ms: Option<i64>,
    pub section_count: i32,
    pub created_by: Option<String>,
    pub created_ms: i64,
    pub started_ms: Option<i64>,
    pub finished_ms: Option<i64>,
}

/// A run plus its rendered result (the viewer / export endpoints).
#[derive(Debug, Clone, Serialize)]
pub struct ReportRunDetail {
    #[serde(flatten)]
    pub run: ReportRun,
    pub result_json: Option<serde_json::Value>,
    pub result_html: Option<String>,
}

// ── Spec parsing (the opaque definition document, parsed at generation time) ─────────────

/// The report document the WebUI owns. Parsed leniently (unknown fields/sections tolerated) so a
/// newer WebUI shape stays compatible with an older core (ADR-017).
#[derive(Debug, Clone, Deserialize, Default)]
struct ReportSpec {
    #[serde(default)]
    params: ReportParams,
    #[serde(default)]
    sections: Vec<SectionSpec>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct ReportParams {
    #[serde(default)]
    range_secs: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
struct SectionSpec {
    #[serde(default)]
    id: Option<String>,
    kind: String,
    #[serde(default)]
    settings: serde_json::Value,
}

/// Read a numeric setting (accepts JSON number or numeric string), clamped by the caller.
fn setting_i64(settings: &serde_json::Value, key: &str, default: i64) -> i64 {
    match settings.get(key) {
        Some(serde_json::Value::Number(n)) => n.as_i64().unwrap_or(default),
        Some(serde_json::Value::String(s)) => s.parse().unwrap_or(default),
        _ => default,
    }
}

/// Read a string setting.
fn setting_str(settings: &serde_json::Value, key: &str, default: &str) -> String {
    settings
        .get(key)
        .and_then(|v| v.as_str())
        .unwrap_or(default)
        .to_owned()
}

// ── Section catalog (drives the builder + validates kinds) ────────────────────────────────

/// One selectable choice for a `select` setting.
#[derive(Debug, Clone, Serialize)]
pub struct SettingOption {
    pub value: &'static str,
    pub label: &'static str,
}

/// A configurable setting on a section (rendered generically by the builder).
#[derive(Debug, Clone, Serialize)]
pub struct SectionSetting {
    pub key: &'static str,
    pub label: &'static str,
    /// `number` | `select`.
    pub kind: &'static str,
    pub default: serde_json::Value,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<SettingOption>,
}

/// A report-section type the user can add to a report.
#[derive(Debug, Clone, Serialize)]
pub struct SectionDef {
    pub kind: &'static str,
    pub title: &'static str,
    pub blurb: &'static str,
    pub group: &'static str,
    pub settings: Vec<SectionSetting>,
}

fn agg_setting() -> SectionSetting {
    SectionSetting {
        key: "agg",
        label: "Aggregation",
        kind: "select",
        default: serde_json::json!("max_1h"),
        options: vec![
            SettingOption {
                value: "now",
                label: "Current",
            },
            SettingOption {
                value: "max_1h",
                label: "1h peak",
            },
        ],
    }
}

fn limit_setting(default: i64) -> SectionSetting {
    SectionSetting {
        key: "limit",
        label: "Rows",
        kind: "number",
        default: serde_json::json!(default),
        options: Vec::new(),
    }
}

/// The catalog of available report sections (served at `/reports/sections`, mirrors the dashboard
/// widget registry). Each maps to an existing data source; the builder renders `settings` generically.
#[must_use]
pub fn section_catalog() -> Vec<SectionDef> {
    vec![
        SectionDef {
            kind: "availability-summary",
            title: "Availability summary (SLA)",
            blurb: "Fleet uptime % and per-state share over the report window.",
            group: "Health",
            settings: Vec::new(),
        },
        SectionDef {
            kind: "alert-summary",
            title: "Alert summary",
            blurb: "Active alerts now and alert fires in the window, by severity.",
            group: "Alerts",
            settings: Vec::new(),
        },
        SectionDef {
            kind: "top-alerting-nodes",
            title: "Top alerting nodes",
            blurb: "Nodes with the most alert fires in the window (chronic offenders).",
            group: "Alerts",
            settings: vec![limit_setting(10)],
        },
        SectionDef {
            kind: "top-cpu",
            title: "Top CPU",
            blurb: "Highest-CPU nodes fleet-wide.",
            group: "Performance",
            settings: vec![limit_setting(10), agg_setting()],
        },
        SectionDef {
            kind: "top-rtt",
            title: "Top latency (RTT)",
            blurb: "Highest ICMP round-trip-time nodes fleet-wide.",
            group: "Performance",
            settings: vec![limit_setting(10), agg_setting()],
        },
        SectionDef {
            kind: "top-memory",
            title: "Top memory",
            blurb: "Highest-memory-usage nodes fleet-wide.",
            group: "Performance",
            settings: vec![limit_setting(10), agg_setting()],
        },
        SectionDef {
            kind: "throughput-trend",
            title: "Throughput trend",
            blurb: "Fleet aggregate in/out throughput over the window.",
            group: "Capacity",
            settings: Vec::new(),
        },
        SectionDef {
            kind: "inventory-listing",
            title: "Inventory listing",
            blurb: "All monitored nodes with their current state.",
            group: "Inventory",
            settings: vec![limit_setting(200)],
        },
    ]
}

/// Whether `kind` is a known section type (the API edge validates a definition's sections).
#[must_use]
pub fn is_known_section(kind: &str) -> bool {
    section_catalog().iter().any(|s| s.kind == kind)
}

// ── Rendering: a section's structured output (data + HTML stay in sync) ───────────────────

/// A simple labelled table for a section.
#[derive(Debug, Clone, Default)]
struct Table {
    columns: Vec<String>,
    rows: Vec<Vec<String>>,
}

/// A rendered section: a title, optional KPIs/table/chart. Produces both the structured JSON
/// (for CSV / re-render) and the HTML fragment (the in-app + PDF artifact) from one source.
#[derive(Debug, Clone, Default)]
struct Section {
    id: String,
    kind: String,
    title: String,
    summary: Option<String>,
    kpis: Vec<(String, String)>,
    table: Option<Table>,
    chart_svg: Option<String>,
    note: Option<String>,
}

impl Section {
    fn to_data(&self) -> serde_json::Value {
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

    fn to_html(&self) -> String {
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
fn human_bps(v: f64) -> String {
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
fn read_step(from_s: i64, to_s: i64) -> u64 {
    let span = (to_s - from_s).max(1);
    ((span / MAX_POINTS).max(60)) as u64
}

/// The PromQL metric/selector + display label + unit for a top-metric section. CPU/memory expand to
/// a `{__name__=~"…"}` selector (the logical alias across vendors, mirrors api.rs); RTT is a plain
/// validated metric. Returns `None` for an unknown kind.
fn top_metric_selector(kind: &str) -> Option<(String, &'static str, &'static str)> {
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
fn availability_from_snapshots(rows: &[(i64, String, i64)]) -> (Option<f64>, Vec<(String, i64)>) {
    let mut by_state: HashMap<String, i64> = HashMap::new();
    for (_, state, count) in rows {
        *by_state.entry(state.clone()).or_insert(0) += *count;
    }
    let get = |s: &str| -> i64 { by_state.get(s).copied().unwrap_or(0) };
    let up = get("ok") + get("warning");
    let down = get("critical") + get("unreachable");
    let denom = up + down;
    let uptime = if denom > 0 {
        Some(up as f64 / denom as f64 * 100.0)
    } else {
        None
    };
    // Stable display order.
    let order = [
        "ok",
        "warning",
        "critical",
        "unreachable",
        "unknown",
        "maintenance",
    ];
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
struct LineSeries {
    label: String,
    color: &'static str,
    points: Vec<(i64, f64)>,
}

/// Render an inline SVG line chart (no external deps, prints cleanly to PDF). Empty series ⇒ a
/// small "no data" placeholder.
fn svg_line_chart(series: &[LineSeries]) -> String {
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

/// Join fields into one CSV record (RFC-4180 quoting).
fn csv_row(fields: &[&str]) -> String {
    fields
        .iter()
        .map(|f| csv_escape(f))
        .collect::<Vec<_>>()
        .join(",")
}

fn csv_escape(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_owned()
    }
}

/// Wrap section HTML fragments in a self-contained, print-friendly HTML document. The CSS is inline
/// so the same document renders in the WebUI viewer and the PDF renderer (WYSIWYG).
fn render_document(title: &str, window_label: &str, generated_label: &str, body: &str) -> String {
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

// ── Repository ───────────────────────────────────────────────────────────────────────

const DEF_COLS: &str = "id, name, description, spec, updated_by, \
     (EXTRACT(EPOCH FROM created_at) * 1000)::bigint AS created_ms, \
     (EXTRACT(EPOCH FROM updated_at) * 1000)::bigint AS updated_ms";

const RUN_COLS: &str = "id, definition_id, name, trigger, state, pct, error, \
     (EXTRACT(EPOCH FROM range_from) * 1000)::bigint AS range_from_ms, \
     (EXTRACT(EPOCH FROM range_to) * 1000)::bigint AS range_to_ms, \
     section_count, created_by, \
     (EXTRACT(EPOCH FROM created_at) * 1000)::bigint AS created_ms, \
     (EXTRACT(EPOCH FROM started_at) * 1000)::bigint AS started_ms, \
     (EXTRACT(EPOCH FROM finished_at) * 1000)::bigint AS finished_ms";

const SCHED_COLS: &str = "s.id, s.definition_id, d.name AS definition_name, s.frequency, \
     s.day_of_week, s.day_of_month, s.at_hour, s.at_minute, s.enabled, \
     (EXTRACT(EPOCH FROM s.next_run_at) * 1000)::bigint AS next_run_ms, \
     (EXTRACT(EPOCH FROM s.last_run_at) * 1000)::bigint AS last_run_ms, s.last_status";

fn def_from_row(row: &sqlx::postgres::PgRow) -> anyhow::Result<ReportDefinition> {
    Ok(ReportDefinition {
        id: row.try_get("id")?,
        name: row.try_get("name")?,
        description: row.try_get("description")?,
        spec: row.try_get("spec")?,
        updated_by: row.try_get("updated_by")?,
        created_ms: row.try_get("created_ms")?,
        updated_ms: row.try_get("updated_ms")?,
    })
}

fn run_from_row(row: &sqlx::postgres::PgRow) -> anyhow::Result<ReportRun> {
    Ok(ReportRun {
        id: row.try_get("id")?,
        definition_id: row.try_get("definition_id")?,
        name: row.try_get("name")?,
        trigger: row.try_get("trigger")?,
        state: row.try_get("state")?,
        pct: row.try_get("pct")?,
        error: row.try_get("error")?,
        range_from_ms: row.try_get("range_from_ms")?,
        range_to_ms: row.try_get("range_to_ms")?,
        section_count: row.try_get("section_count")?,
        created_by: row.try_get("created_by")?,
        created_ms: row.try_get("created_ms")?,
        started_ms: row.try_get("started_ms")?,
        finished_ms: row.try_get("finished_ms")?,
    })
}

fn sched_from_row(row: &sqlx::postgres::PgRow) -> anyhow::Result<ReportSchedule> {
    Ok(ReportSchedule {
        id: row.try_get("id")?,
        definition_id: row.try_get("definition_id")?,
        definition_name: row.try_get("definition_name")?,
        frequency: row.try_get("frequency")?,
        day_of_week: row.try_get("day_of_week")?,
        day_of_month: row.try_get("day_of_month")?,
        at_hour: row.try_get("at_hour")?,
        at_minute: row.try_get("at_minute")?,
        enabled: row.try_get("enabled")?,
        next_run_ms: row.try_get("next_run_ms")?,
        last_run_ms: row.try_get("last_run_ms")?,
        last_status: row.try_get("last_status")?,
    })
}

/// Validated fields for creating/updating a schedule (parsed at the API edge).
#[derive(Debug, Clone)]
pub struct ScheduleInput {
    pub definition_id: Uuid,
    pub frequency: String,
    pub day_of_week: Option<i16>,
    pub day_of_month: Option<i16>,
    pub at_hour: i16,
    pub at_minute: i16,
    pub enabled: bool,
}

/// PostgreSQL store for report definitions, schedules, and runs.
pub struct ReportsRepo {
    pool: PgPool,
}

impl ReportsRepo {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    // — Definitions —

    pub async fn list_definitions(&self) -> anyhow::Result<Vec<ReportDefinition>> {
        let rows = sqlx::query(&format!(
            "SELECT {DEF_COLS} FROM report_definitions ORDER BY name"
        ))
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(def_from_row).collect()
    }

    pub async fn get_definition(&self, id: Uuid) -> anyhow::Result<Option<ReportDefinition>> {
        let row = sqlx::query(&format!(
            "SELECT {DEF_COLS} FROM report_definitions WHERE id = $1"
        ))
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(def_from_row).transpose()
    }

    pub async fn create_definition(
        &self,
        name: &str,
        description: Option<&str>,
        spec: &serde_json::Value,
        updated_by: Option<&str>,
    ) -> anyhow::Result<ReportDefinition> {
        let id = Uuid::new_v4();
        let row = sqlx::query(&format!(
            "INSERT INTO report_definitions (id, name, description, spec, updated_by) \
             VALUES ($1, $2, $3, $4, $5) RETURNING {DEF_COLS}"
        ))
        .bind(id)
        .bind(name)
        .bind(description)
        .bind(spec)
        .bind(updated_by)
        .fetch_one(&self.pool)
        .await?;
        def_from_row(&row)
    }

    pub async fn update_definition(
        &self,
        id: Uuid,
        name: &str,
        description: Option<&str>,
        spec: &serde_json::Value,
        updated_by: Option<&str>,
    ) -> anyhow::Result<bool> {
        let res = sqlx::query(
            "UPDATE report_definitions SET name = $2, description = $3, spec = $4, \
             updated_by = $5, updated_at = now() WHERE id = $1",
        )
        .bind(id)
        .bind(name)
        .bind(description)
        .bind(spec)
        .bind(updated_by)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() > 0)
    }

    pub async fn delete_definition(&self, id: Uuid) -> anyhow::Result<bool> {
        let res = sqlx::query("DELETE FROM report_definitions WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }

    // — Schedules —

    pub async fn list_schedules(&self) -> anyhow::Result<Vec<ReportSchedule>> {
        let rows = sqlx::query(&format!(
            "SELECT {SCHED_COLS} FROM report_schedules s \
             JOIN report_definitions d ON d.id = s.definition_id ORDER BY s.next_run_at"
        ))
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(sched_from_row).collect()
    }

    pub async fn create_schedule(
        &self,
        input: &ScheduleInput,
        next_run_at: DateTime<Utc>,
        updated_by: Option<&str>,
    ) -> anyhow::Result<Uuid> {
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO report_schedules \
             (id, definition_id, frequency, day_of_week, day_of_month, at_hour, at_minute, \
              enabled, next_run_at, updated_by) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
        )
        .bind(id)
        .bind(input.definition_id)
        .bind(&input.frequency)
        .bind(input.day_of_week)
        .bind(input.day_of_month)
        .bind(input.at_hour)
        .bind(input.at_minute)
        .bind(input.enabled)
        .bind(next_run_at)
        .bind(updated_by)
        .execute(&self.pool)
        .await?;
        Ok(id)
    }

    pub async fn update_schedule(
        &self,
        id: Uuid,
        input: &ScheduleInput,
        next_run_at: DateTime<Utc>,
        updated_by: Option<&str>,
    ) -> anyhow::Result<bool> {
        let res = sqlx::query(
            "UPDATE report_schedules SET definition_id = $2, frequency = $3, day_of_week = $4, \
             day_of_month = $5, at_hour = $6, at_minute = $7, enabled = $8, next_run_at = $9, \
             updated_by = $10, updated_at = now() WHERE id = $1",
        )
        .bind(id)
        .bind(input.definition_id)
        .bind(&input.frequency)
        .bind(input.day_of_week)
        .bind(input.day_of_month)
        .bind(input.at_hour)
        .bind(input.at_minute)
        .bind(input.enabled)
        .bind(next_run_at)
        .bind(updated_by)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() > 0)
    }

    pub async fn delete_schedule(&self, id: Uuid) -> anyhow::Result<bool> {
        let res = sqlx::query("DELETE FROM report_schedules WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }

    /// Enabled schedules whose `next_run_at` has passed (the scheduler's due-query).
    pub async fn due_schedules(&self) -> anyhow::Result<Vec<ReportSchedule>> {
        let rows = sqlx::query(&format!(
            "SELECT {SCHED_COLS} FROM report_schedules s \
             JOIN report_definitions d ON d.id = s.definition_id \
             WHERE s.enabled = true AND s.next_run_at <= now() ORDER BY s.next_run_at"
        ))
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(sched_from_row).collect()
    }

    /// Record a fire: stamp `last_run_at`/`last_status` and advance `next_run_at`.
    pub async fn mark_fired(
        &self,
        id: Uuid,
        status: &str,
        next_run_at: DateTime<Utc>,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "UPDATE report_schedules SET last_run_at = now(), last_status = $2, next_run_at = $3 \
             WHERE id = $1",
        )
        .bind(id)
        .bind(status)
        .bind(next_run_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    // — Runs —

    pub async fn list_runs(&self, limit: i64) -> anyhow::Result<Vec<ReportRun>> {
        let rows = sqlx::query(&format!(
            "SELECT {RUN_COLS} FROM report_runs ORDER BY created_at DESC LIMIT $1"
        ))
        .bind(limit.clamp(1, 500))
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(run_from_row).collect()
    }

    pub async fn get_run(&self, id: Uuid) -> anyhow::Result<Option<ReportRun>> {
        let row = sqlx::query(&format!("SELECT {RUN_COLS} FROM report_runs WHERE id = $1"))
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(run_from_row).transpose()
    }

    /// A run plus its rendered payloads (the viewer / export endpoints).
    pub async fn get_run_detail(&self, id: Uuid) -> anyhow::Result<Option<ReportRunDetail>> {
        let row = sqlx::query(&format!(
            "SELECT {RUN_COLS}, result_json, result_html FROM report_runs WHERE id = $1"
        ))
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        match row {
            Some(row) => {
                let run = run_from_row(&row)?;
                Ok(Some(ReportRunDetail {
                    run,
                    result_json: row.try_get("result_json")?,
                    result_html: row.try_get("result_html")?,
                }))
            }
            None => Ok(None),
        }
    }

    pub async fn delete_run(&self, id: Uuid) -> anyhow::Result<bool> {
        let res = sqlx::query("DELETE FROM report_runs WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }

    /// Insert a run in the `running` state and return its row.
    #[allow(clippy::too_many_arguments)]
    async fn insert_run(
        &self,
        definition_id: Option<Uuid>,
        name: &str,
        trigger: &str,
        from_s: i64,
        to_s: i64,
        section_count: i32,
        spec_snapshot: &serde_json::Value,
        created_by: Option<&str>,
    ) -> anyhow::Result<ReportRun> {
        let id = Uuid::new_v4();
        let row = sqlx::query(&format!(
            "INSERT INTO report_runs \
             (id, definition_id, name, trigger, state, pct, range_from, range_to, \
              section_count, spec_snapshot, created_by, started_at) \
             VALUES ($1, $2, $3, $4, 'running', 0, to_timestamp($5), to_timestamp($6), \
                     $7, $8, $9, now()) \
             RETURNING {RUN_COLS}"
        ))
        .bind(id)
        .bind(definition_id)
        .bind(name)
        .bind(trigger)
        .bind(from_s)
        .bind(to_s)
        .bind(section_count)
        .bind(spec_snapshot)
        .bind(created_by)
        .fetch_one(&self.pool)
        .await?;
        run_from_row(&row)
    }

    async fn set_run_progress(&self, id: Uuid, pct: i32) -> anyhow::Result<()> {
        sqlx::query("UPDATE report_runs SET pct = $2 WHERE id = $1 AND state = 'running'")
            .bind(id)
            .bind(pct.clamp(0, 100))
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn finish_run(
        &self,
        id: Uuid,
        result_json: &serde_json::Value,
        result_html: &str,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "UPDATE report_runs SET state = 'succeeded', pct = 100, result_json = $2, \
             result_html = $3, finished_at = now() WHERE id = $1",
        )
        .bind(id)
        .bind(result_json)
        .bind(result_html)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn fail_run(&self, id: Uuid, error: &str) -> anyhow::Result<()> {
        sqlx::query(
            "UPDATE report_runs SET state = 'failed', error = $2, finished_at = now() WHERE id = $1",
        )
        .bind(id)
        .bind(error)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// On startup, fail any run left `running`/`queued` by a previous process (it can't resume).
    pub async fn fail_orphans(&self) -> anyhow::Result<u64> {
        let res = sqlx::query(
            "UPDATE report_runs SET state = 'failed', \
             error = 'core restarted while running', finished_at = now() \
             WHERE state IN ('queued', 'running')",
        )
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected())
    }

    /// Delete runs older than `older_than_secs` (retention). Returns rows removed.
    pub async fn prune_runs(&self, older_than_secs: i64) -> anyhow::Result<u64> {
        let res = sqlx::query(
            "DELETE FROM report_runs WHERE created_at < now() - ($1::double precision * interval '1 second')",
        )
        .bind(older_than_secs as f64)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected())
    }
}

// ── Schedule maths (preset cadence → next firing) ────────────────────────────────────────

/// Compute the next firing instant for a preset schedule, strictly after `now`. Unknown frequency
/// falls back to daily. Pure (testable).
#[must_use]
pub fn compute_next_run(
    frequency: &str,
    day_of_week: Option<i16>,
    day_of_month: Option<i16>,
    at_hour: i16,
    at_minute: i16,
    now: DateTime<Utc>,
) -> DateTime<Utc> {
    let hour = at_hour.clamp(0, 23) as u32;
    let minute = at_minute.clamp(0, 59) as u32;
    let at_time = |date: NaiveDate| -> DateTime<Utc> {
        let naive = date
            .and_hms_opt(hour, minute, 0)
            .unwrap_or_else(|| date.and_hms_opt(0, 0, 0).unwrap_or_else(|| now.naive_utc()));
        Utc.from_utc_datetime(&naive)
    };
    match frequency {
        "weekly" => {
            // 0=Sun..6=Sat; chrono's num_days_from_sunday matches.
            let target = day_of_week.unwrap_or(0).clamp(0, 6) as i64;
            let current = i64::from(now.weekday().num_days_from_sunday());
            let mut days = (target - current).rem_euclid(7);
            let mut candidate = at_time(now.date_naive() + ChronoDuration::days(days));
            if candidate <= now {
                days += 7;
                candidate = at_time(now.date_naive() + ChronoDuration::days(days));
            }
            candidate
        }
        "monthly" => {
            // Clamp to 28 so every month has the day.
            let dom = day_of_month.unwrap_or(1).clamp(1, 28) as u32;
            let (mut y, mut m) = (now.year(), now.month());
            if let Some(date) = NaiveDate::from_ymd_opt(y, m, dom) {
                let candidate = at_time(date);
                if candidate > now {
                    return candidate;
                }
            }
            // Advance to next month.
            if m == 12 {
                y += 1;
                m = 1;
            } else {
                m += 1;
            }
            let date = NaiveDate::from_ymd_opt(y, m, dom)
                .unwrap_or_else(|| now.date_naive() + ChronoDuration::days(28));
            at_time(date)
        }
        // "daily" and any unknown frequency.
        _ => {
            let candidate = at_time(now.date_naive());
            if candidate > now {
                candidate
            } else {
                at_time(now.date_naive() + ChronoDuration::days(1))
            }
        }
    }
}

// ── Runner ─────────────────────────────────────────────────────────────────────────────

/// Orchestrates report generation: create a run → background task → render sections (progress
/// persisted + broadcast over SSE) → persist the result. Holds the read seams the renderers use.
pub struct ReportRunner {
    repo: Arc<ReportsRepo>,
    store: Arc<dyn MetricStore>,
    nodes: Arc<NodeRepo>,
    alerts: Arc<AlertManager>,
    history: Arc<AlertHistoryStore>,
    tx: broadcast::Sender<String>,
}

impl ReportRunner {
    #[must_use]
    pub fn new(
        repo: Arc<ReportsRepo>,
        store: Arc<dyn MetricStore>,
        nodes: Arc<NodeRepo>,
        alerts: Arc<AlertManager>,
        history: Arc<AlertHistoryStore>,
    ) -> Self {
        let (tx, _) = broadcast::channel(EVENT_BUFFER);
        Self {
            repo,
            store,
            nodes,
            alerts,
            history,
            tx,
        }
    }

    /// The underlying repo (definitions/schedules/runs CRUD for the API handlers).
    #[must_use]
    pub fn repo(&self) -> Arc<ReportsRepo> {
        self.repo.clone()
    }

    /// Subscribe to the live run-status stream (SSE).
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<String> {
        self.tx.subscribe()
    }

    fn broadcast_run(&self, run: &ReportRun) {
        if let Ok(json) = serde_json::to_string(run) {
            let _ = self.tx.send(json);
        }
    }

    /// Generate a report from a definition: insert a run row, spawn the background task, and return
    /// the freshly-inserted row. `Ok(None)` ⇒ no such definition.
    pub async fn run_now(
        self: &Arc<Self>,
        definition_id: Uuid,
        trigger: &str,
        created_by: Option<String>,
    ) -> anyhow::Result<Option<ReportRun>> {
        let Some(def) = self.repo.get_definition(definition_id).await? else {
            return Ok(None);
        };
        let spec: ReportSpec = serde_json::from_value(def.spec.clone()).unwrap_or_default();
        let range = spec
            .params
            .range_secs
            .unwrap_or(DEFAULT_RANGE_SECS)
            .clamp(300, 365 * 86_400);
        let to_s = now_s();
        let from_s = to_s - range;
        let section_count = i32::try_from(spec.sections.len()).unwrap_or(i32::MAX);

        let run = self
            .repo
            .insert_run(
                Some(def.id),
                &def.name,
                trigger,
                from_s,
                to_s,
                section_count,
                &def.spec,
                created_by.as_deref(),
            )
            .await?;
        self.broadcast_run(&run);

        let runner = self.clone();
        let id = run.id;
        let name = def.name.clone();
        tokio::spawn(async move {
            runner.generate(id, name, spec, from_s, to_s).await;
        });
        Ok(Some(run))
    }

    /// Persist a progress tick and broadcast the updated row.
    async fn progress(&self, id: Uuid, pct: i32) {
        if let Err(e) = self.repo.set_run_progress(id, pct).await {
            tracing::warn!(error = %e, run = %id, "report progress update failed");
        }
        if let Ok(Some(run)) = self.repo.get_run(id).await {
            self.broadcast_run(&run);
        }
    }

    /// The whole job: render each section, assemble the document, persist, finalize.
    async fn generate(
        self: Arc<Self>,
        id: Uuid,
        name: String,
        spec: ReportSpec,
        from_s: i64,
        to_s: i64,
    ) {
        let total = spec.sections.len().max(1);
        let mut sections_data: Vec<serde_json::Value> = Vec::new();
        let mut body = String::new();

        for (i, sec) in spec.sections.iter().enumerate() {
            self.progress(id, (i * 90 / total) as i32).await;
            let section = self.render_section(sec, from_s, to_s).await;
            sections_data.push(section.to_data());
            body.push_str(&section.to_html());
        }
        self.progress(id, 95).await;

        let window_label = format!("{} → {}", fmt_day(from_s), fmt_day(to_s));
        let generated_label = format!("Generated {}", fmt_minute(now_s()));
        let html = render_document(&name, &window_label, &generated_label, &body);
        let result = serde_json::json!({
            "title": name,
            "generated_ms": now_ms(),
            "range_from_ms": from_s * 1000,
            "range_to_ms": to_s * 1000,
            "sections": sections_data,
        });

        match self.repo.finish_run(id, &result, &html).await {
            Ok(()) => {}
            Err(e) => {
                tracing::error!(error = %e, run = %id, "failed to persist report result");
                let _ = self.repo.fail_run(id, "failed to persist report").await;
            }
        }
        if let Ok(Some(run)) = self.repo.get_run(id).await {
            self.broadcast_run(&run);
        }
    }

    /// Render one section to its structured + HTML output. Unknown kinds render a placeholder.
    async fn render_section(&self, sec: &SectionSpec, from_s: i64, to_s: i64) -> Section {
        let id = sec.id.clone().unwrap_or_else(|| sec.kind.clone());
        match sec.kind.as_str() {
            "availability-summary" => self.render_availability(id, from_s, to_s).await,
            "alert-summary" => self.render_alert_summary(id, from_s, to_s).await,
            "top-alerting-nodes" => self.render_top_alerting(id, &sec.settings, from_s).await,
            "top-cpu" | "top-rtt" | "top-memory" => {
                self.render_top_metric(id, &sec.kind, &sec.settings).await
            }
            "throughput-trend" => self.render_throughput(id, from_s, to_s).await,
            "inventory-listing" => self.render_inventory(id, &sec.settings).await,
            other => Section {
                id,
                kind: other.to_owned(),
                title: format!("Unknown section: {other}"),
                note: Some("This section type is not supported by this server version.".to_owned()),
                ..Default::default()
            },
        }
    }

    async fn render_availability(&self, id: String, from_s: i64, to_s: i64) -> Section {
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

    async fn render_alert_summary(&self, id: String, from_s: i64, _to_s: i64) -> Section {
        let from_ms = from_s * 1000;
        // Fires in the window, by severity (resolved=false records since `from`).
        let recent = self.history.recent(1000, None).await.unwrap_or_default();
        let mut fires: HashMap<String, i64> = HashMap::new();
        for r in &recent {
            if !r.resolved && r.at_unix_ms >= from_ms {
                *fires.entry(r.severity.clone()).or_insert(0) += 1;
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

    async fn render_top_alerting(
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
        let names = self.nodes.node_names(&ids).await.unwrap_or_default();
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

    async fn render_top_metric(
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
        let names = self.nodes.node_names(&ids).await.unwrap_or_default();
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

    async fn render_throughput(&self, id: String, from_s: i64, to_s: i64) -> Section {
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

    async fn render_inventory(&self, id: String, settings: &serde_json::Value) -> Section {
        let limit = setting_i64(settings, "limit", 200).clamp(1, 5000) as usize;
        let mut nodes = self.nodes.list_nodes().await.unwrap_or_default();
        nodes.sort_by(|a, b| a.name.cmp(&b.name));
        let states = self.alerts.node_states();
        let total = nodes.len();
        let rows: Vec<Vec<String>> = nodes
            .iter()
            .take(limit)
            .map(|n| {
                let state = states
                    .get(&n.id)
                    .map_or_else(|| "unknown".to_owned(), |s| s.as_str().to_owned());
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

/// Format a Unix-seconds instant as a UTC date (YYYY-MM-DD).
fn fmt_day(s: i64) -> String {
    Utc.timestamp_opt(s, 0)
        .single()
        .map_or_else(|| s.to_string(), |t| t.format("%Y-%m-%d").to_string())
}

/// Format a Unix-seconds instant as a UTC date+time (YYYY-MM-DD HH:MM UTC).
fn fmt_minute(s: i64) -> String {
    Utc.timestamp_opt(s, 0).single().map_or_else(
        || s.to_string(),
        |t| t.format("%Y-%m-%d %H:%M UTC").to_string(),
    )
}

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
        assert!(csv.contains("\"My, Report\"")); // comma forces quoting
        assert!(csv.contains("Node,CPU"));
        assert!(csv.contains("\"r\"\"2\",10")); // embedded quote doubled
        assert!(csv.contains("Peak,88%"));
    }

    #[test]
    fn next_run_daily_rolls_to_tomorrow_when_past() {
        // now = 2026-06-20 10:00 UTC; daily at 09:00 → next is 2026-06-21 09:00.
        let now = Utc.with_ymd_and_hms(2026, 6, 20, 10, 0, 0).unwrap();
        let next = compute_next_run("daily", None, None, 9, 0, now);
        assert_eq!(next, Utc.with_ymd_and_hms(2026, 6, 21, 9, 0, 0).unwrap());
    }

    #[test]
    fn next_run_daily_today_when_future() {
        let now = Utc.with_ymd_and_hms(2026, 6, 20, 6, 0, 0).unwrap();
        let next = compute_next_run("daily", None, None, 9, 30, now);
        assert_eq!(next, Utc.with_ymd_and_hms(2026, 6, 20, 9, 30, 0).unwrap());
    }

    #[test]
    fn next_run_weekly_finds_target_weekday() {
        // 2026-06-20 is a Saturday (dow=6). Target Monday (dow=1) at 08:00 → 2026-06-22 08:00.
        let now = Utc.with_ymd_and_hms(2026, 6, 20, 12, 0, 0).unwrap();
        let next = compute_next_run("weekly", Some(1), None, 8, 0, now);
        assert_eq!(next, Utc.with_ymd_and_hms(2026, 6, 22, 8, 0, 0).unwrap());
    }

    #[test]
    fn next_run_weekly_same_day_future_vs_past() {
        // Saturday now 06:00; weekly Saturday(6) at 09:00 → today; at 03:00 → next week.
        let now = Utc.with_ymd_and_hms(2026, 6, 20, 6, 0, 0).unwrap();
        assert_eq!(
            compute_next_run("weekly", Some(6), None, 9, 0, now),
            Utc.with_ymd_and_hms(2026, 6, 20, 9, 0, 0).unwrap()
        );
        assert_eq!(
            compute_next_run("weekly", Some(6), None, 3, 0, now),
            Utc.with_ymd_and_hms(2026, 6, 27, 3, 0, 0).unwrap()
        );
    }

    #[test]
    fn next_run_monthly_rolls_to_next_month() {
        // now 2026-06-20; monthly on the 1st → 2026-07-01.
        let now = Utc.with_ymd_and_hms(2026, 6, 20, 12, 0, 0).unwrap();
        let next = compute_next_run("monthly", None, Some(1), 0, 0, now);
        assert_eq!(next, Utc.with_ymd_and_hms(2026, 7, 1, 0, 0, 0).unwrap());
        // Year rollover from December.
        let dec = Utc.with_ymd_and_hms(2026, 12, 15, 12, 0, 0).unwrap();
        let jan = compute_next_run("monthly", None, Some(5), 6, 0, dec);
        assert_eq!(jan, Utc.with_ymd_and_hms(2027, 1, 5, 6, 0, 0).unwrap());
    }

    #[test]
    fn catalog_kinds_are_known() {
        assert!(is_known_section("availability-summary"));
        assert!(is_known_section("top-cpu"));
        assert!(!is_known_section("totally-made-up"));
        // Every catalog kind round-trips through the renderer's match (no unknown placeholder).
        assert_eq!(section_catalog().len(), 8);
    }
}
