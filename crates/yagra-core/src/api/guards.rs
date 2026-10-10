// SPDX-License-Identifier: AGPL-3.0-only
//! Which domains owe an **accepted-write** test, and whether they have one (ADR-115).
//!
//! ## The failure this exists for
//!
//! Measured on 2026-09-01, the status codes this module's tests asserted were 401 ninety-six
//! times, 503 sixty-nine and 403 forty-nine — against forty-five `200`s, two `204`s, **no `201`
//! and no `202`**, while the handlers return twenty-nine and nine of them. Every fixture was
//! skeleton mode (`admin: None`), so the 193 handlers taking [`super::extract::Admin`] answered
//! `503` and their bodies never ran. A suite in that state passes if *everything* is refused,
//! which is the shape `rejection-only-tests-pass-when-everything-rejects` names.
//!
//! ADR-115 closed it once. This module is what stops it reopening: a file that registers a write
//! route must contain at least one test built on [`super::tests_support::live_state`].
//!
//! ## Two things it is careful about
//!
//! 🚨 **It reads raw source, not [`crate::module_source`].** That reader exists to drop test-only
//! items, and the tests it is looking for are *entirely* test-only — pointed at it, every needle
//! below would search an empty string and pass forever (`floor-must-count-what-was-checked`).
//!
//! 🚨 **Every needle is built at runtime.** A literal `live_state(` written here would match this
//! file's own text, so the check would be satisfied by its own source. That is the quiet half of
//! `self-matching-needle-has-two-directions`: the negated form fails loudly and gets noticed, the
//! positive form passes forever and does not.
//!
//! Both detectors read **code only** — every line whose first non-space characters are a comment
//! marker is dropped first. That is not tidiness: the first version of this module described
//! `.route(path, post(h))` in the doc comment above, and the check reported *this file* as an
//! undeclared write domain. The needles are built at runtime so they cannot see themselves, and
//! the prose is removed so it cannot either.
//!
//! And it has an accept side. A detector that has stopped matching answers "nothing is wrong" in
//! exactly the words a healthy surface does, so [`the_detectors_still_recognise_what_they_are_for`]
//! runs both of them against a file whose answer is known.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Every `api/*.rs` that registers at least one write route, and therefore owes a test in which a
/// write is **accepted**.
///
/// A ledger, not a wish list — the same contract as [`super::route_table`]. Adding a write route to
/// a file not named here fails [`every_file_that_registers_a_write_is_declared`], and removing the
/// last write from one that is named fails it too.
const WRITE_DOMAINS: [&str; 45] = [
    "alerts.rs",
    "analysis.rs",
    "api_tokens.rs",
    "bus.rs",
    "checks.rs",
    "classification.rs",
    "collection.rs",
    "config_bundle.rs",
    "credentials.rs",
    "dashboard.rs",
    "discovery.rs",
    "events.rs",
    "forwarding.rs",
    "groups.rs",
    "health.rs",
    "ldap.rs",
    "maintenance.rs",
    "meraki.rs",
    "netbox.rs",
    "mib.rs",
    "neighbors.rs",
    "nodes.rs",
    "notifications.rs",
    "oidc.rs",
    "pins.rs",
    "pollers.rs",
    "pools.rs",
    "preferences.rs",
    "prefix_gaps.rs",
    "profiles.rs",
    "public_dashboard.rs",
    "rca.rs",
    "reclassify.rs",
    "rediscover.rs",
    "relocation.rs",
    "reports.rs",
    "retention.rs",
    "session.rs",
    "subnet_overlaps.rs",
    "thresholds.rs",
    "topology.rs",
    "upgrade.rs",
    "users.rs",
    "webtls.rs",
    "wireless.rs",
];

/// Domains excused from owing an accepted-write test, each with the reason it cannot have one.
///
/// **Empty, and that is the goal state rather than an oversight.** It exists so that the next
/// genuinely-unreachable write is recorded here with an argument, in a diff a reviewer sees,
/// rather than by quietly not writing the test. A reason is required by the type.
const EXEMPT: [(&str, &str); 0] = [];

