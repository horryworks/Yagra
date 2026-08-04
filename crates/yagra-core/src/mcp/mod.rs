// SPDX-License-Identifier: AGPL-3.0-only
//! MCP server (ADR-028, Phase 4 inward-facing AI tool surface).
//!
//! Exposes yagra-core's **read** seams to an MCP client (Claude Code/Desktop, an oncall operator's
//! AI client, or later the internal RCA agent of ADR-029) over **Streamable HTTP**. The transport is
//! an [`rmcp`] `StreamableHttpService` mounted into the existing axum `serve()` at `/mcp`, gated OFF
//! by default behind `YAGRA_ENABLE_MCP` ([`crate::config::Config::enable_mcp`]); when off the route is
//! not mounted (a request 404s), byte-identical to pre-MCP behavior (ADR-017, additive/N-1-safe).
//!
//! **Auth** is enforced in [`mcp_auth_mw`] before any tool runs: a valid bearer token (an API token,
//! [`crate::apitokens`], or a session token, [`crate::auth`]) with `View`. MCP is **always
//! authenticated even under `public_dashboard`** — an AI surface must never be anonymous.
//!
//! **Group-scoped principals are admitted** (WS-F, closed). They were refused here until every tool
//! filtered, because admitting one would have handed it the whole fleet — the refusal *was* the
//! enforcement. Each tool now resolves the caller's scope from the identity below
//! ([`tools::YagraMcp::scope_of`]) and applies the same rule its REST counterpart does. Note what
//! that trade means: a tool that forgets to ask now fails **open**, where before it could not,
//! which is why `tools.rs` pins "every tool takes a `RequestContext`" with a test — taking one is
//! the only way a tool body can reach the caller at all.
//!
//! The authenticated principal is propagated to tool bodies as an [`McpIdentity`] in the request
//! extensions (WS-D): rmcp forwards the HTTP request `Parts` into each tool's `RequestContext`, so a
//! **write** tool reads it back and enforces its own [`Permission`] (`ack_alert`→`AckAlerts`,
//! `open_maintenance`→`ManageMaintenance`, `poll_now`→`ManageConfig`) and records an audit entry. So
//! a Viewer token is read-only; an Operator/Admin token can also act. There are still **no
//! device-configuration tools** (monitoring lane, ADR-015/029), and no credential/secret is ever
//! placed in a tool result (ADR-018; enforced structurally in [`dto`] + its canary test).

mod dto;
mod folded;
mod tools;

use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::Response;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::model::{Implementation, ProtocolVersion, ServerCapabilities, ServerInfo};
use rmcp::transport::streamable_http_server::{
    session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
};
use rmcp::{tool_handler, ServerHandler};
use tokio_util::sync::CancellationToken;
use yagra_common::{Permission, Principal, TokenSurface};

use crate::api::ApiState;

/// Instructions shown to the MCP client at `initialize` — sets expectations for the model.
///
/// ⚠️ **Published verbatim to every client**, so a tool named here that does not exist is a wrong
/// specification shipped with confidence. `every_tool_named_in_the_instructions_exists` pins the
/// names; nothing pins the prose around them, so keep the claims narrow.
const INSTRUCTIONS: &str = "Yagra network-monitoring MCP. Read tools query live node status, alerts, \
    metrics (per node and per interface), fleet rankings, topology, CDP/LLDP adjacency, traffic \
    flows, and passive events (syslog/traps/webhooks), and run on-demand Troubleshoot analyses \
    (anomaly/correlation/capacity/flap). A few write tools act on the monitoring system — \
    acknowledge an alert (ack_alert), open a maintenance window (open_maintenance), or trigger an \
    immediate poll (poll_now); each needs an authorized token and is audited. There are still no \
    tools that configure or change network devices, or change Yagra's own configuration. Start with \
    get_fleet_summary, then drill in with list_nodes / get_node_status / get_active_alerts / \
    query_metrics / search_events. To find what is worst across the fleet use top_metrics (nodes) or \
    top_interfaces (interfaces); for one link's history use get_interface_series; for what a node is \
    cabled to use get_neighbors; for where things are filed use list_node_groups. Before concluding \
    a fleet is healthy, check list_suppressions — a silenced fleet looks quiet. For how alerting has \
    behaved over time use alert_trends, and to find what diagnostics have turned up across runs use \
    search_analysis_findings. Use run_analysis for deeper diagnosis (poll a long run with \
    get_analysis_findings), and run_rca to have a configured LLM explain one incident. Before \
    trusting any of the above, check get_system_health — if a poller is offline or a store is \
    unreachable, missing data means missing collection rather than a healthy quiet, and its \
    monitoring_gaps section names the windows where that was true. get_audit says who changed or \
    acknowledged what. Node ids are UUIDs; timestamps are RFC 3339 or Unix seconds per tool.";

