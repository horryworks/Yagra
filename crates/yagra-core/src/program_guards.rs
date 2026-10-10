// SPDX-License-Identifier: AGPL-3.0-only
//! **The checks that speak for the whole program**, not for one crate (ADR-202 Inc.5).
//!
//! Each of these reads source text and makes a claim of the form "nowhere in Yagra-core is X
//! written". When the database layer left core for `yagra-base`, every such check kept compiling
//! and quietly stopped seeing a third of the code it was about: `env!("CARGO_MANIFEST_DIR")`
//! expands to the crate a check is written in, so a walk of "this crate" is a walk of one tree.
//! They are gathered here and read **every** tree — core's, `yagra-base`'s and `yagra-netbox`'s — through
//! [`crate::module_source::program_src_dirs`] and [`crate::module_source::program_code`], which
//! name a split-out crate's files with its name as a prefix.
//!
//! A check about one module of `yagra-base` stays in that crate and reads through its own
//! `module_source`. The rule for which side a check goes on: does its claim name the program, or
//! one module?
//!
//! ⚠️ One check was deleted rather than moved: `repo_imports_no_domain_module` searched `repo/` for
//! an import of a module above it. `yagra-base` cannot depend on `yagra-core`, so that import no
//! longer compiles — the offence is unspellable, and a grep for it would only be a second, weaker
//! copy of the compiler (`testing.md`).

// ---- database tests (ADR-114/116), formerly `pgtest::guards` ----

use yagra_common::srcread::{file_name, read, rs_files};

/// How many database tests must exist for the check below to mean anything.
///
/// Deliberately below the real count: this is a floor against the detector going blind, not a
/// target. Raise it when a whole new area gets covered, not per test.
// Raised from 12 by ADR-115, which took the population from 18 to 79. The floor is about
// the *detector*, not about coverage: it has to be able to tell "nothing is wrong" apart from
// "the needle stopped matching", and a floor left at its first value stops doing that.
const MIN_DATABASE_TESTS: usize = 70;

/// Every `.rs` file of the program — core's and `yagra-base`'s — as `(name, raw text)`.
///
/// Both trees, because a database test is a database test whichever crate holds it: the
/// repositories' own tests went to `yagra-base` with them (ADR-202 Inc.5), and a check left
/// reading core alone would have lost a third of its population without failing.
///
/// 🚨 **Raw**, not [`crate::module_source::code`]. That reader removes every test-only item,
/// which is *all* of the population these checks are about — pointed at it they would search
/// an empty string, find nothing, and pass forever. That is the same failure the reader
/// itself exists to stop, met from the other side.
fn raw_files() -> Vec<(String, String)> {
    let mut paths = Vec::new();
    for src in crate::module_source::program_src_dirs() {
        rs_files(&src, &mut paths);
    }
    paths.iter().map(|p| (file_name(p), read(p))).collect()
}

/// Line numbers of every database test in `text`, paired with whether it carries the mark.
///
/// Both needles are assembled at runtime. Written out as literals they would appear in this
/// file, which is one of the files scanned, and the check would then be reporting on itself
/// (`self-matching-needle-has-two-directions`).
fn database_tests(text: &str) -> Vec<(usize, bool)> {
    let attr = format!("#[{}::test", "sqlx");
    let mark = format!("#[{}", "ignore");
    let lines: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if !line.trim_start().starts_with(&attr) {
            continue;
        }
        // The mark must be the very next line. An attribute anywhere below the macro is
        // re-emitted above the generated `#[test]` and would work, but only the adjacent one
        // is unambiguous to a human reading the test.
        let marked = lines
            .get(i + 1)
            .is_some_and(|l| l.trim_start().starts_with(&mark));
        out.push((i + 1, marked));
    }
    out
}

