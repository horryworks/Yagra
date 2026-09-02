// SPDX-License-Identifier: AGPL-3.0-only
//! **Tests that run against a real PostgreSQL** — the convention, the fixtures, and the checks
//! that keep the convention honest (ADR-114).
//!
//! ## Why this exists
//!
//! ADR-111, ADR-112 and ADR-113 each stopped at the same sentence: the remaining lines are SQL,
//! and the only way to check SQL is to run it. Cutting more seams does not reach them.
//! `#[sqlx::test]` does: it creates a throwaway database per test, migrates it, and hands the
//! body a [`sqlx::PgPool`].
//!
//! ⚠️ **The number that used to be here has moved, so it is stated as a date.** At ADR-114 it was
//! ~3,950 production lines of untested SQL; ADR-114 took it to 3,566 across ten files, ADR-115 to
//! 2,111 across five, and ADR-116 to **189 lines in one file** — `examples/seed_nodes.rs`, a
//! load-test rig outside `src/`. Every production file holding SQL now has a test or an entry in
//! `guards::SQL_WITHOUT_A_TEST_OF_ITS_OWN` saying why not.
//!
//! 🚨 **"The file has a test" is not "the SQL runs", and the two disagree in both directions.**
//! Measured on 2026-09-01 with `scripts/sql-coverage.sh` — a throwaway server with
//! `log_statement=all`, matched against the literals in the source — the workspace's 474
//! statements went from **143 executed / 242 never executed / 89 unresolvable** to
//! **186 / 199 / 89**. `arp.rs`, `neighbors.rs` and `topology_links.rs` each carried tests and ran
//! **none** of their SQL, while `config_bundle/export.rs` carried none and ran all seventeen of
//! its statements through `api/config_bundle.rs`. The guard answers the cheap question; that
//! script answers the real one, and is deliberately not a gate.
//!
//! Those three ran their SQL for the first time on 2026-09-02, and the third of them found a
//! shipped defect on the first run: all three of `arp.rs`'s address projections read an `inet`
//! with an explicit cast to text, which renders the netmask (`10.0.0.1/32`) and does not parse —
//! so every discovered endpoint listed as `0.0.0.0` and every monitored node was reported as
//! unmonitored. Five tests failed; no source-reading check could have said anything about it.
//!
//! Measured again the same day, after `auth.rs` and half of `meraki.rs`: **240 executed / 145
//! never executed / 90 unresolvable**. `auth.rs` is 29/29. The number moves with the suite, so
//! re-derive it rather than quoting this line — the two commands are in `scripts/sql-coverage.sh`.
//!
//! **Say "untested", never "untestable"** — the obstacle was removed in ADR-114.
//!
//! ## The convention — two attributes, in this order
//!
//! ```ignore
//! #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
//! #[ignore = "needs DATABASE_URL"]
//! async fn a_fresh_database_starts_empty(pool: sqlx::PgPool) {
//!     let repo = crate::pgtest::repo(pool);
//!     assert_eq!(repo.list_nodes().await.unwrap().len(), 0);
//! }
//! ```
//!
//! * **`migrator` rather than `migrations = "…"`.** The path form expands to a second
//!   `sqlx::migrate!`, and `repo/migrate.rs` exists to keep that macro to one call site — two
//!   sites are two answers to "what does this build embed?" that nothing keeps equal.
//! * **The mark goes second**, because `#[sqlx::test]` is the attribute macro and everything
//!   after it is what the macro receives and re-emits above the `#[test]` it generates.
//!
//! ## Why a mark and not a cargo feature
//!
//! sqlx offers no third option: with no `DATABASE_URL` its harness **panics** rather than
//! skipping (`sqlx-postgres/src/testing/mod.rs` — `dotenvy::var("DATABASE_URL").expect(…)`), so a
//! plain `cargo test` on a machine with no database would fail. A cargo feature would hide these
//! tests from the default build entirely — including from `clippy --all-targets` — and code that
//! is neither compiled nor linted locally rots without anyone seeing it. The mark keeps them
//! compiled and linted always; only the *running* is opt-in, and CI and `scripts/flash-verify.sh`
//! both opt in on every run.
//!
//! ## Running them
//!
//! ```text
//! docker run -d --name yagra-pg -p 5432:5432 -e POSTGRES_PASSWORD=postgres postgres:17-alpine
//! echo 'DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres' > .env   # gitignored
//! cargo test --workspace -- --include-ignored
//! ```
//!
//! The account must be able to `CREATE DATABASE`; sqlx makes one per test, named from a hash of
//! the test's path, and drops it again **only when the test passed** — a failed test leaves its
//! database behind on purpose, to be inspected. Dropping the `_sqlx_test` schema (and the
//! `_sqlx_test_*` databases it lists) is the tidy-up.

