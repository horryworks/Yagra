// SPDX-License-Identifier: AGPL-3.0-only
//! This crate's binding of [`yagra_common::srcread`] — reading a module's own source text.
//!
//! **The rule itself is not here.** It is written down once, in `yagra-common/src/srcread.rs`,
//! along with why it removes every top-level test-only *item* rather than cutting at the first one
//! (ADR-091). What is left here is the one fact a shared implementation cannot know: **where this
//! crate is on disk**, because `env!("CARGO_MANIFEST_DIR")` expands where the code is written.
//!
//! ⚠️ It was `yagra-core`'s six lines verbatim until ADR-108 Inc.2, which added [`crate_files`] —
//! the crate-wide walk, needed because the shared `files` refuses a nested module directory and
//! this crate has `worker/`. That is still a *binding* rather than a second copy of the rule: the
//! cut it applies is `srcread`'s.
//!
//! ⚠️ **This crate had no source-reading check at all until ADR-099** — sixteen files and roughly
//! ten thousand lines, including the 4,360-line worker. The mechanism lived in `yagra-core`, and the
//! poller cannot depend on core (they talk only over the bus, `coding-conventions.md`). Copying the
//! rule here would have been the twenty-fourth copy ADR-091 exists to refuse, so it moved to
//! `yagra-common` behind `test-util` instead.

use std::path::{Path, PathBuf};

/// This crate's root on disk. See the module doc for why it cannot live in `srcread`.
const BASE: &str = env!("CARGO_MANIFEST_DIR");

/// Both spellings of a module root, relative to this crate: `<dir>/<stem>.rs` and `<dir>/<stem>/`.
pub(crate) fn roots(dir: &str, stem: &str) -> Vec<PathBuf> {
    yagra_common::srcread::roots_in(Path::new(BASE), dir, stem)
}

/// The whole module's code, concatenated.
///
/// ⚠️ `dir` may leave this crate — `"../yagra-bus/src"` is how `guards.rs` derives the capability
/// vocabulary from the message definitions, the same shape `yagra-core`'s `api/metrics.rs` uses to
/// read this crate's worker. It resolves against `BASE`, so it is a path and not a package name.
pub(crate) fn code(dir: &str, stem: &str) -> String {
    yagra_common::srcread::code_in(Path::new(BASE), dir, stem)
}

/// Every production file in this crate, as `(file name, code)`.
///
/// For a check whose question is about the crate rather than about one module — "this metric name
/// appears in exactly one place". [`yagra_common::srcread::files`] cannot answer it here: it refuses
/// a nested module directory on purpose, and this crate has `worker/`. So the walk is `rs_files`
/// plus the same cut, which is what `assert_crate_is_readable` does. It is here rather than at the
/// call site because `BASE` is the one fact this file exists to hold.
pub(crate) fn crate_files() -> Vec<(String, String)> {
    use yagra_common::srcread as sr;
    let src = Path::new(BASE).join("src");
    let mut paths = Vec::new();
    sr::rs_files(&src, &mut paths);
    paths.sort();
    // ⚠️ Test-only modules are declared by the *crate root* and by each directory's `mod.rs`, and
    // `src/` has no `mod.rs` — so the root is asked as a file and the directories as directories.
    // Skipping them is the whole reason a check can search for a needle it also writes down.
    let mut skip = sr::test_only_modules(&src.join("main.rs"));
    for dir in paths.iter().filter_map(|p| p.parent()) {
        skip.extend(sr::test_only_modules(dir));
    }
    paths
        .iter()
        .map(|p| (sr::file_name(p), p))
        .filter(|(name, _)| !skip.contains(name))
        .map(|(name, p)| {
            let code = sr::strip_and_check(&name, &sr::read(p));
            (name, code)
        })
        .collect()
}

/// Every file of the module as `(file name, code)`, with whole-line `//` comments dropped.
///
/// The comment-free form is what a "this pattern must not appear" check wants, and naming the
/// file is what lets a finding say *where* — see `srcread`'s doc on why neither is done at the
/// call site.
pub(crate) fn files_no_comments(roots: &[PathBuf]) -> Vec<(String, String)> {
    yagra_common::srcread::files_no_comments(roots)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// This crate holds itself to the mechanism's two invariants, and nobody here writes the rule
    /// down for themselves.
    ///
    /// The floors are this crate's, which is why they are arguments — see `srcread`'s module doc.
    /// 20 is what the crate has today; they are here so that a walk which stops finding sources
    /// fails instead of passing over nothing.
    ///
    /// ⚠️ **The third check is the strict form, and only because this crate earned it.** ADR-102
    /// deliberately policed the *use* rather than the read — a literal needle thrown at one's own
    /// raw text — because forbidding `include_str!` outright would have needed an exemption list
    /// and `main.rs` would have been its only member (the `leaving` beat check wanted the raw
    /// file). ADR-103 moved that check to `heartbeat.rs` and pointed it at [`code`], so the list
    /// has nobody on it and the strict form costs nothing. `yagra-core` still has eight legitimate
    /// raw readers and keeps the lenient form; demanding zero there would only teach the next
    /// person to write themselves into `exempt`.
    #[test]
    fn this_crate_is_readable_and_writes_the_rule_down_nowhere() {
        let src = Path::new(BASE).join("src");
        yagra_common::srcread::assert_crate_is_readable(&src, 20);
        yagra_common::srcread::assert_no_file_spells_the_attribute(&src, 20, &[]);
        yagra_common::srcread::assert_no_file_reads_its_own_raw_text(&src, 20, &[]);
    }
}
