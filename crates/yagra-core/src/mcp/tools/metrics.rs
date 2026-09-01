// SPDX-License-Identifier: AGPL-3.0-only
//! MCP tools: numbers: one node, one port, or the worst of the fleet (ADR-086).
//!
//! Split out of the single `tools.rs` by ADR-086; the module doc for the surface as a whole,
//! and the rules every tool here obeys, are in [`super`].

use chrono::Utc;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
// The module (not just the trait) — the `JsonSchema` derive expands to `schemars::…` paths, so the
// `schemars` name must be in scope. rmcp re-exports it, keeping exactly one schemars version.
use crate::api::metrics::MetricDimension;
use rmcp::schemars;
use rmcp::service::{RequestContext, RoleServer};
use rmcp::{tool, tool_router, ErrorData as McpError};
use serde::Deserialize;
use uuid::Uuid;
use yagra_common::{MetricKind, NodeId, SeriesKey};

use super::YagraMcp;
use crate::api::scope::NodeScope;
use crate::mcp::dto::{MetricPointDto, MetricSeriesDto};

// The shared scope: the helpers in `support.rs` and the types the other domain modules declare,
// re-exported by `mod.rs` so no file has to name where a sibling keeps a thing.
use super::*;

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
pub(super) struct InterfaceThresholdsParams {
    /// The node's UUID.
    node_id: Uuid,
    /// SNMP ifIndex of the interface (from get_node_status's `interfaces`).
    ifindex: u32,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(super) struct InterfaceSeriesParams {
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
pub(super) struct TopMetricsParams {
    /// Metric name (icmp_rtt_ms, …) or a logical alias: cpu, memory.
    metric: String,
    /// now (default) ⇒ most recent value; max_1h ⇒ trailing-hour peak.
    agg: Option<String>,
    /// Max nodes to return (1–50, default 5).
    limit: Option<usize>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(super) struct TopInterfacesParams {
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
pub(super) struct FleetThroughputParams {
    /// Window start, Unix seconds (default: 24 hours ago).
    from: Option<i64>,
    /// Window end, Unix seconds (default: now).
    to: Option<i64>,
    /// Sample step in seconds (clamped, minimum 60; default 300).
    step: Option<u64>,
}

/// A bad-parameter error (records `bad_params`). Maps to a JSON-RPC invalid-params error.
/// What a node-level read of one metric can honestly answer (ADR-042 I4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum NodeRead {
    /// One series per node: read it as it is stored.
    Direct,
    /// Several series share the name on this node and they are gauges — collapse to the node
    /// maximum, the same reading `?agg=max` gives the WebUI's Device-health cards.
    NodeMax,
    /// Several series, and they are counters. There is no node-level number to give: a sum
    /// double-counts traffic that entered one port and left another, and a maximum invents a
    /// figure no surface displays. Say so and name where the answer lives.
    Refuse,
}

/// The rule, kept pure so it is testable without a store.
///
/// ⚠️ **This is a second statement of `metricView` in `web/src/lib/metricInventory.ts`**, and
/// nothing compares the two — the TS side decides what the Collection tab draws, this one decides
/// what `/mcp` answers. They agree on the load-bearing half (a multi-series counter has no
/// node-level read; a multi-series gauge is a node max) and that agreement is the whole point of
/// ADR-042: the same question must not get two answers. Change one, change both.
pub(super) fn node_read(kind: MetricKind, dimension: MetricDimension) -> NodeRead {
    match dimension {
        MetricDimension::None => NodeRead::Direct,
        MetricDimension::Interface | MetricDimension::Entity => match kind {
            MetricKind::Gauge => NodeRead::NodeMax,
            MetricKind::Counter => NodeRead::Refuse,
        },
    }
}

/// The word for a dimension in a sentence addressed to a model.
pub(super) fn dimension_word(dimension: MetricDimension) -> &'static str {
    match dimension {
        MetricDimension::None => "node",
        MetricDimension::Interface => "interface",
        MetricDimension::Entity => "table-row",
    }
}

/// Why a metric has no node-level answer, and where the answer is instead.
///
/// The destination matters more than the refusal: a model told only "no" retries or guesses, while
/// one handed the next tool name proceeds. An `entity` counter genuinely has nowhere to go — row
/// identity is discarded at collection time — and saying that plainly is better than inventing a
/// destination that will not work.
pub(super) fn no_node_level_answer(
    metric: &str,
    entry: &crate::api::metrics::NodeMetricEntry,
) -> String {
    let n = entry.series_count;
    match entry.dimension {
        MetricDimension::Interface => format!(
            "{metric} is a counter with one series per interface on this node ({n} of them), so no \
             single node-level value exists. Use get_interface_series for one interface's rates \
             (get_node_status lists the ifindexes), or top_interfaces to rank the fleet."
        ),
        MetricDimension::Entity => format!(
            "{metric} is a counter with one series per table row on this node ({n} of them), and \
             row identity is not retained at collection time, so neither a per-row nor a \
             node-level rate is available."
        ),
        // Unreachable via `node_read`, and written out rather than `unreachable!()` so a future
        // dimension cannot turn a wrong answer into a panic on a live deployment.
        MetricDimension::None => {
            format!("{metric} has no node-level answer available.")
        }
    }
}

#[tool_router(router = metrics_router, vis = "pub(super)")]
impl YagraMcp {
    #[tool(
        description = "Query a node's metric time-series from the TSDB. `metric` is a name such as \
                       icmp_rtt_ms, cpu_percent, or mem_percent. `mode` is latest|range|rate \
                       (default latest). For range/rate, `from`/`to` are Unix seconds (default: last \
                       hour) and `step` is the sample interval in seconds (clamped).\n\n\
                       This answers at the NODE level, so it depends on the metric's `dimension` \
                       (call list_node_metrics first). `none` is answered directly. A gauge with \
                       several series per node (`entity`, `interface`) is collapsed to the node \
                       maximum and the answer says so. A COUNTER with several series per node is \
                       refused rather than answered, because no single node-level number exists — \
                       use get_interface_series for one interface's rates, or top_interfaces to \
                       rank the fleet."
    )]
    async fn query_metrics(
        &self,
        Parameters(p): Parameters<QueryMetricsParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        const TOOL: &str = "query_metrics";
        match self.scope_for(identity_of(&ctx)).await {
            Ok(scope) => self.query_metrics_in(p, &scope).await,
            Err(e) => tool_api_error(TOOL, &e),
        }
    }

