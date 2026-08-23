// SPDX-License-Identifier: AGPL-3.0-only
//! The checks that read this module **as source text** (ADR-095).
//!
//! Things `events/` must hold true that have no type: that the predicate and its binder agree on
//! how many parameters there are, that the events INSERT binds every column it names, and that a
//! query only appears in a file allowed to hold one. Each is answered by reading the module's own
//! source, which is the only technique available — the alternatives need a live PostgreSQL, which
//! is exactly what these files are the adapter for.
//!
//! Declared `#[cfg(test)] mod guards;` in [`super`], which is how [`crate::module_source`]'s
//! exclusion derives it — a scan that lives in the text it scans matches its own literals. ADR-086
//! hit that within minutes of splitting `mcp/tools.rs`, so the mechanism has been in place since.

use std::collections::BTreeSet;

use super::{EVENT_FILTER_BINDS, EVENT_FILTER_WHERE};

/// The whole module's source, read the one way this repository reads source (ADR-091).
///
/// The **directory**, not a file: both needles below name functions that could reasonably move
/// between `sql.rs` and `repo.rs` later, and reading the concatenation means such a move needs no
/// edit here. `include_str!` cannot express it — it takes a literal path, and there is no literal
/// that means "every file of this module", so a reader left on one file would search a fraction and
/// report success. That is the failure ADR-086 measured (one guard checked 10 tools of 36) and the
/// reason [`crate::module_source`] exists.
fn source() -> String {
    crate::module_source::code("src", "events")
}

/// The predicate and its binder must agree on how many parameters there are.
///
/// A mismatch is not a compile error: `sqlx` discovers it at query time, so the first symptom is
/// every event search failing against a live database. Both halves are checked — that the predicate
/// mentions each parameter the constant promises, and that the binder supplies exactly that many.
///
/// Needles are built at runtime. A literal `$16` written out in this file would also match itself
/// in the source scan, and a literal `.bind(` would match this very comment.
#[test]
fn the_where_clause_binds_every_parameter_it_names() {
    for n in 1..=EVENT_FILTER_BINDS {
        assert!(
            EVENT_FILTER_WHERE.contains(&format!("${n}")),
            "the predicate never mentions ${n}, so EVENT_FILTER_BINDS is too high"
        );
    }
    assert!(
        !EVENT_FILTER_WHERE.contains(&format!("${}", EVENT_FILTER_BINDS + 1)),
        "the predicate reaches past EVENT_FILTER_BINDS, which is what every consumer's own \
         trailing bind is derived from — it would collide with the page size"
    );

    let src = source();
    let body = src
        .split("fn bind_event_filter<'q>(")
        .nth(1)
        .expect("bind_event_filter is somewhere in events/");
    let body = &body[..body.find("\n}\n").expect("the function ends")];
    let binds = body.matches(&format!(".{}(", "bind")).count();
    assert_eq!(
        binds, EVENT_FILTER_BINDS,
        "the predicate names {EVENT_FILTER_BINDS} parameters and the binder supplies {binds}"
    );
}

/// The events INSERT names its columns in one string and binds them positionally elsewhere;
/// nothing makes the two agree.
///
/// A mismatch is not a compile error and not a crash: the batch insert fails, `flush_persist`
/// logs a warning and moves on, and the deployment quietly stops persisting **every** event
/// while continuing to alert normally. There is no integration test that runs a real insert, so
/// this count is the only thing between that bug and the test server.
#[test]
fn the_events_insert_binds_every_column_it_names() {
    let src = source();
    // Scoped to the function body — which is why plain needles are safe here, unlike a
    // whole-file scan where this test's own text would match itself.
    let start = src
        .find("pub async fn insert_events_batch")
        .expect("insert_events_batch moved or was renamed");
    let rest = &src[start + 1..];
    let end = rest
        .find("\n    pub async fn ")
        .expect("no following method — the scan window is unbounded");
    let body = &rest[..end];

    let marker = "INSERT INTO events (";
    let cols_at = body
        .find(marker)
        .expect("the INSERT moved out of this function")
        + marker.len();
    let cols_end = cols_at + body[cols_at..].find(')').expect("unterminated column list");
    let columns = body[cols_at..cols_end].split(',').count();
    let binds = body.matches("push_bind(").count();
    assert_eq!(
        columns, binds,
        "the INSERT names {columns} columns but binds {binds} values"
    );
}

