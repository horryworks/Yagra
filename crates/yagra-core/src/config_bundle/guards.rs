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

/// The one table read but never carried: an imported reference is kept only when the target already
/// holds that exact credential id (ADR-018 — no path here turns a sealed secret back into
/// transportable plaintext).
const READ_NOT_CARRIED: &str = "credentials";

/// Which tables each production file of `config_bundle/` may name in SQL — the split's rule, as
/// data.
///
/// `export.rs` is **derived** rather than listed: it may name what the bundle carries and nothing
/// else, so its row cannot drift from [`BUNDLE_TABLES`]. The two import rows are the placement rule
/// itself — which half of the importer a table belongs to — and are checked against the same list
/// below, in both directions, so neither can quietly grow a table the bundle does not carry nor
/// lose one it does.
///
/// Hand-maintained for those two rows, and that is tolerable for the reason `repo/guards.rs` gives:
/// **it falls the safe way.** A table a file uses but has not declared fails the test naming both;
/// a declaration nothing uses is caught by the union check.
const TABLE_OWNERSHIP: &[(&str, &[&str])] = &[
    // The vocabulary, the repo handle, the header check and the note accumulator. No SQL.
    ("mod.rs", &[]),
    // Serde shapes only — the file exists precisely so that it holds no query.
    ("types.rs", &[]),
    // The transaction, the report, and six helpers that take their SQL as a `&'static str` from the
    // call site. The file that owns the transaction owns none of the schema.
    ("import.rs", &[]),
    // What is monitored. Everything in the other half points at one of these.
    (
        "import_inventory.rs",
        &[
            "classification_rules",
            "collection_template_items",
            "collection_templates",
            "credentials",
            "node_groups",
            "nodes",
            "profile_collection_templates",
            "profiles",
        ],
    ),
    // How it is monitored. Everything here points back at the half above.
    (
        "import_attached.rs",
        &[
            "analysis_schedules",
            "app_settings",
            "dns_checks",
            "event_rules",
            "event_sources",
            "forward_destinations",
            "report_definitions",
            "report_schedules",
            "thresholds",
            "url_checks",
        ],
    ),
];

/// Every table `code` names in **write** position, in order, with repeats.
///
/// A narrower question than [`crate::sql_tables::references`], which does not distinguish
/// direction — and deliberately local, because there is one caller. `sql_tables` became a module
/// when a *second* caller needed the same rule; this is that reasoning read the other way round.
/// Move it there when something else asks the question.
fn writes(code: &str, vocab: &BTreeSet<String>) -> Vec<String> {
    let sql = regex::Regex::new(r"\b(?:INTO|UPDATE|DELETE\s+FROM)\s+([a-z_][a-z0-9_]*)")
        .expect("a valid pattern");
    let stripped = code
        .lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");
    sql.captures_iter(&stripped)
        .map(|caps| caps[1].to_owned())
        .filter(|table| vocab.contains(table))
        .collect()
}

