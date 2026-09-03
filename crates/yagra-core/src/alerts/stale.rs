// SPDX-License-Identifier: AGPL-3.0-only
//! The loop that notices an alert nothing is evaluating any more (ADR-097 Increment 6).
//!
//! Two failures of one family, both of which left an alert open for the life of the process:
//!
//! * **The rule was deleted.** `observe` `continue`s a sample whose threshold does not resolve
//!   *before* `process_check` is reached, and `observe_threshold_sample` hard-codes
//!   `alerting: true` — so the `!alerting` close branch is reachable only by the liveness check.
//!   Nothing closed a collected metric's alert. Two doc comments and two tests said otherwise.
//! * **The data stopped.** A node whose SNMP credential is detached stops producing `snmp_up`
//!   entirely, so `observe` never visits that check again and no rule lookup can tell: the rule is
//!   still there, and still resolves. Measured on `.211` 2026-09-02 — four nodes red on a metric
//!   whose last sample was 4.33 days old, across three restarts.
//!
//! The judgement for the first lives in [`AlertManager::resolve_orphaned_collected_alerts`]; for
//! the second, [`AlertManager::freshness_candidates`] says *what to ask* and this file asks it.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use uuid::Uuid;
use yagra_common::{CheckId, NodeKind};

use super::engine::FreshnessCandidate;
use super::sink::AlertSink;
use super::AlertManager;
use crate::store::MetricStore;

/// How often the two questions are asked.
///
/// Deliberately not the siblings' 60 seconds. The rule half's answer can only change when the alert
/// config snapshot is rebuilt (gated on the config generation behind a 30-second refresh), and the
/// freshness half's answer cannot change faster than [`STALE_WINDOW_SECS`], which is hours. A minute
/// tick would ask the same question five times over, at `1 + N` VictoriaMetrics queries a time.
///
/// Bounds "the rule was deleted" to "closed" at roughly `TICK`, and "the data stopped" at
/// `STALE_WINDOW_SECS + TICK`.
const TICK: Duration = Duration::from_secs(300);

/// How long a candidate's own series may be silent before the alert is considered unevaluated.
///
/// **Six times the slowest interval an operator can configure.** With Meraki excluded (see
/// [`MerakiNodeSet`]) the slowest cadence an *alertable* series can have is the node's own poll
/// interval, which `app_settings_default_poll_interval_secs_check` and the identical CHECK on
/// `profiles.poll_interval_secs` cap at 3600 seconds. Every slower cadence in the tree — neighbours,
/// media, l3, arp (21600 default, 86400 max), routing — rides a check whose result is
/// `observational`, so it never reaches the alert engine and can never produce a candidate.
///
/// So a series may miss five consecutive polls at the slowest legal setting and still count as
/// flowing; at the 30-second default it is 720 missed polls. Measured against the real strandings
/// this shipped for: 4.33 days, 8.5 hours, and two with no sample in 30 days.
///
/// ⚠️ **This number rests on that 3600-second cap.** Raise it, or add a check that alerts on a
/// slower cadence, and this window becomes quietly too short — which is why the test below is
/// written against the constant rather than against `21_600`.
const STALE_WINDOW_SECS: u64 = 21_600;

/// How long the candidate's **node** may be silent and still count as answering.
///
/// Twice the slowest configurable interval, so a node at the cap may miss one poll.
///
/// 🚨 **`STALE_WINDOW_SECS >= LIVE_WINDOW_SECS` is what makes total silence safe.** For a node
/// silent for S seconds the canary passes iff `S < LIVE` and the series is fresh iff `S < STALE`,
/// so closing requires `STALE <= S < LIVE` — an empty range whenever `STALE >= LIVE`. A dead
/// poller, a severed site or a bus outage therefore cannot close a single alert, at any duration.
/// Pinned by a test.
const LIVE_WINDOW_SECS: u64 = 7_200;

/// How many node ids go into one freshness query.
///
/// [`MetricStore::fresh_node_ids_scoped`] joins the scope with `|` into a `node=~"…"` selector on a
/// GET query string — about 37 bytes per id. The existing caller gets away with no chunking because
/// its scope is one page of the node list; this one's scope is "every node with an open
/// node-dimension alert", which on a bad day is thousands.
const SCOPE_CHUNK: usize = 200;

