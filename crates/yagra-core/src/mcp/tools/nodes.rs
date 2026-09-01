// SPDX-License-Identifier: AGPL-3.0-only
//! MCP tools: the fleet and the nodes in it — what exists, what state it is in, and what it can be asked for (ADR-086).
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
use std::collections::HashMap;
use uuid::Uuid;
use yagra_common::{NodeId, NodeKind, Permission};

use super::YagraMcp;
use crate::api::scope::NodeScope;
use crate::mcp::dto::{
    AlertDto, FleetSummaryDto, InterfaceDto, NodeGroupDto, NodeStatusDto, NodeSummaryDto,
};

// The shared scope: the helpers in `support.rs` and the types the other domain modules declare,
// re-exported by `mod.rs` so no file has to name where a sibling keeps a thing.
use super::*;

/// Which fleet view `get_fleet_summary` was asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FleetSummaryKind {
    Summary,
    Coverage,
}

/// Resolve the `kind` argument, or `None` for one that names neither view.
///
/// Split out for the same reason as `topology_kind`: the `#[tool]` wrapper cannot be called from a
/// test, so the decision has to live somewhere a test can reach. Omitted means the summary — this
/// tool is the documented starting point and had no argument before I3a, so a caller that passes
/// nothing must keep getting what it always got.
pub(super) fn fleet_summary_kind(kind: Option<&str>) -> Option<FleetSummaryKind> {
    match kind {
        None | Some("summary") => Some(FleetSummaryKind::Summary),
        Some("coverage") => Some(FleetSummaryKind::Coverage),
        Some(_) => None,
    }
}

/// The refusal for a `kind` [`fleet_summary_kind`] does not serve (ADR-085 Inc.1).
///
/// **Written once because there are two doors into every tool body** — the `#[tool]` wrapper and
/// [`YagraMcp::call_in`], the in-process entry ADR-029's RCA agent uses — and these two had
/// already drifted: the wrapper named the value it rejected, `call_in` did not. This text is a
/// specification a model reasons from, so a caller that gets a *different* explanation depending on
/// which door it came through cannot learn the rule from either. The wrapper's wording is the one
/// kept: naming the bad value is what lets a model correct itself in one turn.
///
/// Its siblings [`bad_health_section`] and [`bad_config_kind`] exist for the same reason. Those
/// two had not drifted yet — they were two verbatim copies, which is the state a drift starts from.
pub(super) fn bad_fleet_summary_kind(kind: Option<&str>) -> Result<CallToolResult, McpError> {
    tool_bad_params(
        "get_fleet_summary",
        &format!(
            "unknown kind {:?}; must be summary or coverage",
            kind.unwrap_or_default()
        ),
    )
}

