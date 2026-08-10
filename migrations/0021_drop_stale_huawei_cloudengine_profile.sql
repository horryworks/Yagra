-- 0021_drop_stale_huawei_cloudengine_profile — remove a duplicate Device profile.
--
-- The Device Profiles redesign (0020 + seed_builtin_profiles) ships exactly one Huawei
-- CloudEngine profile: "Huawei CloudEngine switch" (category l3-switch). An earlier
-- operator/discovery-created profile literally named "Huawei CloudEngine" (random v4 UUID,
-- default 'generic-snmp' category — outside the reserved seed-id ranges, so 0020's range
-- delete never touched it) lingers in the live catalog and shows up as a duplicate under
-- the Generic SNMP group. Remove it.
--
-- Exact-name match: this deletes only "Huawei CloudEngine", never the built-in
-- "Huawei CloudEngine switch". The seed never recreates this name, so it stays gone.
-- FK-safe: a node bound to it falls to NULL (nodes.profile_id ON DELETE SET NULL) and is
-- re-classified on the next Discovery scan; its template links / classification rules cascade.
--
-- reversible: deletes one operator-created catalog row, not schema. An older binary starts against
-- the resulting schema unchanged (ADR-050 decision 7).

DELETE FROM profiles WHERE name = 'Huawei CloudEngine';
