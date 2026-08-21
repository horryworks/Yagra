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
use std::sync::Arc;
use std::time::Duration;

use uuid::Uuid;

use crate::alerts::{self, AlertManager, Notifier};
use crate::history::AlertHistoryStore;
use crate::repo::NodeRepo;
use crate::store::{self, MetricStore};
use yagra_common::{IfIndex, MetricKind, NodeId, NodeState};

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
pub fn derived_metric_kind(metric: &str) -> Option<MetricKind> {
    DERIVED_INTERFACE_METRICS
        .contains(&metric)
        .then_some(MetricKind::Gauge)
}

/// The interned name for a derived interface metric, or `None` for anything else.
///
/// Maps a name that arrived as a `String` — off an `Alert`, out of the database — back to the
/// `&'static str` that [`CheckKey`] and the threshold lookup are keyed by. Derived from
/// [`DERIVED_INTERFACE_METRICS`] rather than a hand-written `match`, so a fifth derived metric is
/// covered by adding it to that one list.
#[must_use]
pub fn derived_metric_name(metric: &str) -> Option<&'static str> {
    DERIVED_INTERFACE_METRICS.into_iter().find(|m| *m == metric)
}

/// Whether the evaluator may feed this node's ports an observation, given its **liveness** state.
///
/// The rule ADR-076 decision 3 wrote down is "freeze while the node is not `Ok`", and the intent was
/// always liveness: an unreachable device must keep its port alerts open and honest while its own
/// liveness alert does the paging, and a device in an `Unknown` state must not add "utilisation
/// unknown" noise on top of the outage.
///
/// 🚨 **What is passed in must be the liveness state, never the display roll-up.** The first
/// implementation asked `AlertManager::node_state`, which is the worse of liveness *and every active
/// alert on the node* — so a port alert made its own node read as `Warning`, the evaluator stopped
/// looking at that node, and nothing could resolve the alert afterwards at any traffic level or any
/// threshold (ADR-076 増分 7). This takes a bare state rather than an `AlertManager` precisely so the
/// mistake can only live at the call site.
///
/// `Maintenance` is let **through**, not frozen — decision 3's other half. Inside a window the
/// evaluator feeds `Maintenance`, so an open port alert resolves the way a node-level one does;
/// freezing here made port alerts the only kind a maintenance window could not silence.
///
/// `None` — never observed — is frozen: a node the engine has no opinion about is one whose liveness
/// has not been established, and raising a congestion alert about a device that may not be there is
/// the wrong way round.
#[must_use]
pub fn may_observe_ports(liveness: Option<NodeState>) -> bool {
    matches!(liveness, Some(NodeState::Ok) | Some(NodeState::Maintenance))
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
///
/// - **`above` only.** See [`BoundDemand`] and [`query_floor_bps`] for the `below` case, where the
///   premise this rests on ("carrying less than the floor cannot breach") is exactly inverted.
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

/// What the rules in force for one derived metric ask of the candidate query.
///
/// Two facts, because the floor cannot be computed from the bound alone: a `below` rule is
/// breached by the ports a floor *excludes*.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct BoundDemand {
    /// The smallest warning-or-critical bound any of them names, in the metric's own unit —
    /// percent for a utilisation metric, bits per second for an absolute one. `None` when no rule
    /// can fire (none exists, or none carries a bound).
    pub lowest_bound: Option<f64>,
    /// Whether any of them is a `below` rule.
    pub has_below: bool,
}