// ── Tool parameter structs (schemas derived for `tools/list`) ─────────────────────────────────────

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
pub(super) struct FleetSummaryParams {
    /// `summary` (default) for the state tally, or `coverage` for the monitoring blind-spot view.
    pub(super) kind: Option<String>,
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
pub(super) struct StateHistoryParams {
    /// Window start, Unix seconds (default 24h ago).
    from: Option<i64>,
    /// Window end, Unix seconds (default now). The window may not exceed 90 days.
    to: Option<i64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(super) struct DnsChainParams {
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

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
pub(super) struct ListNodesParams {
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
pub(super) struct NodeIdParams {
    /// The node's UUID.
    pub(super) node_id: Uuid,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(crate) struct NodeMetricsParams {
    /// The node's UUID.
    node_id: Uuid,
    /// How far back a metric may have last been seen and still count as having data, in seconds
    /// (default 6 hours; clamped).
    within_secs: Option<u64>,
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
pub(super) struct ListNodeGroupsParams {
    /// Also return each folder's direct-member state tally (costs a second query).
    include_state: Option<bool>,
}

#[tool_router(router = nodes_router, vis = "pub(super)")]
impl YagraMcp {
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
        const TOOL: &str = "get_fleet_summary";
        let Some(kind) = fleet_summary_kind(p.kind.as_deref()) else {
            return bad_fleet_summary_kind(p.kind.as_deref());
        };
        match self.scope_for(identity_of(&ctx)).await {
            Ok(scope) => self.fleet_summary_dispatch(kind, &scope).await,
            Err(e) => tool_api_error(TOOL, &e),
        }
    }

    /// Route a resolved [`FleetSummaryKind`] to the branch that answers it (ADR-085 Inc.1).
    ///
    /// One exhaustive match rather than one per door. There are two doors into every tool body —
    /// the `#[tool]` wrapper above and [`Self::call_in`], the in-process entry ADR-029's RCA agent
    /// uses — and this branch was written out at both. The wrapper's copy ended in a catch-all
    /// (`Ok(scope) =>`), so a third kind would have been served the summary there while
    /// `call_in`'s exhaustive copy refused to compile: the compiler would have caught half of it,
    /// which is the worst of the two outcomes because the half it catches looks like the whole.
    pub(super) async fn fleet_summary_dispatch(
        &self,
        kind: FleetSummaryKind,
        scope: &NodeScope,
    ) -> Result<CallToolResult, McpError> {
        match kind {
            FleetSummaryKind::Summary => self.fleet_summary_in(scope).await,
            FleetSummaryKind::Coverage => self.fleet_coverage_in(scope).await,
        }
    }

    /// The coverage branch: folded in rather than given its own tool because "is the fleet
    /// healthy?" and "is the fleet actually being *watched*?" are the same question asked twice —
    /// a model that starts here should not have to know a second tool name to find out that a
    /// third of the answer is stale.
    pub(super) async fn fleet_coverage_in(
        &self,
        scope: &NodeScope,
    ) -> Result<CallToolResult, McpError> {
        const TOOL: &str = "get_fleet_summary";
        let Some(admin) = self.state.admin.as_ref() else {
            return tool_unavailable(TOOL, "coverage requires live mode");
        };
        match crate::api::fleet::coverage(&self.state, admin, scope).await {
            Ok(c) => ok_json(TOOL, &c),
            Err(e) => tool_api_error(TOOL, &e),
        }
    }

    pub(super) async fn fleet_summary_in(
        &self,
        scope: &NodeScope,
    ) -> Result<CallToolResult, McpError> {
        const TOOL: &str = "get_fleet_summary";
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
        ok_json(TOOL, &dto)
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
        const TOOL: &str = "list_nodes";
        match self.scope_for(identity_of(&ctx)).await {
            Ok(scope) => self.list_nodes_in(p, &scope).await,
            Err(e) => tool_api_error(TOOL, &e),
        }
    }

    pub(super) async fn list_nodes_in(
        &self,
        p: ListNodesParams,
        scope: &NodeScope,
    ) -> Result<CallToolResult, McpError> {
        const TOOL: &str = "list_nodes";
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
            Err(e) => return tool_api_error(TOOL, &e),
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
                Err(e) => return tool_api_error(TOOL, &e),
            }
        } else {
            self.state.nodes.list_page(groups, None, limit).await
        };
        let nodes = match nodes {
            Ok(n) => n,
            Err(e) => return tool_error(TOOL, "list nodes", &e),
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
        ok_json(TOOL, &out)
    }

    #[tool(
        description = "Full status for one node: its summary, current active alerts, and \
                       interfaces. Each interface carries its identity (ifindex, name, alias, \
                       nominal `speed` in bits/sec), its current operational state \
                       (`oper_status`, 1 = up), its current load in bits/sec each way with \
                       utilization against the nominal speed, and `stale` when the node has not \
                       reported it recently. Three fields describe the physical link. `duplex` is \
                       the negotiated mode, `half` or `full`; on copper it is diagnostic, because \
                       one end forced to full against an auto-negotiating peer is a common cause \
                       of a link that works but is slow. `media` is the canonical IEEE \
                       designation the port is running as — `1000BASE-T`, `10GBASE-SR`. \
                       `transceiver_model` is the pluggable's vendor part string verbatim \
                       (`SFP-1000BaseLX`) — a PART NUMBER, not a media type, and reported \
                       separately so neither has to pretend to be the other. Null is the ordinary \
                       answer for all three and is NOT a fault to report: the device has to \
                       implement one of a handful of optional MIBs before any of them can be \
                       filled, most do not, and a port that is administratively down has \
                       negotiated nothing to describe. IEEE also defines no half duplex above \
                       1 Gbit/s, so a null `duplex` on a 10G or faster port is what correct looks \
                       like, and a fixed copper port has no pluggable so its `transceiver_model` \
                       is always null. Use `if_type` — the raw IANAifType integer — to tell \
                       \\\"this does not apply\\\" from \\\"we could not read it\\\": 6 is \
                       ethernetCsmacd, while 24 (loopback), 53 (virtual), 23 (dialer) and 131 \
                       (tunnel) have no physical link at all, and calling their nulls a problem \
                       would be a false finding. An optical port additionally carries the \
                       transceiver's own acceptable power window — \
                       `rx_power_low_dbm`/`rx_power_high_dbm` and the transmit pair — which is \
                       what makes a light level from get_interface_series judgeable at all, since \
                       -7 dBm is comfortable on one module and failing on another. All four are \
                       null on a copper port and on optical ports whose vendor publishes no \
                       thresholds. Nothing alerts on them: they are the module's own published \
                       figures, not a threshold configured in Yagra. Use this to \
                       find which port is down or busy; use get_interface_series for one port's \
                       history. Requires live mode (returns an availability note in skeleton mode)."
    )]
    async fn get_node_status(
        &self,
        Parameters(p): Parameters<NodeIdParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        const TOOL: &str = "get_node_status";
        match self.scope_for(identity_of(&ctx)).await {
            Ok(scope) => self.node_status_in(p, &scope).await,
            Err(e) => tool_api_error(TOOL, &e),
        }
    }

    pub(super) async fn node_status_in(
        &self,
        p: NodeIdParams,
        scope: &NodeScope,
    ) -> Result<CallToolResult, McpError> {
        const TOOL: &str = "get_node_status";
        // Out of scope answers exactly what a nonexistent id answers, deliberately: a distinct
        // "not allowed" would confirm the node exists, which is the enumeration oracle
        // `scope::require_visible_node` avoids on the REST side.
        if !scope.allows_node(&self.state, NodeId::from(p.node_id)) {
            return tool_unavailable(TOOL, "no node with that id");
        }
        let Some(admin) = self.state.admin.as_ref() else {
            return tool_unavailable(TOOL, "node detail requires live mode");
        };
        let node = match admin.repo.get_node(p.node_id).await {
            Ok(Some(n)) => n,
            Ok(None) => return tool_unavailable(TOOL, "no node with that id"),
            Err(e) => return tool_error(TOOL, "load node", &e),
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
        // The same query-time join the Interfaces tab shows: one batched fetch for the whole node
        // (3 TSDB round-trips regardless of port count), not one per interface. Without it this
        // tool named a node's ports without being able to say which was down or busy, while the
        // route ledger claimed it folded `GET /nodes/:id/interfaces` (ADR-042 I4).
        let live = self
            .state
            .store
            .node_interface_live(p.node_id, crate::api::DEFAULT_RATE_LOOKBACK_SECS)
            .await;
        let now_s = crate::api::util::now_unix_s();
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
            interfaces: interfaces
                .iter()
                .map(|m| {
                    InterfaceDto::from_meta_and_live(
                        m,
                        live.get(&m.ifindex).copied().unwrap_or_default(),
                        now_s,
                        crate::repo::INTERFACE_STALE_SECS,
                    )
                })
                .collect(),
        };
        ok_json(TOOL, &dto)
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
        match self.admit(identity_of(&ctx), TOOL, "").await {
            Ok(scope) => self.list_node_metrics_in(p, &scope).await,
            Err(refusal) => refusal,
        }
    }

    pub(super) async fn list_node_metrics_in(
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
        const TOOL: &str = "list_node_groups";
        match self.scope_for(identity_of(&ctx)).await {
            Ok(scope) => self.list_node_groups_in(p, &scope).await,
            Err(e) => tool_api_error(TOOL, &e),
        }
    }

    pub(super) async fn list_node_groups_in(
        &self,
        p: ListNodeGroupsParams,
        scope: &NodeScope,
    ) -> Result<CallToolResult, McpError> {
        const TOOL: &str = "list_node_groups";
        let Some(admin) = self.state.admin.as_ref() else {
            return tool_unavailable(TOOL, "node groups require live mode");
        };
        // `visible_groups` keeps the caller's subtree *and* the ancestors above it. This DTO carries
        // `parent_id`, so a filter that dropped the ancestors would leave every visible root
        // pointing at a group that is not in the list.
        let groups = match crate::api::groups::visible_groups(admin, scope).await {
            Ok(g) => g,
            Err(e) => return tool_api_error(TOOL, &e),
        };
        if !p.include_state.unwrap_or(false) {
            let out: Vec<crate::mcp::dto::NodeGroupDto> =
                groups.iter().map(NodeGroupDto::from_summary).collect();
            return ok_json(TOOL, &out);
        }
        // The rollup the site-matrix widget reads, joined onto the tree rather than served as a
        // second bare `group_id → counts` map: a model given the map alone has no names to attach
        // the numbers to, and would have to call this tool again to get them.
        let rollup = match crate::api::fleet::group_summary(&self.state, scope).await {
            Ok(s) => s,
            Err(e) => return tool_api_error(TOOL, &e),
        };
        let out: Vec<crate::mcp::dto::NodeGroupDto> = groups
            .iter()
            .map(|g| NodeGroupDto::from_summary(g).with_state(rollup.groups.get(&g.id)))
            .collect();
        ok_json(TOOL, &out)
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
        const TOOL: &str = "poll_now";
        let Some(identity) = authed_for(identity_of(&ctx), Permission::ManageConfig) else {
            return tool_forbidden(TOOL, "this token lacks manage-config permission");
        };
        // Belt-and-braces: `ManageConfig` is Admin, and an Admin cannot hold a group scope
        // (`auth.rs::ADMIN_IS_UNSCOPED`), so this can only ever pass today. It is here so the tool
        // does not depend on that invariant holding somewhere else — the check is one line, and
        // "a permission that happens to imply unscoped" is not a property to build on silently.
        if let Some(deny) = self
            .deny_invisible_node_for(identity_of(&ctx), TOOL, p.node_id)
            .await
        {
            return deny;
        }
        let Some(admin) = self.state.admin.as_ref() else {
            return tool_unavailable(TOOL, "poll requires live mode");
        };
        // Same dispatch as `POST /api/v1/nodes/:id/poll`, including resolving the node's effective
        // pool (own > folder > default) — a manual poll published to the wrong pool's subject has
        // no poller listening for it, and that is not a mistake worth being able to make twice.
        let result = match crate::api::nodes::poll_now(admin, p.node_id).await {
            Ok(r) => r,
            Err(e) => return tool_api_error(TOOL, &e),
        };
        record_audit(
            &self.state,
            &identity,
            &format!("mcp.poll_now node={}", p.node_id),
            202,
        )
        .await;
        ok_json(TOOL, &result)
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
        const TOOL: &str = "fleet_state_history";
        match self.admit(identity_of(&ctx), TOOL, "").await {
            Ok(scope) => self.state_history_in(p, &scope).await,
            Err(refusal) => refusal,
        }
    }

    pub(super) async fn state_history_in(
        &self,
        p: StateHistoryParams,
        scope: &NodeScope,
    ) -> Result<CallToolResult, McpError> {
        const TOOL: &str = "fleet_state_history";
        let Some(admin) = self.state.admin.as_ref() else {
            return tool_unavailable(TOOL, "state history requires live mode");
        };
        // The refusal lives inside the seam, so it cannot be forgotten here.
        match crate::api::fleet::state_history(admin, scope, p.from, p.to).await {
            Ok(h) => ok_json(TOOL, &h),
            Err(e) => tool_api_error(TOOL, &e),
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
        const TOOL: &str = "get_dns_chain";
        let arg = if p.history.unwrap_or(false) {
            "history"
        } else {
            "current"
        };
        match self.admit(identity_of(&ctx), TOOL, arg).await {
            Ok(scope) => self.dns_chain_in(p, &scope).await,
            Err(refusal) => refusal,
        }
    }

    pub(super) async fn dns_chain_in(
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::tools::testkit::*;

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
}
