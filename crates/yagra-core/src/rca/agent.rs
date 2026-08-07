// SPDX-License-Identifier: AGPL-3.0-only
//! The RCA agent's tool surface: the MCP tools, in-process, under the caller's own scope
//! (ADR-028 WS-G).
//!
//! ADR-029 Increment 1 assembled a fixed set of facts and sent one bounded call. That was the right
//! shape to ship, and its limit is written into [`super::context`]: interface lists, metric series
//! and configuration are deliberately *not* in the prompt, so the model can only reason about what
//! somebody decided in advance to include. This module is the other half — the model asks.
//!
//! Three rules bound what it can reach, and each closes a different failure:
//!
//! * **The allow-list is view-only.** Not "reads only" — *view*-only. Every tool here needs nothing
//!   beyond `Permission::View`, checked per **branch** against `mcp/folded.rs`, so a folded tool
//!   cannot smuggle in its `ManageCredentials` section. The RCA caller holds `AckAlerts`, which
//!   implies `View`, so this can never widen what that caller could already read — and there is no
//!   privilege question to get wrong later.
//! * **The scope is the caller's**, resolved once at the API edge and passed down. A "privileged
//!   in-process client" that defaulted to [`NodeScope::All`] would be exactly the fail-open ADR-028
//!   WS-F was built to prevent: a group-scoped operator's agent reading the whole fleet.
//! * **Results go through `ok_json`**, which is the `dto.rs` sanitization boundary. The agent never
//!   reaches past it for a typed value; every byte the model sees is a byte a session client could
//!   have seen.
//!
//! Deliberately absent: the three write tools (ADR-042 decision 6 freezes them), `run_analysis`
//! (blocks for up to two minutes and spends TSDB compute), `run_rca` (recursion), and `get_audit`
//! (`ViewAudit`, refused by the branch rule rather than by being listed).

use rmcp::model::CallToolResult;
use serde_json::Value;
use yagra_common::Permission;

use crate::api::scope::NodeScope;
use crate::api::ApiState;
use crate::mcp::YagraMcp;

use super::provider::ToolDef;

/// The tools an RCA agent may call.
///
/// Ordered as a diagnostic would use them — what is broken, is Yagra itself healthy, then the
/// evidence — because the list is published to the model in this order and a model reads a tool
/// list the way it reads a prompt.
///
/// Pinned against the declared tool set by [`tests::every_agent_tool_exists`], so a tool that is
/// renamed breaks the build rather than silently dropping out of the agent's reach, and
/// [`tests::the_agent_never_reaches_a_write_tool`], so the frozen write surface stays frozen.
pub(crate) const AGENT_TOOLS: &[&str] = &[
    // What is wrong
    "get_active_alerts",
    "get_alert_history",
    "get_fleet_summary",
    "alert_trends",
    "list_suppressions",
    // Is the monitoring itself healthy — the question `INSTRUCTIONS` tells every client to ask
    // first, and the one that decides whether missing data means "quiet" or "not collected"
    "get_system_health",
    // The node and its neighbourhood
    "get_node_status",
    "list_nodes",
    "list_node_groups",
    "get_neighbors",
    "get_topology",
    "get_dns_chain",
    // The measurements
    "query_metrics",
    "get_interface_series",
    "top_metrics",
    "top_interfaces",
    "fleet_throughput",
    "fleet_state_history",
    // What the network said
    "search_events",
    "event_stats",
    "top_flows",
    "flow_fanout",
    // What has already been diagnosed, and how the deployment is configured to react
    "search_analysis_findings",
    "get_analysis_findings",
    "list_analyses",
    "get_config",
];

/// The MCP tools, callable in-process under one caller's scope.
pub(crate) struct AgentTools {
    mcp: YagraMcp,
}

impl AgentTools {
    #[must_use]
    pub(crate) fn new(state: ApiState) -> Self {
        Self {
            mcp: YagraMcp::new(state),
        }
    }

    /// The tool definitions to hand the provider, in [`AGENT_TOOLS`] order.
    ///
    /// The schemas come from rmcp's own router — `ToolRouter::list_all()` needs no session, no
    /// `Peer` and no transport — so there is exactly one description of each tool's arguments and
    /// it is the one `mcp/tools.rs` derived. A hand-written second copy here is the mirror
    /// `extensibility.md` §2 says to avoid creating.
    pub(crate) fn schemas(&self) -> Vec<ToolDef> {
        let published = self.mcp.published_tools();
        AGENT_TOOLS
            .iter()
            .filter_map(|name| {
                let t = published.iter().find(|t| t.name == **name)?;
                Some(ToolDef {
                    name: t.name.to_string(),
                    description: t.description.as_deref().unwrap_or_default().to_string(),
                    schema: Value::Object((*t.input_schema).clone()),
                })
            })
            .collect()
    }

