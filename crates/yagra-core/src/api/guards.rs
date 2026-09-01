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
const WRITE_DOMAINS: [&str; 36] = [
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
    "mib.rs",
    "neighbors.rs",
    "nodes.rs",
    "notifications.rs",
    "oidc.rs",
    "pollers.rs",
    "pools.rs",
    "preferences.rs",
    "profiles.rs",
    "rca.rs",
    "reports.rs",
    "retention.rs",
    "session.rs",
    "thresholds.rs",
    "topology.rs",
    "upgrade.rs",
    "users.rs",
    "webtls.rs",
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
fn registers_a_write(production: &str) -> bool {
    let code = code_only(production);
    let route = format!(".{}(", "route");
    if !code.contains(&route) {
        return false;
    }
    ["post", "put", "delete", "patch"]
        .iter()
        .any(|verb| code.contains(&format!(", {verb}(")) || code.contains(&format!(".{verb}(")))
}

/// Does this test text build a live-mode state — i.e. is there a write here that was *accepted*?
fn has_an_accepted_write(tests: &str) -> bool {
    code_only(tests).contains(&format!("{}_state(", "live"))
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
