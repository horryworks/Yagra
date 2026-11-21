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
use uuid::Uuid;
use yagra_common::{NodeId, NodeKind, Permission};

use super::YagraMcp;
use crate::api::scope::NodeScope;
use crate::mcp::config_text;
use crate::mcp::dto::{
    AlertDto, ConfigLineDto, ConfigVersionDto, FleetSummaryDto, InterfaceDto, NodeConfigTextDto,
    NodeConfigVersionsDto, NodeGroupDto, NodeStatusDto, NodeSummaryDto,
};

// The shared scope: the helpers in `support.rs` and the types the other domain modules declare,
// re-exported by `mod.rs` so no file has to name where a sibling keeps a thing.
use super::*;

/// Which fleet view `get_fleet_summary` was asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FleetSummaryKind {
    Summary,
    Coverage,
    Watch,
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
        Some("watch") => Some(FleetSummaryKind::Watch),
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
            "unknown kind {:?}; must be summary, coverage or watch",
            kind.unwrap_or_default()
        ),
    )
}

// ── Tool parameter structs (schemas derived for `tools/list`) ─────────────────────────────────────

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
pub(super) struct FleetSummaryParams {
    /// `summary` (default) for the state tally, `coverage` for the monitoring blind-spot view, or
    /// `watch` for how many nodes are not being watched right now and why.
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

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(super) struct NodeAvailabilityParams {
    /// The node's UUID.
    node_id: Uuid,
    /// How far back: `24h`, `7d`, `30d` (default) or `90d`.
    period: Option<String>,
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
pub(super) struct ListNodesParams {
    /// Up to eight words separated by spaces. A node is returned when every word is a
    /// case-insensitive substring of its name, its address, one of its ports' addresses, or the
    /// name of a folder above it.
    search: Option<String>,
    /// Exact IP address of the node, compared as an address (so `2001:DB8::1` finds
    /// `2001:db8::1`). Pair it with `kind=device,meraki` to ask whether a device is already
    /// monitored at an address. A value that is not an IP address is an error.
    address: Option<String>,
    /// Max nodes to return (1–100, default 50).
    limit: Option<i64>,
    /// Rolled-up states to include, comma-separated: `ok` | `warning` | `critical` |
    /// `unreachable` | `unknown` | `maintenance`. Omit for all. `warning,critical,unreachable` is
    /// "everything that is not healthy". An unknown token is an error rather than being ignored.
    state: Option<String>,
    /// Monitoring kinds to include, comma-separated: `wireless_ap` | `meraki` | `url` | `dns` |
    /// `device`. Omit for all.
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

/// What `get_node_config` was asked for (ADR-210 stage 10 decision 82).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ConfigView {
    Versions,
    Text,
    Diff,
    Unsaved,
}

impl ConfigView {
    /// Every view, so a token is spelt once, in [`Self::as_str`].
    const ALL: [Self; 4] = [Self::Versions, Self::Text, Self::Diff, Self::Unsaved];

    /// Omitted means `text`: the latest configuration is what a caller asking for one wants.
    pub(super) fn from_token(view: Option<&str>) -> Option<Self> {
        match view {
            None => Some(Self::Text),
            Some(t) => Self::ALL.into_iter().find(|v| v.as_str() == t),
        }
    }

    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Versions => "versions",
            Self::Text => "text",
            Self::Diff => "diff",
            Self::Unsaved => "unsaved",
        }
    }

    /// What the view demands: the list of versions is `View`, as `GET …/versions` is; text is
    /// `ManageConfig`, as the REST routes that serve it are (ADR-210 decision 10).
    pub(super) fn permission(self) -> Permission {
        match self {
            Self::Versions => Permission::View,
            Self::Text | Self::Diff | Self::Unsaved => Permission::ManageConfig,
        }
    }

    /// Whether the view carries configuration text, and so needs the switch and an audit row.
    pub(super) fn carries_text(self) -> bool {
        self != Self::Versions
    }
}

