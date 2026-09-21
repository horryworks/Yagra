-- 0128_wlan_walk_rule_covers_cisco — the "AP table was not read to its end" default rule also watches
-- Cisco wireless controllers (ADR-064 増分 F).
--
-- The rule seeded at DefaultThresholds offset 33 (`wlan_ap_walk_complete` below 0.5 = warning, dwell 3)
-- targets the profiles that walk an AP table, and until increment F that was "Huawei wireless
-- controller" alone. Increment F gives "Cisco wireless controller" the same walk. Seeded rows are
-- inserted `ON CONFLICT (id) DO NOTHING`, so the seeder's longer target list reaches a fresh database
-- only; this adds the Cisco profile to the row an existing deployment already holds.
--
-- Only a row still exactly as it shipped is touched (ADR-140 decision 4): the same metric, bound,
-- dwell and row pattern, no four-sided bounds of an edit (0098) other than the shipped one, and
-- targeting the Huawei profile alone. An operator who edited or re-scoped the rule keeps theirs.
--
-- The ids are array positions (`seed_ids.rs`): profile 30 is "Cisco wireless controller"
-- (…5eed001e), profile 50 is "Huawei wireless controller" (…5eed0032), and offset 33 of the default
-- thresholds is …5eedc021. `seed_ids.rs` pins all three to this file.
--
-- reversible: adds one profile id to one rule's target list. An older core reads the list as it is;
--   its Cisco controllers publish no `wlan_ap_walk_complete`, so the extra target matches nothing.

UPDATE thresholds
   SET scope_ids = ARRAY['00000000-0000-0000-0000-00005eed0032',
                         '00000000-0000-0000-0000-00005eed001e']
 WHERE id = '00000000-0000-0000-0000-00005eedc021'::uuid
   AND scope_level = 'profile'
   AND scope_id = '00000000-0000-0000-0000-00005eed0032'
   AND scope_ids = ARRAY['00000000-0000-0000-0000-00005eed0032']
   AND metric = 'wlan_ap_walk_complete'
   AND direction = 'below'
   AND warning = 0.5
   AND critical IS NULL
   AND dwell_samples = 3
   AND row_match IS NULL
   AND (warning_below IS NULL OR warning_below = 0.5)
   AND critical_below IS NULL
   AND warning_above IS NULL
   AND critical_above IS NULL;
