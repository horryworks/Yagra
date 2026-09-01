// SPDX-License-Identifier: AGPL-3.0-only
//! **What this module must hold true that no type can say** (ADR-102).
//!
//! Everything here reads `reports/` as text, through [`crate::module_source`] and never
//! `include_str!` — the whole directory, with each file's test-only items dropped. That matters
//! twice over: a reader pointed at one file of seven keeps running over a fraction and reports
//! success (ADR-086), and a reader that keeps the test module in view lets an assertion be
//! satisfied by the needle's own source line (ADR-102, thirty-two of those).
//!
//! Because this file is declared `#[cfg(test)] mod guards;`, the reader derives it as test-only and
//! leaves it out — so the literals below are searched for in the production code and nowhere else.

use super::*;
use std::collections::BTreeSet;

/// `report_runs.state` is written by SQL literals, not by a bind, so `as_str` is not on the write
/// path — it is interpolated into the statement. This pins that it still is: if someone re-hardcodes
/// a literal, the enum stops being the single source and the two can drift into a run that is
/// written `succeeded` and read as `Unknown`.
///
/// 🚨 **The last two assertions were dead until ADR-102.** They read `include_str!("reports.rs")`,
/// which included this test, so each needle matched the line it was written on and the assertion
/// could not fail — delete the interpolation they exist to pin and they still passed. The negated
/// needles above them were built at runtime and were fine, which is the whole asymmetry: self-match
/// is loud on a negation and silent on an assertion.
#[test]
fn the_run_state_sql_is_built_from_the_enum() {
    let src = crate::module_source::code_no_comments("src", "reports");
    // Still built at runtime rather than written as literals. The reader drops this file now, so a
    // literal would no longer match itself — but a sibling's test fixture could still spell one, and
    // deriving them from the enum is what makes the check say what it means.
    for state in [
        ReportRunState::Running,
        ReportRunState::Succeeded,
        ReportRunState::Failed,
        ReportRunState::Queued,
    ] {
        let bad = format!("'{}'", state.as_str());
        assert!(
            !src.contains(&bad),
            "{bad} is hardcoded in SQL again; interpolate ReportRunState::…as_str() instead"
        );
    }
    assert!(
        src.contains("ReportRunState::Succeeded.as_str()"),
        "finish_run stopped interpolating the enum"
    );
    assert!(
        src.contains("ReportRunState::Queued.as_str()"),
        "fail_orphans stopped interpolating the enum"
    );
}

/// One const, one nullable bind per clause, in the one order that matches. A positional bind that
/// drifts from its placeholder is silent: the query still runs and answers a different question.
#[test]
fn the_run_filter_binds_every_placeholder_it_names_and_interpolates_none() {
    let placeholders = (1..=3)
        .filter(|n| RUN_FILTER_WHERE.contains(&format!("${n}")))
        .count();
    assert_eq!(placeholders, 3, "{RUN_FILTER_WHERE}");
    assert!(
        !RUN_FILTER_WHERE.contains("{}"),
        "the predicate must not be a format string"
    );
    let src = crate::module_source::code_no_comments("src", "reports");
    let after = src
        .split_once("pub async fn list_runs")
        .expect("list_runs exists")
        .1;
    // Stop at the function's own closing brace: statements after it bind too, and counting those
    // would make this pass for the wrong reason.
    let body = after.split_once("\n    }").map_or(after, |(b, _)| b);
    assert_eq!(
        body.matches(".bind(").count(),
        placeholders + 1,
        "one bind per placeholder, plus the limit"
    );
    // Every clause is always present. A `WHERE` assembled by pushing clauses has a branch per filter
    // that can be forgotten, and a forgotten one fails open.
    for clause in ["definition_id = $1", "state = $2", "created_at >= $3"] {
        assert!(RUN_FILTER_WHERE.contains(clause), "missing {clause}");
    }
}

