-- 0019_profile_metadata — add functional category + vendor to device profiles.
--
-- Additive / expand-only (ADR-017): two new columns on profiles. `category` is the functional
-- role (router/switch/firewall/server/…) — the primary split axis and the grouping the Device
-- profiles UI sections by; `vendor` is descriptive metadata (never a TSDB label). Existing rows
-- default to 'generic-snmp'; the built-in catalog reseed (0020 + seed_builtin_profiles) sets the
-- real values. Tokens are the kebab-case forms of yagra_common::ProfileCategory.

ALTER TABLE profiles
    ADD COLUMN category TEXT NOT NULL DEFAULT 'generic-snmp',
    ADD COLUMN vendor   TEXT;
