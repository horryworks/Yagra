-- 0045_monitoring_gaps — record each core↔poller visibility outage (Phase 3, store-and-forward).
-- Additive only (ADR-017 expand-contract): a new table, no changes to existing ones — an N-1 binary
-- ignores it entirely.
--
-- A row is written once per offline→online transition (the coordinator detects a *known* poller
-- reappearing after its heartbeats lapsed). The window [started_at, ended_at] is the span core could
-- not see the poller; if the poller was alive but partitioned, its store-and-forward buffer backfills
-- the metrics for that window on reconnect. Alerts are deliberately NOT backfilled (they resume from
-- "now"), so this table is the audit trail of "when was monitoring blind, and for how long".
--
-- Self-monitoring data only — no device credentials or secrets (security.md).
CREATE TABLE IF NOT EXISTS monitoring_gaps (
    id                 UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    poller_id          TEXT NOT NULL,
    pool               TEXT NOT NULL,
    started_at_unix_ms BIGINT NOT NULL,
    ended_at_unix_ms   BIGINT NOT NULL,
    -- Reserved for a future increment: how many results the poller backfilled for this window.
    backfilled_results BIGINT NOT NULL DEFAULT 0,
    recorded_at        TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- The Pollers page lists recent gaps newest-first, optionally filtered by pool.
CREATE INDEX IF NOT EXISTS monitoring_gaps_pool_idx ON monitoring_gaps (pool, recorded_at DESC);
