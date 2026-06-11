//! Collection sets: *what* Yagra collects from a node, and how those choices resolve.
//!
//! A **collection set** is the list of metrics polled from a node. Like thresholds
//! (ADR-013), items are defined at a scope — a device-class [`ScopeLevel::Profile`] or
//! directly on a node ([`ScopeLevel::Node`]) — and resolve by **inheritance with
//! override**: a node-level item replaces the profile-level item with the same
//! `metric_name`, and node-only items are added on top. This keeps "what to collect"
//! configurable per device class with per-node overrides.
//!
//! Two shapes of item exist (see [`CollectionKind`]): a **scalar** OID fetched with GET
//! (e.g. sysUpTime), and a **table** column base walked with GETBULK to yield one value
//! per interface row. Either way the `metric_name` is a *stable, bounded* identifier — it
//! becomes the TSDB metric label, so it must never be a free-text/device-supplied value,
//! or series cardinality explodes (monitoring-conventions, ADR-011).
//!
//! Resolution lives *only here* so precedence logic is never scattered (mirrors
//! [`crate::thresholds::resolve_effective`]).

use crate::metric::MetricKind;
use crate::thresholds::ScopeLevel;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// How a [`CollectionItem`] is collected from the agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CollectionKind {
    /// A single scalar instance OID fetched with SNMP GET (e.g. sysUpTime).
    Scalar,
    /// A table *column* base OID walked with GETBULK — one value per row/interface,
    /// keyed by the trailing sub-identifier (the ifIndex).
    Table,
}

/// One thing to collect: a stable metric name, the OID to collect it from, how to collect
/// it (scalar GET vs table walk), and whether it is a gauge or a raw counter.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CollectionItem {
    /// Stable TSDB metric name (e.g. `if_hc_in_octets`). Bounded by convention to keep
    /// label cardinality controlled — never a free-text/device-supplied value (ADR-011).
    pub metric_name: String,
    /// Dotted OID: a scalar instance OID ([`CollectionKind::Scalar`]) or a table column
    /// base ([`CollectionKind::Table`], the walk root).
    pub oid: String,
    /// Scalar GET vs table walk.
    pub kind: CollectionKind,
    /// Gauge vs raw counter — rates/utilization are derived at query time (ADR-012),
    /// never by the poller.
    pub metric_kind: MetricKind,
}

/// A [`CollectionItem`] tagged with the scope it was defined at, for resolution.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScopedCollectionItem {
    /// Scope this item was defined at.
    pub level: ScopeLevel,
    /// The item itself.
    pub item: CollectionItem,
}

impl ScopedCollectionItem {
    /// Convenience constructor.
    #[must_use]
    pub fn new(level: ScopeLevel, item: CollectionItem) -> Self {
        Self { level, item }
    }
}

/// An interface-metadata field discovered alongside the numeric table columns.
///
/// These are *not* time-series — they are descriptive attributes that live in PostgreSQL
/// and are joined to interface metrics at query time (thin-label model, ADR-011). They are
/// walked from the agent but never become TSDB labels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InterfaceField {
    /// `ifName` — short interface name (e.g. `Gi0/1`).
    Name,
    /// `ifAlias` — operator-assigned description.
    Alias,
    /// `ifSpeed`/`ifHighSpeed`-derived line rate, in bits per second.
    Speed,
}

/// Resolve a set of scoped items into the **effective** collection set for a node.
///
/// Precedence is per `metric_name`: the item at the most-specific [`ScopeLevel`] present
/// wins (Node > Group > Profile), so a node-level entry overrides the profile default and
/// node-only entries are added. Output is sorted by `metric_name` for determinism. Items
/// flagged disabled are filtered out *before* calling this (callers pass only enabled rows).
#[must_use]
pub fn resolve_collection_set(items: &[ScopedCollectionItem]) -> Vec<CollectionItem> {
    // metric_name -> (winning level so far, item). A strictly more-specific level replaces.
    let mut winners: BTreeMap<&str, (ScopeLevel, &CollectionItem)> = BTreeMap::new();
    for sci in items {
        let key = sci.item.metric_name.as_str();
        match winners.get(key) {
            Some((level, _)) if *level >= sci.level => {}
            _ => {
                winners.insert(key, (sci.level, &sci.item));
            }
        }
    }
    winners
        .into_values()
        .map(|(_, item)| item.clone())
        .collect()
}

