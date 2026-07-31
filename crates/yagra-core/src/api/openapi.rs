// SPDX-License-Identifier: AGPL-3.0-only
//! The northbound API's OpenAPI document — the **source of truth** for the WebUI's types and
//! client (ADR-035).
//!
//! Before this, the Rust API shapes were transcribed by hand into `web/src/types/api.ts` (1,764
//! lines) and `web/src/services/api.ts` (1,576 lines). Nothing checked the transcription: adding a
//! field, renaming one, changing a status code or moving a path all compiled and deployed fine, and
//! the mismatch surfaced as an empty table or an `undefined` in front of an operator. `.claude/rules/
//! extensibility.md` §2 listed that row as the one mirror with **no guard at all**.
//!
//! Now the document is generated from the handlers and their DTOs, committed as
//! `web/src/api/openapi.json`, and `web/src/api/schema.d.ts` is generated from *that*. A field only
//! exists in TypeScript because it exists in Rust.
//!
//! ## How a document fragment gets here
//!
//! Each domain module declares its own fragment next to its handlers:
//!
//! ```ignore
//! #[derive(utoipa::OpenApi)]
//! #[openapi(paths(list_widgets, create_widget), components(schemas(Widget, WidgetBody)))]
//! pub(super) struct Doc;
//! ```
//!
//! and [`document`] merges them in the same order [`super::router`] merges the routers.
//!
//! ## Why the fragments are listed rather than discovered
//!
//! `utoipa-axum` exists precisely to remove this list — its `OpenApiRouter` pairs `.route()` with
//! the `#[utoipa::path]` attribute so a handler is registered once. It is built against **axum
//! 0.8** and pulls a second axum into the tree, and upgrading the web framework is a decision of
//! its own. So the pairing is enforced by a test instead:
//! [`super::route_table`] already holds the method/path ledger and asserts the router serves every
//! line; it now also asserts the *document* describes every line. A handler that loses its
//! `#[utoipa::path]`, or a fragment that is never merged, fails there — the same way a dropped
//! `.route()` does.

use axum::{routing::get, Json, Router};
use utoipa::openapi::security::{HttpAuthScheme, HttpBuilder, SecurityScheme};
use utoipa::{Modify, OpenApi};

/// Adds the bearer-token security scheme, which cannot be expressed by a derive.
struct SecurityAddon;

impl Modify for SecurityAddon {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        // `components` is always present by the time a modifier runs, but building one rather than
        // unwrapping keeps this total.
        let components = openapi.components.get_or_insert_with(Default::default);
        components.add_security_scheme(
            "bearer",
            SecurityScheme::Http(
                HttpBuilder::new()
                    .scheme(HttpAuthScheme::Bearer)
                    .description(Some(
                        "A session token from `POST /api/v1/auth/login`, or a personal access \
                         token (`yat_…`) from Settings ▸ API tokens.",
                    ))
                    .build(),
            ),
        );
    }
}

#[derive(OpenApi)]
#[openapi(
    info(
        title = "Yagra northbound API",
        description = "The REST surface of Yagra-core: inventory, metrics, alerts, passive events \
                       and configuration.\n\nThis document is generated from the Rust handlers — it \
                       is the contract, not a description of one.",
        license(name = "AGPL-3.0-only", identifier = "AGPL-3.0-only"),
    ),
    modifiers(&SecurityAddon),
    security(("bearer" = [])),
)]
struct Root;

