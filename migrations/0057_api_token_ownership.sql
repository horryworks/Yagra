-- 0057_api_token_ownership — make a PAT usable on REST, bounded in time, and bound to an account.
--
-- Until now a PAT was a **free-standing identity**: `api_tokens` carried its own `role`/`scope` and
-- no link to `users` at all (`created_by` is a username *string*, kept for the audit trail). The
-- verify path read `role, scope` and built a `Principal` without ever consulting `users`, so
-- deleting, disabling or demoting the account that issued a token changed nothing about the token.
-- That was survivable while a PAT authenticated `/mcp` only — a read-mostly, default-OFF surface —
-- but the REST API is the whole configuration surface, so opening it to PATs without an owner would
-- mean a departed admin's credential outliving the account it came from, indefinitely.
--
-- Additive only (ADR-017 expand-contract). Three columns, each with a default that preserves
-- today's behaviour for rows that already exist:
--
--   * `surfaces`   — which auth surfaces the token may be presented at. Existing rows default to
--                    `{mcp}`, so an upgrade **cannot** silently promote a token minted for an AI
--                    client into a REST credential. Opting a token into REST is an explicit act.
--   * `expires_at` — NULL means no expiry, which is deliberate: a service-account token driving CI
--                    should not die on a date nobody remembers. Existing rows keep NULL.
--   * `owner_user_id` — the account the token acts as. Verification now JOINs `users`, so a
--                    disabled or deleted owner takes its tokens with it, and the effective role is
--                    capped at the owner's current role (demotion narrows the token immediately).
--
-- N-1 (rolling upgrade): an older core ignores all three columns, so during the rollout it will
-- still accept an expired or MCP-only token on `/mcp`. The window is one deploy long and each of
-- these is a defence-in-depth layer rather than the only gate (revocation still works on both
-- versions), so this is accepted rather than blocked behind a two-phase migration.

ALTER TABLE api_tokens
    ADD COLUMN surfaces      TEXT[] NOT NULL DEFAULT '{mcp}',
    ADD COLUMN expires_at    TIMESTAMPTZ,
    ADD COLUMN owner_user_id UUID REFERENCES users(id) ON DELETE SET NULL;

-- Backfill the owner from `created_by`, which is the only link that ever existed.
--
-- `users.username` is UNIQUE so the match itself is unambiguous, but a *deleted* username can be
-- re-created for a different person. Binding only to accounts older than the token removes that
-- mis-binding: a re-created account is necessarily newer than a token issued to its predecessor.
--
-- Rows left with a NULL owner are tokens whose issuer no longer exists. Verification is
-- fail-closed (an INNER JOIN), so those stop authenticating — which is exactly the case this
-- change exists to close. The admin listing renders them as "no owner" so they can be revoked
-- deliberately rather than lingering as a mystery.
UPDATE api_tokens t
   SET owner_user_id = u.id
  FROM users u
 WHERE u.username = t.created_by
   AND u.created_at < t.created_at;

-- ON DELETE SET NULL keeps the row (revocation is a soft-delete here, so the audit trail survives
-- the account), but an orphaned token must never authenticate again. The application revokes an
-- owner's tokens in the same transaction as the account delete; this index keeps the "whose tokens
-- are these" lookup that does so from scanning the table.
CREATE INDEX IF NOT EXISTS api_tokens_owner_idx ON api_tokens (owner_user_id);
