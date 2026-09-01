// SPDX-License-Identifier: AGPL-3.0-only
//! The fakes a behaviour test builds a [`ReportRunner`] out of (ADR-112).
//!
//! Every seam in [`super::seams`] gets one, plus a [`FakeMetrics`] for the `MetricStore` that was
//! already a trait, plus a builder that assembles the four into a runner.
//!
//! 🚨 **Counting is half the point.** Several of this module's tests assert a store was **not**
//! asked — the unknown-metric section that returns before touching the TSDB, the inventory
//! section that only runs the freshness query when a node's state is missing, the run that was
//! never inserted because its definition did not exist. Those are cheap to break and, without a
//! counter, impossible to see: the report still renders.
//!
//! ⚠️ **A fake that answers "nothing" is indistinguishable from a healthy store with no data.**
//! Most of what follows is empty answers, so a test that only asserts a section is *empty* proves
//! very little; assert the populated side too. Every "0 calls" assertion here has a sibling that
//! asserts the same seam *was* called under the other input.
//!
//! ⚠️ [`Harness::build`] subscribes to the SSE stream **before** handing the runner back. The
//! channel is a `broadcast`, so a receiver taken after the first `progress` tick silently misses
//! it and the test fails for the wrong reason.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tokio::sync::broadcast;
use uuid::Uuid;
use yagra_alert::{Alert, Subject};
use yagra_bus::PollResult;
use yagra_common::{CheckId, Node, NodeId, NodeState, SeriesKey, Severity};

use crate::repo::GroupFilter;
use crate::store::{DeltaDirection, InterfaceTopMetric, MetricPoint, MetricStore, TopAgg};

use super::seams::{AlertFacts, FleetInventory, RunStore};
use super::*;

/// A uuid from a small integer, so a test can name its subjects readably.
pub(super) fn uid(n: u128) -> Uuid {
    Uuid::from_u128(n)
}

/// A node with the three fields the inventory section prints.
pub(super) fn node(n: u128, name: &str) -> Node {
    let mut node = Node::new(
        NodeId::from(uid(n)),
        name,
        std::net::IpAddr::from([10, 0, 0, u8::try_from(n).unwrap_or(1)]),
    );
    node.vendor = Some("Acme".to_owned());
    node.model = Some("R1".to_owned());
    node
}

/// One active alert. Only `severity` is read by a report.
pub(super) fn alert(sev: Severity) -> Alert {
    Alert {
        subject: Subject::Node(NodeId::from(uid(1))),
        check: CheckId(Uuid::new_v4()),
        severity: sev,
        state: NodeState::Critical,
        at_unix_ms: 0,
        root_cause: None,
        flapping: false,
        metric: "icmp_rtt_ms".to_owned(),
        breach: None,
        ifindex: None,
    }
}

/// A definition whose `spec` is whatever the test wants generation to read.
pub(super) fn definition(id: Uuid, spec: serde_json::Value) -> ReportDefinition {
    ReportDefinition {
        id,
        name: "Weekly".to_owned(),
        description: None,
        spec,
        updated_by: None,
        created_ms: 0,
        updated_ms: 0,
    }
}

// ── Counters ─────────────────────────────────────────────────────────────────────────────────

/// How many times each seam method was called. Shared by every fake so one test can read them all.
#[derive(Default)]
pub(super) struct Calls {
    pub(super) get_definition: AtomicUsize,
    pub(super) insert_run: AtomicUsize,
    pub(super) get_run: AtomicUsize,
    pub(super) set_run_progress: AtomicUsize,
    pub(super) finish_run: AtomicUsize,
    pub(super) fail_run: AtomicUsize,
    pub(super) list_nodes: AtomicUsize,
    pub(super) node_names: AtomicUsize,
    pub(super) state_history: AtomicUsize,
    pub(super) active_alerts: AtomicUsize,
    pub(super) node_states: AtomicUsize,
    pub(super) fires_by_severity: AtomicUsize,
    pub(super) top_nodes_by_fires: AtomicUsize,
    pub(super) top_nodes: AtomicUsize,
    pub(super) throughput_range: AtomicUsize,
    pub(super) fresh_node_ids: AtomicUsize,
}

/// Read a counter.
pub(super) fn n(c: &AtomicUsize) -> usize {
    c.load(Ordering::Relaxed)
}

fn bump(c: &AtomicUsize) {
    c.fetch_add(1, Ordering::Relaxed);
}

// ── The run store ────────────────────────────────────────────────────────────────────────────

