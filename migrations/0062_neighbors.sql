-- 0062_neighbors — CDP/LLDP adjacency (ADR-038): the current neighbour set per node, an
-- append-on-change history of it, and the two settings that govern collection.
--
-- Additive only (ADR-017 expand-contract): two new tables plus two `app_settings` columns with
-- DEFAULTs, no change to any existing column. An N-1 binary never reads the tables and never
-- selects the columns, so a rollback just leaves them idle. Nothing is dropped or retyped, so
-- there is no contract phase.
--
-- WHY ADJACENCY IS NOT IN THE TSDB
-- A neighbour is `local port → remote chassis id → remote port id`. `SeriesKey` is fixed at
-- {node, ifindex, metric} (ADR-011) with no API for extra labels, and a chassis id is unbounded
-- device-supplied text — putting it in VictoriaMetrics would be the cardinality explosion
-- CLAUDE.md §7.1 names as the project's single biggest risk. Adjacency therefore lives here, the
-- same tier as the interface inventory and the DNS chain. Only `snmp_neighbor_count` — one bounded
-- node-level gauge — becomes a metric.
--
-- WHY THE UNIT IS THE WHOLE SET PER NODE, NOT ONE ROW PER ADJACENCY
-- A partial or failed walk must never read as "every link disappeared". With one JSONB document
-- per node the poller reports the set it could observe and core replaces it atomically; a walk that
-- failed sends no set at all (`PollResult.neighbors = None`) so nothing is written. A row-per-
-- adjacency table would need an explicit "this set is complete" flag to get the same guarantee,
-- plus diff logic in core — more moving parts for the same answer. Note that an *empty* set is a
-- real observation and does replace the stored one: that is how an unplugged switch stops showing
-- stale peers.
--
-- ABOUT `neighbor_key`
-- The canonical content encoding from yagra_common::NeighborSet::content_key(): every adjacency's
-- identity (local port, remote chassis, remote port) plus its payload (sysName, sysDescr,
-- management address, platform, capabilities). `lldpRemTimeMark`, `lldpRemIndex`, row order and
-- every TTL/age counter are DELIBERATELY EXCLUDED — `timeMark` advances on each agent refresh and
-- `remIndex` is renumbered at the agent's discretion, so keying on either would append a history
-- row on every single poll and turn this table into a poll log. (ADR-033's DNS chain avoided the
-- identical failure by excluding TTLs and round-robin ordering.) Stored verbatim rather than hashed
-- so "why was this recorded as a change?" is answerable with a plain SELECT. The encoding is
-- versioned (`v1` first line); changing it re-keys every stored set and emits one spurious change
-- row per node.
--
-- `prev_neighbor_key` exists so the upsert and the conditional append are a single atomic
-- statement: a CTE whose RETURNING carries the pre-update key. PostgreSQL's row lock on
-- ON CONFLICT DO UPDATE serializes concurrent cores, so no transition is double-appended or lost.
-- This is the same shape as `dns_chains` / `dns_chain_changes` (migration 0049), deliberately.
--
-- Monitoring data only — no device credentials or secrets (security.md).

CREATE TABLE IF NOT EXISTS node_neighbors (
    node_id           UUID PRIMARY KEY REFERENCES nodes (id) ON DELETE CASCADE,
    neighbor_key      TEXT NOT NULL,
    -- The key this row held before the most recent observation; drives the conditional append.
    prev_neighbor_key TEXT,
    -- The full yagra_common::NeighborSet as observed (canonically ordered).
    neighbors         JSONB NOT NULL,
    -- Denormalized so the node list can show a count without parsing the document.
    neighbor_count    INTEGER NOT NULL DEFAULT 0,
    -- Whether the per-node cap was hit and rows were dropped. Surfaced rather than swallowed: a
    -- truncated view that looks complete is worse than no view.
    truncated         BOOLEAN NOT NULL DEFAULT FALSE,
    -- When this exact set was first observed (reset whenever neighbor_key changes).
    first_seen        TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- When it was last confirmed (bumped on every observation, like interfaces.last_seen).
    last_seen         TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS node_neighbor_changes (
    -- BIGSERIAL rather than a random UUID: a pure append log, so a dense monotonic id gives a cheap
    -- stable keyset-cursor tiebreaker and a tighter index in the append path (as dns_chain_changes).
    id                BIGSERIAL PRIMARY KEY,
    node_id           UUID NOT NULL REFERENCES nodes (id) ON DELETE CASCADE,
    at                TIMESTAMPTZ NOT NULL DEFAULT now(),
    neighbor_key      TEXT NOT NULL,
    -- NULL on the first observation for a node (the genesis row).
    prev_neighbor_key TEXT,
    neighbors         JSONB NOT NULL,
    neighbor_count    INTEGER NOT NULL DEFAULT 0
);

-- The node-detail history view pages newest-first, keyset on (at DESC, id DESC).
CREATE INDEX IF NOT EXISTS node_neighbor_changes_node_at_idx
    ON node_neighbor_changes (node_id, at DESC, id DESC);
-- The retention pruner scans by age across all nodes.
CREATE INDEX IF NOT EXISTS node_neighbor_changes_at_idx ON node_neighbor_changes (at);

-- Collection is deployment-wide, not per node: the OIDs are fixed standards (LLDP-MIB /
-- CISCO-CDP-MIB), so there is nothing here for an operator to tune per device — only whether to
-- collect at all and how often. Enabled by default so the Neighbors tab works out of the box; a
-- device that speaks neither protocol answers the walk in one round trip and costs nothing.
ALTER TABLE app_settings
    ADD COLUMN IF NOT EXISTS neighbor_discovery_enabled BOOLEAN NOT NULL DEFAULT TRUE,
    -- Floor of 5 minutes: adjacency changes on the order of months, and a tighter cadence would
    -- spend device time and rate-limit budget re-reading a constant. Ceiling of 24 hours so the
    -- setting cannot be used to disable collection while still reading as enabled.
    ADD COLUMN IF NOT EXISTS neighbor_interval_secs INTEGER NOT NULL DEFAULT 3600
        CHECK (neighbor_interval_secs BETWEEN 300 AND 86400);
