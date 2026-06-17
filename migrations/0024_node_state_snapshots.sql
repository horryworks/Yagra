-- Periodic node-state count snapshots for the fleet health timeline + nodes-down sparkline.
--
-- Additive (expand-contract, ADR-017). Core writes one row per state every snapshot interval;
-- a retention job trims rows older than the retention window. Cardinality is tiny (≤6 states
-- per snapshot), so a single ts index is enough for the range reads.

CREATE TABLE IF NOT EXISTS node_state_snapshots (
    ts     TIMESTAMPTZ NOT NULL DEFAULT now(),
    state  TEXT NOT NULL,            -- ok | warning | critical | unreachable | unknown | maintenance
    count  INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS node_state_snapshots_ts_idx ON node_state_snapshots (ts);
