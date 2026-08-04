-- 0067_topology_anchor — where each poller sits, and how the derived graph is allowed to be used
-- (ADR-043 Increment 2).
--
-- Additive only (ADR-017 expand-contract): three new columns with defaults, no change to any
-- existing value. An N-1 binary never reads them; an N-1 core reading a row written by an N core
-- simply ignores the extra columns.
--
-- WHY A POLLER NEEDS AN ADDRESS AT ALL
-- The derived graph (0066) is *undirected* — "these two boxes are adjacent" carries no notion of
-- upstream. Direction comes from one place only: a node's parents are the nodes immediately before
-- it on the path from a poller. So the graph has roots if and only if core knows where the pollers
-- are, and until this migration it did not: `pollers` recorded id/pool/version/incarnation and
-- nothing about location, and the heartbeat carried no address either.
--
-- Without roots the projection produces a graph in which every node is a root, nothing has a
-- parent, and therefore nothing is ever suppressed — while every screen looks like the feature is
-- working. That failure is the reason `anchor_node_id` exists and the reason an unresolved anchor
-- blocks derived suppression outright rather than degrading quietly.
--
-- WHY BOTH COLUMNS, AND WHY THE MANUAL ONE IS NOT A FALLBACK
-- `mgmt_addrs` is what the poller reports about itself. It is enough on a poller that runs on the
-- monitored network. It is **not** enough in the single most common deployment: a poller in a
-- container reports its bridge address (172.18.0.x), which shares a subnet with nothing in the
-- inventory. `anchor_node_id` is the operator naming the node the poller sits behind, and it wins
-- outright — it is the answer for the normal case, not a repair for an unusual one.
ALTER TABLE pollers
    -- Reported each heartbeat, replaced wholesale. INET[] rather than TEXT[] so the database
    -- rejects a malformed address at write time and both families store natively.
    ADD COLUMN IF NOT EXISTS mgmt_addrs INET[] NOT NULL DEFAULT '{}',
    -- ON DELETE SET NULL, not CASCADE: deleting the node a poller was anchored to must not delete
    -- the poller. It leaves the anchor unresolved, which is visible and blocking, rather than
    -- removing the poller's inventory row as a side effect of an unrelated edit.
    ADD COLUMN IF NOT EXISTS anchor_node_id UUID REFERENCES nodes(id) ON DELETE SET NULL;

-- HOW THE DERIVED GRAPH MAY BE USED
--
-- Three states, deployment-wide (ADR-043 決定 5 sets the unit of approval at the mode, not at the
-- edge — approving edges one at a time is the input cost this ADR exists to remove):
--
--   manual   the alert engine uses `nodes.parent_id`, as it always has. The default, and what an
--            upgrade lands on: a migration must never change what gets suppressed.
--   shadow   the engine still uses the manual graph; core additionally computes what the derived
--            graph *would* have suppressed, and shows the operator both directions. Nothing about
--            alerting changes.
--   derived  the engine uses the derived graph.
--
-- No CHECK constraint on the value, deliberately, and for the same reason `node_links.sources` has
-- none: a CHECK on an enum column is what turns "add a variant" into a DROP CONSTRAINT / ADD
-- CONSTRAINT migration, and that is how the forward-destination kind reached 21 files. The token is
-- validated at the API edge by `TopologyMode::from_token`, which is also the only place that knows
-- the set, and an unknown token read back from the database falls back to `manual` — the state that
-- changes nothing.
ALTER TABLE app_settings
    ADD COLUMN IF NOT EXISTS topology_mode TEXT NOT NULL DEFAULT 'manual';
