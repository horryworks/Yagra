-- 0002_credentials — encrypted monitoring credentials at rest (ADR-018).
--
-- Expand-contract / additive (ADR-017): create only. The table stores ONLY ciphertext
-- and the wrapped DEK (envelope encryption, yagra-secrets) — never plaintext. The KEK
-- lives outside the DB (mounted file). Binding a credential to a node lands with SNMP.

CREATE TABLE credentials (
    id          UUID PRIMARY KEY,
    name        TEXT NOT NULL,
    kind        TEXT NOT NULL,                 -- e.g. snmp_v2c, snmp_v3, api_token
    key_id      BIGINT NOT NULL,               -- KEK generation that wrapped the DEK
    wrapped_dek BYTEA NOT NULL,
    dek_nonce   BYTEA NOT NULL,
    ciphertext  BYTEA NOT NULL,
    ct_nonce    BYTEA NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