#[derive(Default)]
pub(super) struct FakeRuns {
    calls: Arc<Calls>,
    definition: Option<ReportDefinition>,
    /// `finish_run` returns an error, so the persist-failure path runs.
    finish_fails: bool,
    /// Every run `insert_run` produced, so a test can read the window it was given.
    pub(super) inserted: Mutex<Vec<ReportRun>>,
    /// Every percentage handed to `set_run_progress`, in order.
    pub(super) progress: Mutex<Vec<i32>>,
    /// The `(result, html)` a successful `finish_run` was given.
    pub(super) finished: Mutex<Option<(serde_json::Value, String)>>,
    /// Every reason `fail_run` was given.
    pub(super) failed: Mutex<Vec<String>>,
}

#[async_trait]
impl RunStore for FakeRuns {
    async fn get_definition(&self, _id: Uuid) -> anyhow::Result<Option<ReportDefinition>> {
        bump(&self.calls.get_definition);
        Ok(self.definition.clone())
    }

    async fn insert_run(
        &self,
        definition_id: Option<Uuid>,
        name: &str,
        trigger: ReportRunTrigger,
        from_s: i64,
        to_s: i64,
        section_count: i32,
        _spec_snapshot: &serde_json::Value,
        created_by: Option<&str>,
    ) -> anyhow::Result<ReportRun> {
        bump(&self.calls.insert_run);
        let run = ReportRun {
            id: uid(900),
            definition_id,
            name: name.to_owned(),
            trigger,
            state: ReportRunState::Running,
            pct: 0,
            error: None,
            range_from_ms: Some(from_s * 1000),
            range_to_ms: Some(to_s * 1000),
            section_count,
            created_by: created_by.map(ToOwned::to_owned),
            created_ms: 0,
            started_ms: None,
            finished_ms: None,
        };
        self.inserted.lock().expect("poisoned").push(run.clone());
        Ok(run)
    }

    async fn get_run(&self, _id: Uuid) -> anyhow::Result<Option<ReportRun>> {
        bump(&self.calls.get_run);
        Ok(self.inserted.lock().expect("poisoned").last().cloned())
    }

    async fn set_run_progress(&self, _id: Uuid, pct: i32) -> anyhow::Result<()> {
        bump(&self.calls.set_run_progress);
        self.progress.lock().expect("poisoned").push(pct);
        Ok(())
    }

    async fn finish_run(
        &self,
        _id: Uuid,
        result: &serde_json::Value,
        html: &str,
    ) -> anyhow::Result<()> {
        bump(&self.calls.finish_run);
        if self.finish_fails {
            anyhow::bail!("persist refused");
        }
        *self.finished.lock().expect("poisoned") = Some((result.clone(), html.to_owned()));
        Ok(())
    }

    async fn fail_run(&self, _id: Uuid, error: &str) -> anyhow::Result<()> {
        bump(&self.calls.fail_run);
        self.failed.lock().expect("poisoned").push(error.to_owned());
        Ok(())
    }
}

// ── The inventory ────────────────────────────────────────────────────────────────────────────

#[derive(Default)]
pub(super) struct FakeInventory {
    calls: Arc<Calls>,
    nodes: Vec<Node>,
    names: HashMap<Uuid, String>,
    history: Vec<(i64, String, i64)>,
    /// Every read answers `Err`, so a section's `unwrap_or_default` path runs.
    fails: bool,
    /// Every scope filter `node_names` was handed. A report must never narrow one.
    pub(super) scopes: Mutex<Vec<Option<Vec<Uuid>>>>,
}

#[async_trait]
impl FleetInventory for FakeInventory {
    async fn list_nodes(&self) -> anyhow::Result<Vec<Node>> {
        bump(&self.calls.list_nodes);
        if self.fails {
            anyhow::bail!("inventory unavailable");
        }
        Ok(self.nodes.clone())
    }

    async fn node_names(
        &self,
        groups: GroupFilter<'_>,
        ids: &[Uuid],
    ) -> anyhow::Result<HashMap<Uuid, String>> {
        bump(&self.calls.node_names);
        self.scopes
            .lock()
            .expect("poisoned")
            .push(groups.map(<[Uuid]>::to_vec));
        if self.fails {
            anyhow::bail!("inventory unavailable");
        }
        Ok(ids
            .iter()
            .filter_map(|id| self.names.get(id).map(|nm| (*id, nm.clone())))
            .collect())
    }

    async fn state_history(
        &self,
        _from_s: i64,
        _to_s: i64,
    ) -> anyhow::Result<Vec<(i64, String, i64)>> {
        bump(&self.calls.state_history);
        if self.fails {
            anyhow::bail!("snapshots unavailable");
        }
        Ok(self.history.clone())
    }
}

