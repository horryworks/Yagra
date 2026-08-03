-- 0064_ldap — LDAP/AD authentication (ADR-041 Increment 1): the single directory Yagra binds to,
-- and the group→role mapping that decides what a person who signs in through it may do.
--
-- Additive only (ADR-017 expand-contract): one new table, plus comments on three existing columns.
-- No column is added, dropped or retyped anywhere, so there is no contract phase. An N-1 binary
-- never selects `ldap_config`; a rollback just leaves it orphaned and directory login stops being
-- offered.
--
-- DEFAULT OFF, AND OFF MEANS NO EGRESS
-- Absence of a row is the off state, not a flag that happens to be false — the same shape as
-- `oidc_providers` (0044), `forward_destinations` (0050) and `llm_config` (0053). With no row there
-- is no host to reach, no bind password to decrypt and no outbound connection. Upgrading a running
-- system changes nothing until an operator deliberately configures a directory.
--
-- WHY THIS IS A SINGLE ROW
-- `CHECK (id = 1)`, like `llm_config`. One corporate directory is the case ADR-041 is for: a
-- closed network whose AD or OpenLDAP is the source of truth for who works there. Multiple
-- directories would need a resolution order between them — which one authenticates a name both
-- know? — and that order is a security decision nobody asked for yet. Making it structural rather
-- than an application rule means the question has to be reopened deliberately.
--
-- WHY `users` GAINS NO COLUMNS — READ THIS BEFORE ADDING SOME
-- An LDAP account's identity is stored in `users.oidc_provider_id` + `users.oidc_subject`, the pair
-- 0044 added, and is protected by the same partial unique index `users_oidc_identity_idx`. Those two
-- columns are hereby **the generic external-identity pair**; the `oidc_` prefix is historical.
-- `users.auth_source` says which kind of provider the pair refers to.
--
-- The alternative — `ldap_subject` / `ldap_provider_id` beside them — would have been a third
-- parallel set of the same concept, a second partial unique index to keep in agreement with the
-- first, and a fork of the ~75-line JIT-provisioning function whose three non-obvious decisions
-- (refuse a disabled account, keep the stored scope, never hijack a username) would then have to be
-- re-derived. Renaming the columns instead would be an expand-contract spread over two releases for
-- a cosmetic gain. The comments below are the cheaper honest answer.
--
-- `ldap_config.id` is a SMALLINT and cannot go in a UUID column, so the value written to
-- `oidc_provider_id` for a directory account is a stable name-derived constant in `ldap.rs`
-- (`LDAP_PROVIDER_ID`), pinned by a test — changing it orphans every account already provisioned.
--
-- No CHECK is added to `users.auth_source`. 0044 did not add one, and doing it now would mean a
-- constraint migration for every kind added later; `UserKind` plus its fail-safe parse (an unknown
-- value reads as a local account) is the guard, and it is in one place.
--
-- THE SEALED COLUMNS
-- The service account's bind password is envelope-encrypted like every other secret in the schema —
-- ciphertext plus a wrapped DEK, KEK outside the database (ADR-018). The five columns are the same
-- set `credentials`, `oidc_providers` and `llm_config` carry, and the CHECK enforces all-five-or-none
-- so a half-written seal can never be read back as a password.
--
-- Unlike `llm_config` they are NOT NULL. There is no valid configuration with no bind password: a
-- simple bind with a DN and an empty password is an *unauthenticated* bind (RFC 4513), which a
-- permissive directory answers `success` — so an empty stored password does not mean "no
-- credential", it means the search silently runs as anonymous. The API rejects a blank one at the
-- edge with a typed 400; this NOT NULL is the backstop behind that, not the primary guard.
--
-- `ca_cert` is a certificate, not a secret, so it is a plaintext column that round-trips through the
-- API — the same reasoning `forward_destinations` records for its TLS trust anchors. It exists
-- because a corporate AD almost always presents an enterprise CA that is not in the container's
-- bundle: without a place to put that CA, every LDAPS handshake fails and the only apparent fix is
-- the certificate-verification-disable flag ADR-041 forbids. Giving trust somewhere to go is what
-- keeps that flag from being demanded.
--
-- The remaining CHECKs (port range, non-empty DNs, the `{username}` placeholder) are backstops too.
-- The primary guard is `LdapRepo::validate` at the API edge, which answers with a typed 400 naming
-- the field — a constraint violation surfacing as a 500 is a worse version of the same rejection.

