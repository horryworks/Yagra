-- 0193_alert_history_roll_up - which outage each alert was grouped into, kept in History, and
-- which check each delivery was about (ADR-270).
--
-- reversible: additive only - nullable or defaulted columns and two indexes. An older core names
-- its columns in both its INSERT and its SELECT, so it writes rows that leave these empty and reads
-- past them. No `schema_compat` floor, for the reason 0108 records: every release from 0.2.2 on
-- tolerates a database carrying migrations it does not embed. Known cost: an older core reads a
-- `rolled_up` row as one more fire of that check: its History shows the transition twice, and
-- nothing is lost.
--
-- WHY
-- When ADR-236 grouped an outage, History kept a fire and a resolve for every node and nothing
-- else: which node an alert was grouped under, and the moment an alert that had paged was grouped
-- after all, were gone once the alerts closed.
--
-- * `alert_history.root_cause` is the node the alert was grouped under when the row was written
--   (`Alert.root_cause`). NULL when it was not grouped, and on every older row.
-- * `alert_history.rolled_up` marks the row written when an alert that had paged on its own was
--   grouped into an outage (`NotifyAction::Suppress`). It is not a resolve: the alert stays open.
--   Every reader that counts fires skips it.
-- * `notification_deliveries.check_id` is the check a delivery was about, so History can show the
--   deliveries of one transition. NULL for a test notification and on every older row.

ALTER TABLE alert_history
    ADD COLUMN IF NOT EXISTS root_cause UUID NULL,
    ADD COLUMN IF NOT EXISTS rolled_up BOOLEAN NOT NULL DEFAULT false;

-- What "the nodes grouped under this border" reads (`grouped_under`): few rows carry a root cause,
-- so the partial index stays small.
CREATE INDEX IF NOT EXISTS alert_history_root_cause_idx
    ON alert_history (root_cause, recorded_at)
    WHERE root_cause IS NOT NULL;

ALTER TABLE notification_deliveries
    ADD COLUMN IF NOT EXISTS check_id UUID NULL;

-- What a History row reads its deliveries by: one subject and check, newest first.
CREATE INDEX IF NOT EXISTS notification_deliveries_subject_check_idx
    ON notification_deliveries (subject, check_id, at DESC);
