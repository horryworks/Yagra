-- 0004_thresholds — static thresholds with scope-based inheritance (ADR-013).
--
-- Additive (ADR-017). A rule is defined at a scope (profile/group/node); the alert
-- engine resolves the effective threshold per (node, metric) by most-specific-wins with
-- most-restrictive tie-break (yagra-common::thresholds). `scope_id` is the node UUID, the
-- profile UUID, or a group identifier (a node tag value), as text.

CREATE TABLE thresholds (
    id            UUID PRIMARY KEY,
    scope_level   TEXT NOT NULL,             -- profile | group | node
    scope_id      TEXT NOT NULL,
    metric        TEXT NOT NULL,
    direction     TEXT NOT NULL,             -- above | below
    warning       DOUBLE PRECISION,
    critical      DOUBLE PRECISION,
    dwell_samples INTEGER NOT NULL DEFAULT 3,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX thresholds_metric_idx ON thresholds (metric);
