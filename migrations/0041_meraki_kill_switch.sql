-- Global Cisco Meraki polling kill switch (a safeguard: instantly halt all Meraki Dashboard API
-- polling fleet-wide during an incident, without deleting any org config).
--
-- Additive (expand, ADR-017): one column with a DEFAULT on the singleton app_settings row, so
-- existing rows and an N-1 binary (which never selects it) keep working. Default true = polling on.

ALTER TABLE app_settings
    ADD COLUMN meraki_polling_enabled BOOLEAN NOT NULL DEFAULT true;
