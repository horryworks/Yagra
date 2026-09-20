-- 0127_meraki_collect_failures — which of an organization's collects are failing, and why
-- (ADR-164 決定 18).
--
-- reversible: one column is added to `meraki_orgs`, with a default. Nothing is dropped and nothing
-- is narrowed. A core from before it names its columns explicitly and never reads this one, so
-- rolling the binary back leaves every row readable. No `schema_compat` floor, for the reason 0108
-- records: every release from 0.2.2 on tolerates a database carrying migrations it does not embed.
--
-- WHAT IT HOLDS
-- A JSON array with one entry per collect tier that is failing right now:
--   [{"tier": "availability", "reason": "auth", "since_unix_ms": 1790000000000, "failures": 4}]
-- `[]` — the default, and what every existing row gets — means no tier is known to be failing.
-- `reason` is a `MerakiSyncFailure` token (the vocabulary `last_sync_error` already uses, plus
-- `no_answer`: a collect was sent and nothing came back).
--
-- WHY IT IS ON THE ROW AND NOT ONLY IN MEMORY
-- The leader's health loop (`meraki_health.rs`) knows this from the collect reports it has heard.
-- The organization's row and page are served by whichever core answers the request, and after a
-- restart the loop knows nothing until the next round of collects reports in — up to a cadence
-- later. The row is what lets both say "collection failing: the API key was refused" in between.
-- The loop writes it only when it changes, and clears a tier only when that tier is *answered*:
-- knowing nothing clears nothing (ADR-156 決定 3).
--
-- One JSONB column rather than a column per tier: the set of tiers is the enum's to decide, and a
-- fourth would otherwise be a migration.

ALTER TABLE meraki_orgs
    ADD COLUMN collect_failures JSONB NOT NULL DEFAULT '[]'::jsonb;
