-- 0180_unsaved_config_rule_covers_huawei_ac — the "running configuration left unsaved" default rule
-- for Huawei also watches Huawei wireless controllers (ADR-210 stage 11 decision 89).
--
-- The rule seeded at DefaultThresholds offset 36 (`config_unsaved_seconds` above 14400, dwell 2)
-- targets the Huawei profiles whose configuration is fetched with oxidized's `vrp` model, because the
-- save script compares running with startup only for those. Stage 11 fetches "Huawei wireless
-- controller" with `vrp` too. Seeded rows are inserted `ON CONFLICT (id) DO NOTHING`, so the seeder's
-- longer target list reaches a fresh database only; this adds the controller profile to the row an
-- existing deployment already holds — the shape 0128 used for the AP-walk rule.
--
-- Only a row still exactly as it shipped is touched (ADR-140 decision 4): the same metric, bound,
-- dwell and row pattern, no four-sided bound other than the shipped one, and targeting the three
-- Huawei profiles it shipped with, in the order the seeder wrote them. An operator who edited or
-- re-scoped the rule keeps theirs.
--
-- The ids are array positions (`seed_ids.rs`): profile 3 is "Huawei NE/AR router" (…5eed0003),
-- 11 "Huawei CloudEngine switch" (…5eed000b), 28 "Huawei USG firewall" (…5eed001c), 50 "Huawei
-- wireless controller" (…5eed0032), and offset 36 of the default thresholds is …5eedc024.
-- `seed_ids.rs` pins all five to this file.
--
-- reversible: adds one profile id to one rule's target list. An older core reads the list as it is;
--   it fetches no Huawei controller with `vrp`, so the controller reports no `config_unsaved_seconds`
--   there and the extra target matches nothing.

UPDATE thresholds
   SET scope_ids = ARRAY['00000000-0000-0000-0000-00005eed0003',
                         '00000000-0000-0000-0000-00005eed000b',
                         '00000000-0000-0000-0000-00005eed001c',
                         '00000000-0000-0000-0000-00005eed0032']
 WHERE id = '00000000-0000-0000-0000-00005eedc024'::uuid
   AND scope_level = 'profile'
   AND scope_id = '00000000-0000-0000-0000-00005eed0003'
   AND scope_ids = ARRAY['00000000-0000-0000-0000-00005eed0003',
                         '00000000-0000-0000-0000-00005eed000b',
                         '00000000-0000-0000-0000-00005eed001c']
   AND metric = 'config_unsaved_seconds'
   AND direction = 'above'
   AND warning = 14400
   AND critical IS NULL
   AND dwell_samples = 2
   AND row_match IS NULL
   AND warning_below IS NULL
   AND critical_below IS NULL
   AND (warning_above IS NULL OR warning_above = 14400)
   AND critical_above IS NULL;
