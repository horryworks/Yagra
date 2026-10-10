// SPDX-License-Identifier: AGPL-3.0-only
//! Metrics Yagra **computes** rather than collects: their names and how each is computed
//! (ADR-105 at the node dimension, ADR-076 at the port dimension).
//!
//! Only the vocabulary lives here. The evaluators that query the TSDB and feed the alert engine
//! stay in `yagra-core` (`derived.rs` and `interface_util.rs`), and so does the reasoning about rows
//! and lookback in their module docs. The names moved down so the alert engine, which branches on
//! them, can be built without `yagra-core` (ADR-202 Inc.4): one table, read by both.

use crate::metric::MetricKind;

// ---- The node dimension (ADR-105) ----

/// How a derived metric is computed from collected ones.
///
/// Exhaustive on purpose — no `_ =>` arm (`extensibility.md` §1). A sixth shape must be handled
/// everywhere the compiler names, rather than falling through to whatever the last author assumed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Formula {
    /// `100 x part / whole`. Both sides are collected on the same row.
    PercentOf {
        part: &'static str,
        whole: &'static str,
    },
    /// `100 x used / (used + free)` — vendors that report the two halves and no total.
    PercentOfSum {
        used: &'static str,
        free: &'static str,
    },
    /// `100 x (total - free) / total` — vendors that report a total and what is left of it.
    PercentUsedOfTotal {
        total: &'static str,
        free: &'static str,
    },
    /// `100 - idle`. The one shape with a single input.
    Complement { idle: &'static str },
    /// `value` divided by **how many rows of `per`** this node has.
    ///
    /// The denominator is a count of series, not a value: a load average is only interpretable
    /// against the number of logical processors, and `hr_processor_load` publishes one row each.
    PerSeriesCount {
        value: &'static str,
        per: &'static str,
    },
}

impl Formula {
    /// The collected metrics this formula reads, in the order the evaluator queries them.
    #[must_use]
    pub fn inputs(&self) -> [&'static str; 2] {
        match self {
            Formula::PercentOf { part, whole } => [part, whole],
            Formula::PercentOfSum { used, free } => [used, free],
            Formula::PercentUsedOfTotal { total, free } => [total, free],
            // One input, repeated: the caller deduplicates, and a fixed-width array keeps every
            // shape the same size so no caller has to branch on which one it got.
            Formula::Complement { idle } => [idle, idle],
            Formula::PerSeriesCount { value, per } => [value, per],
        }
    }

    /// Compute one row's value from its two inputs, or `None` when the row cannot be evaluated.
    ///
    /// 🚨 Every division guards its denominator. `100 * 5 / 0` is `inf` in IEEE arithmetic and
    /// `inf > 90` is *true*, so an unguarded divide turns one device reporting a zero total into a
    /// fleet-wide critical. Non-finite results are refused for the same reason.
    #[must_use]
    pub fn apply(&self, a: f64, b: f64) -> Option<f64> {
        let out = match self {
            Formula::PercentOf { .. } => {
                if b <= 0.0 {
                    return None;
                }
                100.0 * a / b
            }
            Formula::PercentOfSum { .. } => {
                let total = a + b;
                if total <= 0.0 {
                    return None;
                }
                100.0 * a / total
            }
            Formula::PercentUsedOfTotal { .. } => {
                if a <= 0.0 {
                    return None;
                }
                100.0 * (a - b) / a
            }
            Formula::Complement { .. } => 100.0 - a,
            Formula::PerSeriesCount { .. } => {
                if b <= 0.0 {
                    return None;
                }
                a / b
            }
        };
        out.is_finite().then_some(out)
    }
}

