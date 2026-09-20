-- 0126_meraki_availability_tier_always_on — every Meraki organization collects availability
-- (ADR-164 決定 17).
--
-- reversible: one UPDATE that appends a token to a TEXT[] column on the rows that lack it. No column
-- is added, dropped or narrowed, and a core from before it reads `enabled_tiers` exactly as it did:
-- `availability` is a tier every release since 0038 has known. Rolling the binary back leaves every
-- row readable. No `schema_compat` floor, for the reason 0108 records: every release from 0.2.2 on
-- tolerates a database carrying migrations it does not embed.
--
-- WHY
-- Since ADR-164 Inc.3 the availability tier is the only one that says whether a Meraki device is up:
-- a device the Dashboard reports offline is `Unreachable` from that tier's sample, and the uplink
-- and traffic tiers are observational. An organization saved without `availability` therefore gives
-- its nodes no liveness observation at all — they sit in `unknown`, and node-down can never fire.
-- Before Inc.3 the uplink tier said `Reachable` for every device, so such an organization looked
-- healthy; after it, it looks like nothing, with no screen saying why.
--
-- `PUT /api/v1/meraki/orgs/{id}/cadence` refuses that shape from here on
-- (`400 availability_required`), and the WebUI no longer offers the checkbox. But a stored value
-- never fixes itself: refusing the next write does nothing for a row written last month. This is
-- that half of the fix.
--
-- WHAT IT COSTS
-- One paged request to `/organizations/{id}/devices/availabilities` per `availability_secs`
-- (default 300 s) for each organization it touches — inside the rate budget 決定 4 sets.
--
-- The appended token goes first because that is where the column's own default has it
-- (`ARRAY['availability', 'uplink', 'traffic']`); the order carries no meaning to the scheduler.

UPDATE meraki_orgs
   SET enabled_tiers = ARRAY['availability'] || enabled_tiers,
       updated_at = now()
 WHERE NOT ('availability' = ANY (enabled_tiers));
