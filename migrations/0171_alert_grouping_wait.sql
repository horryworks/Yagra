-- 0171_alert_grouping_wait - whether a border's notification waits for a failing neighbour to
-- commit before it pages (ADR-236 Inc.2 decision 12). One switch for the whole deployment; false is
-- how increment 1 behaved, so an upgrade changes nothing until someone turns it on.
--
-- reversible: additive only - one column on `app_settings` with a default. An older core never
-- names it. No `schema_compat` floor, for the reason 0108 records.
ALTER TABLE app_settings
    ADD COLUMN IF NOT EXISTS alert_grouping_wait BOOLEAN NOT NULL DEFAULT false;