/// Every file of this module, comment-free, keyed by name.
///
/// **Comment-free is load-bearing.** Every module doc under `reports/` names `.await` while
/// promising not to use it, and the prose below names the tables it forbids — a reader on the raw
/// text would report each of those as a violation. Per-file rather than
/// [`crate::module_source::code`] because a finding here has to name the file it is in.
fn files() -> Vec<(String, String)> {
    crate::module_source::files_no_comments(&crate::module_source::roots("src", "reports"))
}

/// Whether a file is allowed to wait on the outside world — the line this split was cut on.
///
/// Pinned to the directory in both directions below, so an eighth file cannot arrive unclassified.
/// The declaration is one word, and writing it is the moment to ask which half the new file is in.
const PURITY: &[(&str, bool)] = &[
    // The vocabulary two or more of the others need: the constants, the clock, the two formatters.
    ("mod.rs", true),
    // What a report is. Enums, DTOs, and the spec document the WebUI owns.
    ("types.rs", true),
    // What may go in one. A `const`-shaped menu, served as-is.
    ("catalog.rs", true),
    // 🚨 The one that matters. Everything a section's numbers are *turned into* lives here — the
    // arithmetic, the metric selector, the SI formatting, the CSV — and it lives here so a unit test
    // can reach it. Seven of this module's eleven tests are in that file for exactly this reason; an
    // `.await` added here is the first step of moving one of them out of reach.
    ("render.rs", true),
    // The impure half by design: PostgreSQL.
    ("repo.rs", false),
    // Drives a run: writes progress, awaits each section, persists, broadcasts.
    ("runner.rs", false),
    // Each section fetches its own numbers from the stores.
    ("sections.rs", false),
    // The traits the two above reach through, and the live implementations behind them
    // (ADR-112). Impure by definition: every method here forwards to a store.
    ("seams.rs", false),
];

/// **A pure file never `.await`s, and an impure one does.**
///
/// Both directions on purpose. The first is the rule; the second is what stops the check passing
/// vacuously — a reader that returned nothing, or one pointed at the wrong directory, satisfies "no
/// file awaits" perfectly (`rejection-only-tests-pass-when-everything-rejects`). Nine separate
/// source checks in this repository have gone green over nothing (ADR-083, 085, 086, 088, 089, 091,
/// 094, 099, 101), so the floor and the accept side are written first.
///
/// ⚠️ **This is a second implementation of `scheduler/guards.rs`'s rule, and deliberately not
/// shared.** What could be shared is "count the lines containing `.await`", which is three lines;
/// what carries the weight is the table above and the floors below, and both are facts about *this*
/// module. `crate::sql_tables` was lifted to a shared module because it derives a fact — which
/// tables exist — that must not be written down twice. A predicate is not that.
#[test]
fn the_pure_half_never_waits_on_the_outside_world() {
    let declared: BTreeSet<&str> = PURITY.iter().map(|(f, _)| *f).collect();
    let files = files();

    // Both directions against the directory. `files` already omits the test-only modules `mod.rs`
    // declares, so this file is not expected — which is also why its own literals cannot match.
    let present: BTreeSet<&str> = files.iter().map(|(name, _)| name.as_str()).collect();
    for name in &present {
        assert!(
            declared.contains(name),
            "reports/{name} is not in PURITY, so nothing says which half of the split it is in"
        );
    }
    for name in &declared {
        assert!(
            present.contains(name),
            "PURITY names reports/{name}, which no longer exists"
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
                "reports/{name} is declared pure but has {waits} `.await` and {asyncs} `async fn`. \
                 Either the fetch belongs in sections.rs and only its result comes back here, or \
                 this is a piece of a report's arithmetic moving out of reach of a unit test. \
                 Flipping its PURITY row is not the fix."
            );
        } else {
            assert!(
                waits > 0,
                "reports/{name} is declared impure but never waits on anything, so the check above \
                 is passing over a file that could not fail it"
            );
            impure_waits += waits;
        }
    }

    // The floors count what was **inspected**, not what was collected: a reader that returns seven
    // empty files satisfies every assertion above (ADR-091 shipped exactly that mistake).
    //
    // Measured while breaking them: an empty reader is caught by the directory pinning and a
    // truncating one by the accept side, both before these fire. That does not make them
    // decoration — they are what is left when the reader still returns whole files and the module
    // is the thing that shrank — but the order is worth knowing when one of them does go off.
    assert!(
        pure_lines >= 500,
        "only {pure_lines} lines of pure code were scanned (613 at ADR-102); the reader is finding \
         a fraction of the module and the assertions above mean nothing"
    );
    assert!(
        impure_waits >= 35,
        "only {impure_waits} awaits were seen on the impure side (58 at ADR-112, 47 at ADR-102)"
    );
}

