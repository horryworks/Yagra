-- 0133_meraki_full_sync — a whole-organization read: asked for, and how far it has got (ADR-164
-- 決定 30〜32).
--
-- reversible: four nullable columns are added to `meraki_orgs`. Nothing is dropped and nothing is
-- narrowed. A core from before it names its columns explicitly, never reads these and never writes
-- them, so rolling the binary back leaves every row readable; a request an older core finds is
-- simply never run. No `schema_compat` floor, for the reason 0108 records: every release from 0.2.2
-- on tolerates a database carrying migrations it does not embed.
--
-- WHY THE ROW HOLDS THEM
-- "Sync now" re-reads the whole organization — every MX network's VLANs, one network at a time —
-- which takes minutes, not seconds. So the endpoint no longer runs it: it writes the request here
-- and answers 202, and the leader's sync loop runs it once the organization's slow lane is free.
-- Kept in the database rather than in the leader's memory so that any core can take the request and
-- a leader that changes mid-way hands it on rather than losing it.
--
-- WHAT THEY HOLD
-- `full_sync_requested_at`: when "Sync now" asked; NULL when nothing is asked for. Cleared when the
-- read ends, however it ends — a second press while one runs is the same request.
-- `full_sync_started_at` / `full_sync_networks` / `full_sync_read`: the read in flight, the number
-- of networks it reads and how many it has tried so far — for "Sync now" and for an organization's
-- first read alike, so the page can say how far it has got. NULL when none is running; the leader's
-- loop clears any it finds when it starts, since a read never outlives the process running it.
ALTER TABLE meraki_orgs ADD COLUMN IF NOT EXISTS full_sync_requested_at TIMESTAMPTZ;
ALTER TABLE meraki_orgs ADD COLUMN IF NOT EXISTS full_sync_started_at TIMESTAMPTZ;
ALTER TABLE meraki_orgs ADD COLUMN IF NOT EXISTS full_sync_networks INTEGER
    CHECK (full_sync_networks >= 0);
ALTER TABLE meraki_orgs ADD COLUMN IF NOT EXISTS full_sync_read INTEGER
    CHECK (full_sync_read >= 0);
