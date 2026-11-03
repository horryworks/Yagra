-- 0166_event_rule_folder — an event rule can target a folder and everything under it (ADR-232).
--
-- A rule names at most one target: a node, a folder, or neither (every node). Deleting the folder
-- deletes its rules, as deleting a node already does for the rules that name it: clearing the
-- column instead would widen the rule to the whole fleet.
ALTER TABLE event_rules
    ADD COLUMN IF NOT EXISTS group_id UUID REFERENCES node_groups (id) ON DELETE CASCADE;
ALTER TABLE event_rules
    ADD CONSTRAINT event_rules_one_target CHECK (node_id IS NULL OR group_id IS NULL);
CREATE INDEX IF NOT EXISTS event_rules_group_id_idx ON event_rules (group_id);

-- An older core does not read `group_id`, so it would take a folder rule for a rule on every node
-- and fire it fleet-wide. Going back to it needs a restore from backup.
INSERT INTO schema_compat (migration_version, min_core, reason)
VALUES (
    166,
    '0.3.67',
    'event_rules.group_id targets a folder (ADR-232). A core older than 0.3.67 does not read the '
    'column and treats a folder rule as a rule on every node, so its alerts would fire fleet-wide. '
    'Returning to an earlier core needs a restore from backup.'
);
