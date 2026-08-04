-- 0066_topology_links — the derived connectivity graph (ADR-043): materialized edges plus what the
-- last derivation run declined to turn into one.
--
-- Additive only (ADR-017 expand-contract): two new tables, no change to any existing column. An N-1
-- binary never reads them, so a rollback just leaves them idle.
--
-- THIS TABLE IS A CACHE, NOT A SOURCE OF TRUTH
-- Every row here is recomputable from `node_l3` and `node_neighbors` at any time. Losing it costs
-- one derivation cycle, not data — which is why there is no history table beside it, unlike the
-- observation tables that feed it. `first_seen` is the one column that cannot be recomputed, and
-- it is the reason `link_key` is versioned: re-keying loses "how long has this link been here"
-- silently, where a re-keyed observation set merely emits one spurious history row.
--
-- WHY THE PRIMARY KEY IS A SURROGATE
-- The identity of a link is its `link_key`, and that has a UNIQUE index. The primary key is a plain
-- BIGSERIAL because Increment 3 attaches ARP-discovered endpoints that are not inventory nodes, and
-- that changes what the identity tuple contains. Changing a UNIQUE index later is additive;
-- changing a primary key is not.
--
-- WHY THE ENDPOINTS ARE NULLABLE FROM DAY ONE
-- Increment 1 never writes a NULL — a test pins that. They are nullable now because widening a
-- *column* later is free while widening a published *DTO* from `Uuid` to `Option<Uuid>` is a
-- breaking API change, with a Breaking-changes release note and a `schema.d.ts` ripple through
-- every client. The cost is one null check; the alternative is a breaking change on a schedule
-- nobody chose.
--
-- WHY `sources` IS TEXT[] WITH NO CHECK
-- One physical link seen by LLDP, by CDP and by shared-subnet membership is one row with three
-- sources, not three rows — the identity of a link is the node pair, and per-port adjacency already
-- lives in `node_neighbors` (0062). A CHECK constraint on the values is deliberately absent: a
-- `dest_kind` CHECK is exactly what makes "add a forwarding destination" a 21-file change, and
-- Increments 2-4 each add a source. The Rust enum is the guard; unknown tokens are skipped on read.
--
-- Monitoring data only — no device credentials or secrets (security.md).

CREATE TABLE IF NOT EXISTS node_links (
    id         BIGSERIAL PRIMARY KEY,
    -- "v1|n:<lower-uuid>|n:<upper-uuid>", endpoints canonically ordered so an undirected link has
    -- exactly one representation. Versioned; see the header.
    link_key   TEXT NOT NULL UNIQUE,
    a_node     UUID REFERENCES nodes (id) ON DELETE CASCADE,
    b_node     UUID REFERENCES nodes (id) ON DELETE CASCADE,
    -- The ports, when a source named them. LLDP/CDP name a physical port; a shared-subnet edge can
    -- only name the SVI, so the stronger source wins where both are present.
    a_if       INTEGER,
    b_if       INTEGER,
    a_if_name  TEXT,
    b_if_name  TEXT,
    -- Every source that produced this link. No CHECK, deliberately — see the header.
    sources    TEXT[] NOT NULL DEFAULT '{}',
    -- The subnet behind an `l3_subnet` edge, e.g. `192.168.1.0/24`, so "why is this edge here" is
    -- answerable without re-deriving anything.
    subnet     TEXT,
    first_seen TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_seen  TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- The map and the scope filter both start from an endpoint.
CREATE INDEX IF NOT EXISTS node_links_a_idx ON node_links (a_node);
CREATE INDEX IF NOT EXISTS node_links_b_idx ON node_links (b_node);
-- The staleness prune scans by age. Links are deleted per-row once they stop being re-derived,
-- never wholesale: the observations that feed them arrive at different times per node, so a
-- rebuild-from-empty on a cycle where one node's document was momentarily missing is migration
-- 0062's "everything disappeared" failure at graph scale.
CREATE INDEX IF NOT EXISTS node_links_last_seen_idx ON node_links (last_seen);

-- One row. What the last derivation run saw, including everything it declined to turn into a link.
--
-- These counters are not decoration: each one is a place the derivation chose to stay silent rather
-- than guess, and without them an incomplete graph is indistinguishable from a complete one.
-- `unmatched_lldp_rows` in particular is the acceptance instrument for the half of ADR-043 that
-- cannot be tested in this lab — the day a switch is racked, the number moves with no code change.
CREATE TABLE IF NOT EXISTS topology_derivation (
    id         BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (id),
    derived_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- yagra_common::TopologyLinkSummary
    summary    JSONB NOT NULL DEFAULT '{}'::jsonb,
    -- How many links the run produced, so the API can report the graph size without counting rows.
    link_count INTEGER NOT NULL DEFAULT 0
);
