-- 0160_schedule_time_zone — a preset schedule names the zone its day and time are a wall time in
-- (ADR-221 decision 4): "09:00 in Asia/Tokyo" rather than an hour in UTC, so a monthly "1st at
-- 07:00 JST" can be said at all and a daylight-saving zone keeps its wall time all year.
--
-- reversible: additive only — one nullable column on each of the three schedule tables. NULL is
-- UTC, which is what every existing row meant, so nothing already stored changes meaning. An older
-- core reads and writes these rows through explicit column lists and never names the column, so
-- rolling the binary back keeps every schedule running, at its hour read as UTC (ADR-221 records
-- that cost). No `schema_compat` floor, for the reason 0108 records.
--
-- The column holds an IANA name and is checked by the API edge against the zone database core
-- was built with, not here: a CHECK would have to list every zone, and a newer tz release adding
-- one would make an older core's rows unwritable.
ALTER TABLE report_schedules ADD COLUMN IF NOT EXISTS time_zone TEXT;
ALTER TABLE analysis_schedules ADD COLUMN IF NOT EXISTS time_zone TEXT;
ALTER TABLE config_backup_schedule ADD COLUMN IF NOT EXISTS time_zone TEXT;
