-- 0191_tag_match - a routing rule and an alert-grouping exception can ask for a node carrying
-- every one of their tags, not only one of them (ADR-267 decision 1).
--
-- `tags_all` false (every existing row) keeps what a setting naming two or more tags has meant
-- since ADR-259 decision 2: a node carrying any one of them is reached. True means a node must
-- carry every one of them among its effective labels. A setting naming one tag or none reads the
-- same either way. How the tags then combine with the folders (`join_and`, ADR-262 decision 10) is
-- unchanged.
--
-- Declares a floor: a core older than 0.3.90 does not embed this migration and refuses to start
-- against a database that has applied it. Were it allowed to run, it would ignore the column and
-- read "every tag" as "any tag" - sending a rule's alerts, or grouping an exception's nodes, wider
-- than the operator asked.

ALTER TABLE routing_rules ADD COLUMN tags_all BOOLEAN NOT NULL DEFAULT false;
ALTER TABLE alert_grouping_exceptions ADD COLUMN tags_all BOOLEAN NOT NULL DEFAULT false;

INSERT INTO schema_compat (migration_version, min_core, reason)
VALUES (
    191,
    '0.3.90',
    'routing_rules.tags_all and alert_grouping_exceptions.tags_all ask for a node carrying every '
    'one of the tags (ADR-267). A core older than 0.3.90 does not embed this migration and refuses '
    'to start against a database that has applied it.'
);
