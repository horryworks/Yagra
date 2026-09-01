// SPDX-License-Identifier: AGPL-3.0-only
//! Migrations: what this binary embeds, and whether it may start against a given database.
//!
//! [`MIGRATIONS`] is the **one** `sqlx::migrate!` call site in the workspace, because two call
//! sites of that macro are two answers to "what does this build embed?" that nothing keeps equal.
//! It has three readers: [`NodeRepo::migrate`] applies it, `yagra-core migrations` prints it
//! **without a database** (ADR-050 決定 6), and `#[sqlx::test(migrator = …)]` migrates each test's
//! throwaway database from it (ADR-114). The first two go through [`embedded_migrations`], which
//! hands back an owned value they may adjust.
//!
//! 🚨 **An applied migration is immutable.** `sqlx::migrate!` checksums every file, so changing one
//! byte in a migration that has already run makes every existing deployment refuse to start. That
//! is why the historical reversibility declarations live in `GRANDFATHERED_REVERSIBLE` in this
//! file's tests rather than as comments in the migrations themselves — the first cut of that rule
//! wrote the comment into nine applied files and took the test server down on deploy.

// Only the settings struct: `retention::Row` would collide with `sqlx::Row` above.

use super::*;

/// **The one `sqlx::migrate!` call site in the workspace**, and `guards` holds it to that.
///
/// Two call sites of that macro are two answers to "what does this build embed?" that nothing
/// keeps equal, which is why [`embedded_migrations`] was a function wrapping the macro rather
/// than a second call of it. ADR-114 needed the same set from a *third* place —
/// `#[sqlx::test(migrator = "crate::repo::MIGRATIONS")]`, which takes a `&'static Migrator` and
/// so cannot call a function — so the macro moved into this static and the function reads it.
/// The count is unchanged: still one macro, now with two readers instead of one.
///
/// A `static` works because `Migrator`'s fields are `pub` (`#[doc(hidden)]`, semver-exempt) for
/// exactly this purpose — sqlx-core's own comment says they exist so `migrate!()` can initialise
/// them "in an implicitly const-promotable context".
pub static MIGRATIONS: sqlx::migrate::Migrator = sqlx::migrate!("../../migrations");

/// The migration set compiled into this binary, as an owned value the caller may adjust.
///
/// [`NodeRepo::migrate`] needs ownership: it may call `set_ignore_missing` (ADR-050 decision 7).
/// `yagra-core migrations` prints it **without a database**, which is what lets an upgrade be
/// planned from the target image before anything is touched (ADR-050 decision 6).
#[must_use]
pub fn embedded_migrations() -> sqlx::migrate::Migrator {
    sqlx::migrate::Migrator {
        // `Cow::Borrowed` — the clone copies a pointer, not 101 migrations.
        migrations: MIGRATIONS.migrations.clone(),
        ignore_missing: MIGRATIONS.ignore_missing,
        locking: MIGRATIONS.locking,
        no_tx: MIGRATIONS.no_tx,
    }
}

/// May this binary start against a database whose migration history it does not fully recognise?
///
/// sqlx refuses by default: `validate_applied_migrations` returns `VersionMissing` for any applied
/// version the binary does not embed. That guard is a **policy check, not a data check** — under
/// expand-contract (ADR-017) an `up` is additive, so an older binary reads a newer schema perfectly
/// well; it simply never selects the new columns. The guard exists to catch *misconfiguration*, and
/// the misconfigurations it catches are worth keeping.
///
/// So the relaxation is deliberately narrow (ADR-050 decision 7). `true` only when:
///
///  * at least one applied version is not embedded here, **and**
///  * every such version is greater than the newest version this binary embeds.
///
/// That is exactly the shape of "the database is simply ahead of me" — a downgrade. Anything else
/// (a hole in the middle, a version from a different migration set) still fails hard, because those
/// are the real accidents: pointed at the wrong database, or handed someone else's migrations.
///
/// Checksum mismatches are unaffected — they surface as `VersionMismatch`, which `ignore_missing`
/// does not touch. An edited applied migration still refuses to boot, as it must.
fn relax_ignore_missing(embedded: &[i64], applied: &[i64]) -> bool {
    let Some(newest_embedded) = embedded.iter().copied().max() else {
        return false; // No embedded migrations at all: nothing to reason from.
    };
    let known: std::collections::BTreeSet<i64> = embedded.iter().copied().collect();
    let mut saw_newer = false;
    for version in applied {
        if known.contains(version) {
            continue;
        }
        if *version <= newest_embedded {
            return false; // A gap *within* our own range — not a downgrade.
        }
        saw_newer = true;
    }
    saw_newer
}

