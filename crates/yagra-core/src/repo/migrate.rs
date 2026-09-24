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
                    // A data-modifying CTE starts with `with`, so neither prefix above sees it —
                    // and 0122 is exactly that shape. Nothing in migrations/ started with `with`
                    // before it, so widening the needle cannot turn an existing file red
                    // (ADR-162 decision 3).
                    || (s.starts_with("with ")
                        && (s.contains("update ") || s.contains("delete ")))
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

    /// **Migration 0122 gives each scope one scale, and does not move anything on screen.**
    ///
    /// The state it repairs is what migration 0015 left in every deployment: `row_number()` seeded
    /// per table, so a scope's first folder and its first node both carry `1`. Once the tree draws
    /// them as one list that tie is broken by name — and, worse, `placement_orders` divides the gap
    /// between two neighbours, so two equal neighbours collapse every midpoint onto their value and
    /// a drag writes 204 while nothing moves.
    ///
    /// The statements are read out of the migration file rather than retyped, so this cannot pass
    /// against a second copy of the SQL that the deployment never runs.
    ///
    /// ⚠️ The harness has already run every migration, 0122 included, so the fixture puts a scope
    /// *back* into the pre-0122 shape and applies the file again. That is what makes the assertions
    /// about the statement rather than about the seeding.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn the_single_scale_migration_preserves_the_visible_order(pool: sqlx::PgPool) {
        use crate::groups::{GroupRepo, GroupType};
        let groups = GroupRepo::new(pool.clone());
        let parent = groups
            .create("parent", GroupType::Site, None, None)
            .await
            .expect("parent");
        // Named so name order and position order disagree: repaired correctly the folders keep
        // their own order (zulu, alpha) rather than falling into alphabetical.
        let f1 = groups
            .create("zulu", GroupType::Generic, Some(parent), None)
            .await
            .expect("f1");
        let f2 = groups
            .create("alpha", GroupType::Generic, Some(parent), None)
            .await
            .expect("f2");
        let n1 = crate::pgtest::node(&pool, "node-one", 40, Some(parent)).await;
        let n2 = crate::pgtest::node(&pool, "node-two", 41, Some(parent)).await;

        // Back to the pre-0122 shape: each table numbered from 1 in its own scope.
        for (id, order) in [(f1, 1.0_f64), (f2, 2.0)] {
            sqlx::query("UPDATE node_groups SET sort_order = $2 WHERE id = $1")
                .bind(id)
                .bind(order)
                .execute(&pool)
                .await
                .expect("seed folder order");
        }
        for (id, order) in [(n1, 1.0_f64), (n2, 2.0)] {
            sqlx::query("UPDATE nodes SET sort_order = $2 WHERE id = $1")
                .bind(id)
                .bind(order)
                .execute(&pool)
                .await
                .expect("seed node order");
        }

        let sql = include_str!("../../../../migrations/0122_tree_ordering_single_scale.sql");
        let code = sql
            .lines()
            .filter(|l| !l.trim_start().starts_with("--"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut applied = 0usize;
        for stmt in code.split(';') {
            if stmt.trim().is_empty() {
                continue;
            }
            sqlx::query(stmt)
                .execute(&pool)
                .await
                .expect("the migration's statement applies");
            applied += 1;
        }
        assert_eq!(
            applied, 2,
            "0122 is two statements; the split found {applied}"
        );

        let rows = crate::groups::ordered_tree_siblings(&pool, Some(parent))
            .await
            .expect("siblings");
        assert_eq!(
            rows.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            vec![f1, f2, n1, n2],
            "the folders keep their order and stay above the nodes — the order that was on screen"
        );
        let mut orders: Vec<f64> = rows.iter().map(|(_, o)| *o).collect();
        orders.sort_by(f64::total_cmp);
        orders.dedup_by(|a, b| (*a - *b).abs() < f64::EPSILON);
        assert_eq!(orders.len(), 4, "every position distinct after the repair");
    }

    /// **Migration 0123 turns AP import on — for the controllers that exist and for every one
    /// after** (ADR-064 R22).
    ///
    /// Both halves are claims about this file, not about the seeding: an existing row that says
    /// `FALSE` becomes `TRUE`, and a row created afterwards by a first inventory — whose INSERT
    /// never names the column — comes out `TRUE`, because the column default is the only place the
    /// default is written. As with 0122 the harness has already applied it, so the fixture puts
    /// the table back into the pre-0123 shape first and applies the file's own statements.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn the_import_default_migration_switches_existing_and_future_controllers_on(
        pool: sqlx::PgPool,
    ) {
        use yagra_common::{WlanFlavor, WlanInventory};
        let repo = crate::wireless::WirelessRepo::new(pool.clone());
        let empty = WlanInventory::bounded(WlanFlavor::Huawei, Vec::new(), 1024);
        let existing = crate::pgtest::node(&pool, "wac-existing", 50, None).await;
        repo.record_inventory(existing, &empty, chrono::Utc::now())
            .await
            .expect("first inventory");

        // Back to the pre-0123 shape: the 0121 default, and a row that inherited it.
        for stmt in [
            "ALTER TABLE wireless_controllers ALTER COLUMN import_aps SET DEFAULT FALSE",
            "UPDATE wireless_controllers SET import_aps = FALSE",
        ] {
            sqlx::query(stmt)
                .execute(&pool)
                .await
                .expect("restore the pre-0123 shape");
        }
        assert!(
            !repo.controller(existing).await.unwrap().unwrap().import_aps,
            "the fixture did not put the row back to off"
        );

        let sql = include_str!("../../../../migrations/0123_wireless_import_default_on.sql");
        let code = sql
            .lines()
            .filter(|l| !l.trim_start().starts_with("--"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut applied = 0usize;
        for stmt in code.split(';') {
            if stmt.trim().is_empty() {
                continue;
            }
            sqlx::query(stmt)
                .execute(&pool)
                .await
                .expect("the migration's statement applies");
            applied += 1;
        }
        assert_eq!(
            applied, 2,
            "0123 is two statements; the split found {applied}"
        );

        assert!(
            repo.controller(existing).await.unwrap().unwrap().import_aps,
            "an existing controller is switched on"
        );
        let later = crate::pgtest::node(&pool, "wac-later", 51, None).await;
        repo.record_inventory(later, &empty, chrono::Utc::now())
            .await
            .expect("first inventory");
        assert!(
            repo.controller(later).await.unwrap().unwrap().import_aps,
            "a controller registered afterwards starts on"
        );
    }

    /// **Migration 0128 adds the Cisco controller profile to the seeded AP-walk rule — and only to
    /// the row as it shipped** (ADR-064 増分 F).
    ///
    /// The harness has applied 0128 to an empty table and the seeder has since written the new,
    /// two-profile row, so the fixture first puts the row back to what v0.3.27/28 shipped — the
    /// Huawei profile alone — and applies the file's own statement. A second copy of the row with an
    /// operator's bound on it must come out exactly as it went in.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn the_wlan_walk_rule_migration_adds_the_cisco_profile_to_the_shipped_row_only(
        pool: sqlx::PgPool,
    ) {
        use crate::seed_ids::SeedRange;
        crate::pgtest::repo(pool.clone())
            .seed_builtin_profiles()
            .await
            .expect("seed");
        let profiles = yagra_common::builtin_profiles();
        let id_of = |name: &str| {
            SeedRange::Profiles
                .id(profiles.iter().position(|p| p.name == name).expect(name))
                .to_string()
        };
        let huawei = id_of("Huawei wireless controller");
        let cisco = id_of("Cisco wireless controller");
        let rule = SeedRange::DefaultThresholds.id(33);
        let targets = |id: uuid::Uuid| {
            let pool = pool.clone();
            async move {
                sqlx::query_scalar::<_, Vec<String>>(
                    "SELECT scope_ids FROM thresholds WHERE id = $1",
                )
                .bind(id)
                .fetch_one(&pool)
                .await
                .expect("the rule exists")
            }
        };
        assert_eq!(
            targets(rule).await,
            vec![huawei.clone(), cisco.clone()],
            "a fresh database is seeded with both"
        );

        // Back to the shipped shape, plus an edited copy that must be left alone.
        sqlx::query("UPDATE thresholds SET scope_ids = ARRAY[$2] WHERE id = $1")
            .bind(rule)
            .bind(&huawei)
            .execute(&pool)
            .await
            .expect("restore the pre-0128 shape");
        let edited = uuid::Uuid::from_u128(0x0128_0000_0000_0000_0000_0000_0000_0001);
        sqlx::query(
            "INSERT INTO thresholds (id, scope_level, scope_id, scope_ids, metric, direction, \
             warning, critical, dwell_samples, row_match, warning_below) \
             SELECT $1, scope_level, scope_id, scope_ids, metric, direction, 0.9, critical, \
                    dwell_samples, row_match, 0.9 \
               FROM thresholds WHERE id = $2",
        )
        .bind(edited)
        .bind(rule)
        .execute(&pool)
        .await
        .expect("an operator's edited copy");

        let sql = include_str!("../../../../migrations/0128_wlan_walk_rule_covers_cisco.sql");
        let code = sql
            .lines()
            .filter(|l| !l.trim_start().starts_with("--"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut applied = 0u64;
        for stmt in code.split(';').filter(|s| !s.trim().is_empty()) {
            applied += sqlx::query(stmt)
                .execute(&pool)
                .await
                .expect("the migration's statement applies")
                .rows_affected();
        }
        assert_eq!(applied, 1, "exactly the shipped row changes");
        assert_eq!(targets(rule).await, vec![huawei.clone(), cisco]);
        assert_eq!(
            targets(edited).await,
            vec![huawei],
            "an edited rule keeps its targets"
        );
    }

    /// Foreign-key columns deliberately left without an index, and why each one is safe.
    ///
    /// PostgreSQL runs one referential action per deleted row against every table whose foreign
    /// key points at it; where the referencing column has no index, each action reads the whole
    /// table. ADR-124 増分 7 found six such columns to `nodes` by querying the catalog by hand —
    /// the review that prompted it had listed three — and indexed the two that grow with the
    /// fleet. The first run of this check (2026-09-15, ADR-150) asked the same question of every
    /// foreign key in the schema and found eighteen, in three classes:
    ///
    /// * **rows an operator writes by hand** — the table stays small whatever the inventory does
    ///   (ADR-124 Inc.7 decision B, and the same reason for the fourteen it did not look at);
    /// * **one deployment-wide row**;
    /// * **the referenced row is deleted one at a time**: `nodes`, `url_checks`, `events` and
    ///   `report_runs` do grow, but their referenced tables (credentials, event sources, report
    ///   definitions) are edited by an operator one row at a time with `ON DELETE SET NULL`, so
    ///   the action is **one** sequential scan per delete — not one per deleted row, which was
    ///   the ADR-124 Inc.7 shape — and no query filters by the column (`grep '<col> = $'`, none).
    ///
    /// **An entry here is a decision about growth, not a way to silence the test.** A column on a
    /// table that grows with nodes, interfaces or events, whose referenced rows are deleted in
    /// bulk or read by that column, wants a migration, not a line. ⚠️ The third class is the one
    /// to revisit: the day a "which nodes use this credential" read is added, `nodes.credential_id`
    /// wants its index for the read, whatever the delete costs.
    const OPERATOR_ROWS: &str =
        "rows an operator writes by hand; the table does not grow with the \
                                 fleet (ADR-124 Inc.7 decision B)";
    const ONE_ROW: &str = "a single deployment-wide row; the whole-table read is one row";
    const REFERENCED_DELETED_SINGLY: &str =
        "the table grows with the fleet, but the referenced row is deleted one at a time by an \
         operator (ON DELETE SET NULL), so the action is one sequential scan per delete rather \
         than one per deleted row, and no query filters by this column (ADR-150 first run)";
    const FOREIGN_KEYS_WITHOUT_AN_INDEX: &[(&str, &str, &str)] = &[
        ("bus_tls_config", "issued_by", ONE_ROW),
        ("classification_rules", "profile_id", OPERATOR_ROWS),
        ("event_rules", "node_id", OPERATOR_ROWS),
        ("event_rules", "source_id", OPERATOR_ROWS),
        ("event_sources", "node_id", OPERATOR_ROWS),
        (
            "events",
            "source_id",
            "grows with traffic, but a source is deleted one at a time by an operator (ON DELETE \
             SET NULL) and no query filters events by source; `events` is the hot insert path and \
             already carries the six indexes ADR-024 wants fewer of — an index here would cost \
             every insert to speed a rare delete (ADR-150 first run)",
        ),
        ("meraki_orgs", "group_id", OPERATOR_ROWS),
        ("netbox_servers", "credential_id", OPERATOR_ROWS),
        ("nodes", "credential_id", REFERENCED_DELETED_SINGLY),
        (
            "pollers",
            "anchor_node_id",
            "one row per poller, registered by an operator — tens, never tens of thousands \
             (ADR-124 Inc.7 decision B)",
        ),
        (
            "pollers",
            "token_issued_by",
            "one row per poller, registered by an operator — tens, never tens of thousands",
        ),
        ("profile_collection_templates", "template_id", OPERATOR_ROWS),
        ("profiles", "parent_id", OPERATOR_ROWS),
        (
            "report_runs",
            "definition_id",
            "one row per report run and pruned by retention; a definition is deleted one at a time \
             (ON DELETE SET NULL) and the runs are not read by definition (ADR-150 first run)",
        ),
        ("report_schedules", "definition_id", OPERATOR_ROWS),
        (
            "suppression_exemptions",
            "node_id",
            "rows an operator writes by hand; `UNIQUE (kind, node_id)` leads on `kind`, so it \
             cannot serve the lookup either (ADR-124 Inc.7 decision B)",
        ),
        ("url_checks", "credential_id", REFERENCED_DELETED_SINGLY),
        ("web_tls_config", "imported_by", ONE_ROW),
    ];

    /// Every foreign key whose **leading** column no index leads on, as `(table, column,
    /// referenced table)`.
    ///
    /// A partial index counts (`WHERE col IS NOT NULL`, migration 0115): the referential action
    /// looks rows up by equality with a real id, which a NULL row can never match. A composite
    /// index counts when the key's first column is its first column, which is what
    /// `indkey[0] = conkey[1]` says — `pg_index.indkey` is zero-based and `pg_constraint.conkey`
    /// one-based, and getting that wrong reports every key as unindexed.
    async fn unindexed_foreign_keys(pool: &sqlx::PgPool) -> Vec<(String, String, String)> {
        sqlx::query_as::<_, (String, String, String)>(
            "SELECT c.conrelid::regclass::text, a.attname::text, c.confrelid::regclass::text \
             FROM pg_constraint c \
             JOIN pg_attribute a ON a.attrelid = c.conrelid AND a.attnum = c.conkey[1] \
             WHERE c.contype = 'f' \
               AND NOT EXISTS (\
                 SELECT 1 FROM pg_index i \
                 WHERE i.indrelid = c.conrelid AND i.indkey[0] = c.conkey[1]\
               ) \
             ORDER BY 1, 2",
        )
        .fetch_all(pool)
        .await
        .expect("query the catalog for unindexed foreign keys")
    }

    /// **Every foreign key column has an index leading on it, or a written reason not to**
    /// (ADR-150 決定 4(c)).
    ///
    /// The detector is proven before the schema is judged by it: a throwaway table with an
    /// unindexed key to `nodes` must be reported, or the catalog query has drifted and the empty
    /// answer below means nothing. The floor on the key count is the same defence from the other
    /// side.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn every_foreign_key_column_has_an_index_or_states_why(pool: sqlx::PgPool) {
        sqlx::query(
            "CREATE TABLE _probe_fk (id serial PRIMARY KEY, node uuid REFERENCES nodes (id))",
        )
        .execute(&pool)
        .await
        .expect("create the probe table");
        let seen = unindexed_foreign_keys(&pool).await;
        assert!(
            seen.iter()
                .any(|(t, c, r)| t == "_probe_fk" && c == "node" && r == "nodes"),
            "the catalog query no longer reports a plainly unindexed foreign key: {seen:?}"
        );
        sqlx::query("DROP TABLE _probe_fk")
            .execute(&pool)
            .await
            .expect("drop the probe table");

        let total: i64 =
            sqlx::query_scalar("SELECT count(*) FROM pg_constraint WHERE contype = 'f'")
                .fetch_one(&pool)
                .await
                .expect("count foreign keys");
        assert!(
            total >= 40,
            "only {total} foreign keys in the schema — the query drifted"
        );

        let unindexed = unindexed_foreign_keys(&pool).await;
        let unexplained: Vec<String> = unindexed
            .iter()
            .filter(|(t, c, _)| {
                !FOREIGN_KEYS_WITHOUT_AN_INDEX
                    .iter()
                    .any(|(et, ec, _)| et == t && ec == c)
            })
            .map(|(t, c, r)| format!("{t}.{c} -> {r}"))
            .collect();
        assert!(
            unexplained.is_empty(),
            "these foreign key columns have no index leading on them. Deleting from the referenced \
             table reads the whole referencing table once per deleted row; add a migration with the \
             index, or — only if the table cannot grow with the fleet — an entry in \
             FOREIGN_KEYS_WITHOUT_AN_INDEX saying why: {unexplained:#?}"
        );

        // The exemption list needs the hygiene every exemption list here has: each entry still
        // names a real unindexed key (an index added later means the line comes out), and says why.
        for (t, c, why) in FOREIGN_KEYS_WITHOUT_AN_INDEX {
            assert!(
                why.trim().len() >= 20,
                "FOREIGN_KEYS_WITHOUT_AN_INDEX exempts {t}.{c} without saying why"
            );
            assert!(
                unindexed.iter().any(|(ut, uc, _)| ut == t && uc == c),
                "FOREIGN_KEYS_WITHOUT_AN_INDEX names {t}.{c}, which now has an index or no longer \
                 exists — drop the entry"
            );
        }
    }

    /// **Migration 0125 leaves an organization that already exists OFF and starts a new one ON**
    /// (ADR-164 Inc.4).
    ///
    /// The file does it with two statements — `ADD COLUMN … DEFAULT FALSE`, which is what fills the
    /// existing rows, then `SET DEFAULT TRUE` — and the order is the whole decision: every
    /// organization in a deployment today was imported by hand, and switching automatic import on
    /// for it would add every device somebody chose to leave out. A harness that migrates first can
    /// only ever see the second half, and no lab box holds an organization to see the first on, so
    /// this applies the history in two steps with a row created in between.
    #[sqlx::test(migrations = false)]
    #[ignore = "needs DATABASE_URL"]
    async fn migration_0125_leaves_existing_meraki_organizations_off_and_starts_new_ones_on(
        pool: sqlx::PgPool,
    ) {
        const IMPORT_SETTINGS: i64 = 125;
        // 0134 raises the cap's default for organizations added after it (ADR-164 決定 33), which
        // is its own test below; this one stops before it, so it pins what 0125 did and nothing after.
        const CAP_DEFAULT: i64 = 134;
        let embedded = embedded_migrations();
        let (before, from): (Vec<_>, Vec<_>) = embedded
            .iter()
            .filter(|m| m.version < CAP_DEFAULT)
            .partition(|m| m.version < IMPORT_SETTINGS);
        assert!(
            from.iter().any(|m| m.version == IMPORT_SETTINGS),
            "migration 0125 is not embedded"
        );

        for m in before {
            sqlx::raw_sql(&m.sql)
                .execute(&pool)
                .await
                .unwrap_or_else(|e| panic!("apply {}: {e}", m.version));
        }
        let credential = crate::pgtest::credential(&pool, "meraki-key", "meraki_api").await;
        let orgs = crate::meraki::MerakiOrgRepo::new(pool.clone());
        let old = orgs
            .create(
                "1",
                "Imported by hand",
                "https://api.meraki.com",
                credential,
            )
            .await
            .expect("an organization from before 0125");

        for m in from {
            sqlx::raw_sql(&m.sql)
                .execute(&pool)
                .await
                .unwrap_or_else(|e| panic!("apply {}: {e}", m.version));
        }
        let new = orgs
            .create(
                "2",
                "Added afterwards",
                "https://api.meraki.com",
                credential,
            )
            .await
            .expect("an organization from after 0125");

        let old = orgs.get(old).await.expect("get").expect("old");
        let new = orgs.get(new).await.expect("get").expect("new");
        assert!(
            !old.import_devices,
            "an organization imported by hand was switched to automatic import by the upgrade"
        );
        assert!(
            new.import_devices,
            "a new organization must import on its own"
        );
        // The other three are the same for both.
        for o in [&old, &new] {
            assert!(o.file_by_prefix, "{}", o.name);
            assert_eq!((o.max_devices, o.devices_over_cap), (1000, 0), "{}", o.name);
        }
    }

    /// **Migration 0134 raises the import cap only for an organization added after it** (ADR-164
    /// 決定 33).
    ///
    /// It changes nothing but the column's default, so an organization already added keeps the 1,000
    /// it was given — which may be one an operator chose, and nothing tells the two apart — while
    /// one added afterwards starts at 10,000. The history is applied in two steps with an
    /// organization created in between, for the reason the 0125 test above gives.
    #[sqlx::test(migrations = false)]
    #[ignore = "needs DATABASE_URL"]
    async fn migration_0134_raises_the_cap_only_for_organizations_added_after_it(
        pool: sqlx::PgPool,
    ) {
        const CAP_DEFAULT: i64 = 134;
        let embedded = embedded_migrations();
        let (before, from): (Vec<_>, Vec<_>) =
            embedded.iter().partition(|m| m.version < CAP_DEFAULT);
        assert!(
            from.iter().any(|m| m.version == CAP_DEFAULT),
            "migration 0134 is not embedded"
        );

        for m in before {
            sqlx::raw_sql(&m.sql)
                .execute(&pool)
                .await
                .unwrap_or_else(|e| panic!("apply {}: {e}", m.version));
        }
        let credential = crate::pgtest::credential(&pool, "meraki-key", "meraki_api").await;
        let orgs = crate::meraki::MerakiOrgRepo::new(pool.clone());
        let old = orgs
            .create("1", "Added before", "https://api.meraki.com", credential)
            .await
            .expect("an organization from before 0134");

        for m in from {
            sqlx::raw_sql(&m.sql)
                .execute(&pool)
                .await
                .unwrap_or_else(|e| panic!("apply {}: {e}", m.version));
        }
        let new = orgs
            .create("2", "Added after", "https://api.meraki.com", credential)
            .await
            .expect("an organization from after 0134");

        let old = orgs.get(old).await.expect("get").expect("old");
        let new = orgs.get(new).await.expect("get").expect("new");
        assert_eq!(
            old.max_devices, 1000,
            "the upgrade changed the cap of an organization already added"
        );
        assert_eq!(new.max_devices, 10_000);
    }

    /// **Migration 0135 shortens the traffic interval only for an organization added after it**
    /// (ADR-164 決定 34).
    ///
    /// Like 0134 it changes nothing but the column's default: an organization already added keeps
    /// the 1,800 seconds it was given — which may be one an operator chose — while one added
    /// afterwards collects its MX uplinks' traffic every 300. The history is applied in two steps
    /// with an organization created in between, for the reason the 0125 test above gives.
    #[sqlx::test(migrations = false)]
    #[ignore = "needs DATABASE_URL"]
    async fn migration_0135_shortens_the_traffic_interval_only_for_organizations_added_after_it(
        pool: sqlx::PgPool,
    ) {
        const TRAFFIC_DEFAULT: i64 = 135;
        let embedded = embedded_migrations();
        let (before, from): (Vec<_>, Vec<_>) =
            embedded.iter().partition(|m| m.version < TRAFFIC_DEFAULT);
        assert!(
            from.iter().any(|m| m.version == TRAFFIC_DEFAULT),
            "migration 0135 is not embedded"
        );

        for m in before {
            sqlx::raw_sql(&m.sql)
                .execute(&pool)
                .await
                .unwrap_or_else(|e| panic!("apply {}: {e}", m.version));
        }
        let credential = crate::pgtest::credential(&pool, "meraki-key", "meraki_api").await;
        let orgs = crate::meraki::MerakiOrgRepo::new(pool.clone());
        let old = orgs
            .create("1", "Added before", "https://api.meraki.com", credential)
            .await
            .expect("an organization from before 0135");

        for m in from {
            sqlx::raw_sql(&m.sql)
                .execute(&pool)
                .await
                .unwrap_or_else(|e| panic!("apply {}: {e}", m.version));
        }
        let new = orgs
            .create("2", "Added after", "https://api.meraki.com", credential)
            .await
            .expect("an organization from after 0135");

        let old = orgs.get(old).await.expect("get").expect("old");
        let new = orgs.get(new).await.expect("get").expect("new");
        assert_eq!(
            old.traffic_secs, 1800,
            "the upgrade changed the traffic interval of an organization already added"
        );
        assert_eq!(new.traffic_secs, 300);
    }

    /// **Migration 0126 gives availability back to an organization saved without it, and touches no
    /// other row** (ADR-164 決定 17).
    ///
    /// Since Inc.3 that tier is the only one that says whether a Meraki device is up, so such an
    /// organization's nodes could never be reported down. The API refuses the shape from here on,
    /// but a stored value never fixes itself — and no lab box holds an organization to see this on,
    /// so the history is applied in two steps with the rows written in between.
    #[sqlx::test(migrations = false)]
    #[ignore = "needs DATABASE_URL"]
    async fn migration_0126_gives_the_availability_tier_back_and_touches_no_other_row(
        pool: sqlx::PgPool,
    ) {
        const AVAILABILITY_ALWAYS_ON: i64 = 126;
        // 0130 appends the switch-port tier to every row (ADR-167), which is its own test below;
        // this one stops before it, so it pins what 0126 did and nothing after.
        const SWITCH_PORTS: i64 = 130;
        let embedded = embedded_migrations();
        let (before, from): (Vec<_>, Vec<_>) = embedded
            .iter()
            .filter(|m| m.version < SWITCH_PORTS)
            .partition(|m| m.version < AVAILABILITY_ALWAYS_ON);
        assert!(
            from.iter().any(|m| m.version == AVAILABILITY_ALWAYS_ON),
            "migration 0126 is not embedded"
        );

        for m in before {
            sqlx::raw_sql(&m.sql)
                .execute(&pool)
                .await
                .unwrap_or_else(|e| panic!("apply {}: {e}", m.version));
        }
        let credential = crate::pgtest::credential(&pool, "meraki-key", "meraki_api").await;
        let orgs = crate::meraki::MerakiOrgRepo::new(pool.clone());
        let mut made = Vec::new();
        for (org_id, tiers) in [
            ("1", vec!["uplink", "traffic"]),
            ("2", vec![]),
            ("3", vec!["traffic", "availability"]),
        ] {
            let id = orgs
                .create(org_id, org_id, "https://api.meraki.com", credential)
                .await
                .expect("an organization from before 0126");
            sqlx::query("UPDATE meraki_orgs SET enabled_tiers = $2 WHERE id = $1")
                .bind(id)
                .bind(&tiers)
                .execute(&pool)
                .await
                .expect("store the tiers an older release accepted");
            made.push(id);
        }

        for m in from {
            sqlx::raw_sql(&m.sql)
                .execute(&pool)
                .await
                .unwrap_or_else(|e| panic!("apply {}: {e}", m.version));
        }

        // Read raw: the repository's reader names columns later migrations add.
        let mut after: Vec<Vec<String>> = Vec::new();
        for id in made {
            after.push(
                sqlx::query_scalar("SELECT enabled_tiers FROM meraki_orgs WHERE id = $1")
                    .bind(id)
                    .fetch_one(&pool)
                    .await
                    .expect("the row"),
            );
        }
        assert_eq!(
            after[0],
            vec!["availability", "uplink", "traffic"],
            "an organization saved without availability still has nothing that says a device is down"
        );
        assert_eq!(after[1], vec!["availability"], "the empty set");
        assert_eq!(
            after[2],
            vec!["traffic", "availability"],
            "a row that already carried the tier was rewritten"
        );
    }

    /// **Migration 0130 starts every existing organization collecting its switch ports** (ADR-167
    /// 決定 12, the user's decision): the tier is appended once, whatever else the row holds, the
    /// interval takes its default, and a new organization gets both from the column defaults.
    /// Applied in two steps with the rows written in between, like 0126's test.
    #[sqlx::test(migrations = false)]
    #[ignore = "needs DATABASE_URL"]
    async fn migration_0130_adds_the_switch_port_tier_to_every_organization_once(
        pool: sqlx::PgPool,
    ) {
        const SWITCH_PORTS: i64 = 130;
        // 0131 appends the wireless tier to every row (ADR-168), which is its own test below; this
        // one stops before it, so it pins what 0130 did and nothing after.
        const WIRELESS: i64 = 131;
        let embedded = embedded_migrations();
        let (before, from): (Vec<_>, Vec<_>) = embedded
            .iter()
            .filter(|m| m.version < WIRELESS)
            .partition(|m| m.version < SWITCH_PORTS);
        assert!(
            from.iter().any(|m| m.version == SWITCH_PORTS),
            "migration 0130 is not embedded"
        );
        for m in before {
            sqlx::raw_sql(&m.sql)
                .execute(&pool)
                .await
                .unwrap_or_else(|e| panic!("apply {}: {e}", m.version));
        }
        let credential = crate::pgtest::credential(&pool, "meraki-key", "meraki_api").await;
        let orgs = crate::meraki::MerakiOrgRepo::new(pool.clone());
        let mut made = Vec::new();
        for (org_id, tiers) in [
            ("1", vec!["availability", "uplink", "traffic"]),
            ("2", vec!["availability"]),
            // Already carrying it (a core that had it, rolled back and forward again).
            ("3", vec!["availability", "switch_ports"]),
        ] {
            let id = orgs
                .create(org_id, org_id, "https://api.meraki.com", credential)
                .await
                .expect("an organization from before 0130");
            sqlx::query("UPDATE meraki_orgs SET enabled_tiers = $2 WHERE id = $1")
                .bind(id)
                .bind(&tiers)
                .execute(&pool)
                .await
                .expect("store the tiers");
            made.push(id);
        }
        for m in from {
            sqlx::raw_sql(&m.sql)
                .execute(&pool)
                .await
                .unwrap_or_else(|e| panic!("apply {}: {e}", m.version));
        }

        // Read raw: the repository's reader names columns later migrations add.
        let read = |id: uuid::Uuid| {
            let pool = pool.clone();
            async move {
                sqlx::query_as::<_, (Vec<String>, i32)>(
                    "SELECT enabled_tiers, switch_ports_secs FROM meraki_orgs WHERE id = $1",
                )
                .bind(id)
                .fetch_one(&pool)
                .await
                .expect("the row")
            }
        };
        let mut after = Vec::new();
        for id in &made {
            after.push(read(*id).await);
        }
        assert_eq!(
            after[0].0,
            ["availability", "uplink", "traffic", "switch_ports"]
        );
        assert_eq!(after[1].0, ["availability", "switch_ports"]);
        assert_eq!(
            after[2].0,
            ["availability", "switch_ports"],
            "a row that already carried the tier got it twice"
        );
        assert!(after.iter().all(|o| o.1 == 300));

        // A new organization gets the tier and the interval from the column defaults.
        let fresh = orgs
            .create("4", "4", "https://api.meraki.com", credential)
            .await
            .expect("a new organization");
        let fresh = read(fresh).await;
        assert!(fresh.0.iter().any(|t| t == "switch_ports"));
        assert_eq!(fresh.1, 300);
    }

    /// **Migration 0131 starts every existing organization collecting its access points' readings**
    /// (ADR-168 決定 10): the tier is appended once, whatever else the row holds, the interval takes
    /// its default, and a new organization gets both from the column defaults. Applied in two steps
    /// with the rows written in between, like 0130's test.
    #[sqlx::test(migrations = false)]
    #[ignore = "needs DATABASE_URL"]
    async fn migration_0131_adds_the_wireless_tier_to_every_organization_once(pool: sqlx::PgPool) {
        const WIRELESS: i64 = 131;
        let embedded = embedded_migrations();
        let (before, from): (Vec<_>, Vec<_>) = embedded.iter().partition(|m| m.version < WIRELESS);
        assert!(
            from.iter().any(|m| m.version == WIRELESS),
            "migration 0131 is not embedded"
        );
        for m in before {
            sqlx::raw_sql(&m.sql)
                .execute(&pool)
                .await
                .unwrap_or_else(|e| panic!("apply {}: {e}", m.version));
        }
        let credential = crate::pgtest::credential(&pool, "meraki-key", "meraki_api").await;
        let orgs = crate::meraki::MerakiOrgRepo::new(pool.clone());
        let mut made = Vec::new();
        for (org_id, tiers) in [
            (
                "1",
                vec!["availability", "uplink", "traffic", "switch_ports"],
            ),
            ("2", vec!["availability"]),
            // Already carrying it (a core that had it, rolled back and forward again).
            ("3", vec!["availability", "wireless"]),
        ] {
            let id = orgs
                .create(org_id, org_id, "https://api.meraki.com", credential)
                .await
                .expect("an organization from before 0131");
            sqlx::query("UPDATE meraki_orgs SET enabled_tiers = $2 WHERE id = $1")
                .bind(id)
                .bind(&tiers)
                .execute(&pool)
                .await
                .expect("store the tiers");
            made.push(id);
        }
        for m in from {
            sqlx::raw_sql(&m.sql)
                .execute(&pool)
                .await
                .unwrap_or_else(|e| panic!("apply {}: {e}", m.version));
        }

        let mut after = Vec::new();
        for id in &made {
            after.push(orgs.get(*id).await.expect("get").expect("org"));
        }
        assert_eq!(
            after[0].enabled_tiers,
            [
                "availability",
                "uplink",
                "traffic",
                "switch_ports",
                "wireless"
            ]
        );
        assert_eq!(after[1].enabled_tiers, ["availability", "wireless"]);
        assert_eq!(
            after[2].enabled_tiers,
            ["availability", "wireless"],
            "a row that already carried the tier got it twice"
        );
        assert!(after.iter().all(|o| o.wireless_secs == 300));

        // A new organization gets the tier and the interval from the column defaults.
        let fresh = orgs
            .create("4", "4", "https://api.meraki.com", credential)
            .await
            .expect("a new organization");
        let fresh = orgs.get(fresh).await.expect("get").expect("org");
        assert!(fresh.enabled_tiers.iter().any(|t| t == "wireless"));
        assert_eq!(fresh.wireless_secs, 300);

        // The band is the database's too, not only the API's.
        for outside in [299, 601] {
            let refused = sqlx::query("UPDATE meraki_orgs SET wireless_secs = $2 WHERE id = $1")
                .bind(made[0])
                .bind(outside)
                .execute(&pool)
                .await;
            assert!(refused.is_err(), "{outside} s was stored");
        }
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
