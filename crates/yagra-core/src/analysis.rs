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

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use chrono::DateTime;
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row};
use tokio::sync::{broadcast, Semaphore};
use uuid::Uuid;
use yagra_common::{NodeId, SeriesKey};

use crate::events::{EventFilter, EventRepo, EventRow, EventSeverityCount};
use crate::flowstore::{AsDir, FlowQuery, FlowSeriesQuery, FlowStore};
use crate::groups::{group_subtree, GroupRepo};
use crate::ipasn::IpAsnHandle;
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

/// Read a positive-`usize` cap from an env var, falling back to `default` when unset/invalid/zero.
fn env_cap(var: &str, default: usize) -> usize {
    std::env::var(var)
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(default)
}

/// Why [`AnalysisRunner::create`] declined to launch a job. The two rate outcomes map to HTTP 429 at
/// the REST edge (and a "try again shortly" note over MCP); [`CreateError::Internal`] maps to 500.
#[derive(Debug, thiserror::Error)]
pub enum CreateError {
    /// The concurrent-job cap is full (value = the cap).
    #[error("analysis capacity reached ({0} jobs already running) — retry shortly")]
    TooManyConcurrent(usize),
    /// The per-minute creation cap is exhausted (value = the cap).
    #[error("analysis rate limit reached (max {0} new jobs/minute) — retry shortly")]
    RateLimited(usize),
    /// An internal failure (e.g. the job-row insert failed).
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

/// Charge one creation against a sliding window: prune entries older than `window`, then admit iff
/// fewer than `max` remain (pushing `now` on admission). Pure so the rate limiter is unit-testable
/// without a full runner (which needs a live `PgPool`).
fn charge_window(q: &mut VecDeque<Instant>, now: Instant, window: Duration, max: usize) -> bool {
    while let Some(&front) = q.front() {
        if now.duration_since(front) >= window {
            q.pop_front();
        } else {
            break;
        }
    }
    if q.len() >= max {
        return false;
    }
    q.push_back(now);
    true
}

// ── Tool / state / scope enums ──────────────────────────────────────────────────────

/// Which diagnostic an analysis job runs. The first four read VictoriaMetrics (ADR-022); the
/// `event_*` kinds read the passive-event store (`events`, ADR-024) and the `flow_*`/`traffic_*`/
/// `talker_*`/`new_destination`/`scan` kinds read the flow store (ClickHouse, ADR-031). `saturation`
/// and `incident_correlate` are cross-store. All remain read-only + admission-bounded.
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
    // ── Passive monitoring (events) ──
    /// Per-node passive-event volume spike vs baseline.
    EventStorm,
    /// Repeated fire↔clear churn of the same event rule per node.
    EventFlap,
    /// Syslog severity mix skewing toward error/critical vs baseline.
    SeverityShift,
    /// High-volume unmatched events clustered by signature (missing rule coverage).
    RuleGap,
    /// Authentication-failure clustering by source (brute force / misconfigured NMS).
    AuthProbe,
    // ── Flow monitoring (ClickHouse) ──
    /// Node/interface flow-volume anomaly vs baseline.
    TrafficAnomaly,
    /// A newly dominant talker/conversation vs a baseline window.
    TalkerShift,
    /// Traffic to a destination AS/port absent from the baseline window.
    NewDestination,
    /// A source contacting an abnormal number of distinct destinations/ports (scan/worm).
    FlowScan,
    // ── Cross-store ──
    /// A single conversation dominating a busy node's traffic (link-hog / saturation).
    Saturation,
    /// Cross-signal incident timeline (metric anomaly + events + flow shift) for a node.
    IncidentCorrelate,
}

impl AnalysisTool {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            AnalysisTool::Anomaly => "anomaly",
            AnalysisTool::Correlation => "correlation",
            AnalysisTool::Capacity => "capacity",
            AnalysisTool::Flap => "flap",
            AnalysisTool::EventStorm => "event_storm",
            AnalysisTool::EventFlap => "event_flap",
            AnalysisTool::SeverityShift => "severity_shift",
            AnalysisTool::RuleGap => "rule_gap",
            AnalysisTool::AuthProbe => "auth_probe",
            AnalysisTool::TrafficAnomaly => "traffic_anomaly",
            AnalysisTool::TalkerShift => "talker_shift",
            AnalysisTool::NewDestination => "new_destination",
            AnalysisTool::FlowScan => "flow_scan",
            AnalysisTool::Saturation => "saturation",
            AnalysisTool::IncidentCorrelate => "incident_correlate",
        }
    }

    #[must_use]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "anomaly" => Some(AnalysisTool::Anomaly),
            "correlation" => Some(AnalysisTool::Correlation),
            "capacity" => Some(AnalysisTool::Capacity),
            "flap" => Some(AnalysisTool::Flap),
            "event_storm" => Some(AnalysisTool::EventStorm),
            "event_flap" => Some(AnalysisTool::EventFlap),
            "severity_shift" => Some(AnalysisTool::SeverityShift),
            "rule_gap" => Some(AnalysisTool::RuleGap),
            "auth_probe" => Some(AnalysisTool::AuthProbe),
            "traffic_anomaly" => Some(AnalysisTool::TrafficAnomaly),
            "talker_shift" => Some(AnalysisTool::TalkerShift),
            "new_destination" => Some(AnalysisTool::NewDestination),
            "flow_scan" => Some(AnalysisTool::FlowScan),
            "saturation" => Some(AnalysisTool::Saturation),
            "incident_correlate" => Some(AnalysisTool::IncidentCorrelate),
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
    /// Passive-event store (ADR-024) — read by the `event_*` analyses and `incident_correlate`.
    events: Arc<EventRepo>,
    /// Flow store (ClickHouse, ADR-031), `None` when the flow tier is off — the `flow_*`/`traffic_*`/
    /// `talker_*`/`new_destination`/`scan`/`saturation` analyses no-op with an info finding then.
    flows: Option<Arc<dyn FlowStore>>,
    /// IP→ASN table handle for resolving AS names in flow findings (`new_destination`).
    ipasn: IpAsnHandle,
    tx: broadcast::Sender<String>,
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