/// **A database test may not run by default**, so `cargo test` never starts needing a server.
///
/// Without this, one forgotten mark turns a green workspace run on a machine with no
/// PostgreSQL into a failing one, and the failure looks like a broken test rather than a
/// missing attribute.
#[test]
fn every_database_test_is_ignored_by_default() {
    // Acceptance side first, on text that is not on disk: a scanner that has stopped matching
    // is indistinguishable from a clean crate (rejection-only tests pass when everything is
    // rejected).
    let attr = format!("#[{}::test", "sqlx");
    let mark = format!("#[{}", "ignore");
    let sample = format!(
        "{attr}(migrator = \"m\")]\n{mark}]\nasync fn good() {{}}\n{attr}]\nasync fn bad() {{}}\n"
    );
    let found = database_tests(&sample);
    assert_eq!(
        found.len(),
        2,
        "the scanner no longer reads the idiom it exists to find: {found:?}"
    );
    assert_eq!(
        (found[0].1, found[1].1),
        (true, false),
        "the scanner cannot tell a marked test from an unmarked one: {found:?}"
    );

    let files = raw_files();
    assert!(
        files.len() >= 150,
        "only {} files were read under src/; nothing below is being checked",
        files.len()
    );
    let mut total = 0usize;
    let mut offenders: Vec<String> = Vec::new();
    for (name, text) in &files {
        for (line, marked) in database_tests(text) {
            total += 1;
            if !marked {
                offenders.push(format!("{name}:{line}"));
            }
        }
    }
    // 🚨 The floor counts the database tests **found**, not the files walked. The population
    // this rule is about is "somebody wrote a database test"; a detector that stopped seeing
    // them reports a clean crate in exactly the same words as a clean crate.
    assert!(
        total >= MIN_DATABASE_TESTS,
        "only {total} database test(s) were found under src/; the detector has stopped \
         matching and the assertion below is vacuous"
    );
    assert!(
        offenders.is_empty(),
        "{offenders:?} run by default. A database test needs the ignore attribute on the line \
         below its `sqlx::test` attribute — see `crate::pgtest`"
    );
}

/// **The migration macro keeps exactly one call site**, which is what `yagra-base/src/repo/migrate.rs` says.
///
/// Two call sites are two answers to "what does this build embed?" that nothing keeps equal.
/// ADR-114 needed the set from a third place and it would have been very easy to write the
/// path form of the test attribute on every test instead — that spelling expands to the
/// macro, once per test.
#[test]
fn the_migration_macro_has_exactly_one_call_site() {
    let needle = format!("{}::migrate!(", "sqlx");
    assert!(
        format!("static M: Migrator = {needle}\"../../migrations\");").contains(&needle),
        "the needle no longer matches the idiom it exists to find"
    );

    let files = raw_files();
    assert!(
        files.len() >= 150,
        "only {} files were read under src/",
        files.len()
    );
    let sites: Vec<String> = files
        .iter()
        .flat_map(|(name, text)| {
            text.lines()
                .enumerate()
                .filter(|(_, l)| !l.trim_start().starts_with("//") && l.contains(&needle))
                .map(|(i, _)| format!("{name}:{}", i + 1))
                .collect::<Vec<_>>()
        })
        .collect();
    assert_eq!(
        sites.len(),
        1,
        "the migration macro is called from {} place(s): {sites:?}. It must be called once, \
         from `yagra-base/src/repo/migrate.rs`'s `MIGRATIONS` static, and read from there",
        sites.len()
    );
    assert!(
        sites[0].starts_with("migrate.rs:"),
        "the one call site moved out of `yagra-base/src/repo/migrate.rs`: {sites:?}"
    );
}

/// Files holding raw SQL that have no test **in the file**, each with the reason why.
///
/// ⚠️ **Not a backlog.** Each entry is an argument that a test does not belong *here*, not a
/// note that one has yet to be written. The list was two entries when it was created; the third
/// is a crate boundary (ADR-202 Inc.5), not a missing test.
const SQL_WITHOUT_A_TEST_OF_ITS_OWN: [(&str, &str); 3] = [
    (
        "address_owners.rs",
        "`yagra-base`'s address-owner reads. Their tests feed an L3 observation through the          production writer, `L3Repo`, which lives in core and which the database layer cannot          name (ADR-202 Inc.5) — so they are in `yagra-core/src/l3.rs::address_owner_reads`, the          same tests as before the split.",
    ),
    (
        "import_inventory.rs",
        "one `write` taking a transaction, and half of one operation: it hands the four id \
         sets to `import_attached` (ADR-101), so the two are driven together by the round \
         trip in `import.rs`. Testing this half alone would mean building a `Transaction` to \
         ask a question the round trip already answers better.",
    ),
    (
        "import_attached.rs",
        "the other half of the same operation, and the one that cannot run first — it takes \
         what `import_inventory` returns. Same round trip, same file.",
    ),
];

