-- 0163_delivery_hours — notifications reach people only inside the hours a site keeps (ADR-226).
--
-- Four things, all additive:
--   * a folder's time zone, kept in two columns so NetBox and a person never write the same one
--     (decision 6): `netbox_time_zone` is written by the sync from the Site's own `time_zone`, and
--     `time_zone` by a person. NetBox's value wins whenever it has one.
--   * the deployment's fallback zone, for a node no folder above it names one for (decision 5).
--   * the named schedules (decision 3) and the folders and labels each one is applied to
--     (decision 4). The targets are arrays rather than a join table: a folder deleted later leaves
--     an id that matches nothing, which is harmless, and there is no foreign key to index.
--   * the notifications held outside those hours, so a restart does not lose what was due to be
--     sent in the morning (decision 7).
--
-- reversible: additive only — two nullable columns on `node_groups`, one on `app_settings`, and
-- two new tables. An older core never names any of them, so rolling the binary back keeps every
-- folder, setting and alert as it was; it simply delivers every notification at once again, which
-- is the direction ADR-226 accepts for N-1. No `schema_compat` floor, for the reason 0108 records.
--
-- Zone names are IANA names checked at the API edge against the zone database core was built
-- with, not here: a CHECK would have to list every zone (the reason 0160 gives).
ALTER TABLE node_groups ADD COLUMN IF NOT EXISTS time_zone TEXT;
ALTER TABLE node_groups ADD COLUMN IF NOT EXISTS netbox_time_zone TEXT;
ALTER TABLE app_settings ADD COLUMN IF NOT EXISTS default_time_zone TEXT;

CREATE TABLE IF NOT EXISTS delivery_hours (
    id           UUID PRIMARY KEY,
    name         TEXT NOT NULL,
    enabled      BOOLEAN NOT NULL DEFAULT TRUE,
    -- ISO weekdays, 1 = Monday .. 7 = Sunday. Never empty: a schedule open on no day would hold
    -- every notification for good, and the API refuses it.
    days         SMALLINT[] NOT NULL,
    -- Minutes after local midnight. A start later than the end spans midnight; equal means the
    -- whole of each listed day.
    start_minute SMALLINT NOT NULL CHECK (start_minute BETWEEN 0 AND 1439),
    end_minute   SMALLINT NOT NULL CHECK (end_minute BETWEEN 0 AND 1439),
    group_ids    UUID[] NOT NULL DEFAULT '{}',
    labels       TEXT[] NOT NULL DEFAULT '{}',
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS held_notifications (
    id          BIGSERIAL PRIMARY KEY,
    -- `fire`, `resolve` or `suppress`: what the notifier was handed.
    event       TEXT NOT NULL,
    -- The alert's subject and check, which is what a released fire is matched back to the live
    -- alert by — severity is left out because it may have moved while the fire was held.
    subject     TEXT NOT NULL,
    check_id    UUID NOT NULL,
    node_id     UUID,
    -- The alert as the notifier was handed it, so a held resolve can be sent without the engine.
    alert       JSONB NOT NULL,
    held_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- When the node's hours next open, as computed at hold time. Display only: the release loop
    -- asks the live schedules, so an edit to them moves the release without touching this row.
    release_at  TIMESTAMPTZ
);
CREATE INDEX IF NOT EXISTS held_notifications_subject_idx ON held_notifications (subject, check_id);
