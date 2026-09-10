// SPDX-License-Identifier: AGPL-3.0-only
//! One route pattern, two spellings — and the conversion between them.
//!
//! A parameterized route is written two ways in this workspace, and both spellings are correct
//! for their own reader:
//!
//! | Spelling | Who writes it | Who reads it |
//! |---|---|---|
//! | `/api/v1/nodes/{node_id}/interfaces` | the OpenAPI document (ADR-035), and therefore
//!   `web/src/dashboard/widgetRoutes.json`, whose test pins every declared route to a `paths` key |
//! | `/api/v1/nodes/:node_id/interfaces` | `axum`'s `.route(...)` registrations, the ledger in
//!   [`super::route_table`], and **`MatchedPath`** |
//!
//! **axum 0.7's `MatchedPath` returns the pattern exactly as it was registered** — its own doc
//! shows `Router::new().route("/users/:id", …)` yielding `"/users/:id"`. So anything comparing a
//! `MatchedPath` against a table built from the OpenAPI document must convert first, and comparing
//! them raw does not fail loudly: it simply never matches.
//!
//! 🚨 **That is not hypothetical — it shipped.** ADR-123's anonymous allow-list is derived from
//! `widgetRoutes.json` (OpenAPI form) and matched against `MatchedPath` (axum form) with no
//! conversion, so every route taking a path parameter was refused for every anonymous visitor
//! whatever the public board carried. The failure is silent in both directions: the operator who
//! composed the board is signed in and sees it work, and the visitor sees an empty widget rather
//! than a 401.
//!
//! ⚠️ **Which direction this converts is a fact about the axum version.** axum 0.8 changed the
//! path-parameter syntax; if this workspace moves to it, this function's direction is a decision
//! to re-make rather than a detail to carry over. The two checks that would fail are deliberate
//! and live apart: `public_access`'s static tests compare against the ledger's spelling, and
//! `api::extract`'s router tests compare against what `MatchedPath` actually hands over.

/// Rewrite an OpenAPI path into the spelling axum registers and `MatchedPath` returns.
///
/// Only whole segments are rewritten: `{node_id}` becomes `:node_id`, while a segment that merely
/// contains a brace is left alone. Idempotent — a path already in router form passes through.
pub(crate) fn from_openapi(openapi_path: &str) -> String {
    openapi_path
        .split('/')
        .map(
            |seg| match seg.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
                Some(name) => format!(":{name}"),
                None => seg.to_owned(),
            },
        )
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::from_openapi;

    #[test]
    fn rewrites_every_brace_segment_and_leaves_the_rest_alone() {
        assert_eq!(from_openapi("/api/v1/nodes"), "/api/v1/nodes");
        assert_eq!(
            from_openapi("/api/v1/nodes/{node_id}/interfaces"),
            "/api/v1/nodes/:node_id/interfaces"
        );
        // Two parameters in one path: the second must be rewritten too. A conversion that stopped
        // at the first would leave `/nodes/:node_id/interfaces/{ifindex}/series` — which matches
        // nothing, exactly like doing nothing at all, but looks half-fixed while reading.
        assert_eq!(
            from_openapi("/api/v1/nodes/{node_id}/interfaces/{ifindex}/series"),
            "/api/v1/nodes/:node_id/interfaces/:ifindex/series"
        );
    }

    #[test]
    fn is_idempotent_on_a_path_that_is_already_in_router_form() {
        // The tables this reads are generated, but nothing stops a hand-written caller from
        // passing the router's own spelling; converting it twice must not corrupt it.
        let axum_form = "/api/v1/nodes/:node_id/interfaces/:ifindex/series";
        assert_eq!(from_openapi(axum_form), axum_form);
    }
}
