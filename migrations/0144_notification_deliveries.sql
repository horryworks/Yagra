-- 0144_notification_deliveries - one row per notification delivery (ADR-195).
--
-- reversible: additive only - one new table and two indexes. An older core never names the table,
-- so rolling the binary back leaves it in place and unread, and rolling forward again finds it.
-- No `schema_compat` floor, for the reason 0108 records: every release from 0.2.2 on tolerates a
-- database carrying migrations it does not embed.
--
-- WHY
-- Nothing about an individual delivery was kept, so when a page did not arrive an operator could
-- not tell a refusal by the receiving service from a network fault from a Yagra fault. Each row
-- says whether the delivery arrived and, when it did not, which side failed, the status the
-- remote answered with, and the start of its answer.
--
-- * `channel_id` has no foreign key on purpose: the log outlives a deleted channel, and the
--   channel's kind at delivery time is kept in `channel_kind` for exactly that case. NULL means
--   the environment default route (`YAGRA_WEBHOOK_URL` / `YAGRA_SMTP_*`).
-- * `side`, `status`, `error` and `response` describe the failure that decided a failed row and
--   are NULL on a delivered one. Every attempt, failed or not, is in `attempt_log`.
-- * `response` is at most 512 characters and has the channel's URL, host and key replaced by the
--   writer before it gets here.
-- * Pruned on the alert-linked retention window (`retention::Subject::NotificationDeliveries`).

CREATE TABLE IF NOT EXISTS notification_deliveries (
    id            BIGSERIAL PRIMARY KEY,
    at            TIMESTAMPTZ NOT NULL,
    channel_id    UUID NULL,
    channel_kind  TEXT NULL,
    event         TEXT NOT NULL,
    result        TEXT NOT NULL,
    side          TEXT NULL,
    status        INTEGER NULL,
    attempts      INTEGER NOT NULL,
    duration_ms   BIGINT NOT NULL,
    subject       TEXT NOT NULL,
    node_id       UUID NULL,
    subject_name  TEXT NULL,
    severity      TEXT NULL,
    error         TEXT NULL,
    response      TEXT NULL,
    attempt_log   JSONB NOT NULL DEFAULT '[]'::jsonb
);

-- The page order and its keyset cursor, and the retention prune.
CREATE INDEX IF NOT EXISTS notification_deliveries_at_idx
    ON notification_deliveries (at DESC, id DESC);

-- "This channel's deliveries", the filter the channel list links to.
CREATE INDEX IF NOT EXISTS notification_deliveries_channel_at_idx
    ON notification_deliveries (channel_id, at DESC);