// --- Built-in standard catalog ------------------------------------------------------
//
// Applied to every SNMP-enabled node that has no explicit collection set, so behaviour is
// useful out of the box (and the legacy "poll sysUpTime" default is preserved). Every
// metric_name here is a fixed identifier — the bounded set is what keeps TSDB cardinality
// controlled.

/// sysUpTime.0 — system uptime in hundredths of a second (scalar).
pub const OID_SYS_UPTIME: &str = "1.3.6.1.2.1.1.3.0";

/// The standard scalar + interface-table metrics collected by default.
///
/// Scalar: sysUpTime. Table (per-interface, ifXTable/ifTable columns): 64-bit octet
/// counters, error counters, oper status, and high-speed. Counters are stored **raw**;
/// utilization is derived from `rate()` at query time (ADR-012).
#[must_use]
pub fn builtin_catalog() -> Vec<CollectionItem> {
    let scalar = |metric: &str, oid: &str, mk: MetricKind| CollectionItem {
        metric_name: metric.to_owned(),
        oid: oid.to_owned(),
        kind: CollectionKind::Scalar,
        metric_kind: mk,
    };
    let table = |metric: &str, oid: &str, mk: MetricKind| CollectionItem {
        metric_name: metric.to_owned(),
        oid: oid.to_owned(),
        kind: CollectionKind::Table,
        metric_kind: mk,
    };
    vec![
        scalar("snmp_sys_uptime_ticks", OID_SYS_UPTIME, MetricKind::Gauge),
        // ifXTable high-capacity octet counters (64-bit) — preferred over ifInOctets.
        table(
            "if_hc_in_octets",
            "1.3.6.1.2.1.31.1.1.1.6",
            MetricKind::Counter,
        ),
        table(
            "if_hc_out_octets",
            "1.3.6.1.2.1.31.1.1.1.10",
            MetricKind::Counter,
        ),
        // ifTable error counters.
        table("if_in_errors", "1.3.6.1.2.1.2.2.1.14", MetricKind::Counter),
        table("if_out_errors", "1.3.6.1.2.1.2.2.1.20", MetricKind::Counter),
        // ifOperStatus (1=up) and ifHighSpeed (Mbps) as gauges.
        table("if_oper_status", "1.3.6.1.2.1.2.2.1.8", MetricKind::Gauge),
        table(
            "if_high_speed",
            "1.3.6.1.2.1.31.1.1.1.15",
            MetricKind::Gauge,
        ),
    ]
}

/// The standard interface-metadata columns (ifName, ifAlias, ifSpeed) and their OID bases.
///
/// Walked alongside the numeric columns to populate the PostgreSQL `interfaces` table;
/// these never become TSDB series. `ifSpeed` is in bits/sec (the 32-bit gauge — adequate
/// for the metadata speed; ifHighSpeed in the numeric catalog covers >4 Gbps links).
#[must_use]
pub fn builtin_interface_meta_columns() -> Vec<(InterfaceField, &'static str)> {
    vec![
        (InterfaceField::Name, "1.3.6.1.2.1.31.1.1.1.1"),
        (InterfaceField::Alias, "1.3.6.1.2.1.31.1.1.1.18"),
        (InterfaceField::Speed, "1.3.6.1.2.1.2.2.1.5"),
    ]
}

// --- Built-in collection templates + device profiles --------------------------------
//
// Collection templates are reusable, named metric bundles (the design's middle layer:
// MIB → Collection templates → Device profile references). A device profile is just a set
// of attached templates (it holds no raw OIDs). Core seeds these templates and links the
// built-in profiles to them, so binding a profile (+ an SNMP credential) yields graphs out
// of the box. The generic interface set works on virtually any SNMP agent; the vendor
// columns are *best-effort* common OIDs (walked as table columns so we don't guess an
// instance index) — an OID a device lacks is simply skipped by the poller.

/// A named, reusable collection template and the metrics it bundles.
#[derive(Debug, Clone, PartialEq)]
pub struct BuiltinTemplate {
    /// Display name (unique).
    pub name: &'static str,
    /// One-line description.
    pub description: &'static str,
    /// The metrics this template collects.
    pub items: Vec<CollectionItem>,
}

