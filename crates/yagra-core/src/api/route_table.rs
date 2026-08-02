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
//! Adding the column was itself the mechanism. Widening the tuple made all 209 lines fail to
//! compile at once, which forced one deliberate pass over the whole surface with a human deciding
//! each entry, rather than a sweep that stops when the obvious ones are done. From here on a new
//! endpoint cannot be added without answering the question, because its ledger line does not
//! compile without a rule.

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

/// Every `(method, path, scoping)` the router serves, sorted by path. Axum path params keep their
/// `:name` form; the test substitutes a value that parses for every extractor in use.
pub(crate) const ROUTES: &[(&str, &str, Scoping)] = &[
    ("GET", "/api/v1/alerts", PostFiltered),
    ("POST", "/api/v1/alerts/ack", NodeScoped),
    ("GET", "/api/v1/alerts/calendar", GroupFiltered),
    ("GET", "/api/v1/alerts/history", PostFiltered),
    ("GET", "/api/v1/alerts/top-nodes", PostFiltered),
    ("GET", "/api/v1/alerts/transitions", PostFiltered),
    ("GET", "/api/v1/analysis/findings", GroupFiltered),
    ("GET", "/api/v1/analysis/jobs", PostFiltered),
    ("POST", "/api/v1/analysis/jobs", NodeScoped),
    ("GET", "/api/v1/analysis/jobs/:id", NodeScoped),
    ("POST", "/api/v1/analysis/jobs/:id/cancel", NodeScoped),
    ("GET", "/api/v1/analysis/jobs/:id/findings", PostFiltered),
    ("GET", "/api/v1/api-tokens", ADMIN_CFG),
    ("POST", "/api/v1/api-tokens", ADMIN_CFG),
    ("DELETE", "/api/v1/api-tokens/:id", ADMIN_CFG),
    (
        "GET",
        "/api/v1/audit",
        Global(
            "admin-only, and audit rows carry no node attribution to filter on (ADR-014 non-goal)",
        ),
    ),
    ("POST", "/api/v1/auth/login", ACCOUNT),
    ("POST", "/api/v1/auth/logout", ACCOUNT),
    ("GET", "/api/v1/auth/me", ACCOUNT),
    ("GET", "/api/v1/auth/oidc/authorize", ACCOUNT),
    ("POST", "/api/v1/auth/oidc/callback", ACCOUNT),
    ("GET", "/api/v1/classification-rules", ADMIN_CFG),
    ("POST", "/api/v1/classification-rules", ADMIN_CFG),
    ("DELETE", "/api/v1/classification-rules/:id", ADMIN_CFG),
    ("PUT", "/api/v1/classification-rules/:id", ADMIN_CFG),
    ("GET", "/api/v1/collection-templates", ADMIN_CFG),
    ("POST", "/api/v1/collection-templates", ADMIN_CFG),
    ("DELETE", "/api/v1/collection-templates/:id", ADMIN_CFG),
    ("GET", "/api/v1/collection-templates/:id/items", ADMIN_CFG),
    ("POST", "/api/v1/collection-templates/:id/items", ADMIN_CFG),
    (
        "DELETE",
        "/api/v1/collection-templates/:id/items/:item_id",
        ADMIN_CFG,
    ),
    ("DELETE", "/api/v1/collection/:item_id", ADMIN_CFG),
    ("GET", "/api/v1/config", DEPLOY_WIDE),
    ("PUT", "/api/v1/config", ADMIN_CFG),
    ("GET", "/api/v1/credentials", ADMIN_CFG),
    ("POST", "/api/v1/credentials", ADMIN_CFG),
    ("DELETE", "/api/v1/credentials/:id", ADMIN_CFG),
    ("PUT", "/api/v1/credentials/:id", ADMIN_CFG),
    (
        "GET",
        "/api/v1/dashboard",
        Global("the caller's own widget layout; the widgets' queries are scoped individually"),
    ),
    (
        "PUT",
        "/api/v1/dashboard",
        Global("the caller's own widget layout"),
    ),
    ("GET", "/api/v1/discovery/candidates", ADMIN_CFG),
    ("POST", "/api/v1/discovery/import", ADMIN_CFG),
    ("POST", "/api/v1/discovery/scan", ADMIN_CFG),
    ("GET", "/api/v1/discovery/scan/:id", ADMIN_CFG),
    ("POST", "/api/v1/dns-monitors", ADMIN_CFG),
    ("GET", "/api/v1/event-rules", ADMIN_CFG),
    ("POST", "/api/v1/event-rules", ADMIN_CFG),
    ("DELETE", "/api/v1/event-rules/:id", ADMIN_CFG),
    ("PUT", "/api/v1/event-rules/:id", ADMIN_CFG),
    ("POST", "/api/v1/event-rules/test", ADMIN_CFG),
    ("GET", "/api/v1/event-sources", ADMIN_CFG),
    ("POST", "/api/v1/event-sources", ADMIN_CFG),
    ("DELETE", "/api/v1/event-sources/:id", ADMIN_CFG),
    ("PUT", "/api/v1/event-sources/:id", ADMIN_CFG),
    ("POST", "/api/v1/event-sources/:id/rotate-token", ADMIN_CFG),
    ("GET", "/api/v1/events", PostFiltered),
    ("GET", "/api/v1/events/stats", GroupFiltered),
    ("GET", "/api/v1/fleet/coverage", GroupFiltered),
    ("GET", "/api/v1/fleet/group-summary", PostFiltered),
    ("GET", "/api/v1/fleet/state-history", PRE_AGGREGATED),
    ("GET", "/api/v1/fleet/summary", PostFiltered),
    ("GET", "/api/v1/flow/conversations", PRE_AGGREGATED),
    ("GET", "/api/v1/flow/protocols", PRE_AGGREGATED),
    ("GET", "/api/v1/flow/series", PRE_AGGREGATED),
    ("GET", "/api/v1/flow/top-as", PRE_AGGREGATED),
    ("GET", "/api/v1/flow/top-ports", PRE_AGGREGATED),
    ("GET", "/api/v1/flow/top-talkers", PRE_AGGREGATED),
    ("GET", "/api/v1/forwarding/destinations", ADMIN_CFG),
    ("POST", "/api/v1/forwarding/destinations", ADMIN_CFG),
    ("DELETE", "/api/v1/forwarding/destinations/:id", ADMIN_CFG),
    ("PUT", "/api/v1/forwarding/destinations/:id", ADMIN_CFG),
    (
        "POST",
        "/api/v1/forwarding/destinations/:id/test",
        ADMIN_CFG,
    ),
    ("GET", "/api/v1/forwarding/status", ADMIN_CFG),
    (
        "POST",
        "/api/v1/ingest/webhook/:source_id",
        Global("token-authenticated ingest from a device, not a user-facing read"),
    ),
    ("GET", "/api/v1/llm/config", ADMIN_CFG),
    ("PUT", "/api/v1/llm/config", ADMIN_CFG),
    ("POST", "/api/v1/llm/test", ADMIN_CFG),
    ("GET", "/api/v1/maintenance-windows", PostFiltered),
    ("POST", "/api/v1/maintenance-windows", NodeScoped),
    ("DELETE", "/api/v1/maintenance-windows/:id", NodeScoped),
    ("PUT", "/api/v1/maintenance-windows/:id", NodeScoped),
    ("POST", "/api/v1/meraki/import", ADMIN_CFG),
    ("GET", "/api/v1/meraki/orgs", ADMIN_CFG),
    ("POST", "/api/v1/meraki/orgs", ADMIN_CFG),
    ("DELETE", "/api/v1/meraki/orgs/:id", ADMIN_CFG),
    ("PUT", "/api/v1/meraki/orgs/:id/cadence", ADMIN_CFG),
    ("PUT", "/api/v1/meraki/orgs/:id/enabled", ADMIN_CFG),
    ("POST", "/api/v1/meraki/orgs/:id/enumerate", ADMIN_CFG),
    ("GET", "/api/v1/meraki/orgs/:id/networks", ADMIN_CFG),
    ("PUT", "/api/v1/meraki/orgs/:id/networks", ADMIN_CFG),
    ("POST", "/api/v1/meraki/orgs/discover", ADMIN_CFG),
    ("GET", "/api/v1/meraki/polling", ADMIN_CFG),
    ("PUT", "/api/v1/meraki/polling", ADMIN_CFG),
    ("GET", "/api/v1/metrics/interface-delta", PostFiltered),
    ("GET", "/api/v1/metrics/interface-heatmap", PostFiltered),
    ("GET", "/api/v1/metrics/interface-top", PostFiltered),
    ("GET", "/api/v1/metrics/throughput-range", PRE_AGGREGATED),
    ("GET", "/api/v1/metrics/top", PostFiltered),
    ("GET", "/api/v1/mib-catalog", ADMIN_CFG),
    ("POST", "/api/v1/mib-catalog", ADMIN_CFG),
    ("DELETE", "/api/v1/mib-catalog/:id", ADMIN_CFG),
    ("GET", "/api/v1/monitoring-gaps", INFRA),
    ("GET", "/api/v1/mutes", PostFiltered),
    ("POST", "/api/v1/mutes", NodeScoped),
    ("DELETE", "/api/v1/mutes/:id", NodeScoped),
    ("GET", "/api/v1/node-groups", GroupFiltered),
    ("POST", "/api/v1/node-groups", ADMIN_CFG),
    ("DELETE", "/api/v1/node-groups/:id", ADMIN_CFG),
    ("PUT", "/api/v1/node-groups/:id", ADMIN_CFG),
    ("PUT", "/api/v1/node-groups/:id/geo", ADMIN_CFG),
    ("PUT", "/api/v1/node-groups/:id/placement", ADMIN_CFG),
    ("PUT", "/api/v1/node-groups/:id/pool", ADMIN_CFG),
    ("POST", "/api/v1/node-names", GroupFiltered),
    ("GET", "/api/v1/nodes", GroupFiltered),
    ("POST", "/api/v1/nodes", ADMIN_CFG),
    ("DELETE", "/api/v1/nodes/:node_id", ADMIN_CFG),
    ("GET", "/api/v1/nodes/:node_id", NodeScoped),
    ("GET", "/api/v1/nodes/:node_id/assignment", NodeScoped),
    ("PUT", "/api/v1/nodes/:node_id/bindings", ADMIN_CFG),
    ("GET", "/api/v1/nodes/:node_id/collection", NodeScoped),
    ("POST", "/api/v1/nodes/:node_id/collection", ADMIN_CFG),
    ("GET", "/api/v1/nodes/:node_id/dns-chain", NodeScoped),
    (
        "GET",
        "/api/v1/nodes/:node_id/dns-chain/history",
        NodeScoped,
    ),
    ("DELETE", "/api/v1/nodes/:node_id/dns-check", ADMIN_CFG),
    ("GET", "/api/v1/nodes/:node_id/dns-check", NodeScoped),
    ("PUT", "/api/v1/nodes/:node_id/dns-check", ADMIN_CFG),
    (
        "GET",
        "/api/v1/nodes/:node_id/flow/conversations",
        NodeScoped,
    ),
    ("GET", "/api/v1/nodes/:node_id/flow/protocols", NodeScoped),
    ("GET", "/api/v1/nodes/:node_id/flow/series", NodeScoped),
    ("GET", "/api/v1/nodes/:node_id/flow/top-as", NodeScoped),
    ("GET", "/api/v1/nodes/:node_id/flow/top-ports", NodeScoped),
    ("GET", "/api/v1/nodes/:node_id/flow/top-talkers", NodeScoped),
    ("PUT", "/api/v1/nodes/:node_id/group", ADMIN_CFG),
    ("GET", "/api/v1/nodes/:node_id/interfaces", NodeScoped),
    (
        "GET",
        "/api/v1/nodes/:node_id/interfaces/:ifindex/series",
        NodeScoped,
    ),
    ("GET", "/api/v1/nodes/:node_id/metrics/:metric", NodeScoped),
    (
        "GET",
        "/api/v1/nodes/:node_id/metrics/:metric/range",
        NodeScoped,
    ),
    ("PUT", "/api/v1/nodes/:node_id/parent", ADMIN_CFG),
    ("PUT", "/api/v1/nodes/:node_id/placement", ADMIN_CFG),
    ("POST", "/api/v1/nodes/:node_id/poll", NodeScoped),
    ("PUT", "/api/v1/nodes/:node_id/pool", ADMIN_CFG),
    ("GET", "/api/v1/nodes/:node_id/status", NodeScoped),
    ("DELETE", "/api/v1/nodes/:node_id/url-check", ADMIN_CFG),
    ("GET", "/api/v1/nodes/:node_id/url-check", NodeScoped),
    ("PUT", "/api/v1/nodes/:node_id/url-check", ADMIN_CFG),
    ("GET", "/api/v1/nodes/by-group", GroupFiltered),
    ("GET", "/api/v1/nodes/search", GroupFiltered),
    ("GET", "/api/v1/notification-channels", ADMIN_CFG),
    ("POST", "/api/v1/notification-channels", ADMIN_CFG),
    ("DELETE", "/api/v1/notification-channels/:id", ADMIN_CFG),
    ("PUT", "/api/v1/notification-channels/:id", ADMIN_CFG),
    (
        "GET",
        "/api/v1/openapi.json",
        Global("the API contract document itself"),
    ),
    ("GET", "/api/v1/poller-health", INFRA),
    ("GET", "/api/v1/pollers", INFRA),
    ("DELETE", "/api/v1/pollers/:id", INFRA),
    ("GET", "/api/v1/pollers/:id/nodes", PostFiltered),
    ("GET", "/api/v1/pools", INFRA),
    ("GET", "/api/v1/profiles", ADMIN_CFG),
    ("POST", "/api/v1/profiles", ADMIN_CFG),
    ("DELETE", "/api/v1/profiles/:id", ADMIN_CFG),
    ("PUT", "/api/v1/profiles/:id", ADMIN_CFG),
    ("GET", "/api/v1/profiles/:id/templates", ADMIN_CFG),
    ("PUT", "/api/v1/profiles/:id/templates", ADMIN_CFG),
    ("POST", "/api/v1/rca", NodeScoped),
    ("GET", "/api/v1/reports/definitions", ADMIN_CFG),
    ("POST", "/api/v1/reports/definitions", ADMIN_CFG),
    ("DELETE", "/api/v1/reports/definitions/:id", ADMIN_CFG),
    ("PUT", "/api/v1/reports/definitions/:id", ADMIN_CFG),
    ("POST", "/api/v1/reports/definitions/:id/run", ADMIN_CFG),
    ("GET", "/api/v1/reports/runs", REPORT),
    ("DELETE", "/api/v1/reports/runs/:id", ADMIN_CFG),
    ("GET", "/api/v1/reports/runs/:id", REPORT),
    ("GET", "/api/v1/reports/runs/:id/export", REPORT),
    ("GET", "/api/v1/reports/schedules", ADMIN_CFG),
    ("POST", "/api/v1/reports/schedules", ADMIN_CFG),
    ("DELETE", "/api/v1/reports/schedules/:id", ADMIN_CFG),
    ("PUT", "/api/v1/reports/schedules/:id", ADMIN_CFG),
    (
        "GET",
        "/api/v1/reports/sections",
        Global("a static catalog of report building blocks"),
    ),
    ("GET", "/api/v1/roles", DEPLOY_WIDE),
    ("GET", "/api/v1/routing-rules", ADMIN_CFG),
    ("POST", "/api/v1/routing-rules", ADMIN_CFG),
    ("DELETE", "/api/v1/routing-rules/:id", ADMIN_CFG),
    ("PUT", "/api/v1/routing-rules/:id", ADMIN_CFG),
    ("GET", "/api/v1/settings/oidc", ADMIN_CFG),
    ("POST", "/api/v1/settings/oidc", ADMIN_CFG),
    ("DELETE", "/api/v1/settings/oidc/:id", ADMIN_CFG),
    ("PUT", "/api/v1/settings/oidc/:id", ADMIN_CFG),
    (
        "GET",
        "/api/v1/shared-dashboard",
        Global("one admin-edited layout shown to everyone; its widgets' queries are scoped"),
    ),
    ("PUT", "/api/v1/shared-dashboard", ADMIN_CFG),
    ("GET", "/api/v1/stream/alerts", PostFiltered),
    ("GET", "/api/v1/stream/analysis", PostFiltered),
    ("GET", "/api/v1/stream/node-states", PostFiltered),
    ("GET", "/api/v1/stream/report-runs", REPORT),
    (
        "GET",
        "/api/v1/system-health",
        Global("core's own health, not monitored-node data"),
    ),
    (
        "GET",
        "/api/v1/system/hosts",
        Global("core and poller host metrics, not monitored nodes"),
    ),
    (
        "GET",
        "/api/v1/system/hosts/:instance/metrics/range",
        Global("core and poller host metrics, not monitored nodes"),
    ),
    ("GET", "/api/v1/thresholds", ADMIN_CFG),
    ("POST", "/api/v1/thresholds", ADMIN_CFG),
    ("DELETE", "/api/v1/thresholds/:id", ADMIN_CFG),
    ("GET", "/api/v1/topology", GroupFiltered),
    ("POST", "/api/v1/url-monitors", ADMIN_CFG),
    ("GET", "/api/v1/users", ADMIN_CFG),
    ("POST", "/api/v1/users", ADMIN_CFG),
    ("DELETE", "/api/v1/users/:id", ADMIN_CFG),
    ("PUT", "/api/v1/users/:id/enabled", ADMIN_CFG),
    ("PUT", "/api/v1/users/:id/password", ADMIN_CFG),
    ("PUT", "/api/v1/users/:id/role", ADMIN_CFG),
    ("GET", "/api/v1/version", DEPLOY_WIDE),
    ("GET", "/healthz", Global("unauthenticated liveness probe")),
    ("GET", "/readyz", Global("unauthenticated readiness probe")),
];

#[cfg(test)]
mod tests {
    use super::{Scoping, ROUTES};
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
        for (method, path, _) in ROUTES {
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
            .filter(|(m, p, _)| !documented.contains(&((*m).to_owned(), (*p).to_owned())))
            .map(|(m, p, _)| (m, p))
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
            .map(|(m, p, _)| ((*m).to_owned(), (*p).to_owned()))
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
            .filter(|(m, p, _)| !seen.insert((*m, *p)))
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
        for (method, path, scoping) in ROUTES {
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
        for (method, path, scoping) in ROUTES {
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
        for (method, path, scoping) in ROUTES {
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
            .filter(|(_, _, s)| matches!(s, Scoping::Pending(_)))
            .count();
        assert_eq!(
            pending, 0,
            "the number of not-yet-scoped routes changed — if you scoped one, lower this; \
             if you added one, scope it instead"
        );
    }
}
