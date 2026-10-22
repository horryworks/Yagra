-- 0154_config_backup_scan_mode — how fast a node tries its CLI logins (ADR-210 stage 3 decision 27):
-- `rapid`, `normal` or `slow`, each a set of lockout limits.
--
-- reversible: additive only — one defaulted column. A core from before this migration does not name
-- it and applies the limits it was built with, which are `normal`'s. No `schema_compat` floor, for
-- the reason 0108 records.
--
-- A token the Rust enum guards rather than a CHECK, for the reason `topology_links.sources` gives;
-- a token this core does not know is enforced as the most careful mode.
ALTER TABLE node_config_backup ADD COLUMN scan_mode TEXT NOT NULL DEFAULT 'normal';
