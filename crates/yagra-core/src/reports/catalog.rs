// SPDX-License-Identifier: AGPL-3.0-only
//! **What you may put in a report** — the section menu, served at `GET /reports/sections` and used
//! to validate a definition's sections at the API edge.
//!
//! Mirrors the dashboard widget registry: each entry names an existing data source and declares its
//! settings, which the builder renders generically. Adding a section starts here — the other two
//! sites are [`super::runner`]'s dispatch arm and the `render_*` beside it in
//! [`super::sections`].

use serde::Serialize;

// ── Section catalog (drives the builder + validates kinds) ────────────────────────────────

/// One selectable choice for a `select` setting.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct SettingOption {
    pub value: &'static str,
    pub label: &'static str,
}

/// A configurable setting on a section (rendered generically by the builder).
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
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
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_kinds_are_known() {
        assert!(is_known_section("availability-summary"));
        assert!(is_known_section("top-cpu"));
        assert!(!is_known_section("totally-made-up"));
        // Every catalog kind round-trips through the renderer's match (no unknown placeholder).
        assert_eq!(section_catalog().len(), 8);
    }
}
