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
    /// Every tool, grouped by the store it reads: metric, passive-event, flow, then cross-store.
    ///
    /// This is the enumeration, and [`AnalysisTool::from_str`] is derived from it. It used to be a
    /// second hand-written `match` listing all fifteen tokens, with a third copy in the MCP tool's
    /// description text — so adding a tool meant remembering three places, and forgetting the
    /// parser meant the API silently rejected a tool the UI offered.
    pub const ALL: [AnalysisTool; 15] = [
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
    ];

    /// The valid tokens, comma-separated — for the "must be one of…" half of a rejection message.
    #[must_use]
    pub fn token_list() -> String {
        Self::ALL
            .iter()
            .map(|t| t.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }

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

    /// Parse the API/DB token back into a tool. Derived from [`AnalysisTool::ALL`], so a variant
    /// that reaches `as_str` is parseable by construction.
    #[must_use]
    pub fn from_str(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|t| t.as_str() == s)
    }

    /// Whether this analysis reads the flow store (ClickHouse, ADR-031).
    ///
    /// Exhaustive rather than a `matches!` over the flow group, because a wildcard is what lets a
    /// new tool ship on the wrong side of this question (`extensibility.md` §1). The answer must
    /// agree with which `run_*` short-circuits to `flow_tier_off()` — a test pins that by reading
    /// this file.
    #[must_use]
    pub const fn needs_flow_tier(self) -> bool {
        match self {
            AnalysisTool::TrafficAnomaly
            | AnalysisTool::TalkerShift
            | AnalysisTool::NewDestination
            | AnalysisTool::FlowScan
            | AnalysisTool::Saturation => true,
            AnalysisTool::Anomaly
            | AnalysisTool::Correlation
            | AnalysisTool::Capacity
            | AnalysisTool::Flap
            | AnalysisTool::EventStorm
            | AnalysisTool::EventFlap
            | AnalysisTool::SeverityShift
            | AnalysisTool::RuleGap
            | AnalysisTool::AuthProbe
            // Cross-store: reads flow when it is there and simply omits that signal when it is
            // not, so it stays useful with the tier off — unlike the five above, which have
            // nothing left to say.
            | AnalysisTool::IncidentCorrelate => false,
        }
    }
}

// Lifecycle states are stored as text in `analysis_jobs.state`:
// running | done | failed | cancelled (set via the repo's UPDATE statements).

/// Which nodes an analysis runs over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

/// Where an analysis run is in its lifecycle.
///
/// A bare `String` until v0.2.6, and the cost of that was paid twice. The vocabulary had to be
/// written out by hand at the API edge to validate the runs filter, and again in the WebUI to fill
/// its dropdown — two lists nothing compared. Worse, `state: string` compares equal to *any*
/// string: the in-app "your analysis finished" notice tested `state === 'succeeded'`, which is the
/// **report-run** vocabulary, so every successful analysis announced itself as a failure. Typing it
/// makes that comparison a compile error on both sides.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AnalysisJobState {
    /// Accepted and waiting on admission control. Not written by any statement today — a run is
    /// inserted `running` — but it is a state the column may hold and an operator may filter for.
    Queued,
    /// In flight. The only state `set_progress` / `mark_cancelled` will act on.
    Running,
    /// Finished and produced its findings. Note the token: **`done`**, not `succeeded`.
    Done,
    /// Finished with a reason it could not.
    Failed,
    /// Stopped by an operator, or by a core restart tripping its cancel flag.
    Cancelled,
    /// A state this build does not know — a newer core wrote it.
    Unknown,
}

crate::stored_enum::token_enum!(AnalysisJobState, Unknown, "analysis_jobs.state", [
    Queued => "queued",
    Running => "running",
    Done => "done",
    Failed => "failed",
    Cancelled => "cancelled",
    Unknown => "unknown",
]);

impl AnalysisJobState {
    /// Whether the run has stopped moving — nothing will change this row again.
    ///
    /// [`Self::Unknown`] counts as terminal on purpose. Anything waiting on a run polls until this
    /// says yes, and a state this build cannot read is one it will never learn to read, so calling
    /// it "still running" is an infinite wait.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        match self {
            Self::Queued | Self::Running => false,
            Self::Done | Self::Failed | Self::Cancelled | Self::Unknown => true,
        }
    }

    /// Parse a filter token, refusing anything outside the writable vocabulary.
    #[must_use]
    pub fn from_filter_token(s: &str) -> Option<Self> {
        crate::stored_enum::parse_filter_token(Self::ALL, Self::Unknown, Self::as_str, s)
    }

    /// The filterable tokens, for the 400 that names them. Mirrors `AnalysisTool::token_list`.
    #[must_use]
    pub fn filter_token_list() -> String {
        crate::stored_enum::filter_token_list(Self::ALL, Self::Unknown, Self::as_str)
    }
}

/// A job row, as served to the API / SSE. Timestamps are epoch-millis so the WebUI formats
/// relative times without a date dependency.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct AnalysisJob {
    pub id: Uuid,
    pub tool: String,
    pub scope_kind: String,
    pub scope_id: Option<Uuid>,
    pub scope_label: String,
    pub params: serde_json::Value,
    pub state: AnalysisJobState,
    pub pct: i32,
    pub phase: Option<String>,
    pub finding_count: i32,
    pub summary: Option<String>,
    pub error: Option<String>,
    pub created_ms: i64,
    pub started_ms: Option<i64>,
    pub finished_ms: Option<i64>,
}

/// One analysis-stream frame: the run's own scope beside its already-serialized JSON.
///
/// Mirrors [`crate::alerts::StreamFrame`] and exists for the same reason — a group-scoped SSE
/// subscriber has to be filtered per frame, and re-parsing the body to recover a field the sender
/// already held would be waste. `scope_kind` is `None` for a persisted value this build does not
/// recognise, which the API edge reads as unbounded (fail-closed).
pub type JobFrame = (Option<ScopeKind>, Option<Uuid>, std::sync::Arc<str>);

/// One finding produced by an analysis (anomaly card / correlation pair / capacity / flap row).
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
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

// ⚠️ Every `///` on this type is published verbatim to API clients through the OpenAPI document
// (ADR-035), so the rationale below is a `//` comment. The same slip shipped once already
// (commit a7e0180) and had to be reverted out of the generated contract.
//
// The row deliberately omits the run's `scope_label`. A label is a node or group *name*, and a
// finding can outlive the grouping its run was launched over — a node moved between folders after
// the run finished is visible to whoever holds its new group, while the run's label still names the
// old one. `job_id` + `tool` is enough to link to the report, and the report is itself gated on the
// run row (`GET /analysis/jobs/{id}` answers 404 out of scope), so the link stays honest without
// this row carrying somebody else's group name.
//
// `tool` and `severity` are strings rather than enums because a row written by a newer core may
// name a tool this build has never heard of, and dropping such a row would be a worse answer than
// showing its raw key.
/// One finding as the cross-run search returns it: the finding, plus the run it came from.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct SavedFinding {
    pub id: Uuid,
    /// The analysis run that produced this finding.
    pub job_id: Uuid,
    /// Which diagnostic produced it, as that run recorded it (e.g. `anomaly`).
    pub tool: String,
    pub score: f64,
    /// `crit`, `warn` or `info`.
    pub severity: String,
    /// The node this finding is about; absent for a fleet-level finding.
    pub node_id: Option<Uuid>,
    pub node_name: String,
    pub metric: String,
    pub kind: String,
    pub when_label: String,
    pub duration: String,
    /// When the finding was written (RFC 3339). Pass it back as `before`, with `id` as
    /// `before_id`, to fetch the next page.
    pub at: String,
}

// ⚠️ `ToSchema` — every `///` below is published verbatim to API clients (ADR-035). Rationale goes
// in `//` comments like this one.
//
// `Busy` is the variant `ReportScheduleStatus` has no equivalent for, and it is the whole reason
// this is its own enum rather than a reuse of that one. The analysis runner has admission control
// (`YAGRA_ANALYSIS_MAX_CONCURRENT`, `YAGRA_ANALYSIS_RATE_PER_MIN`), so a fire can be *refused*
// rather than failing — and a refusal that advanced `next_run_at` would silently skip a whole
// period, leaving a schedule that looks healthy and simply produced no run that day.
/// Outcome of a schedule's most recent firing attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AnalysisScheduleStatus {
    /// A run was launched.
    Queued,
    /// Admission control was full; the schedule stays due and the next tick retries.
    Busy,
    /// Launching failed for a reason retrying will not fix.
    Error,
    /// A status this build does not know — a newer core wrote it.
    Unknown,
}

crate::stored_enum::token_enum!(AnalysisScheduleStatus, Unknown, "analysis_schedules.last_status", [
    Queued => "queued",
    Busy => "busy",
    Error => "error",
    Unknown => "unknown",
]);

/// A schedule row, as served to the API.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct AnalysisSchedule {
    pub id: Uuid,
    /// Which diagnostic runs, as an `AnalysisTool` token.
    pub tool: String,
    /// `all` | `group` | `node`.
    pub scope_kind: String,
    pub scope_id: Option<Uuid>,
    pub scope_label: String,
    /// The launch knobs, the same shape as `AnalysisJob.params`.
    pub params: serde_json::Value,
    pub frequency: crate::cadence::Cadence,
    pub day_of_week: Option<i16>,
    pub day_of_month: Option<i16>,
    pub at_hour: i16,
    pub at_minute: i16,
    pub enabled: bool,
    pub next_run_ms: i64,
    pub last_run_ms: Option<i64>,
    pub last_status: Option<AnalysisScheduleStatus>,
}