/// A built-in device profile: a name and the template names it references (no raw OIDs).
#[derive(Debug, Clone, PartialEq)]
pub struct BuiltinProfile {
    /// Display name shown in Device profiles.
    pub name: &'static str,
    /// Names of the [`builtin_templates`] this profile attaches (empty ⇒ ICMP-only).
    pub templates: Vec<&'static str>,
}

/// A vendor table column (CPU/mem usage etc.), walked per-entity like the interface table.
fn vendor_table(metric: &str, oid: &str) -> CollectionItem {
    CollectionItem {
        metric_name: metric.to_owned(),
        oid: oid.to_owned(),
        kind: CollectionKind::Table,
        metric_kind: MetricKind::Gauge,
    }
}

/// Standard SNMP template name — the cross-vendor system + interface set most profiles use.
pub const TEMPLATE_STANDARD_SNMP: &str = "Standard SNMP";

/// The built-in collection templates shipped with Yagra (seeded on startup).
#[must_use]
pub fn builtin_templates() -> Vec<BuiltinTemplate> {
    vec![
        BuiltinTemplate {
            name: TEMPLATE_STANDARD_SNMP,
            description:
                "System uptime + per-interface traffic, errors, and status (any SNMP agent).",
            items: builtin_catalog(),
        },
        BuiltinTemplate {
            name: "Cisco device health",
            description: "Cisco IOS CPU (cpmCPUTotal5min) and memory-pool used/free.",
            items: vec![
                vendor_table("cisco_cpu_5min", "1.3.6.1.4.1.9.9.109.1.1.1.1.8"),
                vendor_table("cisco_mem_used", "1.3.6.1.4.1.9.9.48.1.1.1.5"),
                vendor_table("cisco_mem_free", "1.3.6.1.4.1.9.9.48.1.1.1.6"),
            ],
        },
        BuiltinTemplate {
            name: "Huawei device health",
            description: "Huawei VRP entity CPU and memory usage (hwEntityCpu/MemUsage).",
            items: vec![
                vendor_table("huawei_cpu_usage", "1.3.6.1.4.1.2011.5.25.31.1.1.1.1.5"),
                vendor_table("huawei_mem_usage", "1.3.6.1.4.1.2011.5.25.31.1.1.1.1.7"),
            ],
        },
    ]
}

