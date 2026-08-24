// SPDX-License-Identifier: AGPL-3.0-only
//! This crate's binding of [`yagra_common::srcread`] — reading a module's own source text.
//!
//! **The rule itself is not here**, and this file is deliberately the same six lines as
//! `yagra-core`'s. It is written down once, in `yagra-common/src/srcread.rs`, along with why it
//! removes every top-level test-only *item* rather than cutting at the first one (ADR-091). What is
//! left here is the one fact a shared implementation cannot know: **where this crate is on disk**,
//! because `env!("CARGO_MANIFEST_DIR")` expands where the code is written.
//!
//! ⚠️ **This crate had no source-reading check at all until ADR-099** — sixteen files and roughly
//! ten thousand lines, including the 4,360-line worker. The mechanism lived in `yagra-core`, and the
//! poller cannot depend on core (they talk only over the bus, `coding-conventions.md`). Copying the
//! rule here would have been the twenty-fourth copy ADR-091 exists to refuse, so it moved to
//! `yagra-common` behind `test-util` instead.

use std::path::Path;

/// This crate's root on disk. See the module doc for why it cannot live in `srcread`.
const BASE: &str = env!("CARGO_MANIFEST_DIR");

#[cfg(test)]
mod tests {
    use super::*;

    /// This crate holds itself to the mechanism's two invariants, and nobody here writes the rule
    /// down for themselves.
    ///
    /// Both floors are this crate's, which is why they are arguments — see `srcread`'s module doc.
    /// The floor of 16 is what the crate has today; it is here so that a walk which stops finding
    /// sources fails instead of passing over nothing.
    ///
    /// ⚠️ `main.rs` reads its own text with `include_str!` (the `leaving` heartbeat must flush). That
    /// is not an offence against the second check — it slices by a *production* needle rather than
    /// defining "production code" for itself, which is the thing that was wrong twenty-three times.
    #[test]
    fn this_crate_is_readable_and_writes_the_rule_down_nowhere() {
        let src = Path::new(BASE).join("src");
        yagra_common::srcread::assert_crate_is_readable(&src, 16);
        yagra_common::srcread::assert_no_file_spells_the_attribute(&src, 16, &[]);
    }
}