/// Validated fields for creating/updating an analysis schedule (parsed at the API edge).
#[derive(Debug, Clone)]
pub struct ScheduleInput {
    /// The launch spec, already validated and clamped by the same `job_params` the manual launch
    /// path uses — so a schedule cannot be saved with a window a direct launch would refuse.
    pub params: JobParams,
    pub cadence: crate::cadence::Schedule,
    pub enabled: bool,
}

/// One page of the cross-run findings search.
///
/// The cursor is `(before, before_id)` and not `before` alone. A run writes its findings in a tight
/// loop, so several rows routinely share a millisecond — and with a timestamp-only cursor the rows
/// sharing the boundary instant are either repeated forever or skipped without trace, depending on
/// whether the comparison is `<` or `<=`. The id makes the ordering total.
#[derive(Debug, Clone, Default)]
pub struct FindingSearch<'a> {
    /// Page cursor: the `at`/`id` of the last row of the previous page. `before_id` may be omitted,
    /// which reads as "strictly before this instant" rather than "before this row".
    pub before: Option<DateTime<Utc>>,
    pub before_id: Option<Uuid>,
    /// Inclusive lower bound on the finding's own timestamp (the range filter, not the cursor).
    pub since: Option<DateTime<Utc>>,
    /// Any of these diagnostics. Empty means unfiltered — never an empty array in SQL, which
    /// `= ANY(…)` would match nothing against.
    pub tool: &'a [AnalysisTool],
    /// Any of [`FINDING_SEVERITIES`]; validated at the API edge. Empty means unfiltered.
    pub severity: &'a [&'a str],
    /// Case-insensitive substring of **either** the metric name or the finding kind.
    ///
    /// Both, because the Saved-findings *What* column renders both and a filter mounted on a column
    /// should match what that column shows. The same shape as the audit log's `q` over username and
    /// action — and the same imprecision, stated rather than hidden: a term can match on the half
    /// the operator was not thinking of.
    pub q: Option<&'a str>,
    /// Restrict to findings about one node.
    pub node_id: Option<Uuid>,
    /// Case-insensitive substring of the node's **current** name. Fleet-wide findings have no node
    /// and are therefore excluded whenever this is set, which is correct: they are not about a node
    /// whose name could match.
    pub node_q: Option<&'a str>,
    /// The **caller's** group scope (ADR-014). `None` is unrestricted; `Some(&[])` matches nothing.
    pub groups: crate::repo::GroupFilter<'a>,
    /// The folder-group subtree the caller asked to filter *by* — a request, unlike `groups`.
    pub in_group: Option<&'a [Uuid]>,
    /// Inclusive lower / upper bound on the finding's score (ADR-053 Inc.6's numeric column).
    ///
    /// Inclusive at both ends deliberately: an operator asking for 3–5 and not seeing the rows that
    /// score exactly 3 or exactly 5 reads as missing data, not as a boundary convention. Either side
    /// may be `None`, and one-sided is the common shape — "8 or worse" is a bound, not a window.
    pub min_score: Option<f64>,
    pub max_score: Option<f64>,
    pub limit: i64,
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

/// The three severity buckets a finding can carry, most severe first.
///
/// One list, because there are now two readers: [`severity_for`] writes them and the Saved-findings
/// search validates `?severity=` against them. A hand-written copy of the set at the API edge is the
/// duplicated-constant trap `extensibility.md` names — and the copy that drifts would be the one
/// that decides which values a client is allowed to ask for.
pub const FINDING_SEVERITIES: [&str; 3] = [SEV_CRIT, SEV_WARN, SEV_INFO];
const SEV_CRIT: &str = "crit";
const SEV_WARN: &str = "warn";
const SEV_INFO: &str = "info";

/// Severity bucket from a 0..100 score (matches the WebUI: ≥90 crit, ≥75 warn, else info).
fn severity_for(score: f64) -> &'static str {
    if score >= 90.0 {
        SEV_CRIT
    } else if score >= 75.0 {
        SEV_WARN
    } else {
        SEV_INFO
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
/// What narrows the runs list. Every field optional; all of them ANDed.
///
/// A struct rather than three `Option` parameters, because the order of three optionals is exactly
/// the call-site mistake that compiles, runs, and answers a different question.
#[derive(Debug, Default, Clone, Copy)]
pub struct JobFilter<'a> {
    /// Only runs of this tool. Validated against [`AnalysisTool`] at the API edge.
    pub tool: Option<&'a str>,
    /// Only runs in this state. Typed, so the vocabulary the filter accepts is the vocabulary the
    /// writers produce — there is no second list to keep in step.
    pub state: Option<AnalysisJobState>,
    /// Only runs started at or after this instant.
    pub since: Option<DateTime<Utc>>,
}

/// The runs filter's predicate: one const, every clause always present, every value a nullable
/// bind. Not assembled conditionally — a `WHERE` built by pushing clauses has a branch per filter
/// that can be forgotten, and a forgotten one fails open, showing runs the operator did not ask for.
const JOB_FILTER_WHERE: &str = "($1::text IS NULL OR tool = $1) \
     AND ($2::text IS NULL OR state = $2) \
     AND ($3::timestamptz IS NULL OR created_at >= $3)";

const JOB_COLS: &str = "id, tool, scope_kind, scope_id, scope_label, params, state, pct, phase, \
     finding_count, summary, error, \
     (EXTRACT(EPOCH FROM created_at) * 1000)::bigint AS created_ms, \
     (EXTRACT(EPOCH FROM started_at) * 1000)::bigint AS started_ms, \
     (EXTRACT(EPOCH FROM finished_at) * 1000)::bigint AS finished_ms";

/// The `WHERE` of the cross-run findings search — one always-present clause per filter, `NULL`
/// meaning "no filter".
///
/// Written this way rather than appended per filter because of `$7`, the caller's group scope: a
/// conditionally-added restriction has a branch that can be forgotten, and forgetting *that* one
/// fails **open**, returning the whole fleet's findings. `NodeRepo::SCOPE_PREDICATE` and
/// `EVENT_FILTER_WHERE` are the same shape for the same reason.
///
/// ⚠️ `$7` and `$8` look alike and are not alike. `$7` is what the caller **may** see and is never
/// optional; `$8` is the group they **asked** to narrow to, and is dropped when they don't. Binding
/// a request into `$7` would let a caller widen their own scope by omitting a query parameter.
///
/// A finding with no `node_id` (the flow-tier-off notice, a fleet-level summary row) matches
/// neither group clause — `NULL IN (…)` is never true — so it is visible only to a caller with no
/// group restriction at all. That is the same rule the per-job endpoint applies in Rust, and the
/// same one `Scope::allows` applies to an ungrouped node.
const FINDING_SEARCH_WHERE: &str = "\
     ($1::timestamptz IS NULL OR (f.created_at, f.id) < \
        ($1, coalesce($2::uuid, '00000000-0000-0000-0000-000000000000'::uuid))) \
     AND ($3::timestamptz IS NULL OR f.created_at >= $3) \
     AND ($4::text[] IS NULL OR j.tool = ANY($4)) \
     AND ($5::text[] IS NULL OR f.severity = ANY($5)) \
     AND ($6::uuid IS NULL OR f.node_id = $6) \
     AND ($7::uuid[] IS NULL OR f.node_id IN (SELECT id FROM nodes WHERE group_id = ANY($7))) \
     AND ($8::uuid[] IS NULL OR f.node_id IN (SELECT id FROM nodes WHERE group_id = ANY($8))) \
     AND ($9::text IS NULL \
          OR (f.metric ILIKE '%' || $9 || '%' OR f.kind ILIKE '%' || $9 || '%')) \
     AND ($10::text IS NULL \
          OR f.node_id IN (SELECT id FROM nodes WHERE name ILIKE '%' || $10 || '%')) \
     AND ($11::double precision IS NULL OR f.score >= $11) \
     AND ($12::double precision IS NULL OR f.score <= $12)";

/// The cross-run findings query. `ORDER BY` matches the cursor in [`FINDING_SEARCH_WHERE`] column
/// for column, and both match `analysis_findings_created_idx` (migration 0058) — if those three
/// ever disagree the paging silently drops rows, which is why a test pins them together.
fn finding_search_sql() -> String {
    format!(
        "SELECT f.id, f.job_id, j.tool, f.score, f.severity, f.node_id, f.node_name, \
         f.metric, f.kind, f.when_label, f.duration, f.created_at \
         FROM analysis_findings f JOIN analysis_jobs j ON j.id = f.job_id \
         WHERE {FINDING_SEARCH_WHERE} \
         ORDER BY f.created_at DESC, f.id DESC LIMIT ${}",
        FINDING_SEARCH_BINDS + 1
    )
}

/// How many placeholders [`FINDING_SEARCH_WHERE`] uses. The page size is the one *after* them.
///
/// Derived rather than written twice, for the reason `EVENT_FILTER_BINDS` records: renumbering by
/// hand after widening the predicate is neither a compile error nor a crash — the page size lands in
/// a filter's slot and the query answers a different question. Here that would be `LIMIT` binding
/// into `max_score`, i.e. "findings scoring at most 100" returned unpaged.
const FINDING_SEARCH_BINDS: usize = 12;

/// Columns selected for a schedule row (timestamps projected to epoch-millis, as the job rows are).
const SCHED_COLS: &str = "id, tool, scope_kind, scope_id, scope_label, params, frequency, \
     day_of_week, day_of_month, at_hour, at_minute, enabled, last_status, \
     (EXTRACT(EPOCH FROM next_run_at) * 1000)::bigint AS next_run_ms, \
     (EXTRACT(EPOCH FROM last_run_at) * 1000)::bigint AS last_run_ms";