/// One metric Yagra computes at the node dimension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DerivedMetric {
    /// The name an operator writes a threshold rule against.
    pub name: &'static str,
    /// How it is computed.
    pub formula: Formula,
    /// Whether each table row is a reading of its own (ADR-143) — a memory pool, a filesystem, a
    /// PSE group — rather than the node's one value computed from scalars.
    ///
    /// 🚨 **Declared, not inferred from the row keys.** A scalar's series carries no row label and
    /// reads back as row `0` (`MetricStore::series_rows` in yagra-core), which is also a real row
    /// key on some agents, so the keys cannot tell a Net-SNMP host's memory from a one-row table. A
    /// per-row metric alerts per row (`metric@row`); a scalar one keeps the node-wide check it always
    /// had, so no existing check id moves. `every_per_row_flag_agrees_with_how_its_input_is_collected`
    /// pins this to the collection catalogue.
    pub per_row: bool,
}

/// Percentage of a Cisco Enhanced Memory Pool in use.
pub const METRIC_CISCO_CEMP_MEM_USED_PCT: &str = "cisco_cemp_mem_used_pct";
/// Percentage of a Cisco per-CPU memory pool in use.
pub const METRIC_CISCO_CPU_MEM_USED_PCT: &str = "cisco_cpu_mem_used_pct";
/// Percentage of a Cisco memory pool in use, from used and free halves.
pub const METRIC_CISCO_MEM_USED_PCT: &str = "cisco_mem_used_pct";
/// Percentage of a Host Resources storage row in use (a filesystem, RAM or swap).
pub const METRIC_HR_STORAGE_USED_PCT: &str = "hr_storage_used_pct";
/// Percentage of physical memory in use, from a total and a free reading (Huawei VRP).
pub const METRIC_HUAWEI_MEM_USED_PCT: &str = "huawei_mem_used_pct";
/// Percentage of a PSE's power budget drawn by attached devices.
pub const METRIC_POE_POWER_USED_PCT: &str = "poe_power_used_pct";
/// Percentage of CPU in use on a Net-SNMP host, from the idle reading.
pub const METRIC_UCD_CPU_USED_PCT: &str = "ucd_cpu_used_pct";
/// One-minute load average per logical processor.
pub const METRIC_UCD_LOAD_PER_CORE: &str = "ucd_load_per_core";
/// Percentage of physical memory in use on a Net-SNMP host.
pub const METRIC_UCD_MEM_USED_PCT: &str = "ucd_mem_used_pct";
/// Percentage of swap in use on a Net-SNMP host.
pub const METRIC_UCD_SWAP_USED_PCT: &str = "ucd_swap_used_pct";

/// Every derived node metric, and how each is computed.
///
/// Sorted by name so the generated locale file and every enumeration read the same order. Adding a
/// row is the whole cost of an eleventh derived metric on the Rust side; the WebUI's picker list
/// and the sentence in `metric_meaning.rs` are the two places a compiler cannot reach, and both
/// have a test that fails until they are written.
pub const DERIVED_NODE_METRICS: [DerivedMetric; 10] = [
    DerivedMetric {
        name: METRIC_CISCO_CEMP_MEM_USED_PCT,
        formula: Formula::PercentOfSum {
            used: "cisco_cemp_mem_used",
            free: "cisco_cemp_mem_free",
        },
        per_row: true,
    },
    DerivedMetric {
        name: METRIC_CISCO_CPU_MEM_USED_PCT,
        formula: Formula::PercentOfSum {
            used: "cisco_cpu_mem_used",
            free: "cisco_cpu_mem_free",
        },
        per_row: true,
    },
    DerivedMetric {
        name: METRIC_CISCO_MEM_USED_PCT,
        formula: Formula::PercentOfSum {
            used: "cisco_mem_used",
            free: "cisco_mem_free",
        },
        per_row: true,
    },
    DerivedMetric {
        name: METRIC_HR_STORAGE_USED_PCT,
        formula: Formula::PercentOf {
            part: "hr_storage_used",
            whole: "hr_storage_size",
        },
        per_row: true,
    },
    DerivedMetric {
        name: METRIC_HUAWEI_MEM_USED_PCT,
        formula: Formula::PercentUsedOfTotal {
            total: "huawei_mem_total",
            free: "huawei_mem_free",
        },
        per_row: true,
    },
    DerivedMetric {
        name: METRIC_POE_POWER_USED_PCT,
        formula: Formula::PercentOf {
            part: "poe_power_consumed_w",
            whole: "poe_power_capacity_w",
        },
        per_row: true,
    },
    DerivedMetric {
        name: METRIC_UCD_CPU_USED_PCT,
        formula: Formula::Complement {
            idle: "ucd_cpu_idle_pct",
        },
        per_row: false,
    },
    DerivedMetric {
        name: METRIC_UCD_LOAD_PER_CORE,
        formula: Formula::PerSeriesCount {
            value: "ucd_load_1min",
            per: "hr_processor_load",
        },
        per_row: false,
    },
    DerivedMetric {
        name: METRIC_UCD_MEM_USED_PCT,
        formula: Formula::PercentUsedOfTotal {
            total: "ucd_mem_total_kb",
            free: "ucd_mem_avail_kb",
        },
        per_row: false,
    },
    DerivedMetric {
        name: METRIC_UCD_SWAP_USED_PCT,
        formula: Formula::PercentUsedOfTotal {
            total: "ucd_swap_total_kb",
            free: "ucd_swap_avail_kb",
        },
        per_row: false,
    },
];

