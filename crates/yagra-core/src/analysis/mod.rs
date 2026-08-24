// SPDX-License-Identifier: AGPL-3.0-only
//! Troubleshoot deep-diagnostic jobs (ADR-022).
//!
//! These are heavy, long-running analyses that read long metric histories and run statistical
//! models — anomaly detection, event correlation, capacity forecast, flap analysis. Unlike
//! discovery (device I/O ⇒ poller, ADR-003), an analysis is a **TSDB-read computation**: it
//! reads VictoriaMetrics through the [`MetricStore`] seam and never touches a device, so it runs
//! as a background `tokio` task inside core. A future scale-out can move it to a dedicated
//! worker behind the same seam without changing the API.
//!
//! Lifecycle mirrors the discovery runner + the alert engine's broadcast: [`AnalysisRunner::create`]
//! inserts a job, spawns the task, and returns immediately; the task updates progress (persisted +
//! broadcast over SSE) and writes findings when done. Job metadata + findings are metadata, so
//! they live in PostgreSQL ([`AnalysisRepo`], ADR-004).
//!
//! **Two types, split by what each needs in order to run** (ADR-098). [`AnalysisRunner`] owns a
//! job's *life* — admission, insert, spawn, cancel, persist, finalize — and is the only thing that
//! holds [`AnalysisRepo`]. [`Engine`] owns what an analysis *reads*, and every one of its seven
//! fields is a trait object or a handle, so it can be built in a test. That is the whole reason the
//! split exists: the fifteen `run_*` bodies were untestable not because of what they do but because
//! of the value they were methods on. The seams themselves are in [`seams`].

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row};
use tokio::sync::{broadcast, Semaphore};
use uuid::Uuid;
use yagra_common::{NodeId, SeriesKey};

use crate::events::{
    EventAction, EventAuthSource, EventBucketCount, EventFilter, EventRepo, EventRow,
    EventSeverityCount, EventSignatureCount,
};
use crate::flowstore::{AsDir, FlowQuery, FlowSeriesQuery, FlowStore};
use crate::groups::{group_subtree, GroupRepo};
use crate::ipasn::IpAsnHandle;
use crate::logstore::LogStore;
use crate::repo::NodeRepo;
use crate::store::{MetricPoint, MetricStore};
use yagra_topology::Topology;

/// Broadcast buffer for the job-status SSE stream (matches the alert engine's sizing intent).
const EVENT_BUFFER: usize = 256;
/// Target sample count when reading a series — bounds the step so a long window stays cheap.
const MAX_POINTS: i64 = 300;
/// Minimum samples a series needs before an analysis will draw a conclusion from it.
const MIN_POINTS: usize = 12;
/// Cap on findings returned per job (the report lists the most significant first).
const MAX_FINDINGS: usize = 60;
/// Default cap on concurrently-running analysis jobs (env `YAGRA_ANALYSIS_MAX_CONCURRENT`). This is
/// the abuse control that lets an analysis be treated as a **read** (ADR-028 Increment 2): a
/// View-scoped MCP client may launch one, but a fleet-wide TSDB scan is heavy, so bound how many run
/// at once regardless of who asked — the concurrency cap, not a role, is the guard rail.
const DEFAULT_MAX_CONCURRENT: usize = 4;
/// Default cap on new jobs admitted per [`RATE_WINDOW`] (env `YAGRA_ANALYSIS_RATE_PER_MIN`) — bounds
/// rapid-fire creation even when each job finishes quickly.
const DEFAULT_RATE_PER_MIN: usize = 30;
/// The sliding window the per-minute creation rate limit is measured over.
const RATE_WINDOW: Duration = Duration::from_secs(60);

// Admission control (the concurrency cap's companion) lives in [`crate::ratelimit`], shared with the
// AI-assisted RCA endpoint (ADR-029) so there is one definition of "an expensive read is bounded by
// a cap, not by a role".
use crate::ratelimit::{charge_window, env_cap};

