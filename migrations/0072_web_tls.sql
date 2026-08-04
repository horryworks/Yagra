-- 0072_web_tls — the certificate the WebUI serves, and the private key behind it (ADR-044).
--
-- Additive only (ADR-017 expand-contract): one new table. An older core simply never reads it, so
-- rolling back the binary leaves the row in place and the materialized file on the volume — the
-- WebUI keeps serving the same certificate either way.
--
-- WHY THE DATABASE IS THE SOURCE OF TRUTH AND NOT THE FILESYSTEM
-- nginx reads a file, so a file has to exist. The obvious design is to make that file authoritative
-- and have operators drop a certificate into a volume. Three things make that the wrong choice here:
-- an operator cannot reach the volume from the WebUI, which is where they are; a bind-mounted path
-- makes "import a certificate" a shell task on the host rather than a permission in the product;
-- and nothing else in this schema keeps a secret outside the envelope. So the row is the record and
-- the file is a materialization of it — deleting the volume is safe, and core rewrites it on the
-- next start.
--
-- THE SPLIT DOWN THE MIDDLE OF ONE FEATURE
-- A certificate and its private key are opposites and this table treats them that way. The chain is
-- a plaintext column: it is public by construction, it must round-trip through the API so the UI can
-- show what is being served, and it has to be *downloadable* so an operator can hand it to a
-- Prometheus `ca_file`, a `curl --cacert`, or an OS trust store while running on a self-signed one.
-- The same argument `0064_ldap.sql` makes for `ca_cert`, with that extra reason on top.
--
-- The private key gets the same five sealed columns as `credentials`, `oidc_providers`, `ldap_config`
-- and `llm_config` (ADR-018), with the same all-or-none CHECK so a half-written seal can never be
-- read back as a key.
--
-- WHY THERE IS NO CERTIFICATE HISTORY TABLE
-- Deliberate, and a departure from the neighbour/L3 tables that do keep one. Those record something
-- that changed underneath us and is worth a timeline. This records a deliberate operator action that
-- is **validated before it commits**: a chain that does not parse, a key that does not match it, or
-- an expired certificate is refused at the API edge, so the state this table can hold is always one
-- that was serviceable when it was written. There is no "it broke, roll back" to support — rollback
-- is importing the previous certificate again, which the operator still has.

CREATE TABLE IF NOT EXISTS web_tls_config (
    id                 SMALLINT PRIMARY KEY DEFAULT 1 CHECK (id = 1),

    -- Where this came from. Drives one behaviour that matters: a self-signed certificate may be
    -- silently regenerated (on renewal, or when an ephemeral KEK means the key can no longer be
    -- opened), and an imported one may NEVER be. Replacing an operator's certificate with a
    -- generated one because a key rotation made it unreadable would be exactly the kind of quiet
    -- data loss ADR-017 forbids, so core refuses, logs, and keeps serving the last materialized file.
    source             TEXT     NOT NULL CHECK (source IN ('self_signed', 'imported')),

    -- The PEM chain, leaf first. Plaintext, see the header.
    certificate        TEXT     NOT NULL CHECK (length(trim(certificate)) > 0),

    -- The sealed private key (ADR-018). NOT NULL as a set: there is no valid row without a key,
    -- because a certificate with no key cannot be served — unlike `llm_config`, where the absence of
    -- a key is a meaningful "not configured yet".
    key_id             INTEGER  NOT NULL,
    wrapped_dek        BYTEA    NOT NULL,
    dek_nonce          BYTEA    NOT NULL,
    ciphertext         BYTEA    NOT NULL,
    ct_nonce           BYTEA    NOT NULL,

    -- ── Parsed at import, stored so nothing has to decrypt to answer a question ──────────────
    -- The settings page renders these on every load and the expiry check reads `not_after` on a
    -- timer. Re-deriving them would mean parsing the chain on each read, and worse, it would put a
    -- decrypt on a path that has no business needing one. They are derived data with a single
    -- writer, so they cannot drift from the certificate beside them.
    subject            TEXT     NOT NULL,
    issuer             TEXT     NOT NULL,
    -- Subject alternative names, as an array of strings. A certificate with none is refused at the
    -- edge (Chrome ignores CN entirely, so a CN-only certificate is a support ticket rather than a
    -- working import), which is why this has no empty-array default.
    sans               JSONB    NOT NULL,
    not_before         TIMESTAMPTZ NOT NULL,
    not_after          TIMESTAMPTZ NOT NULL,
    -- Lowercase hex SHA-256 of the leaf's DER. Shown in the UI so an operator can compare what the
    -- server holds against what their browser shows, which is the only way to tell "the import
    -- worked" from "I am looking at a cached connection".
    fingerprint_sha256 TEXT     NOT NULL,
    -- Human-readable, e.g. `RSA-2048` or `ECDSA P-256`. Descriptive only — nothing branches on it.
    key_algorithm      TEXT     NOT NULL,

    imported_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- Who imported it. SET NULL rather than CASCADE: deleting the account does not un-import the
    -- certificate, and losing the row would take the certificate down with it.
    imported_by        UUID REFERENCES users(id) ON DELETE SET NULL,

    -- The seal is all-or-nothing. Every column above is already NOT NULL, so this cannot currently
    -- fail — it is here for the same reason its siblings are: it states the invariant at the schema
    -- level, so a future ALTER that relaxes one of them cannot quietly permit a half-written seal.
    CHECK (
        (key_id IS NULL AND wrapped_dek IS NULL AND dek_nonce IS NULL
             AND ciphertext IS NULL AND ct_nonce IS NULL)
        OR
        (key_id IS NOT NULL AND wrapped_dek IS NOT NULL AND dek_nonce IS NOT NULL
             AND ciphertext IS NOT NULL AND ct_nonce IS NOT NULL)
    )
);

COMMENT ON TABLE web_tls_config IS
    'The certificate the WebUI serves (ADR-044). One row; no row = core generates a self-signed one '
    'on next start. The file on the TLS volume is a materialization of this row, not the record.';