/// Which of these nodes are Meraki devices.
///
/// A seam, for one reason: the concrete [`crate::meraki::MerakiDeviceRepo`] needs a live PostgreSQL,
/// and the fail-closed behaviour below is the part most worth testing.
///
/// # 🚨 Why Meraki is excluded at all
///
/// `MERAKI_TRAFFIC_MAX_SECS` is 86_400 and `MERAKI_INVENTORY_MAX_SECS` is **604_800 — seven days**
/// (`config::…`). A Meraki node still gets an ICMP check from `assemble_node_jobs`, so its liveness
/// arrives every poll interval while a traffic or inventory metric can legitimately be a week apart.
/// A window that covered that would be longer than the strandings this sweep exists to catch
/// (4.33 days, measured), so the whole node kind is out of scope instead.
///
/// Excluded **structurally, never by a `meraki_` name prefix** — a string rule silently misses rows
/// where a set membership cannot.
#[async_trait]
pub(crate) trait MerakiNodeSet: Send + Sync {
    async fn meraki_subset(&self, node_ids: &[Uuid]) -> anyhow::Result<HashSet<Uuid>>;
}

#[async_trait]
impl MerakiNodeSet for crate::meraki::MerakiDeviceRepo {
    async fn meraki_subset(&self, node_ids: &[Uuid]) -> anyhow::Result<HashSet<Uuid>> {
        self.filter_meraki(node_ids).await
    }
}

/// The fresh subset of `scope` for `metrics`, asked in bounded chunks.
async fn fresh_within(
    store: &dyn MetricStore,
    metrics: &[&str],
    window_secs: u64,
    scope: &[Uuid],
) -> HashSet<Uuid> {
    let mut fresh = HashSet::new();
    for part in scope.chunks(SCOPE_CHUNK) {
        fresh.extend(
            store
                .fresh_node_ids_scoped(metrics, window_secs, part)
                .await,
        );
    }
    fresh
}

/// Which candidates nothing is measuring, given what the store answered.
///
/// Pure, so the decision is testable without a TSDB. A candidate is stranded when its node is
/// demonstrably answering **and** at least one of the series that would feed its check is missing —
/// "at least one" because a derived metric with one input gone has nothing left to compute from.
fn stranded(
    candidates: &[FreshnessCandidate],
    answering: &HashSet<Uuid>,
    fresh: &HashMap<String, HashSet<Uuid>>,
) -> Vec<CheckId> {
    candidates
        .iter()
        .filter(|c| answering.contains(&c.node.as_uuid()))
        .filter(|c| {
            c.inputs.iter().any(|series| {
                !fresh
                    .get(series)
                    .is_some_and(|nodes| nodes.contains(&c.node.as_uuid()))
            })
        })
        .map(|c| c.check)
        .collect()
}

