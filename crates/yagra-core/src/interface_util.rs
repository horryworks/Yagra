// SPDX-License-Identifier: AGPL-3.0-only
//! Per-interface bandwidth utilisation, and the bookkeeping the evaluator needs around it
//! (ADR-076 decisions 2 and 3).
//!
//! Utilisation is **not a stored series**. ADR-012 fixed the shape: pollers store raw counters,
//! rates come from VictoriaMetrics' `rate()` at evaluation time, and the percentage is that rate
//! divided by `interfaces.if_speed` from PostgreSQL. This module is the pure half of that — the
//! arithmetic, the query floor, and the set of checks currently being tracked — so all of it is
//! testable without a TSDB or a database.
//!
//! The impure half is `main.rs::run_interface_utilization_watch`, modelled on
//! `run_pool_coverage_watch`: it is the only other loop that raises alerts from outside the poll
//! path.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use uuid::Uuid;
use yagra_common::{IfIndex, MetricKind, NodeId};

/// Derived metric: receive utilisation as a percentage of the port's own speed.
pub const METRIC_IF_IN_UTIL_PCT: &str = "if_in_util_pct";
/// Derived metric: transmit utilisation as a percentage of the port's own speed.
pub const METRIC_IF_OUT_UTIL_PCT: &str = "if_out_util_pct";

/// Both derived metrics, in the order the evaluator ticks them.
///
/// Receive and transmit are **separate metrics rather than one "utilisation"**, because a link is
/// asymmetric far more often than not: an uplink saturated inbound and idle outbound is one
/// problem, not half of one, and collapsing them to `max` would leave an operator unable to tell
/// which direction is congested without opening the chart.
pub const DERIVED_INTERFACE_METRICS: [&str; 2] = [METRIC_IF_IN_UTIL_PCT, METRIC_IF_OUT_UTIL_PCT];

/// The kind every derived interface metric is, for the API's threshold validation.
///
/// A gauge: a percentage is a level, not an odometer, so `above`/`below` both mean what they say
/// and the counter rejection (`reject_counter_metric`) must not catch it. `None` for a name this
/// module does not define — the caller then falls back to the collection catalogue.
#[must_use]
pub fn derived_metric_kind(metric: &str) -> Option<MetricKind> {
    DERIVED_INTERFACE_METRICS
        .contains(&metric)
        .then_some(MetricKind::Gauge)
}

/// How often the evaluator ticks.
///
/// One tick is **one sample** for a rule's breach count, which is the thing an operator has to be
/// told: a rule saying "5 breaches" damps for five minutes here, not five polls. 60s is chosen
/// against the 300s `rate()` lookback — ticking faster would re-read the same VictoriaMetrics
/// window and inflate the dwell without adding information.
pub const WATCH_TICK: Duration = Duration::from_secs(60);

/// Utilisation as a percentage of a port's own speed.
///
/// `None` when the port has no usable speed, which is a real and common state rather than an
/// error: `interfaces.if_speed` is NULL on an agent that implements neither `ifSpeed` nor
/// `ifHighSpeed`, and it is NULL rather than `4294967295` on a port whose 32-bit gauge saturated
/// (ADR-063 decision 7 — the sentinel means "faster than I can say", and dividing by it reported a
/// 10G port as 4.29G, which migration 0086 had to clean up in production).
///
/// `0` is treated the same as absent: a port that reports zero speed is an administratively down
/// or unnegotiated one, and dividing by it yields infinity, which would breach every bound.
#[must_use]
pub fn utilisation_pct(bits_per_sec: f64, if_speed_bps: Option<i64>) -> Option<f64> {
    let speed = if_speed_bps.filter(|s| *s > 0)?;
    if !bits_per_sec.is_finite() || bits_per_sec < 0.0 {
        return None;
    }
    Some(bits_per_sec / speed as f64 * 100.0)
}

/// The absolute bits/sec floor below which no covered port can breach any rule.
///
/// `slowest_covered_bps` is the smallest usable `if_speed` among the ports a rule covers, and
/// `lowest_bound_pct` the smallest warning-or-critical percentage any of those rules names. A port
/// carrying less than their product cannot be at or above any rule's percentage of its *own*
/// speed, because its own speed is at least the slowest one.
///
/// ⚠️ **This is a correctness device, not a scalability one.** It admits no false negatives, and
/// that is all it promises: the floor is set by the slowest covered link, so a single 64 kbps
/// circuit with a 70% rule puts it at ~45 kbps and essentially every series in the fleet passes.
/// What actually keeps the query small on a normal deployment is that the caller names the ports
/// when the rules are node- or interface-scoped, and only falls back to the fleet when a rule is
/// scoped broadly enough to mean it.
///
/// `None` when nothing is covered (no rules, or no covered port has a usable speed), which the
/// caller reads as "run no query at all".
#[must_use]
pub fn candidate_floor_bps(
    slowest_covered_bps: Option<i64>,
    lowest_bound_pct: Option<f64>,
) -> Option<f64> {
    let speed = slowest_covered_bps.filter(|s| *s > 0)? as f64;
    let pct = lowest_bound_pct.filter(|p| p.is_finite() && *p > 0.0)?;
    // Clamped to the speed itself: a bound above 100% is expressible (a port can carry more than
    // its declared rate on some counters) and must not push the floor past what any port emits.
    Some((speed * pct / 100.0).min(speed))
}

