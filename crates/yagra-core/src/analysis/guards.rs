// SPDX-License-Identifier: AGPL-3.0-only
//! The check that reads the analysis module **as source text** (ADR-089, ADR-098).
//!
//! One thing this module must hold true has no type, so reading the source is the only technique
//! available. It was inline in `analysis.rs` and moved here when that became a directory, for the
//! reason `mcp/tools/guards.rs` exists: a scan that lives in the text it scans matches its own
//! literals. This file is declared `#[cfg(test)] mod guards;`, which is how [`super::source`]'s
//! exclusion derives it — it is never part of what it reads.
//!
//! 🎯 **There were three, and ADR-098 retired two of them by making the analyses runnable.**
//!
//! - `every_event_analysis_reads_through_the_store_router` watched for a `run_*` reading
//!   `self.events.event_*` directly, which with a log store configured answers from the
//!   alert-linked subset — and for `rule_gap`, which looks for *unmatched* events, from the empty
//!   set. The routing now lives behind the `AnalysisEvents` seam, so an analysis has no
//!   `EventRepo` to reach and the offence cannot be written. **A check deleted because the thing
//!   it forbade became unspellable is a better outcome than a check converted** (ADR-092
//!   decision 1).
//! - `needs_flow_tier_matches_which_analyses_actually_short_circuit` looked for the literal
//!   `return … flow_tier_off()` inside each `run_*` body. It is now
//!   `super::tests::every_analysis_needing_the_flow_tier_says_so_when_there_is_none`, which runs
//!   all fifteen through the real dispatch with no flow store — strictly stronger, because it also
//!   catches an arm wired to the wrong analysis and a short circuit returning the wrong finding.
//!
//! What is left is about SQL, which no seam reaches.

use super::source::analysis_source;
use super::AnalysisJobState;

/// `analysis_jobs.state` is written by statement literals, not by a bind, so without this the enum
/// would be the source only for the *reader* and a writer could drift away from it silently.
///
/// 🚨 **The negative assertion below is vacuously true over text holding no SQL**, which is exactly
/// what this test would have been handed if the split left it reading one file of the module. That
/// is why the interpolation-site floor exists and why it comes first: a check whose only failure
/// mode is "found something bad" cannot tell "nothing bad" from "nothing at all"
/// (`rejection-only-tests-pass-when-everything-rejects`, run backwards).
#[test]
fn the_job_state_sql_is_built_from_the_enum() {
    // Comments stripped — the doc comment on `is_filterable` names `state = 'unknown'` as the
    // thing it refuses, and prose about a literal must not read as the literal. The `#[cfg(test)]`
    // tail is already gone: `module_source` cuts each file at its own, rather than this test
    // cutting the concatenation once and keeping only the first file (ADR-086's measured trap).
    let production = analysis_source()
        .lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");

    // The floor: the writer must actually be in the haystack. Eight sites interpolate the enum
    // into a statement today; six is the floor, so a site may legitimately go while half the
    // module vanishing still fails.
    let sites = production
        .lines()
        .filter(|l| l.contains("AnalysisJobState::") && l.contains(".as_str()"))
        .count();
    assert!(
        sites >= 6,
        "only {sites} statements interpolate AnalysisJobState — the SQL writer is not in the text \
         this test read, so its 'no hardcoded state' assertion below would pass over nothing"
    );

    for state in AnalysisJobState::ALL.iter().copied() {
        let bad = format!("'{}'", state.as_str());
        assert!(
            !production.contains(&bad),
            "{bad} is hardcoded in SQL again; interpolate AnalysisJobState::…as_str() instead"
        );
    }
    assert!(production.contains("AnalysisJobState::Running.as_str()"));
    assert!(production.contains("AnalysisJobState::Done.as_str()"));
    assert!(production.contains("AnalysisJobState::Cancelled.as_str()"));
}
