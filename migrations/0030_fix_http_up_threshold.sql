-- Fix the built-in URL/HTTP-endpoint default threshold for `http_up`.
--
-- `http_up` is a 0/1 gauge (1 = reachable AND status matches, 0 = down/wrong-status). The seed in
-- repo.rs originally used a "below 1" critical bound, but the alert engine's "below" comparison is
-- INCLUSIVE (`value <= bound`, crates/yagra-common/src/thresholds.rs). So the healthy value 1 also
-- satisfied `1 <= 1` and every URL monitor was permanently Critical even while serving HTTP 200.
--
-- The corrected bound sits between the two states (0.5): only `http_up = 0` trips critical. The seed
-- is `ON CONFLICT (id) DO NOTHING`, so it cannot rewrite the already-inserted row — do it here.
-- Stable id = Uuid::from_u128(0x0000_0000_0000_0000_0000_0000_5eed_a000). Guard on the old value so
-- an operator who deliberately customised this threshold is left untouched.
UPDATE thresholds
   SET critical = 0.5
 WHERE id = '00000000-0000-0000-0000-00005eeda000'
   AND metric = 'http_up'
   AND critical = 1;
