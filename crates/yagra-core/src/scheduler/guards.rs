// SPDX-License-Identifier: AGPL-3.0-only
//! The checks that read this module **as source text** (ADR-096).
//!
//! `scheduler/` is cut on "what does this file need in order to run", and the half of that rule
//! worth defending mechanically is **the pure half never `.await`s**. That is not tidiness: a store
//! round trip added to [`super::checks`] or [`super::assemble`] runs *once per node per sweep*, so
//! at fleet scale it is the exact cost [`super::MonitorHints`] was written to avoid — and it would
//! read fine, pass review, and be found under load. Nothing else can see it: the type system is
//! happy, and the two files have no database to fail against.
//!
//! ⚠️ ADR-094 and ADR-095 defended their splits with the table-ownership check. **That does not
//! transfer here** — `scheduler/` contains no SQL at all (`sqlx` appears zero times), so the
//! vocabulary in `crate::sql_tables` has nothing to match. This is the replacement rule, not a
//! second copy of that one.
//!
//! Declared `#[cfg(test)] mod guards;` in [`super`], which is how [`crate::module_source`]'s
//! exclusion derives it — a scan that lives in the text it scans matches its own literals.

use std::collections::BTreeSet;

/// Every file of this module, comment-free, keyed by name.
///
/// **Comment-free is load-bearing, and this file proved it before it was written.** Every module
/// doc under `scheduler/` names `.await` while promising never to use it — a first draft that read
/// the raw text found one "violation" in each of three pure files, all of them prose. Per-file
/// rather than [`crate::module_source::code`] because a finding here must name the file it is in.
fn files() -> Vec<(String, String)> {
    crate::module_source::files_no_comments(&crate::module_source::roots("src", "scheduler"))
}

/// Whether a file is allowed to wait on the outside world — the line the split was cut on.
///
/// Pinned to the directory in both directions below, so an eighth file cannot arrive unclassified.
/// The declaration is one word, and writing it is the moment to ask which half the new file is in.
const PURITY: &[(&str, bool)] = &[
    // The vocabulary two or more of the others need. Interval arithmetic and a resolved credential.
    ("mod.rs", true),
    // One check at a time, from the arguments it is handed.
    ("checks.rs", true),
    // 🚨 The one that matters. This decides *what gets polled*; a lookup added per node here is the
    // per-node round trip the sweep's preloaded hint sets exist to remove.
    ("assemble.rs", true),
    // Lock-free counters. Nothing to wait for.
    ("stats.rs", true),
    // The impure half by design: reads PostgreSQL and Redis, publishes to the bus.
    ("dispatch.rs", false),
    // The loop. Waits on the clock, the coordinator and the dispatcher.
    ("sweep.rs", false),
];