/// The floor on how many files were read at all. Not a fact about the API — a fact about the
/// reader: if the directory walk breaks, every set below is empty and every assertion holds.
const MIN_FILES_INSPECTED: usize = 45;

/// This crate's `src/api` directory.
fn api_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("api")
}

/// Every `api/*.rs`, as `(file name, raw text)` — **raw**, for the reason in the module doc.
fn files() -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = std::fs::read_dir(api_dir())
        .expect("read src/api")
        .map(|e| e.expect("dir entry").path())
        .filter(|p| p.extension().is_some_and(|x| x == "rs"))
        .map(|p| {
            let name = p
                .file_name()
                .expect("file name")
                .to_string_lossy()
                .into_owned();
            (name, std::fs::read_to_string(&p).expect("read source"))
        })
        .collect();
    out.sort();
    assert!(
        out.len() >= MIN_FILES_INSPECTED,
        "only {} files were read from src/api; the walk is broken and every check below is vacuous",
        out.len()
    );
    out
}

/// Everything but the comment lines.
///
/// Cheaper and blunter than `crate::module_source`, and deliberately so: that reader also drops
/// **test-only items**, which is the entire population the second detector is about.
fn code_only(text: &str) -> String {
    text.lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Split a file at the first test module: `(production, tests)`.
///
/// The needle is assembled so this file's own attribute cannot be the thing it finds.
fn halves(text: &str) -> (&str, &str) {
    let needle = format!("\n#[{}({})]", "cfg", "test");
    match text.find(&needle) {
        Some(i) => (&text[..i], &text[i..]),
        None => (text, ""),
    }
}

/// Does this production text register a route with a mutating verb?
///
/// Both spellings, because a route registers its verbs either way: `.route(path, post(h))` and
/// `.route(path, get(h).post(h))`.
/// 🚨 **Whitespace-insensitive, and that is the whole of it.** `cargo fmt` breaks a long
/// `.route(path, post(h))` over four lines, which left `, post(` matching nothing — so a file whose
/// write routes all wrap read as registering **no write at all**, which is precisely the case this
/// module exists to catch. Found by ADR-064's wireless routes, whose paths are long enough to wrap;
/// every other domain's happened to fit on one line, so the hole had never been stepped in.
fn registers_a_write(production: &str) -> bool {
    let code = code_only(production);
    let flat = code.split_whitespace().collect::<Vec<_>>().join(" ");
    let route = format!(".{}(", "route");
    if !flat.contains(&route) {
        return false;
    }
    ["post", "put", "delete", "patch"]
        .iter()
        .any(|verb| flat.contains(&format!(", {verb}(")) || flat.contains(&format!(".{verb}(")))
}

/// Does this test text build a live-mode state — i.e. is there a write here that was *accepted*?
///
/// ⚠️ **The needle stops at the name, not at the opening bracket.** `tests_support` grew variants
/// of the fixture — `live_state_with_env_community`, `live_state_with_upgrade_dir` — because
/// production reads some of its inputs from the process environment, which a test cannot set for
/// itself alone. Each one builds the same live `ApiState`, so each one is an accepted write; a
/// needle spelled `live_state(` would report a domain that uses one as having no accepted-write
/// test at all, which reads exactly like the failure this module exists to catch.
fn has_an_accepted_write(tests: &str) -> bool {
    code_only(tests).contains(&format!("{}_state", "live"))
}

/// The detector must see a write route however `cargo fmt` laid it out, and must not see a read.
///
/// A recognition test rather than a floor: the healthy answer to "which files register a write" is
/// a list that changes, so the only way to know the reader is alive is to hand it text whose answer
/// is known. The wrapped form is the one that was silently missed.
///
/// ⚠️ **Every sample is assembled at runtime**, for the reason the module doc gives: this file has no
/// `#[cfg(test)]` of its own (it is declared `#[cfg(test)] mod guards;`), so [`halves`] reads all of
/// it as production and a literal write route written here makes the check report *this file*. That
/// happened on the first attempt at this test.
#[test]
fn the_write_detector_sees_a_wrapped_route_and_not_a_read_only_one() {
    let route = format!(".{}(", "route");
    let verb = |v: &str| format!("{v}(h)");
    let wrapped = format!(
        "Router::new()\n    {route}\n        \"/api/v1/x/:id/y\",\n        {},\n    )\n",
        verb("post")
    );
    let inline = format!("Router::new(){route}\"/api/v1/x\", {})\n", verb("put"));
    let chained = format!(
        "Router::new(){route}\"/api/v1/x\", {}.{})\n",
        verb("get"),
        verb("delete")
    );
    let read_only = format!("Router::new(){route}\"/api/v1/x\", {})\n", verb("get"));
    assert!(
        registers_a_write(&wrapped),
        "a wrapped write route was missed"
    );
    assert!(registers_a_write(&inline));
    assert!(registers_a_write(&chained));
    assert!(!registers_a_write(&read_only));
    // A commented-out write is not a write, and neither is prose about one.
    assert!(!registers_a_write(&format!(
        "// {route}\"/api/v1/x\", {})\n{read_only}",
        verb("post")
    )));
}

#[test]
fn every_file_that_registers_a_write_is_declared() {
    let derived: BTreeSet<String> = files()
        .into_iter()
        .filter(|(_, text)| registers_a_write(halves(text).0))
        .map(|(name, _)| name)
        .collect();
    let declared: BTreeSet<String> = WRITE_DOMAINS.iter().map(|s| (*s).to_owned()).collect();
    assert_eq!(
        declared.len(),
        WRITE_DOMAINS.len(),
        "WRITE_DOMAINS holds a duplicate"
    );
    assert!(
        derived.len() >= 30,
        "only {} write domains were detected; the route detector has stopped matching",
        derived.len()
    );

    let undeclared: Vec<_> = derived.difference(&declared).collect();
    assert!(
        undeclared.is_empty(),
        "{undeclared:?} register a write route and are not in WRITE_DOMAINS. Add the line, and \
         give the file an accepted-write test (see `api/nodes.rs` for the shape)"
    );
    let stale: Vec<_> = declared.difference(&derived).collect();
    assert!(
        stale.is_empty(),
        "{stale:?} are in WRITE_DOMAINS but register no write route any more — delete the line"
    );
}

#[test]
fn every_write_domain_has_an_accepted_write_test() {
    let exempt: BTreeSet<&str> = EXEMPT.iter().map(|(f, _)| *f).collect();
    assert!(
        EXEMPT.iter().all(|(_, why)| !why.trim().is_empty()),
        "an exemption without a reason is an omission with a comment"
    );

    let mut checked = 0usize;
    let mut missing = Vec::new();
    for (name, text) in files() {
        if !WRITE_DOMAINS.contains(&name.as_str()) || exempt.contains(name.as_str()) {
            continue;
        }
        checked += 1;
        if !has_an_accepted_write(halves(&text).1) {
            missing.push(name);
        }
    }
    assert_eq!(
        checked,
        WRITE_DOMAINS.len() - EXEMPT.len(),
        "the ledger names {} domains but only {checked} were found on disk",
        WRITE_DOMAINS.len() - EXEMPT.len()
    );
    assert!(
        missing.is_empty(),
        "{missing:?} register a write route but no test ever sees one accepted — every test there \
         is a refusal, which passes just as well when everything is refused. Build the state with \
         `tests_support::live_state` and assert the 2xx and the row (ADR-115)"
    );
}

#[test]
fn the_detectors_still_recognise_what_they_are_for() {
    // Both directions on both detectors. A check whose healthy answer is "found nothing" has to be
    // able to tell that apart from "looked at nothing", and only the accept side can.
    let by_name = |want: &str| {
        files()
            .into_iter()
            .find(|(n, _)| n == want)
            .unwrap_or_else(|| panic!("{want} is missing from src/api"))
            .1
    };

    let nodes = by_name("nodes.rs");
    assert!(
        registers_a_write(halves(&nodes).0),
        "nodes.rs serves POST /api/v1/nodes; the write detector no longer sees it"
    );
    assert!(
        has_an_accepted_write(halves(&nodes).1),
        "nodes.rs has an accepted-write test; the test detector no longer sees it"
    );

    let error = by_name("error.rs");
    assert!(
        !registers_a_write(halves(&error).0),
        "error.rs registers no route at all; the write detector matches anything"
    );
    assert!(
        !has_an_accepted_write(halves(&error).1),
        "error.rs builds no live state; the test detector matches anything"
    );
}

/// The refusal-test probe (`status_of`) is written out once, in `tests_support.rs` (ADR-202).
///
/// Twenty-three domain files each held their own copy, in nine variants that differed only in
/// which verbs carried `{}`. A domain may still keep a one-line `status_of` that picks its body —
/// that choice is a fact about the domain — but the request itself is built in one place. Read
/// raw (the probes are test-only) and as code only, with every needle assembled at runtime.
#[test]
fn the_status_probe_builds_its_request_in_one_place() {
    // `fn status_` covers `status_of`, `status_of_path` and the shared `status_with`.
    let probe = format!("fn {}", "status_");
    let builder = format!("Request::{}", "builder");
    let mut home_builds = false;
    let mut copies = Vec::new();
    for (name, text) in files() {
        let code = code_only(&text);
        let mut rest = code.as_str();
        while let Some(at) = rest.find(&probe) {
            let body = &rest[at..];
            // A probe ends at the first line closing a block at its own indentation: top level in
            // `tests_support.rs`, four spaces inside a domain's `mod tests`.
            let end = ["\n}", "\n    }"]
                .iter()
                .filter_map(|close| body.find(close).map(|e| e + close.len()))
                .min()
                .unwrap_or(body.len());
            if body[..end].contains(&builder) {
                if name == "tests_support.rs" {
                    home_builds = true;
                } else {
                    copies.push(name.clone());
                }
            }
            rest = &body[end..];
        }
    }
    // The accept side, and the reason this check carries no count: the reader must still
    // recognise the one probe that does build a request. A floor on probes found would shrink as
    // copies are removed, which is the work succeeding.
    assert!(
        home_builds,
        "the shared probe in tests_support.rs was not recognised; the reader is broken"
    );
    assert!(
        copies.is_empty(),
        "{copies:?} build the status probe's request by hand — call \
         `tests_support::status_with` (or `status_of`) and pass the body instead (ADR-202)"
    );
}

/// A list endpoint's row count goes through `util::page_limit`, on both surfaces (ADR-202).
///
/// The defaults and maxima differ per endpoint on purpose; the shape did not need to, and twenty
/// hand-written `unwrap_or(N).clamp(1, M)` lines are twenty places a lower bound of `0` or a
/// missing upper bound can slip in. The needle is a `limit` that is clamped on the same line.
#[test]
fn every_list_limit_is_clamped_through_page_limit() {
    let mcp = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("mcp")
        .join("tools");
    let mut sources = files();
    for entry in std::fs::read_dir(&mcp).expect("read src/mcp/tools") {
        let p = entry.expect("dir entry").path();
        if p.extension().is_some_and(|x| x == "rs") {
            let name = format!(
                "mcp/tools/{}",
                p.file_name().expect("name").to_string_lossy()
            );
            sources.push((name, std::fs::read_to_string(&p).expect("read source")));
        }
    }
    let clamp = format!(".{}(1,", "clamp");
    let helper = format!("{}(", "page_limit");
    let mut callers = 0usize;
    let mut hand_written = Vec::new();
    for (name, text) in &sources {
        let production = code_only(halves(text).0);
        callers += production.matches(&helper).count();
        for line in production.lines() {
            if line.contains("limit") && line.contains(&clamp) {
                hand_written.push(format!("{name}: {}", line.trim()));
            }
        }
    }
    assert!(
        callers >= 15,
        "only {callers} calls to the helper were found; the reader no longer sees them"
    );
    assert!(
        hand_written.is_empty(),
        "{hand_written:#?} clamp a limit by hand — use `api::util::page_limit` (ADR-202)"
    );
}

/// Production files outside `api/` and `mcp/` that may still name `crate::api`, each with the
/// reason. Checked both ways: a file that stops needing its entry fails the test until it leaves.
const IMPORTS_API_ALLOWED: &[(&str, &str)] = &[
    (
        "rca/agent.rs",
        "the LLM root-cause agent calls the MCP tools in-process under the caller's scope, so it \
         sits above the API surface rather than under it (ADR-029)",
    ),
    (
        "rca/orchestrator.rs",
        "carries the caller's `api::scope::NodeScope` to the agent beside it, for the same reason",
    ),
];

/// Whether a production file names the API layer: `crate::api` as a path, or `api` inside a
/// grouped `use crate::{…}`.
fn names_the_api_layer(code: &str) -> bool {
    let path = regex::Regex::new(r"\bcrate::api\b").expect("a valid pattern");
    let grouped = regex::Regex::new(r"\bcrate::\{[^}]*\bapi\b").expect("a valid pattern");
    path.is_match(code) || grouped.is_match(code)
}

/// **Nothing below the API layer imports it** (ADR-202 Inc.3).
///
/// A domain module that reaches up into `api/` for a type or a helper is the dependency that makes
/// no part of this crate separable from the rest: the API layer depends on everything, so anything
/// it is depended on by is tied to everything too. Seven production files did that before ADR-202
/// cut them; this keeps the number from growing back one convenient `use` at a time.
///
/// Reads production code through [`crate::module_source::crate_code`] — unlike the checks above,
/// the population here *is* production code, and a test-only `use crate::api::tests_support` is
/// allowed. `main.rs` is the wiring and names everything; it spells the module `api::` anyway.
#[test]
fn no_module_outside_api_imports_api() {
    let files = crate::module_source::crate_code();
    let below: Vec<&(String, String)> = files
        .iter()
        .filter(|(name, _)| {
            !name.starts_with("api/") && !name.starts_with("mcp/") && name != "main.rs"
        })
        .collect();
    assert!(
        below.len() >= 120,
        "only {} files outside api/ and mcp/ were read; the walk no longer sees the crate",
        below.len()
    );
    let offenders: BTreeSet<&str> = below
        .iter()
        .filter(|(_, code)| names_the_api_layer(code))
        .map(|(name, _)| name.as_str())
        .collect();
    let allowed: BTreeSet<&str> = IMPORTS_API_ALLOWED.iter().map(|(f, _)| *f).collect();
    let undeclared: Vec<&&str> = offenders.difference(&allowed).collect();
    assert!(
        undeclared.is_empty(),
        "{undeclared:?} import the API layer. Move the type or helper they need below `api/` (and \
         have `api/` import it from there), or move the file into `api/` if it is part of a route"
    );
    let stale: Vec<&&str> = allowed.difference(&offenders).collect();
    assert!(
        stale.is_empty(),
        "{stale:?} no longer import the API layer — take them out of IMPORTS_API_ALLOWED"
    );
}

/// The detector behind [`no_module_outside_api_imports_api`] still recognises both spellings, and
/// does not mistake a neighbouring name for the module.
#[test]
fn the_api_import_detector_recognises_both_spellings() {
    let module = "api";
    assert!(names_the_api_layer(&format!(
        "use crate::{module}::scope::NodeScope;"
    )));
    assert!(names_the_api_layer(&format!(
        "use crate::{{alerts, {module}}};"
    )));
    assert!(!names_the_api_layer("use crate::apitokens::TokenRepo;"));
    assert!(!names_the_api_layer(
        "use crate::alerts::{api_like, Other};"
    ));
}
