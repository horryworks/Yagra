-- Retention window for on-demand diagnostic artefacts (ADR-040 decision 2, ADR-022 follow-up).
--
-- `analysis_jobs` (with its cascading `analysis_findings`) and `rca_reports` had no pruner at all:
-- migration 0026 says "no auto-trim yet" in as many words, and 0059 later added *scheduled*
-- analyses, so the table that nothing prunes is now written on a cadence. Same story for
-- `rca_reports`, which carries a full JSONB body per generated report.
--
-- One window over both because they are one class — a diagnosis someone asked for, reproducible by
-- asking again. Deliberately NOT report_run_retention_days: that control is labelled "Report runs"
-- in Settings, and a window's name must not silently govern a second kind of data
-- (`retention.rs`'s own rule, stated at DEFAULT_REPORT_RUN_DAYS).
--
-- `monitoring_gaps` is fixed in the same change but takes the *alert-linked* window instead, so it
-- needs no column: a gap window explains why no alert fired, and is only meaningful beside the
-- history it explains.
--
-- Additive (expand, ADR-017): one DEFAULTed column on the singleton app_settings row. An N-1 core
-- names its columns explicitly in both the SELECT and the upsert, so it neither reads nor writes
-- this one and the default survives a downgrade. 90 matches the compiled DEFAULT_DIAGNOSTIC_DAYS,
-- so applying this migration changes no behaviour by itself — the first prune does, which is why
-- the release notes say so.
--
-- The CHECK is a backstop; `retention::days_in_bounds` rejects out-of-band input at the API edge.

ALTER TABLE app_settings
    ADD COLUMN diagnostic_retention_days INTEGER NOT NULL DEFAULT 90
        CHECK (diagnostic_retention_days BETWEEN 1 AND 3650);
