-- Index the event log on **event time** (`at_unix_ms`), which is now what `/events` filters,
-- orders and pages by (`events::EVENT_FILTER_WHERE`).
--
-- Why the change: the SQL path filtered and ordered on `recorded_at` (ingest time) while the
-- VictoriaLogs path filtered `_time`, which it writes from `at_unix_ms`. The same search therefore
-- returned different rows in a different order depending on whether the log store was enabled, and
-- `stats_series_sql` bucketed on event time while filtering on ingest time. Event time is the side
-- that can be unified — VictoriaLogs' `_time` is already written from `at_unix_ms` and cannot be
-- rewritten retroactively — so the SQL side moved.
--
-- Additive (expand-contract, ADR-017): indexes only, no column or data change. The existing
-- `recorded_at` indexes from 0042 are deliberately left in place — `recorded_at` is still selected
-- and returned, retention pruning still filters on it (`events` cleanup uses `recorded_at <
-- now() - retention`), and keeping them makes this migration reversible by DROP INDEX alone.

CREATE INDEX IF NOT EXISTS events_at_idx ON events (at_unix_ms DESC);
CREATE INDEX IF NOT EXISTS events_node_at_idx ON events (node_id, at_unix_ms DESC);
CREATE INDEX IF NOT EXISTS events_rule_at_idx ON events (matched_rule_id, at_unix_ms DESC);
