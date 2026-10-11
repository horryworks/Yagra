// SPDX-License-Identifier: AGPL-3.0-only
//! This crate's binding of [`yagra_common::srcread`] — reading a module's own source text.
//!
//! **The rule itself is not here**; it is written down once, in `yagra-common/src/srcread.rs`
//! (ADR-091). What is left is the one fact a shared implementation cannot know: **where this crate
//! is on disk**, because `env!("CARGO_MANIFEST_DIR")` expands where the code is written. No module
//! here reads its own text, so only the self-check is left (ADR-202 Inc.6); checks about the whole
//! program are `yagra-core/src/program_guards.rs`, which reads this crate's tree too.

#[cfg(test)]
mod tests {
    use std::path::Path;

    /// This crate's root on disk. See the module doc for why it cannot live in `srcread`.
    const BASE: &str = env!("CARGO_MANIFEST_DIR");

    /// This crate holds itself to the mechanism's invariants, nobody here writes the rule down for
    /// themselves, and nobody binds their own raw text. Eight files, so the floor is 8: it proves
    /// the walk found them.
    #[test]
    fn this_crate_is_readable_and_writes_the_rule_down_nowhere() {
        let src = Path::new(BASE).join("src");
        yagra_common::srcread::assert_crate_is_readable(&src, 8);
        yagra_common::srcread::assert_no_file_spells_the_attribute(&src, 8, &[]);
        yagra_common::srcread::assert_no_file_reads_its_own_raw_text(&src, 8, &[]);
    }
}
