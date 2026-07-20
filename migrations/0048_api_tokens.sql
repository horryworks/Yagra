-- 0048_api_tokens — long-lived API tokens (PATs) for non-browser clients (ADR-028, Phase 4).
-- Additive only (ADR-017 expand-contract): a new table, no changes to existing ones — an N-1 binary
-- ignores it entirely, and the feature it backs (`/mcp`) is itself default-OFF.
--
-- Yagra's session tokens are login-minted and short-lived (idle + absolute TTL, ADR-016 2a), which
-- suits a browser but not an unattended MCP client (Claude Code/Desktop) whose stored credential
-- must survive restarts. An API token is a durable, admin-issued credential carrying a role + scope,
-- so the MCP auth gate resolves it to the same `Principal` a session yields, going through the same
-- RBAC path (ADR-014). Issuance/revocation are admin actions, audited via the API's `audit_mw`.
--
-- SECURITY (security.md/ADR-018): the raw token is shown to the admin **once** at creation and never
-- stored — only its SHA-256 hex lives here (`token_hash`), so a DB dump cannot recover a usable
-- credential. The token grants exactly `role`/`scope`; keep it least-privileged (Viewer + a group
-- scope) for a read-only MCP client.
CREATE TABLE IF NOT EXISTS api_tokens (
    id           UUID PRIMARY KEY,
    -- Human label shown in Settings ▸ API tokens (e.g. "oncall-laptop"). Unique so the list reads
    -- unambiguously and a name can't be silently duplicated.
    name         TEXT NOT NULL UNIQUE,
    -- SHA-256 hex of the raw `yat_…` token (never the token itself). Unique = the lookup key.
    token_hash   TEXT NOT NULL UNIQUE,
    -- The granted role, serde snake_case of yagra_common::Role ('viewer' | 'operator' | 'admin').
    role         TEXT NOT NULL,
    -- The granted visibility scope, serde JSON of yagra_common::Scope ("All" or {"Groups":[…]}).
    scope        JSONB NOT NULL,
    -- Audit trail: which account issued this token (username, never a credential).
    created_by   TEXT NOT NULL,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- Refreshed (throttled) on use so an admin can spot stale/unused tokens. NULL until first use.
    last_used_at TIMESTAMPTZ,
    -- Soft-revoke: a non-NULL value makes the token fail auth immediately (kept as an audit record
    -- rather than hard-deleted). The active-token lookup filters on `revoked_at IS NULL`.
    revoked_at   TIMESTAMPTZ
);

-- The auth hot path looks a token up by its hash among the non-revoked rows.
CREATE INDEX IF NOT EXISTS api_tokens_active_idx ON api_tokens (token_hash) WHERE revoked_at IS NULL;