/// **A file with raw SQL has a test, or says why not.**
///
/// ADR-116's rule. Before it, four files held 1,927 lines and sixty-two statements between
/// them with no test at all, and the reason had stopped being true: ADR-114 removed the
/// obstacle and nobody went back.
///
/// 🚨 **What this can and cannot say.** It can say the file has a test. It cannot say the SQL
/// *runs* — a file full of statements and one unrelated unit test passes here. That question
/// needs the statements a server actually executed, which is `scripts/sql-coverage.sh`, and it
/// is deliberately not a gate: standing a server up costs more than a check earns, and its
/// `unknown` bucket means the number cannot reach zero. Measured 2026-09-01, the two answers
/// disagree in **both** directions — `arp.rs`, `neighbors.rs` and `topology_links.rs` each
/// passed this check and ran none of their SQL, while `config_bundle/export.rs` fails it and
/// runs all seventeen of its statements through `api/config_bundle.rs`.
///
/// Raw text, like its two neighbours: the tests it looks for are exactly what
/// [`crate::module_source`] removes.
#[test]
fn every_file_with_raw_sql_has_a_test_of_its_own() {
    // Assembled, not written out: this file holds SQL of its own (`pgtest::rows`), so it is in
    // the population, and a literal needle would also match the line declaring it.
    let sql = format!("{}::query", "sqlx");
    let any_test = |text: &str| {
        [
            format!("#[{}]", "test"),
            format!("#[{}::test", "tokio"),
            format!("#[{}::test", "sqlx"),
        ]
        .iter()
        .any(|n| text.contains(n.as_str()))
    };

    // Acceptance side first, on text that is not on disk: a detector that has stopped matching
    // reports a clean crate in the same words as a clean crate.
    assert!(
        !any_test(&format!("fn f() {{ {sql}(\"SELECT 1\"); }}")),
        "the detector no longer recognises a file with SQL and no test"
    );
    assert!(
        any_test(&format!(
            "fn f() {{ {sql}(\"SELECT 1\"); }}\n#[{}]\nfn t() {{}}",
            "test"
        )),
        "the detector no longer recognises a test"
    );

    let with_sql: Vec<(String, String)> = raw_files()
        .into_iter()
        .filter(|(_, text)| text.contains(sql.as_str()))
        .collect();
    // 🚨 The floor counts the files that **survived the filter**, which is the set the loop
    // below walks. Counting `raw_files()` instead would leave the assertion vacuous the moment
    // the needle stopped matching (`floor-must-count-what-was-checked`).
    assert!(
        with_sql.len() >= 45,
        "only {} file(s) under src/ were found to hold SQL; the detector has stopped matching \
         and the assertion below is vacuous",
        with_sql.len()
    );

    let offenders: Vec<String> = with_sql
        .iter()
        .filter(|(_, text)| !any_test(text))
        .map(|(name, _)| name.clone())
        .collect();
    let exempt: Vec<&str> = SQL_WITHOUT_A_TEST_OF_ITS_OWN
        .iter()
        .map(|(f, _)| *f)
        .collect();

    let unexcused: Vec<&String> = offenders
        .iter()
        .filter(|n| !exempt.contains(&n.as_str()))
        .collect();
    assert!(
        unexcused.is_empty(),
        "{unexcused:?} hold raw SQL and no test. Write one — the database is there since \
         ADR-114 — or add the file to `SQL_WITHOUT_A_TEST_OF_ITS_OWN` with the reason a test \
         does not belong in it"
    );

    // The other direction: an exemption whose file has since gained a test, or moved, is a
    // reason nobody is reading any more.
    let stale: Vec<&str> = exempt
        .iter()
        .filter(|e| !offenders.iter().any(|o| o == *e))
        .copied()
        .collect();
    assert!(
        stale.is_empty(),
        "{stale:?} are excused from having a test and no longer need to be. Remove the entry"
    );
}

// ---- folder-scope predicates (ADR-184 Inc.14), formerly in `repo/guards.rs` ----

/// Files that spell a group-scope predicate on a column by hand, and why each may.
const SCOPE_PREDICATE_COPIES: &[(&str, &str)] = &[
    (
        "yagra-base/repo/mod.rs",
        "`NodeRepo::SCOPE_PREDICATE`, the `$1` form the node listing interpolates as a const",
    ),
    (
        "arp.rs",
        "the module refuses `format!` so every statement stays a literal, and a test holds it to that",
    ),
    (
        "wireless.rs",
        "its predicates sit inside `EXISTS` and `CASE`, where the NULL guard is written one level up",
    ),
    (
        "events/sql.rs",
        "`e.node_id = ANY($9)` is a set of node ids resolved by the caller, not a folder scope",
    ),
];

