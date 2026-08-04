-- 0068_link_overrides — what an operator has decided about a link, against what was derived
-- (ADR-043 Increment 2, 決定 4: manual always wins).
--
-- Additive only (ADR-017 expand-contract): one new table and one new nullable column.
--
-- WHY THIS IS A SEPARATE TABLE AND NOT A FLAG ON `node_links`
-- `node_links` (0066) is a cache — every row is recomputable, and stale rows are deleted by age. An
-- operator's decision is neither: it must survive a derivation cycle in which the link was not
-- observed, and it must survive the cache being emptied entirely. Storing it in the cache would
-- mean the pruner needed an exception, and an exception in a pruner is how data quietly outlives
-- the rule that was supposed to remove it.
--
-- Instead a pinned link is re-emitted by *every* derivation run, so the ordinary staleness rule
-- never reaches it and the pruner stays a single unqualified age test. One mechanism, not two.
--
-- WHY THE ENDPOINTS ARE CANONICALLY ORDERED
-- A link is an unordered pair, so `(a,b)` and `(b,a)` are the same decision. The writer normalizes
-- to a < b before insert, which is what makes the UNIQUE index below mean anything at all; without
-- it the same hide could be stored twice and un-hiding once would appear not to work.
CREATE TABLE IF NOT EXISTS link_overrides (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    -- CASCADE here, unlike `pollers.anchor_node_id`: a decision about a link to a node that no
    -- longer exists is not a decision, it is a dangling row that would be re-applied to whatever
    -- reused the pair.
    a_node      UUID NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    b_node      UUID NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    -- `pin` | `hide` | `direction`. No CHECK — see 0067 and 0066 for why an enum column's CHECK is
    -- the constraint that makes adding a variant a schema migration. Validated at the API edge by
    -- `LinkOverrideAction::from_token`; an unknown token read back is skipped, so a row written by a
    -- newer core degrades to "no override" rather than failing the read.
    action      TEXT NOT NULL,
    -- Which endpoint is upstream, for `action = 'direction'` only: `a` or `b`. It cannot be carried
    -- by the column order, because the column order is the canonical one — that is the cost of
    -- canonicalization and this column is it.
    direction   TEXT,
    note        TEXT,
    created_by  TEXT,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- One decision of each kind per pair. `pin` and `direction` legitimately coexist (an operator
    -- asserting a link exists *and* which way it runs); two conflicting directions do not, which is
    -- why `direction` is a column of one row rather than two rows with different actions.
    UNIQUE (a_node, b_node, action)
);

-- The derivation reads every override on every run.
CREATE INDEX IF NOT EXISTS link_overrides_a_idx ON link_overrides (a_node);
CREATE INDEX IF NOT EXISTS link_overrides_b_idx ON link_overrides (b_node);

-- The resolved direction, written by the derivation onto the cached link.
--
-- Storing it here rather than re-reading `link_overrides` at projection time keeps "manual always
-- wins" a property of exactly one function (`derive::apply_overrides`). The alternative — having
-- the projection consult the override table too — would put the same rule in two places, which is
-- the shape every drift trap in this repository has had.
--
-- NULL means "no operator direction"; the projection then derives direction from distance to the
-- nearest anchor.
ALTER TABLE node_links
    ADD COLUMN IF NOT EXISTS forced_parent UUID REFERENCES nodes(id) ON DELETE SET NULL;
