-- 0157_config_backup_relay_ports — the port each config-backup node's relay listens on (ADR-210
-- stage 5 decisions 43-44). The bundled oxidized connects to core on this port instead of to the
-- device, and core carries the stream over the bus to the poller assigned to that device.
--
-- reversible: additive only — one new table and one sequence. A core from before this migration
-- hands oxidized the device's own address, as it did. No `schema_compat` floor, for the reason
-- 0108 records.
--
-- The SSH relay listens on `port`, the telnet relay on `port + 1`. A port is kept for as long as
-- its node exists, because oxidized may still hold a node list naming it: were it handed to another
-- node, a login meant for the first device would be sent to the second. The sequence cycles through
-- the whole range before a freed port can come round again, which is what keeps that window long.
CREATE SEQUENCE config_backup_relay_port_seq MINVALUE 0 MAXVALUE 19999 START 0 CYCLE;

CREATE TABLE config_backup_relay_ports (
    node_id      UUID PRIMARY KEY REFERENCES nodes(id) ON DELETE CASCADE,
    port         INTEGER NOT NULL UNIQUE
                 CHECK (port BETWEEN 20000 AND 59998 AND port % 2 = 0),
    assigned_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
