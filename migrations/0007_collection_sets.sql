-- 0007_collection_sets — per-node/profile SNMP collection sets.
--
-- Additive (ADR-017). A collection item is defined at a scope (profile or node); the
-- scheduler resolves the effective set per node by most-specific-wins (node overrides
-- profile for the same metric_name — yagra-common::collection). When a node resolves to
-- an empty set, the scheduler falls back to the built-in catalog, so existing behaviour
-- (poll sysUpTime) is preserved.
--
-- `scope_id` is the profile UUID or the node UUID (no FK — matches thresholds' loose
-- coupling; an orphaned row after a profile delete is simply ignored at resolution).
-- `metric_name` is the stable TSDB metric label and is bounded by convention to keep
-- series cardinality controlled (ADR-011); `oid` is a scalar instance OID or a table
-- column base; `collection` is scalar|table; `metric_kind` is gauge|counter.

CREATE TABLE collection_items (
    id            UUID PRIMARY KEY,
    scope_level   TEXT NOT NULL,             -- profile | node
    scope_id      UUID NOT NULL,
    metric_name   TEXT NOT NULL,
    oid           TEXT NOT NULL,
    collection    TEXT NOT NULL,             -- scalar | table
    metric_kind   TEXT NOT NULL,             -- gauge | counter
    enabled       BOOLEAN NOT NULL DEFAULT true,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (scope_level, scope_id, metric_name)
);

CREATE INDEX collection_items_scope_idx ON collection_items (scope_level, scope_id);
