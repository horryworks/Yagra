-- 0104_node_group_prefixes — a folder carries the IP prefixes used at that site, so a discovery
-- sweep can be aimed at it (ADR-100 decision 10 / Inc.4).
--
-- reversible: additive only — one new table, nothing added to or narrowed on an existing one. An
-- older core never reads it, so rolling the binary back leaves the rows in place and the folder
-- tree keeps working exactly as before; what is lost is the ability to aim a sweep at a folder,
-- which was not there to begin with. No `schema_compat` floor, for the same reason
-- 0089 / 0099 / 0100 / 0102 / 0103 record: every release from 0.2.2 on tolerates a database
-- carrying migrations it does not embed, and the floor 0080 recorded covers this one.
--
-- WHY THIS IS A FOLDER TABLE AND NOT A `netbox_prefixes`
-- 0102's header states the property this keeps: "`node_groups` rows written by a sync are ordinary
-- rows, with nothing in them that says NetBox owns this ... which is what keeps the rest of the
-- codebase from learning about this integration." Naming the table after the folder means
-- `GroupRepo::list()` reads a folder table, and the tree never learns that NetBox exists. Who
-- wrote a row is recorded in `netbox_server_id` — the same shape, one level down.
--
-- WHY `CIDR` AND NOT `TEXT`
-- These values become the target list of an outbound scan, so they are validated before they are
-- stored — and there is no CIDR parser in the Rust workspace at all (the only expander in the
-- repository is `web/src/lib/cidr.ts`, IPv4-only). PostgreSQL already has the type, so the write
-- is the validation. ⚠️ The writer casts through `network($n::inet)::cidr` rather than `$n::cidr`:
-- a plain `::cidr` cast REJECTS a value with host bits set (`192.168.1.5/24`), while `network()`
-- canonicalises it. Only something that is not an address at all is refused, and the sync counts
-- that row rather than failing (`SyncReport::prefixes_skipped`).
--
-- WHY DELETION IS SYNCHRONIZED HERE, WHEN ADR-100 DECISION 5 SAYS IT NEVER IS
-- Decision 5 refuses to delete a *folder* NetBox has stopped mentioning, because deleting one is
-- destructive: migration 0014 re-parents every child node and child folder to the grandparent, so
-- one mistake in an external system reshapes the tree. A prefix row has no children and nothing
-- references it. Removing it destroys nothing, so a stale-row sweep is more honest here than a
-- mark — the operator would otherwise be aimed at a subnet the source of truth no longer claims.
--
-- ⚠️ PRIMARY KEY (group_id, prefix), NOT the NetBox prefix id.
-- NetBox permits the same CIDR twice when the VRFs differ, so this collapses duplicates at one
-- site into a single row. For "which addresses should this sweep touch" that is the right answer;
-- the cost is that a row cannot be traced back to one specific NetBox object.
CREATE TABLE node_group_prefixes (
    -- The folder this prefix belongs to. CASCADE because a prefix has no meaning without it.
    group_id         UUID NOT NULL REFERENCES node_groups (id) ON DELETE CASCADE,
    prefix           CIDR NOT NULL,
    -- NetBox's own free-text description ("Matsuyama LAN"), shown beside the prefix so a person
    -- picking a sweep target reads a name rather than only a number. Never NULL: an absent
    -- description is an empty string, so no reader needs a second case.
    description      TEXT NOT NULL DEFAULT '',
    -- Which NetBox deployment wrote this row; NULL for a hand-made one (nothing creates those
    -- today). The stale-row sweep is scoped by this, so two NetBox servers cannot delete each
    -- other's rows.
    netbox_server_id UUID REFERENCES netbox_servers (id) ON DELETE CASCADE,
    -- Set on every upsert. A row whose `last_seen_at` predates the sync's own start time is one
    -- NetBox no longer mentions.
    last_seen_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (group_id, prefix)
);

-- The stale-row sweep's predicate: "everything this server wrote".
CREATE INDEX node_group_prefixes_server_idx ON node_group_prefixes (netbox_server_id);