/// The assembled OpenAPI document for `/api/v1`.
///
/// Merged in the same order as [`super::router`], so reading the two side by side stays possible.
#[must_use]
pub fn document() -> utoipa::openapi::OpenApi {
    let mut doc = Root::openapi();
    for fragment in [
        Doc::openapi(),
        super::nodes::Doc::openapi(),
        super::checks::Doc::openapi(),
        super::metrics::Doc::openapi(),
        super::thresholds::Doc::openapi(),
        super::flow::Doc::openapi(),
        super::users::Doc::openapi(),
        super::alerts::Doc::openapi(),
        super::topology::Doc::openapi(),
        super::fleet::Doc::openapi(),
        super::analysis::Doc::openapi(),
        super::maintenance::Doc::openapi(),
        super::eventlog::Doc::openapi(),
        super::audit::Doc::openapi(),
        super::dashboard::Doc::openapi(),
        super::mib::Doc::openapi(),
        super::api_tokens::Doc::openapi(),
        super::session::Doc::openapi(),
        super::oidc::Doc::openapi(),
        super::system::Doc::openapi(),
        super::collection::Doc::openapi(),
        super::classification::Doc::openapi(),
        super::discovery::Doc::openapi(),
        super::notifications::Doc::openapi(),
        super::rca::Doc::openapi(),
        super::forwarding::Doc::openapi(),
        super::groups::Doc::openapi(),
        super::profiles::Doc::openapi(),
        super::credentials::Doc::openapi(),
        super::pollers::Doc::openapi(),
        super::health::Doc::openapi(),
        super::reports::Doc::openapi(),
        super::meraki::Doc::openapi(),
        super::events::Doc::openapi(),
    ] {
        doc.merge(fragment);
    }
    doc
}

/// Serve the document at `/api/v1/openapi.json`.
///
/// Deliberately unauthenticated, like `/api/v1/version` and `/api/v1/config`: it describes the
/// product's API surface, which is identical on every deployment and already derivable from the
/// unauthenticated JS bundle. It discloses no inventory, no configuration and no state — so gating
/// it would cost API clients their schema without denying an attacker anything.
pub(super) fn routes() -> Router<super::ApiState> {
    Router::new().route("/api/v1/openapi.json", get(serve_document))
}

#[utoipa::path(
    get, path = "/api/v1/openapi.json", tag = "meta",
    responses((status = 200, description = "This document", content_type = "application/json")),
    security(()),
)]
async fn serve_document() -> Json<utoipa::openapi::OpenApi> {
    Json(document())
}

/// The endpoints belonging to no domain: this document itself — self-describing on purpose, since
/// an API surface that omits the endpoint carrying its own description is one the route ledger
/// would need an exception for — and the two orchestrator probes, which live in [`super`] because
/// they sit outside `/api/v1` entirely.
#[derive(OpenApi)]
#[openapi(paths(serve_document, super::healthz, super::readyz))]
struct Doc;

/// The document as pretty-printed JSON with a trailing newline — the exact bytes of
/// `web/src/api/openapi.json`.
///
/// One function so the staleness test and the file writer cannot disagree about formatting; a
/// difference in trailing whitespace would otherwise read as a contract change on every run.
///
/// Test-only: the running server serves the document as a value ([`serve_document`]), and the exact
/// committed bytes matter to nothing else.
#[cfg(test)]
#[must_use]
fn document_json() -> String {
    let mut s = document()
        .to_pretty_json()
        .expect("the OpenAPI document is serializable");
    s.push('\n');
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The committed `web/src/api/openapi.json` is what `openapi-typescript` reads, so a DTO change
    /// that is not reflected there reaches the WebUI as stale types — exactly the drift this whole
    /// mechanism exists to remove, just moved one file along.
    ///
    /// Set `UPDATE_OPENAPI=1` to rewrite the file instead of failing.
    #[test]
    fn the_committed_document_is_current() {
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web/src/api/openapi.json");
        let generated = document_json();

        if std::env::var_os("UPDATE_OPENAPI").is_some() {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir).expect("create web/src/api");
            }
            std::fs::write(&path, &generated).expect("write openapi.json");
            return;
        }

        let committed = std::fs::read_to_string(&path).unwrap_or_default();
        assert_eq!(
            committed, generated,
            "web/src/api/openapi.json is stale. Regenerate it with:\n    \
             UPDATE_OPENAPI=1 cargo test -p yagra-core the_committed_document_is_current\n\
             then `cd web && npm run generate:api` to refresh schema.d.ts."
        );
    }
}