/// The MCP server handler: holds the shared read state and the macro-generated tool router. Cheap to
/// clone (the state is all `Arc`s); a fresh instance is created per session by the transport factory.
#[derive(Clone)]
pub struct YagraMcp {
    /// Shared read seams (inventory, metrics, alerts, flows, …). The tools in [`tools`] wrap these.
    state: ApiState,
    /// Tool dispatch table generated by `#[tool_router]` (see [`tools`]).
    tool_router: ToolRouter<YagraMcp>,
}

/// The authenticated MCP caller, propagated to tool bodies (WS-D). `mcp_auth_mw` inserts this into
/// the HTTP request extensions after authentication; rmcp forwards the request `Parts` into each
/// tool's `RequestContext`, so a write tool reads it back to enforce a [`Permission`] and to attribute
/// its audit entry. `Clone` is required for storage in `http::Extensions`.
#[derive(Debug, Clone)]
pub(crate) struct McpIdentity {
    /// The resolved principal (role + scope) — write tools check `principal.can(perm)`.
    pub principal: Principal,
    /// A human-facing actor label for audit attribution: a session's username, or an API token's
    /// `owner (token:name)`. Never a secret (not the raw token).
    pub actor: String,
}

// `router = self.tool_router` dispatches through the router built once in `YagraMcp::new` (the
// default would rebuild it on every call).
#[tool_handler(router = self.tool_router)]
impl ServerHandler for YagraMcp {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::from_build_env())
            .with_protocol_version(ProtocolVersion::LATEST)
            .with_instructions(INSTRUCTIONS.to_owned())
    }
}

/// Build the auth-gated MCP sub-router. `serve()` nests it at `/mcp`; every request first passes
/// [`mcp_auth_mw`], then the `StreamableHttpService` handles the MCP protocol (POST messages, GET SSE,
/// DELETE session end). The `cancel` token (a child of the server shutdown token) lets in-flight MCP
/// sessions drain on shutdown (ADR-017).
pub fn build_router(state: ApiState, cancel: CancellationToken) -> axum::Router {
    let factory_state = state.clone();
    // rmcp's StreamableHttpService ships DNS-rebinding protection that rejects any `Host` header not
    // in `allowed_hosts` (default `localhost`/`127.0.0.1`/`::1`) with 403. Yagra is reached at an
    // operator-chosen host (a LAN IP, a hostname behind a proxy), so the default would 403 every real
    // client. We rely on our own **mandatory Bearer auth** (`mcp_auth_mw`, which runs *before* this
    // service) as the actual gate — a DNS-rebinding attacker's browser can't supply a valid token, so
    // it's stopped at 401 regardless of Host. Default: disable the allowlist (accept any Host).
    // Operators who still want Host pinning set `YAGRA_MCP_ALLOWED_HOSTS` (comma-separated).
    let hosts = mcp_allowed_hosts();
    let config = StreamableHttpServerConfig::default().with_cancellation_token(cancel);
    let config = if hosts.is_empty() {
        config.disable_allowed_hosts()
    } else {
        config.with_allowed_hosts(hosts)
    };
    let service = StreamableHttpService::new(
        move || Ok(YagraMcp::new(factory_state.clone())),
        LocalSessionManager::default().into(),
        config,
    );
    axum::Router::new()
        .fallback_service(service)
        .layer(axum::middleware::from_fn_with_state(state, mcp_auth_mw))
}

