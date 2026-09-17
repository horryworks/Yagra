// SPDX-License-Identifier: AGPL-3.0-only
//! MCP tools: wireless controllers and their access points (ADR-064).
//!
//! The rules every tool here obeys are in [`super`]. The one tool mirrors `GET /api/v1/wireless/aps`
//! and calls the same seam, `api::wireless::wireless_ap_page`, so the scope predicate — an AP is
//! visible when a controller the caller can see reports it — lives in one statement for both.

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

use super::*;

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(super) struct WirelessApsParams {
    /// Only APs this wireless controller node reports (its UUID).
    controller_node_id: Option<Uuid>,
    /// Only APs in this state: "associated", "backup" or "not_associated".
    state: Option<String>,
    /// Case-insensitive substring of the AP's name, MAC address, IP address or model (at most 64
    /// characters, matched literally).
    search: Option<String>,
    /// Max APs to return (1–2048, default 200).
    limit: Option<i64>,
    /// Keyset cursor: `next.key` from the previous page. Pair with `after_id`.
    after_key: Option<String>,
    /// Keyset cursor: `next.ap_id` from the previous page. Pair with `after_key`.
    after_id: Option<Uuid>,
}

#[tool_router(router = wireless_router, vis = "pub(super)")]
impl YagraMcp {
    #[tool(
        description = "Access points behind the wireless controllers Yagra monitors, ordered by \
                       name — listed from each controller's own AP table, whether or not an AP has \
                       been imported as a node. Each AP has its MAC-derived `ap_id`, name, model, \
                       software version, serial, IP, `clients` online, and `state`: `associated` \
                       (in service), `not_associated` (down, not joined, or failing) or `backup` (a \
                       standby controller's view). An AP behind an HA controller pair appears once: \
                       `state` and `clients` are what the controller serving it says \
                       (`controller_node_id`), and `reported_by` lists every controller's view. \
                       Narrow with `controller_node_id`, `state` or `search`; `limit` is 1–2048 \
                       (default 200); page with `after_key` and `after_id` from `next`, both together \
                       or neither. `last_seen` stops advancing when no controller reports an AP any \
                       more — an old `last_seen` means the AP is no longer reported, not that it was \
                       removed. For a controller's own AP and client totals use list_node_metrics on \
                       the controller node."
    )]
    async fn list_wireless_aps(
        &self,
        Parameters(p): Parameters<WirelessApsParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        const TOOL: &str = "list_wireless_aps";
        match self.scope_for(identity_of(&ctx)).await {
            Ok(scope) => self.wireless_aps_in(p, &scope).await,
            Err(e) => tool_api_error(TOOL, &e),
        }
    }

    pub(super) async fn wireless_aps_in(
        &self,
        p: WirelessApsParams,
        scope: &NodeScope,
    ) -> Result<CallToolResult, McpError> {
        const TOOL: &str = "list_wireless_aps";
        let Some(admin) = self.state.admin.as_ref() else {
            return tool_unavailable(TOOL, "the access point list requires live mode");
        };
        let request = match crate::api::wireless::WirelessApRequest::parse(
            p.controller_node_id,
            p.state.as_deref(),
            p.search.as_deref(),
            p.after_key,
            p.after_id,
            p.limit,
        ) {
            Ok(r) => r,
            Err(e) => return tool_api_error(TOOL, &e),
        };
        match crate::api::wireless::wireless_ap_page(admin, scope, request).await {
            Ok(page) => ok_json(TOOL, &page),
            Err(e) => tool_api_error(TOOL, &e),
        }
    }
}
