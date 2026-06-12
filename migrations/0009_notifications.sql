-- Alert routing & notifications (Phase A).
--
-- notification_channels: where alerts can be delivered (webhook/email). The connection
-- config (URLs, SMTP host/from/to/user/pass) is a SECRET, so it is envelope-encrypted at
-- rest exactly like credentials (ADR-018) — only ciphertext + wrapped DEK are stored, and
-- the API never returns it. routing_rules: which alerts go to which channels, matched by
-- severity (NULL = all). Channels fan out per the engine's dedup + retry.

CREATE TABLE notification_channels (
    id          UUID PRIMARY KEY,
    name        TEXT NOT NULL UNIQUE,
    kind        TEXT NOT NULL,              -- 'webhook' | 'email'
    enabled     BOOLEAN NOT NULL DEFAULT true,
    -- Envelope-encrypted config blob (mirrors the credentials table columns).
    key_id      INTEGER NOT NULL,
    wrapped_dek BYTEA NOT NULL,
    dek_nonce   BYTEA NOT NULL,
    ciphertext  BYTEA NOT NULL,
    ct_nonce    BYTEA NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE routing_rules (
    id          UUID PRIMARY KEY,
    name        TEXT NOT NULL,
    enabled     BOOLEAN NOT NULL DEFAULT true,
    severity    TEXT,                        -- 'critical' | 'warning' | 'info' | NULL = all
    channel_ids UUID[] NOT NULL DEFAULT '{}',
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
