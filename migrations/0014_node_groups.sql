-- Hierarchical node groups: organize the inventory into a folder tree.
--
-- A group has a type (Site / Region / Device type / Service / Generic), each rendered with its
-- own icon in the UI, and may nest under another group (parent_id self-FK). A node belongs to at
-- most one group (nodes.group_id); ungrouped nodes show at the tree root.
--
-- Deleting a group must never delete nodes. The API deletes a group by first re-parenting its
-- direct child groups and member nodes up to the group's own parent (NULL ⇒ root), then removing
-- the row. The ON DELETE SET NULL clauses below are a backstop so a stray delete can never orphan
-- a row or cascade into node loss.
--
-- Additive (expand) migration: existing rows get a NULL group_id (ungrouped). An older binary
-- that doesn't select group_id keeps working — N/N-1 upgrade-safe (ADR-017).
CREATE TABLE node_groups (
    id          UUID PRIMARY KEY,
    name        TEXT NOT NULL,
    group_type  TEXT NOT NULL,
    parent_id   UUID REFERENCES node_groups (id) ON DELETE SET NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX node_groups_parent_idx ON node_groups (parent_id);

ALTER TABLE nodes
    ADD COLUMN group_id UUID REFERENCES node_groups (id) ON DELETE SET NULL;

CREATE INDEX nodes_group_idx ON nodes (group_id);
