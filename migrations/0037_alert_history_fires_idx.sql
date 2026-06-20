-- 0037_alert_history_fires_idx — index the alert-fire scans on alert_history.
--
-- Additive / expand-only (ADR-017): a new index, no data change, safe to roll forward and
-- trivially reversible (DROP INDEX). The existing indexes are on `recorded_at` (recent-first
-- per-node history), but the analytics queries — `top_nodes_by_fires` (Top alerting nodes) and
-- `fires_by_weekday_hour` (alert calendar) — filter on `resolved = false AND at_unix_ms >= $1`
-- and were doing a full scan of a table that grows to millions of rows. A partial index on
-- at_unix_ms over only the fired rows covers both predicates.

CREATE INDEX alert_history_fires_idx
    ON alert_history (at_unix_ms)
    WHERE resolved = false;
