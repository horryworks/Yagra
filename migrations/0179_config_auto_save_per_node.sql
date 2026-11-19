-- 0179_config_auto_save_per_node - automatic save is switched on or off per node, beside the
-- node's config backup (ADR-222 Inc.11 decision 14), rather than once for the whole deployment.
--
-- reversible: one column with a default, and an UPDATE that only copies the deployment-wide
-- switch onto it. An older core never names the column; it reads `config_save_settings.auto_save`,
-- which a newer core keeps writing as "true while any node is on" (decision 22), so rolling the
-- binary back keeps automatic save running, for every backed-up node rather than the chosen ones.
-- No `schema_compat` floor, for the reason 0108 records.
--
-- A deployment that had automatic save on keeps it on for every node backed up now: that is what
-- the switch meant until this migration. A node whose model has no save command may be marked on
-- here; core checks the model before every save, so nothing is sent to it.
ALTER TABLE node_config_backup ADD COLUMN IF NOT EXISTS auto_save BOOLEAN NOT NULL DEFAULT false;

UPDATE node_config_backup
   SET auto_save = true
 WHERE enabled
   AND (SELECT auto_save FROM config_save_settings LIMIT 1);
