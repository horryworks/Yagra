-- 0022_drop_huawei_mem_size — remove the broken Huawei memory-size metric.
--
-- PR #47 added `huawei_mem_size` (hwEntityMemSize, entity-extent .1.3.6.1.4.1.2011.5.25.31.1.1.1.1.6)
-- to the built-in "Huawei VRP health" template, but on Huawei YunShan OS (USG / newer VRP) that OID
-- returns a bogus small value (e.g. 90) instead of bytes. It's replaced by HUAWEI-MEMORY-MIB
-- hwMemoryDevSize64/hwMemoryDevFree64 (.1.3.6.1.4.1.2011.6.3.5.1.1.8/.9, real 64-bit RAM bytes),
-- which the seed adds as new metric_names on the next boot.
--
-- The seed only ever inserts (ON CONFLICT (template_id, metric_name) DO NOTHING), so it can't remove
-- the stale row — delete it here. Migrations run before the seed (main.rs). The name is specific to
-- our built-in template, so operator-created metrics are not affected. A node's resolved collection
-- set simply stops including it; any past values in the TSDB are harmless and just stop being written.
--
-- reversible: deletes one built-in catalog row, not schema — and an older binary re-seeds its own
-- built-ins on boot, so going back restores it (ADR-050 decision 7).

DELETE FROM collection_template_items WHERE metric_name = 'huawei_mem_size';
