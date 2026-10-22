-- 0155_config_backup_remote — where config backups are pushed (ADR-210 stage 4): one Git repository
-- for the whole deployment, the sealed login that reaches it, and how the last push went.
--
-- reversible: additive only — one new table. A core from before this migration does not name it and
-- pushes nothing, which is what it did before. No `schema_compat` floor, for the reason 0108 records.
--
-- `id` is a boolean that can only be true, so the table holds at most one row (decision 28: one
-- destination per deployment). `auth` and `failure` are tokens the Rust enums guard rather than
-- CHECKs, for the reason `topology_links.sources` gives.
--
-- `credential_id` points at a `git_remote` credential, sealed like every other secret. RESTRICT, as
-- `netbox_servers` does: the destination is removed with its credential, never the other way round.
--
-- `generation` moves on every change to where or how a push goes, so a push that was already running
-- when the destination changed reports against a generation that is no longer current and is
-- ignored rather than recorded against the new one.
CREATE TABLE config_backup_remote (
    id              BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (id),
    url             TEXT NOT NULL,
    branch          TEXT NOT NULL,
    auth            TEXT NOT NULL,
    credential_id   UUID NOT NULL REFERENCES credentials (id) ON DELETE RESTRICT,
    -- The public half of a key Yagra generated, shown so it can be registered as a deploy key.
    ssh_public_key  TEXT,
    -- The destination's host keys, in known_hosts form, learned on the first SSH push.
    known_hosts     TEXT,
    generation      BIGINT NOT NULL DEFAULT 1,
    pushed_head     TEXT,
    pushed_at       TIMESTAMPTZ,
    failure         TEXT,
    failed_at       TIMESTAMPTZ,
    failures        INTEGER NOT NULL DEFAULT 0,
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);
