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

use std::collections::{BTreeSet, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row};
use tokio::sync::broadcast;
use uuid::Uuid;
use yagra_common::{NodeId, SeriesKey};

use crate::groups::{group_subtree, GroupRepo};
use crate::repo::NodeRepo;
use crate::store::{MetricPoint, MetricStore};

/// Broadcast buffer for the job-status SSE stream (matches the alert engine's sizing intent).
const EVENT_BUFFER: usize = 256;
/// Target sample count when reading a series — bounds the step so a long window stays cheap.
const MAX_POINTS: i64 = 300;
/// Minimum samples a series needs before an analysis will draw a conclusion from it.
const MIN_POINTS: usize = 12;
/// Cap on findings returned per job (the report lists the most significant first).
const MAX_FINDINGS: usize = 60;

// ── Tool / state / scope enums ──────────────────────────────────────────────────────

/// Which diagnostic an analysis job runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnalysisTool {
    /// Baseline-relative deviation scoring (anomaly detection).
    Anomaly,
    /// Co-moving series within a window (event correlation).
    Correlation,
    /// Time-to-exhaustion projection (capacity forecast).
    Capacity,
    /// Reachability/link state churn (flap analysis).
    Flap,
}

impl AnalysisTool {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            AnalysisTool::Anomaly => "anomaly",
            AnalysisTool::Correlation => "correlation",
            AnalysisTool::Capacity => "capacity",
            AnalysisTool::Flap => "flap",
        }
    }

    #[must_use]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "anomaly" => Some(AnalysisTool::Anomaly),
            "correlation" => Some(AnalysisTool::Correlation),
            "capacity" => Some(AnalysisTool::Capacity),
            "flap" => Some(AnalysisTool::Flap),
            _ => None,
        }
    }
}

// Lifecycle states are stored as text in `analysis_jobs.state`:
// running | done | failed | cancelled (set via the repo's UPDATE statements).

/// Which nodes an analysis runs over.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopeKind {
    /// Every node in the inventory.
    All,
    /// The direct members of one node group.
    Group,
    /// A single node.
    Node,
}

impl ScopeKind {
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            ScopeKind::All => "all",
            ScopeKind::Group => "group",
            ScopeKind::Node => "node",
        }
    }

    #[must_use]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "all" => Some(ScopeKind::All),
            "group" => Some(ScopeKind::Group),
            "node" => Some(ScopeKind::Node),
            _ => None,
        }
    }
}

/// The validated request to launch an analysis (parsed at the API edge from the create body).
#[derive(Debug, Clone)]
pub struct JobParams {
    pub tool: AnalysisTool,
    pub scope_kind: ScopeKind,
    /// Group/node id for `group`/`node` scope; `None` for `all`.
    pub scope_id: Option<Uuid>,
    pub scope_label: String,
    /// Analysis window (recent period to inspect), seconds.
    pub window_secs: i64,
    /// Baseline lookback the model learns from (anomaly), seconds.
    pub baseline_secs: i64,
    /// Anomaly σ threshold (a point past this many std-devs is flagged); also the slider value.
    pub sensitivity: f64,
    /// Quick / Standard / Exhaustive — caps how many nodes are scanned.
    pub depth: String,
    /// Metric-family filter: `all` | `reachability_interface` | `system`.
    pub family: String,
    /// Notify-me vs run-silently (stored for the UI; notification wiring is future work).
    pub notify: bool,
}

impl JobParams {
    /// The params blob persisted as JSONB (echoed back to the client for display).
    fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "window_secs": self.window_secs,
            "baseline_secs": self.baseline_secs,
            "sensitivity": self.sensitivity,
            "depth": self.depth,
            "family": self.family,
            "notify": self.notify,
        })
    }

    /// Max nodes scanned for the selected depth (keeps a heavy scan bounded).
    fn node_cap(&self) -> usize {
        match self.depth.as_str() {
            "quick" => 20,
            "exhaustive" => 100_000,
            _ => 60, // standard
        }
    }
}

// ── Persisted shapes ────────────────────────────────────────────────────────────────

/// A job row, as served to the API / SSE. Timestamps are epoch-millis so the WebUI formats
/// relative times without a date dependency.
#[derive(Debug, Clone, Serialize)]
pub struct AnalysisJob {
    pub id: Uuid,
    pub tool: String,
    pub scope_kind: String,
    pub scope_id: Option<Uuid>,
    pub scope_label: String,
    pub params: serde_json::Value,
    pub state: String,
    pub pct: i32,
    pub phase: Option<String>,
    pub finding_count: i32,
    pub summary: Option<String>,
    pub error: Option<String>,
    pub created_ms: i64,
    pub started_ms: Option<i64>,
    pub finished_ms: Option<i64>,
}

/// One finding produced by an analysis (anomaly card / correlation pair / capacity / flap row).
#[derive(Debug, Clone, Serialize)]
pub struct AnalysisFinding {
    pub id: Uuid,
    pub score: f64,
    pub severity: String,
    pub node_id: Option<Uuid>,
    pub node_name: String,
    pub metric: String,
    pub kind: String,
    pub when_label: String,
    pub duration: String,
    pub detail: serde_json::Value,
}

/// A finding before persistence (the engine fills these; the repo assigns ids).
#[derive(Debug, Clone)]
struct NewFinding {
    score: f64,
    severity: String,
    node_id: Option<Uuid>,
    node_name: String,
    metric: String,
    kind: String,
    when_label: String,
    duration: String,
    detail: serde_json::Value,
}

/// Severity bucket from a 0..100 score (matches the WebUI: ≥90 crit, ≥75 warn, else info).
fn severity_for(score: f64) -> &'static str {
    if score >= 90.0 {
        "crit"
    } else if score >= 75.0 {
        "warn"
    } else {
        "info"
    }
}

