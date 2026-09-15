-- 0117_default_poll_interval_300 — a new installation polls every 300 seconds, not every 30 (ADR-144).
--
-- reversible: a column default and nothing else. No row is read or written, so every deployment keeps
-- the interval it has stored; an older core never inserts `app_settings` without naming the column, so
-- it never sees the default either. No `schema_compat` floor, for the reason 0108 records: every
-- release from 0.2.2 on tolerates a database carrying migrations it does not embed.
--
-- WHY THIS EXISTS WHEN NOTHING USES THE DEFAULT
-- The row is created by `seed_app_settings` at startup, which names the value it writes
-- (`config::DEFAULT_POLL_INTERVAL_SECS`), so migration 0027's `DEFAULT 30` is never what a deployment
-- gets. It is still a second copy of the default, and left at 30 it would be the one anybody reading
-- the schema believes. 0027 is applied and checksummed, so the correction goes here.

ALTER TABLE app_settings ALTER COLUMN default_poll_interval_secs SET DEFAULT 300;
