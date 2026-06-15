-- 0017_user_enabled — account status (active/disabled) for local accounts.
--
-- Additive + defaulted (ADR-017 expand-contract): every existing account becomes enabled,
-- so no data is lost and older binaries that don't know the column keep working (N-1-safe).
-- A disabled account is retained for the audit trail but is barred from authenticating.

ALTER TABLE users ADD COLUMN enabled BOOLEAN NOT NULL DEFAULT TRUE;
