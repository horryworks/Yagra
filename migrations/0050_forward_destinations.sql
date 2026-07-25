-- 0050_forward_destinations — forwarding ("tee") of received passive data (ADR-034).
-- Additive only (ADR-017 expand-contract): a new table, no changes to existing ones. An N-1 core
-- ignores it entirely, and with no rows the feature is inert — nothing is forwarded.
--
-- Yagra receives syslog and SNMP traps but had no way to hand them onward, forcing operators to
-- configure a second export target on every device (device load, duplicated config, and per-device
-- limits on how many collectors can be named). A destination here says "everything matching this
-- filter also goes there", and core's leader-only forwarder does the sending — one egress point to
-- allow through a firewall rather than one per poller.
--
-- FIDELITY: `verbatim` = relay the original datagram byte-for-byte (carried on `EventMsg.raw`).
-- The alternative rebuilds the message from the parsed fields, which is lossy — RFC 5424
-- structured data, non-UTF-8 bytes, text past 4096 chars and varbinds past the 32nd were all
-- discarded at reception and cannot be recovered. Verbatim is therefore the default; it degrades
-- to rendering only for events from an N-1 poller that carried no raw payload.
--
-- SECURITY (security.md/ADR-018): syslog bodies routinely carry credentials and a forwarding
-- destination sends them off-box, so destinations are `ManageConfig`-gated and audited like any
-- other config change. Per-destination secrets (currently only the SNMP community used when a trap
-- has to be re-encoded) are envelope-encrypted with the same KEK as monitoring credentials — the
-- five sealed columns mirror `notification_channels`, and are NULL when a destination needs none.
CREATE TABLE IF NOT EXISTS forward_destinations (
    id           UUID PRIMARY KEY,
    -- Human label shown in Settings ▸ Forwarding. Unique so the list reads unambiguously and so a
    -- name is usable as the (bounded) `dest` metric label.
    name         TEXT NOT NULL UNIQUE CHECK (length(name) BETWEEN 1 AND 120),
    enabled      BOOLEAN NOT NULL DEFAULT true,
    -- Which received stream is teed. serde snake_case of yagra_forward::SourceKind.
    -- Widened by a later migration when flow forwarding lands (ADR-034 Increment 2).
    source_kind  TEXT NOT NULL CHECK (source_kind IN ('syslog', 'trap')),
    -- How the destination is spoken to. serde snake_case of yagra_forward::DestKind.
    dest_kind    TEXT NOT NULL CHECK (dest_kind IN ('syslog_udp', 'syslog_tcp', 'snmp_trap_udp')),
    -- `host:port` of the collector. Deliberately free-form text (not INET): a hostname is the
    -- common case and resolution happens in the sender, not at write time.
    target       TEXT NOT NULL CHECK (length(target) BETWEEN 1 AND 255),
    -- Restrict to one poller pool; NULL = every pool. Not an FK: pools are a label on nodes and
    -- pollers, not a table, and a destination may legitimately name a pool that has no poller yet.
    pool         TEXT,
    -- true = relay the original bytes; false = rebuild from the parsed fields (see FIDELITY above).
    verbatim     BOOLEAN NOT NULL DEFAULT true,
    -- serde JSON of yagra_forward::FilterExpr. `{}` = forward everything on the stream; compiled
    -- and validated at the API edge, so an unparseable filter can never reach the forwarder.
    filter       JSONB NOT NULL DEFAULT '{}'::jsonb,
    -- Optional ceiling on messages/second sent to this destination (NULL = no ceiling). Protects a
    -- collector that is smaller than Yagra's intake; excess is dropped and counted, never queued.
    rate_limit_per_sec INTEGER CHECK (rate_limit_per_sec IS NULL OR rate_limit_per_sec > 0),
    -- Envelope-encrypted per-destination secret (yagra_secrets::SealedSecret). NULL when the
    -- destination needs none. All five move together — either every column is set or none is.
    key_id       INTEGER,
    wrapped_dek  BYTEA,
    dek_nonce    BYTEA,
    ciphertext   BYTEA,
    ct_nonce     BYTEA,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT forward_destinations_secret_all_or_none CHECK (
        (key_id IS NULL AND wrapped_dek IS NULL AND dek_nonce IS NULL
             AND ciphertext IS NULL AND ct_nonce IS NULL)
        OR (key_id IS NOT NULL AND wrapped_dek IS NOT NULL AND dek_nonce IS NOT NULL
             AND ciphertext IS NOT NULL AND ct_nonce IS NOT NULL)
    )
);

-- The forwarder reloads only the enabled rows, on every config generation change.
CREATE INDEX IF NOT EXISTS forward_destinations_enabled_idx
    ON forward_destinations (enabled) WHERE enabled;