/// ADR-184: a statement restricts a folder-id column to a scope through
/// `repo::scope_predicate`, or its file says why not. The shape is the one whose two halves each
/// fail silently: drop `IS NULL` and every unrestricted caller sees nothing, drop `ANY` and every
/// scoped caller sees everything.
#[test]
fn every_group_scope_predicate_is_the_helper_or_declared() {
    let files = crate::module_source::program_code();
    assert!(files.len() >= 150, "only {} files were read", files.len());
    let hand = regex::Regex::new(&format!(
        r"(?i)\(\$\d+::uuid\[\] {} OR [\w.]+ = ANY\(\$\d+\)\)",
        "IS NULL"
    ))
    .unwrap();
    let mut offenders = Vec::new();
    let mut declared_seen = 0;
    for (name, code) in &files {
        let found = hand.find_iter(code).count();
        if found == 0 {
            continue;
        }
        if SCOPE_PREDICATE_COPIES.iter().any(|(f, _)| f == name) {
            declared_seen += 1;
        } else {
            offenders.push(format!("{name}: {found}"));
        }
    }
    assert!(
        offenders.is_empty(),
        "{offenders:?} spell a group-scope predicate by hand — use `repo::scope_predicate`, or add \
         the file to SCOPE_PREDICATE_COPIES with the reason"
    );
    assert_eq!(
        declared_seen,
        SCOPE_PREDICATE_COPIES.len(),
        "a declared file no longer holds a hand-written predicate; take it off the list"
    );
    let calls: usize = files
        .iter()
        .map(|(_, c)| c.matches(&format!("{}(", "scope_predicate")).count())
        .sum();
    assert!(calls >= 13, "only {calls} statements ask scope_predicate");
}

// ---- sealed columns (ADR-184 Inc.9), formerly in `sealed_row.rs` ----

/// The files allowed to name a sealed column in Rust rather than in SQL. `yagra-base/secrets.rs` owns
/// `SEALED_TABLES` and its counting query, which reads through this module like everyone else.
const READS_BY_HAND: &[&str] = &["yagra-base/sealed_row.rs"];

