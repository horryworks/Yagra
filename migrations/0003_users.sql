-- 0003_users — local authentication accounts (Phase 1 auth foundation, ADR-014).
--
-- Additive (ADR-017). Passwords are Argon2id PHC strings (yagra-secrets), never
-- plaintext and never decryptable — only verified. Role is one of viewer/operator/admin;
-- group-scope columns for multi-tenant visibility land with Phase 2 scope enforcement.

CREATE TABLE users (
    id            UUID PRIMARY KEY,
    username      TEXT NOT NULL UNIQUE,
    password_hash TEXT NOT NULL,
    role          TEXT NOT NULL,           -- viewer | operator | admin
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);
