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
//! device-configuration tools (monitoring lane, ADR-015/029); the heavier writes
//! (`run_probe`/`trigger_discovery`) remain future work. The plain-`async fn` shape (params in, DTO
//! out) is what lets the ADR-029 RCA agent reuse these bodies in-process later.
//!
//! ## Every tool takes a `RequestContext`, and that is a security property (WS-F)
//!
//! `/mcp` used to refuse a group-scoped principal outright, so a tool could read the whole fleet and
//! be correct. That refusal is gone: scoped callers are admitted and **each tool filters**, using
//! [`YagraMcp::scope_of`] to resolve the caller's scope and then the same rule its REST counterpart
//! carries in `api/route_table.rs` — a `group_id = ANY(…)` predicate where the query is ours, a
//! post-filter where the store ranks or aggregates, and the id-shaped `no node with that id` where
//! the tool names one node (never a distinct refusal, which would confirm the node exists).
//!
//! The consequence worth stating plainly: a tool that forgets to ask now returns the fleet, silently.
//! A tool body cannot reach the caller except through its `RequestContext`, so `ctx` is the visible
//! marker that the question was asked — and `every_tool_takes_a_request_context` fails if a new one
//! does not take it.

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
use yagra_common::{NodeId, NodeKind, Permission, SeriesKey, Severity};