/// **A statement may only name a table its file has declared, and nothing outside the bundle's own
/// list may be written at all.**
///
/// This is the module doc's "what is deliberately not carried" list — users, api_tokens,
/// oidc_providers, credentials, notification_channels, routing_rules, llm_config, ldap_config,
/// meraki_*, pollers, mib_catalog, user_dashboards, user_preferences, maintenance_windows, mutes —
/// as a build failure rather than as forty lines of prose. Every one of those exclusions is a
/// decision with its reason beside it, and the sharpest is the first: an import is a write path, so
/// carrying accounts across would make "restore a config" the shortest route to granting yourself a
/// role.
///
/// **The two filters live in [`crate::sql_tables`]** — a name counts only in SQL position *and*
/// only if it is a real table derived from `migrations/`, and neither is sufficient alone.
///
/// 🚨 **Three floors, because this is a check whose healthy answer is "found nothing".** One on the
/// vocabulary, one on the files seen, and one on the statements that survived both filters. The
/// last counts inspected *sites* rather than files gathered — counting the wrong set is the mistake
/// ADR-091 made in its own guard and only found by breaking it.
#[test]
fn every_statement_names_a_table_its_file_declares() {
    let vocab = crate::sql_tables::vocabulary();
    assert!(
        vocab.len() >= 55,
        "only {} tables were derived from migrations/, so this check can barely see anything; \
         every table it cannot name is a table it silently skips",
        vocab.len()
    );

    let carried: BTreeSet<&str> = BUNDLE_TABLES.iter().copied().collect();
    // The export may name what the bundle carries, plus the settings table it takes two columns of.
    let exported: Vec<&str> = carried
        .iter()
        .copied()
        .chain(["app_settings"])
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let mut declared: std::collections::BTreeMap<&str, Vec<&str>> = TABLE_OWNERSHIP
        .iter()
        .map(|(name, tables)| (*name, tables.to_vec()))
        .collect();
    declared.insert("export.rs", exported.clone());

    let files = files();
    let present: BTreeSet<&str> = files.iter().map(|(name, _)| name.as_str()).collect();
    assert!(
        present.len() >= 6,
        "only {} production files were read; the reader has stopped seeing this module",
        present.len()
    );
    // Both directions against the directory: a new file cannot arrive unlisted, and a renamed one
    // cannot leave a stale row behind.
    for name in &present {
        assert!(
            declared.contains_key(name),
            "config_bundle/{name} is not in TABLE_OWNERSHIP, so nothing checks what it reads or \
             writes"
        );
    }
    for name in declared.keys() {
        assert!(
            present.contains(name),
            "TABLE_OWNERSHIP names config_bundle/{name}, which no longer exists"
        );
    }

    // The two import rows must between them account for exactly what the bundle carries, plus the
    // settings columns and the one table that is read and never written. Derived from
    // `BUNDLE_TABLES`, so adding a table to the bundle and forgetting to place it fails here.
    let importable: BTreeSet<&str> = declared["import_inventory.rs"]
        .iter()
        .chain(declared["import_attached.rs"].iter())
        .copied()
        .collect();
    let expected: BTreeSet<&str> = carried
        .iter()
        .copied()
        .chain(["app_settings", READ_NOT_CARRIED])
        .collect();
    assert_eq!(
        importable, expected,
        "the two halves of the importer do not between them cover exactly the carried tables"
    );

    let mut checked = 0usize;
    let mut wrong: Vec<String> = Vec::new();
    for (name, text) in &files {
        let allowed = &declared[name.as_str()];
        for table in crate::sql_tables::references(text, &vocab) {
            checked += 1;
            if !allowed.contains(&table.as_str()) {
                wrong.push(format!(
                    "config_bundle/{name} names `{table}`, which it does not declare. Either the \
                     block belongs in the other half of the importer, or — if `{table}` is not in \
                     `BUNDLE_TABLES` — read why it is not carried, in this module's docs, before \
                     adding it"
                ));
            }
        }
        // Writes are the narrower question: the bundle carries no secret, no account and no
        // deployment-local inventory, so nothing outside its own list may be written at all.
        for table in writes(text, &vocab) {
            if !exported.contains(&table.as_str()) {
                wrong.push(format!(
                    "config_bundle/{name} writes to `{table}`, which the bundle does not carry. An \
                     import is a write path; see the exclusion list in this module's docs"
                ));
            }
        }
    }
    wrong.sort();
    wrong.dedup();
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    assert!(
        checked >= 45,
        "only {checked} statements were inspected; the scan has stopped matching and would report \
         a module full of misplaced queries as clean"
    );
}

/// **The export writes nothing.**
///
/// `GET /api/v1/config/bundle` is a read, and until now nothing said so: the export was a 423-line
/// function whose read-only-ness was a property of every statement in it rather than a property
/// anybody checked. One `INSERT` or `UPDATE` added while chasing a missing row would have made a
/// GET mutate the deployment it was describing.
///
/// The floor is the read count — a reader that came back empty writes nothing either.
#[test]
fn the_export_writes_nothing() {
    let vocab = crate::sql_tables::vocabulary();
    let (_, export) = files()
        .into_iter()
        .find(|(name, _)| name == "export.rs")
        .expect("config_bundle/export.rs exists");
    let reads = crate::sql_tables::references(&export, &vocab).len();
    assert!(
        reads >= 16,
        "the export was read as {reads} statements; it selects from seventeen tables, so this \
         check is looking at the wrong text"
    );
    let writes = writes(&export, &vocab);
    assert!(
        writes.is_empty(),
        "the export writes to {writes:?}; it answers a GET and must only read"
    );
}
