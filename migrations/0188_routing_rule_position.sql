-- 0188_routing_rule_position - routing rules have an order, and the first matching rule that is open
-- decides where an alert goes (ADR-262 decision 14). A rule may hold instead of sending (`hold`).
--
-- Every existing rule is numbered in the order it was created, which is the order the list showed
-- until now. A new rule takes the next number from a sequence default, so it lists last without its
-- INSERT naming the column (the shape 0150 chose for credentials). Saving an order writes 1..n,
-- which stays below the sequence.
--
-- `hold` true means the rule sends nowhere and keeps the alert until another rule decides. It is a
-- column rather than an empty channel list because deleting a channel removes it from every rule:
-- an empty list would silently turn a rule into a holding one.
--
-- Declares a floor: a core older than 0.3.85 reads neither column, so it sends an alert to every
-- matching rule's channels - including the ones a holding rule above was placed there to keep it
-- from at night.

ALTER TABLE routing_rules ADD COLUMN position BIGINT NOT NULL DEFAULT 0;
ALTER TABLE routing_rules ADD COLUMN hold BOOLEAN NOT NULL DEFAULT false;

UPDATE routing_rules SET position = t.ord
FROM (SELECT id, row_number() OVER (ORDER BY created_at, id) AS ord FROM routing_rules) AS t
WHERE routing_rules.id = t.id;

CREATE SEQUENCE IF NOT EXISTS routing_rule_position_seq OWNED BY routing_rules.position;

SELECT setval('routing_rule_position_seq', COALESCE((SELECT max(position) FROM routing_rules), 0) + 1, false);

ALTER TABLE routing_rules
    ALTER COLUMN position SET DEFAULT nextval('routing_rule_position_seq');

INSERT INTO schema_compat (migration_version, min_core, reason)
VALUES (
    188,
    '0.3.85',
    'routing_rules.position orders the rules and routing_rules.hold keeps an alert back; the first '
    'open matching rule decides where it goes (ADR-262). A core older than 0.3.85 ignores both and '
    'sends to every matching rule, including at hours a holding rule was placed there to keep quiet.'
);
