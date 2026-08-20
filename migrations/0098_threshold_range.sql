-- 0098_threshold_range — one rule may bound a metric on both sides (ADR-081).
--
-- Until now a rule named one `direction` and two bounds, so "alert when the optical receive level
-- falls below -20 dBm OR rises above -3 dBm" could not be written as one rule. Operators wrote two
-- rules at the same scope instead — and the second one was stored, listed, and never evaluated:
-- `thresholds` has no unique key over (scope_level, scope_ids, metric), and `resolve_effective`
-- took `direction` from whichever winner came first in the index, then folded every other winner's
-- bounds under that direction. A rule facing the other way had its bounds compared the wrong way
-- round and disappeared into the merge.
--
-- The four new columns are the truth from here on; `direction`, `warning` and `critical` are
-- DELIBERATELY KEPT and are still written by the new core, carrying the *primary side* of the rule.
-- That is the rollback path: a core that predates this migration reads exactly the three columns it
-- always did and enforces one side of the band. It watches for less than the operator asked for,
-- which is the direction that goes quiet rather than the direction that breaks, and the row itself
-- stays valid and editable. Same shape as 0096's `scope_id` and ADR-076's `alert_history.ifindex`.
--
-- reversible: adds four columns and backfills only the columns it just added. Nothing is dropped,
-- narrowed, or re-typed; no column an older binary selects changes meaning. An N-1 core boots on
-- this schema and resolves every row through `direction`/`warning`/`critical` as before, so the
-- oldest bootable core does not move and no `schema_compat` floor is owed (0078).
--
-- No UNIQUE constraint over (scope_level, scope_ids, metric) is added, and that is a decision
-- rather than an omission (ADR-081 decision 4). Measured on the test deployment 2026-08-21: 0
-- duplicate groups across 33 rules — but one deployment is not every deployment, and a constraint
-- that fails to apply is a core that refuses to start. The duplicate is refused at the API edge
-- instead, where it can return a 409 to the person creating it.

ALTER TABLE thresholds
    ADD COLUMN IF NOT EXISTS warning_below  DOUBLE PRECISION,
    ADD COLUMN IF NOT EXISTS critical_below DOUBLE PRECISION,
    ADD COLUMN IF NOT EXISTS warning_above  DOUBLE PRECISION,
    ADD COLUMN IF NOT EXISTS critical_above DOUBLE PRECISION;

-- Backfill every existing row from the triple it already carries. `direction` is free text with no
-- CHECK constraint, and the reader has always treated anything that is not exactly 'below' as
-- 'above' — so this mirrors that reading rather than inventing a stricter one here, which would
-- move rows to the other side of the band during an upgrade.
UPDATE thresholds
SET warning_below  = CASE WHEN direction = 'below' THEN warning  END,
    critical_below = CASE WHEN direction = 'below' THEN critical END,
    warning_above  = CASE WHEN direction = 'below' THEN NULL ELSE warning  END,
    critical_above = CASE WHEN direction = 'below' THEN NULL ELSE critical END
WHERE warning_below IS NULL
  AND critical_below IS NULL
  AND warning_above IS NULL
  AND critical_above IS NULL;