impl NodeRepo {
    /// Apply all embedded migrations (expand-contract, ADR-017). Embedded at compile
    /// time, so this needs no database at build.
    ///
    /// Starts in **downgrade-compatibility mode** when the database carries migrations this binary
    /// does not embed *and every one of them is newer than everything it does* — see
    /// [`relax_ignore_missing`] for why that condition is the whole safety argument.
    pub async fn migrate(&self) -> anyhow::Result<()> {
        let mut migrator = embedded_migrations();
        let embedded: Vec<i64> = migrator.iter().map(|m| m.version).collect();
        let applied = self.applied_migration_versions().await;
        if relax_ignore_missing(&embedded, &applied) {
            let ahead: Vec<i64> = applied
                .iter()
                .copied()
                .filter(|v| !embedded.contains(v))
                .collect();
            tracing::warn!(
                versions = ?ahead,
                core_version = env!("CARGO_PKG_VERSION"),
                "this database was migrated by a NEWER core; starting in downgrade-compatibility \
                 mode (ADR-050). Columns those migrations added are present but unread — upgrading \
                 again makes them visible, and nothing is lost meanwhile."
            );
            migrator.set_ignore_missing(true);
        }
        migrator.run(&self.pool).await?;
        tracing::info!("database migrations applied");
        Ok(())
    }

