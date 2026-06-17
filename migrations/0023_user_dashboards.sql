-- Per-user "My Dashboard" layout storage.
--
-- Additive (expand-contract, ADR-017): a new table only, no change to existing schema, so
-- an N-1 binary that doesn't know about dashboards keeps working. One row per user holds the
-- opaque widget-layout JSON (the backend never parses widget types — the WebUI owns the shape
-- and migrates it client-side). Deleted on CASCADE when the user is removed, so no orphans.

CREATE TABLE IF NOT EXISTS user_dashboards (
    user_id     UUID PRIMARY KEY REFERENCES users (id) ON DELETE CASCADE,
    layout_json JSONB NOT NULL,
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