/// Which tables each file may name, and why it is the one that names them.
///
/// The shape ADR-094 and ADR-095 use. A hand-written table is tolerable for the reason
/// `retention.rs::PRUNE_SITES` gives about itself: it falls the safe way — forgetting to declare a
/// table fails the build, declaring one nothing uses costs nobody anything.
///
/// 🚨 **Six of the seven declare nothing, and that is the claim.** A report is generated by reading
/// stores through their repositories; a `sqlx::query` in `runner.rs` or `sections.rs` would be the
/// store-separation rule (`coding-conventions.md`) breaking in the place nobody looks, and it would
/// compile, run, and read like the lines around it.
const TABLE_OWNERSHIP: &[(&str, &[&str])] = &[
    ("mod.rs", &[]),
    ("types.rs", &[]),
    ("catalog.rs", &[]),
    ("render.rs", &[]),
    // Definitions, schedules and runs. The only file here with a database at all.
    (
        "repo.rs",
        &["report_definitions", "report_runs", "report_schedules"],
    ),
    ("runner.rs", &[]),
    ("sections.rs", &[]),
    // Forwards to repositories and names no table itself. If a seam implementation ever
    // writes its own SQL, the store it should be forwarding to is the thing that is missing.
    ("seams.rs", &[]),
];

/// **A statement may only name a table its file has declared** — and every file is in the table.
///
/// The two filters live in [`crate::sql_tables`]: a name counts only in SQL position *and* only if
/// it is a real table derived from `migrations/`, and neither is sufficient alone.
///
/// 🚨 **Two floors, because this is a check that reports "nothing wrong" when it sees nothing.** One
/// on the vocabulary and one on the statements actually inspected — and the second counts the
/// statements, not the files, which is the distinction ADR-091's own guard got wrong.
#[test]
fn every_statement_names_a_table_its_file_declares() {
    let vocab = crate::sql_tables::vocabulary();
    assert!(
        vocab.len() >= 55,
        "only {} tables were derived from migrations/; the scan below would be filtering against \
         almost nothing",
        vocab.len()
    );

    let declared: std::collections::BTreeMap<&str, &[&str]> =
        TABLE_OWNERSHIP.iter().copied().collect();
    let files = crate::module_source::files(&crate::module_source::roots("src", "reports"));

    let present: BTreeSet<&str> = files.iter().map(|(name, _)| name.as_str()).collect();
    for name in &present {
        assert!(
            declared.contains_key(name),
            "reports/{name} is not in TABLE_OWNERSHIP, so nothing checks what it reads or writes"
        );
    }
    for (name, _) in TABLE_OWNERSHIP {
        assert!(
            present.contains(name),
            "TABLE_OWNERSHIP names reports/{name}, which no longer exists"
        );
    }

    let mut checked = 0usize;
    let mut wrong: Vec<String> = Vec::new();
    for (name, text) in &files {
        let allowed = declared[name.as_str()];
        for table in crate::sql_tables::references(text, &vocab) {
            checked += 1;
            if !allowed.contains(&table.as_str()) {
                wrong.push(format!(
                    "reports/{name} names `{table}`, which it does not declare. A report reads \
                     through a repository; if this file needs a store it does not have, it is the \
                     seam that is missing, not the query"
                ));
            }
        }
    }
    wrong.sort();
    wrong.dedup();
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    assert!(
        checked >= 18,
        "only {checked} statements were inspected (23 at ADR-102); the scan has stopped matching \
         and would report a module full of misplaced queries as clean"
    );
}

