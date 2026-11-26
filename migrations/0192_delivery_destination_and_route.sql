-- 0192_delivery_destination_and_route - where each delivery went and why it went there (ADR-269).
--
-- reversible: additive only - four nullable or defaulted columns on an existing table. An older
-- core names its columns in both its INSERT and its SELECT, so it writes rows that leave these
-- empty and reads past them. No `schema_compat` floor, for the reason 0108 records: every release
-- from 0.2.2 on tolerates a database carrying migrations it does not embed.
--
-- WHY
-- A row said which channel id it went to and nothing else: not the address it reached, not the
-- routing rule that chose that channel, and not the channel's name once the channel was deleted.
-- An operator could not tell from the log whether a page went where the rules say it should.
--
-- * `channel_name` is the channel's name when the delivery was made. The page shows the current
--   name and falls back to this one for a deleted channel.
-- * `destination` is the part of the channel's address that is not a secret: scheme and host (and
--   for PagerDuty and JSM the path, with token-like segments replaced), or an email's recipients.
--   Never a key, a query string or a webhook's path.
-- * `route` is why the delivery went to this channel (`rule`, `catch_all`, `hold_failed`,
--   `closing_rule`, `open_incident`, `default_route`, `test`). NULL on rows written before this.
-- * `route_rules` is `[{"id": <uuid>, "name": <text>}, ...]`: the routing rules behind it, with
--   their names at delivery time. One for a fire; possibly several for a close.

ALTER TABLE notification_deliveries
    ADD COLUMN IF NOT EXISTS channel_name TEXT NULL,
    ADD COLUMN IF NOT EXISTS destination TEXT NULL,
    ADD COLUMN IF NOT EXISTS route TEXT NULL,
    ADD COLUMN IF NOT EXISTS route_rules JSONB NOT NULL DEFAULT '[]'::jsonb;
