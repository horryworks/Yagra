-- Configurable polling intervals: a global default + per-device-profile override.
--
-- Additive (expand-contract, ADR-017): a new singleton settings table + one nullable column on
-- profiles. An N-1 binary that knows neither keeps polling on its env-var interval (it never reads
-- these), so the rollout is safe in both directions and no data is lost.
--
-- Resolution at schedule time: profiles.poll_interval_secs (if set) -> app_settings.default_poll_
-- interval_secs -> (no-row / skeleton fallback) the compiled 30s default. NULL on a profile means
-- "inherit the global default". Bounds 10-3600s are re-validated at the API edge; the CHECK
-- constraints here are a backstop against a bad direct write.
--
-- The app_settings row is NOT inserted here: core seeds it at startup so it can honor the legacy
-- YAGRA_POLL_INTERVAL_SECS env var as the initial value on first boot (ON CONFLICT DO NOTHING),
-- after which the UI-edited value is authoritative.

CREATE TABLE IF NOT EXISTS app_settings (
    -- Singleton: exactly one row, id is always TRUE.
    id          BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (id),
    default_poll_interval_secs INTEGER NOT NULL DEFAULT 30
        CHECK (default_poll_interval_secs BETWEEN 10 AND 3600),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

ALTER TABLE profiles ADD COLUMN IF NOT EXISTS poll_interval_secs INTEGER
    CHECK (poll_interval_secs IS NULL OR poll_interval_secs BETWEEN 10 AND 3600);
