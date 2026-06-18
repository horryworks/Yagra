-- 0026_analysis_jobs — Troubleshoot deep-diagnostic jobs + their findings.
--
-- Additive / expand-only (ADR-017): two new tables, no change to existing ones. These back the
-- Troubleshoot section's background analyses (anomaly detection, event correlation, capacity
-- forecast, flap analysis). The analysis itself is a TSDB-read computation that runs in core as
-- a background task (ADR-022) — it never touches devices, so unlike discovery it does not go to
-- the poller/bus. Job metadata + findings are metadata, so they live in PostgreSQL (ADR-004).
--
-- Retention: a job row is small; findings are bounded per job. A retention/trim job can prune
-- old finished jobs later — kept simple here (no auto-trim yet).

CREATE TABLE IF NOT EXISTS analysis_jobs (
    id            UUID PRIMARY KEY,
    -- Which diagnostic: anomaly | correlation | capacity | flap.
    tool          TEXT NOT NULL,
    -- Scope the analysis ran over (resolved to a node set at run time).
    scope_kind    TEXT NOT NULL,              -- all | group | node
    scope_id      UUID,                        -- group/node id; NULL for 'all'
    scope_label   TEXT NOT NULL,               -- human label shown in the runs list
    -- Run parameters (window/baseline/depth/sensitivity/notify) as JSON for forward flexibility.
    params        JSONB NOT NULL DEFAULT '{}'::jsonb,
    -- Lifecycle: queued | running | done | failed | cancelled.
    state         TEXT NOT NULL,
    pct           INTEGER NOT NULL DEFAULT 0,  -- 0..100 progress
    phase         TEXT,                        -- current phase caption while running
    -- Result headline for the runs list (e.g. "23 anomalies · 8 nodes").
    finding_count INTEGER NOT NULL DEFAULT 0,
    summary       TEXT,
    error         TEXT,                        -- failure reason when state = failed
    created_by    TEXT,                        -- username that launched it (audit)
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    started_at    TIMESTAMPTZ,
    finished_at   TIMESTAMPTZ
);

-- The runs list is "most recent first".
CREATE INDEX IF NOT EXISTS analysis_jobs_created_idx ON analysis_jobs (created_at DESC);

CREATE TABLE IF NOT EXISTS analysis_findings (
    id          UUID PRIMARY KEY,
    job_id      UUID NOT NULL REFERENCES analysis_jobs (id) ON DELETE CASCADE,
    -- Higher = more significant (0..100); severity is derived from it client-side too.
    score       DOUBLE PRECISION NOT NULL,
    severity    TEXT NOT NULL,                 -- crit | warn | info
    node_id     UUID,                          -- the subject node (NULL for fleet-level findings)
    node_name   TEXT NOT NULL,                 -- joined name (denormalised so the report needs no join)
    metric      TEXT NOT NULL,                 -- metric / pair label
    kind        TEXT NOT NULL,                 -- anomaly shape / correlation / capacity / flap kind
    when_label  TEXT NOT NULL,                 -- human "when" (e.g. "today 03:12")
    duration    TEXT NOT NULL,                 -- human duration (e.g. "6 min", "ongoing")
    -- Per-finding detail for the report (e.g. anomaly chart series: expected/band/actual).
    detail      JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Findings are always fetched by job, ordered by score.
CREATE INDEX IF NOT EXISTS analysis_findings_job_idx ON analysis_findings (job_id, score DESC);
