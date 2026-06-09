-- 0006_alert_history — append-only alert lifecycle log (Workstream #3).
--
-- Additive (ADR-017). One row per fire/resolve transition so the operator can see what
-- happened and when, independent of the in-memory active set. Indexed for recent-first
-- per-node queries.

CREATE TABLE alert_history (
    id          UUID PRIMARY KEY,
    node        UUID NOT NULL,
    check_id    UUID NOT NULL,
    severity    TEXT NOT NULL,        -- info | warning | critical
    state       TEXT NOT NULL,        -- ok | warning | critical | unreachable | …
    at_unix_ms  BIGINT NOT NULL,
    resolved    BOOLEAN NOT NULL,     -- false = fired, true = recovered
    recorded_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX alert_history_recent_idx ON alert_history (recorded_at DESC);
CREATE INDEX alert_history_node_idx ON alert_history (node, recorded_at DESC);
