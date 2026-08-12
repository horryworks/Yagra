-- 0081_suppression_exemptions — take one node out of a suppression it inherited.
--
-- reversible: additive (one new table, nothing narrowed). No `schema_compat` floor is owed —
-- 0080 already pins min_core 0.2.2, and an additive migration does not move the oldest core that
-- can start afterwards.
--
-- ⚠️ STATE THE ROLLBACK BEHAVIOUR PLAINLY RATHER THAN CALLING IT SAFE. An older core does not read
-- this table, so after a downgrade the exemptions stop applying and every exempted node goes back
-- to being suppressed. That direction alerts *less*, not more: an outage on such a node during a
-- rollback is not paged. The exposure is bounded by the design below — an exemption never outlives
-- the coverage it was carved out of — but it is a real gap for the length of the rollback.
--
-- WHY A TABLE AND NOT TWO COLUMNS ON `nodes`
-- `nodes` is the most-read table in the deployment (every list, page and rollup), and the natural
-- lifecycle here is a self-expiring row: the writer computes `until_at` from the coverage actually
-- in force, and the reader drops what has passed — the same lazy sweep `mutes` uses. As columns,
-- expiry would mean UPDATEing `nodes` to NULL on a read path. `ON DELETE CASCADE` also comes free.
--
-- ⚠️ NOT `nodes.suppression_opt_out` (migration 0069). That flag excludes a node from *derived
-- dependency* suppression (ADR-043) — a different mechanism with a different lifetime and a
-- different owner. Two things named "suppression opt-out" would be one grep away from being wired
-- to each other by mistake, which is why this one is spelled "exemption" everywhere.
--
-- SEMANTICS THE ENGINE APPLIES (crates/yagra-core/src/main.rs)
-- An exemption cancels only coverage the node does not own directly: a folder-group / profile /
-- tag / system window, or a group mute. A window or mute naming *this node* still applies. Without
-- that rule, an operator who released a node and then deliberately put that one node into
-- maintenance would get silence with no way to see why.
CREATE TABLE IF NOT EXISTS suppression_exemptions (
    id          UUID PRIMARY KEY,
    -- 'maintenance' or 'mute'. The two carry different permissions at the API edge
    -- (ManageMaintenance vs AckAlerts), so they are separate rows rather than one flag.
    kind        TEXT NOT NULL CHECK (kind IN ('maintenance', 'mute')),
    node_id     UUID NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    -- When the coverage this was carved out of runs out. Computed by the server from the windows
    -- and mutes actually in force, never supplied by the browser: an exemption that outlived its
    -- window would silently exclude the node from the *next* one, which is the monitoring blind
    -- spot this design exists to avoid.
    until_at    TIMESTAMPTZ NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- One exemption per node per kind, so releasing twice extends the expiry instead of stacking
    -- rows the reader would have to reconcile.
    UNIQUE (kind, node_id)
);

-- The engine reads "which nodes are exempt from <kind> right now" on every alert-config refresh.
CREATE INDEX IF NOT EXISTS suppression_exemptions_live
    ON suppression_exemptions (kind, until_at);
