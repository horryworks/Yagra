-- 0115_node_delete_fk_indexes — deleting nodes no longer reads a whole table per deleted row
-- (ADR-124 Increment 7).
--
-- reversible: additive only — two partial indexes, nothing narrowed and nothing rewritten. An older
-- core never names them, so rolling the binary back leaves them in place and unused, and rolling
-- forward again finds them. No `schema_compat` floor, for the reason 0108 records: every release
-- from 0.2.2 on tolerates a database carrying migrations it does not embed.
--
-- WHY
-- Deleting a node makes PostgreSQL run one referential action per deleted row against every table
-- whose foreign key points at `nodes`. Where the referencing column has no index, each of those
-- actions reads the whole table. The bulk delete (`POST /api/v1/nodes/delete`) removes up to 1000
-- nodes in one statement, so a table that grows with the fleet is read up to 1000 times inside one
-- transaction.
--
-- WHY THESE TWO AND NOT THE OTHER FOUR
-- A catalog query over a fully migrated database found six single-column foreign keys to `nodes`
-- with no index leading on the column. These two sit on tables that grow with the fleet:
-- `node_links` is derived from every node's neighbours, and `l3_discovered` from every router's ARP
-- and neighbour caches. The other four — `event_rules.node_id`, `event_sources.node_id`,
-- `pollers.anchor_node_id` and `suppression_exemptions.node_id` — are on rows an operator creates,
-- which stay few whatever the size of the inventory.
--
-- WHY PARTIAL
-- The referential action finds its rows by equality with the deleted id, so a row whose column is
-- NULL is never what it is looking for. Leaving those rows out keeps the index to the rows that can
-- match.
--
-- Plain `CREATE INDEX` rather than `CONCURRENTLY`: sqlx runs each migration in a transaction, where
-- `CONCURRENTLY` is refused.

CREATE INDEX IF NOT EXISTS node_links_forced_parent_idx
    ON node_links (forced_parent) WHERE forced_parent IS NOT NULL;

CREATE INDEX IF NOT EXISTS l3_discovered_promoted_node_idx
    ON l3_discovered (promoted_node_id) WHERE promoted_node_id IS NOT NULL;
