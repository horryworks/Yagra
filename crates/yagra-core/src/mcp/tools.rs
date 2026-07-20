// SPDX-License-Identifier: AGPL-3.0-only
//! The read-only MCP tool surface (ADR-028 Increments 1 & 2).
//!
//! Each `#[tool]` is a thin adapter over a read seam on [`ApiState`](crate::api::ApiState): it parses
//! typed params, calls the seam, maps the result into a **sanitized DTO** ([`crate::mcp::dto`]), and
//! records a metric. Tool bodies never serialize a raw model/row (ADR-018) and never trigger device
//! I/O. Increment 2 added: the Troubleshoot tools (`run_analysis` / `get_analysis_findings` /
//! `list_analyses`) — a Troubleshoot analysis is a **read** that forces `notify = false` and is
//! bounded by the runner's rate/concurrency limits rather than a role; the event reader
//! (`search_events`, WS-C); and the first **write** tools (`ack_alert`, `open_maintenance`,
//! `poll_now`, WS-E). A write tool reads the authenticated [`McpIdentity`] from its `RequestContext`
//! (propagated via the HTTP request `Parts`, WS-D), enforces its own [`Permission`]
//! (`AckAlerts`/`ManageMaintenance`/`ManageConfig`), and records an audit entry. There are still no
//! device-configuration tools (monitoring lane, ADR-015/029). Group-scope visibility on reads and the
//! heavier writes (`run_probe`/`trigger_discovery`) remain future work. The plain-`async fn` shape
//! (params in, DTO out) is what lets the ADR-029 RCA agent reuse these bodies in-process later.

use chrono::{DateTime, Utc};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock};
// The module (not just the trait) — the `JsonSchema` derive expands to `schemars::…` paths, so the
// `schemars` name must be in scope. rmcp re-exports it, keeping exactly one schemars version.
use rmcp::schemars;
use rmcp::service::{RequestContext, RoleServer};
use rmcp::{tool, tool_router, ErrorData as McpError};
use serde::Deserialize;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};
use uuid::Uuid;
use yagra_common::{NodeId, Permission, SeriesKey, Severity};

use super::{McpIdentity, YagraMcp};
use crate::ack::AckView;
use crate::analysis::{AnalysisTool, CreateError, JobParams, ScopeKind};
use crate::api::ApiState;
use crate::events::EventFilter;
use crate::flowstore::{AsDir, FlowQuery};
use crate::mcp::dto::{
    AlertDto, AlertHistoryDto, AnalysisFindingDto, AnalysisJobDto, EventDto, FleetSummaryDto,
    InterfaceDto, MetricPointDto, MetricSeriesDto, NodeStatusDto, NodeSummaryDto, TopologyEdgeDto,
};

/// Default window (seconds) for range/rate metric and flow queries when `from`/`to` are omitted.
const DEFAULT_WINDOW_SECS: i64 = 3600;
/// How long `run_analysis` blocks polling for a job to finish before returning it still-running.
const ANALYSIS_MAX_WAIT: Duration = Duration::from_secs(120);
/// Poll interval while `run_analysis` waits for a job to reach a terminal state.
const ANALYSIS_POLL: Duration = Duration::from_millis(750);

#[tool_router]
impl YagraMcp {
    /// Construct the handler over the shared API state, building the macro-generated tool router.
    pub(crate) fn new(state: crate::api::ApiState) -> Self {
        Self {
            state,
            tool_router: Self::tool_router(),
        }
    }

