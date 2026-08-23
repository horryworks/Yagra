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

use std::collections::BTreeSet;
use std::path::Path;

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

/// Which tables each production file of `repo/` may name in SQL — the split's rule, as data.
///
/// Hand-maintained, and that is tolerable for the same reason `retention.rs::PRUNE_SITES` is:
/// **it falls the safe way.** A table a file uses but has not declared fails the test naming both;
/// a declaration nothing uses is invisible. The dangerous direction is the one that cannot happen.
///
/// Both directions against the directory are checked below, so a new file cannot arrive unlisted
/// and a renamed one cannot leave a stale row behind.
const TABLE_OWNERSHIP: &[(&str, &[&str])] = &[
    // No SQL of its own: the type, the connection, and the column list and scope predicate the
    // other files interpolate. `connect` and `healthy` issue `SELECT 1`, which names no table.
    ("mod.rs", &[]),
    // A `const` table of seeded alert rules. `super::seed` is what writes it.
    ("defaults.rs", &[]),
    // The two joins are for display names (`node_facts` answers "what is this node called, in
    // which folder, on which profile"), not a second file's worth of `profiles` logic.
    ("nodes.rs", &["nodes", "node_groups", "profiles"]),
    // `NodeListing for NodeRepo`. Separate from `nodes.rs` because this file is the *mirror*:
    // the SQL scope predicate and `StaticNodeList`'s in-memory twin, with the tests that pin them.
    ("listing.rs", &["nodes"]),
    ("interfaces.rs", &["interfaces"]),
    ("profiles.rs", &["profiles"]),
    ("settings.rs", &["app_settings"]),
    ("snapshots.rs", &["node_state_snapshots"]),
    // sqlx's own bookkeeping table; no migration declares it, so `table_vocabulary` adds it by hand.
    ("migrate.rs", &["_sqlx_migrations"]),
    // 🚨 The one file that is not about a table, and the exemption is structural rather than
    // granted: seeding is by definition writing the whole catalogue. Listing all eight is the
    // point — a ninth still has to be added here, so "the seeder writes everywhere" never becomes
    // a wildcard nobody reads.
    (
        "seed.rs",
        &[
            "classification_rules",
            "collection_items",
            "collection_template_items",
            "collection_templates",
            "nodes",
            "profile_collection_templates",
            "profiles",
            "thresholds",
        ],
    ),
];

/// Every table this deployment has, derived from `migrations/` rather than written down.
///
/// Derived because a hand-written vocabulary would be the very thing this check exists to stop: a
/// list that falls behind the schema, quietly narrowing what the scan below can even see. A table
/// missing from here is a table the scan skips, so this list going empty would make the whole check
/// vacuous — which is why its size has a floor of its own.
fn table_vocabulary() -> BTreeSet<String> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../migrations");
    let mut out = BTreeSet::new();
    for entry in std::fs::read_dir(&dir).expect("migrations/ is readable from the crate directory")
    {
        let path = entry.expect("a readable directory entry").path();
        if path.extension().is_none_or(|x| x != "sql") {
            continue;
        }
        let sql = std::fs::read_to_string(&path).expect("migration is readable");
        // Whitespace is collapsed first: `CREATE TABLE` and `IF NOT EXISTS` are split across lines
        // in several migrations, and a line-oriented scan reads the tail of the first line as the
        // table name. Measured — it produced `if` and `node_l` as "tables".
        let flat = sql
            .to_lowercase()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        for tail in flat.split("create table ").skip(1) {
            let tail = tail.strip_prefix("if not exists ").unwrap_or(tail);
            let name: String = tail
                .chars()
                .take_while(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '_')
                .collect();
            if !name.is_empty() {
                out.insert(name);
            }
        }
    }
    // sqlx creates this one itself, so no migration declares it — and `migrate.rs` reads it.
    out.insert("_sqlx_migrations".to_owned());
    out
}

/// **A statement may only name a table its file has declared** — the rule the split was cut on.
///
/// Placement in this module is decided by the table a method's SQL names, not by what the method is
/// called (two of them disagreed: `suppression_opt_outs` and its setter sat among the deployment
/// settings and read a per-node column). A rule written only in `mod.rs`'s prose is a rule that
/// holds until the next person adds a method next to the one they were reading, so it is here as a
/// test instead.
///
/// **Two filters, and neither is sufficient alone.** A name is counted only when it appears in SQL
/// position (after `FROM` / `INTO` / `UPDATE` / `JOIN`) *and* is a real table. Position alone
/// matches `FROM unnest($1::uuid[])` and `extract(epoch FROM last_seen)`; vocabulary alone matches
/// `nodes` inside `list_nodes` and a dozen other identifiers.
///
/// 🚨 **Two floors, because this is a check that reports "nothing wrong" when it sees nothing.**
/// One on the vocabulary and one on the statements actually inspected — and the second counts the
/// sites that survived both filters, not the files gathered. Counting the wrong set is the mistake
/// ADR-091 made in its own guard and only found by breaking it.
#[test]
fn every_statement_names_a_table_its_file_declares() {
    let vocab = table_vocabulary();
    assert!(
        vocab.len() >= 55,
        "only {} tables were derived from migrations/, so this check can barely see anything; \
         every table it cannot name is a table it silently skips",
        vocab.len()
    );

    let declared: std::collections::BTreeMap<&str, &[&str]> =
        TABLE_OWNERSHIP.iter().copied().collect();
    let files = crate::module_source::files(&crate::module_source::roots("src", "repo"));

    // Both directions against the directory. `files` already omits the test-only modules `mod.rs`
    // declares, so this file is not expected to appear and cannot match its own literals.
    let present: BTreeSet<&str> = files.iter().map(|(name, _)| name.as_str()).collect();
    for name in &present {
        assert!(
            declared.contains_key(name),
            "repo/{name} is not in TABLE_OWNERSHIP, so nothing checks what it reads or writes"
        );
    }
    for (name, _) in TABLE_OWNERSHIP {
        assert!(
            present.contains(name),
            "TABLE_OWNERSHIP names repo/{name}, which no longer exists"
        );
    }

    let sql = regex::Regex::new(r"\b(?:FROM|INTO|UPDATE|JOIN)\s+([a-z_][a-z0-9_]*)")
        .expect("a valid pattern");
    let mut checked = 0usize;
    let mut wrong: Vec<String> = Vec::new();
    for (name, text) in &files {
        // Whole-line comments out first: prose explaining a query would otherwise read as one.
        let code = text
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        let allowed = declared[name.as_str()];
        for caps in sql.captures_iter(&code) {
            let table = &caps[1];
            if !vocab.contains(table) {
                continue; // not a table — `unnest(…)`, a column in `extract(epoch FROM …)`
            }
            checked += 1;
            if !allowed.contains(&table) {
                wrong.push(format!(
                    "repo/{name} names `{table}`, which it does not declare. Either the method \
                     belongs in the file for `{table}`, or `{table}` belongs in this file's \
                     TABLE_OWNERSHIP row with a comment saying why"
                ));
            }
        }
    }
    // Reported all at once: one method in the wrong file usually brings several statements with it,
    // and one failure per run would mean one round trip each.
    wrong.sort();
    wrong.dedup();
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    assert!(
        checked >= 60,
        "only {checked} statements were inspected; the scan has stopped matching and would report \
         a module full of misplaced queries as clean"
    );
}