/// Lines either side of a change that a `diff` keeps, as a unified diff shows them.
const DIFF_CONTEXT: usize = 3;

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(super) struct NodeConfigParams {
    /// The node's UUID.
    node_id: Uuid,
    /// `text` (default) for one version, `diff` for two compared, `unsaved` for how the running
    /// configuration differs from startup, or `versions` for the list of stored versions.
    view: Option<String>,
    /// `text`: the version to show (default the latest).
    version: Option<String>,
    /// `diff`: the older version (default the one before `to`).
    from: Option<String>,
    /// `diff`: the newer version (default the latest).
    to: Option<String>,
    /// Keep only lines matching this regular expression (at most 200 characters), for example
    /// `^interface ` or `(?i)snmp`. Matched against the text after secrets are blanked.
    #[serde(rename = "match")]
    pattern: Option<String>,
    /// Matched lines to skip, for the next page (default 0).
    offset: Option<usize>,
    /// Lines to return (1-2000, default 500).
    limit: Option<usize>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(super) struct PrefixGapsParams {
    /// The folder to check (an id from list_node_groups). Its subfolders are included.
    group_id: Uuid,
}

/// The answer for a node with no stored version.
fn no_versions() -> Result<CallToolResult, McpError> {
    tool_unavailable(
        "get_node_config",
        "this node has no stored configuration version; get_node_status says whether it is \
         backed up and how its last fetch went",
    )
}

/// Numbered lines from redacted ones.
fn numbered(lines: Vec<(String, bool)>) -> Vec<(ConfigLineDto, bool)> {
    lines
        .into_iter()
        .enumerate()
        .map(|(i, (text, redacted))| {
            (
                ConfigLineDto {
                    n: i + 1,
                    change: None,
                    text,
                },
                redacted,
            )
        })
        .collect()
}

/// Which two versions a `diff` compares when the caller left one or both out: `to` defaults to the
/// latest and `from` to the version stored just before `to`. `versions` is newest first.
pub(super) fn pick_diff_pair(
    versions: &[crate::api::config_backup::ConfigVersion],
    from: Option<&str>,
    to: Option<&str>,
) -> Option<(String, String)> {
    let to = match to {
        Some(t) => t.to_owned(),
        None => versions.first()?.oid.clone(),
    };
    let from = match from {
        Some(f) => f.to_owned(),
        None => {
            let at = versions.iter().position(|v| v.oid == to)?;
            versions.get(at + 1)?.oid.clone()
        }
    };
    Some((from, to))
}

/// Two texts compared line by line, each side's secrets blanked over its whole text first (so a
/// private-key block is recognised across its lines) and only changes with their context kept.
///
/// The comparison runs over the texts as stored, then takes each line's words from the blanked
/// side, so a changed password still shows as a changed (blanked) line rather than vanishing.
pub(super) fn diff_lines(old: &str, new: &str) -> Vec<(ConfigLineDto, bool)> {
    let diff = similar::TextDiff::from_lines(old, new);
    // Blank the very lines the diff indexes. `str::lines` and `similar` do not cut a text at the
    // same places (a lone `\r` is one example), and a blanked line looked up by an index from the
    // other cut is another line's words, or a private-key line judged without its block.
    let old_red = config_text::redact_lines(diff.old_slices().iter().copied());
    let new_red = config_text::redact_lines(diff.new_slices().iter().copied());
    let changes: Vec<(similar::ChangeTag, String, bool)> = diff
        .iter_all_changes()
        .map(|c| {
            let side = match c.tag() {
                similar::ChangeTag::Delete => c.old_index().and_then(|i| old_red.get(i)),
                similar::ChangeTag::Equal | similar::ChangeTag::Insert => {
                    c.new_index().and_then(|i| new_red.get(i))
                }
            };
            // Fail closed: a change whose line was not blanked above is shown as blanked whole.
            let (text, redacted) = side
                .cloned()
                .unwrap_or_else(|| (config_text::REDACTED.to_owned(), true));
            (c.tag(), text, redacted)
        })
        .collect();
    let changed: Vec<bool> = changes
        .iter()
        .map(|(tag, _, _)| *tag != similar::ChangeTag::Equal)
        .collect();
    let keep = config_text::diff_context(&changed, DIFF_CONTEXT);
    changes
        .into_iter()
        .enumerate()
        .filter(|(i, _)| keep[*i])
        .map(|(i, (tag, text, redacted))| {
            (
                ConfigLineDto {
                    n: i + 1,
                    change: Some(match tag {
                        similar::ChangeTag::Equal => "same",
                        similar::ChangeTag::Insert => "added",
                        similar::ChangeTag::Delete => "removed",
                    }),
                    text,
                },
                redacted,
            )
        })
        .collect()
}

#[tool_router(router = nodes_router, vis = "pub(super)")]
impl YagraMcp {
    #[tool(
        description = "Fleet health summary: total node count, node counts per rolled-up state \
                       (ok/warning/critical/unknown/unreachable/maintenance), the number of active \
                       alerts, and which optional data tiers (metrics/flow/log) are enabled. Start here. \
                       Pass kind=\"coverage\" instead for the blind-spot view — which nodes have \
                       actually reported recently, and a watchlist of the ones that have not. \
                       Pass kind=\"watch\" for counts of nodes not being watched right now: \
                       no_live_poller (their poller pool has no live poller), no_recent_data, \
                       pool_polled, and pollers_known (false on an HA standby)."
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
            FleetSummaryKind::Watch => self.fleet_watch_in(scope).await,
        }
    }

    /// The watch branch (ADR-227): the same counts the Active alerts page shows above its list.
    pub(super) async fn fleet_watch_in(
        &self,
        scope: &NodeScope,
    ) -> Result<CallToolResult, McpError> {
        const TOOL: &str = "get_fleet_summary";
        let Some(admin) = self.state.admin.as_ref() else {
            return tool_unavailable(TOOL, "watch requires live mode");
        };
        match crate::api::fleet::watch(&self.state, admin, scope).await {
            Ok(w) => ok_json(TOOL, &w),
            Err(e) => tool_api_error(TOOL, &e),
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
        description = "List monitored nodes with their rolled-up state. Optional `search` is up \
                       to eight words separated by spaces, every one of which must match, case \
                       ignored, the node's name, its address, one of its ports' addresses or the \
                       name of a folder above it; narrow further with `state` \
                       (ok|warning|critical|unreachable|unknown|maintenance), `kind` \
                       (wireless_ap|meraki|url|dns|device) and `pool` (the effective poll pool, \
                       inherited from the folder tree when the node sets none); `limit` is 1–100 \
                       (default 50). Returns node id, name, address, state, kind, group, \
                       vendor, model, and tags. `kind` says what a node is — `device`, `url`, \
                       `dns`, `meraki` or `wireless_ap` — and therefore which metrics it can have. \
                       `address` is null when the node has none — a Meraki access point marked \
                       `meraki_repeater: true` is a mesh repeater, with no wired uplink and so \
                       no LAN IP."
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
        let limit = crate::api::util::page_limit(p.limit, 50, 100);
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
        // The same edge parser as REST, so a model sending a non-address is told so rather than
        // handed the whole fleet.
        let address = match crate::api::nodes::parse_address_filter(p.address.as_deref()) {
            Ok(a) => a,
            Err(e) => return tool_api_error(TOOL, &e),
        };
        // The scope goes into the query as the same indexed `group_id = ANY(…)` predicate the REST
        // list uses — `NodeListing` takes a `GroupFilter` on every method precisely so no call site
        // can default to the whole fleet without saying so.
        let groups = scope.group_filter();
        // The words go through the same reader as REST (ADR-258), so a ninth word is the same
        // refusal and a folder name the same match.
        let words = match crate::api::nodes::search_words_for(
            &self.state,
            scope,
            p.search.as_deref().unwrap_or(""),
        )
        .await
        {
            Ok(w) => w,
            Err(e) => return tool_api_error(TOOL, &e),
        };
        let nodes = if !words.is_empty() || address.is_some() || filter.is_set() {
            // The shared seam, so a model and the WebUI cannot be told different things about
            // which nodes match — the filters run in-process here too, over the same bounded scan.
            match crate::api::nodes::filtered_node_page(
                &self.state,
                scope,
                &words,
                address,
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
        // The kind comes from the same resolver the REST list uses (ADR-042 read parity), so a
        // model and the WebUI cannot be told different things about what a node is.
        let ids: Vec<NodeId> = nodes.iter().map(|n| n.id).collect();
        let (states, mut kinds) = crate::api::nodes::node_enrichment(&self.state, &ids).await;
        // Built once for the whole page, like `kinds` above — a resolver per node would be one
        // whole-table read per row.
        let tags = match self.state.admin.as_ref() {
            Some(admin) => crate::api::util::tag_resolver(admin).await,
            None => crate::inventory::tagres::TagResolver::empty(),
        };
        let out: Vec<NodeSummaryDto> = nodes
            .iter()
            .map(|n| {
                NodeSummaryDto::from_node(
                    n,
                    states.get(&n.id).copied(),
                    kinds
                        .kinds
                        .get(&n.id.as_uuid())
                        .copied()
                        .unwrap_or(NodeKind::Device),
                    &tags,
                )
                .with_meraki_product_type(kinds.meraki_product_types.remove(&n.id.as_uuid()))
                .with_meraki_repeater(kinds.meraki_repeaters.contains(&n.id.as_uuid()))
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
                       figures, not a threshold configured in Yagra. \
                       `addresses` lists every IP address configured on the interface as the \
                       device reports it — secondaries included, IPv4 before IPv6, and SNMP does \
                       not say which one is primary. Each entry is `ip` plus `prefix_len` in bits \
                       (24 for a /24); a null `prefix_len` means the device gave a mask that could \
                       not be decoded, NOT that the address is a host route. The list comes from \
                       an address walk that runs about once an hour, so an empty list on every \
                       port of a node means that walk has not run yet or the device does not \
                       answer the IP address tables — not that the device has no addresses — and \
                       a change made on the device can take up to an hour to show. An address \
                       the device attributes to no interface is not listed. \
                       `vlan` is the port's VLAN configuration — `mode` (access, trunk, hybrid, \
                       member of the aggregate named by `lag`, not_l2, unknown), the native VLAN, \
                       the access and voice VLANs, and the trunk's allowed VLANs as configured, \
                       with every VLAN written as the single span 1-4094. It is read about once an \
                       hour for Cisco Catalyst, Huawei and Meraki MS switches; null means not \
                       reported (not walked yet, another make, or a device that answers none of the \
                       VLAN tables), not that the port has no VLAN. \
                       `vlans` is the node's VLANs by ID — the ones its VLAN table defines and the \
                       ones its ports use as access, voice or native VLAN (a trunk's allowed list \
                       adds none). Each has `name` (null when names were not read; Cisco reads \
                       CISCO-VTP-MIB, Huawei Q-BRIDGE, a Meraki switch or MX its network's MX VLAN \
                       list), `state` (active or suspended, Cisco only), `defined` (false means a \
                       port uses a VLAN the device's own table does not list — a configuration gap; \
                       null means not judged, always for Meraki), `subnet` (Meraki only) and \
                       `gateway` — the VLAN interface that routes it (Vlan30, Vlanif30) with its \
                       addresses, or for a Meraki switch the network's MX, named by `node_id`. \
                       Empty until the hourly VLAN walk has run. \
                       `snmp_configured` says whether SNMP polling is CONFIGURED for this node — a \
                       credential bound to it, or the deployment-wide fallback community — and NOT \
                       whether the device is answering. False means no ifTable or CDP/LLDP walk \
                       ever runs, so an empty `interfaces` is the design and there are no \
                       neighbours to ask get_neighbors for; do not report either as a fault. A \
                       Meraki switch is the exception: its ports and its LLDP/CDP neighbours are \
                       read from the Meraki Dashboard whatever this says, and so are a Meraki \
                       MX's and MR's neighbours. True \
                       with nothing arriving is the case worth investigating, and \
                       list_node_metrics is what tells the two apart. `notes` is free text an \
                       operator wrote about this node — standing context such as \"this link \
                       flaps on purpose\" or \"replacement scheduled\" — and is null when nobody \
                       wrote one; read it before calling anything a fault. Use this to \
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
        // `get_node_with_notes`, not `get_node`: this tool folds `GET /api/v1/nodes/{node_id}`, so
        // it owes the same answer that route gives — the note included (ADR-135).
        let (node, notes, os_version, serial_number, profile_locked, sys_name) =
            match admin.repo.get_node_with_notes(p.node_id).await {
                Ok(Some(n)) => (
                    n.node,
                    n.notes,
                    n.os_version,
                    n.serial_number,
                    n.profile_locked,
                    n.sys_name,
                ),
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
            .node_interface_live(
                p.node_id,
                yagra_common::poll_interval::RATE_WINDOW_FLOOR_SECS,
            )
            .await;
        // Each port's addresses, from the same stored set the REST list joins (ADR-157). Best
        // effort like the interface list above it: a failed read leaves the lists empty rather
        // than failing the whole status.
        let l3 = admin.l3.current(p.node_id).await.unwrap_or(None);
        let mut addresses = crate::api::collection::addresses_by_ifindex(l3.as_ref());
        // Each port's mode and VLANs, from the same stored snapshot the REST list joins (ADR-201).
        // Best effort like the addresses: a failed read leaves every port's `vlan` null.
        let vlans = crate::api::collection::VlanJoin::new(
            admin.vlans.current(p.node_id).await.unwrap_or(None),
            &interfaces,
        );
        let now_s = crate::api::util::now_unix_s();
        let kind = crate::api::nodes::node_kinds(admin, &[p.node_id])
            .await
            .get(&p.node_id)
            .copied()
            .unwrap_or(NodeKind::Device);
        // The fallback the REST detail applies (ADR-147 decision 7), from the same function. Best
        // effort for the reason it is there: a failed Meraki lookup reads as "no binding".
        let meraki = admin.meraki_devices.get(p.node_id).await.unwrap_or(None);
        let serial_number = crate::api::nodes::serial_number_of(serial_number, meraki.as_ref());
        let wireless =
            crate::api::wireless::node_wireless(&self.state, admin, scope, p.node_id).await;
        // The same pair, from the same function, narrowed to the same scope (ADR-164 decision 26).
        let meraki_pair = crate::api::meraki::node_meraki_pair(
            &self.state,
            admin,
            scope,
            p.node_id,
            meraki.as_ref(),
        )
        .await;
        // The organization and network by name, from the same function (ADR-185).
        let meraki_site = match meraki {
            Some(_) => crate::api::meraki::node_meraki_site(admin, p.node_id).await,
            None => None,
        };
        // The product type and the repeater mark the list carries (ADR-168 decision 11, ADR-175), read
        // only for a Meraki node.
        let product = match meraki {
            Some(_) => admin
                .meraki_devices
                .product_types(&[p.node_id])
                .await
                .unwrap_or_default()
                .remove(&p.node_id),
            None => None,
        };
        // The site's zone and delivery state, from the function the REST detail calls (ADR-252).
        let local_time = crate::api::nodes::local_time_of(admin, &node).await;
        // The node's VLANs, from the function `GET /nodes/{id}/vlans` calls, narrowed to the same
        // scope (ADR-257). Best effort like the interface list: a failed read leaves it empty.
        let node_vlans = crate::api::vlans::node_vlans(admin, p.node_id, |n| {
            scope.allows_node(&self.state, NodeId::from(n))
        })
        .await
        .unwrap_or_default();
        let dto = NodeStatusDto {
            node: NodeSummaryDto::from_node(
                &node,
                Some(state),
                kind,
                &crate::api::util::tag_resolver(admin).await,
            )
            .with_meraki_repeater(product.as_ref().is_some_and(|p| p.mesh_repeater))
            .with_meraki_product_type(product.map(|p| p.product_type)),
            // The same one rule the REST detail view reads, from the same holder — never
            // `node.credential`, which misses the deployment-wide community fallback (ADR-119).
            snmp_configured: admin.dispatcher.snmp_configured_for(&node),
            time_zone: local_time.0,
            delivery: local_time.1,
            notes,
            os_version,
            serial_number,
            profile_locked,
            sys_name,
            wireless,
            meraki_pair,
            meraki_site,
            config_backup: crate::api::config_backup::summary_of(admin, p.node_id).await,
            collection_fault: crate::api::nodes::collection_fault_of(&self.state, node.id).await,
            // Every alert here is on this node, so its name is this node's name.
            alerts: alerts
                .iter()
                .map(|a| {
                    let mut dto = AlertDto::from_alert(a, Some(node.name.clone()));
                    // The node's own interface list is already in hand: name the port from it.
                    dto.if_name = a.ifindex.and_then(|i| {
                        interfaces
                            .iter()
                            .find(|m| u32::try_from(m.ifindex).ok() == Some(i.0))
                            .and_then(|m| m.if_name.clone())
                            .filter(|n| !n.trim().is_empty())
                    });
                    dto
                })
                .collect(),
            interfaces: interfaces
                .iter()
                .map(|m| {
                    InterfaceDto::from_meta_and_live(
                        m,
                        live.get(&m.ifindex).copied().unwrap_or_default(),
                        now_s,
                        yagra_base::repo::INTERFACE_STALE_SECS,
                        u32::try_from(m.ifindex)
                            .ok()
                            .and_then(|i| addresses.remove(&i))
                            .unwrap_or_default(),
                        u32::try_from(m.ifindex)
                            .ok()
                            .and_then(|i| vlans.for_port(i)),
                    )
                })
                .collect(),
            vlans: node_vlans,
        };
        ok_json(TOOL, &dto)
    }

    #[tool(
        description = "Which metrics a node actually has, and how each must be read. Call this \
                       before query_metrics rather than guessing a name. Each entry gives \
                       `metric_kind` (gauge or counter — a counter's stored value is an odometer, \
                       so ask query_metrics for mode=rate), `dimension` (none = one series per \
                       node; interface = one per interface, use get_interface_series; entity = one \
                       per table row such as a memory pool, CPU or sensor — query_metrics reads \
                       the node-wide maximum by default and one row with `row`, and \
                       mode=latest lists every row with its name), and `status` (ok = configured and \
                       flowing; no_data = configured but nothing has arrived; unconfigured = data \
                       exists with no collection item, which is normal for reachability, URL/DNS \
                       monitors and neighbour counts). A configured metric that comes from a \
                       metric set also names it in `template` (e.g. `Cisco WLAN SSIDs (WLC)`), \
                       which says which device family's reading it is. `within_secs` sets how far \
                       back a metric may have last been seen and still count as having data \
                       (default 6 hours)."
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
                       A folder carrying `origin` (`meraki` or `netbox`) was made by that \
                       integration and is still kept by it, not by a person. \
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
        description = "Which subnets the devices in one folder (and every folder beneath it) \
                       carry that none of those folders' IP prefixes contains. Each subnet says \
                       why: `unregistered` (no prefix overlaps it), `partial` (a prefix lies \
                       inside it but none covers all of it), `other_folder` (another folder's \
                       prefix contains it: a prefix attached to the wrong folder, or one private \
                       range reused at two sites), or `parent_only` (only a folder above has a \
                       containing prefix). The prefix and \
                       its folder are omitted when that folder is outside your scope. No gaps \
                       means complete only when nodes_with_addresses equals nodes_total — a \
                       device with no SNMP address walk contributes nothing. A folder holding \
                       more than 2,000 devices is refused; ask about a folder further down. \
                       Requires live mode."
    )]
    async fn get_prefix_gaps(
        &self,
        Parameters(p): Parameters<PrefixGapsParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        const TOOL: &str = "get_prefix_gaps";
        match self.scope_for(identity_of(&ctx)).await {
            Ok(scope) => self.prefix_gaps_in(p, &scope).await,
            Err(e) => tool_api_error(TOOL, &e),
        }
    }

    pub(super) async fn prefix_gaps_in(
        &self,
        p: PrefixGapsParams,
        scope: &NodeScope,
    ) -> Result<CallToolResult, McpError> {
        const TOOL: &str = "get_prefix_gaps";
        let Some(admin) = self.state.admin.as_ref() else {
            return tool_unavailable(TOOL, "prefix gaps require live mode");
        };
        // The REST handler's own function, so the two surfaces disclose the same things.
        match crate::api::groups::prefix_gap_report(admin, scope, p.group_id).await {
            Ok(report) => ok_json(TOOL, &report),
            Err(e) => tool_api_error(TOOL, &e),
        }
    }

    #[tool(
        description = "For every site, the subnets its devices carry that none of the IP prefixes \
                       filed in the site's folder (or beneath it) contains: get_prefix_gaps for \
                       the whole fleet at once. A site is the nearest folder of type Site above a \
                       device, else the device's own folder (is_site false); devices filed in no \
                       folder are one site with a null site_id. Each site has a status: gaps, \
                       clean, intentional (every gap it has was marked intentional by an \
                       operator), or no_data (none of its devices reported an address, so nothing \
                       was compared; never read that as complete). Each gap carries the same kind \
                       as get_prefix_gaps (unregistered, partial, other_folder, parent_only), and \
                       intentional with the operator's note when it was marked; gap_count counts \
                       only the unmarked ones, intentional_count the rest. Sites are ordered most \
                       unmarked gaps first; at most 2,000 gaps are listed in all \
                       (gaps_total says how many there are, each site's gap_count is uncapped). A \
                       range filed in a folder outside your scope is omitted. Requires live mode."
    )]
    async fn get_site_prefix_gaps(
        &self,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        const TOOL: &str = "get_site_prefix_gaps";
        match self.scope_for(identity_of(&ctx)).await {
            Ok(scope) => self.site_prefix_gaps_in(&scope).await,
            Err(e) => tool_api_error(TOOL, &e),
        }
    }

    pub(super) async fn site_prefix_gaps_in(
        &self,
        scope: &NodeScope,
    ) -> Result<CallToolResult, McpError> {
        const TOOL: &str = "get_site_prefix_gaps";
        let Some(admin) = self.state.admin.as_ref() else {
            return tool_unavailable(TOOL, "prefix gaps require live mode");
        };
        // The REST handler's own function, so the two surfaces disclose the same things.
        match crate::api::prefix_gaps::site_gaps_view(admin, scope).await {
            Ok(view) => ok_json(TOOL, &view),
            Err(e) => tool_api_error(TOOL, &e),
        }
    }

    #[tool(
        description = "Address ranges that more than one site carries, from the interface addresses \
                       devices report: the same range at two sites (same_address when one address \
                       is configured at both, which is almost certainly a reuse; same_range when \
                       every address differs, which is a reuse or a line the sites share), or one \
                       site's range inside another's (nested). A site is the nearest folder of type \
                       Site above a device, or its own folder when there is none. Each overlap has a status: open, intentional (an \
                       operator said it is deliberate), or excluded (a link between two sites, or \
                       an exclusion rule such as a WAN port or the carrier CGNAT range), and may \
                       carry a hint (wan, redundancy, shared_line, template), which is a suggestion \
                       only. Also returns the exclusion rules. No overlaps means none only when \
                       nodes_with_addresses equals nodes_total. A folder-scoped token is told how \
                       many other sites an overlap reaches, never which. Requires live mode."
    )]
    async fn get_subnet_overlaps(
        &self,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        const TOOL: &str = "get_subnet_overlaps";
        match self.scope_for(identity_of(&ctx)).await {
            Ok(scope) => self.subnet_overlaps_in(&scope).await,
            Err(e) => tool_api_error(TOOL, &e),
        }
    }

    pub(super) async fn subnet_overlaps_in(
        &self,
        scope: &NodeScope,
    ) -> Result<CallToolResult, McpError> {
        const TOOL: &str = "get_subnet_overlaps";
        let Some(admin) = self.state.admin.as_ref() else {
            return tool_unavailable(TOOL, "subnet overlaps require live mode");
        };
        // The REST handler's own function, so the two surfaces disclose the same things.
        match crate::api::subnet_overlaps::overlaps_view(admin, scope).await {
            Ok(view) => ok_json(TOOL, &view),
            Err(e) => tool_api_error(TOOL, &e),
        }
    }

    #[tool(
        description = "Trigger an immediate, out-of-schedule poll of one node. Requires manage-config \
                       permission. Returns how many poll jobs were dispatched. The poll also reads \
                       what is otherwise read only once an hour: the device's identity (sysDescr, \
                       OS version, serial number) and the names of its vendor-table rows. Results \
                       arrive asynchronously. Requires live mode."
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
        description = "A device configuration backed up by Yagra's config backup: `view=text` \
                       (default) for one stored version, `diff` for two versions compared, \
                       `unsaved` for how the running configuration differs from startup, or \
                       `versions` for the list of stored versions and their times. Text comes as \
                       numbered lines with secrets replaced by `<redacted>`: a line naming a \
                       password, secret, community, key, cipher or similar loses everything after \
                       that word, and stored hashes and the credentials inside a URL are blanked \
                       wherever they are. Both oxidized's \
                       filter and Yagra's own can miss a vendor spelling, so treat the text as \
                       sensitive. A diff shows changed lines with 3 lines of context. Pages: \
                       `limit` lines (default 500) from `offset`; `match` keeps only lines \
                       matching a regular expression; `truncated` true means more lines follow. \
                       `versions` needs view permission; the text views need manage-config, are \
                       audited per read, and answer only when an Admin has switched configuration \
                       text for AI clients on (shown as `mcp_config_text` in \
                       get_system_health(section=config_schedule)). Whether a node is backed up at \
                       all, and its last fetch, are in get_node_status."
    )]
    async fn get_node_config(
        &self,
        Parameters(p): Parameters<NodeConfigParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        const TOOL: &str = "get_node_config";
        let Some(view) = ConfigView::from_token(p.view.as_deref()) else {
            return tool_bad_params(
                TOOL,
                &format!(
                    "unknown view {:?}; must be text, diff, unsaved or versions",
                    p.view.as_deref().unwrap_or_default()
                ),
            );
        };
        let identity = identity_of(&ctx);
        let Some(authed) = authed_for(identity.clone(), view.permission()) else {
            if view.carries_text() {
                if let Some(id) = identity.as_ref() {
                    self.audit_config_read(id, p.node_id, view, 403).await;
                }
            }
            return tool_forbidden(
                TOOL,
                &format!(
                    "this token lacks {} permission",
                    permission_label(view.permission())
                ),
            );
        };
        if let Some(deny) = self
            .deny_invisible_node_for(Some(authed.clone()), TOOL, p.node_id)
            .await
        {
            // A refusal is a read attempt too (decision 86), and the row is how a scoped token
            // probing for nodes outside its folders shows up.
            if view.carries_text() {
                self.audit_config_read(&authed, p.node_id, view, 404).await;
            }
            return deny;
        }
        let node_id = p.node_id;
        let (result, status) = self.node_config_in(p, view).await;
        if view.carries_text() {
            self.audit_config_read(&authed, node_id, view, status).await;
        }
        result
    }

    /// One audit row per read of configuration text, refusals included (ADR-210 stage 10
    /// decision 86). `/mcp` does not pass through `audit_mw`, so the tool writes it.
    async fn audit_config_read(
        &self,
        identity: &crate::mcp::McpIdentity,
        node: Uuid,
        view: ConfigView,
        status: u16,
    ) {
        record_audit(
            &self.state,
            identity,
            &format!("mcp.get_node_config node={node} view={}", view.as_str()),
            status,
        )
        .await;
    }

    /// The body of `get_node_config` once the caller is authorized and the node visible, with the
    /// status its audit row records. Not reachable through `call_in`: the RCA agent is not offered
    /// configuration text (decision 88).
    pub(super) async fn node_config_in(
        &self,
        p: NodeConfigParams,
        view: ConfigView,
    ) -> (Result<CallToolResult, McpError>, u16) {
        const TOOL: &str = "get_node_config";
        let Some(admin) = self.state.admin.as_ref() else {
            return (
                tool_unavailable(TOOL, "config backup requires live mode"),
                503,
            );
        };
        let fail = |e: &crate::api::ApiError| (tool_api_error(TOOL, e), e.status().as_u16());
        if view == ConfigView::Versions {
            return match crate::api::config_backup::versions_of(admin, p.node_id).await {
                Ok(v) => (
                    ok_json(
                        TOOL,
                        &NodeConfigVersionsDto {
                            node_id: p.node_id,
                            versions: v
                                .into_iter()
                                .map(|v| ConfigVersionDto {
                                    version: v.oid,
                                    stored_at: v.stored_at,
                                })
                                .collect(),
                        },
                    ),
                    200,
                ),
                Err(e) => fail(&e),
            };
        }
        // Decision 84: the text views answer only while an Admin has switched them on.
        match admin.config_saves.mcp_config_text().await {
            Ok(s) if s.enabled => {}
            Ok(_) => {
                return (
                    tool_unavailable(
                        TOOL,
                        "configuration text is switched off for AI clients on this deployment; \
                         an Admin can switch it on under Config backups",
                    ),
                    403,
                )
            }
            Err(e) => {
                return (
                    tool_error(TOOL, "read the MCP config-text setting", &e),
                    500,
                )
            }
        }
        let pattern = match p.pattern.as_deref().map(config_text::compile_match) {
            None => None,
            Some(Ok(re)) => Some(re),
            Some(Err(why)) => return (tool_bad_params(TOOL, &why), 400),
        };
        let mut dto = NodeConfigTextDto {
            node_id: p.node_id,
            view: view.as_str(),
            version: None,
            from: None,
            to: None,
            since: None,
            checked_at: None,
            total_lines: 0,
            matched_lines: 0,
            offset: p.offset.unwrap_or(0),
            truncated: false,
            redacted_lines: 0,
            lines: Vec::new(),
        };
        let lines: Vec<(ConfigLineDto, bool)> = match view {
            // Answered above, before the switch: the list carries no text.
            ConfigView::Versions => return (no_versions(), 404),
            ConfigView::Text => {
                let oid = match p.version {
                    Some(v) => v,
                    None => match crate::api::config_backup::versions_of(admin, p.node_id).await {
                        Ok(v) => match v.into_iter().next() {
                            Some(latest) => latest.oid,
                            None => return (no_versions(), 404),
                        },
                        Err(e) => return fail(&e),
                    },
                };
                let text = match crate::api::config_backup::version_text_of(admin, p.node_id, &oid)
                    .await
                {
                    Ok(t) => t,
                    Err(e) => return fail(&e),
                };
                dto.version = Some(oid);
                numbered(config_text::redact_lines(text.lines()))
            }
            ConfigView::Unsaved => {
                let u = match crate::api::config_save::unsaved_of(admin, p.node_id).await {
                    Ok(u) => u,
                    Err(e) => return fail(&e),
                };
                dto.since = Some(u.since);
                dto.checked_at = Some(u.checked_at);
                numbered(config_text::redact_lines(u.text.lines()))
            }
            ConfigView::Diff => {
                let (from, to) =
                    if let (Some(f), Some(t)) = (p.from.clone(), p.to.clone()) {
                        (f, t)
                    } else {
                        let versions =
                            match crate::api::config_backup::versions_of(admin, p.node_id).await {
                                Ok(v) => v,
                                Err(e) => return fail(&e),
                            };
                        match pick_diff_pair(&versions, p.from.as_deref(), p.to.as_deref()) {
                            Some(pair) => pair,
                            None => return (
                                tool_unavailable(
                                    TOOL,
                                    "this node has fewer than two stored versions to compare, or \
                                     `to` is its oldest",
                                ),
                                404,
                            ),
                        }
                    };
                let old = match crate::api::config_backup::version_text_of(admin, p.node_id, &from)
                    .await
                {
                    Ok(t) => t,
                    Err(e) => return fail(&e),
                };
                let new =
                    match crate::api::config_backup::version_text_of(admin, p.node_id, &to).await {
                        Ok(t) => t,
                        Err(e) => return fail(&e),
                    };
                dto.from = Some(from);
                dto.to = Some(to);
                diff_lines(&old, &new)
            }
        };
        dto.total_lines = lines.len();
        dto.redacted_lines = lines.iter().filter(|(_, r)| *r).count();
        let page = config_text::page(
            lines.into_iter().map(|(l, _)| l).collect(),
            |l: &ConfigLineDto| l.text.as_str(),
            pattern.as_ref(),
            dto.offset,
            p.limit.unwrap_or(config_text::DEFAULT_LIMIT),
        );
        dto.matched_lines = page.matched;
        dto.truncated = page.truncated;
        dto.lines = page.lines;
        (ok_json(TOOL, &dto), 200)
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
        description = "When one node was down over the last `period` (24h, 7d, 30d by default, \
                       or 90d), and the share of the period it was up. Down means the node's \
                       liveness alert was open, so a blip shorter than its dwell is not counted. \
                       `spans` lists every down, maintenance and no_record stretch (Unix ms, \
                       oldest first); time no span covers was up. no_record is before the node \
                       was added, older than the alert-history retention, or while a poller of \
                       its pool could not reach core, and every quiet stretch while nothing would \
                       record an outage (no liveness rule reaches the node, or its pool has no \
                       live poller now) - never read it as up or as down. \
                       `availability_pct` is up / (up + down) and leaves maintenance and \
                       no_record out; `outages` counts how often the alert opened. If \
                       `truncated_before_unix_ms` is set, spans before it were dropped (totals \
                       still cover the whole period). Use get_alert_history for what fired."
    )]
    async fn get_node_availability(
        &self,
        Parameters(p): Parameters<NodeAvailabilityParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        const TOOL: &str = "get_node_availability";
        match self.admit(identity_of(&ctx), TOOL, "").await {
            Ok(scope) => self.node_availability_in(p, &scope).await,
            Err(refusal) => refusal,
        }
    }

    pub(super) async fn node_availability_in(
        &self,
        p: NodeAvailabilityParams,
        scope: &NodeScope,
    ) -> Result<CallToolResult, McpError> {
        const TOOL: &str = "get_node_availability";
        if let Some(deny) = deny_invisible_node(&self.state, scope, TOOL, p.node_id) {
            return deny;
        }
        let Some(admin) = self.state.admin.as_ref() else {
            return tool_unavailable(TOOL, "availability requires live mode");
        };
        let period = match crate::api::availability::parse_period(p.period.as_deref()) {
            Ok(period) => period,
            Err(e) => return tool_api_error(TOOL, &e),
        };
        match crate::api::availability::availability_of(
            &self.state,
            admin,
            p.node_id,
            period,
            chrono::Utc::now(),
        )
        .await
        {
            Ok(a) => ok_json(TOOL, &a),
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

    // ── get_node_config (ADR-210 stage 10) ──────────────────────────────────────────────────────

    #[test]
    fn a_config_view_names_its_permission_and_whether_it_carries_text() {
        assert_eq!(ConfigView::from_token(None), Some(ConfigView::Text));
        assert_eq!(ConfigView::from_token(Some("bogus")), None);
        for v in [
            ConfigView::Versions,
            ConfigView::Text,
            ConfigView::Diff,
            ConfigView::Unsaved,
        ] {
            assert_eq!(ConfigView::from_token(Some(v.as_str())), Some(v));
            // Text needs manage-config, as the REST routes that serve it do; the list does not.
            assert_eq!(
                v.permission() == Permission::ManageConfig,
                v.carries_text(),
                "{v:?}"
            );
        }
        assert_eq!(ConfigView::Versions.permission(), Permission::View);
    }

    fn version(oid: char) -> crate::api::config_backup::ConfigVersion {
        crate::api::config_backup::ConfigVersion {
            oid: oid.to_string().repeat(40),
            stored_at: None,
        }
    }

    #[test]
    fn a_diff_defaults_to_the_latest_and_the_version_before_it() {
        let v = [version('c'), version('b'), version('a')]; // newest first
        let oid = |c: char| c.to_string().repeat(40);
        assert_eq!(pick_diff_pair(&v, None, None), Some((oid('b'), oid('c'))));
        assert_eq!(
            pick_diff_pair(&v, None, Some(&oid('b'))),
            Some((oid('a'), oid('b')))
        );
        // `to` is the oldest: there is nothing before it.
        assert_eq!(pick_diff_pair(&v, None, Some(&oid('a'))), None);
        assert_eq!(pick_diff_pair(&v[..1], None, None), None);
        assert_eq!(pick_diff_pair(&[], None, None), None);
        assert_eq!(
            pick_diff_pair(&v, Some(&oid('a')), None),
            Some((oid('a'), oid('c')))
        );
    }

    #[test]
    fn a_diff_shows_a_changed_secret_as_changed_without_its_value() {
        let mut old: Vec<String> = (1..=20).map(|i| format!("line {i}")).collect();
        let mut new = old.clone();
        old[9] = "snmp-server community oldvalue RO".to_owned();
        new[9] = "snmp-server community newvalue RO".to_owned();
        let lines = diff_lines(&old.join("\n"), &new.join("\n"));
        let text: Vec<String> = lines.iter().map(|(l, _)| l.text.clone()).collect();
        assert!(
            text.iter()
                .all(|t| !t.contains("oldvalue") && !t.contains("newvalue")),
            "{text:?}"
        );
        let kinds: Vec<_> = lines.iter().map(|(l, _)| l.change.unwrap()).collect();
        assert!(kinds.contains(&"removed") && kinds.contains(&"added"));
        // Three lines of context either side of the change, and nothing further away.
        assert_eq!(lines.len(), 3 + 2 + 3, "{text:?}");
        assert!(text.contains(&"line 7".to_owned()) && !text.contains(&"line 6".to_owned()));
        assert_eq!(lines.iter().filter(|(_, r)| *r).count(), 2);
    }

    /// A lone `\r` cuts a line for the diff but not for `str::lines`; the blanking has to follow
    /// the diff's cut, or a key line is looked up by the wrong index and shown.
    #[test]
    fn a_diff_blanks_by_the_lines_it_compares() {
        let old =
            "a\n-----BEGIN PRIVATE KEY-----\nMIIEold\rMIIEold2\n-----END PRIVATE KEY-----\nb\n";
        let new =
            "a\n-----BEGIN PRIVATE KEY-----\nMIIEnew\rMIIEnew2\n-----END PRIVATE KEY-----\nb\n";
        let lines = diff_lines(old, new);
        assert!(!lines.is_empty());
        for (l, _) in &lines {
            assert!(!l.text.contains("MIIE"), "{:?}", l.text);
        }
    }

    #[test]
    fn identical_versions_diff_to_nothing() {
        assert!(diff_lines("a\nb\n", "a\nb\n").is_empty());
    }

    /// The deployment switch is what stands between an MCP client and configuration text (ADR-210
    /// stage 10 decision 85): off, every text view is refused before oxidized is asked; on, the
    /// same request goes on to oxidized (here a closed port, so 502). The list of versions carries
    /// no text and is not behind the switch.
    #[sqlx::test(migrator = "yagra_base::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn the_text_views_answer_only_while_the_switch_is_on(pool: sqlx::PgPool) {
        use yagra_base::config_save::McpConfigText;
        let home = std::env::temp_dir().join(format!("yagra-ox-mcp-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&home).expect("dir");
        let st = crate::api::tests_support::live_state_with_oxidized(
            pool,
            std::sync::Arc::new(crate::api::config_backup::OxidizedHandle {
                settings: crate::oxidized::Settings {
                    url: "http://127.0.0.1:9".to_owned(),
                    home: home.clone(),
                },
                client: std::sync::Arc::new(crate::oxidized::client::OxidizedClient::new(
                    "http://127.0.0.1:9",
                )),
            }),
        )
        .await;
        let admin = st.admin.clone().expect("live mode");
        let mcp = YagraMcp::new(st);
        let params = |view: &str| -> NodeConfigParams {
            serde_json::from_value(serde_json::json!({
                "node_id": uuid::Uuid::new_v4(),
                "view": view,
                "version": "a".repeat(40),
            }))
            .expect("params")
        };
        for view in [ConfigView::Text, ConfigView::Diff, ConfigView::Unsaved] {
            let (r, status) = mcp.node_config_in(params(view.as_str()), view).await;
            assert_eq!(status, 403, "{view:?} while off");
            let text = text_of(&r.expect("answered"));
            assert!(text.contains("switched off"), "{view:?}: {text}");
        }
        // The list is View and textless: it reaches oxidized whatever the switch says.
        let (_, status) = mcp
            .node_config_in(params("versions"), ConfigView::Versions)
            .await;
        assert_ne!(status, 403, "versions is not behind the switch");

        admin
            .config_saves
            .set_mcp_config_text(McpConfigText { enabled: true })
            .await
            .expect("switched on");
        let (_, status) = mcp.node_config_in(params("text"), ConfigView::Text).await;
        assert_eq!(status, 502, "on: the request reaches oxidized");
        std::fs::remove_dir_all(&home).ok();
    }

    /// With no write side there is no config backup to read, and no text leaves.
    #[tokio::test]
    async fn get_node_config_needs_live_mode() {
        let p: NodeConfigParams =
            serde_json::from_value(serde_json::json!({ "node_id": uuid::Uuid::nil() }))
                .expect("params");
        let (r, status) = mcp().node_config_in(p, ConfigView::Text).await;
        assert_eq!(status, 503);
        assert_eq!(json_of(&r.expect("ok"))["available"], false);
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
            fleet_summary_kind(Some("watch")),
            Some(FleetSummaryKind::Watch)
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

    /// Both surfaces answer "is SNMP configured for this node?", and answer it the same way —
    /// **including on the branch a credential cannot see**.
    ///
    /// `snmp_configured` is the second axis the node detail hides its SNMP-fed tabs on (ADR-119).
    /// It shipped on `GET /nodes/{node_id}` alone, so `get_node_status` — the tool the route ledger
    /// says folds that route — could not answer a question the WebUI could, which is the
    /// read-parity rule in `api-conventions.md`. That failure is silent by construction: the
    /// ledger's `Mcp::Tool` column asserts the tool *exists*, never that it gives the same answer.
    ///
    /// 🚨 **The whole test is the second half.** A node with no bound credential on a deployment
    /// with no fallback community answers `false` under *either* implementation — the correct one
    /// and a re-derivation from `node.credential` — so a version of this test with only that case
    /// passes with the bug in place. It was written that way first and **verified to pass while
    /// the tool derived the value from `node.credential`**, which is exactly the shape of a check
    /// that looks like coverage and is not. The community case is what separates them: the same
    /// node, no credential, and the answer must flip to `true` on both surfaces.
    ///
    /// It also drives real HTTP and the real tool body over one live database rather than
    /// comparing source text, because the offence is a value and not a spelling.
    #[sqlx::test(migrator = "yagra_base::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn both_surfaces_agree_on_whether_snmp_is_configured(pool: sqlx::PgPool) {
        use crate::api::tests_support::{live_state_with_env_community, send, token};

        // One node, no bound credential, asked of two deployments that differ in one thing.
        for (community, expected) in [(None, false), (Some("public".to_owned()), true)] {
            let st = live_state_with_env_community(pool.clone(), community.clone()).await;
            let tok = token(&st, yagra_common::Role::Admin);
            let label = format!("env community {community:?}");

            let (status, created) = send(
                &st,
                "POST",
                "/api/v1/nodes",
                &tok,
                Some(serde_json::json!({ "name": "ping-only-01", "address": "10.0.0.9" })),
            )
            .await;
            assert_eq!(
                status,
                axum::http::StatusCode::CREATED,
                "{label}: {created}"
            );
            let node_id: Uuid = serde_json::from_value(created["id"].clone())
                .unwrap_or_else(|e| panic!("{label}: the create response carries an id: {e}"));

            let (status, detail) =
                send(&st, "GET", &format!("/api/v1/nodes/{node_id}"), &tok, None).await;
            assert_eq!(status, axum::http::StatusCode::OK, "{label}: {detail}");
            let rest = detail
                .get("snmp_configured")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or_else(|| panic!("{label}: REST still carries snmp_configured: {detail}"));

            let r = YagraMcp::new(st.clone())
                .node_status_in(NodeIdParams { node_id }, &unrestricted())
                .await
                .expect("ok result");
            let tool = json_of(&r);
            let mcp = tool
                .get("snmp_configured")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or_else(|| {
                    panic!("{label}: the tool still carries snmp_configured: {tool}")
                });

            // Both directions, so neither surface can pass by being hardcoded: they must agree
            // with each other AND both must follow the deployment's community.
            assert_eq!(
                rest, expected,
                "{label}: REST must follow the fallback community, not the node's credential"
            );
            assert_eq!(
                mcp, expected,
                "{label}: get_node_status folds GET /nodes/:node_id and must answer the same"
            );

            // The row is per-iteration state; the next deployment starts from an empty inventory.
            let (status, body) = send(
                &st,
                "DELETE",
                &format!("/api/v1/nodes/{node_id}"),
                &tok,
                None,
            )
            .await;
            assert!(status.is_success(), "{label}: cleanup delete: {body}");
        }
    }

    /// Both surfaces report the same note (ADR-135), because this tool folds `GET /nodes/:node_id`.
    ///
    /// 🚨 **The note has to have content, and that is the whole design of this test.** A node with
    /// no note answers `null` on both surfaces under *any* implementation — including one where the
    /// tool never learned about the column at all. So the interesting assertion is the second pair,
    /// after something is written; the `null` pair is only here to prove the field exists on both
    /// before there is anything to carry (`consistent-state-is-weaker-than-a-transition`).
    #[sqlx::test(migrator = "yagra_base::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn both_surfaces_agree_on_a_nodes_note(pool: sqlx::PgPool) {
        use crate::api::tests_support::{live_state, send, token};
        let st = live_state(pool.clone()).await;
        let tok = token(&st, yagra_common::Role::Admin);
        let node_id = yagra_base::pgtest::node(&pool, "sw-1", 1, None).await;

        // Before: both say "no note", which on its own proves nothing.
        let (_, detail) = send(&st, "GET", &format!("/api/v1/nodes/{node_id}"), &tok, None).await;
        assert_eq!(detail["notes"], serde_json::Value::Null, "{detail}");
        let r = YagraMcp::new(st.clone())
            .node_status_in(NodeIdParams { node_id }, &unrestricted())
            .await
            .expect("ok result");
        assert_eq!(json_of(&r)["notes"], serde_json::Value::Null);

        const NOTE: &str = "flaps on purpose: the carrier reprovisions this circuit nightly";
        let (status, body) = send(
            &st,
            "PUT",
            &format!("/api/v1/nodes/{node_id}/bindings"),
            &tok,
            Some(serde_json::json!({ "notes": NOTE })),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::NO_CONTENT, "{body}");

        // After: the two must carry the same text. This is the pair that separates a tool which
        // reads the column from one which does not.
        let (_, detail) = send(&st, "GET", &format!("/api/v1/nodes/{node_id}"), &tok, None).await;
        assert_eq!(detail["notes"], NOTE, "REST lost the note: {detail}");
        let r = YagraMcp::new(st.clone())
            .node_status_in(NodeIdParams { node_id }, &unrestricted())
            .await
            .expect("ok result");
        let tool = json_of(&r);
        assert_eq!(
            tool["notes"], NOTE,
            "get_node_status folds GET /nodes/:node_id and must answer the same: {tool}"
        );
    }

    /// Both surfaces report the same OS version (ADR-138), because this tool folds
    /// `GET /nodes/:node_id`.
    ///
    /// 🚨 Same shape as the note test above and for the same reason: `null` on both sides before
    /// anything is recorded is true of a tool that never learned the column, so the pair after the
    /// write is the one that proves the fold.
    #[sqlx::test(migrator = "yagra_base::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn both_surfaces_agree_on_a_nodes_os_version(pool: sqlx::PgPool) {
        use crate::api::tests_support::{live_state, send, token};
        let st = live_state(pool.clone()).await;
        let tok = token(&st, yagra_common::Role::Admin);
        let node_id = yagra_base::pgtest::node(&pool, "sw-1", 1, None).await;

        let r = YagraMcp::new(st.clone())
            .node_status_in(NodeIdParams { node_id }, &unrestricted())
            .await
            .expect("ok result");
        assert_eq!(json_of(&r)["os_version"], serde_json::Value::Null);

        const VERSION: &str = "10.5(2)";
        yagra_base::pgtest::repo(pool.clone())
            .update_os_version_batch(&[(node_id, VERSION.to_owned())])
            .await
            .expect("record a version");

        let (_, detail) = send(&st, "GET", &format!("/api/v1/nodes/{node_id}"), &tok, None).await;
        assert_eq!(
            detail["os_version"], VERSION,
            "REST lost the version: {detail}"
        );
        let r = YagraMcp::new(st.clone())
            .node_status_in(NodeIdParams { node_id }, &unrestricted())
            .await
            .expect("ok result");
        let tool = json_of(&r);
        assert_eq!(
            tool["os_version"], VERSION,
            "get_node_status folds GET /nodes/:node_id and must answer the same: {tool}"
        );
    }

    /// Both surfaces report the same serial number (ADR-147), for the reason the version test above
    /// gives — the pair after the write is what proves the fold.
    #[sqlx::test(migrator = "yagra_base::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn both_surfaces_agree_on_a_nodes_serial_number(pool: sqlx::PgPool) {
        use crate::api::tests_support::{live_state, send, token};
        let st = live_state(pool.clone()).await;
        let tok = token(&st, yagra_common::Role::Admin);
        let node_id = yagra_base::pgtest::node(&pool, "sw-stack", 1, None).await;

        let r = YagraMcp::new(st.clone())
            .node_status_in(NodeIdParams { node_id }, &unrestricted())
            .await
            .expect("ok result");
        assert_eq!(json_of(&r)["serial_number"], serde_json::Value::Null);

        const SERIAL: &str = "FCW1929B68S, FCW1931A06Z, FCW1929B6BP";
        yagra_base::pgtest::repo(pool.clone())
            .update_serial_number_batch(&[(node_id, SERIAL.to_owned())])
            .await
            .expect("record a serial");

        let (_, detail) = send(&st, "GET", &format!("/api/v1/nodes/{node_id}"), &tok, None).await;
        assert_eq!(
            detail["serial_number"], SERIAL,
            "REST lost the serial: {detail}"
        );
        let r = YagraMcp::new(st.clone())
            .node_status_in(NodeIdParams { node_id }, &unrestricted())
            .await
            .expect("ok result");
        let tool = json_of(&r);
        assert_eq!(
            tool["serial_number"], SERIAL,
            "get_node_status folds GET /nodes/:node_id and must answer the same: {tool}"
        );
    }

    /// `get_prefix_gaps` withholds another folder's range from a scoped caller exactly as
    /// `GET /node-groups/:id/prefix-gaps` does (ADR-170, ADR-014), and a folder outside the scope
    /// answers "not available" rather than its report. The unrestricted call first, so the
    /// withheld fields are known to exist when there is nothing to hide.
    #[sqlx::test(migrator = "yagra_base::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_scoped_caller_is_not_told_whose_range_a_gap_falls_in(pool: sqlx::PgPool) {
        use crate::api::tests_support::{live_state, send, token};
        let st = live_state(pool.clone()).await;
        let tok = token(&st, yagra_common::Role::Admin);
        let folder = |name: &'static str| {
            let st = st.clone();
            let tok = tok.clone();
            async move {
                let (_, body) = send(
                    &st,
                    "POST",
                    "/api/v1/node-groups",
                    &tok,
                    Some(serde_json::json!({ "name": name, "group_type": "site" })),
                )
                .await;
                body["id"]
                    .as_str()
                    .expect("id")
                    .parse::<Uuid>()
                    .expect("uuid")
            }
        };
        let site = folder("site-a").await;
        let other = folder("site-b").await;
        yagra_base::pgtest::prefix(&pool, other, "10.9.0.0/24").await;
        let node = yagra_base::pgtest::node(&pool, "cs-a", 1, Some(site)).await;
        crate::topology::l3::L3Repo::new(pool.clone())
            .record_observation(
                node,
                &yagra_common::L3Snapshot::new(vec![yagra_common::L3Address::new(
                    1,
                    "10.9.0.5".parse().expect("ip"),
                    24,
                )]),
            )
            .await
            .expect("record");

        let tool = YagraMcp::new(st.clone());
        let gap = |r: &CallToolResult| json_of(r)["gaps"][0].clone();

        let r = tool
            .prefix_gaps_in(PrefixGapsParams { group_id: site }, &unrestricted())
            .await
            .expect("ok result");
        let open = gap(&r);
        assert_eq!(open["kind"], "other_folder", "{open}");
        assert_eq!(open["range_group"], other.to_string(), "{open}");
        assert_eq!(open["range_group_name"], "site-b", "{open}");

        let scoped = NodeScope::Groups(std::sync::Arc::new(crate::api::scope::ScopeSet {
            visible: crate::api::scope::subtree_of(&st, site)
                .await
                .expect("subtree"),
            breadcrumb: Vec::new(),
        }));
        let r = tool
            .prefix_gaps_in(PrefixGapsParams { group_id: site }, &scoped)
            .await
            .expect("ok result");
        let hidden = gap(&r);
        assert_eq!(hidden["kind"], "other_folder", "{hidden}");
        assert!(hidden["range"].is_null(), "range withheld: {hidden}");
        assert!(hidden["range_group"].is_null(), "folder withheld: {hidden}");
        assert!(
            hidden["range_group_name"].is_null(),
            "name withheld: {hidden}"
        );

        let r = tool
            .prefix_gaps_in(PrefixGapsParams { group_id: other }, &scoped)
            .await
            .expect("ok result");
        assert_eq!(json_of(&r)["available"], false, "{}", text_of(&r));
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
