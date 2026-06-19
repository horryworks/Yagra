-- 0034_report_runs — generated/saved report instances (the "Saved reports" list).
--
-- One row per generated report. Generation is a TSDB+PostgreSQL-read background computation in core
-- (mirrors the analysis runner, ADR-022) — it never touches devices, so it is not a poller/bus job.
-- The rendered report is stored in PostgreSQL: `result_html` is the in-app artifact (shown in the
-- viewer, also the source for an on-demand PDF) and `result_json` is the structured per-section data
-- (feeds CSV export and any re-render). Binary exports (PDF/CSV) are generated ON DEMAND at download
-- time, not stored, so the table stays lean (no BYTEA blob). `spec_snapshot` captures the definition
-- spec at run time for reproducibility (the definition may later change or be deleted).
--
-- Additive / expand-only (ADR-017). Deleting a definition sets definition_id NULL (the saved run is
-- kept — it is a historical artifact). Retention: a loop in core prunes rows older than the window
-- (same pattern as node_state_snapshots / alert_history).

CREATE TABLE IF NOT EXISTS report_runs (
    id            UUID PRIMARY KEY,
    definition_id UUID REFERENCES report_definitions (id) ON DELETE SET NULL,
    name          TEXT NOT NULL,                 -- snapshot of the definition name (for the list)
    trigger       TEXT NOT NULL,                 -- manual | scheduled
    -- Lifecycle: queued | running | succeeded | failed.
    state         TEXT NOT NULL,
    pct           INTEGER NOT NULL DEFAULT 0,    -- 0..100 progress while running
    error         TEXT,                          -- failure reason when state = failed
    range_from    TIMESTAMPTZ,                   -- resolved report window (start)
    range_to      TIMESTAMPTZ,                   -- resolved report window (end)
    section_count INTEGER NOT NULL DEFAULT 0,    -- number of sections rendered (for the list)
    spec_snapshot JSONB NOT NULL DEFAULT '{}'::jsonb,
    result_json   JSONB,                         -- structured per-section results (CSV / re-render)
    result_html   TEXT,                          -- rendered report body (in-app viewer + PDF source)
    created_by    TEXT,                          -- username that launched it (audit; NULL for scheduled)
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    started_at    TIMESTAMPTZ,
    finished_at   TIMESTAMPTZ
);

-- The saved-reports list is "most recent first".
CREATE INDEX IF NOT EXISTS report_runs_created_idx ON report_runs (created_at DESC);