    pub(crate) async fn query_metrics_in(
        &self,
        p: QueryMetricsParams,
        scope: &NodeScope,
    ) -> Result<CallToolResult, McpError> {
        const TOOL: &str = "query_metrics";
        // Inside the split, not in the wrapper: a metric name is interpolated into a TSDB query, and
        // the in-process caller (ADR-028 WS-G) needs the same edge validation the session one gets.
        if !crate::api::is_valid_metric_name(&p.metric) {
            return tool_bad_params(TOOL, "invalid metric name");
        }
        // A series is node data. The TSDB has never heard of groups, so this is the only place the
        // question can be asked — and it is the same "no node with that id" a miss gets.
        if !scope.allows_node(&self.state, NodeId::from(p.node_id)) {
            return tool_unavailable(TOOL, "no node with that id");
        }
        // How many series share this name on this node, and what they are. Without this the node
        // selector matches every one of them and the store answers with the FIRST — an arbitrary
        // interface's rate presented as the node's (ADR-042 I4). The inventory is the same seam
        // `list_node_metrics` reads, so the two tools cannot come to disagree about a metric's
        // shape. Skeleton mode has no inventory and no multi-series data either, so it reads
        // directly, exactly as before.
        let entry = match self.state.admin.as_ref() {
            Some(admin) => crate::api::metrics::node_metric_inventory(
                admin,
                self.state.store.as_ref(),
                p.node_id,
                None,
            )
            .await
            .unwrap_or_default()
            .into_iter()
            .find(|e| e.metric == p.metric),
            None => None,
        };
        let read = entry
            .as_ref()
            .map_or(NodeRead::Direct, |e| node_read(e.metric_kind, e.dimension));
        if read == NodeRead::Refuse {
            let e = entry
                .as_ref()
                .expect("Refuse is only reachable with an entry");
            return tool_bad_params(TOOL, &no_node_level_answer(&p.metric, e));
        }

        let key = SeriesKey::node(NodeId::from(p.node_id), p.metric.clone());
        let mode = p.mode.as_deref().unwrap_or("latest");
        let agg = read == NodeRead::NodeMax;
        // Said out loud, never inferred: a collapsed answer that reads like a plain one is the same
        // class of wrong as the bug this fixes.
        //
        // The second clause names the direction because "maximum" is not self-evidently the
        // interesting end. An optical receive level (ADR-062) is the case that makes it obvious:
        // the maximum across a node's ports is its brightest link, so a fleet with one dying fibre
        // and seven healthy ones reports the healthy figure. The wording stays generic rather than
        // sniffing the metric name — `node_read` is a mirror of `metricView` in
        // `web/src/lib/metricInventory.ts` and the *rule* must keep answering what the WebUI's
        // Collection tab answers; only the sentence describing it is ours to sharpen.
        let note = entry.as_ref().filter(|_| agg).map(|e| {
            format!(
                "collapsed the node's {} {} series to their maximum — the highest value, so for a \
                 metric where low is the fault (an optical receive level) this is the healthiest \
                 series rather than the worst; use get_interface_series for one interface",
                e.series_count,
                dimension_word(e.dimension)
            )
        });
        let dto = match mode {
            "latest" => MetricSeriesDto {
                node_id: p.node_id,
                metric: p.metric.clone(),
                mode: "latest".to_owned(),
                latest: if agg {
                    self.state.store.aggregate_latest(&key).await
                } else {
                    self.state.store.latest(&key).await
                },
                points: Vec::new(),
                note,
            },
            // A `NodeMax` metric is a gauge by construction (a multi-series counter was refused
            // above), so `rate` of one is meaningless twice over — and there is no aggregate-rate
            // query to serve it with. Refusing beats quietly rating one arbitrary series.
            "rate" if agg => {
                return tool_bad_params(
                    TOOL,
                    &format!(
                        "{} is a gauge with several series on this node; `rate` has no node-level \
                         meaning here. Use mode=range for the node maximum over time.",
                        p.metric
                    ),
                )
            }
            "range" | "rate" => {
                let to = p.to.unwrap_or_else(|| Utc::now().timestamp());
                let from = p.from.unwrap_or(to - DEFAULT_WINDOW_SECS);
                if from >= to {
                    return tool_bad_params(TOOL, "`from` must be earlier than `to`");
                }
                let step = crate::api::clamp_range_step(from, to, p.step.unwrap_or(60), 1);
                let points = if mode == "rate" {
                    self.state
                        .store
                        .rate_range(&key, from, to, step, step.max(60))
                        .await
                } else if agg {
                    self.state.store.aggregate_range(&key, from, to, step).await
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
                    note,
                }
            }
            _ => return tool_bad_params(TOOL, "`mode` must be latest, range, or rate"),
        };
        ok_json(TOOL, &dto)
    }