    #[tool(
        description = "Fleet health summary: total node count, node counts per rolled-up state \
                       (ok/warning/critical/unknown/unreachable/maintenance), the number of active \
                       alerts, and which optional data tiers (metrics/flow/log) are enabled. Start here."
    )]
    async fn get_fleet_summary(&self) -> Result<CallToolResult, McpError> {
        let counts = self.state.alerts.node_state_counts();
        let total = self.state.nodes.count().await.unwrap_or(0);
        let observed: i64 = counts.values().map(|&n| n as i64).sum();
        let mut states: BTreeMap<String, i64> = BTreeMap::new();
        for (st, n) in &counts {
            states.insert(st.as_str().to_owned(), *n as i64);
        }
        // Never-observed nodes read as "unknown" (total minus what the engine has seen).
        let unknown = (total - observed).max(0);
        if unknown > 0 {
            *states.entry("unknown".to_owned()).or_insert(0) += unknown;
        }
        let dto = FleetSummaryDto {
            total_nodes: total,
            states,
            active_alerts: self.state.alerts.active_alerts().len(),
            metrics_healthy: self.state.store.healthy().await,
            flow_tier_enabled: self.state.flows.is_some(),
            log_tier_enabled: self.state.logs.is_some(),
        };
        ok_json("get_fleet_summary", &dto)
    }

    #[tool(
        description = "List monitored nodes with their rolled-up state. Optional case-insensitive \
                       `search` matches name or address; `limit` is 1–100 (default 50). Returns \
                       node id, name, address, state, parent, group, vendor, model, and tags."
    )]
    async fn list_nodes(
        &self,
        Parameters(p): Parameters<ListNodesParams>,
    ) -> Result<CallToolResult, McpError> {
        let limit = p.limit.unwrap_or(50).clamp(1, 100);
        let nodes = match &p.search {
            Some(term) => self.state.nodes.search(term, limit).await,
            None => self.state.nodes.list_page(None, limit).await,
        };
        let nodes = match nodes {
            Ok(n) => n,
            Err(e) => return tool_error("list_nodes", "list nodes", &e),
        };
        let states = self.state.alerts.node_states();
        let out: Vec<NodeSummaryDto> = nodes
            .iter()
            .map(|n| NodeSummaryDto::from_node(n, states.get(&n.id).copied()))
            .collect();
        ok_json("list_nodes", &out)
    }

    #[tool(
        description = "Full status for one node: its summary, current active alerts, and interfaces. \
                       Requires live mode (returns an availability note in skeleton mode)."
    )]
    async fn get_node_status(
        &self,
        Parameters(p): Parameters<NodeIdParams>,
    ) -> Result<CallToolResult, McpError> {
        let Some(admin) = self.state.admin.as_ref() else {
            return tool_unavailable("get_node_status", "node detail requires live mode");
        };
        let node = match admin.repo.get_node(p.node_id).await {
            Ok(Some(n)) => n,
            Ok(None) => return tool_unavailable("get_node_status", "no node with that id"),
            Err(e) => return tool_error("get_node_status", "load node", &e),
        };
        let nid = NodeId::from(p.node_id);
        let state = self.state.alerts.node_state(nid);
        let alerts = self.state.alerts.alerts_for(nid);
        let interfaces = admin
            .repo
            .list_interfaces(p.node_id)
            .await
            .unwrap_or_default();
        let dto = NodeStatusDto {
            node: NodeSummaryDto::from_node(&node, state),
            // Every alert here is on this node, so its name is this node's name.
            alerts: alerts
                .iter()
                .map(|a| AlertDto::from_alert(a, Some(node.name.clone())))
                .collect(),
            interfaces: interfaces.iter().map(InterfaceDto::from_meta).collect(),
        };
        ok_json("get_node_status", &dto)
    }

    #[tool(
        description = "Currently active alerts, newest first. Optional `node_id` filters to one node; \
                       `min_severity` is info|warning|critical; `limit` is 1–500 (default 100). Node \
                       names are resolved when available."
    )]
    async fn get_active_alerts(
        &self,
        Parameters(p): Parameters<ActiveAlertsParams>,
    ) -> Result<CallToolResult, McpError> {
        let mut alerts = self.state.alerts.active_alerts();
        if let Some(node_id) = p.node_id {
            let nid = NodeId::from(node_id);
            alerts.retain(|a| a.node == nid);
        }
        if let Some(min) = p.min_severity.as_deref() {
            let min_rank = severity_rank(min);
            alerts.retain(|a| severity_rank(a.severity.as_str()) >= min_rank);
        }
        alerts.sort_by_key(|a| std::cmp::Reverse(a.at_unix_ms));
        let limit = p.limit.unwrap_or(100).clamp(1, 500);
        alerts.truncate(limit);
        let names = self.resolve_names(alerts.iter().map(|a| a.node.0)).await;
        let out: Vec<AlertDto> = alerts
            .iter()
            .map(|a| AlertDto::from_alert(a, names.get(&a.node.0).cloned()))
            .collect();
        ok_json("get_active_alerts", &out)
    }

    #[tool(
        description = "Recent alert history (fires and clears), newest first. `limit` is 1–1000 \
                       (default 100); `before` is an RFC 3339 timestamp for keyset paging (pass the \
                       oldest row's `at` to fetch the next page). Requires live mode."
    )]
    async fn get_alert_history(
        &self,
        Parameters(p): Parameters<AlertHistoryParams>,
    ) -> Result<CallToolResult, McpError> {
        let Some(history) = self.state.history.as_ref() else {
            return tool_unavailable("get_alert_history", "alert history requires live mode");
        };
        let limit = p.limit.unwrap_or(100).clamp(1, 1000);
        let before = match p.before.as_deref() {
            Some(s) => match chrono::DateTime::parse_from_rfc3339(s) {
                Ok(dt) => Some(dt.with_timezone(&Utc)),
                Err(_) => {
                    return tool_bad_params(
                        "get_alert_history",
                        "`before` must be an RFC 3339 timestamp",
                    )
                }
            },
            None => None,
        };
        let rows = match history.recent(limit, before).await {
            Ok(r) => r,
            Err(e) => return tool_error("get_alert_history", "load history", &e),
        };
        let names = self.resolve_names(rows.iter().map(|r| r.node)).await;
        let out: Vec<AlertHistoryDto> = rows
            .iter()
            .map(|r| AlertHistoryDto::from_row(r, names.get(&r.node).cloned()))
            .collect();
        ok_json("get_alert_history", &out)
    }

    #[tool(
        description = "Query a node's metric time-series from the TSDB. `metric` is a name such as \
                       icmp_rtt_ms, cpu_percent, or mem_percent. `mode` is latest|range|rate \
                       (default latest). For range/rate, `from`/`to` are Unix seconds (default: last \
                       hour) and `step` is the sample interval in seconds (clamped)."
    )]
    async fn query_metrics(
        &self,
        Parameters(p): Parameters<QueryMetricsParams>,
    ) -> Result<CallToolResult, McpError> {
        if !crate::api::is_valid_metric_name(&p.metric) {
            return tool_bad_params("query_metrics", "invalid metric name");
        }
        let key = SeriesKey::node(NodeId::from(p.node_id), p.metric.clone());
        let mode = p.mode.as_deref().unwrap_or("latest");
        let dto = match mode {
            "latest" => MetricSeriesDto {
                node_id: p.node_id,
                metric: p.metric.clone(),
                mode: "latest".to_owned(),
                latest: self.state.store.latest(&key).await,
                points: Vec::new(),
            },
            "range" | "rate" => {
                let to = p.to.unwrap_or_else(|| Utc::now().timestamp());
                let from = p.from.unwrap_or(to - DEFAULT_WINDOW_SECS);
                if from >= to {
                    return tool_bad_params("query_metrics", "`from` must be earlier than `to`");
                }
                let step = crate::api::clamp_range_step(from, to, p.step.unwrap_or(60), 1);
                let points = if mode == "rate" {
                    self.state
                        .store
                        .rate_range(&key, from, to, step, step.max(60))
                        .await
                } else {
                    self.state.store.range(&key, from, to, step).await
                };
                MetricSeriesDto {
                    node_id: p.node_id,
                    metric: p.metric.clone(),
                    mode: mode.to_owned(),
                    latest: None,
                    points: points
                        .iter()
                        .map(|pt| MetricPointDto { t: pt.t, v: pt.v })
                        .collect(),
                }
            }
            _ => return tool_bad_params("query_metrics", "`mode` must be latest, range, or rate"),
        };
        ok_json("query_metrics", &dto)
    }

    #[tool(
        description = "The dependency-graph edges (id, name, upstream parent) in keyset pages. \
                       `after` is a cursor UUID (return nodes with a greater id); `limit` is 1–1000 \
                       (default 200). Requires live mode."
    )]
    async fn get_topology(
        &self,
        Parameters(p): Parameters<TopologyParams>,
    ) -> Result<CallToolResult, McpError> {
        let Some(admin) = self.state.admin.as_ref() else {
            return tool_unavailable("get_topology", "topology requires live mode");
        };
        let limit = p.limit.unwrap_or(200).clamp(1, 1000);
        let rows = match admin.repo.list_topology_page(p.after, limit).await {
            Ok(r) => r,
            Err(e) => return tool_error("get_topology", "load topology", &e),
        };
        let out: Vec<TopologyEdgeDto> = rows.iter().map(TopologyEdgeDto::from_row).collect();
        ok_json("get_topology", &out)
    }

    #[tool(
        description = "Top traffic flows for a node from the flow tier. `kind` is \
                       talkers|conversations|ports|protocols|as (default talkers). `from`/`to` are \
                       Unix seconds (default: last hour); `limit` is clamped by the store. Returns an \
                       availability note when the flow tier is not enabled."
    )]
    async fn top_flows(
        &self,
        Parameters(p): Parameters<TopFlowsParams>,
    ) -> Result<CallToolResult, McpError> {
        let Some(flows) = self.state.flows.as_ref() else {
            return tool_unavailable("top_flows", "flow tier not enabled on this core");
        };
        let to_s = p.to.unwrap_or_else(|| Utc::now().timestamp());
        let from_s = p.from.unwrap_or(to_s - DEFAULT_WINDOW_SECS);
        let q = FlowQuery {
            node_id: p.node_id,
            from_unix_ms: from_s.saturating_mul(1000),
            to_unix_ms: to_s.saturating_mul(1000),
            limit: p.limit.unwrap_or(100),
            proto: None,
            dst_port: None,
            peer: None,
            asn: None,
        };
        let kind = p.kind.as_deref().unwrap_or("talkers");
        let result: anyhow::Result<Value> = match kind {
            "talkers" => flows
                .top_talkers(&q)
                .await
                .and_then(|r| serde_json::to_value(r).map_err(Into::into)),
            "conversations" => flows
                .top_conversations(&q)
                .await
                .and_then(|r| serde_json::to_value(r).map_err(Into::into)),
            "ports" => flows
                .top_ports(&q)
                .await
                .and_then(|r| serde_json::to_value(r).map_err(Into::into)),
            "protocols" => flows
                .top_protocols(&q)
                .await
                .and_then(|r| serde_json::to_value(r).map_err(Into::into)),
            "as" => flows
                .top_as(&q, AsDir::Dst)
                .await
                .and_then(|r| serde_json::to_value(r).map_err(Into::into)),
            _ => {
                return tool_bad_params(
                    "top_flows",
                    "`kind` must be talkers, conversations, ports, protocols, or as",
                )
            }
        };
        match result {
            Ok(value) => ok_json_value("top_flows", value),
            Err(e) => tool_error("top_flows", "query flows", &e),
        }
    }

    #[tool(
        description = "Run an on-demand Troubleshoot analysis and wait for its findings (read-only: it \
                       reads metric history and returns findings, and never notifies or changes device \
                       configuration). `tool` is anomaly|correlation|capacity|flap. `scope` is \
                       all|group|node (default all); for group/node pass `scope_id` (a UUID). Optional \
                       `window_secs`, `baseline_secs`, `sensitivity` (0.5–6.0), `depth` \
                       (quick|standard|exhaustive), and `family` (all|reachability_interface|system) \
                       tune the run. Blocks up to ~2 minutes; if the job is still running it returns \
                       the job id to poll with get_analysis_findings. Rate-limited; requires live mode."
    )]
    async fn run_analysis(
        &self,
        Parameters(p): Parameters<RunAnalysisParams>,
    ) -> Result<CallToolResult, McpError> {
        let Some(admin) = self.state.admin.as_ref() else {
            return tool_unavailable("run_analysis", "analysis requires live mode");
        };
        let Some(tool) = AnalysisTool::from_str(&p.tool) else {
            return tool_bad_params(
                "run_analysis",
                "`tool` must be anomaly, correlation, capacity, or flap",
            );
        };
        let scope = p.scope.as_deref().unwrap_or("all");
        let Some(scope_kind) = ScopeKind::from_str(scope) else {
            return tool_bad_params("run_analysis", "`scope` must be all, group, or node");
        };
        let scope_id = p.scope_id;
        if scope_kind != ScopeKind::All && scope_id.is_none() {
            return tool_bad_params(
                "run_analysis",
                "`scope_id` is required for group or node scope",
            );
        }
        // A readable label for the runs list (mirrors the WebUI's launch drawer).
        let scope_label = match &scope_kind {
            ScopeKind::All => "All nodes".to_owned(),
            ScopeKind::Node => match scope_id {
                Some(id) => self
                    .resolve_names(std::iter::once(id))
                    .await
                    .get(&id)
                    .cloned()
                    .unwrap_or_else(|| format!("node {id}")),
                None => "node".to_owned(),
            },
            ScopeKind::Group => {
                scope_id.map_or_else(|| "group".to_owned(), |id| format!("group {id}"))
            }
        };
        // Clamp numerics like the REST edge; force `notify = false` — a read-only MCP run must never
        // trigger the one external side effect (a notification). ADR-028 Increment 2 design decision.
        let params = JobParams {
            tool,
            scope_kind,
            scope_id,
            scope_label,
            window_secs: p
                .window_secs
                .unwrap_or(DEFAULT_WINDOW_SECS)
                .clamp(300, 365 * 86_400),
            baseline_secs: p
                .baseline_secs
                .unwrap_or(14 * 86_400)
                .clamp(3600, 365 * 86_400),
            sensitivity: p.sensitivity.unwrap_or(3.0).clamp(0.5, 6.0),
            depth: p.depth.clone().unwrap_or_else(|| "standard".to_owned()),
            family: p.family.clone().unwrap_or_else(|| "all".to_owned()),
            notify: false,
        };
        let job = match admin.analysis.create(params, Some("mcp".to_owned())).await {
            Ok(j) => j,
            // Capacity/rate rejections are transient — present as unavailable-with-reason so the model
            // retries rather than treating it as a hard failure.
            Err(e @ (CreateError::TooManyConcurrent(_) | CreateError::RateLimited(_))) => {
                return tool_unavailable("run_analysis", &e.to_string());
            }
            Err(CreateError::Internal(e)) => {
                return tool_error("run_analysis", "create analysis", &e)
            }
        };
        // Block-poll until the job reaches a terminal state or the wait budget is spent.
        let deadline = Instant::now() + ANALYSIS_MAX_WAIT;
        let final_job = loop {
            match admin.analysis.get(job.id).await {
                Ok(Some(j)) if is_terminal(&j.state) => break j,
                Ok(Some(j)) => {
                    if Instant::now() >= deadline {
                        let body = serde_json::json!({
                            "job": AnalysisJobDto::from_job(&j),
                            "findings": Vec::<AnalysisFindingDto>::new(),
                            "note": "analysis still running after the wait budget; \
                                     call get_analysis_findings with job.id to fetch results",
                        });
                        return ok_json_value("run_analysis", body);
                    }
                    tokio::time::sleep(ANALYSIS_POLL).await;
                }
                Ok(None) => {
                    return tool_unavailable("run_analysis", "job vanished before completion")
                }
                Err(e) => return tool_error("run_analysis", "poll analysis", &e),
            }
        };
        // Findings exist only on success; a failed/cancelled job carries its state/error instead.
        let findings: Vec<AnalysisFindingDto> = if final_job.state == "done" {
            match admin.analysis.findings(final_job.id).await {
                Ok(fs) => fs.iter().map(AnalysisFindingDto::from_finding).collect(),
                Err(e) => return tool_error("run_analysis", "load findings", &e),
            }
        } else {
            Vec::new()
        };
        let body = serde_json::json!({
            "job": AnalysisJobDto::from_job(&final_job),
            "findings": findings,
        });
        ok_json_value("run_analysis", body)
    }

    #[tool(
        description = "Fetch a Troubleshoot analysis job and its findings by job id (from run_analysis \
                       or list_analyses). Use this to poll a run that was still in progress. Requires \
                       live mode."
    )]
    async fn get_analysis_findings(
        &self,
        Parameters(p): Parameters<AnalysisJobIdParams>,
    ) -> Result<CallToolResult, McpError> {
        let Some(admin) = self.state.admin.as_ref() else {
            return tool_unavailable("get_analysis_findings", "analysis requires live mode");
        };
        let job = match admin.analysis.get(p.job_id).await {
            Ok(Some(j)) => j,
            Ok(None) => {
                return tool_unavailable("get_analysis_findings", "no analysis job with that id")
            }
            Err(e) => return tool_error("get_analysis_findings", "load job", &e),
        };
        let findings: Vec<AnalysisFindingDto> = match admin.analysis.findings(p.job_id).await {
            Ok(fs) => fs.iter().map(AnalysisFindingDto::from_finding).collect(),
            Err(e) => return tool_error("get_analysis_findings", "load findings", &e),
        };
        let body = serde_json::json!({
            "job": AnalysisJobDto::from_job(&job),
            "findings": findings,
        });
        ok_json_value("get_analysis_findings", body)
    }

    #[tool(
        description = "List recent Troubleshoot analysis jobs (the runs list), newest first, with \
                       their tool, scope, state, and result summary. `limit` is 1–100 (default 20). \
                       Requires live mode."
    )]
    async fn list_analyses(
        &self,
        Parameters(p): Parameters<ListAnalysesParams>,
    ) -> Result<CallToolResult, McpError> {
        let Some(admin) = self.state.admin.as_ref() else {
            return tool_unavailable("list_analyses", "analysis requires live mode");
        };
        let limit = p.limit.unwrap_or(20).clamp(1, 100);
        let jobs = match admin.analysis.list(limit).await {
            Ok(js) => js,
            Err(e) => return tool_error("list_analyses", "list analyses", &e),
        };
        let out: Vec<AnalysisJobDto> = jobs.iter().map(AnalysisJobDto::from_job).collect();
        ok_json("list_analyses", &out)
    }

    #[tool(
        description = "Search received passive events (syslog / SNMP traps / webhooks), newest first. \
                       Optional `search` (case-insensitive substring over source/message, or a \
                       message-only regex when `regex` is true), `kind` (syslog|trap|webhook), \
                       `node_id`, `matched` (only rule-matched events), `since`/`until` (RFC 3339), and \
                       `limit` (1–500, default 100). Requires live mode."
    )]
    async fn search_events(
        &self,
        Parameters(p): Parameters<EventSearchParams>,
    ) -> Result<CallToolResult, McpError> {
        let Some(admin) = self.state.admin.as_ref() else {
            return tool_unavailable("search_events", "event search requires live mode");
        };
        let since = match parse_opt_rfc3339(p.since.as_deref()) {
            Ok(v) => v,
            Err(()) => {
                return tool_bad_params("search_events", "`since` must be an RFC 3339 timestamp")
            }
        };
        let until = match parse_opt_rfc3339(p.until.as_deref()) {
            Ok(v) => v,
            Err(()) => {
                return tool_bad_params("search_events", "`until` must be an RFC 3339 timestamp")
            }
        };
        if let Some(kind) = p.kind.as_deref() {
            if !matches!(kind, "syslog" | "trap" | "webhook") {
                return tool_bad_params("search_events", "`kind` must be syslog, trap, or webhook");
            }
        }
        let regex = p.regex.unwrap_or(false);
        let search = p
            .search
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        // Validate a regex at the edge (size / ReDoS guard, shared with rule compilation).
        if regex {
            if let Some(term) = search.as_deref() {
                if let Err(e) = crate::events::compile_matcher("regex", term) {
                    return tool_bad_params(
                        "search_events",
                        &format!("invalid regular expression: {e}"),
                    );
                }
            }
        }
        let filter = EventFilter {
            before: None,
            since,
            until,
            kind: p.kind.clone(),
            node_id: p.node_id,
            matched: p.matched,
            search,
            regex,
        };
        let limit = p.limit.unwrap_or(100).clamp(1, 500);
        // Mirrors the REST events reader: the log store is the search source of record when enabled
        // (free-text node-name search resolves to ids here so the name never enters the store), else
        // fall back to the PostgreSQL alert-linked rows.
        let rows = if let Some(logs) = self.state.logs.as_ref() {
            let name_node_ids = match (filter.regex, filter.search.as_deref()) {
                (false, Some(term)) => admin
                    .repo
                    .node_ids_by_name_like(term, 50)
                    .await
                    .unwrap_or_default(),
                _ => Vec::new(),
            };
            logs.search(&filter, &name_node_ids, limit).await
        } else {
            admin.events.list_events(&filter, limit).await
        };
        let rows = match rows {
            Ok(r) => r,
            Err(e) => return tool_error("search_events", "search events", &e),
        };
        let names = self
            .resolve_names(rows.iter().filter_map(|r| r.node_id))
            .await;
        let out: Vec<EventDto> = rows
            .iter()
            .map(|r| EventDto::from_row(r, r.node_id.and_then(|id| names.get(&id).cloned())))
            .collect();
        ok_json("search_events", &out)
    }

    #[tool(
        description = "Acknowledge an active alert (or clear its ack). Requires ack-alerts permission. \
                       Identify the alert by `node_id` + `check_id` + `severity` — all from \
                       get_active_alerts or get_node_status. `acked` defaults true; set false to clear. \
                       Optional `note`. Requires live mode."
    )]
    async fn ack_alert(
        &self,
        Parameters(p): Parameters<AckAlertParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let Some(identity) = authed_for(&ctx, Permission::AckAlerts) else {
            return tool_forbidden("ack_alert", "this token lacks ack-alerts permission");
        };
        let Some(ack) = self.state.ack.as_ref() else {
            return tool_unavailable("ack_alert", "ack requires live mode");
        };
        let Some(severity) = parse_severity(&p.severity) else {
            return tool_bad_params("ack_alert", "`severity` must be info, warning, or critical");
        };
        if p.acked.unwrap_or(true) {
            let view = AckView {
                at_unix_ms: Utc::now().timestamp_millis(),
                by: identity.actor.clone(),
                source: "mcp".to_owned(),
                note: p.note.clone(),
            };
            if let Err(e) = ack
                .set(p.node_id, p.check_id, severity.as_str(), &view)
                .await
            {
                return tool_error("ack_alert", "record ack", &e);
            }
            self.state.alerts.broadcast_acked(
                p.node_id,
                p.check_id,
                severity,
                serde_json::to_value(&view).ok(),
            );
            record_audit(
                &self.state,
                &identity,
                &format!(
                    "mcp.ack_alert node={} check={} sev={}",
                    p.node_id,
                    p.check_id,
                    severity.as_str()
                ),
                200,
            )
            .await;
            ok_json_value(
                "ack_alert",
                serde_json::json!({ "acked": true, "node_id": p.node_id, "check_id": p.check_id }),
            )
        } else {
            if let Err(e) = ack.clear(p.node_id, p.check_id, severity.as_str()).await {
                return tool_error("ack_alert", "clear ack", &e);
            }
            self.state
                .alerts
                .broadcast_acked(p.node_id, p.check_id, severity, None);
            record_audit(
                &self.state,
                &identity,
                &format!(
                    "mcp.ack_alert(clear) node={} check={} sev={}",
                    p.node_id,
                    p.check_id,
                    severity.as_str()
                ),
                200,
            )
            .await;
            ok_json_value(
                "ack_alert",
                serde_json::json!({ "acked": false, "node_id": p.node_id, "check_id": p.check_id }),
            )
        }
    }

    #[tool(
        description = "Open a maintenance window for one node so its alerts are suppressed for a period. \
                       Requires manage-maintenance permission. Give `node_id` and either \
                       `duration_mins` (from now, default 60, max 10080) or explicit `starts_at`/ \
                       `ends_at` (RFC 3339). Optional `name`. Requires live mode."
    )]
    async fn open_maintenance(
        &self,
        Parameters(p): Parameters<OpenMaintenanceParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let Some(identity) = authed_for(&ctx, Permission::ManageMaintenance) else {
            return tool_forbidden(
                "open_maintenance",
                "this token lacks manage-maintenance permission",
            );
        };
        let Some(admin) = self.state.admin.as_ref() else {
            return tool_unavailable("open_maintenance", "maintenance requires live mode");
        };
        let (starts, ends) = match (p.starts_at.as_deref(), p.ends_at.as_deref()) {
            (Some(s), Some(e)) => {
                let (Some(s), Some(e)) = (parse_rfc3339_ok(s), parse_rfc3339_ok(e)) else {
                    return tool_bad_params(
                        "open_maintenance",
                        "`starts_at`/`ends_at` must be RFC 3339 timestamps",
                    );
                };
                (s, e)
            }
            (None, None) => {
                let mins = p.duration_mins.unwrap_or(60).clamp(1, 7 * 24 * 60);
                let now = Utc::now();
                (now, now + chrono::Duration::minutes(mins))
            }
            _ => {
                return tool_bad_params(
                    "open_maintenance",
                    "provide both starts_at and ends_at, or neither (use duration_mins)",
                )
            }
        };
        if ends <= starts {
            return tool_bad_params("open_maintenance", "the window must end after it starts");
        }
        let node = match admin.repo.get_node(p.node_id).await {
            Ok(Some(n)) => n,
            Ok(None) => return tool_unavailable("open_maintenance", "no node with that id"),
            Err(e) => return tool_error("open_maintenance", "load node", &e),
        };
        let name = p
            .name
            .clone()
            .unwrap_or_else(|| format!("MCP maintenance — {}", node.name));
        match admin
            .maintenance
            .create_window(&name, "node", &p.node_id.to_string(), starts, ends)
            .await
        {
            Ok(id) => {
                record_audit(
                    &self.state,
                    &identity,
                    &format!("mcp.open_maintenance node={} window={id}", p.node_id),
                    201,
                )
                .await;
                ok_json_value(
                    "open_maintenance",
                    serde_json::json!({
                        "created": true,
                        "window_id": id,
                        "node_id": p.node_id,
                        "starts_at": starts.to_rfc3339(),
                        "ends_at": ends.to_rfc3339(),
                    }),
                )
            }
            Err(e) => tool_error("open_maintenance", "create maintenance window", &e),
        }
    }

    #[tool(
        description = "Trigger an immediate, out-of-schedule poll of one node. Requires manage-config \
                       permission. Returns whether a poll job was dispatched. Requires live mode."
    )]
    async fn poll_now(
        &self,
        Parameters(p): Parameters<NodeIdParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let Some(identity) = authed_for(&ctx, Permission::ManageConfig) else {
            return tool_forbidden("poll_now", "this token lacks manage-config permission");
        };
        let Some(admin) = self.state.admin.as_ref() else {
            return tool_unavailable("poll_now", "poll requires live mode");
        };
        let node = match admin.repo.get_node(p.node_id).await {
            Ok(Some(n)) => n,
            Ok(None) => return tool_unavailable("poll_now", "no node with that id"),
            Err(e) => return tool_error("poll_now", "load node", &e),
        };
        let dispatched = admin.poll.poll_now(&node).await;
        record_audit(
            &self.state,
            &identity,
            &format!("mcp.poll_now node={}", p.node_id),
            202,
        )
        .await;
        ok_json_value(
            "poll_now",
            serde_json::json!({ "dispatched": dispatched, "node_id": p.node_id }),
        )
    }

    /// Resolve node ids → display names via the live repo (empty in skeleton mode). Deduplicated so a
    /// repeated node in an alert list doesn't bloat the `IN (…)` query.
    async fn resolve_names(&self, ids: impl Iterator<Item = Uuid>) -> HashMap<Uuid, String> {
        let ids: Vec<Uuid> = {
            let mut seen: Vec<Uuid> = ids.collect();
            seen.sort_unstable();
            seen.dedup();
            seen
        };
        if ids.is_empty() {
            return HashMap::new();
        }
        match self.state.admin.as_ref() {
            Some(admin) => admin.repo.node_names(&ids).await.unwrap_or_default(),
            None => HashMap::new(),
        }
    }
}

