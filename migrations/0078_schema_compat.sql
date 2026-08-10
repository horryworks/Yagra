-- 0078_schema_compat — how far back this schema can still be run (ADR-050 decision 7).
--
-- Yagra can already go forward. It could not answer "can I go back?", and the reason that question
-- looked unanswerable was a conflation worth writing down:
--
--   (A) data compatibility — under expand-contract (ADR-017) an `up` is ADDITIVE: a nullable
--       column, a new table. An N-1 binary reads an N schema perfectly well; it just does not
--       select the new column. Only a contract step or an in-place rewrite breaks that.
--   (B) sqlx's startup guard — `validate_applied_migrations` refuses to start when the database
--       carries a version the binary does not embed. That is a POLICY check, not a data problem,
--       and `Migrator::set_ignore_missing` is its knob.
--
-- So the barrier is mostly (B), and (B) is safe to relax exactly when the extra applied versions
-- are all NEWER than everything this binary embeds — "the database is simply ahead of me" — which
-- is what `repo::relax_ignore_missing` decides. This table answers (A): whether the schema has
-- been narrowed in a way an older binary cannot survive.
--
-- ── The contract ─────────────────────────────────────────────────────────────────────────────
-- EMPTY MEANS REVERSIBLE. A migration that only adds things inserts nothing here. A migration that
-- narrows the schema — drops a column an older binary still selects, rewrites values in place,
-- retypes a column — inserts ONE row naming the oldest core version that can still run afterwards.
-- The floor for the whole database is then `max(min_core)` over the applied rows.
--
-- Reversible is the default because it is the truth for 77 of the 77 migrations that predate this
-- one, and because getting the default backwards would make the UI advertise a rollback that
-- crash-loops. Declaring the exception is the rare act.
--
-- ⚠️ Nothing in the compiler or in sqlx enforces the insert — it is a hand-written fact in SQL, and
-- forgetting it makes Yagra promise a rollback it cannot deliver. `repo.rs`'s
-- `every_destructive_migration_declares_its_reversibility` is the guard: any migration containing
-- destructive DDL/DML must carry either a row here or an explicit `-- reversible: <why>` marker.
--
-- `min_core` is a bare semver string ("0.2.4"), compared with the `semver` crate rather than in SQL
-- — a text comparison puts 0.2.10 before 0.2.9, and this is the one value that must not be subtly
-- wrong. `migration_version` is the `_sqlx_migrations.version` that imposed the floor, which is how
-- the join finds only the floors that have actually been applied to THIS database.
--
-- Additive, and no core reads it until ADR-050 Inc.1a ⇒ N-1 safe.

CREATE TABLE schema_compat (
    migration_version BIGINT PRIMARY KEY,
    min_core          TEXT        NOT NULL,
    reason            TEXT        NOT NULL,
    declared_at       TIMESTAMPTZ NOT NULL DEFAULT now()
);