use sqlx::PgPool;
use uuid::Uuid;

/// A [`NodeRepo`](crate::repo::NodeRepo) over the pool the test harness handed us.
///
/// Thin, and deliberately the only wrapper here: every other store already takes a pool
/// (`AlertHistoryStore::new`, `EventRepo::new`, `ReportsRepo::new`, …), so a test builds those
/// directly and there is nothing for this module to add.
#[must_use]
pub fn repo(pool: PgPool) -> crate::repo::NodeRepo {
    crate::repo::NodeRepo::from_pool(pool)
}

/// A folder group, created through the production writer.
///
/// Fixtures here go through the real repository rather than a hand-written `INSERT` on purpose:
/// an insert spelled out in a test is a second copy of the schema that drifts silently, and the
/// table-placement guards (`repo/guards.rs`, `events/guards.rs`) would not see it — they read
/// production text, and this module is test-only.
pub async fn group(pool: &PgPool, name: &str) -> Uuid {
    crate::groups::GroupRepo::new(pool.clone())
        .create(name, crate::groups::GroupType::Site, None, None)
        .await
        .expect("create group")
}

/// A node with an address derived from `n`, optionally in `group`.
pub async fn node(pool: &PgPool, name: &str, n: u8, group: Option<Uuid>) -> Uuid {
    let repo = repo(pool.clone());
    let addr = std::net::IpAddr::from([10, 0, 0, n]);
    let id = repo
        .create_node(name, addr, None, None, None, None, None, None)
        .await
        .expect("create node");
    if let Some(g) = group {
        repo.set_node_group(id, Some(g)).await.expect("set group");
    }
    id
}

/// A device profile, created through the production writer.
///
/// For the tables keyed by `REFERENCES profiles (id)` — `profile_collection_templates` is the one
/// this was written for, where a hand-rolled uuid is refused by the foreign key rather than by
/// anything a test would recognise.
///
/// `generic-snmp` is the column's own default and the category an operator-created profile gets,
/// so a test that does not care about the category is not silently exercising an unusual one.
pub async fn profile(pool: &PgPool, name: &str) -> Uuid {
    repo(pool.clone())
        .create_profile(name, "generic-snmp", None, None)
        .await
        .unwrap_or_else(|e| panic!("create profile {name}: {e}"))
}

/// Rows in `table`. A `count(*)` spelled once rather than in every test that needs one.
pub async fn rows(pool: &PgPool, table: &str) -> i64 {
    // The name is interpolated because a table name cannot be a bind parameter. Every caller is
    // a literal in this crate's own tests, so there is no input to sanitise.
    sqlx::query_scalar::<_, i64>(&format!("SELECT count(*) FROM {table}"))
        .fetch_one(pool)
        .await
        .unwrap_or_else(|e| panic!("count {table}: {e}"))
}

/// The stand-in key-encryption key every sealing store in these tests shares.
///
/// A fixed in-memory key, the same one `api::tests_support` uses and for the same reason: the
/// sealed value round-trips inside one test and nowhere else, so nothing here says anything about
/// a real deployment's KEK handling. Two stores now take one ([`CredentialStore`] and
/// [`crate::notifications::NotificationRepo`]), which is why it is spelled once.
///
/// [`CredentialStore`]: crate::secrets::CredentialStore
#[must_use]
pub fn kek() -> crate::secrets::Kek {
    std::sync::Arc::new(yagra_secrets::StaticKeyProvider::single([7u8; 32]))
}

/// A sealed credential, created through the production writer.
///
/// For the tables that carry a `REFERENCES credentials (id)` foreign key — `meraki_orgs`, `nodes`,
/// `url_checks` — where a hand-written `INSERT` would be a second copy of the envelope-encryption
/// columns as well as of the schema.
///
/// Sealed with [`kek`], which says what that does and does not prove.
pub async fn credential(pool: &PgPool, name: &str, kind: &str) -> Uuid {
    crate::secrets::CredentialStore::new(pool.clone(), kek())
        .create(name, kind, b"a-test-secret")
        .await
        .unwrap_or_else(|e| panic!("create credential {name}: {e}"))
}