    /// Run one tool the model asked for, or say why not.
    ///
    /// Returns the text the model should see either way: a refusal is information it can act on,
    /// where a hard error is something it can only retry blindly. This mirrors `tool_api_error`'s
    /// rule that a 404/503 is a *successful* result carrying `available: false`.
    pub(crate) async fn call(&self, name: &str, args: Value, scope: &NodeScope) -> String {
        if !AGENT_TOOLS.contains(&name) {
            return refusal(&format!(
                "{name} is not available to this analysis; the tools you may call are: {}",
                AGENT_TOOLS.join(", ")
            ));
        }
        // The branch rule. A folded tool's sections do not share a permission — `get_system_health`
        // alone spans View, ManageConfig and ManageCredentials — so the check is per branch and not
        // per tool, and it reads the same table the session surface enforces from.
        let arg = branch_arg(name, &args);
        match crate::mcp::folded::permission_of(name, &arg) {
            Some(Permission::View) | None => {}
            Some(other) => {
                return refusal(&format!(
                    "{name}({arg}) needs {} permission; this analysis runs view-only",
                    other.key().replace('_', "-")
                ))
            }
        }
        match self.mcp.call_in(name, args, scope).await {
            Ok(result) => text_of(&result),
            // The tool refused the arguments or failed. The message is already written for a model
            // (it lists the valid vocabulary where there is one), so it is handed back rather than
            // ending the turn.
            Err(e) => refusal(&e.message),
        }
    }
}

/// The argument that names a folded tool's branch, for the tools that select one by string.
///
/// Separate from [`branch_arg`] so a test can prove the runtime check is able to *see* every
/// non-`View` branch: a folded tool whose key this does not know resolves to `""`, which finds no
/// row, which would let the branch through unchecked.
///
/// The `_` arm is over tool *names*, not a domain enum — a tool with no branches has no key, which
/// is the honest answer.
fn branch_key(name: &str) -> Option<&'static str> {
    match name {
        "get_system_health" => Some("section"),
        "get_config" => Some("kind"),
        // `summary` (the default) has no row; only `coverage` does, and both are View.
        "get_fleet_summary" => Some("kind"),
        _ => None,
    }
}

/// The `FOLDED_READS` key a call selects, or `""` for a tool that has no branches.
fn branch_arg(name: &str, args: &Value) -> String {
    if let Some(key) = branch_key(name) {
        return args
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
    }
    match name {
        // Selected by the *presence* of an argument rather than its value. Both branches are `View`
        // on each, so getting these wrong costs nothing today — they are here so the mapping is
        // complete rather than because the permission turns on it.
        "get_report_runs" => {
            if args.get("run_id").is_some() {
                "detail".to_owned()
            } else {
                "list".to_owned()
            }
        }
        "get_dns_chain" => {
            if args.get("history").and_then(Value::as_bool) == Some(true) {
                "history".to_owned()
            } else {
                "current".to_owned()
            }
        }
        _ => String::new(),
    }
}

/// The text blocks of a tool result, concatenated — the JSON `ok_json` produced.
fn text_of(result: &CallToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|c| c.as_text().map(|t| t.text.as_str()))
        .collect::<Vec<_>>()
        .concat()
}