// ── Alerting ─────────────────────────────────────────────────────────────────────────────────

#[derive(Default)]
pub(super) struct FakeAlerts {
    calls: Arc<Calls>,
    active: Vec<Alert>,
    states: HashMap<NodeId, NodeState>,
    fires: Vec<(String, i64)>,
    top: Vec<(Uuid, i64)>,
    fails: bool,
    /// Every `since_ms` this fake was handed, by **either** aggregate — so a test can assert the
    /// two halves of the alert story are counted from the same instant (ADR-112 Inc.2).
    pub(super) since: Mutex<Vec<i64>>,
}

#[async_trait]
impl AlertFacts for FakeAlerts {
    fn active_alerts(&self) -> Vec<Alert> {
        bump(&self.calls.active_alerts);
        self.active.clone()
    }

    fn node_states(&self) -> HashMap<NodeId, NodeState> {
        bump(&self.calls.node_states);
        self.states.clone()
    }

    async fn fires_by_severity(&self, since_ms: i64) -> anyhow::Result<Vec<(String, i64)>> {
        bump(&self.calls.fires_by_severity);
        self.since.lock().expect("poisoned").push(since_ms);
        if self.fails {
            anyhow::bail!("history unavailable");
        }
        Ok(self.fires.clone())
    }

    async fn top_nodes_by_fires(
        &self,
        since_ms: i64,
        limit: i64,
    ) -> anyhow::Result<Vec<(Uuid, i64)>> {
        bump(&self.calls.top_nodes_by_fires);
        self.since.lock().expect("poisoned").push(since_ms);
        if self.fails {
            anyhow::bail!("history unavailable");
        }
        Ok(self
            .top
            .iter()
            .take(usize::try_from(limit).unwrap_or(0))
            .copied()
            .collect())
    }
}

// ── The TSDB ─────────────────────────────────────────────────────────────────────────────────

/// The three methods a report reads, counted; everything else on the trait answers "nothing".
#[derive(Default)]
pub(super) struct FakeMetrics {
    calls: Arc<Calls>,
    top: Vec<(Uuid, f64)>,
    throughput: (Vec<MetricPoint>, Vec<MetricPoint>),
    fresh: Vec<Uuid>,
    /// Every aggregate `top_nodes` was asked for. The setting is parsed with a `_ =>` fallback,
    /// so a typo silently becomes the hourly max and nothing else can see that happen.
    pub(super) aggs: Mutex<Vec<TopAgg>>,
}

#[async_trait]
impl MetricStore for FakeMetrics {
    async fn top_nodes(&self, _metric: &str, agg: TopAgg, limit: usize) -> Vec<(Uuid, f64)> {
        bump(&self.calls.top_nodes);
        self.aggs.lock().expect("poisoned").push(agg);
        self.top.iter().take(limit).copied().collect()
    }

    async fn throughput_range(
        &self,
        _from_s: i64,
        _to_s: i64,
        _step_s: u64,
    ) -> (Vec<MetricPoint>, Vec<MetricPoint>) {
        bump(&self.calls.throughput_range);
        self.throughput.clone()
    }

    async fn fresh_node_ids(&self, _metrics: &[&str], _within_secs: u64) -> Vec<Uuid> {
        bump(&self.calls.fresh_node_ids);
        self.fresh.clone()
    }

    // Everything below is a required method a report never calls.
    async fn write(&self, _result: &PollResult) {}
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
}

// ── The harness ──────────────────────────────────────────────────────────────────────────────

pub(super) struct Harness {
    pub(super) runner: Arc<ReportRunner>,
    pub(super) metrics: Arc<FakeMetrics>,
    pub(super) calls: Arc<Calls>,
    pub(super) runs: Arc<FakeRuns>,
    pub(super) inventory: Arc<FakeInventory>,
    pub(super) alerts: Arc<FakeAlerts>,
    /// Subscribed in [`HarnessBuilder::build`], before anything can broadcast.
    pub(super) events: broadcast::Receiver<String>,
}

#[derive(Default)]
pub(super) struct HarnessBuilder {
    definition: Option<ReportDefinition>,
    finish_fails: bool,
    started_run: bool,
    nodes: Vec<Node>,
    names: HashMap<Uuid, String>,
    state_history: Vec<(i64, String, i64)>,
    inventory_fails: bool,
    active: Vec<Alert>,
    states: HashMap<NodeId, NodeState>,
    fires: Vec<(String, i64)>,
    top_fires: Vec<(Uuid, i64)>,
    alerts_fail: bool,
    top_nodes: Vec<(Uuid, f64)>,
    throughput: (Vec<MetricPoint>, Vec<MetricPoint>),
    fresh: Vec<Uuid>,
}

