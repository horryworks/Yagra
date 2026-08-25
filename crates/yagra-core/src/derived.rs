// SPDX-License-Identifier: AGPL-3.0-only
//! Node-level metrics Yagra **computes** rather than collects (ADR-105).
//!
//! Monitoring is discussed in percentages, but devices mostly report two raw numbers. The Huawei
//! USG in the lab returns `huawei_mem_total = 3_702_417_408` and `huawei_mem_free = 720_375_808`;
//! **80.5% appears in no series at all**, so an operator who wants "page me at 90% memory" has no
//! name to write a rule against. The 2026-08-24 audit classified 53 metrics nobody could write a
//! default rule for, and **16 of them were held up by nothing but the missing division**.
//!
//! The precedent is [`crate::interface_util`]: `if_in_util_pct` is not stored either — it is a
//! VictoriaMetrics `rate()` divided by PostgreSQL's `if_speed`, computed at evaluation time
//! (ADR-012/ADR-076). That machinery is pinned to the port dimension. This module is the same idea
//! at the node dimension, and like its sibling it is split into a **pure half** (this file's table
//! and arithmetic, testable with no store and no database) and an impure evaluator
//! ([`run_derived_metric_watch`]) that does the I/O and the ordering.
//!
//! ## Why a closed table and not an expression language
//!
//! An operator never needs to *write* `a / (a + b) * 100`; they need a name to hang a bound on.
//! A parser would add validation, a UI, error wording and an N-1 story, and buy none of that. Five
//! shapes cover ten names here, so the eleventh costs one row in [`DERIVED_NODE_METRICS`].
//!
//! ## Why rows, not node aggregates
//!
//! 🚨 The single place in this module where a mistake is silent. `hr_storage_used` and
//! `hr_storage_size` are one hrStorageTable walk, and one Linux box has `/`, `/boot`, physical
//! memory and swap in it. `max(used) / max(size)` divides a small `/boot`'s usage by a large `/`'s
//! capacity and reports a disk-full alert nobody can find. Everything here joins on the **row key**
//! first and divides second; a scalar is simply the one-row case, keyed `0`, so there is one path.
//!
//! Which row is *worst* is not decided here — [`yagra_common::EffectiveThreshold::is_worse`] does
//! it, exactly as it does for `huawei_temp`'s fifteen sensors on the poll path (ADR-081). Reading
//! it as "the highest value" would be wrong the moment a rule's fault direction is `below`.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use uuid::Uuid;

use crate::alerts::sink::AlertSink;
use crate::alerts::AlertManager;
use crate::store::MetricStore;
use yagra_common::{MetricKind, NodeId};

/// How often the evaluator ticks.
///
/// The same cadence as the interface evaluator, and the same consequence: a rule's
/// `dwell_samples` counts **ticks**, not polls, for every metric in this table. The alert-rules
/// screen says so, because "3 consecutive breaches" means three minutes here and three polls
/// everywhere else.
pub const WATCH_TICK: Duration = Duration::from_secs(60);

