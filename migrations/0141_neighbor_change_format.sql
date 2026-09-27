-- 0141_neighbor_change_format — mark a neighbour-history row that only the spelling moved (ADR-182).
--
-- reversible: one column is added, NOT NULL with a constant default, so the add is a catalogue
-- change and rewrites no row. A core from before it neither reads nor writes the column — its
-- append names the columns it sets, so its rows take the default. No `schema_compat` floor, for the
-- reason 0108 records: every release from 0.2.2 on tolerates a database carrying migrations it does
-- not embed.
--
-- WHY
-- A neighbour set is compared by its content key, which holds the port names and ids as written.
-- When a producer changes how it writes one — ADR-181 増分 4 made a Meraki port read "Port 7"
-- instead of "7" — every node it feeds appends one history row on the next read, with nothing
-- recabled and nothing saying why. The set now carries the producer's format number
-- (`NeighborSet.format`, kept inside the `neighbors` document), and the row appended when that number
-- moved is marked here so the Neighbors tab can say "recorded differently after an upgrade".
ALTER TABLE node_neighbor_changes
    ADD COLUMN IF NOT EXISTS format_changed BOOLEAN NOT NULL DEFAULT FALSE;