    /// Versions recorded in `_sqlx_migrations`, ascending.
    ///
    /// Every failure collapses to "none", which is correct for the one case that matters — a fresh
    /// database has no such table — and harmless for the rest: an empty answer only ever *disables*
    /// the relaxation below, and a database that is genuinely unreachable fails a moment later in
    /// `run` with a far better error than this function could produce.
    async fn applied_migration_versions(&self) -> Vec<i64> {
        match sqlx::query_scalar::<_, i64>("SELECT version FROM _sqlx_migrations ORDER BY version")
            .fetch_all(&self.pool)
            .await
        {
            Ok(v) => v,
            Err(e) => {
                tracing::debug!(error = %e, "no migration history yet (fresh database?)");
                Vec::new()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The downgrade relaxation opens for a database that is *ahead*, and for nothing else.
    ///
    /// Each case here is a misconfiguration the default guard exists to catch, so the assertion is
    /// as much about the `false`s as the `true` (ADR-050 decision 7).
    #[test]
    fn the_ignore_missing_relaxation_only_opens_for_a_newer_database() {
        let embedded = [1i64, 2, 3];

        // The ordinary cases: nothing extra to forgive.
        assert!(!relax_ignore_missing(&embedded, &[]), "fresh database");
        assert!(
            !relax_ignore_missing(&embedded, &[1, 2, 3]),
            "exactly current"
        );
        assert!(
            !relax_ignore_missing(&embedded, &[1, 2]),
            "database behind us — `run` simply applies 3"
        );

        // The one case that opens it: every unknown version is newer than everything we embed.
        assert!(relax_ignore_missing(&embedded, &[1, 2, 3, 4]), "one ahead");
        assert!(
            relax_ignore_missing(&embedded, &[1, 2, 3, 4, 5]),
            "several ahead"
        );
        assert!(
            relax_ignore_missing(&embedded, &[2, 3, 4]),
            "ahead, and a version we embed was never applied — `run` still applies 1"
        );

        // A hole *inside* our own range is a different database or a different migration set —
        // note both sets below are chosen so the unknown version sits BELOW the newest embedded
        // one, which is the only thing that distinguishes an accident from a downgrade.
        assert!(
            !relax_ignore_missing(&[1, 2, 5], &[1, 3, 5]),
            "3 is unknown and sits below our newest — an unrecognised history, not a downgrade"
        );
        assert!(
            !relax_ignore_missing(&[10, 20], &[5]),
            "wholly foreign history below our newest"
        );

        // Degenerate input must not fail open.
        assert!(!relax_ignore_missing(&[], &[1, 2, 3]), "nothing embedded");
    }

    /// The exact boot decision an N-1 core makes against the ADR-081 schema, on real numbers.
    ///
    /// Measured 2026-08-21 rather than imagined: the shipped `f30570a` image reports 97 embedded
    /// migrations when asked (`yagra-core migrations`, side-effect-free by design), and the test
    /// deployment's `_sqlx_migrations` holds 98 applied. So the rollback question — "does the
    /// older binary start, or does it refuse a database it thinks is from elsewhere" — is exactly
    /// this predicate on exactly these two lists.
    ///
    /// 🚨 This is the half of the rollback that can be answered without a rollback. It says the
    /// binary decides to start; it does not say the process then runs. That is still owed, and the
    /// ADR-081 entry in the backlog says so.
    #[test]
    fn the_shipped_n1_core_boots_against_the_adr_081_schema() {
        let embedded: Vec<i64> = (1..=97).collect();
        let applied: Vec<i64> = (1..=98).collect();
        assert!(
            relax_ignore_missing(&embedded, &applied),
            "a core that predates migration 0098 must still start against a database that has it"
        );
        // The relaxation must still be a decision. On the same measured embedded list, a database
        // that is merely *current* has nothing to forgive and must not open it — otherwise the
        // assertion above would pass equally on a function that returned true unconditionally.
        // (The misconfigurations the guard catches are enumerated in
        // `the_ignore_missing_relaxation_only_opens_for_a_newer_database`; this pair is only about
        // the two lists that were actually measured.)
        assert!(
            !relax_ignore_missing(&embedded, &embedded),
            "a database at exactly this binary's level forgives nothing"
        );
    }

    /// Every migration that narrows the schema must say how far back it can still be run.
    ///
    /// `schema_compat` (0078) answers "can this deployment go back to version X?", and its default
    /// is **reversible** — an additive migration inserts nothing. That default is true for all 77
    /// migrations that predate it, and it is also the dangerous one: a contract step whose author
    /// forgets the row makes the WebUI advertise a rollback that crash-loops. Neither SQL nor sqlx
    /// can catch that, so this does — a destructive migration must carry either an
    /// `INSERT INTO schema_compat` floor or an explicit `-- reversible: <why>` marker.
    ///
    /// Comments are stripped before the scan. Three of the twelve files a naive grep first flagged
    /// mention `DROP INDEX` only in prose explaining why they are reversible, and an index is
    /// invisible to the binary anyway — which is why `drop index` is not in the needle list.
    ///
    /// ⚠️ **The historical declarations live HERE rather than in the files, and that is forced.**
    /// `sqlx::migrate!` checksums every migration, so adding even a comment line to one that has
    /// already been applied makes every existing deployment refuse to start with
    /// `migration N was previously applied but has been modified`. This was learned the expensive
    /// way: the first cut of this rule wrote a `-- reversible:` line into all nine and took the
    /// test server down on deploy. An applied migration is immutable — full stop.
    ///
    /// So: migrations **already applied somewhere** are grandfathered in this list. New ones carry
    /// the marker in the file, where it belongs, because nothing has checksummed them yet.
    const GRANDFATHERED_REVERSIBLE: &[(&str, &str)] = &[
        ("0015", "backfills only the two columns it just added"),
        (
            "0020",
            "range-deletes built-in catalog rows; an older core re-seeds its own on boot",
        ),
        (
            "0021",
            "deletes one operator-created catalog row, not schema",
        ),
        (
            "0022",
            "deletes one built-in catalog row; an older core re-seeds it",
        ),
        (
            "0030",
            "corrects one seeded threshold value; the schema is untouched",
        ),
        (
            "0051",
            "drops a CHECK only to immediately re-add a WIDER one",
        ),
        (
            "0052",
            "drops a CHECK only to immediately re-add a WIDER one",
        ),
        ("0057", "backfills only the column it just added"),
        (
            "0076",
            "`DROP CONSTRAINT IF EXISTS` is the idempotent re-create idiom here",
        ),
    ];

    #[test]
    fn every_destructive_migration_declares_its_reversibility() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../migrations");
        let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(&dir)
            .expect("migrations/ is readable from the crate directory")
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "sql"))
            .collect();
        files.sort();
        assert!(files.len() >= 78, "migrations/ looks truncated");

        for path in files {
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            let raw = std::fs::read_to_string(&path).expect("migration is readable");
            // Statements only. Splitting each line at `--` would also truncate a string literal
            // containing a double dash, but that can only ever *hide* a statement from the scan,
            // never invent one — and no migration here has such a literal.
            let code = raw
                .lines()
                .map(|l| l.split("--").next().unwrap_or(""))
                .collect::<Vec<_>>()
                .join("\n")
                .to_lowercase();
            // No migration uses dollar-quoted bodies, so `;` is a safe statement separator.
            let destructive = code.split(';').any(|stmt| {
                let s = stmt.trim();
                s.starts_with("update ")
                    || s.starts_with("delete ")
                    || s.contains("drop column")
                    || s.contains("drop table")
                    || s.contains("drop constraint")
                    || (s.contains("alter column") && s.contains(" type "))
            });
            if !destructive {
                continue;
            }
            let grandfathered = GRANDFATHERED_REVERSIBLE
                .iter()
                .any(|(prefix, _)| name.starts_with(prefix));
            assert!(
                grandfathered
                    || raw.to_lowercase().contains("-- reversible:")
                    || code.contains("insert into schema_compat"),
                "{name} narrows the schema or rewrites rows in place, but declares neither an \
                 `INSERT INTO schema_compat` floor nor a `-- reversible: <why>` marker. Decide \
                 which it is — the WebUI promises a rollback based on this (ADR-050 decision 7). \
                 If this migration has ALREADY been applied to a live deployment, do not edit the \
                 file: it is checksummed, and changing it stops every existing deployment from \
                 starting. Add it to GRANDFATHERED_REVERSIBLE instead."
            );
        }
    }

