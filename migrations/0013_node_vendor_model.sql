-- Vendor (maker) and model metadata on nodes.
--
-- Populated best-effort by discovery classification from the device's SNMP sysDescr
-- (yagra_discovery::identify) and editable from the node detail. Shown in the node display
-- as "Name (address) (vendor) (model)". Both nullable and purely descriptive — they are
-- PostgreSQL metadata, never TSDB labels (no cardinality cost).
--
-- Additive (expand) migration: existing rows simply read NULL, so an older binary that
-- doesn't select these columns keeps working — N/N-1 upgrade-safe (ADR-017).
ALTER TABLE nodes
    ADD COLUMN vendor TEXT,
    ADD COLUMN model  TEXT;
