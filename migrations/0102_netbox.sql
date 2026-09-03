-- 0102_netbox — NetBox becomes a source the folder tree can be pulled from (ADR-100 Inc.1).
--
-- reversible: additive only — two new tables, no column added to or narrowed on an existing one.
-- An older core never reads either, so rolling the binary back leaves both in place and the folder
-- tree they populated keeps working: `node_groups` rows written by a sync are ordinary rows, with
-- nothing in them that says "NetBox owns this". That is deliberate — the ownership lives in
-- `netbox_groups`, so losing this table degrades to "these folders are now hand-maintained"
-- rather than to a tree an old binary cannot read. No `schema_compat` floor, for the same reason
-- 0089 / 0099 / 0100 record: every release from 0.2.2 on tolerates a database carrying migrations
-- it does not embed, and the floor 0080 recorded covers this one.
--
-- WHAT IS OWNED BY WHOM (ADR-100 decision 2 — the centre of the design)
-- A sync may write only the columns NetBox is the source of truth for:
--     node_groups.name / parent_id / latitude / longitude   -> NetBox owns, overwritten every sync
--     node_groups.pool                                      -> the operator owns, NEVER written
--     node_groups.sort_order                                -> Yagra owns (kept name-ordered)
-- Splitting ownership per column is what makes the sync a pure idempotent upsert: there is no
-- conflict to resolve, so there is no conflict-resolution design to get wrong. The `ON CONFLICT
-- DO UPDATE` in `netbox.rs` therefore names four columns and must never grow `pool`.
--
-- DELETION IS NOT SYNCHRONIZED (decision 5)
-- A Site that disappears from NetBox leaves its folder in place, marked. Deleting a folder
-- re-parents its child nodes (0014), so one mistaken click in an external system would silently
-- restructure the monitoring tree. `last_seen_at` below is how the mark is derived — compared
-- against the server's `last_sync_at`, which advances ONLY on a fully successful enumeration.
-- Recording a failed sync as a successful one would mark every folder as missing at once.

-- One configured NetBox deployment. Several are allowed: an MSP may front more than one.
CREATE TABLE netbox_servers (
    id                 UUID PRIMARY KEY,
    name               TEXT NOT NULL,
    -- Operator-entered, which is what makes this the first integration subject to SSRF validation
    -- (Meraki pins an allow-list of vendor hosts; ADR-034 made BigQuery's endpoint a constant).
    -- Validated at the API edge: scheme http/https, and an IP literal must not be loopback /
    -- link-local / multicast / unspecified. Private ranges are allowed — NetBox lives inside.
    base_url           TEXT NOT NULL,
    -- The sealed API token (`credentials.kind = 'netbox_token'`, ADR-018 envelope encryption).
    -- RESTRICT, not CASCADE: deleting a credential that a server still uses should fail loudly
    -- rather than leave a server row that can never authenticate again.
    credential_id      UUID NOT NULL REFERENCES credentials (id) ON DELETE RESTRICT,
    -- PEM for a private CA (ADR-100 decision 8). NOT a secret — a CA certificate is public-key
    -- material, so it is stored in the clear and may be returned by the API, unlike the token.
    -- When set, only this server's HTTP client trusts it; the process trust store is untouched.
    ca_cert_pem        TEXT,
    enabled            BOOLEAN NOT NULL DEFAULT TRUE,
    sync_interval_secs INTEGER NOT NULL DEFAULT 3600,
    -- `netbox-version` from /api/status/, recorded so the UI can state the supported range. The
    -- 3.x/4.x `device_role` -> `role` split does not reach Inc.1 (it reads no devices) but will.
    api_version        TEXT,
    -- Advanced ONLY by a fully successful enumeration. Everything about the "missing from NetBox"
    -- mark hangs off this being honest; see the header.
    last_sync_at       TIMESTAMPTZ,
    last_sync_ok       BOOLEAN,
    last_sync_error    TEXT,
    created_at         TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- The join between a NetBox object and the folder it produced. This is the only place that says a
-- folder came from NetBox, so a hand-made folder and a synced one are indistinguishable everywhere
-- else — which is what keeps the rest of the codebase from learning about this integration.
CREATE TABLE netbox_groups (
    server_id    UUID NOT NULL REFERENCES netbox_servers (id) ON DELETE CASCADE,
    -- 'region' or 'site'. Not a CHECK on purpose, matching `LinkSource`'s reasoning: the Rust enum
    -- is the guard, and an older core meeting an unknown kind should skip that row rather than
    -- fail the query. There are only ever two here in Inc.1.
    object_kind  TEXT NOT NULL,
    -- NetBox's own integer id. BIGINT because NetBox ids are unbounded in principle.
    object_id    BIGINT NOT NULL,
    -- CASCADE: if an operator deletes the folder by hand, forget the mapping. The next sync then
    -- recreates it — the id is deterministic (UUIDv5), so it comes back as the same folder rather
    -- than as a duplicate. That is the whole reason the id is derived rather than random.
    group_id     UUID NOT NULL REFERENCES node_groups (id) ON DELETE CASCADE,
    last_seen_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (server_id, object_kind, object_id)
);

-- The sweep that derives the "missing from NetBox" mark reads by server and compares timestamps.
CREATE INDEX netbox_groups_server_seen_idx ON netbox_groups (server_id, last_seen_at);
-- Resolving "is this folder NetBox-owned" from the folder's side (the tree renderer's question).
CREATE INDEX netbox_groups_group_idx ON netbox_groups (group_id);
