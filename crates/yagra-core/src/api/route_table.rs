// SPDX-License-Identifier: AGPL-3.0-only
//! The northbound API's method/path inventory, pinned by a test.
//!
//! `api.rs` was one 13.7k-line file whose `router()` was a single 393-line expression registering
//! every endpoint. Splitting that per domain means moving route registrations between files, and a
//! dropped `.route(...)` line would not fail to compile — the endpoint would simply 404 at runtime,
//! most likely noticed by a user rather than by CI.
//!
//! So the inventory is recorded here and checked two ways:
//!  1. every pair below is actually served by [`super::router`] (nothing was lost in a move), and
//!  2. no pair is listed twice (a domain module was not merged in twice under a different path).
//!
//! **This list is not a wish list — it is a ledger.** Adding an endpoint means adding its line
//! here in the same commit; deleting one means deleting its line, deliberately.

/// Every `(method, path)` the router serves, sorted by path. Axum path params keep their `:name`
/// form; the test substitutes a value that parses for every extractor in use.
pub(crate) const ROUTES: &[(&str, &str)] = &[
    ("GET", "/api/v1/alerts"),
    ("POST", "/api/v1/alerts/ack"),
    ("GET", "/api/v1/alerts/calendar"),
    ("GET", "/api/v1/alerts/history"),
    ("GET", "/api/v1/alerts/top-nodes"),
    ("GET", "/api/v1/alerts/transitions"),
    ("GET", "/api/v1/analysis/jobs"),
    ("POST", "/api/v1/analysis/jobs"),
    ("GET", "/api/v1/analysis/jobs/:id"),
    ("POST", "/api/v1/analysis/jobs/:id/cancel"),
    ("GET", "/api/v1/analysis/jobs/:id/findings"),
    ("GET", "/api/v1/api-tokens"),
    ("POST", "/api/v1/api-tokens"),
    ("DELETE", "/api/v1/api-tokens/:id"),
    ("GET", "/api/v1/audit"),
    ("POST", "/api/v1/auth/login"),
    ("POST", "/api/v1/auth/logout"),
    ("GET", "/api/v1/auth/me"),
    ("GET", "/api/v1/auth/oidc/authorize"),
    ("POST", "/api/v1/auth/oidc/callback"),
    ("GET", "/api/v1/classification-rules"),
    ("POST", "/api/v1/classification-rules"),
    ("DELETE", "/api/v1/classification-rules/:id"),
    ("PUT", "/api/v1/classification-rules/:id"),
    ("GET", "/api/v1/collection-templates"),
    ("POST", "/api/v1/collection-templates"),
    ("DELETE", "/api/v1/collection-templates/:id"),
    ("GET", "/api/v1/collection-templates/:id/items"),
    ("POST", "/api/v1/collection-templates/:id/items"),
    ("DELETE", "/api/v1/collection-templates/:id/items/:item_id"),
    ("DELETE", "/api/v1/collection/:item_id"),
    ("GET", "/api/v1/config"),
    ("PUT", "/api/v1/config"),
    ("GET", "/api/v1/credentials"),
    ("POST", "/api/v1/credentials"),
    ("DELETE", "/api/v1/credentials/:id"),
    ("PUT", "/api/v1/credentials/:id"),
    ("GET", "/api/v1/dashboard"),
    ("PUT", "/api/v1/dashboard"),
    ("GET", "/api/v1/discovery/candidates"),
    ("POST", "/api/v1/discovery/import"),
    ("POST", "/api/v1/discovery/scan"),
    ("GET", "/api/v1/discovery/scan/:id"),
    ("POST", "/api/v1/dns-monitors"),
    ("GET", "/api/v1/event-rules"),
    ("POST", "/api/v1/event-rules"),
    ("DELETE", "/api/v1/event-rules/:id"),
    ("PUT", "/api/v1/event-rules/:id"),
    ("POST", "/api/v1/event-rules/test"),
    ("GET", "/api/v1/event-sources"),
    ("POST", "/api/v1/event-sources"),
    ("DELETE", "/api/v1/event-sources/:id"),
    ("PUT", "/api/v1/event-sources/:id"),
    ("POST", "/api/v1/event-sources/:id/rotate-token"),
    ("GET", "/api/v1/events"),
    ("POST", "/api/v1/events/alerts/close"),
    ("GET", "/api/v1/events/stats"),
    ("GET", "/api/v1/fleet/coverage"),
    ("GET", "/api/v1/fleet/group-summary"),
    ("GET", "/api/v1/fleet/state-history"),
    ("GET", "/api/v1/fleet/summary"),
    ("GET", "/api/v1/flow/conversations"),
    ("GET", "/api/v1/flow/protocols"),
    ("GET", "/api/v1/flow/series"),
    ("GET", "/api/v1/flow/top-as"),
    ("GET", "/api/v1/flow/top-ports"),
    ("GET", "/api/v1/flow/top-talkers"),
    ("GET", "/api/v1/forwarding/destinations"),
    ("POST", "/api/v1/forwarding/destinations"),
    ("DELETE", "/api/v1/forwarding/destinations/:id"),
    ("PUT", "/api/v1/forwarding/destinations/:id"),
    ("POST", "/api/v1/forwarding/destinations/:id/test"),
    ("GET", "/api/v1/forwarding/status"),
    ("POST", "/api/v1/ingest/webhook/:source_id"),
    ("GET", "/api/v1/llm/config"),
    ("PUT", "/api/v1/llm/config"),
    ("POST", "/api/v1/llm/test"),
    ("GET", "/api/v1/maintenance-windows"),
    ("POST", "/api/v1/maintenance-windows"),
    ("DELETE", "/api/v1/maintenance-windows/:id"),
    ("PUT", "/api/v1/maintenance-windows/:id"),
    ("POST", "/api/v1/meraki/import"),
    ("GET", "/api/v1/meraki/orgs"),
    ("POST", "/api/v1/meraki/orgs"),
    ("DELETE", "/api/v1/meraki/orgs/:id"),
    ("PUT", "/api/v1/meraki/orgs/:id/cadence"),
    ("PUT", "/api/v1/meraki/orgs/:id/enabled"),
    ("POST", "/api/v1/meraki/orgs/:id/enumerate"),
    ("GET", "/api/v1/meraki/orgs/:id/networks"),
    ("PUT", "/api/v1/meraki/orgs/:id/networks"),
    ("POST", "/api/v1/meraki/orgs/discover"),
    ("GET", "/api/v1/meraki/polling"),
    ("PUT", "/api/v1/meraki/polling"),
    ("GET", "/api/v1/metrics/interface-delta"),
    ("GET", "/api/v1/metrics/interface-heatmap"),
    ("GET", "/api/v1/metrics/interface-top"),
    ("GET", "/api/v1/metrics/throughput-range"),
    ("GET", "/api/v1/metrics/top"),
    ("GET", "/api/v1/mib-catalog"),
    ("POST", "/api/v1/mib-catalog"),
    ("DELETE", "/api/v1/mib-catalog/:id"),
    ("GET", "/api/v1/monitoring-gaps"),
    ("GET", "/api/v1/mutes"),
    ("POST", "/api/v1/mutes"),
    ("DELETE", "/api/v1/mutes/:id"),
    ("GET", "/api/v1/node-groups"),
    ("POST", "/api/v1/node-groups"),
    ("DELETE", "/api/v1/node-groups/:id"),
    ("PUT", "/api/v1/node-groups/:id"),
    ("PUT", "/api/v1/node-groups/:id/geo"),
    ("PUT", "/api/v1/node-groups/:id/placement"),
    ("PUT", "/api/v1/node-groups/:id/pool"),
    ("POST", "/api/v1/node-names"),
    ("GET", "/api/v1/nodes"),
    ("POST", "/api/v1/nodes"),
    ("DELETE", "/api/v1/nodes/:node_id"),
    ("GET", "/api/v1/nodes/:node_id"),
    ("GET", "/api/v1/nodes/:node_id/assignment"),
    ("PUT", "/api/v1/nodes/:node_id/bindings"),
    ("GET", "/api/v1/nodes/:node_id/collection"),
    ("POST", "/api/v1/nodes/:node_id/collection"),
    ("GET", "/api/v1/nodes/:node_id/dns-chain"),
    ("GET", "/api/v1/nodes/:node_id/dns-chain/history"),
    ("DELETE", "/api/v1/nodes/:node_id/dns-check"),
    ("GET", "/api/v1/nodes/:node_id/dns-check"),
    ("PUT", "/api/v1/nodes/:node_id/dns-check"),
    ("GET", "/api/v1/nodes/:node_id/flow/conversations"),
    ("GET", "/api/v1/nodes/:node_id/flow/protocols"),
    ("GET", "/api/v1/nodes/:node_id/flow/series"),
    ("GET", "/api/v1/nodes/:node_id/flow/top-as"),
    ("GET", "/api/v1/nodes/:node_id/flow/top-ports"),
    ("GET", "/api/v1/nodes/:node_id/flow/top-talkers"),
    ("PUT", "/api/v1/nodes/:node_id/group"),
    ("GET", "/api/v1/nodes/:node_id/interfaces"),
    ("GET", "/api/v1/nodes/:node_id/interfaces/:ifindex/series"),
    ("GET", "/api/v1/nodes/:node_id/metrics/:metric"),
    ("GET", "/api/v1/nodes/:node_id/metrics/:metric/range"),
    ("PUT", "/api/v1/nodes/:node_id/parent"),
    ("PUT", "/api/v1/nodes/:node_id/placement"),
    ("POST", "/api/v1/nodes/:node_id/poll"),
    ("PUT", "/api/v1/nodes/:node_id/pool"),
    ("GET", "/api/v1/nodes/:node_id/status"),
    ("DELETE", "/api/v1/nodes/:node_id/url-check"),
    ("GET", "/api/v1/nodes/:node_id/url-check"),
    ("PUT", "/api/v1/nodes/:node_id/url-check"),
    ("GET", "/api/v1/nodes/by-group"),
    ("GET", "/api/v1/nodes/search"),
    ("GET", "/api/v1/notification-channels"),
    ("POST", "/api/v1/notification-channels"),
    ("DELETE", "/api/v1/notification-channels/:id"),
    ("PUT", "/api/v1/notification-channels/:id"),
    ("GET", "/api/v1/poller-health"),
    ("GET", "/api/v1/pollers"),
    ("DELETE", "/api/v1/pollers/:id"),
    ("GET", "/api/v1/pollers/:id/nodes"),
    ("GET", "/api/v1/pools"),
    ("GET", "/api/v1/profiles"),
    ("POST", "/api/v1/profiles"),
    ("DELETE", "/api/v1/profiles/:id"),
    ("PUT", "/api/v1/profiles/:id"),
    ("GET", "/api/v1/profiles/:id/templates"),
    ("PUT", "/api/v1/profiles/:id/templates"),
    ("POST", "/api/v1/rca"),
    ("GET", "/api/v1/rca/:id"),
    ("GET", "/api/v1/reports/definitions"),
    ("POST", "/api/v1/reports/definitions"),
    ("DELETE", "/api/v1/reports/definitions/:id"),
    ("GET", "/api/v1/reports/definitions/:id"),
    ("PUT", "/api/v1/reports/definitions/:id"),
    ("POST", "/api/v1/reports/definitions/:id/run"),
    ("GET", "/api/v1/reports/runs"),
    ("DELETE", "/api/v1/reports/runs/:id"),
    ("GET", "/api/v1/reports/runs/:id"),
    ("GET", "/api/v1/reports/runs/:id/export"),
    ("GET", "/api/v1/reports/schedules"),
    ("POST", "/api/v1/reports/schedules"),
    ("DELETE", "/api/v1/reports/schedules/:id"),
    ("PUT", "/api/v1/reports/schedules/:id"),
    ("GET", "/api/v1/reports/sections"),
    ("GET", "/api/v1/roles"),
    ("GET", "/api/v1/routing-rules"),
    ("POST", "/api/v1/routing-rules"),
    ("DELETE", "/api/v1/routing-rules/:id"),
    ("PUT", "/api/v1/routing-rules/:id"),
    ("GET", "/api/v1/settings/oidc"),
    ("POST", "/api/v1/settings/oidc"),
    ("DELETE", "/api/v1/settings/oidc/:id"),
    ("PUT", "/api/v1/settings/oidc/:id"),
    ("GET", "/api/v1/shared-dashboard"),
    ("PUT", "/api/v1/shared-dashboard"),
    ("GET", "/api/v1/stream/alerts"),
    ("GET", "/api/v1/stream/analysis"),
    ("GET", "/api/v1/stream/node-states"),
    ("GET", "/api/v1/stream/report-runs"),
    ("GET", "/api/v1/system-health"),
    ("GET", "/api/v1/system/hosts"),
    ("GET", "/api/v1/system/hosts/:instance/metrics/range"),
    ("GET", "/api/v1/thresholds"),
    ("POST", "/api/v1/thresholds"),
    ("DELETE", "/api/v1/thresholds/:id"),
    ("GET", "/api/v1/topology"),
    ("POST", "/api/v1/url-monitors"),
    ("GET", "/api/v1/users"),
    ("POST", "/api/v1/users"),
    ("DELETE", "/api/v1/users/:id"),
    ("PUT", "/api/v1/users/:id/enabled"),
    ("PUT", "/api/v1/users/:id/password"),
    ("PUT", "/api/v1/users/:id/role"),
    ("GET", "/api/v1/version"),
    ("GET", "/healthz"),
    ("GET", "/readyz"),
];

#[cfg(test)]
mod tests {
    use super::ROUTES;
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
        for (method, path) in ROUTES {
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

    #[test]
    fn the_ledger_has_no_duplicates() {
        let mut seen = std::collections::BTreeSet::new();
        let dupes: Vec<_> = ROUTES.iter().filter(|r| !seen.insert(**r)).collect();
        assert!(dupes.is_empty(), "duplicate route entries: {dupes:?}");
    }
}