/// One pass: close what no rule evaluates, then close what no data feeds.
///
/// Returns the resolutions for the caller to deliver, so the whole decision — including both
/// refusals below — is reachable from a test with fakes.
///
/// # 🚨 The liveness canary is the safety property, not a convenience — and it is asked twice
///
/// [`MetricStore::fresh_node_ids_scoped`] returns `Vec::new()` on a transport error **and** on a
/// JSON parse failure, so an empty answer is indistinguishable from "nothing is fresh". Without
/// asking whether these nodes are reporting *at all*, one VictoriaMetrics blip would close every
/// open alert in the fleet and send a recovery for each — the ADR-080 accident, arrived at from a
/// different direction. **Both answers come from the same store, which is what makes it sound.**
///
/// It is asked **before and after** the per-series reads, and a node must appear in both to be
/// closed. One canary only covers a store that was already down when the tick began; the window it
/// leaves open is a store that answers the canary and then fails the series queries, which reads as
/// "every series is missing" and closes everything answering. Bookending removes the case where the
/// store was down for any part of the read.
///
/// ⚠️ **What is left, stated rather than implied**: a store that fails *only* between the two
/// canaries and recovers before the second is still read as evidence. That residue is bounded
/// rather than eliminated, and the damage is a spurious resolve followed by a re-fire on the next
/// poll — not the permanent silence a dropped dwell window would cause.
///
/// ⚠️ Two lines enforce this and each covers the other's gap: the early return when the first
/// canary is empty (which also saves the per-series queries), and [`stranded`]'s membership test.
/// Removing either alone leaves both tests green; removing both fails them. Neither is redundant —
/// they answer for the whole-fleet and per-node cases respectively.
///
/// # 🚨 Meraki identification fails closed, unlike `scheduler/sweep.rs`
///
/// That call site degrades to an empty set safely, because the dispatcher short-circuits Meraki
/// nodes anyway. Here an empty set means "no Meraki node was excluded", which is exactly the false
/// close the exclusion exists to prevent — so an `Err` skips the freshness half of the tick
/// entirely. The rule half above it is unaffected: it touches no store and cannot be wrong for
/// this reason.
async fn sweep_once(
    alerts: &AlertManager,
    store: &dyn MetricStore,
    meraki: &dyn MerakiNodeSet,
) -> Vec<super::NotifyAction> {
    // The rule half first, and before any `.await`: a deleted rule is exactly the case where the
    // rest of this tick has nothing left to ask about, and it must not cost two store queries.
    let mut actions = alerts.resolve_orphaned_collected_alerts();

    let candidates = alerts.freshness_candidates();
    if candidates.is_empty() {
        return actions;
    }

    let mut scope: Vec<Uuid> = candidates.iter().map(|c| c.node.as_uuid()).collect();
    scope.sort_unstable();
    scope.dedup();

    let meraki_nodes = match meraki.meraki_subset(&scope).await {
        Ok(set) => set,
        Err(e) => {
            tracing::warn!(
                error = %e,
                "could not identify Meraki nodes; skipping the freshness sweep this tick"
            );
            return actions;
        }
    };
    let candidates: Vec<FreshnessCandidate> = candidates
        .into_iter()
        .filter(|c| !meraki_nodes.contains(&c.node.as_uuid()))
        .collect();
    if candidates.is_empty() {
        return actions;
    }
    let scope: Vec<Uuid> = {
        let mut s: Vec<Uuid> = candidates.iter().map(|c| c.node.as_uuid()).collect();
        s.sort_unstable();
        s.dedup();
        s
    };

    let answering =
        fresh_within(store, &NodeKind::LIVENESS_METRICS, LIVE_WINDOW_SECS, &scope).await;
    if answering.is_empty() {
        // Not "the fleet is quiet" — this is also what a store that could not be reached looks
        // like, and the two must be treated the same way. See the doc above.
        return actions;
    }

    // One query per distinct series, scoped to the nodes that actually want it.
    let mut by_series: HashMap<&str, Vec<Uuid>> = HashMap::new();
    for c in &candidates {
        if !answering.contains(&c.node.as_uuid()) {
            continue;
        }
        for series in &c.inputs {
            by_series
                .entry(series.as_str())
                .or_default()
                .push(c.node.as_uuid());
        }
    }
    let mut fresh: HashMap<String, HashSet<Uuid>> = HashMap::new();
    for (series, mut nodes) in by_series {
        nodes.sort_unstable();
        nodes.dedup();
        let found = fresh_within(store, &[series], STALE_WINDOW_SECS, &nodes).await;
        fresh.insert(series.to_owned(), found);
    }

    // The closing half of the bookend: a node must have been answering *after* the series reads
    // too. Without this, a store that answered the canary and then failed every series query reads
    // as "every series is missing" and closes everything. See the doc above for what this still
    // does not cover.
    let still_answering =
        fresh_within(store, &NodeKind::LIVENESS_METRICS, LIVE_WINDOW_SECS, &scope).await;
    let answering: HashSet<Uuid> = answering.intersection(&still_answering).copied().collect();

    let checks = stranded(&candidates, &answering, &fresh);
    if !checks.is_empty() {
        tracing::info!(
            resolved = checks.len(),
            "closed the alerts of checks whose data stopped arriving"
        );
        actions.extend(alerts.resolve_stale_alerts(checks));
    }
    actions
}

