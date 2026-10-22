-- 0152_config_backup — which nodes the bundled oxidized fetches a config from, and what it last
-- reported for each (ADR-210 stage 2).
--
-- reversible: additive only — two new tables, nothing on an existing table changes. A core from
-- before this migration names neither table, so rolling the binary back leaves the rows unread and
-- oxidized keeps whatever node list it last loaded. No `schema_compat` floor, for the reason 0108
-- records.
--
-- `node_config_backup` — ONE ROW PER NODE A PERSON CONFIGURED
-- No row means the node is not fetched, so the default for every existing node is "off" without a
-- backfill. `input` is a token the Rust enum guards rather than a CHECK, for the reason
-- `topology_links.sources` gives: a token a newer core adds must not make an older core's write
-- fail. The default is SSH alone: telnet sends the login in cleartext, so it is used only for a
-- node a person set to `telnet` or `ssh_then_telnet` — never because port 22 happened to be
-- blocked. `model_override` is the oxidized model a person picked; NULL means "from the profile".
-- The credential is a `cli_login` (checked at the API edge, not here); deleting it leaves the node
-- configured but unfetchable rather than deleting the person's choice. `bound_address` is the
-- node's address when the configuration was last saved: the source hands the login over only while
-- the node still has that address, so an account that may change a node's address but not choose a
-- login (one restricted to folders) cannot redirect a login someone else chose to a host it runs.
--
-- `config_backup_status` — WHAT OXIDIZED LAST SAID
-- Copied from oxidized's node list by the leader every five minutes, so the screen still shows the
-- last result while oxidized is down. Observed state, not configuration: the configuration bundle
-- carries neither table.
CREATE TABLE node_config_backup (
    node_id        UUID PRIMARY KEY REFERENCES nodes (id) ON DELETE CASCADE,
    enabled        BOOLEAN NOT NULL DEFAULT false,
    credential_id  UUID REFERENCES credentials (id) ON DELETE SET NULL,
    input          TEXT NOT NULL DEFAULT 'ssh',
    model_override TEXT,
    bound_address  INET,
    updated_at     TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX node_config_backup_credential_idx ON node_config_backup (credential_id);

CREATE TABLE config_backup_status (
    node_id     UUID PRIMARY KEY REFERENCES nodes (id) ON DELETE CASCADE,
    status      TEXT NOT NULL,
    last_end    TIMESTAMPTZ,
    observed_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
