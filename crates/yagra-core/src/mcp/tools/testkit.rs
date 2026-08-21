// SPDX-License-Identifier: AGPL-3.0-only
//! Fixtures shared by the tool tests (ADR-086).
//!
//! Six helpers that every domain's tests reach for. They were local to one `mod tests` while the
//! surface was one file; splitting the file is what made them need a home of their own.

use rmcp::model::CallToolResult;
// The module (not just the trait) — the `JsonSchema` derive expands to `schemars::…` paths, so the
// `schemars` name must be in scope. rmcp re-exports it, keeping exactly one schemars version.
use serde_json::Value;
use uuid::Uuid;

use super::YagraMcp;
use crate::api::scope::NodeScope;
use crate::api::ApiState;

// The shared scope: the helpers in `support.rs` and the types the other domain modules declare,
// re-exported by `mod.rs` so no file has to name where a sibling keeps a thing.
use super::*;
use crate::alerts::AlertManager;
use crate::auth::{LoginThrottle, SessionStore};
use crate::sink::InMemorySink;
use crate::store::MetricStore;
use std::sync::Arc;

/// A skeleton-mode state: no `admin`, no flow/log tier. This is deliberately the *degraded*
/// shape — it is what exercises every "tier not enabled / requires live mode" branch, which is
/// the half of each tool a live-DB test would never reach.
pub(super) fn skeleton_state() -> ApiState {
    let store: Arc<dyn MetricStore> = Arc::new(InMemorySink::default());
    ApiState {
        store,
        logs: None,
        flows: None,
        ipasn: crate::ipasn::empty_handle(),
        host_sample: Arc::new(std::sync::Mutex::new(None)),
        nodes: Arc::new(crate::repo::StaticNodeList::demo()),
        alerts: Arc::new(AlertManager::new()),
        admin: None,
        sessions: Arc::new(SessionStore::new()),
        login_throttle: Arc::new(LoginThrottle::new()),
        history: None,
        ack: None,
        events: None,
        public_dashboard: false,
        is_leader: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        ldap: None,
        oidc: None,
        oidc_flight: Arc::new(crate::oidc::OidcFlight::new()),
        enable_mcp: true,
        rca: None,
        webtls: None,
        bus_tls: None,
        upgrade: None,
        metrics: None,
        started: std::time::SystemTime::now(),
        poller_logs: None,
    }
}

pub(super) fn mcp() -> YagraMcp {
    YagraMcp::new(skeleton_state())
}

/// The text a tool result carries.
pub(super) fn text_of(r: &CallToolResult) -> String {
    r.content
        .iter()
        .filter_map(|b| b.as_text().map(|t| t.text.clone()))
        .collect::<Vec<_>>()
        .join("")
}

pub(super) fn json_of(r: &CallToolResult) -> Value {
    serde_json::from_str(&text_of(r)).expect("tool result body is JSON")
}

// ── Tool bodies over a skeleton state ───────────────────────────────────────────────────────

/// The scope an unrestricted caller resolves to. The `#[tool]` wrappers are unreachable from a
/// test — rmcp's `RequestContext` needs a live `Peer`, whose constructor is crate-private — so
/// the tests drive the `*_in` bodies the wrappers delegate to. That split is what makes the
/// scoped behaviour testable at all; before it, the only reachable entry point required a
/// running MCP session.
pub(super) fn unrestricted() -> NodeScope {
    NodeScope::All
}

/// A scope naming a group that does not exist — i.e. one that can see nothing. The shape a
/// scoped caller has in skeleton mode, where there is no group store to expand.
pub(super) fn sees_nothing() -> NodeScope {
    NodeScope::Groups(Arc::new(crate::api::scope::ScopeSet {
        visible: vec![Uuid::from_u128(1)],
        breadcrumb: Vec::new(),
    }))
}

pub(super) fn flow_params(node_id: Uuid) -> TopFlowsParams {
    TopFlowsParams {
        node_id: Some(node_id),
        kind: None,
        from: None,
        to: None,
        limit: None,
        proto: None,
        port: None,
        peer: None,
        asn: None,
        dir: None,
    }
}

pub(super) fn neighbor_params(node_id: Uuid) -> NeighborsParams {
    NeighborsParams {
        node_id,
        history_limit: None,
        before_at: None,
        before_id: None,
    }
}
