-- 0046_auth_revocations — durable revocation list for stateless signed session tokens
-- (Core HA active/active, ADR-016 Increment 2a).
-- Additive only (ADR-017 expand-contract): a new table, no changes to existing ones — an N-1 binary
-- ignores it entirely.
--
-- A signed session token is self-validating and outlives the process that minted it, so a logout or
-- account disable/demote/password-reset/delete cannot be honored by simply dropping in-process state.
-- Each core keeps an in-memory denylist (the hot path); this table is the durable source of truth it
-- cold-loads on startup so a restart / leader failover never silently un-revokes a token, and it is
-- the backstop for a core that missed the live `yagra.auth.revoke` fan-out.
--
-- Rows self-expire: `expires_at` bounds how long an entry must be kept (a token's own expiry, or the
-- maximum lifetime of any token that could still be in flight for a user). Expired rows are pruned.
--
-- Contains only a token SHA-256 **hash** or a user id — never a raw token or password (security.md).
CREATE TABLE IF NOT EXISTS auth_revocations (
    -- 'token' (single logout) or 'user' (revoke all of a user's tokens issued <= cutoff_iat).
    kind        TEXT NOT NULL,
    -- For 'token': the token's SHA-256 hex. For 'user': the user's UUID as text.
    key         TEXT NOT NULL,
    -- 'user' rows only: deny tokens whose issued-at (Unix seconds) is <= this. NULL for 'token' rows.
    cutoff_iat  BIGINT,
    -- The entry can be dropped after this instant (token exp / max token lifetime).
    expires_at  TIMESTAMPTZ NOT NULL,
    recorded_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (kind, key)
);

-- Cold-load filters on `expires_at > now()` and the periodic prune deletes `expires_at <= now()`.
CREATE INDEX IF NOT EXISTS auth_revocations_expires_idx ON auth_revocations (expires_at);