/// The floor for one direction's candidate query, over both of its derived metrics.
///
/// The query returns the ports at or above one absolute bits/sec value, and that one value has to
/// satisfy every rule on either metric. So: take each family's floor and keep the **smallest**,
/// since a lower floor only ever admits more ports, and a port the query drops is a port nothing
/// evaluates.
///
/// - the percentage family's floor is [`candidate_floor_bps`] — the slowest covered link at the
///   lowest percentage;
/// - the absolute family's floor is its lowest bound outright, no denominator involved;
/// - **either family containing a `below` rule pushes that family's floor to 0.** "Below the floor
///   is below every bound" is the premise this whole device rests on, and it says the opposite for
///   a `below` rule: the quiet ports the floor excludes are precisely the ones that should fire.
///   Before this, a `below` rule on a port metric could not fire at all unless the port had been
///   busy first. The cost is that such a direction queries every port that has a series, which is
///   why it is decided per metric rather than applied to every tick.
///
/// `None` when neither family can fire, which the caller reads as "run no query at all".
#[must_use]
pub fn query_floor_bps(
    slowest_covered_bps: Option<i64>,
    pct: BoundDemand,
    abs: BoundDemand,
) -> Option<f64> {
    let pct_floor = match pct.lowest_bound {
        None => None,
        Some(_) if pct.has_below => Some(0.0),
        bound => candidate_floor_bps(slowest_covered_bps, bound),
    };
    let abs_floor = match abs.lowest_bound {
        None => None,
        Some(_) if abs.has_below => Some(0.0),
        // A non-positive or non-finite absolute bound cannot select anything meaningful; floor it
        // at 0 so the query still runs rather than silently dropping the rule.
        Some(b) => Some(if b.is_finite() { b.max(0.0) } else { 0.0 }),
    };
    match (pct_floor, abs_floor) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (Some(a), None) | (None, Some(a)) => Some(a),
        (None, None) => None,
    }
}

/// One tracked per-port check: which node, which port, which derived metric.
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
    ///
    /// ⚠️ Call this **after** the observation, not before: the caller only knows a rule was in
    /// force from `observe_interface_metric` returning `Some` (ADR-076 increment 6d). Marking
    /// first tracked every port above the floor fleet-wide, including nodes nobody had written a
    /// rule for.
    pub fn mark(&mut self, key: CheckKey) {
        self.seen.insert(key);
    }

    /// Stop tracking a check — the rule it belonged to is gone.
    ///
    /// 🚨 This is the **only** way the set shrinks. Without it `seen` is a high-water mark rather
    /// than a live set: every key ever marked stays forever, and the recovery sweep walks all of
    /// them on every tick for the life of the process.
    pub fn forget(&mut self, key: &CheckKey) {
        self.seen.remove(key);
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

    /// How many checks are tracked for one metric — what the self-observability gauge reports.
    ///
    /// Per metric, not a whole-set total: the gauge carries a `metric` label, and publishing the
    /// same total once per label reads as "every metric is watching everything".
    ///
    /// The set only grows when a port crosses the query floor, so this is also the honest answer
    /// to "how much is this loop actually watching" — a fleet whose ports are all idle tracks
    /// nothing and costs nothing.
    #[must_use]
    pub fn count_for(&self, metric: &str) -> usize {
        self.seen.iter().filter(|(_, _, m)| *m == metric).count()
    }
}

/// One port's reading, ready to be fed to the alert engine.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PortReading {
    pub node: NodeId,
    pub ifindex: IfIndex,
    /// Bits per second, straight from the store. Always present: no denominator is involved, which
    /// is what lets an absolute rule cover a port whose speed the device never reports.
    pub bps: f64,
    /// The same figure as a percentage of the port's own speed, or `None` when that speed is
    /// unusable (absent, zero, or the saturated sentinel — see [`utilisation_pct`]).
    pub pct: Option<f64>,
}

