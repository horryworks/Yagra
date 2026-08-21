// SPDX-License-Identifier: AGPL-3.0-only
//! The MCP tool surface **as source text** — one entry point for every check that reads it
//! (ADR-086).
//!
//! Twelve tests across four files read the tool surface as a string rather than as code, because
//! the things they check have no type: that a `#[tool]` names itself, that a refusal says the
//! permission the description says, that a parameter struct still carries a filter dimension. That
//! is a legitimate technique here and each of those tests explains why it is the only one available.
//!
//! What it is not legitimate to do is name the file twelve times. ADR-086 splits `tools.rs` into a
//! directory, and a reader left pointing at one file of several would go on running — over a
//! fraction of the surface, reporting success. Six of the twelve would fail loudly (they search for
//! a needle and assert it was found); four have a count floor; **two have no floor at all, and one
//! cannot be given a useful one** — `every_tool_wrapper_declares_its_own_name` compares its own
//! count against `declared_mcp_tools().len()`, and both sides are derived from this same text, so a
//! lost file shrinks both and they still agree. That is the third time this repository has met a
//! check that goes quietly green (ADR-083's `.split()`, ADR-085's needle); the new face is *both
//! sides of a comparison sharing a source*.
//!
//! **Measured, not predicted** (2026-08-22). Reading only the first ten tools' worth of surface,
//! with the ADR-086 floors removed, leaves **16** tests failing and **2 passing that should not**:
//! `every_tool_wrapper_declares_its_own_name` (the one this module was written for) and
//! `every_mcp_tool_is_claimed_by_a_route_or_declared_mcp_only` — which the ADR did not name,
//! because it compares the tool set against the ledger's and both shrink together in a way that
//! looks like agreement. With the floors, all 18 fail. **The prediction was one; there were two.**
//! The general rule that produced both: *a check that compares two things derived from one source
//! cannot notice that source shrinking.* Look for that shape rather than for these two names.
//!
//! So: **one accessor, derived from the directory, with the floor inside it.** Not a list of files —
//! a list would be the thirteenth place to forget, which is the shape this module exists to remove.
//! `read_dir` cannot be out of date.
//!
//! Everything here is test-only. It reads from disk (`CARGO_MANIFEST_DIR`) rather than
//! `include_str!` because a macro needs a literal path and there is no literal that means "every
//! file of the surface". `api/route_table.rs::declared_mcp_tools` has always read this way and its
//! doc gives the same reason.

use std::path::{Path, PathBuf};

/// The tool surface's root: `tools.rs` before ADR-086 splits it, `tools/` after. Both are named so
/// this module needs no edit on the day of the split — and so a half-finished split (both present)
/// is read whole rather than half.
fn surface_roots() -> Vec<PathBuf> {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/mcp");
    [src.join("tools.rs"), src.join("tools")]
        .into_iter()
        .filter(|p| p.exists())
        .collect()
}

/// Every file holding part of the tool surface, as `(file name, contents)`, sorted by name.
///
/// Sorted so a caller that reports a finding names the same file every run; a `read_dir` order is
/// not stable across platforms and an unstable message reads as a flaky test.
pub(crate) fn tool_surface_files() -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for root in surface_roots() {
        if root.is_file() {
            let name = root
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("tools.rs")
                .to_owned();
            out.push((name, read(&root)));
            continue;
        }
        for entry in std::fs::read_dir(&root).expect("read src/mcp/tools/") {
            let path = entry.expect("a directory entry").path();
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .expect("a UTF-8 file name")
                .to_owned();
            out.push((name, read(&path)));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    assert!(
        !out.is_empty(),
        "no MCP tool surface found under src/mcp — tools.rs and tools/ are both absent"
    );
    out
}

/// The whole tool surface, concatenated.
///
/// **The floor lives here, so every caller inherits it.** Thirty-six tools are declared today; 34 is
/// the same floor `route_table.rs` has always asserted, and it is a floor rather than a count on
/// purpose — a tool may legitimately be removed, but half the surface disappearing at once is a
/// build accident, not a decision. A caller that needs a tighter number still states its own.
pub(crate) fn tool_surface() -> String {
    let joined = tool_surface_files()
        .into_iter()
        .map(|(_, text)| text)
        .collect::<Vec<_>>()
        .join("\n");
    let declared = joined.matches("#[tool(").count();
    assert!(
        declared >= 34,
        "the tool surface holds only {declared} `#[tool(` declarations; a file of it is missing \
         from what this accessor read, and every check built on it is now checking a fraction of \
         the surface while reporting success"
    );
    joined
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The accessor reads the whole surface, and its floor fires when it does not.
    ///
    /// Written acceptance-first: the positive case runs before the needle, because a test that only
    /// proves a refusal passes against a system that refuses everything
    /// (`rejection-only-tests-pass-when-everything-rejects`).
    #[test]
    fn the_surface_is_read_whole_and_says_so_when_it_is_not() {
        let files = tool_surface_files();
        assert!(!files.is_empty(), "no surface file was found at all");
        let whole = tool_surface();
        assert!(
            whole.matches("#[tool(").count() >= 34,
            "the surface this accessor returns does not hold the tools it must"
        );
        // …and the floor is not decoration: a fraction of the surface must not pass it.
        let fraction: String = whole
            .split("#[tool(")
            .take(10)
            .collect::<Vec<_>>()
            .join("#[tool(");
        assert!(
            fraction.matches("#[tool(").count() < 34,
            "the needle is not a fraction of the surface, so this test proves nothing"
        );
    }

    /// Every `.rs` file under the surface root is in what the accessor returns.
    ///
    /// Not tautological, though it reads that way: it is the assertion that the accessor keeps
    /// deriving its answer from the directory. The moment someone replaces `read_dir` with a
    /// hand-written list — the obvious "tidy-up" — this is what refuses it.
    #[test]
    fn the_accessor_is_derived_from_the_directory_not_from_a_list() {
        let named: std::collections::BTreeSet<String> =
            tool_surface_files().into_iter().map(|(n, _)| n).collect();
        let mut on_disk = std::collections::BTreeSet::new();
        for root in surface_roots() {
            if root.is_file() {
                on_disk.insert(root.file_name().unwrap().to_str().unwrap().to_owned());
                continue;
            }
            for entry in std::fs::read_dir(&root).expect("read the surface directory") {
                let path = entry.expect("a directory entry").path();
                if path.extension().and_then(|e| e.to_str()) == Some("rs") {
                    on_disk.insert(path.file_name().unwrap().to_str().unwrap().to_owned());
                }
            }
        }
        assert_eq!(
            named, on_disk,
            "the accessor and the directory disagree about which files are the tool surface"
        );
    }
}
