// SPDX-License-Identifier: AGPL-3.0-only
//! The northbound API's method/path inventory, pinned by a test.
//!
//! `api.rs` was one 13.7k-line file whose `router()` was a single 393-line expression registering
//! every endpoint. Splitting that per domain means moving route registrations between files, and a
//! dropped `.route(...)` line would not fail to compile — the endpoint would simply 404 at runtime,
//! most likely noticed by a user rather than by CI.
//!
//! So the inventory is recorded here and checked three ways:
//!  1. every pair below is actually served by [`super::router`] (nothing was lost in a move),
//!  2. no pair is listed twice (a domain module was not merged in twice under a different path), and
//!  3. every pair is **described by the OpenAPI document** (ADR-035), which is what the WebUI's
//!     types and client are generated from — an endpoint missing from the document is invisible to
//!     TypeScript exactly as an endpoint missing from the router is invisible at runtime.
//!
//! Check 3 is why this ledger survived the move to generated contracts rather than being replaced
//! by it. `utoipa-axum` would have paired the route and its `#[utoipa::path]` in one call, but it is
//! built against axum 0.8; with plain utoipa the two declarations sit in the same file and nothing
//! in the language ties them together. This does.
//!
//! **This list is not a wish list — it is a ledger.** Adding an endpoint means adding its line
//! here in the same commit; deleting one means deleting its line, deliberately.
//!
//! ## The third column: what each route does about RBAC group scope (ADR-014)
//!
//! Scope filtering is the kind of thing that is applied to the twenty endpoints someone thought of
//! and silently missing from the twenty-first — and a miss fails **open**, returning the whole
//! fleet with no error anywhere. So it is not left to memory: every route carries a [`Scoping`]
//! rule, and the tests below check the handler's signature actually matches what its line claims.
//!
//! Adding the column was itself the mechanism. Widening the tuple made every line fail to compile
//! at once, which forced one deliberate pass over the whole surface with a human deciding each
//! entry, rather than a sweep that stops when the obvious ones are done. From here on a new
//! endpoint cannot be added without answering the question, because its ledger line does not
//! compile without a rule.
//!
//! ## The fourth column: can the MCP surface answer this? (ADR-042)
//!
//! Same shape, same reason, one increment later. The MCP tool surface started as a read-only
//! subset and `api-conventions.md` step 6 said "*if* AI clients should see it" for a year, so every
//! feature since was free to skip it. Each omission was individually reasonable; the sum was 110
//! GETs against 17 tools, with the missing ones including per-interface history, fleet-wide
//! rankings, CDP/LLDP adjacency, and listing the maintenance windows the MCP surface can itself
//! create.
//!
//! So the rule is now **read parity** — every read the WebUI can reach, `/mcp` can reach — and it
//! is recorded per route rather than remembered. [`Mcp::Pending`] is the honest landing place for a
//! gap and is ratcheted downward by `the_mcp_gap_only_moves_deliberately`.
//!
//! **Writes are out of scope by decision** (ADR-042 decision 6): MCP stays read-only, the write
//! surface is frozen at three tools, and [`NO_MCP_WRITE`] is the one shared exemption.
//!
//! One caveat worth stating precisely, because someone will test it with `cargo check` and conclude
//! the mechanism is broken: this module is `#[cfg(test)]`, so "cannot be added without deciding"
//! binds under `cargo test` — which is what CI runs — not under `cargo build`. That has always been
//! true of the third column too.

