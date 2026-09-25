-- 0136_netbox_sync_request — "Sync now" on a NetBox server becomes a request, and the Site ID
-- counts of the last successful run are kept on the row (ADR-172 決定 1).
--
-- reversible: four nullable columns are added to `netbox_servers`. Nothing is dropped and nothing is
-- narrowed. A core from before it names its columns explicitly, never reads these and never writes
-- them, so rolling the binary back leaves every row readable; a request an older core finds is
-- simply never run. No `schema_compat` floor, for the reason 0108 records: every release from 0.2.2
-- on tolerates a database carrying migrations it does not embed.
--
-- WHY THE ROW HOLDS THEM
-- The endpoint used to run the sync inside the HTTP request. A browser that closed the tab or
-- reloaded made nginx drop the connection to core, and the handler was dropped with it: some folders
-- written, the prefix sweep never run, and neither success nor failure recorded. Now the endpoint
-- writes the request here and answers 202, and the leader's sync loop runs it. On the row rather
-- than in the leader's memory so that any core can take the request and a leader that changes mid-way
-- hands it on — the shape 0133 gave Meraki.
--
-- WHAT THEY HOLD
-- `sync_requested_at`: when "Sync now" asked; NULL when nothing is asked for. A second press keeps
-- the first time. Cleared at the end of a run that started after it, however that run ends.
-- `sync_started_at`: the run in flight, for "Syncing…" on the page. NULL when none is running; the
-- leader's loop clears any it finds when it starts, since a run never outlives the process running it.
-- `last_sync_sites` / `last_sync_sites_without_site_id`: the Site ID outcome of the last successful
-- run. It used to exist only in the response, so leaving the page lost it.
ALTER TABLE netbox_servers ADD COLUMN IF NOT EXISTS sync_requested_at TIMESTAMPTZ;
ALTER TABLE netbox_servers ADD COLUMN IF NOT EXISTS sync_started_at TIMESTAMPTZ;
ALTER TABLE netbox_servers ADD COLUMN IF NOT EXISTS last_sync_sites INTEGER
    CHECK (last_sync_sites >= 0);
ALTER TABLE netbox_servers ADD COLUMN IF NOT EXISTS last_sync_sites_without_site_id INTEGER
    CHECK (last_sync_sites_without_site_id >= 0);
