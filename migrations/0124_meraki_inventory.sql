-- 0124_meraki_inventory — what Meraki says exists, kept between syncs (ADR-164 Inc.1).
--
-- reversible: this adds one table and two nullable columns, widens one CHECK, changes one column
-- default, and rewrites that column where it still holds the old default. Nothing is dropped and
-- nothing is narrowed. A core from before it (v0.3.27) never reads the new table or columns, reads
-- `inventory_secs` as a number it does nothing with (the inventory tier was a no-op there), and its
-- own API still refuses a value under 900 on write, so rolling the binary back leaves every row
-- readable. No `schema_compat` floor, for the reason 0108 records: every release from 0.2.2 on
-- tolerates a database carrying migrations it does not embed.
--
-- WHY A TABLE
-- Until now the only record of a Meraki device was its node. Core could not say "Meraki has three
-- devices you are not monitoring", could not tell a device an operator deleted from one it had
-- never seen, and had nowhere to notice that a monitored device is gone from the Dashboard. The
-- sync (`meraki_sync.rs`) reads the organization's whole device listing and keeps it here.
--
-- One row per (organization, serial). Three timestamps carry the decisions, and each is written
-- once at a transition rather than on every sync — a sync that finds nothing changed writes nothing:
--   first_online_at  set the first time Meraki reports the device online or alerting. A device that
--                    has never been online is registered in the Dashboard but still in its box;
--                    importing those would raise one node-down alert each.
--   missing_since    set when a COMPLETE listing no longer contains the serial, cleared when it
--                    comes back. Never set from a failed or partial sync — the transport refuses to
--                    return one (`fetch_inventory`).
--   imported_at      set when a node is created for this serial, and deliberately NOT cleared when
--                    that node is deleted: a row with `imported_at` and no `meraki_devices` row is
--                    a device an operator removed on purpose, which automatic import must not put
--                    back. This is why the column lives here and not on `meraki_devices`, whose row
--                    goes with the node (ON DELETE CASCADE).
--
-- `lan_ip` is TEXT, not INET: it is display and the input to the IP-range match, both of which take
-- the canonical text. Core parses it before storing, so a string here is always an address.
--
-- WHY `inventory_secs` MOVES
-- 0038 gave it a floor of 900 s and a default of 21600 s (six hours) for a tier that never ran.
-- It is now how often the sync runs, and therefore how long a newly connected device waits to be
-- noticed. Three paged GETs per sync is 36 requests an hour at 300 s, against a Dashboard limit of
-- 10 requests a SECOND per organization — so the default becomes five minutes and the floor one
-- (user decision 2026-09-19: as fast as the quota comfortably allows).
-- ⚠️ A row still holding 21600 is moved to 300. Nothing records whether that value was typed or
-- inherited, and until this migration it changed nothing, so nobody can have been relying on it. A
-- row holding any other value was set by an operator and is left alone.
-- ⚠️ 0038's comment on this column cannot be edited — an applied migration is checksummed — so this
-- file is where the current answer is written down.

CREATE TABLE IF NOT EXISTS meraki_inventory (
    org_id          UUID NOT NULL REFERENCES meraki_orgs (id) ON DELETE CASCADE,
    serial          TEXT NOT NULL,
    name            TEXT NOT NULL DEFAULT '',
    model           TEXT,
    product_type    TEXT NOT NULL DEFAULT '',
    network_id      TEXT NOT NULL DEFAULT '',
    lan_ip          TEXT,
    first_seen_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    first_online_at TIMESTAMPTZ,
    missing_since   TIMESTAMPTZ,
    imported_at     TIMESTAMPTZ,
    PRIMARY KEY (org_id, serial)
);

-- The outcome of the last sync, beside the `last_sync_at` 0038 already has. NULL until one has run:
-- "has not synced yet" is not "failed". `last_sync_error` holds a closed token (`MerakiSyncFailure`),
-- never upstream text — a Dashboard error body can quote the request, and the request has the key.
ALTER TABLE meraki_orgs ADD COLUMN IF NOT EXISTS last_sync_ok BOOLEAN;
ALTER TABLE meraki_orgs ADD COLUMN IF NOT EXISTS last_sync_error TEXT;

ALTER TABLE meraki_orgs DROP CONSTRAINT IF EXISTS meraki_orgs_inventory_secs_check;
ALTER TABLE meraki_orgs ADD CONSTRAINT meraki_orgs_inventory_secs_check
    CHECK (inventory_secs BETWEEN 60 AND 604800);
ALTER TABLE meraki_orgs ALTER COLUMN inventory_secs SET DEFAULT 300;

UPDATE meraki_orgs SET inventory_secs = 300 WHERE inventory_secs = 21600;