    // ── Against a real database (ADR-114) ────────────────────────────────────────────────
    //
    // These take `migrations = false`, which is the one place in the crate that wants an
    // *unmigrated* database: everything below is about `NodeRepo::migrate` itself, and a harness
    // that had already run it would be testing nothing.

    /// **Every embedded migration applies, in order, to an empty database.**
    ///
    /// Nothing proved this before ADR-114. The set is 101 files and the only thing that had ever
    /// run them from nothing was a deployment — so a migration that is fine on top of the
    /// previous release and broken from scratch would first be seen by a *new install*, which is
    /// the one path with no rollback and the one this repository has repeatedly found unguarded.
    ///
    /// The count assertion is the floor: `run()` returning `Ok` over zero migrations looks exactly
    /// like `run()` succeeding over all of them.
    #[sqlx::test(migrations = false)]
    #[ignore = "needs DATABASE_URL"]
    async fn every_embedded_migration_applies_to_an_empty_database(pool: sqlx::PgPool) {
        let embedded = embedded_migrations().iter().count();
        assert!(embedded >= 100, "only {embedded} migrations are embedded");

        let repo = crate::pgtest::repo(pool.clone());
        repo.migrate().await.expect("migrate an empty database");

        let applied: i64 = sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations")
            .fetch_one(&pool)
            .await
            .expect("read _sqlx_migrations");
        assert_eq!(
            applied,
            i64::try_from(embedded).unwrap(),
            "the database has {applied} of {embedded} migrations applied"
        );
        // And the schema is usable, not merely recorded: `nodes` is the table the first migration
        // creates and the last release still reads.
        assert_eq!(crate::pgtest::rows(&pool, "nodes").await, 0);
    }