/// The table row for `metric`, if it is one Yagra computes at the node dimension.
///
/// Looked up in [`DERIVED_NODE_METRICS`] rather than a hand-written `match`, so an eleventh metric
/// is reachable here the moment its row exists.
#[must_use]
pub fn derived_node_metric(metric: &str) -> Option<&'static DerivedMetric> {
    DERIVED_NODE_METRICS.iter().find(|d| d.name == metric)
}

/// The kind every derived node metric is, for the API's threshold validation.
///
/// A gauge, always: a percentage and a per-core load are levels, not odometers, so `above` and
/// `below` both mean what they say and a fixed bound is meaningful. Nothing here is monotonic, so
/// the counter rejection (ADR-012) must not catch them.
#[must_use]
pub fn derived_node_metric_kind(metric: &str) -> Option<MetricKind> {
    derived_node_metric(metric).map(|_| MetricKind::Gauge)
}

// ---- The port dimension (ADR-076) ----

/// Derived metric: receive utilisation as a percentage of the port's own speed.
pub const METRIC_IF_IN_UTIL_PCT: &str = "if_in_util_pct";
/// Derived metric: transmit utilisation as a percentage of the port's own speed.
pub const METRIC_IF_OUT_UTIL_PCT: &str = "if_out_util_pct";

/// Derived metric: receive traffic in bits per second.
pub const METRIC_IF_IN_BPS: &str = "if_in_bps";
/// Derived metric: transmit traffic in bits per second.
pub const METRIC_IF_OUT_BPS: &str = "if_out_bps";

/// The two derived metrics for one direction: the percentage and the absolute rate.
///
/// They are computed from **the same** VictoriaMetrics answer — the percentage is that answer
/// divided by the port's speed — so the evaluator queries per direction and observes both, rather
/// than querying once per metric. A pair rather than a naming convention, so a future direction
/// has to say which rate it is, instead of inheriting one by string coincidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DerivedPair {
    /// Percentage of the port's own speed. Needs a denominator.
    pub pct: &'static str,
    /// Bits per second. Needs no denominator, so it covers the ports that report no speed.
    pub bps: &'static str,
}

/// Receive and transmit, in the order the evaluator ticks them.
///
/// Receive and transmit are **separate metrics rather than one "utilisation"**, because a link is
/// asymmetric far more often than not: an uplink saturated inbound and idle outbound is one
/// problem, not half of one, and collapsing them to `max` would leave an operator unable to tell
/// which direction is congested without opening the chart.
pub const DERIVED_PAIRS: [DerivedPair; 2] = [
    DerivedPair {
        pct: METRIC_IF_IN_UTIL_PCT,
        bps: METRIC_IF_IN_BPS,
    },
    DerivedPair {
        pct: METRIC_IF_OUT_UTIL_PCT,
        bps: METRIC_IF_OUT_BPS,
    },
];

