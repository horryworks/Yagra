-- 0156_config_backup_export_names — the file names config backups are pushed under (ADR-210 stage 4
-- decisions 37-41): a node's name, kept after the node is deleted, and what the destination last
-- received.
--
-- reversible: additive only — one new table and two nullable columns. A core from before this
-- migration does not name them and pushes oxidized's own tree, as it did. No `schema_compat` floor,
-- for the reason 0108 records.
--
-- `config_backup_names` has no foreign key on purpose: its rows are what is left of a node after the
-- node is deleted, so a deleted device's configuration can still be filed under its name.
CREATE TABLE config_backup_names (
    node_id  UUID PRIMARY KEY,
    name     TEXT NOT NULL,
    seen_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- The file-name map last pushed (a digest), and the commit the destination holds.
ALTER TABLE config_backup_remote ADD COLUMN pushed_layout TEXT;
ALTER TABLE config_backup_remote ADD COLUMN pushed_commit TEXT;
