-- 0140_meraki_inventory_mac — keep each Meraki device's MAC, so a neighbour with no management address
-- can still be matched to it (ADR-180 増分 3, ADR-181 増分 2 ⑶).
--
-- reversible: one nullable column and one index are added. A core from before it neither reads nor
-- writes the column — its upsert names the columns it sets, so `ON CONFLICT … DO UPDATE` leaves `mac`
-- as it was — and the index only serves this version's lookup. No `schema_compat` floor, for the
-- reason 0108 records: every release from 0.2.2 on tolerates a database carrying migrations it does
-- not embed.
--
-- WHY
-- The Dashboard's LLDP listing gives no management address for an MR or an MX (measured on a real
-- organization: 1,327 MR rows and 524 MX rows, none with one), so the Neighbors tab could not say
-- whether the access point or appliance on a port is monitored. Every device in the organization's
-- device listing carries its `mac` (3,252 of 3,252), and each of those neighbour rows' chassis id
-- equalled it. The column is filled by the next inventory sync, which writes every row once because
-- the stored value (none) differs from the listed one. The index is partial on the reader's own
-- predicate, as 0139's is.
ALTER TABLE meraki_inventory ADD COLUMN IF NOT EXISTS mac TEXT;
CREATE INDEX IF NOT EXISTS meraki_inventory_mac_listed
    ON meraki_inventory (mac) WHERE missing_since IS NULL;