/// Leader-only: close the alerts nothing is evaluating any more.
///
/// # Why its own task rather than a step of the config-refresh loop
///
/// The same reason `deleted` is: [`AlertSink::dispatch`] awaits the History insert **and** the
/// notification, and ADR-104 measured one notification taking up to 31.5 seconds against a slow
/// vendor. Folding this into `alerts::config::run_alert_config_refresh` would park maintenance-
/// window resolution, mute expiry and the classifier reload behind a notification storm.
///
/// # Why leader-only
///
/// Poll-result ingest is leader-only, so only the leader's engine holds any of this state, and two
/// instances dispatching the same resolution would double every notification. ➕ Unlike a design
/// that counted observations in process memory, a promoted standby is correct here by
/// construction — the store answers it the same way it answered the old leader.
///
/// # Why there is no startup cadence, unlike `deleted`
///
/// That loop runs fast for its first two minutes because ADR-097 Increment 5 gave it a transient
/// that exists only between startup and the first sweep — a restored deleted-node alert that makes
/// the fleet breakdown sum past its own total. This loop has no such transient: it reads a store,
/// so its answer one second after start is the same as its answer an hour later. The one thing it
/// waits for is a loaded config, and that is handled inside the engine methods rather than by a
/// cadence here. ⇒ an already-stranded alert closes on the **first** tick.
///
/// ⚠️ The safety properties this loop depends on live in [`sweep_once`] and in the engine methods,
/// not here. A guard at this call site would have to be remembered by the next caller.
pub(crate) async fn run_stale_check_watch(
    alerts: Arc<AlertManager>,
    store: Arc<dyn MetricStore>,
    meraki: Arc<dyn MerakiNodeSet>,
    sink: Arc<dyn AlertSink>,
) {
    loop {
        tokio::time::sleep(TICK).await;
        // Resolutions are dispatched one at a time and in order, the same as every other alert
        // source: `Dispatcher` serialises per channel anyway, so concurrency here would only move
        // the queue.
        for action in sweep_once(&alerts, store.as_ref(), meraki.as_ref()).await {
            sink.dispatch(action).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::alerts::testkit::{cfg, manager, meta_for, open_alert, result};
    use crate::alerts::NotifyAction;
    use crate::store::{DeltaDirection, InterfaceTopMetric, MetricPoint, TopAgg};
    use yagra_common::{NodeId, NodeState, ScopeLevel, SeriesKey, ThresholdBounds, ThresholdRule};

    /// A store holding exactly the `(metric, node)` series named — and able to go blind partway
    /// through a tick, which is the only way to reach the bookend.
    ///
    /// The window is deliberately ignored: what a real store returns for a six-hour range is
    /// exercised on the box, and what these tests are about is the *set*.
    struct ScriptedStore {
        fresh: HashMap<String, HashSet<Uuid>>,
        calls: std::sync::atomic::AtomicUsize,
        /// After this many `fresh_node_ids` calls, answer nothing — what a transport error and a
        /// JSON parse failure both look like through `VmStore`.
        blind_after: Option<usize>,
    }

    impl ScriptedStore {
        fn holding(series: &[(&str, NodeId)]) -> Self {
            let mut fresh: HashMap<String, HashSet<Uuid>> = HashMap::new();
            for (metric, node) in series {
                fresh
                    .entry((*metric).to_owned())
                    .or_default()
                    .insert(node.as_uuid());
            }
            Self {
                fresh,
                calls: std::sync::atomic::AtomicUsize::new(0),
                blind_after: None,
            }
        }
        fn blind_after(mut self, n: usize) -> Self {
            self.blind_after = Some(n);
            self
        }
    }

    #[async_trait]
    impl MetricStore for ScriptedStore {
        async fn fresh_node_ids(&self, metrics: &[&str], _within_secs: u64) -> Vec<Uuid> {
            let seen = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if self.blind_after.is_some_and(|n| seen >= n) {
                return Vec::new();
            }
            let mut out: Vec<Uuid> = metrics
                .iter()
                .filter_map(|m| self.fresh.get(*m))
                .flatten()
                .copied()
                .collect();
            out.sort();
            out.dedup();
            out
        }

        // Everything below is a required method this sweep never calls.
        async fn write(&self, _result: &yagra_bus::PollResult) {}
        async fn latest(&self, _key: &SeriesKey) -> Option<f64> {
            None
        }
        async fn range(
            &self,
            _key: &SeriesKey,
            _from_s: i64,
            _to_s: i64,
            _step_s: u64,
        ) -> Vec<MetricPoint> {
            Vec::new()
        }
        async fn rate_range(
            &self,
            _key: &SeriesKey,
            _from_s: i64,
            _to_s: i64,
            _step_s: u64,
            _lookback_s: u64,
        ) -> Vec<MetricPoint> {
            Vec::new()
        }
        async fn aggregate_latest(&self, _key: &SeriesKey) -> Option<f64> {
            None
        }
        async fn aggregate_range(
            &self,
            _key: &SeriesKey,
            _from_s: i64,
            _to_s: i64,
            _step_s: u64,
        ) -> Vec<MetricPoint> {
            Vec::new()
        }
        async fn top_nodes(&self, _metric: &str, _agg: TopAgg, _limit: usize) -> Vec<(Uuid, f64)> {
            Vec::new()
        }
        async fn top_interfaces(
            &self,
            _metric: InterfaceTopMetric,
            _agg: TopAgg,
            _limit: usize,
        ) -> Vec<(Uuid, i32, f64)> {
            Vec::new()
        }
        async fn interface_candidates(
            &self,
            _metric: InterfaceTopMetric,
            _floor_bps: f64,
            _nodes: Option<&[Uuid]>,
        ) -> Option<Vec<(Uuid, i32, f64)>> {
            None
        }
        async fn interface_delta(
            &self,
            _direction: DeltaDirection,
            _window_secs: u64,
            _limit: usize,
        ) -> Vec<(Uuid, i32, f64)> {
            Vec::new()
        }
        async fn interface_throughput_range(
            &self,
            _node: Uuid,
            _ifindex: i32,
            _from_s: i64,
            _to_s: i64,
            _step_s: u64,
        ) -> Vec<MetricPoint> {
            Vec::new()
        }
        async fn throughput_range(
            &self,
            _from_s: i64,
            _to_s: i64,
            _step_s: u64,
        ) -> (Vec<MetricPoint>, Vec<MetricPoint>) {
            (Vec::new(), Vec::new())
        }
    }

    fn store_with(series: &[(&str, NodeId)]) -> ScriptedStore {
        ScriptedStore::holding(series)
    }

    /// A `MerakiNodeSet` that answers from a fixed set, or fails.
    struct FakeMeraki {
        members: HashSet<Uuid>,
        fails: bool,
    }

    impl FakeMeraki {
        fn none() -> Self {
            Self {
                members: HashSet::new(),
                fails: false,
            }
        }
        fn holding(ids: &[Uuid]) -> Self {
            Self {
                members: ids.iter().copied().collect(),
                fails: false,
            }
        }
        fn broken() -> Self {
            Self {
                members: HashSet::new(),
                fails: true,
            }
        }
    }

    #[async_trait]
    impl MerakiNodeSet for FakeMeraki {
        async fn meraki_subset(&self, node_ids: &[Uuid]) -> anyhow::Result<HashSet<Uuid>> {
            if self.fails {
                anyhow::bail!("no database");
            }
            Ok(node_ids
                .iter()
                .copied()
                .filter(|id| self.members.contains(id))
                .collect())
        }
    }

    fn rule(node: NodeId, metric: &str) -> crate::thresholds::StoredThreshold {
        crate::thresholds::StoredThreshold::new(
            Uuid::new_v4(),
            ScopeLevel::Node,
            vec![node.to_string()],
            ThresholdRule::new(metric, ThresholdBounds::below(None, Some(0.5)), 1),
        )
    }

    /// A manager holding one open `snmp_up` alert on `node`, with the rule still in force.
    fn manager_with_open_snmp_alert(node: NodeId) -> Arc<AlertManager> {
        let mgr = Arc::new(manager());
        mgr.set_config(cfg(vec![rule(node, "snmp_up")], meta_for(node)));
        let mut r = result(node, yagra_bus::CheckOutcome::Reachable, 0);
        r.samples = vec![yagra_bus::Sample::gauge("snmp_up", 0.0)];
        let _ = mgr.observe(&r);
        assert_eq!(
            mgr.active_alerts().len(),
            1,
            "the alert is open to begin with"
        );
        mgr
    }

    /// 🚨 **The accepting half, and it is load-bearing.** A sweep that closed everything would
    /// satisfy every other assertion in this file. This is the `.210` control in unit form: a node
    /// that is answering, on a metric that is arriving, must not be touched.
    #[tokio::test]
    async fn a_metric_that_is_still_arriving_is_left_alone() {
        let node = NodeId::new();
        let mgr = manager_with_open_snmp_alert(node);
        let store = store_with(&[("icmp_rtt_ms", node), ("snmp_up", node)]);

        let acts = sweep_once(&mgr, &store, &FakeMeraki::none()).await;
        assert!(acts.is_empty(), "nothing is wrong here, got {acts:?}");
        assert_eq!(mgr.active_alerts().len(), 1);
    }

    /// The close: the node answers, the series does not.
    #[tokio::test]
    async fn an_alert_whose_series_stopped_is_closed() {
        let node = NodeId::new();
        let mgr = manager_with_open_snmp_alert(node);
        let store = store_with(&[("icmp_rtt_ms", node)]);

        let acts = sweep_once(&mgr, &store, &FakeMeraki::none()).await;
        assert_eq!(acts.len(), 1, "got {acts:?}");
        assert!(matches!(acts[0], NotifyAction::Resolve(_)));
        assert!(mgr.active_alerts().is_empty());

        // Idempotent — this runs every five minutes for the life of the process.
        assert!(sweep_once(&mgr, &store, &FakeMeraki::none())
            .await
            .is_empty());
    }

    /// 🚨 And the check must still be able to fire, which is the trap `resolve_orphans`' doc is
    /// about: closing an alert without dropping its dwell window leaves the state machine
    /// committed, so the next breach is not a transition and the check goes silent for good.
    #[tokio::test]
    async fn a_metric_that_comes_back_after_being_swept_alerts_again() {
        let node = NodeId::new();
        let mgr = manager_with_open_snmp_alert(node);
        let store = store_with(&[("icmp_rtt_ms", node)]);
        assert_eq!(sweep_once(&mgr, &store, &FakeMeraki::none()).await.len(), 1);

        let mut r = result(node, yagra_bus::CheckOutcome::Reachable, 1_000);
        r.samples = vec![yagra_bus::Sample::gauge("snmp_up", 0.0)];
        let acts = mgr.observe(&r);
        assert!(
            acts.iter().any(|a| matches!(a, NotifyAction::Fire(_))),
            "a check whose data came back must alert again, got {acts:?}"
        );
    }

    /// 🚨 **The most important test here.** An empty answer from the store is what a transport
    /// error and a JSON parse failure both look like. Without the canary this input closes the
    /// whole fleet and pages a recovery for each.
    #[tokio::test]
    async fn an_empty_liveness_answer_closes_nothing() {
        let node = NodeId::new();
        let mgr = manager_with_open_snmp_alert(node);
        let store = store_with(&[]);

        let acts = sweep_once(&mgr, &store, &FakeMeraki::none()).await;
        assert!(
            acts.is_empty(),
            "a store that answered nothing is not evidence, got {acts:?}"
        );
        assert_eq!(mgr.active_alerts().len(), 1);
    }

    /// A node that has genuinely gone quiet keeps its alerts, at any duration — the `STALE >= LIVE`
    /// proof in [`LIVE_WINDOW_SECS`], exercised.
    #[tokio::test]
    async fn a_node_that_is_no_longer_answering_keeps_its_alerts() {
        let quiet = NodeId::new();
        let loud = NodeId::new();
        let mgr = Arc::new(manager());
        mgr.set_config(cfg(vec![rule(quiet, "snmp_up"), rule(loud, "snmp_up")], {
            let mut m = meta_for(quiet);
            m.extend(meta_for(loud));
            m
        }));
        for n in [quiet, loud] {
            let mut r = result(n, yagra_bus::CheckOutcome::Reachable, 0);
            r.samples = vec![yagra_bus::Sample::gauge("snmp_up", 0.0)];
            let _ = mgr.observe(&r);
        }
        assert_eq!(mgr.active_alerts().len(), 2);

        // Only `loud` is answering, and neither has a fresh `snmp_up`.
        let store = store_with(&[("icmp_rtt_ms", loud)]);
        let acts = sweep_once(&mgr, &store, &FakeMeraki::none()).await;
        assert_eq!(acts.len(), 1, "only the answering node's alert closes");
        let open = mgr.active_alerts();
        assert_eq!(open.len(), 1);
        assert_eq!(
            open[0].node(),
            Some(quiet),
            "the quiet node keeps its alert"
        );
    }

    /// Meraki cadences reach seven days; the whole node kind is out of scope.
    #[tokio::test]
    async fn a_meraki_node_is_never_swept() {
        let node = NodeId::new();
        let mgr = manager_with_open_snmp_alert(node);
        let store = store_with(&[("icmp_rtt_ms", node)]);

        let acts = sweep_once(&mgr, &store, &FakeMeraki::holding(&[node.as_uuid()])).await;
        assert!(acts.is_empty(), "got {acts:?}");
        assert_eq!(mgr.active_alerts().len(), 1);
    }

    /// 🚨 …and failing to identify them skips the tick, rather than reading "none" as "no Meraki
    /// node exists". Unlike `scheduler/sweep.rs`, an empty answer here is the false close itself.
    #[tokio::test]
    async fn a_failure_to_identify_meraki_nodes_skips_the_freshness_half() {
        let node = NodeId::new();
        let mgr = manager_with_open_snmp_alert(node);
        let store = store_with(&[("icmp_rtt_ms", node)]);

        let acts = sweep_once(&mgr, &store, &FakeMeraki::broken()).await;
        assert!(acts.is_empty(), "got {acts:?}");
        assert_eq!(mgr.active_alerts().len(), 1);
    }

    /// 🚨 The closing half of the bookend. A store that answers the liveness canary and then fails
    /// every series query reads as "every series is missing" — which, with one canary, closes every
    /// answering node's alert on the strength of an outage.
    #[tokio::test]
    async fn a_store_that_goes_blind_after_the_canary_closes_nothing() {
        let node = NodeId::new();
        let mgr = manager_with_open_snmp_alert(node);
        // Answers the first canary, then nothing — including the second.
        let store = store_with(&[("icmp_rtt_ms", node)]).blind_after(1);

        let acts = sweep_once(&mgr, &store, &FakeMeraki::none()).await;
        assert!(
            acts.is_empty(),
            "an outage that began mid-tick is not evidence either, got {acts:?}"
        );
        assert_eq!(mgr.active_alerts().len(), 1);
    }

    /// The rule half still runs when the store cannot answer — it touches no store, so a store
    /// problem must not withhold it.
    #[tokio::test]
    async fn the_rule_half_runs_even_when_the_freshness_half_cannot() {
        let node = NodeId::new();
        let mgr = manager_with_open_snmp_alert(node);
        mgr.set_config(cfg(Vec::new(), meta_for(node)));
        let store = store_with(&[]);

        let acts = sweep_once(&mgr, &store, &FakeMeraki::broken()).await;
        assert_eq!(acts.len(), 1, "the deleted rule still closes, got {acts:?}");
        assert!(mgr.active_alerts().is_empty());
    }

    /// 🚨 A derived metric is never stored, so it must be asked about by its inputs. Keyed on its
    /// own name, every healthy derived alert in the fleet reads as stranded.
    #[test]
    fn a_derived_alert_answers_with_its_inputs_not_its_own_name() {
        let node = NodeId::new();
        let mgr = manager();
        mgr.set_config(cfg(
            vec![rule(node, "cisco_cemp_mem_used_pct")],
            meta_for(node),
        ));
        mgr.restore(vec![open_alert(
            node,
            "cisco_cemp_mem_used_pct",
            NodeState::Critical,
        )]);

        let cands = mgr.freshness_candidates();
        assert_eq!(cands.len(), 1);
        assert_eq!(
            cands[0].inputs,
            vec!["cisco_cemp_mem_used", "cisco_cemp_mem_free"],
            "a derived metric asks about the series that actually arrive"
        );
        assert!(
            !cands[0].inputs.contains(&cands[0].metric),
            "…and never about itself, which nothing ever writes"
        );
    }

    /// Every derived node metric names inputs that are not itself, and a `Complement` names one.
    /// Without this, an eleventh row could reintroduce the self-reference the test above forbids.
    #[test]
    fn every_derived_node_metric_has_inputs_of_its_own() {
        let mut checked = 0usize;
        for d in crate::derived::DERIVED_NODE_METRICS {
            let [x, y] = d.formula.inputs();
            assert_ne!(x, d.name, "{} would ask about itself", d.name);
            assert_ne!(y, d.name, "{} would ask about itself", d.name);
            checked += 1;
        }
        assert_eq!(
            checked,
            crate::derived::DERIVED_NODE_METRICS.len(),
            "the table did not load"
        );
        assert!(
            checked >= 10,
            "only {checked} derived metrics were inspected"
        );
    }

    /// The windows, against the cap they are derived from rather than against their own literals —
    /// so raising the poll-interval ceiling fails here instead of quietly shortening the window.
    #[test]
    fn the_windows_are_derived_from_the_slowest_configurable_interval() {
        let max = u64::from(crate::config::MAX_POLL_INTERVAL_SECS);
        assert!(
            STALE_WINDOW_SECS >= 6 * max,
            "the stale window must survive five missed polls at the slowest legal interval"
        );
        assert!(LIVE_WINDOW_SECS >= 2 * max);
    }

    /// 🚨 The invariant that makes total silence safe. A node silent for S passes the canary iff
    /// `S < LIVE` and is fresh iff `S < STALE`; closing needs `STALE <= S < LIVE`, which is empty
    /// whenever `STALE >= LIVE`.
    ///
    /// A `const` block rather than a runtime assertion, so the build fails rather than the suite —
    /// both numbers are compile-time constants and there is nothing here a test could observe that
    /// the compiler cannot. The test exists to carry the name and the reasoning.
    #[test]
    fn the_stale_window_is_never_shorter_than_the_live_window() {
        const {
            assert!(
                STALE_WINDOW_SECS >= LIVE_WINDOW_SECS,
                "a quiet node could then have an alert closed"
            );
        }
    }

    /// The scope must be chunked below what a GET query string will carry — roughly 37 bytes per
    /// id in the `node=~"…"` selector. Compile-time, for the reason above.
    #[test]
    fn the_scope_is_chunked_below_the_url_budget() {
        const {
            assert!(SCOPE_CHUNK > 0);
            assert!(
                SCOPE_CHUNK * 37 < 8_192,
                "a chunk of ids would exceed a conservative URL budget"
            );
        }
    }

    /// The tick is slower than its siblings on purpose; pinned so "make it match the others" is a
    /// deliberate edit rather than a tidy-up.
    #[test]
    fn the_tick_is_slower_than_the_config_refresh_it_reads_behind() {
        assert!(
            TICK >= Duration::from_secs(60),
            "a faster tick asks a question whose answer cannot have changed"
        );
    }

    /// The loop must run both halves, in that order, and drain them.
    ///
    /// 🚨 The floor comes first: everything below asks whether something is *present*, and over an
    /// empty slice that is a claim about nothing.
    #[test]
    fn both_sweeps_run_inside_the_tick_and_the_rule_sweep_runs_first() {
        let production = crate::module_source::code("src/alerts", "stale");
        let watch = production
            .split("async fn sweep_once")
            .nth(1)
            .expect("the sweep exists");
        let body = &watch[..watch.find("\nasync fn ").unwrap_or(watch.len())];
        assert!(
            body.contains("resolve_stale_alerts("),
            "the slice is not the sweep's body — it does not even resolve anything"
        );
        let rule_at = body
            .find("resolve_orphaned_collected_alerts()")
            .expect("without it, a deleted rule strands its alert for the life of the process");
        let fresh_at = body
            .find("freshness_candidates()")
            .expect("the freshness half exists");
        assert!(
            body.contains("LIVENESS_METRICS"),
            "the liveness canary is the safety property, not a convenience"
        );
        assert!(
            body.contains("meraki_subset("),
            "Meraki cadences reach seven days; they must be excluded"
        );
        assert!(
            rule_at < fresh_at,
            "a deleted rule must not cost two store queries"
        );

        let loop_body = production
            .split("async fn run_stale_check_watch")
            .nth(1)
            .expect("the watch loop exists");
        assert!(
            loop_body.contains("sink.dispatch("),
            "the loop must deliver what the sweep decided"
        );
    }
}
