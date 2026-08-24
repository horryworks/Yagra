// SPDX-License-Identifier: AGPL-3.0-only
//! This crate's binding of [`yagra_common::srcread`] — reading a module's own source text.
//!
//! **The rule itself is not here.** It is written down once, in `yagra-common/src/srcread.rs`, along
//! with why it removes every top-level test-only *item* rather than cutting at the first one
//! (ADR-091) and why it looks for both spellings of a root (ADR-089). Read that module before
//! touching anything that reads source as a string.
//!
//! What is left here is one fact `srcread` cannot know: **where this crate is on disk.**
//! `env!("CARGO_MANIFEST_DIR")` expands where the code is *written*, so a shared implementation
//! resolves to `crates/yagra-common` and every relative path a caller passes would mean the wrong
//! thing. Binding it here keeps the fifty-three call sites in this crate spelling `dir` exactly as
//! they always have — `"src/mcp"`, `"src/analysis"`, `"../yagra-poller/src"` — and means moving the
//! mechanism to a shared crate (ADR-099, so `yagra-poller` could reach it) changed none of them.
//!
//! The over-cut direction is guarded here rather than in `srcread`, for the reason its own doc
//! gives: a general assertion would have to know what a file is *supposed* to contain, which is the
//! caller's floor. So the two files that were actually being mis-read are named below.

use std::path::{Path, PathBuf};
pub(crate) use yagra_common::srcread::{files, files_no_comments, test_only_modules};

/// This crate's root on disk. See the module doc for why it cannot live in `srcread`.
const BASE: &str = env!("CARGO_MANIFEST_DIR");

/// Both spellings of a module root, relative to this crate: `<dir>/<stem>.rs` and `<dir>/<stem>/`.
pub(crate) fn roots(dir: &str, stem: &str) -> Vec<PathBuf> {
    yagra_common::srcread::roots_in(Path::new(BASE), dir, stem)
}

/// The whole module's code, concatenated.
pub(crate) fn code(dir: &str, stem: &str) -> String {
    yagra_common::srcread::code_in(Path::new(BASE), dir, stem)
}

/// [`code`] with whole-line `//` comments dropped.
pub(crate) fn code_no_comments(dir: &str, stem: &str) -> String {
    yagra_common::srcread::code_no_comments_in(Path::new(BASE), dir, stem)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **Two files this crate really holds, read back whole.**
    ///
    /// The synthetic strings in `srcread` prove the algorithm; these two prove it against the shapes
    /// that were actually being mis-read, and they name the files so a regression says where to
    /// look. This is the over-cut direction, which `srcread`'s own invariants cannot see: restoring
    /// the old "cut at the first attribute" rule satisfies both of them on every file in the
    /// workspace, and fails only here.
    #[test]
    fn the_two_files_that_were_being_mis_read_come_back_whole() {
        // `config.rs`: two inline test modules with production code between them.
        let cfg = code("src", "config");
        for needle in ["fn interval_in_bounds(", "fn parse_interval("] {
            assert!(
                cfg.contains(needle),
                "config.rs is missing `{needle}`, which sits between its two test modules"
            );
        }
        assert!(!cfg.contains("mod system_log_days_tests"), "…and both go");

        // `logstore.rs`: a test-only `use` on line 16 used to end the file there.
        let logs = code("src", "logstore");
        assert!(
            logs.contains("impl LogStore for VlStore"),
            "logstore.rs was read as its first few lines again — the production impl is gone"
        );
        assert!(
            logs.lines().count() > 500,
            "logstore.rs came back as {} lines; it used to come back as 15",
            logs.lines().count()
        );
    }

    /// This crate holds itself to the mechanism's two invariants, and nobody here writes the rule
    /// down for themselves.
    ///
    /// Both floors are this crate's, which is why they are arguments — see `srcread`'s module doc.
    /// ⚠️ No file is exempt from the second one any more: the rule lives in `yagra-common` now, and
    /// this wrapper does not spell the attribute.
    #[test]
    fn this_crate_is_readable_and_writes_the_rule_down_nowhere() {
        let src = Path::new(BASE).join("src");
        yagra_common::srcread::assert_crate_is_readable(&src, 150);
        yagra_common::srcread::assert_no_file_spells_the_attribute(&src, 150, &[]);
    }
}