impl AnalysisRunner {
    #[must_use]
    pub fn new(
        repo: Arc<AnalysisRepo>,
        store: Arc<dyn MetricStore>,
        nodes: Arc<NodeRepo>,
        groups: Arc<GroupRepo>,
        events: Arc<EventRepo>,
        flows: Option<Arc<dyn FlowStore>>,
        ipasn: IpAsnHandle,
    ) -> Self {
        let (tx, _) = broadcast::channel(EVENT_BUFFER);
        let max_concurrent = env_cap("YAGRA_ANALYSIS_MAX_CONCURRENT", DEFAULT_MAX_CONCURRENT);
        let max_per_window = env_cap("YAGRA_ANALYSIS_RATE_PER_MIN", DEFAULT_RATE_PER_MIN);
        Self {
            repo,
            store,
            nodes,
            groups,
            events,
            flows,
            ipasn,
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
        self.broadcast_job(&job);
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

    // ── Engine: Event Storm (passive) ─────────────────────────────────────────────
    //
    // Per node: bucket the passive-event volume, learn a baseline rate, and flag a recent bucket
    // whose count spikes past the sensitivity σ (boot loop, interface churn, chatty misconfig).
    async fn run_event_storm(
        &self,
        id: Uuid,
        params: &JobParams,
        node_ids: &[Uuid],
        names: &HashMap<Uuid, String>,
        cancel: &AtomicBool,
    ) -> anyhow::Result<Option<(Vec<NewFinding>, String)>> {
        let to = now_s();
        let from = to - params.baseline_secs.max(6 * 3600);
        let recent_cutoff = to - params.window_secs.max(600);
        let sigma = params.sensitivity.max(0.5);
        self.progress(id, 25, "Reading event volume…").await;
        if cancel.load(Ordering::Relaxed) {
            return Ok(None);
        }
        let rows = self
            .events
            .event_counts_by_bucket(from * 1000, to * 1000, EVENT_BUCKET_SECS)
            .await?;
        let scope: HashSet<Uuid> = node_ids.iter().copied().collect();
        // node → (baseline bucket counts, recent bucket counts with their bucket time).
        let mut per_node: HashMap<Uuid, StormBuckets> = HashMap::new();
        for r in rows {
            if !scope.contains(&r.node_id) {
                continue;
            }
            let e = per_node.entry(r.node_id).or_default();
            if r.bucket_start_s < recent_cutoff {
                e.0.push(r.count as f64);
            } else {
                e.1.push((r.bucket_start_s, r.count as f64));
            }
        }
        self.progress(id, 75, "Scoring volume spikes…").await;
        let mut findings: Vec<NewFinding> = Vec::new();
        for (node, (baseline, recent)) in per_node {
            let (peak_bucket, peak) =
                recent
                    .iter()
                    .copied()
                    .fold((0i64, 0f64), |a, (t, c)| if c > a.1 { (t, c) } else { a });
            if peak < EVENT_STORM_FLOOR {
                continue;
            }
            let Some(score) = burst_score(&baseline, peak, sigma) else {
                continue;
            };
            findings.push(NewFinding {
                score,
                severity: severity_for(score).to_owned(),
                node_id: Some(node),
                node_name: name_lookup(names, &node),
                metric: "event_rate".to_owned(),
                kind: "event_storm".to_owned(),
                when_label: rel_label(peak_bucket, to),
                duration: format!("{peak:.0} in {}m", EVENT_BUCKET_SECS / 60),
                detail: serde_json::json!({
                    "peak": peak,
                    "baseline_mean": mean(&baseline),
                    "bucket_secs": EVENT_BUCKET_SECS,
                }),
            });
        }
        finalize(&mut findings);
        let summary = format!("{} nodes with event-volume spikes", findings.len());
        Ok(Some((findings, summary)))
    }

    // ── Engine: Event Flap (passive) ──────────────────────────────────────────────
    //
    // Repeated fire↔clear of the same event rule per node (linkDown/linkUp thrash, BGP session
    // churn) — complements the ICMP-only `flap`. A completed cycle is one fire paired with a clear.
    async fn run_event_flap(
        &self,
        id: Uuid,
        params: &JobParams,
        node_ids: &[Uuid],
        names: &HashMap<Uuid, String>,
        cancel: &AtomicBool,
    ) -> anyhow::Result<Option<(Vec<NewFinding>, String)>> {
        let to = now_s();
        let from = to - params.window_secs.max(6 * 3600);
        self.progress(id, 30, "Reading event churn…").await;
        if cancel.load(Ordering::Relaxed) {
            return Ok(None);
        }
        let stats = self.events.event_flap_stats(from * 1000, to * 1000).await?;
        let scope: HashSet<Uuid> = node_ids.iter().copied().collect();
        let window_hours = ((to - from) as f64 / 3600.0).max(1.0);
        let mut findings: Vec<NewFinding> = Vec::new();
        for s in stats {
            if !scope.contains(&s.node_id) {
                continue;
            }
            let cycles = s.fires.min(s.clears);
            if cycles < 2 {
                continue;
            }
            let score = flap_score(u32::try_from(cycles).unwrap_or(u32::MAX));
            let rate = cycles as f64 / window_hours;
            findings.push(NewFinding {
                score,
                severity: severity_for(score).to_owned(),
                node_id: Some(s.node_id),
                node_name: name_lookup(names, &s.node_id),
                metric: format!("event:{}", s.rule_name),
                kind: "event_flap".to_owned(),
                when_label: format!("{cycles} cycles"),
                duration: format!("{rate:.1}/h"),
                detail: serde_json::json!({
                    "rule_id": s.rule_id, "fires": s.fires, "clears": s.clears,
                    "cycles": cycles, "per_hour": rate,
                }),
            });
        }
        finalize(&mut findings);
        let summary = format!("{} event-flapping (rule, node) pairs", findings.len());
        Ok(Some((findings, summary)))
    }

    // ── Engine: Severity Shift (passive) ──────────────────────────────────────────
    //
    // A node whose syslog severity mix skews toward error/critical in the recent window vs its
    // baseline — a quiet degradation signal.
    async fn run_severity_shift(
        &self,
        id: Uuid,
        params: &JobParams,
        node_ids: &[Uuid],
        names: &HashMap<Uuid, String>,
        cancel: &AtomicBool,
    ) -> anyhow::Result<Option<(Vec<NewFinding>, String)>> {
        let to = now_s();
        let from = to - params.baseline_secs.max(6 * 3600);
        let recent_cutoff = to - params.window_secs.max(600);
        self.progress(id, 30, "Reading severity mix…").await;
        if cancel.load(Ordering::Relaxed) {
            return Ok(None);
        }
        let baseline = self
            .events
            .event_severity_counts(from * 1000, recent_cutoff * 1000)
            .await?;
        if cancel.load(Ordering::Relaxed) {
            return Ok(None);
        }
        let recent = self
            .events
            .event_severity_counts(recent_cutoff * 1000, to * 1000)
            .await?;
        let scope: HashSet<Uuid> = node_ids.iter().copied().collect();
        let base_frac = severity_high_fractions(&baseline, &scope);
        let recent_frac = severity_high_fractions(&recent, &scope);
        let mut findings: Vec<NewFinding> = Vec::new();
        for (node, (rhigh, rtotal, rfrac)) in &recent_frac {
            if *rtotal < SEVERITY_FLOOR {
                continue;
            }
            let bfrac = base_frac.get(node).map_or(0.0, |x| x.2);
            let Some(score) = severity_shift_score(bfrac, *rfrac) else {
                continue;
            };
            findings.push(NewFinding {
                score,
                severity: severity_for(score).to_owned(),
                node_id: Some(*node),
                node_name: name_lookup(names, node),
                metric: "syslog_severity".to_owned(),
                kind: "severity_shift".to_owned(),
                when_label: format!("{:.0}% err+", rfrac * 100.0),
                duration: format!("was {:.0}%", bfrac * 100.0),
                detail: serde_json::json!({
                    "recent_high_frac": rfrac, "baseline_high_frac": bfrac,
                    "recent_high": rhigh, "recent_total": rtotal,
                }),
            });
        }
        finalize(&mut findings);
        let summary = format!("{} nodes with a severity shift", findings.len());
        Ok(Some((findings, summary)))
    }

    // ── Engine: Rule Gap (passive) ────────────────────────────────────────────────
    //
    // High-volume unmatched events clustered by signature (trap OID / syslog app-name): "you're
    // receiving N of these but no rule matches — consider one". Coverage advice, capped at warning.
    async fn run_rule_gap(
        &self,
        id: Uuid,
        params: &JobParams,
        node_ids: &[Uuid],
        names: &HashMap<Uuid, String>,
        cancel: &AtomicBool,
    ) -> anyhow::Result<Option<(Vec<NewFinding>, String)>> {
        let to = now_s();
        let from = to - params.window_secs.max(86_400);
        self.progress(id, 35, "Clustering unmatched events…").await;
        if cancel.load(Ordering::Relaxed) {
            return Ok(None);
        }
        let sigs = self
            .events
            .event_unmatched_signatures(from * 1000, to * 1000, 200)
            .await?;
        let scope: HashSet<Uuid> = node_ids.iter().copied().collect();
        let all_scope = params.scope_kind == ScopeKind::All;
        let mut findings: Vec<NewFinding> = Vec::new();
        for s in sigs {
            if s.count < RULE_GAP_FLOOR {
                continue;
            }
            if !all_scope && !s.sample_node.is_some_and(|n| scope.contains(&n)) {
                continue;
            }
            let score = gap_score(s.count);
            findings.push(NewFinding {
                score,
                severity: severity_for(score).to_owned(),
                node_id: s.sample_node,
                node_name: s
                    .sample_node
                    .map_or_else(|| "fleet".to_owned(), |n| name_lookup(names, &n)),
                metric: format!("{}:{}", s.kind, s.signature),
                kind: "rule_gap".to_owned(),
                when_label: format!("{} events", s.count),
                duration: "unmatched".to_owned(),
                detail: serde_json::json!({
                    "kind": s.kind, "signature": s.signature, "count": s.count,
                }),
            });
        }
        finalize(&mut findings);
        let summary = format!("{} unmatched-event signatures (rule gaps)", findings.len());
        Ok(Some((findings, summary)))
    }

    // ── Engine: Auth Probe (passive) ──────────────────────────────────────────────
    //
    // authenticationFailure traps + auth-failure syslog clustered by source — brute force or a
    // misconfigured NMS hammering SNMP/SSH.
    async fn run_auth_probe(
        &self,
        id: Uuid,
        params: &JobParams,
        node_ids: &[Uuid],
        names: &HashMap<Uuid, String>,
        cancel: &AtomicBool,
    ) -> anyhow::Result<Option<(Vec<NewFinding>, String)>> {
        let to = now_s();
        let from = to - params.window_secs.max(3600);
        self.progress(id, 35, "Clustering auth failures…").await;
        if cancel.load(Ordering::Relaxed) {
            return Ok(None);
        }
        let sources = self
            .events
            .event_auth_sources(from * 1000, to * 1000, 100)
            .await?;
        let scope: HashSet<Uuid> = node_ids.iter().copied().collect();
        let all_scope = params.scope_kind == ScopeKind::All;
        let mut findings: Vec<NewFinding> = Vec::new();
        for s in sources {
            if s.count < AUTH_FLOOR {
                continue;
            }
            if !all_scope && !s.node_id.is_some_and(|n| scope.contains(&n)) {
                continue;
            }
            let score = auth_score(s.count);
            let src = s.source_ip.clone().unwrap_or_else(|| "unknown".to_owned());
            findings.push(NewFinding {
                score,
                severity: severity_for(score).to_owned(),
                node_id: s.node_id,
                node_name: s
                    .node_id
                    .map_or_else(|| src.clone(), |n| name_lookup(names, &n)),
                metric: "auth_failures".to_owned(),
                kind: "auth_probe".to_owned(),
                when_label: format!("{} failures", s.count),
                duration: src,
                detail: serde_json::json!({ "source_ip": s.source_ip, "count": s.count }),
            });
        }
        finalize(&mut findings);
        let summary = format!("{} auth-failure sources", findings.len());
        Ok(Some((findings, summary)))
    }

    // ── Engine: Traffic Anomaly (flow) ────────────────────────────────────────────
    //
    // Per node: sum flow bytes per 5-minute bucket, learn a baseline, flag a recent spike (DDoS,
    // saturation, runaway backup, exfiltration).
    async fn run_traffic_anomaly(
        &self,
        id: Uuid,
        params: &JobParams,
        node_ids: &[Uuid],
        names: &HashMap<Uuid, String>,
        cancel: &AtomicBool,
    ) -> anyhow::Result<Option<(Vec<NewFinding>, String)>> {
        let Some(flows) = self.flows.clone() else {
            return Ok(Some(flow_tier_off()));
        };
        let to = now_s();
        let from = to - params.baseline_secs.max(6 * 3600);
        let recent_cutoff = to - params.window_secs.max(600);
        let sigma = params.sensitivity.max(0.5);
        let total = node_ids.len().max(1);
        let mut findings: Vec<NewFinding> = Vec::new();
        for (i, node) in node_ids.iter().enumerate() {
            if cancel.load(Ordering::Relaxed) {
                return Ok(None);
            }
            self.progress(id, 15 + (i * 70 / total) as i32, "Reading flow volume…")
                .await;
            let q = FlowSeriesQuery {
                node_id: Some(*node),
                from_unix_ms: from * 1000,
                to_unix_ms: to * 1000,
                proto: None,
            };
            let pts = flows.series(&q).await.unwrap_or_default();
            if pts.len() < MIN_POINTS {
                continue;
            }
            let mut per_bucket: std::collections::BTreeMap<i64, f64> =
                std::collections::BTreeMap::new();
            for p in &pts {
                *per_bucket.entry(p.ts_unix_ms / 1000).or_default() += p.bytes as f64;
            }
            let baseline: Vec<f64> = per_bucket
                .iter()
                .filter(|(t, _)| **t < recent_cutoff)
                .map(|(_, v)| *v)
                .collect();
            let recent: Vec<(i64, f64)> = per_bucket
                .iter()
                .filter(|(t, _)| **t >= recent_cutoff)
                .map(|(t, v)| (*t, *v))
                .collect();
            if recent.is_empty() || baseline.len() < MIN_POINTS / 2 {
                continue;
            }
            let (peak_t, peak) =
                recent
                    .iter()
                    .copied()
                    .fold((0i64, 0f64), |a, (t, c)| if c > a.1 { (t, c) } else { a });
            let Some(score) = burst_score(&baseline, peak, sigma) else {
                continue;
            };
            findings.push(NewFinding {
                score,
                severity: severity_for(score).to_owned(),
                node_id: Some(*node),
                node_name: name_lookup(names, node),
                metric: "flow_bytes".to_owned(),
                kind: "traffic_anomaly".to_owned(),
                when_label: rel_label(peak_t, to),
                duration: format!("{} peak", human_bytes(peak)),
                detail: serde_json::json!({
                    "peak_bytes": peak, "baseline_mean_bytes": mean(&baseline),
                }),
            });
        }
        finalize(&mut findings);
        let summary = format!("{} nodes with flow-volume anomalies", findings.len());
        Ok(Some((findings, summary)))
    }

    // ── Engine: Talker Shift (flow) ───────────────────────────────────────────────
    //
    // A talker that is newly dominant vs the previous equal-length window (new heavy host / exfil
    // source / rogue device).
    async fn run_talker_shift(
        &self,
        id: Uuid,
        params: &JobParams,
        node_ids: &[Uuid],
        names: &HashMap<Uuid, String>,
        cancel: &AtomicBool,
    ) -> anyhow::Result<Option<(Vec<NewFinding>, String)>> {
        let Some(flows) = self.flows.clone() else {
            return Ok(Some(flow_tier_off()));
        };
        let to = now_s();
        let window = params.window_secs.max(1800);
        let recent_from = to - window;
        let base_from = to - 2 * window;
        let total = node_ids.len().max(1);
        let mut findings: Vec<NewFinding> = Vec::new();
        for (i, node) in node_ids.iter().enumerate() {
            if cancel.load(Ordering::Relaxed) {
                return Ok(None);
            }
            self.progress(id, 15 + (i * 70 / total) as i32, "Comparing talkers…")
                .await;
            let recent_q = FlowQuery {
                node_id: Some(*node),
                from_unix_ms: recent_from * 1000,
                to_unix_ms: to * 1000,
                limit: 10,
                proto: None,
                dst_port: None,
                peer: None,
                asn: None,
            };
            let base_q = FlowQuery {
                from_unix_ms: base_from * 1000,
                to_unix_ms: recent_from * 1000,
                limit: 100,
                ..recent_q
            };
            let recent = flows.top_talkers(&recent_q).await.unwrap_or_default();
            if recent.is_empty() {
                continue;
            }
            let base = flows.top_talkers(&base_q).await.unwrap_or_default();
            let base_keys: HashSet<String> = base.iter().map(|t| t.addr.clone()).collect();
            let recent_keys: Vec<String> = recent.iter().map(|t| t.addr.clone()).collect();
            let Some((addr, rank)) = first_novel(&recent_keys, &base_keys) else {
                continue;
            };
            let bytes = recent
                .iter()
                .find(|t| t.addr == addr)
                .map_or(0, |t| t.bytes);
            if bytes < TALKER_FLOOR {
                continue;
            }
            let score = novelty_score(rank);
            findings.push(NewFinding {
                score,
                severity: severity_for(score).to_owned(),
                node_id: Some(*node),
                node_name: name_lookup(names, node),
                metric: "top_talker".to_owned(),
                kind: "talker_shift".to_owned(),
                when_label: format!("new #{}", rank + 1),
                duration: human_bytes(bytes as f64),
                detail: serde_json::json!({ "addr": addr, "bytes": bytes, "rank": rank + 1 }),
            });
        }
        finalize(&mut findings);
        let summary = format!("{} nodes with a new dominant talker", findings.len());
        Ok(Some((findings, summary)))
    }

    // ── Engine: New Destination (flow) ────────────────────────────────────────────
    //
    // Traffic to a destination AS or port absent from the baseline window — a new external
    // destination (possible C2/exfil), a new service, or a scan target.
    async fn run_new_destination(
        &self,
        id: Uuid,
        params: &JobParams,
        node_ids: &[Uuid],
        names: &HashMap<Uuid, String>,
        cancel: &AtomicBool,
    ) -> anyhow::Result<Option<(Vec<NewFinding>, String)>> {
        let Some(flows) = self.flows.clone() else {
            return Ok(Some(flow_tier_off()));
        };
        let to = now_s();
        let window = params.window_secs.max(1800);
        let recent_from = to - window;
        let base_from = to - 2 * window;
        let total = node_ids.len().max(1);
        let mut findings: Vec<NewFinding> = Vec::new();
        for (i, node) in node_ids.iter().enumerate() {
            if cancel.load(Ordering::Relaxed) {
                return Ok(None);
            }
            self.progress(id, 15 + (i * 70 / total) as i32, "Comparing destinations…")
                .await;
            let recent_q = FlowQuery {
                node_id: Some(*node),
                from_unix_ms: recent_from * 1000,
                to_unix_ms: to * 1000,
                limit: 10,
                proto: None,
                dst_port: None,
                peer: None,
                asn: None,
            };
            let base_q = FlowQuery {
                from_unix_ms: base_from * 1000,
                to_unix_ms: recent_from * 1000,
                limit: 200,
                ..recent_q
            };
            // Destination AS novelty (the headline signal, using the AS enrichment).
            let recent_as = flows
                .top_as(&recent_q, AsDir::Dst)
                .await
                .unwrap_or_default();
            let base_as = flows.top_as(&base_q, AsDir::Dst).await.unwrap_or_default();
            let base_as_keys: HashSet<String> = base_as
                .iter()
                .filter(|a| a.asn != 0)
                .map(|a| a.asn.to_string())
                .collect();
            let recent_as_keys: Vec<String> = recent_as
                .iter()
                .filter(|a| a.asn != 0)
                .map(|a| a.asn.to_string())
                .collect();
            if let Some((asn_str, rank)) = first_novel(&recent_as_keys, &base_as_keys) {
                let asn: u32 = asn_str.parse().unwrap_or(0);
                let bytes = recent_as
                    .iter()
                    .find(|a| a.asn == asn)
                    .map_or(0, |a| a.bytes);
                if bytes >= DEST_FLOOR {
                    let name = self.resolve_as_name(asn);
                    let score = novelty_score(rank);
                    findings.push(NewFinding {
                        score,
                        severity: severity_for(score).to_owned(),
                        node_id: Some(*node),
                        node_name: name_lookup(names, node),
                        metric: "dst_as".to_owned(),
                        kind: "new_destination".to_owned(),
                        when_label: format!("AS{asn}"),
                        duration: name.clone().unwrap_or_else(|| human_bytes(bytes as f64)),
                        detail: serde_json::json!({ "asn": asn, "as_name": name, "bytes": bytes }),
                    });
                }
            }
            // Destination port novelty (noisier — score capped just under warning).
            let recent_ports = flows.top_ports(&recent_q).await.unwrap_or_default();
            let base_ports = flows.top_ports(&base_q).await.unwrap_or_default();
            let base_port_keys: HashSet<String> =
                base_ports.iter().map(|p| p.port.to_string()).collect();
            let recent_port_keys: Vec<String> =
                recent_ports.iter().map(|p| p.port.to_string()).collect();
            if let Some((port_str, rank)) = first_novel(&recent_port_keys, &base_port_keys) {
                let port: u16 = port_str.parse().unwrap_or(0);
                let bytes = recent_ports
                    .iter()
                    .find(|p| p.port == port)
                    .map_or(0, |p| p.bytes);
                if bytes >= DEST_FLOOR {
                    let score = novelty_score(rank).min(74.0);
                    findings.push(NewFinding {
                        score,
                        severity: severity_for(score).to_owned(),
                        node_id: Some(*node),
                        node_name: name_lookup(names, node),
                        metric: "dst_port".to_owned(),
                        kind: "new_destination".to_owned(),
                        when_label: format!("port {port}"),
                        duration: human_bytes(bytes as f64),
                        detail: serde_json::json!({ "port": port, "bytes": bytes }),
                    });
                }
            }
        }
        finalize(&mut findings);
        let summary = format!("{} new destination signals", findings.len());
        Ok(Some((findings, summary)))
    }

    // ── Engine: Flow Scan (flow) ──────────────────────────────────────────────────
    //
    // A source contacting an abnormal number of distinct destinations (horizontal) or destination
    // ports (vertical) — scan / worm behaviour, via the ClickHouse distinct-count fan-out.
    async fn run_flow_scan(
        &self,
        id: Uuid,
        params: &JobParams,
        node_ids: &[Uuid],
        names: &HashMap<Uuid, String>,
        cancel: &AtomicBool,
    ) -> anyhow::Result<Option<(Vec<NewFinding>, String)>> {
        let Some(flows) = self.flows.clone() else {
            return Ok(Some(flow_tier_off()));
        };
        let to = now_s();
        let from = to - params.window_secs.max(1800);
        let total = node_ids.len().max(1);
        let mut findings: Vec<NewFinding> = Vec::new();
        for (i, node) in node_ids.iter().enumerate() {
            if cancel.load(Ordering::Relaxed) {
                return Ok(None);
            }
            self.progress(id, 15 + (i * 70 / total) as i32, "Scanning fan-out…")
                .await;
            let q = FlowQuery {
                node_id: Some(*node),
                from_unix_ms: from * 1000,
                to_unix_ms: to * 1000,
                limit: 50,
                proto: None,
                dst_port: None,
                peer: None,
                asn: None,
            };
            let fan = flows.fanout_by_src(&q).await.unwrap_or_default();
            for f in fan {
                let Some(score) = scan_score(f.distinct_dst, f.distinct_ports) else {
                    continue;
                };
                let (kind_label, n) = if f.distinct_dst >= f.distinct_ports {
                    ("horizontal", f.distinct_dst)
                } else {
                    ("vertical", f.distinct_ports)
                };
                findings.push(NewFinding {
                    score,
                    severity: severity_for(score).to_owned(),
                    node_id: Some(*node),
                    node_name: name_lookup(names, node),
                    metric: "flow_fanout".to_owned(),
                    kind: "flow_scan".to_owned(),
                    when_label: format!("{} → {} dst", f.src, f.distinct_dst),
                    duration: format!("{kind_label} · {n}"),
                    detail: serde_json::json!({
                        "src": f.src, "distinct_dst": f.distinct_dst,
                        "distinct_ports": f.distinct_ports, "flows": f.flows,
                    }),
                });
            }
        }
        finalize(&mut findings);
        let summary = format!("{} scanning sources", findings.len());
        Ok(Some((findings, summary)))
    }

    // ── Engine: Saturation (cross-store) ──────────────────────────────────────────
    //
    // A single conversation dominating a busy node's traffic (link hog). Concentration comes from
    // the flow store; the node's current interface throughput (TSDB) is attached as context.
    async fn run_saturation(
        &self,
        id: Uuid,
        params: &JobParams,
        node_ids: &[Uuid],
        names: &HashMap<Uuid, String>,
        cancel: &AtomicBool,
    ) -> anyhow::Result<Option<(Vec<NewFinding>, String)>> {
        let Some(flows) = self.flows.clone() else {
            return Ok(Some(flow_tier_off()));
        };
        let to = now_s();
        let from = to - params.window_secs.max(900);
        let total = node_ids.len().max(1);
        let mut findings: Vec<NewFinding> = Vec::new();
        for (i, node) in node_ids.iter().enumerate() {
            if cancel.load(Ordering::Relaxed) {
                return Ok(None);
            }
            self.progress(
                id,
                15 + (i * 70 / total) as i32,
                "Checking traffic concentration…",
            )
            .await;
            let conv_q = FlowQuery {
                node_id: Some(*node),
                from_unix_ms: from * 1000,
                to_unix_ms: to * 1000,
                limit: 5,
                proto: None,
                dst_port: None,
                peer: None,
                asn: None,
            };
            let convos = flows.top_conversations(&conv_q).await.unwrap_or_default();
            let Some(top) = convos.first() else {
                continue;
            };
            let proto_q = FlowQuery {
                limit: 256,
                ..conv_q
            };
            let protos = flows.top_protocols(&proto_q).await.unwrap_or_default();
            let node_total = protos
                .iter()
                .map(|p| p.bytes)
                .sum::<u64>()
                .max(top.bytes)
                .max(1);
            let ratio = top.bytes as f64 / node_total as f64;
            let Some(score) = concentration_score(ratio) else {
                continue;
            };
            let iface_bps = self.node_throughput_bps(*node).await;
            findings.push(NewFinding {
                score,
                severity: severity_for(score).to_owned(),
                node_id: Some(*node),
                node_name: name_lookup(names, node),
                metric: "flow_concentration".to_owned(),
                kind: "saturation".to_owned(),
                when_label: format!("{:.0}% one flow", ratio * 100.0),
                duration: format!("{} → {}", top.src, top.dst),
                detail: serde_json::json!({
                    "src": top.src, "dst": top.dst, "conversation_bytes": top.bytes,
                    "node_bytes": node_total, "ratio": ratio, "interface_bps": iface_bps,
                }),
            });
        }
        finalize(&mut findings);
        let summary = format!("{} nodes with a dominant conversation", findings.len());
        Ok(Some((findings, summary)))
    }

    // ── Engine: Incident Correlate (cross-store) ──────────────────────────────────
    //
    // For each node, assemble a cross-signal timeline over the window: a reachability metric anomaly
    // (TSDB), passive events (events store), and the dominant flow (ClickHouse). Emit a finding only
    // when ≥2 signals of ≥2 distinct kinds coincide — the root-cause on-ramp (ADR-029). Single-node
    // for now; topology-neighbour expansion is a follow-up.
    async fn run_incident_correlate(
        &self,
        id: Uuid,
        params: &JobParams,
        node_ids: &[Uuid],
        names: &HashMap<Uuid, String>,
        cancel: &AtomicBool,
    ) -> anyhow::Result<Option<(Vec<NewFinding>, String)>> {
        let to = now_s();
        let window = params.window_secs.max(3600);
        let from = to - window;
        let nodes = &node_ids[..node_ids.len().min(INCIDENT_NODE_CAP)];
        let total = nodes.len().max(1);
        let mut findings: Vec<NewFinding> = Vec::new();
        for (i, node) in nodes.iter().enumerate() {
            if cancel.load(Ordering::Relaxed) {
                return Ok(None);
            }
            self.progress(
                id,
                15 + (i * 75 / total) as i32,
                "Assembling incident timeline…",
            )
            .await;
            let mut signals: Vec<IncidentSignal> = Vec::new();
            // 1) Reachability metric anomaly.
            let step = read_step(from, to);
            let rtt = self.gauge_range(*node, "icmp_rtt_ms", from, to, step).await;
            if rtt.len() >= MIN_POINTS {
                if let Some(a) = score_anomaly(
                    &rtt,
                    to - params.window_secs.max(600),
                    params.sensitivity.max(2.0),
                ) {
                    signals.push(IncidentSignal {
                        at_s: a.when_s,
                        severity: a.score,
                        kind: "metric",
                        label: format!("icmp_rtt_ms {}", a.kind),
                    });
                }
            }
            // 2) Passive events on the node.
            let filter = EventFilter {
                since: DateTime::from_timestamp(from, 0),
                node_id: Some(*node),
                ..Default::default()
            };
            let events = self
                .events
                .list_events(&filter, 20)
                .await
                .unwrap_or_default();
            for e in events.iter().take(8) {
                signals.push(IncidentSignal {
                    at_s: e.at_unix_ms / 1000,
                    severity: event_signal_severity(&e.action, e.syslog_severity),
                    kind: "event",
                    label: incident_event_label(e),
                });
            }
            // 3) Dominant flow conversation.
            if let Some(flows) = self.flows.clone() {
                let q = FlowQuery {
                    node_id: Some(*node),
                    from_unix_ms: from * 1000,
                    to_unix_ms: to * 1000,
                    limit: 1,
                    proto: None,
                    dst_port: None,
                    peer: None,
                    asn: None,
                };
                if let Ok(cs) = flows.top_conversations(&q).await {
                    if let Some(c) = cs.first() {
                        signals.push(IncidentSignal {
                            at_s: to,
                            severity: 40.0,
                            kind: "flow",
                            label: format!(
                                "top flow {} → {} ({})",
                                c.src,
                                c.dst,
                                human_bytes(c.bytes as f64)
                            ),
                        });
                    }
                }
            }
            // Cross-signal evidence required: ≥2 signals across ≥2 kinds.
            let kinds: HashSet<&str> = signals.iter().map(|s| s.kind).collect();
            if signals.len() < 2 || kinds.len() < 2 {
                continue;
            }
            signals.sort_by_key(|s| s.at_s);
            let score = signals.iter().map(|s| s.severity).fold(0.0, f64::max);
            let earliest = signals.first().map_or(to, |s| s.at_s);
            let timeline: Vec<serde_json::Value> = signals
                .iter()
                .map(|s| {
                    serde_json::json!({
                        "at": s.at_s, "kind": s.kind, "label": s.label, "severity": s.severity,
                    })
                })
                .collect();
            findings.push(NewFinding {
                score,
                severity: severity_for(score).to_owned(),
                node_id: Some(*node),
                node_name: name_lookup(names, node),
                metric: "incident".to_owned(),
                kind: "incident_correlate".to_owned(),
                when_label: rel_label(earliest, to),
                duration: format!("{} signals", signals.len()),
                detail: serde_json::json!({ "timeline": timeline }),
            });
        }
        finalize(&mut findings);
        let summary = format!("{} correlated incidents", findings.len());
        Ok(Some((findings, summary)))
    }

    /// Resolve an AS number to its organization name via the hot-swappable IP→ASN table (`None`
    /// when the table is unloaded or the ASN is unknown/0).
    fn resolve_as_name(&self, asn: u32) -> Option<String> {
        if asn == 0 {
            return None;
        }
        let db = self.ipasn.read().ok()?.clone();
        db.and_then(|d| d.name_of(asn).map(str::to_owned))
    }

    /// Best-effort current total interface throughput (bits/sec) for a node from the TSDB — context
    /// for the saturation finding. `None` when the node has no interface series.
    async fn node_throughput_bps(&self, node: Uuid) -> Option<f64> {
        let live = self.store.node_interface_live(node, 300).await;
        if live.is_empty() {
            return None;
        }
        let bytes: f64 = live
            .values()
            .map(|v| v.in_bps.unwrap_or(0.0) + v.out_bps.unwrap_or(0.0))
            .sum();
        Some(bytes * 8.0)
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

// ── Event/flow analysis maths + constants (ADR-022 event/flow increment, unit-tested) ──

/// Bucket width for event-storm volume counting (seconds).
const EVENT_BUCKET_SECS: i64 = 300;
/// Minimum peak-bucket event count before an event storm is worth reporting.
const EVENT_STORM_FLOOR: f64 = 5.0;
/// Minimum recent syslog volume before a severity shift is meaningful.
const SEVERITY_FLOOR: i64 = 10;
/// Minimum count for an unmatched signature to count as a rule gap.
const RULE_GAP_FLOOR: i64 = 20;
/// Minimum auth-failure count from a source before flagging.
const AUTH_FLOOR: i64 = 5;
/// Minimum bytes a novel talker must carry to be a real shift (1 MB).
const TALKER_FLOOR: u64 = 1_000_000;
/// Minimum bytes a novel destination must carry (0.5 MB).
const DEST_FLOOR: u64 = 500_000;
/// Hard node cap for the per-node multi-store incident correlation.
const INCIDENT_NODE_CAP: usize = 20;

/// Per-node split of event-bucket counts for `event_storm`: (baseline counts, recent (bucket, count)).
type StormBuckets = (Vec<f64>, Vec<(i64, f64)>);

/// One dated signal on an incident timeline (`incident_correlate`).
struct IncidentSignal {
    at_s: i64,
    severity: f64,
    kind: &'static str,
    label: String,
}

/// The flow-tier-off result: a single info finding + summary (mirrors `top_flows`' availability note).
fn flow_tier_off() -> (Vec<NewFinding>, String) {
    (
        vec![info_finding("flow", "flow tier not enabled on this core")],
        "flow tier not enabled".to_owned(),
    )
}

/// A zero-score info finding carrying a note (used for the flow-tier-off case).
fn info_finding(metric: &str, note: &str) -> NewFinding {
    NewFinding {
        score: 0.0,
        severity: "info".to_owned(),
        node_id: None,
        node_name: "—".to_owned(),
        metric: metric.to_owned(),
        kind: "info".to_owned(),
        when_label: String::new(),
        duration: String::new(),
        detail: serde_json::json!({ "note": note }),
    }
}

/// Sort findings by score (highest first) and cap at [`MAX_FINDINGS`].
fn finalize(findings: &mut Vec<NewFinding>) {
    findings.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    findings.truncate(MAX_FINDINGS);
}

/// Upward-spike score: how far a recent peak exceeds the baseline mean, in σ, mapped to 0..100 (at
/// the σ threshold → 75; ~1.5× → ~100). `None` if within threshold or the baseline is empty. Unlike
/// [`score_anomaly`] this is one-sided (only *more* volume/traffic matters for storms/DDoS).
fn burst_score(baseline: &[f64], recent_peak: f64, sigma: f64) -> Option<f64> {
    if baseline.is_empty() {
        return None;
    }
    let m = mean(baseline);
    let sd = stddev(baseline, m).max(m.max(1.0) * 1e-3);
    let sig = sigma.max(0.5);
    let z = (recent_peak - m) / sd;
    if z < sig {
        return None;
    }
    Some((75.0 * z / sig).clamp(0.0, 100.0))
}

/// Per-node high-severity (syslog ≤ 3: err/crit/alert/emerg) fraction, restricted to `scope`.
/// Returns `node → (high_count, total_count, fraction)`.
fn severity_high_fractions(
    counts: &[EventSeverityCount],
    scope: &HashSet<Uuid>,
) -> HashMap<Uuid, (i64, i64, f64)> {
    let mut acc: HashMap<Uuid, (i64, i64)> = HashMap::new();
    for c in counts {
        if !scope.contains(&c.node_id) {
            continue;
        }
        let e = acc.entry(c.node_id).or_default();
        e.1 += c.count;
        if c.severity <= 3 {
            e.0 += c.count;
        }
    }
    acc.into_iter()
        .map(|(k, (high, total))| {
            let frac = if total > 0 {
                high as f64 / total as f64
            } else {
                0.0
            };
            (k, (high, total, frac))
        })
        .collect()
}

/// Severity-shift score from the baseline vs recent high-severity fraction. `None` if the recent
/// mix didn't skew meaningfully more toward error/critical.
fn severity_shift_score(baseline_frac: f64, recent_frac: f64) -> Option<f64> {
    let delta = recent_frac - baseline_frac;
    if delta < 0.15 {
        return None;
    }
    Some((60.0 + delta * 60.0).clamp(0.0, 100.0))
}

/// Score an unmatched-signature volume (rule-coverage gap). Capped at warning — advice, not an outage.
fn gap_score(count: i64) -> f64 {
    if count >= 500 {
        80.0
    } else if count >= 100 {
        72.0
    } else {
        60.0
    }
}

/// Score an auth-failure volume from one source.
fn auth_score(count: i64) -> f64 {
    if count >= 50 {
        90.0
    } else if count >= 10 {
        78.0
    } else {
        62.0
    }
}

/// The highest-ranked recent key absent from the baseline set, with its 0-based rank.
fn first_novel(recent: &[String], baseline: &HashSet<String>) -> Option<(String, usize)> {
    recent
        .iter()
        .enumerate()
        .find(|(_, k)| !baseline.contains(*k))
        .map(|(i, k)| (k.clone(), i))
}

/// Novelty score by the rank a new key entered at (a brand-new #1 is the strongest signal).
fn novelty_score(rank: usize) -> f64 {
    match rank {
        0 => 82.0,
        1 => 74.0,
        2 => 66.0,
        _ => 55.0,
    }
}

/// Scan score from a source's distinct destination / port fan-out. `None` below the scan floor.
fn scan_score(distinct_dst: u64, distinct_ports: u64) -> Option<f64> {
    let d = distinct_dst.max(distinct_ports);
    if d < 50 {
        return None;
    }
    if d >= 500 {
        Some(92.0)
    } else if d >= 150 {
        Some(80.0)
    } else {
        Some(66.0)
    }
}

/// Concentration score from the top conversation's share of a node's traffic. `None` below 50%.
fn concentration_score(top_ratio: f64) -> Option<f64> {
    if top_ratio < 0.5 {
        return None;
    }
    Some((60.0 + (top_ratio - 0.5) * 80.0).clamp(0.0, 100.0))
}

/// Human byte size (`1.2GB`, `512B`) for flow-finding labels.
fn human_bytes(b: f64) -> String {
    const U: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = b.max(0.0);
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{v:.0}{}", U[i])
    } else {
        format!("{v:.1}{}", U[i])
    }
}

/// Severity weight for a passive event on an incident timeline: a fired alert is strongest, else
/// scale by syslog severity.
fn event_signal_severity(action: &str, syslog_severity: Option<i16>) -> f64 {
    match action {
        "fired" => 85.0,
        "refreshed" => 70.0,
        "cleared" => 60.0,
        _ => match syslog_severity {
            Some(s) if s <= 2 => 80.0,
            Some(3) => 65.0,
            Some(4) => 50.0,
            _ => 35.0,
        },
    }
}

/// A compact label for one event on an incident timeline (trap/app name + clipped message).
fn incident_event_label(e: &EventRow) -> String {
    let head = e
        .trap_name
        .clone()
        .or_else(|| e.app_name.clone())
        .unwrap_or_else(|| e.kind.clone());
    let msg: String = e.message.chars().take(60).collect();
    format!("{head}: {msg}")
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
            AnalysisTool::EventStorm,
            AnalysisTool::EventFlap,
            AnalysisTool::SeverityShift,
            AnalysisTool::RuleGap,
            AnalysisTool::AuthProbe,
            AnalysisTool::TrafficAnomaly,
            AnalysisTool::TalkerShift,
            AnalysisTool::NewDestination,
            AnalysisTool::FlowScan,
            AnalysisTool::Saturation,
            AnalysisTool::IncidentCorrelate,
        ] {
            assert_eq!(AnalysisTool::from_str(t.as_str()), Some(t));
        }
        assert_eq!(AnalysisTool::from_str("nope"), None);
    }

    #[test]
    fn burst_score_flags_upward_spike_only() {
        let baseline = vec![10.0, 12.0, 11.0, 9.0, 10.0, 11.0];
        // A big upward peak past 3σ scores; a value within the baseline does not.
        assert!(burst_score(&baseline, 60.0, 3.0).is_some());
        assert!(burst_score(&baseline, 11.0, 3.0).is_none());
        // One-sided: a drop below the mean is never a burst.
        assert!(burst_score(&baseline, 0.0, 3.0).is_none());
        assert!(burst_score(&[], 100.0, 3.0).is_none());
    }

    #[test]
    fn severity_shift_needs_a_real_skew() {
        assert!(severity_shift_score(0.1, 0.15).is_none()); // +0.05, below threshold
        let s = severity_shift_score(0.1, 0.6).unwrap(); // +0.5 skew
        assert!(s > 75.0);
    }

    #[test]
    fn severity_high_fractions_counts_err_and_worse() {
        let scope: HashSet<Uuid> = [Uuid::from_u128(1)].into_iter().collect();
        let counts = vec![
            EventSeverityCount {
                node_id: Uuid::from_u128(1),
                severity: 3,
                count: 3,
            }, // err → high
            EventSeverityCount {
                node_id: Uuid::from_u128(1),
                severity: 6,
                count: 7,
            }, // info → not high
            EventSeverityCount {
                node_id: Uuid::from_u128(9),
                severity: 0,
                count: 100,
            }, // out of scope
        ];
        let f = severity_high_fractions(&counts, &scope);
        let (high, total, frac) = f[&Uuid::from_u128(1)];
        assert_eq!((high, total), (3, 10));
        assert!((frac - 0.3).abs() < 1e-9);
        assert!(!f.contains_key(&Uuid::from_u128(9)));
    }

    #[test]
    fn first_novel_finds_highest_ranked_new_key() {
        let baseline: HashSet<String> = ["a".to_owned(), "b".to_owned()].into_iter().collect();
        let recent = vec!["a".to_owned(), "z".to_owned(), "b".to_owned()];
        assert_eq!(first_novel(&recent, &baseline), Some(("z".to_owned(), 1)));
        // Nothing new → None.
        let recent2 = vec!["a".to_owned(), "b".to_owned()];
        assert_eq!(first_novel(&recent2, &baseline), None);
    }

    #[test]
    fn scan_and_concentration_thresholds() {
        assert!(scan_score(10, 5).is_none());
        assert_eq!(scan_score(600, 3), Some(92.0));
        assert_eq!(scan_score(3, 200), Some(80.0)); // vertical scan on the port axis
        assert!(concentration_score(0.4).is_none());
        assert_eq!(concentration_score(1.0), Some(100.0));
    }

    #[test]
    fn human_bytes_scales_units() {
        assert_eq!(human_bytes(512.0), "512B");
        assert_eq!(human_bytes(1536.0), "1.5KB");
        assert_eq!(human_bytes(5.0 * 1024.0 * 1024.0 * 1024.0), "5.0GB");
    }

    #[test]
    fn event_signal_severity_ranks_fired_highest() {
        assert!(event_signal_severity("fired", None) > event_signal_severity("cleared", None));
        assert!(
            event_signal_severity("none", Some(0)) > event_signal_severity("none", Some(6)),
            "emergency syslog outweighs debug"
        );
    }

    #[test]
    fn charge_window_admits_under_cap_and_rejects_at_cap() {
        let mut q = VecDeque::new();
        let win = Duration::from_secs(60);
        let t0 = Instant::now();
        assert!(charge_window(&mut q, t0, win, 2), "1st admitted");
        assert!(charge_window(&mut q, t0, win, 2), "2nd admitted");
        assert!(!charge_window(&mut q, t0, win, 2), "3rd at cap rejected");
        assert_eq!(q.len(), 2);
    }

    #[test]
    fn charge_window_prunes_expired_then_readmits() {
        let mut q = VecDeque::new();
        let win = Duration::from_secs(60);
        let t0 = Instant::now();
        assert!(charge_window(&mut q, t0, win, 1));
        assert!(
            !charge_window(&mut q, t0, win, 1),
            "still within window ⇒ full"
        );
        // Advance past the window: the old entry is pruned and a new one is admitted.
        let t1 = t0 + Duration::from_secs(61);
        assert!(
            charge_window(&mut q, t1, win, 1),
            "expired entry pruned ⇒ readmit"
        );
        assert_eq!(q.len(), 1);
    }

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

    #[test]
    fn env_cap_falls_back_when_unset() {
        // An unset var yields the default; a zero/invalid value would too (filtered out).
        assert_eq!(env_cap("YAGRA_ANALYSIS_CAP_DEFINITELY_UNSET_XYZ", 4), 4);
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
