// SPDX-License-Identifier: AGPL-3.0-only
//! MCP tools: the plumbing every tool uses: result shapes, refusals, the metric, and the identity checks (ADR-086).
//!
//! Split out of the single `tools.rs` by ADR-086; the module doc for the surface as a whole,
//! and the rules every tool here obeys, are in [`super`].

use rmcp::model::{CallToolResult, ContentBlock};
// The module (not just the trait) — the `JsonSchema` derive expands to `schemars::…` paths, so the
// `schemars` name must be in scope. rmcp re-exports it, keeping exactly one schemars version.
use rmcp::service::{RequestContext, RoleServer};
use rmcp::ErrorData as McpError;
use serde_json::Value;
use uuid::Uuid;
use yagra_common::{NodeId, Permission, Severity};

use super::McpIdentity;
use crate::api::scope::NodeScope;
use crate::api::{ApiError, ApiState};
use axum::http::StatusCode;

// The shared scope: the helpers in `support.rs` and the types the other domain modules declare,
// re-exported by `mod.rs` so no file has to name where a sibling keeps a thing.

/// How this surface names a permission to a model.
///
/// `Permission::key()` is the stored form (`manage_config`) and is what a *database* row holds.
/// Every tool description and refusal on this surface has always used the hyphenated spelling
/// (`manage-config`, `ack-alerts`), so a permission rendered from the key would read differently in
/// a tool's error than in the description that told the model what it needed — a small mismatch,
/// but this text is a specification a model reasons from, and two spellings for one thing is
/// exactly what makes it guess.
pub(super) fn permission_label(p: Permission) -> String {
    p.key().replace('_', "-")
}

// ── Result / metric helpers ───────────────────────────────────────────────────────────────────────

/// Serialize a DTO to a pretty-JSON tool result (records an `ok` outcome).
pub(super) fn ok_json<T: serde::Serialize>(
    tool: &str,
    value: &T,
) -> Result<CallToolResult, McpError> {
    match serde_json::to_string_pretty(value) {
        Ok(text) => {
            record_tool(tool, "ok");
            Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
        }
        Err(e) => tool_error(tool, "serialize result", &anyhow::Error::new(e)),
    }
}

/// Serialize an already-built JSON value to a pretty-JSON tool result (records `ok`).
pub(super) fn ok_json_value(tool: &str, value: Value) -> Result<CallToolResult, McpError> {
    let text = serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string());
    record_tool(tool, "ok");
    Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
}

/// A "feature not available here" answer (records `unavailable`). Returned as a **successful** result
/// with an explanatory body so the model understands the tier is off rather than seeing a hard error.
pub(super) fn tool_unavailable(tool: &str, reason: &str) -> Result<CallToolResult, McpError> {
    record_tool(tool, "unavailable");
    let body = serde_json::json!({ "available": false, "reason": reason });
    Ok(CallToolResult::success(vec![ContentBlock::text(
        body.to_string(),
    )]))
}

pub(super) fn tool_bad_params(tool: &str, reason: &str) -> Result<CallToolResult, McpError> {
    record_tool(tool, "bad_params");
    Err(McpError::invalid_params(reason.to_string(), None))
}

