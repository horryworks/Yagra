-- 0096_threshold_scope_ids — one rule may name several targets (ADR-078 decision 2).
--
-- Until now a rule reached exactly one profile / folder group / node, so covering the two Cisco
-- IOS profiles meant two rules that then had to be edited twice. `scope_ids` holds the whole set;
-- `scope_level` still says what kind of thing those ids are.
--
-- `scope_id` is deliberately KEPT and is still written by the new core, holding the first target.
-- That is the rollback path: a core that predates this column reads `scope_id` exactly as before
-- and resolves the rule at that one target. It covers fewer nodes than intended, which is the
-- direction that goes quiet rather than the direction that breaks — and the row itself stays valid.
--
-- reversible: adds a column and backfills only the column it just added. `scope_id` is not
--   touched, so an older core reads every row exactly as it did before this ran.

ALTER TABLE thresholds ADD COLUMN scope_ids TEXT[] NOT NULL DEFAULT '{}';

-- A `global` rule has nothing to point at and keeps the empty array.
UPDATE thresholds SET scope_ids = ARRAY[scope_id] WHERE scope_id <> '';
