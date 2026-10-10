// SPDX-License-Identifier: AGPL-3.0-only
//! This crate's binding of [`yagra_common::srcread`] — reading a module's own source text.
//!
//! **The rule itself is not here.** It is written down once, in `yagra-common/src/srcread.rs`,
//! along with why it removes every top-level test-only *item* rather than cutting at the first one
//! (ADR-091). What is left here is the one fact a shared implementation cannot know: **where this
//! crate is on disk**, because `env!("CARGO_MANIFEST_DIR")` expands where the code is written.
//!
//! It is `yagra-core`'s binding, cut to what this crate's own checks call (ADR-202 Inc.5). A check
//! about the whole program does not use it: those live in `yagra-core/src/program_guards.rs`,
//! which reads this crate's tree beside core's.

use std::path::{Path, PathBuf};
pub(crate) use yagra_common::srcread::files;

/// This crate's root on disk. See the module doc for why it cannot live in `srcread`.
const BASE: &str = env!("CARGO_MANIFEST_DIR");

/// Both spellings of a module root, relative to this crate: `<dir>/<stem>.rs` and `<dir>/<stem>/`.
pub(crate) fn roots(dir: &str, stem: &str) -> Vec<PathBuf> {
    yagra_common::srcread::roots_in(Path::new(BASE), dir, stem)
}

/// [`yagra_common::srcread::code_in`] with whole-line `//` comments dropped.
pub(crate) fn code_no_comments(dir: &str, stem: &str) -> String {
    yagra_common::srcread::code_no_comments_in(Path::new(BASE), dir, stem)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// This crate holds itself to the mechanism's invariants, and nobody here writes the rule down
    /// for themselves.
    ///
    /// The floors are this crate's, which is why they are arguments — see `srcread`'s module doc.
    /// 25 is below what the crate has (31 files at the split); they are here so that a walk which
    /// stops finding sources fails instead of passing over nothing.
    #[test]
    fn this_crate_is_readable_and_writes_the_rule_down_nowhere() {
        let src = Path::new(BASE).join("src");
        yagra_common::srcread::assert_crate_is_readable(&src, 25);
        yagra_common::srcread::assert_no_file_spells_the_attribute(&src, 25, &[]);
        yagra_common::srcread::assert_no_file_reads_its_own_raw_text(&src, 25, &[]);
    }
}
