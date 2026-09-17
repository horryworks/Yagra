-- 0121_wireless — wireless controllers and the access points behind them (ADR-064).
--
-- reversible: additive only — three new tables, nothing on an existing table narrowed or rewritten.
-- An older core never names them, so rolling the binary back leaves them in place, unread, and
-- rolling forward again finds them. No `schema_compat` floor is owed, for the reason 0083, 0115 and
-- 0119 record: every release from 0.2.2 on tolerates a database carrying migrations it does not
-- embed, so this does not move the oldest core that can start afterwards.
-- ⚠️ One thing a rollback does lose: an older core does not run the AP walk, so the AP list stops
-- refreshing (its `last_seen` ages) until this core returns. Nothing is deleted.
--
-- WHY THREE TABLES AND NOT ONE JSONB DOCUMENT PER CONTROLLER
-- `node_neighbors` (0062) and `node_arp` (0070) keep a node's observation as one document, replaced
-- wholesale. An AP is different in the one way that matters: it is reported by **more than one
-- node**. An HA pair reports the same 38 APs from both members (measured on the PoC: the active says
-- `normal`, the standby `standby`), and an AP's identity must survive the pair switching roles. So the
-- AP is a row of its own, keyed by its MAC-derived id, and each controller's view of it is a row
-- beside it:
--
--   wireless_controllers   one row per controller — today an SNMP AC, later a Meraki network
--   wireless_aps           one row per AP, across every controller that reports it
--   wireless_ap_sightings  one row per (AP, controller): what *that* controller said, and when
--
-- WHY `wireless_aps` IS KEYED BY `ap_id` AND NOT BY A NODE
-- An AP is listed before anyone imports it as a node (ADR-064 決定 8: import is opt-in), and a
-- Meraki MR that later joins this model already has a node whose id is random, not MAC-derived
-- (ADR-064 改訂 R3). So the node is a nullable column, not the key.
--
-- WHY NO CHECK CONSTRAINT ON `state`, `run_state`, `source` OR `flavor`
-- The reason `topology_links.sources` has none (ADR-043): a newer core writing a token an older one
-- does not know must not fail the write for every other AP in the batch. The Rust enums are the guard
-- (`WlanApState`, `WlanFlavor`), and a reader skips a token it cannot parse.
--
-- WHAT IS NEVER DELETED
-- An AP that stops being reported keeps its row (ADR-064 決定 10, as revised): its `last_seen` stops
-- advancing, and nothing is concluded from the absence (ADR-156 決定 3). A controller's row goes with
-- its node, and its sightings go with it; the AP row stays, owner-less.

CREATE TABLE IF NOT EXISTS wireless_controllers (
    -- For an SNMP controller, its node id. Not declared as a foreign key to `nodes` here because a
    -- Meraki network (a later source) has no node; `node_id` below is the foreign key.
    id                UUID PRIMARY KEY,
    -- Where the inventory comes from: 'snmp' today.
    source            TEXT NOT NULL DEFAULT 'snmp',
    -- The controller's node. CASCADE: a controller that is no longer monitored has no view to keep.
    node_id           UUID UNIQUE REFERENCES nodes(id) ON DELETE CASCADE,
    -- The vendor dialect the last inventory was read in ('huawei').
    flavor            TEXT,
    -- ── Import (ADR-064 決定 8; written from increment B2) ──
    -- Whether this controller's APs become nodes. Off by default: registering a controller must not
    -- create hundreds of nodes by itself.
    import_aps        BOOLEAN NOT NULL DEFAULT FALSE,
    -- The most APs this controller may import, and publish in one inventory.
    max_aps           INTEGER NOT NULL DEFAULT 1024 CHECK (max_aps BETWEEN 1 AND 2048),
    -- Where imported AP nodes are filed. NULL ⇒ a folder named after the controller.
    ap_group_id       UUID REFERENCES node_groups(id) ON DELETE SET NULL,
    -- How many APs the import cap left out on its last pass. Shown, never silent.
    aps_over_cap      INTEGER NOT NULL DEFAULT 0,
    -- ── The last complete inventory ──
    -- How many APs it carried, and — when the controller reported more than it could publish — how
    -- many there were.
    aps_reported      INTEGER NOT NULL DEFAULT 0,
    aps_truncated_at  INTEGER,
    last_inventory_at TIMESTAMPTZ,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at        TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS wireless_controllers_ap_group_idx ON wireless_controllers (ap_group_id);

CREATE TABLE IF NOT EXISTS wireless_aps (
    -- `yagra_common::ap_id(mac)`: UUID v5 of the six MAC bytes. The identity (ADR-064 決定 8b).
    ap_id              UUID PRIMARY KEY,
    -- Lower-case, colon-separated. Unique for the same reason the id is.
    mac                TEXT NOT NULL UNIQUE,
    -- The AP's node once imported (increment B2). SET NULL: deleting the node does not make the AP
    -- stop existing, and `imported_at` below remembers that someone chose to delete it.
    node_id            UUID UNIQUE REFERENCES nodes(id) ON DELETE SET NULL,
    -- The controller currently serving it: the last one to report it associated, while that report
    -- is fresh (`wireless::ownership`). SET NULL with the controller.
    owner_controller_id UUID REFERENCES wireless_controllers(id) ON DELETE SET NULL,
    -- Descriptive device strings, cleaned and capped at 128 characters by the poller and again at
    -- ingest. For PostgreSQL and the AP list only — never a TSDB label (ADR-011).
    name               TEXT,
    serial             TEXT,
    model              TEXT,
    sw_version         TEXT,
    ip                 INET,
    vendor_group       TEXT,
    -- What the serving controller said: its own word ('normal', 'fault', 'standby') and what that
    -- means ('associated', 'backup', 'not_associated').
    run_state          TEXT NOT NULL,
    state              TEXT NOT NULL,
    clients            INTEGER,
    -- Set when the AP was imported as a node. With `node_id` NULL it means an operator deleted the
    -- node, and the importer must not bring it back (increment B2).
    imported_at        TIMESTAMPTZ,
    first_seen         TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_seen          TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- The last time any controller reported it associated. NULL ⇒ it has never been in service, and
    -- the importer leaves it alone (ADR-064 改訂 R7, a user decision).
    last_associated_at TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS wireless_aps_owner_idx ON wireless_aps (owner_controller_id);

CREATE TABLE IF NOT EXISTS wireless_ap_sightings (
    ap_id              UUID NOT NULL REFERENCES wireless_aps(ap_id) ON DELETE CASCADE,
    controller_id      UUID NOT NULL REFERENCES wireless_controllers(id) ON DELETE CASCADE,
    -- What this controller said on its last complete inventory.
    run_state          TEXT NOT NULL,
    state              TEXT NOT NULL,
    clients            INTEGER,
    first_seen         TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_seen          TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_associated_at TIMESTAMPTZ,
    PRIMARY KEY (ap_id, controller_id)
);

-- A controller's AP list reads its sightings; the primary key leads on the AP.
CREATE INDEX IF NOT EXISTS wireless_ap_sightings_controller_idx ON wireless_ap_sightings (controller_id);
