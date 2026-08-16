-- 0088_clear_port_name_transceiver — drop transceiver "models" that are just the port's own name.
--
-- ADR-063 Inc.2 shipped an ENTITY-MIB fallback that reads a pluggable's part string. ENTITY-MIB
-- describes *every* component, and the entity a port's ifIndex resolves to is usually the **port**,
-- not a module inside it — so on a device whose `entPhysicalDescr` is simply the port name, every
-- port was recorded as its own transceiver and the UI rendered "Transceiver: GE0/0/1". Measured on
-- the test server within minutes of deploying it: all 12 physical ports, no exceptions.
--
-- The poller now discards a description that only restates the component's own name
-- (`mau::entity_text`). That fix cannot clean up after itself, for the same reason migration 0086
-- existed: the interface upsert COALESCEs each column, so a NULL from the poller preserves the bad
-- value rather than clearing it. A row that is already wrong never heals. This is the second time
-- that property has needed a one-time UPDATE beside a poller fix — it is a consequence of the
-- multiple-writer design, not an accident, and any future change that *corrects* a stored interface
-- value needs the same pair.
--
-- The predicate matches only rows where the stored model equals the interface's own name, folded
-- for case and surrounding whitespace exactly as the poller now folds it. A genuine part number
-- never equals the port name, so nothing real is at risk; a port whose device does report a real
-- module keeps it and is re-confirmed on the next hourly walk.
--
-- reversible: a one-time data correction, no schema change. An older core SELECTs `transceiver_model`
-- by name and renders NULL as "no module reported" — which is what it should have said all along —
-- so a rollback runs unchanged.

UPDATE interfaces
   SET transceiver_model = NULL
 WHERE transceiver_model IS NOT NULL
   AND lower(btrim(transceiver_model)) = lower(btrim(COALESCE(if_name, '')));
