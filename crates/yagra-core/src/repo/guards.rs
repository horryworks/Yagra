// SPDX-License-Identifier: AGPL-3.0-only
//! The checks that read this module **as source text** (ADR-094).
//!
//! Things `repo/` must hold true that have no type. Each is answered by reading the module's own
//! source, which is the only technique available: the alternative needs a live PostgreSQL, which
//! is exactly what these files are the adapter for.
//!
//! Declared `#[cfg(test)] mod guards;` in [`super`], which is how [`crate::module_source`]'s
//! exclusion derives it — a scan that lives in the text it scans matches its own literals. ADR-086
//! hit that within minutes of splitting `mcp/tools.rs`, so the mechanism has been in place since.

/// The search cap is one number, and no implementation may re-clamp to its own.
///
/// A source-reading test rather than a behavioural one because the PostgreSQL path needs a
/// live database — and that is exactly where the regression lived: the API edge clamped to
/// 500 and documented that as the maximum, while `NodeRepo::search` re-clamped to 100, so
/// filtering a large fleet silently returned 100 rows. The needle is built at runtime; a
/// literal written out in this file would match itself and fail forever (testing.md).
///
/// Reads through [`crate::module_source`] rather than `include_str!`, so it keeps seeing both
/// implementations now that this module is a directory (ADR-094). `include_str!` needs a
/// literal path and there is no literal that means "every file of this module"; a reader left
/// on one file of several would find one `clamp` instead of two. That the count is asserted
/// as an exact `2` is also this test's floor: a reader that came back empty fails here rather
/// than reporting that nothing re-clamps.
#[test]
fn the_search_cap_is_declared_once() {
    let src = crate::module_source::code_no_comments("src", "repo");
    let src = src.as_str();
    // Both needles are assembled at runtime. A literal spelled out here would appear in this
    // file and match itself — the stale one would fail forever, the good one would over-count.
    let stale = format!("clamp(1, {})", 100);
    assert!(
        !src.contains(&stale),
        "a search path re-clamps to its own literal instead of the shared constant"
    );
    let shared = format!("clamp(1, {})", "NODE_SCAN_MAX");
    // Both implementations clamp, and both clamp against the constant.
    assert_eq!(src.matches(&shared).count(), 2);
}