/// ADR-184: nobody else reads or binds the five columns field by field.
///
/// Three shapes, built at run time so this file's own production text is the only literal: a
/// column read by name (`"key_id")`), a `SealedSecret` assembled by hand, and a field of one
/// handed to `bind`. SQL text names the columns too, but never in any of these shapes.
#[test]
fn no_module_reads_a_sealed_column_by_hand() {
    let files = crate::module_source::program_code();
    assert!(files.len() >= 150, "only {} files were read", files.len());
    let needles = [
        format!("\"{}\")", "key_id"),
        format!("\"{}\")", "wrapped_dek"),
        format!("\"{}\")", "ct_nonce"),
        format!("{} {{", "SealedSecret"),
        format!(".{})", "wrapped_dek"),
        format!(".{}.clone()", "wrapped_dek"),
    ];
    let mut offenders = Vec::new();
    for (name, code) in &files {
        if READS_BY_HAND.contains(&name.as_str()) {
            continue;
        }
        for needle in &needles {
            if code.contains(needle.as_str()) {
                offenders.push(format!("{name}: {needle}"));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "{offenders:?} read or bind the sealed columns by hand. Use `sealed_row::sealed_from_row` \
         and `BindSealed` — the last hand-written reader asked for the wrong key_id width and \
         silently disabled notifications"
    );
    // The floor: the callers this was written for were found using it.
    let callers = files
        .iter()
        .filter(|(_, code)| {
            code.contains(&format!("{}(", "sealed_from_row"))
                || code.contains(&format!("{}(", "sealed_from_row_opt"))
                || code.contains(&format!(".{}(", "bind_sealed"))
                || code.contains(&format!(".{}(", "bind_sealed_opt"))
        })
        .count();
    assert!(
        callers >= 9,
        "only {callers} files store a sealed secret through this module"
    );
}

// ---- token tables (ADR-184 Inc.11), formerly in `stored_enum.rs` ----

/// Parsers that match literal tokens by hand, with why each is not a `token_enum!`.
const HAND_WRITTEN: &[(&str, &str)] = &[
    (
        "events/mod.rs",
        "`EventStatGroup::parse` reads a query parameter one way; it has no stored token and no `as_str`",
    ),
    (
        "yagra-netbox/lib.rs",
        "`NetboxField` has a `Custom(String)` variant, so it is not a fieldless enum",
    ),
    (
        "api/flow.rs",
        "`FlowAgg::parse` accepts two spellings of three kinds (`talkers` and `top-talkers`)",
    ),
    (
        "yagra-base/secrets.rs",
        "`parse` validates a stored JSON document; the matched strings are a field's values",
    ),
];

/// ADR-184: a parser whose body is a `"token" =>` table is the second copy of an `as_str` —
/// the macro writes both halves from one list. Needles are built at run time; the floor counts
/// the macro's users, which only grows.
#[test]
fn no_enum_hand_writes_its_token_table() {
    let files = crate::module_source::program_code();
    assert!(files.len() >= 150, "only {} files were read", files.len());
    let parser = regex::Regex::new(&format!(
        r"fn (?:{}|from_str|from_token|from_stored)\([^)]*\)[^{{]*\{{",
        "parse"
    ))
    .unwrap();
    let arm = regex::Regex::new(r#""[a-z0-9_]+" => "#).unwrap();
    let mut offenders = Vec::new();
    let mut uses = 0;
    for (name, code) in &files {
        uses += code.matches(&format!("{}!(", "token_enum")).count();
        if HAND_WRITTEN.iter().any(|(f, _)| f == name) || name == "yagra-base/stored_enum.rs" {
            continue;
        }
        for m in parser.find_iter(code) {
            // The body: from the opening brace to its match.
            let mut depth = 0usize;
            let mut end = code.len();
            for (i, ch) in code[m.end() - 1..].char_indices() {
                match ch {
                    '{' => depth += 1,
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            end = m.end() - 1 + i;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            if arm.is_match(&code[m.end()..end]) {
                offenders.push(format!("{name}: {}", m.as_str()));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "{offenders:?} spell a token table by hand beside an `as_str` — declare the enum with \
         `token_enum!` so the two directions come from one list"
    );
    assert!(uses >= 22, "only {uses} `token_enum!` users found");
}

// ---- tree-table inserts (ADR-162), formerly in `groups.rs` ----

/// **Every `INSERT` into the two tree tables names `sort_order`.**
///
/// 🚨 This is the class ADR-162 could not afford to leave to review. A scope is one ordering
/// sequence now, so a row inserted at the column's `DEFAULT 0` sits above everything in it —
/// and a row appended with a `MAX` taken over one of the two tables lands on a value the other
/// table is already using. A tie is not an error: `placement_orders` divides the gap between
/// two neighbours, and when they are equal every midpoint collapses onto the same value. **The
/// write returns 204 and the row does not move.** There is nothing on screen to read, and the
/// operator concludes the drag is broken.
///
/// Four inserts were sitting at `DEFAULT 0` when this was written (two Meraki folders, the
/// Meraki device node, the imported AP) and were invisible for as long as the renderer kept
/// the two lists apart.
///
/// ⚠️ **It cannot see the other half**: an append that names `sort_order` but computes it over
/// one table passes this. That half is `yagra_base::groups::append_base_sql` having one call shape and being read
/// by a person. What this closes is the forgotten column.
///
/// Read through `srcread`, so test fixtures and this test's own needles are not in the text,
/// and with whole-line comments dropped — `config_bundle/guards.rs` describes an
/// `INSERT INTO nodes` in its own module doc.
#[test]
fn every_insert_into_a_tree_table_names_sort_order() {
    use yagra_common::srcread as sr;

    let mut paths = Vec::new();
    for src in crate::module_source::program_src_dirs() {
        sr::rs_files(&src, &mut paths);
    }
    assert!(
        paths.len() >= 150,
        "only {} source files walked; the program is larger than that",
        paths.len()
    );
    let mut inspected = 0usize;
    let mut offenders: Vec<String> = Vec::new();
    for path in &paths {
        let name = sr::file_name(path);
        let code = sr::strip_and_check(&name, &sr::read(path))
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        // Built at run time: this file is test-only and sits in the walk, so a literal needle
        // would match its own line (it did, the day the check moved here).
        for table in [
            format!("INSERT INTO {} ", "nodes"),
            format!("INSERT INTO {} ", "node_groups"),
        ] {
            let mut from = 0usize;
            while let Some(hit) = code[from..].find(table.as_str()) {
                let at = from + hit;
                // The column list runs to the first `)` after the table name. Slicing to it
                // rather than taking a fixed window keeps the check stable under `cargo fmt`,
                // which decides for itself where the string literal's continuations fall.
                let end = code[at..].find(')').map_or(code.len(), |o| at + o);
                let cols = &code[at..end];
                inspected += 1;
                if !cols.contains("sort_order") {
                    offenders.push(format!(
                        "{name}: {}",
                        cols.split_whitespace().collect::<Vec<_>>().join(" ")
                    ));
                }
                from = end.max(at + table.len());
            }
        }
    }
    assert!(
        inspected >= 8,
        "only {inspected} tree-table inserts inspected — the needle stopped matching, which \
         reads exactly like a codebase where every insert is correct"
    );
    assert!(
        offenders.is_empty(),
        "these INSERT into a tree table without naming `sort_order`, so the row lands at the \
         column's DEFAULT 0 and sits above everything in its scope (ADR-162):\n  {}",
        offenders.join("\n  ")
    );
}

// ---- outbound HTTP clients (ADR-184 Inc.2), formerly in `http.rs` ----

/// Files that build a client, with why each one is allowed to. Everything else in the program
/// goes through `yagra_base::http::builder` or `client`.
const BUILDS_ITS_OWN: &[&str] = &["yagra-base/http.rs"];

/// ADR-184: nobody else in the program builds an outbound client.
///
/// The spellings searched for are the three ways `reqwest` offers one. They are put together at
/// run time so that this file's own production text — which has to contain one of them — is
/// the only place a literal exists.
#[test]
fn every_outbound_client_is_built_here() {
    let files = crate::module_source::program_code();
    assert!(files.len() >= 150, "only {} files were read", files.len());
    let spellings = [
        format!("{}::builder()", "Client"),
        format!("{}::new()", "reqwest::Client"),
        format!("{}::new()", "ClientBuilder"),
    ];
    let offenders: Vec<String> = files
        .iter()
        .filter(|(name, _)| !BUILDS_ITS_OWN.contains(&name.as_str()))
        .flat_map(|(name, code)| {
            spellings
                .iter()
                .filter(|s| code.contains(s.as_str()))
                .map(move |s| format!("{name}: {s}"))
        })
        .collect();
    assert!(
        offenders.is_empty(),
        "{offenders:?} build an HTTP client of their own. Use `yagra_base::http::client` (or \
         `builder`, to add to it) so the timeout and the redirect policy are decided in the \
         open — the last client built by hand had no timeout at all"
    );
    // The floor: the callers this was written for were found using it, so "no offenders" was
    // measured over the real crate.
    let callers = files
        .iter()
        .filter(|(_, code)| {
            code.contains(&format!("http::{}(", "client"))
                || code.contains(&format!("http::{}(", "builder"))
        })
        .count();
    assert!(
        callers >= 9,
        "only {callers} files build a client through this module"
    );
}

/// The callers whose URL comes from an operator or a third party. Each must refuse redirects.
const NEVER_FOLLOWS: &[&str] = &["alerts/notify.rs", "yagra-netbox/lib.rs", "oidc.rs"];

/// ADR-184: the three callers that must not follow a redirect still say so.
///
/// Since the policy became an argument, relaxing it is a one-word edit that compiles, runs and
/// passes every behavioural test that does not happen to redirect.
#[test]
fn the_no_redirect_callers_still_say_so() {
    let files = crate::module_source::program_code();
    let refuse = format!("Redirects::{}", "None");
    let follow = format!("Redirects::{}", "Follow");
    let mut inspected = 0;
    for name in NEVER_FOLLOWS {
        let (_, code) = files
            .iter()
            .find(|(n, _)| n == name)
            .unwrap_or_else(|| panic!("{name} is gone; NEVER_FOLLOWS names a file that moved"));
        assert!(
            code.contains(&refuse),
            "{name} no longer builds its client with `{refuse}`"
        );
        assert!(
            !code.contains(&follow),
            "{name} builds a client that follows redirects, and its URL is not core's to trust"
        );
        inspected += 1;
    }
    assert_eq!(inspected, NEVER_FOLLOWS.len());
}
