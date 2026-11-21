-- 0186_routing_rule_when_and_catch_all - delivery hours move onto the routing rules, and a
-- catch-all takes what no open rule does (ADR-262 Inc.3).
--
-- 1. A routing rule says when it is open: `when_mode` 0 = always, 1 = during the window, 2 = outside
--    it. The window is ISO weekdays plus a wall-clock span in minutes after local midnight, read in
--    the node's own zone (the folder zones of ADR-226 decision 5 stay where they are). A start later
--    than the end spans midnight; equal start and end means the whole of each listed day. Every
--    existing rule keeps meaning "always".
-- 2. `app_settings.catch_all_channel_ids`: the channels an alert goes to when no rule that matches it
--    is open, or no rule matches it at all. Empty means none: an alert whose matching rules are all
--    closed is then held until one opens.
-- 3. `delivery_hours` is no longer read by the notifier. The table stays (an older core reads it, and
--    the screen lists what it holds so an operator can rebuild it as rules) - ADR-262 decision 6.
--
-- Declares a floor: a core older than 0.3.84 does not know `when_mode`, so it would send a rule's
-- alerts at hours the rule is closed and never reach the catch-all. The first is the direction that
-- floods.

ALTER TABLE routing_rules
    ADD COLUMN when_mode    SMALLINT   NOT NULL DEFAULT 0 CHECK (when_mode BETWEEN 0 AND 2),
    ADD COLUMN days         SMALLINT[] NOT NULL DEFAULT '{}',
    ADD COLUMN start_minute SMALLINT   NOT NULL DEFAULT 0 CHECK (start_minute BETWEEN 0 AND 1439),
    ADD COLUMN end_minute   SMALLINT   NOT NULL DEFAULT 0 CHECK (end_minute BETWEEN 0 AND 1439);

ALTER TABLE app_settings ADD COLUMN catch_all_channel_ids UUID[] NOT NULL DEFAULT '{}';

INSERT INTO schema_compat (migration_version, min_core, reason)
VALUES (
    186,
    '0.3.84',
    'routing_rules.when_mode closes a rule outside its hours and app_settings.catch_all_channel_ids '
    'takes what no open rule does (ADR-262). A core older than 0.3.84 ignores both and would send a '
    'closed rule''s alerts at any hour.'
);