/// **The dispatch delegates: no arm fetches for itself.**
///
/// Adding a section is three sites — a catalog entry, an arm here, a `render_*` in
/// [`super::sections`] — and the arm is the one with nowhere else to grow. `worker::execute` is the
/// measured example: its HTTP arm reached 101 lines and its DNS arm 53, together 47% of the
/// dispatch, and between them they owned nineteen tests while being reachable by no function name
/// (ADR-099). Nothing said they should not, so they did.
///
/// 🚨 **The floor comes first and is the load-bearing half.** A body that came back empty — a
/// renamed method, a changed signature, a reader pointed at the wrong file — satisfies "no arm
/// fetches anything" perfectly, which is `rejection-only-tests-pass-when-everything-rejects` read
/// backwards.
///
/// 🚨 **The handles are derived from the struct, not listed here, and that is a repair** (ADR-112).
/// This check used to name `store`/`nodes`/`alerts`/`history` as literals. ADR-112 folded `history`
/// into `alerts` and renamed `repo` to `runs` — after which one literal matched nothing, one was
/// never in the list, and **both failures are silent** because every assertion here is a negation.
/// A needle that cannot match is `rejection-only-tests-pass-when-everything-rejects` with no
/// symptom at all. Deriving them means the next field is checked the moment it exists.
#[test]
fn the_section_dispatch_delegates_and_fetches_nothing() {
    let code = files()
        .into_iter()
        .find(|(name, _)| name == "runner.rs")
        .map(|(_, code)| code)
        .expect("reports/runner.rs");
    let head = "async fn render_section(";
    let start = code
        .find(head)
        .expect("`render_section` is still the dispatch in reports/runner.rs");
    let body = &code[start..];
    // A method, so its brace closes one level in.
    let end = body
        .find("\n    }")
        .expect("the dispatch's closing brace inside the impl");
    let body = &body[..end];

    // The floor: the arms are really in the text this test read.
    let arms = body.matches("self.render_").count();
    assert!(
        arms >= 6,
        "only {arms} renderers were named in `render_section`; the assertion below would pass over \
         a body this test never actually read"
    );

    // Every handle the runner holds, read out of its own declaration. The struct is in the same
    // text this test already has, so there is no second list to keep in step.
    let decl = code
        .split_once("pub struct ReportRunner {")
        .expect("the runner struct is still declared in reports/runner.rs")
        .1;
    let decl = decl.split_once('}').map_or(decl, |(d, _)| d);
    let handles: Vec<&str> = decl
        .lines()
        .filter_map(|l| l.trim().strip_prefix("pub(super) "))
        .filter_map(|l| l.split_once(':'))
        .map(|(name, _)| name.trim())
        .collect();
    assert!(
        handles.len() >= 4,
        "only {} handles were read off ReportRunner ({handles:?}); the loop below would be \
         searching for nothing, which is exactly how this check broke in ADR-112",
        handles.len()
    );

    // Assembled at runtime so this file's own prose cannot satisfy the search, the same discipline
    // every other source-reading check here uses.
    for seam in handles {
        let reach = format!("{}.{seam}", "self");
        assert!(
            !body.contains(&reach),
            "`render_section` reads `{reach}` itself. Every arm hands off to the `render_*` named \
             for its section kind — that is what keeps the dispatch a dispatch, and what stops the \
             next line having nowhere to go but here"
        );
    }
}
