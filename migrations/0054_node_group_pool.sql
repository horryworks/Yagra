-- Poll-pool assignment on node groups (ADR-009/020): a folder can carry a pool that its nodes
-- inherit, so "everything under the Tokyo site is polled from the Tokyo pollers" is one setting
-- instead of one edit per node.
--
-- Resolution order is node's own `nodes.pool` > nearest ancestor group's `pool` > `default`,
-- computed in core (`poolres.rs`) rather than in SQL: `node_groups` is a small table read whole
-- once per sweep, while `nodes` is sized for tens of thousands of rows where a per-node recursive
-- walk would not be affordable.
--
-- Additive (expand-contract, ADR-017): one nullable column. An N-1 core never selects it, so a
-- rollback simply stops honouring folder pools — nodes fall back to their own value / `default`.
-- No index: the table is hundreds of rows and is always read in full.

ALTER TABLE node_groups ADD COLUMN IF NOT EXISTS pool TEXT;
