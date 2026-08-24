// SPDX-License-Identifier: AGPL-3.0-only
//! The fakes a behaviour test builds an [`Engine`] out of (ADR-098).
//!
//! Every seam in [`super::seams`] gets one, plus a builder that assembles them. Two of the six
//! stores an analysis reads did not need writing at all — [`crate::flowstore::InMemoryFlowStore`]
//! and [`crate::logstore::InMemoryLogStore`] are real in-memory implementations that already
//! existed for other modules' tests, and the flow analyses run against the first of them end to
//! end.
//!
//! 🚨 **[`SeededMetrics`] keeps `range` and `aggregate_range` in separate maps, and that is not
//! tidiness.** Three of the four metric analyses read a series through `gauge_range`, which is
//! `aggregate_range`; `run_flap` reads `range` directly, because the *gaps* in an otherwise regular
//! series are what a flap is, and an aggregate fills them in. Nothing else in the repository can
//! tell those two calls apart, so a fake that served both from one map would let that distinction
//! be "tidied" away with every test still green.
//!
//! ⚠️ **A fake that answers "nothing" for a method an analysis has started to call looks exactly
//! like a healthy store with no data.** Most of [`SeededMetrics`] is empty answers, so a test that
//! only asserts a finding is absent proves very little here; assert a finding is *present* too.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use uuid::Uuid;
use yagra_bus::PollResult;
use yagra_common::{Node, NodeId, SeriesKey};
use yagra_topology::Topology;

use crate::events::{
    EventAuthSource, EventBucketCount, EventFilter, EventFlapStat, EventRow, EventSeverityCount,
    EventSignatureCount,
};
use crate::flowstore::{FlowStore, InMemoryFlowStore};
use crate::ipasn;
use crate::store::{
    DeltaDirection, InterfaceLive, InterfaceTopMetric, MetricPoint, MetricStore, TopAgg,
};

use super::seams::{AnalysisEvents, DerivedGraph, FleetInventory, JobProgress};
use super::{AnalysisTool, Engine, JobParams, ScopeKind};

/// A node id from a small integer, so a test can name its subjects readably.
pub(super) fn nid(n: u128) -> Uuid {
    Uuid::from_u128(n)
}

/// A series of `count` points ending at `to_s`, `step_s` apart, all equal to `v`.
pub(super) fn flat(to_s: i64, step_s: i64, count: usize, v: f64) -> Vec<MetricPoint> {
    (0..count)
        .map(|i| MetricPoint {
            t: to_s - step_s * (count as i64 - 1 - i as i64),
            v,
        })
        .collect()
}

/// Job parameters for `tool` over a node scope, with the windows a test usually wants.
pub(super) fn params(tool: AnalysisTool) -> JobParams {
    JobParams {
        tool,
        scope_kind: ScopeKind::Node,
        scope_id: None,
        scope_label: String::new(),
        window_secs: 3600,
        baseline_secs: 86_400,
        sensitivity: 3.0,
        depth: "standard".to_owned(),
        family: "all".to_owned(),
        notify: false,
    }
}

// ── MetricStore ──────────────────────────────────────────────────────────────────────────────

/// A [`MetricStore`] a test seeds by hand. Keyed on `(node, metric)`; the interface dimension is
/// not modelled because no analysis reads a per-interface series.
#[derive(Default)]
pub(super) struct SeededMetrics {
    raw: Mutex<HashMap<(Uuid, String), Vec<MetricPoint>>>,
    agg: Mutex<HashMap<(Uuid, String), Vec<MetricPoint>>>,
    iface: Mutex<HashMap<Uuid, HashMap<i32, InterfaceLive>>>,
}

impl SeededMetrics {
    pub(super) fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Seed the **raw** series only — what `range` serves and `aggregate_range` does not.
    pub(super) fn raw(&self, node: Uuid, metric: &str, points: Vec<MetricPoint>) {
        self.raw
            .lock()
            .expect("raw poisoned")
            .insert((node, metric.to_owned()), points);
    }

    /// Seed the **aggregated** series only — what `gauge_range` serves.
    pub(super) fn aggregated(&self, node: Uuid, metric: &str, points: Vec<MetricPoint>) {
        self.agg
            .lock()
            .expect("agg poisoned")
            .insert((node, metric.to_owned()), points);
    }

