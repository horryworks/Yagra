-- 0184_maintenance_window_time_zone - the clock a maintenance window was asked for in (ADR-261).
--
-- starts_at / ends_at stay the instants that decide everything. This column only records which IANA
-- zone the operator typed the times in, so the list can show "22:00 EST" next to the viewer's own
-- clock. NULL means none was recorded: every existing window, and every window opened over MCP or
-- by the upgrade path, which carry an instant with its own offset.
--
-- reversible: adds one nullable column. An older core never reads it and a window it writes has
-- NULL, which reads back exactly as before.

ALTER TABLE maintenance_windows ADD COLUMN time_zone TEXT;
