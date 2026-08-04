-- 0070_arp_discovery — what has actually replied on the wire, and which of it nobody is monitoring
-- (ADR-043 Increment 3, 3-b).
--
-- Additive only (ADR-017 expand-contract): two new tables and two new settings columns.
--
-- WHAT THIS ANSWERS THAT NOTHING ELSE CAN
-- `node_l3` says what a device *is*; `node_neighbors` says what it is *cabled to*. Both are limited
-- to the inventory: they can only describe boxes someone already added. A router's ARP / IPv6
-- neighbour cache is the one source that names a host **nobody has told Yagra about** — which is
-- what makes "you are monitoring 9 things on this segment and there are 41 more" answerable.
--
-- WHY THIS IS THE ONLY ADR-043 WALK THAT SHIPS DISABLED
-- Every other table this ADR reads has one row per interface (tens). `ipNetToPhysicalTable` has one
-- row per *host on the network*, which on a campus distribution switch is thousands. It is the only
-- check in the ADR that costs a device real work, so it is opt-in and its cadence defaults to six
-- hours — the tier Meraki's inventory collection already established for "facts that change on the
-- order of days".

-- ── The observation: one summary per node ────────────────────────────────────
--
-- Same shape and same discipline as `node_neighbors` (0062) and `node_l3` (0065): one JSONB
-- document per node, replaced wholesale, so a partial or failed walk can never read as "every
-- endpoint left the network". The poller aggregates before publishing — a bounded entry sample plus
-- per-port totals — so this row is kilobytes, not the raw table.
--
-- ⚠️ **There is deliberately no `node_arp_changes` history table**, and that is a departure from the
-- two tables above rather than an omission. Their content is stable for months, so a transition is
-- worth a row. An ARP cache is not: entries expire on a five-minute timer, so an append-on-change
-- log would gain rows almost every cycle and answer nothing. The durable record of "when did we
-- first and last see this host" lives on `l3_discovered` below, where it belongs — keyed by the
-- endpoint rather than by the router that happened to observe it.
CREATE TABLE IF NOT EXISTS node_arp (
    node_id      UUID PRIMARY KEY REFERENCES nodes(id) ON DELETE CASCADE,
    -- The canonical content key (`ArpSummary::content_key`). Stored so an unchanged observation is
    -- recognisable without re-deriving it, exactly as `node_l3.l3_key` is.
    arp_key      TEXT NOT NULL,
    -- The whole summary: entries + per-interface counts + the truncation flag.
    summary      JSONB NOT NULL,
    -- How many discoverable endpoints the walk saw, before the sample cap. A plain column rather
    -- than a JSONB path because the node-detail badge reads it on every render.
    entry_count  INTEGER NOT NULL DEFAULT 0,
    -- Whether either cap dropped something. Surfaced, never swallowed: a truncated summary means
    -- `entry_count` is a floor, not a total.
    truncated    BOOLEAN NOT NULL DEFAULT FALSE,
    first_seen   TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_seen    TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- The endpoint sweep reads every node's summary by recency, so it can skip a fleet whose walks have
-- not moved since the last pass.
CREATE INDEX IF NOT EXISTS node_arp_last_seen_idx ON node_arp (last_seen DESC);

-- ── The finding: one row per endpoint, across the whole fleet ────────────────
--
-- WHY THE ADDRESS IS THE PRIMARY IDENTITY AND NOT (router, port, address)
-- The same host is seen by every router on its segment. Keying on the observation would produce one
-- row per (host × router) and an operator reviewing "what is unmonitored" would read the same host
-- three times. Keying on the address means `via_node` is "who told us", which is a fact about the
-- most recent observation and is allowed to change — that is why it is nullable-on-delete rather
-- than part of the key.
CREATE TABLE IF NOT EXISTS l3_discovered (
    id               UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    -- The endpoint. INET, never TEXT: v4 and v6 must compare and sort as addresses, and the sweep
    -- joins this against `nodes.address` which is already INET.
    ip               INET NOT NULL UNIQUE,
    -- Lowercase colon-separated hex, or NULL for an incomplete ARP entry. Descriptive device data —
    -- treated as untrusted text and escaped at render, like every other agent-supplied string.
    mac              TEXT,
    -- Which monitored node resolved it, and on which port. SET NULL rather than CASCADE: the
    -- endpoint did not stop existing because the router that saw it was deleted, and the next sweep
    -- re-attributes it to whoever else can see it.
    via_node         UUID REFERENCES nodes(id) ON DELETE SET NULL,
    via_ifindex      INTEGER,
    first_seen       TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_seen        TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- Set once the address becomes an inventory node, whether it was imported from here or added by
    -- hand. Reconciled by the sweep against `nodes.address`, so there is one rule and not two: the
    -- import path calls the same reconcile the sweep does rather than stamping the column itself.
    --
    -- The row is kept rather than deleted so "this endpoint is now node X" stays answerable — and so
    -- `first_seen` survives, which is the only record of how long the host was on the network before
    -- anyone monitored it.
    promoted_node_id UUID REFERENCES nodes(id) ON DELETE SET NULL
);

-- Keyset paging, newest-seen first (ADR-019 — never OFFSET). The `id` tiebreak is what makes the
-- cursor stable while the sweep is updating `last_seen` underneath the reader.
CREATE INDEX IF NOT EXISTS l3_discovered_seen_idx ON l3_discovered (last_seen DESC, id DESC);
-- Group scoping filters on the observing node, so this join runs on every scoped read.
CREATE INDEX IF NOT EXISTS l3_discovered_via_idx ON l3_discovered (via_node);

-- ── Settings ────────────────────────────────────────────────────────────────
--
-- Off by default, unlike its two siblings in 0062 and 0065. See the header: this is the walk that
-- costs the device something, so an upgrade must not quietly start issuing it against a fleet whose
-- operator never asked.
ALTER TABLE app_settings
    ADD COLUMN IF NOT EXISTS arp_discovery_enabled BOOLEAN NOT NULL DEFAULT FALSE;

-- Six hours. The same band the neighbour and interface-address cadences use, so one validator at the
-- API edge covers all three; the CHECK is the backstop, not the primary guard.
ALTER TABLE app_settings
    ADD COLUMN IF NOT EXISTS arp_interval_secs INTEGER NOT NULL DEFAULT 21600
        CHECK (arp_interval_secs BETWEEN 300 AND 86400);
