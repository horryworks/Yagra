-- 0089_bus_tls — the certificate the core⇄poller BUS serves, and the private key behind it (ADR-065).
--
-- Additive only (ADR-017 expand-contract): one new table. An older core never reads it, so rolling
-- the binary back leaves the row in place and the materialized files on the volume — NATS keeps
-- serving the same certificate either way.
--
-- **No `schema_compat` floor, deliberately.** 0080's rule is "declare a floor when the oldest core
-- that can start afterwards is newer than the one this replaces". This adds a table and narrows
-- nothing, and every release from 0.2.2 on carries the relaxation that tolerates a database with
-- migrations it does not embed. The floor 0080 already recorded covers this one.
--
-- WHY THIS IS A SECOND TABLE AND NOT A SECOND ROW IN `web_tls_config`
-- They look alike and are not the same thing. `web_tls_config` is the certificate a *browser* sees,
-- and its SANs are the names an operator types into an address bar; this is the certificate a
-- *poller at another site* pins, and its SANs are the names that site dials over the WAN. The two
-- move independently — importing a real CA-issued certificate for the WebUI must not touch the bus,
-- and regenerating the bus certificate to add a site must not hand every browser a new fingerprint.
-- Sharing the table would make each of those a discriminator column and a filter every reader had
-- to remember, which is the shape `extensibility.md` §1 warns about. ADR-044 decision 0 scoped bus
-- certificates out of the WebUI TLS work for the neighbouring reason — the import *screen* must not
-- appear to cover both — and this keeps them apart in storage for the same reason.
--
-- WHAT THIS TABLE DOES **NOT** HAVE, AND WHY
-- No `source` column. `web_tls_config` needs one because an operator may import their own
-- certificate there and core must never silently replace it. Here there is no import: the bus
-- certificate is always one Yagra minted, because the only party that has to trust it is a poller
-- Yagra also configures, and pinning a self-signed leaf is a complete answer for that. Adding an
-- import path later means adding the column then — with the same all-or-none reasoning
-- `web_tls_config` records — not leaving an unused one here now.
--
-- The private key gets the same five sealed columns as `credentials`, `web_tls_config`,
-- `oidc_providers`, `ldap_config` and `llm_config` (ADR-018), with the same all-or-none CHECK so a
-- half-written seal can never be read back as a key.

CREATE TABLE IF NOT EXISTS bus_tls_config (
    id                 SMALLINT PRIMARY KEY DEFAULT 1 CHECK (id = 1),

    -- The PEM chain, leaf first. Plaintext: a certificate is public by construction, and this one
    -- has to be *downloadable* — handing it to a remote site as its `YAGRA_BUS_CA_FILE` is the
    -- whole point, since a self-signed leaf is its own CA.
    certificate        TEXT     NOT NULL CHECK (length(trim(certificate)) > 0),

    -- The sealed private key (ADR-018). NOT NULL as a set: a certificate with no key cannot be
    -- served, so there is no valid row without one.
    key_id             INTEGER  NOT NULL,
    wrapped_dek        BYTEA    NOT NULL,
    dek_nonce          BYTEA    NOT NULL,
    ciphertext         BYTEA    NOT NULL,
    ct_nonce           BYTEA    NOT NULL,

    -- ── Parsed at generation, stored so nothing has to decrypt to answer a question ────────────
    -- Same reasoning as `web_tls_config`: the settings card renders these on every load and the
    -- expiry check reads `not_after` on a timer, and neither has any business needing a decrypt.
    subject            TEXT     NOT NULL,
    issuer             TEXT     NOT NULL,
    -- Subject alternative names. The load-bearing column on this table: a poller's TLS handshake
    -- fails unless the exact host or IP it dials appears here, so this is what the UI must show and
    -- what the operator edits when they add a site.
    sans               JSONB    NOT NULL,
    not_before         TIMESTAMPTZ NOT NULL,
    not_after          TIMESTAMPTZ NOT NULL,
    -- Lowercase hex SHA-256 of the leaf's DER — what a site compares against the CA file it was
    -- given, and the only way to tell "the new certificate reached the poller" from "the poller is
    -- still holding the old one".
    fingerprint_sha256 TEXT     NOT NULL,
    -- Human-readable, e.g. `ECDSA P-256`. Descriptive only — nothing branches on it.
    key_algorithm      TEXT     NOT NULL,

    issued_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- Who asked for it, when a person did. NULL for the one the `bus-cert` one-shot mints on first
    -- start, which is the ordinary case: there is no session behind a container that runs before
    -- core does. SET NULL rather than CASCADE — deleting the account must not take the bus down.
    issued_by          UUID REFERENCES users(id) ON DELETE SET NULL,

    -- The seal is all-or-nothing. Every column above is already NOT NULL, so this cannot currently
    -- fail — it is here so a future ALTER that relaxes one of them cannot quietly permit a
    -- half-written seal.
    CHECK (
        (key_id IS NULL AND wrapped_dek IS NULL AND dek_nonce IS NULL
             AND ciphertext IS NULL AND ct_nonce IS NULL)
        OR
        (key_id IS NOT NULL AND wrapped_dek IS NOT NULL AND dek_nonce IS NOT NULL
             AND ciphertext IS NOT NULL AND ct_nonce IS NOT NULL)
    )
);

COMMENT ON TABLE bus_tls_config IS
    'The certificate NATS serves on the core-poller bus (ADR-065). One row; no row = the bus-cert '
    'one-shot generates a self-signed one before NATS starts. The files on the bus TLS volume are a '
    'materialization of this row, not the record.';
