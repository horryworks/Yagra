// SPDX-License-Identifier: AGPL-3.0-only
//! **Which tables this deployment has, and which of them a piece of source names** (ADR-095).
//!
//! A companion to [`crate::module_source`], and deliberately not part of it: that module answers
//! "what is this module's own source text", this one answers "what does `migrations/` declare".
//! Two different subjects with two different failure modes, so folding them together would put a
//! schema question behind a name that promises a source question.
//!
//! It exists as a module rather than as a helper inside one caller because there are now two
//! callers — `repo/guards.rs` (ADR-094) and `events/guards.rs` (ADR-095) — and the rule they share
//! is the one thing that must not be written twice. ADR-091 is the precedent: the "production
//! source" rule was hand-copied into twenty-three files and every copy carried the same defect.
//!
//! **No floor lives here.** How many tables *should* be found, and how many statements *should* be
//! inspected, are facts about the caller's module, not about this mechanism — the same reasoning
//! `module_source`'s doc gives for keeping its floors caller-side. What this module owes its callers
//! is a number they can put a floor on, which is why [`references`] returns every hit rather than a
//! deduplicated set.

use std::collections::BTreeSet;
use std::path::Path;

/// Every table this deployment has, derived from `migrations/` rather than written down.
///
/// Derived because a hand-written vocabulary would be the very thing the checks above this exist to
/// stop: a list that falls behind the schema, quietly narrowing what a scan can even see. A table
/// missing from here is a table the scan skips — so a caller must put a floor on this set's size.
pub(crate) fn vocabulary() -> BTreeSet<String> {
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
    // sqlx creates this one itself, so no migration declares it — and `repo/migrate.rs` reads it.
    out.insert("_sqlx_migrations".to_owned());
    out
}

/// Every table `code` names in SQL position, **in order, with repeats**.
///
/// **Two filters, and neither is sufficient alone.** A name counts only when it appears in SQL
/// position (after `FROM` / `INTO` / `UPDATE` / `JOIN`) *and* is in `vocab`. Position alone matches
/// `FROM unnest($1::uuid[])` and `extract(epoch FROM last_seen)`; vocabulary alone matches `nodes`
/// inside `list_nodes` and a dozen other identifiers. Both were measured on `repo/` (ADR-094).
///
/// Whole-line comments come out first — prose explaining a query would otherwise read as one.
/// Repeats are kept because the caller's floor counts *statements inspected*, and a module with one
/// table and forty statements must not read as a module with one statement.
pub(crate) fn references(code: &str, vocab: &BTreeSet<String>) -> Vec<String> {
    let sql = regex::Regex::new(r"\b(?:FROM|INTO|UPDATE|JOIN)\s+([a-z_][a-z0-9_]*)")
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
