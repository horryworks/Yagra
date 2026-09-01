// SPDX-License-Identifier: AGPL-3.0-only
//! The checks that read this module **as source text** (ADR-094).
//!
//! Things `repo/` must hold true that have no type. Each is answered by reading the module's own
//! source.
//!
//! ⚠️ **This doc used to say that was the only technique available, because the alternative needed
//! a live PostgreSQL.** Since ADR-114 the suite has one. These checks stay anyway, because they do
//! not answer the same question: which table a statement *names*, and which constant an expression
//! *clamps to*, are facts about the text. Running the statement cannot see either — it sees the
//! answer the statement gave, which is the same answer a second, wrong implementation would give
//! on the fixture in front of it. Where a behavioural test **is** stronger, prefer it: the
//! interface upsert's own tests now execute what `the_upsert_writes_every_column_it_inserts`
//! could only read.
//!
//! Declared `#[cfg(test)] mod guards;` in [`super`], which is how [`crate::module_source`]'s
//! exclusion derives it — a scan that lives in the text it scans matches its own literals. ADR-086
//! hit that within minutes of splitting `mcp/tools.rs`, so the mechanism has been in place since.

use std::collections::BTreeSet;

/// The search cap is one number, and no implementation may re-clamp to its own.
///
/// A source-reading test, and still the right one after ADR-114 gave the suite a database. The
/// regression it exists for was the API edge clamping to 500 and documenting that as the maximum
/// while `NodeRepo::search` re-clamped to 100 — so filtering a large fleet silently returned 100
/// rows. A behavioural test sees that only with more than 100 nodes in the fixture *and* a caller
/// asking for more; what is actually wrong is that a second number exists at all, and that is a
/// fact about the text. (`listing.rs` now also runs the query, which is the complementary half.)
/// The needle is built at runtime; a literal written out in this file would match itself and fail
/// forever (testing.md).
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
    // The description of a pool, and - deliberately - the three tables a refused delete has to
    // name. Counting what still points at a pool is part of that refusal, not a second answer to
    // "who is in this pool": those live in nodes.rs and crate::pollers and stay there.
    ("pools.rs", &["pools", "nodes", "node_groups", "pollers"]),
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

