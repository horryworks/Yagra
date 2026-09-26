-- 0138_node_l3_address_gin — find which node carries an interface address without scanning every
-- node's address document (ADR-180).
--
-- reversible: one index is created. Nothing is added to or removed from any table, so a core from
-- before it reads and writes `node_l3` exactly as it did; the index only makes some of its reads
-- faster. No `schema_compat` floor, for the reason 0108 records: every release from 0.2.2 on
-- tolerates a database carrying migrations it does not embed.
--
-- WHY
-- The Neighbors tab asks, for each peer's management address, which node claims it — by inventory
-- address (already indexed) or by any address one of its interfaces carries. The second lives inside
-- `node_l3.addresses`, one JSONB document per node, and without an index answering it reads every
-- node's document on every refresh of every open tab (15 s). `jsonb_path_ops` is the smaller GIN
-- operator class and serves exactly the containment test the reader uses:
--   addresses->'addresses' @> '[{"ip": "…"}]'
-- The expression must match the reader's byte for byte, or the planner cannot use the index; a
-- database test (`repo/address_owners.rs`) checks that it does.
CREATE INDEX IF NOT EXISTS node_l3_addresses_gin
    ON node_l3 USING GIN ((addresses -> 'addresses') jsonb_path_ops);