/// **A pure file never `.await`s, and an impure one does.**
///
/// Both directions on purpose. The first is the rule; the second is what stops the check passing
/// vacuously — a reader that returned nothing, or one pointed at the wrong directory, satisfies
/// "no file awaits" perfectly (`rejection-only-tests-pass-when-everything-rejects`). ADR-083, 085,
/// 086, 088, 089, 091 and 094 are seven separate occasions on which a source check in this
/// repository went green over nothing, so the floor and the accept side are written first.
#[test]
fn the_pure_half_never_waits_on_the_outside_world() {
    let declared: BTreeSet<&str> = PURITY.iter().map(|(f, _)| *f).collect();
    let files = files();

    // Both directions against the directory. `files` already omits the test-only modules `mod.rs`
    // declares, so this file and `testkit.rs` are not expected — which is also why this file's own
    // literals cannot match.
    let present: BTreeSet<&str> = files.iter().map(|(name, _)| name.as_str()).collect();
    for name in &present {
        assert!(
            declared.contains(name),
            "scheduler/{name} is not in PURITY, so nothing says which half of the split it is in"
        );
    }
    for name in &declared {
        assert!(
            present.contains(name),
            "PURITY names scheduler/{name}, which no longer exists"
        );
    }

    let mut pure_lines = 0usize;
    let mut impure_waits = 0usize;
    for (name, code) in &files {
        let pure = PURITY
            .iter()
            .find(|(f, _)| f == name)
            .map(|(_, p)| *p)
            .expect("checked above");
        let waits = code.matches(".await").count();
        let asyncs = code.matches("async fn").count();
        if pure {
            pure_lines += code.lines().filter(|l| !l.trim().is_empty()).count();
            assert_eq!(
                waits + asyncs,
                0,
                "scheduler/{name} is declared pure but has {waits} `.await` and {asyncs} `async \
                 fn`. Either the work belongs in dispatch.rs, or this is a per-node round trip on \
                 the sweep's hot path — see MonitorHints. Flipping its PURITY row is not the fix."
            );
        } else {
            assert!(
                waits > 0,
                "scheduler/{name} is declared impure but never waits on anything, so the check \
                 above is passing over a file that could not fail it"
            );
            impure_waits += waits;
        }
    }

    // The floors count what was **inspected**, not what was collected: a reader that returns six
    // empty files satisfies every assertion above (ADR-091 shipped exactly that mistake).
    assert!(
        pure_lines >= 600,
        "only {pure_lines} lines of pure code were scanned (760 at ADR-096 — the module's 1,056 \
         non-blank lines less the 296 that are comments, which this reader drops); the reader is \
         finding a fraction of the module and the assertions above mean nothing"
    );
    assert!(
        impure_waits >= 30,
        "only {impure_waits} awaits were seen on the impure side (41 at ADR-096)"
    );
}

/// **Only [`super::assemble`] may name a [`CheckSpec`](yagra_bus::CheckSpec) variant.**
///
/// Constructing a spec *is* deciding what a node gets polled for, and it belongs in the one file
/// that makes that decision. ADR-084 removed sixteen hand-written blocks from `assemble_node_jobs`
/// by folding them into the two `SnmpJobSource` impls; those sixteen wraps are what this counts,
/// and the check is what keeps them from spreading back out — into `dispatch.rs`, which could
/// short-circuit the assembler entirely, or into `checks.rs`, which builds the *inner* check type
/// and should stay unaware of how it is labelled.
///
/// ⚠️ Today every mention is already in the right file, so this is **prevention, not repair**.
/// The vocabulary is derived from the bus, not listed, so a twenty-first variant needs no edit
/// here — the same reason `crate::sql_tables` derives table names from `migrations/`.
#[test]
fn only_the_assembler_names_a_check_kind() {
    let bus = std::fs::read_to_string("../yagra-bus/src/messages.rs")
        .expect("the bus message crate is a sibling of this one");
    let body = bus
        .split_once("pub enum CheckSpec {")
        .expect("CheckSpec is declared in the bus crate")
        .1;
    let body = &body[..body.find("\n}\n").expect("the enum ends")];
    let variants: BTreeSet<&str> = body
        .lines()
        .filter_map(|l| l.strip_prefix("    "))
        .filter(|l| l.starts_with(|c: char| c.is_ascii_uppercase()))
        .filter_map(|l| l.split_once('(').map(|(n, _)| n))
        .collect();
    assert!(
        variants.len() >= 18,
        "only {} CheckSpec variants were derived (20 at ADR-096); the scan below would then be \
         matching a fraction of the vocabulary",
        variants.len()
    );

    let mut seen = 0usize;
    for (name, code) in files() {
        for line in code.lines() {
            for hit in line.split("CheckSpec::").skip(1) {
                let variant: String = hit
                    .chars()
                    .take_while(char::is_ascii_alphanumeric)
                    .collect();
                if !variants.contains(variant.as_str()) {
                    continue;
                }
                seen += 1;
                assert_eq!(
                    name, "assemble.rs",
                    "scheduler/{name} names CheckSpec::{variant}. Building a spec is deciding what \
                     a node is polled for, and assemble.rs is where that decision lives (ADR-084 \
                     folded sixteen of these into two trait impls; this keeps them there)."
                );
            }
        }
    }
    assert!(
        seen >= 14,
        "only {seen} CheckSpec mentions were inspected (16 at ADR-096), so the placement rule was \
         checked against almost nothing"
    );
}