use super::{McpIdentity, YagraMcp};
use crate::ack::AckView;
use crate::api::scope::NodeScope;
use crate::api::{ApiError, ApiState};
use crate::flowstore::{AsDir, FlowQuery};
use crate::mcp::dto::{
    AlertDto, AlertHistoryDto, AnalysisFindingDto, AnalysisJobDto, EventDto, FleetSummaryDto,
    InterfaceDto, MetricPointDto, MetricSeriesDto, NodeGroupDto, NodeStatusDto, NodeSummaryDto,
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
                       alerts, and which optional data tiers (metrics/flow/log) are enabled. Start here. \
                       Pass kind=\"coverage\" instead for the blind-spot view — which nodes have \
                       actually reported recently, and a watchlist of the ones that have not."
    )]
    async fn get_fleet_summary(
        &self,
        Parameters(p): Parameters<FleetSummaryParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let kind = match fleet_summary_kind(p.kind.as_deref()) {
            Some(k) => k,
            None => {
                return tool_bad_params(
                    "get_fleet_summary",
                    &format!(
                        "unknown kind {:?}; must be summary or coverage",
                        p.kind.unwrap_or_default()
                    ),
                )
            }
        };
        match self.scope_of(&ctx).await {
            Ok(scope) if kind == FleetSummaryKind::Coverage => self.fleet_coverage_in(&scope).await,
            Ok(scope) => self.fleet_summary_in(&scope).await,
            Err(e) => tool_api_error("get_fleet_summary", &e),
        }
    }

    /// The coverage branch: folded in rather than given its own tool because "is the fleet
    /// healthy?" and "is the fleet actually being *watched*?" are the same question asked twice —
    /// a model that starts here should not have to know a second tool name to find out that a
    /// third of the answer is stale.
    async fn fleet_coverage_in(&self, scope: &NodeScope) -> Result<CallToolResult, McpError> {
        let Some(admin) = self.state.admin.as_ref() else {
            return tool_unavailable("get_fleet_summary", "coverage requires live mode");
        };
        match crate::api::fleet::coverage(&self.state, admin, scope).await {
            Ok(c) => ok_json("get_fleet_summary", &c),
            Err(e) => tool_api_error("get_fleet_summary", &e),
        }
    }

    async fn fleet_summary_in(&self, scope: &NodeScope) -> Result<CallToolResult, McpError> {
        // Same tally as `GET /api/v1/fleet/summary`. This used to be a second implementation that
        // inserted only the states it had observed, so an AI client reading `states["warning"]`
        // got a missing key where the WebUI got a zero — the kind of difference that only shows up
        // as a model confidently reporting there is no warning data.
        let summary = crate::api::fleet::state_tally(&self.state, scope).await;
        let dto = FleetSummaryDto {
            total_nodes: summary.total,
            states: summary
                .states
                .into_iter()
                .map(|(k, v)| (k.to_owned(), v))
                .collect(),
            // Beyond the shared tally, this surface adds what an AI client needs to know before
            // trusting an answer: how much is currently wrong, and which tiers exist at all. The
            // count is filtered like the tally is — a scoped caller reading "42 active alerts" over
            // a 7-node summary would reasonably conclude the tally was wrong.
            active_alerts: self
                .state
                .alerts
                .active_alerts()
                .iter()
                // Node subjects only, matching `GET /api/v1/alerts` — the tally an operator sees
                // in the WebUI and the tally a model is told must be the same number.
                .filter(|a| {
                    a.node()
                        .is_some_and(|node| scope.allows_node(&self.state, node))
                })
                .count(),
            metrics_healthy: self.state.store.healthy().await,
            flow_tier_enabled: self.state.flows.is_some(),
            log_tier_enabled: self.state.logs.is_some(),
        };
        ok_json("get_fleet_summary", &dto)
    }

    #[tool(
        description = "List monitored nodes with their rolled-up state. Optional case-insensitive \
                       `search` matches name or address; narrow further with `state` \
                       (ok|warning|critical|unreachable|unknown|maintenance), `kind` \
                       (meraki|url|dns|device) and `pool` (the effective poll pool, inherited from \
                       the folder tree when the node sets none); `limit` is 1–100 (default 50). \
                       Returns node id, name, address, state, kind, parent, group, vendor, model, \
                       and tags. `kind` says what a node is — `device`, `url`, `dns` or `meraki` \
                       — and therefore which metrics it can have."
    )]
    async fn list_nodes(
        &self,
        Parameters(p): Parameters<ListNodesParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        match self.scope_of(&ctx).await {
            Ok(scope) => self.list_nodes_in(p, &scope).await,
            Err(e) => tool_api_error("list_nodes", &e),
        }
    }

    async fn list_nodes_in(
        &self,
        p: ListNodesParams,
        scope: &NodeScope,
    ) -> Result<CallToolResult, McpError> {
        let limit = p.limit.unwrap_or(50).clamp(1, 100);
        // Parsed through the REST edge's own function, not a copy of it. Rejected, never ignored:
        // an unrecognised token dropped here would widen the answer, and a model that asked for the
        // URL monitors would reason over every node believing it had the narrower set. Since
        // ADR-053 Inc.6 all three take comma-separated sets, which is the whole reason this is one
        // function — two hand-written parsers would have to agree about the empty set, the unknown
        // token and the untrimmed name, and nothing would notice when they stopped.
        let filter = match crate::api::nodes::parse_node_filter(
            p.state.as_deref(),
            p.kind.as_deref(),
            p.pool.as_deref(),
        ) {
            Ok(f) => f,
            Err(e) => return tool_api_error("list_nodes", &e),
        };
        // The scope goes into the query as the same indexed `group_id = ANY(…)` predicate the REST
        // list uses — `NodeListing` takes a `GroupFilter` on every method precisely so no call site
        // can default to the whole fleet without saying so.
        let groups = scope.group_filter();
        let nodes = if p.search.is_some() || filter.is_set() {
            // The shared seam, so a model and the WebUI cannot be told different things about
            // which nodes match — the filters run in-process here too, over the same bounded scan.
            match crate::api::nodes::filtered_node_page(
                &self.state,
                scope,
                p.search.as_deref().unwrap_or(""),
                &filter,
                limit,
            )
            .await
            {
                Ok((nodes, _truncated)) => Ok(nodes),
                Err(e) => return tool_api_error("list_nodes", &e),
            }
        } else {
            self.state.nodes.list_page(groups, None, limit).await
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
        // The kind comes from the same resolver the REST list uses (ADR-042 read parity), so a
        // model and the WebUI cannot be told different things about what a node is. Skeleton mode
        // has no side tables to read, so everything resolves to `device` — the same degradation
        // the REST path takes on a failed read.
        let uuids: Vec<Uuid> = nodes.iter().map(|n| n.id.as_uuid()).collect();
        let kinds = match self.state.admin.as_ref() {
            Some(admin) => crate::api::nodes::node_kinds(admin, &uuids).await,
            None => HashMap::new(),
        };
        let out: Vec<NodeSummaryDto> = nodes
            .iter()
            .map(|n| {
                NodeSummaryDto::from_node(
                    n,
                    states.get(&n.id).copied(),
                    kinds
                        .get(&n.id.as_uuid())
                        .copied()
                        .unwrap_or(NodeKind::Device),
                )
            })
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
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        match self.scope_of(&ctx).await {
            Ok(scope) => self.node_status_in(p, &scope).await,
            Err(e) => tool_api_error("get_node_status", &e),
        }
    }

    async fn node_status_in(
        &self,
        p: NodeIdParams,
        scope: &NodeScope,
    ) -> Result<CallToolResult, McpError> {
        // Out of scope answers exactly what a nonexistent id answers, deliberately: a distinct
        // "not allowed" would confirm the node exists, which is the enumeration oracle
        // `scope::require_visible_node` avoids on the REST side.
        if !scope.allows_node(&self.state, NodeId::from(p.node_id)) {
            return tool_unavailable("get_node_status", "no node with that id");
        }
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
        let kind = crate::api::nodes::node_kinds(admin, &[p.node_id])
            .await
            .get(&p.node_id)
            .copied()
            .unwrap_or(NodeKind::Device);
        let dto = NodeStatusDto {
            node: NodeSummaryDto::from_node(&node, Some(state), kind),
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
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        match self.scope_of(&ctx).await {
            Ok(scope) => self.active_alerts_in(p, &scope).await,
            Err(e) => tool_api_error("get_active_alerts", &e),
        }
    }

    async fn active_alerts_in(
        &self,
        p: ActiveAlertsParams,
        scope: &NodeScope,
    ) -> Result<CallToolResult, McpError> {
        let mut alerts = self.state.alerts.active_alerts();
        // Filtered before the severity cut and the truncation, so a scoped caller's `limit` is
        // spent on rows they can see rather than on rows that are about to be dropped.
        alerts.retain(|a| scope.allows_subject(&self.state, &a.subject));
        if let Some(node_id) = p.node_id {
            let nid = NodeId::from(node_id);
            alerts.retain(|a| a.subject.is_node(nid));
        }
        if let Some(min) = p.min_severity.as_deref() {
            let min_rank = severity_rank(min);
            alerts.retain(|a| severity_rank(a.severity.as_str()) >= min_rank);
        }
        alerts.sort_by_key(|a| std::cmp::Reverse(a.at_unix_ms));
        let limit = p.limit.unwrap_or(100).clamp(1, 500);
        alerts.truncate(limit);
        let names = self
            .resolve_names(scope, alerts.iter().filter_map(|a| Some(a.node()?.0)))
            .await;
        let out: Vec<AlertDto> = alerts
            .iter()
            .map(|a| {
                let name = a.node().and_then(|n| names.get(&n.0).cloned());
                AlertDto::from_alert(a, name)
            })
            .collect();
        ok_json("get_active_alerts", &out)
    }

    #[tool(
        description = "Recent alert history (fires and clears), newest first. `limit` is 1–1000 \
                       (default 100). Narrow with `severity` and `state` (each comma-separated for \
                       several values), `resolved` (false=fires, true=clears), `acked` (false = \
                       nobody has acknowledged it yet), `metric` (substring of the metric name), \
                       `node_id`, `node_q` (substring of the node's name), `group_id` (that folder \
                       and everything beneath it) and `since`/`until` — the window to search, which \
                       is separate from the cursor. To page, pass the oldest returned row's \
                       `cursor_at` as `before` and its `cursor_id` as `before_id` — both, and not \
                       its `at`, which is when the alert fired rather than when the row was \
                       written. Requires live mode."
    )]
    async fn get_alert_history(
        &self,
        Parameters(p): Parameters<AlertHistoryParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        match self.scope_of(&ctx).await {
            Ok(scope) => self.alert_history_in(p, &scope).await,
            Err(e) => tool_api_error("get_alert_history", &e),
        }
    }

    pub(crate) async fn alert_history_in(
        &self,
        p: AlertHistoryParams,
        scope: &NodeScope,
    ) -> Result<CallToolResult, McpError> {
        if self.state.history.is_none() {
            return tool_unavailable("get_alert_history", "alert history requires live mode");
        }
        // The whole page function is the shared seam — parsing, the scope checks on `node_id` /
        // `group_id`, the store call and the post-filter — so this surface cannot validate more
        // loosely than REST does. That is the drift `parse_event_filter` already paid for, on the
        // surface with no human in the loop. Since ADR-053 Inc.4b the set parsing is inside it too,
        // which is why there is no longer a token-parsing step here to get wrong.
        let input = crate::api::alerts::HistoryFilterInput {
            limit: p.limit,
            before: p.before.as_deref(),
            before_id: p.before_id,
            since: p.since.as_deref(),
            until: p.until.as_deref(),
            severity: p.severity.as_deref(),
            state: p.state.as_deref(),
            resolved: p.resolved,
            acked: p.acked,
            metric: p.metric.as_deref(),
            node_id: p.node_id,
            node_q: p.node_q.as_deref(),
            group_id: p.group_id,
        };
        let rows = match crate::api::alerts::history_page(&self.state, scope, input).await {
            Ok(r) => r,
            Err(e) => return tool_api_error("get_alert_history", &e),
        };
        let names = self
            .resolve_names(scope, rows.iter().filter_map(|r| r.node))
            .await;
        let out: Vec<AlertHistoryDto> = rows
            .iter()
            .map(|r| AlertHistoryDto::from_row(r, r.node.and_then(|n| names.get(&n).cloned())))
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
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        match self.scope_of(&ctx).await {
            Ok(scope) => self.query_metrics_in(p, &scope).await,
            Err(e) => tool_api_error("query_metrics", &e),
        }
    }

    pub(crate) async fn query_metrics_in(
        &self,
        p: QueryMetricsParams,
        scope: &NodeScope,
    ) -> Result<CallToolResult, McpError> {
        // Inside the split, not in the wrapper: a metric name is interpolated into a TSDB query, and
        // the in-process caller (ADR-028 WS-G) needs the same edge validation the session one gets.
        if !crate::api::is_valid_metric_name(&p.metric) {
            return tool_bad_params("query_metrics", "invalid metric name");
        }
        // A series is node data. The TSDB has never heard of groups, so this is the only place the
        // question can be asked — and it is the same "no node with that id" a miss gets.
        if !scope.allows_node(&self.state, NodeId::from(p.node_id)) {
            return tool_unavailable("query_metrics", "no node with that id");
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
        description = "Which metrics a node actually has, and how each must be read. Call this \
                       before query_metrics rather than guessing a name. Each entry gives \
                       `metric_kind` (gauge or counter — a counter's stored value is an odometer, \
                       so ask query_metrics for mode=rate), `dimension` (none = one series per \
                       node; interface = one per interface, use get_interface_series; entity = one \
                       per table row whose identity was lost at collection time, so only a \
                       node-wide aggregate is meaningful), and `status` (ok = configured and \
                       flowing; no_data = configured but nothing has arrived; unconfigured = data \
                       exists with no collection item, which is normal for reachability, URL/DNS \
                       monitors and neighbour counts). `within_secs` sets how far back a metric \
                       may have last been seen and still count as having data (default 6 hours)."
    )]
    async fn list_node_metrics(
        &self,
        Parameters(p): Parameters<NodeMetricsParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        const TOOL: &str = "list_node_metrics";
        if let Some(denied) = self.deny_unless_permitted(&ctx, TOOL, "") {
            return denied;
        }
        match self.scope_of(&ctx).await {
            Ok(scope) => self.list_node_metrics_in(p, &scope).await,
            Err(e) => tool_api_error(TOOL, &e),
        }
    }

    async fn list_node_metrics_in(
        &self,
        p: NodeMetricsParams,
        scope: &NodeScope,
    ) -> Result<CallToolResult, McpError> {
        const TOOL: &str = "list_node_metrics";
        if let Some(deny) = deny_invisible_node(&self.state, scope, TOOL, p.node_id) {
            return deny;
        }
        let Some(admin) = self.state.admin.as_ref() else {
            return tool_unavailable(TOOL, "the metric inventory requires live mode");
        };
        // Through the same seam the REST handler uses, so the two surfaces cannot come to differ
        // about what a node collects — the inventory is a join, and a second copy of the join is a
        // second set of rules for the three statuses.
        match crate::api::metrics::node_metric_inventory(
            admin,
            self.state.store.as_ref(),
            p.node_id,
            p.within_secs,
        )
        .await
        {
            Ok(rows) => ok_json(TOOL, &rows),
            Err(e) => tool_api_error(TOOL, &e),
        }
    }

    #[tool(
        description = "One interface's traffic history: in/out throughput in bits/sec and in/out \
                       error rates, all on one shared timestamp axis (nulls mark gaps). Give \
                       `node_id` and `ifindex` (from get_node_status's interfaces). `from`/`to` are \
                       Unix seconds (default: last hour) and `step` is the sample interval in \
                       seconds (clamped; defaults to ~120 points across the window). This is the \
                       per-interface counterpart to query_metrics, which is node-level only."
    )]
    async fn get_interface_series(
        &self,
        Parameters(p): Parameters<InterfaceSeriesParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        match self.scope_of(&ctx).await {
            Ok(scope) => self.interface_series_in(p, &scope).await,
            Err(e) => tool_api_error("get_interface_series", &e),
        }
    }

    async fn interface_series_in(
        &self,
        p: InterfaceSeriesParams,
        scope: &NodeScope,
    ) -> Result<CallToolResult, McpError> {
        if let Some(deny) =
            deny_invisible_node(&self.state, scope, "get_interface_series", p.node_id)
        {
            return deny;
        }
        let to = p.to.unwrap_or_else(|| Utc::now().timestamp());
        let from = p.from.unwrap_or(to - DEFAULT_WINDOW_SECS);
        if from >= to {
            return tool_bad_params("get_interface_series", "`from` must be earlier than `to`");
        }
        // The four metric names, the step/lookback rule and the ×8 bytes→bits scaling all live in
        // `api::metrics` — reproducing them here would mean this surface silently answering a
        // different question from the one the node-detail chart answers.
        let series = crate::api::metrics::interface_series(
            &self.state,
            NodeId::from(p.node_id),
            yagra_common::IfIndex(p.ifindex),
            from,
            to,
            p.step,
        )
        .await;
        ok_json("get_interface_series", &series)
    }

    #[tool(
        description = "Rank NODES by a metric across the fleet — which nodes are worst right now. \
                       `metric` is a metric name (icmp_rtt_ms, …) or a logical alias: cpu, memory. \
                       `agg` is now (default) or max_1h (trailing-hour peak). `limit` is 1–50 \
                       (default 5). `partial` in the result means a group scope may have shortened \
                       the list. For interfaces rather than nodes, use top_interfaces."
    )]
    async fn top_metrics(
        &self,
        Parameters(p): Parameters<TopMetricsParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        match self.scope_of(&ctx).await {
            Ok(scope) => self.top_metrics_in(p, &scope).await,
            Err(e) => tool_api_error("top_metrics", &e),
        }
    }

    async fn top_metrics_in(
        &self,
        p: TopMetricsParams,
        scope: &NodeScope,
    ) -> Result<CallToolResult, McpError> {
        // `ranked_nodes` validates the metric before it reaches the PromQL selector. That check is
        // the reason this is not inlined: it is the same injection boundary the REST edge has, now
        // reachable from a second surface.
        match crate::api::metrics::ranked_nodes(
            &self.state,
            scope,
            &p.metric,
            p.agg.as_deref(),
            p.limit,
        )
        .await
        {
            Ok(ranked) => ok_json("top_metrics", &ranked),
            Err(e) => tool_api_error("top_metrics", &e),
        }
    }

    #[tool(
        description = "Rank INTERFACES across the fleet, with node and interface names joined. \
                       `rank_by` is throughput | in_bps | out_bps | errors | discards (current \
                       rate), or delta_up | delta_down (biggest traffic spikes or drops vs a while \
                       ago). `agg` is now (default) or max_1h and applies to the rate kinds only; \
                       `window_secs` (60–3600, default 300) is the comparison window and applies to \
                       the delta kinds only. `limit` is 1–50 (default 6)."
    )]
    async fn top_interfaces(
        &self,
        Parameters(p): Parameters<TopInterfacesParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        match self.scope_of(&ctx).await {
            Ok(scope) => self.top_interfaces_in(p, &scope).await,
            Err(e) => tool_api_error("top_interfaces", &e),
        }
    }

    async fn top_interfaces_in(
        &self,
        p: TopInterfacesParams,
        scope: &NodeScope,
    ) -> Result<CallToolResult, McpError> {
        use crate::api::metrics::InterfaceRanking;
        // Two REST endpoints folded behind one vocabulary, because a row is the same thing in both
        // cases: an interface. Splitting on `kind` instead would have made `metric` mean two
        // disjoint things depending on another parameter, which is how a model sends
        // {kind: "interface", metric: "cpu"} and gets an error it cannot learn from.
        let rank = match p.rank_by.as_str() {
            "delta_up" | "delta_down" => {
                let direction = match crate::api::metrics::parse_delta_direction(
                    &p.rank_by["delta_".len()..],
                ) {
                    Ok(d) => d,
                    Err(e) => return tool_api_error("top_interfaces", &e),
                };
                InterfaceRanking::delta(direction, p.window_secs)
            }
            metric => {
                let m = match crate::api::metrics::parse_interface_metric(metric) {
                    Ok(m) => m,
                    Err(e) => return tool_api_error("top_interfaces", &e),
                };
                let agg = match crate::api::metrics::parse_top_agg(p.agg.as_deref()) {
                    Ok(a) => a,
                    Err(e) => return tool_api_error("top_interfaces", &e),
                };
                InterfaceRanking::Metric(m, agg)
            }
        };
        let ranked =
            crate::api::metrics::ranked_interfaces(&self.state, scope, rank, p.limit).await;
        ok_json("top_interfaces", &ranked)
    }

    #[tool(
        description = "Total fleet throughput over time (in/out bits per second), summed across \
                       every exporter. `from`/`to` are Unix seconds (default: last 24h), `step` is \
                       the sample interval in seconds. Refused for a token limited to a group: the \
                       total is summed inside the TSDB with no per-node breakdown left to narrow."
    )]
    async fn fleet_throughput(
        &self,
        Parameters(p): Parameters<FleetThroughputParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        match self.scope_of(&ctx).await {
            Ok(scope) => self.fleet_throughput_in(p, &scope).await,
            Err(e) => tool_api_error("fleet_throughput", &e),
        }
    }

    async fn fleet_throughput_in(
        &self,
        p: FleetThroughputParams,
        scope: &NodeScope,
    ) -> Result<CallToolResult, McpError> {
        // The refusal lives inside `fleet_throughput`, so this tool cannot serve the fleet's numbers
        // to a scoped caller by forgetting to ask.
        match crate::api::metrics::fleet_throughput(&self.state, scope, p.from, p.to, p.step).await
        {
            Ok(range) => ok_json("fleet_throughput", &range),
            Err(e) => tool_api_error("fleet_throughput", &e),
        }
    }

    #[tool(
        description = "A node's CDP/LLDP neighbours: which local port faces which peer right now, \
                       plus the most recent adjacency changes. A change row is written only when \
                       the adjacency actually moved, so a stable rack has none. `history_limit` is \
                       1–200 (default 10); page further back with `before_at` (RFC 3339) and \
                       `before_id` taken from `next` — both together or neither. Returns an \
                       availability note when no walk has recorded anything for the node, which is \
                       different from a device that reports no neighbours."
    )]
    async fn get_neighbors(
        &self,
        Parameters(p): Parameters<NeighborsParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        match self.scope_of(&ctx).await {
            Ok(scope) => self.neighbors_in(p, &scope).await,
            Err(e) => tool_api_error("get_neighbors", &e),
        }
    }

    async fn neighbors_in(
        &self,
        p: NeighborsParams,
        scope: &NodeScope,
    ) -> Result<CallToolResult, McpError> {
        // Scope first, availability second — the same order the REST guards run in. Reversed, a
        // scoped caller learns from the error which nodes exist.
        if let Some(deny) = deny_invisible_node(&self.state, scope, "get_neighbors", p.node_id) {
            return deny;
        }
        let Some(admin) = self.state.admin.as_ref() else {
            return tool_unavailable("get_neighbors", "neighbours require live mode");
        };
        let cursor = match crate::api::neighbors::parse_history_cursor(
            p.before_at.as_deref(),
            p.before_id,
        ) {
            Ok(c) => c,
            Err(e) => return tool_api_error("get_neighbors", &e),
        };
        // Current and history are one question, so this returns both rather than branching on a
        // mode param — a result whose shape depends on an argument is harder for a model than two
        // tools would be, and buys nothing.
        let current = match crate::api::neighbors::current_neighbors(admin, p.node_id).await {
            Ok(c) => c,
            // 404 here means "never walked", which `tool_api_error` renders as an availability note
            // rather than an error. Inventing an empty set instead would assert the device has no
            // neighbours, which is a different and false claim.
            Err(e) => return tool_api_error("get_neighbors", &e),
        };
        let history = match crate::api::neighbors::neighbor_history(
            admin,
            p.node_id,
            cursor,
            p.history_limit,
        )
        .await
        {
            Ok(h) => h,
            Err(e) => return tool_api_error("get_neighbors", &e),
        };
        ok_json_value(
            "get_neighbors",
            serde_json::json!({ "current": current, "history": history }),
        )
    }

    #[tool(
        description = "IP addresses seen on the network that Yagra does **not** monitor, derived \
                       from the ARP\\IPv6-neighbour caches of the routers it does. Use this to \
                       answer \"what is on this segment that we are not watching\". Each row names \
                       the address, its MAC where known, and which monitored node saw it on which \
                       ifIndex — so `via_node` plus `via_ifindex` is the port the host is behind. \
                       `limit` is 1–500 (default 100); page with `before_last_seen` (RFC 3339) and \
                       `before_id` from `next`, both together or neither. Filter to one router with \
                       `via_node`. Empty when ARP discovery is switched off, which is the default. \
                       ⚠️ Check `summary.truncated_nodes`: above zero, at least one router's cache \
                       exceeded its row budget and this list is a sample, not the whole segment. \
                       These endpoints are deliberately not topology vertices — they have no \
                       monitored state — so get_topology will not show them."
    )]
    async fn list_discovered_endpoints(
        &self,
        Parameters(p): Parameters<DiscoveredEndpointsParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        match self.scope_of(&ctx).await {
            Ok(scope) => self.discovered_endpoints_in(p, &scope).await,
            Err(e) => tool_api_error("list_discovered_endpoints", &e),
        }
    }

    async fn discovered_endpoints_in(
        &self,
        p: DiscoveredEndpointsParams,
        scope: &NodeScope,
    ) -> Result<CallToolResult, McpError> {
        let Some(admin) = self.state.admin.as_ref() else {
            return tool_unavailable(
                "list_discovered_endpoints",
                "endpoint discovery requires live mode",
            );
        };
        let cursor = match crate::api::discovery::endpoint_cursor(
            p.before_last_seen.as_deref(),
            p.before_id,
        ) {
            Ok(c) => c,
            Err(e) => return tool_api_error("list_discovered_endpoints", &e),
        };
        // The scope reaches the SQL rather than being applied here: the rows carry an address and no
        // node id, so the only thing that bounds them is the *observing* node's group, and that join
        // belongs in the one statement both surfaces share.
        match crate::api::discovery::discovered_endpoint_page(
            admin,
            scope,
            p.via_node,
            p.include_promoted.unwrap_or(false),
            cursor,
            p.limit,
        )
        .await
        {
            Ok(page) => ok_json("list_discovered_endpoints", &page),
            Err(e) => tool_api_error("list_discovered_endpoints", &e),
        }
    }

    #[tool(
        description = "The folder groups nodes are filed under: id, name, kind, parent (for \
                       rebuilding the tree), and map coordinates. Use this to find the group id \
                       run_analysis takes for scope=\"group\", or to turn a node's `group` field \
                       into a place. Set `include_state` to also get each folder's direct-member \
                       health tally (ok/warning/critical/unknown/unreachable/maintenance) — that \
                       is the per-site rollup, where get_fleet_summary tallies the whole fleet. \
                       Requires live mode."
    )]
    async fn list_node_groups(
        &self,
        Parameters(p): Parameters<ListNodeGroupsParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        match self.scope_of(&ctx).await {
            Ok(scope) => self.list_node_groups_in(p, &scope).await,
            Err(e) => tool_api_error("list_node_groups", &e),
        }
    }

    async fn list_node_groups_in(
        &self,
        p: ListNodeGroupsParams,
        scope: &NodeScope,
    ) -> Result<CallToolResult, McpError> {
        let Some(admin) = self.state.admin.as_ref() else {
            return tool_unavailable("list_node_groups", "node groups require live mode");
        };
        // `visible_groups` keeps the caller's subtree *and* the ancestors above it. This DTO carries
        // `parent_id`, so a filter that dropped the ancestors would leave every visible root
        // pointing at a group that is not in the list.
        let groups = match crate::api::groups::visible_groups(admin, scope).await {
            Ok(g) => g,
            Err(e) => return tool_api_error("list_node_groups", &e),
        };
        if !p.include_state.unwrap_or(false) {
            let out: Vec<crate::mcp::dto::NodeGroupDto> =
                groups.iter().map(NodeGroupDto::from_summary).collect();
            return ok_json("list_node_groups", &out);
        }
        // The rollup the site-matrix widget reads, joined onto the tree rather than served as a
        // second bare `group_id → counts` map: a model given the map alone has no names to attach
        // the numbers to, and would have to call this tool again to get them.
        let rollup = match crate::api::fleet::group_summary(&self.state, scope).await {
            Ok(s) => s,
            Err(e) => return tool_api_error("list_node_groups", &e),
        };
        let out: Vec<crate::mcp::dto::NodeGroupDto> = groups
            .iter()
            .map(|g| NodeGroupDto::from_summary(g).with_state(rollup.groups.get(&g.id)))
            .collect();
        ok_json("list_node_groups", &out)
    }

    #[tool(
        description = "What is currently suppressing alerts, in one answer: planned maintenance \
                       windows (each with its scope, start/end, and whether it covers now), \
                       reactive mutes (node or folder, with an expiry), and exemptions — nodes an \
                       operator has released from a window or mute they only inherited, which are \
                       alerting normally despite it. Check this before concluding a fleet is \
                       healthy — a quiet fleet and a silenced one look the same in \
                       get_active_alerts. Windows opened with open_maintenance appear here. \
                       Requires live mode."
    )]
    async fn list_suppressions(
        &self,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        match self.scope_of(&ctx).await {
            Ok(scope) => self.list_suppressions_in(&scope).await,
            Err(e) => tool_api_error("list_suppressions", &e),
        }
    }

    async fn list_suppressions_in(&self, scope: &NodeScope) -> Result<CallToolResult, McpError> {
        let Some(admin) = self.state.admin.as_ref() else {
            return tool_unavailable("list_suppressions", "suppression state requires live mode");
        };
        // Both lists filter on the row's own target, and a window scoped to a profile or a tag is
        // hidden from a scoped caller entirely — the shared seams carry that, so this surface
        // cannot show a window the WebUI would not.
        let windows =
            match crate::api::maintenance::visible_windows(&self.state, scope, admin).await {
                Ok(w) => w,
                Err(e) => return tool_api_error("list_suppressions", &e),
            };
        let mutes = match crate::api::maintenance::visible_mutes(&self.state, scope, admin).await {
            Ok(m) => m,
            Err(e) => return tool_api_error("list_suppressions", &e),
        };
        // The negative half. Without it a released node reads as suppressed, which is the wrong
        // way round for a tool whose whole point is telling a quiet fleet from a silenced one.
        let exemptions =
            match crate::api::maintenance::visible_exemptions(&self.state, scope, admin).await {
                Ok(x) => x,
                Err(e) => return tool_api_error("list_suppressions", &e),
            };
        ok_json(
            "list_suppressions",
            &crate::mcp::dto::SuppressionsDto {
                maintenance_windows: windows,
                mutes,
                exemptions,
            },
        )
    }

    #[tool(
        description = "How the fleet has been alerting over time — the three views the alert \
                       dashboards draw. `kind` is top_nodes (which nodes alert most often over \
                       `window_secs`, default 24h — chronic offenders, which get_active_alerts \
                       cannot show because it reports only what is firing now), transitions (the \
                       latest fires and recoveries, newest first), or calendar (fire counts \
                       bucketed by weekday and hour over `days`, default 7, for spotting a \
                       nightly pattern). `limit` applies to top_nodes (1–50, default 6) and \
                       transitions (default 12)."
    )]
    async fn alert_trends(
        &self,
        Parameters(p): Parameters<AlertTrendsParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        match self.scope_of(&ctx).await {
            Ok(scope) => self.alert_trends_in(p, &scope).await,
            Err(e) => tool_api_error("alert_trends", &e),
        }
    }

    async fn alert_trends_in(
        &self,
        p: AlertTrendsParams,
        scope: &NodeScope,
    ) -> Result<CallToolResult, McpError> {
        // Three endpoints behind one `kind`, following `top_flows`: the row type varies, but the
        // parameters mean the same thing in every branch (a window and a count), so there is no
        // argument whose meaning another argument changes — which is what made folding the metric
        // rankings the wrong call in I1.
        match p.kind.as_str() {
            "top_nodes" => {
                match crate::api::alerts::top_alerting_nodes(
                    &self.state,
                    scope,
                    p.window_secs,
                    p.limit,
                )
                .await
                {
                    Ok(ranked) => ok_json("alert_trends", &ranked),
                    Err(e) => tool_api_error("alert_trends", &e),
                }
            }
            "transitions" => {
                match crate::api::alerts::recent_transitions(&self.state, scope, p.limit).await {
                    Ok(rows) => ok_json("alert_trends", &rows),
                    Err(e) => tool_api_error("alert_trends", &e),
                }
            }
            "calendar" => {
                match crate::api::alerts::alert_calendar_buckets(&self.state, scope, p.days).await {
                    Ok(rows) => ok_json("alert_trends", &rows),
                    Err(e) => tool_api_error("alert_trends", &e),
                }
            }
            other => tool_bad_params(
                "alert_trends",
                &format!("unknown kind {other:?}; must be top_nodes, transitions or calendar"),
            ),
        }
    }

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
        match self.scope_of(&ctx).await {
            Ok(scope) => self.search_analysis_findings_in(p, &scope).await,
            Err(e) => tool_api_error("search_analysis_findings", &e),
        }
    }

    async fn search_analysis_findings_in(
        &self,
        p: SearchFindingsParams,
        scope: &NodeScope,
    ) -> Result<CallToolResult, McpError> {
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
            Ok(rows) => ok_json("search_analysis_findings", &rows),
            Err(e) => tool_api_error("search_analysis_findings", &e),
        }
    }

    #[tool(
        description = "How the network is connected, in keyset pages. `kind=dependency` (default) \
                       is the alert-suppression graph: each node with its upstream parent, current \
                       state, and the upstream node blamed for its alert (root_cause). \
                       `kind=links` is the physical/logical connectivity graph derived from \
                       CDP/LLDP adjacency and shared IP subnets: undirected links between nodes, \
                       each with the evidence that produced it (`sources`) and the subnet behind a \
                       shared-subnet link. `kind=overrides` lists the decisions an operator has \
                       recorded about links (pin, hide, or which end is upstream), which always \
                       beat what was derived. `kind=shadow` compares the two dependency graphs and \
                       is what answers whether derived suppression is safe to enable here: it \
                       reports which active alerts the derived graph would newly suppress \
                       (`would_suppress` — the risky direction) or stop suppressing, plus any \
                       pools whose poller has no place in the graph yet (`unresolved_pools`, which \
                       block enabling it). Dependency and links return `next_cursor` for the \
                       following page; `after` is that cursor (a node UUID for dependency, a \
                       number for links) and `limit` is 1–1000 (default 200). Requires live mode."
    )]
    async fn get_topology(
        &self,
        Parameters(p): Parameters<TopologyParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        match self.scope_of(&ctx).await {
            Ok(scope) => self.topology_in(p, &scope).await,
            Err(e) => tool_api_error("get_topology", &e),
        }
    }

    pub(crate) async fn topology_in(
        &self,
        p: TopologyParams,
        scope: &NodeScope,
    ) -> Result<CallToolResult, McpError> {
        let Some(admin) = self.state.admin.as_ref() else {
            return tool_unavailable("get_topology", "topology requires live mode");
        };
        // Same assembly as the REST handlers; only the page bound differs, because an AI client
        // wants a handful of edges where the graph view wants the fleet.
        let limit = p.limit.unwrap_or(200).clamp(1, 1000);
        match topology_kind(p.kind.as_deref()) {
            TopologyKind::Dependency => {
                let after = match p.after.as_deref().map(str::parse::<Uuid>) {
                    Some(Ok(id)) => Some(id),
                    Some(Err(_)) => {
                        return tool_bad_params(
                            "get_topology",
                            "`after` must be a node UUID when kind is dependency",
                        )
                    }
                    None => None,
                };
                match crate::api::topology::topology_page(&self.state, admin, scope, after, limit)
                    .await
                {
                    Ok(page) => ok_json("get_topology", &page),
                    Err(e) => tool_api_error("get_topology", &e),
                }
            }
            TopologyKind::Links => {
                let after = match p.after.as_deref().map(str::parse::<i64>) {
                    Some(Ok(id)) => Some(id),
                    Some(Err(_)) => {
                        return tool_bad_params(
                            "get_topology",
                            "`after` must be a number when kind is links",
                        )
                    }
                    None => None,
                };
                match crate::api::topology::topology_link_page(admin, scope, after, limit).await {
                    Ok(page) => ok_json("get_topology", &page),
                    Err(e) => tool_api_error("get_topology", &e),
                }
            }
            TopologyKind::Overrides => {
                match crate::api::topology::link_override_list(&self.state, admin, scope).await {
                    Ok(list) => ok_json("get_topology", &list),
                    Err(e) => tool_api_error("get_topology", &e),
                }
            }
            TopologyKind::Shadow => {
                match crate::api::topology::topology_shadow(&self.state, admin, scope).await {
                    Ok(s) => ok_json("get_topology", &s),
                    Err(e) => tool_api_error("get_topology", &e),
                }
            }
            TopologyKind::Unknown => tool_bad_params(
                "get_topology",
                "`kind` must be one of: dependency, links, overrides, shadow",
            ),
        }
    }

    #[tool(
        description = "Top traffic flows from the flow tier. Omit `node_id` for the whole fleet, or \
                       give one to look at a single exporter — which is where flow analysis is \
                       usually done. `kind` is talkers|conversations|ports|protocols|series|as \
                       (default talkers). `from`/`to` are Unix seconds (default: last hour); `limit` \
                       is 1–1000 (default 100). Optional drill-down filters: `proto`, `port`, `peer` \
                       (an IP), `asn`; for kind=as, `dir` is src|dst (default dst). Conversations/as \
                       rows carry resolved AS names when the IP→ASN table is loaded. The fleet-wide \
                       form is refused for a token limited to a group: the rows are grouped by \
                       address, port, protocol or AS, so no exporter attribution survives to narrow."
    )]
    async fn top_flows(
        &self,
        Parameters(p): Parameters<TopFlowsParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        match self.scope_of(&ctx).await {
            Ok(scope) => self.top_flows_in(p, &scope).await,
            Err(e) => tool_api_error("top_flows", &e),
        }
    }

    async fn top_flows_in(
        &self,
        p: TopFlowsParams,
        scope: &NodeScope,
    ) -> Result<CallToolResult, McpError> {
        match p.node_id {
            Some(node_id) => {
                if let Some(deny) = deny_invisible_node(&self.state, scope, "top_flows", node_id) {
                    return deny;
                }
            }
            // Fleet-wide: the same refusal `GET /api/v1/flow/*` gives, through the same helper.
            //
            // **Before the tier check, and that ordering is a disclosure property.** REST refuses
            // the scope first and only then consults the store, so a scoped caller learns it is
            // refused without learning whether this deployment runs a flow tier at all. Reversed,
            // the availability note becomes an oracle for the deployment's configuration.
            None => {
                if let Err(e) = crate::api::flow::fleet_flow_is_unattributed(scope) {
                    return tool_api_error("top_flows", &e);
                }
            }
        }
        // The six-arm dispatch, the AS-name fill and the store's availability gate all live in
        // `api::flow` now. This tool used to re-implement all three, and that copy had lost the
        // limit clamp.
        let agg = match crate::api::flow::FlowAgg::parse(p.kind.as_deref().unwrap_or("talkers")) {
            Ok(a) => a,
            Err(e) => return tool_api_error("top_flows", &e),
        };
        let dir = if p.dir.as_deref() == Some("src") {
            AsDir::Src
        } else {
            AsDir::Dst
        };
        match crate::api::flow::flow_agg_rows(&self.state, &flow_query_from(&p), dir, agg).await {
            Ok(rows) => ok_json("top_flows", &rows),
            Err(e) => tool_api_error("top_flows", &e),
        }
    }

    #[tool(
        description = "Per-source flow fan-out for ONE node: how many distinct destinations and \
                       destination ports each source contacted, highest fan-out first (scan / worm \
                       triage). `node_id` is required — unlike top_flows this has no fleet-wide \
                       form, because the count is grouped by source and a flow seen by two exporters \
                       would be counted twice. Same window/filters as top_flows \
                       (`from`/`to`/`limit`/`proto`/`port`/`peer`/`asn`). Returns an availability \
                       note when the flow tier is off."
    )]
    async fn flow_fanout(
        &self,
        Parameters(p): Parameters<TopFlowsParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        match self.scope_of(&ctx).await {
            Ok(scope) => self.flow_fanout_in(p, &scope).await,
            Err(e) => tool_api_error("flow_fanout", &e),
        }
    }

    async fn flow_fanout_in(
        &self,
        p: TopFlowsParams,
        scope: &NodeScope,
    ) -> Result<CallToolResult, McpError> {
        // Node-scoped only, deliberately. `FlowQuery.node_id` makes a fleet-wide form structurally
        // possible, but `fanout_by_src` groups by source, so across exporters a flow seen twice is
        // counted twice and "distinct destinations contacted" inflates by an amount nobody can
        // bound. There is no REST counterpart to compare a fleet answer against either, so it would
        // be an MCP-only capability with a known correctness caveat.
        let Some(node_id) = p.node_id else {
            return tool_bad_params(
                "flow_fanout",
                "`node_id` is required: fan-out is counted per source, so a fleet-wide form would \
                 double-count any flow two exporters both saw. Query one exporter at a time.",
            );
        };
        if let Some(deny) = deny_invisible_node(&self.state, scope, "flow_fanout", node_id) {
            return deny;
        }
        let Some(flows) = self.state.flows.as_ref() else {
            return tool_unavailable("flow_fanout", "flow tier not enabled on this core");
        };
        match flows.fanout_by_src(&flow_query_from(&p)).await {
            Ok(rows) => ok_json("flow_fanout", &rows),
            Err(e) => tool_error("flow_fanout", "query flow fan-out", &e),
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
        // Operator and up, matching `POST /api/v1/analysis/jobs`. An analysis reads what a Viewer
        // could already query, so this gate is not about disclosure — it is that launching one is
        // incident-response work with a real compute cost, and the two surfaces must agree on who
        // may do it. They did not: this was `View` while REST was admin-only.
        let Some(identity) = authed_for(&ctx, Permission::AckAlerts) else {
            return tool_forbidden("run_analysis", "this token lacks ack-alerts permission");
        };
        let visible = match self.scope_of(&ctx).await {
            Ok(s) => s,
            Err(e) => return tool_api_error("run_analysis", &e),
        };
        let Some(admin) = self.state.admin.as_ref() else {
            return tool_unavailable("run_analysis", "analysis requires live mode");
        };
        let scope = p.scope.as_deref().unwrap_or("all");
        // The same launch-target rule as `POST /api/v1/analysis/jobs`: a run's findings are read
        // back later, so an over-broad launch would hand a scoped caller fleet-wide data through
        // the results. Checked before the label lookup — an invalid target should not first cost a
        // name query, and the label must not name a node the caller cannot see.
        if let Err(e) =
            crate::api::analysis::require_launchable_scope(&self.state, &visible, scope, p.scope_id)
        {
            return tool_api_error("run_analysis", &e);
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
            Err(e) => return tool_api_error("run_analysis", &e),
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
            Ok(r) => ok_json_value("run_analysis", self.scoped_report_body(&visible, r)),
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
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        match self.scope_of(&ctx).await {
            Ok(scope) => self.analysis_findings_in(p, &scope).await,
            Err(e) => tool_api_error("get_analysis_findings", &e),
        }
    }

    pub(crate) async fn analysis_findings_in(
        &self,
        p: AnalysisJobIdParams,
        scope: &NodeScope,
    ) -> Result<CallToolResult, McpError> {
        let Some(admin) = self.state.admin.as_ref() else {
            return tool_unavailable("get_analysis_findings", "analysis requires live mode");
        };
        let report = match crate::api::analysis::report(admin, p.job_id).await {
            Ok(r) => r,
            Err(e) => return tool_api_error("get_analysis_findings", &e),
        };
        // The run row is the unit of visibility — somebody else's fleet-wide run is not readable
        // just because its id was guessed or came from a shared transcript.
        if let Err(e) = crate::api::analysis::require_visible_job(&self.state, scope, &report.job) {
            return tool_api_error("get_analysis_findings", &e);
        }
        ok_json_value(
            "get_analysis_findings",
            self.scoped_report_body(scope, report),
        )
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
        match self.scope_of(&ctx).await {
            Ok(scope) => self.list_analyses_in(p, &scope).await,
            Err(e) => tool_api_error("list_analyses", &e),
        }
    }

    async fn list_analyses_in(
        &self,
        p: ListAnalysesParams,
        scope: &NodeScope,
    ) -> Result<CallToolResult, McpError> {
        // Folded onto the runs list rather than given its own tool: both answer "what analysis is
        // there", both are post-filtered on the row's own target, and a schedule is what a run will
        // be. The `limit` only applies to runs, which is why it is documented that way.
        match p.kind.as_deref().unwrap_or("runs") {
            "runs" => {}
            "schedules" => {
                return match crate::api::analysis::visible_schedules(&self.state, scope).await {
                    Ok(rows) => ok_json("list_analyses", &rows),
                    Err(e) => tool_api_error("list_analyses", &e),
                };
            }
            other => {
                return tool_bad_params(
                    "list_analyses",
                    &format!("unknown kind {other:?}; must be runs or schedules"),
                );
            }
        }
        let Some(admin) = self.state.admin.as_ref() else {
            return tool_unavailable("list_analyses", "analysis requires live mode");
        };
        // A smaller page than the REST default (50): an AI client reads the runs list to orient,
        // not to render a table.
        let limit = p.limit.unwrap_or(20).clamp(1, 100);
        // No filter: `list_analyses` answers "what has run recently", and a model narrows by
        // reading the rows rather than by re-asking. The seam is shared so the cap cannot differ.
        let jobs = match admin.analysis.list(limit, &Default::default()).await {
            Ok(js) => js,
            Err(e) => return tool_error("list_analyses", "list analyses", &e),
        };
        // Post-filtered on each run's own target, matching `GET /api/v1/analysis/jobs`. A short
        // page is correct: this is "recent activity", not a cursor-paged collection.
        let out: Vec<AnalysisJobDto> = jobs
            .iter()
            .filter(|j| scope.allows_target(&self.state, crate::api::analysis::row_target(j)))
            .map(AnalysisJobDto::from_job)
            .collect();
        ok_json("list_analyses", &out)
    }

    #[tool(
        description = "Search received passive events (syslog / SNMP traps / webhooks), newest first. \
                       Optional `search` (case-insensitive over source/message; it matches whole \
                       words on a log-store deployment and any substring otherwise, so set `regex` \
                       to true for a message-only regex that reaches inside words either way), \
                       `kind` (syslog|trap|webhook), `action` (none|info|suppressed|cleared|\
                       refreshed|fired) and `severity` (0–7), each taking a comma-separated set, \
                       `node_id`, `matched` (only rule-matched events), `since`/`until` (RFC 3339), and \
                       `limit` (1–500, default 100). Per-column conditions narrow one field at a \
                       time and can be negated: `msg` + `msg_regex` + `msg_not` on the message, \
                       `src` + `src_not` on the source IP or node name. Requires live mode."
    )]
    async fn search_events(
        &self,
        Parameters(p): Parameters<EventSearchParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        match self.scope_of(&ctx).await {
            Ok(scope) => self.search_events_in(p, &scope).await,
            Err(e) => tool_api_error("search_events", &e),
        }
    }

    pub(crate) async fn search_events_in(
        &self,
        p: EventSearchParams,
        scope: &NodeScope,
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
                kind: p.kind.as_deref(),
                action: p.action.as_deref(),
                severity: p.severity.as_deref(),
                node_id: p.node_id,
                matched: p.matched,
                q: p.search.as_deref(),
                regex: p.regex.unwrap_or(false),
                msg: p.msg.as_deref(),
                msg_regex: p.msg_regex.unwrap_or(false),
                msg_not: p.msg_not.unwrap_or(false),
                src: p.src.as_deref(),
                src_not: p.src_not.unwrap_or(false),
            }) {
                Ok(f) => f,
                Err(e) => return tool_api_error("search_events", &e),
            };
        let limit = p.limit.unwrap_or(100).clamp(1, 500);
        // Same store routing too, including resolving a node-name term to ids so the name never
        // enters the log store (ADR-011).
        let rows =
            match crate::api::eventlog::search(&self.state, admin, scope, &filter, limit).await {
                Ok(r) => r,
                Err(e) => return tool_api_error("search_events", &e),
            };
        let names = self
            .resolve_names(scope, rows.iter().filter_map(|r| r.node_id))
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
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        match self.scope_of(&ctx).await {
            Ok(scope) => self.event_stats_in(p, &scope).await,
            Err(e) => tool_api_error("event_stats", &e),
        }
    }

    pub(crate) async fn event_stats_in(
        &self,
        p: EventStatsParams,
        scope: &NodeScope,
    ) -> Result<CallToolResult, McpError> {
        let Some(admin) = self.state.admin.as_ref() else {
            return tool_unavailable("event_stats", "event stats requires live mode");
        };
        let to_s = p.to.unwrap_or_else(|| Utc::now().timestamp());
        let from_s = p.from.unwrap_or(to_s - 86_400);
        // The window, as the shared event filter. Scope stays a post-filter below rather than a
        // push-down (see the comment there), so `visible_node_ids` is deliberately left unset —
        // changing MCP's scope semantics is a separate decision from fixing which store answers.
        let window = crate::events::EventFilter {
            since: DateTime::from_timestamp(from_s, 0),
            until: DateTime::from_timestamp(to_s, 0),
            ..Default::default()
        };
        // Volume: sum per-node hourly buckets over the window.
        //
        // Through the log store when one is configured (ADR-024): PostgreSQL keeps only alert-linked
        // rows there, so answering from it reported the events that had already alerted as if they
        // were the whole passive-event volume.
        let buckets = match crate::logstore::route_counts_by_bucket(
            self.state.logs.as_ref(),
            &admin.events,
            &window,
            3600,
        )
        .await
        {
            Ok(b) => b,
            Err(e) => return tool_error("event_stats", "event volume", &e),
        };
        // Both of the per-node views carry a `node_id`, so the scope filter runs here rather than
        // in the store — unlike `GET /api/v1/events/stats`, whose grouped counts arrive already
        // summed and therefore need the restriction pushed into the query.
        let mut per_node: HashMap<Uuid, i64> = HashMap::new();
        for b in &buckets {
            if p.node_id.is_none_or(|n| n == b.node_id)
                && scope.allows_node(&self.state, NodeId::from(b.node_id))
            {
                *per_node.entry(b.node_id).or_default() += b.count;
            }
        }
        let mut top_nodes: Vec<(Uuid, i64)> = per_node.into_iter().collect();
        top_nodes.sort_by_key(|n| std::cmp::Reverse(n.1));
        top_nodes.truncate(20);
        let names = self
            .resolve_names(scope, top_nodes.iter().map(|(id, _)| *id))
            .await;
        let volume: Vec<Value> = top_nodes
            .iter()
            .map(|(id, c)| {
                serde_json::json!({ "node_id": id, "node_name": names.get(id).cloned(), "count": c })
            })
            .collect();
        // Severity mix.
        let sev = crate::logstore::route_severity_counts(
            self.state.logs.as_ref(),
            &admin.events,
            &window,
        )
        .await
        .unwrap_or_default();
        let mut sev_mix: BTreeMap<i16, i64> = BTreeMap::new();
        for s in &sev {
            if p.node_id.is_none_or(|n| n == s.node_id)
                && scope.allows_node(&self.state, NodeId::from(s.node_id))
            {
                *sev_mix.entry(s.severity).or_default() += s.count;
            }
        }
        let severity_mix: Vec<Value> = sev_mix
            .iter()
            .map(|(sev, c)| serde_json::json!({ "severity": sev, "count": c }))
            .collect();
        // Top unmatched signatures (rule gaps) are aggregated **across** nodes, so a row retains no
        // node to filter by — the same shape REST refuses a scoped caller for. Omitted with a note
        // rather than served: a rule-gap ranking over the whole fleet, handed to an account that
        // sees seven nodes, reads as a fact about those seven.
        let (unmatched, note) = if scope.is_all() {
            let sigs = crate::logstore::route_unmatched_signatures(
                self.state.logs.as_ref(),
                &admin.events,
                &window,
                20,
            )
            .await
            .unwrap_or_default();
            let rows: Vec<Value> = sigs
                .iter()
                .map(|s| {
                    serde_json::json!({ "kind": s.kind, "signature": s.signature, "count": s.count })
                })
                .collect();
            (rows, Value::Null)
        } else {
            (
                Vec::new(),
                Value::from(
                    "unmatched_signatures is omitted: it is aggregated across nodes and keeps no \
                     node attribution, so it cannot be narrowed to this account's groups",
                ),
            )
        };
        let body = serde_json::json!({
            "window": { "from": from_s, "to": to_s },
            "top_nodes_by_volume": volume,
            "severity_mix": severity_mix,
            "unmatched_signatures": unmatched,
            "note": note,
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
        // A write is scoped like a read, and this one is also a read: the 200/404 difference would
        // otherwise tell a scoped caller whether an invisible node currently has that alert.
        if let Some(deny) = self
            .deny_invisible_node_ctx(&ctx, "ack_alert", p.node_id)
            .await
        {
            return deny;
        }
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
        // Node subjects only. The MCP **write** surface is frozen at these three tools (ADR-042
        // 決定 6), so widening the parameter to accept a pool would be a write-surface change, not
        // the read parity this rule is about — a pool alert is readable here and acknowledged from
        // the WebUI or `POST /api/v1/alerts/ack`.
        let subject = yagra_alert::Subject::Node(NodeId::from(p.node_id));
        if let Err(e) =
            crate::api::alerts::apply_ack(&self.state, &subject, p.check_id, severity, view).await
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
        if let Some(deny) = self
            .deny_invisible_node_ctx(&ctx, "open_maintenance", p.node_id)
            .await
        {
            return deny;
        }
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
        // Belt-and-braces: `ManageConfig` is Admin, and an Admin cannot hold a group scope
        // (`auth.rs::ADMIN_IS_UNSCOPED`), so this can only ever pass today. It is here so the tool
        // does not depend on that invariant holding somewhere else — the check is one line, and
        // "a permission that happens to imply unscoped" is not a property to build on silently.
        if let Some(deny) = self
            .deny_invisible_node_ctx(&ctx, "poll_now", p.node_id)
            .await
        {
            return deny;
        }
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

    #[tool(
        description = "Is Yagra itself healthy? Check this before trusting anything else you read. \
                       `section` is one of: pollers (the poller fleet and per-pool summary), \
                       poller_health (poll-loop counters), pools, poller_nodes (which nodes one \
                       poller holds — needs `poller_id`), node_assignment (the inverse: which \
                       poller owns one node — needs `node_id`; the first thing to check when a \
                       single node stops reporting while its pool is fine), monitoring_gaps (recent core↔poller \
                       outages: data missing from these windows is missing, not flat), \
                       dependencies (per-store reachability), hosts (core/poller CPU, memory, \
                       disk), host_trends (one host over time — needs `instance`, optional \
                       `from`/`to`/`step`), forwarding (relay delivery status), credentials \
                       (whether stored credentials still decrypt), version, deployment (which \
                       optional tiers are enabled), upgrade (which binary is actually running — \
                       commit and build profile, not just the version — how much schema is \
                       applied, and whether this deployment could still be taken back to an \
                       earlier release). Sections require different permissions: most need view, \
                       forwarding and upgrade need manage-config, credentials needs \
                       manage-credentials."
    )]
    async fn get_system_health(
        &self,
        Parameters(p): Parameters<SystemHealthParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let Some(section) = HealthSection::parse(&p.section) else {
            return tool_bad_params(
                "get_system_health",
                &format!(
                    "unknown section {:?}; must be one of: {}",
                    p.section,
                    HealthSection::NAMES.join(", ")
                ),
            );
        };
        // Resolve → authorize → scope → availability. The permission check sits above every store
        // lookup so a caller who may not read a section cannot infer, from a 403-vs-unavailable,
        // whether this deployment has that subsystem configured at all.
        if let Some(deny) = self.deny_unless_permitted(&ctx, "get_system_health", section.arg()) {
            return deny;
        }
        let scope = match self.scope_of(&ctx).await {
            Ok(s) => s,
            Err(e) => return tool_api_error("get_system_health", &e),
        };
        self.system_health_in(section, p, &scope).await
    }

    async fn system_health_in(
        &self,
        section: HealthSection,
        p: SystemHealthParams,
        scope: &NodeScope,
    ) -> Result<CallToolResult, McpError> {
        const TOOL: &str = "get_system_health";
        // The sections that read the write side; `dependencies` and `hosts` deliberately do not,
        // because reporting that the database is unreachable is most useful when it is.
        let admin = self.state.admin.as_ref();
        match section {
            HealthSection::Pollers => match admin {
                Some(a) => ok_json(TOOL, &crate::api::pollers::poller_inventory(a).await),
                None => tool_unavailable(TOOL, "the poller inventory requires live mode"),
            },
            HealthSection::PollerHealth => match admin {
                Some(a) => ok_json(TOOL, &a.scheduler_stats.snapshot()),
                None => tool_unavailable(TOOL, "poll-loop counters require live mode"),
            },
            HealthSection::Pools => match admin {
                Some(a) => ok_json(TOOL, &crate::api::nodes::pool_options(a).await),
                None => tool_unavailable(TOOL, "the pool list requires live mode"),
            },
            HealthSection::PollerNodes => {
                let Some(poller_id) = p.poller_id else {
                    return tool_bad_params(TOOL, "section poller_nodes needs `poller_id`");
                };
                match admin {
                    Some(a) => {
                        let page = crate::api::pollers::poller_nodes_page(
                            &self.state,
                            a,
                            poller_id,
                            p.limit,
                            scope,
                        )
                        .await;
                        ok_json(TOOL, &page)
                    }
                    None => tool_unavailable(TOOL, "the poller drill-down requires live mode"),
                }
            }
            HealthSection::NodeAssignment => {
                let Some(node_id) = p.node_id else {
                    return tool_bad_params(TOOL, "section node_assignment needs `node_id`");
                };
                // The one node-scoped section. Out of scope answers exactly what a nonexistent id
                // answers, so the tool cannot be used to confirm a node exists outside the
                // caller's groups.
                if let Some(deny) = deny_invisible_node(&self.state, scope, TOOL, node_id) {
                    return deny;
                }
                match admin {
                    Some(a) => {
                        match crate::api::pollers::node_assignment_of(&self.state, a, node_id).await
                        {
                            Ok(r) => ok_json(TOOL, &r),
                            Err(e) => tool_api_error(TOOL, &e),
                        }
                    }
                    None => tool_unavailable(TOOL, "node assignment requires live mode"),
                }
            }
            HealthSection::MonitoringGaps => match admin {
                Some(a) => ok_json(TOOL, &crate::api::pollers::monitoring_gaps(a).await),
                None => tool_unavailable(TOOL, "monitoring gaps require live mode"),
            },
            HealthSection::Dependencies => ok_json(
                TOOL,
                &crate::api::health::system_health_snapshot(&self.state).await,
            ),
            HealthSection::Hosts => ok_json(TOOL, &crate::api::system::host_inventory(&self.state)),
            HealthSection::HostTrends => {
                let Some(instance) = p.instance else {
                    return tool_bad_params(
                        TOOL,
                        "section host_trends needs `instance` (`core`, or a poller id from \
                         section=hosts)",
                    );
                };
                match crate::api::system::host_trends(&self.state, instance, p.from, p.to, p.step)
                    .await
                {
                    Ok(r) => ok_json(TOOL, &r),
                    Err(e) => tool_api_error(TOOL, &e),
                }
            }
            HealthSection::Forwarding => match admin {
                Some(a) => ok_json(
                    TOOL,
                    &crate::api::forwarding::forwarding_delivery_status(&self.state, a),
                ),
                None => tool_unavailable(TOOL, "forwarding status requires live mode"),
            },
            HealthSection::Credentials => match admin {
                Some(a) => match crate::api::credentials::credential_decrypt_health(a).await {
                    Ok(h) => ok_json(TOOL, &h),
                    Err(e) => tool_api_error(TOOL, &e),
                },
                None => tool_unavailable(TOOL, "credential health requires live mode"),
            },
            HealthSection::Version => ok_json(TOOL, &crate::api::health::running_version()),
            HealthSection::Deployment => {
                ok_json(TOOL, &crate::api::health::client_config(&self.state).await)
            }
            HealthSection::Upgrade => {
                match (self.state.upgrade.as_ref(), self.state.admin.as_ref()) {
                    (Some(u), Some(admin)) => match crate::api::upgrade::upgrade_status(
                        u,
                        self.state.started,
                        &crate::api::upgrade::poller_builds(admin),
                    )
                    .await
                    {
                        Ok(r) => ok_json(TOOL, &r),
                        Err(e) => tool_unavailable(TOOL, &format!("{e}")),
                    },
                    _ => tool_unavailable(TOOL, "the upgrade view requires live mode"),
                }
            }
        }
    }

    #[tool(
        description = "How many nodes were in each state over time, as one aligned series per \
                       state. `from`/`to` are Unix seconds (default the last 24h, max a 90-day \
                       window). Fleet-wide only: the timeline is stored already summed with no \
                       per-node attribution, so a group-scoped token is refused rather than shown \
                       the whole fleet."
    )]
    async fn fleet_state_history(
        &self,
        Parameters(p): Parameters<StateHistoryParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        if let Some(deny) = self.deny_unless_permitted(&ctx, "fleet_state_history", "") {
            return deny;
        }
        match self.scope_of(&ctx).await {
            Ok(scope) => self.state_history_in(p, &scope).await,
            Err(e) => tool_api_error("fleet_state_history", &e),
        }
    }

    async fn state_history_in(
        &self,
        p: StateHistoryParams,
        scope: &NodeScope,
    ) -> Result<CallToolResult, McpError> {
        let Some(admin) = self.state.admin.as_ref() else {
            return tool_unavailable("fleet_state_history", "state history requires live mode");
        };
        // The refusal lives inside the seam, so it cannot be forgotten here.
        match crate::api::fleet::state_history(admin, scope, p.from, p.to).await {
            Ok(h) => ok_json("fleet_state_history", &h),
            Err(e) => tool_api_error("fleet_state_history", &e),
        }
    }

    #[tool(
        description = "Saved report runs. Without `run_id`, the most recent runs (newest first, \
                       `limit` 1–500, default 50), optionally narrowed by `definition_id`, \
                       `state` (queued|running|succeeded|failed — note `succeeded`, not the \
                       `done` an analysis run uses) and `since` (RFC 3339); with `run_id`, that \
                       run plus its rendered result. Fleet-wide only: a rendered report keeps no \
                       per-node attribution, so a group-scoped token is refused rather than shown \
                       the whole fleet."
    )]
    async fn get_report_runs(
        &self,
        Parameters(p): Parameters<ReportRunsParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let arg = if p.run_id.is_some() { "detail" } else { "list" };
        if let Some(deny) = self.deny_unless_permitted(&ctx, "get_report_runs", arg) {
            return deny;
        }
        match self.scope_of(&ctx).await {
            Ok(scope) => self.report_runs_in(p, &scope).await,
            Err(e) => tool_api_error("get_report_runs", &e),
        }
    }

    async fn report_runs_in(
        &self,
        p: ReportRunsParams,
        scope: &NodeScope,
    ) -> Result<CallToolResult, McpError> {
        match p.run_id {
            Some(id) => {
                match crate::api::reports::report_run_detail(&self.state, scope, id).await {
                    Ok(r) => ok_json("get_report_runs", &r),
                    Err(e) => tool_api_error("get_report_runs", &e),
                }
            }
            None => {
                let filter = match crate::api::reports::parse_run_filter(
                    p.definition_id,
                    p.state.as_deref(),
                    p.since.as_deref(),
                ) {
                    Ok(f) => f,
                    Err(e) => return tool_api_error("get_report_runs", &e),
                };
                match crate::api::reports::report_runs(&self.state, scope, p.limit, &filter).await {
                    Ok(rows) => ok_json("get_report_runs", &rows),
                    Err(e) => tool_api_error("get_report_runs", &e),
                }
            }
        }
    }

    #[tool(
        description = "The audit log: who changed, acknowledged or triggered what, newest first. \
                       `limit` is 1–500 (default 100); `before` is an RFC 3339 timestamp for the \
                       next page. Narrow with `since`/`until` (the window to search, distinct from \
                       the `before` cursor), `q` (free text over the username and the action), \
                       `action` (post|put|patch|delete|login|mcp — `login` covers local, LDAP and \
                       OIDC sign-ins; `mcp` covers actions taken through this tool surface) and \
                       `status` (ok|client|server). `action` and `status` each take several values \
                       comma-separated. Requires the view-audit permission, which is separate from \
                       view."
    )]
    async fn get_audit(
        &self,
        Parameters(p): Parameters<AuditParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        if let Some(deny) = self.deny_unless_permitted(&ctx, "get_audit", "") {
            return deny;
        }
        self.audit_in(p).await
    }

    async fn audit_in(&self, p: AuditParams) -> Result<CallToolResult, McpError> {
        // Availability is checked **here** rather than left to `audit_page`, and the two surfaces
        // therefore order the two failures differently: REST answers 400 for a malformed filter on
        // a deployment with no log, this answers "unavailable" first. That is deliberate and
        // pre-dates Inc.4b — an assistant that gets a typed "this deployment keeps no audit log"
        // stops asking, whereas a 400 about a cursor invites it to retry with a different cursor
        // forever. `the_audit_tool_reports_a_missing_write_side_rather_than_an_empty_log` pins it.
        if self.state.admin.is_none() {
            return tool_unavailable("get_audit", "the audit log requires live mode");
        }
        // An unparseable cursor is a 400 here as it is over REST, never a silent jump back to the
        // newest page — a client walking the log would otherwise loop forever on page one. The
        // whole page function is the shared seam, so the filter cannot be validated more loosely
        // here than it is over REST (which is the drift `parse_event_filter` already paid for).
        let input = crate::api::audit::AuditFilterInput {
            limit: p.limit,
            before: p.before.as_deref(),
            since: p.since.as_deref(),
            until: p.until.as_deref(),
            q: p.q.as_deref(),
            action: p.action.as_deref(),
            status: p.status.as_deref(),
        };
        match crate::api::audit::audit_page(&self.state, input).await {
            Ok(rows) => ok_json("get_audit", &rows),
            Err(e) => tool_api_error("get_audit", &e),
        }
    }

    #[tool(
        description = "The recorded DNS resolution chain for a DNS-monitor node. By default the \
                       current chain and how long it has held; with `history=true`, the log of \
                       changes (newest first, `limit` 1–200). A node that has never resolved \
                       returns an availability note rather than an error."
    )]
    async fn get_dns_chain(
        &self,
        Parameters(p): Parameters<DnsChainParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let arg = if p.history.unwrap_or(false) {
            "history"
        } else {
            "current"
        };
        if let Some(deny) = self.deny_unless_permitted(&ctx, "get_dns_chain", arg) {
            return deny;
        }
        match self.scope_of(&ctx).await {
            Ok(scope) => self.dns_chain_in(p, &scope).await,
            Err(e) => tool_api_error("get_dns_chain", &e),
        }
    }

    async fn dns_chain_in(
        &self,
        p: DnsChainParams,
        scope: &NodeScope,
    ) -> Result<CallToolResult, McpError> {
        const TOOL: &str = "get_dns_chain";
        if let Some(deny) = deny_invisible_node(&self.state, scope, TOOL, p.node_id) {
            return deny;
        }
        let Some(admin) = self.state.admin.as_ref() else {
            return tool_unavailable(TOOL, "the resolution chain requires live mode");
        };
        if p.history.unwrap_or(false) {
            let before =
                match crate::api::checks::parse_history_cursor(p.before_at.as_deref(), p.before_id)
                {
                    Ok(c) => c,
                    Err(e) => return tool_api_error(TOOL, &e),
                };
            match crate::api::checks::dns_chain_history(admin, p.node_id, p.limit, before).await {
                Ok(h) => ok_json(TOOL, &h),
                Err(e) => tool_api_error(TOOL, &e),
            }
        } else {
            match crate::api::checks::dns_chain_current(admin, p.node_id).await {
                Ok(c) => ok_json(TOOL, &c),
                Err(e) => tool_api_error(TOOL, &e),
            }
        }
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
        let Some(identity) = authed_for(&ctx, want) else {
            return tool_forbidden(
                TOOL,
                &format!("this token lacks {} permission", permission_label(want)),
            );
        };
        let scope = match self.scope_of(&ctx).await {
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

    #[tool(
        description = "Read Yagra's own configuration — what it is set up to monitor, alert on, \
                       notify and forward, and how. `kind` is one of: **alerting/notification** — \
                       thresholds (`limit` 1–500), event_rules, event_sources, \
                       notification_channels, routing_rules; **collection** — profiles, \
                       profile_templates (needs `profile_id`), collection_templates, \
                       template_items (needs `template_id`), node_collection (one node's collected \
                       metrics — needs `node_id`; `resolved=true` for the effective set the poller \
                       actually uses), classification_rules, mib_catalog (`search` filters, \
                       `limit` 1–2000, default 100); **per-node checks** — url_check, dns_check \
                       (both need `node_id`); **discovery** — discovery_candidates (`limit` 1–50, \
                       default 10), discovery_scan (needs `scan_id`); **Meraki** — meraki_orgs, \
                       meraki_networks (needs `org_id`), meraki_polling; **forwarding** — \
                       forward_destinations; **reports** — report_definitions, report_schedules; \
                       **deployment settings** — retention, adjacency_settings, llm, roles, oidc, \
                       ldap. Kinds require different permissions: oidc and ldap need manage-users; \
                       mib_catalog, url_check, dns_check, discovery_candidates, the three meraki \
                       kinds, the two report kinds, retention, adjacency_settings and roles need \
                       view; the rest need manage-config. This reads configuration only — no tool \
                       changes it. No stored secret is returned: url_check reports whether a \
                       credential is bound, not which one."
    )]
    async fn get_config(
        &self,
        Parameters(p): Parameters<ConfigParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let Some(kind) = ConfigKind::parse(&p.kind) else {
            return tool_bad_params(
                "get_config",
                &format!(
                    "unknown kind {:?}; must be one of: {}",
                    p.kind,
                    ConfigKind::NAMES.join(", ")
                ),
            );
        };
        // Resolve → authorize → scope → availability, as `get_system_health` does and for the same
        // reason: the permission check sits above every store lookup so a caller who may not read a
        // kind cannot infer, from a 403-vs-unavailable, whether this deployment has that subsystem
        // configured at all.
        if let Some(deny) = self.deny_unless_permitted(&ctx, "get_config", kind.arg()) {
            return deny;
        }
        let scope = match self.scope_of(&ctx).await {
            Ok(s) => s,
            Err(e) => return tool_api_error("get_config", &e),
        };
        self.config_in(kind, p, &scope).await
    }

    async fn config_in(
        &self,
        kind: ConfigKind,
        p: ConfigParams,
        scope: &NodeScope,
    ) -> Result<CallToolResult, McpError> {
        const TOOL: &str = "get_config";

        // Required-id and scope both run **above** the availability check, which is the ordering
        // `get_system_health` documents and the reason this prelude exists rather than a guard per
        // arm: a caller who cannot see a node must get the same answer whether or not this
        // deployment has a write side, or the 503-vs-unavailable difference is itself a
        // disclosure. `required_id` is the single source of which kinds take one.
        let id = match kind.required_id() {
            None => Uuid::nil(),
            Some(want) => {
                let got = match want {
                    ConfigId::Node => p.node_id,
                    ConfigId::Template => p.template_id,
                    ConfigId::Profile => p.profile_id,
                    ConfigId::Org => p.org_id,
                    ConfigId::Scan => p.scan_id,
                };
                let Some(id) = got else {
                    return tool_bad_params(
                        TOOL,
                        &format!("kind {} needs `{}`", kind.arg(), want.param()),
                    );
                };
                if want == ConfigId::Node {
                    // The same answer a nonexistent id gets, so the tool cannot confirm that a node
                    // exists outside the caller's groups.
                    if let Some(deny) = deny_invisible_node(&self.state, scope, TOOL, id) {
                        return deny;
                    }
                }
                id
            }
        };
        // `id` is `Uuid::nil()` for the 21 kinds that need none, and every arm that reads it is one
        // `required_id` just validated — so there is no unwrap here and no second copy of the fact.
        let Some(a) = self.state.admin.as_ref() else {
            return tool_unavailable(TOOL, "reading configuration requires live mode");
        };
        match kind {
            // ── alerting / notification ──────────────────────────────────────
            ConfigKind::Thresholds => {
                // No filter: `get_config` is a configuration dump, and its callers ask for the
                // ruleset rather than a slice of it — see `threshold_page` for the reasoning.
                match crate::api::thresholds::threshold_page(a, p.limit, &Default::default()).await
                {
                    Ok(page) => ok_json(TOOL, &page),
                    Err(e) => tool_api_error(TOOL, &e),
                }
            }
            ConfigKind::EventRules => match a.events.list_rules().await {
                Ok(rules) => ok_json(TOOL, &rules),
                Err(e) => tool_error(TOOL, "list event rules", &e),
            },
            ConfigKind::EventSources => match a.events.list_sources().await {
                Ok(sources) => ok_json(TOOL, &sources),
                Err(e) => tool_error(TOOL, "list event sources", &e),
            },
            ConfigKind::NotificationChannels => match a.notifications.list_channels().await {
                Ok(list) => ok_json(TOOL, &list),
                Err(e) => tool_error(TOOL, "list notification channels", &e),
            },
            ConfigKind::RoutingRules => match a.notifications.list_rules().await {
                Ok(list) => ok_json(TOOL, &list),
                Err(e) => tool_error(TOOL, "list routing rules", &e),
            },
            // ── collection ───────────────────────────────────────────────────
            ConfigKind::Profiles => match a.repo.list_profiles().await {
                Ok(list) => ok_json(TOOL, &list),
                Err(e) => tool_error(TOOL, "list profiles", &e),
            },
            ConfigKind::ProfileTemplates => match a.collection.list_profile_templates(id).await {
                Ok(list) => ok_json(TOOL, &list),
                Err(e) => tool_error(TOOL, "list profile templates", &e),
            },
            ConfigKind::CollectionTemplates => match a.collection.list_templates().await {
                Ok(list) => ok_json(TOOL, &list),
                Err(e) => tool_error(TOOL, "list collection templates", &e),
            },
            ConfigKind::TemplateItems => match a.collection.list_template_items(id).await {
                Ok(list) => ok_json(TOOL, &list),
                Err(e) => tool_error(TOOL, "list template items", &e),
            },
            ConfigKind::NodeCollection => {
                match crate::api::collection::node_collection(a, id, p.resolved.unwrap_or(false))
                    .await
                {
                    Ok(set) => ok_json(TOOL, &set),
                    Err(e) => tool_api_error(TOOL, &e),
                }
            }
            ConfigKind::ClassificationRules => match a.classification.list_rules().await {
                Ok(list) => ok_json(TOOL, &list),
                Err(e) => tool_error(TOOL, "list classification rules", &e),
            },
            ConfigKind::MibCatalog => {
                // Default 100 where REST defaults to the 2000 cap: a model asking about one OID
                // does not want the whole catalog in its context, and `search` is the narrowing
                // this branch expects to be used with. The *ceiling* is shared (`api::mib`); only
                // the default differs, which is the `get_topology` precedent.
                match crate::api::mib::mib_catalog(
                    a,
                    p.search.as_deref(),
                    Some(p.limit.unwrap_or(100)),
                )
                .await
                {
                    Ok(list) => ok_json(TOOL, &list),
                    Err(e) => tool_api_error(TOOL, &e),
                }
            }
            // ── per-node checks ──────────────────────────────────────────────
            ConfigKind::UrlCheck => {
                use crate::api::checks::CheckKind as _;
                match crate::api::checks::UrlCheck::load(a, id).await {
                    Ok(Some(cfg)) => {
                        ok_json(TOOL, &crate::mcp::dto::UrlCheckDto::from_config(&cfg))
                    }
                    Ok(None) => tool_unavailable(TOOL, "that node has no URL check configured"),
                    Err(e) => tool_error(TOOL, "load url check", &e),
                }
            }
            ConfigKind::DnsCheck => {
                use crate::api::checks::CheckKind as _;
                match crate::api::checks::DnsCheck::load(a, id).await {
                    Ok(Some(cfg)) => ok_json(TOOL, &cfg),
                    Ok(None) => tool_unavailable(TOOL, "that node has no DNS check configured"),
                    Err(e) => tool_error(TOOL, "load dns check", &e),
                }
            }
            // ── discovery ────────────────────────────────────────────────────
            ConfigKind::DiscoveryCandidates => {
                // `matched_credential_id` stays on these rows, and that is a decision rather than
                // an oversight: `SECRET_KEYS` is an exact-match rule, so nothing would have caught
                // it either way. Unlike a URL check's binding — which a model can neither resolve
                // nor use — *which stored credential answered on an unclassified device* is the
                // answer to a discovery-triage question, and it names something an operator can
                // look up in the UI. Different question, different treatment.
                let limit = p.limit.and_then(|n| usize::try_from(n).ok());
                ok_json(
                    TOOL,
                    &crate::api::discovery::recent_candidates(&self.state, limit),
                )
            }
            ConfigKind::DiscoveryScan => match a.discovery.get(id) {
                Some(status) => ok_json(TOOL, &status),
                None => tool_unavailable(TOOL, "no scan with that id"),
            },
            // ── Meraki ───────────────────────────────────────────────────────
            ConfigKind::MerakiOrgs => match crate::api::meraki::org_views(a).await {
                Ok(list) => ok_json(TOOL, &list),
                Err(e) => tool_api_error(TOOL, &e),
            },
            ConfigKind::MerakiNetworks => match crate::api::meraki::network_views(a, id).await {
                Ok(list) => ok_json(TOOL, &list),
                Err(e) => tool_api_error(TOOL, &e),
            },
            ConfigKind::MerakiPolling => {
                ok_json(TOOL, &crate::api::meraki::polling_switch(a).await)
            }
            // ── forwarding ───────────────────────────────────────────────────
            ConfigKind::ForwardDestinations => match a.forward.list().await {
                Ok(rows) => ok_json(TOOL, &rows),
                Err(e) => tool_error(TOOL, "list forward destinations", &e),
            },
            // ── reports ──────────────────────────────────────────────────────
            ConfigKind::ReportDefinitions => match a.reports.repo().list_definitions().await {
                Ok(list) => ok_json(TOOL, &list),
                Err(e) => tool_error(TOOL, "list report definitions", &e),
            },
            ConfigKind::ReportSchedules => match a.reports.repo().list_schedules().await {
                Ok(list) => ok_json(TOOL, &list),
                Err(e) => tool_error(TOOL, "list report schedules", &e),
            },
            // ── deployment settings ──────────────────────────────────────────
            ConfigKind::Retention => ok_json(
                TOOL,
                &crate::api::retention::retention_policy(&self.state, a).await,
            ),
            ConfigKind::AdjacencySettings => {
                ok_json(TOOL, &crate::api::neighbors::adjacency_config(a).await)
            }
            ConfigKind::Llm => match crate::api::rca::llm_config_view(a).await {
                Ok(view) => ok_json(TOOL, &view),
                Err(e) => tool_api_error(TOOL, &e),
            },
            // Pure: the matrix is the type system's, not the deployment's.
            ConfigKind::Roles => ok_json(TOOL, &crate::api::users::roles_matrix()),
            ConfigKind::Oidc => match self.state.oidc.as_ref() {
                Some(oidc) => match oidc.list().await {
                    Ok(list) => ok_json(TOOL, &list),
                    Err(e) => tool_error(TOOL, "list oidc providers", &e),
                },
                None => tool_unavailable(TOOL, "this deployment persists no SSO configuration"),
            },
            ConfigKind::Ldap => match self.state.ldap.as_ref() {
                Some(ldap) => match ldap.view().await {
                    Ok(view) => ok_json(TOOL, &view),
                    Err(e) => tool_error(TOOL, "read ldap config", &e),
                },
                None => tool_unavailable(TOOL, "this deployment persists no LDAP configuration"),
            },
        }
    }

    /// Every tool's published name, description and argument schema (ADR-028 WS-G).
    ///
    /// Reads the router this instance already holds — `list_all()` touches no `Peer`, no session and
    /// no transport, which is the fact that made WS-G cheap. It is exposed here rather than in
    /// `rca/agent.rs` because the router field and the macro-generated constructor are private to
    /// this module.
    pub(crate) fn published_tools(&self) -> Vec<rmcp::model::Tool> {
        self.tool_router.list_all()
    }

    /// Run one tool by name, in-process, with an already-resolved scope (ADR-028 WS-G).
    ///
    /// **This is the wiring the module doc promised.** `ToolRouter::call` is unreachable from here —
    /// it needs a `RequestContext`, which needs a `Peer`, whose constructor is crate-private in rmcp
    /// — so the RCA agent dispatches by name to the same `*_in` bodies the `#[tool]` wrappers use.
    ///
    /// It lives in this file rather than in `rca/agent.rs` because the arms need the private
    /// discriminator parsers (`ConfigKind`, `HealthSection`, `fleet_summary_kind`) that are only in
    /// scope here. **Policy is not here**: which tools an agent may call, and with what permission,
    /// is `rca::agent`'s to decide — this function runs whatever it is given.
    ///
    /// Results come back as `CallToolResult` rather than typed values on purpose. Every byte has
    /// been through `ok_json`, which is the `dto.rs` sanitization boundary; bypassing it to get a
    /// typed value would discard the ADR-018 canary coverage for the sake of a shape the caller
    /// immediately re-serializes anyway.
    pub(crate) async fn call_in(
        &self,
        name: &str,
        args: serde_json::Value,
        scope: &NodeScope,
    ) -> Result<CallToolResult, McpError> {
        /// Decode the model's arguments, or refuse in the tool's own vocabulary.
        macro_rules! p {
            ($t:ty) => {
                match serde_json::from_value::<$t>(args) {
                    Ok(v) => v,
                    Err(e) => {
                        return tool_bad_params(name, &format!("could not read the arguments: {e}"))
                    }
                }
            };
        }
        match name {
            "get_fleet_summary" => {
                let p = p!(FleetSummaryParams);
                match fleet_summary_kind(p.kind.as_deref()) {
                    Some(FleetSummaryKind::Summary) => self.fleet_summary_in(scope).await,
                    Some(FleetSummaryKind::Coverage) => self.fleet_coverage_in(scope).await,
                    None => tool_bad_params(name, "`kind` must be summary or coverage"),
                }
            }
            "list_nodes" => self.list_nodes_in(p!(ListNodesParams), scope).await,
            "get_node_status" => self.node_status_in(p!(NodeIdParams), scope).await,
            "get_active_alerts" => self.active_alerts_in(p!(ActiveAlertsParams), scope).await,
            "get_alert_history" => self.alert_history_in(p!(AlertHistoryParams), scope).await,
            "query_metrics" => self.query_metrics_in(p!(QueryMetricsParams), scope).await,
            "get_interface_series" => {
                self.interface_series_in(p!(InterfaceSeriesParams), scope)
                    .await
            }
            "top_metrics" => self.top_metrics_in(p!(TopMetricsParams), scope).await,
            "top_interfaces" => self.top_interfaces_in(p!(TopInterfacesParams), scope).await,
            "fleet_throughput" => {
                self.fleet_throughput_in(p!(FleetThroughputParams), scope)
                    .await
            }
            "get_neighbors" => self.neighbors_in(p!(NeighborsParams), scope).await,
            "list_discovered_endpoints" => {
                self.discovered_endpoints_in(p!(DiscoveredEndpointsParams), scope)
                    .await
            }
            "list_node_groups" => {
                self.list_node_groups_in(p!(ListNodeGroupsParams), scope)
                    .await
            }
            "list_suppressions" => self.list_suppressions_in(scope).await,
            "alert_trends" => self.alert_trends_in(p!(AlertTrendsParams), scope).await,
            "search_analysis_findings" => {
                self.search_analysis_findings_in(p!(SearchFindingsParams), scope)
                    .await
            }
            "get_topology" => self.topology_in(p!(TopologyParams), scope).await,
            "top_flows" => self.top_flows_in(p!(TopFlowsParams), scope).await,
            "flow_fanout" => self.flow_fanout_in(p!(TopFlowsParams), scope).await,
            "get_analysis_findings" => {
                self.analysis_findings_in(p!(AnalysisJobIdParams), scope)
                    .await
            }
            "list_analyses" => self.list_analyses_in(p!(ListAnalysesParams), scope).await,
            "search_events" => self.search_events_in(p!(EventSearchParams), scope).await,
            "event_stats" => self.event_stats_in(p!(EventStatsParams), scope).await,
            "fleet_state_history" => self.state_history_in(p!(StateHistoryParams), scope).await,
            "get_report_runs" => self.report_runs_in(p!(ReportRunsParams), scope).await,
            "get_dns_chain" => self.dns_chain_in(p!(DnsChainParams), scope).await,
            "get_system_health" => {
                let p = p!(SystemHealthParams);
                match HealthSection::parse(&p.section) {
                    Some(section) => self.system_health_in(section, p, scope).await,
                    None => tool_bad_params(
                        name,
                        &format!(
                            "unknown section {:?}; must be one of: {}",
                            p.section,
                            HealthSection::NAMES.join(", ")
                        ),
                    ),
                }
            }
            "get_config" => {
                let p = p!(ConfigParams);
                match ConfigKind::parse(&p.kind) {
                    Some(kind) => self.config_in(kind, p, scope).await,
                    None => tool_bad_params(
                        name,
                        &format!(
                            "unknown kind {:?}; must be one of: {}",
                            p.kind,
                            ConfigKind::NAMES.join(", ")
                        ),
                    ),
                }
            }
            // Not "every tool minus a few": the caller's allow-list decides what reaches here, and
            // anything it lets through that this table does not know is a wiring mistake rather than
            // model input.
            other => tool_bad_params(name, &format!("no in-process tool named {other:?}")),
        }
    }

    /// The caller's resolved visibility scope for this request (ADR-028 WS-F).
    ///
    /// This is why every read tool takes a `RequestContext` it otherwise has no use for: a tool
    /// body cannot ask for the caller any other way, and a scope resolved from anything but the
    /// authenticated principal is not a scope.
    ///
    /// **Fails closed on a missing identity.** `mcp_auth_mw` inserts one into every request it lets
    /// through, so the fallback is unreachable — which is exactly why it must resolve to "sees
    /// nothing" rather than "sees everything". An unreachable branch is the one nobody notices
    /// becoming reachable.
    async fn scope_of(&self, ctx: &RequestContext<RoleServer>) -> Result<NodeScope, ApiError> {
        let principal = identity_of(ctx).map(|id| id.principal).unwrap_or_else(|| {
            tracing::error!("MCP tool ran with no authenticated identity; treating it as empty");
            yagra_common::Principal::new(
                yagra_common::Role::Viewer,
                yagra_common::Scope::Groups(std::collections::BTreeSet::new()),
            )
        });
        crate::api::scope::resolve(&self.state, &principal).await
    }

    /// [`deny_invisible_node`] for a tool that still holds its `RequestContext` (the write tools,
    /// which need the identity anyway and so never took the scope as a parameter).
    async fn deny_invisible_node_ctx(
        &self,
        ctx: &RequestContext<RoleServer>,
        tool: &str,
        node: Uuid,
    ) -> Option<Result<CallToolResult, McpError>> {
        match self.scope_of(ctx).await {
            Ok(scope) => deny_invisible_node(&self.state, &scope, tool, node),
            Err(e) => Some(tool_api_error(tool, &e)),
        }
    }

    /// Resolve node ids → display names via the live repo (empty in skeleton mode). Deduplicated so a
    /// repeated node in an alert list doesn't bloat the `IN (…)` query.
    ///
    /// Takes the scope because a name is data: resolving ids the caller may not see would leak the
    /// names of out-of-scope nodes through any list that happens to mention one.
    async fn resolve_names(
        &self,
        scope: &NodeScope,
        ids: impl Iterator<Item = Uuid>,
    ) -> HashMap<Uuid, String> {
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
            Some(admin) => admin
                .repo
                .node_names(scope.group_filter(), &ids)
                .await
                .unwrap_or_default(),
            None => HashMap::new(),
        }
    }

    /// [`analysis_report_body`] with the findings `scope` may not see removed.
    ///
    /// Shares the filter with the REST findings endpoint rather than repeating it, because the rule
    /// it encodes is subtle: a run's scope is fixed when it starts, so a node can move between
    /// groups before anyone reads the results, and a finding with no node at all belongs to nobody's
    /// scope.
    fn scoped_report_body(
        &self,
        scope: &NodeScope,
        report: crate::api::analysis::AnalysisReport,
    ) -> Value {
        let findings = crate::api::analysis::visible_findings(&self.state, scope, report.findings);
        analysis_report_body(&crate::api::analysis::AnalysisReport {
            job: report.job,
            findings,
        })
    }

    // The AS-name fills used to live here, as a copy of the REST ones that had already lost their
    // `asn != 0` short-circuit. That divergence was harmless — `IpAsnDb::from_tsv` drops `asn = 0`
    // rows, so `name_of(0)` is `None` regardless — but it is the copy drifting silently that
    // matters, since nothing would have caught it if the loader's behaviour changed. Both surfaces
    // now reach the fill through `api::flow::flow_agg_rows`.

    /// Refuse unless the caller holds what this folded branch demands (ADR-042 I3a).
    ///
    /// The permission comes from `folded::FOLDED_READS`, never from a literal here, because the
    /// endpoints behind one folded tool do not share one — `get_system_health` alone spans `View`,
    /// `ManageConfig` and `ManageCredentials`. A test compares each row against the REST handler's
    /// own extractor, so this cannot drift from what the WebUI enforces.
    fn deny_unless_permitted(
        &self,
        ctx: &RequestContext<RoleServer>,
        tool: &'static str,
        arg: &str,
    ) -> Option<Result<CallToolResult, McpError>> {
        let want = crate::mcp::folded::required_permission(tool, arg);
        if authed_for(ctx, want).is_some() {
            return None;
        }
        Some(tool_forbidden(
            tool,
            &format!("this token lacks {} permission", permission_label(want)),
        ))
    }
}

