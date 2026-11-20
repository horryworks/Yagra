-- 0183_webui_url - the address operators open the WebUI at, for links in notifications (ADR-260).
--
-- NULL means not set, which is what every existing deployment keeps meaning: no notification gains
-- a link until an operator saves an address in Settings > System settings.
--
-- reversible: adds one nullable column. An older core never reads it, so its notifications simply
-- carry no link; nothing it does depends on the value.

ALTER TABLE app_settings ADD COLUMN webui_url TEXT;
