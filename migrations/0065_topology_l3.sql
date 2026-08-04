-- 0065_topology_l3 — interface addresses (ADR-043): the current address set per node, an
-- append-on-change history of it, and the two settings that govern collection.
--
-- Additive only (ADR-017 expand-contract): two new tables plus two `app_settings` columns with
-- DEFAULTs, no change to any existing column. An N-1 binary never reads the tables and never
-- selects the columns, so a rollback just leaves them idle. Nothing is dropped or retyped, so
-- there is no contract phase.
--
-- WHY THIS EXISTS AT ALL
-- Dependency suppression (ADR-015) has never fired in practice, because its only input is
-- `nodes.parent_id` and nobody fills that in by hand. ADR-043 derives the connectivity graph
-- instead, and the fact it derives it from is this table: two nodes with an address in the same
-- prefix are L3-adjacent. That is not an inference — it is what having a foot in a network means.
--
-- WHY THIS IS NOT THE ROUTING TABLE
-- The obvious way to learn what a router is next to is `ipCidrRouteTable`, which on a full-route
-- core router runs to hundreds of thousands of rows; walking it would spend the device's CPU and
-- the poll budget to answer a question `ipAddrTable` answers in tens of rows. The cost of the
-- naive design is not managed here, it is designed out. The known limit of the cheap answer is
-- point-to-point `/32`s (a PPPoE Dialer, a tunnel), which share no subnet with their peer and are
-- therefore stored but form no edge until Increment 4 resolves them through routing.
--
-- WHY ADDRESSES ARE NOT IN THE TSDB
-- Same reason adjacency is not (migration 0062): `SeriesKey` is fixed at {node, ifindex, metric}
-- (ADR-011) with no API for extra labels, and an IP address as a label is the cardinality
-- explosion CLAUDE.md §7.1 names as the project's single biggest risk. Addresses therefore live
-- here, the same tier as the interface inventory, the DNS chain and the neighbour set. Only
-- `snmp_l3_address_count` — one bounded node-level gauge — becomes a metric.
--
-- WHY THE UNIT IS THE WHOLE SET PER NODE, NOT ONE ROW PER ADDRESS
-- A partial or failed walk must never read as "this device lost its addressing" — which, one
-- derivation later, reads as "every link through this node disappeared". With one JSONB document
-- per node the poller reports the set it could observe and core replaces it atomically; a walk that
-- failed sends no set at all (`PollResult.l3 = None`) so nothing is written. An *empty* set is a
-- real observation and does replace the stored one: that is how a device whose addressing was
-- removed stops showing stale prefixes.
--
-- ABOUT `l3_key`
-- The canonical content encoding from yagra_common::L3Snapshot::content_key(): every address, its
-- prefix length, its ifIndex, its type and which table it came from. Row order is excluded (the
-- set is canonically sorted first), as is anything the agent renumbers between polls. Stored
-- verbatim rather than hashed so "why was this recorded as a change?" is answerable with a plain
-- SELECT. The encoding is versioned (`v1` first line); changing it re-keys every stored set and
-- emits one spurious change row per node.
--
-- `prev_l3_key` exists so the upsert and the conditional append are a single atomic statement: a
-- CTE whose RETURNING carries the pre-update key. PostgreSQL's row lock on ON CONFLICT DO UPDATE
-- serializes concurrent cores, so no transition is double-appended or lost. This is the same shape
-- as `node_neighbors` / `node_neighbor_changes` (0062) and `dns_chains` (0049), deliberately.
--
-- Monitoring data only — no device credentials or secrets (security.md).

CREATE TABLE IF NOT EXISTS node_l3 (
    node_id       UUID PRIMARY KEY REFERENCES nodes (id) ON DELETE CASCADE,
    l3_key        TEXT NOT NULL,
    -- The key this row held before the most recent observation; drives the conditional append.
    prev_l3_key   TEXT,
    -- The full yagra_common::L3Snapshot as observed (canonically ordered).
    addresses     JSONB NOT NULL,
    -- Denormalized so a list can show a count without parsing the document.
    address_count INTEGER NOT NULL DEFAULT 0,
    -- Whether the per-node cap was hit and rows were dropped. Surfaced rather than swallowed: a
    -- truncated view that looks complete is worse than no view.
    truncated     BOOLEAN NOT NULL DEFAULT FALSE,
    -- When this exact set was first observed (reset whenever l3_key changes).
    first_seen    TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- When it was last confirmed (bumped on every observation, like interfaces.last_seen).
    last_seen     TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS node_l3_changes (
    -- BIGSERIAL rather than a random UUID: a pure append log, so a dense monotonic id gives a cheap
    -- stable keyset-cursor tiebreaker and a tighter index in the append path (as node_neighbor_changes).
    id            BIGSERIAL PRIMARY KEY,
    node_id       UUID NOT NULL REFERENCES nodes (id) ON DELETE CASCADE,
    at            TIMESTAMPTZ NOT NULL DEFAULT now(),
    l3_key        TEXT NOT NULL,
    -- NULL on the first observation for a node (the genesis row).
    prev_l3_key   TEXT,
    addresses     JSONB NOT NULL,
    address_count INTEGER NOT NULL DEFAULT 0
);

-- History pages newest-first, keyset on (at DESC, id DESC).
CREATE INDEX IF NOT EXISTS node_l3_changes_node_at_idx
    ON node_l3_changes (node_id, at DESC, id DESC);
-- The retention pruner scans by age across all nodes.
CREATE INDEX IF NOT EXISTS node_l3_changes_at_idx ON node_l3_changes (at);
-- The derivation task reads every current row and needs a cheap freshness watermark
-- (max(last_seen)) to decide whether anything moved since the last run.
CREATE INDEX IF NOT EXISTS node_l3_last_seen_idx ON node_l3 (last_seen);

-- Collection is deployment-wide, not per node: the OIDs are fixed standards (RFC 1213 / RFC 4293),
-- so there is nothing here for an operator to tune per device — only whether to collect at all and
-- how often. Enabled by default so the Network map has edges out of the box; a device that
-- implements neither table answers the walk in one round trip and costs nothing.
--
-- Deliberately a *separate* toggle from `neighbor_discovery_enabled` rather than a shared one: a
-- fleet may have reason to collect L2 adjacency and not L3 addressing, or the reverse.
ALTER TABLE app_settings
    ADD COLUMN IF NOT EXISTS l3_discovery_enabled BOOLEAN NOT NULL DEFAULT TRUE,
    -- Same band as the neighbour cadence, for the same reason: addressing changes on the order of
    -- months, and the ceiling stops the setting being used to disable collection while still
    -- reading as enabled.
    ADD COLUMN IF NOT EXISTS l3_interval_secs INTEGER NOT NULL DEFAULT 3600
        CHECK (l3_interval_secs BETWEEN 300 AND 86400);
