// SPDX-License-Identifier: AGPL-3.0-only
//! MCP tools: what is cabled or adjacent to what (ADR-086).
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
use uuid::Uuid;

use super::YagraMcp;
use crate::api::scope::NodeScope;

// The shared scope: the helpers in `support.rs` and the types the other domain modules declare,
// re-exported by `mod.rs` so no file has to name where a sibling keeps a thing.
use super::*;

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(super) struct NeighborsParams {
    /// The node's UUID.
    pub(super) node_id: Uuid,
    /// Max recent adjacency changes to include (1–200, default 10).
    pub(super) history_limit: Option<i64>,
    /// Keyset cursor: the `at` of the last change you saw (RFC 3339). Pair with `before_id`.
    pub(super) before_at: Option<String>,
    /// Keyset cursor: the `id` of the last change you saw. Pair with `before_at`.
    pub(super) before_id: Option<i64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(super) struct DiscoveredEndpointsParams {
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

/// Which graph `get_topology` was asked for.
///
/// Split out of the tool body so the folding decision is testable without a `RequestContext` — the
/// `*_in` shape `api-conventions.md` asks for. `Unknown` is deliberately distinct from the default:
/// a caller who typed `kind="Links"` should be told, not silently handed the dependency graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TopologyKind {
    Dependency,
    Links,
    Overrides,
    Shadow,
    Unknown,
}

pub(super) fn topology_kind(kind: Option<&str>) -> TopologyKind {
    match kind {
        None | Some("dependency") => TopologyKind::Dependency,
        Some("links") => TopologyKind::Links,
        Some("overrides") => TopologyKind::Overrides,
        Some("shadow") => TopologyKind::Shadow,
        Some(_) => TopologyKind::Unknown,
    }
}

#[tool_router(router = topology_router, vis = "pub(super)")]
impl YagraMcp {
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
        const TOOL: &str = "get_neighbors";
        match self.scope_of(&ctx).await {
            Ok(scope) => self.neighbors_in(p, &scope).await,
            Err(e) => tool_api_error(TOOL, &e),
        }
    }

    pub(super) async fn neighbors_in(
        &self,
        p: NeighborsParams,
        scope: &NodeScope,
    ) -> Result<CallToolResult, McpError> {
        const TOOL: &str = "get_neighbors";
        // Scope first, availability second — the same order the REST guards run in. Reversed, a
        // scoped caller learns from the error which nodes exist.
        if let Some(deny) = deny_invisible_node(&self.state, scope, TOOL, p.node_id) {
            return deny;
        }
        let Some(admin) = self.state.admin.as_ref() else {
            return tool_unavailable(TOOL, "neighbours require live mode");
        };
        let cursor = match crate::api::neighbors::parse_history_cursor(
            p.before_at.as_deref(),
            p.before_id,
        ) {
            Ok(c) => c,
            Err(e) => return tool_api_error(TOOL, &e),
        };
        // Current and history are one question, so this returns both rather than branching on a
        // mode param — a result whose shape depends on an argument is harder for a model than two
        // tools would be, and buys nothing.
        let current = match crate::api::neighbors::current_neighbors(admin, p.node_id).await {
            Ok(c) => c,
            // 404 here means "never walked", which `tool_api_error` renders as an availability note
            // rather than an error. Inventing an empty set instead would assert the device has no
            // neighbours, which is a different and false claim.
            Err(e) => return tool_api_error(TOOL, &e),
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
            Err(e) => return tool_api_error(TOOL, &e),
        };
        ok_json_value(
            TOOL,
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
        const TOOL: &str = "list_discovered_endpoints";
        match self.scope_of(&ctx).await {
            Ok(scope) => self.discovered_endpoints_in(p, &scope).await,
            Err(e) => tool_api_error(TOOL, &e),
        }
    }

    pub(super) async fn discovered_endpoints_in(
        &self,
        p: DiscoveredEndpointsParams,
        scope: &NodeScope,
    ) -> Result<CallToolResult, McpError> {
        const TOOL: &str = "list_discovered_endpoints";
        let Some(admin) = self.state.admin.as_ref() else {
            return tool_unavailable(TOOL, "endpoint discovery requires live mode");
        };
        let cursor = match crate::api::discovery::endpoint_cursor(
            p.before_last_seen.as_deref(),
            p.before_id,
        ) {
            Ok(c) => c,
            Err(e) => return tool_api_error(TOOL, &e),
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
            Ok(page) => ok_json(TOOL, &page),
            Err(e) => tool_api_error(TOOL, &e),
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
        const TOOL: &str = "get_topology";
        match self.scope_of(&ctx).await {
            Ok(scope) => self.topology_in(p, &scope).await,
            Err(e) => tool_api_error(TOOL, &e),
        }
    }

    pub(crate) async fn topology_in(
        &self,
        p: TopologyParams,
        scope: &NodeScope,
    ) -> Result<CallToolResult, McpError> {
        const TOOL: &str = "get_topology";
        let Some(admin) = self.state.admin.as_ref() else {
            return tool_unavailable(TOOL, "topology requires live mode");
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
                            TOOL,
                            "`after` must be a node UUID when kind is dependency",
                        )
                    }
                    None => None,
                };
                match crate::api::topology::topology_page(&self.state, admin, scope, after, limit)
                    .await
                {
                    Ok(page) => ok_json(TOOL, &page),
                    Err(e) => tool_api_error(TOOL, &e),
                }
            }
            TopologyKind::Links => {
                let after = match p.after.as_deref().map(str::parse::<i64>) {
                    Some(Ok(id)) => Some(id),
                    Some(Err(_)) => {
                        return tool_bad_params(TOOL, "`after` must be a number when kind is links")
                    }
                    None => None,
                };
                match crate::api::topology::topology_link_page(admin, scope, after, limit).await {
                    Ok(page) => ok_json(TOOL, &page),
                    Err(e) => tool_api_error(TOOL, &e),
                }
            }
            TopologyKind::Overrides => {
                match crate::api::topology::link_override_list(&self.state, admin, scope).await {
                    Ok(list) => ok_json(TOOL, &list),
                    Err(e) => tool_api_error(TOOL, &e),
                }
            }
            TopologyKind::Shadow => {
                match crate::api::topology::topology_shadow(&self.state, admin, scope).await {
                    Ok(s) => ok_json(TOOL, &s),
                    Err(e) => tool_api_error(TOOL, &e),
                }
            }
            TopologyKind::Unknown => tool_bad_params(
                TOOL,
                "`kind` must be one of: dependency, links, overrides, shadow",
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::tools::testkit::*;

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

    // ── ADR-042 I1 tools ────────────────────────────────────────────────────────────────────────

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
}