impl HarnessBuilder {
    pub(super) fn definition(mut self, def: ReportDefinition) -> Self {
        self.definition = Some(def);
        self
    }
    pub(super) const fn finish_fails(mut self) -> Self {
        self.finish_fails = true;
        self
    }
    /// Seed the row `insert_run` would have written.
    ///
    /// `generate` never runs before `insert_run` in production, so a test that calls it directly
    /// has to stand the row up first — otherwise `get_run` answers `None` and the progress
    /// broadcast is silently skipped, which reads as "the stream is broken".
    pub(super) const fn started_run(mut self) -> Self {
        self.started_run = true;
        self
    }
    pub(super) fn nodes(mut self, nodes: Vec<Node>) -> Self {
        self.nodes = nodes;
        self
    }
    pub(super) fn names(mut self, names: &[(Uuid, &str)]) -> Self {
        self.names = names
            .iter()
            .map(|(id, nm)| (*id, (*nm).to_owned()))
            .collect();
        self
    }
    pub(super) fn state_history(mut self, rows: Vec<(i64, String, i64)>) -> Self {
        self.state_history = rows;
        self
    }
    pub(super) const fn inventory_fails(mut self) -> Self {
        self.inventory_fails = true;
        self
    }
    pub(super) fn active(mut self, alerts: Vec<Alert>) -> Self {
        self.active = alerts;
        self
    }
    pub(super) fn states(mut self, states: &[(u128, NodeState)]) -> Self {
        self.states = states
            .iter()
            .map(|(n, s)| (NodeId::from(uid(*n)), *s))
            .collect();
        self
    }
    pub(super) fn fires(mut self, counts: Vec<(String, i64)>) -> Self {
        self.fires = counts;
        self
    }
    pub(super) fn top_fires(mut self, pairs: Vec<(Uuid, i64)>) -> Self {
        self.top_fires = pairs;
        self
    }
    pub(super) const fn alerts_fail(mut self) -> Self {
        self.alerts_fail = true;
        self
    }
    pub(super) fn top_nodes(mut self, pairs: Vec<(Uuid, f64)>) -> Self {
        self.top_nodes = pairs;
        self
    }
    pub(super) fn throughput(mut self, into: Vec<MetricPoint>, out: Vec<MetricPoint>) -> Self {
        self.throughput = (into, out);
        self
    }
    pub(super) fn fresh(mut self, ids: Vec<Uuid>) -> Self {
        self.fresh = ids;
        self
    }

    pub(super) fn build(self) -> Harness {
        let calls = Arc::new(Calls::default());
        let runs = Arc::new(FakeRuns {
            calls: calls.clone(),
            definition: self.definition,
            finish_fails: self.finish_fails,
            ..FakeRuns::default()
        });
        if self.started_run {
            runs.inserted.lock().expect("poisoned").push(ReportRun {
                id: uid(900),
                definition_id: None,
                name: "R".to_owned(),
                trigger: ReportRunTrigger::Manual,
                state: ReportRunState::Running,
                pct: 0,
                error: None,
                range_from_ms: Some(0),
                range_to_ms: Some(60_000),
                section_count: 0,
                created_by: None,
                created_ms: 0,
                started_ms: None,
                finished_ms: None,
            });
        }
        let inventory = Arc::new(FakeInventory {
            calls: calls.clone(),
            nodes: self.nodes,
            names: self.names,
            history: self.state_history,
            fails: self.inventory_fails,
            ..FakeInventory::default()
        });
        let alerts = Arc::new(FakeAlerts {
            calls: calls.clone(),
            active: self.active,
            states: self.states,
            fires: self.fires,
            top: self.top_fires,
            fails: self.alerts_fail,
            ..FakeAlerts::default()
        });
        let metrics = Arc::new(FakeMetrics {
            calls: calls.clone(),
            top: self.top_nodes,
            throughput: self.throughput,
            fresh: self.fresh,
            ..FakeMetrics::default()
        });
        let runner = Arc::new(ReportRunner::from_seams(
            runs.clone(),
            metrics.clone(),
            inventory.clone(),
            alerts.clone(),
        ));
        // Before anything can publish — see the module doc.
        let events = runner.subscribe();
        Harness {
            runner,
            metrics,
            calls,
            runs,
            inventory,
            alerts,
            events,
        }
    }
}

pub(super) fn harness() -> HarnessBuilder {
    HarnessBuilder::default()
}
