-- 0033_report_schedules — periodic execution of a report definition (preset cadence).
--
-- A schedule runs one report definition on a recurring PRESET cadence (daily / weekly / monthly at
-- a time-of-day) — no cron expression (kept simple; the WebUI offers presets). `next_run_at` is the
-- precomputed next firing instant; a 60s loop in core picks up rows whose `next_run_at` has passed,
-- enqueues a run, and recomputes `next_run_at` from the preset. Indexed on (enabled, next_run_at)
-- so the due-query is cheap.
--
-- Additive / expand-only (ADR-017). Admin-edited (Permission::ManageConfig at the edge). Deleting a
-- definition cascades to its schedules (a schedule with no definition is meaningless).

CREATE TABLE IF NOT EXISTS report_schedules (
    id            UUID PRIMARY KEY,
    definition_id UUID NOT NULL REFERENCES report_definitions (id) ON DELETE CASCADE,
    -- Cadence preset: daily | weekly | monthly.
    frequency     TEXT NOT NULL,
    -- 0=Sun .. 6=Sat for weekly (NULL otherwise).
    day_of_week   SMALLINT,
    -- 1 .. 28 for monthly (clamped to 28 so every month has the day; NULL otherwise).
    day_of_month  SMALLINT,
    -- Time-of-day to fire (UTC). 0..23 / 0..59.
    at_hour       SMALLINT NOT NULL DEFAULT 0,
    at_minute     SMALLINT NOT NULL DEFAULT 0,
    enabled       BOOLEAN NOT NULL DEFAULT TRUE,
    -- Precomputed next firing instant (the due-query compares this to now()).
    next_run_at   TIMESTAMPTZ NOT NULL,
    last_run_at   TIMESTAMPTZ,
    last_status   TEXT,                          -- outcome of the last fire (e.g. "queued", "failed")
    updated_by    TEXT,                          -- admin who last saved (audit)
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- The scheduler polls "enabled rows whose next_run_at has passed".
CREATE INDEX IF NOT EXISTS report_schedules_due_idx ON report_schedules (enabled, next_run_at);