/// Turn a candidate set and a speed table into readings.
///
/// Returns one reading per addressable port, plus a count of the ports whose **percentage** could
/// not be computed. That count is not diagnostic noise: "no alert fired" and "we could not
/// evaluate this port at all" look identical from the outside, and without a number an operator
/// has no way to learn that a third of the fleet is uncovered by a percentage rule.
///
/// A port with no usable speed is **still returned**, with `pct: None`. Dropping it here is what
/// the first version did, and it made those ports unmonitorable by any rule at all; the absolute
/// metrics exist precisely to reach them.
#[must_use]
pub fn evaluate(
    candidates: &[(Uuid, i32, f64)],
    speeds: &BTreeMap<(Uuid, i32), Option<i64>>,
) -> (Vec<PortReading>, usize) {
    let mut out = Vec::with_capacity(candidates.len());
    let mut no_pct = 0_usize;
    for (node, ifindex, bps) in candidates {
        let Ok(idx) = u32::try_from(*ifindex) else {
            // A negative ifindex cannot come from the TSDB label parser, but it is the one value
            // `i32` can hold that `IfIndex` cannot — skipping beats wrapping into another port.
            no_pct += 1;
            continue;
        };
        let speed = speeds.get(&(*node, *ifindex)).copied().flatten();
        let pct = utilisation_pct(*bps, speed);
        if pct.is_none() {
            no_pct += 1;
        }
        out.push(PortReading {
            node: NodeId::from(*node),
            ifindex: IfIndex(idx),
            bps: *bps,
            pct,
        });
    }
    (out, no_pct)
}

/// Which VictoriaMetrics dimension each direction's derived metric pair is computed from.
///
/// One entry per **direction**, not per metric: the percentage and the absolute rate come out of
/// the same answer, so a direction costs one query however many of its metrics carry rules.
///
/// The names come from `DERIVED_PAIRS` rather than being written again here; what this table adds
/// is the dimension, which `interface_util` cannot name — it is the pure half and knows nothing
/// about the store. The index-to-direction pairing is what
/// `the_dimensions_match_their_metric_pairs` exists to pin.
pub(crate) const INTERFACE_DIMENSIONS: [(
    crate::interface_util::DerivedPair,
    store::InterfaceTopMetric,
); 2] = [
    (
        crate::interface_util::DERIVED_PAIRS[0],
        store::InterfaceTopMetric::InBps,
    ),
    (
        crate::interface_util::DERIVED_PAIRS[1],
        store::InterfaceTopMetric::OutBps,
    ),
];

