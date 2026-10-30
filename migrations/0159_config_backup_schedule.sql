-- 0159_config_backup_schedule — when config backups are fetched (ADR-210 stage 6): one start time
-- for the whole deployment, how many nodes are fetched at once, and where the latest run stands.
--
-- reversible: additive only — one new table. A core from before this migration does not name it,
-- and the oxidized configuration it writes keeps oxidized's own daily interval, which is what it
-- did before. No `schema_compat` floor, for the reason 0108 records.
--
-- `id` is a boolean that can only be true, so the table holds at most one row (decision 59: one
-- start time per deployment). The row is inserted here so the reader never has to invent one.
--
-- The time is UTC, as `report_schedules` keeps it (decision 59). `concurrency` stops at 8, the
-- `threads` the bundled oxidized runs with (decision 60).
--
-- `run_started_at` / `run_finished_at` are the latest run; `run_finished_at` is NULL while it is
-- still going. They are all that is kept (decision 66): a node with an attempt after the start is
-- done, so a restart or a failover continues the run instead of starting it again. `updated_at`
-- moves when a person changes the time, so a new time takes effect the next time it comes round
-- rather than starting a run at once for an instant that has already passed today.
CREATE TABLE config_backup_schedule (
    id               BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (id),
    at_hour          SMALLINT NOT NULL DEFAULT 2 CHECK (at_hour BETWEEN 0 AND 23),
    at_minute        SMALLINT NOT NULL DEFAULT 0 CHECK (at_minute BETWEEN 0 AND 59),
    concurrency      SMALLINT NOT NULL DEFAULT 1 CHECK (concurrency BETWEEN 1 AND 8),
    run_started_at   TIMESTAMPTZ,
    run_finished_at  TIMESTAMPTZ,
    updated_at       TIMESTAMPTZ NOT NULL DEFAULT now()
);

INSERT INTO config_backup_schedule (id) VALUES (TRUE) ON CONFLICT DO NOTHING;