/// Every derived interface metric, flat — what the API's threshold validation and the WebUI's
/// metric picker enumerate.
pub const DERIVED_INTERFACE_METRICS: [&str; 4] = [
    METRIC_IF_IN_UTIL_PCT,
    METRIC_IF_OUT_UTIL_PCT,
    METRIC_IF_IN_BPS,
    METRIC_IF_OUT_BPS,
];

/// The kind every derived interface metric is, for the API's threshold validation.
///
/// A gauge: a percentage is a level, not an odometer, so `above`/`below` both mean what they say
/// and the counter rejection (`reject_counter_metric`) must not catch it. `None` for a name this
/// module does not define — the caller then falls back to the collection catalogue.
#[must_use]
pub fn derived_interface_metric_kind(metric: &str) -> Option<MetricKind> {
    DERIVED_INTERFACE_METRICS
        .contains(&metric)
        .then_some(MetricKind::Gauge)
}

/// The interned name for a derived interface metric, or `None` for anything else.
///
/// Maps a name that arrived as a `String` — off an `Alert`, out of the database — back to the
/// `&'static str` that the interface evaluator's check key and the threshold lookup are keyed by. Derived from
/// [`DERIVED_INTERFACE_METRICS`] rather than a hand-written `match`, so a fifth derived metric is
/// covered by adding it to that one list.
#[must_use]
pub fn derived_interface_metric_name(metric: &str) -> Option<&'static str> {
    DERIVED_INTERFACE_METRICS.into_iter().find(|m| *m == metric)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn every_derived_metric_is_a_gauge_and_is_not_collected() {
        for m in DERIVED_INTERFACE_METRICS {
            assert!(
                crate::is_valid_metric_name(m),
                "{m} must be spellable as a series name"
            );
            assert_eq!(derived_interface_metric_kind(m), Some(MetricKind::Gauge));
            // They are computed, never walked — a name that also existed in the catalogue would
            // mean two different things wrote the same series.
            assert_eq!(
                crate::builtin_metric_kind(m),
                None,
                "{m} must not be a collected metric"
            );
        }
        assert_eq!(derived_interface_metric_kind("if_hc_in_octets"), None);
        // The names say which half they count, because they are not each other's complement.
        assert_ne!(METRIC_IF_IN_UTIL_PCT, METRIC_IF_OUT_UTIL_PCT);
        assert_ne!(METRIC_IF_IN_BPS, METRIC_IF_OUT_BPS);

        // The pairs cover the flat list exactly, in both directions. A metric in one and not the
        // other is a metric the evaluator either never ticks or never validates.
        let paired: BTreeSet<&str> = DERIVED_PAIRS.iter().flat_map(|d| [d.pct, d.bps]).collect();
        assert_eq!(
            paired,
            DERIVED_INTERFACE_METRICS
                .into_iter()
                .collect::<BTreeSet<_>>()
        );
        // The pair's two halves are different metrics — a pair whose `pct` and `bps` collapsed to
        // one name would make the evaluator observe the same check twice with different units.
        for d in DERIVED_PAIRS {
            assert_ne!(d.pct, d.bps);
        }
    }

    /// The interning that lets a runtime `String` off an `Alert` become the interface evaluator's
    /// check key.
    #[test]
    fn only_the_four_derived_names_intern() {
        for m in DERIVED_INTERFACE_METRICS {
            // The `&'static str` is what matters: core's `CheckKey` is keyed by it, so a `String` here
            // would not compile at the call site.
            let interned: &'static str =
                derived_interface_metric_name(m).expect("a derived metric interns");
            assert_eq!(interned, m);
        }
        // A collected per-interface metric belongs to the poll path, not to the interface sweep.
        assert_eq!(derived_interface_metric_name("if_oper_status"), None);
        assert_eq!(derived_interface_metric_name("icmp_rtt_ms"), None);
        assert_eq!(derived_interface_metric_name(""), None);
        // Not a prefix match: a longer name starting with a derived one is a different metric.
        assert_eq!(derived_interface_metric_name("if_in_util_pct_avg"), None);
    }
}
