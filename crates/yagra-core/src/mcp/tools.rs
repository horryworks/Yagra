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

use chrono::Utc;
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
use crate::api::{ApiError, ApiState};
use crate::flowstore::{AsDir, FlowAsAgg, FlowConversation, FlowQuery};
use crate::mcp::dto::{
    AlertDto, AlertHistoryDto, AnalysisFindingDto, AnalysisJobDto, EventDto, FleetSummaryDto,
    InterfaceDto, MetricPointDto, MetricSeriesDto, NodeStatusDto, NodeSummaryDto,
};
use axum::http::StatusCode;

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
        // Same tally as `GET /api/v1/fleet/summary`. This used to be a second implementation that
        // inserted only the states it had observed, so an AI client reading `states["warning"]`
        // got a missing key where the WebUI got a zero — the kind of difference that only shows up
        // as a model confidently reporting there is no warning data.
        let summary = crate::api::fleet::state_tally(&self.state).await;
        let dto = FleetSummaryDto {
            total_nodes: summary.total,
            states: summary
                .states
                .into_iter()
                .map(|(k, v)| (k.to_owned(), v))
                .collect(),
            // Beyond the shared tally, this surface adds what an AI client needs to know before
            // trusting an answer: how much is currently wrong, and which tiers exist at all.
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
        // The same state resolution the REST list uses, including its recent-RTT fallback for a
        // node the alert engine has not observed yet. Reading `node_states()` directly — which is
        // what this did — skips that, so a just-added node (or every node, in the window after a
        // core restart) reported `unknown` here while the dashboard showed it `ok`.
        let ids: Vec<NodeId> = nodes.iter().map(|n| n.id).collect();
        let states = crate::api::nodes::display_states(&self.state, &ids).await;
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
        // Same fallback as `list_nodes` above and as the REST detail view: the engine's opinion, or
        // a recent RTT sample when it has none.
        let state = crate::api::nodes::display_state(&self.state, nid).await;
        let alerts = self.state.alerts.alerts_for(nid);
        let interfaces = admin
            .repo
            .list_interfaces(p.node_id)
            .await
            .unwrap_or_default();
        let dto = NodeStatusDto {
            node: NodeSummaryDto::from_node(&node, Some(state)),
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
        description = "The dependency graph in keyset pages: each node with its upstream parent, \
                       current state, and the upstream node blamed for its alert (root_cause), plus \
                       a `next_cursor` for the following page. `after` is a cursor UUID (return \
                       nodes with a greater id); `limit` is 1–1000 (default 200). Requires live mode."
    )]
    async fn get_topology(
        &self,
        Parameters(p): Parameters<TopologyParams>,
    ) -> Result<CallToolResult, McpError> {
        let Some(admin) = self.state.admin.as_ref() else {
            return tool_unavailable("get_topology", "topology requires live mode");
        };
        // Same assembly as `GET /api/v1/topology`; only the page bound differs, because an AI
        // client wants a handful of edges where the graph view wants the fleet.
        let limit = p.limit.unwrap_or(200).clamp(1, 1000);
        match crate::api::topology::topology_page(&self.state, admin, p.after, limit).await {
            Ok(page) => ok_json("get_topology", &page),
            Err(e) => tool_api_error("get_topology", &e),
        }
    }

    #[tool(
        description = "Top traffic flows for a node from the flow tier. `kind` is \
                       talkers|conversations|ports|protocols|as (default talkers). `from`/`to` are \
                       Unix seconds (default: last hour); `limit` is clamped by the store. Optional \
                       drill-down filters: `proto`, `port`, `peer` (an IP), `asn`; for kind=as, `dir` \
                       is src|dst (default dst). Conversations/as rows carry resolved AS names when the \
                       IP→ASN table is loaded. Returns an availability note when the flow tier is off."
    )]
    async fn top_flows(
        &self,
        Parameters(p): Parameters<TopFlowsParams>,
    ) -> Result<CallToolResult, McpError> {
        let Some(flows) = self.state.flows.as_ref() else {
            return tool_unavailable("top_flows", "flow tier not enabled on this core");
        };
        let q = flow_query_from(&p);
        let dir = if p.dir.as_deref() == Some("src") {
            AsDir::Src
        } else {
            AsDir::Dst
        };
        let kind = p.kind.as_deref().unwrap_or("talkers");
        let result: anyhow::Result<Value> = match kind {
            "talkers" => flows
                .top_talkers(&q)
                .await
                .and_then(|r| serde_json::to_value(r).map_err(Into::into)),
            "conversations" => flows.top_conversations(&q).await.and_then(|mut r| {
                self.resolve_conversation_as_names(&mut r);
                serde_json::to_value(r).map_err(Into::into)
            }),
            "ports" => flows
                .top_ports(&q)
                .await
                .and_then(|r| serde_json::to_value(r).map_err(Into::into)),
            "protocols" => flows
                .top_protocols(&q)
                .await
                .and_then(|r| serde_json::to_value(r).map_err(Into::into)),
            "as" => flows.top_as(&q, dir).await.and_then(|mut r| {
                self.resolve_as_names(&mut r);
                serde_json::to_value(r).map_err(Into::into)
            }),
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
        description = "Per-source flow fan-out for a node: how many distinct destinations and \
                       destination ports each source contacted, highest fan-out first (scan / worm \
                       triage). Same window/filters as top_flows (`from`/`to`/`limit`/`proto`/`port`/\
                       `peer`/`asn`). Returns an availability note when the flow tier is off."
    )]
    async fn flow_fanout(
        &self,
        Parameters(p): Parameters<TopFlowsParams>,
    ) -> Result<CallToolResult, McpError> {
        let Some(flows) = self.state.flows.as_ref() else {
            return tool_unavailable("flow_fanout", "flow tier not enabled on this core");
        };
        match flows.fanout_by_src(&flow_query_from(&p)).await {
            Ok(rows) => ok_json("flow_fanout", &rows),
            Err(e) => tool_error("flow_fanout", "query flow fan-out", &e),
        }
    }

    #[tool(
        description = "Run an on-demand Troubleshoot analysis and wait for its findings (read-only: it \
                       reads metric/event/flow history and returns findings, and never notifies or \
                       changes device configuration). `tool` is one of: metric — anomaly, correlation, \
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
    ) -> Result<CallToolResult, McpError> {
        let Some(admin) = self.state.admin.as_ref() else {
            return tool_unavailable("run_analysis", "analysis requires live mode");
        };
        // A readable label for the runs list (mirrors the WebUI's launch drawer). Built before
        // validation because it needs a name lookup; an invalid scope is rejected either way.
        let scope = p.scope.as_deref().unwrap_or("all");
        let scope_label = match (scope, p.scope_id) {
            ("node", Some(id)) => self
                .resolve_names(std::iter::once(id))
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
        let job = match crate::api::analysis::launch(admin, req, Some("mcp".to_owned())).await {
            Ok(j) => j,
            // 429 (capacity/rate) arrives here as a *successful* unavailable-with-reason result so
            // the model retries rather than treating a transient refusal as a hard failure.
            Err(e) => return tool_api_error("run_analysis", &e),
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
        match crate::api::analysis::report(admin, final_job.id).await {
            Ok(r) => ok_json_value("run_analysis", analysis_report_body(&r)),
            Err(e) => tool_api_error("run_analysis", &e),
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
    ) -> Result<CallToolResult, McpError> {
        let Some(admin) = self.state.admin.as_ref() else {
            return tool_unavailable("get_analysis_findings", "analysis requires live mode");
        };
        match crate::api::analysis::report(admin, p.job_id).await {
            Ok(r) => ok_json_value("get_analysis_findings", analysis_report_body(&r)),
            Err(e) => tool_api_error("get_analysis_findings", &e),
        }
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
        // A smaller page than the REST default (50): an AI client reads the runs list to orient,
        // not to render a table.
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
        // Same validation edge as `GET /api/v1/events`. This was a second copy, and the copies had
        // drifted on the term-length cap: the REST edge capped it and this one did not, so the
        // surface with no human in the loop was the one that could send an unbounded term to the
        // store. `since`/`until` are this surface's names for `start`/`end`.
        let filter =
            match crate::api::eventlog::parse_event_filter(crate::api::eventlog::EventFilterInput {
                before: None,
                start: p.since.as_deref(),
                end: p.until.as_deref(),
                kind: p.kind.clone(),
                node_id: p.node_id,
                matched: p.matched,
                q: p.search.as_deref(),
                regex: p.regex.unwrap_or(false),
            }) {
                Ok(f) => f,
                Err(e) => return tool_api_error("search_events", &e),
            };
        let limit = p.limit.unwrap_or(100).clamp(1, 500);
        // Same store routing too, including resolving a node-name term to ids so the name never
        // enters the log store (ADR-011).
        let rows = match crate::api::eventlog::search(&self.state, admin, &filter, limit).await {
            Ok(r) => r,
            Err(e) => return tool_api_error("search_events", &e),
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
        description = "Passive-event statistics for triage over a window: the noisiest nodes by event \
                       volume, the syslog severity mix, and the top unmatched-event signatures (rule \
                       gaps to consider writing). `from`/`to` are Unix seconds (default: last 24h); \
                       optional `node_id` narrows the volume/severity views. Requires live mode."
    )]
    async fn event_stats(
        &self,
        Parameters(p): Parameters<EventStatsParams>,
    ) -> Result<CallToolResult, McpError> {
        let Some(admin) = self.state.admin.as_ref() else {
            return tool_unavailable("event_stats", "event stats requires live mode");
        };
        let to_s = p.to.unwrap_or_else(|| Utc::now().timestamp());
        let from_s = p.from.unwrap_or(to_s - 86_400);
        let (from_ms, to_ms) = (from_s.saturating_mul(1000), to_s.saturating_mul(1000));
        // Volume: sum per-node hourly buckets over the window.
        let buckets = match admin
            .events
            .event_counts_by_bucket(from_ms, to_ms, 3600)
            .await
        {
            Ok(b) => b,
            Err(e) => return tool_error("event_stats", "event volume", &e),
        };
        let mut per_node: HashMap<Uuid, i64> = HashMap::new();
        for b in &buckets {
            if p.node_id.is_none_or(|n| n == b.node_id) {
                *per_node.entry(b.node_id).or_default() += b.count;
            }
        }
        let mut top_nodes: Vec<(Uuid, i64)> = per_node.into_iter().collect();
        top_nodes.sort_by_key(|n| std::cmp::Reverse(n.1));
        top_nodes.truncate(20);
        let names = self
            .resolve_names(top_nodes.iter().map(|(id, _)| *id))
            .await;
        let volume: Vec<Value> = top_nodes
            .iter()
            .map(|(id, c)| {
                serde_json::json!({ "node_id": id, "node_name": names.get(id).cloned(), "count": c })
            })
            .collect();
        // Severity mix.
        let sev = admin
            .events
            .event_severity_counts(from_ms, to_ms)
            .await
            .unwrap_or_default();
        let mut sev_mix: BTreeMap<i16, i64> = BTreeMap::new();
        for s in &sev {
            if p.node_id.is_none_or(|n| n == s.node_id) {
                *sev_mix.entry(s.severity).or_default() += s.count;
            }
        }
        let severity_mix: Vec<Value> = sev_mix
            .iter()
            .map(|(sev, c)| serde_json::json!({ "severity": sev, "count": c }))
            .collect();
        // Top unmatched signatures (rule gaps) — cross-node, so the node filter isn't applied here.
        let sigs = admin
            .events
            .event_unmatched_signatures(from_ms, to_ms, 20)
            .await
            .unwrap_or_default();
        let unmatched: Vec<Value> = sigs
            .iter()
            .map(|s| {
                serde_json::json!({ "kind": s.kind, "signature": s.signature, "count": s.count })
            })
            .collect();
        let body = serde_json::json!({
            "window": { "from": from_s, "to": to_s },
            "top_nodes_by_volume": volume,
            "severity_mix": severity_mix,
            "unmatched_signatures": unmatched,
        });
        ok_json_value("event_stats", body)
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
        let Some(severity) = parse_severity(&p.severity) else {
            return tool_bad_params("ack_alert", "`severity` must be info, warning, or critical");
        };
        let acked = p.acked.unwrap_or(true);
        // `apply_ack` persists *and* broadcasts. Both surfaces used to do the two steps
        // separately, and dropping the broadcast is a silent failure: the write succeeds, the
        // caller sees success, and every open dashboard keeps showing the alert unacknowledged
        // until someone reloads. `source` is what distinguishes this surface's acks in the audit
        // trail and in the pill the operator sees.
        let view = acked.then(|| AckView {
            at_unix_ms: Utc::now().timestamp_millis(),
            by: identity.actor.clone(),
            source: "mcp".to_owned(),
            note: p.note.clone(),
        });
        if let Err(e) =
            crate::api::alerts::apply_ack(&self.state, p.node_id, p.check_id, severity, view).await
        {
            return tool_api_error("ack_alert", &e);
        }
        let verb = if acked {
            "ack_alert"
        } else {
            "ack_alert(clear)"
        };
        record_audit(
            &self.state,
            &identity,
            &format!(
                "mcp.{verb} node={} check={} sev={}",
                p.node_id,
                p.check_id,
                severity.as_str()
            ),
            200,
        )
        .await;
        ok_json_value(
            "ack_alert",
            serde_json::json!({ "acked": acked, "node_id": p.node_id, "check_id": p.check_id }),
        )
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
        // Explicit bounds go through the same parse + ordering check as the REST edge; a window
        // that ends before it starts is stored happily and suppresses nothing, so the operator
        // believes they are covered and gets paged through the change anyway.
        let (starts, ends) = match (p.starts_at.as_deref(), p.ends_at.as_deref()) {
            (Some(s), Some(e)) => match crate::api::maintenance::window_bounds(s, e) {
                Ok(pair) => pair,
                Err(err) => return tool_api_error("open_maintenance", &err),
            },
            // This surface's own convenience: "mute it for an hour" without composing timestamps.
            // The duration is clamped, so the ordering check below can only pass — it runs anyway
            // because the invariant belongs to the window, not to how the bounds were obtained.
            (None, None) => {
                let mins = p.duration_mins.unwrap_or(60).clamp(1, 7 * 24 * 60);
                let now = Utc::now();
                let pair = (now, now + chrono::Duration::minutes(mins));
                if let Err(err) = crate::api::maintenance::check_order(pair.0, pair.1) {
                    return tool_api_error("open_maintenance", &err);
                }
                pair
            }
            _ => {
                return tool_bad_params(
                    "open_maintenance",
                    "provide both starts_at and ends_at, or neither (use duration_mins)",
                )
            }
        };
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
        // Same dispatch as `POST /api/v1/nodes/:id/poll`, including resolving the node's effective
        // pool (own > folder > default) — a manual poll published to the wrong pool's subject has
        // no poller listening for it, and that is not a mistake worth being able to make twice.
        let result = match crate::api::nodes::poll_now(admin, p.node_id).await {
            Ok(r) => r,
            Err(e) => return tool_api_error("poll_now", &e),
        };
        record_audit(
            &self.state,
            &identity,
            &format!("mcp.poll_now node={}", p.node_id),
            202,
        )
        .await;
        ok_json("poll_now", &result)
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

    /// Fill `src_as_name`/`dst_as_name` on conversation rows from the hot-swappable IP→ASN table
    /// (a no-op when the table is unloaded). Mirrors the REST flow handler so the MCP surface returns
    /// the same enriched rows.
    fn resolve_conversation_as_names(&self, rows: &mut [FlowConversation]) {
        let Some(db) = self.state.ipasn.read().ok().and_then(|g| g.clone()) else {
            return;
        };
        for r in rows.iter_mut() {
            r.src_as_name = db.name_of(r.src_asn).map(str::to_owned);
            r.dst_as_name = db.name_of(r.dst_asn).map(str::to_owned);
        }
    }

    /// Fill `name` on top-AS rows from the IP→ASN table (no-op when unloaded).
    fn resolve_as_names(&self, rows: &mut [FlowAsAgg]) {
        let Some(db) = self.state.ipasn.read().ok().and_then(|g| g.clone()) else {
            return;
        };
        for r in rows.iter_mut() {
            r.name = db.name_of(r.asn).map(str::to_owned);
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
    /// Optional IP-protocol filter (e.g. 6 = TCP, 17 = UDP).
    proto: Option<u8>,
    /// Optional destination-port filter.
    port: Option<u16>,
    /// Optional peer filter — an IP address that must be the source or destination.
    peer: Option<String>,
    /// Optional AS filter — an ASN that must be the source or destination AS.
    asn: Option<u32>,
    /// For kind=as, which side to aggregate: src or dst (default dst).
    dir: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct RunAnalysisParams {
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
struct EventStatsParams {
    /// Restrict the volume/severity views to one node (UUID).
    node_id: Option<Uuid>,
    /// Window start, Unix seconds (default: 24 hours ago).
    from: Option<i64>,
    /// Window end, Unix seconds (default: now).
    to: Option<i64>,
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

/// Render a shared [`AnalysisReport`](crate::api::analysis::AnalysisReport) through this surface's
/// sanitized DTOs.
///
/// The *assembly* is shared — which job, whether its findings exist yet — but the *serialization*
/// is not, and deliberately so: `dto.rs` is the ADR-018 enforcement boundary, and letting an
/// internal model serialize itself straight to an AI client is exactly the leak that file exists to
/// prevent. Sharing the query while keeping the projection is the whole point of the split.
fn analysis_report_body(report: &crate::api::analysis::AnalysisReport) -> Value {
    serde_json::json!({
        "job": AnalysisJobDto::from_job(&report.job),
        "findings": report.findings.iter().map(AnalysisFindingDto::from_finding).collect::<Vec<_>>(),
    })
}

/// Translate a failure from a shared API service function into this surface's vocabulary.
///
/// The tools call the same `pub(crate)` service functions as the REST handlers, so they inherit
/// [`ApiError`] — which is an HTTP shape. This is the single place that mapping happens; doing it
/// per tool is how the two surfaces drifted into answering differently for the same condition.
///
/// "Missing", "not configured" and "busy, try later" come back as **successful** results carrying
/// `available: false`, deliberately: a model that receives a hard JSON-RPC error tends to retry
/// blindly or give up, where an explanatory body lets it say "there is no node with that id" — or
/// wait out a full analysis queue — and move on. Genuine faults stay hard errors. Forwarding
/// `message()` is safe by construction — see [`ApiError::message`].
fn tool_api_error(tool: &str, err: &ApiError) -> Result<CallToolResult, McpError> {
    match err.status() {
        StatusCode::NOT_FOUND | StatusCode::SERVICE_UNAVAILABLE | StatusCode::TOO_MANY_REQUESTS => {
            tool_unavailable(tool, err.message())
        }
        StatusCode::BAD_REQUEST => tool_bad_params(tool, err.message()),
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => tool_forbidden(tool, err.message()),
        _ => {
            // `from_internal` has already logged the cause at the point of conversion; the message
            // here is the fixed operator-facing sentence, never the underlying error.
            record_tool(tool, "error");
            tracing::warn!(tool, code = err.code(), "MCP tool error: {}", err.message());
            Err(McpError::internal_error(err.message().to_owned(), None))
        }
    }
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

/// Build a [`FlowQuery`] from the shared flow-tool params: default trailing-hour window, typed
/// drill-down filters (an unparseable `peer` is ignored, never interpolated). Shared by `top_flows`
/// and `flow_fanout`.
fn flow_query_from(p: &TopFlowsParams) -> FlowQuery {
    let to_s = p.to.unwrap_or_else(|| Utc::now().timestamp());
    let from_s = p.from.unwrap_or(to_s - DEFAULT_WINDOW_SECS);
    let peer: Option<std::net::IpAddr> = p.peer.as_deref().and_then(|s| s.parse().ok());
    FlowQuery {
        node_id: Some(p.node_id),
        from_unix_ms: from_s.saturating_mul(1000),
        to_unix_ms: to_s.saturating_mul(1000),
        limit: p.limit.unwrap_or(100),
        proto: p.proto,
        dst_port: p.port,
        peer,
        asn: p.asn,
    }
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

// This surface no longer parses timestamps of its own: `parse_rfc3339_ok`/`parse_opt_rfc3339`
// lived here and were the MCP copies of the REST edge's parsing. Both callers (`open_maintenance`,
// `search_events`) now go through the shared validators in `api::maintenance` / `api::eventlog`,
// which is what makes a bound rejected on one surface rejected on both.

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::alerts::AlertManager;
    use crate::auth::{LoginThrottle, SessionStore};
    use crate::sink::InMemorySink;
    use crate::store::MetricStore;
    use std::sync::Arc;

    /// A skeleton-mode state: no `admin`, no flow/log tier. This is deliberately the *degraded*
    /// shape — it is what exercises every "tier not enabled / requires live mode" branch, which is
    /// the half of each tool a live-DB test would never reach.
    fn skeleton_state() -> ApiState {
        let store: Arc<dyn MetricStore> = Arc::new(InMemorySink::default());
        ApiState {
            store,
            logs: None,
            flows: None,
            ipasn: crate::ipasn::empty_handle(),
            host_sample: Arc::new(std::sync::Mutex::new(None)),
            nodes: Arc::new(crate::repo::StaticNodeList::demo()),
            alerts: Arc::new(AlertManager::new()),
            admin: None,
            sessions: Arc::new(SessionStore::new()),
            login_throttle: Arc::new(LoginThrottle::new()),
            history: None,
            ack: None,
            events: None,
            public_dashboard: false,
            is_leader: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            oidc: None,
            oidc_flight: Arc::new(crate::oidc::OidcFlight::new()),
            enable_mcp: true,
            rca: None,
        }
    }

    fn mcp() -> YagraMcp {
        YagraMcp::new(skeleton_state())
    }

    /// The text a tool result carries.
    fn text_of(r: &CallToolResult) -> String {
        r.content
            .iter()
            .filter_map(|b| b.as_text().map(|t| t.text.clone()))
            .collect::<Vec<_>>()
            .join("")
    }

    fn json_of(r: &CallToolResult) -> Value {
        serde_json::from_str(&text_of(r)).expect("tool result body is JSON")
    }

    // ── Pure helpers ────────────────────────────────────────────────────────────────────────────

    #[test]
    fn terminal_states_are_exactly_the_finished_ones() {
        for s in ["done", "failed", "cancelled"] {
            assert!(is_terminal(s), "{s} is terminal");
        }
        for s in ["running", "queued", "", "DONE"] {
            assert!(!is_terminal(s), "{s} is not terminal");
        }
    }

    #[test]
    fn severity_parses_case_insensitively_and_rejects_junk() {
        assert_eq!(parse_severity("info"), Some(Severity::Info));
        assert_eq!(parse_severity("  WARNING "), Some(Severity::Warning));
        assert_eq!(parse_severity("Critical"), Some(Severity::Critical));
        assert_eq!(parse_severity("fatal"), None);
        assert_eq!(parse_severity(""), None);
    }

    #[test]
    fn severity_rank_orders_and_defaults_unknown_lowest() {
        assert!(severity_rank("critical") > severity_rank("warning"));
        assert!(severity_rank("warning") > severity_rank("info"));
        assert_eq!(severity_rank("nonsense"), severity_rank("info"));
    }

    // The two RFC 3339 parsing tests that were here moved with the code they covered:
    // offset-applied-and-malformed-rejected is `api::util::parse_rfc3339`'s test, and
    // absent-vs-malformed is pinned by `api::eventlog`'s filter tests. Keeping copies here would
    // have tested this surface's *former* parser rather than the one it now calls.

    // ── Flow query construction (the injection-relevant one) ─────────────────────────────────────

    fn flow_params(node_id: Uuid) -> TopFlowsParams {
        TopFlowsParams {
            node_id,
            kind: None,
            from: None,
            to: None,
            limit: None,
            proto: None,
            port: None,
            peer: None,
            asn: None,
            dir: None,
        }
    }

    #[test]
    fn flow_query_defaults_to_a_trailing_hour_in_millis() {
        let id = Uuid::new_v4();
        let q = flow_query_from(&flow_params(id));
        assert_eq!(q.node_id, Some(id));
        assert_eq!(q.limit, 100, "default row cap");
        assert_eq!(
            q.to_unix_ms - q.from_unix_ms,
            DEFAULT_WINDOW_SECS * 1000,
            "default window is one hour, expressed in ms"
        );
    }

    #[test]
    fn flow_query_carries_explicit_window_and_typed_filters() {
        let mut p = flow_params(Uuid::new_v4());
        p.from = Some(1_700_000_000);
        p.to = Some(1_700_003_600);
        p.limit = Some(7);
        p.proto = Some(6);
        p.port = Some(443);
        p.asn = Some(15169);
        let q = flow_query_from(&p);
        assert_eq!(q.from_unix_ms, 1_700_000_000_000);
        assert_eq!(q.to_unix_ms, 1_700_003_600_000);
        assert_eq!(
            (q.limit, q.proto, q.dst_port, q.asn),
            (7, Some(6), Some(443), Some(15169))
        );
    }

    /// `peer` is the only free-text flow filter, and ClickHouse SQL interpolates it. It must reach
    /// the query as a typed `IpAddr` or not at all — a string that isn't an address is dropped, so
    /// nothing an MCP client sends can be interpolated verbatim.
    #[test]
    fn flow_query_drops_an_unparseable_peer_rather_than_passing_it_through() {
        let mut p = flow_params(Uuid::new_v4());
        p.peer = Some(r#"' OR 1=1 --"#.to_owned());
        assert_eq!(
            flow_query_from(&p).peer,
            None,
            "junk peer is dropped, never interpolated"
        );

        p.peer = Some("2001:db8::1".to_owned());
        assert_eq!(
            flow_query_from(&p).peer,
            Some("2001:db8::1".parse::<std::net::IpAddr>().unwrap()),
            "a valid v6 address survives as a typed value"
        );

        p.peer = Some("8.8.8.8".to_owned());
        assert_eq!(flow_query_from(&p).peer, Some("8.8.8.8".parse().unwrap()));
    }

    // ── Result-shape helpers ────────────────────────────────────────────────────────────────────

    /// "Tier off" is a *successful* result with a machine-readable body, not a protocol error — the
    /// model needs to tell "off here" apart from "broke".
    #[test]
    fn unavailable_is_a_success_result_with_an_availability_body() {
        let r = tool_unavailable("t", "flow tier not enabled on this core").expect("Ok result");
        let body = json_of(&r);
        assert_eq!(body["available"], serde_json::json!(false));
        assert_eq!(body["reason"], "flow tier not enabled on this core");
    }

    #[test]
    fn bad_params_and_forbidden_are_protocol_errors() {
        assert!(tool_bad_params("t", "`since` must be RFC 3339").is_err());
        assert!(tool_forbidden("t", "this token lacks ack-alerts permission").is_err());
    }

    /// Canary for security.md / coding-conventions: an internal error must never be echoed to the
    /// client. Only the caller-supplied context string may surface.
    #[test]
    fn internal_errors_never_leak_the_underlying_message() {
        let secret = "connection string postgres://user:hunter2@db/yagra";
        let err = tool_error("t", "load node", &anyhow::anyhow!(secret)).unwrap_err();
        let rendered = format!("{err:?} {}", err.message);
        assert!(
            !rendered.contains("hunter2"),
            "internal detail leaked: {rendered}"
        );
        assert!(
            !rendered.contains("postgres://"),
            "internal detail leaked: {rendered}"
        );
        assert!(
            rendered.contains("load node"),
            "the safe context is what surfaces"
        );
    }

    // ── Tool bodies over a skeleton state ───────────────────────────────────────────────────────

    #[tokio::test]
    async fn fleet_summary_counts_never_observed_nodes_as_unknown() {
        let r = mcp().get_fleet_summary().await.expect("ok");
        let body = json_of(&r);
        assert_eq!(body["total_nodes"], 1, "the demo inventory has one node");
        assert_eq!(
            body["states"]["unknown"], 1,
            "a node the alert engine has never seen reads as unknown, not as missing"
        );
        assert_eq!(body["active_alerts"], 0);
        assert_eq!(body["flow_tier_enabled"], serde_json::json!(false));
        assert_eq!(body["log_tier_enabled"], serde_json::json!(false));
    }

    #[tokio::test]
    async fn list_nodes_returns_sanitized_summaries_and_clamps_the_limit() {
        let r = mcp()
            .list_nodes(Parameters(ListNodesParams {
                search: None,
                limit: Some(10_000),
            }))
            .await
            .expect("ok");
        let body = json_of(&r);
        let arr = body.as_array().expect("an array of nodes");
        assert_eq!(
            arr.len(),
            1,
            "limit clamps to 100, and the demo list holds one node"
        );
        assert_eq!(arr[0]["name"], "demo-localhost");
        assert!(
            arr[0].get("credential").is_none(),
            "the DTO must not carry the credential reference (ADR-018)"
        );
    }

    #[tokio::test]
    async fn list_nodes_search_filters_by_name() {
        let m = mcp();
        let hit = m
            .list_nodes(Parameters(ListNodesParams {
                search: Some("demo".to_owned()),
                limit: None,
            }))
            .await
            .expect("ok");
        assert_eq!(json_of(&hit).as_array().unwrap().len(), 1);

        let miss = m
            .list_nodes(Parameters(ListNodesParams {
                search: Some("no-such-node".to_owned()),
                limit: None,
            }))
            .await
            .expect("ok");
        assert!(json_of(&miss).as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn active_alerts_is_empty_on_a_quiet_fleet() {
        let r = mcp()
            .get_active_alerts(Parameters(ActiveAlertsParams {
                node_id: None,
                min_severity: None,
                limit: None,
            }))
            .await
            .expect("ok");
        assert!(json_of(&r).as_array().unwrap().is_empty());
    }

    /// Every tool whose backing tier is absent answers "unavailable" rather than erroring or, worse,
    /// panicking on an `unwrap` of the missing handle.
    #[tokio::test]
    async fn tools_report_unavailable_when_their_tier_is_off() {
        let m = mcp();

        let flows = m
            .top_flows(Parameters(flow_params(Uuid::new_v4())))
            .await
            .expect("ok result");
        assert_eq!(json_of(&flows)["available"], serde_json::json!(false));

        let status = m
            .get_node_status(Parameters(NodeIdParams {
                node_id: Uuid::new_v4(),
            }))
            .await
            .expect("ok result");
        assert_eq!(json_of(&status)["available"], serde_json::json!(false));
    }
}
