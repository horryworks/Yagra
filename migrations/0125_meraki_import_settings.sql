-- 0125_meraki_import_settings — per-organization automatic import of Meraki devices (ADR-164 Inc.4).
--
-- reversible: four columns are added to `meraki_orgs`, each with a default. Nothing is dropped and
-- nothing is narrowed. A core from before it (v0.3.27) names its columns explicitly, never reads
-- these four, and imports nothing on its own whatever `import_devices` says, so rolling the binary
-- back leaves every row readable. No `schema_compat` floor, for the reason 0108 records: every
-- release from 0.2.2 on tolerates a database carrying migrations it does not embed.
--
-- WHAT THE SYNC DOES WITH THEM
-- The inventory sync (`meraki_sync.rs`, every `inventory_secs`) already records what the Dashboard
-- lists. With `import_devices` on it also turns a listed device into a node — but only one that is
-- in a watched network, has been reported online at least once (`meraki_inventory.first_online_at`)
-- and has never been a node here (`imported_at IS NULL`). The last condition is what keeps a device
-- an operator deleted from coming back on the next sync.
--   import_devices    the switch.
--   file_by_prefix    file a new node in the folder whose IP range holds its address, when exactly
--                     one does; otherwise under Organization ▸ Network. Off is for an organization
--                     whose sites all reuse one private range: there, a range match would gather
--                     every site's devices into one folder.
--   max_devices       the most nodes this organization may hold. The sync stops importing at it.
--   devices_over_cap  how many devices the cap left out on the last sync. Written back so the cap is
--                     never silent: the organization's page says "N devices were not imported".
--
-- WHY EXISTING ORGANIZATIONS START OFF AND NEW ONES START ON
-- Every organization that exists today was imported by hand: somebody picked which devices to
-- monitor. Turning the switch on for them would add every device they left out. So the column is
-- added with DEFAULT FALSE — which is what fills the existing rows — and the default is then moved
-- to TRUE for rows created from here on. Two statements, no UPDATE.
-- ⚠️ An organization switched on later has whatever network scope it had, which for a hand-imported
-- one is often "only the networks that were picked". Its page says how many networks are not watched
-- and offers to watch them all; nothing here changes an existing network's flag.
--
-- 🚨 THIS OVERTURNS 0040
-- 0040's header says a network the sync discovers is stored `monitored = false` "so they surface as
-- candidates and are never auto-monitored". That was the right answer while a person chose every
-- device. With `import_devices` on, the sync inserts a newly discovered network as
-- `monitored = true` — otherwise a new site would sit unmonitored until somebody opened the page,
-- which is the thing this switch exists to remove (user decision 2026-09-19: import everything
-- automatically, new networks included). An existing row's flag is still never touched, and an
-- organization with the switch off behaves exactly as 0040 describes.
-- An applied migration is checksummed and cannot be edited, so this file is where the current
-- answer is written down.

ALTER TABLE meraki_orgs ADD COLUMN IF NOT EXISTS import_devices BOOLEAN NOT NULL DEFAULT FALSE;
ALTER TABLE meraki_orgs ALTER COLUMN import_devices SET DEFAULT TRUE;

ALTER TABLE meraki_orgs ADD COLUMN IF NOT EXISTS file_by_prefix BOOLEAN NOT NULL DEFAULT TRUE;

ALTER TABLE meraki_orgs ADD COLUMN IF NOT EXISTS max_devices INTEGER NOT NULL DEFAULT 1000;
ALTER TABLE meraki_orgs DROP CONSTRAINT IF EXISTS meraki_orgs_max_devices_check;
ALTER TABLE meraki_orgs ADD CONSTRAINT meraki_orgs_max_devices_check
    CHECK (max_devices BETWEEN 1 AND 50000);

ALTER TABLE meraki_orgs ADD COLUMN IF NOT EXISTS devices_over_cap INTEGER NOT NULL DEFAULT 0;
ALTER TABLE meraki_orgs DROP CONSTRAINT IF EXISTS meraki_orgs_devices_over_cap_check;
ALTER TABLE meraki_orgs ADD CONSTRAINT meraki_orgs_devices_over_cap_check
    CHECK (devices_over_cap >= 0);
