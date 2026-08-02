-- 0059_analysis_schedules — run a Troubleshoot analysis on a recurring preset cadence.
--
-- Additive / expand-only (ADR-017): one new table, nothing existing touched.
--
-- The cadence columns are deliberately identical to `report_schedules` (0033) — same preset
-- vocabulary, same `next_run_at` precomputation, same `(enabled, next_run_at)` due index — because
-- one function computes the next firing instant for both (`crate::cadence`). What differs is what
-- fires: a report schedule points at a *definition row*, while an analysis schedule carries the
-- launch spec itself (tool + scope + params), mirroring `analysis_jobs`.
--
-- That is why there is no foreign key here. A report schedule cascades from its definition; an
-- analysis schedule's `scope_id` is polymorphic — a node id for `scope_kind = 'node'`, a folder
-- group id for `'group'`, NULL for `'all'` — and no single FK can express that. A schedule whose
-- target has since been deleted resolves to an empty node set and produces a run that finds
-- nothing, which is visible in the runs list rather than silent.
--
-- `params` holds the same blob as `analysis_jobs.params` (window/baseline/sensitivity/depth/
-- family/notify). It is re-validated through the API's clamps at *fire* time, not trusted as
-- stored: the bounds are edge validation, and a schedule saved before a bound changed must not
-- keep firing outside the new one.

CREATE TABLE IF NOT EXISTS analysis_schedules (
    id            UUID PRIMARY KEY,
    -- What to run, and over what — the launch spec (mirrors analysis_jobs).
    tool          TEXT NOT NULL,
    scope_kind    TEXT NOT NULL,              -- all | group | node
    scope_id      UUID,                       -- group/node id; NULL for 'all'
    scope_label   TEXT NOT NULL,              -- human label, shown in the schedules list
    params        JSONB NOT NULL DEFAULT '{}'::jsonb,
    -- Cadence preset: daily | weekly | monthly (see crate::cadence).
    frequency     TEXT NOT NULL,
    day_of_week   SMALLINT,                   -- 0=Sun .. 6=Sat for weekly (NULL otherwise)
    day_of_month  SMALLINT,                   -- 1 .. 28 for monthly (NULL otherwise)
    at_hour       SMALLINT NOT NULL DEFAULT 0,
    at_minute     SMALLINT NOT NULL DEFAULT 0,
    enabled       BOOLEAN NOT NULL DEFAULT TRUE,
    -- Precomputed next firing instant (the due-query compares this to now()).
    next_run_at   TIMESTAMPTZ NOT NULL,
    last_run_at   TIMESTAMPTZ,
    -- Outcome of the last attempt: queued | busy | error (see AnalysisScheduleStatus). `busy` is
    -- the one reports has no equivalent for — the analysis runner has admission control, so a fire
    -- can be refused, and a refused fire deliberately leaves next_run_at alone so the next tick
    -- retries instead of skipping a whole period.
    last_status   TEXT,
    updated_by    TEXT,                       -- operator who last saved (audit)
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- The scheduler polls "enabled rows whose next_run_at has passed".
CREATE INDEX IF NOT EXISTS analysis_schedules_due_idx ON analysis_schedules (enabled, next_run_at);
