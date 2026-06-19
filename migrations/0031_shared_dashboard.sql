-- Shared Dashboard: one global widget layout shown to all users (the customizable replacement for
-- the old fixed "Overview"). Only admins write it; everyone reads it.
--
-- Additive (expand-contract, ADR-017): a new singleton table. An N-1 binary that doesn't know it
-- simply never reads/writes it, so the rollout is safe in both directions and no data is lost. The
-- layout is opaque JSON — the WebUI owns and migrates the shape (same contract as user_dashboards).
--
-- No row is seeded: the first admin save inserts id = TRUE; until then the WebUI renders its default
-- layout (GET returns NULL).

CREATE TABLE IF NOT EXISTS shared_dashboard (
    -- Singleton: exactly one row, id is always TRUE.
    id          BOOLEAN     PRIMARY KEY DEFAULT TRUE CHECK (id),
    layout_json JSONB       NOT NULL,
    -- Username of the admin who last saved (audit/attribution; nullable for an unattributed write).
    updated_by  TEXT,
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
