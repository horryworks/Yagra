-- 0109_node_tags_are_labels — a node's tags become a set of value-only labels (ADR-135 inc. 2).
--
-- 0001 declared `tags JSONB NOT NULL DEFAULT '{}'` as a key->value map, and ADR-135 shipped the
-- first writer for it. The same day, the shape was rejected: an operator wanting to say "JAPAN"
-- had to keep spelling `region=` identically on every node, and one typo in the key made a
-- different tag. A label is one string.
--
-- WHY THIS IS SAFE TO CHANGE AT ALL
-- No released version carries a tag in any shape. `git tag --contains f079bfb7` is empty and
-- v0.3.16 is that commit's ancestor, so the only rows this can find are the ones a config-bundle
-- import wrote (the column's only writer for its whole life before ADR-135) and whatever the two
-- verification boxes hold. The conversion below is therefore a formality on every real deployment
-- and is written anyway, because "it should be empty" is not the same as looking.
--
-- WHY TEXT[] AND NOT A JSONB ARRAY
-- sqlx decodes `Vec<String>` from `text[]` natively, so `repo::node_from_row` loses the `Json<>`
-- wrapper instead of keeping it around a shape it no longer needs. The precedent in this schema is
-- `topology_links.sources TEXT[]` (0066), and it was chosen there for the same reason: a set of
-- short tokens is an array, not a document.
--
-- NO CHECK CONSTRAINT, deliberately
-- Same call 0066/0075/0077/0105 made and the same one 0108 wrote out at length: the length and
-- character rules are enforced at the API edge, where a violation can answer 400 with a message.
-- A CHECK here answers 500 through `from_internal`, and adding one later to a deployment that
-- already holds a violating row is a migration that fails — which is a core that will not start.
--
-- 🚨 THE CONVERSION CAN PRODUCE A LABEL THE NEW VALIDATOR REFUSES, AND THAT IS ON PURPOSE
-- The old value limit was 128 code points and nothing ever rejected a control character; the new
-- label limit is 64. A row converted from a longer value reads back fine and renders fine, and the
-- edit dialog then cannot be saved at all — with a 400 naming a label the operator never typed.
-- The alternative was `left(value, 64)`, which is silent data loss inside a migration, so this
-- does NOT truncate. The repair lives where the operator can see it: the chip input marks an
-- over-long chip, and `validated_labels` runs only on the submitted list, so deleting that chip
-- makes the save succeed.
--
-- 🚨 THE GIN INDEX IS DROPPED AND NOT RE-CREATED
-- `nodes_tags_gin_idx` has existed since 0001 and has never served a query: no statement in the
-- workspace has ever put a JSONB operator against this column. It is less useful after this change
-- rather than more — once a folder's labels are inherited by its whole subtree (0110), "which
-- nodes carry label X" is not answerable from this column at all, because the inherited half is
-- resolved in core and never stored. So the index would be write amplification on every node
-- UPDATE, including the bulk-tag path, in exchange for nothing. Re-creating it later is a one-line
-- purely additive migration that owes no floor, unlike this column.
--
-- ⚠️ THE MIGRATION AND THE BINARY MUST SHIP IN ONE IMAGE
-- The moment this runs, an older `node_from_row` fails at `try_get` on every node row — that read
-- backs the node list, the scheduler sweep and the alert-config rebuild. There is no intermediate
-- state in which the migration alone is safe, which is why this declares a floor below.

-- Add beside, convert, then swap. `ALTER COLUMN ... USING` cannot hold a subquery or a
-- set-returning function, and `jsonb_each_text` is both.
ALTER TABLE nodes ADD COLUMN tags_v2 TEXT[] NOT NULL DEFAULT '{}';

-- The object shape every release up to v0.3.16 could write: keep the VALUES, drop the keys. That
-- is not a lossy choice made here — it is what the product already did with them. Both the
-- threshold engine's `ScopeLevel::Group` and the maintenance `WindowScope::Group` matched on
-- `tags.values()` and discarded the key, so the key never selected anything.
UPDATE nodes n
   SET tags_v2 = COALESCE((
         SELECT array_agg(DISTINCT btrim(kv.value) ORDER BY btrim(kv.value))
           FROM jsonb_each_text(n.tags) AS kv
          WHERE btrim(kv.value) <> ''), '{}')
 WHERE jsonb_typeof(n.tags) = 'object' AND n.tags <> '{}'::jsonb;

-- The array shape. Nothing ever constrained this column to be an object — the config-bundle import
-- bound an untyped `serde_json::Value` until ADR-135 typed it, and that module's own fixture wrote
-- `json!(["core"])`. A row in that shape is unreadable to `node_from_row` today, so this is also
-- the one chance to rescue it rather than carry it forward broken.
UPDATE nodes n
   SET tags_v2 = COALESCE((
         SELECT array_agg(DISTINCT btrim(e) ORDER BY btrim(e))
           FROM jsonb_array_elements_text(n.tags) AS e
          WHERE btrim(e) <> ''), '{}')
 WHERE jsonb_typeof(n.tags) = 'array';

DROP INDEX IF EXISTS nodes_tags_gin_idx;
ALTER TABLE nodes DROP COLUMN tags;
ALTER TABLE nodes RENAME COLUMN tags_v2 TO tags;

-- The floor 0080's ⚠️ note asks for. This is not the additive case that note exempts: the oldest
-- core that can start against this database and still read a node moves up to the release carrying
-- this migration. `relax_ignore_missing` does not help — it decides whether an older binary
-- *starts*, and this one starts perfectly well and then fails at the first node read.
INSERT INTO schema_compat (migration_version, min_core, reason)
VALUES (
    109,
    '0.3.17',
    'nodes.tags changed from a JSONB object to text[] (ADR-135 inc. 2). A core older than 0.3.17 '
    'decodes that column as Json<BTreeMap<String, String>> and fails on every node row: it will '
    'start, and then answer 500 on the node list while the alert engine keeps a stale config '
    '(ADR-080). Returning to an earlier core needs a restore from backup.'
)
ON CONFLICT (migration_version) DO NOTHING;
