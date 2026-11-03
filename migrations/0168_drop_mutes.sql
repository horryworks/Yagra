-- 0168_drop_mutes — maintenance windows are the one way to silence alerts (ADR-234).
--
-- A mute silenced only the notification for one node (optionally one check) or one folder group,
-- while the alert itself still fired. It overlapped the maintenance window closely enough that
-- operators could not tell the two apart and did not use the mute, so the REST endpoints, the
-- notifier's mute matching and every screen that showed one are gone. Nothing reads these rows.
--
-- A node released from a group mute it inherited was a `mute` row in `suppression_exemptions`
-- (migration 0081); those go too. The CHECK on that column still admits 'mute', which is harmless:
-- no code writes it any more.
--
-- Destructive, so it declares a floor: a core older than 0.3.67 reads `mutes` on every 30-second
-- refresh and answers `/api/v1/mutes` from it. Returning to an earlier core needs a restore from
-- backup (the same floor ADR-232 and ADR-233 already set).

DELETE FROM suppression_exemptions WHERE kind = 'mute';

DROP TABLE IF EXISTS mutes;

INSERT INTO schema_compat (migration_version, min_core, reason)
VALUES (
    168,
    '0.3.67',
    'mutes is dropped (ADR-234). A core older than 0.3.67 reads it on every alert-config refresh '
    'and serves /api/v1/mutes from it. Returning to an earlier core needs a restore from backup.'
);
