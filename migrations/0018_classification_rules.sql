-- 0018_classification_rules — map a discovered device's SNMP signature to a device profile.
--
-- Additive / expand-only (ADR-017): a new table, no change to existing ones. Discovery probes
-- each device for sysObjectID (the vendor-assigned enterprise OID — authoritative device-type
-- key) and the free-form sysDescr. A rule matches by sysObjectID prefix (preferred) and/or a
-- sysDescr regex, and names the profile to suggest on import. Rules are operator-editable, so
-- new device types are added as data — not a code change + redeploy. Built-in rules are seeded
-- at startup with stable ids + ON CONFLICT DO NOTHING (see repo::seed_builtin_classification_rules),
-- so operator edits survive restarts.

CREATE TABLE classification_rules (
    id                 UUID PRIMARY KEY,
    priority           INT  NOT NULL,          -- ascending: lowest evaluated first (most specific)
    sysobjectid_prefix TEXT,                   -- dotted-OID prefix, e.g. '1.3.6.1.4.1.9.'
    sysdescr_regex     TEXT,                   -- fallback regex over sysDescr
    profile_id         UUID NOT NULL REFERENCES profiles (id) ON DELETE CASCADE,
    vendor             TEXT,                   -- optional pre-fill override (descriptive metadata)
    model              TEXT,
    enabled            BOOLEAN NOT NULL DEFAULT true,
    created_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- A rule with no matcher would match everything (or nothing) — require at least one.
    CHECK (sysobjectid_prefix IS NOT NULL OR sysdescr_regex IS NOT NULL)
);

-- Classification walks rules in priority order; index the sort key.
CREATE INDEX classification_rules_priority_idx ON classification_rules (priority);