/// How this surface names a permission to a model.
///
/// `Permission::key()` is the stored form (`manage_config`) and is what a *database* row holds.
/// Every tool description and refusal on this surface has always used the hyphenated spelling
/// (`manage-config`, `ack-alerts`), so a permission rendered from the key would read differently in
/// a tool's error than in the description that told the model what it needed — a small mismatch,
/// but this text is a specification a model reasons from, and two spellings for one thing is
/// exactly what makes it guess.
fn permission_label(p: Permission) -> String {
    p.key().replace('_', "-")
}

/// Which fleet view `get_fleet_summary` was asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FleetSummaryKind {
    Summary,
    Coverage,
}

/// Resolve the `kind` argument, or `None` for one that names neither view.
///
/// Split out for the same reason as `topology_kind`: the `#[tool]` wrapper cannot be called from a
/// test, so the decision has to live somewhere a test can reach. Omitted means the summary — this
/// tool is the documented starting point and had no argument before I3a, so a caller that passes
/// nothing must keep getting what it always got.
fn fleet_summary_kind(kind: Option<&str>) -> Option<FleetSummaryKind> {
    match kind {
        None | Some("summary") => Some(FleetSummaryKind::Summary),
        Some("coverage") => Some(FleetSummaryKind::Coverage),
        Some(_) => None,
    }
}