// The one accessor every source-text check reads this module through, the checks themselves
// (ADR-089), and the fakes the behaviour tests build an [`Engine`] from (ADR-098). All three are
// `#[cfg(test)]`, which is how `source`'s exclusion derives them — a guard file inside the text it
// greps matches its own needles.
#[cfg(test)]
mod guards;
#[cfg(test)]
mod source;

// ADR-089: what was one 4,401-line file is now this directory. `mod.rs` keeps the runner's
// lifecycle — admission, scoping, progress, and the one exhaustive `match` that dispatches to an
// analysis — and re-exports the vocabulary so `crate::analysis::X` still resolves for the 30-odd
// sites outside this module.
mod events;
mod flow;
mod incident;
mod metric;
mod repo;
mod seams;
mod stats;
mod types;

// Re-exported at the visibility each half already had, so `crate::analysis::X` still resolves for
// the sites outside this module and `use super::*` in a sibling still sees everything the single
// file used to put in one scope.
pub use repo::*;
pub use types::*;
// `stats` is the `self`-free half: everything in it is `pub(super)` because only this module's own
// files score anything, so a `pub use` glob would re-export nothing and warn. The one exception is
// the value type the AI-assisted RCA endpoint reads (ADR-029).
use seams::{
    AnalysisEvents, DerivedGraph, FleetInventory, JobProgress, ProjectedGraph, RepoInventory,
    RepoProgress, RoutedEvents,
};
pub(crate) use stats::IncidentSignal;
use stats::*;

/// Serialize a job row and push it onto the SSE stream.
///
/// A free function rather than a method because it has **two** owners since ADR-098: the runner
/// broadcasts on create and on the terminal state, and [`RepoProgress`] broadcasts every progress
/// tick. Duplicating six lines into the seam would be a second place the frame's shape is decided.
pub(super) fn broadcast_job(tx: &broadcast::Sender<JobFrame>, job: &AnalysisJob) {
    if let Ok(json) = serde_json::to_string(job) {
        let kind = ScopeKind::from_str(&job.scope_kind);
        let _ = tx.send((kind, job.scope_id, std::sync::Arc::from(json)));
    }
}

/// Everything the fifteen analyses read, and the one effect they have.
///
/// 🔑 **Every field is a trait object or a handle**, which is what makes a `run_*` reachable from a
/// test. Two of them were already seams before ADR-098 (`store`, `flows`); the other three plus
/// `progress` are what that ADR added. What is deliberately *absent* is [`AnalysisRepo`]: an
/// analysis has no business reading the job table, and now it structurally cannot.
pub(super) struct Engine {
    store: Arc<dyn MetricStore>,
    /// Flow store (ClickHouse, ADR-031), `None` when the flow tier is off — the `flow_*`/`traffic_*`/
    /// `talker_*`/`new_destination`/`scan`/`saturation` analyses no-op with an info finding then.
    flows: Option<Arc<dyn FlowStore>>,
    /// IP→ASN table handle for resolving AS names in flow findings (`new_destination`).
    ipasn: IpAsnHandle,
    /// Passive events, already routed to whichever store can answer completely (ADR-024). An
    /// analysis cannot reach `EventRepo` past this — see [`seams`].
    events: Arc<dyn AnalysisEvents>,
    /// The fleet and how it is filed — scope resolution and node names.
    inventory: Arc<dyn FleetInventory>,
    /// Connectivity graph, for `incident_correlate`'s neighbour expansion (ADR-043 → ADR-022).
    graph: Arc<dyn DerivedGraph>,
    /// Where a progress tick goes. ⚠️ The field and the method below share a name deliberately:
    /// method resolution picks the method, so the twenty-five call sites inside the `run_*` bodies
    /// did not change when this became a seam.
    progress: Arc<dyn JobProgress>,
}

impl Engine {
    /// Persist a progress tick and broadcast the updated row.
    async fn progress(&self, id: Uuid, pct: i32, phase: &str) {
        self.progress.tick(id, pct, phase).await;
    }