/// How far back a series may have last been seen and still count as current.
///
/// 🚨 **Measured, and both earlier answers were wrong in different ways.** It shipped as 600s,
/// which is shorter than a normal SNMP collection interval, so the one device in the lab that
/// could exercise this feature evaluated **zero rows** and looked exactly like "no such device
/// here". Borrowing [`crate::store::INSTANT_LOOKBACK_SECS`] (1800s) was closer and still wrong:
/// the live Huawei's `huawei_mem_total` arrives every **1,419–2,975 seconds** (measured over 95k
/// samples, 2026-08-25), because the poller is sharing its budget with twenty-two unreachable
/// nodes.
///
/// It is deliberately **longer** than `INSTANT_LOOKBACK_SECS`, which is a different question with
/// a different cost. That one asks "what should a screen show as the current value", where a stale
/// number misleads a reader. This one asks "is there a reading to evaluate", where too-short means
/// the whole feature silently does nothing and too-long is bounded by something else entirely:
/// [`crate::alerts::AlertManager::observe_derived_metric`] refuses to observe a node whose liveness
/// is not `Ok`, so a device that went away stops being evaluated regardless of this window. **The
/// guard against a stale reading is the liveness freeze, not the lookback.**
///
/// What an hour does accept: a metric that stops being collected while its node stays up — a
/// template detached from a profile — keeps alerting on its last value for up to an hour.
pub const LOOKBACK_SECS: u64 = 3600;

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
    },
    DerivedMetric {
        name: METRIC_CISCO_CPU_MEM_USED_PCT,
        formula: Formula::PercentOfSum {
            used: "cisco_cpu_mem_used",
            free: "cisco_cpu_mem_free",
        },
    },
    DerivedMetric {
        name: METRIC_CISCO_MEM_USED_PCT,
        formula: Formula::PercentOfSum {
            used: "cisco_mem_used",
            free: "cisco_mem_free",
        },
    },
    DerivedMetric {
        name: METRIC_HR_STORAGE_USED_PCT,
        formula: Formula::PercentOf {
            part: "hr_storage_used",
            whole: "hr_storage_size",
        },
    },
    DerivedMetric {
        name: METRIC_HUAWEI_MEM_USED_PCT,
        formula: Formula::PercentUsedOfTotal {
            total: "huawei_mem_total",
            free: "huawei_mem_free",
        },
    },
    DerivedMetric {
        name: METRIC_POE_POWER_USED_PCT,
        formula: Formula::PercentOf {
            part: "poe_power_consumed_w",
            whole: "poe_power_capacity_w",
        },
    },
    DerivedMetric {
        name: METRIC_UCD_CPU_USED_PCT,
        formula: Formula::Complement {
            idle: "ucd_cpu_idle_pct",
        },
    },
    DerivedMetric {
        name: METRIC_UCD_LOAD_PER_CORE,
        formula: Formula::PerSeriesCount {
            value: "ucd_load_1min",
            per: "hr_processor_load",
        },
    },
    DerivedMetric {
        name: METRIC_UCD_MEM_USED_PCT,
        formula: Formula::PercentUsedOfTotal {
            total: "ucd_mem_total_kb",
            free: "ucd_mem_avail_kb",
        },
    },
    DerivedMetric {
        name: METRIC_UCD_SWAP_USED_PCT,
        formula: Formula::PercentUsedOfTotal {
            total: "ucd_swap_total_kb",
            free: "ucd_swap_avail_kb",
        },
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

/// One node's computed reading for one derived metric, on one row.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DerivedReading {
    pub node: NodeId,
    /// The row key the inputs shared. `0` for a scalar.
    ///
    /// `i64` because a table walk's row key is the last OID sub-identifier, whatever its size —
    /// the live Huawei USG answers with `3237192130`, past `i32::MAX`.
    pub row: i64,
    pub value: f64,
}

/// Join two input tables on the row key and apply the formula.
///
/// 🚨 The join is the point (module doc). A row present on one side only is **skipped**, not
/// defaulted: a filesystem whose size arrived and whose usage did not is unknown, and treating the
/// missing half as zero would report either 0% or a division by zero as if it were a measurement.
///
/// [`Formula::PerSeriesCount`] is the one shape that does not join — its denominator is how many
/// rows the second input has on that node, so it needs the whole node's row set rather than a
/// matching key.
#[must_use]
pub fn evaluate(
    formula: Formula,
    first: &BTreeMap<(Uuid, i64), f64>,
    second: &BTreeMap<(Uuid, i64), f64>,
) -> Vec<DerivedReading> {
    let mut out = Vec::new();
    match formula {
        Formula::Complement { .. } => {
            for ((node, row), a) in first {
                if let Some(value) = formula.apply(*a, 0.0) {
                    out.push(DerivedReading {
                        node: NodeId::from(*node),
                        row: *row,
                        value,
                    });
                }
            }
        }
        Formula::PerSeriesCount { .. } => {
            let mut per_node: BTreeMap<Uuid, usize> = BTreeMap::new();
            for (node, _) in second.keys() {
                *per_node.entry(*node).or_insert(0) += 1;
            }
            for ((node, row), a) in first {
                let Some(count) = per_node.get(node) else {
                    // The node reports a load average but no processor rows. Not an error — it is
                    // what a host answering UCD-SNMP-MIB but not HOST-RESOURCES-MIB looks like —
                    // and skipping is the only honest answer: dividing by one would publish the
                    // raw load average under a name that promises per-core.
                    continue;
                };
                #[allow(clippy::cast_precision_loss)]
                let denominator = *count as f64;
                if let Some(value) = formula.apply(*a, denominator) {
                    out.push(DerivedReading {
                        node: NodeId::from(*node),
                        row: *row,
                        value,
                    });
                }
            }
        }
        Formula::PercentOf { .. }
        | Formula::PercentOfSum { .. }
        | Formula::PercentUsedOfTotal { .. } => {
            for ((node, row), a) in first {
                let Some(b) = second.get(&(*node, *row)) else {
                    continue;
                };
                if let Some(value) = formula.apply(*a, *b) {
                    out.push(DerivedReading {
                        node: NodeId::from(*node),
                        row: *row,
                        value,
                    });
                }
            }
        }
    }
    out
}