/// Which self-health question `get_system_health` was asked (ADR-042 I3a).
///
/// Split out of the tool body so the folding decision is testable without a `RequestContext`, the
/// same shape `topology_kind` uses. Parsing is exact: a caller who types `Pollers` is told, rather
/// than silently handed a different section.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HealthSection {
    Pollers,
    PollerHealth,
    Pools,
    PollerNodes,
    NodeAssignment,
    MonitoringGaps,
    Dependencies,
    Hosts,
    HostTrends,
    Forwarding,
    Credentials,
    Version,
    Deployment,
    Upgrade,
}

impl HealthSection {
    /// Every accepted `section` value, in the order the description lists them.
    const NAMES: &'static [&'static str] = &[
        "pollers",
        "poller_health",
        "pools",
        "poller_nodes",
        "node_assignment",
        "monitoring_gaps",
        "dependencies",
        "hosts",
        "host_trends",
        "forwarding",
        "credentials",
        "version",
        "deployment",
        "upgrade",
    ];

    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "pollers" => Self::Pollers,
            "poller_health" => Self::PollerHealth,
            "pools" => Self::Pools,
            "poller_nodes" => Self::PollerNodes,
            "node_assignment" => Self::NodeAssignment,
            "monitoring_gaps" => Self::MonitoringGaps,
            "dependencies" => Self::Dependencies,
            "hosts" => Self::Hosts,
            "host_trends" => Self::HostTrends,
            "forwarding" => Self::Forwarding,
            "credentials" => Self::Credentials,
            "version" => Self::Version,
            "deployment" => Self::Deployment,
            "upgrade" => Self::Upgrade,
            _ => return None,
        })
    }

    /// The `folded::FOLDED_READS` key for this section — the string the permission is filed under.
    fn arg(self) -> &'static str {
        match self {
            Self::Pollers => "pollers",
            Self::PollerHealth => "poller_health",
            Self::Pools => "pools",
            Self::PollerNodes => "poller_nodes",
            Self::NodeAssignment => "node_assignment",
            Self::MonitoringGaps => "monitoring_gaps",
            Self::Dependencies => "dependencies",
            Self::Hosts => "hosts",
            Self::HostTrends => "host_trends",
            Self::Forwarding => "forwarding",
            Self::Credentials => "credentials",
            Self::Version => "version",
            Self::Deployment => "deployment",
            Self::Upgrade => "upgrade",
        }
    }
}

