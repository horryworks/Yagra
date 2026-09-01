// SPDX-License-Identifier: AGPL-3.0-only
//! MCP tools: what arrived on its own — syslog, traps, webhooks — and the traffic flows beside them (ADR-086).
//!
//! Split out of the single `tools.rs` by ADR-086; the module doc for the surface as a whole,
//! and the rules every tool here obeys, are in [`super`].

use chrono::{DateTime, Utc};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
// The module (not just the trait) — the `JsonSchema` derive expands to `schemars::…` paths, so the
// `schemars` name must be in scope. rmcp re-exports it, keeping exactly one schemars version.
use rmcp::schemars;
use rmcp::service::{RequestContext, RoleServer};
use rmcp::{tool, tool_router, ErrorData as McpError};
use serde::Deserialize;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use uuid::Uuid;
use yagra_common::NodeId;

use super::YagraMcp;
use crate::api::scope::NodeScope;
use crate::flowstore::{AsDir, FlowQuery};
use crate::mcp::dto::EventDto;

// The shared scope: the helpers in `support.rs` and the types the other domain modules declare,
// re-exported by `mod.rs` so no file has to name where a sibling keeps a thing.
use super::*;

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(super) struct TopFlowsParams {
    /// The exporter node's UUID. Omit for the whole fleet (top_flows only; flow_fanout requires it).
    pub(super) node_id: Option<Uuid>,
    /// Aggregation: talkers, conversations, ports, protocols, or as (default talkers).
    pub(super) kind: Option<String>,
    /// Window start, Unix seconds (default: one hour ago).
    pub(super) from: Option<i64>,
    /// Window end, Unix seconds (default: now).
    pub(super) to: Option<i64>,
    /// Max rows to return (clamped by the store).
    pub(super) limit: Option<u32>,
    /// Optional IP-protocol filter (e.g. 6 = TCP, 17 = UDP).
    pub(super) proto: Option<u8>,
    /// Optional destination-port filter.
    pub(super) port: Option<u16>,
    /// Optional peer filter — an IP address that must be the source or destination.
    pub(super) peer: Option<String>,
    /// Optional AS filter — an ASN that must be the source or destination AS.
    pub(super) asn: Option<u32>,
    /// For kind=as, which side to aggregate: src or dst (default dst).
    pub(super) dir: Option<String>,
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

/// Build a [`FlowQuery`] from the shared flow-tool params: typed drill-down filters (an unparseable
/// `peer` is ignored, never interpolated). Shared by `top_flows` and `flow_fanout`.
///
/// ⚠️ **The tool params stay single-valued while REST took sets** (ADR-053 Inc.8). That is a
/// deliberate asymmetry, not an oversight: parity is about *which questions can be answered*, and a
/// model that wants two protocols asks twice — where the WebUI needs one control that can say
/// "TCP and UDP" because an operator ticking two boxes and getting one is the failure the whole ADR
/// is about. Widening these means widening the JSON-schema params a model reads, which is a change
/// to published vocabulary; do it when a model actually needs it, not for symmetry.
///
/// The window and the row limit come from [`crate::api::flow::flow_window`], which is the REST
/// edge's rule. This used to be a hand copy with `limit.unwrap_or(100)` and **no clamp**, while the
/// REST side clamped to `1..=1000` and its own test calls an unbounded top-N a DoS vector — the
/// surface with no human in the loop was the one without the cap. The default of 100 is kept: a
/// model orienting itself wants more rows than a dashboard table does, and that difference is a
/// choice rather than a drift.
pub(super) fn flow_query_from(p: &TopFlowsParams) -> FlowQuery {
    let (from_unix_ms, to_unix_ms, limit) =
        crate::api::flow::flow_window(p.from, p.to, p.limit, 100);
    let peer: Option<std::net::IpAddr> = p.peer.as_deref().and_then(|s| s.parse().ok());
    FlowQuery {
        node_id: p.node_id,
        from_unix_ms,
        to_unix_ms,
        limit,
        proto: p.proto.into_iter().collect(),
        dst_port: p.port.into_iter().collect(),
        peer: peer.into_iter().collect(),
        asn: p.asn.into_iter().collect(),
    }
}

#[tool_router(router = events_router, vis = "pub(super)")]
impl YagraMcp {
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
        const TOOL: &str = "top_flows";
        match self.scope_for(identity_of(&ctx)).await {
            Ok(scope) => self.top_flows_in(p, &scope).await,
            Err(e) => tool_api_error(TOOL, &e),
        }
    }

    pub(super) async fn top_flows_in(
        &self,
        p: TopFlowsParams,
        scope: &NodeScope,
    ) -> Result<CallToolResult, McpError> {
        const TOOL: &str = "top_flows";
        match p.node_id {
            Some(node_id) => {
                if let Some(deny) = deny_invisible_node(&self.state, scope, TOOL, node_id) {
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
                    return tool_api_error(TOOL, &e);
                }
            }
        }
        // The six-arm dispatch, the AS-name fill and the store's availability gate all live in
        // `api::flow` now. This tool used to re-implement all three, and that copy had lost the
        // limit clamp.
        let agg = match crate::api::flow::FlowAgg::parse(p.kind.as_deref().unwrap_or("talkers")) {
            Ok(a) => a,
            Err(e) => return tool_api_error(TOOL, &e),
        };
        let dir = if p.dir.as_deref() == Some("src") {
            AsDir::Src
        } else {
            AsDir::Dst
        };
        match crate::api::flow::flow_agg_rows(&self.state, &flow_query_from(&p), dir, agg).await {
            Ok(rows) => ok_json(TOOL, &rows),
            Err(e) => tool_api_error(TOOL, &e),
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
        const TOOL: &str = "flow_fanout";
        match self.scope_for(identity_of(&ctx)).await {
            Ok(scope) => self.flow_fanout_in(p, &scope).await,
            Err(e) => tool_api_error(TOOL, &e),
        }
    }

    pub(super) async fn flow_fanout_in(
        &self,
        p: TopFlowsParams,
        scope: &NodeScope,
    ) -> Result<CallToolResult, McpError> {
        const TOOL: &str = "flow_fanout";
        // Node-scoped only, deliberately. `FlowQuery.node_id` makes a fleet-wide form structurally
        // possible, but `fanout_by_src` groups by source, so across exporters a flow seen twice is
        // counted twice and "distinct destinations contacted" inflates by an amount nobody can
        // bound. There is no REST counterpart to compare a fleet answer against either, so it would
        // be an MCP-only capability with a known correctness caveat.
        let Some(node_id) = p.node_id else {
            return tool_bad_params(
                TOOL,
                "`node_id` is required: fan-out is counted per source, so a fleet-wide form would \
                 double-count any flow two exporters both saw. Query one exporter at a time.",
            );
        };
        if let Some(deny) = deny_invisible_node(&self.state, scope, TOOL, node_id) {
            return deny;
        }
        let Some(flows) = self.state.flows.as_ref() else {
            return tool_unavailable(TOOL, "flow tier not enabled on this core");
        };
        match flows.fanout_by_src(&flow_query_from(&p)).await {
            Ok(rows) => ok_json(TOOL, &rows),
            Err(e) => tool_error(TOOL, "query flow fan-out", &e),
        }
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
        const TOOL: &str = "search_events";
        match self.scope_for(identity_of(&ctx)).await {
            Ok(scope) => self.search_events_in(p, &scope).await,
            Err(e) => tool_api_error(TOOL, &e),
        }
    }

    pub(crate) async fn search_events_in(
        &self,
        p: EventSearchParams,
        scope: &NodeScope,
    ) -> Result<CallToolResult, McpError> {
        const TOOL: &str = "search_events";
        let Some(admin) = self.state.admin.as_ref() else {
            return tool_unavailable(TOOL, "event search requires live mode");
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
                Err(e) => return tool_api_error(TOOL, &e),
            };
        let limit = p.limit.unwrap_or(100).clamp(1, 500);
        // Same store routing too, including resolving a node-name term to ids so the name never
        // enters the log store (ADR-011).
        let rows =
            match crate::api::eventlog::search(&self.state, admin, scope, &filter, limit).await {
                Ok(r) => r,
                Err(e) => return tool_api_error(TOOL, &e),
            };
        let names = self
            .resolve_names(scope, rows.iter().filter_map(|r| r.node_id))
            .await;
        let out: Vec<EventDto> = rows
            .iter()
            .map(|r| EventDto::from_row(r, r.node_id.and_then(|id| names.get(&id).cloned())))
            .collect();
        ok_json(TOOL, &out)
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
        const TOOL: &str = "event_stats";
        match self.scope_for(identity_of(&ctx)).await {
            Ok(scope) => self.event_stats_in(p, &scope).await,
            Err(e) => tool_api_error(TOOL, &e),
        }
    }

    pub(crate) async fn event_stats_in(
        &self,
        p: EventStatsParams,
        scope: &NodeScope,
    ) -> Result<CallToolResult, McpError> {
        const TOOL: &str = "event_stats";
        let Some(admin) = self.state.admin.as_ref() else {
            return tool_unavailable(TOOL, "event stats requires live mode");
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
            Err(e) => return tool_error(TOOL, "event volume", &e),
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
        ok_json_value(TOOL, body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::tools::testkit::*;

    // The two RFC 3339 parsing tests that were here moved with the code they covered:
    // offset-applied-and-malformed-rejected is `api::util::parse_rfc3339`'s test, and
    // absent-vs-malformed is pinned by `api::eventlog`'s filter tests. Keeping copies here would
    // have tested this surface's *former* parser rather than the one it now calls.

    // ── Flow query construction (the injection-relevant one) ─────────────────────────────────────

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
        // A single tool value becomes a one-element set: `FlowQuery`'s filters are `Vec`s since
        // ADR-053 Inc.8, but the tool's params stay single-valued on purpose (see `flow_query_from`).
        assert_eq!(
            (q.limit, q.proto, q.dst_port, q.asn),
            (7, vec![6], vec![443], vec![15169])
        );

        // And an unset tool param is the empty set, i.e. no filter — not a filter on nothing.
        let none = flow_query_from(&flow_params(Uuid::new_v4()));
        assert!(none.proto.is_empty() && none.dst_port.is_empty() && none.asn.is_empty());
    }

    /// `peer` is the only free-text flow filter, and ClickHouse SQL interpolates it. It must reach
    /// the query as a typed `IpAddr` or not at all — a string that isn't an address is dropped, so
    /// nothing an MCP client sends can be interpolated verbatim.
    #[test]
    fn flow_query_drops_an_unparseable_peer_rather_than_passing_it_through() {
        let mut p = flow_params(Uuid::new_v4());
        p.peer = Some(r#"' OR 1=1 --"#.to_owned());
        assert!(
            flow_query_from(&p).peer.is_empty(),
            "junk peer is dropped, never interpolated"
        );

        p.peer = Some("2001:db8::1".to_owned());
        assert_eq!(
            flow_query_from(&p).peer,
            vec!["2001:db8::1".parse::<std::net::IpAddr>().unwrap()],
            "a valid v6 address survives as a typed value"
        );

        p.peer = Some("8.8.8.8".to_owned());
        assert_eq!(
            flow_query_from(&p).peer,
            vec!["8.8.8.8".parse::<std::net::IpAddr>().unwrap()]
        );
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

    /// Fan-out has no fleet-wide form, and the refusal says why rather than just failing.
    #[tokio::test]
    async fn flow_fanout_requires_a_node() {
        let mut p = flow_params(Uuid::nil());
        p.node_id = None;
        assert!(mcp().flow_fanout_in(p, &unrestricted()).await.is_err());
    }
}