/// The built-in device profiles and the templates each references.
#[must_use]
pub fn builtin_profiles() -> Vec<BuiltinProfile> {
    vec![
        // ICMP-only: no templates (for devices without a usable SNMP agent).
        BuiltinProfile {
            name: "Generic ping",
            templates: Vec::new(),
        },
        BuiltinProfile {
            name: "Generic SNMP",
            templates: vec![TEMPLATE_STANDARD_SNMP],
        },
        BuiltinProfile {
            name: "Cisco IOS router",
            templates: vec![TEMPLATE_STANDARD_SNMP, "Cisco device health"],
        },
        BuiltinProfile {
            name: "Huawei USG firewall",
            templates: vec![TEMPLATE_STANDARD_SNMP, "Huawei device health"],
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(metric: &str, oid: &str) -> CollectionItem {
        CollectionItem {
            metric_name: metric.to_owned(),
            oid: oid.to_owned(),
            kind: CollectionKind::Scalar,
            metric_kind: MetricKind::Gauge,
        }
    }

    #[test]
    fn node_overrides_profile_for_same_metric() {
        // Profile and Node both define cpu_util; the node-level OID must win.
        let items = [
            ScopedCollectionItem::new(ScopeLevel::Profile, item("cpu_util", "1.1.1")),
            ScopedCollectionItem::new(ScopeLevel::Node, item("cpu_util", "2.2.2")),
        ];
        let resolved = resolve_collection_set(&items);
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].oid, "2.2.2");
    }

    #[test]
    fn node_only_items_are_added_on_top_of_profile() {
        let items = [
            ScopedCollectionItem::new(ScopeLevel::Profile, item("a", "1")),
            ScopedCollectionItem::new(ScopeLevel::Node, item("b", "2")),
        ];
        let resolved = resolve_collection_set(&items);
        let names: Vec<&str> = resolved.iter().map(|i| i.metric_name.as_str()).collect();
        assert_eq!(names, vec!["a", "b"]); // sorted by metric_name, both present
    }

    #[test]
    fn profile_default_used_when_no_node_override() {
        let items = [ScopedCollectionItem::new(
            ScopeLevel::Profile,
            item("cpu_util", "1.1.1"),
        )];
        let resolved = resolve_collection_set(&items);
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].oid, "1.1.1");
    }

    #[test]
    fn empty_resolves_to_empty() {
        assert!(resolve_collection_set(&[]).is_empty());
    }

    #[test]
    fn builtin_catalog_has_scalar_and_table_items_with_bounded_names() {
        let cat = builtin_catalog();
        assert!(cat
            .iter()
            .any(|i| i.metric_name == "snmp_sys_uptime_ticks" && i.kind == CollectionKind::Scalar));
        let octets = cat
            .iter()
            .find(|i| i.metric_name == "if_hc_in_octets")
            .expect("hc in octets present");
        assert_eq!(octets.kind, CollectionKind::Table);
        assert_eq!(octets.metric_kind, MetricKind::Counter);
    }

    #[test]
    fn collection_kind_serde_is_snake_case() {
        assert_eq!(
            serde_json::to_string(&CollectionKind::Table).unwrap(),
            "\"table\""
        );
        assert_eq!(
            serde_json::from_str::<CollectionKind>("\"scalar\"").unwrap(),
            CollectionKind::Scalar
        );
    }

    #[test]
    fn builtin_templates_carry_expected_metrics() {
        let templates = builtin_templates();
        let by_name = |n: &str| templates.iter().find(|t| t.name == n).unwrap();

        // Standard SNMP = the system + interface set.
        let std = &by_name(TEMPLATE_STANDARD_SNMP).items;
        assert!(std.iter().any(|i| i.metric_name == "snmp_sys_uptime_ticks"));
        assert!(std.iter().any(|i| i.metric_name == "if_hc_in_octets"
            && i.kind == CollectionKind::Table
            && i.metric_kind == MetricKind::Counter));

        // Vendor templates carry only their extras (not the generic set).
        let cisco = &by_name("Cisco device health").items;
        assert!(cisco.iter().any(|i| i.metric_name == "cisco_cpu_5min"));
        assert!(!cisco.iter().any(|i| i.metric_name == "if_hc_in_octets"));
        assert!(by_name("Huawei device health")
            .items
            .iter()
            .any(|i| i.metric_name == "huawei_cpu_usage"));
    }

    #[test]
    fn builtin_profiles_reference_existing_templates() {
        let profiles = builtin_profiles();
        let names: Vec<&str> = profiles.iter().map(|p| p.name).collect();
        assert_eq!(
            names,
            vec![
                "Generic ping",
                "Generic SNMP",
                "Cisco IOS router",
                "Huawei USG firewall"
            ]
        );
        let by_name = |n: &str| profiles.iter().find(|p| p.name == n).unwrap();

        assert!(by_name("Generic ping").templates.is_empty());
        assert_eq!(
            by_name("Generic SNMP").templates,
            vec![TEMPLATE_STANDARD_SNMP]
        );
        assert_eq!(
            by_name("Cisco IOS router").templates,
            vec![TEMPLATE_STANDARD_SNMP, "Cisco device health"]
        );

        // Every referenced template actually exists.
        let template_names: std::collections::BTreeSet<&str> =
            builtin_templates().iter().map(|t| t.name).collect();
        for p in &profiles {
            for t in &p.templates {
                assert!(
                    template_names.contains(t),
                    "{} references unknown template {t}",
                    p.name
                );
            }
        }
    }

    #[test]
    fn builtin_template_oids_are_dotted_digits() {
        // Mirror the API's is_valid_oid so seeded items can never be rejected on re-create.
        let dotted_digits = |oid: &str| {
            !oid.is_empty()
                && oid
                    .split('.')
                    .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
        };
        for t in builtin_templates() {
            for item in t.items {
                assert!(
                    dotted_digits(&item.oid),
                    "bad OID {} in {}",
                    item.oid,
                    t.name
                );
            }
        }
    }
}
