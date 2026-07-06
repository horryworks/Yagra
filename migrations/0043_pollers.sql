-- 0043_pollers — durable poller inventory (ADR-009). Additive only (ADR-017 expand-contract):
-- a new table, no changes to existing ones — an N-1 binary ignores it entirely.
--
-- This is the *durable* record of which pollers exist (first/last seen, last pool/version/
-- incarnation), upserted on a throttle from heartbeats so the Pollers view can show a poller that
-- is currently offline. Liveness and node→poller assignment stay **volatile** in Redis (ADR-004);
-- they are rebuildable and must not live in PostgreSQL.
CREATE TABLE IF NOT EXISTS pollers (
    id               TEXT PRIMARY KEY,
    pool             TEXT NOT NULL DEFAULT 'default',
    first_seen       TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_seen        TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_version     TEXT,
    last_incarnation UUID
);

-- Pollers are listed and summarized per pool (the Pools summary + failover reassignment).
CREATE INDEX IF NOT EXISTS pollers_pool_idx ON pollers (pool);
