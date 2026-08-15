-- 0083_user_preferences — per-account WebUI preferences, as one opaque JSON document (ADR-058).
--
-- reversible: additive (one new table, nothing narrowed, nothing rewritten in place). No
-- `schema_compat` floor is owed — 0080 already pins min_core 0.2.2, and an additive migration does
-- not move the oldest core that can start afterwards. (0078 needed a floor despite also being
-- additive, but for an unrelated reason: releases before 0.2.2 lack `repo.rs::relax_ignore_missing`
-- and refuse to start on an applied version they do not embed. Every release from 0.2.2 on has it.)
--
-- ⚠️ STATE THE ROLLBACK BEHAVIOUR PLAINLY RATHER THAN CALLING IT SAFE. An older core does not serve
-- /api/v1/preferences, so after a downgrade the WebUI's GET answers 404 and every operator falls
-- back to the value in their own browser (`localStorage['yagra_prefs']`). Nothing is lost — the row
-- survives untouched and is picked up again on the way back up — and nothing about monitoring
-- changes, because no part of the backend reads this table. The exposure is exactly "the preference
-- stops following you between machines for the length of the rollback".
--
-- WHY ONE OPAQUE BLOB AND NOT A COLUMN PER PREFERENCE
-- The same argument migration 0023 made for `user_dashboards`, and it is worth repeating because it
-- is the whole value of the decision: the backend never parses this. The WebUI owns the shape and
-- migrates it client-side, so the SECOND preference costs zero backend work — no migration, no DTO,
-- no OpenAPI regeneration, no generated TypeScript, no N/N-1 argument. A column per preference would
-- put every future checkbox on both sides of the wire.
-- ⚠️ The cost of that choice, written down so nobody is surprised by it: the server cannot validate,
-- query, report on, or migrate the contents. If a preference ever needs to be READ by the backend (a
-- notification default, a report locale), it does not belong here — it belongs in its own typed
-- column with its own migration.
--
-- NOT FOLDED INTO `user_dashboards`. A widget layout and a UI preference have different owners,
-- different lifetimes and different sizes; one row would mean one save clobbering the other's
-- concurrent write, and would put a 256 KiB layout and a 40-byte number under one cap.
--
-- Deleted on CASCADE when the account is removed, so no orphans (0023's rule).
CREATE TABLE IF NOT EXISTS user_preferences (
    user_id    UUID PRIMARY KEY REFERENCES users (id) ON DELETE CASCADE,
    prefs_json JSONB NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
