-- 0071_routing_adjacency — the links that share no subnet (ADR-043 Increment 4).
--
-- Additive only (ADR-017 expand-contract): one new table and two new settings columns.
--
-- WHAT THIS ANSWERS THAT NOTHING ELSE CAN
-- Increment 1 derives a link from two nodes holding an address in the same prefix. Three real link
-- classes never satisfy that and are therefore structurally invisible to it:
--
--   * a point-to-point `/32` — a PPPoE `Dialer` or an `ip unnumbered` interface shares a subnet with
--     nothing, not even with the peer `/32` at the other end of the same cable;
--   * an unnumbered OSPF link, which has no address of its own to share;
--   * an eBGP session across a segment whose members' addressing has not been observed.
--
-- `node_routing` holds the evidence for all three. It does not replace the shared-subnet rule; it
-- fills the holes the shared-subnet rule cannot reach, and where both see the same pair the result
-- is one link carrying both sources (`node_links.sources`, 0066).
--
-- WHY THE ROUTING TABLE IS STILL NOT WALKED
-- `inetCidrRouteTable` runs to hundreds of thousands of rows on a router carrying a full table, and
-- a *bounded* walk of it is worse than none: the bound would return the numerically-first routes,
-- which are never the interesting ones. The table's index begins `(destType, dest, …)`, so the
-- collection instead roots a subtree walk at one destination and gets back that destination's
-- routes and nothing else — tens of round trips, whatever the table's size. The scale problem is
-- designed out exactly as Increment 1 designed it out by reading `ipAddrTable` instead.
--
-- WHY THERE IS NO `node_routing_changes` HISTORY TABLE
-- A departure from `node_neighbors` (0062) and `node_l3` (0065), and a deliberate one — the same
-- call 0070 made for `node_arp`, for a different reason. Their content is stable for months, so a
-- transition is worth a row. What an operator actually asks about a routing adjacency is "how long
-- have these two been connected", and `node_links.first_seen` (0066) already answers it, keyed by
-- the *pair* rather than by whichever end happened to report. A per-node history here would be a
-- second, worse answer to a question already answered, and it would be noisy: `bgpPeerState` moves
-- whenever a session flaps. That is also why the session state is recorded on the row but excluded
-- from the content key — a flap is not a topology change.

-- ── The observation: one snapshot per node ───────────────────────────────────
--
-- Same shape and same discipline as `node_neighbors` and `node_l3`: one JSONB document per node,
-- replaced wholesale, so a partial or failed collection can never read as "every peer disappeared".
-- A failed walk publishes nothing at all (`PollResult.routing = None`) and this table is not
-- touched; an empty snapshot is a real observation and does replace the stored one.
CREATE TABLE IF NOT EXISTS node_routing (
    node_id          UUID PRIMARY KEY REFERENCES nodes(id) ON DELETE CASCADE,
    -- The model's own content key (`RoutingSnapshot::content_key`, versioned `v1`). Stored rather
    -- than recomputed so the reader and the writer cannot disagree about what counts as a change.
    routing_key      TEXT NOT NULL,
    -- The whole snapshot: protocol, peer address, local ifIndex and the protocol's state value.
    adjacencies      JSONB NOT NULL,
    adjacency_count  INTEGER NOT NULL DEFAULT 0,
    -- Whether a cap was hit, so a partial read of a large peering mesh is never published as the
    -- whole picture.
    truncated        BOOLEAN NOT NULL DEFAULT FALSE,
    -- "How long has this node had this set of adjacencies" — held across an unchanged observation,
    -- reset when the content key moves.
    first_seen       TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_seen        TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- The derivation's change signal: `max(last_seen)` over this table. `config_gen` (ADR-026) moves on
-- an operator edit, which is exactly what a poll does not do, so the watermark is the other half.
CREATE INDEX IF NOT EXISTS node_routing_seen_idx ON node_routing (last_seen DESC);

-- ── Settings ────────────────────────────────────────────────────────────────
--
-- On by default, unlike the ARP walk in 0070 and like the two walks in 0062 and 0065. The tables
-- read here are sized by the device's own peering mesh (tens to hundreds of rows), not by the size
-- of the network, and the route probes are bounded by construction — so this does not carry the
-- cost that made ARP opt-in.
ALTER TABLE app_settings
    ADD COLUMN IF NOT EXISTS routing_discovery_enabled BOOLEAN NOT NULL DEFAULT TRUE;

-- One hour, matching the neighbour and interface-address cadences: a peering relationship changes on
-- the order of weeks, and the session *state* it carries is not what this is for. The same band, so
-- one validator at the API edge covers every walk; the CHECK is the backstop, not the primary guard.
ALTER TABLE app_settings
    ADD COLUMN IF NOT EXISTS routing_interval_secs INTEGER NOT NULL DEFAULT 3600
        CHECK (routing_interval_secs BETWEEN 300 AND 86400);
