-- 0105_pool_takeover — remember where a pool's members came from, so taking them over is
-- reversible (ADR-107 増分 4).
--
-- reversible: additive only — one new table, nothing added to or narrowed on an existing one. An
-- older core never reads it, so rolling the binary back leaves the rows in place; what is lost is
-- the ability to put the members back automatically, and the pool values themselves are ordinary
-- `nodes.pool` / `node_groups.pool` columns an operator can still edit by hand. No `schema_compat`
-- floor, for the same reason 0089 / 0099 / 0100 / 0102 / 0103 / 0104 record: every release from
-- 0.2.2 on tolerates a database carrying migrations it does not embed, and the floor 0080 recorded
-- covers this one.
--
-- WHY A TABLE AND NOT A COLUMN
-- The obvious shape is `nodes.pool_before_takeover TEXT`, and it cannot express the majority case.
-- A node is in a pool three ways (ADR-107 増分 3): its own column, a folder it inherits from, or
-- falling through to the default — and the two that no column records are the common ones. Taking
-- an inheriting node over writes an explicit pool into a column that was NULL, so putting it back
-- means writing NULL again. A single column cannot tell "it was inheriting" from "it was never
-- taken over": both read NULL. The row says which, because its existence is the marker and
-- `previous_pool` is free to be NULL.
--
-- WHY IT HOLDS NO FOREIGN KEY TO `pools`
-- `from_pool` names the pool that lost its poller, and ADR-107's own decision is that the pool
-- list is a composite of four sources rather than a closed vocabulary — a pool can be referenced
-- while no `pools` row describes it. A foreign key here would refuse exactly the case this table
-- exists for.
CREATE TABLE IF NOT EXISTS pool_takeover (
    -- 'node' | 'group'. Not an enum: `SubjectKind` in Rust is the guard, the same choice
    -- `topology_links.sources` made — an older core skips a token it does not know rather than
    -- failing the row.
    kind          TEXT NOT NULL,
    -- The node or folder whose assignment was changed. No FK: the row is bookkeeping about a
    -- decision, and a node deleted while taken over should leave the record rather than take it
    -- with it. `restore` skips ids that no longer resolve.
    subject_id    UUID NOT NULL,
    -- The pool that had nodes and no live poller — what the operator was covering for.
    from_pool     TEXT NOT NULL,
    -- Where they were pointed instead.
    to_pool       TEXT NOT NULL,
    -- 🚨 NULL means "it was inheriting, and restoring means writing NULL back", not "unknown".
    -- See WHY A TABLE above; this nullability is the entire reason the table exists.
    previous_pool TEXT,
    taken_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- Who chose it. A takeover is a monitoring-configuration change made during an incident, and
    -- the audit row alone would not say which subjects it touched.
    taken_by      TEXT NOT NULL DEFAULT '',
    -- One outstanding takeover per subject. Taking a node over twice would lose the first
    -- `previous_pool`, which is the only copy of where it belongs.
    PRIMARY KEY (kind, subject_id)
);

-- Both reads are "what is outstanding for this pool": the restore, and the badge that tells an
-- operator a pool is being covered from elsewhere.
CREATE INDEX IF NOT EXISTS pool_takeover_from_idx ON pool_takeover (from_pool);
