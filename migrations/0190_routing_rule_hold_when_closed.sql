-- 0190_routing_rule_hold_when_closed - a routing rule with hours can keep an alert back while its
-- hours are closed, instead of passing it to the rules below (ADR-266 decision 1).
--
-- `hold_when_closed` false (every existing row) keeps what a closed rule has done since ADR-262
-- decision 14: it is passed over, and the next open rule below it, or the catch-all, decides. True
-- means that while the rule matches an alert and its hours are closed, the alert is held there and
-- sent when a rule that sends opens, if it is still open then. A rule that is always open never
-- reads it.
--
-- Declares a floor: a core older than 0.3.90 does not embed this migration and refuses to start
-- against a database that has applied it. Were it allowed to run, it would ignore the column and
-- pass a closed rule's alerts on to the rules below - the alerts this column was set to keep back.

ALTER TABLE routing_rules ADD COLUMN hold_when_closed BOOLEAN NOT NULL DEFAULT false;

INSERT INTO schema_compat (migration_version, min_core, reason)
VALUES (
    190,
    '0.3.90',
    'routing_rules.hold_when_closed keeps an alert back while its rule''s hours are closed (ADR-266). '
    'A core older than 0.3.90 does not embed this migration and refuses to start against a database '
    'that has applied it.'
);