/// Evaluate per-interface bandwidth utilisation against the threshold rules, on the leader
/// (ADR-076 decision 3).
///
/// Modelled on [`run_pool_coverage_watch`] — the other loop that raises alerts from outside the
/// poll path — and it follows the same order for the same reason: persist to History **before**
/// notifying, so an operator who is paged can always find the row.
///
/// Utilisation is not a stored series (ADR-012), so each tick is: ask the rules what they cover →
/// compute the query floor from the slowest covered port → ask VictoriaMetrics for the ports above
/// it → join their speeds from PostgreSQL → divide → feed the engine. The pure half of that is
/// [`crate::interface_util`]; this function is the I/O and the ordering.
///
/// Leader-only for the same reason pool coverage is: two instances evaluating the same ports would
/// double every notification, and the engine's check state is process-local.
pub(crate) async fn run_interface_utilization_watch(
    store: Arc<dyn MetricStore>,
    repo: Arc<NodeRepo>,
    alerts: Arc<AlertManager>,
    notifier: Arc<Notifier>,
    history: Arc<AlertHistoryStore>,
) {
    use crate::interface_util as util;
    use std::collections::{BTreeMap, BTreeSet};

    let mut tracked = util::TrackedChecks::default();
    loop {
        tokio::time::sleep(util::WATCH_TICK).await;
        let now_ms = chrono::Utc::now().timestamp_millis();

        // One action list per tick, drained once at the bottom. The orphan sweep below runs even
        // when no dimension does (a deleted rule is exactly the case where nothing can fire), so
        // the dispatch cannot live inside the dimension loop the way it used to.
        let mut actions = Vec::new();

        // Close the port alerts whose rule was deleted. The poll path's `!alerting` branch does
        // this for every check it visits, but it never visits a derived per-interface check —
        // nothing polls `if_in_util_pct` — so before this, deleting a port rule stranded its alert
        // for the life of the process (ADR-076 増分 7 決定 14).
        for action in alerts.resolve_orphaned_interface_alerts() {
            // Stop asking about a port nobody has a rule for. `TrackedChecks` has no other way to
            // shrink (増分 6d), and the recovery sweep below is skipped entirely when a metric has
            // no rule left — so without this the key would outlive the rule forever.
            if let crate::alerts::NotifyAction::Resolve(a) = &action {
                if let (Some(node), Some(idx), Some(m)) =
                    (a.node(), a.ifindex, util::derived_metric_name(&a.metric))
                {
                    tracked.forget(&(node, idx, m));
                }
            }
            actions.push(action);
        }

        for (pair, dimension) in INTERFACE_DIMENSIONS {
            let pct_cov = alerts.interface_rule_coverage(pair.pct);
            let bps_cov = alerts.interface_rule_coverage(pair.bps);
            let pct_demand = util::BoundDemand {
                lowest_bound: pct_cov.lowest_bound,
                has_below: pct_cov.has_below,
            };
            let bps_demand = util::BoundDemand {
                lowest_bound: bps_cov.lowest_bound,
                has_below: bps_cov.has_below,
            };
            let pct_can_fire = pct_demand.lowest_bound.is_some();
            let bps_can_fire = bps_demand.lowest_bound.is_some();
            if !pct_can_fire && !bps_can_fire {
                // No rule that can fire on either metric. Nothing is queried and nothing is swept
                // clear — a port that was alerting when its rule was deleted is resolved by the
                // config refresh, not by pretending its traffic dropped.
                continue;
            }
            // The union of both families' node sets. A family with no rule contributes an empty
            // set; one scoped too broadly to enumerate contributes `None`, which means the fleet.
            let nodes = match (&pct_cov.nodes, &bps_cov.nodes) {
                (Some(a), Some(b)) => Some(a.union(b).copied().collect::<BTreeSet<Uuid>>()),
                _ => None,
            };
            if nodes.as_ref().is_some_and(BTreeSet::is_empty) {
                continue;
            }
            let scoped: Vec<Uuid> = nodes
                .as_ref()
                .map(|s| s.iter().copied().collect())
                .unwrap_or_default();

            // Only the percentage family needs a denominator, so an absolute-only tick asks
            // PostgreSQL nothing. Taken over the union rather than over the percentage rules'
            // own nodes: a slower link in the other family can only *lower* the floor, and a
            // lower floor never hides a breaching port.
            let slowest = if pct_can_fire {
                match repo.slowest_interface_speed_bps(&scoped).await {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!(error = %e, metric = pair.pct, "reading the interface speed floor failed");
                        continue;
                    }
                }
            } else {
                None
            };
            let Some(floor) = util::query_floor_bps(slowest, pct_demand, bps_demand) else {
                // Percentage rules exist but no covered port has a usable speed, and no absolute
                // rule is there to reach them. Not an error — it is what a fleet of agents that
                // implement neither speed OID looks like — but it is invisible without a number.
                metrics::gauge!("yagra_interface_util_unknown_speed", "metric" => pair.pct)
                    .set(0.0);
                continue;
            };

            // `scoped` is the union of both families' node sets — computed above for the speed
            // floor and, until increment 6c, thrown away afterwards. `None` here means the same
            // thing it means there: a rule is scoped too broadly to enumerate, so ask the fleet.
            let scope: Option<&[Uuid]> = nodes.is_some().then_some(scoped.as_slice());
            let Some(candidates) = store.interface_candidates(dimension, floor, scope).await else {
                // The store could not answer. Skip the whole tick rather than treating it as
                // "nothing is busy": the recovery sweep below would otherwise resolve every open
                // interface alert and send a recovery to PagerDuty for each, then re-fire them all
                // when VictoriaMetrics came back.
                metrics::counter!("yagra_interface_util_query_failures_total").increment(1);
                tracing::warn!(
                    metric = pair.pct,
                    "interface-utilisation query did not answer; tick skipped"
                );
                continue;
            };

            // Speeds for exactly the candidate nodes — bounded by what crossed the floor, not by
            // the fleet. Skipped entirely when only absolute rules are in force.
            let speeds: BTreeMap<(Uuid, i32), Option<i64>> = if pct_can_fire {
                let ids: Vec<Uuid> = candidates
                    .iter()
                    .map(|(n, _, _)| *n)
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect();
                match repo.interface_idents_for(&ids).await {
                    Ok(m) => m.iter().map(|(k, v)| (*k, v.if_speed)).collect(),
                    Err(e) => {
                        tracing::warn!(error = %e, metric = pair.pct, "reading interface speeds failed");
                        continue;
                    }
                }
            } else {
                BTreeMap::new()
            };

            let (readings, no_pct) = util::evaluate(&candidates, &speeds);
            if pct_can_fire {
                metrics::gauge!("yagra_interface_util_unknown_speed", "metric" => pair.pct)
                    .set(no_pct as f64);
            }
            metrics::gauge!("yagra_interface_util_tracked", "metric" => pair.pct)
                .set(tracked.count_for(pair.pct) as f64);
            metrics::gauge!("yagra_interface_util_tracked", "metric" => pair.bps)
                .set(tracked.count_for(pair.bps) as f64);

            let mut present = BTreeSet::new();
            for r in &readings {
                present.insert((r.node, r.ifindex));
                // A node whose *liveness* is not Ok is frozen, not observed. Feeding `Ok` would
                // resolve a real congestion alert the moment the device went unreachable; feeding
                // the reading would page about a link on a box that is already down.
                //
                // 🚨 Liveness, never the display roll-up: the roll-up folds in this port's own
                // alert, so gating on it froze the evaluator on its own output and the alert could
                // never clear (ADR-076 増分 7 決定 13). A maintenance window is let through, so an
                // open port alert resolves inside one the way a node-level alert does.
                if !util::may_observe_ports(alerts.node_liveness(r.node)) {
                    continue;
                }
                // ⚠️ The mark comes *after* the observation, and that ordering is the point
                // (ADR-076 increment 6d). Marking first tracked every port above the floor —
                // fleet-wide, including nodes nobody wrote a rule for — and since `TrackedChecks`
                // never shrank, the recovery sweep then walked that set on every subsequent tick
                // forever. `None` means no rule resolved: nothing to remember, nothing to recover.
                if let (true, Some(pct)) = (pct_can_fire, r.pct) {
                    if let Some(a) =
                        alerts.observe_interface_metric(r.node, r.ifindex, pair.pct, pct, now_ms)
                    {
                        tracked.mark((r.node, r.ifindex, pair.pct));
                        actions.extend(a);
                    }
                }
                if bps_can_fire {
                    if let Some(a) =
                        alerts.observe_interface_metric(r.node, r.ifindex, pair.bps, r.bps, now_ms)
                    {
                        tracked.mark((r.node, r.ifindex, pair.bps));
                        actions.extend(a);
                    }
                }
            }

            // Anything tracked that is no longer above the floor has recovered: below the floor is
            // below every rule's bound by construction. Frozen nodes are skipped here too, for the
            // same reason they are skipped above.
            //
            // Not done for a metric that has a `below` rule, and the reason is the same inversion
            // that `query_floor_bps` handles: the floor is zero there, so a port only leaves the
            // candidate set by losing its series altogether — and calling that "0 bits/sec" would
            // *fire* a `below` rule on a port nobody has heard from, which is a data gap being
            // reported as a traffic level.
            for metric in [pair.pct, pair.bps] {
                let (can_fire, has_below) = if metric == pair.pct {
                    (pct_can_fire, pct_demand.has_below)
                } else {
                    (bps_can_fire, bps_demand.has_below)
                };
                if !can_fire || has_below {
                    continue;
                }
                for key in tracked.absent(metric, &present) {
                    let (node, ifindex, m) = key;
                    if !util::may_observe_ports(alerts.node_liveness(node)) {
                        continue;
                    }
                    match alerts.observe_interface_metric(node, ifindex, m, 0.0, now_ms) {
                        Some(a) => actions.extend(a),
                        // The rule was deleted between the mark and now. Drop the key instead of
                        // asking about it every tick for the life of the process; any alert it left
                        // open was already closed by the orphan sweep at the top of this tick — not
                        // by claiming the traffic fell to zero.
                        None => tracked.forget(&key),
                    }
                }
            }
        }

        // One drain per tick, for every dimension and for the orphan sweep. History first, then
        // the notifier: Increment 1 wired this loop to the notifier alone and the alerts paged with
        // no row behind them, so `a_transition_from_the_interface_watch_reaches_the_history_store`
        // pins both calls to this function's body.
        for action in actions {
            if let Some(alert) = alerts::recordable_alert(&action) {
                let resolved = matches!(action, crate::alerts::NotifyAction::Resolve(_));
                if let Err(e) = history.record(alert, resolved).await {
                    tracing::warn!(error = %e, "recording an interface-utilisation transition failed");
                }
            }
            notifier.handle(action).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// This module's own source, for the structural assertions below.
    ///
    /// It reads `interface_util.rs` rather than `main.rs` because ADR-083 moved the loop here. Repointing it
    /// is not optional bookkeeping: a needle aimed at the old file finds nothing and panics, which
    /// is the loud half of the failure — the quiet half is a `.split()` argument that still matches
    /// something and checks nothing. Both assertions below were re-run against a deliberately
    /// broken body after the move, and both failed, which is the only reason to believe them.
    const SRC: &str = include_str!("interface_util.rs");

    /// The dimension table is positional, and getting it backwards is silent: the loop would
    /// evaluate receive rules against transmit traffic and never fail anywhere.
    #[test]
    fn the_dimensions_match_their_metric_pairs() {
        use crate::interface_util as util;
        use store::InterfaceTopMetric;

        assert_eq!(INTERFACE_DIMENSIONS.len(), util::DERIVED_PAIRS.len());
        for (pair, dimension) in INTERFACE_DIMENSIONS {
            let want = match dimension {
                InterfaceTopMetric::InBps => {
                    ("in", util::METRIC_IF_IN_UTIL_PCT, util::METRIC_IF_IN_BPS)
                }
                InterfaceTopMetric::OutBps => {
                    ("out", util::METRIC_IF_OUT_UTIL_PCT, util::METRIC_IF_OUT_BPS)
                }
                // The loop reads one direction at a time; a combined or per-error dimension has
                // no percentage to divide and no pair to observe.
                InterfaceTopMetric::Throughput
                | InterfaceTopMetric::Errors
                | InterfaceTopMetric::Discards => {
                    panic!("{dimension:?} is not a single direction")
                }
            };
            assert_eq!(pair.pct, want.1, "{} percentage", want.0);
            assert_eq!(pair.bps, want.2, "{} absolute rate", want.0);
        }
        // Both directions are present, and neither is listed twice.
        let dims: std::collections::BTreeSet<&str> = INTERFACE_DIMENSIONS
            .iter()
            .map(|(pair, _)| pair.pct)
            .collect();
        assert_eq!(dims.len(), INTERFACE_DIMENSIONS.len());
    }

    /// The interface watch must record to History as well as notify — the same property
    /// `a_pool_coverage_transition_reaches_the_history_store` pins for the pool watch, and for the
    /// same reason: Increment 1 wired that loop to the notifier alone, the alerts paged, and
    /// History stayed empty with every gauge and log line looking correct.
    ///
    /// Structural because the loop body is a 60-second tick around VictoriaMetrics, PostgreSQL and
    /// a notifier. It also guards the ADR-076 増分 7 refactor that moved the drain out of the
    /// per-dimension loop so the orphan sweep could share it: a drain that ended up outside this
    /// function would still compile.
    #[test]
    fn a_transition_from_the_interface_watch_reaches_the_history_store() {
        let production = SRC
            .split("#[cfg(test)]")
            .next()
            .expect("split always yields a first element");
        let watch = production
            .split("async fn run_interface_utilization_watch")
            .nth(1)
            .expect("the watch loop exists");
        let body = &watch[..watch.find("\nfn ").unwrap_or(watch.len())];
        assert!(
            body.contains("history.record("),
            "the interface watch notifies without recording — the alert pages and History stays \
             empty"
        );
        assert!(
            body.contains("notifier.handle("),
            "…and it must still notify"
        );
        assert!(
            body.contains("resolve_orphaned_interface_alerts()"),
            "without the sweep, deleting a port rule strands its alert for the life of the process"
        );
    }

    #[test]
    fn every_derived_metric_is_a_gauge_and_is_not_collected() {
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

    /// The floor over both families. Its job is to admit every port any rule could fire on.
    #[test]
    fn the_query_floor_takes_the_lowest_of_the_two_families() {
        let none = BoundDemand::default();
        let pct90 = BoundDemand {
            lowest_bound: Some(90.0),
            has_below: false,
        };
        // 90% of the slowest covered 100 Mbps link.
        assert_eq!(
            query_floor_bps(Some(100_000_000), pct90, none),
            Some(90_000_000.0)
        );
        // An absolute rule at 8 Mbps is lower, so it wins — a lower floor only admits more.
        let abs8m = BoundDemand {
            lowest_bound: Some(8_000_000.0),
            has_below: false,
        };
        assert_eq!(
            query_floor_bps(Some(100_000_000), pct90, abs8m),
            Some(8_000_000.0)
        );
        // Absolute only: no denominator is consulted, so an unknown slowest speed is irrelevant.
        assert_eq!(query_floor_bps(None, none, abs8m), Some(8_000_000.0));
        // A percentage rule with no covered port that reports a speed contributes nothing — but
        // it must not veto the absolute family, which is exactly the case those ports need.
        assert_eq!(query_floor_bps(None, pct90, abs8m), Some(8_000_000.0));
        assert_eq!(query_floor_bps(None, pct90, none), None);
        // Nothing can fire ⇒ no query at all.
        assert_eq!(query_floor_bps(Some(100_000_000), none, none), None);
    }

    /// The regression this increment exists for: a `below` rule could not fire on a quiet port,
    /// because the floor excluded exactly the ports it was about.
    #[test]
    fn a_below_rule_drops_the_floor_to_zero() {
        let none = BoundDemand::default();
        let below10 = BoundDemand {
            lowest_bound: Some(10.0),
            has_below: true,
        };
        assert_eq!(query_floor_bps(Some(100_000_000), below10, none), Some(0.0));
        // …and it wins over an `above` rule in the other family, for the same reason a lower
        // floor always wins: the query has to satisfy both.
        let abs8m = BoundDemand {
            lowest_bound: Some(8_000_000.0),
            has_below: false,
        };
        assert_eq!(
            query_floor_bps(Some(100_000_000), below10, abs8m),
            Some(0.0)
        );
        // An absolute `below` does the same.
        let abs_below = BoundDemand {
            lowest_bound: Some(8_000_000.0),
            has_below: true,
        };
        assert_eq!(query_floor_bps(None, none, abs_below), Some(0.0));
        // A port carrying nothing at all is admitted by that floor — which is the whole point.
        assert!(0.0 >= query_floor_bps(Some(100_000_000), below10, none).unwrap());
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
    fn evaluate_keeps_every_port_and_counts_the_ones_with_no_percentage() {
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
        let (out, no_pct) = evaluate(&candidates, &speeds);
        // Three ports cannot be expressed as a percentage…
        assert_eq!(no_pct, 3);
        // …but all four are still readings, because an absolute rule can judge every one of them.
        assert_eq!(out.len(), 4);
        assert_eq!(out[0].ifindex, IfIndex(1));
        assert!((out[0].pct.expect("speed known") - 90.0).abs() < 1e-9);
        assert!((out[0].bps - 90_000_000.0).abs() < 1e-9);
        for r in &out[1..] {
            assert_eq!(r.pct, None, "port {:?} has no usable speed", r.ifindex);
            assert!((r.bps - 90_000_000.0).abs() < 1e-9);
        }
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
        assert_eq!(tracked.count_for(METRIC_IF_IN_UTIL_PCT), 3);
        assert_eq!(tracked.count_for(METRIC_IF_OUT_UTIL_PCT), 1);
        assert_eq!(tracked.count_for(METRIC_IF_IN_BPS), 0);

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
    #[test]
    fn a_forgotten_check_leaves_the_tracked_set() {
        // 🚨 Before increment 6d there was no `forget` at all, so `seen` was a high-water mark:
        // every port ever above the floor stayed in it, and the recovery sweep asked about all of
        // them on every tick for the life of the process. The sweep now drops a key whose rule has
        // gone. This test is the only thing that says the set can shrink.
        let node = NodeId::from(Uuid::new_v4());
        let key = (node, IfIndex(7), METRIC_IF_IN_UTIL_PCT);
        let none: BTreeSet<(NodeId, IfIndex)> = BTreeSet::new();

        let mut tracked = TrackedChecks::default();
        tracked.mark(key);
        // Marked and not present ⇒ the sweep would ask about it.
        assert_eq!(tracked.absent(METRIC_IF_IN_UTIL_PCT, &none), vec![key]);
        assert_eq!(tracked.count_for(METRIC_IF_IN_UTIL_PCT), 1);

        tracked.forget(&key);
        assert!(tracked.absent(METRIC_IF_IN_UTIL_PCT, &none).is_empty());
        assert_eq!(tracked.count_for(METRIC_IF_IN_UTIL_PCT), 0);

        // And forgetting is per key, not per metric or per node: a sibling port must survive.
        let sibling = (node, IfIndex(8), METRIC_IF_IN_UTIL_PCT);
        tracked.mark(key);
        tracked.mark(sibling);
        tracked.forget(&key);
        assert_eq!(tracked.absent(METRIC_IF_IN_UTIL_PCT, &none), vec![sibling]);
    }

    /// The whole state table, because the gate is one line at two call sites and the interesting
    /// half is what it *rejects*.
    #[test]
    fn the_freeze_gate_answers_for_every_liveness_state() {
        assert!(may_observe_ports(Some(NodeState::Ok)));
        // A window must not freeze the loop, or an open port alert can never be silenced by one.
        assert!(may_observe_ports(Some(NodeState::Maintenance)));

        // The device is not there, or we could not run the check: keep the port alert open and
        // honest and let the node's own liveness alert do the paging.
        assert!(!may_observe_ports(Some(NodeState::Unreachable)));
        assert!(!may_observe_ports(Some(NodeState::Unknown)));
        // Never observed is "no opinion", not "fine".
        assert!(!may_observe_ports(None));

        // Those four plus `None` are the whole domain: the liveness map is written only from a
        // reachability outcome (`Reachable`/`Unreachable`/`Error`) or a maintenance substitution,
        // so it never holds `Warning` or `Critical`. They are pinned as frozen anyway — the
        // conservative answer — so that a caller regressing to the display roll-up produces a
        // visibly wrong monitoring gap rather than a silently different rule.
        assert!(!may_observe_ports(Some(NodeState::Warning)));
        assert!(!may_observe_ports(Some(NodeState::Critical)));
    }

    /// The interning that lets a runtime `String` off an `Alert` become a `CheckKey`.
    #[test]
    fn only_the_four_derived_names_intern() {
        for m in DERIVED_INTERFACE_METRICS {
            // The `&'static str` is what matters: `CheckKey` is keyed by it, so a `String` here
            // would not compile at the call site.
            let interned: &'static str = derived_metric_name(m).expect("a derived metric interns");
            assert_eq!(interned, m);
        }
        // A collected per-interface metric belongs to the poll path, not to this module's sweep.
        assert_eq!(derived_metric_name("if_oper_status"), None);
        assert_eq!(derived_metric_name("icmp_rtt_ms"), None);
        assert_eq!(derived_metric_name(""), None);
        // Not a prefix match: a longer name starting with a derived one is a different metric.
        assert_eq!(derived_metric_name("if_in_util_pct_avg"), None);
    }
}