/// One tracked per-port check: which node, which port, which of the two derived metrics.
pub type CheckKey = (NodeId, IfIndex, &'static str);

/// The checks the evaluator has fed at least once, so it knows what to observe as recovered when a
/// port drops out of the candidate set.
///
/// Bounded by whatever has ever crossed the floor, not by the fleet size: a port that has never
/// been busy has never been tracked, and is never observed.
#[derive(Debug, Default)]
pub struct TrackedChecks {
    seen: BTreeSet<CheckKey>,
}

impl TrackedChecks {
    /// Note that a check was fed this tick.
    pub fn mark(&mut self, key: CheckKey) {
        self.seen.insert(key);
    }

    /// The tracked checks for `metric` that are **not** in `present` — the ports that were busy
    /// and no longer are, which the caller observes as recovered.
    #[must_use]
    pub fn absent<'a>(
        &'a self,
        metric: &'static str,
        present: &'a BTreeSet<(NodeId, IfIndex)>,
    ) -> Vec<CheckKey> {
        self.seen
            .iter()
            .filter(|(_, _, m)| *m == metric)
            .filter(|(node, idx, _)| !present.contains(&(*node, *idx)))
            .copied()
            .collect()
    }

    /// How many checks are tracked (for the self-observability gauge).
    ///
    /// The set only grows when a port crosses the query floor, so this is also the honest answer
    /// to "how much is this loop actually watching" — a fleet whose ports are all idle tracks
    /// nothing and costs nothing.
    #[must_use]
    pub fn len(&self) -> usize {
        self.seen.len()
    }
}

/// One port's evaluated utilisation, ready to be fed to the alert engine.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Utilisation {
    pub node: NodeId,
    pub ifindex: IfIndex,
    pub pct: f64,
}

