-- 0131_meraki_wireless_tier — collect every Meraki access point's clients, radio utilization, SSIDs
-- and radio settings (ADR-168).
--
-- reversible: one column is added to `meraki_orgs`, with a default and a CHECK on its own value, the
-- column default of `enabled_tiers` gains a token, and an UPDATE appends that token to the rows that
-- lack it. Nothing is dropped and nothing is narrowed. A core from before it names its columns
-- explicitly and never reads the new one; its scheduler skips a tier token it does not know
-- (`MerakiOrg::active_tiers`), so rolling the binary back leaves every row readable and every
-- collect it knows running. No `schema_compat` floor, for the reason 0108 records: every release
-- from 0.2.2 on tolerates a database carrying migrations it does not embed.
--
-- ⚠️ One thing does not survive a rollback cleanly, as with 0130: the cadence dialog of an older core
-- sends the stored `enabled_tiers` back, `wireless` included, and its `PUT …/cadence` refuses a token
-- it does not know with `400 invalid_tier`. Saving the dialog fails until this core is back; nothing
-- is lost and nothing stops collecting. By hand:
--   UPDATE meraki_orgs SET enabled_tiers = array_remove(enabled_tiers, 'wireless');
--
-- WHY EVERY ROW, NOT ONLY NEW ONES
-- As 0130 did for the switch ports: an organization that exists already starts collecting its access
-- points' readings on the upgrade, like a new one. The token is appended at the end — the order
-- carries no meaning to the scheduler (`MerakiOrg::active_tiers` sorts by cadence).
--
-- WHAT IT COSTS
-- Two organization-wide paged reads per `wireless_secs` (default 300 s) — every access point's
-- clients and every radio's utilization, 1,000 a page — and every twenty minutes the SSIDs and radio
-- settings at 250 a page. Measured on a real organization of 1,710 access points: about 5 s, and
-- about 80 s in the collects that read the SSIDs, inside the 300 s the organization's single collect
-- flight is held. Core sends it only to a pool whose every live poller claims `meraki-wireless`, and
-- only to an organization with at least one imported access point.
--
-- THE BAND
-- 300–600 s. The utilization is a five-minute bucket, and a radio collected much less often would
-- have its `interfaces` row drawn as stale (900 s) between two collects.

ALTER TABLE meraki_orgs ADD COLUMN IF NOT EXISTS wireless_secs INTEGER NOT NULL DEFAULT 300;
ALTER TABLE meraki_orgs DROP CONSTRAINT IF EXISTS meraki_orgs_wireless_secs_check;
ALTER TABLE meraki_orgs ADD CONSTRAINT meraki_orgs_wireless_secs_check
    CHECK (wireless_secs BETWEEN 300 AND 600);

ALTER TABLE meraki_orgs
    ALTER COLUMN enabled_tiers
    SET DEFAULT ARRAY['availability', 'uplink', 'traffic', 'switch_ports', 'wireless'];

UPDATE meraki_orgs
   SET enabled_tiers = enabled_tiers || ARRAY['wireless'],
       updated_at = now()
 WHERE NOT ('wireless' = ANY (enabled_tiers));
