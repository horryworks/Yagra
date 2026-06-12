-- Maintenance windows + mutes (alert-quality, monitoring-conventions).
--
-- maintenance_windows: planned periods during which matching nodes are treated as
-- `maintenance` — no alerts fire, existing alerts resolve, and the inventory shows the
-- state. Scoped like thresholds (node / profile / group, ADR-013) so one window can cover
-- a device class or a site.
-- mutes: ad-hoc notification silences — alerts still fire and show in the UI/history, but
-- the page is suppressed for one node (optionally one check) until a given time.

CREATE TABLE maintenance_windows (
    id          UUID PRIMARY KEY,
    name        TEXT NOT NULL,
    scope_level TEXT NOT NULL,               -- 'node' | 'profile' | 'group'
    scope_id    TEXT NOT NULL,
    starts_at   TIMESTAMPTZ NOT NULL,
    ends_at     TIMESTAMPTZ NOT NULL,
    enabled     BOOLEAN NOT NULL DEFAULT true,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE mutes (
    id          UUID PRIMARY KEY,
    node_id     UUID NOT NULL,
    check_name  TEXT,                        -- NULL = every check on the node
    until_at    TIMESTAMPTZ NOT NULL,
    reason      TEXT,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
