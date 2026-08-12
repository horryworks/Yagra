-- 0082_alert_history_cursor_idx — the History page's keyset cursor is (recorded_at, id).
--
-- reversible: index work only. Nothing is narrowed, no column an older binary selects is removed,
-- and the replacement index has `recorded_at DESC` as its leading column — so an N-1 core, which
-- orders by `recorded_at DESC` alone, is served by it exactly as before. No `schema_compat` floor
-- is owed (0078): the oldest bootable core does not move.
--
-- Why the composite. `alert_history.recorded_at` defaults to `now()`, which in PostgreSQL is the
-- *transaction* timestamp, and `AlertHistoryStore::record_batch` writes a whole flush as ONE
-- multi-row INSERT. Every row of a flush therefore shares a `recorded_at` to the microsecond. The
-- reader paged with `recorded_at < $cursor`, so a page boundary landing inside a flush silently
-- SKIPPED that flush's remaining rows — and a fleet-wide event is exactly when a flush is large and
-- exactly when someone is reading this log. Same shape and same reason as
-- `analysis_findings_created_idx` (0058).
--
-- The old index is dropped rather than kept: `(recorded_at DESC)` is a strict prefix of the new one,
-- so keeping both would buy nothing and cost write throughput on the most-inserted table here.
--
-- Not CONCURRENTLY: sqlx runs each migration inside a transaction and no migration in this tree
-- uses `-- no-transaction`. 0037 created an index on this same table plainly. The cost is a brief
-- write lock proportional to table size; the table is pruned by retention, so it stays bounded.

CREATE INDEX alert_history_cursor_idx ON alert_history (recorded_at DESC, id DESC);
DROP INDEX alert_history_recent_idx;
