-- 0032_report_definitions — reusable report templates (Dashboard → Reports).
--
-- A report definition is the saved, reusable description of a report: its name and an opaque
-- JSONB `spec` (the selected sections + their settings + report-level params like the time range).
-- Reports are a SHARED resource (everyone reads, only admins write — same model as
-- shared_dashboard, ADR-017), so there is no per-user owner column; the write authorization is
-- enforced at the API edge (Permission::ManageConfig).
--
-- Additive / expand-only (ADR-017): a new table. An N-1 binary that doesn't know it simply never
-- reads/writes it, so the rollout is safe both ways and no data is lost. Unlike the dashboard the
-- backend DOES parse `spec` at generation time (it must know which sections to render), but it
-- tolerates unknown fields/sections so a newer WebUI shape stays compatible.

CREATE TABLE IF NOT EXISTS report_definitions (
    id          UUID PRIMARY KEY,
    name        TEXT NOT NULL,
    description TEXT,
    -- Opaque report document: { version, params:{ range_secs, ... }, sections:[{ id, kind, settings }] }.
    spec        JSONB NOT NULL DEFAULT '{}'::jsonb,
    -- Username of the admin who last saved (audit/attribution; nullable for an unattributed write).
    updated_by  TEXT,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- The templates list is ordered by name.
CREATE INDEX IF NOT EXISTS report_definitions_name_idx ON report_definitions (name);
