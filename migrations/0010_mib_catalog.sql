-- MIB repository (Phase B): a curated, searchable OID catalog.
--
-- Lets operators pick a metric by name (with its OID + kind) instead of typing raw OIDs into
-- the collection editor. Seeded on boot from the built-in standard catalog + the vendor
-- (Cisco/Huawei) OID sets; operators can add their own entries. Self-contained (no FKs) — it
-- is a reference/lookup table, not part of the polling path.

CREATE TABLE mib_catalog (
    id          UUID PRIMARY KEY,
    metric_name TEXT NOT NULL UNIQUE,
    oid         TEXT NOT NULL,
    collection  TEXT NOT NULL,          -- 'scalar' | 'table'
    metric_kind TEXT NOT NULL,          -- 'gauge' | 'counter'
    vendor      TEXT,                    -- e.g. 'Cisco', 'Huawei'; NULL = standard
    description TEXT,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX mib_catalog_oid_idx ON mib_catalog (oid);
