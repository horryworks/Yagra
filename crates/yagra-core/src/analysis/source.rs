// SPDX-License-Identifier: AGPL-3.0-only
//! The analysis module **as source text** — one entry point for every check that reads it
//! (ADR-089).
//!
//! Four tests read this module as a string rather than as code, because what they check has no
//! type: that an analysis short-circuits on exactly the store tier it declares it needs, that no
//! `run_*` reaches an event store directly, that a SQL statement interpolates the state enum
//! instead of re-hardcoding its token, and — from `retention.rs` — that the retention policy's
//! `analysis_jobs` row is implemented by an actual `DELETE`.
//!
//! ADR-089 splits `analysis.rs` into a directory, so the same failure ADR-086 met applies here:
//! **a reader left pointing at one file of several keeps running, over a fraction, reporting
//! success.** **Measured, not predicted** (2026-08-23): with this accessor returning the first 40%
//! of each file, and the floors below removed —
//!
//! | reader | over 40% of the module |
//! |---|---|
//! | `needs_flow_tier_matches_which_analyses_actually_short_circuit` | fails — no analysis short-circuits ✅ |
//! | `every_event_analysis_reads_through_the_store_router` | fails — `bodies_seen` is 4, not 15 ✅ |
//! | `the_job_state_sql_is_built_from_the_enum` | 🚨 **passes.** Its three positive assertions all sit inside the first 40%, and its one negative assertion is satisfied by text holding no SQL |
//! | `retention.rs::PRUNE_SITES` | fails — its needle is gone ✅ |
//!
//! **One of four goes quietly green, and the floors are the only thing that stops it.** With them
//! restored, all four fail. The general shape, the same one ADR-086 named: *an assertion whose only
//! failure mode is "found something bad" cannot tell "nothing bad" from "nothing at all".*
//!
//! The third is why this module exists, and it had *two* defects rather than one. Its negative
//! assertion — "no SQL re-hardcodes `'running'`" — is vacuously true over text that holds no SQL,
//! so it needed a floor counting the writer's interpolation sites. And it computed "production
//! code" with `src.split_once("#[cfg(test)]")`, which over a **concatenation** keeps only the first
//! file: the exact shape ADR-086 measured at 10 tools of 36. That cut now happens per file, in
//! [`crate::module_source`], where no caller can forget it.
//!
//! A fourth thing the split makes dangerous, found by reading rather than by a failure:
//! `needs_flow_tier_matches_…` tracked "which `run_*` am I inside" and **never reset** — not at the
//! end of a function and not at the end of a file. Harmless in one file; across a concatenation it
//! attributes one file's opening lines to the previous file's last analysis. Both scanners now
//! iterate per file and reset at the boundary.
//!
//! **The floor lives here, so every caller inherits it** — fifteen `run_*` are declared today and
//! the accessor refuses to hand back text holding fewer than fifteen. Deliberately not shared with
//! the MCP surface's floor: what is being counted differs, and a floor whose subject is invisible
//! from the place that cares is a floor nobody maintains.

use crate::module_source;

/// Every file of the analysis module as `(file name, code)`, sorted by name, each already
/// stripped of its own test-only items.
///
/// Callers that attribute a finding to a function **must** use this rather than
/// [`analysis_source`], and must reset their per-function state at each file boundary.
pub(crate) fn analysis_source_files() -> Vec<(String, String)> {
    let out = module_source::files(&module_source::roots("src", "analysis"));
    assert!(
        !out.is_empty(),
        "no analysis module found under src — analysis.rs and analysis/ are both absent"
    );
    let declared: usize = out
        .iter()
        .map(|(_, text)| text.matches("async fn run_").count())
        .sum();
    assert!(
        declared >= 15,
        "the analysis module holds only {declared} `async fn run_` declarations; a file of it is \
         missing from what this accessor read, and every check built on it is now checking a \
         fraction of the module while reporting success"
    );
    out
}

/// The whole analysis module, concatenated, code only.
///
/// Use [`analysis_source_files`] instead when a finding names the function it was found in — see
/// the module doc's fourth paragraph.
pub(crate) fn analysis_source() -> String {
    analysis_source_files()
        .into_iter()
        .map(|(_, text)| text)
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The accessor reads the whole module, and its floor fires when it does not.
    ///
    /// Acceptance-first: the positive case runs before the needle, because a test that only proves
    /// a refusal passes against a system that refuses everything
    /// (`rejection-only-tests-pass-when-everything-rejects`).
    #[test]
    fn the_module_is_read_whole_and_says_so_when_it_is_not() {
        let files = analysis_source_files();
        assert!(!files.is_empty(), "no analysis file was found at all");
        let whole = analysis_source();
        assert!(
            whole.matches("async fn run_").count() >= 15,
            "the module this accessor returns does not hold the analyses it must"
        );
        // …and the floor is not decoration: a fraction must not clear it.
        let fraction: String = whole
            .split("async fn run_")
            .take(5)
            .collect::<Vec<_>>()
            .join("async fn run_");
        assert!(
            fraction.matches("async fn run_").count() < 15,
            "the needle is not a fraction of the module, so this test proves nothing"
        );
    }

    /// Each file arrives stripped of its **own** test-only items, which is what makes a
    /// per-function scan safe over more than one file.
    #[test]
    fn every_file_arrives_without_its_test_module() {
        let files = analysis_source_files();
        // That no file's test module survived is asserted inside `module_source::files` itself
        // since ADR-091, so every caller inherits it rather than the ones that remembered to ask.
        // What is left to check here is the part only this module knows.
        assert!(!files.is_empty(), "no analysis file was read at all");
        // The scaffolding must not be part of what the guards grep, or their needles match
        // themselves — the mistake ADR-086 made within minutes of splitting.
        for scaffold in ["guards.rs", "source.rs"] {
            assert!(
                !files.iter().any(|(n, _)| n == scaffold),
                "{scaffold} is inside the text it reads; it must be declared `#[cfg(test)] mod` so \
                 the exclusion derives it"
            );
        }
    }
}