    /// The [`EventFilter`] an event analysis reads through: the job's time window, plus its
    /// authorized node set pushed down to the store.
    ///
    /// `None` for an `All`-scope job because there is nothing to restrict *to* — the whole fleet is
    /// the answer, and materialising up to 100k ids (the `exhaustive` node cap) into an `IN` list
    /// to say "everything" would be slower and mean the same thing.
    ///
    /// The push-down is not only an optimisation. `rule_gap` and `auth_probe` used to group
    /// fleet-wide and then drop a group whose *representative* node was out of scope, so a
    /// signature genuinely occurring inside the caller's group vanished whenever some node outside
    /// it happened to sort lower. Restricting before the grouping removes the question.
    fn scoped_window(params: &JobParams, node_ids: &[Uuid], from_s: i64, to_s: i64) -> EventFilter {
        EventFilter {
            since: DateTime::from_timestamp(from_s, 0),
            until: DateTime::from_timestamp(to_s, 0),
            visible_node_ids: match params.scope_kind {
                ScopeKind::All => None,
                ScopeKind::Group | ScopeKind::Node => Some(node_ids.to_vec()),
            },
            ..Default::default()
        }
    }

    // ── Passive-event aggregates (ADR-022 Increment 2) ──────────────────────────────────────────
    //
    // Named wrappers over [`AnalysisEvents`], kept so the analyses read the same at every call
    // site as they did before the seam. The store choice itself lives in `logstore::route_*` —
    // shared with the MCP `event_stats` tool, which had the same defect.

    async fn agg_counts_by_bucket(
        &self,
        filter: &EventFilter,
        bucket_secs: i64,
    ) -> anyhow::Result<Vec<EventBucketCount>> {
        self.events.counts_by_bucket(filter, bucket_secs).await
    }

    async fn agg_severity_counts(
        &self,
        filter: &EventFilter,
    ) -> anyhow::Result<Vec<EventSeverityCount>> {
        self.events.severity_counts(filter).await
    }

    async fn agg_unmatched_signatures(
        &self,
        filter: &EventFilter,
        limit: i64,
    ) -> anyhow::Result<Vec<EventSignatureCount>> {
        self.events.unmatched_signatures(filter, limit).await
    }

    async fn agg_auth_sources(
        &self,
        filter: &EventFilter,
        limit: i64,
    ) -> anyhow::Result<Vec<EventAuthSource>> {
        self.events.auth_sources(filter, limit).await
    }

    /// Resolve scope to a node set and run the tool's engine. `Ok(None)` ⇒ cancelled mid-run.
    async fn execute(
        &self,
        id: Uuid,
        params: &JobParams,
        cancel: &AtomicBool,
    ) -> anyhow::Result<Option<(Vec<NewFinding>, String)>> {
        self.progress(id, 5, "Resolving scope…").await;
        let mut node_ids = self.resolve_scope(params).await?;
        node_ids.truncate(params.node_cap());
        if node_ids.is_empty() {
            return Ok(Some((Vec::new(), "no nodes in scope".to_owned())));
        }
        let names = self
            .inventory
            .node_names(&node_ids)
            .await
            .unwrap_or_default();

        if cancel.load(Ordering::Relaxed) {
            return Ok(None);
        }

        match params.tool {
            AnalysisTool::Anomaly => {
                self.run_anomaly(id, params, &node_ids, &names, cancel)
                    .await
            }
            AnalysisTool::Capacity => {
                self.run_capacity(id, params, &node_ids, &names, cancel)
                    .await
            }
            AnalysisTool::Flap => self.run_flap(id, params, &node_ids, &names, cancel).await,
            AnalysisTool::Correlation => {
                self.run_correlation(id, params, &node_ids, &names, cancel)
                    .await
            }
            AnalysisTool::EventStorm => {
                self.run_event_storm(id, params, &node_ids, &names, cancel)
                    .await
            }
            AnalysisTool::EventFlap => {
                self.run_event_flap(id, params, &node_ids, &names, cancel)
                    .await
            }
            AnalysisTool::SeverityShift => {
                self.run_severity_shift(id, params, &node_ids, &names, cancel)
                    .await
            }
            AnalysisTool::RuleGap => {
                self.run_rule_gap(id, params, &node_ids, &names, cancel)
                    .await
            }
            AnalysisTool::AuthProbe => {
                self.run_auth_probe(id, params, &node_ids, &names, cancel)
                    .await
            }
            AnalysisTool::TrafficAnomaly => {
                self.run_traffic_anomaly(id, params, &node_ids, &names, cancel)
                    .await
            }
            AnalysisTool::TalkerShift => {
                self.run_talker_shift(id, params, &node_ids, &names, cancel)
                    .await
            }
            AnalysisTool::NewDestination => {
                self.run_new_destination(id, params, &node_ids, &names, cancel)
                    .await
            }
            AnalysisTool::FlowScan => {
                self.run_flow_scan(id, params, &node_ids, &names, cancel)
                    .await
            }
            AnalysisTool::Saturation => {
                self.run_saturation(id, params, &node_ids, &names, cancel)
                    .await
            }
            AnalysisTool::IncidentCorrelate => {
                self.run_incident_correlate(id, params, &node_ids, &names, cancel)
                    .await
            }
        }
    }

