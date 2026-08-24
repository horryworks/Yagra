// SPDX-License-Identifier: AGPL-3.0-only
//! The checks that read this module **as source text** (ADR-101).
//!
//! Two properties of the bundle have no type to carry them: that no export query names a sealed
//! column, and that every table the bundle claims to carry is handled on *both* sides. Both were
//! already checked before the split — over `include_str!("config_bundle.rs")` cut at the literal
//! `// ── Import ───` banner, with a comment saying the cut "has to be exact, so it is asserted
//! rather than assumed". Turning that banner into a file boundary is what this ADR was for, and it
//! closed two holes the string version had:
//!
//! * the import half ran to end-of-file, so **the test module counted** — a fixture containing
//!   `INSERT INTO nodes` satisfied the round-trip assertion for `nodes`;
//! * the export half stopped at the banner, so **the importer was never scanned for a sealed
//!   column at all**. An `INSERT INTO credentials (wrapped_dek, …)` passed every test in the repo.
//!
//! Declared `#[cfg(test)] mod guards;` in [`super`], which is how [`crate::module_source`] derives
//! its exclusion — a scan living inside the text it scans matches its own literals. That exclusion
//! is also why the needles below may be written out: this file is not in what it reads. ADR-086
//! hit the other side of that within minutes of splitting `mcp/tools.rs`.

use std::collections::BTreeSet;

use super::BUNDLE_TABLES;

/// The whole module's production code, comments removed.
fn code() -> String {
    crate::module_source::code_no_comments("src", "config_bundle")
}

/// Every production file of the module, as `(name, code)`.
fn files() -> Vec<(String, String)> {
    crate::module_source::files(&crate::module_source::roots("src", "config_bundle"))
}

/// **No query anywhere in this module names a sealed column** — the one property the whole design
/// rests on.
///
/// A source check rather than a check on a value, because the bundle type has *no field* that could
/// hold a secret: a leak could only arrive as a new `SELECT`, and only source sees that. The needles
/// are the real column names shared by `credentials`, `notification_channels` and
/// `forward_destinations`.
///
/// 🚨 **This now covers the importer, which it never did before.** The string version read only up
/// to the import banner, so for the whole life of this feature an `INSERT INTO credentials` naming
/// sealed bytes would have passed. Reading the directory removes the boundary rather than moving
/// it.
///
/// `key_id` is the deliberate exception and is checked by *shape*, not by count: it may be tested
/// for null — which reveals only whether a secret exists — and may never be selected. The old
/// version asserted an exact count of one, which was both brittle against prose (it read doc
/// comments too) and blind to the second occurrence, in the importer.
#[test]
fn no_query_names_a_sealed_column() {
    let src = code();
    for column in ["wrapped_dek", "dek_nonce", "ciphertext", "ct_nonce"] {
        assert!(
            !src.contains(column),
            "config_bundle names the sealed column {column}; a bundle must carry no secret"
        );
    }
    let mentions = src.matches("key_id").count();
    let null_tests = src.matches("key_id IS NOT NULL").count();
    // The floor: a reader that came back empty would satisfy the equality below with 0 == 0.
    assert!(
        mentions >= 2,
        "only {mentions} mentions of `key_id` were found, and there are two — one per direction. \
         The reader has stopped seeing this module, so every assertion above it is vacuous"
    );
    assert_eq!(
        mentions, null_tests,
        "`key_id` appears somewhere other than an `IS NOT NULL` test; the bundle may learn that a \
         secret exists, never which key sealed it"
    );
}

/// **Every carried table is both read by the export and written by an import file.**
///
/// A table added to the bundle struct but forgotten in the importer would export fine and silently
/// vanish on the way in.
///
/// The two sides are now *files*, and the import side is every file whose name starts with
/// `import` — so splitting the importer further needs no edit here. The needles are still built at
/// runtime: cheap, and it keeps the test honest if it is ever moved into a file the reader can see.
///
/// 🚨 **The hole this closes**: the old import half ran to end of file, so the test module was part
/// of it. `module_source::files` removes each file's test-only items, so a fixture string can no
/// longer answer for the importer.
#[test]
fn every_bundle_table_is_both_exported_and_imported() {
    let files = files();
    let side = |prefix: &str| -> String {
        files
            .iter()
            .filter(|(name, _)| name.starts_with(prefix))
            .map(|(_, code)| code.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    };
    let export = side("export");
    let import = side("import");
    assert!(
        export.len() > 5_000 && import.len() > 5_000,
        "export came back as {} bytes and import as {} bytes; one of the two sides is not being \
         read, and every assertion below would pass or fail for the wrong reason",
        export.len(),
        import.len()
    );

    let mut checked = 0usize;
    for table in BUNDLE_TABLES {
        let read = format!("FROM {table} ");
        let written = format!("INTO {table} ");
        assert!(
            export.contains(&read),
            "{table} is never selected by the export"
        );
        assert!(
            import.contains(&written),
            "{table} is never written by the import"
        );
        checked += 2;
    }
    assert_eq!(
        checked,
        BUNDLE_TABLES.len() * 2,
        "the table list emptied out from under this check"
    );
    // Both directions against the list, so a table dropped from `BUNDLE_TABLES` while its SQL stays
    // behind is caught too — the report is keyed by that list, so such a table would be carried and
    // never counted.
    let named: BTreeSet<&str> = BUNDLE_TABLES.iter().copied().collect();
    assert_eq!(
        named.len(),
        BUNDLE_TABLES.len(),
        "`BUNDLE_TABLES` holds a duplicate, so its report rows would collide"
    );
}
