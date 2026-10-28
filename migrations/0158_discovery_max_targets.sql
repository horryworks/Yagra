-- 0158_discovery_max_targets — the most addresses one discovery sweep may carry becomes a setting
-- (ADR-219 decision 1). It was the compiled constant `MAX_SCAN_TARGETS` (4096, a /20, ADR-173).
--
-- reversible: additive only — one column on the singleton `app_settings` row, with a default equal
-- to the constant it replaces. An older core reads and writes this row through explicit column
-- lists, so it never selects the column and its upserts leave it at whatever it holds: rolling the
-- binary back returns to the compiled 4096, and rolling forward again finds the operator's value.
-- No `schema_compat` floor, for the reason 0108 records.
--
-- The upper bound is a /16. Past that one request stops being a site and starts being a
-- background scan of someone else's network, and the import of what it found no longer fits the
-- request-body limit the discovery routes carry.
ALTER TABLE app_settings
    ADD COLUMN IF NOT EXISTS discovery_max_targets INTEGER NOT NULL DEFAULT 4096
        CHECK (discovery_max_targets BETWEEN 1 AND 65536);
