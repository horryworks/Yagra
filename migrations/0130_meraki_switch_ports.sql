-- 0130_meraki_switch_ports — collect every Meraki switch port's status, speed and traffic
-- (ADR-167).
--
-- reversible: one column is added to `meraki_orgs`, with a default and a CHECK on its own value, the
-- column default of `enabled_tiers` gains a token, and an UPDATE appends that token to the rows that
-- lack it. Nothing is dropped and nothing is narrowed. A core from before it names its columns
-- explicitly and never reads the new one; its scheduler skips a tier token it does not know
-- (`MerakiOrg::active_tiers`), so rolling the binary back leaves every row readable and every
-- collect it knows running. No `schema_compat` floor, for the reason 0108 records: every release
-- from 0.2.2 on tolerates a database carrying migrations it does not embed.
--
-- ⚠️ One thing does not survive a rollback cleanly: the cadence dialog of that older core sends the
-- stored `enabled_tiers` back, `switch_ports` included, and its `PUT …/cadence` refuses a token it
-- does not know with `400 invalid_tier`. Saving the dialog fails until this core is back; nothing is
-- lost and nothing stops collecting.
--
-- WHY EVERY ROW, NOT ONLY NEW ONES
-- The user's decision (2026-09-22): an organization that exists already starts collecting its
-- switches' ports on the upgrade, like a new one. The token is appended at the end — the order
-- carries no meaning to the scheduler (`MerakiOrg::active_tiers` sorts by cadence).
--
-- WHAT IT COSTS
-- Two organization-wide paged reads per `switch_ports_secs` (default 300 s) — the ports' statuses at
-- 20 switches a page and one usage bucket at 50 — and once an hour the ports' configured names at 50
-- a page. Measured on a real organization of 854 switches: about 100 s of requests, and about 190 s
-- in the hour the names are read, inside the 300 s the organization's single collect flight is held.
-- Core sends it only to a pool whose every live poller claims `meraki-switch-ports`, and only to an
-- organization with at least one imported switch.
--
-- THE BAND
-- 300–600 s. Nothing finer than the Dashboard's five-minute usage bucket exists, and a port collected
-- much less often would have its `interfaces` row drawn as stale (900 s) between two collects.

ALTER TABLE meraki_orgs ADD COLUMN IF NOT EXISTS switch_ports_secs INTEGER NOT NULL DEFAULT 300;
ALTER TABLE meraki_orgs DROP CONSTRAINT IF EXISTS meraki_orgs_switch_ports_secs_check;
ALTER TABLE meraki_orgs ADD CONSTRAINT meraki_orgs_switch_ports_secs_check
    CHECK (switch_ports_secs BETWEEN 300 AND 600);

ALTER TABLE meraki_orgs
    ALTER COLUMN enabled_tiers SET DEFAULT ARRAY['availability', 'uplink', 'traffic', 'switch_ports'];

UPDATE meraki_orgs
   SET enabled_tiers = enabled_tiers || ARRAY['switch_ports'],
       updated_at = now()
 WHERE NOT ('switch_ports' = ANY (enabled_tiers));
