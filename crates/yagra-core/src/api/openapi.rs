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
                        "Either a session token from `POST /api/v1/auth/login` (short-lived; what \
                         the WebUI uses), or a personal access token (`yat_…`, Settings ▸ API \
                         tokens) whose surfaces include `rest`. A token issued for `mcp` only is \
                         refused here with 401. An API token acts as the account that owns it, so \
                         its role **and** its visibility scope are capped at that account's \
                         current ones, and it stops working when the account is disabled or \
                         deleted; it cannot administer users, and endpoints that identify the \
                         signed-in account (`/auth/me`, the personal dashboard) answer 403 to it.",
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

/// Every domain's slice of the document, named for the file it comes from.
///
/// One list, so [`document`] and the test that no two slices name a schema differently read the
/// same set.
fn fragments() -> Vec<(&'static str, utoipa::openapi::OpenApi)> {
    vec![
        ("openapi", Doc::openapi()),
        ("nodes", super::nodes::Doc::openapi()),
        ("duplicates", super::duplicates::Doc::openapi()),
        ("subnet_overlaps", super::subnet_overlaps::Doc::openapi()),
        ("prefix_gaps", super::prefix_gaps::Doc::openapi()),
        ("pools", super::pools::Doc::openapi()),
        ("checks", super::checks::Doc::openapi()),
        ("metrics", super::metrics::Doc::openapi()),
        ("thresholds", super::thresholds::Doc::openapi()),
        ("flow", super::flow::Doc::openapi()),
        ("users", super::users::Doc::openapi()),
        ("alerts", super::alerts::Doc::openapi()),
        ("topology", super::topology::Doc::openapi()),
        ("fleet", super::fleet::Doc::openapi()),
        ("analysis", super::analysis::Doc::openapi()),
        ("maintenance", super::maintenance::Doc::openapi()),
        ("eventlog", super::eventlog::Doc::openapi()),
        ("audit", super::audit::Doc::openapi()),
        ("dashboard", super::dashboard::Doc::openapi()),
        ("public_dashboard", super::public_dashboard::Doc::openapi()),
        ("preferences", super::preferences::Doc::openapi()),
        ("changes", super::changes::Doc::openapi()),
        ("pins", super::pins::Doc::openapi()),
        ("mib", super::mib::Doc::openapi()),
        ("api_tokens", super::api_tokens::Doc::openapi()),
        ("session", super::session::Doc::openapi()),
        ("ldap", super::ldap::Doc::openapi()),
        ("webtls", super::webtls::Doc::openapi()),
        ("bus", super::bus::Doc::openapi()),
        ("upgrade", super::upgrade::Doc::openapi()),
        ("relocation", super::relocation::Doc::openapi()),
        ("oidc", super::oidc::Doc::openapi()),
        ("system", super::system::Doc::openapi()),
        ("support", super::support::Doc::openapi()),
        ("collection", super::collection::Doc::openapi()),
        ("classification", super::classification::Doc::openapi()),
        ("reclassify", super::reclassify::Doc::openapi()),
        ("rediscover", super::rediscover::Doc::openapi()),
        ("config_backup", super::config_backup::Doc::openapi()),
        ("config_remote", super::config_remote::Doc::openapi()),
        ("discovery", super::discovery::Doc::openapi()),
        ("notifications", super::notifications::Doc::openapi()),
        ("rca", super::rca::Doc::openapi()),
        ("forwarding", super::forwarding::Doc::openapi()),
        ("groups", super::groups::Doc::openapi()),
        ("profiles", super::profiles::Doc::openapi()),
        ("credentials", super::credentials::Doc::openapi()),
        ("pollers", super::pollers::Doc::openapi()),
        ("health", super::health::Doc::openapi()),
        ("reports", super::reports::Doc::openapi()),
        ("neighbors", super::neighbors::Doc::openapi()),
        ("retention", super::retention::Doc::openapi()),
        ("config_bundle", super::config_bundle::Doc::openapi()),
        ("meraki", super::meraki::Doc::openapi()),
        ("netbox", super::netbox::Doc::openapi()),
        ("events", super::events::Doc::openapi()),
        ("wireless", super::wireless::Doc::openapi()),
    ]
}

