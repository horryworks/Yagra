-- 0139_neighbor_lookup_indexes — answer the Neighbors tab's two per-address lookups without a scan
-- (ADR-180 増分 2).
--
-- reversible: two indexes are created. Nothing is added to or removed from any table, so a core from
-- before it reads and writes `wireless_aps` and `meraki_inventory` exactly as it did; the indexes only
-- make some of its reads faster. No `schema_compat` floor, for the reason 0108 records: every release
-- from 0.2.2 on tolerates a database carrying migrations it does not embed.
--
-- WHY
-- Every open Neighbors tab refreshes every 15 s and asks, for each peer's management address, whether
-- a wireless controller reports an access point there (`WirelessRepo::aps_at`, `a.ip = ANY(…)`) and
-- whether a Meraki organization lists a device there (`MerakiInventoryRepo::devices_at`,
-- `i.lan_ip = ANY(…) AND i.missing_since IS NULL`). Neither column was indexed — 0121 indexes
-- `owner_controller_id`, 0124's key is `(org_id, serial)` — and both tables grow with the fleet (one
-- real Meraki organization lists some 1,700 access points). The second is partial on the reader's own
-- predicate, so a device the organization stopped listing costs the index nothing. Database tests in
-- `wireless.rs` and `meraki_inventory.rs` check the planner can use each one.
CREATE INDEX IF NOT EXISTS wireless_aps_ip
    ON wireless_aps (ip);
CREATE INDEX IF NOT EXISTS meraki_inventory_lan_ip_listed
    ON meraki_inventory (lan_ip) WHERE missing_since IS NULL;
