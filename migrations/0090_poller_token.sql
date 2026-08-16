-- 0090_poller_token — a credential per poller instead of one shared by every site (ADR-065 Inc.3).
--
-- Additive only (ADR-017 expand-contract): three nullable columns on an existing table. An older
-- core never selects them, so rolling the binary back leaves every token in place and the fleet
-- authenticating exactly as it did — the bootstrap secret is still what the NATS static account
-- checks when Auth Callout is off. No `schema_compat` floor for the reason 0080 records: this
-- narrows nothing, and the floor already there covers an additive migration.
--
-- WHAT WAS WRONG
-- `yagra-authz`'s callout compared one deployment-wide bootstrap secret and then took the poller id
-- straight from the connection's CONNECT frame. The id was self-asserted and checked against
-- nothing. So a `.env` leaked at one site did not merely expose that site: the holder could claim
-- ANY id, and the working set core then pushed to it carries the plaintext SNMP communities,
-- SNMPv3 credentials and API tokens of whatever nodes that id is assigned (ADR-020). One site's
-- filesystem was, in effect, the whole fleet's credential store.
--
-- Deleting the poller did not help either: `upsert_seen` is an unconditional upsert, so a row
-- removed in the WebUI came back on the next ten-second heartbeat.
--
-- WHY THE COLUMNS ARE NULLABLE, AND WHY THAT IS NOT A HALF-MEASURE
-- Every poller that has ever connected is already in this table with no token, and refusing them
-- would take a working fleet down on upgrade. So the rule is per row rather than per deployment:
--
--   * a row WITH `token_hash` is admitted only by that token — the shared secret stops working
--     for it, which is what makes issuing one worth doing;
--   * a row WITHOUT one is still admitted by the shared secret (the N-1 path);
--   * an id with NO ROW AT ALL is refused outright, whatever it presents.
--
-- The third bullet is the part that closes the hole above, and it needs no token to be issued
-- anywhere: an attacker with a leaked secret can now only impersonate a poller the operator
-- already registered, rather than inventing one. The first bullet is what an operator earns by
-- issuing tokens, one site at a time, with the WebUI showing which sites still have none.
--
-- ONLY THE DIGEST IS STORED, exactly as `api_tokens.token_hash` does it: `hex(sha256(token))`, so a
-- database dump yields no working credential. The token is displayed once, at issue.

ALTER TABLE pollers
    -- Lowercase hex SHA-256 of the poller's token. NULL = no token issued yet (see above).
    ADD COLUMN IF NOT EXISTS token_hash      TEXT,
    ADD COLUMN IF NOT EXISTS token_issued_at TIMESTAMPTZ,
    -- Who issued it. SET NULL rather than CASCADE: deleting the account must not revoke a site's
    -- ability to connect, which is a monitoring outage caused by an unrelated administrative act.
    ADD COLUMN IF NOT EXISTS token_issued_by UUID REFERENCES users(id) ON DELETE SET NULL;

-- The callout looks a poller up by id on every connection, and `id` is already the primary key, so
-- no index is added here. Stated rather than left silent: the lookup is on the connection path, and
-- the next person to read this should know it was considered.

COMMENT ON COLUMN pollers.token_hash IS
    'hex(sha256()) of this poller''s own bus token (ADR-065). NULL = admitted by the deployment-wide '
    'bootstrap secret instead, which is the pre-token behaviour and still the default for a '
    'co-located poller.';