fn sched_from_row(row: &sqlx::postgres::PgRow) -> anyhow::Result<AnalysisSchedule> {
    Ok(AnalysisSchedule {
        id: row.try_get("id")?,
        tool: row.try_get("tool")?,
        scope_kind: row.try_get("scope_kind")?,
        scope_id: row.try_get("scope_id")?,
        scope_label: row.try_get("scope_label")?,
        params: row.try_get("params")?,
        frequency: crate::cadence::Cadence::from_stored(row.try_get("frequency")?),
        day_of_week: row.try_get("day_of_week")?,
        day_of_month: row.try_get("day_of_month")?,
        at_hour: row.try_get("at_hour")?,
        at_minute: row.try_get("at_minute")?,
        enabled: row.try_get("enabled")?,
        next_run_ms: row.try_get("next_run_ms")?,
        last_run_ms: row.try_get("last_run_ms")?,
        last_status: row
            .try_get::<Option<String>, _>("last_status")?
            .as_deref()
            .map(AnalysisScheduleStatus::from_stored),
    })
}

fn job_from_row(row: &sqlx::postgres::PgRow) -> anyhow::Result<AnalysisJob> {
    Ok(AnalysisJob {
        id: row.try_get("id")?,
        tool: row.try_get("tool")?,
        scope_kind: row.try_get("scope_kind")?,
        scope_id: row.try_get("scope_id")?,
        scope_label: row.try_get("scope_label")?,
        params: row.try_get("params")?,
        state: AnalysisJobState::from_stored(row.try_get::<String, _>("state")?.as_str()),
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
             VALUES ($1, $2, $3, $4, $5, $6, '{}', 0, $7, 0, $8, now()) \
             RETURNING {JOB_COLS}",
            AnalysisJobState::Running.as_str()
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
        sqlx::query(&format!(
            "UPDATE analysis_jobs SET pct = $2, phase = $3 WHERE id = $1 AND state = '{}'",
            AnalysisJobState::Running.as_str()
        ))
        .bind(id)
        .bind(pct.clamp(0, 100))
        .bind(phase)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Mark a job done with its result summary (findings inserted separately).
    pub async fn finish(&self, id: Uuid, finding_count: i32, summary: &str) -> anyhow::Result<()> {
        sqlx::query(&format!(
            "UPDATE analysis_jobs SET state = '{}', pct = 100, phase = NULL, \
             finding_count = $2, summary = $3, finished_at = now() WHERE id = $1",
            AnalysisJobState::Done.as_str()
        ))
        .bind(id)
        .bind(finding_count)
        .bind(summary)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Mark a job failed with a reason.
    pub async fn fail(&self, id: Uuid, error: &str) -> anyhow::Result<()> {
        sqlx::query(&format!(
            "UPDATE analysis_jobs SET state = '{}', phase = NULL, error = $2, \
             finished_at = now() WHERE id = $1",
            AnalysisJobState::Failed.as_str()
        ))
        .bind(id)
        .bind(error)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Mark a job cancelled (set by the runner when its cancel flag was tripped).
    pub async fn mark_cancelled(&self, id: Uuid) -> anyhow::Result<()> {
        sqlx::query(&format!(
            "UPDATE analysis_jobs SET state = '{}', phase = NULL, finished_at = now() \
             WHERE id = $1 AND state = '{}'",
            AnalysisJobState::Cancelled.as_str(),
            AnalysisJobState::Running.as_str()
        ))
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Drop analysis runs older than `retention_secs`, and with them their findings.
    ///
    /// `retention::Subject::AnalysisRuns`. Migration 0026 shipped this table with "no auto-trim
    /// yet" written into it, and 0059 later added *scheduled* analyses — so the table nothing
    /// pruned became one that fills on a cadence. `analysis_findings` needs no statement of its
    /// own: it is `ON DELETE CASCADE` from here, which is also why the cascade never fired before.
    pub async fn prune_jobs(&self, retention_secs: i64) -> anyhow::Result<u64> {
        let res = sqlx::query(
            "DELETE FROM analysis_jobs WHERE created_at < now() - make_interval(secs => $1)",
        )
        .bind(retention_secs as f64)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected())
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
    pub async fn list(
        &self,
        limit: i64,
        filter: &JobFilter<'_>,
    ) -> anyhow::Result<Vec<AnalysisJob>> {
        let rows = sqlx::query(&format!(
            "SELECT {JOB_COLS} FROM analysis_jobs WHERE {JOB_FILTER_WHERE} \
             ORDER BY created_at DESC LIMIT $4"
        ))
        .bind(filter.tool.map(str::to_owned))
        .bind(filter.state.map(|s| s.as_str().to_owned()))
        .bind(filter.since)
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

    /// Findings across **every** run, newest first — the Saved-findings search.
    ///
    /// The join to `analysis_jobs` is what makes `?tool=` possible at all: a finding row records
    /// what was found, never which diagnostic found it.
    pub async fn search_findings(
        &self,
        q: &FindingSearch<'_>,
    ) -> anyhow::Result<Vec<SavedFinding>> {
        // An empty set means *unfiltered*, which is a NULL bind — an empty array would make
        // `= ANY(…)` match nothing and turn "no filter" into "no results".
        fn set<'s>(tokens: impl Iterator<Item = &'s str>) -> Option<Vec<String>> {
            let v: Vec<String> = tokens.map(str::to_owned).collect();
            (!v.is_empty()).then_some(v)
        }
        let rows = sqlx::query(&finding_search_sql())
            .bind(q.before)
            .bind(q.before_id)
            .bind(q.since)
            .bind(set(q.tool.iter().map(|t| t.as_str())))
            .bind(set(q.severity.iter().copied()))
            .bind(q.node_id)
            .bind(q.groups.map(<[Uuid]>::to_vec))
            .bind(q.in_group.map(<[Uuid]>::to_vec))
            .bind(q.q)
            .bind(q.node_q)
            .bind(q.min_score)
            .bind(q.max_score)
            .bind(q.limit)
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter()
            .map(|row| {
                let at: DateTime<Utc> = row.try_get("created_at")?;
                Ok(SavedFinding {
                    id: row.try_get("id")?,
                    job_id: row.try_get("job_id")?,
                    tool: row.try_get("tool")?,
                    score: row.try_get("score")?,
                    severity: row.try_get("severity")?,
                    node_id: row.try_get("node_id")?,
                    node_name: row.try_get("node_name")?,
                    metric: row.try_get("metric")?,
                    kind: row.try_get("kind")?,
                    when_label: row.try_get("when_label")?,
                    duration: row.try_get("duration")?,
                    at: at.to_rfc3339(),
                })
            })
            .collect()
    }

    // — Schedules —

    /// Every schedule, soonest first.
    pub async fn list_schedules(&self) -> anyhow::Result<Vec<AnalysisSchedule>> {
        let rows = sqlx::query(&format!(
            "SELECT {SCHED_COLS} FROM analysis_schedules ORDER BY next_run_at"
        ))
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(sched_from_row).collect()
    }

    pub async fn create_schedule(
        &self,
        input: &ScheduleInput,
        next_run_at: DateTime<Utc>,
        updated_by: Option<&str>,
    ) -> anyhow::Result<Uuid> {
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO analysis_schedules \
             (id, tool, scope_kind, scope_id, scope_label, params, frequency, day_of_week, \
              day_of_month, at_hour, at_minute, enabled, next_run_at, updated_by) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)",
        )
        .bind(id)
        .bind(input.params.tool.as_str())
        .bind(input.params.scope_kind.as_str())
        .bind(input.params.scope_id)
        .bind(&input.params.scope_label)
        .bind(input.params.to_json())
        .bind(input.cadence.frequency.as_str())
        .bind(input.cadence.day_of_week)
        .bind(input.cadence.day_of_month)
        .bind(input.cadence.at_hour)
        .bind(input.cadence.at_minute)
        .bind(input.enabled)
        .bind(next_run_at)
        .bind(updated_by)
        .execute(&self.pool)
        .await?;
        Ok(id)
    }

    pub async fn update_schedule(
        &self,
        id: Uuid,
        input: &ScheduleInput,
        next_run_at: DateTime<Utc>,
        updated_by: Option<&str>,
    ) -> anyhow::Result<bool> {
        let res = sqlx::query(
            "UPDATE analysis_schedules SET tool = $2, scope_kind = $3, scope_id = $4, \
             scope_label = $5, params = $6, frequency = $7, day_of_week = $8, day_of_month = $9, \
             at_hour = $10, at_minute = $11, enabled = $12, next_run_at = $13, updated_by = $14, \
             updated_at = now() WHERE id = $1",
        )
        .bind(id)
        .bind(input.params.tool.as_str())
        .bind(input.params.scope_kind.as_str())
        .bind(input.params.scope_id)
        .bind(&input.params.scope_label)
        .bind(input.params.to_json())
        .bind(input.cadence.frequency.as_str())
        .bind(input.cadence.day_of_week)
        .bind(input.cadence.day_of_month)
        .bind(input.cadence.at_hour)
        .bind(input.cadence.at_minute)
        .bind(input.enabled)
        .bind(next_run_at)
        .bind(updated_by)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() > 0)
    }

    /// One schedule by id — the read the API edge does before letting a scoped caller edit it.
    pub async fn get_schedule(&self, id: Uuid) -> anyhow::Result<Option<AnalysisSchedule>> {
        let row = sqlx::query(&format!(
            "SELECT {SCHED_COLS} FROM analysis_schedules WHERE id = $1"
        ))
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(sched_from_row).transpose()
    }

    pub async fn delete_schedule(&self, id: Uuid) -> anyhow::Result<bool> {
        let res = sqlx::query("DELETE FROM analysis_schedules WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }

    /// Enabled schedules whose `next_run_at` has passed (the scheduler's due-query).
    pub async fn due_schedules(&self) -> anyhow::Result<Vec<AnalysisSchedule>> {
        let rows = sqlx::query(&format!(
            "SELECT {SCHED_COLS} FROM analysis_schedules \
             WHERE enabled = true AND next_run_at <= now() ORDER BY next_run_at"
        ))
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(sched_from_row).collect()
    }

    /// Record a fire that produced a run: stamp `last_run_at`/`last_status` and advance to `next`.
    pub async fn mark_fired(
        &self,
        id: Uuid,
        status: AnalysisScheduleStatus,
        next: DateTime<Utc>,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "UPDATE analysis_schedules SET last_run_at = now(), last_status = $2, next_run_at = $3 \
             WHERE id = $1",
        )
        .bind(id)
        .bind(status.as_str())
        .bind(next)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Record an attempt admission control refused: **leave `next_run_at` where it is** so the
    /// schedule stays due and the next tick retries.
    ///
    /// `last_run_at` is deliberately not stamped either — nothing ran, and a schedule reporting a
    /// last run that produced no row is the confusing half of this failure mode. The status alone
    /// says what happened.
    pub async fn mark_deferred(&self, id: Uuid) -> anyhow::Result<()> {
        sqlx::query("UPDATE analysis_schedules SET last_status = $2 WHERE id = $1")
            .bind(id)
            .bind(AnalysisScheduleStatus::Busy.as_str())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// On startup, fail any job left `running` by a previous core process (it can't resume).
    pub async fn fail_orphans(&self) -> anyhow::Result<u64> {
        let res = sqlx::query(&format!(
            "UPDATE analysis_jobs SET state = '{}', phase = NULL, \
             error = 'core restarted while running', finished_at = now() WHERE state = '{}'",
            AnalysisJobState::Failed.as_str(),
            AnalysisJobState::Running.as_str()
        ))
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
    ///
    /// ⚠️ **Never read directly from a `run_*`.** Once `logs` is `Some`, this holds only the
    /// alert-linked subset, so an analysis that counts events must go through the `agg_*` routers
    /// below. `every_event_analysis_reads_through_the_store_router` enforces it.
    events: Arc<EventRepo>,
    /// The event log store (ADR-024), `None` when VictoriaLogs is not configured. `Some` means
    /// PostgreSQL is a subset and the `agg_*` routers must ask this instead.
    logs: Option<Arc<dyn LogStore>>,
    /// Flow store (ClickHouse, ADR-031), `None` when the flow tier is off — the `flow_*`/`traffic_*`/
    /// `talker_*`/`new_destination`/`scan`/`saturation` analyses no-op with an info finding then.
    flows: Option<Arc<dyn FlowStore>>,
    /// IP→ASN table handle for resolving AS names in flow findings (`new_destination`).
    ipasn: IpAsnHandle,
    /// Connectivity graph sources — `incident_correlate` expands an incident to a node's directly
    /// linked neighbours (ADR-022 Increment 2, on the graph ADR-043 derives).
    topo: crate::topology_projection::TopologySources,
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

/// The stores an [`AnalysisRunner`] reads.
///
/// A struct rather than more parameters: `new` was already at the `clippy::too_many_arguments`
/// threshold, and that lint is a design signal rather than something to silence
/// (coding-conventions). Same shape as [`crate::topology_projection::TopologySources`].
pub struct AnalysisSeams {
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
    pub fn new(repo: Arc<AnalysisRepo>, seams: AnalysisSeams) -> Self {
        let AnalysisSeams {
            store,
            nodes,
            groups,
            events,
            logs,
            flows,
            ipasn,
            topo,
        } = seams;
        let (tx, _) = broadcast::channel(EVENT_BUFFER);
        let max_concurrent = env_cap("YAGRA_ANALYSIS_MAX_CONCURRENT", DEFAULT_MAX_CONCURRENT);
        let max_per_window = env_cap("YAGRA_ANALYSIS_RATE_PER_MIN", DEFAULT_RATE_PER_MIN);
        Self {
            repo,
            store,
            nodes,
            groups,
            events,
            logs,
            flows,
            ipasn,
            topo,
            tx,
            cancels: Mutex::new(std::collections::HashMap::new()),
            slots: Arc::new(Semaphore::new(max_concurrent)),
            max_concurrent,
            recent_starts: Mutex::new(VecDeque::new()),
            max_per_window,
        }
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
    // The only way a `run_*` may reach an event store. The store choice itself lives in
    // `logstore::route_*` — shared with the MCP `event_stats` tool, which had the same defect.

    async fn agg_counts_by_bucket(
        &self,
        filter: &EventFilter,
        bucket_secs: i64,
    ) -> anyhow::Result<Vec<EventBucketCount>> {
        crate::logstore::route_counts_by_bucket(
            self.logs.as_ref(),
            &self.events,
            filter,
            bucket_secs,
        )
        .await
    }

    async fn agg_severity_counts(
        &self,
        filter: &EventFilter,
    ) -> anyhow::Result<Vec<EventSeverityCount>> {
        crate::logstore::route_severity_counts(self.logs.as_ref(), &self.events, filter).await
    }

    async fn agg_unmatched_signatures(
        &self,
        filter: &EventFilter,
        limit: i64,
    ) -> anyhow::Result<Vec<EventSignatureCount>> {
        crate::logstore::route_unmatched_signatures(self.logs.as_ref(), &self.events, filter, limit)
            .await
    }

    async fn agg_auth_sources(
        &self,
        filter: &EventFilter,
        limit: i64,
    ) -> anyhow::Result<Vec<EventAuthSource>> {
        crate::logstore::route_auth_sources(self.logs.as_ref(), &self.events, filter, limit).await
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

    fn broadcast_job(&self, job: &AnalysisJob) {
        if let Ok(json) = serde_json::to_string(job) {
            let kind = ScopeKind::from_str(&job.scope_kind);
            let _ = self
                .tx
                .send((kind, job.scope_id, std::sync::Arc::from(json)));
        }
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

    /// Whether this deployment has a flow store (ClickHouse, ADR-031).
    ///
    /// The API edge asks before accepting a *scheduled* flow analysis. A manual run of one with the
    /// tier off short-circuits to a single "flow tier not enabled" info finding, which is the right
    /// answer to a one-off question; scheduled daily it would stack up an empty successful run
    /// every day forever, which reads as a working schedule producing nothing.
    #[must_use]
    pub fn flow_enabled(&self) -> bool {
        self.flows.is_some()
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
        // Unrestricted deliberately: `node_ids` is the job's own resolved scope, which was checked
        // against the launching principal at create time (`api/analysis.rs`). Re-filtering here
        // would scope a background run to whoever happens to be reading the results.
        let names = self
            .nodes
            .node_names(None, &node_ids)
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
        let window = Self::scoped_window(params, node_ids, from, to);
        let rows = self
            .agg_counts_by_bucket(&window, EVENT_BUCKET_SECS)
            .await?;
        // The store restricts to the caller's scope; this fold additionally honours `node_cap`,
        // which bounds how many nodes a `quick`/`standard` run scores at all.
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
                detail: storm_detail(peak, mean(&baseline), peak_bucket),
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
            .agg_severity_counts(&Self::scoped_window(params, node_ids, from, recent_cutoff))
            .await?;
        if cancel.load(Ordering::Relaxed) {
            return Ok(None);
        }
        let recent = self
            .agg_severity_counts(&Self::scoped_window(params, node_ids, recent_cutoff, to))
            .await?;
        // As in `run_event_storm`: the store applies the caller's scope, this fold applies
        // `node_cap`.
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
            .agg_unmatched_signatures(&Self::scoped_window(params, node_ids, from, to), 200)
            .await?;
        // No sample-node scope filter here any more: the store already restricted, and filtering on
        // the representative node dropped signatures that *did* occur inside the caller's group
        // whenever some out-of-group node sorted lower. On the log-store path `sample_node` is
        // `None` for every row anyway (LogsQL has no `min(uuid)`), which that filter would have
        // read as "out of scope" and discarded wholesale.
        let mut findings: Vec<NewFinding> = Vec::new();
        for s in sigs {
            if s.count < RULE_GAP_FLOOR {
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
            .agg_auth_sources(&Self::scoped_window(params, node_ids, from, to), 100)
            .await?;
        // Same as `run_rule_gap`: the store restricted, so no post-filter on the correlated node.
        // The old one also hid every auth source that mapped to no inventory node at all — which is
        // precisely what an external prober looks like.
        let mut findings: Vec<NewFinding> = Vec::new();
        for s in sources {
            if s.count < AUTH_FLOOR {
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
                detail: traffic_detail(peak, mean(&baseline), peak_t),
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

        // The one-hop neighbourhood, built once per job rather than per node.
        //
        // ⚠️ **Scope rule**: a neighbour may be consulted, scored or named only if it is itself in
        // the job's resolved node set, which was checked against the launching principal at create
        // time. This is the direct analogue of `TopoLinkRepo::list_page`'s "both endpoints visible"
        // rule — one visible end still tells a scoped operator that a node exists outside their
        // scope. The weaker "consult anything, name only what is visible" leaks by inference: the
        // finding's score, its signal count, and whether it is emitted at all would move with data
        // the caller cannot see.
        let authorized: HashSet<Uuid> = node_ids.iter().copied().collect();
        let neighbours = self.incident_neighbourhood(&authorized).await;

        // Memoized signal fetch: each `incident_signals` call is one TSDB read plus one event query
        // plus one ClickHouse query, so a naive expansion would multiply the job's I/O by the fan-out
        // (20 nodes × 4 peers = up to 100 fetches instead of 20). Bounded by `INCIDENT_NODE_CAP`
        // distinct nodes overall.
        let mut cache: HashMap<Uuid, Vec<IncidentSignal>> = HashMap::new();
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
            let own = self.signals_for(&mut cache, *node, params, from, to).await;
            // A node never gets a finding purely from its neighbours: it must show something of its
            // own. This keeps the pre-expansion behaviour as the floor.
            if own.is_empty() {
                continue;
            }

            // Corroborating peers, most severe first, capped.
            let mut peers: Vec<(Uuid, &'static str, Vec<IncidentSignal>)> = Vec::new();
            for (peer, relation) in neighbours.get(node).into_iter().flatten() {
                if cancel.load(Ordering::Relaxed) {
                    return Ok(None);
                }
                let sigs = self.signals_for(&mut cache, *peer, params, from, to).await;
                if sigs.is_empty() || !signals_coincide(&own, &sigs, NEIGHBOUR_COINCIDENCE_SECS) {
                    continue;
                }
                peers.push((*peer, relation, sigs));
            }
            peers.sort_by(|a, b| {
                peak_severity(&b.2)
                    .partial_cmp(&peak_severity(&a.2))
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            peers.truncate(NEIGHBOUR_CAP);

            // Cross-signal evidence required: ≥2 signals across ≥2 kinds. A corroborating peer's
            // signals count toward both — that is what the expansion buys, and why an outage that
            // only shows one kind of symptom locally can now be recognised.
            let mut all: Vec<(Option<Uuid>, &IncidentSignal)> =
                own.iter().map(|s| (None, s)).collect();
            for (peer, _, sigs) in &peers {
                all.extend(sigs.iter().map(|s| (Some(*peer), s)));
            }
            let kinds: HashSet<&str> = all.iter().map(|(_, s)| s.kind).collect();
            if all.len() < 2 || kinds.len() < 2 {
                continue;
            }
            all.sort_by_key(|(_, s)| s.at_s);
            let score = all.iter().map(|(_, s)| s.severity).fold(0.0, f64::max);
            let earliest = all.first().map_or(to, |(_, s)| s.at_s);
            // The subject's own entries carry no `node_id`/`node_name`, so the shape stays purely
            // additive and `format.ts::timelineOf` renders old and new findings unchanged.
            let timeline: Vec<serde_json::Value> = all
                .iter()
                .map(|(peer, s)| {
                    let mut v = serde_json::json!({
                        "at": s.at_s, "kind": s.kind, "label": s.label, "severity": s.severity,
                    });
                    if let (Some(p), Some(obj)) = (peer, v.as_object_mut()) {
                        obj.insert("node_id".into(), serde_json::json!(p));
                        obj.insert("node_name".into(), serde_json::json!(name_lookup(names, p)));
                    }
                    v
                })
                .collect();
            let peer_rows: Vec<serde_json::Value> = peers
                .iter()
                .map(|(p, relation, sigs)| {
                    serde_json::json!({
                        "node_id": p,
                        "node_name": name_lookup(names, p),
                        "relation": relation,
                        "signals": sigs.len(),
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
                duration: format!("{} signals", all.len()),
                detail: serde_json::json!({
                    "timeline": timeline,
                    "peers": peer_rows,
                    "peer_count": peer_rows.len(),
                }),
            });
        }
        finalize(&mut findings);
        let summary = format!("{} correlated incidents", findings.len());
        Ok(Some((findings, summary)))
    }

    /// One-hop neighbours per node, restricted to `authorized`, labelled upstream/downstream.
    ///
    /// **Not gated on [`crate::topology_mode::TopologyMode`], deliberately.** That gate exists
    /// because a wrong derived edge *suppresses a real outage*, and silence is unrecoverable;
    /// `incident_correlate` suppresses nothing, so a wrong edge here only adds a peer to a
    /// diagnostic — the noisy direction. Gating on it would ship this dead on every default
    /// deployment (`manual` is the default and is where upgrades land), which is the "built it and
    /// nobody used it" failure ADR-043 exists to fix. `topology_mode.rs` says the same thing from
    /// the other side: nothing outside the read endpoint should branch on the mode.
    ///
    /// Both graphs are unioned, so a hand-authored `parent_id` counts as an edge alongside a
    /// derived one. A node with the "never suppress" opt-out gets no parents from
    /// [`crate::topology_projection::derived_topology`], and that is kept rather than worked
    /// around: "do not reason about this node's upstream" is an operator statement.
    async fn incident_neighbourhood(
        &self,
        authorized: &HashSet<Uuid>,
    ) -> HashMap<Uuid, Vec<(Uuid, &'static str)>> {
        let nodes = match self.nodes.list_nodes().await {
            Ok(n) => n,
            Err(e) => {
                // Degrade to no expansion rather than failing the job: single-node correlation is
                // exactly the behaviour this analysis had before, so it is a safe floor.
                tracing::warn!(error = %e, "incident correlation: reading the inventory failed");
                return HashMap::new();
            }
        };
        let (derived, _) = crate::topology_projection::derived_topology(&self.topo, &nodes).await;
        let manual = crate::topology_projection::manual_topology(&nodes);
        one_hop_neighbours(&derived, &manual, authorized)
    }

    /// `incident_signals` for one node, memoized for the job.
    ///
    /// Two things this buys. Each call is one TSDB read plus one event query plus one ClickHouse
    /// query, and a node is typically both a subject and some other subject's neighbour — so
    /// without the cache the expansion multiplies the job's I/O by the fan-out. And the cache is
    /// also the bound: at most [`INCIDENT_CACHE_CAP`] distinct nodes are ever fetched, so a hub
    /// with a hundred links cannot turn a bounded job into an unbounded one. A node past the cap
    /// contributes no signals rather than being fetched — the same direction as every other cap
    /// here, less evidence rather than a longer job.
    async fn signals_for(
        &self,
        cache: &mut HashMap<Uuid, Vec<IncidentSignal>>,
        node: Uuid,
        params: &JobParams,
        from: i64,
        to: i64,
    ) -> Vec<IncidentSignal> {
        if let Some(hit) = cache.get(&node) {
            return hit.clone();
        }
        if cache.len() >= INCIDENT_CACHE_CAP {
            return Vec::new();
        }
        let sigs = self
            .incident_signals(
                node,
                from,
                to,
                params.window_secs.max(600),
                params.sensitivity.max(2.0),
            )
            .await;
        cache.insert(node, sigs.clone());
        sigs
    }

    /// Assemble one node's cross-signal timeline over `[from_s, to_s]`: a reachability metric
    /// anomaly (TSDB), its recent passive events (event store), and the dominant flow conversation
    /// (ClickHouse, when the flow tier is on). Signals are unordered and unfiltered — the caller
    /// decides how much evidence is enough.
    ///
    /// Shared by the `incident_correlate` analysis and the LLM RCA context builder (ADR-029) so
    /// there is exactly one definition of "what this node's incident looked like". A second
    /// implementation would drift, and the two would then disagree about the same outage.
    ///
    /// `recent_window_s` is how far back still counts as "recent" for the anomaly scorer, and
    /// `sigma` its sensitivity.
    pub(crate) async fn incident_signals(
        &self,
        node: Uuid,
        from_s: i64,
        to_s: i64,
        recent_window_s: i64,
        sigma: f64,
    ) -> Vec<IncidentSignal> {
        let mut signals: Vec<IncidentSignal> = Vec::new();
        // 1) Reachability metric anomaly.
        let step = read_step(from_s, to_s);
        let rtt = self
            .gauge_range(node, "icmp_rtt_ms", from_s, to_s, step)
            .await;
        if rtt.len() >= MIN_POINTS {
            if let Some(a) = score_anomaly(&rtt, to_s - recent_window_s, sigma) {
                signals.push(IncidentSignal {
                    at_s: a.when_s,
                    severity: a.score,
                    kind: "metric",
                    label: format!("icmp_rtt_ms {}", a.kind),
                });
            }
        }
        // 2) Passive events on the node. Read through the log store when one is configured: with
        // ADR-024 on, PostgreSQL holds only the alert-linked subset, so a timeline built from it
        // showed the events that had already alerted and nothing that led up to them.
        let filter = EventFilter {
            since: DateTime::from_timestamp(from_s, 0),
            node_id: Some(node),
            ..Default::default()
        };
        let events = match self.logs.as_ref() {
            Some(logs) => {
                logs.search(&filter, crate::logstore::NameIds::default(), 20)
                    .await
            }
            None => self.events.list_events(&filter, 20).await,
        }
        .unwrap_or_default();
        for e in events.iter().take(INCIDENT_EVENT_CAP) {
            signals.push(IncidentSignal {
                at_s: e.at_unix_ms / 1000,
                severity: event_signal_severity(e.action, e.syslog_severity),
                kind: "event",
                label: incident_event_label(e),
            });
        }
        // 3) Dominant flow conversation.
        if let Some(flows) = self.flows.clone() {
            let q = FlowQuery {
                node_id: Some(node),
                from_unix_ms: from_s * 1000,
                to_unix_ms: to_s * 1000,
                limit: 1,
                proto: None,
                dst_port: None,
                peer: None,
                asn: None,
            };
            if let Ok(cs) = flows.top_conversations(&q).await {
                if let Some(c) = cs.first() {
                    signals.push(IncidentSignal {
                        at_s: to_s,
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
        signals
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
/// Distinct nodes whose signals one incident job may fetch, subjects and neighbours together.
///
/// [`INCIDENT_NODE_CAP`] subjects each expanding to [`NEIGHBOUR_CAP`] peers is the worst case, and
/// this is what stops that arithmetic from being unbounded when the graph is dense.
const INCIDENT_CACHE_CAP: usize = INCIDENT_NODE_CAP * (NEIGHBOUR_CAP + 1);
/// How close a neighbour's signal must land to one of the subject's to count as corroboration.
///
/// One [`EVENT_BUCKET_SECS`], because that is already the resolution at which this codebase treats
/// passive events as contemporaneous. Wider would let a chatty upstream corroborate anything.
const NEIGHBOUR_COINCIDENCE_SECS: i64 = EVENT_BUCKET_SECS;
/// Most neighbours carried on one incident finding, after sorting by peak severity.
///
/// A core switch can have hundreds of links; an incident report naming hundreds of peers is not a
/// report. ⚠️ This number is a guess until the derivation runs against a real multi-vendor fleet —
/// the lab cannot verify a single derived edge (ADR-043).
const NEIGHBOUR_CAP: usize = 4;

/// Whether a peer's signals corroborate the subject's: at least one peer signal lands within
/// `window_s` of a subject signal.
///
/// Pure, so the correlation rule is testable without any store — the rest of `incident_correlate`
/// needs three of them. Coincidence is required rather than mere adjacency in the graph: without
/// it, one noisy upstream manufactures an incident for every quiet device hanging off it.
fn peak_severity(signals: &[IncidentSignal]) -> f64 {
    signals.iter().map(|s| s.severity).fold(0.0, f64::max)
}

/// The one-hop neighbourhood of every authorized node, from the union of the two graphs.
///
/// ⚠️ **The scope rule lives here, and it is the security-relevant part of the expansion**: a peer
/// appears only if it is itself in `authorized`. Pure, so that rule is testable without a database
/// — which is the point, because the failure it prevents is silent. See
/// `incident_neighbourhood` for why both graphs are unioned and why the topology mode is not a gate.
fn one_hop_neighbours(
    derived: &Topology,
    manual: &Topology,
    authorized: &HashSet<Uuid>,
) -> HashMap<Uuid, Vec<(Uuid, &'static str)>> {
    let mut out: HashMap<Uuid, Vec<(Uuid, &'static str)>> = HashMap::new();
    for &node in authorized {
        let id = NodeId::from(node);
        let mut seen: HashSet<Uuid> = HashSet::new();
        let mut peers: Vec<(Uuid, &'static str)> = Vec::new();
        // Upstream first, so a node that is somehow both keeps the more useful label.
        for (set, relation) in [
            (derived.parents_of(id), "upstream"),
            (manual.parents_of(id), "upstream"),
            (derived.children_of(id), "downstream"),
            (manual.children_of(id), "downstream"),
        ] {
            for peer in set {
                let peer = peer.as_uuid();
                if peer != node && authorized.contains(&peer) && seen.insert(peer) {
                    peers.push((peer, relation));
                }
            }
        }
        if !peers.is_empty() {
            out.insert(node, peers);
        }
    }
    out
}

/// Whether a peer's signals corroborate the subject's: at least one peer signal lands within
/// `window_s` of a subject signal.
fn signals_coincide(subject: &[IncidentSignal], peer: &[IncidentSignal], window_s: i64) -> bool {
    subject.iter().any(|s| {
        peer.iter()
            .any(|p| (p.at_s - s.at_s).abs() <= window_s.max(0))
    })
}

/// Per-node split of event-bucket counts for `event_storm`: (baseline counts, recent (bucket, count)).
type StormBuckets = (Vec<f64>, Vec<(i64, f64)>);

/// The `event_storm` finding detail. `peak_at` is the peak bucket's start (Unix **seconds**) so the
/// WebUI can render a *localized* relative time instead of falling back to the pre-rendered English
/// `when_label` — the label itself is built by `rel_label` and can't go through `t()`. Purely
/// additive to the JSONB blob (older rows simply lack the key, and the UI falls back), so this is
/// N-1 safe with no migration. Split out from the engine fn so it is unit-testable — the engine
/// itself needs a live event store.
fn storm_detail(peak: f64, baseline_mean: f64, peak_at: i64) -> serde_json::Value {
    serde_json::json!({
        "peak": peak,
        "baseline_mean": baseline_mean,
        "bucket_secs": EVENT_BUCKET_SECS,
        "peak_at": peak_at,
    })
}

/// The `traffic_anomaly` finding detail — the flow twin of [`storm_detail`], carrying the same
/// additive `peak_at` (Unix seconds) for a localizable relative label.
fn traffic_detail(peak_bytes: f64, baseline_mean_bytes: f64, peak_at: i64) -> serde_json::Value {
    serde_json::json!({
        "peak_bytes": peak_bytes,
        "baseline_mean_bytes": baseline_mean_bytes,
        "peak_at": peak_at,
    })
}

/// One dated signal on an incident timeline (`incident_correlate`).
///
/// `Serialize` because an RCA report stores the timeline it was grounded in alongside the answer:
/// the UI shows the two together so a reader can check the explanation against its evidence rather
/// than taking it on faith (ADR-029).
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub(crate) struct IncidentSignal {
    pub(crate) at_s: i64,
    pub(crate) severity: f64,
    pub(crate) kind: &'static str,
    pub(crate) label: String,
}

/// Most passive events carried into one node's timeline. A noisy device can log hundreds in the
/// window; past a handful they stop adding evidence and start crowding out the other signal kinds
/// (and, for the RCA context, the prompt budget).
const INCIDENT_EVENT_CAP: usize = 8;

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
fn event_signal_severity(action: EventAction, syslog_severity: Option<i16>) -> f64 {
    match action {
        EventAction::Fired => 85.0,
        EventAction::Refreshed => 70.0,
        EventAction::Cleared => 60.0,
        // An event that raised no alert is scored by what the device itself called it. Listed
        // rather than left to a wildcard so a new outcome has to choose a side.
        EventAction::Suppressed | EventAction::Info | EventAction::None => match syslog_severity {
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
        .unwrap_or_else(|| e.kind.as_str().to_owned());
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
/// The built-in catalog's declared [`yagra_common::MetricKind`] is authoritative; the substring
/// heuristic survives only for custom metrics outside the catalog (there is no DB handle here,
/// and a counter-ish name is the best remaining signal).
fn is_counter(metric: &str) -> bool {
    match yagra_common::builtin_metric_kind(metric) {
        Some(yagra_common::MetricKind::Counter) => true,
        Some(yagra_common::MetricKind::Gauge) => false,
        None => {
            metric.contains("octets")
                || metric.contains("errors")
                || metric.contains("discards")
                || metric.contains("packets")
        }
    }
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
    fn every_severity_the_engine_writes_is_one_the_search_accepts() {
        // The two readers of the same set: `severity_for` writes it, the Saved-findings edge
        // validates `?severity=` against `FINDING_SEVERITIES`. A value the engine can produce but
        // the edge rejects would be findings nobody can filter for — so sweep the whole score
        // range rather than the three thresholds.
        for score in (-50..=150).map(f64::from) {
            let written = severity_for(score);
            assert!(
                FINDING_SEVERITIES.contains(&written),
                "severity_for({score}) = {written:?}, which is not in FINDING_SEVERITIES"
            );
        }
    }

    #[test]
    fn needs_flow_tier_matches_which_analyses_actually_short_circuit() {
        // The two must agree or a scheduled analysis is refused for a tier it does not need, or —
        // worse — accepted and left to stack up an empty successful run every day. Read from the
        // source rather than restated, so adding a `flow_tier_off()` arm without updating
        // `needs_flow_tier` fails here.
        //
        // The needle is built at runtime: a literal `flow_tier_off()` written in this test would
        // match itself in the file and pass forever.
        let src = include_str!("analysis.rs");
        let needle = format!("{}{}", "flow_tier", "_off()");
        let mut short_circuits = std::collections::BTreeSet::new();
        let mut current_fn: Option<String> = None;
        for line in src.lines() {
            if let Some(rest) = line.trim().strip_prefix("async fn run_") {
                current_fn = rest.split('(').next().map(str::to_owned);
            }
            if line.contains(&needle) && line.contains("return") {
                if let Some(f) = &current_fn {
                    short_circuits.insert(f.clone());
                }
            }
        }
        assert!(
            short_circuits.len() >= 5,
            "the source scan stopped matching: {short_circuits:?}"
        );
        for tool in AnalysisTool::ALL.iter().copied() {
            // `run_<token>` is the naming convention every analysis follows.
            let name = tool.as_str().to_owned();
            let short_circuits_here = short_circuits.contains(&name);
            assert_eq!(
                tool.needs_flow_tier(),
                short_circuits_here,
                "{name}: needs_flow_tier() = {} but the runner {} short-circuit on the flow tier",
                tool.needs_flow_tier(),
                if short_circuits_here {
                    "does"
                } else {
                    "does not"
                }
            );
        }
    }

    // ── incident_correlate neighbour expansion (ADR-022 Increment 2) ────────────────────────────

    fn sig(at_s: i64, kind: &'static str, severity: f64) -> IncidentSignal {
        IncidentSignal {
            at_s,
            severity,
            kind,
            label: "x".to_owned(),
        }
    }

    /// Coincidence is what stops a chatty upstream from manufacturing an incident for every quiet
    /// device hanging off it — adjacency in the graph alone is not evidence.
    #[test]
    fn signals_coincide_only_within_the_window() {
        let subject = [sig(1_000, "metric", 1.0)];
        assert!(signals_coincide(&subject, &[sig(1_100, "event", 1.0)], 300));
        assert!(signals_coincide(&subject, &[sig(900, "event", 1.0)], 300));
        // Exactly at the boundary counts; one second past it does not.
        assert!(signals_coincide(&subject, &[sig(1_300, "event", 1.0)], 300));
        assert!(!signals_coincide(
            &subject,
            &[sig(1_301, "event", 1.0)],
            300
        ));
        assert!(!signals_coincide(
            &subject,
            &[sig(9_999, "event", 1.0)],
            300
        ));
        // Either side empty is no corroboration, never a vacuous yes.
        assert!(!signals_coincide(&subject, &[], 300));
        assert!(!signals_coincide(&[], &[sig(1_000, "event", 1.0)], 300));
        // Any pair inside the window is enough, not every pair.
        assert!(signals_coincide(
            &subject,
            &[sig(9_999, "event", 1.0), sig(1_050, "event", 1.0)],
            300
        ));
    }

    /// **The security test.** A neighbour is consulted, scored and named only if it is itself in
    /// the job's authorized node set.
    ///
    /// The weaker rule — consult anything, name only what is visible — leaks by inference: the
    /// finding's score, its signal count, and whether it is emitted at all would move with data the
    /// caller cannot see. This is the analogue of `TopoLinkRepo::list_page`'s "both endpoints
    /// visible", and `ScopeKind::Group` is an inventory-folder subtree, never a topology
    /// neighbourhood — so a peer is not in scope merely by being adjacent.
    #[test]
    fn a_neighbour_outside_the_job_scope_is_never_consulted() {
        let (mine, theirs) = (Uuid::from_u128(1), Uuid::from_u128(2));
        let mut derived = Topology::new();
        // `theirs` is `mine`'s upstream, but the caller cannot see it.
        derived.add_dependency(NodeId::from(mine), NodeId::from(theirs));

        let authorized: HashSet<Uuid> = [mine].into_iter().collect();
        let n = one_hop_neighbours(&derived, &Topology::new(), &authorized);
        assert!(
            !n.contains_key(&mine),
            "an out-of-scope neighbour must not appear at all: {n:?}"
        );

        // …and with both endpoints authorized, the edge is used and labelled from the subject's
        // point of view.
        let authorized: HashSet<Uuid> = [mine, theirs].into_iter().collect();
        let n = one_hop_neighbours(&derived, &Topology::new(), &authorized);
        assert_eq!(
            n.get(&mine).map(Vec::as_slice),
            Some(&[(theirs, "upstream")][..])
        );
        assert_eq!(
            n.get(&theirs).map(Vec::as_slice),
            Some(&[(mine, "downstream")][..])
        );
    }

    /// The manual graph counts as evidence alongside the derived one, and a node appearing in both
    /// is listed once. This is what makes the expansion useful on a deployment still in `manual`
    /// topology mode — which is the default, and where upgrades land.
    #[test]
    fn hand_authored_and_derived_edges_are_unioned_without_duplicates() {
        let (child, parent) = (Uuid::from_u128(1), Uuid::from_u128(2));
        let mut derived = Topology::new();
        derived.add_dependency(NodeId::from(child), NodeId::from(parent));
        let mut manual = Topology::new();
        manual.add_dependency(NodeId::from(child), NodeId::from(parent));

        let authorized: HashSet<Uuid> = [child, parent].into_iter().collect();
        let n = one_hop_neighbours(&derived, &manual, &authorized);
        assert_eq!(n[&child], vec![(parent, "upstream")], "listed twice");

        // A manual-only edge still counts, so the feature is not dead in `manual` mode.
        let n = one_hop_neighbours(&Topology::new(), &manual, &authorized);
        assert_eq!(n[&child], vec![(parent, "upstream")]);
    }

    /// A self-edge is not a neighbour, and a node with no edges gets no entry at all (rather than
    /// an empty vector the caller would have to distinguish).
    #[test]
    fn a_node_is_not_its_own_neighbour() {
        let a = Uuid::from_u128(1);
        let mut topo = Topology::new();
        topo.add_dependency(NodeId::from(a), NodeId::from(a));
        let authorized: HashSet<Uuid> = [a].into_iter().collect();
        assert!(one_hop_neighbours(&topo, &Topology::new(), &authorized).is_empty());
        assert!(one_hop_neighbours(&Topology::new(), &Topology::new(), &authorized).is_empty());
    }

    /// The fan-out bound. Twenty subjects each expanding to four peers is the worst case, and the
    /// cache cap is what keeps `incident_signals`' three-store fetch from multiplying by it.
    #[test]
    fn the_incident_cache_bounds_the_worst_case_fan_out() {
        // Every subject fits, plus room for each one's peers.
        assert_eq!(INCIDENT_CACHE_CAP, INCIDENT_NODE_CAP * (NEIGHBOUR_CAP + 1));
        // One event bucket: the resolution at which this codebase already treats passive events as
        // contemporaneous. Widening it would let an unrelated upstream corroborate anything.
        assert_eq!(NEIGHBOUR_COINCIDENCE_SECS, EVENT_BUCKET_SECS);
    }

    /// No `run_*` may read an event aggregate straight off `self.events`.
    ///
    /// This is the guard for the defect ADR-022 Increment 2 fixes. With a log store configured,
    /// PostgreSQL holds only alert-linked rows (ADR-024), so `self.events.event_*` answers about a
    /// subset — and `rule_gap`, which looks for *unmatched* events, about the empty set. Only the
    /// four `agg_*` routers may touch a store; every analysis goes through them.
    ///
    /// Same shape as `needs_flow_tier_matches_…` above, including the runtime-built needles: a
    /// literal `self.events.event_counts_by_bucket` written here would match itself in the file and
    /// pass forever.
    #[test]
    fn every_event_analysis_reads_through_the_store_router() {
        /// Whether a trimmed line opens a function, under any visibility spelling.
        fn is_fn_definition(t: &str) -> bool {
            let rest = t
                .strip_prefix("pub(crate) ")
                .or_else(|| t.strip_prefix("pub(super) "))
                .or_else(|| t.strip_prefix("pub "))
                .unwrap_or(t);
            let rest = rest.strip_prefix("async ").unwrap_or(rest);
            rest.starts_with("fn ")
        }

        let src = include_str!("analysis.rs");
        // The aggregates that have a log-store twin. `event_flap_stats` is deliberately absent:
        // every action it counts is alert-linked, so PostgreSQL is complete for it (pinned by
        // `events::tests::event_flap_only_counts_rows_postgresql_keeps`).
        let routed = [
            "event_counts_by_bucket",
            "event_severity_counts",
            "event_unmatched_signatures",
            "event_auth_sources",
        ];
        let direct: Vec<String> = routed
            .iter()
            .map(|m| format!("{}{}", "self.events.", m))
            .collect();

        let mut offenders: Vec<String> = Vec::new();
        let mut bodies_seen = 0usize;
        let mut current_fn: Option<String> = None;
        for line in src.lines() {
            let t = line.trim();
            if let Some(rest) = t.strip_prefix("async fn run_") {
                current_fn = rest.split('(').next().map(|f| format!("run_{f}"));
                bodies_seen += 1;
            } else if is_fn_definition(t) {
                // Left the `run_*` body. Every visibility spelling counts as a boundary, not just
                // a bare `async fn` — `pub(crate) async fn incident_signals` sits between two
                // `run_*` bodies, and missing it would attribute its lines to the previous one.
                current_fn = None;
            }
            if let Some(f) = &current_fn {
                if direct.iter().any(|n| line.contains(n.as_str())) {
                    offenders.push(format!("{f}: {}", t.trim()));
                }
            }
        }
        assert!(
            bodies_seen >= 15,
            "the source scan stopped matching `run_*` bodies (saw {bodies_seen})"
        );
        assert!(
            offenders.is_empty(),
            "these analyses read PostgreSQL directly and will answer from the alert-linked subset \
             when a log store is configured — route them through the `agg_*` helpers: {offenders:#?}"
        );
    }

    /// The push-down is what makes a group-scoped `rule_gap` correct.
    ///
    /// Before it, the analysis grouped fleet-wide and then kept a signature only if its
    /// *representative* node (the alphabetically smallest UUID) was in scope — so a signature
    /// genuinely occurring inside the caller's group disappeared whenever some out-of-group node
    /// sorted lower. Asserted on the filter rather than end-to-end because building a scoped run
    /// needs a database; what can be wrong here is which nodes reach the store.
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
        let f = AnalysisRunner::scoped_window(&scoped, &[in_group], 100, 200);
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
        assert!(AnalysisRunner::scoped_window(&all, &[in_group], 100, 200)
            .visible_node_ids
            .is_none());

        // An empty scope must stay `Some(vec![])` — "no visible nodes", which both backends match
        // nothing for. Collapsing it to `None` is the fail-open inversion.
        let empty = AnalysisRunner::scoped_window(&scoped, &[], 100, 200);
        assert_eq!(empty.visible_node_ids.as_deref(), Some(&[][..]));
    }

    #[test]
    fn token_and_serde_agree_for_every_schedule_status() {
        // The column value and the JSON tag come from two mechanisms — `as_str` and
        // `#[serde(rename_all)]` — and nothing else makes them agree.
        for s in AnalysisScheduleStatus::ALL.iter().copied() {
            let json = serde_json::to_string(&s).expect("status serializes");
            assert_eq!(json, format!("\"{}\"", s.as_str()));
            assert_eq!(AnalysisScheduleStatus::from_stored(s.as_str()), s);
        }
        assert_eq!(
            AnalysisScheduleStatus::from_stored("throttled"),
            AnalysisScheduleStatus::Unknown
        );
    }

    #[test]
    fn token_and_serde_agree_for_every_job_state() {
        for s in AnalysisJobState::ALL.iter().copied() {
            let json = serde_json::to_string(&s).expect("state serializes");
            assert_eq!(json, format!("\"{}\"", s.as_str()));
            assert_eq!(AnalysisJobState::from_stored(s.as_str()), s);
            // Deserialize too: unlike the schedule status, this one is read back off the wire —
            // the MCP DTO carries it and the filter parses it.
            let back: AnalysisJobState = serde_json::from_str(&json).expect("state parses");
            assert_eq!(back, s);
        }
        // The vocabulary a *report* run uses. Feeding it here has to fail, because the two lists
        // looking interchangeable is what put `succeeded` in the analysis path in the first place.
        assert_eq!(
            AnalysisJobState::from_stored("succeeded"),
            AnalysisJobState::Unknown
        );
        assert_eq!(AnalysisJobState::from_filter_token("succeeded"), None);
    }

    #[test]
    fn the_filter_vocabulary_is_everything_the_writers_can_produce_and_nothing_else() {
        // `Unknown` is the one variant nothing writes, so offering it as a filter would hand the
        // operator a confident empty answer.
        assert_eq!(AnalysisJobState::from_filter_token("unknown"), None);
        let listed = AnalysisJobState::filter_token_list();
        for s in AnalysisJobState::ALL.iter().copied() {
            let filterable = s != AnalysisJobState::Unknown;
            assert_eq!(
                AnalysisJobState::from_filter_token(s.as_str()).is_some(),
                filterable
            );
            assert_eq!(
                listed.split(", ").any(|t| t == s.as_str()),
                filterable,
                "{s:?} is listed in the 400 iff it is filterable"
            );
        }
    }

    #[test]
    fn the_job_state_sql_is_built_from_the_enum() {
        // `analysis_jobs.state` is written by statement literals, not by a bind, so without this
        // the enum would be the source only for the *reader* and a writer could drift away from it
        // silently. Needles are built at runtime: this test reads its own file, so a literal one
        // would match itself and pass forever.
        let src = include_str!("analysis.rs");
        // Executable code above the tests, comments stripped — the doc comment on
        // `is_filterable` names `state = 'unknown'` as the thing it refuses, and prose about a
        // literal must not read as the literal.
        let production = src
            .split_once("#[cfg(test)]")
            .map_or(src, |(before, _)| before)
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        for state in AnalysisJobState::ALL.iter().copied() {
            let bad = format!("'{}'", state.as_str());
            assert!(
                !production.contains(&bad),
                "{bad} is hardcoded in SQL again; interpolate AnalysisJobState::…as_str() instead"
            );
        }
        assert!(production.contains("AnalysisJobState::Running.as_str()"));
        assert!(production.contains("AnalysisJobState::Done.as_str()"));
        assert!(production.contains("AnalysisJobState::Cancelled.as_str()"));
    }

    #[test]
    fn the_findings_search_orders_on_exactly_the_columns_its_cursor_pages_on() {
        // The three that must agree or paging silently drops rows: the cursor predicate, the
        // ORDER BY, and the index in migration 0058. The events list has its own version of this
        // test because the same disagreement shipped there once.
        let sql = finding_search_sql();
        // The page size is derived from `FINDING_SEARCH_BINDS`, so this asserts the derivation is
        // right rather than re-hardcoding the number the derivation exists to stop anyone writing.
        assert!(
            sql.contains("ORDER BY f.created_at DESC, f.id DESC LIMIT $13"),
            "{sql}"
        );
        assert_eq!(FINDING_SEARCH_BINDS, 12);
        assert!(sql.contains(FINDING_SEARCH_WHERE), "{sql}");
        assert!(
            FINDING_SEARCH_WHERE.contains("(f.created_at, f.id) <"),
            "the cursor must be the row value, not the timestamp alone: {FINDING_SEARCH_WHERE}"
        );
        // Findings carry no tool of their own; `?tool=` is only answerable through the run.
        assert!(
            sql.contains("JOIN analysis_jobs j ON j.id = f.job_id"),
            "{sql}"
        );
    }

    #[test]
    fn the_findings_search_restricts_by_scope_unconditionally() {
        // The inversion that would be a privilege escalation: the caller's scope must be a clause
        // that is always in the statement, with NULL — not absence — meaning unrestricted. It must
        // also be bound, never interpolated (security.md).
        assert!(FINDING_SEARCH_WHERE.contains(
            "($7::uuid[] IS NULL OR f.node_id IN (SELECT id FROM nodes WHERE group_id = ANY($7)))"
        ));
        // …and the group the caller *asked* for is a separate bind, so dropping the request cannot
        // drop the restriction.
        assert!(FINDING_SEARCH_WHERE.contains("ANY($8)"));
        // Two kinds of quoted literal are allowed here and nothing else: the cursor's nil-uuid
        // floor, and the `'%'` wildcards the substring filters concatenate around a *bound* value.
        // Anything left after removing those means a request value reached SQL as text.
        let without_wildcards = FINDING_SEARCH_WHERE.replace("'%'", "");
        assert_eq!(
            without_wildcards.matches('\'').count(),
            2,
            "the nil-uuid cursor floor is the only non-wildcard literal that belongs here: \
             {FINDING_SEARCH_WHERE}"
        );
        // …and each wildcard sits beside a placeholder, never beside inlined text.
        for bind in ["$9", "$10"] {
            assert!(
                FINDING_SEARCH_WHERE.contains(&format!("'%' || {bind} || '%'")),
                "{bind} must be concatenated as a bound value: {FINDING_SEARCH_WHERE}"
            );
        }
    }

    #[test]
    fn the_score_bounds_are_inclusive_and_every_placeholder_is_used_once() {
        // Inclusive at both ends. `>` instead of `>=` is the version of this that looks right and
        // drops exactly the rows sitting on the bound the operator typed — invisible unless you go
        // looking, because the answer is still plausible.
        assert!(
            FINDING_SEARCH_WHERE.contains("($11::double precision IS NULL OR f.score >= $11)"),
            "{FINDING_SEARCH_WHERE}"
        );
        assert!(
            FINDING_SEARCH_WHERE.contains("($12::double precision IS NULL OR f.score <= $12)"),
            "{FINDING_SEARCH_WHERE}"
        );
        // Every placeholder the predicate declares is actually written, and none beyond it — this is
        // what makes `FINDING_SEARCH_BINDS` a fact about the string rather than a hopeful constant.
        // (`search_findings` binds them in order, so a gap here would silently shift every later
        // filter's value into the wrong clause.)
        for i in 1..=FINDING_SEARCH_BINDS {
            assert!(
                FINDING_SEARCH_WHERE.contains(&format!("${i}")),
                "${i} is declared by FINDING_SEARCH_BINDS but never used: {FINDING_SEARCH_WHERE}"
            );
        }
        assert!(
            !FINDING_SEARCH_WHERE.contains(&format!("${}", FINDING_SEARCH_BINDS + 1)),
            "the predicate uses the slot reserved for LIMIT: {FINDING_SEARCH_WHERE}"
        );
    }

    #[test]
    fn tool_round_trips() {
        // `as_str` is an exhaustive match, so a new variant cannot avoid getting a token — but
        // nothing forces it into `ALL`, and a tool missing from `ALL` is unparseable while still
        // being displayable. Pinning the expected tokens against `ALL` closes the gap either way
        // round: adding to `ALL` without updating this list fails on the length, and adding here
        // without updating `ALL` fails on the membership.
        let expected = [
            "anomaly",
            "correlation",
            "capacity",
            "flap",
            "event_storm",
            "event_flap",
            "severity_shift",
            "rule_gap",
            "auth_probe",
            "traffic_anomaly",
            "talker_shift",
            "new_destination",
            "flow_scan",
            "saturation",
            "incident_correlate",
        ];
        assert_eq!(AnalysisTool::ALL.len(), expected.len());
        for token in expected {
            let tool =
                AnalysisTool::from_str(token).unwrap_or_else(|| panic!("{token} must parse"));
            assert_eq!(tool.as_str(), token);
            assert!(AnalysisTool::ALL.contains(&tool));
        }
        assert_eq!(AnalysisTool::from_str("nope"), None);
        // The rejection message enumerates the real list rather than a hand-copied one.
        assert_eq!(AnalysisTool::token_list(), expected.join(", "));
    }

    #[test]
    fn storm_detail_carries_peak_at_for_a_localizable_label() {
        // `when_label` is pre-rendered English (`rel_label`), so the WebUI needs the raw peak time
        // to format a JA-correct relative label. Regression guard: don't drop `peak_at`.
        let d = storm_detail(42.0, 3.5, 1_700_000_000);
        assert_eq!(d["peak"], 42.0);
        assert_eq!(d["baseline_mean"], 3.5);
        assert_eq!(d["peak_at"], 1_700_000_000_i64);
        assert_eq!(d["bucket_secs"], EVENT_BUCKET_SECS);
    }

    #[test]
    fn traffic_detail_carries_peak_at_for_a_localizable_label() {
        let d = traffic_detail(1_048_576.0, 1024.0, 1_700_000_500);
        assert_eq!(d["peak_bytes"], 1_048_576.0);
        assert_eq!(d["baseline_mean_bytes"], 1024.0);
        assert_eq!(d["peak_at"], 1_700_000_500_i64);
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
        assert!(
            event_signal_severity(EventAction::Fired, None)
                > event_signal_severity(EventAction::Cleared, None)
        );
        assert!(
            event_signal_severity(EventAction::None, Some(0))
                > event_signal_severity(EventAction::None, Some(6)),
            "emergency syslog outweighs debug"
        );
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