    /// Seed both, for a metric whose read path a test is not trying to distinguish.
    pub(super) fn series(&self, node: Uuid, metric: &str, points: Vec<MetricPoint>) {
        self.raw(node, metric, points.clone());
        self.aggregated(node, metric, points);
    }

    pub(super) fn interfaces(&self, node: Uuid, live: HashMap<i32, InterfaceLive>) {
        self.iface
            .lock()
            .expect("iface poisoned")
            .insert(node, live);
    }

    fn get(
        map: &Mutex<HashMap<(Uuid, String), Vec<MetricPoint>>>,
        k: &SeriesKey,
    ) -> Vec<MetricPoint> {
        map.lock()
            .expect("poisoned")
            .get(&(k.node.as_uuid(), k.metric.clone()))
            .cloned()
            .unwrap_or_default()
    }
}

#[async_trait]
impl MetricStore for SeededMetrics {
    async fn write(&self, _result: &PollResult) {}

    async fn latest(&self, key: &SeriesKey) -> Option<f64> {
        Self::get(&self.raw, key).last().map(|p| p.v)
    }

    async fn range(
        &self,
        key: &SeriesKey,
        _from_s: i64,
        _to_s: i64,
        _step_s: u64,
    ) -> Vec<MetricPoint> {
        Self::get(&self.raw, key)
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

    async fn aggregate_latest(&self, key: &SeriesKey) -> Option<f64> {
        Self::get(&self.agg, key).last().map(|p| p.v)
    }

    async fn aggregate_range(
        &self,
        key: &SeriesKey,
        _from_s: i64,
        _to_s: i64,
        _step_s: u64,
    ) -> Vec<MetricPoint> {
        Self::get(&self.agg, key)
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
        Some(Vec::new())
    }

    async fn fresh_node_ids(&self, _metrics: &[&str], _within_secs: u64) -> Vec<Uuid> {
        Vec::new()
    }

    async fn interface_delta(
        &self,
        _direction: DeltaDirection,
        _window_secs: u64,
        _limit: usize,
    ) -> Vec<(Uuid, i32, f64)> {
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

    /// Derived from what was seeded, in either map, so seeding a series is enough to make it
    /// discoverable — one fewer thing for a test to remember, and the union is what a real store
    /// would report.
    async fn node_metric_names(&self, node: Uuid, _within_secs: u64) -> Vec<String> {
        let mut names: Vec<String> = self
            .raw
            .lock()
            .expect("raw poisoned")
            .keys()
            .chain(self.agg.lock().expect("agg poisoned").keys())
            .filter(|(n, _)| *n == node)
            .map(|(_, m)| m.clone())
            .collect();
        names.sort();
        names.dedup();
        names
    }

    async fn node_interface_live(
        &self,
        node: Uuid,
        _lookback_s: u64,
    ) -> HashMap<i32, InterfaceLive> {
        self.iface
            .lock()
            .expect("iface poisoned")
            .get(&node)
            .cloned()
            .unwrap_or_default()
    }
}

// ── Progress ─────────────────────────────────────────────────────────────────────────────────

/// Records every progress tick, so a test can assert the sequence a job reports.
#[derive(Default)]
pub(super) struct RecordingProgress {
    ticks: Mutex<Vec<(i32, String)>>,
}

impl RecordingProgress {
    pub(super) fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub(super) fn ticks(&self) -> Vec<(i32, String)> {
        self.ticks.lock().expect("ticks poisoned").clone()
    }
}

#[async_trait]
impl JobProgress for RecordingProgress {
    async fn tick(&self, _id: Uuid, pct: i32, phase: &str) {
        self.ticks
            .lock()
            .expect("ticks poisoned")
            .push((pct, phase.to_owned()));
    }
}

// ── Passive events ───────────────────────────────────────────────────────────────────────────

/// Canned answers for each [`AnalysisEvents`] method, plus a switch that makes every one fail.
///
/// ⚠️ This fake stands in for the *router*, so it cannot show which store a routed call would have
/// gone to — that branch needs an `EventRepo`, which needs a database. What it does buy is that the
/// analyses are exercised at all, and (by construction) that none of them can reach past it.
#[derive(Default)]
pub(super) struct FakeEvents {
    pub(super) buckets: Mutex<Vec<EventBucketCount>>,
    /// ⚠️ A **script**, not one answer: `run_severity_shift` asks twice — once for the baseline
    /// window and once for the recent one — and a single canned reply makes the two identical, which
    /// is precisely the case that has no shift to report. Each call takes the next entry.
    pub(super) severities: Mutex<Vec<Vec<EventSeverityCount>>>,
    pub(super) signatures: Mutex<Vec<EventSignatureCount>>,
    pub(super) auth: Mutex<Vec<EventAuthSource>>,
    pub(super) flaps: Mutex<Vec<EventFlapStat>>,
    pub(super) recent: Mutex<Vec<EventRow>>,
    /// Every method returns `Err` while this is set — for the paths that must degrade rather than
    /// fail the whole job.
    pub(super) failing: Mutex<bool>,
}

impl FakeEvents {
    pub(super) fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn guard(&self) -> anyhow::Result<()> {
        if *self.failing.lock().expect("failing poisoned") {
            anyhow::bail!("event store unavailable (fake)");
        }
        Ok(())
    }
}

#[async_trait]
impl AnalysisEvents for FakeEvents {
    async fn counts_by_bucket(
        &self,
        _filter: &EventFilter,
        _bucket_secs: i64,
    ) -> anyhow::Result<Vec<EventBucketCount>> {
        self.guard()?;
        Ok(self.buckets.lock().expect("poisoned").clone())
    }

    async fn severity_counts(
        &self,
        _filter: &EventFilter,
    ) -> anyhow::Result<Vec<EventSeverityCount>> {
        self.guard()?;
        let mut script = self.severities.lock().expect("poisoned");
        if script.is_empty() {
            return Ok(Vec::new());
        }
        Ok(script.remove(0))
    }

    async fn unmatched_signatures(
        &self,
        _filter: &EventFilter,
        _limit: i64,
    ) -> anyhow::Result<Vec<EventSignatureCount>> {
        self.guard()?;
        Ok(self.signatures.lock().expect("poisoned").clone())
    }

    async fn auth_sources(
        &self,
        _filter: &EventFilter,
        _limit: i64,
    ) -> anyhow::Result<Vec<EventAuthSource>> {
        self.guard()?;
        Ok(self.auth.lock().expect("poisoned").clone())
    }

    async fn flap_stats(&self, _from_ms: i64, _to_ms: i64) -> anyhow::Result<Vec<EventFlapStat>> {
        self.guard()?;
        Ok(self.flaps.lock().expect("poisoned").clone())
    }

    async fn recent_events(
        &self,
        _filter: &EventFilter,
        _limit: i64,
    ) -> anyhow::Result<Vec<EventRow>> {
        self.guard()?;
        Ok(self.recent.lock().expect("poisoned").clone())
    }
}

// ── Inventory ────────────────────────────────────────────────────────────────────────────────

/// A fixed fleet. `node_names` is derived from it, so a test names a node once.
#[derive(Default)]
pub(super) struct StaticInventory {
    nodes: Mutex<Vec<Node>>,
    edges: Mutex<Vec<(Uuid, Option<Uuid>)>>,
    members: Mutex<HashMap<Uuid, Vec<Uuid>>>,
    failing: Mutex<bool>,
}

impl StaticInventory {
    pub(super) fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Add a node with an id derived from `n` and the given name.
    pub(super) fn node(&self, n: u128, name: &str) -> Uuid {
        let id = nid(n);
        let node = Node::new(
            NodeId::from(id),
            name,
            format!("10.0.0.{}", (n % 250) + 1).parse().expect("addr"),
        );
        self.nodes.lock().expect("nodes poisoned").push(node);
        id
    }

    pub(super) fn group(&self, group: Uuid, parent: Option<Uuid>, members: Vec<Uuid>) {
        self.edges
            .lock()
            .expect("edges poisoned")
            .push((group, parent));
        self.members
            .lock()
            .expect("members poisoned")
            .insert(group, members);
    }

    /// Make every read fail — the inventory being unreachable is a path some analyses must
    /// survive rather than abort on.
    pub(super) fn fail(&self) {
        *self.failing.lock().expect("failing poisoned") = true;
    }

    fn guard(&self) -> anyhow::Result<()> {
        if *self.failing.lock().expect("failing poisoned") {
            anyhow::bail!("inventory unavailable (fake)");
        }
        Ok(())
    }
}

#[async_trait]
impl FleetInventory for StaticInventory {
    async fn list_nodes(&self) -> anyhow::Result<Vec<Node>> {
        self.guard()?;
        Ok(self.nodes.lock().expect("nodes poisoned").clone())
    }

    async fn nodes_in_groups(&self, group_ids: &[Uuid]) -> anyhow::Result<Vec<Uuid>> {
        self.guard()?;
        let m = self.members.lock().expect("members poisoned");
        Ok(group_ids
            .iter()
            .filter_map(|g| m.get(g))
            .flatten()
            .copied()
            .collect())
    }

    async fn node_names(&self, ids: &[Uuid]) -> anyhow::Result<HashMap<Uuid, String>> {
        self.guard()?;
        let want: std::collections::HashSet<Uuid> = ids.iter().copied().collect();
        Ok(self
            .nodes
            .lock()
            .expect("nodes poisoned")
            .iter()
            .filter(|n| want.contains(&n.id.as_uuid()))
            .map(|n| (n.id.as_uuid(), n.name.clone()))
            .collect())
    }

    async fn group_edges(&self) -> anyhow::Result<Vec<(Uuid, Option<Uuid>)>> {
        self.guard()?;
        Ok(self.edges.lock().expect("edges poisoned").clone())
    }
}

// ── Connectivity graph ───────────────────────────────────────────────────────────────────────

/// A fixed derived graph. Empty unless a test seeds one, which is the shape a deployment in the
/// default `manual` topology mode has.
#[derive(Default)]
pub(super) struct StaticGraph {
    topology: Mutex<Topology>,
}

impl StaticGraph {
    pub(super) fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub(super) fn set(&self, topology: Topology) {
        *self.topology.lock().expect("topology poisoned") = topology;
    }
}

#[async_trait]
impl DerivedGraph for StaticGraph {
    async fn derived(&self, _nodes: &[Node]) -> Topology {
        self.topology.lock().expect("topology poisoned").clone()
    }
}

// ── The harness ──────────────────────────────────────────────────────────────────────────────

/// The fakes, kept reachable after the [`Engine`] is built so a test can seed them and then read
/// back what the run did to them.
pub(super) struct Harness {
    pub(super) metrics: Arc<SeededMetrics>,
    pub(super) flows: Option<Arc<InMemoryFlowStore>>,
    pub(super) events: Arc<FakeEvents>,
    pub(super) inventory: Arc<StaticInventory>,
    pub(super) graph: Arc<StaticGraph>,
    pub(super) progress: Arc<RecordingProgress>,
}

impl Harness {
    /// A deployment with **no flow tier** — the default, because ten of the fifteen analyses do not
    /// need one and the five that do must say so rather than fail.
    pub(super) fn new() -> Self {
        Self {
            metrics: SeededMetrics::new(),
            flows: None,
            events: FakeEvents::new(),
            inventory: StaticInventory::new(),
            graph: StaticGraph::new(),
            progress: RecordingProgress::new(),
        }
    }

    /// Attach a real in-memory ClickHouse stand-in (`crate::flowstore::InMemoryFlowStore`), which
    /// models the query contract rather than canning answers.
    pub(super) fn with_flows(mut self) -> Self {
        self.flows = Some(Arc::new(InMemoryFlowStore::default()));
        self
    }

    /// The flow store, for seeding. Panics when the harness was built without one.
    pub(super) fn flow_store(&self) -> &Arc<InMemoryFlowStore> {
        self.flows
            .as_ref()
            .expect("harness built without a flow tier")
    }

    pub(super) fn engine(&self) -> Engine {
        Engine {
            store: self.metrics.clone(),
            flows: self.flows.clone().map(|f| f as Arc<dyn FlowStore>),
            ipasn: ipasn::empty_handle(),
            events: self.events.clone(),
            inventory: self.inventory.clone(),
            graph: self.graph.clone(),
            progress: self.progress.clone(),
        }
    }
}
