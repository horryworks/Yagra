-- 0120_alert_history_check_latest_idx — the retention prune asks "is there a newer row for this
-- check?" once per old row (ADR-158 A7).
--
-- reversible: additive only — one index, nothing narrowed and nothing rewritten. An older core never
-- names it, so rolling the binary back leaves it in place and unused, and rolling forward again finds
-- it. No `schema_compat` floor, for the reason 0108 records: every release from 0.2.2 on tolerates a
-- database carrying migrations it does not embed.
--
-- WHY
-- The prune used to delete every row older than the retention window, which took an alert that had
-- stayed open longer than that window out of the restore (`open_alerts`). It now keeps a check's
-- latest row when that row is a fire, and finds "not the latest" with a correlated EXISTS on
-- `check_id` ordered by `at_unix_ms`. Without an index leading on `check_id`, each probe reads the
-- whole table, and the prune runs every few minutes.
--
-- Plain `CREATE INDEX` rather than `CONCURRENTLY`: sqlx runs each migration in a transaction, where
-- `CONCURRENTLY` is refused.
CREATE INDEX IF NOT EXISTS alert_history_check_latest_idx
    ON alert_history (check_id, at_unix_ms DESC);
