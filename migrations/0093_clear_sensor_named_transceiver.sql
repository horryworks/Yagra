-- 0093_clear_sensor_named_transceiver — drop transceiver "models" that are the name of a sensor.
--
-- The fourth one-time UPDATE beside a poller fix, and the second in one afternoon. Migration 0092
-- required a component's text to carry a digit; an IOS-XR router attaches
-- `Transceiver Voltage Sensor - 3.3V` to 13 of its ports, and "3.3V" is a digit. The word
-- *Transceiver* in it made the wrong answer read as the right one.
--
-- The poller now excludes `entPhysicalClass = sensor(8)` structurally, so no string has to be
-- interpreted there. This statement cannot do that — `entPhysicalClass` is device state the
-- database does not hold — so it matches on the one word that is never part of a module's
-- designation. A transceiver is named by its form factor, its rate and its reach; none of the
-- registrations in `MAU_TYPES` nor any vendor part number measured here contains "sensor".
--
-- As with 0086, 0088 and 0092: the interface upsert COALESCEs every column, so a NULL from the
-- poller preserves the wrong value rather than clearing it. A row that is already wrong never heals.
--
-- reversible: a one-time data correction, no schema change. An older core renders NULL as "no
-- module reported", so a rollback runs unchanged.

UPDATE interfaces
   SET transceiver_model = NULL
 WHERE transceiver_model IS NOT NULL
   AND transceiver_model ILIKE '%sensor%';