/// A refusal shaped like a tool result, so the model reads it as an answer rather than a fault.
fn refusal(reason: &str) -> String {
    serde_json::json!({ "available": false, "reason": reason }).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    /// Every name here is a tool that exists. A rename would otherwise drop a tool out of the
    /// agent's reach with nothing failing — the agent would simply stop being able to ask.
    #[test]
    fn every_agent_tool_exists() {
        let declared = crate::api::route_table::declared_mcp_tools();
        let missing: Vec<&&str> = AGENT_TOOLS
            .iter()
            .filter(|n| !declared.contains(**n))
            .collect();
        assert!(
            missing.is_empty(),
            "the agent allow-list names tools that do not exist: {missing:?}"
        );
    }

    /// The write surface is frozen (ADR-042 decision 6) and the two expensive reads are excluded by
    /// judgement. Stated as an explicit deny rather than left to the allow-list's spelling, because
    /// the failure mode is someone adding a name that looks like a read.
    #[test]
    fn the_agent_never_reaches_a_write_tool() {
        for banned in [
            "ack_alert",
            "open_maintenance",
            "poll_now",
            "run_analysis",
            "run_rca",
        ] {
            assert!(
                !AGENT_TOOLS.contains(&banned),
                "{banned} must not be callable by the RCA agent"
            );
        }
    }

    /// The branches the agent reaches that demand more than `View` are ones the runtime check can
    /// *name*.
    ///
    /// Two allow-listed tools are folded over mixed permissions — `get_system_health` spans View,
    /// ManageConfig and ManageCredentials; `get_config` spans View, ManageConfig and ManageUsers —
    /// so "no reachable branch needs more than View" is false and refusing them at run time is the
    /// design. What must hold instead is that [`branch_arg`] extracts the key, because a branch it
    /// cannot name resolves to `""`, finds no row, and passes unchecked.
    #[test]
    fn every_non_view_branch_is_one_the_check_can_name() {
        let allowed: BTreeSet<&str> = AGENT_TOOLS.iter().copied().collect();
        let mut checked = 0usize;
        for f in crate::mcp::folded::FOLDED_READS {
            if !allowed.contains(f.tool) || f.perm == Some(Permission::View) || f.perm.is_none() {
                continue;
            }
            checked += 1;
            let key = branch_key(f.tool).unwrap_or_else(|| {
                panic!(
                    "`{}` has a non-View branch `{}` but no branch key; the permission check \
                     cannot see it",
                    f.tool, f.arg
                )
            });
            assert_eq!(
                branch_arg(f.tool, &serde_json::json!({ key: f.arg })),
                f.arg,
                "`{}`/`{}` is not recoverable from its own arguments",
                f.tool,
                f.arg
            );
        }
        assert!(
            checked >= 16,
            "only checked {checked} non-View reachable branches; the allow-list or the table drifted"
        );
    }

    /// …and every one of them is actually refused, end to end.
    ///
    /// The test above proves the check can see the branch; this one proves it acts. Driving `call`
    /// rather than the helper is the point — a refusal that only happens in a unit of the policy
    /// and not in the path the model takes is not a refusal.
    #[tokio::test]
    async fn every_non_view_branch_is_refused_by_a_call() {
        let allowed: BTreeSet<&str> = AGENT_TOOLS.iter().copied().collect();
        let tools = AgentTools::new(crate::api::tests_support::private_state());
        let mut checked = 0usize;
        for f in crate::mcp::folded::FOLDED_READS {
            if !allowed.contains(f.tool) || f.perm == Some(Permission::View) || f.perm.is_none() {
                continue;
            }
            let key = branch_key(f.tool).expect("checked by the test above");
            let out = tools
                .call(
                    f.tool,
                    serde_json::json!({ key: f.arg }),
                    // `All` on purpose: the refusal must come from the permission rule, not from
                    // the scope happening to hide everything.
                    &NodeScope::All,
                )
                .await;
            assert!(
                out.contains("view-only"),
                "`{}`/`{}` was not refused: {out}",
                f.tool,
                f.arg
            );
            checked += 1;
        }
        assert!(checked >= 16, "only exercised {checked} refusals");
    }

    /// The refusal names the permission, in the hyphenated spelling every description uses, so a
    /// model can stop asking rather than retry.
    #[tokio::test]
    async fn a_refusal_names_the_permission_it_lacked() {
        let tools = AgentTools::new(crate::api::tests_support::private_state());
        let out = tools
            .call(
                "get_system_health",
                serde_json::json!({ "section": "credentials" }),
                &NodeScope::All,
            )
            .await;
        assert!(out.contains("manage-credentials"), "{out}");
        let out = tools
            .call(
                "get_config",
                serde_json::json!({ "kind": "oidc" }),
                &NodeScope::All,
            )
            .await;
        assert!(out.contains("manage-users"), "{out}");
    }

    /// A `View` branch of the same folded tool goes through. Without this the test above passes for
    /// a rule that refuses everything.
    #[tokio::test]
    async fn a_view_branch_of_a_mixed_tool_is_not_refused() {
        let tools = AgentTools::new(crate::api::tests_support::private_state());
        for (tool, args) in [
            ("get_config", serde_json::json!({ "kind": "roles" })),
            (
                "get_system_health",
                serde_json::json!({ "section": "version" }),
            ),
        ] {
            let out = tools.call(tool, args, &NodeScope::All).await;
            assert!(
                !out.contains("view-only"),
                "{tool} was refused by the permission rule: {out}"
            );
        }
    }

    /// A tool outside the allow-list is refused with the list, not with a bare error.
    #[tokio::test]
    async fn an_unlisted_tool_is_refused_with_the_alternatives() {
        let tools = AgentTools::new(crate::api::tests_support::private_state());
        let out = tools
            .call("poll_now", serde_json::json!({}), &NodeScope::All)
            .await;
        assert!(out.contains("not available to this analysis"), "{out}");
        assert!(out.contains("query_metrics"), "{out}");
    }

    /// The schemas are rmcp's own, reachable with no session — the fact WS-G turned out to rest on.
    #[test]
    fn the_tool_schemas_come_from_the_router_without_a_session() {
        let tools = AgentTools::new(crate::api::tests_support::private_state());
        let defs = tools.schemas();
        assert_eq!(
            defs.len(),
            AGENT_TOOLS.len(),
            "every allow-listed tool must resolve to a published schema"
        );
        let qm = defs
            .iter()
            .find(|d| d.name == "query_metrics")
            .expect("query_metrics is on the list");
        assert!(!qm.description.is_empty(), "the description is published");
        assert_eq!(qm.schema["type"], serde_json::json!("object"));
        assert!(qm.schema["properties"].get("metric").is_some());
    }
}
