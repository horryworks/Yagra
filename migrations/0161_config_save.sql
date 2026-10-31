-- 0161_config_save — saving a device's running configuration from Yagra (ADR-222).
--
-- reversible: additive only — three new tables. A core from before this migration does not name
-- them, the bundled oxidized it configures has no save hook, and nothing it reads changes. No
-- `schema_compat` floor, for the reason 0108 records.
--
-- `config_saves` is one row per save a person asked for or Yagra made on its own (decision 7, 8):
-- `pending` until the fetch that precedes it succeeds and the save script claims it, `running`
-- while the script is logged in, then `saved` or `failed`. `startup_before` is the startup
-- configuration the device held just before the save (decision 6), with oxidized's secret
-- filter applied — the record of what the save replaced. Only the newest ten per node are kept.
--
-- `requested_by` is the account that pressed the button, NULL for a save Yagra made itself. Not a
-- foreign key: the row is history and outlives the account, as `audit_log` does.
--
-- `config_unsaved` is what the save script last saw on a device Yagra cannot ask over SNMP
-- (decision 4): `since` is when it first saw running and startup differ, NULL while they agree.
-- Core turns it into `config_unsaved_seconds` on that node's poll results.
CREATE TABLE config_saves (
    id              UUID PRIMARY KEY,
    node_id         UUID NOT NULL REFERENCES nodes (id) ON DELETE CASCADE,
    origin          TEXT NOT NULL CHECK (origin IN ('person', 'auto')),
    requested_by    UUID,
    requested_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    state           TEXT NOT NULL CHECK (state IN ('pending', 'running', 'saved', 'failed')),
    failure         TEXT,
    startup_before  TEXT,
    finished_at     TIMESTAMPTZ
);

CREATE INDEX config_saves_node_idx ON config_saves (node_id, requested_at DESC);

CREATE TABLE config_unsaved (
    node_id     UUID PRIMARY KEY REFERENCES nodes (id) ON DELETE CASCADE,
    since       TIMESTAMPTZ,
    checked_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Automatic save (decision 8): off by default; when on, a device unsaved for `after_hours` is saved
-- right after its next successful fetch, outside a maintenance window. One row per deployment.
CREATE TABLE config_save_settings (
    id           BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (id),
    auto_save    BOOLEAN NOT NULL DEFAULT false,
    after_hours  SMALLINT NOT NULL DEFAULT 24 CHECK (after_hours BETWEEN 1 AND 720),
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);

INSERT INTO config_save_settings (id) VALUES (TRUE) ON CONFLICT DO NOTHING;
