-- 0185_applies_to_folders_and_tags - folders and tags, mixed, wherever Notification delivery
-- picks the nodes a setting applies to (ADR-262 Inc.2).
--
-- 1. A routing rule can name folders as well as tags. A node matches when it is inside one of the
--    folders (at any depth) or carries one of the tags; a rule naming neither covers every node,
--    which is what every existing rule keeps meaning.
-- 2. The alert-grouping exceptions move to a table of their own that can name several folders and
--    tags at once. Each existing per-folder level becomes one exception naming that folder. When
--    several exceptions reach one node, the weakest level wins (ADR-262 decision 2). The old table
--    stays, unread, because an older core reads it.
--
-- Declares a floor: a core older than 0.3.84 does not know `routing_rules.group_ids`, so it would
-- read a rule written for one folder as a rule for every node and page that channel for the whole
-- fleet (the reasoning of 0182). It would also keep grouping by the old per-folder table, which no
-- longer changes when an exception is edited.

ALTER TABLE routing_rules ADD COLUMN group_ids UUID[] NOT NULL DEFAULT '{}';

CREATE TABLE alert_grouping_exceptions (
    id         UUID        PRIMARY KEY,
    group_ids  UUID[]      NOT NULL DEFAULT '{}',
    tags       TEXT[]      NOT NULL DEFAULT '{}',
    level      SMALLINT    NOT NULL CHECK (level BETWEEN 0 AND 3),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (cardinality(group_ids) + cardinality(tags) > 0)
);

INSERT INTO alert_grouping_exceptions (id, group_ids, tags, level, created_at)
SELECT gen_random_uuid(), ARRAY[group_id], '{}', level, created_at
FROM alert_grouping_overrides;

INSERT INTO schema_compat (migration_version, min_core, reason)
VALUES (
    185,
    '0.3.84',
    'routing_rules.group_ids narrows a rule to nodes inside its folders, and alert grouping reads '
    'alert_grouping_exceptions (ADR-262). A core older than 0.3.84 ignores the column and would '
    'send such a rule''s alerts for every node.'
);
