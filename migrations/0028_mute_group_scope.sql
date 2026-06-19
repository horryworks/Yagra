-- Group-scoped mutes (All Nodes right-click → Mute a folder group, recursive).
--
-- Mutes were node-only. A folder group can now be muted: every node under the group
-- (incl. subgroups) is silenced. Expand-contract / N-1 safe (ADR-017): purely additive
-- columns plus relaxing node_id to nullable. Existing rows default to scope_kind='node',
-- so an N-1 core keeps reading them unchanged; group-scoped rows are only written by the
-- new core, so the old core never encounters a NULL node_id.

ALTER TABLE mutes ADD COLUMN scope_kind TEXT NOT NULL DEFAULT 'node';  -- 'node' | 'group'
ALTER TABLE mutes ADD COLUMN group_id UUID;                            -- set when scope_kind='group'
ALTER TABLE mutes ALTER COLUMN node_id DROP NOT NULL;                  -- NULL for group-scoped mutes
