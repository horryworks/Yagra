-- 0162_reseed_unsaved_config_rules — the Unsaved configuration rules move from one hour to four
-- (ADR-222 decision 12), and become automatic save's grace (decision 11).
--
-- Seeded rows carry stable ids and are inserted `ON CONFLICT (id) DO NOTHING`, so the new bound in
-- `repo/defaults.rs` cannot reach a row that already exists. Deleting the two lets the next boot
-- seed them again with four hours - migration 0097's mechanism.
--
-- Offsets 35 and 36 of the `DefaultThresholds` range (0x...5eedc000 + offset). Both shipped only to
-- the verification deployments and are listed under `## Unreleased`, so an operator's edit to them
-- is lost rather than merged, which is acceptable for the reason 0097 gives.
--
-- reversible: deletes two seeded default threshold rows; a core that boots against this database
--   seeds its own version of them, so an older one restores exactly what it used to write.

DELETE FROM thresholds
 WHERE id IN ('00000000-0000-0000-0000-00005eedc023'::uuid,
              '00000000-0000-0000-0000-00005eedc024'::uuid);