    #[tool(
        description = "Which threshold rules govern one interface, and which of them is in \
                       force. A port is reached by rules at six scope levels — all nodes, device \
                       profile, tag group, folder group, node, and the port itself — and the \
                       narrow ones are usually not where the interesting rule lives, so listing \
                       only the port's own rules would report nothing about a port that is \
                       alerting. Each entry carries the rule (its `scope_level`, `scope_id`, \
                       `metric`, `direction`, bounds and `dwell_samples`) and `in_force`: the \
                       most specific level that reaches this port wins, and among folder-group \
                       rules only the nearest group in the chain. Several rules can be in force \
                       for one metric at once, in which case the engine keeps the more \
                       restrictive bound of each severity **on each side of the band \
                       independently** — the higher lower bound and the lower upper bound, since \
                       those are the ones that trip first. So `in_force` means the rule \
                       contributes, not that it alone decides. Metrics reading `if_in_util_pct` \
                       / `if_out_util_pct` are a percentage of the port's own speed and cannot be \
                       evaluated at all where that speed is unknown; `if_in_bps` / `if_out_bps` \
                       are absolute bits/sec and always can."
    )]
    async fn get_interface_thresholds(
        &self,
        Parameters(p): Parameters<InterfaceThresholdsParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        const TOOL: &str = "get_interface_thresholds";
        match self.admit(identity_of(&ctx), TOOL, "").await {
            Ok(scope) => self.get_interface_thresholds_in(p, &scope).await,
            Err(refusal) => refusal,
        }
    }

    pub(super) async fn get_interface_thresholds_in(
        &self,
        p: InterfaceThresholdsParams,
        scope: &NodeScope,
    ) -> Result<CallToolResult, McpError> {
        const TOOL: &str = "get_interface_thresholds";
        if let Some(deny) = deny_invisible_node(&self.state, scope, TOOL, p.node_id) {
            return deny;
        }
        let Some(admin) = self.state.admin.as_ref() else {
            return tool_unavailable(TOOL, "the threshold ruleset requires live mode");
        };
        // The same seam the REST route calls, so the two surfaces cannot come to disagree about
        // which rules reach a port — the resolution is scope inheritance, and a second copy of it
        // is a second set of precedence rules.
        match crate::api::thresholds::interface_thresholds(&self.state, admin, p.node_id, p.ifindex)
            .await
        {
            Ok(rows) => ok_json(TOOL, &rows),
            Err(e) => tool_api_error(TOOL, &e),
        }
    }

    #[tool(
        description = "One interface's history: in/out throughput in bits/sec \
                       (`in_bps`/`out_bps`) and in unicast packets/sec \
                       (`in_ucast_pps`/`out_ucast_pps`), in/out error rates \
                       (`in_errors`/`out_errors`) and in/out discard rates \
                       (`in_discards`/`out_discards`), plus optical transmit and receive power \
                       (`tx_power_dbm`/`rx_power_dbm`), all on one shared timestamp axis (nulls \
                       mark gaps). Consult both \
                       units before calling a link healthy: a device's forwarding ceiling is often \
                       a packet rate, so a link well under its bandwidth can still be saturated. \
                       The packet counters are unicast only, so a link carrying heavy broadcast \
                       reads low and bits divided by packets overstates the average frame size; \
                       they also only exist from the deployment's upgrade to the release that \
                       began collecting them, so an empty pps array over an old window is expected \
                       rather than a fault. Errors and discards are different faults: an error is \
                       a frame that arrived damaged (cabling, optics, NIC), a discard is a frame \
                       the device dropped although nothing was wrong with it (congestion, queue \
                       overflow, ACL) — do not read one as evidence of the other. Both are counted \
                       in packets; IF-MIB has no byte counter for either, so there is no \
                       bits/sec form of them. The two optical readings differ from the eight rates \
                       above in three ways worth knowing. They are gauges reported by the \
                       transceiver, not counter rates. They are normally NEGATIVE: a healthy \
                       receive level is roughly -3 to -20 dBm, and 0 dBm means one milliwatt \
                       rather than nothing. And only optical ports have them, so both arrays being \
                       entirely null is how a copper port, a virtual interface, or a transceiver \
                       whose vendor MIB Yagra does not speak is told apart from a fibre link that \
                       is reading low — it is not a collection fault. A multi-lane module (QSFP) \
                       reports its first lane rather than an aggregate, and like the packet \
                       counters these only exist from the deployment's upgrade to the release that \
                       began collecting them. Judge a level against that module's own acceptable \
                       window, which get_node_status carries on the same interface \
                       (`rx_power_low_dbm`/`rx_power_high_dbm` and the transmit pair), rather than \
                       against a fixed number: -7 dBm is comfortable on one module and failing on \
                       another. Give `node_id` and `ifindex` (from get_node_status's \
                       interfaces). `from`/`to` are Unix seconds (default: last hour) and `step` is \
                       the sample interval in seconds (clamped; defaults to ~120 points across the \
                       window). This is the per-interface counterpart to query_metrics, which is \
                       node-level only."
    )]
    async fn get_interface_series(
        &self,
        Parameters(p): Parameters<InterfaceSeriesParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        const TOOL: &str = "get_interface_series";
        match self.scope_for(identity_of(&ctx)).await {
            Ok(scope) => self.interface_series_in(p, &scope).await,
            Err(e) => tool_api_error(TOOL, &e),
        }
    }

    pub(super) async fn interface_series_in(
        &self,
        p: InterfaceSeriesParams,
        scope: &NodeScope,
    ) -> Result<CallToolResult, McpError> {
        const TOOL: &str = "get_interface_series";
        if let Some(deny) = deny_invisible_node(&self.state, scope, TOOL, p.node_id) {
            return deny;
        }
        let to = p.to.unwrap_or_else(|| Utc::now().timestamp());
        let from = p.from.unwrap_or(to - DEFAULT_WINDOW_SECS);
        if from >= to {
            return tool_bad_params(TOOL, "`from` must be earlier than `to`");
        }
        // The eight metric names, the step/lookback rule and the ×8 bytes→bits scaling all live in
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
        ok_json(TOOL, &series)
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
        const TOOL: &str = "top_metrics";
        match self.scope_for(identity_of(&ctx)).await {
            Ok(scope) => self.top_metrics_in(p, &scope).await,
            Err(e) => tool_api_error(TOOL, &e),
        }
    }

    pub(super) async fn top_metrics_in(
        &self,
        p: TopMetricsParams,
        scope: &NodeScope,
    ) -> Result<CallToolResult, McpError> {
        const TOOL: &str = "top_metrics";
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
            Ok(ranked) => ok_json(TOOL, &ranked),
            Err(e) => tool_api_error(TOOL, &e),
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
        const TOOL: &str = "top_interfaces";
        match self.scope_for(identity_of(&ctx)).await {
            Ok(scope) => self.top_interfaces_in(p, &scope).await,
            Err(e) => tool_api_error(TOOL, &e),
        }
    }

    pub(super) async fn top_interfaces_in(
        &self,
        p: TopInterfacesParams,
        scope: &NodeScope,
    ) -> Result<CallToolResult, McpError> {
        const TOOL: &str = "top_interfaces";
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
                    Err(e) => return tool_api_error(TOOL, &e),
                };
                InterfaceRanking::delta(direction, p.window_secs)
            }
            metric => {
                let m = match crate::api::metrics::parse_interface_metric(metric) {
                    Ok(m) => m,
                    Err(e) => return tool_api_error(TOOL, &e),
                };
                let agg = match crate::api::metrics::parse_top_agg(p.agg.as_deref()) {
                    Ok(a) => a,
                    Err(e) => return tool_api_error(TOOL, &e),
                };
                InterfaceRanking::Metric(m, agg)
            }
        };
        let ranked =
            crate::api::metrics::ranked_interfaces(&self.state, scope, rank, p.limit).await;
        ok_json(TOOL, &ranked)
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
        const TOOL: &str = "fleet_throughput";
        match self.scope_for(identity_of(&ctx)).await {
            Ok(scope) => self.fleet_throughput_in(p, &scope).await,
            Err(e) => tool_api_error(TOOL, &e),
        }
    }

    pub(super) async fn fleet_throughput_in(
        &self,
        p: FleetThroughputParams,
        scope: &NodeScope,
    ) -> Result<CallToolResult, McpError> {
        const TOOL: &str = "fleet_throughput";
        // The refusal lives inside `fleet_throughput`, so this tool cannot serve the fleet's numbers
        // to a scoped caller by forgetting to ask.
        match crate::api::metrics::fleet_throughput(&self.state, scope, p.from, p.to, p.step).await
        {
            Ok(range) => ok_json(TOOL, &range),
            Err(e) => tool_api_error(TOOL, &e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::tools::testkit::*;

    // ── The node-level read rule (ADR-042 I4) ────────────────────────────────────────────────────
    // A node selector has no `ifindex`, so it matches every series sharing the metric's name and
    // the store answers with the first. Live proof on 2026-08-16: `if_hc_in_octets` read 0.0 while
    // the interface actually carried ~3.9 Mbps. Nothing about that is a type error, a panic or an
    // empty result — it is a plausible number, which is why the rule has to be decided rather than
    // discovered.

    #[test]
    fn a_single_series_metric_is_read_directly() {
        assert_eq!(
            node_read(MetricKind::Gauge, MetricDimension::None),
            NodeRead::Direct
        );
        assert_eq!(
            node_read(MetricKind::Counter, MetricDimension::None),
            NodeRead::Direct
        );
    }

    #[test]
    fn a_multi_series_gauge_collapses_to_the_node_maximum() {
        // The reading `?agg=max` already gives the WebUI's Device-health cards. Both dimensions,
        // because `entity` (folded table rows) and `interface` differ in what the rows *are*, not
        // in whether a node-level number exists.
        assert_eq!(
            node_read(MetricKind::Gauge, MetricDimension::Entity),
            NodeRead::NodeMax
        );
        assert_eq!(
            node_read(MetricKind::Gauge, MetricDimension::Interface),
            NodeRead::NodeMax
        );
    }

    #[test]
    fn a_multi_series_counter_has_no_node_level_answer() {
        assert_eq!(
            node_read(MetricKind::Counter, MetricDimension::Interface),
            NodeRead::Refuse
        );
        assert_eq!(
            node_read(MetricKind::Counter, MetricDimension::Entity),
            NodeRead::Refuse
        );
    }

    /// A refusal that does not say where to go instead makes a model guess or retry, so the
    /// destination is part of the contract rather than a nicety. The `entity` case deliberately
    /// names none — row identity is discarded at collection time and there is nowhere to send it.
    #[test]
    fn a_refusal_names_the_tool_that_can_answer() {
        let iface = crate::api::metrics::NodeMetricEntry {
            metric: "if_hc_in_octets".to_owned(),
            metric_kind: MetricKind::Counter,
            dimension: MetricDimension::Interface,
            status: crate::api::metrics::MetricStatus::Ok,
            series_count: 16,
        };
        let msg = no_node_level_answer("if_hc_in_octets", &iface);
        assert!(msg.contains("get_interface_series"), "{msg}");
        assert!(msg.contains("top_interfaces"), "{msg}");
        assert!(msg.contains("16"), "the fan-out is the evidence: {msg}");

        let entity = crate::api::metrics::NodeMetricEntry {
            dimension: MetricDimension::Entity,
            ..iface
        };
        let msg = no_node_level_answer("huawei_bytes", &entity);
        assert!(
            !msg.contains("get_interface_series"),
            "an entity row is not an interface — sending a model there wastes a call: {msg}"
        );
        assert!(msg.contains("identity"), "{msg}");
    }

    /// The alignment invariant is what can be silently wrong here: every series on one axis, so a
    /// chart — or a model — can read column `j` across all of them without bounds-checking.
    ///
    /// ⚠️ **The series list is derived, not written out.** It was a hardcoded array of eight names
    /// until ADR-062 Inc.5, and ADR-062 had already added two more without touching it — so the
    /// alignment of `rx_power_dbm`/`tx_power_dbm` went unchecked from the day they shipped, in the
    /// one test whose whole job is alignment. Reading the keys off the canary means the next field
    /// is covered by existing, rather than by someone remembering.
    #[tokio::test]
    async fn an_interface_series_returns_every_array_on_one_axis() {
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
        let shape = serde_json::to_value(crate::api::metrics::canary_interface_series())
            .expect("InterfaceSeries serializes");
        let keys: Vec<&String> = shape
            .as_object()
            .expect("InterfaceSeries serializes to a JSON object")
            .keys()
            .filter(|k| k.as_str() != "timestamps")
            .collect();
        assert!(
            keys.len() >= 10,
            "only {} series to align — the canary drifted and this test checks nothing",
            keys.len()
        );
        for k in keys {
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
}