/// What a route does about the caller's group scope.
///
/// [`Self::Global`] and [`Self::Pending`] both carry a **reason string**, and it is required to be
/// non-empty. That is the whole defence against the lazy path: without it, `Global` would be the
/// variant that always compiles, and an unscoped node-returning endpoint would sail through review
/// looking considered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Scoping {
    /// Addresses one node — or one stored row that names a node or a folder group as its target
    /// (a mute, a maintenance window). Takes `VisibleNode` (a `:node_id` path param) or checks the
    /// id it was handed against the caller's scope.
    ///
    /// Out of scope answers `404`, never `403` — see `extract::VisibleNode`.
    NodeScoped,
    /// The scope becomes a **predicate in the store query**, so the answer is already narrow when it
    /// arrives: `group_id = ANY(…)` against `nodes` for the SQL lists, or the caller's resolved
    /// node-id set for a store that has never heard of groups (`/events/stats`, both backends).
    GroupFiltered,
    /// Ranked, aggregated or streamed by something that does not know about groups (the alert
    /// engine, VictoriaMetrics, ClickHouse, VictoriaLogs), so the filter runs after the answer.
    PostFiltered,
    /// Refuses a group-scoped caller outright, because it cannot yet honour one. The same
    /// fail-closed shape `POST /api/v1/api-tokens` and `/mcp` already use, and for the same reason:
    /// a number that quietly covers the whole fleet is worse than a refusal.
    Refused(&'static str),
    /// ⚠️ **Not scoped yet** — the reason names what is missing. This exists so the gap lives in the
    /// ledger, where a reviewer sees it, instead of in a plan file nobody reads. The count is
    /// ratcheted by a test: it may go down, never up.
    ///
    /// **Currently unconstructed, and that is the goal state, not dead code.** Every route now has a
    /// real rule; this variant is the declared landing place for the next one that does not, so that
    /// adding an unscoped endpoint is a visible ledger entry with a written reason rather than a
    /// silently missing filter. Deleting it would remove the only honest way to record a gap and
    /// leave `Global` — which means "deliberately unscoped" — as the path of least resistance.
    #[allow(dead_code)]
    Pending(&'static str),
    /// Deliberately unscoped, with the reason recorded. Infrastructure, deployment-wide config,
    /// admin-only administration, or a resource with no node attribution at all.
    Global(&'static str),
}

// `Pending` is deliberately absent from this list while no route needs it — see its doc comment.
use Scoping::{Global, GroupFiltered, NodeScoped, PostFiltered, Refused};

/// Whether the MCP surface can answer the question this route answers (ADR-042).
///
/// The rule is **read parity**: every read reachable from the WebUI is reachable from `/mcp`. It
/// exists because the opposite was the default — `api-conventions.md` step 6 read "*if* AI clients
/// should see it" for a year, so each feature was free to skip MCP, and the surface drifted from "a
/// subset" into "whatever happened to get wired". Individually every omission was reasonable. The
/// sum was not: 110 GETs against 17 tools.
///
/// Every variant but [`Self::Tool`] carries a **non-empty reason**, for exactly the purpose
/// [`Scoping::Global`]'s does — without it the exemption is the variant that always compiles, and a
/// GET-only capability sails through review looking considered.
///
/// **Writes are out of scope by decision, not by backlog** (ADR-042 decision 6). MCP stays
/// read-only; the write surface is frozen at the three ADR-028 WS-E incident-response tools. A new
/// `PUT`/`POST`/`DELETE` takes [`NO_MCP_WRITE`] and moves on — its `GET` counterpart does not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mcp {
    /// An `#[tool]` in `mcp/tools.rs` answers this. The string is the **tool name**, which rmcp
    /// derives from the `async fn` name (no `name =` override exists today);
    /// `every_named_mcp_tool_exists` checks it against that source.
    ///
    /// **One tool may answer several routes.** The six `/flow/*` aggregations are one `top_flows`;
    /// `/nodes/:node_id/interfaces` is part of `get_node_status`. That is what parity means here —
    /// the same questions can be answered, not a 1:1 route map. A tool per endpoint would be ~60
    /// tools, and a model picks worse from a list that long.
    Tool(&'static str),
    /// Deliberately **no** tool, with the reason recorded: session plumbing, the contract document,
    /// the probes, machine-to-machine ingest, the caller's own UI layout, secret material, or a
    /// rendering of what other tools already answer.
    ///
    /// A reason that names its replacement (\"tools X and Y compose this\") is falsifiable and
    /// therefore worth more than one that asserts irrelevance.
    Exempt(&'static str),
    /// ⚠️ **A gap** — the reason names what is missing and which ADR-042 increment is expected to
    /// close it. Ratcheted by `the_mcp_gap_only_moves_deliberately`: the count may go down, never
    /// up.
    ///
    /// Unlike [`Scoping::Pending`], which is unconstructed because scoping is finished, this one
    /// starts high on purpose. The gap was always there; the column is what makes it visible.
    Pending(&'static str),
}

use Mcp::{Exempt, Pending, Tool};

/// Reasons reused across many admin-only configuration routes, so the ledger stays readable and the
/// same justification is not re-worded twenty times.
const ADMIN_CFG: Scoping = Global("admin-only configuration; an Admin is unscoped by construction");
const INFRA: Scoping = Global("infrastructure, not monitored-node data");
const DEPLOY_WIDE: Scoping = Global("deployment-wide, identical for every caller");
const ACCOUNT: Scoping = Global("account/session identity, not node data");
/// A **report** artefact. Refused to a group-scoped caller rather than filtered, because a saved run
/// is rendered output over fleet-wide sections and retains no node attribution to filter by — see
/// `api/reports.rs::reports_are_fleet_wide`.
const REPORT: Scoping = Refused(
    "a rendered report has no per-node attribution left to filter, so a scoped caller is refused \
     rather than shown the fleet or told there are no reports",
);
/// A number that was **already summed** before this endpoint saw it — the fleet state timeline
/// (snapshotted as `(ts, state, count)`) and fleet throughput (`sum()` inside VictoriaMetrics).
/// Neither retains a node id, so there is nothing to filter and nothing to join; narrowing them
/// means changing how they are *recorded*, which is a feature rather than a filter.
const PRE_AGGREGATED: Scoping = Refused(
    "the value arrives already summed with no per-node attribution, so it can be served whole or \
     not at all; a group-scoped caller gets the refusal rather than the fleet's numbers",
);

/// The write routes. MCP is read-only and the write surface is frozen (ADR-042 decision 6).
///
/// **This is the only shared exemption, and that asymmetry is the design.** Making the write case
/// cheap is what keeps the column from being resented into uselessness; making the *read* case
/// cheap is what would kill it — a generic `TODO_MCP` would be the one spelling that requires no
/// thought, so it would become the only one anybody writes. Outside the three `PENDING_*` families
/// below, every read writes its own sentence.
const NO_MCP_WRITE: Mcp = Exempt(
    "MCP is read-only; the write surface is frozen at ack_alert/open_maintenance/poll_now \
     (ADR-042 decision 6)",
);

// `PENDING_CONFIG_READ` lived here and is gone: ADR-042 I3b shipped `get_config(kind=…)`, so all 28
// routes that carried it — thresholds, event rules, profiles, templates, classification rules, the
// MIB catalog, notification channels, forwarding destinations, Meraki, discovery, the per-node check
// configs and the OIDC/LDAP/retention/neighbour settings — now name a tool. Deleted rather than kept
// for the next occasion, same as `PENDING_INFRA` below: a shared `Pending` const outliving its
// family is how a gap stops being visible as one.
//
// The warning it carried survives in `mcp/folded.rs`, which is where it now does work rather than
// describing work: the permission is **not one value** across this family (`ManageConfig` ×14,
// `View` ×12, `ManageUsers` ×2), and one permission for the whole tool would either hand the
// identity-provider configuration to any viewer or refuse a viewer eleven reads the UI shows them.

/// The four SSE streams. Recorded as a gap rather than an exemption on purpose — `/mcp` declares
/// `enable_tools()` only, so there is no subscription transport, and that is a missing capability
/// rather than a decision. The polling tools answer the same questions one snapshot at a time.
const PENDING_STREAM: Mcp = Pending(
    "ADR-042: MCP declares enable_tools() only, so there is no subscription transport for a live \
     stream; the polling tools answer the same question one snapshot at a time",
);

// `PENDING_INFRA` lived here and is gone: ADR-042 I3a shipped `get_system_health`, so every route
// that carried it now names a tool. A shared `Pending` const outliving its family is how a gap
// stops being visible as one — the const is deleted rather than kept for the next occasion.

/// Every `(method, path, scoping, mcp)` the router serves, sorted by path. Axum path params keep
/// their `:name` form; the test substitutes a value that parses for every extractor in use.
pub(crate) const ROUTES: &[(&str, &str, Scoping, Mcp)] = &[
    ("GET", "/api/v1/alerts", PostFiltered, Tool("get_active_alerts")),
    ("POST", "/api/v1/alerts/ack", NodeScoped, Tool("ack_alert")),
    (
        "GET",
        "/api/v1/alerts/calendar",
        GroupFiltered,
        Tool("alert_trends"),
    ),
    (
        "GET",
        "/api/v1/alerts/history",
        PostFiltered,
        Tool("get_alert_history"),
    ),
    (
        "GET",
        "/api/v1/alerts/top-nodes",
        PostFiltered,
        Tool("alert_trends"),
    ),
    (
        "GET",
        "/api/v1/alerts/transitions",
        PostFiltered,
        Tool("alert_trends"),
    ),
    (
        "GET",
        "/api/v1/analysis/findings",
        GroupFiltered,
        Tool("search_analysis_findings"),
    ),
    (
        "GET",
        "/api/v1/analysis/jobs",
        PostFiltered,
        Tool("list_analyses"),
    ),
    (
        "POST",
        "/api/v1/analysis/jobs",
        NodeScoped,
        // A read wearing POST: it changes no configuration and forces `notify = false` on the MCP
        // side. The ledger records the tool that exists, not the method it happens to use.
        Tool("run_analysis"),
    ),
    (
        "GET",
        "/api/v1/analysis/jobs/:id",
        NodeScoped,
        Tool("get_analysis_findings"),
    ),
    (
        "POST",
        "/api/v1/analysis/jobs/:id/cancel",
        NodeScoped,
        NO_MCP_WRITE,
    ),
    (
        "GET",
        "/api/v1/analysis/jobs/:id/findings",
        PostFiltered,
        Tool("get_analysis_findings"),
    ),
    (
        "GET",
        "/api/v1/analysis/schedules",
        PostFiltered,
        // `list_analyses(kind="schedules")` — folded onto the runs list because both answer "what
        // analysis is there" and a schedule is what a run will be.
        Tool("list_analyses"),
    ),
    (
        "POST",
        "/api/v1/analysis/schedules",
        NodeScoped,
        NO_MCP_WRITE,
    ),
    (
        "PUT",
        "/api/v1/analysis/schedules/:id",
        NodeScoped,
        NO_MCP_WRITE,
    ),
    (
        "DELETE",
        "/api/v1/analysis/schedules/:id",
        NodeScoped,
        NO_MCP_WRITE,
    ),
    (
        "GET",
        "/api/v1/api-tokens",
        ADMIN_CFG,
        Exempt(
            "the token inventory is the credential material for this very surface; a client \
             enumerating the tokens that authenticate it is an access question, not a monitoring \
             one",
        ),
    ),
    ("POST", "/api/v1/api-tokens", ADMIN_CFG, NO_MCP_WRITE),
    ("DELETE", "/api/v1/api-tokens/:id", ADMIN_CFG, NO_MCP_WRITE),
    (
        "GET",
        "/api/v1/audit",
        Global(
            "admin-only, and audit rows carry no node attribution to filter on (ADR-014 non-goal)",
        ),
        Tool("get_audit"),
    ),
    (
        "GET",
        "/api/v1/audit/export.csv",
        Global(
            "admin-only, and audit rows carry no node attribution to filter on (ADR-014 non-goal)",
        ),
        // The same rows `get_audit` already answers, in a file format. An MCP client asking for
        // CSV would be asking this surface to render a spreadsheet for it, which is what the tool
        // result already is in a form a model can actually read.
        Exempt("the CSV rendering of get_audit's answer; the data is reachable via that tool"),
    ),
    ("POST", "/api/v1/auth/login", ACCOUNT, NO_MCP_WRITE),
    ("POST", "/api/v1/auth/logout", ACCOUNT, NO_MCP_WRITE),
    (
        "GET",
        "/api/v1/auth/me",
        ACCOUNT,
        Exempt(
            "an MCP caller arrives already authenticated by its own token, and its identity is in \
             every tool's RequestContext",
        ),
    ),
    (
        "GET",
        "/api/v1/auth/oidc/authorize",
        ACCOUNT,
        Exempt(
            "a browser redirect into an external IdP; a tool surface has no interactive login leg",
        ),
    ),
    ("POST", "/api/v1/auth/oidc/callback", ACCOUNT, NO_MCP_WRITE),
    (
        "GET",
        "/api/v1/classification-rules",
        ADMIN_CFG,
        Tool("get_config"),
    ),
    (
        "POST",
        "/api/v1/classification-rules",
        ADMIN_CFG,
        NO_MCP_WRITE,
    ),
    (
        "DELETE",
        "/api/v1/classification-rules/:id",
        ADMIN_CFG,
        NO_MCP_WRITE,
    ),
    (
        "PUT",
        "/api/v1/classification-rules/:id",
        ADMIN_CFG,
        NO_MCP_WRITE,
    ),
    (
        "GET",
        "/api/v1/collection-templates",
        ADMIN_CFG,
        Tool("get_config"),
    ),
    (
        "POST",
        "/api/v1/collection-templates",
        ADMIN_CFG,
        NO_MCP_WRITE,
    ),
    (
        "DELETE",
        "/api/v1/collection-templates/:id",
        ADMIN_CFG,
        NO_MCP_WRITE,
    ),
    (
        "GET",
        "/api/v1/collection-templates/:id/items",
        ADMIN_CFG,
        Tool("get_config"),
    ),
    (
        "POST",
        "/api/v1/collection-templates/:id/items",
        ADMIN_CFG,
        NO_MCP_WRITE,
    ),
    (
        "DELETE",
        "/api/v1/collection-templates/:id/items/:item_id",
        ADMIN_CFG,
        NO_MCP_WRITE,
    ),
    (
        "DELETE",
        "/api/v1/collection/:item_id",
        ADMIN_CFG,
        NO_MCP_WRITE,
    ),
    (
        "GET",
        "/api/v1/config",
        DEPLOY_WIDE,
        Tool("get_system_health"),
    ),
    ("PUT", "/api/v1/config", ADMIN_CFG, NO_MCP_WRITE),
    (
        "GET",
        "/api/v1/config/bundle",
        ADMIN_CFG,
        Exempt(
            "a whole-deployment configuration export is a migration artefact, not a question about \
             the network; every setting inside it is separately readable",
        ),
    ),
    ("POST", "/api/v1/config/bundle", ADMIN_CFG, NO_MCP_WRITE),
    (
        "GET",
        "/api/v1/credentials",
        ADMIN_CFG,
        Exempt(
            "the monitoring-credential inventory; ADR-018 keeps secret material out of every tool \
             result, and the operational question (is a credential failing) is credentials/health",
        ),
    ),
    ("POST", "/api/v1/credentials", ADMIN_CFG, NO_MCP_WRITE),
    (
        "GET",
        "/api/v1/credentials/health",
        ADMIN_CFG,
        Tool("get_system_health"),
    ),
    ("DELETE", "/api/v1/credentials/:id", ADMIN_CFG, NO_MCP_WRITE),
    ("PUT", "/api/v1/credentials/:id", ADMIN_CFG, NO_MCP_WRITE),
    (
        "GET",
        "/api/v1/dashboard",
        Global("the caller's own widget layout; the widgets' queries are scoped individually"),
        Exempt(
            "widget layout is presentation, and every widget's underlying query is separately \
             reachable as its own tool",
        ),
    ),
    (
        "PUT",
        "/api/v1/dashboard",
        Global("the caller's own widget layout"),
        NO_MCP_WRITE,
    ),
    (
        "GET",
        "/api/v1/discovered-endpoints",
        GroupFiltered,
        // Not folded into `get_topology`: a discovered endpoint is inventory, not graph structure —
        // it has no edges, deliberately (ADR-043 I3) — so `kind=` would be a switch between two
        // unrelated answers rather than the same question narrowed, which is the fold criterion.
        Tool("list_discovered_endpoints"),
    ),
    (
        "POST",
        "/api/v1/discovered-endpoints/:id/import",
        ADMIN_CFG,
        NO_MCP_WRITE,
    ),
    (
        "GET",
        "/api/v1/discovery/candidates",
        ADMIN_CFG,
        Tool("get_config"),
    ),
    ("POST", "/api/v1/discovery/import", ADMIN_CFG, NO_MCP_WRITE),
    ("POST", "/api/v1/discovery/scan", ADMIN_CFG, NO_MCP_WRITE),
    (
        "GET",
        "/api/v1/discovery/scan/:id",
        ADMIN_CFG,
        Tool("get_config"),
    ),
    ("POST", "/api/v1/dns-monitors", ADMIN_CFG, NO_MCP_WRITE),
    (
        "GET",
        "/api/v1/event-rules",
        ADMIN_CFG,
        Tool("get_config"),
    ),
    ("POST", "/api/v1/event-rules", ADMIN_CFG, NO_MCP_WRITE),
    ("DELETE", "/api/v1/event-rules/:id", ADMIN_CFG, NO_MCP_WRITE),
    ("PUT", "/api/v1/event-rules/:id", ADMIN_CFG, NO_MCP_WRITE),
    ("POST", "/api/v1/event-rules/test", ADMIN_CFG, NO_MCP_WRITE),
    (
        "GET",
        "/api/v1/event-sources",
        ADMIN_CFG,
        Tool("get_config"),
    ),
    ("POST", "/api/v1/event-sources", ADMIN_CFG, NO_MCP_WRITE),
    (
        "DELETE",
        "/api/v1/event-sources/:id",
        ADMIN_CFG,
        NO_MCP_WRITE,
    ),
    ("PUT", "/api/v1/event-sources/:id", ADMIN_CFG, NO_MCP_WRITE),
    (
        "POST",
        "/api/v1/event-sources/:id/rotate-token",
        ADMIN_CFG,
        NO_MCP_WRITE,
    ),
    ("GET", "/api/v1/events", PostFiltered, Tool("search_events")),
    (
        "GET",
        "/api/v1/events/stats",
        GroupFiltered,
        Tool("event_stats"),
    ),
    (
        "GET",
        "/api/v1/fleet/coverage",
        GroupFiltered,
        Tool("get_fleet_summary"),
    ),
    (
        "GET",
        "/api/v1/fleet/group-summary",
        PostFiltered,
        // `list_node_groups(include_state=true)` — the tally is joined onto the folder it belongs
        // to, because the bare `group_id → counts` map has no names for a model to reason with.
        Tool("list_node_groups"),
    ),
    (
        "GET",
        "/api/v1/fleet/state-history",
        PRE_AGGREGATED,
        Tool("fleet_state_history"),
    ),
    (
        "GET",
        "/api/v1/fleet/summary",
        PostFiltered,
        Tool("get_fleet_summary"),
    ),
    // The six fleet-wide flow aggregations. `top_flows` takes an optional `node_id` and mirrors the
    // REST refusal for a group-scoped caller when it is omitted (ADR-042 I1).
    (
        "GET",
        "/api/v1/flow/conversations",
        PRE_AGGREGATED,
        Tool("top_flows"),
    ),
    (
        "GET",
        "/api/v1/flow/protocols",
        PRE_AGGREGATED,
        Tool("top_flows"),
    ),
    (
        "GET",
        "/api/v1/flow/series",
        PRE_AGGREGATED,
        Tool("top_flows"),
    ),
    (
        "GET",
        "/api/v1/flow/top-as",
        PRE_AGGREGATED,
        Tool("top_flows"),
    ),
    (
        "GET",
        "/api/v1/flow/top-ports",
        PRE_AGGREGATED,
        Tool("top_flows"),
    ),
    (
        "GET",
        "/api/v1/flow/top-talkers",
        PRE_AGGREGATED,
        Tool("top_flows"),
    ),
    (
        "GET",
        "/api/v1/forwarding/destinations",
        ADMIN_CFG,
        Tool("get_config"),
    ),
    (
        "POST",
        "/api/v1/forwarding/destinations",
        ADMIN_CFG,
        NO_MCP_WRITE,
    ),
    (
        "DELETE",
        "/api/v1/forwarding/destinations/:id",
        ADMIN_CFG,
        NO_MCP_WRITE,
    ),
    (
        "PUT",
        "/api/v1/forwarding/destinations/:id",
        ADMIN_CFG,
        NO_MCP_WRITE,
    ),
    (
        "POST",
        "/api/v1/forwarding/destinations/:id/test",
        ADMIN_CFG,
        NO_MCP_WRITE,
    ),
    (
        "GET",
        "/api/v1/forwarding/status",
        ADMIN_CFG,
        Tool("get_system_health"),
    ),
    (
        "POST",
        "/api/v1/ingest/webhook/:source_id",
        Global("token-authenticated ingest from a device, not a user-facing read"),
        Exempt("a device posts to this; it is machine-to-machine ingest, not a question anyone asks"),
    ),
    ("GET", "/api/v1/llm/config", ADMIN_CFG, Tool("get_config")),
    ("PUT", "/api/v1/llm/config", ADMIN_CFG, NO_MCP_WRITE),
    ("POST", "/api/v1/llm/test", ADMIN_CFG, NO_MCP_WRITE),
    (
        "DELETE",
        "/api/v1/maintenance-windows",
        // Clears the *ended* windows, and like its GET sibling it reads the rows and filters them
        // afterwards rather than pushing the scope into a store predicate.
        PostFiltered,
        NO_MCP_WRITE,
    ),
    (
        "GET",
        "/api/v1/maintenance-windows",
        PostFiltered,
        // The headline asymmetry ADR-042 was written over — open_maintenance could create a window
        // and nothing on this surface could read one back. Closed by I2.
        Tool("list_suppressions"),
    ),
    (
        "POST",
        "/api/v1/maintenance-windows",
        NodeScoped,
        Tool("open_maintenance"),
    ),
    (
        "DELETE",
        "/api/v1/maintenance-windows/:id",
        NodeScoped,
        NO_MCP_WRITE,
    ),
    (
        "PUT",
        "/api/v1/maintenance-windows/:id",
        NodeScoped,
        NO_MCP_WRITE,
    ),
    (
        "POST",
        "/api/v1/maintenance-windows/:id/end",
        // Same row, same visibility rule as deleting it — `require_visible_window`.
        NodeScoped,
        NO_MCP_WRITE,
    ),
    ("POST", "/api/v1/meraki/import", ADMIN_CFG, NO_MCP_WRITE),
    ("GET", "/api/v1/meraki/orgs", ADMIN_CFG, Tool("get_config")),
    ("POST", "/api/v1/meraki/orgs", ADMIN_CFG, NO_MCP_WRITE),
    ("DELETE", "/api/v1/meraki/orgs/:id", ADMIN_CFG, NO_MCP_WRITE),
    (
        "PUT",
        "/api/v1/meraki/orgs/:id/cadence",
        ADMIN_CFG,
        NO_MCP_WRITE,
    ),
    (
        "PUT",
        "/api/v1/meraki/orgs/:id/enabled",
        ADMIN_CFG,
        NO_MCP_WRITE,
    ),
    (
        "POST",
        "/api/v1/meraki/orgs/:id/enumerate",
        ADMIN_CFG,
        NO_MCP_WRITE,
    ),
    (
        "GET",
        "/api/v1/meraki/orgs/:id/networks",
        ADMIN_CFG,
        Tool("get_config"),
    ),
    (
        "PUT",
        "/api/v1/meraki/orgs/:id/networks",
        ADMIN_CFG,
        NO_MCP_WRITE,
    ),
    (
        "POST",
        "/api/v1/meraki/orgs/discover",
        ADMIN_CFG,
        NO_MCP_WRITE,
    ),
    (
        "GET",
        "/api/v1/meraki/polling",
        ADMIN_CFG,
        Tool("get_config"),
    ),
    ("PUT", "/api/v1/meraki/polling", ADMIN_CFG, NO_MCP_WRITE),
    (
        "GET",
        "/api/v1/metrics/interface-delta",
        PostFiltered,
        // Folded: `rank_by = delta_up | delta_down`.
        Tool("top_interfaces"),
    ),
    (
        "GET",
        "/api/v1/metrics/interface-heatmap",
        PostFiltered,
        Exempt(
            "a links × time grid is the pre-joined rendering of two tools that both exist: \
             top_interfaces picks the links and returns (node_id, ifindex), get_interface_series \
             gives each one its history. A matrix shaped for cell-shading is a visualisation, not \
             an answer a model reads better than the two calls that compose it",
        ),
    ),
    (
        "GET",
        "/api/v1/metrics/interface-top",
        PostFiltered,
        Tool("top_interfaces"),
    ),
    (
        "GET",
        "/api/v1/metrics/throughput-range",
        PRE_AGGREGATED,
        Tool("fleet_throughput"),
    ),
    ("GET", "/api/v1/metrics/top", PostFiltered, Tool("top_metrics")),
    ("GET", "/api/v1/mib-catalog", ADMIN_CFG, Tool("get_config")),
    ("POST", "/api/v1/mib-catalog", ADMIN_CFG, NO_MCP_WRITE),
    ("DELETE", "/api/v1/mib-catalog/:id", ADMIN_CFG, NO_MCP_WRITE),
    ("GET", "/api/v1/monitoring-gaps", INFRA, Tool("get_system_health")),
    (
        "GET",
        "/api/v1/mutes",
        PostFiltered,
        Tool("list_suppressions"),
    ),
    ("POST", "/api/v1/mutes", NodeScoped, NO_MCP_WRITE),
    ("DELETE", "/api/v1/mutes/:id", NodeScoped, NO_MCP_WRITE),
    (
        "GET",
        "/api/v1/node-groups",
        GroupFiltered,
        Tool("list_node_groups"),
    ),
    ("POST", "/api/v1/node-groups", ADMIN_CFG, NO_MCP_WRITE),
    ("DELETE", "/api/v1/node-groups/:id", ADMIN_CFG, NO_MCP_WRITE),
    ("PUT", "/api/v1/node-groups/:id", ADMIN_CFG, NO_MCP_WRITE),
    ("PUT", "/api/v1/node-groups/:id/geo", ADMIN_CFG, NO_MCP_WRITE),
    (
        "PUT",
        "/api/v1/node-groups/:id/placement",
        ADMIN_CFG,
        NO_MCP_WRITE,
    ),
    (
        "PUT",
        "/api/v1/node-groups/:id/pool",
        ADMIN_CFG,
        NO_MCP_WRITE,
    ),
    (
        "POST",
        "/api/v1/node-names",
        GroupFiltered,
        Exempt(
            "a batch id→name resolver for the WebUI's table cells; tools resolve names internally \
             (YagraMcp::resolve_names) and never hand a bare id to the client",
        ),
    ),
    ("GET", "/api/v1/nodes", GroupFiltered, Tool("list_nodes")),
    ("POST", "/api/v1/nodes", ADMIN_CFG, NO_MCP_WRITE),
    ("DELETE", "/api/v1/nodes/:node_id", ADMIN_CFG, NO_MCP_WRITE),
    (
        "GET",
        "/api/v1/nodes/:node_id",
        NodeScoped,
        Tool("get_node_status"),
    ),
    (
        "GET",
        "/api/v1/nodes/:node_id/assignment",
        NodeScoped,
        Tool("get_system_health"),
    ),
    (
        "PUT",
        "/api/v1/nodes/:node_id/bindings",
        ADMIN_CFG,
        NO_MCP_WRITE,
    ),
    (
        "GET",
        "/api/v1/nodes/:node_id/collection",
        NodeScoped,
        Tool("get_config"),
    ),
    (
        "POST",
        "/api/v1/nodes/:node_id/collection",
        ADMIN_CFG,
        NO_MCP_WRITE,
    ),
    (
        "GET",
        "/api/v1/nodes/:node_id/dns-chain",
        NodeScoped,
        Tool("get_dns_chain"),
    ),
    (
        "GET",
        "/api/v1/nodes/:node_id/dns-chain/history",
        NodeScoped,
        Tool("get_dns_chain"),
    ),
    (
        "DELETE",
        "/api/v1/nodes/:node_id/dns-check",
        ADMIN_CFG,
        NO_MCP_WRITE,
    ),
    (
        "GET",
        "/api/v1/nodes/:node_id/dns-check",
        NodeScoped,
        Tool("get_config"),
    ),
    (
        "PUT",
        "/api/v1/nodes/:node_id/dns-check",
        ADMIN_CFG,
        NO_MCP_WRITE,
    ),
    (
        "GET",
        "/api/v1/nodes/:node_id/flow/conversations",
        NodeScoped,
        Tool("top_flows"),
    ),
    (
        "GET",
        "/api/v1/nodes/:node_id/flow/protocols",
        NodeScoped,
        Tool("top_flows"),
    ),
    (
        "GET",
        "/api/v1/nodes/:node_id/flow/series",
        NodeScoped,
        Tool("top_flows"),
    ),
    (
        "GET",
        "/api/v1/nodes/:node_id/flow/top-as",
        NodeScoped,
        Tool("top_flows"),
    ),
    (
        "GET",
        "/api/v1/nodes/:node_id/flow/top-ports",
        NodeScoped,
        Tool("top_flows"),
    ),
    (
        "GET",
        "/api/v1/nodes/:node_id/flow/top-talkers",
        NodeScoped,
        Tool("top_flows"),
    ),
    (
        "PUT",
        "/api/v1/nodes/:node_id/group",
        ADMIN_CFG,
        NO_MCP_WRITE,
    ),
    (
        "GET",
        "/api/v1/nodes/:node_id/interfaces",
        NodeScoped,
        // Folded: `NodeStatusDto` carries the interface list. Parity is about which questions can
        // be answered, so a route answered inside a larger tool is answered.
        Tool("get_node_status"),
    ),
    (
        "GET",
        "/api/v1/nodes/:node_id/interfaces/:ifindex/series",
        NodeScoped,
        // `query_metrics` stays node-level (`SeriesKey::node`); this is its per-interface twin,
        // deliberately a second tool rather than an `ifindex` param, because the aligned four-series
        // answer is not what an extra parameter on `query_metrics` would produce.
        Tool("get_interface_series"),
    ),
    (
        "GET",
        "/api/v1/nodes/:node_id/metrics",
        NodeScoped,
        // Not folded into `query_metrics`: that tool answers "what is this series doing", and this
        // one answers "which series exist" — the question a client has to settle first.
        Tool("list_node_metrics"),
    ),
    (
        "GET",
        "/api/v1/nodes/:node_id/metrics/:metric",
        NodeScoped,
        Tool("query_metrics"),
    ),
    (
        "GET",
        "/api/v1/nodes/:node_id/metrics/:metric/range",
        NodeScoped,
        Tool("query_metrics"),
    ),
    (
        "PUT",
        "/api/v1/nodes/:node_id/maintenance-exemption",
        NodeScoped,
        NO_MCP_WRITE,
    ),
    (
        "PUT",
        "/api/v1/nodes/:node_id/mute-exemption",
        NodeScoped,
        NO_MCP_WRITE,
    ),
    (
        "GET",
        "/api/v1/nodes/:node_id/neighbors",
        NodeScoped,
        Tool("get_neighbors"),
    ),
    (
        "GET",
        "/api/v1/nodes/:node_id/neighbors/history",
        NodeScoped,
        // Folded: `get_neighbors` returns the current adjacency and recent changes together, since
        // that is one troubleshooting question rather than two.
        Tool("get_neighbors"),
    ),
    (
        "PUT",
        "/api/v1/nodes/:node_id/parent",
        ADMIN_CFG,
        NO_MCP_WRITE,
    ),
    (
        "PUT",
        "/api/v1/nodes/:node_id/placement",
        ADMIN_CFG,
        NO_MCP_WRITE,
    ),
    (
        "POST",
        "/api/v1/nodes/:node_id/poll",
        NodeScoped,
        Tool("poll_now"),
    ),
    (
        "PUT",
        "/api/v1/nodes/:node_id/pool",
        ADMIN_CFG,
        NO_MCP_WRITE,
    ),
    (
        "GET",
        "/api/v1/nodes/:node_id/status",
        NodeScoped,
        Tool("get_node_status"),
    ),
    (
        "DELETE",
        "/api/v1/nodes/:node_id/url-check",
        ADMIN_CFG,
        NO_MCP_WRITE,
    ),
    (
        "GET",
        "/api/v1/nodes/:node_id/url-check",
        NodeScoped,
        Tool("get_config"),
    ),
    (
        "PUT",
        "/api/v1/nodes/:node_id/url-check",
        ADMIN_CFG,
        NO_MCP_WRITE,
    ),
    (
        "GET",
        "/api/v1/nodes/by-group",
        GroupFiltered,
        // Still deliberately not `Tool("list_nodes")` — the tree-grouped projection is not what
        // that tool returns, and claiming it would be the first dishonest line in this column.
        // Exempt instead, on the reassessment this line asked for: `list_node_groups` shipped in
        // I1 and gained the state tally in I2, so the two tools now carry every field this
        // projection has. Naming them makes the exemption falsifiable — delete either tool and
        // this reason becomes checkably wrong.
        Exempt(
            "the folder-grouped node tree is the pre-joined rendering of two tools that both \
             exist: list_node_groups gives the folders (with their tallies), list_nodes gives the \
             members and takes a group filter",
        ),
    ),
    (
        "GET",
        "/api/v1/nodes/search",
        GroupFiltered,
        Tool("list_nodes"),
    ),
    (
        "GET",
        "/api/v1/notification-channels",
        ADMIN_CFG,
        Tool("get_config"),
    ),
    (
        "POST",
        "/api/v1/notification-channels",
        ADMIN_CFG,
        NO_MCP_WRITE,
    ),
    (
        "DELETE",
        "/api/v1/notification-channels/:id",
        ADMIN_CFG,
        NO_MCP_WRITE,
    ),
    (
        "PUT",
        "/api/v1/notification-channels/:id",
        ADMIN_CFG,
        NO_MCP_WRITE,
    ),
    (
        "PUT",
        "/api/v1/notification-channels/:id/template",
        ADMIN_CFG,
        NO_MCP_WRITE,
    ),
    (
        "POST",
        "/api/v1/notification-channels/preview",
        ADMIN_CFG,
        // A read wearing POST, and still no tool: it renders operator-supplied template text, and
        // MCP has no way to save one — the write surface is frozen (ADR-042 decision 6). A preview
        // of something you cannot then store is not a capability.
        Exempt(
            "renders a notification template the caller cannot save, since MCP has no write path \
             for one; the alert data it renders against is the fixed preview sample, not live \
             inventory",
        ),
    ),
    (
        "GET",
        "/api/v1/notification-channels/template-variables",
        ADMIN_CFG,
        // Documentation, in the same family as `/openapi.json`: a static catalogue of the names a
        // template may use. It names no node and no group, and it varies only with the Yagra
        // version.
        Exempt(
            "a static catalogue of notification-template variable names, closer to the contract \
             document than to monitoring data; it returns nothing about the fleet",
        ),
    ),
    (
        "GET",
        "/api/v1/openapi.json",
        Global("the API contract document itself"),
        Exempt("the REST contract document; an MCP client reads tools/list, not OpenAPI"),
    ),
    ("GET", "/api/v1/poller-health", INFRA, Tool("get_system_health")),
    ("GET", "/api/v1/pollers", INFRA, Tool("get_system_health")),
    (
        "PUT",
        "/api/v1/nodes/:node_id/suppression-opt-out",
        NodeScoped,
        NO_MCP_WRITE,
    ),
    ("DELETE", "/api/v1/pollers/:id", INFRA, NO_MCP_WRITE),
    ("PUT", "/api/v1/pollers/:id/anchor", INFRA, NO_MCP_WRITE),
    (
        "GET",
        "/api/v1/pollers/:id/nodes",
        PostFiltered,
        Tool("get_system_health"),
    ),
    ("GET", "/api/v1/pools", INFRA, Tool("get_system_health")),
    ("GET", "/api/v1/profiles", ADMIN_CFG, Tool("get_config")),
    ("POST", "/api/v1/profiles", ADMIN_CFG, NO_MCP_WRITE),
    ("DELETE", "/api/v1/profiles/:id", ADMIN_CFG, NO_MCP_WRITE),
    ("PUT", "/api/v1/profiles/:id", ADMIN_CFG, NO_MCP_WRITE),
    (
        "GET",
        "/api/v1/profiles/:id/templates",
        ADMIN_CFG,
        Tool("get_config"),
    ),
    (
        "PUT",
        "/api/v1/profiles/:id/templates",
        ADMIN_CFG,
        NO_MCP_WRITE,
    ),
    (
        "POST",
        "/api/v1/rca",
        NodeScoped,
        // A read wearing POST, like `POST /analysis/jobs` — it changes no configuration. What is
        // undecided is whether an MCP client (itself an LLM) should be able to spend a second LLM
        // call, rather than reading the findings and explaining them itself. ADR-042 OPEN (1).
        Tool("run_rca"),
    ),
    (
        "GET",
        "/api/v1/reports/definitions",
        ADMIN_CFG,
        Tool("get_config"),
    ),
    (
        "POST",
        "/api/v1/reports/definitions",
        ADMIN_CFG,
        NO_MCP_WRITE,
    ),
    (
        "DELETE",
        "/api/v1/reports/definitions/:id",
        ADMIN_CFG,
        NO_MCP_WRITE,
    ),
    (
        "PUT",
        "/api/v1/reports/definitions/:id",
        ADMIN_CFG,
        NO_MCP_WRITE,
    ),
    (
        "POST",
        "/api/v1/reports/definitions/:id/run",
        ADMIN_CFG,
        NO_MCP_WRITE,
    ),
    (
        "GET",
        "/api/v1/reports/runs",
        REPORT,
        Tool("get_report_runs"),
    ),
    (
        "DELETE",
        "/api/v1/reports/runs/:id",
        ADMIN_CFG,
        NO_MCP_WRITE,
    ),
    (
        "GET",
        "/api/v1/reports/runs/:id",
        REPORT,
        Tool("get_report_runs"),
    ),
    (
        "GET",
        "/api/v1/reports/runs/:id/export",
        REPORT,
        Exempt(
            "a rendered byte stream (PDF/CSV) has no MCP transport; /reports/runs/{id} carries the \
             same content as structured sections",
        ),
    ),
    (
        "GET",
        "/api/v1/reports/schedules",
        ADMIN_CFG,
        Tool("get_config"),
    ),
    (
        "POST",
        "/api/v1/reports/schedules",
        ADMIN_CFG,
        NO_MCP_WRITE,
    ),
    (
        "DELETE",
        "/api/v1/reports/schedules/:id",
        ADMIN_CFG,
        NO_MCP_WRITE,
    ),
    (
        "PUT",
        "/api/v1/reports/schedules/:id",
        ADMIN_CFG,
        NO_MCP_WRITE,
    ),
    (
        "GET",
        "/api/v1/reports/sections",
        Global("a static catalog of report building blocks"),
        Exempt(
            "the building blocks for composing a report definition, which is a write this surface \
             does not offer; the catalog is only useful to something that can author one",
        ),
    ),
    (
        "GET",
        "/api/v1/roles",
        DEPLOY_WIDE,
        Tool("get_config"),
    ),
    (
        "GET",
        "/api/v1/routing-rules",
        ADMIN_CFG,
        Tool("get_config"),
    ),
    ("POST", "/api/v1/routing-rules", ADMIN_CFG, NO_MCP_WRITE),
    (
        "DELETE",
        "/api/v1/routing-rules/:id",
        ADMIN_CFG,
        NO_MCP_WRITE,
    ),
    ("PUT", "/api/v1/routing-rules/:id", ADMIN_CFG, NO_MCP_WRITE),
    (
        "GET",
        "/api/v1/settings/ldap",
        ADMIN_CFG,
        Tool("get_config"),
    ),
    ("PUT", "/api/v1/settings/ldap", ADMIN_CFG, NO_MCP_WRITE),
    (
        "POST",
        "/api/v1/settings/ldap/test",
        ADMIN_CFG,
        NO_MCP_WRITE,
    ),
    (
        "GET",
        "/api/v1/settings/neighbors",
        Global("deployment-wide adjacency-collection policy; it names no node and no group"),
        Tool("get_config"),
    ),
    (
        "PUT",
        "/api/v1/settings/neighbors",
        ADMIN_CFG,
        NO_MCP_WRITE,
    ),
    // ⚠️ Write-only, and there is deliberately no `GET` beside it. The mode is already reported by
    // `GET /api/v1/topology/shadow`, which has a tool — so the read exists without a second route
    // to serve it. Adding the symmetric GET "for consistency" would mean either a new `get_config`
    // kind nobody asked for or a fresh `Pending`, and `MCP_PENDING` only moves down.
    (
        "PUT",
        "/api/v1/settings/topology",
        ADMIN_CFG,
        NO_MCP_WRITE,
    ),
    (
        "GET",
        "/api/v1/settings/oidc",
        ADMIN_CFG,
        Tool("get_config"),
    ),
    ("POST", "/api/v1/settings/oidc", ADMIN_CFG, NO_MCP_WRITE),
    (
        "DELETE",
        "/api/v1/settings/oidc/:id",
        ADMIN_CFG,
        NO_MCP_WRITE,
    ),
    ("PUT", "/api/v1/settings/oidc/:id", ADMIN_CFG, NO_MCP_WRITE),
    (
        "GET",
        "/api/v1/settings/retention",
        Global("deployment-wide retention policy; the same for every group, and it names no node"),
        Tool("get_config"),
    ),
    (
        "PUT",
        "/api/v1/settings/retention",
        ADMIN_CFG,
        NO_MCP_WRITE,
    ),
    (
        "GET",
        "/api/v1/settings/tls",
        ADMIN_CFG,
        // Written out rather than joining the config-read family, and I3b is why that mattered:
        // that family was deferred work and this one is a decision. Had it been filed with them it
        // would now be a `get_config` kind serving the deployment's TLS material, carried in by a
        // sweep rather than by anyone deciding.
        Exempt(
            "the deployment's own TLS material — the private key never leaves the server, and \
             certificate metadata answers no question about the monitored fleet that a model would \
             ask. Certificate *expiry* is a monitoring question and is answered by the system-health \
             tool, not by exposing this",
        ),
    ),
    ("PUT", "/api/v1/settings/tls", ADMIN_CFG, NO_MCP_WRITE),
    (
        "POST",
        "/api/v1/settings/tls/regenerate",
        ADMIN_CFG,
        NO_MCP_WRITE,
    ),
    (
        "GET",
        "/api/v1/shared-dashboard",
        Global("one admin-edited layout shown to everyone; its widgets' queries are scoped"),
        Exempt(
            "widget layout is presentation, and every widget's underlying query is separately \
             reachable as its own tool",
        ),
    ),
    (
        "PUT",
        "/api/v1/shared-dashboard",
        ADMIN_CFG,
        NO_MCP_WRITE,
    ),
    (
        "GET",
        "/api/v1/stream/alerts",
        PostFiltered,
        PENDING_STREAM,
    ),
    (
        "GET",
        "/api/v1/stream/analysis",
        PostFiltered,
        PENDING_STREAM,
    ),
    (
        "GET",
        "/api/v1/stream/node-states",
        PostFiltered,
        PENDING_STREAM,
    ),
    ("GET", "/api/v1/stream/report-runs", REPORT, PENDING_STREAM),
    (
        "GET",
        "/api/v1/suppression-exemptions",
        // Each row names one node, so the rows are read and filtered afterwards — the same shape as
        // the window and mute lists it belongs beside.
        PostFiltered,
        // Folded into the existing tool rather than given its own: "what is currently suppressing
        // alerts" and "which node has been taken out of that" are one question, and a model that
        // reads the first without the second concludes a released node is still silenced.
        Tool("list_suppressions"),
    ),
    (
        "GET",
        "/api/v1/system-health",
        Global("core's own health, not monitored-node data"),
        Tool("get_system_health"),
    ),
    (
        "GET",
        "/api/v1/system/hosts",
        Global("core and poller host metrics, not monitored nodes"),
        Tool("get_system_health"),
    ),
    (
        "GET",
        "/api/v1/system/hosts/:instance/metrics/range",
        Global("core and poller host metrics, not monitored nodes"),
        Tool("get_system_health"),
    ),
    (
        "GET",
        "/api/v1/system/upgrade",
        Global(
            "describes the deployment itself — which binary is running and how much schema it \
             carries — not monitored-node data, and it demands ManageConfig, whose holder is \
             unscoped by construction (ADR-014)",
        ),
        Tool("get_system_health"),
    ),
    (
        "POST",
        "/api/v1/system/upgrade",
        Global("replaces every container in the deployment; there is no narrower unit of it"),
        NO_MCP_WRITE,
    ),
    (
        "POST",
        "/api/v1/system/upgrade/check",
        Global("refreshes the deployment's own view of what releases exist; there is no per-group release list"),
        NO_MCP_WRITE,
    ),
    (
        "POST",
        "/api/v1/system/upgrade/bundle",
        Global("installs an uploaded release across every container in the deployment"),
        NO_MCP_WRITE,
    ),
    (
        "PUT",
        "/api/v1/system/upgrade/enabled",
        Global("a deployment-wide switch; there is no per-group upgrade to permit or refuse"),
        NO_MCP_WRITE,
    ),
    (
        "GET",
        "/api/v1/system/support-bundle",
        Global(
            "a diagnostic snapshot of the deployment itself; it demands ManageConfig + \
             ManageCredentials + ViewAudit, and a caller holding all three is unscoped by \
             construction (ADR-014)",
        ),
        Exempt(
            "a gzipped tar has no MCP transport, and every section inside it is already a tool: \
             get_system_health covers health/*, get_active_alerts covers alerts/, get_audit covers \
             audit/. What has no tool is the archive, not the answers",
        ),
    ),
    ("GET", "/api/v1/thresholds", ADMIN_CFG, Tool("get_config")),
    ("POST", "/api/v1/thresholds", ADMIN_CFG, NO_MCP_WRITE),
    ("DELETE", "/api/v1/thresholds/:id", ADMIN_CFG, NO_MCP_WRITE),
    (
        "GET",
        "/api/v1/topology",
        GroupFiltered,
        Tool("get_topology"),
    ),
    (
        "GET",
        "/api/v1/topology/links",
        // Both endpoints must be visible, not either — a link with one visible end would tell a
        // scoped operator that a node exists outside their scope.
        GroupFiltered,
        // Folded rather than given its own tool: `get_topology(kind="links")` answers this. Both
        // branches take the same `cursor`+`limit` and return a keyset page of graph structure,
        // which is ADR-042's fold criterion — a model picks worse from a longer tool list.
        Tool("get_topology"),
    ),
    (
        "GET",
        "/api/v1/topology/link-overrides",
        // Both endpoints visible, exactly as for the links themselves — a decision naming a node
        // the caller cannot see would disclose that the node exists.
        GroupFiltered,
        // Folded on the same criterion: `get_topology(kind="overrides")`. "Why is this edge here"
        // and "what did an operator decide about it" are one question a model asks together.
        Tool("get_topology"),
    ),
    (
        "POST",
        "/api/v1/topology/link-overrides",
        // The body names both endpoints, so the handler takes `Scoped` and checks each itself
        // (`scope::require_visible_node`) rather than relying on a path param.
        NodeScoped,
        NO_MCP_WRITE,
    ),
    (
        "DELETE",
        "/api/v1/topology/link-overrides/:id",
        NodeScoped,
        NO_MCP_WRITE,
    ),
    (
        "GET",
        "/api/v1/topology/shadow",
        GroupFiltered,
        // **Not exempt.** Asked "should derived suppression be enabled here", a model cannot
        // synthesize this from the other two tools without reimplementing `is_suppressed` over the
        // fleet's live down-set — which is exactly the kind of question read parity exists for.
        // Folded as `get_topology(kind="shadow")`.
        Tool("get_topology"),
    ),
    ("POST", "/api/v1/url-monitors", ADMIN_CFG, NO_MCP_WRITE),
    (
        "GET",
        "/api/v1/users",
        ADMIN_CFG,
        Exempt(
            "an API token is refused ManageUsers before its role is consulted \
             (extract::TOKEN_DENIED_PERMISSIONS), so a user-administration read is unreachable for \
             the token-authenticated case this surface exists for",
        ),
    ),
    ("POST", "/api/v1/users", ADMIN_CFG, NO_MCP_WRITE),
    ("DELETE", "/api/v1/users/:id", ADMIN_CFG, NO_MCP_WRITE),
    ("PUT", "/api/v1/users/:id/enabled", ADMIN_CFG, NO_MCP_WRITE),
    ("PUT", "/api/v1/users/:id/password", ADMIN_CFG, NO_MCP_WRITE),
    ("PUT", "/api/v1/users/:id/role", ADMIN_CFG, NO_MCP_WRITE),
    ("PUT", "/api/v1/users/:id/scope", ADMIN_CFG, NO_MCP_WRITE),
    (
        "GET",
        "/api/v1/version",
        DEPLOY_WIDE,
        Tool("get_system_health"),
    ),
    (
        "GET",
        "/healthz",
        Global("unauthenticated liveness probe"),
        Exempt("an unauthenticated container probe; /mcp is never anonymous"),
    ),
    (
        "GET",
        "/readyz",
        Global("unauthenticated readiness probe"),
        Exempt("an unauthenticated container probe; /mcp is never anonymous"),
    ),
];

/// Every `#[tool]` name `mcp/tools.rs` declares.
///
/// Source-text, like `openapi.rs`'s body guard and `registered_handlers` below, and partial in the
/// same deliberate way: an attribute shape this cannot parse is skipped rather than failed, and the
/// callers assert a floor so "the parser stopped matching" cannot pass for "all clear".
///
/// Note the trick this does **not** need: `tools.rs::every_tool_takes_a_request_context` assembles
/// its needles at runtime because it reads its own file, where a literal would match itself and pass
/// forever. This reads a different file, so a literal is correct here — copying the workaround would
/// be cargo-culting a fix for a problem this code does not have.
///
/// Outside `mod tests` (though still test-only, since this whole module is `#[cfg(test)]`) because
/// `mcp/mod.rs` needs it too: the instructions string names tools, and one parser for "what tools
/// exist" is the point of having a parser at all.
pub(crate) fn declared_mcp_tools() -> std::collections::BTreeSet<String> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/mcp/tools.rs");
    let src = std::fs::read_to_string(path).expect("read src/mcp/tools.rs");
    let mut out = std::collections::BTreeSet::new();
    // `#[tool_router]` and `#[tool_handler(…)]` do not match this needle, so they cost nothing.
    for chunk in src.split("#[tool(").skip(1) {
        let Some((attr, after)) = chunk.split_once(")]") else {
            continue;
        };
        // rmcp derives the tool name from the fn name unless the attribute overrides it.
        // Nothing does today; parse it anyway rather than fail on a legitimate rmcp feature.
        let name = match attr
            .split_once("name = \"")
            .and_then(|(_, r)| r.split_once('"'))
        {
            Some((n, _)) => n.to_owned(),
            None => {
                let Some((_, sig)) = after.split_once("async fn ") else {
                    continue;
                };
                let Some(n) = sig.split('(').next() else {
                    continue;
                };
                n.trim().to_owned()
            }
        };
        if !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_') {
            out.insert(name);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{declared_mcp_tools, Mcp, Scoping, NO_MCP_WRITE, ROUTES};
    use crate::api::{router, tests_support::public_state};
    use axum::body::{to_bytes, Body};
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt as _;

    /// Fill axum path params with values that parse for every extractor the API uses today
    /// (`Uuid`, `IfIndex`/`u32`, and free-form string segments).
    fn concrete(path: &str) -> String {
        path.split('/')
            .map(|seg| match seg.strip_prefix(':') {
                None => seg.to_owned(),
                Some(name) if name.contains("ifindex") || name.contains("index") => "1".to_owned(),
                Some(name) if name.contains("id") => {
                    "00000000-0000-0000-0000-000000000000".to_owned()
                }
                Some(_) => "x".to_owned(),
            })
            .collect::<Vec<_>>()
            .join("/")
    }

    #[tokio::test]
    async fn every_listed_route_is_served() {
        // A route that is registered answers *something* — including its own typed 404, which
        // carries the ADR-019 envelope. Axum's fallback for an unrouted path answers 404 with an
        // EMPTY body, and a wrong method answers 405. That is the discriminator: a 404 with no
        // body means the route is genuinely missing, not that the resource wasn't found.
        let mut missing = Vec::new();
        for (method, path, _, _) in ROUTES {
            let app = router(public_state());
            let req = Request::builder()
                .method(*method)
                .uri(concrete(path))
                .header("content-type", "application/json")
                // A body for the mutating verbs; handlers may still reject it as malformed, which
                // is fine — we only care that routing reached a handler at all.
                .body(Body::from("{}"))
                .unwrap();
            let resp = app.oneshot(req).await.unwrap();
            let status = resp.status();
            let body = to_bytes(resp.into_body(), 1 << 20).await.unwrap();
            let unrouted = status == StatusCode::NOT_FOUND && body.is_empty();
            if unrouted || status == StatusCode::METHOD_NOT_ALLOWED {
                missing.push(format!("{method} {path} → {status}"));
            }
        }
        assert!(missing.is_empty(), "routes not served: {missing:#?}");
    }

    /// The paths the OpenAPI document describes, in ledger form (`{id}` → `:id`).
    fn documented() -> std::collections::BTreeSet<(String, String)> {
        let doc = crate::api::openapi::document();
        let mut out = std::collections::BTreeSet::new();
        for (path, item) in &doc.paths.paths {
            // OpenAPI writes params as `{name}`; the ledger and axum write them as `:name`.
            let ledger_path = path
                .split('/')
                .map(
                    |seg| match seg.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
                        Some(name) => format!(":{name}"),
                        None => seg.to_owned(),
                    },
                )
                .collect::<Vec<_>>()
                .join("/");
            for (method, op) in [
                ("GET", &item.get),
                ("PUT", &item.put),
                ("POST", &item.post),
                ("DELETE", &item.delete),
                ("PATCH", &item.patch),
                ("HEAD", &item.head),
            ] {
                if op.is_some() {
                    out.insert((method.to_owned(), ledger_path.clone()));
                }
            }
        }
        out
    }

    #[test]
    fn every_listed_route_is_described_by_the_openapi_document() {
        // The WebUI's types and client are generated from this document, so an endpoint absent from
        // it cannot be called from TypeScript at all — the generated `paths` type simply has no key
        // for it. That is a compile error rather than a 404, but it is the same class of loss, and
        // like a dropped `.route()` it produces no Rust compile error on its own.
        let documented = documented();
        let missing: Vec<_> = ROUTES
            .iter()
            .filter(|(m, p, _, _)| !documented.contains(&((*m).to_owned(), (*p).to_owned())))
            .map(|(m, p, _, _)| (m, p))
            .collect();
        assert!(
            missing.is_empty(),
            "routes served but absent from the OpenAPI document \
             (add `#[utoipa::path]` and list the handler in the domain's `Doc`): {missing:#?}"
        );
    }

    #[test]
    fn the_openapi_document_describes_nothing_the_router_does_not_serve() {
        // The other direction, and the one the ledger test alone cannot catch: a stale
        // `#[utoipa::path]` left behind after its route moved or was deleted generates a TypeScript
        // method for an endpoint that answers 404. A client written against it fails at runtime,
        // which is exactly the failure mode this whole mechanism exists to remove.
        let served: std::collections::BTreeSet<_> = ROUTES
            .iter()
            .map(|(m, p, _, _)| ((*m).to_owned(), (*p).to_owned()))
            .collect();
        let extra: Vec<_> = documented()
            .into_iter()
            .filter(|r| !served.contains(r))
            .collect();
        assert!(extra.is_empty(), "documented but not served: {extra:#?}");
    }

    #[test]
    fn the_ledger_has_no_duplicates() {
        // Keyed on (method, path) only: two lines for the same endpoint are a duplicate however
        // they classify it — and if they classified it *differently*, that is worse, not exempt.
        let mut seen = std::collections::BTreeSet::new();
        let dupes: Vec<_> = ROUTES
            .iter()
            .filter(|(m, p, _, _)| !seen.insert((*m, *p)))
            .collect();
        assert!(dupes.is_empty(), "duplicate route entries: {dupes:?}");
    }

    // ── The scoping column, checked against the handlers ──────────────────────

    /// `(method, path) → (module, handler fn name)`, parsed out of every `routes()` in `api/`.
    ///
    /// Source-text, like `openapi.rs`'s body guard, and partial in the same deliberate way: a
    /// registration shape this cannot parse is skipped rather than failed, and the callers assert a
    /// floor on how many they compared so "the parser stopped matching" cannot pass for "all clear".
    fn registered_handlers() -> std::collections::BTreeMap<(String, String), (String, String)> {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/api");
        let mut out = std::collections::BTreeMap::new();
        for entry in std::fs::read_dir(&dir).expect("read src/api") {
            let path = entry.expect("dir entry").path();
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let file = path
                .file_name()
                .and_then(|f| f.to_str())
                .unwrap_or("?")
                .to_owned();
            let src = std::fs::read_to_string(&path).expect("read module");
            for chunk in src.split(".route(").skip(1) {
                // The route path is the first string literal after `.route(`.
                let Some(rest) = chunk.split_once('"').map(|(_, r)| r) else {
                    continue;
                };
                let Some((route_path, after)) = rest.split_once('"') else {
                    continue;
                };
                if !route_path.starts_with('/') {
                    continue;
                }
                // Then the `method(handler)` pairs, up to the end of this registration. Bounded by
                // the next `.route(` (already handled by the split) and by the `routes()` fn end.
                let reg = after.split("\n}").next().unwrap_or(after);
                for (verb, method) in [
                    ("get(", "GET"),
                    ("post(", "POST"),
                    ("put(", "PUT"),
                    ("delete(", "DELETE"),
                    ("patch(", "PATCH"),
                ] {
                    let mut cursor = reg;
                    while let Some((_, tail)) = cursor.split_once(verb) {
                        let name: String = tail
                            .chars()
                            .take_while(|c| c.is_alphanumeric() || *c == '_')
                            .collect();
                        if !name.is_empty() {
                            out.insert(
                                (method.to_owned(), route_path.to_owned()),
                                (file.clone(), name),
                            );
                        }
                        cursor = tail;
                    }
                }
            }
        }
        out
    }

    /// The parameter list of `async fn <name>` in `api/<file>`, or `None` if it cannot be found.
    ///
    /// Handlers generated by a macro (the twelve flow endpoints) are declared as `async fn $node_fn`
    /// and cannot be located by name — those are skipped, which is why the floor assertions matter.
    fn handler_signature(file: &str, name: &str) -> Option<String> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/api")
            .join(file);
        let src = std::fs::read_to_string(path).ok()?;
        let needle = format!("async fn {name}(");
        let (_, after) = src.split_once(&needle)?;
        Some(after.split_once(')')?.0.to_owned())
    }

    /// A route that says it is node-scoped must actually take the guard that enforces it.
    ///
    /// This is the half of the mechanism the ledger alone cannot provide: a line can *claim*
    /// `NodeScoped` while the handler quietly returns the node to anyone. Path-param routes take
    /// `VisibleNode`; the operator writes that name their target in the body take `Scoped` and check
    /// it themselves (`scope::require_visible_node`), which is what the second arm allows.
    #[test]
    fn every_node_scoped_route_takes_a_scope_guard() {
        let handlers = registered_handlers();
        let mut checked = 0usize;
        let mut wrong = Vec::new();
        for (method, path, scoping, _) in ROUTES {
            if *scoping != Scoping::NodeScoped {
                continue;
            }
            let Some((file, name)) = handlers.get(&((*method).to_owned(), (*path).to_owned()))
            else {
                continue; // macro-generated or an unparsed registration shape
            };
            let Some(sig) = handler_signature(file, name) else {
                continue;
            };
            checked += 1;
            let guarded = sig.contains("VisibleNode") || sig.contains("Scoped");
            if !guarded {
                wrong.push(format!("{method} {path} → {file}::{name}"));
            }
        }
        assert!(
            checked >= 15,
            "only checked {checked} node-scoped handlers — the parser stopped matching"
        );
        assert!(
            wrong.is_empty(),
            "ledger says NodeScoped but the handler takes no scope guard:\n  {}",
            wrong.join("\n  ")
        );
    }

    /// Same, for the list/aggregate rules: the handler must take `Scoped`.
    #[test]
    fn every_filtered_route_takes_the_scope_extractor() {
        let handlers = registered_handlers();
        let mut checked = 0usize;
        let mut wrong = Vec::new();
        for (method, path, scoping, _) in ROUTES {
            if !matches!(
                scoping,
                Scoping::GroupFiltered | Scoping::PostFiltered | Scoping::Refused(_)
            ) {
                continue;
            }
            let Some((file, name)) = handlers.get(&((*method).to_owned(), (*path).to_owned()))
            else {
                continue;
            };
            let Some(sig) = handler_signature(file, name) else {
                continue;
            };
            checked += 1;
            if !sig.contains("Scoped") {
                wrong.push(format!("{method} {path} → {file}::{name}"));
            }
        }
        assert!(
            checked >= 10,
            "only checked {checked} filtered handlers — the parser stopped matching"
        );
        assert!(
            wrong.is_empty(),
            "ledger says the route is scope-filtered but the handler never asks for the scope:\n  {}",
            wrong.join("\n  ")
        );
    }

    /// Every deliberate exemption states why, in the ledger, where a reviewer sees it.
    ///
    /// Cheap, and the point is not the assertion — it is that `Global` cannot be the variant that
    /// always compiles. Without a required reason it would be, and an unscoped node-returning
    /// endpoint would look reviewed.
    #[test]
    fn every_unscoped_route_states_why() {
        let mut silent = Vec::new();
        for (method, path, scoping, _) in ROUTES {
            let reason = match scoping {
                Scoping::Global(r) | Scoping::Pending(r) | Scoping::Refused(r) => *r,
                _ => continue,
            };
            if reason.trim().len() < 20 {
                silent.push(format!("{method} {path}"));
            }
        }
        assert!(
            silent.is_empty(),
            "these routes opt out of scoping without saying why: {silent:#?}"
        );
    }

    /// **A ratchet.** The `Pending` entries are the scoping work that is designed but not built;
    /// this pins how many there are, so finishing one means lowering the number and adding a new
    /// unscoped endpoint fails until someone either scopes it or argues it down in review.
    ///
    /// Deliberately an equality, not a `<=`: an upper bound lets the count drift down silently and
    /// then quietly absorb a new gap under the old ceiling.
    #[test]
    fn the_unscoped_route_count_only_moves_deliberately() {
        let pending = ROUTES
            .iter()
            .filter(|(_, _, s, _)| matches!(s, Scoping::Pending(_)))
            .count();
        assert_eq!(
            pending, 0,
            "the number of not-yet-scoped routes changed — if you scoped one, lower this; \
             if you added one, scope it instead"
        );
    }

    // ── The MCP column, checked against the tool source (ADR-042) ─────────────

    /// Tools with no REST counterpart, and why.
    ///
    /// This lives here rather than in `tools.rs` on purpose: "which surface answers what" is this
    /// file's subject, and a second list over there would be a second ledger.
    const MCP_ONLY_TOOLS: &[(&str, &str)] = &[(
        "flow_fanout",
        "per-source fan-out (distinct destinations and destination ports per source, for scan and \
         worm triage) is an aggregation no REST endpoint exposes, so no route line can claim it",
    )];

    /// How many routes are recorded as an MCP gap (ADR-042).
    ///
    /// **Deliberately an equality, not a `<=`** — an upper bound lets the count drift down silently
    /// and then absorb a new gap under the old ceiling. Same reasoning as
    /// `the_unscoped_route_count_only_moves_deliberately`, with one difference: `Scoping::Pending`
    /// sits at 0 because scoping is finished, while this starts high and falls per increment.
    ///
    /// The number counts **routes**, where the v0.1.20 audit that prompted ADR-042 counted
    /// **capabilities** (~30). One capability is routinely 2–4 routes — neighbours is 2, Meraki is
    /// 3 — so the two figures are not meant to reconcile.
    ///
    /// **Since I3b the remaining four are the `/stream/*` SSE routes, and they are a different kind
    /// of number.** Every earlier value was a backlog that the next increment would spend down;
    /// this one is not, because MCP declares `enable_tools()` and there is no subscription
    /// transport to write a tool against. It stays `Pending` rather than becoming `Exempt` because
    /// it is a missing capability rather than a decision — but nothing is planned to move it, so a
    /// reader should not take a non-zero count as work in flight.
    const MCP_PENDING: usize = 4;

    #[test]
    fn every_named_mcp_tool_exists() {
        let tools = declared_mcp_tools();
        assert!(
            tools.len() >= 34,
            "only parsed {} #[tool] declarations — the parser drifted",
            tools.len()
        );

        let mut named = 0usize;
        let mut missing = Vec::new();
        for (method, path, _, mcp) in ROUTES {
            let Mcp::Tool(name) = mcp else { continue };
            named += 1;
            if !tools.contains(*name) {
                missing.push(format!("{method} {path} → {name}"));
            }
        }
        // The second floor is the load-bearing one. Without it, mass-converting `Tool(…)` lines to
        // `Pending(…)` to make a build green would leave this test passing while the gap silently
        // grew — which is the exact regression the column exists to prevent. Raise it as increments
        // land; it is a floor, so shipping tools never trips it.
        assert!(
            named >= 100,
            "only {named} ledger lines name a tool — the column is being emptied"
        );
        assert!(
            missing.is_empty(),
            "the ledger names a tool that mcp/tools.rs does not declare:\n  {}",
            missing.join("\n  ")
        );
    }

    /// The other direction, and the more interesting failure: **a tool ships while its route's
    /// ledger line still says `Pending`**, so the gap count never falls and nobody notices the work
    /// is done.
    #[test]
    fn every_mcp_tool_is_claimed_by_a_route_or_declared_mcp_only() {
        let claimed: std::collections::BTreeSet<&str> = ROUTES
            .iter()
            .filter_map(|(_, _, _, m)| match m {
                Mcp::Tool(n) => Some(*n),
                _ => None,
            })
            .collect();
        let orphans: Vec<_> = declared_mcp_tools()
            .into_iter()
            .filter(|t| {
                !claimed.contains(t.as_str()) && !MCP_ONLY_TOOLS.iter().any(|(n, _)| n == t)
            })
            .collect();
        assert!(
            orphans.is_empty(),
            "these tools exist but no ledger line names them — if a route one answers still says \
             Pending, flip it (and lower MCP_PENDING); if it genuinely has no REST counterpart, add \
             it to MCP_ONLY_TOOLS with a reason: {orphans:?}"
        );
    }

    /// The MCP-only list is an exemption list, so it needs the same hygiene as the others:
    /// its entries must still exist, and each must say why.
    #[test]
    fn the_mcp_only_list_stays_current() {
        let tools = declared_mcp_tools();
        for (name, reason) in MCP_ONLY_TOOLS {
            assert!(
                tools.contains(*name),
                "MCP_ONLY_TOOLS names `{name}`, which mcp/tools.rs no longer declares"
            );
            assert!(
                reason.trim().len() >= 20,
                "MCP_ONLY_TOOLS entry `{name}` does not say why it has no REST counterpart"
            );
        }
    }

    #[test]
    fn the_mcp_gap_only_moves_deliberately() {
        let pending = ROUTES
            .iter()
            .filter(|(_, _, _, m)| matches!(m, Mcp::Pending(_)))
            .count();
        assert_eq!(
            pending, MCP_PENDING,
            "the number of reads with no MCP tool changed — if you shipped a tool, flip its ledger \
             line to Mcp::Tool and lower MCP_PENDING; if you added a read endpoint, give it a tool \
             or argue the Pending up in review"
        );
    }

    /// Mirror of `every_unscoped_route_states_why`, for the fourth column.
    #[test]
    fn every_mcp_exemption_states_why() {
        let mut silent = Vec::new();
        for (method, path, _, mcp) in ROUTES {
            let reason = match mcp {
                Mcp::Exempt(r) | Mcp::Pending(r) => *r,
                Mcp::Tool(_) => continue,
            };
            if reason.trim().len() < 20 {
                silent.push(format!("{method} {path}"));
            }
        }
        assert!(
            silent.is_empty(),
            "these routes opt out of MCP without saying why: {silent:#?}"
        );
    }

    /// The one shared exemption is for **writes**. Pasting it onto a GET is how this column dies:
    /// it is the only spelling that requires no thought, so it is the only one that needs a guard.
    ///
    /// Note what this does *not* assert: that no non-GET route may be `Pending`. Reads wearing POST
    /// are real here — `POST /analysis/jobs` and `POST /rca` change no configuration — and they
    /// belong in the gap count like any other read.
    #[test]
    fn the_write_exemption_is_only_used_on_writes() {
        let wrong: Vec<_> = ROUTES
            .iter()
            .filter(|(m, _, _, mcp)| *m == "GET" && *mcp == NO_MCP_WRITE)
            .map(|(m, p, _, _)| format!("{m} {p}"))
            .collect();
        assert!(
            wrong.is_empty(),
            "these reads are marked with the read-only-surface exemption, which is for writes; \
             give them a tool or their own reason: {wrong:#?}"
        );
    }
}