    /// Map a scope to its node ids (direct group members for `group` scope).
    async fn resolve_scope(&self, params: &JobParams) -> anyhow::Result<Vec<Uuid>> {
        match params.scope_kind {
            ScopeKind::All => Ok(self
                .inventory
                .list_nodes()
                .await?
                .into_iter()
                .map(|n| n.id.as_uuid())
                .collect()),
            ScopeKind::Group => {
                let Some(root) = params.scope_id else {
                    return Ok(Vec::new());
                };
                // A group scope covers the group AND every descendant subgroup (ADR-022): flatten
                // the subtree from the group edges, then fetch the nodes filed under any of them.
                let edges = self.inventory.group_edges().await?;
                let group_ids = group_subtree(&edges, root);
                self.inventory.nodes_in_groups(&group_ids).await
            }
            ScopeKind::Node => Ok(params.scope_id.into_iter().collect()),
        }
    }

    /// Read a gauge series at the node level (collapses table gauges to a node max).
    async fn gauge_range(
        &self,
        node: Uuid,
        metric: &str,
        from_s: i64,
        to_s: i64,
        step_s: u64,
    ) -> Vec<MetricPoint> {
        let key = SeriesKey::node(NodeId::from(node), metric);
        self.store.aggregate_range(&key, from_s, to_s, step_s).await
    }
}

/// Orchestrates analysis jobs: create → background task → progress (persist + broadcast) →
/// findings. Holds the SSE broadcast channel and a per-job cancel flag map.
pub struct AnalysisRunner {
    repo: Arc<AnalysisRepo>,
    /// What the analyses read. Held behind an `Arc` because a spawned job task outlives the call
    /// that created it.
    engine: Arc<Engine>,
    tx: broadcast::Sender<JobFrame>,
    cancels: Mutex<std::collections::HashMap<Uuid, Arc<AtomicBool>>>,
    /// Concurrency cap (ADR-028 Increment 2 WS-A): a permit is held for each running job's lifetime.
    slots: Arc<Semaphore>,
    /// The configured concurrent-job cap (for the [`CreateError::TooManyConcurrent`] message).
    max_concurrent: usize,
    /// Sliding-window creation timestamps for the per-minute rate limit.
    recent_starts: Mutex<VecDeque<Instant>>,
    /// The configured per-[`RATE_WINDOW`] creation cap.
    max_per_window: usize,
}

