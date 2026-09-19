-- 0123_wireless_import_default_on — the access points behind a wireless controller become nodes by
-- default (ADR-064 R22).
--
-- reversible: this changes one column default and sets one boolean on existing rows. Nothing is
-- narrowed, dropped or added. A core from before it (v0.3.27) reads the column the same way and
-- already runs the importer, so rolling the binary back keeps importing — the state this migration
-- asked for. No `schema_compat` floor, for the reason 0108 records: every release from 0.2.2 on
-- tolerates a database carrying migrations it does not embed.
--
-- WHY
-- 0121 made `import_aps` default to FALSE: registering a controller must not create hundreds of
-- nodes by itself (ADR-064 決定 8). The first real deployment changed that. Four controllers reported
-- 43 access points and none of them became a node, which read as "an AP is not monitored until
-- someone presses Monitor on it". Monitoring a controller is taken to mean monitoring what it
-- serves. The two limits that make the default safe to flip both stay: `max_aps` (1,024 unless an
-- operator sets another) and "only an AP that has been in service at least once" (ADR-064 R7).
--
-- WHY THE DEFAULT LIVES HERE AND NOWHERE ELSE
-- A controller's row is created by its first inventory (`WirelessRepo::record_inventory`), and that
-- INSERT does not name this column. The column default is therefore THE default — including for a
-- row an older core creates — and no Rust constant repeats it.
--
-- WHY EXISTING ROWS ARE SWITCHED ON TOO
-- Nothing records whether a FALSE was chosen or merely inherited: `updated_at` moves on every
-- inventory as well as on a settings write. The feature had been public for hours when this was
-- decided, so the rows that exist are overwhelmingly untouched defaults (ADR-064 R22 決定 B, user
-- decision 2026-09-19).
-- ⚠️ Consequence, stated in the release notes: within a minute of the upgrade, every access point
-- that has ever been in service becomes a node, up to each controller's cap.
-- ⚠️ 0121's comment on this column still says "Off by default". It cannot be edited — an applied
-- migration is checksummed — so this file is where the current answer is written down.

ALTER TABLE wireless_controllers ALTER COLUMN import_aps SET DEFAULT TRUE;

UPDATE wireless_controllers SET import_aps = TRUE WHERE import_aps = FALSE;