    /// **Migrating twice does nothing the second time**, which is what every restart does.
    #[sqlx::test(migrations = false)]
    #[ignore = "needs DATABASE_URL"]
    async fn migrating_a_second_time_applies_nothing(pool: sqlx::PgPool) {
        let repo = crate::pgtest::repo(pool.clone());
        repo.migrate().await.expect("first migrate");
        let after_first: Vec<i64> = sqlx::query_scalar(
            "SELECT version FROM _sqlx_migrations WHERE success ORDER BY version",
        )
        .fetch_all(&pool)
        .await
        .expect("read versions");

        repo.migrate().await.expect("second migrate");
        let after_second: Vec<i64> = sqlx::query_scalar(
            "SELECT version FROM _sqlx_migrations WHERE success ORDER BY version",
        )
        .fetch_all(&pool)
        .await
        .expect("read versions");

        assert_eq!(after_first, after_second);
    }

    /// **A database a newer core migrated still boots** (ADR-050 決定 7) — the downgrade path.
    ///
    /// [`relax_ignore_missing`] is unit-tested above as a pure function; what was never tested is
    /// that [`NodeRepo::migrate`] actually consults it and passes the answer to sqlx. That is the
    /// half a rollback depends on, and the half that fails as a refusal to start.
    ///
    /// Paired with the rejection below, deliberately: a `migrate` that always succeeded would
    /// satisfy this test on its own.
    #[sqlx::test(migrations = false)]
    #[ignore = "needs DATABASE_URL"]
    async fn a_database_migrated_by_a_newer_core_still_boots(pool: sqlx::PgPool) {
        let repo = crate::pgtest::repo(pool.clone());
        repo.migrate().await.expect("migrate");
        record_foreign_migration(&pool, 999_999, "a version from a future release").await;

        repo.migrate()
            .await
            .expect("a database that is merely ahead must still boot");
    }

    /// **A hole in the middle of the history refuses to boot**, which is a different database or
    /// a different migration set — the misconfiguration the sqlx guard exists to catch.
    ///
    /// Version 0 is the only spelling available: the embedded set is 1…N with no gaps, so every
    /// value inside its range is one we know. Below the range is still "unknown and not ahead",
    /// which is exactly the predicate.
    #[sqlx::test(migrations = false)]
    #[ignore = "needs DATABASE_URL"]
    async fn a_history_we_do_not_recognise_refuses_to_boot(pool: sqlx::PgPool) {
        let repo = crate::pgtest::repo(pool.clone());
        repo.migrate().await.expect("migrate");
        record_foreign_migration(&pool, 0, "a version from somebody else's migration set").await;

        let err = repo
            .migrate()
            .await
            .expect_err("an unrecognised history must not be forgiven");
        let text = format!("{err:#}");
        assert!(
            text.contains('0'),
            "the refusal should name the version it did not recognise: {text}"
        );
    }

    /// Write a row into sqlx's own bookkeeping table, as a core with a different set would have.
    ///
    /// The checksum is deliberately not a real one: nothing reads it on this path
    /// (`ignore_missing` does not touch `VersionMismatch`), and computing one would make the
    /// fixture look like it was asserting something it is not.
    async fn record_foreign_migration(pool: &sqlx::PgPool, version: i64, description: &str) {
        sqlx::query(
            "INSERT INTO _sqlx_migrations \
             (version, description, success, checksum, execution_time) \
             VALUES ($1, $2, true, $3, 0)",
        )
        .bind(version)
        .bind(description)
        .bind(vec![0u8; 48])
        .execute(pool)
        .await
        .expect("insert a foreign migration row");
    }
}