CREATE TABLE IF NOT EXISTS ldap_config (
    id                SMALLINT PRIMARY KEY DEFAULT 1 CHECK (id = 1),

    -- Connection. Host/port/security are stored separately and the URL is derived in code: a
    -- free-text URL lets `ldap://dc1` be typed, which sends the bind password and every user's
    -- password in cleartext with no error and no warning. `security` has no plaintext value.
    host              TEXT     NOT NULL CHECK (length(trim(host)) > 0),
    port              INTEGER  NOT NULL DEFAULT 636 CHECK (port BETWEEN 1 AND 65535),
    security          TEXT     NOT NULL DEFAULT 'ldaps' CHECK (security IN ('ldaps', 'starttls')),
    ca_cert           TEXT,

    -- The service account used for the search leg of the two-stage bind.
    bind_dn           TEXT     NOT NULL CHECK (length(trim(bind_dn)) > 0),
    key_id            INTEGER  NOT NULL,
    wrapped_dek       BYTEA    NOT NULL,
    dek_nonce         BYTEA    NOT NULL,
    ciphertext        BYTEA    NOT NULL,
    ct_nonce          BYTEA    NOT NULL,

    -- Finding the user. `user_filter` must contain the `{username}` placeholder: without it the
    -- filter matches the whole subtree, and a code path that took the first result would let anyone
    -- log in as whoever sorts first. The application also requires the search to match exactly one
    -- entry, so this CHECK is the outermost of three independent gates.
    user_base_dn      TEXT     NOT NULL CHECK (length(trim(user_base_dn)) > 0),
    user_filter       TEXT     NOT NULL DEFAULT '(&(objectClass=user)(sAMAccountName={username}))'
                               CHECK (position('{username}' in user_filter) > 0),
    -- The attribute holding the canonical login name. The stored `users.username` comes from here,
    -- never from what was typed: AD matches `sAMAccountName` case-insensitively while
    -- `users.username` is compared case-sensitively, so trusting the typed string produces a second
    -- account the first time somebody capitalises their own name.
    username_attribute TEXT    NOT NULL DEFAULT 'sAMAccountName',
    -- The immutable per-entry identifier: `objectGUID` on AD, `entryUUID` on OpenLDAP. This is what
    -- survives a rename, and it is required — an entry that does not return it is refused rather
    -- than stored with an empty subject, which the partial unique index would let the first such
    -- account own outright.
    uid_attribute     TEXT     NOT NULL DEFAULT 'objectGUID',

    -- Group membership, two ways. The attribute is read from the user's own entry (AD populates it
    -- natively; OpenLDAP needs the memberof overlay). The optional search covers directories that do
    -- not, and is the only way to resolve *nested* groups on AD — `memberOf` is not transitive
    -- there, so a user in a group that is a member of another group does not list the outer one.
    -- With `group_base_dn` NULL only the attribute is consulted.
    member_of_attribute TEXT   NOT NULL DEFAULT 'memberOf',
    group_base_dn     TEXT,
    group_filter      TEXT,
    group_name_attribute TEXT  NOT NULL DEFAULT 'cn',

    -- Group→role mapping, deliberately the same shape `oidc_providers` uses (ADR-041 decision 2:
    -- one mechanism, not two). Keys are group names or DNs as the operator typed them; the match is
    -- normalised in code, so what is stored keeps their casing. NULL `default_role` = deny anyone no
    -- group maps for. `enabled = true` with an empty map and no default is refused at save time,
    -- because it denies every login while looking configured.
    role_map          JSONB    NOT NULL DEFAULT '{}'::jsonb,
    default_role      TEXT,

    enabled           BOOLEAN  NOT NULL DEFAULT FALSE,
    updated_at        TIMESTAMPTZ NOT NULL DEFAULT now(),

    CHECK ((group_base_dn IS NULL) = (group_filter IS NULL))
);

COMMENT ON TABLE ldap_config IS
    'The single LDAP/AD directory Yagra authenticates against (ADR-041). No row = not configured.';

-- The two columns 0044 introduced for OIDC now carry any external identity. Said here because the
-- names cannot say it, and a reader who only sees `oidc_` will conclude LDAP stored its identities
-- in the wrong place.
COMMENT ON COLUMN users.oidc_subject IS
    'Generic external-identity subject: the OIDC `sub` claim, or the directory entry''s immutable id '
    '(objectGUID/entryUUID) for an LDAP account. The `oidc_` prefix is historical (ADR-041); '
    '`auth_source` says which kind of provider it belongs to.';
COMMENT ON COLUMN users.oidc_provider_id IS
    'Generic external-identity provider: the `oidc_providers.id` row, or the stable synthetic '
    'LDAP_PROVIDER_ID constant for the configured directory (ADR-041). The `oidc_` prefix is '
    'historical.';
COMMENT ON COLUMN users.auth_source IS
    'How the account authenticates — a UserKind key: local | oidc | ldap | service. Unknown values '
    'read as local, which is the column default that predates the enum.';
COMMENT ON INDEX users_oidc_identity_idx IS
    'Uniqueness of an external identity across every provider kind, not just OIDC (ADR-041).';
