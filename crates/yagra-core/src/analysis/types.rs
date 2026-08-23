// SPDX-License-Identifier: AGPL-3.0-only
//! The analysis module's vocabulary: what an analysis *is*, what it is run over, what it produces
//! (ADR-089, split out of `analysis.rs`).
//!
//! Nothing here reads a store or runs anything. [`AnalysisTool::ALL`] is the enumeration every other
//! list of analyses is derived from — the parser, the flow-tier question, and the MCP tool's
//! description all come from it, because when they were written out separately the three disagreed.

use super::*;

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
    pub(super) fn to_json(&self) -> serde_json::Value {
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
    pub(super) fn node_cap(&self) -> usize {
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
pub(super) struct NewFinding {
    pub(super) score: f64,
    pub(super) severity: String,
    pub(super) node_id: Option<Uuid>,
    pub(super) node_name: String,
    pub(super) metric: String,
    pub(super) kind: String,
    pub(super) when_label: String,
    pub(super) duration: String,
    pub(super) detail: serde_json::Value,
}

/// The three severity buckets a finding can carry, most severe first.
///
/// One list, because there are now two readers: [`severity_for`] writes them and the Saved-findings
/// search validates `?severity=` against them. A hand-written copy of the set at the API edge is the
/// duplicated-constant trap `extensibility.md` names — and the copy that drifts would be the one
/// that decides which values a client is allowed to ask for.
pub const FINDING_SEVERITIES: [&str; 3] = [SEV_CRIT, SEV_WARN, SEV_INFO];
pub(super) const SEV_CRIT: &str = "crit";
pub(super) const SEV_WARN: &str = "warn";
pub(super) const SEV_INFO: &str = "info";

/// Severity bucket from a 0..100 score (matches the WebUI: ≥90 crit, ≥75 warn, else info).
pub(super) fn severity_for(score: f64) -> &'static str {
    if score >= 90.0 {
        SEV_CRIT
    } else if score >= 75.0 {
        SEV_WARN
    } else {
        SEV_INFO
    }
}

/// Joined node name, falling back to the id string when unknown.
pub(super) fn name_lookup(names: &HashMap<Uuid, String>, id: &Uuid) -> String {
    names.get(id).cloned().unwrap_or_else(|| id.to_string())
}

pub(super) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

pub(super) fn now_s() -> i64 {
    now_ms() / 1000
}

// ── Repository ───────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

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
}
