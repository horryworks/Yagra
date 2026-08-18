-- 0092_clear_non_module_transceiver — drop transceiver "models" that describe the port, not a module.
--
-- Migration 0088 cleared the obvious case: a description that merely restates the entity's own
-- name. That guard was not enough, and the third time this has needed a one-time UPDATE beside a
-- poller fix. ENTITY-MIB describes *every* component, and most devices describe the **port** with a
-- string that is simply *different* from its name while still saying nothing about a module.
-- Measured on the running deployment on 2026-08-18:
--
--   "Linecard-Port"                        54 Nexus ports (N9K) + 52 (N3K)
--   "Port"                                 53 Huawei S5720 ports
--   "Ethernet Port, Vitual Domain: root"   47 FortiGate ports
--   "N/A", "RSP Management Ethernet Port"  IOS-XR
--   "Transceiver Rx Power Sensor"          IOS-XR — the name of a SENSOR, shown as a part number
--
-- The poller now requires a component to be marked field-replaceable (`entPhysicalIsFRU`) or its
-- text to carry a digit — a part number or a rate always does, and none of the strings above does.
-- As with 0086 and 0088, the poller cannot clean up after itself: the interface upsert COALESCEs
-- each column, so a NULL from the poller preserves the bad value rather than clearing it.
--
-- The predicate is the *digit* half of the new rule only, deliberately. It cannot consult
-- `entPhysicalIsFRU`, which is device state this database does not hold — so it also clears the
-- handful of genuine part numbers that happen to carry no digit (`GLC-SX-MMD` on the lab's 2960X).
-- That is the right trade: the hourly media walk re-confirms every module it still accepts, so a
-- real one returns within the hour, while a wrong one would otherwise have stayed forever.
--
-- reversible: a one-time data correction, no schema change. An older core SELECTs
-- `transceiver_model` by name and renders NULL as "no module reported", so a rollback runs unchanged.

UPDATE interfaces
   SET transceiver_model = NULL
 WHERE transceiver_model IS NOT NULL
   AND transceiver_model !~ '[0-9]';
