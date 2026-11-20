-- 0182_routing_rule_tags - a routing rule can name the tags a node must carry (ADR-259).
--
-- Empty means every node, which is what every existing rule keeps meaning. A rule with tags fans
-- out only for an alert whose node carries at least one of them among its effective labels.
--
-- Declares a floor: a core older than 0.3.83 does not know the column, so it would read a rule
-- written for one tag as a rule for every node and page that channel for the whole fleet. Widening
-- a rule is the direction that floods, so the older core is kept from starting rather than trusted
-- to ignore the column (the reasoning of ADR-232 decision 6). Returning to an earlier core needs a
-- restore from backup.

ALTER TABLE routing_rules ADD COLUMN tags TEXT[] NOT NULL DEFAULT '{}';

INSERT INTO schema_compat (migration_version, min_core, reason)
VALUES (
    182,
    '0.3.83',
    'routing_rules.tags narrows a rule to nodes carrying one of its tags (ADR-259). A core older '
    'than 0.3.83 ignores the column and would send such a rule''s alerts for every node.'
);