/// The id a [`ConfigKind`] needs, and which parameter carries it.
///
/// Named per referent rather than one polymorphic `id`, which is what keeps the 28-branch fold
/// inside the rule I1 set: no argument's meaning changes with another. `org_id` never means a node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConfigId {
    Node,
    Template,
    Profile,
    Org,
    Scan,
}

impl ConfigId {
    /// The parameter name, as the refusal message and the published schema both spell it.
    fn param(self) -> &'static str {
        match self {
            Self::Node => "node_id",
            Self::Template => "template_id",
            Self::Profile => "profile_id",
            Self::Org => "org_id",
            Self::Scan => "scan_id",
        }
    }
}

/// Which configuration read `get_config` was asked for (ADR-042 I3b).
///
/// Twenty-eight branches behind one `kind`, following [`HealthSection`] rather than becoming
/// twenty-eight tools. The fold is defensible for the reason `alert_trends` was: no argument's
/// meaning changes with another. The five ids are named per referent — `node_id`, `template_id`,
/// `profile_id`, `org_id`, `scan_id` — so none of them ever means two things, which is the rule
/// I1 rejected `top_metrics` over. A single polymorphic `id` would have broken it.
///
/// The other half of the argument is recovery: a caller who mistypes `kind` is handed
/// [`Self::NAMES`] and can retry. A caller who picks the wrong *tool* is not told the right one
/// exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConfigKind {
    Thresholds,
    EventRules,
    EventSources,
    NotificationChannels,
    RoutingRules,
    Profiles,
    ProfileTemplates,
    CollectionTemplates,
    TemplateItems,
    NodeCollection,
    ClassificationRules,
    MibCatalog,
    UrlCheck,
    DnsCheck,
    DiscoveryCandidates,
    DiscoveryScan,
    MerakiOrgs,
    MerakiNetworks,
    MerakiPolling,
    ForwardDestinations,
    ReportDefinitions,
    ReportSchedules,
    Retention,
    AdjacencySettings,
    Llm,
    Roles,
    Oidc,
    Ldap,
}

