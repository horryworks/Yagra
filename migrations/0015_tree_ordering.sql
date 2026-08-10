-- Manual sort order for the inventory tree (drag-to-reorder).
--
-- Groups and nodes previously displayed in name order only. To let operators arrange the tree
-- by hand (HoTTY-style drag reorder), each group and each node carries a fractional sort_order
-- within its sibling scope (group_groups by parent_id; nodes by group_id). Reordering inserts a
-- value *between* neighbours (midpoint), so a single drag updates one row — no sibling renumber.
--
-- Expand-only (ADR-017): additive columns with a DEFAULT, so existing rows and the previous code
-- (which never names sort_order on INSERT) keep working — N/N-1 upgrade-safe.
--
-- reversible: the UPDATEs write only to the two columns this migration just added, so no value an
-- older binary reads is rewritten (ADR-050 decision 7).

ALTER TABLE node_groups ADD COLUMN sort_order DOUBLE PRECISION NOT NULL DEFAULT 0;
ALTER TABLE nodes ADD COLUMN sort_order DOUBLE PRECISION NOT NULL DEFAULT 0;

-- Seed existing rows from their current (name) order so the first render is stable and the
-- midpoint inserts that follow have distinct, well-spaced neighbours to interpolate between.
-- PARTITION BY treats all NULL parents/groups (top-level groups, ungrouped nodes) as one scope.
UPDATE node_groups g
SET sort_order = s.rn
FROM (
    SELECT id, row_number() OVER (PARTITION BY parent_id ORDER BY name, id) AS rn
    FROM node_groups
) s
WHERE g.id = s.id;

UPDATE nodes n
SET sort_order = s.rn
FROM (
    SELECT id, row_number() OVER (PARTITION BY group_id ORDER BY name, id) AS rn
    FROM nodes
) s
WHERE n.id = s.id;
