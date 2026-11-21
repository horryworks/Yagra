-- 0187_folders_and_tags_join - a routing rule or an alert-grouping exception that names both
-- folders and tags can require both (ADR-262 decision 10).
--
-- `join_and` false (every existing row) keeps "inside one of the folders or carrying one of the
-- tags"; true means "inside one of the folders and carrying one of the tags". Inside each list one
-- entry is still enough. A row naming only folders or only tags reads the same either way.
--
-- Declares a floor: a core older than 0.3.84 does not know `join_and`, so it would read an "and"
-- rule as "or" and send that rule's channels alerts about nodes the rule leaves out.

ALTER TABLE routing_rules ADD COLUMN join_and BOOLEAN NOT NULL DEFAULT false;
ALTER TABLE alert_grouping_exceptions ADD COLUMN join_and BOOLEAN NOT NULL DEFAULT false;

INSERT INTO schema_compat (migration_version, min_core, reason)
VALUES (
    187,
    '0.3.84',
    'routing_rules.join_and and alert_grouping_exceptions.join_and make a setting naming folders '
    'and tags require both (ADR-262). A core older than 0.3.84 ignores them and would treat such a '
    'rule as either, sending its alerts for nodes it leaves out.'
);