/// Which tables each production file of `events/` may name in SQL — the split's rule, as data.
///
/// Hand-maintained, and that is tolerable for the same reason `retention.rs::PRUNE_SITES` is:
/// **it falls the safe way.** A table a file uses but has not declared fails the test naming both;
/// a declaration nothing uses is invisible. The dangerous direction is the one that cannot happen.
///
/// Both directions against the directory are checked below, so a new file cannot arrive unlisted
/// and a renamed one cannot leave a stale row behind.
const TABLE_OWNERSHIP: &[(&str, &[&str])] = &[
    // The vocabulary. `parse_severity` and `event_kind_from_stored` turn stored strings into enums;
    // neither issues a query.
    ("mod.rs", &[]),
    // The predicate every event listing and aggregate is built on. `nodes` is the display-name
    // LEFT JOIN the free-text search reaches into — searching a node's name is part of the contract
    // (`logstore.rs` implements the same reach in LogsQL), not a second file's worth of inventory.
    ("sql.rs", &["events", "nodes"]),
    ("repo.rs", &["events", "event_rules", "event_sources"]),
    // A stored rule becomes a matcher. Reading the rules is `repo.rs`'s job.
    ("rules.rs", &[]),
    // 🚨 The two that matter. Both are hot paths — the engine runs per message and the writers run
    // per batch — and a synchronous query added to either would read fine, pass review, and be
    // found in production under load. This is what the check is for.
    ("engine.rs", &[]),
    ("ingest.rs", &[]),
];

/// **A statement may only name a table its file has declared** — the rule the split was cut on.
///
/// A rule written only in `mod.rs`'s prose is a rule that holds until the next person adds a
/// function next to the one they were reading, so it is here as a test instead (ADR-094 established
/// the shape on `repo/`). What it buys here that it did not there: `engine.rs` and `ingest.rs`
/// declare **no** tables, so store discipline in the two hot paths is a build failure rather than a
/// convention.
///
/// The two filters live in [`crate::sql_tables`] — a name counts only in SQL position *and* only if
/// it is a real table derived from `migrations/`, and neither is sufficient alone.
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
    let files = crate::module_source::files(&crate::module_source::roots("src", "events"));

    // Both directions against the directory. `files` already omits the test-only modules `mod.rs`
    // declares, so this file and `testkit.rs` are not expected to appear — which is also why this
    // file's own literals cannot match.
    let present: BTreeSet<&str> = files.iter().map(|(name, _)| name.as_str()).collect();
    for name in &present {
        assert!(
            declared.contains_key(name),
            "events/{name} is not in TABLE_OWNERSHIP, so nothing checks what it reads or writes"
        );
    }
    for (name, _) in TABLE_OWNERSHIP {
        assert!(
            present.contains(name),
            "TABLE_OWNERSHIP names events/{name}, which no longer exists"
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
                    "events/{name} names `{table}`, which it does not declare. Either the code \
                     belongs in the file for `{table}`, or `{table}` belongs in this file's \
                     TABLE_OWNERSHIP row with a comment saying why"
                ));
            }
        }
    }
    // Reported all at once: one function in the wrong file usually brings several statements with
    // it, and one failure per run would mean one round trip each.
    wrong.sort();
    wrong.dedup();
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    assert!(
        checked >= 25,
        "only {checked} statements were inspected; the scan has stopped matching and would report \
         a module full of misplaced queries as clean"
    );
}
