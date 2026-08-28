-- 0100_pools — poller pools become a thing you can create, describe and delete (ADR-107).
--
-- reversible: additive only — one new table and one nullable column. An older core never reads
-- either, so rolling the binary back leaves both in place and nothing narrows. No `schema_compat`
-- floor for the same reason 0089 and 0099 record: every release from 0.2.2 on tolerates a database
-- carrying migrations it does not embed, and the floor 0080 recorded covers this one.
--
-- WHAT A POOL IS, AND WHAT THIS TABLE IS NOT
-- `migrations/0050_forward_destinations.sql` says "pools are a label on nodes and pollers, not a
-- table". That line is **applied and therefore immutable** (changing one byte of an applied
-- migration makes every existing deployment refuse to start), and it stays correct on the point it
-- was making: there is still no foreign key, and a forward destination may still legitimately name
-- a pool that has no poller yet. What changes is only that a pool may now also have a *row* — a
-- name with a description on it — so an operator can create one deliberately instead of summoning
-- it as a side effect of typing an unknown string into a node.
--
-- The assignment itself is untouched: which nodes are in a pool is still `nodes.pool` /
-- `node_groups.pool`, and which poller serves one is still what that poller reports. This table is
-- a place to hang a name, not a second answer to "who polls what". `build_pool_options` therefore
-- merges it with the three sources that already existed rather than replacing them — a pool this
-- table has never heard of must keep appearing (ADR-068), or the picker silently loses the pools an
-- N-1 core created.

CREATE TABLE IF NOT EXISTS pools (
    -- The pool name, and the primary key: a pool IS its name. It travels into the NATS subject
    -- `yagra.jobs.{pool}`, so the same one-token rule the API enforces applies here — letters,
    -- digits, `_` and `-`. The CHECK is a backstop for a direct `psql` write, not the validation
    -- (that is `validate_pool_update`, which can return a typed error the UI shows).
    name        TEXT PRIMARY KEY
                CHECK (name ~ '^[A-Za-z0-9_-]{1,63}$'),

    -- Why this pool exists, in the operator's words. Shown on the pool card and nowhere else.
    description TEXT,

    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- The username that created it, for the same reason `pollers.token_issued_by` records one.
    created_by  TEXT
);

COMMENT ON TABLE pools IS
    'Poller pools that have been named deliberately (ADR-107). NOT the record of which nodes or '
    'pollers are in a pool - that stays on nodes.pool / node_groups.pool and the poller''s own '
    'report. No foreign keys, deliberately: a pool may be referenced before it is described, and '
    'an N-1 core knows nothing about this table.';

-- Where the operator wants this poller to be, as opposed to where it says it is.
--
-- 🚨 The heartbeat must NOT write this column. `pollers.pool` is overwritten unconditionally on
-- every beat (`SEEN_UPSERT_SQL` / `SEEN_UPDATE_SQL`), which is correct — that column is the
-- poller's own report. This one is the operator's, exactly like `anchor_node_id`, and for the same
-- reason it is excluded from those two statements: a poller that is the last writer would revive
-- its own value within one beat and the recorded destination would vanish inside a minute.
--
-- The single exception is one-directional and lives in those statements: when a beat arrives whose
-- reported `pool` equals `desired_pool`, the column is cleared. That is the operator's wish having
-- come true, not the poller overruling it, and it is what makes the "移動待ち" badge disappear by
-- itself once the site has been restarted.
ALTER TABLE pollers ADD COLUMN IF NOT EXISTS desired_pool TEXT;

COMMENT ON COLUMN pollers.desired_pool IS
    'Operator-set destination pool (ADR-107). NULL = no move pending. Never written by a heartbeat; '
    'cleared by one only when the poller reports having arrived.';

-- Seed the default pool, and adopt every pool this deployment is already using so the strip does
-- not appear empty on a database that has been running for months. Descriptions are left NULL:
-- inventing one would put words in the operator's mouth, and the card renders "説明なし".
INSERT INTO pools (name, description)
VALUES ('default', NULL)
ON CONFLICT (name) DO NOTHING;

INSERT INTO pools (name)
SELECT DISTINCT trim(pool) FROM nodes
 WHERE pool IS NOT NULL AND trim(pool) <> '' AND trim(pool) ~ '^[A-Za-z0-9_-]{1,63}$'
ON CONFLICT (name) DO NOTHING;

INSERT INTO pools (name)
SELECT DISTINCT trim(pool) FROM node_groups
 WHERE pool IS NOT NULL AND trim(pool) <> '' AND trim(pool) ~ '^[A-Za-z0-9_-]{1,63}$'
ON CONFLICT (name) DO NOTHING;

INSERT INTO pools (name)
SELECT DISTINCT trim(pool) FROM pollers
 WHERE trim(pool) <> '' AND trim(pool) ~ '^[A-Za-z0-9_-]{1,63}$'
ON CONFLICT (name) DO NOTHING;
