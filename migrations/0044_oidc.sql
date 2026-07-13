-- 0044_oidc — external IdP login via OpenID Connect (ADR-010 Phase 3).
--
-- oidc_providers: one configurable OIDC provider (issuer/client_id + envelope-encrypted
-- client_secret, exactly like notification_channels/credentials — only ciphertext + wrapped DEK
-- are stored and the API never returns it, ADR-018). role_map maps IdP group names → Yagra roles;
-- a login's role is the highest mapped role among the user's groups (default_role as a fallback).
--
-- users gains federated-identity columns so an OIDC user can be JIT-provisioned. Additive + N-1
-- safe (ADR-017 expand-contract): auth_source defaults to 'local' so existing rows and older
-- binaries are unaffected. password_hash STAYS NOT NULL on purpose — an OIDC user is inserted with
-- a non-verifiable sentinel (see auth.rs OIDC_PASSWORD_SENTINEL) rather than NULL, so an old binary
-- reading the row still gets a String it simply can never verify against, and the new binary also
-- refuses local login for auth_source='oidc'. This keeps a rolling upgrade fully N-1 safe.

CREATE TABLE IF NOT EXISTS oidc_providers (
    id           UUID PRIMARY KEY,
    name         TEXT NOT NULL,                 -- display label ("Company SSO")
    issuer       TEXT NOT NULL,                 -- OIDC issuer URL (discovery base)
    client_id    TEXT NOT NULL,
    -- Envelope-encrypted client_secret (mirrors notification_channels / credentials columns).
    key_id       INTEGER NOT NULL,
    wrapped_dek  BYTEA NOT NULL,
    dek_nonce    BYTEA NOT NULL,
    ciphertext   BYTEA NOT NULL,
    ct_nonce     BYTEA NOT NULL,
    redirect_uri TEXT NOT NULL,                 -- the WebUI /auth/callback URL registered at the IdP
    scopes       TEXT NOT NULL DEFAULT 'openid profile email groups',
    groups_claim TEXT NOT NULL DEFAULT 'groups',
    role_map     JSONB NOT NULL DEFAULT '{}'::jsonb,   -- { "<idp-group>": "viewer|operator|admin" }
    default_role TEXT,                          -- role when no group maps (NULL = deny login)
    enabled      BOOLEAN NOT NULL DEFAULT FALSE,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Federated-identity columns on users (additive, defaulted ⇒ N-1 safe).
ALTER TABLE users ADD COLUMN auth_source TEXT NOT NULL DEFAULT 'local';   -- 'local' | 'oidc'
ALTER TABLE users ADD COLUMN oidc_subject TEXT;                           -- IdP 'sub' claim
ALTER TABLE users ADD COLUMN oidc_provider_id UUID;                       -- which provider issued it

-- One local account per (provider, subject); partial so local accounts (NULL subject) are exempt.
CREATE UNIQUE INDEX IF NOT EXISTS users_oidc_identity_idx
    ON users (oidc_provider_id, oidc_subject)
    WHERE oidc_subject IS NOT NULL;