/// Turn a candidate set and a speed table into utilisations, dropping the ports whose speed is
/// unknown.
///
/// Returns the usable readings plus a count of the ports that had to be skipped. The count is not
/// diagnostic noise: "no alert fired" and "we could not evaluate this port at all" look identical
/// from the outside, and without a number an operator has no way to learn that a third of the
/// fleet is uncovered.
#[must_use]
pub fn evaluate(
    candidates: &[(Uuid, i32, f64)],
    speeds: &BTreeMap<(Uuid, i32), Option<i64>>,
) -> (Vec<Utilisation>, usize) {
    let mut out = Vec::with_capacity(candidates.len());
    let mut skipped = 0_usize;
    for (node, ifindex, bps) in candidates {
        let speed = speeds.get(&(*node, *ifindex)).copied().flatten();
        let Some(pct) = utilisation_pct(*bps, speed) else {
            skipped += 1;
            continue;
        };
        let Ok(idx) = u32::try_from(*ifindex) else {
            // A negative ifindex cannot come from the TSDB label parser, but it is the one value
            // `i32` can hold that `IfIndex` cannot — skipping beats wrapping into another port.
            skipped += 1;
            continue;
        };
        out.push(Utilisation {
            node: NodeId::from(*node),
            ifindex: IfIndex(idx),
            pct,
        });
    }
    (out, skipped)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_two_derived_metrics_are_gauges_and_are_not_collected() {
        for m in DERIVED_INTERFACE_METRICS {
            assert!(
                yagra_common::is_valid_metric_name(m),
                "{m} must be spellable as a series name"
            );
            assert_eq!(derived_metric_kind(m), Some(MetricKind::Gauge));
            // They are computed, never walked — a name that also existed in the catalogue would
            // mean two different things wrote the same series.
            assert_eq!(
                yagra_common::builtin_metric_kind(m),
                None,
                "{m} must not be a collected metric"
            );
        }
        assert_eq!(derived_metric_kind("if_hc_in_octets"), None);
        // The names say which half they count, because they are not each other's complement.
        assert_ne!(METRIC_IF_IN_UTIL_PCT, METRIC_IF_OUT_UTIL_PCT);
    }

    #[test]
    fn utilisation_is_bits_over_speed_as_a_percentage() {
        // The lab's real reading, and the arithmetic that catches the two mistakes that pass every
        // type check: a missing ×8 (bytes read as bits) and the wrong denominator. 2.9 Mbps on a
        // 100 Mbps port is 2.9%, not 0.36% and not 29%.
        let pct = utilisation_pct(2_896_192.0, Some(100_000_000)).expect("speed known");
        assert!((pct - 2.896).abs() < 0.001, "got {pct}");

        assert_eq!(utilisation_pct(90_000_000.0, Some(100_000_000)), Some(90.0));
        // A port can briefly exceed its declared rate; that is a reading, not an error.
        let over = utilisation_pct(110_000_000.0, Some(100_000_000)).expect("speed known");
        assert!((over - 110.0).abs() < 1e-9, "got {over}");
    }

    #[test]
    fn a_port_with_no_usable_speed_is_not_evaluated() {
        // All three of these are live states, not hypotheticals: NULL on an agent implementing
        // neither speed OID, 0 on an unnegotiated port, and the saturated sentinel that ADR-063
        // decision 7 stopped storing (migration 0086 cleaned up the rows that already had it).
        assert_eq!(utilisation_pct(1_000.0, None), None);
        assert_eq!(utilisation_pct(1_000.0, Some(0)), None);
        assert_eq!(utilisation_pct(1_000.0, Some(-1)), None);
        // A non-finite rate cannot produce a percentage either.
        assert_eq!(utilisation_pct(f64::NAN, Some(1_000)), None);
        assert_eq!(utilisation_pct(f64::INFINITY, Some(1_000)), None);
        assert_eq!(utilisation_pct(-1.0, Some(1_000)), None);
    }

    /// The floor's whole job is to admit no false negatives — assert that, not a magic number.
    #[test]
    fn the_floor_admits_every_value_that_could_breach_any_rule() {
        for slowest in [64_000_i64, 10_000_000, 1_000_000_000] {
            for bound in [1.0_f64, 50.0, 70.0, 90.0, 99.9] {
                let floor = candidate_floor_bps(Some(slowest), Some(bound)).expect("a floor");
                // Any port at least as fast as the slowest one, at exactly its bound, must be at
                // or above the floor — otherwise the query drops a breaching port silently.
                for speed in [slowest, slowest * 2, slowest * 1000] {
                    let at_bound = speed as f64 * bound / 100.0;
                    assert!(
                        at_bound >= floor,
                        "floor {floor} would hide a {speed}bps port at {bound}%"
                    );
                }
            }
        }
    }

    #[test]
    fn no_covered_port_and_no_bound_means_no_query() {
        assert_eq!(candidate_floor_bps(None, Some(90.0)), None);
        assert_eq!(candidate_floor_bps(Some(1_000), None), None);
        assert_eq!(candidate_floor_bps(Some(0), Some(90.0)), None);
        assert_eq!(candidate_floor_bps(Some(1_000), Some(0.0)), None);
        // A bound over 100% must not push the floor past what the port can emit, or the query
        // would exclude the very port the rule is about.
        assert_eq!(candidate_floor_bps(Some(1_000), Some(150.0)), Some(1_000.0));
    }

    #[test]
    fn evaluate_drops_the_ports_it_cannot_judge_and_says_how_many() {
        let node = Uuid::new_v4();
        let mut speeds = BTreeMap::new();
        speeds.insert((node, 1), Some(100_000_000));
        speeds.insert((node, 2), None); // agent reports no speed
        speeds.insert((node, 3), Some(0)); // unnegotiated
                                           // Port 4 is absent from the table entirely — the interface row was deleted between the
                                           // TSDB read and the PostgreSQL read.
        let candidates = vec![
            (node, 1, 90_000_000.0),
            (node, 2, 90_000_000.0),
            (node, 3, 90_000_000.0),
            (node, 4, 90_000_000.0),
        ];
        let (out, skipped) = evaluate(&candidates, &speeds);
        assert_eq!(skipped, 3);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].ifindex, IfIndex(1));
        assert!((out[0].pct - 90.0).abs() < 1e-9);
    }

    #[test]
    fn a_check_that_leaves_the_candidate_set_is_reported_as_absent() {
        let node = NodeId::new();
        let other = NodeId::new();
        let mut tracked = TrackedChecks::default();
        tracked.mark((node, IfIndex(1), METRIC_IF_IN_UTIL_PCT));
        tracked.mark((node, IfIndex(2), METRIC_IF_IN_UTIL_PCT));
        tracked.mark((node, IfIndex(1), METRIC_IF_OUT_UTIL_PCT));
        tracked.mark((other, IfIndex(1), METRIC_IF_IN_UTIL_PCT));
        assert_eq!(tracked.len(), 4);

        // Only port 1 of `node` is still busy on the receive side.
        let present: BTreeSet<(NodeId, IfIndex)> = [(node, IfIndex(1))].into_iter().collect();
        // Compared as a set: the tracked checks are keyed by NodeId, so the iteration order
        // follows two random UUIDs and asserting a sequence would fail on roughly half of all runs.
        let absent: BTreeSet<CheckKey> = tracked
            .absent(METRIC_IF_IN_UTIL_PCT, &present)
            .into_iter()
            .collect();
        assert_eq!(
            absent,
            [
                (node, IfIndex(2), METRIC_IF_IN_UTIL_PCT),
                (other, IfIndex(1), METRIC_IF_IN_UTIL_PCT),
            ]
            .into_iter()
            .collect::<BTreeSet<_>>(),
            "the transmit-side check must not be swept by a receive-side tick"
        );

        // The other metric is tracked independently — a busy transmit side does not clear a
        // receive-side alert, which is the whole point of two metrics.
        let none: BTreeSet<(NodeId, IfIndex)> = BTreeSet::new();
        assert_eq!(tracked.absent(METRIC_IF_OUT_UTIL_PCT, &none).len(), 1);
    }
}
