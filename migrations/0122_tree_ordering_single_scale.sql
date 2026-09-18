-- 0122_tree_ordering_single_scale — one ordering scale per sibling scope, shared by folders and
-- nodes (ADR-162).
--
-- reversible: this rewrites the display-order column and nothing else. It narrows no type, drops
-- nothing, and adds nothing an older binary would trip over — and it deliberately preserves the
-- order that is on screen today (every folder above every node), so a core from before ADR-162
-- renders exactly the same tree after it as before it. No `schema_compat` floor, for the reason
-- 0108 records: every release from 0.2.2 on tolerates a database carrying migrations it does not
-- embed.
--
-- WHY
-- `sort_order` has always meant "where this row sits among its siblings", and folders and nodes
-- under one parent have always BEEN siblings (`node_groups.parent_id` and `nodes.group_id` name the
-- same parent). What kept them apart was the renderer, which walked the two lists one after the
-- other — so 0015 could seed each table independently with `row_number()` starting at 1, and the
-- collision was invisible.
--
-- ADR-162 makes the tree one list, at which point every scope in every existing deployment holds a
-- folder and a node claiming the same position, and the tie is broken by name. Worse, the midpoint
-- arithmetic (`groups::placement_orders`) collapses to a no-op when two neighbours are equal: the
-- write succeeds, returns 204, and the row does not move.
--
-- So: renumber each scope as ONE integer sequence. `is_group DESC` first keeps the current visible
-- order — folders, then nodes — so nothing moves on screen; from here on, a drag is free to put a
-- folder between two nodes and the value it computes has room to land in.
--
-- PARTITION BY treats all NULL parents/groups (top-level folders, ungrouped nodes) as one scope,
-- exactly as 0015 did. That scope is not interleaved in the UI (the "Ungrouped" header keeps
-- top-level nodes apart, ADR-162 decision 6), but giving it one scale costs nothing and stops the
-- two halves colliding if that decision is ever revisited.

WITH merged AS (
    SELECT id, parent_id AS scope, TRUE  AS is_group, sort_order, name FROM node_groups
    UNION ALL
    SELECT id, group_id  AS scope, FALSE AS is_group, sort_order, name FROM nodes
),
ranked AS (
    SELECT id,
           is_group,
           row_number() OVER (
               PARTITION BY scope
               ORDER BY is_group DESC, sort_order, name, id
           ) AS rn
    FROM merged
)
UPDATE node_groups g
SET sort_order = r.rn
FROM ranked r
WHERE g.id = r.id AND r.is_group;

WITH merged AS (
    SELECT id, parent_id AS scope, TRUE  AS is_group, sort_order, name FROM node_groups
    UNION ALL
    SELECT id, group_id  AS scope, FALSE AS is_group, sort_order, name FROM nodes
),
ranked AS (
    SELECT id,
           is_group,
           row_number() OVER (
               PARTITION BY scope
               ORDER BY is_group DESC, sort_order, name, id
           ) AS rn
    FROM merged
)
UPDATE nodes n
SET sort_order = r.rn
FROM ranked r
WHERE n.id = r.id AND NOT r.is_group;
