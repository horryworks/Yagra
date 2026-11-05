-- 0172_alert_history_liveness_idx - the grouping replay reads a week of node-down rows (ADR-236
-- Inc.2 decision 13).
--
-- reversible: additive only - one partial index, nothing narrowed and nothing rewritten. An older
-- core never names it, so rolling the binary back leaves it in place and unused, and rolling forward
-- again finds it. No `schema_compat` floor, for the reason 0108 records: every release from 0.2.2 on
-- tolerates a database carrying migrations it does not embed.
--
-- WHY
-- `AlertHistoryStore::liveness_since` asks for every node liveness row at or after a time, fires and
-- resolves both, oldest first. `alert_history_fires_idx` (0037) holds fires only and
-- `alert_history_check_latest_idx` (0120) leads on `check_id`, so neither serves it, and each read of
-- the alert grouping settings after its ten-minute cache expired scanned and sorted the whole table.
-- Partial on the predicate the query always carries, so it holds liveness rows only; `resolved` is
-- the second key because the query orders a fire before a resolve at the same instant.
--
-- Plain `CREATE INDEX` rather than `CONCURRENTLY`: sqlx runs each migration in a transaction, where
-- `CONCURRENTLY` is refused.
CREATE INDEX IF NOT EXISTS alert_history_liveness_idx
    ON alert_history (at_unix_ms, resolved)
    WHERE subject_kind = 'node' AND metric = '__liveness__';