impl ConfigKind {
    /// Every accepted `kind` value, in the order the description lists them.
    ///
    /// Several names are longer than the REST path's last segment on purpose. `forward_destinations`
    /// rather than `forwarding` because `get_system_health(section="forwarding")` already publishes
    /// that word for the delivery status; `adjacency_settings` rather than `neighbors` because
    /// `get_neighbors` is a live per-node read and this is a deployment-wide policy;
    /// `report_schedules` rather than `schedules` because `list_analyses(kind="schedules")` has it.
    /// One word meaning two things across two tools is how a model comes to guess.
    const NAMES: &'static [&'static str] = &[
        "thresholds",
        "event_rules",
        "event_sources",
        "notification_channels",
        "routing_rules",
        "profiles",
        "profile_templates",
        "collection_templates",
        "template_items",
        "node_collection",
        "classification_rules",
        "mib_catalog",
        "url_check",
        "dns_check",
        "discovery_candidates",
        "discovery_scan",
        "meraki_orgs",
        "meraki_networks",
        "meraki_polling",
        "forward_destinations",
        "report_definitions",
        "report_schedules",
        "retention",
        "adjacency_settings",
        "llm",
        "roles",
        "oidc",
        "ldap",
    ];

    /// Exact match, with no default. A `kind` is the whole question here — unlike `get_topology`,
    /// where one graph is the obvious default — so a caller who omits it or mistypes it is told,
    /// never quietly served something else.
    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "thresholds" => Self::Thresholds,
            "event_rules" => Self::EventRules,
            "event_sources" => Self::EventSources,
            "notification_channels" => Self::NotificationChannels,
            "routing_rules" => Self::RoutingRules,
            "profiles" => Self::Profiles,
            "profile_templates" => Self::ProfileTemplates,
            "collection_templates" => Self::CollectionTemplates,
            "template_items" => Self::TemplateItems,
            "node_collection" => Self::NodeCollection,
            "classification_rules" => Self::ClassificationRules,
            "mib_catalog" => Self::MibCatalog,
            "url_check" => Self::UrlCheck,
            "dns_check" => Self::DnsCheck,
            "discovery_candidates" => Self::DiscoveryCandidates,
            "discovery_scan" => Self::DiscoveryScan,
            "meraki_orgs" => Self::MerakiOrgs,
            "meraki_networks" => Self::MerakiNetworks,
            "meraki_polling" => Self::MerakiPolling,
            "forward_destinations" => Self::ForwardDestinations,
            "report_definitions" => Self::ReportDefinitions,
            "report_schedules" => Self::ReportSchedules,
            "retention" => Self::Retention,
            "adjacency_settings" => Self::AdjacencySettings,
            "llm" => Self::Llm,
            "roles" => Self::Roles,
            "oidc" => Self::Oidc,
            "ldap" => Self::Ldap,
            _ => return None,
        })
    }

    /// Which id this kind cannot answer without, if any.
    ///
    /// Exhaustive, so a new kind has to state whether it takes one, and **the only place that fact
    /// is written**: `config_in`'s prelude validates from this and the arms use what it produced,
    /// rather than each arm carrying its own copy of "this one needs an id".
    fn required_id(self) -> Option<ConfigId> {
        match self {
            Self::NodeCollection | Self::UrlCheck | Self::DnsCheck => Some(ConfigId::Node),
            Self::TemplateItems => Some(ConfigId::Template),
            Self::ProfileTemplates => Some(ConfigId::Profile),
            Self::MerakiNetworks => Some(ConfigId::Org),
            Self::DiscoveryScan => Some(ConfigId::Scan),
            Self::Thresholds
            | Self::EventRules
            | Self::EventSources
            | Self::NotificationChannels
            | Self::RoutingRules
            | Self::Profiles
            | Self::CollectionTemplates
            | Self::ClassificationRules
            | Self::MibCatalog
            | Self::DiscoveryCandidates
            | Self::MerakiOrgs
            | Self::MerakiPolling
            | Self::ForwardDestinations
            | Self::ReportDefinitions
            | Self::ReportSchedules
            | Self::Retention
            | Self::AdjacencySettings
            | Self::Llm
            | Self::Roles
            | Self::Oidc
            | Self::Ldap => None,
        }
    }

    /// The `folded::FOLDED_READS` key for this kind — the string the permission is filed under.
    fn arg(self) -> &'static str {
        match self {
            Self::Thresholds => "thresholds",
            Self::EventRules => "event_rules",
            Self::EventSources => "event_sources",
            Self::NotificationChannels => "notification_channels",
            Self::RoutingRules => "routing_rules",
            Self::Profiles => "profiles",
            Self::ProfileTemplates => "profile_templates",
            Self::CollectionTemplates => "collection_templates",
            Self::TemplateItems => "template_items",
            Self::NodeCollection => "node_collection",
            Self::ClassificationRules => "classification_rules",
            Self::MibCatalog => "mib_catalog",
            Self::UrlCheck => "url_check",
            Self::DnsCheck => "dns_check",
            Self::DiscoveryCandidates => "discovery_candidates",
            Self::DiscoveryScan => "discovery_scan",
            Self::MerakiOrgs => "meraki_orgs",
            Self::MerakiNetworks => "meraki_networks",
            Self::MerakiPolling => "meraki_polling",
            Self::ForwardDestinations => "forward_destinations",
            Self::ReportDefinitions => "report_definitions",
            Self::ReportSchedules => "report_schedules",
            Self::Retention => "retention",
            Self::AdjacencySettings => "adjacency_settings",
            Self::Llm => "llm",
            Self::Roles => "roles",
            Self::Oidc => "oidc",
            Self::Ldap => "ldap",
        }
    }
}

