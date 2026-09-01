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
//! [`YagraMcp::scope_for`] to resolve the caller's scope and then the same rule its REST counterpart
//! carries in `api/route_table.rs` — a `group_id = ANY(…)` predicate where the query is ours, a
//! post-filter where the store ranks or aggregates, and the id-shaped `no node with that id` where
//! the tool names one node (never a distinct refusal, which would confirm the node exists).
//!
//! The consequence worth stating plainly: a tool that forgets to ask now returns the fleet, silently.
//! A tool body cannot reach the caller except through its `RequestContext`, so `ctx` is the visible
//! marker that the question was asked — and `every_tool_takes_a_request_context` fails if a new one
//! does not take it.

use rmcp::model::CallToolResult;
// 🎯 This file names no rmcp *session* type any more (ADR-113). `RequestContext` — and therefore
// `Peer`, which no test can build — reaches only as far as the `#[tool]` wrappers and
// `support::identity_of`. The gates below take the identity that came out of one.
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
    ///
    /// 🎯 **Takes the identity, not the `RequestContext` it came out of** (ADR-113). A
    /// `RequestContext` needs an `rmcp::Peer`, whose constructor is crate-private in rmcp, so a
    /// gate that takes one is a gate no test can call — and this one had none, including over the
    /// fail-closed branch above. The `#[tool]` wrapper still takes `ctx` and reads the identity out
    /// of it with [`identity_of`]; that is the only place on this surface that touches an rmcp type.
    pub(super) async fn scope_for(&self, id: Option<McpIdentity>) -> Result<NodeScope, ApiError> {
        let principal = id.map(|id| id.principal).unwrap_or_else(|| {
            tracing::error!("MCP tool ran with no authenticated identity; treating it as empty");
            yagra_common::Principal::new(
                yagra_common::Role::Viewer,
                yagra_common::Scope::Groups(std::collections::BTreeSet::new()),
            )
        });
        crate::api::scope::resolve(&self.state, &principal).await
    }

    /// [`deny_invisible_node`] for a tool that never took the scope as a parameter (the write
    /// tools, which need the identity anyway).
    pub(super) async fn deny_invisible_node_for(
        &self,
        id: Option<McpIdentity>,
        tool: &str,
        node: Uuid,
    ) -> Option<Result<CallToolResult, McpError>> {
        match self.scope_for(id).await {
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
        id: Option<&McpIdentity>,
        tool: &'static str,
        arg: &str,
    ) -> Option<Result<CallToolResult, McpError>> {
        let want = crate::mcp::folded::required_permission(tool, arg);
        if id.is_some_and(|id| id.principal.can(want)) {
            return None;
        }
        Some(tool_forbidden(
            tool,
            &format!("this token lacks {} permission", permission_label(want)),
        ))
    }

    /// Authorize, **then** resolve the scope — the prelude every folded read runs (ADR-113).
    ///
    /// The order is a security property, not a preference: the permission check sits above every
    /// store lookup so a caller who may not read a branch cannot infer, from a 403-vs-unavailable,
    /// whether this deployment has that subsystem configured at all. It was written out in eight
    /// tool wrappers, each declaring that rule in a comment and none of them holding anything to
    /// it — so a ninth written the other way round would have shipped green.
    ///
    /// `Err` carries the refusal the tool should return verbatim. The nested `Result` reads oddly
    /// once and matches [`Self::deny_unless_permitted`]'s `Option<Result<…>>`, which is how every
    /// early return on this surface is already spelled.
    ///
    /// ⚠️ `get_audit` is the one folded read that does **not** come through here: it answers from
    /// the audit log, which carries no node dimension, so it has no scope to resolve. It calls
    /// [`Self::deny_unless_permitted`] directly.
    pub(super) async fn admit(
        &self,
        id: Option<McpIdentity>,
        tool: &'static str,
        arg: &str,
    ) -> Result<NodeScope, Result<CallToolResult, McpError>> {
        if let Some(denied) = self.deny_unless_permitted(id.as_ref(), tool, arg) {
            return Err(denied);
        }
        self.scope_for(id)
            .await
            .map_err(|e| tool_api_error(tool, &e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::tools::testkit::*;
    use std::collections::BTreeSet;
    use yagra_common::{Principal, Role, Scope};

    /// An authenticated caller of `role` seeing the whole fleet.
    fn unrestricted_id(role: Role) -> McpIdentity {
        McpIdentity {
            principal: Principal::new(role, Scope::All),
            actor: "test".to_owned(),
        }
    }

    /// An authenticated caller of `role` restricted to one group.
    fn scoped_id(role: Role, group: Uuid) -> McpIdentity {
        McpIdentity {
            principal: Principal::new(role, Scope::Groups(BTreeSet::from([group.to_string()]))),
            actor: "test".to_owned(),
        }
    }

    // ── The scope gate ──────────────────────────────────────────────────────────────────────────

    /// 🚨 **The branch the whole surface's safety rests on, and the reason this module now has
    /// tests at all.** `mcp_auth_mw` inserts an identity into every request it admits, so this is
    /// unreachable in production — which is exactly why it must resolve to *sees nothing*. Nobody
    /// notices an unreachable branch becoming reachable, and the wrong answer here is the whole
    /// fleet.
    #[tokio::test]
    async fn a_caller_with_no_identity_sees_nothing() {
        let scope = mcp().scope_for(None).await.expect("resolves");
        match scope {
            NodeScope::All => panic!("a missing identity must not resolve to the whole fleet"),
            NodeScope::Groups(s) => assert!(
                s.visible.is_empty(),
                "an absent identity must see no group at all, saw {:?}",
                s.visible
            ),
        }
    }

    /// The paired accept side. Without it, "sees nothing" is also satisfied by a `scope_for` that
    /// refuses everyone (`rejection-only-tests-pass-when-everything-rejects`).
    #[tokio::test]
    async fn an_unrestricted_identity_sees_the_whole_fleet() {
        let scope = mcp()
            .scope_for(Some(unrestricted_id(Role::Admin)))
            .await
            .expect("resolves");
        assert!(
            matches!(scope, NodeScope::All),
            "an unrestricted principal resolves to the whole fleet"
        );
    }

    /// A group-scoped principal keeps its group. In skeleton mode there is no group store to expand
    /// through, so the visible set is the named root itself — which is what makes the *shape*
    /// testable here without a database.
    #[tokio::test]
    async fn a_group_scoped_identity_keeps_its_group() {
        let g = Uuid::from_u128(7);
        let scope = mcp()
            .scope_for(Some(scoped_id(Role::Viewer, g)))
            .await
            .expect("resolves");
        match scope {
            NodeScope::All => panic!("a group-scoped principal must not widen to the fleet"),
            NodeScope::Groups(s) => assert_eq!(s.visible, vec![g]),
        }
    }

    // ── The permission gate ─────────────────────────────────────────────────────────────────────

    /// The permission comes from `folded::FOLDED_READS`, not from a literal — `get_system_health`
    /// alone spans `View`, `ManageSystem` and `ManageCredentials`, so a Viewer reading the fleet's
    /// poller list is fine and a Viewer reading credential health is not.
    #[test]
    fn a_viewer_is_refused_a_branch_that_needs_more_than_view() {
        let m = mcp();
        let viewer = unrestricted_id(Role::Viewer);
        assert!(
            m.deny_unless_permitted(Some(&viewer), "get_system_health", "credentials")
                .is_some(),
            "credential health needs manage-credentials"
        );
        // The accept side, on the same tool, so the refusal above is about the *branch* and not
        // about the gate refusing everything it is shown.
        assert!(
            m.deny_unless_permitted(Some(&viewer), "get_system_health", "pollers")
                .is_none(),
            "the poller list needs only view"
        );
    }

    #[test]
    fn an_admin_passes_the_branch_a_viewer_is_refused() {
        let admin = unrestricted_id(Role::Admin);
        assert!(
            mcp()
                .deny_unless_permitted(Some(&admin), "get_system_health", "credentials")
                .is_none(),
            "an admin holds manage-credentials"
        );
    }

    /// Fail-closed, the same rule as [`a_caller_with_no_identity_sees_nothing`] on the other gate:
    /// no identity is refused even where the branch asks for nothing more than `View`.
    #[test]
    fn no_identity_is_refused_even_on_a_view_only_branch() {
        assert!(
            mcp()
                .deny_unless_permitted(None, "get_system_health", "pollers")
                .is_some(),
            "an absent identity must not pass a permission check"
        );
    }

    /// 🚨 **`required_permission` panics on a `(tool, arg)` the table does not hold**, and it is
    /// called from inside a live request. `folded.rs` checks the other direction — that every row
    /// has an arm — so nothing until now walked the arguments a wrapper can actually pass.
    ///
    /// The literals are the wrappers that pass a fixed string or a computed one; the two
    /// enumerations are the folded discriminators, taken from the same `NAMES` list `parse` reads,
    /// so a section added there is checked here without being added here.
    #[test]
    fn every_argument_a_wrapper_can_pass_is_in_the_folded_table() {
        let mut args: Vec<(&str, String)> = vec![
            ("get_interface_thresholds", String::new()),
            ("list_node_metrics", String::new()),
            ("fleet_state_history", String::new()),
            ("get_audit", String::new()),
            ("get_dns_chain", "current".to_owned()),
            ("get_dns_chain", "history".to_owned()),
            ("get_report_runs", "list".to_owned()),
            ("get_report_runs", "detail".to_owned()),
        ];
        for name in HealthSection::NAMES {
            let s = HealthSection::parse(name).expect("NAMES parses");
            args.push(("get_system_health", s.arg().to_owned()));
        }
        for name in ConfigKind::NAMES {
            let k = ConfigKind::parse(name).expect("NAMES parses");
            args.push(("get_config", k.arg().to_owned()));
        }
        // The floor counts what was checked, not what was listed: a `NAMES` that stopped parsing
        // would otherwise leave this passing over the eight literals alone.
        assert!(
            args.len() >= 40,
            "only {} folded arguments were walked; the discriminator lists have shrunk",
            args.len()
        );
        for (tool, arg) in &args {
            // Panics if the pair has no row — which is the failure this test exists for.
            let _ = crate::mcp::folded::required_permission(tool, arg);
        }
    }

    // ── The two gates in order ──────────────────────────────────────────────────────────────────

    /// A caller who may not read the branch gets the refusal, not a scope — so no store is ever
    /// consulted on their behalf. ⚠️ The *ordering* itself cannot be observed from the return value
    /// (in skeleton mode resolving a scope cannot fail), so this pins the outcome and the ordering
    /// is proved by breaking it: swapping the two statements in `admit` turns this red.
    #[tokio::test]
    async fn admit_refuses_before_it_resolves_a_scope() {
        let viewer = scoped_id(Role::Viewer, Uuid::from_u128(7));
        let refusal = mcp()
            .admit(Some(viewer), "get_system_health", "credentials")
            .await
            .expect_err("a viewer may not read credential health");
        let err = refusal.expect_err("a refusal is a protocol error");
        assert!(
            err.message.contains("manage-credentials"),
            "the refusal names the permission the caller lacks: {}",
            err.message
        );
    }

    #[tokio::test]
    async fn admit_hands_back_the_scope_when_the_permission_holds() {
        let scope = mcp()
            .admit(
                Some(unrestricted_id(Role::Admin)),
                "get_system_health",
                "credentials",
            )
            .await
            .map_err(|_| "refused")
            .expect("an admin is admitted");
        assert!(matches!(scope, NodeScope::All));
    }

    // ── The invisible-node gate ─────────────────────────────────────────────────────────────────

    /// 🚨 The refusal must not vary with the id it was asked about, and must not echo it. A
    /// distinct answer for a node that exists would confirm the node exists, which is the leak the
    /// whole scoping design is for — the tool says the same thing about every id it will not serve.
    #[tokio::test]
    async fn the_refusal_for_an_unseeable_node_never_varies_with_the_id() {
        let m = mcp();
        let scoped = scoped_id(Role::Admin, Uuid::from_u128(7));
        let real = m
            .deny_invisible_node_for(Some(scoped.clone()), "t", Uuid::from_u128(1))
            .await
            .expect("a scoped caller sees no node in skeleton mode")
            .expect("the refusal is a successful tool result");
        let ghost = m
            .deny_invisible_node_for(Some(scoped), "t", Uuid::from_u128(999))
            .await
            .expect("a nonexistent node is refused too")
            .expect("the refusal is a successful tool result");
        assert_eq!(
            text_of(&real),
            text_of(&ghost),
            "the two answers must be indistinguishable"
        );
        assert!(
            !text_of(&real).contains(&Uuid::from_u128(1).to_string()),
            "the refusal must not echo the id it was asked about"
        );
    }

    /// The accept side: an unrestricted caller is not denied, so the assertion above is about the
    /// scope and not about a gate that refuses everything.
    #[tokio::test]
    async fn a_visible_node_is_not_denied() {
        assert!(
            mcp()
                .deny_invisible_node_for(
                    Some(unrestricted_id(Role::Admin)),
                    "t",
                    Uuid::from_u128(1)
                )
                .await
                .is_none(),
            "an unrestricted caller reaches the tool body"
        );
    }

    // ── The dispatcher ──────────────────────────────────────────────────────────────────────────

    /// Every published tool with no arm in [`YagraMcp::call_in`], and why.
    ///
    /// `rca/agent.rs::every_agent_tool_has_an_in_process_arm` walks the *allow-list*, so it proves
    /// nothing about a tool the agent is not offered — two arms (`get_report_runs`,
    /// `list_discovered_endpoints`) are outside it, and eight declared tools have no arm at all.
    /// Declaring the eight makes a ninth a test failure rather than a silent gap: a new **read**
    /// tool that lands with no arm is invisible to the RCA agent forever, and nothing else notices.
    ///
    /// ⚠️ The last three are reads and are **not** a decision anyone recorded — they were found by
    /// this test (ADR-113 decision 5). `list_node_metrics` in particular is the tool `INSTRUCTIONS`
    /// tells every client to call before `query_metrics`.
    const NOT_DISPATCHABLE: &[&str] = &[
        // Writes: the in-process caller is a language model, and MCP's write surface is frozen at
        // three tools it is deliberately not offered (ADR-042 decision 6).
        "ack_alert",
        "open_maintenance",
        "poll_now",
        // Starts work and spends money; the agent must not recurse into itself.
        "run_analysis",
        "run_rca",
        // ⚠️ Reads with no arm. Not yet decided — see the doc above.
        "get_audit",
        "get_interface_thresholds",
        "list_node_metrics",
    ];

    #[tokio::test]
    async fn every_published_tool_either_dispatches_or_is_declared_undispatchable() {
        const UNKNOWN: &str = "no in-process tool named";
        let m = mcp();
        let mut missing = Vec::new();
        let published = m.published_tools();
        for t in &published {
            let name = t.name.to_string();
            let out = m
                .call_in(&name, serde_json::json!({}), &NodeScope::All)
                .await;
            let refused_as_unknown = match &out {
                Err(e) => e.message.contains(UNKNOWN),
                Ok(_) => false,
            };
            if refused_as_unknown {
                missing.push(name);
            }
        }
        missing.sort();
        let mut declared: Vec<String> = NOT_DISPATCHABLE.iter().map(|s| (*s).to_owned()).collect();
        declared.sort();
        assert_eq!(
            missing, declared,
            "the set of published tools with no `call_in` arm has moved; add the arm, or add the \
             name to NOT_DISPATCHABLE with the reason"
        );
        assert!(
            published.len() >= 36,
            "only {} tools were published; the router lost a domain",
            published.len()
        );
    }

    /// The load-bearing half of the test above: prove the needle it searches for is what an unknown
    /// name actually produces. Without this, a dispatcher that stopped producing that message would
    /// make every tool look reachable.
    #[tokio::test]
    async fn a_name_the_dispatcher_does_not_know_is_refused_by_that_wording() {
        let err = mcp()
            .call_in("get_the_weather", serde_json::json!({}), &NodeScope::All)
            .await
            .expect_err("an unknown name is refused");
        assert!(
            err.message.contains("no in-process tool named"),
            "the needle is not what an unknown name produces: {}",
            err.message
        );
    }
}
