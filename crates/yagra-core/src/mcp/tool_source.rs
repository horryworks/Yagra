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
//! **After the split, dropping one domain file from what this returns fails 20 tests** — every
//! source-text reader plus this module's own two. That is the state the increment was for: a
//! botched split cannot be quiet.
//!
//! **How it reads is no longer written here** (ADR-089). Finding both spellings of a root,
//! removing each file's *own* test-only items (ADR-091 — not cutting at the first one, which read
//! ten files in this crate as 1–14% of themselves), and deriving the exclusions from `mod.rs` are
//! all in [`crate::module_source`], because `analysis.rs` needed the identical three and writing
//! them twice is how the second copy comes to disagree. What stays here is what is actually about
//! MCP: which root, and **the floor** — 34 `#[tool(` declarations. The floor deliberately did not
//! move: a shared floor would hide what is being counted from the surface that cares.
//!
//! Everything here is test-only, for the reason `module_source` gives.

use crate::module_source;

/// Every file holding part of the tool surface, as `(file name, contents)`, sorted by name.
///
/// The contents are the **code**: each file has its own test-only items removed, and the modules that
/// are test-only in full are not here at all. Every caller wants it that way — a tool name, a
/// `#[tool(` attribute, a refusal's wording and a folded branch's argument all live in a tool body.
pub(crate) fn tool_surface_files() -> Vec<(String, String)> {
    let out = module_source::files(&module_source::roots("src/mcp", "tools"));
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

    /// The accessor returns every `.rs` file under the surface root **except** the test-only
    /// modules, and both halves of that are derived rather than listed.
    ///
    /// Not tautological, though it reads that way: it is the assertion that the accessor keeps
    /// deriving its answer from the directory and from `mod.rs`. The moment someone replaces either
    /// `read_dir` or the `#[cfg(test)] mod …` scan with a hand-written list — the obvious "tidy-up"
    /// — this is what refuses it, because a list would not track a file added afterwards.
    #[test]
    fn the_accessor_is_derived_from_the_directory_not_from_a_list() {
        let named: std::collections::BTreeSet<String> =
            tool_surface_files().into_iter().map(|(n, _)| n).collect();
        let roots = module_source::roots("src/mcp", "tools");
        let mut on_disk = std::collections::BTreeSet::new();
        let mut skip = std::collections::BTreeSet::new();
        for root in &roots {
            if root.is_file() {
                on_disk.insert(root.file_name().unwrap().to_str().unwrap().to_owned());
                continue;
            }
            skip.extend(module_source::test_only_modules(root));
            for entry in std::fs::read_dir(root).expect("read the surface directory") {
                let path = entry.expect("a directory entry").path();
                if path.extension().and_then(|e| e.to_str()) == Some("rs") {
                    on_disk.insert(path.file_name().unwrap().to_str().unwrap().to_owned());
                }
            }
        }
        let expected: std::collections::BTreeSet<String> =
            on_disk.difference(&skip).cloned().collect();
        assert_eq!(
            named, expected,
            "the accessor and the directory disagree about which files are the tool surface"
        );
        // Both halves must be doing something, or the equality above is one empty set against
        // another dressed up as agreement.
        assert!(
            named.len() >= 5 && !skip.is_empty(),
            "surface files: {named:?}, test-only: {skip:?} — one of the two derivations returned \
             nothing, so this test compares nothing"
        );
        // …and the exclusion must be excluding the file that reads this surface. `guards.rs` sits
        // inside the directory it greps, so if it were included its own needles would match
        // themselves — which is exactly what happened on the day of the split.
        assert!(
            skip.contains("guards.rs"),
            "guards.rs is not being excluded; its own literals are back in the text it searches"
        );
    }

    /// A file's own `#[cfg(test)]` tail is not part of the surface.
    #[test]
    fn a_files_test_module_is_cut_away_from_its_code() {
        let files = tool_surface_files();
        let (name, code) = files
            .iter()
            .find(|(n, _)| n == "system.rs")
            .expect("system.rs is part of the surface");
        assert!(
            code.contains("async fn get_config"),
            "{name}: the code half is missing the tool it declares"
        );
        // That the test module went is asserted inside `module_source::files` itself since
        // ADR-091, so every caller inherits it instead of each one remembering to ask.
    }
}