/// The stores an [`AnalysisRunner`] is wired to.
///
/// ⚠️ **This is the wiring, not the seams** — every field here is a concrete store, and that is
/// correct: `main.rs` has the real ones. The seams are inside [`Engine`], built from these by
/// [`AnalysisRunner::new`]. It was called `AnalysisSeams` until ADR-098 and the name was a lie of
/// exactly the kind `PollDispatcherSeams` still tells (ADR-096 decision 3).
///
/// A struct rather than more parameters: `new` was already at the `clippy::too_many_arguments`
/// threshold, and that lint is a design signal rather than something to silence
/// (coding-conventions). Same shape as [`crate::topology_projection::TopologySources`].
pub struct AnalysisStores {
    pub store: Arc<dyn MetricStore>,
    pub nodes: Arc<NodeRepo>,
    pub groups: Arc<GroupRepo>,
    pub events: Arc<EventRepo>,
    /// The event log store (ADR-024). `Some` ⇒ PostgreSQL holds only alert-linked rows, so the
    /// count-based aggregates must be read from here or they answer about a subset.
    pub logs: Option<Arc<dyn LogStore>>,
    pub flows: Option<Arc<dyn FlowStore>>,
    pub ipasn: IpAsnHandle,
    /// The connectivity graph, for `incident_correlate`'s neighbour expansion (ADR-043 → ADR-022).
    pub topo: crate::topology_projection::TopologySources,
}

impl AnalysisRunner {
    #[must_use]
    pub fn new(repo: Arc<AnalysisRepo>, stores: AnalysisStores) -> Self {
        let AnalysisStores {
            store,
            nodes,
            groups,
            events,
            logs,
            flows,
            ipasn,
            topo,
        } = stores;
        let (tx, _) = broadcast::channel(EVENT_BUFFER);
        let max_concurrent = env_cap("YAGRA_ANALYSIS_MAX_CONCURRENT", DEFAULT_MAX_CONCURRENT);
        let max_per_window = env_cap("YAGRA_ANALYSIS_RATE_PER_MIN", DEFAULT_RATE_PER_MIN);
        let engine = Arc::new(Engine {
            store,
            flows,
            ipasn,
            events: Arc::new(RoutedEvents::new(logs, events)),
            inventory: Arc::new(RepoInventory::new(nodes, groups)),
            graph: Arc::new(ProjectedGraph::new(topo)),
            progress: Arc::new(RepoProgress::new(repo.clone(), tx.clone())),
        });
        Self {
            repo,
            engine,
            tx,
            cancels: Mutex::new(std::collections::HashMap::new()),
            slots: Arc::new(Semaphore::new(max_concurrent)),
            max_concurrent,
            recent_starts: Mutex::new(VecDeque::new()),
            max_per_window,
        }
    }

    /// Record a creation against the sliding-window rate limit. `Err(RateLimited)` when the window is
    /// already full (delegates the pruning/count logic to the pure [`charge_window`]).
    fn admit_rate(&self) -> Result<(), CreateError> {
        let mut q = self
            .recent_starts
            .lock()
            .expect("recent_starts mutex poisoned");
        if charge_window(&mut q, Instant::now(), RATE_WINDOW, self.max_per_window) {
            Ok(())
        } else {
            Err(CreateError::RateLimited(self.max_per_window))
        }
    }

    /// Subscribe to the live job-status stream (SSE).
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<JobFrame> {
        self.tx.subscribe()
    }

    /// Recent jobs (the runs list), narrowed.
    pub async fn list(
        &self,
        limit: i64,
        filter: &JobFilter<'_>,
    ) -> anyhow::Result<Vec<AnalysisJob>> {
        self.repo.list(limit, filter).await
    }

    /// One job by id.
    pub async fn get(&self, id: Uuid) -> anyhow::Result<Option<AnalysisJob>> {
        self.repo.get(id).await
    }

    /// A job's findings.
    pub async fn findings(&self, id: Uuid) -> anyhow::Result<Vec<AnalysisFinding>> {
        self.repo.findings(id).await
    }