/// Operator-configured `Host`-header allowlist for the MCP endpoint (`YAGRA_MCP_ALLOWED_HOSTS`,
/// comma-separated, e.g. `yagra.example.com,192.168.1.2:8080`). Empty/unset ⇒ the allowlist is
/// disabled and any Host is accepted (Bearer auth remains the gate — see `build_router`).
fn mcp_allowed_hosts() -> Vec<String> {
    std::env::var("YAGRA_MCP_ALLOWED_HOSTS")
        .ok()
        .map(|raw| {
            raw.split(',')
                .map(|h| h.trim().to_owned())
                .filter(|h| !h.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// Authenticate every `/mcp` request before the MCP protocol handler runs. A denial short-circuits
/// with the standard error envelope; on success the resolved [`McpIdentity`] is inserted into the
/// request extensions so tool bodies can read it (WS-D — rmcp forwards the request `Parts` into each
/// tool's `RequestContext`). The scope gate stays in [`authenticate`]; per-tool `Permission` checks
/// live in the write tools themselves.
async fn mcp_auth_mw(State(st): State<ApiState>, mut req: Request, next: Next) -> Response {
    let token = crate::api::bearer(req.headers()).map(str::to_owned);
    match authenticate(&st, token.as_deref()).await {
        Ok(identity) => {
            req.extensions_mut().insert(identity);
            next.run(req).await
        }
        Err(resp) => {
            metrics::counter!("yagra_mcp_auth_failures_total").increment(1);
            resp
        }
    }
}

/// Resolve a bearer token to an [`McpIdentity`] permitted to use MCP, or an error `Response`. Routes
/// by token shape: a `yat_` API token → [`crate::apitokens::ApiTokenStore::verify`]; otherwise a
/// session token → [`crate::auth::SessionStore::authorize`]. Requires `View` and (Increment 1) global
/// scope.
async fn authenticate(st: &ApiState, token: Option<&str>) -> Result<McpIdentity, Response> {
    let Some(token) = token else {
        return Err(unauthorized("a valid bearer token is required"));
    };
    // The actor label is derived from the same resolution that authenticates, not from a second
    // lookup: a PAT used to be audited as the constant `mcp-token`, which named the surface rather
    // than anyone answerable for the call.
    let resolved = if crate::apitokens::is_api_token_shape(token) {
        // API tokens are backed by PostgreSQL (live mode only). Naming the surface is what keeps a
        // token minted for REST automation out of here, and vice versa.
        match st.admin.as_ref() {
            Some(admin) => admin
                .api_tokens
                .verify(token, TokenSurface::Mcp)
                .await
                .map(|auth| (auth.principal.clone(), auth.audit_actor())),
            None => None,
        }
    } else {
        st.sessions
            .authorize(Some(token), Permission::View)
            .ok()
            .map(|session| (session.principal, session.username))
    };
    let Some((principal, actor)) = resolved else {
        return Err(unauthorized("invalid or expired token"));
    };
    if !principal.can(Permission::View) {
        return Err(forbidden("this token lacks view permission"));
    }
    // A group-scoped principal is admitted (ADR-028 WS-F). This used to be refused here, because
    // the tools returned unfiltered data and admitting one would have handed it the fleet — the
    // refusal was the whole enforcement. Every tool now resolves the caller's scope from this
    // identity (`tools.rs::scope_of`) and filters, so the gate that replaced it is per tool.
    //
    // ⚠️ Which means a tool that forgets to ask fails **open** now, where it could not before.
    // `mcp/tools.rs` has a test asserting every `#[tool]` takes a `RequestContext`, since taking
    // one is the only way a body can reach the scope at all.
    Ok(McpIdentity { principal, actor })
}

fn unauthorized(message: &str) -> Response {
    crate::api::error_response(StatusCode::UNAUTHORIZED, "unauthorized", message.to_owned())
}

fn forbidden(message: &str) -> Response {
    crate::api::error_response(StatusCode::FORBIDDEN, "forbidden", message.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **Every tool [`INSTRUCTIONS`] names actually exists.**
    ///
    /// That string is handed to every client at `initialize` and read as the specification of this
    /// surface, but it is prose: sixteen tool names were hard-coded in it with nothing pinning them
    /// to `tools.rs`, so a rename or a removal would have shipped a wrong instruction to every AI
    /// client with no test failing. A model told to "use get_neighbors" for a tool that no longer
    /// exists does not fall back gracefully — it calls it, fails, and reasons from the failure.
    ///
    /// **One direction only.** The instructions are guidance, not a catalogue, so a tool that goes
    /// unmentioned is fine; a mentioned tool that does not exist is not.
    #[test]
    fn every_tool_named_in_the_instructions_exists() {
        // The same parser the route ledger uses, so there is one definition of "a declared tool".
        let declared = crate::api::route_table::declared_mcp_tools();
        assert!(
            declared.len() >= 33,
            "only found {} tool declarations; the parser drifted",
            declared.len()
        );
        // Tool names are the only lowercase_with_underscore words in the prose, which makes them
        // findable without a second list to keep in step. Since ADR-042 I3a a folded tool's
        // argument vocabulary — `monitoring_gaps`, `poller_nodes` — is published the same way and
        // is just as real, so those count as valid names too. What must not appear is a word that
        // *looks* like part of this surface and names nothing on it.
        let sections: Vec<&str> = crate::mcp::folded::FOLDED_READS
            .iter()
            .map(|f| f.arg)
            .collect();
        let named: Vec<&str> = INSTRUCTIONS
            .split(|c: char| !(c.is_alphanumeric() || c == '_'))
            .filter(|w| w.contains('_') && w.chars().all(|c| c.is_ascii_lowercase() || c == '_'))
            .collect();
        assert!(
            named.len() >= 15,
            "only found {} tool-shaped words in the instructions; the parser drifted",
            named.len()
        );
        let missing: Vec<&&str> = named
            .iter()
            .filter(|w| !declared.contains(**w) && !sections.contains(*w))
            .collect();
        assert!(
            missing.is_empty(),
            "the MCP instructions name tools or sections that do not exist, and that text is \
             published verbatim to every client: {missing:?}"
        );
    }
    use crate::auth::{LoginThrottle, SessionStore};
    use std::sync::Arc;
    use yagra_common::{Principal, Role, Scope};

    /// A minimal state whose only wired pieces are the session store (for token auth) and the flag.
    /// `admin` is `None`, so PAT auth is unavailable — session tokens are the auth path under test.
    fn state_with_sessions(sessions: Arc<SessionStore>) -> ApiState {
        use crate::alerts::AlertManager;
        use crate::sink::InMemorySink;
        use crate::store::MetricStore;
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
            sessions,
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
        }
    }

    #[tokio::test]
    async fn missing_token_is_unauthorized() {
        let st = state_with_sessions(Arc::new(SessionStore::new()));
        let resp = authenticate(&st, None).await.unwrap_err();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn invalid_token_is_unauthorized() {
        let st = state_with_sessions(Arc::new(SessionStore::new()));
        let resp = authenticate(&st, Some("not-a-real-token"))
            .await
            .unwrap_err();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn global_scope_viewer_session_is_accepted() {
        let sessions = Arc::new(SessionStore::new());
        let token = sessions.issue(
            uuid::Uuid::new_v4(),
            Principal::new(Role::Viewer, Scope::All),
            "viewer1",
        );
        let st = state_with_sessions(sessions);
        let identity = authenticate(&st, Some(&token)).await.expect("accepted");
        assert_eq!(identity.principal.role, Role::Viewer);
        assert_eq!(
            identity.actor, "viewer1",
            "a session is audited by its username"
        );
    }

    #[test]
    fn write_tool_permission_gate_intent() {
        use yagra_common::Permission;
        // A Viewer (the recommended MCP token role) can read but not act.
        let viewer = Principal::new(Role::Viewer, Scope::All);
        assert!(viewer.can(Permission::View));
        assert!(
            !viewer.can(Permission::AckAlerts),
            "viewer cannot ack_alert"
        );
        assert!(
            !viewer.can(Permission::ManageMaintenance),
            "viewer cannot open_maintenance"
        );
        assert!(
            !viewer.can(Permission::ManageConfig),
            "viewer cannot poll_now"
        );
        // An Admin can drive every write tool.
        let admin = Principal::new(Role::Admin, Scope::All);
        assert!(admin.can(Permission::AckAlerts));
        assert!(admin.can(Permission::ManageMaintenance));
        assert!(admin.can(Permission::ManageConfig));
    }

    #[tokio::test]
    async fn a_group_scoped_token_is_admitted_and_keeps_its_scope() {
        // The inverse of what this asserted until WS-F closed. The scope must arrive at the tools
        // *intact*: this gate is no longer the enforcement, so silently widening it here — or
        // dropping it — would hand a scoped token the fleet with every tool believing it had asked.
        let sessions = Arc::new(SessionStore::new());
        let scope = Scope::groups(["3f1b4c9e-0000-4000-8000-000000000001"]);
        let token = sessions.issue(
            uuid::Uuid::new_v4(),
            Principal::new(Role::Operator, scope.clone()),
            "op1",
        );
        let st = state_with_sessions(sessions);
        let identity = authenticate(&st, Some(&token)).await.expect("admitted");
        assert_eq!(identity.principal.scope, scope);
    }
}
