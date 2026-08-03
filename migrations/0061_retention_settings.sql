-- Operator-configurable data retention (ADR-040 decision 2).
--
-- Before this, every retention window was a compiled constant and `90 * 86_400` was written in
-- three separate files. The policy now lives in `yagra-core/src/retention.rs` and these columns are
-- where an operator's edits to it are stored; the prune loops re-read them each tick.
--
-- Additive (expand, ADR-017): columns with DEFAULTs on the singleton app_settings row, so existing
-- rows and an N-1 binary (which never selects them) keep working. The defaults are exactly the
-- values that were compiled in before, so applying this migration changes no behaviour.
--
-- flow_retention_days deliberately defaults to 30 even on a deployment whose .env sets
-- YAGRA_FLOW_RETENTION_DAYS to something smaller. That env var never reached an existing ClickHouse
-- volume (the TTL was only ever emitted inside CREATE TABLE IF NOT EXISTS, so it was inert after
-- first creation), which means 30 is what those tables are *actually* enforcing today. Seeding the
-- env value here would make the first ALTER ... MODIFY TTL after the upgrade delete real flow rows
-- an operator never asked to lose in that moment. The env var still seeds a *fresh* deployment
-- (repo.rs::seed_app_settings, first boot only, like YAGRA_POLL_INTERVAL_SECS); on an existing one
-- the operator lowers it in Settings, where the consequence is visible and deliberate.
--
-- The CHECKs are a backstop, not the primary guard — `retention::days_in_bounds` /
-- `hours_in_bounds` reject out-of-band input at the API edge with a typed 400.

ALTER TABLE app_settings
    ADD COLUMN alert_linked_retention_days     INTEGER NOT NULL DEFAULT 90
        CHECK (alert_linked_retention_days BETWEEN 1 AND 3650),
    ADD COLUMN unmatched_event_retention_hours INTEGER NOT NULL DEFAULT 24
        CHECK (unmatched_event_retention_hours BETWEEN 1 AND 8760),
    ADD COLUMN report_run_retention_days       INTEGER NOT NULL DEFAULT 90
        CHECK (report_run_retention_days BETWEEN 1 AND 3650),
    ADD COLUMN flow_retention_days             INTEGER NOT NULL DEFAULT 30
        CHECK (flow_retention_days BETWEEN 1 AND 3650);