    /// Findings across every run (the All-findings search).
    ///
    /// ⚠️ The row type is still `SavedFinding` and stays that way: it is a published OpenAPI schema
    /// name, so renaming it would break every generated client to fix a label. The **screen** was
    /// renamed because "Saved" promised an action that does not exist — findings are written by
    /// `insert_findings` the moment a run completes, and there is nothing to save.
    pub async fn search_findings(
        &self,
        q: &FindingSearch<'_>,
    ) -> anyhow::Result<Vec<SavedFinding>> {
        self.repo.search_findings(q).await
    }

    /// The job/finding/schedule store, for the API's schedule CRUD and the leader's scheduler loop.
    #[must_use]
    pub fn repo(&self) -> Arc<AnalysisRepo> {
        self.repo.clone()
    }

    /// One node's cross-signal timeline, for the AI-assisted RCA context builder (ADR-029).
    ///
    /// A delegation because the signals are assembled by the [`Engine`] since ADR-098, and RCA
    /// holds the runner. Sharing the assembly is deliberate: a second implementation would drift,
    /// and the two would then disagree about the same outage.
    pub(crate) async fn incident_signals(
        &self,
        node: Uuid,
        from_s: i64,
        to_s: i64,
        recent_window_s: i64,
        sigma: f64,
    ) -> Vec<IncidentSignal> {
        self.engine
            .incident_signals(node, from_s, to_s, recent_window_s, sigma)
            .await
    }

    /// Whether this deployment has a flow store (ClickHouse, ADR-031).
    ///
    /// The API edge asks before accepting a *scheduled* flow analysis. A manual run of one with the
    /// tier off short-circuits to a single "flow tier not enabled" info finding, which is the right
    /// answer to a one-off question; scheduled daily it would stack up an empty successful run
    /// every day forever, which reads as a working schedule producing nothing.
    #[must_use]
    pub fn flow_enabled(&self) -> bool {
        self.engine.flows.is_some()
    }

    /// Request cancellation of a running job; the task observes the flag between phases.
    /// Returns whether the job was running.
    pub fn cancel(&self, id: Uuid) -> bool {
        let g = self.cancels.lock().expect("cancels mutex poisoned");
        if let Some(flag) = g.get(&id) {
            flag.store(true, Ordering::Relaxed);
            true
        } else {
            false
        }
    }

    /// Create a job, spawn its background task, and return the freshly-inserted row. Admission is
    /// bounded (ADR-028 Increment 2 WS-A): the concurrency permit is taken first (no side effect, so a
    /// rate-limit rejection drops it cleanly), then the creation-rate window is charged; the permit is
    /// moved into the job task and released when the job ends.
    pub async fn create(
        self: &Arc<Self>,
        params: JobParams,
        created_by: Option<String>,
    ) -> Result<AnalysisJob, CreateError> {
        let permit = Arc::clone(&self.slots)
            .try_acquire_owned()
            .map_err(|_| CreateError::TooManyConcurrent(self.max_concurrent))?;
        self.admit_rate()?;

        let job = self.repo.insert(&params, created_by.as_deref()).await?;
        broadcast_job(&self.tx, &job);
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancels
            .lock()
            .expect("cancels mutex poisoned")
            .insert(job.id, cancel.clone());
        let runner = self.clone();
        let id = job.id;
        tokio::spawn(async move {
            // Hold the concurrency permit for the whole job; dropping it frees a slot.
            let _permit = permit;
            runner.run_job(id, params, cancel).await;
        });
        Ok(job)
    }