/// **A statement may only name a table its file has declared** — the rule the split was cut on.
///
/// Placement in this module is decided by the table a method's SQL names, not by what the method is
/// called (two of them disagreed: `suppression_opt_outs` and its setter sat among the deployment
/// settings and read a per-node column). A rule written only in `mod.rs`'s prose is a rule that
/// holds until the next person adds a method next to the one they were reading, so it is here as a
/// test instead.
///
/// **The two filters live in [`crate::sql_tables`]** — a name counts only in SQL position *and* only if it
/// is a real table, and neither is sufficient alone. They moved there when `events/guards.rs` needed
/// the same pair (ADR-095): a rule written twice is a rule that rots on one side, which is the
/// lesson ADR-091 paid twenty-three times for. What stays here is what is about `repo/` — the
/// ownership table above, and both floors below.
///
/// 🚨 **Two floors, because this is a check that reports "nothing wrong" when it sees nothing.**
/// One on the vocabulary and one on the statements actually inspected — and the second counts the
/// sites that survived both filters, not the files gathered. Counting the wrong set is the mistake
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

    let mut checked = 0usize;
    let mut wrong: Vec<String> = Vec::new();
    for (name, text) in &files {
        let allowed = declared[name.as_str()];
        for table in crate::sql_tables::references(text, &vocab) {
            checked += 1;
            if !allowed.contains(&table.as_str()) {
                wrong.push(format!(
                    "repo/{name} names `{table}`, which it does not declare. Either \
                     the method belongs in the file for `{table}`, or `{table}` belongs in \
                     this file's TABLE_OWNERSHIP row with a comment saying why"
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

/// **No polling interval that can skip a write leaves a row old enough to look stale**
/// (ADR-110 Increment 1).
///
/// This is the entire safety argument for not writing a row whose values did not change, and it is
/// enumerated rather than asserted as an inequality — partly because every interval is checked
/// rather than the boundary alone, and partly because a bare `assert!` over two constants folds to
/// `assert!(true)` and is optimised out, which clippy rightly refuses.
///
/// A row is skipped only while it is newer than `INTERFACE_TOUCH_SECS`, which can only happen on a
/// node polled *faster* than that window; a node polled more slowly finds its row already older on
/// every poll and is written exactly as before, so it cannot regress. The oldest such a row can get
/// is the window plus one more interval — it is written by the first poll that finds it old
/// enough. The staleness flag must not fire below that, or the fleet's Interfaces tabs would fill
/// with "no longer reported" for ports that are answering perfectly.
///
/// ⚠️ **Nothing else catches a widened touch window.** Raising `INTERFACE_TOUCH_SECS` to 450 keeps
/// every other test in the workspace green — including the database tests ADR-114 added, which
/// exercise the boundary relative to whatever the constant currently says rather than against
/// `INTERFACE_STALE_SECS`. This arithmetic is the only thing holding the two numbers together.
#[test]
fn no_skippable_interval_leaves_a_row_old_enough_to_look_stale() {
    use super::interfaces::{INTERFACE_STALE_SECS, INTERFACE_TOUCH_SECS};
    let mut checked = 0usize;
    let mut worst = (0i64, 0i64);
    // Exactly the cadences at which a skip is possible. 🚨 The bound is **inclusive**, and that one
    // character is the whole case: the predicate skips while the row is *at most* the window old,
    // so a node polled at exactly the window skips at age TOUCH and is written at 2*TOUCH — the
    // worst age there is. An exclusive bound stops one short of it and happily passes a 450s window
    // against a 900s threshold, which is precisely the widening this check exists to refuse. It was
    // written exclusive first, and only breaking the check on purpose found it.
    for interval in 1..=INTERFACE_TOUCH_SECS {
        // The first poll whose multiple of the interval clears the window is the one that writes,
        // so this is the exact oldest a row on this cadence can be observed, not an estimate.
        let worst_age = (INTERFACE_TOUCH_SECS / interval + 1) * interval;
        assert!(
            worst_age < INTERFACE_STALE_SECS,
            "a node polled every {interval}s can leave a row {worst_age}s old, and the staleness \
             flag fires at {INTERFACE_STALE_SECS}s: a live port would be reported as stale on both \
             the Interfaces tab and get_node_status"
        );
        if worst_age > worst.1 {
            worst = (interval, worst_age);
        }
        checked += 1;
    }
    // Cadences above the window skip nothing — the first poll already finds the row old enough —
    // so they behave exactly as they did before this existed and need no bound here.

    // The floors. A zero or negative window makes that loop empty, and an empty loop is
    // indistinguishable from a proof: it would report this check as passing while every row was
    // written on every poll again.
    assert!(
        checked > 0,
        "no cadence was examined, so nothing was proved"
    );
    assert_eq!(
        checked, INTERFACE_TOUCH_SECS as usize,
        "the touch window is {INTERFACE_TOUCH_SECS}, so this check examined {checked} cadences"
    );
    assert_eq!(
        worst,
        (INTERFACE_TOUCH_SECS, 2 * INTERFACE_TOUCH_SECS),
        "the worst cadence is no longer the one polled at exactly the window, so the reasoning in \
         this test's doc has stopped describing what the loop measures"
    );
}

/// **Every column the interface upsert inserts is also a column it updates and compares.**
///
/// The statement is generated from `interfaces::VALUE_COLUMNS`, so the `SET` list and the change
/// predicate cannot disagree with each other. What they *can* disagree with is the INSERT column
/// list, which is a literal because each column needs its own `unnest` cast. A column added there
/// and forgotten in `VALUE_COLUMNS` would be written once, on the row's first insert, and **never
/// updated again** — the device would report a new speed or a new alias forever and Yagra would
/// keep the original, with no compile error and no runtime error. ⚠️ ADR-114's database tests do
/// not close this either: they exercise the columns they name, and the whole failure is a column
/// nobody named.
///
/// It reads the **generated statement**, not the source text: that is the thing that actually runs,
/// and it makes the check immune to how the SQL happens to be spelled. Needles are assembled at
/// runtime for the usual reason — a literal `IS DISTINCT FROM` written out here would match this
/// file's own text if the check ever moved to a source scan (testing.md).
///
/// 🚨 Floors on both ends: the column count and the placeholder count. This is a check whose
/// healthy answer is "found nothing wrong", so it has to be able to tell that apart from "parsed
/// nothing".
#[test]
fn every_inserted_column_is_updated_and_compared() {
    use super::interfaces::{UPSERT_SQL, VALUE_COLUMNS};
    let sql: &str = &UPSERT_SQL;

    // The INSERT column list, verbatim from the statement.
    let open = sql
        .find("INSERT INTO interfaces (")
        .expect("the statement inserts into interfaces");
    let list_start = open + "INSERT INTO interfaces (".len();
    let list_end = list_start
        + sql[list_start..]
            .find(')')
            .expect("the INSERT column list is closed");
    let inserted: Vec<&str> = sql[list_start..list_end]
        .split(',')
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .collect();
    // The key is not a value, and the clock is not something a device reports.
    let key_or_clock = ["node_id", "ifindex", "last_seen"];
    let values: Vec<&str> = inserted
        .iter()
        .copied()
        .filter(|c| !key_or_clock.contains(c))
        .collect();
    assert_eq!(
        inserted.len(),
        values.len() + key_or_clock.len(),
        "the INSERT list {inserted:?} no longer names all of {key_or_clock:?}, so the split \
         between the row's key, its clock and its device-supplied values has moved"
    );
    assert_eq!(
        values,
        VALUE_COLUMNS.to_vec(),
        "the statement inserts {values:?} but VALUE_COLUMNS is {:?}. A column in the first list \
         and not the second is inserted once and never updated again",
        VALUE_COLUMNS
    );
    assert_eq!(
        VALUE_COLUMNS.len(),
        11,
        "the floor moved: {} device-supplied columns were compared, and a parse that stopped \
         matching would report the same clean result as a correct statement",
        VALUE_COLUMNS.len()
    );

    // Each of them is COALESCEd into the row and named on both sides of the change predicate.
    let coalesce = |c: &str| format!("{c} = COALESCE(EXCLUDED.{c}, interfaces.{c})");
    let compare = |c: &str| format!("interfaces.{c} IS DISTINCT FROM EXCLUDED.{c}");
    let present = |c: &str| format!("EXCLUDED.{c} IS NOT NULL");
    for c in VALUE_COLUMNS {
        assert!(
            sql.contains(&coalesce(c)),
            "{c} is not COALESCEd into the row"
        );
        assert!(
            sql.contains(&compare(c)),
            "{c} is never compared, so a change to it is discarded"
        );
        assert!(
            sql.contains(&present(c)),
            "{c} is compared without the IS NOT NULL half, so a walk that stopped reporting it \
             would count as a change and rewrite every row forever"
        );
    }

    // The clock is written unconditionally *inside* the SET, and gated *outside* it by the touch
    // window. Losing the second half puts the statement back to rewriting every polled row.
    assert!(sql.contains("last_seen = now()"));
    assert!(
        sql.contains("interfaces.last_seen < now() - interval '1 second' * $14::float8"),
        "the lazy touch is gone, so every polled row is written again on every poll"
    );

    // Fourteen placeholders, each used exactly once, and the touch window is the last — because the
    // binds are positional and a mismatch between the two lists writes one column's values into
    // another with no error of any kind.
    for i in 1..=14 {
        let needle = format!("${i}:");
        assert_eq!(
            sql.matches(&needle).count(),
            1,
            "placeholder ${i} is used {} times; the bind list is positional",
            sql.matches(&needle).count()
        );
    }
    assert!(
        !sql.contains("$15"),
        "a fifteenth placeholder has no bind behind it"
    );
}