/// An internal tool error (records `error`). Logs the context + underlying error but returns a
/// generic message to the client — never a raw internal error string (coding-conventions / security).
pub(super) fn tool_error(
    tool: &str,
    context: &str,
    err: &anyhow::Error,
) -> Result<CallToolResult, McpError> {
    record_tool(tool, "error");
    tracing::warn!(tool, error = %err, "MCP tool error while {context}");
    Err(McpError::internal_error(context.to_string(), None))
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
pub(super) fn tool_api_error(tool: &str, err: &ApiError) -> Result<CallToolResult, McpError> {
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
///
/// ⚠️ `::metrics`, absolutely, because ADR-086 gave this surface a **module** called `metrics` (the
/// per-node and per-port reads) and `use super::*` brings it into scope here. A domain module named
/// after a crate shadows that crate — the compiler catches it as an ambiguity rather than choosing
/// wrong, but the fix is to say which one, not to rename the domain.
pub(super) fn record_tool(tool: &str, outcome: &str) {
    ::metrics::counter!("yagra_mcp_tool_calls_total", "tool" => tool.to_owned(), "outcome" => outcome.to_owned())
        .increment(1);
}

/// The authenticated caller `mcp_auth_mw` inserted into the request extensions (WS-D). rmcp forwards
/// the HTTP request `Parts` into the tool's `RequestContext`, so the identity is read back from
/// `parts.extensions`.
pub(super) fn identity_of(ctx: &RequestContext<RoleServer>) -> Option<McpIdentity> {
    ctx.extensions
        .get::<axum::http::request::Parts>()
        .and_then(|parts| parts.extensions.get::<McpIdentity>())
        .cloned()
}

/// [`identity_of`], but only if it holds `perm`. Fail-closed: `None` ⇒ the tool returns forbidden.
pub(super) fn authed_for(
    ctx: &RequestContext<RoleServer>,
    perm: Permission,
) -> Option<McpIdentity> {
    identity_of(ctx).filter(|id| id.principal.can(perm))
}

/// The early return for a tool that names a node the caller may not see, or `None` to continue.
///
/// One helper rather than the check written out per tool: six tools take a `node_id`, and a missing
/// check on any one of them is a silent leak with nothing to catch it. It answers exactly what a
/// nonexistent id answers — a distinct refusal would confirm the node exists.
pub(super) fn deny_invisible_node(
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
pub(super) async fn record_audit(
    state: &ApiState,
    identity: &McpIdentity,
    action: &str,
    status: u16,
) {
    if let Some(admin) = state.admin.as_ref() {
        if let Err(e) = admin.audit.record(&identity.actor, action, status).await {
            tracing::warn!(error = %e, action, "MCP audit record failed");
        }
    }
}

/// Parse a severity string (info|warning|critical) into the enum. `None` on anything else.
pub(super) fn parse_severity(s: &str) -> Option<Severity> {
    // An LLM wrote this argument, so normalize the shape before matching — unlike the REST edge,
    // where the value came from a form and "Critical" is a client bug worth surfacing.
    Severity::from_token(s.trim().to_ascii_lowercase().as_str())
}

// This surface no longer parses timestamps of its own: `parse_rfc3339_ok`/`parse_opt_rfc3339`
// lived here and were the MCP copies of the REST edge's parsing. Both callers (`open_maintenance`,
// `search_events`) now go through the shared validators in `api::maintenance` / `api::eventlog`,
// which is what makes a bound rejected on one surface rejected on both.

/// A permission-denied tool result (records `forbidden`). Maps to a JSON-RPC invalid-request error.
pub(super) fn tool_forbidden(tool: &str, reason: &str) -> Result<CallToolResult, McpError> {
    record_tool(tool, "forbidden");
    Err(McpError::invalid_request(reason.to_string(), None))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::tools::testkit::*;

    #[test]
    fn severity_parses_case_insensitively_and_rejects_junk() {
        assert_eq!(parse_severity("info"), Some(Severity::Info));
        assert_eq!(parse_severity("  WARNING "), Some(Severity::Warning));
        assert_eq!(parse_severity("Critical"), Some(Severity::Critical));
        assert_eq!(parse_severity("fatal"), None);
        assert_eq!(parse_severity(""), None);
    }

    /// The ordering `min_severity` filters by, read off the type rather than off a rank table.
    ///
    /// `severity_rank(&str)` used to sit here with a `_ => 0` arm, so "nonsense" ranked equal to
    /// `info` and the filter silently matched everything. `Severity` is `Ord` already; the only
    /// thing the helper added was a second, weaker copy of that order.
    #[test]
    fn severity_orders_low_to_high_and_an_unknown_token_is_not_a_severity() {
        assert!(Severity::Critical > Severity::Warning);
        assert!(Severity::Warning > Severity::Info);
        assert_eq!(parse_severity("nonsense"), None);
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
}