    /// The whole job: dispatch to the engine, persist findings, finalize.
    async fn run_job(self: Arc<Self>, id: Uuid, params: JobParams, cancel: Arc<AtomicBool>) {
        let outcome = self.engine.execute(id, &params, &cancel).await;
        match outcome {
            Ok(Some((findings, summary))) => {
                if let Err(e) = self.repo.insert_findings(id, &findings).await {
                    tracing::error!(error = %e, job = %id, "failed to persist analysis findings");
                    let _ = self.repo.fail(id, "failed to persist findings").await;
                } else {
                    let count = i32::try_from(findings.len()).unwrap_or(i32::MAX);
                    let _ = self.repo.finish(id, count, &summary).await;
                }
            }
            Ok(None) => {
                let _ = self.repo.mark_cancelled(id).await;
            }
            Err(e) => {
                tracing::error!(error = %e, job = %id, "analysis job failed");
                let _ = self.repo.fail(id, "analysis failed — see core logs").await;
            }
        }
        // Final broadcast of the terminal state, then drop the cancel handle.
        if let Ok(Some(job)) = self.repo.get(id).await {
            broadcast_job(&self.tx, &job);
        }
        self.cancels
            .lock()
            .expect("cancels mutex poisoned")
            .remove(&id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The push-down is what makes a group-scoped `rule_gap` correct.
    ///
    /// Before it, the analysis grouped fleet-wide and then kept a signature only if its
    /// *representative* node (the alphabetically smallest UUID) was in scope — so a signature
    /// genuinely occurring inside the caller's group disappeared whenever some out-of-group node
    /// sorted lower. Asserted on the filter because that is where the decision is made; that the
    /// analysis then reads through it is covered by the behaviour tests in `events.rs`.
    #[test]
    fn a_scoped_analysis_restricts_at_the_store_not_by_a_representative_node() {
        let in_group = Uuid::from_u128(0xFFFF);
        let scoped = JobParams {
            tool: AnalysisTool::RuleGap,
            scope_kind: ScopeKind::Group,
            scope_id: None,
            scope_label: String::new(),
            window_secs: 3600,
            baseline_secs: 86_400,
            sensitivity: 3.0,
            depth: "standard".to_owned(),
            family: "all".to_owned(),
            notify: false,
        };
        let f = Engine::scoped_window(&scoped, &[in_group], 100, 200);
        assert_eq!(
            f.visible_node_ids.as_deref(),
            Some(&[in_group][..]),
            "a scoped job must restrict at the store"
        );
        assert_eq!(f.since.map(|t| t.timestamp()), Some(100));
        assert_eq!(f.until.map(|t| t.timestamp()), Some(200));

        // An `All`-scope job restricts to nothing rather than materialising the fleet into an
        // `IN` list that means "everything".
        let all = JobParams {
            tool: AnalysisTool::RuleGap,
            scope_kind: ScopeKind::All,
            scope_id: None,
            scope_label: String::new(),
            window_secs: 3600,
            baseline_secs: 86_400,
            sensitivity: 3.0,
            depth: "standard".to_owned(),
            family: "all".to_owned(),
            notify: false,
        };
        assert!(Engine::scoped_window(&all, &[in_group], 100, 200)
            .visible_node_ids
            .is_none());

        // An empty scope must stay `Some(vec![])` — "no visible nodes", which both backends match
        // nothing for. Collapsing it to `None` is the fail-open inversion.
        let empty = Engine::scoped_window(&scoped, &[], 100, 200);
        assert_eq!(empty.visible_node_ids.as_deref(), Some(&[][..]));
    }

    // The sliding-window rule itself is tested in [`crate::ratelimit`]; what belongs here is that
    // this runner's caps are wired to it (below) and that the concurrency permit behaves.

    #[test]
    fn concurrency_permit_caps_and_frees() {
        // Mirrors AnalysisRunner's `slots`: try_acquire_owned admits up to the cap, then errors until
        // an outstanding permit is dropped (a finished job frees its slot).
        let sem = Arc::new(Semaphore::new(2));
        let p1 = Arc::clone(&sem).try_acquire_owned().expect("slot 1");
        let _p2 = Arc::clone(&sem).try_acquire_owned().expect("slot 2");
        assert!(
            Arc::clone(&sem).try_acquire_owned().is_err(),
            "at cap ⇒ rejected"
        );
        drop(p1);
        assert!(
            Arc::clone(&sem).try_acquire_owned().is_ok(),
            "freed slot ⇒ admitted"
        );
    }
}
