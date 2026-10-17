-- 0150_credential_try_order_default — a new credential goes to the end of the try order by itself
-- (ADR-205 Inc.2).
--
-- reversible: a sequence is created and becomes the column's default; no row is rewritten. A core
-- from before 0149 never names the column, and its creates now land at the end of the order instead
-- of at 0, the front. No `schema_compat` floor, for the reason 0108 records.
--
-- WHY
-- 0149 left placing a new row to the INSERT (`max(try_order) + 1`), which names the column. Every
-- writer then needs the column to exist — a database test that migrates only up to an older version
-- before creating a credential could no longer create one, and a core from before 0149 created its
-- credentials at 0, listed first. A default needs neither: the writer names nothing, and every new
-- row takes the next number. Saving an order writes 1..n, which stays below the sequence, so the next
-- credential still follows the last.
CREATE SEQUENCE IF NOT EXISTS credential_try_order_seq OWNED BY credentials.try_order;

SELECT setval('credential_try_order_seq', COALESCE((SELECT max(try_order) FROM credentials), 0) + 1, false);

ALTER TABLE credentials
    ALTER COLUMN try_order SET DEFAULT nextval('credential_try_order_seq');
