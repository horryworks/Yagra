// SPDX-License-Identifier: AGPL-3.0-only
//! MCP tools: what Yagra worked out: Troubleshoot runs, their findings, and an LLM root-cause account (ADR-086).
//!
//! Split out of the single `tools.rs` by ADR-086; the module doc for the surface as a whole,
//! and the rules every tool here obeys, are in [`super`].

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
// The module (not just the trait) — the `JsonSchema` derive expands to `schemars::…` paths, so the
// `schemars` name must be in scope. rmcp re-exports it, keeping exactly one schemars version.
use rmcp::schemars;
use rmcp::service::{RequestContext, RoleServer};
use rmcp::{tool, tool_router, ErrorData as McpError};
use serde::Deserialize;
use serde_json::Value;
use std::time::Instant;
use uuid::Uuid;
use yagra_common::{NodeId, Permission};

use super::YagraMcp;
use crate::api::scope::NodeScope;
use crate::mcp::dto::{AnalysisFindingDto, AnalysisJobDto};

// The shared scope: the helpers in `support.rs` and the types the other domain modules declare,
// re-exported by `mod.rs` so no file has to name where a sibling keeps a thing.
use super::*;

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(super) struct RunRcaParams {
    /// The alerting node's UUID.
    node_id: Uuid,
    /// The check that is alerting on it.
    check_id: Uuid,
    /// Timeline window in seconds (default 1h, clamped by the orchestrator).
    window_secs: Option<i64>,
    /// Answer language tag (`en`, `ja`, …). Unknown tags fall back to English.
    language: Option<String>,
    /// Regenerate instead of serving a recent cached answer. Still rate-limited.
    force: Option<bool>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(super) struct RunAnalysisParams {
    /// Which diagnostic to run: anomaly, correlation, capacity, flap (metric); event_storm,
    /// event_flap, severity_shift, rule_gap, auth_probe (passive events); traffic_anomaly,
    /// talker_shift, new_destination, flow_scan (flow); saturation, incident_correlate (cross-store).
    tool: String,
    /// Scope: all, group, or node (default all).
    scope: Option<String>,
    /// Group or node UUID (required when scope is group or node).
    scope_id: Option<Uuid>,
    /// Recent window to inspect, seconds (tool-specific floor applied; default 1 hour).
    window_secs: Option<i64>,
    /// Baseline lookback for anomaly detection, seconds (default 14 days).
    baseline_secs: Option<i64>,
    /// Anomaly sensitivity in σ (0.5–6.0, default 3.0); lower flags more.
    sensitivity: Option<f64>,
    /// Scan depth: quick, standard, or exhaustive (default standard) — caps how many nodes are scanned.
    depth: Option<String>,
    /// Metric family filter: all, reachability_interface, or system (default all).
    family: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(crate) struct AnalysisJobIdParams {
    /// The analysis job's UUID (from run_analysis or list_analyses).
    job_id: Uuid,
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
pub(super) struct ListAnalysesParams {
    /// What to list: runs (default) | schedules.
    kind: Option<String>,
    /// Max jobs to return (1–100, default 20). Applies to `runs` only.
    limit: Option<i64>,
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
pub(super) struct SearchFindingsParams {
    /// Page cursor: the `at` of the previous page's last row (RFC 3339).
    before: Option<String>,
    /// Page cursor tiebreak: that same row's `id`. Findings from one run share a millisecond
    /// routinely, so paging without it repeats or skips the rows on the boundary.
    before_id: Option<Uuid>,
    /// Inclusive lower bound on finding time (RFC 3339) — the range filter, not the cursor.
    since: Option<String>,
    /// Diagnostics to include, comma-separated: anomaly | correlation | capacity | flap. Omit for
    /// all. An unknown token is an error rather than being ignored.
    tool: Option<String>,
    /// Severities to include, comma-separated: crit | warn | info. Omit for all.
    severity: Option<String>,
    /// Case-insensitive substring of the metric name or the finding kind, e.g. `cpu`.
    q: Option<String>,
    /// Restrict to findings about one node.
    node_id: Option<Uuid>,
    /// Only findings about nodes whose name contains this text (case-insensitive). Use this rather
    /// than `node_id` when the question is about a set of nodes. Fleet-wide findings never match.
    node_q: Option<String>,
    /// Restrict to findings about nodes in one folder group and everything beneath it.
    group_id: Option<Uuid>,
    /// Lowest score to include, inclusive. Higher scores are worse, so this is the usual way to ask
    /// for only the serious findings.
    min_score: Option<f64>,
    /// Highest score to include, inclusive.
    max_score: Option<f64>,
    /// Page size, 1–200 (default 100).
    limit: Option<i64>,
}

/// Render a shared [`AnalysisReport`](crate::api::analysis::AnalysisReport) through this surface's
/// sanitized DTOs.
///
/// The *assembly* is shared — which job, whether its findings exist yet — but the *serialization*
/// is not, and deliberately so: `dto.rs` is the ADR-018 enforcement boundary, and letting an
/// internal model serialize itself straight to an AI client is exactly the leak that file exists to
/// prevent. Sharing the query while keeping the projection is the whole point of the split.
pub(super) fn analysis_report_body(report: &crate::api::analysis::AnalysisReport) -> Value {
    serde_json::json!({
        "job": AnalysisJobDto::from_job(&report.job),
        "findings": report.findings.iter().map(AnalysisFindingDto::from_finding).collect::<Vec<_>>(),
    })
}

#[tool_router(router = analysis_router, vis = "pub(super)")]
impl YagraMcp {
    #[tool(
        description = "Search Troubleshoot findings across every run — \"has anything been found \
                       about this node / this site / this week\". Distinct from \
                       get_analysis_findings, which reports one run you already know about. \
                       Filters: `node_id`, `node_q` (substring of the node's name), `group_id` (a \
                       folder and everything beneath it), `tool` (anomaly | correlation | capacity \
                       | flap), `severity` (crit | warn | info), `q` (substring of the metric name \
                       or the finding kind), and `since` (RFC 3339). `tool` and `severity` each \
                       take several values comma-separated. Page with `before` + `before_id` from \
                       the last row; `limit` is 1–200 (default 100). Returns [] on a deployment \
                       with no analysis runner."
    )]
    async fn search_analysis_findings(
        &self,
        Parameters(p): Parameters<SearchFindingsParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        const TOOL: &str = "search_analysis_findings";
        match self.scope_for(identity_of(&ctx)).await {
            Ok(scope) => self.search_analysis_findings_in(p, &scope).await,
            Err(e) => tool_api_error(TOOL, &e),
        }
    }

    pub(super) async fn search_analysis_findings_in(
        &self,
        p: SearchFindingsParams,
        scope: &NodeScope,
    ) -> Result<CallToolResult, McpError> {
        const TOOL: &str = "search_analysis_findings";
        // The seam validates the tool/severity vocabularies and the cursor, and scope-checks a
        // named node or group, *before* it looks at availability — so a filter naming a node the
        // caller cannot see reads as "no such node" rather than as an empty result set.
        let q = crate::api::analysis::SavedFindingsQuery {
            before: p.before,
            before_id: p.before_id,
            since: p.since,
            tool: p.tool,
            severity: p.severity,
            q: p.q,
            node_id: p.node_id,
            node_q: p.node_q,
            group_id: p.group_id,
            min_score: p.min_score,
            max_score: p.max_score,
            limit: p.limit,
        };
        match crate::api::analysis::search_saved_findings(&self.state, scope, q).await {
            Ok(rows) => ok_json(TOOL, &rows),
            Err(e) => tool_api_error(TOOL, &e),
        }
    }

    #[tool(
        description = "Run an on-demand Troubleshoot analysis and wait for its findings. Requires \
                       ack-alerts permission (Operator and up) — it reads metric/event/flow history \
                       and never notifies or changes device configuration, but a run is expensive, \
                       so launching one is gated like other incident-response actions. `tool` is one \
                       of: metric — anomaly, correlation, \
                       capacity, flap; passive events — event_storm, event_flap, severity_shift, \
                       rule_gap, auth_probe; flow — traffic_anomaly, talker_shift, new_destination, \
                       flow_scan; cross-store — saturation, incident_correlate. (flow_* need the flow \
                       tier enabled, else they return an info finding.) `scope` is all|group|node \
                       (default all); for group/node pass `scope_id` (a UUID). Optional `window_secs`, \
                       `baseline_secs`, `sensitivity` (0.5–6.0), `depth` (quick|standard|exhaustive), \
                       and `family` (all|reachability_interface|system) tune the run. Blocks up to ~2 \
                       minutes; if still running it returns the job id to poll with \
                       get_analysis_findings. Rate-limited; requires live mode."
    )]
    async fn run_analysis(
        &self,
        Parameters(p): Parameters<RunAnalysisParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        const TOOL: &str = "run_analysis";
        // Operator and up, matching `POST /api/v1/analysis/jobs`. An analysis reads what a Viewer
        // could already query, so this gate is not about disclosure — it is that launching one is
        // incident-response work with a real compute cost, and the two surfaces must agree on who
        // may do it. They did not: this was `View` while REST was admin-only.
        let Some(identity) = authed_for(identity_of(&ctx), Permission::AckAlerts) else {
            return tool_forbidden(TOOL, "this token lacks ack-alerts permission");
        };
        let visible = match self.scope_for(identity_of(&ctx)).await {
            Ok(s) => s,
            Err(e) => return tool_api_error(TOOL, &e),
        };
        let Some(admin) = self.state.admin.as_ref() else {
            return tool_unavailable(TOOL, "analysis requires live mode");
        };
        let scope = p.scope.as_deref().unwrap_or("all");
        // The same launch-target rule as `POST /api/v1/analysis/jobs`: a run's findings are read
        // back later, so an over-broad launch would hand a scoped caller fleet-wide data through
        // the results. Checked before the label lookup — an invalid target should not first cost a
        // name query, and the label must not name a node the caller cannot see.
        if let Err(e) =
            crate::api::analysis::require_launchable_scope(&self.state, &visible, scope, p.scope_id)
        {
            return tool_api_error(TOOL, &e);
        }
        // A readable label for the runs list (mirrors the WebUI's launch drawer).
        let scope_label = match (scope, p.scope_id) {
            ("node", Some(id)) => self
                .resolve_names(&visible, std::iter::once(id))
                .await
                .get(&id)
                .cloned()
                .unwrap_or_else(|| format!("node {id}")),
            ("group", Some(id)) => format!("group {id}"),
            _ => "All nodes".to_owned(),
        };
        // Same validation and the same clamps as `POST /api/v1/analysis/jobs` — these were written
        // out twice, and two copies of a *validation* rule is the worst kind to keep, because when
        // they drift the looser one becomes the boundary.
        //
        // `notify: false` is this surface's one deliberate difference: a read-only MCP run must
        // never trigger the single external side effect an analysis has (ADR-028 Increment 2).
        let req = crate::api::analysis::AnalysisRequest {
            tool: p.tool.clone(),
            scope_kind: scope.to_owned(),
            scope_id: p.scope_id,
            scope_label,
            window_secs: p.window_secs.unwrap_or(DEFAULT_WINDOW_SECS),
            baseline_secs: p.baseline_secs,
            sensitivity: p.sensitivity,
            depth: p.depth.clone(),
            family: p.family.clone(),
            notify: false,
        };
        let launched = crate::api::analysis::launch(admin, req, Some("mcp".to_owned())).await;
        // Audited here, deliberately. `POST /api/v1/analysis/jobs` gets its row from `audit_mw`,
        // which is REST-only middleware — so the identical job launched through the identical
        // `launch()` seam over MCP produced no record at all. Two surfaces that can start the same
        // work must leave the same trace, or the audit log quietly answers "nobody" for half of it.
        record_audit(
            &self.state,
            &identity,
            &format!("mcp.run_analysis tool={}", p.tool),
            match &launched {
                Ok(_) => 202,
                Err(e) => e.status().as_u16(),
            },
        )
        .await;
        let job = match launched {
            Ok(j) => j,
            // 429 (capacity/rate) arrives here as a *successful* unavailable-with-reason result so
            // the model retries rather than treating a transient refusal as a hard failure.
            Err(e) => return tool_api_error(TOOL, &e),
        };
        // Block-poll until the job reaches a terminal state or the wait budget is spent.
        let deadline = Instant::now() + ANALYSIS_MAX_WAIT;
        let final_job = loop {
            match admin.analysis.get(job.id).await {
                Ok(Some(j)) if j.state.is_terminal() => break j,
                Ok(Some(j)) => {
                    if Instant::now() >= deadline {
                        let body = serde_json::json!({
                            "job": AnalysisJobDto::from_job(&j),
                            "findings": Vec::<AnalysisFindingDto>::new(),
                            "note": "analysis still running after the wait budget; \
                                     call get_analysis_findings with job.id to fetch results",
                        });
                        return ok_json_value(TOOL, body);
                    }
                    tokio::time::sleep(ANALYSIS_POLL).await;
                }
                Ok(None) => return tool_unavailable(TOOL, "job vanished before completion"),
                Err(e) => return tool_error(TOOL, "poll analysis", &e),
            }
        };
        match crate::api::analysis::report(admin, final_job.id).await {
            Ok(r) => ok_json_value(TOOL, self.scoped_report_body(&visible, r)),
            Err(e) => tool_api_error(TOOL, &e),
        }
    }

    #[tool(
        description = "Fetch a Troubleshoot analysis job and its findings by job id (from run_analysis \
                       or list_analyses). Use this to poll a run that was still in progress. Requires \
                       live mode."
    )]
    async fn get_analysis_findings(
        &self,
        Parameters(p): Parameters<AnalysisJobIdParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        const TOOL: &str = "get_analysis_findings";
        match self.scope_for(identity_of(&ctx)).await {
            Ok(scope) => self.analysis_findings_in(p, &scope).await,
            Err(e) => tool_api_error(TOOL, &e),
        }
    }

    pub(crate) async fn analysis_findings_in(
        &self,
        p: AnalysisJobIdParams,
        scope: &NodeScope,
    ) -> Result<CallToolResult, McpError> {
        const TOOL: &str = "get_analysis_findings";
        let Some(admin) = self.state.admin.as_ref() else {
            return tool_unavailable(TOOL, "analysis requires live mode");
        };
        let report = match crate::api::analysis::report(admin, p.job_id).await {
            Ok(r) => r,
            Err(e) => return tool_api_error(TOOL, &e),
        };
        // The run row is the unit of visibility — somebody else's fleet-wide run is not readable
        // just because its id was guessed or came from a shared transcript.
        if let Err(e) = crate::api::analysis::require_visible_job(&self.state, scope, &report.job) {
            return tool_api_error(TOOL, &e);
        }
        ok_json_value(TOOL, self.scoped_report_body(scope, report))
    }

    #[tool(
        description = "List Troubleshoot analyses. `kind` is runs (default: recent jobs, newest \
                       first, with their tool, scope, state and result summary) or schedules (the \
                       recurring analyses configured on this deployment, with their cadence, next \
                       run and last status). `limit` applies to runs only, 1–100 (default 20). \
                       Requires live mode."
    )]
    async fn list_analyses(
        &self,
        Parameters(p): Parameters<ListAnalysesParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        const TOOL: &str = "list_analyses";
        match self.scope_for(identity_of(&ctx)).await {
            Ok(scope) => self.list_analyses_in(p, &scope).await,
            Err(e) => tool_api_error(TOOL, &e),
        }
    }

    pub(super) async fn list_analyses_in(
        &self,
        p: ListAnalysesParams,
        scope: &NodeScope,
    ) -> Result<CallToolResult, McpError> {
        const TOOL: &str = "list_analyses";
        // Folded onto the runs list rather than given its own tool: both answer "what analysis is
        // there", both are post-filtered on the row's own target, and a schedule is what a run will
        // be. The `limit` only applies to runs, which is why it is documented that way.
        match p.kind.as_deref().unwrap_or("runs") {
            "runs" => {}
            "schedules" => {
                return match crate::api::analysis::visible_schedules(&self.state, scope).await {
                    Ok(rows) => ok_json(TOOL, &rows),
                    Err(e) => tool_api_error(TOOL, &e),
                };
            }
            other => {
                return tool_bad_params(
                    TOOL,
                    &format!("unknown kind {other:?}; must be runs or schedules"),
                );
            }
        }
        let Some(admin) = self.state.admin.as_ref() else {
            return tool_unavailable(TOOL, "analysis requires live mode");
        };
        // A smaller page than the REST default (50): an AI client reads the runs list to orient,
        // not to render a table.
        let limit = p.limit.unwrap_or(20).clamp(1, 100);
        // No filter: `list_analyses` answers "what has run recently", and a model narrows by
        // reading the rows rather than by re-asking. The seam is shared so the cap cannot differ.
        let jobs = match admin.analysis.list(limit, &Default::default()).await {
            Ok(js) => js,
            Err(e) => return tool_error(TOOL, "list analyses", &e),
        };
        // Post-filtered on each run's own target, matching `GET /api/v1/analysis/jobs`. A short
        // page is correct: this is "recent activity", not a cursor-paged collection.
        let out: Vec<AnalysisJobDto> = jobs
            .iter()
            .filter(|j| scope.allows_target(&self.state, crate::api::analysis::row_target(j)))
            .map(AnalysisJobDto::from_job)
            .collect();
        ok_json(TOOL, &out)
    }

    #[tool(
        description = "Ask the configured LLM to explain one incident, grounded in the node's \
                       alert, timeline, dependents and recent config changes. Needs the \
                       ack-alerts permission and is audited, because unlike every other read here \
                       it spends money at an external provider. A recent identical request is \
                       served from cache unless `force` is set. Returns an availability note when \
                       no provider is configured, or when the rate cap is reached. Reads only — it \
                       changes no configuration and touches no device."
    )]
    async fn run_rca(
        &self,
        Parameters(p): Parameters<RunRcaParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        const TOOL: &str = "run_rca";
        // Holds the identity rather than just the scope: it is both the audit actor and the
        // `created_by` stamped on the stored report, exactly as `Actor` is over REST.
        let want = crate::mcp::folded::required_permission(TOOL, "");
        let Some(identity) = authed_for(identity_of(&ctx), want) else {
            return tool_forbidden(
                TOOL,
                &format!("this token lacks {} permission", permission_label(want)),
            );
        };
        let scope = match self.scope_for(identity_of(&ctx)).await {
            Ok(s) => s,
            Err(e) => return tool_api_error(TOOL, &e),
        };
        let req = crate::rca::orchestrator::RcaRequest {
            node: NodeId::from(p.node_id),
            check: yagra_common::CheckId::from(p.check_id),
            window_secs: p.window_secs,
            language: crate::rca::prompt::Language::from_tag(p.language.as_deref().unwrap_or("en")),
            force: p.force.unwrap_or(false),
            // The stored report's `created_by`, and the audit actor below — one value for both, as
            // `Actor` is over REST.
            username: identity.actor.clone(),
            // Set by `explain_incident` from the scope it checks; empty here so a path that skipped
            // that step would see nothing rather than the fleet.
            scope: NodeScope::sees_nothing(),
        };
        let result = crate::api::rca::explain_incident(&self.state, &scope, req).await;
        // Audited on both outcomes. A refusal is as interesting as a success here — it is the
        // record that someone tried to spend the provider budget.
        let status = match &result {
            Ok(_) => 200,
            Err(e) => e.status().as_u16(),
        };
        record_audit(
            &self.state,
            &identity,
            &format!("mcp.run_rca node={}", p.node_id),
            status,
        )
        .await;
        match result {
            Ok(report) => ok_json(TOOL, &report),
            Err(e) => tool_api_error(TOOL, &e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::tools::testkit::*;

    // ── Pure helpers ────────────────────────────────────────────────────────────────────────────

    #[test]
    fn the_wait_loop_stops_on_every_state_that_is_not_still_moving() {
        use crate::analysis::AnalysisJobState as S;
        // The loop this drives polls until terminal, so a wrong answer here is either a tool that
        // returns a half-finished run or one that never returns at all. `Unknown` is terminal
        // deliberately: a token this build cannot read is one it will never learn to read.
        for s in [S::Done, S::Failed, S::Cancelled, S::Unknown] {
            assert!(s.is_terminal(), "{s:?} should stop the wait");
        }
        for s in [S::Running, S::Queued] {
            assert!(!s.is_terminal(), "{s:?} should keep waiting");
        }
        // Every variant is covered above — a new state must be classified, not defaulted.
        assert_eq!(S::ALL.len(), 6);
    }

    /// A findings search naming a node the caller cannot see answers "no such node", not `[]`.
    /// The seam checks scope before it reaches the store, so this holds on a skeleton state too.
    #[tokio::test]
    async fn a_findings_search_hides_a_node_the_caller_cannot_see() {
        let r = mcp()
            .search_analysis_findings_in(
                SearchFindingsParams {
                    node_id: Some(Uuid::nil()),
                    ..Default::default()
                },
                &sees_nothing(),
            )
            .await
            .expect("ok result");
        assert_eq!(json_of(&r)["available"], serde_json::json!(false));
    }

    /// A bad filter vocabulary is rejected rather than dropped — dropping `severity` would widen
    /// the search silently, which is the edge-parsing rule in `security.md`.
    #[tokio::test]
    async fn a_findings_search_rejects_an_unknown_severity() {
        assert!(
            mcp()
                .search_analysis_findings_in(
                    SearchFindingsParams {
                        severity: Some("fatal".to_owned()),
                        ..Default::default()
                    },
                    &unrestricted(),
                )
                .await
                .is_err(),
            "an unknown severity is a protocol error"
        );
    }

    /// `kind` picks the row type, and an unknown one is rejected rather than silently listing runs
    /// — the failure that would otherwise look like "this deployment has no schedules".
    #[tokio::test]
    async fn listing_analyses_distinguishes_runs_from_schedules() {
        let m = mcp();
        // Skeleton mode has no runner, so schedules answer an empty list where runs report the
        // subsystem as unavailable. Both are correct, and they differ: a search over nothing is
        // legitimately empty, while the runs list is a view of a store that is not there.
        let schedules = m
            .list_analyses_in(
                ListAnalysesParams {
                    kind: Some("schedules".to_owned()),
                    limit: None,
                },
                &unrestricted(),
            )
            .await
            .expect("ok result");
        assert_eq!(json_of(&schedules), serde_json::json!([]));

        assert!(
            m.list_analyses_in(
                ListAnalysesParams {
                    kind: Some("everything".to_owned()),
                    limit: None,
                },
                &unrestricted(),
            )
            .await
            .is_err(),
            "an unknown kind is a protocol error"
        );
    }
}
