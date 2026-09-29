-- 0142_nodes_name_key_idx — look a device node up by its name as Discovery compares it (ADR-139).
--
-- reversible: additive only — one index, nothing narrowed and nothing rewritten. An older core
-- never names the index, so rolling the binary back leaves it in place and unused, and rolling
-- forward again finds it. No `schema_compat` floor, for the reason 0108 records: every release from
-- 0.2.2 on tolerates a database carrying migrations it does not embed.
--
-- WHY
-- ADR-139 increment 3 marks a Discovery candidate that looks like a node monitored at another
-- address, and one of its two pieces of evidence is the name. `NodeRepo::device_nodes_named` asks
-- for `lower(btrim(name)) = ANY(...)` on every read of the scan — every two seconds per open tab
-- while a sweep runs — and no index matched that expression, so each read was a full scan of the
-- inventory. This is 0112's argument for `nodes.address`, made for the name.
--
-- The expression must stay byte-for-byte the one the query writes, or the planner cannot use it.
-- Plain `CREATE INDEX` rather than `CONCURRENTLY`, as in 0112: sqlx runs each migration in a
-- transaction, and at 50k rows the build takes well under a second.

CREATE INDEX IF NOT EXISTS nodes_name_key_idx ON nodes (lower(btrim(name)));