/// One `TIMESTAMPTZ` column of the row a node owns (`WHERE node_id = …`).
///
/// The common case of [`timestamp_of`], spelled out so the majority of callers cannot get the key
/// column's name wrong.
pub async fn node_timestamp(
    pool: &PgPool,
    table: &str,
    column: &str,
    node: Uuid,
) -> chrono::DateTime<chrono::Utc> {
    timestamp_of(pool, table, column, "node_id", node).await
}

/// One `TIMESTAMPTZ` column of one row, found by a uuid key.
///
/// For the columns a repository *writes* and exposes no reader for. `node_arp.first_seen` is the
/// case this was written for: the ARP store keeps "this port has looked like this for three weeks"
/// and nothing in production ever selects it, so without this the rule that an unchanged walk must
/// not restart the clock is assertable only as text — which cannot tell a `CASE` that works from
/// one that is spelled right and evaluates the wrong way. `meraki_orgs.last_sync_at` is the second.
///
/// Deliberately narrow rather than a general "run this statement": a test that can spell any SQL
/// becomes a second copy of the schema, which is what the fixtures above exist to avoid.
///
/// ⚠️ Three of the five arguments are names, so the order matters and nothing checks it: it is
/// `(table, column, key_column, key)`. Prefer [`node_timestamp`] where it fits.
pub async fn timestamp_of(
    pool: &PgPool,
    table: &str,
    column: &str,
    key_column: &str,
    key: Uuid,
) -> chrono::DateTime<chrono::Utc> {
    // Interpolated for the same reason `rows` interpolates: neither a table nor a column name can
    // be a bind parameter. Every caller is a literal in this crate's own tests, and the key — the
    // only value that comes from anywhere — is bound.
    sqlx::query_scalar(&format!(
        "SELECT {column} FROM {table} WHERE {key_column} = $1"
    ))
    .bind(key)
    .fetch_one(pool)
    .await
    .unwrap_or_else(|e| panic!("{table}.{column}: {e}"))
}

#[cfg(test)]
mod guards {
    use std::path::{Path, PathBuf};

    use yagra_common::srcread::{file_name, read, rs_files};

    /// How many database tests must exist for the check below to mean anything.
    ///
    /// Deliberately below the real count: this is a floor against the detector going blind, not a
    /// target. Raise it when a whole new area gets covered, not per test.
    // Raised from 12 by ADR-115, which took the population from 18 to 79. The floor is about
    // the *detector*, not about coverage: it has to be able to tell "nothing is wrong" apart from
    // "the needle stopped matching", and a floor left at its first value stops doing that.
    const MIN_DATABASE_TESTS: usize = 70;

    /// This crate's `src` directory.
    fn src() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
    }

    /// Every `.rs` file under `src`, as `(name, raw text)`.
    ///
    /// 🚨 **Raw**, not [`crate::module_source::code`]. That reader removes every test-only item,
    /// which is *all* of the population these checks are about — pointed at it they would search
    /// an empty string, find nothing, and pass forever. That is the same failure the reader
    /// itself exists to stop, met from the other side.
    fn raw_files() -> Vec<(String, String)> {
        let mut paths = Vec::new();
        rs_files(&src(), &mut paths);
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
        let sample =
            format!("{attr}(migrator = \"m\")]\n{mark}]\nasync fn good() {{}}\n{attr}]\nasync fn bad() {{}}\n");
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

    /// **The migration macro keeps exactly one call site**, which is what `repo/migrate.rs` says.
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
             from `repo/migrate.rs`'s `MIGRATIONS` static, and read from there",
            sites.len()
        );
        assert!(
            sites[0].starts_with("migrate.rs:"),
            "the one call site moved out of `repo/migrate.rs`: {sites:?}"
        );
    }

    /// Files holding raw SQL that have no test **in the file**, each with the reason why.
    ///
    /// ⚠️ **Not a backlog.** Each entry is an argument that a test does not belong *here*, not a
    /// note that one has yet to be written. The list was two entries when it was created and the
    /// intent is that it stays that way.
    const SQL_WITHOUT_A_TEST_OF_ITS_OWN: [(&str, &str); 2] = [
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
}
