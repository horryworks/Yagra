// SPDX-License-Identifier: AGPL-3.0-only
//! The checks that read the analysis module **as source text** (ADR-089).
//!
//! Three things this module must hold true have no type, so reading the source is the only
//! technique available; each test below says what it would take to check it any other way. They
//! were inline in `analysis.rs` and moved here when it became a directory, for the reason
//! `mcp/tools/guards.rs` exists: a scan that lives in the text it scans matches its own literals.
//! This file is declared `#[cfg(test)] mod guards;`, which is how
//! [`super::source`]'s exclusion derives it — it is never part of what it reads.
//!
//! Every scan iterates [`super::source::analysis_source_files`] and **resets its per-function state
//! at each file boundary**. That is not tidiness: before the split there was one file and no
//! boundary, so a scanner that never reset was correct by accident. Concatenated, it attributes one
//! file's opening lines to the previous file's last `run_*`.

use super::source::{analysis_source, analysis_source_files};
use super::{AnalysisJobState, AnalysisTool};

/// A trimmed line with its visibility prefix removed, whatever it is.
///
/// 🚨 Not cosmetic. The scans below matched `async fn run_` against the trimmed line, which was
/// right while every analysis was private in one file. The split made them `pub(super)`, and both
/// scans instantly saw **one** `run_*` instead of fifteen — caught by their floors, which is the
/// whole reason those floors exist. Neither scan would have found a single offender, and without
/// the floors both would have said so as "nothing wrong".
fn without_visibility(t: &str) -> &str {
    t.strip_prefix("pub(crate) ")
        .or_else(|| t.strip_prefix("pub(super) "))
        .or_else(|| t.strip_prefix("pub "))
        .unwrap_or(t)
}

/// The name in `… async fn run_<name>(`, if the line opens an analysis.
fn opens_an_analysis(t: &str) -> Option<String> {
    let rest = without_visibility(t).strip_prefix("async fn run_")?;
    rest.split('(').next().map(str::to_owned)
}

/// Whether a trimmed line opens a function, under any visibility spelling.
fn is_fn_definition(t: &str) -> bool {
    let rest = without_visibility(t);
    let rest = rest.strip_prefix("async ").unwrap_or(rest);
    rest.starts_with("fn ")
}

#[test]
fn needs_flow_tier_matches_which_analyses_actually_short_circuit() {
    // The two must agree or a scheduled analysis is refused for a tier it does not need, or —
    // worse — accepted and left to stack up an empty successful run every day. Read from the
    // source rather than restated, so adding a `flow_tier_off()` arm without updating
    // `needs_flow_tier` fails here.
    //
    // The needle is built at runtime: a literal `flow_tier_off()` written in this test would
    // match itself if this file were ever read back as part of the module.
    let needle = format!("{}{}", "flow_tier", "_off()");
    let mut short_circuits = std::collections::BTreeSet::new();
    for (_file, src) in analysis_source_files() {
        // Reset per file: `current_fn` used to survive to the end of the source, which was
        // harmless in one file and wrong the moment there was more than one.
        let mut current_fn: Option<String> = None;
        for line in src.lines() {
            let t = line.trim();
            if let Some(name) = opens_an_analysis(t) {
                current_fn = Some(name);
            } else if is_fn_definition(t) {
                current_fn = None;
            }
            if line.contains(&needle) && line.contains("return") {
                if let Some(f) = &current_fn {
                    short_circuits.insert(f.clone());
                }
            }
        }
    }
    assert!(
        short_circuits.len() >= 5,
        "the source scan stopped matching: {short_circuits:?}"
    );
    for tool in AnalysisTool::ALL.iter().copied() {
        // `run_<token>` is the naming convention every analysis follows.
        let name = tool.as_str().to_owned();
        let short_circuits_here = short_circuits.contains(&name);
        assert_eq!(
            tool.needs_flow_tier(),
            short_circuits_here,
            "{name}: needs_flow_tier() = {} but the runner {} short-circuit on the flow tier",
            tool.needs_flow_tier(),
            if short_circuits_here {
                "does"
            } else {
                "does not"
            }
        );
    }
}

/// PostgreSQL holds only alert-linked rows (ADR-024), so `self.events.event_*` answers about a
/// subset — and `rule_gap`, which looks for *unmatched* events, about the empty set. Only the
/// four `agg_*` routers may touch a store; every analysis goes through them.
///
/// Same shape as `needs_flow_tier_matches_…` above, including the runtime-built needles.
#[test]
fn every_event_analysis_reads_through_the_store_router() {
    // The aggregates that have a log-store twin. `event_flap_stats` is deliberately absent:
    // every action it counts is alert-linked, so PostgreSQL is complete for it (pinned by
    // `events::tests::event_flap_only_counts_rows_postgresql_keeps`).
    let routed = [
        "event_counts_by_bucket",
        "event_severity_counts",
        "event_unmatched_signatures",
        "event_auth_sources",
    ];
    let direct: Vec<String> = routed
        .iter()
        .map(|m| format!("{}{}", "self.events.", m))
        .collect();

    let mut offenders: Vec<String> = Vec::new();
    let mut bodies_seen = 0usize;
    for (file, src) in analysis_source_files() {
        let mut current_fn: Option<String> = None;
        for line in src.lines() {
            let t = line.trim();
            if let Some(name) = opens_an_analysis(t) {
                current_fn = Some(format!("run_{name}"));
                bodies_seen += 1;
            } else if is_fn_definition(t) {
                // Left the `run_*` body. Every visibility spelling counts as a boundary, not just
                // a bare `async fn` — `pub(crate) async fn incident_signals` sits between two
                // `run_*` bodies, and missing it would attribute its lines to the previous one.
                current_fn = None;
            }
            if let Some(f) = &current_fn {
                if direct.iter().any(|n| line.contains(n.as_str())) {
                    offenders.push(format!("{file}::{f}: {}", t.trim()));
                }
            }
        }
    }
    assert!(
        bodies_seen >= 15,
        "the source scan stopped matching `run_*` bodies (saw {bodies_seen})"
    );
    assert!(
        offenders.is_empty(),
        "these analyses read PostgreSQL directly and will answer from the alert-linked subset \
         when a log store is configured — route them through the `agg_*` helpers: {offenders:#?}"
    );
}

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
