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

use rmcp::model::CallToolResult;
// The module (not just the trait) — the `JsonSchema` derive expands to `schemars::…` paths, so the
// `schemars` name must be in scope. rmcp re-exports it, keeping exactly one schemars version.
use rmcp::service::{RequestContext, RoleServer};
use rmcp::ErrorData as McpError;
use serde_json::Value;
use std::collections::HashMap;
use std::time::Duration;
use uuid::Uuid;

use super::{McpIdentity, YagraMcp};
use crate::api::scope::NodeScope;
use crate::api::ApiError;

/// Default window (seconds) for range/rate metric and flow queries when `from`/`to` are omitted.
const DEFAULT_WINDOW_SECS: i64 = 3600;
/// How long `run_analysis` blocks polling for a job to finish before returning it still-running.
const ANALYSIS_MAX_WAIT: Duration = Duration::from_secs(120);
/// Poll interval while `run_analysis` waits for a job to reach a terminal state.
const ANALYSIS_POLL: Duration = Duration::from_millis(750);

mod alerts;
mod analysis;
mod events;
#[cfg(test)]
mod guards;
mod metrics;
mod nodes;
mod support;
mod system;
#[cfg(test)]
mod testkit;
mod topology;

// Glob imports so a sibling module can reach a type or helper that lives in another one —
// `ConfigParams` is declared beside `get_config` but `call_in` below builds one. Every domain file
// says `use super::*` and gets the whole surface's vocabulary from here. Enumerating the names
// would be a list to maintain, which is the thing ADR-086 exists to avoid.
//
// Private (not `pub(crate)`): nothing outside `mcp::tools` names one of these, and a child module
// can see an ancestor's private items, so this is the reach a sibling needs and nothing wider.
use self::{
    alerts::*, analysis::*, events::*, metrics::*, nodes::*, support::*, system::*, topology::*,
};

impl YagraMcp {
    /// Construct the handler over the shared API state, building the macro-generated tool router.
    pub(crate) fn new(state: crate::api::ApiState) -> Self {
        Self {
            state,
            // One router per domain module (ADR-086), merged in the order the tools were
            // declared in before the split — `tools/list` is not a documented contract, so the
            // order is kept rather than changed, there being no reason to change it.
            tool_router: Self::nodes_router()
                + Self::alerts_router()
                + Self::metrics_router()
                + Self::topology_router()
                + Self::events_router()
                + Self::analysis_router()
                + Self::system_router(),
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
                    Some(kind) => self.fleet_summary_dispatch(kind, scope).await,
                    None => bad_fleet_summary_kind(p.kind.as_deref()),
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
                    None => bad_health_section(&p.section),
                }
            }
            "get_config" => {
                let p = p!(ConfigParams);
                match ConfigKind::parse(&p.kind) {
                    Some(kind) => self.config_in(kind, p, scope).await,
                    None => bad_config_kind(&p.kind),
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
    pub(super) async fn scope_of(
        &self,
        ctx: &RequestContext<RoleServer>,
    ) -> Result<NodeScope, ApiError> {
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
    pub(super) async fn deny_invisible_node_ctx(
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
    pub(super) async fn resolve_names(
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
    pub(super) fn scoped_report_body(
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
    pub(super) fn deny_unless_permitted(
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
