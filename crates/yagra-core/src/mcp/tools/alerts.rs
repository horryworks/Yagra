// SPDX-License-Identifier: AGPL-3.0-only
//! MCP tools: what is wrong now and what was wrong before, plus the two writes that act on an alert (ADR-086).
//!
//! Split out of the single `tools.rs` by ADR-086; the module doc for the surface as a whole,
//! and the rules every tool here obeys, are in [`super`].

use chrono::Utc;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
// The module (not just the trait) — the `JsonSchema` derive expands to `schemars::…` paths, so the
// `schemars` name must be in scope. rmcp re-exports it, keeping exactly one schemars version.
use rmcp::schemars;
use rmcp::service::{RequestContext, RoleServer};
use rmcp::{tool, tool_router, ErrorData as McpError};
use serde::Deserialize;
use uuid::Uuid;
use yagra_common::{NodeId, Permission};

use super::YagraMcp;
use crate::ack::AckView;
use crate::api::scope::NodeScope;
use crate::mcp::dto::{AlertDto, AlertHistoryDto};

// The shared scope: the helpers in `support.rs` and the types the other domain modules declare,
// re-exported by `mod.rs` so no file has to name where a sibling keeps a thing.
use super::*;

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(super) struct ActiveAlertsParams {
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

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
pub(super) struct AlertTrendsParams {
    /// Which view: top_nodes | transitions | calendar.
    kind: String,
    /// Trailing window in seconds for top_nodes (60–2592000, default 86400).
    window_secs: Option<i64>,
    /// Days of history for calendar (1–90, default 7).
    days: Option<i64>,
    /// Row count: top_nodes 1–50 (default 6), transitions default 12.
    limit: Option<i64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(super) struct AckAlertParams {
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
pub(super) struct OpenMaintenanceParams {
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

#[tool_router(router = alerts_router, vis = "pub(super)")]
impl YagraMcp {
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
        const TOOL: &str = "get_active_alerts";
        match self.scope_for(identity_of(&ctx)).await {
            Ok(scope) => self.active_alerts_in(p, &scope).await,
            Err(e) => tool_api_error(TOOL, &e),
        }
    }

    pub(super) async fn active_alerts_in(
        &self,
        p: ActiveAlertsParams,
        scope: &NodeScope,
    ) -> Result<CallToolResult, McpError> {
        const TOOL: &str = "get_active_alerts";
        let mut alerts = self.state.alerts.active_alerts();
        // Filtered before the severity cut and the truncation, so a scoped caller's `limit` is
        // spent on rows they can see rather than on rows that are about to be dropped.
        alerts.retain(|a| scope.allows_subject(&self.state, &a.subject));
        if let Some(node_id) = p.node_id {
            let nid = NodeId::from(node_id);
            alerts.retain(|a| a.subject.is_node(nid));
        }
        if let Some(min) = p.min_severity.as_deref() {
            // `Severity` is `Ord` low→high and `a.severity` already *is* one, so this compares the
            // values instead of ranking their spellings. The rank helper this replaces took a
            // `&str` with a `_ => 0` fallback: an unparseable `min_severity` scored below `info`,
            // so the filter matched everything and the model reasoned from a full list believing
            // it had been narrowed. Refusing names the three valid values, the way `ack_alert`
            // already does with the same parser.
            let Some(min) = parse_severity(min) else {
                return tool_bad_params(TOOL, "`min_severity` must be info, warning, or critical");
            };
            alerts.retain(|a| a.severity >= min);
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
        ok_json(TOOL, &out)
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
        const TOOL: &str = "get_alert_history";
        match self.scope_for(identity_of(&ctx)).await {
            Ok(scope) => self.alert_history_in(p, &scope).await,
            Err(e) => tool_api_error(TOOL, &e),
        }
    }

    pub(crate) async fn alert_history_in(
        &self,
        p: AlertHistoryParams,
        scope: &NodeScope,
    ) -> Result<CallToolResult, McpError> {
        const TOOL: &str = "get_alert_history";
        if self.state.history.is_none() {
            return tool_unavailable(TOOL, "alert history requires live mode");
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
            Err(e) => return tool_api_error(TOOL, &e),
        };
        let names = self
            .resolve_names(scope, rows.iter().filter_map(|r| r.node))
            .await;
        let out: Vec<AlertHistoryDto> = rows
            .iter()
            .map(|r| AlertHistoryDto::from_row(r, r.node.and_then(|n| names.get(&n).cloned())))
            .collect();
        ok_json(TOOL, &out)
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
        const TOOL: &str = "list_suppressions";
        match self.scope_for(identity_of(&ctx)).await {
            Ok(scope) => self.list_suppressions_in(&scope).await,
            Err(e) => tool_api_error(TOOL, &e),
        }
    }

    pub(super) async fn list_suppressions_in(
        &self,
        scope: &NodeScope,
    ) -> Result<CallToolResult, McpError> {
        const TOOL: &str = "list_suppressions";
        let Some(admin) = self.state.admin.as_ref() else {
            return tool_unavailable(TOOL, "suppression state requires live mode");
        };
        // Both lists filter on the row's own target, and a window scoped to a profile or a tag is
        // hidden from a scoped caller entirely — the shared seams carry that, so this surface
        // cannot show a window the WebUI would not.
        let windows =
            match crate::api::maintenance::visible_windows(&self.state, scope, admin).await {
                Ok(w) => w,
                Err(e) => return tool_api_error(TOOL, &e),
            };
        let mutes = match crate::api::maintenance::visible_mutes(&self.state, scope, admin).await {
            Ok(m) => m,
            Err(e) => return tool_api_error(TOOL, &e),
        };
        // The negative half. Without it a released node reads as suppressed, which is the wrong
        // way round for a tool whose whole point is telling a quiet fleet from a silenced one.
        let exemptions =
            match crate::api::maintenance::visible_exemptions(&self.state, scope, admin).await {
                Ok(x) => x,
                Err(e) => return tool_api_error(TOOL, &e),
            };
        ok_json(
            TOOL,
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
        const TOOL: &str = "alert_trends";
        match self.scope_for(identity_of(&ctx)).await {
            Ok(scope) => self.alert_trends_in(p, &scope).await,
            Err(e) => tool_api_error(TOOL, &e),
        }
    }

    pub(super) async fn alert_trends_in(
        &self,
        p: AlertTrendsParams,
        scope: &NodeScope,
    ) -> Result<CallToolResult, McpError> {
        const TOOL: &str = "alert_trends";
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
                    Ok(ranked) => ok_json(TOOL, &ranked),
                    Err(e) => tool_api_error(TOOL, &e),
                }
            }
            "transitions" => {
                match crate::api::alerts::recent_transitions(&self.state, scope, p.limit).await {
                    Ok(rows) => ok_json(TOOL, &rows),
                    Err(e) => tool_api_error(TOOL, &e),
                }
            }
            "calendar" => {
                match crate::api::alerts::alert_calendar_buckets(&self.state, scope, p.days).await {
                    Ok(rows) => ok_json(TOOL, &rows),
                    Err(e) => tool_api_error(TOOL, &e),
                }
            }
            other => tool_bad_params(
                TOOL,
                &format!("unknown kind {other:?}; must be top_nodes, transitions or calendar"),
            ),
        }
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
        const TOOL: &str = "ack_alert";
        let Some(identity) = authed_for(identity_of(&ctx), Permission::AckAlerts) else {
            return tool_forbidden(TOOL, "this token lacks ack-alerts permission");
        };
        // A write is scoped like a read, and this one is also a read: the 200/404 difference would
        // otherwise tell a scoped caller whether an invisible node currently has that alert.
        if let Some(deny) = self
            .deny_invisible_node_for(identity_of(&ctx), TOOL, p.node_id)
            .await
        {
            return deny;
        }
        let Some(severity) = parse_severity(&p.severity) else {
            return tool_bad_params(TOOL, "`severity` must be info, warning, or critical");
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
            return tool_api_error(TOOL, &e);
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
            TOOL,
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
        const TOOL: &str = "open_maintenance";
        let Some(identity) = authed_for(identity_of(&ctx), Permission::ManageMaintenance) else {
            return tool_forbidden(TOOL, "this token lacks manage-maintenance permission");
        };
        if let Some(deny) = self
            .deny_invisible_node_for(identity_of(&ctx), TOOL, p.node_id)
            .await
        {
            return deny;
        }
        let Some(admin) = self.state.admin.as_ref() else {
            return tool_unavailable(TOOL, "maintenance requires live mode");
        };
        // Explicit bounds go through the same parse + ordering check as the REST edge; a window
        // that ends before it starts is stored happily and suppresses nothing, so the operator
        // believes they are covered and gets paged through the change anyway.
        let (starts, ends) = match (p.starts_at.as_deref(), p.ends_at.as_deref()) {
            (Some(s), Some(e)) => match crate::api::maintenance::window_bounds(s, e) {
                Ok(pair) => pair,
                Err(err) => return tool_api_error(TOOL, &err),
            },
            // This surface's own convenience: "mute it for an hour" without composing timestamps.
            // The duration is clamped, so the ordering check below can only pass — it runs anyway
            // because the invariant belongs to the window, not to how the bounds were obtained.
            (None, None) => {
                let mins = p.duration_mins.unwrap_or(60).clamp(1, 7 * 24 * 60);
                let now = Utc::now();
                let pair = (now, now + chrono::Duration::minutes(mins));
                if let Err(err) = crate::api::maintenance::check_order(pair.0, pair.1) {
                    return tool_api_error(TOOL, &err);
                }
                pair
            }
            _ => {
                return tool_bad_params(
                    TOOL,
                    "provide both starts_at and ends_at, or neither (use duration_mins)",
                )
            }
        };
        let node = match admin.repo.get_node(p.node_id).await {
            Ok(Some(n)) => n,
            Ok(None) => return tool_unavailable(TOOL, "no node with that id"),
            Err(e) => return tool_error(TOOL, "load node", &e),
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
                    TOOL,
                    serde_json::json!({
                        "created": true,
                        "window_id": id,
                        "node_id": p.node_id,
                        "starts_at": starts.to_rfc3339(),
                        "ends_at": ends.to_rfc3339(),
                    }),
                )
            }
            Err(e) => tool_error(TOOL, "create maintenance window", &e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::tools::testkit::*;

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

    /// An unparseable `min_severity` is refused, and a valid one is not.
    ///
    /// The acceptance half comes first on purpose: a rejection-only test passes just as well on a
    /// tool that refuses *every* `min_severity`, which would be a worse bug than the one being
    /// fixed. Before this, the value went through a `severity_rank(&str)` with a `_ => 0` arm, so
    /// `"fatal"` scored below `info`, the filter matched everything, and the model was handed the
    /// unfiltered list as though it were the answer to a narrowed question.
    #[tokio::test]
    async fn an_unparseable_min_severity_is_refused_where_a_valid_one_is_accepted() {
        let params = |min: &str| ActiveAlertsParams {
            node_id: None,
            min_severity: Some(min.to_owned()),
            limit: None,
        };
        for good in ["info", "warning", "critical", "  CRITICAL "] {
            mcp()
                .active_alerts_in(params(good), &unrestricted())
                .await
                .unwrap_or_else(|e| panic!("{good} must be accepted, got {e}"));
        }
        for bad in ["fatal", "", "warn", "2"] {
            assert!(
                mcp()
                    .active_alerts_in(params(bad), &unrestricted())
                    .await
                    .is_err(),
                "{bad} must be refused rather than silently widening the filter"
            );
        }
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
}
