-- Cisco Meraki organizations: the org-scoped polling + rate-limit unit for Dashboard API monitoring.
--
-- Additive (expand-contract, ADR-017): new tables only. Meraki devices are ordinary nodes (their
-- role category) discriminated by a `meraki_devices` row (mig 0039); this table holds the *org* — the
-- unit the Dashboard API is rate-limited by (~5-10 req/s per org). The scheduler runs one org-scoped
-- collector job per (org, tier) on its own cadence, pages the org-bulk endpoints once, and fans the
-- results out to many nodes by serial. An N-1 binary that doesn't know these tables simply never
-- polls Meraki — safe both ways, no data lost.
--
-- READ-ONLY integration: the poller only issues HTTP GET to `base_url` (host-allow-listed).
-- `credential_id` -> a `meraki_api` credential holding ONLY the API key (org id lives here on the
-- row), so one key can back many orgs (MSP/admin key). ON DELETE RESTRICT: you can't delete a
-- credential still bound to an org (delete/repoint the org first).
--
-- Per-tier cadence columns have their OWN bounds (deliberately not the node band's 1h cap) so slow
-- tiers (traffic/inventory) aren't blocked. `target_rps` is the conservative safeguard budget
-- (default ~2 req/s, well under the cap, headroom left for the customer's own tooling).
-- `enabled_tiers` selects which tiers run; `enabled=false` pauses the whole org (zero requests).
-- `group_id` is the org's HostTree root group (org->network hierarchy hangs under it).

CREATE TABLE IF NOT EXISTS meraki_orgs (
    id                UUID PRIMARY KEY,
    org_id            TEXT NOT NULL UNIQUE,
    name              TEXT NOT NULL,
    base_url          TEXT NOT NULL DEFAULT 'https://api.meraki.com',
    credential_id     UUID NOT NULL REFERENCES credentials (id) ON DELETE RESTRICT,
    availability_secs INTEGER NOT NULL DEFAULT 300
        CHECK (availability_secs BETWEEN 60 AND 3600),
    uplink_secs       INTEGER NOT NULL DEFAULT 300
        CHECK (uplink_secs BETWEEN 60 AND 3600),
    traffic_secs      INTEGER NOT NULL DEFAULT 1800
        CHECK (traffic_secs BETWEEN 300 AND 86400),
    inventory_secs    INTEGER NOT NULL DEFAULT 21600
        CHECK (inventory_secs BETWEEN 900 AND 604800),
    enabled_tiers     TEXT[] NOT NULL DEFAULT ARRAY['availability', 'uplink', 'traffic'],
    target_rps        DOUBLE PRECISION NOT NULL DEFAULT 2.0
        CHECK (target_rps > 0 AND target_rps <= 10),
    group_id          UUID REFERENCES node_groups (id) ON DELETE SET NULL,
    enabled           BOOLEAN NOT NULL DEFAULT true,
    last_sync_at      TIMESTAMPTZ,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at        TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS meraki_orgs_credential_idx ON meraki_orgs (credential_id);