/// The assembled OpenAPI document for `/api/v1`.
///
/// Merged in the same order as [`super::router`], so reading the two side by side stays possible.
///
/// ⚠️ A merge keys schemas by their short name and keeps one: two different types both called
/// `TestResult` leave the document describing only one of them, silently (ADR-210 stage 4).
/// `no_two_domains_give_one_schema_name_two_shapes` is what fails for that.
#[must_use]
pub fn document() -> utoipa::openapi::OpenApi {
    let mut doc = Root::openapi();
    for (_, fragment) in fragments() {
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
    /// Two domains may share a type, but never a schema name for two different shapes: the merge
    /// keeps one of them without a word, so the generated TypeScript describes the wrong type for
    /// every screen reading the other (found when a config-backup `TestResult` replaced the
    /// forwarding one, ADR-210 stage 4).
    #[test]
    fn no_two_domains_give_one_schema_name_two_shapes() {
        let mut seen: std::collections::BTreeMap<String, (&'static str, serde_json::Value)> =
            std::collections::BTreeMap::new();
        let mut clashes = Vec::new();
        let mut inspected = 0usize;
        let mut slices = vec![("root", Root::openapi())];
        slices.extend(fragments());
        for (domain, fragment) in slices {
            let Some(components) = fragment.components else {
                continue;
            };
            for (name, schema) in components.schemas {
                inspected += 1;
                let shape = serde_json::to_value(&schema).expect("a schema serializes");
                match seen.get(&name) {
                    Some((first, earlier)) if *earlier != shape => {
                        clashes.push(format!("{name}: {first} and {domain}"));
                    }
                    Some(_) => {}
                    None => {
                        seen.insert(name, (domain, shape));
                    }
                }
            }
        }
        // A floor on what was looked at, so a change that stopped finding the slices' schemas
        // cannot pass over nothing.
        assert!(
            inspected > 300,
            "only {inspected} schemas inspected - the fragments are no longer read"
        );
        assert!(
            clashes.is_empty(),
            "two different types share an OpenAPI schema name; rename one: {clashes:?}"
        );
    }

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

    /// Strip module paths and whitespace so `Vec<crate::discover::discovery::Candidate>` and
    /// `Json<Vec<Candidate>>` compare equal — what matters is which type, not how it was spelled.
    fn normalize(ty: &str) -> String {
        let t: String = ty.chars().filter(|c| !c.is_whitespace()).collect();
        match t.strip_prefix("Vec<").and_then(|r| r.strip_suffix('>')) {
            Some(inner) => format!("Vec<{}>", inner.rsplit("::").next().unwrap_or(inner)),
            None => t.rsplit("::").next().unwrap_or(&t).to_owned(),
        }
    }

    /// The success `body =` of a `#[utoipa::path]` attribute: the first 2xx response carrying one.
    /// `None` for a documented stream/download (a `content_type` with no body) or a bare 204.
    fn declared_body(attr: &str) -> Option<String> {
        for line in attr.lines() {
            if !line.contains("(status = 2") {
                continue;
            }
            let rest = line.split("body = ").nth(1)?;
            return Some(normalize(
                rest.trim_end().trim_end_matches(',').trim_end_matches(')'),
            ));
        }
        None
    }

    /// The type a handler actually serializes, from its signature. `None` when it returns no body
    /// (`StatusCode`) or erases to a `Response` — those are the cases nothing here can compare.
    fn returned_body(ret: &str) -> Option<String> {
        let mut t: String = ret.chars().filter(|c| !c.is_whitespace()).collect();
        if let Some(inner) = t
            .strip_prefix("ApiResult<")
            .and_then(|r| r.strip_suffix('>'))
        {
            t = inner.to_owned();
        }
        if let Some(inner) = t
            .strip_prefix("(StatusCode,")
            .and_then(|r| r.strip_suffix(')'))
        {
            t = inner.to_owned();
        }
        let json = t.strip_prefix("Json<").and_then(|r| r.strip_suffix('>'))?;
        Some(normalize(json))
    }

    /// **The one link utoipa itself does not check.**
    ///
    /// `#[utoipa::path]` is an attribute; the macro never reads the function it sits on. Naming a
    /// type that does not exist, or one without `ToSchema`, is a compile error — but naming a
    /// *different real* schema type compiles, generates a wrong TypeScript type, and fails only in
    /// front of an operator. That is the same class of silent drift the whole generated-contract
    /// change exists to remove, so it gets a guard rather than a warning in a rules file.
    ///
    /// Deliberately source-text, and deliberately partial: it compares only handlers whose success
    /// body is a plain `Json<T>`, skipping SSE streams, downloads and 204s, where there is nothing
    /// to compare. A handler shape this cannot parse is *skipped*, never failed — a heuristic that
    /// blocks CI on its own blind spots gets disabled, and then guards nothing.
    #[test]
    fn every_documented_body_is_the_type_its_handler_returns() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/api");
        let mut compared = 0usize;
        let mut wrong = Vec::new();

        for entry in std::fs::read_dir(&dir).expect("read src/api") {
            let path = entry.expect("dir entry").path();
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let src = std::fs::read_to_string(&path).expect("read module");
            let file = path
                .file_name()
                .and_then(|f| f.to_str())
                .unwrap_or("?")
                .to_owned();

            for chunk in src.split("#[utoipa::path(").skip(1) {
                let Some((attr, after)) = chunk.split_once("\n)]") else {
                    continue;
                };
                // The signature runs from the fn name to the `{` that opens the body.
                let Some(sig) = after
                    .split_once("async fn ")
                    .and_then(|(_, s)| s.split_once('{'))
                else {
                    continue;
                };
                let name = sig.0.split('(').next().unwrap_or("?").trim().to_owned();
                let Some(ret) = sig.0.rsplit_once("->").map(|(_, r)| r) else {
                    continue;
                };
                let (Some(declared), Some(returned)) = (declared_body(attr), returned_body(ret))
                else {
                    continue;
                };
                compared += 1;
                if declared != returned {
                    wrong.push(format!(
                        "{file}::{name} — documents {declared}, returns {returned}"
                    ));
                }
            }
        }

        // 🚨 The floor counts what was **compared**, not the `#[utoipa::path]` blocks walked. It
        // sat at 100 against a real 167 for long enough that the parser could have stopped seeing
        // a third of the surface and still reported a clean contract — the failure
        // `floor-must-count-what-was-checked` names. Keep it just under the true count: it moves
        // up when handlers are added, and a drop below it means the parser broke, not that the
        // API shrank (documented shapes this cannot parse are skipped, and are ~90 more).
        assert!(
            compared > 160,
            "only compared {compared} handlers — the parser stopped matching"
        );
        assert!(
            wrong.is_empty(),
            "documented body ≠ handler return type:\n  {}",
            wrong.join("\n  ")
        );
    }

    /// **The twelve routes the check above is structurally unable to reach.**
    ///
    /// `flow_endpoints!` generates a per-node and a fleet handler from each of six invocations, and
    /// every one of them returns `ApiResult<Response>` — `run_flow_agg` erases the row type because
    /// one function serves all six aggregations. So `returned_body` yields `None` for all twelve and
    /// they are skipped, by design.
    ///
    /// What that leaves is the failure this test exists for: the row type each pair *documents* is a
    /// macro argument, and the row type it actually *returns* is decided by a separate hand-written
    /// match in `flow_agg_rows`. Two independent lists, nothing comparing them. Pair
    /// `FlowAgg::Series` with `FlowTalker` and the OpenAPI document, the generated `schema.d.ts` and
    /// every `api.ts` call all name the wrong shape — while the server keeps returning the right one
    /// and every existing test stays green.
    ///
    /// Reads `flow.rs` through `module_source` (ADR-091: test items removed, never cut at the first
    /// attribute) rather than this file's own directory, so no needle written here can match the
    /// line it is written on.
    #[test]
    fn every_flow_endpoint_documents_the_row_its_aggregation_produces() {
        let production = crate::module_source::code("src/api", "flow").replace("\r\n", "\n");

        // 1. Each `FlowRows` variant → the row type it carries.
        let rows_enum = production
            .split_once("enum FlowRows {")
            .and_then(|(_, rest)| rest.split_once("\n}"))
            .expect("the FlowRows enum")
            .0;
        let mut carries = std::collections::BTreeMap::new();
        for line in rows_enum.lines() {
            let Some((variant, rest)) = line.trim().split_once("(Vec<") else {
                continue;
            };
            let Some((path, _)) = rest.split_once(">)") else {
                continue;
            };
            carries.insert(
                variant.to_owned(),
                path.rsplit("::").next().unwrap_or(path).to_owned(),
            );
        }
        assert_eq!(
            carries.len(),
            6,
            "parsed {carries:?} out of the FlowRows enum — the parser stopped matching"
        );

        // 2. Each `FlowAgg` variant → the `FlowRows` variant `flow_agg_rows` builds for it. Bounded
        //    to that function's body: `FlowAgg::` also appears in the invocations parsed below.
        let body = production
            .split_once("fn flow_agg_rows(")
            .and_then(|(_, rest)| rest.split_once("\n}\n"))
            .expect("the flow_agg_rows body")
            .0;
        let mut produces = std::collections::BTreeMap::new();
        for chunk in body.split("FlowAgg::").skip(1) {
            let Some((agg, after)) = chunk.split_once(" =>") else {
                continue;
            };
            let Some((_, tail)) = after.split_once("FlowRows::") else {
                continue;
            };
            produces.insert(
                agg.trim().to_owned(),
                tail.chars()
                    .take_while(char::is_ascii_alphanumeric)
                    .collect::<String>(),
            );
        }
        assert_eq!(
            produces.len(),
            6,
            "parsed {produces:?} out of flow_agg_rows — the parser stopped matching"
        );

        // 3. Each invocation: the aggregation it serves, and the row type it puts in the document.
        let mut checked = 0usize;
        for inv in production.split("flow_endpoints!(").skip(1) {
            let args: Vec<&str> = inv
                .split_once("\n);")
                .expect("an unterminated flow_endpoints! invocation")
                .0
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .collect();
            // Positional, and each pick is shape-checked below so a reordered macro fails loudly
            // rather than comparing two arguments that no longer mean what this test assumes.
            assert_eq!(args.len(), 7, "flow_endpoints! was passed {args:?}");
            let agg = args[2]
                .strip_prefix("FlowAgg::")
                .and_then(|a| a.strip_suffix(','))
                .unwrap_or_else(|| panic!("argument 3 is the aggregation; got {:?}", args[2]));
            let documented = args[5]
                .strip_suffix(',')
                .filter(|r| !r.is_empty() && r.chars().all(|c| c.is_ascii_alphanumeric()))
                .unwrap_or_else(|| panic!("argument 6 is the row type; got {:?}", args[5]));

            let variant = produces
                .get(agg)
                .unwrap_or_else(|| panic!("flow_agg_rows has no arm for FlowAgg::{agg}"));
            let returned = carries
                .get(variant)
                .unwrap_or_else(|| panic!("FlowRows has no variant {variant}"));
            assert_eq!(
                documented, returned,
                "both routes for FlowAgg::{agg} document `Vec<{documented}>`, but `flow_agg_rows` \
                 builds FlowRows::{variant}, which carries {returned}. The published document, \
                 `web/src/api/schema.d.ts` and every `api.ts` call would all name the wrong shape, \
                 and nothing else would notice"
            );
            checked += 1;
        }
        assert_eq!(
            checked, 6,
            "only {checked} flow_endpoints! invocation(s) were parsed — the twelve routes this \
             test covers are not all being checked"
        );
    }
}
