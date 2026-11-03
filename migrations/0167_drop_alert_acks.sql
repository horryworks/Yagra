-- 0167_drop_alert_acks — Yagra keeps no acknowledgement (ADR-233).
--
-- The table held ack state mirrored in from an external incident tool through
-- `POST /api/v1/alerts/ack` (migration 0035). Yagra is a read-only monitor: it neither records nor
-- shows whether someone has acknowledged an alert, and that endpoint, the MCP `ack_alert` tool and
-- every `acked` field are gone. Nothing reads the table any more.
--
-- Destructive, so it declares a floor: a core older than 0.3.67 reads `alert_acks` in every
-- History query and would answer that page with an error. Returning to an earlier core needs a
-- restore from backup (the same floor ADR-232 already set).

DROP TABLE IF EXISTS alert_acks;

INSERT INTO schema_compat (migration_version, min_core, reason)
VALUES (
    167,
    '0.3.67',
    'alert_acks is dropped (ADR-233). A core older than 0.3.67 reads it in every alert-history '
    'query, so its History page would fail. Returning to an earlier core needs a restore from backup.'
);