// ── Tool parameter structs (schemas derived for `tools/list`) ─────────────────────────────────────

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
struct FleetSummaryParams {
    /// `summary` (default) for the state tally, or `coverage` for the monitoring blind-spot view.
    kind: Option<String>,
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
struct ConfigParams {
    /// Which configuration to read. Required; see the tool description for the 28 values.
    kind: String,
    /// The node (kind=node_collection | url_check | dns_check).
    node_id: Option<Uuid>,
    /// The collection template whose items to list (kind=template_items).
    template_id: Option<Uuid>,
    /// The profile whose templates to list (kind=profile_templates).
    profile_id: Option<Uuid>,
    /// The Meraki organization whose networks to list (kind=meraki_networks).
    org_id: Option<Uuid>,
    /// The discovery scan to report on (kind=discovery_scan).
    scan_id: Option<Uuid>,
    /// Return the effective set the poller collects rather than the node's own overrides
    /// (kind=node_collection; default false).
    resolved: Option<bool>,
    /// Case-insensitive substring over metric name / OID / vendor (kind=mib_catalog).
    search: Option<String>,
    /// Row cap (kind=thresholds 1–500 default 500; mib_catalog 1–2000 default 100;
    /// discovery_candidates 1–50 default 10).
    limit: Option<i64>,
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
struct SystemHealthParams {
    /// Which self-health question: pollers | poller_health | pools | poller_nodes |
    /// monitoring_gaps | dependencies | hosts | host_trends | forwarding | credentials | version |
    /// deployment.
    section: String,
    /// The poller to drill into (section=poller_nodes).
    poller_id: Option<String>,
    /// Which node's poller assignment to resolve (section=node_assignment).
    node_id: Option<Uuid>,
    /// Which host to trend (section=host_trends): `core`, or a poller id from section=hosts.
    instance: Option<String>,
    /// Trend window start, Unix seconds (section=host_trends; default 1h ago).
    from: Option<i64>,
    /// Trend window end, Unix seconds (section=host_trends; default now).
    to: Option<i64>,
    /// Trend resolution in seconds (section=host_trends; clamped to the window).
    step: Option<u64>,
    /// Max nodes to return (section=poller_nodes; 1–500, default 500).
    limit: Option<usize>,
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
struct StateHistoryParams {
    /// Window start, Unix seconds (default 24h ago).
    from: Option<i64>,
    /// Window end, Unix seconds (default now). The window may not exceed 90 days.
    to: Option<i64>,
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
struct ReportRunsParams {
    /// One run to fetch with its rendered result; omit for the recent-runs list.
    run_id: Option<Uuid>,
    /// Max runs to list (1–500, default 50). Ignored when `run_id` is given.
    limit: Option<i64>,
    /// Only runs generated from this report definition. Ignored when `run_id` is given.
    definition_id: Option<Uuid>,
    /// Only runs in this state: `queued` | `running` | `succeeded` | `failed`. Ignored when
    /// `run_id` is given.
    state: Option<String>,
    /// Only runs created at or after this RFC 3339 timestamp. Ignored when `run_id` is given.
    since: Option<String>,
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
struct AuditParams {
    /// Max rows (1–500, default 100).
    limit: Option<i64>,
    /// Keyset cursor: return rows older than this RFC 3339 timestamp.
    before: Option<String>,
    /// Only entries at or after this RFC 3339 timestamp.
    since: Option<String>,
    /// Only entries at or before this RFC 3339 timestamp.
    until: Option<String>,
    /// Free text matched against the username and the action (case-insensitive substring).
    q: Option<String>,
    /// Action kinds to include, comma-separated: `post` | `put` | `patch` | `delete` | `login` |
    /// `mcp`. Omit for all. An unknown token is an error rather than being ignored.
    action: Option<String>,
    /// Status classes to include, comma-separated: `ok` | `client` | `server`. Omit for all.
    status: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct DnsChainParams {
    /// The DNS-monitor node's UUID.
    node_id: Uuid,
    /// `true` for the log of chain changes instead of the current chain.
    history: Option<bool>,
    /// Max changes to return (history only; 1–200, default 50).
    limit: Option<i64>,
    /// Keyset cursor timestamp (history only). Must be given with `before_id`.
    before_at: Option<String>,
    /// Keyset cursor id (history only). Must be given with `before_at`.
    before_id: Option<i64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct RunRcaParams {
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

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
struct ListNodesParams {
    /// Case-insensitive substring matched against node name or address.
    search: Option<String>,
    /// Max nodes to return (1–100, default 50).
    limit: Option<i64>,
    /// Rolled-up states to include, comma-separated: `ok` | `warning` | `critical` |
    /// `unreachable` | `unknown` | `maintenance`. Omit for all. `warning,critical,unreachable` is
    /// "everything that is not healthy". An unknown token is an error rather than being ignored.
    state: Option<String>,
    /// Monitoring kinds to include, comma-separated: `meraki` | `url` | `dns` | `device`. Omit for
    /// all.
    kind: Option<String>,
    /// Effective poll pools to include, comma-separated (a node's own pool, else the nearest folder
    /// ancestor that sets one, else `default`). Omit for all. Pool names are chosen by the
    /// operator, so an unrecognised one matches nothing rather than being an error.
    pool: Option<String>,
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
pub(crate) struct AlertHistoryParams {
    /// Max rows to return (1–1000, default 100).
    limit: Option<i64>,
    /// Keyset cursor, first half: the oldest returned row's `cursor_at`. Not its `at` — that is
    /// event time, a different clock, and paging on it returns the wrong rows.
    before: Option<String>,
    /// Keyset cursor, second half: the same row's `cursor_id`. Send both — a whole flush of alerts
    /// shares one `cursor_at`, so a timestamp-only cursor skips that flush's remaining rows.
    before_id: Option<Uuid>,
    /// Only transitions recorded at or after this RFC 3339 timestamp.
    since: Option<String>,
    /// Only transitions recorded at or before this RFC 3339 timestamp.
    until: Option<String>,
    /// Severities to include, comma-separated: `info` | `warning` | `critical`. Omit for all. An
    /// unknown token is an error rather than being ignored.
    severity: Option<String>,
    /// Node states to include, comma-separated, e.g. `critical,unreachable`. Omit for all.
    state: Option<String>,
    /// `false` for fires only, `true` for clears only. Omit for both.
    resolved: Option<bool>,
    /// `true` for transitions whose incident has been acknowledged, `false` for those that have
    /// not. Omit for both. Ask `false` for "what fired and nobody has looked at".
    acked: Option<bool>,
    /// Only transitions whose metric name contains this text (case-insensitive), e.g. `cpu`.
    /// Liveness transitions store no metric and never match.
    metric: Option<String>,
    /// Only transitions about this node.
    node_id: Option<Uuid>,
    /// Only transitions about nodes whose name contains this text (case-insensitive). Use this
    /// rather than `node_id` when the question is about a set of nodes, e.g. every `core-sw…`.
    node_q: Option<String>,
    /// Only transitions about nodes in this folder group or any group beneath it.
    group_id: Option<Uuid>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(crate) struct NodeMetricsParams {
    /// The node's UUID.
    node_id: Uuid,
    /// How far back a metric may have last been seen and still count as having data, in seconds
    /// (default 6 hours; clamped).
    within_secs: Option<u64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(crate) struct QueryMetricsParams {
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
struct InterfaceSeriesParams {
    /// The node's UUID.
    node_id: Uuid,
    /// SNMP ifIndex of the interface (from get_node_status's `interfaces`).
    ifindex: u32,
    /// Window start, Unix seconds (default: one hour ago).
    from: Option<i64>,
    /// Window end, Unix seconds (default: now).
    to: Option<i64>,
    /// Sample step in seconds (clamped; default ~120 points across the window).
    step: Option<u64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct TopMetricsParams {
    /// Metric name (icmp_rtt_ms, …) or a logical alias: cpu, memory.
    metric: String,
    /// now (default) ⇒ most recent value; max_1h ⇒ trailing-hour peak.
    agg: Option<String>,
    /// Max nodes to return (1–50, default 5).
    limit: Option<usize>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct TopInterfacesParams {
    /// What to rank by: throughput | in_bps | out_bps | errors | discards | delta_up | delta_down.
    rank_by: String,
    /// now (default) | max_1h. Applies to the rate kinds only.
    agg: Option<String>,
    /// Comparison window in seconds (60–3600, default 300). Applies to delta_up/delta_down only.
    window_secs: Option<u64>,
    /// Max interfaces to return (1–50, default 6).
    limit: Option<usize>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct FleetThroughputParams {
    /// Window start, Unix seconds (default: 24 hours ago).
    from: Option<i64>,
    /// Window end, Unix seconds (default: now).
    to: Option<i64>,
    /// Sample step in seconds (clamped, minimum 60; default 300).
    step: Option<u64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct NeighborsParams {
    /// The node's UUID.
    node_id: Uuid,
    /// Max recent adjacency changes to include (1–200, default 10).
    history_limit: Option<i64>,
    /// Keyset cursor: the `at` of the last change you saw (RFC 3339). Pair with `before_id`.
    before_at: Option<String>,
    /// Keyset cursor: the `id` of the last change you saw. Pair with `before_at`.
    before_id: Option<i64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct DiscoveredEndpointsParams {
    /// Max endpoints to return (1–500, default 100).
    limit: Option<i64>,
    /// Only endpoints seen by this monitored node (its UUID).
    via_node: Option<Uuid>,
    /// Also include endpoints that have since become monitored nodes (default false).
    include_promoted: Option<bool>,
    /// Keyset cursor: the `last_seen` of the last row you saw (RFC 3339). Pair with `before_id`.
    before_last_seen: Option<String>,
    /// Keyset cursor: the `id` of the last row you saw. Pair with `before_last_seen`.
    before_id: Option<Uuid>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(crate) struct TopologyParams {
    /// Which view: "dependency" (default), "links", "overrides" or "shadow".
    kind: Option<String>,
    /// Keyset cursor. For kind=dependency this is a node UUID; for kind=links it is the numeric
    /// `next_cursor` from the previous page.
    after: Option<String>,
    /// Max rows to return (1–1000, default 200).
    limit: Option<i64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct TopFlowsParams {
    /// The exporter node's UUID. Omit for the whole fleet (top_flows only; flow_fanout requires it).
    node_id: Option<Uuid>,
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
pub(crate) struct AnalysisJobIdParams {
    /// The analysis job's UUID (from run_analysis or list_analyses).
    job_id: Uuid,
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
struct ListAnalysesParams {
    /// What to list: runs (default) | schedules.
    kind: Option<String>,
    /// Max jobs to return (1–100, default 20). Applies to `runs` only.
    limit: Option<i64>,
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
struct ListNodeGroupsParams {
    /// Also return each folder's direct-member state tally (costs a second query).
    include_state: Option<bool>,
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
struct AlertTrendsParams {
    /// Which view: top_nodes | transitions | calendar.
    kind: String,
    /// Trailing window in seconds for top_nodes (60–2592000, default 86400).
    window_secs: Option<i64>,
    /// Days of history for calendar (1–90, default 7).
    days: Option<i64>,
    /// Row count: top_nodes 1–50 (default 6), transitions default 12.
    limit: Option<i64>,
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
struct SearchFindingsParams {
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

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(crate) struct EventSearchParams {
    /// Case-insensitive match over source/message: whole words on a log-store deployment, any
    /// substring otherwise. Use `regex` for a message-only pattern that reaches inside words.
    search: Option<String>,
    /// Interpret `search` as a regular expression (message-only) rather than a plain term.
    regex: Option<bool>,
    /// Restrict to one or more event kinds, comma-separated: syslog, trap, webhook.
    kind: Option<String>,
    /// Restrict to one or more rule outcomes, comma-separated: none, info, suppressed, cleared,
    /// refreshed, fired. `fired,refreshed,cleared,suppressed` is what `matched: true` means.
    action: Option<String>,
    /// Restrict to one or more syslog severities (0–7), comma-separated. Traps and webhooks carry
    /// no severity and are excluded whenever this is set.
    severity: Option<String>,
    /// Restrict to one node's events (UUID).
    node_id: Option<Uuid>,
    /// Only events that matched an event rule (raised/cleared an alert).
    matched: Option<bool>,
    /// Condition on the message alone, with the same word rules as `search`.
    msg: Option<String>,
    /// Interpret `msg` as a regular expression, which reaches inside words on either store.
    msg_regex: Option<bool>,
    /// Return the events whose message does **not** match `msg`.
    msg_not: Option<bool>,
    /// Condition on the event's source: its IP, or the name of the node it is attributed to.
    /// There is no regex form for this one.
    src: Option<String>,
    /// Return the events whose source does **not** match `src`.
    src_not: Option<bool>,
    /// Time-range lower bound, RFC 3339.
    since: Option<String>,
    /// Time-range upper bound, RFC 3339.
    until: Option<String>,
    /// Max events to return (1–500, default 100).
    limit: Option<i64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(crate) struct EventStatsParams {
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

/// Which graph `get_topology` was asked for.
///
/// Split out of the tool body so the folding decision is testable without a `RequestContext` — the
/// `*_in` shape `api-conventions.md` asks for. `Unknown` is deliberately distinct from the default:
/// a caller who typed `kind="Links"` should be told, not silently handed the dependency graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TopologyKind {
    Dependency,
    Links,
    Overrides,
    Shadow,
    Unknown,
}

fn topology_kind(kind: Option<&str>) -> TopologyKind {
    match kind {
        None | Some("dependency") => TopologyKind::Dependency,
        Some("links") => TopologyKind::Links,
        Some("overrides") => TopologyKind::Overrides,
        Some("shadow") => TopologyKind::Shadow,
        Some(_) => TopologyKind::Unknown,
    }
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

/// Build a [`FlowQuery`] from the shared flow-tool params: typed drill-down filters (an unparseable
/// `peer` is ignored, never interpolated). Shared by `top_flows` and `flow_fanout`.
///
/// The window and the row limit come from [`crate::api::flow::flow_window`], which is the REST
/// edge's rule. This used to be a hand copy with `limit.unwrap_or(100)` and **no clamp**, while the
/// REST side clamped to `1..=1000` and its own test calls an unbounded top-N a DoS vector — the
/// surface with no human in the loop was the one without the cap. The default of 100 is kept: a
/// model orienting itself wants more rows than a dashboard table does, and that difference is a
/// choice rather than a drift.
fn flow_query_from(p: &TopFlowsParams) -> FlowQuery {
    let (from_unix_ms, to_unix_ms, limit) =
        crate::api::flow::flow_window(p.from, p.to, p.limit, 100);
    let peer: Option<std::net::IpAddr> = p.peer.as_deref().and_then(|s| s.parse().ok());
    FlowQuery {
        node_id: p.node_id,
        from_unix_ms,
        to_unix_ms,
        limit,
        proto: p.proto,
        dst_port: p.port,
        peer,
        asn: p.asn,
    }
}

/// The authenticated caller `mcp_auth_mw` inserted into the request extensions (WS-D). rmcp forwards
/// the HTTP request `Parts` into the tool's `RequestContext`, so the identity is read back from
/// `parts.extensions`.
fn identity_of(ctx: &RequestContext<RoleServer>) -> Option<McpIdentity> {
    ctx.extensions
        .get::<axum::http::request::Parts>()
        .and_then(|parts| parts.extensions.get::<McpIdentity>())
        .cloned()
}

/// [`identity_of`], but only if it holds `perm`. Fail-closed: `None` ⇒ the tool returns forbidden.
fn authed_for(ctx: &RequestContext<RoleServer>, perm: Permission) -> Option<McpIdentity> {
    identity_of(ctx).filter(|id| id.principal.can(perm))
}

/// The early return for a tool that names a node the caller may not see, or `None` to continue.
///
/// One helper rather than the check written out per tool: six tools take a `node_id`, and a missing
/// check on any one of them is a silent leak with nothing to catch it. It answers exactly what a
/// nonexistent id answers — a distinct refusal would confirm the node exists.
fn deny_invisible_node(
    st: &ApiState,
    scope: &NodeScope,
    tool: &str,
    node: Uuid,
) -> Option<Result<CallToolResult, McpError>> {
    if scope.allows_node(st, NodeId::from(node)) {
        None
    } else {
        Some(tool_unavailable(tool, "no node with that id"))
    }
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
    // An LLM wrote this argument, so normalize the shape before matching — unlike the REST edge,
    // where the value came from a form and "Critical" is a client bug worth surfacing.
    Severity::from_token(s.trim().to_ascii_lowercase().as_str())
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

    /// Every filter dimension a shared REST/MCP seam declares is actually passed by the tool.
    ///
    /// **This is the half the compiler cannot see.** A seam struct like
    /// `api::alerts::HistoryFilterInput` makes a *new* field a compile error here — the initializer
    /// below will not build without it — but the compiler is satisfied by `acked: None`. A
    /// dimension that is declared and then hardcoded away is the same silent failure as one that
    /// was never declared, and it is the failure ADR-042 read parity exists to prevent: the WebUI
    /// can ask the question and `/mcp` quietly cannot.
    ///
    /// One table rather than one test per seam. There were four seams by ADR-053 Inc.4b and the
    /// per-seam version had already been written twice; a fifth would have been written a third
    /// time, or not at all.
    ///
    /// The two escape hatches each need a stated reason, so an exception is a decision rather than
    /// the path of least resistance:
    /// - `hardcoded` — the tool deliberately does not offer the dimension.
    /// - `renamed` — the tool's parameter has a different name from the seam's field.
    #[test]
    fn every_shared_filter_seam_is_passed_through_whole() {
        const TOOLS: &str = include_str!("tools.rs");
        const ALERTS: &str = include_str!("../api/alerts.rs");
        const AUDIT: &str = include_str!("../api/audit.rs");
        const ANALYSIS: &str = include_str!("../api/analysis.rs");
        const EVENTLOG: &str = include_str!("../api/eventlog.rs");

        struct Seam {
            src: &'static str,
            decl: &'static str,
            init: &'static str,
            /// `(field, why the tool does not take it)`
            hardcoded: &'static [(&'static str, &'static str)],
            /// `(seam field, the tool's parameter name)`
            renamed: &'static [(&'static str, &'static str)],
        }
        let seams = [
            Seam {
                src: ALERTS,
                decl: "pub(crate) struct HistoryFilterInput<'a> {",
                init: "crate::api::alerts::HistoryFilterInput {",
                hardcoded: &[],
                renamed: &[],
            },
            Seam {
                src: AUDIT,
                decl: "pub(crate) struct AuditFilterInput<'a> {",
                init: "crate::api::audit::AuditFilterInput {",
                hardcoded: &[],
                renamed: &[],
            },
            Seam {
                src: ANALYSIS,
                decl: "pub(crate) struct SavedFindingsQuery {",
                init: "crate::api::analysis::SavedFindingsQuery {",
                hardcoded: &[],
                renamed: &[],
            },
            Seam {
                src: EVENTLOG,
                decl: "pub(crate) struct EventFilterInput<'a> {",
                init: "crate::api::eventlog::EventFilterInput {",
                // `search_events` pages by `before_ts`/`before_id` through its own cursor rather
                // than the log list's single `before`, so there is no parameter to pass here.
                hardcoded: &[("before", "the tool carries its own two-part cursor")],
                // The tool names the whole-row term `search` (it is not a column), and bounds the
                // window with `since`/`until` like every other tool rather than `start`/`end`.
                renamed: &[("start", "since"), ("end", "until"), ("q", "search")],
            },
        ];

        let mut checked = 0;
        for seam in seams {
            let body = seam
                .src
                .split(seam.decl)
                .nth(1)
                .unwrap_or_else(|| panic!("seam struct not found: {}", seam.decl));
            let fields: Vec<&str> = body
                .lines()
                .take_while(|l| !l.starts_with('}'))
                .map(str::trim)
                .filter(|l| !l.starts_with("///") && !l.starts_with("//") && !l.starts_with('#'))
                .filter_map(|l| {
                    l.strip_prefix("pub(crate) ")
                        .or_else(|| l.strip_prefix("pub "))
                })
                .filter_map(|l| l.split(':').next())
                .filter(|f| !f.is_empty())
                .collect();
            assert!(
                fields.len() >= 5,
                "field extraction produced {} fields for {} — the struct shape changed and this \
                 test is now checking almost nothing",
                fields.len(),
                seam.decl
            );

            let call = TOOLS
                .split(seam.init)
                .nth(1)
                .unwrap_or_else(|| panic!("no tool builds {}", seam.init));
            let call: Vec<&str> = call
                .lines()
                .map(str::trim)
                .take_while(|l| !l.starts_with('}'))
                .collect();

            for field in fields {
                let line = call
                    .iter()
                    .find(|l| l.starts_with(&format!("{field}:")))
                    .unwrap_or_else(|| panic!("{} does not pass {field}", seam.init));
                if let Some((_, why)) = seam.hardcoded.iter().find(|(f, _)| *f == field) {
                    assert!(
                        !line.contains("p."),
                        "{field} is listed as hardcoded ({why}) but now reads a parameter — \
                         remove the exception: {line}"
                    );
                    continue;
                }
                let param = seam
                    .renamed
                    .iter()
                    .find(|(f, _)| *f == field)
                    .map_or(field, |(_, p)| *p);
                assert!(
                    line.contains(&format!("p.{param}")),
                    "{} passes {field} from something other than `p.{param}`: {line}",
                    seam.init
                );
                checked += 1;
            }
        }
        // The assertion that stops "the parser stopped matching" from masquerading as "everything
        // is fine" — the same load-bearing shape `every_documented_body_is_the_type_its_handler_returns`
        // uses.
        assert!(checked >= 40, "only {checked} dimensions were compared");
    }

    /// The fold's dispatch, tested without a `RequestContext`. Two properties matter: the default
    /// is the graph that existed before the fold (so an existing client's call is unchanged), and a
    /// misspelled kind is an error rather than a silent fallback to that default.
    #[test]
    fn the_topology_kind_defaults_to_dependency_and_rejects_a_typo() {
        assert_eq!(topology_kind(None), TopologyKind::Dependency);
        assert_eq!(topology_kind(Some("dependency")), TopologyKind::Dependency);
        assert_eq!(topology_kind(Some("links")), TopologyKind::Links);
        for bad in ["Links", "link", "LINKS", "", "edges"] {
            assert_eq!(
                topology_kind(Some(bad)),
                TopologyKind::Unknown,
                "{bad} must be refused, not silently treated as the default"
            );
        }
    }

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
            ldap: None,
            oidc: None,
            oidc_flight: Arc::new(crate::oidc::OidcFlight::new()),
            enable_mcp: true,
            rca: None,
            webtls: None,
            upgrade: None,
            metrics: None,
            started: std::time::SystemTime::now(),
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

    // ── The WS-F guard ──────────────────────────────────────────────────────────────────────────

    #[test]
    fn every_tool_takes_a_request_context() {
        // `/mcp` admits group-scoped principals now, so a tool that does not consult the caller
        // returns the whole fleet — silently, with no compile error and no failing assertion
        // anywhere. A tool body can only reach the caller through its `RequestContext`, so taking
        // one is the observable marker that the question was asked. This does not prove the answer
        // is *used* correctly; it proves nobody added a tool that cannot ask.
        let src = include_str!("tools.rs");
        // Assembled at runtime — this test reads its own file, so literal needles would match
        // themselves and pass forever.
        let attr = format!("#[{}(", "tool");
        let ctx_param = format!("{}: RequestContext<RoleServer>", "ctx");
        let mut checked = 0;
        for (idx, _) in src.match_indices(&attr) {
            let rest = &src[idx..];
            // The tool's signature runs from its attribute to the opening brace of its body.
            let body_at = rest
                .find(") -> Result<CallToolResult")
                .unwrap_or(rest.len());
            let signature = &rest[..body_at];
            let name = signature
                .find("async fn ")
                .map(|i| signature[i + 9..].split('(').next().unwrap_or("?"))
                .unwrap_or("?");
            assert!(
                signature.contains(&ctx_param),
                "MCP tool `{name}` does not take a RequestContext, so it cannot resolve the \
                 caller's group scope and will answer fleet-wide to a scoped token"
            );
            checked += 1;
        }
        // The load-bearing half: if the parse stops matching, "everything is fine" must not be the
        // answer. There were 17 tools when this was written and 23 after ADR-042 I1.
        assert!(
            checked >= 34,
            "only matched {checked} tools; parser drifted"
        );
    }

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
            node_id: Some(node_id),
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

    /// The row limit is clamped on **this** surface too.
    ///
    /// It was not: this tool had its own copy of the query builder reading `limit.unwrap_or(100)`
    /// with no clamp, while the REST edge clamped to `1..=1000` and `api-conventions.md` calls an
    /// unbounded top-N a DoS vector. The surface with no human in the loop was the one without the
    /// cap, which is the usual direction for a duplicated bound. Both now go through
    /// `api::flow::flow_window`.
    #[test]
    fn an_unbounded_row_limit_is_clamped_like_the_rest_edge_clamps_it() {
        let mut p = flow_params(Uuid::new_v4());
        p.limit = Some(100_000);
        assert_eq!(
            flow_query_from(&p).limit,
            1000,
            "clamped to the store's cap"
        );

        p.limit = Some(0);
        assert_eq!(flow_query_from(&p).limit, 1, "and to at least one row");

        // The default differs from REST's 20 on purpose — a model orienting itself wants more rows
        // than a dashboard table does. That difference is a choice; the missing clamp was not.
        p.limit = None;
        assert_eq!(flow_query_from(&p).limit, 100);
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

    /// The scope an unrestricted caller resolves to. The `#[tool]` wrappers are unreachable from a
    /// test — rmcp's `RequestContext` needs a live `Peer`, whose constructor is crate-private — so
    /// the tests drive the `*_in` bodies the wrappers delegate to. That split is what makes the
    /// scoped behaviour testable at all; before it, the only reachable entry point required a
    /// running MCP session.
    fn unrestricted() -> NodeScope {
        NodeScope::All
    }

    /// A scope naming a group that does not exist — i.e. one that can see nothing. The shape a
    /// scoped caller has in skeleton mode, where there is no group store to expand.
    fn sees_nothing() -> NodeScope {
        NodeScope::Groups(Arc::new(crate::api::scope::ScopeSet {
            visible: vec![Uuid::from_u128(1)],
            breadcrumb: Vec::new(),
        }))
    }

    #[tokio::test]
    async fn fleet_summary_counts_never_observed_nodes_as_unknown() {
        let r = mcp().fleet_summary_in(&unrestricted()).await.expect("ok");
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
            .list_nodes_in(
                ListNodesParams {
                    search: None,
                    limit: Some(10_000),
                    ..Default::default()
                },
                &unrestricted(),
            )
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
            .list_nodes_in(
                ListNodesParams {
                    search: Some("demo".to_owned()),
                    ..Default::default()
                },
                &unrestricted(),
            )
            .await
            .expect("ok");
        assert_eq!(json_of(&hit).as_array().unwrap().len(), 1);

        let miss = m
            .list_nodes_in(
                ListNodesParams {
                    search: Some("no-such-node".to_owned()),
                    ..Default::default()
                },
                &unrestricted(),
            )
            .await
            .expect("ok");
        assert!(json_of(&miss).as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_scoped_caller_sees_neither_the_node_nor_its_name() {
        // The demo node belongs to no group, so it is outside *every* group scope — the same rule
        // `rbac.rs` applies to an ungrouped node, reproduced at this surface.
        let m = mcp();
        let scoped = sees_nothing();

        let listed = m
            .list_nodes_in(
                ListNodesParams {
                    search: None,
                    limit: None,
                    ..Default::default()
                },
                &scoped,
            )
            .await
            .expect("ok");
        assert!(
            json_of(&listed).as_array().unwrap().is_empty(),
            "a scoped caller must not see an ungrouped node"
        );

        // …and naming it directly answers what a nonexistent id answers, not a distinct refusal:
        // "you may not see this node" would confirm the node exists.
        let status = m
            .node_status_in(
                NodeIdParams {
                    // The demo inventory's one node (`repo::DEMO_NODE_ID`).
                    node_id: Uuid::nil(),
                },
                &scoped,
            )
            .await
            .expect("ok result");
        let body = json_of(&status);
        assert_eq!(body["available"], serde_json::json!(false));
        assert_eq!(
            body["reason"], "no node with that id",
            "an out-of-scope node and an unknown id must be indistinguishable"
        );
    }

    #[tokio::test]
    async fn active_alerts_is_empty_on_a_quiet_fleet() {
        let r = mcp()
            .active_alerts_in(
                ActiveAlertsParams {
                    node_id: None,
                    min_severity: None,
                    limit: None,
                },
                &unrestricted(),
            )
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
            .top_flows_in(flow_params(Uuid::new_v4()), &unrestricted())
            .await
            .expect("ok result");
        assert_eq!(json_of(&flows)["available"], serde_json::json!(false));

        let status = m
            .node_status_in(
                NodeIdParams {
                    node_id: Uuid::new_v4(),
                },
                &unrestricted(),
            )
            .await
            .expect("ok result");
        assert_eq!(json_of(&status)["available"], serde_json::json!(false));

        let neighbors = m
            .neighbors_in(neighbor_params(Uuid::nil()), &unrestricted())
            .await
            .expect("ok result");
        assert_eq!(json_of(&neighbors)["available"], serde_json::json!(false));

        let groups = m
            .list_node_groups_in(ListNodeGroupsParams::default(), &unrestricted())
            .await
            .expect("ok result");
        assert_eq!(json_of(&groups)["available"], serde_json::json!(false));

        let suppressions = m
            .list_suppressions_in(&unrestricted())
            .await
            .expect("ok result");
        assert_eq!(
            json_of(&suppressions)["available"],
            serde_json::json!(false)
        );
    }

    // ── ADR-042 I1 tools ────────────────────────────────────────────────────────────────────────

    fn neighbor_params(node_id: Uuid) -> NeighborsParams {
        NeighborsParams {
            node_id,
            history_limit: None,
            before_at: None,
            before_id: None,
        }
    }

    /// The alignment invariant is what can be silently wrong here: four series on one axis, so a
    /// chart — or a model — can read column `j` across all four without bounds-checking.
    #[tokio::test]
    async fn an_interface_series_returns_four_arrays_on_one_axis() {
        let r = mcp()
            .interface_series_in(
                InterfaceSeriesParams {
                    node_id: Uuid::nil(),
                    ifindex: 1,
                    from: None,
                    to: None,
                    step: None,
                },
                &unrestricted(),
            )
            .await
            .expect("ok");
        let body = json_of(&r);
        let n = body["timestamps"].as_array().expect("timestamps").len();
        for k in ["in_bps", "out_bps", "in_errors", "out_errors"] {
            assert_eq!(
                body[k].as_array().unwrap_or(&Vec::new()).len(),
                n,
                "{k} must be the same length as the shared timestamp axis"
            );
        }
    }

    #[tokio::test]
    async fn an_interface_series_hides_a_node_the_caller_cannot_see() {
        let r = mcp()
            .interface_series_in(
                InterfaceSeriesParams {
                    node_id: Uuid::nil(),
                    ifindex: 1,
                    from: None,
                    to: None,
                    step: None,
                },
                &sees_nothing(),
            )
            .await
            .expect("ok result");
        assert_eq!(json_of(&r)["reason"], "no node with that id");
    }

    /// A bad metric name must be a rejection, not an empty ranking.
    ///
    /// The name reaches a PromQL selector, so this is the injection boundary — now reachable from a
    /// second surface. An empty list would read to a model as "nothing is high", which is a claim
    /// about the fleet rather than about the request.
    #[tokio::test]
    async fn top_metrics_rejects_a_name_that_is_not_an_identifier() {
        let r = mcp()
            .top_metrics_in(
                TopMetricsParams {
                    metric: "up} or vector(1) #".to_owned(),
                    agg: None,
                    limit: None,
                },
                &unrestricted(),
            )
            .await;
        assert!(r.is_err(), "a junk metric name is a protocol error");
    }

    #[tokio::test]
    async fn top_metrics_accepts_the_logical_aliases_and_clamps_the_limit() {
        for metric in ["cpu", "memory", "icmp_rtt_ms"] {
            let r = mcp()
                .top_metrics_in(
                    TopMetricsParams {
                        metric: metric.to_owned(),
                        agg: None,
                        limit: Some(10_000),
                    },
                    &unrestricted(),
                )
                .await
                .expect("ok");
            // The in-memory store ranks nothing, so this asserts the shape rather than the rows —
            // the clamp itself is pinned in `api::metrics`, where it now lives for both surfaces.
            assert!(
                json_of(&r)["entries"].is_array(),
                "{metric} returns a ranking"
            );
        }
    }

    #[tokio::test]
    async fn top_interfaces_takes_one_vocabulary_and_rejects_the_rest() {
        let m = mcp();
        for rank_by in [
            "throughput",
            "in_bps",
            "out_bps",
            "errors",
            "discards",
            "delta_up",
            "delta_down",
        ] {
            let r = m
                .top_interfaces_in(
                    TopInterfacesParams {
                        rank_by: rank_by.to_owned(),
                        agg: None,
                        window_secs: None,
                        limit: None,
                    },
                    &unrestricted(),
                )
                .await
                .expect("ok");
            assert!(json_of(&r)["entries"].is_array(), "{rank_by} ranks");
        }
        // `cpu` is a *node* metric alias. Sending it here is the mistake a folded `kind` param would
        // have invited, and it must be a rejection the model can learn from.
        assert!(m
            .top_interfaces_in(
                TopInterfacesParams {
                    rank_by: "cpu".to_owned(),
                    agg: None,
                    window_secs: None,
                    limit: None,
                },
                &unrestricted(),
            )
            .await
            .is_err());
    }

    /// The fleet-wide forms mirror the REST refusal, and they refuse **before** consulting the tier.
    ///
    /// Reversed, the availability note would tell a scoped caller whether this deployment runs a
    /// flow tier — the same ordering property `api-conventions.md` states for the REST guards. This
    /// state has `flows: None`, so a tier-first implementation would answer `available: false` here
    /// instead of erroring, which is exactly what this pins.
    #[tokio::test]
    async fn the_fleet_wide_forms_refuse_a_scoped_caller_before_looking_at_the_tier() {
        let m = mcp();
        let mut fleet = flow_params(Uuid::nil());
        fleet.node_id = None;
        assert!(
            m.top_flows_in(fleet, &sees_nothing()).await.is_err(),
            "fleet-wide flow is refused, not reported unavailable"
        );
        assert!(
            m.fleet_throughput_in(
                FleetThroughputParams {
                    from: None,
                    to: None,
                    step: None
                },
                &sees_nothing()
            )
            .await
            .is_err(),
            "fleet throughput is refused, not reported unavailable"
        );
    }

    /// Fan-out has no fleet-wide form, and the refusal says why rather than just failing.
    #[tokio::test]
    async fn flow_fanout_requires_a_node() {
        let mut p = flow_params(Uuid::nil());
        p.node_id = None;
        assert!(mcp().flow_fanout_in(p, &unrestricted()).await.is_err());
    }

    /// A half-specified cursor is rejected, not ignored: dropping it restarts paging from the top,
    /// so a client walking the history loops over page one forever while looking like progress.
    ///
    /// Asserted against the shared parser rather than through the tool, because the tool checks
    /// availability before it parses — the same order the REST handler's extractors run in — so on
    /// this skeleton state the cursor is never reached. Testing it through the tool would have
    /// meant reordering the tool to suit the test, and that order is a disclosure property.
    #[test]
    fn a_half_specified_neighbour_cursor_is_rejected() {
        assert!(
            crate::api::neighbors::parse_history_cursor(Some("2026-08-03T00:00:00Z"), None)
                .is_err()
        );
        assert!(crate::api::neighbors::parse_history_cursor(None, Some(7)).is_err());
        assert!(crate::api::neighbors::parse_history_cursor(None, None)
            .expect("no cursor is fine")
            .is_none());
        assert!(
            crate::api::neighbors::parse_history_cursor(Some("2026-08-03T00:00:00Z"), Some(7))
                .expect("both halves")
                .is_some()
        );
    }

    /// Scope is checked **before** availability, so the error a scoped caller gets does not reveal
    /// whether the node exists on a live deployment.
    #[tokio::test]
    async fn neighbours_hide_an_invisible_node_rather_than_reporting_skeleton_mode() {
        let r = mcp()
            .neighbors_in(neighbor_params(Uuid::nil()), &sees_nothing())
            .await
            .expect("ok result");
        assert_eq!(
            json_of(&r)["reason"],
            "no node with that id",
            "the scope check runs before the live-mode check"
        );
    }

    // ── ADR-042 I2 tools ────────────────────────────────────────────────────────────────────────

    fn trend(kind: &str) -> AlertTrendsParams {
        AlertTrendsParams {
            kind: kind.to_owned(),
            ..Default::default()
        }
    }

    /// Each `kind` reaches a different store call, and a junk one is a protocol error rather than
    /// an empty list — a model that gets `[]` for a typo learns nothing and asks again.
    #[tokio::test]
    async fn alert_trends_takes_three_kinds_and_rejects_the_rest() {
        let m = mcp();
        for kind in ["top_nodes", "transitions", "calendar"] {
            let r = m
                .alert_trends_in(trend(kind), &unrestricted())
                .await
                .unwrap_or_else(|e| panic!("{kind} should answer, got {e:?}"));
            let body = json_of(&r);
            // Skeleton mode has no history store, so every branch answers its own empty shape:
            // a `Ranked` object for the ranking, a bare array for the other two.
            assert!(
                body.is_array() || body.get("entries").is_some(),
                "{kind} returned an unexpected shape: {body}"
            );
        }
        assert!(
            m.alert_trends_in(trend("cpu"), &unrestricted())
                .await
                .is_err(),
            "an unknown kind is a protocol error, not an empty result"
        );
    }

    /// The suppression view is one answer, not two calls: a model asking "is the fleet quiet"
    /// needs both halves or it reports health where there is silencing.
    #[tokio::test]
    async fn suppressions_carry_both_halves_in_one_result() {
        // Skeleton mode has no maintenance store, so this is the unavailable branch — what matters
        // is that the tool reports it once rather than half-answering.
        let r = mcp()
            .list_suppressions_in(&unrestricted())
            .await
            .expect("ok result");
        let body = json_of(&r);
        assert_eq!(body["available"], serde_json::json!(false));
        assert_eq!(body["reason"], "suppression state requires live mode");
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

    // ── ADR-042 I3a ─────────────────────────────────────────────────────────────────────────────

    /// Every section the description advertises is one the dispatcher accepts, and vice versa.
    ///
    /// The description is published verbatim, so a section named there but not parsed would have a
    /// model calling it and reasoning from the failure — the same class of harm as a wrong tool
    /// name, one level down.
    #[test]
    fn every_advertised_health_section_parses() {
        for name in HealthSection::NAMES {
            let parsed = HealthSection::parse(name)
                .unwrap_or_else(|| panic!("section {name} is advertised but not parsed"));
            assert_eq!(
                parsed.arg(),
                *name,
                "section {name} round-trips to a different key, so its permission would be looked \
                 up under the wrong row"
            );
        }
        assert_eq!(
            HealthSection::NAMES.len(),
            14,
            "the advertised section list changed; check the description and folded.rs together"
        );
    }

    /// A typo is refused, not silently resolved to a default. `get_system_health` has no sensible
    /// default section — every one answers a different question.
    /// A refusal must name the permission the way the descriptions do. The stored key is
    /// `manage_config`; every description on this surface says `manage-config`, and a model told
    /// one thing then shown another has to guess which is real.
    #[test]
    fn a_permission_is_named_the_way_the_descriptions_name_it() {
        assert_eq!(permission_label(Permission::ManageConfig), "manage-config");
        assert_eq!(permission_label(Permission::AckAlerts), "ack-alerts");
        assert_eq!(permission_label(Permission::ViewAudit), "view-audit");
        assert_eq!(permission_label(Permission::View), "view");
        // The spelling the existing hand-written refusals use, so the two families agree.
        assert!(
            include_str!("tools.rs").contains("lacks ack-alerts permission"),
            "the older refusals spell it hyphenated; this helper must match them"
        );
    }

    #[test]
    fn an_unknown_health_section_is_rejected() {
        assert!(
            HealthSection::parse("Pollers").is_none(),
            "parsing is exact"
        );
        assert!(HealthSection::parse("poller-nodes").is_none());
        assert!(HealthSection::parse("").is_none());
    }

    /// The two sections that need an id say so instead of answering about something else.
    #[tokio::test]
    async fn the_sections_that_need_an_id_refuse_without_one() {
        let m = mcp();
        for section in [HealthSection::PollerNodes, HealthSection::NodeAssignment] {
            let r = m
                .system_health_in(section, SystemHealthParams::default(), &unrestricted())
                .await;
            assert!(
                r.is_err(),
                "section {} answered without the id it needs",
                section.arg()
            );
        }
    }

    /// A section whose subsystem is absent reports that as an availability note, not a hard error —
    /// the model should say "this deployment has no write side" and move on.
    #[tokio::test]
    async fn skeleton_mode_reports_unavailable_rather_than_failing() {
        let r = mcp()
            .system_health_in(
                HealthSection::Pollers,
                SystemHealthParams::default(),
                &unrestricted(),
            )
            .await
            .expect("ok result");
        assert_eq!(json_of(&r)["available"], serde_json::json!(false));
    }

    /// The sections that read no store answer for real even in skeleton mode. `dependencies` in
    /// particular must: reporting that the database is unreachable is most useful when it is.
    #[tokio::test]
    async fn the_stateless_sections_answer_in_skeleton_mode() {
        let m = mcp();
        let deps = json_of(
            &m.system_health_in(
                HealthSection::Dependencies,
                SystemHealthParams::default(),
                &unrestricted(),
            )
            .await
            .expect("ok result"),
        );
        assert_eq!(
            deps["overall"], "degraded",
            "skeleton mode has no write side, and that is a fact to report rather than an error"
        );
        let version = json_of(
            &m.system_health_in(
                HealthSection::Version,
                SystemHealthParams::default(),
                &unrestricted(),
            )
            .await
            .expect("ok result"),
        );
        assert!(version["core"].is_string());
    }

    // ── get_config(kind=…) — ADR-042 I3b ─────────────────────────────────────

    /// Every kind the description advertises is one the dispatcher accepts, and round-trips to the
    /// key its permission is filed under. Same reasoning as the health-section version: the
    /// description ships verbatim, and a kind named there but not parsed has a model reasoning from
    /// a failure it cannot learn from.
    #[test]
    fn every_advertised_config_kind_parses() {
        for name in ConfigKind::NAMES {
            let parsed = ConfigKind::parse(name)
                .unwrap_or_else(|| panic!("kind {name} is advertised but not parsed"));
            assert_eq!(
                parsed.arg(),
                *name,
                "kind {name} round-trips to a different key, so its permission would be looked up \
                 under the wrong row"
            );
        }
        assert_eq!(
            ConfigKind::NAMES.len(),
            28,
            "the advertised kind list changed; check the description and folded.rs together"
        );
    }

    /// The biconditional the reachability guard cannot express.
    ///
    /// `folded::every_folded_branch_is_reachable_from_its_tool` is a bare substring search over this
    /// file, so a row whose `arg` happens to be a word already quoted somewhere here would pass with
    /// no arm at all — `schedules`, `credentials`, `forwarding`, `version`, `list` and `history` are
    /// all already present as literals. Comparing the two *sets* closes that class: a kind with no
    /// row loses its permission lookup (`required_permission` panics), and a row with no kind is a
    /// route the ledger claims is served and is not.
    #[test]
    fn every_config_kind_has_a_folded_row_and_vice_versa() {
        let rows: std::collections::BTreeSet<&str> = crate::mcp::folded::FOLDED_READS
            .iter()
            .filter(|f| f.tool == "get_config")
            .map(|f| f.arg)
            .collect();
        let kinds: std::collections::BTreeSet<&str> = ConfigKind::NAMES.iter().copied().collect();
        assert_eq!(
            rows, kinds,
            "the `get_config` folded rows and its advertised kinds disagree"
        );
    }

    /// A typo is refused, and there is no default to fall back to: unlike `get_topology`, no one
    /// kind is the obvious question here.
    #[test]
    fn an_unknown_config_kind_is_rejected() {
        assert!(ConfigKind::parse("url-check").is_none(), "parsing is exact");
        assert!(ConfigKind::parse("Thresholds").is_none());
        assert!(ConfigKind::parse("forwarding").is_none());
        assert!(ConfigKind::parse("schedules").is_none());
        assert!(ConfigKind::parse("").is_none());
    }

    /// The five kinds that need an id say so rather than answering about something else.
    #[tokio::test]
    async fn the_config_kinds_that_need_an_id_refuse_without_one() {
        let m = mcp();
        for kind in [
            ConfigKind::NodeCollection,
            ConfigKind::UrlCheck,
            ConfigKind::DnsCheck,
            ConfigKind::TemplateItems,
            ConfigKind::ProfileTemplates,
            ConfigKind::MerakiNetworks,
            ConfigKind::DiscoveryScan,
        ] {
            let r = m
                .config_in(kind, ConfigParams::default(), &unrestricted())
                .await;
            assert!(
                r.is_err(),
                "kind {} answered without the id it needs",
                kind.arg()
            );
        }
    }

    /// The three node-scoped kinds hide a node outside the caller's groups, and hide it the same
    /// way a nonexistent id is hidden — otherwise the tool is an existence oracle.
    #[tokio::test]
    async fn a_per_node_config_kind_hides_a_node_outside_the_scope() {
        for kind in [
            ConfigKind::NodeCollection,
            ConfigKind::UrlCheck,
            ConfigKind::DnsCheck,
        ] {
            let r = mcp()
                .config_in(
                    kind,
                    ConfigParams {
                        kind: kind.arg().to_owned(),
                        node_id: Some(Uuid::nil()),
                        ..Default::default()
                    },
                    &sees_nothing(),
                )
                .await
                .unwrap_or_else(|_| panic!("kind {} should answer, not error", kind.arg()));
            let body = json_of(&r);
            assert_eq!(
                body["available"],
                serde_json::json!(false),
                "{}",
                kind.arg()
            );
            assert_eq!(body["reason"], "no node with that id", "{}", kind.arg());
        }
    }

    /// A node the caller cannot see answers exactly what a nonexistent one answers.
    #[tokio::test]
    async fn node_assignment_hides_a_node_outside_the_scope() {
        let r = mcp()
            .system_health_in(
                HealthSection::NodeAssignment,
                SystemHealthParams {
                    section: "node_assignment".to_owned(),
                    node_id: Some(Uuid::nil()),
                    ..Default::default()
                },
                &sees_nothing(),
            )
            .await
            .expect("ok result");
        let body = json_of(&r);
        assert_eq!(body["available"], serde_json::json!(false));
        assert_eq!(body["reason"], "no node with that id");
    }

    /// `get_fleet_summary` gained a second kind. Omitting it must still mean the summary — this is
    /// the documented entry point and every existing caller passes nothing — and a typo must be
    /// refused rather than silently resolving to that same default.
    #[test]
    fn the_fleet_summary_kind_defaults_to_summary_and_rejects_a_typo() {
        assert_eq!(fleet_summary_kind(None), Some(FleetSummaryKind::Summary));
        assert_eq!(
            fleet_summary_kind(Some("summary")),
            Some(FleetSummaryKind::Summary)
        );
        assert_eq!(
            fleet_summary_kind(Some("coverage")),
            Some(FleetSummaryKind::Coverage)
        );
        assert_eq!(fleet_summary_kind(Some("Coverage")), None);
        assert_eq!(fleet_summary_kind(Some("")), None);
    }

    /// The DNS chain answers "nothing recorded" as availability, not as a fault: a monitor that has
    /// never resolved is a normal state to report.
    #[tokio::test]
    async fn the_dns_chain_reports_an_unmonitored_node_as_unavailable() {
        let r = mcp()
            .dns_chain_in(
                DnsChainParams {
                    node_id: Uuid::nil(),
                    history: None,
                    limit: None,
                    before_at: None,
                    before_id: None,
                },
                &unrestricted(),
            )
            .await
            .expect("ok result");
        assert_eq!(json_of(&r)["available"], serde_json::json!(false));
    }

    /// A half-specified keyset cursor is a protocol error on this surface as it is over REST.
    /// Dropping it would restart paging from the newest page, so a client walking the history would
    /// loop over page one forever while looking like it was progressing.
    #[test]
    fn a_half_specified_dns_cursor_is_rejected() {
        assert!(
            crate::api::checks::parse_history_cursor(Some("2026-08-04T00:00:00Z"), None).is_err()
        );
        assert!(crate::api::checks::parse_history_cursor(None, Some(7)).is_err());
        assert!(crate::api::checks::parse_history_cursor(None, None).is_ok());
        assert!(crate::api::checks::parse_history_cursor(Some("not-a-time"), Some(7)).is_err());
    }

    /// Without a write side the audit tool says so, rather than answering an empty log — "nobody
    /// has done anything" and "this deployment keeps no audit log" must not read alike to a model.
    ///
    /// The cursor rule itself is not reachable from here (it lives in `audit::audit_page`, behind
    /// the live store, and is covered on the REST side). Saying that is better than a test whose
    /// name claims more than it checks.
    #[tokio::test]
    async fn the_audit_tool_reports_a_missing_write_side_rather_than_an_empty_log() {
        let r = mcp()
            .audit_in(AuditParams {
                limit: None,
                before: Some("yesterday".to_owned()),
                since: None,
                until: None,
                q: None,
                action: None,
                status: None,
            })
            .await
            .expect("ok result");
        let body = json_of(&r);
        assert_eq!(body["available"], serde_json::json!(false));
        assert_eq!(body["reason"], "the audit log requires live mode");
    }
}
