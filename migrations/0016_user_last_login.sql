-- 0016_user_last_login — record each account's most recent successful login.
--
-- Additive (ADR-017): a nullable column, no backfill. Existing accounts read as "never logged
-- in" until their next login. N-1-safe — older binaries simply ignore the column.

ALTER TABLE users ADD COLUMN last_login_at TIMESTAMPTZ;
