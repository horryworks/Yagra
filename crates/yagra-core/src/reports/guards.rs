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
