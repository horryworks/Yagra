-- 0164_drop_label_scopes — a label no longer decides anything Yagra judges (ADR-229).
--
-- Two kinds of row are removed:
--   * threshold rules at the node-label level (`scope_level = 'group'`). The WebUI never offered
--     the level; only a config-bundle import could create one.
--   * maintenance windows scoped to a label (`scope_level = 'group'`). Not offered either.
-- A label stays on nodes and folders as a description carried to notifications.
--
-- reversible: deletes only rows the new core cannot read back as what they were. An older core
-- reads the remaining tables unchanged and boots on them, so no `schema_compat` floor is owed.
-- What a rollback cannot do is bring those rows back; the pre-upgrade backup holds them.
DELETE FROM thresholds WHERE scope_level = 'group';
DELETE FROM maintenance_windows WHERE scope_level = 'group';
