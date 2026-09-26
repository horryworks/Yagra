-- 0137_discovery_evidence — an unmonitored endpoint says where it was seen, and syslog/trap senders
-- that match no node are remembered (ADR-179 decisions 1 and 3).
--
-- reversible: two columns are added to `l3_discovered`, both with a default, and one table is
-- created. Nothing is dropped and nothing is narrowed. A core from before it names its columns
-- explicitly, never reads these and never writes them: its sweep keeps upserting ARP rows, which
-- read back here as rows with no evidence (the reader shows those as one ARP observation). The new
-- table is written and read by nothing older. No `schema_compat` floor, for the reason 0108 records:
-- every release from 0.2.2 on tolerates a database carrying migrations it does not embed.
--
-- WHY ONE ROW PER ADDRESS STILL
-- ADR-043 increment 3 keyed the finding by the endpoint so that one host seen by three routers is
-- one row, not three. A host seen by ARP on one router and by LLDP on a switch is the same case, so
-- the sources become evidence ON the row rather than rows of their own.

-- Every observation that made this address a candidate: `[{source, via_node, via_ifindex, port,
-- detail}]`, ordered and capped at eight by the sweep. Device-supplied text (a port name, a
-- platform, a syslog hostname) — untrusted, escaped at render like every other agent string.
ALTER TABLE l3_discovered ADD COLUMN IF NOT EXISTS evidence JSONB NOT NULL DEFAULT '[]'::jsonb;
-- The best name any source gave it (LLDP sysName, then the CDP device id, then the syslog
-- hostname). Prefilled as the node name when the operator imports the row.
ALTER TABLE l3_discovered ADD COLUMN IF NOT EXISTS name TEXT CHECK (length(name) <= 255);

-- ── Senders of passive events that no node claimed ─────────────────────────────
--
-- WHY A TABLE AND NOT A QUERY OVER `events`
-- With VictoriaLogs enabled, PostgreSQL keeps only the alert-linked events (ADR-024), so "who sent
-- syslog that matched no node" has a different answer depending on which store is on. The event
-- writer folds each batch's unattributed senders into this table instead — a few rows per batch —
-- and the discovery sweep reads it. Kept for seven days and at most ten thousand rows, pruned by the
-- sweep, the same bounds as `l3_discovered`.
--
-- Keyed by (address, kind) so a device sending both syslog and traps is two pieces of evidence.
-- Webhooks are absent: they carry no sender address to discover.
CREATE TABLE IF NOT EXISTS event_senders (
    ip          INET NOT NULL,
    kind        TEXT NOT NULL CHECK (kind IN ('syslog', 'trap')),
    -- The syslog HOSTNAME field, when there was one. Device-supplied, untrusted.
    hostname    TEXT CHECK (length(hostname) <= 255),
    first_seen  TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_seen   TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (ip, kind)
);

CREATE INDEX IF NOT EXISTS event_senders_last_seen_idx ON event_senders (last_seen DESC);
