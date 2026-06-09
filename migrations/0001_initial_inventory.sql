-- 0001_initial_inventory — profiles, nodes, interfaces.
--
-- The first migration. Per the upgrade-safety policy (ADR-017) every migration is
-- expand-contract: this one is purely additive (create), with no destructive change.
-- Identity is UUID (stable); human-readable names are metadata, never keys (ADR-011).
--
-- NOTE: not yet executed anywhere — there is no live database. It is the authored
-- schema the Phase-1 "walking skeleton" will run against.

-- ── Device-class / profile: inherited polling templates + default thresholds ──
CREATE TABLE profiles (
    id          UUID PRIMARY KEY,
    name        TEXT NOT NULL,
    parent_id   UUID REFERENCES profiles (id) ON DELETE SET NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- ── Nodes: multi-tier nested inventory (parent_id), v4/v6 address, grouping tags ──
CREATE TABLE nodes (
    id          UUID PRIMARY KEY,
    name        TEXT NOT NULL,
    parent_id   UUID REFERENCES nodes (id) ON DELETE SET NULL,   -- nesting → dependency suppression
    address     INET NOT NULL,                                   -- IPv4 or IPv6 (never v4-only)
    profile_id  UUID REFERENCES profiles (id) ON DELETE SET NULL,
    pool        TEXT,                                            -- poller-assignment attribute (ADR-009)
    tags        JSONB NOT NULL DEFAULT '{}'::jsonb,              -- grouping (site/region/role…); RBAC scope-ready
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX nodes_parent_id_idx  ON nodes (parent_id);
CREATE INDEX nodes_profile_id_idx ON nodes (profile_id);
CREATE INDEX nodes_pool_idx       ON nodes (pool);
CREATE INDEX nodes_tags_gin_idx   ON nodes USING gin (tags);     -- tag filtering / group scoping

-- ── Interfaces: the (node, ifIndex) → metadata mapping (ADR-011) ──
-- TSDB series are keyed by (node_id, ifindex); descriptive/mutable fields live here
-- and are joined at query time. if_name/if_alias let us re-match on ifIndex renumber.
CREATE TABLE interfaces (
    node_id     UUID NOT NULL REFERENCES nodes (id) ON DELETE CASCADE,
    ifindex     INTEGER NOT NULL,
    if_name     TEXT,
    if_alias    TEXT,
    if_speed    BIGINT,                                          -- bits/sec; for utilization% (ADR-012)
    first_seen  TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_seen   TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (node_id, ifindex)
);
