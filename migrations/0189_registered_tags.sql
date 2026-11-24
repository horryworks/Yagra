-- 0189_registered_tags - tags an Admin registers by name before anything carries them (ADR-263
-- Inc.2 decision 8).
--
-- Until now a tag existed only where it was used: a node's or a folder's labels, a refusal, a
-- routing rule, a grouping exception. A registered tag is listed on Nodes > Tags even when nothing
-- carries it, and the tag fields offer it as a suggestion. It does not restrict what may be typed.
--
-- The rename and the delete on Nodes > Tags change this table in the same transaction as the six
-- columns a tag lives in (`yagra-base/src/tag_catalog.rs`), so a registered name never outlives the
-- tag it was renamed away from.

CREATE TABLE registered_tags (
    tag        TEXT        PRIMARY KEY,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

INSERT INTO schema_compat (migration_version, min_core, reason)
VALUES (
    189,
    '0.3.88',
    'registered_tags holds the tags an Admin registers on Nodes > Tags (ADR-263 Inc.2). A core older '
    'than 0.3.88 does not embed this migration and refuses to start against a database that has '
    'applied it.'
);
