-- 0080_upgrade_compat_floor — declare the floor that migration 0078 actually imposed (ADR-050).
--
-- 0078 introduced `schema_compat` with the rule "saying nothing means reversible", and said nothing
-- about itself. That was wrong, and the WebUI made it visible: with an empty table the release
-- picker believed every published version could still run this database, and offered twenty
-- downgrades. **All of them would refuse to start.**
--
-- Measured against the real image (2026-08-11): `ghcr.io/horryworks/yagra-core:v0.2.1` embeds
-- migrations up to 0077 and contains neither 0078 nor `relax_ignore_missing`. sqlx's
-- `validate_applied_migrations` sees applied versions it does not know, `ignore_missing` is false,
-- and core exits with `VersionMissing` before it serves anything.
--
-- The floor is therefore NOT about a narrowed schema — nothing here is narrower. It is the other
-- half of decision 7, the one the ADR wrote down and 0078 forgot to encode: **the relaxation only
-- helps if the version being returned to has it.** Every release from 0.2.2 on does, because it is
-- on main before that version exists; every release up to and including 0.2.1 does not, and no
-- other version can ever be published in between.
--
-- Additive: one row. An N-1 core never reads `schema_compat`, and a core that does read it treats a
-- floor above itself as "this database is newer than me", which is exactly true.
--
-- ⚠️ Whoever adds the next migration should ask whether it needs its own row. The rule is not
-- "narrowing migrations declare a floor" — it is **"any migration declares a floor when the oldest
-- core that can start afterwards is newer than the one this replaces"**. For an additive migration
-- landing on a database that already carries this row, that answer is no: this floor already covers
-- it. The row to add is the one that raises the floor further.
INSERT INTO schema_compat (migration_version, min_core, reason)
VALUES (
    78,
    '0.2.2',
    'Releases before 0.2.2 refuse to start against a database carrying migrations they do not '
    'embed: the startup relaxation that tolerates a newer database (repo.rs::relax_ignore_missing) '
    'first shipped in 0.2.2, so returning to an earlier one needs a restore from backup.'
)
ON CONFLICT (migration_version) DO NOTHING;