/// Joined node name, falling back to the id string when unknown.
fn name_lookup(names: &HashMap<Uuid, String>, id: &Uuid) -> String {
    names.get(id).cloned().unwrap_or_else(|| id.to_string())
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

fn now_s() -> i64 {
    now_ms() / 1000
}

// ── Repository ───────────────────────────────────────────────────────────────────────

/// Columns selected for a job row (timestamps projected to epoch-millis).
const JOB_COLS: &str = "id, tool, scope_kind, scope_id, scope_label, params, state, pct, phase, \
     finding_count, summary, error, \
     (EXTRACT(EPOCH FROM created_at) * 1000)::bigint AS created_ms, \
     (EXTRACT(EPOCH FROM started_at) * 1000)::bigint AS started_ms, \
     (EXTRACT(EPOCH FROM finished_at) * 1000)::bigint AS finished_ms";

fn job_from_row(row: &sqlx::postgres::PgRow) -> anyhow::Result<AnalysisJob> {
    Ok(AnalysisJob {
        id: row.try_get("id")?,
        tool: row.try_get("tool")?,
        scope_kind: row.try_get("scope_kind")?,
        scope_id: row.try_get("scope_id")?,
        scope_label: row.try_get("scope_label")?,
        params: row.try_get("params")?,
        state: row.try_get("state")?,
        pct: row.try_get("pct")?,
        phase: row.try_get("phase")?,
        finding_count: row.try_get("finding_count")?,
        summary: row.try_get("summary")?,
        error: row.try_get("error")?,
        created_ms: row.try_get("created_ms")?,
        started_ms: row.try_get("started_ms")?,
        finished_ms: row.try_get("finished_ms")?,
    })
}

/// PostgreSQL-backed store for analysis jobs and their findings.
pub struct AnalysisRepo {
    pool: PgPool,
}

impl AnalysisRepo {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Insert a new job in the `running` state (started_at = now) and return its row.
    pub async fn insert(
        &self,
        params: &JobParams,
        created_by: Option<&str>,
    ) -> anyhow::Result<AnalysisJob> {
        let id = Uuid::new_v4();
        let row = sqlx::query(&format!(
            "INSERT INTO analysis_jobs \
             (id, tool, scope_kind, scope_id, scope_label, params, state, pct, phase, \
              finding_count, created_by, started_at) \
             VALUES ($1, $2, $3, $4, $5, $6, 'running', 0, $7, 0, $8, now()) \
             RETURNING {JOB_COLS}"
        ))
        .bind(id)
        .bind(params.tool.as_str())
        .bind(params.scope_kind.as_str())
        .bind(params.scope_id)
        .bind(&params.scope_label)
        .bind(params.to_json())
        .bind("Queued — fetching history…")
        .bind(created_by)
        .fetch_one(&self.pool)
        .await?;
        job_from_row(&row)
    }

    /// Update progress (percent + phase caption) of a running job.
    pub async fn set_progress(&self, id: Uuid, pct: i32, phase: &str) -> anyhow::Result<()> {
        sqlx::query(
            "UPDATE analysis_jobs SET pct = $2, phase = $3 WHERE id = $1 AND state = 'running'",
        )
        .bind(id)
        .bind(pct.clamp(0, 100))
        .bind(phase)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Mark a job done with its result summary (findings inserted separately).
    pub async fn finish(&self, id: Uuid, finding_count: i32, summary: &str) -> anyhow::Result<()> {
        sqlx::query(
            "UPDATE analysis_jobs SET state = 'done', pct = 100, phase = NULL, \
             finding_count = $2, summary = $3, finished_at = now() WHERE id = $1",
        )
        .bind(id)
        .bind(finding_count)
        .bind(summary)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Mark a job failed with a reason.
    pub async fn fail(&self, id: Uuid, error: &str) -> anyhow::Result<()> {
        sqlx::query(
            "UPDATE analysis_jobs SET state = 'failed', phase = NULL, error = $2, \
             finished_at = now() WHERE id = $1",
        )
        .bind(id)
        .bind(error)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Mark a job cancelled (set by the runner when its cancel flag was tripped).
    pub async fn mark_cancelled(&self, id: Uuid) -> anyhow::Result<()> {
        sqlx::query(
            "UPDATE analysis_jobs SET state = 'cancelled', phase = NULL, finished_at = now() \
             WHERE id = $1 AND state = 'running'",
        )
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Insert a batch of findings for a job.
    async fn insert_findings(&self, job_id: Uuid, findings: &[NewFinding]) -> anyhow::Result<()> {
        for f in findings {
            sqlx::query(
                "INSERT INTO analysis_findings \
                 (id, job_id, score, severity, node_id, node_name, metric, kind, when_label, duration, detail) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
            )
            .bind(Uuid::new_v4())
            .bind(job_id)
            .bind(f.score)
            .bind(&f.severity)
            .bind(f.node_id)
            .bind(&f.node_name)
            .bind(&f.metric)
            .bind(&f.kind)
            .bind(&f.when_label)
            .bind(&f.duration)
            .bind(&f.detail)
            .execute(&self.pool)
            .await?;
        }
        Ok(())
    }

    /// Recent jobs, newest first (the runs list). `limit` clamped by the caller.
    pub async fn list(&self, limit: i64) -> anyhow::Result<Vec<AnalysisJob>> {
        let rows = sqlx::query(&format!(
            "SELECT {JOB_COLS} FROM analysis_jobs ORDER BY created_at DESC LIMIT $1"
        ))
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(job_from_row).collect()
    }

    /// One job by id.
    pub async fn get(&self, id: Uuid) -> anyhow::Result<Option<AnalysisJob>> {
        let row = sqlx::query(&format!(
            "SELECT {JOB_COLS} FROM analysis_jobs WHERE id = $1"
        ))
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(job_from_row).transpose()
    }

    /// A job's findings, highest score first.
    pub async fn findings(&self, job_id: Uuid) -> anyhow::Result<Vec<AnalysisFinding>> {
        let rows = sqlx::query(
            "SELECT id, score, severity, node_id, node_name, metric, kind, when_label, duration, detail \
             FROM analysis_findings WHERE job_id = $1 ORDER BY score DESC",
        )
        .bind(job_id)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                Ok(AnalysisFinding {
                    id: row.try_get("id")?,
                    score: row.try_get("score")?,
                    severity: row.try_get("severity")?,
                    node_id: row.try_get("node_id")?,
                    node_name: row.try_get("node_name")?,
                    metric: row.try_get("metric")?,
                    kind: row.try_get("kind")?,
                    when_label: row.try_get("when_label")?,
                    duration: row.try_get("duration")?,
                    detail: row.try_get("detail")?,
                })
            })
            .collect()
    }

    /// On startup, fail any job left `running` by a previous core process (it can't resume).
    pub async fn fail_orphans(&self) -> anyhow::Result<u64> {
        let res = sqlx::query(
            "UPDATE analysis_jobs SET state = 'failed', phase = NULL, \
             error = 'core restarted while running', finished_at = now() WHERE state = 'running'",
        )
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected())
    }
}

// ── Runner ─────────────────────────────────────────────────────────────────────────

/// Orchestrates analysis jobs: create → background task → progress (persist + broadcast) →
/// findings. Holds the SSE broadcast channel and a per-job cancel flag map.
pub struct AnalysisRunner {
    repo: Arc<AnalysisRepo>,
    store: Arc<dyn MetricStore>,
    nodes: Arc<NodeRepo>,
    /// Group hierarchy — to expand a "group" scope to the group + its descendant subgroups.
    groups: Arc<GroupRepo>,
    tx: broadcast::Sender<String>,
    cancels: Mutex<std::collections::HashMap<Uuid, Arc<AtomicBool>>>,
}

impl AnalysisRunner {
    #[must_use]
    pub fn new(
        repo: Arc<AnalysisRepo>,
        store: Arc<dyn MetricStore>,
        nodes: Arc<NodeRepo>,
        groups: Arc<GroupRepo>,
    ) -> Self {
        let (tx, _) = broadcast::channel(EVENT_BUFFER);
        Self {
            repo,
            store,
            nodes,
            groups,
            tx,
            cancels: Mutex::new(std::collections::HashMap::new()),
        }
    }

    /// Subscribe to the live job-status stream (SSE).
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<String> {
        self.tx.subscribe()
    }

    fn broadcast_job(&self, job: &AnalysisJob) {
        if let Ok(json) = serde_json::to_string(job) {
            let _ = self.tx.send(json);
        }
    }

    /// Recent jobs (the runs list).
    pub async fn list(&self, limit: i64) -> anyhow::Result<Vec<AnalysisJob>> {
        self.repo.list(limit).await
    }

    /// One job by id.
    pub async fn get(&self, id: Uuid) -> anyhow::Result<Option<AnalysisJob>> {
        self.repo.get(id).await
    }

    /// A job's findings.
    pub async fn findings(&self, id: Uuid) -> anyhow::Result<Vec<AnalysisFinding>> {
        self.repo.findings(id).await
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

    /// Create a job, spawn its background task, and return the freshly-inserted row.
    pub async fn create(
        self: &Arc<Self>,
        params: JobParams,
        created_by: Option<String>,
    ) -> anyhow::Result<AnalysisJob> {
        let job = self.repo.insert(&params, created_by.as_deref()).await?;
        self.broadcast_job(&job);
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancels
            .lock()
            .expect("cancels mutex poisoned")
            .insert(job.id, cancel.clone());
        let runner = self.clone();
        let id = job.id;
        tokio::spawn(async move {
            runner.run_job(id, params, cancel).await;
        });
        Ok(job)
    }

    /// Persist a progress tick and broadcast the updated row.
    async fn progress(&self, id: Uuid, pct: i32, phase: &str) {
        if let Err(e) = self.repo.set_progress(id, pct, phase).await {
            tracing::warn!(error = %e, job = %id, "analysis progress update failed");
        }
        if let Ok(Some(job)) = self.repo.get(id).await {
            self.broadcast_job(&job);
        }
    }

    /// The whole job: resolve scope, dispatch to the engine, persist findings, finalize.
    async fn run_job(self: Arc<Self>, id: Uuid, params: JobParams, cancel: Arc<AtomicBool>) {
        let outcome = self.execute(id, &params, &cancel).await;
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
            self.broadcast_job(&job);
        }
        self.cancels
            .lock()
            .expect("cancels mutex poisoned")
            .remove(&id);
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
        let names = self.nodes.node_names(&node_ids).await.unwrap_or_default();

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
        }
    }

    /// Map a scope to its node ids (direct group members for `group` scope).
    async fn resolve_scope(&self, params: &JobParams) -> anyhow::Result<Vec<Uuid>> {
        match params.scope_kind {
            ScopeKind::All => Ok(self
                .nodes
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
                let edges = self.groups.edges().await?;
                let group_ids = group_subtree(&edges, root);
                self.nodes.nodes_in_groups(&group_ids).await
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

    // ── Engine: Anomaly Detection ─────────────────────────────────────────────────
    //
    // For each node × usable gauge: learn a baseline (mean/σ over the baseline window) and flag
    // the recent window's largest deviation past the sensitivity σ threshold. Score scales with
    // how far past the threshold it went; the shape (spike/level/drift/flat/season) is classified
    // from the recent segment. The full series is stored for the report chart.
    async fn run_anomaly(
        &self,
        id: Uuid,
        params: &JobParams,
        node_ids: &[Uuid],
        names: &HashMap<Uuid, String>,
        cancel: &AtomicBool,
    ) -> anyhow::Result<Option<(Vec<NewFinding>, String)>> {
        let to = now_s();
        let from = to - params.baseline_secs.max(3600);
        let step = read_step(from, to);
        let recent_cutoff = to - params.window_secs.max(300);
        let sigma = params.sensitivity.max(0.5);

        self.progress(id, 15, "Fetching baseline…").await;
        let mut findings: Vec<NewFinding> = Vec::new();
        let mut nodes_hit: BTreeSet<Uuid> = BTreeSet::new();
        let mut series_scanned = 0usize;
        let total = node_ids.len().max(1);

        for (i, node) in node_ids.iter().enumerate() {
            if cancel.load(Ordering::Relaxed) {
                return Ok(None);
            }
            let pct = 15 + (i * 70 / total) as i32;
            self.progress(id, pct, "Fitting per-metric models…").await;

            let metrics = self
                .store
                .node_metric_names(*node, params.baseline_secs as u64)
                .await;
            for metric in metrics {
                if !anomaly_usable(&metric) || !family_matches(params, &metric) {
                    continue;
                }
                let pts = self.gauge_range(*node, &metric, from, to, step).await;
                if pts.len() < MIN_POINTS {
                    continue;
                }
                series_scanned += 1;
                let Some(found) = score_anomaly(&pts, recent_cutoff, sigma) else {
                    continue;
                };
                nodes_hit.insert(*node);
                let node_name = name_lookup(names, node);
                findings.push(NewFinding {
                    score: found.score,
                    severity: severity_for(found.score).to_owned(),
                    node_id: Some(*node),
                    node_name,
                    metric: metric.clone(),
                    kind: found.kind.to_owned(),
                    when_label: rel_label(found.when_s, to),
                    duration: found.duration,
                    detail: found.detail,
                });
            }
        }

        self.progress(id, 90, "Ranking & classifying findings…")
            .await;
        findings.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        findings.truncate(MAX_FINDINGS);
        let summary = format!(
            "{} anomalies · {} nodes · {} series scanned",
            findings.len(),
            nodes_hit.len(),
            series_scanned
        );
        Ok(Some((findings, summary)))
    }

    // ── Engine: Capacity Forecast ─────────────────────────────────────────────────
    //
    // For each node × utilization-percent gauge: least-squares regress over the window, and if the
    // trend is rising, project the seconds until it reaches 100%. Nearer exhaustion ⇒ higher score.
    async fn run_capacity(
        &self,
        id: Uuid,
        params: &JobParams,
        node_ids: &[Uuid],
        names: &HashMap<Uuid, String>,
        cancel: &AtomicBool,
    ) -> anyhow::Result<Option<(Vec<NewFinding>, String)>> {
        let to = now_s();
        let from = to - params.window_secs.max(7 * 86_400);
        let step = read_step(from, to);
        self.progress(id, 15, "Reading utilization history…").await;

        let mut findings: Vec<NewFinding> = Vec::new();
        let total = node_ids.len().max(1);
        for (i, node) in node_ids.iter().enumerate() {
            if cancel.load(Ordering::Relaxed) {
                return Ok(None);
            }
            self.progress(id, 15 + (i * 70 / total) as i32, "Projecting growth…")
                .await;
            let metrics = self
                .store
                .node_metric_names(*node, params.window_secs as u64)
                .await;
            for metric in metrics {
                if !is_utilization(&metric) {
                    continue;
                }
                let pts = self.gauge_range(*node, &metric, from, to, step).await;
                if pts.len() < MIN_POINTS {
                    continue;
                }
                let Some(proj) = project_exhaustion(&pts) else {
                    continue;
                };
                let days = proj.tte_secs as f64 / 86_400.0;
                let score = capacity_score(days);
                findings.push(NewFinding {
                    score,
                    severity: severity_for(score).to_owned(),
                    node_id: Some(*node),
                    node_name: name_lookup(names, node),
                    metric: metric.clone(),
                    kind: "capacity".to_owned(),
                    when_label: format!("{:.0}% now", proj.current),
                    duration: format!("~{} to 100%", human_days(days)),
                    detail: serde_json::json!({
                        "current": proj.current,
                        "slope_per_day": proj.slope_per_s * 86_400.0,
                        "tte_days": days,
                    }),
                });
            }
        }

        self.progress(id, 90, "Ranking by urgency…").await;
        findings.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        findings.truncate(MAX_FINDINGS);
        let near = findings.iter().filter(|f| f.severity != "info").count();
        let summary = format!(
            "{} resources approaching exhaustion ({} within 30d)",
            findings.len(),
            near
        );
        Ok(Some((findings, summary)))
    }

    // ── Engine: Flap Analysis ─────────────────────────────────────────────────────
    //
    // Reachability flap: read each node's ICMP RTT and count gaps (down→up cycles) — a node that
    // bounces leaves gaps in its otherwise-regular series. High churn ⇒ a flapping node.
    async fn run_flap(
        &self,
        id: Uuid,
        params: &JobParams,
        node_ids: &[Uuid],
        names: &HashMap<Uuid, String>,
        cancel: &AtomicBool,
    ) -> anyhow::Result<Option<(Vec<NewFinding>, String)>> {
        let to = now_s();
        let from = to - params.window_secs.max(86_400);
        let step = read_step(from, to);
        self.progress(id, 15, "Scanning reachability history…")
            .await;

        let mut findings: Vec<NewFinding> = Vec::new();
        let total = node_ids.len().max(1);
        let window_hours = ((to - from) as f64 / 3600.0).max(1.0);
        for (i, node) in node_ids.iter().enumerate() {
            if cancel.load(Ordering::Relaxed) {
                return Ok(None);
            }
            self.progress(id, 15 + (i * 70 / total) as i32, "Counting state churn…")
                .await;
            // Raw RTT series (not aggregated) — gaps are down periods.
            let key = SeriesKey::node(NodeId::from(*node), "icmp_rtt_ms");
            let pts = self.store.range(&key, from, to, step).await;
            if pts.len() < MIN_POINTS {
                continue;
            }
            let flaps = count_flaps(&pts, step as i64);
            if flaps < 2 {
                continue;
            }
            let rate = flaps as f64 / window_hours;
            let score = flap_score(flaps);
            findings.push(NewFinding {
                score,
                severity: severity_for(score).to_owned(),
                node_id: Some(*node),
                node_name: name_lookup(names, node),
                metric: "icmp_rtt_ms".to_owned(),
                kind: "flap".to_owned(),
                when_label: format!("{flaps} flaps"),
                duration: format!("{rate:.1}/h"),
                detail: serde_json::json!({ "flaps": flaps, "per_hour": rate }),
            });
        }

        self.progress(id, 90, "Ranking flapping nodes…").await;
        findings.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        findings.truncate(MAX_FINDINGS);
        let chronic = findings.iter().filter(|f| f.severity == "crit").count();
        let summary = format!("{} nodes flapping · {} chronic", findings.len(), chronic);
        Ok(Some((findings, summary)))
    }

    // ── Engine: Event Correlation ─────────────────────────────────────────────────
    //
    // Pull a capped set of the most-variable gauges across the scope over the window, then surface
    // pairs whose values co-move (|Pearson r| past a threshold) on their shared timestamps.
    async fn run_correlation(
        &self,
        id: Uuid,
        params: &JobParams,
        node_ids: &[Uuid],
        names: &HashMap<Uuid, String>,
        cancel: &AtomicBool,
    ) -> anyhow::Result<Option<(Vec<NewFinding>, String)>> {
        let to = now_s();
        let from = to - params.window_secs.max(3600);
        let step = read_step(from, to);
        self.progress(id, 15, "Collecting series…").await;

        // Gather candidate series (node, metric, points). Cap per-node to keep it bounded.
        let mut series: Vec<CandidateSeries> = Vec::new();
        let total = node_ids.len().max(1);
        for (i, node) in node_ids.iter().enumerate() {
            if cancel.load(Ordering::Relaxed) {
                return Ok(None);
            }
            self.progress(id, 15 + (i * 45 / total) as i32, "Collecting series…")
                .await;
            let metrics = self
                .store
                .node_metric_names(*node, params.window_secs as u64)
                .await;
            let mut per_node = 0;
            for metric in metrics {
                if !anomaly_usable(&metric) {
                    continue;
                }
                let pts = self.gauge_range(*node, &metric, from, to, step).await;
                if pts.len() < MIN_POINTS {
                    continue;
                }
                let values: Vec<f64> = pts.iter().map(|p| p.v).collect();
                let m = mean(&values);
                let var = variance(&values, m);
                if var <= f64::EPSILON {
                    continue;
                }
                series.push(CandidateSeries {
                    label: format!("{} · {}", name_lookup(names, node), metric),
                    var,
                    points: pts,
                });
                per_node += 1;
                if per_node >= 6 {
                    break;
                }
            }
        }

        // Keep the most-variable series (the interesting movers), cap the pair count.
        self.progress(id, 65, "Cross-correlating…").await;
        series.sort_by(|a, b| {
            b.var
                .partial_cmp(&a.var)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        series.truncate(24);

        let mut findings: Vec<NewFinding> = Vec::new();
        for a in 0..series.len() {
            if cancel.load(Ordering::Relaxed) {
                return Ok(None);
            }
            for b in (a + 1)..series.len() {
                let Some((r, n)) = correlate(&series[a].points, &series[b].points) else {
                    continue;
                };
                if n < 10 || r.abs() < 0.85 {
                    continue;
                }
                let score = (r.abs() * 100.0).min(100.0);
                findings.push(NewFinding {
                    score,
                    severity: severity_for(score).to_owned(),
                    node_id: None,
                    node_name: series[a].label.clone(),
                    metric: format!("{} ↔ {}", series[a].label, series[b].label),
                    kind: "correlation".to_owned(),
                    when_label: if r >= 0.0 {
                        "co-rising".to_owned()
                    } else {
                        "inverse".to_owned()
                    },
                    duration: format!("r={r:.2}"),
                    detail: serde_json::json!({ "r": r, "samples": n }),
                });
            }
        }

        self.progress(id, 90, "Ranking correlations…").await;
        findings.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        findings.truncate(MAX_FINDINGS);
        let summary = format!("{} correlated pairs", findings.len());
        Ok(Some((findings, summary)))
    }
}

/// A candidate series for correlation (its label, variance, and points).
struct CandidateSeries {
    label: String,
    var: f64,
    points: Vec<MetricPoint>,
}

// ── Pure analysis maths (unit-tested) ────────────────────────────────────────────────

/// Sample step that keeps a window under [`MAX_POINTS`] samples (min 60s).
fn read_step(from_s: i64, to_s: i64) -> u64 {
    let span = (to_s - from_s).max(1);
    ((span / MAX_POINTS).max(60)) as u64
}

fn mean(xs: &[f64]) -> f64 {
    if xs.is_empty() {
        return 0.0;
    }
    xs.iter().sum::<f64>() / xs.len() as f64
}

fn variance(xs: &[f64], mean: f64) -> f64 {
    if xs.len() < 2 {
        return 0.0;
    }
    xs.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / xs.len() as f64
}

fn stddev(xs: &[f64], mean: f64) -> f64 {
    variance(xs, mean).sqrt()
}

/// Least-squares slope of `ys` against `xs` (per unit of x). `None` if x has no spread.
fn linreg_slope(xs: &[f64], ys: &[f64]) -> Option<f64> {
    let n = xs.len();
    if n < 2 || n != ys.len() {
        return None;
    }
    let mx = mean(xs);
    let my = mean(ys);
    let mut num = 0.0;
    let mut den = 0.0;
    for (x, y) in xs.iter().zip(ys) {
        let dx = x - mx;
        num += dx * (y - my);
        den += dx * dx;
    }
    if den.abs() <= f64::EPSILON {
        None
    } else {
        Some(num / den)
    }
}

/// Pearson correlation of two equal-length vectors. `None` if either is constant.
fn pearson(xs: &[f64], ys: &[f64]) -> Option<f64> {
    let n = xs.len();
    if n < 2 || n != ys.len() {
        return None;
    }
    let mx = mean(xs);
    let my = mean(ys);
    let (mut sxy, mut sxx, mut syy) = (0.0, 0.0, 0.0);
    for (x, y) in xs.iter().zip(ys) {
        let dx = x - mx;
        let dy = y - my;
        sxy += dx * dy;
        sxx += dx * dx;
        syy += dy * dy;
    }
    let den = (sxx * syy).sqrt();
    if den <= f64::EPSILON {
        None
    } else {
        Some(sxy / den)
    }
}

/// Correlate two point series on their shared timestamps; returns `(r, sample_count)`.
fn correlate(a: &[MetricPoint], b: &[MetricPoint]) -> Option<(f64, usize)> {
    use std::collections::HashMap;
    let bmap: HashMap<i64, f64> = b.iter().map(|p| (p.t, p.v)).collect();
    let mut xs = Vec::new();
    let mut ys = Vec::new();
    for p in a {
        if let Some(v) = bmap.get(&p.t) {
            xs.push(p.v);
            ys.push(*v);
        }
    }
    pearson(&xs, &ys).map(|r| (r, xs.len()))
}

/// A scored anomaly within a series.
struct ScoredAnomaly {
    score: f64,
    kind: &'static str,
    when_s: i64,
    duration: String,
    detail: serde_json::Value,
}

/// Detect the largest baseline-relative deviation in the recent window. Baseline stats come from
/// points before `recent_cutoff` (falling back to the whole series if the baseline is too short).
fn score_anomaly(pts: &[MetricPoint], recent_cutoff: i64, sigma: f64) -> Option<ScoredAnomaly> {
    let baseline: Vec<f64> = pts
        .iter()
        .filter(|p| p.t < recent_cutoff)
        .map(|p| p.v)
        .collect();
    let recent: Vec<&MetricPoint> = pts.iter().filter(|p| p.t >= recent_cutoff).collect();
    if recent.is_empty() {
        return None;
    }
    // Baseline statistics (fall back to the whole series when the pre-window history is too thin).
    let (base_mean, base_sd) = if baseline.len() >= MIN_POINTS / 2 {
        let m = mean(&baseline);
        (m, stddev(&baseline, m))
    } else {
        let all: Vec<f64> = pts.iter().map(|p| p.v).collect();
        let m = mean(&all);
        (m, stddev(&all, m))
    };
    if base_sd <= f64::EPSILON {
        // Flat baseline: only a real move off the constant is interesting.
        let moved = recent
            .iter()
            .any(|p| (p.v - base_mean).abs() > base_mean.abs().max(1.0) * 0.25);
        if !moved {
            return None;
        }
    }
    let sd = base_sd.max(base_mean.abs().max(1.0) * 1e-3); // floor to avoid divide-by-zero
                                                           // Largest |z| in the recent window.
    let mut zmax = 0.0;
    let mut at = recent[0].t;
    for p in &recent {
        let z = (p.v - base_mean).abs() / sd;
        if z > zmax {
            zmax = z;
            at = p.t;
        }
    }
    if zmax < sigma {
        return None;
    }
    // Score: at the threshold → 75 (warning); ~1.5× threshold → ~100 (critical).
    let score = (75.0 * zmax / sigma).clamp(0.0, 100.0);
    let recent_vals: Vec<f64> = recent.iter().map(|p| p.v).collect();
    let kind = classify_shape(base_mean, sd, &recent_vals, &recent);
    // Downsample the series for the report chart (≤64 points).
    let detail = chart_detail(pts, base_mean, sd, recent_cutoff);
    let dur_pts = recent
        .iter()
        .filter(|p| (p.v - base_mean).abs() / sd >= sigma)
        .count();
    let duration = if dur_pts >= recent.len().saturating_sub(1) {
        "ongoing".to_owned()
    } else {
        format!("{dur_pts} samples")
    };
    Some(ScoredAnomaly {
        score,
        kind,
        when_s: at,
        duration,
        detail,
    })
}

/// Classify the recent segment's shape relative to the baseline.
fn classify_shape(base_mean: f64, sd: f64, recent: &[f64], pts: &[&MetricPoint]) -> &'static str {
    if recent.len() < 3 {
        return "spike";
    }
    let rmean = mean(recent);
    let rsd = stddev(recent, rmean);
    // Stuck / flatline: recent variance collapses well below the baseline's.
    if rsd <= sd * 0.1 && (rmean - base_mean).abs() < sd * 0.5 {
        return "flat";
    }
    // Trend drift: a sustained slope across the recent window.
    let xs: Vec<f64> = pts.iter().map(|p| p.t as f64).collect();
    if let Some(slope) = linreg_slope(&xs, recent) {
        let span = (pts.last().map(|p| p.t).unwrap_or(0) - pts.first().map(|p| p.t).unwrap_or(0))
            .max(1) as f64;
        if (slope * span).abs() > sd * 2.0 {
            return "drift";
        }
    }
    // Level shift: the recent mean settled far from the baseline and stays there.
    if (rmean - base_mean).abs() > sd * 1.5 && rsd < sd * 1.5 {
        return "level";
    }
    // A single brief excursion that returns is a spike.
    let over = recent
        .iter()
        .filter(|v| (*v - base_mean).abs() > sd * 2.0)
        .count();
    if over <= 2 {
        return "spike";
    }
    // Otherwise the rhythm changed without a clean level/drift — a seasonality break.
    "season"
}

/// Build the report chart payload: a downsampled actual line plus the baseline mean/σ band.
fn chart_detail(pts: &[MetricPoint], mean: f64, sd: f64, recent_cutoff: i64) -> serde_json::Value {
    const MAX: usize = 64;
    let stride = (pts.len() / MAX).max(1);
    let sampled: Vec<&MetricPoint> = pts.iter().step_by(stride).collect();
    let points: Vec<serde_json::Value> = sampled
        .iter()
        .map(|p| serde_json::json!({ "t": p.t, "v": p.v, "recent": p.t >= recent_cutoff }))
        .collect();
    serde_json::json!({
        "points": points,
        "mean": mean,
        "sigma": sd,
        "recent_from": recent_cutoff,
    })
}

/// A capacity projection: current value, growth slope, and seconds to 100%.
struct Projection {
    current: f64,
    slope_per_s: f64,
    tte_secs: i64,
}

/// Project when a utilization-percent series reaches 100% by least-squares trend. `None` if it
/// isn't rising, is already full, or exhaustion is beyond a 1-year horizon.
fn project_exhaustion(pts: &[MetricPoint]) -> Option<Projection> {
    let xs: Vec<f64> = pts.iter().map(|p| p.t as f64).collect();
    let ys: Vec<f64> = pts.iter().map(|p| p.v).collect();
    let slope = linreg_slope(&xs, &ys)?;
    let current = *ys.last()?;
    if slope <= 0.0 || !(0.0..100.0).contains(&current) {
        return None;
    }
    let tte = (100.0 - current) / slope;
    if tte <= 0.0 || tte > 365.0 * 86_400.0 {
        return None;
    }
    Some(Projection {
        current,
        slope_per_s: slope,
        tte_secs: tte as i64,
    })
}

/// Urgency score from days-to-exhaustion: ≤7d critical, ≤30d warning, else info.
fn capacity_score(days: f64) -> f64 {
    if days <= 7.0 {
        95.0
    } else if days <= 30.0 {
        82.0
    } else if days <= 90.0 {
        65.0
    } else {
        52.0
    }
}

/// Count reachability flaps: gaps between consecutive samples larger than 2× the expected step
/// each mark one down→up cycle.
fn count_flaps(pts: &[MetricPoint], step_s: i64) -> u32 {
    let threshold = step_s.max(1) * 2;
    let mut flaps = 0u32;
    for w in pts.windows(2) {
        if w[1].t - w[0].t > threshold {
            flaps += 1;
        }
    }
    flaps
}

/// Score a flapping node by its flap count: ≥6 critical, ≥3 warning, else info.
fn flap_score(flaps: u32) -> f64 {
    if flaps >= 6 {
        92.0
    } else if flaps >= 3 {
        80.0
    } else {
        60.0
    }
}

/// Human "N days" / "N hours" label.
fn human_days(days: f64) -> String {
    if days < 1.0 {
        format!("{}h", (days * 24.0).round() as i64)
    } else if days < 90.0 {
        format!("{}d", days.round() as i64)
    } else {
        format!("{}mo", (days / 30.0).round() as i64)
    }
}

/// Relative "when" label from an event time vs now.
fn rel_label(at_s: i64, now_s: i64) -> String {
    let d = (now_s - at_s).max(0);
    if d < 3600 {
        format!("{}m ago", d / 60)
    } else if d < 86_400 {
        format!("{}h ago", d / 3600)
    } else {
        format!("{}d ago", d / 86_400)
    }
}

// ── Metric classification (by name) ───────────────────────────────────────────────────

/// Gauges suitable for anomaly/correlation: numeric, continuous, not raw counters or discrete
/// status enums.
fn anomaly_usable(metric: &str) -> bool {
    if is_counter(metric) {
        return false;
    }
    if metric.contains("status") || metric.contains("state") {
        return false; // discrete enums (oper/admin/bgp state)
    }
    true
}

/// Raw counters (rate-derived elsewhere) — excluded from level-based anomaly/capacity reads.
fn is_counter(metric: &str) -> bool {
    metric.contains("octets")
        || metric.contains("errors")
        || metric.contains("discards")
        || metric.contains("packets")
}

/// Percent-like utilization gauges the capacity forecast can extrapolate toward 100%.
fn is_utilization(metric: &str) -> bool {
    metric.contains("pct")
        || metric.contains("util")
        || metric.contains("usage")
        || metric.ends_with("_pct")
}

/// Whether a metric belongs to the job's requested family filter.
fn family_matches(params: &JobParams, metric: &str) -> bool {
    match params.family.as_str() {
        "reachability_interface" => metric == "icmp_rtt_ms" || metric.starts_with("if_"),
        "system" => {
            metric.contains("cpu")
                || metric.contains("mem")
                || metric.contains("temp")
                || metric.contains("load")
                || metric.contains("usage")
                || metric.contains("util")
                || metric.contains("processor")
                || metric.contains("disk")
                || metric.contains("storage")
                || metric.contains("sessions")
                || metric.contains("swap")
        }
        _ => true, // "all"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pt(t: i64, v: f64) -> MetricPoint {
        MetricPoint { t, v }
    }

    #[test]
    fn mean_and_stddev_basic() {
        let xs = [2.0, 4.0, 6.0];
        assert!((mean(&xs) - 4.0).abs() < 1e-9);
        assert!((stddev(&xs, 4.0) - (8.0_f64 / 3.0).sqrt()).abs() < 1e-9);
    }

    #[test]
    fn linreg_slope_recovers_line() {
        let xs = [0.0, 1.0, 2.0, 3.0];
        let ys = [1.0, 3.0, 5.0, 7.0]; // slope 2
        assert!((linreg_slope(&xs, &ys).unwrap() - 2.0).abs() < 1e-9);
    }

    #[test]
    fn pearson_perfect_and_inverse() {
        let xs = [1.0, 2.0, 3.0, 4.0];
        let up = [2.0, 4.0, 6.0, 8.0];
        let down = [8.0, 6.0, 4.0, 2.0];
        assert!((pearson(&xs, &up).unwrap() - 1.0).abs() < 1e-9);
        assert!((pearson(&xs, &down).unwrap() + 1.0).abs() < 1e-9);
        assert!(pearson(&xs, &[1.0, 1.0, 1.0, 1.0]).is_none()); // constant
    }

    #[test]
    fn correlate_uses_shared_timestamps() {
        let a = [pt(0, 1.0), pt(10, 2.0), pt(20, 3.0), pt(30, 4.0)];
        let b = [pt(10, 2.0), pt(20, 4.0), pt(30, 6.0), pt(40, 8.0)];
        let (r, n) = correlate(&a, &b).unwrap();
        assert_eq!(n, 3); // shared t = 10,20,30
        assert!((r - 1.0).abs() < 1e-9);
    }

    #[test]
    fn read_step_bounds_points() {
        // 14 days at ≤300 points ⇒ step ≥ ~4032s.
        let step = read_step(0, 14 * 86_400);
        assert!(step >= 60);
        assert!((14 * 86_400) as u64 / step <= MAX_POINTS as u64 + 1);
    }

    #[test]
    fn project_exhaustion_linear_rise() {
        // 50% climbing 1%/day → ~50 days to 100%.
        let day = 86_400;
        let pts: Vec<MetricPoint> = (0..15).map(|i| pt(i * day, 50.0 + i as f64)).collect();
        let proj = project_exhaustion(&pts).expect("rising series projects");
        assert!((proj.current - 64.0).abs() < 1e-6);
        let days = proj.tte_secs as f64 / 86_400.0;
        assert!((days - 36.0).abs() < 2.0); // (100-64)/1 ≈ 36 days
    }

    #[test]
    fn project_exhaustion_skips_flat_and_full() {
        let day = 86_400;
        let flat: Vec<MetricPoint> = (0..15).map(|i| pt(i * day, 40.0)).collect();
        assert!(project_exhaustion(&flat).is_none());
        let full: Vec<MetricPoint> = (0..15).map(|i| pt(i * day, 100.0)).collect();
        assert!(project_exhaustion(&full).is_none());
    }

    #[test]
    fn count_flaps_detects_gaps() {
        // Regular 60s samples with two big gaps.
        let mut pts = vec![pt(0, 1.0), pt(60, 1.0), pt(120, 1.0)];
        pts.push(pt(600, 1.0)); // gap 1 (480s > 120s)
        pts.push(pt(660, 1.0));
        pts.push(pt(2000, 1.0)); // gap 2
        assert_eq!(count_flaps(&pts, 60), 2);
    }

    #[test]
    fn score_anomaly_flags_spike_past_sigma() {
        // Flat baseline ~10 with tiny noise, then a recent spike to 30.
        let step = 300;
        let cutoff = 40 * step;
        let mut pts: Vec<MetricPoint> = (0..40)
            .map(|i| pt(i * step, 10.0 + ((i % 2) as f64) * 0.1))
            .collect();
        pts.push(pt(40 * step, 30.0)); // recent spike
        pts.push(pt(41 * step, 10.1));
        let found = score_anomaly(&pts, cutoff, 3.0).expect("spike flagged");
        assert!(found.score >= 75.0);
    }

    #[test]
    fn score_anomaly_ignores_normal_recent() {
        let step = 300;
        let cutoff = 40 * step;
        let mut pts: Vec<MetricPoint> = (0..40)
            .map(|i| pt(i * step, 10.0 + ((i % 2) as f64) * 0.5))
            .collect();
        pts.push(pt(40 * step, 10.2)); // within noise
        pts.push(pt(41 * step, 9.9));
        assert!(score_anomaly(&pts, cutoff, 3.0).is_none());
    }

    #[test]
    fn severity_thresholds() {
        assert_eq!(severity_for(95.0), "crit");
        assert_eq!(severity_for(80.0), "warn");
        assert_eq!(severity_for(50.0), "info");
    }

    #[test]
    fn tool_round_trips() {
        for t in [
            AnalysisTool::Anomaly,
            AnalysisTool::Correlation,
            AnalysisTool::Capacity,
            AnalysisTool::Flap,
        ] {
            assert_eq!(AnalysisTool::from_str(t.as_str()), Some(t));
        }
        assert_eq!(AnalysisTool::from_str("nope"), None);
    }

    #[test]
    fn metric_classification() {
        assert!(is_counter("if_hc_in_octets"));
        assert!(is_counter("if_in_errors"));
        assert!(!is_counter("huawei_cpu_usage"));
        assert!(!anomaly_usable("if_oper_status"));
        assert!(!anomaly_usable("if_hc_in_octets"));
        assert!(anomaly_usable("icmp_rtt_ms"));
        assert!(is_utilization("huawei_mem_usage"));
        assert!(is_utilization("ucd_disk_used_pct"));
        assert!(!is_utilization("icmp_rtt_ms"));
    }
}
