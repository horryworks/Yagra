-- 0178_config_text_for_ai - whether `/mcp` may hand a device configuration's text to an AI client
-- (ADR-210 stage 10 decision 85). Off by default: an Admin switches it on, signed in.
--
-- reversible: one column with a default. An older core reads `config_save_settings` through
-- explicit column lists (`SELECT auto_save …`), so it never selects this one, and it has no tool
-- that serves the text, so the column has nothing to gate there. No `schema_compat` floor, for the
-- reason 0108 records.
ALTER TABLE config_save_settings ADD COLUMN mcp_config_text BOOLEAN NOT NULL DEFAULT false;
