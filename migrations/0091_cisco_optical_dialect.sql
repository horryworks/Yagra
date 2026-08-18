-- 0091_cisco_optical_dialect — point the Cisco profiles at the optical MIB Cisco actually serves.
--
-- ADR-070 decision 1. Cisco does not implement ENTITY-SENSOR-MIB (RFC 3433): measured across eight
-- real device walks, the standard `1.3.6.1.2.1.99` sensor table returns **zero rows** on seven of
-- them (the exception is a wireless controller), while `1.3.6.1.4.1.9.9.91` returns 10–342. Every
-- Catalyst and Nexus has therefore been walking a table its agent does not serve, which is
-- invisible from the outside: the collection item exists, the poller issues the walk, the device
-- returns nothing, and one `no_data` row appears.
--
-- The new template ("Optical transceivers (Cisco)") and its links are appended to the built-in
-- catalog, so `seed_builtin_profiles` inserts them on the next boot without help. What the seeder
-- **cannot** do is remove the old link — every one of its statements is `ON CONFLICT DO NOTHING`,
-- and none of them delete. Left in place, an existing deployment would carry both links and walk
-- two transceiver tables on every poll, one of which can never have rows
-- (`collection.rs::no_builtin_profile_attaches_two_optical_dialects` states that cost).
--
-- Scope: four rows in the LINK table. Profiles, templates and nodes are untouched, so unlike the
-- range-delete in 0020 this cannot unbind a node from its profile (`nodes.profile_id ON DELETE SET
-- NULL` is never reached). Ids are the stable seed ids — `Uuid::from_u128(BASE + array_index)`,
-- see `crates/yagra-core/src/seed_ids.rs` — and were verified against a running deployment before
-- this file was written.
--
-- ⚠️ Cisco ASA (…5eed0016) keeps the standard dialect deliberately: its walk carries 15 rows in the
-- standard sensor table and none in Cisco's, the reverse of every other Cisco device measured.
--
-- reversible: an older core re-seeds the link it knows about on its next boot. Its
-- `builtin_profiles()` still names "Optical transceivers (ENTITY-SENSOR)" for these four profiles,
-- and `seed_builtin_profiles` inserts profile→template links unconditionally (ON CONFLICT DO
-- NOTHING), so rolling back restores exactly the rows this deletes. No schema change; the Cisco
-- template row itself is left in place, where an older core simply never links to it.

DELETE FROM profile_collection_templates
 WHERE template_id = '00000000-0000-0000-0000-00005eed7015'::uuid  -- Optical (ENTITY-SENSOR)
   AND profile_id IN (
     '00000000-0000-0000-0000-00005eed0000'::uuid,  -- Cisco IOS/IOS-XE router
     '00000000-0000-0000-0000-00005eed0001'::uuid,  -- Cisco IOS-XR router
     '00000000-0000-0000-0000-00005eed0007'::uuid,  -- Cisco Catalyst switch (IOS/IOS-XE)
     '00000000-0000-0000-0000-00005eed0008'::uuid   -- Cisco Nexus switch (NX-OS)
   );