/// Compute every derived node metric a rule can currently fire on, and feed the alert engine.
///
/// Each tick is: ask the rules which derived metrics anyone is watching, query VictoriaMetrics for
/// their inputs (one round-trip per input metric, fleet-wide), join on the row key, divide, and
/// observe. The pure half of that is above; this function is the I/O and the ordering.
///
/// Cost is **the number of derived metrics with a live rule**, not the number of nodes — which is
/// why this loop carries none of the query-floor machinery its interface sibling needs (a port
/// evaluator's cost is ports times nodes; ADR-105 decision 4).
///
/// Leader-only for the same reason pool coverage and the interface evaluator are: the engine's
/// check state is process-local, so two evaluators would double every notification.
pub(crate) async fn run_derived_metric_watch(
    store: Arc<dyn MetricStore>,
    alerts: Arc<AlertManager>,
    sink: Arc<dyn AlertSink>,
) {
    loop {
        tokio::time::sleep(WATCH_TICK).await;
        let now_ms = chrono::Utc::now().timestamp_millis();
        let mut evaluated = 0_usize;

        // Close the alerts whose rule was deleted. The poll path's `!alerting` branch does this for
        // every check it visits, but it never visits a derived metric — nothing polls
        // `huawei_mem_used_pct` — so before this, deleting a rule left its alert open for the life
        // of the process. Found on the deployment: the verification rule was removed and its
        // warning was still open three minutes later.
        //
        // It runs even when no metric is evaluated below, because a deleted rule is exactly the
        // case where nothing can fire.
        let mut actions = alerts.resolve_orphaned_node_derived_alerts();

        for derived in DERIVED_NODE_METRICS {
            let coverage = alerts.rule_coverage(derived.name);
            if coverage.lowest_bound.is_none() {
                // No rule that can fire, so nothing is queried. Its alert, if it had one, was
                // closed by the sweep above — this branch must not resolve anything itself, or a
                // metric with a rule and no data would look the same as one with no rule.
                //
                // The gauge is zeroed rather than left at its last reading: a stale "1" beside a
                // metric nobody is watching reads as "one row is being evaluated".
                metrics::gauge!("yagra_derived_metric_rows", "metric" => derived.name).set(0.0);
                continue;
            }
            if coverage.nodes.as_ref().is_some_and(BTreeSet::is_empty) {
                continue;
            }
            // `None` means a rule is scoped too broadly to enumerate (a group, a profile, the
            // fleet), so ask about every node; a non-empty set narrows the selector.
            let scoped: Vec<Uuid> = coverage
                .nodes
                .as_ref()
                .map(|s| s.iter().copied().collect())
                .unwrap_or_default();
            let scope: Option<&[Uuid]> = coverage.nodes.is_some().then_some(scoped.as_slice());

            let [first_name, second_name] = derived.formula.inputs();
            let first: BTreeMap<(Uuid, i64), f64> = store
                .series_rows(first_name, scope, LOOKBACK_SECS)
                .await
                .into_iter()
                .collect();
            if first.is_empty() {
                // Nothing collected the numerator on any covered node. Distinguishable from "the
                // store did not answer" only by the store's own warning, which is where that
                // belongs — this loop must not resolve open alerts on an empty answer.
                metrics::gauge!("yagra_derived_metric_rows", "metric" => derived.name).set(0.0);
                continue;
            }
            // `Complement` repeats its single input; querying it twice would cost a round-trip per
            // tick for no second answer.
            let second: BTreeMap<(Uuid, i64), f64> = if second_name == first_name {
                first.clone()
            } else {
                store
                    .series_rows(second_name, scope, LOOKBACK_SECS)
                    .await
                    .into_iter()
                    .collect()
            };

            let readings = evaluate(derived.formula, &first, &second);
            evaluated += readings.len();
            #[allow(clippy::cast_precision_loss)]
            let row_count = readings.len() as f64;
            metrics::gauge!("yagra_derived_metric_rows", "metric" => derived.name).set(row_count);

            // Rows are grouped per node and observed together: they share one node-level check, so
            // the engine folds them to the worst under the rule's own direction (ADR-081). Feeding
            // them one at a time would push N observations into one dwell window — the ADR-076 bug,
            // on the rows ADR-076 did not split.
            let mut by_node: BTreeMap<NodeId, Vec<f64>> = BTreeMap::new();
            for r in readings {
                by_node.entry(r.node).or_default().push(r.value);
            }
            for (node, values) in by_node {
                if let Some(a) = alerts.observe_derived_metric(node, derived.name, &values, now_ms)
                {
                    actions.extend(a);
                }
            }
        }

        #[allow(clippy::cast_precision_loss)]
        let total = evaluated as f64;
        metrics::gauge!("yagra_derived_metric_readings").set(total);
        for action in actions {
            sink.dispatch(action).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(entries: &[(u128, i64, f64)]) -> BTreeMap<(Uuid, i64), f64> {
        entries
            .iter()
            .map(|(n, r, v)| ((Uuid::from_u128(*n), *r), *v))
            .collect()
    }

    /// The live Huawei USG's own numbers, and its own answer to the same question.
    ///
    /// Accept side first: the fleet's one reachable SNMP device reports
    /// `huawei_mem_total`/`huawei_mem_free`, **and separately** `huawei_mem_usage = 80.0`. Two
    /// independent readings agreeing is the only evidence this feature has that is not a unit test
    /// of its own arithmetic.
    #[test]
    fn the_huawei_ratio_matches_what_the_device_reports_about_itself() {
        let f = Formula::PercentUsedOfTotal {
            total: "huawei_mem_total",
            free: "huawei_mem_free",
        };
        let pct = f.apply(3_702_417_408.0, 720_375_808.0).expect("finite");
        assert!(
            (pct - 80.0).abs() < 1.0,
            "derived {pct:.2}% must agree with the device's own huawei_mem_usage of 80.0"
        );
    }

    /// A row present on one side only is skipped rather than defaulted.
    ///
    /// Accept side first, or "nothing came out" and "the join is inverted" look identical.
    #[test]
    fn a_ratio_joins_on_the_row_key_and_skips_a_half_measured_row() {
        let f = Formula::PercentOf {
            part: "hr_storage_used",
            whole: "hr_storage_size",
        };
        // Row 1 is a nearly full `/`; row 31's usage never arrived and row 7's size has no usage.
        let used = rows(&[(1, 1, 90.0), (1, 31, 1.0)]);
        let size = rows(&[(1, 1, 100.0), (1, 7, 4096.0)]);
        let out = evaluate(f, &used, &size);
        assert_eq!(out.len(), 1, "only the row measured on both sides");
        assert_eq!(out[0].row, 1);
        assert!((out[0].value - 90.0).abs() < 1e-9);
    }

    /// 🚨 The failure this module exists to prevent: the aggregate answer is a different number.
    #[test]
    fn folding_to_node_maxima_before_dividing_would_report_a_different_disk() {
        let f = Formula::PercentOf {
            part: "hr_storage_used",
            whole: "hr_storage_size",
        };
        // `/` is 4 GB and 1% used; `/boot` is 100 MB and 90% used.
        let used = rows(&[(1, 1, 40.0), (1, 2, 90.0)]);
        let size = rows(&[(1, 1, 4000.0), (1, 2, 100.0)]);
        let per_row = evaluate(f, &used, &size);
        let mut values: Vec<f64> = per_row.iter().map(|r| r.value).collect();
        values.sort_by(f64::total_cmp);
        assert_eq!(values.len(), 2);
        assert!((values[0] - 1.0).abs() < 1e-9);
        assert!((values[1] - 90.0).abs() < 1e-9);
        // What an aggregate read would have produced: max(used) over max(size).
        let aggregate = f.apply(90.0, 4000.0).expect("finite");
        assert!(
            aggregate < 3.0,
            "max/max reads 2.25%, so the 90%-full /boot never fires — this is the bug"
        );
    }

    /// Division by zero must not become a fleet-wide critical.
    #[test]
    fn every_shape_refuses_a_zero_or_negative_denominator() {
        let percent_of = Formula::PercentOf {
            part: "a",
            whole: "b",
        };
        assert_eq!(percent_of.apply(5.0, 0.0), None);
        assert_eq!(percent_of.apply(5.0, -1.0), None);
        let of_sum = Formula::PercentOfSum {
            used: "a",
            free: "b",
        };
        assert_eq!(of_sum.apply(0.0, 0.0), None);
        let of_total = Formula::PercentUsedOfTotal {
            total: "a",
            free: "b",
        };
        assert_eq!(of_total.apply(0.0, 0.0), None);
        let per_core = Formula::PerSeriesCount {
            value: "a",
            per: "b",
        };
        assert_eq!(per_core.apply(4.0, 0.0), None);
        // Accept side: the same shapes answer when the denominator is usable.
        assert_eq!(percent_of.apply(5.0, 10.0), Some(50.0));
        assert_eq!(of_sum.apply(3.0, 1.0), Some(75.0));
        assert_eq!(of_total.apply(10.0, 4.0), Some(60.0));
        assert_eq!(per_core.apply(4.0, 2.0), Some(2.0));
    }

    /// A row key past `i32::MAX` is a real one, and it must still join.
    ///
    /// The live Huawei USG keys its memory rows on `3237192130`. Both halves carry it, so the
    /// pairing works — but only if nothing along the way narrows the key.
    #[test]
    fn a_row_key_from_a_real_entity_index_still_joins() {
        let f = Formula::PercentUsedOfTotal {
            total: "huawei_mem_total",
            free: "huawei_mem_free",
        };
        let total = rows(&[(1, 3_237_192_130, 3_702_417_408.0)]);
        let free = rows(&[(1, 3_237_192_130, 727_363_584.0)]);
        let out = evaluate(f, &total, &free);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].row, 3_237_192_130);
        assert!(
            (out[0].value - 80.4).abs() < 0.5,
            "got {} — the device's own huawei_mem_usage reads 80",
            out[0].value
        );
    }

    /// Load per core divides by how many processor rows the node has, not by a matching key.
    #[test]
    fn load_per_core_counts_the_processor_rows_and_skips_a_host_with_none() {
        let f = Formula::PerSeriesCount {
            value: "ucd_load_1min",
            per: "hr_processor_load",
        };
        let load = rows(&[(1, 0, 8.0), (2, 0, 8.0)]);
        // Node 1 has four logical processors; node 2 reports none.
        let cpus = rows(&[(1, 1, 12.0), (1, 2, 9.0), (1, 3, 30.0), (1, 4, 5.0)]);
        let out = evaluate(f, &load, &cpus);
        assert_eq!(out.len(), 1, "a host with no processor rows is skipped");
        assert_eq!(out[0].node, NodeId::from(Uuid::from_u128(1)));
        assert!((out[0].value - 2.0).abs() < 1e-9, "8.0 over four cores");
    }

    /// The one-input shape reads its input once and needs no join.
    #[test]
    fn the_complement_shape_inverts_an_idle_reading() {
        let f = Formula::Complement {
            idle: "ucd_cpu_idle_pct",
        };
        let idle = rows(&[(1, 0, 93.0)]);
        let out = evaluate(f, &idle, &BTreeMap::new());
        assert_eq!(out.len(), 1);
        assert!((out[0].value - 7.0).abs() < 1e-9);
        assert_eq!(f.inputs()[0], f.inputs()[1], "one input, repeated");
    }

    /// Every input a formula names is a metric a built-in template actually collects.
    ///
    /// Without this, a typo in the table is invisible: the query returns nothing, the join is
    /// empty, and the derived metric simply never fires — indistinguishable from a fleet that has
    /// no such device. Derived from the catalogue rather than a second list, so a template that is
    /// renamed fails here rather than going quiet.
    #[test]
    fn every_input_is_a_metric_something_collects() {
        let collected: BTreeSet<String> = crate::mib::builtin_mib_rows()
            .into_iter()
            .map(|(item, _)| item.metric_name.to_string())
            .collect();
        assert!(
            collected.len() > 50,
            "only {} catalogue rows — the walk drifted and this check is vacuous",
            collected.len()
        );
        let mut checked = 0_usize;
        for d in DERIVED_NODE_METRICS {
            for input in d.formula.inputs() {
                assert!(
                    collected.contains(input),
                    "{}'s input `{input}` is collected by no built-in metric set",
                    d.name
                );
                checked += 1;
            }
        }
        assert!(checked >= 20, "only {checked} inputs checked");
    }

    /// The WebUI's derived-metric list holds exactly the metrics Rust computes, in both directions.
    ///
    /// `web/src/lib/metricMeaning.ts`'s `DERIVED_METRICS` is what groups the picker's "Derived"
    /// section, and it is **hand-written** — the generated catalogue only knows metrics that are
    /// *collected*, so it can never answer for these. That makes it a mirror with nothing guarding
    /// it (`extensibility.md` §2): a name missing there is a metric an operator cannot select even
    /// though the engine would evaluate it, and a name left there after a Rust rename is an option
    /// that saves a rule nothing will ever fire.
    ///
    /// Both directions, and a floor: a regex that stopped matching would otherwise compare two
    /// empty sets and pass.
    #[test]
    fn the_webui_derived_list_holds_exactly_what_rust_computes() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../web/src/lib/metricMeaning.ts");
        let src = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
        let block = src
            .split_once("export const DERIVED_METRICS = [")
            .expect("DERIVED_METRICS is declared")
            .1
            .split_once("] as const;")
            .expect("the array is closed")
            .0;
        let listed: BTreeSet<&str> = block
            .lines()
            .filter_map(|l| {
                // `'name',` — the quote is spelled by code point so this line survives the shell
                // and script quoting every edit to this file has to pass through.
                let quote = char::from(39_u8);
                let t = l.trim();
                t.strip_prefix(quote)
                    .and_then(|r| r.split_once(quote))
                    .map(|(name, _)| name)
            })
            .collect();
        assert!(
            listed.len() >= 14,
            "only {} names parsed out of DERIVED_METRICS — the reader drifted",
            listed.len()
        );
        let mut expected: BTreeSet<&str> = DERIVED_NODE_METRICS.iter().map(|d| d.name).collect();
        expected.extend(crate::interface_util::DERIVED_INTERFACE_METRICS);
        let missing: Vec<&&str> = expected.difference(&listed).collect();
        let orphaned: Vec<&&str> = listed.difference(&expected).collect();
        assert!(
            missing.is_empty() && orphaned.is_empty(),
            "web/src/lib/metricMeaning.ts DERIVED_METRICS is out of step.
               missing (Rust computes it, the picker will not offer it): {missing:?}
               orphaned (the picker offers it, nothing computes it): {orphaned:?}"
        );
    }

    /// The orphan sweep runs inside the watch loop, and before the per-metric loop.
    ///
    /// The same structural check its interface sibling carries, for the same reason: a deleted
    /// rule is exactly the case where no metric is evaluated, so a sweep placed inside the loop
    /// over [`DERIVED_NODE_METRICS`] would be skipped in the one situation it exists for.
    #[test]
    fn the_orphan_sweep_runs_inside_the_watch_loop_and_before_the_metrics() {
        let production = crate::module_source::code("src", "derived");
        let watch = production
            .split("async fn run_derived_metric_watch")
            .nth(1)
            .expect("the watch loop exists");
        let body = &watch[..watch.find("\nfn ").unwrap_or(watch.len())];
        // A floor: everything below asks whether something is *present*, which over an empty
        // slice is a claim about nothing (ADR-089).
        assert!(
            body.contains("sink.dispatch("),
            "the slice is not the watch loop's body — it does not even drain its actions"
        );
        let sweep = body
            .find("resolve_orphaned_node_derived_alerts()")
            .expect("without the sweep, deleting a rule strands its alert for the process's life");
        let loop_start = body
            .find("for derived in DERIVED_NODE_METRICS")
            .expect("the per-metric loop exists");
        assert!(
            sweep < loop_start,
            "the sweep must run before the per-metric loop, which skips every metric whose rule \
             was just deleted"
        );
    }

    /// Sorted and unique, for the same reason `METRIC_MEANINGS` is: enumeration order reaches a
    /// generated locale file, and a duplicate name would make the second row unreachable.
    #[test]
    fn the_table_is_sorted_and_holds_each_name_once() {
        let names: Vec<&str> = DERIVED_NODE_METRICS.iter().map(|d| d.name).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(names, sorted, "DERIVED_NODE_METRICS must stay sorted");
        assert_eq!(names.iter().collect::<BTreeSet<_>>().len(), names.len());
        assert_eq!(
            derived_node_metric_kind("huawei_mem_used_pct"),
            Some(MetricKind::Gauge)
        );
        assert_eq!(derived_node_metric_kind("huawei_mem_total"), None);
    }
}