// ── Tool parameter structs (schemas derived for `tools/list`) ─────────────────────────────────────

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ListNodesParams {
    /// Case-insensitive substring matched against node name or address.
    search: Option<String>,
    /// Max nodes to return (1–100, default 50).
    limit: Option<i64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct NodeIdParams {
    /// The node's UUID.
    node_id: Uuid,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ActiveAlertsParams {
    /// Restrict to this node's alerts (UUID).
    node_id: Option<Uuid>,
    /// Minimum severity: info, warning, or critical.
    min_severity: Option<String>,
    /// Max alerts to return (1–500, default 100).
    limit: Option<usize>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct AlertHistoryParams {
    /// Max rows to return (1–1000, default 100).
    limit: Option<i64>,
    /// Only rows recorded before this RFC 3339 timestamp (keyset paging).
    before: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct QueryMetricsParams {
    /// The node's UUID.
    node_id: Uuid,
    /// Metric name (e.g. icmp_rtt_ms, cpu_percent, mem_percent).
    metric: String,
    /// Query mode: latest, range, or rate (default latest).
    mode: Option<String>,
    /// Range start, Unix seconds (range/rate modes).
    from: Option<i64>,
    /// Range end, Unix seconds (range/rate modes).
    to: Option<i64>,
    /// Sample step in seconds (range/rate modes; clamped to bound the point count).
    step: Option<u64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct TopologyParams {
    /// Keyset cursor: return nodes with an id greater than this UUID.
    after: Option<Uuid>,
    /// Max edges to return (1–1000, default 200).
    limit: Option<i64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct TopFlowsParams {
    /// The node's UUID whose flows to query.
    node_id: Uuid,
    /// Aggregation: talkers, conversations, ports, protocols, or as (default talkers).
    kind: Option<String>,
    /// Window start, Unix seconds (default: one hour ago).
    from: Option<i64>,
    /// Window end, Unix seconds (default: now).
    to: Option<i64>,
    /// Max rows to return (clamped by the store).
    limit: Option<u32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct RunAnalysisParams {
    /// Which diagnostic to run: anomaly, correlation, capacity, or flap.
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
struct AnalysisJobIdParams {
    /// The analysis job's UUID (from run_analysis or list_analyses).
    job_id: Uuid,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ListAnalysesParams {
    /// Max jobs to return (1–100, default 20).
    limit: Option<i64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct EventSearchParams {
    /// Case-insensitive substring over source/message (or a message-only regex when `regex` is true).
    search: Option<String>,
    /// Interpret `search` as a regular expression (message-only) rather than a substring.
    regex: Option<bool>,
    /// Restrict to an event kind: syslog, trap, or webhook.
    kind: Option<String>,
    /// Restrict to one node's events (UUID).
    node_id: Option<Uuid>,
    /// Only events that matched an event rule (raised/cleared an alert).
    matched: Option<bool>,
    /// Time-range lower bound, RFC 3339.
    since: Option<String>,
    /// Time-range upper bound, RFC 3339.
    until: Option<String>,
    /// Max events to return (1–500, default 100).
    limit: Option<i64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct AckAlertParams {
    /// The alerting node's UUID.
    node_id: Uuid,
    /// The alert's check UUID (the `check_id` field from get_active_alerts / get_node_status).
    check_id: Uuid,
    /// The alert severity: info, warning, or critical.
    severity: String,
    /// True to acknowledge (default), false to clear a prior ack.
    acked: Option<bool>,
    /// Optional free-text note recorded with the acknowledgement.
    note: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct OpenMaintenanceParams {
    /// The node to place into maintenance (UUID).
    node_id: Uuid,
    /// Window length in minutes from now (default 60, max 10080). Ignored if starts_at/ends_at given.
    duration_mins: Option<i64>,
    /// Explicit window start, RFC 3339 (must be paired with ends_at).
    starts_at: Option<String>,
    /// Explicit window end, RFC 3339 (must be paired with starts_at).
    ends_at: Option<String>,
    /// Optional window name (defaults to a generated label).
    name: Option<String>,
}

// ── Result / metric helpers ───────────────────────────────────────────────────────────────────────

/// Serialize a DTO to a pretty-JSON tool result (records an `ok` outcome).
fn ok_json<T: serde::Serialize>(tool: &str, value: &T) -> Result<CallToolResult, McpError> {
    match serde_json::to_string_pretty(value) {
        Ok(text) => {
            record_tool(tool, "ok");
            Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
        }
        Err(e) => tool_error(tool, "serialize result", &anyhow::Error::new(e)),
    }
}

/// Serialize an already-built JSON value to a pretty-JSON tool result (records `ok`).
fn ok_json_value(tool: &str, value: Value) -> Result<CallToolResult, McpError> {
    let text = serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string());
    record_tool(tool, "ok");
    Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
}

/// A "feature not available here" answer (records `unavailable`). Returned as a **successful** result
/// with an explanatory body so the model understands the tier is off rather than seeing a hard error.
fn tool_unavailable(tool: &str, reason: &str) -> Result<CallToolResult, McpError> {
    record_tool(tool, "unavailable");
    let body = serde_json::json!({ "available": false, "reason": reason });
    Ok(CallToolResult::success(vec![ContentBlock::text(
        body.to_string(),
    )]))
}

/// A bad-parameter error (records `bad_params`). Maps to a JSON-RPC invalid-params error.
fn tool_bad_params(tool: &str, reason: &str) -> Result<CallToolResult, McpError> {
    record_tool(tool, "bad_params");
    Err(McpError::invalid_params(reason.to_string(), None))
}

/// An internal tool error (records `error`). Logs the context + underlying error but returns a
/// generic message to the client — never a raw internal error string (coding-conventions / security).
fn tool_error(tool: &str, context: &str, err: &anyhow::Error) -> Result<CallToolResult, McpError> {
    record_tool(tool, "error");
    tracing::warn!(tool, error = %err, "MCP tool error while {context}");
    Err(McpError::internal_error(context.to_string(), None))
}

/// Increment the per-tool call counter (self-observability).
fn record_tool(tool: &str, outcome: &str) {
    metrics::counter!("yagra_mcp_tool_calls_total", "tool" => tool.to_owned(), "outcome" => outcome.to_owned())
        .increment(1);
}

/// Whether an analysis job has reached a terminal lifecycle state (no further progress).
fn is_terminal(state: &str) -> bool {
    matches!(state, "done" | "failed" | "cancelled")
}

/// The authenticated caller `mcp_auth_mw` inserted into the request extensions, if it has `perm`
/// (WS-D). rmcp forwards the HTTP request `Parts` into the tool's `RequestContext`, so the identity
/// is read back from `parts.extensions`. Fail-closed: `None` ⇒ the write tool returns forbidden.
fn authed_for(ctx: &RequestContext<RoleServer>, perm: Permission) -> Option<McpIdentity> {
    ctx.extensions
        .get::<axum::http::request::Parts>()
        .and_then(|parts| parts.extensions.get::<McpIdentity>())
        .filter(|id| id.principal.can(perm))
        .cloned()
}

/// Best-effort audit record for an MCP write tool (a store hiccup must never fail the action — the
/// side effect already happened; log and move on).
async fn record_audit(state: &ApiState, identity: &McpIdentity, action: &str, status: u16) {
    if let Some(admin) = state.admin.as_ref() {
        if let Err(e) = admin.audit.record(&identity.actor, action, status).await {
            tracing::warn!(error = %e, action, "MCP audit record failed");
        }
    }
}

/// Parse a severity string (info|warning|critical) into the enum. `None` on anything else.
fn parse_severity(s: &str) -> Option<Severity> {
    match s.trim().to_ascii_lowercase().as_str() {
        "info" => Some(Severity::Info),
        "warning" => Some(Severity::Warning),
        "critical" => Some(Severity::Critical),
        _ => None,
    }
}

/// Parse an RFC 3339 timestamp to UTC. `None` if malformed.
fn parse_rfc3339_ok(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

/// Parse an optional RFC 3339 timestamp: `Ok(None)` when absent, `Err(())` when present but malformed.
fn parse_opt_rfc3339(s: Option<&str>) -> Result<Option<DateTime<Utc>>, ()> {
    match s {
        None => Ok(None),
        Some(v) => parse_rfc3339_ok(v).map(Some).ok_or(()),
    }
}

/// A permission-denied tool result (records `forbidden`). Maps to a JSON-RPC invalid-request error.
fn tool_forbidden(tool: &str, reason: &str) -> Result<CallToolResult, McpError> {
    record_tool(tool, "forbidden");
    Err(McpError::invalid_request(reason.to_string(), None))
}

/// Rank a severity string for the `min_severity` filter (unknown ⇒ lowest).
fn severity_rank(sev: &str) -> u8 {
    match sev {
        "critical" => 2,
        "warning" => 1,
        _ => 0,
    }
}
