-- 0097_reseed_vendor_thresholds — move the vendor default rules off `global` (ADR-078 decision 1).
--
-- ADR-077 seeded 21 vendor-specific rules at the `global` scope, arguing that a metric only Cisco
-- collects can sit there harmlessly because no other node ever evaluates it. That is true of the
-- alerting and false of the screen: Alerts > Metric alert rules then told the operator that a
-- Cisco-only rule applied to every node, twenty-one times, in a thirty-row list.
--
-- Seeded rows carry stable ids and are inserted `ON CONFLICT (id) DO NOTHING`, so changing the
-- definition in `repo.rs` cannot update a row that already exists. Deleting them lets the next boot
-- re-seed the corrected ones — the same mechanism migration 0020 uses for the built-in catalog.
--
-- Offsets 4..23 of the `DefaultThresholds` range (0x…5eedc000 + offset). Offsets 0..3 are left
-- alone: node down, SNMP down, packet loss and round-trip time really do apply to every node.
--
-- Two consequences worth being explicit about:
--   * An operator's EDITS to these twenty rows are lost, not merged. That is acceptable only
--     because they have never been in a release — they shipped to the test deployment on 2026-08-20
--     and the release notes still carry them under `## Unreleased`.
--   * Row 15 (`huawei_mem_usage`) also comes back with a different bound, 85/95 rather than 80/90.
--     The firewall on the test deployment measures exactly 80.0 and the comparison is inclusive, so
--     that rule alerted continuously from the moment it shipped.
--
-- reversible: range-deletes seeded default threshold rows; a core that boots against this database
--   re-seeds its own version of them, so an older one restores exactly what it used to write.

DELETE FROM thresholds
 WHERE id >= '00000000-0000-0000-0000-00005eedc004'::uuid
   AND id <= '00000000-0000-0000-0000-00005eedc017'::uuid;
