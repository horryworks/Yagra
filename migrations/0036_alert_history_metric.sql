-- 0036_alert_history_metric — capture WHAT fired on each alert transition (human-readable history).
--
-- Additive / expand-only (ADR-017): four new nullable columns on alert_history, nothing existing
-- changes (N-1 compatible — an older core that doesn't write these just leaves them NULL, and the
-- WebUI renders "—"). The check_id is a one-way hash of (node, metric) so the metric name cannot be
-- recovered from it; the alert engine now records the plaintext metric (and, for threshold checks,
-- the observed value / crossed bound / direction) at fire time so History reads as
-- "icmp_rtt_ms above 100 (was 450)" instead of an opaque UUID.
--
-- All columns NULL-able: pre-existing rows stay valid, liveness up/down rows carry only `metric`
-- (the liveness sentinel) with no numeric breach. No destructive change, rollback-safe.

ALTER TABLE alert_history
    ADD COLUMN IF NOT EXISTS metric          TEXT,             -- e.g. icmp_rtt_ms (liveness sentinel for up/down)
    ADD COLUMN IF NOT EXISTS observed_value  DOUBLE PRECISION, -- sample value that committed the transition
    ADD COLUMN IF NOT EXISTS threshold_value DOUBLE PRECISION, -- bound crossed for the committed severity
    ADD COLUMN IF NOT EXISTS direction       TEXT;             -- above | below (threshold checks only)
