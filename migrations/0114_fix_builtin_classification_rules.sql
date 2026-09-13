-- 0114_fix_builtin_classification_rules — correct nine built-in classification rules that send
-- devices to the wrong profile (ADR-140).
--
-- reversible: this deletes at most nine seed rows, and each only while it is still exactly what
-- Yagra shipped. The same boot's seeder (`NodeRepo::seed_builtin_profiles`, which runs after every
-- migration) inserts them again under the same ids with the corrected content, so a deployment is
-- never left without these rules. Nothing about the schema moves, so no `schema_compat` floor.
-- ⚠️ Rolling back is safe but not free: an older core's seeder re-inserts the OLD content under these
-- ids, and rolling forward again does not re-run this migration (it is recorded as applied), so the
-- old rules stay until someone corrects them on the Classification rules screen.
--
-- WHY A GUARDED DELETE, AND NOT A RANGE DELETE (0020) OR AN UPDATE
-- A seed row keeps its id for ever (`SeedRange::ClassificationRules` — its index in the array) and
-- the seeder's `ON CONFLICT (id) DO NOTHING` never overwrites one, so a rule corrected in code reaches
-- a running deployment only if the old row is gone first. 0020 deleted the whole range, which also
-- throws away every edit an operator made to a built-in rule. Here a row is deleted only when every
-- column still holds the shipped value — the pattern 0030 used for a threshold — so a rule an
-- operator rewrote or switched off stays exactly as they left it. An UPDATE alone would not do:
-- rows 26 and 28 now point at profiles appended in the same release, which do not exist until the
-- seeder runs, after this migration.
--
-- THE NINE ROWS (array index → what changes). Every one is proven by a LibreNMS recording in
-- `crates/yagra-discovery/testdata/os_version_fixtures.json`, replayed through the built-in rules by
-- `classification::tests::every_librenms_fixture_lands_on_the_profile_it_names`:
--    4  Cisco FTD      priority 50 → 25 and "firepower threat defense" — an FTD also says "ASA Version"
--    5  Cisco WLC      + "cisco controller", "cisco business wireless", "c9800"
--    6  IOS-XE router  "\bASR\b" → "\bASR", + "c8000" — the model number follows directly ("ASR1000")
--    8  Juniper SRX    "\bsrx\b" → "\bv?srx" — "srx100h2"
--    9  Juniper MX     "\bmx\b" → "\bv?mx(\d|\b)" — "vmx", "mx480"
--   12  Huawei NE/AR   + "netengine" — "NetEngine 8000"
--   21  14823.         Aruba/HPE switch → Aruba wireless controller
--   26  25053.         Brocade / Ruckus switch → Ruckus wireless controller (new profile)
--   28  6486.          Nokia SR router → Alcatel-Lucent OmniSwitch (new profile)
-- `seed_ids::tests::migration_0114_names_the_rules_the_catalog_issues_today` pins every id below to
-- today's array positions.
DELETE FROM classification_rules AS r
 USING (VALUES
    ('00000000-0000-0000-0000-00005eed8004'::uuid, 50,  '1.3.6.1.4.1.9.',     '(?i)firepower|\bFTD\b'::text,                      '00000000-0000-0000-0000-00005eed0017'::uuid, 'Cisco'),
    ('00000000-0000-0000-0000-00005eed8005'::uuid, 60,  '1.3.6.1.4.1.9.',     '(?i)wireless lan controller|\bWLC\b|air-ct',       '00000000-0000-0000-0000-00005eed001e'::uuid, 'Cisco'),
    ('00000000-0000-0000-0000-00005eed8006'::uuid, 70,  '1.3.6.1.4.1.9.',     '(?i)\bISR\b|\bASR\b|ios[ -]?xe.*router',           '00000000-0000-0000-0000-00005eed0000'::uuid, 'Cisco'),
    ('00000000-0000-0000-0000-00005eed8008'::uuid, 100, '1.3.6.1.4.1.2636.',  '(?i)\bsrx\b',                                      '00000000-0000-0000-0000-00005eed001b'::uuid, 'Juniper'),
    ('00000000-0000-0000-0000-00005eed8009'::uuid, 110, '1.3.6.1.4.1.2636.',  '(?i)\bmx\b|\bptx\b',                               '00000000-0000-0000-0000-00005eed0002'::uuid, 'Juniper'),
    ('00000000-0000-0000-0000-00005eed800c'::uuid, 150, '1.3.6.1.4.1.2011.',  '(?i)\bne\d|\bar\d|router',                         '00000000-0000-0000-0000-00005eed0003'::uuid, 'Huawei'),
    ('00000000-0000-0000-0000-00005eed8015'::uuid, 270, '1.3.6.1.4.1.14823.', NULL,                                               '00000000-0000-0000-0000-00005eed000c'::uuid, 'Aruba'),
    ('00000000-0000-0000-0000-00005eed801a'::uuid, 320, '1.3.6.1.4.1.25053.', NULL,                                               '00000000-0000-0000-0000-00005eed000f'::uuid, 'Ruckus'),
    ('00000000-0000-0000-0000-00005eed801c'::uuid, 340, '1.3.6.1.4.1.6486.',  NULL,                                               '00000000-0000-0000-0000-00005eed0004'::uuid, 'Alcatel-Lucent')
 ) AS shipped (id, priority, prefix, regex, profile_id, vendor)
 WHERE r.id = shipped.id
   AND r.priority = shipped.priority
   AND r.sysobjectid_prefix IS NOT DISTINCT FROM shipped.prefix
   AND r.sysdescr_regex IS NOT DISTINCT FROM shipped.regex
   AND r.profile_id = shipped.profile_id
   AND r.vendor IS NOT DISTINCT FROM shipped.vendor
   AND r.model IS NULL
   AND r.enabled;
