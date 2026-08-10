-- 0020_reseed_builtin_catalog — clean re-seed for the Device Profiles redesign.
--
-- Breaking change (explicitly approved): the built-in device profiles, collection templates, and
-- classification rules are rebuilt from scratch, split by functional role × vendor-NOS family.
-- The old seeded rows must be removed so the next boot's seed_builtin_profiles() can repopulate
-- the new catalog cleanly (seeding is ON CONFLICT (id) DO NOTHING, so a stale row sharing a
-- reserved id would otherwise shadow the new one and corrupt name↔items mappings).
--
-- Only the built-in rows are removed, identified by their reserved stable-id ranges
-- (Uuid::from_u128(BASE + index); see repo::seed_builtin_profiles):
--   profiles            0x…5eed0000 … 0x…5eed00ff
--   collection_templates 0x…5eed7000 … 0x…5eed70ff
--   classification_rules 0x…5eed8000 … 0x…5eed80ff
-- Operator-created rows use random v4 UUIDs outside these ranges and are untouched.
--
-- Cascades: deleting a template removes its items + profile links; deleting a profile removes
-- its template links + classification rules. A node bound to a removed built-in profile falls to
-- NULL (nodes.profile_id ON DELETE SET NULL) — it keeps polling and is re-classified on the next
-- Discovery scan, or re-assigned by the operator. The new catalog is broader, so most classes map
-- to a more specific successor (e.g. "Cisco IOS router" → "Cisco IOS/IOS-XE router").
--
-- reversible: range-deletes built-in catalog rows, not schema — and an older binary re-seeds its own
-- built-ins on boot, so going back restores its catalog (ADR-050 decision 7).

DELETE FROM classification_rules
 WHERE id >= '00000000-0000-0000-0000-00005eed8000'::uuid
   AND id <= '00000000-0000-0000-0000-00005eed80ff'::uuid;

DELETE FROM collection_templates
 WHERE id >= '00000000-0000-0000-0000-00005eed7000'::uuid
   AND id <= '00000000-0000-0000-0000-00005eed70ff'::uuid;

DELETE FROM profiles
 WHERE id >= '00000000-0000-0000-0000-00005eed0000'::uuid
   AND id <= '00000000-0000-0000-0000-00005eed00ff'::uuid;
